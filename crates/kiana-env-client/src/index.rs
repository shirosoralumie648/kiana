//! index.ts 再导出面。
//!
//! 按 TS 文件顺序导出 34 个符号（spec §2.1.1）。`Json`、`abortResult`、`toFileError`、
//! `listFilesOver`、`remotePath` 是 `pub` 但**不**在这里再导出（spec §5.2）。
//!
//! # 为什么需要这个文件
//! TS 的 `index.ts` 只做再导出，没有自己的类型；Rust 侧保留同名模块，是为了让“包的公开面”
//! 有一处可对照的清单：`lib.rs` 里的 `pub use index::*;` 把它提升到 crate 根，集成测试可以
//! 直接从 crate 根使用这 34 个名字（未再导出的项仍走各自的模块路径）。
//!
//! 分组与 TS 的源文件一一对应（connection → list-files → remote-env → ssh → terminal →
//! watch）；调整分组不会改变符号的模块路径或可见性。
//!
//! # 注意事项
//! - 这里只允许 `pub use`，不要在这里定义类型：index.ts 是纯再导出清单（spec §2.1.1），
//!   类型留在各自模块，公开面才能与 TS 逐项对照，也不会把实现细节混进这份清单。
//! - 被排除的五个符号只通过各自的模块路径提供（例如
//!   `kiana_env_client::list_files::list_files_over`）；不要顺手把它们加到本文件。
//! - 公开符号数量（34）是 spec 的硬性清单；增删或改可见性前先同步 spec 与调用方。
//!
//! TS: packages/env/src/index.ts

pub use crate::connection::{
    Connection, ConnectionOptions, RemoteError, RemoteInfo, Reply, RequestOptions,
    is_connection_lost, node_platform,
};
pub use crate::list_files::{LocalExecutionEnv, LocalExecutionEnvOptions};
pub use crate::remote_env::{RemoteExecutionEnv, RemoteExecutionEnvOptions};
pub use crate::ssh::{
    HostKeyChangedError, HostKeyUnknownError, RemotePlatform, SshConnectOptions, SshError,
    SshTarget, accept_host_key, connect_ssh, deploy_daemon, detect_platform, forget_host_key,
    local_daemon, packaged_daemon, scan_host_key, ssh_arguments, ssh_connection,
};
pub use crate::terminal::{RemoteTerminal, TerminalForeground, TerminalOptions, open_terminal};
pub use crate::watch::{RemoteWatchOptions, RemoteWatcher};
