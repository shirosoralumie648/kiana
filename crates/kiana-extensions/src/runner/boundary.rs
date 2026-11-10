//! `emit_boundary`（最复杂的派发）：`agent_before_settle` 与 `end > continue > undefined` 三方裁决。
//!
//! TS: `core/extensions/runner.ts#emitBoundary`（`runner.ts:1036–1090`）。
//!
//! 合并语义全部在 `js/dispatch.js` 的 `emitBoundary` 里（共享 `entries`/`continue`/`context`；
//! 每个 handler 之后重新 `buildContext(entries)`；抛错 → `valid = false` +
//! `Invalid boundary entries: <msg>`【逐字】；最终 `valid ? {...} : {entries: [], continue:
//! false, context, valid: false}`）。本模块只是 Rust 入口。
//!
//! 构建上下文回调是调用方（`kiana-session`）提供的异步函数：
//! [`BuildBoundaryContext`]。
//! 共享修改（`event.entries.push`）在 handler 抛错之后仍然保留——JS 侧的事件对象就是
//! 共享草稿数组，`catch` 不会回滚它。若 `buildContext` 因无效草稿抛错，后续 handler 仍会
//! 看到被拒绝的草稿并可以修复它（`valid` 在下一次成功构建时回到 `true`）。
//!
//! # 在系统中的位置与调用链
//!
//! 会话层（`kiana-session`）在收到 `turn_end` / `agent_before_settle` 时调用
//! [`ExtensionRunner::emit_boundary`](super::ExtensionRunner::emit_boundary)，后者转发到本
//! 模块的 [`emit_boundary`]，真正的派发由 `runner::dispatch` 的
//! `run_boundary_dispatch` 完成：
//!
//! 1. 按事件类型取 handler 快照（快照在本次调用内固定，之后新注册的 handler 不参与）；
//! 2. 组装初始 state `{ base, entries: [], context: null }` 交给 `js/dispatch.js` 的
//!    `emitBoundary`，其中 `base` 是去掉三个共享字段后的事件 JSON；
//! 3. 把 JS 返回的对象反序列化成 [`BoundaryDispatchResult`]。
//!
//! # 合并规则（以 `js/dispatch.js` 的 `emitBoundary` 为准）
//!
//! - handler 循环开始前先对空草稿调用一次 `buildContext`。这次失败会让整个 Promise
//!   reject，调用方拿到 `Err(DispatchError)`，而不是一个 `valid = false` 的结果。
//! - 每个 handler 看到同一个 `{...base, entries, continue, context}` 快照。handler 返回的
//!   `entries` 不是 `undefined` 时**整体替换**共享草稿（返回空数组也算显式替换）；没有
//!   返回时，如果 JS 事件对象的 `entries` 数组内容与当前草稿不同（handler 原地
//!   `push`），则采用原地修改后的数组。`continue` 只在显式非 `undefined` 时更新。
//! - 每个 handler 之后都重新构建 context。构建失败时记 `valid = false` 并上报
//!   `Invalid boundary entries: <msg>`，循环继续，后续 handler 可以修复草稿。
//! - 最终 `valid == false` 时丢弃全部草稿：`entries = []`、`continue = false`；`context`
//!   保留最后一次成功构建的预览。
//!
//! # 失败模式
//!
//! - 当前线程没有安装引擎：`DispatchError::EngineNotInstalled`。
//! - JS 侧 Promise 拒绝（初始 `buildContext` 失败、emitter/宿主桥自身抛错）：
//!   `DispatchError::Js`；返回值无法反序列化：`DispatchError::InvalidResult`。
//! - 单个 handler 抛错不会中止派发：JS 侧捕获后交给错误上报器，草稿保持抛错前的状态。

use super::dispatch::DispatchError;
use super::{BoundaryBaseEvent, BoundaryDispatchResult, BuildBoundaryContext, ExtensionRunner};

/// `emitBoundary`：`turn_end` / `agent_before_settle` 的两阶段裁决。
///
/// TS: `runner.ts#emitBoundary`。
///
/// 参数：
/// - `base_event`：不含 `entries`/`continue`/`context` 的基事件；三个共享字段由 JS 侧初始化
///   并在 handler 之间传递（[`BoundaryBaseEvent::event_type`](super::BoundaryBaseEvent::event_type)
///   决定取哪一类 handler 快照）。
/// - `build_context`：把当前草稿校验为上下文预览的异步回调，由调用方注入；它在循环前和
///   每个 handler 之后被反复调用，失败表示草稿无效（错误文案见模块文档）。
///
/// 返回最终裁决。`Err` 表示派发基础设施失败或初始构建失败，此时没有部分结果可消费。
/// handler 抛错不会让本函数返回 `Err`（JS 侧捕获并上报）；草稿校验失败只体现在返回值的
/// `valid`/`entries`/`continue` 上。
pub async fn emit_boundary(
    runner: &ExtensionRunner,
    base_event: BoundaryBaseEvent,
    build_context: BuildBoundaryContext,
) -> Result<BoundaryDispatchResult, DispatchError> {
    super::dispatch::run_boundary_dispatch(runner, base_event, build_context).await
}
