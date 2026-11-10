//! `context/index.ts`：`Context` trait、`ContextKey`、`EmptyContext`、`Background/Todo` 组合子、
//! `trace_in`、`abort_error`（feature `context`）。
//!
//! `Context` 是不可变的、调用作用域内的值，被显式地穿过操作传递（与 `types::Ctx` 不同：那个是 Chord
//! 的 root 调用上下文）。两个"ctx"在 TS 里也不同名：`Context`（`context/index.ts`）与 `Ctx`
//! （`types.ts`）。
//!
//! ## 与 TS 的对应
//!
//! - `Context.abortSignal`（`AbortSignal | undefined`）→ [`Context::abort_signal`]
//!   （本模块的 [`AbortSignal`]：`CancellationToken` + 可选原因，按 ARCHITECTURE §4.2）。
//! - `Context.telemetry`（`TelemetrySpan`）→ [`Context::telemetry`]（`Arc<dyn TelemetrySpan>`）；
//!   缺省是 `NOOP_TELEMETRY_CONTEXT` 这个进程级单例（`Arc::ptr_eq` 成立，对应 TS 的
//!   `ctx.telemetry === NOOP_TELEMETRY_CONTEXT`）。
//! - `Context.value(key)` → [`Context::value`]。Rust 没有泛型方法返回任意 `T` 的对象安全形式，因此
//!   值以类型擦除的 [`ContextValue`] 承载（`Arc<dyn Any + Send + Sync>`，`ContextKey` 带类型标记）。
//! - `toString()` → `Display`。
//!
//! # 模块结构：一个文件实现，一个文件再导出
//!
//! 本模块分成两层：
//!
//! - `index`（`index.rs`）是实现体，对应 TS 的 `context/index.ts`；
//! - 本文件（`mod.rs`）只声明子模块并 `pub use` 它的全部公开项，不含任何逻辑。
//!
//! 因此 `kiana_chord::context::X` 与 `kiana_chord::context::index::X` 是**同一个项**（再导出，
//! 不是包装，也不是副本）。这样保留了与 TS 文件路径的一一对应；同时调用方可以省略 `index::`
//! 这一段。新增公开项时必须同步下面的 `pub use` 清单，否则该项只能从 `context::index`
//! 访问到。
//!
//! 调用方：crate 的顶层再导出（`crate::index`）从这里取 `Context` 与 `ContextKey`；集成测试
//! `crates/kiana-chord/tests/context.rs` 以及 `kiana-durable` 等下游 crate 都通过
//! `kiana_chord::context::…` 使用本模块。
//!
//! # 核心模型：不可变的分层上下文
//!
//! `Context` 是只读的、调用作用域内的值，通过 `ContextRef`（`Arc<dyn Context>`）共享。派生
//! （[`with_context_value`]）不会修改父上下文，而是返回一层新的 `Context`：内部持有
//! `parent: ContextRef` 与本层写入的键值。查找 `value` 时先比对本层的键，不匹配再委托父层，
//! 从而形成一条从新到旧的责任链。直接推论：
//!
//! - 已有上下文永远不被改写，从一个父上下文派生出的兄弟上下文互不影响；
//! - [`BACKGROUND_CONTEXT`] 与 [`TODO_CONTEXT`] 是两条空链的根（`EmptyContext`），是**两个**
//!   不同实例；二者都没有取消信号，telemetry 都是进程级 NOOP 单例；
//! - 取消信号与 telemetry 也复用同一套分层机制：它们存放在两个内部静态键（描述分别为
//!   `chord.abortSignal`、`chord.telemetry`）上，所以 [`with_abort_signal`]、[`with_telemetry`]
//!   都是 `with_context_value` 的特化，而 `Context::abort_signal` / `Context::telemetry` 是从
//!   链上取值的 getter。
//!
//! 键的身份只由 [`KeyIdentity`] 里 `Arc` 的指针相等决定（对应 TS 的 `Symbol`）：
//! [`create_context_key`] 每次调用都生成新 token，所以描述相同的两个键互不相通；`clone()`
//! 以及保留 token 的 `ContextKey::erase()`（内部同样用 `Arc::clone` 复制 token）都共享同一个
//! token，因此克隆或擦除出来的键仍是同一个键（`same_token` 为 true）；`with_context_value`
//! 写入本层时存的就是 `key.erase()` 的结果。值以 `Arc<dyn Any + Send + Sync>` 擦除存放，
//! `ContextKey::get` 用泛型参数 `T` 做 downcast——键不存在、或存的类型与 `T` 不符，都会得到
//! `None`。
//!
//! # 失败与边界
//!
//! - **取消只生效一次**：`AbortSignal::abort` 以首个原因为准，其后的调用是空操作；监听器在
//!   `abort` 时同步执行。要绕开父上下文的取消，用 [`without_abort_signal`]：它把内部取消键
//!   **遮蔽**为 `None`（而非删除），只影响这一层派生出的子上下文，父上下文与兄弟上下文不变。
//! - **两种等待的语义不同**：`await_with_context` 在取消时立即返回 [`AbortError`]，被等待的
//!   future 随函数返回而 drop（Rust 的常规语义，等于放弃它）；`await_handle_with_context`
//!   等待的是 `tokio::task::JoinHandle`，取消只放弃等待，任务本身继续运行（与 TS 的 Promise
//!   一致）。
//! - **telemetry 没有“关闭”分支**：缺省是进程级 NOOP 单例，[`trace_in`] / [`trace_in_try`]
//!   在 NOOP 下仍然走 `startSpan`；[`trace_in_try`] 把回调返回的 `Err` 当作 TS 的 `throw`，
//!   span 记为 error（除非回调已显式设置 span 状态），错误原样返回。
//! - **feature 门控**：`Cargo.toml` 声明了 `context` feature（默认启用），但本 crate 源码里没有
//!   任何 `#[cfg(feature = "context")]`，所以本模块始终参与编译，不受该 feature 开关影响。
//!
//! # 阅读顺序
//!
//! 1. [`Context`] / [`ContextRef`]：对象安全的 trait 与共享句柄；
//! 2. [`ContextKey`] / [`ContextValue`] / [`KeyIdentity`]：类型化键与类型擦除的值；
//! 3. [`AbortSignal`] / [`AbortReason`] / [`AbortError`]：取消模型；
//! 4. 组合子：`with_context_value`、`with_abort_signal`、`without_abort_signal`、`with_cancel`、
//!    `with_telemetry`；
//! 5. 等待与追踪：`await_with_context`、`await_handle_with_context`、`trace_in`、`trace_in_try`。
//!
//! 行为级验证见 `crates/kiana-chord/tests/context.rs`（TS `context.test.ts` 的端口，配
//! `fixtures/context_golden.json` golden 数据）。

pub mod index;

pub use index::{
    AbortError, AbortReason, AbortSignal, AbortSubscription, BACKGROUND_CONTEXT, CancelHandle,
    Context, ContextKey, ContextRef, ContextValue, KeyIdentity, TODO_CONTEXT, abort_error,
    abort_signal_of, await_handle_with_context, await_with_context, context_string,
    create_context_key, create_context_key_anonymous, telemetry_of, trace_in, trace_in_try,
    with_abort_signal, with_cancel, with_context_value, with_telemetry, without_abort_signal,
};
