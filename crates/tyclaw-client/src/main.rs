//! TyClaw Client —— 轻量级独立 CLI 客户端。
//!
//! 与 tyclaw-app 共享编排层（Orchestrator），具备完整的上下文能力
//! （技能、案例记忆、会话历史），但不初始化 Docker 沙箱。
//!
//! 配置优先级：命令行参数 > 环境变量 > config.yaml > 默认值

use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use tyclaw_orchestration::{
    load_yaml, mask_secret, parse_thinking_prefix, BaseConfig, LoggingConfig, OnProgress,
    Orchestrator,
};
use tyclaw_provider::OpenAICompatProvider;

#[derive(Parser)]
#[command(
    name = "tyclaw-client",
    about = "TyClaw Client — Lightweight standalone CLI"
)]
struct Args {
    /// 工作区路径。所有运行时与配置都从该目录统一解析。
    #[arg(short, long)]
    workspace: PathBuf,

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

    /// 上下文窗口大小（token 数）
    #[arg(long = "context-window-tokens")]
    context_window_tokens: Option<usize>,

    /// 用户提示词
    #[arg(trailing_var_arg = true, required = true)]
    prompt: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let workspace_root = canonicalize_workspace(&args.workspace);
    let config_path = workspace_root.join("config").join("config.yaml");
    let cfg: BaseConfig = load_yaml(&config_path);
    let _log_guard = init_logging(&workspace_root, &cfg.logging);

    info!(?config_path, "Loaded config");

    // 配置优先级：命令行参数 > 环境变量 > providers[name] > llm 内联字段 > 默认值
    let (api_key, api_base, model, thinking) =
        if let Some(ref provider_name) = args.api_key.as_ref().map(|_| None::<String>).unwrap_or(cfg.llm.provider.clone()) {
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
                .unwrap_or_else(|| "opus".into());
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

    print_config(
        &config_path,
        &workspace_root,
        &api_key,
        &api_base,
        &model,
        max_iterations,
    );

    // 创建 LLM Provider
    let mut provider_impl = OpenAICompatProvider::new(&api_key, &api_base, &model, thinking);
    provider_impl.set_snapshot_dir(
        workspace_root
            .join("logs")
            .join("snap")
            .join("llm_requests"),
    );
    let provider: Arc<dyn tyclaw_provider::LLMProvider> = Arc::new(provider_impl);

    // 构建 Orchestrator（不初始化 sandbox）
    let orchestrator = Orchestrator::builder(provider, &workspace_root)
        .with_model(model)
        .with_max_iterations(max_iterations)
        .with_context_window_tokens_opt(context_window)
        .with_write_snapshot(snapshot)
        .with_workspaces_config(cfg.workspaces)
        .with_subtasks(cfg.subtasks)
        .with_web_search(cfg.web_search)
        .with_control(cfg.control)
        .build();

    // 执行单轮对话
    let prompt = args.prompt.join(" ");
    let progress_cb: OnProgress = Box::new(|msg: &str| {
        let msg = msg.to_string();
        Box::pin(async move {
            let (is_thinking, content) = parse_thinking_prefix(&msg);
            if is_thinking {
                eprintln!("\x1b[2m[Thinking] {content}\x1b[0m");
            } else {
                eprintln!("\x1b[2m{msg}\x1b[0m");
            }
        })
    });

    let result = orchestrator
        .handle(
            &prompt,
            "client_user",
            "default",
            "cli",
            "direct",
            Some(&progress_cb),
        )
        .await?;

    println!("{}", result.text);
    if !result.tools_used.is_empty() {
        eprintln!(
            "\x1b[2m(tools: {} | {:.1}s)\x1b[0m",
            result.tools_used.join(", "),
            result.duration_seconds
        );
    }

    Ok(())
}

fn canonicalize_workspace(path: &std::path::Path) -> PathBuf {
    if let Err(e) = std::fs::create_dir_all(path) {
        eprintln!(
            "Warning: Failed to create workspace {}: {e}",
            path.display()
        );
    }
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn init_logging(workspace: &std::path::Path, logging: &LoggingConfig) -> WorkerGuard {
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
        .unwrap_or_else(|| workspace.join("logs").join("tyclaw-client.log"));
    let parent = log_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("logs"));
    std::fs::create_dir_all(&parent).ok();

    let file_name = log_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("tyclaw-client.log")
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
        .init();

    info!(log_file = %log_path.display(), log_level = %level, "Logging initialized");
    guard
}

fn print_config(
    config_path: &std::path::Path,
    workspace: &std::path::Path,
    api_key: &str,
    api_base: &str,
    model: &str,
    max_iterations: usize,
) {
    eprintln!(
        "\x1b[2m[config] {} | model={} | api_base={} | key={} | max_iter={}\x1b[0m",
        config_path.display(),
        model,
        api_base,
        mask_secret(api_key),
        max_iterations
    );
    eprintln!("\x1b[2m[workspace] {}\x1b[0m", workspace.display());
}
