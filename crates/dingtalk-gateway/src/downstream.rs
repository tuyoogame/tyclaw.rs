//! 下游连接管理 —— WebSocket 服务端，接受 TyClaw 实例连接。
//!
//! 每个 TyClaw 实例建立一条 WebSocket 连接到网关。
//! 网关按 sender_id 路由消息（route_table 或 hash 模式）。
//! 某个实例断开后，其绑定的会话自动重新分配。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify, RwLock};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{info, warn};

use crate::config::RoutingConfig;
use crate::upstream::IncomingMessage;

struct Backend {
    id: String,
    label: String,
    tx: mpsc::Sender<String>,
}

/// 下游连接管理器。
pub struct DownstreamManager {
    backends: Arc<RwLock<Vec<Backend>>>,
    routing: ArcSwap<RoutingConfig>,
    ready: AtomicBool,
    backend_connected: Notify,
    ready_wait_secs: u64,
}

impl DownstreamManager {
    pub fn new(ready_wait_secs: u64, routing: RoutingConfig) -> Arc<Self> {
        Arc::new(Self {
            backends: Arc::new(RwLock::new(Vec::new())),
            routing: ArcSwap::from_pointee(routing),
            ready: AtomicBool::new(false),
            backend_connected: Notify::new(),
            ready_wait_secs,
        })
    }

    /// 热更新路由配置，不影响已连接的后端。
    pub fn swap_routing(&self, new_routing: RoutingConfig) {
        self.routing.store(Arc::new(new_routing));
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    /// 等待就绪。第一个后端连入后开始倒计时，窗口内无新后端则就绪。
    pub async fn wait_ready(&self) {
        loop {
            if !self.backends.read().await.is_empty() {
                break;
            }
            self.backend_connected.notified().await;
        }
        let count = self.backends.read().await.len();
        info!(backends = count, "First backend connected, starting ready window ({}s)", self.ready_wait_secs);

        loop {
            match tokio::time::timeout(
                std::time::Duration::from_secs(self.ready_wait_secs),
                self.backend_connected.notified(),
            ).await {
                Ok(()) => {
                    let count = self.backends.read().await.len();
                    info!(backends = count, "New backend connected, resetting ready window");
                }
                Err(_) => break,
            }
        }

        let count = self.backends.read().await.len();
        self.ready.store(true, Ordering::Relaxed);
        info!(backends = count, "Gateway READY — dispatching messages");
    }

    /// 启动 WebSocket 服务端，接受 TyClaw 实例连接。
    pub async fn listen(self: &Arc<Self>, addr: &str) {
        let listener = TcpListener::bind(addr).await.unwrap_or_else(|e| {
            eprintln!("Failed to bind {addr}: {e}");
            std::process::exit(1);
        });
        info!(addr, "Gateway listening for TyClaw instances");

        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "Accept failed");
                    continue;
                }
            };
            let mgr = Arc::clone(self);
            tokio::spawn(async move {
                mgr.handle_backend(stream, peer).await;
            });
        }
    }

    /// 处理单个 TyClaw 实例的 WebSocket 连接。
    async fn handle_backend(self: &Arc<Self>, stream: TcpStream, peer: SocketAddr) {
        let ws = match tokio_tungstenite::accept_async(stream).await {
            Ok(ws) => ws,
            Err(e) => {
                warn!(peer = %peer, error = %e, "WebSocket handshake failed");
                return;
            }
        };

        let backend_id = format!("tyclaw-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        info!(backend_id, peer = %peer, "TyClaw instance connected, waiting for register");

        let (mut ws_write, mut ws_read) = ws.split();
        let (tx, mut rx) = mpsc::channel::<String>(256);

        // 等待 register 消息获取 label（10s 超时）
        let label = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            Self::wait_register(&mut ws_read),
        ).await {
            Ok(Some(l)) => l,
            Ok(None) => {
                warn!(backend_id, peer = %peer, "Connection closed before register");
                return;
            }
            Err(_) => {
                warn!(backend_id, peer = %peer, "Register timeout (10s), disconnecting");
                return;
            }
        };

        info!(backend_id, label, peer = %peer, "Backend registered");

        // 同 label 去重：踢掉旧连接
        {
            let mut backends = self.backends.write().await;
            let evicted: Vec<String> = backends.iter()
                .filter(|b| b.label == label)
                .map(|b| b.id.clone())
                .collect();
            for old_id in &evicted {
                info!(old_id, label, "Evicting stale backend with same label");
            }
            backends.retain(|b| b.label != label);
            backends.push(Backend {
                id: backend_id.clone(),
                label: label.clone(),
                tx,
            });
            self.backend_connected.notify_waiters();
        }

        // 写任务：从 channel 读消息发给 TyClaw
        let write_task = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if ws_write.send(WsMessage::Text(msg.into())).await.is_err() {
                    break;
                }
            }
        });

        // 读循环：心跳 / 状态
        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(WsMessage::Text(text)) => {
                    if text.contains("\"type\":\"ping\"") || text.contains("\"type\":\"heartbeat\"") {
                        continue;
                    }
                }
                Ok(WsMessage::Ping(d)) => { let _ = d; }
                Ok(WsMessage::Close(_)) => break,
                Err(e) => {
                    warn!(backend_id, error = %e, "Backend read error");
                    break;
                }
                _ => {}
            }
        }

        info!(backend_id, label, "TyClaw instance disconnected");
        write_task.abort();
        self.remove_backend(&backend_id).await;
    }

    /// 从第一帧中解析 register 消息的 label。
    async fn wait_register(
        ws_read: &mut futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<TcpStream>,
        >,
    ) -> Option<String> {
        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(WsMessage::Text(text)) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&*text) {
                        if v.get("type").and_then(|t| t.as_str()) == Some("register") {
                            if let Some(label) = v.get("label").and_then(|l| l.as_str()) {
                                return Some(label.to_string());
                            }
                        }
                    }
                    // 非 register 消息（如 ping）在注册前忽略
                }
                Ok(WsMessage::Ping(_)) => continue,
                Ok(WsMessage::Close(_)) | Err(_) => return None,
                _ => continue,
            }
        }
        None
    }

    async fn remove_backend(&self, id: &str) {
        let mut backends = self.backends.write().await;
        if let Some(pos) = backends.iter().position(|b| b.id == id) {
            backends.remove(pos);
            info!(id, remaining = backends.len(), "Backend removed");
        }
    }

    /// 将消息分发给下游 TyClaw 实例。
    ///
    /// 路由策略按 routing.mode 决定：
    /// - route_table: sender_id 查 rules → label，未命中走 default label
    /// - hash: sender_id 哈希，有 weights 则灰度分流，无 weights 则均匀分配
    pub async fn dispatch(&self, msg: &IncomingMessage) {
        if !self.is_ready() {
            warn!(message_id = %msg.message_id, "Gateway not ready, message dropped");
            send_maintenance_reply(msg).await;
            return;
        }

        let backends = self.backends.read().await;
        if backends.is_empty() {
            send_maintenance_reply(msg).await;
            return;
        }

        let routing = self.routing.load();

        let envelope = serde_json::json!({
            "type": "message",
            "message_id": msg.message_id,
            "conversation_id": msg.conversation_id,
            "sender_id": msg.sender_id,
            "data": msg.data,
        });
        let json = match serde_json::to_string(&envelope) {
            Ok(j) => j,
            Err(_) => return,
        };

        match routing.mode.as_str() {
            "route_table" => {
                let target_label = routing.rules
                    .get(&msg.sender_id)
                    .unwrap_or(&routing.default);

                let matching: Vec<_> = backends.iter()
                    .filter(|b| b.label == *target_label)
                    .collect();

                if matching.is_empty() {
                    warn!(
                        message_id = %msg.message_id,
                        sender_id = %msg.sender_id,
                        target_label,
                        "No backend with target label online"
                    );
                    drop(backends);
                    send_maintenance_reply(msg).await;
                    return;
                }

                let idx = hash_route(&msg.sender_id, matching.len());
                info!(
                    message_id = %msg.message_id,
                    sender_id = %msg.sender_id,
                    target_label,
                    target_backend = %matching[idx].id,
                    mode = "route_table",
                    "Dispatching message"
                );
                let _ = matching[idx].tx.send(json).await;
            }

            _ => {
                if routing.weights.is_empty() {
                    let idx = hash_route(&msg.sender_id, backends.len());
                    info!(
                        message_id = %msg.message_id,
                        sender_id = %msg.sender_id,
                        target_backend = %backends[idx].id,
                        target_label = %backends[idx].label,
                        mode = "hash_uniform",
                        "Dispatching message"
                    );
                    let _ = backends[idx].tx.send(json).await;
                } else {
                    let n = hash_route_100(&msg.sender_id);
                    let target_label = resolve_label_by_weight(&routing.weights, n)
                        .unwrap_or(&routing.default);

                    let matching: Vec<_> = backends.iter()
                        .filter(|b| b.label == *target_label)
                        .collect();

                    if matching.is_empty() {
                        warn!(
                            message_id = %msg.message_id,
                            sender_id = %msg.sender_id,
                            target_label,
                            hash_n = n,
                            "No backend with target label online (hash+weights)"
                        );
                        drop(backends);
                        send_maintenance_reply(msg).await;
                        return;
                    }

                    let idx = hash_route(&msg.sender_id, matching.len());
                    info!(
                        message_id = %msg.message_id,
                        sender_id = %msg.sender_id,
                        target_label,
                        target_backend = %matching[idx].id,
                        hash_n = n,
                        mode = "hash_weighted",
                        "Dispatching message"
                    );
                    let _ = matching[idx].tx.send(json).await;
                }
            }
        }
    }
}

/// sender_id 哈希取模，稳定路由。
fn hash_route(key: &str, n: usize) -> usize {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    (hasher.finish() as usize) % n
}

/// sender_id 哈希到 0..99，用于 weights 灰度分流。
fn hash_route_100(key: &str) -> u32 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    (hasher.finish() % 100) as u32
}

/// 按 weights 百分比区间找到目标 label。
fn resolve_label_by_weight(
    weights: &std::collections::HashMap<String, u32>,
    n: u32,
) -> Option<&String> {
    let mut sorted: Vec<_> = weights.iter().collect();
    sorted.sort_by_key(|(k, _)| (*k).clone());
    let mut acc = 0u32;
    for (label, pct) in &sorted {
        acc += *pct;
        if n < acc {
            return Some(label);
        }
    }
    None
}

/// 通过 sessionWebhook 回复维护提示。
async fn send_maintenance_reply(msg: &IncomingMessage) {
    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&msg.data) {
        if let Some(webhook) = data.get("sessionWebhook").and_then(|v| v.as_str()) {
            let body = serde_json::json!({
                "msgtype": "text",
                "text": { "content": "请耐心等待，服务维护中..." }
            });
            let client = reqwest::Client::new();
            match client.post(webhook).json(&body)
                .timeout(std::time::Duration::from_secs(5))
                .send().await
            {
                Ok(_) => info!(message_id = %msg.message_id, "Sent maintenance reply"),
                Err(e) => warn!(message_id = %msg.message_id, error = %e, "Failed to send maintenance reply"),
            }
        }
    }
}
