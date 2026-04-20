use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tyclaw_types::TyclawError;

pub type ToolParams = HashMap<String, Value>;

// ── Sandbox 抽象 ──

/// 沙箱内命令执行结果。
#[derive(Debug, Clone)]
pub struct SandboxExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
}

impl SandboxExecResult {
    /// 将 stdout + stderr 合并为工具输出格式。
    pub fn to_tool_output(&self) -> String {
        let mut parts = Vec::new();
        if !self.stdout.is_empty() {
            parts.push(self.stdout.clone());
        }
        let stderr = self.stderr.trim();
        if !stderr.is_empty() {
            parts.push(format!("STDERR:\n{stderr}"));
        }
        if self.timed_out {
            parts.push("Error: Command timed out".into());
        } else if self.exit_code != 0 {
            parts.push(format!("\nExit code: {}", self.exit_code));
        }
        if parts.is_empty() {
            "(no output)".into()
        } else {
            parts.join("\n")
        }
    }
}

/// 目录条目（list_dir 返回）。
#[derive(Debug, Clone)]
pub struct SandboxDirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// 文件元信息（供工具在 sandbox 中统一判断文件/目录/大小）。
#[derive(Debug, Clone)]
pub struct SandboxFileStat {
    pub exists: bool,
    pub is_file: bool,
    pub is_dir: bool,
    pub size: Option<u64>,
}

/// 递归目录遍历条目（供 host/sandbox 共享 list_dir 语义）。
#[derive(Debug, Clone)]
pub struct SandboxWalkEntry {
    pub path: String,
    pub is_dir: bool,
    pub depth: usize,
}

/// grep 搜索请求。
#[derive(Debug, Clone)]
pub struct SandboxGrepRequest {
    pub pattern: String,
    pub path: String,
    pub include: Option<String>,
    pub file_type: Option<String>,
    pub context_lines: Option<usize>,
    pub case_insensitive: bool,
    pub output_mode: String,
    pub max_results: usize,
}

/// grep 搜索原始响应，由工具层做统一格式化。
#[derive(Debug, Clone)]
pub struct SandboxGrepResponse {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// glob 匹配结果。
#[derive(Debug, Clone)]
pub struct SandboxGlobEntry {
    pub path: String,
    pub modified_unix_secs: u64,
}

/// 数据挂载描述：host 上的路径 → 容器内的路径。
#[derive(Debug, Clone)]
pub struct PathMount {
    pub host_path: PathBuf,
    pub container_path: String,
    pub readonly: bool,
}

/// 沙箱实例 —— 隔离执行环境的统一接口。
///
/// 具体实现可以是 Docker 容器、Podman、Wasm runtime 等。
/// 工具通过此 trait 执行沙箱内操作，不感知底层实现。
#[async_trait]
pub trait Sandbox: Send + Sync {
    async fn exec(&self, cmd: &str, timeout: Duration) -> Result<SandboxExecResult, TyclawError>;
    async fn stat(&self, path: &str) -> Result<SandboxFileStat, TyclawError>;
    async fn read_file(&self, path: &str) -> Result<Vec<u8>, TyclawError>;
    async fn write_file(&self, path: &str, content: &[u8]) -> Result<(), TyclawError>;
    async fn create_dir(&self, path: &str) -> Result<(), TyclawError>;
    async fn list_dir(&self, path: &str) -> Result<Vec<SandboxDirEntry>, TyclawError>;
    async fn walk_dir(
        &self,
        path: &str,
        max_depth: usize,
    ) -> Result<Vec<SandboxWalkEntry>, TyclawError>;
    async fn grep_search(
        &self,
        request: SandboxGrepRequest,
    ) -> Result<SandboxGrepResponse, TyclawError>;
    async fn glob_search(
        &self,
        pattern: &str,
        path: &str,
    ) -> Result<Vec<SandboxGlobEntry>, TyclawError>;
    async fn file_exists(&self, path: &str) -> bool;
    async fn remove_file(&self, path: &str) -> Result<(), TyclawError>;
    async fn copy_from(&self, container_path: &str, host_path: &PathBuf)
        -> Result<(), TyclawError>;
    fn workspace_root(&self) -> &str;
    fn id(&self) -> &str;
}

/// 沙箱池 —— 管理沙箱实例的 acquire/release 生命周期。
#[async_trait]
pub trait SandboxPool: Send + Sync {
    async fn acquire(
        &self,
        task_workspace: &PathBuf,
        data_mounts: &[PathMount],
    ) -> Result<Arc<dyn Sandbox>, TyclawError>;

    async fn release(
        &self,
        sandbox: Arc<dyn Sandbox>,
        task_workspace: &PathBuf,
    ) -> Result<(), TyclawError>;

    async fn available_count(&self) -> usize;
    async fn total_count(&self) -> usize;
    async fn is_available(&self) -> bool;
}

// ── 执行门禁 ──

/// 门禁判定动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateAction {
    Allow,
    Deny,
    Confirm,
}

/// 门禁判定结果。
#[derive(Debug, Clone)]
pub struct GateJudgment {
    pub action: GateAction,
    pub reason: String,
}

/// 执行门禁策略 —— 判断工具调用是否应该被允许执行。
///
/// 具体实现在 tyclaw-control（RBAC 规则），此处只定义接口。
pub trait GatePolicy: Send + Sync {
    fn judge(&self, tool_name: &str, risk_level: &str, user_role: &str) -> GateJudgment;
}

/// 默认门禁：全部放行。
pub struct AllowAllGate;

impl GatePolicy for AllowAllGate {
    fn judge(&self, _tool_name: &str, _risk_level: &str, _user_role: &str) -> GateJudgment {
        GateJudgment {
            action: GateAction::Allow,
            reason: "No gate configured".into(),
        }
    }
}

// ── 工具定义与运行时 ──

/// 工具定义源 —— 仅提供定义查询能力，不含执行。
pub trait ToolDefinitionProvider: Send + Sync {
    fn get_definitions(&self) -> Vec<Value>;

    fn has_tool(&self, name: &str) -> bool;

    fn risk_level(&self, name: &str) -> Option<String>;

    fn tool_names(&self) -> Vec<String> {
        self.get_definitions()
            .iter()
            .filter_map(|d| d["function"]["name"].as_str().map(String::from))
            .collect()
    }

    /// 给 UI（钉钉卡片、CLI 进度行等）用的工具调用简短描述。
    ///
    /// 典型输出：`"exec: npm test"`、`"read: foo.rs"`、`"grep: pattern"`。
    /// 默认返回 `None`，具体工具或注册表可实现并按参数格式化。
    fn brief(&self, _name: &str, _args: &ToolParams) -> Option<String> {
        None
    }
}

/// 结构化工具执行结果。
#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    pub output: String,
    pub route: String,
    pub status: String,
    pub duration_ms: u64,
    pub gate_action: String,
    pub risk_level: String,
    pub sandbox_id: Option<String>,
}

impl ToolExecutionResult {
    pub fn ok(output: String, route: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            output,
            route: route.into(),
            status: "ok".into(),
            duration_ms,
            gate_action: "allow".into(),
            risk_level: "unknown".into(),
            sandbox_id: None,
        }
    }
}

/// 完整的工具运行时 —— 定义 + 执行。
#[async_trait]
pub trait ToolRuntime: ToolDefinitionProvider + Send + Sync {
    async fn execute(&self, name: &str, params: ToolParams) -> ToolExecutionResult;
}

// ── blanket impls for Arc<T> ──

impl<T> ToolDefinitionProvider for Arc<T>
where
    T: ToolDefinitionProvider + ?Sized,
{
    fn get_definitions(&self) -> Vec<Value> {
        self.as_ref().get_definitions()
    }

    fn has_tool(&self, name: &str) -> bool {
        self.as_ref().has_tool(name)
    }

    fn risk_level(&self, name: &str) -> Option<String> {
        self.as_ref().risk_level(name)
    }

    fn tool_names(&self) -> Vec<String> {
        self.as_ref().tool_names()
    }

    fn brief(&self, name: &str, args: &ToolParams) -> Option<String> {
        self.as_ref().brief(name, args)
    }
}

#[async_trait]
impl<T> ToolRuntime for Arc<T>
where
    T: ToolRuntime + ?Sized,
{
    async fn execute(&self, name: &str, params: ToolParams) -> ToolExecutionResult {
        self.as_ref().execute(name, params).await
    }
}
