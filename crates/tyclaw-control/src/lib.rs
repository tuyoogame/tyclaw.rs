//! 权限控制层：执行门禁、RBAC 角色管理、审计日志和工作区管理。
//!
//! 本 crate 负责系统的安全和治理，包括：
//! - RBAC（基于角色的访问控制）：分层角色权限管理
//! - 执行门禁（ExecutionGate）：判断工具调用是否被允许
//! - 审计日志（AuditLog）：追加写入的操作记录
//! - 工作区管理（WorkspaceManager）：多租户隔离

/// RBAC 模块 —— 角色层级和权限定义
pub mod rbac;

/// 执行门禁模块 —— 根据角色和风险等级判断工具调用许可
pub mod gate;

/// 审计日志模块 —— 按工作区追加记录所有操作
pub mod audit;

/// 工作区管理模块 —— 多租户工作区隔离和目录管理
pub mod workspace;

/// 速率限制模块 —— 基于滑动窗口的请求频率控制
pub mod rate_limiter;

/// 配置模块 —— control.yaml 的配置结构
pub mod config;

// 重新导出核心类型
pub use audit::{AuditEntry, AuditLog};
pub use config::ControlConfig;
pub use gate::{ExecutionGate, Judgment, JudgmentAction};
pub use rate_limiter::RateLimiter;
pub use rbac::RBACManager;
pub use workspace::{
    PathConfig, RequestIdentity, Workspace, WorkspaceConfig, WorkspaceKeyStrategy,
    WorkspaceManager, workspace_path, workspace_path_in,
};
