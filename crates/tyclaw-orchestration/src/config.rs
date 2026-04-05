//! 公共配置结构 —— 供 tyclaw-app 和 tyclaw-client 共用。
//!
//! 配置优先级：命令行参数 > 环境变量 > config.yaml > 默认值

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tyclaw_control::WorkspaceConfig;

use crate::subtasks::SubtasksConfig;

// ── 公共配置结构 ──────────────────────────────────────────

/// 统一配置（config.yaml，app 和 client 共用同一个文件）。
///
/// 各端只解析自己需要的段，不认识的段自动忽略（serde default）。
#[derive(Debug, Default, Deserialize)]
pub struct BaseConfig {
    /// 全局 Provider 定义：各模型独立配置 endpoint / api_key / model。
    /// 主控 LLM 和子任务引擎通过名字引用。
    #[serde(default)]
    pub providers: HashMap<String, crate::subtasks::config::ProviderConfig>,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub workspace: WorkspaceRuntimeConfig,
    #[serde(default)]
    pub workspaces: HashMap<String, WorkspaceConfig>,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub subtasks: SubtasksConfig,
    #[serde(default)]
    pub web_search: tyclaw_tools::WebSearchConfig,
    #[serde(default)]
    pub control: tyclaw_control::ControlConfig,
}

/// Workspace 运行时配置。
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceRuntimeConfig {
    /// workspace key 解析策略
    #[serde(default)]
    pub key_strategy: tyclaw_control::WorkspaceKeyStrategy,
    /// 空闲超时（秒）：超过此时间未访问的 workspace 将被回收。0 表示不回收。
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
}

impl Default for WorkspaceRuntimeConfig {
    fn default() -> Self {
        Self {
            key_strategy: tyclaw_control::WorkspaceKeyStrategy::default(),
            idle_timeout_secs: default_idle_timeout_secs(),
        }
    }
}

fn default_idle_timeout_secs() -> u64 {
    1800 // 30 分钟
}

/// LLM 配置。
///
/// 推荐用法：在顶层 `providers` 定义模型，这里用 `provider` 引用名字。
/// 也兼容旧格式：直接写 `api_key` / `api_base` / `model`。
#[derive(Debug, Default, Deserialize)]
pub struct LlmConfig {
    /// 引用全局 providers 中的名字（推荐）。
    /// 设置后忽略下方的 api_key / api_base / model / thinking_* 字段。
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub api_base: Option<String>,
    pub model: Option<String>,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
    #[serde(default)]
    pub context_window_tokens: Option<usize>,
    #[serde(default)]
    pub snapshot: bool,
    #[serde(default)]
    pub thinking_enabled: bool,
    #[serde(default = "default_thinking_effort")]
    pub thinking_effort: String,
    #[serde(default)]
    pub thinking_budget_tokens: Option<u32>,
}

/// 日志配置。
#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default)]
    pub file: Option<PathBuf>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file: None,
        }
    }
}

// ── 默认值函数 ──────────────────────────────────────────

pub fn default_max_iterations() -> usize {
    40
}
pub fn default_thinking_effort() -> String {
    "high".into()
}
pub fn default_log_level() -> String {
    "info".into()
}

// ── 辅助函数 ──────────────────────────────────────────

/// 加载 YAML 配置文件，文件不存在或解析失败时返回默认值。
pub fn load_yaml<T: Default + serde::de::DeserializeOwned>(config_path: &Path) -> T {
    if !config_path.exists() {
        return T::default();
    }
    match std::fs::read_to_string(config_path) {
        Ok(text) => match serde_yaml::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("Warning: Failed to parse {}: {e}", config_path.display());
                T::default()
            }
        },
        Err(e) => {
            eprintln!("Warning: Failed to read {}: {e}", config_path.display());
            T::default()
        }
    }
}

/// 对 API 密钥等敏感字段做掩码（保留首尾 4 字符）。
pub fn mask_secret(secret: &str) -> String {
    if secret.is_empty() {
        return "<empty>".into();
    }
    if secret.len() <= 8 {
        return "***".into();
    }
    let prefix = &secret[..4];
    let suffix = &secret[secret.len() - 4..];
    format!("{prefix}***{suffix}")
}
