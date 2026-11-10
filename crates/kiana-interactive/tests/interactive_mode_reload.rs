//! `/reload`（§2.50.13）。

#![allow(
    clippy::arc_with_non_send_sync,
    clippy::await_holding_lock,
    reason = "单线程 LocalSet 测试：扩展工厂没有 Send/Sync 约束，且不存在并发借用者"
)]

mod common;

use std::sync::{Mutex, MutexGuard};

use kiana_ai::providers::faux::{FauxResponseStep, faux_assistant_message};
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
    temp: tempfile::TempDir,
    faux: kiana_ai::providers::faux::FauxCore,
    _engine: common::TestEngine,
}

async fn fixture(auto_trust_cwd: Option<String>) -> Fixture {
    unsafe { std::env::set_var("KIANA_OFFLINE", "1") };
    let host = common::runtime_host(common::RuntimeHostOptions::new()).await;
    let common::RuntimeHost {
        runtime,
        faux,
        temp_dir,
        engine,
        ..
    } = host;
    let mode = InteractiveMode::new(
        runtime,
        InteractiveModeOptions {
            terminal: Some(Box::new(common::TestTerminal::new())),
            tui_mode: Some(TuiMode::Regular),
            auto_trust_on_reload_cwd: auto_trust_cwd,
            ..Default::default()
        },
    );
    Fixture {
        mode,
        temp: temp_dir,
        faux,
        _engine: engine,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn reload_reports_success_and_restores_the_editor() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture(None).await;
        fixture.mode.init().await;
        fixture.mode.test_handle_reload_command().await;
        let rendered = fixture.mode.chat_render_text();
        assert!(
            rendered.contains(
                "Reloaded keybindings, extensions, skills, prompts, themes, and context files"
            ),
            "{rendered}"
        );
        assert_eq!(fixture.mode.test_editor_slot_child_count(), 1);
        let _ = &fixture.temp;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn reload_while_streaming_warns_and_does_not_reload() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture(None).await;
        fixture.mode.init().await;
        fixture
            .faux
            .set_responses(vec![FauxResponseStep::message_factory(|_input| async {
                std::future::pending::<kiana_ai::types::AssistantMessage>().await
            })]);
        let session = fixture.mode.session();
        let prompt_session = session.clone();
        let prompt = tokio::task::spawn_local(async move {
            prompt_session
                .prompt("hi".to_string(), Default::default())
                .await
        });
        common::wait_until(|| session.is_streaming()).await;

        fixture.mode.test_handle_reload_command().await;
        let rendered = fixture.mode.chat_render_text();
        assert!(
            rendered.contains("Wait for the current response to finish before reloading."),
            "{rendered}"
        );
        assert!(
            !rendered.contains("Reloaded keybindings"),
            "reload must not run while streaming: {rendered}"
        );
        session.request_abort();
        let _ = prompt.await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn reload_saves_implicit_project_trust_when_flagged() {
    let _theme = theme();
    common::run_local(async {
        let host = common::runtime_host(common::RuntimeHostOptions::new()).await;
        let cwd = host.cwd.clone();
        let common::RuntimeHost {
            runtime,
            faux,
            temp_dir,
            engine,
            ..
        } = host;
        std::fs::create_dir_all(std::path::Path::new(&cwd).join(".kiana/skills/demo"))
            .expect("skills dir");
        std::fs::write(
            std::path::Path::new(&cwd).join(".kiana/skills/demo/SKILL.md"),
            "---\nname: demo\ndescription: demo\n---\nbody\n",
        )
        .expect("skill file");
        let mut mode = InteractiveMode::new(
            runtime,
            InteractiveModeOptions {
                terminal: Some(Box::new(common::TestTerminal::new())),
                tui_mode: Some(TuiMode::Regular),
                auto_trust_on_reload_cwd: Some(cwd.clone()),
                ..Default::default()
            },
        );
        mode.settings_manager().set_project_trusted(true);
        mode.init().await;
        mode.test_handle_reload_command().await;
        let rendered = mode.chat_render_text();
        assert!(
            rendered.contains(
                "Reloaded keybindings, extensions, skills, prompts, themes, and context files; saved project trust"
            ),
            "{rendered}"
        );
        let trust = std::path::Path::new(&engine_agent_dir(&temp_dir)).join("trust.json");
        assert!(trust.exists(), "trust.json should be written: {}", trust.display());
        let _ = (faux, engine);

        fn engine_agent_dir(temp: &tempfile::TempDir) -> String {
            temp.path().join("agent").to_string_lossy().into_owned()
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn reload_ignores_faux_assistant_messages_helper() {
    // 该用例只保证测试夹具里的 faux 助手消息类型可用（避免未使用导入）。
    let message = faux_assistant_message("ok", Default::default());
    assert_eq!(message.content.len(), 1);
}

// ---------------------------------------------------------------------------
// `5943-session-start-notify.test.ts`：session_start 期间的顺序保证
// ---------------------------------------------------------------------------

use std::sync::Arc;

use kiana_ai::types::UserContent;
use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventKind, ExtensionHandler, SessionEvent, SessionStartReason,
};
use kiana_ext_api::types::{CustomMessagePayload, ExtensionFactory, SendMessageOptions};
use kiana_ext_api::ui::NotifyKind;

fn session_start_factory(
    on_start: impl Fn(
        &kiana_ext_api::events::SessionStartEvent,
        &Arc<dyn kiana_ext_api::types::ExtensionContextApi>,
    ) + Send
    + Sync
    + 'static,
) -> ExtensionFactory {
    let on_start = Arc::new(on_start);
    Arc::new(move |kiana| {
        let on_start = Arc::clone(&on_start);
        let _ = kiana.on(
            ExtensionEventKind::SessionStart,
            ExtensionHandler::Standard(Arc::new(move |event, ctx| {
                let on_start = Arc::clone(&on_start);
                Box::pin(async move {
                    if let ExtensionEvent::Session(SessionEvent::Start(start)) = &event {
                        on_start(start, &ctx);
                    }
                    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                })
            })),
        );
        Box::pin(async {})
    })
}

async fn fixture_with_factory(factory: ExtensionFactory) -> Fixture {
    unsafe { std::env::set_var("KIANA_OFFLINE", "1") };
    let host = common::runtime_host(common::RuntimeHostOptions {
        extension_factory: Some(factory),
        ..Default::default()
    })
    .await;
    let common::RuntimeHost {
        runtime,
        faux,
        temp_dir,
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
        temp: temp_dir,
        faux,
        _engine: engine,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn reload_rebuilds_chat_before_session_start_handlers_notify() {
    let _theme = theme();
    common::run_local(async {
        let factory = session_start_factory(|start, ctx| {
            if start.reason == SessionStartReason::Reload {
                ctx.ui().notify("reload notice", Some(NotifyKind::Error));
            }
        });
        let mut fixture = fixture_with_factory(factory).await;
        fixture.mode.init().await;
        fixture.mode.test_handle_reload_command().await;
        common::wait_until(|| fixture.mode.chat_render_text().contains("reload notice")).await;
        let rendered = fixture.mode.chat_render_text();
        assert!(rendered.contains("Error: reload notice"), "{rendered}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn new_session_renders_state_before_session_start_handlers_notify() {
    let _theme = theme();
    common::run_local(async {
        let factory = session_start_factory(|start, ctx| {
            if start.reason == SessionStartReason::New {
                ctx.ui().notify("Hello Error", Some(NotifyKind::Error));
            }
        });
        let mut fixture = fixture_with_factory(factory).await;
        fixture.mode.init().await;
        let before = fixture.mode.test_session_id();
        fixture.mode.test_new_session().await;
        assert_ne!(fixture.mode.test_session_id(), before);
        common::wait_until(|| fixture.mode.chat_render_text().contains("Hello Error")).await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn new_session_subscribes_before_session_start_sends_messages() {
    let _theme = theme();
    common::run_local(async {
        let factory: ExtensionFactory = Arc::new(move |kiana| {
            let kiana_for_handler = Arc::clone(&kiana);
            let _ = kiana.on(
                ExtensionEventKind::SessionStart,
                ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                    let kiana = Arc::clone(&kiana_for_handler);
                    Box::pin(async move {
                        if let ExtensionEvent::Session(SessionEvent::Start(start)) = &event
                            && start.reason == SessionStartReason::New
                        {
                            kiana.send_message(SendMessageOptions {
                                message: CustomMessagePayload {
                                    custom_type: "session-start".to_string(),
                                    content: UserContent::Text("custom from start".to_string()),
                                    display: true,
                                    details: None,
                                },
                                trigger_turn: None,
                                deliver_as: None,
                            });
                        }
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                    })
                })),
            );
            Box::pin(async {})
        });
        let mut fixture = fixture_with_factory(factory).await;
        fixture.mode.init().await;
        fixture.mode.test_new_session().await;
        common::wait_until(|| {
            fixture
                .mode
                .chat_render_text()
                .contains("custom from start")
        })
        .await;
        assert_eq!(
            fixture
                .mode
                .chat_render_text()
                .matches("custom from start")
                .count(),
            1
        );
    })
    .await;
}

/// 处理器**同步**写入 chat 的路径（扩展错误监听器）：重建必须先发生，否则输出被清掉。
#[tokio::test(flavor = "current_thread")]
async fn reload_rebuilds_chat_before_session_start_handler_errors_are_rendered() {
    let _theme = theme();
    common::run_local(async {
        let factory: ExtensionFactory = Arc::new(move |kiana| {
            let _ = kiana.on(
                ExtensionEventKind::SessionStart,
                ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                    Box::pin(async move {
                        if let ExtensionEvent::Session(SessionEvent::Start(start)) = &event
                            && start.reason == SessionStartReason::Reload
                        {
                            return Err("boom".into());
                        }
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                    })
                })),
            );
            Box::pin(async {})
        });
        let mut fixture = fixture_with_factory(factory).await;
        fixture.mode.init().await;
        fixture.mode.test_handle_reload_command().await;
        let rendered = fixture.mode.chat_render_text();
        assert!(
            rendered.contains("error: boom"),
            "handler error output must survive the reload rebuild: {rendered}"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// 第二轮审阅补齐：5943 的 hideThinkingBlock / reload blocker，以及
// startup-session-rebind-duplicate-subscription
// ---------------------------------------------------------------------------

/// reload 钩子先刷新 `hideThinkingBlock`，再重建聊天（TS 的顺序 [reload, rebuild:true, start:true]）。
#[tokio::test(flavor = "current_thread")]
async fn reload_refreshes_hide_thinking_block_before_rebuilding_chat() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture(None).await;
        fixture.mode.init().await;
        assert!(!fixture.mode.test_hide_thinking_block());
        fixture
            .mode
            .settings_manager()
            .set_hide_thinking_block(true);

        // reload 的 beforeSessionStart 钩子（会话切换前执行）。
        fixture.mode.test_restore_chat_before_session_start();
        assert!(
            fixture.mode.test_hide_thinking_block(),
            "restoreChatBeforeSessionStart must refresh hideThinkingBlock before the rebuild"
        );

        // 完整 reload 后设置仍然生效。
        fixture.mode.test_handle_reload_command().await;
        assert!(fixture.mode.test_hide_thinking_block());
    })
    .await;
}

/// 异步 reload 进行期间，reload blocker 仍是编辑器槽的焦点组件。
#[tokio::test(flavor = "current_thread")]
async fn reload_keeps_the_blocker_focused_until_async_reload_completes() {
    let _theme = theme();
    common::run_local(async {
        let observations: Arc<std::sync::Mutex<Vec<(bool, bool)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&observations);
        let factory: ExtensionFactory = Arc::new(move |kiana| {
            let recorded = Arc::clone(&recorded);
            let _ = kiana.on(
                ExtensionEventKind::SessionStart,
                ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                    let recorded = Arc::clone(&recorded);
                    Box::pin(async move {
                        if let ExtensionEvent::Session(SessionEvent::Start(start)) = &event
                            && start.reason == SessionStartReason::Reload
                            && let Some(mode) =
                                kiana_interactive::interactive_mode::session::current_mode_for_test(
                                )
                        {
                            recorded
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .push((
                                    mode.test_editor_slot_is_focused(),
                                    mode.test_editor_slot_text()
                                        .contains("Reloading keybindings"),
                                ));
                        }
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                    })
                })),
            );
            Box::pin(async {})
        });

        let mut fixture = fixture_with_factory(factory).await;
        fixture.mode.init().await;
        fixture.mode.test_handle_reload_command().await;

        let observations = observations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        assert_eq!(observations.len(), 1, "reload session_start must run once");
        assert!(
            observations[0].0,
            "the reload blocker must stay focused while the async reload runs"
        );
        assert!(
            observations[0].1,
            "the reload box must be mounted while the async reload runs"
        );
        // reload 结束后恢复编辑器并聚焦。
        assert!(fixture.mode.test_editor_slot_is_focused());
        assert!(
            !fixture
                .mode
                .test_editor_slot_text()
                .contains("Reloading keybindings")
        );
    })
    .await;
}

/// 会话替换后的 rebind 只能保留一个事件订阅（重复订阅会让事件渲染两次）。
///
/// TS 的 `startup-session-rebind-duplicate-subscription` 用可控 Promise 制造"启动绑定期
/// 被替换 rebind 插入"的竞态；Rust 的 `bind_current_session_extensions` 在同一个
/// `&mut self` 上等待、无法从外部并发插入。这里用真实的 `newSession` 触发
/// replacement rebind，覆盖同一不变量：旧订阅被移除、只保留一个订阅。
#[tokio::test(flavor = "current_thread")]
async fn replacement_rebind_keeps_exactly_one_agent_subscription() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture(None).await;
        fixture.mode.init().await;
        // 真实会话替换：旧订阅必须被移除，新会话只订阅一次。
        fixture.mode.test_new_session().await;

        fixture
            .faux
            .set_responses(vec![FauxResponseStep::message_factory(|_| async {
                faux_assistant_message("hello once", Default::default())
            })]);
        let session = fixture.mode.session();
        let prompt_session = session.clone();
        let prompt = tokio::task::spawn_local(async move {
            prompt_session
                .prompt("hi".to_string(), Default::default())
                .await
        });
        common::wait_until(|| fixture.mode.chat_render_text().contains("hello once")).await;
        let rendered = fixture.mode.chat_render_text();
        assert_eq!(
            rendered.matches("hello once").count(),
            1,
            "a replacement rebind must not install a duplicate subscription: {rendered}"
        );
        session.request_abort();
        let _ = prompt.await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn reload_applies_an_externally_edited_ui_locale() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture(None).await;
        fixture.mode.init().await;
        fixture.mode.test_apply_ui_locale("en");
        assert_eq!(fixture.mode.test_ui_locale(), "en");

        // 模拟用户在运行期编辑 settings.json 的 ui.locale（不经过 settings 面板的
        // 即时回调），再执行 /reload。
        fixture.mode.settings_manager().set_ui_locale("zh");
        fixture.mode.test_handle_reload_command().await;
        assert_eq!(
            fixture.mode.test_ui_locale(),
            "zh",
            "reload must rebuild the UI translator from the new ui.locale"
        );
        let _ = &fixture.temp;
    })
    .await;
}
