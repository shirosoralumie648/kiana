//! `test/suite/agent-session-runtime.test.ts`：AgentSessionRuntime 的特征测试。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
mod session_support;

use std::sync::{Arc, Mutex};

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::Message;
use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventKind, ExtensionEventResult, ForkPosition, MessageEndEvent,
    MessageEndEventResult, SessionBeforeForkEvent, SessionBeforeSwitchEvent, SessionShutdownEvent,
    SessionStartEvent,
};
use kiana_ext_api::events::{ExtensionHandler, SessionEvent};
use kiana_session::agent_session::PromptOptions;
use kiana_session::agent_session_runtime::{
    AgentSessionRuntimeError, ForkSessionOptions, NewSessionOptionsInput, SwitchSessionOptions,
};
use kiana_session_core::session_manager::{SessionEntry, SessionManager};
use serde_json::json;

use session_support::{
    RuntimeHost, RuntimeHostOptions, install_engine, message_text, run, runtime_host,
};

/// 事件记录器（`session_before_switch` / `session_before_fork` / `session_shutdown` / `session_start`）。
#[derive(Clone, Default)]
struct RecordedEvents(Arc<Mutex<Vec<ExtensionEvent>>>);

impl RecordedEvents {
    fn push(&self, event: ExtensionEvent) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
    }

    fn drain(&self) -> Vec<ExtensionEvent> {
        std::mem::take(
            &mut *self
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
}

fn recording_factory(
    events: RecordedEvents,
    cancel: Arc<Mutex<Option<String>>>,
) -> kiana_ext_api::types::ExtensionFactory {
    Arc::new(move |kiana| {
        let start_events = events.clone();
        let _ = kiana.on(
            ExtensionEventKind::SessionStart,
            ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                let events = start_events.clone();
                Box::pin(async move {
                    events.push(event);
                    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                })
            })),
        );
        let switch_events = events.clone();
        let switch_cancel = Arc::clone(&cancel);
        let _ = kiana.on(
            ExtensionEventKind::SessionBeforeSwitch,
            ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                let events = switch_events.clone();
                let cancel = Arc::clone(&switch_cancel);
                Box::pin(async move {
                    let reason = match &event {
                        ExtensionEvent::Session(SessionEvent::BeforeSwitch(event)) => {
                            Some(event.reason)
                        }
                        _ => None,
                    };
                    events.push(event);
                    let should_cancel = match reason {
                        Some(kiana_ext_api::events::SessionBeforeSwitchReason::New) => {
                            cancel.lock().unwrap().as_deref() == Some("new")
                        }
                        Some(kiana_ext_api::events::SessionBeforeSwitchReason::Resume) => {
                            cancel.lock().unwrap().as_deref() == Some("resume")
                        }
                        _ => false,
                    };
                    if should_cancel {
                        Ok(Some(ExtensionEventResult::SessionBeforeSwitch(
                            kiana_ext_api::events::SessionBeforeSwitchResult { cancel: Some(true) },
                        )))
                    } else {
                        Ok(None)
                    }
                })
            })),
        );
        let shutdown_events = events.clone();
        let _ = kiana.on(
            ExtensionEventKind::SessionShutdown,
            ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                let events = shutdown_events.clone();
                Box::pin(async move {
                    events.push(event);
                    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                })
            })),
        );
        let fork_events = events.clone();
        let fork_cancel = Arc::clone(&cancel);
        let _ = kiana.on(
            ExtensionEventKind::SessionBeforeFork,
            ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                let events = fork_events.clone();
                let cancel = Arc::clone(&fork_cancel);
                Box::pin(async move {
                    events.push(event);
                    if cancel.lock().unwrap().as_deref() == Some("fork") {
                        *cancel.lock().unwrap() = None;
                        return Ok(Some(ExtensionEventResult::SessionBeforeFork(
                            kiana_ext_api::events::SessionBeforeForkResult {
                                cancel: Some(true),
                                skip_conversation_restore: None,
                            },
                        )));
                    }
                    Ok(None)
                })
            })),
        );
        Box::pin(async {})
    })
}

fn event_summary(event: &ExtensionEvent) -> serde_json::Value {
    match event {
        ExtensionEvent::Session(SessionEvent::Start(SessionStartEvent {
            reason,
            previous_session_file,
            ..
        })) => json!({
            "type": "session_start",
            "reason": reason,
            "previousSessionFile": previous_session_file,
        }),
        ExtensionEvent::Session(SessionEvent::BeforeSwitch(SessionBeforeSwitchEvent {
            reason,
            target_session_file,
            ..
        })) => json!({
            "type": "session_before_switch",
            "reason": reason,
            "targetSessionFile": target_session_file,
        }),
        ExtensionEvent::Session(SessionEvent::Shutdown(SessionShutdownEvent {
            reason,
            target_session_file,
            ..
        })) => json!({
            "type": "session_shutdown",
            "reason": reason,
            "targetSessionFile": target_session_file,
        }),
        ExtensionEvent::Session(SessionEvent::BeforeFork(SessionBeforeForkEvent {
            entry_id,
            position,
            ..
        })) => json!({
            "type": "session_before_fork",
            "entryId": entry_id,
            "position": position,
        }),
        _ => json!({ "type": "other" }),
    }
}

fn summaries(events: &[ExtensionEvent]) -> Vec<serde_json::Value> {
    events.iter().map(event_summary).collect()
}

#[tokio::test]
async fn persists_message_end_assistant_replacements_to_the_session_manager() {
    run(async {
        let factory: kiana_ext_api::types::ExtensionFactory = Arc::new(|kiana| {
            let _ = kiana.on(
                ExtensionEventKind::MessageEnd,
                ExtensionHandler::Standard(Arc::new(|event, _ctx| {
                    Box::pin(async move {
                        let ExtensionEvent::MessageEnd(MessageEndEvent { message, .. }) = event
                        else {
                            return Ok(None);
                        };
                        let AgentMessage::Standard(Message::Assistant(mut assistant)) = message
                        else {
                            return Ok(None);
                        };
                        assistant.usage.cost.total = 0.123;
                        Ok(Some(ExtensionEventResult::MessageEnd(
                            MessageEndEventResult {
                                message: Some(AgentMessage::Standard(Message::Assistant(
                                    assistant,
                                ))),
                            },
                        )))
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
        let session = host.runtime.session();
        session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let assistant = session
            .messages()
            .into_iter()
            .find_map(|message| match message {
                AgentMessage::Standard(Message::Assistant(assistant)) => Some(assistant),
                _ => None,
            })
            .expect("assistant message");
        assert_eq!(assistant.usage.cost.total, 0.123);
        let persisted = session
            .session_manager()
            .get_entries()
            .into_iter()
            .find_map(|entry| match entry {
                kiana_session_core::session_manager::SessionEntry::Message(entry) => {
                    match entry.message {
                        AgentMessage::Standard(Message::Assistant(assistant)) => Some(assistant),
                        _ => None,
                    }
                }
                _ => None,
            })
            .expect("persisted assistant");
        assert_eq!(persisted.usage.cost.total, 0.123);
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn preserves_an_existing_session_when_importing_a_file_with_the_same_name() {
    run(async {
        let host = runtime_host(RuntimeHostOptions::default()).await;
        let session_dir = host.runtime.session().session_manager().get_session_dir().to_string();
        let import_dir = host.temp_dir.path().join("import");
        std::fs::create_dir_all(&session_dir).expect("session dir");
        std::fs::create_dir_all(&import_dir).expect("import dir");
        let stored_path = std::path::Path::new(&session_dir).join("collision.jsonl");
        let import_path = import_dir.join("collision.jsonl");
        let stored = format!(
            "{}\n",
            json!({"type":"session","version":3,"id":"stored","timestamp":"2026-01-01T00:00:00.000Z","cwd":host.cwd})
        );
        let imported = format!(
            "{}\n",
            json!({"type":"session","version":3,"id":"imported","timestamp":"2026-01-01T00:00:00.000Z","cwd":host.cwd})
        );
        std::fs::write(&stored_path, &stored).expect("stored session");
        std::fs::write(&import_path, &imported).expect("import session");

        host.runtime
            .import_from_jsonl(import_path.to_str().unwrap(), None)
            .await
            .expect("import");

        assert_eq!(std::fs::read_to_string(&stored_path).expect("read"), stored);
        let session_file = host.runtime.session().session_file().expect("session file");
        assert_ne!(session_file, stored_path.to_string_lossy());
        assert!(
            std::fs::read_to_string(&session_file)
                .expect("read imported")
                .contains("\"id\":\"imported\"")
        );
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn import_maps_copyfile_failures_to_node_error_text() {
    run(async {
        let temp = tempfile::tempdir().expect("temp");
        let session_dir = temp.path().join("sessions");
        std::fs::create_dir_all(&session_dir).expect("session dir");
        let cwd = temp.path().join("project");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let session_manager = SessionManager::create(
            cwd.to_str().unwrap(),
            Some(session_dir.to_str().unwrap()),
            None,
        )
        .expect("session manager");
        let host = runtime_host(RuntimeHostOptions {
            cwd: Some(cwd.to_string_lossy().into_owned()),
            session_manager: Some(session_manager),
            ..Default::default()
        })
        .await;
        let normalized_session_dir = host
            .runtime
            .session()
            .session_manager()
            .get_session_dir()
            .to_string();
        let source = temp.path().join("a-directory");
        std::fs::create_dir_all(&source).expect("source dir");
        let destination = std::path::Path::new(&normalized_session_dir).join("a-directory");

        let error = host
            .runtime
            .import_from_jsonl(source.to_str().unwrap(), None)
            .await
            .expect_err("copying a directory must fail");
        assert_eq!(
            error.to_string(),
            format!(
                "EISDIR: illegal operation on a directory, copyfile '{}' -> '{}'",
                source.display(),
                destination.display()
            )
        );
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn import_preserves_the_source_file_permissions() {
    use std::os::unix::fs::PermissionsExt;

    run(async {
        let temp = tempfile::tempdir().expect("temp");
        let session_dir = temp.path().join("sessions");
        std::fs::create_dir_all(&session_dir).expect("session dir");
        let cwd = temp.path().join("project");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let cwd_string = cwd.to_string_lossy().into_owned();
        let session_manager =
            SessionManager::create(&cwd_string, Some(session_dir.to_str().unwrap()), None)
                .expect("session manager");
        let host = runtime_host(RuntimeHostOptions {
            cwd: Some(cwd_string.clone()),
            session_manager: Some(session_manager),
            ..Default::default()
        })
        .await;

        let import_dir = temp.path().join("import-src");
        std::fs::create_dir_all(&import_dir).expect("import dir");
        let mut source_manager =
            SessionManager::create(&cwd_string, Some(import_dir.to_str().unwrap()), None)
                .expect("source manager");
        source_manager
            .append_message(AgentMessage::Standard(Message::User(
                kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("hi".to_string()),
                    timestamp: 0,
                },
            )))
            .expect("append");
        let source_file = source_manager.get_session_file().expect("source file");
        std::fs::set_permissions(source_file, std::fs::Permissions::from_mode(0o700))
            .expect("chmod source");

        host.runtime
            .import_from_jsonl(source_file, None)
            .await
            .expect("import");
        let destination = host.runtime.session().session_file().expect("session file");
        let mode = std::fs::metadata(&destination)
            .expect("destination metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "copyFileSync preserves the source mode");
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn import_maps_mkdir_failures_to_node_error_text() {
    use std::os::unix::fs::PermissionsExt;

    run(async {
        let temp = tempfile::tempdir().expect("temp");
        let readonly_parent = temp.path().join("readonly");
        std::fs::create_dir_all(&readonly_parent).expect("readonly parent");
        let session_dir = readonly_parent.join("sessions");
        std::fs::create_dir_all(&session_dir).expect("session dir");
        let cwd = temp.path().join("project");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let session_manager = SessionManager::create(
            cwd.to_str().unwrap(),
            Some(session_dir.to_str().unwrap()),
            None,
        )
        .expect("session manager");
        let host = runtime_host(RuntimeHostOptions {
            cwd: Some(cwd.to_string_lossy().into_owned()),
            session_manager: Some(session_manager),
            ..Default::default()
        })
        .await;
        let normalized_session_dir = host
            .runtime
            .session()
            .session_manager()
            .get_session_dir()
            .to_string();
        // 会话目录在 apply/newSession 前不存在时才走 `mkdirSync(dir, { recursive: true })`。
        std::fs::remove_dir_all(&normalized_session_dir).expect("remove session dir");
        std::fs::set_permissions(&readonly_parent, std::fs::Permissions::from_mode(0o500))
            .expect("chmod readonly");
        // 以 root 运行时 chmod 不生效（测试机通常不是 root）；此时跳过错误断言。
        let privileged = std::fs::create_dir(readonly_parent.join("probe")).is_ok();
        if privileged {
            let _ = std::fs::remove_dir(readonly_parent.join("probe"));
            std::fs::set_permissions(&readonly_parent, std::fs::Permissions::from_mode(0o700))
                .expect("restore");
            host.dispose().await;
            return;
        }
        let import_dir = temp.path().join("import");
        std::fs::create_dir_all(&import_dir).expect("import dir");
        let import_path = import_dir.join("imported.jsonl");
        std::fs::write(
            &import_path,
            format!(
                "{}\n",
                json!({"type":"session","version":3,"id":"imported","timestamp":"2026-01-01T00:00:00.000Z","cwd":host.cwd})
            ),
        )
        .expect("import session");

        let error = host
            .runtime
            .import_from_jsonl(import_path.to_str().unwrap(), None)
            .await
            .expect_err("mkdir must fail");
        std::fs::set_permissions(&readonly_parent, std::fs::Permissions::from_mode(0o700))
            .expect("restore");
        assert_eq!(
            error.to_string(),
            format!(
                "EACCES: permission denied, mkdir '{}'",
                normalized_session_dir
            )
        );
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn emits_session_before_switch_and_session_start_for_new_and_resume_flows() {
    run(async {
        let events = RecordedEvents::default();
        let cancel = Arc::new(Mutex::new(None));
        let host = runtime_host(RuntimeHostOptions {
            extension_factory: Some(recording_factory(events.clone(), cancel)),
            ..Default::default()
        })
        .await;
        host.bind_extensions().await;
        assert_eq!(
            summaries(&events.drain()),
            vec![json!({"type":"session_start","reason":"startup","previousSessionFile":null})]
        );

        host.runtime
            .session()
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let original_session_file = host.runtime.session().session_file();
        let original_session = host.runtime.session();

        let result = host
            .runtime
            .new_session(None)
            .await
            .expect("new session");
        assert!(!result.cancelled);
        host.bind_extensions().await;
        assert!(!std::ptr::eq(
            &host.runtime.session() as *const _,
            &original_session as *const _
        ));
        assert!(host.runtime.session().messages().is_empty());
        let second_session_file = host.runtime.session().session_file();
        assert_eq!(
            summaries(&events.drain()),
            vec![
                json!({"type":"session_before_switch","reason":"new","targetSessionFile":null}),
                json!({"type":"session_shutdown","reason":"new","targetSessionFile":second_session_file}),
                json!({"type":"session_start","reason":"new","previousSessionFile":original_session_file}),
            ]
        );

        let result = host
            .runtime
            .switch_session(
                original_session_file.as_deref().unwrap(),
                None,
            )
            .await
            .expect("switch");
        assert!(!result.cancelled);
        host.bind_extensions().await;
        assert_eq!(
            summaries(&events.drain()),
            vec![
                json!({"type":"session_before_switch","reason":"resume","targetSessionFile":original_session_file}),
                json!({"type":"session_shutdown","reason":"resume","targetSessionFile":original_session_file}),
                json!({"type":"session_start","reason":"resume","previousSessionFile":second_session_file}),
            ]
        );
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn honors_session_before_switch_cancellation_for_new_and_resume() {
    run(async {
        let events = RecordedEvents::default();
        let cancel = Arc::new(Mutex::new(None));
        let host = runtime_host(RuntimeHostOptions {
            extension_factory: Some(recording_factory(events.clone(), Arc::clone(&cancel))),
            ..Default::default()
        })
        .await;
        host.bind_extensions().await;
        events.drain();
        host.runtime
            .session()
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let original_session_file = host.runtime.session().session_file();

        *cancel.lock().unwrap() = Some("new".to_string());
        let result = host.runtime.new_session(None).await.expect("new");
        assert!(result.cancelled);
        assert_eq!(host.runtime.session().session_file(), original_session_file);

        events.drain();
        let other_dir = tempfile::tempdir().expect("other dir");
        let other = SessionManager::create(
            other_dir.path().to_str().unwrap(),
            Some(other_dir.path().join("sessions").to_str().unwrap()),
            None,
        )
        .expect("other session");
        let other_file = other.get_session_file().map(str::to_string);
        // 写入一个用户消息以持久化。
        let mut other = other;
        other
            .append_message(AgentMessage::Standard(Message::User(
                kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("other".to_string()),
                    timestamp: 0,
                },
            )))
            .expect("append");
        drop(other);
        *cancel.lock().unwrap() = Some("resume".to_string());
        let result = host
            .runtime
            .switch_session(other_file.as_deref().unwrap(), None)
            .await
            .expect("switch");
        assert!(result.cancelled);
        assert_eq!(host.runtime.session().session_file(), original_session_file);
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn emits_session_before_fork_and_session_start_and_honors_cancellation() {
    run(async {
        let events = RecordedEvents::default();
        let cancel = Arc::new(Mutex::new(None));
        let host = runtime_host(RuntimeHostOptions {
            extension_factory: Some(recording_factory(events.clone(), Arc::clone(&cancel))),
            ..Default::default()
        })
        .await;
        host.bind_extensions().await;
        events.drain();
        host.runtime
            .session()
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let user_message = host.runtime.session().get_user_messages_for_forking()[0].clone();
        let previous_session_file = host.runtime.session().session_file();

        let result = host
            .runtime
            .fork(&user_message.0, None)
            .await
            .expect("fork");
        assert!(!result.cancelled);
        assert_eq!(result.selected_text.as_deref(), Some("hello"));
        host.bind_extensions().await;
        let file = host.runtime.session().session_file();
        assert_eq!(
            summaries(&events.drain()),
            vec![
                json!({"type":"session_before_fork","entryId":user_message.0,"position":"before"}),
                json!({"type":"session_shutdown","reason":"fork","targetSessionFile":file}),
                json!({"type":"session_start","reason":"fork","previousSessionFile":previous_session_file}),
            ]
        );
        let session_id = host.runtime.session().session_id();
        assert!(file.as_deref().unwrap().contains(&format!("_{session_id}.jsonl")));

        events.drain();
        *cancel.lock().unwrap() = Some("fork".to_string());
        let result = host
            .runtime
            .fork(&user_message.0, None)
            .await
            .expect("fork");
        assert!(result.cancelled);
        assert_eq!(
            summaries(&events.drain()),
            vec![json!({"type":"session_before_fork","entryId":user_message.0,"position":"before"})]
        );

        events.drain();
        *cancel.lock().unwrap() = Some("fork".to_string());
        let result = host
            .runtime
            .fork(
                "missing-entry",
                Some(ForkSessionOptions {
                    position: Some(ForkPosition::At),
                    ..Default::default()
                }),
            )
            .await
            .expect("fork");
        assert!(result.cancelled);
        assert_eq!(
            summaries(&events.drain()),
            vec![json!({"type":"session_before_fork","entryId":"missing-entry","position":"at"})]
        );
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn reports_why_an_unflushed_session_cannot_be_forked() {
    run(async {
        let host = runtime_host(RuntimeHostOptions::default()).await;
        let session_file = host.runtime.session().session_file().expect("session file");
        let leaf_id = host
            .runtime
            .session()
            .session_manager()
            .get_leaf_id()
            .map(str::to_string)
            .expect("leaf");
        assert!(!std::path::Path::new(&session_file).exists());
        let error = host
            .runtime
            .fork(
                &leaf_id,
                Some(ForkSessionOptions {
                    position: Some(ForkPosition::At),
                    ..Default::default()
                }),
            )
            .await
            .expect_err("must fail");
        assert_eq!(
            error.to_string(),
            "This session has not been saved yet. Send a message before cloning or forking it."
        );
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn duplicates_the_current_active_branch_when_forking_at_the_current_position() {
    run(async {
        let host = runtime_host(RuntimeHostOptions::default()).await;
        let session = host.runtime.session();
        session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .expect("p1");
        session
            .prompt("again".to_string(), PromptOptions::default())
            .await
            .expect("p2");
        let before: Vec<(String, String)> = session
            .messages()
            .iter()
            .map(|message| (message.role().to_string(), message_text(message)))
            .collect();
        let previous_session_file = session.session_file();
        let leaf_id = session
            .session_manager()
            .get_leaf_id()
            .map(str::to_string)
            .expect("leaf");
        let result = host
            .runtime
            .fork(
                &leaf_id,
                Some(ForkSessionOptions {
                    position: Some(ForkPosition::At),
                    ..Default::default()
                }),
            )
            .await
            .expect("fork");
        assert!(!result.cancelled);
        assert!(result.selected_text.is_none());
        let new_session = host.runtime.session();
        assert_ne!(new_session.session_file(), previous_session_file);
        let after: Vec<(String, String)> = new_session
            .messages()
            .iter()
            .map(|message| (message.role().to_string(), message_text(message)))
            .collect();
        assert_eq!(after, before);
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn duplicates_the_current_active_branch_in_memory_when_forking_at_the_current_position() {
    run(async {
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().join("project");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let cwd = cwd.to_string_lossy().into_owned();
        let session_manager = SessionManager::in_memory(&cwd, None, None).expect("session");
        let host = runtime_host(RuntimeHostOptions {
            cwd: Some(cwd.clone()),
            session_manager: Some(session_manager),
            ..Default::default()
        })
        .await;
        let session = host.runtime.session();
        session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .expect("p1");
        session
            .prompt("again".to_string(), PromptOptions::default())
            .await
            .expect("p2");
        let before: Vec<(String, String)> = session
            .messages()
            .iter()
            .map(|message| (message.role().to_string(), message_text(message)))
            .collect();
        let leaf_id = session
            .session_manager()
            .get_leaf_id()
            .map(str::to_string)
            .expect("leaf");
        assert_eq!(session.session_file(), None);
        let result = host
            .runtime
            .fork(
                &leaf_id,
                Some(ForkSessionOptions {
                    position: Some(ForkPosition::At),
                    ..Default::default()
                }),
            )
            .await
            .expect("fork");
        assert!(!result.cancelled);
        assert!(result.selected_text.is_none());
        let new_session = host.runtime.session();
        assert_eq!(new_session.session_file(), None);
        let after: Vec<(String, String)> = new_session
            .messages()
            .iter()
            .map(|message| (message.role().to_string(), message_text(message)))
            .collect();
        assert_eq!(after, before);
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn throws_when_forking_with_an_invalid_entry_id() {
    run(async {
        let host = runtime_host(RuntimeHostOptions::default()).await;
        let error = host
            .runtime
            .fork("missing-entry", None)
            .await
            .expect_err("must fail");
        assert!(matches!(
            error,
            AgentSessionRuntimeError::InvalidEntryIdForForking
        ));
        assert_eq!(error.to_string(), "Invalid entry ID for forking");
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn updates_the_runtime_session_cwd_on_cross_cwd_session_replacement() {
    run(async {
        let first_dir = tempfile::tempdir().expect("first");
        let second_dir = tempfile::tempdir().expect("second");
        let first = runtime_host(RuntimeHostOptions {
            cwd: Some(first_dir.path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await;
        let second = runtime_host(RuntimeHostOptions {
            cwd: Some(second_dir.path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await;
        second
            .runtime
            .session()
            .prompt("other".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let other_session_file = second.runtime.session().session_file().expect("file");

        first
            .runtime
            .switch_session(&other_session_file, None)
            .await
            .expect("switch");
        assert_eq!(
            std::fs::canonicalize(first.runtime.session().session_manager().get_cwd())
                .expect("canonical"),
            std::fs::canonicalize(second_dir.path()).expect("canonical")
        );
        assert_eq!(
            std::fs::canonicalize(first.runtime.cwd()).expect("canonical cwd"),
            std::fs::canonicalize(second_dir.path()).expect("canonical")
        );
        first.dispose().await;
        second.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn restores_model_and_thinking_state_from_the_destination_session() {
    run(async {
        let _engine = install_engine();
        let dir = tempfile::tempdir().expect("dir");
        let other_dir = dir.path().join("other");
        std::fs::create_dir_all(&other_dir).expect("other");
        let source_cwd = dir.path().join("source");
        std::fs::create_dir_all(&source_cwd).expect("source");
        let source = runtime_host(RuntimeHostOptions {
            cwd: Some(source_cwd.to_string_lossy().into_owned()),
            bootstrap_model: false,
            ..Default::default()
        })
        .await;
        let other = runtime_host(RuntimeHostOptions {
            cwd: Some(other_dir.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await;
        let other_session = other.runtime.session();
        other_session
            .set_model(
                other
                    .faux
                    .get_model(Some("faux-2"))
                    .expect("faux-2")
                    .clone(),
                Default::default(),
            )
            .await
            .expect("set model");
        other_session
            .set_thinking_level(kiana_ai::types::ModelThinkingLevel::Off, Default::default());
        other_session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let target = other_session.session_file().expect("file");

        source
            .runtime
            .switch_session(&target, None)
            .await
            .expect("switch");
        let session = source.runtime.session();
        assert_eq!(
            session.model().map(|model| model.id),
            Some("faux-2".to_string())
        );
        assert_eq!(
            session.thinking_level(),
            kiana_ai::types::ModelThinkingLevel::Off
        );
        source.dispose().await;
        other.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn new_session_treats_an_empty_parent_session_as_absent() {
    run(async {
        let host = runtime_host(RuntimeHostOptions::default()).await;
        host.runtime
            .new_session(Some(NewSessionOptionsInput {
                parent_session: Some(String::new()),
                setup: None,
                with_session: None,
            }))
            .await
            .expect("new session");
        let header = host
            .runtime
            .session()
            .session_manager()
            .get_header()
            .cloned()
            .expect("header");
        assert_eq!(header.parent_session, None);

        host.runtime
            .new_session(Some(NewSessionOptionsInput {
                parent_session: Some("/tmp/parent-session.jsonl".to_string()),
                setup: None,
                with_session: None,
            }))
            .await
            .expect("new session");
        let header = host
            .runtime
            .session()
            .session_manager()
            .get_header()
            .cloned()
            .expect("header");
        assert_eq!(
            header.parent_session,
            Some("/tmp/parent-session.jsonl".to_string())
        );
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn awaits_async_new_session_setup_before_refreshing_context() {
    run(async {
        let host = runtime_host(RuntimeHostOptions::default()).await;
        let setup: kiana_ext_api::types::SetupFn = Arc::new(|manager| {
            Box::pin(async move {
                // 真正的异步等待：只有在 setup 被 await 时才会继续。
                tokio::task::yield_now().await;
                manager
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .append_message(AgentMessage::Standard(Message::User(
                        kiana_ai::types::UserMessage {
                            role: kiana_ai::types::UserRole::User,
                            content: kiana_ai::types::UserContent::Text("from setup".to_string()),
                            timestamp: 0,
                        },
                    )))
                    .expect("append from setup");
            })
        });
        host.runtime
            .new_session(Some(NewSessionOptionsInput {
                parent_session: None,
                setup: Some(setup),
                with_session: None,
            }))
            .await
            .expect("new session");
        let session = host.runtime.session();
        assert!(
            session
                .messages()
                .iter()
                .any(|message| message_text(message) == "from setup"),
            "setup must be awaited and refreshContext must run after it"
        );
        host.dispose().await;
    })
    .await;
}

/// 阻塞到 abort 的工具（TS 的 `block` 工具）。
struct BlockingExecute {
    started: Arc<tokio::sync::Notify>,
}

impl kiana_ext_api::tool_types::ToolDefinitionExecute for BlockingExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: serde_json::Value,
        signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
        _ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<
            kiana_agent_core::types::AgentToolResult<serde_json::Value>,
            kiana_agent_core::types::AgentToolError,
        >,
    > {
        let started = Arc::clone(&self.started);
        Box::pin(async move {
            started.notify_one();
            if let Some(signal) = signal {
                signal.cancelled().await;
            }
            Ok(kiana_agent_core::types::AgentToolResult {
                content: vec![kiana_ai::types::UserContentBlock::Text(
                    kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text: "tool aborted".to_string(),
                        text_signature: None,
                    },
                )],
                details: serde_json::Value::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

#[tokio::test]
async fn settles_the_active_response_before_session_replacement() {
    run(async {
        let started = Arc::new(tokio::sync::Notify::new());
        let started_for_tool = Arc::clone(&started);
        let factory: kiana_ext_api::types::ExtensionFactory = Arc::new(move |kiana| {
            let mut tool = common::simple_tool_definition("block", "Blocks until aborted", None);
            tool.execute = Arc::new(BlockingExecute {
                started: Arc::clone(&started_for_tool),
            });
            kiana.register_tool(tool);
            Box::pin(async {})
        });
        let host = runtime_host(RuntimeHostOptions {
            extension_factory: Some(factory),
            ..Default::default()
        })
        .await;
        host.bind_extensions().await;
        host.set_responses(vec![kiana_ai::providers::faux::FauxResponseStep::Message(
            kiana_ai::providers::faux::faux_assistant_message("hello reply", Default::default()),
        )]);
        host.runtime
            .session()
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .expect("first prompt");
        let first_session_file = host.runtime.session().session_file().expect("file");
        assert!(!host.runtime.new_session(None).await.expect("new").cancelled);
        host.bind_extensions().await;

        host.set_responses(vec![common::faux_tool_response(
            kiana_ai::providers::faux::faux_tool_call("block", kiana_ai::types::JsonObject::new()),
            kiana_ai::types::StopReason::ToolUse,
        )]);
        let outgoing = host.runtime.session();
        let outgoing_prompt = tokio::task::spawn_local({
            let outgoing = outgoing.clone();
            async move {
                outgoing
                    .prompt("start blocking tool".to_string(), PromptOptions::default())
                    .await
            }
        });
        started.notified().await;
        let switch = host
            .runtime
            .switch_session(&first_session_file, None)
            .await
            .expect("switch");
        let _ = outgoing_prompt.await.expect("join");
        assert!(!switch.cancelled);
        assert_eq!(
            host.runtime.session().session_file().as_deref(),
            Some(first_session_file.as_str())
        );
        // 替换前旧会话已结算：中断的工具调用留下持久化的 toolResult，而不是悬空。
        let outgoing_file = outgoing.session_file().expect("outgoing file");
        let roles: Vec<String> = SessionManager::open(&outgoing_file, None, None)
            .expect("open outgoing")
            .get_entries()
            .iter()
            .filter_map(|entry| match entry {
                SessionEntry::Message(message) => Some(message.message.role().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            roles,
            vec!["system", "user", "assistant", "toolResult", "assistant"]
        );
        host.dispose().await;
    })
    .await;
}

// 供未使用的 helper（避免 dead_code 噪音在编译目标里）。
#[allow(dead_code)]
fn _unused(host: &RuntimeHost, options: NewSessionOptionsInput, switch: SwitchSessionOptions) {
    let _ = host;
    let _ = options;
    let _ = switch;
}
