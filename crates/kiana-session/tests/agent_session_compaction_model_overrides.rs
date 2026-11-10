//! `test/suite/agent-session-compaction-model-overrides.test.ts` 的 Rust 移植（验收门补齐）。
//!
//! `it.each(["manual","pre-prompt","post-run","overflow"])` 展开为 4 个独立测试。
//! 未移植的用例：`captures model identity before awaiting summarization auth` 需要在
//! `ModelRuntime::getAuth` 上做 spy（Rust 没有等价的可注入点）。

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxModelDefinition, FauxResponseStep, faux_assistant_message,
};
use kiana_ai::types::{
    AssistantContentBlock, Message, StopReason, TextContent, TextKind, Usage, UsageCost,
    UserContent, UserContentBlock, UserMessage, UserRole,
};
use kiana_compaction::compaction::CompactionSettings;
use kiana_config::settings::compaction::CompactionModelRef;
use kiana_ext_api::events::{
    CompactionReason, ExtensionEvent, ExtensionEventKind, ExtensionEventResult,
    SessionBeforeCompactResult, SessionEvent,
};
use kiana_session::agent_session::{AgentSessionEvent, ModelMutationOptions, PromptOptions};
use serde_json::json;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Clone, Debug, PartialEq)]
struct RecordedPreparation {
    settings: CompactionSettings,
    reason: CompactionReason,
    first_kept_entry_id: String,
}

fn seed_history(harness: &Harness, total_tokens: u64) -> String {
    let model = harness.session.model().expect("model");
    let mut recent_user_id = String::new();
    for label in ["old", "recent"] {
        let mut text = label.to_string();
        while text.len() < 400 {
            text.push('x');
        }
        recent_user_id =
            harness.append_session_message(AgentMessage::Standard(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text,
                    text_signature: None,
                })]),
                timestamp: now_ms() - 2000,
            })));
        let mut assistant_text = label.to_string();
        while assistant_text.len() < 400 {
            assistant_text.push('y');
        }
        let mut assistant = faux_assistant_message(
            AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: assistant_text,
                text_signature: None,
            }),
            FauxAssistantMessageOptions {
                timestamp: Some(now_ms() - 1000),
                ..Default::default()
            },
        );
        assistant.api = model.api.clone();
        assistant.provider = model.provider.clone();
        assistant.model = model.id.clone();
        assistant.usage = Usage {
            input: total_tokens,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write1h: None,
            reasoning: None,
            total_tokens,
            cost: UsageCost::default(),
        };
        harness.append_session_message(AgentMessage::Standard(Message::Assistant(assistant)));
    }
    recent_user_id
}

async fn run_compaction_path(path: &str) {
    let recorded: Arc<Mutex<Vec<RecordedPreparation>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&recorded);
    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(4000),
            ..Default::default()
        }]),
        tools: Some(Vec::new()),
        settings: Some(settings_from_json(json!({
            "compaction": {
                "enabled": path != "manual",
                "reserveTokens": 10,
                "keepRecentTokens": 20000,
                "modelOverrides": {
                    "faux/faux-1": { "reserveTokens": 2000, "keepRecentTokens": 150 }
                }
            }
        }))),
        extension_factories: vec![factory(move |kiana| {
            let sink = Arc::clone(&sink);
            let _ = kiana.on(
                ExtensionEventKind::SessionBeforeCompact,
                standard(move |event, _ctx| {
                    let sink = Arc::clone(&sink);
                    Box::pin(async move {
                        let ExtensionEvent::Session(SessionEvent::BeforeCompact(before)) = event
                        else {
                            return Ok(None);
                        };
                        sink.lock().unwrap().push(RecordedPreparation {
                            settings: before.preparation.settings.clone(),
                            reason: before.reason,
                            first_kept_entry_id: before.preparation.first_kept_entry_id.clone(),
                        });
                        Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                            SessionBeforeCompactResult {
                                cancel: None,
                                compaction: Some(kiana_compaction::compaction::CompactionResult {
                                    summary: "compacted history".to_string(),
                                    first_kept_entry_id: before
                                        .preparation
                                        .first_kept_entry_id
                                        .clone(),
                                    tokens_before: before.preparation.tokens_before,
                                    estimated_tokens_after: None,
                                    usage: None,
                                    details: None,
                                }),
                            },
                        )))
                    })
                }),
            );
        })],
        ..Default::default()
    })
    .await;
    let recent_user_id = seed_history(&harness, if path == "pre-prompt" { 2500 } else { 650 });

    match path {
        "manual" => {
            harness.session.compact(None).await.unwrap();
        }
        "overflow" => {
            harness.set_responses(vec![
                FauxResponseStep::Message(faux_assistant_message(
                    "",
                    FauxAssistantMessageOptions {
                        stop_reason: Some(StopReason::Error),
                        error_message: Some("prompt is too long".to_string()),
                        ..Default::default()
                    },
                )),
                faux_text_response("recovered"),
            ]);
            harness
                .session
                .prompt("continue".to_string(), PromptOptions::default())
                .await
                .unwrap();
        }
        _ => {
            let text = if path == "post-run" {
                "z".repeat(8000)
            } else {
                "done".to_string()
            };
            harness.set_responses(vec![faux_text_response(text)]);
            harness
                .session
                .prompt("continue".to_string(), PromptOptions::default())
                .await
                .unwrap();
        }
    }

    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded.len(), 1, "path {path}: preparations");
    let preparation = &recorded[0];
    assert_eq!(
        preparation.settings,
        CompactionSettings {
            enabled: path != "manual",
            reserve_tokens: 2000,
            keep_recent_tokens: 150,
        },
        "path {path}"
    );
    let expected_reason = match path {
        "manual" => CompactionReason::Manual,
        "overflow" => CompactionReason::Overflow,
        _ => CompactionReason::Threshold,
    };
    assert_eq!(preparation.reason, expected_reason, "path {path}");
    if path == "manual" || path == "pre-prompt" {
        assert_eq!(
            preparation.first_kept_entry_id, recent_user_id,
            "path {path}"
        );
    }
    let ends = harness.events_of_type("compaction_end");
    assert_eq!(ends.len(), 1, "path {path}: compaction_end count");
    match &ends[0] {
        AgentSessionEvent::CompactionEnd {
            aborted,
            will_retry,
            result: Some(result),
            ..
        } => {
            assert!(!aborted, "path {path}");
            assert_eq!(*will_retry, path == "overflow", "path {path}");
            assert_eq!(result.summary, "compacted history", "path {path}");
        }
        other => panic!("path {path}: unexpected compaction_end {other:?}"),
    }
    assert_eq!(harness.get_pending_response_count(), 0, "path {path}");
    harness.cleanup();
}

/// TS: `it.each(...)`（manual）
#[tokio::test]
async fn uses_model_token_settings_for_manual_compaction() {
    run(run_compaction_path("manual")).await;
}

/// TS: `it.each(...)`（pre-prompt）
#[tokio::test]
async fn uses_model_token_settings_for_pre_prompt_compaction() {
    run(run_compaction_path("pre-prompt")).await;
}

/// TS: `it.each(...)`（post-run）
#[tokio::test]
async fn uses_model_token_settings_for_post_run_compaction() {
    run(run_compaction_path("post-run")).await;
}

/// TS: `it.each(...)`（overflow）
#[tokio::test]
async fn uses_model_token_settings_for_overflow_compaction() {
    run(run_compaction_path("overflow")).await;
}

/// TS: "uses the newly selected model without changing ordinary settings"
#[tokio::test]
async fn uses_the_newly_selected_model_without_changing_ordinary_settings() {
    run(async {
        let recorded: Arc<Mutex<Vec<RecordedPreparation>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&recorded);
        let harness = create_harness(HarnessOptions {
            models: Some(vec![
                FauxModelDefinition {
                    id: "small".to_string(),
                    context_window: Some(4000),
                    ..Default::default()
                },
                FauxModelDefinition {
                    id: "big".to_string(),
                    context_window: Some(10000),
                    ..Default::default()
                },
            ]),
            tools: Some(Vec::new()),
            settings: Some(settings_from_json(json!({
                "compaction": {
                    "reserveTokens": 10,
                    "modelOverrides": {
                        "faux/big": { "reserveTokens": 8000, "keepRecentTokens": 150 }
                    }
                }
            }))),
            extension_factories: vec![factory(move |kiana| {
                let sink = Arc::clone(&sink);
                let _ = kiana.on(
                    ExtensionEventKind::SessionBeforeCompact,
                    standard(move |event, _ctx| {
                        let sink = Arc::clone(&sink);
                        Box::pin(async move {
                            let ExtensionEvent::Session(SessionEvent::BeforeCompact(before)) =
                                event
                            else {
                                return Ok(None);
                            };
                            sink.lock().unwrap().push(RecordedPreparation {
                                settings: before.preparation.settings.clone(),
                                reason: before.reason,
                                first_kept_entry_id: before.preparation.first_kept_entry_id.clone(),
                            });
                            Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                                SessionBeforeCompactResult {
                                    cancel: None,
                                    compaction: Some(
                                        kiana_compaction::compaction::CompactionResult {
                                            summary: "big model summary".to_string(),
                                            first_kept_entry_id: before
                                                .preparation
                                                .first_kept_entry_id
                                                .clone(),
                                            tokens_before: before.preparation.tokens_before,
                                            estimated_tokens_after: None,
                                            usage: None,
                                            details: None,
                                        },
                                    ),
                                },
                            )))
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        seed_history(&harness, 2500);
        harness.set_responses(vec![
            faux_text_response("small response"),
            faux_text_response("big response"),
        ]);
        harness
            .session
            .prompt("continue on small".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert!(harness.events_of_type("compaction_start").is_empty());

        // 保留 small 模型的 usage：下一次检查必须使用当前 big 模型的策略。
        seed_history(&harness, 2500);
        let big = harness.get_model(Some("big")).expect("big");
        harness
            .session
            .set_model(big.clone(), ModelMutationOptions::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("continue on big".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let ends = harness.events_of_type("compaction_end");
        assert_eq!(ends.len(), 1);
        match &ends[0] {
            AgentSessionEvent::CompactionEnd {
                result: Some(result),
                ..
            } => assert_eq!(result.summary, "big model summary"),
            other => panic!("unexpected compaction_end: {other:?}"),
        }
        assert_eq!(
            harness
                .settings_manager
                .get_compaction_reserve_tokens(None)
                .unwrap(),
            10.0
        );

        let small = harness.get_model(Some("small")).expect("small");
        harness
            .session
            .set_model(small.clone(), ModelMutationOptions::default())
            .await
            .unwrap();
        assert_eq!(
            harness
                .settings_manager
                .get_compaction_reserve_tokens(Some(CompactionModelRef::from_model(&small)))
                .unwrap(),
            10.0
        );
        harness.cleanup();
    })
    .await;
}

// 未移植：`captures model identity before awaiting summarization auth`（需要 mock
// `ModelRuntime::getAuth`；TS 通过 `vi.spyOn` 在 auth resolve 期间切换模型）。
