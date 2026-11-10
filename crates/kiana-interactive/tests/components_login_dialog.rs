//! `login-dialog-device-code.test.ts`、`auth-url-copy.test.ts` 的 LoginDialog 部分、
//! `5433-extension-oauth-prompt-input.test.ts` 的 Rust 移植。
//!
//! TS 用 `vi.mock` 替换 `copyToClipboard` / `openBrowser`；Rust 侧用组件提供的注入点
//! （`set_clipboard_copy` / `set_open_browser` / `set_spawn`）。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

use futures::executor::block_on;
use kiana_ai::auth::oauth_compat::OAuthDeviceCodeInfo;
use kiana_core::ansi::strip_ansi;
use kiana_interactive::components::login_dialog::{
    LoginCancelledError, LoginClipboardCopy, LoginDialogComponent, LoginOpenBrowser, LoginSpawn,
};
use kiana_theme::init_theme;
use kiana_tui::keybindings::{KeybindingsConfig, KeybindingsManager, set_keybindings};
use kiana_tui::tui::{Component, Focusable, TuiMouseButton, TuiMouseEvent, TuiMouseEventType};

const DEVICE_URI: &str = "https://example.com/device";
const DEVICE_CODE: &str = "ABCD-EFGH";

fn theme() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| init_theme(Some("dark"), false));
}

fn install_keybindings() {
    set_keybindings(KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        KeybindingsConfig::new(),
    ));
}

fn device() -> OAuthDeviceCodeInfo {
    OAuthDeviceCodeInfo {
        user_code: DEVICE_CODE.to_string(),
        verification_uri: DEVICE_URI.to_string(),
        open_browser_on_confirm: None,
        copy_code_on_confirm: None,
        interval_seconds: None,
        expires_in_seconds: None,
    }
}

type RenderLog = Rc<RefCell<usize>>;
type Calls = Rc<RefCell<Vec<String>>>;
type CompleteLog = Rc<RefCell<Vec<(bool, Option<String>)>>>;

struct Harness {
    dialog: LoginDialogComponent,
    complete: CompleteLog,
    open_browser_calls: Calls,
    copy_calls: Calls,
    renders: RenderLog,
}

fn clipboard_ok(calls: Calls) -> LoginClipboardCopy {
    Rc::new(move |text: String| {
        calls.borrow_mut().push(text);
        Box::pin(async { Ok(()) })
    })
}

fn clipboard_err(calls: Calls) -> LoginClipboardCopy {
    Rc::new(move |text: String| {
        calls.borrow_mut().push(text);
        Box::pin(async { Err("Clipboard unavailable".to_string()) })
    })
}

fn open_ok(calls: Calls) -> LoginOpenBrowser {
    Rc::new(move |url: &str| {
        calls.borrow_mut().push(url.to_string());
        Ok(())
    })
}

fn open_err(calls: Calls) -> LoginOpenBrowser {
    Rc::new(move |url: &str| {
        calls.borrow_mut().push(url.to_string());
        Err("Browser unavailable".to_string())
    })
}

fn spawn_now() -> LoginSpawn {
    Rc::new(|future| {
        block_on(future);
    })
}

fn harness(provider_id: &str, provider_name_override: Option<&str>) -> Harness {
    theme();
    install_keybindings();
    let renders: RenderLog = Rc::new(RefCell::new(0));
    let render_log = Rc::clone(&renders);
    let complete: CompleteLog = Rc::new(RefCell::new(Vec::new()));
    let complete_log = Rc::clone(&complete);
    let mut dialog = LoginDialogComponent::new(
        Rc::new(move || *render_log.borrow_mut() += 1),
        provider_id,
        Box::new(move |success, message| {
            complete_log.borrow_mut().push((success, message));
        }),
        provider_name_override,
        None,
    );
    let open_browser_calls: Calls = Rc::new(RefCell::new(Vec::new()));
    let copy_calls: Calls = Rc::new(RefCell::new(Vec::new()));
    dialog.set_open_browser(open_ok(Rc::clone(&open_browser_calls)));
    dialog.set_clipboard_copy(clipboard_ok(Rc::clone(&copy_calls)));
    dialog.set_spawn(spawn_now());
    dialog.set_focused(true);
    Harness {
        dialog,
        complete,
        open_browser_calls,
        copy_calls,
        renders,
    }
}

fn render(dialog: &mut LoginDialogComponent) -> String {
    strip_ansi(&dialog.render(120).join("\n"))
}

/// 与 TS `TuiMouseEvent` 同形的鼠标事件（`x`/`y` 是相对接收组件的坐标）。
fn mouse_event(
    event_type: TuiMouseEventType,
    x: isize,
    y: isize,
    width: usize,
    height: usize,
) -> TuiMouseEvent {
    TuiMouseEvent {
        event_type,
        button: TuiMouseButton::Left,
        x,
        y,
        screen_x: x,
        screen_y: y,
        width,
        height,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: (event_type == TuiMouseEventType::Click).then_some(1),
    }
}

// --- login-dialog-device-code.test.ts -------------------------------------

#[test]
fn opens_the_verification_page_and_copies_the_code_once_after_confirmation() {
    let mut harness = harness("custom-provider", None);
    let dialog = &mut harness.dialog;

    let mut domain =
        dialog.show_prompt("GitHub Enterprise URL/domain (blank for github.com)", None);
    dialog.handle_input("\r");
    assert_eq!(block_on(&mut domain).unwrap().unwrap(), "");

    dialog.show_device_code(device());
    dialog.show_waiting("Waiting for authentication...");
    assert!(*harness.renders.borrow() > 0);
    let output = render(dialog);
    assert!(output.contains("to open browser and copy code"));
    assert!(harness.open_browser_calls.borrow().is_empty());
    assert!(harness.copy_calls.borrow().is_empty());

    dialog.handle_input("\r");
    dialog.handle_input("\r");
    assert_eq!(
        harness.open_browser_calls.borrow().clone(),
        vec![DEVICE_URI.to_string()]
    );
    assert_eq!(
        harness.copy_calls.borrow().clone(),
        vec![DEVICE_CODE.to_string()]
    );
    let output = render(dialog);
    assert!(output.contains("Code copied to clipboard"));
    assert!(output.contains(&format!("Enter code: {DEVICE_CODE}")));
    assert!(output.contains("Waiting for authentication..."));
    assert!(!output.contains("to open browser and copy code"));
}

#[test]
fn honors_browser_and_clipboard_provider_overrides() {
    for (open_browser_on_confirm, copy_code_on_confirm) in
        [(false, false), (true, false), (false, true)]
    {
        let mut harness = harness("custom-provider", None);
        let dialog = &mut harness.dialog;
        let mut info = device();
        info.open_browser_on_confirm = Some(open_browser_on_confirm);
        info.copy_code_on_confirm = Some(copy_code_on_confirm);
        dialog.show_device_code(info);
        dialog.handle_input("\r");
        assert_eq!(
            harness.open_browser_calls.borrow().len(),
            usize::from(open_browser_on_confirm),
            "browser={open_browser_on_confirm} clipboard={copy_code_on_confirm}"
        );
        assert_eq!(
            harness.copy_calls.borrow().len(),
            usize::from(copy_code_on_confirm),
            "browser={open_browser_on_confirm} clipboard={copy_code_on_confirm}"
        );
        assert!(!dialog.signal().is_cancelled());
    }
}

#[test]
fn clears_the_previous_auth_url_copy_action_when_showing_a_device_code() {
    let mut harness = harness("custom-provider", None);
    let dialog = &mut harness.dialog;
    dialog.show_auth("https://example.com/authorize", None);
    harness.open_browser_calls.borrow_mut().clear();

    dialog.show_device_code(device());
    dialog.handle_input("\x18");
    assert!(harness.copy_calls.borrow().is_empty());
    assert!(harness.open_browser_calls.borrow().is_empty());

    dialog.handle_input("\r");
    assert_eq!(
        harness.copy_calls.borrow().clone(),
        vec![DEVICE_CODE.to_string()]
    );
}

#[test]
fn clears_device_actions_when_switching_to_browser_authentication() {
    let mut harness = harness("custom-provider", None);
    let dialog = &mut harness.dialog;
    dialog.show_device_code(device());
    dialog.show_auth("https://example.com/authorize", None);
    harness.open_browser_calls.borrow_mut().clear();

    dialog.handle_input("\r");
    assert!(harness.copy_calls.borrow().is_empty());
    assert!(harness.open_browser_calls.borrow().is_empty());

    dialog.handle_input("\x18");
    assert_eq!(
        harness.copy_calls.borrow().clone(),
        vec!["https://example.com/authorize".to_string()]
    );
}

#[test]
fn clears_both_action_types_when_showing_details() {
    let mut harness = harness("custom-provider", None);
    let dialog = &mut harness.dialog;
    dialog.show_auth("https://example.com/authorize", None);
    dialog.show_details(&["Choose a login method"]);
    dialog.handle_input("\x18");
    assert!(harness.copy_calls.borrow().is_empty());

    dialog.show_device_code(device());
    dialog.show_details(&["Choose a login method"]);
    harness.open_browser_calls.borrow_mut().clear();
    dialog.handle_input("\r");
    assert!(harness.copy_calls.borrow().is_empty());
    assert!(harness.open_browser_calls.borrow().is_empty());
}

#[test]
fn cancels_without_opening_a_browser_or_changing_the_clipboard() {
    let mut harness = harness("custom-provider", None);
    let dialog = &mut harness.dialog;
    dialog.show_device_code(device());
    dialog.handle_input("\x1b");
    dialog.handle_input("\r");
    assert!(dialog.signal().is_cancelled());
    assert_eq!(
        harness.complete.borrow().clone(),
        vec![(false, Some("Login cancelled".to_string()))]
    );
    assert!(harness.open_browser_calls.borrow().is_empty());
    assert!(harness.copy_calls.borrow().is_empty());
}

#[test]
fn keeps_manual_login_available_when_browser_and_clipboard_access_fail() {
    let mut harness = harness("custom-provider", None);
    harness
        .dialog
        .set_open_browser(open_err(Rc::clone(&harness.open_browser_calls)));
    harness
        .dialog
        .set_clipboard_copy(clipboard_err(Rc::clone(&harness.copy_calls)));
    let dialog = &mut harness.dialog;
    dialog.show_device_code(device());
    dialog.show_waiting("Waiting for authentication...");
    dialog.handle_input("\r");
    assert_eq!(
        harness.open_browser_calls.borrow().clone(),
        vec![DEVICE_URI.to_string()]
    );
    assert_eq!(
        harness.copy_calls.borrow().clone(),
        vec![DEVICE_CODE.to_string()]
    );
    let output = render(dialog);
    assert!(output.contains(DEVICE_URI));
    assert!(output.contains(&format!("Enter code: {DEVICE_CODE}")));
    assert!(output.contains("Copy code manually"));
    assert!(output.contains("Waiting for authentication..."));
    assert!(!dialog.signal().is_cancelled());
    assert!(harness.complete.borrow().is_empty());
}

#[test]
fn cancelling_a_pending_prompt_rejects_with_login_cancelled() {
    let mut harness = harness("custom-provider", None);
    let mut prompt = harness.dialog.show_manual_input("Paste the code:");
    harness.dialog.handle_input("\x1b");
    assert_eq!(
        block_on(&mut prompt).unwrap(),
        Err(LoginCancelledError),
        "rejection value must be Error(\"Login cancelled\")"
    );
    assert_eq!(LoginCancelledError.to_string(), "Login cancelled");
}

#[test]
fn superseded_prompts_stay_pending_instead_of_resolving_with_cancellation() {
    let mut harness = harness("custom-provider", None);
    let first = harness.dialog.show_manual_input("First:");
    let _second = harness.dialog.show_prompt("Second:", None);
    // 即使对话框随后销毁，被顶替的 Promise（TS）依旧永不 settle。
    drop(harness.dialog);

    // TS 覆盖 `inputResolver` / `inputRejecter`：被顶替的 Promise 永远不会 settle。
    let mut first = Box::pin(first);
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    assert!(
        std::future::Future::poll(first.as_mut(), &mut context).is_pending(),
        "a superseded prompt receiver must stay pending, not fail with Canceled"
    );
}

// --- auth-url-copy.test.ts（LoginDialog 部分） -----------------------------

const COPY_URL: &str = "https://auth.example.invalid/authorize";

#[test]
fn login_dialog_copies_the_auth_url_instead_of_typing_into_the_code_input() {
    let mut harness = harness("test", None);
    let dialog = &mut harness.dialog;
    dialog.show_auth(COPY_URL, None);
    let _pending = dialog.show_manual_input("Paste the code:");
    assert!(render(dialog).contains("ctrl+x to copy"));

    dialog.handle_input("\x18");
    assert!(render(dialog).contains("Copied URL to clipboard"));
    assert_eq!(
        harness.copy_calls.borrow().clone(),
        vec![COPY_URL.to_string()]
    );
}

#[test]
fn login_dialog_ignores_the_copy_key_without_an_auth_url() {
    let mut harness = harness("test", None);
    let dialog = &mut harness.dialog;
    dialog.show_device_code(device());
    dialog.handle_input("\x18");
    assert!(harness.copy_calls.borrow().is_empty());
}

#[test]
fn copies_the_url_shown_at_key_press_even_if_the_view_is_replaced_before_the_copy_runs() {
    type Pending = Rc<RefCell<Option<std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>>>>;
    let mut harness = harness("test", None);
    let pending: Pending = Rc::new(RefCell::new(None));
    let pending_log = Rc::clone(&pending);
    harness.dialog.set_spawn(Rc::new(move |future| {
        *pending_log.borrow_mut() = Some(future);
    }));
    let dialog = &mut harness.dialog;
    dialog.show_auth(COPY_URL, None);

    dialog.handle_input("\x18");
    // TS `void this.authUrl.copy()` 在按键时同步调用 `copyToClipboard(this.url)`。
    assert_eq!(
        harness.copy_calls.borrow().clone(),
        vec![COPY_URL.to_string()]
    );

    // 复制 future 首次 poll 之前，授权 URL 槽位被新的 showAuth 替换。
    dialog.show_auth("https://auth.example.invalid/second", None);
    let future = pending
        .borrow_mut()
        .take()
        .expect("pending auth url copy task");
    block_on(future);

    // 复制的仍是按键时展示的 URL；结果只写回旧实例，新视图的提示行不受影响。
    assert_eq!(
        harness.copy_calls.borrow().clone(),
        vec![COPY_URL.to_string()]
    );
    let output = render(dialog);
    assert!(output.contains("https://auth.example.invalid/second"));
    assert!(!output.contains("Copied URL to clipboard"));
}

#[test]
fn deferred_auth_url_copy_still_copies_when_a_device_code_replaces_the_view() {
    type Pending = Rc<RefCell<Option<std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>>>>;
    let mut harness = harness("test", None);
    let pending: Pending = Rc::new(RefCell::new(None));
    let pending_log = Rc::clone(&pending);
    harness.dialog.set_spawn(Rc::new(move |future| {
        *pending_log.borrow_mut() = Some(future);
    }));
    let dialog = &mut harness.dialog;
    dialog.show_auth(COPY_URL, None);

    dialog.handle_input("\x18");
    // 槽位清空（showDeviceCode 把 authUrl 置空）发生在复制任务首次 poll 之前。
    dialog.show_device_code(device());
    let future = pending
        .borrow_mut()
        .take()
        .expect("pending auth url copy task");
    block_on(future);

    assert_eq!(
        harness.copy_calls.borrow().clone(),
        vec![COPY_URL.to_string()]
    );
    let output = render(dialog);
    assert!(output.contains("to open browser and copy code"));
    assert!(!output.contains("Copied URL to clipboard"));
}

// --- 5433-extension-oauth-prompt-input.test.ts ----------------------------

fn strip_apc_sequences(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("\x1b_") {
        output.push_str(&rest[..start]);
        rest = &rest[start..];
        match rest.find('\x07') {
            Some(end) => rest = &rest[end + 1..],
            None => return output,
        }
    }
    output.push_str(rest);
    output
}

fn render_dialog_lines(dialog: &mut LoginDialogComponent) -> Vec<String> {
    let raw = strip_ansi(&dialog.render(120).join("\n"));
    strip_apc_sequences(&raw)
        .split('\n')
        .map(|line| line.trim_end().to_string())
        .collect()
}

fn count_rendered_value(lines: &[String], value: &str) -> usize {
    lines
        .iter()
        .filter(|line| line.trim() == format!("> {value}"))
        .count()
}

fn prompt_dialog() -> LoginDialogComponent {
    theme();
    install_keybindings();
    let mut dialog = LoginDialogComponent::new(
        Rc::new(|| {}),
        "prompt-repro",
        Box::new(|_, _| {}),
        Some("Prompt Repro"),
        None,
    );
    dialog.set_open_browser(Rc::new(|_| Ok(())));
    dialog
}

#[test]
fn keeps_previous_prompt_input_stable_when_a_later_prompt_is_active() {
    let mut dialog = prompt_dialog();
    let mut first_prompt = dialog.show_prompt("First prompt:", Some("first-value"));
    dialog.handle_input("first-value");
    dialog.handle_input("\n");
    assert_eq!(block_on(&mut first_prompt).unwrap().unwrap(), "first-value");

    let mut second_prompt = dialog.show_prompt("Second prompt:", None);
    dialog.handle_input("second-secret-demo");

    let lines = render_dialog_lines(&mut dialog);
    let joined = lines.join("\n");
    assert!(joined.contains("First prompt:"));
    assert!(joined.contains("Second prompt:"));
    assert_eq!(count_rendered_value(&lines, "first-value"), 1);
    assert_eq!(count_rendered_value(&lines, "second-secret-demo"), 1);

    dialog.handle_input("\n");
    assert_eq!(
        block_on(&mut second_prompt).unwrap().unwrap(),
        "second-secret-demo"
    );
}

#[test]
fn preserves_auth_instructions_when_showing_a_prompt() {
    let mut dialog = prompt_dialog();
    dialog.show_auth(
        "https://example.invalid/login",
        Some("Authorize the extension"),
    );
    drop(dialog.show_prompt("First prompt:", None));

    let output = render_dialog_lines(&mut dialog).join("\n");
    assert!(output.contains("https://example.invalid/login"));
    assert!(output.contains("Authorize the extension"));
    assert!(output.contains("First prompt:"));
}

#[test]
fn preserves_neutral_information_and_links_when_showing_a_prompt() {
    let mut dialog = prompt_dialog();
    dialog.show_info(
        "Configure credentials outside kiana.",
        &[kiana_ai::auth::types::AuthInfoLink {
            url: "https://example.invalid/docs".to_string(),
            label: Some("Provider documentation".to_string()),
        }],
        false,
    );
    drop(dialog.show_prompt("Press Enter to continue:", None));

    let output = render_dialog_lines(&mut dialog).join("\n");
    assert!(output.contains("Configure credentials outside kiana."));
    assert!(output.contains("Provider documentation: https://example.invalid/docs"));
    assert!(output.contains("Press Enter to continue:"));
}

#[test]
fn preserves_setup_details_when_showing_a_prompt() {
    let mut dialog = prompt_dialog();
    dialog.show_details(&["AWS credential setup:", "providers.md"]);
    drop(dialog.show_prompt("Enter API key:", None));

    let output = render_dialog_lines(&mut dialog).join("\n");
    assert!(output.contains("AWS credential setup:"));
    assert!(output.contains("providers.md"));
    assert!(output.contains("Enter API key:"));
}

#[test]
fn keeps_previous_manual_input_stable_when_a_later_prompt_is_active() {
    let mut dialog = prompt_dialog();
    let mut manual_input = dialog.show_manual_input("Paste callback URL:");
    dialog.handle_input("callback-value");
    dialog.handle_input("\n");
    assert_eq!(
        block_on(&mut manual_input).unwrap().unwrap(),
        "callback-value"
    );

    let mut prompt = dialog.show_prompt("Second prompt:", None);
    dialog.handle_input("second-secret-demo");

    let lines = render_dialog_lines(&mut dialog);
    let joined = lines.join("\n");
    assert!(joined.contains("Paste callback URL:"));
    assert!(joined.contains("Second prompt:"));
    assert_eq!(count_rendered_value(&lines, "callback-value"), 1);
    assert_eq!(count_rendered_value(&lines, "second-secret-demo"), 1);

    dialog.handle_input("\n");
    assert_eq!(
        block_on(&mut prompt).unwrap().unwrap(),
        "second-secret-demo"
    );
}

// --- 渲染 golden（由真实 TS 组件在 dark 主题、120 列下生成） ---------------

fn golden_render(key: &str) -> Vec<String> {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/login-dialog-renders-dark.json"
    ))
    .expect("login dialog renders golden fixture");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("valid golden json");
    value[key]
        .as_array()
        .expect("golden array")
        .iter()
        .map(|line| line.as_str().expect("golden line").to_string())
        .collect()
}

fn render_exact(dialog: &mut LoginDialogComponent, width: usize) -> Vec<String> {
    dialog
        .render(width)
        .into_iter()
        .map(|line| strip_apc_sequences(&strip_ansi(&line)))
        .collect()
}

fn bare_dialog(provider_id: &str, provider_name_override: Option<&str>) -> LoginDialogComponent {
    let mut dialog = LoginDialogComponent::new(
        Rc::new(|| {}),
        provider_id,
        Box::new(|_, _| {}),
        provider_name_override,
        None,
    );
    dialog.set_open_browser(Rc::new(|_| Ok(())));
    dialog
}

#[test]
fn login_dialog_renders_like_the_ts_component() {
    theme();
    install_keybindings();

    let mut dialog = bare_dialog("custom-provider", None);
    dialog.set_focused(true);
    dialog.show_device_code(device());
    dialog.show_waiting("Waiting for authentication...");
    assert_eq!(
        render_exact(&mut dialog, 120),
        golden_render("loginDeviceCode")
    );

    let mut details = bare_dialog("prompt-repro", Some("Prompt Repro"));
    details.show_details(&["AWS credential setup:", "providers.md"]);
    drop(details.show_prompt("Enter API key:", None));
    details.handle_input("abc");
    assert_eq!(
        render_exact(&mut details, 120),
        golden_render("loginDetailsPrompt")
    );

    let mut neutral = bare_dialog("info-provider", None);
    neutral.show_info(
        "Configure credentials outside kiana.",
        &[kiana_ai::auth::types::AuthInfoLink {
            url: "https://example.invalid/docs".to_string(),
            label: Some("Provider documentation".to_string()),
        }],
        true,
    );
    assert_eq!(render_exact(&mut neutral, 120), golden_render("loginInfo"));

    let mut manual = bare_dialog("manual-provider", None);
    drop(manual.show_manual_input("Paste callback URL:"));
    manual.handle_input("code");
    assert_eq!(render_exact(&mut manual, 120), golden_render("loginManual"));
}

// --- 鼠标事件转发（Container.handleMouse） ---------------------------------

/// 回归（minor）：TS `LoginDialogComponent extends Container`，`Container.handleMouse`
/// 把点击交给内容区里的 `Input`（定位光标）。Rust 之前没有覆写 `handle_mouse`。
#[test]
fn login_dialog_forwards_clicks_to_the_manual_input() {
    let mut harness = harness("custom-provider", None);
    let dialog = &mut harness.dialog;
    let mut receiver = dialog.show_prompt("Token", None);
    dialog.handle_input("abcd");
    let lines = dialog.render(120);
    let height = lines.len();
    let row = lines
        .iter()
        .position(|line| strip_apc_sequences(&strip_ansi(line)).contains("> abcd"))
        .expect("input row");
    let press = dialog
        .handle_mouse(&mouse_event(
            TuiMouseEventType::Press,
            4,
            row as isize,
            120,
            height,
        ))
        .expect("press must reach the Input");
    assert_eq!(press.handled, Some(true));
    assert_eq!(press.focus, Some(true));

    // x=4 → 可见列 2 → 光标落在 "ab|cd"，提交值应为 "abXcd"。
    dialog.handle_input("X");
    dialog.handle_input("\r");
    assert_eq!(block_on(&mut receiver).unwrap().unwrap(), "abXcd");
}
