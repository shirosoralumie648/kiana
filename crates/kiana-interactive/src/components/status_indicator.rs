//! components/status-indicator.ts。
//!
//! 编辑器顶栏内嵌的工作 / 重试 / 压缩 / 分支摘要状态指示器；`IdleStatus` 用于占位。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/status-indicator.ts`。
//!
//! # 结构差异
//!
//! TS 里 `StatusIndicator extends Loader`，四个子类是它的子类。Rust 没有继承，改为
//! [`StatusIndicator`] trait + 共享 [`Loader`] 的四个具体类型。
//!
//! ## Public API for InteractiveMode
//!
//! - trait [`StatusIndicator`]（`kind` / `set_message` / `render_in_border` /
//!   `render_spinner_in_border` / `dispose` / `set_indicator` / `set_timer` /
//!   `set_countdown_timer` / `set_request_render`，并继承 `Component`）
//! - 构造：[`WorkingStatusIndicator::new`]、[`RetryStatusIndicator::new`]、
//!   [`CompactionStatusIndicator::new`]、[`BranchSummaryStatusIndicator::new`]、[`IdleStatus::new`]
//! - 便捷工厂：[`working_status_indicator`]、[`retry_status_indicator`]、
//!   [`compaction_status_indicator`]、[`branch_summary_status_indicator`]、[`idle_status`]
//!
//! 定时器：TS 的 `Loader` / `CountdownTimer` 在构造时各自 `setInterval`；Rust 侧沿用
//! `kiana-tui` 的宿主定时器约定（没有宿主注入时只会按需渲染）。宿主通过
//! [`StatusIndicator::set_timer`] 安装动画节拍器（所有 kind），通过
//! [`StatusIndicator::set_countdown_timer`] 安装重试倒计时（仅 Retry）。
//!
//! # 为什么需要它
//!
//! Agent 在“工作中 / 自动重试 / 压缩上下文 / 生成分支摘要”期间不能让界面看起来卡死。
//! 指示器把当前阶段（以及可取消提示）塞进编辑器顶边框或独立状态行；`IdleStatus` 在空闲
//! 时仍输出两行空白，占住独立状态行的高度，保持编辑器位置在“有/无状态指示器”之间
//! 切换时不跳动。
//!
//! # 生命周期与调用方
//!
//! 1. 交互模式在收到 Agent 事件（`interactive_mode/events.rs` 的 `AutoRetryStart`、
//!    `SummarizationRetryScheduled` 等）或执行命令（`commands/session.rs` 的分支摘要）时
//!    构造具体指示器，存进 `InteractiveModeInner::active_status_indicator`（类型固定为
//!    `Box<dyn StatusIndicator>`）。
//! 2. 编辑器启用 `embed_working_status` 时，`CustomEditor` 渲染顶边框会调用
//!    [`StatusIndicator::render_in_border`]；空间不足时改用
//!    [`StatusIndicator::render_spinner_in_border`]。更换编辑器时
//!    `interactive_mode/extension_ui/widgets.rs` 通过共享单元格把**同一个实例**重新挂到
//!    新编辑器或状态栏，两条路径操作的是同一对象。
//! 3. 清除时调用 [`StatusIndicator::dispose`]：Loader 停止动画计时，Retry 还会 dispose
//!    内部倒计时。
//!
//! # 渲染契约
//!
//! 所有类型都以 [`Loader`] 作为唯一渲染载体。`Loader::render` 的第 0 行恒为空行、内容从
//! 索引 1 开始；`render_in_border` 依赖这个行号约定取第 1 行（见该方法内的注释），
//! `Component::render` 则把两行原样交给上层。不要在未核对调用方的情况下改变行序。
//!
//! # 注意事项
//!
//! - 所有着色都走全局主题（`kiana_theme::THEME`）；主题未初始化时 `fg` 会 panic，行为
//!   与 TS 在未初始化主题时抛错一致，调用方必须先完成主题初始化（测试都先调
//!   `kiana_theme::init_theme`）。
//! - trait 方法 `dispose` 与 [`Component::dispose`] 同名；具体类型转发时必须写成
//!   `StatusIndicator::dispose(&mut self.base)`，否则会递归调用到自身。
//! - `RetryStatusIndicator` 把倒计时 tick 与渲染解耦（见其字段注释与
//!   `apply_pending_tick`）：tick 只把秒数写入共享单元，下一次渲染前才更新 loader 文本。

use std::cell::Cell;
use std::rc::Rc;

use kiana_ext_api::ui::WorkingIndicatorOptions;
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::loader::{Loader, LoaderIndicatorOptions, RequestRenderCallback};
use kiana_tui::components::text_component::StyleFn;
use kiana_tui::text::truncate_to_width;
use kiana_tui::tui::{Component, Timer};

/// `StatusIndicatorKind = "working" | "retry" | "compaction" | "branchSummary"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum StatusIndicatorKind {
    /// `"working"`。
    #[serde(rename = "working")]
    Working,
    /// `"retry"`。
    #[serde(rename = "retry")]
    Retry,
    /// `"compaction"`。
    #[serde(rename = "compaction")]
    Compaction,
    /// `"branchSummary"`。
    #[serde(rename = "branchSummary")]
    BranchSummary,
}

/// `CompactionStatusReason = "manual" | "threshold" | "overflow"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionStatusReason {
    /// `"manual"`。
    Manual,
    /// `"threshold"`。
    Threshold,
    /// `"overflow"`。
    Overflow,
}

fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn accent_style() -> StyleFn {
    std::sync::Arc::new(|text: &str| fg(ThemeColor::Accent, text))
}

fn muted_style() -> StyleFn {
    std::sync::Arc::new(|text: &str| fg(ThemeColor::Muted, text))
}

fn warning_style() -> StyleFn {
    std::sync::Arc::new(|text: &str| fg(ThemeColor::Warning, text))
}

/// JS 模板里的 `${seconds}`（`Number::toString`）。
///
/// 倒计时秒数以 `f64` 传入（`CountdownTimer` 的 tick 直接回传），渲染进文案前统一按
/// JS 的数字格式化输出（`kiana_json::js_num::format_js_number`）；集中在此转换，
/// 避免调用方各自用 Rust 默认格式化产生漂移。
fn js_seconds(seconds: f64) -> String {
    kiana_json::js_num::format_js_number(seconds)
}

/// `WorkingIndicatorOptions` → `LoaderIndicatorOptions`（TS 直接透传同一个对象）。
pub fn loader_indicator_options(
    indicator: Option<&WorkingIndicatorOptions>,
) -> Option<LoaderIndicatorOptions> {
    indicator.map(|indicator| LoaderIndicatorOptions {
        frames: indicator.frames.clone(),
        interval_ms: indicator.interval_ms,
    })
}

/// `class StatusIndicator extends Loader` 的公共面（Rust trait）。
///
/// 除 TS 的 `kind` / `setMessage` / `renderInBorder` / `renderSpinnerInBorder` / `dispose`
/// 外，还暴露了从 `Loader` 继承、交互模式需要的三个入口（`setIndicator` 用于扩展的
/// `setWorkingIndicator`；`set_timer` 安装宿主动画节拍器；`set_countdown_timer` 安装
/// 重试倒计时的 1 秒定时器）。默认实现为空，便于外部实现者保持源码兼容。
pub trait StatusIndicator: Component {
    /// `readonly kind`。
    fn kind(&self) -> StatusIndicatorKind;

    /// `setMessage(message)`。
    fn set_message(&mut self, message: &str);

    /// `renderInBorder(width)`。
    fn render_in_border(&mut self, width: usize) -> String;

    /// `renderSpinnerInBorder(width)`。
    fn render_spinner_in_border(&mut self, width: usize) -> String;

    /// `dispose()`（停止 loader 动画）。
    ///
    /// 宿主在更换/清除指示器前调用（`show_status_indicator` / `clear_status_indicator`
    /// 路径）；已 dispose 的实例不应再被渲染。
    fn dispose(&mut self);

    /// `setIndicator(indicator?)`（继承自 `Loader`）。
    fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>) {
        let _ = indicator;
    }

    /// 安装 loader 的动画节拍器（TS 的 `setInterval`；见 [`Loader::set_timer`]）。
    fn set_timer(&mut self, timer: Box<dyn Timer>) {
        let _ = timer;
    }

    /// 安装 1 秒倒计时定时器（仅 [`RetryStatusIndicator`] 使用；其它 kind 无操作）。
    fn set_countdown_timer(&mut self, timer: Box<dyn Timer>) {
        let _ = timer;
    }

    /// 替换重绘回调（测试/宿主重建 UI 时用）。
    fn set_request_render(&mut self, request_render: Option<RequestRenderCallback>) {
        let _ = request_render;
    }
}

/// TS 的 `class StatusIndicator` 本体：一个带 kind 的 [`Loader`]。
pub struct BaseStatusIndicator {
    kind: StatusIndicatorKind,
    loader: Loader,
}

impl BaseStatusIndicator {
    /// `constructor(kind, ui, spinnerColorFn, messageColorFn, message, indicator?)`。
    pub fn new(
        kind: StatusIndicatorKind,
        request_render: Option<RequestRenderCallback>,
        spinner_color_fn: StyleFn,
        message_color_fn: StyleFn,
        message: &str,
        indicator: Option<LoaderIndicatorOptions>,
    ) -> Self {
        BaseStatusIndicator {
            kind,
            loader: Loader::new(
                request_render,
                spinner_color_fn,
                message_color_fn,
                message,
                indicator,
            ),
        }
    }

    /// 底层 loader（测试/宿主查询用）。
    pub fn loader(&self) -> &Loader {
        &self.loader
    }

    /// 底层 loader 的可变引用。
    pub fn loader_mut(&mut self) -> &mut Loader {
        &mut self.loader
    }

    /// `setIndicator(indicator?)`。
    pub fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>) {
        self.loader.set_indicator(indicator);
    }

    /// 安装 loader 的动画节拍器（见 [`Loader::set_timer`]）。
    pub fn set_timer(&mut self, timer: Box<dyn Timer>) {
        self.loader.set_timer(timer);
    }

    /// 替换 loader 的重绘回调。
    pub fn set_request_render(&mut self, request_render: Option<RequestRenderCallback>) {
        self.loader.set_request_render(request_render);
    }
}

impl StatusIndicator for BaseStatusIndicator {
    fn kind(&self) -> StatusIndicatorKind {
        self.kind
    }

    fn set_message(&mut self, message: &str) {
        self.loader.set_message(message);
    }

    fn render_in_border(&mut self, width: usize) -> String {
        // `Loader::render` 的第 0 行恒为空行、内容在第 1 行（见模块文档"渲染契约"）。
        // 先按 width + 2 渲染，避免 loader 内部按目标宽度过早截断消息（TS 的
        // `renderInBorder` 同样传 `width + 2`），取回第 1 行后去掉行首的 spinner 分隔
        // 空格与尾部 padding，再截到调用方要的宽度。
        let lines = self.loader.render(width + 2);
        let line = lines.get(1).cloned().unwrap_or_default();
        let cleaned = match line.strip_prefix(' ') {
            Some(rest) => rest.trim_end().to_string(),
            None => line.trim_end().to_string(),
        };
        truncate_to_width(&cleaned, width, "", false)
    }

    fn render_spinner_in_border(&mut self, width: usize) -> String {
        // 空间不足时的退化路径：只输出当前 spinner 帧（不含消息文本），同样截到 width。
        // 帧文本来自 loader 已按当前时刻算好的指示器（`get_rendered_indicator`，不含
        // 消息与分隔空格），这里不推进动画状态。
        truncate_to_width(&self.loader.get_rendered_indicator(), width, "", false)
    }

    fn dispose(&mut self) {
        self.loader.stop();
    }

    fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>) {
        BaseStatusIndicator::set_indicator(self, indicator);
    }

    fn set_timer(&mut self, timer: Box<dyn Timer>) {
        BaseStatusIndicator::set_timer(self, timer);
    }

    fn set_request_render(&mut self, request_render: Option<RequestRenderCallback>) {
        BaseStatusIndicator::set_request_render(self, request_render);
    }
}

impl Component for BaseStatusIndicator {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.loader.render(width)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        Component::invalidate(&mut self.loader);
    }
}

/// `class WorkingStatusIndicator extends StatusIndicator`。
pub struct WorkingStatusIndicator {
    base: BaseStatusIndicator,
}

impl WorkingStatusIndicator {
    /// `constructor(ui, message, indicator?, colorFn?)`。
    ///
    /// `color_fn` 同时用于 spinner 与 message；未提供时 spinner `accent`、message `muted`。
    /// `indicator` 是自定义 spinner 帧/速度（TS 的 `WorkingIndicatorOptions`），经
    /// [`loader_indicator_options`] 转换后透传给底层 [`Loader`]；只有 Working 会传
    /// indicator，其余 kind 固定 `None`。
    pub fn new(
        request_render: Option<RequestRenderCallback>,
        message: &str,
        indicator: Option<WorkingIndicatorOptions>,
        color_fn: Option<StyleFn>,
    ) -> Self {
        let spinner = color_fn.clone().unwrap_or_else(accent_style);
        let text = color_fn.unwrap_or_else(muted_style);
        WorkingStatusIndicator {
            base: BaseStatusIndicator::new(
                StatusIndicatorKind::Working,
                request_render,
                spinner,
                text,
                message,
                loader_indicator_options(indicator.as_ref()),
            ),
        }
    }

    /// 底层指示器。
    pub fn base(&self) -> &BaseStatusIndicator {
        &self.base
    }

    /// 底层指示器（可变）。
    pub fn base_mut(&mut self) -> &mut BaseStatusIndicator {
        &mut self.base
    }
}

impl StatusIndicator for WorkingStatusIndicator {
    fn kind(&self) -> StatusIndicatorKind {
        StatusIndicatorKind::Working
    }

    fn set_message(&mut self, message: &str) {
        self.base.set_message(message);
    }

    fn render_in_border(&mut self, width: usize) -> String {
        self.base.render_in_border(width)
    }

    fn render_spinner_in_border(&mut self, width: usize) -> String {
        self.base.render_spinner_in_border(width)
    }

    fn dispose(&mut self) {
        StatusIndicator::dispose(&mut self.base);
    }

    fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>) {
        self.base.set_indicator(indicator);
    }

    fn set_timer(&mut self, timer: Box<dyn Timer>) {
        self.base.set_timer(timer);
    }

    fn set_request_render(&mut self, request_render: Option<RequestRenderCallback>) {
        self.base.set_request_render(request_render);
    }
}

impl Component for WorkingStatusIndicator {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.base.render(width)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        Component::invalidate(&mut self.base);
    }
}

/// `class RetryStatusIndicator extends StatusIndicator`。
pub struct RetryStatusIndicator {
    base: BaseStatusIndicator,
    /// `CountdownTimer`（TS 的 `setInterval` 由定时器抽象承担）。
    countdown: Option<kiana_ui_kit::CountdownTimer>,
    /// 当前剩余秒数（宿主/测试查询）。
    seconds: Rc<Cell<f64>>,
    /// 上一次倒计时 tick 写入、尚未应用到 loader 文本的秒数。
    ///
    /// TS 只在 tick 回调里 `setMessage`；外部 `set_message` 必须保持到下一次 tick。这里把
    /// tick 与渲染解耦（回调中拿不到 loader），在渲染前才应用 tick 的值。
    pending_tick: Rc<Cell<Option<f64>>>,
    /// 尝试序号（`attempt`）。
    pub attempt: u32,
    /// 最大尝试次数（`maxAttempts`）。
    pub max_attempts: u32,
}

impl RetryStatusIndicator {
    /// `constructor(ui, attempt, maxAttempts, delayMs)`。
    ///
    /// 构造即显示第一次倒计时文案（`Math.ceil(delayMs / 1000)` 秒）；spinner 固定
    /// `warning` 色、message 固定 `muted` 色。倒计时 tick 与渲染是解耦的（见
    /// `pending_tick` 字段注释）：构造期 `CountdownTimer` 会立即触发一次 tick，此处显式
    /// 清掉对应的 pending 值，让外部 `set_message` 的语义从构造完成后即刻生效。
    pub fn new(
        request_render: Option<RequestRenderCallback>,
        attempt: u32,
        max_attempts: u32,
        delay_ms: f64,
    ) -> Self {
        // TS `Math.ceil(delayMs / 1000)`。
        let initial_seconds = (delay_ms / 1000.0).ceil();
        let seconds = Rc::new(Cell::new(initial_seconds));
        let pending_tick = Rc::new(Cell::new(None::<f64>));
        let tick_seconds = Rc::clone(&seconds);
        let tick_pending = Rc::clone(&pending_tick);
        let countdown = kiana_ui_kit::CountdownTimer::new(
            delay_ms,
            request_render.clone(),
            Box::new(move |seconds_now: f64| {
                tick_seconds.set(seconds_now);
                tick_pending.set(Some(seconds_now));
            }),
            Box::new(|| {}),
        );
        let mut indicator = RetryStatusIndicator {
            base: BaseStatusIndicator::new(
                StatusIndicatorKind::Retry,
                request_render,
                warning_style(),
                muted_style(),
                "",
                None,
            ),
            countdown: Some(countdown),
            seconds,
            pending_tick,
            attempt,
            max_attempts,
        };
        let seconds_now = indicator.seconds.get();
        let message = indicator.retry_message(seconds_now);
        indicator.base.set_message(&message);
        // 构造期 `CountdownTimer` 立即触发了一次 onTick；上面的 set_message 已经体现了它，
        // 清掉 pending，避免首次渲染再覆盖（外部 set_message 应当保持到下一次 tick）。
        indicator.pending_tick.set(None);
        indicator
    }

    fn retry_message(&self, seconds: f64) -> String {
        format!(
            "Retrying ({}/{}) in {}s... ({} to cancel)",
            self.attempt,
            self.max_attempts,
            js_seconds(seconds),
            kiana_ui_kit::key_text("app.interrupt")
        )
    }

    /// 把倒计时 tick 写入的秒数应用到 loader 文本（TS 在 tick 回调里 `setMessage`）。
    fn apply_pending_tick(&mut self) {
        if let Some(seconds) = self.pending_tick.get() {
            self.pending_tick.set(None);
            let message = self.retry_message(seconds);
            self.base.set_message(&message);
        }
    }

    /// 手动推进倒计时（测试/宿主定时器用）：设置剩余秒数并刷新文本。
    ///
    /// 与 tick 路径不同：直接应用、不留 pending（下一次渲染立即看到新秒数）。
    pub fn set_remaining_seconds(&mut self, seconds: f64) {
        self.seconds.set(seconds);
        self.pending_tick.set(None);
        let message = self.retry_message(seconds);
        self.base.set_message(&message);
    }

    /// 当前剩余秒数。
    pub fn remaining_seconds(&self) -> f64 {
        self.seconds.get()
    }

    /// 安装 1 秒倒计时定时器（转发给内部 `CountdownTimer`）。
    ///
    /// 必须由宿主在构造后调用：`CountdownTimer` 自身不产生节拍（Rust 侧没有 TS 的
    /// `setInterval`），未安装定时器时秒数保持不动，只有渲染/`set_remaining_seconds`
    /// 会改写文本。`dispose` 会丢弃倒计时，之后再调用本方法是无操作（`countdown` 已是
    /// `None`）。
    pub fn set_countdown_timer(&mut self, timer: Box<dyn Timer>) {
        if let Some(countdown) = &mut self.countdown {
            countdown.set_timer(timer);
        }
    }

    /// 底层指示器。
    pub fn base(&self) -> &BaseStatusIndicator {
        &self.base
    }

    /// 底层指示器（可变）。
    pub fn base_mut(&mut self) -> &mut BaseStatusIndicator {
        &mut self.base
    }
}

impl StatusIndicator for RetryStatusIndicator {
    fn kind(&self) -> StatusIndicatorKind {
        StatusIndicatorKind::Retry
    }

    fn set_message(&mut self, message: &str) {
        self.base.set_message(message);
    }

    fn render_in_border(&mut self, width: usize) -> String {
        self.apply_pending_tick();
        self.base.render_in_border(width)
    }

    fn render_spinner_in_border(&mut self, width: usize) -> String {
        self.base.render_spinner_in_border(width)
    }

    /// `dispose()` 先停倒计时再停 loader；`countdown.take()` 保证重复 dispose 无副作用。
    fn dispose(&mut self) {
        if let Some(mut countdown) = self.countdown.take() {
            countdown.dispose();
        }
        StatusIndicator::dispose(&mut self.base);
    }

    fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>) {
        self.base.set_indicator(indicator);
    }

    fn set_timer(&mut self, timer: Box<dyn Timer>) {
        self.base.set_timer(timer);
    }

    fn set_countdown_timer(&mut self, timer: Box<dyn Timer>) {
        RetryStatusIndicator::set_countdown_timer(self, timer);
    }

    fn set_request_render(&mut self, request_render: Option<RequestRenderCallback>) {
        self.base.set_request_render(request_render);
    }
}

impl Component for RetryStatusIndicator {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.apply_pending_tick();
        self.base.render(width)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        Component::invalidate(&mut self.base);
    }
}

/// `class CompactionStatusIndicator extends StatusIndicator`。
pub struct CompactionStatusIndicator {
    base: BaseStatusIndicator,
}

impl CompactionStatusIndicator {
    /// `constructor(ui, reason, colorFn?)`。
    ///
    /// 三种 reason 对应三条逐字文案（manual/threshold/overflow；见模块文档与
    /// `spec/ca-interactive-components.md` §2.24 的消息表），spinner/message 的着色规则
    /// 与 [`WorkingStatusIndicator::new`] 相同：给了 `color_fn` 就两处都用，否则
    /// spinner `accent`、message `muted`。
    pub fn new(
        request_render: Option<RequestRenderCallback>,
        reason: CompactionStatusReason,
        color_fn: Option<StyleFn>,
    ) -> Self {
        let cancel_hint = format!("({} to cancel)", kiana_ui_kit::key_text("app.interrupt"));
        let label = match reason {
            CompactionStatusReason::Manual => format!("Compacting context... {cancel_hint}"),
            CompactionStatusReason::Threshold => format!("Auto-compacting... {cancel_hint}"),
            CompactionStatusReason::Overflow => {
                format!("Context overflow detected, Auto-compacting... {cancel_hint}")
            }
        };
        let spinner = color_fn.clone().unwrap_or_else(accent_style);
        let text = color_fn.unwrap_or_else(muted_style);
        CompactionStatusIndicator {
            base: BaseStatusIndicator::new(
                StatusIndicatorKind::Compaction,
                request_render,
                spinner,
                text,
                &label,
                None,
            ),
        }
    }

    /// 底层指示器。
    pub fn base(&self) -> &BaseStatusIndicator {
        &self.base
    }

    /// 底层指示器（可变）。
    pub fn base_mut(&mut self) -> &mut BaseStatusIndicator {
        &mut self.base
    }
}

impl StatusIndicator for CompactionStatusIndicator {
    fn kind(&self) -> StatusIndicatorKind {
        StatusIndicatorKind::Compaction
    }

    fn set_message(&mut self, message: &str) {
        self.base.set_message(message);
    }

    fn render_in_border(&mut self, width: usize) -> String {
        self.base.render_in_border(width)
    }

    fn render_spinner_in_border(&mut self, width: usize) -> String {
        self.base.render_spinner_in_border(width)
    }

    fn dispose(&mut self) {
        StatusIndicator::dispose(&mut self.base);
    }

    fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>) {
        self.base.set_indicator(indicator);
    }

    fn set_timer(&mut self, timer: Box<dyn Timer>) {
        self.base.set_timer(timer);
    }

    fn set_request_render(&mut self, request_render: Option<RequestRenderCallback>) {
        self.base.set_request_render(request_render);
    }
}

impl Component for CompactionStatusIndicator {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.base.render(width)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        Component::invalidate(&mut self.base);
    }
}

/// `class BranchSummaryStatusIndicator extends StatusIndicator`。
pub struct BranchSummaryStatusIndicator {
    base: BaseStatusIndicator,
}

impl BranchSummaryStatusIndicator {
    /// `constructor(ui)`。
    pub fn new(request_render: Option<RequestRenderCallback>) -> Self {
        BranchSummaryStatusIndicator {
            base: BaseStatusIndicator::new(
                StatusIndicatorKind::BranchSummary,
                request_render,
                accent_style(),
                muted_style(),
                &format!(
                    "Summarizing branch... ({} to cancel)",
                    kiana_ui_kit::key_text("app.interrupt")
                ),
                None,
            ),
        }
    }

    /// 底层指示器。
    pub fn base(&self) -> &BaseStatusIndicator {
        &self.base
    }

    /// 底层指示器（可变）。
    pub fn base_mut(&mut self) -> &mut BaseStatusIndicator {
        &mut self.base
    }
}

impl StatusIndicator for BranchSummaryStatusIndicator {
    fn kind(&self) -> StatusIndicatorKind {
        StatusIndicatorKind::BranchSummary
    }

    fn set_message(&mut self, message: &str) {
        self.base.set_message(message);
    }

    fn render_in_border(&mut self, width: usize) -> String {
        self.base.render_in_border(width)
    }

    fn render_spinner_in_border(&mut self, width: usize) -> String {
        self.base.render_spinner_in_border(width)
    }

    fn dispose(&mut self) {
        StatusIndicator::dispose(&mut self.base);
    }

    fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>) {
        self.base.set_indicator(indicator);
    }

    fn set_timer(&mut self, timer: Box<dyn Timer>) {
        self.base.set_timer(timer);
    }

    fn set_request_render(&mut self, request_render: Option<RequestRenderCallback>) {
        self.base.set_request_render(request_render);
    }
}

impl Component for BranchSummaryStatusIndicator {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.base.render(width)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        Component::invalidate(&mut self.base);
    }
}

/// `class IdleStatus implements Component`：两行满宽空格，保持编辑器高度。
///
/// 不实现 [`StatusIndicator`]：它是"没有指示器"时的占位。清除一个**非内嵌**指示器时，
/// 宿主在 regular 模式 + `clearOnShrink` 下会往状态容器里补一个 `IdleStatus`
/// （见 `interactive_mode/extension_ui/widgets.rs` 的 `clear_status_indicator`），
/// 用两行空格顶住高度，避免界面收缩产生跳变。
#[derive(Default)]
pub struct IdleStatus;

impl IdleStatus {
    /// 构造。
    pub fn new() -> Self {
        IdleStatus
    }
}

impl Component for IdleStatus {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 复用同一个 String 实例（与 TS 相同）：两行内容恒等，clone 一个 String 比重复
        // 分配更省；宽度为 0 时两行都是空串。
        let empty_line = " ".repeat(width);
        vec![empty_line.clone(), empty_line]
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        // 没有缓存状态需要失效。
    }
}

/// `StatusIndicatorKind` 的字面量（TS 字符串联合值的 Rust 镜像），供需要字符串形式
/// kind 的调用方使用。
/// （待核实：当前仓库内暂无调用方。宿主的 `clear_status_indicator` 比较的是
/// `StatusIndicatorKind` 枚举值，不走本函数。）
pub fn status_indicator_kind_name(kind: StatusIndicatorKind) -> &'static str {
    match kind {
        StatusIndicatorKind::Working => "working",
        StatusIndicatorKind::Retry => "retry",
        StatusIndicatorKind::Compaction => "compaction",
        StatusIndicatorKind::BranchSummary => "branchSummary",
    }
}

/// `WorkingStatusIndicator(ui, message)` 的便捷构造。
pub fn working_status_indicator(
    request_render: Option<RequestRenderCallback>,
    message: &str,
) -> WorkingStatusIndicator {
    WorkingStatusIndicator::new(request_render, message, None, None)
}

/// `RetryStatusIndicator(ui, attempt, maxAttempts, delayMs)` 的便捷构造。
pub fn retry_status_indicator(
    request_render: Option<RequestRenderCallback>,
    attempt: u32,
    max_attempts: u32,
    delay_ms: f64,
) -> RetryStatusIndicator {
    RetryStatusIndicator::new(request_render, attempt, max_attempts, delay_ms)
}

/// `CompactionStatusIndicator(ui, reason, colorFn?)` 的便捷构造。
pub fn compaction_status_indicator(
    request_render: Option<RequestRenderCallback>,
    reason: CompactionStatusReason,
    color_fn: Option<StyleFn>,
) -> CompactionStatusIndicator {
    CompactionStatusIndicator::new(request_render, reason, color_fn)
}

/// `BranchSummaryStatusIndicator(ui)` 的便捷构造。
pub fn branch_summary_status_indicator(
    request_render: Option<RequestRenderCallback>,
) -> BranchSummaryStatusIndicator {
    BranchSummaryStatusIndicator::new(request_render)
}

/// `IdleStatus` 的便捷构造。
pub fn idle_status() -> IdleStatus {
    IdleStatus::new()
}
