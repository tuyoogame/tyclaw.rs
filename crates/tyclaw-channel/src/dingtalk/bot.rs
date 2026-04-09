//! 钉钉机器人 —— 将 Stream 客户端与 MessageBus 连接起来。

use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info, warn};

use tyclaw_orchestration::{BusHandle, InboundMessage};

use super::credential::TokenManager;
use super::handler::{self, ChatbotHandler};
use super::message::{CallbackMessage, ChatbotMessage};

const CLEAR_KEYWORDS: &[&str] = &["新话题", "/new"];

pub struct DingTalkBot {
    bus_handle: BusHandle,
    token_manager: TokenManager,
    http_client: Client,
    robot_code: String,
    workspace_root: PathBuf,
}

impl DingTalkBot {
    pub fn new(
        bus_handle: BusHandle,
        token_manager: TokenManager,
        robot_code: impl Into<String>,
        workspace_root: impl Into<PathBuf>,
    ) -> Arc<Self> {
        let robot_code_str: String = robot_code.into();
        let workspace_root: PathBuf = workspace_root.into();
        Arc::new(Self {
            bus_handle,
            token_manager,
            http_client: Client::new(),
            robot_code: robot_code_str,
            workspace_root,
        })
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

        let conv_id = if message.conversation_id.is_empty() {
            None
        } else {
            Some(message.conversation_id.clone())
        };

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

        // 轻量回复：发一条文本确认收到
        handler::reply_text(&self.http_client, "🦀 收到", &message).await;
        let emotion_attached = false;

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

                let sent =
                    handler::reply_markdown(&self.http_client, "执行结果", &formatted, &message)
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

                if !response.output_files.is_empty() {
                    match self.token_manager.get_token().await {
                        Ok(token) => {
                            for file_path in &response.output_files {
                                let ext = std::path::Path::new(file_path)
                                    .extension()
                                    .and_then(|e| e.to_str())
                                    .unwrap_or("file");
                                match handler::upload_media(
                                    &self.http_client,
                                    &token,
                                    &self.robot_code,
                                    file_path,
                                    "file",
                                )
                                .await
                                {
                                    Ok(media_id) => {
                                        let fname = std::path::Path::new(file_path)
                                            .file_name()
                                            .and_then(|n| n.to_str())
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
                                    Err(e) => {
                                        error!(file = %file_path, error = %e, "DingTalk: file upload failed")
                                    }
                                }
                            }
                        }
                        Err(e) => error!(error = %e, "DingTalk: failed to get token for file send"),
                    }
                }
                // 撤回所有表情气泡（初始 + 心跳）
                if emotion_attached {
                    if let Ok(token) = self.token_manager.get_token().await {
                        handler::emotion_recall(
                            &self.http_client, &token, &self.robot_code, &message, "🦀收到...",
                        ).await;
                        handler::emotion_recall(
                            &self.http_client, &token, &self.robot_code, &message, "🦀努力中...",
                        ).await;
                    }
                }
                info!(sender = %nick, "DingTalk: request completed");
            }
            Err(e) => {
                error!(sender = %nick, error = %e, "DingTalk: orchestrator error");
                handler::reply_text(&self.http_client, "处理过程出错，请联系管理员。", &message)
                    .await;
            }
        }

        (200, "OK".into())
    }
}
