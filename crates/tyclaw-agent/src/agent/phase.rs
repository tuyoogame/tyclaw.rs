//! 探索 / 产出阶段：ContextState 同步与后置 system nudge。
//!
//! 从 `agent_loop` 拆出，便于单测与阅读；不包含 LLM 调用。

use serde_json::Value;
use std::collections::HashMap;

use crate::context_state::{AgentPhase, ContextManager};
// STATE_VIEW 暂时禁用
// use crate::loop_helpers::{FIRST_TURN_STATE_SNAPSHOT_CHARS, STATE_SNAPSHOT_CHARS};

/// 每轮迭代前：根据阶段与历史工具使用更新 `ContextManager` 的 phase / focus / plan。
pub(crate) fn sync_context_state_for_iteration(
    ctx_state: &mut ContextManager,
    in_exploration: bool,
    exploration_iterations: usize,
    explore_max: usize,
    has_dispatch: bool,
    has_write: bool,
    dispatch_count: usize,
    write_count: usize,
    tools_used: &[String],
) {
    let has_recent_dispatch = tools_used
        .iter()
        .rev()
        .take(3)
        .any(|t| t == "dispatch_subtasks");

    if in_exploration {
        ctx_state.set_phase(AgentPhase::Explore);
        let progress = format!("Explore ({exploration_iterations}/{explore_max}).");
        let guidance = if exploration_iterations <= 1 {
            "Read the task requirements first. Extract key info (formulas, column names, rules) from user message before exploring."
        } else if exploration_iterations <= explore_max / 2 {
            "Merge queries: inspect multiple sheets/files in ONE exec call. Don't explore one-by-one."
        } else {
            "Exploration budget nearly exhausted. Start producing output NOW with information already gathered."
        };
        let dispatch_hint = if has_dispatch {
            " If ≥2 files to create/modify, plan to use dispatch_subtasks."
        } else {
            ""
        };
        ctx_state.set_focus(&format!("{progress} {guidance}{dispatch_hint}"));
    } else if dispatch_count == 0 && write_count == 0 {
        ctx_state.set_phase(AgentPhase::Execute);
        ctx_state.set_focus(if has_dispatch {
            "Produce phase started. Use dispatch_subtasks for file creation. Set dependencies between subtasks correctly."
        } else {
            "Produce phase started. Write files with write_file/edit_file. Don't over-verify — one comprehensive check is enough."
        });
    } else if dispatch_count > 0 || write_count > 0 {
        if has_recent_dispatch {
            ctx_state.set_phase(AgentPhase::Execute);
            ctx_state.set_focus(&format!(
                "dispatch_subtasks returned ({dispatch_count} calls so far, {write_count} writes). \
                 Review the results. If files created successfully, verify or finalize. \
                 If failed, analyze and retry with improved prompts."
            ));
        } else {
            ctx_state.set_phase(AgentPhase::Summarize);
            ctx_state.set_focus(&format!(
                "Production mostly done ({dispatch_count} dispatches, {write_count} writes). \
                 Finalize: verify outputs if not yet verified, then output final summary to complete the task."
            ));
        }
    }

    let mut plan: Vec<String> = vec![
        "Reuse existing evidence before new tool calls.".into(),
        "If data seems large, aggregate in script instead of paging output.".into(),
    ];
    if has_dispatch {
        plan.push(
            "dispatch_subtasks: set dependencies when subtasks have input/output relationships. Default to serial if unsure."
                .into(),
        );
    }
    if has_write {
        plan.push("Move to write_file/edit_file when logic is clear.".into());
    }
    ctx_state.upsert_plan(plan);
}

// STATE_VIEW 暂时禁用（破坏 prompt cache 前缀匹配），函数保留备用。
// pub(crate) fn state_snapshot_limit_chars(total_iterations: usize) -> usize {
//     if total_iterations == 1 {
//         FIRST_TURN_STATE_SNAPSHOT_CHARS
//     } else {
//         STATE_SNAPSHOT_CHARS
//     }
// }

/// 探索阶段在一轮工具执行后的结果：是否应切换到产出阶段，以及要追加的 system 消息。
pub(crate) struct ExplorePhaseAfterTools {
    pub transition_to_produce: bool,
    pub nudge_messages: Vec<HashMap<String, Value>>,
}

/// 探索阶段：检测是否应切换到产出阶段。
pub(crate) fn explore_phase_after_tools(
    _exploration_iterations: usize,
    _explore_max: usize,
    has_production: bool,
) -> ExplorePhaseAfterTools {
    ExplorePhaseAfterTools {
        transition_to_produce: has_production,
        nudge_messages: vec![],
    }
}
