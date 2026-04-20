//! Workspace 超时回收后台任务（reaper）。
//!
//! 定期扫描活跃 workspace，超时未访问的执行回收：
//! consolidate 对话历史 → memory，清空临时文件，销毁容器。

use std::sync::Arc;
use tracing::info;

use tyclaw_control::AuditEntry;

use crate::orchestrator::Orchestrator;

impl Orchestrator {
    /// 启动 workspace 超时回收后台任务。
    ///
    /// 每 `check_interval_secs` 秒扫描一次活跃 workspace，
    /// 超过 `idle_timeout_secs` 未访问的执行回收：
    /// 1. consolidate 对话历史 → memory
    /// 2. 清空 history.jsonl
    /// 3. 清空 work/tmp、work/dispatches、work/attachments
    /// 4. 销毁 Docker 容器
    pub fn spawn_reaper(
        self: &Arc<Self>,
        idle_timeout_secs: u64,
        check_interval_secs: u64,
    ) {
        let orch = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(check_interval_secs));
            loop {
                interval.tick().await;
                let idle_keys = orch
                    .persistence
                    .sessions
                    .find_idle_workspaces(idle_timeout_secs);
                for workspace_key in idle_keys {
                    // 检查 work 目录下是否有近期文件修改（子 agent 可能仍在执行）
                    let work_dir = orch.persistence.workspace_mgr.work_dir(&workspace_key);
                    if has_recent_file_activity(&work_dir, idle_timeout_secs) {
                        // 刷新 last_access，跳过本轮回收
                        orch.persistence.sessions.touch(&workspace_key);
                        info!(
                            workspace_key = %workspace_key,
                            "Skipping reap: work directory has recent file activity"
                        );
                        continue;
                    }

                    info!(
                        workspace_key = %workspace_key,
                        "Reaping idle workspace"
                    );

                    // 1. consolidate 对话到 memory
                    if orch.app.features.enable_memory {
                        if let Some(session) = orch.persistence.sessions.evict(&workspace_key) {
                            if !session.messages.is_empty() {
                                let mem_dir = orch
                                    .persistence
                                    .workspace_mgr
                                    .memory_dir(&workspace_key);
                                let store =
                                    tyclaw_memory::MemoryStore::new(&mem_dir);
                                let snapshot =
                                    &session.messages[session.last_consolidated..];
                                if !snapshot.is_empty() {
                                    tyclaw_memory::consolidate_with_provider(
                                        &store,
                                        snapshot,
                                        orch.provider.as_ref(),
                                        &orch.app.model,
                                    )
                                    .await;
                                }
                            }
                        }
                    } else {
                        orch.persistence.sessions.evict(&workspace_key);
                    }

                    // 2. 清空 history.jsonl
                    let history_path = orch
                        .persistence
                        .workspace_mgr
                        .history_path(&workspace_key);
                    let _ = std::fs::remove_file(&history_path);

                    // 3. 清空临时目录
                    for dir_name in &["tmp", "dispatches", "attachments"] {
                        let dir = orch
                            .persistence
                            .workspace_mgr
                            .work_dir(&workspace_key)
                            .join(dir_name);
                        if dir.is_dir() {
                            let _ = std::fs::remove_dir_all(&dir);
                            let _ = std::fs::create_dir_all(&dir);
                        }
                    }

                    // 4. 销毁 Docker 容器
                    let container_name = format!("tyclaw-{workspace_key}");
                    let _ = tokio::process::Command::new("docker")
                        .args(["rm", "-f", &container_name])
                        .output()
                        .await;

                    // 5. 清除 prompt cache（避免旧 tool_call 残留导致 400）
                    let cache_scope = format!("session:{workspace_key}");
                    orch.provider.clear_cache_scope(&cache_scope);

                    // 6. 清理 per-workspace 串行锁
                    orch.injection_queues.lock().remove(&workspace_key);

                    // 6b. 清理 pending_ask_user 避免无限制内存增长
                    orch.pending_ask_user.lock().remove(&workspace_key);

                    // 7. 写审计日志
                    let session_id = "reaper".to_string();
                    let _ = orch.persistence.audit.log(&AuditEntry {
                        timestamp: chrono::Utc::now(),
                        workspace_key: workspace_key.clone(),
                        session_id,
                        user_id: "system".into(),
                        user_name: "system".into(),
                        channel: "reaper".into(),
                        request: "workspace idle timeout".into(),
                        tool_calls: vec![],
                        skills_used: vec![],
                        final_response: Some("consolidated and cleaned".into()),
                        total_duration: None,
                        token_usage: None,
                    });

                    info!(
                        workspace_key = %workspace_key,
                        "Workspace reaped successfully"
                    );
                }
            }
        });
    }
}

/// 检查目录下是否有最近 `threshold_secs` 秒内修改的文件。
/// 用于 reaper 判断子 agent 是否仍在活跃执行。
fn has_recent_file_activity(dir: &std::path::Path, threshold_secs: u64) -> bool {
    let now = std::time::SystemTime::now();
    let threshold = std::time::Duration::from_secs(threshold_secs);

    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                if let Ok(age) = now.duration_since(modified) {
                    if age < threshold {
                        return true;
                    }
                }
            }
        }
    }
    false
}
