//! components/loader.ts。
//!
//! 带动画指示器的加载组件：继承自 [`Text`]（见"表示差异"），`render` 总是在最前面加一个
//! 空行——下游（coding-agent 的 `StatusIndicator`）依赖"第二行是内容行"这一实现细节。
//!
//! ## 与 TS 的表示差异
//!
//! 1. **定时器**：TS 用 `setInterval` 每 `intervalMs` 前进一帧并 `ui.requestRender()`。
//!    Rust 的组件在单线程 `Rc` 世界里不能跨线程持有状态，这里改为**按经过时间惰性计算帧**
//!    （`started_at.elapsed() / interval`）：任何一次渲染看到的帧与 TS 定时器到同一时刻的
//!    帧相同。动画的**节拍**由宿主事件循环的定时器驱动：[`Loader::set_timer`] 接收
//!    [`crate::tui::Timer`]（与 `TuiBase::set_timer` 同一抽象；测试可用手动触发的替身），
//!    每次到期调用 `requestRender` 并重新排程，形成周期性节拍。未安装定时器时
//!    （`timer` 为 `None`，行为等价于 [`crate::tui::NoopTimer`]）与 TS 一样只在状态变化时
//!    请求重绘——只是不会自行推进动画节拍，因此任何一次渲染都会先把文本同步到"当前时刻
//!    的帧"（见 `Loader::sync_display`）。
//! 2. **UI 引用**：TS 保存 `TUI` 并调用 `ui.requestRender()`；Rust 接收一个
//!    [`RequestRenderCallback`]（`Rc<dyn Fn()>`），避免对 TUI 的可变借用重入。
//! 3. [`Loader::get_rendered_indicator`] 是 `pub`（TS 里是 `protected`，供子类读取）。
//!
//! # 状态与相位
//!
//! 动画帧不是"每次定时器到期就 +1"，而是按经过时间计算：
//! `frame = (current_frame + elapsed / interval) % frames.len()`。其中 `current_frame` 是
//! 本次动画开始时的基准帧，`started_at` 是相位起点（私有的 `frame_value_at` 实现了这个
//! 公式）。各入口对相位的处理：
//!
//! - `set_indicator()`：先把相位清零（`current_frame = 0`、`started_at = None`）再 `start()`，
//!   保证从第 0 帧重新开始；
//! - `start()`：先把显示同步到"此刻"，再从当前基准帧开新相位；
//! - `stop()`：把此刻的帧冻结进 `current_frame` 并清掉相位起点，因此再次 `start()` 不会跳帧；
//! - `frames.len() <= 1`：`restart_animation()` 直接返回，不建立相位、也不排程定时器。
//!
//! 没有相位起点（`started_at == None`）时所有计算都返回冻结的 `current_frame`；`frames`
//! 为空或 `interval_ms <= 0` 时也直接返回它，避免除零/取模零。
//!
//! # 定时器与生命周期
//!
//! [`Loader::set_timer`] 把宿主的 [`crate::tui::Timer`] 包进私有的 `AnimationClock`。到期
//! 回调只做两件事：调用 `request_render`，然后按同一 interval 重新排程；**帧本身不由定时器
//! 推进**。`running` 标志与 `generation` 计数让"已排程但已过期"的回调变成 no-op：`stop()`
//! 会 `cancel()` 并递增 `generation`，旧回调恢复执行时发现代次不符就直接返回。回调只持有
//! `Weak`，宿主即使长期保存回调也不会与 `AnimationClock` 形成引用环；`Loader` 的 `Drop`
//! 会停止定时器。替换定时器时旧的 `AnimationClock` 被丢弃，其已排程回调因 `Weak` 升级失败
//! 而不会请求重绘（旧 `Timer` 不会被显式 `cancel`，随 `AnimationClock` 一起丢弃）。
//!
//! # 注意事项
//!
//! - [`LoaderIndicatorOptions`] 只要以 `Some` 传入（哪怕是空结构），帧就按"原样"渲染，
//!   不再经过 `spinner_color_fn`（TS 的 `renderIndicatorVerbatim`）；
//!   `frames: Some(vec![])` 表示隐藏指示器，只显示消息。
//! - `render()` 的第一行恒为空行，内容从第二行开始——这是下游 `StatusIndicator` 依赖的
//!   行号契约，不要"顺手"去掉。
//! - `render()` 与 `tick()` 都不请求重绘；由组件自身状态变化触发的重绘统一走
//!   `update_display()`，定时器到期则由 `AnimationClock` 直接调用同一个回调。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use crate::components::text_component::{StyleFn, Text};
use crate::tui::{Component, Timer};

/// 动画帧（TS `DEFAULT_FRAMES`，**【逐字】**）。
pub const DEFAULT_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// 默认帧间隔（TS `DEFAULT_INTERVAL_MS`）。
///
/// 保留 `f64` 以对齐 TS 的 `number`；真正排程到 [`crate::tui::Timer`] 前会经
/// `timer_interval_ms` 归一为整数毫秒。
pub const DEFAULT_INTERVAL_MS: f64 = 80.0;
/// 默认消息（TS `"Loading..."`）。
pub const DEFAULT_MESSAGE: &str = "Loading...";

/// 请求重绘回调（TS 的 `ui.requestRender()`）。
///
/// 用 `Rc<dyn Fn()>` 而非 `Arc`：TUI 与所有组件都在同一个线程上，回调也只需在该线程调用。
/// 组件只负责调用它，不关心宿主如何节流或调度。
pub type RequestRenderCallback = Rc<dyn Fn()>;

/// 指示器选项（TS `LoaderIndicatorOptions`）。
///
/// `None` 与 `Some(...)` 不是"有没有指示器"的区别，而是**帧文本是否原样输出**的区别：
/// 传入 `Some` 时 `render_indicator_verbatim` 为 true，帧不再经过 `spinner_color_fn`
/// 着色；`frames: Some(vec![])` 才是隐藏指示器。字段缺省时分别回退到
/// [`DEFAULT_FRAMES`] 与 [`DEFAULT_INTERVAL_MS`]。
#[derive(Clone, Default)]
pub struct LoaderIndicatorOptions {
    /// 动画帧；空数组表示隐藏指示器。
    pub frames: Option<Vec<String>>,
    /// 动画帧间隔（毫秒）。
    pub interval_ms: Option<f64>,
}

/// 动画节拍器（TS `restartAnimation()` 里的 `setInterval`）。
///
/// 每次到期：调用 `requestRender`，然后按 `intervalMs` 重新排程（TS 的 `setInterval`
/// 会一直重复）。帧本身不在这里推进——[`Loader::get_rendered_indicator`] 按经过时间
/// 惰性计算，因此定时器只负责"让宿主在正确的时间重绘"。
///
/// 回调里持有 [`Weak`](std::rc::Weak)，避免 `Timer`（宿主可能长期持有回调）与
/// `AnimationClock` 形成引用环。
struct AnimationClock {
    timer: RefCell<Box<dyn Timer>>,
    /// 归一后的整数毫秒；`set_indicator` 会就地更新它，下一次重新排程即生效。
    interval_ms: Cell<u64>,
    /// 与 [`Loader`] 共享的重绘回调；`set_request_render` 会同步替换这里的副本。
    request_render: RefCell<Option<RequestRenderCallback>>,
    /// 是否还有效：`stop()` 置 false，已排程的回调据此放弃重新排程。
    running: Cell<bool>,
    /// 排程代次。每次 `stop()` 递增；回调只在自己捕获的代次仍是最新时才执行，
    /// 防止"取消后仍已入队"的旧回调把动画重新拉起来。
    generation: Cell<u64>,
}

impl AnimationClock {
    fn new(
        timer: Box<dyn Timer>,
        interval_ms: u64,
        request_render: Option<RequestRenderCallback>,
    ) -> Self {
        AnimationClock {
            timer: RefCell::new(timer),
            interval_ms: Cell::new(interval_ms),
            request_render: RefCell::new(request_render),
            running: Cell::new(false),
            generation: Cell::new(0),
        }
    }

    fn schedule(self: &Rc<Self>, generation: u64) {
        // 停止后（或被新一轮 start 取代后）不再排程。
        if !self.running.get() || self.generation.get() != generation {
            return;
        }
        // 回调只捕获 Weak：否则 Timer（宿主可能长期持有回调）会与 AnimationClock 互保。
        let this = Rc::downgrade(self);
        let delay_ms = self.interval_ms.get();
        self.timer.borrow_mut().set(
            delay_ms,
            Box::new(move || {
                let Some(this) = this.upgrade() else { return };
                // 升级成功不代表仍然有效：这期间可能已经 stop() 或重新 start()。
                if !this.running.get() || this.generation.get() != generation {
                    return;
                }
                let callback = this.request_render.borrow().clone();
                if let Some(callback) = callback {
                    callback();
                }
                // 到期后重新排程，模拟 setInterval 的重复触发；回调里若 stop()/start()，
                // 上面的 running/generation 检查会让本次重新排程自然失效。
                this.schedule(generation);
            }),
        );
    }

    fn start(self: &Rc<Self>) {
        // stop() 递增 generation：已入队的旧回调触发时会因代次不符而失效。
        self.stop();
        self.running.set(true);
        let generation = self.generation.get();
        self.schedule(generation);
    }

    fn stop(&self) {
        self.running.set(false);
        self.generation.set(self.generation.get() + 1);
        self.timer.borrow_mut().cancel();
    }
}

/// JS 定时器把 delay 归一为正整数毫秒（< 1 视为 1）。
fn timer_interval_ms(interval_ms: f64) -> u64 {
    if !interval_ms.is_finite() || interval_ms < 1.0 {
        1
    } else {
        interval_ms.floor() as u64
    }
}

/// 加载组件（TS `class Loader extends Text`）。
///
/// 对外状态由三部分组成：消息（`message`）、帧表（`frames`，空表表示隐藏指示器）与相位
/// （`started_at` + `current_frame`）。显示文本是 `"<帧> <消息>"`；帧为空串时没有前导空格。
/// 渲染输出是这一行前面再加一个空行，见 [`Component::render`] 的实现。
///
/// 内部持有 `Rc`，因此不是 `Send`/`Sync`，只能在 TUI 所在线程使用。
pub struct Loader {
    text: Text,
    frames: Vec<String>,
    interval_ms: f64,
    current_frame: usize,
    started_at: Option<Instant>,
    request_render: Option<RequestRenderCallback>,
    timer: Option<Rc<AnimationClock>>,
    render_indicator_verbatim: bool,
    spinner_color_fn: StyleFn,
    message_color_fn: StyleFn,
    message: String,
}

impl Loader {
    /// `constructor(ui, spinnerColorFn, messageColorFn, message = "Loading...", indicator?)`。
    ///
    /// 构造末尾会调用 `set_indicator(indicator)`：确定帧表与间隔、把相位归零并 `start()`
    /// （帧数大于 1 时动画才真正开始推进），同时经由 `update_display()` 请求一次重绘
    /// （若已提供 `request_render`）。`message` 是必填参数；需要 TS 的默认值 `"Loading..."`
    /// 时可传 [`DEFAULT_MESSAGE`]，但它不会自动生效。
    pub fn new(
        request_render: Option<RequestRenderCallback>,
        spinner_color_fn: StyleFn,
        message_color_fn: StyleFn,
        message: &str,
        indicator: Option<LoaderIndicatorOptions>,
    ) -> Self {
        let mut loader = Loader {
            text: Text::with_padding("", 1, 0),
            frames: DEFAULT_FRAMES
                .iter()
                .map(|frame| (*frame).to_string())
                .collect(),
            interval_ms: DEFAULT_INTERVAL_MS,
            current_frame: 0,
            started_at: None,
            request_render,
            timer: None,
            render_indicator_verbatim: false,
            spinner_color_fn,
            message_color_fn,
            message: message.to_string(),
        };
        loader.set_indicator(indicator);
        loader
    }

    /// 安装/替换重绘回调。
    ///
    /// 若已安装 `AnimationClock`，会同步替换其中的副本，否则定时器到期仍会调用旧回调。
    pub fn set_request_render(&mut self, request_render: Option<RequestRenderCallback>) {
        self.request_render = request_render.clone();
        if let Some(timer) = &self.timer {
            *timer.request_render.borrow_mut() = request_render;
        }
    }

    /// 安装动画节拍器（宿主事件循环的定时器，见模块文档第 1 条）。
    ///
    /// TS 的 `setInterval` 由构造期的 `TUI` 提供；Rust 侧改为显式注入
    /// [`Timer`]（与 `TuiBase::set_timer` 同一抽象）。测试用可手动触发的替身，
    /// 生产用 tokio 定时器。安装时若动画正在运行则立即开始排程。
    ///
    /// 重复调用会替换旧节拍器：旧 `AnimationClock` 被丢弃后，它已排程的回调因只持有
    /// `Weak` 而不会请求重绘；但旧 [`Timer`] 不会被显式 `cancel`（随 `AnimationClock` 一起
    /// 被丢弃），其未触发的回调是否清理取决于 `Timer` 实现。
    pub fn set_timer(&mut self, timer: Box<dyn Timer>) {
        let clock = Rc::new(AnimationClock::new(
            timer,
            timer_interval_ms(self.interval_ms),
            self.request_render.clone(),
        ));
        self.timer = Some(Rc::clone(&clock));
        if self.started_at.is_some() && self.frames.len() > 1 {
            clock.start();
        }
    }

    /// 当前消息（不含帧与分隔空格）。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 底层 [`Text`]（渲染缓存与留白）。
    ///
    /// 返回的是 `Text` 当前持有的内容；`render`、`invalidate`、`set_message`、`start` 都会
    /// 先把它同步到当时时刻的帧。文本留白按 TS 的 `super("", 1, 0)` 设置：左右 1 列、
    /// 上下 0 行。
    pub fn text(&self) -> &Text {
        &self.text
    }

    /// `start()`：刷新显示并开始动画。
    ///
    /// 先同步文本（可能请求重绘），再由 `restart_animation()` 决定是否建立新相位：
    /// 只有帧数大于 1 时才设置 `started_at` 并启动定时器。
    pub fn start(&mut self) {
        self.update_display();
        self.restart_animation();
    }

    /// `stop()`：停止动画（保留当前帧；TS 只清 interval，惰性实现把此刻的帧冻结下来）。
    ///
    /// 冻结动作是 `current_frame = 当前时刻的帧` 后清掉 `started_at`，所以停止后无论过多久，
    /// 渲染出来的都是停止那一刻的帧；再次 `start()` 会以它为基准继续。
    pub fn stop(&mut self) {
        self.current_frame = self.current_frame_value();
        self.started_at = None;
        if let Some(timer) = &self.timer {
            timer.stop();
        }
    }

    /// `setMessage(message)`。
    ///
    /// 立即把新消息同步进 [`Text`] 并请求一次重绘（经 `update_display()`）；动画相位不变。
    pub fn set_message(&mut self, message: &str) {
        self.message = message.to_string();
        self.update_display();
    }

    /// `setPaddingX(paddingX)`（TS `Loader extends Text`，因此留白影响渲染输出）。
    ///
    /// 只修改 [`Text`] 的留白并失效其缓存，不请求重绘；效果在下一次渲染时可见。
    pub fn set_padding_x(&mut self, padding_x: usize) {
        self.text.set_padding_x(padding_x);
    }

    /// `setIndicator(indicator)`。
    ///
    /// 规则（当前稳定契约，改动需同步测试）：
    /// - `None`：使用 [`DEFAULT_FRAMES`]，帧经 `spinner_color_fn` 着色；
    /// - `Some(...)`：`render_indicator_verbatim = true`，帧**原样**输出（不着色）；
    ///   `frames` 缺省时仍用 [`DEFAULT_FRAMES`]，`Some(vec![])` 则隐藏指示器；
    /// - `interval_ms` 只有在 `> 0` 时采用；0、负数与 `NaN` 回退到 [`DEFAULT_INTERVAL_MS`]。
    ///
    /// 结尾会重置帧相位并 `start()`；若已提供 `request_render`，`start()` 会经由
    /// `update_display()` 请求一次重绘，未提供时不会请求任何重绘。
    pub fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>) {
        // TS 的 `setIndicator` 先 `currentFrame = 0` 再 `start()`（`start()` 里的 `stop()`
        // 只清 interval，不动 `currentFrame`）。惰性实现必须在这里丢掉上一段动画的基准时刻，
        // 否则 `start()` 里的 `updateDisplay()` 会用旧相位覆盖刚重置的 0。
        self.started_at = None;
        self.render_indicator_verbatim = indicator.is_some();
        let frames = indicator
            .as_ref()
            .and_then(|options| options.frames.clone());
        self.frames = match frames {
            Some(frames) => frames,
            None => DEFAULT_FRAMES
                .iter()
                .map(|frame| (*frame).to_string())
                .collect(),
        };
        let interval = indicator
            .as_ref()
            .and_then(|options| options.interval_ms)
            .filter(|interval| *interval > 0.0);
        self.interval_ms = interval.unwrap_or(DEFAULT_INTERVAL_MS);
        self.current_frame = 0;
        if let Some(timer) = &self.timer {
            timer.interval_ms.set(timer_interval_ms(self.interval_ms));
        }
        self.start();
    }

    fn restart_animation(&mut self) {
        self.stop();
        if self.frames.len() <= 1 {
            return;
        }
        self.started_at = Some(Instant::now());
        if let Some(timer) = &self.timer {
            timer.start();
        }
    }

    /// 按经过时间求当前帧下标（TS 的 `currentFrame` 字段）。
    fn current_frame_value(&self) -> usize {
        self.frame_value_at(Instant::now())
    }

    /// 在给定时刻的帧下标。
    ///
    /// 没有相位起点时返回冻结的 `current_frame`。`frames` 为空（隐藏指示器的合法配置）
    /// 或间隔非正时同样提前返回，避免取模零。
    fn frame_value_at(&self, now: Instant) -> usize {
        let Some(started_at) = self.started_at else {
            return self.current_frame;
        };
        if self.frames.is_empty() || self.interval_ms <= 0.0 {
            return self.current_frame;
        }
        // `saturating_duration_since`：早于 `started_at` 的 `now` 得到 0，而不是 panic 或回绕。
        let elapsed_ms = now.saturating_duration_since(started_at).as_secs_f64() * 1000.0;
        // `current_frame` 是本次动画开始时的帧（TS 重启动画时保留 `currentFrame`）。
        (self.current_frame + (elapsed_ms / self.interval_ms).floor() as usize) % self.frames.len()
    }

    /// `getRenderedIndicator()`（TS 里是 `protected`）。
    ///
    /// 返回**当前时刻**的帧文本，不含消息与分隔空格。`render_indicator_verbatim` 为 false
    /// 时已应用 `spinner_color_fn`；`frames` 为空时取不到帧，按空串处理（空串是否会再被
    /// 着色取决于 `render_indicator_verbatim`）。
    pub fn get_rendered_indicator(&self) -> String {
        self.get_rendered_indicator_at(Instant::now())
    }

    fn get_rendered_indicator_at(&self, now: Instant) -> String {
        let frame = self
            .frames
            .get(self.frame_value_at(now))
            .cloned()
            .unwrap_or_default();
        if self.render_indicator_verbatim {
            frame
        } else {
            (self.spinner_color_fn)(&frame)
        }
    }

    /// 宿主渲染循环的节拍回调：给定时刻是否需要重绘（动画已推进到另一帧）。
    ///
    /// 对应 `spec/tui-components.md` 的定时器建议①（暴露 `tick(now) -> bool`，由外部
    /// 渲染循环调用），供没有定时器堆的宿主驱动动画；与 [`Loader::set_timer`] 等价。
    ///
    /// 实现只比较"此刻应显示的文本"与 [`Text`] 里缓存的文本，不修改任何状态（`&mut` 只为
    /// 与组件方法签名一致）。渲染过一次后同一帧内再调用会返回 false，直到帧推进。
    pub fn tick(&mut self, now: Instant) -> bool {
        self.display_text_at(now) != self.text.text()
    }

    /// 当前时刻应显示的文本（TS `updateDisplay()` 里拼接的那一段，不含 requestRender）。
    fn display_text(&self) -> String {
        self.display_text_at(Instant::now())
    }

    fn display_text_at(&self, now: Instant) -> String {
        let rendered_frame = self.get_rendered_indicator_at(now);
        let indicator = if rendered_frame.is_empty() {
            String::new()
        } else {
            format!("{rendered_frame} ")
        };
        let message = (self.message_color_fn)(&self.message);
        format!("{indicator}{message}")
    }

    /// 把 [`Text`] 的内容同步到"此刻"的动画帧。
    ///
    /// TS 由 `setInterval` 在两次渲染之间调用 `setText`；Rust 侧没有自动定时器时，
    /// [`Component::render`] 会先调用这里，保证任何一次渲染看到的帧与 TS 定时器
    /// 到同一时刻的帧一致（参见模块文档第 1 条）。不请求重绘，避免渲染期重入。
    fn sync_display(&mut self) {
        let text = self.display_text();
        if self.text.text() != text.as_str() {
            self.text.set_text(text);
        }
    }

    /// 同步显示并请求一次重绘（TS `updateDisplay()`）。
    ///
    /// 与 `sync_display` 的区别就在这里：`sync_display` 供渲染路径使用（不请求重绘，避免
    /// 渲染期重入），`update_display` 供状态变更路径使用。
    fn update_display(&mut self) {
        self.sync_display();
        if let Some(request_render) = &self.request_render {
            request_render();
        }
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        // 取消排程；即使宿主的 Timer 把回调保存在别处，回调也只持有 Weak，升级失败后不会
        // 访问已销毁的组件。
        if let Some(timer) = &self.timer {
            timer.stop();
        }
    }
}

impl Component for Loader {
    fn render(&mut self, width: usize) -> Vec<String> {
        // TS 的定时器在渲染之间改写 `Text`；惰性实现里先同步到当前时刻的帧，
        // 再走 `Text` 的换行/留白/缓存逻辑。与 TS 的 `render()` 一样不请求重绘。
        self.sync_display();
        let mut lines = vec![String::new()];
        lines.extend(self.text.render(width));
        lines
    }

    fn handles_input(&self) -> bool {
        // TS 的 `Loader` 没有 `handleInput`。
        false
    }

    fn invalidate(&mut self) {
        // 先丢掉 Text 的渲染缓存，再按当前相位重写文本并请求重绘（对应 TS 的
        // `super.invalidate()` + `updateDisplay()`）。
        self.text.invalidate();
        self.update_display();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Arc;

    use super::*;
    use crate::text::visible_width;

    fn identity() -> StyleFn {
        Arc::new(|text: &str| text.to_string())
    }

    #[test]
    fn prepends_a_blank_line_and_renders_the_default_message() {
        let mut loader = Loader::new(None, identity(), identity(), DEFAULT_MESSAGE, None);
        let lines = loader.render(20);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "");
        assert!(lines[1].trim_start().starts_with("⠋ Loading..."));
        assert_eq!(visible_width(&lines[1]), 20);
        assert!(!loader.handles_input());
    }

    #[test]
    fn hides_the_indicator_when_frames_are_empty() {
        let mut loader = Loader::new(
            None,
            identity(),
            identity(),
            "Working...",
            Some(LoaderIndicatorOptions {
                frames: Some(Vec::new()),
                interval_ms: None,
            }),
        );
        let lines = loader.render(20);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "");
        assert!(lines[1].trim_start().starts_with("Working..."));
    }

    #[test]
    fn renders_custom_frames_verbatim_without_colorizing() {
        let spinner: StyleFn = Arc::new(|text: &str| format!("<{text}>"));
        let mut loader = Loader::new(
            None,
            spinner,
            identity(),
            "msg",
            Some(LoaderIndicatorOptions {
                frames: Some(vec!["*".to_string()]),
                interval_ms: None,
            }),
        );
        assert_eq!(loader.get_rendered_indicator(), "*");
        let lines = loader.render(20);
        assert!(lines[1].trim_start().starts_with("* msg"));
    }

    #[test]
    fn updates_the_message_and_notifies_request_render() {
        let calls = Rc::new(Cell::new(0));
        let sink = Rc::clone(&calls);
        let request_render: RequestRenderCallback = Rc::new(move || sink.set(sink.get() + 1));
        let mut loader = Loader::new(
            Some(request_render),
            identity(),
            identity(),
            DEFAULT_MESSAGE,
            None,
        );
        let initial = calls.get();
        loader.set_message("Working...");
        assert!(calls.get() > initial);
        assert_eq!(loader.message(), "Working...");
        assert!(
            loader.render(20)[1]
                .trim_start()
                .starts_with("⠋ Working...")
        );
    }

    fn frames(frames: &[&str], interval_ms: f64) -> Option<LoaderIndicatorOptions> {
        Some(LoaderIndicatorOptions {
            frames: Some(frames.iter().map(|frame| (*frame).to_string()).collect()),
            interval_ms: Some(interval_ms),
        })
    }

    /// TS 的 `setInterval` 在两次渲染之间改写 `Text`；Rust 的惰性实现必须在 `render`
    /// 时把文本同步到"此刻"的帧，否则 spinner 会冻结在构造时的帧（reviewer finding 1）。
    #[test]
    fn render_shows_the_frame_for_the_current_time() {
        let mut loader = Loader::new(
            None,
            identity(),
            identity(),
            "msg",
            frames(&["A", "B", "C", "D", "E"], 200.0),
        );
        assert_eq!(loader.render(20)[1].trim_end(), " A msg");
        std::thread::sleep(std::time::Duration::from_millis(250));
        assert_eq!(loader.render(20)[1].trim_end(), " B msg");
    }

    /// `setIndicator` 必须从第 0 帧重新开始，而不是沿用上一段动画的相位
    /// （reviewer finding 3）。
    #[test]
    fn set_indicator_restarts_the_animation_at_frame_zero() {
        let mut loader = Loader::new(
            None,
            identity(),
            identity(),
            "msg",
            frames(&["A", "B"], 200.0),
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
        loader.set_indicator(frames(&["A", "B"], 200.0));
        assert_eq!(loader.get_rendered_indicator(), "A");
        std::thread::sleep(std::time::Duration::from_millis(250));
        assert_eq!(loader.get_rendered_indicator(), "B");
    }

    /// `tick(now)` 是 spec 建议的宿主渲染循环节拍接口。
    #[test]
    fn tick_reports_when_the_frame_has_advanced() {
        let mut loader = Loader::new(
            None,
            identity(),
            identity(),
            "msg",
            frames(&["A", "B", "C"], 100.0),
        );
        let now = Instant::now();
        assert!(!loader.tick(now + std::time::Duration::from_millis(50)));
        assert!(loader.tick(now + std::time::Duration::from_millis(150)));
        loader.stop();
        assert!(!loader.tick(now + std::time::Duration::from_secs(10)));
    }

    /// 简化的可手动触发定时器：记录 delay 与最后排程的回调。
    type PendingCallback = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;

    struct FakeTimer {
        pending: PendingCallback,
        delays: Rc<RefCell<Vec<u64>>>,
        cancels: Rc<Cell<usize>>,
    }

    impl Timer for FakeTimer {
        fn set(&mut self, delay_ms: u64, callback: Box<dyn FnOnce()>) {
            self.delays.borrow_mut().push(delay_ms);
            *self.pending.borrow_mut() = Some(callback);
        }

        fn cancel(&mut self) {
            self.cancels.set(self.cancels.get() + 1);
            *self.pending.borrow_mut() = None;
        }
    }

    /// 注入节拍器后，每个 interval 调用一次 requestRender 并重新排程（reviewer finding 2
    /// 的实现路径；默认 [`crate::tui::NoopTimer`] 不调度，由宿主决定是否安装真实定时器）。
    #[test]
    fn timer_ticks_request_a_render_and_rearm() {
        let calls = Rc::new(Cell::new(0));
        let sink = Rc::clone(&calls);
        let request_render: RequestRenderCallback = Rc::new(move || sink.set(sink.get() + 1));
        let mut loader = Loader::new(
            Some(request_render),
            identity(),
            identity(),
            "msg",
            frames(&["A", "B", "C"], 10.0),
        );
        let pending: PendingCallback = Rc::new(RefCell::new(None));
        let delays = Rc::new(RefCell::new(Vec::new()));
        let cancels = Rc::new(Cell::new(0));
        loader.set_timer(Box::new(FakeTimer {
            pending: Rc::clone(&pending),
            delays: Rc::clone(&delays),
            cancels: Rc::clone(&cancels),
        }));

        assert_eq!(*delays.borrow(), vec![10]);
        let before = calls.get();
        let callback = pending.borrow_mut().take().expect("scheduled");
        callback();
        assert_eq!(calls.get(), before + 1);
        assert_eq!(*delays.borrow(), vec![10, 10], "re-armed after firing");
        let callback = pending.borrow_mut().take().expect("re-armed");
        callback();
        assert_eq!(calls.get(), before + 2);

        loader.stop();
        assert!(pending.borrow().is_none());
        assert!(cancels.get() > 0);
    }

    #[test]
    fn a_single_frame_does_not_schedule_a_timer() {
        let mut loader = Loader::new(None, identity(), identity(), "msg", frames(&["*"], 10.0));
        let pending: PendingCallback = Rc::new(RefCell::new(None));
        loader.set_timer(Box::new(FakeTimer {
            pending: Rc::clone(&pending),
            delays: Rc::new(RefCell::new(Vec::new())),
            cancels: Rc::new(Cell::new(0)),
        }));
        assert!(pending.borrow().is_none());
    }
}
