//! Shell 命令执行工具 —— 带安全防护机制。
//!
//! 在沙箱化的 shell 环境中执行命令，并提供：
//! - 危险命令拦截（如 rm -rf、格式化磁盘、fork 炸弹等）
//! - 执行超时控制（默认60秒）
//! - 输出长度限制（最大10000字符，超过部分截断）

use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::process::Command;

use tyclaw_tool_abi::Sandbox;

use crate::base::{RiskLevel, Tool};

/// 命令输出的最大字符数。超过此限制的输出会被截断。
const MAX_OUTPUT_CHARS: usize = 20_000;

/// 命令执行的默认超时时间（秒）。
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// 危险命令的正则匹配模式列表。
///
/// 匹配到以下任一模式的命令将被拦截：
/// - `rm -rf` / `rm -fr`: 递归强制删除
/// - `format`: 格式化磁盘
/// - `mkfs` / `diskpart`: 磁盘分区和文件系统操作
/// - `dd if=`: 磁盘镜像写入（可能覆盖磁盘）
/// - `shutdown` / `reboot` / `poweroff`: 系统关机/重启
/// - `:() { :|:& };:`: fork 炸弹（会耗尽系统资源）
static DENY_PATTERNS: &[&str] = &[
    r"\brm\s+-[rf]*r[rf]*\b", // 只拦截 rm -r / rm -rf / rm -fr，不拦截 rm -f（单文件删除）
    r"(?:^|[;&|]\s*)format\b",
    r"\b(mkfs|diskpart)\b",
    r"\bdd\s+if=",
    r"\b(shutdown|reboot|poweroff)\b",
    r":\(\)\s*\{.*\};\s*:",       // fork 炸弹
    r"\brm\s+-[rf]*r[rf]*\s+/\b", // rm -rf / 根目录
    r">\s*/dev/[sh]d",             // 覆写磁盘设备
    r"\bchmod\s+-R\s+777\s+/\b",  // 递归 chmod 根目录
];

/// 按"字符数"安全截断 UTF-8 字符串。
///
/// 返回值：
/// - `Some((prefix, remaining_chars))`：发生截断，包含截断后的前缀和剩余字符数
/// - `None`：无需截断
fn truncate_by_chars(text: &str, max_chars: usize) -> Option<(&str, usize)> {
    if max_chars == 0 {
        let total = text.chars().count();
        return Some(("", total));
    }

    let mut char_count = 0usize;
    let mut cut_byte = text.len();
    let mut truncated = false;

    for (idx, _) in text.char_indices() {
        if char_count == max_chars {
            cut_byte = idx;
            truncated = true;
            break;
        }
        char_count += 1;
    }

    if !truncated {
        return None;
    }

    let remaining = text[cut_byte..].chars().count();
    Some((&text[..cut_byte], remaining))
}

/// Shell 命令执行工具。
///
/// 通过 `sh -c` 执行用户指定的命令，支持管道、重定向等 shell 特性。
pub struct ExecTool {
    timeout_secs: u64,           // 超时时间
    working_dir: Option<String>, // 工作目录
    deny_patterns: Vec<Regex>,   // 编译后的危险命令模式
}

impl ExecTool {
    /// 创建新的 ExecTool 实例。
    ///
    /// - `working_dir`: 命令执行的工作目录（None 则使用当前目录）
    /// - `timeout_secs`: 超时时间（None 则使用默认60秒）
    pub fn new(working_dir: Option<String>, timeout_secs: Option<u64>) -> Self {
        // 预编译所有危险命令正则表达式
        let deny_patterns = DENY_PATTERNS
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();
        Self {
            timeout_secs: timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
            working_dir,
            deny_patterns,
        }
    }
}

#[async_trait]
impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return stdout/stderr. Use for: compilation (cargo build/test), \
         running scripts, git commands, package management, or other shell-native tasks. \
         Prefer dedicated tools whenever one exists, and keep commands narrowly scoped to the task. \
         Do NOT use for: reading files (use read_file), writing files (use write_file), \
         editing files (use edit_file/apply_patch), searching content (use grep_search), \
         or listing dirs / finding files (use list_dir or glob)."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to execute" },
                "working_directory": { "type": "string", "description": "Working directory for command execution. Relative paths resolved against workspace root. Default: workspace root." },
                "timeout": { "type": "integer", "description": "Command timeout in seconds. Default: 120. Increase for long-running builds." }
            },
            "required": ["command"]
        })
    }

    /// 命令执行的风险等级为 Write
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Write
    }

    fn should_sandbox(&self) -> bool {
        true
    }

    async fn execute_in_sandbox(
        &self,
        sandbox: &dyn Sandbox,
        params: HashMap<String, Value>,
    ) -> String {
        let command = match params.get("command").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return "Error: Missing 'command' parameter".into(),
        };
        let lower = command.trim().to_lowercase();
        for pattern in &self.deny_patterns {
            if pattern.is_match(&lower) {
                tracing::warn!(pattern = %pattern, "Command blocked by deny pattern");
                return "Error: Command blocked by safety guard".into();
            }
        }
        let timeout_secs = params
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.timeout_secs);
        match sandbox
            .exec(command, std::time::Duration::from_secs(timeout_secs))
            .await
        {
            Ok(r) => {
                let text = r.to_tool_output();
                if let Some((prefix, remaining)) = truncate_by_chars(&text, MAX_OUTPUT_CHARS) {
                    format!("{prefix}\n... (truncated, {remaining} more chars)")
                } else {
                    text
                }
            }
            Err(e) => format!("Error: {e}"),
        }
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let command = match params.get("command").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return "Error: Missing 'command' parameter".into(),
        };

        // 安全检查：匹配危险命令模式
        let lower = command.trim().to_lowercase();
        for pattern in &self.deny_patterns {
            if pattern.is_match(&lower) {
                tracing::warn!(pattern = %pattern, cmd_len = lower.len(), cmd_prefix = %&lower[..lower.len().min(200)], "Command blocked by deny pattern");
                return "Error: Command blocked by safety guard".into();
            }
        }

        let timeout_secs = params
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.timeout_secs);

        // Host 执行
        let cwd = if let Some(wd) = params.get("working_directory").and_then(|v| v.as_str()) {
            if std::path::Path::new(wd).is_absolute() {
                wd.to_string()
            } else {
                let base = self.working_dir.as_deref().unwrap_or(".");
                std::path::Path::new(base)
                    .join(wd)
                    .to_string_lossy()
                    .to_string()
            }
        } else {
            self.working_dir.as_deref().unwrap_or(".").to_string()
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(&cwd)
                .output(),
        )
        .await;

        match result {
            Err(_) => format!("Error: Command timed out after {timeout_secs} seconds"),
            // 执行失败（如找不到 sh）
            Ok(Err(e)) => format!("Error executing command: {e}"),
            // 执行成功（可能有非0退出码）
            Ok(Ok(output)) => {
                let mut parts = Vec::new();
                // 收集标准输出
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stdout.is_empty() {
                    parts.push(stdout.to_string());
                }
                // 收集标准错误
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stderr = stderr.trim();
                if !stderr.is_empty() {
                    parts.push(format!("STDERR:\n{stderr}"));
                }
                // 非0退出码时附加退出码信息
                if !output.status.success() {
                    let code = output.status.code().unwrap_or(-1);
                    parts.push(format!("\nExit code: {code}"));
                }
                let text = if parts.is_empty() {
                    "(no output)".to_string()
                } else {
                    parts.join("\n")
                };
                // 输出截断（按字符数，避免 UTF-8 边界 panic）
                if let Some((prefix, remaining)) = truncate_by_chars(&text, MAX_OUTPUT_CHARS) {
                    format!("{prefix}\n... (truncated, {remaining} more chars)")
                } else {
                    text
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试：正常执行 echo 命令
    #[tokio::test]
    async fn test_exec_echo() {
        let tool = ExecTool::new(None, None);
        let mut params = HashMap::new();
        params.insert("command".into(), json!("echo hello"));
        let result = tool.execute(params).await;
        assert_eq!(result.trim(), "hello");
    }

    /// 测试：rm -rf 被安全机制拦截
    #[tokio::test]
    async fn test_exec_blocked_rm_rf() {
        let tool = ExecTool::new(None, None);
        let mut params = HashMap::new();
        params.insert("command".into(), json!("rm -rf /"));
        let result = tool.execute(params).await;
        assert!(result.contains("blocked by safety guard"));
    }

    /// 测试：fork 炸弹被安全机制拦截
    #[tokio::test]
    async fn test_exec_blocked_fork_bomb() {
        let tool = ExecTool::new(None, None);
        let mut params = HashMap::new();
        params.insert("command".into(), json!(":() { :|:& };:"));
        let result = tool.execute(params).await;
        assert!(result.contains("blocked by safety guard"));
    }

    /// 测试：非0退出码能正确报告
    #[tokio::test]
    async fn test_exec_exit_code() {
        let tool = ExecTool::new(None, None);
        let mut params = HashMap::new();
        params.insert("command".into(), json!("exit 1"));
        let result = tool.execute(params).await;
        assert!(result.contains("Exit code: 1"));
    }

    /// 测试：UTF-8 文本按字符截断不会触发边界 panic
    #[test]
    fn test_truncate_by_chars_utf8_boundary() {
        let s = "abc建def";
        // 截断到 4 个字符，应该是 "abc建"
        let (prefix, remain) = truncate_by_chars(s, 4).expect("should truncate");
        assert_eq!(prefix, "abc建");
        assert_eq!(remain, 3);
    }
}
