//! `interactive-mode-import-command` / `interactive-mode-clone-command` / 9068 的
//! InteractiveMode 侧，以及 §3.2 提交分派与 §2.50.14 的 `/name`、`/copy`、`/session`、
//! `/hotkeys` 文案。

#![allow(
    clippy::await_holding_lock,
    reason = "主题初始化是进程级状态：同文件的用例必须串行（TS 的 beforeAll(initTheme) 等价）"
)]

mod common;

use std::sync::{Mutex, MutexGuard};

use kiana_config::settings::TuiMode;
use kiana_interactive::interactive_mode::{InteractiveMode, InteractiveModeOptions};

static THEME_LOCK: Mutex<()> = Mutex::new(());

fn theme() -> MutexGuard<'static, ()> {
    let guard = THEME_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    kiana_theme::init_theme(Some("dark"), false);
    guard
}

struct Fixture {
    mode: InteractiveMode,
    _faux: kiana_ai::providers::faux::FauxCore,
    _temp: tempfile::TempDir,
    _engine: common::TestEngine,
    _cwd: String,
}

async fn fixture() -> Fixture {
    fixture_with(common::RuntimeHostOptions::new()).await
}

/// 内存会话（`SessionManager.inMemory()`）：`/clone` 走 in-memory 分支（TS 的 clone 用例）。
async fn in_memory_fixture() -> Fixture {
    let cwd = std::env::temp_dir().to_string_lossy().into_owned();
    fixture_with(common::RuntimeHostOptions {
        session_manager: Some(
            kiana_session_core::session_manager::SessionManager::in_memory(&cwd, None, None)
                .expect("in memory session manager"),
        ),
        ..Default::default()
    })
    .await
}

async fn fixture_with(options: common::RuntimeHostOptions) -> Fixture {
    unsafe { std::env::set_var("KIANA_OFFLINE", "1") };
    let host = common::runtime_host(options).await;
    let common::RuntimeHost {
        runtime,
        faux,
        temp_dir,
        cwd,
        engine,
        ..
    } = host;
    let mode = InteractiveMode::new(
        runtime,
        InteractiveModeOptions {
            terminal: Some(Box::new(common::TestTerminal::new())),
            tui_mode: Some(TuiMode::Regular),
            ..Default::default()
        },
    );
    Fixture {
        mode,
        _faux: faux,
        _temp: temp_dir,
        _engine: engine,
        _cwd: cwd,
    }
}

// ---------------------------------------------------------------------------
// `/import` 参数解析（interactive-mode-import-command.test.ts）
// ---------------------------------------------------------------------------

#[test]
fn strips_quotes_from_import_path_arguments() {
    assert_eq!(
        InteractiveMode::test_path_command_argument("/import \"path/to/session.jsonl\"", "/import")
            .as_deref(),
        Some("path/to/session.jsonl")
    );
    assert_eq!(
        InteractiveMode::test_path_command_argument(
            "/import \"path with spaces/session.jsonl\"",
            "/import"
        )
        .as_deref(),
        Some("path with spaces/session.jsonl")
    );
}

#[test]
fn preserves_apostrophes_in_unquoted_import_path_arguments() {
    assert_eq!(
        InteractiveMode::test_path_command_argument("/import john's/session.jsonl", "/import")
            .as_deref(),
        Some("john's/session.jsonl")
    );
}

#[test]
fn enforces_command_token_boundaries() {
    assert_eq!(
        InteractiveMode::test_path_command_argument("/important /tmp/session.jsonl", "/import"),
        None
    );
    assert_eq!(
        InteractiveMode::test_path_command_argument("/exporter out.html", "/export"),
        None
    );
    assert_eq!(
        InteractiveMode::test_path_command_argument("/import /tmp/session.jsonl", "/import")
            .as_deref(),
        Some("/tmp/session.jsonl")
    );
    assert_eq!(
        InteractiveMode::test_path_command_argument("/import", "/import"),
        None
    );
    assert_eq!(
        InteractiveMode::test_path_command_argument("/import   ", "/import"),
        None
    );
}

// ---------------------------------------------------------------------------
// 提交分派（§3.2）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn dispatch_clears_the_editor_for_commands() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_set_editor_text("/session");
        fixture.mode.test_dispatch_submit("/session");
        assert_eq!(fixture.mode.test_editor_text(), "");
        let rendered = fixture.mode.chat_render_text();
        assert!(rendered.contains("Session Info"), "{rendered}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn non_command_input_goes_to_pending_user_inputs() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_dispatch_submit("hello there");
        assert_eq!(fixture.mode.test_pending_user_input_count(), 1);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn settings_command_mounts_the_selector() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_set_editor_text("/settings");
        fixture.mode.test_dispatch_submit("/settings");
        assert_eq!(fixture.mode.test_editor_text(), "");
        assert!(fixture.mode.test_has_active_selector());
        assert!(fixture.mode.test_selector_child_address().is_some());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn model_command_without_arguments_mounts_the_model_selector() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_set_editor_text("/model");
        fixture.mode.test_dispatch_submit("/model");
        common::yield_tasks(20).await;
        assert!(fixture.mode.test_has_active_selector());
        assert!(fixture.mode.test_selector_child_address().is_some());
    })
    .await;
}

// ---------------------------------------------------------------------------
// `/name`
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn name_without_argument_reports_usage() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_handle_name_command("/name");
        let rendered = fixture.mode.chat_render_text();
        assert!(rendered.contains("Usage: /name <name>"), "{rendered}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn name_sets_the_session_name() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_handle_name_command("/name my-session");
        let rendered = fixture.mode.chat_render_text();
        assert!(
            rendered.contains("Session name set: my-session"),
            "{rendered}"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// `/copy` 与 `/session`、`/hotkeys`
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn copy_without_messages_reports_the_exact_error() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_handle_copy_command(false, false).await;
        let rendered = fixture.mode.chat_render_text();
        assert!(
            rendered.contains("Error: No agent messages to copy yet."),
            "{rendered}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn session_command_renders_the_template() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_handle_session_command();
        let rendered = fixture.mode.chat_render_text();
        for expected in ["Session Info", "Messages", "Tokens", "Cache Warming"] {
            assert!(
                rendered.contains(expected),
                "missing {expected}: {rendered}"
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn hotkeys_command_lists_the_bash_shortcuts() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_handle_hotkeys_command();
        let rendered = fixture.mode.chat_render_text();
        assert!(rendered.contains("Keyboard Shortcuts"), "{rendered}");
        assert!(
            rendered.contains("Run bash command (excluded from context)"),
            "{rendered}"
        );
        assert!(rendered.contains("Cycle thinking level"), "{rendered}");
    })
    .await;
}

// ---------------------------------------------------------------------------
// `/tree` / `/fork` 的空会话提示
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn tree_selector_mounts_for_a_session_with_entries() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let tree_len = fixture.mode.session_manager().get_tree().len();
        assert!(
            tree_len > 0,
            "fixture session should have at least one entry"
        );
        fixture.mode.test_show_tree_selector(None);
        assert!(fixture.mode.test_has_active_selector());
        assert!(fixture.mode.test_selector_child_address().is_some());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn fork_on_empty_session_reports_no_messages() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_show_user_message_selector();
        assert_eq!(
            fixture.mode.test_last_status_message(),
            "No messages to fork from"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// `/clone`（interactive-mode-clone-command.test.ts）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn clone_without_a_leaf_reports_nothing_to_clone() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture
            .mode
            .session()
            .with_session_manager_mut(|manager| manager.reset_leaf());
        assert!(fixture.mode.session_manager().get_leaf_id().is_none());
        fixture.mode.test_handle_clone_command().await;
        assert_eq!(
            fixture.mode.test_last_status_message(),
            "Nothing to clone yet"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn clone_forked_the_current_leaf_into_a_new_session() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = in_memory_fixture().await;
        fixture.mode.test_set_editor_text("draft");
        fixture.mode.test_handle_clone_command().await;
        assert_eq!(
            fixture.mode.test_last_status_message(),
            "Cloned to new session"
        );
        assert_eq!(fixture.mode.test_editor_text(), "");
    })
    .await;
}

// ---------------------------------------------------------------------------
// bash（9068：emit 抛错/非法结果时 fail closed）
// ---------------------------------------------------------------------------

/// `user_bash` handler 的返回值（与 `ExtensionHandlerFn` 一致）。
type UserBashHandlerResult = Result<
    Option<kiana_ext_api::events::ExtensionEventResult>,
    Box<dyn std::error::Error + Send + Sync>,
>;

/// TS `9068-user-bash-fail-closed.test.ts` 的交互式用例：`user_bash` handler 返回 `{}`
/// （非法结果）时，`emitUserBash` 抛错、不落回本地执行。
///
/// Rust 的类型化 handler 无法直接返回 JS 的 `{}`；这里返回序列化后恰为 `{}` 的
/// `BeforeProviderRequest(json!({}))`，它和 TS 的 `return {}` 走同一个 JS 侧
/// `isUserBashEventResult` 校验（canonical 的 `Invalid user_bash handler result…`）。
fn invalid_user_bash_result_factory(
    events: std::sync::Arc<std::sync::Mutex<Vec<kiana_ext_api::events::UserBashEvent>>>,
) -> kiana_ext_api::types::ExtensionFactory {
    std::sync::Arc::new(
        move |kiana: std::sync::Arc<dyn kiana_ext_api::types::ExtensionApi>| {
            let events = std::sync::Arc::clone(&events);
            let handler: kiana_ext_api::events::ExtensionHandlerFn =
                std::sync::Arc::new(move |event, _ctx| {
                    let events = std::sync::Arc::clone(&events);
                    let future: kiana_ext_api::LocalBoxFuture<'static, UserBashHandlerResult> =
                        Box::pin(async move {
                            if let kiana_ext_api::events::ExtensionEvent::UserBash(event) = &event {
                                events.lock().expect("user_bash log").push(event.clone());
                            }
                            Ok(Some(
                                kiana_ext_api::events::ExtensionEventResult::BeforeProviderRequest(
                                    serde_json::json!({}),
                                ),
                            ))
                        });
                    future
                });
            let _ = kiana.on(
                kiana_ext_api::events::ExtensionEventKind::UserBash,
                kiana_ext_api::events::ExtensionHandler::Standard(handler),
            );
            Box::pin(async {})
        },
    )
}

#[tokio::test(flavor = "current_thread")]
async fn user_bash_invalid_extension_result_fails_closed_from_the_editor() {
    let _theme = theme();
    common::run_local(async {
        let events: std::sync::Arc<std::sync::Mutex<Vec<kiana_ext_api::events::UserBashEvent>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut fixture = fixture_with(common::RuntimeHostOptions {
            extension_factory: Some(invalid_user_bash_result_factory(std::sync::Arc::clone(
                &events,
            ))),
            ..Default::default()
        })
        .await;
        fixture.mode.init().await;
        common::yield_tasks(20).await;
        let session_cwd = fixture.mode.session_manager().get_cwd().to_string();
        // runner 已把非法结果按 extension_error 上报（TS 的 runner.report）。
        let errors: std::sync::Arc<std::sync::Mutex<Vec<kiana_ext_api::types::ExtensionError>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let errors = std::sync::Arc::clone(&errors);
            let _ = fixture
                .mode
                .session()
                .extension_runner()
                .on_error(std::sync::Arc::new(move |error| {
                    errors.lock().expect("error log").push(error);
                }));
        }

        for (index, (input, exclude_from_context)) in
            [("!pwd", false), ("!!pwd", true)].into_iter().enumerate()
        {
            fixture.mode.test_set_editor_text(input);
            fixture.mode.test_ui_handle_input("\r");
            common::wait_until(|| events.lock().expect("user_bash log").len() > index).await;

            let events = events.lock().expect("user_bash log").clone();
            assert_eq!(events.len(), index + 1, "one user_bash event: {events:?}");
            assert_eq!(
                events[index],
                kiana_ext_api::events::UserBashEvent {
                    event_type: kiana_ext_api::events::UserBashEventType::UserBash,
                    command: "pwd".to_string(),
                    exclude_from_context,
                    cwd: session_cwd.clone(),
                }
            );
            // fail closed：不挂 BashExecutionComponent、不执行本地 bash。
            let rendered = fixture.mode.chat_render_text();
            assert!(!rendered.contains("$ pwd"), "{rendered}");
            assert!(!fixture.mode.session().is_bash_running());
            common::wait_until(|| !fixture.mode.test_is_bash_mode()).await;

            let errors = errors.lock().expect("error log").clone();
            assert_eq!(errors.len(), index + 1, "one extension error: {errors:?}");
            assert_eq!(errors[index].event, "user_bash");
            assert!(
                errors[index]
                    .error
                    .contains("Invalid user_bash handler result"),
                "{}",
                errors[index].error
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn bash_command_runs_and_clears_bash_mode() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture
            .mode
            .test_handle_bash_command("echo hi", false)
            .await;
        let rendered = fixture.mode.chat_render_text();
        assert!(rendered.contains("$ echo hi"), "{rendered}");
        assert!(rendered.contains("hi"), "{rendered}");
    })
    .await;
}

// ---------------------------------------------------------------------------
// `/export` 到 .jsonl（§2.50.14）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn export_to_jsonl_writes_the_file_and_reports_the_path() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("exported.jsonl");
        let path = path.to_string_lossy().into_owned();
        fixture
            .mode
            .test_handle_export_command(&format!("/export {path}"))
            .await;
        assert!(std::path::Path::new(&path).exists(), "export file missing");
        assert_eq!(
            fixture.mode.test_last_status_message(),
            format!("Session exported to: {path}")
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// 真实 TUI 输入路径：编辑器/渲染器可重入（review 回归）
//
// `DefaultEditorProxy::handle_input` 在回调期间持有编辑器的 `RefCell` 可变借用、
// 输入分发期间宿主持有渲染器的可变借用。TS 的 `this` 没有这些限制，回调必须延后
// 一个本地任务完成编辑器/渲染器写入，而不是 panic。
// ---------------------------------------------------------------------------

use std::cell::RefCell;
use std::rc::Rc;

use kiana_interactive::components::tool_execution::{ToolExecutionComponent, ToolExecutionOptions};

#[tokio::test(flavor = "current_thread")]
async fn real_editor_submit_clears_the_editor_and_records_pending_input() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        common::yield_tasks(10).await;

        fixture.mode.test_ui_handle_input("hello");
        fixture.mode.test_ui_handle_input("\r");
        common::yield_tasks(20).await;

        assert_eq!(fixture.mode.test_pending_user_input_count(), 1);
        assert_eq!(fixture.mode.test_editor_text(), "");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn typing_bang_toggles_bash_mode_and_escape_restores_it() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        common::yield_tasks(10).await;

        fixture.mode.test_ui_handle_input("!");
        common::yield_tasks(10).await;
        assert!(fixture.mode.test_is_bash_mode());

        fixture.mode.test_ui_handle_input("\x1b");
        common::yield_tasks(10).await;
        assert!(!fixture.mode.test_is_bash_mode());
        assert_eq!(fixture.mode.test_editor_text(), "");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn command_submitted_from_the_editor_mounts_and_closes_its_selector() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        common::yield_tasks(10).await;

        for character in "/settings".chars() {
            fixture.mode.test_ui_handle_input(&character.to_string());
        }
        fixture.mode.test_ui_handle_input("\r");
        common::yield_tasks(20).await;
        assert!(fixture.mode.test_has_active_selector());
        assert!(fixture.mode.test_selector_child_address().is_some());

        fixture.mode.test_ui_handle_input("\x1b");
        common::yield_tasks(20).await;
        assert!(!fixture.mode.test_has_active_selector());
        assert_eq!(fixture.mode.test_editor_text(), "");
    })
    .await;
}

/// review #4：`/settings` 修改 showImages / imageWidthCells / outputPad 必须作用到
/// **已经渲染**的组件（TS 遍历 `chatContainer.children` 并做 `instanceof`/结构判定）。
#[tokio::test(flavor = "current_thread")]
async fn settings_changes_apply_to_existing_chat_components() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        common::yield_tasks(10).await;
        // `init()` 会调用 `setCapabilityOverrides` 并清空能力缓存，因此必须在它之后
        // 覆盖图片能力（否则设置列表没有 Show images / Image width 项）。
        kiana_tui::terminal_image::set_capabilities(
            kiana_tui::terminal_image::TerminalCapabilities {
                images: Some(kiana_tui::terminal_image::ImageProtocol::Kitty),
                true_color: true,
                hyperlinks: true,
            },
        );
        let tool = Rc::new(RefCell::new(ToolExecutionComponent::new(
            "read",
            "call-1",
            serde_json::json!({}),
            ToolExecutionOptions {
                show_images: true,
                image_width_cells: 60.0,
                output_pad: 1,
            },
            None,
            Rc::new(|| {}),
            std::env::temp_dir(),
        )));
        fixture.mode.test_add_tool_component_to_chat(&tool);

        // Show images: true → false.
        fixture.mode.test_show_settings_selector();
        common::yield_tasks(5).await;
        for character in "show images".chars() {
            fixture.mode.test_ui_handle_input(&character.to_string());
        }
        common::yield_tasks(5).await;
        eprintln!("SELECTOR: {}", fixture.mode.test_selector_render_text());
        eprintln!("FOCUS: {:?}", fixture.mode.test_ui_focus());
        let kb = kiana_tui::keybindings::get_keybindings();
        eprintln!(
            "CONFIRM MATCH: {} UP: {} DOWN: {}",
            kb.matches("\r", "tui.select.confirm"),
            kb.matches("\x1b[A", "tui.select.up"),
            kb.matches("\x1b[B", "tui.select.down")
        );
        fixture.mode.test_ui_handle_input("\x1b[B");
        common::yield_tasks(5).await;
        eprintln!("AFTER DOWN: {}", fixture.mode.test_selector_render_text());
        fixture.mode.test_ui_handle_input("\x1b[A");
        common::yield_tasks(5).await;
        fixture.mode.test_ui_handle_input("\r");
        common::yield_tasks(10).await;
        eprintln!(
            "AFTER: show_images={} manager={} active={}",
            tool.borrow().show_images(),
            fixture.mode.settings_manager().get_show_images(),
            fixture.mode.test_has_active_selector()
        );
        assert!(
            !tool.borrow().show_images(),
            "showImages must update mounted tools"
        );

        // Image width: 60 → 80.
        fixture.mode.test_show_settings_selector();
        common::yield_tasks(5).await;
        for character in "image width".chars() {
            fixture.mode.test_ui_handle_input(&character.to_string());
        }
        common::yield_tasks(5).await;
        fixture.mode.test_ui_handle_input("\r");
        common::yield_tasks(10).await;
        assert_eq!(tool.borrow().image_width_cells(), 80.0);

        // Output padding: 1 → 0.
        fixture.mode.test_show_settings_selector();
        common::yield_tasks(5).await;
        for character in "output padding".chars() {
            fixture.mode.test_ui_handle_input(&character.to_string());
        }
        common::yield_tasks(5).await;
        fixture.mode.test_ui_handle_input("\r");
        common::yield_tasks(10).await;
        assert_eq!(tool.borrow().output_pad(), 0);
    })
    .await;
}

/// 语言设置：`/settings` 的 Language 行写 `ui.locale`，并立即切换界面文案。
#[tokio::test(flavor = "current_thread")]
async fn settings_language_change_persists_and_localizes_pending_lines() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        common::yield_tasks(10).await;
        fixture.mode.test_show_settings_selector();
        common::yield_tasks(5).await;
        for character in "language".chars() {
            fixture.mode.test_ui_handle_input(&character.to_string());
        }
        common::yield_tasks(5).await;
        let rendered = fixture.mode.test_selector_render_text();
        assert!(rendered.contains("Language"), "{rendered}");
        assert!(rendered.contains("English"), "{rendered}");

        // English → 简体中文：回调写入代码 `zh`，并重建 translator。
        fixture.mode.test_ui_handle_input("\r");
        common::yield_tasks(10).await;
        assert_eq!(fixture.mode.settings_manager().get_ui_locale(), "zh");
        assert!(
            fixture.mode.test_last_status_message().contains("简体中文"),
            "{}",
            fixture.mode.test_last_status_message()
        );

        // 切换后新增的 pending 行立即使用中文文案。
        fixture.mode.test_note_pending_user_submission("你好");
        let lines = fixture.mode.test_pending_user_lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("发送中……"), "{lines:?}");
        assert!(lines[0].contains("你好"), "{lines:?}");
    })
    .await;
}

/// review #3：`selectModel` 的成功/失败分支都必须调用 `done()` 关闭选择器。
#[tokio::test(flavor = "current_thread")]
async fn selecting_a_model_closes_the_selector() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        common::yield_tasks(10).await;
        fixture.mode.test_show_model_selector(None);
        common::yield_tasks(20).await;
        assert!(fixture.mode.test_has_active_selector());

        fixture.mode.test_ui_handle_input("\r");
        common::yield_tasks(40).await;
        assert!(!fixture.mode.test_has_active_selector());
        assert!(
            fixture
                .mode
                .test_last_status_message()
                .starts_with("Model: "),
            "{}",
            fixture.mode.test_last_status_message()
        );
    })
    .await;
}

/// TS 的 `/name` 用 `text.replace(/^\/name\s*/, "").trim()`：JS 的 `\s`/`trim` 把
/// U+FEFF 当空白，因此 `/name \u{FEFF}` 走「无参数」分支（`Usage: /name <name>`）。
#[tokio::test(flavor = "current_thread")]
async fn name_with_a_bom_only_argument_reports_usage() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_handle_name_command("/name \u{feff}");
        let rendered = fixture.mode.chat_render_text();
        assert!(rendered.contains("Usage: /name <name>"), "{rendered}");
        assert!(!rendered.contains("Session name was normalized from"));
        assert_eq!(fixture.mode.session_manager().get_session_name(), None);
    })
    .await;
}

/// `JSON.stringify(name)` 的规范化警告（`[\r\n]+` → 空格）。
#[tokio::test(flavor = "current_thread")]
async fn name_normalization_warns_with_the_json_quoted_names() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_handle_name_command("/name a\r\nb");
        let rendered = fixture.mode.chat_render_text();
        assert!(
            rendered.contains("Session name was normalized from \"a\\r\\nb\" to \"a b\""),
            "{rendered}"
        );
    })
    .await;
}

/// `onChange` 的 `text.trimStart().startsWith("!")` 是 JS `trimStart`：U+FEFF 前缀的
/// `!` 仍进入 bash 模式。
#[tokio::test(flavor = "current_thread")]
async fn bom_prefixed_bang_still_enables_bash_mode() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        common::yield_tasks(10).await;

        fixture.mode.test_ui_handle_input("\u{feff}");
        fixture.mode.test_ui_handle_input("!");
        common::yield_tasks(10).await;
        assert!(fixture.mode.test_is_bash_mode());

        // Esc 清空 bash 模式，再输入 BOM + 普通文本不应进入 bash 模式。
        fixture.mode.test_ui_handle_input("\x1b");
        common::yield_tasks(10).await;
        fixture.mode.test_ui_handle_input("\u{feff}");
        fixture.mode.test_ui_handle_input("l");
        fixture.mode.test_ui_handle_input("s");
        common::yield_tasks(10).await;
        assert!(!fixture.mode.test_is_bash_mode());
    })
    .await;
}

/// 提交前的 `text.trim()` 是 JS 语义：U+FEFF 前缀的命令仍要命中命令表。
#[tokio::test(flavor = "current_thread")]
async fn bom_prefixed_command_still_dispatches_as_a_command() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_set_editor_text("\u{feff}/session");
        fixture.mode.test_dispatch_submit("\u{feff}/session");
        assert!(fixture.mode.chat_render_text().contains("Session Info"));
        assert_eq!(fixture.mode.test_editor_text(), "");
        assert_eq!(fixture.mode.test_pending_user_input_count(), 0);
    })
    .await;
}

/// `/thinking` 参数同样按 JS `trim` 处理（`text.slice(10).trim()`）。
#[tokio::test(flavor = "current_thread")]
async fn thinking_argument_with_a_bom_prefix_selects_the_level() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let level = *fixture
            .mode
            .session()
            .get_available_thinking_levels()
            .last()
            .expect("model supports thinking");
        let level_name = match level {
            kiana_agent_core::ThinkingLevel::Off => "off",
            kiana_agent_core::ThinkingLevel::Minimal => "minimal",
            kiana_agent_core::ThinkingLevel::Low => "low",
            kiana_agent_core::ThinkingLevel::Medium => "medium",
            kiana_agent_core::ThinkingLevel::High => "high",
            kiana_agent_core::ThinkingLevel::Xhigh => "xhigh",
            kiana_agent_core::ThinkingLevel::Max => "max",
        };
        assert_ne!(level_name, "off");
        fixture
            .mode
            .test_dispatch_submit(&format!("/thinking \u{feff}{level_name} "));
        common::yield_tasks(5).await;
        assert_eq!(
            fixture.mode.test_last_status_message(),
            format!("Thinking level: {level_name}")
        );
        assert!(
            !fixture
                .mode
                .chat_render_text()
                .contains("Unknown thinking level")
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// `/import` e2e（interactive-mode-import-command.test.ts 的确认 / 缺文件用例）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn import_confirms_then_replaces_the_current_session() {
    let _theme = theme();
    common::run_local(async {
        let dir = tempfile::tempdir().expect("temp dir");
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).expect("sessions");
        let cwd_string = cwd.to_string_lossy().into_owned();
        let manager = kiana_session_core::session_manager::SessionManager::create(
            &cwd_string,
            Some(sessions.to_string_lossy().as_ref()),
            None,
        )
        .expect("session manager");
        let mut fixture = fixture_with(common::RuntimeHostOptions {
            cwd: Some(cwd_string),
            session_manager: Some(manager),
            ..Default::default()
        })
        .await;

        let export_path = dir
            .path()
            .join("exported.jsonl")
            .to_string_lossy()
            .into_owned();
        fixture
            .mode
            .session()
            .export_to_jsonl(Some(export_path.clone()))
            .expect("export session");

        let mut import_mode = fixture.mode.clone();
        let text = format!("/import {export_path}");
        let import = tokio::task::spawn_local(async move {
            import_mode.test_handle_import_command(&text).await;
        });
        common::wait_until(|| fixture.mode.test_dialog_count() == 1).await;
        // 确认对话框的第一项是 "Yes"。
        fixture.mode.test_send_to_top_dialog("\r");
        let _ = import.await;

        assert_eq!(
            fixture.mode.test_last_status_message(),
            format!("Session imported from: {export_path}")
        );
        assert!(
            !fixture
                .mode
                .chat_render_text()
                .contains("Failed to import session"),
            "{}",
            fixture.mode.chat_render_text()
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn import_reports_a_missing_file_as_a_non_fatal_error() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let missing = std::env::temp_dir().join(format!(
            "kiana-missing-{}.jsonl",
            kiana_ai::utils::uuid::uuidv7(None).expect("uuid")
        ));
        let missing = missing.to_string_lossy().into_owned();
        assert!(!std::path::Path::new(&missing).exists());

        let mut import_mode = fixture.mode.clone();
        let text = format!("/import {missing}");
        let import = tokio::task::spawn_local(async move {
            import_mode.test_handle_import_command(&text).await;
        });
        common::wait_until(|| fixture.mode.test_dialog_count() == 1).await;
        fixture.mode.test_send_to_top_dialog("\r");
        let completed = tokio::time::timeout(std::time::Duration::from_secs(5), import).await;
        assert!(
            completed.is_ok(),
            "a missing import file must not enter the fatal runtime error path"
        );

        let rendered = fixture.mode.chat_render_text();
        assert!(
            rendered.contains(&format!(
                "Failed to import session: File not found: {missing}"
            )),
            "{rendered}"
        );
        assert_eq!(fixture.mode.test_last_status_message(), "");
    })
    .await;
}
