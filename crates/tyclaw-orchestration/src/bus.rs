//! 消息总线 —— 解耦通道与编排器，per-session 串行保证。
//!
//! 架构：
//! - 入站：多通道（CLI / DingTalk / Timer）通过 `BusHandle` 推送 `InboundMessage`
//! - Bus Task 消费入站消息，per-session 锁保证同 session 串行，不同 session 并发
//! - 出站：有 `reply_tx` 走 oneshot 回传（DingTalk），否则走 outbound mpsc（CLI / Timer）

use std::path::PathBuf;
use std::sync::Arc;
use tyclaw_agent::runtime::HeartbeatSender;

use tokio::sync::{mpsc, oneshot};
use tracing::info;

use tyclaw_agent::runtime::OnProgress;
use tyclaw_types::TyclawError;

use crate::orchestrator::Orchestrator;
use crate::types::{AgentResponse, RequestContext};

/// 入站消息 —— 来自任意通道或 Timer。
pub struct InboundMessage {
    pub content: String,
    pub user_id: String,
    pub user_name: String,
    pub workspace_id: String,
    pub channel: String,
    pub chat_id: String,
    /// 钉钉 conversation_id（群聊时为群 ID）。
    pub conversation_id: Option<String>,
    pub images: Vec<String>,
    pub files: Vec<(PathBuf, String)>,
    /// 同步回复通道：DingTalk 设为 Some，CLI / Timer 设为 None。
    pub reply_tx: Option<oneshot::Sender<Result<AgentResponse, TyclawError>>>,
    /// 是否为 Timer 触发的消息（Bus 据此设置 TIMER_IN_CONTEXT task_local）。
    pub is_timer: bool,
    /// 钉钉 emotion 上下文：(msg_id, conversation_id)，用于心跳 emotion 贴/撤。
    pub emotion_context: Option<(String, String)>,
}

/// 出站事件 —— 通过 outbound mpsc 路由到 Dispatcher。
#[derive(Debug, Clone)]
pub enum OutboundEvent {
    Progress {
        channel: String,
        chat_id: String,
        message: String,
        /// 钉钉 emotion 上下文：(msg_id, conversation_id)，有值时心跳用 emotion API
        emotion_context: Option<(String, String)>,
    },
    Thinking {
        channel: String,
        chat_id: String,
        content: String,
    },
    /// 工具**开始**调用。钉钉卡片可据此刷新工具行，CLI 按文本打印。
    Tool {
        channel: String,
        chat_id: String,
        name: String,
        brief: String,
    },
    Reply {
        channel: String,
        chat_id: String,
        response: AgentResponse,
    },
    Error {
        channel: String,
        chat_id: String,
        message: String,
    },
}

/// 消息总线 —— 入站消费 + 并发处理 + 出站路由。
///
/// 每条入站消息 tokio::spawn 独立处理，不同用户/session 并发执行。
/// 同一 workspace 的串行保证由 Orchestrator 内部的 per-workspace 锁实现。
pub struct MessageBus {
    orchestrator: Arc<Orchestrator>,
    inbound_rx: mpsc::Receiver<InboundMessage>,
    outbound_tx: mpsc::Sender<OutboundEvent>,
}

impl MessageBus {
    /// 创建 Bus + BusHandle + outbound 接收端。
    ///
    /// 返回 `(MessageBus, BusHandle, outbound_rx)`。
    /// - `BusHandle` 可 clone，分发给各通道和 Timer
    /// - `outbound_rx` 交给 Dispatcher 消费
    pub fn new(
        orchestrator: Arc<Orchestrator>,
        inbound_capacity: usize,
        outbound_capacity: usize,
    ) -> (Self, BusHandle, mpsc::Receiver<OutboundEvent>) {
        let (inbound_tx, inbound_rx) = mpsc::channel(inbound_capacity);
        let (outbound_tx, outbound_rx) = mpsc::channel(outbound_capacity);

        let bus = Self {
            orchestrator,
            inbound_rx,
            outbound_tx,
        };

        let handle = BusHandle { inbound_tx };

        (bus, handle, outbound_rx)
    }

    /// 启动 Bus 消费循环。每条消息 `tokio::spawn` 独立处理，不同用户并发。
    pub async fn run(mut self) {
        info!("MessageBus started");
        while let Some(msg) = self.inbound_rx.recv().await {
            let orchestrator = self.orchestrator.clone();
            let outbound_tx = self.outbound_tx.clone();

            tokio::spawn(async move {
                Self::handle_message(msg, &orchestrator, &outbound_tx).await;
            });
        }
        info!("MessageBus stopped (all senders dropped)");
    }

    async fn handle_message(
        msg: InboundMessage,
        orchestrator: &Orchestrator,
        outbound_tx: &mpsc::Sender<OutboundEvent>,
    ) {
        let channel = msg.channel.clone();
        let chat_id = msg.chat_id.clone();

        let progress_cb = Self::make_progress_callback(outbound_tx.clone(), &channel, &chat_id, msg.emotion_context.clone());

        let req = {
            let mut r = RequestContext::new(&msg.user_id, &msg.workspace_id, &msg.channel, &msg.chat_id)
                .with_user_name(&msg.user_name)
                .with_images(msg.images)
                .with_files(
                    msg.files
                        .into_iter()
                        .map(|(p, n)| (p.to_string_lossy().into_owned(), n))
                        .collect(),
                );
            r.conversation_id = msg.conversation_id;
            r
        };

        let channel_owned = channel.clone();
        let chat_id_owned = chat_id.clone();

        let run_future = orchestrator.handle_with_context(&msg.content, &req, Some(&progress_cb));

        // 构建心跳发送器：子任务通过 task_local 转发 [heartbeat] 消息到消息总线
        let heartbeat_outbound = outbound_tx.clone();
        let heartbeat_channel = channel.clone();
        let heartbeat_chat_id = chat_id.clone();
        let heartbeat_emotion = msg.emotion_context.clone();
        let heartbeat_sender: HeartbeatSender = Arc::new(move |msg: String| {
            let _ = heartbeat_outbound.try_send(OutboundEvent::Progress {
                channel: heartbeat_channel.clone(),
                chat_id: heartbeat_chat_id.clone(),
                message: msg,
                emotion_context: heartbeat_emotion.clone(),
            });
        });

        let run_future = tyclaw_agent::runtime::HEARTBEAT_TX.scope(heartbeat_sender, run_future);

        let result = if msg.is_timer {
            tyclaw_tools::timer::TIMER_IN_CONTEXT
                .scope(true, run_future)
                .await
        } else {
            run_future.await
        };

        if let Some(reply_tx) = msg.reply_tx {
            let _ = reply_tx.send(result);
        } else {
            match result {
                Ok(response) => {
                    let _ = outbound_tx
                        .send(OutboundEvent::Reply {
                            channel: channel_owned,
                            chat_id: chat_id_owned,
                            response,
                        })
                        .await;
                }
                Err(e) => {
                    let _ = outbound_tx
                        .send(OutboundEvent::Error {
                            channel: channel_owned,
                            chat_id: chat_id_owned,
                            message: format!("{e}"),
                        })
                        .await;
                }
            }
        }
    }

    fn make_progress_callback(
        outbound_tx: mpsc::Sender<OutboundEvent>,
        channel: &str,
        chat_id: &str,
        emotion_context: Option<(String, String)>,
    ) -> OnProgress {
        use tyclaw_agent::runtime::ProgressEvent;
        let ch = channel.to_string();
        let cid = chat_id.to_string();
        Box::new(move |event: ProgressEvent| {
            let tx = outbound_tx.clone();
            let ch = ch.clone();
            let cid = cid.clone();
            let emo = emotion_context.clone();
            Box::pin(async move {
                // 类型化事件 → OutboundEvent。当前 Outbound 只有 Thinking/Progress 两种
                // 承载进度的变体；Phase 4 卡片渠道会再扩展 ToolStart 等。为了保持 CLI
                // 显示效果不变，非 Thinking 的事件统一落到 Progress，文本由
                // `ProgressEvent::legacy_text()` 还原为改造前的带前缀字符串。
                let outbound = match event {
                    ProgressEvent::Thinking(content) => OutboundEvent::Thinking {
                        channel: ch,
                        chat_id: cid,
                        content,
                    },
                    ProgressEvent::ToolStart { name, brief } => OutboundEvent::Tool {
                        channel: ch,
                        chat_id: cid,
                        name,
                        brief,
                    },
                    other => OutboundEvent::Progress {
                        channel: ch,
                        chat_id: cid,
                        message: other.legacy_text(),
                        emotion_context: emo,
                    },
                };
                let _ = tx.send(outbound).await;
            })
        })
    }
}

/// 入站消息生产者句柄 —— 可 Clone，分发给各通道和 Timer。
#[derive(Clone)]
pub struct BusHandle {
    inbound_tx: mpsc::Sender<InboundMessage>,
}

impl BusHandle {
    /// Fire-and-forget 发送（CLI / Timer 用）。
    pub async fn send(
        &self,
        msg: InboundMessage,
    ) -> Result<(), mpsc::error::SendError<InboundMessage>> {
        self.inbound_tx.send(msg).await
    }

    /// 发送并等待回复（DingTalk 用）。
    ///
    /// 自动构造 oneshot 通道，设置 `reply_tx`，返回 `Result<AgentResponse, TyclawError>`。
    pub async fn send_and_wait(
        &self,
        mut msg: InboundMessage,
    ) -> Result<AgentResponse, TyclawError> {
        let (tx, rx) = oneshot::channel();
        msg.reply_tx = Some(tx);
        self.inbound_tx
            .send(msg)
            .await
            .map_err(|_| TyclawError::Other("Bus channel closed".into()))?;
        rx.await
            .map_err(|_| TyclawError::Other("Bus reply channel closed".into()))?
    }
}
