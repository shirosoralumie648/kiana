//! countdown-timer.ts。
//!
//! 对话框复用的倒计时器（秒粒度）。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/countdown-timer.ts`。
//!
//! # 定时器注入（与 spec 的 tokio 建议不同）
//!
//! TS 在构造函数里 `setInterval(..., 1000)`，并通过 `TUI.requestRender()` 请求重绘。Rust 侧沿用
//! kiana-tui `Loader`/`Editor` 的宿主定时器抽象 [`Timer`]（生产环境由宿主事件循环注入 tokio 定时器，
//! 测试注入可手动触发的替身），而不是在组件内部 spawn tokio 任务：
//!
//! * 构造后立即调用一次 `on_tick(ceil(timeout_ms / 1000))`（与 TS 相同，不依赖任何定时器）；
//! * 每次到期：`remaining -= 1; on_tick(remaining); request_render();`，然后若 `remaining <= 0`
//!   先 `dispose()` 再 `on_expire()`（**顺序**：先 tick、再 requestRender、再判断过期）；
//! * [`CountdownTimer::set_timer`] 安装定时器后才开始排程（默认 [`kiana_tui::tui::NoopTimer`] 语义：
//!   没有宿主定时器就不会自行触发，与 Loader 一致）；
//! * 回调里只持有 `Weak`，不会与定时器形成引用环；过期/取消用 generation 守卫迟到的回调。
//!
//! `dispose()` 幂等并取消未触发的回调；TS 没有调用 `timer.unref()`，这里同样没有额外语义。
//!
//! # 状态共享与迟到回调
//!
//! `Shared` 由公开句柄（`Rc`）与定时器回调（`Weak`）共享：回调升级失败说明组件已销毁。
//! `generation` 是“排程代次”，`dispose()` 时递增；已入队但尚未执行的回调在 `fire` 入口
//! 发现代次不符就放弃，避免取消后仍把倒计时拉起来。`on_tick`/`on_expire` 调用前先 `take()`
//! 出来、调用后放回，因此回调内部重入 `dispose()` 不会与 `RefCell` 的借用冲突。
//!
//! 回调在宿主调用定时器回调的当下**同步执行**（本组件不 spawn 任务）；`on_expire` 在
//! `dispose()` 之后调用，此时 `is_disposed()` 已为 `true`、定时器已取消，且不会再有新的到期。
//!
//! # 调用方
//!
//! `kiana-interactive` 的三个组件在用：
//!
//! * `extension_selector`：`timeout > 0.0` 时构造，`on_tick` 改写标题里的剩余秒数，
//!   `on_expire` 触发取消；`set_timer` 由宿主挂载组件时注入，`dispose` 在关闭对话框时调用。
//! * `extension_input`：同样只在 `timeout > 0.0` 时构造（同款守卫）。
//! * `status_indicator` 的 `RetryStatusIndicator`：用 `delay_ms` 构造，不校验正值。

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::tui::Timer;

/// `onTick` 回调（TS `(seconds: number) => void`）。
type TickCallback = Box<dyn FnMut(f64)>;

/// `onExpire` 回调（TS `() => void`）。
type ExpireCallback = Box<dyn FnMut()>;

/// 倒计时状态（定时器回调与公开句柄共享）。
///
/// 用 `Rc`/`Cell`/`RefCell` 而非 `Arc`/锁：所有访问都发生在创建组件的线程（TUI 线程），
/// 类型本身也不满足 `Send`。`remaining_seconds` 只在 `fire` 中递减（构造时写入初值），
/// 其余字段的含义见模块文档。
struct Shared {
    remaining_seconds: Cell<f64>,
    disposed: Cell<bool>,
    generation: Cell<u64>,
    timer: RefCell<Option<Box<dyn Timer>>>,
    request_render: RefCell<Option<RequestRenderCallback>>,
    on_tick: RefCell<Option<TickCallback>>,
    on_expire: RefCell<Option<ExpireCallback>>,
}

impl Shared {
    /// 幂等：先置 `disposed`（后续 `schedule` 直接返回），再递增 `generation` 让已排程的
    /// 回调失效，最后取消定时器。**不调用** `on_expire`——过期回调只在 `fire` 走到
    /// `remaining <= 0` 时触发。
    fn dispose(&self) {
        if self.disposed.get() {
            return;
        }
        self.disposed.set(true);
        self.generation.set(self.generation.get() + 1);
        if let Some(timer) = self.timer.borrow_mut().as_mut() {
            timer.cancel();
        }
    }
}

/// 排程下一次 1000ms 回调（TS `setInterval` 的重复语义）。
///
/// 已 dispose 或没有安装定时器时直接返回：`NoopTimer` 也不会保留回调，因此“没装定时器
/// 就不会自行触发”。回调只捕获 `Weak` 与当前代次，不延长 `Shared` 的生命周期。
fn schedule(shared: &Rc<Shared>) {
    if shared.disposed.get() {
        return;
    }
    let generation = shared.generation.get();
    if shared.timer.borrow().is_none() {
        return;
    }
    let weak = Rc::downgrade(shared);
    if let Some(timer) = shared.timer.borrow_mut().as_mut() {
        timer.set(
            1000,
            Box::new(move || {
                fire(weak, generation);
            }),
        );
    }
}

/// 一次到期（TS 的 `setInterval` 回调体）。
///
/// 固定顺序：递减剩余秒数 → `on_tick` → `request_render` → 若 `remaining <= 0` 则
/// `dispose()` 后 `on_expire()`，否则重新排程。测试
/// `ticks_each_second_and_expires_at_zero` 锁定了 `tick → render → expire` 这个尾部顺序。
/// `on_tick`/`on_expire` 都是 `FnMut`，调用期间对应槽位被 `take()` 成 `None`，回调若间接
/// 再次进入 `fire`，看到的是空槽位而不是已借用的 `RefCell`。
fn fire(weak: Weak<Shared>, generation: u64) {
    let Some(shared) = weak.upgrade() else {
        return;
    };
    if shared.disposed.get() || shared.generation.get() != generation {
        return;
    }

    shared
        .remaining_seconds
        .set(shared.remaining_seconds.get() - 1.0);
    let remaining = shared.remaining_seconds.get();

    // onTick：临时取出回调，避免外部回调重入 `dispose()` 时与 RefCell 借用冲突。
    let tick = shared.on_tick.borrow_mut().take();
    if let Some(mut tick) = tick {
        tick(remaining);
        *shared.on_tick.borrow_mut() = Some(tick);
    }

    let request_render = shared.request_render.borrow().clone();
    if let Some(request_render) = request_render {
        request_render();
    }

    if remaining <= 0.0 {
        shared.dispose();
        let expire = shared.on_expire.borrow_mut().take();
        if let Some(mut expire) = expire {
            expire();
            *shared.on_expire.borrow_mut() = Some(expire);
        }
    } else {
        schedule(&shared);
    }
}

/// 可在对话框里复用的倒计时器（TS `class CountdownTimer`）。
pub struct CountdownTimer {
    shared: Rc<Shared>,
}

impl CountdownTimer {
    /// TS `constructor(timeoutMs, tui, onTick, onExpire)`。
    ///
    /// 立即调用 `on_tick(ceil(timeout_ms / 1000))`；之后每 1000ms 递减一次。
    ///
    /// `timeout_ms` 是 `f64`（对齐 TS 的 `number`），`ceil` 对负数/零不特殊处理：传 0 会先
    /// tick `0`，下一次到期得到 `-1` 并立即过期；传负数同理。本函数不做正值校验；
    /// `extension_selector` 与 `extension_input` 都只在 `timeout > 0.0` 时构造，而
    /// `status_indicator` 的 `RetryStatusIndicator` 直接把 `delay_ms` 传进来（不校验）。
    pub fn new(
        timeout_ms: f64,
        request_render: Option<RequestRenderCallback>,
        on_tick: TickCallback,
        on_expire: ExpireCallback,
    ) -> Self {
        let shared = Rc::new(Shared {
            remaining_seconds: Cell::new((timeout_ms / 1000.0).ceil()),
            disposed: Cell::new(false),
            generation: Cell::new(0),
            timer: RefCell::new(None),
            request_render: RefCell::new(request_render),
            on_tick: RefCell::new(Some(on_tick)),
            on_expire: RefCell::new(Some(on_expire)),
        });
        // TS 在构造时先赋值回调、`remainingSeconds = Math.ceil(...)`，再立即 `onTick(...)`。
        let remaining = shared.remaining_seconds.get();
        let tick = shared.on_tick.borrow_mut().take();
        if let Some(mut tick) = tick {
            tick(remaining);
            *shared.on_tick.borrow_mut() = Some(tick);
        }
        CountdownTimer { shared }
    }

    /// 安装/替换宿主定时器（TUI 适配：TS 的 `setInterval` 由宿主定时器替代）。
    ///
    /// 替换时会取消旧定时器；未过期时立即按 1000ms 排程。
    ///
    /// 已 `dispose()` 时只保存新定时器、不排程——dispose 是终态，之后不会再有任何 tick
    /// （测试 `works_without_any_timer` 锁定）。
    pub fn set_timer(&mut self, timer: Box<dyn Timer>) {
        let old = self.shared.timer.borrow_mut().replace(timer);
        if let Some(mut old) = old {
            old.cancel();
        }
        if !self.shared.disposed.get() {
            schedule(&self.shared);
        }
    }

    /// 当前剩余秒数（TS 私有字段；用于测试与宿主查询）。
    ///
    /// 构造后即为 `ceil(timeout_ms / 1000)`，之后只在定时器回调里递减；没有安装定时器时
    /// 不会自行变化。可能为负（最后一次到期把 `0` 减到 `-1` 也照样回调）。
    pub fn remaining_seconds(&self) -> f64 {
        self.shared.remaining_seconds.get()
    }

    /// 是否已 dispose。
    pub fn is_disposed(&self) -> bool {
        self.shared.disposed.get()
    }

    /// TS `dispose()`：取消定时器；幂等。
    ///
    /// 只取消定时器，不触发 `on_expire`；`disposed` 一旦置位不会复位，之后 `set_timer`
    /// 也不会重新开始倒计时。
    pub fn dispose(&mut self) {
        self.shared.dispose();
    }
}
