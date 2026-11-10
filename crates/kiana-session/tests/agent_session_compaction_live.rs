//! `test/agent-session-compaction.test.ts` 的 Rust 移植。
//!
//! TS 用例在 `describe.skipIf(!API_KEY)` 下运行真实 LLM 调用；Rust 版本全部 `#[ignore]`，
//! 需要 `ANTHROPIC_OAUTH_TOKEN` 或 `ANTHROPIC_API_KEY`。
#![allow(clippy::arc_with_non_send_sync)]

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::agent::{Agent, AgentInitialState, AgentOptions};
use kiana_ai::types::{Message, Model};
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelsPathOption};
use kiana_config::settings::SettingsManager;
use kiana_session::agent_session::{
    AgentSession, AgentSessionConfig, AgentSessionEvent, PromptOptions,
};
use kiana_session_core::session_manager::{SessionEntry, SessionManager};
use serde_json::json;

fn api_key() -> String {
    std::env::var("ANTHROPIC_OAUTH_TOKEN")
        .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
        .expect("ANTHROPIC_OAUTH_TOKEN or ANTHROPIC_API_KEY")
}

struct LiveHarness {
    session: AgentSession,
    events: Arc<Mutex<Vec<AgentSessionEvent>>>,
    temp_dir: tempfile::TempDir,
}

async fn create_session(in_memory: bool) -> LiveHarness {
    let key = api_key();
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let cwd = temp_dir.path().to_string_lossy().into_owned();
    let model: Model =
        kiana_ai::compat::get_model("anthropic", "claude-sonnet-4-5").expect("anthropic model");
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are a helpful assistant. Be concise.".to_string()),
            model: Some(model),
            thinking_level: None,
            tools: Some(Vec::new()),
            messages: None,
        }),
        stream_fn: Some(kiana_session::agent_session::stream_simple_stream_fn()),
        get_api_key: Some(Arc::new(move |_provider: String| {
            let key = key.clone();
            Box::pin(async move { Some(key) })
        })),
        ..Default::default()
    })
    .expect("agent");
    let session_manager = if in_memory {
        SessionManager::in_memory(&cwd, None, None).expect("session")
    } else {
        SessionManager::create(&cwd, None, None).expect("session")
    };
    let settings_manager = SettingsManager::in_memory(Default::default());
    settings_manager.apply_overrides(settings_from_json(json!({
        "compaction": { "keepRecentTokens": 1 }
    })));
    let auth_storage = AuthStorage::in_memory(auth_data_from_value(json!({
        "anthropic": { "type": "api_key", "key": api_key() }
    })));
    let runtime = kiana_config::models::runtime::ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(auth_storage)),
        models_path: ModelsPathOption::Disabled,
        ..Default::default()
    })
    .await
    .expect("model runtime");
    let extensions = create_test_extensions_result(Vec::new(), &cwd).await;
    let resource_loader: Arc<dyn kiana_resources::loader::ResourceLoader> =
        Arc::new(TestResourceLoader::new(extensions));
    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager,
        cwd: cwd.clone(),
        scoped_models: Vec::new(),
        resource_loader,
        custom_tools: Vec::new(),
        model_runtime: runtime,
        cache_warmer: None,
        initial_active_tool_names: None,
        uses_default_tools: Some(true),
        default_tool_modifiers: Vec::new(),
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        extension_runner_ref: None,
        session_start_event: None,
        memory: None,
        output_language: None,
        continuity: None,
        continuity_disabled: false,
        telemetry: None,
    });
    let events: Arc<Mutex<Vec<AgentSessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let _ = session.subscribe(Arc::new(move |event| {
        sink.lock().unwrap().push(event.clone())
    }));
    LiveHarness {
        session,
        events,
        temp_dir,
    }
}

/// TS: "should trigger manual compaction via compact()"
#[tokio::test]
#[ignore = "requires live ANTHROPIC_API_KEY"]
async fn should_trigger_manual_compaction_via_compact() {
    run(async {
        let harness = create_session(false).await;
        let session = harness.session.clone();
        session
            .prompt(
                "What is 2+2? Reply with just the number.".to_string(),
                PromptOptions::default(),
            )
            .await
            .unwrap();
        session
            .prompt(
                "What is 3+3? Reply with just the number.".to_string(),
                PromptOptions::default(),
            )
            .await
            .unwrap();

        let result = session.compact(None).await.expect("compact");
        assert!(!result.summary.is_empty());
        assert!(result.tokens_before > 0);
        let messages = session.messages();
        assert!(!messages.is_empty());
        assert_eq!(messages[0].role(), "compactionSummary");
        session.dispose();
        drop(harness.temp_dir);
    })
    .await;
}

/// TS: "should maintain valid session state after compaction"
#[tokio::test]
#[ignore = "requires live ANTHROPIC_API_KEY"]
async fn should_maintain_valid_session_state_after_compaction() {
    run(async {
        let harness = create_session(false).await;
        let session = harness.session.clone();
        session
            .prompt(
                "What is the capital of France? One word answer.".to_string(),
                PromptOptions::default(),
            )
            .await
            .unwrap();
        session
            .prompt(
                "What is the capital of Germany? One word answer.".to_string(),
                PromptOptions::default(),
            )
            .await
            .unwrap();
        session.compact(None).await.expect("compact");
        session
            .prompt(
                "What is the capital of Italy? One word answer.".to_string(),
                PromptOptions::default(),
            )
            .await
            .unwrap();

        let messages = session.messages();
        assert!(!messages.is_empty());
        assert!(messages.iter().any(|message| message.role() == "assistant"));
        session.dispose();
        drop(harness.temp_dir);
    })
    .await;
}

/// TS: "should persist compaction to session file"
#[tokio::test]
#[ignore = "requires live ANTHROPIC_API_KEY"]
async fn should_persist_compaction_to_session_file() {
    run(async {
        let harness = create_session(false).await;
        let session = harness.session.clone();
        session
            .prompt("Say hello".to_string(), PromptOptions::default())
            .await
            .unwrap();
        session
            .prompt("Say goodbye".to_string(), PromptOptions::default())
            .await
            .unwrap();
        session.compact(None).await.expect("compact");

        let entries = session.session_manager().get_entries();
        let compactions: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                SessionEntry::Compaction(compaction) => Some(compaction),
                _ => None,
            })
            .collect();
        assert_eq!(compactions.len(), 1);
        assert!(!compactions[0].summary.is_empty());
        assert!(!compactions[0].first_kept_entry_id.is_empty());
        assert!(compactions[0].tokens_before > 0.0);
        session.dispose();
        drop(harness.temp_dir);
    })
    .await;
}

/// TS: "should work with --no-session mode (in-memory only)"
#[tokio::test]
#[ignore = "requires live ANTHROPIC_API_KEY"]
async fn should_work_with_in_memory_only_mode() {
    run(async {
        let harness = create_session(true).await;
        let session = harness.session.clone();
        session
            .prompt(
                "What is 2+2? Reply with just the number.".to_string(),
                PromptOptions::default(),
            )
            .await
            .unwrap();
        session
            .prompt(
                "What is 3+3? Reply with just the number.".to_string(),
                PromptOptions::default(),
            )
            .await
            .unwrap();

        let result = session.compact(None).await.expect("compact");
        assert!(!result.summary.is_empty());
        let compactions = session
            .session_manager()
            .get_entries()
            .into_iter()
            .filter(|entry| matches!(entry, SessionEntry::Compaction(_)))
            .count();
        assert_eq!(compactions, 1);
        session.dispose();
        drop(harness.temp_dir);
    })
    .await;
}

/// TS: "should emit compaction events during manual compaction"
#[tokio::test]
#[ignore = "requires live ANTHROPIC_API_KEY"]
async fn should_emit_compaction_events_during_manual_compaction() {
    run(async {
        let harness = create_session(false).await;
        let session = harness.session.clone();
        session
            .prompt("Say hello".to_string(), PromptOptions::default())
            .await
            .unwrap();
        session.compact(None).await.expect("compact");

        let events = harness.events.lock().unwrap();
        let compaction_events: Vec<&AgentSessionEvent> = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    AgentSessionEvent::CompactionStart { .. }
                        | AgentSessionEvent::CompactionEnd { .. }
                )
            })
            .collect();
        assert_eq!(compaction_events.len(), 2);
        match compaction_events[0] {
            AgentSessionEvent::CompactionStart { reason } => {
                assert_eq!(*reason, kiana_ext_api::events::CompactionReason::Manual);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        match compaction_events[1] {
            AgentSessionEvent::CompactionEnd {
                reason,
                aborted,
                will_retry,
                ..
            } => {
                assert_eq!(*reason, kiana_ext_api::events::CompactionReason::Manual);
                assert!(!aborted);
                assert!(!will_retry);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        let message_ends = events
            .iter()
            .filter(|event| matches!(event, AgentSessionEvent::MessageEnd { .. }))
            .count();
        assert!(message_ends > 0);
        drop(events);
        session.dispose();
        drop(harness.temp_dir);
    })
    .await;
}

// 引用避免未使用告警。
#[allow(dead_code)]
fn _unused(_: Message, _: &SessionManager) {}
