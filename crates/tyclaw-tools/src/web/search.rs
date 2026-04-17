//! WebSearchTool —— 搜索引擎工具，支持 Brave API、阿里云百炼 MCP 和 DuckDuckGo 兜底。

use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use tracing::{info, warn};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::sync::atomic::{AtomicU64, Ordering};

use super::html::strip_tags;
use crate::base::{RiskLevel, Tool};

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_7_2) AppleWebKit/537.36";
const DEFAULT_TIMEOUT_SECS: u64 = 15;
const BAILIAN_MCP_ENDPOINT: &str =
    "https://dashscope.aliyuncs.com/api/v1/mcps/WebSearch/mcp";

static BAILIAN_REQ_ID: AtomicU64 = AtomicU64::new(1);

/// WebSearch 配置。
#[derive(Debug, Clone, Deserialize)]
pub struct WebSearchConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    #[serde(default)]
    pub proxy: String,
}

fn default_provider() -> String {
    "brave".into()
}
fn default_max_results() -> usize {
    5
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            api_key: String::new(),
            max_results: default_max_results(),
            proxy: String::new(),
        }
    }
}

pub struct WebSearchTool {
    config: WebSearchConfig,
    client: Client,
}

impl WebSearchTool {
    pub fn new(config: WebSearchConfig) -> Self {
        let mut builder = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS));

        if !config.proxy.is_empty() {
            if let Ok(proxy) = reqwest::Proxy::all(&config.proxy) {
                builder = builder.proxy(proxy);
            }
        }

        let client = builder.build().unwrap_or_else(|_| Client::new());
        Self { config, client }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for real-time information. Returns titles, URLs, and snippets. \
         Use for current events, API docs, technical solutions, or information that may be outdated in training data. \
         Tip: use web_fetch to get full page content from a specific URL."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "count": { "type": "integer", "description": "Number of results (1-10)", "minimum": 1, "maximum": 10 }
            },
            "required": ["query"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Read
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let query = match params.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_string(),
            None => return "Error: Missing 'query' parameter".into(),
        };
        let count = params
            .get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.config.max_results as u64)
            .clamp(1, 10) as usize;

        let provider = self.config.provider.trim().to_lowercase();
        match provider.as_str() {
            "duckduckgo" => self.search_duckduckgo(&query, count).await,
            "bailian" => self.search_bailian(&query, count).await,
            _ => self.search_brave(&query, count).await,
        }
    }
}

impl WebSearchTool {
    async fn search_brave(&self, query: &str, n: usize) -> String {
        let api_key = if self.config.api_key.is_empty() {
            env::var("BRAVE_API_KEY").unwrap_or_default()
        } else {
            self.config.api_key.clone()
        };

        if api_key.is_empty() {
            warn!("BRAVE_API_KEY not set, falling back to DuckDuckGo");
            return self.search_duckduckgo(query, n).await;
        }

        let result = self
            .client
            .get("https://api.search.brave.com/res/v1/web/search")
            .query(&[("q", query), ("count", &n.to_string())])
            .header("Accept", "application/json")
            .header("X-Subscription-Token", &api_key)
            .send()
            .await;

        match result {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    warn!("Brave API returned {status}, falling back to DuckDuckGo");
                    return self.search_duckduckgo(query, n).await;
                }
                match resp.json::<Value>().await {
                    Ok(data) => {
                        let results = data
                            .get("web")
                            .and_then(|w| w.get("results"))
                            .and_then(|r| r.as_array())
                            .cloned()
                            .unwrap_or_default();

                        let items: Vec<SearchItem> = results
                            .iter()
                            .map(|r| SearchItem {
                                title: r
                                    .get("title")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                url: r
                                    .get("url")
                                    .and_then(|u| u.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                snippet: r
                                    .get("description")
                                    .and_then(|d| d.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            })
                            .collect();

                        format_results(query, &items, n)
                    }
                    Err(e) => {
                        warn!("Brave API parse error: {e}, falling back to DuckDuckGo");
                        self.search_duckduckgo(query, n).await
                    }
                }
            }
            Err(e) => {
                warn!("Brave API request error: {e}, falling back to DuckDuckGo");
                self.search_duckduckgo(query, n).await
            }
        }
    }

    /// 阿里云百炼 WebSearch MCP 搜索。
    /// 通过 JSON-RPC 2.0 调用百炼 MCP 的 `bailian_web_search` 工具。
    async fn search_bailian(&self, query: &str, n: usize) -> String {
        let api_key = if self.config.api_key.is_empty() {
            env::var("BAILIAN_API_KEY").unwrap_or_default()
        } else {
            self.config.api_key.clone()
        };

        if api_key.is_empty() {
            warn!("BAILIAN_API_KEY not set, falling back to DuckDuckGo");
            return self.search_duckduckgo(query, n).await;
        }

        let req_id = BAILIAN_REQ_ID.fetch_add(1, Ordering::Relaxed);
        let body = json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": "tools/call",
            "params": {
                "name": "bailian_web_search",
                "arguments": {
                    "query": query,
                    "count": n
                }
            }
        });

        let result = self
            .client
            .post(BAILIAN_MCP_ENDPOINT)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await;

        match result {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    warn!("Bailian MCP returned {status}, falling back to DuckDuckGo");
                    return self.search_duckduckgo(query, n).await;
                }
                match resp.json::<Value>().await {
                    Ok(data) => {
                        if let Some(text) = data
                            .get("result")
                            .and_then(|r| r.get("content"))
                            .and_then(|c| c.as_array())
                            .and_then(|arr| arr.first())
                            .and_then(|item| item.get("text"))
                            .and_then(|t| t.as_str())
                        {
                            text.to_string()
                        } else if let Some(err) = data.get("error") {
                            warn!("Bailian MCP error: {err}, falling back to DuckDuckGo");
                            self.search_duckduckgo(query, n).await
                        } else {
                            warn!(
                                "Bailian MCP unexpected response structure, falling back to DuckDuckGo"
                            );
                            self.search_duckduckgo(query, n).await
                        }
                    }
                    Err(e) => {
                        warn!("Bailian MCP parse error: {e}, falling back to DuckDuckGo");
                        self.search_duckduckgo(query, n).await
                    }
                }
            }
            Err(e) => {
                warn!("Bailian MCP request error: {e}, falling back to DuckDuckGo");
                self.search_duckduckgo(query, n).await
            }
        }
    }

    /// DuckDuckGo HTML 抓取搜索（免费兜底）。
    /// 通过 html.duckduckgo.com/html/ 端点抓取搜索结果页面并用正则解析。
    /// 自动重试一次（DuckDuckGo 对自动化请求可能返回空结果）。
    async fn search_duckduckgo(&self, query: &str, n: usize) -> String {
        for attempt in 0..2 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            let encoded_query = urlencoding::encode(query);
            info!(query, attempt, encoded = %encoded_query, "DuckDuckGo: sending request");

            let result = self
                .client
                .post("https://html.duckduckgo.com/html/")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .header("Accept", "text/html")
                .header("Accept-Language", "en-US,en;q=0.9,zh-CN;q=0.8")
                .body(format!("q={encoded_query}"))
                .send()
                .await;

            match result {
                Ok(resp) => {
                    let status = resp.status();
                    let headers_debug = format!(
                        "content-type={:?} content-length={:?}",
                        resp.headers().get("content-type"),
                        resp.headers().get("content-length"),
                    );
                    info!(
                        query, attempt, %status, headers = %headers_debug,
                        "DuckDuckGo: response received"
                    );
                    if !status.is_success() {
                        warn!(status = %status, query, attempt, "DuckDuckGo: non-200 status");
                        continue;
                    }
                    match resp.text().await {
                        Ok(html) => {
                            let html_len = html.len();
                            let has_result_body = html.contains("result__body");
                            let has_result_a = html.contains("result__a");
                            let has_no_results = html.contains("No results") || html.contains("No more results");
                            let has_captcha = html.contains("captcha") || html.contains("bot-detection") || html.contains("g-recaptcha");
                            let body_snippet: String = html.chars().skip(html.len().saturating_sub(500).min(html.len() / 2)).take(300).collect();

                            info!(
                                query, attempt, html_len, has_result_body, has_result_a,
                                has_no_results, has_captcha,
                                body_snippet = %body_snippet,
                                "DuckDuckGo: HTML analysis"
                            );

                            let items = parse_duckduckgo_html(&html);
                            info!(
                                query, attempt, parsed_count = items.len(),
                                "DuckDuckGo: parsed results"
                            );

                            if items.is_empty() {
                                if attempt == 0 {
                                    warn!(
                                        query, html_len, has_result_body, has_captcha,
                                        "DuckDuckGo: empty results on first attempt, retrying"
                                    );
                                    continue;
                                }
                                return format!("No results for: {query}");
                            }
                            return format_results(query, &items, n);
                        }
                        Err(e) => {
                            warn!(error = %e, query, attempt, "DuckDuckGo: failed to read body");
                            return format!("Error: Failed to read DuckDuckGo response: {e}");
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, query, attempt, "DuckDuckGo: request failed");
                    if attempt == 0 {
                        continue;
                    }
                    return format!("Error: DuckDuckGo search failed ({e})");
                }
            }
        }
        format!("No results for: {query}")
    }
}

struct SearchItem {
    title: String,
    url: String,
    snippet: String,
}

fn format_results(query: &str, items: &[SearchItem], n: usize) -> String {
    if items.is_empty() {
        return format!("No results for: {query}");
    }
    let mut lines = vec![format!("Results for: {query}\n")];
    for (i, item) in items.iter().take(n).enumerate() {
        let title = strip_tags(&item.title);
        let snippet = strip_tags(&item.snippet);
        lines.push(format!("{}. {title}\n   {}", i + 1, item.url));
        if !snippet.is_empty() {
            lines.push(format!("   {snippet}"));
        }
    }
    lines.join("\n")
}

/// 解析 DuckDuckGo HTML 搜索结果页面。
fn parse_duckduckgo_html(html: &str) -> Vec<SearchItem> {
    // DuckDuckGo 的 class 可能是 "result__a" 或带其他 class 前缀
    let re_result = Regex::new(
        r#"<a\s+rel="nofollow"\s+class="result__a"\s+href="([^"]+)"[^>]*>([\s\S]*?)</a>"#,
    )
    .unwrap();
    let re_snippet = Regex::new(r#"class="result__snippet"[^>]*>([\s\S]*?)</a>"#).unwrap();

    // 用 "result__body" 分割（不限定 class= 前缀，兼容 "links_main links_deep result__body" 等变体）
    let result_blocks: Vec<&str> = html.split("result__body").skip(1).collect();

    let mut items = Vec::new();
    for block in result_blocks {
        let url = re_result.captures(block).and_then(|c| {
            let raw_url = c.get(1)?.as_str();
            decode_ddg_url(raw_url)
        });
        let title = re_result
            .captures(block)
            .and_then(|c| c.get(2))
            .map(|m| strip_tags(m.as_str()))
            .unwrap_or_default();
        let snippet = re_snippet
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| strip_tags(m.as_str()))
            .unwrap_or_default();

        if let Some(url) = url {
            if !title.is_empty() {
                items.push(SearchItem {
                    title,
                    url,
                    snippet,
                });
            }
        }
    }
    items
}

/// 解码 DuckDuckGo 的重定向 URL（//duckduckgo.com/l/?uddg=ENCODED_URL&...）。
fn decode_ddg_url(raw: &str) -> Option<String> {
    if raw.contains("duckduckgo.com/l/?") {
        let parsed = url::Url::parse(&format!("https:{raw}")).ok()?;
        parsed
            .query_pairs()
            .find(|(k, _)| k == "uddg")
            .map(|(_, v)| v.into_owned())
    } else if raw.starts_with("http") {
        Some(raw.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_results_empty() {
        let result = format_results("test", &[], 5);
        assert!(result.contains("No results"));
    }

    #[test]
    fn test_format_results_with_items() {
        let items = vec![SearchItem {
            title: "Rust Programming".into(),
            url: "https://rust-lang.org".into(),
            snippet: "A systems language".into(),
        }];
        let result = format_results("rust", &items, 5);
        assert!(result.contains("Rust Programming"));
        assert!(result.contains("https://rust-lang.org"));
        assert!(result.contains("A systems language"));
    }

    #[test]
    fn test_decode_ddg_url_direct() {
        let url = "https://example.com/page";
        assert_eq!(decode_ddg_url(url), Some(url.to_string()));
    }

    #[test]
    fn test_decode_ddg_url_redirect() {
        let raw = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc";
        let result = decode_ddg_url(raw);
        assert_eq!(result, Some("https://example.com/page".to_string()));
    }

    #[test]
    fn test_default_config() {
        let config = WebSearchConfig::default();
        assert_eq!(config.provider, "brave");
        assert_eq!(config.max_results, 5);
    }

    #[test]
    fn test_bailian_config() {
        let json_str = r#"{
            "provider": "bailian",
            "api_key": "sk-test123",
            "max_results": 3
        }"#;
        let config: WebSearchConfig = serde_json::from_str(json_str).unwrap();
        assert_eq!(config.provider, "bailian");
        assert_eq!(config.api_key, "sk-test123");
        assert_eq!(config.max_results, 3);
    }

    #[test]
    fn test_bailian_mcp_request_body() {
        let req_id = 42u64;
        let body = json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": "tools/call",
            "params": {
                "name": "bailian_web_search",
                "arguments": {
                    "query": "rust programming",
                    "count": 5
                }
            }
        });
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["method"], "tools/call");
        assert_eq!(body["params"]["name"], "bailian_web_search");
        assert_eq!(body["params"]["arguments"]["query"], "rust programming");
        assert_eq!(body["params"]["arguments"]["count"], 5);
    }

    #[test]
    fn test_parse_bailian_mcp_response() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [
                    {
                        "type": "text",
                        "text": "1. Rust Programming\n   https://rust-lang.org\n   A systems language"
                    }
                ]
            }
        });

        let text = response
            .get("result")
            .and_then(|r| r.get("content"))
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("text"))
            .and_then(|t| t.as_str());

        assert!(text.is_some());
        assert!(text.unwrap().contains("Rust Programming"));
    }

    #[test]
    fn test_parse_bailian_mcp_error_response() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32600,
                "message": "Invalid request"
            }
        });

        let has_error = response.get("error").is_some();
        let has_result = response
            .get("result")
            .and_then(|r| r.get("content"))
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("text"))
            .and_then(|t| t.as_str())
            .is_some();

        assert!(has_error);
        assert!(!has_result);
    }

    #[test]
    fn test_bailian_req_id_increments() {
        let id1 = BAILIAN_REQ_ID.load(Ordering::Relaxed);
        let _ = BAILIAN_REQ_ID.fetch_add(1, Ordering::Relaxed);
        let id2 = BAILIAN_REQ_ID.load(Ordering::Relaxed);
        assert_eq!(id2, id1 + 1);
    }
}
