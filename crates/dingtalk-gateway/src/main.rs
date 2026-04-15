//! DingTalk Gateway —— 钉钉消息网关。
//!
//! 向钉钉服务器建立多条 WebSocket 连接（可配置），
//! 将收到的消息统一分发给多个后端 TyClaw 实例。
//!
//! 架构：
//! ```
//! DingTalk Server
//!     ↕ (N 条 WebSocket，默认 30)
//! Gateway
//!     ↕ (M 条 WebSocket，每个 TyClaw 实例一条)
//! TyClaw Instance 1..M
//! ```
//!
//! TyClaw 实例连接网关后发送 register 消息注册 label。
//! 路由模式（route_table / hash）按 sender_id 统一路由。
//! 实例断开后，其会话自动重新分配到其他实例。

mod config;
mod upstream;
mod downstream;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tokio::sync::mpsc;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "dingtalk-gateway", about = "DingTalk message gateway for TyClaw")]
struct Args {
    /// 配置文件路径
    #[arg(short, long, default_value = "config.yaml")]
    config: String,
}

#[tokio::main]
async fn main() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).init();

    let args = Args::parse();
    let cfg = config::load(std::path::Path::new(&args.config));

    // 校验 weights 配置
    if !cfg.routing.weights.is_empty() {
        let sum: u32 = cfg.routing.weights.values().sum();
        if sum != 100 {
            eprintln!(
                "routing.weights must sum to 100, got {} (weights: {:?})",
                sum, cfg.routing.weights
            );
            std::process::exit(1);
        }
    }

    info!(
        upstream_connections = cfg.dingtalk.upstream_connections,
        listen_addr = %cfg.gateway.listen_addr,
        client_id = %cfg.dingtalk.client_id,
        routing_mode = %cfg.routing.mode,
        routing_default = %cfg.routing.default,
        "DingTalk Gateway starting"
    );

    // 消息队列：上游 → 分发器
    let (msg_tx, mut msg_rx) = mpsc::channel::<upstream::IncomingMessage>(1024);

    // 启动上游连接池
    upstream::start_pool(
        cfg.dingtalk.client_id,
        cfg.dingtalk.client_secret,
        cfg.dingtalk.upstream_connections,
        msg_tx,
    );

    // 启动下游管理器
    let downstream = downstream::DownstreamManager::new(
        cfg.gateway.ready_wait_secs,
        cfg.routing,
    );
    let downstream_for_listen = Arc::clone(&downstream);
    let listen_addr = cfg.gateway.listen_addr.clone();
    tokio::spawn(async move {
        downstream_for_listen.listen(&listen_addr).await;
    });

    // 等待后端就绪（第一个连入后开始倒计时，窗口内无新连入则就绪）
    let downstream_for_ready = Arc::clone(&downstream);
    tokio::spawn(async move {
        downstream_for_ready.wait_ready().await;
    });

    // 注册 SIGHUP 信号（在进入事件循环之前注册，避免竞态）
    let config_path = PathBuf::from(&args.config);
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .expect("Failed to register SIGHUP handler");

    // 消息分发 + 热加载循环
    info!("Message dispatcher started");
    let mut total_dispatched: u64 = 0;
    loop {
        tokio::select! {
            Some(msg) = msg_rx.recv() => {
                total_dispatched += 1;
                if total_dispatched % 100 == 0 {
                    info!(total_dispatched, "Dispatch milestone");
                }
                downstream.dispatch(&msg).await;
            }
            _ = sighup.recv() => {
                info!("Received SIGHUP, reloading routing config from {:?}", config_path);
                match config::reload_routing(&config_path) {
                    Ok(new_routing) => {
                        info!(
                            mode = %new_routing.mode,
                            default = %new_routing.default,
                            rules = new_routing.rules.len(),
                            weights = ?new_routing.weights,
                            "Routing config reloaded"
                        );
                        downstream.swap_routing(new_routing);
                    }
                    Err(e) => {
                        warn!(error = %e, "Routing config reload failed, keeping current config");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Received SIGINT, shutting down");
                break;
            }
        }
    }
}
