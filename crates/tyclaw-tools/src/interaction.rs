//! 交互工具 —— 允许 Agent 在执行过程中向用户提问。
//!
//! `ask_user` 工具让 Agent 可以在循环中暂停，向用户提问并等待回复，
//! 而不是盲目猜测用户意图。这实现了多轮交互式任务处理。

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::base::{brief_truncate, RiskLevel, Tool};

/// 向用户提问工具。
///
/// 此工具不会真正执行——Agent Loop 检测到此工具调用后会暂停循环，
/// 将问题返回给用户，等待用户回复后再恢复执行。
pub struct AskUserTool;

impl AskUserTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Pause execution and ask the user a question. Use this when you need clarification, \
         confirmation, or additional information before proceeding. Do not use this for information \
         you can obtain from available tools, and do not ask for confirmation on routine safe steps. \
         The agent loop will pause and wait for the user's response before continuing."
    }

    fn brief(&self, args: &HashMap<String, Value>) -> Option<String> {
        let question = args.get("question").and_then(|v| v.as_str())?;
        Some(format!("ask: {}", brief_truncate(question, 60)))
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question or message to present to the user. Be specific about what information you need."
                }
            },
            "required": ["question"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Read
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        // 此方法不会被实际调用——Agent Loop 会拦截 ask_user 工具调用。
        // 如果意外被调用，返回提示信息。
        let question = params
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or("(no question provided)");
        format!("[ask_user] Question sent to user: {question}")
    }
}
