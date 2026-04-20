//! ReAct 循环辅助函数与常量。
//!
//! 从 `agent_loop.rs` 拆分出的独立函数和常量定义，
//! 包括文本处理、阶段推断、历史压缩等工具函数。

use regex::Regex;
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use tyclaw_prompt::strip_non_task_user_message;
use tyclaw_provider::ToolCallRequest;

lazy_static::lazy_static! {
    /// 匹配 <think>...</think> 标签的正则表达式。
    static ref THINK_RE: Regex = Regex::new(r"(?s)<think>[\s\S]*?</think>").unwrap();
}

/// 清除文本中的 <think>...</think> 标签。
///
/// 某些模型（如 DeepSeek）会在回复中包含思考过程，需要在展示前清除。
/// 如果清除后内容为空，返回 None。
pub(crate) fn strip_think(text: Option<&str>) -> Option<String> {
    let t = text?;
    let cleaned = THINK_RE.replace_all(t, "").trim().to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

pub(crate) fn strip_runtime_context_tag(input: &str) -> String {
    strip_non_task_user_message(input).unwrap_or_default()
}

pub(crate) fn latest_user_goal(messages: &[HashMap<String, Value>]) -> String {
    for msg in messages.iter().rev() {
        if msg.get("role").and_then(|v| v.as_str()) == Some("user") {
            if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                let cleaned = strip_runtime_context_tag(content);
                if !cleaned.trim().is_empty() {
                    return cleaned;
                }
            }
        }
    }
    "Complete the current user request with minimal redundant tool calls.".to_string()
}

/// 全量归一化（合并空白），用于 exec 重复检测，**不截断**，避免仅头部相同的脚本被误判。
pub(crate) fn normalize_exec_for_repeat(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 对整段命令计算稳定指纹（基于 `normalize_exec_for_repeat` 的完整字符串）。
pub(crate) fn exec_command_fingerprint(cmd: &str) -> u64 {
    let s = normalize_exec_for_repeat(cmd);
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// 从 exec 命令中提取内联 Python 脚本到临时文件。
///
/// 识别 `python3 -c "..."` 或 `python3 -c '...'` 模式，将脚本内容写入临时文件，
/// 返回替换后的命令。如果不匹配模式则返回 None。
/// 将超长的 `python3 -c "..."` 命令转换为 heredoc 格式。
///
/// 不写临时文件（host 的 /tmp 在容器内不可见），
/// 改用 `python3 << 'PYEOF'` heredoc，host 和 docker exec 都能正常工作。
pub(crate) fn extract_inline_script(cmd: &str) -> Option<String> {
    lazy_static::lazy_static! {
        static ref INLINE_PY_RE: Regex = Regex::new(
            r#"(?s)^((?:cd\s+\S+\s*&&\s*)?)(?:python3?)\s+-c\s+(?:"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)')\s*$"#
        ).unwrap();
    }

    let caps = INLINE_PY_RE.captures(cmd.trim())?;
    let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let script = caps.get(2).or_else(|| caps.get(3)).map(|m| m.as_str())?;

    let script = script
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\\"", "\"")
        .replace("\\'", "'")
        .replace("\\\\", "\\");

    // 用 heredoc 传递脚本，无需写临时文件
    let new_cmd = if prefix.is_empty() {
        format!("python3 << 'PYEOF'\n{script}\nPYEOF")
    } else {
        format!("{prefix}python3 << 'PYEOF'\n{script}\nPYEOF")
    };

    Some(new_cmd)
}

// ─── Constants ───────────────────────────────────────────────────────────────

/// 连续重复同一批工具调用的最大允许次数。
/// 超过后直接终止，避免模型"原地踏步"。
/// 设为 3：容忍偶尔的重试（如网络抖动、文件锁竞争），但阻止无限循环。
/// 值过小（1-2）会误杀合理重试；过大（5+）则浪费 token 且延迟用户感知。
pub(crate) const MAX_REPEAT_TOOL_BATCH: usize = 3;
/// 连续无有效进展轮次上限（仅 DENIED/ERROR 视为无进展）。
/// 设为 2：允许模型在首次失败后做一次调整尝试，第二次仍失败则终止。
/// 过于激进（1）会阻止模型自我修正；过于宽松（3+）会在权限问题上空转。
pub(crate) const MAX_NO_PROGRESS_ROUNDS: usize = 2;
/// 工具输出最大字符数（产出阶段）。超过此长度的输出会被截断，避免 context 膨胀。
/// 中间档：兼顾信息量和噪音控制。
pub(crate) const MAX_TOOL_OUTPUT_CHARS: usize = 4000;
/// 探索阶段的工具输出最大字符数。
/// 设置较低以阻止 LLM 做"逐行翻页"式探索（row-by-row paging），
/// 迫使它只能用 head(3)/shape/dtypes 写紧凑查询。
pub(crate) const MAX_TOOL_OUTPUT_CHARS_EXPLORE: usize = 3500;
/// `read_file` 在 ReAct 循环内的软上限（工具层另有上限）。
pub(crate) const READ_FILE_TOOL_MAX_CHARS: usize = 96 * 1024;
/// 探索阶段最大轮次占总轮次的比例（百分比）。超过后强制催促。
/// 中间档探索预算，避免过早打断和过度探索两端问题。
pub(crate) const EXPLORE_MAX_RATIO_PERCENT: usize = 30;
/// 探索阶段绝对上限轮次。无论 max_iterations 多大，探索不超过此值。
/// 适度放宽探索硬上限，避免复杂输入在早期被过快打断。
/// 设为 30：经验值，覆盖大多数复杂代码库的探索需求（目录结构、依赖关系、
/// 关键文件阅读），同时防止无节制浏览导致 context 溢出。
pub(crate) const EXPLORE_ABSOLUTE_CAP: usize = 30;

/// 工具结果衰减阈值：最近 N 条工具消息保留完整内容。
pub(crate) const TOOL_RESULT_FRESH_COUNT: usize = 8;
/// 工具结果衰减：中等新鲜度的消息截断到此字符数。
pub(crate) const TOOL_RESULT_MEDIUM_CHARS: usize = 700;
/// 工具结果衰减：超过此数量的旧工具消息只保留摘要。
pub(crate) const TOOL_RESULT_OLD_COUNT: usize = 20;
/// 最近 N 轮尽量轻压缩，保留短期上下文。
pub(crate) const LIGHT_COMPRESS_RECENT_ROUNDS: usize = 6;

/// tool_call arguments 衰减：最近 N 条 assistant 消息保留完整 arguments。
pub(crate) const TOOL_CALL_ARGS_FRESH_COUNT: usize = 4;
/// tool_call arguments 衰减：超过此数量的 assistant 消息只保留摘要。
pub(crate) const TOOL_CALL_ARGS_OLD_COUNT: usize = 10;
/// tool_call arguments 衰减：中等新鲜度截断到此字符数（保留最小必要上下文）。
pub(crate) const TOOL_CALL_ARGS_MEDIUM_CHARS: usize = 520;
/// exec intent 摘要：medium 层最大字符数。
pub(crate) const EXEC_INTENT_MEDIUM_MAX_CHARS: usize = 800;
/// exec intent 摘要：old 层最大字符数（更激进）。
pub(crate) const EXEC_INTENT_OLD_MAX_CHARS: usize = 300;

/// exec 内联代码提取阈值（字符数）。超过此长度的内联脚本将被自动提取到临时文件执行。
pub(crate) const EXEC_INLINE_EXTRACT_THRESHOLD: usize = 2000;

/// 规划阶段：前 N 轮要求 LLM 输出计划文本。
/// 如果 LLM 在前 PLAN_CHECK_ITERATIONS 轮只出 tool_calls 没有 content，注入催促。
/// 设为 0 表示禁用强制计划检查（提示词已包含"按需规划"指导，简单任务不需要计划）。
pub(crate) const PLAN_CHECK_ITERATIONS: usize = 0;
/// 温和 exec 防抖：产出阶段若同一大命令重复出现达到阈值，则拦截一次并提示改为一次性脚本。
pub(crate) const REPEAT_EXEC_BLOCK_THRESHOLD: usize = 2;
pub(crate) const REPEAT_EXEC_MIN_CMD_LEN: usize = 120;
pub(crate) const EXEC_HISTORY_WINDOW: usize = 12;

/// 产出工具名称列表。当这些工具首次出现时，标记探索阶段结束。
/// dispatch_subtasks 也是产出工具：多模型模式下主控通过它完成文件写入，
/// 不把它算作产出会导致主控永远停在 explore 阶段。
///
/// NOTE: 此列表必须与 ToolRegistry 中注册的工具名称保持同步。
/// 新增产出型工具时需同步更新此处，否则阶段转换逻辑会失效。
pub(crate) const PRODUCTION_TOOLS: &[&str] =
    &["write_file", "edit_file", "send_file", "dispatch_subtasks"];
/// 轮次重置标记：仅当上一轮因 reach max 结束时写入历史。
pub(crate) const MAX_REACHED_RESET_MARKER: &str = "[[TYCLAW_MAX_REACHED_RESET_NEXT_RUN]]";
/// 由上层注入的"本进程首次进入该会话时重置轮次"标记字段。
pub(crate) const RESET_ON_START_FIELD: &str = "_reset_iterations_next_run";
/// 前 N 轮保留完整 system prompt，后续切换到精简版。
/// 前 N 轮保留完整 system prompt，之后精简。
/// 设为 0 表示从第一轮就使用精简版（含 CACHE_BOUNDARY），
/// 确保 system message 的 block 结构从首轮起保持一致，不破坏 prompt cache。
pub(crate) const EXTENDED_SYSTEM_ROUNDS: usize = 0;
/// 防止重复压缩 system prompt 的内部标记。
pub(crate) const COMPACT_SYSTEM_MARKER: &str = "[[TYCLAW_COMPACT_SYSTEM]]";
/// 最近 N 条 assistant 长文本做"轻压缩"（截断到 COMPACT_THRESHOLD），保留更多推理上下文。
pub(crate) const ASSISTANT_TEXT_LIGHT_KEEP: usize = 5;
/// assistant 普通文本超过该字符数时，进入历史压缩。
/// 最近 LIGHT_KEEP 条截断到此长度（轻压缩），更老的做前后缀压缩（重压缩）。
pub(crate) const ASSISTANT_TEXT_COMPACT_THRESHOLD: usize = 700;
/// 重压缩：保留的前后缀字符数。
pub(crate) const ASSISTANT_TEXT_KEEP_PREFIX: usize = 180;
pub(crate) const ASSISTANT_TEXT_KEEP_SUFFIX: usize = 100;
pub(crate) const COMPACT_ASSISTANT_MARKER: &str = "[[TYCLAW_COMPACT_ASSISTANT]]";
// STATE_VIEW 暂时禁用（破坏 prompt cache 前缀匹配），常量保留备用。
// pub(crate) const STATE_SNAPSHOT_CHARS: usize = 3000;
// pub(crate) const FIRST_TURN_STATE_SNAPSHOT_CHARS: usize = 3800;

// ─── Functions ───────────────────────────────────────────────────────────────

/// 判断工具是否为"产出型"工具（标志探索阶段结束）。
pub(crate) fn is_production_tool(name: &str) -> bool {
    PRODUCTION_TOOLS.contains(&name)
}

/// 从已有消息历史推断阶段状态。
///
/// 返回 (in_exploration, exploration_iters, production_iters, total_iters)
pub(crate) fn infer_phase_from_messages(
    messages: &[HashMap<String, Value>],
) -> (bool, usize, usize, usize) {
    let mut in_exploration = true;
    let mut exploration_iters: usize = 0;
    let mut production_iters: usize = 0;

    for msg in messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role != "assistant" {
            continue;
        }
        let tool_calls = match msg.get("tool_calls").and_then(|v| v.as_array()) {
            Some(tcs) if !tcs.is_empty() => tcs,
            _ => continue,
        };

        if in_exploration {
            exploration_iters += 1;
        } else {
            production_iters += 1;
        }

        if in_exploration {
            for tc in tool_calls {
                let name = tc
                    .pointer("/function/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if is_production_tool(name) {
                    in_exploration = false;
                    exploration_iters = exploration_iters.saturating_sub(1);
                    production_iters += 1;
                    break;
                }
            }
        }
    }

    let total = exploration_iters + production_iters;
    (in_exploration, exploration_iters, production_iters, total)
}

/// 检测并移除"下一轮重置计数器"标记（一次性消费）。
pub(crate) fn take_reset_marker(messages: &mut Vec<HashMap<String, Value>>) -> bool {
    let mut found = false;
    let mut retained: Vec<HashMap<String, Value>> = Vec::with_capacity(messages.len());
    for mut msg in std::mem::take(messages) {
        // 上层注入的结构化重置标记：消费并丢弃该条消息。
        if msg
            .get(RESET_ON_START_FIELD)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            found = true;
            continue;
        }

        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role == "assistant" {
            if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                if content.contains(MAX_REACHED_RESET_MARKER) {
                    found = true;
                    let cleaned = content
                        .replace(MAX_REACHED_RESET_MARKER, "")
                        .trim()
                        .to_string();
                    if cleaned.is_empty() && !msg.contains_key("tool_calls") {
                        continue;
                    }
                    msg.insert("content".into(), Value::String(cleaned));
                }
            }
        }
        retained.push(msg);
    }
    *messages = retained;
    found
}

pub(crate) fn compact_system_prompt(raw: &str) -> String {
    const CACHE_BOUNDARY: &str = "[[CACHE_BOUNDARY]]";

    // 如果有 CACHE_BOUNDARY，只精简 boundary 之后的动态部分，保持 boundary 之前的静态前缀不变。
    // 这样 prompt caching 的缓存前缀不会因为精简而失效。
    if let Some(boundary_pos) = raw.find(CACHE_BOUNDARY) {
        let static_prefix = &raw[..boundary_pos + CACHE_BOUNDARY.len()];
        let dynamic_part = &raw[boundary_pos + CACHE_BOUNDARY.len()..];

        // 动态部分：按 section 分隔，去掉高冗余段
        let mut kept: Vec<String> = Vec::new();
        for sec in dynamic_part.split("\n\n---\n\n") {
            let s = sec.trim();
            if s.is_empty() {
                continue;
            }
            // 去掉重复的能力/技能列表（这些信息在首轮已经看过）
            if s.starts_with("# Available Capabilities") || s.starts_with("# Available Skills") {
                continue;
            }
            kept.push(s.to_string());
        }
        kept.push(format!(
            "# Runtime Policy\n\
             - 先规划后执行，避免过度探索。\n\
             - 工具调用失败先分析原因，不要盲目重试。\n\
             - 任务完成时直接输出最终回复（不调用任何工具即表示结束）。\n\
             {COMPACT_SYSTEM_MARKER}"
        ));
        format!("{}\n\n---\n\n{}", static_prefix, kept.join("\n\n---\n\n"))
    } else {
        // 无 boundary：精简冗余段落，但保留 skills/capabilities（LLM 需要知道可用能力）
        let mut kept: Vec<String> = Vec::new();
        for sec in raw.split("\n\n---\n\n") {
            let s = sec.trim();
            if s.is_empty() {
                continue;
            }
            if s.starts_with("## AGENTS") || s.starts_with("## GUIDELINES") {
                continue;
            }
            kept.push(s.to_string());
        }
        kept.push(format!(
            "# Runtime Policy\n\
             - 先规划后执行，避免过度探索。\n\
             - 工具调用失败先分析原因，不要盲目重试。\n\
             - 任务完成时直接输出最终回复（不调用任何工具即表示结束）。\n\
             {COMPACT_SYSTEM_MARKER}"
        ));
        kept.join("\n\n---\n\n")
    }
}

pub(crate) fn maybe_compact_system_message(
    messages: &mut [HashMap<String, Value>],
    total_iterations: usize,
) {
    if total_iterations <= EXTENDED_SYSTEM_ROUNDS {
        return;
    }
    for msg in messages.iter_mut() {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role != "system" {
            continue;
        }
        let Some(content) = msg.get("content").and_then(|v| v.as_str()) else {
            continue;
        };
        if content.contains(COMPACT_SYSTEM_MARKER) {
            break;
        }
        let compacted = compact_system_prompt(content);
        msg.insert("content".into(), Value::String(compacted));
        break;
    }
}

pub(crate) fn truncate_by_chars_local(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    match input.char_indices().nth(max_chars) {
        Some((idx, _)) => input[..idx].to_string(),
        None => input.to_string(),
    }
}

/// 重压缩：保留前后缀，中间丢弃。
pub(crate) fn compact_assistant_heavy(content: &str) -> String {
    let prefix = truncate_by_chars_local(content, ASSISTANT_TEXT_KEEP_PREFIX);
    let total_chars = content.chars().count();
    let suffix_start = total_chars.saturating_sub(ASSISTANT_TEXT_KEEP_SUFFIX);
    let suffix: String = content.chars().skip(suffix_start).collect();
    format!(
        "{COMPACT_ASSISTANT_MARKER}\n{prefix}\n\n... [compacted, omitted {} chars] ...\n\n{suffix}",
        total_chars.saturating_sub(ASSISTANT_TEXT_KEEP_PREFIX + ASSISTANT_TEXT_KEEP_SUFFIX)
    )
}

/// 轻压缩：截断到 COMPACT_THRESHOLD 字符，保留更多推理上下文。
pub(crate) fn compact_assistant_light(content: &str) -> String {
    let total_chars = content.chars().count();
    if total_chars <= ASSISTANT_TEXT_COMPACT_THRESHOLD {
        return content.to_string();
    }
    let truncated = truncate_by_chars_local(content, ASSISTANT_TEXT_COMPACT_THRESHOLD);
    format!(
        "{COMPACT_ASSISTANT_MARKER}\n{truncated}\n... [light-compacted, omitted {} chars]",
        total_chars - ASSISTANT_TEXT_COMPACT_THRESHOLD
    )
}

/// 历史压缩：两层衰减策略。
/// - 最近 LIGHT_KEEP(5) 条长文本：轻压缩（截断到 700 chars），保留推理结论
/// - 更老的：重压缩（前 180 + 后 100 chars）
/// - 带 tool_calls 的 assistant 消息不动
pub(crate) fn maybe_compact_assistant_history(messages: &mut [HashMap<String, Value>]) {
    let idxs: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
            if role != "assistant" || m.get("tool_calls").is_some() {
                return None;
            }
            let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.is_empty() || content.contains(COMPACT_ASSISTANT_MARKER) {
                return None;
            }
            if content.chars().count() > ASSISTANT_TEXT_COMPACT_THRESHOLD {
                Some(i)
            } else {
                None
            }
        })
        .collect();

    if idxs.is_empty() {
        return;
    }

    // 从后往前：最近 LIGHT_KEEP 条做轻压缩，更老的做重压缩。
    let light_start = idxs.len().saturating_sub(ASSISTANT_TEXT_LIGHT_KEEP);
    for (pos, &i) in idxs.iter().enumerate() {
        if let Some(content) = messages[i].get("content").and_then(|v| v.as_str()) {
            let compacted = if pos < light_start {
                compact_assistant_heavy(content)
            } else {
                compact_assistant_light(content)
            };
            messages[i].insert("content".into(), Value::String(compacted));
        }
    }
}

/// 去重连续重复的 system 催促，减少无效 token 消耗。
pub(crate) fn dedupe_consecutive_system_messages(messages: &mut Vec<HashMap<String, Value>>) {
    let mut deduped: Vec<HashMap<String, Value>> = Vec::with_capacity(messages.len());
    let mut last_system_content: Option<String> = None;
    for msg in std::mem::take(messages) {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role == "system" {
            let content = msg
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if last_system_content.as_deref() == Some(content.as_str()) {
                continue;
            }
            last_system_content = Some(content);
            deduped.push(msg);
        } else {
            last_system_content = None;
            deduped.push(msg);
        }
    }
    *messages = deduped;
}

pub(crate) fn stable_args_signature(args: &HashMap<String, Value>) -> String {
    let mut keys: Vec<&String> = args.keys().collect();
    keys.sort();
    let mut parts = Vec::new();
    for k in keys {
        let v = args
            .get(k)
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".into()))
            .unwrap_or_else(|| "null".into());
        parts.push(format!("{k}={v}"));
    }
    parts.join("&")
}

pub(crate) fn tool_batch_signature(tool_calls: &[ToolCallRequest]) -> String {
    tool_calls
        .iter()
        .map(|tc| format!("{}({})", tc.name, stable_args_signature(&tc.arguments)))
        .collect::<Vec<_>>()
        .join("||")
}

/// 判断工具结果是否为结构化错误 envelope（来自 ToolRegistry）。
pub(crate) fn is_error_envelope(result: &str) -> bool {
    serde_json::from_str::<Value>(result)
        .ok()
        .and_then(|v| {
            Some(
                v.get("status")
                    .and_then(|s| s.as_str())
                    .map(|s| s == "error")
                    .unwrap_or(false),
            )
        })
        .unwrap_or(false)
}
