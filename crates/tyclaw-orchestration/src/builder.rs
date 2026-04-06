//! 编排器构建器：用于注入工具注册表和按需裁剪能力。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::info;

use tyclaw_agent::AgentLoop;
use tyclaw_control::{AuditLog, ExecutionGate, RateLimiter, WorkspaceConfig, WorkspaceManager};
use tyclaw_memory::CaseStore;
use tyclaw_prompt::ContextBuilder;
use tyclaw_provider::LLMProvider;
use tyclaw_tools::{ToolDefinitionProvider, ToolRuntime};
use tyclaw_tools::{
    timer::TimerService, ApplyPatchTool, AskUserTool, CopyFileTool, DeleteFileTool, EditFileTool,
    ExecTool, GlobTool, GrepSearchTool, ListDirTool, MkdirTool, MoveFileTool, PendingFileStore,
    ReadFileTool, SendFileTool, TimerTool, ToolRegistry, WebFetchTool, WebSearchConfig,
    WebSearchTool, WriteFileTool,
};
use tyclaw_types::constants::DEFAULT_CONTEXT_WINDOW;

use crate::app_context::AppContext;
use crate::orchestrator::Orchestrator;
use crate::persistence::PersistenceLayer;
use crate::session_manager::SessionManager;
use crate::skill_manager::SkillManager;
use crate::types::OrchestratorFeatures;

/// 编排器构建器：用于注入工具注册表和按需裁剪能力。
pub struct OrchestratorBuilder {
    pub(crate) provider: Arc<dyn LLMProvider>,
    pub(crate) workspace: PathBuf,
    pub(crate) model: Option<String>,
    pub(crate) max_iterations: Option<usize>,
    pub(crate) context_window_tokens: Option<usize>,
    pub(crate) write_snapshot: bool,
    pub(crate) workspaces_config: Option<HashMap<String, WorkspaceConfig>>,
    pub(crate) tools_for_loop: Option<ToolRegistry>,
    pub(crate) tool_defs_registry: Option<ToolRegistry>,
    pub(crate) features: OrchestratorFeatures,
    pub(crate) subtasks_config: Option<crate::subtasks::SubtasksConfig>,
    pub(crate) timer_service: Option<Arc<TimerService>>,
    pub(crate) web_search_config: Option<WebSearchConfig>,
    pub(crate) control_config: Option<tyclaw_control::ControlConfig>,
    pub(crate) workspace_key_strategy: tyclaw_control::WorkspaceKeyStrategy,
}

impl OrchestratorBuilder {
    pub fn new(provider: Arc<dyn LLMProvider>, workspace: impl AsRef<Path>) -> Self {
        Self {
            provider,
            workspace: workspace.as_ref().to_path_buf(),
            model: None,
            max_iterations: None,
            context_window_tokens: None,
            write_snapshot: false,
            workspaces_config: None,
            tools_for_loop: None,
            tool_defs_registry: None,
            features: OrchestratorFeatures::default(),
            subtasks_config: None,
            timer_service: None,
            web_search_config: None,
            control_config: None,
            workspace_key_strategy: tyclaw_control::WorkspaceKeyStrategy::default(),
        }
    }

    pub fn with_workspace_key_strategy(mut self, strategy: tyclaw_control::WorkspaceKeyStrategy) -> Self {
        self.workspace_key_strategy = strategy;
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_model_opt(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    pub fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.max_iterations = Some(max_iterations);
        self
    }

    pub fn with_max_iterations_opt(mut self, max_iterations: Option<usize>) -> Self {
        self.max_iterations = max_iterations;
        self
    }

    pub fn with_context_window_tokens(mut self, context_window_tokens: usize) -> Self {
        self.context_window_tokens = Some(context_window_tokens);
        self
    }

    pub fn with_context_window_tokens_opt(mut self, context_window_tokens: Option<usize>) -> Self {
        self.context_window_tokens = context_window_tokens;
        self
    }

    pub fn with_write_snapshot(mut self, write_snapshot: bool) -> Self {
        self.write_snapshot = write_snapshot;
        self
    }

    pub fn with_workspaces_config(mut self, cfg: HashMap<String, WorkspaceConfig>) -> Self {
        self.workspaces_config = Some(cfg);
        self
    }

    pub fn with_workspaces_config_opt(
        mut self,
        cfg: Option<HashMap<String, WorkspaceConfig>>,
    ) -> Self {
        self.workspaces_config = cfg;
        self
    }

    pub fn with_runtime_tools(mut self, tools: ToolRegistry) -> Self {
        self.tools_for_loop = Some(tools);
        self
    }

    /// 注入一份共享工具注册表，默认同时作为 runtime 和 definitions 的来源。
    pub fn with_tools(mut self, tools: ToolRegistry) -> Self {
        self.tools_for_loop = Some(tools);
        self.tool_defs_registry = None;
        self
    }

    /// 注入一份单独的 definitions registry。
    ///
    /// 正常情况下优先使用 `with_tools()` 让 runtime 和 definitions 共享同一份来源。
    /// 只有嵌入式场景确实需要“模型可见工具面”与“运行时可执行工具面”分离时，才单独调用此方法。
    pub fn with_tool_definitions(mut self, tools: ToolRegistry) -> Self {
        self.tool_defs_registry = Some(tools);
        self
    }

    /// 注入两份独立 registry（高级 override）。
    ///
    /// 这会显式绕过默认的共享 tool surface 路径。仅适用于 SDK 嵌入等少数高级场景；
    /// 正常应用代码优先使用 `with_tools()`，避免 runtime / definitions 再次漂移。
    pub fn with_tool_registries(
        mut self,
        runtime_tools: ToolRegistry,
        definition_tools: ToolRegistry,
    ) -> Self {
        self.tools_for_loop = Some(runtime_tools);
        self.tool_defs_registry = Some(definition_tools);
        self
    }

    pub fn with_features(mut self, features: OrchestratorFeatures) -> Self {
        self.features = features;
        self
    }

    pub fn enable_audit(mut self, enabled: bool) -> Self {
        self.features.enable_audit = enabled;
        self
    }

    pub fn enable_memory(mut self, enabled: bool) -> Self {
        self.features.enable_memory = enabled;
        self
    }

    pub fn enable_rbac(mut self, enabled: bool) -> Self {
        self.features.enable_rbac = enabled;
        self
    }

    pub fn enable_rate_limit(mut self, enabled: bool) -> Self {
        self.features.enable_rate_limit = enabled;
        self
    }

    pub fn with_subtasks(mut self, config: crate::subtasks::SubtasksConfig) -> Self {
        self.subtasks_config = Some(config);
        self
    }

    pub fn with_timer(mut self, timer: Arc<TimerService>) -> Self {
        self.timer_service = Some(timer);
        self
    }

    pub fn with_web_search(mut self, config: WebSearchConfig) -> Self {
        self.web_search_config = Some(config);
        self
    }

    pub fn with_control(mut self, config: tyclaw_control::ControlConfig) -> Self {
        self.control_config = Some(config);
        self
    }

    pub fn build(self) -> Orchestrator {
        let Self {
            provider,
            workspace,
            model,
            max_iterations,
            context_window_tokens,
            write_snapshot,
            workspaces_config,
            tools_for_loop,
            tool_defs_registry,
            features,
            subtasks_config,
            timer_service,
            web_search_config,
            control_config,
            workspace_key_strategy,
        } = self;

        // 初始化 nudge 提示词加载器（从 config/prompts/nudges/ 加载）
        tyclaw_prompt::nudge_loader::init(&workspace);

        let control = control_config.unwrap_or_default();
        let actual_model = model.unwrap_or_else(|| provider.default_model().to_string());
        let ctx_window = context_window_tokens.unwrap_or(DEFAULT_CONTEXT_WINDOW);

        // control.yaml 驱动 features 开关
        let features = OrchestratorFeatures {
            enable_rbac: control.rbac.enabled,
            enable_rate_limit: control.rate_limit.enabled,
            enable_audit: control.audit.enabled,
            ..features
        };

        let app = AppContext::new(
            workspace.clone(),
            actual_model.clone(),
            write_snapshot,
            ctx_window,
            features.clone(),
        );

        let context = ContextBuilder::new(&workspace);
        let persistence = PersistenceLayer {
            workspace_mgr: WorkspaceManager::new(&workspace, workspace_key_strategy, workspaces_config),
            audit: AuditLog::new(workspace.join("audit")),
            case_store: CaseStore::new(workspace.join("cases")),
            sessions: SessionManager::new(&workspace),
            skills: SkillManager::new(workspace.join("skills"), workspace.clone()),
            rate_limiter: RateLimiter::new(
                control.rate_limit.per_user,
                control.rate_limit.global,
                control.rate_limit.window_secs,
            ),
        };

        // 启动时清理上次残留的临时目录。
        let workspace_dispatches = workspace.join("dispatches");
        if workspace_dispatches.is_dir() {
            let _ = std::fs::remove_dir_all(&workspace_dispatches);
        }
        let tmp_attachments = workspace.join("tmp").join("attachments");
        if tmp_attachments.is_dir() {
            let _ = std::fs::remove_dir_all(&tmp_attachments);
        }
        // 扫描 works/{bucket}/{key}/work/dispatches 并清理
        let works_dir = workspace.join("works");
        if let Ok(buckets) = std::fs::read_dir(&works_dir) {
            for bucket in buckets.flatten() {
                if let Ok(ws_entries) = std::fs::read_dir(bucket.path()) {
                    for ws_entry in ws_entries.flatten() {
                        let dispatches_dir = ws_entry.path().join("work").join("dispatches");
                        if dispatches_dir.is_dir() {
                            let _ = std::fs::remove_dir_all(dispatches_dir);
                        }
                    }
                }
            }
        }

        let pending_files: Arc<PendingFileStore> = Arc::new(PendingFileStore::new());
        let surface_config = ToolSurfaceConfig {
            app: app.clone(),
            pending_files: pending_files.clone(),
            timer_service: timer_service.as_ref(),
            web_search_config: web_search_config.clone(),
            subtasks_config: subtasks_config.clone(),
            provider: provider.clone(),
        };

        let mut runtime_registry = build_tool_surface_registry(
            tools_for_loop.unwrap_or_else(|| default_tool_registry(&workspace)),
            &surface_config,
        );
        attach_runtime_executor(&mut runtime_registry, features.enable_rbac);
        let runtime_registry = Arc::new(runtime_registry);

        let tool_defs_registry: Arc<dyn ToolDefinitionProvider> =
            if let Some(definition_seed) = tool_defs_registry {
                Arc::new(build_tool_surface_registry(
                    definition_seed,
                    &surface_config,
                ))
            } else {
                runtime_registry.clone()
            };

        let runtime_tools: Arc<dyn ToolRuntime> = runtime_registry.clone();

        let runtime = AgentLoop::new(
            provider.clone(),
            runtime_tools,
            Some(actual_model.clone()),
            max_iterations,
        );

        Orchestrator {
            app,
            provider,
            runtime: Box::new(runtime),
            context,
            persistence,
            tool_defs_registry,
            pending_files,
            pending_ask_user: parking_lot::Mutex::new(HashMap::new()),
            timer_service,
            active_tasks: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            sandbox_pool: None, // 由 main.rs 启动时注入
            injection_queues: parking_lot::Mutex::new(HashMap::new()),
        }
    }
}

/// 注册核心文件操作工具集（主 agent 和 sub-agent 共用）。
///
/// 包含：ReadFile, WriteFile, EditFile, ListDir, GrepSearch, Glob, Exec
/// 不含交互类工具（AskUser, SendFile 等），由调用方按需追加。
pub fn register_core_tools(tools: &mut ToolRegistry, workspace: &Path) {
    let ws = Some(workspace.to_path_buf());
    tools.register(Box::new(ReadFileTool::new(ws.clone())));
    tools.register(Box::new(WriteFileTool::new(ws.clone())));
    tools.register(Box::new(EditFileTool::new(ws.clone())));
    tools.register(Box::new(DeleteFileTool::new(ws.clone())));
    tools.register(Box::new(ApplyPatchTool::new(ws.clone())));
    tools.register(Box::new(ListDirTool::new(ws.clone())));
    tools.register(Box::new(GrepSearchTool::new(ws.clone())));
    tools.register(Box::new(GlobTool::new(ws.clone())));
    tools.register(Box::new(CopyFileTool::new(ws.clone())));
    tools.register(Box::new(MoveFileTool::new(ws.clone())));
    tools.register(Box::new(MkdirTool::new(ws)));
    tools.register(Box::new(ExecTool::new(
        Some(workspace.to_string_lossy().into()),
        None,
    )));
}

/// 注册默认工具集到工具注册表（核心工具 + AskUser）。
pub(crate) fn register_default_tools(tools: &mut ToolRegistry, workspace: &Path) {
    register_core_tools(tools, workspace);
    tools.register(Box::new(AskUserTool::new()));
}

pub(crate) fn default_tool_registry(workspace: &Path) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    register_default_tools(&mut reg, workspace);
    reg
}

struct ToolSurfaceConfig<'a> {
    app: Arc<AppContext>,
    pending_files: Arc<PendingFileStore>,
    timer_service: Option<&'a Arc<TimerService>>,
    web_search_config: Option<WebSearchConfig>,
    subtasks_config: Option<crate::subtasks::SubtasksConfig>,
    provider: Arc<dyn LLMProvider>,
}

fn attach_runtime_executor(tools: &mut ToolRegistry, enable_rbac: bool) {
    let gate: Arc<dyn tyclaw_tools::GatePolicy> = if enable_rbac {
        Arc::new(ExecutionGate::new())
    } else {
        Arc::new(tyclaw_tools::AllowAllGate)
    };
    tools.set_executor(Arc::new(tyclaw_tools::FullToolExecutor::new(
        gate,
        Some(tyclaw_sandbox::current_sandbox),
    )));
}

fn build_tool_surface_registry(
    mut tools: ToolRegistry,
    config: &ToolSurfaceConfig<'_>,
) -> ToolRegistry {
    register_orchestration_tools(
        &mut tools,
        &config.app.workspace,
        config.pending_files.clone(),
        config.timer_service,
        config.web_search_config.clone(),
    );

    if let Some(st_config) = config.subtasks_config.clone().filter(|c| c.enabled) {
        let engine = crate::subtasks::SubtasksEngine::new_with_context(
            st_config,
            config.provider.clone(),
            &config.app.model,
            config.app.clone(),
        );
        tools.register(Box::new(engine.into_tool()));
        info!("Multi-model mode: dispatch_subtasks registered on tool surface");
    }

    tools
}

fn register_orchestration_tools(
    tools: &mut ToolRegistry,
    workspace: &Path,
    pending_files: Arc<PendingFileStore>,
    timer_service: Option<&Arc<TimerService>>,
    web_search_config: Option<WebSearchConfig>,
) {
    tools.register(Box::new(SendFileTool::new(
        Some(workspace.to_path_buf()),
        pending_files,
    )));

    if let Some(timer) = timer_service {
        tools.register(Box::new(TimerTool::new(timer.clone())));
        info!("Timer tool registered");
    }

    let ws_config = web_search_config.unwrap_or_default();
    let proxy = if ws_config.proxy.is_empty() {
        None
    } else {
        Some(ws_config.proxy.as_str())
    };
    tools.register(Box::new(WebSearchTool::new(ws_config.clone())));
    tools.register(Box::new(WebFetchTool::new(None, proxy)));
    info!("Web tools registered (provider={})", ws_config.provider);
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use tyclaw_provider::{ChatRequest, GenerationSettings, LLMResponse};

    struct DummyProvider;

    #[async_trait]
    impl LLMProvider for DummyProvider {
        async fn chat(
            &self,
            _request: ChatRequest,
        ) -> Result<LLMResponse, tyclaw_types::TyclawError> {
            Ok(LLMResponse::error("not used in builder tests"))
        }

        fn default_model(&self) -> &str {
            "dummy-model"
        }

        fn generation_settings(&self) -> GenerationSettings {
            GenerationSettings::default()
        }
    }

    fn make_builder() -> OrchestratorBuilder {
        OrchestratorBuilder::new(Arc::new(DummyProvider), PathBuf::from("."))
    }

    #[test]
    fn test_with_tools_clears_definition_override() {
        let mut definitions = ToolRegistry::new();
        definitions.register(Box::new(AskUserTool::new()));

        let mut shared = ToolRegistry::new();
        shared.register(Box::new(AskUserTool::new()));

        let builder = make_builder()
            .with_tool_definitions(definitions)
            .with_tools(shared);

        assert!(builder.tools_for_loop.is_some());
        assert!(
            builder.tool_defs_registry.is_none(),
            "with_tools should restore the default shared tool surface path"
        );
    }

    #[test]
    fn test_with_tool_registries_keeps_explicit_override() {
        let mut runtime = ToolRegistry::new();
        runtime.register(Box::new(AskUserTool::new()));

        let mut definitions = ToolRegistry::new();
        definitions.register(Box::new(ReadFileTool::new(None)));

        let builder = make_builder().with_tool_registries(runtime, definitions);

        assert!(builder.tools_for_loop.is_some());
        assert!(
            builder.tool_defs_registry.is_some(),
            "explicit dual-registry override should remain available for advanced callers"
        );
    }
}
