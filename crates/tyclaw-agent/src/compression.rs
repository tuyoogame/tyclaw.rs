//! 工具结果与参数的历史压缩。
//!
//! 从 `agent_loop.rs` 拆分出的压缩相关函数，负责对历史消息中的
//! 工具调用结果和参数进行衰减压缩，降低 prompt token 消耗。

use serde_json::{json, Value};
use std::collections::HashMap;

use crate::loop_helpers::{
    is_error_envelope, EXEC_INTENT_MEDIUM_MAX_CHARS, EXEC_INTENT_OLD_MAX_CHARS,
    EXPLORE_ABSOLUTE_CAP, TOOL_CALL_ARGS_FRESH_COUNT, TOOL_CALL_ARGS_MEDIUM_CHARS,
    TOOL_CALL_ARGS_OLD_COUNT, TOOL_RESULT_FRESH_COUNT, TOOL_RESULT_MEDIUM_CHARS,
    TOOL_RESULT_OLD_COUNT,
};

/// 对历史消息中的工具结果进行衰减压缩，越老的工具输出越精简。
///
/// **重要**：仅在产出阶段启用衰减。探索阶段保留完整上下文，
/// 避免 LLM 因"失忆"而重复探索相同数据。
///
/// 策略（从消息末尾往前数 tool 类型消息）：
/// - 最近 TOOL_RESULT_FRESH_COUNT 条：保留完整内容
/// - FRESH_COUNT ~ OLD_COUNT 条：截断到 MEDIUM_CHARS 字符
/// - 超过 OLD_COUNT 条：只保留摘要行
///
pub(crate) fn compress_tool_results(
    messages: &[HashMap<String, Value>],
    skip_compression: bool,
    light_keep_recent_rounds: usize,
    protected_prefix_len: usize,
) -> Vec<HashMap<String, Value>> {
    // 跳过压缩时直接返回引用的 clone（不做任何处理）
    if skip_compression {
        return Vec::from(messages);
    }
    // 按 assistant(tool_calls) 估算轮次边界，最近 N 轮尽量不压缩。
    let assistant_round_indices_forward: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.get("role").and_then(|v| v.as_str()) == Some("assistant")
                && m.get("tool_calls")
                    .and_then(|v| v.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false)
        })
        .map(|(i, _)| i)
        .collect();
    let recent_round_cutoff_idx =
        if assistant_round_indices_forward.len() > light_keep_recent_rounds {
            assistant_round_indices_forward
                [assistant_round_indices_forward.len() - light_keep_recent_rounds]
        } else {
            0
        };

    // 先收集所有 tool 消息的索引（从后往前）
    let tool_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, m)| m.get("role").and_then(|v| v.as_str()) == Some("tool"))
        .map(|(i, _)| i)
        .collect();

    let mut result = messages.to_vec();

    // === Part 1: 压缩旧的 tool result 内容 ===
    // 先标记需要保护的 tool result（需求文档类内容，永不压缩）
    // 规则：前 EXPLORE_ABSOLUTE_CAP 条 tool 中，read_file 的结果视为需求/规格文档
    let protected_indices: std::collections::HashSet<usize> = tool_indices
        .iter()
        .rev() // tool_indices 是从后往前的，反转回正序
        .take(EXPLORE_ABSOLUTE_CAP)
        .filter(|&&idx| {
            let msg = &messages[idx];
            let tool_name = msg.get("name").and_then(|v| v.as_str()).unwrap_or("");
            tool_name == "read_file"
        })
        .cloned()
        .collect();

    for (age, &idx) in tool_indices.iter().enumerate() {
        if idx < protected_prefix_len {
            continue;
        }
        if idx >= recent_round_cutoff_idx {
            continue;
        }
        if age < TOOL_RESULT_FRESH_COUNT {
            // 最近的：保留完整
            continue;
        }
        // 需求文档类 tool result 永不压缩
        if protected_indices.contains(&idx) {
            continue;
        }

        let msg = &messages[idx];
        let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let tool_name = msg
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        if age < TOOL_RESULT_OLD_COUNT {
            // 中等新鲜度：截断到 MEDIUM_CHARS
            if content.len() > TOOL_RESULT_MEDIUM_CHARS {
                let boundary = content.floor_char_boundary(TOOL_RESULT_MEDIUM_CHARS);
                let compressed = format!(
                    "{}\n\n... [Compressed: showing {TOOL_RESULT_MEDIUM_CHARS}/{} chars]",
                    &content[..boundary],
                    content.len()
                );
                result[idx].insert("content".into(), json!(compressed));
            }
        } else {
            // 很旧的：只保留摘要
            let status = if content.starts_with("[DENIED]") {
                "denied"
            } else if is_error_envelope(content) {
                "error"
            } else {
                "ok"
            };
            let summary = format!(
                "[tool: {tool_name}, {len} chars, {status}]",
                len = content.len(),
            );
            result[idx].insert("content".into(), json!(summary));
        }
    }

    // === Part 2: 压缩旧的 assistant tool_call arguments ===
    // 三层衰减策略（与 tool result 对称）：
    //   - 最近 FRESH_COUNT 条：保留完整 arguments
    //   - FRESH ~ OLD_COUNT 条：截断到 MEDIUM_CHARS 字符（保留足够上下文）
    //   - 超过 OLD_COUNT 条：替换为结构化摘要
    let assistant_tc_indices: Vec<usize> = result
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, m)| {
            m.get("role").and_then(|v| v.as_str()) == Some("assistant")
                && m.get("tool_calls")
                    .and_then(|v| v.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false)
        })
        .map(|(i, _)| i)
        .collect();

    for (age, &idx) in assistant_tc_indices.iter().enumerate() {
        if idx < protected_prefix_len {
            continue;
        }
        if idx >= recent_round_cutoff_idx {
            continue;
        }
        if age < TOOL_CALL_ARGS_FRESH_COUNT {
            continue;
        }

        if let Some(tool_calls) = result[idx].get("tool_calls").cloned() {
            if let Some(tcs) = tool_calls.as_array() {
                let compressed_tcs: Vec<Value> = tcs
                    .iter()
                    .map(|tc| {
                        let mut tc = tc.clone();
                        if let Some(func) = tc.get_mut("function") {
                            let tool_name = func
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            if let Some(args_val) = func.get("arguments") {
                                let args_str = args_val.as_str().unwrap_or("");
                                let new_args = if age < TOOL_CALL_ARGS_OLD_COUNT {
                                    // 中等新鲜度：截断到 MEDIUM_CHARS
                                    compress_args_medium(tool_name, args_str)
                                } else {
                                    // 很旧：只保留摘要
                                    compress_args_summary(tool_name, args_str)
                                };
                                if let Some(compressed) = new_args {
                                    if let Some(obj) = func.as_object_mut() {
                                        obj.insert("arguments".into(), json!(compressed));
                                    }
                                }
                            }
                        }
                        tc
                    })
                    .collect();
                result[idx].insert("tool_calls".into(), Value::Array(compressed_tcs));
            }
        }
    }

    result
}

/// 中等新鲜度的 arguments 压缩：截断但保留足够上下文。
/// 对 exec 命令特殊处理：提取代码中的注释和 print 语句作为意图摘要。
pub(crate) fn compress_args_medium(tool_name: &str, args_str: &str) -> Option<String> {
    if args_str.len() <= TOOL_CALL_ARGS_MEDIUM_CHARS {
        return None; // 不需要压缩
    }

    if tool_name == "exec" {
        // 对 exec 命令：提取内联代码的意图摘要，截断到 medium 限制
        if let Some(summary) = extract_exec_intent(args_str, EXEC_INTENT_MEDIUM_MAX_CHARS) {
            return Some(summary);
        }
    }

    // 通用截断：尝试解析为 JSON 后用 serde_json 重新序列化安全摘要
    // 避免截断到 JSON 中间导致不合法
    if let Ok(mut parsed) = serde_json::from_str::<Value>(args_str) {
        // 截断所有超长的字符串字段值
        truncate_json_values(&mut parsed, TOOL_CALL_ARGS_MEDIUM_CHARS);
        return Some(parsed.to_string());
    }
    // JSON 解析失败时，生成安全的摘要
    let tool_summary = format!("[args: {} chars]", args_str.len());
    Some(json!({"_summary": tool_summary}).to_string())
}

/// 很旧的 arguments 压缩：只保留结构化摘要。
pub(crate) fn compress_args_summary(tool_name: &str, args_str: &str) -> Option<String> {
    if args_str.len() <= 200 {
        return None; // 很短的不压缩
    }

    match tool_name {
        "exec" => {
            // 提取 exec 的意图摘要，old 层用更短的限制
            if let Some(summary) = extract_exec_intent(args_str, EXEC_INTENT_OLD_MAX_CHARS) {
                return Some(summary);
            }
            // 使用 serde_json 生成合法 JSON
            Some(json!({"command": format!("[exec: {} chars]", args_str.len())}).to_string())
        }
        "write_file" => {
            // 提取文件路径
            let path = extract_json_field(args_str, "path").unwrap_or("?".into());
            Some(
                json!({"path": path, "content": format!("[written: {} chars]", args_str.len())})
                    .to_string(),
            )
        }
        _ => Some(
            json!({"_summary": format!("[{tool_name}: {} chars]", args_str.len())}).to_string(),
        ),
    }
}

/// 从 exec 的 arguments 中提取代码意图摘要。
///
/// 保留：注释行(#)、print() 语句、import 语句、关键赋值。
/// 这些足以让 LLM 理解"之前做了什么"，而无需看到完整代码。
/// `max_intent_chars` 控制 intent 部分的最大字符数，确保不同衰减层有不同精简度。
pub(crate) fn extract_exec_intent(args_str: &str, max_intent_chars: usize) -> Option<String> {
    // 提取 command 字段内容
    let cmd = extract_json_field(args_str, "command")?;

    // 提取有意义的行（注释、print、import）
    let mut intent_lines: Vec<&str> = Vec::new();
    let mut prefix = "";

    for line in cmd.lines() {
        let trimmed = line.trim();
        // 保留 cd 前缀
        if trimmed.starts_with("cd ") && trimmed.contains("&&") {
            prefix = trimmed;
            continue;
        }
        // 保留注释、print、import
        if trimmed.starts_with('#')
            || trimmed.starts_with("print(")
            || trimmed.starts_with("print (")
            || trimmed.starts_with("import ")
            || trimmed.starts_with("from ")
        {
            intent_lines.push(trimmed);
        }
    }

    if intent_lines.is_empty() {
        // 没有注释/print，退回到前 N 字符截断
        return None;
    }

    // 拼接 intent 并截断到 max_intent_chars
    let mut intent = intent_lines.join(" | ");
    if intent.chars().count() > max_intent_chars {
        let boundary = intent
            .char_indices()
            .nth(max_intent_chars)
            .map(|(i, _)| i)
            .unwrap_or(intent.len());
        intent.truncate(boundary);
        intent.push_str("...");
    }

    let summary_text = if !prefix.is_empty() {
        format!(
            "[{prefix} | python script ({} chars) intent: {intent}]",
            cmd.len()
        )
    } else {
        format!("[python script ({} chars) intent: {intent}]", cmd.len())
    };

    // 使用 serde_json 确保生成合法 JSON（自动转义引号、反斜杠等）
    Some(json!({"command": summary_text}).to_string())
}

/// 递归截断 JSON 值中超长的字符串字段。
pub(crate) fn truncate_json_values(value: &mut Value, max_chars: usize) {
    match value {
        Value::String(s) => {
            if s.len() > max_chars {
                let boundary = s.floor_char_boundary(max_chars);
                let original_len = s.len();
                s.truncate(boundary);
                s.push_str(&format!("... [truncated: {max_chars}/{original_len}]"));
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                truncate_json_values(v, max_chars);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                truncate_json_values(v, max_chars);
            }
        }
        _ => {}
    }
}

/// 从 JSON 字符串中提取指定字段的值（简单解析，不依赖完整 JSON parse）。
pub(crate) fn extract_json_field(json_str: &str, field: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", field);
    let start = json_str.find(&pattern)?;
    let value_start = start + pattern.len();
    // 找到未转义的结束引号：需要计算引号前连续反斜杠的数量，
    // 偶数个反斜杠表示引号未被转义（反斜杠自身被转义），奇数个表示引号被转义。
    let mut i = value_start;
    let bytes = json_str.as_bytes();
    loop {
        if i >= bytes.len() {
            return None;
        }
        if bytes[i] == b'"' {
            // 计算引号前连续反斜杠的数量
            let mut num_backslashes = 0;
            let mut j = i;
            while j > value_start && bytes[j - 1] == b'\\' {
                num_backslashes += 1;
                j -= 1;
            }
            // 偶数个反斜杠 → 引号未被转义，是真正的结束引号
            if num_backslashes % 2 == 0 {
                break;
            }
        }
        i += 1;
    }
    let raw = &json_str[value_start..i];
    // 基本反转义
    let unescaped = raw
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\\"", "\"")
        .replace("\\\\", "\\");
    Some(unescaped)
}
