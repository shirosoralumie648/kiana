//! `packages/agent/test/utils/wait-for-tick.ts`：
//! `new Promise((resolve) => setTimeout(resolve, 0))`。
//!
//! `setTimeout(0)` 是宏任务让出；Rust 侧用 [`tokio::task::yield_now`]（把当前任务重新排到
//! 运行队列尾部，让其他任务先跑），语义上等价于「等一个 tick」。

/// `waitForTick()`。
pub async fn wait_for_tick() {
    tokio::task::yield_now().await;
}
