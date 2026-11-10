//! `timers.ts`：宿主计时器契约、`setTimeout` 等价物与前台/后台包装。
//!
//! 与 TS 的差异都来自运行时本身，已在各条目上注明：
//! - Node 的 `unref()` 没有直接对应物。Rust 进程在 `main` 返回时退出，挂起的 tokio 任务不会
//!   单独撑住进程，所以后台计时器的“不阻止进程退出”语义天然成立；`native_timer` 不再区分。
//! - `in_workers`：TS 用 `navigator.userAgent === "Cloudflare-Workers"` 探测。此移植用
//!   `cloudflare` feature 标识 Workers 目标（原生构建恒为 false）。
//! - `setTimeout` 的回调运行在事件循环上；这里的 [`native_timer`] 用 tokio 任务，因此必须在
//!   tokio 运行时内调用。
//!
//! # 在系统中的位置
//!
//! `crate::session::SessionImpl::new` 调用 [`host_timers`] 包装宿主传入的 `set_timer`，
//! 得到的 [`Timers`] 存入 Session（`SessionInner.timers`）。Harness 在 `build_harness` 中
//! 另行调用 [`host_timers`] 生成一份独立的 [`Timers`]，经 `TaskSchedulerOptions.timers`
//! 交给调度器，存入其私有的 `Deps.timers`；两份 `Timers` 各自持有独立的“只报第一次”
//! 节流状态。
//! 前台计时器用于“必须发生的唤醒”：调度器的最早睡眠到期、Session 的 hold 定期存储、
//! 以及工具执行层的进度节流（`TaskRuntime::set_timer`）；后台计时器用于“只释放内存”的
//! 清扫：文档保留期与上下文过期。宿主拒绝后台计时器时，调用方必须自己降级（例如 Session
//! 把 `document_timers_unavailable` 置位，之后不再安排清扫）。
//!
//! # 取消语义
//!
//! [`Disarm`] 是 `FnOnce`：**调用它**才撤销；直接丢弃不会撤销已装载的计时器。
//! [`native_timer`] 的撤销同时置位取消标志并 `abort()` tokio 任务。到期与撤销并发时，
//! 回调可能已经通过标志检查并开始执行，因此撤销不保证回调绝对不跑；需要“回调绝不执行”
//! 的调用方得自己在回调里再检查状态：Session 的计时器回调会用 `Arc::ptr_eq` 判断自己是否
//! 仍是当前登记的计时器（`run_hold_store`、`run_sweep_job`），且在已关闭（`run_hold_store`
//! 还包括会话失败）时提前返回；调度器的 `arm_timer` / `schedule_expiry` 回调没有身份检查，
//! 只依赖 disarm 与基于当前状态的重新计算，不能保证回调绝不执行。
//!
//! # 运行时要求
//!
//! [`native_timer`] 内部 `tokio::spawn`，没有 tokio 运行时会 panic。`host_timers` 的兜底
//! 路径也会调用它：只有 `set_timer` 为 `None`，或宿主拒绝/panic 而回落到 [`native_timer`]
//! 时，前台计时器才要求调用发生在 tokio 运行时内；宿主成功返回 `Some(disarm)` 时前台直接
//! 使用宿主自己的实现，不会调用 [`native_timer`]。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;

/// `setTimeout` 支持的最长延迟；更长的延迟分多步等待。
///
/// 值是 `2^31 - 1`（约 24.8 天），即 Node/浏览器的 32 位有符号毫秒上限。超过它的
/// `delay_ms` 由 [`native_timer`] 拆成多段顺序 `sleep` 等待。
pub const MAX_TIMER_DELAY: u64 = 2_147_483_647;

/// 是否运行在 Cloudflare Workers 一类没有 `unref()` 的运行环境。
///
/// TS 的探测是 `typeof navigator !== "undefined" && navigator.userAgent === "Cloudflare-Workers"`；
/// 原生 Rust 没有 `navigator`，因此以 `cloudflare` feature 作为目标标识。
///
/// 这是编译期常量：同一个二进制里结果不会随运行环境变化。
pub fn in_workers() -> bool {
    cfg!(feature = "cloudflare")
}

/// 计时器回调。TS 的回调可以只被调用一次；这里用 `Arc<dyn Fn()>` 以便宿主拒绝后仍能
/// 交给 `setTimeout` 重试（对应 TS 里同一个函数值被传两次）。
///
/// 回调由计时器任务执行，因此要求 `Send + Sync`；它没有参数和返回值，失败只能由调用方
/// 在闭包里自行处理。
pub type TimerCallback = Arc<dyn Fn() + Send + Sync>;

/// 撤销函数（TS: `() => void`）。
///
/// 这是一个消费型闭包：调用它才撤销计时器；只把它 drop 掉不会阻止回调——`native_timer`
/// 把 `JoinHandle` 存在闭包里，drop 闭包等于丢弃句柄（tokio 会 detach 任务而不是取消）。
/// 调用两次在类型层面不可能（`FnOnce`），实现内部也保证 `abort()` 只对同一个句柄发生一次。
pub type Disarm = Box<dyn FnOnce() + Send + Sync>;

/// `HarnessOptions.setTimer` 的 options 参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetTimerOptions {
    /// 计时器只负责释放内存，宿主可以拒绝（例如为了休眠）。
    ///
    /// 为 `true` 时 [`native_timer`] 在 Workers 目标（[`in_workers`]）上直接返回 `None`；
    /// 在原生目标上，前台/后台对 [`native_timer`] 没有行为差别。宿主可以按自己的策略
    /// 决定是否拒绝。
    pub background: bool,
}

/// `HarnessOptions.setTimer`：在约 `delay_ms` 后调用一次 `callback`，返回撤销函数；
/// 宿主可以返回 `None` 表示拒绝。
///
/// 这是宿主提供的实现：延迟的单位是毫秒，可以是小数。宿主实现若 panic，[`host_timers`]
/// 会用 `catch_unwind` 捕获并转成 [`TimerError`] 报告，而不是让 Session 崩溃。
pub type SetTimer =
    Arc<dyn Fn(TimerCallback, f64, SetTimerOptions) -> Option<Disarm> + Send + Sync + 'static>;

/// 默认的 `HarnessOptions.setTimer`：`setTimeout`，超过上限的延迟分步等待。
///
/// 后台计时器在 Workers（[`in_workers`]）里被拒绝；原生构建里 tokio 任务不会阻止进程退出，
/// 因此总是返回撤销函数。
///
/// # 参数与返回
///
/// - `delay_ms`：毫秒，可为小数；负值按 0 处理。
/// - `options.background`：只参与 Workers 目标的拒绝判断（见 [`SetTimerOptions`]）。
/// - 返回 `Some(disarm)`；仅当 `background && in_workers()` 时返回 `None`。
///
/// # 失败与取消
///
/// 内部 `tokio::spawn`，必须在 tokio 运行时内调用，否则 panic。计时器任务只持有
/// `callback` 与取消标志；撤销会置位标志并 `abort()` 任务。分段等待意味着实际触发时间
/// 至少是 `delay_ms`，分段之间的调度延迟会累加，因此可能略晚。
pub fn native_timer(
    callback: TimerCallback,
    delay_ms: f64,
    options: SetTimerOptions,
) -> Option<Disarm> {
    if options.background && in_workers() {
        return None;
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    // 句柄槽：spawn 之后才拿得到 JoinHandle，而撤销闭包需要它；Mutex 同时保证 abort 只
    // 发生一次（`take()` 之后槽为空）。
    let handle_slot: Arc<Mutex<Option<JoinHandle<()>>>> = Arc::new(Mutex::new(None));

    let task_cancelled = Arc::clone(&cancelled);
    let handle = tokio::spawn(async move {
        // TS 的链式 arm：每次最多等 MAX_TIMER_DELAY，剩余部分继续。
        let mut remaining = delay_ms.max(0.0);
        loop {
            let wait = remaining.min(MAX_TIMER_DELAY as f64);
            tokio::time::sleep(Duration::from_secs_f64(wait / 1000.0)).await;
            // 每段睡醒后先看取消：撤销发生在这段等待期间时，不再进入回调。
            if task_cancelled.load(Ordering::SeqCst) {
                return;
            }
            if remaining > MAX_TIMER_DELAY as f64 {
                remaining -= MAX_TIMER_DELAY as f64;
            } else {
                callback();
                return;
            }
        }
    });
    if let Ok(mut slot) = handle_slot.lock() {
        *slot = Some(handle);
    }

    Some(Box::new(move || {
        cancelled.store(true, Ordering::SeqCst);
        if let Ok(mut slot) = handle_slot.lock()
            && let Some(handle) = slot.take()
        {
            handle.abort();
        }
    }))
}

/// 宿主回调（`HarnessOptions.onReport`）在这里只接收 `Box<dyn Error>`；TS 接收任意值。
///
/// 宿主 `setTimer` panic 时，[`host_timers`] 把 payload 转成实现 `Error` 的私有包装类型：
/// payload 是 `String` 或 `&'static str` 时用其文本做 `Display`，否则用固定兜底文案。
pub type TimerError = Box<dyn std::error::Error + Send + Sync>;
/// 报告函数。
///
/// 可能被多次调用：前台计时器共享“只报第一次”的节流，由 [`host_timers`] 实现；后台计时器
/// 不走这个节流，但只在宿主 panic 时报告——宿主返回 `None` 表示拒绝，不会调用它。
pub type TimerReport = Arc<dyn Fn(TimerError) + Send + Sync>;

/// Session 与 Harness 通过宿主 `setTimer` 装载的计时器（TS: `Timers`）。
///
/// 只能由 [`host_timers`] 构造（字段私有）：两个闭包分别对应 TS 的 `foreground` 与
/// `background`。前台保证有 [`Disarm`]；后台的 `None` 表示宿主拒绝，调用方不能假设回调
/// 一定发生。
pub struct Timers {
    foreground_arm: Box<dyn Fn(TimerCallback, f64) -> Disarm + Send + Sync>,
    background_arm: Box<dyn Fn(TimerCallback, f64) -> Option<Disarm> + Send + Sync>,
}

impl Timers {
    /// 必须触发的工作计时器：宿主抛错或拒绝时改用 `setTimeout`。
    ///
    /// 返回值一定可撤销：宿主抛错/拒绝后回落到 [`native_timer`]（见 [`host_timers`]）。
    pub fn foreground(&self, callback: TimerCallback, delay_ms: f64) -> Disarm {
        (self.foreground_arm)(callback, delay_ms)
    }

    /// 只释放内存的计时器：宿主拒绝时返回 `None`。
    ///
    /// `None` 表示这次回调不会发生，调用方应记录降级状态或等待下一次机会，而不是重试
    /// 同一延迟。
    pub fn background(&self, callback: TimerCallback, delay_ms: f64) -> Option<Disarm> {
        (self.background_arm)(callback, delay_ms)
    }
}

/// `HostSetTimerError`：宿主 `setTimer` 抛出的非 `Error` 值在 Rust 里只能是 panic payload，
/// 这里把它包装成 `Error` 以便报告。
#[derive(Debug)]
struct HostSetTimerError {
    message: String,
}

impl std::fmt::Display for HostSetTimerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HostSetTimerError {}

fn panic_to_error(payload: Box<dyn std::any::Any + Send>) -> TimerError {
    // Rust 的 panic payload 通常是 `String`（`format!`/`panic!("{x}")`）或 `&'static str`
    // （`panic!("literal")`）；其它类型没有可读文本，用固定文案保证报告总有内容。
    let message = match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => "HarnessOptions.setTimer threw".to_owned(),
        },
    };
    Box::new(HostSetTimerError { message })
}

/// 包装宿主 `setTimer`。
///
/// 前台计时器必须触发，否则工作停摆：宿主抛错或拒绝时用 `setTimeout` 装载，并只报告第一次
/// 失败。后台计时器被宿主抛错时每次都报告并拒绝。
///
/// # 输入
///
/// - `set_timer`：`None` 时直接把 [`native_timer`] 当作装载函数。
/// - `report`：接收失败。前台失败跨多次 `foreground` 调用共享一个 `reported` 标志，只有
///   第一次会报告；后台失败不共享该节流，且只在宿主 panic 时报告——宿主返回 `None` 表示
///   拒绝，不调用 `report`。
///
/// # 宿主 panic 的处理
///
/// 宿主闭包是外部代码，`catch_unwind` 把其中的 panic 转成 [`TimerError`]：payload 是
/// `String` 或 `&'static str` 时用其文本，否则用固定文案
/// `"HarnessOptions.setTimer threw"`。前台报告后回落到 [`native_timer`]；后台报告后
/// 返回 `None`。注意这里只捕获 Rust panic，不捕获进程 abort。
pub fn host_timers(set_timer: Option<SetTimer>, report: TimerReport) -> Timers {
    let arm: SetTimer = set_timer.unwrap_or_else(|| Arc::new(native_timer));
    let reported = Arc::new(AtomicBool::new(false));

    let fall_back = {
        let reported = Arc::clone(&reported);
        let report = Arc::clone(&report);
        move |error: TimerError| {
            // `swap` 返回旧值：只有第一次 false -> true 的调用会真正报告。
            if reported.swap(true, Ordering::SeqCst) {
                return;
            }
            report(error);
        }
    };

    let foreground_arm = {
        let arm = Arc::clone(&arm);
        let fall_back = fall_back; // move closure into this closure
        Box::new(move |callback: TimerCallback, delay_ms: f64| -> Disarm {
            // 宿主实现可能 panic；前台计时器不能因此让 Session 崩溃，先捕获再决定。
            let result = catch_unwind(AssertUnwindSafe(|| {
                arm(
                    Arc::clone(&callback),
                    delay_ms,
                    SetTimerOptions { background: false },
                )
            }));
            let armed = match result {
                Ok(Some(disarm)) => Some(disarm),
                Ok(None) => {
                    // 宿主明确拒绝：前台计时器不能缺席，随后回落到 native_timer。
                    fall_back(Box::new(HostSetTimerError {
                        message:
                            "HarnessOptions.setTimer declined a timer that paces work; using setTimeout"
                                .to_owned(),
                    }));
                    None
                }
                Err(payload) => {
                    fall_back(panic_to_error(payload));
                    None
                }
            };
            match armed {
                Some(disarm) => disarm,
                None => {
                    match native_timer(callback, delay_ms, SetTimerOptions { background: false }) {
                        Some(disarm) => disarm,
                        // 前台计时器在原生构建里总是被装载；对应 TS 的非空断言。
                        None => Box::new(|| {}),
                    }
                }
            }
        })
    };

    let background_arm = {
        let arm = Arc::clone(&arm);
        Box::new(
            move |callback: TimerCallback, delay_ms: f64| -> Option<Disarm> {
                match catch_unwind(AssertUnwindSafe(|| {
                    arm(callback, delay_ms, SetTimerOptions { background: true })
                })) {
                    Ok(disarm) => disarm,
                    Err(payload) => {
                        // 后台失败不重试：报告后返回 None，让调用方知道这次清扫不会发生。
                        report(panic_to_error(payload));
                        None
                    }
                }
            },
        )
    };

    Timers {
        foreground_arm,
        background_arm,
    }
}
