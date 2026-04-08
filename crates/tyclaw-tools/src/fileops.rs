//! 轻量文件操作和搜索工具。
//!
//! 供多模型模式下的主控 LLM 使用：能做简单文件操作和探索，
//! 但不能执行任意代码（exec）或写代码文件（write_file/edit_file）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;
use serde_json::{json, Value};

use tyclaw_tool_abi::{Sandbox, SandboxGlobEntry, SandboxGrepRequest, SandboxGrepResponse};

use crate::base::{RiskLevel, Tool};

// ── 路径解析 ──────────────────────────────────────────────

fn resolve_path(path: &str, workspace: Option<&Path>) -> PathBuf {
    let p = PathBuf::from(shellexpand::tilde(path).as_ref());
    if p.is_absolute() {
        p
    } else if let Some(ws) = workspace {
        ws.join(p)
    } else {
        p
    }
}

/// Canonicalize a path and verify it is within the workspace.
/// For existing paths, uses std::fs::canonicalize.
/// For non-existent paths, canonicalizes the nearest existing ancestor
/// and appends the remaining components.
fn safe_resolve(path: &str, workspace: Option<&Path>) -> Result<PathBuf, String> {
    let resolved = resolve_path(path, workspace);

    // Canonicalize the path (resolve symlinks and ..)
    let canonical = canonicalize_best_effort(&resolved)?;

    // Check workspace bounds
    if let Some(ws) = workspace {
        let canonical_ws = ws.canonicalize().unwrap_or_else(|_| ws.to_path_buf());
        if !canonical.starts_with(&canonical_ws) {
            return Err(format!(
                "Error: Path must be within workspace: {}",
                canonical_ws.display()
            ));
        }
    }

    Ok(canonical)
}

/// Best-effort canonicalize: if the full path exists, canonicalize it.
/// Otherwise, canonicalize the deepest existing ancestor and append the rest.
fn canonicalize_best_effort(path: &PathBuf) -> Result<PathBuf, String> {
    if path.exists() {
        path.canonicalize()
            .map_err(|e| format!("Error: Failed to resolve path: {e}"))
    } else {
        // Walk up to find an existing ancestor
        let mut existing = path.clone();
        let mut tail_components = Vec::new();
        while !existing.exists() {
            if let Some(file_name) = existing.file_name() {
                tail_components.push(file_name.to_os_string());
            } else {
                // Reached root or empty - just return the original
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

// ── CopyFileTool ─────────────────────────────────────────

pub struct CopyFileTool {
    workspace: Option<PathBuf>,
}

impl CopyFileTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for CopyFileTool {
    fn name(&self) -> &str {
        "copy_file"
    }

    fn description(&self) -> &str {
        "Copy a file from source to destination. Use this instead of read_file + write_file when you want to preserve the same contents at a new path."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "src": { "type": "string", "description": "Source file path" },
                "dst": { "type": "string", "description": "Destination file path" }
            },
            "required": ["src", "dst"]
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
        let src = match params.get("src").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: Missing 'src' parameter".into(),
        };
        let dst = match params.get("dst").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: Missing 'dst' parameter".into(),
        };

        let bytes = match sandbox.read_file(src).await {
            Ok(bytes) => bytes,
            Err(e) => return format!("Error reading source: {e}"),
        };
        match sandbox.write_file(dst, &bytes).await {
            Ok(()) => format!("Copied {} -> {} ({} bytes)", src, dst, bytes.len()),
            Err(e) => format!("Error writing destination: {e}"),
        }
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let src = match params.get("src").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return "Error: Missing 'src' parameter".into(),
        };
        let dst = match params.get("dst").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return "Error: Missing 'dst' parameter".into(),
        };

        let ws = self.workspace.clone();
        tokio::task::spawn_blocking(move || {
            let src_path = match safe_resolve(&src, ws.as_deref()) {
                Ok(p) => p,
                Err(e) => return e,
            };
            let dst_path = match safe_resolve(&dst, ws.as_deref()) {
                Ok(p) => p,
                Err(e) => return e,
            };

            if !src_path.exists() {
                return format!("Error: Source file not found: {src}");
            }

            // 确保目标目录存在
            if let Some(parent) = dst_path.parent() {
                if !parent.exists() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return format!("Error: Failed to create directory: {e}");
                    }
                }
            }

            match std::fs::copy(&src_path, &dst_path) {
                Ok(bytes) => format!("Copied {} -> {} ({} bytes)", src, dst, bytes),
                Err(e) => format!("Error: {e}"),
            }
        })
        .await
        .unwrap_or_else(|e| format!("Error: Task failed: {e}"))
    }
}

// ── MoveFileTool ─────────────────────────────────────────

pub struct MoveFileTool {
    workspace: Option<PathBuf>,
}

impl MoveFileTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for MoveFileTool {
    fn name(&self) -> &str {
        "move_file"
    }

    fn description(&self) -> &str {
        "Move or rename a file. Use this instead of copy_file + delete_file when the source should no longer remain at the old path."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "src": { "type": "string", "description": "Source file path" },
                "dst": { "type": "string", "description": "Destination file path" }
            },
            "required": ["src", "dst"]
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
        let src = match params.get("src").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: Missing 'src' parameter".into(),
        };
        let dst = match params.get("dst").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: Missing 'dst' parameter".into(),
        };

        let stat = match sandbox.stat(src).await {
            Ok(stat) => stat,
            Err(e) => return format!("Error: {e}"),
        };
        if !stat.exists {
            return format!("Error: Source not found: {src}");
        }
        if stat.is_dir {
            return format!(
                "Error: move_file currently supports files only in sandbox mode: {src}"
            );
        }

        let bytes = match sandbox.read_file(src).await {
            Ok(bytes) => bytes,
            Err(e) => return format!("Error reading source: {e}"),
        };
        if let Err(e) = sandbox.write_file(dst, &bytes).await {
            return format!("Error writing destination: {e}");
        }
        match sandbox.remove_file(src).await {
            Ok(()) => format!("Moved {} -> {}", src, dst),
            Err(e) => format!("Error removing source after move: {e}"),
        }
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let src = match params.get("src").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return "Error: Missing 'src' parameter".into(),
        };
        let dst = match params.get("dst").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return "Error: Missing 'dst' parameter".into(),
        };

        let ws = self.workspace.clone();
        tokio::task::spawn_blocking(move || {
            let src_path = match safe_resolve(&src, ws.as_deref()) {
                Ok(p) => p,
                Err(e) => return e,
            };
            let dst_path = match safe_resolve(&dst, ws.as_deref()) {
                Ok(p) => p,
                Err(e) => return e,
            };

            if !src_path.exists() {
                return format!("Error: Source not found: {src}");
            }

            if let Some(parent) = dst_path.parent() {
                if !parent.exists() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return format!("Error: Failed to create directory: {e}");
                    }
                }
            }

            match std::fs::rename(&src_path, &dst_path) {
                Ok(()) => format!("Moved {} -> {}", src, dst),
                Err(e) => format!("Error: {e}"),
            }
        })
        .await
        .unwrap_or_else(|e| format!("Error: Task failed: {e}"))
    }
}

// ── MkdirTool ────────────────────────────────────────────

pub struct MkdirTool {
    workspace: Option<PathBuf>,
}

impl MkdirTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for MkdirTool {
    fn name(&self) -> &str {
        "mkdir"
    }

    fn description(&self) -> &str {
        "Create a directory (including parent directories). Use this only when a directory must exist before a later step; write_file already creates parent directories automatically."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path to create" }
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
        match sandbox.create_dir(path).await {
            Ok(()) => format!("Created directory: {path}"),
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
            let dir_path = match safe_resolve(&path, ws.as_deref()) {
                Ok(p) => p,
                Err(e) => return e,
            };

            if dir_path.exists() {
                return format!("Directory already exists: {path}");
            }

            match std::fs::create_dir_all(&dir_path) {
                Ok(()) => format!("Created directory: {path}"),
                Err(e) => format!("Error: {e}"),
            }
        })
        .await
        .unwrap_or_else(|e| format!("Error: Task failed: {e}"))
    }
}

// ── GrepSearchTool ───────────────────────────────────────

const GREP_DEFAULT_MAX_RESULTS: usize = 200;

fn build_grep_request(params: &HashMap<String, Value>) -> Result<SandboxGrepRequest, String> {
    let pattern = params
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Error: Missing 'pattern' parameter".to_string())?
        .to_string();

    Ok(SandboxGrepRequest {
        pattern,
        path: params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string(),
        include: params
            .get("include")
            .and_then(|v| v.as_str())
            .map(String::from),
        file_type: params
            .get("file_type")
            .and_then(|v| v.as_str())
            .map(String::from),
        context_lines: params
            .get("context_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize),
        case_insensitive: params
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        output_mode: params
            .get("output_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("content")
            .to_string(),
        max_results: params
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(GREP_DEFAULT_MAX_RESULTS),
    })
}

fn format_grep_response(
    response: SandboxGrepResponse,
    output_mode: &str,
    max_results: usize,
) -> String {
    if !response.stderr.trim().is_empty() && response.stdout.trim().is_empty() {
        if response.stderr.contains("unrecognized file type") {
            return format!(
                "Error: Unknown file_type. Use --type-list with rg to see available types.\n{}",
                response.stderr.trim()
            );
        }
        return format!("Error: {}", response.stderr.trim());
    }

    if response.stdout.trim().is_empty() {
        return "No matches found.".into();
    }

    let mut lines: Vec<&str> = response.stdout.lines().collect();
    lines.sort_unstable();
    let total = lines.len();
    let truncated = total >= max_results;
    let sorted_stdout = lines.join("\n");

    let mut result = match output_mode {
        "files_only" => format!("{total} files match:\n{}", sorted_stdout.trim()),
        "count" => format!("Match counts:\n{}", sorted_stdout.trim()),
        _ => {
            let match_count = lines
                .iter()
                .filter(|l| !l.starts_with('-') && !l.is_empty())
                .count();
            format!("{match_count} matches:\n{}", sorted_stdout.trim())
        }
    };

    if truncated {
        result.push_str(&format!("\n... (truncated at {max_results} results)"));
    }
    result
}

fn validate_glob_pattern(pattern: &str) -> Option<String> {
    if pattern.contains('$')
        || pattern.contains('`')
        || pattern.contains(';')
        || pattern.contains('|')
        || pattern.contains('&')
        || pattern.contains('(')
        || pattern.contains(')')
        || pattern.contains('<')
        || pattern.contains('>')
        || pattern.contains('\n')
    {
        Some("Error: Pattern contains disallowed characters".into())
    } else {
        None
    }
}

fn format_glob_entries(mut entries: Vec<SandboxGlobEntry>) -> String {
    entries.retain(|entry| {
        !entry
            .path
            .split('/')
            .any(|component| GLOB_SKIP_DIRS.contains(&component))
    });

    if entries.is_empty() {
        return "No files found.".into();
    }

    entries.sort_by(|a, b| b.modified_unix_secs.cmp(&a.modified_unix_secs));

    let total = entries.len();
    let display: Vec<String> = entries
        .iter()
        .take(GLOB_MAX_RESULTS)
        .map(|entry| entry.path.clone())
        .collect();
    let result = display.join("\n");

    if total > GLOB_MAX_RESULTS {
        format!("{result}\n... ({total} total, showing first {GLOB_MAX_RESULTS})")
    } else {
        format!("{total} files found:\n{result}")
    }
}

pub struct GrepSearchTool {
    workspace: Option<PathBuf>,
}

impl GrepSearchTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for GrepSearchTool {
    fn name(&self) -> &str {
        "grep_search"
    }

    fn description(&self) -> &str {
        "Search file contents using ripgrep. Returns matching lines with file paths and line numbers. \
         Use context_lines to see surrounding code without a separate read_file call. \
         Use output_mode='files_only' to find which files contain a pattern. \
         Use file_type (e.g. 'rust', 'python', 'js') to filter by language. \
         Prefer this over exec grep; after locating the right file or match, use read_file for deeper inspection."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Search pattern (regex supported)"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search in (default: workspace root)"
                },
                "include": {
                    "type": "string",
                    "description": "File glob pattern to filter (e.g. '*.rs', '*.{ts,tsx}')"
                },
                "file_type": {
                    "type": "string",
                    "description": "Filter by file type: rust, python, js, ts, go, java, cpp, etc."
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Number of lines to show before and after each match (default: 0)"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Case insensitive search (default: false)"
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_only", "count"],
                    "description": "content: matching lines (default). files_only: file paths only. count: match counts per file."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of matching lines to return (default: 200)"
                }
            },
            "required": ["pattern"]
        })
    }

    /// grep 输出压缩：截断超长行，限制每文件匹配数
    fn compress_output(&self, output: &str, _params: &HashMap<String, Value>) -> String {
        const MAX_LINE_CHARS: usize = 300;
        const MAX_MATCHES_PER_FILE: usize = 15;

        let lines: Vec<&str> = output.lines().collect();
        if lines.len() < 30 {
            return output.to_string();
        }

        let mut result: Vec<String> = Vec::new();
        let mut current_file = String::new();
        let mut file_match_count = 0u32;
        let mut file_overflow = 0u32;

        for line in &lines {
            if let Some(colon_pos) = line.find(':') {
                let file_part = &line[..colon_pos];
                if file_part.contains('/') || file_part.contains('.') {
                    if file_part != current_file {
                        if file_overflow > 0 {
                            result.push(format!("  ... +{file_overflow} more matches"));
                        }
                        current_file = file_part.to_string();
                        file_match_count = 0;
                        file_overflow = 0;
                    }
                    file_match_count += 1;
                    if file_match_count > MAX_MATCHES_PER_FILE as u32 {
                        file_overflow += 1;
                        continue;
                    }
                }
            }
            if line.len() > MAX_LINE_CHARS {
                let boundary = line.char_indices().nth(MAX_LINE_CHARS).map(|(i, _)| i).unwrap_or(line.len());
                result.push(format!("{}...", &line[..boundary]));
            } else {
                result.push(line.to_string());
            }
        }
        if file_overflow > 0 {
            result.push(format!("  ... +{file_overflow} more matches"));
        }
        result.join("\n")
    }

    fn should_sandbox(&self) -> bool {
        true
    }

    async fn execute_in_sandbox(
        &self,
        sandbox: &dyn Sandbox,
        params: HashMap<String, Value>,
    ) -> String {
        let request = match build_grep_request(&params) {
            Ok(request) => request,
            Err(err) => return err,
        };
        let max_results = request.max_results;
        let output_mode = request.output_mode.clone();
        match sandbox.grep_search(request).await {
            Ok(response) => format_grep_response(response, &output_mode, max_results),
            Err(e) => format!("Error: {e}"),
        }
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let request = match build_grep_request(&params) {
            Ok(request) => request,
            Err(err) => return err,
        };

        let ws = self.workspace.clone();
        tokio::task::spawn_blocking(move || {
            let search_path = if request.path != "." {
                match safe_resolve(&request.path, ws.as_deref()) {
                    Ok(resolved) => resolved,
                    Err(e) => return e,
                }
            } else {
                ws.clone().unwrap_or_else(|| PathBuf::from("."))
            };
            let search_arg = if let Some(ref ws_path) = ws {
                if request.path == "." {
                    ".".to_string()
                } else {
                    search_path
                        .strip_prefix(ws_path)
                        .ok()
                        .map(|p| {
                            let s = p.to_string_lossy().replace('\\', "/");
                            if s.is_empty() {
                                ".".into()
                            } else {
                                s
                            }
                        })
                        .unwrap_or_else(|| search_path.to_string_lossy().to_string())
                }
            } else {
                search_path.to_string_lossy().to_string()
            };

            let rg_available = Command::new("rg").arg("--version").output().is_ok();
            if !rg_available {
                return Self::fallback_grep(
                    &request.pattern,
                    &search_path,
                    request.include.as_deref(),
                    request.max_results,
                );
            }

            let mut cmd = Command::new("rg");
            if let Some(ref ws_path) = ws {
                cmd.current_dir(ws_path);
            }
            cmd.args(["--no-heading", "--line-number", "--color", "never"]);

            match request.output_mode.as_str() {
                "files_only" => {
                    cmd.arg("-l");
                }
                "count" => {
                    cmd.arg("-c");
                }
                _ => {}
            }

            if request.case_insensitive {
                cmd.arg("-i");
            }

            if let Some(c) = request.context_lines {
                if c > 0 && request.output_mode == "content" {
                    cmd.args(["-C", &c.to_string()]);
                }
            }

            if let Some(ref t) = request.file_type {
                cmd.args(["--type", t]);
            }

            if let Some(ref inc) = request.include {
                cmd.args(["--glob", inc]);
            }

            cmd.args(["--max-count", &request.max_results.to_string()]);

            cmd.arg("--").arg(&request.pattern).arg(&search_arg);

            match cmd.output() {
                Ok(output) => format_grep_response(
                    SandboxGrepResponse {
                        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                        exit_code: output.status.code().unwrap_or(-1),
                    },
                    &request.output_mode,
                    request.max_results,
                ),
                Err(e) => format!("Error running rg: {e}"),
            }
        })
        .await
        .unwrap_or_else(|e| format!("Error: Task failed: {e}"))
    }
}

impl GrepSearchTool {
    /// 当 rg 不可用时回退到系统 grep。
    fn fallback_grep(
        pattern: &str,
        search_path: &Path,
        include: Option<&str>,
        max_results: usize,
    ) -> String {
        let mut cmd = Command::new("grep");
        cmd.args(["-rn", "--color=never"]);
        cmd.args(["-m", &max_results.to_string()]);

        if let Some(inc) = include {
            cmd.arg(format!("--include={inc}"));
        }

        cmd.arg(pattern).arg(search_path);

        match cmd.output() {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.is_empty() {
                    "No matches found.".into()
                } else {
                    let lines: Vec<&str> = stdout.lines().collect();
                    if lines.len() >= max_results {
                        format!("{}\n... (truncated at {max_results} lines, rg not available — install ripgrep for full features)", stdout.trim())
                    } else {
                        format!("{} matches:\n{}", lines.len(), stdout.trim())
                    }
                }
            }
            Err(e) => format!("Error running grep: {e}"),
        }
    }
}

// ── GlobTool ─────────────────────────────────────────────

/// 最大返回文件数。
const GLOB_MAX_RESULTS: usize = 200;

/// 需要跳过的目录名（噪音目录）。
const GLOB_SKIP_DIRS: &[&str] = &["target", ".git", "node_modules", "__pycache__", ".venv"];

pub struct GlobTool {
    workspace: Option<PathBuf>,
}

impl GlobTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Fast file pattern matching tool. Supports glob patterns like '**/*.rs', 'src/**/*.py', '*.tsx'. \
         Returns matching file paths sorted by modification time (most recently modified first). \
         Use this to find files by name or path patterns. Prefer this over list_dir when you already know the filename shape."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match files (e.g. '**/*.rs', 'src/**/*.py', '*.json', 'crates/*/Cargo.toml')"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: workspace root). The pattern is relative to this path."
                }
            },
            "required": ["pattern"]
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
        let pattern = match params.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: Missing 'pattern' parameter".into(),
        };
        let base = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        if let Some(error) = validate_glob_pattern(pattern) {
            return error;
        }
        match sandbox.glob_search(pattern, base).await {
            Ok(entries) => format_glob_entries(entries),
            Err(e) => format!("Error: {e}"),
        }
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let pattern = match params.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return "Error: Missing 'pattern' parameter".into(),
        };

        let path_param = params
            .get("path")
            .and_then(|v| v.as_str())
            .map(String::from);

        let ws = self.workspace.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(error) = validate_glob_pattern(&pattern) {
                return error;
            }
            let base = if let Some(ref p) = path_param {
                match safe_resolve(p, ws.as_deref()) {
                    Ok(resolved) => resolved,
                    Err(e) => return e,
                }
            } else {
                ws.clone().unwrap_or_else(|| PathBuf::from("."))
            };

            // 构造完整 glob 路径
            let full_pattern = base.join(&pattern).to_string_lossy().to_string();

            let entries = match glob::glob(&full_pattern) {
                Ok(paths) => paths,
                Err(e) => return format!("Error: Invalid glob pattern: {e}"),
            };

            // 收集匹配结果，过滤噪音目录
            let mut files: Vec<SandboxGlobEntry> = Vec::new();
            for entry in entries {
                let path = match entry {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                // 跳过目录和噪音路径
                if !path.is_file() {
                    continue;
                }
                let dominated_by_skip = path.components().any(|c| {
                    if let std::path::Component::Normal(name) = c {
                        GLOB_SKIP_DIRS
                            .iter()
                            .any(|d| name == std::ffi::OsStr::new(d))
                    } else {
                        false
                    }
                });
                if dominated_by_skip {
                    continue;
                }

                // Workspace bounds check for each matched file
                if let Some(ref ws_path) = ws {
                    let canonical_ws = ws_path.canonicalize().unwrap_or_else(|_| ws_path.clone());
                    if let Ok(canonical_file) = path.canonicalize() {
                        if !canonical_file.starts_with(&canonical_ws) {
                            continue;
                        }
                    }
                }

                let mtime = path
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let rel = path
                    .strip_prefix(&base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                files.push(SandboxGlobEntry {
                    path: rel,
                    modified_unix_secs: mtime,
                });
            }

            format_glob_entries(files)
        })
        .await
        .unwrap_or_else(|e| format!("Error: Task failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;
    use std::fs;
    use std::time::Duration;
    use tempfile::TempDir;
    use tyclaw_tool_abi::{
        SandboxDirEntry, SandboxExecResult, SandboxFileStat, SandboxGlobEntry, SandboxGrepRequest,
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
            unimplemented!("exec is not needed for fileops tests")
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
            _path: &str,
        ) -> Result<Vec<SandboxDirEntry>, tyclaw_types::TyclawError> {
            Ok(Vec::new())
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
            request: SandboxGrepRequest,
        ) -> Result<SandboxGrepResponse, tyclaw_types::TyclawError> {
            let mut cmd = Command::new("rg");
            cmd.current_dir(&self.root);
            cmd.args(["--no-heading", "--line-number", "--color", "never"]);

            match request.output_mode.as_str() {
                "files_only" => {
                    cmd.arg("-l");
                }
                "count" => {
                    cmd.arg("-c");
                }
                _ => {}
            }
            if request.case_insensitive {
                cmd.arg("-i");
            }
            if let Some(c) = request.context_lines {
                if c > 0 && request.output_mode == "content" {
                    cmd.args(["-C", &c.to_string()]);
                }
            }
            if let Some(ref t) = request.file_type {
                cmd.args(["--type", t]);
            }
            if let Some(ref inc) = request.include {
                cmd.args(["--glob", inc]);
            }
            cmd.args(["--max-count", &request.max_results.to_string()]);
            cmd.arg("--").arg(&request.pattern).arg(&request.path);

            let output = cmd.output().map_err(|e| tyclaw_types::TyclawError::Tool {
                tool: "test_grep".into(),
                message: e.to_string(),
            })?;

            Ok(SandboxGrepResponse {
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                exit_code: output.status.code().unwrap_or(-1),
            })
        }

        async fn glob_search(
            &self,
            pattern: &str,
            path: &str,
        ) -> Result<Vec<SandboxGlobEntry>, tyclaw_types::TyclawError> {
            let base = self.root.join(path);
            let mut entries = Vec::new();
            let full_pattern = base.join(pattern).to_string_lossy().to_string();
            let matches =
                glob::glob(&full_pattern).map_err(|e| tyclaw_types::TyclawError::Tool {
                    tool: "test_glob".into(),
                    message: e.to_string(),
                })?;

            for entry in matches {
                let full = match entry {
                    Ok(path) if path.is_file() => path,
                    Ok(_) => continue,
                    Err(_) => continue,
                };
                let modified_unix_secs = std::fs::metadata(&full)
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let rel = full
                    .strip_prefix(&base)
                    .unwrap_or(&full)
                    .to_string_lossy()
                    .replace('\\', "/");
                entries.push(SandboxGlobEntry {
                    path: rel,
                    modified_unix_secs,
                });
            }
            Ok(entries)
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
            unimplemented!("copy_from is not needed for fileops tests")
        }

        fn workspace_root(&self) -> &str {
            self.root.to_str().unwrap_or(".")
        }

        fn id(&self) -> &str {
            &self.id
        }
    }

    #[tokio::test]
    async fn test_copy_move_mkdir_in_sandbox() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().to_path_buf();
        let sandbox = TestSandbox::new(ws.clone());
        fs::write(ws.join("src.txt"), "hello").unwrap();

        let mkdir = MkdirTool::new(Some(ws.clone()));
        let mut mkdir_params = HashMap::new();
        mkdir_params.insert("path".into(), json!("nested/out"));
        let mkdir_result = mkdir.execute_in_sandbox(&sandbox, mkdir_params).await;
        assert!(mkdir_result.contains("Created directory"));
        assert!(ws.join("nested/out").is_dir());

        let copy = CopyFileTool::new(Some(ws.clone()));
        let mut copy_params = HashMap::new();
        copy_params.insert("src".into(), json!("src.txt"));
        copy_params.insert("dst".into(), json!("nested/out/copied.txt"));
        let copy_result = copy.execute_in_sandbox(&sandbox, copy_params).await;
        assert!(copy_result.contains("Copied"));
        assert_eq!(
            fs::read_to_string(ws.join("nested/out/copied.txt")).unwrap(),
            "hello"
        );

        let mov = MoveFileTool::new(Some(ws.clone()));
        let mut move_params = HashMap::new();
        move_params.insert("src".into(), json!("nested/out/copied.txt"));
        move_params.insert("dst".into(), json!("moved.txt"));
        let move_result = mov.execute_in_sandbox(&sandbox, move_params).await;
        assert!(move_result.contains("Moved"));
        assert_eq!(fs::read_to_string(ws.join("moved.txt")).unwrap(), "hello");
        assert!(!ws.join("nested/out/copied.txt").exists());
    }

    #[tokio::test]
    async fn test_grep_search_sandbox_parity() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().to_path_buf();
        let sandbox = TestSandbox::new(ws.clone());
        fs::write(ws.join("a.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
        fs::write(ws.join("b.rs"), "fn beta() {}\n").unwrap();

        let tool = GrepSearchTool::new(Some(ws));
        let mut params = HashMap::new();
        params.insert("pattern".into(), json!("beta"));
        params.insert("file_type".into(), json!("rust"));
        params.insert("output_mode".into(), json!("content"));

        let host = tool.execute(params.clone()).await;
        let sandbox_out = tool.execute_in_sandbox(&sandbox, params).await;
        assert_eq!(host, sandbox_out);
    }

    #[tokio::test]
    async fn test_glob_search_sandbox_parity() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().to_path_buf();
        let sandbox = TestSandbox::new(ws.clone());
        fs::create_dir_all(ws.join("src/nested")).unwrap();
        fs::write(ws.join("src/a.rs"), "").unwrap();
        fs::write(ws.join("src/nested/b.rs"), "").unwrap();

        let tool = GlobTool::new(Some(ws));
        let mut params = HashMap::new();
        params.insert("pattern".into(), json!("src/**/*.rs"));

        let host = tool.execute(params.clone()).await;
        let sandbox_out = tool.execute_in_sandbox(&sandbox, params).await;
        assert_eq!(host, sandbox_out);
    }
}
