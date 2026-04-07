use serde::{Deserialize, Serialize};

use super::protocol::FailurePolicy;
use super::routing::{RoutingPolicy, RoutingRule};

/// `[subtasks]` 配置段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtasksConfig {
    /// 是否启用多模型编排。
    #[serde(default)]
    pub enabled: bool,

    /// Planner 使用的模型名称。
    #[serde(default)]
    pub planner_model: Option<String>,

    /// Planner 是否启用 thinking（推荐开启，任务拆分需要深度推理）。
    #[serde(default)]
    pub planner_thinking_enabled: bool,

    /// Planner thinking 力度（默认 high）。
    #[serde(default = "default_thinking_effort")]
    pub planner_thinking_effort: String,

    /// Reducer 使用的模型名称。
    #[serde(default)]
    pub reducer_model: Option<String>,

    /// 最大并发执行节点数。
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,

    /// 默认故障策略。
    #[serde(default)]
    pub failure_policy: FailurePolicy,

    /// 默认节点超时（ms）。
    #[serde(default = "default_timeout_ms")]
    pub default_timeout_ms: u64,

    /// 路由规则列表。
    #[serde(default)]
    pub routing_rules: Vec<RoutingRuleConfig>,

    /// 子 agent 最大迭代次数。
    #[serde(default = "default_sub_agent_max_iterations")]
    pub sub_agent_max_iterations: usize,

    /// 无规则匹配时的默认模型。
    #[serde(default)]
    pub default_model: Option<String>,

    /// 多 Provider 注册，model_name → provider 配置。
    #[serde(default)]
    pub providers: std::collections::HashMap<String, ProviderConfig>,
}

/// 路由规则的配置表示。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRuleConfig {
    pub node_type_pattern: String,
    pub target_model: String,
}

/// 单个 Provider 的配置。
///
/// 每个模型可以有独立的生成参数（temperature、max_tokens 等），
/// 以适配不同 LLM 的 API 差异（如 Claude 支持 thinking，GPT 用 max_completion_tokens）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub endpoint: String,
    pub api_key: Option<String>,
    pub model: Option<String>,
    /// 采样温度（默认 0.3）。不同模型范围可能不同（Gemini 0-2，其他 0-1）。
    #[serde(default)]
    pub temperature: Option<f64>,
    /// 单次生成最大 token 数（默认 16384）。
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// 是否启用 extended thinking（仅 Claude 系列支持）。
    #[serde(default)]
    pub thinking_enabled: bool,
    /// thinking 力度：none/minimal/low/medium/high/xhigh（仅 thinking_enabled 时生效）。
    #[serde(default = "default_thinking_effort")]
    pub thinking_effort: String,
    /// 强制 thinking token 预算（设置后忽略 effort）。
    #[serde(default)]
    pub thinking_budget_tokens: Option<u32>,
}

impl Default for SubtasksConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            planner_model: None,
            planner_thinking_enabled: false,
            planner_thinking_effort: default_thinking_effort(),
            reducer_model: None,
            max_concurrency: default_max_concurrency(),
            failure_policy: FailurePolicy::default(),
            default_timeout_ms: default_timeout_ms(),
            sub_agent_max_iterations: default_sub_agent_max_iterations(),
            routing_rules: vec![],
            default_model: None,
            providers: std::collections::HashMap::new(),
        }
    }
}

impl SubtasksConfig {
    /// 将配置转换为 RoutingPolicy。
    pub fn to_routing_policy(&self, fallback_model: &str) -> RoutingPolicy {
        let default_model = self
            .default_model
            .clone()
            .unwrap_or_else(|| fallback_model.to_string());

        let rules = self
            .routing_rules
            .iter()
            .map(|r| RoutingRule {
                node_type_pattern: r.node_type_pattern.clone(),
                target_model: r.target_model.clone(),
            })
            .collect();

        RoutingPolicy {
            rules,
            default_model,
        }
    }
}

fn default_max_concurrency() -> usize {
    4
}
fn default_timeout_ms() -> u64 {
    120_000
}
fn default_sub_agent_max_iterations() -> usize {
    40
}
fn default_thinking_effort() -> String {
    "high".into()
}
