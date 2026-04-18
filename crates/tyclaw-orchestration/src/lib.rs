//! 编排层 —— 连接所有层的中枢。
//!
//! 本 crate 是 TyClaw 架构的最顶层，将以下组件串联起来：
//! - Context（上下文构建）→ AgentLoop（ReAct 循环）→ Gate（权限门禁）
//! - Memory（案例记忆 + 记忆合并）→ Audit（审计日志）
//! - Session（会话管理）→ Skills（技能发现）

/// 不可变的应用级上下文
pub mod app_context;

/// 有状态的持久化服务层
pub(crate) mod persistence;

/// 编排器模块 —— 端到端请求处理
pub mod orchestrator;

/// 会话管理模块 —— JSONL 格式的对话历史持久化
pub mod session_manager;

/// 技能管理模块 —— 内建和个人技能的发现与分类
pub mod skill_manager;

/// 子任务调度引擎 —— DAG 拆分 → 并行调度 → 结果归并
pub mod subtasks;

/// 编排器核心类型与常量
pub mod types;

/// 编排器构建器
pub mod builder;

/// 公共配置结构（app 和 client 共用）
pub mod config;

/// 消息总线 —— 解耦通道与编排器
pub mod bus;

/// 历史消息处理（去重、裁剪、配对修复）
pub(crate) mod history;

/// 编排器辅助函数（技能路由、案例优化、预算计算）
pub(crate) mod helpers;

/// Memory 段落相关性过滤
pub(crate) mod memory_filter;

/// 请求处理器 —— 14 步端到端消息处理流程
mod handler;

/// Workspace 超时回收后台任务
pub(crate) mod reaper;

/// 终端输出工具（ANSI 滚动区域内打印）
pub mod term;

// 重新导出核心类型
pub use app_context::AppContext;
pub use builder::OrchestratorBuilder;
pub use bus::{BusHandle, InboundMessage, MessageBus, OutboundEvent};
pub use config::{load_yaml, mask_secret, BaseConfig, LlmConfig, LoggingConfig, WorkspaceRuntimeConfig};
pub use orchestrator::Orchestrator;
pub use session_manager::{Session, SessionManager};
pub use skill_manager::SkillManager;
pub use tyclaw_agent::runtime::{parse_thinking_prefix, OnProgress};
pub use tyclaw_control::ControlConfig;
pub use tyclaw_control::WorkspaceConfig;
pub use types::{AgentResponse, OrchestratorFeatures, RequestContext};
