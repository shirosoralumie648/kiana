//! 定时器堆与 `queueMicrotask`（§2.5）。
//!
//! TS 对应物：Node 的 `setTimeout`/`setInterval`/`setImmediate`/`process.nextTick`
//! 与 `queueMicrotask`。设计文档 §2.5、§10.4 的第 4 条回归。
//!
//! 分工：
//!
//! - **JS 侧**（`js/prelude_core.js`）持有 `Map<id, {fn, args, repeat}>`，负责回调、
//!   `clearTimeout` 的语义，以及 `setImmediate`/`process.nextTick` → `queueMicrotask`。
//! - **Rust 侧**（本文件）只持有最小堆 `(due, id)`，`timerStart`/`timerCancel` 由 JS 调用；
//!   主循环用 [`JsEngine::next_timer_deadline`] + `tokio::time::sleep_until` 等待，
//!   到点调用 [`JsEngine::fire_due_timers`]（内部 `__kiana.timerFire(id)`）。主循环的
//!   调用点是 `runner::dispatch` 的 `await_promise` 与 `loader` 的
//!   `await_js_promise` / `await_js_promise_value`（原始值版本，同一推进循环）。
//!
//! **`setTimeout` 必须真的等 delay**（§2.5 的事故记录：`@anthropic-ai/sdk` 的
//! `fetchWithTimeout` 用 `setTimeout(() => controller.abort(), ms)`，立即触发会把每个请求
//! abort 成 `APIUserAbortError("Request was aborted.")`）。`tests/engine_timers.rs` 有回归。
//!
//! `setInterval` 不要求无漂移，但同一 id 的上一拍未完成时不得堆积：重复定时器只在
//! `__kiana.timerFire` 返回 `true`（JS 侧仍持有该 id 且为 interval）后才重新入堆。
//! 每个到点的定时器各占一次 `with_js` 事务：微任务在回调之间泵空（Node 语义），
//! 100 ms 看门狗也只针对当次回调；被看门狗打断的回调记为 `JsErrorKind::Timer`，
//! interval 继续存活（见 [`JsEngine::fire_due_timers`]）。
//!
//! # 数据结构与不变量
//!
//! `TimerQueue` = 最小堆 `BinaryHeap<Reverse<HeapItem>>`（元素是 `(due, id)`）+
//! 活动表 `HashMap<id, TimerEntry>`。
//!
//! - `active` 是“某个 id 现在应当在何时触发”的唯一真相；堆里同 id 的旧项通过与
//!   `active` 的 `due` 比较判定为陈旧，并在 `next_deadline` / `pop_due` 里**惰性丢弃**。
//!   所以 `cancel` 只做一次 `HashMap::remove`，代价与堆的大小无关；代价是堆里可能暂时
//!   保留已取消/已重排的项，直到它们浮到堆顶。
//! - `HeapItem` 派生的 `Ord` 先比 `due` 再比 `id`，`Reverse` 把整体顺序反过来，所以触发
//!   顺序是 `due` 升序、同一时刻按 `id` 升序（测试里“到期顺序”的断言依赖这一点）。
//! - Rust 侧**不持有任何 JS 值**：回调、参数、`Timeout` 句柄都留在 JS 的 `Map` 里。
//!   因此 `clear()` 与引擎 `Drop` 不需要处理 `Persistent`。
//!
//! # 注意事项
//!
//! - [`JsEngine::next_timer_deadline`] 会弹出陈旧堆项，所以内部需要 `&mut TimerQueue`
//!   （通过 `RefCell` 取得）。
//! - [`JsEngine::fire_due_timers`] 只处理**进入调用时**已到期的快照；回调里新排的定时器
//!   要等主循环下一轮。
//! - 定时器堆用 `tokio::time::Instant`（可被 `start_paused` 冻结），与看门狗使用的
//!   `std::time::Instant` 不是同一个时间轴（见 `engine/limits.rs` 的模块文档）。

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::rc::Rc;
use std::time::Duration;

use rquickjs::{Function, Object, Persistent};
use tokio::time::Instant;

use super::promise::PendingJsError;
use super::{EngineResult, JsEngine, JsErrorKind, pump_jobs_with};

/// Node 的 `TIMEOUT_MAX`（`2^31 - 1` ms）。JS 侧已钳制，这里再兜一层。
const TIMEOUT_MAX_MS: f64 = 2_147_483_647.0;

/// 定时器堆（最小堆 + id → 活动项）。
///
/// 两个容器必须一起维护：`schedule` 同时写入两者，`cancel` 只删 `active`（堆项惰性回收），
/// `finish` 决定重排还是删除。除 `clear` 外不要单独操作其中一个。
#[derive(Debug, Default)]
pub(crate) struct TimerQueue {
    heap: BinaryHeap<Reverse<HeapItem>>,
    active: HashMap<u64, TimerEntry>,
}

/// 堆项：到期时刻 + 定时器 id。`id` 参与比较，使相同 `due` 的触发顺序确定（id 升序）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct HeapItem {
    due: Instant,
    id: u64,
}

/// 活动定时器的当前排程。`delay_ms` 保留原始（已归一化的）毫秒数，供 interval 重排复用。
#[derive(Clone, Copy, Debug)]
struct TimerEntry {
    due: Instant,
    delay_ms: f64,
    repeat: bool,
}

/// 毫秒 → `Duration`：非有限值（NaN/±inf）或 ≤ 0 视为 0，超过 `TIMEOUT_MAX_MS` 钳制。
///
/// JS 侧 `normalizeDelay` 已把 `setTimeout`/`setInterval` 的普通调用归一到
/// `[1, TIMEOUT_MAX]`；这里是宿主原语被直接调用时的第二道防线（见 `TIMEOUT_MAX_MS` 的说明）。
fn delay_duration(delay_ms: f64) -> Duration {
    if !delay_ms.is_finite() || delay_ms <= 0.0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(delay_ms.min(TIMEOUT_MAX_MS) / 1000.0)
}

impl TimerQueue {
    /// 入堆（JS 的 `setTimeout`/`setInterval`，以及 interval 的下一拍）。
    ///
    /// 同 id 再次调用表示重排（`Timeout.refresh()`、或 interval 的下一拍）：覆盖 `active`
    /// 里的条目并入堆。旧堆项不立即删除，等 `next_deadline` / `pop_due` 发现 `due` 不匹配
    /// 后丢弃。`delay_ms` 非有限值或 ≤ 0 按 0 处理、超过 `TIMEOUT_MAX_MS` 被钳制
    /// （见 `delay_duration`）；`due` 以调用时刻的 `Instant::now()` 为基准。
    pub(crate) fn schedule(&mut self, id: u64, delay_ms: f64, repeat: bool) {
        let due = Instant::now() + delay_duration(delay_ms);
        self.active.insert(
            id,
            TimerEntry {
                due,
                delay_ms,
                repeat,
            },
        );
        self.heap.push(Reverse(HeapItem { due, id }));
    }

    /// 真正移除（`clearTimeout`/`clearInterval`）。
    ///
    /// 只删活动表；堆里对应的项在下一次 `next_deadline` / `pop_due` 时被识别为陈旧并丢弃。
    /// 不存在的 id 是无操作。
    pub(crate) fn cancel(&mut self, id: u64) {
        self.active.remove(&id);
    }

    /// 最近一个**有效**定时器的到期时刻（惰性丢弃被取消/重排后的陈旧堆项）。
    ///
    /// 堆空（或只剩陈旧项）时返回 `None`。会修改堆，所以需要 `&mut self`。
    pub(crate) fn next_deadline(&mut self) -> Option<Instant> {
        loop {
            let Reverse(item) = self.heap.peek().copied()?;
            let live = self
                .active
                .get(&item.id)
                .is_some_and(|entry| entry.due == item.due);
            if live {
                return Some(item.due);
            }
            self.heap.pop();
        }
    }

    /// 取出所有 `due <= now` 的有效定时器（保持到期顺序），带出被弹出的到期时刻。
    ///
    /// 返回顺序是 `due` 升序、同一时刻按 `id` 升序（堆的 `Ord` 决定）。活动项仍留在
    /// `active` 里，等 JS 回调返回后由 [`TimerQueue::finish`] 决定删除还是一次性重排；
    /// 这样“前一个回调里 `clearTimeout` 掉同批的另一个定时器”仍能生效（与 Node 的
    /// timers 相位一致）。带出 `due` 还让“回调里 `timeout.refresh()` 了另一个同批定时器”
    /// 能被识别（active 的 due 已改变）。
    pub(crate) fn pop_due(&mut self, now: Instant) -> Vec<(u64, Instant)> {
        let mut due = Vec::new();
        while let Some(Reverse(item)) = self.heap.peek().copied() {
            if item.due > now {
                break;
            }
            self.heap.pop();
            let Some(entry) = self.active.get(&item.id).copied() else {
                continue;
            };
            if entry.due != item.due {
                continue;
            }
            due.push((item.id, item.due));
        }
        due
    }

    /// 该 id 当前的到期时刻（不存在 → `None`）。
    pub(crate) fn due_at(&self, id: u64) -> Option<Instant> {
        self.active.get(&id).map(|entry| entry.due)
    }

    /// JS 回调返回后的收尾：`keep_repeating` 为 `true` 时重排 interval，否则删除。
    ///
    /// `fired_due` 是本次弹出的到期时刻；若活动项的 due 已变（回调里调用了
    /// `Timeout.refresh()`），说明新排程已经在堆里，保持原样即可。
    ///
    /// 重排的下一拍以**回调结束后的** `Instant::now()` 为基准加 `delay_ms`（固定延迟，
    /// 落后时不补发多拍，所以 `setInterval` 会有漂移，这是设计允许的）。
    pub(crate) fn finish(&mut self, id: u64, fired_due: Instant, keep_repeating: bool) {
        let Some(entry) = self.active.get(&id).copied() else {
            return;
        };
        if entry.due != fired_due {
            return;
        }
        if entry.repeat && keep_repeating {
            let due = Instant::now() + delay_duration(entry.delay_ms);
            self.active.insert(id, TimerEntry { due, ..entry });
            self.heap.push(Reverse(HeapItem { due, id }));
        } else {
            self.active.remove(&id);
        }
    }

    /// 该 id 是否是重复定时器（`setInterval`）。
    pub(crate) fn is_repeat(&self, id: u64) -> bool {
        self.active.get(&id).is_some_and(|entry| entry.repeat)
    }

    /// 活动定时器数量（测试用；不含堆里的陈旧项）。
    pub(crate) fn len(&self) -> usize {
        self.active.len()
    }

    /// 清空（引擎 `Drop` 用；定时器项本身不含 `Persistent`，只避免跨代残留）。
    ///
    /// 与 `cancel` 不同，这里连堆一起清掉，不留下任何陈旧项。
    pub(crate) fn clear(&mut self) {
        self.heap.clear();
        self.active.clear();
    }
}

/// 安装 `__kiana_host.timerStart` / `__kiana_host.timerCancel`。
///
/// 前置条件：`__kiana_host` 对象已由 `JsEngine::install_host_primitives` 创建；本函数在
/// prelude 之前调用。JS 侧（`js/prelude_core.js` 的 `addTimer` / `Timeout.refresh` /
/// `clearTimer`）负责分配 id、保存回调；这两个宿主函数只改 Rust 侧堆，不运行 JS。
pub(crate) fn install_host_timers(engine: &JsEngine) -> EngineResult<()> {
    let shared = Rc::clone(&engine.shared);
    engine.with_js(|ctx| {
        let host: Object<'_> = ctx.globals().get("__kiana_host")?;

        let start_shared = Rc::clone(&shared);
        let start = Function::new(
            ctx.clone(),
            move |id: u64, delay_ms: f64, repeat: bool| -> EngineResult<()> {
                start_shared
                    .timers
                    .borrow_mut()
                    .schedule(id, delay_ms, repeat);
                Ok(())
            },
        )?;
        host.set("timerStart", start)?;

        let cancel = Function::new(ctx.clone(), move |id: u64| -> EngineResult<()> {
            shared.timers.borrow_mut().cancel(id);
            Ok(())
        })?;
        host.set("timerCancel", cancel)?;
        Ok(())
    })
}

impl JsEngine {
    /// 最近一个定时器的到期时刻；主循环用它 `tokio::time::sleep_until`。
    ///
    /// 没有活动定时器时返回 `None`（主循环据此改为只等完成队列）。内部会惰性清掉被
    /// 取消/重排的堆项，所以每次调用都可能修改 `TimerQueue`。
    pub fn next_timer_deadline(&self) -> Option<Instant> {
        self.shared.timers.borrow_mut().next_deadline()
    }

    /// 触发所有已到期定时器：每个定时器一次独立事务
    /// （`__kiana.timerFire(id)` → 收尾（重排/删除）→ 泵 job）。
    ///
    /// 返回本次处理的定时器数量。必须在主循环里、`with_js` 之外调用。
    ///
    /// 处理范围是**进入调用时**已到期的快照（`pop_due`）：回调里新排或重排的定时器不在
    /// 本批，等下一轮。被前面的回调 `clearTimeout` / `refresh` 掉的项会被跳过，且不计入
    /// 返回值。找不到 `__kiana` 或 `__kiana.timerFire`（prelude 未安装）时，该项目仍按
    /// “不重排”收尾：一次性定时器被移除，interval 会停表。
    ///
    /// **每个定时器一次 `with_js`** 是有意的（设计文档 §2.1/§2.5/§2.7）：
    ///
    /// - 微任务在每两个定时器之间泵空，与 Node 的“回调后立即清空微任务队列”一致；
    /// - 100 ms 看门狗按定时器计算：一个超时的回调不会连带杀死同批的其它定时器；
    /// - 超时被中断的回调把异常记进错误队列（`JsErrorKind::Timer`），不再静默丢失；
    /// - `setInterval` 的回调被看门狗打断时保持排程（Node 不会因为一次慢回调杀死
    ///   interval），只有一次性 `setTimeout` 会被移除。
    pub fn fire_due_timers(&self) -> usize {
        let due = {
            let mut queue = self.shared.timers.borrow_mut();
            queue.pop_due(Instant::now())
        };
        let mut fired = 0;
        for (id, fired_due) in due {
            // 同批的前一个回调可能已经 `clearTimeout(id)` 或 `refresh()` 了它
            // （Node 的 timers 相位语义）：due 变了就跳过，等新排程到点再触发。
            if self.shared.timers.borrow().due_at(id) != Some(fired_due) {
                continue;
            }
            let repeat = self.shared.timers.borrow().is_repeat(id);
            // keep 由 JS 的 `timerFire` 返回值决定：true 仅当它是 interval 且 JS 侧仍持有
            // 该 id（回调里没 `clearInterval`）。默认 false → 按一次性定时器收尾。
            let mut keep = false;
            self.with_js(|ctx| {
                // prelude 未安装（或 `__kiana` 被删）时提前返回，keep 保持 false。
                let Ok(kiana) = ctx.globals().get::<_, Object>("__kiana") else {
                    let _ = ctx.catch();
                    return;
                };
                let Ok(timer_fire) = kiana.get::<_, Function>("timerFire") else {
                    let _ = ctx.catch();
                    return;
                };
                match timer_fire.call::<_, bool>((id,)) {
                    Ok(value) => keep = value,
                    Err(_) => {
                        // `__kiana.timerFire` 里的 try/catch 兜不住中断（QuickJS 的
                        // `interrupted` 是 uncatchable），异常由 Rust 收下并映射成扩展错误。
                        let value = ctx.catch();
                        self.shared.errors.borrow_mut().push(PendingJsError {
                            kind: JsErrorKind::Timer,
                            promise: None,
                            reason: Persistent::save(&ctx, value),
                        });
                        // 看门狗中断的不是 JS 侧的 `clearInterval`：重复定时器继续存活。
                        keep = repeat;
                    }
                }
                // 每个回调之后立刻泵微任务（Node：回调 → 微任务 → 下一个定时器）。
                pump_jobs_with(&ctx);
            });
            // 收尾在 `with_js` 之外：interval 的下一拍以回调真正结束的时刻为基准，同时避免
            // 在 JS 事务里借用 `timers` 的 `RefCell`。
            self.shared.timers.borrow_mut().finish(id, fired_due, keep);
            fired += 1;
        }
        fired
    }

    /// 活动定时器数量（泄漏测试用；`setTimeout` 返回后应在堆里，`clearTimeout` 后消失）。
    pub fn timer_count(&self) -> usize {
        self.shared.timers.borrow().len()
    }
}
