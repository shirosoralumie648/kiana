//! Hook 审计接口。
//!
//! 审计与决策分离：async hook 的结果只进审计；同步 hook 也写一条审计记录，方便
//! 会话层展示 / 落盘。实现方可以把它接到 tracing、会话事件或测试用的内存 sink。

use std::time::Duration;

use crate::event::{HookEvent, HookSource};

/// handler 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookHandlerKind {
    /// 本地命令。
    Command,
    /// HTTP 请求。
    Http,
}

impl HookHandlerKind {
    /// 审计/诊断中的名字。
    pub fn as_str(self) -> &'static str {
        match self {
            HookHandlerKind::Command => "command",
            HookHandlerKind::Http => "http",
        }
    }
}

/// 单次 hook 的最终结果（审计用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookAuditOutcome {
    /// 成功（退出码 0 / HTTP 2xx）。
    Success,
    /// 退出码 2 的阻塞。
    Blocked,
    /// 非零退出、超时、网络错误、非法输出等非阻塞问题。
    Warning,
    /// 超时被杀。
    Timeout,
    /// spawn 失败 / 连接失败。
    SpawnError,
    /// 输出无法解析。
    InvalidOutput,
    /// async hook 已启动（完成记录使用实际的 Success/Blocked/... 结果）。
    AsyncStarted,
}

/// 一条审计记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookAuditRecord {
    /// 事件。
    pub event: HookEvent,
    /// 来源。
    pub source: HookSource,
    /// handler 类型。
    pub handler: HookHandlerKind,
    /// 可读摘要（command 文本或 URL）。
    pub summary: String,
    /// 结果。
    pub outcome: HookAuditOutcome,
    /// 退出码（command）。
    pub exit_code: Option<i32>,
    /// HTTP 状态码。
    pub http_status: Option<u16>,
    /// 执行耗时。
    pub duration: Duration,
    /// 是否 async hook 的记录。
    pub asynchronous: bool,
    /// 该次运行产生的警告文本（exit 1、超时、非法 JSON、SSRF/白名单拒绝等）。
    ///
    /// 与 `HookOutcome::warnings` 同源：审计落盘后仍能回答“hook 为什么没生效”。
    pub warnings: Vec<String>,
}

/// 审计 sink。
///
/// 实现必须是非阻塞的（会在 hook 执行路径上同步调用）；需要做慢 IO 时请自行
/// 转发到 channel / 后台任务。
pub trait HookAuditSink: Send + Sync {
    /// 记录一条审计。
    fn record(&self, record: HookAuditRecord);
}

/// 默认 sink：丢弃所有记录。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopAuditSink;

impl HookAuditSink for NoopAuditSink {
    fn record(&self, _record: HookAuditRecord) {}
}
