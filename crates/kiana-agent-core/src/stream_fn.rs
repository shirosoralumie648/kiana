//! stream-fn.ts：默认 `StreamFn` 的进程级单例（TS 用模块级变量 + `setDefaultStreamFn`）。
//!
//! TS 源：
//!
//! ```ts
//! let defaultStreamFn: StreamFn | undefined;
//! export function setDefaultStreamFn(streamFn: StreamFn | undefined): void
//! export function getDefaultStreamFn(): StreamFn  // 未配置时 throw
//! ```
//!
//! `getDefaultStreamFn` 只被 `agent-loop.ts` / `agent.ts` 内部使用，**不**出现在包的公开面
//! （`index.ts` 只显式导出 `setDefaultStreamFn`）。
//!
//! 求值时机固定（惰性与急切求值会影响构造期是否失败）：`agentLoop`/`agentLoopContinue` 里 `streamFn ?? getDefaultStreamFn()`
//! 是惰性的，只在真正要发请求时求值；`Agent` 构造函数是**急切**的
//! （`this.streamFunction = runtimeOptions.streamFn ?? getDefaultStreamFn()`），未配置默认流函数时
//! `new Agent({...})` 立即失败，而不是等到第一次 prompt。
//!
//! Rust 侧用 `OnceLock<RwLock<Option<StreamFn>>>`；[`get_default_stream_fn`] 对应 TS 的“可缺省读取”
//! （骨架固定签名），[`require_default_stream_fn`] 表示“未配置即报错”，错误文本是当前稳定契约。
//!
//! # 设计原因与并发约束
//!
//! - 为什么需要进程级单例：`agentLoop` / `Agent` 都允许调用方不传 `streamFn`，此时使用默认值。
//!   例如 `kiana-session` 的 SDK 入口在首次调用时通过 `ensure_default_stream_fn()` 安装
//!   `streamSimple`。
//! - `OnceLock` 负责首次访问时初始化静态单元；`RwLock` 让多个读者可以同时读取，set/clear
//!   这两个写者互斥。
//! - 锁中毒（持锁线程 panic）时用 `p.into_inner()` 取回内部值继续工作：默认流函数是全局配置，
//!   不应因为一次 panic 就让之后所有请求永久失败。
//! - [`get_default_stream_fn`] 返回的是 `StreamFn`（内部是 `Arc`）的**克隆**：取到之后即使别的
//!   线程再 `set` / `clear`，本次 run 使用的仍是取到的那一个流函数；下一次读取才会看到新值。
//! - 这是**进程全局**状态，不是每个 `Agent` 一份。并行测试必须自己调用
//!   [`clear_default_stream_fn`] 隔离（本文件的单元测试与 `tests/public_api.rs` 都这样做）。
//!
//! 注意：单例本身不做“只允许设置一次”的限制，后一次 [`set_default_stream_fn`] 会直接覆盖前一次。

use std::sync::{OnceLock, RwLock};

use crate::types::StreamFn;

/// `getDefaultStreamFn()` 未配置默认流函数时的错误消息（逐字）。
pub const NO_DEFAULT_STREAM_FN_MESSAGE: &str =
    "No default stream function configured. Pass streamFn explicitly or call setDefaultStreamFn().";

// 进程级单元：首次调用时初始化，此后 set/get/clear 都经过同一把读写锁。
// 中毒时取回内部值（`into_inner`）：这里保存的是可替换的全局配置，而不是需要保持
// 一致性的复合状态，继续使用不会产生半个写入的中间值。
fn cell() -> &'static RwLock<Option<StreamFn>> {
    static CELL: OnceLock<RwLock<Option<StreamFn>>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(None))
}

/// `setDefaultStreamFn(fn)`。
///
/// TS 允许传 `undefined` 清除；Rust 的 `StreamFn` 是具体类型，清除用
/// [`clear_default_stream_fn`]（等价于 `setDefaultStreamFn(undefined)`）。
///
/// 写入后所有**新的**读取者看到新值；已经取出的 `StreamFn` 克隆不受影响。
pub fn set_default_stream_fn(f: StreamFn) {
    let mut guard = cell().write().unwrap_or_else(|p| p.into_inner());
    *guard = Some(f);
}

/// `getDefaultStreamFn()` 的可缺省形态：未配置时返回 `None`（不清除、不报错）。
///
/// 返回的是句柄克隆（`StreamFn` 内部为 `Arc`），调用方可以自由长期持有；
/// 它不会因为之后有人 `set` / `clear` 而失效。
pub fn get_default_stream_fn() -> Option<StreamFn> {
    cell().read().unwrap_or_else(|p| p.into_inner()).clone()
}

/// `getDefaultStreamFn()`：未配置时返回携带稳定错误文本的 [`DefaultStreamFnError`]。
///
/// 与 [`get_default_stream_fn`] 的区别只是错误通道：这里把 `None` 转成 `Err`，
/// 错误消息即 [`NO_DEFAULT_STREAM_FN_MESSAGE`]（`Agent::new` 的急切求值走这条路径）。
pub fn require_default_stream_fn() -> Result<StreamFn, DefaultStreamFnError> {
    get_default_stream_fn().ok_or(DefaultStreamFnError)
}

/// 测试与宿主隔离用：清空默认值（TS 的 `setDefaultStreamFn(undefined)`）。
///
/// 已经在运行中的 run 持有自己的克隆，不受影响；之后的读取才会得到 `None`。
pub fn clear_default_stream_fn() {
    let mut guard = cell().write().unwrap_or_else(|p| p.into_inner());
    *guard = None;
}

/// `getDefaultStreamFn()` 未配置时的错误。
///
/// `Display` 逐字等于 TS 的 `error.message`（ARCHITECTURE.md §4.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultStreamFnError;

impl std::fmt::Display for DefaultStreamFnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(NO_DEFAULT_STREAM_FN_MESSAGE)
    }
}

impl std::error::Error for DefaultStreamFnError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn sentinel() -> StreamFn {
        std::sync::Arc::new(|_model, _context, _options| {
            Box::pin(async { panic!("sentinel stream fn must not be invoked") })
        })
    }

    #[test]
    fn default_stream_fn_set_get_clear() {
        // 单一测试串行覆盖全局单例，避免并行测试互相干扰。
        clear_default_stream_fn();
        assert!(get_default_stream_fn().is_none());
        assert_eq!(
            require_default_stream_fn()
                .map(|_| ())
                .unwrap_err()
                .to_string(),
            "No default stream function configured. Pass streamFn explicitly or call setDefaultStreamFn()."
        );

        set_default_stream_fn(sentinel());
        assert!(get_default_stream_fn().is_some());
        assert!(require_default_stream_fn().is_ok());

        clear_default_stream_fn();
        assert!(get_default_stream_fn().is_none());
        assert!(require_default_stream_fn().is_err());
    }
}
