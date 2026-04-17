//! 编排器核心类型与常量。

/// 工具结果最大截断长度。
pub(crate) const TOOL_RESULT_MAX_CHARS: usize = 2000;
/// `/save` 交接文件最多保留的历史消息条数（取最近 N 条）。
pub(crate) const HANDOFF_MAX_MESSAGES: usize = 120;
/// `/save` 交接文件中单条消息最大字符数。
pub(crate) const HANDOFF_MAX_CONTENT_CHARS: usize = 2000;
/// 注入到系统提示中的技能正文上限（降低 token 噪声）。
pub(crate) const MAX_INJECTED_SKILLS: usize = 8;
/// 相似案例提示最大字符数（超出截断）。
pub(crate) const MAX_SIMILAR_CASES_CHARS: usize = 2500;
/// 历史消息在上下文窗口中的预算比例。
pub(crate) const HISTORY_BUDGET_RATIO: usize = 45;
/// 历史消息绝对 Token 上限（防止在大 Context Window 下历史无限制膨胀）。
/// 16K tokens ≈ 50-80 条消息，保留最近 2-3 轮完整上下文，
/// 避免大量旧 tool results 稀释当前用户意图。
pub(crate) const MAX_HISTORY_TOKENS_HARD_LIMIT: usize = 16_384;
/// 历史消息最小 token 预算。
pub(crate) const MIN_HISTORY_BUDGET_TOKENS: usize = 256;
/// 动态预算下，历史消息最大预算比例。
pub(crate) const MAX_HISTORY_BUDGET_RATIO: usize = 65;
/// 动态预算下，技能注入上限。
pub(crate) const MAX_DYNAMIC_INJECTED_SKILLS: usize = 12;
/// 动态预算下，相似案例最大字符数。
pub(crate) const MAX_DYNAMIC_SIMILAR_CASES_CHARS: usize = 4000;
/// 首轮额外注入技能数（在动态预算基础上增加，随后 clamp）。
pub(crate) const FIRST_TURN_SKILL_BONUS: usize = 2;
/// 首轮相似案例字符预算增量（在动态预算基础上增加，随后 clamp）。
pub(crate) const FIRST_TURN_CASES_CHARS_BONUS: usize = 1200;
/// 注入给 AgentLoop 的一次性轮次重置标记字段。
pub(crate) const RESET_ON_START_FIELD: &str = "_reset_iterations_next_run";

/// 请求上下文（用于描述一次上层调用的身份与会话信息）。
#[derive(Debug, Clone)]
pub struct RequestContext {
    pub user_id: String,
    pub user_name: String,
    pub workspace_id: String,
    pub channel: String,
    pub chat_id: String,
    /// 钉钉 conversation_id（群聊时为群 ID，私聊时可为空）。
    /// 用于 WorkspaceKeyStrategy::Conversation 模式。
    pub conversation_id: Option<String>,
    /// 用户发送的图片 data URI 列表（`data:image/...;base64,...`）。
    /// 非空时构建多模态消息。
    pub image_data_uris: Vec<String>,
    /// 用户发送的文件附件列表（本地路径, 原始文件名）。
    /// 非空时将文件信息附加到用户消息中。
    pub file_attachments: Vec<(String, String)>,
}

impl Default for RequestContext {
    fn default() -> Self {
        Self {
            user_id: "user".into(),
            user_name: String::new(),
            workspace_id: "default".into(),
            channel: "api".into(),
            chat_id: "direct".into(),
            conversation_id: None,
            image_data_uris: Vec::new(),
            file_attachments: Vec::new(),
        }
    }
}

impl RequestContext {
    pub fn new(
        user_id: impl Into<String>,
        workspace_id: impl Into<String>,
        channel: impl Into<String>,
        chat_id: impl Into<String>,
    ) -> Self {
        Self {
            user_id: user_id.into(),
            user_name: String::new(),
            workspace_id: workspace_id.into(),
            channel: channel.into(),
            chat_id: chat_id.into(),
            conversation_id: None,
            image_data_uris: Vec::new(),
            file_attachments: Vec::new(),
        }
    }

    /// 设置用户名（builder 风格）。
    pub fn with_user_name(mut self, name: impl Into<String>) -> Self {
        self.user_name = name.into();
        self
    }

    /// 设置 conversation_id（builder 风格）。
    pub fn with_conversation_id(mut self, cid: impl Into<String>) -> Self {
        self.conversation_id = Some(cid.into());
        self
    }

    /// 附加图片 data URI 列表（builder 风格）。
    pub fn with_images(mut self, images: Vec<String>) -> Self {
        self.image_data_uris = images;
        self
    }

    /// 附加文件附件列表（builder 风格）。
    /// 每个元素为 (本地路径, 原始文件名)。
    pub fn with_files(mut self, files: Vec<(String, String)>) -> Self {
        self.file_attachments = files;
        self
    }
}

/// 编排功能开关：用于 SDK 化时按需裁剪能力。
#[derive(Debug, Clone)]
pub struct OrchestratorFeatures {
    pub enable_audit: bool,
    pub enable_memory: bool,
    pub enable_rbac: bool,
    pub enable_rate_limit: bool,
}

impl Default for OrchestratorFeatures {
    fn default() -> Self {
        Self {
            enable_audit: true,
            enable_memory: true,
            enable_rbac: true,
            enable_rate_limit: true,
        }
    }
}

/// 编排器的响应结构。
#[derive(Debug, Clone)]
pub struct AgentResponse {
    pub text: String,
    pub tools_used: Vec<String>,
    pub duration_seconds: f64,
    /// 累计 prompt tokens
    pub prompt_tokens: u64,
    /// 累计 completion tokens
    pub completion_tokens: u64,
    /// Agent 期望发送给用户的文件路径列表。
    /// 由 `send_file` 工具写入，上层（如 DingTalk bot）负责实际发送。
    pub output_files: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ContextBudgetPlan {
    /// 历史消息预算占 context_window 的百分比。
    pub(crate) history_ratio: usize,
    /// 注入 system prompt 的技能正文数量上限。
    pub(crate) max_skills: usize,
    /// 相似案例段最大字符数，超过后会截断。
    pub(crate) max_cases_chars: usize,
}
