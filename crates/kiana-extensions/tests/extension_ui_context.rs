//! `ctx.ui` 桥：JS 对象 ↔ `Arc<dyn ExtensionUiContext>`（设计文档 §4.5、§5.6）。
//!
//! 用一个逐成员记录的 fake UI 上下文，从 JS 侧调用全部 28 个成员，断言：
//! - 异步对话框经 `__kiana.promise("kiana_ui", …)` 往返，`signal`/`timeout` 生效；
//! - 同步成员逐一转发（notify kind、working 指示器、标题、终端输入退订）；
//! - `setWidget` 三个重载（数组原样转发；`undefined` 保留 placement；非数组内容不转发）；
//!   footer/header/editor 工厂拿到原生 TUI/主题/keybindings/footerData，返回的 JS 组件可渲染；
//! - `custom` 的工厂、静态/动态 overlay 选项与 `onHandle`；
//! - 主题成员（同一原生实例、全部 token、get/setTheme）；
//! - autocomplete provider 工厂与 getSuggestions 的 Promise 桥。

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kiana_core::event_bus::Unsubscribe;
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::ui::{
    AutocompleteProviderFactory, CustomDoneFn, CustomFactory, CustomOptions, EditorFactory,
    ExtensionUIDialogOptions, ExtensionUiContext, ExtensionWidgetOptions, FooterFactory,
    HeaderFactory, NotifyKind, OverlayOptionsOrFn, ReadonlyFooterDataProvider,
    TerminalInputHandler, ThemeInfo, ThemeOrName, ThemeResult, WidgetContent,
    WorkingIndicatorOptions,
};
use kiana_theme::theme::Theme;
use kiana_tui::TuiMainScreen;
use kiana_tui::autocomplete::{
    ApplyCompletionResult, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions,
    GetSuggestionsOptions,
};
use kiana_tui::terminal_colors::{ProgramStatus, Terminal};
use kiana_tui::tui::{FocusTarget, OverlayBounds, OverlayHandle, OverlayUnfocusOptions, Tui};
mod support;
use kiana_config::keybindings::KeybindingsManager;
use kiana_extensions::engine::{self, EngineConfig, JsEngine};
use kiana_extensions::runner::context::register_ui_context;
use support::run_local;

// ===========================================================================
// fake 终端 / TUI
// ===========================================================================

/// 一次 `set_widget` 调用。
type WidgetCall = (
    String,
    Option<WidgetContent>,
    Option<ExtensionWidgetOptions>,
);

struct FakeTerminal {
    writes: Mutex<String>,
    titles: Mutex<Vec<String>>,
    rows: usize,
}

impl Default for FakeTerminal {
    fn default() -> Self {
        FakeTerminal {
            writes: Mutex::new(String::new()),
            titles: Mutex::new(Vec::new()),
            rows: 24,
        }
    }
}

impl Terminal for FakeTerminal {
    fn start(&mut self, _on_input: Box<dyn FnMut(&str)>, _on_resize: Box<dyn FnMut()>) {}
    fn stop(&mut self) {}
    fn drain_input(
        &mut self,
        _max_ms: u64,
        _idle_ms: u64,
    ) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        Box::pin(async {})
    }
    fn write(&mut self, data: &str) {
        self.writes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push_str(data);
    }
    fn columns(&self) -> usize {
        80
    }
    fn rows(&self) -> usize {
        self.rows
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&mut self, _lines: isize) {}
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn set_title(&mut self, title: &str) {
        self.titles
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(title.to_owned());
    }
    fn set_progress(&mut self, _active: bool) {}
    fn set_program_status(&mut self, _status: &ProgramStatus) {}
}

/// `Arc<dyn Tui>` 的 `Send + Sync` 包装（`Tui` 是 `!Send`；测试单线程访问）。
#[derive(Clone)]
struct TuiHandle(Arc<dyn Tui>);

// SAFETY: 仅在创建 TUI 的测试线程读取；没有跨线程共享。
unsafe impl Send for TuiHandle {}
unsafe impl Sync for TuiHandle {}

#[allow(clippy::arc_with_non_send_sync)]
fn fake_tui_with_rows(rows: usize) -> Arc<dyn Tui> {
    Arc::new(TuiMainScreen::new(Box::new(FakeTerminal {
        rows,
        ..FakeTerminal::default()
    })))
}

#[allow(clippy::arc_with_non_send_sync)]
fn fake_tui() -> Arc<dyn Tui> {
    fake_tui_with_rows(24)
}

// ===========================================================================
// fake footer data / overlay handle / provider
// ===========================================================================

#[derive(Default)]
struct FakeFooterData {
    statuses: indexmap::IndexMap<String, String>,
}

impl ReadonlyFooterDataProvider for FakeFooterData {
    fn get_git_branch(&self) -> Option<String> {
        Some("main".to_owned())
    }
    fn get_extension_statuses(&self) -> &indexmap::IndexMap<String, String> {
        &self.statuses
    }
    fn get_available_provider_count(&self) -> usize {
        3
    }
    fn on_branch_change(&self, _callback: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        Box::new(|| {})
    }
}

/// 可变的 footer 数据（模拟 `setStatus()` 之后同一个 `ReadonlyMap` 立即变化）。
///
/// `ReadonlyFooterDataProvider::get_extension_statuses` 返回 `&IndexMap`，只能靠内部可变性
/// 模拟宿主更新；测试是单线程的，`UnsafeCell` + `unsafe impl Send/Sync` 足够。
#[derive(Default)]
struct LiveFooterData {
    statuses: std::cell::UnsafeCell<indexmap::IndexMap<String, String>>,
}

// SAFETY: 测试单线程访问；没有跨线程共享同一实例。
unsafe impl Send for LiveFooterData {}
unsafe impl Sync for LiveFooterData {}

impl LiveFooterData {
    fn set(&self, key: &str, value: &str) {
        // SAFETY: 见上（测试单线程）。
        unsafe {
            (*self.statuses.get()).insert(key.to_owned(), value.to_owned());
        }
    }
}

impl ReadonlyFooterDataProvider for LiveFooterData {
    fn get_git_branch(&self) -> Option<String> {
        Some("main".to_owned())
    }
    fn get_extension_statuses(&self) -> &indexmap::IndexMap<String, String> {
        // SAFETY: 见上（测试单线程）。
        unsafe { &*self.statuses.get() }
    }
    fn get_available_provider_count(&self) -> usize {
        0
    }
    fn on_branch_change(&self, _callback: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        Box::new(|| {})
    }
}

/// 不在仓库中的 footer 数据（`getGitBranch()` 返回 `None`）。
struct NoRepoFooterData;

impl ReadonlyFooterDataProvider for NoRepoFooterData {
    fn get_git_branch(&self) -> Option<String> {
        None
    }
    fn get_extension_statuses(&self) -> &indexmap::IndexMap<String, String> {
        static EMPTY: std::sync::LazyLock<indexmap::IndexMap<String, String>> =
            std::sync::LazyLock::new(indexmap::IndexMap::new);
        &EMPTY
    }
    fn get_available_provider_count(&self) -> usize {
        0
    }
    fn on_branch_change(&self, _callback: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        Box::new(|| {})
    }
}

#[derive(Default)]
struct FakeOverlayHandle {
    hidden: bool,
    focused: bool,
    hidden_calls: Arc<AtomicUsize>,
}

impl OverlayHandle for FakeOverlayHandle {
    fn hide(&mut self) {
        self.hidden_calls.fetch_add(1, Ordering::SeqCst);
    }
    fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
    }
    fn is_hidden(&self) -> bool {
        self.hidden
    }
    fn focus(&mut self) {
        self.focused = true;
    }
    fn unfocus(&mut self, _options: Option<OverlayUnfocusOptions>) {
        self.focused = false;
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn bounds(&self) -> Option<OverlayBounds> {
        Some(OverlayBounds {
            row: 2,
            col: 3,
            width: 40,
            height: 10,
        })
    }
}

struct BaseProvider;

impl AutocompleteProvider for BaseProvider {
    fn trigger_characters(&self) -> Option<Vec<char>> {
        Some(vec!['!'])
    }
    fn get_suggestions<'a>(
        &'a mut self,
        _lines: &'a [String],
        _cursor_line: usize,
        _cursor_col: usize,
        _options: &'a GetSuggestionsOptions,
    ) -> Pin<Box<dyn Future<Output = Option<AutocompleteSuggestions>> + Send + 'a>> {
        Box::pin(async { None })
    }
    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        _item: &AutocompleteItem,
        _prefix: &str,
    ) -> ApplyCompletionResult {
        ApplyCompletionResult {
            lines: lines.to_vec(),
            cursor_line,
            cursor_col,
        }
    }
}

// ===========================================================================
// fake UI 上下文
// ===========================================================================

struct FakeUi {
    log: Mutex<Vec<String>>,
    dialog_calls: AtomicUsize,
    select_result: Mutex<Option<String>>,
    confirm_result: Mutex<bool>,
    input_result: Mutex<Option<String>>,
    editor_result: Mutex<Option<String>>,
    last_dialog_options: Mutex<Option<ExtensionUIDialogOptions>>,
    dialog_options_log: Mutex<Vec<(String, Option<ExtensionUIDialogOptions>)>>,
    last_select: Mutex<Option<(String, Vec<String>)>>,
    editor_text: Mutex<String>,
    widgets: Mutex<Vec<WidgetCall>>,
    footer: Mutex<Option<FooterFactory>>,
    header: Mutex<Option<HeaderFactory>>,
    editor_factory: Mutex<Option<EditorFactory>>,
    autocomplete_factory: Mutex<Option<AutocompleteProviderFactory>>,
    terminal_handler: Mutex<Option<TerminalInputHandler>>,
    unsubscribe_calls: Arc<AtomicUsize>,
    custom_overlay: Mutex<Option<bool>>,
    custom_overlay_summary: Mutex<Option<String>>,
    tools_expanded: Mutex<bool>,
    theme: Arc<Theme>,
    set_theme_calls: Mutex<Vec<String>>,
    theme_result: Mutex<Option<ThemeResult>>,
    custom_lines: Arc<Mutex<Vec<String>>>,
    on_handle_calls: Arc<AtomicUsize>,
    last_tui: Mutex<Option<TuiHandle>>,
    terminal_rows: Mutex<Option<usize>>,
}

impl Default for FakeUi {
    fn default() -> Self {
        FakeUi {
            log: Mutex::new(Vec::new()),
            dialog_calls: AtomicUsize::new(0),
            select_result: Mutex::new(None),
            confirm_result: Mutex::new(false),
            input_result: Mutex::new(None),
            editor_result: Mutex::new(None),
            last_dialog_options: Mutex::new(None),
            dialog_options_log: Mutex::new(Vec::new()),
            last_select: Mutex::new(None),
            editor_text: Mutex::new(String::new()),
            widgets: Mutex::new(Vec::new()),
            footer: Mutex::new(None),
            header: Mutex::new(None),
            editor_factory: Mutex::new(None),
            autocomplete_factory: Mutex::new(None),
            terminal_handler: Mutex::new(None),
            unsubscribe_calls: Arc::new(AtomicUsize::new(0)),
            custom_overlay: Mutex::new(None),
            custom_overlay_summary: Mutex::new(None),
            tools_expanded: Mutex::new(false),
            theme: kiana_extensions::ui::current_global_theme(),
            set_theme_calls: Mutex::new(Vec::new()),
            theme_result: Mutex::new(None),
            custom_lines: Arc::new(Mutex::new(Vec::new())),
            on_handle_calls: Arc::new(AtomicUsize::new(0)),
            last_tui: Mutex::new(None),
            terminal_rows: Mutex::new(None),
        }
    }
}

impl FakeUi {
    fn new() -> Self {
        let ui = FakeUi::default();
        *ui.select_result.lock().unwrap() = Some("two".to_owned());
        *ui.confirm_result.lock().unwrap() = true;
        *ui.input_result.lock().unwrap() = Some("typed".to_owned());
        *ui.editor_result.lock().unwrap() = Some("edited".to_owned());
        *ui.theme_result.lock().unwrap() = Some(ThemeResult {
            success: true,
            error: None,
        });
        ui
    }

    fn log(&self, entry: impl Into<String>) {
        self.log
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(entry.into());
    }

    fn entries(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn theme_arc(&self) -> Arc<Theme> {
        Arc::clone(&self.theme)
    }
}

impl ExtensionUiContext for FakeUi {
    fn select(
        &self,
        title: &str,
        options: &[String],
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        self.dialog_calls.fetch_add(1, Ordering::SeqCst);
        *self
            .last_select
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            Some((title.to_owned(), options.to_vec()));
        *self
            .last_dialog_options
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = opts.clone();
        self.dialog_options_log
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(("select".to_owned(), opts));
        self.log(format!("select:{title}:{}", options.join("|")));
        let result = self
            .select_result
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        Box::pin(async move { result })
    }

    fn confirm(
        &self,
        title: &str,
        message: &str,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, bool> {
        self.dialog_calls.fetch_add(1, Ordering::SeqCst);
        *self
            .last_dialog_options
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = opts.clone();
        self.dialog_options_log
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(("confirm".to_owned(), opts));
        self.log(format!("confirm:{title}:{message}"));
        let result = *self
            .confirm_result
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Box::pin(async move { result })
    }

    fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        self.dialog_calls.fetch_add(1, Ordering::SeqCst);
        *self
            .last_dialog_options
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = opts.clone();
        self.dialog_options_log
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(("input".to_owned(), opts));
        self.log(format!("input:{title}:{}", placeholder.unwrap_or("<none>")));
        let result = self
            .input_result
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        Box::pin(async move { result })
    }

    fn notify(&self, message: &str, kind: Option<NotifyKind>) {
        let kind = match kind.unwrap_or_default() {
            NotifyKind::Info => "info",
            NotifyKind::Warning => "warning",
            NotifyKind::Error => "error",
        };
        self.log(format!("notify:{kind}:{message}"));
    }

    fn on_terminal_input(&self, handler: TerminalInputHandler) -> Unsubscribe {
        *self
            .terminal_handler
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(handler);
        let calls = Arc::clone(&self.unsubscribe_calls);
        self.log("onTerminalInput");
        Box::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        self.log(format!("setStatus:{key}:{}", text.unwrap_or("<none>")));
    }

    fn set_working_message(&self, message: Option<&str>) {
        self.log(format!("setWorkingMessage:{}", message.unwrap_or("<none>")));
    }

    fn set_working_visible(&self, visible: bool) {
        self.log(format!("setWorkingVisible:{visible}"));
    }

    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>) {
        let frames = options
            .as_ref()
            .and_then(|options| options.frames.clone())
            .map(|frames| frames.join(","))
            .unwrap_or_else(|| "<none>".to_owned());
        let interval = options
            .as_ref()
            .and_then(|options| options.interval_ms)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "<none>".to_owned());
        self.log(format!("setWorkingIndicator:{frames}:{interval}"));
    }

    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        self.log(format!(
            "setHiddenThinkingLabel:{}",
            label.unwrap_or("<none>")
        ));
    }

    fn set_widget(
        &self,
        key: &str,
        content: Option<WidgetContent>,
        options: Option<ExtensionWidgetOptions>,
    ) {
        let placement = options
            .as_ref()
            .and_then(|options| options.placement)
            .map(|placement| placement.as_str().to_owned())
            .unwrap_or_else(|| "<none>".to_owned());
        let kind = match &content {
            Some(WidgetContent::Lines(lines)) => format!("lines:{}", lines.join("|")),
            Some(WidgetContent::Component(_)) => "component".to_owned(),
            None => "clear".to_owned(),
        };
        self.log(format!("setWidget:{key}:{kind}:{placement}"));
        self.widgets
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((key.to_owned(), content, options));
    }

    fn set_footer(&self, factory: Option<FooterFactory>) {
        self.log(format!("setFooter:{}", factory.is_some()));
        *self
            .footer
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = factory;
    }

    fn set_header(&self, factory: Option<HeaderFactory>) {
        self.log(format!("setHeader:{}", factory.is_some()));
        *self
            .header
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = factory;
    }

    fn set_title(&self, title: &str) {
        self.log(format!("setTitle:{title}"));
    }

    fn custom(
        &self,
        factory: CustomFactory,
        options: Option<CustomOptions>,
    ) -> LocalBoxFuture<'_, Box<dyn Any + Send>> {
        let tui = match *self
            .terminal_rows
            .lock()
            .unwrap_or_else(|error| error.into_inner())
        {
            Some(rows) => fake_tui_with_rows(rows),
            None => fake_tui(),
        };
        *self
            .last_tui
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(TuiHandle(Arc::clone(&tui)));
        let theme = self.theme_arc();
        let keybindings = Arc::new(KeybindingsManager::new(Default::default(), None));
        let on_handle_calls = Arc::clone(&self.on_handle_calls);
        let recorded_lines = Arc::clone(&self.custom_lines);
        let on_handle = options
            .as_ref()
            .and_then(|options| options.on_handle.clone());
        let overlay_record = options.as_ref().and_then(|options| options.overlay);
        let summary = match options
            .as_ref()
            .and_then(|options| options.overlay_options.as_ref())
        {
            Some(OverlayOptionsOrFn::Dynamic(_)) => "dynamic".to_owned(),
            Some(OverlayOptionsOrFn::Static(overlay)) => {
                let visible = overlay
                    .visible
                    .as_ref()
                    .map(|visible| visible(80, 24))
                    .unwrap_or(false);
                format!(
                    "static:width={:?}:maxHeight={:?}:anchor={:?}:visible={visible}",
                    overlay.width, overlay.max_height, overlay.anchor
                )
            }
            None => "none".to_owned(),
        };
        *self
            .custom_overlay
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = overlay_record;
        *self
            .custom_overlay_summary
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(summary.clone());
        self.log(format!(
            "custom:overlay={}:{summary}",
            overlay_record.unwrap_or(false)
        ));
        Box::pin(async move {
            let (sender, receiver) = futures::channel::oneshot::channel::<Box<dyn Any + Send>>();
            let sender = Mutex::new(Some(sender));
            let done: CustomDoneFn = Arc::new(move |value| {
                if let Some(sender) = sender
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                {
                    let _ = sender.send(value);
                }
            });
            let mut component = factory(tui, &theme, keybindings, done).await;
            *recorded_lines
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = component.render(60);
            if let Some(on_handle) = on_handle {
                on_handle_calls.fetch_add(1, Ordering::SeqCst);
                on_handle(Box::<FakeOverlayHandle>::default());
            }
            match receiver.await {
                Ok(value) => value,
                Err(_) => Box::new(()),
            }
        })
    }

    fn paste_to_editor(&self, text: &str) {
        self.log(format!("pasteToEditor:{text}"));
    }

    fn set_editor_text(&self, text: &str) {
        *self
            .editor_text
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = text.to_owned();
        self.log(format!("setEditorText:{text}"));
    }

    fn get_editor_text(&self) -> String {
        self.editor_text
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn editor(&self, title: &str, prefill: Option<&str>) -> LocalBoxFuture<'_, Option<String>> {
        self.dialog_calls.fetch_add(1, Ordering::SeqCst);
        self.dialog_options_log
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(("editor".to_owned(), None));
        self.log(format!("editor:{title}:{}", prefill.unwrap_or("<none>")));
        let result = self
            .editor_result
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        Box::pin(async move { result })
    }

    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory) {
        self.log("addAutocompleteProvider");
        *self
            .autocomplete_factory
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(factory);
    }

    fn set_editor_component(&self, factory: Option<EditorFactory>) {
        self.log(format!("setEditorComponent:{}", factory.is_some()));
        *self
            .editor_factory
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = factory;
    }

    fn get_editor_component(&self) -> Option<EditorFactory> {
        self.editor_factory
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn theme(&self) -> &Theme {
        &self.theme
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        vec![
            ThemeInfo {
                name: "dark".to_owned(),
                path: None,
            },
            ThemeInfo {
                name: "light".to_owned(),
                path: Some("/themes/light.json".to_owned()),
            },
        ]
    }

    fn get_theme(&self, name: &str) -> Option<Theme> {
        self.log(format!("getTheme:{name}"));
        None
    }

    fn set_theme(&self, theme: ThemeOrName) -> ThemeResult {
        let label = match &theme {
            ThemeOrName::Name(name) => name.clone(),
            ThemeOrName::Theme(theme) => format!("<theme:{}>", theme.name().unwrap_or("?")),
        };
        self.set_theme_calls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(label.clone());
        self.log(format!("setTheme:{label}"));
        self.theme_result
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .unwrap_or_default()
    }

    fn get_tools_expanded(&self) -> bool {
        *self
            .tools_expanded
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn set_tools_expanded(&self, expanded: bool) {
        *self
            .tools_expanded
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = expanded;
        self.log(format!("setToolsExpanded:{expanded}"));
    }
}

// ===========================================================================
// harness
// ===========================================================================

struct Fixture {
    engine: Rc<JsEngine>,
    ui: Arc<FakeUi>,
    handle: u64,
}

fn fixture() -> Fixture {
    let engine = Rc::new(JsEngine::with_config(EngineConfig::new()).expect("engine"));
    engine::install(&engine).expect("prelude");
    kiana_extensions::ui::install_attached(&engine).expect("ui adapter");
    let ui = Arc::new(FakeUi::new());
    let handle = register_ui_context(Arc::clone(&ui) as Arc<dyn ExtensionUiContext>);
    Fixture { engine, ui, handle }
}

impl Fixture {
    fn eval<T>(&self, source: &str) -> T
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.engine
            .eval_script_error::<T>(source, "<ui-test>")
            .unwrap_or_else(|error| panic!("eval: {}\n{:?}", error.message, error.stack))
    }

    /// 求值成 JSON（`serde_json::Value` 没有 `FromJs`）。
    fn eval_json(&self, expression: &str) -> serde_json::Value {
        let text: String = self.eval(&format!("JSON.stringify({expression})"));
        serde_json::from_str(&text).expect("parse json")
    }
}

fn make_ui(fixture: &Fixture) -> String {
    format!("globalThis.__ui = __kiana_ctx_make_ui({});", fixture.handle)
}

// ===========================================================================
// 成员表
// ===========================================================================

#[test]
fn the_bridge_exposes_exactly_the_28_ts_members() {
    let fixture = fixture();
    fixture.eval::<()>(&make_ui(&fixture));
    let keys: Vec<String> = fixture.eval("Object.keys(globalThis.__ui).sort()");
    let mut expected = vec![
        "addAutocompleteProvider",
        "confirm",
        "custom",
        "editor",
        "getAllThemes",
        "getEditorComponent",
        "getEditorText",
        "getTheme",
        "getToolsExpanded",
        "input",
        "notify",
        "onTerminalInput",
        "pasteToEditor",
        "select",
        "setEditorComponent",
        "setEditorText",
        "setFooter",
        "setHeader",
        "setHiddenThinkingLabel",
        "setStatus",
        "setTheme",
        "setTitle",
        "setToolsExpanded",
        "setWidget",
        "setWorkingIndicator",
        "setWorkingMessage",
        "setWorkingVisible",
        "theme",
    ];
    expected.sort();
    assert_eq!(keys, expected);
    // `theme` 是 getter（不是方法）。
    let theme_kind: String = fixture.eval("typeof __ui.theme");
    assert_eq!(theme_kind, "object");
}

#[test]
fn no_op_and_unknown_handles_return_undefined() {
    let fixture = fixture();
    let noop_handle = register_ui_context(kiana_extensions::runner::noop_ui::no_op_ui_context());
    let kind: String = fixture.eval(&format!(
        "String(typeof __kiana_ctx_make_ui({noop_handle})) + '/' + String(typeof __kiana_ctx_make_ui(987654))"
    ));
    assert_eq!(kind, "undefined/undefined");
}

// ===========================================================================
// 同步成员
// ===========================================================================

#[test]
fn sync_members_are_forwarded() {
    let fixture = fixture();
    fixture.eval::<()>(&make_ui(&fixture));
    fixture.eval::<()>(
        r#"
        __ui.notify("hello");
        __ui.notify("careful", "warning");
        __ui.notify("broken", "error");
        __ui.setStatus("k", "v");
        __ui.setStatus("k", undefined);
        __ui.setWorkingMessage("working");
        __ui.setWorkingMessage(undefined);
        __ui.setWorkingVisible(false);
        __ui.setWorkingIndicator({ frames: ["a", "b"], intervalMs: 120 });
        __ui.setWorkingIndicator(undefined);
        __ui.setHiddenThinkingLabel("hidden");
        __ui.setTitle("title!");
        __ui.pasteToEditor("pasted");
        __ui.setEditorText("abc");
        globalThis.__editorText = __ui.getEditorText();
        globalThis.__tools = [__ui.getToolsExpanded(), (__ui.setToolsExpanded(true), __ui.getToolsExpanded())];
        "#,
    );
    let entries = fixture.ui.entries();
    assert!(
        entries.contains(&"notify:info:hello".to_owned()),
        "{entries:?}"
    );
    assert!(entries.contains(&"notify:warning:careful".to_owned()));
    assert!(entries.contains(&"notify:error:broken".to_owned()));
    assert!(entries.contains(&"setStatus:k:v".to_owned()));
    assert!(entries.contains(&"setStatus:k:<none>".to_owned()));
    assert!(entries.contains(&"setWorkingMessage:working".to_owned()));
    assert!(entries.contains(&"setWorkingMessage:<none>".to_owned()));
    assert!(entries.contains(&"setWorkingVisible:false".to_owned()));
    assert!(entries.contains(&"setWorkingIndicator:a,b:120".to_owned()));
    assert!(entries.contains(&"setWorkingIndicator:<none>:<none>".to_owned()));
    assert!(entries.contains(&"setHiddenThinkingLabel:hidden".to_owned()));
    assert!(entries.contains(&"setTitle:title!".to_owned()));
    assert!(entries.contains(&"pasteToEditor:pasted".to_owned()));
    assert!(entries.contains(&"setEditorText:abc".to_owned()));
    assert!(entries.contains(&"setToolsExpanded:true".to_owned()));
    let editor_text: String = fixture.eval("globalThis.__editorText");
    assert_eq!(editor_text, "abc");
    let tools: Vec<bool> = fixture.eval("globalThis.__tools");
    assert_eq!(tools, vec![false, true]);
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_input_handler_round_trips_and_unsubscribes() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            globalThis.__seen = [];
            globalThis.__unsubscribe = __ui.onTerminalInput((data) => {
                __seen.push(data);
                return data === "drop" ? { consume: true, data: "rewritten" } : undefined;
            });
            "#,
        );
        let handler = fixture
            .ui
            .terminal_handler
            .lock()
            .unwrap()
            .clone()
            .expect("handler");
        assert_eq!(handler("abc"), None);
        let result = handler("drop").expect("result");
        assert_eq!(result.consume, Some(true));
        assert_eq!(result.data.as_deref(), Some("rewritten"));
        let seen: Vec<String> = fixture.eval("globalThis.__seen");
        assert_eq!(seen, vec!["abc".to_owned(), "drop".to_owned()]);
        fixture.eval::<()>("__unsubscribe(); __unsubscribe();");
        assert_eq!(fixture.ui.unsubscribe_calls.load(Ordering::SeqCst), 2);
    })
    .await;
}

// ===========================================================================
// 对话框
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn dialogs_round_trip_with_signal_and_timeout() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            globalThis.__out = {};
            globalThis.__controller = new AbortController();
            __ui.select("Pick", ["one", "two"], { signal: __controller.signal, timeout: 250 })
              .then((v) => { __out.select = v; });
            __ui.confirm("Sure?", "really", { timeout: 10 }).then((v) => { __out.confirm = v; });
            __ui.input("Name", "placeholder", {}).then((v) => { __out.input = v; });
            __ui.editor("Body", "prefill").then((v) => { __out.editor = v; });
            "#,
        );
        let harness = TestDrive::new(&fixture);
        assert!(harness.settle().await);
        let out: serde_json::Value = fixture.eval_json("globalThis.__out");
        assert_eq!(out["select"], "two");
        assert_eq!(out["confirm"], true);
        assert_eq!(out["input"], "typed");
        assert_eq!(out["editor"], "edited");
        let entries = fixture.ui.entries();
        assert!(
            entries.contains(&"select:Pick:one|two".to_owned()),
            "{entries:?}"
        );
        assert!(entries.contains(&"confirm:Sure?:really".to_owned()));
        assert!(entries.contains(&"input:Name:placeholder".to_owned()));
        assert!(entries.contains(&"editor:Body:prefill".to_owned()));
        let log = fixture.ui.dialog_options_log.lock().unwrap().clone();
        let select_options = log
            .iter()
            .find(|(op, _)| op == "select")
            .and_then(|(_, options)| options.clone())
            .expect("select options");
        assert_eq!(select_options.timeout, Some(250.0));
        assert!(select_options.signal.is_some(), "signal must be linked");
        let confirm_options = log
            .iter()
            .find(|(op, _)| op == "confirm")
            .and_then(|(_, options)| options.clone())
            .expect("confirm options");
        assert_eq!(confirm_options.timeout, Some(10.0));
        assert!(confirm_options.signal.is_none());
        let input_options = log
            .iter()
            .find(|(op, _)| op == "input")
            .and_then(|(_, options)| options.clone())
            .expect("input options");
        assert_eq!(input_options.timeout, None);
        // 原生 linkSignal 登记过 select 的 signal（token 在 host 侧被消费）。
        let token_cancelled: bool =
            fixture.eval("(() => { __controller.abort(); return __controller.signal.aborted; })()");
        assert!(token_cancelled);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn pre_aborted_signal_still_reaches_the_host_bridge() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            globalThis.__out = {};
            const controller = new AbortController();
            controller.abort();
            __ui.select("Pick", ["one"], { signal: controller.signal }).then((v) => { __out.select = String(v); });
            __ui.confirm("Sure?", "yes", { signal: controller.signal }).then((v) => { __out.confirm = v; });
            __ui.input("Name", undefined, { signal: controller.signal }).then((v) => { __out.input = String(v); });
            "#,
        );
        let drive = TestDrive::new(&fixture);
        assert!(drive.settle().await);
        // TS 的早退在 mode 实现里（`withUIPrompt` 内部），不在 JS 桥：JS 桥照常把调用交给
        // UI 上下文（`UiPromptContext` 随后发出 `ui_prompt_start`/`ui_prompt_end` 并立即完成）。
        // 这里的 host 是裸测试替身（没有 UiPromptContext 包装），因此按替身的返回值完成。
        assert_eq!(fixture.ui.dialog_calls.load(Ordering::SeqCst), 3);
        // 预中止的 signal 必须以“已取消 token”到达宿主（ui.js `linkSignal` 的 -1），
        // 否则 RPC 的 `createDialogPromise` 早退条件看不到它，会发出 TS 从不发送的请求。
        let log = fixture.ui.dialog_options_log.lock().unwrap().clone();
        assert_eq!(log.len(), 3);
        for (op, options) in &log {
            let signal = options
                .as_ref()
                .and_then(|options| options.signal.as_ref())
                .unwrap_or_else(|| panic!("{op}: pre-aborted signal must reach the host"));
            assert!(signal.is_cancelled(), "{op}: token must already be cancelled");
        }
    })
    .await;
}

// ===========================================================================
// 主题
// ===========================================================================

#[test]
fn theme_members_use_the_same_native_instance() {
    let fixture = fixture();
    fixture.eval::<()>(&make_ui(&fixture));
    // 全局 symbol 与 ctx.ui.theme 是同一个实例。
    let same: bool = fixture.eval(
        r#"
        globalThis.__ui.theme === globalThis[Symbol.for("@earendil-works/kiana-coding-agent:theme")] &&
        globalThis.__ui.theme === globalThis[Symbol.for("@mariozechner/kiana-coding-agent:theme")]
        "#,
    );
    assert!(same);

    let theme = fixture.ui.theme_arc();
    let mut checked = 0;
    for color in kiana_theme::ThemeColor::ALL {
        let token = color.as_str();
        let expected = theme.get_fg_ansi(color);
        let actual: String = fixture.eval(&format!("globalThis.__ui.theme.getFgAnsi({token:?})"));
        assert_eq!(actual, expected, "token {token}");
        let styled: String = fixture.eval(&format!("globalThis.__ui.theme.fg({token:?}, \"x\")"));
        assert_eq!(styled, theme.fg(color, "x"), "token {token}");
        checked += 1;
    }
    assert_eq!(checked, 49);

    let themes: Vec<String> = fixture.eval(
        "globalThis.__ui.getAllThemes().map((theme) => theme.name + ':' + String(theme.path))",
    );
    assert_eq!(themes, vec!["dark:undefined", "light:/themes/light.json"]);

    let missing: String = fixture.eval("String(__ui.getTheme('missing'))");
    assert_eq!(missing, "undefined");
    let result: serde_json::Value = fixture.eval_json("__ui.setTheme('light')");
    assert_eq!(result["success"], true);
    assert_eq!(
        fixture.ui.set_theme_calls.lock().unwrap().as_slice(),
        &["light".to_owned()]
    );
}

#[test]
fn set_theme_failure_result_is_forwarded() {
    let fixture = fixture();
    fixture.eval::<()>(&make_ui(&fixture));
    *fixture.ui.theme_result.lock().unwrap() = Some(ThemeResult {
        success: false,
        error: Some("nope".to_owned()),
    });
    let result: serde_json::Value = fixture.eval_json("__ui.setTheme('light')");
    assert_eq!(result["success"], false);
    assert_eq!(result["error"], "nope");
}

// ===========================================================================
// widget / footer / header
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn widget_lines_are_forwarded_verbatim_and_cleared() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            __ui.setWidget("w", Array.from({ length: 12 }, (_, i) => "line" + i), { placement: "belowEditor" });
            "#,
        );
        {
            let widgets = fixture.ui.widgets.lock().unwrap();
            let (key, content, options) = widgets.last().unwrap();
            assert_eq!(key, "w");
            assert_eq!(
                options.as_ref().unwrap().placement,
                Some(kiana_ext_api::ui::WidgetPlacement::BelowEditor)
            );
            match content {
                // RPC 的 `widgetLines` 是调用方的完整数组：10 行截断属于 interactive
                // 渲染层（`interactive-mode.ts:2425-2430`），不在共享 JS 桥里做。
                Some(WidgetContent::Lines(lines)) => {
                    assert_eq!(lines.len(), 12, "{lines:?}");
                    assert_eq!(lines[0], "line0");
                    assert_eq!(lines[11], "line11");
                }
                other => panic!("expected lines, got {other:?}"),
            }
        }

        fixture.eval::<()>(
            r#"
            __ui.setWidget("w", () => ({
                render(width) { return ["factory:" + String(width)]; },
            }));
            "#,
        );
        let factory = {
            let widgets = fixture.ui.widgets.lock().unwrap();
            match &widgets.last().expect("widget").1 {
                Some(WidgetContent::Component(factory)) => Arc::clone(factory),
                other => panic!("expected a component widget, got {other:?}"),
            }
        };
        let tui = fake_tui();
        let theme = fixture.ui.theme_arc();
        let mut component = factory(tui, &theme);
        assert_eq!(component.render(42), vec!["factory:42".to_owned()]);

        fixture.eval::<()>("__ui.setWidget('w', undefined, { placement: 'belowEditor' });");
        {
            let widgets = fixture.ui.widgets.lock().unwrap();
            let (_, content, options) = widgets.last().unwrap();
            assert!(content.is_none());
            // RPC 的不可见 widget 记录仍带 placement（`rpc-mode.ts:206`）。
            assert_eq!(
                options.as_ref().unwrap().placement,
                Some(kiana_ext_api::ui::WidgetPlacement::BelowEditor)
            );
        }

        // TS rpc-mode：content 既不是 undefined 也不是数组 -> 不写任何记录。共享桥因此
        // 不把它转发给 UI 上下文（interactive 的非法调用在 TS 里会抛 TypeError）。
        let before = fixture.ui.widgets.lock().unwrap().len();
        fixture.eval::<()>(
            r#"
            __ui.setWidget("w", null);
            __ui.setWidget("w", "text");
            __ui.setWidget("w", 42);
            "#,
        );
        assert_eq!(fixture.ui.widgets.lock().unwrap().len(), before);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn footer_and_header_factories_receive_the_native_arguments() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            __ui.setFooter((tui, theme, footerData) => ({
                render(width) {
                    return [
                        "footer:" + String(width) + ":" + String(footerData.getGitBranch()) +
                        ":" + String(footerData.getAvailableProviderCount()) +
                        ":" + theme.fg("accent", "A"),
                    ];
                },
            }));
            __ui.setHeader((tui, theme) => ({
                render() { return ["header:" + theme.getColorMode()]; },
            }));
            "#,
        );
        let footer = fixture.ui.footer.lock().unwrap().clone().expect("footer");
        let theme = fixture.ui.theme_arc();
        let mut component = footer(
            fake_tui(),
            &theme,
            Arc::new(FakeFooterData::default()) as Arc<dyn ReadonlyFooterDataProvider>,
        );
        let lines = component.render(30);
        assert_eq!(
            lines,
            vec![format!(
                "footer:30:main:3:{}",
                theme.fg(kiana_theme::ThemeColor::Accent, "A")
            )]
        );

        let header = fixture.ui.header.lock().unwrap().clone().expect("header");
        let mut component = header(fake_tui(), &theme);
        let mode = match theme.get_color_mode() {
            kiana_tui::terminal_colors::TerminalColorMode::Palette256 => "256color",
            kiana_tui::terminal_colors::TerminalColorMode::Truecolor => "truecolor",
        };
        assert_eq!(component.render(30), vec![format!("header:{mode}")]);
    })
    .await;
}

// ===========================================================================
// editor 工厂
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn editor_factory_builds_a_js_editor_component() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            globalThis.__factoryArgs = null;
            __ui.setEditorComponent((tui, theme, keybindings) => {
                globalThis.__factoryArgs = {
                    borderColor: typeof theme.borderColor,
                    selectList: typeof theme.selectList,
                    matches: typeof keybindings.matches,
                };
                const editor = new __kiana_tui.Editor(tui, theme, { paddingX: 0 });
                globalThis.__editor = editor;
                return editor;
            });
            globalThis.__storedFactory = typeof __ui.getEditorComponent();
            "#,
        );
        let stored: String = fixture.eval("globalThis.__storedFactory");
        assert_eq!(stored, "function");

        let factory = fixture
            .ui
            .editor_factory
            .lock()
            .unwrap()
            .clone()
            .expect("editor factory");
        let theme = fixture.ui.theme_arc();
        let _ = theme;
        let editor_theme = kiana_theme::get_editor_theme();
        let keybindings = Arc::new(KeybindingsManager::new(Default::default(), None));
        let mut component = factory(fake_tui(), &editor_theme, keybindings);
        let args: serde_json::Value = fixture.eval_json("globalThis.__factoryArgs");
        assert_eq!(args["borderColor"], "function");
        assert_eq!(args["selectList"], "object");
        assert_eq!(args["matches"], "function");
        fixture.eval::<()>(
            r#"__editor.setAutocompleteProvider({
                triggerCharacters: ["@"],
                getSuggestions: () => Promise.resolve(null),
                applyCompletion: () => ({ lines: [], cursorLine: 0, cursorCol: 0 }),
                shouldTriggerFileCompletion: () => false,
            });"#,
        );
        assert!(!fixture.eval::<bool>("__editor.isShowingAutocomplete()"));
        component.set_text("hello");
        assert_eq!(component.get_text(), "hello");
        let lines = component.render(20);
        assert!(lines.len() >= 3, "{lines:?}");
        assert!(lines.iter().any(|line| line.contains("hello")));
    })
    .await;
}

// ===========================================================================
// custom
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn custom_factory_done_result_and_overlay_options() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            globalThis.__custom = {};
            __ui.custom((tui, theme, keybindings, done) => {
                globalThis.__custom.theme = typeof theme.fg;
                globalThis.__custom.keybindings = typeof keybindings.matches;
                done("answer");
                return {
                    render(width) { return ["custom:" + String(width)]; },
                    dispose() { __custom.disposed = true; },
                };
            }, {
                overlay: true,
                overlayOptions: { width: "50%", maxHeight: 12, visible: (w, h) => w > 0 && h > 0 },
                onHandle: (handle) => {
                    __custom.bounds = handle.getBounds();
                    handle.hide();
                },
            }).then((value) => { __custom.result = value; });
            "#,
        );
        let drive = TestDrive::new(&fixture);
        assert!(drive.settle().await);
        let custom: serde_json::Value = fixture.eval_json("globalThis.__custom");
        assert_eq!(custom["result"], "answer");
        assert_eq!(custom["theme"], "function");
        assert_eq!(custom["keybindings"], "function");
        assert_eq!(custom["bounds"]["width"], 40.0);
        assert_eq!(custom["bounds"]["row"], 2.0);

        assert_eq!(*fixture.ui.custom_overlay.lock().unwrap(), Some(true));
        let summary = fixture
            .ui
            .custom_overlay_summary
            .lock()
            .unwrap()
            .clone()
            .expect("overlay summary");
        assert!(summary.starts_with("static:"), "{summary}");
        assert!(summary.contains("Percent(50.0)"), "{summary}");
        assert!(summary.contains("Absolute(12.0)"), "{summary}");
        assert!(summary.contains("visible=true"), "{summary}");
        assert_eq!(fixture.ui.on_handle_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture.ui.custom_lines.lock().unwrap().as_slice(),
            &["custom:60".to_owned()]
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn custom_dynamic_overlay_options_are_evaluated_per_call() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            __ui.custom(() => ({ render() { return ["x"]; } }), {
                overlayOptions: () => ({ width: 40, anchor: "top-center" }),
            });
            "#,
        );
        let drive = TestDrive::new(&fixture);
        assert!(drive.settle().await);
        assert_eq!(
            fixture.ui.custom_overlay_summary.lock().unwrap().as_deref(),
            Some("dynamic")
        );
    })
    .await;
}

// ===========================================================================
// autocomplete provider
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn autocomplete_provider_bridge_calls_js_methods() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            globalThis.__providerCalls = [];
            __ui.addAutocompleteProvider((current) => ({
                triggerCharacters: ["@"],
                getSuggestions(lines, cursorLine, cursorCol, options) {
                    __providerCalls.push("get:" + lines.join("|") + ":" + cursorLine + ":" + cursorCol + ":" + options.force + ":" + typeof current.getSuggestions);
                    return Promise.resolve({
                        items: [{ value: "@one", label: "One", description: "first" }],
                        prefix: "@",
                    });
                },
                applyCompletion(lines, cursorLine, cursorCol, item, prefix) {
                    __providerCalls.push("apply:" + item.value + ":" + prefix);
                    return { lines: [prefix + item.value], cursorLine: 0, cursorCol: item.value.length + prefix.length };
                },
                shouldTriggerFileCompletion() { return false; },
            }));
            "#,
        );
        let factory = fixture
            .ui
            .autocomplete_factory
            .lock()
            .unwrap()
            .clone()
            .expect("autocomplete factory");
        let provider = factory(Box::new(BaseProvider));
        assert_eq!(provider.trigger_characters(), Some(vec!['@']));
        assert!(!provider.should_trigger_file_completion(&["a".into()], 0, 1));
        let result = provider.apply_completion(
            &["ab".into()],
            0,
            2,
            &AutocompleteItem::new("@one", "One"),
            "@",
        );
        assert_eq!(result.lines, vec!["@@one".to_owned()]);

        let mut provider = factory(Box::new(BaseProvider));
        let options = GetSuggestionsOptions::new(tokio_util::sync::CancellationToken::new(), true);
        let lines = vec!["hello".to_owned()];
        let mut future = Box::pin(provider.get_suggestions(&lines, 0, 5, &options));
        let suggestions = {
            let engine = Rc::clone(&fixture.engine);
            loop {
                tokio::select! {
                    biased;
                    value = &mut future => break value,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(1)) => {
                        engine.pump_jobs();
                        if engine.try_drain_completions() > 0 { continue; }
                    }
                }
            }
        }
        .expect("suggestions");
        assert_eq!(suggestions.prefix, "@");
        assert_eq!(suggestions.items.len(), 1);
        assert_eq!(suggestions.items[0].value, "@one");
        assert_eq!(suggestions.items[0].description.as_deref(), Some("first"));

        let calls: Vec<String> = fixture.eval("globalThis.__providerCalls");
        assert!(
            calls.iter().any(|call| call.starts_with("get:hello:0:5:true:function")),
            "{calls:?}"
        );
        assert!(calls.contains(&"apply:@one:@".to_owned()), "{calls:?}");
    })
    .await;
}

// ===========================================================================
// 驱动
// ===========================================================================

/// 复用测试 support 的引擎泵（`settle_all`）而不依赖它的 `Harness` 所有权。
struct TestDrive<'a> {
    fixture: &'a Fixture,
}

impl<'a> TestDrive<'a> {
    fn new(fixture: &'a Fixture) -> Self {
        Self { fixture }
    }

    async fn settle(&self) -> bool {
        let engine = &self.fixture.engine;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if engine.try_drain_completions() > 0 {
                continue;
            }
            if engine.pump_jobs() > 0 {
                continue;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            match engine.next_timer_deadline() {
                Some(due) if due <= deadline => {
                    tokio::select! {
                        _ = tokio::time::sleep_until(due) => { engine.fire_due_timers(); }
                        _ = engine.pump_completions() => {}
                        _ = tokio::time::sleep_until(deadline) => return false,
                    }
                }
                _ => {
                    tokio::select! {
                        _ = engine.pump_completions() => {}
                        _ = tokio::time::sleep(std::time::Duration::from_millis(1)) => {
                            if engine.try_drain_completions() == 0 && engine.pump_jobs() == 0 {
                                return true;
                            }
                        }
                        _ = tokio::time::sleep_until(deadline) => return false,
                    }
                }
            }
        }
    }
}

/// `WidgetContent` 没有 `Clone`；测试里按引用读取的辅助函数在此避免误用。
fn _assert_widget_debug(content: &WidgetContent) -> String {
    match content {
        WidgetContent::Lines(lines) => lines.join("\n"),
        WidgetContent::Component(_) => "<component>".to_owned(),
    }
}

// ===========================================================================
// 回归：评审发现的偏差
// ===========================================================================

/// `ctx.ui` 必须是**单个**对象（`runner.ts:887-890`），`setEditorComponent` 之后任意一次
/// `getEditorComponent()` 都看得见同一个工厂（`interactive-mode.ts:2690-2710`）。
#[test]
fn ui_object_identity_is_stable_and_editor_factory_survives_reads() {
    let fixture = fixture();
    fixture.eval::<()>(&format!(
        "globalThis.__handle = {}; globalThis.__a = __kiana_ctx_make_ui(__handle); globalThis.__b = __kiana_ctx_make_ui(__handle);",
        fixture.handle
    ));
    let same: bool = fixture.eval("__a === __b && __a === __kiana_ctx_make_ui(__handle)");
    assert!(same, "ctx.ui must be a stable object");
    fixture.eval::<()>(
        r#"
        __a.setEditorComponent((tui, theme, keybindings) => ({
            render() { return []; },
            getText() { return ""; },
            setText() {},
        }));
        globalThis.__factoryType = typeof __b.getEditorComponent();
        globalThis.__sameFactory = __a.getEditorComponent() === __b.getEditorComponent();
        "#,
    );
    let factory_type: String = fixture.eval("globalThis.__factoryType");
    assert_eq!(factory_type, "function");
    let same_factory: bool = fixture.eval("globalThis.__sameFactory");
    assert!(same_factory);
}

/// `done(null)` 解析出 `null`，`done()` 解析出 `undefined`（`showExtensionCustom` 的原值语义；
/// handoff.ts:161 / qna.ts:109 用 `result === null` 判定取消）。
#[tokio::test(flavor = "current_thread")]
async fn custom_done_preserves_null_and_undefined() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            globalThis.__doneResults = {};
            const empty = { render() { return []; } };
            __ui.custom((tui, theme, keybindings, done) => { done(null); return empty; })
                .then((value) => { __doneResults.null = value === null ? "null" : String(value); });
            __ui.custom((tui, theme, keybindings, done) => { done(); return empty; })
                .then((value) => { __doneResults.undefined = value === undefined ? "undefined" : String(value); });
            __ui.custom((tui, theme, keybindings, done) => { done(0); return empty; })
                .then((value) => { __doneResults.zero = value; });
            "#,
        );
        let drive = TestDrive::new(&fixture);
        assert!(drive.settle().await);
        let results: serde_json::Value = fixture.eval_json("globalThis.__doneResults");
        assert_eq!(results["null"], "null");
        assert_eq!(results["undefined"], "undefined");
        assert_eq!(results["zero"], 0);
    })
    .await;
}

/// `getSuggestions(..., options)` 的 `options` 必须带真实 `AbortSignal`
/// （`autocomplete.ts:268-297`；示例 github-issue-autocomplete.ts:105 读 `options.signal.aborted`）。
#[tokio::test(flavor = "current_thread")]
async fn autocomplete_suggestions_receive_an_abort_signal() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            globalThis.__providerOptions = [];
            __ui.addAutocompleteProvider((current) => ({
                getSuggestions(lines, cursorLine, cursorCol, options) {
                    __providerOptions.push({
                        isSignal: options.signal instanceof AbortSignal,
                        aborted: options.signal.aborted,
                        hasAddEventListener: typeof options.signal.addEventListener === "function",
                        hasForce: "force" in options,
                        force: options.force,
                    });
                    return Promise.resolve(null);
                },
            }));
            "#,
        );
        let factory = fixture
            .ui
            .autocomplete_factory
            .lock()
            .unwrap()
            .clone()
            .expect("autocomplete factory");
        let mut provider = factory(Box::new(BaseProvider));
        let lines = vec!["hello".to_owned()];
        let options = GetSuggestionsOptions::new(tokio_util::sync::CancellationToken::new(), false);
        let engine = Rc::clone(&fixture.engine);
        let mut future = Box::pin(provider.get_suggestions(&lines, 0, 5, &options));
        loop {
            tokio::select! {
                biased;
                _ = &mut future => break,
                _ = tokio::time::sleep(std::time::Duration::from_millis(1)) => {
                    engine.pump_jobs();
                    if engine.try_drain_completions() > 0 { continue; }
                }
            }
        }
        let seen: serde_json::Value = fixture.eval_json("globalThis.__providerOptions");
        let entry = &seen.as_array().expect("array")[0];
        assert_eq!(entry["isSignal"], true, "{entry}");
        assert_eq!(entry["aborted"], false);
        assert_eq!(entry["hasAddEventListener"], true);
        // TS 的编辑器总是传 `force`（`editor.ts:2374-2379`）。
        assert_eq!(entry["hasForce"], true);

        let mut provider = factory(Box::new(BaseProvider));
        let options = GetSuggestionsOptions::new(tokio_util::sync::CancellationToken::new(), true);
        let engine = Rc::clone(&fixture.engine);
        let mut future = Box::pin(provider.get_suggestions(&lines, 0, 5, &options));
        loop {
            tokio::select! {
                biased;
                _ = &mut future => break,
                _ = tokio::time::sleep(std::time::Duration::from_millis(1)) => {
                    engine.pump_jobs();
                    if engine.try_drain_completions() > 0 { continue; }
                }
            }
        }
        let seen: serde_json::Value = fixture.eval_json("globalThis.__providerOptions");
        assert_eq!(seen.as_array().expect("array")[1]["hasForce"], true);
        assert_eq!(seen.as_array().expect("array")[1]["force"], true);
    })
    .await;
}

/// 实时状态表：`size`/`get`/`has`/`entries`/`[Symbol.iterator]` 都反映 `getExtensionStatuses()`
/// 之后的变化（`footer-data-provider.ts:134-137`；footer.ts:290-295 用 `entries()`）。
#[tokio::test(flavor = "current_thread")]
async fn footer_statuses_are_a_live_map() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            __ui.setFooter((tui, theme, footerData) => ({
                render() {
                    const statuses = footerData.getExtensionStatuses();
                    const entries = Array.from(statuses.entries())
                        .map(([key, value]) => key + "=" + value)
                        .join(",");
                    const spread = Array.from(statuses).map(([key]) => key).join(",");
                    let seen = "";
                    statuses.forEach((value, key) => { seen += key + ":" + value + ";"; });
                    return [
                        String(statuses.size) + "|" +
                        String(statuses.get("a")) + "|" +
                        String(statuses.has("b")) + "|" +
                        entries + "|" + spread + "|" + seen,
                    ];
                },
            }));
            "#,
        );
        let footer = fixture.ui.footer.lock().unwrap().clone().expect("footer");
        let provider = Arc::new(LiveFooterData::default());
        provider.set("a", "1");
        let theme = fixture.ui.theme_arc();
        let mut component = footer(
            fake_tui(),
            &theme,
            Arc::clone(&provider) as Arc<dyn ReadonlyFooterDataProvider>,
        );
        assert_eq!(
            component.render(40),
            vec!["1|1|false|a=1|a|a:1;".to_owned()]
        );
        provider.set("b", "2");
        assert_eq!(
            component.render(40),
            vec!["2|1|true|a=1,b=2|a,b|a:1;b:2;".to_owned()]
        );
        assert!(component.render(40)[0].contains("b=2"));
    })
    .await;
}

/// `getGitBranch()` 不在仓库中时返回 `null`（不是 `undefined`，`footer-data-provider.ts:125-131`）。
#[tokio::test(flavor = "current_thread")]
async fn footer_git_branch_is_null_outside_a_repo() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            __ui.setFooter((tui, theme, footerData) => ({
                render() {
                    return [
                        String(footerData.getGitBranch()) + "|" +
                        String(footerData.getGitBranch() === null),
                    ];
                },
            }));
            "#,
        );
        let footer = fixture.ui.footer.lock().unwrap().clone().expect("footer");
        let theme = fixture.ui.theme_arc();
        let mut component = footer(
            fake_tui(),
            &theme,
            Arc::new(NoRepoFooterData) as Arc<dyn ReadonlyFooterDataProvider>,
        );
        assert_eq!(component.render(20), vec!["null|true".to_owned()]);
    })
    .await;
}

/// `getDefinition()` 原样返回定义条目 `{ defaultKeys, description? }`
/// （`keybindings.ts:63-66、282-284`）。
#[tokio::test(flavor = "current_thread")]
async fn keybindings_get_definition_returns_the_raw_entry() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            __ui.setEditorComponent((tui, theme, keybindings) => {
                globalThis.__definitions = {
                    multi: keybindings.getDefinition("tui.select.cancel"),
                    single: keybindings.getDefinition("tui.editor.cursorUp"),
                    missing: String(keybindings.getDefinition("does.not.exist")),
                };
                return { render() { return []; }, getText() { return ""; }, setText() {} };
            });
            "#,
        );
        let factory = fixture
            .ui
            .editor_factory
            .lock()
            .unwrap()
            .clone()
            .expect("editor factory");
        let editor_theme = kiana_theme::get_editor_theme();
        let keybindings = Arc::new(KeybindingsManager::new(Default::default(), None));
        let _ = factory(fake_tui(), &editor_theme, keybindings);
        let definitions: serde_json::Value = fixture.eval_json("globalThis.__definitions");
        assert_eq!(
            definitions["multi"],
            serde_json::json!({
                "defaultKeys": ["escape", "ctrl+c"],
                "description": "Cancel selection",
            })
        );
        assert_eq!(
            definitions["single"],
            serde_json::json!({ "defaultKeys": "up", "description": "Move cursor up" })
        );
        assert_eq!(definitions["missing"], "undefined");
        // TS 的形状里没有 `keybinding`/`keys` 字段。
        assert!(definitions["multi"].get("keybinding").is_none());
        assert!(definitions["multi"].get("keys").is_none());
    })
    .await;
}

/// TUI 代理：`terminal` 是宿主终端（实时 rows/columns + 方法）；
/// `addChild`/`removeChild` 按对象身份挂载/摘除且不调用 `dispose()`；
/// `setFocus` 在挂载树里找组件（不是按代理自己的 added 下标）。
#[tokio::test(flavor = "current_thread")]
async fn tui_proxy_terminal_children_and_focus() {
    run_local(async {
        let fixture = fixture();
        *fixture.ui.terminal_rows.lock().unwrap() = Some(50);
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            globalThis.__tuiChecks = {};
            __ui.custom((tui, theme, keybindings, done) => {
                const terminal = tui.terminal;
                __tuiChecks.rows = terminal.rows;
                __tuiChecks.columns = terminal.columns;
                __tuiChecks.sameTerminal = tui.terminal === terminal;
                __tuiChecks.hasWrite = typeof terminal.write === "function";
                __tuiChecks.hasSetTitle = typeof terminal.setTitle === "function";
                __tuiChecks.kitty = terminal.kittyProtocolActive;

                // JS Editor 的滚动窗口读宿主的实时 rows（editor.ts:537：30% 行数、最少 5 行）。
                const editorTheme = { borderColor: (text) => text, selectList: {} };
                const editor = new __kiana_tui.Editor(tui, editorTheme, { paddingX: 0 });
                editor.setText(Array.from({ length: 20 }, (_, i) => "l" + i).join("\n"));
                __tuiChecks.editorLines = editor.render(20).length;

                const existing = new __kiana_tui.Text("existing", 0, 0);
                const root = new __kiana_tui.Container();
                const input = new __kiana_tui.Input({ prompt: "" });
                root.addChild(input);
                root.dispose = () => { __tuiChecks.disposed = true; };
                tui.addChild(existing);
                tui.addChild(root);
                __tuiChecks.added = tui.getChildComponents().length;
                tui.setFocus(root);
                tui.removeChild(root);
                __tuiChecks.removed = tui.getChildComponents().length;
                __tuiChecks.afterRemove = [existing].length;

                // 嵌套控件：焦点应落在包含它的挂载组件上（index 1），不是代理 added[0]。
                tui.addChild(root);
                tui.setFocus(root.children[0]);

                // overlay：句柄、隐藏状态与整个 TUI 的 overlay 栈。
                const overlay = tui.showOverlay(new __kiana_tui.Text("overlay", 0, 0), { width: 10 });
                __tuiChecks.overlayHasHide = typeof overlay.hide === "function";
                __tuiChecks.overlayVisible = tui.hasOverlay();
                __tuiChecks.overlayHiddenBefore = overlay.isHidden();
                overlay.setHidden(true);
                __tuiChecks.overlayHiddenAfter = overlay.isHidden();
                overlay.setHidden(false);
                overlay.hide();
                tui.hideOverlay();
                __tuiChecks.overlayAfterHideAll = tui.hasOverlay();
                done("ok");
                return { render() { return ["custom"]; } };
            });
            "#,
        );
        let drive = TestDrive::new(&fixture);
        assert!(drive.settle().await);
        let checks: serde_json::Value = fixture.eval_json("globalThis.__tuiChecks");
        assert_eq!(
            checks["rows"], 50.0,
            "terminal rows must be the host terminal's"
        );
        assert_eq!(checks["columns"], 80.0);
        assert_eq!(checks["sameTerminal"], true);
        assert_eq!(checks["hasWrite"], true);
        assert_eq!(checks["hasSetTitle"], true);
        assert_eq!(checks["kitty"], false);
        // 50 行终端 → max(5, floor(50 * 0.3)) = 15 行内容 + 上下边框。
        assert_eq!(
            checks["editorLines"], 17.0,
            "the editor must read the host terminal rows"
        );
        assert_eq!(checks["added"], 2.0);
        assert_eq!(checks["removed"], 1.0);
        assert!(
            checks.get("disposed").is_none(),
            "removeChild must not dispose"
        );
        assert_eq!(checks["overlayHasHide"], true);
        assert_eq!(checks["overlayVisible"], true);
        assert_eq!(checks["overlayHiddenBefore"], false);
        assert_eq!(checks["overlayHiddenAfter"], true);
        assert_eq!(checks["overlayAfterHideAll"], false);

        let tui = fixture.ui.last_tui.lock().unwrap().clone().expect("tui");
        // 最后一次 `setFocus(root.children[0])`（root 重新挂载为 index 1）聚焦 index 1。
        assert_eq!(tui.0.focus(), Some(FocusTarget::Component { index: 1 }));
    })
    .await;
}

/// JS 构造的 Loader 自动运行动画（TS 构造期 `setIndicator → start → setInterval`，
/// `loader.ts:83-113`）：没有宿主驱动也会每隔 `intervalMs` 请求重绘。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn loaders_animate_without_a_host_driver() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            globalThis.__renders = 0;
            globalThis.__loader = new __kiana_tui.Loader(
                { requestRender() { __renders += 1; } },
                (text) => text,
                (text) => text,
                "working",
                { intervalMs: 20 },
            );
            "#,
        );
        for _ in 0..20 {
            if fixture.eval::<f64>("globalThis.__renders") > 0.0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let renders: f64 = fixture.eval("globalThis.__renders");
        assert!(renders > 0.0, "the spinner timer must request renders");
        // 停止后不再请求。
        fixture.eval::<()>("__loader.stop();");
        let at_stop: f64 = fixture.eval("globalThis.__renders");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let after: f64 = fixture.eval("globalThis.__renders");
        assert_eq!(after, at_stop, "stop() must cancel the animation timer");
    })
    .await;
}

// ===========================================================================
// 回归：第二轮评审发现的偏差（焦点 / dispose / 拒绝 / 引用释放）
// ===========================================================================

/// `setFocus` / overlay `focus()`/`unfocus()` 把 `focused` 写到 JS 组件对象上
/// （TS `tui.ts:672-680、812-843`）；`unfocus()` 无参数恢复上一个可见 overlay。
#[tokio::test(flavor = "current_thread")]
async fn focus_flags_reach_js_components_and_unfocus_restores() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            globalThis.__focusChecks = {};
            __ui.custom((tui, theme, keybindings, done) => {
                const checks = __focusChecks;
                const panel = { focused: false, render() { return ["panel"]; } };
                tui.addChild(panel);
                tui.setFocus(panel);
                checks.afterSetFocus = panel.focused;

                const other = { focused: false, render() { return ["other"]; } };
                tui.addChild(other);
                tui.setFocus(other);
                checks.previousCleared = panel.focused === false;
                checks.otherFocused = other.focused;

                const a = { focused: false, render() { return ["a"]; } };
                const b = { focused: false, render() { return ["b"]; } };
                const ha = tui.showOverlay(a, { width: 5 });
                const hb = tui.showOverlay(b, { width: 5 });
                checks.aFocusedAfterShow = a.focused;
                checks.bFocusedAfterShow = b.focused;

                ha.focus();
                checks.aFocused = a.focused;
                checks.bClearedByAFocus = b.focused === false;

                hb.focus();
                checks.bFocusedAgain = b.focused;

                hb.unfocus();
                checks.unfocusRestoredA = a.focused;
                checks.unfocusClearedB = b.focused === false;

                hb.focus();
                hb.unfocus({ target: panel });
                checks.targetFocused = panel.focused;
                checks.targetClearedB = b.focused === false;

                done("ok");
                return { render() { return ["custom"]; } };
            });
            "#,
        );
        let drive = TestDrive::new(&fixture);
        assert!(drive.settle().await);
        let checks: serde_json::Value = fixture.eval_json("globalThis.__focusChecks");
        assert_eq!(
            checks["afterSetFocus"], true,
            "setFocus must write focused=true"
        );
        assert_eq!(
            checks["previousCleared"], true,
            "the previous component must be cleared"
        );
        assert_eq!(checks["otherFocused"], true);
        assert_eq!(checks["aFocusedAfterShow"], false);
        assert_eq!(checks["bFocusedAfterShow"], true);
        assert_eq!(
            checks["aFocused"], true,
            "overlay focus() must write focused=true"
        );
        assert_eq!(checks["bClearedByAFocus"], true);
        assert_eq!(checks["bFocusedAgain"], true);
        assert_eq!(
            checks["unfocusRestoredA"], true,
            "unfocus() must restore the topmost visible overlay (A)"
        );
        assert_eq!(checks["unfocusClearedB"], true);
        assert_eq!(
            checks["targetFocused"], true,
            "unfocus({{target}}) must focus the target"
        );
        assert_eq!(checks["targetClearedB"], true);
    })
    .await;
}

/// `dispose()` 只在所有者关闭时调用：自定义对话框关闭会 dispose；overlay `hide()` 不会
/// （`interactive-mode.ts:2953-2967`、`tui.ts:770-790`）。
#[tokio::test(flavor = "current_thread")]
async fn custom_close_disposes_but_overlay_hide_does_not() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            globalThis.__disposeChecks = {};
            __ui.custom((tui, theme, keybindings, done) => {
                const overlayPanel = {
                    render() { return ["overlay"]; },
                    dispose() { __disposeChecks.overlayDisposed = true; },
                };
                const handle = tui.showOverlay(overlayPanel, { width: 10 });
                handle.hide();
                done("ok");
                return {
                    render() { return ["custom"]; },
                    dispose() { __disposeChecks.customDisposed = true; },
                };
            });
            "#,
        );
        let drive = TestDrive::new(&fixture);
        assert!(drive.settle().await);
        // 组件在宿主任务里 drop（无 JS 事务），dispose 登记到下一个事务；原生方法调用会清理。
        fixture.eval::<()>("new __kiana_tui.Text('x').render(1);");
        let checks: serde_json::Value = fixture.eval_json("globalThis.__disposeChecks");
        assert_eq!(
            checks["customDisposed"], true,
            "closing the dialog disposes the component"
        );
        assert!(
            checks.get("overlayDisposed").is_none(),
            "overlay hide() must not dispose the component: {checks}"
        );
    })
    .await;
}

/// 工厂同步抛错或返回的 Promise reject 会让 `ctx.ui.custom(...)` 的 Promise reject
/// （`interactive-mode.ts:2982-2990`），扩展可以 `catch` 到。
#[tokio::test(flavor = "current_thread")]
async fn custom_factory_failure_rejects_the_js_promise() {
    run_local(async {
        let fixture = fixture();
        fixture.eval::<()>(&make_ui(&fixture));
        fixture.eval::<()>(
            r#"
            globalThis.__rejections = {};
            __ui.custom(() => { throw new Error("sync-boom"); }).then(
                () => { __rejections.syncResolved = true; },
                (error) => {
                    __rejections.syncRejected = true;
                    __rejections.syncMessage = error.message;
                    __rejections.syncName = error.name;
                },
            );
            __ui.custom(async () => { throw new Error("async-boom"); }).then(
                () => { __rejections.asyncResolved = true; },
                (error) => {
                    __rejections.asyncRejected = true;
                    __rejections.asyncMessage = error.message;
                },
            );
            "#,
        );
        let drive = TestDrive::new(&fixture);
        assert!(drive.settle().await);
        let rejections: serde_json::Value = fixture.eval_json("globalThis.__rejections");
        assert_eq!(rejections["syncRejected"], true, "{rejections}");
        assert_eq!(rejections["syncMessage"], "sync-boom");
        assert_eq!(rejections["syncName"], "Error");
        assert!(rejections.get("syncResolved").is_none(), "{rejections}");
        assert_eq!(rejections["asyncRejected"], true, "{rejections}");
        assert_eq!(rejections["asyncMessage"], "async-boom");
        assert!(rejections.get("asyncResolved").is_none(), "{rejections}");
    })
    .await;
}

/// 替换 footer/header/widget/editor 工厂与退订终端输入时释放旧的 `__kiana_refs` 条目
/// （TS 依赖 GC；桥里必须显式释放，否则长会话无限增长）。
#[test]
fn replaced_callbacks_release_their_references() {
    let fixture = fixture();
    fixture.eval::<()>(&make_ui(&fixture));
    let before: f64 = fixture.eval("Object.keys(__kiana_refs).length");
    fixture.eval::<()>(
        r#"
        for (let i = 0; i < 5; i++) {
            __ui.setFooter((tui, theme) => ({ render() { return ["f" + i]; } }));
        }
        for (let i = 0; i < 5; i++) {
            __ui.setHeader((tui, theme) => ({ render() { return []; } }));
        }
        for (let i = 0; i < 5; i++) {
            __ui.setWidget("w", (tui, theme) => ({ render() { return []; } }));
        }
        for (let i = 0; i < 5; i++) {
            __ui.setEditorComponent(() => ({
                render() { return []; },
                getText() { return ""; },
                setText() {},
            }));
        }
        const unsubscribe = __ui.onTerminalInput(() => undefined);
        unsubscribe();
        // 退订闭包在 JS 事务外只登记释放；原生方法调用会清理。
        new __kiana_tui.Text("x").render(1);
        "#,
    );
    let after: f64 = fixture.eval("Object.keys(__kiana_refs).length");
    // 只剩当前 footer / header / widget 工厂 / editor 工厂各一个。
    assert_eq!(
        after - before,
        4.0,
        "replaced callbacks must be released ({before} -> {after})"
    );
}

/// `/reload` 的 `resetExtensionUI` 直接清空宿主工厂（`interactive-mode.ts:2465-2470`）：
/// `ctx.ui.getEditorComponent()` 之后必须返回 `undefined`。
#[test]
fn get_editor_component_reflects_host_reset() {
    let fixture = fixture();
    fixture.eval::<()>(&make_ui(&fixture));
    fixture.eval::<()>(
        r#"__ui.setEditorComponent(() => ({
            render() { return []; },
            getText() { return ""; },
            setText() {},
        }));"#,
    );
    let present: String = fixture.eval("String(typeof __ui.getEditorComponent())");
    assert_eq!(present, "function");
    // 宿主侧 resetExtensionUI：不经过 ctx.ui 直接清空。
    fixture
        .ui
        .editor_factory
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    let after: String = fixture.eval("String(__ui.getEditorComponent())");
    assert_eq!(after, "undefined", "the host cleared the factory");
}
