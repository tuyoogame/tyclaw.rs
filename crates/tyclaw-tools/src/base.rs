//! 工具 trait 和风险等级定义。
//!
//! 定义了所有工具必须实现的统一接口，
//! 以及参数类型转换和验证的辅助函数。

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;

use tyclaw_tool_abi::Sandbox;

/// 工具操作的风险等级。
///
/// 用于权限控制，决定不同角色能否执行某个工具：
/// - `Read`: 只读操作（如读取文件）—— 所有角色可用
/// - `Write`: 写入操作（如修改文件、执行命令）—— 需要 Member 及以上
/// - `Dangerous`: 危险操作（如删除文件系统）—— 需要 Admin 确认
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    Read,      // 只读
    Write,     // 写入
    Dangerous, // 危险
}

/// RiskLevel 的显示实现，用于日志和权限检查时的字符串比较。
impl fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RiskLevel::Read => write!(f, "read"),
            RiskLevel::Write => write!(f, "write"),
            RiskLevel::Dangerous => write!(f, "dangerous"),
        }
    }
}

/// 工具抽象接口 —— 所有工具必须实现此 trait。
///
/// `Send + Sync` 约束确保工具可以在异步多线程环境中安全使用。
/// 通过 `async_trait` 支持异步执行方法。
#[async_trait]
pub trait Tool: Send + Sync {
    /// 工具的唯一名称（如 "read_file"、"exec"），用于工具查找和 LLM 调用。
    fn name(&self) -> &str;

    /// 工具的描述信息，展示给 LLM 帮助其理解工具的用途。
    fn description(&self) -> &str;

    /// 工具参数的 JSON Schema 定义。
    /// 符合 OpenAI function calling 的参数格式规范。
    fn parameters(&self) -> Value;

    /// 工具的风险等级，默认为 Read（只读）。
    /// 子类可覆盖此方法来声明更高的风险等级。
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Read
    }

    /// 执行工具并返回结果字符串（本地执行路径）。
    ///
    /// 参数以 HashMap<String, Value> 传入，由 LLM 生成。
    /// 返回值为工具执行的文本输出（成功内容或错误信息）。
    async fn execute(&self, params: HashMap<String, Value>) -> String;

    /// 是否应该路由到沙箱执行。
    /// 默认 false，有副作用的工具（exec、write_file 等）应覆盖为 true。
    fn should_sandbox(&self) -> bool {
        false
    }

    /// 在沙箱中执行工具。
    /// 默认回退到本地执行。需要沙箱支持的工具应覆盖此方法。
    async fn execute_in_sandbox(
        &self,
        _sandbox: &dyn Sandbox,
        params: HashMap<String, Value>,
    ) -> String {
        self.execute(params).await
    }

    /// 压缩工具执行结果，减少 LLM token 消耗。
    ///
    /// 各工具可覆盖此方法实现定制压缩策略（参考 RTK 模式）：
    /// - exec：按命令类型压缩（测试输出只保留失败、编译去掉进度行等）
    /// - read_file：去掉注释和空行
    /// - grep：限制每文件匹配数、截断长行
    /// - list_dir：去掉权限/时间戳
    ///
    /// `params` 为原始调用参数，供判断命令类型等上下文信息。
    /// 默认实现：不压缩，原样返回。
    fn compress_output(&self, output: &str, _params: &HashMap<String, Value>) -> String {
        output.to_string()
    }
}

/// 根据 JSON Schema 中声明的属性类型，自动转换参数类型。
///
/// LLM 有时会把数字或布尔值以字符串形式传递（如 "42"、"true"），
/// 此函数负责将这些值转换为 Schema 期望的正确类型：
/// - "integer": 字符串 → i64，浮点数 → i64
/// - "number": 字符串 → f64
/// - "boolean": 字符串 "true"/"1"/"yes" → true，其他 → false
/// - "string": 数字/布尔值 → 字符串表示
pub fn cast_params(params: &mut HashMap<String, Value>, schema: &Value) {
    // 从 Schema 中获取 properties 定义
    let props = match schema.get("properties").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return,
    };

    for (key, prop_schema) in props {
        // 获取期望的类型
        let expected_type = match prop_schema.get("type").and_then(|t| t.as_str()) {
            Some(t) => t,
            None => continue,
        };
        // 获取当前值
        let value = match params.get(key) {
            Some(v) => v.clone(),
            None => continue,
        };

        // 根据期望类型进行转换
        let casted = match expected_type {
            "integer" => match &value {
                Value::String(s) => s.parse::<i64>().ok().map(Value::from), // "42" → 42
                Value::Number(n) => n.as_f64().map(|f| Value::from(f as i64)), // 42.0 → 42
                _ => None,
            },
            "number" => match &value {
                Value::String(s) => s.parse::<f64>().ok().map(Value::from), // "3.14" → 3.14
                _ => None,
            },
            "boolean" => match &value {
                Value::String(s) => {
                    let lower = s.to_lowercase();
                    Some(Value::Bool(
                        lower == "true" || lower == "1" || lower == "yes", // "true" → true
                    ))
                }
                _ => None,
            },
            "string" => match &value {
                Value::String(_) => None, // 已经是正确类型，无需转换
                Value::Number(n) => Some(Value::String(n.to_string())), // 42 → "42"
                Value::Bool(b) => Some(Value::String(b.to_string())), // true → "true"
                _ => None,
            },
            _ => None,
        };

        // 如果成功转换，更新参数值
        if let Some(v) = casted {
            params.insert(key.clone(), v);
        }
    }
}

/// 验证必填参数是否都已提供。
///
/// 检查 Schema 中 "required" 数组列出的所有参数名是否在 params 中存在。
/// 如果有缺失的必填参数，返回错误信息；否则返回 None。
pub fn validate_params(params: &HashMap<String, Value>, schema: &Value) -> Option<String> {
    let required = match schema.get("required").and_then(|r| r.as_array()) {
        Some(arr) => arr,
        None => return None, // 没有 required 字段，所有参数都是可选的
    };
    for key in required {
        if let Some(k) = key.as_str() {
            if !params.contains_key(k) {
                return Some(format!("Missing required parameter: {k}"));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 测试：字符串参数转换为整数
    #[test]
    fn test_cast_string_to_int() {
        let schema = json!({
            "properties": { "count": { "type": "integer" } }
        });
        let mut params = HashMap::new();
        params.insert("count".into(), json!("42"));
        cast_params(&mut params, &schema);
        assert_eq!(params["count"], json!(42));
    }

    /// 测试：字符串参数转换为布尔值
    #[test]
    fn test_cast_string_to_bool() {
        let schema = json!({
            "properties": { "flag": { "type": "boolean" } }
        });
        let mut params = HashMap::new();
        params.insert("flag".into(), json!("true"));
        cast_params(&mut params, &schema);
        assert_eq!(params["flag"], json!(true));
    }

    /// 测试：缺少必填参数时返回错误信息
    #[test]
    fn test_validate_missing_required() {
        let schema = json!({
            "required": ["path", "content"]
        });
        let mut params = HashMap::new();
        params.insert("path".into(), json!("/tmp/test"));
        let err = validate_params(&params, &schema);
        assert_eq!(err, Some("Missing required parameter: content".into()));
    }

    /// 测试：所有必填参数都存在时返回 None
    #[test]
    fn test_validate_all_present() {
        let schema = json!({
            "required": ["path"]
        });
        let mut params = HashMap::new();
        params.insert("path".into(), json!("/tmp/test"));
        assert_eq!(validate_params(&params, &schema), None);
    }
}
