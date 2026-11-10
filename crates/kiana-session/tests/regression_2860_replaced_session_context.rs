//! regression #2860: 替换后的会话回调（`withSession`）绑定到新会话。
//!
//! TS 通过扩展命令 ctx 触发（`ctx.newSession({withSession})`）。Rust 的命令上下文动作是
//! `Send + Sync` 闭包，而 `AgentSessionRuntime` 是 `!Send`（资源加载器 trait 不带
//! Send/Sync），因此测试用线程本地句柄把动作接回同一会话线程上的 runtime（app 层的等价
//! 装配方式）。
#![allow(clippy::arc_with_non_send_sync)]

mod session_support;

use std::cell::RefCell;
use std::panic::AssertUnwindSafe;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use kiana_ai::types::UserContent;
use kiana_ext_api::types::SendUserMessageOptions;
use kiana_ext_api::types::{
    ExtensionApi, ExtensionCommandContextActions, ExtensionCommandContextApi, NewSessionOptions,
    NewSessionResult, RegisterCommandOptions,
};
use kiana_session::agent_session::{ExtensionBindings, PromptOptions};
use kiana_session::agent_session_runtime::{
    AgentSessionRuntime, ForkSessionOptions, NewSessionOptionsInput, ReplacedSessionCallback,
    SwitchSessionOptions,
};
use serde_json::json;

use session_support::{RuntimeHostOptions, message_text, run, runtime_host};

thread_local! {
    /// 当前测试的 runtime（场景动作在会话线程上取回；`AgentSessionRuntime` 是 `!Send`）。
    static RUNTIME_SLOT: RefCell<Option<Rc<AgentSessionRuntime>>> = const { RefCell::new(None) };
    /// 旧会话的扩展 API（`ExtensionApi` 不是 `Send`，与 agent-session-runtime-events 同法）。
    static KIANA_SLOT: RefCell<Option<Arc<dyn ExtensionApi>>> = const { RefCell::new(None) };
}

fn runtime_handle() -> Rc<AgentSessionRuntime> {
    RUNTIME_SLOT.with(|slot| slot.borrow().as_ref().expect("runtime installed").clone())
}

/// `bindExtensions({ commandContextActions })`（TS 的 `rebindSession`）。
fn command_bindings() -> ExtensionBindings {
    ExtensionBindings {
        command_context_actions: Some(ExtensionCommandContextActions {
            wait_for_idle: Arc::new(|| {
                Box::pin(async move {
                    runtime_handle().session().wait_for_idle().await;
                })
            }),
            new_session: Arc::new(|options| {
                Box::pin(async move {
                    let options = options.map(|options| NewSessionOptionsInput {
                        parent_session: options.parent_session,
                        setup: options.setup,
                        with_session: options.with_session.map(|callback| {
                            let callback: ReplacedSessionCallback = callback;
                            callback
                        }),
                    });
                    let result = runtime_handle()
                        .new_session(options)
                        .await
                        .expect("new session");
                    NewSessionResult {
                        cancelled: result.cancelled,
                    }
                })
            }),
            fork: Arc::new(|entry_id, options| {
                Box::pin(async move {
                    let options = options.map(|options| ForkSessionOptions {
                        position: options.position,
                        with_session: options.with_session.map(|callback| {
                            let callback: ReplacedSessionCallback = callback;
                            callback
                        }),
                    });
                    let result = runtime_handle()
                        .fork(&entry_id, options)
                        .await
                        .expect("fork");
                    NewSessionResult {
                        cancelled: result.cancelled,
                    }
                })
            }),
            navigate_tree: Arc::new(|_target_id, _options| {
                Box::pin(async { NewSessionResult { cancelled: false } })
            }),
            switch_session: Arc::new(|session_path, options| {
                Box::pin(async move {
                    let options = options.map(|options| SwitchSessionOptions {
                        with_session: options.with_session.map(|callback| {
                            let callback: ReplacedSessionCallback = callback;
                            callback
                        }),
                        ..Default::default()
                    });
                    let result = runtime_handle()
                        .switch_session(&session_path, options)
                        .await
                        .expect("switch session");
                    NewSessionResult {
                        cancelled: result.cancelled,
                    }
                })
            }),
            reload: Arc::new(|| Box::pin(async {})),
        }),
        ..Default::default()
    }
}

fn stale_panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_string(),
            Err(_) => "non-string panic".to_string(),
        },
    }
}

fn send_callback(
    messages: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> ReplacedSessionCallback {
    std::sync::Arc::new(move |ctx| {
        let messages = std::sync::Arc::clone(&messages);
        Box::pin(async move {
            ctx.send_user_message(
                UserContent::Text("Hello from the new session!".to_string()),
                Some(SendUserMessageOptions {
                    deliver_as: None,
                    expand_prompt_templates: None,
                }),
            )
            .await;
            messages.lock().unwrap().push("with-session".to_string());
        })
    })
}

#[tokio::test]
async fn with_session_targets_the_replacement_session() {
    run(async {
        let host = runtime_host(RuntimeHostOptions::default()).await;
        host.set_responses(vec![kiana_ai::providers::faux::FauxResponseStep::Message(
            kiana_ai::providers::faux::faux_assistant_message("hello reply", Default::default()),
        )]);
        host.bind_extensions().await;
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let old_file = host.runtime.session().session_file();
        let result = host
            .runtime
            .new_session(Some(NewSessionOptionsInput {
                parent_session: old_file.clone(),
                setup: None,
                with_session: Some(send_callback(std::sync::Arc::clone(&calls))),
            }))
            .await
            .expect("new session");
        assert!(!result.cancelled);
        let session = host.runtime.session();
        let non_system: Vec<String> = session
            .messages()
            .iter()
            .filter(|message| message.role() != "system")
            .map(|message| format!("{}:{}", message.role(), message_text(message)))
            .collect();
        assert_eq!(
            non_system,
            vec![
                "user:Hello from the new session!".to_string(),
                "assistant:hello reply".to_string()
            ]
        );
        assert_eq!(
            calls.lock().unwrap().clone(),
            vec!["with-session".to_string()]
        );
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn with_session_targets_the_new_session_after_seed_prompt() {
    run(async {
        let host = runtime_host(RuntimeHostOptions::default()).await;
        host.set_responses(vec![
            kiana_ai::providers::faux::FauxResponseStep::Message(
                kiana_ai::providers::faux::faux_assistant_message("seed reply", Default::default()),
            ),
            kiana_ai::providers::faux::FauxResponseStep::Message(
                kiana_ai::providers::faux::faux_assistant_message("fork reply", Default::default()),
            ),
        ]);
        host.bind_extensions().await;
        host.runtime
            .session()
            .prompt("seed".to_string(), PromptOptions::default())
            .await
            .expect("seed");
        let leaf_id = host
            .runtime
            .session()
            .session_manager()
            .get_leaf_id()
            .map(str::to_string)
            .expect("leaf");
        let result = host
            .runtime
            .fork(
                &leaf_id,
                Some(kiana_session::agent_session_runtime::ForkSessionOptions {
                    position: Some(kiana_ext_api::events::ForkPosition::At),
                    with_session: Some(send_callback(std::sync::Arc::new(std::sync::Mutex::new(
                        Vec::new(),
                    )))),
                }),
            )
            .await
            .expect("fork");
        assert!(!result.cancelled);
        let non_system: Vec<String> = host
            .runtime
            .session()
            .messages()
            .iter()
            .filter(|message| message.role() != "system")
            .map(|message| format!("{}:{}", message.role(), message_text(message)))
            .collect();
        assert_eq!(
            non_system,
            vec![
                "user:seed".to_string(),
                "assistant:seed reply".to_string(),
                "user:Hello from the new session!".to_string(),
                "assistant:fork reply".to_string(),
            ]
        );
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn with_session_targets_the_switched_session() {
    run(async {
        let host = runtime_host(RuntimeHostOptions::default()).await;
        host.set_responses(vec![
            kiana_ai::providers::faux::FauxResponseStep::Message(
                kiana_ai::providers::faux::faux_assistant_message("root reply", Default::default()),
            ),
            kiana_ai::providers::faux::FauxResponseStep::Message(
                kiana_ai::providers::faux::faux_assistant_message(
                    "target reply",
                    Default::default(),
                ),
            ),
            kiana_ai::providers::faux::FauxResponseStep::Message(
                kiana_ai::providers::faux::faux_assistant_message(
                    "switch reply",
                    Default::default(),
                ),
            ),
        ]);
        host.bind_extensions().await;
        host.runtime
            .session()
            .prompt("root".to_string(), PromptOptions::default())
            .await
            .expect("root");
        let result = host.runtime.new_session(None).await.expect("new session");
        assert!(!result.cancelled);
        host.runtime
            .session()
            .prompt("target".to_string(), PromptOptions::default())
            .await
            .expect("target");
        let target_session_path = host.runtime.session().session_file().expect("target file");

        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        host.runtime
            .switch_session(
                &target_session_path,
                Some(kiana_session::agent_session_runtime::SwitchSessionOptions {
                    with_session: Some(send_callback(std::sync::Arc::clone(&calls))),
                    ..Default::default()
                }),
            )
            .await
            .expect("switch");
        let session = host.runtime.session();
        assert_eq!(
            session.session_file().as_deref(),
            Some(target_session_path.as_str())
        );
        let non_system: Vec<String> = session
            .messages()
            .iter()
            .filter(|message| message.role() != "system")
            .map(|message| format!("{}:{}", message.role(), message_text(message)))
            .collect();
        assert_eq!(
            non_system,
            vec![
                "user:target".to_string(),
                "assistant:target reply".to_string(),
                "user:Hello from the new session!".to_string(),
                "assistant:switch reply".to_string(),
            ]
        );
        assert_eq!(
            calls.lock().unwrap().clone(),
            vec!["with-session".to_string()]
        );
        host.dispose().await;
    })
    .await;
}

/// TS: "rebinds before withSession, targets the replacement session, and invalidates stale
/// kiana/ctx"（命令 `repro` 触发 `ctx.newSession({withSession})`）。
#[tokio::test]
async fn rebinds_before_with_session_and_invalidates_stale_kiana_and_ctx() {
    use kiana_ext_api::events::{
        ExtensionEvent, ExtensionEventKind, ExtensionHandler, SessionEvent,
    };
    use kiana_ext_api::types::{ExtensionFactory, WithSessionFn};
    use std::sync::atomic::{AtomicUsize, Ordering};

    run(async {
        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let instances = Arc::new(AtomicUsize::new(0));
        let stale_ctx_panics: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let stale_kiana_panics: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let old_session_file: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let replacement_session_file: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let factory: ExtensionFactory = {
            let events = Arc::clone(&events);
            let instances = Arc::clone(&instances);
            let stale_ctx_panics = Arc::clone(&stale_ctx_panics);
            let stale_kiana_panics = Arc::clone(&stale_kiana_panics);
            let old_session_file = Arc::clone(&old_session_file);
            let replacement_session_file = Arc::clone(&replacement_session_file);
            Arc::new(move |kiana| {
                let current = instances.fetch_add(1, Ordering::SeqCst) + 1;
                // `ExtensionApi` 不是 Send；与 TS 一样只保留第一个（旧）实例。
                KIANA_SLOT.with(|slot| {
                    if slot.borrow().is_none() {
                        *slot.borrow_mut() = Some(Arc::clone(&kiana));
                    }
                });

                let start_events = Arc::clone(&events);
                let _ = kiana.on(
                    ExtensionEventKind::SessionStart,
                    ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                        let events = Arc::clone(&start_events);
                        Box::pin(async move {
                            if matches!(event, ExtensionEvent::Session(SessionEvent::Start(_))) {
                                events.lock().unwrap().push(format!("start:{current}"));
                            }
                            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                        })
                    })),
                );
                let shutdown_events = Arc::clone(&events);
                let _ = kiana.on(
                    ExtensionEventKind::SessionShutdown,
                    ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                        let events = Arc::clone(&shutdown_events);
                        Box::pin(async move {
                            if matches!(event, ExtensionEvent::Session(SessionEvent::Shutdown(_))) {
                                events.lock().unwrap().push(format!("shutdown:{current}"));
                            }
                            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                        })
                    })),
                );

                let command_events = Arc::clone(&events);
                let command_stale_ctx = Arc::clone(&stale_ctx_panics);
                let command_stale_kiana = Arc::clone(&stale_kiana_panics);
                let command_old_file = Arc::clone(&old_session_file);
                let command_replacement_file = Arc::clone(&replacement_session_file);
                kiana.register_command(
                    "repro",
                    RegisterCommandOptions {
                        description: Some("repro".to_string()),
                        get_argument_completions: None,
                        handler: Arc::new(move |_args, ctx| {
                            let events = Arc::clone(&command_events);
                            let stale_ctx_panics = Arc::clone(&command_stale_ctx);
                            let stale_kiana_panics = Arc::clone(&command_stale_kiana);
                            let old_session_file = Arc::clone(&command_old_file);
                            let replacement_session_file = Arc::clone(&command_replacement_file);
                            Box::pin(async move {
                                let old_ctx: Arc<dyn ExtensionCommandContextApi> = ctx;
                                let parent_session = old_ctx
                                    .session_manager()
                                    .get_session_file()
                                    .map(str::to_string);
                                *old_session_file.lock().unwrap() = parent_session.clone();
                                let ctx_for_stale = Arc::clone(&old_ctx);
                                let with_session: WithSessionFn = Arc::new(
                                    move |replaced: Arc<
                                        dyn kiana_ext_api::types::ReplacedSessionContextApi,
                                    >| {
                                        let events = Arc::clone(&events);
                                        let stale_ctx_panics = Arc::clone(&stale_ctx_panics);
                                        let stale_kiana_panics = Arc::clone(&stale_kiana_panics);
                                        let replacement_session_file =
                                            Arc::clone(&replacement_session_file);
                                        let old_ctx = Arc::clone(&ctx_for_stale);
                                        Box::pin(async move {
                                            events.lock().unwrap().push(format!("with:{current}"));
                                            let stale_ctx =
                                                std::panic::catch_unwind(AssertUnwindSafe(|| {
                                                    let _ = old_ctx.session_manager();
                                                }));
                                            if let Err(payload) = stale_ctx {
                                                stale_ctx_panics
                                                    .lock()
                                                    .unwrap()
                                                    .push(stale_panic_message(payload));
                                            }
                                            let stale_kiana = KIANA_SLOT.with(|slot| {
                                                let kiana = slot
                                                    .borrow()
                                                    .as_ref()
                                                    .cloned()
                                                    .expect("kiana instance");
                                                std::panic::catch_unwind(AssertUnwindSafe(|| {
                                                    kiana.send_user_message(
                                                        UserContent::Text(
                                                            "stale message".to_string(),
                                                        ),
                                                        None,
                                                    );
                                                }))
                                            });
                                            if let Err(payload) = stale_kiana {
                                                stale_kiana_panics
                                                    .lock()
                                                    .unwrap()
                                                    .push(stale_panic_message(payload));
                                            }
                                            *replacement_session_file.lock().unwrap() = replaced
                                                .session_manager()
                                                .get_session_file()
                                                .map(str::to_string);
                                            replaced
                                                .send_user_message(
                                                    UserContent::Text(
                                                        "Hello from the new session!".to_string(),
                                                    ),
                                                    Some(SendUserMessageOptions {
                                                        deliver_as: None,
                                                        expand_prompt_templates: None,
                                                    }),
                                                )
                                                .await;
                                        })
                                    },
                                );
                                let result = old_ctx
                                    .new_session(Some(NewSessionOptions {
                                        parent_session,
                                        setup: None,
                                        with_session: Some(with_session),
                                    }))
                                    .await;
                                assert!(!result.cancelled);
                            })
                        }),
                    },
                );
                Box::pin(async {})
            })
        };

        let host = runtime_host(RuntimeHostOptions {
            extension_factory: Some(factory),
            ..Default::default()
        })
        .await;
        host.set_responses(vec![kiana_ai::providers::faux::FauxResponseStep::Message(
            kiana_ai::providers::faux::faux_assistant_message("hello reply", Default::default()),
        )]);
        let runtime = Rc::new(host.runtime);
        RUNTIME_SLOT.with(|slot| *slot.borrow_mut() = Some(Rc::clone(&runtime)));
        runtime.set_rebind_session(Some(Arc::new(|session| {
            Box::pin(async move {
                session.bind_extensions(command_bindings()).await;
            })
        })));
        runtime.session().bind_extensions(command_bindings()).await;
        assert_eq!(*events.lock().unwrap(), vec!["start:1".to_string()]);

        runtime
            .session()
            .prompt("/repro".to_string(), PromptOptions::default())
            .await
            .expect("repro command");

        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "start:1".to_string(),
                "shutdown:1".to_string(),
                "start:2".to_string(),
                "with:1".to_string()
            ]
        );
        let old_file = old_session_file.lock().unwrap().clone();
        let new_file = replacement_session_file.lock().unwrap().clone();
        assert!(new_file.is_some());
        assert_ne!(new_file, old_file);
        assert_eq!(
            *stale_ctx_panics.lock().unwrap(),
            vec![kiana_extensions::loader::DEFAULT_STALE_MESSAGE.to_string()]
        );
        assert_eq!(
            *stale_kiana_panics.lock().unwrap(),
            vec![kiana_extensions::loader::DEFAULT_STALE_MESSAGE.to_string()]
        );
        let session = runtime.session();
        let non_system: Vec<String> = session
            .messages()
            .iter()
            .filter(|message| message.role() != "system")
            .map(|message| format!("{}:{}", message.role(), message_text(message)))
            .collect();
        assert_eq!(
            non_system,
            vec![
                "user:Hello from the new session!".to_string(),
                "assistant:hello reply".to_string()
            ]
        );
        runtime.dispose().await;
        RUNTIME_SLOT.with(|slot| {
            slot.borrow_mut().take();
        });
    })
    .await;
}

#[allow(dead_code)]
fn _unused() {
    let _ = json!({});
}
