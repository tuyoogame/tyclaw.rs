//! 钉钉 Stream 客户端 —— TyClaw 的钉钉机器人通道。
//!
//! 子模块说明：
//! - `credential`: 钉钉凭证与 token 管理
//! - `message`: Stream/Callback/Chatbot/Ack 消息结构
//! - `handler`: 消息处理与回复/下载/上传能力
//! - `stream`: WebSocket stream 客户端
//! - `bot`: 编排器适配层（把消息交给 Orchestrator）

pub mod ai_card;
pub mod bot;
pub mod credential;
pub mod gateway;
pub mod handler;
pub mod message;
pub mod stream;

pub use ai_card::{
    new_card_registry, AiCardCallbackHandler, AiCardRegistry, CardReplier, CARD_CALLBACK_TOPIC,
};
pub use bot::DingTalkBot;
pub use credential::{Credential, TokenManager};
pub use gateway::GatewayClient;
pub use handler::ChatbotHandler;
pub use message::{AckMessage, CallbackMessage, ChatbotMessage};
pub use stream::DingTalkStreamClient;
