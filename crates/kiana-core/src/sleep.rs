//! utils/sleep.ts。
//!
//! TS: `packages/coding-agent/src/utils/sleep.ts`（18 行）。
//!
//! ```ts
//! export function sleep(ms: number, signal?: AbortSignal): Promise<void>
//! ```
//!
//! - `signal.aborted` → `reject(new Error("Aborted"))`；
//! - 否则 `setTimeout(resolve, ms)`，abort 时清掉 timer 并 `reject(new Error("Aborted"))`。
//!
//! Rust 用同名文案的错误：[`AbortError`] 携带 `message = "Aborted"`（`name` 保持 TS 的
//! `Error`；`ARCHITECTURE.md` §4.1 的「`Display` == `error.message`」）。
//!
//! 偏差：`setTimeout(0)` 在 Node 里至少延迟约 1ms，`tokio::time::sleep(Duration::ZERO)`
//! 只让出一次调度；调用方（重试退避）不依赖这 1ms 的墙钟。
//!
//! ## 实现与并发边界
//!
//! - 无 `signal`：直接等待 `tokio::time::sleep`，不可取消；调用方只能 drop future。
//! - 有 `signal`：先用 `is_cancelled()` 快速失败（对应 TS 的 `signal.aborted` 早退），
//!   再 `select!` 竞速。`select!` 没有 `biased`，两个分支同时就绪时选中哪个是随机的；
//!   因此“定时器到期”和“被取消”恰好同时发生时，返回 `Ok(())` 或 `Err(Aborted)` 都合法，
//!   调用方不应依赖其中之一。
//! - `CancellationToken::cancelled()` 是广播等待，只读不消费取消状态；取消后再次调用
//!   `sleep` 仍会立即返回 `Err`。
//! - `tokio::time::sleep` 要求当前处于启用了 time 驱动的 Tokio 运行时；在运行时外调用会
//!   panic。本仓库当前调用方是 `kiana-config` 的异步锁重试（`auth/lock.rs::lock_async`），
//!   它在 async 上下文把 `Err` 映射为 `LockError::Aborted` 并结束重试。

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::abort::AbortError;

/// TS: `sleep(ms, signal?)` —— 可被取消的定时等待。
///
/// 调用前已取消时立即返回 `Err(AbortError { message: "Aborted" })`；等待期间取消通常
/// 返回 `Err`，但与定时器同时就绪时 `select!` 也可能选中 timer 分支而返回 `Ok(())`
/// （见模块文档）。
///
/// 参数：`duration` 是最短等待时长；`signal` 为 `None` 时不可取消，`Some` 时只借用
/// token（不取得所有权，也不注册会残留的监听）。副作用：向 Tokio 定时器注册/注销；有
/// `signal` 时还会在 token 上注册一个等待者，随本次等待结束（`select!` 结束 / future
/// drop）而移除；不修改 token 的取消状态、不写日志。
pub async fn sleep(
    duration: Duration,
    signal: Option<&CancellationToken>,
) -> Result<(), AbortError> {
    let Some(signal) = signal else {
        tokio::time::sleep(duration).await;
        return Ok(());
    };

    if signal.is_cancelled() {
        return Err(AbortError::aborted());
    }

    tokio::select! {
        _ = tokio::time::sleep(duration) => Ok(()),
        _ = signal.cancelled() => Err(AbortError::aborted()),
    }
}
