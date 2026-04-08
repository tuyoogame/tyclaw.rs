use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tyclaw_agent::runtime::{DecisionEvent, RunDiagnosticsSummary, ToolExecutionEvent};
use tyclaw_types::TyclawError;

// ── TaskPlan ─────────────────────────────────────────────

/// Planner 输出的任务 DAG。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    pub id: String,
    pub nodes: Vec<TaskNode>,
    /// 依赖边 `(from, to)`：`from` 完成后 `to` 才可执行。
    pub edges: Vec<(String, String)>,
    #[serde(default)]
    pub failure_policy: FailurePolicy,
    #[serde(default)]
    pub metadata: PlanMetadata,
}

/// DAG 中的单个任务节点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskNode {
    pub id: String,
    /// 任务类型标签，用于路由匹配（如 "coding", "reasoning", "search"）。
    pub node_type: String,
    pub prompt: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub model_override: Option<String>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub acceptance_criteria: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    #[default]
    FailFast,
    BestEffort,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanMetadata {
    #[serde(default)]
    pub source_prompt: String,
    #[serde(default)]
    pub planner_model: String,
}

// ── NodeStatus & ExecutionRecord ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,
    Running,
    Success,
    Failed,
    Skipped,
}

/// 单节点执行记录，供审计和归并使用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub node_id: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration_ms: u64,
    pub status: NodeStatus,
    pub output: Option<String>,
    pub error: Option<String>,
    pub retries: u32,
    /// 子 agent 的完整消息历史（仅 snapshot 开启时记录）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<HashMap<String, serde_json::Value>>>,
    /// 子 agent 使用的工具列表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_used: Vec<String>,
    /// 子 agent 的结构化工具事件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_events: Vec<ToolExecutionEvent>,
    /// 子 agent 的关键决策事件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_events: Vec<DecisionEvent>,
    /// 子 agent 的工具诊断摘要。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics_summary: Option<RunDiagnosticsSummary>,
    /// 子 agent 使用的 skill 列表（从 messages 中提取）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills_used: Vec<serde_json::Value>,
}

/// Reducer 输出的归并报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeReport {
    pub final_text: String,
    pub records: Vec<ExecutionRecord>,
    pub has_conflicts: bool,
    pub partial_failure: bool,
}

// ── Validation ───────────────────────────────────────────

impl TaskPlan {
    /// 校验计划的完整性：节点 ID 唯一、边引用合法、无环。
    pub fn validate(&self) -> Result<(), TyclawError> {
        if self.nodes.is_empty() {
            return Err(TyclawError::Other("TaskPlan has no nodes".into()));
        }

        let mut ids = HashSet::new();
        for node in &self.nodes {
            if !ids.insert(&node.id) {
                return Err(TyclawError::Other(format!(
                    "duplicate node id: {}",
                    node.id
                )));
            }
        }

        for (from, to) in &self.edges {
            if !ids.contains(from) {
                return Err(TyclawError::Other(format!(
                    "edge references unknown node: {from}"
                )));
            }
            if !ids.contains(to) {
                return Err(TyclawError::Other(format!(
                    "edge references unknown node: {to}"
                )));
            }
        }

        // 同时校验 dependencies 字段引用的节点
        for node in &self.nodes {
            for dep in &node.dependencies {
                if !ids.contains(dep) {
                    return Err(TyclawError::Other(format!(
                        "node {} references unknown dependency: {dep}",
                        node.id
                    )));
                }
            }
        }

        self.check_cycle()?;
        Ok(())
    }

    /// Kahn 算法检测环。
    fn check_cycle(&self) -> Result<(), TyclawError> {
        let ids: Vec<&str> = self.nodes.iter().map(|n| n.id.as_str()).collect();
        let idx: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let n = ids.len();

        let mut in_degree = vec![0usize; n];
        let mut adj: Vec<Vec<usize>> = vec![vec![]; n];

        for (from, to) in &self.edges {
            let fi = idx[from.as_str()];
            let ti = idx[to.as_str()];
            adj[fi].push(ti);
            in_degree[ti] += 1;
        }
        // dependencies 也是边 (dep -> node)
        for node in &self.nodes {
            let ti = idx[node.id.as_str()];
            for dep in &node.dependencies {
                let fi = idx[dep.as_str()];
                adj[fi].push(ti);
                in_degree[ti] += 1;
            }
        }

        let mut queue: Vec<usize> = in_degree
            .iter()
            .enumerate()
            .filter(|(_, &d)| d == 0)
            .map(|(i, _)| i)
            .collect();
        let mut visited = 0usize;

        while let Some(u) = queue.pop() {
            visited += 1;
            for &v in &adj[u] {
                in_degree[v] -= 1;
                if in_degree[v] == 0 {
                    queue.push(v);
                }
            }
        }

        if visited != n {
            return Err(TyclawError::Other("TaskPlan contains a cycle".into()));
        }
        Ok(())
    }

    /// 构建单节点退化计划（Planner 解析失败时的回退）。
    pub fn single_node_fallback(prompt: String) -> Self {
        Self {
            id: uuid_v4(),
            nodes: vec![TaskNode {
                id: "fallback".into(),
                node_type: "general".into(),
                prompt,
                dependencies: vec![],
                model_override: None,
                timeout_ms: None,
                max_retries: None,
                acceptance_criteria: None,
            }],
            edges: vec![],
            failure_policy: FailurePolicy::FailFast,
            metadata: PlanMetadata::default(),
        }
    }
}

fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("plan-{:x}", ts)
}
