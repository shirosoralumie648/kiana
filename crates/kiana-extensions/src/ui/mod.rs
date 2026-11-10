//! 扩展 UI 适配层：把 `ExtensionUIContext` 接到 JS，并提供 `@earendil-works/kiana-tui` 的运行期面。
//!
//! - [`adapter`]：`JsComponent` / `JsEditorComponent`——把 JS 组件对象实现为 `kiana_tui::Component`；
//! - [`print_json`]：print/json 模式的 no-op UI 上下文（复用 `runner::noop_ui` 的单一实现）；
//! - 本文件：原生类（`Text`/`Box`/`Editor`/…）、`ctx.ui` 的 JS 桥、TUI 代理、主题实例。
//!
//! TS 源：`packages/tui/src/tui.ts`、`packages/tui/src/components/*.ts`、
//! `packages/coding-agent/src/core/extensions/types.ts`、`runner.ts`。
//!
//! # 安装（设计文档 §9.2）
//!
//! [`install`] 在 prelude、node globals、context 之后调用。它安装：
//!
//! - `globalThis.__kiana_refs`：JS 值引用表（原生类持有 JS 回调/对象用引用 id，不用
//!   `Persistent`，避免引擎销毁顺序问题）；
//! - `globalThis.__kiana_tui_native`：原生类与原生函数；
//! - `globalThis.__kiana_tui`：`js/ui/tui.js` 定义的完整运行期面（`Container`/`Stack`/`VStack`/
//!   `HStack` 是 JS 类；`Editor` 是 JS 类继承原生 `Editor`，从而 `extends`/`super` 可用；
//!   其余符号直接引用原生实现）；
//! - `globalThis.__kiana_ui_native` 与 `globalThis.__kiana_ctx_make_ui`：`ctx.ui` 的 JS 桥
//!   （`js/ui/ui.js`）；
//! - `Symbol.for("@earendil-works/kiana-coding-agent:theme")` 与旧键：当前主题的原生实例
//!   （§5.5：扩展通过全局 symbol 读取主题，必须拿到同一个 Rust 原生实例）；
//! - host kind `"kiana_ui"`：`ctx.ui.select/confirm/input/editor/custom` 的异步桥。
//!
//! 虚拟模块单元（`@earendil-works/kiana-tui`）应从 `globalThis.__kiana_tui` 取导出；本模块不
//! `include_str!` 其它单元的 JS。
//!
//! # 线程与重入
//!
//! 引擎是单线程的（§2.0）。UI 工厂闭包是 `Send + Sync`，不能捕获 `Rc<JsEngine>`；它们在
//! 被调用时用 [`adapter::JsEngineSlot::current`] 查回引擎。所有「Rust 调 JS」都走
//! [`adapter::with_engine_js`]：已处于 JS 事务时复用当前 `Ctx`，绝不嵌套 `with_js`。
//!
//! 原生方法入口的 `let _active = ActiveCtxGuard::enter(&ctx)` 是这套重入协议的登记点：
//! 它把当前 `Ctx` 压入线程局部事务栈，使方法体内任何 `with_engine_js` 都复用同一事务；
//! 守卫在方法返回时出栈。进入时还会补做在事务外（JS 终结器、组件 Drop）登记的延迟引用
//! 清理（`adapter` 的 `PENDING_DISPOSALS`/`PENDING_REF_DROPS`）。守卫是 `!Send` 且只能
//! 包住一次同步调用，不能跨 `await` 保存，所以在每个 `#[qjs]` 方法开头声明、离开作用域
//! 时自动释放；在会回调 JS 的方法上漏写这一行，嵌套的 `with_engine_js` 会尝试对已借用
//! 的 runtime 再开事务并 panic。
//!
//! # 本文件负责的边界
//!
//! 只处理「扩展 JS 能看到的 UI 对象」，不实现模块加载、事件派发或组件转发（分别属于
//! `loader`、`runner`、[`adapter`]）。三类入口：
//!
//! - 原生组件类：`#[rquickjs::class]` 包装 `kiana-tui` 的 Rust 组件，方法名/参数与
//!   `packages/tui/src/components/*.ts` 一致，由 `js/ui/tui.js` 组装成 `__kiana_tui`；
//! - `ctx.ui`：`js/ui/ui.js` 的包装对象调用 `__kiana_ui_native.ExtensionUI`（[`JsUiBridge`]），
//!   异步对话框再经 `__kiana.promise("kiana_ui", …)` 进入 [`HOST_KIND`] 的处理器；
//! - 宿主侧工厂：widget/footer/header/editor/custom 的 JS 工厂以引用 id 保存，宿主在渲染
//!   线程调用 Rust 工厂闭包时再开事务回到 JS。
//!
//! # 引用与生命周期（`__kiana_refs`）
//!
//! 原生类不能把 `Persistent` 存进 `Rc` 组件（引擎可能先于组件销毁），因此回调与需要跨调用
//! 保留的 JS 对象（组件、TUI 对象、工厂）都经 [`adapter`] 的引用表保存成 `u64` id；主题实例
//! 走 `__kiana_theme_cache`/`__kiana_live_theme` 这两个独立缓存。由此有两条硬约束：
//!
//! - **只在事务里读写引用表**：`store_ref`/`load_ref` 需要 `Ctx`；从渲染等无事务路径回调 JS
//!   时统一走 `with_engine_js`（已处于事务则复用当前 `Ctx`，否则新开一个）。
//! - **引用释放是显式的**：本文件只有 [`JsSelectList`]、[`JsTui`]、[`JsUiBridge`] 的 `Drop`
//!   会清理引用（`release_ref`，或 `JsTui` 用的 `schedule_disposal`）；部分回调槽在替换时
//!   立即释放旧值（`replace_callback_ref`、`setFooter`/`setHeader`/`setEditorComponent`），
//!   而 `onSubmit`/`borderColor` 等 setter 直接覆盖。其余引用留在表里，随引擎销毁
//!   （`__kiana_refs` 是普通 JS 对象）一起回收。
//!
//! 例外是重绘回调：`request_render_from_js`/`request_render_fn_from_js` 会把创建时的
//! `Rc<JsEngine>` 捕获进回调，因此构造过 `Loader`/`ScrollView` 等组件的引擎在组件存活期间
//! 不会被释放（`adapter::JsComponent` 则用 `Weak`，两者策略不同）。
//!
//! 另一条贯穿本文件的规则是「回调用队列、方法返回后再派发」：`Input`/`SelectList`/
//! `SettingsList` 的回调在 kiana-tui 的 `handle_input`/`handle_mouse` 内部触发，此时组件正被
//! `RefCell` 可变借用；立刻回调 JS 会让 `getValue()` 之类的重入 `borrow_mut` panic。因此
//! 回调参数先写入 `pending_*`，借用释放后由 `drain_callbacks` 统一调用。
//!
//! # 一次典型调用
//!
//! 1. 扩展 `new Text(...)`：`JsText::new` 按 JS `Number()`/`String()` 语义归一化参数，构造
//!    `kiana_tui` 组件；挂到 JS `Container`/`TUIProxy` 之后由 `adapter::JsComponent` 把
//!    `render`/`handleInput` 转发回 JS。
//! 2. 扩展调用 `ctx.ui.select(...)`：`js/ui/ui.js` → `__kiana.promise("kiana_ui", payload)` →
//!    `ui_host_kind` 解析 payload → `spawn_local` 中 await `ExtensionUiContext::select` →
//!    `HostCompletion::Settle` 回到 JS Promise。`notify`/`setStatus` 等同步成员不经过 promise。
//! 3. 宿主装配 UI：`setWidget`/`setFooter`/… 把 JS 工厂存成引用；宿主之后调用 Rust 工厂闭包时，
//!    本文件把 `TUIProxy`、live 主题等 JS 值传给工厂，返回值包成 `adapter::OwnedJsComponent`
//!    （所有者在替换/关闭时调用 JS `dispose()`）。
//!
//! # 边界与失败模式
//!
//! - `EngineResult<T>` 就是 `rquickjs::Result<T>`：无法转换/校验失败的参数由原生方法抛 JS
//!   异常（可选参数按各自的默认值处理）。
//! - 引擎不可用/无事务时回调**静默降级**：样式函数在调用时查不到引擎就返回原文；
//!   `requestRender` 在构造回调时没有引擎就退化为空操作；autocomplete 退回宿主 provider
//!   （没有则返回「无建议」）。`adapter::JsComponent` 在引擎不可用时返回上一次成功渲染的
//!   行，因此 TUI 还能画出最后一帧。
//! - `with_tui_mut` 用裸指针取得 `&mut dyn Tui`，没有重入检测：若 `f` 内的 JS 回调再次调用
//!   代理方法（例如组件 `render` 里调用 `requestRender`），会同时存在两个 `&mut` 视图。
//! - [`install`] 重复调用会重建 `__kiana_refs`/`__kiana_theme_cache`，旧的引用 id 全部失效；
//!   上层只在引擎创建后调用一次（[`install_attached`]）。
//!
//! # 阅读顺序
//!
//! 主题（[`JsTheme`]、[`current_global_theme`]）→ 简单组件（`Text`/`Box`/`Spacer`）→
//! 交互组件（`Input`/`SelectList`/`SettingsList`/`Editor`）→ [`JsTui`]/[`JsTerminal`] →
//! [`JsUiBridge`] 与工厂桥 → `ui_host_kind`/`autocomplete_host_kind` → [`install`]。
//!
//! # 与 JS 侧的契约
//!
//! `js/ui/tui.js`/`js/ui/ui.js` 按名字读取 `__kiana_tui_native`/`__kiana_ui_native` 的成员，
//! 并定义 `__kiana_tui`、`__kiana_ctx_make_ui` 等全局。改动这里的方法名、参数顺序或返回值
//! 形状会直接破坏 JS 侧；`__kiana_tui` 又被 `virtual_modules` 的
//! `@earendil-works/kiana-tui` 虚拟 namespace 转发（`assembly::publish_kiana_tui_bridge`）。

pub mod adapter;
pub mod print_json;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use adapter::JsEditorComponent;
use kiana_config::keybindings::KeybindingsManager;
use kiana_ext_api::ui::{
    AutocompleteProviderFactory, CustomDoneFn, CustomFactory, CustomOptions, EditorFactory,
    ExtensionUIDialogOptions, ExtensionUiContext, ExtensionWidgetOptions, FooterFactory,
    HeaderFactory, NotifyKind, OverlayOptionsOrFn, TerminalInputHandler, TerminalInputResult,
    ThemeInfo, ThemeOrName, WidgetContent, WidgetFactory, WorkingIndicatorOptions,
};
use kiana_theme::theme::{Theme, ThemeBgValue, ThemeFg, ThemeStyle};
use kiana_theme::{ThemeBg, ThemeColor};
use kiana_tui::autocomplete::{AutocompleteItem, AutocompleteProvider};
use kiana_tui::components::box_component::BoxComponent;
use kiana_tui::components::cancellable_loader::CancellableLoader;
use kiana_tui::components::editor::{Editor, EditorOptions, EditorTheme};
use kiana_tui::components::image::{Image, ImageOptions, ImageTheme};
use kiana_tui::components::input::{Input, InputOptions};
use kiana_tui::components::loader::{Loader, LoaderIndicatorOptions};
use kiana_tui::components::markdown::{Markdown, MarkdownOptions, MarkdownTheme};
use kiana_tui::components::scroll_view::{
    ScrollView, ScrollViewOptions, ScrollViewScrollToOptions,
};
use kiana_tui::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme, TruncatePrimaryFn,
};
use kiana_tui::components::settings_list::{
    SettingItem, SettingsDoneCallback, SettingsList, SettingsListOptions, SettingsListTheme,
    SettingsNavigateOptions, SubmenuFactory,
};
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::{StyleFn, Text};
use kiana_tui::components::truncated_text::TruncatedText;
use kiana_tui::editor_component::{AutocompleteProviderObject, EditorComponent};
use kiana_tui::layout_node::ScrollbarVisibility;
/// 光标标记的渲染函数（`packages/tui/src/tui.ts` 的同名导出）：把文本包成伪光标转义序列。
///
/// 本文件把它注册成 `__kiana_tui_native.renderFakeCursor`，`js/ui/tui.js` 再以
/// `renderFakeCursor` 导出；这样扩展拿到的转义序列与 `kiana-tui` 组件内部使用的一致。
pub use kiana_tui::tui::render_fake_cursor;
use kiana_tui::tui::{
    Component, FocusTarget, Focusable, OverlayHandle, OverlayOptions, Tui, TuiStopOptions,
};
use rquickjs::class::Trace;
use rquickjs::function::{Opt, This};
use rquickjs::{Array, Class, Ctx, Exception, Function, JsLifetime, Object, Value};

use crate::engine::{EngineResult, JsEngine};
use crate::runner::noop_ui::is_no_op_ui_context;
use adapter::{
    ActiveCtxGuard, JsArg, JsComponent, JsEngineSlot, call_method, call_ref, call_ref_with_values,
    drop_ref, js_string, js_truthy, json_to_value, load_ref, opt_bool, opt_number, opt_string,
    store_ref, strings_from_array, value_to_json, with_engine_js,
};

/// `js/ui/tui.js`：`@earendil-works/kiana-tui` 的运行期面（组合类 + 原生类的 JS 包装）。
const TUI_JS: &str = include_str!("../../js/ui/tui.js");
/// `js/ui/ui.js`：`ctx.ui` 的 JS 桥。
const UI_JS: &str = include_str!("../../js/ui/ui.js");

/// 异步 UI 操作的 host kind（`js/ui/ui.js` 的 `__kiana.promise` 使用）。
pub const HOST_KIND: &str = "kiana_ui";

/// `setWidget` 字符串数组的最大行数（`interactive-mode.ts` 的 `MAX_WIDGET_LINES`）。
///
/// 本文件的 `setWidgetLines` **不**做截断：RPC 的 `setWidget` 记录保留调用方传入的完整数组
/// （`tests/extension_ui_context.rs` 的 `widget_lines_are_forwarded_verbatim_and_cleared`
/// 锁定 12 行原样转发）；截断发生在 interactive 模式的渲染层。
pub const MAX_WIDGET_LINES: usize = 10;
/// 超出行数时追加的固定文本（`interactive-mode.ts:2427`）。
///
/// 与 [`MAX_WIDGET_LINES`] 一样由交互式渲染层使用，本文件的 JS 桥只转发原始行数组。
pub const WIDGET_TRUNCATED: &str = "... (widget truncated)";

/// 主题的原生实例（`ctx.ui.theme` / `getTheme` / symbol 全局）。
///
/// 构造期不安装任何 JS 可见字段（§5.3）；全部成员是原型上的方法。
///
/// **两种实例**（`theme.ts:648-663`）：
/// - `live = true`：`ctx.ui.theme`、工厂收到的主题、全局 symbol。TS 里它们是模块 `Proxy`，
///   每次属性读取都转发到 `globalThis[Symbol.for(…)]`，因此**缓存引用**在 `setTheme`
///   之后仍然有效；这里每个方法都读 [`current_global_theme`]。
/// - `live = false`：`getTheme(name)` 的返回值（TS 返回真实 `Theme` 实例），保持快照语义。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "Theme")]
pub struct JsTheme {
    #[qjs(skip_trace)]
    theme: Arc<Theme>,
    #[qjs(skip_trace)]
    live: bool,
}

impl JsTheme {
    /// 本次调用读取的主题（live 实例跟随全局切换）。
    fn theme_arc(&self) -> Arc<Theme> {
        if self.live {
            current_global_theme()
        } else {
            Arc::clone(&self.theme)
        }
    }
}

#[rquickjs::methods]
impl JsTheme {
    /// `fg(color, text)`：前景 token 着色。
    #[qjs(rename = "fg")]
    fn fg(&self, ctx: Ctx<'_>, color: String, text: String) -> EngineResult<String> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let Some(token) = ThemeColor::from_str(&color) else {
            return Err(Exception::throw_message(
                &ctx,
                &format!("Unknown theme color: {color}"),
            ));
        };
        Ok(self.theme_arc().fg(token, &text))
    }

    /// `bg(color, text)`：背景 token 着色。
    #[qjs(rename = "bg")]
    fn bg(&self, ctx: Ctx<'_>, color: String, text: String) -> EngineResult<String> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(token) = ThemeBg::from_str(&color) else {
            return Err(Exception::throw_message(
                &ctx,
                &format!("Unknown theme color: {color}"),
            ));
        };
        Ok(self.theme_arc().bg(token, &text))
    }

    /// `getFgAnsi(color)`：前景 token 的转义序列。
    #[qjs(rename = "getFgAnsi")]
    fn get_fg_ansi(&self, ctx: Ctx<'_>, color: String) -> EngineResult<String> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(token) = ThemeColor::from_str(&color) else {
            return Err(Exception::throw_message(
                &ctx,
                &format!("Unknown theme color: {color}"),
            ));
        };
        Ok(self.theme_arc().get_fg_ansi(token))
    }

    /// `getBgAnsi(color)`：背景 token 的转义序列。
    #[qjs(rename = "getBgAnsi")]
    fn get_bg_ansi(&self, ctx: Ctx<'_>, color: String) -> EngineResult<String> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(token) = ThemeBg::from_str(&color) else {
            return Err(Exception::throw_message(
                &ctx,
                &format!("Unknown theme color: {color}"),
            ));
        };
        Ok(self.theme_arc().get_bg_ansi(token))
    }

    /// `getColorMode()`：`"truecolor"` / `"256color"`。
    #[qjs(rename = "getColorMode")]
    fn get_color_mode(&self) -> &'static str {
        match self.theme_arc().get_color_mode() {
            kiana_tui::TerminalColorMode::Truecolor => "truecolor",
            kiana_tui::TerminalColorMode::Palette256 => "256color",
        }
    }

    /// `bold(text)`。
    #[qjs(rename = "bold")]
    fn bold(&self, text: String) -> String {
        self.theme_arc().bold(&text)
    }

    /// `italic(text)`。
    #[qjs(rename = "italic")]
    fn italic(&self, text: String) -> String {
        self.theme_arc().italic(&text)
    }

    /// `underline(text)`。
    #[qjs(rename = "underline")]
    fn underline(&self, text: String) -> String {
        self.theme_arc().underline(&text)
    }

    /// `inverse(text)`。
    #[qjs(rename = "inverse")]
    fn inverse(&self, text: String) -> String {
        self.theme_arc().inverse(&text)
    }

    /// `strikethrough(text)`。
    #[qjs(rename = "strikethrough")]
    fn strikethrough(&self, text: String) -> String {
        self.theme_arc().strikethrough(&text)
    }

    /// `getThinkingBorderColor(level)`：返回一个着色函数。
    #[qjs(rename = "getThinkingBorderColor")]
    fn get_thinking_border_color<'js>(
        &self,
        ctx: Ctx<'js>,
        level: String,
    ) -> EngineResult<Function<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let color = self.theme_arc().get_thinking_border_color(&level);
        Function::new(ctx, move |text: String| -> String { color(&text) })
    }

    /// `getBashModeBorderColor()`：返回一个着色函数。
    #[qjs(rename = "getBashModeBorderColor")]
    fn get_bash_mode_border_color<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Function<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let color = self.theme_arc().get_bash_mode_border_color();
        Function::new(ctx, move |text: String| -> String { color(&text) })
    }

    /// `style(text, options)`（TS `ThemeStyle`）。
    #[qjs(rename = "style")]
    fn style<'js>(
        &self,
        ctx: Ctx<'js>,
        text: String,
        options: Object<'js>,
    ) -> EngineResult<String> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let style = theme_style_from_object(&ctx, &options)?;
        self.theme_arc()
            .style(&text, &style)
            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))
    }

    /// `colors`：全部 token 的具体颜色（`{ kind: "rgb"|"indexed"|"oklch", ... }`）。
    #[qjs(get, rename = "colors")]
    fn colors<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let colors = self.theme_arc().colors();
        let object = Object::new(ctx.clone())?;
        for (token, color) in colors.iter() {
            object.set(
                token.as_str(),
                json_to_value(&ctx, &serde_json::to_value(color).unwrap_or_default())?,
            )?;
        }
        Ok(object.into_value())
    }

    /// `appearance`：`"dark"` / `"light"`。
    #[qjs(get, rename = "appearance")]
    fn appearance(&self) -> &'static str {
        match self.theme_arc().appearance() {
            kiana_theme::ThemeAppearance::Dark => "dark",
            kiana_theme::ThemeAppearance::Light => "light",
        }
    }

    /// `name`：主题名（内置主题为 `undefined`? TS 里 `name?`）。
    #[qjs(get, rename = "name")]
    fn name<'js>(&self, ctx: Ctx<'js>) -> Value<'js> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match self.theme_arc().name() {
            Some(name) => rquickjs::String::from_str(ctx.clone(), name)
                .map(|value| value.into_value())
                .unwrap_or_else(|_| Value::new_undefined(ctx.clone())),
            None => Value::new_undefined(ctx),
        }
    }

    /// `sourcePath`：主题文件路径（内置主题为 `undefined`）。
    #[qjs(get, rename = "sourcePath")]
    fn source_path<'js>(&self, ctx: Ctx<'js>) -> Value<'js> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match self.theme_arc().source_path() {
            Some(path) => rquickjs::String::from_str(ctx.clone(), &path.to_string_lossy())
                .map(|value| value.into_value())
                .unwrap_or_else(|_| Value::new_undefined(ctx.clone())),
            None => Value::new_undefined(ctx),
        }
    }
}

/// 把 `Theme` 包装成 JS 原生实例（**快照**语义，`getTheme(name)` 用）。
///
/// 同一 `Arc` 指针复用同一个 JS 对象；实例不跟随 `setTheme` 变化（TS 的 `getThemeByName`
/// 也是返回当时的真实 `Theme` 实例）。需要跟随全局主题的引用请用 [`live_theme_value`]。
///
/// 前置条件：[`install`] 已创建 `__kiana_theme_cache`（否则读全局对象失败，返回
/// `EngineResult` 错误）。缓存没有淘汰路径，条目随引擎销毁。
pub fn theme_to_value<'js>(ctx: &Ctx<'js>, theme: Arc<Theme>) -> EngineResult<Value<'js>> {
    let key = format!("{}", Arc::as_ptr(&theme) as usize);
    let cache: Object = ctx.globals().get("__kiana_theme_cache")?;
    let cached: Option<Value> = cache.get(key.as_str())?;
    if let Some(value) = cached
        && !value.is_undefined()
    {
        return Ok(value);
    }
    let instance = Class::instance(ctx.clone(), JsTheme { theme, live: false })?;
    let value = instance.into_value();
    cache.set(key.as_str(), value.clone())?;
    Ok(value)
}

/// 当前主题的 **live** 实例（TS 的模块 `Proxy`，`theme.ts:648-663`）。
///
/// 每个成员读取都转发到 [`current_global_theme`]，因此扩展缓存的引用在 `setTheme` /
/// 用户主题切换之后仍然渲染新主题。`ctx.ui.theme`、工厂收到的主题与两个 `Symbol.for`
/// 全局都用它（同一个 JS 对象）。
pub fn live_theme_value<'js>(ctx: &Ctx<'js>) -> EngineResult<Value<'js>> {
    let cached: Option<Value> = ctx.globals().get("__kiana_live_theme")?;
    if let Some(value) = cached
        && !value.is_undefined()
    {
        return Ok(value);
    }
    let instance = Class::instance(
        ctx.clone(),
        JsTheme {
            theme: current_global_theme(),
            live: true,
        },
    )?;
    let value = instance.into_value();
    ctx.globals().set("__kiana_live_theme", value.clone())?;
    Ok(value)
}

/// 当前全局主题（`kiana_theme::theme::current_theme`，未初始化时回退内置 `dark`）。
///
/// 未初始化时的 `dark` 缓存为进程级 `Arc`：`ctx.ui.theme` 与 `Symbol.for(...)` 全局必须
/// 返回**同一个**原生实例（§5.5），而 `get_theme_by_name` 每次都会构造新的 `Theme`。
pub fn current_global_theme() -> Arc<Theme> {
    static FALLBACK: std::sync::LazyLock<Arc<Theme>> = std::sync::LazyLock::new(|| {
        kiana_theme::theme::get_theme_by_name("dark")
            .expect("the builtin dark theme must always be available")
    });
    kiana_theme::theme::current_theme().unwrap_or_else(|| Arc::clone(&FALLBACK))
}

/// 把当前主题登记到两个 `Symbol.for` 键上（§5.5；旧键 `@mariozechner/...` 也要有）。
pub fn register_theme_symbols(ctx: &Ctx<'_>) -> EngineResult<()> {
    let value = live_theme_value(ctx)?;
    // `Object::set` 的键是 QuickJS atom（字符串/数字），Symbol 不是 atom；Symbol 键必须走
    // `globalThis[Symbol.for(key)] = value`。这里建一次 helper（全局缓存）。
    let helper: Option<Function> = ctx.globals().get("__kiana_set_theme_symbol")?;
    let helper = match helper {
        Some(helper) => helper,
        None => {
            let helper: Function =
                ctx.eval("(function (key, value) { globalThis[Symbol.for(key)] = value; })")?;
            ctx.globals()
                .set("__kiana_set_theme_symbol", helper.clone())?;
            helper
        }
    };
    for key in [
        "@earendil-works/kiana-coding-agent:theme",
        "@mariozechner/kiana-coding-agent:theme",
    ] {
        helper.call::<_, ()>((key, value.clone()))?;
    }
    Ok(())
}

/// 解析 TS `ThemeStyle` 对象。
///
/// `fg`/`bg` 接受两种形状：token 名（`"accent"` 等，经 `ThemeColor::from_str`/
/// `ThemeBg::from_str`）或 JSON 颜色对象（直接反序列化成 `kiana_tui::Color`）。token 名非法
/// 时抛 JS 异常；字段是既非字符串也无法反序列化成颜色的值时该颜色留空（不报错）。
/// 六个布尔属性（bold/dim/italic/underline/inverse/strikethrough）走 `opt_bool`，缺失保持 `None`。
fn theme_style_from_object<'js>(ctx: &Ctx<'js>, options: &Object<'js>) -> EngineResult<ThemeStyle> {
    let mut style = ThemeStyle::new();
    if let Some(value) = options.get::<_, Option<Value>>("fg")? {
        if let Some(name) = value.as_string().and_then(|text| text.to_string().ok()) {
            if let Some(token) = ThemeColor::from_str(&name) {
                style.fg = Some(ThemeFg::Token(token));
            } else {
                return Err(Exception::throw_message(
                    ctx,
                    &format!("Unknown theme color: {name}"),
                ));
            }
        } else if let Ok(color) =
            serde_json::from_value::<kiana_tui::Color>(value_to_json(ctx, value)?)
        {
            style.fg = Some(ThemeFg::Color(color));
        }
    }
    if let Some(value) = options.get::<_, Option<Value>>("bg")? {
        if let Some(name) = value.as_string().and_then(|text| text.to_string().ok()) {
            if let Some(token) = ThemeBg::from_str(&name) {
                style.bg = Some(ThemeBgValue::Token(token));
            } else {
                return Err(Exception::throw_message(
                    ctx,
                    &format!("Unknown theme color: {name}"),
                ));
            }
        } else if let Ok(color) =
            serde_json::from_value::<kiana_tui::Color>(value_to_json(ctx, value)?)
        {
            style.bg = Some(ThemeBgValue::Color(color));
        }
    }
    let attributes = &mut style.attributes;
    attributes.bold = opt_bool(options, "bold")?;
    attributes.dim = opt_bool(options, "dim")?;
    attributes.italic = opt_bool(options, "italic")?;
    attributes.underline = opt_bool(options, "underline")?;
    attributes.inverse = opt_bool(options, "inverse")?;
    attributes.strikethrough = opt_bool(options, "strikethrough")?;
    Ok(style)
}

// ===========================================================================
// JS 值 → Rust 的小工具
// ===========================================================================

/// 读取 optional 参数为 `Option<Value>`（缺失 / `undefined` / `null` 都是 `None`）。
fn opt_value<'js>(value: &Opt<Value<'js>>) -> Option<Value<'js>> {
    match value {
        Opt(Some(value)) if !value.is_undefined() && !value.is_null() => Some(value.clone()),
        _ => None,
    }
}

/// optional 字符串参数。
fn opt_arg_string<'js>(value: &Opt<Value<'js>>) -> Option<String> {
    opt_value(value).and_then(|value| js_string(&value).ok())
}

/// optional 数字参数（JS `Number()` 语义；缺失/`null`/转换抛错都得到 `None`）。
fn opt_arg_number<'js>(ctx: &Ctx<'js>, value: &Opt<Value<'js>>) -> Option<f64> {
    let value = opt_value(value)?;
    let number: f64 = ctx
        .globals()
        .get::<_, Function<'js>>("Number")
        .ok()?
        .call((value,))
        .ok()?;
    Some(number)
}

/// `Math.max(0, Math.floor(v))`（TS 组件的 padding 归一）；非有限值（NaN/±∞）按 0 处理，
/// 这是 JS 版本没有的分支（JS 会让 NaN 继续传播）。
fn non_negative_floor(value: f64) -> usize {
    if !value.is_finite() {
        return 0;
    }
    value.max(0.0).floor() as usize
}

/// `Math.max(0, Math.floor(v))`，非有限值回退 `fallback`。负数同样钳到 0。
fn floor_or(value: f64, fallback: usize) -> usize {
    if value.is_finite() {
        value.max(0.0).floor() as usize
    } else {
        fallback
    }
}

/// 把行数组转换成 JS 数组。
fn lines_to_js<'js>(ctx: &Ctx<'js>, lines: Vec<String>) -> EngineResult<Array<'js>> {
    let array = Array::new(ctx.clone())?;
    for (index, line) in lines.into_iter().enumerate() {
        array.set(index, rquickjs::String::from_str(ctx.clone(), &line)?)?;
    }
    Ok(array)
}

/// 把字符串数组转换成 JS 数组。
fn strings_to_js<'js>(ctx: &Ctx<'js>, lines: &[String]) -> EngineResult<Array<'js>> {
    let array = Array::new(ctx.clone())?;
    for (index, line) in lines.iter().enumerate() {
        array.set(index, rquickjs::String::from_str(ctx.clone(), line)?)?;
    }
    Ok(array)
}

/// 读取 JS 数组为字符串列表（每一项按 `String()` 转换）。
fn array_strings<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> EngineResult<Vec<String>> {
    match value.as_array() {
        Some(array) => strings_from_array(array),
        None => Err(Exception::throw_message(ctx, "expected an array")),
    }
}

/// 从 JS 值中取一个函数并保存为引用。
fn store_function_ref<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> EngineResult<Option<u64>> {
    if value.as_function().is_none() {
        return Ok(None);
    }
    Ok(Some(store_ref(ctx, value)?))
}

/// 从对象属性取函数引用（属性缺失/非函数 → `None`）。
fn object_fn_ref<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    key: &str,
) -> EngineResult<Option<u64>> {
    let value: Value = object.get(key)?;
    store_function_ref(ctx, value)
}

/// 引用 id → `Fn(&str) -> String`（主题样式函数、边框颜色等）。
///
/// 回调可能在任意 Rust 渲染路径里被调用；引擎已销毁或当前线程没有引擎时返回原文。
/// 调用常常发生在「JS 调原生 render」的重入路径里（`Box::render` 等）：那时事务已存在，
/// `with_engine_js` 复用当前 `Ctx`；直接 `with_js` 会因 runtime 已借用而失败。回调抛错或
/// 返回值不是字符串时同样退回原文。
fn style_fn_from_ref(reference: u64) -> StyleFn {
    Arc::new(move |text: &str| {
        let Some(engine) = JsEngineSlot::current() else {
            return text.to_owned();
        };
        let text_owned = text.to_owned();
        with_engine_js(&engine, |ctx| -> Option<String> {
            let value: Option<rquickjs::String> =
                call_ref(&ctx, reference, None, &[JsArg::String(text_owned.clone())]).ok();
            value.and_then(|text| text.to_string().ok())
        })
        .unwrap_or_else(|| text.to_owned())
    })
}

/// 引用 id → `Fn(&str) -> String`（`Send + Sync` 形式，`ScrollViewOptions` 用）。
///
/// 与 [`style_fn_from_ref`] 的降级规则相同，只是 `ScrollViewOptions` 的字段要求闭包
/// `Send + Sync`（实际调用仍在创建它的 UI 线程上）。
fn send_style_fn_from_ref(reference: u64) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
    Arc::new(move |text: &str| {
        let Some(engine) = JsEngineSlot::current() else {
            return text.to_owned();
        };
        let text_owned = text.to_owned();
        with_engine_js(&engine, |ctx| -> Option<String> {
            let value: Option<rquickjs::String> =
                call_ref(&ctx, reference, None, &[JsArg::String(text_owned.clone())]).ok();
            value.and_then(|text| text.to_string().ok())
        })
        .unwrap_or_else(|| text.to_owned())
    })
}

/// `(text, selected) -> String`（`SettingsListTheme` 的 label/value）。
fn setting_text_fn_from_ref(reference: u64) -> kiana_tui::components::settings_list::SettingTextFn {
    Arc::new(move |text: &str, selected: bool| {
        let Some(engine) = JsEngineSlot::current() else {
            return text.to_owned();
        };
        let text_owned = text.to_owned();
        with_engine_js(&engine, |ctx| -> Option<String> {
            let value: Option<rquickjs::String> = call_ref(
                &ctx,
                reference,
                None,
                &[JsArg::String(text_owned.clone()), JsArg::Bool(selected)],
            )
            .ok();
            value.and_then(|text| text.to_string().ok())
        })
        .unwrap_or_else(|| text.to_owned())
    })
}

/// 从 JS 主题对象读取五个样式函数（`SelectListTheme`）。
///
/// 缺失的成员保持 `SelectListTheme::plain()` 的恒等函数；对象里其它字段被忽略。
fn select_list_theme_from_object<'js>(
    ctx: &Ctx<'js>,
    object: Option<&Object<'js>>,
) -> EngineResult<SelectListTheme> {
    let Some(object) = object else {
        return Ok(SelectListTheme::plain());
    };
    let identity: StyleFn = Arc::new(|text: &str| text.to_string());
    let mut theme = SelectListTheme::plain();
    if let Some(reference) = object_fn_ref(ctx, object, "selectedPrefix")? {
        theme.selected_prefix = style_fn_from_ref(reference);
    } else {
        theme.selected_prefix = Arc::clone(&identity);
    }
    if let Some(reference) = object_fn_ref(ctx, object, "selectedText")? {
        theme.selected_text = style_fn_from_ref(reference);
    }
    if let Some(reference) = object_fn_ref(ctx, object, "description")? {
        theme.description = style_fn_from_ref(reference);
    }
    if let Some(reference) = object_fn_ref(ctx, object, "scrollInfo")? {
        theme.scroll_info = style_fn_from_ref(reference);
    }
    if let Some(reference) = object_fn_ref(ctx, object, "noMatch")? {
        theme.no_match = style_fn_from_ref(reference);
    }
    Ok(theme)
}

/// 从 JS 主题对象读取 `SettingsListTheme`。
///
/// `label`/`value` 是 `(text, selected) -> string`，`description`/`hint` 是
/// `(text) -> string`；`cursor` 是普通字符串。缺失成员保持 `plain()` 的默认值。
fn settings_list_theme_from_object<'js>(
    ctx: &Ctx<'js>,
    object: Option<&Object<'js>>,
) -> EngineResult<SettingsListTheme> {
    let mut theme = SettingsListTheme::plain();
    let Some(object) = object else {
        return Ok(theme);
    };
    if let Some(reference) = object_fn_ref(ctx, object, "label")? {
        theme.label = setting_text_fn_from_ref(reference);
    }
    if let Some(reference) = object_fn_ref(ctx, object, "value")? {
        theme.value = setting_text_fn_from_ref(reference);
    }
    if let Some(reference) = object_fn_ref(ctx, object, "description")? {
        theme.description = style_fn_from_ref(reference);
    }
    if let Some(reference) = object_fn_ref(ctx, object, "hint")? {
        theme.hint = style_fn_from_ref(reference);
    }
    if let Some(cursor) = opt_string(object, "cursor")? {
        theme.cursor = cursor;
    }
    Ok(theme)
}

/// 从 JS 主题对象读取 `MarkdownTheme`。
///
/// 基线是 `kiana_theme::theme::get_markdown_theme()`，对象里出现的字段逐个覆盖；
/// `highlightCode`/`codeBlockIndent` 是 TS 的可选扩展字段，只在 JS 提供时设置。
fn markdown_theme_from_object<'js>(
    ctx: &Ctx<'js>,
    object: Option<&Object<'js>>,
) -> EngineResult<MarkdownTheme> {
    let plain = kiana_theme::theme::get_markdown_theme();
    let Some(object) = object else {
        return Ok(plain);
    };
    let mut theme = plain;
    for (key, slot) in [
        ("heading", 0usize),
        ("link", 1),
        ("linkUrl", 2),
        ("code", 3),
        ("codeBlock", 4),
        ("codeBlockBorder", 5),
        ("quote", 6),
        ("quoteBorder", 7),
        ("hr", 8),
        ("listBullet", 9),
    ] {
        if let Some(reference) = object_fn_ref(ctx, object, key)? {
            let function = style_fn_from_ref(reference);
            match slot {
                0 => theme.heading = function,
                1 => theme.link = function,
                2 => theme.link_url = function,
                3 => theme.code = function,
                4 => theme.code_block = function,
                5 => theme.code_block_border = function,
                6 => theme.quote = function,
                7 => theme.quote_border = function,
                8 => theme.hr = function,
                _ => theme.list_bullet = function,
            }
        }
    }
    if let Some(reference) = object_fn_ref(ctx, object, "bold")? {
        theme.bold = style_fn_from_ref(reference);
    }
    if let Some(reference) = object_fn_ref(ctx, object, "italic")? {
        theme.italic = style_fn_from_ref(reference);
    }
    if let Some(reference) = object_fn_ref(ctx, object, "strikethrough")? {
        theme.strikethrough = style_fn_from_ref(reference);
    }
    if let Some(reference) = object_fn_ref(ctx, object, "underline")? {
        theme.underline = style_fn_from_ref(reference);
    }
    // `highlightCode?(code, lang?) => string[]`（`markdown.ts:237`）：TS 的 `getMarkdownTheme()`
    // 总是带 cli-highlight 实现（`theme.ts:1049-1059`），扩展把它原样转交给 `Markdown`。
    if let Some(reference) = object_fn_ref(ctx, object, "highlightCode")? {
        theme.highlight_code = Some(highlight_fn_from_ref(reference));
    }
    if let Some(indent) = opt_string(object, "codeBlockIndent")? {
        theme.code_block_indent = Some(indent);
    }
    Ok(theme)
}

/// 引用 id → `TransformFn`（`(markdown, availableWidth) => string`）。
///
/// 回调抛错、返回非字符串或没有引擎时返回原 markdown（渲染不能因为扩展的 transform 失败
/// 而中断）。
fn transform_fn_from_ref(reference: u64) -> kiana_tui::components::markdown::TransformFn {
    Arc::new(move |markdown: &str, available_width: usize| {
        let Some(engine) = JsEngineSlot::current() else {
            return markdown.to_owned();
        };
        let markdown_owned = markdown.to_owned();
        with_engine_js(&engine, |ctx| -> Option<String> {
            let value: Option<Value> = call_ref(
                &ctx,
                reference,
                None,
                &[
                    JsArg::String(markdown_owned.clone()),
                    JsArg::num(available_width as f64),
                ],
            )
            .ok()?;
            let value = value?;
            if !value.is_string() {
                return None;
            }
            js_string(&value).ok()
        })
        .unwrap_or_else(|| markdown.to_owned())
    })
}

/// 引用 id → `HighlightFn`（`(code, lang?) => string[]`）。
///
/// 回调必须返回字符串数组；抛错、返回非数组或没有引擎时退化为按 `\n` 切分的原文。
fn highlight_fn_from_ref(reference: u64) -> kiana_tui::components::markdown::HighlightFn {
    Arc::new(move |code: &str, lang: Option<&str>| {
        let Some(engine) = JsEngineSlot::current() else {
            return code.split('\n').map(str::to_owned).collect();
        };
        let code_owned = code.to_owned();
        let lang_owned = lang.map(str::to_owned);
        with_engine_js(&engine, |ctx| -> Option<Vec<String>> {
            let lang_arg = match &lang_owned {
                Some(lang) => JsArg::String(lang.clone()),
                None => JsArg::Undefined,
            };
            let value: Option<Value> = call_ref(
                &ctx,
                reference,
                None,
                &[JsArg::String(code_owned.clone()), lang_arg],
            )
            .ok()?;
            let value = value?;
            let array = value.as_array()?;
            strings_from_array(array).ok()
        })
        .unwrap_or_else(|| code.split('\n').map(str::to_owned).collect())
    })
}

/// 从 JS 主题对象读取 `EditorTheme`（`borderColor` + `selectList`）。
///
/// 返回的第二个值是 `borderColor` 的引用 id：`JsEditor` 要把它暴露成 JS 的
/// `borderColor` 属性，并在渲染期经 `dispatch_border` 使用；没有该函数时为 `None`
/// （调用方会换成恒等函数）。
fn editor_theme_from_object<'js>(
    ctx: &Ctx<'js>,
    object: Option<&Object<'js>>,
) -> EngineResult<(EditorTheme, Option<u64>)> {
    let mut theme = EditorTheme::plain();
    let mut border_reference = None;
    let Some(object) = object else {
        return Ok((theme, border_reference));
    };
    if let Some(reference) = object_fn_ref(ctx, object, "borderColor")? {
        theme.border_color = Arc::new(move |text: &str| {
            let Some(engine) = JsEngineSlot::current() else {
                return text.to_owned();
            };
            let text_owned = text.to_owned();
            with_engine_js(&engine, |ctx| -> Option<String> {
                let value: Option<rquickjs::String> =
                    call_ref(&ctx, reference, None, &[JsArg::String(text_owned.clone())]).ok();
                value.and_then(|text| text.to_string().ok())
            })
            .unwrap_or_else(|| text.to_owned())
        });
        border_reference = Some(reference);
    }
    let select_list: Option<Object<'js>> = object.get("selectList")?;
    theme.select_list = select_list_theme_from_object(ctx, select_list.as_ref())?;
    Ok((theme, border_reference))
}

/// 共享的原始 JS 条目表：`(引用 id, 解析后的字段)`。`TruncatePrimaryFn` 是
/// `Send + Sync`，所以用 `Arc<Mutex<…>>` 而不是 `Rc<RefCell<…>>`（实际只在本线程访问）。
type SharedSelectItems = Arc<std::sync::Mutex<Vec<(u64, SelectItem)>>>;

/// 找到与 `item` 对应的原始 JS 对象（字段全等；TS 传的是同一个对象）。
///
/// 字段全等的条目有多份时返回第一个匹配项（TS 按对象身份区分，这里无法区分）；一个都
/// 找不到时用 [`select_item_to_js`] 重建普通对象（会丢失扩展附加的额外字段）。
fn select_item_value<'js>(
    ctx: &Ctx<'js>,
    items: &SharedSelectItems,
    item: &SelectItem,
) -> EngineResult<Value<'js>> {
    {
        let guard = items.lock().unwrap_or_else(|error| error.into_inner());
        for (reference, original) in guard.iter() {
            if original.value == item.value
                && original.label == item.label
                && original.description == item.description
            {
                return load_ref(ctx, *reference);
            }
        }
    }
    select_item_to_js(ctx, item)
}

/// 替换回调槽：写入新引用后释放旧引用（TS 依赖 GC 回收旧闭包）。
fn replace_callback_ref<'js>(
    ctx: &Ctx<'js>,
    slot: &RefCell<Option<u64>>,
    value: Opt<Value<'js>>,
) -> EngineResult<()> {
    let reference = match opt_value(&value) {
        Some(value) => store_function_ref(ctx, value)?,
        None => None,
    };
    let previous = slot.replace(reference);
    if let Some(previous) = previous
        && Some(previous) != reference
    {
        adapter::release_ref(previous);
    }
    Ok(())
}

/// 引用 id → `TruncatePrimaryFn`。
///
/// `items` 保存每个原始 JS 条目（`__kiana_refs` 引用 id + 解析后的字段）：TS 的
/// `truncatePrimary(context)` 收到的 `context.item` 是扩展传入的**那个对象**
/// （`select-list.ts:243-251`），额外字段与对象身份都要保留。
fn truncate_primary_from_ref(reference: u64, items: SharedSelectItems) -> TruncatePrimaryFn {
    Arc::new(move |context| {
        let Some(engine) = JsEngineSlot::current() else {
            return context.text.clone();
        };
        with_engine_js(&engine, |ctx| -> Option<String> {
            let item_value = select_item_value(&ctx, &items, &context.item).ok()?;
            let payload = Object::new(ctx.clone()).ok()?;
            payload.set("text", context.text.clone()).ok()?;
            payload.set("maxWidth", context.max_width).ok()?;
            payload.set("columnWidth", context.column_width).ok()?;
            payload.set("item", item_value).ok()?;
            payload.set("isSelected", context.is_selected).ok()?;
            let value =
                adapter::call_ref_with_values(&ctx, reference, None, vec![payload.into_value()])
                    .ok()?;
            js_string(&value).ok()
        })
        .unwrap_or_else(|| context.text.clone())
    })
}

// ===========================================================================
// 鼠标事件桥
// ===========================================================================

/// JS 事件对象 → [`kiana_tui::tui::TuiMouseEvent`]。
fn mouse_event_from_js<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
) -> EngineResult<kiana_tui::tui::TuiMouseEvent> {
    let json = value_to_json(ctx, value)?;
    serde_json::from_value(json)
        .map_err(|error| Exception::throw_message(ctx, &format!("invalid mouse event: {error}")))
}

/// [`kiana_tui::tui::TuiMouseEventResult`] → JS 对象（`undefined` 原样返回）。
///
/// TS 的组件结果只有 `handled`/`capture`/`focus`/`render` 四个布尔成员；`focusTarget`
/// 是组件引用，由 `Container.handleMouse`/`dispatchMouseEvent` 在 JS 侧补上
/// （`tui.ts:81-104、383-400`）。Rust 的 [`kiana_tui::tui::MouseFocusTarget`] 是下标/地址，
/// 不能转成组件对象，因此这里省略该字段（JS 容器会按命中的子组件重新写入）。
fn mouse_result_to_js<'js>(
    ctx: &Ctx<'js>,
    result: Option<kiana_tui::tui::TuiMouseEventResult>,
) -> EngineResult<Value<'js>> {
    let Some(result) = result else {
        return Ok(Value::new_undefined(ctx.clone()));
    };
    let object = Object::new(ctx.clone())?;
    if let Some(handled) = result.handled {
        object.set("handled", handled)?;
    }
    if let Some(capture) = result.capture {
        object.set("capture", capture)?;
    }
    if let Some(focus) = result.focus {
        object.set("focus", focus)?;
    }
    if let Some(render) = result.render {
        object.set("render", render)?;
    }
    Ok(object.into_value())
}

/// 读取 JS 对象上的函数并调用，返回值丢弃；属性缺失时静默跳过。
fn call_object_method<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    name: &str,
    args: &[JsArg],
) -> EngineResult<()> {
    call_method::<Value>(ctx, object, name, args).map(|_| ())
}

// ===========================================================================
// Text
// ===========================================================================

/// `@earendil-works/kiana-tui` 的 `Text`。
///
/// 构造参数按 JS 语义归一：缺失的 `text` 变成空串，`paddingX`/`paddingY` 缺失时为 1，
/// 负数/非有限值经 `non_negative_floor` 落到 0。`customBgFn` 是渲染期回调 JS 的样式函数
/// （引擎不可用时原样返回文本）。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "Text")]
pub struct JsText {
    #[qjs(skip_trace)]
    inner: RefCell<Text>,
}

#[rquickjs::methods]
impl JsText {
    /// `new Text(text = "", paddingX = 1, paddingY = 1, customBgFn?)`。
    #[qjs(constructor)]
    fn new<'js>(
        ctx: Ctx<'js>,
        text: Opt<Value<'js>>,
        padding_x: Opt<Value<'js>>,
        padding_y: Opt<Value<'js>>,
        custom_bg: Opt<Value<'js>>,
    ) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let text_value = opt_arg_string(&text).unwrap_or_default();
        let padding_x = opt_arg_number(&ctx, &padding_x)
            .map(non_negative_floor)
            .unwrap_or(1);
        let padding_y = opt_arg_number(&ctx, &padding_y)
            .map(non_negative_floor)
            .unwrap_or(1);
        let custom_bg = match opt_value(&custom_bg) {
            Some(value) => store_function_ref(&ctx, value)?.map(style_fn_from_ref),
            None => None,
        };
        Ok(JsText {
            inner: RefCell::new(Text::with_background(
                text_value, padding_x, padding_y, custom_bg,
            )),
        })
    }

    /// `setText(text)`。
    #[qjs(rename = "setText")]
    fn set_text<'js>(&self, ctx: Ctx<'js>, text: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let text = opt_arg_string(&text).unwrap_or_default();
        let _ = ctx;
        self.inner.borrow_mut().set_text(text);
        Ok(())
    }

    /// `setCustomBgFn(fn?)`。
    #[qjs(rename = "setCustomBgFn")]
    fn set_custom_bg_fn<'js>(&self, ctx: Ctx<'js>, custom_bg: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let function = match opt_value(&custom_bg) {
            Some(value) => store_function_ref(&ctx, value)?.map(style_fn_from_ref),
            None => None,
        };
        self.inner.borrow_mut().set_custom_bg_fn(function);
        Ok(())
    }

    /// `setPaddingX(paddingX)`。
    #[qjs(rename = "setPaddingX")]
    fn set_padding_x<'js>(&self, ctx: Ctx<'js>, padding_x: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let padding = opt_arg_number(&ctx, &padding_x)
            .map(non_negative_floor)
            .unwrap_or(0);
        self.inner.borrow_mut().set_padding_x(padding);
        Ok(())
    }

    /// `invalidate()`。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self) {
        self.inner.borrow_mut().invalidate();
    }

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let lines = Component::render(&mut *self.inner.borrow_mut(), width);
        lines_to_js(&ctx, lines)
    }
}

// ===========================================================================
// Box
// ===========================================================================

/// `@earendil-works/kiana-tui` 的 `Box`。
///
/// `addChild` 接受任意 JS 对象并包成 `adapter::JsComponent`；`removeChild` 先按对象身份在
/// 引用表里找到同一个 JS 对象，再在子项列表里按引用 id 定位，摘除时只 `detach()`、不调用
/// JS `dispose()`（TS `Container.removeChild` 只做 `splice`），因此组件可以再次挂载。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "Box")]
pub struct JsBox {
    #[qjs(skip_trace)]
    inner: RefCell<BoxComponent>,
}

impl JsBox {
    /// 把 JS 组件对象包装成 `Box<dyn Component>`。
    ///
    /// 非对象直接抛错；引擎未登记时先释放刚保存的引用再抛错（避免失败路径留下引用表项）。
    fn wrap_child<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> EngineResult<Box<dyn Component>> {
        let Some(object) = value.as_object() else {
            return Err(Exception::throw_message(ctx, "component is not an object"));
        };
        let reference = store_ref(ctx, object.clone().into_value())?;
        let Some(engine) = JsEngineSlot::current() else {
            drop_ref(ctx, reference);
            return Err(Exception::throw_message(
                ctx,
                "Cannot add a component: the extension runtime is not available",
            ));
        };
        Ok(Box::new(JsComponent::from_ref(&engine, reference)))
    }
}

#[rquickjs::methods]
impl JsBox {
    /// `new Box(paddingX = 1, paddingY = 1, bgFn?)`。
    #[qjs(constructor)]
    fn new<'js>(
        ctx: Ctx<'js>,
        padding_x: Opt<Value<'js>>,
        padding_y: Opt<Value<'js>>,
        bg_fn: Opt<Value<'js>>,
    ) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let padding_x = opt_arg_number(&ctx, &padding_x)
            .map(non_negative_floor)
            .unwrap_or(1);
        let padding_y = opt_arg_number(&ctx, &padding_y)
            .map(non_negative_floor)
            .unwrap_or(1);
        let bg_fn = match opt_value(&bg_fn) {
            Some(value) => store_function_ref(&ctx, value)?.map(style_fn_from_ref),
            None => None,
        };
        Ok(JsBox {
            inner: RefCell::new(BoxComponent::new(padding_x, padding_y, bg_fn)),
        })
    }

    /// `addChild(component)`。
    #[qjs(rename = "addChild")]
    fn add_child<'js>(&self, ctx: Ctx<'js>, component: Value<'js>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let child = JsBox::wrap_child(&ctx, component)?;
        self.inner.borrow_mut().add_child(child);
        Ok(())
    }

    /// `removeChild(component)`。
    ///
    /// TS 的 `Container.removeChild` 按**对象引用**查找并 `splice`（`tui.ts:370-377`）。
    /// 这里逐个比较子组件引用表里的 JS 对象：同一对象 → 同一引用；移除后 `detach()`
    /// （**不**调用 `dispose()`，组件对象仍可由扩展重新挂载），最后释放引用表项。
    #[qjs(rename = "removeChild")]
    fn remove_child<'js>(&self, ctx: Ctx<'js>, component: Value<'js>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(object) = component.as_object().cloned() else {
            return Ok(());
        };
        let references: Vec<u64> = self
            .inner
            .borrow()
            .children
            .iter()
            .filter_map(|child| {
                child
                    .as_any()
                    .and_then(|any| any.downcast_ref::<JsComponent>())
                    .map(|component| component.reference())
            })
            .collect();
        let mut matched = None;
        for reference in references {
            if reference == 0 {
                continue;
            }
            if load_ref(&ctx, reference)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .is_some_and(|candidate| candidate == object)
            {
                matched = Some(reference);
                break;
            }
        }
        let Some(reference) = matched else {
            return Ok(());
        };
        let mut inner = self.inner.borrow_mut();
        let index = inner.children.iter().position(|child| {
            child
                .as_any()
                .and_then(|any| any.downcast_ref::<JsComponent>())
                .is_some_and(|component| component.reference() == reference)
        });
        if let Some(index) = index {
            let mut removed = inner.children.remove(index);
            if let Some(component) = removed
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<JsComponent>())
            {
                component.detach();
            }
            drop(removed);
        }
        drop(inner);
        drop_ref(&ctx, reference);
        Ok(())
    }

    /// `clear()`。
    #[qjs(rename = "clear")]
    fn clear(&self) {
        self.inner.borrow_mut().clear();
    }

    /// `setBgFn(fn?)`。
    #[qjs(rename = "setBgFn")]
    fn set_bg_fn<'js>(&self, ctx: Ctx<'js>, bg_fn: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let function = match opt_value(&bg_fn) {
            Some(value) => store_function_ref(&ctx, value)?.map(style_fn_from_ref),
            None => None,
        };
        self.inner.borrow_mut().set_bg_fn(function);
        Ok(())
    }

    /// `setPaddingX(paddingX)`。
    #[qjs(rename = "setPaddingX")]
    fn set_padding_x<'js>(&self, ctx: Ctx<'js>, padding_x: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let padding = opt_arg_number(&ctx, &padding_x)
            .map(non_negative_floor)
            .unwrap_or(0);
        self.inner.borrow_mut().set_padding_x(padding);
        Ok(())
    }

    /// `invalidate()`：Box 会把失效传给子组件（JS 组件会调用 JS `invalidate`，
    /// 必须复用当前事务）。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().invalidate();
    }

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let lines = Component::render(&mut *self.inner.borrow_mut(), width);
        lines_to_js(&ctx, lines)
    }

    /// `handleMouse(event)`。
    #[qjs(rename = "handleMouse")]
    fn handle_mouse<'js>(&self, ctx: Ctx<'js>, event: Value<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let event = mouse_event_from_js(&ctx, event)?;
        let result = Component::handle_mouse(&mut *self.inner.borrow_mut(), &event);
        mouse_result_to_js(&ctx, result)
    }
}

// ===========================================================================
// Spacer
// ===========================================================================

/// `@earendil-works/kiana-tui` 的 `Spacer`。
///
/// `lines` 原样保存（缺省 1），渲染时按 `max(0, ceil(lines))` 输出空行；非有限值渲染为 0 行。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "Spacer")]
pub struct JsSpacer {
    #[qjs(skip_trace)]
    inner: RefCell<Spacer>,
}

#[rquickjs::methods]
impl JsSpacer {
    /// `new Spacer(lines = 1)`。
    #[qjs(constructor)]
    fn new<'js>(ctx: Ctx<'js>, lines: Opt<Value<'js>>) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let lines = opt_arg_number(&ctx, &lines).unwrap_or(1.0);
        Ok(JsSpacer {
            inner: RefCell::new(Spacer::with_lines(lines)),
        })
    }

    /// `setLines(lines)`。
    #[qjs(rename = "setLines")]
    fn set_lines<'js>(&self, ctx: Ctx<'js>, lines: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let lines = opt_arg_number(&ctx, &lines).unwrap_or(1.0);
        self.inner.borrow_mut().set_lines(lines);
        Ok(())
    }

    /// `invalidate()`。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self) {
        self.inner.borrow_mut().invalidate();
    }

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let lines = Component::render(&mut *self.inner.borrow_mut(), width);
        lines_to_js(&ctx, lines)
    }
}

// ===========================================================================
// TruncatedText
// ===========================================================================

/// `@earendil-works/kiana-tui` 的 `TruncatedText`。
///
/// `render(width)` 固定输出 `paddingY` 个整宽空行 + 1 行内容 + `paddingY` 个空行；内容只取
/// 文本第一个 `\n` 之前的部分，按去掉左右 padding 后的列宽截断（省略号 `...`）。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "TruncatedText")]
pub struct JsTruncatedText {
    #[qjs(skip_trace)]
    inner: RefCell<TruncatedText>,
}

#[rquickjs::methods]
impl JsTruncatedText {
    /// `new TruncatedText(text, paddingX = 0, paddingY = 0)`。
    #[qjs(constructor)]
    fn new<'js>(
        ctx: Ctx<'js>,
        text: Opt<Value<'js>>,
        padding_x: Opt<Value<'js>>,
        padding_y: Opt<Value<'js>>,
    ) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let text = opt_arg_string(&text).unwrap_or_default();
        let padding_x = opt_arg_number(&ctx, &padding_x)
            .map(non_negative_floor)
            .unwrap_or(0);
        let padding_y = opt_arg_number(&ctx, &padding_y)
            .map(non_negative_floor)
            .unwrap_or(0);
        Ok(JsTruncatedText {
            inner: RefCell::new(TruncatedText::with_padding(text, padding_x, padding_y)),
        })
    }

    /// `invalidate()`。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self) {
        self.inner.borrow_mut().invalidate();
    }

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let lines = Component::render(&mut *self.inner.borrow_mut(), width);
        lines_to_js(&ctx, lines)
    }
}

// ===========================================================================
// DynamicBorder（coding-agent 侧的组合组件）
// ===========================================================================

/// 边框颜色函数（`DynamicBorder`/`Editor` 的 `borderColor`）。
type BorderColorFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// 随视口宽度变化的分隔线（TS `DynamicBorder`）。
///
/// `render(width)` 恒为一行 `─`（宽度为 0 时也输出 1 个字符）。颜色函数在**渲染时**调用；
/// 构造时没有传函数则使用当前全局主题的 `border` token（见 `default_border_color`）。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "DynamicBorder")]
pub struct JsDynamicBorder {
    #[qjs(skip_trace)]
    color: RefCell<BorderColorFn>,
}

/// `DynamicBorder` 的默认颜色：当前主题的 `border` token。
///
/// TS 的默认参数是 `(str) => theme.fg("border", str)`，其中 `theme` 是模块级 `Proxy`
/// （`dynamic-border.ts:10-14`）：颜色在**渲染时**读取当前主题，`setTheme` 之后立即生效。
/// 因此这里每次都调用 [`current_global_theme`]，不能在构造期冻结。
fn default_border_color() -> BorderColorFn {
    Arc::new(|text: &str| current_global_theme().fg(ThemeColor::Border, text))
}

#[rquickjs::methods]
impl JsDynamicBorder {
    /// `new DynamicBorder(color = theme.fg("border"))`。
    #[qjs(constructor)]
    fn new<'js>(ctx: Ctx<'js>, color: Opt<Value<'js>>) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let color = match opt_value(&color) {
            Some(value) => match store_function_ref(&ctx, value)? {
                Some(reference) => send_style_fn_from_ref(reference),
                None => default_border_color(),
            },
            None => default_border_color(),
        };
        Ok(JsDynamicBorder {
            color: RefCell::new(color),
        })
    }

    /// `invalidate()`：无缓存可失效（`render` 每次都按传入宽度重新生成边框并重新着色）。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self) {}

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let border = "─".repeat(width.max(1));
        let line = (self.color.borrow())(&border);
        lines_to_js(&ctx, vec![line])
    }
}

// ===========================================================================
// Loader
// ===========================================================================

/// 从 JS TUI 对象构造 `requestRender` 回调。
///
/// 对象被存进 `__kiana_refs`；本函数没有对应的释放点，引用表项会保留到引擎销毁（回调
/// 每次触发都用 `load_ref` 取回对象，取不到就静默跳过）。
fn request_render_from_js<'js>(ctx: &Ctx<'js>, tui: Option<&Object<'js>>) -> Option<Rc<dyn Fn()>> {
    let tui = tui?.clone();
    let engine = JsEngineSlot::current()?;
    let reference = store_ref(ctx, tui.into_value()).ok()?;
    Some(Rc::new(move || {
        let engine = Rc::clone(&engine);
        let _ = with_engine_js(&engine, |ctx| -> EngineResult<()> {
            if let Ok(value) = load_ref(&ctx, reference)
                && let Some(object) = value.as_object()
            {
                let _ = call_object_method(&ctx, object, "requestRender", &[]);
            }
            Ok(())
        });
    }))
}

/// TS `setInterval` 的等价物：基于 tokio 计时器的 [`kiana_tui::tui::Timer`]。
///
/// kiana-tui 的组件不持有异步运行时，动画（`Loader.restartAnimation`）与
/// 自动补全防抖（`Editor` 的 `setTimeout`）都由宿主注入 [`kiana_tui::tui::Timer`] 驱动
/// （`spec/tui-components.md` §8.2）。JS 侧构造的组件在这里自动装上一个 tokio 计时器，
/// 复刻 TS "构造即开始动画" 的行为；没有 tokio 运行时的线程（纯同步测试）上静默不排程。
struct JsIntervalTimer {
    /// 取消代次：`cancel` 后已排程的回调被丢弃（TS 的 `clearInterval`）。
    generation: Rc<std::cell::Cell<u64>>,
}

impl JsIntervalTimer {
    fn new() -> Self {
        JsIntervalTimer {
            generation: Rc::new(std::cell::Cell::new(0)),
        }
    }
}

impl kiana_tui::tui::Timer for JsIntervalTimer {
    fn set(&mut self, delay_ms: u64, callback: Box<dyn FnOnce()>) {
        // 代次前进：之前排程但尚未执行的回调在醒来后发现代次不同，自行丢弃（clearInterval）。
        let generation = Rc::clone(&self.generation);
        let current = generation.get().wrapping_add(1);
        generation.set(current);
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        tokio::task::spawn_local(async move {
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            if generation.get() != current {
                return;
            }
            callback();
        });
    }

    fn cancel(&mut self) {
        self.generation.set(self.generation.get().wrapping_add(1));
    }
}

/// 给 Loader 装上自动动画计时器（TS 构造期 `start()` 的 `setInterval`）。
fn install_loader_timer(loader: &mut Loader) {
    loader.set_timer(Box::new(JsIntervalTimer::new()));
}

/// 从 JS `requestRender` 函数值构造回调（`ScrollView.updateLayout` 的第三个参数）。
///
/// 同时返回引用 id：调用方在替换回调时释放旧引用（否则每帧 `updateLayout` 都会往
/// `__kiana_refs` 里堆一个函数）。
fn request_render_fn_from_js<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
) -> Option<(Rc<dyn Fn()>, u64)> {
    let function = value.as_function()?.clone();
    let reference = store_ref(ctx, function.into_value()).ok()?;
    let engine = JsEngineSlot::current()?;
    let callback = Rc::new(move || {
        let engine = Rc::clone(&engine);
        let _ = with_engine_js(&engine, |ctx| -> EngineResult<()> {
            let _ = call_ref::<Value>(&ctx, reference, None, &[]);
            Ok(())
        });
    });
    Some((callback, reference))
}

/// 读取 `LoaderIndicatorOptions`（`{ frames, intervalMs }`）。
///
/// `frames` 存在且非 `null` 时必须是数组（否则抛 JS 异常）；`intervalMs` 走 `opt_number`
/// （JS `Number()` 语义，转换失败按缺失处理）。整个对象缺失时返回 `None`（调用方用默认
/// 指示器）。
fn loader_indicator_from_object<'js>(
    ctx: &Ctx<'js>,
    value: Option<&Object<'js>>,
) -> EngineResult<Option<LoaderIndicatorOptions>> {
    let Some(object) = value else {
        return Ok(None);
    };
    let mut indicator = LoaderIndicatorOptions::default();
    let frames: Option<Value> = object.get("frames")?;
    if let Some(frames) = frames
        && !frames.is_undefined()
        && !frames.is_null()
    {
        indicator.frames = Some(array_strings(ctx, frames)?);
    }
    indicator.interval_ms = opt_number(object, "intervalMs")?;
    Ok(Some(indicator))
}

/// `@earendil-works/kiana-tui` 的 `Loader`。
///
/// 构造时从第一个参数（`TUIProxy` 或带 `requestRender` 的对象）取重绘回调，并安装
/// `JsIntervalTimer` 驱动动画——对应 TS 构造函数里启动的 `setInterval`。计时器在没有
/// tokio 运行时的线程上不排程。`setText` 与 `setMessage` 同义（`Loader extends Text`）。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "Loader")]
pub struct JsLoader {
    #[qjs(skip_trace)]
    inner: RefCell<Loader>,
}

#[rquickjs::methods]
impl JsLoader {
    /// `new Loader(tui, spinnerColorFn, messageColorFn, message = "Loading...", indicator?)`。
    #[qjs(constructor)]
    fn new<'js>(
        ctx: Ctx<'js>,
        tui: Opt<Value<'js>>,
        spinner_color: Opt<Value<'js>>,
        message_color: Opt<Value<'js>>,
        message: Opt<Value<'js>>,
        indicator: Opt<Value<'js>>,
    ) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let tui = opt_value(&tui);
        let tui_object = tui.as_ref().and_then(|value| value.as_object()).cloned();
        let request_render = request_render_from_js(&ctx, tui_object.as_ref());
        let spinner_color = match opt_value(&spinner_color) {
            Some(value) => store_function_ref(&ctx, value)?
                .map(style_fn_from_ref)
                .unwrap_or_else(|| Arc::new(|text: &str| text.to_string())),
            None => Arc::new(|text: &str| text.to_string()),
        };
        let message_color = match opt_value(&message_color) {
            Some(value) => store_function_ref(&ctx, value)?
                .map(style_fn_from_ref)
                .unwrap_or_else(|| Arc::new(|text: &str| text.to_string())),
            None => Arc::new(|text: &str| text.to_string()),
        };
        let message = opt_arg_string(&message).unwrap_or_else(|| "Loading...".to_owned());
        let indicator_value = opt_value(&indicator);
        let indicator_object = indicator_value
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        let indicator = loader_indicator_from_object(&ctx, indicator_object.as_ref())?;
        let mut loader = Loader::new(
            request_render,
            spinner_color,
            message_color,
            &message,
            indicator,
        );
        install_loader_timer(&mut loader);
        Ok(JsLoader {
            inner: RefCell::new(loader),
        })
    }

    /// `start()`。
    #[qjs(rename = "start")]
    fn start(&self, ctx: Ctx<'_>) {
        // `Loader::start` 会 `requestRender`（JS 回调），必须复用当前事务（§2.2）。
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().start();
    }

    /// `stop()`。
    #[qjs(rename = "stop")]
    fn stop(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().stop();
    }

    /// `setMessage(message)`。
    #[qjs(rename = "setMessage")]
    fn set_message(&self, ctx: Ctx<'_>, message: String) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().set_message(&message);
    }

    /// `setIndicator(indicator?)`。
    #[qjs(rename = "setIndicator")]
    fn set_indicator<'js>(&self, ctx: Ctx<'js>, indicator: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let value = opt_value(&indicator);
        let object = value.as_ref().and_then(|value| value.as_object()).cloned();
        let indicator = loader_indicator_from_object(&ctx, object.as_ref())?;
        self.inner.borrow_mut().set_indicator(indicator);
        Ok(())
    }

    /// `setText(text)`（`Loader extends Text`）：改写消息文本（与 `setMessage` 同义）。
    #[qjs(rename = "setText")]
    fn set_text(&self, ctx: Ctx<'_>, text: String) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().set_message(&text);
    }

    /// `setPaddingX(paddingX)`：`Loader` 继承 `Text`，留白会改变渲染输出
    /// （`loader.ts:16`、`text.ts:39-40`）。
    #[qjs(rename = "setPaddingX")]
    fn set_padding_x<'js>(&self, ctx: Ctx<'js>, padding_x: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let padding = opt_arg_number(&ctx, &padding_x)
            .map(non_negative_floor)
            .unwrap_or(0);
        self.inner.borrow_mut().set_padding_x(padding);
        Ok(())
    }

    /// `invalidate()`。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        Component::invalidate(&mut *self.inner.borrow_mut());
    }

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let lines = Component::render(&mut *self.inner.borrow_mut(), width);
        lines_to_js(&ctx, lines)
    }
}

// ===========================================================================
// Input
// ===========================================================================

/// `@earendil-works/kiana-tui` 的 `Input`。
///
/// `onSubmit`/`onEscape` 是原型访问器（§5.3）：同名的 `get`/`set` 方法由宏收进一个属性。
/// kiana-tui 的回调在 `handle_input` 内部触发；此时 `Input` 处于可变借用，直接调用 JS 会让
/// `getValue()` 之类的重入 panic，因此回调只入队，方法返回后再派发。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "Input")]
pub struct JsInput {
    #[qjs(skip_trace)]
    inner: RefCell<Input>,
    #[qjs(skip_trace)]
    submit_ref: RefCell<Option<u64>>,
    #[qjs(skip_trace)]
    escape_ref: RefCell<Option<u64>>,
    #[qjs(skip_trace)]
    pending_submit: Rc<RefCell<Option<String>>>,
    #[qjs(skip_trace)]
    pending_escape: Rc<std::cell::Cell<bool>>,
}

impl JsInput {
    /// 派发队列里的回调（在 `RefCell<Input>` 借用释放之后调用）。
    ///
    /// 接收者是**本次调用**的 `this`（TS `this.onSubmit?.(this.value)`）：直接在
    /// `handleInput`/`handleMouse` 的 `This` 里传递，不需要为组件保存自引用（否则引用表
    /// 会让原生实例永远无法被 GC 回收）。
    fn drain_callbacks<'js>(&self, ctx: &Ctx<'js>, receiver: Option<Value<'js>>) {
        if self.pending_escape.take() {
            let reference = *self.escape_ref.borrow();
            if let Some(reference) = reference {
                let _ = call_ref::<Value>(ctx, reference, receiver.clone(), &[]);
            }
        }
        if let Some(text) = self.pending_submit.borrow_mut().take() {
            let reference = *self.submit_ref.borrow();
            if let Some(reference) = reference {
                let _ = call_ref::<Value>(ctx, reference, receiver, &[JsArg::String(text)]);
            }
        }
    }
}

#[rquickjs::methods]
impl JsInput {
    /// `new Input(options = {})`。
    #[qjs(constructor)]
    fn new<'js>(ctx: Ctx<'js>, options: Opt<Value<'js>>) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let options_object = opt_value(&options)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        let mut input_options = InputOptions::default();
        if let Some(object) = options_object.as_ref() {
            input_options.prompt = opt_string(object, "prompt")?;
            input_options.placeholder = opt_string(object, "placeholder")?;
            if let Some(reference) = object_fn_ref(&ctx, object, "placeholderStyle")? {
                input_options.placeholder_style = Some(style_fn_from_ref(reference));
            }
        }
        let pending_submit: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let pending_escape: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(false));
        let mut input = Input::new(input_options);
        let submit_slot = Rc::clone(&pending_submit);
        input.on_submit = Some(Box::new(move |text: &str| {
            *submit_slot.borrow_mut() = Some(text.to_owned());
        }));
        let escape_slot = Rc::clone(&pending_escape);
        input.on_escape = Some(Box::new(move || {
            escape_slot.set(true);
        }));
        Ok(JsInput {
            inner: RefCell::new(input),
            submit_ref: RefCell::new(None),
            escape_ref: RefCell::new(None),
            pending_submit,
            pending_escape,
        })
    }

    /// `getValue()`。
    #[qjs(rename = "getValue")]
    fn get_value(&self) -> String {
        self.inner.borrow().get_value().to_owned()
    }

    /// `setValue(value)`。
    #[qjs(rename = "setValue")]
    fn set_value(&self, value: String) {
        self.inner.borrow_mut().set_value(value);
    }

    /// `handleInput(data)`。
    #[qjs(rename = "handleInput")]
    fn handle_input<'js>(&self, ctx: Ctx<'js>, this: This<Value<'js>>, data: String) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().handle_input(&data);
        self.drain_callbacks(&ctx, Some(this.0));
    }

    /// `handleMouse(event)`。
    #[qjs(rename = "handleMouse")]
    fn handle_mouse<'js>(
        &self,
        ctx: Ctx<'js>,
        this: This<Value<'js>>,
        event: Value<'js>,
    ) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let event = mouse_event_from_js(&ctx, event)?;
        let result = Component::handle_mouse(&mut *self.inner.borrow_mut(), &event);
        self.drain_callbacks(&ctx, Some(this.0));
        mouse_result_to_js(&ctx, result)
    }

    /// `invalidate()`。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self) {
        self.inner.borrow_mut().invalidate();
    }

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let lines = Component::render(&mut *self.inner.borrow_mut(), width);
        lines_to_js(&ctx, lines)
    }

    /// `focused`（TS 的公开可写字段；TUI 通过它设置焦点）。
    #[qjs(get, rename = "focused")]
    fn get_focused(&self) -> bool {
        Focusable::focused(&*self.inner.borrow())
    }

    /// `focused = value`。
    #[qjs(set, rename = "focused")]
    fn set_focused(&self, focused: bool) {
        Focusable::set_focused(&mut *self.inner.borrow_mut(), focused);
    }

    /// `onSubmit` 回调（未设置时 `undefined`）。
    #[qjs(get, rename = "onSubmit")]
    fn get_on_submit<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match *self.submit_ref.borrow() {
            Some(reference) => load_ref(&ctx, reference),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `onSubmit = fn`。
    #[qjs(set, rename = "onSubmit")]
    fn set_on_submit<'js>(&self, ctx: Ctx<'js>, value: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let reference = match opt_value(&value) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        *self.submit_ref.borrow_mut() = reference;
        Ok(())
    }

    /// `onEscape` 回调。
    #[qjs(get, rename = "onEscape")]
    fn get_on_escape<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match *self.escape_ref.borrow() {
            Some(reference) => load_ref(&ctx, reference),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `onEscape = fn`。
    #[qjs(set, rename = "onEscape")]
    fn set_on_escape<'js>(&self, ctx: Ctx<'js>, value: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let reference = match opt_value(&value) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        *self.escape_ref.borrow_mut() = reference;
        Ok(())
    }
}

// ===========================================================================
// SelectList
// ===========================================================================

/// 从 JS `SelectItem` 对象解析条目（缺 `label` 时用 `value`）。
fn select_item_from_object<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> EngineResult<SelectItem> {
    let Some(object) = value.as_object() else {
        return Err(Exception::throw_message(
            ctx,
            "select item is not an object",
        ));
    };
    let value_text = opt_string(object, "value")?.unwrap_or_default();
    let label = opt_string(object, "label")?.unwrap_or_else(|| value_text.clone());
    match opt_string(object, "description")? {
        Some(description) => {
            Ok(SelectItem::with_label(value_text, label).with_description(description))
        }
        None => Ok(SelectItem::with_label(value_text, label)),
    }
}

/// 条目 → JS 对象。`description` 缺失时显式写入 `undefined`（而不是省略属性），
/// 与 TS 读取可选字段的结果一致；只在 [`select_item_value`] 找不到原始 JS 对象时兜底，
/// 此时扩展附加的额外字段会丢失。
fn select_item_to_js<'js>(ctx: &Ctx<'js>, item: &SelectItem) -> EngineResult<Value<'js>> {
    let object = Object::new(ctx.clone())?;
    object.set(
        "value",
        rquickjs::String::from_str(ctx.clone(), &item.value)?,
    )?;
    object.set(
        "label",
        rquickjs::String::from_str(ctx.clone(), &item.label)?,
    )?;
    match item.description.as_ref() {
        Some(description) => {
            object.set(
                "description",
                rquickjs::String::from_str(ctx.clone(), description)?,
            )?;
        }
        None => object.set("description", Value::new_undefined(ctx.clone()))?,
    }
    Ok(object.into_value())
}

/// JS 回调槽：入队，方法返回后派发（避免原生 `RefCell` 可变借用期间重入）。
#[derive(Clone, Default)]
struct SelectCallbacks {
    select: Rc<RefCell<Option<u64>>>,
    cancel: Rc<RefCell<Option<u64>>>,
    change: Rc<RefCell<Option<u64>>>,
    pending_select: Rc<RefCell<Option<SelectItem>>>,
    pending_cancel: Rc<std::cell::Cell<bool>>,
    pending_change: Rc<RefCell<Option<SelectItem>>>,
}

/// `@earendil-works/kiana-tui` 的 `SelectList`。
///
/// 构造时把每个条目对象**原样**存进 `__kiana_refs`（同时解析成 `SelectItem`）：`onSelect`、
/// `onSelectionChange` 与 `getSelectedItem()` 必须返回扩展传入的那个对象（额外字段与身份），
/// 不能重建 `{value,label,description}`。回调在 `handleInput`/`handleMouse` 的 `RefCell`
/// 借用释放后派发（见 `drain_callbacks`）；`Drop` 释放条目与三个回调的引用。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "SelectList")]
pub struct JsSelectList {
    #[qjs(skip_trace)]
    inner: RefCell<SelectList>,
    #[qjs(skip_trace)]
    callbacks: SelectCallbacks,
    /// 扩展传入的原始条目（`__kiana_refs` 引用 id + 字段）。回调与 `getSelectedItem()` 必须
    /// 返回**同一个对象**（`select-list.ts:130-139、246-272`），不能重建
    /// `{value,label,description}`。
    #[qjs(skip_trace)]
    items: SharedSelectItems,
}

// 组件被 JS GC 回收时释放它持有的全部引用：原始条目对象与三个回调。这里不调用 JS
// `dispose()`（TS 的 SelectList 没有处置协议，容器移除成员也只做 splice）。
impl Drop for JsSelectList {
    fn drop(&mut self) {
        let references: Vec<u64> = self
            .items
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain(..)
            .map(|(reference, _)| reference)
            .collect();
        for reference in references {
            adapter::release_ref(reference);
        }
        for slot in [
            &self.callbacks.select,
            &self.callbacks.cancel,
            &self.callbacks.change,
        ] {
            if let Some(reference) = slot.borrow_mut().take() {
                adapter::release_ref(reference);
            }
        }
    }
}

impl JsSelectList {
    /// 派发队列里的回调（接收者是本次调用的 `this`，`this.onSelect(item)`）。
    fn drain_callbacks<'js>(&self, ctx: &Ctx<'js>, receiver: Option<Value<'js>>) {
        if self.callbacks.pending_cancel.replace(false) {
            let reference = *self.callbacks.cancel.borrow();
            if let Some(reference) = reference {
                let _ = call_ref::<Value>(ctx, reference, receiver.clone(), &[]);
            }
        }
        if let Some(item) = self.callbacks.pending_change.borrow_mut().take() {
            let reference = *self.callbacks.change.borrow();
            if let Some(reference) = reference {
                // TS 传的是扩展提供的原始条目对象（含额外字段与对象身份）。
                if let Ok(value) = select_item_value(ctx, &self.items, &item) {
                    let _ = adapter::call_ref_with_values(
                        ctx,
                        reference,
                        receiver.clone(),
                        vec![value],
                    );
                }
            }
        }
        if let Some(item) = self.callbacks.pending_select.borrow_mut().take() {
            let reference = *self.callbacks.select.borrow();
            if let Some(reference) = reference
                && let Ok(value) = select_item_value(ctx, &self.items, &item)
            {
                let _ = adapter::call_ref_with_values(ctx, reference, receiver, vec![value]);
            }
        }
    }
}

#[rquickjs::methods]
impl JsSelectList {
    /// `new SelectList(items, maxVisible, theme, layout = {})`。
    #[qjs(constructor)]
    fn new<'js>(
        ctx: Ctx<'js>,
        items: Opt<Value<'js>>,
        max_visible: Opt<Value<'js>>,
        theme: Opt<Value<'js>>,
        layout: Opt<Value<'js>>,
    ) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let mut parsed_items = Vec::new();
        let mut original_items: Vec<(u64, SelectItem)> = Vec::new();
        if let Some(items) = opt_value(&items) {
            let Some(array) = items.as_array() else {
                return Err(Exception::throw_message(&ctx, "items must be an array"));
            };
            for index in 0..array.len() {
                let value: Value = array.get(index)?;
                let item = select_item_from_object(&ctx, value.clone())?;
                // 保留原始对象：回调与 getSelectedItem 必须返回同一个对象。
                let reference = store_ref(&ctx, value)?;
                original_items.push((reference, item.clone()));
                parsed_items.push(item);
            }
        }
        let items = Arc::new(std::sync::Mutex::new(original_items));
        let max_visible = opt_arg_number(&ctx, &max_visible)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let theme_object = opt_value(&theme)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        let theme = select_list_theme_from_object(&ctx, theme_object.as_ref())?;
        let mut layout_options = SelectListLayoutOptions::default();
        if let Some(layout_object) = opt_value(&layout)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned()
        {
            layout_options.min_primary_column_width =
                opt_number(&layout_object, "minPrimaryColumnWidth")?;
            layout_options.max_primary_column_width =
                opt_number(&layout_object, "maxPrimaryColumnWidth")?;
            if let Some(reference) = object_fn_ref(&ctx, &layout_object, "truncatePrimary")? {
                // truncatePrimary 收到的 `context.item` 要能映射回原始 JS 对象，因此与列表
                // 共享同一张条目表（Arc<Mutex<Vec<…>>>）。
                layout_options.truncate_primary =
                    Some(truncate_primary_from_ref(reference, Arc::clone(&items)));
            }
        }
        let callbacks = SelectCallbacks::default();
        let mut list = SelectList::new(parsed_items, max_visible, theme, layout_options);
        let select_slot = Rc::clone(&callbacks.pending_select);
        list.on_select = Some(Box::new(move |item: &SelectItem| {
            *select_slot.borrow_mut() = Some(item.clone());
        }));
        let cancel_slot = Rc::clone(&callbacks.pending_cancel);
        list.on_cancel = Some(Box::new(move || cancel_slot.set(true)));
        let change_slot = Rc::clone(&callbacks.pending_change);
        list.on_selection_change = Some(Box::new(move |item: &SelectItem| {
            *change_slot.borrow_mut() = Some(item.clone());
        }));
        Ok(JsSelectList {
            inner: RefCell::new(list),
            callbacks,
            items,
        })
    }

    /// `setFilter(filter)`。
    #[qjs(rename = "setFilter")]
    fn set_filter(&self, filter: String) {
        self.inner.borrow_mut().set_filter(&filter);
    }

    /// `setSelectedIndex(index)`。
    #[qjs(rename = "setSelectedIndex")]
    fn set_selected_index<'js>(&self, ctx: Ctx<'js>, index: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let index = opt_arg_number(&ctx, &index).unwrap_or(0.0);
        let index = if index.is_finite() {
            index.trunc() as isize
        } else {
            0
        };
        self.inner.borrow_mut().set_selected_index(index);
        Ok(())
    }

    /// `getSelectedItem()`。
    #[qjs(rename = "getSelectedItem")]
    fn get_selected_item<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let item = self.inner.borrow().get_selected_item().cloned();
        match item {
            Some(item) => select_item_value(&ctx, &self.items, &item),
            None => Ok(Value::new_null(ctx.clone())),
        }
    }

    /// `invalidate()`。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self) {
        self.inner.borrow_mut().invalidate();
    }

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let lines = Component::render(&mut *self.inner.borrow_mut(), width);
        lines_to_js(&ctx, lines)
    }

    /// `handleMouse(event)`。
    #[qjs(rename = "handleMouse")]
    fn handle_mouse<'js>(
        &self,
        ctx: Ctx<'js>,
        this: This<Value<'js>>,
        event: Value<'js>,
    ) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let event = mouse_event_from_js(&ctx, event)?;
        let result = {
            let mut inner = self.inner.borrow_mut();
            Component::handle_mouse(&mut *inner, &event)
        };
        self.drain_callbacks(&ctx, Some(this.0));
        mouse_result_to_js(&ctx, result)
    }

    /// `handleInput(data)`。
    #[qjs(rename = "handleInput")]
    fn handle_input<'js>(&self, ctx: Ctx<'js>, this: This<Value<'js>>, data: String) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().handle_input(&data);
        self.drain_callbacks(&ctx, Some(this.0));
    }

    /// `onSelect` 回调。
    #[qjs(get, rename = "onSelect")]
    fn get_on_select<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match *self.callbacks.select.borrow() {
            Some(reference) => load_ref(&ctx, reference),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `onSelect = fn`。
    #[qjs(set, rename = "onSelect")]
    fn set_on_select<'js>(&self, ctx: Ctx<'js>, value: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        replace_callback_ref(&ctx, &self.callbacks.select, value)
    }

    /// `onCancel` 回调。
    #[qjs(get, rename = "onCancel")]
    fn get_on_cancel<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match *self.callbacks.cancel.borrow() {
            Some(reference) => load_ref(&ctx, reference),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `onCancel = fn`。
    #[qjs(set, rename = "onCancel")]
    fn set_on_cancel<'js>(&self, ctx: Ctx<'js>, value: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        replace_callback_ref(&ctx, &self.callbacks.cancel, value)
    }

    /// `onSelectionChange` 回调。
    #[qjs(get, rename = "onSelectionChange")]
    fn get_on_selection_change<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match *self.callbacks.change.borrow() {
            Some(reference) => load_ref(&ctx, reference),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `onSelectionChange = fn`。
    #[qjs(set, rename = "onSelectionChange")]
    fn set_on_selection_change<'js>(
        &self,
        ctx: Ctx<'js>,
        value: Opt<Value<'js>>,
    ) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        replace_callback_ref(&ctx, &self.callbacks.change, value)
    }
}

// ===========================================================================
// SettingsList
// ===========================================================================

/// 解析 JS `SettingItem`。
fn setting_item_from_object<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> EngineResult<SettingItem> {
    let Some(object) = value.as_object() else {
        return Err(Exception::throw_message(
            ctx,
            "setting item is not an object",
        ));
    };
    let id = opt_string(object, "id")?.unwrap_or_default();
    let label = opt_string(object, "label")?.unwrap_or_default();
    let current_value = opt_string(object, "currentValue")?.unwrap_or_default();
    let mut item = SettingItem::new(id, label, current_value);
    item.description = opt_string(object, "description")?;
    let values: Option<Value> = object.get("values")?;
    if let Some(values) = values
        && !values.is_undefined()
        && !values.is_null()
    {
        item.values = Some(array_strings(ctx, values)?);
    }
    let submenu: Option<Value> = object.get("submenu")?;
    if let Some(submenu) = submenu
        && submenu.as_function().is_some()
    {
        let reference = store_ref(ctx, submenu)?;
        item.submenu = Some(submenu_factory_from_ref(reference));
    }
    Ok(item)
}

/// JS 子菜单工厂 → [`SubmenuFactory`]。
///
/// 工厂返回的组件对象被包成 [`JsComponent`]；返回 Promise 时（TS 允许异步子菜单）暂不支持，
/// 退化为空组件（见报告剩余缺口）。
fn submenu_factory_from_ref(reference: u64) -> SubmenuFactory {
    Rc::new(
        move |current: &str, done: SettingsDoneCallback| -> Box<dyn Component> {
            // 任何一步失败（引擎缺失/工厂抛错/返回非对象/返回 Promise）都退化为空 Spacer，
            // 调用方总能拿到一个可渲染组件。
            let Some(engine) = JsEngineSlot::current() else {
                return Box::new(Spacer::with_lines(0.0));
            };
            let done = Rc::new(RefCell::new(done));
            let current = current.to_owned();
            let outcome: Option<Box<dyn Component>> = with_engine_js(&engine, |ctx| {
                let done_slot = Rc::clone(&done);
                let done_function = Function::new(
                    ctx.clone(),
                    move |_ctx: Ctx<'_>,
                          value: Opt<Value<'_>>,
                          options: Opt<Value<'_>>|
                          -> EngineResult<()> {
                        let selected = opt_value(&value).and_then(|value| js_string(&value).ok());
                        let mut navigate = SettingsNavigateOptions::default();
                        if let Some(options) = opt_value(&options)
                            && let Some(object) = options.as_object()
                        {
                            navigate.navigate_to = opt_string(object, "navigateTo")?;
                        }
                        let mut callback = done_slot.borrow_mut();
                        callback(selected, Some(navigate));
                        Ok(())
                    },
                )
                .ok()?;
                let done_reference = store_ref(&ctx, done_function.into_value()).ok()?;
                let value: Option<Value> = call_ref(
                    &ctx,
                    reference,
                    None,
                    &[JsArg::String(current.clone()), JsArg::Ref(done_reference)],
                )
                .ok();
                let Some(value) = value else {
                    adapter::release_ref(done_reference);
                    return None;
                };
                if value.is_undefined() || value.is_null() {
                    adapter::release_ref(done_reference);
                    return None;
                }
                let Some(object) = value.as_object() else {
                    adapter::release_ref(done_reference);
                    return None;
                };
                let Some(component_reference) = store_ref(&ctx, object.clone().into_value()).ok()
                else {
                    adapter::release_ref(done_reference);
                    return None;
                };
                // 子菜单组件的生命周期结束时释放本次工厂调用创建的 `done` 函数引用
                // （TS 里闭包随组件一起被 GC）。
                let mut component = JsComponent::from_ref(&engine, component_reference);
                component.attach_ref(done_reference);
                Some(Box::new(component) as Box<dyn Component>)
            });
            outcome.unwrap_or_else(|| Box::new(Spacer::with_lines(0.0)))
        },
    )
}

/// 设置列表的回调槽（入队后派发）。
#[derive(Clone, Default)]
struct SettingsCallbacks {
    change: Rc<RefCell<Vec<(String, String)>>>,
    cancel: Rc<std::cell::Cell<bool>>,
}

/// `@earendil-works/kiana-tui` 的 `SettingsList`。
///
/// `onChange` 的 `(id, value)` 对先入队（`Vec` 保留产生顺序），方法返回后由
/// `drain_callbacks` 依次派发；`onCancel` 只用布尔标记。`submenu` 工厂返回的组件会把本次
/// 工厂调用创建的 `done` 函数引用绑定到组件生命周期上（`JsComponent::attach_ref`），组件
/// 被 Drop 时一并释放。工厂返回 Promise（TS 允许异步子菜单）时退化为空组件。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "SettingsList")]
pub struct JsSettingsList {
    #[qjs(skip_trace)]
    inner: RefCell<SettingsList>,
    #[qjs(skip_trace)]
    change_ref: RefCell<Option<u64>>,
    #[qjs(skip_trace)]
    cancel_ref: RefCell<Option<u64>>,
    #[qjs(skip_trace)]
    callbacks: SettingsCallbacks,
}

impl JsSettingsList {
    /// 派发队列里的回调（接收者是本次调用的 `this`）。
    fn drain_callbacks<'js>(&self, ctx: &Ctx<'js>, receiver: Option<Value<'js>>) {
        if self.callbacks.cancel.replace(false) {
            let reference = *self.cancel_ref.borrow();
            if let Some(reference) = reference {
                let _ = call_ref::<Value>(ctx, reference, receiver.clone(), &[]);
            }
        }
        let pending: Vec<(String, String)> = {
            let mut queue = self.callbacks.change.borrow_mut();
            queue.drain(..).collect()
        };
        for (id, value) in pending {
            let reference = *self.change_ref.borrow();
            if let Some(reference) = reference {
                let _ = call_ref::<Value>(
                    ctx,
                    reference,
                    receiver.clone(),
                    &[JsArg::String(id), JsArg::String(value)],
                );
            }
        }
    }
}

#[rquickjs::methods]
impl JsSettingsList {
    /// `new SettingsList(items, maxVisible, theme, onChange, onCancel, options = {})`。
    #[qjs(constructor)]
    fn new<'js>(
        ctx: Ctx<'js>,
        items: Opt<Value<'js>>,
        max_visible: Opt<Value<'js>>,
        theme: Opt<Value<'js>>,
        on_change: Opt<Value<'js>>,
        on_cancel: Opt<Value<'js>>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let mut parsed_items = Vec::new();
        if let Some(items) = opt_value(&items) {
            let Some(array) = items.as_array() else {
                return Err(Exception::throw_message(&ctx, "items must be an array"));
            };
            for index in 0..array.len() {
                let value: Value = array.get(index)?;
                parsed_items.push(setting_item_from_object(&ctx, value)?);
            }
        }
        let max_visible = opt_arg_number(&ctx, &max_visible)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let theme_object = opt_value(&theme)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        let theme = settings_list_theme_from_object(&ctx, theme_object.as_ref())?;
        let change_ref = match opt_value(&on_change) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        let cancel_ref = match opt_value(&on_cancel) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        let callbacks = SettingsCallbacks::default();
        let change_slot = Rc::clone(&callbacks.change);
        let cancel_slot = Rc::clone(&callbacks.cancel);
        let mut list_options = SettingsListOptions::default();
        if let Some(options_object) = opt_value(&options)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned()
        {
            list_options.enable_search = opt_bool(&options_object, "enableSearch")?;
        }
        let list = SettingsList::new(
            parsed_items,
            max_visible,
            theme,
            Box::new(move |id: &str, value: &str| {
                change_slot
                    .borrow_mut()
                    .push((id.to_owned(), value.to_owned()));
            }),
            Box::new(move || cancel_slot.set(true)),
            list_options,
        );
        Ok(JsSettingsList {
            inner: RefCell::new(list),
            change_ref: RefCell::new(change_ref),
            cancel_ref: RefCell::new(cancel_ref),
            callbacks,
        })
    }

    /// `updateValue(id, newValue)`。
    #[qjs(rename = "updateValue")]
    fn update_value(&self, id: String, new_value: String) {
        self.inner.borrow_mut().update_value(&id, &new_value);
    }

    /// `selectItem(id)`。
    #[qjs(rename = "selectItem")]
    fn select_item(&self, id: String) {
        self.inner.borrow_mut().select_item(&id);
    }

    /// `invalidate()`。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self) {
        self.inner.borrow_mut().invalidate();
    }

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let lines = Component::render(&mut *self.inner.borrow_mut(), width);
        lines_to_js(&ctx, lines)
    }

    /// `handleMouse(event)`。
    #[qjs(rename = "handleMouse")]
    fn handle_mouse<'js>(
        &self,
        ctx: Ctx<'js>,
        this: This<Value<'js>>,
        event: Value<'js>,
    ) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let event = mouse_event_from_js(&ctx, event)?;
        let result = {
            let mut inner = self.inner.borrow_mut();
            Component::handle_mouse(&mut *inner, &event)
        };
        self.drain_callbacks(&ctx, Some(this.0));
        mouse_result_to_js(&ctx, result)
    }

    /// `handleInput(data)`。
    #[qjs(rename = "handleInput")]
    fn handle_input<'js>(&self, ctx: Ctx<'js>, this: This<Value<'js>>, data: String) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().handle_input(&data);
        self.drain_callbacks(&ctx, Some(this.0));
    }

    /// `onChange` 回调。
    #[qjs(get, rename = "onChange")]
    fn get_on_change<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match *self.change_ref.borrow() {
            Some(reference) => load_ref(&ctx, reference),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `onChange = fn`。
    #[qjs(set, rename = "onChange")]
    fn set_on_change<'js>(&self, ctx: Ctx<'js>, value: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let reference = match opt_value(&value) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        *self.change_ref.borrow_mut() = reference;
        Ok(())
    }

    /// `onCancel` 回调。
    #[qjs(get, rename = "onCancel")]
    fn get_on_cancel<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match *self.cancel_ref.borrow() {
            Some(reference) => load_ref(&ctx, reference),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `onCancel = fn`。
    #[qjs(set, rename = "onCancel")]
    fn set_on_cancel<'js>(&self, ctx: Ctx<'js>, value: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let reference = match opt_value(&value) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        *self.cancel_ref.borrow_mut() = reference;
        Ok(())
    }
}

// ===========================================================================
// Markdown
// ===========================================================================

/// `@earendil-works/kiana-tui` 的 `Markdown`。
///
/// 主题对象按 `MarkdownTheme` 的字段名读取（缺省用 `get_markdown_theme()` 的默认值）；
/// `options.renderLatex` 缺省为 `true`，另外两个选项缺省 `false`。`transform` 回调按
/// `(markdown, width)` 在渲染时调用，失败时返回原文。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "Markdown")]
pub struct JsMarkdown {
    #[qjs(skip_trace)]
    inner: RefCell<Markdown>,
}

#[rquickjs::methods]
impl JsMarkdown {
    /// `new Markdown(text, paddingX, paddingY, theme, defaultTextStyle?, options?)`。
    #[qjs(constructor)]
    fn new<'js>(
        ctx: Ctx<'js>,
        text: Opt<Value<'js>>,
        padding_x: Opt<Value<'js>>,
        padding_y: Opt<Value<'js>>,
        theme: Opt<Value<'js>>,
        default_text_style: Opt<Value<'js>>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let text = opt_arg_string(&text).unwrap_or_default();
        let padding_x = opt_arg_number(&ctx, &padding_x)
            .map(non_negative_floor)
            .unwrap_or(0);
        let padding_y = opt_arg_number(&ctx, &padding_y)
            .map(non_negative_floor)
            .unwrap_or(0);
        let theme_object = opt_value(&theme)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        let theme = markdown_theme_from_object(&ctx, theme_object.as_ref())?;
        let mut markdown = Markdown::new(text, padding_x, padding_y, theme);
        if let Some(style_object) = opt_value(&default_text_style)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned()
        {
            let mut style = kiana_tui::components::markdown::DefaultTextStyle::default();
            if let Some(reference) = object_fn_ref(&ctx, &style_object, "color")? {
                style.color = Some(style_fn_from_ref(reference));
            }
            if let Some(reference) = object_fn_ref(&ctx, &style_object, "bgColor")? {
                style.bg_color = Some(style_fn_from_ref(reference));
            }
            style.bold = opt_bool(&style_object, "bold")?.unwrap_or(false);
            style.italic = opt_bool(&style_object, "italic")?.unwrap_or(false);
            style.strikethrough = opt_bool(&style_object, "strikethrough")?.unwrap_or(false);
            style.underline = opt_bool(&style_object, "underline")?.unwrap_or(false);
            markdown = markdown.with_default_text_style(style);
        }
        if let Some(options_object) = opt_value(&options)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned()
        {
            let mut markdown_options = MarkdownOptions {
                preserve_ordered_list_markers: opt_bool(
                    &options_object,
                    "preserveOrderedListMarkers",
                )?
                .unwrap_or(false),
                preserve_backslash_escapes: opt_bool(&options_object, "preserveBackslashEscapes")?
                    .unwrap_or(false),
                render_latex: opt_bool(&options_object, "renderLatex")?.unwrap_or(true),
                ..MarkdownOptions::default()
            };
            // `transform?: (markdown, availableWidth) => string`（`markdown.ts:248`）。
            if let Some(reference) = object_fn_ref(&ctx, &options_object, "transform")? {
                markdown_options.transform = Some(transform_fn_from_ref(reference));
            }
            markdown = markdown.with_options(markdown_options);
        }
        Ok(JsMarkdown {
            inner: RefCell::new(markdown),
        })
    }

    /// `setText(text)`。
    #[qjs(rename = "setText")]
    fn set_text(&self, text: String) {
        self.inner.borrow_mut().set_text(text);
    }

    /// `invalidate()`。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self) {
        self.inner.borrow_mut().invalidate();
    }

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let lines = Component::render(&mut *self.inner.borrow_mut(), width);
        lines_to_js(&ctx, lines)
    }
}

// ===========================================================================
// Image
// ===========================================================================

/// `@earendil-works/kiana-tui` 的 `Image`。
///
/// `options` 只读取 `maxWidthCells`/`maxHeightCells`/`filename`/`imageId`（`imageId` 取
/// 非负有限值后截断为 `u64`）；`dimensions` 对象缺失时由 `Image` 自己决定尺寸，提供时
/// `widthPx`/`heightPx` 分别缺省 800/600。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "Image")]
pub struct JsImage {
    #[qjs(skip_trace)]
    inner: RefCell<Image>,
}

#[rquickjs::methods]
impl JsImage {
    /// `new Image(base64Data, mimeType, theme, options = {}, dimensions?)`。
    #[qjs(constructor)]
    fn new<'js>(
        ctx: Ctx<'js>,
        base64_data: Opt<Value<'js>>,
        mime_type: Opt<Value<'js>>,
        theme: Opt<Value<'js>>,
        options: Opt<Value<'js>>,
        dimensions: Opt<Value<'js>>,
    ) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let base64_data = opt_arg_string(&base64_data).unwrap_or_default();
        let mime_type = opt_arg_string(&mime_type).unwrap_or_default();
        let mut image_theme = ImageTheme::plain();
        if let Some(theme_object) = opt_value(&theme)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned()
            && let Some(reference) = object_fn_ref(&ctx, &theme_object, "fallbackColor")?
        {
            image_theme.fallback_color = style_fn_from_ref(reference);
        }
        let mut image_options = ImageOptions::default();
        if let Some(options_object) = opt_value(&options)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned()
        {
            image_options.max_width_cells = opt_number(&options_object, "maxWidthCells")?;
            image_options.max_height_cells = opt_number(&options_object, "maxHeightCells")?;
            image_options.filename = opt_string(&options_object, "filename")?;
            image_options.image_id = opt_number(&options_object, "imageId")?
                .filter(|value| value.is_finite())
                .map(|value| value.max(0.0) as u64);
        }
        let parsed_dimensions = opt_value(&dimensions)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned()
            .map(|object| -> EngineResult<kiana_tui::ImageDimensions> {
                Ok(kiana_tui::ImageDimensions {
                    width_px: opt_number(&object, "widthPx")?.unwrap_or(800.0),
                    height_px: opt_number(&object, "heightPx")?.unwrap_or(600.0),
                })
            })
            .transpose()?;
        Ok(JsImage {
            inner: RefCell::new(Image::new(
                base64_data,
                mime_type,
                image_theme,
                image_options,
                parsed_dimensions,
            )),
        })
    }

    /// `getImageId()`。
    #[qjs(rename = "getImageId")]
    fn get_image_id<'js>(&self, ctx: Ctx<'js>) -> Value<'js> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match self.inner.borrow().get_image_id() {
            Some(id) => Value::new_number(ctx, id as f64),
            None => Value::new_undefined(ctx),
        }
    }

    /// `invalidate()`。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self) {
        self.inner.borrow_mut().invalidate();
    }

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let lines = Component::render(&mut *self.inner.borrow_mut(), width);
        lines_to_js(&ctx, lines)
    }
}

// ===========================================================================
// ScrollView
// ===========================================================================

/// `@earendil-works/kiana-tui` 的 `ScrollView`。
///
/// 只允许一个子组件：`addChild`/`removeChild`/`clear` 与 TS 一样永远抛错
/// （`scroll-view.ts:206-216`）。`follow`/`overscroll`/`scrollbar` 的原始字符串在 getter 上
/// 原样返回，Rust 枚举只保留渲染需要的取值（未知值落到 `None`/`Hidden`）。
/// `updateLayout` 的第三个参数替换重绘回调；缺省时清空，不会回退到宿主安装的回调。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "ScrollView")]
pub struct JsScrollView {
    #[qjs(skip_trace)]
    inner: RefCell<ScrollView>,
    #[qjs(skip_trace)]
    request_render: RefCell<Option<Rc<dyn Fn()>>>,
    /// `updateLayout` 第三个参数的引用 id（替换时释放旧值）。
    #[qjs(skip_trace)]
    request_render_ref: RefCell<Option<u64>>,
    /// 构造期的只读字段（TS 是 `readonly followEnd/primary/overscroll`）。
    #[qjs(skip_trace)]
    follow_end: bool,
    #[qjs(skip_trace)]
    primary: bool,
    #[qjs(skip_trace)]
    overscroll: String,
    /// `scrollbar` 的原始字符串。TS 的 getter 返回构造/`setScrollbar` 存入的值（可以是任意
    /// 字符串，`isScrollbarVisible` 只比较 `"always"`/`"auto"`），Rust 的枚举无法表达未知值。
    #[qjs(skip_trace)]
    scrollbar_raw: RefCell<String>,
}

#[rquickjs::methods]
impl JsScrollView {
    /// `new ScrollView(component, options = {})`。
    #[qjs(constructor)]
    fn new<'js>(
        ctx: Ctx<'js>,
        component: Opt<Value<'js>>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let Some(component) = opt_value(&component) else {
            return Err(Exception::throw_message(
                &ctx,
                "ScrollView requires exactly one child component",
            ));
        };
        let Some(object) = component.as_object() else {
            return Err(Exception::throw_message(&ctx, "component is not an object"));
        };
        let reference = store_ref(&ctx, object.clone().into_value())?;
        let Some(engine) = JsEngineSlot::current() else {
            return Err(Exception::throw_message(
                &ctx,
                "Cannot create a ScrollView: the extension runtime is not available",
            ));
        };
        let child: Box<dyn Component> = Box::new(JsComponent::from_ref(&engine, reference));
        let mut scroll_options = ScrollViewOptions::default();
        let mut follow_end = false;
        let mut primary = false;
        let mut overscroll = "chain".to_owned();
        let mut scrollbar_raw = "hidden".to_owned();
        let options_object = opt_value(&options)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        if let Some(object) = options_object.as_ref() {
            scroll_options.axis = opt_string(object, "axis")?;
            // TS 只把 `follow === "end"` 当作 `end`，其余字符串原样存储但不报错
            // （`scroll-view.ts:47`）；`overscroll` / `scrollbar` 同理（类型注解之外不做校验）。
            follow_end = opt_string(object, "follow")?.as_deref() == Some("end");
            scroll_options.follow = Some(if follow_end {
                kiana_tui::components::scroll_view::ScrollViewFollow::End
            } else {
                kiana_tui::components::scroll_view::ScrollViewFollow::None
            });
            primary = opt_bool(object, "primary")?.unwrap_or(false);
            scroll_options.primary = Some(primary);
            if let Some(value) = opt_string(object, "overscroll")? {
                overscroll = value.clone();
                scroll_options.overscroll = Some(if value == "contain" {
                    kiana_tui::layout_node::Overscroll::Contain
                } else {
                    kiana_tui::layout_node::Overscroll::Chain
                });
            }
            if let Some(value) = opt_string(object, "scrollbar")? {
                scrollbar_raw = value.clone();
                scroll_options.scrollbar = Some(match value.as_str() {
                    "auto" => ScrollbarVisibility::Auto,
                    "always" => ScrollbarVisibility::Always,
                    _ => ScrollbarVisibility::Hidden,
                });
            }
            if let Some(reference) = object_fn_ref(&ctx, object, "scrollbarTrackStyle")? {
                scroll_options.scrollbar_track_style = Some(send_style_fn_from_ref(reference));
            }
            if let Some(reference) = object_fn_ref(&ctx, object, "scrollbarThumbStyle")? {
                scroll_options.scrollbar_thumb_style = Some(send_style_fn_from_ref(reference));
            }
            scroll_options.scrollbar_hide_delay_ms = opt_number(object, "scrollbarHideDelayMs")?;
        }
        let view = ScrollView::new(child, scroll_options)
            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?;
        Ok(JsScrollView {
            inner: RefCell::new(view),
            request_render: RefCell::new(None),
            request_render_ref: RefCell::new(None),
            follow_end,
            primary,
            overscroll,
            scrollbar_raw: RefCell::new(scrollbar_raw),
        })
    }

    /// 安装重绘回调（宿主在构造后调用）。
    #[qjs(rename = "setRequestRender")]
    fn set_request_render<'js>(&self, ctx: Ctx<'js>, tui: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let tui_object = opt_value(&tui)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        let callback = request_render_from_js(&ctx, tui_object.as_ref());
        self.inner.borrow().set_request_render(callback.clone());
        *self.request_render.borrow_mut() = callback;
        Ok(())
    }

    /// `scrollbar`（TS getter）：构造/`setScrollbar` 存入的原始字符串。
    #[qjs(get, rename = "scrollbar")]
    fn scrollbar(&self) -> String {
        self.scrollbar_raw.borrow().clone()
    }

    /// `scrollTop` getter。
    #[qjs(get, rename = "scrollTop")]
    fn scroll_top(&self) -> f64 {
        self.inner.borrow().scroll_top() as f64
    }

    /// `viewportHeight` getter。
    #[qjs(get, rename = "viewportHeight")]
    fn viewport_height(&self) -> f64 {
        self.inner.borrow().viewport_height() as f64
    }

    /// `isScrollbarVisible` getter。
    ///
    /// 惰性实现可能在读取时结算滚动条隐藏定时器并发起重绘请求（JS 回调），因此必须
    /// 复用当前事务（§2.2）。
    #[qjs(get, rename = "isScrollbarVisible")]
    fn is_scrollbar_visible(&self, ctx: Ctx<'_>) -> bool {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow().is_scrollbar_visible()
    }

    /// `isScrollbarActive` getter。
    #[qjs(get, rename = "isScrollbarActive")]
    fn is_scrollbar_active(&self) -> bool {
        self.inner.borrow().is_scrollbar_active()
    }

    /// `followEnd`（TS `readonly followEnd: boolean`）。
    #[qjs(get, rename = "followEnd")]
    fn follow_end_field(&self) -> bool {
        self.follow_end
    }

    /// `primary`（TS `readonly primary: boolean`）。
    #[qjs(get, rename = "primary")]
    fn primary_field(&self) -> bool {
        self.primary
    }

    /// `overscroll`（TS `readonly overscroll: "chain" | "contain"`）。
    #[qjs(get, rename = "overscroll")]
    fn overscroll_field(&self) -> String {
        self.overscroll.clone()
    }

    /// `setScrollbar(scrollbar)`。
    #[qjs(rename = "setScrollbar")]
    fn set_scrollbar(&self, ctx: Ctx<'_>, scrollbar: String) {
        let _active = ActiveCtxGuard::enter(&ctx);
        // TS 不做校验：任意字符串都保存下来（只有 `"auto"`/`"always"` 有可见效果）。
        *self.scrollbar_raw.borrow_mut() = scrollbar.clone();
        let value = match scrollbar.as_str() {
            "auto" => ScrollbarVisibility::Auto,
            "always" => ScrollbarVisibility::Always,
            _ => ScrollbarVisibility::Hidden,
        };
        self.inner.borrow().set_scrollbar(value);
    }

    /// `addChild(component)`：TS 的 `ScrollView` 只有一个子项，永远抛错
    /// （`scroll-view.ts:206-208`）。
    #[qjs(rename = "addChild")]
    fn add_child(&self, ctx: Ctx<'_>, _component: Opt<Value<'_>>) -> EngineResult<()> {
        Err(Exception::throw_message(
            &ctx,
            "ScrollView has exactly one child",
        ))
    }

    /// `removeChild(component)`：永远抛错（`scroll-view.ts:210-212`）。
    #[qjs(rename = "removeChild")]
    fn remove_child(&self, ctx: Ctx<'_>, _component: Opt<Value<'_>>) -> EngineResult<()> {
        Err(Exception::throw_message(
            &ctx,
            "ScrollView child cannot be removed",
        ))
    }

    /// `clear()`：永远抛错（`scroll-view.ts:214-216`）。
    #[qjs(rename = "clear")]
    fn clear(&self, ctx: Ctx<'_>) -> EngineResult<()> {
        Err(Exception::throw_message(
            &ctx,
            "ScrollView child cannot be cleared",
        ))
    }

    /// `getContentWidth(width)`。
    #[qjs(rename = "getContentWidth")]
    fn get_content_width<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> f64 {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        self.inner.borrow().get_content_width(width) as f64
    }

    /// `setScrollbarActive(active)`。
    #[qjs(rename = "setScrollbarActive")]
    fn set_scrollbar_active(&self, ctx: Ctx<'_>, active: bool) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow().set_scrollbar_active(active);
    }

    /// `scrollTo(scrollTop, options = {})`。
    #[qjs(rename = "scrollTo")]
    fn scroll_to<'js>(
        &self,
        ctx: Ctx<'js>,
        scroll_top: Opt<Value<'js>>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let scroll_top = opt_arg_number(&ctx, &scroll_top).unwrap_or(0.0);
        let mut scroll_options = ScrollViewScrollToOptions::default();
        if let Some(object) = opt_value(&options)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned()
        {
            scroll_options.disable_follow = opt_bool(&object, "disableFollow")?;
        }
        self.inner.borrow().scroll_to(scroll_top, scroll_options);
        Ok(())
    }

    /// `scrollBy(lines)`。
    #[qjs(rename = "scrollBy")]
    fn scroll_by<'js>(&self, ctx: Ctx<'js>, lines: Opt<Value<'js>>) -> f64 {
        let _active = ActiveCtxGuard::enter(&ctx);
        let lines = opt_arg_number(&ctx, &lines).unwrap_or(0.0);
        self.inner.borrow().scroll_by(lines)
    }

    /// `scrollToStart()`。
    #[qjs(rename = "scrollToStart")]
    fn scroll_to_start(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow().scroll_to_start();
    }

    /// `scrollToEnd()`。
    #[qjs(rename = "scrollToEnd")]
    fn scroll_to_end(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow().scroll_to_end();
    }

    /// `updateLayout(contentHeight, viewportHeight, requestRender)`。
    ///
    /// TS 每次都把第三个参数存进 `requestRenderCallback`（`scroll-view.ts:189-192`）；
    /// 缺省即清除（不会回退到 `setRequestRender` 安装的宿主回调）。
    #[qjs(rename = "updateLayout")]
    fn update_layout<'js>(
        &self,
        ctx: Ctx<'js>,
        content_height: Opt<Value<'js>>,
        viewport_height: Opt<Value<'js>>,
        request_render: Opt<Value<'js>>,
    ) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let content_height = opt_arg_number(&ctx, &content_height)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let viewport_height = opt_arg_number(&ctx, &viewport_height)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let parsed = match opt_value(&request_render) {
            Some(value) => request_render_fn_from_js(&ctx, value),
            None => None,
        };
        let (callback, reference) = match parsed {
            Some((callback, reference)) => (Some(callback), Some(reference)),
            None => (None, None),
        };
        self.inner.borrow().set_request_render(callback.clone());
        *self.request_render.borrow_mut() = callback;
        if let Some(previous) = self.request_render_ref.replace(reference)
            && Some(previous) != reference
        {
            drop_ref(&ctx, previous);
        }
        let request: Rc<dyn Fn()> = self
            .request_render
            .borrow()
            .clone()
            .unwrap_or_else(|| Rc::new(|| {}));
        self.inner
            .borrow()
            .update_layout(content_height, viewport_height, request.as_ref());
        Ok(())
    }

    /// `isFollowingEnd()`。
    #[qjs(rename = "isFollowingEnd")]
    fn is_following_end(&self) -> bool {
        self.inner.borrow().is_following_end()
    }

    /// `invalidate()`。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().invalidate();
    }

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let lines = Component::render(&mut *self.inner.borrow_mut(), width);
        lines_to_js(&ctx, lines)
    }

    /// `handleMouse(event)`。
    #[qjs(rename = "handleMouse")]
    fn handle_mouse<'js>(&self, ctx: Ctx<'js>, event: Value<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let event = mouse_event_from_js(&ctx, event)?;
        let result = Component::handle_mouse(&mut *self.inner.borrow_mut(), &event);
        mouse_result_to_js(&ctx, result)
    }
}

// ===========================================================================
// CancellableLoader
// ===========================================================================

/// 构造一个与 `CancellationToken` 关联的 JS `AbortSignal`。
///
/// 优先复用 `js/context.js` 的 `__kiana_ctx.signal(handle)`（真正的 `AbortSignal` 原型，支持
/// `instanceof` 与 `AbortSignal.any`）；context 桥未安装时返回一个新建的
/// `AbortController().signal`（不会 abort，但保留 `AbortSignal` 的形状）。
fn abort_signal_for_token<'js>(
    ctx: &Ctx<'js>,
    token: &tokio_util::sync::CancellationToken,
) -> EngineResult<Value<'js>> {
    let context_result = ctx.globals().get::<_, Object>("__kiana_ctx");
    if let Ok(context) = context_result {
        if let Ok(signal) = context.get::<_, Function>("signal") {
            let handle = crate::runner::context::register_signal_handle(token);
            match signal.call((This(context), handle)) {
                Ok(value) => return Ok(value),
                Err(_) => {
                    let _ = ctx.catch();
                }
            }
        }
    } else {
        // `__kiana_ctx` 未安装（只有 ui 适配层的纯测试）：清掉这次失败的属性读取。
        let _ = ctx.catch();
    }
    let constructor: rquickjs::Constructor = ctx.globals().get("AbortController")?;
    let controller: Object = constructor.construct(())?;
    controller.get("signal")
}

/// `@earendil-works/kiana-tui` 的 `CancellableLoader`。
///
/// `signal` 每次读取都从内部 `CancellationToken` 造一个 `AbortSignal`：优先复用
/// `__kiana_ctx.signal`（真正的 `AbortSignal` 原型），桥未安装时退化为一个永不 abort 的
/// `AbortController().signal`。Esc 取消时调用 `onAbort`（回调以引用 id 保存）。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "CancellableLoader")]
pub struct JsCancellableLoader {
    #[qjs(skip_trace)]
    inner: RefCell<CancellableLoader>,
    /// `onAbort` 回调引用。与 `CancellableLoader::on_abort` 闭包共享同一个槽位：构造函数
    /// 里注册的 Rust 回调读的就是这里（TS `cancellable-loader.ts:29-35`）。
    #[qjs(skip_trace)]
    on_abort_ref: Rc<RefCell<Option<u64>>>,
}

#[rquickjs::methods]
impl JsCancellableLoader {
    /// `new CancellableLoader(tui, spinnerColorFn, messageColorFn, message, indicator?)`。
    #[qjs(constructor)]
    fn new<'js>(
        ctx: Ctx<'js>,
        tui: Opt<Value<'js>>,
        spinner_color: Opt<Value<'js>>,
        message_color: Opt<Value<'js>>,
        message: Opt<Value<'js>>,
        indicator: Opt<Value<'js>>,
    ) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let tui_object = opt_value(&tui)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        let request_render = request_render_from_js(&ctx, tui_object.as_ref());
        let spinner_color = match opt_value(&spinner_color) {
            Some(value) => store_function_ref(&ctx, value)?
                .map(style_fn_from_ref)
                .unwrap_or_else(|| Arc::new(|text: &str| text.to_string())),
            None => Arc::new(|text: &str| text.to_string()),
        };
        let message_color = match opt_value(&message_color) {
            Some(value) => store_function_ref(&ctx, value)?
                .map(style_fn_from_ref)
                .unwrap_or_else(|| Arc::new(|text: &str| text.to_string())),
            None => Arc::new(|text: &str| text.to_string()),
        };
        let message = opt_arg_string(&message).unwrap_or_else(|| "Loading...".to_owned());
        let indicator_object = opt_value(&indicator)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        let indicator = loader_indicator_from_object(&ctx, indicator_object.as_ref())?;
        let mut loader = CancellableLoader::new(
            request_render,
            spinner_color,
            message_color,
            &message,
            indicator,
        );
        loader.set_timer(Box::new(JsIntervalTimer::new()));
        let on_abort_ref: Rc<RefCell<Option<u64>>> = Rc::new(RefCell::new(None));
        let slot = Rc::clone(&on_abort_ref);
        loader.on_abort = Some(Box::new(move || {
            let reference = *slot.borrow();
            if let Some(reference) = reference
                && let Some(engine) = JsEngineSlot::current()
            {
                let _ = with_engine_js(&engine, |ctx| -> EngineResult<()> {
                    let _ = call_ref::<Value>(&ctx, reference, None, &[]);
                    Ok(())
                });
            }
        }));
        Ok(JsCancellableLoader {
            inner: RefCell::new(loader),
            on_abort_ref,
        })
    }

    /// `signal`：与取消令牌关联的 `AbortSignal`。
    #[qjs(get, rename = "signal")]
    fn get_signal<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let token = self.inner.borrow().signal();
        abort_signal_for_token(&ctx, &token)
    }

    /// `aborted`。
    #[qjs(get, rename = "aborted")]
    fn get_aborted(&self) -> bool {
        self.inner.borrow().aborted()
    }

    /// `onAbort = fn`。
    #[qjs(get, rename = "onAbort")]
    fn get_on_abort<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match *self.on_abort_ref.borrow() {
            Some(reference) => load_ref(&ctx, reference),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `onAbort = fn`。
    #[qjs(set, rename = "onAbort")]
    fn set_on_abort<'js>(&self, ctx: Ctx<'js>, value: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let reference = match opt_value(&value) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        *self.on_abort_ref.borrow_mut() = reference;
        Ok(())
    }

    /// `handleInput(data)`：Esc 触发取消（会调用 JS `onAbort`，必须复用当前事务）。
    #[qjs(rename = "handleInput")]
    fn handle_input(&self, ctx: Ctx<'_>, data: String) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().handle_input(&data);
    }

    /// `start()`。
    #[qjs(rename = "start")]
    fn start(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().start();
    }

    /// `stop()`。
    #[qjs(rename = "stop")]
    fn stop(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().stop();
    }

    /// `setMessage(message)`。
    #[qjs(rename = "setMessage")]
    fn set_message(&self, ctx: Ctx<'_>, message: String) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().set_message(&message);
    }

    /// `setText(text)`（`CancellableLoader extends Loader extends Text`）。
    #[qjs(rename = "setText")]
    fn set_text(&self, ctx: Ctx<'_>, text: String) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().set_message(&text);
    }

    /// `setPaddingX(paddingX)`（继承自 `Text`）。
    #[qjs(rename = "setPaddingX")]
    fn set_padding_x<'js>(&self, ctx: Ctx<'js>, padding_x: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let padding = opt_arg_number(&ctx, &padding_x)
            .map(non_negative_floor)
            .unwrap_or(0);
        self.inner.borrow_mut().loader_mut().set_padding_x(padding);
        Ok(())
    }

    /// `setIndicator(indicator?)`。
    #[qjs(rename = "setIndicator")]
    fn set_indicator<'js>(&self, ctx: Ctx<'js>, indicator: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let indicator_object = opt_value(&indicator)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        let indicator = loader_indicator_from_object(&ctx, indicator_object.as_ref())?;
        self.inner.borrow_mut().set_indicator(indicator);
        Ok(())
    }

    /// `dispose()`。
    #[qjs(rename = "dispose")]
    fn dispose(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.inner.borrow_mut().dispose();
    }

    /// `invalidate()`。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        Component::invalidate(&mut *self.inner.borrow_mut());
    }

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let lines = Component::render(&mut *self.inner.borrow_mut(), width);
        lines_to_js(&ctx, lines)
    }
}

// ===========================================================================
// 终端代理（Editor 只需要 terminal.rows 与 requestRender）
// ===========================================================================

thread_local! {
    /// `BridgeTerminal` 使用的终端尺寸（默认 `(24, 80)`）。
    ///
    /// 只有 `host_terminal` 无法识别宿主实现时才读到这里；`TuiMainScreen`/`TuiAltScreen`
    /// 会通过 `terminal()` 提供真实终端。宿主要覆盖这条回退路径时调用
    /// [`set_terminal_size`]。
    static TERMINAL_SIZE: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((24, 80)) };
}

/// 设置 `BridgeTerminal` 回退使用的终端尺寸。
///
/// 参数顺序是 `(rows, columns)`，与 [`terminal_size`] 和 `BridgeTerminal::rows`/`columns`
/// 一致。只在宿主的 `Tui` 实现无法被 `host_terminal` 识别时才会被 Editor 读到。
pub fn set_terminal_size(rows: usize, columns: usize) {
    TERMINAL_SIZE.with(|size| size.set((rows, columns)));
}

/// 读取 `BridgeTerminal` 回退使用的终端尺寸 `(rows, columns)`。
pub fn terminal_size() -> (usize, usize) {
    TERMINAL_SIZE.with(|size| size.get())
}

/// `kiana_tui::Terminal` 的最小实现：`rows`/`columns` 来自线程局部的 [`TERMINAL_SIZE`]，
/// `kitty_protocol_active` 委托给进程级检测，其余方法都是 no-op（不写终端、不读输入）。
struct BridgeTerminal;

impl kiana_tui::Terminal for BridgeTerminal {
    fn start(&mut self, _on_input: Box<dyn FnMut(&str)>, _on_resize: Box<dyn FnMut()>) {}
    fn stop(&mut self) {}
    fn drain_input(
        &mut self,
        _max_ms: u64,
        _idle_ms: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        Box::pin(async {})
    }
    fn write(&mut self, _data: &str) {}
    fn columns(&self) -> usize {
        terminal_size().1
    }
    fn rows(&self) -> usize {
        terminal_size().0
    }
    fn kitty_protocol_active(&self) -> bool {
        kiana_tui::is_kitty_protocol_active()
    }
    fn move_by(&mut self, _lines: isize) {}
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn set_title(&mut self, _title: &str) {}
    fn set_progress(&mut self, _active: bool) {}
    fn set_program_status(&mut self, _status: &kiana_tui::ProgramStatus) {}
}

// ===========================================================================
// 终端对象（`tui.terminal`）
// ===========================================================================

/// `TUI.terminal`（TS `interface Terminal` 的运行期面）。
///
/// TS 的 `TuiBase.terminal` 是公开字段，扩展用它读取实时行列、写终端、设置标题等。
/// Rust 侧包裹宿主的 [`kiana_tui::Terminal`] 句柄（[`Tui`] trait 不暴露终端，见
/// `host_terminal`）；没有宿主终端时退回静态尺寸的 `BridgeTerminal`。
///
/// 未实现 `start` 的输入/尺寸回调（JS 无法提供 Rust 的 `Terminal::start` 需要的两个
/// 闭包），保留为 no-op，见报告剩余缺口。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "Terminal")]
pub struct JsTerminal {
    #[qjs(skip_trace)]
    inner: Option<Rc<RefCell<Box<dyn kiana_tui::Terminal>>>>,
}

impl JsTerminal {
    /// 用宿主终端句柄构造（`tui.terminal` 读取时使用）。
    fn from_terminal(terminal: Rc<RefCell<Box<dyn kiana_tui::Terminal>>>) -> Self {
        JsTerminal {
            inner: Some(terminal),
        }
    }

    /// 只读访问宿主终端；`inner` 为 `None`（`new Terminal()` 造出的空壳实例）时返回
    /// `None`，只读成员因此落到各自的默认值（`columns` → 80、`rows` → 24）。
    fn read<R>(&self, f: impl FnOnce(&dyn kiana_tui::Terminal) -> R) -> Option<R> {
        let terminal = self.inner.as_ref()?.borrow();
        Some(f(terminal.as_ref()))
    }

    /// 可变访问宿主终端；空壳实例下返回 `None`，调用方的写操作因此静默跳过。
    fn with_terminal<R>(&self, f: impl FnOnce(&mut dyn kiana_tui::Terminal) -> R) -> Option<R> {
        let mut terminal = self.inner.as_ref()?.borrow_mut();
        Some(f(terminal.as_mut()))
    }
}

#[rquickjs::methods]
impl JsTerminal {
    /// 原生构造器不对外开放（由 `tui.terminal` 提供实例）。
    #[qjs(constructor)]
    fn new() -> EngineResult<Self> {
        Ok(JsTerminal { inner: None })
    }

    /// `columns`（实时）。
    #[qjs(get, rename = "columns")]
    fn columns(&self) -> f64 {
        self.read(|terminal| terminal.columns()).unwrap_or(80) as f64
    }

    /// `rows`（实时）。
    #[qjs(get, rename = "rows")]
    fn rows(&self) -> f64 {
        self.read(|terminal| terminal.rows()).unwrap_or(24) as f64
    }

    /// `kittyProtocolActive`。
    #[qjs(get, rename = "kittyProtocolActive")]
    fn kitty_protocol_active(&self) -> bool {
        self.read(|terminal| terminal.kitty_protocol_active())
            .unwrap_or(false)
    }

    /// `write(data)`。
    #[qjs(rename = "write")]
    fn write(&self, data: String) {
        self.with_terminal(|terminal| terminal.write(&data));
    }

    /// `moveBy(lines)`。
    #[qjs(rename = "moveBy")]
    fn move_by(&self, lines: f64) {
        let lines = if lines.is_finite() {
            lines.trunc() as isize
        } else {
            0
        };
        self.with_terminal(|terminal| terminal.move_by(lines));
    }

    /// `hideCursor()`。
    #[qjs(rename = "hideCursor")]
    fn hide_cursor(&self) {
        self.with_terminal(|terminal| terminal.hide_cursor());
    }

    /// `showCursor()`。
    #[qjs(rename = "showCursor")]
    fn show_cursor(&self) {
        self.with_terminal(|terminal| terminal.show_cursor());
    }

    /// `clearLine()`。
    #[qjs(rename = "clearLine")]
    fn clear_line(&self) {
        self.with_terminal(|terminal| terminal.clear_line());
    }

    /// `clearFromCursor()`。
    #[qjs(rename = "clearFromCursor")]
    fn clear_from_cursor(&self) {
        self.with_terminal(|terminal| terminal.clear_from_cursor());
    }

    /// `clearScreen()`。
    #[qjs(rename = "clearScreen")]
    fn clear_screen(&self) {
        self.with_terminal(|terminal| terminal.clear_screen());
    }

    /// `setTitle(title)`。
    #[qjs(rename = "setTitle")]
    fn set_title(&self, title: String) {
        self.with_terminal(|terminal| terminal.set_title(&title));
    }

    /// `setProgress(active)`。
    #[qjs(rename = "setProgress")]
    fn set_progress(&self, active: bool) {
        self.with_terminal(|terminal| terminal.set_progress(active));
    }

    /// `setProgramStatus(status)`。
    ///
    /// 参数转成 JSON 后反序列化为 `kiana_tui::ProgramStatus`；形状不对或字段缺失时静默
    /// 忽略（不抛错），`undefined`/`null` 直接返回。
    #[qjs(rename = "setProgramStatus")]
    fn set_program_status<'js>(&self, ctx: Ctx<'js>, status: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(status) = opt_value(&status) else {
            return Ok(());
        };
        let parsed: Option<kiana_tui::ProgramStatus> = value_to_json(&ctx, status)
            .ok()
            .and_then(|json| serde_json::from_value(json).ok());
        if let Some(status) = parsed {
            self.with_terminal(|terminal| terminal.set_program_status(&status));
        }
        Ok(())
    }

    /// `stop()`。
    #[qjs(rename = "stop")]
    fn stop(&self) {
        self.with_terminal(|terminal| terminal.stop());
    }

    /// `start(...)`：TS 的输入/尺寸回调无法从 JS 侧提供，保留为 no-op。
    #[qjs(rename = "start")]
    fn start(&self) {}

    /// `restoreOnExit(before, after)`。
    #[qjs(rename = "restoreOnExit")]
    fn restore_on_exit(&self, before: String, after: String) {
        self.with_terminal(|terminal| terminal.restore_on_exit(&before, &after));
    }

    /// `drainInput(maxMs?, idleMs?)`：返回已完成的 Promise。
    ///
    /// Rust 的 [`kiana_tui::Terminal::drain_input`] 需要把 `&mut` 借用跨进 future，同步方法里
    /// 无法驱动；TS 只在退出流程里 `await` 它，这里立即完成（见报告剩余缺口）。
    #[qjs(rename = "drainInput")]
    fn drain_input<'js>(
        &self,
        ctx: Ctx<'js>,
        _max_ms: Opt<Value<'js>>,
        _idle_ms: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let promise: Object = ctx.globals().get("Promise")?;
        let resolve: Function = promise.get("resolve")?;
        resolve.call((promise,))
    }
}

// ===========================================================================
// TUI 代理
// ===========================================================================

/// `Arc<dyn Tui>` 的可变访问。
///
/// `Tui` 的成员方法都是 `&mut self`，而工厂签名（`kiana-ext-api` 的冻结契约）只给
/// `Arc<dyn Tui>`；`Arc::get_mut` 在引用计数 > 1 时不可用，因此这里按单线程前提取一次
/// 可变引用。
///
/// **前提（没有被运行时强制）**：调用期间不能有另一个 `&mut dyn Tui` 同时存活。代理通常
/// 只在同步调用里短暂进入；但 `f` 可能回调 JS（`renderNow`/`showOverlay` 会渲染 JS 组件），
/// 若该 JS 再调用代理方法（例如组件 `render` 里调用 `tui.requestRender()`），本函数会被
/// 重入并产生第二个 `&mut`。本函数没有重入检测或锁。
fn with_tui_mut<R>(tui: &Arc<dyn Tui>, f: impl FnOnce(&mut dyn Tui) -> R) -> R {
    let pointer = Arc::as_ptr(tui) as *mut dyn Tui;
    // SAFETY: 单线程 UI 代理的同步调用；调用方必须保证没有其他 `&mut` 与本次调用重叠
    // （包括 `f` 内 JS 回调触发的重入，见上面的说明）。
    unsafe { f(&mut *pointer) }
}

/// 从宿主 TUI 取回终端句柄（TS `TuiBase.terminal` 是公开字段）。
///
/// `Tui` trait 没有终端访问器（`tui.rs` 是 §4.9 的冻结契约），具体渲染器
/// （[`kiana_tui::TuiMainScreen`] / [`kiana_tui::TuiAltScreen`]）用 `Component::as_any` 暴露自身；
/// 未知宿主返回 `None`（调用方退回静态尺寸的 [`BridgeTerminal`]）。
fn host_terminal(tui: &Arc<dyn Tui>) -> Option<Rc<RefCell<Box<dyn kiana_tui::Terminal>>>> {
    let any = tui.as_any()?;
    if let Some(main) = any.downcast_ref::<kiana_tui::TuiMainScreen>() {
        return Some(Rc::clone(main.terminal()));
    }
    if let Some(alt) = any.downcast_ref::<kiana_tui::TuiAltScreen>() {
        return Some(Rc::clone(alt.terminal()));
    }
    None
}

/// 通过 `tui.addChild` 挂上去的子项记录。
struct AddedChild {
    /// JS 对象的引用 id。
    reference: u64,
    /// 组件在堆上的数据地址（`Box` 移动不改变它），`removeChild` 用它找回挂载项。
    address: usize,
}

/// `factory(tui, …)` 收到的 TUI 代理对象（`tui.ts:464–493` 的运行期面）。
///
/// 把 `Arc<dyn Tui>` 转成 JS 可见的方法面：渲染/停止、overlay 句柄、焦点、子组件增删、终端。
/// 工厂闭包是 `Send + Sync`、不持有引擎，因此代理只持有 TUI；`terminal` 首次读取时构造并
/// 缓存 `JsTerminal`（Drop 时只登记引用清理）。
///
/// `addChild` 记录 `(引用 id, 组件堆地址)`：`removeChild` 用对象身份找引用、用堆地址在 TUI
/// 的子项里定位（备用屏会把子项包进共享代理，因此还要在子树里递归查找）。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "TUIProxy")]
pub struct JsTui {
    #[qjs(skip_trace)]
    inner: Option<Arc<dyn Tui>>,
    #[qjs(skip_trace)]
    added: RefCell<Vec<AddedChild>>,
    /// `tui.terminal` 的 JS 实例引用（同一代理重复读取返回同一对象）。
    #[qjs(skip_trace)]
    terminal_ref: RefCell<Option<u64>>,
}

impl Drop for JsTui {
    fn drop(&mut self) {
        // `tui.terminal` 的实例缓存在 `__kiana_refs` 里；代理被 GC 时释放它（`JsTerminal`
        // 没有 `dispose`，延迟清理只做 drop_ref）。
        if let Some(reference) = self.terminal_ref.get_mut().take() {
            adapter::schedule_disposal(reference);
        }
        // `added` 里的组件引用由对应 `JsComponent` 的 Drop 释放（组件随 TUI 树销毁）；
        // 空代理（`new TUIProxy()`）没有组件包装，`addChild` 记录的引用没有释放路径。
    }
}

impl JsTui {
    /// 从 `Arc<dyn Tui>` 构造。
    ///
    /// 同时把它登记为 `LAST_TUI`（弱引用）：宿主 `onHandle` 回调拿到的 overlay 句柄没有
    /// TUI 引用，`unfocus({ target })` 解析组件对象时靠这个最近值回退。
    pub fn from_tui(tui: Arc<dyn Tui>) -> Self {
        // 记下最近的宿主 TUI：`onHandle` 收到的 overlay 句柄没有 TUI 引用，
        // `unfocus({target})` 解析组件对象时用它回退。
        LAST_TUI.with(|slot| *slot.borrow_mut() = Some(Arc::downgrade(&tui)));
        JsTui {
            inner: Some(tui),
            added: RefCell::new(Vec::new()),
            terminal_ref: RefCell::new(None),
        }
    }

    /// 内部 TUI。
    fn tui(&self) -> Option<&Arc<dyn Tui>> {
        self.inner.as_ref()
    }
}

#[rquickjs::methods]
impl JsTui {
    /// 原生构造器不对外开放（工厂提供实例）；`new TUIProxy()` 返回空代理。
    #[qjs(constructor)]
    fn new() -> EngineResult<Self> {
        Ok(JsTui {
            inner: None,
            added: RefCell::new(Vec::new()),
            terminal_ref: RefCell::new(None),
        })
    }

    /// `requestRender(force = false)`。
    #[qjs(rename = "requestRender")]
    fn request_render(&self, force: Option<bool>) {
        if let Some(tui) = self.tui() {
            with_tui_mut(tui, |tui| tui.request_render(force.unwrap_or(false)));
        }
    }

    /// `renderNow(force = false)`。
    ///
    /// 同步渲染会调用挂载树里的 JS 组件 `render`，因此必须复用当前事务（§2.2）。
    #[qjs(rename = "renderNow")]
    fn render_now(&self, ctx: Ctx<'_>, force: Option<bool>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        if let Some(tui) = self.tui() {
            with_tui_mut(tui, |tui| tui.render_now(force.unwrap_or(false)));
        }
    }

    /// `start()`。
    #[qjs(rename = "start")]
    fn start(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        if let Some(tui) = self.tui() {
            with_tui_mut(tui, |tui| tui.start());
        }
    }

    /// `stop()`。
    #[qjs(rename = "stop")]
    fn stop(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        if let Some(tui) = self.tui() {
            with_tui_mut(tui, |tui| {
                tui.stop(TuiStopOptions {
                    preserve_screen: false,
                })
            });
        }
    }

    /// `hideOverlay()`。
    #[qjs(rename = "hideOverlay")]
    fn hide_overlay(&self) {
        if let Some(tui) = self.tui() {
            with_tui_mut(tui, |tui| tui.hide_overlay());
        }
    }

    /// `addChild(component)`。
    ///
    /// 代理没有内部 TUI（`new TUIProxy()`）时只记录引用，不实际挂载；`getChildComponents()`
    /// 仍然返回它。
    #[qjs(rename = "addChild")]
    fn add_child<'js>(&self, ctx: Ctx<'js>, component: Value<'js>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(object) = component.as_object() else {
            return Err(Exception::throw_message(&ctx, "component is not an object"));
        };
        let reference = store_ref(&ctx, object.clone().into_value())?;
        let Some(engine) = JsEngineSlot::current() else {
            drop_ref(&ctx, reference);
            return Err(Exception::throw_message(
                &ctx,
                "Cannot add a component: the extension runtime is not available",
            ));
        };
        let Some(tui) = self.tui() else {
            self.added.borrow_mut().push(AddedChild {
                reference,
                address: 0,
            });
            return Ok(());
        };
        let child: Box<dyn Component> = Box::new(JsComponent::from_ref(&engine, reference));
        // `Box` 移进 `Vec<Box<dyn Component>>` 不改变堆地址；`removeChild` 按它找回挂载项。
        let address = child.as_ref() as *const dyn Component as *const () as usize;
        with_tui_mut(tui, |tui| tui.add_child(child));
        self.added
            .borrow_mut()
            .push(AddedChild { reference, address });
        Ok(())
    }

    /// `removeChild(component)`。
    ///
    /// TS 的 `Container.removeChild` 按对象引用找到挂载项并 `splice`，不调用 `dispose()`
    /// （`tui.ts:370-377`）。Rust 侧按 addChild 时记录的堆地址找回顶层子项并移除；
    /// 组件的 `JsComponent` 包装被 `detach()`，因此不会误调 JS `dispose()`，引用表项由
    /// 代理释放。若被移除的子树正持有焦点，焦点清空（TS 保留对已移除组件的引用，
    /// Rust 的索引目标无法表达这一点）。
    #[qjs(rename = "removeChild")]
    fn remove_child<'js>(&self, ctx: Ctx<'js>, component: Value<'js>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(object) = component.as_object().cloned() else {
            return Ok(());
        };
        let references: Vec<u64> = self
            .added
            .borrow()
            .iter()
            .map(|entry| entry.reference)
            .collect();
        let mut matched = None;
        for reference in references {
            if load_ref(&ctx, reference)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .is_some_and(|candidate| candidate == object)
            {
                matched = Some(reference);
                break;
            }
        }
        let Some(reference) = matched else {
            return Ok(());
        };
        let entry = {
            let mut added = self.added.borrow_mut();
            added
                .iter()
                .position(|entry| entry.reference == reference)
                .map(|index| added.remove(index))
        };
        let Some(entry) = entry else {
            return Ok(());
        };
        if let Some(tui) = self.tui() {
            let mut index = None;
            with_tui_mut(tui, |tui| {
                tui.with_children(&mut |children| {
                    let Some(found) = children.iter().position(|child| {
                        child.as_ref() as *const dyn Component as *const () as usize
                            == entry.address
                            || subtree_contains(child.as_ref(), entry.address)
                    }) else {
                        return;
                    };
                    // 先就地 `detach()`：组件从树里摘下后 Drop 不再误调 JS `dispose()`（TS 的
                    // `Container.removeChild` 只是 splice）。备用屏会把子项包进共享代理，
                    // 因此沿容器子项下钻（代理会透传遍历）。
                    detach_js_components(children[found].as_mut());
                    index = Some(found);
                });
                if let Some(index) = index {
                    tui.remove_child_at(index);
                }
            });
        }
        drop_ref(&ctx, reference);
        Ok(())
    }

    /// `showOverlay(component, options = {})`。
    #[qjs(rename = "showOverlay")]
    fn show_overlay<'js>(
        &self,
        ctx: Ctx<'js>,
        component: Value<'js>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(object) = component.as_object() else {
            return Err(Exception::throw_message(&ctx, "component is not an object"));
        };
        let reference = store_ref(&ctx, object.clone().into_value())?;
        let Some(engine) = JsEngineSlot::current() else {
            drop_ref(&ctx, reference);
            return Err(Exception::throw_message(
                &ctx,
                "Cannot show an overlay: the extension runtime is not available",
            ));
        };
        let options_object = opt_value(&options)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        let overlay_options = overlay_options_from_js(&ctx, options_object.as_ref())?;
        let Some(tui) = self.tui() else {
            return Ok(Value::new_undefined(ctx.clone()));
        };
        let child: Box<dyn Component> = Box::new(JsComponent::from_ref(&engine, reference));
        let handle = with_tui_mut(tui, |tui| tui.show_overlay(child, overlay_options));
        let instance = Class::instance(
            ctx.clone(),
            JsOverlayHandle {
                inner: RefCell::new(Some(handle)),
                tui: Some(Arc::clone(tui)),
            },
        )?;
        Ok(instance.into_value())
    }

    /// `hasOverlay()`。
    #[qjs(rename = "hasOverlay")]
    fn has_overlay(&self) -> bool {
        self.tui()
            .map(|tui| with_tui_mut(tui, |tui| tui.has_overlay()))
            .unwrap_or(false)
    }

    /// `setFocus(component | undefined)`。
    ///
    /// TS 按组件引用设置焦点（`tui.ts:621-623`）；Rust 的 [`FocusTarget`] 是树内下标，
    /// 因此按 `containsComponent` 语义在挂载树里找到该对象（JS `Container.children` 递归），
    /// 再聚焦包含它的组件路径。找不到时保持当前焦点不变。
    #[qjs(rename = "setFocus")]
    fn set_focus<'js>(&self, ctx: Ctx<'js>, component: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(tui) = self.tui() else {
            return Ok(());
        };
        let Some(value) = opt_value(&component) else {
            with_tui_mut(tui, |tui| tui.set_focus(None));
            return Ok(());
        };
        let Some(object) = value.as_object().cloned() else {
            return Ok(());
        };
        let target = resolve_component_focus_target(tui, &ctx, &object)?;
        if let Some(target) = target {
            with_tui_mut(tui, |tui| tui.set_focus(Some(target)));
        }
        Ok(())
    }

    /// `getChildComponents()`。
    #[qjs(rename = "getChildComponents")]
    fn get_child_components<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let array = Array::new(ctx.clone())?;
        for (index, entry) in self.added.borrow().iter().enumerate() {
            array.set(index, load_ref(&ctx, entry.reference)?)?;
        }
        Ok(array)
    }

    /// `terminal`：宿主终端（TS `TuiBase.terminal` 是公开字段；`rows`/`columns` 实时）。
    #[qjs(get, rename = "terminal")]
    fn terminal<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        if let Some(reference) = *self.terminal_ref.borrow()
            && let Ok(value) = load_ref(&ctx, reference)
            && !value.is_undefined()
        {
            return Ok(value);
        }
        let terminal = self.tui().and_then(host_terminal).unwrap_or_else(|| {
            Rc::new(RefCell::new(
                Box::new(BridgeTerminal) as Box<dyn kiana_tui::Terminal>
            ))
        });
        let instance = Class::instance(ctx.clone(), JsTerminal::from_terminal(terminal))?;
        let value = instance.into_value();
        let reference = store_ref(&ctx, value.clone())?;
        *self.terminal_ref.borrow_mut() = Some(reference);
        Ok(value)
    }
}

/// 挂载树里的 JS 组件：`(从顶层子项开始的下标路径, 引用 id)`。
fn collect_component_refs(
    component: &dyn Component,
    path: &mut Vec<usize>,
    out: &mut Vec<(Vec<usize>, u64)>,
) {
    if let Some(js) = component
        .as_any()
        .and_then(|any| any.downcast_ref::<JsComponent>())
    {
        out.push((path.clone(), js.reference()));
        return;
    }
    let mut index = 0usize;
    component.for_each_container_child(&mut |child| {
        path.push(index);
        collect_component_refs(child, path, out);
        path.pop();
        index += 1;
        true
    });
}

/// 把 JS 组件对象解析成 [`FocusTarget`]：先在挂载树里找精确匹配，再按 TS
/// `containsComponent` 的容器包含关系回退到包含它的挂载组件。
///
/// `tui.setFocus(component)` 与 `overlayHandle.unfocus({ target })` 共用
/// （`tui.ts:621-623、836-841`）。
fn resolve_component_focus_target<'js>(
    tui: &Arc<dyn Tui>,
    ctx: &Ctx<'js>,
    object: &Object<'js>,
) -> EngineResult<Option<FocusTarget>> {
    // 收集挂载树里的 JS 组件（下标路径 + 引用 id），期间不做 JS 调用。
    let mut mounted: Vec<(Vec<usize>, u64)> = Vec::new();
    with_tui_mut(tui, |tui| {
        let len = tui.children_len();
        for index in 0..len {
            let mut path = vec![index];
            tui.with_children(&mut |children| {
                if let Some(child) = children.get(index) {
                    collect_component_refs(child.as_ref(), &mut path, &mut mounted);
                }
            });
        }
    });
    let mut exact = None;
    let mut containing = None;
    for (path, reference) in &mounted {
        let Some(candidate) = load_ref(ctx, *reference)
            .ok()
            .and_then(|value| value.as_object().cloned())
        else {
            continue;
        };
        if candidate == *object {
            exact = Some(path.clone());
            break;
        }
        if containing.is_none() && contains_component_js(ctx, &candidate, object)? {
            containing = Some(path.clone());
        }
    }
    let Some(path) = exact.or(containing) else {
        return Ok(None);
    };
    Ok(Some(if path.len() == 1 {
        FocusTarget::Component { index: path[0] }
    } else {
        FocusTarget::Path {
            path: kiana_tui::tui::FocusPath::new(&path).unwrap_or_default(),
        }
    }))
}

thread_local! {
    /// 最近构造过代理的宿主 TUI（`Weak`，不延长生命周期）。
    ///
    /// `overlayHandle.unfocus({ target })` 的句柄可能来自宿主 `onHandle` 回调（只有
    /// `Box<dyn OverlayHandle>`，没有 TUI 引用）；解析组件对象为 [`FocusTarget`] 时需要
    /// 当前 TUI。单线程运行时只有一个活动 TUI，这里按“最近一个”回退。
    static LAST_TUI: RefCell<Option<std::sync::Weak<dyn Tui>>> = const { RefCell::new(None) };
}

/// 最近的宿主 TUI（见 [`LAST_TUI`]）。
fn last_tui() -> Option<Arc<dyn Tui>> {
    LAST_TUI
        .try_with(|slot| slot.borrow().as_ref().and_then(std::sync::Weak::upgrade))
        .ok()
        .flatten()
}

/// 子树里是否包含挂载在 `address` 的组件（备用屏把子项包进共享代理）。
fn subtree_contains(component: &dyn Component, address: usize) -> bool {
    if component as *const dyn Component as *const () as usize == address {
        return true;
    }
    let mut found = false;
    component.for_each_container_child(&mut |child| {
        if subtree_contains(child, address) {
            found = true;
            false
        } else {
            true
        }
    });
    found
}

/// 把子树里的 [`JsComponent`] 标记为“已摘除”（释放引用所有权，但**不**调用 JS `dispose()`）。
fn detach_js_components(component: &mut (dyn Component + 'static)) {
    if let Some(js) = component
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<JsComponent>())
    {
        js.detach();
        return;
    }
    component.for_each_container_child_mut(&mut |child| {
        detach_js_components(child);
        true
    });
}

/// TS `containsComponent(root, target)`：JS `Container.children` 递归（`__kiana_contains_component`）。
fn contains_component_js<'js>(
    ctx: &Ctx<'js>,
    root: &Object<'js>,
    target: &Object<'js>,
) -> EngineResult<bool> {
    let helper: Option<Function> = ctx.globals().get("__kiana_contains_component")?;
    match helper {
        Some(helper) => Ok(helper.call((root.clone(), target.clone()))?),
        None => Ok(false),
    }
}

/// `showOverlay` 的 `OverlayOptions` 解析。
///
/// `width`/`maxHeight`/`row`/`col` 走 [`parse_size_value`]（数字/数字串/`N%`）；未知
/// `anchor` 字符串抛 JS 异常；`margin` 反序列化失败时静默留 `None`；`visible` 回调抛错或
/// 没有引擎时按 `true` 处理（overlay 默认可见）。
fn overlay_options_from_js<'js>(
    ctx: &Ctx<'js>,
    object: Option<&Object<'js>>,
) -> EngineResult<OverlayOptions> {
    let mut options = OverlayOptions::default();
    let Some(object) = object else {
        return Ok(options);
    };
    options.width = parse_size_value(ctx, object, "width")?;
    options.min_width = opt_number(object, "minWidth")?;
    options.max_height = parse_size_value(ctx, object, "maxHeight")?;
    if let Some(anchor) = opt_string(object, "anchor")? {
        options.anchor = Some(
            serde_json::from_value(serde_json::Value::String(anchor.clone())).map_err(|_| {
                Exception::throw_message(ctx, &format!("Unknown overlay anchor: {anchor}"))
            })?,
        );
    }
    options.offset_x = opt_number(object, "offsetX")?;
    options.offset_y = opt_number(object, "offsetY")?;
    options.row = parse_size_value(ctx, object, "row")?;
    options.col = parse_size_value(ctx, object, "col")?;
    if let Some(margin) = object.get::<_, Option<Value>>("margin")? {
        let json = value_to_json(ctx, margin)?;
        options.margin = serde_json::from_value(json).ok();
    }
    if let Some(reference) = object_fn_ref(ctx, object, "visible")? {
        options.visible = Some(Box::new(move |width: usize, height: usize| {
            let Some(engine) = JsEngineSlot::current() else {
                return true;
            };
            with_engine_js(&engine, |ctx| -> bool {
                let value: Option<bool> = call_ref(
                    &ctx,
                    reference,
                    None,
                    &[JsArg::num(width as f64), JsArg::num(height as f64)],
                )
                .ok();
                value.unwrap_or(true)
            })
        }));
    }
    options.non_capturing = opt_bool(object, "nonCapturing")?;
    Ok(options)
}

/// `SizeValue`（数字或 `"50%"`）。
///
/// 接受 JS 数字、数字字符串与 `"50%"` 形式；字符串解析失败抛 JS 异常。`undefined`/`null`
/// 返回 `None`。非百分比字符串先 `trim` 再 `parse::<f64>()`，因此 `" 12 "` 也被接受。
fn parse_size_value<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    key: &str,
) -> EngineResult<Option<kiana_tui::tui::SizeValue>> {
    let value: Option<Value> = object.get(key)?;
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    if let Some(text) = value.as_string().and_then(|text| text.to_string().ok()) {
        let trimmed = text.trim();
        if let Some(percent) = trimmed.strip_suffix('%') {
            let number: f64 = percent.trim().parse().map_err(|_| {
                Exception::throw_message(ctx, &format!("Invalid size value: {text}"))
            })?;
            return Ok(Some(kiana_tui::tui::SizeValue::Percent(number)));
        }
        let number: f64 = trimmed
            .parse()
            .map_err(|_| Exception::throw_message(ctx, &format!("Invalid size value: {text}")))?;
        return Ok(Some(kiana_tui::tui::SizeValue::Absolute(number)));
    }
    let number: f64 = ctx.globals().get::<_, Function>("Number")?.call((value,))?;
    Ok(Some(kiana_tui::tui::SizeValue::Absolute(number)))
}

/// `showOverlay` 返回的句柄（TS `OverlayHandle`）。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "OverlayHandle")]
pub struct JsOverlayHandle {
    #[qjs(skip_trace)]
    inner: RefCell<Option<Box<dyn OverlayHandle>>>,
    /// `unfocus({ target })` 需要把 JS 组件对象解析成 [`FocusTarget`]（见
    /// `resolve_component_focus_target`）。`showOverlay` 直接持有宿主 TUI；宿主
    /// `onHandle` 回调没有 TUI 引用，解析时回退到 `last_tui`。
    #[qjs(skip_trace)]
    tui: Option<Arc<dyn Tui>>,
}

impl JsOverlayHandle {
    /// 在持有的句柄上执行操作。
    fn with_handle(&self, f: impl FnOnce(&mut dyn OverlayHandle)) {
        if let Some(handle) = self.inner.borrow_mut().as_mut() {
            f(handle.as_mut());
        }
    }

    /// 把 `unfocus` 的 `target` 解析成 [`FocusTarget`]。
    ///
    /// `undefined`/`null`/非对象/找不到 TUI 时都返回 `None`；对句柄而言这就是
    /// `target: undefined`（清空焦点）。
    fn resolve_unfocus_target<'js>(
        &self,
        ctx: &Ctx<'js>,
        target: Value<'js>,
    ) -> EngineResult<Option<FocusTarget>> {
        if target.is_undefined() || target.is_null() {
            return Ok(None);
        }
        let Some(object) = target.as_object() else {
            return Ok(None);
        };
        let tui = match &self.tui {
            Some(tui) => Some(Arc::clone(tui)),
            None => last_tui(),
        };
        let Some(tui) = tui else {
            return Ok(None);
        };
        resolve_component_focus_target(&tui, ctx, object)
    }

    /// 句柄改完焦点后立即把 `focused` 标志刷到组件（TS 的 `handle.focus()`/`unfocus()`
    /// 直接调用 `setFocus`，组件属性同步可见；Rust 的 `Box<dyn OverlayHandle>` 只置脏位）。
    ///
    /// `set_focus(当前目标)` 是 `Tui` 门面上唯一会调用 `TuiBase::sync_focus_flags` 且
    /// 不产生额外副作用（不渲染、不写终端）的操作。
    fn sync_focus_flags(&self) {
        let tui = match &self.tui {
            Some(tui) => Some(Arc::clone(tui)),
            None => last_tui(),
        };
        let Some(tui) = tui else {
            return;
        };
        with_tui_mut(&tui, |tui| {
            let target = tui.focus();
            tui.set_focus(target);
        });
    }
}

#[rquickjs::methods]
impl JsOverlayHandle {
    /// `new OverlayHandle()`（内部使用；扩展不会直接构造）。
    #[qjs(constructor)]
    fn new() -> EngineResult<Self> {
        Ok(JsOverlayHandle {
            inner: RefCell::new(None),
            tui: None,
        })
    }

    /// `hide()`。
    #[qjs(rename = "hide")]
    fn hide(&self, ctx: Ctx<'_>) {
        // `sync_focus_flags` 会调用 JS 组件的 `focused` 访问器：必须复用当前事务（§2.2）。
        let _active = ActiveCtxGuard::enter(&ctx);
        self.with_handle(|handle| handle.hide());
        self.sync_focus_flags();
    }

    /// `setHidden(hidden)`。
    #[qjs(rename = "setHidden")]
    fn set_hidden(&self, ctx: Ctx<'_>, hidden: bool) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.with_handle(|handle| handle.set_hidden(hidden));
        self.sync_focus_flags();
    }

    /// `isHidden()`。
    #[qjs(rename = "isHidden")]
    fn is_hidden(&self) -> bool {
        self.inner
            .borrow()
            .as_ref()
            .map(|handle| handle.is_hidden())
            .unwrap_or(false)
    }

    /// `focus()`。
    #[qjs(rename = "focus")]
    fn focus(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.with_handle(|handle| handle.focus());
        self.sync_focus_flags();
    }

    /// `unfocus(options?)`。
    ///
    /// TS 区分两种调用（`tui.ts:812-843`）：
    /// - `unfocus()`：`unfocusOptions` 为 `undefined`，恢复上一个可见 overlay / `preFocus`；
    /// - `unfocus({ target })`：聚焦显式目标（`undefined` 目标 = 清空焦点）。
    #[qjs(rename = "unfocus")]
    fn unfocus<'js>(&self, ctx: Ctx<'js>, options: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let value = opt_value(&options);
        let truthy = value.as_ref().is_some_and(js_truthy);
        if !truthy {
            // `unfocus()` / `unfocus(undefined)` / `unfocus(null)` / falsy 值：与 TS 的
            // `unfocusOptions === undefined` 分支相同（恢复 overlay 焦点）。
            self.with_handle(|handle| handle.unfocus(None));
            self.sync_focus_flags();
            return Ok(());
        }
        let mut parsed = kiana_tui::tui::OverlayUnfocusOptions::default();
        if let Some(object) = value.as_ref().and_then(|value| value.as_object()).cloned()
            && let Some(target) = object.get::<_, Option<Value>>("target")?
        {
            parsed.target = self.resolve_unfocus_target(&ctx, target)?;
        }
        self.with_handle(|handle| handle.unfocus(Some(parsed)));
        self.sync_focus_flags();
        Ok(())
    }

    /// `isFocused()`。
    #[qjs(rename = "isFocused")]
    fn is_focused(&self) -> bool {
        self.inner
            .borrow()
            .as_ref()
            .map(|handle| handle.is_focused())
            .unwrap_or(false)
    }

    /// `getBounds()`。
    #[qjs(rename = "getBounds")]
    fn get_bounds<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let bounds = self
            .inner
            .borrow()
            .as_ref()
            .and_then(|handle| handle.bounds());
        match bounds {
            Some(bounds) => json_to_value(
                &ctx,
                &serde_json::json!({
                    "row": bounds.row,
                    "col": bounds.col,
                    "width": bounds.width,
                    "height": bounds.height,
                }),
            ),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }
}

// ===========================================================================
// KeybindingsManager（编辑器工厂收到的 `keybindings` 参数）
// ===========================================================================

/// 应用键位管理器（TS `KeybindingsManager`）。
///
/// 由编辑器工厂的第三个参数提供实例（原生构造器只能造出空壳）。`getUserBindings()` 把每个
/// 绑定的归一化键列表表示成 `string`（恰好一个键）或 `string[]`（多个/空列表），与 TS 的
/// `KeyId | KeyId[]` 形状一致。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "KeybindingsManager")]
pub struct JsKeybindings {
    #[qjs(skip_trace)]
    inner: Option<Arc<KeybindingsManager>>,
}

impl JsKeybindings {
    /// 从 Rust 管理器构造。
    pub fn from_manager(manager: Arc<KeybindingsManager>) -> Self {
        JsKeybindings {
            inner: Some(manager),
        }
    }
}

#[rquickjs::methods]
impl JsKeybindings {
    /// 原生构造器不对外开放；扩展从编辑器工厂的参数拿到实例。
    #[qjs(constructor)]
    fn new() -> EngineResult<Self> {
        Ok(JsKeybindings { inner: None })
    }

    /// `matches(data, keybinding)`。
    #[qjs(rename = "matches")]
    fn matches(&self, data: String, keybinding: String) -> bool {
        self.inner
            .as_ref()
            .map(|manager| manager.matches(&data, &keybinding))
            .unwrap_or(false)
    }

    /// `getKeys(keybinding)`。
    #[qjs(rename = "getKeys")]
    fn get_keys<'js>(&self, ctx: Ctx<'js>, keybinding: String) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let keys = self
            .inner
            .as_ref()
            .map(|manager| manager.get_keys(&keybinding))
            .unwrap_or_default();
        strings_to_js(&ctx, &keys)
    }

    /// `getDefinition(keybinding)`：原样返回定义条目 `{ defaultKeys, description? }`
    /// （`keybindings.ts:63-66、282-284`；`defaultKeys` 保持 `KeyId | KeyId[]` 的原始形状）。
    #[qjs(rename = "getDefinition")]
    fn get_definition<'js>(&self, ctx: Ctx<'js>, keybinding: String) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(definition) = self
            .inner
            .as_ref()
            .and_then(|manager| manager.get_definition(&keybinding))
        else {
            return Ok(Value::new_undefined(ctx.clone()));
        };
        let object = Object::new(ctx.clone())?;
        let default_keys = match &definition.default_keys {
            kiana_config::keybindings::KeyIdList::One(key) => {
                rquickjs::String::from_str(ctx.clone(), key)?.into_value()
            }
            kiana_config::keybindings::KeyIdList::Many(keys) => {
                strings_to_js(&ctx, keys)?.into_value()
            }
        };
        object.set("defaultKeys", default_keys)?;
        if let Some(description) = definition.description.as_deref() {
            object.set(
                "description",
                rquickjs::String::from_str(ctx.clone(), description)?,
            )?;
        }
        Ok(object.into_value())
    }

    /// `getUserBindings()`。
    #[qjs(rename = "getUserBindings")]
    fn get_user_bindings<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let bindings = self
            .inner
            .as_ref()
            .map(|manager| manager.get_user_bindings())
            .unwrap_or_default();
        let mut json = serde_json::Map::new();
        for (name, keys) in bindings.iter() {
            let keys = keys.normalize();
            let value = if keys.len() == 1 {
                serde_json::Value::String(keys[0].clone())
            } else {
                serde_json::Value::Array(keys.into_iter().map(serde_json::Value::String).collect())
            };
            json.insert(name.clone(), value);
        }
        json_to_value(&ctx, &serde_json::Value::Object(json))
    }
}

// ===========================================================================
// ReadonlyFooterDataProvider
// ===========================================================================

/// 实时的只读状态表（TS `ReadonlyMap<string, string>`，`footer-data-provider.ts:134-137`）。
///
/// 每次成员调用都重新读取 provider 的 `IndexMap`，因此 `setStatus()` 之后同一个对象立即
/// 反映新值；JS 侧由 `__kiana_make_status_map` 补上 `Map` 风格的 `forEach`/`Symbol.iterator`。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "ExtensionStatusMap")]
pub struct JsFooterStatuses {
    #[qjs(skip_trace)]
    provider: Option<Arc<dyn kiana_ext_api::ui::ReadonlyFooterDataProvider>>,
}

impl JsFooterStatuses {
    fn from_provider(provider: Arc<dyn kiana_ext_api::ui::ReadonlyFooterDataProvider>) -> Self {
        JsFooterStatuses {
            provider: Some(provider),
        }
    }

    /// provider 当前的 `(key, value)` 快照：每次成员调用都重新读取，顺序沿用
    /// `get_extension_statuses()` 的迭代顺序；`keys`/`values`/`entries` 都基于它。
    fn snapshot(&self) -> Vec<(String, String)> {
        self.provider
            .as_ref()
            .map(|provider| {
                provider
                    .get_extension_statuses()
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[rquickjs::methods]
impl JsFooterStatuses {
    /// 原生构造器不对外开放。
    #[qjs(constructor)]
    fn new() -> EngineResult<Self> {
        Ok(JsFooterStatuses { provider: None })
    }

    /// `size`（实时）。
    #[qjs(get, rename = "size")]
    fn size(&self) -> f64 {
        self.snapshot().len() as f64
    }

    /// `get(key)`：缺失返回 `undefined`。
    #[qjs(rename = "get")]
    fn get<'js>(&self, ctx: Ctx<'js>, key: String) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match self
            .snapshot()
            .into_iter()
            .find(|(candidate, _)| *candidate == key)
        {
            Some((_, value)) => Ok(rquickjs::String::from_str(ctx.clone(), &value)?.into_value()),
            None => Ok(Value::new_undefined(ctx)),
        }
    }

    /// `has(key)`。
    #[qjs(rename = "has")]
    fn has(&self, key: String) -> bool {
        self.snapshot()
            .iter()
            .any(|(candidate, _)| *candidate == key)
    }

    /// `keys()`：按插入序返回键数组（JS 包装层转成迭代器）。
    #[qjs(rename = "keys")]
    fn keys<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Array<'js>> {
        let array = Array::new(ctx.clone())?;
        for (index, (key, _)) in self.snapshot().into_iter().enumerate() {
            array.set(index, rquickjs::String::from_str(ctx.clone(), &key)?)?;
        }
        Ok(array)
    }

    /// `values()`：按插入序返回值数组。
    #[qjs(rename = "values")]
    fn values<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Array<'js>> {
        let array = Array::new(ctx.clone())?;
        for (index, (_, value)) in self.snapshot().into_iter().enumerate() {
            array.set(index, rquickjs::String::from_str(ctx.clone(), &value)?)?;
        }
        Ok(array)
    }

    /// `entries()`：按插入序返回 `[key, value]` 数组的数组。
    #[qjs(rename = "entries")]
    fn entries<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Array<'js>> {
        let array = Array::new(ctx.clone())?;
        for (index, (key, value)) in self.snapshot().into_iter().enumerate() {
            let pair = Array::new(ctx.clone())?;
            pair.set(0, rquickjs::String::from_str(ctx.clone(), &key)?)?;
            pair.set(1, rquickjs::String::from_str(ctx.clone(), &value)?)?;
            array.set(index, pair)?;
        }
        Ok(array)
    }
}

/// 只读 footer 数据（TS `ReadonlyFooterDataProvider`）。
///
/// `getExtensionStatuses()` 每次调用返回新的 JS 包装对象，但底层是 provider 的实时
/// `IndexMap`：`setStatus()` 之后同一个包装对象立刻看到新值（见 [`JsFooterStatuses`]）。
/// `onBranchChange` 返回的函数只解除订阅；回调的引用 id 在订阅期间保留，本文件没有在
/// 退订时释放它的路径。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "FooterDataProvider")]
pub struct JsFooterData {
    #[qjs(skip_trace)]
    inner: Option<Arc<dyn kiana_ext_api::ui::ReadonlyFooterDataProvider>>,
}

impl JsFooterData {
    /// 从 trait 对象构造。
    pub fn from_provider(provider: Arc<dyn kiana_ext_api::ui::ReadonlyFooterDataProvider>) -> Self {
        JsFooterData {
            inner: Some(provider),
        }
    }
}

#[rquickjs::methods]
impl JsFooterData {
    /// 原生构造器不对外开放。
    #[qjs(constructor)]
    fn new() -> EngineResult<Self> {
        Ok(JsFooterData { inner: None })
    }

    /// `getGitBranch()`：不在仓库中时为 `null`（`footer-data-provider.ts:125-131`）。
    #[qjs(rename = "getGitBranch")]
    fn get_git_branch<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match self
            .inner
            .as_ref()
            .and_then(|provider| provider.get_git_branch())
        {
            Some(branch) => Ok(rquickjs::String::from_str(ctx.clone(), &branch)?.into_value()),
            None => Ok(Value::new_null(ctx.clone())),
        }
    }

    /// `getExtensionStatuses()`：**实时** `ReadonlyMap`（`footer-data-provider.ts:134-137`）。
    #[qjs(rename = "getExtensionStatuses")]
    fn get_extension_statuses<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(provider) = self.inner.as_ref() else {
            return Ok(Value::new_undefined(ctx.clone()));
        };
        let instance = Class::instance(
            ctx.clone(),
            JsFooterStatuses::from_provider(Arc::clone(provider)),
        )?;
        let value = instance.into_value();
        let helper: Option<Function> = ctx.globals().get("__kiana_make_status_map")?;
        match helper {
            Some(helper) => Ok(helper.call((value,))?),
            None => Ok(value),
        }
    }

    /// `getAvailableProviderCount()`。
    #[qjs(rename = "getAvailableProviderCount")]
    fn get_available_provider_count(&self) -> f64 {
        self.inner
            .as_ref()
            .map(|provider| provider.get_available_provider_count())
            .unwrap_or(0) as f64
    }

    /// `onBranchChange(callback)`：返回退订函数。
    #[qjs(rename = "onBranchChange")]
    fn on_branch_change<'js>(
        &self,
        ctx: Ctx<'js>,
        callback: Value<'js>,
    ) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(reference) = store_function_ref(&ctx, callback)? else {
            return Ok(Value::new_undefined(ctx.clone()));
        };
        let Some(provider) = self.inner.as_ref() else {
            return Ok(Value::new_undefined(ctx.clone()));
        };
        let unsubscribe = provider.on_branch_change(Arc::new(move || {
            let Some(engine) = JsEngineSlot::current() else {
                return;
            };
            let _ = with_engine_js(&engine, |ctx| -> EngineResult<()> {
                let _ = call_ref::<Value>(&ctx, reference, None, &[]);
                Ok(())
            });
        }));
        let function = Function::new(ctx, move || {
            unsubscribe();
        })?;
        Ok(function.into_value())
    }
}

// ===========================================================================
// BorderedLoader（coding-agent 侧的组合组件）
// ===========================================================================

/// 把 JS 主题对象的方法包装成 Rust 着色函数。
fn theme_color_fn(
    theme_reference: u64,
    color: &'static str,
) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
    Arc::new(move |text: &str| {
        let Some(engine) = JsEngineSlot::current() else {
            return text.to_owned();
        };
        let text_owned = text.to_owned();
        with_engine_js(&engine, |ctx| -> Option<String> {
            let value = load_ref(&ctx, theme_reference).ok()?;
            let object = value.as_object()?;
            let result: Option<rquickjs::String> = call_method(
                &ctx,
                object,
                "fg",
                &[JsArg::str(color), JsArg::String(text_owned.clone())],
            )
            .ok()?;
            result.and_then(|text| text.to_string().ok())
        })
        .unwrap_or_else(|| text.to_owned())
    })
}

/// `formatKeyText`（`keybinding-hints.ts`；没有 capitalize 时把 `alt` 在 macOS 上显示为 `option`）。
fn format_key_text(key: &str) -> String {
    let darwin = cfg!(target_os = "macos");
    key.split('/')
        .map(|chord| {
            chord
                .split('+')
                .map(|part| {
                    if darwin && part.eq_ignore_ascii_case("alt") {
                        "option".to_owned()
                    } else {
                        part.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// 简单的分隔线组件（`DynamicBorder` 的 Rust 版本，供 `BorderedLoader` 组合）。
struct BorderLine {
    color: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

impl Component for BorderLine {
    fn render(&mut self, width: usize) -> Vec<String> {
        vec![(self.color)(&"─".repeat(width.max(1)))]
    }
}

/// 加载器种类（`BorderedLoader` 内部）。
enum BorderedLoaderInner {
    Cancellable(CancellableLoader),
    Plain(Loader),
}

impl Component for BorderedLoaderInner {
    fn render(&mut self, width: usize) -> Vec<String> {
        match self {
            BorderedLoaderInner::Cancellable(loader) => loader.render(width),
            BorderedLoaderInner::Plain(loader) => loader.render(width),
        }
    }

    fn handle_input(&mut self, data: &str) {
        if let BorderedLoaderInner::Cancellable(loader) = self {
            loader.handle_input(data);
        }
    }

    fn invalidate(&mut self) {
        match self {
            BorderedLoaderInner::Cancellable(loader) => Component::invalidate(loader),
            BorderedLoaderInner::Plain(loader) => Component::invalidate(loader),
        }
    }
}

/// 共享加载器代理（`Container` 独占子项，但 `BorderedLoader` 的方法要操作同一个实例）。
struct SharedLoader(Rc<RefCell<BorderedLoaderInner>>);

impl Component for SharedLoader {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.0.borrow_mut().handle_input(data);
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }

    fn handles_input(&self) -> bool {
        matches!(*self.0.borrow(), BorderedLoaderInner::Cancellable(_))
    }
}

/// `BorderedLoader`：带上下边框的加载器（TS `BorderedLoader`）。
///
/// 内部是一个固定的 `kiana_tui::tui::Container`：上边框、被 `SharedLoader` 共享的加载器、
/// 可取消时的取消提示、间隔、下边框。加载器本体放在 `Rc<RefCell<_>>` 里，容器的子项只是
/// 代理，因此 `start`/`setMessage` 等方法操作的是同一个实例。
///
/// `children`/`removeChild`/`clear` 只管理扩展通过 `addChild` 挂上的 JS 子组件；内部边框/
/// 加载器不会出现在 `children` 里（TS extends Container，内部组件也在 `children` 中，这是
/// 已知差异，见字段注释）。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "BorderedLoader")]
pub struct JsBorderedLoader {
    #[qjs(skip_trace)]
    container: RefCell<kiana_tui::tui::Container>,
    #[qjs(skip_trace)]
    loader: Rc<RefCell<BorderedLoaderInner>>,
    #[qjs(skip_trace)]
    cancellable: bool,
    #[qjs(skip_trace)]
    on_abort_ref: Rc<RefCell<Option<u64>>>,
    #[qjs(skip_trace)]
    plain_signal: RefCell<Option<u64>>,
    /// 扩展通过 `addChild` 挂上的 JS 子组件：`(引用 id, 组件堆地址)`。
    /// TS 的 `BorderedLoader extends Container`，`children`/`removeChild`/`clear` 操作的是
    /// 这个数组（内部边框/loader 也会出现在 `children` 里，见报告的已知差异）。
    #[qjs(skip_trace)]
    added: RefCell<Vec<(u64, usize)>>,
}

#[rquickjs::methods]
impl JsBorderedLoader {
    /// `new BorderedLoader(tui, theme, message, options = {})`。
    #[qjs(constructor)]
    fn new<'js>(
        ctx: Ctx<'js>,
        tui: Opt<Value<'js>>,
        theme: Opt<Value<'js>>,
        message: Opt<Value<'js>>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let tui_object = opt_value(&tui)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        let request_render = request_render_from_js(&ctx, tui_object.as_ref());
        let message = opt_arg_string(&message).unwrap_or_default();
        let mut cancellable = true;
        if let Some(options_object) = opt_value(&options)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned()
        {
            cancellable = opt_bool(&options_object, "cancellable")?.unwrap_or(true);
        }
        let theme_reference = opt_value(&theme)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned()
            .and_then(|object| store_ref(&ctx, object.into_value()).ok());
        let border_color = match theme_reference {
            Some(reference) => theme_color_fn(reference, "border"),
            None => default_border_color(),
        };
        let accent = match theme_reference {
            Some(reference) => theme_color_fn(reference, "accent"),
            None => Arc::new(|text: &str| text.to_owned()),
        };
        let muted = match theme_reference {
            Some(reference) => theme_color_fn(reference, "muted"),
            None => Arc::new(|text: &str| text.to_owned()),
        };
        let dim = match theme_reference {
            Some(reference) => theme_color_fn(reference, "dim"),
            None => Arc::new(|text: &str| text.to_owned()),
        };
        let on_abort_ref: Rc<RefCell<Option<u64>>> = Rc::new(RefCell::new(None));
        let inner = if cancellable {
            let mut loader = CancellableLoader::new(
                request_render,
                Arc::clone(&accent),
                Arc::clone(&muted),
                &message,
                None,
            );
            let slot = Rc::clone(&on_abort_ref);
            loader.on_abort = Some(Box::new(move || {
                let reference = *slot.borrow();
                if let Some(reference) = reference
                    && let Some(engine) = JsEngineSlot::current()
                {
                    let _ = with_engine_js(&engine, |ctx| -> EngineResult<()> {
                        let _ = call_ref::<Value>(&ctx, reference, None, &[]);
                        Ok(())
                    });
                }
            }));
            loader.set_timer(Box::new(JsIntervalTimer::new()));
            BorderedLoaderInner::Cancellable(loader)
        } else {
            let mut loader = Loader::new(
                request_render,
                Arc::clone(&accent),
                Arc::clone(&muted),
                &message,
                None,
            );
            install_loader_timer(&mut loader);
            BorderedLoaderInner::Plain(loader)
        };
        let loader = Rc::new(RefCell::new(inner));
        // 固定布局（与 TS 的 BorderedLoader 构造一致）：上边框 → loader → 可选取消提示
        // → 间隔 → 下边框。容器只持有 `SharedLoader` 代理，loader 本体由 `self.loader` 持有。
        let mut container = kiana_tui::tui::Container::new();
        container.add_child(Box::new(BorderLine {
            color: Arc::clone(&border_color),
        }));
        container.add_child(Box::new(SharedLoader(Rc::clone(&loader))));
        if cancellable {
            container.add_child(Box::new(Spacer::with_lines(1.0)));
            let keys = kiana_tui::keybindings::get_keybindings().get_keys("tui.select.cancel");
            let key_text = if keys.is_empty() {
                String::new()
            } else {
                format_key_text(&keys.join("/"))
            };
            let hint = format!("{}{}", (dim)(&key_text), (muted)(" cancel"));
            container.add_child(Box::new(Text::with_padding(hint, 1, 0)));
        }
        container.add_child(Box::new(Spacer::with_lines(1.0)));
        container.add_child(Box::new(BorderLine {
            color: border_color,
        }));
        Ok(JsBorderedLoader {
            container: RefCell::new(container),
            loader,
            cancellable,
            on_abort_ref,
            plain_signal: RefCell::new(None),
            added: RefCell::new(Vec::new()),
        })
    }

    /// `signal`：可取消时来自 `CancellableLoader`；否则在首次读取时创建 `AbortController`
    /// 并缓存其 `signal`（同一实例重复返回，永不 abort）。
    #[qjs(get, rename = "signal")]
    fn get_signal<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        if self.cancellable {
            let token = match &*self.loader.borrow() {
                BorderedLoaderInner::Cancellable(loader) => loader.signal(),
                BorderedLoaderInner::Plain(_) => tokio_util::sync::CancellationToken::new(),
            };
            return abort_signal_for_token(&ctx, &token);
        }
        if let Some(reference) = *self.plain_signal.borrow() {
            return load_ref(&ctx, reference);
        }
        let constructor: rquickjs::Constructor = ctx.globals().get("AbortController")?;
        let controller: Object = constructor.construct(())?;
        let signal: Value = controller.get("signal")?;
        *self.plain_signal.borrow_mut() = store_ref(&ctx, signal.clone()).ok();
        Ok(signal)
    }

    /// `onAbort`。
    #[qjs(get, rename = "onAbort")]
    fn get_on_abort<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match *self.on_abort_ref.borrow() {
            Some(reference) => load_ref(&ctx, reference),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `onAbort = fn`：仅可取消实例保存回调。非可取消时写入被静默忽略：函数仍会经
    /// `store_function_ref` 写进 `__kiana_refs`，但返回的 id 被丢弃且不释放，条目保留到
    /// 引擎销毁；之后读取 `onAbort` 仍是 `undefined`。
    #[qjs(set, rename = "onAbort")]
    fn set_on_abort<'js>(&self, ctx: Ctx<'js>, value: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let reference = match opt_value(&value) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        if self.cancellable {
            *self.on_abort_ref.borrow_mut() = reference;
        }
        Ok(())
    }

    /// `handleInput(data)`（可取消时会调用 JS `onAbort`，必须复用当前事务）。
    #[qjs(rename = "handleInput")]
    fn handle_input(&self, ctx: Ctx<'_>, data: String) {
        let _active = ActiveCtxGuard::enter(&ctx);
        if self.cancellable {
            self.loader.borrow_mut().handle_input(&data);
        }
    }

    /// `dispose()`。
    ///
    /// 可取消的 loader 走 `dispose()`，普通 loader 走 `stop()`；两条路径都只停止动画，
    /// **不**取消令牌、也不触发 `onAbort`（取消只由 Esc 键触发，见 `handleInput`）。
    #[qjs(rename = "dispose")]
    fn dispose(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        match &mut *self.loader.borrow_mut() {
            BorderedLoaderInner::Cancellable(loader) => loader.dispose(),
            BorderedLoaderInner::Plain(loader) => loader.stop(),
        }
    }

    /// `addChild(component)`（`BorderedLoader extends Container`）。
    #[qjs(rename = "addChild")]
    fn add_child<'js>(&self, ctx: Ctx<'js>, component: Value<'js>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let child = JsBox::wrap_child(&ctx, component)?;
        let reference = child
            .as_any()
            .and_then(|any| any.downcast_ref::<JsComponent>())
            .map(JsComponent::reference)
            .unwrap_or(0);
        let address = child.as_ref() as *const dyn Component as *const () as usize;
        self.container.borrow_mut().add_child(child);
        self.added.borrow_mut().push((reference, address));
        Ok(())
    }

    /// `children`（TS `Container.children`）：通过 `addChild` 挂上的 JS 子组件。
    #[qjs(get, rename = "children")]
    fn children<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let array = Array::new(ctx.clone())?;
        for (index, (reference, _)) in self.added.borrow().iter().enumerate() {
            array.set(index, load_ref(&ctx, *reference)?)?;
        }
        Ok(array)
    }

    /// `removeChild(component)`：按对象身份移除（`tui.ts:370-377`，不调用 `dispose()`）。
    #[qjs(rename = "removeChild")]
    fn remove_child<'js>(&self, ctx: Ctx<'js>, component: Value<'js>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(object) = component.as_object().cloned() else {
            return Ok(());
        };
        let references: Vec<u64> = self
            .added
            .borrow()
            .iter()
            .map(|(reference, _)| *reference)
            .collect();
        let mut matched = None;
        for reference in references {
            if reference == 0 {
                continue;
            }
            if load_ref(&ctx, reference)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .is_some_and(|candidate| candidate == object)
            {
                matched = Some(reference);
                break;
            }
        }
        let Some(reference) = matched else {
            return Ok(());
        };
        let entry = {
            let mut added = self.added.borrow_mut();
            added
                .iter()
                .position(|(candidate, _)| *candidate == reference)
                .map(|index| added.remove(index))
        };
        if let Some((_, address)) = entry {
            let mut container = self.container.borrow_mut();
            let index = container.children.iter().position(|child| {
                child.as_ref() as *const dyn Component as *const () as usize == address
            });
            if let Some(index) = index {
                let mut removed = container.remove_child_at(index).expect("index checked");
                if let Some(js) = removed
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<JsComponent>())
                {
                    js.detach();
                }
                drop(removed);
            }
        }
        drop_ref(&ctx, reference);
        Ok(())
    }

    /// `clear()`：清空所有子项（TS `Container.clear`，不调用 `dispose()`）。
    ///
    /// 这也会移除内部的上/下边框、加载器代理与间隔（它们就在同一个容器里），因此清空后
    /// 只剩一个空组件；`loader` 实例仍由 `self.loader` 持有，之后 `start`/`setMessage`
    /// 不再有可见效果。
    #[qjs(rename = "clear")]
    fn clear(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        let references: Vec<u64> = self
            .added
            .borrow_mut()
            .drain(..)
            .map(|(reference, _)| reference)
            .collect();
        self.container.borrow_mut().clear();
        for reference in references {
            drop_ref(&ctx, reference);
        }
    }

    /// `handleMouse(event)`（`BorderedLoader extends Container`）。
    #[qjs(rename = "handleMouse")]
    fn handle_mouse<'js>(&self, ctx: Ctx<'js>, event: Value<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let event = mouse_event_from_js(&ctx, event)?;
        let result = Component::handle_mouse(&mut *self.container.borrow_mut(), &event);
        mouse_result_to_js(&ctx, result)
    }

    /// `invalidate()`。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self, ctx: Ctx<'_>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.container.borrow_mut().invalidate();
    }

    /// `render(width)`。
    #[qjs(rename = "render")]
    fn render<'js>(&self, ctx: Ctx<'js>, width: Opt<Value<'js>>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let lines = Component::render(&mut *self.container.borrow_mut(), width);
        lines_to_js(&ctx, lines)
    }
}

// ===========================================================================
// Editor
// ===========================================================================

/// `Editor` 的 JS 回调槽。
#[derive(Clone)]
struct EditorCallbacks {
    submit: Rc<RefCell<Option<u64>>>,
    change: Rc<RefCell<Option<u64>>>,
    border: Rc<RefCell<Option<u64>>>,
    this: Rc<std::cell::Cell<u64>>,
    pending_submit: Rc<RefCell<Option<String>>>,
    pending_change: Rc<RefCell<Option<String>>>,
}

impl EditorCallbacks {
    fn new() -> Self {
        EditorCallbacks {
            submit: Rc::new(RefCell::new(None)),
            change: Rc::new(RefCell::new(None)),
            border: Rc::new(RefCell::new(None)),
            this: Rc::new(std::cell::Cell::new(0)),
            pending_submit: Rc::new(RefCell::new(None)),
            pending_change: Rc::new(RefCell::new(None)),
        }
    }

    /// 记住 `this`（同一个编辑器实例终生不变）。
    fn remember_this<'js>(&self, ctx: &Ctx<'js>, this: &Value<'js>) {
        if self.this.get() != 0 {
            return;
        }
        if let Some(object) = this.as_object()
            && let Ok(reference) = store_ref(ctx, object.clone().into_value())
        {
            self.this.set(reference);
        }
    }

    /// 派发 `onChange` / `onSubmit`（在 `Editor` 借用释放之后）。
    fn drain(&self, ctx: &Ctx<'_>) {
        let receiver = if self.this.get() == 0 {
            None
        } else {
            load_ref(ctx, self.this.get())
                .ok()
                .and_then(|value| value.as_object().cloned())
        };
        if let Some(text) = self.pending_change.borrow_mut().take() {
            let reference = *self.change.borrow();
            if let Some(reference) = reference {
                let _ = call_ref::<Value>(
                    ctx,
                    reference,
                    receiver.clone().map(|object| object.into_value()),
                    &[JsArg::String(text)],
                );
            }
        }
        if let Some(text) = self.pending_submit.borrow_mut().take() {
            let reference = *self.submit.borrow();
            if let Some(reference) = reference {
                let _ = call_ref::<Value>(
                    ctx,
                    reference,
                    receiver.map(|object| object.into_value()),
                    &[JsArg::String(text)],
                );
            }
        }
    }
}

/// 编辑器边框的哨兵行（Rust 渲染时替换成 JS `renderTopBorder`/`renderBottomBorder` 的结果）。
const TOP_BORDER_SENTINEL: &str = "\u{0}kiana:top-border\u{0}";
const BOTTOM_BORDER_SENTINEL: &str = "\u{0}kiana:bottom-border\u{0}";

/// `@earendil-works/kiana-tui` 的 `Editor`。
///
/// 原生类的 `render` 是**不可变**方法 + 内部 `RefCell<Option<Editor>>`：渲染期间 JS 子类会
/// 覆写 `renderTopBorder` 并回调本类的方法，宏为 `&self` 使用共享借用，重入不会 panic。
///
/// 边框覆写：构造时给 `kiana_tui::components::editor::Editor` 注入两个边框渲染器，它们只
/// 记录 `hidden` 行数并返回哨兵字符串；`render` 结束后逐行把哨兵换成 JS 覆写
/// （`this.renderTopBorder`/`renderBottomBorder`）的结果，方法缺失或返回非字符串时回退到
/// `base_top_border`/`base_bottom_border`。这样 JS 子类可以用 `super` 调原生边框，而 Rust
/// 侧不需要知道 JS 的类结构。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "Editor")]
pub struct JsEditor {
    #[qjs(skip_trace)]
    editor: RefCell<Option<Editor>>,
    #[qjs(skip_trace)]
    top_hidden: Arc<Mutex<Option<usize>>>,
    #[qjs(skip_trace)]
    bottom_hidden: Arc<Mutex<Option<usize>>>,
    #[qjs(skip_trace)]
    callbacks: EditorCallbacks,
}

impl JsEditor {
    /// 取出编辑器执行一次操作（操作期间 JS 可重入其它原生方法）。
    ///
    /// 期间 `self.editor` 是 `None`：如果 JS 重入同一个编辑器的原生方法，读方法返回默认值
    /// （如 `getText()` → `""`），写方法静默不生效。这是为了在 `f` 可能回调 JS 时避免
    /// `RefCell` 双重可变借用 panic。
    fn with_editor<R>(&self, f: impl FnOnce(&mut Editor) -> R) -> Option<R> {
        let mut editor = self.editor.borrow_mut().take()?;
        let result = f(&mut editor);
        *self.editor.borrow_mut() = Some(editor);
        Some(result)
    }

    /// 读取编辑器（不可变）。
    fn read_editor<R>(&self, f: impl FnOnce(&Editor) -> R) -> Option<R> {
        self.editor.borrow().as_ref().map(f)
    }

    /// 通过 `This` 调用 JS 的边框覆写；缺失时回退到原生边框。
    fn dispatch_border(
        &self,
        _ctx: &Ctx<'_>,
        this: &Value<'_>,
        method: &str,
        width: usize,
        hidden: usize,
    ) -> EngineResult<String> {
        let fallback = || {
            self.read_editor(|editor| {
                if method == "renderTopBorder" {
                    editor.base_top_border(width, hidden)
                } else {
                    editor.base_bottom_border(width, hidden)
                }
            })
            .unwrap_or_default()
        };
        let Some(object) = this.as_object() else {
            return Ok(fallback());
        };
        let member: Value = object.get(method)?;
        let Some(function) = member.as_function() else {
            return Ok(fallback());
        };
        let result: Value = function.call((This(object.clone()), width as f64, hidden as f64))?;
        Ok(js_string(&result).unwrap_or_else(|_| fallback()))
    }
}

#[rquickjs::methods]
impl JsEditor {
    /// `new Editor(tui, theme, options = {})`。
    ///
    /// `tui` 是原生 `TUIProxy` 时优先用它的宿主 TUI（重绘与真实终端）；否则回退到对象上的
    /// `requestRender` 与 `BridgeTerminal`（线程局部尺寸，默认 24x80）。`theme.borderColor`
    /// 只保存为 JS 覆写的引用，原生主题的边框着色被换成恒等函数，避免同一结果着色两次。
    #[qjs(constructor)]
    fn new<'js>(
        ctx: Ctx<'js>,
        tui: Opt<Value<'js>>,
        theme: Opt<Value<'js>>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Self> {
        let _active = ActiveCtxGuard::enter(&ctx);

        let tui_value = opt_value(&tui);
        let native_tui = tui_value
            .as_ref()
            .and_then(|value| Class::<JsTui>::from_value(value).ok())
            .and_then(|class| class.borrow().tui().cloned());
        let tui_object = tui_value
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        let request_render: Rc<dyn Fn()> = match native_tui.clone() {
            Some(tui) => Rc::new(move || {
                with_tui_mut(&tui, |tui| tui.request_render(false));
            }),
            None => {
                request_render_from_js(&ctx, tui_object.as_ref()).unwrap_or_else(|| Rc::new(|| {}))
            }
        };
        let terminal: Rc<RefCell<Box<dyn kiana_tui::Terminal>>> = native_tui
            .as_ref()
            .and_then(|tui| host_terminal(tui))
            .unwrap_or_else(|| Rc::new(RefCell::new(Box::new(BridgeTerminal))));
        let editor_tui = kiana_tui::components::editor::EditorTui::new(terminal, request_render);
        let theme_object = opt_value(&theme)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned();
        let (mut editor_theme, border_reference) =
            editor_theme_from_object(&ctx, theme_object.as_ref())?;
        // 边框由 JS 覆写负责着色（`base_top_border` 只给未着色文本），原生主题用恒等函数，
        // 避免同一结果被着色两次。
        editor_theme.border_color = Arc::new(|text: &str| text.to_string());
        let callbacks = EditorCallbacks::new();
        if let Some(reference) = border_reference {
            *callbacks.border.borrow_mut() = Some(reference);
        } else {
            let function = Function::new(ctx.clone(), |text: String| -> String { text })?;
            *callbacks.border.borrow_mut() = Some(store_ref(&ctx, function.into_value())?);
        }
        let mut editor_options = EditorOptions::default();
        if let Some(options_object) = opt_value(&options)
            .as_ref()
            .and_then(|value| value.as_object())
            .cloned()
        {
            editor_options.padding_x = opt_number(&options_object, "paddingX")?;
            editor_options.autocomplete_max_visible =
                opt_number(&options_object, "autocompleteMaxVisible")?;
        }
        let mut editor = Editor::new(editor_tui, editor_theme, editor_options);
        editor.set_timer(Box::new(JsIntervalTimer::new()));
        let submit_slot = Rc::clone(&callbacks.pending_submit);
        editor.on_submit = Some(Box::new(move |text: &str| {
            *submit_slot.borrow_mut() = Some(text.to_owned());
        }));
        let change_slot = Rc::clone(&callbacks.pending_change);
        editor.on_change = Some(Box::new(move |text: &str| {
            *change_slot.borrow_mut() = Some(text.to_owned());
        }));
        let top_hidden: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
        let bottom_hidden: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
        let top_slot = Arc::clone(&top_hidden);
        editor.set_top_border_renderer(Some(Arc::new(move |_width, hidden, _color, _base| {
            *top_slot.lock().unwrap_or_else(|error| error.into_inner()) = Some(hidden);
            TOP_BORDER_SENTINEL.to_owned()
        })));
        let bottom_slot = Arc::clone(&bottom_hidden);
        editor.set_bottom_border_renderer(Some(Arc::new(move |_width, hidden, _color, _base| {
            *bottom_slot
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(hidden);
            BOTTOM_BORDER_SENTINEL.to_owned()
        })));
        Ok(JsEditor {
            editor: RefCell::new(Some(editor)),
            top_hidden,
            bottom_hidden,
            callbacks,
        })
    }

    /// `render(width)`：渲染后把哨兵行换成 JS 覆写的边框。
    #[qjs(rename = "render")]
    fn render<'js>(
        &self,
        ctx: Ctx<'js>,
        this: This<Value<'js>>,
        width: Opt<Value<'js>>,
    ) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.callbacks.remember_this(&ctx, &this.0);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        // 每次渲染清空边框隐藏行数：注入的渲染器会在 `Component::render` 内部重新写入。
        *self
            .top_hidden
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        *self
            .bottom_hidden
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        let Some(lines) = self.with_editor(|editor| Component::render(editor, width)) else {
            return Array::new(ctx);
        };
        let top = *self
            .top_hidden
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let bottom = *self
            .bottom_hidden
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut result = Vec::with_capacity(lines.len());
        // 哨兵行换成 JS 覆写的边框；其它行原样返回。
        for line in lines {
            if line == TOP_BORDER_SENTINEL {
                result.push(self.dispatch_border(
                    &ctx,
                    &this.0,
                    "renderTopBorder",
                    width,
                    top.unwrap_or(0),
                )?);
            } else if line == BOTTOM_BORDER_SENTINEL {
                result.push(self.dispatch_border(
                    &ctx,
                    &this.0,
                    "renderBottomBorder",
                    width,
                    bottom.unwrap_or(0),
                )?);
            } else {
                result.push(line);
            }
        }
        lines_to_js(&ctx, result)
    }

    /// `__topBorderBase(width, hiddenLineCount)`：未着色的上边框（JS 基类实现调用）。
    #[qjs(rename = "__topBorderBase")]
    fn top_border_base<'js>(
        &self,
        ctx: Ctx<'js>,
        width: Opt<Value<'js>>,
        hidden: Opt<Value<'js>>,
    ) -> EngineResult<String> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let hidden = opt_arg_number(&ctx, &hidden)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        Ok(self
            .read_editor(|editor| editor.base_top_border(width, hidden))
            .unwrap_or_default())
    }

    /// `__bottomBorderBase(width, hiddenLineCount)`：未着色的下边框。
    #[qjs(rename = "__bottomBorderBase")]
    fn bottom_border_base<'js>(
        &self,
        ctx: Ctx<'js>,
        width: Opt<Value<'js>>,
        hidden: Opt<Value<'js>>,
    ) -> EngineResult<String> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let width = opt_arg_number(&ctx, &width)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        let hidden = opt_arg_number(&ctx, &hidden)
            .map(|value| floor_or(value, 0))
            .unwrap_or(0);
        Ok(self
            .read_editor(|editor| editor.base_bottom_border(width, hidden))
            .unwrap_or_default())
    }

    /// `handleInput(data)`。
    #[qjs(rename = "handleInput")]
    fn handle_input<'js>(&self, ctx: Ctx<'js>, this: This<Value<'js>>, data: String) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.callbacks.remember_this(&ctx, &this.0);
        self.with_editor(|editor| Component::handle_input(editor, &data));
        self.callbacks.drain(&ctx);
    }

    /// `getText()`。
    #[qjs(rename = "getText")]
    fn get_text(&self) -> String {
        self.read_editor(|editor| editor.get_text())
            .unwrap_or_default()
    }

    /// `getExpandedText()`。
    #[qjs(rename = "getExpandedText")]
    fn get_expanded_text(&self) -> String {
        self.read_editor(|editor| editor.get_expanded_text())
            .unwrap_or_default()
    }

    /// `setText(text)`。
    #[qjs(rename = "setText")]
    fn set_text(&self, ctx: Ctx<'_>, text: String) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.with_editor(|editor| editor.set_text(&text));
        self.callbacks.drain(&ctx);
    }

    /// `insertTextAtCursor(text)`。
    #[qjs(rename = "insertTextAtCursor")]
    fn insert_text_at_cursor(&self, ctx: Ctx<'_>, text: String) {
        let _active = ActiveCtxGuard::enter(&ctx);
        self.with_editor(|editor| editor.insert_text_at_cursor(&text));
        self.callbacks.drain(&ctx);
    }

    /// `getLines()`。
    #[qjs(rename = "getLines")]
    fn get_lines<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let lines = self
            .read_editor(|editor| editor.get_lines())
            .unwrap_or_default();
        lines_to_js(&ctx, lines)
    }

    /// `getCursor()`：返回 `{ line, col }`。
    #[qjs(rename = "getCursor")]
    fn get_cursor<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let (line, col) = self
            .read_editor(|editor| editor.get_cursor())
            .unwrap_or((0, 0));
        let object = Object::new(ctx.clone())?;
        object.set("line", line as f64)?;
        object.set("col", col as f64)?;
        Ok(object.into_value())
    }

    /// `addToHistory(text)`。
    #[qjs(rename = "addToHistory")]
    fn add_to_history(&self, text: String) {
        self.with_editor(|editor| editor.add_to_history(&text));
    }

    /// `getPaddingX()`。
    #[qjs(rename = "getPaddingX")]
    fn get_padding_x(&self) -> f64 {
        self.read_editor(|editor| editor.get_padding_x())
            .unwrap_or(0) as f64
    }

    /// `setPaddingX(paddingX)`。
    #[qjs(rename = "setPaddingX")]
    fn set_padding_x<'js>(&self, ctx: Ctx<'js>, padding: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let padding = opt_arg_number(&ctx, &padding).unwrap_or(0.0);
        self.with_editor(|editor| editor.set_padding_x(padding));
        Ok(())
    }

    /// `getAutocompleteMaxVisible()`。
    #[qjs(rename = "getAutocompleteMaxVisible")]
    fn get_autocomplete_max_visible(&self) -> f64 {
        self.read_editor(|editor| editor.get_autocomplete_max_visible())
            .unwrap_or(5) as f64
    }

    /// `setAutocompleteMaxVisible(maxVisible)`。
    #[qjs(rename = "setAutocompleteMaxVisible")]
    fn set_autocomplete_max_visible<'js>(
        &self,
        ctx: Ctx<'js>,
        max_visible: Opt<Value<'js>>,
    ) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let max_visible = opt_arg_number(&ctx, &max_visible).unwrap_or(5.0);
        self.with_editor(|editor| editor.set_autocomplete_max_visible(max_visible));
        Ok(())
    }

    /// `isShowingAutocomplete()`。
    #[qjs(rename = "isShowingAutocomplete")]
    fn is_showing_autocomplete(&self) -> bool {
        self.read_editor(|editor| editor.is_showing_autocomplete())
            .unwrap_or(false)
    }

    /// `setAutocompleteProvider(provider)`。
    ///
    /// 宿主传回来的原生 `AutocompleteProvider` 句柄直接接到 `kiana_tui::Editor`；扩展自己构造的
    /// JS provider 对象包成 `JsAutocompleteProviderWrapper`（`getSuggestions` 走 Promise 桥）。
    #[qjs(rename = "setAutocompleteProvider")]
    fn set_autocomplete_provider<'js>(&self, ctx: Ctx<'js>, provider: Opt<Value<'js>>) {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(value) = opt_value(&provider) else {
            return;
        };
        if let Ok(class) = Class::<JsAutocompleteProvider>::from_value(&value) {
            let handle = class.borrow().handle;
            let native = PROVIDER_HANDLES.with(|handles| handles.borrow().get(&handle).cloned());
            if let Some(native) = native {
                self.with_editor(|editor| {
                    editor.set_autocomplete_provider(Box::new(FallbackProvider(native)));
                });
                return;
            }
        }
        if value.as_object().is_none() {
            return;
        }
        let Ok(reference) = store_ref(&ctx, value) else {
            return;
        };
        let fallback = Arc::new(tokio::sync::Mutex::new(
            Box::new(NullAutocompleteProvider) as Box<dyn AutocompleteProvider>
        ));
        self.with_editor(|editor| {
            editor.set_autocomplete_provider(Box::new(JsAutocompleteProviderWrapper {
                reference,
                fallback,
            }));
        });
    }

    /// `handleMouse(event)`。
    #[qjs(rename = "handleMouse")]
    fn handle_mouse<'js>(&self, ctx: Ctx<'js>, event: Value<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let event = mouse_event_from_js(&ctx, event)?;
        let result = self.with_editor(|editor| Component::handle_mouse(editor, &event));
        mouse_result_to_js(&ctx, result.flatten())
    }

    /// `invalidate()`。
    #[qjs(rename = "invalidate")]
    fn invalidate(&self) {
        self.with_editor(Component::invalidate);
    }

    /// `focused`。
    #[qjs(get, rename = "focused")]
    fn get_focused(&self) -> bool {
        self.read_editor(Focusable::focused).unwrap_or(false)
    }

    /// `focused = value`。
    #[qjs(set, rename = "focused")]
    fn set_focused(&self, focused: bool) {
        self.with_editor(|editor| Focusable::set_focused(editor, focused));
    }

    /// `disableSubmit`。
    #[qjs(get, rename = "disableSubmit")]
    fn get_disable_submit(&self) -> bool {
        self.read_editor(|editor| editor.disable_submit)
            .unwrap_or(false)
    }

    /// `disableSubmit = value`。
    #[qjs(set, rename = "disableSubmit")]
    fn set_disable_submit(&self, disabled: bool) {
        self.with_editor(|editor| editor.disable_submit = disabled);
    }

    /// `borderColor`：边框着色函数。
    #[qjs(get, rename = "borderColor")]
    fn get_border_color<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match *self.callbacks.border.borrow() {
            Some(reference) => load_ref(&ctx, reference),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `borderColor = fn`。
    #[qjs(set, rename = "borderColor")]
    fn set_border_color<'js>(&self, ctx: Ctx<'js>, value: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let reference = match opt_value(&value) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        *self.callbacks.border.borrow_mut() = reference;
        Ok(())
    }

    /// `onSubmit`。
    #[qjs(get, rename = "onSubmit")]
    fn get_on_submit<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match *self.callbacks.submit.borrow() {
            Some(reference) => load_ref(&ctx, reference),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `onSubmit = fn`。
    #[qjs(set, rename = "onSubmit")]
    fn set_on_submit<'js>(&self, ctx: Ctx<'js>, value: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let reference = match opt_value(&value) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        *self.callbacks.submit.borrow_mut() = reference;
        Ok(())
    }

    /// `onChange`。
    #[qjs(get, rename = "onChange")]
    fn get_on_change<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        match *self.callbacks.change.borrow() {
            Some(reference) => load_ref(&ctx, reference),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `onChange = fn`。
    #[qjs(set, rename = "onChange")]
    fn set_on_change<'js>(&self, ctx: Ctx<'js>, value: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let reference = match opt_value(&value) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        *self.callbacks.change.borrow_mut() = reference;
        Ok(())
    }
}

// ===========================================================================
// UI 上下文桥：原生类
// ===========================================================================

/// 工厂参数里的 `&Theme` → JS **live** 主题实例。
///
/// TS 的 widget/footer/header/custom/editor 工厂收到的 `theme` 是模块 `Proxy`
/// （`interactive-mode.ts` 传的是导入的 `theme`），缓存引用必须跟随全局主题切换；
/// 参数本身只是宿主当前主题的借用，不携带身份，因此统一返回 [`live_theme_value`]。
fn theme_value_from_ref<'js>(ctx: &Ctx<'js>, _theme: &Theme) -> EngineResult<Value<'js>> {
    live_theme_value(ctx)
}

/// 默认的空组件（工厂失败时的兜底）。
struct EmptyComponent;

impl Component for EmptyComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        Vec::new()
    }

    fn handles_input(&self) -> bool {
        false
    }
}

/// 默认的空编辑器组件。
struct EmptyEditorComponent;

impl Component for EmptyEditorComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        Vec::new()
    }

    fn handles_input(&self) -> bool {
        false
    }
}

impl EditorComponent for EmptyEditorComponent {
    fn get_text(&self) -> String {
        String::new()
    }

    fn set_text(&mut self, _text: &str) {}
}

/// 会话级 UI 上下文（`ctx.ui` 的原生落点）。
///
/// `handle` 是 `runner::context` 分配的 UI 上下文 id；未知句柄解析成 `None`，此时写成员是
/// 空操作、读成员返回默认值（`getEditorText` → `""`、`getAllThemes` → `[]`、
/// `getToolsExpanded` → `false`、`theme` → `undefined`），`setTheme` 返回
/// `{ success: false, error: "UI not available" }`。`js/ui/ui.js#isNoopUi` 已在 JS 侧把
/// no-op 上下文变成 `undefined`，这里的 `None` 只是兜底。
///
/// 工厂类成员（`setFooter`/`setHeader`/`setWidget`/`setEditorComponent`）把 JS 函数存成
/// 引用 id 并在替换时释放旧引用；`Drop` 释放仍然持有的引用（`editor_factory`/footer/
/// header/widget 表）。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "ExtensionUI")]
pub struct JsUiBridge {
    #[qjs(skip_trace)]
    ui: Option<Arc<dyn ExtensionUiContext>>,
    #[qjs(skip_trace)]
    editor_factory: Rc<RefCell<Option<u64>>>,
    /// `setFooter`/`setHeader` 当前的工厂引用（替换时释放旧值，避免 `__kiana_refs` 增长）。
    #[qjs(skip_trace)]
    footer_ref: RefCell<Option<u64>>,
    #[qjs(skip_trace)]
    header_ref: RefCell<Option<u64>>,
    /// `setWidget` 工厂的引用，按 `(key, placement)` 记录（替换/清除时释放旧值）。
    #[qjs(skip_trace)]
    widget_refs: RefCell<HashMap<(String, Option<&'static str>), u64>>,
}

impl JsUiBridge {
    /// 解析 UI 上下文；句柄未知时返回 `None`。
    fn ui(&self) -> Option<&Arc<dyn ExtensionUiContext>> {
        self.ui.as_ref()
    }

    /// widget 工厂引用的槽位：`(key, placement)`。placement 归一化成 TS 的两个 Map
    /// （`extensionWidgetsAbove` / `extensionWidgetsBelow`，`interactive-mode.ts:2402-2458`）。
    ///
    /// 未知字符串与缺失值都归到 `aboveEditor`（TS 类型之外的值不做校验）。
    fn widget_slot(key: &str, placement: Option<&str>) -> (String, Option<&'static str>) {
        let label = match parse_placement(placement) {
            Some(kiana_ext_api::ui::WidgetPlacement::BelowEditor) => "belowEditor",
            _ => "aboveEditor",
        };
        (key.to_owned(), Some(label))
    }

    /// 记录（或清除）某个 widget 槽位的工厂引用，释放被替换的旧引用。
    fn store_widget_ref(&self, key: &str, placement: Option<&str>, reference: Option<u64>) {
        let slot = Self::widget_slot(key, placement);
        let previous = match reference {
            Some(reference) => self.widget_refs.borrow_mut().insert(slot, reference),
            None => self.widget_refs.borrow_mut().remove(&slot),
        };
        if let Some(previous) = previous
            && Some(previous) != reference
        {
            adapter::release_ref(previous);
        }
    }

    /// 清除某个 key 在两个 placement 上的工厂引用（`clearWidget`）。
    fn release_widget_refs(&self, key: &str) {
        let removed: Vec<u64> = {
            let mut refs = self.widget_refs.borrow_mut();
            let slots: Vec<(String, Option<&'static str>)> = refs
                .keys()
                .filter(|(slot, _)| slot == key)
                .cloned()
                .collect();
            slots
                .into_iter()
                .filter_map(|slot| refs.remove(&slot))
                .collect()
        };
        for reference in removed {
            adapter::release_ref(reference);
        }
    }
}

impl Drop for JsUiBridge {
    fn drop(&mut self) {
        if let Some(reference) = self.editor_factory.borrow_mut().take() {
            adapter::release_ref(reference);
        }
        if let Some(reference) = self.footer_ref.get_mut().take() {
            adapter::release_ref(reference);
        }
        if let Some(reference) = self.header_ref.get_mut().take() {
            adapter::release_ref(reference);
        }
        let widget_references: Vec<u64> = self
            .widget_refs
            .get_mut()
            .drain()
            .map(|(_, reference)| reference)
            .collect();
        for reference in widget_references {
            adapter::release_ref(reference);
        }
    }
}

#[rquickjs::methods]
impl JsUiBridge {
    /// `new ExtensionUI(handle)`（由 `__kiana_ctx_make_ui` 使用）。
    #[qjs(constructor)]
    fn new(handle: f64) -> EngineResult<Self> {
        let handle = if handle.is_finite() && handle >= 0.0 {
            handle as u64
        } else {
            u64::MAX
        };
        Ok(JsUiBridge {
            ui: crate::runner::context::resolve_ui_context(handle),
            editor_factory: Rc::new(RefCell::new(None)),
            footer_ref: RefCell::new(None),
            header_ref: RefCell::new(None),
            widget_refs: RefCell::new(HashMap::new()),
        })
    }

    /// `notify(message, type?)`。
    ///
    /// `type` 缺失时必须保持 `None`：RPC 的 notify 记录逐字来自调用参数
    /// （`rpc-mode.ts:152-161` 的 `notifyType: type`），缺失即键缺失。interactive 的宿主
    /// 自己按 `unwrap_or_default()` 落回 `info`。
    #[qjs(rename = "notify")]
    fn notify(&self, message: String, kind: Opt<String>) {
        if let Some(ui) = self.ui() {
            let kind = match kind.0.as_deref() {
                Some("warning") => Some(NotifyKind::Warning),
                Some("error") => Some(NotifyKind::Error),
                Some(_) => Some(NotifyKind::Info),
                None => None,
            };
            ui.notify(&message, kind);
        }
    }

    /// `onTerminalInput(handler)`：返回退订函数。
    ///
    /// 引用的释放只发生在退订函数被调用时；扩展直接丢弃退订函数时引用保留到引擎销毁
    /// （宿主仍可能持有 `handler` 并调用它）。
    #[qjs(rename = "onTerminalInput")]
    fn on_terminal_input<'js>(
        &self,
        ctx: Ctx<'js>,
        handler: Value<'js>,
    ) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(reference) = store_function_ref(&ctx, handler)? else {
            return Ok(Value::new_undefined(ctx.clone()));
        };
        let Some(ui) = self.ui() else {
            adapter::release_ref(reference);
            return Ok(Value::new_undefined(ctx.clone()));
        };
        let handler: TerminalInputHandler = Arc::new(move |data: &str| {
            let engine = JsEngineSlot::current()?;
            let data = data.to_owned();
            with_engine_js(&engine, |ctx| -> Option<TerminalInputResult> {
                let value: Value =
                    call_ref(&ctx, reference, None, &[JsArg::String(data.clone())]).ok()?;
                if !value.is_object() {
                    return None;
                }
                let object = value.as_object()?;
                Some(TerminalInputResult {
                    consume: opt_bool(object, "consume").ok()?,
                    data: opt_string(object, "data").ok()?,
                })
            })
        });
        let unsubscribe = ui.on_terminal_input(handler);
        let function = Function::new(ctx, move || {
            unsubscribe();
            // TS 退订后监听器不再被引用，闭包可被 GC；这里释放保存的 JS 函数引用。
            adapter::release_ref(reference);
        })?;
        Ok(function.into_value())
    }

    /// `setStatus(key, text?)`。
    #[qjs(rename = "setStatus")]
    fn set_status(&self, key: String, text: Opt<String>) {
        if let Some(ui) = self.ui() {
            ui.set_status(&key, text.0.as_deref());
        }
    }

    /// `setWorkingMessage(message?)`。
    #[qjs(rename = "setWorkingMessage")]
    fn set_working_message(&self, message: Opt<String>) {
        if let Some(ui) = self.ui() {
            ui.set_working_message(message.0.as_deref());
        }
    }

    /// `setWorkingVisible(visible)`。
    #[qjs(rename = "setWorkingVisible")]
    fn set_working_visible(&self, visible: bool) {
        if let Some(ui) = self.ui() {
            ui.set_working_visible(visible);
        }
    }

    /// `setWorkingIndicator(options?)`。
    #[qjs(rename = "setWorkingIndicator")]
    fn set_working_indicator<'js>(
        &self,
        ctx: Ctx<'js>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(ui) = self.ui() else {
            return Ok(());
        };
        let parsed = match opt_value(&options) {
            Some(value) => {
                let Some(object) = value.as_object() else {
                    return Ok(());
                };
                let mut parsed = WorkingIndicatorOptions::default();
                let frames: Option<Value> = object.get("frames")?;
                if let Some(frames) = frames
                    && !frames.is_undefined()
                    && !frames.is_null()
                {
                    parsed.frames = Some(array_strings(&ctx, frames)?);
                }
                parsed.interval_ms = opt_number(object, "intervalMs")?;
                Some(parsed)
            }
            None => None,
        };
        ui.set_working_indicator(parsed);
        Ok(())
    }

    /// `setHiddenThinkingLabel(label?)`。
    #[qjs(rename = "setHiddenThinkingLabel")]
    fn set_hidden_thinking_label(&self, label: Opt<String>) {
        if let Some(ui) = self.ui() {
            ui.set_hidden_thinking_label(label.0.as_deref());
        }
    }

    /// `setWidgetLines(key, lines, placement?)`：字符串数组 → 每行一个 `Text`。
    ///
    /// 行数组原样转发（RPC 的 `setWidget` 记录发出调用方的完整数组）；10 行截断 +
    /// muted 的 "... (widget truncated)" 是 interactive 渲染层的语义
    /// （`interactive-mode.ts:2425-2430`），由 interactive 模式的 UI 上下文在渲染时做。
    #[qjs(rename = "setWidgetLines")]
    fn set_widget_lines<'js>(
        &self,
        ctx: Ctx<'js>,
        key: String,
        lines: Value<'js>,
        placement: Opt<String>,
    ) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(ui) = self.ui() else {
            return Ok(());
        };
        let lines = array_strings(&ctx, lines)?;
        ui.set_widget(
            &key,
            Some(WidgetContent::Lines(lines)),
            Some(ExtensionWidgetOptions {
                placement: parse_placement(placement.0.as_deref()),
            }),
        );
        // 同一 placement 上的旧工厂组件被宿主替换/销毁；工厂函数本身也要释放。
        self.store_widget_ref(&key, placement.0.as_deref(), None);
        Ok(())
    }

    /// `setWidgetFactory(key, factory, placement?)`。
    #[qjs(rename = "setWidgetFactory")]
    fn set_widget_factory<'js>(
        &self,
        ctx: Ctx<'js>,
        key: String,
        factory: Opt<Value<'js>>,
        placement: Opt<String>,
    ) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(ui) = self.ui() else {
            return Ok(());
        };
        let Some(reference) =
            opt_value(&factory).and_then(|value| store_function_ref(&ctx, value).ok().flatten())
        else {
            return Ok(());
        };
        let widget = widget_factory_from_ref(reference);
        ui.set_widget(
            &key,
            Some(WidgetContent::Component(widget)),
            Some(ExtensionWidgetOptions {
                placement: parse_placement(placement.0.as_deref()),
            }),
        );
        self.store_widget_ref(&key, placement.0.as_deref(), Some(reference));
        Ok(())
    }

    /// `clearWidget(key, placement?)`：清除两个 placement 上的 widget（TS 的
    /// `removeExisting` 查两张表）。
    ///
    /// `placement` 只在 `setWidget(key, undefined, options)` 的路径上携带：RPC 的
    /// `setWidget` 记录必须保留 `options.placement`（`rpc-mode.ts:203-212`）。interactive
    /// 模式忽略它（`content === undefined` 时两个 map 都清）。
    #[qjs(rename = "clearWidget")]
    fn clear_widget(&self, key: String, placement: Opt<String>) {
        if let Some(ui) = self.ui() {
            ui.set_widget(
                &key,
                None,
                Some(ExtensionWidgetOptions {
                    placement: parse_placement(placement.0.as_deref()),
                }),
            );
        }
        self.release_widget_refs(&key);
    }

    /// `setFooter(factory?)`。
    #[qjs(rename = "setFooter")]
    fn set_footer<'js>(&self, ctx: Ctx<'js>, factory: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(ui) = self.ui() else {
            return Ok(());
        };
        let reference = match opt_value(&factory) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        ui.set_footer(reference.map(footer_factory_from_ref));
        let previous = self.footer_ref.replace(reference);
        if let Some(previous) = previous
            && Some(previous) != reference
        {
            adapter::release_ref(previous);
        }
        Ok(())
    }

    /// `setHeader(factory?)`。
    #[qjs(rename = "setHeader")]
    fn set_header<'js>(&self, ctx: Ctx<'js>, factory: Opt<Value<'js>>) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(ui) = self.ui() else {
            return Ok(());
        };
        let reference = match opt_value(&factory) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        ui.set_header(reference.map(header_factory_from_ref));
        let previous = self.header_ref.replace(reference);
        if let Some(previous) = previous
            && Some(previous) != reference
        {
            adapter::release_ref(previous);
        }
        Ok(())
    }

    /// `setTitle(title)`。
    #[qjs(rename = "setTitle")]
    fn set_title(&self, title: String) {
        if let Some(ui) = self.ui() {
            ui.set_title(&title);
        }
    }

    /// `pasteToEditor(text)`。
    #[qjs(rename = "pasteToEditor")]
    fn paste_to_editor(&self, text: String) {
        if let Some(ui) = self.ui() {
            ui.paste_to_editor(&text);
        }
    }

    /// `setEditorText(text)`。
    #[qjs(rename = "setEditorText")]
    fn set_editor_text(&self, text: String) {
        if let Some(ui) = self.ui() {
            ui.set_editor_text(&text);
        }
    }

    /// `getEditorText()`。
    #[qjs(rename = "getEditorText")]
    fn get_editor_text(&self) -> String {
        self.ui().map(|ui| ui.get_editor_text()).unwrap_or_default()
    }

    /// `addAutocompleteProvider(factory)`。
    #[qjs(rename = "addAutocompleteProvider")]
    fn add_autocomplete_provider<'js>(
        &self,
        ctx: Ctx<'js>,
        factory: Value<'js>,
    ) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(ui) = self.ui() else {
            return Ok(());
        };
        let Some(reference) = store_function_ref(&ctx, factory)? else {
            return Ok(());
        };
        ui.add_autocomplete_provider(autocomplete_factory_from_ref(reference));
        Ok(())
    }

    /// `setEditorComponent(factory?)`。
    #[qjs(rename = "setEditorComponent")]
    fn set_editor_component<'js>(
        &self,
        ctx: Ctx<'js>,
        factory: Opt<Value<'js>>,
    ) -> EngineResult<()> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(ui) = self.ui() else {
            return Ok(());
        };
        let reference = match opt_value(&factory) {
            Some(value) => store_function_ref(&ctx, value)?,
            None => None,
        };
        let previous = self.editor_factory.replace(reference);
        ui.set_editor_component(reference.map(editor_factory_from_ref));
        if let Some(previous) = previous
            && Some(previous) != reference
        {
            adapter::release_ref(previous);
        }
        Ok(())
    }

    /// `getEditorComponent()`：读取**宿主**当前的工厂（TS 是
    /// `getEditorComponent: () => this.editorComponentFactory`，`interactive-mode.ts:2664`）。
    ///
    /// `/reload` 的 `resetExtensionUI` 直接调用 `setCustomEditorComponent(undefined)`
    /// （`interactive-mode.ts:2470`），不会经过 `ctx.ui`；因此宿主已清空时必须返回
    /// `undefined`，同时释放桥里缓存的旧函数引用。
    #[qjs(rename = "getEditorComponent")]
    fn get_editor_component<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let host_has_factory = self
            .ui()
            .map(|ui| ui.get_editor_component().is_some())
            .unwrap_or(false);
        if !host_has_factory {
            if let Some(previous) = self.editor_factory.borrow_mut().take() {
                adapter::release_ref(previous);
            }
            return Ok(Value::new_undefined(ctx.clone()));
        }
        match *self.editor_factory.borrow() {
            Some(reference) => load_ref(&ctx, reference),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `theme()`：当前主题的原生实例。
    #[qjs(rename = "theme")]
    fn theme<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(ui) = self.ui() else {
            return Ok(Value::new_undefined(ctx.clone()));
        };
        theme_value_from_ref(&ctx, ui.theme())
    }

    /// `getAllThemes()`。
    #[qjs(rename = "getAllThemes")]
    fn get_all_themes<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Array<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let themes: Vec<ThemeInfo> = self.ui().map(|ui| ui.get_all_themes()).unwrap_or_default();
        let array = Array::new(ctx.clone())?;
        for (index, info) in themes.into_iter().enumerate() {
            let object = Object::new(ctx.clone())?;
            object.set("name", rquickjs::String::from_str(ctx.clone(), &info.name)?)?;
            object.set(
                "path",
                match info.path.as_deref() {
                    Some(path) => rquickjs::String::from_str(ctx.clone(), path)?.into_value(),
                    None => Value::new_undefined(ctx.clone()),
                },
            )?;
            array.set(index, object)?;
        }
        Ok(array)
    }

    /// `getTheme(name)`。
    #[qjs(rename = "getTheme")]
    fn get_theme<'js>(&self, ctx: Ctx<'js>, name: String) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(ui) = self.ui() else {
            return Ok(Value::new_undefined(ctx.clone()));
        };
        match ui.get_theme(&name) {
            Some(theme) => {
                // TS 的 `getThemeByName` 每次调用都 `createTheme` 一个新实例
                // （`theme.ts:557-566`）：这里同样返回新的原生实例，不写
                // `__kiana_theme_cache`/`THEME_REGISTRY`（否则每次调用都会留下无法回收的条目）。
                let instance = Class::instance(
                    ctx.clone(),
                    JsTheme {
                        theme: Arc::new(theme),
                        live: false,
                    },
                )?;
                Ok(instance.into_value())
            }
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `setTheme(nameOrTheme)`：返回 `{ success, error? }`。
    #[qjs(rename = "setTheme")]
    fn set_theme<'js>(&self, ctx: Ctx<'js>, value: Value<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let Some(ui) = self.ui() else {
            return theme_result_to_js(
                &ctx,
                &kiana_ext_api::ui::ThemeResult {
                    success: false,
                    error: Some(crate::runner::noop_ui::UI_NOT_AVAILABLE.to_owned()),
                },
            );
        };
        // 参数是名字或 Theme 实例：实例先按 name 在上游注册表里找回（`setThemeByName` 语义），
        // 无名主题只能直接安装实例（`setThemeInstance`）。
        let target = if let Some(name) = value.as_string().and_then(|text| text.to_string().ok()) {
            ThemeOrName::Name(name)
        } else if let Ok(class) = Class::<JsTheme>::from_value(&value) {
            let theme = class.borrow().theme_arc();
            let name = theme.name().map(str::to_owned);
            match name.and_then(|name| ui.get_theme(&name)) {
                Some(owned) => ThemeOrName::Theme(owned),
                None => {
                    // 无名主题：只能直接安装实例（与 TS 的 `setThemeInstance` 等价）。
                    kiana_theme::theme::set_theme_instance(Arc::clone(&theme));
                    let result = kiana_ext_api::ui::ThemeResult {
                        success: true,
                        error: None,
                    };
                    let _ = register_theme_symbols(&ctx);
                    return theme_result_to_js(&ctx, &result);
                }
            }
        } else {
            let result = kiana_ext_api::ui::ThemeResult {
                success: false,
                error: Some("UI not available".to_owned()),
            };
            return theme_result_to_js(&ctx, &result);
        };
        let result = ui.set_theme(target);
        if result.success {
            let _ = register_theme_symbols(&ctx);
        }
        theme_result_to_js(&ctx, &result)
    }

    /// `getToolsExpanded()`。
    #[qjs(rename = "getToolsExpanded")]
    fn get_tools_expanded(&self) -> bool {
        self.ui().map(|ui| ui.get_tools_expanded()).unwrap_or(false)
    }

    /// `setToolsExpanded(expanded)`。
    #[qjs(rename = "setToolsExpanded")]
    fn set_tools_expanded(&self, expanded: bool) {
        if let Some(ui) = self.ui() {
            ui.set_tools_expanded(expanded);
        }
    }
}

/// `placement` 字符串 → 枚举（缺省 `aboveEditor`）。
///
/// 只认识 `"belowEditor"`；`"aboveEditor"`、缺失与任何其它字符串都映射到
/// `AboveEditor`，与 TS 只区分两个 map 的行为一致。
fn parse_placement(value: Option<&str>) -> Option<kiana_ext_api::ui::WidgetPlacement> {
    match value {
        Some("belowEditor") => Some(kiana_ext_api::ui::WidgetPlacement::BelowEditor),
        Some("aboveEditor") | None => Some(kiana_ext_api::ui::WidgetPlacement::AboveEditor),
        Some(_) => Some(kiana_ext_api::ui::WidgetPlacement::AboveEditor),
    }
}

/// `ThemeResult` → JS `{ success, error? }`。
fn theme_result_to_js<'js>(
    ctx: &Ctx<'js>,
    result: &kiana_ext_api::ui::ThemeResult,
) -> EngineResult<Value<'js>> {
    let object = Object::new(ctx.clone())?;
    object.set("success", result.success)?;
    if let Some(error) = result.error.as_deref() {
        object.set("error", rquickjs::String::from_str(ctx.clone(), error)?)?;
    }
    Ok(object.into_value())
}

// ===========================================================================
// 工厂桥
// ===========================================================================

/// 把 Rust 样式函数包装成 JS 函数。
fn style_fn_to_js<'js>(
    ctx: &Ctx<'js>,
    function: Arc<dyn Fn(&str) -> String + Send + Sync>,
) -> EngineResult<Function<'js>> {
    Function::new(ctx.clone(), move |text: String| -> String {
        function(&text)
    })
}

/// `SelectListTheme` → JS 对象。
fn select_list_theme_to_js<'js>(
    ctx: &Ctx<'js>,
    theme: &SelectListTheme,
) -> EngineResult<Value<'js>> {
    let object = Object::new(ctx.clone())?;
    object.set(
        "selectedPrefix",
        style_fn_to_js(ctx, Arc::clone(&theme.selected_prefix))?,
    )?;
    object.set(
        "selectedText",
        style_fn_to_js(ctx, Arc::clone(&theme.selected_text))?,
    )?;
    object.set(
        "description",
        style_fn_to_js(ctx, Arc::clone(&theme.description))?,
    )?;
    object.set(
        "scrollInfo",
        style_fn_to_js(ctx, Arc::clone(&theme.scroll_info))?,
    )?;
    object.set("noMatch", style_fn_to_js(ctx, Arc::clone(&theme.no_match))?)?;
    Ok(object.into_value())
}

/// `EditorTheme` → JS 对象（`borderColor` + `selectList`）。
fn editor_theme_to_js<'js>(ctx: &Ctx<'js>, theme: &EditorTheme) -> EngineResult<Value<'js>> {
    let object = Object::new(ctx.clone())?;
    let border = Arc::clone(&theme.border_color);
    object.set(
        "borderColor",
        Function::new(ctx.clone(), move |text: String| -> String { border(&text) })?,
    )?;
    object.set(
        "selectList",
        select_list_theme_to_js(ctx, &theme.select_list)?,
    )?;
    Ok(object.into_value())
}

/// 组件 JS 值 → [`OwnedJsComponent`]。
///
/// widget/footer/header 工厂返回的组件由宿主所有者管理：TS 在替换/清除时显式
/// `dispose?.()`（`interactive-mode.ts:2406-2458、2530、2559`），所以包装成 Drop 时
/// 调用 `dispose()` 的所有者组件（普通容器成员用 [`adapter::JsComponent`]）。
fn js_component_from_value<'js>(
    engine: &Rc<JsEngine>,
    _ctx: &Ctx<'js>,
    value: &Value<'js>,
) -> Option<Box<dyn Component>> {
    let object = value.as_object()?;
    let component = adapter::OwnedJsComponent::from_object(engine, object.clone()).ok()?;
    Some(Box::new(component))
}

/// `setWidget(key, factory)` 的工厂桥。
fn widget_factory_from_ref(reference: u64) -> WidgetFactory {
    Arc::new(
        move |tui: Arc<dyn Tui>, theme: &Theme| -> Box<dyn Component> {
            let Some(engine) = JsEngineSlot::current() else {
                return Box::new(EmptyComponent);
            };
            with_engine_js(&engine, |ctx| -> Option<Box<dyn Component>> {
                let tui_value = Class::instance(ctx.clone(), JsTui::from_tui(tui.clone()))
                    .ok()?
                    .into_value();
                let theme_value = theme_value_from_ref(&ctx, theme).ok()?;
                let result = adapter::call_ref_with_values(
                    &ctx,
                    reference,
                    None,
                    vec![tui_value, theme_value],
                )
                .ok()?;
                js_component_from_value(&engine, &ctx, &result)
            })
            .unwrap_or_else(|| Box::new(EmptyComponent))
        },
    )
}

/// `setFooter(factory)` 的工厂桥。
fn footer_factory_from_ref(reference: u64) -> FooterFactory {
    Arc::new(
        move |tui: Arc<dyn Tui>,
              theme: &Theme,
              footer_data: Arc<dyn kiana_ext_api::ui::ReadonlyFooterDataProvider>|
              -> Box<dyn Component> {
            let Some(engine) = JsEngineSlot::current() else {
                return Box::new(EmptyComponent);
            };
            with_engine_js(&engine, |ctx| -> Option<Box<dyn Component>> {
                let tui_value = Class::instance(ctx.clone(), JsTui::from_tui(tui.clone()))
                    .ok()?
                    .into_value();
                let theme_value = theme_value_from_ref(&ctx, theme).ok()?;
                let footer_value = Class::instance(
                    ctx.clone(),
                    JsFooterData::from_provider(footer_data.clone()),
                )
                .ok()?
                .into_value();
                let result = adapter::call_ref_with_values(
                    &ctx,
                    reference,
                    None,
                    vec![tui_value, theme_value, footer_value],
                )
                .ok()?;
                js_component_from_value(&engine, &ctx, &result)
            })
            .unwrap_or_else(|| Box::new(EmptyComponent))
        },
    )
}

/// `setHeader(factory)` 的工厂桥。
fn header_factory_from_ref(reference: u64) -> HeaderFactory {
    Arc::new(
        move |tui: Arc<dyn Tui>, theme: &Theme| -> Box<dyn Component> {
            let Some(engine) = JsEngineSlot::current() else {
                return Box::new(EmptyComponent);
            };
            with_engine_js(&engine, |ctx| -> Option<Box<dyn Component>> {
                let tui_value = Class::instance(ctx.clone(), JsTui::from_tui(tui.clone()))
                    .ok()?
                    .into_value();
                let theme_value = theme_value_from_ref(&ctx, theme).ok()?;
                let result = adapter::call_ref_with_values(
                    &ctx,
                    reference,
                    None,
                    vec![tui_value, theme_value],
                )
                .ok()?;
                js_component_from_value(&engine, &ctx, &result)
            })
            .unwrap_or_else(|| Box::new(EmptyComponent))
        },
    )
}

/// `setEditorComponent(factory)` 的工厂桥。
///
/// 工厂收到的第三个参数是 `KeybindingsManager`；返回值必须是对象（包装成
/// `JsEditorComponent`），否则/引擎不可用时退回 `EmptyEditorComponent`（空文本、不渲染）。
fn editor_factory_from_ref(reference: u64) -> EditorFactory {
    Arc::new(
        move |tui: Arc<dyn Tui>,
              theme: &EditorTheme,
              keybindings: Arc<KeybindingsManager>|
              -> Box<dyn EditorComponent> {
            let Some(engine) = JsEngineSlot::current() else {
                return Box::new(EmptyEditorComponent);
            };
            with_engine_js(&engine, |ctx| -> Option<Box<dyn EditorComponent>> {
                let tui_value = Class::instance(ctx.clone(), JsTui::from_tui(tui.clone()))
                    .ok()?
                    .into_value();
                let theme_value = editor_theme_to_js(&ctx, theme).ok()?;
                let keybindings_value = Class::instance(
                    ctx.clone(),
                    JsKeybindings::from_manager(keybindings.clone()),
                )
                .ok()?
                .into_value();
                let result = adapter::call_ref_with_values(
                    &ctx,
                    reference,
                    None,
                    vec![tui_value, theme_value, keybindings_value],
                )
                .ok()?;
                let object = result.as_object()?;
                let component_reference = store_ref(&ctx, object.clone().into_value()).ok()?;
                Some(
                    Box::new(JsEditorComponent::from_ref(&engine, component_reference))
                        as Box<dyn EditorComponent>,
                )
            })
            .unwrap_or_else(|| Box::new(EmptyEditorComponent))
        },
    )
}

// ===========================================================================
// 异步桥：对话框信号、custom、autocomplete
// ===========================================================================

/// JS `current` provider 的共享句柄。
type SharedAutocompleteProvider = Arc<tokio::sync::Mutex<Box<dyn AutocompleteProvider>>>;

thread_local! {
    /// `AbortSignal` → `CancellationToken` 的登记表（`__kiana_ui_link_signal`）。
    static SIGNAL_TOKENS: RefCell<HashMap<u64, tokio_util::sync::CancellationToken>> =
        RefCell::new(HashMap::new());
    static SIGNAL_SEQ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// `AutocompleteProvider` 句柄表（交给 JS 的 `current` provider）。
    static PROVIDER_HANDLES: RefCell<HashMap<u64, SharedAutocompleteProvider>> =
        RefCell::new(HashMap::new());
    /// 与 `PROVIDER_HANDLES` 同句柄的只读副本；用于 `getSuggestions` 持锁期间仍能
    /// 同步执行 `applyCompletion`/`triggerCharacters`/`shouldTriggerFileCompletion`。
    ///
    /// 这是一个**有界**缓存（[`SYNC_PROVIDER_HANDLE_LIMIT`]）：JS 对象没有 finalizer，
    /// 无法在 GC 时精确移除；超过上限按插入顺序淘汰最旧的条目，未命中时同步方法退回
    /// `try_lock` 路径（见各方法的实现）。
    static SYNC_PROVIDER_HANDLES: RefCell<
        HashMap<u64, std::sync::Arc<dyn AutocompleteProvider>>,
    > = RefCell::new(HashMap::new());
    /// `SYNC_PROVIDER_HANDLES` 的插入顺序（淘汰用）。
    static SYNC_PROVIDER_ORDER: RefCell<VecDeque<u64>> = RefCell::new(VecDeque::new());
    static PROVIDER_SEQ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// [`SYNC_PROVIDER_HANDLES`] 的上限。
///
/// JS provider 对象没有 finalizer，句柄无法在 GC 时精确移除；缓存按插入顺序淘汰最旧的
/// 条目即可避免长期运行时的无界增长。被淘汰的句柄仍有 `PROVIDER_HANDLES` 里的可变句柄，
/// 同步方法会退回 `try_lock` 路径。
const SYNC_PROVIDER_HANDLE_LIMIT: usize = 64;

/// 登记一个 provider 的同步只读快照，并在超过上限时淘汰最旧的条目。
fn remember_sync_provider(handle: u64, provider: std::sync::Arc<dyn AutocompleteProvider>) {
    SYNC_PROVIDER_HANDLES.with(|handles| {
        handles.borrow_mut().insert(handle, provider);
    });
    SYNC_PROVIDER_ORDER.with(|order| {
        let mut order = order.borrow_mut();
        order.push_back(handle);
        while order.len() > SYNC_PROVIDER_HANDLE_LIMIT {
            if let Some(evicted) = order.pop_front() {
                SYNC_PROVIDER_HANDLES.with(|handles| {
                    handles.borrow_mut().remove(&evicted);
                });
            }
        }
    });
}

/// 把一个 JS `AbortSignal` 关联到取消令牌，返回令牌 id。
///
/// 令牌放进线程局部的 `SIGNAL_TOKENS`，等 `dialog_options_from_args` 在对话框启动时
/// `remove` 取走（一个 id 只能消费一次）。abort 监听器的引用只在真正 abort 时释放；
/// 未 abort 的监听器引用会保留到引擎销毁。
fn link_signal<'js>(ctx: &Ctx<'js>, signal: Value<'js>) -> EngineResult<u64> {
    let token = tokio_util::sync::CancellationToken::new();
    let id = SIGNAL_SEQ.with(|seq| {
        let id = seq.get().wrapping_add(1).max(1);
        seq.set(id);
        id
    });
    SIGNAL_TOKENS.with(|tokens| tokens.borrow_mut().insert(id, token.clone()));
    let reference_slot: Rc<std::cell::Cell<Option<u64>>> = Rc::new(std::cell::Cell::new(None));
    let slot = Rc::clone(&reference_slot);
    let callback = Function::new(ctx.clone(), move || {
        token.cancel();
        // abort 监听器只会触发一次：触发后释放回调引用（TS 里监听器随 signal 一起被 GC）。
        if let Some(reference) = slot.take() {
            adapter::release_ref(reference);
        }
    })?;
    if let Some(object) = signal.as_object() {
        let reference = store_ref(ctx, callback.into_value())?;
        reference_slot.set(Some(reference));
        let _ = call_method::<Value>(
            ctx,
            object,
            "addEventListener",
            &[JsArg::str("abort"), JsArg::Ref(reference)],
        )?;
    }
    Ok(id)
}

/// 从 host payload 解析对话框选项（`signalId` / `timeout`）。
///
/// `ui.js` 的 `linkSignal` 对**已中止**的 `AbortSignal` 返回 `-1`（不登记句柄）。
/// 这里把 `-1` 还原成一个已取消的 token：`ExtensionUiContext` 的对话框早退检查
/// （`opts.signal?.aborted`，`rpc-mode.ts:97-99` / `UiPromptContext`）于是立即返回默认值，
/// 不会发出 TS 从不发送的 `extension_ui_request`。
///
/// 非负 id 用 `remove` 从 `SIGNAL_TOKENS` 取走（正常路径只消费一次）；表里没有该 id 时
/// `signal` 保持 `None`（对话框因此不可取消）。
fn dialog_options_from_args(args: &serde_json::Value) -> ExtensionUIDialogOptions {
    let mut options = ExtensionUIDialogOptions::default();
    if let Some(signal_id) = args.get("signalId").and_then(serde_json::Value::as_i64) {
        if signal_id == -1 {
            let token = tokio_util::sync::CancellationToken::new();
            token.cancel();
            options.signal = Some(token);
        } else if let Ok(signal_id) = u64::try_from(signal_id) {
            options.signal = SIGNAL_TOKENS.with(|tokens| tokens.borrow_mut().remove(&signal_id));
        }
    }
    if let Some(timeout) = args.get("timeout").and_then(serde_json::Value::as_f64) {
        options.timeout = Some(timeout);
    }
    options
}

/// `Option<String>` → `HostValue`。
fn optional_string_value(value: Option<String>) -> crate::engine::HostValue {
    match value {
        Some(text) => crate::engine::HostValue::String(text),
        None => crate::engine::HostValue::Undefined,
    }
}

/// `Box<dyn Any + Send>` → `HostValue`（`custom` 的 `done` 结果）。
fn boxed_to_host_value(value: Box<dyn std::any::Any + Send>) -> crate::engine::HostValue {
    use crate::engine::HostValue;
    if value.is::<()>() {
        return HostValue::Undefined;
    }
    if let Some(json) = value.downcast_ref::<serde_json::Value>() {
        return HostValue::Json(json.clone());
    }
    if let Some(text) = value.downcast_ref::<String>() {
        return HostValue::String(text.clone());
    }
    if let Some(flag) = value.downcast_ref::<bool>() {
        return HostValue::Bool(*flag);
    }
    if let Some(number) = value.downcast_ref::<f64>() {
        return HostValue::Number(*number);
    }
    HostValue::Undefined
}

/// 解析 `OverlayOptions` 的 JSON 形式（`custom` 的 overlayOptions）。
fn overlay_options_from_json(value: &serde_json::Value) -> OverlayOptions {
    let mut options = OverlayOptions::default();
    let size = |key: &str| -> Option<kiana_tui::tui::SizeValue> {
        match value.get(key)? {
            serde_json::Value::Number(number) => {
                number.as_f64().map(kiana_tui::tui::SizeValue::Absolute)
            }
            serde_json::Value::String(text) => text
                .trim()
                .strip_suffix('%')
                .and_then(|percent| percent.trim().parse::<f64>().ok())
                .map(kiana_tui::tui::SizeValue::Percent)
                .or_else(|| {
                    text.trim()
                        .parse::<f64>()
                        .ok()
                        .map(kiana_tui::tui::SizeValue::Absolute)
                }),
            _ => None,
        }
    };
    options.width = size("width");
    options.min_width = value.get("minWidth").and_then(serde_json::Value::as_f64);
    options.max_height = size("maxHeight");
    options.anchor = value
        .get("anchor")
        .cloned()
        .and_then(|anchor| serde_json::from_value(anchor).ok());
    options.offset_x = value.get("offsetX").and_then(serde_json::Value::as_f64);
    options.offset_y = value.get("offsetY").and_then(serde_json::Value::as_f64);
    options.row = size("row");
    options.col = size("col");
    options.margin = value
        .get("margin")
        .cloned()
        .and_then(|margin| serde_json::from_value(margin).ok());
    options.non_capturing = value
        .get("nonCapturing")
        .and_then(serde_json::Value::as_bool);
    options
}

/// `custom` 的选项：静态 JSON / 动态函数 / `onHandle`。
fn custom_options_from_args(args: &serde_json::Value) -> CustomOptions {
    let overlay = args.get("overlay").and_then(serde_json::Value::as_bool);
    let overlay_options_json = args.get("overlayOptions");
    let overlay_options_ref = args
        .get("overlayOptionsRef")
        .and_then(serde_json::Value::as_u64);
    let overlay_visible_ref = args
        .get("overlayVisibleRef")
        .and_then(serde_json::Value::as_u64);
    let overlay_options = match (overlay_options_ref, overlay_options_json) {
        (Some(reference), _) => Some(OverlayOptionsOrFn::Dynamic(Arc::new(move || {
            let Some(engine) = JsEngineSlot::current() else {
                return OverlayOptions::default();
            };
            with_engine_js(&engine, |ctx| -> OverlayOptions {
                let value: Option<Value> = call_ref(&ctx, reference, None, &[]).ok();
                match value.and_then(|value| value_to_json(&ctx, value).ok()) {
                    Some(json) => overlay_options_from_json(&json),
                    None => OverlayOptions::default(),
                }
            })
        }))),
        (None, Some(json)) => {
            let mut parsed = overlay_options_from_json(json);
            if let Some(reference) = overlay_visible_ref {
                parsed.visible = Some(Box::new(move |width: usize, height: usize| {
                    let Some(engine) = JsEngineSlot::current() else {
                        return true;
                    };
                    with_engine_js(&engine, |ctx| -> bool {
                        let value: Option<bool> = call_ref(
                            &ctx,
                            reference,
                            None,
                            &[JsArg::num(width as f64), JsArg::num(height as f64)],
                        )
                        .ok();
                        value.unwrap_or(true)
                    })
                }));
            }
            Some(OverlayOptionsOrFn::Static(parsed))
        }
        (None, None) => None,
    };
    let on_handle = args
        .get("onHandleRef")
        .and_then(serde_json::Value::as_u64)
        .map(|reference| {
            Arc::new(move |handle: Box<dyn OverlayHandle>| {
                let Some(engine) = JsEngineSlot::current() else {
                    return;
                };
                let _ = with_engine_js(&engine, |ctx| -> EngineResult<()> {
                    let instance = Class::instance(
                        ctx.clone(),
                        JsOverlayHandle {
                            inner: RefCell::new(Some(handle)),
                            tui: None,
                        },
                    )?;
                    let _ =
                        call_ref_with_values(&ctx, reference, None, vec![instance.into_value()]);
                    Ok(())
                });
            }) as Arc<dyn Fn(Box<dyn OverlayHandle>) + Send + Sync>
        });
    CustomOptions {
        overlay,
        overlay_options,
        on_handle,
    }
}

/// `custom` 工厂失败槽：同步抛错或返回的 Promise reject 时记录拒绝原因
/// （HostValue 原样传给 JS 的 `Promise.reject`，`tui.ts`/`interactive-mode.ts:2982-2990`）。
type CustomFailure = Arc<Mutex<Option<crate::engine::HostValue>>>;

/// 把 JS 拒绝值转成 [`crate::engine::HostValue`]（`Error` 对象保留 name/message）。
fn rejection_value<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> crate::engine::HostValue {
    use crate::engine::HostValue;
    if value.is_undefined() {
        return HostValue::Undefined;
    }
    if let Some(object) = value.as_object() {
        let has_message = object.contains_key("message").unwrap_or(false);
        if has_message {
            let name = object
                .get::<_, Option<String>>("name")
                .ok()
                .flatten()
                .unwrap_or_else(|| "Error".to_owned());
            let message = object
                .get::<_, Option<String>>("message")
                .ok()
                .flatten()
                .unwrap_or_default();
            return HostValue::Error { name, message };
        }
    }
    if value.is_string() {
        return HostValue::String(js_string(&value).unwrap_or_default());
    }
    match value_to_json(ctx, value) {
        Ok(json) => HostValue::Json(json),
        Err(_) => HostValue::Undefined,
    }
}

/// 把 JS 返回值（组件或 Promise）转成 [`JsComponent`] 的 future。
///
/// `failure` 在 Promise reject 时写入拒绝原因；同步抛错由调用方（`custom_factory_from_ref`）
/// 记录。TS 里工厂失败会让 `ctx.ui.custom(...)` 返回的 Promise reject
/// （`interactive-mode.ts:2982-2990`），这里保留同样的可观察行为。
///
/// 返回 `None` 只表示无法构造 `then` 的两个回调（rquickjs 分配失败）。接收端的结果：
/// `Some(id)` 是组件对象（同步返回或 Promise resolve 出对象），`None` 是非对象结果或
/// promise reject，`Err` 是发送端在未发送的情况下被丢弃。
fn component_future_from_call(
    call_result: Value<'_>,
    failure: CustomFailure,
) -> Option<futures::channel::oneshot::Receiver<Option<u64>>> {
    let ctx = call_result.ctx().clone();
    let (sender, receiver) = futures::channel::oneshot::channel::<Option<u64>>();
    let sender = Rc::new(RefCell::new(Some(sender)));
    // thenable（Promise）：ok/err 回调把结果经 oneshot 送回，接收端只等一次。
    if let Some(object) = call_result.as_object() {
        let then: Option<Value> = object.get("then").ok();
        if then
            .as_ref()
            .and_then(|value| value.as_function())
            .is_some()
        {
            let ok_sender = Rc::clone(&sender);
            let ok = Function::new(
                ctx.clone(),
                move |value: Opt<Value<'_>>| -> EngineResult<()> {
                    let reference = opt_value(&value).and_then(|value| {
                        let value_ctx = value.ctx().clone();
                        value
                            .as_object()
                            .cloned()
                            .and_then(|object| store_ref(&value_ctx, object.into_value()).ok())
                    });
                    if let Some(sender) = ok_sender.borrow_mut().take() {
                        let _ = sender.send(reference);
                    }
                    Ok(())
                },
            )
            .ok()?;
            let err_sender = Rc::clone(&sender);
            let err_failure = Arc::clone(&failure);
            let err = Function::new(
                ctx.clone(),
                move |error: Opt<Value<'_>>| -> EngineResult<()> {
                    if let Some(value) = opt_value(&error) {
                        let value_ctx = value.ctx().clone();
                        *err_failure
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) =
                            Some(rejection_value(&value_ctx, value));
                    } else {
                        *err_failure
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) =
                            Some(crate::engine::HostValue::Undefined);
                    }
                    if let Some(sender) = err_sender.borrow_mut().take() {
                        let _ = sender.send(None);
                    }
                    Ok(())
                },
            )
            .ok()?;
            let then = then.and_then(|value| value.as_function().cloned())?;
            let _: EngineResult<Value> = then.call((This(object.clone()), ok, err));
            return Some(receiver);
        }
    }
    // 非 thenable 的对象：按同步返回的组件处理。
    if let Some(object) = call_result.as_object()
        && let Ok(reference) = store_ref(&ctx, object.clone().into_value())
    {
        if let Some(sender) = sender.borrow_mut().take() {
            let _ = sender.send(Some(reference));
        }
        return Some(receiver);
    }
    // undefined/null/标量：没有组件可交付。
    if let Some(sender) = sender.borrow_mut().take() {
        let _ = sender.send(None);
    }
    Some(receiver)
}

/// `custom(factory)` 的工厂桥。
///
/// `failure` 由 [`ui_host_kind`] 提供：工厂同步抛错或返回的 Promise reject 时写入拒绝原因，
/// `ui_host_kind` 在 `ui.custom(...)` 返回后据此 reject JS 端的 Promise
/// （`interactive-mode.ts:2982-2990`）。
fn custom_factory_from_ref(reference: u64, failure: CustomFailure) -> CustomFactory {
    Arc::new(
        move |tui: Arc<dyn Tui>,
              theme: &Theme,
              keybindings: Arc<KeybindingsManager>,
              done: CustomDoneFn|
              -> kiana_ext_api::LocalBoxFuture<'static, Box<dyn Component>> {
            let theme = theme_value_for_future(theme);
            let failure = Arc::clone(&failure);
            Box::pin(async move {
                let Some(engine) = JsEngineSlot::current() else {
                    return Box::new(EmptyComponent) as Box<dyn Component>;
                };
                let failure_for_call = Arc::clone(&failure);
                // 工厂调用必须在当前事务里完成：TUIProxy/live 主题/键位/done 都是 JS 值。
                let outcome = with_engine_js(&engine, |ctx| -> Option<_> {
                    let tui_value = Class::instance(ctx.clone(), JsTui::from_tui(tui.clone()))
                        .ok()?
                        .into_value();
                    let theme_value = theme_value_from_ref(&ctx, &theme.resolve()).ok()?;
                    let keybindings_value = Class::instance(
                        ctx.clone(),
                        JsKeybindings::from_manager(keybindings.clone()),
                    )
                    .ok()?
                    .into_value();
                    let done_slot = Arc::clone(&done);
                    let done_function = Function::new(
                        ctx.clone(),
                        move |value: Opt<Value<'_>>| -> EngineResult<()> {
                            // TS 用**原值** resolve（`showExtensionCustom`）：`done(null)` 解析出
                            // `null`，`done()`/`done(undefined)` 解析出 `undefined`。`opt_value`
                            // 会把 null 折成 None，这里只看 `undefined`。
                            let boxed: Box<dyn std::any::Any + Send> = match value.0 {
                                Some(value) if !value.is_undefined() => {
                                    let value_ctx = value.ctx().clone();
                                    Box::new(
                                        value_to_json(&value_ctx, value)
                                            .unwrap_or(serde_json::Value::Null),
                                    )
                                }
                                _ => Box::new(()),
                            };
                            done_slot(boxed);
                            Ok(())
                        },
                    )
                    .ok()?;
                    let result = adapter::call_ref_with_values(
                        &ctx,
                        reference,
                        None,
                        vec![
                            tui_value,
                            theme_value,
                            keybindings_value,
                            done_function.into_value(),
                        ],
                    );
                    let result = match result {
                        Ok(value) => value,
                        Err(_) => {
                            // 工厂同步抛错：按 TS 的 `.catch` 语义把异常传给 JS 调用方。
                            let exception = crate::engine::take_exception(
                                &ctx,
                                crate::engine::JsErrorKind::Host,
                            );
                            *failure_for_call
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) =
                                Some(crate::engine::HostValue::Error {
                                    name: "Error".to_owned(),
                                    message: exception.message,
                                });
                            return None;
                        }
                    };
                    component_future_from_call(result, Arc::clone(&failure_for_call))
                });
                // `None` = 工厂同步抛错（failure 已写入）；否则等 Promise/同步对象的结果。
                match outcome {
                    Some(receiver) => match receiver.await {
                        Ok(Some(reference)) => match JsEngineSlot::current() {
                            Some(engine) => Box::new(adapter::OwnedJsComponent::new(
                                JsComponent::from_ref(&engine, reference),
                            )) as Box<dyn Component>,
                            None => Box::new(EmptyComponent),
                        },
                        _ => {
                            let rejected = failure
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .is_some();
                            if rejected {
                                // TS 的 `.catch` 直接 reject 外层 Promise；Rust 宿主在
                                // `done` 之前不会返回，因此这里用空结果解除等待，
                                // `ui_host_kind` 随后把 Promise reject 掉。
                                (done)(Box::new(()));
                            }
                            Box::new(EmptyComponent)
                        }
                    },
                    None => {
                        let rejected = failure
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .is_some();
                        if rejected {
                            (done)(Box::new(()));
                        }
                        Box::new(EmptyComponent)
                    }
                }
            })
        },
    )
}

/// 工厂 future 里无法安全借用 `&Theme`：记下地址，带过 `await`。
///
/// `ThemeSnapshot::resolve` 不读取这个地址：JS 侧一律拿 live 主题（模块 `Proxy` 语义，
/// 见 [`theme_value_from_ref`]），因此解析结果就是 [`current_global_theme`]。
fn theme_value_for_future(theme: &Theme) -> ThemeSnapshot {
    ThemeSnapshot {
        pointer: theme as *const Theme as usize,
    }
}

/// 主题指针快照（跨 await 传递；只在主线程事务里解析）。
struct ThemeSnapshot {
    pointer: usize,
}

impl ThemeSnapshot {
    /// 取回主题实例。工厂收到的 `&Theme` 只用于跨 `await` 传递，JS 侧一律拿 live 主题
    /// （模块 `Proxy` 语义），因此这里返回当前全局主题。
    fn resolve(&self) -> Arc<Theme> {
        let _ = self.pointer;
        current_global_theme()
    }
}

// ===========================================================================
// 自动补全 provider 桥
// ===========================================================================

/// 交给 JS 的 `current` provider（`AutocompleteProvider` 原生对象）。
///
/// 实例只持有 `PROVIDER_HANDLES` 里的数字句柄。`getSuggestions` 经
/// `__kiana.promise("kiana_autocomplete", …)` 回到 Rust provider 并返回 Promise；其余成员
/// 同步读句柄表。句柄表保存 `Arc<tokio::sync::Mutex<Box<dyn AutocompleteProvider>>>`，
/// `autocomplete_host_kind` 在 `spawn_local` 里 `lock().await` 后调用 provider。句柄表是
/// 线程局部的，本文件没有删除条目的路径。
///
/// 同步成员用 `try_lock` 而不是 `blocking_lock`：这些方法在 tokio 运行时驱动的 UI 线程上
/// 执行，`blocking_lock` 会 panic；锁被在飞的 `getSuggestions` 持有时退化为默认行为
/// （不应用补全 / 无触发字符 / 允许文件补全）。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "AutocompleteProvider")]
pub struct JsAutocompleteProvider {
    #[qjs(skip_trace)]
    handle: u64,
}

#[rquickjs::methods]
impl JsAutocompleteProvider {
    /// 内部构造器。
    #[qjs(constructor)]
    fn new() -> EngineResult<Self> {
        Ok(JsAutocompleteProvider { handle: 0 })
    }

    /// `getSuggestions(lines, cursorLine, cursorCol, options)`：返回 Promise。
    #[qjs(rename = "getSuggestions")]
    fn get_suggestions<'js>(
        &self,
        ctx: Ctx<'js>,
        lines: Value<'js>,
        cursor_line: Opt<Value<'js>>,
        cursor_col: Opt<Value<'js>>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let kiana: Object = ctx.globals().get("__kiana")?;
        let promise: Function = kiana.get("promise")?;
        let lines = array_strings(&ctx, lines)?;
        let cursor_line = opt_arg_number(&ctx, &cursor_line).unwrap_or(0.0);
        let cursor_col = opt_arg_number(&ctx, &cursor_col).unwrap_or(0.0);
        let force = opt_value(&options)
            .and_then(|value| value.as_object().cloned())
            .map(|object| opt_bool(&object, "force").ok().flatten().unwrap_or(false))
            .unwrap_or(false);
        let payload = serde_json::json!({
            "handle": self.handle,
            "lines": lines,
            "cursorLine": cursor_line,
            "cursorCol": cursor_col,
            "force": force,
        });
        promise.call((
            This(kiana),
            "kiana_autocomplete",
            json_to_value(&ctx, &payload)?,
        ))
    }

    /// `applyCompletion(lines, cursorLine, cursorCol, item, prefix)`。
    #[qjs(rename = "applyCompletion")]
    fn apply_completion<'js>(
        &self,
        ctx: Ctx<'js>,
        lines: Value<'js>,
        cursor_line: Opt<Value<'js>>,
        cursor_col: Opt<Value<'js>>,
        item: Value<'js>,
        prefix: Opt<String>,
    ) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let lines = array_strings(&ctx, lines)?;
        let cursor_line = opt_arg_number(&ctx, &cursor_line).unwrap_or(0.0) as usize;
        let cursor_col = opt_arg_number(&ctx, &cursor_col).unwrap_or(0.0) as usize;
        let Some(object) = item.as_object() else {
            return Err(Exception::throw_message(&ctx, "item is not an object"));
        };
        let selected = select_item_from_object(&ctx, object.clone().into_value())?;
        let mut item = AutocompleteItem::new(selected.value, selected.label);
        item.description = selected.description;
        let prefix = prefix.0.unwrap_or_default();
        // 优先使用与句柄关联的只读副本：它不参与 `getSuggestions` 的异步互斥，
        // 因此即使一次查询在飞，`applyCompletion` 仍能同步执行（TS 语义）。
        if let Some(result) = SYNC_PROVIDER_HANDLES.with(|handles| {
            handles.borrow().get(&self.handle).map(|provider| {
                provider.apply_completion(&lines, cursor_line, cursor_col, &item, &prefix)
            })
        }) {
            return json_to_value(
                &ctx,
                &serde_json::json!({
                    "lines": result.lines,
                    "cursorLine": result.cursor_line,
                    "cursorCol": result.cursor_col,
                }),
            );
        }
        let provider = PROVIDER_HANDLES.with(|handles| handles.borrow().get(&self.handle).cloned());
        let Some(provider) = provider else {
            return Ok(Value::new_undefined(ctx.clone()));
        };
        let Ok(provider) = provider.try_lock() else {
            // 非 JS provider 没有同步副本；provider 正被 getSuggestions 的异步任务持有
            // （同一线程）时，blocking_lock 会 panic，退化为“不应用补全”的当前文本。
            return json_to_value(
                &ctx,
                &serde_json::json!({
                    "lines": lines,
                    "cursorLine": cursor_line,
                    "cursorCol": cursor_col,
                }),
            );
        };
        let result = provider.apply_completion(&lines, cursor_line, cursor_col, &item, &prefix);
        let json = serde_json::json!({
            "lines": result.lines,
            "cursorLine": result.cursor_line,
            "cursorCol": result.cursor_col,
        });
        json_to_value(&ctx, &json)
    }

    /// `triggerCharacters`。
    #[qjs(get, rename = "triggerCharacters")]
    fn trigger_characters<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let characters = if let Some(characters) = SYNC_PROVIDER_HANDLES.with(|handles| {
            handles
                .borrow()
                .get(&self.handle)
                .map(|provider| provider.trigger_characters())
        }) {
            characters
        } else {
            let provider =
                PROVIDER_HANDLES.with(|handles| handles.borrow().get(&self.handle).cloned());
            let Some(provider) = provider else {
                return Ok(Value::new_undefined(ctx.clone()));
            };
            let Ok(provider) = provider.try_lock() else {
                return Ok(Value::new_undefined(ctx.clone()));
            };
            provider.trigger_characters()
        };
        match characters {
            Some(characters) => {
                let array = Array::new(ctx.clone())?;
                for (index, character) in characters.iter().enumerate() {
                    array.set(
                        index,
                        rquickjs::String::from_str(ctx.clone(), &character.to_string())?,
                    )?;
                }
                Ok(array.into_value())
            }
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    }

    /// `shouldTriggerFileCompletion(lines, cursorLine, cursorCol)`。
    #[qjs(rename = "shouldTriggerFileCompletion")]
    fn should_trigger_file_completion<'js>(
        &self,
        ctx: Ctx<'js>,
        lines: Value<'js>,
        cursor_line: Opt<Value<'js>>,
        cursor_col: Opt<Value<'js>>,
    ) -> EngineResult<bool> {
        let _active = ActiveCtxGuard::enter(&ctx);
        let lines = array_strings(&ctx, lines)?;
        let cursor_line = opt_arg_number(&ctx, &cursor_line).unwrap_or(0.0) as usize;
        let cursor_col = opt_arg_number(&ctx, &cursor_col).unwrap_or(0.0) as usize;
        if let Some(value) = SYNC_PROVIDER_HANDLES.with(|handles| {
            handles.borrow().get(&self.handle).map(|provider| {
                provider.should_trigger_file_completion(&lines, cursor_line, cursor_col)
            })
        }) {
            return Ok(value);
        }
        let provider = PROVIDER_HANDLES.with(|handles| handles.borrow().get(&self.handle).cloned());
        let Some(provider) = provider else {
            return Ok(true);
        };
        let Ok(provider) = provider.try_lock() else {
            return Ok(true);
        };
        Ok(provider.should_trigger_file_completion(&lines, cursor_line, cursor_col))
    }
}

/// 宿主把 Rust `AutocompleteProvider` 交给 JS 自定义编辑器时的装箱约定。
///
/// `EditorComponent::set_autocomplete_provider` 的冻结类型是 `Box<dyn Any>`；宿主用本包装
/// （或直接装箱 `Box<dyn AutocompleteProvider>`）即可让 `JsEditorComponent` 把它转成 JS
/// 原生 provider 句柄交给 `editor.setAutocompleteProvider(...)`。
pub struct NativeAutocompleteProvider(pub Box<dyn AutocompleteProvider>);

/// 把 Rust provider 包成 JS 可见的原生 `AutocompleteProvider` 实例（句柄表 + 原生类）。
pub(crate) fn autocomplete_provider_value<'js>(
    ctx: &Ctx<'js>,
    provider: AutocompleteProviderObject,
) -> EngineResult<Value<'js>> {
    let provider: Box<dyn AutocompleteProvider> =
        match provider.downcast::<NativeAutocompleteProvider>() {
            Ok(wrapped) => wrapped.0,
            Err(other) => match other.downcast::<Box<dyn AutocompleteProvider>>() {
                Ok(double) => *double,
                Err(_) => {
                    return Err(Exception::throw_message(
                        ctx,
                        "unsupported autocomplete provider object",
                    ));
                }
            },
        };
    let handle = PROVIDER_SEQ.with(|seq| {
        let handle = seq.get().wrapping_add(1).max(1);
        seq.set(handle);
        handle
    });
    let sync_snapshot = provider.sync_snapshot();
    PROVIDER_HANDLES.with(|handles| {
        handles
            .borrow_mut()
            .insert(handle, Arc::new(tokio::sync::Mutex::new(provider)));
    });
    if let Some(snapshot) = sync_snapshot {
        remember_sync_provider(handle, snapshot);
    }
    let instance = Class::instance(ctx.clone(), JsAutocompleteProvider { handle })?;
    Ok(instance.into_value())
}

/// JS provider → Rust [`AutocompleteProvider`]（`getSuggestions` 经 Promise 桥）。
struct JsAutocompleteProviderWrapper {
    reference: u64,
    fallback: Arc<tokio::sync::Mutex<Box<dyn AutocompleteProvider>>>,
}

impl AutocompleteProvider for JsAutocompleteProviderWrapper {
    fn sync_snapshot(&self) -> Option<std::sync::Arc<dyn AutocompleteProvider>> {
        // 包装只持有 JS 引用 id 与回退句柄，复制两份不会共享可变 Rust 状态；同步方法
        // 因此可以在 `getSuggestions` 持锁期间独立执行。
        Some(std::sync::Arc::new(JsAutocompleteProviderWrapper {
            reference: self.reference,
            fallback: Arc::clone(&self.fallback),
        }))
    }

    fn trigger_characters(&self) -> Option<Vec<char>> {
        let engine = JsEngineSlot::current()?;
        let reference = self.reference;
        with_engine_js(&engine, |ctx| -> Option<Vec<char>> {
            let value = load_ref(&ctx, reference).ok()?;
            let object = value.as_object()?;
            let characters: Option<Value> = object.get("triggerCharacters").ok()?;
            let characters = characters?;
            if characters.is_undefined() || characters.is_null() {
                return None;
            }
            let array = characters.as_array()?;
            let strings = strings_from_array(array).ok()?;
            Some(
                strings
                    .into_iter()
                    .filter_map(|text| text.chars().next())
                    .collect(),
            )
        })
    }

    fn get_suggestions<'a>(
        &'a mut self,
        lines: &'a [String],
        cursor_line: usize,
        cursor_col: usize,
        options: &'a kiana_tui::autocomplete::GetSuggestionsOptions,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Option<kiana_tui::autocomplete::AutocompleteSuggestions>,
                > + Send
                + 'a,
        >,
    > {
        let lines = lines.to_vec();
        let force = options.force;
        let reference = self.reference;
        let fallback = Arc::clone(&self.fallback);
        Box::pin(async move {
            let (sender, receiver) = tokio::sync::oneshot::channel::<Option<serde_json::Value>>();
            let sender = std::sync::Arc::new(std::sync::Mutex::new(Some(sender)));
            // 先把调用排进当前事务；引擎不在、对象或方法缺失时 dispatched=false。
            let dispatched = match JsEngineSlot::current() {
                Some(engine) => with_engine_js(&engine, |ctx| -> bool {
                    let value = match load_ref(&ctx, reference) {
                        Ok(value) => value,
                        Err(_) => return false,
                    };
                    let Some(object) = value.as_object() else {
                        return false;
                    };
                    let get_suggestions: Option<Value> =
                        object.get("getSuggestions").ok().flatten();
                    let Some(function) = get_suggestions
                        .as_ref()
                        .and_then(|value| value.as_function())
                    else {
                        return false;
                    };
                    let lines_value = match strings_to_js(&ctx, &lines) {
                        Ok(array) => array.into_value(),
                        Err(_) => return false,
                    };
                    let options_object = match Object::new(ctx.clone()) {
                        Ok(object) => object,
                        Err(_) => return false,
                    };
                    // TS 的编辑器总是传 `{ signal, force }`（`editor.ts:2374-2379`）。
                    let _ = options_object.set("force", force);
                    // TS 的 `GetSuggestionsOptions` 是 `{ signal: AbortSignal; force?: boolean }`
                    // （`autocomplete.ts:268-297`）；仓库里的示例直接读 `options.signal.aborted`
                    // （`github-issue-autocomplete.ts:105`），缺少 `signal` 会让 provider 抛错。
                    if let Ok(signal) = abort_signal_for_token(&ctx, &options.signal) {
                        let _ = options_object.set("signal", signal);
                    }
                    let call = function.call((
                        This(object.clone()),
                        lines_value,
                        cursor_line as f64,
                        cursor_col as f64,
                        options_object,
                    ));
                    let Ok(result) = call else {
                        let _ = ctx.catch();
                        return false;
                    };
                    let result: Value = result;
                    let Some(result_object) = result.as_object() else {
                        return false;
                    };
                    let then: Option<Value> = result_object.get("then").ok().flatten();
                    let Some(then) = then.as_ref().and_then(|value| value.as_function()) else {
                        return false;
                    };
                    let sender_slot = std::sync::Arc::clone(&sender);
                    let ok = Function::new(
                        ctx.clone(),
                        move |value: Opt<Value<'_>>| -> EngineResult<()> {
                            let json = opt_value(&value).and_then(|value| {
                                let value_ctx = value.ctx().clone();
                                value_to_json(&value_ctx, value).ok()
                            });
                            if let Some(sender) = sender_slot
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .take()
                            {
                                let _ = sender.send(json);
                            }
                            Ok(())
                        },
                    );
                    let Ok(ok) = ok else {
                        return false;
                    };
                    let sender_slot = std::sync::Arc::clone(&sender);
                    let err = Function::new(
                        ctx.clone(),
                        move |_error: Opt<Value<'_>>| -> EngineResult<()> {
                            if let Some(sender) = sender_slot
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .take()
                            {
                                let _ = sender.send(None);
                            }
                            Ok(())
                        },
                    );
                    let Ok(err) = err else {
                        return false;
                    };
                    let _: EngineResult<Value> = then.call((This(result_object.clone()), ok, err));
                    true
                }),
                None => false,
            };
            if !dispatched {
                // JS 侧不可用：交给宿主传入的 provider（`addAutocompleteProvider` 的原入参）。
                let mut fallback = fallback.lock().await;
                return fallback
                    .get_suggestions(&lines, cursor_line, cursor_col, options)
                    .await;
            }
            // resolve 值按 `{ items, prefix }` 解析；reject、形状不符或引擎丢失都按“无建议”返回。
            let json = receiver.await.ok().flatten();
            let json = json?;
            let items = json.get("items")?.as_array()?;
            let mut suggestions = Vec::with_capacity(items.len());
            for item in items {
                let value = item.get("value")?.as_str()?.to_owned();
                let label = item
                    .get("label")
                    .and_then(|label| label.as_str())
                    .unwrap_or(&value)
                    .to_owned();
                let mut parsed = AutocompleteItem::new(value, label);
                parsed.description = item
                    .get("description")
                    .and_then(|description| description.as_str())
                    .map(str::to_owned);
                suggestions.push(parsed);
            }
            let prefix = json
                .get("prefix")
                .and_then(|prefix| prefix.as_str())
                .unwrap_or("")
                .to_owned();
            Some(kiana_tui::autocomplete::AutocompleteSuggestions {
                items: suggestions,
                prefix,
            })
        })
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> kiana_tui::autocomplete::ApplyCompletionResult {
        let engine = JsEngineSlot::current();
        if let Some(engine) = engine {
            let reference = self.reference;
            if let Some(result) = with_engine_js(
                &engine,
                |ctx| -> Option<kiana_tui::autocomplete::ApplyCompletionResult> {
                    let value = load_ref(&ctx, reference).ok()?;
                    let object = value.as_object()?;
                    let lines_value = strings_to_js(&ctx, lines).ok()?.into_value();
                    let item_value = json_to_value(
                        &ctx,
                        &serde_json::json!({
                            "value": item.value,
                            "label": item.label,
                            "description": item.description,
                        }),
                    )
                    .ok()?;
                    let result: Value = call_method(
                        &ctx,
                        object,
                        "applyCompletion",
                        &[
                            JsArg::Json(serde_json::json!(lines)),
                            JsArg::num(cursor_line as f64),
                            JsArg::num(cursor_col as f64),
                            JsArg::Json(serde_json::json!({
                                "value": item.value,
                                "label": item.label,
                                "description": item.description,
                            })),
                            JsArg::str(prefix),
                        ],
                    )
                    .ok()??;
                    let _ = (lines_value, item_value);
                    let json = value_to_json(&ctx, result).ok()?;
                    let lines = json
                        .get("lines")?
                        .as_array()?
                        .iter()
                        .filter_map(|line| line.as_str().map(str::to_owned))
                        .collect::<Vec<_>>();
                    Some(kiana_tui::autocomplete::ApplyCompletionResult {
                        lines,
                        cursor_line: json.get("cursorLine")?.as_u64()? as usize,
                        cursor_col: json.get("cursorCol")?.as_u64()? as usize,
                    })
                },
            ) {
                return result;
            }
        }
        let fallback = self.fallback.try_lock();
        match fallback {
            Ok(fallback) => fallback.apply_completion(lines, cursor_line, cursor_col, item, prefix),
            Err(_) => kiana_tui::autocomplete::ApplyCompletionResult {
                lines: lines.to_vec(),
                cursor_line,
                cursor_col,
            },
        }
    }

    fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        let Some(engine) = JsEngineSlot::current() else {
            return true;
        };
        let reference = self.reference;
        with_engine_js(&engine, |ctx| -> Option<bool> {
            let value = load_ref(&ctx, reference).ok()?;
            let object = value.as_object()?;
            call_method(
                &ctx,
                object,
                "shouldTriggerFileCompletion",
                &[
                    JsArg::Json(serde_json::json!(lines)),
                    JsArg::num(cursor_line as f64),
                    JsArg::num(cursor_col as f64),
                ],
            )
            .ok()
            .flatten()
        })
        .unwrap_or(true)
    }
}

/// `addAutocompleteProvider(factory)` 的工厂桥。
///
/// 工厂收到一个指向宿主 provider 的原生句柄，返回值被包成 `JsAutocompleteProviderWrapper`
/// （JS 方法优先，`fallback` 指向原 provider）。引擎不可用或工厂调用失败时直接返回
/// `FallbackProvider`，行为退化为宿主 provider 本身。
fn autocomplete_factory_from_ref(reference: u64) -> AutocompleteProviderFactory {
    Arc::new(
        move |current: Box<dyn AutocompleteProvider>| -> Box<dyn AutocompleteProvider> {
            let current = Arc::new(tokio::sync::Mutex::new(current));
            let Some(engine) = JsEngineSlot::current() else {
                return Box::new(FallbackProvider(current));
            };
            let outcome = with_engine_js(&engine, |ctx| -> Option<Box<dyn AutocompleteProvider>> {
                let handle = PROVIDER_SEQ.with(|seq| {
                    let handle = seq.get().wrapping_add(1).max(1);
                    seq.set(handle);
                    handle
                });
                PROVIDER_HANDLES.with(|handles| {
                    handles.borrow_mut().insert(handle, Arc::clone(&current));
                });
                let provider_value =
                    Class::instance(ctx.clone(), JsAutocompleteProvider { handle })
                        .ok()?
                        .into_value();
                let result =
                    adapter::call_ref_with_values(&ctx, reference, None, vec![provider_value])
                        .ok()?;
                let object = result.as_object()?;
                let provider_reference = store_ref(&ctx, object.clone().into_value()).ok()?;
                Some(Box::new(JsAutocompleteProviderWrapper {
                    reference: provider_reference,
                    fallback: Arc::clone(&current),
                }) as Box<dyn AutocompleteProvider>)
            });
            outcome.unwrap_or_else(|| Box::new(FallbackProvider(current)))
        },
    )
}

/// JS provider 的兜底：没有宿主 provider 时返回“无建议”。
struct NullAutocompleteProvider;

impl AutocompleteProvider for NullAutocompleteProvider {
    fn get_suggestions<'a>(
        &'a mut self,
        _lines: &'a [String],
        _cursor_line: usize,
        _cursor_col: usize,
        _options: &'a kiana_tui::autocomplete::GetSuggestionsOptions,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Option<kiana_tui::autocomplete::AutocompleteSuggestions>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async { None })
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        _item: &AutocompleteItem,
        _prefix: &str,
    ) -> kiana_tui::autocomplete::ApplyCompletionResult {
        kiana_tui::autocomplete::ApplyCompletionResult {
            lines: lines.to_vec(),
            cursor_line,
            cursor_col,
        }
    }
}

/// 引擎不可用时的 provider 兜底（代理到原 provider）。
struct FallbackProvider(Arc<tokio::sync::Mutex<Box<dyn AutocompleteProvider>>>);

impl AutocompleteProvider for FallbackProvider {
    fn trigger_characters(&self) -> Option<Vec<char>> {
        self.0
            .try_lock()
            .ok()
            .and_then(|provider| provider.trigger_characters())
    }

    fn get_suggestions<'a>(
        &'a mut self,
        lines: &'a [String],
        cursor_line: usize,
        cursor_col: usize,
        options: &'a kiana_tui::autocomplete::GetSuggestionsOptions,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Option<kiana_tui::autocomplete::AutocompleteSuggestions>,
                > + Send
                + 'a,
        >,
    > {
        let lines = lines.to_vec();
        let options = options.clone();
        let provider = Arc::clone(&self.0);
        Box::pin(async move {
            let mut provider = provider.lock().await;
            provider
                .get_suggestions(&lines, cursor_line, cursor_col, &options)
                .await
        })
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> kiana_tui::autocomplete::ApplyCompletionResult {
        match self.0.try_lock() {
            Ok(provider) => provider.apply_completion(lines, cursor_line, cursor_col, item, prefix),
            Err(_) => kiana_tui::autocomplete::ApplyCompletionResult {
                lines: lines.to_vec(),
                cursor_line,
                cursor_col,
            },
        }
    }
}

// ===========================================================================
// host kind：`__kiana.promise("kiana_ui", …)` / `("kiana_autocomplete", …)`
// ===========================================================================

/// `kiana_ui` 的请求载荷。
#[derive(serde::Deserialize)]
struct UiHostRequest {
    handle: u64,
    op: String,
    #[serde(default)]
    args: serde_json::Value,
}

/// `kiana_autocomplete` 的请求载荷。
#[derive(serde::Deserialize)]
struct AutocompleteHostRequest {
    handle: u64,
    #[serde(default)]
    lines: Vec<String>,
    #[serde(default)]
    cursor_line: f64,
    #[serde(default)]
    cursor_col: f64,
    #[serde(default)]
    force: bool,
}

/// `kiana_ui` 的 host kind（`js/ui/ui.js` 的四个对话框与 `custom`）。
///
/// 每个 op 都做三步：
///
/// 1. 从 payload 的 `args` 读参数（缺失字段按空串/空数组/`None`）；
/// 2. 在 `spawn_local` 上 await 对应的 [`ExtensionUiContext`] 方法；
/// 3. 用 [`crate::engine::HostCompletion::Settle`] 回填 JS Promise。
///
/// 未知 `op`、未知 UI 句柄、`custom` 缺少 `factoryRef` 都会抛 JS 异常。`custom` 的工厂抛错/
/// Promise reject 时由任务用 `ok: false` 回填，JS 侧看到的就是 rejected Promise。
fn ui_host_kind<'js>(call: crate::engine::HostCall<'js>) -> EngineResult<()> {
    let request: UiHostRequest = call.payload_json()?;
    let Some(ui) = crate::runner::context::resolve_ui_context(request.handle) else {
        return Err(Exception::throw_message(
            call.ctx(),
            &format!("Unknown UI context: {}", request.handle),
        ));
    };
    let id = call.id();
    let sender = call.completion_sender();
    let args = request.args;
    match request.op.as_str() {
        "select" => {
            let title = args
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let options: Vec<String> = args
                .get("options")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let opts = dialog_options_from_args(&args);
            tokio::task::spawn_local(async move {
                let value = ui.select(&title, &options, Some(opts)).await;
                sender.send(crate::engine::HostCompletion::Settle {
                    id,
                    ok: true,
                    value: optional_string_value(value),
                });
            });
        }
        "confirm" => {
            let title = args
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let message = args
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let opts = dialog_options_from_args(&args);
            tokio::task::spawn_local(async move {
                let value = ui.confirm(&title, &message, Some(opts)).await;
                sender.send(crate::engine::HostCompletion::Settle {
                    id,
                    ok: true,
                    value: crate::engine::HostValue::Bool(value),
                });
            });
        }
        "input" => {
            let title = args
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let placeholder = args
                .get("placeholder")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let opts = dialog_options_from_args(&args);
            tokio::task::spawn_local(async move {
                let value = ui.input(&title, placeholder.as_deref(), Some(opts)).await;
                sender.send(crate::engine::HostCompletion::Settle {
                    id,
                    ok: true,
                    value: optional_string_value(value),
                });
            });
        }
        "editor" => {
            let title = args
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let prefill = args
                .get("prefill")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            tokio::task::spawn_local(async move {
                let value = ui.editor(&title, prefill.as_deref()).await;
                sender.send(crate::engine::HostCompletion::Settle {
                    id,
                    ok: true,
                    value: optional_string_value(value),
                });
            });
        }
        "custom" => {
            let Some(factory_reference) =
                args.get("factoryRef").and_then(serde_json::Value::as_u64)
            else {
                return Err(Exception::throw_message(
                    call.ctx(),
                    "custom requires a factory function",
                ));
            };
            let failure: CustomFailure = Arc::new(Mutex::new(None));
            let factory = custom_factory_from_ref(factory_reference, Arc::clone(&failure));
            let options = custom_options_from_args(&args);
            tokio::task::spawn_local(async move {
                let value = ui.custom(factory, Some(options)).await;
                let rejection = failure
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                let value = match rejection {
                    // 工厂抛错/返回的 Promise reject：TS 让 `ctx.ui.custom(...)` 的 Promise
                    // reject（`interactive-mode.ts:2982-2990`），扩展可以 `catch` 到。
                    Some(reason) => crate::engine::HostCompletion::Settle {
                        id,
                        ok: false,
                        value: reason,
                    },
                    None => crate::engine::HostCompletion::Settle {
                        id,
                        ok: true,
                        value: boxed_to_host_value(value),
                    },
                };
                sender.send(value);
            });
        }
        other => {
            return Err(Exception::throw_message(
                call.ctx(),
                &format!("unknown UI op: {other}"),
            ));
        }
    }
    Ok(())
}

/// `kiana_autocomplete` 的 host kind（JS provider 的 `getSuggestions`）。
///
/// 句柄未知时抛 JS 异常；否则在 `spawn_local` 里 `lock` provider 并 await
/// `get_suggestions`，结果编码成 `{items, prefix}`（无建议时 `undefined`）。查询用新建的
/// `CancellationToken`：JS 侧没有传取消信号，因此宿主 provider 的这次查询不可取消。
fn autocomplete_host_kind<'js>(call: crate::engine::HostCall<'js>) -> EngineResult<()> {
    let request: AutocompleteHostRequest = call.payload_json()?;
    let Some(provider) =
        PROVIDER_HANDLES.with(|handles| handles.borrow().get(&request.handle).cloned())
    else {
        return Err(Exception::throw_message(
            call.ctx(),
            &format!("Unknown autocomplete provider: {}", request.handle),
        ));
    };
    let id = call.id();
    let sender = call.completion_sender();
    tokio::task::spawn_local(async move {
        let mut provider = provider.lock().await;
        let options = kiana_tui::autocomplete::GetSuggestionsOptions::new(
            tokio_util::sync::CancellationToken::new(),
            request.force,
        );
        let value = provider
            .get_suggestions(
                &request.lines,
                request.cursor_line as usize,
                request.cursor_col as usize,
                &options,
            )
            .await;
        let value = match value {
            Some(suggestions) => crate::engine::HostValue::Json(serde_json::json!({
                "items": suggestions.items.iter().map(|item| serde_json::json!({
                    "value": item.value,
                    "label": item.label,
                    "description": item.description,
                })).collect::<Vec<_>>(),
                "prefix": suggestions.prefix,
            })),
            None => crate::engine::HostValue::Undefined,
        };
        sender.send(crate::engine::HostCompletion::Settle {
            id,
            ok: true,
            value,
        });
    });
    Ok(())
}

// ===========================================================================
// 安装
// ===========================================================================

/// 登记引擎（工厂闭包按线程查回引擎）。返回内部 id。
///
/// 同一线程可以登记多个引擎；`JsEngineSlot::current` 取**最后登记**的一个。重复登记同一
/// 引擎无副作用，并返回它当前的位置（1 起算，随登记表变化）。调用方要保证「当前」是正在
/// 使用的引擎：销毁旧代后再登记新代，或在销毁时 [`detach`]。
pub fn attach(engine: &Rc<JsEngine>) -> u64 {
    JsEngineSlot::attach(engine)
}

/// 注销引擎。
///
/// 只从线程登记表移除；之后 `JsEngineSlot::current` 回退到更早登记的引擎（没有则 `None`），
/// 已经创建的对象不受影响。重复注销无副作用。
pub fn detach(engine: &Rc<JsEngine>) {
    JsEngineSlot::detach(engine);
}

/// 安装 UI 适配层（设计文档 §9.2：prelude、node globals、context 之后）。
///
/// 顺序不能交换：
///
/// 1. 重建 `__kiana_refs`/`__kiana_theme_cache`（旧引用 id 立即失效）；
/// 2. 把 22 个原生类定义到 `__kiana_tui_native`，再补文本/键位/模糊匹配工具函数；
/// 3. 定义 `__kiana_ui_native`（`ExtensionUI`、`linkSignal`、`storeRef`、`isNoopUi`）；
/// 4. 求值 `js/ui/tui.js` 与 `js/ui/ui.js`——两份脚本分别要求上面的 native 对象和
///    `__kiana`（prelude 已安装）存在，否则在求值期抛错；
/// 5. 登记两个主题 symbol，最后安装 loader 的主题/组件桥。
///
/// 「幂等」只指可以重复执行：重复调用会重跑 1–5，而第 1 步会让**旧组件保存的引用 id 全部
/// 失效**（`load_ref` 返回 `undefined`）。因此宿主只在引擎创建后调用一次；已有组件存活时
/// 不要重新安装。
pub fn install(engine: &JsEngine) -> EngineResult<()> {
    engine.register_host_kind(HOST_KIND, ui_host_kind);
    engine.register_host_kind(AUTOCOMPLETE_HOST_KIND, autocomplete_host_kind);
    engine.with_js(|ctx| -> EngineResult<()> {
        // 1. 引用表/主题缓存整体换新：之后的 id 与之前的命名空间互不相干。
        ctx.globals()
            .set("__kiana_refs", Object::new(ctx.clone())?)?;
        ctx.globals()
            .set("__kiana_theme_cache", Object::new(ctx.clone())?)?;

        // 2. 原生类与文本工具：`js/ui/tui.js` 在求值时按名字读取这个对象。
        let native = Object::new(ctx.clone())?;
        Class::<JsText>::define(&native)?;
        Class::<JsBox>::define(&native)?;
        Class::<JsSpacer>::define(&native)?;
        Class::<JsTruncatedText>::define(&native)?;
        Class::<JsDynamicBorder>::define(&native)?;
        Class::<JsLoader>::define(&native)?;
        Class::<JsCancellableLoader>::define(&native)?;
        Class::<JsBorderedLoader>::define(&native)?;
        Class::<JsInput>::define(&native)?;
        Class::<JsSelectList>::define(&native)?;
        Class::<JsSettingsList>::define(&native)?;
        Class::<JsMarkdown>::define(&native)?;
        Class::<JsImage>::define(&native)?;
        Class::<JsScrollView>::define(&native)?;
        Class::<JsEditor>::define(&native)?;
        Class::<JsTheme>::define(&native)?;
        Class::<JsTui>::define(&native)?;
        Class::<JsTerminal>::define(&native)?;
        Class::<JsKeybindings>::define(&native)?;
        Class::<JsFooterData>::define(&native)?;
        Class::<JsFooterStatuses>::define(&native)?;
        Class::<JsOverlayHandle>::define(&native)?;
        install_native_functions(&ctx, &native)?;
        ctx.globals().set("__kiana_tui_native", native)?;

        // 3. `ctx.ui` 的原生落点与三个互操作原语。
        let ui_native = Object::new(ctx.clone())?;
        Class::<JsUiBridge>::define(&ui_native)?;
        ui_native.set(
            "linkSignal",
            Function::new(ctx.clone(), |signal: Value<'_>| -> EngineResult<f64> {
                let signal_ctx = signal.ctx().clone();
                link_signal(&signal_ctx, signal).map(|id| id as f64)
            })?,
        )?;
        // `js/ui/ui.js` 用这两个原语：把 JS 值存成引用 id（`custom` 的工厂/动态 overlay
        // 选项/`onHandle`），以及判断句柄是否为 no-op（是则返回 undefined，交给 context.js
        // 的 `noopUi`）。
        ui_native.set(
            "storeRef",
            Function::new(ctx.clone(), |value: Value<'_>| -> EngineResult<f64> {
                let value_ctx = value.ctx().clone();
                store_ref(&value_ctx, value).map(|id| id as f64)
            })?,
        )?;
        ui_native.set(
            "isNoopUi",
            Function::new(ctx.clone(), |handle: f64| -> bool {
                if !handle.is_finite() || handle < 0.0 {
                    return true;
                }
                match crate::runner::context::resolve_ui_context(handle as u64) {
                    Some(ui) => is_no_op_ui_context(&ui),
                    None => true,
                }
            })?,
        )?;
        ctx.globals().set("__kiana_ui_native", ui_native)?;
        Ok(())
    })?;

    // 4. 两份 JS 依赖上一步的 native 对象：tui.js 读 `__kiana_tui_native`，ui.js 还要求
    //    prelude 的 `__kiana`（`__kiana.promise`）。求值顺序即依赖顺序。
    engine.eval_script::<()>(TUI_JS, "<kiana_tui>")?;
    engine.eval_script::<()>(UI_JS, "<kiana_ui>")?;
    // 5. 主题 symbol 指向 live 实例；loader 桥让扩展渲染器拿到同一个主题与组件包装。
    engine.with_js(|ctx| register_theme_symbols(&ctx))?;
    install_loader_bridges();
    Ok(())
}

/// 把 UI 单元的两个 JS 物化桥安装到 loader（`loader/mod.rs` 的钩子表：`ui` 安装方）。
///
/// - [`crate::loader::set_theme_bridge`]：渲染器第三参 `theme`。TS 传的是
///   `modes/interactive/theme/theme.ts` 的模块级 `Proxy`（读 `globalThis[Symbol.for(…)]`），
///   不是某次调用时的主题快照；[`live_theme_value`] 正是这个对象（`ctx.ui.theme` /
///   `getTheme` 之外的所有路径共享同一实例，`setTheme` 后旧引用仍然生效）。
/// - [`crate::loader::set_component_bridge`]：渲染器返回值 → TUI 组件。TS 直接把返回的
///   组件对象交给容器渲染；Rust 用 [`adapter::JsComponent`] 包装（`render`/`handleInput`
///   转发回 JS），生命周期与 TS 的容器成员一致（Drop 只释放引用，不调用 `dispose()`）。
///
/// 未安装时 loader 的渲染器会收到 `undefined` 主题、返回值被丢弃（历史缺口）；[`install`]
/// 总是安装这两个桥。
///
/// 两个槽都是**线程局部**的（`loader` 的 `THEME_BRIDGE`/`COMPONENT_BRIDGE`），后一次安装
/// 覆盖前一次。组件桥在回调时用 `JsEngineSlot::current()` 找引擎，因此同一线程上若登记了
/// 多个引擎，绑定的是最后登记的那个（`attach`/`detach` 的顺序因此有意义）。
fn install_loader_bridges() {
    crate::loader::set_theme_bridge(Some(Arc::new(|ctx, _theme| live_theme_value(ctx))));
    crate::loader::set_component_bridge(Some(Arc::new(|ctx, value| {
        let Some(engine) = JsEngineSlot::current() else {
            return Err(Exception::throw_message(ctx, "UI engine is not attached"));
        };
        let Some(object) = value.as_object() else {
            return Err(Exception::throw_message(
                ctx,
                "renderer did not return a component object",
            ));
        };
        let component = JsComponent::from_object(&engine, object.clone())?;
        Ok(Box::new(component) as Box<dyn Component>)
    })));
}

/// 安装并登记引擎（`Rc` 版本的便捷入口）。
///
/// 等价于 [`attach`] + [`install`]。装配方（`Generation::with_config`）走这一条：工厂闭包
/// 是 `Send + Sync`、不能捕获 `Rc<JsEngine>`，只能在被调用时按线程查回引擎。安装失败时
/// **不会**自动 [`detach`]——登记已经发生，调用方需要自己清理，否则引擎会活在线程槽里。
pub fn install_attached(engine: &Rc<JsEngine>) -> EngineResult<()> {
    JsEngineSlot::attach(engine);
    install(engine)
}

/// 原生函数（文本宽度/键位/fuzzy/光标/行合成）。
///
/// 名称与 `js/ui/tui.js` 的导出面一一对应，改名字会让 `tui.js` 取到 `undefined` 并在首次
/// 调用时抛 `TypeError`（该文件只检查 `__kiana_tui_native` 对象本身存在，不检查成员）。
fn install_native_functions<'js>(ctx: &Ctx<'js>, native: &Object<'js>) -> EngineResult<()> {
    native.set(
        "visibleWidth",
        Function::new(ctx.clone(), |text: String| -> f64 {
            kiana_tui::text::visible_width(&text) as f64
        })?,
    )?;
    native.set(
        "truncateToWidth",
        Function::new(
            ctx.clone(),
            |text: String, max_width: f64, ellipsis: Opt<String>, pad: Opt<bool>| -> String {
                let max_width = if max_width.is_finite() {
                    max_width.max(0.0) as usize
                } else {
                    return text;
                };
                kiana_tui::text::truncate_to_width(
                    &text,
                    max_width,
                    ellipsis.0.as_deref().unwrap_or("..."),
                    pad.0.unwrap_or(false),
                )
            },
        )?,
    )?;
    native.set(
        "wrapTextWithAnsi",
        Function::new(ctx.clone(), wrap_text_with_ansi_native)?,
    )?;
    native.set(
        "sliceByColumn",
        Function::new(
            ctx.clone(),
            |line: String, start: f64, length: f64, strict: Opt<bool>| -> String {
                let to_usize = |value: f64| {
                    if value.is_finite() && value >= 0.0 {
                        value as usize
                    } else {
                        0
                    }
                };
                kiana_tui::text::slice_by_column(
                    &line,
                    to_usize(start),
                    to_usize(length),
                    strict.0.unwrap_or(false),
                )
            },
        )?,
    )?;
    native.set(
        "renderFakeCursor",
        Function::new(ctx.clone(), |text: String| -> String {
            render_fake_cursor(&text)
        })?,
    )?;
    // `compositeTuiLine(baseLine, overlayLine, startCol, overlayWidth, totalWidth)`
    // （`tui.ts:426-468`）：HStack 的水平合成与 overlay 合成共用同一实现。
    native.set(
        "compositeTuiLine",
        Function::new(
            ctx.clone(),
            |base: String,
             overlay: String,
             start_col: f64,
             overlay_width: f64,
             total_width: f64|
             -> String {
                let to_usize = |value: f64| -> usize {
                    if value.is_finite() && value > 0.0 {
                        value as usize
                    } else {
                        0
                    }
                };
                let start = if start_col.is_finite() {
                    start_col as isize
                } else {
                    0
                };
                kiana_tui::tui::composite_tui_line(
                    &base,
                    &overlay,
                    start,
                    to_usize(overlay_width),
                    to_usize(total_width),
                )
            },
        )?,
    )?;
    native.set(
        "matchesKey",
        Function::new(ctx.clone(), |data: String, key: String| -> bool {
            kiana_tui::keys::matches_key(&data, &key)
        })?,
    )?;
    native.set("parseKey", Function::new(ctx.clone(), parse_key_native)?)?;
    native.set(
        "isKeyRelease",
        Function::new(ctx.clone(), |data: String| -> bool {
            kiana_tui::keys::is_key_release(&data)
        })?,
    )?;
    native.set(
        "fuzzyFilter",
        Function::new(ctx.clone(), fuzzy_filter_native)?,
    )?;
    Ok(())
}

/// `wrapTextWithAnsi` 的原生实现（`packages/tui/src/index.ts` 的同名导出）。
fn wrap_text_with_ansi_native<'js>(
    ctx: Ctx<'js>,
    text: String,
    width: f64,
) -> EngineResult<Array<'js>> {
    let width = if width.is_finite() {
        width.max(0.0) as usize
    } else {
        0
    };
    let lines = kiana_tui::text::wrap_text_with_ansi(&text, width);
    lines_to_js(&ctx, lines)
}

/// `parseKey` 的原生实现（`packages/tui/src/keys.ts` 的同名导出）。
fn parse_key_native<'js>(ctx: Ctx<'js>, data: String) -> EngineResult<Value<'js>> {
    match kiana_tui::keys::parse_key(&data) {
        Some(key) => Ok(rquickjs::String::from_str(ctx.clone(), &key)?.into_value()),
        None => Ok(Value::new_undefined(ctx)),
    }
}

/// `fuzzyFilter` 的原生实现（`packages/tui/src/fuzzy.ts` 的同名导出）。
///
/// 查询按空白与 `/` 切成 token，所有 token 都必须模糊匹配同一段文本，得分求和后按升序
/// 稳定排序。`items` 不是数组、`get_text` 不是函数、查询为空时原样返回输入；`get_text`
/// 抛错的那一项被跳过（异常被吞掉）。
fn fuzzy_filter_native<'js>(
    ctx: Ctx<'js>,
    items: Value<'js>,
    query: String,
    get_text: Value<'js>,
) -> EngineResult<Value<'js>> {
    let Some(array) = items.as_array() else {
        return Ok(items);
    };
    let Some(get_text) = get_text.as_function().cloned() else {
        return Ok(items.clone());
    };
    let query = query.trim().to_owned();
    if query.is_empty() {
        return Ok(items.clone());
    }
    let tokens: Vec<String> = query
        .split(|character: char| character.is_whitespace() || character == '/')
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect();
    if tokens.is_empty() {
        return Ok(items.clone());
    }
    // JS `Array.prototype.sort` 是稳定排序；`sort_by` 同样是稳定排序。
    let mut results: Vec<(Value<'js>, f64)> = Vec::new();
    for index in 0..array.len() {
        let item: Value<'js> = array.get(index)?;
        let text: String = match get_text.call((item.clone(),)) {
            Ok(text) => text,
            Err(_) => {
                let _ = ctx.catch();
                continue;
            }
        };
        let mut total = 0.0f64;
        let mut all_match = true;
        for token in &tokens {
            let found = kiana_tui::fuzzy::fuzzy_match(token, &text);
            if found.matches {
                total += found.score;
            } else {
                all_match = false;
                break;
            }
        }
        if all_match {
            results.push((item, total));
        }
    }
    results.sort_by(|left, right| left.1.total_cmp(&right.1));
    let output = Array::new(ctx.clone())?;
    for (index, (value, _)) in results.into_iter().enumerate() {
        output.set(index, value)?;
    }
    Ok(output.into_value())
}

/// `kiana_autocomplete` host kind 名。
pub const AUTOCOMPLETE_HOST_KIND: &str = "kiana_autocomplete";

/// 键位名（`Key` 常量在 `js/ui/tui.js` 里逐字镜像 `packages/tui/src/keys.ts`）。
pub const KEY_ESCAPE: &str = "escape";
