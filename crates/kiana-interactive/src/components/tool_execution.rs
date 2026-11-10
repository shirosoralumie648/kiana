//! components/tool-execution.ts。
//!
//! 工具调用 / 结果的渲染容器：渲染器槽位（call/result）、self 渲染壳、兜底文本、
//! 点击展开、图片（含非 PNG 的按需转码与复用）。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/tool-execution.ts`。
//!
//! # 与 TS 的表示差异
//!
//! - TS 的 `ui: TUI` 只用于 `requestRender()`；Rust 接收 [`RequestRenderCallback`]。
//! - `ToolRenderContext.invalidate` 的类型是 `Arc<dyn Fn() + Send + Sync>`（kiana-ext-api 冻结）。
//!   TS 的闭包是 `this.invalidate(); this.ui.requestRender()`：这里置位失效标志，并在
//!   **UI 线程**上调用组件的 request-render 回调（同线程的渲染器调用由线程局部注册表找回
//!   `Rc` 回调，不跨线程）。`kiana-tools` 的 `tool_timer_text` 会在**后台线程**每秒调用该闭包；
//!   跨线程时只有宿主动过 [`ToolExecutionComponent::set_thread_request_render`] 安装了线程安全
//!   的重绘通道才会请求重绘，否则只置位标志（下一次任意渲染应用）。
//! - TS 的 `updateArgs` 只置 `argsPending`；这里保持一致（#9265：每个渲染帧最多跑一次渲染器）。
//!
//! ## Public API for InteractiveMode
//!
//! - [`ToolExecutionComponent::new`]（toolName, toolCallId, args, [`ToolExecutionOptions`],
//!   `Option<ToolRenderers>`, requestRender, cwd）
//! - [`renderers_from_definition`]：从 `ToolDefinition` 取三个渲染槽位
//! - `update_args` / `mark_execution_started` / `set_args_complete` / `update_result`
//! - `set_expanded` / `set_output_pad` / `set_show_images` / `set_image_width_cells`
//! - `set_thread_request_render`：跨线程 `invalidate` 的重绘通道
//!
//! # 在系统中的位置
//!
//! 调用方是 `kiana-interactive` 的 `InteractiveMode`：`interactive_mode/events.rs` 在
//! `ToolExecutionStart` / `ToolExecutionUpdate` / `ToolExecutionEnd` 以及流式消息
//! 更新时驱动它，`interactive_mode/render.rs` 的历史重建也用它；
//! `components/index.rs` 只再导出 `ToolExecutionComponent` 与 `ToolExecutionOptions`。
//! 本组件自己实现 [`Component`]，最终作为聊天容器的子项被渲染，所以它不负责滚动或
//! 布局，只需要按给定宽度渲染若干行。
//!
//! # 一次工具调用的执行流程
//!
//! 1. 流式参数：`update_args` 只更新 `args`、置 `args_pending` 并请求重绘，重建推迟到
//!    下一次 `update_display`（通常由 `render` 触发；避免每个参数片段都跑一次渲染器，
//!    见模块开头的 #9265 说明）。
//! 2. 执行开始：`mark_execution_started` 立刻重建并请求重绘，渲染器此时看到
//!    `execution_started = true`。
//! 3. 参数完整：`set_args_complete` 立刻重建并请求重绘。
//! 4. 部分结果：`update_result(..., is_partial = true)` 立刻重建；此时不向渲染器提供耗时。
//! 5. 最终结果：`update_result(..., false)` 立刻重建，并把 `duration_ms` 换算成整数毫秒。
//!
//! 注意第 4、5 步以及 `set_expanded` / `set_output_pad` / `set_show_images` /
//! `set_image_width_cells` 都**只重建组件树、不调用重绘回调**；调用方（`InteractiveMode`
//! 的事件处理）在此之后自行 `request_render`，测试则直接调用 `render`。
//!
//! # 布局与失败模式
//!
//! - [`RenderShell::Default`]：`Spacer` + `BoxComponent`（背景随 partial / 成功 / 失败切换）
//!   + 每张图片前的 `Spacer`；[`RenderShell::SelfRender`]：工具自己的 `Container` + 图片，
//!   首行是否留空由 `render_self_shell` 决定。
//! - 渲染器 panic 由 `catch_unwind_silent` 吞掉，退回兜底文本且不缓存 `last_component`；
//!   这保证单个渲染器出错不会打断整个 TUI 渲染。
//! - 主题未初始化时 `fg` / `bg` / `render_theme` 会 panic（TS 在这里同样抛异常），
//!   宿主必须先调用 `kiana_theme::init_theme`。
//! - 图片只在终端支持图片协议、`show_images` 为真、且 data 与 MIME 都非空时创建；
//!   非 PNG 图片需要 `kiana-core` 的 PNG 转码器，转码器注册成功后通过 `dirty` 标志
//!   触发下一帧重建。
//!
//! # 线程约束
//!
//! 结构体含 `Rc` / `RefCell`（渲染器返回的组件通常也持有 `Rc`），因此整个组件是 `!Send`，
//! 只能在创建它的线程上使用。唯一允许跨线程调用的是 `ToolRenderContext.invalidate`
//! 闭包（`Arc<dyn Fn() + Send + Sync>`）；后台线程调用它时，只有宿主安装过
//! `set_thread_request_render` 才会真正请求重绘。

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::{JsonValue, UserContentBlock};
use kiana_core::image_process::ensure_png_transcoder;
use kiana_ext_api::tool_types::{
    RenderShell, RenderState, ToolRenderContext, ToolRenderResultOptions,
};

use crate::components::markdown_transform::catch_unwind_silent;

/// TS `export type { ToolRenderers }`。
pub use kiana_ext_api::tool_types::ToolRenderers;
use kiana_theme::theme_tokens::{ThemeBg, ThemeColor};
use kiana_tools::render_utils::{format_tool_call_with_args, get_text_output};
use kiana_tui::components::box_component::BoxComponent;
use kiana_tui::components::image::{Image, ImageOptions, ImageTheme};
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::terminal_image::get_capabilities;
use kiana_tui::tui::{
    Component, Container, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType,
    dispatch_mouse_event,
};

/// 无渲染器时的结果预览行数。
const FALLBACK_PREVIEW_LINES: usize = 10;

// 前景/背景着色的小包装。两者在主题未初始化时 panic：调用点都在渲染路径上，
// 与 TS 在这里直接抛异常的行为一致，因此不返回 Result。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn bg(color: ThemeBg, text: &str) -> String {
    kiana_theme::THEME
        .bg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// TS `ToolExecutionOptions`。
///
/// 三个值在创建组件时由调用方从设置里取一次（见 `InteractiveMode::create_tool_component`），
/// 之后可分别用 `set_show_images` / `set_image_width_cells` / `set_output_pad` 更新。
/// [`Default`] 与 TS 的缺省参数一致：显示图片、60 列、内缩 1。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToolExecutionOptions {
    /// 是否显示内嵌图片（默认 true）。为 false 时不为结果里的图片创建 `Image` 组件，
    /// 兜底文本输出改用图片占位符。
    pub show_images: bool,
    /// 图片最大列数（默认 60）。传给 `Image` 的 `max_width_cells`；实际宽度还会受
    /// 当帧渲染宽度限制。
    pub image_width_cells: f64,
    /// 水平内缩（默认 1）。作为内容容器的 `padding_x`，宽度会左右各扣一份。
    pub output_pad: usize,
}

impl Default for ToolExecutionOptions {
    fn default() -> Self {
        ToolExecutionOptions {
            show_images: true,
            image_width_cells: 60.0,
            output_pad: 1,
        }
    }
}

/// `updateResult(result, isPartial?)` 的 result 参数。
///
/// 这是组件保存的结果快照。结果渲染器收到的是由私有方法 `to_agent_result` 转换出的
/// [`AgentToolResult`]，其中 `structured_content` / `usage` / `terminate` 恒为 `None`，
/// `is_error` 来自本结构的 `is_error` 字段。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionResult {
    /// 文本 / 图片内容块。文本块用于兜底输出与 `AgentToolResult.content`；
    /// 图片块在 `update_display` 里另行转成 `Image` 组件。
    pub content: Vec<UserContentBlock>,
    /// 结构化详情，原样交给结果渲染器，本组件不解释它。
    pub details: JsonValue,
    /// 是否错误。为 true 时组件背景切到错误色，并作为 `AgentToolResult.is_error`
    /// 传给结果渲染器。
    pub is_error: bool,
    /// 最终结果的耗时（毫秒）；partial 结果没有。
    ///
    /// 只在最终结果（`is_partial = false`）时被消费：有限且大于 0 的值向下取整成整数
    /// 毫秒交给渲染器，其余情况给 0；`None` 则保持 `None`。
    pub duration_ms: Option<f64>,
}

impl ToolExecutionResult {
    /// 构造（`duration_ms = None`）。
    pub fn new(content: Vec<UserContentBlock>, details: JsonValue, is_error: bool) -> Self {
        ToolExecutionResult {
            content,
            details,
            is_error,
            duration_ms: None,
        }
    }

    /// 带耗时的构造。`Some(ms)` 只在最终结果时被消费（见 `duration_ms` 字段）。
    pub fn with_duration_ms(mut self, duration_ms: Option<f64>) -> Self {
        self.duration_ms = duration_ms;
        self
    }

    /// 从 `AgentToolResult` 构造：`duration_ms = None`。
    ///
    /// 只搬运 `content` 与 `details`；`structured_content` / `usage` / `terminate`
    /// 被丢弃。`is_error` 不由 `result` 推导，而由调用方单独给出。
    pub fn from_agent_result(result: AgentToolResult<JsonValue>, is_error: bool) -> Self {
        ToolExecutionResult {
            content: result.content,
            details: result.details,
            is_error,
            duration_ms: None,
        }
    }

    fn to_agent_result(&self) -> AgentToolResult<JsonValue> {
        AgentToolResult {
            content: self.content.clone(),
            details: self.details.clone(),
            structured_content: None,
            usage: None,
            is_error: Some(self.is_error),
            terminate: None,
        }
    }
}

/// 图片输入快照（用于跨帧复用已转码的 `Image`）。
#[derive(Debug, Clone, PartialEq)]
struct ImageSource {
    data: String,
    mime_type: String,
    width_cells: f64,
}

// GUI 线程上「组件 id → request-render 回调」的注册表。
//
// `ToolRenderContext.invalidate` 的类型被 kiana-ext-api 冻结为 `Arc<dyn Fn() + Send + Sync>`，
// 不能捕获 `!Send` 的 [`RequestRenderCallback`]（`Rc`）。同线程调用时按 id 从这里取回
// `Rc` 回调并调用；后台线程调用不走这里（见 [`ToolExecutionComponent::set_thread_request_render`]）。
thread_local! {
    static INVALIDATE_WAKERS: RefCell<HashMap<u64, RequestRenderCallback>> =
        RefCell::new(HashMap::new());
}

// 每个组件在构造时按 id 登记、Drop 时移除；同线程的 `invalidate` 闭包只带 id，
// 调用时再从本线程注册表取回 `Rc` 回调。
//
// id 只要求唯一，不要求跨线程顺序，所以用 Relaxed 的 fetch_add 即可。
static NEXT_WAKER_ID: AtomicU64 = AtomicU64::new(1);

/// 跨线程重绘通道（TS `ui.requestRender()` 的线程安全形态）。
pub type ThreadRequestRender = Arc<dyn Fn() + Send + Sync>;

/// 共享槽：`set_thread_request_render` 之后，已创建的 `invalidate` 闭包也能读到新值。
type ThreadRequestRenderSlot = Arc<Mutex<Option<ThreadRequestRender>>>;

/// 渲染器上一帧返回的组件（TS `callRendererComponent` / `resultRendererComponent`）。
type CachedComponent = Option<Box<dyn Component>>;

/// 渲染器槽位（call / result），用于在重建前找回上一帧返回的组件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegionSlot {
    Call,
    Result,
}

/// 点击区域：包装一个组件，点击时切换展开状态。
///
/// 与 `MouseRegion` 的语义一致：子组件先处理事件，未处理时才调用这里的处理器。
///
/// `child` 是 `Option`：`updateDisplay` 重建前会取回渲染器上一帧返回的原始组件（TS 的
/// `callRendererComponent` / `resultRendererComponent`），取回后包装器随即被丢弃。
struct ResultRegion {
    child: Option<Box<dyn Component>>,
    slot: RegionSlot,
    /// 子组件是渲染器返回值（true）还是兜底组件（false）；只有前者进入缓存。
    cacheable: bool,
    has_result: Rc<Cell<bool>>,
    expanded: Rc<Cell<bool>>,
    dirty: Rc<Cell<bool>>,
}

impl Component for ResultRegion {
    fn render(&mut self, width: usize) -> Vec<String> {
        match &mut self.child {
            Some(child) => child.render(width),
            None => Vec::new(),
        }
    }

    // 先让子组件处理：`dispatch_mouse_event` 只在子组件停止传播（handled/capture/focus）
    // 时返回 Some。子组件没有消费事件、且已有结果、且是左键单击时，切换展开状态并置
    // `dirty`，下一帧 `render` 据此重建。返回 `handled = true` 让外层停止继续分发。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if let Some(child) = &mut self.child
            && let Some(result) = dispatch_mouse_event(child.as_mut(), event)
        {
            return Some(result);
        }
        if !self.has_result.get()
            || event.event_type != TuiMouseEventType::Click
            || event.button != TuiMouseButton::Left
        {
            return None;
        }
        self.expanded.set(!self.expanded.get());
        self.dirty.set(true);
        Some(TuiMouseEventResult {
            handled: Some(true),
            ..TuiMouseEventResult::default()
        })
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        if let Some(child) = &mut self.child {
            child.invalidate();
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

/// 无 toolDefinition 时的通用兜底区域：`Text` + 点击展开。
struct ContentTextRegion {
    text: Rc<RefCell<Text>>,
    has_result: Rc<Cell<bool>>,
    expanded: Rc<Cell<bool>>,
    dirty: Rc<Cell<bool>>,
}

impl Component for ContentTextRegion {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.text.borrow_mut().render(width)
    }

    // 无子组件可转发：已有结果时左键单击直接切换展开状态并置 `dirty`，下一帧重建。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if !self.has_result.get()
            || event.event_type != TuiMouseEventType::Click
            || event.button != TuiMouseButton::Left
        {
            return None;
        }
        self.expanded.set(!self.expanded.get());
        self.dirty.set(true);
        Some(TuiMouseEventResult {
            handled: Some(true),
            ..TuiMouseEventResult::default()
        })
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        self.text.borrow_mut().invalidate();
    }
}

/// `class ToolExecutionComponent extends Container`。
///
/// 一个实例对应一次工具调用：call 槽位在构造时创建，result 槽位在 `update_result`
/// 之后才创建；两者共享同一份渲染状态。Rust 没有继承，TS 的 Container 行为由组合
/// `BoxComponent` / `Container` / `Text` 实现，因此布局与鼠标分发都是手写的。
pub struct ToolExecutionComponent {
    /// Default 外壳的内容容器（负责背景色与 `output_pad`）。
    content_box: BoxComponent,
    // 无 toolDefinition 时展示的整行文本，以及共享同一个 `Text` 的可点击包装。
    content_text: Rc<RefCell<Text>>,
    content_text_region: ContentTextRegion,
    // SelfRender 外壳的内容容器；背景与内缩由工具渲染器自行处理。
    self_render_container: Container,
    // 上一帧 self 外壳内容（不含图片）的行数，handle_mouse 用它限定可点击范围。
    self_render_height: usize,
    // call / result 渲染器共享的 JSON 状态（`ToolRenderContext.state`）。
    renderer_state: RenderState,
    // 图片三件套按「第 N 张被包含的图片」索引对齐；Spacer 渲染在对应 Image 之前。
    image_components: Vec<Image>,
    image_sources: Vec<ImageSource>,
    image_spacers: Vec<Spacer>,
    tool_name: String,
    tool_call_id: String,
    args: JsonValue,
    expanded: Rc<Cell<bool>>,
    show_images: bool,
    image_width_cells: f64,
    output_pad: usize,
    // 初始为 true，直到 update_result(..., false) 给出最终结果；决定背景色，
    // 并决定渲染器能否看到耗时。
    is_partial: bool,
    tool_definition: Option<ToolRenderers>,
    request_render: RequestRenderCallback,
    cwd: PathBuf,
    /// 本组件在 [`INVALIDATE_WAKERS`] 里的 id（同线程 `invalidate` 找回重绘回调）。
    waker_id: u64,
    /// 创建组件的线程（UI 线程）；只有该线程可以调用 `Rc` 重绘回调。
    ui_thread: ThreadId,
    /// 跨线程 `invalidate` 的重绘通道（宿主可选注入；`get_render_context` 已创建的闭包
    /// 通过共享槽读取，后续安装也立即生效）。
    thread_request_render: ThreadRequestRenderSlot,
    execution_started: bool,
    args_complete: bool,
    // 流式参数待应用：update_args 置位，update_display（render 或其它立即重建路径）清除。
    args_pending: bool,
    result: Option<ToolExecutionResult>,
    // 与点击区域共享的状态：是否已有结果（有结果才允许点击展开）、是否需要重建。
    // `dirty` 也由 PNG 转码器就绪回调置位。
    has_result: Rc<Cell<bool>>,
    hide_component: bool,
    dirty: Rc<Cell<bool>>,
    /// 边框/渲染器线程置位的失效标志（`ToolRenderContext.invalidate`）。
    invalidated: Arc<AtomicBool>,
    // Default 外壳顶部预留的一行空白。
    spacer: Spacer,
}

impl ToolExecutionComponent {
    /// `constructor(toolName, toolCallId, args, options?, toolDefinition, ui, cwd)`。
    ///
    /// `tool_definition` 接受 `ToolRenderers`（由 `ToolDefinition` 的
    /// `render_shell`/`render_call`/`render_result` 三个字段组成，见
    /// [`renderers_from_definition`]）。
    ///
    /// 构造时就把 `request_render` 按 `waker_id` 登记到当前线程的
    /// `INVALIDATE_WAKERS` 注册表，并立即执行一次 `update_display`：调用方拿到的组件
    /// 已经渲染过一帧，工具渲染器在 `new` 返回前已运行一次（测试
    /// `render_context_invalidate_requests_a_repaint_on_the_ui_thread` 锁定该行为）。
    /// `cwd` 与 `tool_call_id` 会放进 [`ToolRenderContext`] 供渲染器读取。
    ///
    /// 初始状态为 `is_partial = true`、无结果；因此构造后的背景是 pending 色，
    /// 直到 `update_result` 给出最终结果。
    pub fn new(
        tool_name: impl Into<String>,
        tool_call_id: impl Into<String>,
        args: JsonValue,
        options: ToolExecutionOptions,
        tool_definition: Option<ToolRenderers>,
        request_render: RequestRenderCallback,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        let expanded = Rc::new(Cell::new(false));
        let has_result = Rc::new(Cell::new(false));
        let dirty = Rc::new(Cell::new(false));
        let waker_id = NEXT_WAKER_ID.fetch_add(1, Ordering::Relaxed);
        let ui_thread = std::thread::current().id();
        let _ = INVALIDATE_WAKERS.try_with(|wakers| {
            wakers
                .borrow_mut()
                .insert(waker_id, Rc::clone(&request_render));
        });
        let content_text = Rc::new(RefCell::new(Text::with_background(
            "",
            1,
            1,
            Some(Arc::new(|text: &str| bg(ThemeBg::ToolPendingBg, text))),
        )));
        let content_text_region = ContentTextRegion {
            text: Rc::clone(&content_text),
            has_result: Rc::clone(&has_result),
            expanded: Rc::clone(&expanded),
            dirty: Rc::clone(&dirty),
        };
        let mut component = ToolExecutionComponent {
            content_box: BoxComponent::new(
                1,
                1,
                Some(Arc::new(|text: &str| bg(ThemeBg::ToolPendingBg, text))),
            ),
            content_text,
            content_text_region,
            self_render_container: Container::new(),
            self_render_height: 0,
            renderer_state: Arc::new(Mutex::new(JsonValue::Object(Default::default()))),
            image_components: Vec::new(),
            image_sources: Vec::new(),
            image_spacers: Vec::new(),
            tool_name: tool_name.into(),
            tool_call_id: tool_call_id.into(),
            args,
            expanded,
            show_images: options.show_images,
            image_width_cells: options.image_width_cells,
            output_pad: options.output_pad,
            is_partial: true,
            tool_definition,
            request_render,
            cwd: cwd.into(),
            waker_id,
            ui_thread,
            thread_request_render: Arc::new(Mutex::new(None)),
            execution_started: false,
            args_complete: false,
            args_pending: false,
            result: None,
            has_result,
            hide_component: false,
            dirty,
            invalidated: Arc::new(AtomicBool::new(false)),
            spacer: Spacer::new(),
        };
        component.update_display();
        component
    }

    fn get_render_shell(&self) -> RenderShell {
        self.tool_definition
            .as_ref()
            .and_then(|definition| definition.render_shell)
            .unwrap_or(RenderShell::Default)
    }

    fn has_renderer_definition(&self) -> bool {
        self.tool_definition.is_some()
    }

    fn render_theme() -> Arc<kiana_theme::theme::Theme> {
        kiana_theme::THEME
            .current()
            .expect("theme must be initialized before rendering (TS throws here)")
    }

    // 组装一次渲染器调用的上下文快照。除 `invalidate` / `state`（共享句柄）与
    // `last_component`（所有权移交）外，其余字段都是本帧的只读拷贝。
    //
    // `duration_ms` 的换算只在最终结果时进行（partial 一律 None）：有限且大于 0 的值
    // 向下取整，其余（含 NaN/无穷）给 0。`output_pad` 由 self-render 的渲染器自行应用。
    fn get_render_context(&self, last_component: Option<Box<dyn Component>>) -> ToolRenderContext {
        let invalidated = Arc::clone(&self.invalidated);
        let waker_id = self.waker_id;
        let ui_thread = self.ui_thread;
        let thread_request_render = Arc::clone(&self.thread_request_render);
        // TS: `invalidate: () => { this.invalidate(); this.ui.requestRender(); }`。
        // 同线程（渲染器在 UI 线程调用）用线程局部注册表找回 `Rc` 重绘回调；后台线程
        // （`tool_timer_text` 的 setInterval 替身）只在宿主注入线程安全通道时重绘。
        let invalidate: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            invalidated.store(true, Ordering::SeqCst);
            if std::thread::current().id() == ui_thread {
                let waker = INVALIDATE_WAKERS
                    .try_with(|wakers| wakers.borrow().get(&waker_id).cloned())
                    .ok()
                    .flatten();
                if let Some(waker) = waker {
                    waker();
                }
            } else {
                let waker = thread_request_render
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                if let Some(waker) = waker {
                    waker();
                }
            }
        });
        ToolRenderContext {
            args: self.args.clone(),
            tool_call_id: self.tool_call_id.clone(),
            invalidate,
            last_component,
            state: Arc::clone(&self.renderer_state),
            cwd: self.cwd.clone(),
            execution_started: self.execution_started,
            args_complete: self.args_complete,
            is_partial: self.is_partial,
            expanded: self.expanded.get(),
            show_images: self.show_images,
            is_error: self.result.as_ref().map(|r| r.is_error).unwrap_or(false),
            duration_ms: if self.is_partial {
                None
            } else {
                self.result
                    .as_ref()
                    .and_then(|result| result.duration_ms)
                    .map(|ms| {
                        if ms.is_finite() && ms > 0.0 {
                            ms.floor() as u64
                        } else {
                            0
                        }
                    })
            },
            output_pad: self.output_pad,
        }
    }

    // 把渲染器返回的组件包进点击区域。`cacheable` 决定下一帧 `reclaim_cached_children`
    // 是否把 child 取回作为 `last_component`：只有渲染器成功返回的组件才缓存，兜底组件
    // 不缓存（与 TS 渲染失败时把缓存置 `undefined` 一致）。
    fn create_result_region(
        &self,
        component: Box<dyn Component>,
        slot: RegionSlot,
        cacheable: bool,
    ) -> Box<dyn Component> {
        Box::new(ResultRegion {
            child: Some(component),
            slot,
            cacheable,
            has_result: Rc::clone(&self.has_result),
            expanded: Rc::clone(&self.expanded),
            dirty: Rc::clone(&self.dirty),
        })
    }

    /// 从容器里取回上一帧渲染器返回的原始组件（TS 的 `callRendererComponent` /
    /// `resultRendererComponent`）。兜底组件不缓存（TS 只在渲染器成功时赋值，失败时置
    /// `undefined`）。
    ///
    /// 必须在 `Container::clear` **之前**调用：明确要清空容器，缓存组件是容器的子项，
    /// 先 clear 会把它们一并析构。同一槽位若出现多个可缓存区域，只有每槽第一个进入
    /// 缓存；后续同槽区域的子组件同样会被 take 出，但不进入缓存，随后随包装器一起
    /// 丢弃。
    fn reclaim_cached_children(
        children: &mut [Box<dyn Component>],
    ) -> (CachedComponent, CachedComponent) {
        let mut call = None;
        let mut result = None;
        for child in children {
            let Some(any) = child.as_any_mut() else {
                continue;
            };
            let Some(region) = any.downcast_mut::<ResultRegion>() else {
                continue;
            };
            if !region.cacheable {
                continue;
            }
            let taken = region.child.take();
            match region.slot {
                RegionSlot::Call => {
                    if call.is_none() {
                        call = taken;
                    }
                }
                RegionSlot::Result => {
                    if result.is_none() {
                        result = taken;
                    }
                }
            }
        }
        (call, result)
    }

    /// 注入跨线程 `invalidate` 的重绘通道（TS 的 `ui.requestRender()`）。
    ///
    /// `ToolRenderContext.invalidate` 由 `kiana-tools` 的计时线程每秒调用；Rust 的 UI 重绘
    /// 回调是 `!Send` 的 `Rc`，跨线程只能经由宿主提供的线程安全通道（例如向 UI 事件循环
    /// 投递消息）。未安装时后台 tick 只置位失效标志，下一次渲染应用。
    ///
    /// 通道放在共享槽里，因此对**已经交给渲染器的** `invalidate` 闭包立即生效；
    /// 安装/清除本身不请求重绘。
    pub fn set_thread_request_render(&mut self, request_render: Option<ThreadRequestRender>) {
        *self
            .thread_request_render
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = request_render;
    }

    // 兜底调用头：标题 + 参数（折叠时是 `key=value` 预览，展开时逐项多行），由
    // kiana-tools 的 `format_tool_call_with_args` 生成；外层包装为非缓存区域。
    fn create_call_fallback(&self, theme: &kiana_theme::theme::Theme) -> Box<dyn Component> {
        Box::new(Text::with_padding(
            format_tool_call_with_args(
                &self.tool_name,
                Some(&self.args),
                theme,
                self.expanded.get(),
            ),
            0,
            0,
        ))
    }

    // 兜底结果文本。输出为空时返回 None（调用方不添加结果子组件）；折叠时只取前
    // `FALLBACK_PREVIEW_LINES` 行，并在末尾追加 "... (N more lines, <展开快捷键>)"。
    // 行数按 `\n` 切分：末尾换行会产生一个空行并计入总数。
    fn create_result_fallback(
        &self,
        theme: &kiana_theme::theme::Theme,
    ) -> Option<Box<dyn Component>> {
        let output = self.get_text_output();
        if output.is_empty() {
            return None;
        }
        let lines: Vec<&str> = output.split('\n').collect();
        let display_lines: Vec<&str> = if self.expanded.get() {
            lines.clone()
        } else {
            lines.iter().take(FALLBACK_PREVIEW_LINES).copied().collect()
        };
        let remaining = lines.len() - display_lines.len();
        let mut text = display_lines
            .iter()
            .map(|line| theme.fg(ThemeColor::ToolOutput, line))
            .collect::<Vec<String>>()
            .join("\n");
        if remaining > 0 {
            text.push_str(&theme.fg(
                ThemeColor::Muted,
                &format!("\n... ({remaining} more lines,"),
            ));
            text.push(' ');
            text.push_str(&kiana_ui_kit::key_hint("app.tools.expand", "to expand"));
            text.push_str(&theme.fg(ThemeColor::Muted, ")"));
        }
        Some(Box::new(Text::with_padding(text, 0, 0)))
    }

    /// `updateArgs(args)`：**不**立即重建，下一次渲染帧应用（#9265）。
    ///
    /// 只覆盖 `args` 并置 `args_pending`，随后请求重绘；`render` 看到 `args_pending`
    /// 才调用 `update_display`（其它立即重建的 setter 也会顺带应用新参数）。因此连续 N 次
    /// 流式更新最多触发一次渲染器调用
    /// （测试 `renders_streamed_partial_arguments_once_per_frame_with_the_latest_arguments`）。
    pub fn update_args(&mut self, args: JsonValue) {
        self.args = args;
        self.args_pending = true;
        (self.request_render)();
    }

    /// `markExecutionStarted()`：置 `execution_started`，立即重建并请求重绘。
    ///
    /// 渲染器从 [`ToolRenderContext::execution_started`] 读到该状态；例如运行期间显示
    /// 计时的渲染器要等这里置位后才开始计时。
    pub fn mark_execution_started(&mut self) {
        self.execution_started = true;
        self.update_display();
        (self.request_render)();
    }

    /// `setArgsComplete()`：置 `args_complete`，立即重建并请求重绘。
    ///
    /// 渲染器从 [`ToolRenderContext::args_complete`] 读到该状态，用来区分「参数仍在
    /// 流式累积」与「参数已完整」。
    pub fn set_args_complete(&mut self) {
        self.args_complete = true;
        self.update_display();
        (self.request_render)();
    }

    /// `updateResult(result, isPartial = false)`：立即重建。
    ///
    /// 保存结果、更新 `is_partial`，并把 `has_result` 置真（点击展开从此刻起才生效）。
    /// **不**调用重绘回调，调用方需要自行请求渲染。`is_partial = true` 时渲染器收到的
    /// `duration_ms` 恒为 `None`；只有最终结果才会换算耗时。
    pub fn update_result(&mut self, result: ToolExecutionResult, is_partial: bool) {
        self.result = Some(result);
        self.is_partial = is_partial;
        self.has_result.set(true);
        self.update_display();
    }

    /// `setExpanded(expanded)`：设置展开状态并立即重建。
    ///
    /// 只重建组件树，不请求重绘；调用方（鼠标点击路径由 `ResultRegion` /
    /// `ContentTextRegion` 置 `dirty`，程序化调用在 `InteractiveMode` 里）负责让
    /// 下一帧发生。
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded.set(expanded);
        self.update_display();
    }

    /// `setOutputPad(outputPad)`：更新水平内缩并立即重建。
    ///
    /// 只重建组件树，不请求重绘。对 `RenderShell::Default` 生效（内容容器的
    /// `padding_x`）；`RenderShell::SelfRender` 的渲染器自行应用内缩。
    pub fn set_output_pad(&mut self, output_pad: usize) {
        self.output_pad = output_pad;
        self.update_display();
    }

    /// `setShowImages(show)`：切换图片显示并立即重建。
    ///
    /// 只重建组件树，不请求重绘。为 false 时结果中的图片不会创建 `Image` 组件。
    pub fn set_show_images(&mut self, show: bool) {
        self.show_images = show;
        self.update_display();
    }

    /// `setImageWidthCells(width)`：`Math.max(1, Math.floor(width))`。
    ///
    /// 只重建组件树，不请求重绘。非有限值（NaN/∞）按 1 处理；结果存入
    /// `image_width_cells`，下次重建图片时作为 `max_width_cells`。
    pub fn set_image_width_cells(&mut self, width: f64) {
        self.image_width_cells = if width.is_finite() {
            width.floor().max(1.0)
        } else {
            1.0
        };
        self.update_display();
    }

    /// 当前展开状态。
    pub fn is_expanded(&self) -> bool {
        self.expanded.get()
    }

    /// `showImages`（TS 的公开字段）。
    pub fn show_images(&self) -> bool {
        self.show_images
    }

    /// `imageWidthCells`（TS 的公开字段）。
    pub fn image_width_cells(&self) -> f64 {
        self.image_width_cells
    }

    /// `outputPad`（TS 的公开字段）。
    pub fn output_pad(&self) -> usize {
        self.output_pad
    }

    // 结果中的纯文本输出：把内部结果转成 `AgentToolResult` 后复用 kiana-tools 的
    // `get_text_output`（剥 ANSI、清洗二进制）。终端不支持图片或 `show_images` 为
    // false 时，图片会被替换成占位行；只有终端支持图片且 `show_images` 为真时才不
    // 生成占位行，此时图片由组件自己渲染。
    fn get_text_output(&self) -> String {
        let result = self
            .result
            .as_ref()
            .map(ToolExecutionResult::to_agent_result);
        get_text_output(result.as_ref(), self.show_images)
    }

    // 无 toolDefinition 时的整行文本：标题 + 缩进 2 空格的 pretty JSON 参数 + 文本输出。
    // 该文本整体放进 content_text（可点击展开的 ContentTextRegion），不做按行折叠。
    fn format_tool_execution(&self, theme: &kiana_theme::theme::Theme) -> String {
        let mut text = theme.fg(ThemeColor::ToolTitle, &theme.bold(&self.tool_name));
        let content = kiana_json::js_object::stringify_pretty(&self.args, 2);
        if !content.is_empty() {
            text.push_str(&format!("\n\n{content}"));
        }
        let output = self.get_text_output();
        if !output.is_empty() {
            text.push_str(&format!("\n{output}"));
        }
        text
    }

    // 重建整棵展示树。步骤：清瞬时标志 → 按状态选背景 → 按「有/无渲染器定义」重建
    // 内容 → 重建图片。渲染器调用是同步的，全部发生在调用本函数的线程上。
    fn update_display(&mut self) {
        self.args_pending = false;
        self.dirty.set(false);
        self.invalidated.store(false, Ordering::SeqCst);

        // 背景色随状态切换：流式中 → pending；最终错误 → error；最终成功 → success。
        let bg_fn: kiana_tui::components::text_component::StyleFn = if self.is_partial {
            Arc::new(|text: &str| bg(ThemeBg::ToolPendingBg, text))
        } else if self.result.as_ref().map(|r| r.is_error).unwrap_or(false) {
            Arc::new(|text: &str| bg(ThemeBg::ToolErrorBg, text))
        } else {
            Arc::new(|text: &str| bg(ThemeBg::ToolSuccessBg, text))
        };

        // `has_content` 只在下面两个分支里赋值，而两个分支都会置 true。因此文件末尾的
        // `hide_component` 判定当前不会命中；self 渲染下「空结果不占布局」是由
        // `render_self_shell` 直接返回空 vec 实现的。
        let mut has_content: bool;
        self.hide_component = false;

        if self.has_renderer_definition() {
            // 有 toolDefinition：用渲染器槽位重建 call / result。self 外壳不套
            // content_box（不设背景与内缩；output_pad 按 ext-api 约定由 self 渲染器
            // 自行应用），default 外壳才套 content_box。
            let is_self = self.get_render_shell() == RenderShell::SelfRender;
            // 先取回上一帧渲染器返回的组件（TS 的 `callRendererComponent` /
            // `resultRendererComponent`），再清空容器。
            let (mut cached_call, mut cached_result) = if is_self {
                Self::reclaim_cached_children(&mut self.self_render_container.children)
            } else {
                Self::reclaim_cached_children(&mut self.content_box.children)
            };
            if is_self {
                self.self_render_container.clear();
            } else {
                self.content_box.set_bg_fn(Some(Arc::clone(&bg_fn)));
                self.content_box.set_padding_x(self.output_pad);
                self.content_box.clear();
            }

            let theme = Self::render_theme();
            // call 槽位：没有渲染器 → 兜底调用头；有渲染器 → 在 catch_unwind_silent 里
            // 调用。成功返回的组件标记为 cacheable，panic 则退回兜底且不缓存。
            let call_renderer = self
                .tool_definition
                .as_ref()
                .and_then(|definition| definition.render_call.clone());
            let call_component = match call_renderer {
                None => self.create_result_region(
                    self.create_call_fallback(theme.as_ref()),
                    RegionSlot::Call,
                    false,
                ),
                Some(renderer) => {
                    let rendered = {
                        let context = self.get_render_context(cached_call.take());
                        catch_unwind_silent(AssertUnwindSafe(|| {
                            renderer(&self.args, theme.as_ref(), &context)
                        }))
                    };
                    match rendered {
                        Ok(component) => {
                            self.create_result_region(component, RegionSlot::Call, true)
                        }
                        Err(_) => self.create_result_region(
                            self.create_call_fallback(theme.as_ref()),
                            RegionSlot::Call,
                            false,
                        ),
                    }
                }
            };
            if is_self {
                self.self_render_container.add_child(call_component);
            } else {
                self.content_box.add_child(call_component);
            }
            has_content = true;

            // result 槽位只在已有结果时创建。没有结果渲染器、渲染器 panic、或兜底文本为空
            // 时都可能没有结果子组件；此时只剩 call 组件和图片。
            if self.result.is_some() {
                let result_renderer = self
                    .tool_definition
                    .as_ref()
                    .and_then(|definition| definition.render_result.clone());
                let agent_result = self
                    .result
                    .as_ref()
                    .map(ToolExecutionResult::to_agent_result);
                let options = ToolRenderResultOptions {
                    expanded: self.expanded.get(),
                    is_partial: self.is_partial,
                };
                let result_component = match result_renderer {
                    None => self
                        .create_result_fallback(theme.as_ref())
                        .map(|component| {
                            self.create_result_region(component, RegionSlot::Result, false)
                        }),
                    Some(renderer) => {
                        let agent_result = agent_result
                            .as_ref()
                            .expect("result exists (checked above)");
                        let rendered = {
                            let context = self.get_render_context(cached_result.take());
                            catch_unwind_silent(AssertUnwindSafe(|| {
                                renderer(agent_result, &options, theme.as_ref(), &context)
                            }))
                        };
                        match rendered {
                            Ok(component) => {
                                Some(self.create_result_region(component, RegionSlot::Result, true))
                            }
                            Err(_) => {
                                self.create_result_fallback(theme.as_ref())
                                    .map(|component| {
                                        self.create_result_region(
                                            component,
                                            RegionSlot::Result,
                                            false,
                                        )
                                    })
                            }
                        }
                    }
                };
                if let Some(component) = result_component {
                    if is_self {
                        self.self_render_container.add_child(component);
                    } else {
                        self.content_box.add_child(component);
                    }
                    has_content = true;
                }
            }
        } else {
            // 无 toolDefinition：整行文本渲染（标题 + 参数 + 文本输出），背景与内缩直接
            // 设在可点击的 Text 上。
            let theme = Self::render_theme();
            self.content_text
                .borrow_mut()
                .set_custom_bg_fn(Some(Arc::clone(&bg_fn)));
            self.content_text
                .borrow_mut()
                .set_padding_x(self.output_pad);
            let text = self.format_tool_execution(theme.as_ref());
            self.content_text.borrow_mut().set_text(text);
            has_content = true;
        }

        // 图片重建（按索引复用已转码的 Image）。
        //
        // 索引是「第 N 张被包含的图片」而不是结果中的块序号：只有通过下面过滤条件的
        // 图片才 push 一个 Spacer 和（可能复用的）Image，因此 `image_spacers` 与
        // `image_components` 的下标一一对应。复用条件是 data、MIME、宽度全部相同。
        let mut previous_images: Vec<Option<Image>> = std::mem::take(&mut self.image_components)
            .into_iter()
            .map(Some)
            .collect();
        let previous_sources = std::mem::take(&mut self.image_sources);
        self.image_spacers.clear();

        if let Some(result) = self.result.as_ref() {
            let image_blocks: Vec<&kiana_ai::types::ImageContent> = result
                .content
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Image(image) => Some(image),
                    UserContentBlock::Text(_) => None,
                })
                .collect();
            let capabilities = get_capabilities();
            for block in image_blocks {
                // 终端不支持图片、用户关闭图片、或数据/MIME 为空时整张跳过，
                // 不创建 Spacer，也不占用复用索引。
                if capabilities.images.is_none()
                    || !self.show_images
                    || block.data.is_empty()
                    || block.mime_type.is_empty()
                {
                    continue;
                }
                self.image_spacers.push(Spacer::new());
                let source = ImageSource {
                    data: block.data.clone(),
                    mime_type: block.mime_type.clone(),
                    width_cells: self.image_width_cells,
                };
                let index = self.image_components.len();
                let previous_matches = previous_sources.get(index).is_some_and(|previous| {
                    previous.data == source.data
                        && previous.mime_type == source.mime_type
                        && previous.width_cells == source.width_cells
                });
                let image = if previous_matches {
                    previous_images.get_mut(index).and_then(|slot| slot.take())
                } else {
                    None
                };
                let image = image.unwrap_or_else(|| {
                    let fallback_color: kiana_tui::components::text_component::StyleFn =
                        Arc::new(|text: &str| fg(ThemeColor::ToolOutput, text));
                    Image::new(
                        source.data.clone(),
                        source.mime_type.clone(),
                        ImageTheme { fallback_color },
                        ImageOptions {
                            max_width_cells: Some(source.width_cells),
                            ..ImageOptions::default()
                        },
                        None,
                    )
                });
                if source.mime_type != "image/png" {
                    // 非 PNG 依赖 kiana-core 的 PNG 转码器；`ensure_png_transcoder` 在
                    // 非 Kitty 协议下直接返回、不注册转码器，只在「本次调用完成了注册」时
                    // 执行回调。回调把 `dirty` 置真，下一帧 `render` 因此重建；新建的
                    // `Image` 首次渲染即走新转码器，而按来源复用的 `Image` 仍持有按宽度
                    // 键控的渲染缓存，重建本身不会让它重新转码（需显式 `invalidate`，
                    // 见 `render` 的外部失效路径）。
                    let dirty = Rc::clone(&self.dirty);
                    ensure_png_transcoder(move || {
                        dirty.set(true);
                    });
                }
                self.image_components.push(image);
                self.image_sources.push(source);
            }
        }

        // 如函数开头所述，`has_content` 在两个分支里都置 true，所以这里当前不可达。
        if self.has_renderer_definition() && !has_content && self.image_components.is_empty() {
            self.hide_component = true;
        }
    }

    // Default 外壳的自顶向下顺序：Spacer → 内容（渲染器布局或整行文本）→ 每张图片的
    // Spacer + Image。图片 Spacer 与 Image 按下标配对；缺失 Spacer 时只渲染图片。
    fn render_default_shell(&mut self, width: usize) -> Vec<String> {
        let mut lines = self.spacer.render(width);
        if self.has_renderer_definition() {
            lines.extend(self.content_box.render(width));
        } else {
            lines.extend(self.content_text_region.render(width));
        }
        for index in 0..self.image_components.len() {
            if let Some(spacer) = self.image_spacers.get_mut(index) {
                lines.extend(spacer.render(width));
            }
            lines.extend(self.image_components[index].render(width));
        }
        lines
    }

    // SelfRender 外壳：先渲染工具自己的容器，记录行数供 handle_mouse 换算坐标；
    // 内容与图片都为空时返回空 vec（整行不占布局高度）。有内容时在内容前插一个空行，
    // 图片有内容但容器为空时不插这一行。
    fn render_self_shell(&mut self, width: usize) -> Vec<String> {
        let content_lines = self.self_render_container.render(width);
        self.self_render_height = content_lines.len();
        if content_lines.is_empty() && self.image_components.is_empty() {
            return Vec::new();
        }
        let mut lines = Vec::new();
        if !content_lines.is_empty() {
            lines.push(String::new());
            lines.extend(content_lines);
        }
        for index in 0..self.image_components.len() {
            if let Some(spacer) = self.image_spacers.get_mut(index) {
                lines.extend(spacer.render(width));
            }
            lines.extend(self.image_components[index].render(width));
        }
        lines
    }
}

// 组件销毁时从当前线程的注册表移除自己的 id。用 `try_with` 是因为线程退出阶段
// thread-local 可能已销毁，此时没有可清理的注册表。组件含 `Rc`，是 `!Send`，
// 因此它只会在注册它的那条线程上被 drop。
impl Drop for ToolExecutionComponent {
    fn drop(&mut self) {
        let waker_id = self.waker_id;
        let _ = INVALIDATE_WAKERS.try_with(|wakers| {
            wakers.borrow_mut().remove(&waker_id);
        });
    }
}

/// `ToolDefinition` → `ToolRenderers`（取三个渲染字段）。
///
/// `render_shell` 直接复制（`Option<RenderShell>` 是 `Copy`，缺省时组件按
/// [`RenderShell::Default`] 处理），call/result 只克隆 `Arc`，不复制闭包。
///
/// 三个字段**整体生效**，本函数不与内置工具的渲染器逐字段合并；「只覆盖一个槽位、
/// 其余沿用内置」由 `kiana-tools` 的 `with_built_in_renderers` 在把 `ToolDefinition`
/// 转成 `ToolRenderers` 交给组件之前完成（见 `interactive_mode/session.rs` 的
/// `get_registered_tool_definition` 与测试
/// `inherits_missing_built_in_result_renderer_slot_from_the_built_in_tool`）。
pub fn renderers_from_definition(
    definition: &kiana_ext_api::tool_types::ToolDefinition,
) -> ToolRenderers {
    ToolRenderers {
        render_shell: definition.render_shell,
        render_call: definition.render_call.clone(),
        render_result: definition.render_result.clone(),
    }
}

impl Component for ToolExecutionComponent {
    /// 允许宿主按具体类型下转（TS 的 `instanceof ToolExecutionComponent`）。
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    // 渲染前的延迟重建有两类触发：
    // - `args_pending`：`update_args` 累积的参数在这里一次性应用，每个渲染帧最多调用
    //   一次渲染器；
    // - `dirty`（点击展开、PNG 转码器就绪）或外部 `invalidate`：整棵展示树重建。
    //   外部失效时先显式失效每张 Image：`update_display` 会按来源复用 Image 实例，
    //   不清掉它的渲染缓存就不会重新走转码/图片协议路径。
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.args_pending {
            self.update_display();
        }
        // `ToolRenderContext.invalidate` 在 TS 里调用 `this.invalidate()`：递归失效子组件
        // （`updateDisplay` 会复用未变来源的 Image，必须清掉它的渲染缓存）后再重建。
        let externally_invalidated = self.invalidated.swap(false, Ordering::SeqCst);
        if self.dirty.get() || externally_invalidated {
            if externally_invalidated {
                for image in &mut self.image_components {
                    image.invalidate();
                }
            }
            self.update_display();
        }
        if self.hide_component {
            return Vec::new();
        }
        if self.has_renderer_definition() && self.get_render_shell() == RenderShell::SelfRender {
            return self.render_self_shell(width);
        }
        self.render_default_shell(width)
    }

    // 鼠标坐标以本组件渲染出的第 0 行为起点，转发给内容容器时要做行偏移：
    // - Default 外壳：第 0 行是 Spacer，先减 1；内容高度由一次现场 `render` 求出
    //   （宽度可能自上次渲染后变化），超出高度则不转发。
    // - SelfRender 外壳：内容非空时 `render_self_shell` 在内容前插入一个空行，
    //   所以 y = 0 不对应可点击内容；y >= 1 映射到容器第 y-1 行。`self_render_height`
    //   是上一帧内容行数，不含图片部分。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        let is_self =
            self.has_renderer_definition() && self.get_render_shell() == RenderShell::SelfRender;
        if is_self {
            if event.y <= 0 || event.y as usize > self.self_render_height {
                return None;
            }
            return self.self_render_container.handle_mouse(&TuiMouseEvent {
                y: event.y - 1,
                height: self.self_render_height,
                ..event.clone()
            });
        }

        // Container 布局：Spacer(1) 之后是 shell。
        if event.y < 1 {
            return None;
        }
        let mut shell_event = event.clone();
        shell_event.y = event.y - 1;
        if self.has_renderer_definition() {
            let height = self.content_box.render(event.width).len();
            if (shell_event.y as usize) >= height {
                return None;
            }
            shell_event.height = height;
            self.content_box.handle_mouse(&shell_event)
        } else {
            let height = self.content_text_region.render(event.width).len();
            if (shell_event.y as usize) >= height {
                return None;
            }
            shell_event.height = height;
            self.content_text_region.handle_mouse(&shell_event)
        }
    }

    fn invalidate(&mut self) {
        // TS `super.invalidate()`（Container）会递归失效所有子组件，包括被
        // updateDisplay 复用、因此不会重建的 Image 与 Spacer。
        //
        // 这里失效完子组件后立刻整体重建并请求重绘；与 `render` 里的外部失效路径不同，
        // 此处不经过 `dirty` 标志，所以即便组件未被挂到树上也会重跑渲染器。
        self.content_box.invalidate();
        self.content_text.borrow_mut().invalidate();
        self.content_text_region.invalidate();
        self.self_render_container.invalidate();
        for image in &mut self.image_components {
            image.invalidate();
        }
        for spacer in &mut self.image_spacers {
            spacer.invalidate();
        }
        self.update_display();
        (self.request_render)();
    }

    fn handles_input(&self) -> bool {
        false
    }
}
