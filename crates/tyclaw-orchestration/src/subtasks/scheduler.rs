use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use tracing::{info, warn};

use super::executor::NodeExecutor;
use super::protocol::{ExecutionRecord, FailurePolicy, NodeStatus, TaskNode, TaskPlan};

/// 默认并发上限。
const DEFAULT_MAX_CONCURRENCY: usize = 4;
/// 默认节点超时（ms）。
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// DAG 调度器：按拓扑序调度 ready 节点并行执行。
pub struct DagScheduler {
    executor: Arc<NodeExecutor>,
    max_concurrency: usize,
    default_timeout_ms: u64,
}

impl DagScheduler {
    pub fn new(
        executor: Arc<NodeExecutor>,
        max_concurrency: Option<usize>,
        default_timeout_ms: Option<u64>,
    ) -> Self {
        Self {
            executor,
            max_concurrency: max_concurrency.unwrap_or(DEFAULT_MAX_CONCURRENCY),
            default_timeout_ms: default_timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
        }
    }

    /// 获取底层 NodeExecutor 的引用（用于单任务短路优化）。
    pub fn executor(&self) -> &NodeExecutor {
        &self.executor
    }

    /// 执行整个 TaskPlan，返回所有节点的 ExecutionRecord。
    pub async fn execute(
        &self,
        plan: &TaskPlan,
        dispatch_dir: &std::path::Path,
        main_context: Option<&str>,
    ) -> Vec<ExecutionRecord> {
        let node_map: HashMap<&str, &TaskNode> =
            plan.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

        // 构建依赖图：node_id → 上游依赖集合
        let mut deps: HashMap<String, HashSet<String>> = HashMap::new();
        // 构建下游图：node_id → 依赖它的节点
        let mut downstream: HashMap<String, Vec<String>> = HashMap::new();

        for node in &plan.nodes {
            deps.entry(node.id.clone()).or_default();
            downstream.entry(node.id.clone()).or_default();
        }
        for (from, to) in &plan.edges {
            deps.entry(to.clone()).or_default().insert(from.clone());
            downstream.entry(from.clone()).or_default().push(to.clone());
        }
        for node in &plan.nodes {
            for dep in &node.dependencies {
                deps.entry(node.id.clone()).or_default().insert(dep.clone());
                downstream
                    .entry(dep.clone())
                    .or_default()
                    .push(node.id.clone());
            }
        }

        let records: Arc<Mutex<Vec<ExecutionRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let outputs: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let statuses: Arc<Mutex<HashMap<String, NodeStatus>>> = Arc::new(Mutex::new(
            plan.nodes
                .iter()
                .map(|n| (n.id.clone(), NodeStatus::Pending))
                .collect(),
        ));

        let semaphore = Arc::new(Semaphore::new(self.max_concurrency));

        // 维护待调度节点
        let pending: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(
            plan.nodes.iter().map(|n| n.id.clone()).collect(),
        ));

        let mut join_set: JoinSet<(String, ExecutionRecord)> = JoinSet::new();

        loop {
            // 检查是否全部完成
            {
                let p = pending.lock().await;
                let st = statuses.lock().await;
                let active = st.values().any(|s| *s == NodeStatus::Running);
                if p.is_empty() && !active {
                    break;
                }
            }

            // 找出 ready 节点（注意：所有 Mutex 必须在同一个 block 内获取和释放，避免死锁）
            let (ready_nodes, skipped_nodes) = {
                let mut p = pending.lock().await;
                let st = statuses.lock().await;

                // FailFast：任一已失败 → 标记剩余为 Skipped
                if plan.failure_policy == FailurePolicy::FailFast
                    && st.values().any(|s| *s == NodeStatus::Failed)
                {
                    let skipped: Vec<String> = p.drain().collect();
                    drop(st);
                    drop(p);
                    let mut st = statuses.lock().await;
                    let mut rec = records.lock().await;
                    for id in skipped {
                        st.insert(id.clone(), NodeStatus::Skipped);
                        rec.push(ExecutionRecord {
                            node_id: id,
                            model: String::new(),
                            input_tokens: 0,
                            output_tokens: 0,
                            duration_ms: 0,
                            status: NodeStatus::Skipped,
                            output: None,
                            error: Some("skipped due to fail-fast".into()),
                            retries: 0,
                            messages: None,
                            tools_used: Vec::new(),
                            tool_events: Vec::new(),
                            decision_events: Vec::new(),
                            diagnostics_summary: None,
                        });
                    }
                    break;
                }

                let mut ready = Vec::new();
                let mut to_remove = Vec::new();
                let mut skipped = Vec::new();

                for id in p.iter() {
                    let node_deps = deps.get(id.as_str()).cloned().unwrap_or_default();
                    let all_deps_done = node_deps
                        .iter()
                        .all(|d| matches!(st.get(d.as_str()), Some(NodeStatus::Success)));
                    let any_dep_failed = node_deps.iter().any(|d| {
                        matches!(
                            st.get(d.as_str()),
                            Some(NodeStatus::Failed) | Some(NodeStatus::Skipped)
                        )
                    });

                    if any_dep_failed && plan.failure_policy == FailurePolicy::BestEffort {
                        to_remove.push(id.clone());
                        skipped.push(id.clone());
                        continue;
                    }

                    if all_deps_done {
                        if let Some(node) = node_map.get(id.as_str()) {
                            ready.push((*node).clone());
                            to_remove.push(id.clone());
                        }
                    }
                }

                for id in &to_remove {
                    p.remove(id);
                }

                // 释放 st 和 p 锁后再处理 skipped
                (ready, skipped)
            };

            // 在锁释放后记录跳过的节点
            if !skipped_nodes.is_empty() {
                let mut st = statuses.lock().await;
                let mut rec = records.lock().await;
                for id in &skipped_nodes {
                    st.insert(id.clone(), NodeStatus::Skipped);
                    rec.push(ExecutionRecord {
                        node_id: id.clone(),
                        model: String::new(),
                        input_tokens: 0,
                        output_tokens: 0,
                        duration_ms: 0,
                        status: NodeStatus::Skipped,
                        output: None,
                        error: Some("skipped: upstream dependency failed".into()),
                        retries: 0,
                        messages: None,
                        tools_used: Vec::new(),
                        tool_events: Vec::new(),
                        decision_events: Vec::new(),
                        diagnostics_summary: None,
                    });
                }
            }

            if ready_nodes.is_empty() {
                // 等待某个正在运行的任务完成
                if let Some(result) = join_set.join_next().await {
                    if let Ok((node_id, record)) = result {
                        let mut st = statuses.lock().await;
                        st.insert(node_id.clone(), record.status);
                        if record.status == NodeStatus::Success {
                            if let Some(ref out) = record.output {
                                outputs.lock().await.insert(node_id.clone(), out.clone());
                            }
                        }
                        records.lock().await.push(record);
                    }
                } else {
                    // JoinSet 为空且无 ready 节点 → 可能是死锁或全部完成
                    break;
                }
                continue;
            }

            // 为 ready 节点启动并发执行
            tracing::debug!(count = ready_nodes.len(), "Scheduler spawning ready nodes");
            for node in ready_nodes {
                let executor = Arc::clone(&self.executor);
                let sem = Arc::clone(&semaphore);
                let outputs_ref = Arc::clone(&outputs);
                let statuses_ref = Arc::clone(&statuses);
                let timeout =
                    Duration::from_millis(node.timeout_ms.unwrap_or(self.default_timeout_ms));
                let node_id = node.id.clone();
                let node_deps = node.dependencies.clone();

                {
                    let mut st = statuses_ref.lock().await;
                    st.insert(node_id.clone(), NodeStatus::Running);
                }

                let dispatch_dir_owned = dispatch_dir.to_path_buf();
                let main_ctx_owned = main_context.map(|s| s.to_string());
                // 捕获当前 sandbox scope（task_local 不跨 spawn 传递，需要手动传）
                let sandbox_for_spawn = tyclaw_sandbox::current_sandbox();
                tracing::debug!(node_id = %node_id, "Scheduler spawning task");
                join_set.spawn(async move {
                    // 在 spawned task 中重建 sandbox scope
                    let inner = async {
                    tracing::debug!(node_id = %node_id, "Task started, acquiring semaphore");
                    let _permit = sem.acquire().await.expect("semaphore closed");
                    tracing::debug!(node_id = %node_id, "Semaphore acquired, executing");

                    // 收集上游输出
                    let upstream: Vec<(String, String)> = {
                        let out = outputs_ref.lock().await;
                        node_deps
                            .iter()
                            .filter_map(|d| out.get(d).map(|o| (d.clone(), o.clone())))
                            .collect()
                    };

                    let record = match tokio::time::timeout(timeout, executor.execute(&node, &upstream, &dispatch_dir_owned, main_ctx_owned.as_deref())).await {
                        Ok(rec) => rec,
                        Err(_) => {
                            warn!(node_id = %node.id, timeout_ms = timeout.as_millis(), "Node timed out");
                            ExecutionRecord {
                                node_id: node.id.clone(),
                                model: String::new(),
                                input_tokens: 0,
                                output_tokens: 0,
                                duration_ms: timeout.as_millis() as u64,
                                status: NodeStatus::Failed,
                                output: None,
                                error: Some("timeout".into()),
                                retries: 0,
                                messages: None,
                                tools_used: Vec::new(),
                                tool_events: Vec::new(),
                                decision_events: Vec::new(),
                                diagnostics_summary: None,
                            }
                        }
                    };

                    (node_id, record)
                    }; // end inner async
                    if let Some(sb) = sandbox_for_spawn {
                        tyclaw_sandbox::CURRENT_SANDBOX.scope(sb, inner).await
                    } else {
                        inner.await
                    }
                });
            }

            // 收割已完成的任务
            while let Some(result) = join_set.try_join_next() {
                if let Ok((node_id, record)) = result {
                    let mut st = statuses.lock().await;
                    st.insert(node_id.clone(), record.status);
                    if record.status == NodeStatus::Success {
                        if let Some(ref out) = record.output {
                            outputs.lock().await.insert(node_id.clone(), out.clone());
                        }
                    }
                    records.lock().await.push(record);
                }
            }
        }

        // 收割所有剩余任务
        while let Some(result) = join_set.join_next().await {
            if let Ok((node_id, record)) = result {
                let mut st = statuses.lock().await;
                st.insert(node_id.clone(), record.status);
                if record.status == NodeStatus::Success {
                    if let Some(ref out) = record.output {
                        outputs.lock().await.insert(node_id.clone(), out.clone());
                    }
                }
                records.lock().await.push(record);
            }
        }

        let result = records.lock().await;
        info!(total_nodes = result.len(), "DAG execution completed");
        result.clone()
    }
}
