//! 编排器辅助函数：技能路由、案例优化、上下文预算计算。

use std::collections::{HashMap, HashSet};
use serde_json::Value;

use crate::history::normalize_text_for_dedupe;
use crate::types::{
    ContextBudgetPlan, HANDOFF_MAX_CONTENT_CHARS, HANDOFF_MAX_MESSAGES, HISTORY_BUDGET_RATIO,
    MAX_DYNAMIC_INJECTED_SKILLS, MAX_DYNAMIC_SIMILAR_CASES_CHARS, MAX_HISTORY_BUDGET_RATIO,
    MAX_INJECTED_SKILLS, MAX_SIMILAR_CASES_CHARS,
};
use tyclaw_prompt::strip_non_task_user_message;

/// 压缩 similar cases 段：
/// 1) 行级去重；2) 长度截断。
/// 目标是在保留案例信号的前提下避免"案例段吞噬主问题 token"。
pub(crate) fn optimize_similar_cases(cases: &str, max_chars: usize) -> String {
    if cases.is_empty() {
        return String::new();
    }
    let mut seen = HashSet::new();
    let mut lines: Vec<String> = Vec::new();
    for line in cases.lines() {
        let normalized = normalize_text_for_dedupe(line);
        if normalized.is_empty() {
            lines.push(String::new());
            continue;
        }
        if seen.insert(normalized) {
            lines.push(line.to_string());
        }
    }
    let mut merged = lines.join("\n");
    if merged.chars().count() > max_chars {
        merged = truncate_by_chars(&merged, max_chars);
        merged.push_str("\n... (similar cases truncated)");
    }
    merged
}

pub(crate) fn truncate_by_chars(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    match input.char_indices().nth(max_chars) {
        Some((idx, _)) => input[..idx].to_string(),
        None => input.to_string(),
    }
}

pub(crate) fn build_handoff_markdown(
    session_key: &str,
    messages: &[std::collections::HashMap<String, serde_json::Value>],
) -> String {
    let mut out = String::new();
    let now = chrono::Utc::now().to_rfc3339();
    let start = messages.len().saturating_sub(HANDOFF_MAX_MESSAGES);
    let selected = &messages[start..];

    out.push_str("# TyClaw Handoff\n\n");
    out.push_str(&format!("- 时间: {now}\n"));
    out.push_str(&format!("- 会话: `{session_key}`\n"));
    out.push_str(&format!("- 总消息数: {}\n", messages.len()));
    out.push_str(&format!("- 导出消息数: {}\n\n", selected.len()));

    for (i, msg) in selected.iter().enumerate() {
        let role = msg
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let mut content = msg
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if role == "user" {
            if let Some(cleaned) = strip_non_task_user_message(&content) {
                content = cleaned;
            } else {
                continue;
            }
        }
        content = truncate_by_chars(&content, HANDOFF_MAX_CONTENT_CHARS);

        out.push_str(&format!("## {}. role={role}\n", i + 1));
        if let Some(tool_call_id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
            out.push_str(&format!("- tool_call_id: `{tool_call_id}`\n"));
        }
        if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            out.push_str(&format!("- tool_calls: {}\n", tool_calls.len()));
        }
        out.push_str("```\n");
        out.push_str(&content);
        out.push_str("\n```\n\n");
    }
    out
}

/// query 驱动的动态预算分配器。
///
/// 这是启发式策略，不依赖模型分类器，优点是稳定和易调参：
/// - 排障类：提升 cases 预算
/// - 连续追问类：提升 history 预算
/// - 实现/编码类：提升 skills 预算
///
/// 最终会做 clamp，确保预算落在可控范围内。
pub(crate) fn compute_context_budget_plan(query: &str) -> ContextBudgetPlan {
    let q = query.to_lowercase();
    let has_any = |words: &[&str]| words.iter().any(|w| q.contains(w));

    // 默认：历史 45%，技能 8，案例 2500 chars。
    let mut plan = ContextBudgetPlan {
        history_ratio: HISTORY_BUDGET_RATIO,
        max_skills: MAX_INJECTED_SKILLS,
        max_cases_chars: MAX_SIMILAR_CASES_CHARS,
    };

    // 调试排障：给 cases 更高预算，history 适中。
    if has_any(&[
        "报错",
        "错误",
        "异常",
        "失败",
        "traceback",
        "error",
        "timeout",
        "排查",
        "日志",
    ]) {
        plan.history_ratio = 40;
        plan.max_skills = 7;
        plan.max_cases_chars = 3600;
    }

    // 连续对话：提高 history 预算，降低 cases。
    if has_any(&[
        "继续",
        "刚才",
        "上次",
        "前面",
        "这个问题",
        "同一个",
        "再补充",
        "延续",
    ]) {
        plan.history_ratio = 60;
        plan.max_skills = 6;
        plan.max_cases_chars = 1800;
    }

    // 实现/编码任务：技能预算提高，history 维持中等。
    if has_any(&[
        "实现",
        "改代码",
        "重构",
        "写一个",
        "patch",
        "fix",
        "refactor",
        "代码",
    ]) {
        plan.history_ratio = 45;
        plan.max_skills = 10;
        plan.max_cases_chars = 1500;
    }

    // clamp，防止越界。
    plan.history_ratio = plan
        .history_ratio
        .clamp(HISTORY_BUDGET_RATIO, MAX_HISTORY_BUDGET_RATIO);
    plan.max_skills = plan.max_skills.clamp(3, MAX_DYNAMIC_INJECTED_SKILLS);
    plan.max_cases_chars = plan
        .max_cases_chars
        .clamp(800, MAX_DYNAMIC_SIMILAR_CASES_CHARS);
    plan
}

/// 从 agent loop 的 messages 中提取实际被调用的 skill 记录。
///
/// 检测三种来源：
/// - **工具型 skill**：exec 调用 `tool.py`/`tool.sh`
/// - **提示型 skill**：read_file 读取 `SKILL.md`
/// - **子 agent skill**：dispatch_subtasks 返回的结构化摘要中包含的 skills_used
///
/// 只记录有明确调用/读取证据的 skill，不含仅注入 prompt 摘要但未实际使用的。
pub fn extract_skills_used(
    messages: &[HashMap<String, Value>],
    workspace_key: &str,
    user_name: &str,
) -> Vec<Value> {
    let mut skills = Vec::new();
    let mut seen = HashSet::new();

    for msg in messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");

        // 从 assistant tool_calls 中提取（主 agent 的 exec/read_file）
        if role == "assistant" {
            if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tool_calls {
                    let tool_name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    let args_str = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                        .unwrap_or("");

                    let extracted = match tool_name {
                        "exec" => extract_skill_from_exec(args_str),
                        "read_file" => extract_skill_from_read(args_str),
                        _ => None,
                    };

                    if let Some((skill_name, invoke_type)) = extracted {
                        if seen.insert(skill_name.clone()) {
                            skills.push(serde_json::json!({
                                "skill": skill_name,
                                "type": invoke_type,
                                "workspace_key": workspace_key,
                                "user_name": user_name,
                            }));
                        }
                    }
                }
            }
        }

        // 从 tool result 中提取子 agent 的 skill 使用（dispatch_subtasks 返回的摘要）
        if role == "tool" {
            if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                if let Some(start) = content.find("[[TYCLAW_DISPATCH_SUMMARY]]") {
                    if let Some(end) = content.find("[[/TYCLAW_DISPATCH_SUMMARY]]") {
                        let json_str = &content[start + "[[TYCLAW_DISPATCH_SUMMARY]]".len()..end].trim();
                        if let Ok(summary) = serde_json::from_str::<Value>(json_str) {
                            if let Some(sub_skills) = summary.get("skills_used").and_then(|v| v.as_array()) {
                                for s in sub_skills {
                                    let name = s.get("skill").and_then(|v| v.as_str()).unwrap_or("");
                                    if !name.is_empty() && seen.insert(name.to_string()) {
                                        skills.push(s.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    skills
}

/// 从 exec 命令中提取 skill 名称（匹配 tool.py/tool.sh 路径）。
fn extract_skill_from_exec(args_json: &str) -> Option<(String, &'static str)> {
    let args: HashMap<String, Value> = serde_json::from_str(args_json).ok()?;
    let command = args.get("command").and_then(|v| v.as_str())?;

    for segment in command.split_whitespace() {
        let path = segment.trim_matches(|c: char| c == '"' || c == '\'');
        if path.ends_with("/tool.py") || path.ends_with("/tool.sh") {
            if let Some(name) = parent_dir_name(path) {
                return Some((name, "tool"));
            }
        }
    }
    None
}

/// 从 read_file 参数中提取 skill 名称（匹配 SKILL.md 路径）。
fn extract_skill_from_read(args_json: &str) -> Option<(String, &'static str)> {
    let args: HashMap<String, Value> = serde_json::from_str(args_json).ok()?;
    let path = args.get("path").and_then(|v| v.as_str())?;

    if path.ends_with("/SKILL.md") {
        if let Some(name) = parent_dir_name(path) {
            return Some((name, "prompt"));
        }
    }
    None
}

/// 提取路径中倒数第二层目录名（即 skill 名称）。
fn parent_dir_name(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() >= 2 {
        let name = parts[parts.len() - 2];
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
}

/// 去除发给用户前的内部标记（`[[TYCLAW_*]]` 系列）。
pub(crate) fn strip_internal_markers(text: &str) -> String {
    text.replace("[[TYCLAW_COMPACT_ASSISTANT]]", "")
        .replace("[[TYCLAW_COMPACT_SYSTEM]]", "")
        .replace("[[TYCLAW_MAX_REACHED_RESET_NEXT_RUN]]", "")
        .trim()
        .to_string()
}
