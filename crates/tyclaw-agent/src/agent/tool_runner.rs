//! 单轮 `tool_calls` 的顺序执行、输出截断与 exec 历史更新。
//!
//! 从 `agent_loop` 拆出，便于阅读与单测；不包含消息链写入。

use std::collections::{HashMap, VecDeque};

use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

use tyclaw_provider::types::ToolCallRequest;
use tyclaw_tool_abi::{ToolExecutionResult, ToolRuntime};

use crate::loop_helpers::{
    exec_command_fingerprint, extract_inline_script, is_error_envelope, EXEC_HISTORY_WINDOW,
    EXEC_INLINE_EXTRACT_THRESHOLD, MAX_TOOL_OUTPUT_CHARS, MAX_TOOL_OUTPUT_CHARS_EXPLORE,
    READ_FILE_TOOL_MAX_CHARS, REPEAT_EXEC_BLOCK_THRESHOLD, REPEAT_EXEC_MIN_CMD_LEN,
};
use crate::runtime::ToolExecutionEvent;

const DISPATCH_SUMMARY_START: &str = "[[TYCLAW_DISPATCH_SUMMARY]]";
const DISPATCH_SUMMARY_END: &str = "[[/TYCLAW_DISPATCH_SUMMARY]]";

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DispatchStructuredSummary {
    pub plan_id: String,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub has_conflicts: bool,
    pub wall_time_ms: u64,
    #[serde(default)]
    pub nodes: Vec<DispatchNodeSummary>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DispatchNodeSummary {
    pub node_id: String,
    pub node_status: String,
    pub declared_result_status: String,
    pub verified_after_last_edit: Option<bool>,
    pub ended_with_unverified_changes: bool,
    pub tools_used_count: usize,
    pub detail_path: String,
}

/// 单条工具执行结果（已截断），供写入 `messages` 与 `ContextManager`。
pub(crate) struct ProcessedToolResult {
    pub tool_id: String,
    pub tool_name: String,
    pub result: String,
    pub original_len: usize,
    pub status: String,
    pub route: String,
    pub duration_ms: u64,
    pub event: ToolExecutionEvent,
    pub dispatch_summary: Option<DispatchStructuredSummary>,
}

pub(crate) struct ToolRoundOutcome {
    pub per_tool: Vec<ProcessedToolResult>,
    pub progressed: bool,
    /// ask_user 被调用时，记录 (tool_call_id, question)
    pub ask_user_pending: Option<(String, String)>,
}

/// 按 `tool_calls` 顺序串行执行一轮工具，并做输出截断与 exec 指纹入队。
pub(crate) async fn run_tool_calls_round(
    registry: &dyn ToolRuntime,
    tool_calls: &[ToolCallRequest],
    total_iterations: usize,
    in_exploration: bool,
    explore_exec_hard_blocked: bool,
    recent_exec_commands: &mut VecDeque<u64>,
    phase: &str,
) -> ToolRoundOutcome {
    let recent_exec_snapshot: Vec<u64> = recent_exec_commands.iter().copied().collect();
    let mut ask_user_pending: Option<(String, String)> = None;
    let mut exec_cmd_by_tool_id: HashMap<String, u64> = HashMap::new();
    for tc in tool_calls {
        if tc.name == "exec" {
            if let Some(cmd) = tc.arguments.get("command").and_then(|v| v.as_str()) {
                exec_cmd_by_tool_id.insert(tc.id.clone(), exec_command_fingerprint(cmd));
            }
        }
    }

    // 一轮内只允许执行一个 dispatch_subtasks，找到最后一个的 id，前面的跳过。
    // 原因：LLM 偶尔在一轮内返回多个 dispatch，每个都会启动子 agent 消耗资源，
    // 但只有最后一个的结果对主控有意义（前面的会被覆盖/忽略）。
    let last_dispatch_id: Option<String> = tool_calls
        .iter()
        .rev()
        .find(|tc| tc.name == "dispatch_subtasks")
        .map(|tc| tc.id.clone());
    let dispatch_count = tool_calls
        .iter()
        .filter(|tc| tc.name == "dispatch_subtasks")
        .count();

    let mut executed: Vec<(String, String, ToolExecutionResult)> = Vec::new();
    for tc in tool_calls {
        let tool_name = tc.name.clone();
        let tool_id = tc.id.clone();
        let mut args = tc.arguments.clone();

        // 跳过非最后一个 dispatch_subtasks
        if tool_name == "dispatch_subtasks"
            && dispatch_count > 1
            && last_dispatch_id.as_deref() != Some(&tool_id)
        {
            warn!(
                tool_id = %tool_id,
                dispatch_count,
                "Skipping duplicate dispatch_subtasks (only last one per round is executed)"
            );
            executed.push((
                tool_id,
                tool_name,
                ToolExecutionResult {
                    output: "(skipped: only one dispatch_subtasks per round is executed; this was superseded by a later call)".to_string(),
                    route: "agent".into(),
                    status: "skipped".into(),
                    duration_ms: 0,
                    gate_action: "bypass".into(),
                    risk_level: "policy".into(),
                    sandbox_id: None,
                },
            ));
            continue;
        }

        if tool_name == "ask_user" {
            let question = args
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            info!(
                question = %question,
                tool_id = %tool_id,
                total_iterations,
                "ask_user: pausing agent loop to wait for user reply"
            );
            // 记录 ask_user 请求，agent_loop 会检测并暂停
            ask_user_pending = Some((tool_id.clone(), question));
            // 不执行后续工具——暂停在这里
            break;
        }

        if tool_name == "exec" && explore_exec_hard_blocked {
            executed.push((
                tool_id,
                tool_name,
                ToolExecutionResult {
                    output: crate::nudge_loader::explore_hard_block(),
                    route: "agent".into(),
                    status: "blocked".into(),
                    duration_ms: 0,
                    gate_action: "bypass".into(),
                    risk_level: "policy".into(),
                    sandbox_id: None,
                },
            ));
            continue;
        }

        if tool_name == "exec" && in_exploration {
            if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                if cmd.len() >= REPEAT_EXEC_MIN_CMD_LEN {
                    let fp = exec_command_fingerprint(cmd);
                    let repeats = recent_exec_snapshot
                        .iter()
                        .copied()
                        .filter(|x| *x == fp)
                        .count();
                    let threshold = REPEAT_EXEC_BLOCK_THRESHOLD + 1;
                    if repeats >= threshold {
                        executed.push((
                            tool_id,
                            tool_name,
                            ToolExecutionResult {
                                output: "[BLOCKED] Repeated exec detected. The same large command was already run multiple times. Reuse prior evidence or merge checks into one script instead of re-running near-identical exec.".to_string(),
                                route: "agent".into(),
                                status: "blocked".into(),
                                duration_ms: 0,
                                gate_action: "bypass".into(),
                                risk_level: "policy".into(),
                                sandbox_id: None,
                            },
                        ));
                        continue;
                    }
                }
            }
        }

        if tool_name == "exec" {
            if let Some(cmd_val) = args.get("command") {
                let cmd = cmd_val.as_str().unwrap_or("");
                if cmd.len() > EXEC_INLINE_EXTRACT_THRESHOLD {
                    if let Some(new_cmd) = extract_inline_script(cmd) {
                        info!(
                            original_len = cmd.len(),
                            "Extracted inline script to temp file"
                        );
                        args.insert("command".into(), json!(new_cmd));
                    }
                }
            }
        }

        // 门禁检查已在 ToolExecutor 层统一处理
        let result = registry.execute(&tool_name, args).await;
        executed.push((tool_id, tool_name, result));
    }

    let mut per_tool = Vec::new();
    let mut progressed = false;

    for (tool_id, tool_name, mut execution) in executed {
        let (clean_output, dispatch_summary) = extract_dispatch_summary(&execution.output);
        execution.output = clean_output;
        let original_len = execution.output.len();
        // 图片 data URI（[[IMAGE:...]]）不截断，完整传递给多模态处理
        let is_image = execution.output.contains("[[IMAGE:data:image/");
        let max_chars = if tool_name == "read_file" {
            READ_FILE_TOOL_MAX_CHARS
        } else if in_exploration {
            MAX_TOOL_OUTPUT_CHARS_EXPLORE
        } else {
            MAX_TOOL_OUTPUT_CHARS
        };
        let truncated = !is_image && execution.output.len() > max_chars;
        let result = if truncated {
            let boundary = execution.output.floor_char_boundary(max_chars);
            let mut truncated = execution.output[..boundary].to_string();
            if in_exploration {
                truncated.push_str(&format!(
                    "\n\n... [Output truncated: showing {max_chars}/{original_len} chars]\n\
                     [EXPLORATION PHASE — HARD RULE]: Output was too large.\n\
                     ❌ DO NOT read 'remaining rows from line N' — this is forbidden paging.\n\
                     ❌ DO NOT print sheets row by row with for-loops.\n\
                     ❌ DO NOT call exec again just to see more data.\n\
                     ✅ Use df.head(3), df.shape, df.dtypes, df.describe() ONLY.\n\
                     ✅ Inspect ALL sheets in ONE exec call using a loop.\n\
                     ✅ After {max_chars} chars you have enough structure info — start writing the solution script NOW."
                ));
            } else {
                truncated.push_str(&format!(
                    "\n\n... [Output truncated: showing {max_chars}/{original_len} chars]\n\
                     [SYSTEM WARNING]: The output is too large. DO NOT try to print raw data page by page. \
                     STOP inspecting data visually. Write a script to aggregate/filter data instead."
                ));
            }
            warn!(tool = %tool_name, original_len, max_chars, phase = %phase, "Tool output truncated");
            truncated
        } else {
            execution.output.clone()
        };

        if execution.status == "ok" && is_error_envelope(&result) {
            execution.status = "error".into();
        }
        let result_len = result.len();
        let preview = build_result_preview(&result);
        info!(
            tool = %tool_name,
            route = %execution.route,
            status = %execution.status,
            duration_ms = execution.duration_ms,
            gate_action = %execution.gate_action,
            risk_level = %execution.risk_level,
            sandbox_id = execution.sandbox_id.as_deref().unwrap_or("-"),
            original_len,
            result_len,
            preview = %preview,
            "Tool result"
        );

        if execution.status != "denied"
            && execution.status != "error"
            && execution.status != "blocked"
            && !result.starts_with("[DENIED]")
            && !is_error_envelope(&result)
        {
            progressed = true;
        }

        if tool_name == "exec" {
            if let Some(sig) = exec_cmd_by_tool_id.get(&tool_id) {
                recent_exec_commands.push_back(*sig);
                while recent_exec_commands.len() > EXEC_HISTORY_WINDOW {
                    recent_exec_commands.pop_front();
                }
            }
        }

        per_tool.push(ProcessedToolResult {
            tool_id: tool_id.clone(),
            tool_name: tool_name.clone(),
            result,
            original_len,
            status: execution.status.clone(),
            route: execution.route.clone(),
            duration_ms: execution.duration_ms,
            event: ToolExecutionEvent {
                tool_id,
                tool_name,
                route: execution.route,
                status: execution.status,
                duration_ms: execution.duration_ms,
                original_len,
                result_len,
                truncated,
                gate_action: execution.gate_action,
                risk_level: execution.risk_level,
                sandbox_id: execution.sandbox_id,
                result_preview: preview,
            },
            dispatch_summary,
        });
    }

    ToolRoundOutcome {
        per_tool,
        progressed,
        ask_user_pending,
    }
}

fn extract_dispatch_summary(output: &str) -> (String, Option<DispatchStructuredSummary>) {
    let Some(start_idx) = output.find(DISPATCH_SUMMARY_START) else {
        return (output.to_string(), None);
    };
    let Some(end_idx) = output.find(DISPATCH_SUMMARY_END) else {
        return (output.to_string(), None);
    };
    if end_idx < start_idx {
        return (output.to_string(), None);
    }

    let json_start = start_idx + DISPATCH_SUMMARY_START.len();
    let json_slice = output[json_start..end_idx].trim();
    let summary = match serde_json::from_str::<DispatchStructuredSummary>(json_slice) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to parse dispatch summary JSON");
            None
        }
    };

    let mut clean = String::new();
    clean.push_str(output[..start_idx].trim_end());
    let suffix = output[end_idx + DISPATCH_SUMMARY_END.len()..].trim();
    if !suffix.is_empty() {
        if !clean.is_empty() {
            clean.push_str("\n\n");
        }
        clean.push_str(suffix);
    }

    (clean, summary)
}

fn build_result_preview(result: &str) -> String {
    let sanitized = result.replace('\n', "\\n");
    let max_chars = 160;
    if sanitized.len() <= max_chars {
        sanitized
    } else {
        let boundary = sanitized.floor_char_boundary(max_chars);
        format!("{}...", &sanitized[..boundary])
    }
}

#[cfg(test)]
mod tests {
    use super::extract_dispatch_summary;

    #[test]
    fn extract_dispatch_summary_strips_hidden_metadata() {
        let output = "visible summary\n\n[[TYCLAW_DISPATCH_SUMMARY]]\n{\"plan_id\":\"p1\",\"succeeded\":1,\"failed\":0,\"skipped\":0,\"has_conflicts\":false,\"wall_time_ms\":12,\"nodes\":[{\"node_id\":\"n1\",\"node_status\":\"success\",\"declared_result_status\":\"verified\",\"verified_after_last_edit\":true,\"ended_with_unverified_changes\":false,\"tools_used_count\":2,\"detail_path\":\"/tmp/n1.md\"}]}\n[[/TYCLAW_DISPATCH_SUMMARY]]";
        let (clean, summary) = extract_dispatch_summary(output);

        assert_eq!(clean, "visible summary");
        let summary = summary.expect("dispatch summary");
        assert_eq!(summary.plan_id, "p1");
        assert_eq!(summary.nodes.len(), 1);
        assert_eq!(summary.nodes[0].declared_result_status, "verified");
    }
}
