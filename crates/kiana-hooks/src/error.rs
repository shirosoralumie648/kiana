//! 本 crate 的 typed error。

/// 调用方错误。
///
/// 单个 hook 的失败（spawn 失败、非零退出、超时、非法 JSON、HTTP 非 2xx）不会变成
/// `HookError`，而是进入 [`crate::HookOutcome::warnings`] 与审计；这里的错误表示
/// “这次调用本身不合法”，调用方需要修正输入或配置。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HookError {
    /// 递归深度超过 `HookRunnerConfig::max_depth`。
    #[error("hook recursion depth {depth} exceeds limit {limit}")]
    RecursionLimit {
        /// 当前深度。
        depth: u32,
        /// 允许的最大深度。
        limit: u32,
    },

    /// 事件输入缺少必要字段（例如工具事件没有 `tool_name`）。
    #[error("invalid hook input: {0}")]
    InvalidInput(String),

    /// runner 配置非法（目前只有 HTTP URL 白名单正则编译失败）。
    #[error("invalid hook configuration: {0}")]
    Config(String),
}
