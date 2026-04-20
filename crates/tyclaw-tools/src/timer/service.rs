use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json;
use tokio::sync::{mpsc, Notify, RwLock};
use tracing::{info, warn};
use uuid::Uuid;

use tyclaw_types::TyclawError;

use md5::{Digest, Md5};

use super::types::*;

/// 计算 workspace 路径（与 tyclaw_control::workspace_path 一致）。
fn workspace_path(root: &Path, workspace_key: &str) -> PathBuf {
    let hash = Md5::digest(workspace_key.as_bytes());
    root.join("works")
        .join(format!("{:02x}", hash[0]))
        .join(workspace_key)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn compute_next_run(schedule: &TimerSchedule, ref_ms: i64) -> Option<i64> {
    match schedule {
        TimerSchedule::At { at_ms } => {
            if *at_ms > ref_ms {
                Some(*at_ms)
            } else {
                None
            }
        }
        TimerSchedule::Every { interval_ms } => {
            if *interval_ms == 0 {
                return None;
            }
            Some(ref_ms + *interval_ms as i64)
        }
        TimerSchedule::Cron { expr, tz } => {
            use chrono::{Local, TimeZone};
            use croner::Cron;

            let cron = match Cron::new(expr).parse() {
                Ok(c) => c,
                Err(_) => return None,
            };

            // Cron fields (hour, minute, etc.) must be matched in the target timezone,
            // not in UTC. Otherwise "11 12 * * *" with tz=Asia/Shanghai would match
            // 12:11 UTC (= 20:11 CST) instead of 12:11 CST.
            if let Some(tz_name) = tz {
                match tz_name.parse::<chrono_tz::Tz>() {
                    Ok(tz_val) => {
                        let dt = tz_val.timestamp_millis_opt(ref_ms).single()?;
                        let next = cron.find_next_occurrence(&dt, false).ok()?;
                        Some(next.timestamp_millis())
                    }
                    Err(_) => {
                        let dt = Local.timestamp_millis_opt(ref_ms).single()?;
                        let next = cron.find_next_occurrence(&dt, false).ok()?;
                        Some(next.timestamp_millis())
                    }
                }
            } else {
                let dt = Local.timestamp_millis_opt(ref_ms).single()?;
                let next = cron.find_next_occurrence(&dt, false).ok()?;
                Some(next.timestamp_millis())
            }
        }
    }
}

fn validate_schedule(schedule: &TimerSchedule) -> Result<(), String> {
    if let TimerSchedule::Cron { expr, tz } = schedule {
        use croner::Cron;
        Cron::new(expr)
            .parse()
            .map_err(|e| format!("invalid cron expression: {e}"))?;
        if let Some(tz_name) = tz {
            tz_name
                .parse::<chrono_tz::Tz>()
                .map_err(|_| format!("unknown timezone '{tz_name}'"))?;
        }
    }
    Ok(())
}

tokio::task_local! {
    /// 当前请求的 channel（per-request，避免并发覆盖）。
    pub static TIMER_CURRENT_CHANNEL: String;
    /// 当前请求的 chat_id（per-request，避免并发覆盖）。
    pub static TIMER_CURRENT_CHAT_ID: String;
    /// 当前请求的 user_id（per-request，用于 timer 创建时记录发送目标）。
    pub static TIMER_CURRENT_USER_ID: String;
    /// 当前请求的 conversation_id（per-request，钉钉群聊场景）。
    pub static TIMER_CURRENT_CONVERSATION_ID: String;
    /// 是否在 timer 回调中执行（per-request 递归防护）。
    pub static TIMER_IN_CONTEXT: bool;
}

/// 按 workspace 隔离存储、单调度器的定时任务服务。
///
/// 存储：`{root}/works/{bucket}/{workspace_key}/timer_jobs.json`
/// 调度：单 run_loop，统一优先级队列扫描所有 workspace 的 job。
pub struct TimerService {
    root: PathBuf,
    /// user_id → TimerStore（内存中合并所有用户）
    stores: RwLock<HashMap<String, TimerStore>>,
    /// user_id → 文件 mtime（用于检测外部修改）
    mtimes: RwLock<HashMap<String, f64>>,
    due_sender: mpsc::Sender<TimerJob>,
    notify: Notify,
    running: AtomicBool,
}

impl TimerService {
    /// 创建 TimerService。`root` 是顶层 workspace 根目录（命令行 --workspace 参数）。
    pub fn new(root: impl AsRef<Path>) -> (Arc<Self>, mpsc::Receiver<TimerJob>) {
        let (tx, rx) = mpsc::channel(32);
        let svc = Arc::new(Self {
            root: root.as_ref().to_path_buf(),
            stores: RwLock::new(HashMap::new()),
            mtimes: RwLock::new(HashMap::new()),
            due_sender: tx,
            notify: Notify::new(),
            running: AtomicBool::new(false),
        });
        (svc, rx)
    }

    /// 读取当前请求的 channel（从 task_local）。
    pub fn current_channel(&self) -> String {
        TIMER_CURRENT_CHANNEL
            .try_with(|c| c.clone())
            .unwrap_or_default()
    }

    /// 读取当前请求的 chat_id（从 task_local）。
    pub fn current_chat_id(&self) -> String {
        TIMER_CURRENT_CHAT_ID
            .try_with(|c| c.clone())
            .unwrap_or_default()
    }

    /// 读取当前请求的 user_id（从 task_local）。
    pub fn current_user_id(&self) -> String {
        TIMER_CURRENT_USER_ID
            .try_with(|c| c.clone())
            .unwrap_or_default()
    }

    /// 读取当前请求的 conversation_id（从 task_local）。
    pub fn current_conversation_id(&self) -> String {
        TIMER_CURRENT_CONVERSATION_ID
            .try_with(|c| c.clone())
            .unwrap_or_default()
    }

    /// 是否在 timer 回调中执行（从 task_local）。
    pub fn is_in_timer_context(&self) -> bool {
        TIMER_IN_CONTEXT.try_with(|v| *v).unwrap_or(false)
    }

    pub async fn start(self: &Arc<Self>) {
        self.running.store(true, Ordering::SeqCst);
        self.load_all_stores().await;
        self.recompute_next_runs().await;
        self.save_all_dirty().await;

        let total: usize = self
            .stores
            .read()
            .await
            .values()
            .map(|s| s.jobs.len())
            .sum();
        let user_count = self.stores.read().await.len();
        info!(
            "Timer service started: {} jobs across {} users",
            total, user_count
        );

        let svc = self.clone();
        tokio::spawn(async move { svc.run_loop().await });
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.notify.notify_one();
    }

    // ── Per-workspace file path ──

    fn workspace_store_path(&self, workspace_key: &str) -> PathBuf {
        workspace_path(&self.root, workspace_key).join("timer_jobs.json")
    }

    fn file_mtime(path: &Path) -> f64 {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .map(|t| {
                t.duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64()
            })
            .unwrap_or(0.0)
    }

    // ── Persistence ──

    /// 启动时扫描 works/ 下所有 workspace 的 timer_jobs.json。
    async fn load_all_stores(&self) {
        let works_dir = self.root.join("works");
        let buckets = match std::fs::read_dir(&works_dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };

        let mut stores = self.stores.write().await;
        let mut mtimes = self.mtimes.write().await;

        for bucket_entry in buckets.flatten() {
            if !bucket_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Ok(ws_entries) = std::fs::read_dir(bucket_entry.path()) else {
                continue;
            };
            for ws_entry in ws_entries.flatten() {
                if !ws_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let workspace_key = match ws_entry.file_name().into_string() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let path = ws_entry.path().join("timer_jobs.json");
                if !path.exists() {
                    continue;
                }
                match std::fs::read_to_string(&path) {
                    Ok(text) => match serde_json::from_str::<TimerStore>(&text) {
                        Ok(store) => {
                            let mtime = Self::file_mtime(&path);
                            mtimes.insert(workspace_key.clone(), mtime);
                            stores.insert(workspace_key, store);
                        }
                        Err(e) => warn!("Timer: failed to parse {}: {e}", path.display()),
                    },
                    Err(e) => warn!("Timer: failed to read {}: {e}", path.display()),
                }
            }
        }
    }

    /// 重新加载单个 workspace 的 store（如果 mtime 变了）。
    #[allow(dead_code)]
    async fn reload_workspace_store(&self, user_id: &str) {
        let path = self.workspace_store_path(user_id);
        if !path.exists() {
            return;
        }
        let mtime = Self::file_mtime(&path);
        let cached = self.mtimes.read().await.get(user_id).copied().unwrap_or(0.0);
        if cached != 0.0 && (mtime - cached).abs() < f64::EPSILON {
            return;
        }
        match tokio::fs::read_to_string(&path).await {
            Ok(text) => match serde_json::from_str::<TimerStore>(&text) {
                Ok(store) => {
                    self.stores.write().await.insert(user_id.to_string(), store);
                    self.mtimes.write().await.insert(user_id.to_string(), mtime);
                }
                Err(e) => warn!("Timer: failed to parse {}: {e}", path.display()),
            },
            Err(e) => warn!("Timer: failed to read {}: {e}", path.display()),
        }
    }

    /// 保存单个 workspace 的 store 到文件。
    async fn save_workspace_store(&self, user_id: &str) {
        let stores = self.stores.read().await;
        let store = match stores.get(user_id) {
            Some(s) => s,
            None => return,
        };
        let json = match serde_json::to_string_pretty(store) {
            Ok(j) => j,
            Err(e) => {
                warn!("Timer: failed to serialize store for {user_id}: {e}");
                return;
            }
        };
        drop(stores);

        let path = self.workspace_store_path(user_id);
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }

        let tmp_path = path.with_extension("json.tmp");
        if let Err(e) = tokio::fs::write(&tmp_path, &json).await {
            warn!("Timer: failed to write tmp file for {user_id}: {e}");
            return;
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &path).await {
            warn!("Timer: failed to rename tmp → store for {user_id}: {e}");
            return;
        }

        let mtime = Self::file_mtime(&path);
        self.mtimes.write().await.insert(user_id.to_string(), mtime);
    }

    /// 保存所有有 job 的用户 store。
    async fn save_all_dirty(&self) {
        let user_ids: Vec<String> = self.stores.read().await.keys().cloned().collect();
        for uid in user_ids {
            self.save_workspace_store(&uid).await;
        }
    }

    // ── Scheduling ──

    async fn recompute_next_runs(&self) {
        let now = now_ms();
        let mut stores = self.stores.write().await;
        for store in stores.values_mut() {
            for job in store.jobs.iter_mut() {
                if job.enabled {
                    job.state.next_run_at_ms = compute_next_run(&job.schedule, now);
                }
            }
        }
    }

    fn next_wake_ms(stores: &HashMap<String, TimerStore>) -> Option<i64> {
        stores
            .values()
            .flat_map(|s| &s.jobs)
            .filter(|j| j.enabled && j.state.next_run_at_ms.is_some())
            .filter_map(|j| j.state.next_run_at_ms)
            .min()
    }

    async fn run_loop(&self) {
        while self.running.load(Ordering::SeqCst) {
            let delay = {
                let stores = self.stores.read().await;
                match Self::next_wake_ms(&stores) {
                    Some(next_ms) => {
                        let diff = next_ms - now_ms();
                        if diff <= 0 {
                            Duration::ZERO
                        } else {
                            Duration::from_millis(diff as u64)
                        }
                    }
                    None => Duration::from_secs(3600),
                }
            };

            tokio::select! {
                _ = tokio::time::sleep(delay) => {
                    if self.running.load(Ordering::SeqCst) {
                        self.fire_due_jobs().await;
                    }
                }
                _ = self.notify.notified() => {
                    // add/remove/enable triggered recalculation
                }
            }
        }
        info!("Timer service stopped");
    }

    async fn fire_due_jobs(&self) {
        let now = now_ms();

        // 收集到期 job 和对应 user_id
        let due_jobs: Vec<(String, TimerJob)> = {
            let stores = self.stores.read().await;
            stores
                .iter()
                .flat_map(|(uid, store)| {
                    store
                        .jobs
                        .iter()
                        .filter(|j| {
                            j.enabled && j.state.next_run_at_ms.map(|t| now >= t).unwrap_or(false)
                        })
                        .map(|j| (uid.clone(), j.clone()))
                })
                .collect()
        };

        for (_, job) in &due_jobs {
            info!("Timer: firing job '{}' ({}) for user '{}'", job.name, job.id, job.user_id);
            let _ = self.due_sender.send(job.clone()).await;
        }

        if !due_jobs.is_empty() {
            // 按 user_id 分组更新
            let mut affected_users: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut stores = self.stores.write().await;
            let now_after = now_ms();

            for (uid, due) in &due_jobs {
                if let Some(store) = stores.get_mut(uid) {
                    if let Some(j) = store.jobs.iter_mut().find(|j| j.id == due.id) {
                        j.state.last_run_at_ms = Some(now_after);
                        j.updated_at_ms = now_after;

                        match &j.schedule {
                            TimerSchedule::At { .. } => {
                                if !j.delete_after_run {
                                    j.enabled = false;
                                    j.state.next_run_at_ms = None;
                                }
                            }
                            _ => {
                                j.state.next_run_at_ms =
                                    compute_next_run(&j.schedule, now_after);
                            }
                        }
                    }
                    // 清理 delete_after_run 的一次性任务
                    store.jobs.retain(|j| {
                        !(j.delete_after_run
                            && matches!(j.schedule, TimerSchedule::At { .. })
                            && due_jobs.iter().any(|(_, d)| d.id == j.id))
                    });
                    affected_users.insert(uid.clone());
                }
            }
            drop(stores);

            for uid in affected_users {
                self.save_workspace_store(&uid).await;
            }
        }
    }

    // ── Public API ──

    pub async fn add_job(
        &self,
        user_id: &str,
        name: &str,
        schedule: TimerSchedule,
        message: &str,
        deliver: bool,
        channel: Option<&str>,
        chat_id: Option<&str>,
        conversation_id: Option<&str>,
        delete_after_run: bool,
    ) -> Result<TimerJob, TyclawError> {
        if user_id.is_empty() {
            return Err(TyclawError::Tool {
                tool: "timer".into(),
                message: "user_id is required".into(),
            });
        }

        validate_schedule(&schedule).map_err(|e| TyclawError::Tool {
            tool: "timer".into(),
            message: e,
        })?;

        let now = now_ms();
        let next = compute_next_run(&schedule, now);

        let job = TimerJob {
            id: Uuid::new_v4().to_string()[..8].to_string(),
            name: name.to_string(),
            user_id: user_id.to_string(),
            enabled: true,
            schedule,
            payload: TimerPayload {
                message: message.to_string(),
                deliver,
                channel: channel.map(|s| s.to_string()),
                chat_id: chat_id.map(|s| s.to_string()),
                workspace_id: None,
                user_id: user_id.to_string(),
                conversation_id: conversation_id.map(|s| s.to_string()),
            },
            state: TimerJobState {
                next_run_at_ms: next,
                ..Default::default()
            },
            created_at_ms: now,
            updated_at_ms: now,
            delete_after_run,
        };

        {
            let mut stores = self.stores.write().await;
            stores
                .entry(user_id.to_string())
                .or_insert_with(|| TimerStore {
                    version: 1,
                    jobs: Vec::new(),
                })
                .jobs
                .push(job.clone());
        }
        self.save_workspace_store(user_id).await;
        self.notify.notify_one();

        info!("Timer: added job '{}' ({}) for user '{}'", name, job.id, user_id);
        Ok(job)
    }

    /// 删除指定用户的 job。只能删除自己的。
    pub async fn remove_job(&self, user_id: &str, job_id: &str) -> bool {
        let removed = {
            let mut stores = self.stores.write().await;
            if let Some(store) = stores.get_mut(user_id) {
                let before = store.jobs.len();
                store.jobs.retain(|j| j.id != job_id);
                store.jobs.len() < before
            } else {
                false
            }
        };
        if removed {
            self.save_workspace_store(user_id).await;
            self.notify.notify_one();
            info!("Timer: removed job {} for user '{}'", job_id, user_id);
        }
        removed
    }

    /// 列出指定用户的 job。
    pub async fn list_jobs(&self, user_id: &str, include_disabled: bool) -> Vec<TimerJob> {
        let stores = self.stores.read().await;
        let Some(store) = stores.get(user_id) else {
            return Vec::new();
        };
        let mut jobs: Vec<TimerJob> = if include_disabled {
            store.jobs.clone()
        } else {
            store.jobs.iter().filter(|j| j.enabled).cloned().collect()
        };
        jobs.sort_by_key(|j| j.state.next_run_at_ms.unwrap_or(i64::MAX));
        jobs
    }

    pub async fn enable_job(&self, user_id: &str, job_id: &str, enabled: bool) -> Option<TimerJob> {
        let result = {
            let mut stores = self.stores.write().await;
            let store = stores.get_mut(user_id)?;
            let job = store.jobs.iter_mut().find(|j| j.id == job_id)?;
            job.enabled = enabled;
            job.updated_at_ms = now_ms();
            if enabled {
                job.state.next_run_at_ms = compute_next_run(&job.schedule, now_ms());
            } else {
                job.state.next_run_at_ms = None;
            }
            Some(job.clone())
        };
        if result.is_some() {
            self.save_workspace_store(user_id).await;
            self.notify.notify_one();
        }
        result
    }

    pub async fn get_job(&self, user_id: &str, job_id: &str) -> Option<TimerJob> {
        let stores = self.stores.read().await;
        stores
            .get(user_id)?
            .jobs
            .iter()
            .find(|j| j.id == job_id)
            .cloned()
    }

    pub async fn record_result(
        &self,
        user_id: &str,
        job_id: &str,
        status: &str,
        duration_ms: u64,
        error: Option<&str>,
    ) {
        {
            let mut stores = self.stores.write().await;
            if let Some(store) = stores.get_mut(user_id) {
                if let Some(job) = store.jobs.iter_mut().find(|j| j.id == job_id) {
                    job.state.last_status = Some(status.to_string());
                    job.state.last_error = error.map(|s| s.to_string());
                    job.state.run_history.push(TimerRunRecord {
                        run_at_ms: now_ms(),
                        status: status.to_string(),
                        duration_ms,
                        error: error.map(|s| s.to_string()),
                    });
                    let max = MAX_RUN_HISTORY;
                    if job.state.run_history.len() > max {
                        let drain = job.state.run_history.len() - max;
                        job.state.run_history.drain(..drain);
                    }
                    job.updated_at_ms = now_ms();
                }
            }
        }
        self.save_workspace_store(user_id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_next_run_at_future() {
        let now = 1000;
        let schedule = TimerSchedule::At { at_ms: 2000 };
        assert_eq!(compute_next_run(&schedule, now), Some(2000));
    }

    #[test]
    fn test_compute_next_run_at_past() {
        let now = 3000;
        let schedule = TimerSchedule::At { at_ms: 2000 };
        assert_eq!(compute_next_run(&schedule, now), None);
    }

    #[test]
    fn test_compute_next_run_every() {
        let now = 1000;
        let schedule = TimerSchedule::Every { interval_ms: 5000 };
        assert_eq!(compute_next_run(&schedule, now), Some(6000));
    }

    #[test]
    fn test_compute_next_run_every_zero() {
        let now = 1000;
        let schedule = TimerSchedule::Every { interval_ms: 0 };
        assert_eq!(compute_next_run(&schedule, now), None);
    }

    #[test]
    fn test_compute_next_run_cron() {
        let now = now_ms();
        let schedule = TimerSchedule::Cron {
            expr: "* * * * *".to_string(),
            tz: None,
        };
        let next = compute_next_run(&schedule, now);
        assert!(next.is_some());
        assert!(next.unwrap() > now);
    }

    #[test]
    fn test_validate_schedule_valid_cron() {
        let schedule = TimerSchedule::Cron {
            expr: "0 9 * * *".to_string(),
            tz: None,
        };
        assert!(validate_schedule(&schedule).is_ok());
    }

    #[test]
    fn test_validate_schedule_invalid_cron() {
        let schedule = TimerSchedule::Cron {
            expr: "not-a-cron".to_string(),
            tz: None,
        };
        assert!(validate_schedule(&schedule).is_err());
    }

    #[test]
    fn test_validate_schedule_bad_tz() {
        let schedule = TimerSchedule::Cron {
            expr: "0 9 * * *".to_string(),
            tz: Some("Not/A/Timezone".to_string()),
        };
        assert!(validate_schedule(&schedule).is_err());
    }

    #[tokio::test]
    async fn test_persistence_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let (svc, _rx) = TimerService::new(dir.path());

        let job = svc
            .add_job(
                "user_alice",
                "test-job",
                TimerSchedule::Every {
                    interval_ms: 60_000,
                },
                "check something",
                false,
                None,
                None,
                None,
                false,
            )
            .await
            .unwrap();

        let store_path = workspace_path(dir.path(), "user_alice").join("timer_jobs.json");
        assert!(store_path.exists());

        // Load in a new service instance
        let (svc2, _rx2) = TimerService::new(dir.path());
        svc2.load_all_stores().await;
        let loaded = svc2.list_jobs("user_alice", true).await;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, job.id);
        assert_eq!(loaded[0].user_id, "user_alice");
        assert_eq!(loaded[0].payload.message, "check something");

        // Other workspace sees nothing
        let other = svc2.list_jobs("user_bob", true).await;
        assert!(other.is_empty());
    }

    #[tokio::test]
    async fn test_workspace_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let (svc, _rx) = TimerService::new(dir.path());

        svc.add_job(
            "alice", "alice-job",
            TimerSchedule::Every { interval_ms: 60_000 },
            "alice task", false, None, None, None, false,
        ).await.unwrap();

        svc.add_job(
            "bob", "bob-job",
            TimerSchedule::Every { interval_ms: 60_000 },
            "bob task", false, None, None, None, false,
        ).await.unwrap();

        assert_eq!(svc.list_jobs("alice", true).await.len(), 1);
        assert_eq!(svc.list_jobs("bob", true).await.len(), 1);
        assert_eq!(svc.list_jobs("alice", true).await[0].name, "alice-job");
        assert_eq!(svc.list_jobs("bob", true).await[0].name, "bob-job");

        // alice can't remove bob's job
        let bob_job_id = svc.list_jobs("bob", true).await[0].id.clone();
        assert!(!svc.remove_job("alice", &bob_job_id).await);
        assert_eq!(svc.list_jobs("bob", true).await.len(), 1);
    }
}
