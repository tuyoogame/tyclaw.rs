//! 记忆存储 —— MEMORY.md（长期记忆）+ HISTORY.md（可搜索日志）。
//!
//! 双层记忆系统：
//! - MEMORY.md：长期事实记忆，由 LLM 合并更新
//! - HISTORY.md：追加式日志，适合 grep 搜索

use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// 双层记忆存储。
pub struct MemoryStore {
    memory_dir: PathBuf,
    memory_file: PathBuf,
    history_file: PathBuf,
}

impl MemoryStore {
    /// 创建 MemoryStore。`memory_dir` 是记忆存储目录（如 `workspaces/{key}/memory`）。
    pub fn new(memory_dir: &Path) -> Self {
        std::fs::create_dir_all(memory_dir).ok();
        Self {
            memory_file: memory_dir.join("MEMORY.md"),
            history_file: memory_dir.join("HISTORY.md"),
            memory_dir: memory_dir.to_path_buf(),
        }
    }

    /// 读取长期记忆。
    pub fn read_long_term(&self) -> String {
        if self.memory_file.exists() {
            std::fs::read_to_string(&self.memory_file).unwrap_or_default()
        } else {
            String::new()
        }
    }

    /// 写入长期记忆。
    pub fn write_long_term(&self, content: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.memory_dir)?;
        std::fs::write(&self.memory_file, content)
    }

    /// 追加历史日志。
    pub fn append_history(&self, entry: &str) -> std::io::Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(&self.memory_dir)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.history_file)?;
        writeln!(file, "{}\n", entry.trim_end())
    }

    /// 获取记忆上下文（用于注入系统提示）。
    ///
    /// 在 Long-term Memory 顶部加一段解读规则（护栏），防止 LLM 把"上次承诺但没做完"
    /// 误读成"当前正在做"从而继续输出承诺——典型失败模式：memory 里有
    /// "assistant acknowledged ... no results yet"，LLM 读到后这一轮又发"我来给你查"
    /// 然后 stop，没有任何 tool call，陷入自我强化循环。
    ///
    /// 护栏文本从 `prompts.yaml` 的 `memory_guard` 字段加载（需先 `init()` prompt_store）。
    pub fn get_memory_context(&self) -> String {
        let long_term = self.read_long_term();
        if long_term.is_empty() {
            String::new()
        } else {
            let guard = tyclaw_prompt::nudge_loader::memory_guard();
            format!("{guard}\n## Long-term Memory\n{long_term}")
        }
    }

    /// 将消息列表格式化为文本，用于 LLM 合并。
    fn format_messages(
        messages: &[std::collections::HashMap<String, serde_json::Value>],
    ) -> String {
        use serde_json::Value;
        let mut lines = Vec::new();
        for msg in messages {
            let content = match msg.get("content") {
                Some(Value::String(s)) if !s.is_empty() => s.as_str(),
                _ => continue,
            };
            let role = msg
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_uppercase();
            let ts = msg.get("timestamp").and_then(|v| v.as_str()).unwrap_or("?");
            let ts_short = &ts[..ts.len().min(16)];
            lines.push(format!("[{ts_short}] {role}: {content}"));
        }
        lines.join("\n")
    }
}

/// save_memory 工具定义（用于 LLM 合并调用）。
pub fn save_memory_tool_def() -> serde_json::Value {
    serde_json::json!([{
        "type": "function",
        "function": {
            "name": "save_memory",
            "description": "Save the memory consolidation result to persistent storage.",
            "parameters": {
                "type": "object",
                "properties": {
                    "history_entry": {
                        "type": "string",
                        "description": "A paragraph summarizing key events/decisions/topics. Start with [YYYY-MM-DD HH:MM]. Include detail useful for grep search."
                    },
                    "memory_update": {
                        "type": "string",
                        "description": "Full updated long-term memory as markdown. Include all existing facts plus new ones. Return unchanged if nothing new."
                    }
                },
                "required": ["history_entry", "memory_update"]
            }
        }
    }])
}

/// 通过 LLM 进行记忆合并。
///
/// 将消息列表交给 LLM，由 LLM 调用 save_memory 工具写入 MEMORY.md 和 HISTORY.md。
pub async fn consolidate_with_provider(
    store: &MemoryStore,
    messages: &[std::collections::HashMap<String, serde_json::Value>],
    provider: &dyn tyclaw_provider::LLMProvider,
    model: &str,
) -> bool {
    if messages.is_empty() {
        return true;
    }

    let current_memory = store.read_long_term();
    let formatted = MemoryStore::format_messages(messages);
    let prompt = format!(
        "Process this conversation and call the save_memory tool.\n\n\
         ## Current Long-term Memory\n{}\n\n\
         ## Conversation to Process\n{}",
        if current_memory.is_empty() {
            "(empty)".to_string()
        } else {
            current_memory.clone()
        },
        formatted,
    );

    let tools_def = save_memory_tool_def();
    let tools_vec = tools_def.as_array().cloned().unwrap_or_default();

    let mut sys_msg = std::collections::HashMap::new();
    sys_msg.insert("role".into(), serde_json::Value::String("system".into()));
    sys_msg.insert(
        "content".into(),
        serde_json::Value::String(
            tyclaw_prompt::prompt_store::get("memory_consolidation_prompt"),
        ),
    );

    let mut user_msg = std::collections::HashMap::new();
    user_msg.insert("role".into(), serde_json::Value::String("user".into()));
    user_msg.insert("content".into(), serde_json::Value::String(prompt));

    match provider
        .chat_with_retry(
            vec![sys_msg, user_msg],
            Some(tools_vec),
            Some(model.to_string()),
            None,
        )
        .await
    {
        response if response.has_tool_calls() => {
            let tc = &response.tool_calls[0];
            if tc.name != "save_memory" {
                warn!("Memory consolidation: unexpected tool call '{}'", tc.name);
                return false;
            }

            if let Some(entry) = tc.arguments.get("history_entry").and_then(|v| v.as_str()) {
                if let Err(e) = store.append_history(entry) {
                    warn!("Failed to append history: {}", e);
                }
            }

            if let Some(update) = tc.arguments.get("memory_update").and_then(|v| v.as_str()) {
                if update != current_memory {
                    if let Err(e) = store.write_long_term(update) {
                        warn!("Failed to write long-term memory: {}", e);
                    }
                }
            }

            info!("Memory consolidation done for {} messages", messages.len());
            true
        }
        _ => {
            warn!("Memory consolidation: LLM did not call save_memory");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_store_read_write() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = MemoryStore::new(tmp.path());

        assert!(store.read_long_term().is_empty());
        store.write_long_term("test memory").unwrap();
        assert_eq!(store.read_long_term(), "test memory");
    }

    #[test]
    fn test_memory_context() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = MemoryStore::new(tmp.path());

        assert!(store.get_memory_context().is_empty());
        store.write_long_term("facts here").unwrap();
        assert!(store.get_memory_context().contains("Long-term Memory"));
    }

    #[test]
    fn test_append_history() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = MemoryStore::new(tmp.path());

        store.append_history("entry 1").unwrap();
        store.append_history("entry 2").unwrap();

        let content = std::fs::read_to_string(&store.history_file).unwrap();
        assert!(content.contains("entry 1"));
        assert!(content.contains("entry 2"));
    }
}
