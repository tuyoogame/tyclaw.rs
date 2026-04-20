use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;

use super::service::TimerService;
use super::types::TimerSchedule;
use crate::base::{brief_truncate, RiskLevel, Tool};

pub struct TimerTool {
    service: Arc<TimerService>,
}

impl TimerTool {
    pub fn new(service: Arc<TimerService>) -> Self {
        Self { service }
    }

    /// 获取当前 user_id，空则报错。
    fn require_user_id(&self) -> Result<String, String> {
        let uid = self.service.current_user_id();
        if uid.is_empty() {
            Err("Error: user_id is required for timer operations".to_string())
        } else {
            Ok(uid)
        }
    }

    async fn handle_add(&self, params: &HashMap<String, Value>) -> String {
        let user_id = match self.require_user_id() {
            Ok(uid) => uid,
            Err(e) => return e,
        };

        let message = match params.get("message").and_then(|v| v.as_str()) {
            Some(m) if !m.is_empty() => m,
            _ => return "Error: message is required for add".to_string(),
        };

        let channel = self.service.current_channel();
        let chat_id = self.service.current_chat_id();
        let channel = if channel.is_empty() {
            "cli".to_string()
        } else {
            channel
        };
        let chat_id = if chat_id.is_empty() {
            "direct".to_string()
        } else {
            chat_id
        };

        // LLM/tooling often sends delay_seconds/every_seconds as 0 when unused; treat as unset
        // so `at` + `tz` does not spuriously conflict with tz+delay rules.
        let delay_seconds = params
            .get("delay_seconds")
            .and_then(|v| v.as_u64())
            .filter(|&n| n > 0);
        let every_seconds = params
            .get("every_seconds")
            .and_then(|v| v.as_u64())
            .filter(|&n| n > 0);
        // LLM often sends cron_expr/at as "" when unused; treat empty strings as unset.
        let cron_expr = params
            .get("cron_expr")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let tz = params
            .get("tz")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let at = params
            .get("at")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        if tz.is_some() && (delay_seconds.is_some() || every_seconds.is_some()) {
            return "Error: tz cannot be used with delay_seconds or every_seconds".to_string();
        }

        // Priority: at > delay_seconds > every_seconds > cron_expr
        // `at` takes priority so LLM sending both `at` + empty `cron_expr` still works correctly.
        let (schedule, delete_after) = if let Some(at_str) = at {
            match chrono::NaiveDateTime::parse_from_str(at_str, "%Y-%m-%dT%H:%M:%S") {
                Ok(dt) => {
                    let at_ms = if let Some(tz_name) = tz {
                        match tz_name.parse::<chrono_tz::Tz>() {
                            Ok(tz_val) => {
                                dt.and_local_timezone(tz_val)
                                    .single()
                                    .map(|d| d.timestamp_millis())
                                    .unwrap_or_else(|| dt.and_utc().timestamp_millis())
                            }
                            Err(_) => {
                                return format!("Error: unknown timezone '{}'", tz_name);
                            }
                        }
                    } else {
                        dt.and_local_timezone(chrono::Local)
                            .single()
                            .map(|d| d.timestamp_millis())
                            .unwrap_or_else(|| dt.and_utc().timestamp_millis())
                    };
                    (TimerSchedule::At { at_ms }, true)
                }
                Err(_) => {
                    return format!(
                        "Error: invalid datetime format '{}'. Expected: YYYY-MM-DDTHH:MM:SS",
                        at_str
                    );
                }
            }
        } else if let Some(secs) = delay_seconds {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            (
                TimerSchedule::At {
                    at_ms: now_ms + (secs as i64 * 1000),
                },
                true,
            )
        } else if let Some(secs) = every_seconds {
            (
                TimerSchedule::Every {
                    interval_ms: secs * 1000,
                },
                false,
            )
        } else if let Some(expr) = cron_expr {
            (
                TimerSchedule::Cron {
                    expr: expr.to_string(),
                    tz: tz.map(|s| s.to_string()),
                },
                false,
            )
        } else {
            return "Error: one of delay_seconds, every_seconds, cron_expr, or at is required"
                .to_string();
        };

        let name: String = message.chars().take(30).collect();
        let conversation_id = self.service.current_conversation_id();

        match self
            .service
            .add_job(
                &user_id,
                &name,
                schedule,
                message,
                true,
                Some(&channel),
                Some(&chat_id),
                if conversation_id.is_empty() {
                    None
                } else {
                    Some(&conversation_id)
                },
                delete_after,
            )
            .await
        {
            Ok(job) => format!("Created job '{}' (id: {})", job.name, job.id),
            Err(e) => format!("Error: {e}"),
        }
    }

    async fn handle_list(&self) -> String {
        let user_id = match self.require_user_id() {
            Ok(uid) => uid,
            Err(e) => return e,
        };

        let jobs = self.service.list_jobs(&user_id, false).await;
        if jobs.is_empty() {
            return "No scheduled jobs.".to_string();
        }

        let mut lines = Vec::new();
        for j in &jobs {
            let timing = format_timing(&j.schedule);
            let mut parts = vec![format!("- {} (id: {}, {})", j.name, j.id, timing)];
            if let Some(last_ms) = j.state.last_run_at_ms {
                let status = j.state.last_status.as_deref().unwrap_or("unknown");
                let dt = chrono::DateTime::from_timestamp_millis(last_ms)
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_default();
                let mut info = format!("  Last run: {} — {}", dt, status);
                if let Some(err) = &j.state.last_error {
                    info.push_str(&format!(" ({})", err));
                }
                parts.push(info);
            }
            if let Some(next_ms) = j.state.next_run_at_ms {
                let dt = chrono::DateTime::from_timestamp_millis(next_ms)
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_default();
                parts.push(format!("  Next run: {}", dt));
            }
            lines.push(parts.join("\n"));
        }
        format!("Scheduled jobs:\n{}", lines.join("\n"))
    }

    async fn handle_remove(&self, params: &HashMap<String, Value>) -> String {
        let user_id = match self.require_user_id() {
            Ok(uid) => uid,
            Err(e) => return e,
        };

        let job_id = match params.get("job_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return "Error: job_id is required for remove".to_string(),
        };
        if self.service.remove_job(&user_id, job_id).await {
            format!("Removed job {}", job_id)
        } else {
            format!("Job {} not found", job_id)
        }
    }
}

fn format_timing(schedule: &TimerSchedule) -> String {
    match schedule {
        TimerSchedule::Cron { expr, tz } => {
            let tz_str = tz
                .as_deref()
                .map(|t| format!(" ({})", t))
                .unwrap_or_default();
            format!("cron: {}{}", expr, tz_str)
        }
        TimerSchedule::Every { interval_ms } => {
            let ms = *interval_ms;
            if ms % 3_600_000 == 0 {
                format!("every {}h", ms / 3_600_000)
            } else if ms % 60_000 == 0 {
                format!("every {}m", ms / 60_000)
            } else if ms % 1000 == 0 {
                format!("every {}s", ms / 1000)
            } else {
                format!("every {}ms", ms)
            }
        }
        TimerSchedule::At { at_ms } => {
            let dt = chrono::DateTime::from_timestamp_millis(*at_ms)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default();
            format!("at {}", dt)
        }
    }
}

#[async_trait]
impl Tool for TimerTool {
    fn name(&self) -> &str {
        "timer"
    }

    fn description(&self) -> &str {
        "Schedule delayed or recurring tasks. Actions: add, list, remove.\n\
         SCHEDULING RULES (strictly follow):\n\
         - One-time at a specific datetime → use `at` (ISO datetime, e.g. '2026-04-17T11:30:00') + optionally `tz`. DO NOT use cron_expr for this case.\n\
         - One-time after a delay → use `delay_seconds` (must be > 0). Do NOT set tz together.\n\
         - Recurring on a schedule → use `cron_expr` (e.g. '0 9 * * *') + optionally `tz`.\n\
         - Repeating every N seconds → use `every_seconds` (must be > 0). Do NOT set tz together.\n\
         Exactly ONE of [at, delay_seconds, cron_expr, every_seconds] must be provided. Do NOT mix them.\n\
         Omit fields that are not needed — do not pass 0 as a placeholder."
    }

    fn brief(&self, args: &HashMap<String, Value>) -> Option<String> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        if let Some(name) = args.get("name").and_then(|v| v.as_str()) {
            Some(format!("timer {action}: {}", brief_truncate(name, 40)))
        } else if let Some(msg) = args.get("message").and_then(|v| v.as_str()) {
            Some(format!("timer {action}: {}", brief_truncate(msg, 40)))
        } else {
            Some(format!("timer {action}"))
        }
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "list", "remove"],
                    "description": "Action to perform"
                },
                "message": {
                    "type": "string",
                    "description": "Task instruction (required for add)"
                },
                "at": {
                    "type": "string",
                    "description": "One-time execution at a specific datetime (ISO 8601, e.g. '2026-04-17T11:30:00'). Use this when the user says 'at HH:MM' or 'today HH:MM' or any specific time point. Interpreted as local timezone unless `tz` is also set. Mutually exclusive with delay_seconds, every_seconds, cron_expr."
                },
                "tz": {
                    "type": "string",
                    "description": "IANA timezone for `at` or `cron_expr` (e.g. 'Asia/Shanghai'). Do NOT use with delay_seconds or every_seconds."
                },
                "delay_seconds": {
                    "type": "integer",
                    "description": "Execute once after N seconds from now (must be > 0). Do NOT use with tz, cron_expr, or at."
                },
                "every_seconds": {
                    "type": "integer",
                    "description": "Repeat every N seconds (must be > 0). Do NOT use with tz, cron_expr, or at."
                },
                "cron_expr": {
                    "type": "string",
                    "description": "Cron expression for recurring tasks (e.g. '0 9 * * *' = every day at 09:00). Use ONLY for recurring schedules, NOT for one-time tasks. Do NOT use when the user asks for a specific one-time time point."
                },
                "job_id": {
                    "type": "string",
                    "description": "Job ID (required for remove)"
                }
            },
            "required": ["action"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Read
    }

    async fn execute(&self, params: HashMap<String, Value>) -> String {
        let action = match params.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => return "Error: action is required".to_string(),
        };

        match action.as_str() {
            "add" => {
                if self.service.is_in_timer_context() {
                    return "Error: cannot schedule new jobs from within a timer callback"
                        .to_string();
                }
                self.handle_add(&params).await
            }
            "list" => self.handle_list().await,
            "remove" => self.handle_remove(&params).await,
            _ => format!("Unknown action: {}", action),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_svc() -> (Arc<TimerService>, tokio::sync::mpsc::Receiver<super::super::types::TimerJob>) {
        let dir = tempfile::tempdir().unwrap();
        // Leak the tempdir so it lives for the test duration
        let dir = Box::leak(Box::new(dir));
        TimerService::new(dir.path())
    }

    #[tokio::test]
    async fn test_add_requires_user_id() {
        let (svc, _rx) = make_svc();
        let tool = TimerTool::new(svc);

        let mut params = HashMap::new();
        params.insert("action".to_string(), json!("add"));
        params.insert("message".to_string(), json!("test"));
        params.insert("delay_seconds".to_string(), json!(60));
        // No TIMER_CURRENT_USER_ID set
        let result = tool.execute(params).await;
        assert!(result.contains("user_id is required"));
    }

    #[tokio::test]
    async fn test_add_requires_message() {
        let (svc, _rx) = make_svc();
        let tool = TimerTool::new(svc);

        let mut params = HashMap::new();
        params.insert("action".to_string(), json!("add"));
        params.insert("delay_seconds".to_string(), json!(60));
        let result = crate::timer::TIMER_CURRENT_USER_ID
            .scope("alice".to_string(), tool.execute(params))
            .await;
        assert!(result.contains("message is required"));
    }

    #[tokio::test]
    async fn test_add_success() {
        let (svc, _rx) = make_svc();
        let tool = TimerTool::new(svc);

        let mut params = HashMap::new();
        params.insert("action".to_string(), json!("add"));
        params.insert("message".to_string(), json!("remind me"));
        params.insert("delay_seconds".to_string(), json!(300));
        let result = crate::timer::TIMER_CURRENT_USER_ID
            .scope(
                "alice".to_string(),
                crate::timer::TIMER_CURRENT_CHANNEL.scope(
                    "dingtalk_group".to_string(),
                    crate::timer::TIMER_CURRENT_CHAT_ID
                        .scope("conv123".to_string(), tool.execute(params)),
                ),
            )
            .await;
        assert!(result.starts_with("Created job"));
    }

    #[tokio::test]
    async fn test_recursive_prevention() {
        let (svc, _rx) = make_svc();
        let tool = TimerTool::new(svc);

        let mut params = HashMap::new();
        params.insert("action".to_string(), json!("add"));
        params.insert("message".to_string(), json!("recursive"));
        params.insert("delay_seconds".to_string(), json!(60));
        let result = crate::timer::TIMER_IN_CONTEXT
            .scope(
                true,
                crate::timer::TIMER_CURRENT_USER_ID.scope(
                    "alice".to_string(),
                    crate::timer::TIMER_CURRENT_CHANNEL.scope(
                        "cli".to_string(),
                        crate::timer::TIMER_CURRENT_CHAT_ID
                            .scope("direct".to_string(), tool.execute(params)),
                    ),
                ),
            )
            .await;
        assert!(result.contains("cannot schedule"));
    }

    #[tokio::test]
    async fn test_list_empty() {
        let (svc, _rx) = make_svc();
        let tool = TimerTool::new(svc);
        let mut params = HashMap::new();
        params.insert("action".to_string(), json!("list"));
        let result = crate::timer::TIMER_CURRENT_USER_ID
            .scope("alice".to_string(), tool.execute(params))
            .await;
        assert_eq!(result, "No scheduled jobs.");
    }

    #[tokio::test]
    async fn test_remove_not_found() {
        let (svc, _rx) = make_svc();
        let tool = TimerTool::new(svc);
        let mut params = HashMap::new();
        params.insert("action".to_string(), json!("remove"));
        params.insert("job_id".to_string(), json!("nonexistent"));
        let result = crate::timer::TIMER_CURRENT_USER_ID
            .scope("alice".to_string(), tool.execute(params))
            .await;
        assert!(result.contains("not found"));
    }

    #[tokio::test]
    async fn test_at_uses_local_timezone() {
        let (svc, _rx) = make_svc();
        let tool = TimerTool::new(svc.clone());

        let mut params = HashMap::new();
        params.insert("action".to_string(), json!("add"));
        params.insert("message".to_string(), json!("local tz test"));
        params.insert("at".to_string(), json!("2030-06-15T19:00:00"));
        let result = crate::timer::TIMER_CURRENT_USER_ID
            .scope(
                "alice".to_string(),
                crate::timer::TIMER_CURRENT_CHANNEL.scope(
                    "cli".to_string(),
                    crate::timer::TIMER_CURRENT_CHAT_ID
                        .scope("direct".to_string(), tool.execute(params)),
                ),
            )
            .await;
        assert!(result.starts_with("Created job"));

        let jobs = svc.list_jobs("alice", true).await;
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        if let TimerSchedule::At { at_ms } = &job.schedule {
            // 用本地时区解析 2030-06-15T19:00:00
            let expected = chrono::NaiveDateTime::parse_from_str(
                "2030-06-15T19:00:00",
                "%Y-%m-%dT%H:%M:%S",
            )
            .unwrap()
            .and_local_timezone(chrono::Local)
            .single()
            .unwrap()
            .timestamp_millis();
            assert_eq!(*at_ms, expected);
        } else {
            panic!("Expected At schedule");
        }
    }

    #[tokio::test]
    async fn test_at_with_explicit_tz() {
        let (svc, _rx) = make_svc();
        let tool = TimerTool::new(svc.clone());

        let mut params = HashMap::new();
        params.insert("action".to_string(), json!("add"));
        params.insert("message".to_string(), json!("tz test"));
        params.insert("at".to_string(), json!("2030-06-15T19:00:00"));
        params.insert("tz".to_string(), json!("Asia/Shanghai"));
        let result = crate::timer::TIMER_CURRENT_USER_ID
            .scope(
                "alice".to_string(),
                crate::timer::TIMER_CURRENT_CHANNEL.scope(
                    "cli".to_string(),
                    crate::timer::TIMER_CURRENT_CHAT_ID
                        .scope("direct".to_string(), tool.execute(params)),
                ),
            )
            .await;
        assert!(result.starts_with("Created job"));

        let jobs = svc.list_jobs("alice", true).await;
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        if let TimerSchedule::At { at_ms } = &job.schedule {
            // Asia/Shanghai = UTC+8, 19:00 CST = 11:00 UTC
            let expected = chrono::NaiveDateTime::parse_from_str(
                "2030-06-15T11:00:00",
                "%Y-%m-%dT%H:%M:%S",
            )
            .unwrap()
            .and_utc()
            .timestamp_millis();
            assert_eq!(*at_ms, expected);
        } else {
            panic!("Expected At schedule");
        }
    }

    #[tokio::test]
    async fn test_delay_seconds_zero_treated_as_unset() {
        let (svc, _rx) = make_svc();
        let tool = TimerTool::new(svc);

        let mut params = HashMap::new();
        params.insert("action".to_string(), json!("add"));
        params.insert("message".to_string(), json!("zero delay"));
        params.insert("delay_seconds".to_string(), json!(0));
        let result = crate::timer::TIMER_CURRENT_USER_ID
            .scope(
                "alice".to_string(),
                crate::timer::TIMER_CURRENT_CHANNEL.scope(
                    "cli".to_string(),
                    crate::timer::TIMER_CURRENT_CHAT_ID
                        .scope("direct".to_string(), tool.execute(params)),
                ),
            )
            .await;
        // 0 is ignored like omitted — no schedule source remains
        assert!(result.contains("one of delay_seconds, every_seconds, cron_expr, or at is required"));
    }

    #[tokio::test]
    async fn test_at_with_tz_succeeds_when_delay_and_every_are_zero() {
        let (svc, _rx) = make_svc();
        let tool = TimerTool::new(svc);

        let mut params = HashMap::new();
        params.insert("action".to_string(), json!("add"));
        params.insert("message".to_string(), json!("hello"));
        params.insert("at".to_string(), json!("2030-06-15T19:00:00"));
        params.insert("tz".to_string(), json!("Asia/Shanghai"));
        params.insert("delay_seconds".to_string(), json!(0));
        params.insert("every_seconds".to_string(), json!(0));
        let result = crate::timer::TIMER_CURRENT_USER_ID
            .scope(
                "alice".to_string(),
                crate::timer::TIMER_CURRENT_CHANNEL.scope(
                    "cli".to_string(),
                    crate::timer::TIMER_CURRENT_CHAT_ID
                        .scope("direct".to_string(), tool.execute(params)),
                ),
            )
            .await;
        assert!(
            result.starts_with("Created job"),
            "unexpected: {result}"
        );
    }

    #[tokio::test]
    async fn test_at_wins_over_empty_cron_expr() {
        // LLM sometimes sends at + cron_expr:"" + delay_seconds:0 + every_seconds:0
        // at should win and succeed.
        let (svc, _rx) = make_svc();
        let tool = TimerTool::new(svc);

        let mut params = HashMap::new();
        params.insert("action".to_string(), json!("add"));
        params.insert("message".to_string(), json!("greeting"));
        params.insert("at".to_string(), json!("2030-06-15T11:30:00"));
        params.insert("tz".to_string(), json!("Asia/Shanghai"));
        params.insert("cron_expr".to_string(), json!(""));
        params.insert("delay_seconds".to_string(), json!(0));
        params.insert("every_seconds".to_string(), json!(0));
        let result = crate::timer::TIMER_CURRENT_USER_ID
            .scope(
                "alice".to_string(),
                crate::timer::TIMER_CURRENT_CHANNEL.scope(
                    "cli".to_string(),
                    crate::timer::TIMER_CURRENT_CHAT_ID
                        .scope("direct".to_string(), tool.execute(params)),
                ),
            )
            .await;
        assert!(result.starts_with("Created job"), "unexpected: {result}");
    }

    #[tokio::test]
    async fn test_every_seconds_with_empty_tz_succeeds() {
        let (svc, _rx) = make_svc();
        let tool = TimerTool::new(svc);

        let mut params = HashMap::new();
        params.insert("action".to_string(), json!("add"));
        params.insert("message".to_string(), json!("hello every 3 min"));
        params.insert("every_seconds".to_string(), json!(180));
        params.insert("tz".to_string(), json!(""));
        params.insert("at".to_string(), json!(""));
        params.insert("cron_expr".to_string(), json!(""));
        params.insert("delay_seconds".to_string(), json!(0));
        params.insert("job_id".to_string(), json!(""));
        let result = crate::timer::TIMER_CURRENT_USER_ID
            .scope(
                "alice".to_string(),
                crate::timer::TIMER_CURRENT_CHANNEL.scope(
                    "cli".to_string(),
                    crate::timer::TIMER_CURRENT_CHAT_ID
                        .scope("direct".to_string(), tool.execute(params)),
                ),
            )
            .await;
        assert!(
            result.starts_with("Created job"),
            "unexpected: {result}"
        );
    }

    #[tokio::test]
    async fn test_tz_rejected_with_delay() {
        let (svc, _rx) = make_svc();
        let tool = TimerTool::new(svc);

        let mut params = HashMap::new();
        params.insert("action".to_string(), json!("add"));
        params.insert("message".to_string(), json!("tz with delay"));
        params.insert("delay_seconds".to_string(), json!(60));
        params.insert("tz".to_string(), json!("Asia/Shanghai"));
        let result = crate::timer::TIMER_CURRENT_USER_ID
            .scope(
                "alice".to_string(),
                crate::timer::TIMER_CURRENT_CHANNEL.scope(
                    "cli".to_string(),
                    crate::timer::TIMER_CURRENT_CHAT_ID
                        .scope("direct".to_string(), tool.execute(params)),
                ),
            )
            .await;
        assert!(result.contains("tz cannot be used with delay_seconds"));
    }
}
