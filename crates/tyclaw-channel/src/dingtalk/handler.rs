//! 钉钉消息处理器 —— 处理收到的消息并回复。
//!
//! ChatbotHandler trait 定义了消息处理的接口，
//! 同时提供 reply_text 和 reply_markdown 回复方法。

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{error, info, warn};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use super::message::{CallbackMessage, ChatbotMessage};

/// 单张图片最大字节数（5MB），超过则跳过。
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// 单个文件最大字节数（500MB）。大文件流式写入磁盘，不经过内存缓冲。
const MAX_FILE_BYTES: usize = 500 * 1024 * 1024;

/// 聊天机器人消息处理器 trait。
///
/// 实现此 trait 来自定义消息处理逻辑。
/// `process` 方法在收到消息时被调用，返回 (状态码, 消息) 元组。
#[async_trait]
pub trait ChatbotHandler: Send + Sync {
    /// 处理收到的回调消息。
    ///
    /// 返回值：(status_code, message)
    /// - (200, "OK") 表示处理成功
    /// - (500, "error msg") 表示处理失败
    async fn process(&self, callback: &CallbackMessage) -> (u16, String);
}

/// 带重试的 HTTP POST（JSON body），返回 Response 或最后一次错误。
///
/// 用于钉钉 API 调用（非 multipart），最多重试 `max_retries` 次。
async fn post_json_with_retry(
    client: &Client,
    url: &str,
    headers: &[(&str, &str)],
    payload: &Value,
    timeout_secs: u64,
    max_retries: usize,
    label: &str,
) -> Result<reqwest::Response, String> {
    let mut last_err = String::new();
    for attempt in 1..=max_retries {
        let mut req = client
            .post(url)
            .header("Content-Type", "application/json")
            .json(payload)
            .timeout(std::time::Duration::from_secs(timeout_secs));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        match req.send().await {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                last_err = format!("{e}");
                warn!(error = %e, attempt, max_retries, url = %url, "DingTalk: {label} network error, retrying");
                if attempt < max_retries {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    }
    error!(url = %url, "DingTalk: {label} failed after {max_retries} attempts");
    Err(format!(
        "{label} failed after {max_retries} attempts: {last_err}"
    ))
}

/// 通过 session_webhook 发送 JSON payload，带重试（最多 2 次）。
///
/// 返回 true 表示发送成功，false 表示所有尝试均失败。
async fn post_webhook(client: &Client, url: &str, payload: &Value, label: &str) -> bool {
    for attempt in 1..=2 {
        match client
            .post(url)
            .header("Content-Type", "application/json")
            .json(payload)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    info!("DingTalk: {label} sent successfully");
                    return true;
                }
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!(status = %status, body = %body, attempt, "DingTalk: {label} HTTP {status}");
            }
            Err(e) => {
                warn!(error = %e, attempt, url = %url, "DingTalk: {label} network error");
            }
        }
        if attempt < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
    error!(url = %url, "DingTalk: {label} failed after all retries");
    false
}

/// 通过主动消息 API 发送文本（session_webhook 失败时的 fallback）。
///
/// 需要 token 和 robot_code，通过 `api.dingtalk.com` 发送。
pub async fn send_text_proactive(
    client: &Client,
    token: &str,
    robot_code: &str,
    message: &ChatbotMessage,
    text: &str,
) {
    let msg_param = json!({"content": text}).to_string();

    let (url, payload) = if message.is_private() {
        (
            "https://api.dingtalk.com/v1.0/robot/oToMessages/batchSend",
            json!({
                "robotCode": robot_code,
                "userIds": [&message.sender_staff_id],
                "msgKey": "sampleText",
                "msgParam": msg_param,
            }),
        )
    } else {
        (
            "https://api.dingtalk.com/v1.0/robot/groupMessages/send",
            json!({
                "robotCode": robot_code,
                "openConversationId": &message.conversation_id,
                "msgKey": "sampleText",
                "msgParam": msg_param,
            }),
        )
    };

    match client
        .post(url)
        .header("x-acs-dingtalk-access-token", token)
        .header("Content-Type", "application/json")
        .json(&payload)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) => {
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!(status = %status, body = %body, "send_text_proactive failed");
            } else {
                info!("send_text_proactive sent successfully (fallback)");
            }
        }
        Err(e) => warn!(error = %e, "send_text_proactive HTTP error"),
    }
}

/// 通过主动消息 API 发送文本（不依赖 ChatbotMessage，用于 timer 等异步场景）。
///
/// `channel` 用于判断私聊/群聊，`user_id` 和 `conversation_id` 用于定位发送目标。
pub async fn send_text_by_channel(
    client: &Client,
    token: &str,
    robot_code: &str,
    channel: &str,
    user_id: &str,
    conversation_id: &str,
    text: &str,
) {
    let msg_param = json!({"content": text}).to_string();

    let (url, payload) = if channel.contains("private") {
        (
            "https://api.dingtalk.com/v1.0/robot/oToMessages/batchSend",
            json!({
                "robotCode": robot_code,
                "userIds": [user_id],
                "msgKey": "sampleText",
                "msgParam": msg_param,
            }),
        )
    } else {
        (
            "https://api.dingtalk.com/v1.0/robot/groupMessages/send",
            json!({
                "robotCode": robot_code,
                "openConversationId": conversation_id,
                "msgKey": "sampleText",
                "msgParam": msg_param,
            }),
        )
    };

    match client
        .post(url)
        .header("x-acs-dingtalk-access-token", token)
        .header("Content-Type", "application/json")
        .json(&payload)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) => {
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!(status = %status, body = %body, "send_text_by_channel failed");
            } else {
                info!(channel = %channel, "send_text_by_channel sent successfully");
            }
        }
        Err(e) => warn!(error = %e, "send_text_by_channel HTTP error"),
    }
}

/// 通过 session_webhook 回复文本消息。
pub async fn reply_text(client: &Client, text: &str, message: &ChatbotMessage) -> bool {
    if message.session_webhook.is_empty() {
        warn!("No session_webhook in message, cannot reply");
        return false;
    }

    let payload = json!({
        "msgtype": "text",
        "text": { "content": text },
        "at": {
            "atUserIds": [&message.sender_staff_id]
        }
    });

    post_webhook(client, &message.session_webhook, &payload, "reply_text").await
}

/// 通过 session_webhook 回复 Markdown 消息。
pub async fn reply_markdown(
    client: &Client,
    title: &str,
    text: &str,
    message: &ChatbotMessage,
) -> bool {
    if message.session_webhook.is_empty() {
        warn!("No session_webhook in message, cannot reply");
        return false;
    }

    let payload = json!({
        "msgtype": "markdown",
        "markdown": {
            "title": title,
            "text": text,
        },
        "at": {
            "atUserIds": [&message.sender_staff_id]
        }
    });

    post_webhook(client, &message.session_webhook, &payload, "reply_markdown").await
}

/// 通过钉钉主动消息 API 发送文件。
pub async fn reply_file(
    client: &Client,
    token: &str,
    robot_code: &str,
    message: &ChatbotMessage,
    media_id: &str,
    file_name: &str,
    file_type: &str,
) -> Result<(), String> {
    let msg_param = json!({
        "mediaId": media_id,
        "fileName": file_name,
        "fileType": file_type,
    })
    .to_string();

    let (url, payload) = if message.is_private() {
        (
            "https://api.dingtalk.com/v1.0/robot/oToMessages/batchSend",
            json!({
                "robotCode": robot_code,
                "userIds": [&message.sender_staff_id],
                "msgKey": "sampleFile",
                "msgParam": msg_param,
            }),
        )
    } else {
        (
            "https://api.dingtalk.com/v1.0/robot/groupMessages/send",
            json!({
                "robotCode": robot_code,
                "openConversationId": &message.conversation_id,
                "msgKey": "sampleFile",
                "msgParam": msg_param,
            }),
        )
    };

    let resp = post_json_with_retry(
        client,
        url,
        &[("x-acs-dingtalk-access-token", token)],
        &payload,
        10,
        3,
        "reply_file",
    )
    .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("reply_file failed: {status} {body}"));
    }

    info!("File sent successfully: {}", file_name);
    Ok(())
}

/// 上传媒体文件到钉钉服务器。
pub async fn upload_media(
    client: &Client,
    token: &str,
    _robot_code: &str,
    file_path: &str,
    media_type: &str,
) -> Result<String, String> {
    let url =
        format!("https://oapi.dingtalk.com/media/upload?access_token={token}&type={media_type}");

    let file_bytes = tokio::fs::read(file_path)
        .await
        .map_err(|e| format!("Read file error: {e}"))?;

    let file_name = std::path::Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");

    info!(file = %file_name, size = file_bytes.len(), "Uploading media to DingTalk");

    let part = reqwest::multipart::Part::bytes(file_bytes).file_name(file_name.to_string());
    let form = reqwest::multipart::Form::new().part("media", part);

    let resp = client
        .post(&url)
        .multipart(form)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| format!("Upload HTTP error: {e}"))?;

    let data: serde_json::Value = resp
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("Upload parse error: {e}"))?;

    let errcode = data
        .get("errcode")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    if errcode != 0 {
        return Err(format!("DingTalk upload failed: {data}"));
    }

    data.get("media_id")
        .and_then(serde_json::Value::as_str)
        .map(String::from)
        .ok_or_else(|| format!("No media_id in response: {data}"))
}

/// 通过钉钉 API 获取文件/图片的实际下载 URL（带重试）。
async fn get_download_url(
    client: &Client,
    token: &str,
    robot_code: &str,
    download_code: &str,
) -> Result<String, String> {
    let api_url = "https://api.dingtalk.com/v1.0/robot/messageFiles/download";
    let payload = json!({
        "downloadCode": download_code,
        "robotCode": robot_code,
    });

    let resp = post_json_with_retry(
        client,
        api_url,
        &[("x-acs-dingtalk-access-token", token)],
        &payload,
        30,
        3,
        "get_download_url",
    )
    .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Download API failed: {status} {body}"));
    }

    let data: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Download API JSON parse error: {e}"))?;

    data.get("downloadUrl")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| format!("No downloadUrl in response: {data}"))
}

/// 通过钉钉 API 下载图片并转为 data URI（`data:image/...;base64,...`）。
pub async fn download_image_as_data_uri(
    client: &Client,
    token: &str,
    robot_code: &str,
    download_code: &str,
) -> Result<String, String> {
    let download_url = get_download_url(client, token, robot_code, download_code).await?;

    let img_resp = client
        .get(download_url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("Image download HTTP error: {e}"))?;

    if !img_resp.status().is_success() {
        let status = img_resp.status();
        return Err(format!("Image download failed: {status}"));
    }

    let bytes = img_resp
        .bytes()
        .await
        .map_err(|e| format!("Read image bytes error: {e}"))?;

    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "Image too large: {} bytes (max {})",
            bytes.len(),
            MAX_IMAGE_BYTES
        ));
    }

    let mime = detect_image_mime(&bytes).ok_or_else(|| {
        format!(
            "Unsupported image format (first bytes: {:02x?})",
            &bytes[..bytes.len().min(8)]
        )
    })?;

    let encoded = BASE64.encode(&bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

/// 通过钉钉 API 下载文件并保存到本地。
pub async fn download_file(
    client: &Client,
    token: &str,
    robot_code: &str,
    download_code: &str,
    save_dir: &std::path::Path,
    file_name: &str,
) -> Result<String, String> {
    let download_url = get_download_url(client, token, robot_code, download_code).await?;

    let file_resp = client
        .get(download_url)
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .await
        .map_err(|e| format!("File download HTTP error: {e}"))?;

    if !file_resp.status().is_success() {
        let status = file_resp.status();
        return Err(format!("File download failed: {status}"));
    }

    // 先用 Content-Length 检查大小（避免下载后才发现超限）
    if let Some(content_length) = file_resp.content_length() {
        if content_length as usize > MAX_FILE_BYTES {
            return Err(format!(
                "File too large: {} bytes (max {})",
                content_length, MAX_FILE_BYTES
            ));
        }
    }

    tokio::fs::create_dir_all(save_dir)
        .await
        .map_err(|e| format!("Create dir error: {e}"))?;

    // 防止路径穿越：只取文件名部分，过滤 ../ 等
    let safe_name = std::path::Path::new(file_name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("downloaded_file");

    // 流式写入磁盘，不缓冲整个文件到内存
    let save_path = save_dir.join(safe_name);
    let mut file = tokio::fs::File::create(&save_path)
        .await
        .map_err(|e| format!("Create file error: {e}"))?;

    let mut stream = file_resp.bytes_stream();
    let mut total_bytes: usize = 0;
    use tokio::io::AsyncWriteExt;
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Stream read error: {e}"))?;
        total_bytes += chunk.len();
        if total_bytes > MAX_FILE_BYTES {
            // 超限：删除半截文件
            drop(file);
            let _ = tokio::fs::remove_file(&save_path).await;
            return Err(format!(
                "File too large: {} bytes downloaded (max {})",
                total_bytes, MAX_FILE_BYTES
            ));
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Write chunk error: {e}"))?;
    }
    file.flush().await.map_err(|e| format!("Flush error: {e}"))?;

    info!(
        file_name = %file_name,
        size = total_bytes,
        "File downloaded and saved"
    );

    Ok(save_path.to_string_lossy().to_string())
}

/// 根据文件头 magic bytes 检测图片 MIME 类型。
fn detect_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 4 {
        return None;
    }
    if bytes.starts_with(b"\x89PNG") {
        Some("image/png")
    } else if bytes.starts_with(b"\xFF\xD8\xFF") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}
