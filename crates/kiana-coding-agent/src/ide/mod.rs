//! 编辑器集成：lockfile 发现、MCP Streamable HTTP 与 IDE 上下文/diff 协议。
//!
//! 设计参照：
//! - `reference_project/ClaudeCode/src/utils/ide.ts` —— `~/.claude/ide/<port>.lock` 的发现、
//!   `workspaceFolders`/`pid`/`authToken`/`transport` 字段、dead pid 与 workspace 不符的
//!   清理、`isProcessRunning` 检查；
//! - `reference_project/ClaudeCode/src/hooks/useDiffInIDE.ts` —— `openDiff` 的
//!   accepted/rejected 往返；
//! - `reference_project/cline` / `reference_project/opencode` 的 MCP Streamable HTTP
//!   客户端（JSON-RPC over POST、SSE 响应、bearer 鉴权）。
//!
//! 本模块只实现协议与状态机，不启动真实的 IDE 插件。生产接入点：
//! [`lockfile::discover_ides`] → [`protocol::HttpIdeTransport`] →
//! [`protocol::send_context_update`] / [`protocol::DiffBroker::open_diff`]。
//!
//! # 安全边界
//!
//! - IDE 监听在 loopback，lockfile 里的 `ppid`/`pid` 必须存活且 workspace 匹配才会连接；
//! - `authToken` 只放进 `Authorization: Bearer` 头，不写日志、不进错误文本；
//! - lockfile 的清理只删除 `.lock` 文件本身，不跟随符号链接。

pub mod lockfile;
pub mod protocol;
pub mod startup;

pub use lockfile::{
    DiscoveredIde, IdeLockfile, cleanup_stale_locks, discover_ides, process_is_alive,
    workspace_matches,
};
pub use protocol::{
    ContextUpdatePayload, DiffBroker, DiffOutcome, DiffRequest, HttpIdeTransport, IdeContext,
    IdeError, IdeTransport, MAX_CONTEXT_BYTES, MAX_CONTEXT_FILES, McpIdeClient, OpenFile, Position,
    Selection, build_context_update, send_context_update,
};
pub use startup::{
    IdeConnection, agent_ide_dir, connect_for_workspace, global_connection, global_diff_broker,
    install_connection, route_ide_notification,
};
