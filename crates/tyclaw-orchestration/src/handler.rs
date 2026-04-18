//! 请求处理器 —— 14 步端到端消息处理流程。

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tracing::{debug, info, warn, Level};

use tyclaw_agent::runtime::OnProgress;
use tyclaw_agent::{RuntimeResult, RuntimeStatus};
use tyclaw_control::AuditEntry;
use tyclaw_memory::{extract_case, CaseRetriever};
use tyclaw_prompt::{strip_non_task_user_message, PromptInputs, SkillContent};
use tyclaw_types::TyclawError;

use crate::helpers;
use crate::history;
use crate::memory_filter::filter_memory_by_relevance;
use crate::orchestrator::{ActiveTask, Orchestrator};
use crate::types::{
    AgentResponse, RequestContext, FIRST_TURN_CASES_CHARS_BONUS, FIRST_TURN_SKILL_BONUS,
    MAX_DYNAMIC_INJECTED_SKILLS, MAX_DYNAMIC_SIMILAR_CASES_CHARS, MAX_HISTORY_TOKENS_HARD_LIMIT,
    MIN_HISTORY_BUDGET_TOKENS, RESET_ON_START_FIELD, TOOL_RESULT_MAX_CHARS,
};

/// 请求处理中间状态，在各阶段之间传递。
struct HandlerContext<'a> {
    start: Instant,
    user_id: &'a str,
    user_name: &'a str,
    workspace_id: &'a str,
    channel: &'a str,
    chat_id: &'a str,
    workspace_key: String,
    on_progress: Option<&'a OnProgress>,
}

impl Orchestrator {
    /// 端到端处理用户消息。
    ///
    /// 参数：
    /// - `user_message`: 用户发送的原始文本
    /// - `user_id`: 用户 ID（staff_id）
    /// - `workspace_id`: 工作区 ID
    /// - `channel`: 消息来源通道（cli / dingtalk_private / dingtalk_group）
    /// - `chat_id`: 对话 ID（CLI 为 "direct"，群聊为 conversation_id）
    /// - `on_progress`: 可选的进度回调（用于流式输出中间思考过程）
    pub async fn handle(
        &self,
        user_message: &str,
        user_id: &str,
        workspace_id: &str,
        channel: &str,
        chat_id: &str,
        on_progress: Option<&OnProgress>,
    ) -> Result<AgentResponse, TyclawError> {
        let req = RequestContext::new(user_id, workspace_id, channel, chat_id);
        self.handle_with_context(user_message, &req, on_progress)
            .await
    }

    /// 端到端处理用户消息（RequestContext 版本，SDK 场景推荐）。
    pub async fn handle_with_context(
        &self,
        user_message: &str,
        req: &RequestContext,
        on_progress: Option<&OnProgress>,
    ) -> Result<AgentResponse, TyclawError> {
        let start = Instant::now();
        let user_id = req.user_id.as_str();
        let user_name = req.user_name.as_str();
        let workspace_id = req.workspace_id.as_str();
        let channel = req.channel.as_str();
        let chat_id = req.chat_id.as_str();

        // 通过策略解析 workspace_key
        let identity = tyclaw_control::RequestIdentity {
            user_id,
            channel,
            chat_id,
            conversation_id: req.conversation_id.as_deref(),
        };
        let workspace_key = self.persistence.workspace_mgr.resolve_key(&identity);

        let ctx = HandlerContext {
            start,
            user_id,
            user_name,
            workspace_id,
            channel,
            chat_id,
            workspace_key: workspace_key.clone(),
            on_progress,
        };

        // 如果 workspace 正在处理中，将消息注入到运行中的 agent loop，立即返回
        if self.persistence.sessions.busy_elapsed(&workspace_key).is_some() {
            return self.handle_busy_workspace(&ctx, user_message, req).await;
        }

        // 确保 workspace 目录结构存在
        self.persistence.workspace_mgr.ensure_workspace(&workspace_key);

        // 标记为忙碌，防止 reaper 在处理期间回收（guard drop 时自动 clear）
        self.persistence.sessions.get_or_create_clone(&workspace_key);
        let _busy_guard = self.persistence.sessions.busy_guard(&workspace_key);

        let pending_entry = self
            .pending_ask_user
            .lock()
            .remove(&workspace_key);
        if let Some((tool_call_id, mut saved_messages)) = pending_entry {
            // 用户回车没输入内容 → 使用默认行为（让 agent 自行决定）
            let reply = if user_message.trim().is_empty() {
                "用户未回复，请根据已有信息自行判断，选择最合理的方案继续执行。".to_string()
            } else {
                format!("User replied: {user_message}")
            };
            info!(
                tool_call_id = %tool_call_id,
                user_reply = %reply,
                "Resuming agent loop after ask_user"
            );
            tyclaw_prompt::ContextBuilder::add_tool_result(
                &mut saved_messages,
                &tool_call_id,
                "ask_user",
                &reply,
            );

            let user_role = if self.app.features.enable_rbac {
                self.persistence.workspace_mgr.get_user_role(workspace_id, user_id)
            } else {
                "admin".to_string()
            };

            let msg_count_before = saved_messages.len();

            // 恢复 agent loop
            let cache_scope = format!("session:{workspace_key}");
            let result: RuntimeResult = self
                .runtime
                .run(saved_messages, &user_role, Some(&cache_scope), on_progress)
                .await?;

            // 检查是否又暂停了
            if let RuntimeStatus::NeedsInput {
                pending_tool_call_id,
            } = &result.status
            {
                let question = result
                    .content
                    .clone()
                    .unwrap_or_else(|| "I need your input.".into());
                info!(
                    tool_call_id = %pending_tool_call_id,
                    "Agent paused again (ask_user) after resume"
                );
                self.pending_ask_user
                    .lock()
                    .insert(
                        workspace_key.clone(),
                        (pending_tool_call_id.clone(), result.messages),
                    );
                return Ok(AgentResponse {
                    text: question,
                    tools_used: result.tools_used,
                    duration_seconds: start.elapsed().as_secs_f64(),
                    prompt_tokens: result.total_prompt_tokens,
                    completion_tokens: result.total_completion_tokens,
                    output_files: Vec::new(),
                });
            }

            let final_content = helpers::strip_internal_markers(
                &result
                    .content
                    .unwrap_or_else(|| "处理完成，未生成回复内容。".into()),
            );
            let tools_used = result.tools_used;
            let duration = start.elapsed().as_secs_f64();

            // 保存恢复后的新消息（跳过之前的部分）
            if !result.messages.is_empty() && result.messages.len() > msg_count_before {
                let new_msgs: Vec<_> = result.messages[msg_count_before..].to_vec();
                let mut session = self.persistence.sessions.get_or_create_clone(&workspace_key);
                for m in &new_msgs {
                    session.messages.push(m.clone());
                }
                session.updated_at = chrono::Utc::now();
                self.persistence.sessions.save(&session).ok();
            }

            let output_files = Vec::new(); // ask_user 恢复路径不产出文件

            return Ok(AgentResponse {
                text: final_content,
                tools_used,
                duration_seconds: duration,
                prompt_tokens: result.total_prompt_tokens,
                completion_tokens: result.total_completion_tokens,
                output_files,
            });
        }

        // 1. 速率限制检查
        if self.app.features.enable_rate_limit {
            self.persistence.rate_limiter
                .check(user_id)
                .map_err(TyclawError::RateLimitExceeded)?;
        }

        // 2. 获取用户角色
        let user_role = if self.app.features.enable_rbac {
            self.persistence.workspace_mgr.get_user_role(workspace_id, user_id)
        } else {
            "admin".to_string()
        };

        // 4. 处理斜杠命令
        if let Some(resp) = self.handle_slash_command(&ctx, user_message).await? {
            return Ok(resp);
        }

        // 4.5 如果有文件附件，复制到 workspace work/attachments 并追加到消息
        let user_message = self.process_attachments(&ctx, user_message, req);
        let user_message = user_message.as_str();

        // 5-8. 准备 prompt
        let initial_messages = self.prepare_prompt(&ctx, user_message, req).await;

        // 8.5-9.5. 运行 agent
        let run_result = self.run_agent(&ctx, initial_messages, &user_role, user_message).await?;
        match run_result {
            AgentRunResult::EarlyReturn(resp) => Ok(resp),
            AgentRunResult::Completed(completed) => {
                // 10-14. 后处理
                self.post_process(&ctx, user_message, completed).await
            }
        }
    }

    /// Workspace 忙碌时，将消息注入到运行中的 agent loop 并立即返回。
    async fn handle_busy_workspace(
        &self,
        ctx: &HandlerContext<'_>,
        user_message: &str,
        req: &RequestContext,
    ) -> Result<AgentResponse, TyclawError> {
        info!(
            workspace_key = %ctx.workspace_key,
            "Workspace busy, injecting message into running agent loop"
        );

        // 复制附件到 workspace 并组装完整消息（与正常流程一致）
        let mut msg = user_message.to_string();
        if !req.file_attachments.is_empty() {
            self.persistence.workspace_mgr.ensure_workspace(&ctx.workspace_key);
            let attachments_dir = self.persistence.workspace_mgr.attachments_dir(&ctx.workspace_key);
            msg.push_str("\n\n[附件文件]");
            for (path, name) in &req.file_attachments {
                let dest = attachments_dir.join(name);
                let _ = std::fs::create_dir_all(&attachments_dir);
                let display_path = format!("{}/{name}", self.persistence.workspace_mgr.path_config().attachments_dir);
                if path != &dest {
                    let _ = std::fs::copy(path, &dest);
                }
                msg.push_str(&format!("\n- {name} (路径: {display_path})"));
            }
        }

        let queue = self.get_injection_queue(&ctx.workspace_key);
        if let Ok(mut pending) = queue.lock() {
            pending.push(tyclaw_agent::runtime::chat_message("user", &msg));
        }

        // 审计记录：注入消息也需要留痕
        if self.app.features.enable_audit {
            let session_id = self.persistence.sessions.get_session_id(&ctx.workspace_key)
                .unwrap_or_else(|| "unknown".into());
            let _ = self.persistence.audit.log(&AuditEntry {
                timestamp: chrono::Utc::now(),
                workspace_key: ctx.workspace_key.clone(),
                session_id,
                user_id: ctx.user_id.into(),
                user_name: ctx.user_name.into(),
                channel: ctx.channel.into(),
                request: format!("[injected] {}", msg.chars().take(500).collect::<String>()),
                tool_calls: vec![],
                skills_used: vec![],
                final_response: Some("injected into running agent loop".into()),
                total_duration: Some(ctx.start.elapsed().as_secs_f64()),
                token_usage: None,
            });
        }

        Ok(AgentResponse {
            text: String::new(),
            tools_used: vec![],
            duration_seconds: ctx.start.elapsed().as_secs_f64(),
            prompt_tokens: 0,
            completion_tokens: 0,
            output_files: Vec::new(),
        })
    }

    /// 处理斜杠命令（/save, /handoff, /new）。返回 Some 表示已处理，None 表示需继续正常流程。
    async fn handle_slash_command(
        &self,
        ctx: &HandlerContext<'_>,
        user_message: &str,
    ) -> Result<Option<AgentResponse>, TyclawError> {
        let cmd = user_message.trim().to_lowercase();

        if cmd == "/save" || cmd == "/handoff" {
            let session = self.persistence.sessions.get_or_create_clone(&ctx.workspace_key);
            let messages = session.messages.clone();
            if messages.is_empty() {
                return Ok(Some(AgentResponse {
                    text: "当前会话暂无可保存内容。".into(),
                    tools_used: Vec::new(),
                    duration_seconds: ctx.start.elapsed().as_secs_f64(),
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    output_files: Vec::new(),
                }));
            }

            let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
            let safe_key = ctx.workspace_key.replace(':', "_");
            let handoff_dir = self.persistence.workspace_mgr.workspace_dir(&ctx.workspace_key);
            let handoff_path = handoff_dir.join(format!("handoff_{safe_key}_{ts}.md"));
            let handoff = helpers::build_handoff_markdown(&ctx.workspace_key, &messages);
            let _ = std::fs::create_dir_all(&handoff_dir);

            match std::fs::write(&handoff_path, handoff) {
                Ok(_) => {
                    let display = handoff_path
                        .strip_prefix(&self.app.workspace)
                        .ok()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| handoff_path.display().to_string());
                    return Ok(Some(AgentResponse {
                        text: format!(
                            "已保存当前会话信息到：`{display}`。\n\
                            现在可以输入 `/new` 开新任务，并把这个文件内容粘贴给我继续。"
                        ),
                        tools_used: Vec::new(),
                        duration_seconds: ctx.start.elapsed().as_secs_f64(),
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        output_files: Vec::new(),
                    }));
                }
                Err(e) => {
                    return Ok(Some(AgentResponse {
                        text: format!("保存会话失败：{e}"),
                        tools_used: Vec::new(),
                        duration_seconds: ctx.start.elapsed().as_secs_f64(),
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        output_files: Vec::new(),
                    }));
                }
            }
        }

        if cmd == "/new" {
            let session = self.persistence.sessions.get_or_create_clone(&ctx.workspace_key);
            let messages = session.messages.clone();
            let last_consolidated = session.last_consolidated;
            if self.app.features.enable_memory {
                let mem_dir = self.persistence.workspace_mgr.memory_dir(&ctx.workspace_key);
                let consolidator = tyclaw_memory::MemoryConsolidator::new(&mem_dir, self.app.context_window_tokens);
                consolidator
                    .archive_unconsolidated(
                        &messages,
                        last_consolidated,
                        self.provider.as_ref(),
                        &self.app.model,
                    )
                    .await;
            }
            let mut session = self.persistence.sessions.get_or_create_clone(&ctx.workspace_key);
            session.clear();
            self.persistence.sessions.save(&session).ok();
            self.persistence.sessions.invalidate(&ctx.workspace_key);
            return Ok(Some(AgentResponse {
                text: "New session started.".into(),
                tools_used: Vec::new(),
                duration_seconds: ctx.start.elapsed().as_secs_f64(),
                prompt_tokens: 0,
                completion_tokens: 0,
                output_files: Vec::new(),
            }));
        }

        Ok(None)
    }

    /// 处理文件附件：复制到 workspace 并追加到消息文本。
    fn process_attachments(
        &self,
        ctx: &HandlerContext<'_>,
        user_message: &str,
        req: &RequestContext,
    ) -> String {
        let user_attachments_dir = self.persistence.workspace_mgr.attachments_dir(&ctx.workspace_key);
        if req.file_attachments.is_empty() {
            return user_message.to_string();
        }

        let mut msg = user_message.to_string();
        msg.push_str("\n\n[附件文件]");
        for (path, name) in &req.file_attachments {
            // 将文件复制到用户 work/attachments，使容器内 /user/work/attachments/{name} 可访问
            let dest = user_attachments_dir.join(name);
            if let Err(e) = std::fs::create_dir_all(&user_attachments_dir) {
                warn!(error = %e, "Failed to create attachments dir for file copy");
            }
            let display_path = format!("{}/{name}", self.persistence.workspace_mgr.path_config().attachments_dir);
            if path == &dest {
                msg.push_str(&format!("\n- {name} (路径: {display_path})"));
                info!(path = %dest.display(), "Attachment already staged in user attachments dir");
            } else {
                match std::fs::copy(path, &dest) {
                    Ok(_) => {
                        msg.push_str(&format!("\n- {name} (路径: {display_path})"));
                        info!(src = %path, dest = %dest.display(), "Copied attachment to user attachments dir");
                    }
                    Err(e) => {
                        warn!(error = %e, src = %path, dest = %dest.display(), "Failed to copy attachment to user attachments dir, using original path");
                        msg.push_str(&format!("\n- {name} (路径: {path})"));
                    }
                }
            }
        }
        info!(
            file_count = req.file_attachments.len(),
            "Appended file attachments to user message"
        );
        msg
    }

    /// Steps 5-8: 收集技能、检索案例、构建历史、过滤记忆、组装 prompt。
    async fn prepare_prompt(
        &self,
        ctx: &HandlerContext<'_>,
        user_message: &str,
        req: &RequestContext,
    ) -> Vec<HashMap<String, Value>> {
        let _workspace = self.persistence.workspace_mgr.get_workspace(ctx.workspace_id);
        let mut budget_plan = helpers::compute_context_budget_plan(user_message);
        let is_first_turn = {
            let session = self.persistence.sessions.get_or_create_clone(&ctx.workspace_key);
            session.get_history(0).is_empty()
        };
        if is_first_turn {
            budget_plan.max_skills = (budget_plan.max_skills + FIRST_TURN_SKILL_BONUS)
                .clamp(3, MAX_DYNAMIC_INJECTED_SKILLS);
            budget_plan.max_cases_chars = (budget_plan.max_cases_chars
                + FIRST_TURN_CASES_CHARS_BONUS)
                .clamp(800, MAX_DYNAMIC_SIMILAR_CASES_CHARS);
        }
        debug!(
            is_first_turn,
            history_ratio = budget_plan.history_ratio,
            max_skills = budget_plan.max_skills,
            max_cases_chars = budget_plan.max_cases_chars,
            "Computed dynamic context budget plan"
        );

        // 6. 收集技能和能力
        let caps = self.persistence.skills.get_caps(&ctx.workspace_key);
        let all_skill_metas = self.persistence.skills.get_skill_contents(&ctx.workspace_key);
        info!(
            total_skills = all_skill_metas.len(),
            "Loaded all skills for context injection"
        );
        let skill_contents: Vec<SkillContent> = all_skill_metas
            .iter()
            .map(|s| SkillContent {
                name: s.name.clone(),
                description: s.description.clone(),
                content: s.content.clone(),
                category: s.category.clone(),
                triggers: s.triggers.clone(),
                tool_path: if self.sandbox_pool.is_some() {
                    let pcfg = self.persistence.workspace_mgr.path_config();
                    s.tool.as_ref().map(|tool| {
                        if tool.starts_with("tools/") || tool.starts_with("skills/") {
                            format!("{}/{}", pcfg.container_root, tool)
                        } else if s.status == "builtin" {
                            format!("{}/{}/{}/{}/{}", pcfg.container_root, pcfg.global_skills_mount, s.category, s.key, tool)
                        } else {
                            let ws_root = self.persistence.workspace_mgr.workspace_dir(&ctx.workspace_key);
                            if let Ok(rel) = s.skill_dir.strip_prefix(&ws_root) {
                                format!("{}/{}/{}", pcfg.container_root, rel.display(), tool)
                            } else {
                                format!("{}/{}/{}/{}", pcfg.container_root, pcfg.skills_dir, s.key, tool)
                            }
                        }
                    })
                } else {
                    s.tool_path()
                },
                risk_level: s.risk_level.clone(),
                requires_capabilities: s.requires_capabilities.clone(),
                matched: false,
            })
            .collect();
        let cap_maps: Vec<std::collections::HashMap<String, String>> = caps
            .iter()
            .map(|c| {
                let mut cap = std::collections::HashMap::new();
                cap.insert("key".into(), c.key.clone());
                cap.insert("name".into(), c.name.clone());
                cap.insert("description".into(), c.description.clone());
                cap.insert("category".into(), c.category.clone());
                cap.insert("status".into(), c.status.clone());
                if !c.tags.is_empty() {
                    cap.insert("tags".into(), c.tags.join(", "));
                }
                if let Some(creator) = &c.creator {
                    cap.insert("creator".into(), creator.clone());
                }
                cap
            })
            .collect();

        // 7. 检索案例
        let (pinned_cases, similar_cases) = if self.app.features.enable_memory {
            let retriever = CaseRetriever::new(&self.persistence.case_store);
            let ws_cases = self.persistence.workspace_mgr.workspace_cases_dir(&ctx.workspace_key);
            let (pinned, similar) =
                retriever.format_for_prompt_split(user_message, &ws_cases, 3);
            let similar = helpers::optimize_similar_cases(&similar, budget_plan.max_cases_chars);
            (pinned, similar)
        } else {
            (String::new(), String::new())
        };

        // 8. 构建消息（含历史）
        let history = {
            let session = self.persistence.sessions.get_or_create_clone(&ctx.workspace_key);
            let raw_history = session.get_history(0);
            let deduped = history::dedupe_history(&raw_history);
            let history_budget = std::cmp::max(
                MIN_HISTORY_BUDGET_TOKENS,
                (self.app.context_window_tokens * budget_plan.history_ratio) / 100,
            );
            let history_budget = std::cmp::min(history_budget, MAX_HISTORY_TOKENS_HARD_LIMIT);

            let trimmed = history::trim_history_by_token_budget(&deduped, history_budget);
            history::enforce_tool_call_pairing(&trimmed)
        };

        // 读取 workspace 的 memory 内容，并按相关性过滤段落
        let memory_content = {
            let mem_file = self.persistence.workspace_mgr.memory_dir(&ctx.workspace_key).join("MEMORY.md");
            let raw = std::fs::read_to_string(&mem_file).unwrap_or_default();
            if raw.is_empty() {
                raw
            } else {
                filter_memory_by_relevance(&raw, user_message)
            }
        };

        let prompt_inputs = PromptInputs {
            mode: tyclaw_prompt::PromptMode::Full,
            capabilities: if cap_maps.is_empty() {
                None
            } else {
                Some(&cap_maps)
            },
            skill_contents: if skill_contents.is_empty() {
                None
            } else {
                Some(&skill_contents)
            },
            pinned_cases: if pinned_cases.is_empty() {
                None
            } else {
                Some(&pinned_cases)
            },
            similar_cases: if similar_cases.is_empty() {
                None
            } else {
                Some(&similar_cases)
            },
            memory_content: if memory_content.is_empty() {
                None
            } else {
                Some(&memory_content)
            },
            channel: Some(ctx.channel),
            chat_id: Some(ctx.chat_id),
            user_id: Some(ctx.user_id),
            workspace_id: Some(ctx.workspace_id),
        };
        let planned_prompt = self.context.plan_prompt_context(&prompt_inputs);

        let mut initial_messages = if req.image_data_uris.is_empty() {
            self.context
                .assemble_messages(&planned_prompt, &history, user_message)
        } else {
            info!(
                image_count = req.image_data_uris.len(),
                "Building multimodal messages with images"
            );
            self.context.assemble_messages_multimodal(
                &planned_prompt,
                &history,
                user_message,
                &req.image_data_uris,
            )
        };
        // 除 ask_user 恢复外，每次新用户输入都强制重置轮次，避免历史轮次继承导致"无法继续"。
        let mut marker = HashMap::new();
        marker.insert("role".into(), Value::String("system".into()));
        marker.insert("content".into(), Value::String(String::new()));
        marker.insert(RESET_ON_START_FIELD.into(), Value::Bool(true));
        initial_messages.push(marker);
        info!(workspace_key = %ctx.workspace_key, "Injected reset marker for fresh user turn");
        if tracing::enabled!(Level::DEBUG) {
            debug!(
                target: "prompt.assembly",
                workspace_id = ctx.workspace_id,
                user_id = ctx.user_id,
                prompt = %serde_json::to_string(&initial_messages).unwrap_or_default(),
                "Assembled messages for LLM",
            );
        }

        initial_messages
    }

    /// Steps 8.5-9.5: 注册活跃任务、运行 agent loop、释放 sandbox、处理 ask_user。
    async fn run_agent(
        &self,
        ctx: &HandlerContext<'_>,
        initial_messages: Vec<HashMap<String, Value>>,
        user_role: &str,
        user_message: &str,
    ) -> Result<AgentRunResult, TyclawError> {
        // 8.5 注册活跃任务到文件
        let task_summary: String = user_message.chars().take(50).collect();
        {
            let mut tasks = self.active_tasks.lock();
            tasks.insert(
                ctx.workspace_key.clone(),
                ActiveTask {
                    user_id: ctx.user_id.to_string(),
                    summary: task_summary,
                    started_at: ctx.start,
                },
            );
        }

        // 9. 运行 Agent 执行引擎
        let request_id = self.pending_files.new_request();
        let channel_owned = ctx.channel.to_string();
        let chat_id_owned = ctx.chat_id.to_string();
        let user_id_owned = ctx.user_id.to_string();
        let conversation_id_owned = ctx.chat_id.to_string();

        // 9a. Per-workspace work root
        let user_workspace = self.persistence.workspace_mgr.work_dir(&ctx.workspace_key);
        std::fs::create_dir_all(&user_workspace).ok();

        let sandbox: Option<(
            std::sync::Arc<dyn tyclaw_sandbox::Sandbox>,
            std::path::PathBuf,
        )> = if let Some(pool) = &self.sandbox_pool {
            match pool.acquire(&user_workspace, &[]).await {
                Ok(sb) => {
                    info!(sandbox = %sb.id(), user = %ctx.user_id, "Acquired sandbox");
                    if let Some(cb) = ctx.on_progress {
                        cb(&format!(
                            "[sandbox] 获取容器 {} | 用户 {}",
                            sb.id(),
                            ctx.user_id
                        ))
                        .await;
                    }
                    Some((sb, user_workspace.clone()))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to acquire sandbox, falling back to host");
                    None
                }
            }
        } else {
            None
        };

        let cache_scope = format!("session:{}", ctx.workspace_key);
        let injection_queue = self.get_injection_queue(&ctx.workspace_key);

        let run_future = self.runtime.run(
            initial_messages,
            user_role,
            Some(&cache_scope),
            ctx.on_progress,
        );

        // task_local 注入
        let user_role_owned = user_role.to_string();
        let result: RuntimeResult = if let Some((ref sb, _)) = sandbox {
            let sb_clone = sb.clone();
            tyclaw_tools::CURRENT_USER_ROLE
                .scope(
                    user_role_owned,
                    tyclaw_tools::CURRENT_REQUEST_ID.scope(
                        request_id,
                        tyclaw_tools::timer::TIMER_CURRENT_CHANNEL.scope(
                            channel_owned,
                            tyclaw_tools::timer::TIMER_CURRENT_CHAT_ID.scope(
                                chat_id_owned,
                                tyclaw_tools::timer::TIMER_CURRENT_USER_ID.scope(
                                    user_id_owned,
                                    tyclaw_tools::timer::TIMER_CURRENT_CONVERSATION_ID.scope(
                                        conversation_id_owned,
                                        tyclaw_sandbox::CURRENT_SANDBOX.scope(
                            sb_clone,
                            tyclaw_agent::runtime::INJECTION_QUEUE
                                .scope(injection_queue.clone(), run_future),
                        ),
                                    ),
                                ),
                            ),
                        ),
                    ),
                )
                .await?
        } else {
            tyclaw_tools::CURRENT_USER_ROLE
                .scope(
                    user_role_owned,
                    tyclaw_tools::CURRENT_REQUEST_ID.scope(
                        request_id,
                        tyclaw_tools::timer::TIMER_CURRENT_CHANNEL.scope(
                            channel_owned,
                            tyclaw_tools::timer::TIMER_CURRENT_CHAT_ID.scope(
                                chat_id_owned,
                                tyclaw_tools::timer::TIMER_CURRENT_USER_ID.scope(
                                    user_id_owned,
                                    tyclaw_tools::timer::TIMER_CURRENT_CONVERSATION_ID
                                        .scope(
                                            conversation_id_owned,
                                            tyclaw_agent::runtime::INJECTION_QUEUE
                                                .scope(injection_queue, run_future),
                                        ),
                                ),
                            ),
                        ),
                    ),
                )
                .await?
        };

        // 9.05 release sandbox
        if let (Some((sb, ws)), Some(pool)) = (sandbox, &self.sandbox_pool) {
            info!(sandbox = %sb.id(), "Releasing sandbox");
            if let Some(cb) = ctx.on_progress {
                cb(&format!("[sandbox] 释放容器 {}", sb.id())).await;
            }
            if let Err(e) = pool.release(sb, &ws).await {
                tracing::warn!(error = %e, "Failed to release sandbox");
            }
        }

        // 9.07 注销活跃任务
        {
            let mut tasks = self.active_tasks.lock();
            tasks.remove(&ctx.workspace_key);
            self.write_active_tasks_file(&tasks);
        }

        // 9.1 输出 token 用量和 cache 概要
        if let Some(cb) = ctx.on_progress {
            let prompt = result.total_prompt_tokens;
            let completion = result.total_completion_tokens;
            let hit = result.cache_hit_tokens;
            let write = result.cache_write_tokens;
            if prompt > 0 || hit > 0 {
                let cache_rate = if hit + write > 0 {
                    (hit as f64 / (hit + write) as f64 * 100.0) as u64
                } else {
                    0
                };
                cb(&format!(
                    "[Token] prompt={prompt} completion={completion} | cache: hit={hit} write={write} ({cache_rate}%)"
                )).await;
            }
        }

        // 9.5 ask_user 暂停处理
        if let RuntimeStatus::NeedsInput {
            pending_tool_call_id,
        } = &result.status
        {
            let question = result
                .content
                .clone()
                .unwrap_or_else(|| "I need your input.".into());
            info!(
                tool_call_id = %pending_tool_call_id,
                question = %question,
                "Agent paused (ask_user), saving state for resume"
            );
            if !result.messages.is_empty() {
                    self.save_turn(&ctx.workspace_key, &result.messages, &result.turn_id);
            }
            self.pending_ask_user
                .lock()
                .insert(
                    ctx.workspace_key.clone(),
                    (pending_tool_call_id.clone(), result.messages),
                );
            return Ok(AgentRunResult::EarlyReturn(AgentResponse {
                text: question,
                tools_used: result.tools_used,
                duration_seconds: ctx.start.elapsed().as_secs_f64(),
                prompt_tokens: result.total_prompt_tokens,
                completion_tokens: result.total_completion_tokens,
                output_files: Vec::new(),
            }));
        }

        let final_content = helpers::strip_internal_markers(
            &result
                .content
                .unwrap_or_else(|| "处理完成，未生成回复内容。".into()),
        );

        Ok(AgentRunResult::Completed(CompletedRun {
            final_content,
            tools_used: result.tools_used,
            messages: result.messages,
            turn_id: result.turn_id,
            total_prompt_tokens: result.total_prompt_tokens,
            total_completion_tokens: result.total_completion_tokens,
            cache_hit_tokens: result.cache_hit_tokens,
            cache_write_tokens: result.cache_write_tokens,
            request_id,
        }))
    }

    /// Steps 10-14: 保存轮次、整理记忆、记录速率、审计日志、提取案例。
    async fn post_process(
        &self,
        ctx: &HandlerContext<'_>,
        user_message: &str,
        run: CompletedRun,
    ) -> Result<AgentResponse, TyclawError> {
        let duration = ctx.start.elapsed().as_secs_f64();

        // 10. 保存轮次到会话
        if !run.messages.is_empty() {
            self.save_turn(&ctx.workspace_key, &run.messages, &run.turn_id);
        }

        // 11. 按需整理记忆
        info!("Step 11 reached, enable_memory={}", self.app.features.enable_memory);
        if self.app.features.enable_memory {
            self.persistence.sessions.invalidate(&ctx.workspace_key);
            let session = self.persistence.sessions.get_or_create_clone(&ctx.workspace_key);
            let msg_count = session.messages.len();
            let unconsolidated_count = msg_count - session.last_consolidated;

            let unconsolidated_tokens: usize = session.messages[session.last_consolidated..]
                .iter()
                .map(|m| tyclaw_types::tokens::estimate_message_tokens(m))
                .sum();
            let threshold = self.app.context_window_tokens / 2;
            let should_consolidate = unconsolidated_tokens > threshold;

            info!(
                msg_count,
                unconsolidated_count,
                should_consolidate,
                "Step 11: consolidation check"
            );

            if should_consolidate {
                if let Some(cb) = ctx.on_progress {
                    cb(&format!("[整理记忆中... ({unconsolidated_count} 条消息)]")).await;
                }

                let mem_dir = self.persistence.workspace_mgr.memory_dir(&ctx.workspace_key);
                let consolidator = tyclaw_memory::MemoryConsolidator::new(
                    &mem_dir,
                    self.app.context_window_tokens,
                );
                consolidator
                    .archive_unconsolidated(
                        &session.messages,
                        session.last_consolidated,
                        self.provider.as_ref(),
                        &self.app.model,
                    )
                    .await;

                let mut session = self.persistence.sessions.get_or_create_clone(&ctx.workspace_key);
                session.clear();
                self.persistence.sessions.save(&session).ok();
                self.persistence.sessions.invalidate(&ctx.workspace_key);

                if let Some(cb) = ctx.on_progress {
                    cb("[记忆整理完成，历史已清空]").await;
                }
                info!("Step 11: consolidation done, session cleared");
            }
        }

        // 12. 记录速率
        if self.app.features.enable_rate_limit {
            self.persistence.rate_limiter.record(ctx.user_id);
        }

        // 13. 写入审计日志
        if self.app.features.enable_audit {
            let turn_messages: Vec<_> = run.messages.iter()
                .filter(|m| m.get("_turn_id").and_then(|v| v.as_str()) == Some(&run.turn_id))
                .cloned()
                .collect();
            let skills_used = helpers::extract_skills_used(&turn_messages, &ctx.workspace_key, ctx.user_name);

            let session_id = self.persistence.sessions.get_session_id(&ctx.workspace_key)
                .unwrap_or_else(|| "unknown".into());
            let _ = self.persistence.audit.log(&AuditEntry {
                timestamp: chrono::Utc::now(),
                workspace_key: ctx.workspace_key.clone(),
                session_id,
                user_id: ctx.user_id.into(),
                user_name: ctx.user_name.into(),
                channel: ctx.channel.into(),
                request: user_message.chars().take(500).collect(),
                tool_calls: run.tools_used
                    .iter()
                    .map(|t| serde_json::json!({"name": t}))
                    .collect(),
                skills_used,
                final_response: Some(run.final_content.chars().take(500).collect()),
                total_duration: Some(duration),
                token_usage: Some(serde_json::json!({
                    "prompt_tokens": run.total_prompt_tokens,
                    "completion_tokens": run.total_completion_tokens,
                    "cache_hit_tokens": run.cache_hit_tokens,
                    "cache_write_tokens": run.cache_write_tokens,
                })),
            });
            let cache_rate = if run.cache_hit_tokens + run.cache_write_tokens > 0 {
                (run.cache_hit_tokens as f64
                    / (run.cache_hit_tokens + run.cache_write_tokens) as f64
                    * 100.0) as u64
            } else {
                0
            };
            info!(
                target: "audit",
                workspace_key = %ctx.workspace_key,
                user_id = ctx.user_id,
                tools = %run.tools_used.join(","),
                duration_seconds = duration,
                prompt_tokens = run.total_prompt_tokens,
                completion_tokens = run.total_completion_tokens,
                cache_hit = run.cache_hit_tokens,
                cache_write = run.cache_write_tokens,
                cache_rate = cache_rate,
                "Audit entry recorded",
            );
        }

        // 14. 自动提取案例记录
        if self.app.features.enable_memory && !run.tools_used.is_empty() {
            if let Some(case) = extract_case(
                user_message,
                &run.final_content,
                &run.tools_used,
                ctx.workspace_id,
                ctx.user_id,
                duration,
            ) {
                let ws_cases = self.persistence.workspace_mgr.workspace_cases_dir(&ctx.workspace_key);
                self.persistence.case_store.save(&case, &ws_cases);
                info!(case_id = %case.case_id, "Auto-extracted case");
            }
        }

        // 收集 send_file 工具产生的待发送文件
        let output_files = self.pending_files.drain(run.request_id);

        Ok(AgentResponse {
            text: run.final_content,
            tools_used: run.tools_used,
            duration_seconds: duration,
            prompt_tokens: run.total_prompt_tokens,
            completion_tokens: run.total_completion_tokens,
            output_files,
        })
    }

    /// 保存新轮次消息到会话（截断大的工具结果）。
    ///
    /// 通过 `_reset_iterations_next_run` 标记定位本轮新消息的起始位置，
    /// 只保存标记之后的消息。这比基于数量的 skip 更健壮——
    /// 即使 agent_loop 内部压缩/修改了前缀消息，标记位置也不会漂移。
    ///
    /// 处理逻辑：
    /// - 截断过长的工具结果（超过 500 字符）
    /// - 剥离用户消息中的运行时上下文元数据标签
    /// - 为每条消息添加时间戳
    fn save_turn(
        &self,
        workspace_key: &str,
        messages: &[std::collections::HashMap<String, serde_json::Value>],
        turn_id: &str,
    ) {
        use serde_json::Value;

        let mut entries = Vec::new();
        // 收集 session 历史中已有的 tool_call id，检测 LLM 跨轮次复用 id
        let session = self.persistence.sessions.get_or_create_clone(workspace_key);
        let mut seen_call_ids: HashSet<String> = HashSet::new();
        for m in &session.messages {
            if let Some(Value::Array(tcs)) = m.get("tool_calls") {
                for tc in tcs {
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        seen_call_ids.insert(id.to_string());
                    }
                }
            }
            if let Some(id) = m.get("tool_call_id").and_then(|v| v.as_str()) {
                seen_call_ids.insert(id.to_string());
            }
        }

        for m in messages.iter() {
            // 只保存带有匹配 _turn_id 的消息（agent_loop 本轮新增的）
            let msg_turn_id = m.get("_turn_id").and_then(|v| v.as_str()).unwrap_or("");
            if msg_turn_id != turn_id {
                continue;
            }

            let mut entry = m.clone();
            // 移除内部标记字段，不写入 history
            entry.remove("_turn_id");

            // 对与历史冲突的 tool_call id 添加后缀，避免 Anthropic 400。
            // 使用固定后缀基数确保同一批 assistant + tool result 得到相同后缀。
            let dedup_suffix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as usize % 0xFFFF)
                .unwrap_or(0);
            if let Some(Value::Array(tcs)) = entry.get_mut("tool_calls") {
                for tc in tcs.iter_mut() {
                    if let Some(Value::String(id)) = tc.get_mut("id") {
                        if !seen_call_ids.insert(id.clone()) {
                            let new_id = format!("{}_{:04x}", id, dedup_suffix);
                            warn!(old_id = %id, new_id = %new_id, "Deduplicating tool_call id on save");
                            *id = new_id.clone();
                            seen_call_ids.insert(new_id);
                        }
                    }
                }
            }
            if let Some(Value::String(tcid)) = entry.get_mut("tool_call_id") {
                if !seen_call_ids.insert(tcid.clone()) {
                    let new_id = format!("{}_{:04x}", tcid, dedup_suffix);
                    *tcid = new_id.clone();
                    seen_call_ids.insert(new_id);
                }
            }

            let role = entry
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = entry
                .get("content")
                .and_then(|v| v.as_str())
                .map(String::from);

            if role == "assistant" && content.is_none() && !entry.contains_key("tool_calls") {
                continue;
            }

            if role == "tool" {
                if let Some(ref c) = content {
                    if c.len() > TOOL_RESULT_MAX_CHARS {
                        let truncated: String = c.chars().take(TOOL_RESULT_MAX_CHARS).collect();
                        entry.insert(
                            "content".into(),
                            Value::String(format!("{truncated}\n... (truncated)")),
                        );
                    }
                }
            }

            if role == "user" {
                if let Some(ref c) = content {
                    if let Some(cleaned) = strip_non_task_user_message(c) {
                        entry.insert("content".into(), Value::String(cleaned));
                    } else {
                        continue;
                    }
                }
            }

            if !entry.contains_key("timestamp") {
                entry.insert(
                    "timestamp".into(),
                    Value::String(chrono::Utc::now().to_rfc3339()),
                );
            }

            entries.push(entry);
        }

        // 追加写入（O_APPEND 模式，并发安全）
        self.persistence.sessions.append_messages(workspace_key, &entries).ok();
    }
}

/// Agent 执行完成后的中间数据，传递给 post_process。
struct CompletedRun {
    final_content: String,
    tools_used: Vec<String>,
    messages: Vec<HashMap<String, Value>>,
    turn_id: String,
    total_prompt_tokens: u64,
    total_completion_tokens: u64,
    cache_hit_tokens: u64,
    cache_write_tokens: u64,
    request_id: u64,
}

/// run_agent 的返回值：要么提前返回（ask_user），要么正常完成。
enum AgentRunResult {
    EarlyReturn(AgentResponse),
    Completed(CompletedRun),
}
