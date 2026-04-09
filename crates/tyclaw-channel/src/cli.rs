//! CLI 交互循环 —— 基于 rustyline 的交互式 REPL。
//!
//! 工作流程：
//! 1. 打印欢迎信息
//! 2. 循环读取用户输入（支持行编辑和历史）
//! 3. 跳过空行；输入 exit/quit 退出
//! 4. 将输入通过 BusHandle 推送到 MessageBus
//! 5. 回复和进度由 Dispatcher 打印

use std::path::PathBuf;

use tyclaw_orchestration::{BusHandle, InboundMessage};

/// 交互式 CLI 通道。
///
/// 封装了用户身份和工作区信息，通过 BusHandle 将消息推送到 MessageBus。
pub struct CliChannel {
    user_id: String,
    workspace_id: String,
}

impl CliChannel {
    pub fn new(user_id: impl Into<String>, workspace_id: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            workspace_id: workspace_id.into(),
        }
    }

    /// 运行交互式 CLI 循环（REPL）。
    ///
    /// 退出条件：输入 exit / quit / Ctrl+D / 读取错误。
    pub async fn run(
        &self,
        bus_handle: BusHandle,
        timer_service: &tyclaw_tools::timer::TimerService,
    ) {
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel::<String>(1);

        tokio::task::spawn_blocking(move || {
            use rustyline::error::ReadlineError;
            use rustyline::DefaultEditor;

            println!("TyClaw Agent (Rust edition) — type 'exit' to quit, '/new' for new session\n");
            println!("Tips: Ctrl+A 行首, Ctrl+E 行尾, ↑↓ 历史, Ctrl+R 搜索历史\n");

            let mut rl = DefaultEditor::new().expect("Failed to init readline");
            let history_path = cli_history_path();
            rl.load_history(&history_path).ok();

            loop {
                match rl.readline("You> ") {
                    Ok(line) => {
                        let trimmed = line.trim().to_string();
                        if trimmed.is_empty() {
                            continue;
                        }
                        rl.add_history_entry(&trimmed).ok();
                        if trimmed.eq_ignore_ascii_case("exit")
                            || trimmed.eq_ignore_ascii_case("quit")
                        {
                            println!("Goodbye!");
                            break;
                        }
                        if input_tx.blocking_send(trimmed).is_err() {
                            break;
                        }
                    }
                    Err(ReadlineError::Interrupted) => {
                        println!("^C");
                        continue;
                    }
                    Err(e) => {
                        println!("Goodbye! (reason: {e})");
                        let _ = input_tx.blocking_send("__exit__".into());
                        break;
                    }
                }
            }
            rl.save_history(&history_path).ok();
        });

        while let Some(input) = input_rx.recv().await {
            if input == "__exit__" {
                break;
            }

            let msg = InboundMessage {
                content: input,
                user_id: self.user_id.clone(),
                user_name: "cli_user".into(),
                workspace_id: self.workspace_id.clone(),
                channel: "cli".into(),
                chat_id: "direct".into(),
                conversation_id: None,
                images: vec![],
                files: vec![],
                reply_tx: None,
                is_timer: false,
                emotion_context: None,
            };

            if let Err(e) = bus_handle.send(msg).await {
                eprintln!("Error: failed to send message to bus: {e}");
            }
        }

        timer_service.stop();
    }
}

fn cli_history_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".tyclaw_cli_history")
}
