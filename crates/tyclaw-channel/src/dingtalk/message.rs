//! 钉钉消息类型定义。
//!
//! 包含：
//! - CallbackMessage：WebSocket 收到的原始回调消息
//! - ChatbotMessage：解析后的聊天机器人消息
//! - AckMessage：消息确认响应

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// ── WebSocket 帧 ─────────────────────────────────────────────

/// WebSocket 收到的消息帧。
///
/// 钉钉 Stream 协议的消息格式：
/// ```json
/// {
///   "type": "CALLBACK",
///   "headers": { "messageId": "...", "topic": "...", ... },
///   "data": "json_string"
/// }
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct StreamFrame {
    /// 消息类型：CALLBACK、EVENT、SYSTEM
    #[serde(rename = "type")]
    pub msg_type: String,

    /// 消息头：包含 messageId、topic、appId 等
    #[serde(default)]
    pub headers: HashMap<String, Value>,

    /// 消息体：JSON 字符串（需要二次解析）
    #[serde(default)]
    pub data: Value,
}

impl StreamFrame {
    /// 获取消息 ID。
    pub fn message_id(&self) -> &str {
        self.headers
            .get("messageId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }

    /// 获取消息主题。
    pub fn topic(&self) -> &str {
        self.headers
            .get("topic")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }
}

// ── CallbackMessage ──────────────────────────────────────────

/// 回调消息 —— 从 StreamFrame 解析出的结构化回调。
#[derive(Debug, Clone)]
pub struct CallbackMessage {
    /// 消息头
    pub headers: HashMap<String, Value>,
    /// 消息体（已解析的 JSON）
    pub data: Value,
}

impl CallbackMessage {
    /// 从 StreamFrame 创建。
    pub fn from_frame(frame: &StreamFrame) -> Self {
        // data 可能是 JSON 字符串，需要尝试二次解析
        let data = if let Some(s) = frame.data.as_str() {
            serde_json::from_str(s).unwrap_or(Value::String(s.to_string()))
        } else {
            frame.data.clone()
        };

        Self {
            headers: frame.headers.clone(),
            data,
        }
    }
}

// ── ChatbotMessage ───────────────────────────────────────────

/// 聊天机器人消息 —— 从 CallbackMessage.data 解析的用户消息。
///
/// 钉钉机器人收到的消息结构，包含发送者信息、会话信息、消息内容等。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatbotMessage {
    /// 消息 ID
    #[serde(default)]
    pub msg_id: String,

    /// 会话类型："1" = 单聊，"2" = 群聊
    #[serde(default)]
    pub conversation_type: String,

    /// 会话 ID（群聊时为群 ID）
    #[serde(default)]
    pub conversation_id: String,

    /// 发送者 ID
    #[serde(default)]
    pub sender_id: String,

    /// 发送者员工 ID
    #[serde(default)]
    pub sender_staff_id: String,

    /// 发送者昵称
    #[serde(default)]
    pub sender_nick: String,

    /// 消息类型：text、picture、richText、file
    #[serde(default)]
    pub msgtype: String,

    /// 文本消息内容（msgtype=text 时）
    #[serde(default)]
    pub text: Option<TextContent>,

    /// 消息内容体（图片、富文本等）
    #[serde(default)]
    pub content: Option<Value>,

    /// 扩展信息
    #[serde(default)]
    pub extensions: HashMap<String, Value>,

    /// 回复用的 Webhook URL（每条消息唯一，有效期有限）
    #[serde(default)]
    pub session_webhook: String,

    /// 被 @ 的用户列表
    #[serde(default, rename = "atUsers")]
    pub at_users: Vec<Value>,

    /// 消息创建时间戳
    #[serde(default, rename = "createAt")]
    pub create_at: i64,

    /// 机器人代码
    #[serde(default)]
    pub robot_code: String,
}

/// 文本消息内容。
#[derive(Debug, Clone, Deserialize)]
pub struct TextContent {
    #[serde(default)]
    pub content: String,
}

/// 聊天机器人消息的主题常量。
impl ChatbotMessage {
    /// 钉钉机器人消息的订阅主题。
    pub const TOPIC: &'static str = "/v1.0/im/bot/messages/get";

    /// 构造一个只包含 msg_id 和 conversation_id 的最小实例（用于 emotion API 调用）。
    pub fn with_ids(msg_id: &str, conversation_id: &str) -> Self {
        Self {
            msg_id: msg_id.to_string(),
            conversation_id: conversation_id.to_string(),
            ..Default::default()
        }
    }

    /// 从 JSON Value 反序列化。
    pub fn from_value(data: &Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(data.clone())
    }

    /// 判断是否为单聊（私聊）。
    pub fn is_private(&self) -> bool {
        self.conversation_type == "1"
    }

    /// 提取文本内容列表。
    ///
    /// 处理 text 和 richText 两种消息类型：
    /// - text 类型：直接从 text.content 提取
    /// - richText 类型：遍历 richText 数组提取所有 text 段
    pub fn get_text_list(&self) -> Vec<String> {
        let mut texts = Vec::new();

        // 从 text 字段提取
        if let Some(ref tc) = self.text {
            let t = tc.content.trim().to_string();
            if !t.is_empty() {
                texts.push(t);
            }
        }

        // 从 richText content 中提取
        if self.msgtype == "richText" {
            if let Some(ref content) = self.content {
                if let Some(rich_text) = content.get("richText").and_then(|v| v.as_array()) {
                    for item in rich_text {
                        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                            let t = text.trim().to_string();
                            if !t.is_empty() {
                                texts.push(t);
                            }
                        }
                    }
                }
            }
        }

        texts
    }

    /// 提取文件信息（downloadCode 和 fileName）。
    ///
    /// 仅处理 msgtype=file 的消息。
    /// 返回 Vec<(download_code, file_name)>
    pub fn get_file_list(&self) -> Vec<(String, String)> {
        let mut files = Vec::new();

        if self.msgtype == "file" {
            if let Some(ref content) = self.content {
                let code = content
                    .get("downloadCode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let name = content
                    .get("fileName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown_file");
                if !code.is_empty() {
                    files.push((code.to_string(), name.to_string()));
                }
            }
        }

        files
    }

    /// 提取图片下载码列表。
    ///
    /// 处理 picture 和 richText 中嵌入的图片：
    /// - picture 类型：从 content.downloadCode 提取
    /// - richText 类型：遍历 richText 数组中所有 type=picture 的项
    pub fn get_image_list(&self) -> Vec<String> {
        let mut codes = Vec::new();

        if self.msgtype == "picture" {
            if let Some(ref content) = self.content {
                if let Some(code) = content.get("downloadCode").and_then(|v| v.as_str()) {
                    if !code.is_empty() {
                        codes.push(code.to_string());
                    }
                }
            }
        }

        // richText 中的图片
        if self.msgtype == "richText" {
            if let Some(ref content) = self.content {
                if let Some(rich_text) = content.get("richText").and_then(|v| v.as_array()) {
                    for item in rich_text {
                        if item.get("type").and_then(|v| v.as_str()) == Some("picture") {
                            if let Some(code) = item.get("downloadCode").and_then(|v| v.as_str()) {
                                if !code.is_empty() {
                                    codes.push(code.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        codes
    }
}

// ── AckMessage ───────────────────────────────────────────────

/// ACK 确认消息 —— 通过 WebSocket 发回给钉钉的确认响应。
///
/// 收到消息后必须发送 ACK，否则钉钉会重发。
#[derive(Debug, Clone, Serialize)]
pub struct AckMessage {
    pub code: u16,
    pub headers: HashMap<String, Value>,
    pub message: String,
    pub data: String,
}

impl AckMessage {
    /// 成功状态码
    pub const STATUS_OK: u16 = 200;
    /// 客户端错误
    pub const STATUS_BAD_REQUEST: u16 = 400;
    /// 未实现
    pub const STATUS_NOT_FOUND: u16 = 404;
    /// 服务器错误
    pub const STATUS_ERROR: u16 = 500;

    /// 创建成功确认。
    pub fn ok(headers: HashMap<String, Value>) -> Self {
        Self {
            code: Self::STATUS_OK,
            headers,
            message: "OK".into(),
            data: String::new(),
        }
    }

    /// 创建错误确认。
    pub fn error(headers: HashMap<String, Value>, message: impl Into<String>) -> Self {
        Self {
            code: Self::STATUS_ERROR,
            headers,
            message: message.into(),
            data: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_chatbot_message() {
        let data = json!({
            "msgId": "msg123",
            "conversationType": "1",
            "conversationId": "conv456",
            "senderStaffId": "staff789",
            "senderNick": "张三",
            "msgtype": "text",
            "text": { "content": "你好" },
            "sessionWebhook": "https://oapi.dingtalk.com/robot/sendBySession",
        });
        let msg: ChatbotMessage = serde_json::from_value(data).unwrap();
        assert!(msg.is_private());
        assert_eq!(msg.sender_nick, "张三");
        assert_eq!(msg.get_text_list(), vec!["你好"]);
    }

    #[test]
    fn test_parse_image_message() {
        let data = json!({
            "msgtype": "picture",
            "conversationType": "2",
            "content": { "downloadCode": "img_code_123" },
            "sessionWebhook": "https://example.com",
        });
        let msg: ChatbotMessage = serde_json::from_value(data).unwrap();
        assert!(!msg.is_private());
        assert_eq!(msg.get_image_list(), vec!["img_code_123"]);
    }

    #[test]
    fn test_parse_file_message() {
        let data = json!({
            "msgtype": "file",
            "conversationType": "1",
            "content": {
                "downloadCode": "file_code_456",
                "fileName": "report.xlsx",
                "fileSize": "12345",
            },
            "sessionWebhook": "https://example.com",
        });
        let msg: ChatbotMessage = serde_json::from_value(data).unwrap();
        let files = msg.get_file_list();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "file_code_456");
        assert_eq!(files[0].1, "report.xlsx");
    }

    #[test]
    fn test_ack_message_serialize() {
        let ack = AckMessage::ok(HashMap::new());
        let json = serde_json::to_value(&ack).unwrap();
        assert_eq!(json["code"], 200);
        assert_eq!(json["message"], "OK");
    }
}
