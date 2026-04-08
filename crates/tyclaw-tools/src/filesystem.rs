//! 文件系统工具：读文件、写文件、编辑文件、列目录。
//!
//! 所有文件操作都支持工作区路径解析：
//! - 绝对路径直接使用
//! - 相对路径基于工作区目录解析
//! - 支持波浪号（~）展开为用户主目录

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::Mutex;

use tyclaw_tool_abi::{Sandbox, SandboxFileStat, SandboxWalkEntry};

use crate::base::{RiskLevel, Tool};

/// 读取文件的最大字符数限制。
/// 超过此限制的内容会被截断，并附加截断提示。
const MAX_READ_CHARS: usize = 128_000;

#[derive(Debug, Clone, Copy)]
struct ReadRequest {
    offset: usize,
    limit: Option<usize>,
}

fn parse_read_request(params: &HashMap<String, Value>) -> ReadRequest {
    let offset = params
        .get("offset")
        .and_then(|v| v.as_i64())
        .map(|n| n.max(1) as usize)
        .unwrap_or(1);
    let limit = params
        .get("limit")
        .and_then(|v| v.as_i64())
        .map(|n| n.max(0) as usize);

    ReadRequest { offset, limit }
}

fn render_read_file_output(
    path: &str,
    stat: SandboxFileStat,
    raw_bytes: Vec<u8>,
    req: ReadRequest,
) -> String {
    if !stat.exists {
        return format!("Error: File not found: {path}");
    }
    if stat.is_dir {
        return format!("Error: Not a file: {path}");
    }
    if !stat.is_file {
        return format!("Error: Not a regular file: {path}");
    }

    let check_len = raw_bytes.len().min(8192);
    if raw_bytes[..check_len].contains(&0) {
        // 检测图片文件：按扩展名判断，返回 data URI 供多模态分析
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let mime = match ext.as_str() {
            "jpg" | "jpeg" => Some("image/jpeg"),
            "png" => Some("image/png"),
            "gif" => Some("image/gif"),
            "webp" => Some("image/webp"),
            _ => None,
        };
        if let Some(mime_type) = mime {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&raw_bytes);
            // 特殊前缀标记，agent_loop 识别后转为多模态 vision content
            return format!("[[IMAGE:data:{mime_type};base64,{b64}]]");
        }
        return format!(
            "Binary file ({} bytes), cannot display as text.",
            raw_bytes.len()
        );
    }

    let content = match String::from_utf8(raw_bytes) {
        Ok(s) => s,
        Err(_) => return "Binary file (not valid UTF-8), cannot display as text.".into(),
    };

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    if req.offset > 1 || req.limit.is_some() {
        let start = (req.offset - 1).min(lines.len());
        let end = match req.limit {
            Some(n) => (start + n).min(lines.len()),
            None => lines.len(),
        };

        if start >= lines.len() {
            return format!(
                "Error: offset {} exceeds total lines ({total_lines})",
                req.offset
            );
        }

        let mut result = String::new();
        for (i, line) in lines[start..end].iter().enumerate() {
            let line_no = start + i + 1;
            result.push_str(&format!("{line_no:>6}|{line}\n"));
        }
        result.push_str(&format!(
            "\n[Lines {}-{} of {} total]",
            start + 1,
            end,
            total_lines
        ));
        return result;
    }

    if content.len() > MAX_READ_CHARS {
        let mut result = String::new();
        let mut char_count = 0;
        let mut shown = 0;
        for (i, line) in lines.iter().enumerate() {
            let formatted = format!("{:>6}|{}\n", i + 1, line);
            char_count += formatted.len();
            if char_count > MAX_READ_CHARS {
                break;
            }
            result.push_str(&formatted);
            shown = i + 1;
        }
        result.push_str(&format!(
            "\n... (truncated, showing {shown} of {total_lines} lines, {} chars total)",
            content.len()
        ));
        return result;
    }

    let mut result = String::with_capacity(content.len() + total_lines * 8 + 30);
    for (i, line) in lines.iter().enumerate() {
        result.push_str(&format!("{:>6}|{}\n", i + 1, line));
    }
    result.push_str(&format!("\n[{total_lines} lines total]"));
    result
}

/// 解析文件路径：处理波浪号展开和相对路径。
///
/// - 先用 shellexpand::tilde 处理 ~ 符号
/// - 绝对路径直接返回
/// - 相对路径基于 workspace 目录拼接
/// - 没有 workspace 时，相对路径原样返回
fn resolve_path(path: &str, workspace: Option<&Path>) -> PathBuf {
    let p = PathBuf::from(shellexpand::tilde(path).as_ref());
    if p.is_absolute() {
        p
    } else if let Some(ws) = workspace {
        ws.join(p) // 相对路径 → 工作区路径 + 相对路径
    } else {
        p
    }
}

/// Best-effort canonicalize: if the full path exists, canonicalize it.
/// Otherwise, canonicalize the deepest existing ancestor and append the rest.
fn canonicalize_best_effort(path: &PathBuf) -> Result<PathBuf, String> {
    if path.exists() {
        path.canonicalize()
            .map_err(|e| format!("Error: Failed to resolve path: {e}"))
    } else {
        let mut existing = path.clone();
        let mut tail_components = Vec::new();
        while !existing.exists() {
            if let Some(file_name) = existing.file_name() {
                tail_components.push(file_name.to_os_string());
            } else {
                return Ok(path.clone());
            }
            existing = match existing.parent() {
                Some(p) => p.to_path_buf(),
                None => return Ok(path.clone()),
            };
        }
        let mut canonical = existing
            .canonicalize()
            .map_err(|e| format!("Error: Failed to resolve path: {e}"))?;
        for component in tail_components.into_iter().rev() {
            canonical.push(component);
        }
        Ok(canonical)
    }
}

/// Canonicalize a path and verify it is within the workspace.
fn safe_resolve(path: &str, workspace: Option<&Path>) -> Result<PathBuf, String> {
    let resolved = resolve_path(path, workspace);
    let canonical = canonicalize_best_effort(&resolved)?;

    if let Some(ws) = workspace {
        let canonical_ws = ws.canonicalize()
            .map_err(|e| format!("Error: workspace not accessible: {e}"))?;
        if !canonical.starts_with(&canonical_ws) {
            return Err(format!(
                "Error: Path must be within workspace: {}",
                canonical_ws.display()
            ));
        }
    }

    Ok(canonical)
}

// ── ReadFileTool ──────────────────────────────────────────────
// 读取文件内容工具

/// 文件读取工具 —— 读取指定路径文件的全部内容。
///
/// 风险等级：Read（只读，所有角色可用）
/// 超过 128KB 的内容会被截断。
pub struct ReadFileTool {
    workspace: Option<PathBuf>, // 工作区根目录
}

impl ReadFileTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read file contents with line numbers (format: LINE_NUMBER|CONTENT). \
         Supports offset/limit for partial reads. Binary files are detected automatically. \
         For image files (jpg/png/gif/webp), the content is returned as a vision input \
         so you can directly see and analyze the image. \
         Always use this before editing a file, and prefer this over exec cat/head/tail."
    }

    /// read_file 输出压缩：合并连续空行，大文件时去掉纯注释块
    fn compress_output(&self, output: &str, params: &HashMap<String, Value>) -> String {
        let lines: Vec<&str> = output.lines().collect();
        // 小文件不压缩
        if lines.len() < 100 {
            return output.to_string();
        }

        // 检测文件类型（从 path 参数推断）
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let is_code = path.ends_with(".rs")
            || path.ends_with(".py")
            || path.ends_with(".js")
            || path.ends_with(".ts")
            || path.ends_with(".go")
            || path.ends_with(".java")
            || path.ends_with(".c")
            || path.ends_with(".cpp");

        let mut result: Vec<&str> = Vec::new();
        let mut consecutive_empty = 0u32;
        let mut in_block_comment = false;
        let mut block_comment_lines = 0u32;

        for line in &lines {
            // LINE_NUMBER|CONTENT 格式，提取 content 部分
            let content = line.split_once('|').map(|(_, c)| c).unwrap_or(line);
            let trimmed = content.trim();

            // 合并连续空行（最多保留 1 行）
            if trimmed.is_empty() {
                consecutive_empty += 1;
                if consecutive_empty <= 1 {
                    result.push(line);
                }
                continue;
            }
            consecutive_empty = 0;

            // 代码文件：压缩大段块注释（超过 5 行的注释块只保留首尾）
            if is_code {
                if trimmed.starts_with("/*") || trimmed.starts_with("/**") || trimmed.starts_with("\"\"\"") || trimmed.starts_with("'''") {
                    in_block_comment = true;
                    block_comment_lines = 0;
                }
                if in_block_comment {
                    block_comment_lines += 1;
                    if block_comment_lines <= 2 {
                        result.push(line);
                    } else if trimmed.ends_with("*/") || trimmed.ends_with("\"\"\"") || trimmed.ends_with("'''") {
                        result.push(&"     |  ... (comment block)");
                        result.push(line);
                        in_block_comment = false;
                    }
                    continue;
                }
            }

            result.push(line);
        }

        result.join("\n")
    }

    /// 参数 Schema：path（必填）、offset（可选，起始行号，从1开始）、limit（可选，读取行数）
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The file path to read" },
                "offset": { "type": "integer", "description": "Starting line number (1-based). Defaults to 1." },
                "limit": { "type": "integer", "description": "Number of lines to read. Defaults to all lines." }
            },
            "required": ["path"]
        })
    }

    /// 执行文件读取。
    ///
    /// 检查项：文件是否存在、是否为文件（非目录）。
    /// 支持 offset/limit 按行读取；无参数时读取全部内容（超过 128KB 截断）。
    fn should_sandbox(&self) -> bool {
        true
    }

    async fn execute_in_sandbox(
        &self,
        sandbox: &dyn Sandbox,
        params: HashMap<String, Value>,
    ) -> String {
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: Missing 'path' parameter".into(),
        };
        let req = parse_read_request(&params);
        let stat = match sandbox.stat(path).await {
            Ok(stat) => stat,
            Err(e) => return format!("Error: {e}"),
        };
        if !stat.exists || !stat.is_file {
            return render_read_file_output(path, stat, Vec::new(), req);
        }
        match sandbox.read_file(path).await {
            Ok(bytes) => render_read_file_output(path, stat, bytes, req),
            Err(e) => format!("Error: {e}"),
        }
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return "Error: Missing 'path' parameter".into(),
        };
        let req = parse_read_request(&params);

        let ws = self.workspace.clone();
        tokio::task::spawn_blocking(move || {
            let file_path = match safe_resolve(&path, ws.as_deref()) {
                Ok(p) => p,
                Err(e) => return e,
            };

            let stat = match std::fs::metadata(&file_path) {
                Ok(meta) => SandboxFileStat {
                    exists: true,
                    is_file: meta.is_file(),
                    is_dir: meta.is_dir(),
                    size: Some(meta.len()),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => SandboxFileStat {
                    exists: false,
                    is_file: false,
                    is_dir: false,
                    size: None,
                },
                Err(e) => return format!("Error reading file metadata: {e}"),
            };

            if !stat.exists || !stat.is_file {
                return render_read_file_output(&path, stat, Vec::new(), req);
            }

            let raw_bytes = match std::fs::read(&file_path) {
                Ok(b) => b,
                Err(e) => return format!("Error reading file: {e}"),
            };
            render_read_file_output(&path, stat, raw_bytes, req)
        })
        .await
        .unwrap_or_else(|e| format!("Error: Task failed: {e}"))
    }
}

// ── WriteFileTool ─────────────────────────────────────────────
// 写入文件内容工具

/// 文件写入工具 —— 将内容写入指定路径的文件。
///
/// 风险等级：Write（需要 Member 及以上角色）
/// 会自动创建不存在的父目录。
pub struct WriteFileTool {
    workspace: Option<PathBuf>,
}

impl WriteFileTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file. Automatically creates all parent directories — do NOT mkdir before calling this tool. \
         Prefer this for new files or full rewrites; prefer edit_file or apply_patch for targeted changes."
    }

    /// 参数 Schema：需要 path（文件路径）和 content（写入内容）
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The file path to write to" },
                "content": { "type": "string", "description": "The content to write" }
            },
            "required": ["path", "content"]
        })
    }

    /// 写入操作的风险等级为 Write
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Write
    }

    /// 执行文件写入。
    ///
    /// 自动创建不存在的父目录，写入成功后返回写入的字节数。
    fn should_sandbox(&self) -> bool {
        true
    }

    async fn execute_in_sandbox(
        &self,
        sandbox: &dyn Sandbox,
        params: HashMap<String, Value>,
    ) -> String {
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: Missing 'path' parameter".into(),
        };
        let content = match params.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return "Error: Missing 'content' parameter".into(),
        };
        match sandbox.write_file(path, content.as_bytes()).await {
            Ok(()) => format!("Successfully wrote {} bytes to {}", content.len(), path),
            Err(e) => format!("Error: {e}"),
        }
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return "Error: Missing 'path' parameter".into(),
        };
        let content = match params.get("content").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return "Error: Missing 'content' parameter".into(),
        };

        let ws = self.workspace.clone();
        tokio::task::spawn_blocking(move || {
            // For write_file, the file may not exist yet, so use safe_resolve
            // which handles non-existent paths by canonicalizing the parent.
            let file_path = match safe_resolve(&path, ws.as_deref()) {
                Ok(p) => p,
                Err(e) => return e,
            };

            // 自动创建父目录
            if let Some(parent) = file_path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return format!("Error creating directories: {e}");
                }
            }

            match std::fs::write(&file_path, &content) {
                Ok(()) => format!(
                    "Successfully wrote {} bytes to {}",
                    content.len(),
                    file_path.display()
                ),
                Err(e) => format!("Error writing file: {e}"),
            }
        })
        .await
        .unwrap_or_else(|e| format!("Error: Task failed: {e}"))
    }
}

// ── EditFileTool ──────────────────────────────────────────────
// 文件编辑工具（查找替换）

/// 文件编辑工具 —— 通过精确查找替换来修改文件内容。
///
/// 风险等级：Write
/// 安全机制：
/// - old_text 必须在文件中精确存在
/// - 如果 old_text 出现多次，拒绝执行（要求提供更多上下文以唯一定位）
/// - 只替换第一次出现的匹配
pub struct EditFileTool {
    workspace: Option<PathBuf>,
}

impl EditFileTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing old_text with new_text. Use this for one localized change or one exact replacement. \
         Set replace_all=true to replace all occurrences (e.g. renaming a variable). Prefer apply_patch for multiple hunks."
    }

    /// 参数 Schema：需要 path、old_text、new_text，可选 replace_all
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The file path to edit" },
                "old_text": { "type": "string", "description": "The exact text to find" },
                "new_text": { "type": "string", "description": "The replacement text" },
                "replace_all": { "type": "boolean", "description": "If true, replace ALL occurrences (useful for renaming variables). Default: false (single replacement, requires unique match)." }
            },
            "required": ["path", "old_text", "new_text"]
        })
    }

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
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: Missing 'path' parameter".into(),
        };
        let old_text = match params.get("old_text").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return "Error: Missing 'old_text' parameter".into(),
        };
        let new_text = match params.get("new_text").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return "Error: Missing 'new_text' parameter".into(),
        };
        let replace_all = params
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        match sandbox.read_file(path).await {
            Ok(bytes) => {
                let content = String::from_utf8_lossy(&bytes).to_string();
                if !content.contains(old_text) {
                    return format!("Error: old_text not found in {path}");
                }
                let count = content.matches(old_text).count();
                if count > 1 && !replace_all {
                    return format!("Warning: old_text appears {count} times. Provide more context, or set replace_all=true.");
                }
                let new_content = if replace_all {
                    content.replace(old_text, new_text)
                } else {
                    content.replacen(old_text, new_text, 1)
                };
                match sandbox.write_file(path, new_content.as_bytes()).await {
                    Ok(()) => {
                        if replace_all && count > 1 {
                            format!("Successfully edited {} ({} replacements)", path, count)
                        } else {
                            format!("Successfully edited {}", path)
                        }
                    }
                    Err(e) => format!("Error writing: {e}"),
                }
            }
            Err(e) => format!("Error reading: {e}"),
        }
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return "Error: Missing 'path' parameter".into(),
        };
        let old_text = match params.get("old_text").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return "Error: Missing 'old_text' parameter".into(),
        };
        let new_text = match params.get("new_text").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return "Error: Missing 'new_text' parameter".into(),
        };
        let replace_all = params
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let ws = self.workspace.clone();
        tokio::task::spawn_blocking(move || {
            let file_path = match safe_resolve(&path, ws.as_deref()) {
                Ok(p) => p,
                Err(e) => return e,
            };

            if !file_path.exists() {
                return format!("Error: File not found: {path}");
            }

            let content = match std::fs::read_to_string(&file_path) {
                Ok(c) => c,
                Err(e) => return format!("Error reading file: {e}"),
            };

            // 检查 old_text 是否存在
            if !content.contains(&*old_text) {
                return format!("Error: old_text not found in {path}");
            }

            let count = content.matches(&*old_text).count();

            let new_content = if replace_all {
                // 全部替换模式（适合重命名变量等）
                content.replace(&*old_text, &new_text)
            } else {
                // 单次替换模式（默认，要求唯一匹配）
                if count > 1 {
                    return format!("Warning: old_text appears {count} times. Provide more context, or set replace_all=true.");
                }
                content.replacen(&*old_text, &new_text, 1)
            };
            let pos = match content.find(&*old_text) {
                Some(p) => p,
                None => return format!("Error: old_text disappeared unexpectedly in {path}"),
            };
            let start_line = content[..pos].matches('\n').count() + 1;
            let old_lines = old_text.lines().count().max(1);
            let new_lines = new_text.lines().count().max(1);
            let end_line = start_line + old_lines - 1;

            match std::fs::write(&file_path, new_content) {
                Ok(()) => {
                    if replace_all && count > 1 {
                        format!(
                            "Successfully edited {} ({} replacements, first at line {})",
                            file_path.display(), count, start_line
                        )
                    } else {
                        format!(
                            "Successfully edited {} (lines {}-{}, {} → {} lines)",
                            file_path.display(), start_line, end_line, old_lines, new_lines
                        )
                    }
                }
                Err(e) => format!("Error writing file: {e}"),
            }
        })
        .await
        .unwrap_or_else(|e| format!("Error: Task failed: {e}"))
    }
}

// ── DeleteFileTool ────────────────────────────────────────────

/// 文件删除工具 —— 删除工作区内的单个文件。
pub struct DeleteFileTool {
    workspace: Option<PathBuf>,
}

impl DeleteFileTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for DeleteFileTool {
    fn name(&self) -> &str {
        "delete_file"
    }

    fn description(&self) -> &str {
        "Delete a single file inside the workspace. Use this for explicit file removal instead of exec rm. \
         Do not use it for directories."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The file path to delete. Must refer to a file, not a directory." }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Dangerous
    }

    fn should_sandbox(&self) -> bool {
        true
    }

    async fn execute_in_sandbox(
        &self,
        sandbox: &dyn Sandbox,
        params: HashMap<String, Value>,
    ) -> String {
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: Missing 'path' parameter".into(),
        };

        let stat = match sandbox.stat(path).await {
            Ok(stat) => stat,
            Err(e) => return format!("Error: {e}"),
        };
        if !stat.exists {
            return format!("File already absent: {path}");
        }
        if stat.is_dir {
            return format!("Error: Refusing to delete directory with delete_file: {path}");
        }
        if !stat.is_file {
            return format!("Error: Not a regular file: {path}");
        }

        match sandbox.remove_file(path).await {
            Ok(()) => format!("Successfully deleted {}", path),
            Err(e) => format!("Error: {e}"),
        }
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return "Error: Missing 'path' parameter".into(),
        };

        let ws = self.workspace.clone();
        tokio::task::spawn_blocking(move || {
            let file_path = match safe_resolve(&path, ws.as_deref()) {
                Ok(p) => p,
                Err(e) => return e,
            };

            match std::fs::metadata(&file_path) {
                Ok(meta) => {
                    if meta.is_dir() {
                        return format!(
                            "Error: Refusing to delete directory with delete_file: {path}"
                        );
                    }
                    if !meta.is_file() {
                        return format!("Error: Not a regular file: {path}");
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return format!("File already absent: {path}");
                }
                Err(e) => return format!("Error reading file metadata: {e}"),
            }

            match std::fs::remove_file(&file_path) {
                Ok(()) => format!("Successfully deleted {}", file_path.display()),
                Err(e) => format!("Error deleting file: {e}"),
            }
        })
        .await
        .unwrap_or_else(|e| format!("Error: Task failed: {e}"))
    }
}

// ── ApplyPatchTool ────────────────────────────────────────────

fn apply_patch_hunks(
    mut content: String,
    hunks: &[Value],
    path: &str,
) -> Result<(String, usize), String> {
    let mut applied = 0usize;

    for (idx, hunk) in hunks.iter().enumerate() {
        let action = hunk
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("replace");
        let old_text = hunk
            .get("old_text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("Error: Missing old_text in hunk {}", idx + 1))?;
        let new_text = hunk.get("new_text").and_then(|v| v.as_str()).unwrap_or("");
        let replace_all = hunk
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        match action {
            "replace" | "insert_before" | "insert_after" | "delete" => {}
            _ => {
                return Err(format!(
                    "Error: Unsupported action '{}' in hunk {}",
                    action,
                    idx + 1
                ))
            }
        }

        if matches!(action, "replace" | "insert_before" | "insert_after")
            && !hunk.get("new_text").is_some()
        {
            return Err(format!(
                "Error: Missing new_text for '{}' in hunk {}",
                action,
                idx + 1
            ));
        }

        if !content.contains(old_text) {
            return Err(format!(
                "Error: Hunk {} old_text not found in {}",
                idx + 1,
                path
            ));
        }
        let count = content.matches(old_text).count();
        if count > 1 && !replace_all {
            return Err(format!(
                "Error: Hunk {} old_text appears {} times in {}. Provide more context or set replace_all=true.",
                idx + 1,
                count,
                path
            ));
        }

        let replacement = match action {
            "replace" => new_text.to_string(),
            "insert_before" => format!("{new_text}{old_text}"),
            "insert_after" => format!("{old_text}{new_text}"),
            "delete" => String::new(),
            _ => unreachable!(),
        };

        content = if replace_all {
            content.replace(old_text, &replacement)
        } else {
            content.replacen(old_text, &replacement, 1)
        };
        applied += if replace_all { count } else { 1 };
    }

    Ok((content, applied))
}

/// 强补丁编辑工具 —— 支持在同一文件上应用多个精确替换 hunk。
pub struct ApplyPatchTool {
    workspace: Option<PathBuf>,
}

impl ApplyPatchTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply multiple targeted patch operations to one existing file. Supports replace, insert_before, \
         insert_after, and delete actions anchored on exact text. Use this for multi-step or multi-hunk edits \
         when edit_file would require several sequential replacements. Prefer edit_file for a single localized change."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The file path to patch" },
                "operations": {
                    "type": "array",
                    "description": "Ordered patch operations to apply to the file. Preferred over hunks.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "action": {
                                "type": "string",
                                "enum": ["replace", "insert_before", "insert_after", "delete"],
                                "description": "Patch action. Default: replace."
                            },
                            "old_text": { "type": "string", "description": "Exact anchor text to find for this operation" },
                            "new_text": { "type": "string", "description": "Replacement or inserted text. Required except for delete." },
                            "replace_all": { "type": "boolean", "description": "If true, apply the operation to all matches. Default: false." }
                        },
                        "required": ["old_text"]
                    }
                },
                "hunks": {
                    "type": "array",
                    "description": "Legacy alias for operations. Ordered patch hunks to apply to the file.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "action": {
                                "type": "string",
                                "enum": ["replace", "insert_before", "insert_after", "delete"],
                                "description": "Patch action. Default: replace."
                            },
                            "old_text": { "type": "string", "description": "Exact anchor text to find for this hunk" },
                            "new_text": { "type": "string", "description": "Replacement or inserted text. Required except for delete." },
                            "replace_all": { "type": "boolean", "description": "If true, apply the operation to all matches. Default: false." }
                        },
                        "required": ["old_text"]
                    }
                }
            },
            "required": ["path"]
        })
    }

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
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: Missing 'path' parameter".into(),
        };
        let hunks = match params
            .get("operations")
            .or_else(|| params.get("hunks"))
            .and_then(|v| v.as_array())
        {
            Some(hunks) if !hunks.is_empty() => hunks,
            Some(_) => return "Error: 'operations' must not be empty".into(),
            None => return "Error: Missing 'operations' parameter".into(),
        };

        let stat = match sandbox.stat(path).await {
            Ok(stat) => stat,
            Err(e) => return format!("Error: {e}"),
        };
        if !stat.exists {
            return format!("Error: File not found: {path}");
        }
        if stat.is_dir {
            return format!("Error: Not a file: {path}");
        }

        let bytes = match sandbox.read_file(path).await {
            Ok(bytes) => bytes,
            Err(e) => return format!("Error reading: {e}"),
        };
        let content = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => return "Error: apply_patch only supports UTF-8 text files".into(),
        };

        let (new_content, applied) = match apply_patch_hunks(content, hunks, path) {
            Ok(result) => result,
            Err(e) => return e,
        };

        match sandbox.write_file(path, new_content.as_bytes()).await {
            Ok(()) => format!(
                "Successfully applied {} patch operations to {}",
                applied, path
            ),
            Err(e) => format!("Error writing: {e}"),
        }
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return "Error: Missing 'path' parameter".into(),
        };
        let hunks = match params
            .get("operations")
            .or_else(|| params.get("hunks"))
            .and_then(|v| v.as_array())
        {
            Some(hunks) if !hunks.is_empty() => hunks.clone(),
            Some(_) => return "Error: 'operations' must not be empty".into(),
            None => return "Error: Missing 'operations' parameter".into(),
        };

        let ws = self.workspace.clone();
        tokio::task::spawn_blocking(move || {
            let file_path = match safe_resolve(&path, ws.as_deref()) {
                Ok(p) => p,
                Err(e) => return e,
            };
            if !file_path.exists() {
                return format!("Error: File not found: {path}");
            }
            if !file_path.is_file() {
                return format!("Error: Not a file: {path}");
            }

            let content = match std::fs::read_to_string(&file_path) {
                Ok(c) => c,
                Err(e) => return format!("Error reading file: {e}"),
            };

            let (new_content, applied) = match apply_patch_hunks(content, &hunks, &path) {
                Ok(result) => result,
                Err(e) => return e,
            };

            match std::fs::write(&file_path, new_content) {
                Ok(()) => format!(
                    "Successfully applied {} patch operations to {}",
                    applied,
                    file_path.display()
                ),
                Err(e) => format!("Error writing file: {e}"),
            }
        })
        .await
        .unwrap_or_else(|e| format!("Error: Task failed: {e}"))
    }
}

// ── ListDirTool ───────────────────────────────────────────────

const LIST_MAX_ENTRIES: usize = 500;
const LIST_DEFAULT_MAX_DEPTH: usize = 3;
const LIST_SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    ".tox",
    "dist",
    "build",
];

fn collect_local_walk_entries(
    dir: &Path,
    base: &Path,
    depth: usize,
    max_depth: usize,
    items: &mut Vec<SandboxWalkEntry>,
    max_entries: usize,
) {
    if depth > max_depth || items.len() >= max_entries {
        return;
    }
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(e) => e.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        if items.len() >= max_entries {
            break;
        }
        let path = entry.path();
        let rel = path.strip_prefix(base).unwrap_or(&path);

        if path.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if LIST_SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            items.push(SandboxWalkEntry {
                path: rel.to_string_lossy().replace('\\', "/"),
                is_dir: true,
                depth: depth + 1,
            });
            collect_local_walk_entries(&path, base, depth + 1, max_depth, items, max_entries);
        } else {
            items.push(SandboxWalkEntry {
                path: rel.to_string_lossy().replace('\\', "/"),
                is_dir: false,
                depth: depth + 1,
            });
        }
    }
}

fn render_list_dir_entries(path: &str, entries: Vec<SandboxWalkEntry>, recursive: bool) -> String {
    let mut items: Vec<String> = entries
        .into_iter()
        .filter(|entry| {
            !entry
                .path
                .split('/')
                .any(|component| LIST_SKIP_DIRS.contains(&component))
        })
        .map(|entry| {
            let prefix = if entry.is_dir { "dir  " } else { "file " };
            format!("{prefix}{}", entry.path)
        })
        .collect();
    items.sort();

    if items.is_empty() {
        return format!("Directory {path} is empty");
    }

    if recursive {
        let total = items.len();
        let result = items.join("\n");
        if total >= LIST_MAX_ENTRIES {
            format!("{result}\n... (truncated at {LIST_MAX_ENTRIES} entries)")
        } else {
            format!("{total} entries:\n{result}")
        }
    } else {
        items.join("\n")
    }
}

/// 目录列表工具 —— 列出指定目录下的所有文件和子目录。
///
/// 风险等级：Read（只读）
/// 支持递归遍历和深度控制。
pub struct ListDirTool {
    workspace: Option<PathBuf>,
}

impl ListDirTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List directory contents. Each entry prefixed with 'file' or 'dir'. \
         Set recursive=true to walk subdirectories (skips .git, node_modules, target, etc.). \
         Use max_depth to control recursion depth. Prefer this for understanding directory structure; \
         for finding files by name or path pattern, prefer glob."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The directory path" },
                "recursive": { "type": "boolean", "description": "List contents recursively. Default: false." },
                "max_depth": { "type": "integer", "description": "Max recursion depth (default: 3). Only used when recursive=true." }
            },
            "required": ["path"]
        })
    }

    fn should_sandbox(&self) -> bool {
        true
    }

    async fn execute_in_sandbox(
        &self,
        sandbox: &dyn Sandbox,
        params: HashMap<String, Value>,
    ) -> String {
        let path = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let recursive = params
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max_depth = params
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(LIST_DEFAULT_MAX_DEPTH);

        let stat = match sandbox.stat(path).await {
            Ok(stat) => stat,
            Err(e) => return format!("Error: {e}"),
        };
        if !stat.exists {
            return format!("Error: Directory not found: {path}");
        }
        if !stat.is_dir {
            return format!("Error: Not a directory: {path}");
        }

        let result = if recursive {
            sandbox.walk_dir(path, max_depth).await
        } else {
            sandbox.list_dir(path).await.map(|entries| {
                entries
                    .into_iter()
                    .map(|e| SandboxWalkEntry {
                        path: e.name,
                        is_dir: e.is_dir,
                        depth: 1,
                    })
                    .collect()
            })
        };

        match result {
            Ok(entries) => render_list_dir_entries(path, entries, recursive),
            Err(e) => format!("Error: {e}"),
        }
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return "Error: Missing 'path' parameter".into(),
        };
        let recursive = params
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max_depth = params
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(LIST_DEFAULT_MAX_DEPTH);

        let ws = self.workspace.clone();
        tokio::task::spawn_blocking(move || {
            let dir_path = match safe_resolve(&path, ws.as_deref()) {
                Ok(p) => p,
                Err(e) => return e,
            };

            if !dir_path.exists() {
                return format!("Error: Directory not found: {path}");
            }
            if !dir_path.is_dir() {
                return format!("Error: Not a directory: {path}");
            }

            if recursive {
                let mut items = Vec::new();
                collect_local_walk_entries(
                    &dir_path,
                    &dir_path,
                    0,
                    max_depth,
                    &mut items,
                    LIST_MAX_ENTRIES,
                );
                render_list_dir_entries(&path, items, true)
            } else {
                match std::fs::read_dir(&dir_path) {
                    Ok(entries) => {
                        let items: Vec<SandboxWalkEntry> = entries
                            .filter_map(|e| e.ok())
                            .map(|e| SandboxWalkEntry {
                                path: e.file_name().to_string_lossy().replace('\\', "/"),
                                is_dir: e.path().is_dir(),
                                depth: 1,
                            })
                            .collect();
                        render_list_dir_entries(&path, items, false)
                    }
                    Err(e) => format!("Error listing directory: {e}"),
                }
            }
        })
        .await
        .unwrap_or_else(|e| format!("Error: Task failed: {e}"))
    }
}

// ── SendFileTool ──────────────────────────────────────────────
// ── PendingFileStore ──────────────────────────────────────────
// 按请求隔离的待发送文件存储，支持并发 agent loop。

/// 按请求 ID 隔离的待发送文件存储。
///
/// 每次 agent loop 执行前分配一个 request_id，工具层通过 task-local 获取
/// 当前 request_id 并将文件路径存入对应槽位，执行结束后 drain 取走。
pub struct PendingFileStore {
    inner: Mutex<HashMap<u64, Vec<String>>>,
    next_id: std::sync::atomic::AtomicU64,
}

impl PendingFileStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            next_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// 分配一个新的请求 ID。
    pub fn new_request(&self) -> u64 {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut map = self.inner.lock();
        map.insert(id, Vec::new());
        id
    }

    /// 向指定请求追加一个待发送文件路径。
    pub fn push(&self, request_id: u64, path: String) {
        let mut map = self.inner.lock();
        map.entry(request_id).or_default().push(path);
    }

    /// 取走并返回指定请求的所有待发送文件。
    pub fn drain(&self, request_id: u64) -> Vec<String> {
        let mut map = self.inner.lock();
        map.remove(&request_id).unwrap_or_default()
    }
}

tokio::task_local! {
    /// 当前 agent loop 执行的请求 ID（用于 pending_files 隔离）。
    pub static CURRENT_REQUEST_ID: u64;
}

// 标记文件待发送给用户

/// 文件发送工具 —— 标记一个文件待发送给用户。
///
/// 实际发送由上层（如 DingTalk bot）在 Agent 完成后执行。
/// 此工具仅验证文件存在并记录路径。
pub struct SendFileTool {
    workspace: Option<PathBuf>,
    /// 按请求隔离的待发送文件存储。
    pending_files: Arc<PendingFileStore>,
}

impl SendFileTool {
    pub fn new(workspace: Option<PathBuf>, pending_files: Arc<PendingFileStore>) -> Self {
        Self {
            workspace,
            pending_files,
        }
    }
}

#[async_trait]
impl Tool for SendFileTool {
    fn name(&self) -> &str {
        "send_file"
    }

    fn description(&self) -> &str {
        "Send a file to the user. The file will be delivered through the current channel (e.g., DingTalk). \
         Use this after creating a file with write_file when you want the user to receive it as a downloadable attachment."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path to send to the user"
                }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Read
    }

    fn should_sandbox(&self) -> bool {
        true
    }

    async fn execute_in_sandbox(
        &self,
        sandbox: &dyn Sandbox,
        params: HashMap<String, Value>,
    ) -> String {
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: Missing 'path' parameter".into(),
        };
        if !sandbox.file_exists(path).await {
            return format!("Error: File not found in sandbox: {path}");
        }
        let tmp_dir = std::env::temp_dir().join("tyclaw_send");
        let filename = std::path::Path::new(path)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".into());
        let host_path = tmp_dir.join(&filename);
        if let Err(e) = sandbox.copy_from(path, &host_path).await {
            return format!("Error copying from sandbox: {e}");
        }
        let abs_path = host_path.to_string_lossy().to_string();
        let request_id = CURRENT_REQUEST_ID.try_with(|id| *id).unwrap_or(0);
        self.pending_files.push(request_id, abs_path.clone());
        format!("File queued for sending: {abs_path}")
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return "Error: Missing 'path' parameter".into(),
        };

        let ws = self.workspace.clone();
        let store = self.pending_files.clone();
        let request_id = CURRENT_REQUEST_ID.try_with(|id| *id).unwrap_or(0);
        tokio::task::spawn_blocking(move || {
            let file_path = match safe_resolve(&path, ws.as_deref()) {
                Ok(p) => p,
                Err(e) => return e,
            };

            if !file_path.exists() {
                return format!("Error: File not found: {path}");
            }
            if !file_path.is_file() {
                return format!("Error: Not a file: {path}");
            }

            let abs_path = file_path.to_string_lossy().to_string();
            store.push(request_id, abs_path.clone());

            format!("File queued for sending: {abs_path}")
        })
        .await
        .unwrap_or_else(|e| format!("Error: Task failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::fs;
    use std::time::Duration;
    use tempfile::TempDir;
    use tyclaw_tool_abi::{
        SandboxDirEntry, SandboxExecResult, SandboxGlobEntry, SandboxGrepRequest,
        SandboxGrepResponse, SandboxWalkEntry,
    };

    struct TestSandbox {
        root: PathBuf,
        id: String,
    }

    impl TestSandbox {
        fn new(root: PathBuf) -> Self {
            Self {
                root,
                id: "test-sandbox".into(),
            }
        }
    }

    #[async_trait]
    impl Sandbox for TestSandbox {
        async fn exec(
            &self,
            _cmd: &str,
            _timeout: Duration,
        ) -> Result<SandboxExecResult, tyclaw_types::TyclawError> {
            unimplemented!("exec is not needed for filesystem tool tests")
        }

        async fn stat(&self, path: &str) -> Result<SandboxFileStat, tyclaw_types::TyclawError> {
            let full = self.root.join(path);
            match tokio::fs::metadata(&full).await {
                Ok(meta) => Ok(SandboxFileStat {
                    exists: true,
                    is_file: meta.is_file(),
                    is_dir: meta.is_dir(),
                    size: Some(meta.len()),
                }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SandboxFileStat {
                    exists: false,
                    is_file: false,
                    is_dir: false,
                    size: None,
                }),
                Err(e) => Err(tyclaw_types::TyclawError::Tool {
                    tool: "test_stat".into(),
                    message: e.to_string(),
                }),
            }
        }

        async fn read_file(&self, path: &str) -> Result<Vec<u8>, tyclaw_types::TyclawError> {
            tokio::fs::read(self.root.join(path)).await.map_err(|e| {
                tyclaw_types::TyclawError::Tool {
                    tool: "test_read".into(),
                    message: e.to_string(),
                }
            })
        }

        async fn write_file(
            &self,
            path: &str,
            content: &[u8],
        ) -> Result<(), tyclaw_types::TyclawError> {
            let full = self.root.join(path);
            if let Some(parent) = full.parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
            tokio::fs::write(full, content)
                .await
                .map_err(|e| tyclaw_types::TyclawError::Tool {
                    tool: "test_write".into(),
                    message: e.to_string(),
                })
        }

        async fn create_dir(&self, path: &str) -> Result<(), tyclaw_types::TyclawError> {
            tokio::fs::create_dir_all(self.root.join(path))
                .await
                .map_err(|e| tyclaw_types::TyclawError::Tool {
                    tool: "test_mkdir".into(),
                    message: e.to_string(),
                })
        }

        async fn list_dir(
            &self,
            path: &str,
        ) -> Result<Vec<SandboxDirEntry>, tyclaw_types::TyclawError> {
            let mut entries = Vec::new();
            let mut rd = tokio::fs::read_dir(self.root.join(path))
                .await
                .map_err(|e| tyclaw_types::TyclawError::Tool {
                    tool: "test_list".into(),
                    message: e.to_string(),
                })?;
            while let Some(entry) =
                rd.next_entry()
                    .await
                    .map_err(|e| tyclaw_types::TyclawError::Tool {
                        tool: "test_list".into(),
                        message: e.to_string(),
                    })?
            {
                entries.push(SandboxDirEntry {
                    name: entry.file_name().to_string_lossy().to_string(),
                    is_dir: entry.path().is_dir(),
                });
            }
            Ok(entries)
        }

        async fn walk_dir(
            &self,
            path: &str,
            max_depth: usize,
        ) -> Result<Vec<SandboxWalkEntry>, tyclaw_types::TyclawError> {
            let base = self.root.join(path);
            let entries = tokio::task::spawn_blocking(move || {
                fn walk(
                    dir: &std::path::Path,
                    base: &std::path::Path,
                    depth: usize,
                    max_depth: usize,
                    items: &mut Vec<SandboxWalkEntry>,
                ) {
                    if depth > max_depth {
                        return;
                    }
                    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
                        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
                        Err(_) => return,
                    };
                    entries.sort_by_key(|entry| entry.file_name());
                    for entry in entries {
                        let path = entry.path();
                        let rel = path.strip_prefix(base).unwrap_or(&path);
                        let is_dir = path.is_dir();
                        items.push(SandboxWalkEntry {
                            path: rel.to_string_lossy().replace('\\', "/"),
                            is_dir,
                            depth,
                        });
                        if is_dir {
                            walk(&path, base, depth + 1, max_depth, items);
                        }
                    }
                }

                let mut items = Vec::new();
                walk(&base, &base, 1, max_depth, &mut items);
                items
            })
            .await
            .map_err(|e| tyclaw_types::TyclawError::Tool {
                tool: "test_walk".into(),
                message: e.to_string(),
            })?;
            Ok(entries)
        }

        async fn grep_search(
            &self,
            _request: SandboxGrepRequest,
        ) -> Result<SandboxGrepResponse, tyclaw_types::TyclawError> {
            Ok(SandboxGrepResponse {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1,
            })
        }

        async fn glob_search(
            &self,
            _pattern: &str,
            _path: &str,
        ) -> Result<Vec<SandboxGlobEntry>, tyclaw_types::TyclawError> {
            Ok(Vec::new())
        }

        async fn file_exists(&self, path: &str) -> bool {
            self.root.join(path).exists()
        }

        async fn remove_file(&self, path: &str) -> Result<(), tyclaw_types::TyclawError> {
            tokio::fs::remove_file(self.root.join(path))
                .await
                .map_err(|e| tyclaw_types::TyclawError::Tool {
                    tool: "test_remove".into(),
                    message: e.to_string(),
                })
        }

        async fn copy_from(
            &self,
            _container_path: &str,
            _host_path: &PathBuf,
        ) -> Result<(), tyclaw_types::TyclawError> {
            unimplemented!("copy_from is not needed for filesystem tool tests")
        }

        fn workspace_root(&self) -> &str {
            self.root.to_str().unwrap_or(".")
        }

        fn id(&self) -> &str {
            &self.id
        }
    }

    /// 测试：写入后能正确读取文件
    #[tokio::test]
    async fn test_read_write_file() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().to_path_buf();

        // 写入文件
        let write_tool = WriteFileTool::new(Some(ws.clone()));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("test.txt"));
        params.insert("content".into(), json!("hello world"));
        let result = write_tool.execute(params).await;
        assert!(result.contains("Successfully wrote"));

        // 读取文件
        let read_tool = ReadFileTool::new(Some(ws));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("test.txt"));
        let result = read_tool.execute(params).await;
        assert!(result.contains("hello world"));
        assert!(result.contains("1|"));
    }

    /// 测试：文件编辑（查找替换）
    #[tokio::test]
    async fn test_edit_file() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().to_path_buf();
        let file = ws.join("test.txt");
        fs::write(&file, "foo bar baz").unwrap();

        let tool = EditFileTool::new(Some(ws));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("test.txt"));
        params.insert("old_text".into(), json!("bar"));
        params.insert("new_text".into(), json!("qux"));
        let result = tool.execute(params).await;
        assert!(result.contains("Successfully edited"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "foo qux baz");
    }

    /// 测试：old_text 多次出现时应拒绝编辑
    #[tokio::test]
    async fn test_edit_multiple_matches() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        fs::write(&file, "aaa aaa aaa").unwrap();

        let tool = EditFileTool::new(Some(tmp.path().to_path_buf()));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("test.txt"));
        params.insert("old_text".into(), json!("aaa"));
        params.insert("new_text".into(), json!("bbb"));
        let result = tool.execute(params).await;
        assert!(result.contains("Warning"));
    }

    /// 测试：列目录功能（非递归）
    #[tokio::test]
    async fn test_list_dir() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().to_path_buf();
        fs::write(ws.join("a.txt"), "").unwrap();
        fs::create_dir(ws.join("subdir")).unwrap();

        let tool = ListDirTool::new(Some(ws));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("."));
        let result = tool.execute(params).await;
        assert!(result.contains("file a.txt"));
        assert!(result.contains("dir  subdir"));
    }

    /// 测试：列目录功能（递归）
    #[tokio::test]
    async fn test_list_dir_recursive() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().to_path_buf();
        fs::write(ws.join("root.txt"), "").unwrap();
        fs::create_dir_all(ws.join("sub/nested")).unwrap();
        fs::write(ws.join("sub/child.txt"), "").unwrap();
        fs::write(ws.join("sub/nested/deep.txt"), "").unwrap();
        fs::create_dir(ws.join("target")).unwrap();
        fs::write(ws.join("target/ignored.txt"), "").unwrap();

        let tool = ListDirTool::new(Some(ws));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("."));
        params.insert("recursive".into(), json!(true));
        let result = tool.execute(params).await;
        assert!(result.contains("file root.txt"));
        assert!(result.contains("file sub/child.txt") || result.contains("file sub\\child.txt"));
        assert!(result.contains("deep.txt"));
        assert!(!result.contains("target"), "should skip noisy directories");
    }

    /// 测试：递归深度限制
    #[tokio::test]
    async fn test_list_dir_max_depth() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().to_path_buf();
        fs::create_dir_all(ws.join("a/b/c")).unwrap();
        fs::write(ws.join("a/b/c/deep.txt"), "").unwrap();
        fs::write(ws.join("a/shallow.txt"), "").unwrap();

        let tool = ListDirTool::new(Some(ws));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("."));
        params.insert("recursive".into(), json!(true));
        params.insert("max_depth".into(), json!(1));
        let result = tool.execute(params).await;
        assert!(result.contains("shallow.txt"));
        assert!(
            !result.contains("deep.txt"),
            "depth=1 should not reach a/b/c/deep.txt"
        );
    }

    /// 测试：read_file 二进制文件检测
    #[tokio::test]
    async fn test_read_binary_file() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("binary.bin");
        fs::write(&file, b"\x00\x01\x02\x03binary data").unwrap();

        let tool = ReadFileTool::new(Some(tmp.path().to_path_buf()));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("binary.bin"));
        let result = tool.execute(params).await;
        assert!(result.contains("Binary file"));
    }

    /// 测试：read_file 全文带行号
    #[tokio::test]
    async fn test_read_file_line_numbers() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("code.rs");
        fs::write(&file, "fn main() {\n    println!(\"hello\");\n}\n").unwrap();

        let tool = ReadFileTool::new(Some(tmp.path().to_path_buf()));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("code.rs"));
        let result = tool.execute(params).await;
        assert!(result.contains("1|fn main()"));
        assert!(result.contains("2|"));
        assert!(result.contains("3|}"));
        assert!(result.contains("[3 lines total]"));
    }

    /// 测试：读取不存在的文件应返回错误
    #[tokio::test]
    async fn test_read_nonexistent() {
        let tool = ReadFileTool::new(None);
        let mut params = HashMap::new();
        params.insert("path".into(), json!("/tmp/__no_such_file_tyclaw__"));
        let result = tool.execute(params).await;
        assert!(result.contains("Error: File not found"));
    }

    /// 测试：read_file 带 offset 和 limit 读取指定行范围
    #[tokio::test]
    async fn test_read_file_offset_limit() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("lines.txt");
        fs::write(&file, "line1\nline2\nline3\nline4\nline5\n").unwrap();

        let tool = ReadFileTool::new(Some(tmp.path().to_path_buf()));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("lines.txt"));
        params.insert("offset".into(), json!(2));
        params.insert("limit".into(), json!(3));
        let result = tool.execute(params).await;
        // 应该包含 line2, line3, line4，带行号
        assert!(result.contains("line2"));
        assert!(result.contains("line3"));
        assert!(result.contains("line4"));
        assert!(!result.contains("line1"));
        assert!(!result.contains("line5"));
        assert!(result.contains("[Lines 2-4 of 5 total]"));
    }

    /// 测试：read_file 只指定 offset，读到文件末尾
    #[tokio::test]
    async fn test_read_file_offset_only() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("lines.txt");
        fs::write(&file, "a\nb\nc\nd\n").unwrap();

        let tool = ReadFileTool::new(Some(tmp.path().to_path_buf()));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("lines.txt"));
        params.insert("offset".into(), json!(3));
        let result = tool.execute(params).await;
        assert!(result.contains("c"));
        assert!(result.contains("d"));
        assert!(!result.contains("1|a"));
        assert!(result.contains("[Lines 3-4 of 4 total]"));
    }

    /// 测试：read_file offset 超出总行数应返回错误
    #[tokio::test]
    async fn test_read_file_offset_exceeds() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("lines.txt");
        fs::write(&file, "a\nb\n").unwrap();

        let tool = ReadFileTool::new(Some(tmp.path().to_path_buf()));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("lines.txt"));
        params.insert("offset".into(), json!(100));
        let result = tool.execute(params).await;
        assert!(result.contains("exceeds total lines"));
    }

    /// 测试：read_file 只指定 limit，从头读
    #[tokio::test]
    async fn test_read_file_limit_only() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("lines.txt");
        fs::write(&file, "a\nb\nc\nd\n").unwrap();

        let tool = ReadFileTool::new(Some(tmp.path().to_path_buf()));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("lines.txt"));
        params.insert("limit".into(), json!(2));
        let result = tool.execute(params).await;
        assert!(result.contains("a"));
        assert!(result.contains("b"));
        assert!(result.contains("[Lines 1-2 of 4 total]"));
    }

    #[tokio::test]
    async fn test_read_file_sandbox_parity() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("lines.txt");
        fs::write(&file, "line1\nline2\nline3\n").unwrap();

        let tool = ReadFileTool::new(Some(tmp.path().to_path_buf()));
        let sandbox = TestSandbox::new(tmp.path().to_path_buf());
        let mut params = HashMap::new();
        params.insert("path".into(), json!("lines.txt"));
        params.insert("offset".into(), json!(2));
        params.insert("limit".into(), json!(2));

        let host_result = tool.execute(params.clone()).await;
        let sandbox_result = tool.execute_in_sandbox(&sandbox, params).await;
        assert_eq!(host_result, sandbox_result);
    }

    #[tokio::test]
    async fn test_list_dir_sandbox_parity() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().to_path_buf();
        let sandbox = TestSandbox::new(ws.clone());
        fs::write(ws.join("root.txt"), "").unwrap();
        fs::create_dir_all(ws.join("sub/nested")).unwrap();
        fs::write(ws.join("sub/nested/deep.txt"), "").unwrap();

        let tool = ListDirTool::new(Some(ws));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("."));
        params.insert("recursive".into(), json!(true));
        params.insert("max_depth".into(), json!(3));

        let host_result = tool.execute(params.clone()).await;
        let sandbox_result = tool.execute_in_sandbox(&sandbox, params).await;
        assert_eq!(host_result, sandbox_result);
    }

    #[tokio::test]
    async fn test_delete_file_host_and_sandbox() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().to_path_buf();
        fs::write(ws.join("delete_me.txt"), "bye").unwrap();
        fs::write(ws.join("delete_me_sandbox.txt"), "bye").unwrap();

        let tool = DeleteFileTool::new(Some(ws.clone()));
        let sandbox = TestSandbox::new(ws.clone());

        let mut host_params = HashMap::new();
        host_params.insert("path".into(), json!("delete_me.txt"));
        let host_result = tool.execute(host_params).await;
        assert!(host_result.contains("Successfully deleted"));
        assert!(!ws.join("delete_me.txt").exists());

        let mut sandbox_params = HashMap::new();
        sandbox_params.insert("path".into(), json!("delete_me_sandbox.txt"));
        let sandbox_result = tool.execute_in_sandbox(&sandbox, sandbox_params).await;
        assert!(sandbox_result.contains("Successfully deleted"));
        assert!(!ws.join("delete_me_sandbox.txt").exists());
    }

    #[tokio::test]
    async fn test_apply_patch_multiple_hunks() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().to_path_buf();
        let file = ws.join("patch.txt");
        fs::write(&file, "alpha\nbeta\ngamma\n").unwrap();

        let tool = ApplyPatchTool::new(Some(ws));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("patch.txt"));
        params.insert(
            "hunks".into(),
            json!([
                { "old_text": "alpha", "new_text": "ALPHA" },
                { "old_text": "gamma", "new_text": "GAMMA" }
            ]),
        );
        let result = tool.execute(params).await;
        assert!(result.contains("Successfully applied 2 patch operations"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "ALPHA\nbeta\nGAMMA\n");
    }

    #[tokio::test]
    async fn test_apply_patch_operations_insert_and_delete() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().to_path_buf();
        let file = ws.join("patch_ops.txt");
        fs::write(&file, "alpha\nbeta\ngamma\n").unwrap();

        let tool = ApplyPatchTool::new(Some(ws));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("patch_ops.txt"));
        params.insert("operations".into(), json!([
            { "action": "insert_before", "old_text": "beta", "new_text": "// inserted before\n" },
            { "action": "insert_after", "old_text": "beta", "new_text": "\n// inserted after" },
            { "action": "delete", "old_text": "gamma\n" }
        ]));
        let result = tool.execute(params).await;
        assert!(result.contains("Successfully applied 3 patch operations"));
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "alpha\n// inserted before\nbeta\n// inserted after\n"
        );
    }

    #[tokio::test]
    async fn test_apply_patch_sandbox_operations_parity() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().to_path_buf();
        let sandbox = TestSandbox::new(ws.clone());
        let file = ws.join("patch_sandbox.txt");
        fs::write(&file, "alpha\nbeta\ngamma\n").unwrap();

        let tool = ApplyPatchTool::new(Some(ws));
        let mut params = HashMap::new();
        params.insert("path".into(), json!("patch_sandbox.txt"));
        params.insert(
            "operations".into(),
            json!([
                { "action": "replace", "old_text": "alpha", "new_text": "ALPHA" },
                { "action": "delete", "old_text": "gamma\n" }
            ]),
        );

        let host_result = tool.execute(params.clone()).await;
        fs::write(&file, "alpha\nbeta\ngamma\n").unwrap();
        let sandbox_result = tool.execute_in_sandbox(&sandbox, params).await;
        assert!(host_result.contains("Successfully applied 2 patch operations"));
        assert!(sandbox_result.contains("Successfully applied 2 patch operations"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "ALPHA\nbeta\n");
    }
}
