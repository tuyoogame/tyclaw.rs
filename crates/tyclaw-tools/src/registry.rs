//! 工具注册表：负责工具的注册、执行和定义导出。
//!
//! ToolRegistry 是工具系统的核心管理器，提供：
//! 1. 工具注册：将 Tool 实例注册到内部 HashMap
//! 2. 工具查找：根据名称获取工具引用
//! 3. 工具定义导出：生成 OpenAI function calling 格式的工具列表
//! 4. 工具执行：根据名称执行工具，自动处理参数转换和验证
//! 5. 执行策略注入：允许将 sandbox 路由外提为可替换执行层

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, warn};

use crate::base::{self, Tool};
use crate::executor::{FullToolExecutor, ToolExecutor};
use tyclaw_tool_abi::{ToolDefinitionProvider, ToolExecutionResult, ToolParams, ToolRuntime};

/// 工具注册管理器 —— 管理所有已注册工具的生命周期。
///
/// 使用 `HashMap<String, Box<dyn Tool>>` 存储工具实例，
/// 支持动态注册和按名称查找。
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>, // 工具名 → 工具实例
    executor: Arc<dyn ToolExecutor>,
}

impl ToolRegistry {
    fn error_envelope(message: &str, tool: &str) -> String {
        json!({
            "status": "error",
            "tool": tool,
            "message": message,
            "hint": "Analyze the error and try a different approach"
        })
        .to_string()
    }

    /// 创建空的工具注册表。
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            executor: Arc::new(FullToolExecutor::default()),
        }
    }

    pub fn with_executor(executor: Arc<dyn ToolExecutor>) -> Self {
        Self {
            tools: HashMap::new(),
            executor,
        }
    }

    pub fn set_executor(&mut self, executor: Arc<dyn ToolExecutor>) {
        self.executor = executor;
    }

    /// 注册一个工具实例。
    ///
    /// 工具名从 `tool.name()` 获取，作为 HashMap 的 key。
    /// 如果已存在同名工具，会被覆盖。
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// 根据名称获取工具的不可变引用。
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    /// 返回所有已注册工具的定义，采用 OpenAI function calling 格式。
    ///
    /// 每个工具生成如下 JSON 结构：
    /// ```json
    /// {
    ///   "type": "function",
    ///   "function": {
    ///     "name": "工具名",
    ///     "description": "工具描述",
    ///     "parameters": { /* JSON Schema */ }
    ///   }
    /// }
    /// ```
    pub fn get_definitions(&self) -> Vec<Value> {
        self.tools
            .values()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name(),
                        "description": tool.description(),
                        "parameters": tool.parameters(),
                    }
                })
            })
            .collect()
    }

    /// 根据名称执行工具。
    ///
    /// 执行流程：
    /// 1. 查找工具（未找到则返回错误）
    /// 2. 根据工具的 JSON Schema 自动转换参数类型
    /// 3. 验证必填参数是否完整
    /// 4. 调用工具的 execute 方法
    /// 5. 如果工具返回错误，追加提示信息引导 Agent 调整策略
    pub async fn execute(&self, name: &str, mut params: ToolParams) -> ToolExecutionResult {
        let tool = match self.tools.get(name) {
            Some(t) => t,
            None => {
                return ToolExecutionResult {
                    output: Self::error_envelope(&format!("Error: Unknown tool '{name}'"), name),
                    route: "registry".into(),
                    status: "unknown_tool".into(),
                    duration_ms: 0,
                    gate_action: "n/a".into(),
                    risk_level: "unknown".into(),
                    sandbox_id: None,
                };
            }
        };

        let schema = tool.parameters();
        // 自动类型转换（如字符串 "42" → 整数 42）
        base::cast_params(&mut params, &schema);

        // 验证必填参数
        if let Some(error) = base::validate_params(&params, &schema) {
            warn!(
                tool = name,
                error = %error,
                keys = ?params.keys().collect::<Vec<_>>(),
                "Param validation failed"
            );
            let mut msg = format!("Error: Invalid parameters for {name}: {error}");
            // 特殊提示：write_file 缺少 content 参数时，可能是因为 LLM 输出被截断
            if error.contains("content") && name == "write_file" {
                msg.push_str(
                    "\nHint: The 'content' parameter is missing, likely because your response \
                     was truncated. Try writing the file in smaller chunks or simplify the content.",
                );
            }
            return ToolExecutionResult {
                output: Self::error_envelope(&msg, name),
                route: "registry".into(),
                status: "invalid_params".into(),
                duration_ms: 0,
                gate_action: "n/a".into(),
                risk_level: tool.risk_level().to_string(),
                sandbox_id: None,
            };
        }

        debug!(tool = name, keys = ?params.keys().collect::<Vec<_>>(), "Executing tool");
        let params_for_compress = params.clone();
        let mut result = self.executor.execute(tool.as_ref(), name, params).await;

        // 如果工具执行返回错误，追加引导信息
        if result.output.starts_with("Error") {
            result.output = Self::error_envelope(&result.output, name);
            if result.status == "ok" {
                result.status = "error".into();
            }
        }

        // 工具级输出压缩（各工具可覆盖 compress_output 实现定制策略）
        let original_len = result.output.len();
        result.output = tool.compress_output(&result.output, &params_for_compress);
        if result.output.len() < original_len {
            debug!(
                tool = name,
                original = original_len,
                compressed = result.output.len(),
                "Tool output compressed"
            );
        }

        result
    }
}

/// 实现 Default trait，方便使用 ToolRegistry::default() 创建空注册表。
impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDefinitionProvider for ToolRegistry {
    fn get_definitions(&self) -> Vec<Value> {
        ToolRegistry::get_definitions(self)
    }

    fn has_tool(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    fn risk_level(&self, name: &str) -> Option<String> {
        self.get(name).map(|tool| tool.risk_level().to_string())
    }

    fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

#[async_trait]
impl ToolRuntime for ToolRegistry {
    async fn execute(&self, name: &str, params: ToolParams) -> ToolExecutionResult {
        ToolRegistry::execute(self, name, params).await
    }
}

impl From<ToolRegistry> for Arc<dyn ToolRuntime> {
    fn from(reg: ToolRegistry) -> Self {
        Arc::new(reg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::ReadFileTool;

    /// 测试：工具定义格式是否符合 OpenAI function calling 规范
    #[test]
    fn test_definitions_format() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(ReadFileTool::new(None)));
        let defs = reg.get_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0]["type"], "function");
        assert_eq!(defs[0]["function"]["name"], "read_file");
    }

    /// 测试：调用不存在的工具应返回错误
    #[tokio::test]
    async fn test_unknown_tool() {
        let reg = ToolRegistry::new();
        let result = reg.execute("nonexistent", HashMap::new()).await;
        assert!(result.output.contains("Unknown tool"));
        assert_eq!(result.status, "unknown_tool");
    }

    /// 测试：读取不存在的文件应返回错误
    #[tokio::test]
    async fn test_read_nonexistent_file() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(ReadFileTool::new(None)));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("/tmp/__tyclaw_nonexistent_test__"));
        let result = reg.execute("read_file", params).await;
        assert!(result.output.contains("Error"));
    }
}
