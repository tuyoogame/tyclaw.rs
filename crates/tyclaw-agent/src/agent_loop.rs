//! ReAct 循环引擎 —— TyClaw Agent 的核心执行逻辑。
//!
//! ReAct（Reasoning + Acting）模式的工作流程：
//! 1. 将当前消息列表 + 工具定义发送给 LLM
//! 2. LLM 返回文本回复和/或工具调用请求
//! 3. 如果有工具调用：通过门禁检查 → 执行工具 → 将结果加入消息历史 → 回到第1步
//! 4. 如果没有工具调用：返回最终回复，循环结束
//! 5. 如果达到最大迭代次数：强制终止并提示用户

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tracing::{debug, info, warn};

use tyclaw_provider::{parse_reasoning, LLMProvider};
use tyclaw_tool_abi::{ToolDefinitionProvider, ToolRuntime};
use tyclaw_types::tokens::estimate_prompt_tokens_chain;
use tyclaw_types::TyclawError;

use crate::agent::iteration_budget::IterationBudget;
use crate::agent::phase;
use crate::agent::tool_runner;
use crate::agent::tool_runner::DispatchStructuredSummary;
use crate::compression::compress_tool_results;
use crate::context_state::ContextManager;
use crate::loop_helpers::{
    dedupe_consecutive_system_messages, infer_phase_from_messages, is_production_tool,
    latest_user_goal, maybe_compact_assistant_history, maybe_compact_system_message, strip_think,
    take_reset_marker, tool_batch_signature, LIGHT_COMPRESS_RECENT_ROUNDS, MAX_NO_PROGRESS_ROUNDS,
    MAX_REACHED_RESET_MARKER, MAX_REPEAT_TOOL_BATCH, PLAN_CHECK_ITERATIONS,
};
use crate::runtime::{
    chat_message, AgentRuntime, DecisionEvent, OnProgress, RunDiagnosticsSummary, RuntimeResult,
    RuntimeStatus, ToolExecutionEvent,
};
use tyclaw_prompt::ContextBuilder;

/// ReAct 循环引擎 —— 迭代式 LLM 调用 + 工具执行。
pub struct AgentLoop {
    provider: Arc<dyn LLMProvider>,
    tools: Arc<dyn ToolRuntime>,
    model: Option<String>,
    max_iterations: usize,
    /// 累计输出字符预算（含 content + reasoning）。
    /// None = 无限制（主 agent 默认），Some(n) = 超过 n 字符后强制停止。
    /// 用于约束 sub-agent 的总输出量，防止 GLM 等模型疯狂输出。
    max_output_chars: Option<usize>,
}

#[derive(Debug, Clone)]
struct PendingDispatchObservation {
    origin_iteration: usize,
    statuses: Vec<String>,
    summary_preview: String,
}

impl AgentLoop {
    pub fn new(
        provider: Arc<dyn LLMProvider>,
        tools: impl Into<Arc<dyn ToolRuntime>>,
        model: Option<String>,
        max_iterations: Option<usize>,
    ) -> Self {
        let actual_model = model.or_else(|| Some(provider.default_model().to_string()));
        Self {
            provider,
            tools: tools.into(),
            model: actual_model,
            max_iterations: max_iterations.unwrap_or(25),
            max_output_chars: None,
        }
    }

    /// 设置累计输出字符预算。超过后 agent loop 强制停止并返回已有结果。
    pub fn with_max_output_chars(mut self, max_chars: usize) -> Self {
        self.max_output_chars = Some(max_chars);
        self
    }
}

#[async_trait]
impl AgentRuntime for AgentLoop {
    async fn run(
        &self,
        initial_messages: Vec<HashMap<String, Value>>,
        _user_role: &str,
        cache_scope: Option<&str>,
        on_progress: Option<&OnProgress>,
    ) -> Result<RuntimeResult, TyclawError> {
        let mut messages = initial_messages;
        let mut tools_used: Vec<String> = Vec::new();
        let mut tool_events: Vec<ToolExecutionEvent> = Vec::new();
        let mut decision_events: Vec<DecisionEvent> = Vec::new();
        let mut pending_dispatch_observation: Option<PendingDispatchObservation> = None;
        let mut final_content: Option<String> = None;
        let tool_defs = self.tools.get_definitions();

        let mut last_batch_sig: Option<String> = None;
        let mut repeated_batches: usize = 0;
        let mut no_progress_rounds: usize = 0;
        let mut cumulative_output_chars: usize = 0;
        let goal = latest_user_goal(&messages);
        let mut ctx_state = ContextManager::new(goal.clone());
        ctx_state.ingest_user_message(&goal);
        ctx_state
            .add_fact("The user request should be completed with minimal redundant tool calls.");
        ctx_state.add_hypothesis(
            "Structured state snapshots can reduce repeated exec exploration.",
            "Testing",
        );

        // 规划追踪：是否已输出计划
        let mut has_plan = false;
        // 最近 exec 命令（归一化后），用于温和重复防抖。
        let mut recent_exec_commands: VecDeque<u64> = VecDeque::new();
        // Sub-agent 空转检测：产出阶段连续 exec 但没有 write/edit 的计数
        let mut consecutive_exec_without_write: usize = 0;
        let is_sub_agent = self.max_output_chars.is_some();
        // 累计 token 统计
        let mut total_cache_hit: u64 = 0;
        let mut total_cache_write: u64 = 0;
        let mut total_prompt_tokens: u64 = 0;
        let mut total_completion_tokens: u64 = 0;

        // 阶段追踪：探索 → 产出
        // 仅在"上一轮 reach max"后重置计数，否则沿用历史推断（支持连续任务续跑）。
        let should_reset_iterations = take_reset_marker(&mut messages);

        // 生成本轮唯一 turn_id，所有新增消息都打上此标记，
        // save_turn 据此精确筛选本轮消息。
        let turn_id = format!("t_{}", chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f"));
        let (
            mut in_exploration,
            mut exploration_iterations,
            mut production_iterations,
            mut total_iterations,
        ) = if should_reset_iterations {
            (true, 0, 0, 0)
        } else {
            infer_phase_from_messages(&messages)
        };
        let budget = IterationBudget::new(self.max_iterations);
        let explore_max = budget.explore_max;
        if should_reset_iterations {
            info!(
                exploration_iterations,
                production_iterations,
                total_iterations,
                "Iteration counters reset because previous run hit max iterations"
            );
        } else if !in_exploration {
            info!(
                exploration_iterations,
                production_iterations,
                total_iterations,
                "Resumed in production phase (detected from message history)"
            );
        } else if total_iterations > 0 {
            info!(
                exploration_iterations,
                total_iterations, "Resumed in exploration phase (detected from message history)"
            );
        }

        let mut status = RuntimeStatus::Complete;

        loop {
            // 检查是否有运行期间注入的用户消息
            if let Ok(queue) = crate::runtime::INJECTION_QUEUE
                .try_with(|q: &crate::runtime::InjectionQueue| q.clone())
            {
                if let Ok(mut pending) = queue.lock() {
                    if !pending.is_empty() {
                        let injected: Vec<_> = pending.drain(..).collect();
                        info!(count = injected.len(), "Injected user messages into running loop");
                        messages.extend(injected);
                    }
                }
            }

            total_iterations += 1;
            // 检查总轮次上限（探索+产出不超过 2 * max_iterations）
            if total_iterations > budget.global_llm_cap() {
                warn!(
                    total_iterations,
                    cap = budget.global_llm_cap(),
                    "Absolute max iterations reached"
                );
                break;
            }
            // 检查当前阶段轮次上限
            if in_exploration && exploration_iterations >= self.max_iterations {
                warn!(
                    exploration_iterations,
                    "Exploration phase max iterations reached"
                );
                break;
            }
            if !in_exploration && production_iterations >= self.max_iterations {
                warn!(
                    production_iterations,
                    "Production phase max iterations reached"
                );
                break;
            }

            let phase = if in_exploration { "explore" } else { "produce" };
            let phase_iter = if in_exploration {
                exploration_iterations + 1
            } else {
                production_iterations + 1
            };

            // 第3轮开始自动切 system prompt 精简版，降低历史冗余占比。
            maybe_compact_system_message(&mut messages, total_iterations);
            // 历史 assistant 长文本压缩 + system 连续重复去重。
            maybe_compact_assistant_history(&mut messages);
            dedupe_consecutive_system_messages(&mut messages);

            // 更新阶段计数
            if in_exploration {
                exploration_iterations += 1;
            } else {
                production_iterations += 1;
            }
            info!(
                phase,
                phase_iter,
                total = total_iterations,
                max = self.max_iterations,
                msg_count = messages.len(),
                "=== Iteration ==="
            );

            // CLI 进度：打印当前轮次
            if let Some(cb) = on_progress {
                // 超长任务提醒：超过 30 轮时发一次文本通知（约 5 分钟+）
                if total_iterations == 30 {
                    cb("[heartbeat]🦀 仍在处理中，请耐心等待...").await;
                }
                cb(&format!(
                    "[轮次 {total_iterations}] 阶段={phase} 第{phase_iter}轮"
                ))
                .await;
            }

            // 压缩旧的工具结果，减少 prompt tokens
            // 探索阶段前半段不压缩（让 LLM 看到完整 schema），后半段开始压缩
            let skip_compression = in_exploration && exploration_iterations <= explore_max / 2;
            let protected_prefix_len = cache_scope
                .map(|scope| self.provider.cache_breakpoint_idx(scope))
                .unwrap_or(0);
            let compressed = compress_tool_results(
                &messages,
                skip_compression,
                LIGHT_COMPRESS_RECENT_ROUNDS,
                protected_prefix_len,
            );

            let has_dispatch = self.tools.has_tool("dispatch_subtasks");
            let has_write = self.tools.has_tool("write_file");
            let dispatch_count = tools_used
                .iter()
                .filter(|t| *t == "dispatch_subtasks")
                .count();
            let write_count = tools_used
                .iter()
                .filter(|t| *t == "write_file" || *t == "edit_file")
                .count();

            phase::sync_context_state_for_iteration(
                &mut ctx_state,
                in_exploration,
                exploration_iterations,
                explore_max,
                has_dispatch,
                has_write,
                dispatch_count,
                write_count,
                &tools_used,
            );

            // TODO: STATE_VIEW 每轮内容变化会破坏 prompt cache 的前缀匹配，
            // 暂时禁用以验证缓存增长。后续应改为嵌入 user 消息或固定化。
            // let snapshot_chars = phase::state_snapshot_limit_chars(total_iterations);
            // let snapshot = ctx_state.render_prompt_context(snapshot_chars);
            // if !snapshot.is_empty() {
            //     compressed.push(chat_message(
            //         "system",
            //         &format!("[[TYCLAW_STATE_VIEW]]\n{snapshot}"),
            //     ));
            // }

            // === Context 快照（可选） ===
            // 仅在显式开启 write_snapshot 时落盘，默认关闭以降低 I/O 和磁盘占用。
            // 调试观测产物统一写到 sessions/snap/ 下，与会话历史/附件缓存隔离。
            // snapshot 由 provider 层 chat_stream 内写入（含 cache_control 等最终加工），
            // 不在此处重复写入。

            // 进度指标：本轮请求 LLM 的上下文大小（估算 token + 序列化字符数）。
            let (prompt_tokens_est, estimator) =
                estimate_prompt_tokens_chain(&compressed, Some(&tool_defs));
            let req_chars = serde_json::to_string(&compressed)
                .map(|s| s.len())
                .unwrap_or(0)
                + serde_json::to_string(&tool_defs)
                    .map(|s| s.len())
                    .unwrap_or(0);
            if let Some(cb) = on_progress {
                cb(&format!(
                    "  ↗ LLM请求: ~{prompt_tokens_est} tokens ({estimator}), {req_chars} chars, messages={}",
                    compressed.len()
                ))
                .await;
            }

            let response = self
                .provider
                .chat_with_retry(
                    compressed,
                    Some(tool_defs.clone()),
                    self.model.clone(),
                    cache_scope.map(str::to_string),
                )
                .await;
            debug!(
                target: "react.response",
                finish_reason = %response.finish_reason,
                has_tool_calls = response.has_tool_calls(),
                has_reasoning = response.reasoning_content.is_some(),
                reasoning_len = response.reasoning_content.as_ref().map(|r| r.len()).unwrap_or(0),
                "Received LLM response",
            );

            // 累计 token 统计
            total_prompt_tokens += response.usage.get("prompt_tokens").copied().unwrap_or(0);
            total_completion_tokens += response
                .usage
                .get("completion_tokens")
                .copied()
                .unwrap_or(0);
            total_cache_hit += response.usage.get("cached_tokens").copied().unwrap_or(0);
            total_cache_write += response
                .usage
                .get("cache_write_tokens")
                .copied()
                .unwrap_or(0);

            // 累计输出字符跟踪（content + reasoning）
            let this_output_chars = response.content.as_ref().map(|s| s.len()).unwrap_or(0)
                + response
                    .reasoning_content
                    .as_ref()
                    .map(|s| s.len())
                    .unwrap_or(0);
            cumulative_output_chars += this_output_chars;

            // 检查累计输出预算
            if let Some(max_chars) = self.max_output_chars {
                if cumulative_output_chars > max_chars {
                    warn!(
                        cumulative_output_chars,
                        max_chars,
                        iteration = total_iterations,
                        "Output budget exceeded — forcing stop"
                    );
                    // 如果有工具写过文件，视为成功完成
                    let has_written = tools_used
                        .iter()
                        .any(|t| t == "write_file" || t == "edit_file");
                    final_content = Some(if has_written {
                        format!(
                            "Task completed (output budget reached after {} chars). Files written successfully. Tools used: {}",
                            cumulative_output_chars, tools_used.join(", ")
                        )
                    } else {
                        let partial = strip_think(response.content.as_deref()).unwrap_or_default();
                        if partial.is_empty() {
                            format!(
                                "Output budget exceeded ({} chars). Partial result unavailable.",
                                cumulative_output_chars
                            )
                        } else {
                            partial
                        }
                    });
                    break;
                }
            }

            if response.has_tool_calls() {
                // === 规划阶段检测 ===
                // 检查 LLM 是否输出了计划文本（content 长度 > 100 视为有实质性规划）
                let content_text = strip_think(response.content.as_deref());
                if let Some(obs) = pending_dispatch_observation.take() {
                    let next_tools: Vec<String> = response
                        .tool_calls
                        .iter()
                        .map(|tc| tc.name.clone())
                        .collect();
                    let (decision, reason) = classify_post_dispatch_tool_decision(&next_tools);
                    record_dispatch_followup_decision(
                        &mut decision_events,
                        total_iterations,
                        is_sub_agent,
                        &phase,
                        obs,
                        decision,
                        reason,
                        next_tools,
                    );
                }
                if let Some(ref text) = content_text {
                    if text.len() > 100 {
                        has_plan = true;
                    }
                }

                // 前 PLAN_CHECK_ITERATIONS 轮如果没有输出计划，注入催促
                // Sub-agent 不触发此催促（coding 类已有 workflow 要求，非 coding 类不需要规划）
                if !has_plan && total_iterations <= PLAN_CHECK_ITERATIONS && !is_sub_agent {
                    let nudge = crate::nudge_loader::plan_required();
                    warn!(total_iterations, "LLM skipped planning, injecting nudge");
                    messages.push(chat_message("system", &nudge));
                    // 不执行这轮的工具调用，让 LLM 重新回答
                    continue;
                }

                if let Some(cb) = on_progress {
                    // 先输出 reasoning（thinking 过程），使用解析器清洗
                    if let Some(ref reasoning) = response.reasoning_content {
                        let parsed = parse_reasoning(reasoning);
                        let display = if parsed.display_text.is_empty() {
                            // 如果解析后为空（纯 tool_call XML），显示摘要
                            if parsed.has_misplaced_tool_calls {
                                "[thinking contains tool calls only]".to_string()
                            } else {
                                String::new()
                            }
                        } else {
                            // 截断过长的 reasoning，避免刷屏
                            let max_display = 800;
                            if parsed.display_text.len() > max_display {
                                let boundary = parsed.display_text.floor_char_boundary(max_display);
                                format!(
                                    "{}... ({} chars total)",
                                    &parsed.display_text[..boundary],
                                    parsed.raw_length
                                )
                            } else {
                                parsed.display_text.clone()
                            }
                        };
                        if !display.is_empty() {
                            cb(&format!("[Thinking]\n{}", display)).await;
                        }
                    }
                    if let Some(ref thought) = content_text {
                        cb(thought).await;
                    }
                }

                let tool_call_dicts: Vec<Value> = response
                    .tool_calls
                    .iter()
                    .map(|tc| {
                        json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default(),
                            }
                        })
                    })
                    .collect();
                for tc in &response.tool_calls {
                    let args_json = serde_json::to_string(&tc.arguments).unwrap_or_default();
                    ctx_state.ingest_tool_call(&tc.name, &args_json);
                }

                ContextBuilder::add_assistant_message(
                    &mut messages,
                    response.content.as_deref(),
                    Some(tool_call_dicts),
                    response.reasoning_content.as_deref(),
                );

                let batch_sig = tool_batch_signature(&response.tool_calls);
                if last_batch_sig.as_deref() == Some(batch_sig.as_str()) {
                    repeated_batches += 1;
                } else {
                    repeated_batches = 0;
                    last_batch_sig = Some(batch_sig);
                }

                if repeated_batches >= MAX_REPEAT_TOOL_BATCH {
                    // 如果已经使用过 write_file/edit_file，说明有实际产出，
                    // 只是模型不知道何时停止。只输出成功消息，不要混入失败前缀，
                    // 否则主控 LLM 会误判为失败并重试。
                    let has_written = tools_used
                        .iter()
                        .any(|t| t == "write_file" || t == "edit_file");
                    if has_written {
                        warn!(
                            repeated_batches,
                            "Repeated tool batch detected after write — treating as completed"
                        );
                        final_content = Some(format!(
                            "Task completed. Files written successfully. Tools used: {}",
                            tools_used.join(", ")
                        ));
                    } else {
                        final_content = Some(
                            "I am repeatedly calling the same tools without progress. \
                             Please provide additional constraints or a narrower objective."
                                .into(),
                        );
                    }
                    break;
                }

                // 让出执行权，确保 progress channel 中排队的 Thinking/content 事件
                // 有机会被 dispatcher 消费并打印，避免 tool 执行（尤其是 dispatch_subtasks）
                // 的同步 stderr 输出抢先于 main 的异步 progress 输出。
                tokio::task::yield_now().await;

                // 探索阶段硬封：nudge 3条耗尽后仍在探索，直接拦截所有 exec
                let explore_exec_hard_blocked =
                    in_exploration && exploration_iterations >= explore_max + 3;

                let round_outcome = tool_runner::run_tool_calls_round(
                    self.tools.as_ref(),
                    &response.tool_calls,
                    total_iterations,
                    in_exploration,
                    explore_exec_hard_blocked,
                    &mut recent_exec_commands,
                    &phase,
                )
                .await;

                let progressed = round_outcome.progressed;
                for pr in round_outcome.per_tool {
                    tools_used.push(pr.tool_name.clone());
                    tool_events.push(pr.event.clone());
                    let dispatch_summary = pr.dispatch_summary.clone();
                    let tool_id = pr.tool_id;
                    let tool_name = pr.tool_name;
                    let result = pr.result;
                    let original_len = pr.original_len;
                    // CLI 进度：打印工具执行结果摘要
                    if let Some(cb) = on_progress {
                        // exec 工具额外显示具体命令
                        if tool_name == "exec" {
                            let cmd = response
                                .tool_calls
                                .iter()
                                .find(|tc| tc.id == tool_id)
                                .and_then(|tc| tc.arguments.get("command"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            // 截断过长的命令
                            let cmd_display = if cmd.len() > 120 {
                                &cmd[..cmd.floor_char_boundary(120)]
                            } else {
                                cmd
                            };
                            cb(&format!(
                                "  → {tool_name}: `{cmd_display}` ({original_len} chars, {}, {}, {}ms)",
                                pr.status,
                                pr.route,
                                pr.duration_ms
                            )).await;
                        } else {
                            cb(&format!(
                                "  → {tool_name} ({original_len} chars, {}, {}, {}ms)",
                                pr.status, pr.route, pr.duration_ms
                            ))
                            .await;
                        }
                    }
                    ContextBuilder::add_tool_result(&mut messages, &tool_id, &tool_name, &result);
                    ctx_state.ingest_tool_result(&tool_name, &result);
                    if tool_name == "dispatch_subtasks" {
                        if let Some(observation) = dispatch_summary.as_ref().and_then(|summary| {
                            observe_dispatch_result(total_iterations, summary, &result)
                        }) {
                            info!(
                                iteration = total_iterations,
                                phase = %phase,
                                agent_scope = if is_sub_agent { "sub" } else { "main" },
                                dispatch_statuses = ?observation.statuses,
                                summary_preview = %observation.summary_preview,
                                "Dispatch outcome observed"
                            );
                            pending_dispatch_observation = Some(observation);
                        }
                    }
                }

                let diagnostics = build_run_diagnostics_summary(&tool_events);
                let this_round_tools: Vec<String> = tools_used
                    .iter()
                    .rev()
                    .take(response.tool_calls.len())
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                info!(
                    iteration = total_iterations,
                    phase = %phase,
                    tools_this_round = ?this_round_tools,
                    total_tool_calls = diagnostics.total_tool_calls,
                    exec_count = diagnostics.exec_count,
                    dedicated_tool_count = diagnostics.dedicated_tool_count,
                    error_tool_count = diagnostics.error_tool_count,
                    denied_tool_count = diagnostics.denied_tool_count,
                    sandbox_tool_count = diagnostics.sandbox_tool_count,
                    host_tool_count = diagnostics.host_tool_count,
                    verified_after_last_edit = diagnostics.verified_after_last_edit,
                    ended_with_unverified_changes = diagnostics.ended_with_unverified_changes,
                    "Iteration tool summary"
                );

                if progressed {
                    no_progress_rounds = 0;
                } else {
                    no_progress_rounds += 1;
                    if no_progress_rounds >= MAX_NO_PROGRESS_ROUNDS {
                        final_content = Some(
                            "I cannot make progress with current tool responses. \
                             Please provide more details or relax constraints."
                                .into(),
                        );
                        break;
                    }
                }

                // === ask_user 暂停 ===
                if let Some((pending_id, question)) = round_outcome.ask_user_pending {
                    info!(
                        tool_call_id = %pending_id,
                        question = %question,
                        "Agent loop pausing for ask_user"
                    );
                    final_content = Some(question);
                    status = RuntimeStatus::NeedsInput {
                        pending_tool_call_id: pending_id,
                    };
                    break;
                }

                // === 阶段检测与管理 ===
                let has_production = tools_used
                    .iter()
                    .rev()
                    .take(response.tool_calls.len())
                    .any(|name| is_production_tool(name));

                if in_exploration {
                    let after = phase::explore_phase_after_tools(
                        exploration_iterations,
                        explore_max,
                        has_production,
                    );
                    if after.transition_to_produce {
                        in_exploration = false;
                        info!(
                            exploration_iterations,
                            "Phase transition: explore → produce (production tool detected)"
                        );
                        if let Some(cb) = on_progress {
                            cb("分析完成，正在生成结果...").await;
                        }
                    }
                    for m in after.nudge_messages {
                        messages.push(m);
                    }
                }

                // Sub-agent 空转检测：产出阶段连续 exec 没有 write/edit
                if is_sub_agent && !in_exploration {
                    let this_round: Vec<&str> = tools_used
                        .iter()
                        .rev()
                        .take(response.tool_calls.len())
                        .map(|s| s.as_str())
                        .collect();
                    let has_write_or_edit = this_round
                        .iter()
                        .any(|t| *t == "write_file" || *t == "edit_file");
                    let has_exec = this_round.iter().any(|t| *t == "exec");

                    if has_write_or_edit {
                        consecutive_exec_without_write = 0;
                    } else if has_exec {
                        consecutive_exec_without_write += 1;
                    }

                    if consecutive_exec_without_write >= 3 {
                        warn!(
                            consecutive_exec = consecutive_exec_without_write,
                            "Sub-agent idle spin: consecutive exec without write/edit — nudging"
                        );
                        messages.push(chat_message("system", &crate::nudge_loader::idle_spin()));
                        consecutive_exec_without_write = 0;
                    }
                }
            } else {
                // 最终响应：输出 reasoning（thinking 过程）
                if let Some(cb) = on_progress {
                    if let Some(ref reasoning) = response.reasoning_content {
                        let parsed = parse_reasoning(reasoning);
                        let display = if parsed.display_text.len() > 800 {
                            let boundary = parsed.display_text.floor_char_boundary(800);
                            format!(
                                "{}... ({} chars total)",
                                &parsed.display_text[..boundary],
                                parsed.raw_length
                            )
                        } else {
                            parsed.display_text.clone()
                        };
                        if !display.is_empty() {
                            cb(&format!("[Thinking]\n{}", display)).await;
                        }
                    }
                }

                let clean = strip_think(response.content.as_deref());

                if response.finish_reason == "error" {
                    warn!(error = ?clean, "LLM error");
                    final_content =
                        Some(clean.unwrap_or_else(|| "Sorry, I encountered an error.".into()));
                    break;
                }

                // 检测上一轮是否有工具参数验证错误（如 dispatch_subtasks 参数格式错误）。
                // 如果有，LLM 可能说了"我要继续"但没发 tool_calls——不要退出循环，
                // 注入提醒让它重新调用正确的工具。
                let last_tool_had_param_error = messages
                    .iter()
                    .rev()
                    .take(10) // 只看最近几条
                    .any(|m| {
                        m.get("role").and_then(|v| v.as_str()) == Some("tool")
                            && m.get("content")
                                .and_then(|v| v.as_str())
                                .map_or(false, |c| {
                                    c.contains("Invalid parameters")
                                        || c.contains("Missing required parameter")
                                })
                    });

                if last_tool_had_param_error
                    && total_iterations < self.max_iterations.saturating_sub(2)
                {
                    warn!("LLM stopped without tool_calls after parameter validation error — nudging to retry");
                    ContextBuilder::add_assistant_message(
                        &mut messages,
                        clean.as_deref(),
                        None,
                        response.reasoning_content.as_deref(),
                    );
                    messages.push(chat_message(
                        "system",
                        &crate::nudge_loader::param_error_retry(),
                    ));
                    continue;
                }

                if let Some(obs) = pending_dispatch_observation.take() {
                    let final_preview = clean
                        .as_deref()
                        .map(build_decision_preview)
                        .unwrap_or_else(|| "finalized without textual content".into());
                    record_dispatch_followup_decision(
                        &mut decision_events,
                        total_iterations,
                        is_sub_agent,
                        &phase,
                        obs,
                        "finalize",
                        format!("assistant stopped with final response: {final_preview}"),
                        Vec::new(),
                    );
                }

                ContextBuilder::add_assistant_message(
                    &mut messages,
                    clean.as_deref(),
                    None,
                    response.reasoning_content.as_deref(),
                );
                if let Some(ref answer) = clean {
                    ctx_state.add_fact(&format!("Assistant produced final response: {}", answer));
                }
                // LLM 返回 stop 且无 tool_calls：正常结束。
                // 如果 content 为空（某些模型偶发），用工具记录构建兜底总结。
                final_content = Some(clean.unwrap_or_else(|| {
                    if tools_used.is_empty() {
                        "处理完成，未生成回复内容。".to_string()
                    } else {
                        format!("Task completed. Tools used: {}", tools_used.join(", "))
                    }
                }));
                break;
            }
        }

        if final_content.is_none() {
            warn!(
                total_iterations,
                exploration_iterations,
                production_iterations,
                max = self.max_iterations,
                "Max iterations reached"
            );
            // 构建进度摘要：从 tools_used 和最近消息中提取关键信息，
            // 让调用方（主 LLM）知道 sub-agent 到底做了什么、哪些完成了、哪些还有问题。
            let max_msg = build_max_iterations_summary(
                exploration_iterations,
                production_iterations,
                &tools_used,
                &messages,
            );
            // 写入一次性重置标记：仅允许下一次 run 从 0 开始计数。
            ContextBuilder::add_assistant_message(
                &mut messages,
                Some(&format!("{max_msg}\n{MAX_REACHED_RESET_MARKER}")),
                None,
                None,
            );
            final_content = Some(max_msg);
        }

        // 给本轮新增的消息打上 _turn_id（用于 save_turn 精确筛选）。
        // 来自 history 的消息已有 timestamp 字段，不会被误标。
        // system 消息（nudge 等）不打标——不需要持久化到 history。
        for msg in messages.iter_mut() {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            if role == "system" {
                continue;
            }
            if msg.contains_key("timestamp") || msg.contains_key("_turn_id") {
                continue;
            }
            msg.insert("_turn_id".into(), json!(turn_id));
        }

        let diagnostics_summary = build_run_diagnostics_summary(&tool_events);
        Ok(RuntimeResult {
            content: final_content,
            tools_used,
            messages,
            status,
            tool_events,
            decision_events,
            diagnostics_summary,
            cache_hit_tokens: total_cache_hit,
            cache_write_tokens: total_cache_write,
            total_prompt_tokens,
            total_completion_tokens,
            turn_id,
        })
    }
}

fn observe_dispatch_result(
    iteration: usize,
    summary: &DispatchStructuredSummary,
    result: &str,
) -> Option<PendingDispatchObservation> {
    let statuses = extract_dispatch_statuses(summary);
    if statuses.is_empty() {
        return None;
    }
    Some(PendingDispatchObservation {
        origin_iteration: iteration,
        statuses,
        summary_preview: build_dispatch_summary_preview(summary, result),
    })
}

fn extract_dispatch_statuses(summary: &DispatchStructuredSummary) -> Vec<String> {
    let mut statuses = Vec::new();
    for node in &summary.nodes {
        let status = if node.declared_result_status != "unknown" {
            node.declared_result_status.as_str()
        } else {
            node.node_status.as_str()
        };
        if !status.is_empty() && !statuses.iter().any(|s| s == status) {
            statuses.push(status.to_string());
        }
    }
    statuses
}

fn build_dispatch_summary_preview(
    summary: &DispatchStructuredSummary,
    fallback_result: &str,
) -> String {
    if summary.nodes.is_empty() {
        return build_decision_preview(fallback_result);
    }

    let mut parts = Vec::new();
    parts.push(format!(
        "plan={} ok={} failed={} skipped={} conflicts={} wall={}ms",
        summary.plan_id,
        summary.succeeded,
        summary.failed,
        summary.skipped,
        summary.has_conflicts,
        summary.wall_time_ms
    ));
    let node_preview = summary
        .nodes
        .iter()
        .take(3)
        .map(|node| {
            format!(
                "{}:{}:{} verified={:?} unverified={} tools={} path={}",
                node.node_id,
                node.node_status,
                node.declared_result_status,
                node.verified_after_last_edit,
                node.ended_with_unverified_changes,
                node.tools_used_count,
                node.detail_path
            )
        })
        .collect::<Vec<_>>()
        .join(" | ");
    parts.push(node_preview);
    build_decision_preview(&parts.join(" ; "))
}

fn classify_post_dispatch_tool_decision(next_tools: &[String]) -> (&'static str, String) {
    let tool_list = if next_tools.is_empty() {
        "(none)".to_string()
    } else {
        next_tools.join(",")
    };
    if next_tools.iter().any(|name| name == "dispatch_subtasks") {
        (
            "redispatch",
            format!("requested another dispatch_subtasks round ({tool_list})"),
        )
    } else if next_tools
        .iter()
        .all(|name| is_dispatch_inspection_tool(name))
    {
        (
            "inspect_subtask_output",
            format!("inspecting subtask artifacts with tools: {tool_list}"),
        )
    } else if next_tools.iter().any(|name| is_takeover_tool(name)) {
        (
            "take_over_execution",
            format!("main agent resumed execution with tools: {tool_list}"),
        )
    } else {
        (
            "continue_with_tools",
            format!("continued with post-dispatch tools: {tool_list}"),
        )
    }
}

fn is_dispatch_inspection_tool(tool_name: &str) -> bool {
    matches!(tool_name, "read_file" | "glob" | "list_dir" | "grep_search")
}

fn is_takeover_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "exec"
            | "write_file"
            | "edit_file"
            | "apply_patch"
            | "delete_file"
            | "move_file"
            | "copy_file"
            | "mkdir"
            | "send_file"
    )
}

fn build_decision_preview(text: &str) -> String {
    let single_line = text.replace('\n', " ");
    let trimmed = single_line.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let max_chars = 160;
    if trimmed.len() <= max_chars {
        trimmed.to_string()
    } else {
        let boundary = trimmed.floor_char_boundary(max_chars);
        format!("{}...", &trimmed[..boundary])
    }
}

fn record_dispatch_followup_decision(
    decision_events: &mut Vec<DecisionEvent>,
    iteration: usize,
    is_sub_agent: bool,
    phase: &str,
    observation: PendingDispatchObservation,
    decision: &str,
    reason: String,
    next_tools: Vec<String>,
) {
    let event = DecisionEvent {
        iteration,
        agent_scope: if is_sub_agent {
            "sub".into()
        } else {
            "main".into()
        },
        decision: decision.into(),
        reason,
        dispatch_origin_iteration: Some(observation.origin_iteration),
        dispatch_statuses: observation.statuses.clone(),
        next_tools,
    };
    info!(
        iteration,
        phase = %phase,
        agent_scope = %event.agent_scope,
        decision = %event.decision,
        dispatch_origin_iteration = observation.origin_iteration,
        dispatch_statuses = ?event.dispatch_statuses,
        next_tools = ?event.next_tools,
        reason = %event.reason,
        dispatch_summary_preview = %observation.summary_preview,
        "Post-dispatch decision"
    );
    decision_events.push(event);
}

fn build_run_diagnostics_summary(tool_events: &[ToolExecutionEvent]) -> RunDiagnosticsSummary {
    let total_tool_calls = tool_events.len();
    let exec_count = tool_events.iter().filter(|e| e.tool_name == "exec").count();
    let dedicated_tool_count = tool_events.iter().filter(|e| e.tool_name != "exec").count();
    let error_tool_count = tool_events
        .iter()
        .filter(|e| {
            matches!(
                e.status.as_str(),
                "error" | "invalid_params" | "unknown_tool"
            )
        })
        .count();
    let denied_tool_count = tool_events
        .iter()
        .filter(|e| matches!(e.status.as_str(), "denied" | "blocked"))
        .count();
    let sandbox_tool_count = tool_events.iter().filter(|e| e.route == "sandbox").count();
    let host_tool_count = tool_events.iter().filter(|e| e.route == "host").count();

    let last_edit_index = tool_events
        .iter()
        .rposition(|e| is_mutating_tool(&e.tool_name));
    let verified_after_last_edit = last_edit_index.map(|idx| {
        tool_events
            .iter()
            .skip(idx + 1)
            .any(|e| is_verification_tool(&e.tool_name) && e.status == "ok")
    });
    let ended_with_unverified_changes = matches!(verified_after_last_edit, Some(false));

    RunDiagnosticsSummary {
        total_tool_calls,
        exec_count,
        dedicated_tool_count,
        error_tool_count,
        denied_tool_count,
        sandbox_tool_count,
        host_tool_count,
        verified_after_last_edit,
        ended_with_unverified_changes,
    }
}

fn is_mutating_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "write_file"
            | "edit_file"
            | "apply_patch"
            | "delete_file"
            | "move_file"
            | "copy_file"
            | "mkdir"
    )
}

fn is_verification_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "read_file" | "list_dir" | "grep_search" | "glob" | "exec"
    )
}

/// 当 agent loop 因 max_iterations 退出时，构建进度摘要而不是泛泛的 "try smaller steps"。
///
/// 从 tools_used 和最近消息中提取：
/// - 写了哪些文件
/// - 最后几轮在做什么（最近的 assistant content）
/// - 是否成功运行过
fn build_max_iterations_summary(
    exploration_iterations: usize,
    production_iterations: usize,
    tools_used: &[String],
    messages: &[HashMap<String, Value>],
) -> String {
    let mut summary = format!(
        "Reached max iterations (explore={exploration_iterations}, produce={production_iterations}).\n\n"
    );

    // 提取写过的文件
    let write_count = tools_used
        .iter()
        .filter(|t| *t == "write_file" || *t == "edit_file")
        .count();
    let exec_count = tools_used.iter().filter(|t| *t == "exec").count();
    if write_count > 0 || exec_count > 0 {
        summary.push_str(&format!(
            "Progress: {} file writes, {} exec calls.\n",
            write_count, exec_count
        ));
    }

    // 从消息历史中提取写过的文件路径
    let mut written_files: Vec<String> = Vec::new();
    for m in messages {
        if m.get("role").and_then(|v| v.as_str()) == Some("tool") {
            if let Some(content) = m.get("content").and_then(|v| v.as_str()) {
                // 匹配 "Successfully wrote ... to /path" 或 "Successfully edited /path"
                if content.contains("Successfully wrote") || content.contains("Successfully edited")
                {
                    // 提取路径：取最后一个 "/" 后的部分作为文件名
                    if let Some(path_start) = content.rfind('/') {
                        let filename = &content[path_start + 1..];
                        // 排除 tmp/ 下的辅助脚本
                        if !filename.contains("investigate")
                            && !filename.contains("compare")
                            && !filename.contains("verify")
                            && !filename.contains("check")
                            && !filename.contains("explore")
                        {
                            let clean = filename.trim();
                            if !clean.is_empty() && !written_files.contains(&clean.to_string()) {
                                written_files.push(clean.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    if !written_files.is_empty() {
        summary.push_str(&format!("Files created: {}\n", written_files.join(", ")));
    }

    // 提取最后 3 条 assistant content 作为"最近在做什么"
    let mut recent_actions: Vec<String> = Vec::new();
    for m in messages.iter().rev() {
        if recent_actions.len() >= 3 {
            break;
        }
        if m.get("role").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        if let Some(content) = m.get("content").and_then(|v| v.as_str()) {
            let trimmed = content.trim();
            if !trimmed.is_empty() && trimmed.len() > 10 {
                // 取第一行或前 150 字符
                let first_line = trimmed.lines().next().unwrap_or(trimmed);
                let preview = if first_line.len() > 150 {
                    format!("{}...", &first_line[..first_line.floor_char_boundary(150)])
                } else {
                    first_line.to_string()
                };
                recent_actions.push(preview);
            }
        }
    }
    if !recent_actions.is_empty() {
        recent_actions.reverse();
        summary.push_str("\nLast actions before timeout:\n");
        for action in &recent_actions {
            summary.push_str(&format!("- {action}\n"));
        }
    }

    // 提取最后一个 exec 的输出摘要（成功或失败）
    for m in messages.iter().rev() {
        if m.get("role").and_then(|v| v.as_str()) != Some("tool") {
            continue;
        }
        if let Some(content) = m.get("content").and_then(|v| v.as_str()) {
            if content.contains("Error") || content.contains("Traceback") {
                let preview = if content.len() > 200 {
                    format!("{}...", &content[..content.floor_char_boundary(200)])
                } else {
                    content.to_string()
                };
                summary.push_str(&format!("\nLast error: {preview}\n"));
                break;
            } else if content.len() > 20 {
                // 最后一个有意义的工具输出
                let preview = if content.len() > 200 {
                    format!("{}...", &content[..content.floor_char_boundary(200)])
                } else {
                    content.to_string()
                };
                summary.push_str(&format!("\nLast tool output: {preview}\n"));
                break;
            }
        }
    }

    summary
}

#[cfg(test)]
mod tests {
    use crate::agent::tool_runner::{DispatchNodeSummary, DispatchStructuredSummary};

    use super::{
        build_run_diagnostics_summary, classify_post_dispatch_tool_decision,
        extract_dispatch_statuses, RunDiagnosticsSummary, ToolExecutionEvent,
    };

    fn event(tool_name: &str, route: &str, status: &str) -> ToolExecutionEvent {
        ToolExecutionEvent {
            tool_id: format!("id_{tool_name}"),
            tool_name: tool_name.into(),
            route: route.into(),
            status: status.into(),
            duration_ms: 10,
            original_len: 20,
            result_len: 20,
            truncated: false,
            gate_action: "allow".into(),
            risk_level: "read".into(),
            sandbox_id: None,
            result_preview: "ok".into(),
        }
    }

    #[test]
    fn diagnostics_marks_verified_after_last_edit() {
        let summary = build_run_diagnostics_summary(&[
            event("read_file", "host", "ok"),
            event("edit_file", "sandbox", "ok"),
            event("read_file", "sandbox", "ok"),
        ]);
        assert_eq!(summary.verified_after_last_edit, Some(true));
        assert!(!summary.ended_with_unverified_changes);
        assert_eq!(summary.sandbox_tool_count, 2);
    }

    #[test]
    fn diagnostics_marks_unverified_tail_edit() {
        let summary: RunDiagnosticsSummary = build_run_diagnostics_summary(&[
            event("edit_file", "sandbox", "ok"),
            event("write_file", "sandbox", "ok"),
        ]);
        assert_eq!(summary.verified_after_last_edit, Some(false));
        assert!(summary.ended_with_unverified_changes);
        assert_eq!(summary.host_tool_count, 0);
    }

    #[test]
    fn extract_dispatch_statuses_collects_unique_statuses() {
        let statuses = extract_dispatch_statuses(&DispatchStructuredSummary {
            plan_id: "plan-1".into(),
            succeeded: 1,
            failed: 1,
            skipped: 0,
            has_conflicts: false,
            wall_time_ms: 1200,
            nodes: vec![
                DispatchNodeSummary {
                    node_id: "a".into(),
                    node_status: "failed".into(),
                    declared_result_status: "blocked".into(),
                    verified_after_last_edit: Some(false),
                    ended_with_unverified_changes: true,
                    tools_used_count: 2,
                    detail_path: "/tmp/a.md".into(),
                },
                DispatchNodeSummary {
                    node_id: "b".into(),
                    node_status: "success".into(),
                    declared_result_status: "verified".into(),
                    verified_after_last_edit: Some(true),
                    ended_with_unverified_changes: false,
                    tools_used_count: 1,
                    detail_path: "/tmp/b.md".into(),
                },
                DispatchNodeSummary {
                    node_id: "c".into(),
                    node_status: "failed".into(),
                    declared_result_status: "blocked".into(),
                    verified_after_last_edit: None,
                    ended_with_unverified_changes: false,
                    tools_used_count: 0,
                    detail_path: "/tmp/c.md".into(),
                },
            ],
        });
        assert_eq!(
            statuses,
            vec!["blocked".to_string(), "verified".to_string()]
        );
    }

    #[test]
    fn classify_post_dispatch_takeover_and_inspect() {
        let (inspect, _) =
            classify_post_dispatch_tool_decision(&["read_file".to_string(), "glob".to_string()]);
        assert_eq!(inspect, "inspect_subtask_output");

        let (takeover, _) = classify_post_dispatch_tool_decision(&[
            "read_file".to_string(),
            "edit_file".to_string(),
        ]);
        assert_eq!(takeover, "take_over_execution");
    }
}
