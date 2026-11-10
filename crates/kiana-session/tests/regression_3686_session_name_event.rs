//! `regressions/3686-session-name-event.test.ts` 的 Rust 移植（regression #3686）。
#![allow(clippy::arc_with_non_send_sync)]

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_ext_api::events::{ExtensionEvent, ExtensionEventKind, SessionEvent};

/// TS: "emits session_info_changed when AgentSession.setSessionName is called"
#[tokio::test]
async fn emits_session_info_changed_when_set_session_name_is_called() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.session.set_session_name("hello world");
        assert_eq!(
            harness
                .session
                .session_manager()
                .get_session_name()
                .as_deref(),
            Some("hello world")
        );
        let names: Vec<Option<String>> = harness
            .events_of_type("session_info_changed")
            .into_iter()
            .filter_map(|event| match event {
                kiana_session::AgentSessionEvent::SessionInfoChanged { name } => Some(name),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec![Some("hello world".to_string())]);
        harness.cleanup();
    })
    .await;
}

/// TS: "emits session_info_changed when an extension calls kiana.setSessionName"
#[tokio::test]
async fn emits_session_info_changed_when_an_extension_calls_set_session_name() {
    run(async {
        let api_slot: Arc<Mutex<Option<Arc<dyn kiana_ext_api::types::ExtensionApi>>>> =
            Arc::new(Mutex::new(None));
        let slot = Arc::clone(&api_slot);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                *slot.lock().unwrap() = Some(kiana);
            })],
            ..Default::default()
        })
        .await;
        api_slot
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .set_session_name("from extension");
        assert_eq!(
            harness
                .session
                .session_manager()
                .get_session_name()
                .as_deref(),
            Some("from extension")
        );
        let names: Vec<Option<String>> = harness
            .events_of_type("session_info_changed")
            .into_iter()
            .filter_map(|event| match event {
                kiana_session::AgentSessionEvent::SessionInfoChanged { name } => Some(name),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec![Some("from extension".to_string())]);
        harness.cleanup();
    })
    .await;
}

/// TS: "emits session_info_changed to extensions"
#[tokio::test]
async fn emits_session_info_changed_to_extensions() {
    run(async {
        let api_slot: Arc<Mutex<Option<Arc<dyn kiana_ext_api::types::ExtensionApi>>>> =
            Arc::new(Mutex::new(None));
        let events: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let slot = Arc::clone(&api_slot);
        let sink = Arc::clone(&events);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                *slot.lock().unwrap() = Some(Arc::clone(&kiana));
                let sink = Arc::clone(&sink);
                let _ = kiana.on(
                    ExtensionEventKind::SessionInfoChanged,
                    standard(move |event, _ctx| {
                        let sink = Arc::clone(&sink);
                        Box::pin(async move {
                            if let ExtensionEvent::Session(SessionEvent::InfoChanged(info)) = event
                            {
                                sink.lock().unwrap().push(info.name);
                            }
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        api_slot
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .set_session_name("first");
        harness.session.set_session_name("second");
        for _ in 0..200 {
            if events.lock().unwrap().len() >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert_eq!(
            *events.lock().unwrap(),
            vec![Some("first".to_string()), Some("second".to_string())]
        );
        harness.cleanup();
    })
    .await;
}
