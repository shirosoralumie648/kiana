//! `test/harness-events.test.ts` 的 Rust 端口。
//!
//! 覆盖：attachment 快照、100 个待投递批次逐个交付、第 101 个折叠成一次快照、关闭结束流、
//! 排队等待 Session 线时被取消的 attachment、流式 partial 的重建与 abort 转换、以及
//! toolcall_delta 的按路径追加。

#![allow(clippy::await_holding_lock)]

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;
#[path = "support/task.rs"]
mod task_support;

use std::any::Any;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_chord::context::{AbortReason, AbortSignal, Context, with_abort_signal};
use kiana_durable::Session;
use kiana_durable::harness::events::AgentEvent;
use kiana_durable::harness::types::Conversation as ConversationTrait;
use kiana_durable::types::{
    BoxError, CommitOptions, ConversationId, EntryDraft, JsonValue, SessionChange, Storage, Tx,
    WatchEnd, WatchListener,
};

use chat_support::{ChatSetup, context, drained, open_chat, text_of, wait_for};

fn memory() -> Arc<dyn Storage> {
    Arc::new(kiana_durable::storage::memory::MemoryStorage::new()) as Arc<dyn Storage>
}

struct Recorder {
    batches: Mutex<Vec<Vec<AgentEvent>>>,
}

#[async_trait]
impl WatchListener<Vec<AgentEvent>> for Recorder {
    async fn call(
        &self,
        value: Vec<AgentEvent>,
        _ops: Vec<kiana_chord::delta::Op>,
        _context: Arc<dyn Context>,
    ) {
        self.batches.lock().unwrap().push(value);
    }
}

/// 追加一条带 `data` 的展示条目。
struct AppendNote {
    conversation_id: ConversationId,
    data: JsonValue,
}

#[async_trait]
impl SessionChange for AppendNote {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let record = tx
            .append_entry(
                self.conversation_id,
                EntryDraft {
                    key_order: None,
                    kind: "note".to_owned(),
                    model: None,
                    data: Some(self.data.clone()),
                    head: None,
                    edits: None,
                },
            )
            .await?;
        Ok(Box::new(record))
    }
}

async fn append_note(
    conversation: &Arc<dyn ConversationTrait>,
    data: JsonValue,
) -> kiana_durable::types::EntryRecord {
    let value = conversation
        .commit(
            &AppendNote {
                conversation_id: conversation.id(),
                data,
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect("commit");
    *value
        .downcast::<kiana_durable::types::EntryRecord>()
        .expect("entry")
}

#[tokio::test(flavor = "multi_thread")]
async fn hydrates_the_attachment_snapshot() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let stream = kiana_durable::harness::events::watch_events(
        &*harness as &dyn kiana_durable::harness::types::Harness,
        root.id(),
        &*context(),
    )
    .await
    .expect("stream");
    let snapshot = &stream.snapshot;
    assert!(snapshot.entries.is_empty());
    assert!(snapshot.tools.is_empty());
    assert!(snapshot.nested_tools.is_empty());
    assert!(snapshot.compactions.is_empty());
    assert!(snapshot.inbox.is_empty());
    assert!(snapshot.run.is_none());
    assert!(snapshot.generation.is_none());
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delivers_each_of_100_pending_batches_in_its_own_call_in_order() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let stream = kiana_durable::harness::events::watch_events(
        &*harness as &dyn kiana_durable::harness::types::Harness,
        root.id(),
        &*context(),
    )
    .await
    .expect("stream");
    for index in 0..100 {
        let _ = append_note(&root, serde_json::json!({"index": index})).await;
    }
    let recorder = Arc::new(Recorder {
        batches: Mutex::new(Vec::new()),
    });
    stream.start(Arc::clone(&recorder) as Arc<dyn WatchListener<Vec<AgentEvent>>>);
    drained(&stream).await;
    let batches = recorder.batches.lock().unwrap();
    assert_eq!(batches.len(), 100);
    let data: Vec<JsonValue> = batches
        .iter()
        .map(|events| match events.as_slice() {
            [AgentEvent::EntryAppended { entry }] => entry.data.clone().expect("data"),
            other => panic!("unexpected batch: {other:?}"),
        })
        .collect();
    let expected: Vec<JsonValue> = (0..100)
        .map(|index| serde_json::json!({"index": index}))
        .collect();
    assert_eq!(data, expected);
    drop(batches);
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replaces_undelivered_batches_with_one_snapshot_after_100_pending_batches() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let stream = kiana_durable::harness::events::watch_events(
        &*harness as &dyn kiana_durable::harness::types::Harness,
        root.id(),
        &*context(),
    )
    .await
    .expect("stream");
    for _ in 0..101 {
        let _ = append_note(&root, serde_json::json!(null)).await;
    }
    let recorder = Arc::new(Recorder {
        batches: Mutex::new(Vec::new()),
    });
    stream.start(Arc::clone(&recorder) as Arc<dyn WatchListener<Vec<AgentEvent>>>);
    drained(&stream).await;
    let batches = recorder.batches.lock().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 1);
    match &batches[0][0] {
        AgentEvent::Snapshot(snapshot) => assert_eq!(snapshot.entries.len(), 101),
        other => panic!("unexpected event: {other:?}"),
    }
    drop(batches);
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_the_stream_with_the_harness() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let stream = kiana_durable::harness::events::watch_events(
        &*harness as &dyn kiana_durable::harness::types::Harness,
        root.id(),
        &*context(),
    )
    .await
    .expect("stream");
    harness.close(&*context()).await;
    assert!(matches!(stream.closed().await, WatchEnd::Ended));
}

/// 一个等释放的提交，用来占住 Session 线。
struct Blocking {
    release: Arc<tokio::sync::Notify>,
    started: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl SessionChange for Blocking {
    async fn run(&self, _tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(Box::new(()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_an_attachment_cancelled_while_it_waits_for_the_session_line() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let release = Arc::new(tokio::sync::Notify::new());
    let started = Arc::new(tokio::sync::Notify::new());
    let change = Blocking {
        release: Arc::clone(&release),
        started: Arc::clone(&started),
    };
    let committing = Arc::clone(&root);
    let commit_context = context();
    let blocking = tokio::spawn(async move {
        committing
            .commit(&change, &*commit_context, CommitOptions::default())
            .await
            .expect("blocking commit")
    });
    started.notified().await;

    let signal = AbortSignal::new();
    let cancelled_context = with_abort_signal(signal.clone(), Arc::clone(&context()));
    let harness_for_attach = Arc::clone(&harness);
    let conversation_id = root.id();
    let attaching = tokio::spawn(async move {
        kiana_durable::harness::events::watch_events(
            &*harness_for_attach as &dyn kiana_durable::harness::types::Harness,
            conversation_id,
            &*cancelled_context,
        )
        .await
    });
    tokio::task::yield_now().await;
    signal.abort(Some(AbortReason::error(std::io::Error::other("cancelled"))));
    release.notify_one();
    blocking.await.expect("the blocking commit joins");
    let error = match attaching.await.expect("the attach task joins") {
        Ok(_) => panic!("the attachment is cancelled"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("cancelled"), "{error}");
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// 序列化面（回归：SnapshotEvent 自带 type；浮点时间戳按 JS 格式写出）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_serializes_like_the_typescript_event() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let stream = kiana_durable::harness::events::watch_events(
        &*harness as &dyn kiana_durable::harness::types::Harness,
        root.id(),
        &*context(),
    )
    .await
    .expect("stream");
    // 字段顺序与 TS 的 `snapshotOf` 一致，`type` 在最前。
    let keys: Vec<String> = serde_json::to_value(&stream.snapshot)
        .expect("snapshot value")
        .as_object()
        .expect("object")
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        keys,
        vec![
            "type",
            "entries",
            "tools",
            "nestedTools",
            "compactions",
            "inbox",
            "agent",
            "usage",
        ]
    );
    let text = serde_json::to_string(&stream.snapshot).expect("snapshot text");
    assert!(text.starts_with(r#"{"type":"snapshot","#), "{text}");
    // 作为 `AgentEvent` 的一条事件，字节不变（枚举标签不会与结构体的 type 重复）。
    let event = AgentEvent::Snapshot(stream.snapshot.clone());
    assert_eq!(
        serde_json::to_string(&event).expect("event text"),
        text,
        "the untagged variant serializes exactly like the snapshot"
    );
    // 反序列化仍按 `type` 判别。
    let parsed: AgentEvent = serde_json::from_str(&text).expect("round trip");
    assert_eq!(parsed, event);
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

#[test]
fn event_timestamps_serialize_like_javascript_numbers() {
    let retry = AgentEvent::AutoRetryStart {
        attempt: 1,
        at: 1_700_000_000_000.0,
        error_message: "x".to_owned(),
    };
    assert_eq!(
        serde_json::to_string(&retry).expect("retry"),
        r#"{"type":"auto_retry_start","attempt":1,"at":1700000000000,"errorMessage":"x"}"#
    );
    let deferred = AgentEvent::DeferredPoll {
        poll_at: 1_700_000_000_000.0,
    };
    assert_eq!(
        serde_json::to_string(&deferred).expect("deferred"),
        r#"{"type":"deferred_poll","pollAt":1700000000000}"#
    );
    // 快照里的 generation 退避与轮询同样走 JS 数字格式。
    let generation = kiana_durable::harness::events::SnapshotGeneration {
        attempt: 2,
        message: None,
        retry: Some(kiana_durable::harness::live::LiveRetry {
            at: 1_700_000_000_000.0,
            error: "boom".to_owned(),
        }),
        deferred: Some(kiana_durable::harness::live::LiveDeferred {
            poll_at: 1_700_000_000_001.0,
        }),
    };
    assert_eq!(
        serde_json::to_string(&generation).expect("generation"),
        r#"{"attempt":2,"retry":{"at":1700000000000,"error":"boom"},"deferred":{"pollAt":1700000000001}}"#
    );
}

#[test]
fn event_usage_and_durations_serialize_like_javascript_numbers() {
    use kiana_ai::types::{Usage, UsageCost};
    use kiana_durable::harness::events::{AgentEvent, MessageChange};
    use kiana_durable::harness::types::UsageState;

    let usage = Usage {
        input: 3,
        output: 4,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 7,
        cost: UsageCost {
            input: 0.0,
            output: 0.0001,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0001,
            total_from_provider: None,
        },
        ..Default::default()
    };
    // JS 的 `JSON.stringify` 把 0 写成 `0`、120 写成 `120`；serde 的 f64 默认是 `0.0`/`120.0`。
    let message = kiana_ai::AssistantMessage {
        usage: usage.clone(),
        duration_ms: Some(120.0),
        timestamp: 1_700_000_000_000,
        ..Default::default()
    };
    let text = serde_json::to_string(&AgentEvent::MessageUpdate {
        usage: usage.clone(),
        changes: vec![MessageChange::Message { message }],
    })
    .expect("message_update");
    assert!(
        text.contains(
            r#""usage":{"input":3,"output":4,"cacheRead":0,"cacheWrite":0,"totalTokens":7,"cost":{"input":0,"output":0.0001,"cacheRead":0,"cacheWrite":0,"total":0.0001}}"#
        ),
        "{text}"
    );
    assert!(text.contains(r#""durationMs":120"#), "{text}");
    assert!(!text.contains(r#""durationMs":120.0"#), "{text}");

    let ledger = UsageState {
        models: indexmap::IndexMap::from([("faux/faux-1".to_owned(), usage)]),
        tools: indexmap::IndexMap::new(),
    };
    let text = serde_json::to_string(&AgentEvent::UsageChanged { usage: ledger }).expect("usage");
    assert!(
        text.contains(
            r#"{"models":{"faux/faux-1":{"input":3,"output":4,"cacheRead":0,"cacheWrite":0,"totalTokens":7,"cost":{"input":0,"output":0.0001,"cacheRead":0,"cacheWrite":0,"total":0.0001}}},"tools":{}}"#
        ),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conversation_keeps_its_harness_alive() {
    use kiana_durable::harness::harness::Harness;
    use kiana_durable::harness::types::{AgentChange, ModelRef, RootOptions};
    let setup = ChatSetup::new();
    let storage = memory();
    let context = context();
    let models: Arc<dyn kiana_ai::models::Models> = setup.models.clone();
    let settings = Arc::clone(&setup.settings);
    let registry: Arc<dyn kiana_durable::harness::types::RegistryReader> =
        Arc::clone(&setup.registry) as Arc<dyn kiana_durable::harness::types::RegistryReader>;
    let harness = Harness::open(
        storage,
        kiana_durable::harness::types::HarnessOptions {
            telemetry: None,
            models: kiana_durable::harness::types::ModelsProvider::Direct(models),
            identity: None,
            registry,
            settings: Some(kiana_durable::harness::types::SettingsProvider::Dynamic(
                Arc::new(move || Some(settings.lock().unwrap().clone())),
            )),
            env: None,
            conversation_created: None,
            now: None,
            set_timer: None,
            on_wake: None,
            on_report: None,
            read_only: None,
        },
        &*context,
    )
    .await
    .expect("open");
    let weak = Arc::downgrade(&harness);
    let root = harness
        .root(
            &*context,
            RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    })),
                    ..Default::default()
                }),
                init: None,
            },
        )
        .await
        .expect("root");
    drop(harness);
    // TS 的 ConversationImpl 强引用 Harness：句柄在手，Harness 就还活着。
    assert!(
        weak.upgrade().is_some(),
        "the conversation keeps the harness alive"
    );
    let agent = root.agent(&*context).await.expect("agent");
    assert!(agent.model.is_some());
    let harness = weak.upgrade().expect("harness");
    harness.close(&*context).await;
}

// ---------------------------------------------------------------------------
// translate(): 工具生命周期、批次边界、文档更新
// ---------------------------------------------------------------------------

use std::pin::Pin;
use std::time::Duration;

use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseInput, FauxResponseStep, FauxToolCallOptions,
    faux_assistant_message, faux_text, faux_thinking, faux_tool_call_with,
};
use kiana_ai::types::Tool;
use kiana_durable::harness::harness::HarnessImpl;
use kiana_durable::harness::types::{
    ToolExecuteFn, ToolExecutionResult, ToolOutput, ToolRegistration,
};

use harness_support::{add_tool, text_input};
use kiana_durable::types::{DocTarget, DocumentReader, JsonObject};
use task_support::Deferred;

async fn listen(
    harness: &Arc<HarnessImpl>,
    root: &Arc<dyn ConversationTrait>,
) -> (
    kiana_durable::harness::events::AgentEventStream,
    Arc<Recorder>,
) {
    let stream = kiana_durable::harness::events::watch_events(
        &**harness as &dyn kiana_durable::harness::types::Harness,
        root.id(),
        &*context(),
    )
    .await
    .expect("stream");
    let recorder = Arc::new(Recorder {
        batches: Mutex::new(Vec::new()),
    });
    stream.start(Arc::clone(&recorder) as Arc<dyn WatchListener<Vec<AgentEvent>>>);
    (stream, recorder)
}

/// 等待 `predicate` 成立，再确认 watch 投递队列已排空。
///
/// 固定 sleep 无法覆盖“提交已落定、但最终批次尚未发布/投递”的窗口；这里等到测试真正
/// 关心的终态事件出现，然后只等队列排空。
async fn drained_until(
    stream: &kiana_durable::harness::events::AgentEventStream,
    mut predicate: impl FnMut() -> bool,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if predicate() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "event condition was not reached within 10s"
        );
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    stream.wait_until_idle().await;
}

fn events_of(recorder: &Recorder) -> Vec<AgentEvent> {
    recorder
        .batches
        .lock()
        .unwrap()
        .iter()
        .flat_map(|batch| batch.iter().cloned())
        .collect()
}

fn batches_of(recorder: &Recorder) -> Vec<Vec<AgentEvent>> {
    recorder.batches.lock().unwrap().clone()
}

#[allow(dead_code)]
fn _keep_batches_helper_used() {
    let _ = batches_of;
}

/// 槽位工具：先输出 `one\n`，等待，再输出 `two\n` 并等 gate。
fn printing_tool(gate: Arc<Deferred>) -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(move |_arguments: JsonValue, api, _context| {
        let gate = Arc::clone(&gate);
        Box::pin(async move {
            api.output(ToolOutput::Text("one\n".to_owned()), None);
            tokio::time::sleep(Duration::from_millis(150)).await;
            api.output(ToolOutput::Text("two\n".to_owned()), None);
            let _ = gate.promise().await;
            Ok(ToolExecutionResult::default())
        })
            as Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    let mut registration = ToolRegistration::new(Tool {
        name: "print".to_owned(),
        description: "Prints".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
        constrained_sampling: None,
    });
    registration.execute = Some(execute);
    registration
}

fn output_text(events: &[AgentEvent]) -> String {
    let mut text = String::new();
    for event in events {
        let AgentEvent::ToolExecutionUpdate {
            output: Some(output),
            ..
        } = event
        else {
            continue;
        };
        match output {
            kiana_durable::harness::events::ToolOutputUpdate::Set { set } => text.clone_from(set),
            kiana_durable::harness::events::ToolOutputUpdate::Window { trim_start, append } => {
                let trimmed = trim_start
                    .map(|n| utf16_slice(&text, n))
                    .unwrap_or(text.clone());
                text = format!("{trimmed}{}", append.clone().unwrap_or_default());
            }
        }
    }
    text
}

/// 按 UTF-16 单元切掉开头 `count` 个（TS `String.prototype.slice`）。
fn utf16_slice(text: &str, count: usize) -> String {
    String::from_utf16_lossy(&text.encode_utf16().skip(count).collect::<Vec<u16>>())
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_tool_start_output_appends_and_the_result_entry() {
    let setup = ChatSetup::new();
    let gate = Arc::new(Deferred::new());
    add_tool(&setup.registry, printing_tool(Arc::clone(&gate)));
    setup.faux.set_responses(vec![
        faux_assistant_message(
            faux_tool_call_with(
                "print",
                serde_json::from_value(serde_json::json!({"n": 1})).expect("args"),
                FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                },
            ),
            FauxAssistantMessageOptions {
                stop_reason: Some(kiana_ai::types::StopReason::ToolUse),
                ..Default::default()
            },
        )
        .into(),
        faux_assistant_message(faux_text("done"), Default::default()).into(),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let (stream, recorder) = listen(&harness, &root).await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("submit");
    wait_for(
        {
            let recorder = Arc::clone(&recorder);
            move || {
                let recorder = Arc::clone(&recorder);
                async move { output_text(&events_of(&recorder)) == "one\ntwo\n" }
            }
        },
        Duration::from_secs(10),
    )
    .await
    .expect("both outputs are delivered");
    gate.resolve(());
    submission.wait(&*context()).await.expect("settles");
    drained_until(&stream, || {
        events_of(&recorder)
            .iter()
            .filter(|event| event_type(event) == "turn_end")
            .count()
            == 2
    })
    .await;

    let events = events_of(&recorder);
    let tool: Vec<AgentEvent> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::ToolExecutionStart { .. }
                    | AgentEvent::ToolExecutionUpdate { .. }
                    | AgentEvent::ToolExecutionEnd { .. }
            )
        })
        .cloned()
        .collect();
    match &tool[0] {
        AgentEvent::ToolExecutionStart { call, args } => {
            assert_eq!(call.tool_call_id, "c1");
            assert_eq!(call.tool_name, "print");
            assert!(call.task_id.is_some());
            assert_eq!(
                args.get("n").and_then(JsonValue::as_f64),
                Some(1.0),
                "{args:?}"
            );
        }
        other => panic!("unexpected first tool event: {other:?}"),
    }
    let AgentEvent::ToolExecutionEnd {
        call,
        entry,
        result,
    } = tool.last().expect("an end")
    else {
        panic!("the last tool event is an end");
    };
    assert_eq!(call.tool_call_id, "c1");
    assert_eq!(call.tool_name, "print");
    assert!(result.is_none());
    assert_eq!(
        entry.as_ref().map(|entry| entry.kind.as_str()),
        Some("kiana.tool-result")
    );
    // 工具结束紧邻其结果消息之前。
    let end_index = events
        .iter()
        .rposition(|event| matches!(event, AgentEvent::ToolExecutionEnd { .. }))
        .expect("the end is in the batch");
    let kinds: Vec<&str> = events[end_index..end_index + 3]
        .iter()
        .map(event_type)
        .collect();
    assert_eq!(
        kinds,
        vec!["tool_execution_end", "message_start", "message_end"]
    );
    match &events[end_index + 1] {
        AgentEvent::MessageStart {
            message: kiana_ai::types::Message::ToolResult(result),
        } => assert_eq!(result.tool_call_id, "c1"),
        other => panic!("unexpected message: {other:?}"),
    }
    let count = |wanted: &str| {
        events
            .iter()
            .filter(|event| event_type(event) == wanted)
            .count()
    };
    assert_eq!(count("turn_start"), 2);
    assert_eq!(count("turn_end"), 2);
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

fn event_type(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::Snapshot(_) => "snapshot",
        AgentEvent::RunStart { .. } => "run_start",
        AgentEvent::RunEnd { .. } => "run_end",
        AgentEvent::TurnStart => "turn_start",
        AgentEvent::TurnEnd => "turn_end",
        AgentEvent::MessageStart { .. } => "message_start",
        AgentEvent::MessageUpdate { .. } => "message_update",
        AgentEvent::MessageEnd { .. } => "message_end",
        AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
        AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
        AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
        AgentEvent::InboxUpdate { .. } => "inbox_update",
        AgentEvent::Submission { .. } => "submission",
        AgentEvent::AutoRetryStart { .. } => "auto_retry_start",
        AgentEvent::AutoRetryEnd { .. } => "auto_retry_end",
        AgentEvent::DeferredPoll { .. } => "deferred_poll",
        AgentEvent::EntryAppended { .. } => "entry_appended",
        AgentEvent::AgentChanged { .. } => "agent_changed",
        AgentEvent::UsageChanged { .. } => "usage_changed",
        AgentEvent::TaskFailed { .. } => "task_failed",
        AgentEvent::CompactionStart { .. } => "compaction_start",
        AgentEvent::CompactionEnd { .. } => "compaction_end",
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn emits_one_exact_batch_when_a_run_ends_and_a_queued_follow_up_starts_the_next() {
    let setup = ChatSetup::new();
    let release = Arc::new(Deferred::new());
    let held = {
        let release = Arc::clone(&release);
        FauxResponseStep::message_factory(move |_input: FauxResponseInput| {
            let release = Arc::clone(&release);
            async move {
                let _ = release.promise().await;
                faux_assistant_message(faux_text("first"), Default::default())
            }
        })
    };
    setup.faux.set_responses(vec![
        held,
        faux_assistant_message(faux_text("second"), Default::default()).into(),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let input = root
        .submit(text_input("a").into_draft(), &*context())
        .await
        .expect("input");
    let follow_up = root
        .submit(text_input("f").into_draft(), &*context())
        .await
        .expect("follow up");
    let (stream, recorder) = listen(&harness, &root).await;
    release.resolve(());
    follow_up.wait(&*context()).await.expect("settles");
    drained(&stream).await;

    let batches = batches_of(&recorder);
    let boundary = batches
        .iter()
        .find(|batch| {
            batch
                .iter()
                .any(|event| matches!(event, AgentEvent::RunEnd { .. }))
        })
        .expect("a run end batch");
    assert_eq!(
        boundary.iter().map(event_type).collect::<Vec<_>>(),
        vec![
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "turn_end",
            "run_end",
            "submission",
            "submission",
            "inbox_update",
            "usage_changed",
            "run_start",
            "turn_start",
        ],
        "{boundary:#?}"
    );
    let submissions: Vec<_> = boundary
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Submission { record } => Some(record.id),
            _ => None,
        })
        .collect();
    assert_eq!(submissions, vec![input.id(), follow_up.id()]);
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

/// 在提交里改写 `kiana.live`（TS 的 `root.commit(async tx => edit(await tx.doc(LiveDoc, id)))`）。
struct EditLive {
    conversation_id: ConversationId,
    edit: Box<dyn Fn(&mut JsonObject) + Send + Sync>,
}

#[async_trait]
impl SessionChange for EditLive {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let mut live = tx
            .doc(&DocTarget::conversation(
                &kiana_durable::harness::live::LIVE_DOC,
                self.conversation_id,
            ))
            .await?;
        (self.edit)(live.value_mut());
        Ok(Box::new(()))
    }
}

async fn edit_live(
    conversation: &Arc<dyn ConversationTrait>,
    edit: impl Fn(&mut JsonObject) + Send + Sync + 'static,
) {
    conversation
        .commit(
            &EditLive {
                conversation_id: conversation.id(),
                edit: Box::new(edit),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect("the live edit");
}

/// 在提交里退役一份内建文档。
struct RetireDoc {
    conversation_id: ConversationId,
    token: &'static kiana_durable::types::ConversationDocToken<JsonObject>,
}

#[async_trait]
impl SessionChange for RetireDoc {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        tx.retire_doc(&DocTarget::conversation(self.token, self.conversation_id))
            .await?;
        Ok(Box::new(()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_usage_only_updates_cleared_tool_progress_tools_ending_without_entries_and_deferred_polls()
 {
    use kiana_durable::harness::agent::AGENT_DOC;
    use kiana_durable::harness::usage::USAGE_DOC;

    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let (stream, recorder) = listen(&harness, &root).await;
    let partial = faux_assistant_message(faux_text("partial"), Default::default());
    let partial_json = serde_json::to_value(&partial).expect("partial");

    // 1. 第一个已提交 partial。
    {
        let partial_json = partial_json.clone();
        edit_live(&root, move |live| {
            live.insert(
                "generation".to_owned(),
                serde_json::json!({"attempt": 1, "message": partial_json}),
            );
        })
        .await;
    }
    // 2. 只改 usage：发送 usage 与空的 changes。
    edit_live(&root, |live| {
        if let Some(input) = live
            .get_mut("generation")
            .and_then(|generation| generation.get_mut("message"))
            .and_then(|message| message.get_mut("usage"))
            .and_then(|usage| usage.get_mut("input"))
        {
            *input = serde_json::json!(42);
        }
    })
    .await;
    // 3. 延迟轮询从无到有，再改时间点。
    edit_live(&root, |live| {
        live.insert(
            "generation".to_owned(),
            serde_json::json!({"attempt": 1, "deferred": {"pollAt": 1}}),
        );
    })
    .await;
    edit_live(&root, |live| {
        if let Some(poll_at) = live
            .get_mut("generation")
            .and_then(|generation| generation.get_mut("deferred"))
            .and_then(|deferred| deferred.get_mut("pollAt"))
        {
            *poll_at = serde_json::json!(2);
        }
    })
    .await;
    // 4. 一个 running 槽，然后安全重放清空它的进度。
    edit_live(&root, |live| {
        live.insert(
            "tools".to_owned(),
            serde_json::json!([{
                "callId": "c1",
                "name": "t",
                "status": "running",
                "details": {"n": 1},
                "diagnostics": []
            }]),
        );
    })
    .await;
    edit_live(&root, |live| {
        if let Some(slot) = live
            .get_mut("tools")
            .and_then(|tools| tools.as_array_mut())
            .and_then(|tools| tools.first_mut())
            .and_then(|slot| slot.as_object_mut())
        {
            slot.remove("details");
            slot.remove("diagnostics");
        }
    })
    .await;
    // 5. fault 把槽标记为 done，但没有结果条目。
    edit_live(&root, |live| {
        if let Some(status) = live
            .get_mut("tools")
            .and_then(|tools| tools.as_array_mut())
            .and_then(|tools| tools.first_mut())
            .and_then(|slot| slot.as_object_mut())
            .and_then(|slot| slot.get_mut("status"))
        {
            *status = serde_json::json!("done");
        }
    })
    .await;
    // 6. 退役的文档读作初始值。
    root.commit(
        &RetireDoc {
            conversation_id: root.id(),
            token: &USAGE_DOC,
        },
        &*context(),
        CommitOptions::default(),
    )
    .await
    .expect("retire usage");
    root.commit(
        &RetireDoc {
            conversation_id: root.id(),
            token: &AGENT_DOC,
        },
        &*context(),
        CommitOptions::default(),
    )
    .await
    .expect("retire agent");
    drained(&stream).await;

    let batches: Vec<Vec<AgentEvent>> = batches_of(&recorder)
        .into_iter()
        .map(|batch| {
            batch
                .into_iter()
                .filter(|event| !matches!(event, AgentEvent::TaskFailed { .. }))
                .collect()
        })
        .filter(|batch: &Vec<AgentEvent>| !batch.is_empty())
        .collect();
    assert_eq!(batches.len(), 9, "{batches:#?}");
    assert!(matches!(
        batches[0].as_slice(),
        [AgentEvent::MessageStart { message: kiana_ai::types::Message::Assistant(message) }] if message.content == partial.content
    ));
    match batches[1].as_slice() {
        [AgentEvent::MessageUpdate { usage, changes }] => {
            assert_eq!(usage.input, 42);
            assert!(changes.is_empty(), "{changes:?}");
        }
        other => panic!("unexpected second batch: {other:#?}"),
    }
    assert!(matches!(
        batches[2].as_slice(),
        [AgentEvent::DeferredPoll { poll_at: 1.0 }]
    ));
    assert!(matches!(
        batches[3].as_slice(),
        [AgentEvent::DeferredPoll { poll_at: 2.0 }]
    ));
    match batches[4].as_slice() {
        [AgentEvent::ToolExecutionStart { call, args }] => {
            assert_eq!(call.tool_call_id, "c1");
            assert_eq!(call.tool_name, "t");
            assert!(call.task_id.is_none());
            assert!(args.is_empty());
        }
        other => panic!("unexpected tool start: {other:#?}"),
    }
    match batches[5].as_slice() {
        [
            AgentEvent::ToolExecutionUpdate {
                call,
                details: Some(details),
                diagnostics: Some(diagnostics),
                ..
            },
        ] => {
            assert_eq!(call.tool_call_id, "c1");
            assert_eq!(details, &JsonValue::Null);
            assert!(diagnostics.is_empty());
        }
        other => panic!("unexpected tool update: {other:#?}"),
    }
    assert!(matches!(
        batches[6].as_slice(),
        [AgentEvent::ToolExecutionEnd { call, entry: None, .. }] if call.tool_call_id == "c1"
    ));
    match batches[7].as_slice() {
        [AgentEvent::UsageChanged { usage }] => {
            assert!(usage.models.is_empty());
            assert!(usage.tools.is_empty());
        }
        other => panic!("unexpected usage change: {other:#?}"),
    }
    match batches[8].as_slice() {
        [AgentEvent::AgentChanged { agent }] => {
            assert_eq!(
                serde_json::to_value(agent).expect("agent"),
                serde_json::json!({})
            );
        }
        other => panic!("unexpected agent change: {other:#?}"),
    }
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn applies_deltas_after_an_overflow_snapshot_that_holds_an_in_flight_partial() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let partial = faux_assistant_message(faux_text("hel"), Default::default());
    let stream = kiana_durable::harness::events::watch_events(
        &*harness as &dyn kiana_durable::harness::types::Harness,
        root.id(),
        &*context(),
    )
    .await
    .expect("stream");
    {
        let partial = serde_json::to_value(&partial).expect("partial");
        edit_live(&root, move |live| {
            live.insert(
                "generation".to_owned(),
                serde_json::json!({"attempt": 1, "message": partial}),
            );
        })
        .await;
    }
    for _ in 0..101 {
        let _ = append_note(&root, serde_json::json!(null)).await;
    }
    let recorder = Arc::new(Recorder {
        batches: Mutex::new(Vec::new()),
    });
    stream.start(Arc::clone(&recorder) as Arc<dyn WatchListener<Vec<AgentEvent>>>);
    drained(&stream).await;
    let batches = batches_of(&recorder);
    // 与 TS 一样：第一批判是溢出快照（之后可能还有别的批次）。
    let AgentEvent::Snapshot(snapshot) = &batches[0][0] else {
        panic!("the overflow batch is a snapshot: {:?}", batches[0]);
    };
    assert_eq!(
        snapshot.r#type,
        kiana_durable::harness::events::SnapshotEventType::Snapshot
    );
    let message = snapshot
        .generation
        .as_ref()
        .and_then(|generation| generation.message.as_ref())
        .expect("the in-flight partial");
    assert_eq!(
        text_of(Some(&kiana_ai::Message::Assistant(message.clone()))),
        Some("hel".to_owned())
    );

    // 快照之后到达的增量按普通批次翻译。
    edit_live(&root, |live| {
        if let Some(text) = live
            .get_mut("generation")
            .and_then(|generation| generation.get_mut("message"))
            .and_then(|message| message.get_mut("content"))
            .and_then(|content| content.as_array_mut())
            .and_then(|content| content.first_mut())
            .and_then(|block| block.as_object_mut())
            .and_then(|block| block.get_mut("text"))
        {
            *text = serde_json::json!("hello");
        }
    })
    .await;
    drained(&stream).await;
    let batches = batches_of(&recorder);
    let AgentEvent::MessageUpdate { changes, .. } = &batches.last().expect("a last batch")[0]
    else {
        panic!("unexpected last batch: {:?}", batches.last());
    };
    assert_eq!(changes.len(), 1);
    match &changes[0] {
        kiana_durable::harness::events::MessageChange::TextDelta {
            content_index,
            delta,
        } => {
            assert_eq!(*content_index, 0);
            assert_eq!(delta, "lo");
        }
        other => panic!("unexpected change: {other:?}"),
    }
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

fn hold_tool(gate: Arc<Deferred>) -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(move |_arguments: JsonValue, _api, _context| {
        let gate = Arc::clone(&gate);
        Box::pin(async move {
            let _ = gate.promise().await;
            Ok(ToolExecutionResult::default())
        })
            as Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    let mut registration = ToolRegistration::new(Tool {
        name: "hold".to_owned(),
        description: "Waits".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
        constrained_sampling: None,
    });
    registration.execute = Some(execute);
    registration
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_a_steer_without_run_events_a_reset_as_an_appended_entry_and_nothing_for_other_conversations()
 {
    use kiana_durable::harness::types::{ConversationCreateOptions, WhenBusy};
    use kiana_durable::types::ConversationOwnership;

    let setup = ChatSetup::new();
    let gate = Arc::new(Deferred::new());
    add_tool(&setup.registry, hold_tool(Arc::clone(&gate)));
    setup.faux.set_responses(vec![
        faux_assistant_message(
            faux_tool_call_with(
                "hold",
                serde_json::from_value(serde_json::json!({})).expect("args"),
                FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                },
            ),
            FauxAssistantMessageOptions {
                stop_reason: Some(kiana_ai::types::StopReason::ToolUse),
                ..Default::default()
            },
        )
        .into(),
        faux_assistant_message(faux_text("done"), Default::default()).into(),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let other = harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context(),
        )
        .await
        .expect("the other conversation");
    let (stream, recorder) = listen(&harness, &root).await;
    let input = root
        .submit(text_input("a").into_draft(), &*context())
        .await
        .expect("input");
    wait_for(
        {
            let recorder = Arc::clone(&recorder);
            move || {
                let recorder = Arc::clone(&recorder);
                async move {
                    events_of(&recorder)
                        .iter()
                        .any(|event| matches!(event, AgentEvent::ToolExecutionStart { .. }))
                }
            }
        },
        Duration::from_secs(10),
    )
    .await
    .expect("the tool starts");
    let mut steer = text_input("s");
    steer.when_busy = Some(WhenBusy::Steer);
    root.submit(steer.into_draft(), &*context())
        .await
        .expect("the steer");
    // Rust 的 watch 投递是异步的；先让它落地，再比较批次数量。
    drained(&stream).await;
    let before = batches_of(&recorder).len();
    // 别的会话的提交不产生事件。
    let _ = append_note(&other, serde_json::json!(null)).await;
    drained(&stream).await;
    assert_eq!(batches_of(&recorder).len(), before);
    gate.resolve(());
    input.wait(&*context()).await.expect("settles");
    root.reset(None, &*context()).await.expect("reset");
    drained(&stream).await;
    let events = events_of(&recorder);
    let count = |wanted: &str| {
        events
            .iter()
            .filter(|event| event_type(event) == wanted)
            .count()
    };
    assert_eq!(count("run_start"), 1);
    assert_eq!(count("run_end"), 1);
    let batches = batches_of(&recorder);
    let last: Vec<&str> = batches
        .last()
        .expect("a batch")
        .iter()
        .map(event_type)
        .collect();
    assert_eq!(last, vec!["entry_appended", "submission"]);
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_a_call_that_never_runs_and_a_tool_aborted_with_its_generation() {
    let setup = ChatSetup::new();
    let wait_execute: ToolExecuteFn = Arc::new(move |_arguments: JsonValue, _api, context| {
        let signal = context
            .abort_signal()
            .expect("the call has an abort signal");
        Box::pin(async move { Err(task_support::aborted(&signal).await) })
            as Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    let mut wait_tool = ToolRegistration::new(Tool {
        name: "wait".to_owned(),
        description: "Waits until aborted".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
        constrained_sampling: None,
    });
    wait_tool.execute = Some(wait_execute);
    add_tool(&setup.registry, wait_tool);
    let ghost = faux_tool_call_with(
        "ghost",
        serde_json::from_value(serde_json::json!({})).expect("args"),
        FauxToolCallOptions {
            id: Some("c1".to_owned()),
        },
    );
    let waiting = faux_tool_call_with(
        "wait",
        serde_json::from_value(serde_json::json!({})).expect("args"),
        FauxToolCallOptions {
            id: Some("c2".to_owned()),
        },
    );
    let mut message = faux_assistant_message(
        kiana_ai::providers::faux::FauxContent::Blocks(vec![
            kiana_ai::types::AssistantContentBlock::ToolCall(ghost),
            kiana_ai::types::AssistantContentBlock::ToolCall(waiting),
        ]),
        Default::default(),
    );
    message.stop_reason = kiana_ai::types::StopReason::ToolUse;
    setup.faux.set_responses(vec![message.into()]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let (stream, recorder) = listen(&harness, &root).await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("submit");
    wait_for(
        {
            let recorder = Arc::clone(&recorder);
            move || {
                let recorder = Arc::clone(&recorder);
                async move {
                    events_of(&recorder)
                        .iter()
                        .any(|event| matches!(event, AgentEvent::ToolExecutionStart { .. }))
                }
            }
        },
        Duration::from_secs(10),
    )
    .await
    .expect("a tool starts");
    let live = harness
        .snapshot(
            &DocTarget::conversation(&kiana_durable::harness::live::LIVE_DOC, root.id()),
            &*context(),
        )
        .await
        .expect("live")
        .expect("live exists");
    let task_id: kiana_durable::TaskId = serde_json::from_value(
        live.get("run")
            .and_then(|run| run.get("taskId"))
            .expect("the run task id")
            .clone(),
    )
    .expect("a task id");
    harness
        .abort_task(task_id, &*context())
        .await
        .expect("abort");
    submission.wait(&*context()).await.expect("settles");
    drained(&stream).await;

    let events = events_of(&recorder);
    let round: Vec<String> = events
        .iter()
        .map(|event| match event {
            AgentEvent::MessageEnd { entry } => format!("end:{}", entry.kind),
            AgentEvent::MessageStart { message } => format!("start:{}", message.role()),
            other => event_type(other).to_owned(),
        })
        .collect();
    let ghost_end = round
        .iter()
        .position(|label| label == "tool_execution_end")
        .expect("a tool end");
    assert_eq!(
        &round[ghost_end - 1..ghost_end + 3],
        [
            "end:kiana.assistant",
            "tool_execution_end",
            "start:toolResult",
            "end:kiana.tool-result",
        ]
    );
    let tool: Vec<(String, Option<String>, bool)> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolExecutionEnd { call, entry, .. } => Some((
                event_type(event).to_owned(),
                Some(call.tool_call_id.clone()),
                entry.is_some(),
            )),
            AgentEvent::ToolExecutionStart { call, .. } => Some((
                event_type(event).to_owned(),
                Some(call.tool_call_id.clone()),
                false,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        tool,
        vec![
            ("tool_execution_end".to_owned(), Some("c1".to_owned()), true),
            (
                "tool_execution_start".to_owned(),
                Some("c2".to_owned()),
                false
            ),
            ("tool_execution_end".to_owned(), Some("c2".to_owned()), true),
        ]
    );
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_queued_submissions_inbox_changes_and_retries() {
    use kiana_durable::harness::types::{HarnessSettings, RetrySettings};

    let setup = ChatSetup::new();
    let release = Arc::new(Deferred::new());
    let held = {
        let release = Arc::clone(&release);
        FauxResponseStep::message_factory(move |_input: FauxResponseInput| {
            let release = Arc::clone(&release);
            async move {
                let _ = release.promise().await;
                faux_assistant_message(faux_text("first"), Default::default())
            }
        })
    };
    let error = faux_assistant_message(
        kiana_ai::providers::faux::FauxContent::Blocks(Vec::new()),
        FauxAssistantMessageOptions {
            stop_reason: Some(kiana_ai::types::StopReason::Error),
            error_message: Some("503 Service Unavailable".to_owned()),
            ..Default::default()
        },
    );
    setup.faux.set_responses(vec![
        held,
        error.into(),
        faux_assistant_message(faux_text("second"), Default::default()).into(),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    *setup.settings.lock().unwrap() = HarnessSettings {
        retry: Some(RetrySettings {
            enabled: Some(true),
            max_retries: Some(1),
            base_delay_ms: Some(1.0),
            max_agent_delay_ms: None,
        }),
        ..Default::default()
    };
    let (stream, recorder) = listen(&harness, &root).await;
    let _input = root
        .submit(text_input("a").into_draft(), &*context())
        .await
        .expect("input");
    let follow_up = root
        .submit(text_input("f").into_draft(), &*context())
        .await
        .expect("follow up");
    drained_until(&stream, || {
        events_of(&recorder)
            .iter()
            .any(|event| matches!(event, AgentEvent::InboxUpdate { .. }))
    })
    .await;
    let inbox: Vec<Vec<(kiana_durable::types::SubmissionId, String)>> = events_of(&recorder)
        .iter()
        .filter_map(|event| match event {
            AgentEvent::InboxUpdate { items } => Some(
                items
                    .iter()
                    .map(|item| {
                        (
                            item.id,
                            match item.mode {
                                kiana_durable::harness::events::QueuedMode::Steer => "steer",
                                kiana_durable::harness::events::QueuedMode::FollowUp => "followUp",
                                kiana_durable::harness::events::QueuedMode::Write => "write",
                            }
                            .to_owned(),
                        )
                    })
                    .collect(),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(inbox, vec![vec![(follow_up.id(), "followUp".to_owned())]]);
    release.resolve(());
    follow_up.wait(&*context()).await.expect("settles");
    drained_until(&stream, || {
        events_of(&recorder)
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    AgentEvent::Submission { record }
                        if record.status == kiana_durable::types::SubmissionStatus::Done
                )
            })
            .count()
            >= 2
    })
    .await;
    let events = events_of(&recorder);
    let count = |wanted: &str| {
        events
            .iter()
            .filter(|event| event_type(event) == wanted)
            .count()
    };
    assert!(count("auto_retry_start") >= 1, "{events:#?}");
    assert!(count("auto_retry_end") >= 1);
    assert_eq!(count("run_start"), 2);
    let statuses: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Submission { record } => Some(match record.status {
                kiana_durable::types::SubmissionStatus::Queued => "queued",
                kiana_durable::types::SubmissionStatus::Placed => "placed",
                kiana_durable::types::SubmissionStatus::Done => "done",
                kiana_durable::types::SubmissionStatus::Unanswered => "unanswered",
            }),
            _ => None,
        })
        .collect();
    assert_eq!(statuses, vec!["placed", "queued", "done", "placed", "done"]);
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// 流式 partial：message_changes 重建每一条已提交的 partial
// ---------------------------------------------------------------------------

/// 每个已提交的 `kiana.live.generation.message`（TS: `partialsOf`），按提交顺序。
fn partials_of(
    harness: &Arc<HarnessImpl>,
) -> (
    Arc<Mutex<Vec<kiana_ai::types::AssistantMessage>>>,
    kiana_durable::types::Unsubscribe,
) {
    let partials = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&partials);
    let unsubscribe = harness
        .observe_commits(Arc::new(move |publication, _context| {
            for change in publication.changes() {
                if let kiana_durable::types::CommitChange::Document(
                    kiana_durable::types::DocumentCommitChange::Document {
                        record,
                        value: Some(value),
                        ..
                    },
                ) = change
                    && record.kind == "kiana.live"
                    && let Some(message) = value
                        .get("generation")
                        .and_then(|generation| generation.get("message"))
                    && let Ok(message) =
                        serde_json::from_value::<kiana_ai::types::AssistantMessage>(message.clone())
                {
                    sink.lock().unwrap().push(message);
                }
            }
        }))
        .expect("the commit observer");
    (partials, unsubscribe)
}

/// 把一个批次的 message changes 应用到消息副本上（TS: `applyChanges`）。
fn apply_changes(
    message: &kiana_ai::types::AssistantMessage,
    changes: &[kiana_durable::harness::events::MessageChange],
) -> kiana_ai::types::AssistantMessage {
    use kiana_ai::types::AssistantContentBlock;
    use kiana_durable::harness::events::MessageChange;
    let mut next = message.clone();
    for change in changes {
        match change {
            MessageChange::Message { message } => next = message.clone(),
            MessageChange::Block {
                content_index,
                block,
            } => {
                if let Some(slot) = next.content.get_mut(*content_index) {
                    *slot = block.clone();
                }
            }
            MessageChange::TextStart {
                content_index,
                block,
            }
            | MessageChange::ThinkingStart {
                content_index,
                block,
            }
            | MessageChange::ToolcallStart {
                content_index,
                block,
            } => next.content.insert(*content_index, block.clone()),
            MessageChange::TextDelta {
                content_index,
                delta,
            } => {
                if let Some(AssistantContentBlock::Text(text)) =
                    next.content.get_mut(*content_index)
                {
                    text.text.push_str(delta);
                }
            }
            MessageChange::ThinkingDelta {
                content_index,
                delta,
            } => {
                if let Some(AssistantContentBlock::Thinking(thinking)) =
                    next.content.get_mut(*content_index)
                {
                    thinking.thinking.push_str(delta);
                }
            }
            MessageChange::ToolcallDelta {
                content_index,
                path,
                delta,
            } => {
                let Some(AssistantContentBlock::ToolCall(call)) =
                    next.content.get_mut(*content_index)
                else {
                    continue;
                };
                let mut arguments = JsonValue::Object(call.arguments.clone().into_iter().collect());
                append_at(&mut arguments, path, delta);
                if let JsonValue::Object(map) = arguments {
                    call.arguments = map.into_iter().collect();
                }
            }
        }
    }
    next
}

/// TS `applyChanges` 的 `toolcall_delta`：在参数路径的末端字符串上追加。
fn append_at(
    value: &mut JsonValue,
    path: &[kiana_durable::harness::events::ChangePathSegment],
    delta: &str,
) {
    use kiana_durable::harness::events::ChangePathSegment;
    let Some((first, rest)) = path.split_first() else {
        return;
    };
    match first {
        ChangePathSegment::Key(key) => {
            let JsonValue::Object(map) = value else {
                return;
            };
            if rest.is_empty() {
                if let Some(slot) = map.get_mut(key) {
                    let current = slot.as_str().unwrap_or("").to_owned();
                    *slot = JsonValue::String(format!("{current}{delta}"));
                }
            } else if let Some(next) = map.get_mut(key) {
                append_at(next, rest, delta);
            }
        }
        ChangePathSegment::Index(index) => {
            if let JsonValue::Array(items) = value
                && let Some(item) = items.get_mut(*index as usize)
            {
                append_at(item, rest, delta);
            }
        }
    }
}

fn content_of(
    message: &kiana_ai::types::AssistantMessage,
) -> Vec<kiana_ai::types::AssistantContentBlock> {
    message.content.clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_a_run_as_lifecycle_events_and_text_deltas_that_rebuild_the_answer() {
    use kiana_ai::providers::faux::{FauxTokenSize, RegisterFauxProviderOptions};

    let setup = ChatSetup::with_faux_options(RegisterFauxProviderOptions {
        tokens_per_second: Some(400.0),
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    });
    let text = "streamed answer text ".repeat(20);
    setup.faux.set_responses(vec![
        faux_assistant_message(faux_text(text.clone()), Default::default()).into(),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let (partials, _unsubscribe) = partials_of(&harness);
    let (stream, recorder) = listen(&harness, &root).await;
    assert_eq!(
        stream.snapshot.r#type,
        kiana_durable::harness::events::SnapshotEventType::Snapshot
    );
    assert!(stream.snapshot.entries.is_empty());
    assert!(stream.snapshot.tools.is_empty());
    assert!(stream.snapshot.inbox.is_empty());
    let submission = root
        .submit(text_input("hi").into_draft(), &*context())
        .await
        .expect("submit");
    submission.wait(&*context()).await.expect("settles");
    drained_until(&stream, || {
        events_of(&recorder)
            .iter()
            .any(|event| event_type(event) == "usage_changed")
    })
    .await;

    let events = events_of(&recorder);
    let mut types: Vec<&str> = Vec::new();
    for event in &events {
        let kind = event_type(event);
        if types.last() != Some(&kind) {
            types.push(kind);
        }
    }
    assert_eq!(
        types,
        vec![
            "message_start",
            "message_end",
            "submission",
            "run_start",
            "turn_start",
            "message_start",
            "message_update",
            "message_end",
            "turn_end",
            "run_end",
            "submission",
            "usage_changed",
        ]
    );
    let run_start = events
        .iter()
        .find(|event| matches!(event, AgentEvent::RunStart { .. }))
        .expect("a run start");
    match run_start {
        AgentEvent::RunStart { inputs } => assert_eq!(inputs, &vec![submission.id()]),
        _ => unreachable!(),
    }
    // 每个事件之后重建的文本等于该提交已提交的 partial。
    let partial_texts: Vec<Option<String>> = partials
        .lock()
        .unwrap()
        .iter()
        .map(|message| text_of(Some(&kiana_ai::Message::Assistant(message.clone()))))
        .collect();
    let mut rebuilt: Vec<String> = Vec::new();
    let mut current = String::new();
    for event in &events {
        match event {
            AgentEvent::MessageStart {
                message: kiana_ai::types::Message::Assistant(message),
            } => {
                current = text_of(Some(&kiana_ai::Message::Assistant(message.clone())))
                    .unwrap_or_default();
                rebuilt.push(current.clone());
            }
            AgentEvent::MessageUpdate { changes, .. } => {
                // 文本流：把 text_start / text_delta 应用到当前文本。
                let mut text = current.clone();
                for change in changes {
                    match change {
                        kiana_durable::harness::events::MessageChange::TextStart {
                            content_index: 0,
                            block: kiana_ai::types::AssistantContentBlock::Text(block),
                        } => text.clone_from(&block.text),
                        kiana_durable::harness::events::MessageChange::TextDelta {
                            content_index: 0,
                            delta,
                        } => text.push_str(delta),
                        _ => {}
                    }
                }
                current = text;
                rebuilt.push(current.clone());
            }
            _ => {}
        }
    }
    let partial_texts: Vec<String> = partial_texts
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect();
    assert_eq!(rebuilt, partial_texts);
    assert!(partial_texts.len() > 1, "{partial_texts:?}");
    let end = events
        .iter()
        .rev()
        .find_map(|event| match event {
            AgentEvent::MessageEnd { entry } => Some(entry),
            _ => None,
        })
        .expect("a message end");
    let first = end.model.as_ref().and_then(|model| model.first());
    assert_eq!(text_of(first), Some(text));
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuilds_every_committed_partial_of_thinking_text_and_tool_call_arguments_from_message_changes()
 {
    use kiana_ai::providers::faux::{FauxContent, FauxTokenSize, RegisterFauxProviderOptions};
    use kiana_ai::types::AssistantContentBlock;

    let setup = ChatSetup::with_faux_options(RegisterFauxProviderOptions {
        tokens_per_second: Some(150.0),
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    });
    let arguments: JsonObject = serde_json::from_value(serde_json::json!({
        "path": "a/long/path/".repeat(10),
        "note": "x".repeat(60)
    }))
    .expect("arguments");
    let mut message = faux_assistant_message(
        FauxContent::Blocks(vec![
            AssistantContentBlock::Thinking(faux_thinking("thinking about it ".repeat(10))),
            AssistantContentBlock::Text(faux_text("some text ".repeat(10))),
            AssistantContentBlock::ToolCall(faux_tool_call_with(
                "missing",
                arguments,
                FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                },
            )),
        ]),
        Default::default(),
    );
    message.stop_reason = kiana_ai::types::StopReason::ToolUse;
    setup.faux.set_responses(vec![
        message.into(),
        faux_assistant_message(faux_text("done"), Default::default()).into(),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let (partials, _unsubscribe) = partials_of(&harness);
    let (stream, recorder) = listen(&harness, &root).await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("submit");
    submission.wait(&*context()).await.expect("settles");
    drained(&stream).await;

    let events = events_of(&recorder);
    let mut rebuilt: Vec<kiana_ai::types::AssistantMessage> = Vec::new();
    let mut current: Option<kiana_ai::types::AssistantMessage> = None;
    for event in &events {
        match event {
            AgentEvent::MessageEnd { entry } if entry.kind == "kiana.assistant" => break,
            AgentEvent::MessageStart {
                message: kiana_ai::types::Message::Assistant(message),
            } => current = Some(message.clone()),
            AgentEvent::MessageUpdate { changes, .. } => {
                current = Some(apply_changes(current.as_ref().expect("a message"), changes));
            }
            _ => continue,
        }
        rebuilt.push(current.clone().expect("a current message"));
    }
    let partials = partials.lock().unwrap().clone();
    assert!(partials.len() > 2, "{}", partials.len());
    let rebuilt_content: Vec<_> = rebuilt.iter().map(content_of).collect();
    let partial_content: Vec<_> = partials.iter().map(content_of).collect();
    assert_eq!(rebuilt_content, partial_content);
    let change_types: std::collections::HashSet<String> = events
        .iter()
        .flat_map(|event| match event {
            AgentEvent::MessageUpdate { changes, .. } => changes
                .iter()
                .map(|change| match change {
                    kiana_durable::harness::events::MessageChange::ThinkingDelta { .. } => {
                        "thinking_delta".to_owned()
                    }
                    kiana_durable::harness::events::MessageChange::TextDelta { .. } => {
                        "text_delta".to_owned()
                    }
                    kiana_durable::harness::events::MessageChange::ToolcallDelta { .. } => {
                        "toolcall_delta".to_owned()
                    }
                    other => format!("{other:?}"),
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    assert!(
        change_types.contains("thinking_delta") || change_types.contains("text_delta"),
        "{change_types:?}"
    );
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuilds_a_sliding_tail_window_from_output_trims_and_appends() {
    use kiana_durable::harness::types::{OutputLimits, OutputRetain};

    let setup = ChatSetup::new();
    let gate = Arc::new(Deferred::new());
    let execute: ToolExecuteFn = {
        let gate = Arc::clone(&gate);
        Arc::new(move |_arguments: JsonValue, api, _context| {
            let gate = Arc::clone(&gate);
            Box::pin(async move {
                for line in 0..6 {
                    api.output(ToolOutput::Text(format!("line {line}\n")), None);
                    tokio::time::sleep(Duration::from_millis(120)).await;
                }
                let _ = gate.promise().await;
                Ok(ToolExecutionResult::default())
            })
                as Pin<
                    Box<
                        dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>>
                            + Send,
                    >,
                >
        })
    };
    let mut tail = ToolRegistration::new(Tool {
        name: "tail".to_owned(),
        description: "Prints lines".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
        constrained_sampling: None,
    });
    tail.execute = Some(execute);
    tail.output_limits = Some(OutputLimits {
        max_bytes: None,
        max_lines: Some(3),
        retain: Some(OutputRetain::Tail),
    });
    add_tool(&setup.registry, tail);
    setup.faux.set_responses(vec![
        faux_assistant_message(
            faux_tool_call_with(
                "tail",
                JsonObject::new(),
                FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                },
            ),
            FauxAssistantMessageOptions {
                stop_reason: Some(kiana_ai::types::StopReason::ToolUse),
                ..Default::default()
            },
        )
        .into(),
        faux_assistant_message(faux_text("done"), Default::default()).into(),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let outputs = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&outputs);
    let _unsubscribe = harness
        .observe_commits(Arc::new(move |publication, _context| {
            for change in publication.changes() {
                if let kiana_durable::types::CommitChange::Document(
                    kiana_durable::types::DocumentCommitChange::Document {
                        record,
                        value: Some(value),
                        ..
                    },
                ) = change
                    && record.kind == "kiana.live"
                    && let Some(output) = value
                        .get("tools")
                        .and_then(|tools| tools.as_array())
                        .and_then(|tools| tools.first())
                        .and_then(|slot| slot.get("output"))
                        .and_then(JsonValue::as_str)
                {
                    let mut outputs = sink.lock().unwrap();
                    if outputs.last().map(String::as_str) != Some(output) {
                        outputs.push(output.to_owned());
                    }
                }
            }
        }))
        .expect("the commit observer");
    let (stream, recorder) = listen(&harness, &root).await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("submit");
    wait_for(
        {
            let outputs = Arc::clone(&outputs);
            move || {
                let outputs = Arc::clone(&outputs);
                async move {
                    outputs
                        .lock()
                        .unwrap()
                        .last()
                        .is_some_and(|output| output.ends_with("line 5\n"))
                }
            }
        },
        Duration::from_secs(10),
    )
    .await
    .expect("the tail reaches line 5");
    gate.resolve(());
    submission.wait(&*context()).await.expect("settles");
    drained(&stream).await;

    let events = events_of(&recorder);
    let mut rebuilt: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut saw_trim = false;
    for event in &events {
        let AgentEvent::ToolExecutionUpdate {
            output: Some(output),
            ..
        } = event
        else {
            continue;
        };
        match output {
            kiana_durable::harness::events::ToolOutputUpdate::Set { set } => text.clone_from(set),
            kiana_durable::harness::events::ToolOutputUpdate::Window { trim_start, append } => {
                if trim_start.is_some() {
                    saw_trim = true;
                }
                let trimmed = trim_start
                    .map(|count| utf16_slice(&text, count))
                    .unwrap_or_else(|| text.clone());
                text = format!("{trimmed}{}", append.clone().unwrap_or_default());
            }
        }
        rebuilt.push(text.clone());
    }
    assert_eq!(rebuilt, *outputs.lock().unwrap());
    assert!(saw_trim, "a window update trims the head");
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn starts_a_message_at_the_first_committed_partial_and_ends_it_with_the_converted_entry_on_abort()
 {
    use kiana_ai::providers::faux::{FauxTokenSize, RegisterFauxProviderOptions};
    use kiana_durable::harness::live::{LIVE_DOC, LiveState};

    let setup = ChatSetup::with_faux_options(RegisterFauxProviderOptions {
        tokens_per_second: Some(100.0),
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    });
    setup.faux.set_responses(vec![
        faux_assistant_message(faux_text("x".repeat(400)), Default::default()).into(),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let (stream, recorder) = listen(&harness, &root).await;
    let submission = root
        .submit(text_input("hi").into_draft(), &*context())
        .await
        .expect("submit");
    let recorder_for_wait = Arc::clone(&recorder);
    chat_support::wait_for(
        || {
            let recorder = Arc::clone(&recorder_for_wait);
            async move {
                events_of(&recorder).iter().any(|event| {
                    matches!(
                        event,
                        AgentEvent::MessageStart {
                            message: kiana_ai::types::Message::Assistant(_)
                        }
                    )
                })
            }
        },
        Duration::from_secs(5),
    )
    .await
    .expect("the assistant message starts at a committed partial");
    let document = harness
        .snapshot(&DocTarget::conversation(&LIVE_DOC, root.id()), &*context())
        .await
        .expect("snapshot")
        .expect("the live document exists");
    let live = LiveState::from_document(&document).expect("a live state");
    let task_id = live.run.expect("a running generation").task_id;
    harness
        .abort_task(task_id, &*context())
        .await
        .expect("abort the generation");
    submission
        .wait(&*context())
        .await
        .expect("the submission settles");
    drained_until(&stream, || {
        events_of(&recorder)
            .iter()
            .any(|event| matches!(event, AgentEvent::MessageEnd { entry } if entry.kind == "kiana.assistant"))
    })
    .await;

    let events = events_of(&recorder);
    let assistant_ends = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::MessageEnd { entry } if entry.kind == "kiana.assistant"
            )
        })
        .count();
    assert_eq!(assistant_ends, 1, "the aborted message ends once");
    let assistant_starts = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::MessageStart {
                    message: kiana_ai::types::Message::Assistant(_)
                }
            )
        })
        .count();
    assert_eq!(assistant_starts, 1, "the message starts once");
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_a_tool_call_partial_as_toolcall_delta_appends_at_its_argument_paths() {
    use kiana_ai::providers::faux::{FauxContent, FauxTokenSize, RegisterFauxProviderOptions};
    use kiana_ai::types::{AssistantContentBlock, StopReason};

    let setup = ChatSetup::with_faux_options(RegisterFauxProviderOptions {
        tokens_per_second: Some(500.0),
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    });
    setup.settings.lock().unwrap().progress =
        Some(kiana_durable::harness::types::ProgressSettings {
            partial_interval_ms: Some(20.0),
            output_interval_ms: Some(100.0),
            ..Default::default()
        });
    let arguments: JsonObject = serde_json::from_value(serde_json::json!({
        "path": "src/file.ts",
        "content": "const a = 1;\n".repeat(150),
    }))
    .expect("arguments");
    let mut message = faux_assistant_message(
        FauxContent::Blocks(vec![AssistantContentBlock::ToolCall(faux_tool_call_with(
            "missing",
            arguments,
            FauxToolCallOptions {
                id: Some("c1".to_owned()),
            },
        ))]),
        Default::default(),
    );
    message.stop_reason = StopReason::ToolUse;
    setup.faux.set_responses(vec![
        message.into(),
        faux_assistant_message(faux_text("done"), Default::default()).into(),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let (partials, _unsubscribe) = partials_of(&harness);
    let (stream, recorder) = listen(&harness, &root).await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("submit");
    submission.wait(&*context()).await.expect("settles");
    drained(&stream).await;

    let events = events_of(&recorder);
    let changes: Vec<&kiana_durable::harness::events::MessageChange> = events
        .iter()
        .flat_map(|event| match event {
            AgentEvent::MessageUpdate { changes, .. } => changes.iter().collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    let deltas: Vec<&kiana_durable::harness::events::MessageChange> = changes
        .iter()
        .copied()
        .filter(|change| {
            matches!(
                change,
                kiana_durable::harness::events::MessageChange::ToolcallDelta { .. }
            )
        })
        .collect();
    assert!(deltas.len() > 10, "{deltas:?}");
    for delta in &deltas {
        let kiana_durable::harness::events::MessageChange::ToolcallDelta { path, .. } = delta
        else {
            unreachable!()
        };
        assert!(
            path.iter().all(|segment| matches!(
                segment,
                kiana_durable::harness::events::ChangePathSegment::Key(key)
                    if key == "path" || key == "content"
            )),
            "{path:?}"
        );
    }
    // 整个调用只在成员出现时重发（每个参数至多一次）；增长中的字符串是追加。
    assert!(
        changes.iter().all(|change| !matches!(
            change,
            kiana_durable::harness::events::MessageChange::Message { .. }
        )),
        "no whole-message replacement"
    );
    let blocks = changes
        .iter()
        .filter(|change| {
            matches!(
                change,
                kiana_durable::harness::events::MessageChange::Block { .. }
            )
        })
        .count();
    assert!(blocks <= 2, "{blocks} whole-block replacements");
    // 已提交的 partial 只带解析后的参数，从不带原始参数文本。
    for partial in partials.lock().unwrap().iter() {
        for block in &partial.content {
            let value = serde_json::to_value(block).expect("serialize the block");
            let mut keys: Vec<&str> = value
                .as_object()
                .expect("a block object")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, vec!["arguments", "id", "name", "type"]);
        }
    }
    let _ = stream.stop().await;
    harness.close(&*context()).await;
}
