//! AgentRuntime trait —— Agent 执行引擎的统一抽象接口。
//!
//! 通过 trait 抽象，可以支持不同的执行引擎实现（如 ReAct、CoT 等），
//! 而上层代码（Orchestrator）无需关心具体实现。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use tyclaw_types::TyclawError;

/// Per-workspace 消息注入队列：外部可在 agent loop 运行期间追加用户消息。
pub type InjectionQueue = Arc<StdMutex<Vec<HashMap<String, Value>>>>;

/// 心跳发送器：子任务可通过此将消息转发到钉钉等通道。
pub type HeartbeatSender = Arc<dyn Fn(String) + Send + Sync>;

tokio::task_local! {
    /// Agent loop 运行期间，orchestrator 可通过此 task_local 注入用户消息。
    pub static INJECTION_QUEUE: InjectionQueue;
    /// 心跳消息发送器：子任务通过此转发心跳到消息总线。
    pub static HEARTBEAT_TX: HeartbeatSender;
    /// 取消令牌：外部（钉钉停止按钮 / 关键字 / 命令行中断）可通过它请求终止当前
    /// 运行中的 agent loop。AgentLoop 在每轮开始前、以及每批工具调用后 check，
    /// 检测到已取消则短路退出，返回一条"任务已终止"的回复。
    pub static CANCEL_TOKEN: tokio_util::sync::CancellationToken;
}

/// 快捷查询：当前 task_local 作用域内的取消令牌是否已触发。
///
/// 未设置令牌时（无外部取消通道）返回 `false`。供 AgentLoop 无条件轮询。
pub fn is_cancel_requested() -> bool {
    CANCEL_TOKEN
        .try_with(|t| t.is_cancelled())
        .unwrap_or(false)
}

/// Agent 运行时的完成状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeStatus {
    /// 任务完成，content 是最终回复。
    Complete,
    /// Agent 需要用户输入才能继续，content 是要问用户的问题。
    /// `pending_tool_call_id` 保存 ask_user 工具调用的 ID，
    /// 用户回复后需要将其作为 tool result 注入消息历史。
    NeedsInput { pending_tool_call_id: String },
}

impl Default for RuntimeStatus {
    fn default() -> Self {
        RuntimeStatus::Complete
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolExecutionEvent {
    pub tool_id: String,
    pub tool_name: String,
    pub route: String,
    pub status: String,
    pub duration_ms: u64,
    pub original_len: usize,
    pub result_len: usize,
    pub truncated: bool,
    pub gate_action: String,
    pub risk_level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_id: Option<String>,
    pub result_preview: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DecisionEvent {
    pub iteration: usize,
    pub agent_scope: String,
    pub decision: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_origin_iteration: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispatch_statuses: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_tools: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunDiagnosticsSummary {
    pub total_tool_calls: usize,
    pub exec_count: usize,
    pub dedicated_tool_count: usize,
    pub error_tool_count: usize,
    pub denied_tool_count: usize,
    pub sandbox_tool_count: usize,
    pub host_tool_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_after_last_edit: Option<bool>,
    pub ended_with_unverified_changes: bool,
}

/// Agent 运行时执行结果。
///
/// - `content`: 最终回复文本（可能为 None，如果只执行了工具操作）
/// - `tools_used`: 本次执行过程中调用的所有工具名称列表
/// - `messages`: 完整的对话历史（包含所有中间步骤的消息）
/// - `status`: 运行时状态（Complete 或 NeedsInput）
#[derive(Debug, Clone, Default)]
pub struct RuntimeResult {
    pub content: Option<String>,
    pub tools_used: Vec<String>,
    pub messages: Vec<HashMap<String, Value>>,
    pub status: RuntimeStatus,
    pub tool_events: Vec<ToolExecutionEvent>,
    pub decision_events: Vec<DecisionEvent>,
    pub diagnostics_summary: RunDiagnosticsSummary,
    /// 累计 prompt cache 命中 tokens
    pub cache_hit_tokens: u64,
    /// 累计 prompt cache 写入 tokens
    pub cache_write_tokens: u64,
    /// 累计 prompt tokens（总量）
    pub total_prompt_tokens: u64,
    /// 累计 completion tokens
    pub total_completion_tokens: u64,
    /// 本轮唯一标识。agent_loop 给所有新增消息打上此 `_turn_id`，
    /// save_turn 据此精确筛选本轮消息，不受前缀压缩/标记消费的影响。
    pub turn_id: String,
}

/// 进度事件——agent loop / handler 向外部渠道（CLI、钉钉卡片等）汇报的消息载荷。
///
/// 原来用 `&str` 走一个通道并在前缀里编码类型（`[Thinking]` / `[heartbeat]` / ...），
/// 分拣只能靠 string matching。类型化之后下游可以按需分流，比如钉钉 AI 卡片
/// 只需要 `Thinking` / `ToolStart` 两种就能更新思考行和工具行。
#[derive(Debug, Clone)]
pub enum ProgressEvent {
    /// LLM 推理增量（原 `[Thinking]\n...`）
    Thinking(String),
    /// LLM 正文 content 片段（非 reasoning）
    Content(String),
    /// 系统状态——轮次、token、sandbox、记忆整理等辅助信息
    Status(String),
    /// 长任务心跳提醒（钉钉走 emotion 接口，CLI 直接打印）
    Heartbeat(String),
    /// 工具**开始**调用——带上给人看的简短描述，用于卡片工具行
    ToolStart { name: String, brief: String },
    /// 工具执行完毕的摘要（原 `  → tool_name (...chars, status, route, ms)`）
    ToolResult(String),
}

impl ProgressEvent {
    /// 兼容旧 CLI 输出：把事件格式化成带前缀的字符串，
    /// 让 bus 的 OutboundEvent::Progress 保持与改造前一致的显示效果。
    pub fn legacy_text(&self) -> String {
        match self {
            ProgressEvent::Thinking(s) => format!("[Thinking]\n{s}"),
            ProgressEvent::Content(s) | ProgressEvent::Status(s) | ProgressEvent::ToolResult(s) => {
                s.clone()
            }
            ProgressEvent::Heartbeat(s) => {
                if s.starts_with("[heartbeat]") {
                    s.clone()
                } else {
                    format!("[heartbeat]{s}")
                }
            }
            ProgressEvent::ToolStart { name, brief } => {
                if brief.is_empty() {
                    format!("  ▸ {name}")
                } else {
                    format!("  ▸ {name}: {brief}")
                }
            }
        }
    }
}

/// 进度回调函数类型。
///
/// 用于在 Agent 执行过程中向外部报告进展。
/// 返回一个 Pin<Box<Future>>，支持异步回调。
pub type OnProgress = Box<
    dyn Fn(ProgressEvent) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// 构造一条 `{role, content}` 格式的聊天消息。
///
/// 主 agent 和 sub-agent 的消息构造都用这个函数，避免散落的 HashMap 手写。
pub fn chat_message(role: &str, content: &str) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    m.insert("role".into(), Value::String(role.into()));
    m.insert("content".into(), Value::String(content.into()));
    m
}

/// 解析 OnProgress 消息中的 `[Thinking]` 前缀。
///
/// 返回 `(is_thinking, content)` — 剥离前缀后的文本。
/// 主 agent 回调和 sub-agent 回调共用此逻辑。
pub fn parse_thinking_prefix(msg: &str) -> (bool, &str) {
    if let Some(content) = msg.strip_prefix("[Thinking]\n") {
        (true, content)
    } else {
        (false, msg)
    }
}

/// Agent 执行引擎必须满足的接口协议。
///
/// `Send + Sync` 约束确保可以在异步多线程环境中安全使用。
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    /// 执行 Agent 运行时。
    ///
    /// 参数：
    /// - `initial_messages`: 初始消息列表（包含系统提示和用户消息）
    /// - `user_role`: 当前用户的角色（影响工具调用的权限判定）
    /// - `on_progress`: 可选的进度回调函数
    ///
    /// 返回：RuntimeResult 或错误
    async fn run(
        &self,
        initial_messages: Vec<HashMap<String, Value>>,
        user_role: &str,
        cache_scope: Option<&str>,
        on_progress: Option<&OnProgress>,
    ) -> Result<RuntimeResult, TyclawError>;
}
