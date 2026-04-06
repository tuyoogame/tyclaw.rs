//! Nudge 提示词加载（基于 prompt_store）。

use std::path::Path;

pub fn init(workspace: &Path) {
    crate::prompt_store::init(workspace);
}

pub fn plan_required() -> String {
    crate::prompt_store::get_nudge("plan_required")
}

pub fn explore_hard_block() -> String {
    crate::prompt_store::get_nudge("explore_hard_block")
}

pub fn idle_spin() -> String {
    crate::prompt_store::get_nudge("idle_spin")
}

pub fn param_error_retry() -> String {
    crate::prompt_store::get_nudge("param_error_retry")
}

pub fn explore_budget_warning(current: usize, max: usize) -> String {
    let template = crate::prompt_store::get_nudge("explore_budget_warning");
    template
        .replace("{current}", &current.to_string())
        .replace("{max}", &max.to_string())
}
