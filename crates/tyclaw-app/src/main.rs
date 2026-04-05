//! TyClaw V2 —— Rust 版本的程序入口点。
//!
//! 支持两种运行模式：
//! - CLI 模式（默认）：交互式命令行
//! - 钉钉模式（--dingtalk）：钉钉 Stream 机器人
//!
//! 配置优先级（从高到低）：命令行参数 > 环境变量 > config.yaml > 默认值

use clap::Parser;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::info;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use tyclaw_channel::{
    dingtalk::TokenManager, ChatbotHandler, ChatbotMessage, CliChannel, Credential, DingTalkBot,
    DingTalkStreamClient, GatewayClient,
};
use tyclaw_orchestration::subtasks::SubtasksConfig;
use tyclaw_orchestration::{
    load_yaml, mask_secret, BaseConfig, BusHandle, InboundMessage, LoggingConfig, MessageBus,
    Orchestrator, OutboundEvent, WorkspaceConfig,
};
use tyclaw_provider::OpenAICompatProvider;

// ── 配置文件结构定义 ──────────────────────────────────────────

/// App 专有配置（config/app.yaml），包含钉钉等服务端配置。
#[derive(Debug, Default, Deserialize)]
struct AppConfig {
    #[serde(default)]
    dingtalk: DingTalkConfig,
}

/// 钉钉配置。
#[derive(Debug, Default, Deserialize)]
struct DingTalkConfig {
    client_id: Option<String>,
    client_secret: Option<String>,
    /// Gateway WebSocket URL（如 "ws://gateway-host:9100"）。不配则直连钉钉 Stream API。
    gateway_url: Option<String>,
}

fn print_effective_config(
    config_path: &Path,
    mode: &str,
    workspace: &Path,
    api_key: &str,
    api_base: &str,
    model: &str,
    max_iterations: usize,
    context_window_tokens: Option<usize>,
    snapshot: bool,
    logging: &LoggingConfig,
    dingtalk: &DingTalkConfig,
    workspaces: &HashMap<String, WorkspaceConfig>,
    subtasks: &SubtasksConfig,
) {
    println!("\n=== TyClaw.rs Effective Config ===");
    println!("mode: {mode}");
    println!("workspace: {}", workspace.display());
    println!("config_file: {}", config_path.display());
    println!("llm.model: {model}");
    println!("llm.api_base: {api_base}");
    println!("llm.api_key: {}", mask_secret(api_key));
    println!("llm.max_iterations: {max_iterations}");
    println!(
        "llm.context_window_tokens: {}",
        context_window_tokens
            .map(|v| v.to_string())
            .unwrap_or_else(|| "<default>".into())
    );
    println!("llm.snapshot: {snapshot}");
    println!(
        "logging.level: {}",
        if logging.level.trim().is_empty() {
            "info"
        } else {
            &logging.level
        }
    );
    println!(
        "logging.file: {}",
        logging
            .file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<workspace>/logs/tyclaw.log".into())
    );
    println!("workspaces.count: {}", workspaces.len());
    if !workspaces.is_empty() {
        let mut ids: Vec<&String> = workspaces.keys().collect();
        ids.sort();
        println!(
            "workspaces.ids: {}",
            ids.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if mode == "dingtalk" {
        println!(
            "dingtalk.client_id: {}",
            dingtalk
                .client_id
                .as_deref()
                .map(mask_secret)
                .unwrap_or_else(|| "<empty>".into())
        );
        println!(
            "dingtalk.client_secret: {}",
            dingtalk
                .client_secret
                .as_deref()
                .map(mask_secret)
                .unwrap_or_else(|| "<empty>".into())
        );
        println!(
            "dingtalk.gateway_url: {}",
            dingtalk
                .gateway_url
                .as_deref()
                .unwrap_or("<none, direct stream>")
        );
    }
    println!("subtasks.enabled: {}", subtasks.enabled);
    if subtasks.enabled {
        println!(
            "subtasks.planner_model: {}",
            subtasks.planner_model.as_deref().unwrap_or("<default>")
        );
        println!("subtasks.max_concurrency: {}", subtasks.max_concurrency);
        println!("subtasks.failure_policy: {:?}", subtasks.failure_policy);
        println!("subtasks.routing_rules: {}", subtasks.routing_rules.len());
        println!("subtasks.providers: {}", subtasks.providers.len());
        for (name, pcfg) in &subtasks.providers {
            println!(
                "  {}: endpoint={}, model={}",
                name,
                pcfg.endpoint,
                pcfg.model.as_deref().unwrap_or(name)
            );
        }
    }
    println!("===============================\n");
}

/// 初始化日志：统一写入文件，避免 CLI 交互与日志混杂。
fn init_logging(workspace: &Path, logging: &LoggingConfig) -> WorkerGuard {
    let log_path = logging
        .file
        .clone()
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                workspace.join(path)
            }
        })
        .unwrap_or_else(|| workspace.join("logs").join("tyclaw.log"));
    let parent = log_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("logs"));
    std::fs::create_dir_all(&parent).ok();

    let file_name = log_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("tyclaw.log")
        .to_string();

    let file_appender = tracing_appender::rolling::never(parent, file_name);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let level = if logging.level.trim().is_empty() {
        "info".to_string()
    } else {
        logging.level.to_lowercase()
    };
    let filter = EnvFilter::try_new(level.clone()).unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(non_blocking)
        .with_timer(tracing_subscriber::fmt::time::OffsetTime::new(
            time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC),
            time::macros::format_description!(
                "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3][offset_hour sign:mandatory]:[offset_minute]"
            ),
        ))
        .init();

    info!(log_file = %log_path.display(), log_level = %level, "Logging initialized");
    guard
}

// ── 命令行参数定义 ──────────────────────────────────────────

/// 命令行参数定义。
#[derive(Parser)]
#[command(
    name = "tyclaw",
    about = "TyClaw.rs — AI Agent for Enterprise Automation"
)]
struct Args {
    /// 运行时根目录。所有运行时与配置都从该目录统一解析。
    #[arg(short = 'r', long = "run-dir")]
    run_dir: PathBuf,

    /// API 密钥（也可通过 OPENAI_API_KEY 环境变量设置）
    #[arg(long, env = "OPENAI_API_KEY")]
    api_key: Option<String>,

    /// API 基础 URL（也可通过 OPENAI_API_BASE 环境变量设置）
    #[arg(long, env = "OPENAI_API_BASE")]
    api_base: Option<String>,

    /// 模型名称
    #[arg(short, long)]
    model: Option<String>,

    /// ReAct 循环最大迭代次数
    #[arg(long)]
    max_iterations: Option<usize>,

    /// 上下文窗口大小（token 数，用于记忆合并决策）
    #[arg(long = "context-window-tokens")]
    context_window_tokens: Option<usize>,

    /// 自定义 works 目录路径（覆盖默认的 {workspace}/works）。
    /// 用于兼容老数据目录或外挂存储。
    #[arg(long = "works-dir")]
    works_dir: Option<PathBuf>,

    /// 是否以钉钉模式启动
    #[arg(long)]
    dingtalk: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let mode = if args.dingtalk { "dingtalk" } else { "cli" };
    let workspace_root = canonicalize_workspace(&args.run_dir);
    let config_path = workspace_root.join("config").join("config.yaml");
    let cfg: BaseConfig = load_yaml(&config_path);
    let app_cfg: AppConfig = load_yaml(&config_path); // 同一文件，只解析 dingtalk 段
    let _log_guard = init_logging(&workspace_root, &cfg.logging);

    info!(?config_path, "Loaded config (or defaults)");

    // 配置优先级：命令行参数 > 环境变量 > providers[name] > llm 内联字段 > 默认值
    //
    // 新格式（推荐）：llm.provider 引用全局 providers 中的名字
    // 旧格式（兼容）：llm.api_key / api_base / model 内联
    let (api_key, api_base, model, thinking) =
        if let Some(ref provider_name) = args.api_key.as_ref().map(|_| None::<String>).unwrap_or(cfg.llm.provider.clone()) {
            // 新格式：从全局 providers 解析
            let pcfg = cfg.providers.get(provider_name).unwrap_or_else(|| {
                eprintln!(
                    "Error: llm.provider = \"{provider_name}\" not found in [providers] section.\n\
                     Available providers: {:?}",
                    cfg.providers.keys().collect::<Vec<_>>()
                );
                std::process::exit(1);
            });
            let api_key = pcfg.api_key.clone().unwrap_or_else(|| {
                eprintln!("Error: providers.{provider_name}.api_key is required.");
                std::process::exit(1);
            });
            let model = pcfg.model.clone().unwrap_or_else(|| provider_name.clone());
            let thinking = if pcfg.thinking_enabled {
                Some(tyclaw_provider::ThinkingConfig {
                    effort: pcfg.thinking_effort.clone(),
                    budget_tokens: pcfg.thinking_budget_tokens,
                })
            } else {
                None
            };
            (api_key, pcfg.endpoint.clone(), model, thinking)
        } else {
            // 旧格式 / CLI 覆盖：内联 api_key / api_base / model
            let api_key = args.api_key.or(cfg.llm.api_key).unwrap_or_else(|| {
                eprintln!(
                    "Error: API key required.\n\
                     Set llm.provider (recommended), llm.api_key, --api-key flag, or OPENAI_API_KEY env var."
                );
                std::process::exit(1);
            });
            let api_base = args
                .api_base
                .or(cfg.llm.api_base)
                .unwrap_or_else(|| "https://api.openai.com/v1".into());
            let model = args
                .model
                .or(cfg.llm.model)
                .unwrap_or_else(|| "gpt-4o".into());
            let thinking = if cfg.llm.thinking_enabled {
                Some(tyclaw_provider::ThinkingConfig {
                    effort: cfg.llm.thinking_effort.clone(),
                    budget_tokens: cfg.llm.thinking_budget_tokens,
                })
            } else {
                None
            };
            (api_key, api_base, model, thinking)
        };

    let max_iterations = args.max_iterations.unwrap_or(cfg.llm.max_iterations);
    let context_window = args.context_window_tokens.or(cfg.llm.context_window_tokens);
    let snapshot = cfg.llm.snapshot;

    print_effective_config(
        &config_path,
        mode,
        &workspace_root,
        &api_key,
        &api_base,
        &model,
        max_iterations,
        context_window,
        snapshot,
        &cfg.logging,
        &app_cfg.dingtalk,
        &cfg.workspaces,
        &cfg.subtasks,
    );

    info!(
        model = %model,
        api_base = %api_base,
        max_iterations,
        mode = mode,
        "Starting TyClaw.rs"
    );

    // 创建主控 LLM 提供者（Arc 共享给 Orchestrator 和 AgentLoop）
    let mut provider_impl = OpenAICompatProvider::new(&api_key, &api_base, &model, thinking);
    provider_impl.set_snapshot_dir(
        workspace_root
            .join("logs")
            .join("snap")
            .join("llm_requests"),
    );
    let provider: Arc<dyn tyclaw_provider::LLMProvider> = Arc::new(provider_impl);

    let run_config = RunConfig {
        provider,
        workspace: workspace_root,
        works_dir: args.works_dir.map(|p| canonicalize_workspace(&p)),
        model,
        max_iterations,
        context_window,
        write_snapshot: snapshot,
        workspaces: cfg.workspaces,
        subtasks_config: {
            // 将全局 providers 合并到 subtasks.providers（全局为底，subtasks 局部覆盖）
            let mut sc = cfg.subtasks;
            for (name, pcfg) in &cfg.providers {
                sc.providers.entry(name.clone()).or_insert_with(|| pcfg.clone());
            }
            sc
        },
        web_search_config: cfg.web_search,
        control_config: cfg.control,
        workspace_config: cfg.workspace,
    };

    if args.dingtalk {
        run_hybrid(run_config, app_cfg.dingtalk).await;
    } else {
        run_cli(run_config).await;
    }
}

fn canonicalize_workspace(path: &Path) -> PathBuf {
    if let Err(e) = std::fs::create_dir_all(path) {
        eprintln!(
            "Warning: Failed to create workspace {}: {e}",
            path.display()
        );
    }
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 创建 TimerService，返回 (timer_arc, timer_rx)。Timer 永远启用。
/// 按 workspace 隔离存储：works/{bucket}/{key}/timer_jobs.json
fn create_timer_service(
    workspace: &Path,
) -> (
    Arc<tyclaw_tools::timer::TimerService>,
    tokio::sync::mpsc::Receiver<tyclaw_tools::timer::TimerJob>,
) {
    let (svc, rx) = tyclaw_tools::timer::TimerService::new(workspace);
    info!(root = %workspace.display(), "Timer service created (per-workspace storage)");
    (svc, rx)
}

/// 运行时配置聚合（减少 run_cli/run_hybrid 的参数数量）。
struct RunConfig {
    provider: Arc<dyn tyclaw_provider::LLMProvider>,
    workspace: PathBuf,
    /// 自定义 works 目录（--works-dir），None 则用 {workspace}/works
    works_dir: Option<PathBuf>,
    model: String,
    max_iterations: usize,
    context_window: Option<usize>,
    write_snapshot: bool,
    workspaces: HashMap<String, WorkspaceConfig>,
    subtasks_config: SubtasksConfig,
    web_search_config: tyclaw_tools::WebSearchConfig,
    control_config: tyclaw_orchestration::ControlConfig,
    workspace_config: tyclaw_orchestration::WorkspaceRuntimeConfig,
}

impl RunConfig {
    fn build_orchestrator(
        self,
        timer_svc: Arc<tyclaw_tools::timer::TimerService>,
    ) -> Orchestrator {
        let mut orch = Orchestrator::builder(self.provider, &self.workspace)
            .with_model(self.model)
            .with_max_iterations(self.max_iterations)
            .with_context_window_tokens_opt(self.context_window)
            .with_write_snapshot(self.write_snapshot)
            .with_workspaces_config(self.workspaces)
            .with_workspace_key_strategy(self.workspace_config.key_strategy.clone())
            .with_subtasks(self.subtasks_config)
            .with_web_search(self.web_search_config)
            .with_control(self.control_config)
            .with_timer(timer_svc)
            .build();
        if let Some(works_dir) = self.works_dir {
            orch.set_works_dir(works_dir);
        }
        orch
    }
}

/// 以 CLI 模式运行。
async fn run_cli(config: RunConfig) {
    let idle_timeout_secs = config.workspace_config.idle_timeout_secs;
    let (timer_svc, timer_rx) = create_timer_service(&config.workspace);

    let mut orchestrator = config.build_orchestrator(timer_svc.clone());

    // Docker Sandbox 初始化（CLI 模式下可选，无沙箱时工具直接在宿主机执行）
    let ws_root = orchestrator.app().workspace.clone();
    match tyclaw_sandbox::DockerPool::new(tyclaw_sandbox::DockerConfig::default(), ws_root).await
    {
        Ok(pool) => {
            info!("Docker sandbox pool initialized");
            orchestrator.set_sandbox_pool(pool as Arc<dyn tyclaw_sandbox::SandboxPool>);
        }
        Err(e) => {
            tracing::warn!(error = %e, "Docker not available — tool commands will run directly on host WITHOUT sandbox isolation");
        }
    };

    timer_svc.start().await;

    let orchestrator = Arc::new(orchestrator);

    // 启动 workspace 超时回收后台任务
    if idle_timeout_secs > 0 {
        orchestrator.spawn_reaper(idle_timeout_secs, 60);
    }

    let (bus, bus_handle, outbound_rx) = MessageBus::new(Arc::clone(&orchestrator), 64, 256);

    spawn_timer_consumer(timer_rx, bus_handle.clone());

    let dispatcher_handle = tokio::spawn(run_outbound_dispatcher(outbound_rx, None));
    let bus_handle_task = tokio::spawn(bus.run());

    let cli = CliChannel::new("cli_user", "default");
    cli.run(bus_handle, timer_svc.as_ref()).await;

    drop(dispatcher_handle);
    drop(bus_handle_task);
    timer_svc.stop();
}

/// 钉钉发送器配置（可选，hybrid 模式下传入）。
struct DingTalkSender {
    http_client: reqwest::Client,
    token_manager: TokenManager,
    robot_code: String,
}

/// Outbound Dispatcher：消费 outbound 事件，CLI 打印到 stdout，钉钉通过 API 发出。
async fn run_outbound_dispatcher(
    mut outbound_rx: tokio::sync::mpsc::Receiver<OutboundEvent>,
    dt_sender: Option<DingTalkSender>,
) {
    use std::io::Write;
    use tyclaw_channel::dingtalk::handler;

    while let Some(event) = outbound_rx.recv().await {
        let (channel, chat_id) = match &event {
            OutboundEvent::Progress {
                channel, chat_id, ..
            }
            | OutboundEvent::Thinking {
                channel, chat_id, ..
            }
            | OutboundEvent::Reply {
                channel, chat_id, ..
            }
            | OutboundEvent::Error {
                channel, chat_id, ..
            } => (channel.clone(), chat_id.clone()),
        };
        let is_dt = channel.starts_with("dingtalk");

        // 钉钉通道的 Reply/Error 通过钉钉 API 发出
        if is_dt {
            if let Some(ref sender) = dt_sender {
                match &event {
                    OutboundEvent::Reply { response, .. } => {
                        // chat_id 格式：群聊 "conversation_id:staff_id"，私聊 "staff_id"
                        let (conversation_id, user_id) = if chat_id.contains(':') {
                            let parts: Vec<&str> = chat_id.splitn(2, ':').collect();
                            (parts[0], parts[1])
                        } else {
                            ("", chat_id.as_str())
                        };
                        if let Ok(token) = sender.token_manager.get_token().await {
                            handler::send_text_by_channel(
                                &sender.http_client,
                                &token,
                                &sender.robot_code,
                                &channel,
                                user_id,
                                conversation_id,
                                &response.text,
                            )
                            .await;
                        }
                        // 同时在 CLI 打印（方便调试）
                        println!("\n\x1b[2m[DT:{chat_id}]\x1b[0m {}\n", response.text);
                    }
                    OutboundEvent::Error { message, .. } => {
                        eprintln!("\n\x1b[31m[DT:{chat_id}] Error: {message}\x1b[0m\n");
                    }
                    OutboundEvent::Progress { message, .. } => {
                        if message.contains("[sandbox]") || message.contains("[已创建任务") {
                            println!("\x1b[32m[DT:{chat_id}] {message}\x1b[0m");
                        } else {
                            println!("\x1b[2m[DT:{chat_id}] {message}\x1b[0m");
                        }
                    }
                    OutboundEvent::Thinking { content, .. } => {
                        println!("\x1b[2m[DT:{chat_id}] [Thinking] {content}\x1b[0m");
                    }
                }
                print!("You> ");
                let _ = std::io::stdout().flush();
                continue;
            }
        }

        // CLI 通道：轮次信息白色，其余灰色
        match &event {
            OutboundEvent::Progress { message, .. } => {
                if message.starts_with("[Thinking]") {
                    let content = message.strip_prefix("[Thinking]\n").unwrap_or(message);
                    println!("\x1b[2m[Thinking] {content}\x1b[0m");
                } else if message.contains("[sandbox]") || message.contains("[已创建任务") {
                    // sandbox 容器操作：绿色
                    println!("\x1b[32m{message}\x1b[0m");
                } else if message.contains("[轮次") || message.contains("[Token]") {
                    // 轮次和 Token 统计：白色（默认色）
                    println!("{message}");
                } else if message.starts_with('[') {
                    // 已有前缀的系统消息（[dispatch]、[scheduler] 等）：灰色原样输出
                    println!("\x1b[2m{message}\x1b[0m");
                } else {
                    // 主 LLM 的 content 输出：灰色，加 [main] 前缀，与 [sub:xxx] 区分
                    println!("\x1b[2m[main] {message}\x1b[0m");
                }
            }
            OutboundEvent::Thinking { content, .. } => {
                println!("\x1b[2m[Thinking] {content}\x1b[0m");
            }
            OutboundEvent::Reply { response, .. } => {
                println!("\nTyClaw.rs> {}\n", response.text);
                if !response.tools_used.is_empty() {
                    println!(
                        "\x1b[2m(tools: {} | {:.1}s)\x1b[0m\n",
                        response.tools_used.join(", "),
                        response.duration_seconds
                    );
                }
                if !response.output_files.is_empty() {
                    println!("\x1b[33m[输出文件]\x1b[0m");
                    for f in &response.output_files {
                        println!("  → {f}");
                    }
                    println!();
                }
                if !is_dt {
                    print!("You> ");
                    let _ = std::io::stdout().flush();
                }
            }
            OutboundEvent::Error { message, .. } => {
                eprintln!("\x1b[31mError: {message}\x1b[0m");
                if !is_dt {
                    print!("You> ");
                    let _ = std::io::stdout().flush();
                }
            }
        }
    }
}

/// 统一 Timer 消费 task：将 TimerJob 转换为 InboundMessage 推入 Bus。
fn spawn_timer_consumer(
    mut timer_rx: tokio::sync::mpsc::Receiver<tyclaw_tools::timer::TimerJob>,
    bus_handle: BusHandle,
) {
    tokio::spawn(async move {
        while let Some(job) = timer_rx.recv().await {
            info!(job_id = %job.id, name = %job.name, "Timer: dispatching job to bus");
            let msg = InboundMessage {
                content: format!("[Scheduled Task: {}] {}", job.name, job.payload.message),
                user_id: job.payload.user_id.clone(),
                workspace_id: job.payload.workspace_id.unwrap_or_else(|| "default".into()),
                channel: job.payload.channel.unwrap_or_else(|| "cli".into()),
                chat_id: job.payload.chat_id.unwrap_or_else(|| "direct".into()),
                conversation_id: job.payload.conversation_id.clone(),
                images: vec![],
                files: vec![],
                reply_tx: None,
                is_timer: true,
            };
            if let Err(e) = bus_handle.send(msg).await {
                tracing::error!(job_id = %job.id, error = %e, "Timer: failed to send to bus");
            }
        }
    });
}

/// 混合模式：同时运行 DingTalk Stream + CLI REPL。
///
/// 共享同一个 Orchestrator 和 MessageBus。
/// DingTalk 消息通过 Stream 收发，CLI 消息通过 stdin/stdout。
/// CLI 退出时整个进程退出。
async fn run_hybrid(config: RunConfig, dt_config: DingTalkConfig) {
    let idle_timeout_secs = config.workspace_config.idle_timeout_secs;
    let DingTalkConfig {
        client_id,
        client_secret,
        gateway_url,
    } = dt_config;

    let client_id = client_id.unwrap_or_else(|| {
        eprintln!(
            "Error: DingTalk client_id required.\n\
             Set in config.yaml (dingtalk.client_id)."
        );
        std::process::exit(1);
    });
    let client_secret = client_secret.unwrap_or_else(|| {
        eprintln!(
            "Error: DingTalk client_secret required.\n\
             Set in config.yaml (dingtalk.client_secret)."
        );
        std::process::exit(1);
    });

    let (timer_svc, timer_rx) = create_timer_service(&config.workspace);
    let mut orchestrator = config.build_orchestrator(timer_svc.clone());

    // Docker Sandbox 初始化（DingTalk 多用户模式必须有沙箱隔离）
    let ws_root = orchestrator.app().workspace.clone();
    match tyclaw_sandbox::DockerPool::new(tyclaw_sandbox::DockerConfig::default(), ws_root).await
    {
        Ok(pool) => {
            info!("Docker sandbox pool initialized (hybrid mode)");
            orchestrator.set_sandbox_pool(pool as Arc<dyn tyclaw_sandbox::SandboxPool>);
        }
        Err(e) => {
            eprintln!(
                "FATAL: Docker not available ({e}).\n\
                 DingTalk mode requires Docker sandbox for multi-user isolation.\n\
                 Please install Docker and ensure the sandbox image is built."
            );
            std::process::exit(1);
        }
    };

    let credential = Credential::new(&client_id, &client_secret);
    let token_manager = TokenManager::new(credential.clone());

    let workspace_path = orchestrator.app().workspace.clone();

    timer_svc.start().await;

    let orchestrator = Arc::new(orchestrator);

    // 启动 workspace 超时回收后台任务
    if idle_timeout_secs > 0 {
        orchestrator.spawn_reaper(idle_timeout_secs, 60);
    }

    let (bus, bus_handle, outbound_rx) = MessageBus::new(Arc::clone(&orchestrator), 64, 256);

    spawn_timer_consumer(timer_rx, bus_handle.clone());
    tokio::spawn(bus.run());

    // Outbound dispatcher：CLI 打印到 stdout，钉钉通过 API 发出
    let dt_sender = DingTalkSender {
        http_client: reqwest::Client::new(),
        token_manager: token_manager.clone(),
        robot_code: client_id.clone(),
    };
    tokio::spawn(run_outbound_dispatcher(outbound_rx, Some(dt_sender)));

    // 启动消息接收（后台运行）
    let bot = DingTalkBot::new(
        bus_handle.clone(),
        token_manager,
        &client_id,
        workspace_path,
    );

    if let Some(gw_url) = gateway_url {
        // Gateway 模式：通过 dingtalk-gateway 中转
        let gateway_client = GatewayClient::new(gw_url, bot as Arc<dyn ChatbotHandler>);
        tokio::spawn(async move {
            info!("Gateway client starting (hybrid mode)...");
            gateway_client.start_forever().await;
        });
    } else {
        // 直连模式：直接连钉钉 Stream API
        let stream_client = DingTalkStreamClient::new(credential);
        stream_client
            .register_handler(ChatbotMessage::TOPIC, bot as Arc<dyn ChatbotHandler>)
            .await;

        tokio::spawn(async move {
            info!("DingTalk stream client starting (hybrid mode)...");
            stream_client.start_forever().await;
        });
    };

    // 有终端时启动 CLI REPL（前台），否则只跑 DingTalk + Timer（后台服务模式）
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        info!("CLI REPL starting (hybrid mode)...");
        let cli = CliChannel::new("cli_user", "default");
        cli.run(bus_handle, timer_svc.as_ref()).await;
        timer_svc.stop();
    } else {
        info!("No terminal detected, running as background service (DingTalk + Timer only)");
        // 等待 Ctrl+C 信号退出
        tokio::signal::ctrl_c().await.ok();
        info!("Received SIGINT, shutting down...");
        timer_svc.stop();
    }
}
