//! 钉钉凭证和 Token 管理。
//!
//! 管理 client_id/client_secret，并提供 OAuth2 access_token 的获取和缓存。
//! Token 有效期内会复用缓存，过期前自动刷新。

use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// 钉钉应用凭证。
#[derive(Clone)]
pub struct Credential {
    pub client_id: String,
    pub client_secret: String,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("client_id", &self.client_id)
            .field("client_secret", &"***")
            .finish()
    }
}

impl Credential {
    pub fn new(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
        }
    }
}

/// OAuth2 Token 响应。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenResponse {
    access_token: String,
    expire_in: Option<u64>,
}

/// 带缓存的 Token 管理器。
///
/// 线程安全（Arc<Mutex<>>），支持并发访问。
/// Token 在过期前 60 秒自动刷新。
#[derive(Clone)]
pub struct TokenManager {
    credential: Credential,
    client: Client,
    state: Arc<Mutex<TokenState>>,
}

/// Token 缓存状态。
struct TokenState {
    token: String,
    expiry: f64, // Unix timestamp
}

impl TokenManager {
    pub fn new(credential: Credential) -> Self {
        Self {
            credential,
            client: Client::new(),
            state: Arc::new(Mutex::new(TokenState {
                token: String::new(),
                expiry: 0.0,
            })),
        }
    }

    /// 获取有效的 access_token。
    ///
    /// 如果缓存的 token 仍在有效期内，直接返回。
    /// 否则调用钉钉 OAuth2 API 获取新 token。
    pub async fn get_token(&self) -> Result<String, String> {
        let mut state = self.state.lock().await;

        // 检查缓存是否有效（提前60秒刷新）
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        if !state.token.is_empty() && now < state.expiry {
            return Ok(state.token.clone());
        }

        // 调用钉钉 OAuth2 API 获取新 token
        info!("Refreshing DingTalk access token");
        let resp = self
            .client
            .post("https://api.dingtalk.com/v1.0/oauth2/accessToken")
            .json(&serde_json::json!({
                "appKey": self.credential.client_id,
                "appSecret": self.credential.client_secret,
            }))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("Token request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Token API returned {status}: {body}"));
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("Token parse error: {e}"))?;

        let expire_in = token_resp.expire_in.unwrap_or(7200);
        state.token = token_resp.access_token.clone();
        state.expiry = now + expire_in as f64 - 60.0; // 提前60秒过期

        info!(expire_in, "DingTalk token refreshed");
        Ok(token_resp.access_token)
    }

    /// 重置 token 缓存（收到 401 时调用）。
    pub async fn reset(&self) {
        let mut state = self.state.lock().await;
        state.token.clear();
        state.expiry = 0.0;
        warn!("DingTalk token cache reset");
    }
}
