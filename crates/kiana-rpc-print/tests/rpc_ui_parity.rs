//! RPC 扩展 UI 子协议对拍测试（spec `ca-rpc-print.md` §2.5.5、§2.6.4、§2.6.5、§4.4.3）。
//!
//! 覆盖 9 种 `extension_ui_request` 的精确键序/省略规则、UUID v4 形状、pending 与
//! fire-and-forget 的区别、timeout/signal 的默认值解析，以及其余 28 个
//! `ExtensionUIContext` 成员的 no-op 语义。

#![allow(clippy::arc_with_non_send_sync)]

use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use kiana_ext_api::types::{ForkOptions, NewSessionOptions, SwitchSessionOptions};
use kiana_ext_api::ui::{
    AutocompleteProviderFactory, ExtensionUIDialogOptions, ExtensionUiContext, NotifyKind,
    ThemeOrName, WidgetContent, WidgetPlacement,
};
use kiana_ext_api::{BoxFuture, LocalBoxFuture};
use kiana_rpc_print::RawStdout;
use kiana_rpc_print::rpc_mode::{AgentSessionFacade, RpcServer, RpcSessionHost};
use kiana_session::agent_session::{
    AgentSession, AgentSessionEventListener, AgentSessionUnsubscribe, ExtensionBindings,
};
use kiana_session::agent_session_runtime::{ForkSessionResult, SessionReplacementResult};

// ============================================================================
// stdout 捕获
// ============================================================================

#[derive(Default)]
struct CaptureStdout {
    lines: Mutex<Vec<String>>,
}

impl CaptureStdout {
    fn lines(&self) -> Vec<String> {
        lock(&self.lines).clone()
    }

    fn clear(&self) {
        lock(&self.lines).clear();
    }
}

impl RawStdout for CaptureStdout {
    fn write_raw(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        lock(&self.lines).push(text.to_string());
    }

    fn wait_for_backpressure(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn flush(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn take_over(&self) {}
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ============================================================================
// Mock 会话 / 宿主
// ============================================================================

struct UiMock {
    bindings: Mutex<Option<ExtensionBindings>>,
}

impl UiMock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            bindings: Mutex::new(None),
        })
    }
}

#[async_trait(?Send)]
impl AgentSessionFacade for UiMock {
    fn agent_session(&self) -> AgentSession {
        panic!("UI tests only exercise bind_extensions/subscribe")
    }

    async fn bind_extensions(&self, bindings: ExtensionBindings) {
        *lock(&self.bindings) = Some(bindings);
    }

    fn subscribe(&self, _listener: AgentSessionEventListener) -> AgentSessionUnsubscribe {
        Box::new(|| {})
    }
}

struct UiHost {
    facade: Arc<dyn AgentSessionFacade>,
}

#[async_trait(?Send)]
impl RpcSessionHost for UiHost {
    fn session(&self) -> Arc<dyn AgentSessionFacade> {
        Arc::clone(&self.facade)
    }

    fn set_rebind_session(
        &self,
        _callback: Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>,
    ) {
    }

    async fn new_session(
        &self,
        _options: Option<NewSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        Ok(SessionReplacementResult { cancelled: true })
    }

    async fn switch_session(
        &self,
        _session_path: String,
        _options: Option<SwitchSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        Ok(SessionReplacementResult { cancelled: true })
    }

    async fn fork(
        &self,
        _entry_id: String,
        _options: Option<ForkOptions>,
    ) -> Result<ForkSessionResult, String> {
        Ok(ForkSessionResult {
            cancelled: true,
            selected_text: None,
        })
    }

    async fn dispose(&self) {}
}

struct UiHarness {
    server: Arc<RpcServer>,
    ui: Arc<dyn ExtensionUiContext>,
    stdout: Arc<CaptureStdout>,
}

async fn ui_harness() -> UiHarness {
    let facade = UiMock::new();
    let host = Arc::new(UiHost {
        facade: Arc::clone(&facade) as Arc<dyn AgentSessionFacade>,
    });
    let stdout = Arc::new(CaptureStdout::default());
    let server = RpcServer::new(
        Arc::clone(&host) as Arc<dyn RpcSessionHost>,
        Arc::clone(&stdout) as Arc<dyn RawStdout>,
    );
    server.rebind_session().await;
    let bindings = lock(&facade.bindings).clone().expect("bindings");
    UiHarness {
        server,
        ui: bindings.ui_context.expect("ui context"),
        stdout,
    }
}

// ============================================================================
// 夹具与断言
// ============================================================================

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/rpc_server/records.json")).expect("fixture JSON")
}

fn fixture_line(name: &str) -> String {
    fixture()[name]
        .as_str()
        .unwrap_or_else(|| panic!("fixture {name} missing"))
        .to_string()
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for condition"
        );
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}

/// 等待一条新请求，校验它是 UUID v4 且与 fixture 的模板逐字节一致（`{id}` 替换为实际 id）。
async fn take_request(stdout: &CaptureStdout, fixture_name: &str) -> String {
    wait_until(|| !stdout.lines().is_empty()).await;
    let line = stdout.lines().pop().expect("request line");
    stdout.clear();
    let raw = line.trim_end_matches('\n').to_string();
    let value: Value = serde_json::from_str(&raw).expect("request JSON");
    let id = value["id"].as_str().expect("request id").to_string();
    let parsed = uuid::Uuid::parse_str(&id).expect("uuid v4");
    assert_eq!(parsed.get_version_num(), 4, "uuid version");
    assert_eq!(id, parsed.to_string(), "uuid canonical form");
    assert_eq!(raw, fixture_line(fixture_name).replace("{id}", &id));
    id
}

fn ui_response(id: &str, payload: Value) -> String {
    let mut object = serde_json::Map::new();
    object.insert(
        "type".to_string(),
        Value::String("extension_ui_response".to_string()),
    );
    object.insert("id".to_string(), Value::String(id.to_string()));
    for (key, value) in payload.as_object().cloned().expect("payload object") {
        object.insert(key, value);
    }
    serde_json::to_string(&Value::Object(object)).expect("response line")
}

// ============================================================================
// dialog（select / confirm / input / editor）
// ============================================================================

#[tokio::test]
async fn select_request_shape_and_response() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;
            let options = vec!["a".to_string(), "b".to_string()];
            let dialog = harness.ui.select(
                "Pick",
                &options,
                Some(ExtensionUIDialogOptions {
                    signal: None,
                    timeout: Some(10.0),
                }),
            );
            let respond = async {
                let id = take_request(&harness.stdout, "ui_select").await;
                harness
                    .server
                    .handle_input_line(&ui_response(&id, json!({"value": "b"})))
                    .await;
            };
            let (result, ()) = tokio::join!(dialog, respond);
            assert_eq!(result, Some("b".to_string()));
        })
        .await;
}

#[tokio::test]
async fn select_without_timeout_omits_key_and_select_zero_keeps_it() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;

            let dialog = harness.ui.select("Pick", &["a".to_string()], None);
            let respond = async {
                let id = take_request(&harness.stdout, "ui_select_no_timeout").await;
                harness
                    .server
                    .handle_input_line(&ui_response(&id, json!({"cancelled": true})))
                    .await;
            };
            let (result, ()) = tokio::join!(dialog, respond);
            assert_eq!(result, None);

            // timeout: 0 仍出现在请求里，但不设置定时器（响应正常解析）。
            let dialog = harness.ui.select(
                "Pick",
                &["a".to_string()],
                Some(ExtensionUIDialogOptions {
                    signal: None,
                    timeout: Some(0.0),
                }),
            );
            let respond = async {
                let id = take_request(&harness.stdout, "ui_select_zero_timeout").await;
                harness
                    .server
                    .handle_input_line(&ui_response(&id, json!({"value": "a"})))
                    .await;
            };
            let (result, ()) = tokio::join!(dialog, respond);
            assert_eq!(result, Some("a".to_string()));
        })
        .await;
}

#[tokio::test]
async fn confirm_parses_confirmed_and_cancelled() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;
            let opts = Some(ExtensionUIDialogOptions {
                signal: None,
                timeout: Some(5000.0),
            });

            let dialog = harness.ui.confirm("Sure?", "Really?", opts.clone());
            let respond = async {
                let id = take_request(&harness.stdout, "ui_confirm").await;
                harness
                    .server
                    .handle_input_line(&ui_response(
                        &id,
                        json!({"confirmed": true, "cancelled": false}),
                    ))
                    .await;
            };
            let (result, ()) = tokio::join!(dialog, respond);
            assert!(result);

            // cancelled 优先于 confirmed。
            let dialog = harness.ui.confirm("Sure?", "Really?", opts.clone());
            let respond = async {
                let id = take_request(&harness.stdout, "ui_confirm").await;
                harness
                    .server
                    .handle_input_line(&ui_response(
                        &id,
                        json!({"confirmed": true, "cancelled": true}),
                    ))
                    .await;
            };
            let (result, ()) = tokio::join!(dialog, respond);
            assert!(!result);

            // 无 confirmed 键 -> false。
            let dialog = harness.ui.confirm(
                "Sure?",
                "Really?",
                Some(ExtensionUIDialogOptions {
                    signal: None,
                    timeout: Some(5000.0),
                }),
            );
            let respond = async {
                let id = take_request(&harness.stdout, "ui_confirm").await;
                harness
                    .server
                    .handle_input_line(&ui_response(&id, json!({})))
                    .await;
            };
            let (result, ()) = tokio::join!(dialog, respond);
            assert!(!result);

            // 非布尔 confirmed：TS 原样返回（调用方按真值判断），Rust 侧按 JS 真值映射。
            for (payload, expected) in [
                (json!({"confirmed": 1}), true),
                (json!({"confirmed": "yes"}), true),
                (json!({"confirmed": 0}), false),
                (json!({"confirmed": ""}), false),
                (json!({"confirmed": null}), false),
            ] {
                let dialog = harness.ui.confirm(
                    "Sure?",
                    "Really?",
                    Some(ExtensionUIDialogOptions {
                        signal: None,
                        timeout: Some(5000.0),
                    }),
                );
                let respond = async {
                    let id = take_request(&harness.stdout, "ui_confirm").await;
                    harness
                        .server
                        .handle_input_line(&ui_response(&id, payload))
                        .await;
                };
                let (result, ()) = tokio::join!(dialog, respond);
                assert_eq!(result, expected, "confirmed truthiness");
            }
        })
        .await;
}

#[tokio::test]
async fn input_handles_placeholder_and_timeout() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;

            let dialog = harness.ui.input(
                "Name",
                Some("type..."),
                Some(ExtensionUIDialogOptions {
                    signal: None,
                    timeout: None,
                }),
            );
            let respond = async {
                let id = take_request(&harness.stdout, "ui_input").await;
                harness
                    .server
                    .handle_input_line(&ui_response(&id, json!({"value": "typed"})))
                    .await;
            };
            let (result, ()) = tokio::join!(dialog, respond);
            assert_eq!(result, Some("typed".to_string()));

            let dialog = harness.ui.input(
                "Name",
                None,
                Some(ExtensionUIDialogOptions {
                    signal: None,
                    timeout: Some(250.0),
                }),
            );
            let respond = async {
                let id = take_request(&harness.stdout, "ui_input_no_placeholder").await;
                harness
                    .server
                    .handle_input_line(&ui_response(&id, json!({"value": 42})))
                    .await;
            };
            // 非字符串 value -> None（Rust 侧的类型化偏离，见 spec §2.6.4 注）。
            let (result, ()) = tokio::join!(dialog, respond);
            assert_eq!(result, None);
        })
        .await;
}

#[tokio::test]
async fn editor_resolves_value_and_cancellation() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;

            let dialog = harness.ui.editor("Edit", Some("line 1"));
            let respond = async {
                let id = take_request(&harness.stdout, "ui_editor").await;
                harness
                    .server
                    .handle_input_line(&ui_response(&id, json!({"value": "edited"})))
                    .await;
            };
            let (result, ()) = tokio::join!(dialog, respond);
            assert_eq!(result, Some("edited".to_string()));

            let dialog = harness.ui.editor("Edit", None);
            let respond = async {
                let id = take_request(&harness.stdout, "ui_editor_no_prefill").await;
                harness
                    .server
                    .handle_input_line(&ui_response(&id, json!({"cancelled": true})))
                    .await;
            };
            let (result, ()) = tokio::join!(dialog, respond);
            assert_eq!(result, None);
        })
        .await;
}

#[tokio::test]
async fn timeout_resolves_default_and_removes_pending() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;
            let start = tokio::time::Instant::now();
            let result = harness
                .ui
                .select(
                    "Pick",
                    &["a".to_string(), "b".to_string()],
                    Some(ExtensionUIDialogOptions {
                        signal: None,
                        timeout: Some(10.0),
                    }),
                )
                .await;
            assert_eq!(result, None);
            assert!(start.elapsed() >= std::time::Duration::from_millis(5));

            // 请求已经写出；迟到的响应不应产生任何记录，也不应崩溃。
            let id = take_request(&harness.stdout, "ui_select").await;
            harness
                .server
                .handle_input_line(&ui_response(&id, json!({"value": "late"})))
                .await;
            assert!(harness.stdout.lines().is_empty());
        })
        .await;
}

#[tokio::test]
async fn pre_cancelled_signal_returns_default_without_request() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;
            let signal = CancellationToken::new();
            signal.cancel();
            let options = vec!["a".to_string()];
            let result = harness
                .ui
                .select(
                    "Pick",
                    &options,
                    Some(ExtensionUIDialogOptions {
                        signal: Some(signal),
                        timeout: Some(1000.0),
                    }),
                )
                .await;
            assert_eq!(result, None);
            assert!(harness.stdout.lines().is_empty());
        })
        .await;
}

#[tokio::test]
async fn cancelled_while_pending_returns_default() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;
            let signal = CancellationToken::new();
            let dialog = harness.ui.confirm(
                "Sure?",
                "Really?",
                Some(ExtensionUIDialogOptions {
                    signal: Some(signal.clone()),
                    timeout: Some(5000.0),
                }),
            );
            let respond = async {
                let id = take_request(&harness.stdout, "ui_confirm").await;
                signal.cancel();
                id
            };
            let (result, id) = tokio::join!(dialog, respond);
            assert!(!result);

            // pending 已被清理：迟到的响应被静默忽略。
            harness
                .server
                .handle_input_line(&ui_response(&id, json!({"confirmed": true})))
                .await;
            assert!(harness.stdout.lines().is_empty());
        })
        .await;
}

// ============================================================================
// fire-and-forget 方法
// ============================================================================

#[tokio::test]
async fn fire_and_forget_methods_write_requests_without_pending_entries() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;

            harness.ui.notify("hi", Some(NotifyKind::Warning));
            let notify_id = take_request(&harness.stdout, "ui_notify").await;

            harness.ui.notify("hi", None);
            let notify_no_type_id = take_request(&harness.stdout, "ui_notify_no_type").await;

            harness.ui.set_status("k", Some("v"));
            let status_id = take_request(&harness.stdout, "ui_set_status").await;

            harness.ui.set_status("k", None);
            let status_undefined_id =
                take_request(&harness.stdout, "ui_set_status_undefined").await;

            harness.ui.set_widget(
                "w",
                Some(WidgetContent::Lines(vec!["x".to_string(), "y".to_string()])),
                Some(kiana_ext_api::ui::ExtensionWidgetOptions {
                    placement: Some(WidgetPlacement::AboveEditor),
                }),
            );
            take_request(&harness.stdout, "ui_set_widget").await;

            harness.ui.set_widget("w", None, None);
            take_request(&harness.stdout, "ui_set_widget_undefined").await;

            harness.ui.set_widget(
                "w",
                Some(WidgetContent::Lines(vec!["z".to_string()])),
                Some(kiana_ext_api::ui::ExtensionWidgetOptions {
                    placement: Some(WidgetPlacement::BelowEditor),
                }),
            );
            take_request(&harness.stdout, "ui_set_widget_below").await;

            harness.ui.set_title("kiana - demo");
            take_request(&harness.stdout, "ui_set_title").await;

            harness.ui.set_editor_text("prefill");
            take_request(&harness.stdout, "ui_set_editor_text").await;

            // fire-and-forget：对这些 id 回包不会 resolve 任何东西，也不会写记录。
            let ids = [notify_id, notify_no_type_id, status_id, status_undefined_id];
            for id in ids {
                harness
                    .server
                    .handle_input_line(&ui_response(&id, json!({"value": "x"})))
                    .await;
            }
            assert!(harness.stdout.lines().is_empty());
        })
        .await;
}

#[tokio::test]
async fn set_widget_component_factory_emits_nothing() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;
            let factory: kiana_ext_api::ui::WidgetFactory = Arc::new(
                |_tui: Arc<dyn kiana_tui::tui::Tui>,
                 _theme: &kiana_theme::theme::Theme|
                 -> Box<dyn kiana_tui::tui::Component> {
                    panic!("component factory must not be called in RPC mode")
                },
            );
            harness
                .ui
                .set_widget("w", Some(WidgetContent::Component(factory)), None);
            assert!(harness.stdout.lines().is_empty());
        })
        .await;
}

#[tokio::test]
async fn paste_to_editor_emits_set_editor_text() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;
            harness.ui.paste_to_editor("prefill");
            take_request(&harness.stdout, "ui_set_editor_text").await;
        })
        .await;
}

// ============================================================================
// 其余 ExtensionUIContext 成员
// ============================================================================

#[tokio::test]
async fn non_dialog_members_follow_no_op_semantics() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;
            let ui = &harness.ui;

            assert_eq!(ui.get_editor_text(), "");
            assert!(ui.get_editor_component().is_none());
            assert!(ui.get_all_themes().is_empty());
            assert!(ui.get_theme("dark").is_none());
            let theme = ui.set_theme(ThemeOrName::Name("dark".to_string()));
            assert!(!theme.success);
            assert_eq!(
                theme.error.as_deref(),
                Some("Theme switching not supported in RPC mode")
            );
            assert!(!ui.get_tools_expanded());
            ui.set_tools_expanded(true);

            ui.set_working_message(Some("working"));
            ui.set_working_visible(true);
            ui.set_working_indicator(None);
            ui.set_hidden_thinking_label(Some("thinking"));
            ui.set_footer(None);
            ui.set_header(None);
            ui.set_editor_component(None);

            let factory: AutocompleteProviderFactory = Arc::new(|provider| provider);
            ui.add_autocomplete_provider(factory);

            let handler = Arc::new(|_data: &str| None);
            let unsubscribe = ui.on_terminal_input(handler);
            unsubscribe();

            let custom_factory: kiana_ext_api::ui::CustomFactory = Arc::new(
                |_tui: Arc<dyn kiana_tui::tui::Tui>,
                 _theme: &kiana_theme::theme::Theme,
                 _keys: Arc<kiana_config::keybindings::KeybindingsManager>,
                 _done: kiana_ext_api::ui::CustomDoneFn|
                 -> LocalBoxFuture<'static, Box<dyn kiana_tui::tui::Component>> {
                    Box::pin(async { panic!("custom factory must not be called in RPC mode") })
                },
            );
            let value = ui.custom(custom_factory, None).await;
            assert!(value.downcast::<()>().is_ok());

            // 无 UI 记录产生。
            assert!(harness.stdout.lines().is_empty());
            // theme getter 返回全局主题（未初始化时回退内置 dark）。
            let _ = ui.theme();
        })
        .await;
}

#[tokio::test]
async fn unknown_extension_ui_response_is_silently_ignored() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;
            harness
                .server
                .handle_input_line(r#"{"type":"extension_ui_response","id":"missing","value":"x"}"#)
                .await;
            assert!(harness.stdout.lines().is_empty());
        })
        .await;
}

// ============================================================================
// 回归：共享 JS 桥（kiana-extensions/src/ui + js/ui/ui.js）到 RPC 记录的映射
//
// 扩展的 `ctx.ui` 先经过 kiana-extensions 的 JS 桥，再到达本文件的 `RpcUiContext`。
// 评审发现桥里的三处改写会改变 RPC 线上字节，这里用真实 JsEngine + 桥 + RPC 上下文
// 逐条锁定；测试驱动方式与 kiana-extensions 的 UI 测试相同（pump jobs/completions）。
// ============================================================================

struct BridgeHarness {
    engine: Rc<kiana_extensions::engine::JsEngine>,
    handle: u64,
    stdout: Arc<CaptureStdout>,
    _server: Arc<RpcServer>,
    _facade: Arc<UiMock>,
    _host: Arc<UiHost>,
}

impl BridgeHarness {
    /// 在 JS 引擎里求值 `()`（panic on error）。
    fn eval_unit(&self, source: &str) {
        self.engine
            .eval_script_error::<()>(source, "<rpc-ui-bridge>")
            .unwrap_or_else(|error| panic!("eval: {}\n{:?}", error.message, error.stack));
    }

    /// 在 JS 引擎里求值成字符串（panic on error）。
    fn eval_string(&self, source: &str) -> String {
        self.engine
            .eval_script_error::<String>(source, "<rpc-ui-bridge>")
            .unwrap_or_else(|error| panic!("eval: {}\n{:?}", error.message, error.stack))
    }

    /// 泵引擎，直到没有待处理 job/timer/completion。
    async fn settle(&self) -> bool {
        let engine = &self.engine;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
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
                        _ = tokio::time::sleep(Duration::from_millis(1)) => {
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

async fn bridge_harness() -> BridgeHarness {
    let facade = UiMock::new();
    let host = Arc::new(UiHost {
        facade: Arc::clone(&facade) as Arc<dyn AgentSessionFacade>,
    });
    let stdout = Arc::new(CaptureStdout::default());
    let server = RpcServer::new(
        Arc::clone(&host) as Arc<dyn RpcSessionHost>,
        Arc::clone(&stdout) as Arc<dyn RawStdout>,
    );
    server.rebind_session().await;
    let bindings = lock(&facade.bindings).clone().expect("bindings");
    let ui = bindings.ui_context.expect("ui context");

    let engine = Rc::new(
        kiana_extensions::engine::JsEngine::with_config(
            kiana_extensions::engine::EngineConfig::new(),
        )
        .expect("engine"),
    );
    kiana_extensions::engine::install(&engine).expect("prelude");
    kiana_extensions::ui::install_attached(&engine).expect("ui bridge");
    let handle = kiana_extensions::runner::context::register_ui_context(ui);
    BridgeHarness {
        engine,
        handle,
        stdout,
        _server: server,
        _facade: facade,
        _host: host,
    }
}

#[tokio::test]
async fn js_bridge_forwards_widget_lines_verbatim_without_truncation() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = bridge_harness().await;
            harness.eval_unit(&format!(
                "globalThis.__ui = __kiana_ctx_make_ui({});",
                harness.handle
            ));
            harness.eval_unit(
                r#"__ui.setWidget("w", Array.from({ length: 12 }, (_, i) => "line" + i), { placement: "belowEditor" });"#,
            );
            take_request(&harness.stdout, "ui_set_widget_verbatim").await;
        })
        .await;
}

#[tokio::test]
async fn js_bridge_keeps_placement_when_clearing_and_drops_unsupported_content() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = bridge_harness().await;
            harness.eval_unit(&format!(
                "globalThis.__ui = __kiana_ctx_make_ui({});",
                harness.handle
            ));
            // `setWidget(key, undefined, { placement })`：TS 仍发 widgetPlacement。
            harness.eval_unit(
                r#"__ui.setWidget("w", undefined, { placement: "belowEditor" });"#,
            );
            take_request(&harness.stdout, "ui_set_widget_undefined_placement").await;

            // null / string / 数字：TS rpc-mode 不发任何记录。
            harness.eval_unit(
                r#"__ui.setWidget("w", null); __ui.setWidget("w", "text"); __ui.setWidget("w", 42);"#,
            );
            assert!(harness.settle().await);
            assert!(
                harness.stdout.lines().is_empty(),
                "{:?}",
                harness.stdout.lines()
            );
        })
        .await;
}

#[tokio::test]
async fn js_bridge_preserves_missing_notify_type() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = bridge_harness().await;
            harness.eval_unit(&format!(
                "globalThis.__ui = __kiana_ctx_make_ui({});",
                harness.handle
            ));
            // 无 type：JSON.stringify 丢弃 `notifyType: undefined`。
            harness.eval_unit(r#"__ui.notify("hi");"#);
            take_request(&harness.stdout, "ui_notify_no_type").await;
            // 显式 type 仍然写出 notifyType。
            harness.eval_unit(r#"__ui.notify("hi", "warning");"#);
            take_request(&harness.stdout, "ui_notify").await;
        })
        .await;
}

#[tokio::test]
async fn js_bridge_pre_aborted_signal_emits_no_request_and_resolves_default() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = bridge_harness().await;
            harness.eval_unit(&format!(
                r#"
                globalThis.__ui = __kiana_ctx_make_ui({});
                globalThis.__out = {{}};
                const controller = new AbortController();
                controller.abort();
                __ui.select("Pick", ["a"], {{ signal: controller.signal }})
                  .then((value) => {{ __out.select = value === undefined ? "undefined" : String(value); }});
                "#,
                harness.handle
            ));
            assert!(harness.settle().await);
            // TS 的 createDialogPromise 在 `opts.signal.aborted` 时直接返回默认值，不发请求。
            assert!(
                harness.stdout.lines().is_empty(),
                "{:?}",
                harness.stdout.lines()
            );
            let resolved = harness.eval_string("String(globalThis.__out.select)");
            assert_eq!(resolved, "undefined");
        })
        .await;
}

// ============================================================================
// setTimeout 的 Node 数值强制（负数 / Infinity / 超过 2^31-1 都是 1ms）
// ============================================================================

#[tokio::test]
async fn dialog_timeouts_follow_node_coercion() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = ui_harness().await;
            for timeout in [1e10_f64, f64::INFINITY, -5.0] {
                let started = tokio::time::Instant::now();
                let value = tokio::time::timeout(
                    Duration::from_millis(500),
                    harness.ui.select(
                        "Pick",
                        &["a".to_string()],
                        Some(ExtensionUIDialogOptions {
                            signal: None,
                            timeout: Some(timeout),
                        }),
                    ),
                )
                .await
                .unwrap_or_else(|_| panic!("timeout {timeout} must fire in ~1ms"));
                assert_eq!(value, None);
                assert!(started.elapsed() < Duration::from_millis(500));
                harness.stdout.clear();
            }
        })
        .await;
}
