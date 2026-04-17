//! 编排器：将 上下文 → 循环 → 门禁 → 记忆 → 审计 → 会话 → 技能 串联起来。
//!
//! 端到端的消息处理流程（14 步）：
//!  1. 速率限制检查（滑动窗口，per-user + global）
//!  2. 获取用户角色（admin / user / viewer）
//!  3. 根据 workspace_id:channel:chat_id 获取或创建会话
//!  4. 处理斜杠命令（如 /new 清除会话并归档记忆）
//!  5. 合并前检查：若 token 超过上下文窗口 50%，自动合并旧消息
//!  6. 收集技能（内建 + 个人）和能力列表
//!  7. 检索相似历史案例（基于关键词匹配）
//!  8. 构建完整消息列表（系统提示 + 历史 + 当前用户消息）
//!  9. 运行 ReAct 循环引擎（AgentLoop）
//! 10. 保存新轮次消息到会话（截断大的工具结果、剥离运行时元数据）
//! 11. 合并后检查：再次检查是否需要合并
//! 12. 记录速率使用
//! 13. 写入审计日志
//! 14. 自动提取案例记录（若本次使用了工具）

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn, Level};

use tyclaw_agent::runtime::OnProgress;
use tyclaw_agent::{AgentRuntime, RuntimeResult, RuntimeStatus};
use tyclaw_control::AuditEntry;
use tyclaw_memory::{extract_case, CaseRetriever};
use tyclaw_prompt::{strip_non_task_user_message, ContextBuilder, PromptInputs, SkillContent};
use tyclaw_provider::LLMProvider;
use tyclaw_types::TyclawError;

use crate::app_context::AppContext;
use crate::builder::OrchestratorBuilder;
use crate::helpers;
use crate::history;
use crate::persistence::PersistenceLayer;
use crate::types::{
    AgentResponse, RequestContext, FIRST_TURN_CASES_CHARS_BONUS, FIRST_TURN_SKILL_BONUS,
    MAX_DYNAMIC_INJECTED_SKILLS, MAX_DYNAMIC_SIMILAR_CASES_CHARS, MAX_HISTORY_TOKENS_HARD_LIMIT,
    MIN_HISTORY_BUDGET_TOKENS, RESET_ON_START_FIELD, TOOL_RESULT_MAX_CHARS,
};

// ---------------------------------------------------------------------------
// Memory 段落相关性过滤
// ---------------------------------------------------------------------------

/// 按段落过滤 MEMORY.md，只保留与当前用户请求相关的段落。
///
/// 规则：
/// - 结构性段落（Skills路径、工作规则等）始终保留
/// - 事实性段落（天气、股价、查询结果等）只在与 user_message 有关键词重叠时保留
/// - 保留段落的顺序不变
fn filter_memory_by_relevance(memory: &str, user_message: &str) -> String {
    // 将用户消息分词为关键词集合（中文按字符，英文按空格）
    let query_tokens = extract_keywords(user_message);
    if query_tokens.is_empty() {
        return memory.to_string();
    }

    // 按 ## 标题拆分段落
    let sections = split_memory_sections(memory);
    if sections.is_empty() {
        return memory.to_string();
    }

    let mut kept = Vec::new();
    let mut filtered_count = 0usize;

    for (header, body) in &sections {
        let full = format!("{}\n{}", header, body);

        // 结构性段落始终保留：包含 skill、path、规则、工作、项目、config 等关键词
        if is_structural_section(header) {
            kept.push(full);
            continue;
        }

        // 事实性段落：检查关键词重叠
        let section_tokens = extract_keywords(&format!("{} {}", header, body));
        let overlap: usize = query_tokens
            .iter()
            .filter(|t| section_tokens.contains(*t))
            .count();

        if overlap > 0 {
            kept.push(full);
        } else {
            filtered_count += 1;
        }
    }

    if filtered_count > 0 {
        info!(
            filtered = filtered_count,
            kept = kept.len(),
            "Filtered irrelevant memory sections"
        );
    }

    kept.join("\n\n")
}

/// 判断是否为结构性段落（始终保留）。
fn is_structural_section(header: &str) -> bool {
    let h = header.to_lowercase();
    // 这些段落是关于工作方式/工具/项目结构的元信息，始终有用
    let structural_keywords = [
        "skill", "path", "location", "规则", "工作", "项目", "config",
        "important", "注意", "workspace", "instability", "note",
        "known", "issue", "bug", "logic", "spec", "结构",
    ];
    structural_keywords.iter().any(|kw| h.contains(kw))
}

/// 将 MEMORY.md 按 `## ` 标题分段。
fn split_memory_sections(memory: &str) -> Vec<(String, String)> {
    let mut sections = Vec::new();
    let mut current_header = String::new();
    let mut current_body = String::new();
    let mut in_section = false;

    for line in memory.lines() {
        if line.starts_with("## ") {
            if in_section {
                sections.push((current_header.clone(), current_body.trim().to_string()));
            }
            current_header = line.to_string();
            current_body = String::new();
            in_section = true;
        } else if in_section {
            current_body.push_str(line);
            current_body.push('\n');
        } else {
            // 顶层内容（## 之前），始终保留
            if !line.trim().is_empty() {
                current_body.push_str(line);
                current_body.push('\n');
            }
        }
    }

    // 处理最后一个段落
    if in_section {
        sections.push((current_header, current_body.trim().to_string()));
    } else if !current_body.trim().is_empty() {
        // 没有任何 ## 标题，整体保留
        sections.push(("".to_string(), current_body.trim().to_string()));
    }

    sections
}

/// 从文本中提取关键词集合。
/// 中文：提取连续的 2 字符 bigram（覆盖中文词汇）
/// 英文/数字：提取连续的 ASCII 词，转小写
fn extract_keywords(text: &str) -> std::collections::HashSet<String> {
    let mut tokens = std::collections::HashSet::new();

    // 先分离 ASCII 词和中文字符
    let mut ascii_buf = String::new();
    let mut cjk_chars: Vec<char> = Vec::new();

    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            ascii_buf.push(c);
            // CJK 序列断开，flush
            if cjk_chars.len() >= 2 {
                for window in cjk_chars.windows(2) {
                    tokens.insert(window.iter().collect::<String>());
                }
            }
            cjk_chars.clear();
        } else if c >= '\u{4E00}' && c <= '\u{9FFF}' {
            // CJK 字符
            cjk_chars.push(c);
            // ASCII 序列断开，flush
            if ascii_buf.len() >= 2 {
                tokens.insert(ascii_buf.to_lowercase());
            }
            ascii_buf.clear();
        } else {
            // 其他字符（标点、空格等），flush 两个 buffer
            if ascii_buf.len() >= 2 {
                tokens.insert(ascii_buf.to_lowercase());
            }
            ascii_buf.clear();
            if cjk_chars.len() >= 2 {
                for window in cjk_chars.windows(2) {
                    tokens.insert(window.iter().collect::<String>());
                }
            }
            cjk_chars.clear();
        }
    }

    // flush 剩余
    if ascii_buf.len() >= 2 {
        tokens.insert(ascii_buf.to_lowercase());
    }
    if cjk_chars.len() >= 2 {
        for window in cjk_chars.windows(2) {
            tokens.insert(window.iter().collect::<String>());
        }
    }

    tokens
}

/// 中枢编排器 —— 连接所有层的核心组件。
///
/// 持有所有子系统的引用，负责协调消息处理的完整生命周期。
pub struct Orchestrator {
    /// 不可变的应用级上下文（workspace/model/features 等），Arc 共享给 subtasks 等子系统
    pub(crate) app: Arc<AppContext>,
    pub(crate) provider: Arc<dyn LLMProvider>,
    pub(crate) runtime: Box<dyn AgentRuntime>,
    pub(crate) context: ContextBuilder,
    /// 有状态的持久化服务（会话/审计/案例/技能/合并/限流/工作区管理）
    pub(crate) persistence: PersistenceLayer,
    pub(crate) pending_files: Arc<tyclaw_tools::PendingFileStore>,
    pub(crate) pending_ask_user:
        parking_lot::Mutex<HashMap<String, (String, Vec<HashMap<String, Value>>)>>,
    pub(crate) timer_service: Option<Arc<tyclaw_tools::timer::TimerService>>,
    pub(crate) active_tasks: Arc<parking_lot::Mutex<HashMap<String, ActiveTask>>>,
    pub(crate) sandbox_pool: Option<Arc<dyn tyclaw_sandbox::SandboxPool>>,
    /// Per-workspace 消息注入队列：workspace busy 时，新消息注入到运行中的 agent loop。
    pub(crate) injection_queues:
        parking_lot::Mutex<HashMap<String, tyclaw_agent::runtime::InjectionQueue>>,
}

/// 活跃任务条目
#[derive(Debug, Clone)]
pub struct ActiveTask {
    pub user_id: String,
    pub summary: String,
    pub started_at: Instant,
}

impl Orchestrator {
    /// 将活跃任务列表写入 .active_tasks.json 文件
    fn write_active_tasks_file(&self, tasks: &HashMap<String, ActiveTask>) {
        let entries: Vec<serde_json::Value> = tasks
            .values()
            .map(|t| {
                serde_json::json!({
                    "user_id": t.user_id,
                    "summary": t.summary,
                    "running_seconds": t.started_at.elapsed().as_secs(),
                })
            })
            .collect();
        let content = serde_json::to_string_pretty(&serde_json::json!({
            "active_tasks": entries,
            "updated_at": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        }))
        .unwrap_or_default();
        let _ = std::fs::write(self.app.workspace.join(".active_tasks.json"), content);
    }

    /// 获取或创建指定 workspace 的注入队列。
    fn get_injection_queue(
        &self,
        workspace_key: &str,
    ) -> tyclaw_agent::runtime::InjectionQueue {
        let mut queues = self.injection_queues.lock();
        queues
            .entry(workspace_key.to_string())
            .or_insert_with(|| {
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))
            })
            .clone()
    }

    /// 创建 Builder（SDK 场景推荐）。
    pub fn builder(
        provider: Arc<dyn LLMProvider>,
        workspace: impl AsRef<Path>,
    ) -> OrchestratorBuilder {
        OrchestratorBuilder::new(provider, workspace)
    }

    /// 创建新的编排器实例。
    ///
    /// 该方法保持兼容原有用法：默认启用审计、记忆、RBAC、限流，并注册默认工具集。
    pub fn new(
        provider: Arc<dyn LLMProvider>,
        workspace: impl AsRef<Path>,
        model: Option<String>,
        max_iterations: Option<usize>,
        context_window_tokens: Option<usize>,
        write_snapshot: bool,
        workspaces_config: Option<HashMap<String, tyclaw_control::WorkspaceConfig>>,
    ) -> Self {
        Self::builder(provider, workspace)
            .with_model_opt(model)
            .with_max_iterations_opt(max_iterations)
            .with_context_window_tokens_opt(context_window_tokens)
            .with_write_snapshot(write_snapshot)
            .with_workspaces_config_opt(workspaces_config)
            .build()
    }

    /// 创建新的编排器实例，支持子任务调度配置。
    pub fn new_with_subtasks(
        provider: Arc<dyn LLMProvider>,
        workspace: impl AsRef<Path>,
        model: Option<String>,
        max_iterations: Option<usize>,
        context_window_tokens: Option<usize>,
        write_snapshot: bool,
        workspaces_config: Option<HashMap<String, tyclaw_control::WorkspaceConfig>>,
        subtasks_config: crate::subtasks::SubtasksConfig,
    ) -> Self {
        Self::builder(provider, workspace)
            .with_model_opt(model)
            .with_max_iterations_opt(max_iterations)
            .with_context_window_tokens_opt(context_window_tokens)
            .with_write_snapshot(write_snapshot)
            .with_workspaces_config_opt(workspaces_config)
            .with_subtasks(subtasks_config)
            .build()
    }

    /// 获取不可变的应用级上下文。
    pub fn app(&self) -> &Arc<AppContext> {
        &self.app
    }

    pub fn timer_service(&self) -> Option<&Arc<tyclaw_tools::timer::TimerService>> {
        self.timer_service.as_ref()
    }

    /// 获取活跃任务列表（监控用）。
    pub fn active_tasks(&self) -> &Arc<parking_lot::Mutex<HashMap<String, ActiveTask>>> {
        &self.active_tasks
    }

    /// 获取持久化层引用（审计、技能等，监控用）。
    pub fn persistence(&self) -> &PersistenceLayer {
        &self.persistence
    }

    /// 覆盖 works 目录路径（对应 --works-dir 命令行参数）。
    pub fn set_works_dir(&mut self, path: std::path::PathBuf) {
        self.persistence.workspace_mgr.set_works_dir(&path);
        self.persistence.skills.set_works_dir(path);
    }

    /// 设置沙箱池（启动时由 main.rs 注入）。
    pub fn set_sandbox_pool(&mut self, pool: Arc<dyn tyclaw_sandbox::SandboxPool>) {
        self.sandbox_pool = Some(pool);
        // sandbox 模式下 LLM 的工具在容器内执行，路径应显示为 "." 而非 host 绝对路径
        self.context.set_display_workspace(".");
    }

    /// 启动 workspace 超时回收后台任务。
    ///
    /// 每 `check_interval_secs` 秒扫描一次活跃 workspace，
    /// 超过 `idle_timeout_secs` 未访问的执行回收：
    /// 1. consolidate 对话历史 → memory
    /// 2. 清空 history.jsonl
    /// 3. 清空 work/tmp、work/dispatches、work/attachments
    /// 4. 销毁 Docker 容器
    pub fn spawn_reaper(
        self: &Arc<Self>,
        idle_timeout_secs: u64,
        check_interval_secs: u64,
    ) {
        let orch = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(check_interval_secs));
            loop {
                interval.tick().await;
                let idle_keys = orch
                    .persistence
                    .sessions
                    .find_idle_workspaces(idle_timeout_secs);
                for workspace_key in idle_keys {
                    // 检查 work 目录下是否有近期文件修改（子 agent 可能仍在执行）
                    let work_dir = orch.persistence.workspace_mgr.work_dir(&workspace_key);
                    if has_recent_file_activity(&work_dir, idle_timeout_secs) {
                        // 刷新 last_access，跳过本轮回收
                        orch.persistence.sessions.touch(&workspace_key);
                        info!(
                            workspace_key = %workspace_key,
                            "Skipping reap: work directory has recent file activity"
                        );
                        continue;
                    }

                    info!(
                        workspace_key = %workspace_key,
                        "Reaping idle workspace"
                    );

                    // 1. consolidate 对话到 memory
                    if orch.app.features.enable_memory {
                        if let Some(session) = orch.persistence.sessions.evict(&workspace_key) {
                            if !session.messages.is_empty() {
                                let mem_dir = orch
                                    .persistence
                                    .workspace_mgr
                                    .memory_dir(&workspace_key);
                                let store =
                                    tyclaw_memory::MemoryStore::new(&mem_dir);
                                let snapshot =
                                    &session.messages[session.last_consolidated..];
                                if !snapshot.is_empty() {
                                    tyclaw_memory::consolidate_with_provider(
                                        &store,
                                        snapshot,
                                        orch.provider.as_ref(),
                                        &orch.app.model,
                                    )
                                    .await;
                                }
                            }
                        }
                    } else {
                        orch.persistence.sessions.evict(&workspace_key);
                    }

                    // 2. 清空 history.jsonl
                    let history_path = orch
                        .persistence
                        .workspace_mgr
                        .history_path(&workspace_key);
                    let _ = std::fs::remove_file(&history_path);

                    // 3. 清空临时目录
                    for dir_name in &["tmp", "dispatches", "attachments"] {
                        let dir = orch
                            .persistence
                            .workspace_mgr
                            .work_dir(&workspace_key)
                            .join(dir_name);
                        if dir.is_dir() {
                            let _ = std::fs::remove_dir_all(&dir);
                            let _ = std::fs::create_dir_all(&dir);
                        }
                    }

                    // 4. 销毁 Docker 容器
                    let container_name = format!("tyclaw-{workspace_key}");
                    let _ = tokio::process::Command::new("docker")
                        .args(["rm", "-f", &container_name])
                        .output()
                        .await;

                    // 5. 清除 prompt cache（避免旧 tool_call 残留导致 400）
                    let cache_scope = format!("session:{workspace_key}");
                    orch.provider.clear_cache_scope(&cache_scope);

                    // 6. 清理 per-workspace 串行锁
                    orch.injection_queues.lock().remove(&workspace_key);

                    // 6b. 清理 pending_ask_user 避免无限制内存增长
                    orch.pending_ask_user.lock().remove(&workspace_key);

                    // 7. 写审计日志
                    let session_id = "reaper".to_string();
                    let _ = orch.persistence.audit.log(&AuditEntry {
                        timestamp: chrono::Utc::now(),
                        workspace_key: workspace_key.clone(),
                        session_id,
                        user_id: "system".into(),
                        user_name: "system".into(),
                        channel: "reaper".into(),
                        request: "workspace idle timeout".into(),
                        tool_calls: vec![],
                        skills_used: vec![],
                        final_response: Some("consolidated and cleaned".into()),
                        total_duration: None,
                        token_usage: None,
                    });

                    info!(
                        workspace_key = %workspace_key,
                        "Workspace reaped successfully"
                    );
                }
            }
        });
    }

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

        // 如果 workspace 正在处理中，将消息注入到运行中的 agent loop，立即返回
        if let Some(_elapsed) = self.persistence.sessions.busy_elapsed(&workspace_key) {
            info!(
                workspace_key = %workspace_key,
                "Workspace busy, injecting message into running agent loop"
            );

            // 复制附件到 workspace 并组装完整消息（与正常流程一致）
            let mut msg = user_message.to_string();
            if !req.file_attachments.is_empty() {
                self.persistence.workspace_mgr.ensure_workspace(&workspace_key);
                let attachments_dir = self.persistence.workspace_mgr.attachments_dir(&workspace_key);
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

            let queue = self.get_injection_queue(&workspace_key);
            if let Ok(mut pending) = queue.lock() {
                pending.push(tyclaw_agent::runtime::chat_message("user", &msg));
            }

            // 审计记录：注入消息也需要留痕
            if self.app.features.enable_audit {
                let session_id = self.persistence.sessions.get_session_id(&workspace_key)
                    .unwrap_or_else(|| "unknown".into());
                let _ = self.persistence.audit.log(&AuditEntry {
                    timestamp: chrono::Utc::now(),
                    workspace_key: workspace_key.clone(),
                    session_id,
                    user_id: user_id.into(),
                    user_name: user_name.into(),
                    channel: channel.into(),
                    request: format!("[injected] {}", msg.chars().take(500).collect::<String>()),
                    tool_calls: vec![],
                    skills_used: vec![],
                    final_response: Some("injected into running agent loop".into()),
                    total_duration: Some(start.elapsed().as_secs_f64()),
                    token_usage: None,
                });
            }

            return Ok(AgentResponse {
                text: String::new(),
                tools_used: vec![],
                duration_seconds: start.elapsed().as_secs_f64(),
                prompt_tokens: 0,
                completion_tokens: 0,
                output_files: Vec::new(),
            });
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
            ContextBuilder::add_tool_result(
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
        let _workspace = self.persistence.workspace_mgr.get_workspace(workspace_id);
        let mut budget_plan = helpers::compute_context_budget_plan(user_message);
        let is_first_turn = {
            let session = self.persistence.sessions.get_or_create_clone(&workspace_key);
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

        // 3. 获取/创建会话（workspace_key 已在前面定义）

        // 4. 处理斜杠命令
        let cmd = user_message.trim().to_lowercase();
        if cmd == "/save" || cmd == "/handoff" {
            let session = self.persistence.sessions.get_or_create_clone(&workspace_key);
            let messages = session.messages.clone();
            if messages.is_empty() {
                return Ok(AgentResponse {
                    text: "当前会话暂无可保存内容。".into(),
                    tools_used: Vec::new(),
                    duration_seconds: start.elapsed().as_secs_f64(),
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    output_files: Vec::new(),
                });
            }

            let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
            let safe_key = workspace_key.replace(':', "_");
            let handoff_dir = self.persistence.workspace_mgr.workspace_dir(&workspace_key);
            let handoff_path = handoff_dir.join(format!("handoff_{safe_key}_{ts}.md"));
            let handoff = helpers::build_handoff_markdown(&workspace_key, &messages);
            let _ = std::fs::create_dir_all(&handoff_dir);

            match std::fs::write(&handoff_path, handoff) {
                Ok(_) => {
                    let display = handoff_path
                        .strip_prefix(&self.app.workspace)
                        .ok()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| handoff_path.display().to_string());
                    return Ok(AgentResponse {
                        text: format!(
                            "已保存当前会话信息到：`{display}`。\n\
                            现在可以输入 `/new` 开新任务，并把这个文件内容粘贴给我继续。"
                        ),
                        tools_used: Vec::new(),
                        duration_seconds: start.elapsed().as_secs_f64(),
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        output_files: Vec::new(),
                    });
                }
                Err(e) => {
                    return Ok(AgentResponse {
                        text: format!("保存会话失败：{e}"),
                        tools_used: Vec::new(),
                        duration_seconds: start.elapsed().as_secs_f64(),
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        output_files: Vec::new(),
                    });
                }
            }
        }
        if cmd == "/new" {
            let session = self.persistence.sessions.get_or_create_clone(&workspace_key);
            let messages = session.messages.clone();
            let last_consolidated = session.last_consolidated;
            if self.app.features.enable_memory {
                let mem_dir = self.persistence.workspace_mgr.memory_dir(&workspace_key);
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
            let mut session = self.persistence.sessions.get_or_create_clone(&workspace_key);
            session.clear();
            self.persistence.sessions.save(&session).ok();
            self.persistence.sessions.invalidate(&workspace_key);
            return Ok(AgentResponse {
                text: "New session started.".into(),
                tools_used: Vec::new(),
                duration_seconds: start.elapsed().as_secs_f64(),
                prompt_tokens: 0,
                completion_tokens: 0,
                output_files: Vec::new(),
            });
        }

        // 4.5 如果有文件附件，复制到 workspace work/attachments 并追加到消息
        let user_attachments_dir = self.persistence.workspace_mgr.attachments_dir(&workspace_key);
        let user_message = if req.file_attachments.is_empty() {
            user_message.to_string()
        } else {
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
        };
        let user_message = user_message.as_str();

        // 5. 合并前检查（已移除）
        // 每轮结束时会 archive + clear，所以新请求开始时历史已经是空的，
        // 无需再做 consolidation。上下文全靠 MEMORY.md 提供。

        // 6. 收集技能和能力
        // 所有 Skill 的摘要注入 context，LLM 自己决定是否读取完整 SKILL.md。
        // 不做 trigger 匹配，不做白名单过滤——让 LLM 语义理解来决定。
        let caps = self.persistence.skills.get_caps(&workspace_key);
        let all_skill_metas = self.persistence.skills.get_skill_contents(&workspace_key);
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
                            // 全局路径（如 tools/ga_query.py, skills/xxx/tool.py）
                            format!("{}/{}", pcfg.container_root, tool)
                        } else if s.status == "builtin" {
                            format!("{}/{}/{}/{}/{}", pcfg.container_root, pcfg.global_skills_mount, s.category, s.key, tool)
                        } else {
                            let ws_root = self.persistence.workspace_mgr.workspace_dir(&workspace_key);
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
                matched: false, // 不再区分匹配/未匹配，统一用摘要模式
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

        // 7. 检索案例：pinned（稳定，缓存）+ similar（动态）
        let (pinned_cases, similar_cases) = if self.app.features.enable_memory {
            let retriever = CaseRetriever::new(&self.persistence.case_store);
            let ws_cases = self.persistence.workspace_mgr.workspace_cases_dir(&workspace_key);
            let (pinned, similar) =
                retriever.format_for_prompt_split(user_message, &ws_cases, 3);
            let similar = helpers::optimize_similar_cases(&similar, budget_plan.max_cases_chars);
            (pinned, similar)
        } else {
            (String::new(), String::new())
        };

        // 8. 构建消息（含历史）
        let history = {
            let session = self.persistence.sessions.get_or_create_clone(&workspace_key);
            let raw_history = session.get_history(0);
            let deduped = history::dedupe_history(&raw_history);
            let history_budget = std::cmp::max(
                MIN_HISTORY_BUDGET_TOKENS,
                (self.app.context_window_tokens * budget_plan.history_ratio) / 100,
            );
            // 应用绝对上限：即使 Window 很大，也不允许历史超过硬顶
            let history_budget = std::cmp::min(history_budget, MAX_HISTORY_TOKENS_HARD_LIMIT);

            let trimmed = history::trim_history_by_token_budget(&deduped, history_budget);
            history::enforce_tool_call_pairing(&trimmed)
        };

        // 读取 workspace 的 memory 内容，并按相关性过滤段落
        let memory_content = {
            let mem_file = self.persistence.workspace_mgr.memory_dir(&workspace_key).join("MEMORY.md");
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
            channel: Some(channel),
            chat_id: Some(chat_id),
            user_id: Some(user_id),
            workspace_id: Some(workspace_id),
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
        info!(workspace_key = %workspace_key, "Injected reset marker for fresh user turn");
        if tracing::enabled!(Level::DEBUG) {
            debug!(
                target: "prompt.assembly",
                workspace_id = workspace_id,
                user_id = user_id,
                prompt = %serde_json::to_string(&initial_messages).unwrap_or_default(),
                "Assembled messages for LLM",
            );
        }

        // 8.5 注册活跃任务到文件
        let task_summary: String = user_message.chars().take(50).collect();
        {
            let mut tasks = self.active_tasks.lock();
            tasks.insert(
                workspace_key.clone(),
                ActiveTask {
                    user_id: user_id.to_string(),
                    summary: task_summary,
                    started_at: start,
                },
            );
            // 不在开始时写磁盘，只在任务完成/失败时写一次（减少 IO）
        }

        // 9. 运行 Agent 执行引擎
        //    通过 task_local 传递 per-request 状态：pending_files ID、timer context、sandbox
        let request_id = self.pending_files.new_request();
        let channel_owned = channel.to_string();
        let chat_id_owned = chat_id.to_string();
        let user_id_owned = user_id.to_string();
        let conversation_id_owned = chat_id.to_string();

        // 9a. Per-workspace work root：每个 workspace 有自己的 work 目录
        let user_workspace = self.persistence.workspace_mgr.work_dir(&workspace_key);
        std::fs::create_dir_all(&user_workspace).ok();

        let sandbox: Option<(
            std::sync::Arc<dyn tyclaw_sandbox::Sandbox>,
            std::path::PathBuf,
        )> = if let Some(pool) = &self.sandbox_pool {
            match pool.acquire(&user_workspace, &[]).await {
                Ok(sb) => {
                    info!(sandbox = %sb.id(), user = %user_id, "Acquired sandbox");
                    if let Some(cb) = on_progress {
                        cb(&format!(
                            "[sandbox] 获取容器 {} | 用户 {}",
                            sb.id(),
                            user_id
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

        // turn_id 由 agent_loop 生成并打在每条新消息上，save_turn 按此筛选。

        let cache_scope = format!("session:{workspace_key}");
        let injection_queue = self.get_injection_queue(&workspace_key);

        let run_future = self.runtime.run(
            initial_messages,
            &user_role,
            Some(&cache_scope),
            on_progress,
        );

        // task_local 注入：user_role + request_id + timer context + sandbox
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
            if let Some(cb) = on_progress {
                cb(&format!("[sandbox] 释放容器 {}", sb.id())).await;
            }
            if let Err(e) = pool.release(sb, &ws).await {
                tracing::warn!(error = %e, "Failed to release sandbox");
            }
        }

        // 9.07 注销活跃任务
        {
            let mut tasks = self.active_tasks.lock();
            tasks.remove(&workspace_key);
            self.write_active_tasks_file(&tasks);
        }

        // 9.1 输出 token 用量和 cache 概要
        if let Some(cb) = on_progress {
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
            // 保存暂停时已完成的轮次到会话
            if !result.messages.is_empty() {
                    self.save_turn(&workspace_key, &result.messages, &result.turn_id);
            }
            // 保存完整消息历史，以便恢复时使用
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

        // 10. 保存轮次到会话
        if !result.messages.is_empty() {
            self.save_turn(&workspace_key, &result.messages, &result.turn_id);
        }

        // 11. 按需整理记忆
        info!("Step 11 reached, enable_memory={}", self.app.features.enable_memory);
        if self.app.features.enable_memory {
            // Invalidate cached session so we pick up messages just written by save_turn.
            self.persistence.sessions.invalidate(&workspace_key);
            let session = self.persistence.sessions.get_or_create_clone(&workspace_key);
            let msg_count = session.messages.len();
            let unconsolidated_count = msg_count - session.last_consolidated;

            // 仅按 token 量触发：超过上下文窗口 50% 时整理
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
                // 告诉用户在整理记忆
                if let Some(cb) = on_progress {
                    cb(&format!("[整理记忆中... ({unconsolidated_count} 条消息)]")).await;
                }

                let mem_dir = self.persistence.workspace_mgr.memory_dir(&workspace_key);
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

                // 清空历史
                let mut session = self.persistence.sessions.get_or_create_clone(&workspace_key);
                session.clear();
                self.persistence.sessions.save(&session).ok();
                self.persistence.sessions.invalidate(&workspace_key);

                if let Some(cb) = on_progress {
                    cb("[记忆整理完成，历史已清空]").await;
                }
                info!("Step 11: consolidation done, session cleared");
            }
        }

        // 12. 记录速率
        if self.app.features.enable_rate_limit {
            self.persistence.rate_limiter.record(user_id);
        }

        // 13. 写入审计日志
        if self.app.features.enable_audit {
            // 只从当前轮次的消息中提取 skill 调用（按 _turn_id 过滤，避免历史消息误报）
            let turn_messages: Vec<_> = result.messages.iter()
                .filter(|m| m.get("_turn_id").and_then(|v| v.as_str()) == Some(&result.turn_id))
                .cloned()
                .collect();
            let skills_used = helpers::extract_skills_used(&turn_messages, &workspace_key, user_name);

            let session_id = self.persistence.sessions.get_session_id(&workspace_key)
                .unwrap_or_else(|| "unknown".into());
            let _ = self.persistence.audit.log(&AuditEntry {
                timestamp: chrono::Utc::now(),
                workspace_key: workspace_key.clone(),
                session_id,
                user_id: user_id.into(),
                user_name: user_name.into(),
                channel: channel.into(),
                request: user_message.chars().take(500).collect(),
                tool_calls: tools_used
                    .iter()
                    .map(|t| serde_json::json!({"name": t}))
                    .collect(),
                skills_used,
                final_response: Some(final_content.chars().take(500).collect()),
                total_duration: Some(duration),
                token_usage: Some(serde_json::json!({
                    "prompt_tokens": result.total_prompt_tokens,
                    "completion_tokens": result.total_completion_tokens,
                    "cache_hit_tokens": result.cache_hit_tokens,
                    "cache_write_tokens": result.cache_write_tokens,
                })),
            });
            let cache_rate = if result.cache_hit_tokens + result.cache_write_tokens > 0 {
                (result.cache_hit_tokens as f64
                    / (result.cache_hit_tokens + result.cache_write_tokens) as f64
                    * 100.0) as u64
            } else {
                0
            };
            info!(
                target: "audit",
                workspace_key = %workspace_key,
                user_id = user_id,
                tools = %tools_used.join(","),
                duration_seconds = duration,
                prompt_tokens = result.total_prompt_tokens,
                completion_tokens = result.total_completion_tokens,
                cache_hit = result.cache_hit_tokens,
                cache_write = result.cache_write_tokens,
                cache_rate = cache_rate,
                "Audit entry recorded",
            );
        }

        // 14. 自动提取案例记录
        if self.app.features.enable_memory && !tools_used.is_empty() {
            if let Some(case) = extract_case(
                user_message,
                &final_content,
                &tools_used,
                workspace_id,
                user_id,
                duration,
            ) {
                let ws_cases = self.persistence.workspace_mgr.workspace_cases_dir(&workspace_key);
                self.persistence.case_store.save(&case, &ws_cases);
                info!(case_id = %case.case_id, "Auto-extracted case");
            }
        }

        // 收集 send_file 工具产生的待发送文件
        let output_files = self.pending_files.drain(request_id);

        Ok(AgentResponse {
            text: final_content,
            tools_used,
            duration_seconds: duration,
            prompt_tokens: result.total_prompt_tokens,
            completion_tokens: result.total_completion_tokens,
            output_files,
        })
    }

    /// 保存新轮次消息���会话（截断大的工具结果）。
    ///
    /// 通过 `_reset_iterations_next_run` 标记定位本轮新消息的起始位置，
    /// 只保存标记之后的消息。这比基于数量的 skip 更健壮——
    /// 即使 agent_loop 内部压缩/修改了前缀消息，标记位置也不会漂移。
    ///
    /// 处理逻辑：
    /// - 截断过长的工具结果（超过 500 字符）
    /// - 剥离��户消息中的运行时上��文元数据标签
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(role: &str, content: &str) -> HashMap<String, serde_json::Value> {
        let mut m = HashMap::new();
        m.insert("role".into(), json!(role));
        m.insert("content".into(), json!(content));
        m
    }

    #[test]
    fn test_dedupe_history_only_removes_consecutive_duplicates() {
        let history = vec![
            msg("user", "hello"),
            msg("user", "hello"), // 连续重复，应该被去掉
            msg("assistant", "ok"),
            msg("user", "hello"), // 非连续重复，应该保留
        ];
        let deduped = history::dedupe_history(&history);
        assert_eq!(deduped.len(), 3);
        assert_eq!(deduped[0]["role"], "user");
        assert_eq!(deduped[1]["role"], "assistant");
        assert_eq!(deduped[2]["role"], "user");
    }

    #[test]
    fn test_dedupe_history_keeps_different_tool_calls() {
        let mut a1 = msg("assistant", "");
        a1.insert(
            "tool_calls".into(),
            json!([{"id":"tool_1","type":"function","function":{"name":"read_file","arguments":"{}"}}]),
        );
        let mut a2 = msg("assistant", "");
        a2.insert(
            "tool_calls".into(),
            json!([{"id":"tool_2","type":"function","function":{"name":"read_file","arguments":"{}"}}]),
        );

        let history = vec![a1, a2];
        let deduped = history::dedupe_history(&history);
        // tool_calls id 不同，不应被误去重
        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn test_trim_history_by_token_budget_keeps_latest() {
        let history = vec![
            msg("user", "first"),
            msg("assistant", "second"),
            msg("user", "third"),
        ];
        let trimmed = history::trim_history_by_token_budget(&history, 2);
        assert!(!trimmed.is_empty());
        // 至少包含最后一条
        assert_eq!(trimmed.last().unwrap()["content"], "third");
    }

    #[test]
    fn test_optimize_similar_cases_dedup_and_truncate() {
        let raw = "Case A\nCase A\nCase B\n";
        let optimized = helpers::optimize_similar_cases(raw, 10);
        assert!(optimized.contains("Case A"));
        assert!(optimized.contains("truncated"));
        // 行级去重后不应出现两次完全相同的 Case A
        assert_eq!(optimized.matches("Case A").count(), 1);
    }

    #[test]
    fn test_optimize_similar_cases_utf8_safe_truncate() {
        let raw = "案例一：中文内容\n案例二：继续排查";
        let optimized = helpers::optimize_similar_cases(raw, 7);
        assert!(optimized.contains("truncated"));
        assert!(optimized.is_char_boundary(optimized.len()));
    }

    #[test]
    fn test_enforce_tool_call_pairing_drops_orphan_tool_result() {
        let mut assistant = msg("assistant", "");
        assistant.insert(
            "tool_calls".into(),
            json!([{"id":"tool_ok","type":"function","function":{"name":"exec","arguments":"{}"}}]),
        );

        let mut valid_tool = msg("tool", "ok");
        valid_tool.insert("tool_call_id".into(), json!("tool_ok"));
        valid_tool.insert("name".into(), json!("exec"));

        let mut orphan_tool = msg("tool", "orphan");
        orphan_tool.insert("tool_call_id".into(), json!("tool_missing"));
        orphan_tool.insert("name".into(), json!("exec"));

        let history = vec![assistant, valid_tool, orphan_tool];
        let cleaned = history::enforce_tool_call_pairing(&history);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(cleaned[0]["role"], "assistant");
        assert_eq!(cleaned[1]["role"], "tool");
        assert_eq!(cleaned[1]["tool_call_id"], "tool_ok");
    }

    #[test]
    fn test_compute_context_budget_plan_modes() {
        let p_debug = helpers::compute_context_budget_plan("帮我排查服务报错和 timeout");
        assert!(p_debug.max_cases_chars >= 3000);

        let p_follow = helpers::compute_context_budget_plan("继续刚才那个问题");
        assert!(p_follow.history_ratio >= 60);

        let p_code = helpers::compute_context_budget_plan("请实现一个重构方案");
        assert!(p_code.max_skills >= 10);
    }

    #[test]
    fn test_cross_round_tool_pairing_regression() {
        // Round 1: assistant(tool_a) -> tool_a result
        let mut a1 = msg("assistant", "");
        a1.insert(
            "tool_calls".into(),
            json!([{"id":"tool_a","type":"function","function":{"name":"list_dir","arguments":"{}"}}]),
        );
        let mut t1 = msg("tool", "result_a");
        t1.insert("tool_call_id".into(), json!("tool_a"));
        t1.insert("name".into(), json!("list_dir"));

        // Round 2: assistant(tool_b) -> tool_b result
        let mut a2 = msg("assistant", "");
        a2.insert(
            "tool_calls".into(),
            json!([{"id":"tool_b","type":"function","function":{"name":"read_file","arguments":"{}"}}]),
        );
        let mut t2 = msg("tool", "result_b");
        t2.insert("tool_call_id".into(), json!("tool_b"));
        t2.insert("name".into(), json!("read_file"));

        // 插入一条孤儿 tool（不在上一条 assistant 的 tool_calls 中），应被清理
        let mut orphan = msg("tool", "orphan");
        orphan.insert("tool_call_id".into(), json!("tool_x"));
        orphan.insert("name".into(), json!("exec"));

        let history = vec![
            msg("user", "round1"),
            a1,
            t1,
            msg("assistant", "after round1"),
            msg("user", "round2"),
            a2,
            t2,
            orphan,
            msg("assistant", "done"),
        ];

        // 模拟真实链路：先预算裁剪（给足预算不触发裁掉），再做配对修复
        let trimmed = history::trim_history_by_token_budget(&history, 10_000);
        let cleaned = history::enforce_tool_call_pairing(&trimmed);

        // 有效 tool 结果保留
        assert!(cleaned
            .iter()
            .any(|m| m.get("tool_call_id").and_then(|v| v.as_str()) == Some("tool_a")));
        assert!(cleaned
            .iter()
            .any(|m| m.get("tool_call_id").and_then(|v| v.as_str()) == Some("tool_b")));

        // 孤儿 tool 结果必须被移除
        assert!(!cleaned
            .iter()
            .any(|m| m.get("tool_call_id").and_then(|v| v.as_str()) == Some("tool_x")));
    }

    #[test]
    fn test_cross_round_pairing_under_tight_budget() {
        // 构造两轮 tool 调用，并让第一轮内容很长，逼迫预算裁剪时优先丢弃旧轮次。
        let mut a1 = msg("assistant", "");
        a1.insert(
            "tool_calls".into(),
            json!([{"id":"tool_old","type":"function","function":{"name":"list_dir","arguments":"{}"}}]),
        );
        let mut t1 = msg("tool", &"old_result ".repeat(200));
        t1.insert("tool_call_id".into(), json!("tool_old"));
        t1.insert("name".into(), json!("list_dir"));

        let mut a2 = msg("assistant", "");
        a2.insert(
            "tool_calls".into(),
            json!([{"id":"tool_new","type":"function","function":{"name":"read_file","arguments":"{}"}}]),
        );
        let mut t2 = msg("tool", "new_result");
        t2.insert("tool_call_id".into(), json!("tool_new"));
        t2.insert("name".into(), json!("read_file"));

        // 额外孤儿 tool，理论上必须清理
        let mut orphan = msg("tool", "orphan");
        orphan.insert("tool_call_id".into(), json!("tool_orphan"));
        orphan.insert("name".into(), json!("exec"));

        let history = vec![
            msg("user", "round_old"),
            a1,
            t1,
            msg("assistant", "after old"),
            msg("user", "round_new"),
            a2,
            t2,
            orphan,
            msg("assistant", "done"),
        ];

        // 小预算触发裁剪（这里只要求行为正确，不依赖精确 token 值）
        let trimmed = history::trim_history_by_token_budget(&history, 120);
        let cleaned = history::enforce_tool_call_pairing(&trimmed);

        // 不允许出现孤儿 tool 结果
        assert!(!cleaned.iter().any(|m| {
            m.get("role").and_then(|v| v.as_str()) == Some("tool")
                && m.get("tool_call_id").and_then(|v| v.as_str()) == Some("tool_orphan")
        }));

        // 如果存在 tool 消息，必须都能在"紧邻之前的 assistant.tool_calls"中找到配对 id
        let mut expected_ids = std::collections::HashSet::new();
        for m in &cleaned {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
            if role == "assistant" {
                expected_ids.clear();
                if let Some(tool_calls) = m.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tool_calls {
                        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                            expected_ids.insert(id.to_string());
                        }
                    }
                }
            } else if role == "tool" {
                let id = m
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .expect("tool must have tool_call_id");
                assert!(
                    expected_ids.contains(id),
                    "found unpaired tool_result id={id} after trimming"
                );
            } else {
                expected_ids.clear();
            }
        }

        // 一般情况下，最近轮次应保留；若预算极端导致无 tool，也应至少不报错。
        let has_new_pair = cleaned
            .iter()
            .any(|m| m.get("tool_call_id").and_then(|v| v.as_str()) == Some("tool_new"));
        if cleaned
            .iter()
            .any(|m| m.get("role").and_then(|v| v.as_str()) == Some("tool"))
        {
            assert!(
                has_new_pair,
                "when tool messages remain, latest pair should survive"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Memory 过滤测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod memory_filter_tests {
    use super::*;

    #[test]
    fn test_filter_removes_irrelevant_weather() {
        let memory = "\
## Skills paths and locations
- Weather Checker: _personal/skills/weather-checker/tool.py
- LTV Excel Generator: _personal/skills/ltv-excel-generator/tool.py

## Weather facts previously obtained
- Beijing 2026-04-10: 晴 22.3°C
- Tokyo 2026-04-11: 阴 12.6~24.2°C
- Hong Kong 2026-04-09: Partly cloudy

## US Stock price facts
- AAPL $258.90 (+$5.40, +2.13%)

## Gold price facts
- Gold spot ~$4,715.74/oz";

        // 用户请求写小游戏 → 天气/股价/金价段落应该被过滤，Skills路径保留
        let filtered = filter_memory_by_relevance(memory, "帮我写一个H5小游戏");
        assert!(filtered.contains("Skills paths"), "structural section should be kept");
        assert!(!filtered.contains("Beijing"), "weather facts should be filtered");
        assert!(!filtered.contains("AAPL"), "stock facts should be filtered");
        assert!(!filtered.contains("Gold spot"), "gold facts should be filtered");
    }

    #[test]
    fn test_filter_keeps_relevant_weather() {
        let memory = "\
## Weather facts previously obtained
- 北京 2026-04-10: 晴 22.3°C, 天气预报数据

## Skills paths and locations
- Weather Checker: tool.py";

        // 用户请求查天气 → 天气段落应该保留（"天气" bigram 重叠）
        let filtered = filter_memory_by_relevance(memory, "帮我查一下北京近3天的天气");
        assert!(filtered.contains("北京"), "weather should be kept when relevant");
    }

    #[test]
    fn test_filter_keeps_all_structural() {
        let memory = "\
## IMPORTANT: Skills workspace notes
- Personal skills directory has been deleted

## Known data quality issues
- input.xlsx contains extrapolated pay data";

        let filtered = filter_memory_by_relevance(memory, "随便什么请求");
        assert!(filtered.contains("IMPORTANT"), "important sections kept");
        assert!(filtered.contains("Known data"), "known issues kept");
    }

    #[test]
    fn test_extract_keywords_chinese() {
        let kws = extract_keywords("帮我写一个H5小游戏");
        assert!(kws.contains("h5"));
        // 应该有中文 bigram
        assert!(kws.contains("游戏") || kws.contains("小游"));
    }

    #[test]
    fn test_split_sections() {
        let memory = "# Top\nsome intro\n\n## Section A\ncontent a\n\n## Section B\ncontent b";
        let sections = split_memory_sections(memory);
        assert_eq!(sections.len(), 2);
        assert!(sections[0].0.contains("Section A"));
        assert!(sections[1].0.contains("Section B"));
    }
}

/// 检查目录下是否有最近 `threshold_secs` 秒内修改的文件。
/// 用于 reaper 判断子 agent 是否仍在活跃执行。
fn has_recent_file_activity(dir: &std::path::Path, threshold_secs: u64) -> bool {
    let now = std::time::SystemTime::now();
    let threshold = std::time::Duration::from_secs(threshold_secs);

    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                if let Ok(age) = now.duration_since(modified) {
                    if age < threshold {
                        return true;
                    }
                }
            }
        }
    }
    false
}
