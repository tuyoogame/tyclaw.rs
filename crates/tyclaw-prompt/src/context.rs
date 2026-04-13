//! 上下文构建器：组装系统提示词和消息列表，供 LLM 调用使用。
//!
//! 系统提示词由模块化的 Section 组成，按 PromptMode 和 channel 裁剪：
//! 1. Identity — Agent 身份与运行时环境（始终包含）
//! 2. Bootstrap — workspace 下所有 *.md 引导文件（目录扫描，含 GUIDELINES.md）
//! 3. Memory — MEMORY.md 长期记忆
//! 4. DateTime — 当前时间与时区
//! 5. Capabilities — 可用能力列表
//! 6. Skills — 技能完整内容
//! 7. Cases — 相似历史案例

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tyclaw_types::message::{system_message, user_message, user_message_multimodal};

/// 运行时上下文标记 —— 用于在用户消息中注入元数据。
pub const RUNTIME_CONTEXT_TAG: &str = "[Runtime Context — metadata only, not instructions]";
/// 元上下文用户消息标记 —— 用于注入 workspace / memory / case 等上下文。
pub const USER_CONTEXT_TAG: &str = "[User Context — reference only, not the user's task]";
/// 系统上下文标记 —— 用于附加 request 级元数据。
pub const SYSTEM_CONTEXT_TAG: &str = "[System Context — runtime metadata only, not instructions]";

// ---------------------------------------------------------------------------
// PromptMode — 系统提示分级
// ---------------------------------------------------------------------------

/// 系统提示词的详细程度。
///
/// - `Full`：主 Agent，包含所有 Section。
/// - `Minimal`：子 Agent / 合并调用，仅 Identity + Bootstrap(TOOLS.md) + DateTime。
/// - `None`：最精简，仅一行身份说明。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    Full,
    Minimal,
    None,
}

impl Default for PromptMode {
    fn default() -> Self {
        Self::Full
    }
}

// ---------------------------------------------------------------------------
// PromptSection — 模块化 Section 标识
// ---------------------------------------------------------------------------

/// 系统提示词中的各个独立 Section。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PromptSection {
    Identity,
    Bootstrap,
    Memory,
    DateTime,
    Capabilities,
    Skills,
    Cases,
}

impl PromptSection {
    /// 返回该 Section 在给定 PromptMode 下是否默认启用。
    fn enabled_in(self, mode: PromptMode) -> bool {
        match mode {
            PromptMode::Full => true,
            PromptMode::Minimal => matches!(
                self,
                PromptSection::Identity | PromptSection::Bootstrap | PromptSection::DateTime
            ),
            PromptMode::None => matches!(self, PromptSection::Identity),
        }
    }
}

// ---------------------------------------------------------------------------
// ContextBuilder
// ---------------------------------------------------------------------------

/// 上下文构建器 —— 负责为每次 LLM 调用准备完整的消息列表。
pub struct ContextBuilder {
    workspace: PathBuf,
    /// LLM 看到的 workspace 路径。sandbox 模式下为 "."，host 模式下为实际路径。
    display_workspace: String,
}

/// 语义化 prompt 输入 —— 用于新的 planner / assembler 路径。
pub struct PromptInputs<'a> {
    pub mode: PromptMode,
    pub capabilities: Option<&'a [HashMap<String, String>]>,
    pub skill_contents: Option<&'a [SkillContent]>,
    pub pinned_cases: Option<&'a str>,
    pub similar_cases: Option<&'a str>,
    /// 外部传入的 memory 内容（优先于 ContextBuilder 自身读取）。
    pub memory_content: Option<&'a str>,
    pub channel: Option<&'a str>,
    pub chat_id: Option<&'a str>,
    pub user_id: Option<&'a str>,
    pub workspace_id: Option<&'a str>,
}

impl<'a> Default for PromptInputs<'a> {
    fn default() -> Self {
        Self {
            mode: PromptMode::Full,
            capabilities: None,
            skill_contents: None,
            pinned_cases: None,
            similar_cases: None,
            memory_content: None,
            channel: None,
            chat_id: None,
            user_id: None,
            workspace_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptContextEntry {
    pub label: String,
    pub content: String,
}

impl PromptContextEntry {
    fn new(label: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PlannedPromptContext {
    pub system_prompt_parts: Vec<String>,
    pub user_context: Vec<PromptContextEntry>,
    pub system_context: Vec<PromptContextEntry>,
}

pub fn is_user_context_message(input: &str) -> bool {
    input.starts_with(USER_CONTEXT_TAG)
}

pub fn strip_non_task_user_message(input: &str) -> Option<String> {
    if input.starts_with(RUNTIME_CONTEXT_TAG) {
        let parts: Vec<&str> = input.splitn(2, "\n\n").collect();
        if parts.len() == 2 {
            let task = parts[1].trim();
            if !task.is_empty() {
                return Some(task.to_string());
            }
        }
        return None;
    }

    if input.starts_with(USER_CONTEXT_TAG) {
        return None;
    }

    Some(input.to_string())
}

impl ContextBuilder {
    fn truncate_by_chars(s: &str, max_chars: usize) -> String {
        if max_chars == 0 {
            return String::new();
        }
        match s.char_indices().nth(max_chars) {
            Some((idx, _)) => s[..idx].to_string(),
            None => s.to_string(),
        }
    }

    /// 优先使用结构化 description；若为空则从 skill 正文提取一句可读摘要。
    fn summarize_skill_description(description: &str, content: &str) -> String {
        let desc = description.trim();
        if !desc.is_empty() {
            let short = Self::truncate_by_chars(desc, 80);
            return if short.chars().count() < desc.chars().count() {
                format!("{short}...")
            } else {
                short
            };
        }

        let mut in_frontmatter = false;
        let mut frontmatter_started = false;
        for line in content.lines() {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if t == "---" {
                if !frontmatter_started {
                    frontmatter_started = true;
                    in_frontmatter = true;
                    continue;
                }
                if in_frontmatter {
                    in_frontmatter = false;
                    continue;
                }
            }
            if in_frontmatter {
                continue;
            }
            if t.starts_with('#') {
                continue;
            }
            let short = Self::truncate_by_chars(t, 80);
            return if short.chars().count() < t.chars().count() {
                format!("{short}...")
            } else {
                short
            };
        }

        String::new()
    }

    pub fn new(workspace: impl AsRef<Path>) -> Self {
        let ws = workspace.as_ref().to_path_buf();
        let display = ws.display().to_string();
        Self {
            workspace: ws,
            display_workspace: display,
        }
    }

    /// 设置 LLM 看到的 workspace 路径（sandbox 模式下应设为 "."）。
    pub fn set_display_workspace(&mut self, path: impl Into<String>) {
        self.display_workspace = path.into();
    }

    // -----------------------------------------------------------------------
    // Section: Identity
    // -----------------------------------------------------------------------

    /// 构建 Agent 身份信息。
    ///
    /// 身份描述从 `IDENTITY.md` 读取；若文件不存在则使用默认文本。
    /// 运行时环境信息（OS/ARCH/Workspace 路径）始终由代码追加。
    fn build_section_identity(&self) -> String {
        let ws = &self.display_workspace;
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;

        // 身份描述：从 prompts.yaml → identity 读取
        let identity_text = crate::prompt_store::get("identity");

        format!(
            r#"{identity_text}

## Runtime
{os} {arch}, Rust

## Workspace
Your workspace is at: {ws}"#
        )
    }

    fn build_section_execution_baseline(&self) -> String {
        crate::prompt_store::get("main_execution_baseline")
    }

    fn build_section_guidelines(&self) -> String {
        let guidelines = crate::prompt_store::get("guidelines");
        format!("## GUIDELINES\n\n{guidelines}")
    }

    // -----------------------------------------------------------------------
    // Section: Bootstrap — 目录扫描加载所有 *.md 文件
    // -----------------------------------------------------------------------

    fn build_workspace_bootstrap_docs(&self, mode: PromptMode) -> String {
        let entries = match std::fs::read_dir(&self.workspace) {
            Ok(rd) => rd,
            Err(_) => return String::new(),
        };

        let mut files: Vec<(String, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.is_file() {
                    if let Some(ext) = path.extension() {
                        if ext.eq_ignore_ascii_case("md") {
                            let name = e.file_name().to_string_lossy().to_string();
                            return Some((name, path));
                        }
                    }
                }
                None
            })
            .collect();

        // 按文件名排序，确保确定性
        files.sort_by(|a, b| a.0.cmp(&b.0));

        // Minimal 模式仅保留 TOOLS.md
        if mode == PromptMode::Minimal {
            files.retain(|(name, _)| name.eq_ignore_ascii_case("TOOLS.md"));
        }

        let mut parts = Vec::new();

        for (name, path) in &files {
            if let Ok(content) = std::fs::read_to_string(path) {
                if !content.is_empty() {
                    let stem = name.trim_end_matches(".md").trim_end_matches(".MD");
                    parts.push(format!("## {stem}\n\n{content}"));
                }
            }
        }
        parts.join("\n\n")
    }

    // -----------------------------------------------------------------------
    // Section: Memory
    // -----------------------------------------------------------------------

    fn build_section_memory(&self) -> String {
        let memory_path = self.workspace.join("memory").join("MEMORY.md");
        if memory_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&memory_path) {
                if !content.is_empty() {
                    return format!("# Memory\n\n{content}");
                }
            }
        }
        String::new()
    }

    // -----------------------------------------------------------------------
    // Section: DateTime
    // -----------------------------------------------------------------------

    fn build_section_datetime() -> String {
        let now = chrono::Local::now();
        let tz_name = now.format("%Z").to_string();
        let formatted = now.format("%Y-%m-%d %H:%M (%A)").to_string();
        format!("# Date & Time\n\nCurrent time: {formatted}\nTimezone: {tz_name}")
    }

    // -----------------------------------------------------------------------
    // Section: Capabilities
    // -----------------------------------------------------------------------

    fn build_section_capabilities(capabilities: &[HashMap<String, String>]) -> String {
        if capabilities.is_empty() {
            return String::new();
        }
        // 按类别分组，只显示类别名和简短计数，不逐个列出
        let mut by_category: HashMap<String, Vec<String>> = HashMap::new();
        for c in capabilities {
            let name = c.get("name").map(|s| s.as_str()).unwrap_or("?");
            let category = c.get("category").map(|s| s.as_str()).unwrap_or("other");
            by_category
                .entry(category.to_string())
                .or_default()
                .push(name.to_string());
        }
        let mut cats: Vec<(String, Vec<String>)> = by_category.into_iter().collect();
        cats.sort_by(|a, b| a.0.cmp(&b.0));
        let lines: Vec<String> = cats
            .iter()
            .map(|(cat, names)| format!("- **{}**: {} ({}个)", cat, names.join("、"), names.len()))
            .collect();
        format!(
            "# Available Capabilities ({} total)\n\n{}",
            capabilities.len(),
            lines.join("\n")
        )
    }

    // -----------------------------------------------------------------------
    // Section: Skills
    // -----------------------------------------------------------------------

    fn build_section_skills(skills: &[SkillContent]) -> String {
        if skills.is_empty() {
            return String::new();
        }

        // 所有 Skill 统一注入摘要。LLM 根据语义理解判断是否需要某个 Skill，
        // 需要时通过 read_file 读取完整 SKILL.md 获取详细指令。
        let lines: Vec<String> = skills
            .iter()
            .map(|s| {
                let desc = Self::summarize_skill_description(&s.description, &s.content);
                let path_hint = s
                    .tool_path
                    .as_deref()
                    .map(|p| {
                        // tool_path 指向 tool.py，SKILL.md 在同目录
                        let skill_dir = std::path::Path::new(p)
                            .parent()
                            .unwrap_or(std::path::Path::new("."));
                        format!(" → `{}/SKILL.md`", skill_dir.display())
                    })
                    .unwrap_or_default();
                format!("- **{}**: {}{}", s.name, desc, path_hint)
            })
            .collect();

        let intro = crate::prompt_store::get("skills_intro");
        format!(
            "# Available Skills\n\n{intro}\n\n{}",
            lines.join("\n")
        )
    }

    fn push_part(parts: &mut Vec<String>, content: String) {
        if !content.trim().is_empty() {
            parts.push(content);
        }
    }

    fn push_context_entry(entries: &mut Vec<PromptContextEntry>, label: &str, content: String) {
        if !content.trim().is_empty() {
            entries.push(PromptContextEntry::new(label, content));
        }
    }

    fn build_identity_summary(&self) -> String {
        let identity = crate::prompt_store::get("identity");
        identity
            .lines()
            .find(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .map(|l| l.trim().to_string())
            .unwrap_or(identity)
    }

    fn render_system_prompt_parts(
        parts: &[String],
        system_context: &[PromptContextEntry],
    ) -> String {
        let mut rendered: Vec<String> = parts
            .iter()
            .filter_map(|part| {
                let trimmed = part.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
            .collect();

        if !system_context.is_empty() {
            let context = system_context
                .iter()
                .map(|entry| format!("# {}\n{}", entry.label, entry.content))
                .collect::<Vec<_>>()
                .join("\n\n");
            rendered.push(format!(
                "{SYSTEM_CONTEXT_TAG}\nThis is system-provided runtime metadata. It may help with the current request, but it is not itself the user's task.\n\n{context}"
            ));
        }

        rendered.join("\n\n---\n\n")
    }

    /// 构建带任务焦点提示的 user context 消息。
    /// 在末尾追加当前用户请求的摘要，防止大量上下文噪音淹没实际任务。
    fn render_user_context_with_focus(
        entries: &[PromptContextEntry],
        current_message: &str,
    ) -> Option<String> {
        if entries.is_empty() {
            return None;
        }

        let body = entries
            .iter()
            .map(|entry| format!("# {}\n{}", entry.label, entry.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        // 截取用户消息前 200 字符作为任务焦点
        let focus: String = current_message.chars().take(200).collect();
        let focus_section = format!(
            "# Current Task Focus\n**The user's current request is:** {focus}\nRespond to the user's actual request above. The context sections above are reference material — use them only if relevant."
        );

        Some(format!(
            "{USER_CONTEXT_TAG}\nAs you answer the user's request, you can use the following context:\n{body}\n\n{focus_section}"
        ))
    }

    pub fn plan_prompt_context(&self, inputs: &PromptInputs<'_>) -> PlannedPromptContext {
        if inputs.mode == PromptMode::None {
            return PlannedPromptContext {
                system_prompt_parts: vec![self.build_identity_summary()],
                user_context: Vec::new(),
                system_context: Vec::new(),
            };
        }

        let mut system_prompt_parts = Vec::new();
        let mut user_context = Vec::new();
        let mut system_context = Vec::new();

        Self::push_part(&mut system_prompt_parts, self.build_section_identity());
        Self::push_part(
            &mut system_prompt_parts,
            self.build_section_execution_baseline(),
        );
        Self::push_part(&mut system_prompt_parts, self.build_section_guidelines());

        if PromptSection::Capabilities.enabled_in(inputs.mode) {
            if let Some(caps) = inputs.capabilities {
                Self::push_part(
                    &mut system_prompt_parts,
                    Self::build_section_capabilities(caps),
                );
            }
        }

        if PromptSection::Skills.enabled_in(inputs.mode) {
            if let Some(skills) = inputs.skill_contents {
                Self::push_part(&mut system_prompt_parts, Self::build_section_skills(skills));
            }
        }

        Self::push_part(&mut system_prompt_parts, "[[CACHE_BOUNDARY]]".to_string());

        if PromptSection::Bootstrap.enabled_in(inputs.mode) {
            Self::push_context_entry(
                &mut user_context,
                "Workspace Instructions",
                self.build_workspace_bootstrap_docs(inputs.mode),
            );
        }

        if PromptSection::Memory.enabled_in(inputs.mode) {
            let memory = if let Some(content) = inputs.memory_content {
                if content.is_empty() {
                    String::new()
                } else {
                    format!("# Memory\n\n{content}")
                }
            } else {
                self.build_section_memory()
            };
            Self::push_context_entry(&mut user_context, "Workspace Memory", memory);
        }

        if PromptSection::Cases.enabled_in(inputs.mode) {
            if let Some(pinned) = inputs.pinned_cases {
                Self::push_context_entry(&mut user_context, "Pinned Cases", pinned.to_string());
            }
        }

        if PromptSection::DateTime.enabled_in(inputs.mode) {
            Self::push_context_entry(
                &mut user_context,
                "Current Date And Time",
                Self::build_section_datetime(),
            );
        }

        if PromptSection::Cases.enabled_in(inputs.mode) {
            if let Some(cases) = inputs.similar_cases {
                Self::push_context_entry(&mut user_context, "Similar Cases", cases.to_string());
            }
        }

        if let Some(uid) = inputs.user_id {
            Self::push_context_entry(
                &mut system_context,
                "User Identity",
                format!("User ID (staff_id): {uid}"),
            );
        }
        if let Some(wid) = inputs.workspace_id {
            Self::push_context_entry(
                &mut system_context,
                "Workspace Identity",
                format!("Workspace ID: {wid}"),
            );
        }
        if let Some(ch) = inputs.channel {
            Self::push_context_entry(
                &mut system_context,
                "Request Channel",
                format!("Channel: {ch}"),
            );
        }
        if let Some(cid) = inputs.chat_id {
            Self::push_context_entry(
                &mut system_context,
                "Conversation Identity",
                format!("Chat ID: {cid}"),
            );
        }

        PlannedPromptContext {
            system_prompt_parts,
            user_context,
            system_context,
        }
    }

    pub fn assemble_messages(
        &self,
        planned: &PlannedPromptContext,
        history: &[HashMap<String, Value>],
        current_message: &str,
    ) -> Vec<HashMap<String, Value>> {
        let system =
            Self::render_system_prompt_parts(&planned.system_prompt_parts, &planned.system_context);

        let mut messages = vec![system_message(&system)];
        if let Some(user_context) =
            Self::render_user_context_with_focus(&planned.user_context, current_message)
        {
            messages.push(user_message(&user_context));
        }
        messages.extend(history.iter().cloned());
        messages.push(user_message(current_message));
        messages
    }

    pub fn assemble_messages_multimodal(
        &self,
        planned: &PlannedPromptContext,
        history: &[HashMap<String, Value>],
        current_message: &str,
        image_data_uris: &[String],
    ) -> Vec<HashMap<String, Value>> {
        let system =
            Self::render_system_prompt_parts(&planned.system_prompt_parts, &planned.system_context);

        let mut messages = vec![system_message(&system)];
        if let Some(user_context) =
            Self::render_user_context_with_focus(&planned.user_context, current_message)
        {
            messages.push(user_message(&user_context));
        }
        messages.extend(history.iter().cloned());
        messages.push(user_message_multimodal(current_message, image_data_uris));
        messages
    }


    // -----------------------------------------------------------------------
    // 工具方法
    // -----------------------------------------------------------------------

    /// 向消息列表中添加工具执行结果。
    ///
    /// 如果 result 包含 `[[IMAGE:data:image/...;base64,...]]` 标记，
    /// tool 消息只保留文本摘要，图片通过额外的 user 消息注入
    /// （OpenAI API 的 tool 消息不支持 image_url content blocks）。
    pub fn add_tool_result(
        messages: &mut Vec<HashMap<String, Value>>,
        tool_call_id: &str,
        tool_name: &str,
        result: &str,
    ) {
        let mut msg = HashMap::new();
        msg.insert("role".into(), json!("tool"));
        msg.insert("tool_call_id".into(), json!(tool_call_id));
        msg.insert("name".into(), json!(tool_name));

        if result.contains("[[IMAGE:data:image/") {
            // 提取图片 data URI 和文本部分
            let mut image_blocks: Vec<Value> = Vec::new();
            let mut text_parts: Vec<String> = Vec::new();
            let mut remaining = result;
            while let Some(start) = remaining.find("[[IMAGE:") {
                let before = &remaining[..start];
                if !before.trim().is_empty() {
                    text_parts.push(before.to_string());
                }
                let after_tag = &remaining[start + 8..]; // skip "[[IMAGE:"
                if let Some(end) = after_tag.find("]]") {
                    let data_uri = &after_tag[..end];
                    image_blocks.push(json!({
                        "type": "image_url",
                        "image_url": {"url": data_uri}
                    }));
                    remaining = &after_tag[end + 2..];
                } else {
                    remaining = "";
                }
            }
            if !remaining.trim().is_empty() {
                text_parts.push(remaining.to_string());
            }

            // tool 消息只放文本摘要
            let summary = if text_parts.is_empty() {
                format!("[Image loaded: {} image(s) from {tool_name}]", image_blocks.len())
            } else {
                text_parts.join("\n")
            };
            msg.insert("content".into(), json!(summary));
            messages.push(msg);

            // 图片通过 user 消息注入（role: user 支持多模态 content blocks）
            if !image_blocks.is_empty() {
                let n_images = image_blocks.len();
                let mut blocks = vec![json!({
                    "type": "text",
                    "text": format!("[Visual input from {tool_name}: {n_images} image(s)]")
                })];
                blocks.extend(image_blocks);
                let mut user_msg = HashMap::new();
                user_msg.insert("role".into(), json!("user"));
                user_msg.insert("content".into(), Value::Array(blocks));
                messages.push(user_msg);
            }
        } else {
            msg.insert("content".into(), json!(result));
            messages.push(msg);
        }
    }

    /// 向消息列表中添加助手消息。
    pub fn add_assistant_message(
        messages: &mut Vec<HashMap<String, Value>>,
        content: Option<&str>,
        tool_calls: Option<Vec<Value>>,
        reasoning: Option<&str>,
    ) {
        let mut msg = HashMap::new();
        msg.insert("role".into(), json!("assistant"));
        msg.insert(
            "content".into(),
            content.map(|c| json!(c)).unwrap_or(Value::Null),
        );
        if let Some(tcs) = tool_calls {
            msg.insert("tool_calls".into(), Value::Array(tcs));
        }
        // 回传 reasoning 给模型，保持多轮 thinking 上下文连续
        if let Some(r) = reasoning {
            msg.insert("reasoning".into(), json!(r));
        }
        messages.push(msg);
    }
}

/// 技能内容（用于注入系统提示）。
#[derive(Debug, Clone)]
pub struct SkillContent {
    pub name: String,
    pub description: String,
    pub content: String,
    pub category: String,
    pub triggers: Vec<String>,
    pub tool_path: Option<String>,
    pub risk_level: String,
    pub requires_capabilities: Vec<String>,
    /// trigger 命中时为 true，注入完整 content；
    /// 否则只注入摘要（name + description + triggers），节省 context。
    pub matched: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// 为测试初始化一个带 prompts.yaml 的临时 workspace。
    fn init_test_workspace(tmp: &TempDir) {
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("prompts.yaml"),
            r#"
identity: |
  # TyClaw Agent
  You are TyClaw, an AI assistant for enterprise automation.

main_execution_baseline: |
  Work efficiently.

guidelines: |
  Follow instructions carefully.
"#,
        )
        .unwrap();
        crate::prompt_store::init(tmp.path());
    }

    #[test]
    fn test_plan_prompt_context_full() {
        let tmp = TempDir::new().unwrap();
        init_test_workspace(&tmp);
        let ctx = ContextBuilder::new(tmp.path());
        let inputs = PromptInputs::default();
        let planned = ctx.plan_prompt_context(&inputs);
        let msgs = ctx.assemble_messages(&planned, &[], "hello");

        // system + user_context + user_message
        assert!(msgs.len() >= 2);
        assert_eq!(msgs[0]["role"], "system");
        let sys = msgs[0]["content"].as_str().unwrap();
        assert!(sys.contains("TyClaw"));
        assert!(sys.contains("[[CACHE_BOUNDARY]]"));
    }

    #[test]
    fn test_plan_prompt_context_none_mode() {
        let tmp = TempDir::new().unwrap();
        init_test_workspace(&tmp);
        let ctx = ContextBuilder::new(tmp.path());
        let inputs = PromptInputs {
            mode: PromptMode::None,
            ..Default::default()
        };
        let planned = ctx.plan_prompt_context(&inputs);
        assert_eq!(planned.system_prompt_parts.len(), 1);
    }

    #[test]
    fn test_assemble_messages_structure() {
        let tmp = TempDir::new().unwrap();
        init_test_workspace(&tmp);
        let ctx = ContextBuilder::new(tmp.path());
        let inputs = PromptInputs::default();
        let planned = ctx.plan_prompt_context(&inputs);
        let msgs = ctx.assemble_messages(&planned, &[], "test message");

        // 最后一条应是用户消息
        let last = msgs.last().unwrap();
        assert_eq!(last["role"], "user");
        assert_eq!(last["content"].as_str().unwrap(), "test message");
    }

    #[test]
    fn test_bootstrap_directory_scan() {
        let tmp = TempDir::new().unwrap();
        init_test_workspace(&tmp);
        std::fs::write(tmp.path().join("CUSTOM.md"), "custom content").unwrap();
        std::fs::write(tmp.path().join("README.txt"), "skip me").unwrap();

        let ctx = ContextBuilder::new(tmp.path());
        let inputs = PromptInputs::default();
        let planned = ctx.plan_prompt_context(&inputs);
        let msgs = ctx.assemble_messages(&planned, &[], "hi");

        // user_context 消息应包含 bootstrap docs
        let all_text: String = msgs.iter()
            .filter_map(|m| m["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all_text.contains("custom content"));
        assert!(!all_text.contains("skip me"));
    }

    #[test]
    fn test_add_tool_result() {
        let mut msgs = Vec::new();
        ContextBuilder::add_tool_result(&mut msgs, "tc_1", "read_file", "file content");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "tool");
        assert_eq!(msgs[0]["tool_call_id"], "tc_1");
    }

    #[test]
    fn test_add_tool_result_with_image() {
        let mut msgs = Vec::new();
        let img_result = "[[IMAGE:data:image/jpeg;base64,/9j/4AAQ]]";
        ContextBuilder::add_tool_result(&mut msgs, "tc_2", "read_file", img_result);
        // tool message with text summary + user message with image
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "tool");
        assert_eq!(msgs[0]["tool_call_id"], "tc_2");
        // tool content should be text, not an array
        assert!(msgs[0]["content"].is_string());
        // user message carries the image
        assert_eq!(msgs[1]["role"], "user");
        let blocks = msgs[1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2); // text + image
        assert_eq!(blocks[1]["type"], "image_url");
    }

    #[test]
    fn test_skills_in_system_prompt() {
        let tmp = TempDir::new().unwrap();
        init_test_workspace(&tmp);
        let ctx = ContextBuilder::new(tmp.path());
        let skills = vec![SkillContent {
            name: "TestSkill".into(),
            description: "test skill description".into(),
            content: "skill body".into(),
            category: "test".into(),
            triggers: vec!["hello".into()],
            tool_path: Some("/tmp/tool.py".into()),
            risk_level: "write".into(),
            requires_capabilities: vec!["sls-query".into()],
            matched: true,
        }];
        let inputs = PromptInputs {
            skill_contents: Some(&skills),
            ..Default::default()
        };
        let planned = ctx.plan_prompt_context(&inputs);
        let msgs = ctx.assemble_messages(&planned, &[], "hi");
        let sys = msgs[0]["content"].as_str().unwrap();
        assert!(sys.contains("Available Skills"));
        assert!(sys.contains("TestSkill"));
    }
}
