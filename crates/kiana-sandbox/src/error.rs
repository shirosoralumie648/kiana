//! 沙箱错误。

use std::path::PathBuf;

/// 构造或启动沙箱失败。
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// 策略需要沙箱，但当前平台没有可用后端。
    #[error("no sandbox backend is available for {policy}: {reason}")]
    Unavailable {
        /// 策略名称（`read-only` / `workspace-write`）。
        policy: &'static str,
        /// 具体原因。
        reason: String,
    },
    /// 指定的后端在当前平台不存在（例如 macOS seatbelt 尚未实现）。
    #[error("sandbox backend {0} is not supported on this platform")]
    Unsupported(&'static str),
    /// bubblewrap 的参数只能是 UTF-8；非 UTF-8 路径无法安全传递。
    #[error("non-UTF-8 path cannot be passed to bubblewrap: {0:?}")]
    NonUtf8Path(PathBuf),
    /// Landlock ruleset 构造失败。
    #[error("failed to prepare landlock ruleset: {0}")]
    Landlock(#[source] std::io::Error),
    /// relay 状态目录/脚本准备失败；命令没有启动。
    #[error("failed to prepare the sandbox relay: {0}")]
    Relay(String),
    /// 空程序名。
    #[error("refusing to spawn an empty command")]
    EmptyCommand,
    /// `spawn` 失败。
    #[error("failed to spawn sandboxed command: {0}")]
    Spawn(#[source] std::io::Error),
}
