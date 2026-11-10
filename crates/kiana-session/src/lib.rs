//! 对应 ca-session：AgentSession、AgentSessionRuntime/Services、SDK 工厂。
//!
//! TS 源：packages/coding-agent/src/core/{agent-session,agent-session-runtime,agent-session-services,sdk}.ts
//!
//! # 这个 crate 解决什么问题
//!
//! 上游 crate 提供的是"零件"：[`kiana_agent_core::agent::Agent`] 驱动模型与工具调用，
//! `SessionManager`（kiana-session-core）负责落盘，扩展/资源加载器在更底层。本 crate
//! 把这些零件组装成一个可以直接对话的 [`AgentSession`]：
//! 安装会话钩子、维护 prompt/steering 队列、在 turn 边界触发压缩与自动重试、持久化
//! 消息，并在创建/切换/fork/导入会话时串起扩展事件与诊断（对应 TS 的 agent-session*.ts）。
//!
//! # 在系统中的位置
//!
//! 分层位于 L11（`docs/ARCHITECTURE_OVERVIEW.md` §2）：上游是 kiana-builtin-extensions /
//! kiana-resources（L10）及更底层的零件 crate；下游消费者是 kiana-interactive 与
//! kiana-rpc-print（两种前端，L12）和 kiana-coding-agent（CLI 启动编排，L13）。
//! 注意与 kiana-session-core 的分工：本 crate 是业务门面，落盘格式由
//! kiana-session-core 负责（`docs/ARCHITECTURE_OVERVIEW.md` §4.3）。
//!
//! # 核心概念与类型
//!
//! - [`AgentSession`]：业务门面；`Arc<Core>` +
//!   资源加载器的组合，在 `Agent` 的钩子上安装会话层逻辑。
//! - [`AgentSessionRuntime`]：
//!   会话生命周期（new/switch/fork/import/dispose），可变状态在 `Mutex` 后面。
//! - `AgentSessionServices`（agent_session_services）：cwd 绑定的运行时服务
//!   （ModelRuntime / SettingsManager / ResourceLoader）。
//! - `create_agent_session`（sdk）：一步到位的 SDK 工厂。
//!
//! 本文件只声明模块树；实现阶段按模块注释里的 TS 路径逐文件移植。
//! 对外符号统一经 [`index`] 再导出（对应 `core/index.ts` 的会话层三条导出），
//! 函数保留与 TS 同名的 camelCase 别名。
//!
//! # 一次典型调用
//!
//! 1. 下游调 `create_agent_session`（sdk；其入口第一条语句用 `Once` 安装默认流式函数，
//!    TS 模块加载副作用的 Rust 等价物）或 `create_agent_session_runtime`（runtime 入口
//!    本身不安装；其调用方提供的工厂最终经 sdk 的 `create_agent_session` 触发同一安装）；
//! 2. 工厂创建 `AgentSessionServices`（绑定 cwd/设置/模型运行时），加载扩展并收集诊断；
//! 3. 拿到 `AgentSession` 后调 `prompt`，会话内部走压缩检查 → 钩子派发 → agent run
//!    循环 → 边界事件的流程（详见 agent_session 模块文档）；
//! 4. 持久化/压缩失败不直接 panic，记入内部错误并在 run 结束点返回。
//!
//! # 边界与失败模式
//!
//! - 会话层错误集中在 `AgentSessionError` / `AgentSessionRuntimeError` /
//!   `CreateAgentSessionError`（各自模块内）；`kiana_ai::AiError` 是冻结的错误类型，
//!   多个入口的公开签名固定为 `Result<_, AiError>`。
//! - 扩展/资源加载在单线程 `LocalSet` 上运行，相关 future 不要求 `Send`。
//! - 公开事件枚举的字段顺序被 RPC 契约锁定（见 agent_session 模块注意事项）。
//!
//! # 阅读顺序
//!
//! [`index`]（再导出面）→ agent_session（门面与 prompt 主流程）→
//! agent_session_runtime（生命周期）→ agent_session_services（服务组装）→
//! sdk（SDK 工厂）。规范见 `spec/ca-session.md`。
pub mod acceptance;
pub mod agent_definition;
pub mod agent_session;
pub mod agent_session_runtime;
pub mod agent_session_services;
pub mod background;
pub mod checkpoint;
pub mod context_shadow;
pub mod continuity;
pub(crate) mod durable_store;
pub mod execution;
pub mod index;
pub mod memory_wiring;
pub mod panel_commands;
pub mod sdk;
pub mod subagent;

// crate 根再导出面（`core/index.ts` 的会话层符号）。
pub use index::*;
