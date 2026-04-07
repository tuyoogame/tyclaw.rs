//! Gateway WebSocket 客户端 —— 连接 dingtalk-gateway 接收消息。
//!
//! 替代 DingTalkStreamClient 的直连模式，通过 gateway 中转。
//! gateway 负责维护多条到钉钉服务器的连接，按会话亲和分发消息。

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{error, info, warn};

use super::handler::ChatbotHandler;
use super::message::CallbackMessage;

/// Gateway 下发的消息信封。
#[derive(Debug, Deserialize)]
struct GatewayEnvelope {
    #[serde(rename = "type")]
    msg_type: String,
    message_id: String,
    #[allow(dead_code)]
    conversation_id: String,
    #[allow(dead_code)]
    sender_id: String,
    /// 原始钉钉消息 JSON 字符串
    data: String,
}

pub struct GatewayClient {
    gateway_url: String,
    handler: Arc<dyn ChatbotHandler>,
}

impl GatewayClient {
    pub fn new(gateway_url: impl Into<String>, handler: Arc<dyn ChatbotHandler>) -> Self {
        Self {
            gateway_url: gateway_url.into(),
            handler,
        }
    }

    /// 单次连接。断开或出错时返回 Err。
    async fn run_once(&self) -> Result<(), String> {
        info!(url = %self.gateway_url, "Connecting to gateway");

        let (ws_stream, _) = tokio_tungstenite::connect_async(&self.gateway_url)
            .await
            .map_err(|e| format!("Gateway connect failed: {e}"))?;

        info!("Connected to gateway");

        let (write, mut read) = ws_stream.split();
        let write = Arc::new(Mutex::new(write));

        // 注册 label，网关要求 10s 内完成
        {
            let mut w = write.lock().await;
            w.send(WsMessage::Text(r#"{"type":"register","label":"rust"}"#.into()))
                .await
                .map_err(|e| format!("Failed to send register: {e}"))?;
            info!("Gateway: register sent (label=rust)");
        }

        // 心跳：每 30 秒发一次
        let write_hb = write.clone();
        let heartbeat_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                let mut w = write_hb.lock().await;
                if w.send(WsMessage::Text(r#"{"type":"heartbeat"}"#.into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        info!("Gateway: entering read loop");
        while let Some(msg_result) = read.next().await {
            match msg_result {
                Ok(WsMessage::Text(text)) => {
                    info!(len = text.len(), "Gateway: received text message");
                    self.handle_text(&text).await;
                }
                Ok(WsMessage::Ping(data)) => {
                    let mut w = write.lock().await;
                    let _ = w.send(WsMessage::Pong(data)).await;
                }
                Ok(WsMessage::Close(reason)) => {
                    warn!(reason = ?reason, "Gateway closed by server");
                    break;
                }
                Err(e) => {
                    error!(error = %e, "Gateway read error");
                    break;
                }
                _ => {}
            }
        }

        heartbeat_task.abort();
        Err("Gateway disconnected".into())
    }

    async fn handle_text(&self, text: &str) {
        let envelope: GatewayEnvelope = match serde_json::from_str(text) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "Failed to parse gateway envelope");
                return;
            }
        };

        if envelope.msg_type != "message" {
            return;
        }

        let data: serde_json::Value = match serde_json::from_str(&envelope.data) {
            Ok(v) => v,
            Err(e) => {
                warn!(message_id = %envelope.message_id, error = %e, "Failed to parse message data");
                return;
            }
        };

        let callback = CallbackMessage {
            headers: std::collections::HashMap::new(),
            data,
        };

        let msg_id = envelope.message_id.clone();
        let handler = self.handler.clone();
        info!(message_id = %msg_id, "Gateway: dispatching message");
        tokio::spawn(async move {
            let start = std::time::Instant::now();
            let (code, _message) = handler.process(&callback).await;
            info!(
                message_id = %msg_id,
                code,
                elapsed_secs = start.elapsed().as_secs_f64(),
                "Gateway: handler completed"
            );
        });
    }

    /// 连接并持续运行，断开自动重连。
    pub async fn start_forever(&self) {
        let mut retry_delay = 3u64;
        loop {
            match self.run_once().await {
                Ok(()) => break,
                Err(e) => {
                    error!(
                        error = %e,
                        retry_in = retry_delay,
                        "Gateway disconnected, reconnecting..."
                    );
                    tokio::time::sleep(Duration::from_secs(retry_delay)).await;
                    retry_delay = (retry_delay * 2).min(10);
                }
            }
        }
    }
}
