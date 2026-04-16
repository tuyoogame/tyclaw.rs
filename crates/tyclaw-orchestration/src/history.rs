//! 历史消息处理：去重、裁剪、tool_call 配对修复。

use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// 统一去重归一化策略：
/// 1) 压缩连续空白；2) 全部转小写。
/// 这样可以把"仅大小写或空白差异"的文本视为同一条。
pub(crate) fn normalize_text_for_dedupe(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// 构造消息签名，用于 history 去重。
/// 当前按 role/name/content 维度做轻量去重，不依赖外部哈希库。
pub(crate) fn message_signature(message: &HashMap<String, Value>) -> String {
    let role = message.get("role").and_then(|v| v.as_str()).unwrap_or("");
    let content = message
        .get("content")
        .and_then(|v| v.as_str())
        .map(normalize_text_for_dedupe)
        .unwrap_or_default();
    let name = message.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let tool_call_id = message
        .get("tool_call_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_calls = message
        .get("tool_calls")
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .unwrap_or_default();
    format!("{role}|{name}|{tool_call_id}|{content}|{tool_calls}")
}

/// 对历史消息做"保守去重"：
/// - 仅去除连续重复消息
/// - 不做全局去重，避免打断 assistant(tool_calls) 与 tool(tool_call_id) 的配对关系
pub(crate) fn dedupe_history(history: &[HashMap<String, Value>]) -> Vec<HashMap<String, Value>> {
    let mut result = Vec::with_capacity(history.len());
    let mut last_sig = String::new();

    for msg in history {
        let sig = message_signature(msg);
        if sig == last_sig {
            continue;
        }
        last_sig = sig;
        result.push(msg.clone());
    }
    result
}

/// 按 token 预算裁剪历史：
/// 从最近消息向前回溯，尽量保留最新上下文。
/// 如果第一条就超预算，仍保留一条，避免 history 为空。
pub(crate) fn trim_history_by_token_budget(
    history: &[HashMap<String, Value>],
    budget_tokens: usize,
) -> Vec<HashMap<String, Value>> {
    if history.is_empty() || budget_tokens == 0 {
        return Vec::new();
    }
    let mut total = 0usize;
    let mut selected_reversed: Vec<HashMap<String, Value>> = Vec::new();

    for msg in history.iter().rev() {
        let t = tyclaw_types::tokens::estimate_message_tokens(msg);
        if !selected_reversed.is_empty() && total + t > budget_tokens {
            break;
        }
        total += t;
        selected_reversed.push(msg.clone());
    }

    selected_reversed.reverse();
    selected_reversed
}

/// 保障 tool 消息配对关系：
/// - tool 消息必须能在"紧邻之前的 assistant.tool_calls"里找到对应 id
/// - 不满足条件的 tool 消息会被丢弃，避免上游 provider（如 Anthropic）400
///
/// 注意：`add_tool_result` 处理图片时会在 tool 消息之间插入 user 消息（携带
/// image blocks），因此 user 消息不应清空 expected_tool_ids，否则同一轮后续
/// 的 tool result 会被误判为孤立消息而丢弃。
pub(crate) fn enforce_tool_call_pairing(
    history: &[HashMap<String, Value>],
) -> Vec<HashMap<String, Value>> {
    let mut cleaned = Vec::with_capacity(history.len());
    let mut expected_tool_ids: HashSet<String> = HashSet::new();

    for msg in history {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");

        if role == "assistant" {
            expected_tool_ids.clear();
            if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tool_calls {
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        expected_tool_ids.insert(id.to_string());
                    }
                }
            }
            cleaned.push(msg.clone());
            continue;
        }

        if role == "tool" {
            let tool_call_id = msg.get("tool_call_id").and_then(|v| v.as_str());
            if let Some(id) = tool_call_id {
                if expected_tool_ids.contains(id) {
                    cleaned.push(msg.clone());
                }
            }
            continue;
        }

        // user 消息不清空 expected_tool_ids：图片处理会在 tool 消息间
        // 插入 user 消息，清空会导致后续同一轮的 tool result 被丢弃。
        // 只有 system 等真正的轮次分隔消息才需要清空。
        if role != "user" {
            expected_tool_ids.clear();
        }
        cleaned.push(msg.clone());
    }

    cleaned
}
