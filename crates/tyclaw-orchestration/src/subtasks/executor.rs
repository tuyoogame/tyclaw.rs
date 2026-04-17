//! 节点执行器：为每个子任务启动一个 mini AgentLoop（带完整工具集）。
//!
//! 与原始实现的区别：
//! - 旧：单次 `chat_with_retry` 调用，子模型只能纯文本问答
//! - 新：完整 ReAct 循环，子模型可以 read_file / write_file / exec 等
//!
//! 这是 openspec design.md 第 16 行的设计意图：
//! > Decision: 保留现有 Orchestrator 作为节点执行单元。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use tracing::{info, warn};

use tyclaw_agent::runtime::OnProgress;
use tyclaw_agent::{parse_thinking_prefix, AgentLoop, AgentRuntime};
use tyclaw_prompt::{ContextBuilder, PlannedPromptContext, PromptContextEntry};
use tyclaw_provider::LLMProvider;
use tyclaw_tools::ToolRegistry;

use crate::app_context::AppContext;
use crate::builder::register_core_tools;

use super::protocol::{ExecutionRecord, NodeStatus, TaskNode};
use super::routing::RoutingPolicy;

/// Sub-agent 的默认最大迭代次数。
/// coding 类任务需要 write → run → debug 多轮循环，需要足够的轮次。
const SUB_AGENT_MAX_ITERATIONS: usize = 40;

/// Sub-agent 累计输出字符上限（content + reasoning）。
/// 60K 字符 ≈ 30K tokens，足够完成大多数子任务。
/// 防止 GLM 等模型无限输出，避免浪费 token 和时间。
const SUB_AGENT_MAX_OUTPUT_CHARS: usize = 60_000;

/// 节点执行器：将单个 TaskNode 交给一个 mini AgentLoop 执行。
///
/// 每个子任务获得：
/// - 独立的 AgentLoop（带完整 ReAct 循环）
/// - 独立的 ToolRegistry（read_file, write_file, exec 等）
/// - 按路由策略选择的 LLM Provider
/// - 共享的 workspace 访问权限
pub struct NodeExecutor {
    /// model_name → provider 的映��表。
    providers: HashMap<String, Arc<dyn LLMProvider>>,
    routing: RoutingPolicy,
    /// 不可变的应用级上下文（workspace 等），Arc 共享。
    app: Arc<AppContext>,
    /// sub-agent 最大迭代次数。
    max_iterations: usize,
}

impl NodeExecutor {
    pub fn new(
        providers: HashMap<String, Arc<dyn LLMProvider>>,
        routing: RoutingPolicy,
        app: Arc<AppContext>,
    ) -> Self {
        Self::with_max_iterations(providers, routing, app, SUB_AGENT_MAX_ITERATIONS)
    }

    pub fn with_max_iterations(
        providers: HashMap<String, Arc<dyn LLMProvider>>,
        routing: RoutingPolicy,
        app: Arc<AppContext>,
        max_iterations: usize,
    ) -> Self {
        Self {
            providers,
            routing,
            app,
            max_iterations,
        }
    }

    /// 执行单个节点：启动 mini AgentLoop。
    ///
    /// `upstream_outputs` 是上游节点的输出，会被注入到节点 prompt 的上下文中。
    /// `dispatch_dir` 是本次 dispatch 调用专属的运行目录，用于隔离并发执行。
    /// `main_context` 是主 LLM 注入的上下文笔记，per-dispatch 传入避免并发冲突。
    pub async fn execute(
        &self,
        node: &TaskNode,
        upstream_outputs: &[(String, String)],
        dispatch_dir: &std::path::Path,
        main_context: Option<&str>,
    ) -> ExecutionRecord {
        let routing_model = self
            .routing
            .resolve(&node.node_type, node.model_override.as_deref());
        let provider = self.resolve_provider(&routing_model);
        // 用 provider 的实际模型名（如 "openai/claude-opus-4.6"）而非路由别名（如 "claude-opus"）
        let model = provider.default_model().to_string();
        let start = Instant::now();

        info!(
            node_id = %node.id,
            routing_model = %routing_model,
            model = %model,
            node_type = %node.node_type,
            "Starting sub-agent for node"
        );

        // 生成 workspace context 文件（供子 agent 读取，避免浪费轮次探索）
        // 包含当前子任务的 prompt 摘要，每次 dispatch 时更新
        let _ = std::fs::create_dir_all(dispatch_dir);
        let ctx_file = dispatch_dir.join(WORKSPACE_CONTEXT_FILENAME);
        let task_summary = &node.prompt;
        // display_workspace: LLM 看到的路径（有 sandbox → /user/work，无 → host 绝对路径）
        let display_workspace = if let Some(sb) = tyclaw_sandbox::current_sandbox() {
            sb.workspace_root().to_string()
        } else {
            self.app.workspace
                .canonicalize()
                .unwrap_or(self.app.workspace.clone())
                .display()
                .to_string()
        };
        let ctx_content = generate_workspace_context(
            &self.app.workspace,
            &display_workspace,
            task_summary,
            main_context,
        );
        // volume mount 模式下，写 host 即写容器
        let _ = std::fs::write(&ctx_file, &ctx_content);

        // 为子任务创建独立的工具注册表
        let tools = self.create_tool_registry();
        Self::log(&format!(
            "[sub-agent:{}] created tool registry, model={}",
            node.id, model
        ));

        // 创建 mini AgentLoop（无 RBAC gate、不写 snapshot）
        // 用临时空目录的 ContextBuilder，避免加载 workspace 的 GUIDELINES.md 等文件
        // sub-agent 的 system prompt 完全由 build_messages 控制
        let sub_agent_context_dir = std::env::temp_dir().join("tyclaw_subagent");
        let _ = std::fs::create_dir_all(&sub_agent_context_dir);
        let agent = AgentLoop::new(
            Arc::clone(&provider),
            tools,
            Some(model.clone()),
            Some(self.max_iterations),
        )
        // sub-agent 累计输出预算：60K 字符（约 30K tokens）。
        // 防止 GLM 等模型疯狂输出，大多数子任务 30K tokens 绰绰有余。
        .with_max_output_chars(SUB_AGENT_MAX_OUTPUT_CHARS);
        Self::log(&format!(
            "[sub-agent:{}] AgentLoop created, calling run()",
            node.id
        ));

        // 构建初始消息
        let messages = build_messages(node, upstream_outputs, &self.app.workspace, dispatch_dir);
        Self::log(&format!(
            "[sub-agent:{}] messages={} msgs, starting agent.run()",
            node.id,
            messages.len()
        ));

        // 子 agent 进度回调：缩进 + 青色竖线，与 main 形成视觉层级
        // [heartbeat] 消息通过 task_local 转发给父 agent 的通道（钉钉等）
        let node_id_for_cb = node.id.clone();
        let sub_progress: OnProgress = Box::new(move |msg: &str| {
            let node_id = node_id_for_cb.clone();
            let msg = msg.to_string();
            Box::pin(async move {
                if msg.starts_with("[heartbeat]") {
                    if let Ok(tx) = tyclaw_agent::runtime::HEARTBEAT_TX.try_with(|tx| tx.clone()) {
                        tx(msg);
                    }
                    return;
                }
                let (_, content) = parse_thinking_prefix(&msg);
                for line in content.lines() {
                    crate::term::scroll_print(
                        &format!("\x1b[2m  │ \x1b[36m[{}]\x1b[0m \x1b[2m{}\x1b[0m", node_id, line)
                    );
                }
            })
        });

        // 运行 AgentLoop（继承父 agent 的 sandbox scope）
        let dispatch_name = dispatch_dir
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "dispatch".to_string());
        let cache_scope = format!("dispatch:{dispatch_name}:node:{}", node.id);
        let result = if let Some(sandbox) = tyclaw_sandbox::current_sandbox() {
            tyclaw_sandbox::CURRENT_SANDBOX
                .scope(
                    sandbox,
                    agent.run(messages, "admin", Some(&cache_scope), Some(&sub_progress)),
                )
                .await
        } else {
            agent
                .run(messages, "admin", Some(&cache_scope), Some(&sub_progress))
                .await
        };
        Self::log(&format!("[sub-agent:{}] agent.run() returned", node.id));

        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(runtime_result) => {
                let raw_output = runtime_result.content.unwrap_or_default();
                let output_text = sanitize_sub_agent_output(&raw_output);
                let tools_used = runtime_result.tools_used;

                info!(
                    node_id = %node.id,
                    model = %model,
                    duration_ms,
                    tools_used = ?tools_used,
                    output_len = output_text.len(),
                    "Sub-agent completed successfully"
                );

                // 从子 agent messages 中提取 skill 使用记录
                let sub_skills = crate::helpers::extract_skills_used(
                    &runtime_result.messages,
                    &node.id,
                    &format!("sub:{}", node.id),
                );

                // snapshot 开启时记录完整消息历史
                let messages = if self.app.write_snapshot {
                    Some(runtime_result.messages)
                } else {
                    None
                };

                // 立即写 dispatch_dir/{node_id}.md，供下游 sub-agent 通过 read_file 按需读取
                let detail_file = dispatch_dir.join(format!("{}.md", node.id));
                let duration_s = duration_ms as f64 / 1000.0;
                let detail_content = format!(
                    "# {} (Success)\n\nModel: {}\nDuration: {:.1}s\nTools: {:?}\n\n---\n\n{}\n",
                    node.id, model, duration_s, tools_used, output_text
                );
                let _ = std::fs::write(&detail_file, &detail_content);

                ExecutionRecord {
                    node_id: node.id.clone(),
                    model,
                    input_tokens: 0, // AgentLoop 内部消耗，暂无法精确统计
                    output_tokens: 0,
                    duration_ms,
                    status: NodeStatus::Success,
                    output: Some(output_text),
                    error: None,
                    retries: 0,
                    messages,
                    tools_used,
                    tool_events: runtime_result.tool_events,
                    decision_events: runtime_result.decision_events,
                    diagnostics_summary: Some(runtime_result.diagnostics_summary),
                    skills_used: sub_skills,
                }
            }
            Err(e) => {
                warn!(
                    node_id = %node.id,
                    model = %model,
                    duration_ms,
                    error = %e,
                    "Sub-agent failed"
                );

                ExecutionRecord {
                    node_id: node.id.clone(),
                    model,
                    input_tokens: 0,
                    output_tokens: 0,
                    duration_ms,
                    status: NodeStatus::Failed,
                    output: None,
                    error: Some(e.to_string()),
                    retries: 0,
                    messages: None,
                    tools_used: Vec::new(),
                    tool_events: Vec::new(),
                    decision_events: Vec::new(),
                    diagnostics_summary: None,
                    skills_used: Vec::new(),
                }
            }
        }
    }

    /// 为子任务创建工具注册表（核心工具集，不含 AskUser/SendFile）。
    fn create_tool_registry(&self) -> ToolRegistry {
        let mut tools = ToolRegistry::new();
        // 继承父 agent 的 sandbox provider，确保子 agent 写文件到同一个容器
        tools.set_executor(std::sync::Arc::new(tyclaw_tools::FullToolExecutor::new(
            std::sync::Arc::new(tyclaw_tools::AllowAllGate),
            Some(tyclaw_sandbox::current_sandbox),
        )));
        register_core_tools(&mut tools, &self.app.workspace);
        // Web 工具：sub agent（尤其是 search 类型）也需要搜索和抓取能力
        let ws_config = tyclaw_tools::WebSearchConfig::default();
        tools.register(Box::new(tyclaw_tools::WebSearchTool::new(ws_config)));
        tools.register(Box::new(tyclaw_tools::WebFetchTool::new(None, None)));
        tools
    }

    /// 强制 flush 的日志输出（绕过 tracing non_blocking 缓冲）。
    fn log(msg: &str) {
        info!("{}", msg);
    }

    fn resolve_provider(&self, model: &str) -> Arc<dyn LLMProvider> {
        if let Some(p) = self.providers.get(model) {
            return Arc::clone(p);
        }
        if let Some(p) = self.providers.get(&self.routing.default_model) {
            warn!(%model, default = %self.routing.default_model, "Model not found in providers, using default");
            return Arc::clone(p);
        }
        self.providers
            .values()
            .next()
            .map(Arc::clone)
            .expect("No LLM providers registered")
    }
}

/// Sub-agent 输出清洗：检测并截断 reasoning 噪音。
///
/// 某些模型（如 Gemini）会把 reasoning/thinking 内容输出到 content 字段，
/// 而不是 reasoning_content 字段。当 reasoning 陷入循环（如 `(Wait, I should make sure...)`
/// 反复出现）时，content 会膨胀到数十万字符，96% 都是噪音。
///
/// 此函数检测并截断这类噪音：
/// 1. 如果输出超过 30K 字符，扫描重复短句模式
/// 2. 找到重复循环的起始点，截断到有用内容部分
/// 3. 添加截断标记
fn sanitize_sub_agent_output(raw: &str) -> String {
    const MAX_USEFUL_CHARS: usize = 30_000;

    if raw.len() <= MAX_USEFUL_CHARS {
        return raw.to_string();
    }

    // 检测重复模式：在后半部分查找连续重复的行
    // 常见噪音模式：`(Done.)`, `(Wait, I should make sure...)`
    let lines: Vec<&str> = raw.lines().collect();
    if lines.len() < 20 {
        // 行数少但字符多 → 可能是超长单行，直接截断
        let boundary = raw.floor_char_boundary(MAX_USEFUL_CHARS);
        return format!(
            "{}...\n\n[OUTPUT TRUNCATED at {} chars — original {} chars]",
            &raw[..boundary],
            MAX_USEFUL_CHARS,
            raw.len()
        );
    }

    // 从后向前找重复行的起始位置
    let mut repeat_start = None;
    let check_window = 6; // 连续 6 行重复视为噪音循环
    'outer: for i in (check_window..lines.len()).rev() {
        let window = &lines[i - check_window..i];
        // 检查窗口内是否有相同行重复 3+ 次
        let mut counts = std::collections::HashMap::new();
        for line in window {
            let trimmed = line.trim();
            if trimmed.len() > 5 {
                *counts.entry(trimmed).or_insert(0usize) += 1;
            }
        }
        if counts.values().any(|&c| c >= 3) {
            // 找到重复区域，继续向前找起始点
            repeat_start = Some(i - check_window);
            continue 'outer;
        } else if repeat_start.is_some() {
            // 重复区域结束，i 是最后一个非重复行
            repeat_start = Some(i + 1);
            break;
        }
    }

    if let Some(start_line) = repeat_start {
        // 计算截断位置（按行）
        let useful_end: usize = lines[..start_line].iter().map(|l| l.len() + 1).sum();
        let useful = &raw[..useful_end.min(raw.len())];
        let noise_lines = lines.len() - start_line;
        warn!(
            total_chars = raw.len(),
            useful_chars = useful.len(),
            noise_lines,
            "Sub-agent output contains reasoning loop noise — truncating"
        );
        format!(
            "{}\n\n[REASONING NOISE TRUNCATED — removed {} lines of repetitive content. \
                 Original output was {} chars, kept {} chars of useful content.]",
            useful.trim_end(),
            noise_lines,
            raw.len(),
            useful.len()
        )
    } else {
        // 没检测到重复模式，但仍然超长，做简单截断
        let boundary = raw.floor_char_boundary(MAX_USEFUL_CHARS);
        format!(
            "{}...\n\n[OUTPUT TRUNCATED at {} chars — original {} chars]",
            &raw[..boundary],
            MAX_USEFUL_CHARS,
            raw.len()
        )
    }
}

/// workspace context 文件路径（放在 .dispatch/ 目录下，与 dispatch results 统一管理）。
const WORKSPACE_CONTEXT_FILENAME: &str = "main_llm.md";

/// 生成 workspace context 文件：包含当前任务描述、工作目录和相关文件列表。
/// 每次 dispatch 前调用并更新，子 agent 可通过 read_file 获取完整信息。
fn generate_workspace_context(
    workspace: &std::path::Path,
    display_workspace: &str,
    task_summary: &str,
    main_context: Option<&str>,
) -> String {
    let mut ctx = String::new();

    ctx.push_str("# Main LLM Context\n\n");

    // 主 LLM 的关键上下文笔记（最重要，放在最前面）
    if let Some(notes) = main_context {
        if !notes.is_empty() {
            ctx.push_str("## Key Context from Main LLM\n");
            ctx.push_str("⚠️ **COMPLETE SPEC — DO NOT RE-EXPLORE.** The following contains ALL findings, rules, and constraints \
                from the main LLM's research phase. This information is authoritative and verified. \
                Use it directly — do not spend iterations reading source files or running exploratory exec to re-discover what is already here.\n\n");
            ctx.push_str(notes);
            ctx.push_str("\n\n");
        }
    }

    // 当前任务描述
    if !task_summary.is_empty() {
        ctx.push_str("## Current Task\n");
        ctx.push_str(task_summary);
        ctx.push_str("\n\n");
    }

    ctx.push_str(&format!(
        "## Working Directory\n`{}`\n\n",
        display_workspace
    ));
    ctx.push_str(&format!(
        "**All file paths in this document are relative to the working directory above. \
        When writing files, use paths relative to `{}`.**\n\n",
        display_workspace
    ));

    // 只列出相关的顶层目录和文件，跳过构建/运行时产物
    const SKIP_ENTRIES: &[&str] = &[
        "target",
        "node_modules",
        "sessions",
        "audit",
        "cases",
        "logs",
        "memory",
        "crates",
        "openspec",
        "__pycache__",
    ];
    ctx.push_str("## Project Structure\n```\n");
    if let Ok(entries) = std::fs::read_dir(workspace) {
        let mut items: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if SKIP_ENTRIES.contains(&name.as_str()) {
                continue;
            }
            // 跳过 backup 目录（*.back, *.bak 等）
            if name.contains(".back") || name.contains(".bak") {
                continue;
            }
            let kind = if entry.path().is_dir() {
                "dir "
            } else {
                "file"
            };
            items.push(format!("  {kind}  {name}"));
        }
        items.sort();
        ctx.push_str(&items.join("\n"));
    }
    ctx.push_str("\n```\n\n");

    // workspace 私有 skills：在 sandbox 模式下位于 /user/skills，host 模式下通过 workspace_key 定位
    // sub-agent 通过 sandbox 挂载自动看到当前 workspace 的 skills 目录
    if let Some(sandbox) = tyclaw_sandbox::current_sandbox() {
        let ws_key = sandbox.id().strip_prefix("tyclaw-").unwrap_or(sandbox.id());
        let ws_skills = tyclaw_control::workspace_path(workspace, ws_key).join("skills");
        if ws_skills.is_dir() {
            ctx.push_str("## Workspace Skills\n");
            if let Ok(skill_entries) = std::fs::read_dir(&ws_skills) {
                for skill_entry in skill_entries.flatten() {
                    let skill_path = skill_entry.path();
                    if !skill_path.is_dir()
                        || skill_entry.file_name().to_string_lossy().starts_with('.')
                    {
                        continue;
                    }
                    let skill_name = skill_entry.file_name().to_string_lossy().to_string();
                    let display_skill_path = format!("skills/{skill_name}");
                    ctx.push_str(&format!("### {skill_name}\n"));
                    ctx.push_str(&format!("Path: `{display_skill_path}`\n```\n"));
                    if let Ok(files) = std::fs::read_dir(&skill_path) {
                        let mut file_items: Vec<String> = files
                            .flatten()
                            .filter_map(|f| {
                                let fname = f.file_name().to_string_lossy().to_string();
                                if fname.starts_with('.') || fname == "__pycache__" || fname == "tmp" {
                                    return None;
                                }
                                let desc = match fname.as_str() {
                                    "SKILL.md" => " # Skill 文档",
                                    "tool.py" => " # 工具脚本",
                                    _ if fname.ends_with(".py") => " # Python 脚本",
                                    _ => "",
                                };
                                Some(format!("├── {fname}{desc}"))
                            })
                            .collect();
                        file_items.sort();
                        if let Some(last) = file_items.last_mut() {
                            *last = last.replacen("├──", "└──", 1);
                        }
                        ctx.push_str(&file_items.join("\n"));
                    }
                    ctx.push_str("\n```\n\n");
                }
            }
        }
    }

    // 只扫描包含数据文件的工作目录（跳过代码/构建/运行时目录）
    const SKIP_DATA_SCAN: &[&str] = &[
        "target",
        "node_modules",
        "crates",
        "src",
        "tests",
        "docs",
        "skills",
        "sessions",
        "audit",
        "cases",
        "memory",
        "logs",
        "openspec",
        "config",
        "tools",
        "tmp",
    ];
    if let Ok(entries) = std::fs::read_dir(workspace) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !entry.path().is_dir() {
                continue;
            }
            if SKIP_DATA_SCAN.contains(&name.as_str()) || name == "users" {
                continue;
            }
            if name.contains(".back") || name.contains(".bak") {
                continue;
            }
            // 只列出含数据文件的目录
            let data_files: Vec<String> = std::fs::read_dir(entry.path())
                .into_iter()
                .flat_map(|rd| rd.flatten())
                .filter(|s| {
                    let n = s.file_name().to_string_lossy().to_lowercase();
                    !n.starts_with('.')
                        && (n.ends_with(".xlsx")
                            || n.ends_with(".csv")
                            || n.ends_with(".json")
                            || n.ends_with(".txt")
                            || n.ends_with(".xls")
                            || n.ends_with(".py"))
                })
                .map(|s| {
                    let n = s.file_name().to_string_lossy().to_string();
                    let size = s.metadata().map(|m| m.len()).unwrap_or(0);
                    if size > 0 {
                        format!("  {} ({})", n, humanize_bytes(size))
                    } else {
                        format!("  {}", n)
                    }
                })
                .collect();
            if !data_files.is_empty() {
                ctx.push_str(&format!("## {name}/\n```\n"));
                let mut sorted = data_files;
                sorted.sort();
                ctx.push_str(&sorted.join("\n"));
                ctx.push_str("\n```\n\n");
            }
        }
    }

    ctx
}

fn humanize_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    if bytes < 1024 * 1024 {
        return format!("{:.1}KB", bytes as f64 / 1024.0);
    }
    format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
}

/// 根据 node_type 生成差异化的 system prompt。
///
/// 提示词从 config/prompts/ 目录加载（优先文件，回退到内置默认值）。
/// 不在 system prompt 中列举工具名——工具定义已通过 tools 参数传递给 LLM。
fn system_prompt_for_node_type(node_type: &str) -> String {
    use super::prompt_loader;

    let role = prompt_loader::node_type_prompt(node_type);
    // coding 类用完整 guidelines（含验证、调查规则），其他类型用轻量版
    let is_coding = matches!(node_type, "coding" | "coding_deep");
    let guidelines = if is_coding {
        prompt_loader::sub_agent_guidelines_coding()
    } else {
        prompt_loader::sub_agent_guidelines()
    }
    .replace("{max_iterations}", &SUB_AGENT_MAX_ITERATIONS.to_string());
    let execution_baseline = prompt_loader::subagent_execution_baseline();
    format!("{role}\n\n{execution_baseline}\n\n{guidelines}")
}

fn build_messages(
    node: &TaskNode,
    upstream_outputs: &[(String, String)],
    workspace: &std::path::Path,
    dispatch_dir: &std::path::Path,
) -> Vec<HashMap<String, Value>> {
    // 统一路径变量：有 sandbox → 容器路径，无 → host 绝对路径
    let (display_ws, display_ctx) = if let Some(sandbox) = tyclaw_sandbox::current_sandbox() {
        let ws = sandbox.workspace_root().to_string();
        let dispatch_rel = dispatch_container_rel(dispatch_dir);
        let ctx = format!("{}/{}/{}", ws, dispatch_rel, WORKSPACE_CONTEXT_FILENAME);
        (ws, ctx)
    } else {
        let abs = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        let abs_dispatch = dispatch_dir
            .canonicalize()
            .unwrap_or_else(|_| dispatch_dir.to_path_buf());
        (
            abs.display().to_string(),
            abs_dispatch
                .join(WORKSPACE_CONTEXT_FILENAME)
                .display()
                .to_string(),
        )
    };

    let ws_hint = super::prompt_loader::workspace_hint()
        .replace("{workspace}", &display_ws)
        .replace("{context_file}", &display_ctx);

    // user prompt：上游 context + 任务指令 + 验收标准
    let mut parts = Vec::new();

    if !upstream_outputs.is_empty() {
        parts.push("=== Context from upstream tasks ===".to_string());
        for (id, output) in upstream_outputs {
            let detail_path = if tyclaw_sandbox::current_sandbox().is_some() {
                format!(
                    "{}/{}/{}.md",
                    display_ws,
                    dispatch_container_rel(dispatch_dir),
                    id
                )
            } else {
                dispatch_dir.join(format!("{id}.md")).display().to_string()
            };
            // 上游完整输出已写入 dispatch_dir/{id}.md，这里只注入摘要 + 文件路径。
            // Sub-agent 可通过 read_file 按需读取完整内容。
            if output.len() > 2000 {
                let boundary = output.floor_char_boundary(800);
                let hint = super::prompt_loader::upstream_truncated_hint(&detail_path);
                parts.push(format!(
                    "[{id}] ({total} chars):\n{preview}...\n\n{hint}",
                    total = output.len(),
                    preview = &output[..boundary],
                ));
            } else {
                let hint = super::prompt_loader::upstream_full_hint(&detail_path);
                parts.push(format!(
                    "[{id}]:\n{output}\n\n{hint}"
                ));
            }
        }
        parts.push("=== End of context ===\n".to_string());
    }

    parts.push(node.prompt.clone());

    if let Some(ref criteria) = node.acceptance_criteria {
        parts.push(format!("\nAcceptance criteria: {criteria}"));
    }

    let context = ContextBuilder::new(workspace);
    let planned = PlannedPromptContext {
        system_prompt_parts: vec![
            system_prompt_for_node_type(&node.node_type),
            "[[CACHE_BOUNDARY]]".to_string(),
        ],
        user_context: vec![PromptContextEntry {
            label: "Workspace Hint".to_string(),
            content: ws_hint,
        }],
        system_context: Vec::new(),
    };

    context.assemble_messages(&planned, &[], &parts.join("\n"))
}

fn dispatch_container_rel(dispatch_dir: &std::path::Path) -> String {
    let parent = dispatch_dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "dispatches".to_string());
    let name = dispatch_dir
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "dispatch".to_string());
    format!("{parent}/{name}")
}
