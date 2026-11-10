//! `interactive-mode.ts` 的扩展 widgets / status / header / footer（§2.50.18–§2.50.20）。
//!
//! 状态指示器、扩展状态行、editor 内嵌状态、widget 容器、自定义 footer/header、
//! 终端输入监听器与扩展快捷键。
//!
//! # 这个文件负责什么
//!
//! 它是交互模式里所有“扩展可替换 UI”的落点：扩展 API（`ExtensionUiContext`）的
//! `setWidget` / `setFooter` / `setHeader` / `setStatus` / `setWorkingVisible` /
//! `setWorkingIndicator` / `onTerminalInput` / `setToolsExpanded` 等请求，经
//! `extension_ui` 的 UI 线程 drain 任务转换后，最终都调用到本文件的
//! `impl InteractiveMode`。除了这些入口，它也维护下列状态的内部生命周期：
//!
//! - 活动状态指示器（working / compaction / retry / branchSummary）究竟内嵌在编辑器边框，
//!   还是作为独立状态行挂在 `status_container`；
//! - 扩展 widget 两个容器（编辑器上方/下方）与自定义 footer/header 的插件式替换；
//! - 扩展终端输入监听器与扩展快捷键在渲染器切换后的重新绑定；
//! - 工具输出展开、thinking 可见性这类“全局显示开关”向已渲染组件的批量分发。
//!
//! # 为什么需要这些代理类型
//!
//! TS 里一个组件就是对象引用，任何人都能拿同一个实例调用方法。Rust 侧渲染树保存的是
//! `Box<dyn Component>`，宿主手里是 `Rc<RefCell<T>>`，两者之间需要显式代理：
//! `SharedComponentProxy`（`tui_renderer`）负责容器 → 共享句柄，`OwnedComponent` 负责
//! 工厂返回的独占 `Box` → 共享句柄，`IndicatorHandle` / `IndicatorProxy` 让“记录在
//! `active_status_indicator` 里的指示器”与“渲染树里的指示器”是同一个实例。
//! `IndicatorCell` 是这条共享链的枢纽：`active_status_indicator` 的类型被冻结为
//! `Box<dyn StatusIndicator>`，无法从中取回句柄，换编辑器时只能靠线程局部的
//! `ACTIVE_INDICATOR_CELL` 取回同一实例。
//!
//! # 三张按“身份”索引的注册表
//!
//! TS 用 `instanceof` / `"setExpanded" in child` 做结构判定，Rust 的 trait 对象做不到，
//! 因此本文件维护三张线程局部表：
//!
//! - `EXPANDABLE_SINKS`：可展开组件的弱引用 + 方法指针，`setToolsExpanded` 用它同步
//!   已加入聊天区的工具行、摘要、bash 行等；
//! - `ASSISTANT_COMPONENTS`：已渲染 assistant 组件的弱引用列表，用于批量修改 thinking
//!   可见性与折叠占位文案；
//! - `TERMINAL_INPUT_IDS`：终端输入监听器的 TUI id（订阅表里的 `unsubscribe` 必须
//!   `Send + Sync`，拿不到渲染器，所以 id 单独放在这里）。
//!
//! 三张表都只在 UI 线程（LocalSet）上读写。失效项的清理时机按表区分：
//! `EXPANDABLE_SINKS` 的失效项会在批量更新（`apply_expanded_to_registered` 的
//! `retain_mut`）时清理；`ASSISTANT_COMPONENTS` 的失效项只在下次
//! `register_assistant_component` 登记时清理，批量读取（`for_each_assistant_component`）
//! 不会移除它们。持有组件的地方用的是 `Weak`，因此组件从聊天区移除后不会被注册表拖住。
//!
//! # 接口边界
//!
//! - 上游：`extension_ui` 的 `UiRequest` 处理器（`set_widget` / `set_footer` / ...）以及
//!   `interactive_mode.rs` 的初始化/关停路径（`mount_interactive_tui`、
//!   `switch_tui_mode`、`stop_with`）。
//! - 下游：`kiana-tui` 的组件与容器、`kiana-ext-api` 的工厂/处理器 trait 对象、
//!   `kiana-resources` 的 footer 数据提供者、`kiana-extensions` 的 runner（扩展快捷键）。
//! - 本文件不直接读终端、不拼接转义序列；所有终端能力都经 `InteractiveTuiReference`。
//!
//! # 重要约束
//!
//! - 所有方法都要求 `&mut InteractiveMode` 且只能在 UI 线程调用；内部只做短时间的
//!   `inner.borrow()` / `borrow_mut()`。不要在组件回调或 `.await` 期间重入这些方法。
//! - `Container::clear()` 只丢弃子项代理，不会调用组件的 `dispose`；替换/清除 widget、
//!   footer、header 时必须显式 `dispose`，否则 loader、watcher 这类后台资源不会停止。
//! - 若干“值未变化”的早退分支（`set_tools_expanded`、`clear_status_indicator` 的 kind
//!   过滤、`set_extension_header` 在 header 尚未建立时）不会触发重绘或状态提示。
//! - 工厂与主题相关的辅助函数在主题未初始化时会 `panic`，对应 TS 在同样位置的调用错误。
//!
//! # 容器布局与替换顺序
//!
//! - widget 容器按 `IndexMap` 的插入顺序渲染；上方容器为空时放一个 `Spacer` 占位，
//!   非空时改为在组件前放一个 `Spacer`；下方容器两种情况都不放
//!   （`render_widget_container`）。
//! - 替换/清除 widget、footer、header 时先 `dispose` 旧组件再 `clear` 容器：
//!   `Container::clear` 只丢弃子项代理，不会触发组件的 `dispose`。
//! - header 容器在内置 header 存在时是 `[Spacer, header, Spacer]`；静默启动的
//!   `Empty` 形态只有一个空 `Text`。自定义 header 直接替换该槽位，移除时按共享句柄
//!   的身份（指针相等）放回内置 header。
//!
//! # 状态指示器的挂载状态机
//!
//! 1. `show_status_indicator` 接收新指示器：`dispose` 旧的，放进 `IndicatorCell`，让
//!    `active_status_indicator` 持有一个 `IndicatorHandle`；
//! 2. 清空 `status_container` 与默认编辑器上的旧内嵌指示器；
//! 3. 默认编辑器活动（`editor_supports_embedded_status`）时内嵌到边框并置
//!    `active_working_indicator_embedded = true`，否则把 `IndicatorProxy` 加进状态容器；
//! 4. `clear_status_indicator` 按当前 kind 过滤后取走并 `dispose`，必要时补一个
//!    `IdleStatus` 占位行（regular + clearOnShrink + 之前是独立状态行）；
//! 5. 扩展编辑器替换完成后，`set_custom_editor_component` 调用
//!    `reattach_active_status_indicator`，用同一实例在新编辑器/状态栏之间重新选择挂载点。
//!
//! # 阅读顺序
//!
//! 模块级辅助（`fg`、三个 thread_local、代理类型）→ `IndicatorHandle` /
//! `apply_expanded_to_component` → `impl InteractiveMode` 里的扩展入口 →
//! 文件尾部的 `#[doc(hidden)]` 测试接缝。

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use kiana_ext_api::ui::{
    ExtensionWidgetOptions, HeaderFactory, ReadonlyFooterDataProvider, TerminalInputHandler,
    WidgetContent, WidgetFactory,
};
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::loader::{LoaderIndicatorOptions, RequestRenderCallback};
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::{StyleFn, Text};
use kiana_tui::tui::{
    Component, Container, Focusable, OverlayHandle, OverlayOptions, Tui, TuiMode, TuiMouseEvent,
    TuiMouseEventResult, TuiStopOptions,
};
use kiana_ui_kit::themed_text::ThemedText;

use crate::components::assistant_message::AssistantMessageComponent;
use crate::components::status_indicator::{IdleStatus, StatusIndicator, StatusIndicatorKind};
use crate::tui_renderer::{InteractiveTuiReference, SharedComponentProxy};

use super::super::{
    Expandable, HeaderComponent, InteractiveMode, MAX_WIDGET_LINES, SharedComponent,
};

/// 用全局主题给 `text` 着色（本文件只用于 widget 的截断提示行）。
///
/// 主题未初始化时 `THEME.fg` 返回 `Err`（`ThemeProxy::fg` 的返回类型是
/// `Result<String, ThemeError>`），这里 `expect` panic；调用方必须保证启动流程
/// 已初始化主题。`extension_ui` 模块里有同约定的同名辅助函数。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

// ---------------------------------------------------------------------------
// 渲染树里共享状态指示器的两种代理
// ---------------------------------------------------------------------------

/// 状态指示器的共享单元。
///
/// `IndicatorHandle`（存进 `active_status_indicator`，供宿主调用状态指示器方法）与
/// `IndicatorProxy`（挂进 `status_container` 或编辑器边框，供渲染树调用）都借用它，
/// 因此两处看到并修改的是同一个实例。
type IndicatorCell = Rc<RefCell<Box<dyn StatusIndicator>>>;

thread_local! {
    /// 当前活动状态指示器的共享单元。
    ///
    /// `active_status_indicator` 的类型是冻结的 `Box<dyn StatusIndicator>`，无法从中取回
    /// 共享句柄；换编辑器时（`setCustomEditorComponent`）需要把**同一个**指示器重新挂到
    /// 新编辑器或状态栏，因此在这里保存单元格。（只在 UI 线程读写。）
    static ACTIVE_INDICATOR_CELL: RefCell<Option<IndicatorCell>> = const { RefCell::new(None) };
}

/// 设置（或清除）当前活动指示器的共享单元。
///
/// 只在 `show_status_indicator`（安装新指示器）与 `clear_status_indicator`
///（确实清掉了指示器时）调用；传 `None` 表示当前没有活动指示器。
fn store_active_indicator_cell(cell: Option<IndicatorCell>) {
    ACTIVE_INDICATOR_CELL.with(|slot| *slot.borrow_mut() = cell);
}

/// 读取当前活动指示器的共享单元；没有活动指示器（或不在 UI 线程）时为 `None`。
fn active_indicator_cell() -> Option<IndicatorCell> {
    ACTIVE_INDICATOR_CELL.with(|slot| slot.borrow().clone())
}

/// 让 `active_status_indicator` 与渲染树共享同一个指示器实例。
///
/// 存进 `InteractiveModeInner::active_status_indicator` 的 `Box<dyn StatusIndicator>`
/// 是这个句柄而不是指示器本体。所有方法（含 `dispose`、`set_indicator`、重绘回调的
/// 安装）都转发给共享单元，因此宿主对活动指示器的操作会作用到真正挂在编辑器边框或
/// `status_container` 里的那个实例；反之渲染树触发的 `invalidate` 也作用于同一对象。
struct IndicatorHandle(IndicatorCell);

impl Component for IndicatorHandle {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }
    fn handles_input(&self) -> bool {
        false
    }
    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

impl StatusIndicator for IndicatorHandle {
    fn kind(&self) -> StatusIndicatorKind {
        self.0.borrow().kind()
    }
    fn set_message(&mut self, message: &str) {
        self.0.borrow_mut().set_message(message);
    }
    fn render_in_border(&mut self, width: usize) -> String {
        self.0.borrow_mut().render_in_border(width)
    }
    fn render_spinner_in_border(&mut self, width: usize) -> String {
        self.0.borrow_mut().render_spinner_in_border(width)
    }
    fn dispose(&mut self) {
        StatusIndicator::dispose(&mut **self.0.borrow_mut());
    }
    fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>) {
        self.0.borrow_mut().set_indicator(indicator);
    }
    fn set_timer(&mut self, timer: Box<dyn kiana_tui::tui::Timer>) {
        self.0.borrow_mut().set_timer(timer);
    }
    fn set_countdown_timer(&mut self, timer: Box<dyn kiana_tui::tui::Timer>) {
        self.0.borrow_mut().set_countdown_timer(timer);
    }
    fn set_request_render(&mut self, request_render: Option<RequestRenderCallback>) {
        self.0.borrow_mut().set_request_render(request_render);
    }
}

/// 渲染树里的代理（与 `IndicatorHandle` 共享同一实例）。
///
/// 只实现 `Component`：状态容器/编辑器边框只需要渲染与输入转发。`StatusIndicator`
/// 的方法（`set_message`、`set_indicator` 等）由 `active_status_indicator` 里的
/// `IndicatorHandle` 调用，两条路径最终落到同一个单元。
struct IndicatorProxy(IndicatorCell);

impl Component for IndicatorProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }
    fn handles_input(&self) -> bool {
        false
    }
    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

/// 对具体组件调用 `setExpanded`（TS `isExpandable` 的 Rust 对应物）。
///
/// `Component` 没有统一的展开接口，只能依次 `downcast_mut` 到本仓库实现了
/// `set_expanded` 的具体类型（清单见函数体）。返回 `true` 表示 `any` 命中其中一种，
/// 已经调用过 `set_expanded`；返回 `false` 表示不是可展开组件，调用方可以继续尝试
/// 其它包装形态（例如 `SharedComponentProxy`）。
///
/// 新增可展开组件类型时要把类型加进宏调用的清单，否则 `setToolsExpanded` 会静默漏掉它。
fn set_expanded_on_any(any: &mut dyn std::any::Any, expanded: bool) -> bool {
    macro_rules! try_component {
        ($($ty:ty),* $(,)?) => {
            $(
                if let Some(component) = any.downcast_mut::<$ty>() {
                    component.set_expanded(expanded);
                    return true;
                }
            )*
        };
    }
    try_component!(
        super::super::ExpandableText,
        crate::components::tool_execution::ToolExecutionComponent,
        crate::components::custom_entry::CustomEntryComponent,
        crate::components::custom_message::CustomMessageComponent,
        crate::components::compaction_summary_message::CompactionSummaryMessageComponent,
        crate::components::branch_summary_message::BranchSummaryMessageComponent,
        crate::components::skill_invocation_message::SkillInvocationMessageComponent,
        crate::components::bash_execution::BashExecutionComponent,
    );
    false
}

/// 对容器子项应用展开状态：直接可展开的类型，或包装了共享句柄的代理。
///
/// TS 的 `isExpandable(child)` 是结构判定（任何带 `setExpanded` 的对象），包括
/// `SharedComponentProxy` 包住的 `ToolExecutionComponent` 等共享组件。
///
/// 尝试顺序：`as_any_mut` 直接下转 → `SharedComponentProxy`（容器里的共享句柄代理）→
/// `OwnedComponent`（widget 工厂返回值的包装，递归解开）。`Component::as_any_mut` 默认
/// 返回 `None` 的纯渲染组件会直接得到 `false`。
///
/// 注意 `SharedComponentProxy` 的 `try_borrow_mut` 失败（组件正在渲染或被宿主借用）时
/// 返回 `false` 并**跳过本次更新**，不会排队重试。
fn apply_expanded_to_component(component: &mut dyn Component, expanded: bool) -> bool {
    let Some(any) = component.as_any_mut() else {
        return false;
    };
    if set_expanded_on_any(any, expanded) {
        return true;
    }
    if let Some(proxy) = any.downcast_mut::<SharedComponentProxy>() {
        let Ok(mut inner) = proxy.component().try_borrow_mut() else {
            return false;
        };
        return inner
            .as_any_mut()
            .is_some_and(|any| set_expanded_on_any(any, expanded));
    }
    if let Some(owned) = any.downcast_mut::<OwnedComponent>() {
        return apply_expanded_to_component(owned.0.as_mut(), expanded);
    }
    false
}

/// 已登记的可展开组件：闭包内部持有弱引用，返回 `false` 表示组件已释放，
/// `apply_expanded_to_registered` 据此把该条目从表中移除。
type ExpandableSink = Box<dyn Fn(bool) -> bool>;

thread_local! {
    /// 可展开组件注册表。
    ///
    /// `Component` trait 没有 `as_any`（`ToolExecutionComponent` 等具体类型也不实现），
    /// 无法像 TS 的 `"setExpanded" in child` 那样从 `&mut dyn Component` 结构判定。
    /// 组件在**加入 chat 时**登记；`setToolsExpanded` 遍历存活项调用 `setExpanded`。
    ///
    /// 表项是弱引用：组件从聊天区移除并被释放后，下一次 `apply_expanded_to_registered`
    /// 会把这些条目 retain 掉。登记本身不做去重，同一组件重复登记会让 `set_expanded`
    /// 被调用多次。
    static EXPANDABLE_SINKS: RefCell<Vec<ExpandableSink>> = const { RefCell::new(Vec::new()) };
}

/// 登记一个可展开组件。
///
/// 只保存 `Rc::downgrade` 与 `apply` 方法指针，不延长组件生命周期；`apply` 由调用方传入
/// 该类型自己的 `set_expanded`（例如 `ToolExecutionComponent::set_expanded`），避免在
/// trait 对象上做反射。
///
/// 登记后组件被可变借用（`try_borrow_mut` 失败）时，本次批量更新会跳过它，但登记仍保留
/// ——闭包仍返回 `true`，不会把它误判成已释放。
pub(crate) fn register_expandable<T: 'static>(component: &Rc<RefCell<T>>, apply: fn(&mut T, bool)) {
    let weak = Rc::downgrade(component);
    EXPANDABLE_SINKS.with(|sinks| {
        sinks.borrow_mut().push(Box::new(move |expanded| {
            let Some(component) = weak.upgrade() else {
                return false;
            };
            if let Ok(mut component) = component.try_borrow_mut() {
                apply(&mut component, expanded);
            }
            true
        }));
    });
}

/// 对所有仍存活的可展开组件应用展开状态。
///
/// `retain_mut` 在遍历的同时调用闭包：返回 `true` 的条目保留，返回 `false`（弱引用无法
/// 升级）的条目被移除。闭包执行期间持有登记表的 `borrow_mut`，因此 `apply` 内不能再次
/// 调用本函数或 `register_expandable`（`RefCell` 会 panic）。
pub(crate) fn apply_expanded_to_registered(expanded: bool) {
    EXPANDABLE_SINKS.with(|sinks| {
        let mut sinks = sinks.borrow_mut();
        sinks.retain_mut(|sink| sink(expanded));
    });
}

// ---------------------------------------------------------------------------
// 共享 Box<dyn Component> 的容器代理
// ---------------------------------------------------------------------------

/// 把工厂返回的 `Box<dyn Component>` 变成可共享句柄。
///
/// 扩展工厂返回独占所有权的 `Box`，而容器与宿主之间共享的是
/// `Rc<RefCell<dyn Component>>`（`kiana_tui::layout_node::SharedComponent`）。本类型实现
/// `Component` 并逐项转发，使包装后的句柄与原组件行为一致：渲染、输入、鼠标、焦点、
/// `dispose`、`width`、容器子项遍历，以及 `as_any` / `as_any_mut` 下转。
///
/// `as_any` 必须转发：`setToolsExpanded` 与设置页的批量更新都靠它拿到内部具体类型；
/// 内层组件不返回 `Some(self)` 时（`Component` 的默认实现），下转就会失败。
pub(crate) struct OwnedComponent(pub(crate) Box<dyn Component>);

impl Component for OwnedComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.render(width)
    }
    fn handle_input(&mut self, data: &str) {
        self.0.handle_input(data);
    }
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.handle_mouse(event)
    }
    fn wants_key_release(&self) -> bool {
        self.0.wants_key_release()
    }
    fn handles_input(&self) -> bool {
        self.0.handles_input()
    }
    fn invalidate(&mut self) {
        self.0.invalidate();
    }
    /// 透明转发 TS 的可选 `dispose()`（宿主替换/清空组件时调用）。
    fn dispose(&mut self) {
        self.0.dispose();
    }
    /// 透明转发 TS 的 `component.width` 属性。
    fn width(&self) -> Option<f64> {
        self.0.width()
    }
    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        self.0.container_children()
    }
    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        self.0.container_children_mut()
    }
    fn for_each_container_child(&self, f: &mut dyn FnMut(&dyn Component) -> bool) {
        self.0.for_each_container_child(f);
    }
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        self.0.as_focusable()
    }
    fn mouse_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        self.0.mouse_children_mut()
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        // 透明转发：允许 `setToolsExpanded` 等按具体类型下转（与 TS 的 `instanceof` 等价）。
        self.0.as_any()
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        self.0.as_any_mut()
    }
}

/// 把 `Box<dyn Component>` 变成 `Rc<RefCell<dyn Component>>` 共享句柄。
///
/// 句柄之外仍会有容器里的 `SharedComponentProxy` 持有同一个 `Rc`，因此宿主可以在组件
/// 已挂进渲染树后继续修改它（对应 TS 的引用语义）。
pub(crate) fn shared_from_boxed(component: Box<dyn Component>) -> SharedComponent {
    Rc::new(RefCell::new(OwnedComponent(component)))
}

/// `Arc<dyn Tui>` 适配：扩展工厂拿到的是渲染器的稳定句柄。
///
/// 实现 `Component` 与 `Tui` 两套方法并全部转发给 `InteractiveTuiReference`。转发目标是
/// “当前渲染器”而不是某个快照：`InteractiveRenderer` 被包在
/// `Rc<RefCell<...>>` 里，`switch_tui_mode` 原地替换其内容，已经交给扩展的句柄仍然指向
/// 同一渲染器槽位。`Arc` 只用于满足冻结的扩展 API 签名，不表示可以跨线程使用。
pub(crate) struct TuiArcAdapter {
    reference: InteractiveTuiReference,
}

impl TuiArcAdapter {
    /// 包装一个渲染器引用。
    pub(crate) fn new(reference: InteractiveTuiReference) -> Self {
        TuiArcAdapter { reference }
    }
}

impl Component for TuiArcAdapter {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.reference.render(width)
    }
    fn handle_input(&mut self, data: &str) {
        self.reference.handle_input(data)
    }
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.reference.handle_mouse(event)
    }
    fn handles_input(&self) -> bool {
        self.reference.handles_input()
    }
    fn invalidate(&mut self) {
        self.reference.invalidate()
    }
}

impl Tui for TuiArcAdapter {
    fn mode(&self) -> TuiMode {
        self.reference.mode()
    }
    fn children(&mut self) -> &mut Vec<Box<dyn Component>> {
        panic!("TuiArcAdapter::children is unavailable; use with_children()/children_len()")
    }
    fn children_len(&mut self) -> usize {
        self.reference.children_len()
    }
    fn with_children(&mut self, f: &mut dyn FnMut(&mut Vec<Box<dyn Component>>)) {
        self.reference.with_children(f);
    }
    fn add_child(&mut self, child: Box<dyn Component>) {
        self.reference.add_child(child)
    }
    fn remove_child(&mut self, child: &dyn Component) {
        self.reference.remove_child(child)
    }
    fn remove_child_at(&mut self, index: usize) -> Option<Box<dyn Component>> {
        self.reference.remove_child_at(index)
    }
    fn clear_children(&mut self) {
        Tui::clear_children(&mut self.reference)
    }
    fn focus(&self) -> Option<kiana_tui::tui::FocusTarget> {
        self.reference.focus()
    }
    fn set_focus(&mut self, target: Option<kiana_tui::tui::FocusTarget>) {
        self.reference.set_focus(target)
    }
    fn show_overlay(
        &mut self,
        component: Box<dyn Component>,
        options: OverlayOptions,
    ) -> Box<dyn OverlayHandle> {
        self.reference.show_overlay(component, options)
    }
    fn hide_overlay(&mut self) {
        self.reference.hide_overlay()
    }
    fn has_overlay(&self) -> bool {
        self.reference.has_overlay()
    }
    fn start(&mut self) {
        self.reference.start()
    }
    fn stop(&mut self, options: TuiStopOptions) {
        self.reference.stop(options)
    }
    fn render_now(&mut self, force: bool) {
        self.reference.render_now(force)
    }
    fn request_render(&mut self, force: bool) {
        self.reference.request_render(force)
    }
    fn add_input_listener(
        &mut self,
        listener: kiana_tui::tui::TuiInputListener,
    ) -> kiana_tui::tui::InputListenerId {
        self.reference.add_input_listener(listener)
    }
    fn remove_input_listener(&mut self, id: kiana_tui::tui::InputListenerId) {
        self.reference.remove_input_listener(id)
    }
    fn query_terminal_colors(
        &mut self,
        timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = kiana_tui::terminal_colors::TerminalColors> + '_>,
    > {
        self.reference.query_terminal_colors(timeout_ms)
    }
    fn is_viewport_tui(&self) -> bool {
        self.reference.is_viewport_tui()
    }
}

/// 当前渲染器的 `Arc<dyn Tui>`。
///
/// 每次调用都会新建一个 `TuiArcAdapter`（内部克隆渲染器的 `Rc`），不缓存实例；工厂拿到
/// 的句柄只在 UI 线程使用。`Arc` 的存在是因为扩展 API 的工厂签名固定为 `Arc<dyn Tui>`，
/// 它不提供 `Send`/`Sync` 语义（见下方 `clippy::arc_with_non_send_sync` 的说明）。
#[allow(
    clippy::arc_with_non_send_sync,
    reason = "InteractiveTuiReference 持有 Rc<RefCell<渲染器>>；Arc 仅用于把稳定句柄交给扩展工厂（单线程 UI）"
)]
pub(crate) fn tui_arc(mode: &InteractiveMode) -> Arc<dyn Tui> {
    let renderer = Rc::clone(&mode.inner.borrow().renderer);
    Arc::new(TuiArcAdapter::new(InteractiveTuiReference::new(renderer)))
}

/// 当前主题实例。
///
/// 返回全局主题的 `Arc` 克隆，供工厂调用期间通过 `&Theme` 借用。主题尚未初始化时
/// `expect` panic：widget/footer/header 工厂和 `fg` 都要求启动流程已调用过主题初始化。
pub(crate) fn current_theme_arc() -> Arc<kiana_theme::theme::Theme> {
    kiana_theme::theme::current_theme()
        .expect("theme must be initialized before rendering (TS throws here)")
}

// ---------------------------------------------------------------------------
// AssistantMessageComponent 注册表（TS 的 `instanceof AssistantMessageComponent`）
// ---------------------------------------------------------------------------

thread_local! {
    static ASSISTANT_COMPONENTS: RefCell<Vec<Weak<RefCell<AssistantMessageComponent>>>> =
        const { RefCell::new(Vec::new()) };
    static TERMINAL_INPUT_IDS: RefCell<Vec<TerminalInputRegistration>> =
        const { RefCell::new(Vec::new()) };
}

/// 扩展终端输入监听器的 TUI id（清理时按 handler 身份找到它）。
struct TerminalInputRegistration {
    handler: TerminalInputHandler,
    id: kiana_tui::tui::InputListenerId,
}

/// 登记一个已渲染的 assistant 组件（`setHiddenThinkingLabel` / thinking 可见性用）。
///
/// 只保存弱引用，登记时顺手丢弃已释放的旧条目；不按句柄去重，同一实例重复登记会被
/// `for_each_assistant_component` 处理多次（当前两个调用点都是幂等设置，重复执行没有
/// 额外影响）。
pub(crate) fn register_assistant_component(handle: &Rc<RefCell<AssistantMessageComponent>>) {
    ASSISTANT_COMPONENTS.with(|components| {
        let mut components = components.borrow_mut();
        components.retain(|weak| weak.strong_count() > 0);
        components.push(Rc::downgrade(handle));
    });
}

/// 对所有仍存活的 assistant 组件执行操作。
///
/// 可升级的句柄先收集到临时 `Vec`，再逐个 `try_borrow_mut`；正被借用的组件本次跳过，
/// 不会排队重试。已释放的弱引用留在表中，等下次登记时清理。
fn for_each_assistant_component(mut f: impl FnMut(&mut AssistantMessageComponent)) {
    ASSISTANT_COMPONENTS.with(|components| {
        let alive: Vec<Rc<RefCell<AssistantMessageComponent>>> = components
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for component in alive {
            if let Ok(mut component) = component.try_borrow_mut() {
                f(&mut component);
            }
        }
    });
}

/// `isEmbeddedStatus(this.editor)`：默认编辑器支持内嵌状态行。
pub(crate) fn editor_supports_embedded_status(mode: &InteractiveMode) -> bool {
    matches!(
        mode.inner.borrow().editor,
        super::super::ActiveEditor::Default { .. }
    )
}

thread_local! {
    /// 当前编辑器边框着色（TS `this.editor.borderColor`）。
    ///
    /// 状态指示器在渲染期调用 colorFn；TS 读 `this.editor.borderColor` 时是实时的，
    /// 这里保存 `updateEditorBorderColor` 安装的同一个函数（跨线程调用时回退到
    /// thinking 边框色）。
    static EDITOR_BORDER_COLOR: RefCell<Option<StyleFn>> = const { RefCell::new(None) };
}

/// 记录当前编辑器边框色（`updateEditorBorderColor` 调用）。
pub(crate) fn set_editor_border_color_style(color: Option<StyleFn>) {
    EDITOR_BORDER_COLOR.with(|slot| *slot.borrow_mut() = color);
}

/// 当前编辑器边框色；UI 线程之外返回 `None`。
pub(crate) fn editor_border_color_style() -> Option<StyleFn> {
    EDITOR_BORDER_COLOR.with(|slot| slot.borrow().clone())
}

impl InteractiveMode {
    /// `isEmbeddedStatus(this.editor)`：默认编辑器支持内嵌状态行。
    pub(crate) fn editor_supports_embedded_status(&self) -> bool {
        matches!(
            self.inner.borrow().editor,
            super::super::ActiveEditor::Default { .. }
        )
    }

    /// `setExtensionStatus(key, text)`。
    ///
    /// 把扩展状态写进 footer 数据提供者（同 `key` 覆盖，`Some` 插入、`None` 删除），
    /// 随后请求重绘；footer 在渲染期读取该实时表。只改数据，不直接触碰状态行，也不判断
    /// 文本是否真的变化。
    pub(crate) fn set_extension_status(&mut self, key: &str, text: Option<&str>) {
        self.inner
            .borrow()
            .footer_data_provider
            .set_extension_status(key, text);
        self.request_render();
    }

    /// `setEditorWorkingStatusIndicator(indicator)`。
    ///
    /// 先用 `None` 清掉默认编辑器上现有的内嵌指示器，再判断当前编辑器是否为默认编辑器
    /// （`editor_supports_embedded_status`）：不是则返回 `false` 且不挂载，是则把
    /// `indicator` 交给默认编辑器并返回 `true`。返回 `false` 表示调用方应改用独立状态行。
    fn set_editor_working_status_indicator(
        &mut self,
        indicator: Option<Box<dyn StatusIndicator>>,
    ) -> bool {
        {
            let editor = Rc::clone(&self.inner.borrow().default_editor);
            editor.borrow_mut().set_working_status_indicator(None);
        }
        if !self.editor_supports_embedded_status() {
            return false;
        }
        let editor = Rc::clone(&self.inner.borrow().default_editor);
        editor.borrow_mut().set_working_status_indicator(indicator);
        true
    }

    /// `showStatusIndicator(indicator)`。
    ///
    /// 安装一个新的活动状态指示器：先 `dispose` 旧的，再把 `indicator` 放进共享单元，
    /// 让 `active_status_indicator` 持有 `IndicatorHandle`。随后清空 `status_container`
    /// 与默认编辑器上的旧内嵌指示器，优先内嵌到编辑器边框；默认编辑器不在位时改为把
    /// `IndicatorProxy` 挂进 `status_container`。挂载前先把
    /// `active_working_indicator_embedded` 置 `false`，避免沿用上一次的挂载形态。
    pub(crate) fn show_status_indicator(&mut self, indicator: Box<dyn StatusIndicator>) {
        if let Some(mut previous) = self.inner.borrow_mut().active_status_indicator.take() {
            StatusIndicator::dispose(&mut *previous);
        }
        let cell: IndicatorCell = Rc::new(RefCell::new(indicator));
        store_active_indicator_cell(Some(Rc::clone(&cell)));
        {
            let mut inner = self.inner.borrow_mut();
            inner.active_status_indicator = Some(Box::new(IndicatorHandle(Rc::clone(&cell))));
            inner.active_working_indicator_embedded = false;
            let status = Rc::clone(&inner.status_container);
            status.borrow_mut().clear();
        }
        self.set_editor_working_status_indicator(None);
        if self
            .set_editor_working_status_indicator(Some(Box::new(IndicatorHandle(Rc::clone(&cell)))))
        {
            self.inner.borrow_mut().active_working_indicator_embedded = true;
            return;
        }
        let status = Rc::clone(&self.inner.borrow().status_container);
        status
            .borrow_mut()
            .add_child(Box::new(IndicatorProxy(cell)));
    }

    /// 换编辑器后重新挂载活动状态指示器（`setCustomEditorComponent` 的尾部）。
    ///
    /// 没有活动指示器时直接返回。否则从线程局部单元取回**同一实例**，清空状态容器后
    /// 按新编辑器是否支持内嵌重新选择挂载点，因此换编辑器不会丢失指示器的动画进度与
    /// 重绘回调。取不到共享单元的防御分支只恢复 TS 的清空动作。
    ///
    /// TS: `statusContainer.clear(); activeWorkingIndicatorEmbedded =
    /// setEditorWorkingStatusIndicator(activeStatusIndicator); if (!embedded)
    /// statusContainer.addChild(activeStatusIndicator)`。
    pub(crate) fn reattach_active_status_indicator(&mut self) {
        if self.inner.borrow().active_status_indicator.is_none() {
            return;
        }
        let Some(cell) = active_indicator_cell() else {
            // 没有共享单元（理论上不会发生）：至少恢复 TS 的清空动作。
            let status = Rc::clone(&self.inner.borrow().status_container);
            status.borrow_mut().clear();
            self.inner.borrow_mut().active_working_indicator_embedded = false;
            return;
        };
        {
            let status = Rc::clone(&self.inner.borrow().status_container);
            status.borrow_mut().clear();
        }
        let embedded = self
            .set_editor_working_status_indicator(Some(Box::new(IndicatorHandle(Rc::clone(&cell)))));
        self.inner.borrow_mut().active_working_indicator_embedded = embedded;
        if !embedded {
            let status = Rc::clone(&self.inner.borrow().status_container);
            status
                .borrow_mut()
                .add_child(Box::new(IndicatorProxy(cell)));
        }
    }

    /// `clearStatusIndicator(kind?)`。
    ///
    /// `kind` 为 `Some` 时只在当前活动指示器的 kind 匹配时继续，否则直接返回（不重绘、
    /// 不改状态）。随后取走并 `dispose` 当前指示器（若有），清空共享单元（仅当确实
    /// 清掉时）、状态容器与编辑器内嵌槽位。若被清掉的指示器之前是独立状态行
    /// （`!was_embedded`），且处于 regular 模式并启用了 `clearOnShrink`，补一个
    /// `IdleStatus` 占位行以维持高度。
    pub(crate) fn clear_status_indicator(&mut self, kind: Option<StatusIndicatorKind>) {
        if let Some(kind) = kind {
            let matches = self
                .inner
                .borrow()
                .active_status_indicator
                .as_ref()
                .is_some_and(|indicator| indicator.kind() == kind);
            if !matches {
                return;
            }
        }
        let (cleared, was_embedded) = {
            let mut inner = self.inner.borrow_mut();
            let cleared = inner.active_status_indicator.take();
            let was_embedded = inner.active_working_indicator_embedded;
            inner.active_working_indicator_embedded = false;
            (cleared, was_embedded)
        };
        let had_indicator = cleared.is_some();
        if had_indicator {
            store_active_indicator_cell(None);
        }
        if let Some(mut indicator) = cleared {
            StatusIndicator::dispose(&mut *indicator);
        }
        {
            let status = Rc::clone(&self.inner.borrow().status_container);
            status.borrow_mut().clear();
        }
        self.set_editor_working_status_indicator(None);
        let regular = matches!(
            self.inner.borrow().options.tui_mode,
            Some(kiana_config::settings::TuiMode::Regular)
        );
        let clear_on_shrink = self.inner.borrow().ui.get_clear_on_shrink();
        // TS 只在“确实清掉了一个不在编辑器内嵌的指示器”且 regular 且 clearOnShrink 时补空行。
        if had_indicator && !was_embedded && regular && clear_on_shrink {
            let status = Rc::clone(&self.inner.borrow().status_container);
            status.borrow_mut().add_child(Box::new(IdleStatus::new()));
        }
    }

    /// `showWorkingStatusIndicator()`。
    ///
    /// 文案优先用扩展设置过的 `working_message`，否则用 `default_working_message`；
    /// 连同 `working_indicator_options` 一起交给 `working_indicator` 新建 working 指示器，
    /// 再走 `show_status_indicator` 的统一挂载流程。
    pub(crate) fn show_working_status_indicator(&mut self) {
        let (message, options) = {
            let inner = self.inner.borrow();
            (
                inner
                    .working_message
                    .clone()
                    .unwrap_or_else(|| inner.default_working_message.clone()),
                inner.working_indicator_options.clone(),
            )
        };
        let indicator = self.working_indicator(&message, options);
        self.show_status_indicator(Box::new(indicator));
    }

    /// `setWorkingVisible(visible)`。
    ///
    /// 先记录开关。`false` 时立即清掉 Working 指示器并重绘；`true` 时仅在会话正在流式
    /// 输出且当前指示器不是 Working 的情况下新建 working 指示器（这会替换掉当前其它
    /// kind 的指示器，因为 `show_status_indicator` 只允许一个活动指示器）。
    /// 两个分支结束时都请求重绘。
    pub(crate) fn set_working_visible(&mut self, visible: bool) {
        self.inner.borrow_mut().working_visible = visible;
        if !visible {
            self.clear_status_indicator(Some(StatusIndicatorKind::Working));
            self.request_render();
            return;
        }
        let is_streaming = self.session().is_streaming();
        let kind = self
            .inner
            .borrow()
            .active_status_indicator
            .as_ref()
            .map(|indicator| indicator.kind());
        if is_streaming && kind != Some(StatusIndicatorKind::Working) {
            self.show_working_status_indicator();
        }
        self.request_render();
    }

    /// `setWorkingIndicator(options)`。
    ///
    /// 保存选项，供之后新建 working 指示器时使用。当前活动指示器恰好是 Working 时，
    /// 把选项转换成 loader 参数并调用其 `set_indicator`（`None` 表示恢复默认动画帧与
    /// 间隔）；其它 kind 的指示器不受影响。最后请求重绘。
    pub(crate) fn set_working_indicator(
        &mut self,
        options: Option<kiana_ext_api::ui::WorkingIndicatorOptions>,
    ) {
        self.inner.borrow_mut().working_indicator_options = options.clone();
        let is_working = self
            .inner
            .borrow()
            .active_status_indicator
            .as_ref()
            .is_some_and(|indicator| indicator.kind() == StatusIndicatorKind::Working);
        if is_working {
            let loader_options =
                crate::components::status_indicator::loader_indicator_options(options.as_ref());
            if let Some(indicator) = &mut self.inner.borrow_mut().active_status_indicator {
                indicator.set_indicator(loader_options);
            }
        }
        self.request_render();
    }

    /// `setHiddenThinkingLabel(label)`。
    ///
    /// `None` 表示恢复默认文案 `default_hidden_thinking_label`。新文案会写进内部字段，
    /// 并广播给所有已登记的 assistant 组件与当前流式组件；组件若已有 `last_message`
    /// 会立即重建内容。最后请求重绘。
    pub(crate) fn set_hidden_thinking_label(&mut self, label: Option<&str>) {
        let label = label
            .map(str::to_string)
            .unwrap_or_else(|| self.inner.borrow().default_hidden_thinking_label.clone());
        self.inner.borrow_mut().hidden_thinking_label = label.clone();
        for_each_assistant_component(|component| {
            component.set_hidden_thinking_label(label.clone());
        });
        if let Some(streaming) = self.inner.borrow().streaming_component.clone() {
            streaming
                .borrow_mut()
                .set_hidden_thinking_label(label.clone());
        }
        self.request_render();
    }

    /// `setExtensionWidget(key, content, options)`。
    ///
    /// `key` 决定替换关系：同一 key 在“上方/下方”两张表里都可能存在旧项，先全部
    /// `dispose` 再按 `placement`（缺省 AboveEditor）放入新组件，因此同一 key 换位置
    /// 不会留下残留。`content` 为 `None` 表示只删除并重绘。文本行最多渲染
    /// `MAX_WIDGET_LINES` 行，超出时追加一行 `... (widget truncated)`（因此总行数可达
    /// 上限 +1）；组件型内容由工厂现场构建并包成共享句柄。
    pub(crate) fn set_extension_widget(
        &mut self,
        key: &str,
        content: Option<WidgetContent>,
        options: Option<ExtensionWidgetOptions>,
    ) {
        let placement = options
            .and_then(|options| options.placement)
            .unwrap_or(kiana_ext_api::ui::WidgetPlacement::AboveEditor);
        // TS `removeExisting`：两个 Map 里同 key 的旧 widget 都要 `dispose?.()`。
        let replaced: Vec<SharedComponent> = {
            let mut inner = self.inner.borrow_mut();
            [
                inner.extension_widgets_above.shift_remove(key),
                inner.extension_widgets_below.shift_remove(key),
            ]
            .into_iter()
            .flatten()
            .collect()
        };
        for component in replaced {
            component.borrow_mut().dispose();
        }

        let Some(content) = content else {
            self.render_widgets();
            return;
        };

        let component: SharedComponent = match content {
            WidgetContent::Lines(lines) => {
                let mut container = Container::new();
                for line in lines.iter().take(MAX_WIDGET_LINES) {
                    container.add_child(Box::new(Text::with_padding(line.clone(), 1, 0)));
                }
                if lines.len() > MAX_WIDGET_LINES {
                    container.add_child(Box::new(ThemedText::with_padding(
                        Box::new(|| fg(ThemeColor::Muted, "... (widget truncated)")),
                        1,
                        0,
                    )));
                }
                Rc::new(RefCell::new(container))
            }
            WidgetContent::Component(factory) => {
                let component = create_widget(factory, &self.ui_handles());
                shared_from_boxed(component)
            }
        };

        let mut inner = self.inner.borrow_mut();
        match placement {
            kiana_ext_api::ui::WidgetPlacement::BelowEditor => {
                inner
                    .extension_widgets_below
                    .insert(key.to_string(), component);
            }
            kiana_ext_api::ui::WidgetPlacement::AboveEditor => {
                inner
                    .extension_widgets_above
                    .insert(key.to_string(), component);
            }
        }
        drop(inner);
        self.render_widgets();
    }

    /// `clearExtensionWidgets()`。
    ///
    /// 清空两张 widget 表并逐个 `dispose`，最后重建容器；footer/header 不受影响。
    pub(crate) fn clear_extension_widgets(&mut self) {
        // TS 先逐个 `widget.dispose?.()` 再清空两个 Map。
        let (above, below) = {
            let mut inner = self.inner.borrow_mut();
            (
                std::mem::take(&mut inner.extension_widgets_above),
                std::mem::take(&mut inner.extension_widgets_below),
            )
        };
        for component in above.values().chain(below.values()) {
            component.borrow_mut().dispose();
        }
        self.render_widgets();
    }

    /// `renderWidgets()`。
    ///
    /// 按两个容器各自的规则重建子项：上方容器为空时放一个 `Spacer` 占位，非空时改为
    /// 在组件前放一个 `Spacer`；下方容器两种情况都不放。widget 在各自表中的插入顺序
    /// （`IndexMap`）即渲染顺序。重建只换代理，不 `dispose` 组件——组件仍由
    /// `extension_widgets_*` 表持有。
    pub(crate) fn render_widgets(&mut self) {
        let (above, below, above_container, below_container) = {
            let inner = self.inner.borrow();
            (
                inner.extension_widgets_above.clone(),
                inner.extension_widgets_below.clone(),
                Rc::clone(&inner.widget_container_above),
                Rc::clone(&inner.widget_container_below),
            )
        };
        render_widget_container(&above_container, &above, true, true);
        render_widget_container(&below_container, &below, false, false);
        self.request_render();
    }

    /// `setExtensionFooter(factory)`。
    ///
    /// `Some` 时先 `dispose` 旧的自定义 footer、清空容器，再用 `(tui, theme,
    /// footer_data)` 建造新组件并包成共享句柄挂载；`None` 时恢复内置 footer。替换与
    /// 清除都必须显式 `dispose`，因为 `Container::clear` 不会触发组件清理。主题未初始化
    /// 时工厂调用会 panic。
    pub(crate) fn set_extension_footer(
        &mut self,
        factory: Option<kiana_ext_api::ui::FooterFactory>,
    ) {
        // TS：先 `customFooter.dispose?.()` 再清空容器。
        let previous = self.inner.borrow_mut().custom_footer.take();
        if let Some(previous) = previous {
            previous.borrow_mut().dispose();
        }
        let footer_container = Rc::clone(&self.inner.borrow().footer_container);
        footer_container.borrow_mut().clear();
        match factory {
            Some(factory) => {
                let handles = self.ui_handles();
                let theme = current_theme_arc();
                let footer_data: Arc<dyn ReadonlyFooterDataProvider> =
                    Arc::clone(&self.inner.borrow().footer_data_provider)
                        as Arc<dyn ReadonlyFooterDataProvider>;
                let component = factory(handles.tui, &theme, footer_data);
                let shared = shared_from_boxed(component);
                self.inner.borrow_mut().custom_footer = Some(Rc::clone(&shared));
                footer_container
                    .borrow_mut()
                    .add_child(Box::new(SharedComponentProxy::new(shared)));
            }
            None => {
                self.inner.borrow_mut().custom_footer = None;
                let footer = Rc::clone(&self.inner.borrow().footer);
                let handle = super::super::shared(&footer);
                footer_container
                    .borrow_mut()
                    .add_child(super::super::boxed_shared(&handle));
            }
        }
        self.request_render();
    }

    /// `setExtensionHeader(factory)`。
    ///
    /// 内置 header 尚未建立（`built_in_header` 为 `None`）时直接返回：不安装、不清理、
    /// 也不重绘。否则先 `dispose` 旧的自定义 header，再在 header 容器里定位当前 header
    /// 的槽位（见 `header_index`）。`Some(factory)` 用 `(tui, theme)` 建造新组件替换该
    /// 槽位，并把当前 `tool_output_expanded` 应用到 `ExpandableText` 形态的 header；
    /// `None` 把内置 header 放回原位。最后请求重绘。
    pub(crate) fn set_extension_header(&mut self, factory: Option<HeaderFactory>) {
        let Some(built_in) = self
            .inner
            .borrow()
            .built_in_header
            .as_ref()
            .map(built_in_descriptor)
        else {
            return;
        };
        // TS：先 `customHeader.dispose?.()`，再按当前 header 在容器里的位置替换。
        let previous = self.inner.borrow().custom_header.clone();
        if let Some(previous) = previous {
            previous.borrow_mut().dispose();
        }
        let header_container = Rc::clone(&self.inner.borrow().header_container);
        let index = {
            let container = header_container.borrow();
            header_index(
                &container,
                self.inner.borrow().custom_header.as_ref(),
                &built_in,
            )
        };

        if let Some(factory) = factory {
            let handles = self.ui_handles();
            let theme = current_theme_arc();
            let component = factory(handles.tui, &theme);
            let shared = shared_from_boxed(component);
            let expanded = self.inner.borrow().tool_output_expanded;
            if let Ok(mut custom) = shared.try_borrow_mut()
                && let Some(any) = custom.as_any_mut()
                && let Some(expandable) = any.downcast_mut::<super::super::ExpandableText>()
            {
                expandable.set_expanded(expanded);
            }
            self.inner.borrow_mut().custom_header = Some(Rc::clone(&shared));
            let mut container = header_container.borrow_mut();
            let proxy: Box<dyn Component> = Box::new(SharedComponentProxy::new(shared));
            match index {
                Some(index) => container.children[index] = proxy,
                None => container.children.insert(0, proxy),
            }
        } else {
            self.inner.borrow_mut().custom_header = None;
            if let Some(HeaderComponent::BuiltIn(handle)) = &self.inner.borrow().built_in_header {
                let expanded = self.inner.borrow().tool_output_expanded;
                handle.borrow_mut().set_expanded(expanded);
            }
            let mut container = header_container.borrow_mut();
            let replacement: Box<dyn Component> = match &built_in {
                BuiltInDescriptor::Shared(shared) => {
                    Box::new(SharedComponentProxy::new(Rc::clone(shared)))
                }
                BuiltInDescriptor::Empty => Box::new(Text::with_padding("", 0, 0)),
            };
            match index {
                Some(index) => container.children[index] = replacement,
                None => container.children.insert(0, replacement),
            }
        }
        self.request_render();
    }

    /// `addExtensionTerminalInputListener(handler)`。
    ///
    /// 把 handler 包成 TUI 监听器：`Some(result)` 原样转成 `TuiInputListenerResult`，
    /// `None`（不处理）继续默认路径。
    ///
    /// 订阅本体登记在 `extension_terminal_input_subscriptions`；实际的 TUI 监听器 id 记在
    /// 线程局部表里（清理需要 `&mut ui`，而存储的 `unsubscribe` 要求 `Send + Sync`）。
    /// 因此该订阅项里的 `unsubscribe` 是空闭包，移除必须走
    /// `remove_extension_terminal_input_listener`。
    pub(crate) fn add_extension_terminal_input_listener(&mut self, handler: TerminalInputHandler) {
        let handler_for_listener = Arc::clone(&handler);
        let listener: kiana_tui::tui::TuiInputListener =
            Box::new(move |data: &str| match handler_for_listener(data) {
                Some(result) => Some(kiana_tui::tui::TuiInputListenerResult {
                    consume: result.consume,
                    data: result.data,
                }),
                None => None,
            });
        let id = self.inner.borrow_mut().ui.add_input_listener(listener);
        TERMINAL_INPUT_IDS.with(|ids| {
            ids.borrow_mut().push(TerminalInputRegistration {
                handler: Arc::clone(&handler),
                id,
            });
        });
        self.inner
            .borrow_mut()
            .extension_terminal_input_subscriptions
            .push(super::super::ExtensionTerminalInputSubscription {
                handler,
                unsubscribe: Box::new(|| {}),
            });
    }

    /// 按 handler 身份移除监听器（桥的 `unsubscribe` 走这里）。
    ///
    /// 用 `Arc::ptr_eq`（指针身份，不是内容）在 `TERMINAL_INPUT_IDS` 里找第一个匹配项，
    /// 从当前渲染器退订并删除，再从订阅列表移除所有同一 handler 的条目。
    /// handler 不存在时是空操作。
    pub(crate) fn remove_extension_terminal_input_listener(
        &mut self,
        handler: &TerminalInputHandler,
    ) {
        let removed_id = TERMINAL_INPUT_IDS.with(|ids| {
            let mut ids = ids.borrow_mut();
            let index = ids
                .iter()
                .position(|registration| Arc::ptr_eq(&registration.handler, handler));
            index.map(|index| ids.remove(index).id)
        });
        if let Some(id) = removed_id {
            self.inner.borrow_mut().ui.remove_input_listener(id);
        }
        let mut inner = self.inner.borrow_mut();
        inner
            .extension_terminal_input_subscriptions
            .retain(|subscription| !Arc::ptr_eq(&subscription.handler, handler));
    }

    /// `rebindExtensionTerminalInputListeners()`。
    ///
    /// 渲染器切换后重建订阅：先取出全部 handler，清掉旧渲染器上的监听器，再逐个重新
    /// 注册到当前 `ui`。旧条目已被清空，因此重复调用是幂等的。
    pub(crate) fn rebind_extension_terminal_input_listeners(&mut self) {
        let handlers: Vec<TerminalInputHandler> = self
            .inner
            .borrow()
            .extension_terminal_input_subscriptions
            .iter()
            .map(|subscription| Arc::clone(&subscription.handler))
            .collect();
        self.clear_extension_terminal_input_listeners();
        for handler in handlers {
            self.add_extension_terminal_input_listener(handler);
        }
    }

    /// `clearExtensionTerminalInputListeners()`。
    ///
    /// 从当前渲染器移除所有扩展监听器并清空订阅列表；不影响普通输入处理。
    pub(crate) fn clear_extension_terminal_input_listeners(&mut self) {
        let ids: Vec<kiana_tui::tui::InputListenerId> = TERMINAL_INPUT_IDS.with(|ids| {
            std::mem::take(&mut *ids.borrow_mut())
                .into_iter()
                .map(|registration| registration.id)
                .collect()
        });
        {
            let mut inner = self.inner.borrow_mut();
            for id in ids {
                inner.ui.remove_input_listener(id);
            }
            inner.extension_terminal_input_subscriptions.clear();
        }
    }

    /// `updateAvailableProviderCount()`。
    ///
    /// 计算“至少有一个可用模型的 provider 数量”并写进 footer 数据提供者：会话存在
    /// scoped models 时以该列表为准，否则读模型运行时的可用快照；`HashSet` 按 provider
    /// 去重。只更新数据，不触发重绘（footer 在下次渲染时读取新值）。
    pub(crate) fn update_available_provider_count(&mut self) {
        let session = self.session();
        let scoped = session.scoped_models();
        let models: Vec<kiana_ai::types::Model> = if scoped.is_empty() {
            session.model_runtime().get_available_snapshot()
        } else {
            scoped.into_iter().map(|scoped| scoped.model).collect()
        };
        let mut providers: HashSet<String> = HashSet::new();
        for model in models {
            providers.insert(model.provider);
        }
        self.inner
            .borrow()
            .footer_data_provider
            .set_available_provider_count(providers.len());
    }

    /// `setupExtensionShortcuts(extensionRunner)`。
    ///
    /// `get_shortcuts` 返回的 `IndexMap` 保持注册顺序。回调在默认编辑器收到输入时按该
    /// 顺序用 `matches_key` 匹配，命中第一个就 `spawn_local` 执行其 handler 并返回
    /// `true`（消费输入），全部不命中返回 `false`（输入继续走默认路径）。
    /// 快捷键表在安装时被闭包按值捕获，是一份快照；扩展注册变化后需要重新调用本方法。
    /// 快捷键表为空时直接返回：不安装，也不会清掉此前已安装的回调。
    pub(crate) fn setup_extension_shortcuts(
        &mut self,
        runner: Arc<kiana_extensions::runner::ExtensionRunner>,
    ) {
        let effective = self.inner.borrow().keybindings.get_effective_config();
        let shortcuts = runner.get_shortcuts(&effective);
        if shortcuts.is_empty() {
            return;
        }
        let runner_for_handler = Arc::clone(&runner);
        self.inner
            .borrow()
            .default_editor
            .borrow_mut()
            .set_on_extension_shortcut(Some(Box::new(move |data: &str| {
                for (shortcut_str, shortcut) in &shortcuts {
                    if kiana_tui::keys::matches_key(data, shortcut_str) {
                        let handler = Arc::clone(&shortcut.handler);
                        let context = runner_for_handler.create_context();
                        tokio::task::spawn_local(async move {
                            handler(context).await;
                        });
                        return true;
                    }
                }
                false
            })));
    }

    /// `toggleToolOutputExpansion()`。
    ///
    /// 读取当前展开状态并取反后转交 `set_tools_expanded`（包括状态提示）。
    pub(crate) fn toggle_tool_output_expansion(&mut self) {
        let expanded = self.inner.borrow().tool_output_expanded;
        self.set_tools_expanded(!expanded);
    }

    /// `setToolsExpanded(expanded)`。
    ///
    /// 值未变化时直接返回（不重绘、不提示）。否则依次更新：built-in/custom header、
    /// `loaded_resources_container` 与 `chat_container` 的直接子项、注册表里的已入聊天
    /// 组件、`pending_tools`；然后发布 UI 快照并显示 `Tool output: expanded/collapsed`
    /// 状态提示。更新是尽力而为：正被可变借用的共享组件本次跳过，不排队重试。
    pub(crate) fn set_tools_expanded(&mut self, expanded: bool) {
        if self.inner.borrow().tool_output_expanded == expanded {
            return;
        }
        self.inner.borrow_mut().tool_output_expanded = expanded;
        // header
        {
            if let Some(HeaderComponent::BuiltIn(handle)) = &self.inner.borrow().built_in_header {
                handle.borrow_mut().set_expanded(expanded);
            }
            let custom = self.inner.borrow().custom_header.clone();
            if let Some(custom) = custom
                && let Ok(mut custom) = custom.try_borrow_mut()
            {
                apply_expanded_to_component(&mut *custom, expanded);
            }
        }
        // loaded resources / chat：TS 对每个子项做 `isExpandable` 结构判定
        // （任何带 `setExpanded` 的对象），这里覆盖所有已实现 `set_expanded` 的组件类型，
        // 以及 `SharedComponentProxy` / `OwnedComponent` 包装的共享句柄。
        let containers = {
            let inner = self.inner.borrow();
            vec![
                Rc::clone(&inner.loaded_resources_container),
                Rc::clone(&inner.chat_container),
            ]
        };
        for container in containers {
            let mut container = container.borrow_mut();
            for child in &mut container.children {
                apply_expanded_to_component(child.as_mut(), expanded);
            }
        }
        // 已加入 chat 的可展开组件（工具行、custom entry/message、摘要、bash 行等）。
        apply_expanded_to_registered(expanded);
        // 活动中/历史工具行
        let tools: Vec<Rc<RefCell<crate::components::tool_execution::ToolExecutionComponent>>> =
            self.inner
                .borrow()
                .pending_tools
                .values()
                .cloned()
                .collect();
        for tool in tools {
            tool.borrow_mut().set_expanded(expanded);
        }
        self.publish_ui_snapshot();
        self.show_status(if expanded {
            "Tool output: expanded"
        } else {
            "Tool output: collapsed"
        });
    }

    /// `updateThinkingBlockVisibility()`。
    ///
    /// 把当前 `hide_thinking_block` 分发给所有已登记的 assistant 组件并重绘；组件会立刻
    /// 按新可见性重建已有内容。
    pub(crate) fn update_thinking_block_visibility(&mut self) {
        let hide = self.inner.borrow().hide_thinking_block;
        for_each_assistant_component(|component| {
            component.set_hide_thinking_block(hide);
        });
        self.request_render();
    }

    /// `toggleThinkingBlockVisibility()`。
    ///
    /// 翻转内部开关、写入设置管理器（持久化）、广播到已渲染组件，最后显示
    /// `Thinking blocks: hidden/visible` 状态提示。
    pub(crate) fn toggle_thinking_block_visibility(&mut self) {
        let hide = {
            let mut inner = self.inner.borrow_mut();
            inner.hide_thinking_block = !inner.hide_thinking_block;
            inner.hide_thinking_block
        };
        self.settings_manager().set_hide_thinking_block(hide);
        self.update_thinking_block_visibility();
        self.show_status(&format!(
            "Thinking blocks: {}",
            if hide { "hidden" } else { "visible" }
        ));
    }

    /// 工厂调用需要的一组句柄。
    fn ui_handles(&self) -> UiHandles {
        UiHandles { tui: tui_arc(self) }
    }
}

/// widget/footer/header 工厂的调用句柄。
struct UiHandles {
    tui: Arc<dyn Tui>,
}

fn create_widget(factory: WidgetFactory, handles: &UiHandles) -> Box<dyn Component> {
    let theme = current_theme_arc();
    factory(Arc::clone(&handles.tui), &theme)
}

/// `renderWidgetContainer(container, widgets, spacerWhenEmpty, leadingSpacer)`。
///
/// 先 `clear` 再重建：`widgets` 为空时仅在 `spacer_when_empty` 下放一个 `Spacer`；
/// 非空时按 `IndexMap` 迭代顺序逐个挂 `SharedComponentProxy`，`leading_spacer` 为真时
/// 先放一个 `Spacer`。`clear` 只丢代理、不 `dispose`——组件仍由 `extension_widgets_*`
/// 表持有，重建后继续复用同一实例。
fn render_widget_container(
    container: &Rc<RefCell<Container>>,
    widgets: &indexmap::IndexMap<String, SharedComponent>,
    spacer_when_empty: bool,
    leading_spacer: bool,
) {
    let mut container = container.borrow_mut();
    container.clear();
    if widgets.is_empty() {
        if spacer_when_empty {
            container.add_child(Box::new(Spacer::new()));
        }
        return;
    }
    if leading_spacer {
        container.add_child(Box::new(Spacer::new()));
    }
    for component in widgets.values() {
        container.add_child(Box::new(SharedComponentProxy::new(Rc::clone(component))));
    }
}

/// 内置 header 的身份（`HeaderComponent` 的只读视图）。
enum BuiltInDescriptor {
    Shared(SharedComponent),
    Empty,
}

fn built_in_descriptor(header: &HeaderComponent) -> BuiltInDescriptor {
    match header {
        HeaderComponent::BuiltIn(handle) => {
            BuiltInDescriptor::Shared(Rc::clone(handle) as SharedComponent)
        }
        HeaderComponent::Empty => BuiltInDescriptor::Empty,
    }
}

/// header 容器里当前 header 的下标。
///
/// `custom` 是调用时尚未从 `custom_header` 取下的共享句柄，`built_in` 是内置 header 的
/// 身份。逐个子项把 `SharedComponentProxy` 的底层 `Rc` 地址与两者比较（指针身份，不是
/// 内容）。`Empty` 形态没有 Rc：静默启动时容器里只有一个空 `Text`，首个自定义 header 经
/// `insert(0)` 位于下标 0，空 `Text` 退到下标 1；回退到下标 1 命中的是空 `Text` 的槽位
/// （自定义 header 被移除后原位替换成空 `Text`、再次插入时触发）。返回 `None` 时调用方
/// 插到 0。
fn header_index(
    container: &Container,
    custom: Option<&SharedComponent>,
    built_in: &BuiltInDescriptor,
) -> Option<usize> {
    let target = custom.map(Rc::as_ptr).map(|ptr| ptr as *const () as usize);
    let built_in_ptr = match built_in {
        BuiltInDescriptor::Shared(shared) => Some(Rc::as_ptr(shared) as *const () as usize),
        BuiltInDescriptor::Empty => None,
    };
    let mut fallback_empty = None;
    for (index, child) in container.children.iter().enumerate() {
        let identity = child
            .as_any()
            .and_then(|any| any.downcast_ref::<SharedComponentProxy>())
            .map(|proxy| Rc::as_ptr(proxy.component()) as *const () as usize);
        if let Some(target) = target
            && identity == Some(target)
        {
            return Some(index);
        }
        if let Some(built_in_ptr) = built_in_ptr
            && identity == Some(built_in_ptr)
        {
            return Some(index);
        }
        if matches!(built_in, BuiltInDescriptor::Empty) && index == 1 {
            fallback_empty = Some(index);
        }
    }
    fallback_empty
}

#[doc(hidden)]
impl InteractiveMode {
    /// `toggleToolOutputExpansion()` 的测试接缝。
    pub fn test_toggle_tool_output_expansion(&mut self) {
        self.toggle_tool_output_expansion();
    }

    /// `setToolsExpanded(expanded)` 的测试接缝。
    pub fn test_set_tools_expanded(&mut self, expanded: bool) {
        self.set_tools_expanded(expanded);
    }

    /// 当前是否展开工具输出。
    pub fn test_tool_output_expanded(&self) -> bool {
        self.inner.borrow().tool_output_expanded
    }

    /// `showWorkingStatusIndicator()` 的测试接缝。
    pub fn test_show_working_status_indicator(&mut self) {
        self.show_working_status_indicator();
    }

    /// 活动状态指示器是否内嵌在编辑器边框里。
    pub fn test_active_working_indicator_embedded(&self) -> bool {
        self.inner.borrow().active_working_indicator_embedded
    }

    /// 编辑器槽当前渲染出的文本（去掉 ANSI）。
    ///
    /// 固定以宽度 120 渲染，多行用 `\n` 连接。
    pub fn test_editor_slot_text(&self) -> String {
        let container = Rc::clone(&self.inner.borrow().editor_container);
        container
            .borrow_mut()
            .render(120)
            .iter()
            .map(|line| kiana_tui::text::strip_terminal_sequences(line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 状态栏当前渲染出的文本（去掉 ANSI）。
    ///
    /// 固定以宽度 120 渲染，多行用 `\n` 连接。
    pub fn test_status_container_text(&self) -> String {
        let container = Rc::clone(&self.inner.borrow().status_container);
        container
            .borrow_mut()
            .render(120)
            .iter()
            .map(|line| kiana_tui::text::strip_terminal_sequences(line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 清空聊天容器（测试辅助）。
    pub fn test_clear_chat(&mut self) {
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        chat.borrow_mut().clear();
    }

    /// 聊天容器渲染文本（测试辅助）。
    pub fn test_chat_render_text(&self) -> String {
        self.chat_render_text()
    }

    /// `toggleThinkingBlockVisibility()` 的测试接缝。
    pub fn test_toggle_thinking_block_visibility(&mut self) {
        self.toggle_thinking_block_visibility();
    }

    /// `updateThinkingBlockVisibility()` 的测试接缝。
    pub fn test_update_thinking_block_visibility(&mut self) {
        self.update_thinking_block_visibility();
    }

    /// 两个 widget 容器渲染出的文本（去掉 ANSI）。
    ///
    /// 先上方后下方，各自固定以宽度 120 渲染，结果用 `\n` 连接。
    pub fn test_widget_text(&self) -> String {
        let (above, below) = {
            let inner = self.inner.borrow();
            (
                Rc::clone(&inner.widget_container_above),
                Rc::clone(&inner.widget_container_below),
            )
        };
        [above, below]
            .into_iter()
            .flat_map(|container| container.borrow_mut().render(120))
            .map(|line| kiana_tui::text::strip_terminal_sequences(&line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `clearStatusIndicator(kind?)` 的测试接缝。
    pub fn test_clear_status_indicator(&mut self, kind: Option<StatusIndicatorKind>) {
        self.clear_status_indicator(kind);
    }

    /// 当前活动状态指示器的 kind。
    pub fn test_active_status_kind(&self) -> Option<StatusIndicatorKind> {
        self.inner
            .borrow()
            .active_status_indicator
            .as_ref()
            .map(|indicator| indicator.kind())
    }

    /// 构造并展示指定 kind 的状态指示器（`showStatusIndicator` 的测试接缝）。
    ///
    /// 固定参数：compaction 使用 `Manual` 原因，retry 使用第 1/3 次、1000ms；都安装
    /// 当前的重绘回调，因此挂载后的动画与倒计时行为与真实路径一致。
    pub fn test_show_status_kind(&mut self, kind: StatusIndicatorKind) {
        use crate::components::status_indicator::{
            BranchSummaryStatusIndicator, CompactionStatusIndicator, CompactionStatusReason,
            RetryStatusIndicator, WorkingStatusIndicator,
        };
        let indicator: Box<dyn crate::components::status_indicator::StatusIndicator> = match kind {
            StatusIndicatorKind::Working => Box::new(WorkingStatusIndicator::new(
                Some(self.request_render_callback()),
                "Working",
                None,
                None,
            )),
            StatusIndicatorKind::Compaction => Box::new(CompactionStatusIndicator::new(
                Some(self.request_render_callback()),
                CompactionStatusReason::Manual,
                None,
            )),
            StatusIndicatorKind::BranchSummary => Box::new(BranchSummaryStatusIndicator::new(
                Some(self.request_render_callback()),
            )),
            StatusIndicatorKind::Retry => Box::new(RetryStatusIndicator::new(
                Some(self.request_render_callback()),
                1,
                3,
                1000.0,
            )),
        };
        self.show_status_indicator(indicator);
    }

    /// 状态容器里的子项数量。
    pub fn test_status_container_child_count(&self) -> usize {
        self.inner.borrow().status_container.borrow().children.len()
    }

    /// `setWorkingVisible(visible)` 的测试接缝。
    pub fn test_set_working_visible(&mut self, visible: bool) {
        self.set_working_visible(visible);
    }

    /// 覆盖渲染器的 `clearOnShrink`（clear-on-shrink 状态间距测试）。
    pub fn test_set_clear_on_shrink(&mut self, enabled: bool) {
        self.inner.borrow_mut().ui.set_clear_on_shrink(enabled);
    }

    /// 内置 header 当前是否展开（`test_toolsExpanded` 覆盖 header 的用例）。
    pub fn test_built_in_header_expanded(&self) -> Option<bool> {
        match &self.inner.borrow().built_in_header {
            Some(HeaderComponent::BuiltIn(handle)) => Some(handle.borrow().inner.expanded()),
            _ => None,
        }
    }

    /// 把一个 `ExpandableText` 探针挂进 loaded-resources 容器，返回其展开状态读取器。
    pub fn test_push_expandable_probe(&mut self) -> Box<dyn Fn() -> bool> {
        let handle = Rc::new(RefCell::new(super::super::ExpandableText::new(
            Rc::new(|| "collapsed".to_string()),
            Rc::new(|| "expanded".to_string()),
            false,
            1,
            0,
        )));
        let shared: super::super::SharedComponent =
            Rc::clone(&handle) as super::super::SharedComponent;
        self.inner
            .borrow()
            .loaded_resources_container
            .borrow_mut()
            .add_child(Box::new(SharedComponentProxy::new(shared)));
        let handle = Rc::clone(&handle);
        Box::new(move || handle.borrow().expanded())
    }
}
