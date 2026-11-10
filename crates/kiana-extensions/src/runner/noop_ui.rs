//! `no_op_ui_context`（print/json 模式的 UI 上下文）。
//!
//! TS 对应物：`packages/coding-agent/src/core/extensions/runner.ts` 的 `noOpUIContext`
//! （`ca-extensions.md` §2.3.6、`design-extension-runtime.md` §4.5）。
//!
//! 这是一个**进程级单例**：`ExtensionRunner::set_ui_context(None, mode)` 安装它，`has_ui()` 用
//! `Arc::ptr_eq` 与 [`no_op_ui_context`] 比较来判断“当前没有 UI”（TS 的
//! `this.uiContext !== noOpUIContext` 引用比较）。`ui/print_json.rs` 也复用它（同一份默认值，
//! 不重复实现）。
//!
//! 逐字默认值（`ca-extensions.md` §2.3.6）：
//!
//! - `select` → `undefined`、`confirm` → `false`、`input` → `undefined`、`editor` → `undefined`；
//! - `custom` → `undefined as never`（`design-extension-runtime.md` §4.5 的 “never 之外的
//!   undefined”）；Rust 的擦除结果是 `Box<dyn Any + Send>` 里的 `()`；
//! - `getEditorText` → `""`、`getEditorComponent` → `undefined`、`getAllThemes` → `[]`、
//!   `getTheme` → `undefined`、`getToolsExpanded` → `false`；
//! - `setTheme` → `{ success: false, error: "UI not available" }`【逐字】；
//! - `onTerminalInput` → 空退订函数；其余全部空操作；
//! - `theme` → 当前全局主题（`kiana_theme::theme::current_theme`，未初始化时回退内置 `dark`，
//!   每次访问都重新读取，与 TS 的 `get theme() { return theme }` 一致）。
//!   注意：TS 返回的是 theme.ts 的稳定 Proxy（未初始化时**属性访问抛**
//!   `Theme not initialized. Call initTheme() first.`）；`ExtensionUiContext::theme` 的冻结签名
//!   是 `&Theme`，无法表达该错误，也无法在主题切换后保持同一引用。JS 侧（`js/context.js` 的
//!   `noopUi.theme`）实现的是 TS 的 Proxy 语义（见 `noop_theme_is_the_stable_ts_proxy`），
//!   因此扩展代码在两条路径下拿到一致的默认行为。
//!
//! `theme` 的 trait 签名是 `&Theme`，无法返回临时 `Arc` 的内部引用；实现里把每次用到的
//! `Arc<Theme>` 追加到进程级历史表（主题切换次数有限），因此切换主题后立即生效，旧的
//! `&Theme` 也不会悬空。JSON 侧“同一个 Theme 原生类实例”的身份由 `ui-adapter` 的 JS 桥保证。
//!
//! # 调用点
//!
//! - [`ExtensionRunner::set_ui_context`](super::ExtensionRunner::set_ui_context)`(None, mode)`
//!   把 runner 的 UI 槽设为 [`no_op_ui_context`]；
//!   [`ExtensionRunner::has_ui`](super::ExtensionRunner::has_ui) 用
//!   [`is_no_op_ui_context`] 做 `Arc` 引用比较。
//! - `ui/print_json.rs` 直接再导出同一组符号，print/json 模式因此共享同一份默认值。
//! - `runner/context.rs` 在没有 UI 时把 `ctx.ui` 物化成纯 JS 的 noop 对象
//!   （`noop_ui_object`），与这里的 Rust 实现是两份代码；两者的可见默认值必须一致。
//!
//! # 约束
//!
//! - 单例必须是**唯一**实例：任何包装（例如
//!   [`UiPromptContext`](super::ui_prompt::UiPromptContext)）都会让 `Arc::ptr_eq` 失败，
//!   从而被判定为“有 UI”（`tests/extension_noop_ui.rs` 锁定）。
//! - 本上下文不做任何 I/O、不持有渲染状态；所有成员要么返回固定默认值，要么空操作。
//!   `theme()` 是唯一按读时求值的成员，其余默认值在调用时现算。
//! - 这里的方法可能在任意持有 `Arc<dyn ExtensionUiContext>` 的线程上被调用（trait 对象是
//!   `Send + Sync`），因此状态全部放在进程级 `LazyLock`/`Mutex` 中，不使用线程本地量。
//! - `global_theme` 的历史表只增不减：每个首次出现的主题 `Arc` 都会永久保留，以保证已经
//!   返回的 `&Theme` 不悬空。主题切换通常很少；但主题文件被 watcher 反复重载时每次都会
//!   产生新的 `Arc`，历史表会随重载次数增长。

use std::sync::{Arc, LazyLock};

use kiana_core::event_bus::Unsubscribe;
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::ui::{
    AutocompleteProviderFactory, CustomFactory, CustomOptions, EditorFactory,
    ExtensionUIDialogOptions, ExtensionUiContext, ExtensionWidgetOptions, FooterFactory,
    HeaderFactory, NotifyKind, TerminalInputHandler, ThemeInfo, ThemeOrName, ThemeResult,
    WidgetContent, WorkingIndicatorOptions,
};
use kiana_theme::theme::Theme;

/// 无 UI 时 `setTheme` 的逐字错误文案。
///
/// 这个字符串同时被 `ui/print_json.rs` 与 `ui/mod.rs` 的 RPC/print 路径复用；
/// `tests/extension_noop_ui.rs` / `tests/print_json_ui.rs` 逐字断言 `"UI not available"`，
/// 修改会同时影响所有无 UI 模式。
pub const UI_NOT_AVAILABLE: &str = "UI not available";

/// 无 UI 上下文（进程级单例）。
///
/// 见模块文档；`ExtensionRunner::has_ui` 通过 `Arc::ptr_eq` 与它比较。
static NO_OP_UI: LazyLock<Arc<dyn ExtensionUiContext>> =
    LazyLock::new(|| Arc::new(NoOpUiContext) as Arc<dyn ExtensionUiContext>);

/// 取得进程级的 no-op UI 上下文（`Arc` 身份稳定）。
///
/// 每次返回同一个 `Arc` 的克隆：指针相等，因此可以安全地与 [`is_no_op_ui_context`] 配合
/// 判定“当前会话没有 UI”。调用方不要用 `Arc::new(NoOp...)` 重建——那会是一个新身份，
/// `hasUI()` 会误判为有 UI。
pub fn no_op_ui_context() -> Arc<dyn ExtensionUiContext> {
    Arc::clone(&NO_OP_UI)
}

/// 对象是否就是进程级 no-op 上下文（`hasUI()` 的引用比较）。
///
/// 只对 [`no_op_ui_context`] 返回的那个实例为 `true`；任何包装器（即使内部委托给它）都
/// 返回 `false`。这正是 TS `this.uiContext !== noOpUIContext` 的语义。
pub fn is_no_op_ui_context(ui: &Arc<dyn ExtensionUiContext>) -> bool {
    Arc::ptr_eq(ui, &NO_OP_UI)
}

/// 当前全局主题（TS 的 `get theme()` 每次读取全局代理）。
///
/// `ExtensionUiContext::theme` 的签名是 `&Theme`，无法返回临时 `Arc` 的内部引用；这里每次
/// 访问都重新读取 [`kiana_theme::theme::current_theme`]，并把用到的 `Arc` 追加到进程级历史表
/// （主题切换次数有限；历史表保证旧引用不会悬空），从而与 TS 一样“切主题后立即生效”，
/// 而不是首次访问的永久快照。
fn global_theme() -> &'static Theme {
    static FALLBACK_THEME: LazyLock<Arc<Theme>> = LazyLock::new(|| {
        kiana_theme::theme::get_theme_by_name("dark")
            .expect("the builtin dark theme must always be available")
    });
    static HISTORY: LazyLock<std::sync::Mutex<Vec<Arc<Theme>>>> =
        LazyLock::new(|| std::sync::Mutex::new(Vec::new()));
    static CURRENT: LazyLock<std::sync::Mutex<Option<Arc<Theme>>>> =
        LazyLock::new(|| std::sync::Mutex::new(None));

    let theme = match kiana_theme::theme::current_theme() {
        Some(theme) => theme,
        // 未初始化时回退内置 dark；缓存同一份 Arc，未设置主题前引用身份稳定。
        None => Arc::clone(&FALLBACK_THEME),
    };
    // `CURRENT` 只保存“最近一次返回的 Arc”（用于判断主题是否切换），`HISTORY` 追加保存
    // 所有返回过的 Arc，保证已经交出去的 `&Theme` 在主题切换后仍然有效。两个锁都只做指针
    // 读写，中毒时恢复内层数据不会破坏不变量。
    let mut current = CURRENT.lock().unwrap_or_else(|error| error.into_inner());
    let same = current
        .as_ref()
        .is_some_and(|existing| Arc::ptr_eq(existing, &theme));
    if !same {
        *current = Some(Arc::clone(&theme));
        HISTORY
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(theme);
    }
    let pointer = Arc::as_ptr(current.as_ref().expect("theme just stored"));
    // SAFETY: 该 Arc 同时保存在 HISTORY（进程级、从不清理）里，因此 `Theme` 活到进程结束。
    unsafe { &*pointer }
}

/// `noOpUIContext` 的实现。
struct NoOpUiContext;

impl ExtensionUiContext for NoOpUiContext {
    fn select(
        &self,
        _title: &str,
        _options: &[String],
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }

    fn confirm(
        &self,
        _title: &str,
        _message: &str,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, bool> {
        Box::pin(async { false })
    }

    fn input(
        &self,
        _title: &str,
        _placeholder: Option<&str>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }

    fn notify(&self, _message: &str, _kind: Option<NotifyKind>) {}

    fn on_terminal_input(&self, _handler: TerminalInputHandler) -> Unsubscribe {
        Box::new(|| {})
    }

    fn set_status(&self, _key: &str, _text: Option<&str>) {}

    fn set_working_message(&self, _message: Option<&str>) {}

    fn set_working_visible(&self, _visible: bool) {}

    fn set_working_indicator(&self, _options: Option<WorkingIndicatorOptions>) {}

    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}

    fn set_widget(
        &self,
        _key: &str,
        _content: Option<WidgetContent>,
        _options: Option<ExtensionWidgetOptions>,
    ) {
    }

    fn set_footer(&self, _factory: Option<FooterFactory>) {}

    fn set_header(&self, _factory: Option<HeaderFactory>) {}

    fn set_title(&self, _title: &str) {}

    fn custom(
        &self,
        _factory: CustomFactory,
        _options: Option<CustomOptions>,
    ) -> LocalBoxFuture<'_, Box<dyn std::any::Any + Send>> {
        // TS: `async () => undefined as never`。
        Box::pin(async { Box::new(()) as Box<dyn std::any::Any + Send> })
    }

    fn paste_to_editor(&self, _text: &str) {}

    fn set_editor_text(&self, _text: &str) {}

    fn get_editor_text(&self) -> String {
        String::new()
    }

    fn editor(&self, _title: &str, _prefill: Option<&str>) -> LocalBoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }

    fn add_autocomplete_provider(&self, _factory: AutocompleteProviderFactory) {}

    fn set_editor_component(&self, _factory: Option<EditorFactory>) {}

    fn get_editor_component(&self) -> Option<EditorFactory> {
        None
    }

    fn theme(&self) -> &Theme {
        global_theme()
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        Vec::new()
    }

    fn get_theme(&self, _name: &str) -> Option<Theme> {
        None
    }

    fn set_theme(&self, _theme: ThemeOrName) -> ThemeResult {
        ThemeResult {
            success: false,
            error: Some(UI_NOT_AVAILABLE.to_owned()),
        }
    }

    fn get_tools_expanded(&self) -> bool {
        false
    }

    fn set_tools_expanded(&self, _expanded: bool) {}
}
