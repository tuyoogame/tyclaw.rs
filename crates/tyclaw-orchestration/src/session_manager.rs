//! 会话管理器 —— JSONL 格式的对话历史持久化。
//!
//! 每个 workspace 对应一个 history.jsonl 文件，第一行是元数据，后续行是消息记录。
//! Session 通过 workspace_key 标识，路径由 WorkspaceManager 提供。

use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
use parking_lot::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

/// 生成 session ID：`s_{YYYYMMDD}_{HHmmss}_{4位hex}`
fn generate_session_id() -> String {
    let now = Utc::now();
    let short_id = &Uuid::new_v4().to_string()[..4];
    format!("s_{}_{}", now.format("%Y%m%d_%H%M%S"), short_id)
}

/// 对话会话 —— 消息追加模式。
#[derive(Debug, Clone)]
pub struct Session {
    /// workspace key（标识归属的 workspace）
    pub workspace_key: String,
    /// 当前 session ID（每次唤醒生成新的）
    pub session_id: String,
    pub messages: Vec<HashMap<String, Value>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub metadata: HashMap<String, Value>,
    pub last_consolidated: usize,
}

impl Session {
    pub fn new(workspace_key: String) -> Self {
        let now = Utc::now();
        Self {
            workspace_key,
            session_id: generate_session_id(),
            messages: Vec::new(),
            created_at: now,
            updated_at: now,
            metadata: HashMap::new(),
            last_consolidated: 0,
        }
    }

    /// 添加一条消息到会话中。
    pub fn add_message(&mut self, role: &str, content: &str) {
        let mut msg = HashMap::new();
        msg.insert("role".into(), Value::String(role.into()));
        msg.insert("content".into(), Value::String(content.into()));
        msg.insert("timestamp".into(), Value::String(Utc::now().to_rfc3339()));
        self.messages.push(msg);
        self.updated_at = Utc::now();
    }

    /// 返回未合并的消息作为 LLM 输入。
    ///
    /// - `max_messages`: 最大返回消息数，0 表示返回所有未合并的消息
    /// - 严格保留切片后的原始顺序，不主动跳过开头非 user 消息。
    pub fn get_history(&self, max_messages: usize) -> Vec<HashMap<String, Value>> {
        let unconsolidated = &self.messages[self.last_consolidated..];
        let sliced = if max_messages > 0 && unconsolidated.len() > max_messages {
            &unconsolidated[unconsolidated.len() - max_messages..]
        } else {
            unconsolidated
        };
        sliced
            .iter()
            .map(|m| {
                let mut entry = HashMap::new();
                entry.insert(
                    "role".into(),
                    m.get("role").cloned().unwrap_or(Value::String("".into())),
                );
                entry.insert(
                    "content".into(),
                    m.get("content")
                        .cloned()
                        .unwrap_or(Value::String("".into())),
                );
                for k in &["tool_calls", "tool_call_id", "name"] {
                    if let Some(v) = m.get(*k) {
                        entry.insert(k.to_string(), v.clone());
                    }
                }
                entry
            })
            .collect()
    }

    /// 清除会话（`/new` 命令）。保留 session_id 不变。
    pub fn clear(&mut self) {
        self.messages.clear();
        self.last_consolidated = 0;
        self.updated_at = Utc::now();
    }
}

/// RAII guard：持有期间 workspace 标记为 busy，drop 时自动 clear。
pub struct BusyGuard<'a> {
    sessions: &'a SessionManager,
    workspace_key: String,
}

impl<'a> Drop for BusyGuard<'a> {
    fn drop(&mut self) {
        self.sessions.clear_busy(&self.workspace_key);
    }
}

/// busy 超时上限：超过此时间仍为 busy 则视为异常，强制允许回收。
const BUSY_TIMEOUT_SECS: u64 = 1800; // 30 分钟

/// 会话活跃状态追踪。
struct ActiveSession {
    session: Session,
    last_access: Instant,
    /// 正在处理请求的起始时间，None 表示空闲。
    busy_since: Option<Instant>,
}

/// 会话管理器 —— 管理多个 workspace 的会话生命周期和持久化。
///
/// 通过 workspace_key 标识每个会话，历史文件路径由外部提供。
pub struct SessionManager {
    /// workspace_key → 活跃会话
    cache: Mutex<HashMap<String, ActiveSession>>,
    /// 用于从 workspace_key 计算 history.jsonl 路径
    root: PathBuf,
}

impl SessionManager {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
            root: root.as_ref().to_path_buf(),
        }
    }

    /// 获取 workspace 的 history.jsonl 路径。
    fn history_path(&self, workspace_key: &str) -> PathBuf {
        tyclaw_control::workspace_path(&self.root, workspace_key).join("history.jsonl")
    }

    /// 获取或创建会话（返回克隆），同时更新 last_access。
    pub fn get_or_create_clone(&self, workspace_key: &str) -> Session {
        let mut cache = self.cache.lock();
        if let Some(active) = cache.get_mut(workspace_key) {
            active.last_access = Instant::now();
            return active.session.clone();
        }
        let session = self
            .load(workspace_key)
            .unwrap_or_else(|| Session::new(workspace_key.into()));
        let cloned = session.clone();
        cache.insert(
            workspace_key.to_string(),
            ActiveSession {
                session,
                last_access: Instant::now(),
                busy_since: None,
            },
        );
        cloned
    }

    /// 刷新 workspace 的 last_access 时间戳（防止活跃任务被误回收）。
    pub fn touch(&self, workspace_key: &str) {
        let mut cache = self.cache.lock();
        if let Some(active) = cache.get_mut(workspace_key) {
            active.last_access = Instant::now();
        }
    }

    /// 标记 workspace 为忙碌状态（handle_with_context 开始时调用）。
    pub fn set_busy(&self, workspace_key: &str) {
        let mut cache = self.cache.lock();
        if let Some(active) = cache.get_mut(workspace_key) {
            active.busy_since = Some(Instant::now());
        }
    }

    /// 查询 workspace 的忙碌状态。
    ///
    /// 返回 `Some(elapsed)` 表示忙碌中（elapsed 为已忙碌时长），`None` 表示空闲。
    pub fn busy_elapsed(&self, workspace_key: &str) -> Option<std::time::Duration> {
        let cache = self.cache.lock();
        cache
            .get(workspace_key)
            .and_then(|a| a.busy_since)
            .filter(|since| since.elapsed().as_secs() < BUSY_TIMEOUT_SECS)
            .map(|since| since.elapsed())
    }

    /// 清除 workspace 的忙碌状态（handle_with_context 结束时调用）。
    pub fn clear_busy(&self, workspace_key: &str) {
        let mut cache = self.cache.lock();
        if let Some(active) = cache.get_mut(workspace_key) {
            active.busy_since = None;
            active.last_access = Instant::now();
        }
    }

    /// 创建一个 RAII guard，在 drop 时自动 clear_busy 并刷新 last_access。
    pub fn busy_guard(&self, workspace_key: &str) -> BusyGuard<'_> {
        self.set_busy(workspace_key);
        BusyGuard {
            sessions: self,
            workspace_key: workspace_key.to_string(),
        }
    }

    /// 获取 session_id（如果有活跃会话）。
    pub fn get_session_id(&self, workspace_key: &str) -> Option<String> {
        let cache = self.cache.lock();
        cache.get(workspace_key).map(|a| a.session.session_id.clone())
    }

    /// 从 JSONL 文件加载会话。
    fn load(&self, workspace_key: &str) -> Option<Session> {
        let path = self.history_path(workspace_key);
        if !path.exists() {
            return None;
        }

        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                warn!("Failed to open session {}: {}", workspace_key, e);
                return None;
            }
        };

        let reader = std::io::BufReader::new(file);
        let mut messages = Vec::new();
        let mut metadata = HashMap::new();
        let mut created_at: Option<DateTime<Utc>> = None;
        let mut last_consolidated = 0usize;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<HashMap<String, Value>>(&line) {
                Ok(data) => {
                    if data.get("_type").and_then(|v| v.as_str()) == Some("metadata") {
                        if let Some(m) = data.get("metadata") {
                            if let Ok(map) =
                                serde_json::from_value::<HashMap<String, Value>>(m.clone())
                            {
                                metadata = map;
                            }
                        }
                        if let Some(ts) = data.get("created_at").and_then(|v| v.as_str()) {
                            created_at = DateTime::parse_from_rfc3339(ts)
                                .ok()
                                .map(|dt| dt.with_timezone(&Utc));
                        }
                        if let Some(lc) = data.get("last_consolidated").and_then(|v| v.as_u64()) {
                            last_consolidated = lc as usize;
                        }
                    } else {
                        messages.push(data);
                    }
                }
                Err(e) => {
                    warn!("Failed to parse session line: {}", e);
                }
            }
        }

        Some(Session {
            workspace_key: workspace_key.to_string(),
            session_id: generate_session_id(), // 每次加载生成新 session_id
            messages,
            created_at: created_at.unwrap_or_else(Utc::now),
            updated_at: Utc::now(),
            metadata,
            last_consolidated,
        })
    }

    /// 全量保存会话到 JSONL 文件（截断重写）。
    pub fn save(&self, session: &Session) -> std::io::Result<()> {
        let path = self.history_path(&session.workspace_key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&path)?;

        let meta = serde_json::json!({
            "_type": "metadata",
            "workspace_key": session.workspace_key,
            "session_id": session.session_id,
            "created_at": session.created_at.to_rfc3339(),
            "updated_at": session.updated_at.to_rfc3339(),
            "metadata": session.metadata,
            "last_consolidated": session.last_consolidated,
        });
        writeln!(file, "{}", serde_json::to_string(&meta)?)?;

        for msg in &session.messages {
            writeln!(file, "{}", serde_json::to_string(msg)?)?;
        }

        let mut cache = self.cache.lock();
        if let Some(active) = cache.get_mut(&session.workspace_key) {
            active.session = session.clone();
            active.last_access = Instant::now();
        } else {
            cache.insert(
                session.workspace_key.clone(),
                ActiveSession {
                    session: session.clone(),
                    last_access: Instant::now(),
                    busy_since: None,
                },
            );
        }
        Ok(())
    }

    /// 追加消息到 JSONL 文件（O_APPEND 模式，并发安全）。
    pub fn append_messages(
        &self,
        workspace_key: &str,
        messages: &[HashMap<String, serde_json::Value>],
    ) -> std::io::Result<()> {
        use std::fs::OpenOptions;

        if messages.is_empty() {
            return Ok(());
        }

        let path = self.history_path(workspace_key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file_exists = path.exists();
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;

        if !file_exists {
            let session_id = self
                .get_session_id(workspace_key)
                .unwrap_or_else(generate_session_id);
            let meta = serde_json::json!({
                "_type": "metadata",
                "workspace_key": workspace_key,
                "session_id": session_id,
                "created_at": chrono::Utc::now().to_rfc3339(),
                "updated_at": chrono::Utc::now().to_rfc3339(),
                "metadata": {},
                "last_consolidated": 0,
            });
            writeln!(file, "{}", serde_json::to_string(&meta)?)?;
        }

        for msg in messages {
            writeln!(file, "{}", serde_json::to_string(msg)?)?;
        }

        // 清缓存，下次 get_or_create_clone 重新从磁盘加载
        let mut cache = self.cache.lock();
        cache.remove(workspace_key);
        Ok(())
    }

    /// 从缓存中移除会话。
    pub fn invalidate(&self, workspace_key: &str) {
        let mut cache = self.cache.lock();
        cache.remove(workspace_key);
    }

    // ── 超时回收 ──

    /// 返回所有超过 `timeout_secs` 未访问且非忙碌状态的 workspace key。
    /// busy 超过 BUSY_TIMEOUT_SECS 视为异常，强制允许回收。
    pub fn find_idle_workspaces(&self, timeout_secs: u64) -> Vec<String> {
        let cache = self.cache.lock();
        let now = Instant::now();
        cache
            .iter()
            .filter(|(_, active)| {
                let is_busy = active.busy_since.map_or(false, |since| {
                    now.duration_since(since).as_secs() < BUSY_TIMEOUT_SECS
                });
                !is_busy
                    && now.duration_since(active.last_access).as_secs() >= timeout_secs
            })
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// 从活跃缓存中移除指定 workspace（回收前调用）。
    /// 返回被移除的 Session（用于 consolidation 等收尾操作）。
    ///
    /// 调用方应在 evict 后执行回收流程：
    /// 1. consolidate 对话历史 → memory
    /// 2. 清空 history.jsonl
    /// 3. 清空 work/tmp/、work/dispatches/、work/attachments/
    /// 4. 销毁 Docker 容器
    pub fn evict(&self, workspace_key: &str) -> Option<Session> {
        let mut cache = self.cache.lock();
        cache.remove(workspace_key).map(|a| {
            info!(
                workspace_key,
                session_id = %a.session.session_id,
                "Session evicted"
            );
            a.session
        })
    }

    /// 当前活跃 workspace 数量。
    pub fn active_count(&self) -> usize {
        self.cache.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_id_format() {
        let id = generate_session_id();
        assert!(id.starts_with("s_"));
        assert!(id.len() > 15);
    }

    #[test]
    fn test_session_add_and_history() {
        let mut session = Session::new("test_ws".into());
        assert!(session.session_id.starts_with("s_"));
        session.add_message("user", "hello");
        session.add_message("assistant", "hi");

        let history = session.get_history(0);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0]["role"], "user");
        assert_eq!(history[1]["role"], "assistant");
    }

    #[test]
    fn test_session_clear() {
        let mut session = Session::new("test_ws".into());
        let sid = session.session_id.clone();
        session.add_message("user", "hello");
        session.clear();
        assert!(session.messages.is_empty());
        assert_eq!(session.last_consolidated, 0);
        // session_id 不变
        assert_eq!(session.session_id, sid);
    }

    #[test]
    fn test_session_manager_persistence() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = SessionManager::new(tmp.path());

        let mut session = mgr.get_or_create_clone("alice");
        session.add_message("user", "hello");
        session.add_message("assistant", "hi");
        mgr.save(&session).unwrap();

        mgr.invalidate("alice");
        let reloaded = mgr.get_or_create_clone("alice");
        assert_eq!(reloaded.messages.len(), 2);
        assert_eq!(reloaded.workspace_key, "alice");
    }

    #[test]
    fn test_find_idle_workspaces() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = SessionManager::new(tmp.path());

        // 访问一次，触发缓存
        let _ = mgr.get_or_create_clone("active_ws");

        // 超时 0 秒 → 所有都算 idle
        let idle = mgr.find_idle_workspaces(0);
        assert!(idle.contains(&"active_ws".to_string()));

        // 超时 9999 秒 → 没有 idle
        let idle = mgr.find_idle_workspaces(9999);
        assert!(idle.is_empty());
    }

    #[test]
    fn test_evict() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = SessionManager::new(tmp.path());
        let _ = mgr.get_or_create_clone("ws1");
        assert_eq!(mgr.active_count(), 1);

        let evicted = mgr.evict("ws1");
        assert!(evicted.is_some());
        assert_eq!(evicted.unwrap().workspace_key, "ws1");
        assert_eq!(mgr.active_count(), 0);
    }
}
