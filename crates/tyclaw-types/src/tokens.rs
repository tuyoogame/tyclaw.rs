//! Token 估算工具 —— 使用 tiktoken 估算消息列表的 token 数量。
//!
//! 用于记忆合并决策：当 session 的 token 数超过 context_window 的 50% 时触发合并。

use serde_json::Value;
use std::collections::HashMap;

/// 估算消息列表的 prompt token 数量。
///
/// 提取所有消息的文本内容，使用 tiktoken cl100k_base 编码计算。
/// 如果提供了 tools 定义，也计入 token 数量。
pub fn estimate_prompt_tokens(
    messages: &[HashMap<String, Value>],
    tools: Option<&[Value]>,
) -> usize {
    let mut parts: Vec<String> = Vec::new();

    for msg in messages {
        if let Some(content) = msg.get("content") {
            match content {
                Value::String(s) => parts.push(s.clone()),
                Value::Array(arr) => {
                    for part in arr {
                        if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(txt) = part.get("text").and_then(|v| v.as_str()) {
                                if !txt.is_empty() {
                                    parts.push(txt.to_string());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if let Some(tool_defs) = tools {
        if let Ok(json_str) = serde_json::to_string(tool_defs) {
            parts.push(json_str);
        }
    }

    let payload = parts.join("\n");
    if payload.is_empty() {
        return 0;
    }

    match tiktoken_rs::cl100k_base() {
        Ok(bpe) => bpe.encode_with_special_tokens(&payload).len(),
        // fallback: 按字符数估算，对 CJK 更准（1 char ≈ 1 token）
        Err(_) => payload.chars().count().max(1),
    }
}

/// 估算单条消息贡献的 token 数量。
pub fn estimate_message_tokens(message: &HashMap<String, Value>) -> usize {
    let mut parts: Vec<String> = Vec::new();

    if let Some(content) = message.get("content") {
        match content {
            Value::String(s) => parts.push(s.clone()),
            Value::Array(arr) => {
                for part in arr {
                    if let Some(obj) = part.as_object() {
                        if obj.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(txt) = obj.get("text").and_then(|v| v.as_str()) {
                                if !txt.is_empty() {
                                    parts.push(txt.to_string());
                                }
                            }
                        } else if let Ok(s) = serde_json::to_string(part) {
                            parts.push(s);
                        }
                    }
                }
            }
            Value::Null => {}
            other => {
                if let Ok(s) = serde_json::to_string(other) {
                    parts.push(s);
                }
            }
        }
    }

    for key in &["name", "tool_call_id"] {
        if let Some(Value::String(v)) = message.get(*key) {
            if !v.is_empty() {
                parts.push(v.clone());
            }
        }
    }

    if let Some(tc) = message.get("tool_calls") {
        if let Ok(s) = serde_json::to_string(tc) {
            parts.push(s);
        }
    }

    let payload = parts.join("\n");
    if payload.is_empty() {
        return 1;
    }

    match tiktoken_rs::cl100k_base() {
        Ok(bpe) => std::cmp::max(1, bpe.encode_with_special_tokens(&payload).len()),
        Err(_) => std::cmp::max(1, payload.len() / 4),
    }
}

/// 估算 prompt token 数量（tiktoken 方式）。
///
/// 返回 (token_count, source_label)。
pub fn estimate_prompt_tokens_chain(
    messages: &[HashMap<String, Value>],
    tools: Option<&[Value]>,
) -> (usize, &'static str) {
    let estimated = estimate_prompt_tokens(messages, tools);
    if estimated > 0 {
        (estimated, "tiktoken")
    } else {
        (0, "none")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_estimate_prompt_tokens() {
        let msgs = vec![{
            let mut m = HashMap::new();
            m.insert("role".into(), json!("user"));
            m.insert("content".into(), json!("Hello, world!"));
            m
        }];
        let tokens = estimate_prompt_tokens(&msgs, None);
        assert!(tokens > 0);
    }

    #[test]
    fn test_estimate_message_tokens() {
        let mut msg = HashMap::new();
        msg.insert("role".into(), json!("user"));
        msg.insert("content".into(), json!("Hello!"));
        let tokens = estimate_message_tokens(&msg);
        assert!(tokens >= 1);
    }

    #[test]
    fn test_empty_message() {
        let msg = HashMap::new();
        assert_eq!(estimate_message_tokens(&msg), 1);
    }
}
