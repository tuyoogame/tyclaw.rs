//! 全局提示词存储：统一从 config/prompts.yaml 加载，各模块按需取值。
//!
//! 调用 `init(workspace)` 后，所有提示词通过 `get(key)` / `get_node_type(type)` / `get_nudge(name)` 获取。
//! 不存在的 key 返回 None，调用方自行决定默认值。

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use tracing::debug;

static STORE: OnceLock<PromptStore> = OnceLock::new();
static PATH_VARS: OnceLock<HashMap<String, String>> = OnceLock::new();

/// 初始化路径变量（从 PathConfig 生成）。应在 init() 之后调用。
pub fn init_path_vars(vars: HashMap<String, String>) {
    let _ = PATH_VARS.set(vars);
}

/// 对文本执行 `${VAR}` 变量替换。
fn substitute_vars(text: &str) -> String {
    let Some(vars) = PATH_VARS.get() else {
        return text.to_string();
    };
    let mut result = text.to_string();
    for (key, value) in vars {
        result = result.replace(&format!("${{{}}}", key), value);
    }
    result
}

struct PromptStore {
    top: HashMap<String, String>,
    node_types: HashMap<String, String>,
    nudges: HashMap<String, String>,
}

/// 初始化：解析 config/prompts.yaml。应在 app 启动时调用一次，幂等。
///
/// 加载失败直接 panic，拒绝启动——提示词是核心配置，缺失意味着不可预期的行为。
pub fn init(workspace: &Path) {
    let yaml_path = workspace.join("config").join("prompts.yaml");
    let content = std::fs::read_to_string(&yaml_path)
        .unwrap_or_else(|e| panic!("FATAL: 无法加载 {}: {e}", yaml_path.display()));
    debug!(path = %yaml_path.display(), "Loaded prompts.yaml");
    let (top, node_types, nudges) = parse_yaml(&content);
    let _ = STORE.set(PromptStore {
        top,
        node_types,
        nudges,
    });
}

/// 获取顶层字段（identity, guidelines, guidelines_default, guidelines_coding 等）。
///
/// key 不存在直接 panic——缺失提示词说明 prompts.yaml 配置不完整。
pub fn get(key: &str) -> String {
    let store = STORE
        .get()
        .expect("FATAL: prompt_store 未初始化，请先调用 init()");
    let value = store
        .top
        .get(key)
        .filter(|v| !v.is_empty())
        .cloned()
        .unwrap_or_else(|| panic!("FATAL: prompts.yaml 缺少必需字段 '{key}'"));
    substitute_vars(&value)
}

/// 获取顶层字段，未初始化或未配置时返回 `None`——适合可选护栏类文本，
/// 不希望在不完整环境（如单元测试）下 panic。
pub fn try_get(key: &str) -> Option<String> {
    let store = STORE.get()?;
    let value = store.top.get(key).filter(|v| !v.is_empty()).cloned()?;
    Some(substitute_vars(&value))
}

/// 获取 node_types.{type} 的提示词。
pub fn get_node_type(node_type: &str) -> String {
    let store = STORE
        .get()
        .expect("FATAL: prompt_store 未初始化，请先调用 init()");
    let value = store
        .node_types
        .get(node_type)
        .filter(|v| !v.is_empty())
        .cloned()
        .unwrap_or_else(|| panic!("FATAL: prompts.yaml 缺少 node_types.'{node_type}'"));
    substitute_vars(&value)
}

/// 获取 nudges.{name} 的催促文本。
pub fn get_nudge(name: &str) -> String {
    let store = STORE
        .get()
        .expect("FATAL: prompt_store 未初始化，请先调用 init()");
    let value = store
        .nudges
        .get(name)
        .filter(|v| !v.is_empty())
        .cloned()
        .unwrap_or_else(|| panic!("FATAL: prompts.yaml 缺少 nudges.'{name}'"));
    substitute_vars(&value)
}

// ─── YAML Parser ────────────────────────────────────────────

fn parse_yaml(
    content: &str,
) -> (
    HashMap<String, String>,
    HashMap<String, String>,
    HashMap<String, String>,
) {
    let mut top = HashMap::new();
    let mut node_types = HashMap::new();
    let mut nudges = HashMap::new();

    let mut current_key: Option<String> = None;
    let mut current_section: Option<String> = None;
    let mut block_indent: Option<usize> = None;
    let mut block_lines: Vec<String> = Vec::new();

    let flush = |key: &Option<String>,
                 section: &Option<String>,
                 lines: &[String],
                 top: &mut HashMap<String, String>,
                 nt: &mut HashMap<String, String>,
                 nu: &mut HashMap<String, String>| {
        if let Some(ref k) = key {
            let value = lines.join("\n").trim_end().to_string();
            if value.is_empty() {
                return;
            }
            match section.as_deref() {
                Some("node_types") => {
                    nt.insert(k.clone(), value);
                }
                Some("nudges") => {
                    nu.insert(k.clone(), value);
                }
                _ => {
                    top.insert(k.clone(), value);
                }
            }
        }
    };

    for line in content.lines() {
        let trimmed = line.trim();

        if block_indent.is_none() && (trimmed.starts_with('#') || trimmed.is_empty()) {
            continue;
        }

        if let Some(indent) = block_indent {
            let line_indent = line.len() - line.trim_start().len();
            if line_indent >= indent || trimmed.is_empty() {
                if trimmed.is_empty() {
                    block_lines.push(String::new());
                } else {
                    block_lines.push(line[indent..].to_string());
                }
                continue;
            } else {
                flush(
                    &current_key,
                    &current_section,
                    &block_lines,
                    &mut top,
                    &mut node_types,
                    &mut nudges,
                );
                current_key = None;
                block_indent = None;
                block_lines.clear();
            }
        }

        if trimmed == "node_types:" {
            current_section = Some("node_types".into());
            continue;
        }
        if trimmed == "nudges:" {
            current_section = Some("nudges".into());
            continue;
        }

        if let Some(colon_pos) = trimmed.find(": |") {
            let key = trimmed[..colon_pos].trim().to_string();
            let line_indent = line.len() - line.trim_start().len();
            if line_indent == 0 {
                current_section = None;
            }
            current_key = Some(key);
            block_indent = Some(line_indent + 2);
            block_lines.clear();
            continue;
        }

        if trimmed.contains(": ") {
            let line_indent = line.len() - line.trim_start().len();
            if line_indent == 0 && !trimmed.ends_with(':') {
                current_section = None;
            }
        }
    }

    flush(
        &current_key,
        &current_section,
        &block_lines,
        &mut top,
        &mut node_types,
        &mut nudges,
    );

    (top, node_types, nudges)
}
