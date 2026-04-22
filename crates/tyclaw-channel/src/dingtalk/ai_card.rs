//! 钉钉 AI 卡片 replier —— 在卡片内呈现"思考中动画 + 当前工具行"。
//!
//! 每条用户提问对应一张卡片实例。生命周期：
//! 1. [`CardReplier::create`]：调用 `/v1.0/card/instances` 创建模板实例，
//!    并通过 `/v1.0/card/instances/deliver` 投递到当前会话。
//! 2. [`CardReplier::feed_thinking`] / [`CardReplier::feed_tool`]：agent 回调里
//!    每次更新"思考一行 + 工具一行"，走 `/v1.0/card/streaming` 全量重绘。
//! 3. [`CardReplier::finalize`]：任务完成，关流并写入最终回复 + flowStatus=3。
//! 4. [`CardReplier::terminate`]：被用户主动停止时用，尾部贴"已终止"提示。
//!
//! # 前置条件
//! 钉钉开发者后台预先创建好 AI 卡片模板，模板变量必须包含 `content`（卡片正文）
//! 和 `flowStatus`（`1` 处理中 / `3` 完成）。模板 id 通过 `config.yaml` 的
//! `dingtalk.card_template_id` 传入——未配置时 bot 层回退到纯文本回复。

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex as PMutex;
use reqwest::Client;
use serde_json::json;
use tracing::{debug, info, warn};
use uuid::Uuid;

use tyclaw_orchestration::Orchestrator;

use super::credential::TokenManager;
use super::handler::ChatbotHandler;
use super::message::CallbackMessage;

/// 钉钉 AI 卡片按钮回调的 Stream topic。
pub const CARD_CALLBACK_TOPIC: &str = "/v1.0/card/instances/callback";

/// 创建卡片实例。
const API_CREATE: &str = "https://api.dingtalk.com/v1.0/card/instances";
/// 投递卡片到会话。
const API_DELIVER: &str = "https://api.dingtalk.com/v1.0/card/instances/deliver";
/// 流式更新卡片内容（PUT）。
const API_STREAMING: &str = "https://api.dingtalk.com/v1.0/card/streaming";
/// 覆盖整个 cardData（PUT）——用于写入最终回复。
const API_UPDATE: &str = "https://api.dingtalk.com/v1.0/card/instances";

/// 卡片模板变量 key（需与钉钉后台模板一致）。
const CONTENT_KEY: &str = "content";
const RESULT_KEY: &str = "result";
/// `progress`（0-100 百分比）是 AI 卡片可选模板变量。当前模板暂未使用，
/// 保留常量待后续模板升级后直接启用。
#[allow(dead_code)]
const PROGRESS_KEY: &str = "progress";
/// flowStatus 是钉钉 AI 卡片组件的内置状态字段，控制"输出中"/"完成"状态切换。
const FLOW_STATUS_KEY: &str = "flowStatus";
/// flowStatus: 1=处理中，3=完成（与 Python 版一致）
const FLOW_STATUS_PROCESSING: &str = "1";
const FLOW_STATUS_FINISHED: &str = "3";
#[allow(dead_code)]
const PROGRESS_PROCESSING: &str = "0";
#[allow(dead_code)]
const PROGRESS_FINISHED: &str = "100";

/// 占位空内容——钉钉卡片空字符串会报错，用零宽空格兜底。
const EMPTY_PLACEHOLDER: &str = "\u{200B}";

/// 卡片当前状态。
#[derive(Default)]
struct CardState {
    /// 最新的 thinking 摘要（取 agent reasoning 的最后一行）。
    latest_thinking: String,
    /// 当前工具行（"exec: foo"、"read: bar.rs" 等 brief 输出）。
    latest_tool: String,
    /// 是否已终结——终结后所有 feed 调用都应是 no-op。
    finalized: bool,
}

/// 卡片超时时间——超过此时长未 finalize 的卡片视为泄漏，reap 时自动清理。
const CARD_TTL_SECS: u64 = 30 * 60; // 30 分钟

/// 单张 AI 卡片的生命周期封装。
pub struct CardReplier {
    http: Client,
    token_manager: TokenManager,
    /// 保留以备后续 API 调用（如 deliver 需要、retry 需要），目前 create 之后暂未复用。
    #[allow(dead_code)]
    robot_code: String,
    /// 钉钉后台预建模板的 id。
    template_id: String,
    /// 本实例的 outTrackId（发给钉钉后即为该卡片的唯一标识）。
    out_track_id: String,
    /// 消息头（通常是"@昵称: 原始问题\n\n"），拼在 thinking 之前展示。
    header: String,
    /// 任务发起者 staff_id——停止按钮回调时用于 owner 校验。
    pub owner_staff_id: String,
    /// 创建时间——用于 reap 超时卡片。
    created_at: std::time::Instant,
    state: PMutex<CardState>,
}

impl CardReplier {
    /// 创建卡片实例并投递到用户会话。成功返回 Arc，失败返回 Err(原因)。
    ///
    /// * `conversation_id` —— 群聊时有值（会话 id），私聊时传空串。
    /// * `sender_staff_id` —— 提问者工号，用于 privateData + owner 校验。
    /// * `is_private` —— 私聊 / 群聊分支。
    pub async fn create(
        http: Client,
        token_manager: TokenManager,
        robot_code: impl Into<String>,
        template_id: impl Into<String>,
        header: impl Into<String>,
        conversation_id: &str,
        sender_staff_id: &str,
        is_private: bool,
    ) -> Result<Arc<Self>, String> {
        let template_id = template_id.into();
        let robot_code = robot_code.into();
        let out_track_id = Uuid::new_v4().to_string();
        let header = header.into();

        let token = token_manager
            .get_token()
            .await
            .map_err(|e| format!("get_token failed: {e}"))?;

        // 1) 创建卡片实例。
        // 必须声明 imRobotOpenSpaceModel / imGroupOpenSpaceModel，
        // 否则 deliver 时钉钉会报 "spaces of card is empty"。
        let create_body = json!({
            "cardTemplateId": template_id,
            "outTrackId": out_track_id,
            "callbackType": "STREAM",
            "cardData": {
                "cardParamMap": {
                    CONTENT_KEY: EMPTY_PLACEHOLDER,
                    FLOW_STATUS_KEY: FLOW_STATUS_PROCESSING,
                }
            },
            "imGroupOpenSpaceModel": { "supportForward": true },
            "imRobotOpenSpaceModel": { "supportForward": true },
            "userIdType": 1
        });
        debug!(track_id = %out_track_id, payload = %create_body, "AI card create request");
        let resp = http
            .post(API_CREATE)
            .header("x-acs-dingtalk-access-token", &token)
            .json(&create_body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("create card network: {e}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("create card HTTP {status}: {body}"));
        }
        debug!(track_id = %out_track_id, response = %body, "AI card created");

        // 2) 投递到会话（私聊 vs 群聊）。
        // 参照钉钉官方 Python SDK (dingtalk-stream-sdk-python/card_replier.py)：
        //
        // 私聊：openSpaceId = "dtv1.card//IM_ROBOT.{sender_staff_id}"
        //        imRobotOpenDeliverModel = { "spaceType": "IM_ROBOT" }
        //
        // 群聊：openSpaceId = "dtv1.card//IM_GROUP.{conversation_id}"
        //        imGroupOpenDeliverModel = { "robotCode": robotCode }
        let mut deliver_body = json!({
            "outTrackId": out_track_id,
            "userIdType": 1,
        });
        if is_private {
            deliver_body["openSpaceId"] = json!(format!("dtv1.card//IM_ROBOT.{sender_staff_id}"));
            deliver_body["imRobotOpenDeliverModel"] = json!({
                "spaceType": "IM_ROBOT",
                "robotCode": robot_code,
            });
        } else {
            deliver_body["openSpaceId"] = json!(format!("dtv1.card//IM_GROUP.{conversation_id}"));
            deliver_body["imGroupOpenDeliverModel"] = json!({
                "robotCode": robot_code,
            });
        }
        debug!(track_id = %out_track_id, payload = %deliver_body, "AI card deliver request");
        let resp = http
            .post(API_DELIVER)
            .header("x-acs-dingtalk-access-token", &token)
            .json(&deliver_body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("deliver card network: {e}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("deliver card HTTP {status}: {body}"));
        }
        // 钉钉 deliver 可能 HTTP 200 但 result 里 success=false，需要检查
        if body.contains("\"success\":false") {
            return Err(format!("deliver card rejected: {body}"));
        }
        debug!(track_id = %out_track_id, response = %body, "AI card delivered");

        let card = Arc::new(Self {
            http,
            token_manager,
            robot_code,
            template_id,
            out_track_id,
            header,
            owner_staff_id: sender_staff_id.to_string(),
            created_at: std::time::Instant::now(),
            state: PMutex::new(CardState::default()),
        });

        // 投递后立刻推一帧"思考中"，避免卡片空白等待（与 Python 版 set_context 一致）。
        card.push_streaming(false).await;

        Ok(card)
    }

    /// 接收一段 thinking 增量——只保留最后一行（与 Python 版一致）。
    pub async fn feed_thinking(&self, content: &str) {
        {
            let mut s = self.state.lock();
            if s.finalized {
                return;
            }
            let latest = content
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .to_string();
            if latest.is_empty() {
                return;
            }
            s.latest_thinking = latest;
        }
        self.push_streaming(false).await;
    }

    /// 接收一次 tool 开始事件。`brief` 来自 Tool::brief（例如 "exec: npm test"）。
    pub async fn feed_tool(&self, brief: &str) {
        {
            let mut s = self.state.lock();
            if s.finalized {
                return;
            }
            if brief.is_empty() {
                return;
            }
            s.latest_tool = brief.to_string();
        }
        self.push_streaming(false).await;
    }

    /// 任务完成：关流 + 覆盖 cardData 为最终回复。
    ///
    /// 返回 `Err` 时调用方应 fallback 到纯文本回复，避免用户什么都收不到。
    pub async fn finalize(&self, reply: &str) -> Result<(), String> {
        {
            let mut s = self.state.lock();
            if s.finalized {
                return Ok(());
            }
            s.finalized = true;
        }
        // 关流：发一次 isFinalize=true 的空内容。
        if let Err(e) = self
            .stream_put(EMPTY_PLACEHOLDER, /*is_full*/ false, /*is_final*/ true)
            .await
        {
            warn!(error = %e, "AI card finalize: stream close failed");
        }
        // reply 由调用方（bot.rs）已拼好 header + 正文 + footer，不再重复拼 self.header。
        let final_content = if reply.trim().is_empty() {
            format!("{}（无输出）", self.header)
        } else {
            reply.to_string()
        };
        // 覆盖 cardData 为最终状态：结果写入 result，flowStatus 设为完成。
        self.update_card_data(&final_content)
            .await
            .map_err(|e| {
                warn!(error = %e, "AI card finalize: update cardData failed");
                e
            })?;
        info!(track_id = %self.out_track_id, "AI card finalized");
        Ok(())
    }

    /// 用户停止：在当前内容尾部追加"已终止"提示并关闭。
    pub async fn terminate(&self) {
        {
            let mut s = self.state.lock();
            if s.finalized {
                return;
            }
            s.finalized = true;
        }
        let _ = self
            .stream_put(EMPTY_PLACEHOLDER, false, true)
            .await;
        let body = self.render_body_with_suffix("\n\n⏹ 任务已被手动终止，如需继续请重新提问。");
        let _ = self.update_card_data(&body).await;
    }

    /// 把当前状态渲染成 markdown 并推一条 streaming 更新。
    async fn push_streaming(&self, is_final: bool) {
        let body = self.render_body();
        if let Err(e) = self.stream_put(&body, true, is_final).await {
            warn!(error = %e, "AI card streaming update failed");
        }
    }

    fn render_body(&self) -> String {
        let state = self.state.lock();
        let mut out = String::new();
        out.push_str(&self.header);
        out.push_str("\n\n🔍 思考中");
        if !state.latest_thinking.is_empty() {
            out.push_str("\n\n▎");
            out.push_str(&state.latest_thinking);
        }
        if !state.latest_tool.is_empty() {
            out.push_str("\n\n▎🔧 ");
            out.push_str(&state.latest_tool);
        }
        out
    }

    fn render_body_with_suffix(&self, suffix: &str) -> String {
        let mut body = self.render_body();
        body.push_str(suffix);
        body
    }

    /// PUT /v1.0/card/streaming
    async fn stream_put(
        &self,
        content: &str,
        is_full: bool,
        is_finalize: bool,
    ) -> Result<(), String> {
        let token = self
            .token_manager
            .get_token()
            .await
            .map_err(|e| format!("get_token: {e}"))?;
        let payload = json!({
            "outTrackId": self.out_track_id,
            "guid": Uuid::new_v4().to_string(),
            "key": CONTENT_KEY,
            "content": content,
            "isFull": is_full,
            "isFinalize": is_finalize,
            "isError": false,
        });
        debug!(
            track_id = %self.out_track_id,
            is_finalize,
            content_len = content.len(),
            "AI card stream_put"
        );
        let resp = self
            .http
            .put(API_STREAMING)
            .header("x-acs-dingtalk-access-token", &token)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("streaming network: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("streaming HTTP {status}: {body}"));
        }
        Ok(())
    }

    /// PUT /v1.0/card/instances —— 覆盖整个 cardData（最终态）。
    async fn update_card_data(&self, result: &str) -> Result<(), String> {
        let token = self
            .token_manager
            .get_token()
            .await
            .map_err(|e| format!("get_token: {e}"))?;
        let payload = json!({
            "outTrackId": self.out_track_id,
            "cardData": {
                "cardParamMap": {
                    FLOW_STATUS_KEY: FLOW_STATUS_FINISHED,
                    RESULT_KEY: result,
                }
            }
        });
        debug!(
            track_id = %self.out_track_id,
            payload = %payload,
            "AI card update_card_data request"
        );
        let resp = self
            .http
            .put(API_UPDATE)
            .header("x-acs-dingtalk-access-token", &token)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("update network: {e}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("update HTTP {status}: {body}"));
        }
        debug!(track_id = %self.out_track_id, response = %body, "AI card update_card_data response");
        Ok(())
    }

    pub fn out_track_id(&self) -> &str {
        &self.out_track_id
    }

    /// 丢一个 silencer 字段让编译器别因 template_id 未用而吵闹；
    /// 模板 id 在 create 之后仍保留，便于后续调试/重建。
    #[allow(dead_code)]
    pub(crate) fn template_id(&self) -> &str {
        &self.template_id
    }
}

/// 多张活跃卡片共享的注册表。
///
/// 键用 `chat_id`（私聊是 staff_id，群聊是 `conversation_id:staff_id`）——
/// 和 `OutboundEvent.chat_id` 一致，outbound dispatcher 查表直接命中。
pub type AiCardRegistry =
    Arc<PMutex<std::collections::HashMap<String, Arc<CardReplier>>>>;

/// 创建一个空注册表。
pub fn new_card_registry() -> AiCardRegistry {
    Arc::new(PMutex::new(std::collections::HashMap::new()))
}

/// 根据 `chat_id` 查已注册的卡片 replier。
pub fn find_card(registry: &AiCardRegistry, chat_id: &str) -> Option<Arc<CardReplier>> {
    registry.lock().get(chat_id).cloned()
}

/// 注册新卡片（同 chat_id 已有的会被直接覆盖）。
pub fn register_card(registry: &AiCardRegistry, chat_id: &str, card: Arc<CardReplier>) {
    registry.lock().insert(chat_id.to_string(), card);
}

/// 注销——任务完成/终止后调用。返回被移除的 replier（若有）。
pub fn unregister_card(registry: &AiCardRegistry, chat_id: &str) -> Option<Arc<CardReplier>> {
    registry.lock().remove(chat_id)
}

/// 清理超时未 finalize 的卡片。返回被清理的数量。
///
/// 由外部定时调用（如 outbound dispatcher 循环里、或独立 tokio::spawn）。
/// 超过 `CARD_TTL_SECS` 未 finalize 的卡片会被 terminate + unregister。
pub async fn reap_stale_cards(registry: &AiCardRegistry) -> usize {
    let stale: Vec<(String, Arc<CardReplier>)> = {
        let guard = registry.lock();
        guard
            .iter()
            .filter(|(_, card)| card.created_at.elapsed().as_secs() > CARD_TTL_SECS)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    let count = stale.len();
    for (chat_id, card) in stale {
        warn!(
            chat_id = %chat_id,
            track_id = %card.out_track_id(),
            age_secs = card.created_at.elapsed().as_secs(),
            "Reaping stale AI card"
        );
        card.terminate().await;
        registry.lock().remove(&chat_id);
    }
    count
}

/// 反查：根据 outTrackId 找到 (chat_id, replier)。活跃卡片数通常很少（每会话最多一张），
/// 所以用线性扫描；如果未来规模变大再加一个 out_track_id → chat_id 的辅助索引。
pub fn find_card_by_track_id(
    registry: &AiCardRegistry,
    track_id: &str,
) -> Option<(String, Arc<CardReplier>)> {
    let guard = registry.lock();
    for (chat_id, card) in guard.iter() {
        if card.out_track_id() == track_id {
            return Some((chat_id.clone(), card.clone()));
        }
    }
    None
}

/// 卡片按钮回调处理器——目前只处理"停止当前任务"按钮。
///
/// 收到回调后：
/// 1. 从 payload 抽 outTrackId 和点击者 userId。
/// 2. 据 outTrackId 反查 registry，拿到 (chat_id, replier)。
/// 3. **Owner 校验**：只有任务发起者（`owner_staff_id`）能停，防止群里被误触。
/// 4. `Orchestrator::cancel_for_identity` 触发取消；`CardReplier::terminate` 贴"已终止"尾部。
pub struct AiCardCallbackHandler {
    orchestrator: Arc<Orchestrator>,
    registry: AiCardRegistry,
}

impl AiCardCallbackHandler {
    pub fn new(orchestrator: Arc<Orchestrator>, registry: AiCardRegistry) -> Arc<Self> {
        Arc::new(Self { orchestrator, registry })
    }
}

#[async_trait]
impl ChatbotHandler for AiCardCallbackHandler {
    async fn process(&self, callback: &CallbackMessage) -> (u16, String) {
        // payload 字段名参考钉钉 AI 卡片回调文档。不同网关版本字段略有差异，
        // 按容错方式逐个尝试，拿到就用。
        let data = &callback.data;
        let out_track_id = data
            .get("outTrackId")
            .or_else(|| data.get("cardInstanceId"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // 点击者的 staff_id（用于 owner 校验，避免群聊中被误触）。
        let clicker = data
            .get("userId")
            .or_else(|| data.get("staffId"))
            .or_else(|| data.get("operatorUserId"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        info!(
            track_id = %out_track_id,
            clicker = %clicker,
            "AI card callback received"
        );

        if out_track_id.is_empty() {
            warn!("AI card callback missing outTrackId, ignoring");
            return (200, "OK".into());
        }

        let Some((_chat_id, card)) = find_card_by_track_id(&self.registry, out_track_id) else {
            warn!(track_id = %out_track_id, "AI card callback for unknown card (already finished?)");
            return (200, "OK".into());
        };

        if !clicker.is_empty() && clicker != card.owner_staff_id {
            warn!(
                clicker = %clicker,
                owner = %card.owner_staff_id,
                "AI card stop click ignored (non-owner)"
            );
            return (200, "OK".into());
        }

        // 触发取消（把活跃 agent loop 引向 cancellation）+ 卡片贴尾部。
        // channel / chat_id 从 owner_staff_id 还原——私聊用 staff_id，
        // 群聊 chat_id 形如 "convId:staffId"。这里 chat_id 在反查 registry 时已拿到。
        let chat_id_ref = _chat_id.clone();
        let (channel, conv_id, user_ref) = if chat_id_ref.contains(':') {
            let parts: Vec<&str> = chat_id_ref.splitn(2, ':').collect();
            ("dingtalk_group", Some(parts[0].to_string()), parts[1].to_string())
        } else {
            ("dingtalk_private", None, chat_id_ref.clone())
        };

        let cancelled = self.orchestrator.cancel_for_identity(
            &user_ref,
            channel,
            &chat_id_ref,
            conv_id.as_deref(),
        );
        info!(
            track_id = %out_track_id,
            cancelled,
            "AI card stop button handled"
        );

        // 不直接 unregister / terminate 卡片——让 bot.process() 的正常收尾路径去做，
        // 避免和正在进行的 finalize 抢写。Agent loop 收到 cancel 后返回
        // `CANCELLED_REPLY`，bot 会把它写进 finalize 里。

        (200, "OK".into())
    }
}
