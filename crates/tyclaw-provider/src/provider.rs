use async_trait::async_trait;
use std::sync::OnceLock;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use tyclaw_types::TyclawError;

use crate::types::{ChatRequest, GenerationSettings, LLMResponse};

/// 重试延迟时间序列（单位：秒）。
/// 采用指数退避策略：1s → 2s → 4s，共 3 次重试后执行最终尝试。
const RETRY_DELAYS: &[u64] = &[1, 2, 4];

/// 默认 LLM 并发上限。
const DEFAULT_MAX_CONCURRENT_LLM: usize = 4;

/// 全局 LLM 并发信号量。
static LLM_SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();

/// 初始化 LLM 并发限制。应在启动时调用一次。
pub fn init_concurrency(max_concurrent: usize) {
    let limit = if max_concurrent == 0 { DEFAULT_MAX_CONCURRENT_LLM } else { max_concurrent };
    let _ = LLM_SEMAPHORE.set(Semaphore::new(limit));
    info!(max_concurrent = limit, "LLM concurrency limit initialized");
}

fn get_semaphore() -> &'static Semaphore {
    LLM_SEMAPHORE.get_or_init(|| Semaphore::new(DEFAULT_MAX_CONCURRENT_LLM))
}

/// 临时性错误的特征字符串列表。
/// 当 LLM 返回的错误信息中包含这些关键词时，认为是可重试的临时错误。
/// 包括：速率限制（429）、服务器内部错误（500-504）、超时、连接问题等。
const TRANSIENT_MARKERS: &[&str] = &[
    "429",
    "rate limit",
    "500",
    "502",
    "503",
    "504",
    "overloaded",
    "timeout",
    "timed out",
    "connection",
    "server error",
    "temporarily unavailable",
];

/// 判断错误是否为临时性/可重试错误。
///
/// 将错误消息转为小写后，检查是否包含任何临时错误特征字符串。
fn is_transient_error(content: Option<&str>) -> bool {
    let err = content.unwrap_or("").to_lowercase();
    TRANSIENT_MARKERS.iter().any(|m| err.contains(m))
}

/// LLM 提供者 trait —— 所有 LLM 实现必须满足的接口。
///
/// 通过 `async_trait` 支持异步方法。
/// `Send + Sync` 约束确保可以在多线程环境中安全使用。
#[async_trait]
pub trait LLMProvider: Send + Sync {
    /// 发送聊天请求并返回 LLM 响应。
    /// 这是核心方法，各个具体实现（如 OpenAI、Anthropic）需要实现此方法。
    async fn chat(&self, request: ChatRequest) -> Result<LLMResponse, TyclawError>;

    /// 返回默认模型标识符（如 "gpt-4o"）。
    fn default_model(&self) -> &str;

    /// 返回 API 基础 URL（用于 multi-model 场景创建衍生 provider）。
    fn api_base(&self) -> String {
        String::new()
    }

    /// 返回 API 密钥（用于 multi-model 场景创建衍生 provider）。
    fn api_key(&self) -> String {
        String::new()
    }

    /// 返回默认生成参数（温度、最大 token 数等）。
    /// 提供默认实现，子类可按需覆盖。
    fn generation_settings(&self) -> GenerationSettings {
        GenerationSettings::default()
    }

    /// 清除指定 cache scope 的缓存状态。
    /// session 回收时调用，避免旧消息残留导致 tool_call 配对错误。
    fn clear_cache_scope(&self, _scope: &str) {}

    /// 返回指定 scope 上一次请求的 cache breakpoint 位置。
    /// 压缩时应保留此位置之前的消息不动，避免破坏 prompt cache 前缀。
    /// 默认返回 0（不保护）。
    fn cache_breakpoint_idx(&self, _scope: &str) -> usize {
        0
    }

    /// 带指数退避重试的聊天方法。
    ///
    /// 工作流程：
    /// 1. 按 RETRY_DELAYS 中的延迟时间进行最多 3 次重试
    /// 2. 每次重试前检查错误是否为临时性错误（如限流、超时）
    /// 3. 非临时性错误（如参数错误）会立即返回，不再重试
    /// 4. 所有重试失败后执行最终尝试
    ///
    /// 注意：此方法始终返回 LLMResponse（不会返回 Err），
    /// 错误信息通过 finish_reason="error" 传递。
    async fn chat_with_retry(
        &self,
        messages: Vec<std::collections::HashMap<String, serde_json::Value>>,
        tools: Option<Vec<serde_json::Value>>,
        model: Option<String>,
        cache_scope: Option<String>,
    ) -> LLMResponse {
        let settings = self.generation_settings();
        let request = ChatRequest {
            messages,
            tools,
            model,
            cache_scope,
            max_tokens: settings.max_tokens,
            temperature: settings.temperature,
        };

        // 获取 LLM 并发信号量（多个 agent loop 共享，限制同时调用 LLM 的数量）
        let _permit = get_semaphore().acquire().await.expect("LLM semaphore closed");

        // 按延迟序列依次重试
        for (attempt, delay) in RETRY_DELAYS.iter().enumerate() {
            let response = match self.chat(request.clone()).await {
                Ok(r) => r,
                Err(e) => LLMResponse::error(format!("Error calling LLM: {e}")),
            };

            // 如果不是错误响应，直接返回成功结果
            if response.finish_reason != "error" {
                // 检测空回复（某些上游偶发返回 finish_reason=stop 但无内容）
                let is_empty = response.content.as_ref().map_or(true, |c| c.is_empty())
                    && response.tool_calls.is_empty();
                if is_empty && attempt < RETRY_DELAYS.len() - 1 {
                    warn!(
                        attempt = attempt + 1,
                        "LLM returned empty response (no content, no tool_calls), retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(RETRY_DELAYS[attempt])).await;
                    continue;
                }
                return response;
            }
            // 如果不是临时性错误，不再重试
            if !is_transient_error(response.content.as_deref()) {
                return response;
            }

            // 记录重试日志
            warn!(
                attempt = attempt + 1,
                total = RETRY_DELAYS.len(),
                delay_s = delay,
                error = response.content.as_deref().unwrap_or(""),
                "LLM transient error, retrying"
            );
            // 等待指定延迟后重试
            tokio::time::sleep(std::time::Duration::from_secs(*delay)).await;
        }

        // 最终尝试（第4次调用），不再重试
        match self.chat(request).await {
            Ok(r) => r,
            Err(e) => LLMResponse::error(format!("Error calling LLM: {e}")),
        }
    }
}
