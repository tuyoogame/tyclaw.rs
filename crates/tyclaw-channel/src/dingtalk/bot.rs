//! 钉钉机器人 —— 将 Stream 客户端与 MessageBus 连接起来。

use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info, warn};

use tyclaw_orchestration::{BusHandle, InboundMessage, Orchestrator};

use super::ai_card::{self, AiCardRegistry, CardReplier};
use super::credential::TokenManager;
use super::handler::{self, ChatbotHandler};
use super::message::{CallbackMessage, ChatbotMessage};

const CLEAR_KEYWORDS: &[&str] = &["新话题", "/new"];

/// 终止当前任务的关键字。用户发这些中任一条消息都会触发 cancel。
const CANCEL_KEYWORDS: &[&str] = &[
    "终止", "停止", "终止任务", "停止任务", "暂停", "stop", "cancel", "/stop", "/cancel",
];

pub struct DingTalkBot {
    bus_handle: BusHandle,
    orchestrator: Arc<Orchestrator>,
    token_manager: TokenManager,
    http_client: Client,
    robot_code: String,
    workspace_root: PathBuf,
    /// 钉钉 AI 卡片模板 id。`None` 时走纯文本回复、不启用卡片动画。
    card_template_id: Option<String>,
    /// 共享的卡片注册表（供 outbound dispatcher 查找同一 chat_id 的 replier）。
    card_registry: AiCardRegistry,
}

impl DingTalkBot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bus_handle: BusHandle,
        orchestrator: Arc<Orchestrator>,
        token_manager: TokenManager,
        robot_code: impl Into<String>,
        workspace_root: impl Into<PathBuf>,
        card_template_id: Option<String>,
        card_registry: AiCardRegistry,
    ) -> Arc<Self> {
        let robot_code_str: String = robot_code.into();
        let workspace_root: PathBuf = workspace_root.into();
        Arc::new(Self {
            bus_handle,
            orchestrator,
            token_manager,
            http_client: Client::new(),
            robot_code: robot_code_str,
            workspace_root,
            card_template_id,
            card_registry,
        })
    }

    /// 根据当前消息尝试创建 AI 卡片。
    /// 模板未配置或创建失败时返回 `None`——调用方据此决定是否回退到纯文本。
    async fn try_create_card(
        &self,
        question: &str,
        nick: &str,
        staff_id: &str,
        conversation_id: &str,
        is_private: bool,
    ) -> Option<Arc<CardReplier>> {
        let template_id = self.card_template_id.as_deref()?;
        let header = format!("**{nick}:** {question}");
        match CardReplier::create(
            self.http_client.clone(),
            self.token_manager.clone(),
            self.robot_code.clone(),
            template_id,
            header,
            conversation_id,
            staff_id,
            is_private,
        )
        .await
        {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "Failed to create AI card, falling back to text");
                None
            }
        }
    }
}

#[async_trait]
impl ChatbotHandler for DingTalkBot {
    async fn process(&self, callback: &CallbackMessage) -> (u16, String) {
        let message = match ChatbotMessage::from_value(&callback.data) {
            Ok(msg) => msg,
            Err(e) => {
                error!(error = %e, "Failed to parse ChatbotMessage");
                return (500, format!("Parse error: {e}"));
            }
        };

        let staff_id = message.sender_staff_id.clone();
        info!(
            msgtype = %message.msgtype,
            sender = %message.sender_staff_id,
            conversation_type = %message.conversation_type,
            conversation_id = %message.conversation_id,
            "DingTalk: received message"
        );
        let nick = if message.sender_nick.is_empty() {
            "unknown".to_string()
        } else {
            message.sender_nick.clone()
        };

        let text_parts = message.get_text_list();
        let question = text_parts
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");

        let (channel, chat_id) = if message.is_private() {
            ("dingtalk_private", staff_id.clone())
        } else {
            (
                "dingtalk_group",
                format!("{}:{}", message.conversation_id, staff_id),
            )
        };

        let clear_keywords: HashSet<&str> = CLEAR_KEYWORDS.iter().copied().collect();
        let cancel_keywords: HashSet<&str> =
            CANCEL_KEYWORDS.iter().map(|k| *k).collect();

        let conv_id = if message.conversation_id.is_empty() {
            None
        } else {
            Some(message.conversation_id.clone())
        };

        // 关键字拦截：终止任务。由 Orchestrator 内部解析 workspace_key，
        // 避免外部重复 key 策略。
        let trimmed_q = question.trim().to_lowercase();
        if cancel_keywords.contains(trimmed_q.as_str()) {
            let cancelled = self.orchestrator.cancel_for_identity(
                &staff_id,
                channel,
                &chat_id,
                conv_id.as_deref(),
            );
            let reply = if cancelled {
                "⏹ 已请求终止当前任务，正在收尾..."
            } else {
                "当前没有正在运行的任务。"
            };
            handler::reply_text(&self.http_client, reply, &message).await;
            return (200, "OK".into());
        }

        if clear_keywords.contains(question.as_str()) {
            let msg = InboundMessage {
                content: "/new".into(),
                user_id: staff_id.clone(),
                user_name: nick.clone(),
                workspace_id: "default".into(),
                channel: channel.into(),
                chat_id: chat_id.clone(),
                conversation_id: conv_id.clone(),
                images: vec![],
                files: vec![],
                reply_tx: None,
                is_timer: false,
                emotion_context: None,
            };
            match self.bus_handle.send_and_wait(msg).await {
                Ok(response) => {
                    handler::reply_text(&self.http_client, &response.text, &message).await;
                }
                Err(e) => {
                    handler::reply_text(&self.http_client, &format!("清除会话失败: {e}"), &message)
                        .await;
                }
            }
            return (200, "OK".into());
        }

        let image_codes = message.get_image_list();
        let file_list = message.get_file_list();
        if question.is_empty() && image_codes.is_empty() && file_list.is_empty() {
            handler::reply_text(&self.http_client, "你好，请发送你的问题~", &message).await;
            return (200, "OK".into());
        }

        // 尝试创建 AI 卡片。成功则注册到 registry（outbound dispatcher 据此 feed 进度），
        // 失败或未配置模板都返回 None，走老的纯文本路径。
        let card = self
            .try_create_card(
                &question,
                &nick,
                &staff_id,
                &message.conversation_id,
                message.is_private(),
            )
            .await;
        if let Some(ref c) = card {
            ai_card::register_card(&self.card_registry, &chat_id, Arc::clone(c));
        }

        // 表情气泡和卡片可以共存：表情贴在用户原消息上，卡片是新消息。
        let emotion_attached = if let Ok(token) = self.token_manager.get_token().await {
            handler::emotion_reply(
                &self.http_client,
                &token,
                &self.robot_code,
                &message,
                "🦀收到...",
            )
            .await
        } else {
            false
        };

        let mut image_data_uris = Vec::new();
        if !image_codes.is_empty() {
            match self.token_manager.get_token().await {
                Ok(token) => {
                    for code in &image_codes {
                        match handler::download_image_as_data_uri(
                            &self.http_client,
                            &token,
                            &self.robot_code,
                            code,
                        )
                        .await
                        {
                            Ok(uri) => image_data_uris.push(uri),
                            Err(e) => {
                                error!(code = %code, error = %e, "DingTalk: image download failed")
                            }
                        }
                    }
                }
                Err(e) => error!(error = %e, "DingTalk: failed to get token for image download"),
            }
        }

        let mut file_attachments = Vec::new();
        if !file_list.is_empty() {
            // 附件临时保存到 run-dir 下的 tmp，orchestrator 会复制到正确的 workspace
            let save_dir = self
                .workspace_root
                .join("tmp")
                .join("attachments");
            match self.token_manager.get_token().await {
                Ok(token) => {
                    for (code, name) in &file_list {
                        match handler::download_file(
                            &self.http_client,
                            &token,
                            &self.robot_code,
                            code,
                            &save_dir,
                            name,
                        )
                        .await
                        {
                            Ok(path) => file_attachments.push((path, name.clone())),
                            Err(e) => {
                                error!(file = %name, error = %e, "DingTalk: file download failed")
                            }
                        }
                    }
                }
                Err(e) => error!(error = %e, "DingTalk: failed to get token for file download"),
            }
        }

        let question_full = if question.is_empty() && !image_data_uris.is_empty() {
            "请查看图片并分析。".to_string()
        } else if question.is_empty() && !file_attachments.is_empty() {
            "请查看附件文件并分析。".to_string()
        } else {
            question
        };
        let question_for_header: String = question_full.chars().take(50).collect();

        let msg = InboundMessage {
            content: question_full,
            user_id: staff_id.clone(),
            user_name: nick.clone(),
            workspace_id: "default".into(),
            channel: channel.into(),
            chat_id: chat_id.clone(),
            conversation_id: conv_id,
            images: image_data_uris,
            files: file_attachments
                .iter()
                .map(|(p, n)| (std::path::PathBuf::from(p), n.clone()))
                .collect(),
            reply_tx: None,
            is_timer: false,
            emotion_context: Some((message.msg_id.clone(), message.conversation_id.clone())),
        };

        let result = self.bus_handle.send_and_wait(msg).await;

        match result {
            Ok(response) => {
                let mut reply_text = response.text;
                // 空回复不发送（如消息注入到运行中的 agent loop 时）
                if reply_text.trim().is_empty() {
                    info!("DingTalk: empty response, skipping reply");
                    return (200, "OK".into());
                }
                if reply_text.len() > 20000 {
                    reply_text.truncate(20000);
                    reply_text.push_str("\n\n...（内容过长已截断）");
                }

                // 拼接头尾小字：用户问题 + 处理时长 + token 用量
                let question_preview = &question_for_header;
                let duration_secs = response.duration_seconds.round() as u64;
                let header = if message.is_private() {
                    format!("<font size=2 color=#888888>您的问题：{question_preview}</font>")
                } else {
                    format!("<font size=2 color=#888888>{nick} 的问题：{question_preview}</font>")
                };
                let total_tokens = response.prompt_tokens + response.completion_tokens;
                let footer = if total_tokens > 0 {
                    format!(
                        "<font size=2 color=#888888>🦀 {duration_secs}秒，{total_tokens} tokens</font>"
                    )
                } else {
                    format!("<font size=2 color=#888888>🦀 {duration_secs}秒</font>")
                };
                let formatted = format!("{header}\n\n{reply_text}\n\n{footer}");

                // 有卡片时写入最终态；如果卡片 finalize 失败，fallback 到纯文本。
                // 卡片场景：不需要 header（问题已在"输出中"阶段展示过），只要正文+footer。
                let card_content = format!("{reply_text}\n\n{footer}");
                let mut need_text_fallback = card.is_none();
                if let Some(ref c) = card {
                    ai_card::unregister_card(&self.card_registry, &chat_id);
                    if let Err(e) = c.finalize(&card_content).await {
                        warn!(error = %e, "AI card finalize failed, falling back to text reply");
                        need_text_fallback = true;
                    }
                }
                if need_text_fallback {
                    let sent = handler::reply_markdown(
                        &self.http_client,
                        "执行结果",
                        &formatted,
                        &message,
                    )
                    .await;

                    if !sent {
                        warn!("DingTalk: webhook failed, falling back to proactive API");
                        if let Ok(token) = self.token_manager.get_token().await {
                            handler::send_text_proactive(
                                &self.http_client,
                                &token,
                                &self.robot_code,
                                &message,
                                &reply_text,
                            )
                            .await;
                        }
                    }
                }

                if !response.output_files.is_empty() {
                    match self.token_manager.get_token().await {
                        Ok(token) => {
                            for file_path in &response.output_files {
                                let is_image = handler::is_image_file(file_path);
                                let media_type = if is_image { "image" } else { "file" };
                                match handler::upload_media(
                                    &self.http_client,
                                    &token,
                                    &self.robot_code,
                                    file_path,
                                    media_type,
                                )
                                .await
                                {
                                    Ok(media_id) => {
                                        if is_image {
                                            if let Err(e) = handler::reply_image(
                                                &self.http_client,
                                                &token,
                                                &self.robot_code,
                                                &message,
                                                &media_id,
                                            )
                                            .await
                                            {
                                                error!(file = %file_path, error = %e, "DingTalk: image send failed");
                                            }
                                        } else {
                                            let fname = std::path::Path::new(file_path)
                                                .file_name()
                                                .and_then(|n| n.to_str())
                                                .unwrap_or("file");
                                            let ext = std::path::Path::new(file_path)
                                                .extension()
                                                .and_then(|e| e.to_str())
                                                .unwrap_or("file");
                                            if let Err(e) = handler::reply_file(
                                                &self.http_client,
                                                &token,
                                                &self.robot_code,
                                                &message,
                                                &media_id,
                                                fname,
                                                ext,
                                            )
                                            .await
                                            {
                                                error!(file = %file_path, error = %e, "DingTalk: file send failed");
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        error!(file = %file_path, error = %e, "DingTalk: {media_type} upload failed")
                                    }
                                }
                            }
                        }
                        Err(e) => error!(error = %e, "DingTalk: failed to get token for file send"),
                    }
                }
                // 撤回"收到"表情气泡
                if emotion_attached {
                    if let Ok(token) = self.token_manager.get_token().await {
                        handler::emotion_recall(
                            &self.http_client, &token, &self.robot_code, &message, "🦀收到...",
                        ).await;
                    }
                }
                info!(sender = %nick, "DingTalk: request completed");
            }
            Err(e) => {
                error!(sender = %nick, error = %e, "DingTalk: orchestrator error");
                if let Some(ref c) = card {
                    ai_card::unregister_card(&self.card_registry, &chat_id);
                    c.terminate().await;
                } else {
                    handler::reply_text(
                        &self.http_client,
                        "处理过程出错，请联系管理员。",
                        &message,
                    )
                    .await;
                }
            }
        }

        (200, "OK".into())
    }
}
