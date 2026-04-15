//! 上游连接池 —— 管理多条到钉钉服务器的 WebSocket 连接。
//!
//! 每条连接独立运行，收到消息后推入统一队列。
//! 连接断开自动重连。

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{error, info, warn};

/// 钉钉 Stream 帧结构。
#[derive(Debug, Deserialize)]
pub struct StreamFrame {
    #[serde(rename = "specVersion")]
    #[allow(dead_code)]
    pub spec_version: Option<String>,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    pub frame_type: Option<String>,
    pub headers: Option<serde_json::Value>,
    pub data: Option<String>,
}

/// ACK 消息。
#[derive(Serialize)]
struct AckMessage {
    code: u32,
    headers: serde_json::Value,
    message: String,
    data: String,
}

/// 连接开启响应。
#[derive(Deserialize)]
struct ConnectionResponse {
    endpoint: String,
    ticket: String,
}

/// 从上游收到的消息（已解析，准备分发给下游）。
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    /// 原始 data JSON 字符串
    pub data: String,
    /// 消息 ID（用于去重）
    pub message_id: String,
    /// 会话 ID（用于会话亲和路由）
    pub conversation_id: String,
    /// 发送者
    pub sender_id: String,
    /// 上游连接编号
    #[allow(dead_code)]
    pub conn_id: usize,
    /// 钉钉 Stream 帧 topic（用于区分聊天消息 / 卡片回调等）
    pub topic: String,
}

/// 启动上游连接池。
///
/// 创建 `count` 条到钉钉的 WebSocket 连接，所有消息汇入 `tx`。
pub fn start_pool(
    client_id: String,
    client_secret: String,
    count: usize,
    tx: mpsc::Sender<IncomingMessage>,
) {
    for i in 0..count {
        let cid = client_id.clone();
        let cs = client_secret.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            // 错开连接启动，避免同时发起多个 WebSocket 握手被钉钉限流
            if i > 0 {
                tokio::time::sleep(Duration::from_millis(i as u64 * 500)).await;
            }
            run_connection(i, cid, cs, tx).await;
        });
    }
    info!(count, "Upstream connection pool started");
}

/// 单条上游连接的生命周期（含自动重连）。
async fn run_connection(
    conn_id: usize,
    client_id: String,
    client_secret: String,
    tx: mpsc::Sender<IncomingMessage>,
) {
    let http = reqwest::Client::new();
    loop {
        info!(conn_id, "Opening upstream connection");
        match open_and_run(conn_id, &client_id, &client_secret, &http, &tx).await {
            Ok(()) => info!(conn_id, "Connection closed normally"),
            Err(e) => warn!(conn_id, error = %e, "Connection failed"),
        }
        // 重连间隔：2-5 秒随机（避免雷群效应）
        let delay = 2 + (conn_id % 4) as u64;
        info!(conn_id, delay_s = delay, "Reconnecting");
        tokio::time::sleep(Duration::from_secs(delay)).await;
    }
}

/// 注册连接 + WebSocket 读循环。
async fn open_and_run(
    conn_id: usize,
    client_id: &str,
    client_secret: &str,
    http: &reqwest::Client,
    tx: &mpsc::Sender<IncomingMessage>,
) -> Result<(), String> {
    // 1. 向钉钉注册连接
    let body = serde_json::json!({
        "clientId": client_id,
        "clientSecret": client_secret,
        "subscriptions": [
            {"type": "CALLBACK", "topic": "/v1.0/im/bot/messages/get"},
            {"type": "CALLBACK", "topic": "/v1.0/card/instances/callback"},
        ],
        "ua": "dingtalk-gateway/0.1",
    });
    let resp: ConnectionResponse = http
        .post("https://api.dingtalk.com/v1.0/gateway/connections/open")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON parse error: {e}"))?;

    let ws_url = format!("{}?ticket={}", resp.endpoint, resp.ticket);
    info!(conn_id, endpoint = %resp.endpoint, "Connection registered");

    // 2. 建立 WebSocket
    let (ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .map_err(|e| format!("WebSocket connect failed: {e}"))?;
    let (mut write, mut read) = ws.split();
    info!(conn_id, "WebSocket connected");

    // 3. 读循环
    while let Some(msg) = read.next().await {
        let text = match msg {
            Ok(WsMessage::Text(t)) => {
                info!(conn_id, len = t.len(), "Received frame from DingTalk");
                t
            }
            Ok(WsMessage::Ping(d)) => {
                let _ = write.send(WsMessage::Pong(d)).await;
                continue;
            }
            Ok(WsMessage::Close(reason)) => {
                info!(conn_id, ?reason, "WebSocket closed by server");
                break;
            }
            Ok(_) => continue,
            Err(e) => {
                warn!(conn_id, error = %e, "WebSocket read error");
                break;
            }
        };

        // 解析帧
        let frame: StreamFrame = match serde_json::from_str(&text) {
            Ok(f) => f,
            Err(e) => {
                warn!(conn_id, error = %e, "Failed to parse frame");
                continue;
            }
        };

        // 发送 ACK
        let msg_id = frame
            .headers
            .as_ref()
            .and_then(|h| h.get("messageId"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let ack = AckMessage {
            code: 200,
            headers: serde_json::json!({"contentType": "application/json", "messageId": msg_id}),
            message: "OK".into(),
            data: "{}".into(),
        };
        if let Ok(ack_json) = serde_json::to_string(&ack) {
            let _ = write.send(WsMessage::Text(ack_json.into())).await;
        }

        // 提取消息内容
        let data = match frame.data {
            Some(d) if !d.is_empty() => d,
            _ => {
                info!(conn_id, message_id = %msg_id, frame_type = ?frame.frame_type, "Upstream: frame has no data, skipping");
                continue;
            }
        };

        let topic = frame
            .headers
            .as_ref()
            .and_then(|h| h.get("topic"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // 解析路由字段（卡片回调用 userId，聊天消息用 senderStaffId/senderId）
        let parsed: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
        let conversation_id = parsed
            .get("conversationId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let sender_id = parsed
            .get("senderStaffId")
            .or_else(|| parsed.get("senderId"))
            .or_else(|| parsed.get("userId"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        info!(
            conn_id,
            message_id = %msg_id,
            topic = %topic,
            conversation_id = %conversation_id,
            sender_id = %sender_id,
            data_len = data.len(),
            "Upstream: received message, forwarding to dispatcher"
        );

        let incoming = IncomingMessage {
            data,
            message_id: msg_id.clone(),
            conversation_id,
            sender_id,
            conn_id,
            topic,
        };

        if tx.send(incoming).await.is_err() {
            error!(conn_id, "Message queue full or closed");
            break;
        }
    }

    Ok(())
}
