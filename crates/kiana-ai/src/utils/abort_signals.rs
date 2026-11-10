//! `utils/abort-signals.ts`——合并多个中止信号。
//!
//! TS 的 `combineAbortSignals(signals)` 返回 `{ signal?, cleanup }`：
//! - 0 个有效 signal → 只有 `cleanup`（无 signal 键）；
//! - 1 个 → 直接返回该 signal，`cleanup` 是空操作；
//! - ≥2 个 → 新建 controller，按输入顺序注册 `once` 监听；遇到**已中止**的 signal 立即以其
//!   reason 中止并 `break`（后续 signal 不再注册）；`cleanup` 移除所有已注册监听。
//!   中止只在 controller 尚未中止时执行。
//!
//! 中止必须是**同步**的：监听器在 `abort()` 的调用栈里执行，不经过任务调度，调用方返回
//! 后即可观察到已完成的中止。
//! `cleanup` 只在被显式调用时移除监听——丢弃它不会停止传播（与 TS 的普通函数一致）。
//!
//! # 为什么需要合并
//!
//! 一次 provider 请求往往同时受两类信号影响：「调用方取消」与「内部超时 / 刷新控制器」。
//! `models` 的 refresh 把调用方 signal 与每个 provider 的 controller 合并；OAuth 登录把
//! 手动取消与交互层 signal 合并。TS 侧把多个 `AbortSignal` 汇成一个，Rust 侧对应地把多个
//! [`AbortScope`] 汇成一个新的 [`AbortScope`]：任一来源中止就让合并结果中止。
//!
//! # 流程
//!
//! 1. 过滤 `None`：0 个有效输入 → 不产生 signal；1 个 → 直接复用该 scope（不额外加监听层）。
//! 2. ≥2 个 → 新建 controller，按输入顺序逐个注册 `once` 监听；遇到**已中止**的输入立刻用
//!    它的 reason 中止并停止注册（controller 只可能中止一次，后续输入不再注册监听）。
//! 3. 返回的 `cleanup` 在显式调用时摘掉全部已注册监听；不调用就一直挂着（源 signal 与
//!    controller 因此被监听闭包互相持有，直到 cleanup 或源 signal 派发）。
//!
//! # 注意事项
//!
//! - cleanup **不是** Drop guard：丢弃 [`Cleanup`] 不会移除监听，活跃的监听会继续把源
//!   signal 的中止传播到 controller。`models` 在只关心本轮传播时直接丢弃该句柄。
//! - 丢弃 cleanup 的代价不止是「监听继续存在」：每个监听闭包都持有源 [`AbortScope`] 的
//!   克隆，而源 scope 的监听列表又持有该闭包，形成一个 `Arc` 引用环。TS 有 GC 可以回收这种
//!   环，Rust 不能——若源 signal 永不中止且不调用 cleanup，这些对象不会被释放。对长期存活
//!   的 signal 反复合并会累积监听；只有 cleanup 或源 signal 中止（派发时清空列表）才能
//!   打破这个环。
//! - 监听器同步执行；「先检查 `is_aborted`、后 `add_abort_listener`」不是原子操作。跨线程
//!   时源可能在两步之间中止，这次注册的监听不会补发，合并结果就收不到该中止（TS 的单线程
//!   事件循环没有这个交错）。详细说明见 [`crate::utils::abort`] 的模块文档。
//! - `signal` 为 `None` 表示「没有任何输入」（TS 的返回值没有 `signal` 键），调用方必须
//!   自行处理这个分支；`Some` 时它可能是输入之一（1 个输入）或新 controller（≥2 个）。
//!
//! TS: `utils/abort-signals.ts`

use std::sync::{Arc, Mutex};

use crate::utils::abort::{AbortListener, AbortListenerFn, AbortReason, AbortScope};

/// `combineAbortSignals` 的返回值。`signal` 为 `None` 对应 TS 没有 `signal` 键。
///
/// 两个字段都是公开的：调用方可以只取 `signal` 而丢弃 `cleanup`。按本模块语义这是允许的
/// （丢弃 cleanup 不会停止传播），代价是监听器保留在源 scope 上；由于监听闭包与源 scope
/// 构成引用环，源 signal 既不中止也不 cleanup 时这些对象不会被回收（见模块文档）。
#[derive(Debug, Clone, Default)]
pub struct CombinedAbortSignal {
    /// 合并后的 signal（0 个输入时为 `None`）。
    pub signal: Option<AbortScope>,
    /// 移除所有监听（幂等；可多次调用）。
    pub cleanup: Cleanup,
}

/// `cleanup` 句柄：调用 [`Cleanup::cleanup`] 时移除监听。
///
/// 克隆共享同一份注册表；它不是 RAII guard——没有 `Drop` 实现，丢弃句柄不会执行清理。
#[derive(Clone, Default)]
pub struct Cleanup {
    on_cleanup: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for Cleanup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cleanup")
            .field("has_action", &self.on_cleanup.is_some())
            .finish()
    }
}

impl Cleanup {
    /// 空操作。
    ///
    /// 0 个或 1 个输入时返回；调用 [`Cleanup::cleanup`] 无副作用。
    pub fn noop() -> Self {
        Cleanup { on_cleanup: None }
    }

    /// 调用清理动作（幂等：内部自行去重）。
    ///
    /// 首次调用会把注册表整批取走并逐个 `remove`；之后（包括克隆出的其它句柄）再调用
    /// 看到的是空表，因此不会重复执行移除。
    pub fn cleanup(&self) {
        if let Some(action) = &self.on_cleanup {
            action();
        }
    }
}

/// `AbortController.abort(signal.reason)`：reason 缺失时用 DOMException 缺省值。
fn abort_with_signal_reason(controller: &AbortScope, signal: &AbortScope) {
    controller.abort_with(
        signal
            .reason()
            .unwrap_or_else(AbortReason::default_abort_error),
    );
}

/// `combineAbortSignals(signals)`。
///
/// # 参数与返回
///
/// `signals` 里允许混有 `None`（对应 TS 的可选 signal），它们被直接过滤，不占「有效输入」
/// 计数。返回的 `CombinedAbortSignal::signal` 在 0 个有效输入时为 `None`；否则是一个
/// [`AbortScope`]（1 个输入时就是那个输入本身，≥2 个时是新 controller）。
///
/// # 语义
///
/// - ≥2 个输入时按数组顺序注册监听；任一源中止 → controller 以该源的 reason 中止。
/// - 遇到已中止的输入：立即中止 controller 并 `break`，**后续输入不再注册监听**。因此结果
///   与「哪个输入先中止」无关，只由遍历顺序决定。
/// - 已中止但没有 reason 的输入（例如令牌被直接 `cancel()`）使用
///   [`AbortReason::default_abort_error`]（"This operation was aborted"），不是回退文本。
/// - controller 已经中止后，其余监听即使触发也不会改写 reason（回调里先检查
///   `is_aborted`，`abort_with` 本身也会拒绝二次中止）。
///
/// # 失败模式
///
/// 本函数不返回错误，也不做 I/O。唯一的边界是上面提到的跨线程注册窗口：源在
/// 「`is_aborted` 检查」与「注册监听」之间中止时，合并结果收不到该事件。
///
/// TS: `utils/abort-signals.ts#combineAbortSignals`
pub fn combine_abort_signals(signals: &[Option<AbortScope>]) -> CombinedAbortSignal {
    let active: Vec<&AbortScope> = signals.iter().flatten().collect();
    match active.len() {
        0 => CombinedAbortSignal {
            signal: None,
            cleanup: Cleanup::noop(),
        },
        1 => CombinedAbortSignal {
            signal: Some(active[0].clone()),
            cleanup: Cleanup::noop(),
        },
        _ => {
            let controller = AbortScope::new();
            // 已注册的句柄集中保存，cleanup 时一次性摘除；用 Arc<Mutex> 是因为闭包需要
            // 与返回的 Cleanup 共享这张表，且 Cleanup 可能被跨线程调用。
            let registrations: Arc<Mutex<Vec<AbortListener>>> = Arc::new(Mutex::new(Vec::new()));
            for signal in active {
                // 先处理「已经中止」的输入：直接以其 reason 中止并停止注册剩余输入。
                if signal.is_aborted() {
                    abort_with_signal_reason(&controller, signal);
                    break;
                }
                let controller_inner = controller.clone();
                let signal_inner = signal.clone();
                let listener: AbortListenerFn = Arc::new(move || {
                    // 合并结果只中止一次：后到的源不覆盖先到的 reason。
                    if !controller_inner.is_aborted() {
                        abort_with_signal_reason(&controller_inner, &signal_inner);
                    }
                });
                let registration = signal.add_abort_listener(listener);
                registrations
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(registration);
            }
            let cleanup_registrations = Arc::clone(&registrations);
            CombinedAbortSignal {
                signal: Some(controller),
                cleanup: Cleanup {
                    on_cleanup: Some(Arc::new(move || {
                        // take 而不是 clear：第二次 cleanup 看到空表，天然幂等。
                        let registrations: Vec<AbortListener> = {
                            let mut registrations = cleanup_registrations
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            std::mem::take(&mut *registrations)
                        };
                        for registration in registrations {
                            registration.remove();
                        }
                    })),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_signals_has_no_signal() {
        let combined = combine_abort_signals(&[]);
        assert!(combined.signal.is_none());
        combined.cleanup.cleanup();
    }

    #[test]
    fn one_signal_is_returned_as_is() {
        let scope = AbortScope::new();
        let combined = combine_abort_signals(&[None, Some(scope.clone()), None]);
        assert_eq!(
            combined.signal.as_ref().map(AbortScope::is_aborted),
            Some(false)
        );
        scope.abort();
        assert!(combined.signal.expect("signal").is_aborted());
    }

    #[test]
    fn aborts_synchronously_when_any_input_aborts() {
        let first = AbortScope::new();
        let second = AbortScope::new();
        let combined = combine_abort_signals(&[Some(first.clone()), Some(second.clone())]);
        let signal = combined.signal.clone().expect("signal");
        assert!(!signal.is_aborted());
        first.abort_with(AbortReason {
            name: "Error".into(),
            message: "boom".into(),
        });
        // 不需要让出任务：TS 的 abort 事件是同步派发的。
        assert!(signal.is_aborted());
        assert_eq!(signal.reason().map(|r| r.message), Some("boom".into()));
    }

    #[test]
    fn works_without_a_tokio_runtime() {
        // 该测试本身运行在没有 reactor 的线程上；tokio::spawn 版本会在这里 panic。
        let first = AbortScope::new();
        let second = AbortScope::new();
        let combined = combine_abort_signals(&[Some(first.clone()), Some(second)]);
        let signal = combined.signal.expect("signal");
        first.abort();
        assert!(signal.is_aborted());
    }

    #[test]
    fn an_already_aborted_input_aborts_immediately() {
        let aborted = AbortScope::new();
        aborted.abort();
        let other = AbortScope::new();
        let combined = combine_abort_signals(&[Some(aborted), Some(other)]);
        assert!(combined.signal.expect("signal").is_aborted());
    }

    #[test]
    fn cleanup_removes_listeners_but_dropping_it_does_not() {
        let first = AbortScope::new();
        let second = AbortScope::new();
        let combined = combine_abort_signals(&[Some(first.clone()), Some(second)]);
        let signal = combined.signal.clone().expect("signal");
        // 丢弃 cleanup 不应停止传播（TS 的 cleanup 只是普通函数）。
        drop(combined.cleanup);
        first.abort();
        assert!(signal.is_aborted());

        // 显式 cleanup 之后源 signal 的中止不再影响 controller。
        let first = AbortScope::new();
        let second = AbortScope::new();
        let combined = combine_abort_signals(&[Some(first.clone()), Some(second)]);
        let signal = combined.signal.expect("signal");
        combined.cleanup.cleanup();
        combined.cleanup.cleanup();
        first.abort();
        assert!(!signal.is_aborted());
    }

    #[test]
    fn aborted_input_without_reason_uses_the_dom_exception_message() {
        let token = tokio_util::sync::CancellationToken::new();
        let aborted = AbortScope::from(token.clone());
        token.cancel();
        let other = AbortScope::new();
        let combined = combine_abort_signals(&[Some(aborted), Some(other)]);
        let reason = combined.signal.expect("signal").reason().expect("reason");
        assert_eq!(reason.message, "This operation was aborted");
    }
}
