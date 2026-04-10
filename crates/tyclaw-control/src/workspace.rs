//! Workspace 管理 —— 统一的持久化空间和目录结构。
//!
//! 每个 workspace 通过可配置的 key 策略标识（user_id / conversation / 自定义），
//! 物理目录按 `works/{sha256(key)[0]:02x}/{key}/` 分桶存储。
//!
//! 目录结构：
//! ```text
//! {root}/
//! ├── config/          ← 全局配置
//! ├── skills/          ← 全局共享技能
//! ├── cases/           ← 全局共享案例
//! ├── audit/           ← 全局审计日志（按天）
//! ├── logs/            ← 全局日志
//! └── works/           ← workspace 容器（分桶）
//!     └── {bucket}/{workspace_key}/
//!         ├── memory/
//!         ├── skills/
//!         ├── cases/
//!         ├── timer_jobs.json
//!         ├── history.jsonl
//!         └── work/
//! ```

use serde::{Deserialize, Serialize};
use md5::{Digest, Md5};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use parking_lot::Mutex;
use tracing::debug;

// ─── 路径配置 ────────────────────────────────────────────────────────────────

/// 路径配置 —— 所有路径通过 config.yaml 配置，支持缺省值。
///
/// 分两类：
/// 1. 容器路径（Docker 相关）：container_root, container_workdir, global_skills_mount
/// 2. Workspace 子目录名（相对 workspace_dir）：skills_dir, memory_dir 等
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathConfig {
    // === Docker 容器配置 ===

    /// 容器挂载根路径（workspace_dir 整体挂载到此路径）
    #[serde(default = "d_container_root")]
    pub container_root: String,

    /// 容器工作目录（docker exec -w）
    #[serde(default = "d_container_workdir")]
    pub container_workdir: String,

    /// 全局 skills 在容器内的挂载目录名（相对 container_root）
    #[serde(default = "d_global_skills_mount")]
    pub global_skills_mount: String,

    // === Workspace 内目录名（相对 workspace_dir） ===

    /// 用户私有 Skill 目录
    #[serde(default = "d_skills_dir")]
    pub skills_dir: String,

    /// 长期记忆目录
    #[serde(default = "d_memory_dir")]
    pub memory_dir: String,

    /// 私有案例目录
    #[serde(default = "d_cases_dir")]
    pub cases_dir: String,

    /// 附件目录（代码控制，在 work/ 下）
    #[serde(default = "d_attachments_dir")]
    pub attachments_dir: String,

    /// 临时文件目录（代码控制，在 work/ 下）
    #[serde(default = "d_tmp_dir")]
    pub tmp_dir: String,

    /// 子任务调度目录（代码控制，在 work/ 下）
    #[serde(default = "d_dispatches_dir")]
    pub dispatches_dir: String,

    /// 会话历史文件名
    #[serde(default = "d_history_file")]
    pub history_file: String,

    /// 定时任务文件名
    #[serde(default = "d_timer_file")]
    pub timer_file: String,
}

impl Default for PathConfig {
    fn default() -> Self {
        Self {
            container_root: d_container_root(),
            container_workdir: d_container_workdir(),
            global_skills_mount: d_global_skills_mount(),
            skills_dir: d_skills_dir(),
            memory_dir: d_memory_dir(),
            cases_dir: d_cases_dir(),
            attachments_dir: d_attachments_dir(),
            tmp_dir: d_tmp_dir(),
            dispatches_dir: d_dispatches_dir(),
            history_file: d_history_file(),
            timer_file: d_timer_file(),
        }
    }
}

impl PathConfig {
    /// 生成 prompts.yaml 变量替换表。
    pub fn to_prompt_vars(&self) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        vars.insert("CONTAINER_ROOT".into(), self.container_root.clone());
        vars.insert("SKILLS_DIR".into(), self.skills_dir.clone());
        vars.insert("MEMORY_DIR".into(), self.memory_dir.clone());
        vars.insert("CASES_DIR".into(), self.cases_dir.clone());
        vars.insert("ATTACHMENTS_DIR".into(), self.attachments_dir.clone());
        vars.insert("TMP_DIR".into(), self.tmp_dir.clone());
        vars.insert("DISPATCHES_DIR".into(), self.dispatches_dir.clone());
        vars.insert("GLOBAL_SKILLS_DIR".into(), self.global_skills_mount.clone());
        vars.insert("GLOBAL_SKILLS_PATH".into(), format!("{}/{}", self.container_root, self.global_skills_mount));
        vars
    }
}

fn d_container_root() -> String { "/workspace".into() }
fn d_container_workdir() -> String { "/workspace".into() }
fn d_global_skills_mount() -> String { "skills".into() }
fn d_skills_dir() -> String { "_personal/skills".into() }
fn d_memory_dir() -> String { "memory".into() }
fn d_cases_dir() -> String { "cases".into() }
fn d_attachments_dir() -> String { "work/attachments".into() }
fn d_tmp_dir() -> String { "work/tmp".into() }
fn d_dispatches_dir() -> String { "work/dispatches".into() }
fn d_history_file() -> String { "history.jsonl".into() }
fn d_timer_file() -> String { "timer_jobs.json".into() }

// ─── Workspace Key Strategy ──────────────────────────────────────────────────

/// Workspace key 解析策略。
///
/// 从请求上下文中提取 workspace_key，决定该请求归属哪个 workspace。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKeyStrategy {
    /// 按用户 ID：一个用户一个 workspace（个人助理模式）
    UserId,
    /// 按会话 ID：私聊用 staff_id，群聊用 conversation_id
    Conversation,
}

impl Default for WorkspaceKeyStrategy {
    fn default() -> Self {
        Self::UserId
    }
}

/// 请求上下文，用于解析 workspace key。
pub struct RequestIdentity<'a> {
    pub user_id: &'a str,
    pub channel: &'a str,
    pub chat_id: &'a str,
    /// 钉钉的 conversation_id（群聊时为群 ID，私聊时可为空）
    pub conversation_id: Option<&'a str>,
}

impl WorkspaceKeyStrategy {
    /// 根据策略从请求上下文中提取 workspace key。
    pub fn resolve(&self, identity: &RequestIdentity<'_>) -> String {
        match self {
            Self::UserId => identity.user_id.to_string(),
            Self::Conversation => {
                // 私聊：用 user_id；群聊：用 conversation_id
                if identity.channel.contains("private") || identity.channel == "cli" {
                    identity.user_id.to_string()
                } else {
                    identity
                        .conversation_id
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| identity.chat_id.to_string())
                }
            }
        }
    }
}

// ─── Workspace 路径解析 ──────────────────────────────────────────────────────

/// 计算分桶目录名：md5(key) 的第一个字节，格式化为 2 位十六进制。
fn bucket_name(workspace_key: &str) -> String {
    let hash = Md5::digest(workspace_key.as_bytes());
    format!("{:02x}", hash[0])
}

/// 计算 workspace 的物理目录路径：`{works_base}/{bucket}/{key}`
///
/// `works_base` 默认为 `{root}/works`，可通过 --works-dir 覆盖。
pub fn workspace_path_in(works_base: &Path, workspace_key: &str) -> PathBuf {
    works_base
        .join(bucket_name(workspace_key))
        .join(workspace_key)
}

/// 计算 workspace 的物理目录路径：`{root}/works/{bucket}/{key}`（默认 works 目录）
pub fn workspace_path(root: &Path, workspace_key: &str) -> PathBuf {
    workspace_path_in(&root.join("works"), workspace_key)
}

// ─── Workspace 定义 ──────────────────────────────────────────────────────────

/// 工作区实例 —— 代表一个独立的持久化空间。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub workspace_id: String,
    pub name: String,
    pub builtin_capabilities: Vec<String>,
    pub members: HashMap<String, String>,
    pub default_role: String,
    pub config: HashMap<String, serde_json::Value>,
}

impl Workspace {
    pub fn new(workspace_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            name: name.into(),
            builtin_capabilities: vec![
                "skill-creator".into(),
                "video-analyzer".into(),
                "ltv-analysis".into(),
            ],
            members: HashMap::new(),
            default_role: "member".into(),
            config: HashMap::new(),
        }
    }
}

/// 配置文件中的 workspace 定义。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub name: Option<String>,
    pub builtin_capabilities: Option<Vec<String>>,
    pub members: Option<HashMap<String, String>>,
    pub default_role: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ─── Workspace Manager ───────────────────────────────────────────────────────

/// 工作区管理器 —— 管理 workspace 实例、目录结构和 key 策略。
pub struct WorkspaceManager {
    /// 顶层根目录（命令行 --run-dir 参数）
    root: PathBuf,
    /// works 目录（默认 {root}/works，可通过 --works-dir 覆盖）
    works_dir: PathBuf,
    /// workspace key 解析策略
    key_strategy: WorkspaceKeyStrategy,
    /// workspace 实例缓存（多租户配置）
    workspaces: Mutex<HashMap<String, Workspace>>,
    /// 路径配置
    paths: PathConfig,
}

impl WorkspaceManager {
    pub fn new(
        root: impl AsRef<Path>,
        key_strategy: WorkspaceKeyStrategy,
        workspaces_config: Option<HashMap<String, WorkspaceConfig>>,
    ) -> Self {
        Self::with_path_config(root, key_strategy, workspaces_config, PathConfig::default())
    }

    pub fn with_path_config(
        root: impl AsRef<Path>,
        key_strategy: WorkspaceKeyStrategy,
        workspaces_config: Option<HashMap<String, WorkspaceConfig>>,
        paths: PathConfig,
    ) -> Self {
        let root = root.as_ref().to_path_buf();
        let works_dir = root.join("works");
        let mut ws_map = HashMap::new();
        Self::init_from_config(&mut ws_map, workspaces_config);
        let mgr = Self {
            root: root.clone(),
            works_dir,
            key_strategy,
            workspaces: Mutex::new(ws_map),
            paths,
        };
        debug!(
            root = %mgr.root.display(),
            strategy = ?mgr.key_strategy,
            "WorkspaceManager initialized"
        );
        mgr
    }

    /// 获取路径配置。
    pub fn path_config(&self) -> &PathConfig {
        &self.paths
    }

    fn init_from_config(
        ws_map: &mut HashMap<String, Workspace>,
        config: Option<HashMap<String, WorkspaceConfig>>,
    ) {
        let Some(config) = config else {
            ws_map.insert("default".into(), Workspace::new("default", "Default"));
            return;
        };

        if config.is_empty() {
            ws_map.insert("default".into(), Workspace::new("default", "Default"));
            return;
        }

        for (workspace_id, ws_cfg) in config {
            let mut ws = Workspace::new(
                workspace_id.clone(),
                ws_cfg.name.unwrap_or_else(|| workspace_id.clone()),
            );
            if let Some(caps) = ws_cfg.builtin_capabilities {
                ws.builtin_capabilities = caps;
            }
            if let Some(members) = ws_cfg.members {
                ws.members = members;
            }
            if let Some(default_role) = ws_cfg.default_role {
                ws.default_role = default_role;
            }
            ws.config = ws_cfg.extra;
            ws_map.insert(workspace_id, ws);
        }
    }

    // ── Key 解析 ──

    /// 覆盖 works 目录路径（对应 --works-dir 命令行参数）。
    pub fn set_works_dir(&mut self, path: impl AsRef<Path>) {
        self.works_dir = path.as_ref().to_path_buf();
    }

    /// 获取 works 目录。
    pub fn works_dir(&self) -> &Path {
        &self.works_dir
    }

    /// 获取当前策略。
    pub fn key_strategy(&self) -> &WorkspaceKeyStrategy {
        &self.key_strategy
    }

    /// 根据策略解析 workspace key。
    pub fn resolve_key(&self, identity: &RequestIdentity<'_>) -> String {
        self.key_strategy.resolve(identity)
    }

    // ── Workspace 实例 ──

    /// 获取工作区实例（返回克隆），如果不存在则自动创建。
    pub fn get_workspace(&self, workspace_id: &str) -> Workspace {
        let mut workspaces = self.workspaces.lock();
        if !workspaces.contains_key(workspace_id) {
            let ws = Workspace::new(workspace_id, workspace_id);
            workspaces.insert(workspace_id.to_string(), ws);
        }
        workspaces[workspace_id].clone()
    }

    /// 获取用户在指定工作区中的角色。
    pub fn get_user_role(&self, workspace_id: &str, user_id: &str) -> String {
        let ws = self.get_workspace(workspace_id);
        ws.members
            .get(user_id)
            .cloned()
            .unwrap_or_else(|| ws.default_role.clone())
    }

    /// 设置用户在指定工作区中的角色。
    pub fn set_user_role(&self, workspace_id: &str, user_id: &str, role: &str) {
        let mut workspaces = self.workspaces.lock();
        if !workspaces.contains_key(workspace_id) {
            let ws = Workspace::new(workspace_id, workspace_id);
            workspaces.insert(workspace_id.to_string(), ws);
        }
        if let Some(ws) = workspaces.get_mut(workspace_id) {
            ws.members.insert(user_id.to_string(), role.to_string());
        }
    }

    // ── 顶层根目录 ──

    /// 顶层根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    // ── 全局目录（不分 workspace）──

    /// 全局配置目录：`{root}/config`
    pub fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }

    /// 全局共享技能目录：`{root}/skills`
    pub fn global_skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    /// 全局共享案例目录：`{root}/cases`
    pub fn global_cases_dir(&self) -> PathBuf {
        self.root.join("cases")
    }

    /// 全局审计日志目录：`{root}/audit`
    pub fn audit_dir(&self) -> PathBuf {
        self.root.join("audit")
    }

    /// 全局日志目录：`{root}/logs`
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    // ── Workspace 级目录 ──

    /// workspace 根目录：`{works_dir}/{bucket}/{key}`
    pub fn workspace_dir(&self, workspace_key: &str) -> PathBuf {
        workspace_path_in(&self.works_dir, workspace_key)
    }

    /// workspace 记忆目录（可配置，默认 `memory`）
    pub fn memory_dir(&self, workspace_key: &str) -> PathBuf {
        self.workspace_dir(workspace_key).join(&self.paths.memory_dir)
    }

    /// workspace 私有技能目录（可配置，默认 `skills`）
    pub fn workspace_skills_dir(&self, workspace_key: &str) -> PathBuf {
        self.workspace_dir(workspace_key).join(&self.paths.skills_dir)
    }

    /// workspace 私有案例目录（可配置，默认 `cases`）
    pub fn workspace_cases_dir(&self, workspace_key: &str) -> PathBuf {
        self.workspace_dir(workspace_key).join(&self.paths.cases_dir)
    }

    /// workspace 对话历史文件（可配置，默认 `history.jsonl`）
    pub fn history_path(&self, workspace_key: &str) -> PathBuf {
        self.workspace_dir(workspace_key).join(&self.paths.history_file)
    }

    /// workspace 定时任务文件（可配置，默认 `timer_jobs.json`）
    pub fn timer_path(&self, workspace_key: &str) -> PathBuf {
        self.workspace_dir(workspace_key).join(&self.paths.timer_file)
    }

    /// workspace 附件目录（可配置，默认 `work/attachments`）
    pub fn attachments_dir(&self, workspace_key: &str) -> PathBuf {
        self.workspace_dir(workspace_key).join(&self.paths.attachments_dir)
    }

    /// workspace tmp 目录（可配置，默认 `work/tmp`）
    pub fn tmp_dir(&self, workspace_key: &str) -> PathBuf {
        self.workspace_dir(workspace_key).join(&self.paths.tmp_dir)
    }

    /// workspace dispatches 目录（可配置，默认 `work/dispatches`）
    pub fn dispatches_dir(&self, workspace_key: &str) -> PathBuf {
        self.workspace_dir(workspace_key).join(&self.paths.dispatches_dir)
    }

    /// workspace work 目录（work/ 子目录的父级）
    pub fn work_dir(&self, workspace_key: &str) -> PathBuf {
        self.workspace_dir(workspace_key).join("work")
    }

    // ── 扫描 ──

    /// 列出所有已存在的 workspace key（扫描 works 目录下所有分桶）。
    pub fn list_workspace_keys(&self) -> Vec<String> {
        let works_dir = &self.works_dir;
        let Ok(buckets) = std::fs::read_dir(&works_dir) else {
            return Vec::new();
        };
        let mut keys = Vec::new();
        for bucket_entry in buckets.flatten() {
            if !bucket_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(bucket_entry.path()) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        if let Ok(name) = entry.file_name().into_string() {
                            keys.push(name);
                        }
                    }
                }
            }
        }
        keys
    }

    /// 确保 workspace 目录结构存在。
    pub fn ensure_workspace(&self, workspace_key: &str) {
        let ws_dir = self.workspace_dir(workspace_key);
        let _ = std::fs::create_dir_all(ws_dir.join(&self.paths.memory_dir));
        let _ = std::fs::create_dir_all(ws_dir.join(&self.paths.skills_dir));
        let _ = std::fs::create_dir_all(ws_dir.join(&self.paths.cases_dir));
        let _ = std::fs::create_dir_all(ws_dir.join(&self.paths.tmp_dir));
        let _ = std::fs::create_dir_all(ws_dir.join(&self.paths.attachments_dir));
        let _ = std::fs::create_dir_all(ws_dir.join(&self.paths.dispatches_dir));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_bucket_name_deterministic() {
        let b1 = bucket_name("0307663902380");
        let b2 = bucket_name("0307663902380");
        assert_eq!(b1, b2);
        assert_eq!(b1.len(), 2);
    }

    #[test]
    fn test_bucket_name_different_keys() {
        // 不同 key 大概率不同桶（不保证，但验证格式正确）
        let b = bucket_name("test_key");
        assert_eq!(b.len(), 2);
        // 验证是有效十六进制
        assert!(u8::from_str_radix(&b, 16).is_ok());
    }

    #[test]
    fn test_workspace_path() {
        let root = Path::new("/data");
        let path = workspace_path(root, "0307663902380");
        let bucket = bucket_name("0307663902380");
        assert_eq!(
            path,
            PathBuf::from(format!("/data/works/{}/0307663902380", bucket))
        );
    }

    #[test]
    fn test_strategy_user_id() {
        let strategy = WorkspaceKeyStrategy::UserId;
        let identity = RequestIdentity {
            user_id: "staff123",
            channel: "dingtalk_group",
            chat_id: "conv:staff123",
            conversation_id: Some("cidXXX"),
        };
        assert_eq!(strategy.resolve(&identity), "staff123");
    }

    #[test]
    fn test_strategy_conversation_group() {
        let strategy = WorkspaceKeyStrategy::Conversation;
        let identity = RequestIdentity {
            user_id: "staff123",
            channel: "dingtalk_group",
            chat_id: "cidXXX:staff123",
            conversation_id: Some("cidXXX"),
        };
        assert_eq!(strategy.resolve(&identity), "cidXXX");
    }

    #[test]
    fn test_strategy_conversation_private() {
        let strategy = WorkspaceKeyStrategy::Conversation;
        let identity = RequestIdentity {
            user_id: "staff123",
            channel: "dingtalk_private",
            chat_id: "staff123",
            conversation_id: None,
        };
        assert_eq!(strategy.resolve(&identity), "staff123");
    }

    #[test]
    fn test_strategy_conversation_cli() {
        let strategy = WorkspaceKeyStrategy::Conversation;
        let identity = RequestIdentity {
            user_id: "cli_user",
            channel: "cli",
            chat_id: "direct",
            conversation_id: None,
        };
        assert_eq!(strategy.resolve(&identity), "cli_user");
    }

    #[test]
    fn test_workspace_manager_dirs() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkspaceManager::new(tmp.path(), WorkspaceKeyStrategy::UserId, None);
        let key = "staff123";
        let bucket = bucket_name(key);

        assert_eq!(
            mgr.workspace_dir(key),
            tmp.path().join("works").join(&bucket).join(key)
        );
        assert_eq!(
            mgr.memory_dir(key),
            tmp.path().join("works").join(&bucket).join(key).join("memory")
        );
        assert_eq!(
            mgr.work_dir(key),
            tmp.path().join("works").join(&bucket).join(key).join("work")
        );
        assert_eq!(
            mgr.history_path(key),
            tmp.path().join("works").join(&bucket).join(key).join("history.jsonl")
        );
        assert_eq!(
            mgr.timer_path(key),
            tmp.path().join("works").join(&bucket).join(key).join("timer_jobs.json")
        );
    }

    #[test]
    fn test_ensure_workspace_creates_dirs() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkspaceManager::new(tmp.path(), WorkspaceKeyStrategy::UserId, None);
        mgr.ensure_workspace("testuser");
        let ws = mgr.workspace_dir("testuser");
        assert!(ws.join("memory").is_dir());
        assert!(ws.join("skills").is_dir());
        assert!(ws.join("cases").is_dir());
        assert!(ws.join("work").join("tmp").is_dir());
        assert!(ws.join("work").join("attachments").is_dir());
        assert!(ws.join("work").join("dispatches").is_dir());
    }

    #[test]
    fn test_global_dirs() {
        let mgr = WorkspaceManager::new("/data", WorkspaceKeyStrategy::UserId, None);
        assert_eq!(mgr.global_skills_dir(), PathBuf::from("/data/skills"));
        assert_eq!(mgr.global_cases_dir(), PathBuf::from("/data/cases"));
        assert_eq!(mgr.audit_dir(), PathBuf::from("/data/audit"));
    }

    #[test]
    fn test_list_workspace_keys() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkspaceManager::new(tmp.path(), WorkspaceKeyStrategy::UserId, None);

        mgr.ensure_workspace("alice");
        mgr.ensure_workspace("bob");

        let mut keys = mgr.list_workspace_keys();
        keys.sort();
        assert_eq!(keys, vec!["alice", "bob"]);
    }

    #[test]
    fn test_user_role_default() {
        let mgr = WorkspaceManager::new("/tmp", WorkspaceKeyStrategy::UserId, None);
        let role = mgr.get_user_role("ws1", "unknown_user");
        assert_eq!(role, "member");
    }

    #[test]
    fn test_set_user_role() {
        let mgr = WorkspaceManager::new("/tmp", WorkspaceKeyStrategy::UserId, None);
        mgr.set_user_role("ws1", "alice", "admin");
        assert_eq!(mgr.get_user_role("ws1", "alice"), "admin");
    }

    #[test]
    fn test_load_workspace_config() {
        use serde_json::json;
        let mut cfg = HashMap::new();
        cfg.insert(
            "prod".to_string(),
            WorkspaceConfig {
                name: Some("Production".into()),
                builtin_capabilities: Some(vec!["code-analysis".into()]),
                members: Some(HashMap::from([("alice".into(), "admin".into())])),
                default_role: Some("guest".into()),
                extra: HashMap::from([("note".into(), json!("critical"))]),
            },
        );

        let mgr = WorkspaceManager::new("/tmp", WorkspaceKeyStrategy::UserId, Some(cfg));
        let ws = mgr.get_workspace("prod");
        assert_eq!(ws.name, "Production");
        assert_eq!(ws.default_role, "guest");
        assert_eq!(ws.builtin_capabilities, vec!["code-analysis"]);
        assert_eq!(ws.members.get("alice").map(String::as_str), Some("admin"));
        assert_eq!(ws.config.get("note"), Some(&json!("critical")));
    }
}
