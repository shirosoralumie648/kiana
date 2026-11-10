//! `test/suite/agent-session-compaction.test.ts` 的 Rust 移植（验收门补齐）。
//!
//! 断言语义与 TS 用例一致；`vi.spyOn(_runAutoCompaction)` 无法直接移植的用例改为断言可观察
//! 事件（`compaction_start` / `compaction_end`）与持久化结果。

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use indexmap::IndexMap;
use kiana_agent_core::types::{AgentMessage, AgentTool, AgentToolError, AgentToolExecute};
use kiana_ai::event_stream::create_assistant_message_event_stream;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, DoneReason, Message,
    SimpleStreamOptions, StopReason, SystemContent, SystemMessage, SystemRole, TextContent,
    TextKind, ToolReference, TranscriptContext, Usage, UsageCost, UserContent, UserContentBlock,
    UserMessage, UserRole,
};
use kiana_compaction::compaction::{CompactionResult, CompactionSettings, estimate_tokens};
use kiana_ext_api::events::{
    CompactionReason, ExtensionEvent, ExtensionEventKind, ExtensionEventResult,
    SessionBeforeCompactResult, SessionEvent,
};
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions};
use kiana_session_core::session_manager::SessionEntry;
use serde_json::json;

// ============================================================================
// 辅助
// ============================================================================

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn usage(total_tokens: u64) -> Usage {
    Usage {
        input: total_tokens,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens,
        cost: UsageCost::default(),
    }
}

/// TS `createAssistant(harness, options)`。
fn create_assistant(
    harness: &Harness,
    stop_reason: StopReason,
    error_message: Option<&str>,
    total_tokens: u64,
    timestamp: i64,
) -> AssistantMessage {
    let model = harness.get_model(None).expect("model");
    let mut assistant = faux_assistant_message(
        "",
        FauxAssistantMessageOptions {
            stop_reason: Some(stop_reason),
            error_message: error_message.map(str::to_string),
            timestamp: Some(timestamp),
            ..Default::default()
        },
    );
    assistant.api = model.api.clone();
    assistant.provider = model.provider.clone();
    assistant.model = model.id.clone();
    assistant.usage = usage(total_tokens);
    assistant
}

/// `useSummaryStreamFn(harness, summary, onRequest?)`：把 agent 的 streamFn 换成固定摘要，
/// 返回调用计数；`on_request` 捕获请求上下文/选项。
type OnRequestFn = Arc<dyn Fn(TranscriptContext, Option<&SimpleStreamOptions>) + Send + Sync>;

fn use_summary_stream_fn(
    harness: &Harness,
    summary: &str,
    on_request: Option<OnRequestFn>,
) -> Arc<AtomicUsize> {
    let call_count = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&call_count);
    let summary = summary.to_string();
    harness
        .agent
        .set_stream_function(Arc::new(move |model, context, options| {
            counter.fetch_add(1, Ordering::SeqCst);
            if let Some(on_request) = &on_request {
                on_request(context.clone(), options.as_ref());
            }
            let message = {
                let mut message =
                    faux_assistant_message(summary.clone(), FauxAssistantMessageOptions::default());
                message.api = model.api.clone();
                message.provider = model.provider.clone();
                message.model = model.id.clone();
                message.usage = usage(10);
                message
            };
            Box::pin(async move { Ok(message_stream(message)) })
        }));
    call_count
}

fn message_stream(
    message: AssistantMessage,
) -> kiana_ai::event_stream::AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let _ = stream.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: Box::new(message.clone()),
    });
    stream.end(message);
    stream
}

/// 固定文本结果工具（可标记 `terminate`）。
struct StaticTool {
    text: String,
    terminate: bool,
}

impl AgentToolExecute for StaticTool {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<kiana_agent_core::types::AgentToolResult<serde_json::Value>, AgentToolError>,
    > {
        let text = self.text.clone();
        let terminate = self.terminate;
        Box::pin(async move {
            Ok(kiana_agent_core::types::AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text,
                    text_signature: None,
                })],
                details: serde_json::Value::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: Some(terminate),
            })
        })
    }
}

fn static_tool(name: &str, description: &str, text: String, terminate: bool) -> AgentTool {
    AgentTool {
        name: name.to_string(),
        label: name.to_string(),
        description: description.to_string(),
        parameters: json!({"type": "object", "properties": {}}),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(StaticTool { text, terminate }),
    }
}

fn seed_compactable_session(harness: &Harness) {
    harness
        .settings_manager
        .apply_overrides(settings_from_json(json!({
            "compaction": { "keepRecentTokens": 1 }
        })));
    let now = now_ms();
    harness.append_session_message(AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "message to compact".to_string(),
            text_signature: None,
        })]),
        timestamp: now - 1000,
    })));
    let mut assistant = create_assistant(harness, StopReason::Stop, None, 100, now - 500);
    assistant.content = vec![AssistantContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: "assistant response to compact".to_string(),
        text_signature: None,
    })];
    harness.append_session_message(AgentMessage::Standard(Message::Assistant(assistant)));
}

fn last_assistant_text(harness: &Harness) -> Option<String> {
    harness.session.get_last_assistant_text()
}

fn user_texts(harness: &Harness) -> Vec<String> {
    get_user_texts(harness)
}

fn compaction_entries(harness: &Harness) -> Vec<SessionEntry> {
    harness
        .session_entries()
        .into_iter()
        .filter(|entry| matches!(entry, SessionEntry::Compaction(_)))
        .collect()
}

fn compaction_end_error(harness: &Harness) -> Option<String> {
    harness
        .events_of_type("compaction_end")
        .into_iter()
        .filter_map(|event| match event {
            AgentSessionEvent::CompactionEnd { error_message, .. } => error_message,
            _ => None,
        })
        .next_back()
}

fn compaction_starts(harness: &Harness) -> Vec<CompactionReason> {
    harness
        .events_of_type("compaction_start")
        .into_iter()
        .filter_map(|event| match event {
            AgentSessionEvent::CompactionStart { reason } => Some(reason),
            _ => None,
        })
        .collect()
}

// ============================================================================
// 用例
// ============================================================================

/// TS: "manually compacts using an extension-provided summary"
#[tokio::test]
async fn manually_compacts_using_an_extension_provided_summary() {
    run(async {
        let summary_usage = Usage {
            input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
            cache_write1h: None,
            reasoning: None,
            total_tokens: 100,
            cost: UsageCost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.3,
                cache_write: 0.4,
                total: 1.0,
                total_from_provider: None,
            },
        };
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1 }
            }))),
            extension_factories: vec![factory({
                let summary_usage = summary_usage.clone();
                move |kiana| {
                    let summary_usage = summary_usage.clone();
                    let _ = kiana.on(
                        ExtensionEventKind::SessionBeforeCompact,
                        standard(move |event, _ctx| {
                            let summary_usage = summary_usage.clone();
                            Box::pin(async move {
                                let ExtensionEvent::Session(SessionEvent::BeforeCompact(before)) =
                                    event
                                else {
                                    return Ok(None);
                                };
                                Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                                    SessionBeforeCompactResult {
                                        cancel: None,
                                        compaction: Some(CompactionResult {
                                            summary: "summary from extension".to_string(),
                                            first_kept_entry_id: before
                                                .preparation
                                                .first_kept_entry_id
                                                .clone(),
                                            tokens_before: before.preparation.tokens_before,
                                            estimated_tokens_after: None,
                                            usage: Some(summary_usage),
                                            details: Some(json!({"source": "extension"})),
                                        }),
                                    },
                                )))
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("one"), faux_text_response("two")]);
        harness
            .session
            .prompt("one".to_string(), PromptOptions::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("two".to_string(), PromptOptions::default())
            .await
            .unwrap();
        let stats_before = harness.session.get_session_stats();

        let result = harness.session.compact(None).await.unwrap();
        let entries = compaction_entries(&harness);
        let estimated_tokens_after: u64 =
            harness.session.messages().iter().map(estimate_tokens).sum();

        assert_eq!(result.summary, "summary from extension");
        assert_eq!(result.usage, Some(summary_usage.clone()));
        assert_eq!(result.estimated_tokens_after, Some(estimated_tokens_after));
        assert_eq!(entries.len(), 1);
        if let SessionEntry::Compaction(entry) = &entries[0] {
            assert_eq!(entry.usage, Some(summary_usage.clone()));
        }
        let stats_after = harness.session.get_session_stats();
        assert_eq!(
            stats_after.tokens.input,
            stats_before.tokens.input + summary_usage.input as f64
        );
        assert_eq!(
            stats_after.tokens.output,
            stats_before.tokens.output + summary_usage.output as f64
        );
        assert_eq!(
            stats_after.tokens.cache_read,
            stats_before.tokens.cache_read + summary_usage.cache_read as f64
        );
        assert_eq!(
            stats_after.tokens.cache_write,
            stats_before.tokens.cache_write + summary_usage.cache_write as f64
        );
        assert_eq!(
            stats_after.cost,
            stats_before.cost + summary_usage.cost.total
        );
        let messages = harness.session.messages();
        assert_eq!(messages[0].role(), "system");
        assert_eq!(messages[1].role(), "compactionSummary");
        harness.cleanup();
    })
    .await;
}

/// TS: "checkpoints the replayed system state and folds summarized and retained system patches into it"
#[tokio::test]
async fn checkpoints_the_replayed_system_state_and_folds_system_patches() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("declared")]);
        harness
            .session
            .prompt("declare the prompt".to_string(), PromptOptions::default())
            .await
            .unwrap();
        let declared = harness.session.messages()[0].clone();
        let AgentMessage::Standard(Message::System(declared)) = &declared else {
            panic!("expected declared system message");
        };
        let declared_sections = declared.sections.clone().unwrap_or_default();

        let mut early = IndexMap::new();
        early.insert("early".to_string(), Some("<early>1</early>".to_string()));
        harness.append_session_message(AgentMessage::Standard(Message::System(SystemMessage {
            role: SystemRole::System,
            content: SystemContent::Text("summarized instruction".to_string()),
            sections: Some(early),
            tools_added: None,
            tools_removed: Some(vec![ToolReference {
                name: "bash".to_string(),
            }]),
            timestamp: now_ms(),
        })));
        let first_kept_entry_id =
            harness.append_session_message(AgentMessage::Standard(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "kept before patch".to_string(),
                    text_signature: None,
                })]),
                timestamp: now_ms(),
            })));
        let mut late = IndexMap::new();
        late.insert("extra".to_string(), Some("<extra>late</extra>".to_string()));
        harness.append_session_message(AgentMessage::Standard(Message::System(SystemMessage {
            role: SystemRole::System,
            content: SystemContent::Text("retained instruction".to_string()),
            sections: Some(late),
            tools_added: None,
            tools_removed: Some(vec![ToolReference {
                name: "read".to_string(),
            }]),
            timestamp: now_ms(),
        })));
        harness.append_session_message(AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "kept after patch".to_string(),
                text_signature: None,
            })]),
            timestamp: now_ms(),
        })));
        harness.session.with_session_manager_mut(|manager| {
            manager
                .append_compaction(
                    "compacted",
                    Some(&first_kept_entry_id),
                    100.0,
                    None,
                    None,
                    None,
                )
                .expect("append compaction")
        });

        let messages = harness
            .session
            .with_session_manager_mut(|manager| manager.build_session_context())
            .messages;
        let roles: Vec<String> = messages
            .iter()
            .map(|message| message.role().to_string())
            .collect();
        assert_eq!(roles, vec!["system", "compactionSummary", "user", "user"]);
        let AgentMessage::Standard(Message::System(checkpoint)) = &messages[0] else {
            panic!("expected checkpoint system message");
        };
        assert_eq!(
            match &checkpoint.content {
                SystemContent::Text(text) => text.clone(),
                SystemContent::Blocks(blocks) => blocks
                    .iter()
                    .map(|block| block.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n"),
            },
            "summarized instruction\n\nretained instruction"
        );
        let sections = checkpoint.sections.clone().unwrap_or_default();
        for (key, value) in declared_sections {
            assert_eq!(sections.get(&key), Some(&value), "section {key}");
        }
        assert_eq!(
            sections.get("early"),
            Some(&Some("<early>1</early>".to_string()))
        );
        assert_eq!(
            sections.get("extra"),
            Some(&Some("<extra>late</extra>".to_string()))
        );
        let tools_added = checkpoint
            .tools_added
            .as_ref()
            .map(|tools| {
                tools
                    .iter()
                    .map(|tool| tool.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let expected: Vec<String> = harness
            .session
            .get_active_tool_names()
            .into_iter()
            .filter(|name| name != "read" && name != "bash")
            .collect();
        assert_eq!(tools_added, expected);
        harness.cleanup();
    })
    .await;
}

/// TS: "allows a queued prompt to start when manual compaction ends"
#[tokio::test]
async fn allows_a_queued_prompt_to_start_when_manual_compaction_ends() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1 }
            }))),
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::SessionBeforeCompact,
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::Session(SessionEvent::BeforeCompact(before)) =
                                event
                            else {
                                return Ok(None);
                            };
                            Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                                SessionBeforeCompactResult {
                                    cancel: None,
                                    compaction: Some(CompactionResult {
                                        summary: "manual compacted".to_string(),
                                        first_kept_entry_id: before
                                            .preparation
                                            .first_kept_entry_id
                                            .clone(),
                                        tokens_before: before.preparation.tokens_before,
                                        estimated_tokens_after: None,
                                        usage: None,
                                        details: Some(json!({})),
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
        seed_compactable_session(&harness);
        harness.set_responses(vec![faux_text_response("queued response")]);

        let queued: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> = Arc::new(Mutex::new(None));
        let session = harness.session.clone();
        let queued_ref = Arc::clone(&queued);
        let _unsubscribe = harness.session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::CompactionEnd {
                reason: CompactionReason::Manual,
                result: Some(_),
                ..
            } = event
            {
                assert!(!session.is_compacting());
                let session = session.clone();
                let handle = tokio::task::spawn_local(async move {
                    session
                        .prompt(
                            "queued after compaction".to_string(),
                            PromptOptions::default(),
                        )
                        .await
                        .unwrap();
                });
                *queued_ref.lock().unwrap() = Some(handle);
            }
        }));

        harness.session.compact(None).await.unwrap();
        let handle = queued
            .lock()
            .unwrap()
            .take()
            .expect("compaction_end did not start the queued prompt");
        handle.await.unwrap();

        assert!(user_texts(&harness).contains(&"queued after compaction".to_string()));
        assert_eq!(
            last_assistant_text(&harness).as_deref(),
            Some("queued response")
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "throws when compacting without configured auth"
#[tokio::test]
async fn throws_when_compacting_without_configured_auth() {
    run(async {
        let harness = create_harness(HarnessOptions {
            with_configured_auth: Some(false),
            ..Default::default()
        })
        .await;
        // Auth 只在 Kiana 自己摘要时解析：先确认有内容可压缩。
        seed_compactable_session(&harness);
        let provider = harness.get_model(None).expect("model").provider;
        let error = harness.session.compact(None).await.unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with(&format!("No API key found for {provider}.")),
            "unexpected error: {error}"
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "manually compacts with a custom streamFn when registry auth is absent"
#[tokio::test]
async fn manually_compacts_with_a_custom_stream_fn_when_registry_auth_is_absent() {
    run(async {
        let harness = create_harness(HarnessOptions {
            with_configured_auth: Some(false),
            ..Default::default()
        })
        .await;
        seed_compactable_session(&harness);
        let get_stream_call_count =
            use_summary_stream_fn(&harness, "summary from custom stream", None);

        let result = harness.session.compact(None).await.unwrap();

        assert!(result.summary.contains("summary from custom stream"));
        assert_eq!(get_stream_call_count.load(Ordering::SeqCst), 1);
        harness.cleanup();
    })
    .await;
}

/// TS: "uses the standalone compaction request context"
#[tokio::test]
async fn uses_the_standalone_compaction_request_context() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1 }
            }))),
            ..Default::default()
        })
        .await;
        seed_compactable_session(&harness);

        let transform_called = Arc::new(AtomicBool::new(false));
        let transform_flag = Arc::clone(&transform_called);
        harness
            .agent
            .set_transform_context(Some(Arc::new(move |messages, _signal| {
                transform_flag.store(true, Ordering::SeqCst);
                Box::pin(async move { messages })
            })));
        harness
            .agent
            .set_session_id(Some("active-routing-session".to_string()));
        harness
            .agent
            .set_transport(kiana_ai::types::Transport::Websocket);

        #[derive(Clone)]
        struct CapturedOptions {
            cache_retention: Option<kiana_ai::types::CacheRetention>,
            session_id: Option<String>,
            transport: Option<kiana_ai::types::Transport>,
        }
        let captured_context: Arc<Mutex<Option<TranscriptContext>>> = Arc::new(Mutex::new(None));
        let captured_options: Arc<Mutex<Option<CapturedOptions>>> = Arc::new(Mutex::new(None));
        let context_sink = Arc::clone(&captured_context);
        let options_sink = Arc::clone(&captured_options);
        let _ = use_summary_stream_fn(
            &harness,
            "standalone summary",
            Some(Arc::new(move |context, options| {
                *context_sink.lock().unwrap() = Some(context);
                *options_sink.lock().unwrap() = options.map(|options| CapturedOptions {
                    cache_retention: options.base.cache_retention,
                    session_id: options.base.session_id.clone(),
                    transport: options.base.transport,
                });
            })),
        );

        harness.session.compact(None).await.unwrap();

        assert!(!transform_called.load(Ordering::SeqCst));
        let context = captured_context
            .lock()
            .unwrap()
            .clone()
            .expect("request captured");
        let options = captured_options
            .lock()
            .unwrap()
            .clone()
            .expect("stream options");
        let text = serde_json::to_string(&context.0.messages).unwrap();
        assert!(!text.contains(&harness.session.system_prompt()));
        // 第 6 周起默认摘要使用结构化模板：对话正文仍在 <conversation> 块内，
        // 后面跟随固定的 Objective/.../Files 指令。
        assert!(text.contains("[User]: message to compact"));
        assert!(text.contains("## Objective"));
        assert_eq!(
            options.cache_retention,
            Some(kiana_ai::types::CacheRetention::None)
        );
        assert_ne!(
            options.session_id.as_deref(),
            Some("active-routing-session")
        );
        assert!(options.transport.is_none());
        harness.cleanup();
    })
    .await;
}

/// TS: "persists usage from kiana-generated manual compaction"
#[tokio::test]
async fn persists_usage_from_kiana_generated_manual_compaction() {
    run(async {
        let harness = create_harness(HarnessOptions {
            with_configured_auth: Some(false),
            ..Default::default()
        })
        .await;
        seed_compactable_session(&harness);
        let _ = use_summary_stream_fn(&harness, "summary from custom stream", None);

        let result = harness.session.compact(None).await.unwrap();

        let entries = compaction_entries(&harness);
        assert_eq!(result.usage, Some(usage(10)));
        assert_eq!(entries.len(), 1);
        if let SessionEntry::Compaction(entry) = &entries[0] {
            assert_eq!(entry.usage, Some(usage(10)));
        }
        harness.cleanup();
    })
    .await;
}

/// TS: "auto-compacts with a custom streamFn when registry auth is absent"
#[tokio::test]
async fn auto_compacts_with_a_custom_stream_fn_when_registry_auth_is_absent() {
    run(async {
        let harness = create_harness(HarnessOptions {
            with_configured_auth: Some(false),
            ..Default::default()
        })
        .await;
        seed_compactable_session(&harness);
        let get_stream_call_count =
            use_summary_stream_fn(&harness, "auto summary from custom stream", None);

        harness
            .session
            .run_auto_compaction(CompactionReason::Threshold, false)
            .await;

        let entries = compaction_entries(&harness);
        let end = harness
            .events_of_type("compaction_end")
            .into_iter()
            .next_back();
        assert_eq!(entries.len(), 1);
        match end {
            Some(AgentSessionEvent::CompactionEnd {
                result: Some(result),
                ..
            }) => assert!(result.estimated_tokens_after.unwrap_or(0) > 0),
            other => panic!("unexpected compaction_end: {other:?}"),
        }
        assert_eq!(get_stream_call_count.load(Ordering::SeqCst), 1);
        harness.cleanup();
    })
    .await;
}

/// TS: "notifies extensions when auto-compaction fails"
#[tokio::test]
async fn notifies_extensions_when_auto_compaction_fails() {
    run(async {
        let failed_events: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let events_sink = Arc::clone(&failed_events);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                let events_sink = Arc::clone(&events_sink);
                let _ = kiana.on(
                    ExtensionEventKind::SessionCompactFailed,
                    standard(move |event, _ctx| {
                        let events_sink = Arc::clone(&events_sink);
                        Box::pin(async move {
                            if let ExtensionEvent::Session(SessionEvent::CompactFailed(failed)) =
                                event
                            {
                                events_sink
                                    .lock()
                                    .unwrap()
                                    .push(serde_json::to_value(&failed).unwrap());
                            }
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        seed_compactable_session(&harness);
        harness
            .agent
            .set_stream_function(Arc::new(|_model, _context, _options| {
                Box::pin(async {
                    Err(kiana_ai::AiError::new(
                        kiana_ai::AiErrorKind::Other,
                        "summary generator blew up",
                    ))
                })
            }));

        let success = harness
            .session
            .run_auto_compaction(CompactionReason::Threshold, false)
            .await;

        assert!(!success);
        let end = harness
            .events_of_type("compaction_end")
            .into_iter()
            .next_back()
            .expect("compaction_end");
        match end {
            AgentSessionEvent::CompactionEnd {
                reason,
                aborted,
                will_retry,
                error_message,
                ..
            } => {
                assert_eq!(reason, CompactionReason::Threshold);
                assert!(!aborted);
                assert!(!will_retry);
                assert_eq!(
                    error_message.as_deref(),
                    Some("Auto-compaction failed: summary generator blew up")
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
        let failed = failed_events.lock().unwrap();
        assert_eq!(failed.len(), 1);
        let failed = &failed[0];
        assert_eq!(failed["type"], "session_compact_failed");
        assert_eq!(failed["reason"], "threshold");
        assert_eq!(failed["aborted"], false);
        assert_eq!(failed["willRetry"], false);
        assert_eq!(failed["fromExtension"], false);
        assert_eq!(
            failed["errorMessage"],
            "Auto-compaction failed: summary generator blew up"
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "compacts and resumes after a length stop below the desired output limit"
#[tokio::test]
async fn compacts_and_resumes_after_a_length_stop_below_the_desired_output_limit() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(1000),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1, "reserveTokens": 0 }
            }))),
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::SessionBeforeCompact,
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::Session(SessionEvent::BeforeCompact(before)) =
                                event
                            else {
                                return Ok(None);
                            };
                            Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                                SessionBeforeCompactResult {
                                    cancel: None,
                                    compaction: Some(CompactionResult {
                                        summary: "overflow compacted".to_string(),
                                        first_kept_entry_id: before
                                            .preparation
                                            .first_kept_entry_id
                                            .clone(),
                                        tokens_before: before.preparation.tokens_before,
                                        estimated_tokens_after: None,
                                        usage: None,
                                        details: Some(json!({})),
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
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                "partial response",
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Length),
                    ..Default::default()
                },
            )),
            faux_text_response("completed response"),
        ]);

        harness
            .session
            .prompt("x".repeat(5000), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.faux.state().call_count(), 2);
        match harness
            .events_of_type("compaction_end")
            .into_iter()
            .next_back()
            .expect("compaction_end")
        {
            AgentSessionEvent::CompactionEnd {
                reason,
                aborted,
                will_retry,
                ..
            } => {
                assert_eq!(reason, CompactionReason::Overflow);
                assert!(!aborted);
                assert!(will_retry);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(
            last_assistant_text(&harness).as_deref(),
            Some("completed response")
        );
        harness.cleanup();
    })
    .await;
}

/// TS: `it.each([false, true])` "compacts after an oversized tool result in the same run"
///（回归 #8133 / #9740）。
async fn compacts_after_oversized_tool_result(model_override: bool) {
    run(async {
        let tool_result = format!("large-tool-result:{}", "x".repeat(8000));
        let large_tool = static_tool(
            "large_result",
            "Returns enough content to cross the compaction threshold",
            tool_result.clone(),
            false,
        );
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let observed_settings: Arc<Mutex<Vec<CompactionSettings>>> =
            Arc::new(Mutex::new(Vec::new()));
        let order_ext = Arc::clone(&order);
        let settings_ext = Arc::clone(&observed_settings);
        let settings = if model_override {
            json!({
                "compaction": {
                    "enabled": true,
                    "reserveTokens": 0,
                    "keepRecentTokens": 20000,
                    "modelOverrides": {
                        "faux/faux-1": { "reserveTokens": 400, "keepRecentTokens": 1750 }
                    }
                }
            })
        } else {
            json!({
                "compaction": { "enabled": true, "reserveTokens": 400, "keepRecentTokens": 1750 }
            })
        };
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(2600),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(settings)),
            tools: Some(vec![large_tool]),
            extension_factories: vec![factory(move |kiana| {
                let order = Arc::clone(&order_ext);
                let settings = Arc::clone(&settings_ext);
                let _ = kiana.on(
                    ExtensionEventKind::SessionBeforeCompact,
                    standard(move |event, _ctx| {
                        let order = Arc::clone(&order);
                        let settings = Arc::clone(&settings);
                        Box::pin(async move {
                            let ExtensionEvent::Session(SessionEvent::BeforeCompact(before)) =
                                event
                            else {
                                return Ok(None);
                            };
                            order.lock().unwrap().push("compaction");
                            settings
                                .lock()
                                .unwrap()
                                .push(before.preparation.settings.clone());
                            Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                                SessionBeforeCompactResult {
                                    cancel: None,
                                    compaction: Some(CompactionResult {
                                        summary: "compacted history".to_string(),
                                        first_kept_entry_id: before
                                            .preparation
                                            .first_kept_entry_id
                                            .clone(),
                                        tokens_before: before.preparation.tokens_before,
                                        estimated_tokens_after: None,
                                        usage: None,
                                        details: Some(json!({})),
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
        let resumed_request: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let resumed_clone = Arc::clone(&resumed_request);
        let order_for_provider = Arc::clone(&order);
        let steps: Vec<FauxResponseStep> = vec![
            faux_text_response(format!("old-history:{}", "a".repeat(800))),
            faux_text_response(format!("recent-history:{}", "b".repeat(800))),
            FauxResponseStep::Message(faux_assistant_message(
                faux_tool_call("large_result", IndexMap::new()),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            FauxResponseStep::message_factory(move |input| {
                let resumed = Arc::clone(&resumed_clone);
                let order = Arc::clone(&order_for_provider);
                async move {
                    order.lock().unwrap().push("provider");
                    *resumed.lock().unwrap() =
                        serde_json::to_string(&input.context.0.messages).unwrap();
                    faux_assistant_message("finished after compaction", Default::default())
                }
            }),
        ];
        harness.set_responses(steps);

        harness
            .session
            .prompt("seed old history".to_string(), PromptOptions::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("seed recent history".to_string(), PromptOptions::default())
            .await
            .unwrap();
        let agent_starts_before = harness.events_of_type("agent_start").len();
        harness
            .session
            .prompt("run the large tool".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let order = order.lock().unwrap().clone();
        assert_eq!(
            order.iter().take(2).copied().collect::<Vec<_>>(),
            vec!["compaction", "provider"]
        );
        let observed = observed_settings.lock().unwrap();
        assert_eq!(
            observed[0],
            CompactionSettings {
                enabled: true,
                reserve_tokens: 400,
                keep_recent_tokens: 1750,
            }
        );
        assert_eq!(
            harness.events_of_type("agent_start").len(),
            agent_starts_before + 1
        );
        assert_eq!(
            compaction_starts(&harness).last().copied(),
            Some(CompactionReason::Threshold)
        );
        let resumed = resumed_request.lock().unwrap();
        assert!(resumed.contains("compacted history"));
        assert!(resumed.contains("large-tool-result"));
        assert_eq!(
            last_assistant_text(&harness).as_deref(),
            Some("finished after compaction")
        );
        harness.cleanup();
    })
    .await;
}

/// TS: `it.each([false, true])`（model override: false）
#[tokio::test]
async fn compacts_after_an_oversized_tool_result_without_model_override() {
    compacts_after_oversized_tool_result(false).await;
}

/// TS: `it.each([false, true])`（model override: true）
#[tokio::test]
async fn compacts_after_an_oversized_tool_result_with_model_override() {
    compacts_after_oversized_tool_result(true).await;
}

/// TS: "includes steering queued during compaction in the resumed assistant request"
#[tokio::test]
async fn includes_steering_queued_during_compaction_in_the_resumed_assistant_request() {
    run(async {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let large_tool = static_tool(
            "large_result",
            "Returns enough content to cross the compaction threshold",
            format!("large-tool-result:{}", "x".repeat(6800)),
            false,
        );
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(2600),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": { "enabled": true, "reserveTokens": 400, "keepRecentTokens": 1750 }
            }))),
            tools: Some(vec![large_tool]),
            extension_factories: vec![factory({
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                move |kiana| {
                    let started = Arc::clone(&started);
                    let release = Arc::clone(&release);
                    let _ = kiana.on(
                        ExtensionEventKind::SessionBeforeCompact,
                        standard(move |event, _ctx| {
                            let started = Arc::clone(&started);
                            let release = Arc::clone(&release);
                            Box::pin(async move {
                                let ExtensionEvent::Session(SessionEvent::BeforeCompact(before)) =
                                    event
                                else {
                                    return Ok(None);
                                };
                                started.notify_one();
                                release.notified().await;
                                Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                                    SessionBeforeCompactResult {
                                        cancel: None,
                                        compaction: Some(CompactionResult {
                                            summary: "compacted history".to_string(),
                                            first_kept_entry_id: before
                                                .preparation
                                                .first_kept_entry_id
                                                .clone(),
                                            tokens_before: before.preparation.tokens_before,
                                            estimated_tokens_after: None,
                                            usage: None,
                                            details: Some(json!({})),
                                        }),
                                    },
                                )))
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        let resumed_request: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let resumed_clone = Arc::clone(&resumed_request);
        harness.set_responses(vec![
            faux_text_response(format!("old-history:{}", "a".repeat(800))),
            faux_text_response(format!("recent-history:{}", "b".repeat(800))),
            FauxResponseStep::Message(faux_assistant_message(
                faux_tool_call("large_result", IndexMap::new()),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            FauxResponseStep::message_factory(move |input| {
                let resumed = Arc::clone(&resumed_clone);
                async move {
                    *resumed.lock().unwrap() =
                        serde_json::to_string(&input.context.0.messages).unwrap();
                    faux_assistant_message("finished after compaction", Default::default())
                }
            }),
            faux_text_response("finished after delayed steering"),
        ]);

        harness
            .session
            .prompt("seed old history".to_string(), PromptOptions::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("seed recent history".to_string(), PromptOptions::default())
            .await
            .unwrap();
        let prompt_session = harness.session.clone();
        let prompt = tokio::task::spawn_local(async move {
            prompt_session
                .prompt("run the large tool".to_string(), PromptOptions::default())
                .await
        });
        started.notified().await;
        harness
            .session
            .steer("change direction".to_string(), None, None)
            .await
            .unwrap();
        release.notify_one();
        prompt.await.unwrap().unwrap();

        assert!(resumed_request.lock().unwrap().contains("change direction"));
        assert_eq!(harness.faux.state().call_count(), 4);
        harness.cleanup();
    })
    .await;
}

/// TS: "does not compact after a terminating tool result"
#[tokio::test]
async fn does_not_compact_after_a_terminating_tool_result() {
    run(async {
        let tool = static_tool(
            "terminate_with_large_result",
            "Returns enough content to cross the compaction threshold, then terminates",
            format!("large-tool-result:{}", "x".repeat(6800)),
            true,
        );
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(2600),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": { "enabled": true, "reserveTokens": 400, "keepRecentTokens": 1750 }
            }))),
            tools: Some(vec![tool]),
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::SessionBeforeCompact,
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::Session(SessionEvent::BeforeCompact(before)) =
                                event
                            else {
                                return Ok(None);
                            };
                            Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                                SessionBeforeCompactResult {
                                    cancel: None,
                                    compaction: Some(CompactionResult {
                                        summary: "unexpected compaction".to_string(),
                                        first_kept_entry_id: before
                                            .preparation
                                            .first_kept_entry_id
                                            .clone(),
                                        tokens_before: before.preparation.tokens_before,
                                        estimated_tokens_after: None,
                                        usage: None,
                                        details: Some(json!({})),
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
        harness.set_responses(vec![
            faux_text_response(format!("old-history:{}", "a".repeat(800))),
            faux_text_response(format!("recent-history:{}", "b".repeat(800))),
            FauxResponseStep::Message(faux_assistant_message(
                faux_tool_call("terminate_with_large_result", IndexMap::new()),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
        ]);

        harness
            .session
            .prompt("seed old history".to_string(), PromptOptions::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("seed recent history".to_string(), PromptOptions::default())
            .await
            .unwrap();
        harness
            .session
            .prompt(
                "run the terminating tool".to_string(),
                PromptOptions::default(),
            )
            .await
            .unwrap();

        assert!(harness.events_of_type("compaction_start").is_empty());
        assert!(compaction_entries(&harness).is_empty());
        assert_eq!(harness.get_pending_response_count(), 0);
        harness.cleanup();
    })
    .await;
}

/// TS: "does not compact when a length stop reaches the desired output limit"
#[tokio::test]
async fn does_not_compact_when_a_length_stop_reaches_the_desired_output_limit() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(1_000_000),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            "x".repeat(400),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Length),
                ..Default::default()
            },
        ))]);

        harness
            .session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.faux.state().call_count(), 1);
        assert!(harness.events_of_type("compaction_start").is_empty());
        harness.cleanup();
    })
    .await;
}

/// TS: "stops after one compact-and-retry when a second response is also truncated"
#[tokio::test]
async fn stops_after_one_compact_and_retry_when_a_second_response_is_also_truncated() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(1_000_000),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1, "reserveTokens": 0 }
            }))),
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::SessionBeforeCompact,
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::Session(SessionEvent::BeforeCompact(before)) =
                                event
                            else {
                                return Ok(None);
                            };
                            Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                                SessionBeforeCompactResult {
                                    cancel: None,
                                    compaction: Some(CompactionResult {
                                        summary: "overflow compacted".to_string(),
                                        first_kept_entry_id: before
                                            .preparation
                                            .first_kept_entry_id
                                            .clone(),
                                        tokens_before: before.preparation.tokens_before,
                                        estimated_tokens_after: None,
                                        usage: None,
                                        details: Some(json!({})),
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
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                "x".repeat(64),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Length),
                    timestamp: Some(now_ms() + 10_000),
                    ..Default::default()
                },
            )),
            FauxResponseStep::Message(faux_assistant_message(
                "y".repeat(64),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Length),
                    timestamp: Some(now_ms() + 10_000),
                    ..Default::default()
                },
            )),
        ]);

        harness
            .session
            .prompt("x".repeat(5000), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.faux.state().call_count(), 2);
        let overflow_starts = compaction_starts(&harness)
            .into_iter()
            .filter(|reason| *reason == CompactionReason::Overflow)
            .count();
        assert_eq!(overflow_starts, 1);
        assert_eq!(
            compaction_end_error(&harness).as_deref(),
            Some("Truncated response recovery failed after one compact-and-retry attempt.")
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "keeps overflow wording when a repeated length stop fills the context window"
#[tokio::test]
async fn keeps_overflow_wording_when_a_repeated_length_stop_fills_the_context_window() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(100),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            ..Default::default()
        })
        .await;
        let length_overflow = create_assistant(&harness, StopReason::Length, None, 100, now_ms());

        // 第一次检查不会产生压缩（会话树里没有可压缩内容），第二次进入 overflow 一次性恢复
        // 失败分支并发出错误事件。
        harness.session.check_compaction_for_test(&length_overflow).await;
        let mut second = length_overflow.clone();
        second.timestamp = now_ms() + 1;
        harness.session.check_compaction_for_test(&second).await;

        assert_eq!(
            compaction_end_error(&harness).as_deref(),
            Some("Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.")
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "cancels in-progress manual compaction when abortCompaction is called"
#[tokio::test]
async fn cancels_in_progress_manual_compaction_when_abort_compaction_is_called() {
    run(async {
        let started = Arc::new(tokio::sync::Notify::new());
        let (harness, _original_stream) =
            create_abortable_compaction_harness(Arc::clone(&started)).await;
        let session = harness.session.clone();
        let compact = tokio::task::spawn_local(async move { session.compact(None).await });
        started.notified().await;
        harness.session.abort_compaction();
        let error = compact.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("Compaction cancelled"));
        harness.cleanup();
    })
    .await;
}

async fn create_abortable_compaction_harness(
    started: Arc<tokio::sync::Notify>,
) -> (Harness, kiana_agent_core::types::StreamFn) {
    let harness = create_harness(HarnessOptions {
        settings: Some(settings_from_json(json!({
            "compaction": { "keepRecentTokens": 1 }
        }))),
        ..Default::default()
    })
    .await;
    seed_compactable_session(&harness);
    // TS 用持有 `event.signal` 的扩展 handler 卡住压缩；Rust 的扩展派发把事件 JSON 化后
    // 重建（signal 不跨 JSON），因此用「永不结束的摘要流」等价地让压缩停留在进行中，
    // 直到 abortCompaction 取消 token。
    let original_stream = harness.agent.stream_function();
    harness
        .agent
        .set_stream_function(Arc::new(move |_model, _context, options| {
            started.notify_one();
            Box::pin(async move {
                if let Some(token) = options.and_then(|options| options.base.signal.clone()) {
                    token.cancelled().await;
                }
                let stream = create_assistant_message_event_stream();
                let message = AssistantMessage {
                    stop_reason: StopReason::Aborted,
                    ..Default::default()
                };
                let _ = stream.push(AssistantMessageEvent::Error {
                    reason: kiana_ai::types::ErrorReason::Aborted,
                    error: Box::new(message.clone()),
                });
                stream.end(message);
                Ok(stream)
            })
        }));
    (harness, original_stream)
}

/// TS: "aborts an in-progress manual compaction and waits until the session is idle"（#8920）
#[tokio::test]
async fn aborts_an_in_progress_manual_compaction_and_waits_until_the_session_is_idle() {
    run(async {
        let started = Arc::new(tokio::sync::Notify::new());
        let (harness, original_stream) =
            create_abortable_compaction_harness(Arc::clone(&started)).await;
        harness.set_responses(vec![faux_text_response("continued")]);
        let session = harness.session.clone();
        let compact = tokio::task::spawn_local(async move { session.compact(None).await });
        started.notified().await;
        harness.session.abort().await;
        let error = compact.await.unwrap().unwrap_err();
        harness.agent.set_stream_function(original_stream);
        assert!(error.to_string().contains("Compaction cancelled"));

        match harness
            .events_of_type("compaction_end")
            .into_iter()
            .next_back()
            .expect("compaction_end")
        {
            AgentSessionEvent::CompactionEnd {
                reason, aborted, ..
            } => {
                assert_eq!(reason, CompactionReason::Manual);
                assert!(aborted);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(!harness.session.is_compacting());
        assert!(harness.session.is_idle());

        harness
            .session
            .prompt("next prompt".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(last_assistant_text(&harness).as_deref(), Some("continued"));
        harness.cleanup();
    })
    .await;
}

/// TS: "resumes after threshold compaction when only agent-level queued messages exist"
#[tokio::test]
async fn resumes_after_threshold_compaction_when_only_agent_level_queued_messages_exist() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1 }
            }))),
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::SessionBeforeCompact,
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::Session(SessionEvent::BeforeCompact(before)) =
                                event
                            else {
                                return Ok(None);
                            };
                            Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                                SessionBeforeCompactResult {
                                    cancel: None,
                                    compaction: Some(CompactionResult {
                                        summary: "auto compacted".to_string(),
                                        first_kept_entry_id: before
                                            .preparation
                                            .first_kept_entry_id
                                            .clone(),
                                        tokens_before: before.preparation.tokens_before,
                                        estimated_tokens_after: None,
                                        usage: None,
                                        details: Some(json!({})),
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
        harness.set_responses(vec![faux_text_response("one"), faux_text_response("two")]);
        harness
            .session
            .prompt("first".to_string(), PromptOptions::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("second".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let mut custom = IndexMap::new();
        custom.insert("role".to_string(), json!("custom"));
        custom.insert("customType".to_string(), json!("test"));
        custom.insert(
            "content".to_string(),
            json!([{"type": "text", "text": "queued custom"}]),
        );
        custom.insert("display".to_string(), json!(false));
        custom.insert("timestamp".to_string(), json!(now_ms()));
        harness
            .agent
            .follow_up(AgentMessage::Custom(Box::new(custom)));
        let resumed = harness
            .session
            .run_auto_compaction(CompactionReason::Threshold, false)
            .await;
        assert!(resumed);
        harness.cleanup();
    })
    .await;
}

/// TS: "does not retry overflow recovery more than once"
#[tokio::test]
async fn does_not_retry_overflow_recovery_more_than_once() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let overflow = create_assistant(
            &harness,
            StopReason::Error,
            Some("prompt is too long"),
            0,
            now_ms(),
        );

        harness.session.check_compaction_for_test(&overflow).await;
        let mut second = overflow.clone();
        second.timestamp = now_ms() + 1;
        harness.session.check_compaction_for_test(&second).await;

        // 第一次进入恢复分支（内部 `_runAutoCompaction` 因无内容可压缩返回 false），
        // 第二次不再尝试压缩，只发一次 overflow 错误事件。
        assert!(compaction_starts(&harness).is_empty());
        assert_eq!(
            compaction_end_error(&harness).as_deref(),
            Some("Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.")
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "compacts successful overflow responses without retrying"
#[tokio::test]
async fn compacts_successful_overflow_responses_without_retrying() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "compaction": { "enabled": true, "keepRecentTokens": 1, "reserveTokens": 0 }
            }))),
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(1),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::SessionBeforeCompact,
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::Session(SessionEvent::BeforeCompact(before)) =
                                event
                            else {
                                return Ok(None);
                            };
                            Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                                SessionBeforeCompactResult {
                                    cancel: None,
                                    compaction: Some(CompactionResult {
                                        summary: "successful overflow compacted".to_string(),
                                        first_kept_entry_id: before
                                            .preparation
                                            .first_kept_entry_id
                                            .clone(),
                                        tokens_before: before.preparation.tokens_before,
                                        estimated_tokens_after: None,
                                        usage: None,
                                        details: Some(json!({})),
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
        harness.set_responses(vec![faux_text_response("completed answer")]);

        harness
            .session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .unwrap();

        match harness
            .events_of_type("compaction_end")
            .into_iter()
            .next_back()
            .expect("compaction_end")
        {
            AgentSessionEvent::CompactionEnd {
                reason,
                aborted,
                will_retry,
                ..
            } => {
                assert_eq!(reason, CompactionReason::Overflow);
                assert!(!aborted);
                assert!(!will_retry);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(harness.faux.state().call_count(), 1);
        harness.cleanup();
    })
    .await;
}

/// TS: "ignores stale pre-compaction assistant usage on pre-prompt checks"
#[tokio::test]
async fn ignores_stale_pre_compaction_assistant_usage_on_pre_prompt_checks() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let stale_timestamp = now_ms() - 10_000;
        let stale_assistant =
            create_assistant(&harness, StopReason::Stop, None, 610_000, stale_timestamp);
        harness.append_session_message(AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "before compaction".to_string(),
                text_signature: None,
            })]),
            timestamp: stale_timestamp - 1000,
        })));
        harness.append_session_message(AgentMessage::Standard(Message::Assistant(
            stale_assistant.clone(),
        )));
        let first_kept = harness
            .session_entries()
            .first()
            .expect("first entry")
            .id()
            .to_string();
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.append_compaction(
                    "summary",
                    Some(&first_kept),
                    stale_assistant.usage.total_tokens as f64,
                    None,
                    Some(false),
                    None,
                )
            })
            .expect("append compaction");
        harness.append_session_message(AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "after compaction".to_string(),
                text_signature: None,
            })]),
            timestamp: now_ms(),
        })));

        harness
            .session
            .check_compaction_for_test(&stale_assistant)
            .await;

        assert!(harness.events_of_type("compaction_start").is_empty());
        harness.cleanup();
    })
    .await;
}

/// TS: "triggers threshold compaction for error messages using the last successful usage"
#[tokio::test]
async fn triggers_threshold_compaction_for_error_messages_using_last_successful_usage() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1 }
            }))),
            ..Default::default()
        })
        .await;
        let successful = create_assistant(&harness, StopReason::Stop, None, 190_000, now_ms());
        let error = create_assistant(
            &harness,
            StopReason::Error,
            Some("529 overloaded"),
            0,
            now_ms() + 1000,
        );
        harness.append_session_message(AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "hello".to_string(),
                text_signature: None,
            })]),
            timestamp: now_ms() - 1000,
        })));
        harness.append_session_message(AgentMessage::Standard(Message::Assistant(
            successful.clone(),
        )));
        harness.append_session_message(AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "retry".to_string(),
                text_signature: None,
            })]),
            timestamp: now_ms() + 500,
        })));
        harness.append_session_message(AgentMessage::Standard(Message::Assistant(error.clone())));
        let mut messages = harness.agent.messages();
        messages.extend([
            AgentMessage::Standard(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "hello".to_string(),
                    text_signature: None,
                })]),
                timestamp: now_ms() - 1000,
            })),
            AgentMessage::Standard(Message::Assistant(successful)),
            AgentMessage::Standard(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "retry".to_string(),
                    text_signature: None,
                })]),
                timestamp: now_ms() + 500,
            })),
            AgentMessage::Standard(Message::Assistant(error.clone())),
        ]);
        harness.agent.set_messages(messages);

        harness.session.check_compaction_for_test(&error).await;

        assert_eq!(
            compaction_starts(&harness).last().copied(),
            Some(CompactionReason::Threshold)
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "does not trigger threshold compaction for error messages when no prior usage exists"
#[tokio::test]
async fn does_not_trigger_threshold_compaction_for_error_messages_without_prior_usage() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let error = create_assistant(
            &harness,
            StopReason::Error,
            Some("529 overloaded"),
            0,
            now_ms(),
        );
        harness.agent.set_messages(vec![
            AgentMessage::Standard(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "hello".to_string(),
                    text_signature: None,
                })]),
                timestamp: now_ms() - 1000,
            })),
            AgentMessage::Standard(Message::Assistant(error.clone())),
        ]);

        harness.session.check_compaction_for_test(&error).await;

        assert!(harness.events_of_type("compaction_start").is_empty());
        harness.cleanup();
    })
    .await;
}

/// TS: "does not trigger threshold compaction when only kept pre-compaction usage exists"
#[tokio::test]
async fn does_not_trigger_threshold_compaction_when_only_kept_pre_compaction_usage_exists() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let pre_compaction_timestamp = now_ms() - 10_000;
        let kept_assistant = create_assistant(
            &harness,
            StopReason::Stop,
            None,
            190_000,
            pre_compaction_timestamp,
        );
        harness.append_session_message(AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "before compaction".to_string(),
                text_signature: None,
            })]),
            timestamp: pre_compaction_timestamp - 1000,
        })));
        harness.append_session_message(AgentMessage::Standard(Message::Assistant(
            kept_assistant.clone(),
        )));
        let first_kept = harness
            .session_entries()
            .first()
            .expect("first entry")
            .id()
            .to_string();
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.append_compaction(
                    "summary",
                    Some(&first_kept),
                    kept_assistant.usage.total_tokens as f64,
                    None,
                    Some(false),
                    None,
                )
            })
            .expect("append compaction");

        let error = create_assistant(
            &harness,
            StopReason::Error,
            Some("529 overloaded"),
            0,
            now_ms(),
        );
        harness.agent.set_messages(vec![
            AgentMessage::Standard(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "kept user".to_string(),
                    text_signature: None,
                })]),
                timestamp: pre_compaction_timestamp - 1000,
            })),
            AgentMessage::Standard(Message::Assistant(kept_assistant)),
            AgentMessage::Standard(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "new prompt".to_string(),
                    text_signature: None,
                })]),
                timestamp: now_ms() - 500,
            })),
            AgentMessage::Standard(Message::Assistant(error.clone())),
        ]);

        harness.session.check_compaction_for_test(&error).await;

        assert!(harness.events_of_type("compaction_start").is_empty());
        harness.cleanup();
    })
    .await;
}

/// TS: "does not trigger threshold compaction below the threshold or when disabled"
#[tokio::test]
async fn does_not_trigger_threshold_compaction_below_the_threshold_or_when_disabled() {
    run(async {
        let below_threshold = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "compaction": { "enabled": true, "reserveTokens": 1000 }
            }))),
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(200_000),
                ..Default::default()
            }]),
            ..Default::default()
        })
        .await;
        let disabled = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "compaction": { "enabled": false }
            }))),
            ..Default::default()
        })
        .await;

        let small = create_assistant(&below_threshold, StopReason::Stop, None, 1_000, now_ms());
        below_threshold
            .session
            .check_compaction_for_test(&small)
            .await;
        let huge = create_assistant(&disabled, StopReason::Stop, None, 1_000_000, now_ms());
        disabled.session.check_compaction_for_test(&huge).await;

        assert!(
            below_threshold
                .events_of_type("compaction_start")
                .is_empty()
        );
        assert!(disabled.events_of_type("compaction_start").is_empty());
        below_threshold.cleanup();
        disabled.cleanup();
    })
    .await;
}

// 未移植：`throws when compacting without a model`（TS 直接把 `agent.state.model = undefined`；
// Rust `Agent::set_model` 只接受 `Model`，没有清空模型的公开 API）。

// ============================================================================
// provider-resolved bearer auth（TS: "manually compacts with provider-resolved bearer auth"）
// ============================================================================

struct BearerAuth;

#[async_trait::async_trait]
impl kiana_ai::auth::types::ApiKeyAuth for BearerAuth {
    fn name(&self) -> &str {
        "Faux bearer token"
    }

    async fn resolve(
        &self,
        _input: kiana_ai::auth::types::ApiKeyAuthInput<'_>,
    ) -> Result<Option<kiana_ai::auth::types::AuthResult>, kiana_ai::auth::types::AuthError> {
        let mut headers = indexmap::IndexMap::new();
        headers.insert(
            "Authorization".to_string(),
            Some("Bearer ambient-token".to_string()),
        );
        Ok(Some(kiana_ai::auth::types::AuthResult::new(
            kiana_ai::auth::types::ModelAuth {
                api_key: None,
                headers: Some(headers),
                base_url: None,
            },
        )))
    }
}

/// TS: "manually compacts with provider-resolved bearer auth"
#[tokio::test]
async fn manually_compacts_with_provider_resolved_bearer_auth() {
    run(async {
        let harness = create_harness(HarnessOptions {
            with_configured_auth: Some(false),
            ..Default::default()
        })
        .await;
        let model = harness.get_model(None).expect("model");
        let provider = kiana_ai::models::provider::create_provider(
            kiana_ai::models::provider::CreateProviderOptions {
                id: model.provider.clone(),
                name: Some("Faux bearer provider".to_string()),
                base_url: Some(model.base_url.clone()),
                headers: None,
                auth: kiana_ai::auth::types::ProviderAuth {
                    api_key: Some(Arc::new(BearerAuth)),
                    oauth: None,
                },
                models: harness
                    .models
                    .iter()
                    .cloned()
                    .map(kiana_ai::types::AnyModel::Chat)
                    .collect(),
                fetch_models: None,
                filter_models: None,
                filter_all_models: None,
                api: Some(kiana_ai::models::provider::ProviderApiImpl::Single(
                    Arc::new(harness.faux.clone()),
                )),
                images: None,
                classifiers: None,
            },
        )
        .expect("bearer provider");
        harness
            .session
            .model_runtime()
            .register_native_provider(provider)
            .expect("register provider");
        seed_compactable_session(&harness);

        type ObservedAuth = (
            Option<String>,
            Option<indexmap::IndexMap<String, Option<String>>>,
        );
        let observed: Arc<Mutex<Option<ObservedAuth>>> = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&observed);
        let call_count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&call_count);
        harness
            .agent
            .set_stream_function(Arc::new(move |model, _context, options| {
                counter.fetch_add(1, Ordering::SeqCst);
                let api_key = options
                    .as_ref()
                    .and_then(|options| options.base.api_key.clone());
                let headers = options
                    .as_ref()
                    .and_then(|options| options.base.headers.clone());
                *sink.lock().unwrap() = Some((api_key, headers));
                let message = {
                    let mut message = faux_assistant_message(
                        "summary with bearer auth",
                        FauxAssistantMessageOptions::default(),
                    );
                    message.api = model.api.clone();
                    message.provider = model.provider.clone();
                    message.model = model.id.clone();
                    message.usage = usage(10);
                    message
                };
                Box::pin(async move { Ok(message_stream(message)) })
            }));

        let result = harness.session.compact(None).await.unwrap();

        assert!(result.summary.contains("summary with bearer auth"));
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        let (api_key, headers) = observed.lock().unwrap().clone().expect("stream options");
        assert!(api_key.is_none());
        assert_eq!(
            headers.and_then(|headers| headers.get("Authorization").cloned().flatten()),
            Some("Bearer ambient-token".to_string())
        );
        assert_eq!(harness.faux.state().call_count(), 0);
        harness.cleanup();
    })
    .await;
}
