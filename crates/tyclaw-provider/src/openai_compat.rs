//! OpenAI 兼容的 HTTP 提供者实现。
//!
//! 适用于任何支持 `/v1/chat/completions` 接口的服务，
//! 包括 OpenAI、Anthropic（通过代理）、Azure OpenAI、本地部署的模型等。

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn, Level};

use tyclaw_types::{TyclawError, json_repair};

use crate::types::{ChatRequest, GenerationSettings, LLMResponse, ToolCallRequest};
use crate::provider::LLMProvider;

/// HTTP 发送超时（等待首个响应头）：涵盖 TCP 握手 + TLS + 服务端排队 + 模型加载。
/// 正常 LLM 请求 10-30s 内开始返回；60s 不响应基本是死连接或严重排队。
const SEND_TIMEOUT_SECS: u64 = 60;

/// SSE chunk 间隔超时：两个连续 data chunk 之间的最大等待。
/// thinking 模型可能有较长的首 chunk 延迟，但 90s 内一定会有心跳或数据。
const CHUNK_TIMEOUT_SECS: u64 = 90;

/// Reasoning 累积上限（字符数）。GLM 偶尔会在 reasoning 中无限输出
/// （把整个代码写进 thinking），超过此阈值后截断 reasoning 并继续处理。
const MAX_REASONING_CHARS: usize = 32_000;

/// 非流式请求的整体超时：需等待完整响应，给予稍长时间。
const NON_STREAM_TIMEOUT_SECS: u64 = 120;

/// OpenAI 兼容的 LLM 提供者。
///
/// 通过标准的 HTTP POST 请求与 LLM API 通信，
/// 支持 Bearer Token 认证和工具调用（function calling）。
pub struct OpenAICompatProvider {
    client: Client,             // HTTP 客户端（复用连接池）
    api_key: String,            // API 密钥
    api_base: String,           // API 基础 URL（如 https://api.openai.com/v1）
    default_model: String,      // 默认模型名称
    generation: GenerationSettings,  // 默认生成参数
    /// 每个 cache scope 上一次成功发送并提交的 cache 状态。
    cache_state: std::sync::Mutex<HashMap<String, CacheScopeState>>,
    /// LLM request 快照根目录（观测用途）。
    snapshot_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
struct CacheScopeState {
    committed_canonical: Vec<Value>,
    committed_final_messages: Vec<Value>,
    protected_prefix_len: usize,
}

#[derive(Debug, Clone)]
struct CacheCommitPlan {
    scope: String,
    state: CacheScopeState,
}

#[derive(Debug, Clone)]
struct PreparedRequestBody {
    body: Value,
    commit_plan: Option<CacheCommitPlan>,
}

impl OpenAICompatProvider {
    /// 创建新的 OpenAI 兼容提供者实例。
    ///
    /// - `api_key`: API 认证密钥
    /// - `api_base`: API 基础 URL，自动去掉末尾的斜杠
    /// - `default_model`: 默认使用的模型名称
    pub fn new(api_key: &str, api_base: &str, default_model: &str, thinking: Option<crate::types::ThinkingConfig>) -> Self {
        let client = Client::builder()
            // 强制 HTTP/1.1：避免 HTTP/2 在代理/LB 上的 SSE 流控问题。
            // 很多 nginx 反向代理对 HTTP/2 长连接 SSE 有 idle timeout 限制，
            // 而 HTTP/1.1 chunked transfer 没有这个问题（与 curl 行为一致）。
            .http1_only()
            // 禁用连接池复用：每次请求建新连接。
            // 代价是多一次 TCP/TLS 握手（~100ms），但彻底避免
            // 用户交互暂停后恢复时打到死连接的问题。
            .pool_max_idle_per_host(0)
            .connect_timeout(std::time::Duration::from_secs(10))
            // 注意：不设置 .timeout()（全局请求超时）。
            // SSE 流式请求的超时由逐 chunk 的 tokio::time::timeout 控制，
            // 全局 timeout 会在请求开始后 N 秒强制断开整个连接，
            // 导致长生成时间的 LLM 响应被截断。
            // keepalive 防止代理/LB 在 LLM 长时间生成响应时断开连接
            .tcp_keepalive(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            client,
            api_key: api_key.to_string(),
            api_base: api_base.trim_end_matches('/').to_string(),
            default_model: default_model.to_string(),
            generation: GenerationSettings {
                thinking,
                ..GenerationSettings::default()
            },
            cache_state: std::sync::Mutex::new(HashMap::new()),
            snapshot_dir: None,
        }
    }

    /// 覆盖默认 temperature。
    pub fn set_temperature(&mut self, temperature: f64) {
        self.generation.temperature = temperature;
    }

    /// 覆盖默认 max_tokens。
    pub fn set_max_tokens(&mut self, max_tokens: u32) {
        self.generation.max_tokens = max_tokens;
    }

    pub fn set_snapshot_dir(&mut self, snapshot_dir: impl AsRef<Path>) {
        self.snapshot_dir = Some(snapshot_dir.as_ref().to_path_buf());
    }

    /// 构建完整的 API 端点 URL。
    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.api_base)
    }

    fn commit_cache_plan(&self, plan: CacheCommitPlan) {
        self.cache_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(plan.scope, plan.state);
    }

    /// 构建 HTTP 请求体（JSON 格式）和可选的 cache 提交计划。
    ///
    /// 处理逻辑：
    /// 1. 确定模型名称：优先使用请求中指定的模型，否则使用默认模型
    /// 2. 去掉 "openai/" 前缀 —— 代理服务器只需要原始模型名
    /// 3. 对消息列表进行清洗（处理空内容、null 值等）
    /// 4. 根据上一次成功发送的 scope 状态规划稳定前缀和 cache marker
    /// 5. 如果提供了工具定义，添加 tools 和 tool_choice 字段
    fn prepare_body(&self, request: &ChatRequest) -> PreparedRequestBody {
        let model = request
            .model
            .as_deref()
            .unwrap_or(&self.default_model)
            // 去掉 "openai/" 前缀 —— 代理服务器期望收到原始模型名
            .strip_prefix("openai/")
            .unwrap_or(
                request
                    .model
                    .as_deref()
                    .unwrap_or(&self.default_model),
            );

        let has_thinking = self.generation.thinking.is_some();

        let sanitized = sanitize_messages(&request.messages);
        let supports_cache = model.contains("claude") || model.contains("gpt")
            || model.contains("deepseek") || model.contains("gemini");

        let mut commit_plan = None;
        let messages_final = if supports_cache {
            let mut canonical = normalize_to_blocks(&sanitized);

            if let Some(scope) = request.cache_scope.as_deref() {
                let prev_state = self
                    .cache_state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(scope)
                    .cloned()
                    .unwrap_or_default();

                // 只复用上一次“已成功发送”的 canonical，避免发送前提前推进状态。
                // 仍保留“最后一条不 overlay”的约定，默认把每轮附加的动态 tail（通常是 STATE_VIEW）留给新请求自己生成。
                let overlay_end = prev_state
                    .committed_canonical
                    .len()
                    .saturating_sub(1)
                    .min(canonical.len());
                if overlay_end >= 2 {
                    for i in 0..overlay_end {
                        canonical[i] = prev_state.committed_canonical[i].clone();
                    }
                }

                let prefix_len = common_prefix_len(&prev_state.committed_canonical, &canonical);
                let mut final_msgs = canonical.clone();
                if !final_msgs.is_empty() {
                    apply_system_cache_control(&mut final_msgs[0]);
                }

                // 在最后一条有实际文本的 assistant 消息上加 cache_control，
                // 让缓存范围随对话增长（覆盖 system + tools + 历史前缀）。
                // 跳过 tool-call-only 的 assistant（content 为空），CC 加在空 block 上无效。
                // 不能加在 tool 消息上（format 会被 normalize_to_blocks 还原，破坏前缀）。
                if let Some(last_hist_idx) = final_msgs.iter().rposition(|m| {
                    let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    if role != "assistant" { return false; }
                    // 检查 content 是否有非空文本
                    match m.get("content") {
                        Some(Value::Array(blocks)) => blocks.iter().any(|b| {
                            b.get("text").and_then(|t| t.as_str()).map(|s| !s.is_empty()).unwrap_or(false)
                        }),
                        Some(Value::String(s)) => !s.is_empty(),
                        _ => false,
                    }
                }) {
                    if last_hist_idx > 0 {
                        add_history_cache_breakpoint(&mut final_msgs[last_hist_idx]);
                    }
                }

                let (first_diff_idx, first_diff_reason) =
                    first_payload_diff(&prev_state.committed_final_messages, &final_msgs);
                let protected_prefix_len = prefix_len.min(final_msgs.len());
                let cache_mode = "top_level_frontier";
                info!(
                    cache_scope = scope,
                    cache_mode,
                    overlay_end,
                    prefix_len,
                    protected_prefix_len,
                    preserved_history_markers = ?Vec::<usize>::new(),
                    candidate_marker = ?Option::<usize>::None,
                    first_diff_idx = ?first_diff_idx,
                    first_diff_reason = %first_diff_reason,
                    total = final_msgs.len(),
                    "Prompt cache planner"
                );

                commit_plan = Some(CacheCommitPlan {
                    scope: scope.to_string(),
                    state: CacheScopeState {
                        committed_canonical: canonical,
                        committed_final_messages: final_msgs.clone(),
                        protected_prefix_len,
                    },
                });
                final_msgs
            } else {
                // 无 scope：只加 system cache_control + 历史 breakpoint
                let mut final_msgs = canonical;
                if !final_msgs.is_empty() {
                    apply_system_cache_control(&mut final_msgs[0]);
                }
                if let Some(last_hist_idx) = final_msgs.iter().rposition(|m| {
                    let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    if role != "assistant" { return false; }
                    match m.get("content") {
                        Some(Value::Array(blocks)) => blocks.iter().any(|b| {
                            b.get("text").and_then(|t| t.as_str()).map(|s| !s.is_empty()).unwrap_or(false)
                        }),
                        Some(Value::String(s)) => !s.is_empty(),
                        _ => false,
                    }
                }) {
                    if last_hist_idx > 0 {
                        add_history_cache_breakpoint(&mut final_msgs[last_hist_idx]);
                    }
                }
                final_msgs
            }
        } else {
            strip_cache_boundary(&sanitized)
        };
        let messages_final = ensure_tool_call_pairs(messages_final);
        let mut body = json!({
            "model": model,
            "messages": messages_final,
        });
        if let Some(provider_prefs) = openrouter_provider_preferences(&self.api_base, model) {
            body["provider"] = provider_prefs;
            info!(
                model = %model,
                provider = %body["provider"],
                "OpenRouter provider routing enforced"
            );
        }

        // 开启 thinking 时必须移除 temperature/top_p/top_k，否则 API 返回 400
        if !has_thinking {
            body["temperature"] = json!(request.temperature);
        }

        // OpenAI 新模型（o3, gpt-5.4 等）使用 max_completion_tokens，旧模型和其他 API 使用 max_tokens
        if self.api_base.contains("openai.com") {
            body["max_completion_tokens"] = json!(request.max_tokens);
        } else {
            body["max_tokens"] = json!(request.max_tokens);
        }

        // 如果有工具定义，添加到请求体中
        if let Some(tools) = &request.tools {
            if !tools.is_empty() {
                let mut tools_arr = tools.clone();
                // 在最后一个 tool 顶层加 cache_control，让 tools 定义被 prompt caching 缓存
                // 注意：必须放在 tool 顶层而非 function 内部，Anthropic API 只识别顶层的 cache_control
                if supports_cache {
                    if let Some(last) = tools_arr.last_mut() {
                        last["cache_control"] = json!({"type": "ephemeral"});
                    }
                }
                body["tools"] = Value::Array(tools_arr);
                body["tool_choice"] = json!("auto");
            }
        }

        // Extended thinking（OpenRouter 兼容格式）
        // - adaptive 模式：reasoning.effort，模型自行决定是否 think
        // - forced 模式：reasoning.max_tokens，强制每轮 think
        if let Some(ref thinking) = self.generation.thinking {
            if let Some(budget) = thinking.budget_tokens {
                body["reasoning"] = json!({ "max_tokens": budget });
            } else {
                body["reasoning"] = json!({ "effort": thinking.effort });
            }
        }

        PreparedRequestBody { body, commit_plan }
    }

    /// 解析 LLM API 的 JSON 响应。
    ///
    /// 从 OpenAI 格式的响应中提取：
    /// 1. 文本内容（content）
    /// 2. 结束原因（finish_reason）：stop、tool_calls 等
    /// 3. 工具调用列表 —— 解析每个工具的 id、name 和 arguments
    /// 4. Token 使用量统计
    /// 5. 推理内容（reasoning_content，部分模型支持）
    fn parse_response(&self, data: Value) -> Result<LLMResponse, TyclawError> {
        if let Some(provider) = response_provider(&data) {
            info!(
                provider = %provider,
                model = %self.default_model,
                "LLM upstream provider"
            );
        }
        // 从 choices 数组中取第一个选择
        let choice = data
            .get("choices")
            .and_then(|c| c.get(0))
            .ok_or_else(|| TyclawError::Provider("No choices in response".into()))?;

        let message = choice.get("message").unwrap_or(&Value::Null);
        let raw_content = message
            .get("content")
            .and_then(|v| v.as_str())
            .map(String::from);
        let finish_reason = choice
            .get("finish_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("stop")
            .to_string();

        // 解析工具调用列表
        let mut tool_calls = Vec::new();
        if let Some(Value::Array(tcs)) = message.get("tool_calls") {
            for tc in tcs {
                // 提取工具调用 ID（默认为 "tc_0"）
                let id = tc
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tc_0")
                    .to_string();
                // 提取函数信息
                let func = tc.get("function").unwrap_or(&Value::Null);
                let name = func
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // arguments 是 JSON 字符串，需要解析为 HashMap
                // 使用 json_repair 处理 LLM 可能输出的畸形 JSON
                let args_str = func
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let arguments: HashMap<String, Value> =
                    match json_repair::repair_json(args_str) {
                        Ok(v) => serde_json::from_value(v).unwrap_or_default(),
                        Err(_) => {
                            let truncated: String = args_str.chars().take(200).collect();
                            warn!(raw_args = %truncated, "json_repair failed for tool call arguments");
                            HashMap::new()
                        }
                    };

                tool_calls.push(ToolCallRequest {
                    id,
                    name,
                    arguments,
                });
            }
        }

        // 提取 token 使用量统计
        let mut usage = HashMap::new();
        if let Some(u) = data.get("usage") {
            for key in &["prompt_tokens", "completion_tokens", "total_tokens"] {
                if let Some(n) = u.get(*key).and_then(|v| v.as_u64()) {
                    usage.insert(key.to_string(), n);
                }
            }
            // Prompt caching 指标（OpenRouter 返回）
            if let Some(details) = u.get("prompt_tokens_details") {
                if let Some(cached) = details.get("cached_tokens").and_then(|v| v.as_u64()) {
                    usage.insert("cached_tokens".to_string(), cached);
                    if cached > 0 {
                        tracing::info!(model = %self.default_model, cached_tokens = cached, "Prompt cache HIT");
                    }
                }
                if let Some(written) = details.get("cache_write_tokens").and_then(|v| v.as_u64()) {
                    if written > 0 {
                        usage.insert("cache_write_tokens".to_string(), written);
                        tracing::info!(model = %self.default_model, cache_write_tokens = written, "Prompt cache WRITE");
                    }
                }
            }
        }

        // 提取推理内容：
        // - DeepSeek 使用 "reasoning_content" 字段
        // - OpenRouter/relay 代理返回 Claude thinking 时使用 "reasoning" 字段
        let reasoning_content = message
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .or_else(|| message.get("reasoning").and_then(|v| v.as_str()))
            .map(String::from);

        let content = raw_content;

        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason,
            usage,
            reasoning_content,
        })
    }

    /// SSE 流式请求：逐 chunk 读取，累积拼出完整响应。
    /// 流式模式下代理/LB 不会因为长时间无数据而断连。
    async fn chat_stream(&self, request: ChatRequest) -> Result<LLMResponse, TyclawError> {
        let prepared = self.prepare_body(&request);
        let mut body = prepared.body;
        body["stream"] = json!(true);
        // stream_options: 请求在流末尾返回 usage 统计（含 prompt cache 命中信息）
        body["stream_options"] = json!({"include_usage": true});

        // 写 snapshot：发给 LLM 的最终请求体（含 cache_control、prefix overlay 等加工）
        // 默认在有 snapshot_dir 时写入，也可通过 TYCLAW_WRITE_LLM_REQUEST_SNAPSHOTS=1 强制开启
        if self.snapshot_dir.is_some() || should_write_llm_request_snapshots() {
            if let Ok(json_str) = serde_json::to_string_pretty(&body) {
                use std::sync::atomic::{AtomicU64, Ordering};
                static SEQ: AtomicU64 = AtomicU64::new(0);
                let seq = SEQ.fetch_add(1, Ordering::Relaxed);
                let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("unknown");
                let model = sanitize_filename_component(model);
                let scope_suffix = request
                    .cache_scope
                    .as_deref()
                    .map(short_scope_hash)
                    .map(|h| format!("_scope-{h}"))
                    .unwrap_or_default();
                let base_dir = self
                    .snapshot_dir
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("logs").join("snap").join("llm_requests"));
                let scoped_dir = request
                    .cache_scope
                    .as_deref()
                    .map(snapshot_scope_dir)
                    .map(|scope_dir| base_dir.join(scope_dir))
                    .unwrap_or(base_dir);
                let path = scoped_dir.join(format!("llm_request_{seq:04}_{model}{scope_suffix}.json"));
                if let Err(e) = std::fs::create_dir_all(&scoped_dir) {
                    warn!(path = %scoped_dir.display(), error = %e, "Failed to create LLM request snapshot directory");
                } else if let Err(e) = std::fs::write(&path, json_str) {
                    warn!(path = %path.display(), error = %e, "Failed to write LLM request snapshot");
                }
            }
        }

        if tracing::enabled!(Level::DEBUG) {
            debug!(
                target: "llm.request",
                endpoint = %self.endpoint(),
                model = %body.get("model").and_then(|v| v.as_str()).unwrap_or("unknown"),
                payload = %serde_json::to_string_pretty(&body).unwrap_or_default(),
                "LLM request payload",
            );
        }

        let endpoint = self.endpoint();
        let auth = format!("Bearer {}", self.api_key);
        let model_in_body = body.get("model").and_then(|v| v.as_str()).unwrap_or("?");
        info!(
            endpoint = %endpoint,
            model = %model_in_body,
            key_prefix = %&self.api_key[..self.api_key.len().min(8)],
            "SSE request sending"
        );

        // 带重试的 SSE 请求
        let mut resp = None;
        let mut last_err = String::new();
        for attempt in 1..=3u32 {
            let mut req = self
                .client
                .post(&endpoint)
                .header("Authorization", &auth)
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .header("X-Accel-Buffering", "no")
                .header("Cache-Control", "no-cache");
            // Anthropic 原生 header 仅在直连时添加（relay 代理不需要）
            if self.api_base.contains("anthropic.com") {
                req = req
                    .header("x-api-key", &self.api_key)
                    .header("anthropic-version", "2023-06-01");
            }
            let send_fut = req.json(&body).send();
            let send_result = tokio::time::timeout(
                std::time::Duration::from_secs(SEND_TIMEOUT_SECS),
                send_fut,
            ).await;

            match send_result {
                Ok(Ok(r)) => {
                    if !r.status().is_success() {
                        let status = r.status();
                        let text = r.text().await.unwrap_or_default();
                        return Ok(LLMResponse::error(format!(
                            "HTTP {}: {}",
                            status.as_u16(),
                            &text[..text.len().min(500)]
                        )));
                    }
                    if attempt > 1 {
                        info!(attempt, "SSE request succeeded after retry");
                    }
                    if let Some(plan) = prepared.commit_plan.clone() {
                        self.commit_cache_plan(plan);
                    }
                    resp = Some(r);
                    break;
                }
                Ok(Err(e)) => {
                    last_err = format!("{e}");
                    warn!(error = %e, attempt, "SSE request failed, retrying");
                    if attempt < 3 {
                        tokio::time::sleep(std::time::Duration::from_secs(attempt as u64)).await;
                    }
                }
                Err(_) => {
                    last_err = format!("send timeout ({SEND_TIMEOUT_SECS}s)");
                    warn!(attempt, timeout_s = SEND_TIMEOUT_SECS, "SSE send timeout, retrying");
                }
            }
        }
        let resp = resp.ok_or_else(|| {
            TyclawError::Provider(format!("SSE HTTP error after 3 attempts: {last_err}"))
        })?;

        // 逐行读取 SSE 事件流，累积 delta
        let mut content_parts: Vec<String> = Vec::new();
        let mut finish_reason = String::from("stop");
        let mut usage_map: HashMap<String, u64> = HashMap::new();
        let mut reasoning_parts: Vec<String> = Vec::new();
        let mut stream_provider: Option<String> = None;
        // tool_calls 按 index 累积：index → (id, name, arguments_parts)
        let mut tool_call_map: HashMap<usize, (String, String, Vec<String>)> = HashMap::new();

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut got_meaningful_content = false;
        let stream_start = std::time::Instant::now();
        let mut last_chunk_time = std::time::Instant::now();
        let mut reasoning_truncated_at: Option<std::time::Instant> = None;
        let mut abort_stream = false;

        loop {
            let chunk_timeout = std::time::Duration::from_secs(CHUNK_TIMEOUT_SECS);
            let chunk = match tokio::time::timeout(chunk_timeout, stream.next()).await {
                Ok(Some(Ok(c))) => {
                    last_chunk_time = std::time::Instant::now();
                    c
                },
                Ok(Some(Err(e))) => {
                    // stream.next() 返回错误 → 连接被对端断开（relay 超时/重置）
                    let err_str = e.to_string();
                    let cause = if err_str.contains("reset") || err_str.contains("RST") {
                        "relay/proxy connection reset"
                    } else if err_str.contains("closed") || err_str.contains("EOF") {
                        "relay/proxy closed connection"
                    } else {
                        "network error"
                    };
                    let total_elapsed = stream_start.elapsed().as_secs();
                    let since_last = last_chunk_time.elapsed().as_secs();
                    return Err(TyclawError::Provider(format!(
                        "SSE read error ({cause}): {e} [has_content={got_meaningful_content}, total={total_elapsed}s, since_last_chunk={since_last}s]",
                    )));
                }
                Ok(None) => {
                    // stream 正常结束但可能内容不完整（relay 提前关闭）
                    if got_meaningful_content && content_parts.is_empty() && tool_call_map.is_empty() {
                        return Err(TyclawError::Provider(
                            "SSE stream ended prematurely (relay/proxy may have closed before LLM finished)".into()
                        ));
                    }
                    break;
                }
                Err(_) => {
                    // tokio::time::timeout 触发 → 我们自己的超时
                    let total_elapsed = stream_start.elapsed().as_secs();
                    return Err(TyclawError::Provider(format!(
                        "SSE chunk timeout [client-side] ({}s, has_content={}, total={total_elapsed}s)",
                        chunk_timeout.as_secs(), got_meaningful_content
                    )));
                }
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));

            // 防止恶意/异常响应导致 OOM
            const MAX_SSE_BUF_BYTES: usize = 2 * 1024 * 1024; // 2MB
            if buf.len() > MAX_SSE_BUF_BYTES {
                return Err(TyclawError::Provider(format!(
                    "SSE buffer exceeded {MAX_SSE_BUF_BYTES} bytes, aborting"
                )));
            }

            // 按行处理 SSE 事件
            while let Some(newline_pos) = buf.find('\n') {
                let line = buf[..newline_pos].trim().to_string();
                buf = buf[newline_pos + 1..].to_string();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }
                if line == "data: [DONE]" {
                    break;
                }
                let json_str = if let Some(stripped) = line.strip_prefix("data: ") {
                    stripped
                } else {
                    continue;
                };

                let chunk_data: Value = match serde_json::from_str(json_str) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if stream_provider.is_none() {
                    stream_provider = response_provider(&chunk_data);
                    if let Some(provider) = stream_provider.as_deref() {
                        info!(
                            provider = %provider,
                            model = %self.default_model,
                            "LLM upstream provider (SSE)"
                        );
                    }
                }

                let choice = match chunk_data.get("choices").and_then(|c| c.get(0)) {
                    Some(c) => c,
                    None => {
                        // 可能是 usage-only chunk
                        if let Some(u) = chunk_data.get("usage") {
                            info!(
                                usage = %u,
                                model = %self.default_model,
                                provider = stream_provider.as_deref().unwrap_or("unknown"),
                                "SSE usage chunk raw"
                            );
                            for key in &["prompt_tokens", "completion_tokens", "total_tokens"] {
                                if let Some(n) = u.get(*key).and_then(|v| v.as_u64()) {
                                    usage_map.insert(key.to_string(), n);
                                }
                            }
                            // Prompt caching 指标
                            if let Some(details) = u.get("prompt_tokens_details") {
                                if let Some(cached) = details.get("cached_tokens").and_then(|v| v.as_u64()) {
                                    usage_map.insert("cached_tokens".to_string(), cached);
                                    if cached > 0 {
                                        tracing::info!(model = %self.default_model, cached_tokens = cached, "Prompt cache HIT (SSE)");
                                    }
                                }
                                if let Some(written) = details.get("cache_write_tokens").and_then(|v| v.as_u64()) {
                                    if written > 0 {
                                        usage_map.insert("cache_write_tokens".to_string(), written);
                                        tracing::info!(model = %self.default_model, cache_write_tokens = written, "Prompt cache WRITE (SSE)");
                                    }
                                }
                            }
                        }
                        continue;
                    }
                };

                if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                    finish_reason = fr.to_string();
                }

                let delta = match choice.get("delta") {
                    Some(d) => d,
                    None => continue,
                };

                // 调试：打印含 reasoning 的 SSE chunk
                if delta.get("reasoning").is_some() || delta.get("reasoning_content").is_some() || delta.get("reasoning_details").is_some() {
                    debug!(target: "sse.reasoning", chunk = %delta, "SSE delta with reasoning");
                }

                // 累积 content
                if let Some(c) = delta.get("content").and_then(|v| v.as_str()) {
                    if !c.is_empty() {
                        got_meaningful_content = true;
                        reasoning_truncated_at = None; // 收到 content，模型已恢复正常
                    }
                    content_parts.push(c.to_string());
                }

                // 累积 reasoning_content（DeepSeek）或 reasoning（OpenRouter/relay 代理的 Claude thinking）
                let reasoning_delta = delta.get("reasoning_content").and_then(|v| v.as_str())
                    .or_else(|| delta.get("reasoning").and_then(|v| v.as_str()));
                if let Some(r) = reasoning_delta {
                    if !r.is_empty() {
                        got_meaningful_content = true;
                    }
                    let current_reasoning_len: usize = reasoning_parts.iter().map(|s| s.len()).sum();
                    if current_reasoning_len < MAX_REASONING_CHARS {
                        reasoning_parts.push(r.to_string());
                    } else if current_reasoning_len < MAX_REASONING_CHARS + 100 {
                        // 只记一次警告，避免日志刷屏
                        tracing::warn!(
                            len = current_reasoning_len,
                            max = MAX_REASONING_CHARS,
                            "Reasoning exceeded max length — truncating (model may be stuck in reasoning loop)"
                        );
                        reasoning_parts.push("\n\n[REASONING TRUNCATED — exceeded limit]".to_string());
                        reasoning_truncated_at = Some(std::time::Instant::now());
                    }
                    // 超过上限后继续读 SSE 流但丢弃 reasoning tokens
                    // 如果截断后 30 秒内仍然只收到 reasoning delta（没有 content/tool_calls），
                    // 说明模型卡在无限 reasoning 循环中，主动中断流。
                    if let Some(truncated_at) = reasoning_truncated_at {
                        if truncated_at.elapsed().as_secs() > 30
                            && content_parts.is_empty()
                            && tool_call_map.is_empty()
                        {
                            tracing::error!(
                                elapsed_since_truncation = truncated_at.elapsed().as_secs(),
                                total_elapsed = stream_start.elapsed().as_secs(),
                                "Model stuck in infinite reasoning loop — aborting SSE stream"
                            );
                            finish_reason = "length".to_string();
                            abort_stream = true;
                            break; // 跳出内层 while 循环
                        }
                    }
                }

                // 累积 tool_calls (按 index 分组)
                if let Some(Value::Array(tcs)) = delta.get("tool_calls") {
                    got_meaningful_content = true;
                    reasoning_truncated_at = None; // 收到 tool_calls，模型已恢复正常
                    for tc in tcs {
                        let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        let entry = tool_call_map.entry(idx).or_insert_with(|| {
                            (String::new(), String::new(), Vec::new())
                        });
                        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                            entry.0 = id.to_string();
                        }
                        if let Some(func) = tc.get("function") {
                            if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                entry.1 = name.to_string();
                            }
                            if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                                entry.2.push(args.to_string());
                            }
                        }
                    }
                }

                // 累积 usage
                if let Some(u) = chunk_data.get("usage") {
                    debug!(
                        usage = %u,
                        model = %self.default_model,
                        provider = stream_provider.as_deref().unwrap_or("unknown"),
                        "SSE usage chunk raw (with choice)"
                    );
                    for key in &["prompt_tokens", "completion_tokens", "total_tokens"] {
                        if let Some(n) = u.get(*key).and_then(|v| v.as_u64()) {
                            usage_map.insert(key.to_string(), n);
                        }
                    }
                    if let Some(details) = u.get("prompt_tokens_details") {
                        if let Some(cached) = details.get("cached_tokens").and_then(|v| v.as_u64()) {
                            usage_map.insert("cached_tokens".to_string(), cached);
                            if cached > 0 {
                                tracing::info!(model = %self.default_model, cached_tokens = cached, "Prompt cache HIT (SSE)");
                            }
                        }
                        if let Some(written) = details.get("cache_write_tokens").and_then(|v| v.as_u64()) {
                            if written > 0 {
                                usage_map.insert("cache_write_tokens".to_string(), written);
                                tracing::info!(model = %self.default_model, cache_write_tokens = written, "Prompt cache WRITE (SSE)");
                            }
                        }
                    }
                }
            }
            if abort_stream {
                break; // 跳出外层 loop
            }
        }

        // 拼装最终响应（复用 parse_response 的后处理逻辑）
        let raw_content = if content_parts.is_empty() {
            None
        } else {
            Some(content_parts.join(""))
        };
        let reasoning_content = if reasoning_parts.is_empty() {
            None
        } else {
            Some(reasoning_parts.join(""))
        };

        // 拼装 tool_calls
        let mut tool_calls = Vec::new();
        let mut indices: Vec<usize> = tool_call_map.keys().cloned().collect();
        indices.sort();
        for idx in indices {
            let Some((id, name, args_parts)) = tool_call_map.remove(&idx) else {
                warn!(idx, "SSE tool_call index missing from map, skipping");
                continue;
            };
            let args_str = args_parts.join("");
            let arguments: HashMap<String, Value> =
                match json_repair::repair_json(&args_str) {
                    Ok(v) => serde_json::from_value(v).unwrap_or_default(),
                    Err(_) => {
                        let truncated: String = args_str.chars().take(200).collect();
                        warn!(raw_args = %truncated, "json_repair failed for SSE tool call arguments");
                        HashMap::new()
                    }
                };
            tool_calls.push(ToolCallRequest {
                id,
                name,
                arguments,
            });
        }

        // GLM 有时把 tool_call 写在 reasoning（thinking）里而不是标准 function calling。
        // 如果 tool_calls 为空但 reasoning 中包含 <tool_call> 标签，尝试提取。
        if tool_calls.is_empty() {
            if let Some(ref reasoning) = reasoning_content {
                let rescued = rescue_tool_calls_from_reasoning(reasoning);
                if !rescued.is_empty() {
                    tracing::warn!(
                        count = rescued.len(),
                        "Rescued tool_calls from reasoning content (model put them in wrong field)"
                    );
                    tool_calls = rescued;
                }
            }
        }

        let content = raw_content;

        // 诊断日志：把最终聚合的 response 内容落盘（截断前 500 字），便于排查
        // "LLM 说要做却没调工具" 这类空承诺型回复。log_level=debug 可见。
        {
            let content_preview: String = content
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(500)
                .collect();
            let reasoning_preview: String = reasoning_content
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(200)
                .collect();
            debug!(
                target: "llm.response",
                model = %self.default_model,
                content_len = content.as_deref().map(|s| s.len()).unwrap_or(0),
                reasoning_len = reasoning_content.as_deref().map(|s| s.len()).unwrap_or(0),
                tool_call_count = tool_calls.len(),
                content_preview = %content_preview,
                reasoning_preview = %reasoning_preview,
                "LLM aggregated response (SSE)"
            );
        }

        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason,
            usage: usage_map,
            reasoning_content,
        })
    }

    /// 非流式请求（SSE 失败时的回退）。
    async fn chat_non_stream(&self, request: ChatRequest) -> Result<LLMResponse, TyclawError> {
        let prepared = self.prepare_body(&request);
        let body = prepared.body;
        let endpoint = self.endpoint();
        let auth = format!("Bearer {}", self.api_key);
        let mut resp = None;
        let mut last_err = String::new();
        for attempt in 1..=3u32 {
            let mut req = self
                .client
                .post(&endpoint)
                .header("Authorization", &auth)
                .header("Content-Type", "application/json");
            // Anthropic 原生 header 仅在直连时添加（relay 代理不需要）
            if self.api_base.contains("anthropic.com") {
                req = req
                    .header("x-api-key", &self.api_key)
                    .header("anthropic-version", "2023-06-01");
            }
            let send_fut = req.json(&body).send();
            let send_result = tokio::time::timeout(
                std::time::Duration::from_secs(NON_STREAM_TIMEOUT_SECS),
                send_fut,
            ).await;

            match send_result {
                Ok(Ok(r)) => {
                    if attempt > 1 {
                        info!(attempt, "Non-stream request succeeded after retry");
                    }
                    if let Some(plan) = prepared.commit_plan.clone() {
                        self.commit_cache_plan(plan);
                    }
                    resp = Some(r);
                    break;
                }
                Ok(Err(e)) => {
                    last_err = format!("{e}");
                    warn!(error = %e, attempt, "Non-stream request failed, retrying");
                    if attempt < 3 {
                        tokio::time::sleep(std::time::Duration::from_secs(attempt as u64)).await;
                    }
                }
                Err(_) => {
                    last_err = format!("send timeout ({NON_STREAM_TIMEOUT_SECS}s)");
                    warn!(attempt, timeout_s = NON_STREAM_TIMEOUT_SECS, "Non-stream send timeout, retrying");
                }
            }
        }
        let resp = resp.ok_or_else(|| {
            TyclawError::Provider(format!("HTTP error after 3 attempts: {last_err}"))
        })?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| TyclawError::Provider(format!("Read body error: {e}")))?;

        if !status.is_success() {
            return Ok(LLMResponse::error(format!(
                "HTTP {}: {}",
                status.as_u16(),
                &text[..text.len().min(500)]
            )));
        }

        let data: Value = serde_json::from_str(&text)
            .map_err(|e| TyclawError::Provider(format!("JSON parse error: {e}")))?;

        self.parse_response(data)
    }
}

/// 从 reasoning 中抢救被错误放置的 tool_call。
///
/// 使用 reasoning 解析器提取 ToolCall 块，转换为标准 ToolCallRequest。
/// 只在 tool_calls 为空时调用，作为兜底解析。
/// 只抢救第一个 tool_call，避免一次性执行太多。
fn rescue_tool_calls_from_reasoning(reasoning: &str) -> Vec<ToolCallRequest> {
    let parsed = crate::reasoning::parse_reasoning(reasoning);
    if !parsed.has_misplaced_tool_calls {
        return Vec::new();
    }

    let mut results = Vec::new();
    for block in &parsed.blocks {
        if let crate::reasoning::ReasoningBlock::ToolCall { tool_name, arguments, .. } = block {
            if tool_name.is_empty() {
                continue;
            }
            let args: HashMap<String, Value> = arguments
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            results.push(ToolCallRequest {
                id: format!("rescued_{}", results.len()),
                name: tool_name.clone(),
                arguments: args,
            });
            // 只抢救第一个
            break;
        }
    }

    results
}

/// 实现 LLMProvider trait，使 OpenAICompatProvider 可以作为通用 LLM 提供者使用。
#[async_trait]
impl LLMProvider for OpenAICompatProvider {
    fn cache_breakpoint_idx(&self, scope: &str) -> usize {
        self.cache_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(scope)
            .map(|state| state.protected_prefix_len)
            .unwrap_or(0)
    }

    /// 发送聊天请求到 OpenAI 兼容 API。
    ///
    /// 流程：
    /// 1. 构建请求体
    /// 2. 发送 HTTP POST 请求（带 Bearer Token 认证）
    /// 3. 检查 HTTP 状态码，非成功状态码返回错误响应
    /// 4. 解析 JSON 响应
    async fn chat(&self, request: ChatRequest) -> Result<LLMResponse, TyclawError> {
        // 注意：不要在这里调用 build_body，chat_stream 内部会调用。
        // 之前这里重复调用 build_body 导致 last_sanitized 被提前更新，
        // chat_stream 里第二次 build_body 的 prefix_len == messages.len()，
        // 条件 prefix_len < len 不满足，history cache breakpoint 丢失。

        // SSE 优先，流读取失败时重建连接重试（最多 2 次 SSE），最后回退非流式
        for sse_attempt in 1..=2u32 {
            match self.chat_stream(request.clone()).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    warn!(
                        error = %e,
                        sse_attempt,
                        "SSE stream failed{}",
                        if sse_attempt < 2 { ", retrying with fresh connection" } else { ", falling back to non-stream" }
                    );
                    if sse_attempt < 2 {
                        // 短暂等待后重试，让底层连接完全释放
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            }
        }
        // SSE 两次都失败，回退非流式
        self.chat_non_stream(request).await
    }

    /// 返回默认模型名称。
    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn api_base(&self) -> String {
        self.api_base.clone()
    }

    fn api_key(&self) -> String {
        self.api_key.clone()
    }

    fn clear_cache_scope(&self, scope: &str) {
        let mut state = self.cache_state.lock().unwrap();
        state.remove(scope);
    }

    /// 返回当前的生成参数配置。
    fn generation_settings(&self) -> GenerationSettings {
        self.generation.clone()
    }
}

/// 为 system 消息注入 prompt caching 断点。
///
/// 识别 `[[CACHE_BOUNDARY]]` 标记，将 system message 拆成两个 content block：
/// - 静态块（Identity + Guidelines + Bootstrap）：加 cache_control，每轮命中缓存
/// - 动态块（DateTime + Skills + Cases）：不缓存，每轮重新处理
///
/// 如果没有边界标记，整个 content 作为一个缓存块。
/// 对不支持显式缓存的模型（GPT/DeepSeek），OpenRouter 会自动忽略 cache_control 字段。
///
/// Anthropic 限制最多 4 个 cache_control 块，超出会返回 400 错误。
#[allow(dead_code)]
fn inject_cache_control(messages: &[Value]) -> Vec<Value> {
    const CACHE_BOUNDARY: &str = "[[CACHE_BOUNDARY]]";
    const MAX_CACHE_BLOCKS: usize = 4;

    let mut cache_blocks_used: usize = 0;
    let mut system_seen = false;

    messages.iter().map(|msg| {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role != "system" {
            return msg.clone();
        }

        // 只对第一条 system 消息注入 cache_control。
        // 后续 system 消息（如 STATE_VIEW、nudge 等）是动态内容，
        // 加 cache_control 会导致缓存 key 每轮变化，反而破坏缓存命中。
        if system_seen {
            // 清除 CACHE_BOUNDARY 标记但不加 cache_control
            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.contains(CACHE_BOUNDARY) {
                let mut new_msg = msg.clone();
                new_msg["content"] = Value::String(content.replace(CACHE_BOUNDARY, ""));
                return new_msg;
            }
            return msg.clone();
        }
        system_seen = true;

        let content = match msg.get("content") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => return msg.clone(),
        };

        if cache_blocks_used >= MAX_CACHE_BLOCKS {
            return msg.clone();
        }

        let mut new_msg = msg.clone();

        if let Some(boundary_pos) = content.find(CACHE_BOUNDARY) {
            let static_part = content[..boundary_pos].trim_end().to_string();
            let dynamic_part = content[boundary_pos + CACHE_BOUNDARY.len()..].trim_start().to_string();

            let mut blocks = vec![
                json!({
                    "type": "text",
                    "text": static_part,
                    "cache_control": { "type": "ephemeral" }
                })
            ];
            cache_blocks_used += 1;
            if !dynamic_part.is_empty() {
                blocks.push(json!({
                    "type": "text",
                    "text": dynamic_part
                }));
            }
            new_msg["content"] = Value::Array(blocks);
        } else {
            new_msg["content"] = json!([
                {
                    "type": "text",
                    "text": content,
                    "cache_control": { "type": "ephemeral" }
                }
            ]);
            cache_blocks_used += 1;
        }

        new_msg
    }).collect()
}

/// 不支持 cache 的模型：从 system message 中删除 [[CACHE_BOUNDARY]] 标记。
fn strip_cache_boundary(messages: &[Value]) -> Vec<Value> {
    messages.iter().map(|msg| {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role != "system" {
            return msg.clone();
        }
        if let Some(Value::String(content)) = msg.get("content") {
            if content.contains("[[CACHE_BOUNDARY]]") {
                let mut new_msg = msg.clone();
                new_msg["content"] = Value::String(content.replace("[[CACHE_BOUNDARY]]", ""));
                return new_msg;
            }
        }
        msg.clone()
    }).collect()
}

/// 清洗消息列表，避免 LLM API 返回 400 错误。
///
/// 处理规则：
/// 1. 只保留已知的消息字段（role、content、tool_calls、tool_call_id、name）
/// 2. 空字符串 content 的处理：
///    - 带 tool_calls 的 assistant 消息：content 设为 null（API 要求）
///    - 其他消息：content 设为 "(empty)"（避免空字符串被拒绝）
/// 3. 确保 assistant 消息始终包含 content 字段（即使为 null）
fn sanitize_messages(messages: &[HashMap<String, Value>]) -> Vec<Value> {
    messages
        .iter()
        .map(|msg| {
            let mut clean: serde_json::Map<String, Value> = serde_json::Map::new();
            for (k, v) in msg {
                // 只保留已知的消息字段，过滤掉未知字段
                if matches!(
                    k.as_str(),
                    "role" | "content" | "tool_calls" | "tool_call_id" | "name" | "reasoning"
                ) {
                    let val = if k == "content" {
                        match v {
                            // 多模态 content array —— 过滤掉损坏的图片部分，保留有效内容
                            Value::Array(arr) => {
                                let filtered: Vec<Value> = arr.iter().filter(|part| {
                                    let pt = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                    if pt == "image_url" {
                                        // 旧格式 image_url：检查是否为有效图片 data URI
                                        let url = part.pointer("/image_url/url")
                                            .and_then(|v| v.as_str()).unwrap_or("");
                                        let valid = url.starts_with("data:image/");
                                        if !valid {
                                            info!(
                                                url_prefix = &url[..url.len().min(60)],
                                                "sanitize: dropping invalid image_url part"
                                            );
                                        }
                                        valid
                                    } else if pt == "image" {
                                        // Anthropic 原生格式：检查 media_type 是否有效
                                        let mt = part.pointer("/source/media_type")
                                            .and_then(|v| v.as_str()).unwrap_or("");
                                        let valid = matches!(mt, "image/jpeg" | "image/png" | "image/gif" | "image/webp");
                                        if !valid {
                                            info!(
                                                media_type = mt,
                                                "sanitize: dropping invalid image part"
                                            );
                                        }
                                        valid
                                    } else {
                                        true // text 等其他部分保留
                                    }
                                }).cloned().collect();
                                // 如果过滤后只剩文本，提取为简单字符串；如果全空则替换为 "(empty)"
                                if filtered.is_empty() {
                                    Value::String("(empty)".into())
                                } else if filtered.len() == 1 && filtered[0].get("type").and_then(|v| v.as_str()) == Some("text") {
                                    filtered[0].get("text").cloned().unwrap_or(Value::String("(empty)".into()))
                                } else {
                                    Value::Array(filtered)
                                }
                            },
                            // 空字符串 content 的特殊处理
                            Value::String(s) if s.is_empty() => {
                                if msg.get("role").and_then(|r| r.as_str()) == Some("assistant")
                                    && msg.contains_key("tool_calls")
                                {
                                    // assistant + tool_calls 时，content 必须为 null
                                    Value::Null
                                } else {
                                    // 其他角色的空 content 替换为 "(empty)"
                                    Value::String("(empty)".into())
                                }
                            }
                            // 其他情况原样保留
                            _ => v.clone(),
                        }
                    } else {
                        v.clone()
                    };
                    clean.insert(k.clone(), val);
                }
            }
            // 确保 assistant 消息始终有 content 字段
            if clean.get("role").and_then(|v| v.as_str()) == Some("assistant")
                && !clean.contains_key("content")
            {
                clean.insert("content".into(), Value::Null);
            }
            Value::Object(clean)
        })
        .collect()
}

/// 确保每个 assistant 的 tool_call 都有对应的 tool result 消息。
///
/// overlay / 压缩可能导致 tool result 被截断而 tool_call 仍在，
/// OpenAI/Azure 会直接 400 拒绝。此函数作为发送前的最终兜底：
/// - 收集所有 assistant tool_call id
/// - 收集所有 tool 消息的 tool_call_id
/// - 对缺失 result 的 tool_call，补一条占位 tool 消息
/// - 对找不到 tool_call 的孤立 tool 消息，直接移除
/// 确保 tool_call / tool_result 严格配对，修复所有已知的配对问题。
///
/// 设计原则：**简单暴力，宁可丢信息也不发坏数据**。
/// 不做复杂的"猜测修补"，而是按三条铁律过滤：
///
/// 1. 每个 tool_call id 全局唯一（重复的只保留第一组）
/// 2. 每个 tool_result 必须有对应的 tool_call（孤立的直接丢）
/// 3. 每个 tool_call 必须有对应的 tool_result（缺失的补占位符）
///
/// 最后确保消息不以 assistant 结尾（Anthropic 要求）。
fn ensure_tool_call_pairs(messages: Vec<Value>) -> Vec<Value> {
    use std::collections::{HashMap, HashSet};

    let mut output: Vec<Value> = Vec::with_capacity(messages.len());

    // 全局已出现的 tool_call id（用于检测跨轮重复）
    let mut global_call_ids: HashSet<String> = HashSet::new();
    // 当前 assistant 消息声明的 tool_call ids（等待配对）
    let mut pending_call_ids: Vec<(String, String)> = Vec::new(); // (id, function_name)
    // 当前 assistant 消息已配对的 tool_result ids
    let mut matched_result_ids: HashSet<String> = HashSet::new();
    // 重复 id 的重映射表（原 id → 新唯一 id），确保 assistant 和 tool_result 同步
    let mut id_remap: HashMap<String, String> = HashMap::new();
    let mut dedup_counter: usize = 0;

    let mut fixes = 0usize;

    for msg in messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");

        match role {
            "assistant" => {
                // 先把上一个 assistant 的未配对 tool_calls 补占位符
                flush_pending(&mut output, &pending_call_ids, &matched_result_ids, &mut fixes);
                pending_call_ids.clear();
                matched_result_ids.clear();
                // 当前 assistant 的 id 重映射表：原 id → 新 id
                id_remap.clear();

                if let Some(Value::Array(tcs)) = msg.get("tool_calls") {
                    let mut patched_tcs: Vec<Value> = Vec::new();
                    for tc in tcs {
                        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                            let fname = tc.pointer("/function/name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            // 如果 id 全局重复，分配新 id
                            let actual_id = if global_call_ids.contains(id) {
                                dedup_counter += 1;
                                let new_id = format!("{id}_d{dedup_counter}");
                                id_remap.insert(id.to_string(), new_id.clone());
                                fixes += 1;
                                new_id
                            } else {
                                id.to_string()
                            };
                            global_call_ids.insert(actual_id.clone());
                            pending_call_ids.push((actual_id.clone(), fname.to_string()));

                            // 构建 patched tool_call
                            let mut tc_clone = tc.clone();
                            if let Some(obj) = tc_clone.as_object_mut() {
                                obj.insert("id".into(), Value::String(actual_id));
                            }
                            patched_tcs.push(tc_clone);
                        }
                    }

                    if patched_tcs.is_empty() {
                        // 无有效 tool_calls，保留 content 部分
                        let mut cleaned = msg.as_object().cloned().unwrap_or_default();
                        cleaned.remove("tool_calls");
                        if cleaned.get("content").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false) {
                            output.push(Value::Object(cleaned));
                        }
                    } else {
                        let mut cleaned = msg.as_object().cloned().unwrap_or_default();
                        cleaned.insert("tool_calls".into(), Value::Array(patched_tcs));
                        output.push(Value::Object(cleaned));
                    }
                } else {
                    output.push(msg);
                }
            }

            "tool" => {
                if let Some(tcid) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
                    // 如果这个 id 被 remap 过，用新 id
                    let actual_id = id_remap.get(tcid).cloned().unwrap_or_else(|| tcid.to_string());
                    let is_expected = pending_call_ids.iter().any(|(id, _)| *id == actual_id);
                    let already_matched = matched_result_ids.contains(&actual_id);

                    if is_expected && !already_matched {
                        matched_result_ids.insert(actual_id.clone());
                        // 如果 id 被 remap 了，更新 tool_result 的 tool_call_id
                        if actual_id != tcid {
                            let mut patched = msg.as_object().cloned().unwrap_or_default();
                            patched.insert("tool_call_id".into(), Value::String(actual_id));
                            output.push(Value::Object(patched));
                        } else {
                            output.push(msg);
                        }
                    } else {
                        fixes += 1; // 孤立 or 重复，丢弃
                    }
                } else {
                    output.push(msg);
                }
            }

            _ => {
                // user / system 消息：先 flush 上一个 assistant 的未配对 tool_calls
                flush_pending(&mut output, &pending_call_ids, &matched_result_ids, &mut fixes);
                pending_call_ids.clear();
                matched_result_ids.clear();
                output.push(msg);
            }
        }
    }

    // 最后一个 assistant 的未配对 tool_calls
    flush_pending(&mut output, &pending_call_ids, &matched_result_ids, &mut fixes);

    // Anthropic 要求消息必须以 user 或 tool 结尾
    if let Some(last) = output.last() {
        let last_role = last.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if last_role == "assistant" {
            output.push(json!({"role": "user", "content": "continue"}));
            fixes += 1;
        }
    }

    if fixes > 0 {
        warn!(fixes, "ensure_tool_call_pairs applied fixes");
    }

    output
}

/// 为 pending_call_ids 中未配对的 tool_call 补占位 tool_result。
fn flush_pending(
    output: &mut Vec<Value>,
    pending: &[(String, String)],
    matched: &std::collections::HashSet<String>,
    fixes: &mut usize,
) {
    for (id, fname) in pending {
        if !matched.contains(id) {
            output.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "name": fname,
                "content": "(result unavailable)"
            }));
            *fixes += 1;
        }
    }
}

fn should_write_llm_request_snapshots() -> bool {
    std::env::var("TYCLAW_WRITE_LLM_REQUEST_SNAPSHOTS")
        .ok()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn sanitize_filename_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(80));
    for ch in input.chars() {
        if out.len() >= 80 {
            break;
        }
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

fn short_scope_hash(input: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

fn snapshot_scope_dir(scope: &str) -> PathBuf {
    if let Some(rest) = scope.strip_prefix("session:") {
        return PathBuf::from("session").join(sanitize_filename_component(rest));
    }
    if let Some(rest) = scope.strip_prefix("dispatch:") {
        let mut parts = rest.split(":node:");
        let dispatch_id = parts.next().unwrap_or("dispatch");
        let dispatch_dir = PathBuf::from("dispatch").join(sanitize_filename_component(dispatch_id));
        if let Some(node_id) = parts.next() {
            return dispatch_dir.join("subagents").join(sanitize_filename_component(node_id));
        }
        return dispatch_dir;
    }
    PathBuf::from("scopes").join(format!("scope-{}", short_scope_hash(scope)))
}

/// 把消息 content 统一为 canonical 格式，消除 string/array 不一致问题。
///
/// - system/user/assistant 的 string content → `[{"type":"text","text":"..."}]`
/// - tool 消息的 content 保持 string（OpenAI 格式要求，relay 不认 array）
/// - null → 不变（assistant with tool_calls）
/// - 已经是 array → strip cache_control，保持结构
///
/// **重要**：assistant 和 tool 消息保持 string 格式（不转 array block）。
/// 原因：cache_control 加在 array block 上会改变 JSON 字节，移走后字节不同，
/// 破坏 Anthropic prompt cache 的前缀匹配。保持 string 格式后，
/// Anthropic 视 string 和 array block 为等价，CC 移走不影响前缀。
/// 只有 system/user 消息转为 array block（system 需要 CC，user context 不变）。
fn normalize_to_blocks(messages: &[Value]) -> Vec<Value> {
    messages.iter().map(|msg| {
        let mut m = msg.clone();
        strip_cache_control_fields(&mut m);
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");

        // tool 消息：必须保持 string（OpenAI 格式要求）
        if role == "tool" {
            if let Some(Value::Array(blocks)) = m.get("content").cloned() {
                // 合并所有 text block，避免多 block 时只取第一个丢内容
                let text = blocks.iter()
                    .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n");
                m["content"] = Value::String(text);
            }
            return m;
        }

        // assistant 消息：保持 string 格式，避免 CC 增删改变字节破坏缓存前缀。
        // null content（tool_calls only）转为空 string。
        // 已经是 array block（之前加了 CC 的）还原为 string。
        if role == "assistant" {
            match m.get("content").cloned() {
                Some(Value::Null) | None => {
                    m["content"] = Value::String(String::new());
                }
                Some(Value::Array(blocks)) => {
                    // array block → 提取文本还原为 string
                    let text = blocks.iter()
                        .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
                        .collect::<Vec<_>>()
                        .join("");
                    m["content"] = Value::String(text);
                }
                Some(Value::String(_)) => {} // 已经是 string，保持
                _ => {}
            }
            return m;
        }

        // system / user 消息：转为 array block（system 需要加 CC）
        if let Some(content) = m.get("content").cloned() {
            match content {
                Value::String(s) => {
                    m["content"] = json!([{"type": "text", "text": s}]);
                }
                Value::Array(_) => {} // 已经是 array，strip_cache_control_fields 已处理
                Value::Null => {
                    m["content"] = json!([{"type": "text", "text": ""}]);
                }
                _ => {}
            }
        }
        m
    }).collect()
}

/// 对 system 消息加 cache_control。
///
/// 去掉 `[[CACHE_BOUNDARY]]` 标记（历史遗留，动态内容已移至独立消息），
/// 将整个 system 合并为单个 content block + cache_control。
/// 多 block system 也合并——避免中间 breakpoint 导致后续 messages 缓存扩展失效。
fn apply_system_cache_control(msg: &mut Value) {
    let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
    if role != "system" { return; }

    const CACHE_BOUNDARY: &str = "[[CACHE_BOUNDARY]]";

    // 提取所有 text block 的内容，合并为一个
    let full_text = match msg.get("content") {
        Some(Value::Array(blocks)) => {
            blocks.iter()
                .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        }
        Some(Value::String(s)) => s.clone(),
        _ => return,
    };

    // 去掉 CACHE_BOUNDARY 标记
    let clean_text = full_text.replace(CACHE_BOUNDARY, "");

    msg["content"] = json!([{
        "type": "text",
        "text": clean_text.trim(),
        "cache_control": {"type": "ephemeral"}
    }]);
}

fn first_payload_diff(prev: &[Value], next: &[Value]) -> (Option<usize>, &'static str) {
    let shared = prev.len().min(next.len());
    for idx in 0..shared {
        if prev[idx] == next[idx] {
            continue;
        }
        let mut prev_stripped = prev[idx].clone();
        let mut next_stripped = next[idx].clone();
        strip_cache_control_fields(&mut prev_stripped);
        strip_cache_control_fields(&mut next_stripped);
        if prev_stripped == next_stripped {
            return (Some(idx), "cache_control_only");
        }
        return (Some(idx), "content_changed");
    }
    if prev.len() == next.len() {
        (None, "identical")
    } else {
        (Some(shared), "length_changed")
    }
}

fn response_provider(data: &Value) -> Option<String> {
    data.get("provider")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

fn openrouter_provider_preferences(api_base: &str, model: &str) -> Option<Value> {
    if !supports_openrouter_provider_routing(api_base) {
        return None;
    }
    if !model.contains("claude") {
        return None;
    }
    Some(json!({
        "only": ["anthropic"],
        "allow_fallbacks": false,
        "require_parameters": true
    }))
}

fn supports_openrouter_provider_routing(api_base: &str) -> bool {
    api_base.contains("openrouter.ai") || api_base.contains("relay.tuyoo.com")
}

/// 计算两个 canonical 消息列表的公共前缀长度。
/// 两边都是 canonical 格式（array block，无 cache_control），直接对比。
fn common_prefix_len(a: &[Value], b: &[Value]) -> usize {
    a.iter()
        .zip(b.iter())
        .take_while(|(x, y)| {
            // canonical 格式下直接对比即可（都是 array block，无 cache_control）
            // 但为安全起见仍 strip cache_control（overlay 后的消息可能残留）
            let mut xc = (*x).clone();
            let mut yc = (*y).clone();
            strip_cache_control_fields(&mut xc);
            strip_cache_control_fields(&mut yc);
            xc == yc
        })
        .count()
}

/// 从 Value 中递归移除 cache_control 字段。
fn strip_cache_control_fields(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.remove("cache_control");
            for val in map.values_mut() {
                strip_cache_control_fields(val);
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                strip_cache_control_fields(item);
            }
        }
        _ => {}
    }
}

/// 在历史消息（assistant/tool）上加 cache_control，推进缓存边界。
fn add_history_cache_breakpoint(msg: &mut Value) {
    if let Some(content) = msg.get("content").cloned() {
        match content {
            Value::String(s) => {
                msg["content"] = json!([{
                    "type": "text",
                    "text": s,
                    "cache_control": { "type": "ephemeral" }
                }]);
            }
            Value::Array(mut blocks) => {
                if let Some(last) = blocks.last_mut() {
                    if let Some(obj) = last.as_object_mut() {
                        obj.insert(
                            "cache_control".to_string(),
                            json!({"type": "ephemeral"}),
                        );
                    }
                }
                msg["content"] = Value::Array(blocks);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_and_commit(provider: &OpenAICompatProvider, request: &ChatRequest) -> Value {
        let prepared = provider.prepare_body(request);
        if let Some(plan) = prepared.commit_plan.clone() {
            provider.commit_cache_plan(plan);
        }
        prepared.body
    }

    fn content_has_cache_control(msg: &Value) -> bool {
        msg.get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .any(|block| block.get("cache_control").is_some())
            })
            .unwrap_or(false)
    }

    /// 测试：空字符串 content 应被替换为 "(empty)"
    #[test]
    fn test_sanitize_empty_content() {
        let msg = {
            let mut m = HashMap::new();
            m.insert("role".into(), json!("user"));
            m.insert("content".into(), json!(""));
            m
        };
        let result = sanitize_messages(&[msg]);
        assert_eq!(result[0]["content"], json!("(empty)"));
    }

    /// 测试：带 tool_calls 的 assistant 消息，空 content 应设为 null
    #[test]
    fn test_sanitize_assistant_with_tool_calls() {
        let msg = {
            let mut m = HashMap::new();
            m.insert("role".into(), json!("assistant"));
            m.insert("content".into(), json!(""));
            m.insert("tool_calls".into(), json!([{"id": "1"}]));
            m
        };
        let result = sanitize_messages(&[msg]);
        assert!(result[0]["content"].is_null());
    }

    /// 测试：正常响应的解析
    #[test]
    fn test_parse_response() {
        let provider = OpenAICompatProvider::new("key", "http://localhost", "test-model", None);
        let data = json!({
            "choices": [{
                "message": {
                    "content": "Hello!",
                    "role": "assistant"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });
        let resp = provider.parse_response(data).unwrap();
        assert_eq!(resp.content.as_deref(), Some("Hello!"));
        assert_eq!(resp.finish_reason, "stop");
        assert!(!resp.has_tool_calls());
        assert_eq!(resp.usage["total_tokens"], 15);
    }

    /// 测试：包含工具调用的响应解析
    #[test]
    fn test_parse_tool_calls() {
        let provider = OpenAICompatProvider::new("key", "http://localhost", "test-model", None);
        let data = json!({
            "choices": [{
                "message": {
                    "content": null,
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "tc_1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\": \"/tmp/test.txt\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let resp = provider.parse_response(data).unwrap();
        assert!(resp.has_tool_calls());
        assert_eq!(resp.tool_calls[0].name, "read_file");
        assert_eq!(resp.tool_calls[0].arguments["path"], "/tmp/test.txt");
    }

    #[test]
    fn test_openrouter_claude_adds_anthropic_provider_preferences() {
        let provider = OpenAICompatProvider::new(
            "key",
            "https://openrouter.ai/api/v1",
            "openai/claude-opus-4.6",
            None,
        );
        let mut msg = HashMap::new();
        msg.insert("role".into(), json!("user"));
        msg.insert("content".into(), json!("hello"));
        let req = ChatRequest {
            messages: vec![msg],
            tools: None,
            model: None,
            cache_scope: None,
            max_tokens: 100,
            temperature: 0.0,
        };
        let prepared = provider.prepare_body(&req);
        assert_eq!(prepared.body["provider"]["only"], json!(["anthropic"]));
        assert_eq!(prepared.body["provider"]["allow_fallbacks"], json!(false));
        assert_eq!(prepared.body["provider"]["require_parameters"], json!(true));
    }

    #[test]
    fn test_non_openrouter_request_does_not_add_provider_preferences() {
        let provider = OpenAICompatProvider::new(
            "key",
            "https://api.example.com/v1",
            "openai/claude-opus-4.6",
            None,
        );
        let mut msg = HashMap::new();
        msg.insert("role".into(), json!("user"));
        msg.insert("content".into(), json!("hello"));
        let req = ChatRequest {
            messages: vec![msg],
            tools: None,
            model: None,
            cache_scope: None,
            max_tokens: 100,
            temperature: 0.0,
        };
        let prepared = provider.prepare_body(&req);
        assert!(prepared.body.get("provider").is_none());
    }

    #[test]
    fn test_relay_tuyoo_uses_openrouter_provider_preferences() {
        let provider = OpenAICompatProvider::new(
            "key",
            "https://relay.tuyoo.com/v1",
            "openai/claude-opus-4.6",
            None,
        );
        let mut msg = HashMap::new();
        msg.insert("role".into(), json!("user"));
        msg.insert("content".into(), json!("hello"));
        let req = ChatRequest {
            messages: vec![msg],
            tools: None,
            model: None,
            cache_scope: None,
            max_tokens: 100,
            temperature: 0.0,
        };
        let prepared = provider.prepare_body(&req);
        assert_eq!(prepared.body["provider"]["only"], json!(["anthropic"]));
    }

    #[test]
    fn test_session_scope_uses_top_level_cache_markers() {
        let provider = OpenAICompatProvider::new(
            "key",
            "https://relay.tuyoo.com/v1",
            "openai/claude-opus-4.6",
            None,
        );
        let mut system = HashMap::new();
        system.insert("role".into(), json!("system"));
        system.insert("content".into(), json!("static[[CACHE_BOUNDARY]]dynamic"));
        let mut user = HashMap::new();
        user.insert("role".into(), json!("user"));
        user.insert("content".into(), json!("hello"));
        let req = ChatRequest {
            messages: vec![system, user],
            tools: Some(vec![json!({
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read file",
                    "parameters": {"type": "object", "properties": {}}
                }
            })]),
            model: None,
            cache_scope: Some("session:default:cli:direct".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        let prepared = provider.prepare_body(&req);
        let body_text = serde_json::to_string(&prepared.body).unwrap();
        assert!(body_text.contains("cache_control"));
        assert_eq!(prepared.body["provider"]["only"], json!(["anthropic"]));
        // CACHE_BOUNDARY 被去掉，system 合并为一个 block
        assert_eq!(prepared.body["messages"][0]["content"][0]["text"], json!("staticdynamic"));
    }

    #[test]
    fn test_dispatch_scope_uses_only_top_level_cache_markers() {
        let provider = OpenAICompatProvider::new(
            "key",
            "https://relay.tuyoo.com/v1",
            "openai/claude-opus-4.6",
            None,
        );
        let mut system = HashMap::new();
        system.insert("role".into(), json!("system"));
        system.insert("content".into(), json!("static[[CACHE_BOUNDARY]]dynamic"));
        let mut user = HashMap::new();
        user.insert("role".into(), json!("user"));
        user.insert("content".into(), json!("hello"));
        let req = ChatRequest {
            messages: vec![system, user],
            tools: Some(vec![json!({
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read file",
                    "parameters": {"type": "object", "properties": {}}
                }
            })]),
            model: None,
            cache_scope: Some("dispatch:demo:node:coding".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        let prepared = provider.prepare_body(&req);
        let body_text = serde_json::to_string(&prepared.body).unwrap();
        assert!(body_text.contains("cache_control"));
        let messages = prepared.body["messages"].as_array().unwrap();
        assert!(content_has_cache_control(&messages[0]));
        assert!(!content_has_cache_control(&messages[1]));
    }

    #[test]
    fn test_sanitize_filename_component() {
        assert_eq!(
            sanitize_filename_component("anthropic/claude-opus-4.6"),
            "anthropic_claude-opus-4.6"
        );
        assert_eq!(sanitize_filename_component(""), "unknown");
    }

    #[test]
    fn test_short_scope_hash_is_stable_and_short() {
        let a = short_scope_hash("workspace:cli:direct");
        let b = short_scope_hash("workspace:cli:direct");
        let c = short_scope_hash("dispatch:abc:node:x");
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
        assert_ne!(a, c);
    }

    /// 测试：prefix overlay 确保压缩差异不影响 cache prefix
    ///
    /// 模拟场景：
    /// - 请求 1：3 条消息，msg[1] content = "original"
    /// - 请求 2：5 条消息，msg[1] content = "compressed"（模拟压缩后内容变了）
    /// - build_body 应该用请求 1 的 msg[1] 覆盖请求 2 的 msg[1]
    /// - 最终 messages_final 里 msg[1] content 应为 "original"
    #[test]
    fn test_prefix_overlay_preserves_previous_content() {
        let provider = OpenAICompatProvider::new(
            "key", "http://localhost", "claude-test", None,
        );

        let make_msg = |role: &str, content: &str| -> HashMap<String, Value> {
            let mut m = HashMap::new();
            m.insert("role".into(), json!(role));
            m.insert("content".into(), json!(content));
            m
        };

        // 请求 1：建立 last_sanitized
        let req1 = ChatRequest {
            messages: vec![
                make_msg("system", "You are helpful."),
                make_msg("user", "Hello"),
                make_msg("assistant", "Hi there!"),
            ],
            tools: None,
            model: None,
            cache_scope: Some("test-scope".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        // helper: 从 canonical array block 格式提取文本
        let get_text = |msgs: &[Value], idx: usize| -> String {
            let content = &msgs[idx]["content"];
            if let Some(arr) = content.as_array() {
                arr.iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>().join("")
            } else {
                content.as_str().unwrap_or("").to_string()
            }
        };

        let body1 = build_and_commit(&provider, &req1);
        let msgs1 = body1["messages"].as_array().unwrap();
        assert_eq!(get_text(msgs1, 1), "Hello");

        // 请求 2：msg[1] content 变了（模拟压缩差异），但 msg[0-1] 在 prefix 内
        let req2 = ChatRequest {
            messages: vec![
                make_msg("system", "You are helpful."),
                make_msg("user", "Hello_compressed"), // 内容变了
                make_msg("assistant", "Hi there!"),
                make_msg("user", "What is Python?"),
                make_msg("assistant", "Python is great."),
            ],
            tools: None,
            model: None,
            cache_scope: Some("test-scope".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        let body2 = build_and_commit(&provider, &req2);
        let msgs2 = body2["messages"].as_array().unwrap();

        // common_prefix_len 基于 sanitized 对比，msg[1] 不同 → prefix_len=1
        // 所以 overlay 不会覆盖 msg[1]（因为 prefix 只到 msg[0]）
        // msg[1] 保持 "Hello_compressed"
        // overlay 把 msg[1] 恢复为上次的 "Hello"（消除压缩差异）
        assert_eq!(get_text(msgs2, 1), "Hello");

        // 请求 3：msg[1] content 和请求 2 一样（压缩一致），msg[2] 不同
        let req3 = ChatRequest {
            messages: vec![
                make_msg("system", "You are helpful."),
                make_msg("user", "Hello_compressed"),
                make_msg("assistant", "Hi there!_v2"), // 这条变了
                make_msg("user", "What is Python?"),
                make_msg("assistant", "Python is great."),
                make_msg("user", "More details"),
            ],
            tools: None,
            model: None,
            cache_scope: Some("test-scope".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        let body3 = build_and_commit(&provider, &req3);
        let msgs3 = body3["messages"].as_array().unwrap();

        // overlay 先用 req2 的 canonical 覆盖 msg[0-4]（req2 有 5 条消息，覆盖 4 条）
        // msg[1] 被恢复为 req2 存的 "Hello"（req2 的 overlay 已恢复过）
        assert_eq!(get_text(msgs3, 1), "Hello");
    }

    /// 测试：prefix overlay 在压缩 age 变化时保持稳定
    ///
    /// 模拟场景：
    /// - 请求 1：msg[2] = tool result "long content"（未被压缩，age < FRESH）
    /// - 请求 2：多了新消息后 msg[2] 的 age 增大，被压缩成 "short"
    /// - overlay 应该把 msg[2] 恢复为请求 1 的版本（"long content"）
    #[test]
    fn test_prefix_overlay_handles_compression_age_change() {
        let provider = OpenAICompatProvider::new(
            "key", "http://localhost", "claude-test", None,
        );

        let make_msg = |role: &str, content: &str| -> HashMap<String, Value> {
            let mut m = HashMap::new();
            m.insert("role".into(), json!(role));
            m.insert("content".into(), json!(content));
            m
        };

        // 请求 1：建立 baseline
        let req1 = ChatRequest {
            messages: vec![
                make_msg("system", "System prompt."),
                make_msg("user", "task"),
                make_msg("assistant", "thinking"),
                make_msg("tool", "long tool result with lots of data"),
                make_msg("user", "next"),
            ],
            tools: None,
            model: None,
            cache_scope: Some("age-test".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        build_and_commit(&provider, &req1);

        // 请求 2：msg[3] 被压缩成 "[summary]"（模拟 age 增大）
        let req2 = ChatRequest {
            messages: vec![
                make_msg("system", "System prompt."),
                make_msg("user", "task"),
                make_msg("assistant", "thinking"),
                make_msg("tool", "[summary]"), // 压缩后的内容
                make_msg("user", "next"),
                make_msg("assistant", "response"),
                make_msg("user", "follow up"),
            ],
            tools: None,
            model: None,
            cache_scope: Some("age-test".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        let body2 = build_and_commit(&provider, &req2);
        let msgs2 = body2["messages"].as_array().unwrap();

        // common_prefix_len: msg[0-2] 相同，msg[3] 不同 → prefix_len=3
        // overlay 覆盖 msg[1-2] (不覆盖 system msg[0])
        // msg[3] 不在 prefix 内，保持 "[summary]"
        // overlay 覆盖 msg[0-3]（req1 有 5 条消息，覆盖 4 条）
        // msg[3] 被恢复为 req1 的 "long tool result with lots of data"
        assert_eq!(msgs2[3]["content"], json!("long tool result with lots of data"));

        // 请求 3：msg[3] 又被压缩成 "[even shorter]"
        let req3 = ChatRequest {
            messages: vec![
                make_msg("system", "System prompt."),
                make_msg("user", "task"),
                make_msg("assistant", "thinking"),
                make_msg("tool", "[even shorter]"), // 进一步压缩
                make_msg("user", "next"),
                make_msg("assistant", "response"),
                make_msg("user", "follow up"),
                make_msg("assistant", "more"),
                make_msg("user", "done"),
            ],
            tools: None,
            model: None,
            cache_scope: Some("age-test".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        let body3 = build_and_commit(&provider, &req3);
        let msgs3 = body3["messages"].as_array().unwrap();

        // overlay 先覆盖 msg[0-6]（req2 有 7 条消息），msg[3] 被恢复为上次存的值
        assert_eq!(msgs3[3]["content"], json!("long tool result with lots of data"));

        // 但如果 req3 的 msg[3] 和 req2 一样（"[summary]"），prefix 就是 7
        let req3b = ChatRequest {
            messages: vec![
                make_msg("system", "System prompt."),
                make_msg("user", "task"),
                make_msg("assistant", "thinking"),
                make_msg("tool", "[summary]"), // 和 req2 相同
                make_msg("user", "next"),
                make_msg("assistant", "response"),
                make_msg("user", "follow up"),
                make_msg("assistant", "more"),
                make_msg("user", "done"),
            ],
            tools: None,
            model: None,
            cache_scope: Some("age-test".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        let body3b = build_and_commit(&provider, &req3b);
        let msgs3b = body3b["messages"].as_array().unwrap();

        // overlay 覆盖范围 = req3 的 msg count(9) - 1 = 8，覆盖 msg[0-7]
        // msg[3] 被恢复为 canonical 链的原始值 "long tool result..."
        assert_eq!(msgs3b[3]["content"], json!("long tool result with lots of data"));
    }

    #[test]
    fn test_common_prefix_len_ignores_cache_control() {
        let a = vec![json!({
            "role": "system",
            "content": [{
                "type": "text",
                "text": "hello",
                "cache_control": {"type": "ephemeral"}
            }]
        })];
        let b = vec![json!({
            "role": "system",
            "content": [{
                "type": "text",
                "text": "hello"
            }]
        })];
        assert_eq!(common_prefix_len(&a, &b), 1);
    }

    /// 核心测试：canonical 格式（array block）对比，cache_control 被忽略
    #[test]
    fn test_common_prefix_len_canonical_format() {
        // 两边都是 canonical 格式（array block）
        let a = vec![
            json!({"role": "system", "content": [{"type": "text", "text": "system prompt"}]}),
            json!({"role": "user", "content": [{"type": "text", "text": "hello"}]}),
            json!({"role": "assistant", "content": [{"type": "text", "text": "world"}]}),
        ];
        // 带 cache_control 的版本（只多一个字段，结构不变）
        let b = vec![
            json!({"role": "system", "content": [{"type": "text", "text": "system prompt"}]}),
            json!({"role": "user", "content": [{"type": "text", "text": "hello", "cache_control": {"type": "ephemeral"}}]}),
            json!({"role": "assistant", "content": [{"type": "text", "text": "world"}]}),
        ];
        assert_eq!(common_prefix_len(&a, &b), 3);

        // 内容不同
        let c = vec![
            json!({"role": "system", "content": [{"type": "text", "text": "system prompt"}]}),
            json!({"role": "user", "content": [{"type": "text", "text": "different"}]}),
        ];
        assert_eq!(common_prefix_len(&a, &c), 1);
    }

    /// 测试：normalize_to_blocks 统一格式
    #[test]
    fn test_normalize_to_blocks() {
        let msgs = vec![
            json!({"role": "system", "content": "system prompt"}),
            json!({"role": "user", "content": "hello"}),
            json!({"role": "assistant", "content": null, "tool_calls": [{"id": "1"}]}),
            json!({"role": "tool", "content": "result"}),
        ];
        let normalized = normalize_to_blocks(&msgs);
        // string → array block
        assert_eq!(normalized[0]["content"], json!([{"type": "text", "text": "system prompt"}]));
        assert_eq!(normalized[1]["content"], json!([{"type": "text", "text": "hello"}]));
        // assistant null → 空 string（保持 string 格式，避免 CC 增删改变字节）
        assert_eq!(normalized[2]["content"], json!(""));
        // tool content 保持 string（OpenAI 格式要求）
        assert_eq!(normalized[3]["content"], json!("result"));
    }

    #[test]
    fn test_history_messages_never_get_cache_control() {
        let provider = OpenAICompatProvider::new(
            "key", "https://relay.tuyoo.com/v1", "openai/claude-opus-4.6", None,
        );

        let make_msg = |role: &str, content: &str| -> HashMap<String, Value> {
            let mut m = HashMap::new();
            m.insert("role".into(), json!(role));
            m.insert("content".into(), json!(content));
            m
        };

        let req1 = ChatRequest {
            messages: vec![
                make_msg("system", "System prompt."),
                make_msg("user", "Goal"),
                make_msg("assistant", "Plan A"),
                make_msg("user", "Continue"),
            ],
            tools: None,
            model: None,
            cache_scope: Some("marker-growth".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        let body1 = build_and_commit(&provider, &req1);
        let msgs1 = body1["messages"].as_array().unwrap();
        assert!(content_has_cache_control(&msgs1[0]));  // system CC
        assert!(!content_has_cache_control(&msgs1[1])); // user: no CC
        assert!(content_has_cache_control(&msgs1[2]));  // last assistant with text: CC

        let req2 = ChatRequest {
            messages: vec![
                make_msg("system", "System prompt."),
                make_msg("user", "Goal"),
                make_msg("assistant", "Plan A"),
                make_msg("user", "Continue"),
                make_msg("assistant", "Plan B"),
                make_msg("user", "More"),
            ],
            tools: None,
            model: None,
            cache_scope: Some("marker-growth".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        let body2 = build_and_commit(&provider, &req2);
        let msgs2 = body2["messages"].as_array().unwrap();
        assert!(content_has_cache_control(&msgs2[0]));  // system CC
        assert!(!content_has_cache_control(&msgs2[1])); // user: no CC
        assert!(!content_has_cache_control(&msgs2[2])); // old assistant: no CC (moved)
        assert!(!content_has_cache_control(&msgs2[3])); // user: no CC
        assert!(content_has_cache_control(&msgs2[4]));  // last assistant with text: CC
        assert!(!content_has_cache_control(&msgs2[5])); // user: no CC

        let req3 = ChatRequest {
            messages: vec![
                make_msg("system", "System prompt."),
                make_msg("user", "Goal"),
                make_msg("assistant", "Plan A"),
                make_msg("user", "Continue"),
                make_msg("assistant", "Plan B"),
                make_msg("user", "More"),
                make_msg("assistant", "Plan C"),
                make_msg("user", "Finish"),
            ],
            tools: None,
            model: None,
            cache_scope: Some("marker-growth".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        let body3 = build_and_commit(&provider, &req3);
        let msgs3 = body3["messages"].as_array().unwrap();
        assert!(content_has_cache_control(&msgs3[0]));  // system CC
        // msgs3: sys, user, asst("Plan A"), user, asst("Plan B"), user, asst("Plan C"), user
        // last assistant with text = msgs3[6] ("Plan C")
        assert!(!content_has_cache_control(&msgs3[1]));
        assert!(!content_has_cache_control(&msgs3[2]));
        assert!(!content_has_cache_control(&msgs3[3]));
        assert!(!content_has_cache_control(&msgs3[4]));
        assert!(!content_has_cache_control(&msgs3[5]));
        assert!(content_has_cache_control(&msgs3[6]));  // last assistant: CC
        assert!(!content_has_cache_control(&msgs3[7]));
    }

    #[test]
    fn test_protected_prefix_len_tracks_stable_prefix_only() {
        let provider = OpenAICompatProvider::new(
            "key", "http://localhost", "claude-test", None,
        );

        let make_msg = |role: &str, content: &str| -> HashMap<String, Value> {
            let mut m = HashMap::new();
            m.insert("role".into(), json!(role));
            m.insert("content".into(), json!(content));
            m
        };

        let req1 = ChatRequest {
            messages: vec![
                make_msg("system", "System prompt."),
                make_msg("user", "Goal"),
                make_msg("assistant", "Plan A"),
            ],
            tools: None,
            model: None,
            cache_scope: Some("prefix-growth".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        let prepared1 = provider.prepare_body(&req1);
        provider.commit_cache_plan(prepared1.commit_plan.unwrap());
        assert_eq!(provider.cache_breakpoint_idx("prefix-growth"), 0);

        let req2 = ChatRequest {
            messages: vec![
                make_msg("system", "System prompt."),
                make_msg("user", "Goal"),
                make_msg("assistant", "Plan A"),
                make_msg("user", "Continue"),
            ],
            tools: None,
            model: None,
            cache_scope: Some("prefix-growth".to_string()),
            max_tokens: 100,
            temperature: 0.0,
        };
        let prepared2 = provider.prepare_body(&req2);
        provider.commit_cache_plan(prepared2.commit_plan.unwrap());
        assert_eq!(provider.cache_breakpoint_idx("prefix-growth"), 3);
    }
}
