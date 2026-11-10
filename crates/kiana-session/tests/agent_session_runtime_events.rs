//! `test/agent-session-runtime-events.test.ts`：runtime 生命周期事件与失效顺序。
#![allow(clippy::arc_with_non_send_sync)]

mod session_support;

use std::sync::{Arc, Mutex};

use kiana_ai::types::UserContent;
use kiana_ext_api::events::{ExtensionEvent, ExtensionEventKind, ExtensionHandler, SessionEvent};
use kiana_session::agent_session::PromptOptions;
use kiana_session::agent_session_runtime::{AgentSessionRuntimeError, ForkSessionOptions};

use session_support::{RuntimeHostOptions, run, runtime_host};

thread_local! {
    static KIANA_SLOT: std::cell::RefCell<Option<Arc<dyn kiana_ext_api::types::ExtensionApi>>> =
        const { std::cell::RefCell::new(None) };
}

fn stale_panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    let payload = payload.downcast::<String>().expect("string panic");
    *payload
}

#[tokio::test]
async fn runs_before_session_invalidate_after_session_shutdown_and_before_rebind_session() {
    run(async {
        let phases: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown_phases = Arc::clone(&phases);
        let factory: kiana_ext_api::types::ExtensionFactory = Arc::new(move |kiana| {
            KIANA_SLOT.with(|slot| {
                let mut slot = slot.borrow_mut();
                // 每次会话替换都会重新加载扩展；只保留第一个（旧）kiana 句柄。
                if slot.is_none() {
                    *slot = Some(Arc::clone(&kiana));
                }
            });
            let phases = Arc::clone(&shutdown_phases);
            let _ = kiana.on(
                ExtensionEventKind::SessionShutdown,
                ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                    let phases = Arc::clone(&phases);
                    Box::pin(async move {
                        if matches!(event, ExtensionEvent::Session(SessionEvent::Shutdown(_))) {
                            phases.lock().unwrap().push("session_shutdown");
                        }
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                    })
                })),
            );
            Box::pin(async {})
        });
        let host = runtime_host(RuntimeHostOptions {
            extension_factory: Some(factory),
            ..Default::default()
        })
        .await;
        host.bind_extensions().await;
        let old_session = host.runtime.session();
        let invalidate_phases = Arc::clone(&phases);
        let old = old_session.clone();
        host.runtime
            .set_before_session_invalidate(Some(Arc::new(move || {
                // 旧 ctx 在失效前仍可用。
                assert_eq!(
                    old.extension_runner().create_context().cwd(),
                    old.session_manager().get_cwd()
                );
                invalidate_phases
                    .lock()
                    .unwrap()
                    .push("beforeSessionInvalidate");
            })));
        let rebind_phases = Arc::clone(&phases);
        host.runtime
            .set_rebind_session(Some(Arc::new(move |_session| {
                let phases = Arc::clone(&rebind_phases);
                Box::pin(async move {
                    phases.lock().unwrap().push("rebindSession");
                })
            })));

        host.runtime.new_session(None).await.expect("new session");

        assert_eq!(
            phases.lock().unwrap().clone(),
            vec![
                "session_shutdown",
                "beforeSessionInvalidate",
                "rebindSession"
            ]
        );

        // 替换后旧 ctx 与旧 kiana 都失效（TS 的 `toThrow(...)`）。
        let stale_ctx = old_session.extension_runner().create_context();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = stale_ctx.cwd();
        }))
        .expect_err("stale ctx must panic");
        assert_eq!(
            stale_panic_message(panic),
            kiana_extensions::loader::DEFAULT_STALE_MESSAGE
        );
        let stale_kiana = KIANA_SLOT
            .with(|slot| slot.borrow().clone())
            .expect("captured kiana");
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            stale_kiana.send_user_message(UserContent::Text("stale message".to_string()), None);
        }))
        .expect_err("stale kiana must panic");
        assert_eq!(
            stale_panic_message(panic),
            kiana_extensions::loader::DEFAULT_STALE_MESSAGE
        );

        host.runtime.set_before_session_invalidate(None);
        host.runtime.set_rebind_session(None);
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn emits_lifecycle_events_for_new_resume_and_fork() {
    run(async {
        let events: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let factory: kiana_ext_api::types::ExtensionFactory = {
            let events = Arc::clone(&events);
            Arc::new(move |kiana| {
                for (kind, label) in [
                    (ExtensionEventKind::SessionStart, "start"),
                    (ExtensionEventKind::SessionBeforeSwitch, "before_switch"),
                    (ExtensionEventKind::SessionBeforeFork, "before_fork"),
                    (ExtensionEventKind::SessionShutdown, "shutdown"),
                ] {
                    let events = Arc::clone(&events);
                    let _ = kiana.on(
                        kind,
                        ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                            let events = Arc::clone(&events);
                            Box::pin(async move {
                                let summary = match &event {
                                    ExtensionEvent::Session(SessionEvent::Start(event)) => {
                                        serde_json::json!({"type":"start","reason":event.reason,"previousSessionFile":event.previous_session_file})
                                    }
                                    ExtensionEvent::Session(SessionEvent::BeforeSwitch(event)) => {
                                        serde_json::json!({"type":"before_switch","reason":event.reason,"targetSessionFile":event.target_session_file})
                                    }
                                    ExtensionEvent::Session(SessionEvent::BeforeFork(event)) => {
                                        serde_json::json!({"type":"before_fork","entryId":event.entry_id,"position":event.position})
                                    }
                                    ExtensionEvent::Session(SessionEvent::Shutdown(event)) => {
                                        serde_json::json!({"type":"shutdown","reason":event.reason,"targetSessionFile":event.target_session_file})
                                    }
                                    _ => serde_json::json!({"type":"other"}),
                                };
                                let _ = label;
                                events.lock().unwrap().push(summary);
                                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                            })
                        })),
                    );
                }
                Box::pin(async {})
            })
        };
        let host = runtime_host(RuntimeHostOptions {
            extension_factory: Some(factory),
            ..Default::default()
        })
        .await;
        host.bind_extensions().await;
        assert_eq!(events.lock().unwrap().len(), 1);
        events.lock().unwrap().clear();

        host.runtime
            .session()
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let original_file = host.runtime.session().session_file();
        host.runtime.new_session(None).await.expect("new");
        host.bind_extensions().await;
        let second_file = host.runtime.session().session_file();
        let summaries = events.lock().unwrap().clone();
        assert_eq!(summaries.len(), 3);
        assert_eq!(summaries[0]["type"], "before_switch");
        assert_eq!(summaries[1]["type"], "shutdown");
        assert_eq!(summaries[2]["type"], "start");
        assert_eq!(summaries[2]["reason"], "new");
        assert_eq!(summaries[2]["previousSessionFile"], serde_json::json!(original_file));
        events.lock().unwrap().clear();

        host.runtime
            .switch_session(original_file.as_deref().unwrap(), None)
            .await
            .expect("switch");
        host.bind_extensions().await;
        let summaries = events.lock().unwrap().clone();
        assert_eq!(summaries[0]["type"], "before_switch");
        assert_eq!(summaries[0]["reason"], "resume");
        assert_eq!(summaries[1]["type"], "shutdown");
        assert_eq!(summaries[2]["type"], "start");
        assert_eq!(summaries[2]["previousSessionFile"], serde_json::json!(second_file));
        events.lock().unwrap().clear();

        let user = host.runtime.session().get_user_messages_for_forking()[0].clone();
        host.runtime.fork(&user.0, None).await.expect("fork");
        host.bind_extensions().await;
        let summaries = events.lock().unwrap().clone();
        assert_eq!(summaries[0]["type"], "before_fork");
        assert_eq!(summaries[0]["entryId"], user.0);
        assert_eq!(summaries[0]["position"], "before");
        assert_eq!(summaries[1]["type"], "shutdown");
        assert_eq!(summaries[1]["reason"], "fork");
        assert_eq!(summaries[2]["type"], "start");
        assert_eq!(summaries[2]["reason"], "fork");
        host.dispose().await;
    })
    .await;
}

#[allow(dead_code)]
fn _unused(_: AgentSessionRuntimeError, _: ForkSessionOptions) {}
