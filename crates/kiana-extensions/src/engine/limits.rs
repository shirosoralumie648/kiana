//! 内存/栈/中断预算与 100 ms 看门狗（`interrupted` 标志 + `set_interrupt_handler`）。
//!
//! TS 对应物：无（Node 由 V8 的 isolate 限制兜底）。设计文档 §2.7、§10.5；
//! `ARCHITECTURE.md` §5 的 `engine/limits.rs` 一行。
//!
//! 三条硬规则（§2.7 的实测结论）：
//!
//! 1. 中断处理器只能读原子变量，**绝不调用 JS**（它会在任意指令边界重入引擎）。
//! 2. `for(;;){}` 在默认看门狗（[`DEFAULT_WATCHDOG`] = 100 ms）内以
//!    `InternalError: interrupted` 结束；错误对象上的 `message` 是 `interrupted`。
//! 3. 显式 deadline 覆盖看门狗：handler 派发 30 s、工厂加载 10 s、渲染不显式设
//!    （仍受默认的 100 ms 硬上限约束，设计文档 §2.7 的 R11）。
//!
//! 时间轴的选择：中断截止时间用 [`std::time::Instant`]（与被中断的同步 JS 在同一个
//! 真实时钟上，且测试里 `tokio::time::pause()` 不会冻结它）；定时器堆用
//! `tokio::time::Instant`（见 `engine/timers.rs`），以便主循环 `sleep_until` 与
//! `tui-core.md` §8.2 的 Timer 抽象共用同一时间轴。
//!
//! # 执行流程与优先级
//!
//! 中断处理器是交给 `Runtime::set_interrupt_handler` 的闭包，由 QuickJS 在**指令边界**
//! 调用；它至多做两次原子读和一次时钟读（`InterruptState::tripped`），不分配、不加锁、
//! 不进入 JS。两个中断来源相互独立：
//!
//! - 外部标志：`JsEngine::interrupt()` 置位，sticky，需要 `clear_interrupt` 清除；
//! - 截止时刻 `deadline_ms`：0 表示没有 deadline。写它的是 `set_deadline` /
//!   `deadline_guard`（显式预算）与 `enter_watchdog`（每次 `with_js` 的默认看门狗）。
//!
//! 一次 `with_js` 的时序：
//!
//! 1. `enter_watchdog` 读取当前 `deadline_ms`；为 0 或已过期时写入
//!    `now + EngineConfig::watchdog`（`watchdog` 为 0 则不写，调用方主动禁用兜底）。
//! 2. `Context::with` 执行闭包。期间每次指令边界检查 `tripped()`，任一来源成立就
//!    以 `InternalError: interrupted` 结束本次执行（逐字文案锁定在
//!    `tests/fixtures/engine/quickjs_messages.json`）。
//! 3. `WatchdogScope` 析构，把 `deadline_ms` 恢复成进入前的值：一次事务的预算不会
//!    泄漏给下一个事务。
//!
//! 显式 deadline 优先于默认看门狗：只要当前值非 0 且未过期，`with_js` 就沿用调用方的
//! 值——**即使它比 100 ms 短**。handler 派发 30 s、工厂加载 10 s 就是这样实现的
//! （见 [`HANDLER_DEADLINE`]、[`FACTORY_DEADLINE`]）。
//!
//! # 注意事项
//!
//! - 引擎只有一个 deadline 原子，无法区分事务归属：`DeadlineGuard` 不得跨 `.await`
//!   持有（其文档说明了跨 await 时会发生什么）。
//! - 这里所有原子读写都是 `Ordering::Relaxed`。引擎按单线程模型使用（`JsEngine`
//!   不是 `Send`/`Sync`，见 `engine/mod.rs`），中断处理器与引擎在同一条线程上推进，
//!   所以只需要原子性，不需要跨线程内存序。
//! - 本模块不把中断映射成扩展错误：`interrupted` 是 QuickJS 抛出的 JS 异常，由上层按
//!   `ca-extensions.md` §3.8 的规则物化；行为回归在 `tests/engine.rs` 的
//!   `watchdog_interrupts_infinite_loop_within_100ms` 等测试里。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rquickjs::Runtime;

/// 默认内存上限：512 MiB（设计文档 §2.7）。
///
/// 建 runtime 时一次性设置。耗尽时 JS 侧看到 `InternalError: out of memory`，可被
/// `catch`；批量分配已经把堆推到上限时 QuickJS 可能退化成 `throw null`（见
/// `tests/fixtures/engine/quickjs_messages.json` 的 `out_of_memory_null_fallback` 注释）。
/// 行为回归：`tests/engine.rs` 的 `out_of_memory_is_catchable_inside_js` /
/// `out_of_memory_is_reported_when_uncaught`。
pub const DEFAULT_MEMORY_LIMIT: usize = 512 * 1024 * 1024;

/// 默认 JS 栈上限：4 MiB（无限递归 → `RangeError: Maximum call stack size exceeded`）。
///
/// 同样在建 runtime 时一次性设置。QuickJS 需要宿主线程有足够的原生栈才能自行检测溢出
/// 并抛 `RangeError`；`tests/engine.rs` 的 `stack_overflow_reports_maximum_call_stack`
/// 为此显式在 16 MiB 原生栈的线程上运行（该测试注释说明了这一点）。
pub const DEFAULT_MAX_STACK_SIZE: usize = 4 * 1024 * 1024;

/// 默认看门狗：单次同步 JS 事务的 100 ms 硬上限（设计文档 §2.7）。
///
/// 每次 `with_js` 进入时按需安装；没有未过期的显式 deadline 时，同步死循环在约 100 ms 后
/// 以 `InternalError: interrupted` 结束。回归：`tests/engine.rs` 的
/// `watchdog_interrupts_infinite_loop_within_100ms`（断言在 50 ms 与 2 s 之间触发）。
pub const DEFAULT_WATCHDOG: Duration = Duration::from_millis(100);

/// 事件 handler 派发的 deadline（设计文档 §2.7）。
///
/// 对应 [`super::JsEngine::with_js_handler_deadline`] / `with_handler_deadline`；
/// `loader` 里扩展 handler 的同步调用（`loader/mod.rs` 的 `call_raw` 等）走这条路径。
pub const HANDLER_DEADLINE: Duration = Duration::from_secs(30);

/// 扩展工厂（`factory`）加载的 deadline（设计文档 §2.7）。
///
/// 对应 [`super::JsEngine::with_js_factory_deadline`]。它只覆盖调用工厂的**同步段**：
/// 工厂返回的 thenable 之后的等待发生在事务之外，按普通 `with_js` 事务的预算处理。
pub const FACTORY_DEADLINE: Duration = Duration::from_secs(10);

/// 引擎限制配置。
///
/// `generation` 是代际号：`/reload` / 会话替换时整代销毁再建，新代 +1（设计文档 §2.8）；
/// 模块名里的 `?v=<gen>-<seq>` 由 [`EngineConfig::generation`] 派生。
///
/// 配置是值语义的快照：`JsEngine::with_config` 把它存进引擎的共享状态。
/// `memory_limit` / `max_stack_size` 只在建 runtime 时设置一次，之后再改外部副本不会
/// 影响已建引擎；`watchdog` 则在每次 `with_js` 进入时读取。要变更内存/栈限制需要重建
/// 引擎（`/reload` 走的是这条路径）。
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// `Runtime::set_memory_limit` 的值（字节）。0 表示不限制。
    pub memory_limit: usize,
    /// `Runtime::set_max_stack_size` 的值（字节）。
    pub max_stack_size: usize,
    /// 单次 `with_js` 事务的默认 deadline。`Duration::ZERO` 表示禁用默认看门狗
    /// （此后同步 JS 只能靠显式 deadline 或外部中断停下）。
    pub watchdog: Duration,
    /// 代际号（模块名与句柄失效用）。不参与任何限制。
    pub generation: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            memory_limit: DEFAULT_MEMORY_LIMIT,
            max_stack_size: DEFAULT_MAX_STACK_SIZE,
            watchdog: DEFAULT_WATCHDOG,
            generation: 0,
        }
    }
}

impl EngineConfig {
    /// 默认配置（512 MiB / 4 MiB / 100 ms / generation 0）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 返回同一配置但代际号 +1（`/reload` 的“整代销毁再建”）。
    ///
    /// 其余字段原样克隆；代际号用 `wrapping_add`，回绕后回到 0（需要 2^64 次调用）。
    /// 回归：`tests/engine.rs` 的 `engine_config_next_generation_bumps_only_generation`。
    pub fn next_generation(&self) -> Self {
        Self {
            generation: self.generation.wrapping_add(1),
            ..self.clone()
        }
    }
}

/// 中断看门狗共享状态。
///
/// `EngineShared` 与 `Runtime::set_interrupt_handler` 的闭包各持有一份
/// `Arc<InterruptState>`。引擎只在这一条线程上使用（`JsEngine` 不是 `Send`/`Sync`），
/// 闭包也只在 QuickJS 执行 JS 期间被调用，所以字段用原子量即可，不需要锁。
///
/// 两个中断来源相互独立：外部标志（sticky）与截止时刻（0 表示没有 deadline）；
/// `tripped` 只要其一成立就返回 `true`。
#[derive(Debug)]
pub(crate) struct InterruptState {
    /// 外部置位的中断标志（`/reload`、`engine.interrupt()`）。
    /// 只由 `interrupt` / `clear_interrupt` 以及关闭流程（`engine/shutdown.rs`）修改。
    pub(crate) interrupted: AtomicBool,
    /// 绝对截止时刻（毫秒，相对 [`InterruptState::origin`]）；0 表示无截止。
    /// 比较用 `now_ms() > deadline_ms`：恰好相等不算过期。
    pub(crate) deadline_ms: AtomicU64,
    /// 单调时钟原点。`Instant` 是 `Copy`，可安全地被闭包捕获。
    pub(crate) origin: Instant,
}

impl InterruptState {
    /// 新建中断状态，以当前时刻为原点。
    pub(crate) fn new() -> Self {
        Self {
            interrupted: AtomicBool::new(false),
            deadline_ms: AtomicU64::new(0),
            origin: Instant::now(),
        }
    }

    /// 当前时刻（毫秒，相对 origin）。
    pub(crate) fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }

    /// 是否已触发中断（标志置位或截止时间已过）。
    ///
    /// 两个条件相互独立且都是 sticky：外部标志要 `clear_interrupt` 才清除；截止时刻
    /// 到期后会一直为真，直到被重写或清零。QuickJS 可能在每个指令边界调用本函数，
    /// 所以实现只有至多两次原子读加一次时钟读，不做分配、不加锁。
    pub(crate) fn tripped(&self) -> bool {
        if self.interrupted.load(Ordering::Relaxed) {
            return true;
        }
        let deadline = self.deadline_ms.load(Ordering::Relaxed);
        deadline != 0 && self.now_ms() > deadline
    }
}

/// 把内存/栈上限与中断处理器装到 runtime 上。
///
/// 必须在创建 `Context` 之前调用（rquickjs 的限制是 runtime 级的）。
/// 中断处理器只捕获 `Arc<InterruptState>`，内部只做原子读与一次时钟读，不调用 JS：
/// 它在任意指令边界被调用，此时引擎已经处在一次 JS 事务里，再调用 JS 就是嵌套事务。
///
/// 处理器返回 `true` 时 QuickJS 中断当前执行，表现为 `InternalError: interrupted`；
/// 它只在指令边界检查，极短的一段表达式可能检查不到就执行完了（见
/// `tests/engine.rs` 的 `external_interrupt_flag_stops_the_next_transaction`）。
pub(crate) fn install_runtime_limits(
    rt: &Runtime,
    state: Arc<InterruptState>,
    memory_limit: usize,
    max_stack_size: usize,
) {
    rt.set_memory_limit(memory_limit);
    rt.set_max_stack_size(max_stack_size);
    rt.set_interrupt_handler(Some(Box::new(move || state.tripped())));
}

/// `with_js` 的看门狗作用域：进入时按需装默认 deadline，退出时恢复进入前的值。
///
/// 未安装时（已有未过期 deadline，或 `watchdog` 为 0）不动全局状态。
pub(crate) struct WatchdogScope<'a> {
    engine: &'a super::JsEngine,
    /// 进入前的 deadline（通常是 0，或外层 `DeadlineGuard` 的值）。
    previous: u64,
    /// 本次是否真的写入了默认看门狗；`Drop` 只在写入过时恢复。
    installed: bool,
}

impl Drop for WatchdogScope<'_> {
    fn drop(&mut self) {
        // 未安装时 `previous` 与当前值相同，写回与否等价；显式判断避免无谓的原子写。
        if self.installed {
            self.engine
                .shared
                .interrupt
                .deadline_ms
                .store(self.previous, Ordering::Relaxed);
        }
    }
}

/// 进入一次 JS 事务的看门狗（见 [`super::JsEngine::with_js`]）。
///
/// 两种情况下装默认看门狗：
///
/// 1. 没有显式 deadline（`0`）；
/// 2. 显式 deadline 已经过期。后者是跨 `.await` 误用 [`DeadlineGuard`] 的兜底：守卫的
///    预算到期后，主循环里与那个派发无关的事务（渲染、定时器回调、其它扩展的完成事件）
///    仍拿到自己的 100 ms 看门狗，而不是在第一条指令就被 `interrupted`。事务结束恢复
///    进入前的值，守卫的语义不变。
///
/// `watchdog == 0` 表示“不装默认看门狗”（与 `set_deadline(0)` 一样留给调用方全权控制）。
///
/// 默认看门狗存在的理由：单线程引擎里一次同步死循环会冻结整个主循环，100 ms 上限把
/// 单次冻结限制在可接受范围内（设计文档 §2.7 的实测约 100 ms）。已知会长时间同步
/// 运行的派发（handler 30 s、工厂 10 s）用显式 deadline 覆盖它。
///
/// 注意 `watchdog == 0` 时不会安装兜底：若外层已有一个**过期**的 deadline，它仍留在
/// 原子上，下一次指令边界检查就会中断（极短表达式可能来不及检查）。这是调用方主动
/// 禁用兜底的直接结果，不是疏漏。
pub(crate) fn enter_watchdog(engine: &super::JsEngine) -> WatchdogScope<'_> {
    let state = &engine.shared.interrupt;
    // 读进入前的值：0 = 没有显式 deadline；非 0 = 外层 `DeadlineGuard` / `set_deadline`
    // 写入的预算（可能已经过期）。
    let previous = state.deadline_ms.load(Ordering::Relaxed);
    // 已过期的预算不能当作本次事务的预算：那是跨 `.await` 误用守卫的残留值。
    let expired = previous != 0 && state.now_ms() > previous;
    let installed = (previous == 0 || expired) && !engine.shared.config.watchdog.is_zero();
    if installed {
        // 毫秒粒度：亚毫秒部分被截断；`as u64` 对超过 u64 毫秒的极端 Duration 会按模
        // 截断（正常配置远小于该范围）。
        let watchdog_ms = engine.shared.config.watchdog.as_millis() as u64;
        // saturating_add：极端大的 watchdog 不会回绕成“过去”的时刻。
        let deadline = state.now_ms().saturating_add(watchdog_ms);
        state.deadline_ms.store(deadline, Ordering::Relaxed);
    }
    WatchdogScope {
        engine,
        previous,
        installed,
    }
}

/// 显式 deadline 的 RAII 守卫：构造时设置，`Drop` 时恢复进入前的值。
///
/// 可以嵌套。
///
/// # 不要跨 `.await` 持有（设计文档 §2.7）
///
/// 引擎只有**一个**中断截止时间原子（`set_interrupt_handler` 只能读原子，不能区分
/// 事务归属），而单线程主循环里的渲染、定时器回调、其它扩展的完成事件都会在同一个
/// 引擎上开新的 `with_js` 事务。守卫活着期间：
///
/// - 这些无关事务会继承守卫的 deadline（失去自己的 100 ms 看门狗）；
/// - deadline 到期时正在运行的事务会被一并 `interrupted`，而 TS/Node 里渲染从不超时。
///   （到期**之后**才开始的事务有兜底：`with_js` 会给它们装默认看门狗，见
///   `enter_watchdog`。）
///
/// 所以守卫只适用于**同步**派发：`let _g = engine.deadline_guard(FACTORY_DEADLINE);`
/// 紧跟着若干次同步 `with_js`，在下一个 `.await` 之前 `drop(_g)`。需要给单次事务
/// 放宽上限（handler 派发 30 s、工厂 10 s）时用
/// [`super::JsEngine::with_js_deadline`] / [`super::JsEngine::with_js_handler_deadline`] /
/// [`super::JsEngine::with_js_factory_deadline`]：它们进入事务前设置、退出后立刻恢复，
/// 不可能泄漏到别的事务。
pub struct DeadlineGuard<'a> {
    engine: &'a super::JsEngine,
    /// 构造时的旧值；`Drop` 无条件写回，所以嵌套守卫会自动恢复外层预算。
    previous: u64,
}

impl Drop for DeadlineGuard<'_> {
    fn drop(&mut self) {
        self.engine
            .shared
            .interrupt
            .deadline_ms
            .store(self.previous, Ordering::Relaxed);
    }
}

impl super::JsEngine {
    /// 设置显式 deadline（覆盖默认看门狗），相对当前时刻。
    ///
    /// 与 [`super::JsEngine::with_deadline`] / [`super::JsEngine::deadline_guard`] 不同，
    /// 本方法不自动恢复；调用方负责 [`super::JsEngine::clear_deadline`]。
    ///
    /// 这是引擎级状态，会作用于设置期间**所有** `with_js` 事务；只在同步派发里使用，
    /// 不要跨 `.await` 保持。异步派发用 [`super::JsEngine::with_js_deadline`]。
    ///
    /// 已经过期的 deadline 不会在事务的第一条指令就打断它：`with_js` 会给该事务装
    /// 默认看门狗（见 `engine/limits.rs` 的 `enter_watchdog`），避免一个跨 `.await`
    /// 的守卫误伤渲染/定时器等无关事务。例外是 `watchdog` 被配置为 0：此时没有兜底，
    /// 过期值会立刻生效。
    ///
    /// 预算按毫秒计：`as_millis()` 之后的亚毫秒部分被丢弃，超过 `u64::MAX` 毫秒的极端值
    /// 被钳制；`now + 预算` 用饱和加法，不会回绕成一个过去时刻。
    pub fn set_deadline(&self, after: Duration) {
        let state = &self.shared.interrupt;
        let deadline = state
            .now_ms()
            .saturating_add(after.as_millis().min(u64::MAX as u128) as u64);
        state.deadline_ms.store(deadline, Ordering::Relaxed);
    }

    /// 清除显式 deadline（恢复默认看门狗）。
    ///
    /// 只清 `deadline_ms`，不影响外部中断标志（那个要用
    /// [`super::JsEngine::clear_interrupt`]）。
    pub fn clear_deadline(&self) {
        self.shared
            .interrupt
            .deadline_ms
            .store(0, Ordering::Relaxed);
    }

    /// 当前是否设置了显式 deadline。
    ///
    /// 只判断“非 0”，不判断是否已经过期（过期判断用
    /// [`super::JsEngine::deadline_expired`]）。
    pub fn has_deadline(&self) -> bool {
        self.shared.interrupt.deadline_ms.load(Ordering::Relaxed) != 0
    }

    /// deadline 是否已过（读原子，不进入 JS）。
    ///
    /// 实现调用 `InterruptState::tripped`，所以它是一个**组合判断**：外部中断标志
    /// 置位时也返回 `true`，即使没有设置 deadline。只关心外部标志时用
    /// [`super::JsEngine::is_interrupted`]；只有它为 `false` 时，本方法的返回值才等价于
    /// “截止时刻已过”。
    pub fn deadline_expired(&self) -> bool {
        self.shared.interrupt.tripped()
    }

    /// 在显式 deadline 内运行 `f`（通常是若干次**同步** [`super::JsEngine::with_js`]）。
    ///
    /// ```ignore
    /// engine.with_deadline(HANDLER_DEADLINE, || engine.with_js(|ctx| ...));
    /// ```
    ///
    /// **不得在 `f` 里跨 `.await`**：deadline 是引擎级原子，跨越 await 会让主循环里
    /// 其它无关事务（渲染、定时器、别的扩展的完成事件）一起继承它，见
    /// [`DeadlineGuard`] 的说明。异步派发请按事务使用
    /// [`super::JsEngine::with_js_deadline`]。
    ///
    /// `f` 里的所有 `with_js` 共享这一个预算（引擎级单值）。若进入前已有外层 deadline，
    /// 本方法会覆盖它，退出时恢复（即使 `f` panic，`Drop` 也会恢复）。
    pub fn with_deadline<R>(&self, after: Duration, f: impl FnOnce() -> R) -> R {
        let _guard = self.deadline_guard(after);
        f()
    }

    /// 在**单次** `with_js` 事务的显式 deadline 内运行 `f`（进入前设置，退出后恢复）。
    ///
    /// 这是 handler 派发（30 s）与工厂加载（10 s）的推荐用法：deadline 只对这一个
    /// 同步事务生效，不会泄漏给同一主循环里的渲染/定时器/其它事务（设计文档 §2.7）。
    ///
    /// ```ignore
    /// engine.with_js_deadline(HANDLER_DEADLINE, |ctx| handler.call(...));
    /// ```
    ///
    /// 守卫在 `with_js` 返回后析构；闭包 panic 时同样由 `Drop` 恢复旧值。
    pub fn with_js_deadline<R>(
        &self,
        after: Duration,
        f: impl for<'js> FnOnce(rquickjs::Ctx<'js>) -> R,
    ) -> R {
        let _guard = self.deadline_guard(after);
        self.with_js(f)
    }

    /// [`super::JsEngine::with_js_deadline`] 的 handler 派发版本（[`HANDLER_DEADLINE`] = 30 s）。
    pub fn with_js_handler_deadline<R>(
        &self,
        f: impl for<'js> FnOnce(rquickjs::Ctx<'js>) -> R,
    ) -> R {
        self.with_js_deadline(HANDLER_DEADLINE, f)
    }

    /// [`super::JsEngine::with_js_deadline`] 的工厂加载版本（[`FACTORY_DEADLINE`] = 10 s）。
    pub fn with_js_factory_deadline<R>(
        &self,
        f: impl for<'js> FnOnce(rquickjs::Ctx<'js>) -> R,
    ) -> R {
        self.with_js_deadline(FACTORY_DEADLINE, f)
    }

    /// 在 handler 派发 deadline（[`HANDLER_DEADLINE`] = 30 s）内运行 `f`。
    pub fn with_handler_deadline<R>(&self, f: impl FnOnce() -> R) -> R {
        self.with_deadline(HANDLER_DEADLINE, f)
    }

    /// 在工厂加载 deadline（[`FACTORY_DEADLINE`] = 10 s）内运行 `f`。
    pub fn with_factory_deadline<R>(&self, f: impl FnOnce() -> R) -> R {
        self.with_deadline(FACTORY_DEADLINE, f)
    }

    /// 显式 deadline 的 RAII 守卫（见 [`DeadlineGuard`]）。
    ///
    /// 建议紧跟着同步派发并在下一个 `.await` 之前 drop；守卫存活期间（预算未过期前）的
    /// **所有** `with_js` 事务都继承这个预算。
    pub fn deadline_guard(&self, after: Duration) -> DeadlineGuard<'_> {
        let state = &self.shared.interrupt;
        // 快照旧值，Drop 时写回；因此可嵌套。
        let previous = state.deadline_ms.load(Ordering::Relaxed);
        let deadline = state
            .now_ms()
            .saturating_add(after.as_millis().min(u64::MAX as u128) as u64);
        state.deadline_ms.store(deadline, Ordering::Relaxed);
        DeadlineGuard {
            engine: self,
            previous,
        }
    }

    /// 外部中断（`/reload`、UI 的 Esc）：置位后下一次指令边界抛 `interrupted`。
    ///
    /// 标志不会自动清除；后续事务要继续可用需调用 [`super::JsEngine::clear_interrupt`]。
    /// 它只在 QuickJS 的指令边界生效：正在执行的 Rust/宿主代码不会被打断。常规用法是在
    /// 事务之外置位，影响下一次进入 JS 的执行；若在宿主函数里置位，则影响同一事务的
    /// 下一次指令边界检查。
    pub fn interrupt(&self) {
        self.shared
            .interrupt
            .interrupted
            .store(true, Ordering::Relaxed);
    }

    /// 清除外部中断标志。
    ///
    /// 只清 `interrupted`，不影响 deadline（那个要用
    /// [`super::JsEngine::clear_deadline`]）。
    pub fn clear_interrupt(&self) {
        self.shared
            .interrupt
            .interrupted
            .store(false, Ordering::Relaxed);
    }

    /// 外部中断标志是否置位。
    ///
    /// 只看外部标志，deadline 到期不算；组合判断（标志或到期）用
    /// [`super::JsEngine::deadline_expired`]。
    pub fn is_interrupted(&self) -> bool {
        self.shared.interrupt.interrupted.load(Ordering::Relaxed)
    }
}
