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

pub fn empty_promise_retry() -> String {
    crate::prompt_store::get_nudge("empty_promise_retry")
}

/// Long-term Memory 块前的护栏文本（顶层 prompts.yaml 字段）。
///
/// 配置缺失或 prompt_store 未 init 时静默返回空串——单元测试或老配置文件
/// 升级前的兼容场景不希望 panic。
pub fn memory_guard() -> String {
    crate::prompt_store::try_get("memory_guard").unwrap_or_default()
}

pub fn explore_budget_warning(current: usize, max: usize) -> String {
    let template = crate::prompt_store::get_nudge("explore_budget_warning");
    template
        .replace("{current}", &current.to_string())
        .replace("{max}", &max.to_string())
}
