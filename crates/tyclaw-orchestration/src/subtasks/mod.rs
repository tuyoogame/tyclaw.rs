pub mod config;
pub mod executor;
pub mod planner;
pub mod prompt_loader;
pub mod protocol;
pub mod reducer;
pub mod routing;
pub mod scheduler;
pub mod tool;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tracing::info;

use tyclaw_provider::{LLMProvider, OpenAICompatProvider, ThinkingConfig};

use crate::app_context::AppContext;

pub use config::SubtasksConfig;
pub use protocol::{ExecutionRecord, MergeReport, TaskPlan};
pub use tool::DispatchSubtasksTool;

use executor::NodeExecutor;
use reducer::RuleReducer;
use routing::RoutingPolicy;

/// 子任务调度引擎。
///
/// 主流程：plan → schedule → reduce → 返回。
/// 在 `subtasks.enabled = true` 时注册 dispatch_subtasks 工具到主控 AgentLoop。
pub struct SubtasksEngine {
    executor: Arc<NodeExecutor>,
    reducer: RuleReducer,
    routing: RoutingPolicy,
    config: SubtasksConfig,
    app: Arc<AppContext>,
}

impl SubtasksEngine {
    pub fn new(
        config: SubtasksConfig,
        default_provider: Arc<dyn LLMProvider>,
        default_model: &str,
        workspace: PathBuf,
    ) -> Self {
        let app = AppContext::new(
            workspace,
            default_model.to_string(),
            false,
            0,
            Default::default(),
        );
        Self::new_with_context(config, default_provider, default_model, app)
    }

    pub fn new_with_snapshot(
        config: SubtasksConfig,
        default_provider: Arc<dyn LLMProvider>,
        default_model: &str,
        workspace: PathBuf,
        write_snapshot: bool,
    ) -> Self {
        let app = AppContext::new(
            workspace,
            default_model.to_string(),
            write_snapshot,
            0,
            Default::default(),
        );
        Self::new_with_context(config, default_provider, default_model, app)
    }

    pub fn new_with_context(
        config: SubtasksConfig,
        default_provider: Arc<dyn LLMProvider>,
        default_model: &str,
        app: Arc<AppContext>,
    ) -> Self {
        prompt_loader::init(&app.workspace);

        let routing = config.to_routing_policy(default_model);

        let mut providers: HashMap<String, Arc<dyn LLMProvider>> = HashMap::new();
        providers.insert(default_model.to_string(), Arc::clone(&default_provider));

        let inherited_api_key = default_provider.api_key();
        for (model_name, pcfg) in &config.providers {
            let api_key = pcfg.api_key.as_deref().unwrap_or(&inherited_api_key);
            let model = pcfg.model.as_deref().unwrap_or(model_name.as_str());
            let thinking = if pcfg.thinking_enabled {
                Some(ThinkingConfig {
                    effort: pcfg.thinking_effort.clone(),
                    budget_tokens: pcfg.thinking_budget_tokens,
                })
            } else {
                None
            };
            let mut p = OpenAICompatProvider::new(api_key, &pcfg.endpoint, model, thinking);
            if let Some(temp) = pcfg.temperature {
                p.set_temperature(temp);
            }
            if let Some(max_t) = pcfg.max_tokens {
                p.set_max_tokens(max_t);
            }
            p.set_snapshot_dir(app.workspace.join("logs").join("snap").join("llm_requests"));
            let provider: Arc<dyn LLMProvider> = Arc::new(p);
            info!(
                model_name = %model_name,
                endpoint = %pcfg.endpoint,
                model = %model,
                thinking_enabled = pcfg.thinking_enabled,
                temperature = pcfg.temperature,
                max_tokens = pcfg.max_tokens,
                "Registered subtasks provider"
            );
            providers.insert(model_name.clone(), provider);
        }

        let default_api_base = default_provider.api_base();
        let default_api_key = default_provider.api_key();
        for rule in &routing.rules {
            providers
                .entry(rule.target_model.clone())
                .or_insert_with(|| {
                    let model_name = rule.target_model.as_str();
                    info!(
                        model = %model_name,
                        "Auto-creating provider for routing target (no thinking, default params)"
                    );
                    let mut provider = OpenAICompatProvider::new(
                        &default_api_key,
                        &default_api_base,
                        model_name,
                        None,
                    );
                    provider.set_snapshot_dir(
                        app.workspace.join("logs").join("snap").join("llm_requests"),
                    );
                    Arc::new(provider)
                });
        }

        let reducer = if let Some(ref reducer_model) = config.reducer_model {
            let reducer_provider = providers
                .get(reducer_model)
                .cloned()
                .unwrap_or_else(|| Arc::clone(&default_provider));
            RuleReducer::with_llm(reducer_provider, reducer_model.clone())
        } else {
            RuleReducer::new()
        };

        let executor = Arc::new(NodeExecutor::with_max_iterations(
            providers,
            routing.clone(),
            app.clone(),
            config.sub_agent_max_iterations,
        ));

        info!(
            max_concurrency = config.max_concurrency,
            failure_policy = ?config.failure_policy,
            registered_providers = config.providers.len() + 1,
            routing_rules_count = routing.rules.len(),
            default_model = %routing.default_model,
            "SubtasksEngine initialized (dispatch_subtasks tool mode)"
        );
        for rule in &routing.rules {
            info!(
                pattern = %rule.node_type_pattern,
                target = %rule.target_model,
                "Routing rule loaded"
            );
        }

        Self {
            executor,
            reducer,
            routing,
            config,
            app,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// 消费自身，返回可注册到 ToolRegistry 的 dispatch_subtasks 工具。
    pub fn into_tool(self) -> DispatchSubtasksTool {
        DispatchSubtasksTool::new(
            self.executor,
            self.reducer,
            self.config.max_concurrency,
            self.config.default_timeout_ms,
            &self.routing,
            self.app,
        )
    }
}
