//! `ExtensionUiContext` / `ReadonlyFooterDataProvider` / `ReplacedSessionContextApi` 的契约。
//!
//! UI 上下文用完整 mock 证明对象安全与 28 个成员（27 个方法 + 只读 `theme`）：漏实现任一成员
//! 都无法编译，方法集合因此被编译器钉住。`ReplacedSessionContextApi` 的完整 mock 需要实现
//! 其父 trait 的全部 25 个上下文方法（依赖会话管理器/模型注册表等运行时对象），因此这里改用
//! trait 方法指针证明对象安全与两个 `LocalBoxFuture` 方法的签名。

use std::any::Any;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_config::keybindings::KeybindingsManager;
use kiana_core::event_bus::Unsubscribe;
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::types::ReplacedSessionContextApi;
use kiana_ext_api::ui::{
    AutocompleteProviderFactory, CustomDoneFn, CustomFactory, CustomOptions, EditorFactory,
    ExtensionUIDialogOptions, ExtensionUiContext, ExtensionWidgetOptions, FooterFactory,
    HeaderFactory, NotifyKind, PermissionCardChoice, PermissionCardRequest, PermissionScopeKind,
    ReadonlyFooterDataProvider, TerminalInputHandler, TerminalInputResult, ThemeInfo, ThemeOrName,
    ThemeResult, WidgetContent, WidgetPlacement, WorkingIndicatorOptions,
};
use kiana_theme::theme::{Theme, get_theme_by_name};
use kiana_tui::tui::Tui;
use serde_json::json;

// ---------------------------------------------------------------------------
// ExtensionUiContext 的完整 mock
// ---------------------------------------------------------------------------

/// `(key, 内容种类, placement)`：setWigdget 的记录项。
type RecordedWidget = (String, Option<&'static str>, Option<WidgetPlacement>);

struct MockUi {
    theme: Arc<Theme>,
    editor_text: Mutex<String>,
    statuses: Mutex<IndexMap<String, String>>,
    tools_expanded: AtomicBool,
    notifications: Mutex<Vec<(String, Option<NotifyKind>)>>,
    widgets: Mutex<Vec<RecordedWidget>>,
    title: Mutex<Option<String>>,
    terminal_handler: Mutex<Option<TerminalInputHandler>>,
    autocomplete_count: AtomicUsize,
    editor_component: Mutex<Option<EditorFactory>>,
    /// 每次 `select` 收到的 `(title, options)`（审批卡契约测试用）。
    select_requests: Mutex<Vec<(String, Vec<String>)>>,
    /// 指定 `select` 的固定回答；`None` 时返回默认的拼接串。
    select_answer: Mutex<Option<String>>,
}

impl MockUi {
    fn new() -> Self {
        Self {
            theme: get_theme_by_name("system").expect("内置 system 主题必须可构造"),
            editor_text: Mutex::new(String::new()),
            statuses: Mutex::new(IndexMap::new()),
            tools_expanded: AtomicBool::new(false),
            notifications: Mutex::new(Vec::new()),
            widgets: Mutex::new(Vec::new()),
            title: Mutex::new(None),
            terminal_handler: Mutex::new(None),
            autocomplete_count: AtomicUsize::new(0),
            editor_component: Mutex::new(None),
            select_requests: Mutex::new(Vec::new()),
            select_answer: Mutex::new(None),
        }
    }
}

fn panic_custom_factory(
    _tui: Arc<dyn Tui>,
    _theme: &Theme,
    _keybindings: Arc<KeybindingsManager>,
    _done: CustomDoneFn,
) -> LocalBoxFuture<'static, Box<dyn kiana_tui::tui::Component>> {
    panic!("测试不调用自定义组件工厂")
}

impl ExtensionUiContext for MockUi {
    fn select(
        &self,
        title: &str,
        options: &[String],
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        self.select_requests
            .lock()
            .unwrap()
            .push((title.to_string(), options.to_vec()));
        if let Some(answer) = self.select_answer.lock().unwrap().clone() {
            return Box::pin(async move { Some(answer) });
        }
        let answer = format!(
            "{title}|{}|{}",
            options.len(),
            options.first().cloned().unwrap_or_default()
        );
        Box::pin(async move { Some(answer) })
    }

    fn confirm(
        &self,
        _title: &str,
        _message: &str,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, bool> {
        Box::pin(async { true })
    }

    fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        let answer = format!("{title}|{}", placeholder.unwrap_or("<none>"));
        Box::pin(async move { Some(answer) })
    }

    fn notify(&self, message: &str, kind: Option<NotifyKind>) {
        self.notifications
            .lock()
            .unwrap()
            .push((message.to_string(), kind));
    }

    fn on_terminal_input(&self, handler: TerminalInputHandler) -> Unsubscribe {
        *self.terminal_handler.lock().unwrap() = Some(handler);
        Box::new(|| {})
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        let mut statuses = self.statuses.lock().unwrap();
        match text {
            Some(text) => {
                statuses.insert(key.to_string(), text.to_string());
            }
            None => {
                statuses.shift_remove(key);
            }
        }
    }

    fn set_working_message(&self, _message: Option<&str>) {}

    fn set_working_visible(&self, _visible: bool) {}

    fn set_working_indicator(&self, _options: Option<WorkingIndicatorOptions>) {}

    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}

    fn set_widget(
        &self,
        key: &str,
        content: Option<WidgetContent>,
        options: Option<ExtensionWidgetOptions>,
    ) {
        let kind = match &content {
            None => None,
            Some(WidgetContent::Lines(_)) => Some("lines"),
            Some(WidgetContent::Component(_)) => Some("component"),
        };
        let placement = options.and_then(|options| options.placement);
        self.widgets
            .lock()
            .unwrap()
            .push((key.to_string(), kind, placement));
    }

    fn set_footer(&self, _factory: Option<FooterFactory>) {}

    fn set_header(&self, _factory: Option<HeaderFactory>) {}

    fn set_title(&self, title: &str) {
        *self.title.lock().unwrap() = Some(title.to_string());
    }

    fn custom(
        &self,
        _factory: CustomFactory,
        _options: Option<CustomOptions>,
    ) -> LocalBoxFuture<'_, Box<dyn Any + Send>> {
        Box::pin(async { Box::new(7u32) as Box<dyn Any + Send> })
    }

    fn paste_to_editor(&self, text: &str) {
        self.editor_text.lock().unwrap().push_str(text);
    }

    fn set_editor_text(&self, text: &str) {
        *self.editor_text.lock().unwrap() = text.to_string();
    }

    fn get_editor_text(&self) -> String {
        self.editor_text.lock().unwrap().clone()
    }

    fn editor(&self, title: &str, prefill: Option<&str>) -> LocalBoxFuture<'_, Option<String>> {
        let answer = format!("{title}|{}", prefill.unwrap_or(""));
        Box::pin(async move { Some(answer) })
    }

    fn add_autocomplete_provider(&self, _factory: AutocompleteProviderFactory) {
        self.autocomplete_count.fetch_add(1, Ordering::SeqCst);
    }

    fn set_editor_component(&self, factory: Option<EditorFactory>) {
        *self.editor_component.lock().unwrap() = factory;
    }

    fn get_editor_component(&self) -> Option<EditorFactory> {
        self.editor_component.lock().unwrap().clone()
    }

    fn theme(&self) -> &Theme {
        &self.theme
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        vec![ThemeInfo {
            name: "system".to_string(),
            path: None,
        }]
    }

    fn get_theme(&self, name: &str) -> Option<Theme> {
        get_theme_by_name(name).and_then(|theme| Arc::try_unwrap(theme).ok())
    }

    fn set_theme(&self, theme: ThemeOrName) -> ThemeResult {
        match theme {
            ThemeOrName::Name(name) if name == "system" => ThemeResult {
                success: true,
                error: None,
            },
            ThemeOrName::Name(name) => ThemeResult {
                success: false,
                error: Some(format!("Theme not found: {name}")),
            },
            ThemeOrName::Theme(_) => ThemeResult {
                success: true,
                error: None,
            },
        }
    }

    fn get_tools_expanded(&self) -> bool {
        self.tools_expanded.load(Ordering::SeqCst)
    }

    fn set_tools_expanded(&self, expanded: bool) {
        self.tools_expanded.store(expanded, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn extension_ui_context_is_object_safe_with_all_members() {
    let mock = Arc::new(MockUi::new());
    let ui: Arc<dyn ExtensionUiContext> = mock.clone();

    assert_eq!(
        ui.select("pick", &["a".to_string(), "b".to_string()], None)
            .await
            .as_deref(),
        Some("pick|2|a")
    );
    assert!(ui.confirm("ok?", "really", None).await);
    assert_eq!(
        ui.input("name", Some("hint"), None).await.as_deref(),
        Some("name|hint")
    );
    assert_eq!(
        ui.input("name", None, None).await.as_deref(),
        Some("name|<none>")
    );

    ui.notify("hello", Some(NotifyKind::Warning));
    ui.notify("quiet", None);

    let handler: TerminalInputHandler = Arc::new(|data: &str| {
        Some(TerminalInputResult {
            consume: Some(true),
            data: Some(format!("{data}!")),
        })
    });
    let unsubscribe = ui.on_terminal_input(handler);
    let stored = mock
        .terminal_handler
        .lock()
        .unwrap()
        .clone()
        .expect("handler 必须被保存");
    let result = stored("x").expect("handler 必须返回结果");
    assert_eq!(result.consume, Some(true));
    assert_eq!(result.data.as_deref(), Some("x!"));
    unsubscribe();

    ui.set_status("k", Some("v"));
    ui.set_status("k", None);
    ui.set_working_message(Some("busy"));
    ui.set_working_message(None);
    ui.set_working_visible(true);
    ui.set_working_indicator(Some(WorkingIndicatorOptions {
        frames: Some(Vec::new()),
        interval_ms: Some(80.0),
    }));
    ui.set_hidden_thinking_label(Some("hidden"));

    ui.set_widget(
        "w1",
        Some(WidgetContent::Lines(vec!["line".to_string()])),
        Some(ExtensionWidgetOptions {
            placement: Some(WidgetPlacement::BelowEditor),
        }),
    );
    ui.set_widget("w2", None, None);
    ui.set_footer(None);
    ui.set_header(None);
    ui.set_title("kiana");

    let custom = ui
        .custom(
            Arc::new(panic_custom_factory) as CustomFactory,
            Some(CustomOptions {
                overlay: Some(false),
                overlay_options: None,
                on_handle: None,
            }),
        )
        .await;
    assert_eq!(custom.downcast_ref::<u32>(), Some(&7));

    ui.paste_to_editor("ab");
    ui.set_editor_text("cd");
    assert_eq!(ui.get_editor_text(), "cd");
    ui.paste_to_editor("ef");
    assert_eq!(ui.get_editor_text(), "cdef");
    assert_eq!(
        ui.editor("body", Some("prefill")).await.as_deref(),
        Some("body|prefill")
    );

    ui.add_autocomplete_provider(Arc::new(|current| current));
    assert_eq!(mock.autocomplete_count.load(Ordering::SeqCst), 1);

    ui.set_editor_component(None);
    assert!(ui.get_editor_component().is_none());

    // 只读 theme 属性返回实现方持有的主题实例。
    assert_eq!(ui.theme().name(), Some("system"));

    let themes = ui.get_all_themes();
    assert_eq!(themes.len(), 1);
    assert_eq!(themes[0].name, "system");
    assert!(themes[0].path.is_none());
    let theme = ui.get_theme("system").expect("system 主题存在");
    assert_eq!(theme.name(), Some("system"));
    assert!(ui.get_theme("definitely-missing").is_none());

    assert!(!ui.get_tools_expanded());
    ui.set_tools_expanded(true);
    assert!(ui.get_tools_expanded());

    // 上面逐个调用了 27 个方法与只读 `theme`：28 个成员齐全。
    let widgets = mock.widgets.lock().unwrap().clone();
    assert_eq!(widgets.len(), 2);
    assert_eq!(widgets[0].1, Some("lines"));
    assert_eq!(widgets[0].2, Some(WidgetPlacement::BelowEditor));
    assert_eq!(widgets[1].1, None);
}

/// 默认审批卡必须在用户选择之前展示工具名与命令/路径。
#[tokio::test]
async fn default_permission_card_shows_tool_and_target_before_approval() {
    let mock = MockUi::new();
    *mock.select_answer.lock().unwrap() = Some("Reject".to_string());

    let choice = mock
        .permission_card(PermissionCardRequest {
            tool_name: "bash".to_string(),
            args: json!({"command": "rm -rf /tmp/important"}),
            cwd: "/workspace".to_string(),
            decision_reason: "default mode asks before execute actions".to_string(),
            always_scopes: vec![PermissionScopeKind::Session],
            dangerous: false,
        })
        .await;
    assert!(matches!(choice, Some(PermissionCardChoice::Reject { .. })));

    {
        let requests = mock.select_requests.lock().unwrap();
        let (title, options) = requests.first().expect("permission card must select");
        assert!(
            title.contains("bash") && title.contains("rm -rf /tmp/important"),
            "title must show the tool and command: {title}"
        );
        assert!(
            title.contains("default mode asks"),
            "title must keep the decision reason: {title}"
        );
        assert!(options.iter().any(|option| option == "Allow once"));
        assert!(options.iter().any(|option| option == "Reject"));
    }

    // 文件写入：标题展示目标路径；没有可持久化作用域时不展示 Always allow。
    mock.select_requests.lock().unwrap().clear();
    let choice = mock
        .permission_card(PermissionCardRequest {
            tool_name: "write".to_string(),
            args: json!({"path": "/workspace/src/lib.rs"}),
            cwd: "/workspace".to_string(),
            decision_reason: "write targets a protected subdirectory (.git/.kiana)".to_string(),
            always_scopes: Vec::new(),
            dangerous: false,
        })
        .await;
    assert!(matches!(choice, Some(PermissionCardChoice::Reject { .. })));
    let requests = mock.select_requests.lock().unwrap();
    let (title, options) = requests.first().expect("permission card must select");
    assert!(
        title.contains("/workspace/src/lib.rs"),
        "title must show the path: {title}"
    );
    assert!(
        !options.iter().any(|option| option == "Always allow"),
        "safety checks must not offer Always allow: {options:?}"
    );
}

#[test]
fn set_theme_and_theme_result_shapes() {
    let ui = MockUi::new();

    let system = ui.set_theme(ThemeOrName::Name("system".into()));
    assert_eq!(
        system,
        ThemeResult {
            success: true,
            error: None
        }
    );
    assert!(system.success);
    assert!(system.error.is_none());

    let missing = ui.set_theme(ThemeOrName::Name("missing".into()));
    assert!(!missing.success);
    assert_eq!(missing.error.as_deref(), Some("Theme not found: missing"));

    let theme = ui.get_theme("system").expect("system 主题存在");
    let direct = ui.set_theme(ThemeOrName::Theme(theme));
    assert_eq!(
        direct,
        ThemeResult {
            success: true,
            error: None
        }
    );

    // 无 UI 时的默认结果（逐字；行为实现在 kiana-extensions）。
    let no_ui = ThemeResult {
        success: false,
        error: Some("UI not available".to_string()),
    };
    assert!(!no_ui.success);
    assert_eq!(no_ui.error.as_deref(), Some("UI not available"));
    assert_eq!(
        format!("{no_ui:?}"),
        r#"ThemeResult { success: false, error: Some("UI not available") }"#
    );
}

#[test]
fn theme_info_and_theme_or_name_debug() {
    let info = ThemeInfo {
        name: "dark".into(),
        path: Some("/themes/dark.json".into()),
    };
    assert_eq!(info.name, "dark");
    assert_eq!(info.path.as_deref(), Some("/themes/dark.json"));
    assert!(format!("{info:?}").contains("dark"));

    let by_name = ThemeOrName::Name("dark".into());
    assert_eq!(format!("{by_name:?}"), r#"Name("dark")"#);
    let owned = match Arc::try_unwrap(get_theme_by_name("system").expect("system 主题存在")) {
        Ok(theme) => theme,
        Err(_) => panic!("主题 Arc 必须唯一"),
    };
    let by_theme = ThemeOrName::Theme(owned);
    assert!(format!("{by_theme:?}").starts_with("Theme("));
}

// ---------------------------------------------------------------------------
// ReadonlyFooterDataProvider
// ---------------------------------------------------------------------------

struct MockFooter {
    statuses: IndexMap<String, String>,
}

impl ReadonlyFooterDataProvider for MockFooter {
    fn get_git_branch(&self) -> Option<String> {
        // detached HEAD 的字面量是小写 "detached"。
        Some("detached".to_string())
    }

    fn get_extension_statuses(&self) -> &IndexMap<String, String> {
        &self.statuses
    }

    fn get_available_provider_count(&self) -> usize {
        3
    }

    fn on_branch_change(&self, callback: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        Box::new(move || callback())
    }
}

#[test]
fn readonly_footer_data_provider_contract() {
    let footer: Arc<dyn ReadonlyFooterDataProvider> = Arc::new(MockFooter {
        statuses: IndexMap::from([("ext".to_string(), "ok".to_string())]),
    });

    assert_eq!(footer.get_git_branch().as_deref(), Some("detached"));
    assert_eq!(
        footer
            .get_extension_statuses()
            .get("ext")
            .map(String::as_str),
        Some("ok")
    );
    // 返回实时引用：底层 map 变化后能看到。
    assert_eq!(footer.get_extension_statuses().len(), 1);
    assert_eq!(footer.get_available_provider_count(), 3);

    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let unsubscribe = footer.on_branch_change(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    unsubscribe();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// ReplacedSessionContextApi
// ---------------------------------------------------------------------------

fn assert_replaced_context_object_safe(_context: &dyn ReplacedSessionContextApi) {}

#[test]
fn replaced_session_context_api_is_object_safe_with_two_async_methods() {
    // 对象安全：可以存在 &dyn ReplacedSessionContextApi（并因此实现父 trait 的方法）。
    let _object_safe: fn(&dyn ReplacedSessionContextApi) = assert_replaced_context_object_safe;

    // 两个异步方法：把它们作为 trait 对象上的方法项取出，确实存在且是对象安全方法
    // （签名由 trait 声明钉住：`LocalBoxFuture<'static, ()>`，与 `SendMessageOptions` /
    // `UserContent + Option<SendUserMessageOptions>` 参数）。
    let send_message = <dyn ReplacedSessionContextApi>::send_message;
    let send_user_message = <dyn ReplacedSessionContextApi>::send_user_message;
    let _ = (send_message, send_user_message);

    // LocalBoxFuture 不是 Send（实现跑在 TUI 单线程上）；这里只检查类型可命名。
    fn accepts_local(_: LocalBoxFuture<'static, ()>) {}
    accepts_local(Box::pin(async {}));
}
