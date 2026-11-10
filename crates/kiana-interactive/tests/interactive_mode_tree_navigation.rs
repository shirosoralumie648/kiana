//! `interactive-mode-tree-navigation.test.ts` 的移植（InteractiveMode 侧）。
//!
//! TS 用伪造的 `this` 驱动 `showTreeSelector` / `handleCloneCommand`；Rust 无法伪造
//! `this`，因此这里用真实会话 + 测试接缝驱动同一批分支：
//! 选中当前 leaf / 导航到历史用户消息 / user-message 选择器挂载。

#![allow(
    clippy::await_holding_lock,
    reason = "主题初始化是进程级状态：同文件的用例必须串行（TS 的 beforeAll(initTheme) 等价）"
)]

mod common;

use std::sync::{Mutex, MutexGuard};

use kiana_config::settings::TuiMode;
use kiana_interactive::interactive_mode::{InteractiveMode, InteractiveModeOptions};
use kiana_session_core::session_manager::SessionManager;

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
    unsafe { std::env::set_var("KIANA_OFFLINE", "1") };
    let cwd = std::env::temp_dir().to_string_lossy().into_owned();
    let host = common::runtime_host(common::RuntimeHostOptions {
        session_manager: Some(
            SessionManager::in_memory(&cwd, None, None).expect("in memory session manager"),
        ),
        ..Default::default()
    })
    .await;
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

/// 在会话里追加 用户 → 助手 → 用户 三个条目，返回第一个用户条目的 id。
fn seed_messages(mode: &InteractiveMode) -> String {
    let session = mode.session();
    session.with_session_manager_mut(|manager| {
        let first = manager
            .append_message(common::user_msg("first"))
            .expect("append user");
        manager
            .append_message(common::assistant_msg("reply"))
            .expect("append assistant");
        manager
            .append_message(common::user_msg("second"))
            .expect("append user");
        first
    })
}

// ---------------------------------------------------------------------------
// 选中当前 leaf 是 no-op
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn selecting_the_current_leaf_reports_already_at_this_point() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        seed_messages(&fixture.mode);
        let leaf = fixture
            .mode
            .session_manager()
            .get_leaf_id()
            .map(str::to_string)
            .expect("leaf");
        fixture
            .mode
            .test_navigate_tree_from_selector(leaf, true)
            .await;
        assert_eq!(
            fixture.mode.test_last_status_message(),
            "Already at this point"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// 导航到历史用户消息
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn navigating_to_a_user_message_updates_the_editor_and_status() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let target = seed_messages(&fixture.mode);
        fixture.mode.test_set_editor_text("");
        fixture
            .mode
            .test_navigate_tree_from_selector(target, true)
            .await;
        assert_eq!(
            fixture.mode.test_last_status_message(),
            "Navigated to selected point"
        );
        assert_eq!(fixture.mode.test_editor_text(), "first");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn navigating_keeps_existing_editor_text() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let target = seed_messages(&fixture.mode);
        fixture.mode.test_set_editor_text("draft");
        fixture
            .mode
            .test_navigate_tree_from_selector(target, true)
            .await;
        assert_eq!(fixture.mode.test_editor_text(), "draft");
    })
    .await;
}

// ---------------------------------------------------------------------------
// 选择器挂载
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn user_message_selector_mounts_and_focuses_its_message_list() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        seed_messages(&fixture.mode);
        fixture.mode.test_show_user_message_selector();
        assert!(fixture.mode.test_has_active_selector());
        assert!(fixture.mode.test_selector_child_address().is_some());
        // 顶层组件的 `handlesInput` 为 false：输入必须由内部消息列表处理。
        let rendered = fixture.mode.test_selector_render_text();
        assert!(rendered.contains("Fork from Message"), "{rendered}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tree_selector_mounts_for_seeded_sessions() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        seed_messages(&fixture.mode);
        fixture.mode.test_show_tree_selector(None);
        assert!(fixture.mode.test_has_active_selector());
        assert!(fixture.mode.test_selector_child_address().is_some());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn session_selector_mounts() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_show_session_selector();
        assert!(fixture.mode.test_has_active_selector());
    })
    .await;
}

// ---------------------------------------------------------------------------
// review 回归：interactive-mode-tree-navigation.test.ts 的 busy / 流式中止分支
// ---------------------------------------------------------------------------

use kiana_ai::providers::faux::FauxResponseStep;
use kiana_ai::types::AssistantMessage;
use kiana_ext_api::events::StreamingBehavior;

const BUSY_MESSAGE: &str = "Wait for the current compaction or tree navigation to finish before navigating the session tree.";

/// 让下一次（压缩摘要 / 助手消息）请求一直挂起，直到被 abort。
fn pending_response() -> FauxResponseStep {
    FauxResponseStep::message_factory(|_input| async {
        std::future::pending::<AssistantMessage>().await
    })
}

#[tokio::test(flavor = "current_thread")]
async fn navigating_while_compacting_reports_busy_without_touching_the_ui() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let target = seed_messages(&fixture.mode);
        // 压缩切点需要超过 keepRecentTokens（默认 20k）的历史；追加一条超长用户消息。
        fixture.mode.session().with_session_manager_mut(|manager| {
            manager
                .append_message(common::user_msg(&"x".repeat(120_000)))
                .expect("append large user message");
            manager
                .append_message(common::assistant_msg("large reply"))
                .expect("append assistant");
        });
        fixture._faux.set_responses(vec![pending_response()]);
        let session = fixture.mode.session();
        let compact_session = session.clone();
        let compact = tokio::task::spawn_local(async move { compact_session.compact(None).await });
        common::wait_until(|| session.is_compacting()).await;
        let leaf_before = fixture
            .mode
            .session_manager()
            .get_leaf_id()
            .map(str::to_string);

        fixture
            .mode
            .test_navigate_tree_from_selector(target, true)
            .await;

        let rendered = fixture.mode.chat_render_text();
        assert!(rendered.contains(BUSY_MESSAGE), "{rendered}");
        assert_eq!(
            fixture.mode.test_last_status_message(),
            "",
            "busy navigation must not report success"
        );
        assert_eq!(
            fixture.mode.test_active_status_kind(),
            None,
            "busy navigation must not install the branch-summary indicator"
        );
        assert_eq!(
            fixture
                .mode
                .session_manager()
                .get_leaf_id()
                .map(str::to_string),
            leaf_before,
            "the leaf must not move"
        );

        session.abort_compaction();
        // 摘要请求可能不响应取消（faux 的挂起流），直接丢弃任务即可。
        compact.abort();
        let _ = compact.await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn navigating_aborts_streaming_and_restores_queued_messages_to_the_editor() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let target = seed_messages(&fixture.mode);
        fixture._faux.set_responses(vec![pending_response()]);
        let session = fixture.mode.session();

        let prompt_session = session.clone();
        let prompt = tokio::task::spawn_local(async move {
            prompt_session
                .prompt("hi".to_string(), Default::default())
                .await
        });
        common::wait_until(|| session.is_streaming()).await;

        let steer_session = session.clone();
        let steer = tokio::task::spawn_local(async move {
            steer_session
                .prompt(
                    "queued steer".to_string(),
                    kiana_session::agent_session::PromptOptions {
                        streaming_behavior: Some(StreamingBehavior::Steer),
                        ..Default::default()
                    },
                )
                .await
        });
        common::wait_until(|| {
            session
                .get_steering_messages()
                .iter()
                .any(|message| message == "queued steer")
        })
        .await;

        fixture.mode.test_set_editor_text("");
        fixture
            .mode
            .test_navigate_tree_from_selector(target, true)
            .await;

        assert_eq!(
            fixture.mode.test_last_status_message(),
            "Navigated to selected point"
        );
        assert!(
            !session.is_streaming(),
            "the active response must be aborted before navigating"
        );
        assert!(
            session.get_steering_messages().is_empty(),
            "queued messages must be moved out of the queue"
        );
        assert_eq!(
            fixture.mode.test_editor_text(),
            "queued steer",
            "restoreQueuedMessagesToEditor must put the queued text back in the editor"
        );

        let _ = steer.await;
        let _ = prompt.await;
    })
    .await;
}

/// TS 用例「preserves operation UI when choosing Summarize/No summary while busy」：
/// 摘要对话框关闭后要重新检查 compaction 状态，错误路径不能安装摘要指示器。
#[tokio::test(flavor = "current_thread")]
async fn busy_is_rechecked_after_the_summary_dialog_closes() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let target = seed_messages(&fixture.mode);
        fixture.mode.session().with_session_manager_mut(|manager| {
            manager
                .append_message(common::user_msg(&"x".repeat(120_000)))
                .expect("append large user message");
            manager
                .append_message(common::assistant_msg("large reply"))
                .expect("append assistant");
        });
        fixture._faux.set_responses(vec![pending_response()]);
        let session = fixture.mode.session();
        let compact_session = session.clone();
        let compact = tokio::task::spawn_local(async move { compact_session.compact(None).await });
        common::wait_until(|| session.is_compacting()).await;
        let leaf_before = fixture
            .mode
            .session_manager()
            .get_leaf_id()
            .map(str::to_string);

        let mut navigate_mode = fixture.mode.clone();
        let navigate = tokio::task::spawn_local(async move {
            navigate_mode
                .test_navigate_tree_from_selector(target, false)
                .await;
        });
        common::wait_until(|| fixture.mode.test_dialog_count() == 1).await;
        // 摘要选择器的第一项是 "No summary"。
        fixture.mode.test_send_to_top_dialog("\r");
        let _ = navigate.await;

        let rendered = fixture.mode.chat_render_text();
        assert!(rendered.contains(BUSY_MESSAGE), "{rendered}");
        assert_eq!(fixture.mode.test_active_status_kind(), None);
        assert_eq!(
            fixture
                .mode
                .session_manager()
                .get_leaf_id()
                .map(str::to_string),
            leaf_before
        );

        session.abort_compaction();
        compact.abort();
        let _ = compact.await;
    })
    .await;
}

/// TS 用例「allows navigation when compaction finishes while the dialog is open」。
#[tokio::test(flavor = "current_thread")]
async fn navigating_proceeds_when_compaction_finishes_while_the_dialog_is_open() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let target = seed_messages(&fixture.mode);
        fixture.mode.session().with_session_manager_mut(|manager| {
            manager
                .append_message(common::user_msg(&"x".repeat(120_000)))
                .expect("append large user message");
            manager
                .append_message(common::assistant_msg("large reply"))
                .expect("append assistant");
        });
        fixture._faux.set_responses(vec![pending_response()]);
        let session = fixture.mode.session();
        let compact_session = session.clone();
        let compact = tokio::task::spawn_local(async move { compact_session.compact(None).await });
        common::wait_until(|| session.is_compacting()).await;

        let mut navigate_mode = fixture.mode.clone();
        let navigate = tokio::task::spawn_local(async move {
            navigate_mode
                .test_navigate_tree_from_selector(target, false)
                .await;
        });
        common::wait_until(|| fixture.mode.test_dialog_count() == 1).await;

        // 对话框打开期间压缩结束：中止压缩并等待状态清除。
        session.abort_compaction();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while session.is_compacting() {
            assert!(
                std::time::Instant::now() < deadline,
                "compaction did not settle after abort"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let _ = compact.await;

        fixture.mode.test_send_to_top_dialog("\r");
        let _ = navigate.await;

        assert_eq!(
            fixture.mode.test_last_status_message(),
            "Navigated to selected point"
        );
        let rendered = fixture.mode.chat_render_text();
        assert!(!rendered.contains(BUSY_MESSAGE), "{rendered}");
    })
    .await;
}

// ---------------------------------------------------------------------------
// `/tree` 的复制回调（review：`if (!text)` 对空字符串也要报错）
// ---------------------------------------------------------------------------

/// TS `copySelected()` 的 `getEntryCopyText` 以 `text?.trim() ? text : undefined` 收尾：
/// 空/仅空白的 custom message 得到 `undefined`，`onCopy` 收到 `undefined` 后必须
/// `showError("Selected entry has no text to copy")`，而不是复制空串并报成功。
/// （`interactive-mode.ts` 用 `if (!text)`，Rust 侧在回调里同样把空串当无文本。）
#[tokio::test(flavor = "current_thread")]
async fn copying_an_entry_with_empty_text_reports_no_text_to_copy() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.session().with_session_manager_mut(|manager| {
            manager
                .append_custom_message_entry("empty-custom", serde_json::json!([]), false, None)
                .expect("append custom message");
        });
        fixture.mode.init().await;
        common::yield_tasks(10).await;
        fixture.mode.test_show_tree_selector(None);
        common::yield_tasks(10).await;
        assert!(fixture.mode.test_has_active_selector());

        // `app.message.copy` 的默认键位是 ctrl+x。
        fixture.mode.test_ui_handle_input("\x18");
        common::wait_until(|| {
            fixture
                .mode
                .chat_render_text()
                .contains("Selected entry has no text to copy")
        })
        .await;
        assert!(
            !fixture
                .mode
                .chat_render_text()
                .contains("Copied selected message to clipboard")
        );
    })
    .await;
}
