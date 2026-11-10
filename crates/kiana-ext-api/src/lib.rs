//! 对应 core/extensions/types.ts 的公开契约。
//! TS 源：packages/coding-agent/src/core/extensions/types.ts
//!
//! 本 crate 是扩展公开 API 的**唯一权威定义位置**（`ARCHITECTURE.md` §3.4/§3.5/§4.9）：
//! `kiana-tools`（L8）、`kiana-extensions`（L9）、`kiana-resources`（L10）、`kiana-session`（L11）、
//! `kiana-interactive`（L12）、`kiana-rpc-print`、`kiana-coding-agent` 都从这里 import，不得定义第二份。
//!
//! 本 crate **只做声明**，外加三个纯函数面：
//! [`tool_types::define_tool`]、8 个 `is_*_tool_result` 守卫与
//! [`events::is_tool_call_event_type`]。Loader/runner/UI 适配器行为、no-op UI、
//! 事件派发与错误文案都在 `kiana-extensions`。
//!
//! # 两个 future 别名（`ARCHITECTURE.md` §4.2）
//!
//! - [`BoxFuture`]（`Send`）：跨进 `kiana-tools`/`kiana-session` 的回调使用
//!   （`kiana-agent-core` 的 `AgentToolExecute` 要求 `Send`）。
//! - [`LocalBoxFuture`]（非 `Send`）：UI 方法、`ExtensionApi` 的异步方法、
//!   事件 handler 与 command handler 使用；这些实现跑在 `kiana-extensions`/`kiana-interactive`
//!   的单线程 `LocalSet` 上（rquickjs 的 `Persistent` 是 `!Send`）。
//!
//! 注意：`ExtensionUiContext`（以及所有返回 [`Box<dyn Component>`](kiana_tui::Component) 的
//! UI 回调）的返回值里有 `!Send` 的 TUI 组件，因此只能使用 [`LocalBoxFuture`]。
//!
//! # 命名
//!
//! TS 的 `ExtensionUIContext` 在 Rust 里是 trait [`ui::ExtensionUiContext`]（`ARCHITECTURE.md`
//! §5.2 的冻结名）。所有 TS 接口的字段名经 `#[serde(rename_all = "camelCase")]` 保留线上 JSON 名。
//!
//! # 为什么需要这个 crate
//!
//! `types.ts` 被工具实现、扩展运行时、资源加载、会话与 UI 同时引用，而这些实现在 Rust 里
//! 分属 L8–L13 的不同 crate。若类型跟着实现走，任一 crate 想使用另一层的类型就得反向依赖，
//! 立刻成环（`ARCHITECTURE.md` §3.4 把它描述为 22 文件类型 SCC）。把声明集中到 L7 的独立
//! crate、让所有实现层统一**向下**依赖，是打破该 SCC 的方式。配套的不变式见 §3.5：同一个符号
//! 在整个 workspace 只能有一个 `struct`/`enum`/`trait` 定义，下游需要时只能 `pub use`。
//!
//! # 在系统中的位置
//!
//! 直接依赖本 crate 的 crate（逐个核对 `crates/*/Cargo.toml`）：`kiana-export`、`kiana-tools`、
//! `kiana-extensions`、`kiana-builtin-extensions`、`kiana-resources`、`kiana-session`、
//! `kiana-interactive`、`kiana-rpc-print`、`kiana-coding-agent`。
//!
//! 本 crate 的上游只到 L6：`kiana-ai`（模型/消息/usage）、`kiana-agent-core`（`AgentTool`、
//! `ToolExecutionMode`）、`kiana-session-core`（会话条目）、`kiana-config`（settings/registry）、
//! `kiana-tui`（`Component`）等；本 crate 在它们之上组合自己的契约类型，不复述上游定义
//! （见 `Cargo.toml`）。
//!
//! # 核心类型
//!
//! - [`crate::types::ExtensionApi`]：扩展作者的工具箱（工具/命令/消息/模型/provider 注册）。
//! - 上下文 trait：[`crate::types::ExtensionContextApi`]、[`crate::types::ExtensionToolContextApi`]、
//!   [`crate::types::ExtensionCommandContextApi`]、[`crate::types::ReplacedSessionContextApi`]；
//!   UI 侧为 [`crate::ui::ExtensionUiContext`]。它们都是对象安全 trait（见 [`crate::types`] 的
//!   模块文档），运行时以 `dyn` 形式传递（例如 `Arc<dyn ExtensionContextApi>`）。
//! - [`crate::events::ExtensionEvent`]（32 个顶层变体，`Session` 展开后对应 41 个事件名）
//!   与返回值联合：五个联合的 `Serialize`/`Deserialize` 由 [`crate::events`] 手写，少数需要
//!   特殊处理的叶子结构体（`McpServersChangeEvent`、`SessionBeforeCompactEvent`、
//!   `SessionBeforeTreeEvent`、`BoundaryContextPreview`）也手写 serde，其余叶子结构体用
//!   derive。这份手写名单不含返回值联合 [`crate::events::ExtensionEventResult`]——它不实现
//!   serde，由 `kiana-extensions` 在派发时按事件类型解析（见下）。
//! - [`crate::tool_types::ToolDefinition`]：工具的名字、schema、`execute` 与渲染回调；
//!   bash/exec 的跨 crate 契约在 [`crate::tool_result`]。
//!
//! # 一次典型调用（事件路径）
//!
//! 事件由 `kiana-session`（`agent_session.rs`）构造后经 `kiana-extensions` 的
//! `ExtensionRunner` 发出，入口不止一个（返回的结果类型也不同）：
//!
//! 1. `emit`（泛型入口）：生命周期与 `session_before_*` 等事件（`agent_start`、
//!    `message_update`、`tool_execution_*`…）。返回
//!    `Result<Option<ExtensionEventResult>, DispatchError>`，但只有 `session_before_*`
//!    才可能有结果，其余事件返回 `None`。
//! 2. `emit_tool_call` / `emit_tool_result`：`tool_call` 与 `tool_result` 事件。分别返回
//!    `ToolCallEventResult`（`agent_session.rs` 用它决定是否拦截工具调用）与
//!    `ToolResultEventResult`（合并进工具结果的补丁）。
//! 3. `emit_boundary`：`turn_end` / `agent_before_settle` 边界事件，返回
//!    `BoundaryDispatchResult`（三方裁决后的草稿条目与是否继续）。
//!
//! 无论走哪个入口，`kiana-extensions` 的 `runner/dispatch.rs` 都用 `serde_json::to_value`
//! 把事件转成 JS 可见对象；对 `tool_call`/`tool_result`/`turn_end`/
//! `agent_before_settle`/`before_agent_start`/`provider_stream_event` 还按 TS 对象字面量的
//! 构造顺序重排键（`ts_event_json`）。
//!
//! handler 的返回值按事件类型经 `serde_json::from_value` 解析为对应的结果结构体；泛型
//! `emit` 的 `session_before_*` 结果再包装为 [`crate::events::ExtensionEventResult`]（该
//! 枚举本身不实现 serde），调用方据此决定继续或改写本次操作。
//!
//! 工具注册路径更短：扩展调用 [`crate::tool_types::define_tool`]（恒等函数，只为类型推断）
//! 得到 [`crate::tool_types::ToolDefinition`]；内置工具在 `kiana-tools` 里构造同样的结构体，
//! `kiana-extensions` 的 loader 负责注册扩展提供的工具。
//!
//! # 边界与失败模式
//!
//! - 源码中没有 IO、任务/线程创建或全局可变状态，也没有 `unsafe` 与主动 panic 的
//!   `unwrap()`/`expect()`/`panic!`；公开面是类型、对象安全 trait 与纯函数。错误源只有两处：
//!   serde 序列化/反序列化（调用方拿到 `serde_json::Error`），以及 [`crate::events::ExtensionEventKind`]
//!   的字符串解析（`FromStr` 返回 `ParseExtensionEventKindError`）。
//! - 线上 JSON 是冻结契约：字段名由 `camelCase` 重命名与逐字段 `rename` 决定；`f64` 字段必须经
//!   私有 `js_number` 辅助按 ECMAScript `Number::toString` 写出（`NaN`/`±Inf` → `null`，
//!   `-0` → `0`），不能改用 serde 默认数字格式（`ARCHITECTURE.md` §4.3）。
//! - 结构体字段顺序是 TS 声明顺序，但不总是 JS 可见键序：`kiana-extensions` 的 `ts_event_json`
//!   会对 `tool_call`/`tool_result`/`turn_end`/`agent_before_settle`/`before_agent_start`/
//!   `provider_stream_event` 这几类事件重排；其余事件与结构体按字段声明顺序序列化。
//! - 两个 future 别名不能互换：需要 `Send` 的边界（[`crate::tool_types::ToolDefinitionExecute`]
//!   与 `kiana-agent-core` 的 `AgentToolExecute`）只能用 [`BoxFuture`]；返回值含 `!Send` TUI
//!   组件的 UI 方法只能用 [`LocalBoxFuture`]。选错是编译期错误，不会在运行期暴露。
//! - `ARCHITECTURE.md` §4.9 把 `types.rs`、`tool_types.rs`、`tool_result.rs`、`events.rs`、
//!   `system_prompt.rs`、`ui.rs` 列为冻结契约，公开签名只有架构者能改；改动会波及上面全部
//!   下游 crate，并可能改变扩展作者看到的 JSON。
//!
//! # 阅读顺序
//!
//! 1. [`crate::types`]：`ExtensionApi` 与四个 context trait，先弄清扩展能做什么。
//! 2. [`crate::events`]：事件与返回值联合，理解会话如何驱动扩展。
//! 3. [`crate::tool_types`]、[`crate::tool_result`]：工具声明与 bash/exec 契约。
//! 4. [`crate::ui`]：UI 上下文与对话框/widget 选项。
//! 5. [`crate::system_prompt`]：system prompt 选项（实现留在 `kiana-resources`）。
//! 6. [`crate::index`]：对照 TS 的 186 个导出名与根路径可见性。
//!
//! 契约由 `crates/kiana-ext-api/tests/` 锁定：`public_surface.rs` 检查根路径可达性与对象安全，
//! `serde_contracts.rs` 以字符串比较钉住键序和 `undefined`/`null` 语义，`event_guards.rs` 钉住
//! 守卫与 41 个事件名，`js_number_contracts.rs` 钉住 JS 数字文本。

// 回调类型是一一对应 TS 签名的直接表达；拆成中间别名只会分散契约定义。局部例外理由见
// ARCHITECTURE.md §4.10（`clippy::type_complexity` 于回调别名）。
#![allow(clippy::type_complexity)]

use std::future::Future;
use std::pin::Pin;

// --- 子模块 ---
pub mod events;
pub mod index;
pub mod system_prompt;
pub mod tool_result;
pub mod tool_types;
pub mod types;
pub mod ui;

/// `f64` 字段的 serde 辅助（ARCHITECTURE.md §4.3 的 JS 数字格式）。
mod js_number;

/// `Send` 的 boxed future：跨进 `kiana-tools`/`kiana-session` 的回调
/// （`kiana-agent-core::AgentToolExecute` 要求 `Send`）。
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 非 `Send` 的 boxed future：UI 方法、`ExtensionApi` 的异步方法与事件/命令 handler，
/// 这些实现跑在单线程 `LocalSet` 上。
pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

// crate 根再导出 TS 的 186 个公开名字（`index.rs` 是纯 barrel）。
pub use index::*;
