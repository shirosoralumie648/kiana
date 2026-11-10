//! core/index.ts 的会话层再导出面。
//!
//! TS: `packages/coding-agent/src/core/index.ts`。本文件只包含 `session-assembly` 单元负责的
//! 前三条导出（`spec/ca-session.md` §2.13）：
//!
//! - `AgentSession` 及其事件/统计类型；
//! - `AgentSessionRuntime` / 工厂 / 结果 / `createAgentSessionRuntime`；
//! - 服务类型与 `createAgentSessionServices` / `createAgentSessionFromServices`。
//!
//! `core/index.ts` 的其余条目由其它 crate 承担（`ARCHITECTURE.md` §1/§3.5）：
//! bash-executor / cache-warmer / compaction / event-bus / experimental / extensions /
//! source-info / tool factories 分别在自己的 crate 根再导出。`ExecOptions`/`ExecResult`
//! 的契约对在 `kiana_ext_api::tool_result`（crate 根可见），本 crate 不重复定义。
//!
//! Rust 的函数名是 snake_case；同时保留与 `core/index.ts` 同名的 camelCase 别名
//! （`createAgentSessionRuntime` 等），便于对照 TS 与 downstream 的门面测试。
//!
//! 这组名字有测试锁定：`tests/crate_surface.rs` 逐条引用下面的导出（含 camelCase 别名），
//! 漏掉一个符号该测试就编译失败——修改再导出清单时同步改那个测试。

// TS: agent-session.ts#AgentSession
pub use crate::agent_session::{
    AgentSession, AgentSessionConfig, AgentSessionEvent, AgentSessionEventListener,
    ModelCycleResult, PromptOptions, SessionStats,
};

// TS: agent-session-runtime.ts#AgentSessionRuntime
pub use crate::agent_session_runtime::{
    AgentSessionRuntime, CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeResult,
    create_agent_session_runtime,
};

// TS: agent-session-services.ts#AgentSessionRuntimeDiagnostic
pub use crate::agent_session_services::{
    AgentSessionRuntimeDiagnostic, AgentSessionServices, CreateAgentSessionFromServicesOptions,
    CreateAgentSessionServicesOptions, create_agent_session_from_services,
    create_agent_session_services,
};

// `core/index.ts` 的函数名（camelCase）。
pub use crate::agent_session_runtime::create_agent_session_runtime as createAgentSessionRuntime;
pub use crate::agent_session_services::create_agent_session_from_services as createAgentSessionFromServices;
pub use crate::agent_session_services::create_agent_session_services as createAgentSessionServices;
