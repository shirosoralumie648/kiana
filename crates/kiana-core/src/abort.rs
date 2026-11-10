//! utils/abort.ts。
//!
//! TS: `packages/coding-agent/src/utils/abort.ts`（48 行）。
//!
//! TS 的 `AbortSignal` → [`tokio_util::sync::CancellationToken`]（`ARCHITECTURE.md` §4.2）。
//!
//! ## 为什么 `race_with_abort_signal` 会 `tokio::spawn`
//!
//! TS 的 `raceWithAbortSignal` 在 abort 后**继续观察** operation 直到它 settle
//! （`void operation.catch(() => {})` 与 `.then` 里的 `settled` 保护）：放弃的是等待，
//! 不是 operation 本身。Rust 的 future 一旦被 drop 就会被取消，因此实现把 operation
//! 交给一个独立任务，abort 时只放弃等待（`JoinHandle` 被 drop 后任务继续运行到结束）。
//! 这与 TS 语义一致，代价是要求 `F: Send + 'static`、`T: Send + 'static`。
//!
//! 唯一与 TS 不同的边界：`signal` **已经**中止时，TS 的 operation 是一个已经启动、
//! 只靠 `catch` 吞掉 rejection 的 promise；而 Rust 的 future 尚未启动，直接丢弃
//! （不产生任何副作用）后立即返回 `AbortError`。
//!
//! ## 已知偏差
//!
//! - TS 的 `abortReason` 优先返回 `signal.reason`；`CancellationToken` 不携带 reason，
//!   因此这里总是返回默认的 `The operation was aborted`。
//! - TS 的 signal 监听是 `{ once: true }` 且可移除；`CancellationToken::cancelled()` 是
//!   广播，语义等价。
//!
//! ## 调用关系
//!
//! - 本 crate 内：`crate::sleep::sleep` 用 `AbortError::aborted()` 构造
//!   `message = "Aborted"` 的错误（对应 TS `sleep` 的 `new Error("Aborted")`）。
//! - 集成测试 `crates/kiana-core/tests/abort.rs` 覆盖 [`race_with_abort_signal`] 的
//!   无 signal / 已取消 / 取消后继续驱动 operation / operation 先完成四个分支。
//!
//! 新增调用方时注意：错误类型只有 `&'static str` 两个字段，想携带 `signal.reason`
//! 或调用方自定义文案需要另建错误类型，不能指望这里的 [`AbortError`] 透传。

use std::fmt;
use std::future::Future;

use tokio_util::sync::CancellationToken;

/// TS: `abortReason` 构造的错误（`name` 为 `AbortError`）。
///
/// `Display` 逐字等于 TS `error.message`：`The operation was aborted`。
/// [`crate::sleep::sleep`] 复用一个携带 `Aborted` 文案、`name` 为 `Error` 的实例以匹配
/// TS `sleep` 的 `new Error("Aborted")`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbortError {
    message: &'static str,
    name: &'static str,
}

impl AbortError {
    /// TS: `new Error("The operation was aborted")` + `error.name = "AbortError"`。
    pub fn new() -> Self {
        Self {
            message: "The operation was aborted",
            name: "AbortError",
        }
    }

    /// TS: `sleep` 的 `new Error("Aborted")`（`name` 保持 `Error`）。
    pub(crate) fn aborted() -> Self {
        Self {
            message: "Aborted",
            name: "Error",
        }
    }

    /// 与 TS `error.name` 对应的名字：`AbortError` 或 `Error`。
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// 错误消息文本是稳定契约（调用方与测试按此文本匹配），改动需同步测试。
    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl Default for AbortError {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AbortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for AbortError {}

/// TS: `operationSignal(signal?)` —— `signal ?? new AbortController().signal`。
///
/// `None` 返回一个全新的、不会被取消的 token（等价于新 controller 的 signal）。
pub fn operation_signal(signal: Option<CancellationToken>) -> CancellationToken {
    signal.unwrap_or_default()
}

/// TS: `raceWithAbortSignal(operation, signal)`。
///
/// 无 signal → 直接等待 operation；signal 已中止 → 丢弃 operation 并立即返回
/// [`AbortError`]；否则等待二者之一。abort 后 operation 仍会被驱动到结束（见模块文档）。
///
/// operation 对应的任务 panic 时，panic 会在调用方重新抛出（任务通过 `JoinHandle`
/// 等待，panic 不会被吞掉）。
///
/// 边界：`tokio::select!` 没有加 `biased;`，当 operation 完成与 signal 取消在同一轮调度中
/// 同时就绪时，两个分支谁先被选中不固定。需要确定性顺序的调用方不能依赖这一点。
pub async fn race_with_abort_signal<T, F>(
    operation: F,
    signal: Option<&CancellationToken>,
) -> Result<T, AbortError>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let Some(signal) = signal else {
        return Ok(operation.await);
    };

    if signal.is_cancelled() {
        // TS 里 operation 已经是一个运行中的 promise（只挂 catch 观察）；Rust 的 future
        // 还没启动，按单元契约直接丢弃。
        drop(operation);
        return Err(AbortError::new());
    }

    let mut handle = tokio::spawn(operation);
    tokio::select! {
        result = &mut handle => Ok(settle(result)),
        _ = signal.cancelled() => {
            // `handle` 在这里被 drop：任务被 detach，继续运行到 settle（TS 的观察语义）。
            Err(AbortError::new())
        }
    }
}

/// 把 `JoinHandle` 的结果还原成 operation 的输出。
///
/// panic 用 `resume_unwind` 原样抛回调用方（保留 payload，测试
/// `a_panicking_operation_propagates_the_panic` 断言调用方看到 `is_panic()`）；
/// 非 panic 的 `JoinError` 表示任务被取消；本函数从不 `abort` 该任务，所以按内部不变量
/// 这不应发生，真出现时以断言失败处理。
fn settle<T>(result: Result<T, tokio::task::JoinError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            if error.is_panic() {
                std::panic::resume_unwind(error.into_panic());
            }
            // 我们从不 abort 该任务，`JoinError` 只可能是 panic/cancel；cancel 不会发生。
            panic!("operation task ended unexpectedly: {error}");
        }
    }
}
