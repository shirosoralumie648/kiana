//! 完整 `Harness` 驱动的端到端用例（验收补齐）。
//!
//! 本文件补上各单元在自己阶段因上层 `harness.rs` / `scheduler.rs` 还是骨架而留在
//! "阶段级子集"里的核心流程，逐条对应 TS：
//! - `test/harness-tools.test.ts` 的 tool round / tool results；
//! - `test/harness-nested-tools.test.ts` 与 `harness-nested-tools-restart.test.ts` 的嵌套调用；
//! - `test/harness-compaction.test.ts` 的端到端压缩；
//! - `test/harness-generation.test.ts` 的重试与延迟轮询；
//! - `test/harness-ownership.test.ts` 的所有权级联。
//!
//! 每个用例标题沿用 TS 的英文原文，便于与验收清单对照。

#![allow(clippy::await_holding_lock)]

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;
#[path = "support/task.rs"]
mod task_support;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, FauxToolCallOptions, faux_assistant_message,
    faux_text, faux_tool_call_with,
};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, StopReason, TextContent,
    TextKind, Tool, ToolResultMessage,
};
use kiana_chord::context::Context;
use kiana_durable::Session;
use kiana_durable::harness::types::{
    Conversation as ConversationTrait, InputSubmissionDraft, SettledSubmissionRecord,
    ToolExecutionApi, ToolExecutionResult, ToolRegistration,
};
use kiana_durable::types::{
    BoxError, DocumentReader, EntryRecord, JsonObject, JsonValue, Storage, SubmissionStatus,
};

use chat_support::{ChatSetup, context, open_chat};
use support::MemoryStorage;

// ---------------------------------------------------------------------------
// 夹具（TS harness-tools.test.ts 顶部）
// ---------------------------------------------------------------------------

/// `tool(name, execute)`：描述 `The <name> tool`，空参数 schema。
fn tool(name: &str, execute: ExecuteFn) -> ToolRegistration {
    ToolRegistration {
        tool: Tool {
            name: name.to_owned(),
            description: format!("The {name} tool"),
            parameters: serde_json::json!({"type": "object"}),
            constrained_sampling: None,
        },
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: None,
        callers: None,
        deferred: false,
        output_limits: None,
        execute: Some(Arc::new(move |args: JsonValue, api, context| {
            let result = execute(args, api, context);
            Box::pin(async move { result })
                as Pin<Box<dyn Future<Output = Result<ToolExecutionResult, BoxError>> + Send>>
        })),
    }
}

type ExecuteFn = Arc<
    dyn Fn(
            JsonValue,
            Arc<dyn ToolExecutionApi>,
            Arc<dyn Context>,
        ) -> Result<ToolExecutionResult, BoxError>
        + Send
        + Sync,
>;

/// 一段文本输出。
fn text_output(text: &str) -> ToolExecutionResult {
    ToolExecutionResult {
        output: Some(vec![kiana_ai::types::UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_owned(),
            text_signature: None,
        })]),
        ..Default::default()
    }
}

/// `calls(...)`：一次带工具调用的回答，每个 `[name, args, id]` 一个调用。
fn calls(list: &[(&str, JsonObject, &str)]) -> FauxResponseStep {
    let mut content = Vec::new();
    for (name, arguments, id) in list {
        content.push(AssistantContentBlock::ToolCall(faux_tool_call_with(
            *name,
            arguments.clone(),
            FauxToolCallOptions {
                id: Some((*id).to_owned()),
            },
        )));
    }
    FauxResponseStep::Message(AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        usage: Default::default(),
        stop_reason: StopReason::ToolUse,
        timestamp: 2,
        ..Default::default()
    })
}

fn done() -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message(
        faux_text("done"),
        FauxAssistantMessageOptions::default(),
    ))
}

fn empty_args() -> JsonObject {
    JsonObject::new()
}

/// `results(entries)`：工具结果消息，按条目顺序。
fn results(entries: &[EntryRecord]) -> Vec<ToolResultMessage> {
    entries
        .iter()
        .filter(|entry| entry.kind == "kiana.tool-result")
        .filter_map(|entry| entry.model.as_ref())
        .filter_map(|model| model.first())
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result.clone()),
            _ => None,
        })
        .collect()
}

/// `resultText(message)`：文本块用 `|` 连接。
fn result_text(message: &ToolResultMessage) -> String {
    message
        .content
        .iter()
        .map(|item| match item {
            kiana_ai::types::UserContentBlock::Text(text) => text.text.clone(),
            other => format!("[{other:?}]"),
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// 提交一段输入并等它落定。
async fn submit_and_wait(root: &Arc<dyn ConversationTrait>, text: &str) -> SettledSubmissionRecord {
    let draft: InputSubmissionDraft = harness_support::text_input(text);
    root.submit(draft.into_draft(), &*context())
        .await
        .expect("submit")
        .wait(&*context())
        .await
        .expect("settled")
}

// ---------------------------------------------------------------------------
// harness-tools.test.ts: tool round
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn runs_input_tool_call_tool_result_and_answer_and_settles_the_input() {
    let setup = ChatSetup::new();
    harness_support::add_tool(
        &setup.registry,
        tool(
            "echo",
            Arc::new(|args: JsonValue, _api, _context| {
                let text = args
                    .get("text")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_owned();
                Ok(text_output(&format!("echo {text}")))
            }),
        ),
    );
    let mut args = JsonObject::new();
    args.insert("text".to_owned(), JsonValue::String("hi".to_owned()));
    setup
        .faux
        .set_responses(vec![calls(&[("echo", args, "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    let entries = chat_support::all_entries(&root, &*context()).await;
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "kiana.user",
            "kiana.system",
            "kiana.assistant",
            "kiana.tool-result",
            "kiana.assistant"
        ]
    );
    let system = match entries[1].model.as_ref().and_then(|model| model.first()) {
        Some(Message::System(system)) => system.clone(),
        other => panic!("entry 1 is the system entry: {other:?}"),
    };
    let tools_added = system.tools_added.clone().unwrap_or_default();
    assert_eq!(tools_added.len(), 1);
    assert_eq!(tools_added[0].name, "echo");
    assert_eq!(tools_added[0].description, "The echo tool");
    let results = results(&entries);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id, "c1");
    assert_eq!(results[0].tool_name, "echo");
    assert!(!results[0].is_error);
    assert_eq!(result_text(&results[0]), "echo hi");
    assert_eq!(
        entries[3].data.clone().unwrap_or(JsonValue::Null),
        serde_json::json!({"diagnostics": []})
    );
    assert!(entries[3].by_task_id.is_some());
    // 第二次请求在它的调用之后立刻看到工具结果。
    assert_eq!(setup.faux.state().call_count(), 2);
    let live = harness
        .snapshot(
            &kiana_durable::types::DocTarget::conversation(
                &kiana_durable::harness::live::LIVE_DOC,
                root.id(),
            ),
            &*context(),
        )
        .await
        .expect("live snapshot")
        .unwrap_or_default();
    assert_eq!(live, JsonObject::new());
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_calls_to_tools_the_request_did_not_offer_without_a_task() {
    let setup = ChatSetup::new();
    harness_support::add_tool(
        &setup.registry,
        tool(
            "echo",
            Arc::new(|_args, _api, _context| Ok(ToolExecutionResult::default())),
        ),
    );
    setup.faux.set_responses(vec![
        calls(&[("ghost", empty_args(), "c1"), ("echo", empty_args(), "c2")]),
        done(),
    ]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    let entries = chat_support::all_entries(&root, &*context()).await;
    let results = results(&entries);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].tool_call_id, "c1");
    assert!(results[0].is_error);
    assert_eq!(
        result_text(&results[0]),
        "<harness>\n[error] Tool ghost is not available\n</harness>"
    );
    assert_eq!(results[1].tool_call_id, "c2");
    assert!(!results[1].is_error);
    let ghost_entry = entries
        .iter()
        .find(|entry| {
            entry.kind == "kiana.tool-result"
                && entry
                    .model
                    .as_ref()
                    .and_then(|model| model.first())
                    .is_some_and(|message| {
                        matches!(
                            message,
                            Message::ToolResult(result) if result.tool_call_id == "c1"
                        )
                    })
        })
        .expect("the ghost result entry");
    let data = ghost_entry.data.clone().unwrap_or(JsonValue::Null);
    assert_eq!(
        data.get("diagnostics").cloned().unwrap_or(JsonValue::Null),
        serde_json::json!([{
            "severity": "error",
            "code": "tool_unavailable",
            "message": "Tool ghost is not available",
        }])
    );
    // 只有真实工具留下任务。
    let tasks = harness
        .commit(
            &ScanTasksChange {
                conversation_id: Some(root.id()),
                kind: None,
                limit: 20,
            },
            &*context(),
            Default::default(),
        )
        .await
        .expect("tasks")
        .downcast::<Vec<kiana_durable::types::TaskRecord>>()
        .map(|tasks| *tasks)
        .expect("task records");
    assert_eq!(
        tasks
            .iter()
            .filter(|task| task.kind == "kiana.tool")
            .count(),
        1
    );
    harness.close(&*context()).await;
}

/// `tx.scanTasks(query, limit)` 的最小 `SessionChange`。
struct ScanTasksChange {
    conversation_id: Option<kiana_durable::types::ConversationId>,
    kind: Option<String>,
    limit: usize,
}

#[async_trait]
impl kiana_durable::types::SessionChange for ScanTasksChange {
    async fn run(
        &self,
        tx: &dyn kiana_durable::types::Tx,
    ) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
        let page = tx
            .scan_tasks(
                &kiana_durable::types::TaskQuery {
                    conversation_id: self.conversation_id,
                    kind: self.kind.clone(),
                    ..Default::default()
                },
                self.limit,
                None,
            )
            .await?;
        Ok(Box::new(page.items))
    }
}

// ---------------------------------------------------------------------------
// harness-nested-tools.test.ts
// ---------------------------------------------------------------------------

fn object(pairs: &[(&str, JsonValue)]) -> JsonObject {
    let mut object = JsonObject::new();
    for (key, value) in pairs {
        object.insert((*key).to_owned(), value.clone());
    }
    object
}

/// `tx.scanTasks({ kind: "kiana.tool" }, 100)` 的任务记录。
async fn tool_tasks(
    harness: &kiana_durable::harness::harness::HarnessImpl,
) -> Vec<kiana_durable::types::TaskRecord> {
    let change = ScanTasksChange {
        conversation_id: None,
        kind: Some("kiana.tool".to_owned()),
        limit: 100,
    };
    harness
        .commit(&change, &*context(), Default::default())
        .await
        .expect("tasks")
        .downcast::<Vec<kiana_durable::types::TaskRecord>>()
        .map(|tasks| *tasks)
        .expect("task records")
}

/// 一段 assistant 工具调用回答；`calls` 之外 `id` 缺省。
fn nested_batch_tool(
    execute: impl Fn(
        JsonValue,
        Arc<dyn ToolExecutionApi>,
        Arc<dyn Context>,
    ) -> Pin<Box<dyn Future<Output = Result<ToolExecutionResult, BoxError>> + Send>>
    + Send
    + Sync
    + 'static,
) -> ToolRegistration {
    ToolRegistration {
        tool: Tool {
            name: "batch".to_owned(),
            description: "batch".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
            constrained_sampling: None,
        },
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: None,
        callers: None,
        deferred: false,
        output_limits: None,
        execute: Some(Arc::new(execute)),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_a_nested_call_as_its_own_tool_task_and_returns_its_result_to_the_caller_not_the_transcript()
 {
    let setup = ChatSetup::new();
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let runs_for_echo = Arc::clone(&runs);
    harness_support::add_tool(
        &setup.registry,
        tool(
            "echo",
            Arc::new(move |args: JsonValue, _api, _context| {
                runs_for_echo.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let text = args
                    .get("text")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_owned();
                Ok(text_output(&format!("echo {text}")))
            }),
        ),
    );
    harness_support::add_tool(
        &setup.registry,
        nested_batch_tool(|_args, api, call_context| {
            Box::pin(async move {
                let nested = Arc::clone(&api)
                    .execute_tool(
                        "echo",
                        object(&[("text", JsonValue::String("a".to_owned()))]),
                        &*call_context,
                        Default::default(),
                    )
                    .await?;
                let text = nested
                    .structured_output
                    .as_ref()
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_owned();
                Ok(text_output(&format!("got {text}")))
            })
        }),
    );
    setup
        .faux
        .set_responses(vec![calls(&[("batch", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    let entries = chat_support::all_entries(&root, &*context()).await;
    let results = results(&entries);
    // 只有模型发出的调用留下结果条目，其中不记录嵌套调用。
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id, "c1");
    assert_eq!(result_text(&results[0]), "got echo a");
    assert!(results[0].nested_calls.is_none());
    let tasks = tool_tasks(&harness).await;
    assert_eq!(tasks.len(), 2);
    let parent = tasks
        .iter()
        .find(|task| task.input.get("kind").and_then(JsonValue::as_str) == Some("model"))
        .expect("the model-issued task");
    let child = tasks
        .iter()
        .find(|task| task.input.get("kind").and_then(JsonValue::as_str) == Some("nested"))
        .expect("the nested task");
    assert_eq!(
        parent.input.get("kind").and_then(JsonValue::as_str),
        Some("model")
    );
    assert_eq!(
        parent.input.get("callId").and_then(JsonValue::as_str),
        Some("c1")
    );
    // TS `ToolTaskInput`：`{ kind: "model", assistant: EntryId, callId }`。
    assert_eq!(
        parent.input.get("kind").and_then(JsonValue::as_str),
        Some("model")
    );
    assert_eq!(
        parent.input.get("callId").and_then(JsonValue::as_str),
        Some("c1")
    );
    assert!(
        parent
            .input
            .get("assistant")
            .and_then(JsonValue::as_i64)
            .is_some()
    );
    assert_eq!(child.owner, Some(parent.id));
    assert_eq!(
        child.input,
        serde_json::json!({
            "kind": "nested",
            "parent": parent.id.get(),
            "parentCallId": "c1",
            "key": "1",
            "call": {"type": "toolCall", "id": "c1/1", "name": "echo", "arguments": {"text": "a"}},
        })
    );
    // 结果随调用者任务退休：文档不再可读。
    let nested_doc = harness
        .snapshot(
            &kiana_durable::types::DocTarget::task_family(
                &kiana_durable::harness::tool::NESTED_CALL_DOC,
                parent.id,
                "1",
                JsonValue::Null,
            ),
            &*context(),
        )
        .await
        .expect("nested doc snapshot");
    assert!(nested_doc.is_none());
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn gives_each_nested_call_its_own_task_and_call_id() {
    let setup = ChatSetup::new();
    let seen: Arc<std::sync::Mutex<Vec<(kiana_durable::types::TaskId, String)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_for_who = Arc::clone(&seen);
    harness_support::add_tool(
        &setup.registry,
        tool(
            "who",
            Arc::new(
                move |_args: JsonValue, api: Arc<dyn ToolExecutionApi>, _context| {
                    seen_for_who
                        .lock()
                        .unwrap()
                        .push((api.task_id(), api.call_id().to_owned()));
                    Ok(ToolExecutionResult::default())
                },
            ),
        ),
    );
    harness_support::add_tool(
        &setup.registry,
        nested_batch_tool(|_args, api, call_context| {
            Box::pin(async move {
                let first = Arc::clone(&api).execute_tool(
                    "who",
                    JsonObject::new(),
                    &*call_context,
                    kiana_durable::harness::types::NestedToolOptions {
                        key: Some("a".to_owned()),
                        progress: None,
                    },
                );
                let second = Arc::clone(&api).execute_tool(
                    "who",
                    JsonObject::new(),
                    &*call_context,
                    kiana_durable::harness::types::NestedToolOptions {
                        key: Some("b".to_owned()),
                        progress: None,
                    },
                );
                let _ = futures::future::try_join(first, second).await?;
                Ok(ToolExecutionResult::default())
            })
        }),
    );
    setup
        .faux
        .set_responses(vec![calls(&[("batch", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    let seen = seen.lock().unwrap().clone();
    let mut call_ids: Vec<String> = seen.iter().map(|(_, call_id)| call_id.clone()).collect();
    call_ids.sort();
    assert_eq!(call_ids, vec!["c1/a".to_owned(), "c1/b".to_owned()]);
    let mut task_ids: Vec<i64> = seen.iter().map(|(task_id, _)| task_id.get()).collect();
    task_ids.sort();
    task_ids.dedup();
    assert_eq!(task_ids.len(), 2);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn returns_an_error_result_for_an_unavailable_tool_and_invalid_arguments() {
    let setup = ChatSetup::new();
    harness_support::add_tool(
        &setup.registry,
        ToolRegistration {
            tool: Tool {
                name: "echo".to_owned(),
                description: "echo".to_owned(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"],
                }),
                constrained_sampling: None,
            },
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            structured_output_schema: None,
            callers: None,
            deferred: false,
            output_limits: None,
            execute: Some(Arc::new(|_args: JsonValue, _api, _context| {
                Box::pin(async { Ok(ToolExecutionResult::default()) })
                    as Pin<Box<dyn Future<Output = Result<ToolExecutionResult, BoxError>> + Send>>
            })),
        },
    );
    let nested: Arc<
        std::sync::Mutex<Vec<kiana_durable::harness::types::NestedToolExecutionResult>>,
    > = Arc::new(std::sync::Mutex::new(Vec::new()));
    let nested_for_batch = Arc::clone(&nested);
    harness_support::add_tool(
        &setup.registry,
        nested_batch_tool(move |_args, api, call_context| {
            let nested = Arc::clone(&nested_for_batch);
            Box::pin(async move {
                let missing = Arc::clone(&api)
                    .execute_tool(
                        "missing",
                        JsonObject::new(),
                        &*call_context,
                        Default::default(),
                    )
                    .await?;
                let invalid = Arc::clone(&api)
                    .execute_tool(
                        "echo",
                        object(&[("text", serde_json::json!({"not": "a string"}))]),
                        &*call_context,
                        Default::default(),
                    )
                    .await?;
                let mut collected = nested.lock().unwrap();
                collected.push(missing);
                collected.push(invalid);
                Ok(ToolExecutionResult::default())
            })
        }),
    );
    setup
        .faux
        .set_responses(vec![calls(&[("batch", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    let nested = nested.lock().unwrap().clone();
    assert_eq!(nested.len(), 2);
    assert!(nested[0].is_error);
    assert_eq!(
        nested[0]
            .diagnostics
            .first()
            .map(|diagnostic| diagnostic.code.clone()),
        Some(Some("tool_unavailable".to_owned()))
    );
    assert!(nested[1].is_error);
    assert_eq!(
        nested[1]
            .diagnostics
            .first()
            .map(|diagnostic| diagnostic.code.clone()),
        Some(Some("invalid_arguments".to_owned()))
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn returns_the_call_a_key_already_names_and_rejects_reusing_the_key_for_another_call() {
    let setup = ChatSetup::new();
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let runs_for_echo = Arc::clone(&runs);
    harness_support::add_tool(
        &setup.registry,
        tool(
            "echo",
            Arc::new(move |args: JsonValue, _api, _context| {
                runs_for_echo.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let text = args
                    .get("text")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_owned();
                Ok(text_output(&format!("echo {text}")))
            }),
        ),
    );
    let reused: Arc<
        std::sync::Mutex<Option<kiana_durable::harness::types::NestedToolExecutionResult>>,
    > = Arc::new(std::sync::Mutex::new(None));
    let reused_for_batch = Arc::clone(&reused);
    harness_support::add_tool(
        &setup.registry,
        nested_batch_tool(move |_args, api, call_context| {
            let reused = Arc::clone(&reused_for_batch);
            Box::pin(async move {
                let key = || kiana_durable::harness::types::NestedToolOptions {
                    key: Some("k".to_owned()),
                    progress: None,
                };
                let _ = Arc::clone(&api)
                    .execute_tool(
                        "echo",
                        object(&[("text", JsonValue::String("a".to_owned()))]),
                        &*call_context,
                        key(),
                    )
                    .await?;
                let again = Arc::clone(&api)
                    .execute_tool(
                        "echo",
                        object(&[("text", JsonValue::String("a".to_owned()))]),
                        &*call_context,
                        key(),
                    )
                    .await?;
                *reused.lock().unwrap() = Some(again);
                // 同一个 key 换一组参数：拒绝（错误向上传播成父调用的 tool_error）。
                Arc::clone(&api)
                    .execute_tool(
                        "echo",
                        object(&[("text", JsonValue::String("b".to_owned()))]),
                        &*call_context,
                        key(),
                    )
                    .await?;
                Ok(ToolExecutionResult::default())
            })
        }),
    );
    setup
        .faux
        .set_responses(vec![calls(&[("batch", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    let reused = reused.lock().unwrap().clone().expect("the reused call");
    assert_eq!(
        reused
            .structured_output
            .as_ref()
            .and_then(JsonValue::as_str),
        Some("echo a")
    );
    let entries = chat_support::all_entries(&root, &*context()).await;
    let results = results(&entries);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    assert!(
        result_text(&results[0])
            .contains("Nested call c1/k was already made with another tool or other arguments"),
        "{}",
        result_text(&results[0])
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_explicit_keys_that_are_empty_have_a_slash_are_proto_or_are_positive_integers() {
    let setup = ChatSetup::new();
    harness_support::add_tool(
        &setup.registry,
        tool(
            "echo",
            Arc::new(|_args: JsonValue, _api, _context| Ok(ToolExecutionResult::default())),
        ),
    );
    let errors: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let errors_for_batch = Arc::clone(&errors);
    harness_support::add_tool(
        &setup.registry,
        nested_batch_tool(move |_args, api, call_context| {
            let errors = Arc::clone(&errors_for_batch);
            Box::pin(async move {
                for key in ["", "a/b", "__proto__", "7"] {
                    if let Err(error) = Arc::clone(&api)
                        .execute_tool(
                            "echo",
                            JsonObject::new(),
                            &*call_context,
                            kiana_durable::harness::types::NestedToolOptions {
                                key: Some(key.to_owned()),
                                progress: None,
                            },
                        )
                        .await
                    {
                        errors.lock().unwrap().push(error.to_string());
                    }
                }
                Ok(ToolExecutionResult::default())
            })
        }),
    );
    setup
        .faux
        .set_responses(vec![calls(&[("batch", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    let rule = "must be non-empty, without \"/\", not \"__proto__\", and not a positive integer";
    assert_eq!(
        *errors.lock().unwrap(),
        vec![
            format!("Nested call key \"\" {rule}"),
            format!("Nested call key \"a/b\" {rule}"),
            format!("Nested call key \"__proto__\" {rule}"),
            format!("Nested call key \"7\" {rule}"),
        ]
    );
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// harness-tools.test.ts: tool results 与 hooks
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn turns_a_throw_into_a_tool_error_result_with_the_partial_output() {
    let setup = ChatSetup::new();
    harness_support::add_tool(
        &setup.registry,
        tool(
            "fail",
            Arc::new(
                |_args: JsonValue, api: Arc<dyn ToolExecutionApi>, _context| {
                    api.output(
                        kiana_durable::harness::types::ToolOutput::Text("partial\n".to_owned()),
                        None,
                    );
                    Err("boom".into())
                },
            ),
        ),
    );
    setup
        .faux
        .set_responses(vec![calls(&[("fail", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    let entries = chat_support::all_entries(&root, &*context()).await;
    let results = results(&entries);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    assert_eq!(
        result_text(&results[0]),
        "partial\n|<harness>\n[error] boom\n</harness>"
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn uses_retained_output_and_the_last_details_when_the_result_omits_them_with_diagnostics_in_order()
 {
    use kiana_durable::harness::types::{
        DiagnosticSeverity, OutputLimits, ToolDiagnostic, ToolOutput,
    };
    let setup = ChatSetup::new();
    let registration = ToolRegistration {
        tool: Tool {
            name: "log".to_owned(),
            description: "The log tool".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
            constrained_sampling: None,
        },
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: None,
        callers: None,
        deferred: false,
        output_limits: Some(OutputLimits {
            max_bytes: None,
            max_lines: Some(2),
            retain: None,
        }),
        execute: Some(Arc::new(
            |_args: JsonValue, api: Arc<dyn ToolExecutionApi>, context: Arc<dyn Context>| {
                Box::pin(async move {
                    api.output(ToolOutput::Text("line 1\n".to_owned()), None);
                    api.output(ToolOutput::Bytes(b"line 2\nline 3\n".to_vec()), None);
                    api.diagnostic(ToolDiagnostic {
                        severity: DiagnosticSeverity::Info,
                        code: None,
                        message: "from api".to_owned(),
                    });
                    api.details(serde_json::json!({"step": 1}), &*context)
                        .await?;
                    api.details(serde_json::json!({"step": 2}), &*context)
                        .await?;
                    Ok(ToolExecutionResult {
                        diagnostics: Some(vec![ToolDiagnostic {
                            severity: DiagnosticSeverity::Warn,
                            code: None,
                            message: "from result".to_owned(),
                        }]),
                        ..Default::default()
                    })
                })
                    as Pin<Box<dyn Future<Output = Result<ToolExecutionResult, BoxError>> + Send>>
            },
        )),
    };
    harness_support::add_tool(&setup.registry, registration);
    setup
        .faux
        .set_responses(vec![calls(&[("log", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    let entries = chat_support::all_entries(&root, &*context()).await;
    let results = results(&entries);
    assert_eq!(
        results[0].details.clone().unwrap_or(JsonValue::Null),
        serde_json::json!({"step": 2})
    );
    assert_eq!(
        result_text(&results[0]),
        "line 1\nline 2\n|<harness>\n[info] from api\n[warn] from result\n[warn] Output truncated to its beginning: 1 lines, 7 bytes dropped\n</harness>"
    );
    let entry = entries
        .iter()
        .find(|entry| entry.kind == "kiana.tool-result")
        .expect("the result entry");
    let codes: Vec<Option<String>> = entry
        .data
        .as_ref()
        .and_then(|data| data.get("diagnostics"))
        .and_then(JsonValue::as_array)
        .map(|diagnostics| {
            diagnostics
                .iter()
                .map(|diagnostic| {
                    diagnostic
                        .get("code")
                        .and_then(JsonValue::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(codes, vec![None, None, Some("truncated".to_owned())]);
    harness.close(&*context()).await;
}

/// beforeTool 回调的测试实现。
struct ValidateBeforeTool;

#[async_trait]
impl kiana_durable::harness::types::ToolHooks for ValidateBeforeTool {
    async fn before_tool(
        &self,
        call: kiana_durable::harness::types::ToolHookCall,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<kiana_durable::harness::types::BeforeToolOutput>, BoxError> {
        use kiana_durable::harness::types::BeforeToolOutput;
        match call.call.id.as_str() {
            "block" => Ok(Some(BeforeToolOutput {
                arguments: None,
                block: Some("not today".to_owned()),
            })),
            "throw" => Err("hook failed".into()),
            "bad" => Ok(Some(BeforeToolOutput {
                arguments: Some(object(&[("text", serde_json::json!({"not": "a string"}))])),
                block: None,
            })),
            _ => {
                let text = call
                    .call
                    .arguments
                    .get("text")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_owned();
                Ok(Some(BeforeToolOutput {
                    arguments: Some(object(&[("text", JsonValue::String(format!("{text}!")))])),
                    block: None,
                }))
            }
        }
    }

    async fn after_tool(
        &self,
        _call: kiana_durable::harness::types::ToolHookCall,
        _result: ToolExecutionResult,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<ToolExecutionResult>, BoxError> {
        Ok(None)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn validates_arguments_before_and_after_before_tool_and_applies_blocks_and_replacements() {
    let setup = ChatSetup::new();
    let seen: Arc<std::sync::Mutex<Vec<JsonValue>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_for_echo = Arc::clone(&seen);
    let mut echo = tool(
        "echo",
        Arc::new(
            move |args: JsonValue, _api: Arc<dyn ToolExecutionApi>, _context| {
                seen_for_echo.lock().unwrap().push(args);
                Ok(ToolExecutionResult::default())
            },
        ),
    );
    echo.tool.parameters = serde_json::json!({
        "type": "object",
        "properties": {"text": {"type": "string"}},
    });
    harness_support::add_tool(&setup.registry, echo);
    harness_support::add_hooks(
        &setup.registry,
        vec![kiana_durable::harness::define::hook(
            &kiana_durable::harness::tool::tool_task(),
            Arc::new(ValidateBeforeTool) as Arc<dyn kiana_durable::harness::types::ToolHooks>,
        )],
        "hooks",
    );
    let round = calls(&[
        ("echo", object(&[("text", serde_json::json!(1))]), "coerced"),
        (
            "echo",
            object(&[("text", serde_json::json!({"not": "a string"}))]),
            "invalid",
        ),
        ("echo", empty_args(), "block"),
        ("echo", empty_args(), "throw"),
        (
            "echo",
            object(&[("text", JsonValue::String("x".to_owned()))]),
            "bad",
        ),
        (
            "echo",
            object(&[("text", JsonValue::String("x".to_owned()))]),
            "ok",
        ),
    ]);
    setup.faux.set_responses(vec![round, done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    let entries = chat_support::all_entries(&root, &*context()).await;
    let by_id: Vec<(String, bool, String)> = results(&entries)
        .iter()
        .map(|result| {
            (
                result.tool_call_id.clone(),
                result.is_error,
                result_text(result),
            )
        })
        .collect();
    let by_id = |id: &str| {
        by_id
            .iter()
            .find(|(call_id, _, _)| call_id == id)
            .cloned()
            .unwrap_or_else(|| panic!("result {id}: {by_id:?}"))
    };
    assert_eq!(
        by_id("block"),
        (
            "block".to_owned(),
            true,
            "<harness>\n[error] Tool call blocked: not today\n</harness>".to_owned()
        )
    );
    assert_eq!(
        by_id("throw"),
        (
            "throw".to_owned(),
            true,
            "<harness>\n[error] Tool call blocked: hook failed\n</harness>".to_owned()
        )
    );
    assert!(by_id("bad").1);
    assert!(by_id("bad").2.contains("Validation failed"));
    assert_eq!(by_id("ok"), ("ok".to_owned(), false, String::new()));
    assert!(by_id("invalid").1);
    assert!(by_id("invalid").2.contains("Validation failed"));
    // kiana-ai 在第一次校验前把数字转成字符串。
    assert_eq!(
        by_id("coerced"),
        ("coerced".to_owned(), false, String::new())
    );
    let mut seen = seen.lock().unwrap().clone();
    seen.sort_by_key(|value| {
        value
            .get("text")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned()
    });
    assert_eq!(
        seen,
        vec![
            serde_json::json!({"text": "1!"}),
            serde_json::json!({"text": "x!"})
        ]
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lets_the_first_before_tool_block_win_and_skips_later_handlers() {
    let setup = ChatSetup::new();
    harness_support::add_tool(
        &setup.registry,
        tool(
            "echo",
            Arc::new(|_args: JsonValue, _api, _context| Ok(ToolExecutionResult::default())),
        ),
    );
    let asked: Arc<std::sync::Mutex<Vec<&'static str>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    struct Ask(
        Arc<std::sync::Mutex<Vec<&'static str>>>,
        &'static str,
        &'static str,
    );
    #[async_trait]
    impl kiana_durable::harness::types::ToolHooks for Ask {
        async fn before_tool(
            &self,
            _call: kiana_durable::harness::types::ToolHookCall,
            _api: Arc<dyn kiana_durable::harness::types::HookApi>,
            _context: &dyn Context,
        ) -> Result<Option<kiana_durable::harness::types::BeforeToolOutput>, BoxError> {
            self.0.lock().unwrap().push(self.1);
            Ok(Some(kiana_durable::harness::types::BeforeToolOutput {
                arguments: None,
                block: Some(self.2.to_owned()),
            }))
        }
        async fn after_tool(
            &self,
            _call: kiana_durable::harness::types::ToolHookCall,
            _result: ToolExecutionResult,
            _api: Arc<dyn kiana_durable::harness::types::HookApi>,
            _context: &dyn Context,
        ) -> Result<Option<ToolExecutionResult>, BoxError> {
            Ok(None)
        }
    }
    harness_support::add_hooks(
        &setup.registry,
        vec![kiana_durable::harness::define::hook(
            &kiana_durable::harness::tool::tool_task(),
            Arc::new(Ask(Arc::clone(&asked), "first", "first says no"))
                as Arc<dyn kiana_durable::harness::types::ToolHooks>,
        )],
        "first-hooks",
    );
    harness_support::add_hooks(
        &setup.registry,
        vec![kiana_durable::harness::define::hook(
            &kiana_durable::harness::tool::tool_task(),
            Arc::new(Ask(Arc::clone(&asked), "second", "second says no"))
                as Arc<dyn kiana_durable::harness::types::ToolHooks>,
        )],
        "second-hooks",
    );
    setup
        .faux
        .set_responses(vec![calls(&[("echo", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    assert_eq!(*asked.lock().unwrap(), vec!["first"]);
    let entries = chat_support::all_entries(&root, &*context()).await;
    assert_eq!(
        result_text(&results(&entries)[0]),
        "<harness>\n[error] Tool call blocked: first says no\n</harness>"
    );
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// harness-tools.test.ts: afterTool / afterTools 与 ToolControl
// ---------------------------------------------------------------------------

struct ReplaceOutput;
struct RecordDetails;
type AfterToolsObservation = Arc<
    std::sync::Mutex<
        Vec<(
            kiana_durable::types::EntryId,
            Vec<kiana_durable::types::EntryId>,
        )>,
    >,
>;

struct ObserveAfterTools(AfterToolsObservation);

#[async_trait]
impl kiana_durable::harness::types::ToolHooks for ReplaceOutput {
    async fn before_tool(
        &self,
        _call: kiana_durable::harness::types::ToolHookCall,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<kiana_durable::harness::types::BeforeToolOutput>, BoxError> {
        Ok(None)
    }
    async fn after_tool(
        &self,
        _call: kiana_durable::harness::types::ToolHookCall,
        result: ToolExecutionResult,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<ToolExecutionResult>, BoxError> {
        Ok(Some(ToolExecutionResult {
            output: Some(vec![kiana_ai::types::UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "first".to_owned(),
                text_signature: None,
            })]),
            ..result
        }))
    }
}

#[async_trait]
impl kiana_durable::harness::types::ToolHooks for RecordDetails {
    async fn before_tool(
        &self,
        _call: kiana_durable::harness::types::ToolHookCall,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<kiana_durable::harness::types::BeforeToolOutput>, BoxError> {
        Ok(None)
    }
    async fn after_tool(
        &self,
        _call: kiana_durable::harness::types::ToolHookCall,
        result: ToolExecutionResult,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<ToolExecutionResult>, BoxError> {
        let replaced = result
            .output
            .clone()
            .unwrap_or_default()
            .iter()
            .map(|item| match item {
                kiana_ai::types::UserContentBlock::Text(text) => text.text.clone(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("|");
        Ok(Some(ToolExecutionResult {
            details: Some(serde_json::json!({"replaced": replaced})),
            ..result
        }))
    }
}

#[async_trait]
impl kiana_durable::harness::types::GenerationHooks for ObserveAfterTools {
    async fn before_request(
        &self,
        request: kiana_durable::harness::types::BeforeRequestInput,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<kiana_durable::harness::types::BeforeRequestInput>, BoxError> {
        Ok(Some(request))
    }
    async fn after_response(
        &self,
        _message: AssistantMessage,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Ok(())
    }
    async fn on_yield(
        &self,
        _answer: AssistantMessage,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<kiana_durable::harness::types::OnYieldOutput>, BoxError> {
        Ok(None)
    }
    async fn after_tools(
        &self,
        assistant: kiana_durable::types::EntryId,
        results: Vec<kiana_durable::types::EntryId>,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        self.0.lock().unwrap().push((assistant, results));
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn chains_after_tool_replacements_and_observes_the_round_with_after_tools() {
    let setup = ChatSetup::new();
    harness_support::add_tool(
        &setup.registry,
        tool(
            "echo",
            Arc::new(|_args: JsonValue, _api, _context| Ok(text_output("raw"))),
        ),
    );
    harness_support::add_hooks(
        &setup.registry,
        vec![kiana_durable::harness::define::hook(
            &kiana_durable::harness::tool::tool_task(),
            Arc::new(ReplaceOutput) as Arc<dyn kiana_durable::harness::types::ToolHooks>,
        )],
        "replace",
    );
    harness_support::add_hooks(
        &setup.registry,
        vec![kiana_durable::harness::define::hook(
            &kiana_durable::harness::tool::tool_task(),
            Arc::new(RecordDetails) as Arc<dyn kiana_durable::harness::types::ToolHooks>,
        )],
        "details",
    );
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    harness_support::add_hooks(
        &setup.registry,
        vec![kiana_durable::harness::define::hook(
            &kiana_durable::harness::generation::generation_task(),
            Arc::new(ObserveAfterTools(Arc::clone(&observed)))
                as Arc<dyn kiana_durable::harness::types::GenerationHooks>,
        )],
        "observe",
    );
    setup
        .faux
        .set_responses(vec![calls(&[("echo", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    let entries = chat_support::all_entries(&root, &*context()).await;
    let results = results(&entries);
    assert_eq!(result_text(&results[0]), "first");
    assert_eq!(
        results[0].details.clone().unwrap_or(JsonValue::Null),
        serde_json::json!({"replaced": "first"})
    );
    let result_entry = entries
        .iter()
        .find(|entry| entry.kind == "kiana.tool-result")
        .expect("the result entry");
    assert_eq!(
        *observed.lock().unwrap(),
        vec![(entries[2].id, vec![result_entry.id])]
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn does_not_terminate_on_terminate_false() {
    let setup = ChatSetup::new();
    let keep = tool(
        "keep",
        Arc::new(|_args: JsonValue, _api, _context| {
            Ok(ToolExecutionResult {
                control: Some(kiana_durable::harness::types::ToolControl {
                    terminate: Some(false),
                    add_tools: None,
                    handoff: None,
                }),
                ..Default::default()
            })
        }),
    );
    harness_support::add_tool(&setup.registry, keep.clone());
    setup
        .faux
        .set_responses(vec![calls(&[("keep", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    root.configure(
        kiana_durable::harness::types::AgentChange {
            tools: Some(kiana_durable::harness::types::ToolChange::Set(vec![keep])),
            ..Default::default()
        },
        &*context(),
    )
    .await
    .expect("configure");
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    let entries = chat_support::all_entries(&root, &*context()).await;
    assert_eq!(entries.last().expect("entries").kind, "kiana.assistant");
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn applies_add_tools_and_terminates_only_when_every_result_of_the_round_asks_to() {
    use kiana_durable::harness::types::{AgentChange, ToolChange, ToolControl};
    let setup = ChatSetup::new();
    let control_tool = |name: &str, control: ToolControl| {
        let control = control.clone();
        tool(
            name,
            Arc::new(
                move |_args: JsonValue, _api: Arc<dyn ToolExecutionApi>, _context| {
                    Ok(ToolExecutionResult {
                        control: Some(control.clone()),
                        ..Default::default()
                    })
                },
            ),
        )
    };
    let stop = control_tool(
        "stop",
        ToolControl {
            terminate: Some(true),
            add_tools: None,
            handoff: None,
        },
    );
    let grow = control_tool(
        "grow",
        ToolControl {
            terminate: None,
            add_tools: Some(vec!["extra".to_owned(), "stop".to_owned()]),
            handoff: None,
        },
    );
    let extra = tool(
        "extra",
        Arc::new(|_args: JsonValue, _api, _context| Ok(ToolExecutionResult::default())),
    );
    harness_support::add_tool(&setup.registry, stop.clone());
    harness_support::add_tool(&setup.registry, grow.clone());
    harness_support::add_tool(&setup.registry, extra);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    root.configure(
        AgentChange {
            tools: Some(ToolChange::Set(vec![stop.clone(), grow.clone()])),
            ..Default::default()
        },
        &*context(),
    )
    .await
    .expect("configure");

    // 一轮只有一个 terminate 的结果：运行在这一轮结束，不再请求模型。
    setup
        .faux
        .set_responses(vec![calls(&[("stop", empty_args(), "c1")])]);
    let first = submit_and_wait(&root, "go").await;
    assert_eq!(first.status, SubmissionStatus::Done);
    let entries = chat_support::all_entries(&root, &*context()).await;
    assert_eq!(entries.last().expect("entries").kind, "kiana.tool-result");
    let tasks = harness
        .commit(
            &ScanTasksChange {
                conversation_id: Some(root.id()),
                kind: None,
                limit: 20,
            },
            &*context(),
            Default::default(),
        )
        .await
        .expect("tasks")
        .downcast::<Vec<kiana_durable::types::TaskRecord>>()
        .map(|tasks| *tasks)
        .expect("records");
    assert!(
        tasks
            .iter()
            .all(|task| task.state.status() == kiana_durable::types::TaskStatus::Terminal),
        "{:?}",
        tasks
            .iter()
            .map(|task| task.state.status())
            .collect::<Vec<_>>()
    );

    // 两个结果里只有一个 terminate：运行继续，模型给出最终回答；addTools 追加进存储的 agent。
    setup.faux.set_responses(vec![
        calls(&[("stop", empty_args(), "c1"), ("grow", empty_args(), "c2")]),
        done(),
    ]);
    let second = submit_and_wait(&root, "again").await;
    assert_eq!(second.status, SubmissionStatus::Done);
    let entries = chat_support::all_entries(&root, &*context()).await;
    assert_eq!(entries.last().expect("entries").kind, "kiana.assistant");
    let agent = harness
        .snapshot(
            &kiana_durable::types::DocTarget::conversation(
                &kiana_durable::harness::agent::AGENT_DOC,
                root.id(),
            ),
            &*context(),
        )
        .await
        .expect("agent snapshot")
        .expect("the agent document");
    assert_eq!(
        agent.get("tools").cloned().unwrap_or(JsonValue::Null),
        serde_json::json!(["stop", "grow", "extra"])
    );
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// harness-generation.test.ts
// ---------------------------------------------------------------------------

fn error_503() -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message(
        Vec::<AssistantContentBlock>::new(),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("503 Service Unavailable".to_owned()),
            ..Default::default()
        },
    ))
}

fn text_of_message(message: &Message) -> Option<String> {
    chat_support::text_of(Some(message))
}

/// 从提交观察里收集 `kiana.live` 的值（TS: `livePublications`）。
fn live_publications(
    harness: &Arc<kiana_durable::harness::harness::HarnessImpl>,
) -> (
    Arc<std::sync::Mutex<Vec<JsonObject>>>,
    kiana_durable::types::Unsubscribe,
) {
    let values = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&values);
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
                {
                    sink.lock().unwrap().push(value.clone());
                }
            }
        }))
        .expect("the commit observer");
    (values, unsubscribe)
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_a_retryable_error_after_a_durable_backoff_and_then_answers() {
    let setup = ChatSetup::new();
    harness_support::add_section(
        &setup.registry,
        "preamble",
        Arc::new(
            |_input: &kiana_durable::harness::types::PromptInput, _context| {
                Box::pin(async { Ok(Some("p".to_owned())) })
                    as Pin<Box<dyn Future<Output = Result<Option<String>, BoxError>> + Send>>
            },
        ),
        Some(false),
    );
    setup.faux.set_responses(vec![
        error_503(),
        FauxResponseStep::Message(faux_assistant_message(
            faux_text("recovered"),
            Default::default(),
        )),
    ]);
    setup.settings.lock().unwrap().retry = Some(kiana_durable::harness::types::RetrySettings {
        enabled: Some(true),
        max_retries: Some(3),
        base_delay_ms: Some(1.0),
        max_agent_delay_ms: None,
    });
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let (values, _unsubscribe) = live_publications(&harness);
    let settled = submit_and_wait(&root, "hi").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    let entries = chat_support::all_entries(&root, &*context()).await;
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "kiana.user",
            "kiana.system",
            "kiana.assistant",
            "kiana.assistant"
        ]
    );
    let failed = match entries[2].model.as_ref().and_then(|model| model.first()) {
        Some(Message::Assistant(assistant)) => assistant.clone(),
        other => panic!("the failed attempt is an assistant entry: {other:?}"),
    };
    assert_eq!(failed.stop_reason, StopReason::Error);
    let values = values.lock().unwrap().clone();
    assert!(
        values.iter().any(|value| {
            value
                .get("generation")
                .and_then(|generation| generation.get("retry"))
                .and_then(|retry| retry.get("error"))
                .and_then(JsonValue::as_str)
                == Some("503 Service Unavailable")
        }),
        "a retry was published: {values:?}"
    );
    assert!(
        values.iter().any(|value| {
            value
                .get("generation")
                .and_then(|generation| generation.get("attempt"))
                .and_then(JsonValue::as_i64)
                == Some(2)
        }),
        "the second attempt was published: {values:?}"
    );
    let live = harness
        .snapshot(
            &kiana_durable::types::DocTarget::conversation(
                &kiana_durable::harness::live::LIVE_DOC,
                root.id(),
            ),
            &*context(),
        )
        .await
        .expect("live snapshot")
        .unwrap_or_default();
    assert_eq!(live, JsonObject::new());
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fails_with_model_error_once_retries_are_exhausted() {
    let setup = ChatSetup::new();
    setup.faux.set_responses(vec![
        error_503(),
        error_503(),
        FauxResponseStep::Message(faux_assistant_message(
            faux_text("never"),
            Default::default(),
        )),
    ]);
    setup.settings.lock().unwrap().retry = Some(kiana_durable::harness::types::RetrySettings {
        enabled: Some(true),
        max_retries: Some(1),
        base_delay_ms: Some(1.0),
        max_agent_delay_ms: None,
    });
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "hi").await;
    assert_eq!(settled.status, SubmissionStatus::Unanswered);
    assert_eq!(settled.reason.as_deref(), Some("model_error"));
    assert_eq!(
        settled.detail.clone().unwrap_or(JsonValue::Null),
        JsonValue::String("503 Service Unavailable".to_owned())
    );
    let entries = chat_support::all_entries(&root, &*context()).await;
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["kiana.user", "kiana.assistant", "kiana.assistant"]
    );
    assert_eq!(setup.faux.get_pending_response_count(), 1);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fails_a_non_retryable_error_without_retrying() {
    let setup = ChatSetup::new();
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            Vec::<AssistantContentBlock>::new(),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("Invalid request".to_owned()),
                ..Default::default()
            },
        )),
        FauxResponseStep::Message(faux_assistant_message(
            faux_text("never"),
            Default::default(),
        )),
    ]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "hi").await;
    assert_eq!(settled.status, SubmissionStatus::Unanswered);
    assert_eq!(settled.reason.as_deref(), Some("model_error"));
    assert_eq!(
        settled.detail.clone().unwrap_or(JsonValue::Null),
        JsonValue::String("Invalid request".to_owned())
    );
    let tasks = harness
        .commit(
            &ScanTasksChange {
                conversation_id: Some(root.id()),
                kind: None,
                limit: 10,
            },
            &*context(),
            Default::default(),
        )
        .await
        .expect("tasks")
        .downcast::<Vec<kiana_durable::types::TaskRecord>>()
        .map(|tasks| *tasks)
        .expect("records");
    match &tasks[0].state {
        kiana_durable::types::TaskState::Terminal {
            outcome: kiana_durable::types::TaskOutcome::Failed { error, .. },
        } => {
            assert_eq!(error.message, "Invalid request");
            assert_eq!(
                error.detail.clone().unwrap_or(JsonValue::Null),
                serde_json::json!({"reason": "model_error"})
            );
        }
        other => panic!("the run failed: {other:?}"),
    }
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn polls_a_deferred_response_until_it_is_ready() {
    let setup =
        ChatSetup::with_faux_options(kiana_ai::providers::faux::RegisterFauxProviderOptions {
            deferred: Some(kiana_ai::providers::faux::FauxDeferredOptions {
                pending_fetches: Some(1.0),
                poll_after_ms: Some(1.0),
            }),
            ..Default::default()
        });
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            faux_text("deferred answer"),
            Default::default(),
        ))]);
    setup.settings.lock().unwrap().stream =
        Some(kiana_durable::harness::types::ConversationStreamOptions {
            deferred: Some(kiana_durable::harness::types::Deferred::Enabled(true)),
            ..Default::default()
        });
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let (values, _unsubscribe) = live_publications(&harness);
    let settled = submit_and_wait(&root, "hi").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    assert_eq!(settled.kind, kiana_durable::types::SubmissionKind::Input);
    assert_eq!(setup.faux.state().deferred_fetch_count(), 2);
    let poll_times: Vec<i64> = values
        .lock()
        .unwrap()
        .iter()
        .filter_map(|value| {
            value
                .get("generation")
                .and_then(|generation| generation.get("deferred"))
                .and_then(|deferred| deferred.get("pollAt"))
                .and_then(JsonValue::as_i64)
        })
        .collect();
    assert_eq!(poll_times.len(), 2);
    assert!(poll_times[1] > poll_times[0], "{poll_times:?}");
    let entries = chat_support::all_entries(&root, &*context()).await;
    let answer = settled.answer.expect("the answer entry");
    let entry = entries
        .iter()
        .find(|entry| entry.id == answer)
        .expect("the answer entry is committed");
    assert_eq!(
        text_of_message(&entry.model.clone().unwrap()[0]).as_deref(),
        Some("deferred answer")
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fails_with_no_model_when_no_model_is_configured_or_the_model_is_unknown() {
    let setup = ChatSetup::new();
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let plain = harness
        .create_conversation(
            kiana_durable::harness::types::ConversationCreateOptions {
                ownership: kiana_durable::types::ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context(),
        )
        .await
        .expect("the ownerless conversation");
    let unset = submit_and_wait(&plain, "hi").await;
    assert_eq!(unset.status, SubmissionStatus::Unanswered);
    assert_eq!(unset.reason.as_deref(), Some("no_model"));

    root.configure(
        kiana_durable::harness::types::AgentChange {
            model: Some(Some(kiana_durable::harness::types::ModelRef {
                provider: "faux".to_owned(),
                model_id: "missing".to_owned(),
            })),
            ..Default::default()
        },
        &*context(),
    )
    .await
    .expect("configure");
    let unknown = submit_and_wait(&root, "hi").await;
    assert_eq!(unknown.status, SubmissionStatus::Unanswered);
    assert_eq!(unknown.reason.as_deref(), Some("no_model"));
    assert!(unknown.entry.is_some());
    let entries = chat_support::all_entries(&root, &*context()).await;
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["kiana.user"]
    );
    let tasks = harness
        .commit(
            &ScanTasksChange {
                conversation_id: Some(root.id()),
                kind: None,
                limit: 10,
            },
            &*context(),
            Default::default(),
        )
        .await
        .expect("tasks")
        .downcast::<Vec<kiana_durable::types::TaskRecord>>()
        .map(|tasks| *tasks)
        .expect("records");
    match &tasks[0].state {
        kiana_durable::types::TaskState::Terminal {
            outcome: kiana_durable::types::TaskOutcome::Failed { error, .. },
        } => {
            assert_eq!(error.message, "Model faux/missing is not available");
            assert_eq!(
                error.detail.clone().unwrap_or(JsonValue::Null),
                serde_json::json!({"reason": "no_model"})
            );
        }
        other => panic!("the run failed: {other:?}"),
    }
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// harness-tools.test.ts: generation hooks
// ---------------------------------------------------------------------------

struct GenerationProbe {
    responses: Arc<std::sync::Mutex<Vec<String>>>,
    yields: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl kiana_durable::harness::types::GenerationHooks for GenerationProbe {
    async fn before_request(
        &self,
        mut request: kiana_durable::harness::types::BeforeRequestInput,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<kiana_durable::harness::types::BeforeRequestInput>, BoxError> {
        request
            .messages
            .push(Message::User(kiana_ai::types::UserMessage {
                role: kiana_ai::types::UserRole::User,
                content: kiana_ai::types::UserContent::Text("injected".to_owned()),
                timestamp: 0,
            }));
        Ok(Some(request))
    }

    async fn after_response(
        &self,
        message: AssistantMessage,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        if let Some(text) = chat_support::text_of(Some(&Message::Assistant(message))) {
            self.responses.lock().unwrap().push(text);
        }
        Ok(())
    }

    async fn on_yield(
        &self,
        _answer: AssistantMessage,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<kiana_durable::harness::types::OnYieldOutput>, BoxError> {
        if self
            .yields
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            == 0
        {
            return Ok(Some(kiana_durable::harness::types::OnYieldOutput {
                continue_run: kiana_ai::types::UserContent::Text("keep going".to_owned()),
            }));
        }
        Ok(None)
    }

    async fn after_tools(
        &self,
        _assistant: kiana_durable::types::EntryId,
        _results: Vec<kiana_durable::types::EntryId>,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn replaces_request_messages_observes_responses_and_continues_on_yield() {
    let setup = ChatSetup::new();
    let responses = Arc::new(std::sync::Mutex::new(Vec::new()));
    let yields = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    harness_support::add_hooks(
        &setup.registry,
        vec![kiana_durable::harness::define::hook(
            &kiana_durable::harness::generation::generation_task(),
            Arc::new(GenerationProbe {
                responses: Arc::clone(&responses),
                yields: Arc::clone(&yields),
            }) as Arc<dyn kiana_durable::harness::types::GenerationHooks>,
        )],
        "probe",
    );
    let requests: Arc<std::sync::Mutex<Vec<Vec<String>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let requests_for_factory = Arc::clone(&requests);
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count_for_factory = Arc::clone(&count);
    let factory = FauxResponseStep::factory(move |input| {
        let requests = Arc::clone(&requests_for_factory);
        let count = Arc::clone(&count_for_factory);
        let recorded: Vec<String> = input
            .context
            .as_context()
            .messages
            .iter()
            .map(|message| {
                format!(
                    "{}:{}",
                    message.role(),
                    text_of_message(message).unwrap_or_default()
                )
            })
            .collect();
        let number = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        async move {
            requests.lock().unwrap().push(recorded);
            Ok(faux_assistant_message(
                faux_text(format!("answer {number}")),
                Default::default(),
            ))
        }
    });
    setup.faux.set_responses(vec![factory.clone(), factory]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    let requests = requests.lock().unwrap().clone();
    assert_eq!(
        requests[0].get(requests[0].len() - 2..).unwrap_or_default(),
        ["user:go".to_owned(), "user:injected".to_owned()]
    );
    assert_eq!(
        *responses.lock().unwrap(),
        vec!["answer 1".to_owned(), "answer 2".to_owned()]
    );
    let entries = chat_support::all_entries(&root, &*context()).await;
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "kiana.user",
            "kiana.assistant",
            "kiana.user",
            "kiana.assistant"
        ]
    );
    // 注入的消息只用于那次请求。
    assert!(entries.iter().all(|entry| {
        entry
            .model
            .as_ref()
            .and_then(|model| model.first())
            .and_then(text_of_message)
            .as_deref()
            != Some("injected")
    }));
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// harness-compaction.test.ts: 手动压缩的端到端放置
// ---------------------------------------------------------------------------

/// 约 `tokens` 个估算 token 的文本，以 `label` 开头。
fn sized_text(label: &str, tokens: usize) -> String {
    let length = tokens * 4;
    format!(
        "{label} {}",
        "x".repeat(length.saturating_sub(label.len() + 1))
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn places_the_summary_at_once_when_idle_and_keeps_raw_history() {
    let setup = ChatSetup::new();
    harness_support::add_section(
        &setup.registry,
        "preamble",
        Arc::new(
            |_input: &kiana_durable::harness::types::PromptInput, _context| {
                Box::pin(async { Ok(Some("You are helpful.".to_owned())) })
                    as Pin<Box<dyn Future<Output = Result<Option<String>, BoxError>> + Send>>
            },
        ),
        Some(false),
    );
    setup.settings.lock().unwrap().compaction =
        Some(kiana_durable::harness::types::CompactionSettings {
            enabled: Some(false),
            reserve_tokens: Some(1000.0),
            keep_recent_tokens: Some(150.0),
            background_tokens: Some(0.0),
            model_overrides: None,
        });
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            faux_text(sized_text("a1", 100)),
            Default::default(),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            faux_text(sized_text("a2", 100)),
            Default::default(),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            faux_text(sized_text("a3", 100)),
            Default::default(),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            faux_text("SUMMARY"),
            Default::default(),
        )),
    ]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    for (user, expected) in [
        (sized_text("u1", 100), SubmissionStatus::Done),
        (sized_text("u2", 100), SubmissionStatus::Done),
        (sized_text("u3", 100), SubmissionStatus::Done),
    ] {
        let settled = submit_and_wait(&root, &user).await;
        assert_eq!(settled.status, expected);
    }
    let before = chat_support::all_entries(&root, &*context()).await;
    let u3 = before
        .iter()
        .find(|entry| {
            entry
                .model
                .as_ref()
                .and_then(|model| model.first())
                .and_then(text_of_message)
                .is_some_and(|text| text.starts_with("u3 "))
        })
        .expect("the u3 entry")
        .id;

    let id = root
        .compact(Some("focus on files".to_owned()), &*context())
        .await
        .expect("compact");
    let settled_task = harness
        .wait_for_task(kiana_durable::id_from_number(id.get()), &*context())
        .await
        .expect("the compaction task");
    let submission_id = match settled_task.outcome() {
        kiana_durable::types::TaskOutcome::Completed { result } => {
            let id = result
                .get("submissionId")
                .and_then(JsonValue::as_i64)
                .expect("the summary submission");
            kiana_durable::id_from_number(id)
        }
        other => panic!("the compaction completed: {other:?}"),
    };
    let placed = harness
        .submission(submission_id, &*context())
        .await
        .expect("the submission lookup")
        .expect("the submission")
        .wait(&*context())
        .await
        .expect("the placed submission");
    assert_eq!(placed.status, SubmissionStatus::Done);

    // 原始历史不变；一个摘要条目指向第一个保留条目 u3。
    let after = chat_support::all_entries(&root, &*context()).await;
    assert_eq!(&after[..before.len()], &before[..]);
    let marker = after.last().expect("the summary entry");
    assert_eq!(marker.kind, "kiana.compaction");
    assert_eq!(marker.head, Some(u3));
    assert_eq!(
        marker.data.clone().unwrap_or(JsonValue::Null),
        serde_json::json!({"reason": "manual"})
    );
    assert_eq!(placed.entry, Some(marker.id));
    let marker_text = text_of_message(&marker.model.as_ref().expect("model")[0]).expect("text");
    assert_eq!(
        marker_text,
        "The conversation history before this point was compacted into the following summary:\n\n<summary>\nSUMMARY\n</summary>"
    );

    // 模型上下文是摘要加上保留的条目。
    let view = root
        .context(
            &*context(),
            kiana_durable::harness::types::ContextViewOptions::default(),
        )
        .await
        .expect("context view");
    let texts: Vec<String> = view.messages.iter().filter_map(text_of_message).collect();
    assert_eq!(
        texts,
        vec![marker_text, sized_text("u3", 100), sized_text("a3", 100)]
    );
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// harness-nested-tools.test.ts: 中止级联
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn aborts_nested_calls_with_their_caller() {
    let setup = ChatSetup::new();
    let running = Arc::new(task_support::Deferred::new());
    let running_for_hang = Arc::clone(&running);
    harness_support::add_tool(
        &setup.registry,
        ToolRegistration {
            tool: Tool {
                name: "hang".to_owned(),
                description: "The hang tool".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
                constrained_sampling: None,
            },
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            structured_output_schema: None,
            callers: None,
            deferred: false,
            output_limits: None,
            execute: Some(Arc::new(
                move |_args: JsonValue,
                      _api: Arc<dyn ToolExecutionApi>,
                      context: Arc<dyn Context>| {
                    let running = Arc::clone(&running_for_hang);
                    Box::pin(async move {
                        running.resolve(());
                        match context.abort_signal() {
                            Some(signal) => Err(task_support::aborted(&signal).await),
                            None => Err("no abort signal".into()),
                        }
                    })
                        as Pin<
                            Box<dyn Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
                        >
                },
            )),
        },
    );
    harness_support::add_tool(
        &setup.registry,
        nested_batch_tool(|_args, api, call_context| {
            Box::pin(async move {
                let _ = Arc::clone(&api)
                    .execute_tool(
                        "hang",
                        JsonObject::new(),
                        &*call_context,
                        Default::default(),
                    )
                    .await;
                Ok(ToolExecutionResult::default())
            })
        }),
    );
    setup
        .faux
        .set_responses(vec![calls(&[("batch", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let draft: InputSubmissionDraft = harness_support::text_input("go");
    let submission = root
        .submit(draft.into_draft(), &*context())
        .await
        .expect("submit");
    running.promise().await.expect("the nested call runs");
    root.abort(
        &*context(),
        kiana_durable::harness::types::ConversationAbortOptions::default(),
    )
    .await
    .expect("abort");
    let settled = submission.wait(&*context()).await.expect("settled");
    assert_eq!(settled.status, SubmissionStatus::Unanswered);
    assert_eq!(settled.reason.as_deref(), Some("aborted"));
    let tasks = tool_tasks(&harness).await;
    let outcome = |kind: &str| {
        tasks
            .iter()
            .find(|task| task.input.get("kind").and_then(JsonValue::as_str) == Some(kind))
            .and_then(|task| match &task.state {
                kiana_durable::types::TaskState::Terminal { outcome } => Some(outcome.clone()),
                _ => None,
            })
    };
    let child = outcome("nested").expect("the nested outcome");
    let parent = outcome("model").expect("the model outcome");
    assert!(
        matches!(
            &child,
            kiana_durable::types::TaskOutcome::Aborted { result: Some(result), .. }
                if result.get("kind").and_then(JsonValue::as_str) == Some("nested")
        ),
        "{child:?}"
    );
    assert!(
        matches!(
            &parent,
            kiana_durable::types::TaskOutcome::Aborted { result: Some(result), .. }
                if result.get("kind").and_then(JsonValue::as_str) == Some("model")
        ),
        "{parent:?}"
    );
    let live = harness
        .snapshot(
            &kiana_durable::types::DocTarget::conversation(
                &kiana_durable::harness::live::LIVE_DOC,
                root.id(),
            ),
            &*context(),
        )
        .await
        .expect("live snapshot")
        .unwrap_or_default();
    assert_eq!(live, JsonObject::new());
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// harness-nested-tools.test.ts: 未 await 的 admission（TS: `void api.executeTool(...)`）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn aborts_a_nested_call_whose_admission_was_still_committing_when_its_caller_returned() {
    let setup = ChatSetup::new();
    // `echo`：热身调用，正常返回。
    harness_support::add_tool(
        &setup.registry,
        tool(
            "echo",
            Arc::new(|_args, _api, _context| Ok(ToolExecutionResult::default())),
        ),
    );
    // `hang`：一直等到自己的中止信号。
    harness_support::add_tool(
        &setup.registry,
        ToolRegistration {
            tool: Tool {
                name: "hang".to_owned(),
                description: "The hang tool".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
                constrained_sampling: None,
            },
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            structured_output_schema: None,
            callers: None,
            deferred: false,
            output_limits: None,
            execute: Some(Arc::new(
                move |_args: JsonValue,
                      _api: Arc<dyn ToolExecutionApi>,
                      context: Arc<dyn Context>| {
                    Box::pin(async move {
                        match context.abort_signal() {
                            Some(signal) => Err(task_support::aborted(&signal).await),
                            None => Err("no abort signal".into()),
                        }
                    })
                        as Pin<
                            Box<dyn Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
                        >
                },
            )),
        },
    );
    // `batch`：先 await 一个热身调用，再像 TS 的 `void api.executeTool("hang", ...)` 那样启动
    // 一个不与它 await 的嵌套调用，然后立刻返回。
    harness_support::add_tool(
        &setup.registry,
        nested_batch_tool(|_args, api, call_context| {
            Box::pin(async move {
                Arc::clone(&api)
                    .execute_tool(
                        "echo",
                        JsonObject::new(),
                        &*call_context,
                        kiana_durable::harness::types::NestedToolOptions {
                            key: Some("warm".to_owned()),
                            progress: None,
                        },
                    )
                    .await?;
                // TS 的 `void api.executeTool(...)`：调用同步登记 admission，返回的 future
                // 被丢弃/派生。结算必须等这个 admission 提交，再中止它创建的嵌套任务。
                let late = Arc::clone(&api).execute_tool(
                    "hang",
                    JsonObject::new(),
                    &*call_context,
                    kiana_durable::harness::types::NestedToolOptions {
                        key: Some("late".to_owned()),
                        progress: None,
                    },
                );
                drop(tokio::spawn(late));
                Ok(ToolExecutionResult::default())
            })
        }),
    );
    setup
        .faux
        .set_responses(vec![calls(&[("batch", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);

    let tasks = tool_tasks(&harness).await;
    let late = tasks
        .iter()
        .find(|task| {
            task.input.get("kind").and_then(JsonValue::as_str) == Some("nested")
                && task
                    .input
                    .get("call")
                    .and_then(|call| call.get("name"))
                    .and_then(JsonValue::as_str)
                    == Some("hang")
        })
        .expect("the fire-and-forget nested call exists");
    assert!(
        matches!(
            &late.state,
            kiana_durable::types::TaskState::Terminal {
                outcome: kiana_durable::types::TaskOutcome::Aborted { .. }
            }
        ),
        "state: {:?}",
        late.state
    );
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// harness-nested-tools.test.ts: 并发 admission
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn aborts_every_left_running_nested_call_before_waiting_for_any() {
    let setup = ChatSetup::new();
    let started_a = Arc::new(task_support::Deferred::new());
    let started_b = Arc::new(task_support::Deferred::new());
    let b_aborted = Arc::new(task_support::Deferred::new());
    // a 只在 b 被要求停止后返回。
    let started_a_for_a = Arc::clone(&started_a);
    let b_aborted_for_a = Arc::clone(&b_aborted);
    harness_support::add_tool(
        &setup.registry,
        ToolRegistration {
            tool: Tool {
                name: "a".to_owned(),
                description: "The a tool".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
                constrained_sampling: None,
            },
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            structured_output_schema: None,
            callers: None,
            deferred: false,
            output_limits: None,
            execute: Some(Arc::new(
                move |_args: JsonValue,
                      _api: Arc<dyn ToolExecutionApi>,
                      _context: Arc<dyn Context>| {
                    let started = Arc::clone(&started_a_for_a);
                    let b_aborted = Arc::clone(&b_aborted_for_a);
                    Box::pin(async move {
                        started.resolve(());
                        b_aborted.promise().await?;
                        Ok(ToolExecutionResult::default())
                    })
                        as Pin<
                            Box<dyn Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
                        >
                },
            )),
        },
    );
    let started_b_for_b = Arc::clone(&started_b);
    let b_aborted_for_b = Arc::clone(&b_aborted);
    harness_support::add_tool(
        &setup.registry,
        ToolRegistration {
            tool: Tool {
                name: "b".to_owned(),
                description: "The b tool".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
                constrained_sampling: None,
            },
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            structured_output_schema: None,
            callers: None,
            deferred: false,
            output_limits: None,
            execute: Some(Arc::new(
                move |_args: JsonValue,
                      _api: Arc<dyn ToolExecutionApi>,
                      context: Arc<dyn Context>| {
                    let started = Arc::clone(&started_b_for_b);
                    let b_aborted = Arc::clone(&b_aborted_for_b);
                    Box::pin(async move {
                        started.resolve(());
                        match context.abort_signal() {
                            Some(signal) => {
                                let _ = task_support::aborted(&signal).await;
                            }
                            None => return Err("no abort signal".into()),
                        }
                        b_aborted.resolve(());
                        Err(Box::new(std::io::Error::other("aborted")) as BoxError)
                    })
                        as Pin<
                            Box<dyn Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
                        >
                },
            )),
        },
    );
    let started_a_for_batch = Arc::clone(&started_a);
    let started_b_for_batch = Arc::clone(&started_b);
    harness_support::add_tool(
        &setup.registry,
        nested_batch_tool(move |_args, api, call_context| {
            let started_a = Arc::clone(&started_a_for_batch);
            let started_b = Arc::clone(&started_b_for_batch);
            Box::pin(async move {
                // 两个 `void api.executeTool(...)`：登记后不等待返回的 future。
                drop(Arc::clone(&api).execute_tool(
                    "a",
                    JsonObject::new(),
                    &*call_context,
                    kiana_durable::harness::types::NestedToolOptions {
                        key: Some("a".to_owned()),
                        progress: None,
                    },
                ));
                drop(Arc::clone(&api).execute_tool(
                    "b",
                    JsonObject::new(),
                    &*call_context,
                    kiana_durable::harness::types::NestedToolOptions {
                        key: Some("b".to_owned()),
                        progress: None,
                    },
                ));
                started_a.promise().await?;
                started_b.promise().await?;
                Ok(ToolExecutionResult::default())
            })
        }),
    );
    setup
        .faux
        .set_responses(vec![calls(&[("batch", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);

    let tasks = tool_tasks(&harness).await;
    let nested: Vec<&kiana_durable::types::TaskRecord> = tasks
        .iter()
        .filter(|task| task.input.get("kind").and_then(JsonValue::as_str) == Some("nested"))
        .collect();
    assert_eq!(nested.len(), 2, "tasks: {tasks:?}");
    for task in nested {
        assert!(
            matches!(
                &task.state,
                kiana_durable::types::TaskState::Terminal { .. }
            ),
            "state: {:?}",
            task.state
        );
    }
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// harness-nested-tools.test.ts: 抛错的调用者 / 并发同 key
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn aborts_a_running_nested_call_when_its_caller_throws() {
    let setup = ChatSetup::new();
    let running = Arc::new(task_support::Deferred::new());
    let running_for_hang = Arc::clone(&running);
    harness_support::add_tool(
        &setup.registry,
        ToolRegistration {
            tool: Tool {
                name: "hang".to_owned(),
                description: "The hang tool".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
                constrained_sampling: None,
            },
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            structured_output_schema: None,
            callers: None,
            deferred: false,
            output_limits: None,
            execute: Some(Arc::new(
                move |_args: JsonValue,
                      _api: Arc<dyn ToolExecutionApi>,
                      context: Arc<dyn Context>| {
                    let running = Arc::clone(&running_for_hang);
                    Box::pin(async move {
                        running.resolve(());
                        match context.abort_signal() {
                            Some(signal) => Err(task_support::aborted(&signal).await),
                            None => Err("no abort signal".into()),
                        }
                    })
                        as Pin<
                            Box<dyn Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
                        >
                },
            )),
        },
    );
    let running_for_batch = Arc::clone(&running);
    harness_support::add_tool(
        &setup.registry,
        nested_batch_tool(move |_args, api, call_context| {
            let running = Arc::clone(&running_for_batch);
            Box::pin(async move {
                // 不 await 的嵌套调用：调用者抛错后仍要中止它。
                drop(Arc::clone(&api).execute_tool(
                    "hang",
                    JsonObject::new(),
                    &*call_context,
                    Default::default(),
                ));
                running.promise().await?;
                Err(Box::new(std::io::Error::other("caller failed")) as BoxError)
            })
        }),
    );
    setup
        .faux
        .set_responses(vec![calls(&[("batch", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);

    let entries = chat_support::all_entries(&root, &*context()).await;
    let results = results(&entries);
    assert_eq!(results.len(), 1);
    assert!(result_text(&results[0]).contains("caller failed"));
    let tasks = tool_tasks(&harness).await;
    let child = tasks
        .iter()
        .find(|task| task.input.get("kind").and_then(JsonValue::as_str) == Some("nested"))
        .expect("the nested call task");
    assert!(
        matches!(
            &child.state,
            kiana_durable::types::TaskState::Terminal {
                outcome: kiana_durable::types::TaskOutcome::Aborted { .. }
            }
        ),
        "state: {:?}",
        child.state
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn admits_one_nested_call_for_a_key_used_by_concurrent_calls() {
    let setup = ChatSetup::new();
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let runs_for_echo = Arc::clone(&runs);
    harness_support::add_tool(
        &setup.registry,
        tool(
            "echo",
            Arc::new(move |args: JsonValue, _api, _context| {
                runs_for_echo.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let text = args
                    .get("text")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_owned();
                Ok(text_output(&format!("echo {text}")))
            }),
        ),
    );
    let both: Arc<std::sync::Mutex<Vec<kiana_durable::harness::types::NestedToolExecutionResult>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let both_for_batch = Arc::clone(&both);
    harness_support::add_tool(
        &setup.registry,
        nested_batch_tool(move |_args, api, call_context| {
            let both = Arc::clone(&both_for_batch);
            Box::pin(async move {
                let key = || kiana_durable::harness::types::NestedToolOptions {
                    key: Some("k".to_owned()),
                    progress: None,
                };
                let args = || object(&[("text", JsonValue::String("a".to_owned()))]);
                let first = Arc::clone(&api).execute_tool("echo", args(), &*call_context, key());
                let second = Arc::clone(&api).execute_tool("echo", args(), &*call_context, key());
                let (first, second) = futures::future::try_join(first, second).await?;
                both.lock().unwrap().extend([first, second]);
                Ok(ToolExecutionResult::default())
            })
        }),
    );
    setup
        .faux
        .set_responses(vec![calls(&[("batch", empty_args(), "c1")]), done()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let settled = submit_and_wait(&root, "go").await;
    assert_eq!(settled.status, SubmissionStatus::Done);
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    let both = both.lock().unwrap().clone();
    assert_eq!(both.len(), 2);
    for result in &both {
        assert_eq!(
            result
                .structured_output
                .as_ref()
                .and_then(JsonValue::as_str),
            Some("echo a")
        );
    }
    assert_eq!(tool_tasks(&harness).await.len(), 2);
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// harness-submissions.test.ts
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn appends_an_idle_write_and_settles_it_done_without_a_turn() {
    let setup = ChatSetup::new();
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let submission = root
        .submit(
            kiana_durable::harness::types::SubmissionDraft {
                request_id: None,
                kind: kiana_durable::harness::types::SubmissionDraftKind::Write {
                    entry: kiana_durable::types::EntryDraft {
                        key_order: None,
                        kind: "note".to_owned(),
                        model: None,
                        data: Some(serde_json::json!({"text": "x"})),
                        head: None,
                        edits: None,
                    },
                },
            },
            &*context(),
        )
        .await
        .expect("submit the write");
    let settled = submission.wait(&*context()).await.expect("settled");
    assert_eq!(settled.status, SubmissionStatus::Done);
    assert_eq!(settled.kind, kiana_durable::types::SubmissionKind::Write);
    assert_eq!(settled.conversation_id, root.id());
    let entry = settled.entry.expect("the appended entry");
    let entries = chat_support::all_entries(&root, &*context()).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, entry);
    assert_eq!(entries[0].kind, "note");
    assert_eq!(
        entries[0].data.clone().unwrap_or(JsonValue::Null),
        serde_json::json!({"text": "x"})
    );
    let live = harness
        .snapshot(
            &kiana_durable::types::DocTarget::conversation(
                &kiana_durable::harness::live::LIVE_DOC,
                root.id(),
            ),
            &*context(),
        )
        .await
        .expect("live snapshot")
        .unwrap_or_default();
    assert_eq!(live, JsonObject::new());
    let tasks = harness
        .commit(
            &ScanTasksChange {
                conversation_id: Some(root.id()),
                kind: None,
                limit: 10,
            },
            &*context(),
            Default::default(),
        )
        .await
        .expect("tasks")
        .downcast::<Vec<kiana_durable::types::TaskRecord>>()
        .map(|tasks| *tasks)
        .expect("records");
    assert!(tasks.is_empty());
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn places_idle_input_and_rejects_busy_input_with_when_busy_reject_without_writing() {
    let setup = ChatSetup::new();
    let busy = chat_support::unanswered();
    setup.faux.set_responses(vec![busy.step.clone()]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    let draft: InputSubmissionDraft = harness_support::text_input("hi");
    let submission = root
        .submit(draft.into_draft(), &*context())
        .await
        .expect("submit");
    busy.reached().await;
    let record = submission.status(&*context()).await.expect("status");
    assert_eq!(record.status, SubmissionStatus::Placed);
    let entry_id = record.entry.expect("the placed entry");
    let entries = chat_support::all_entries(&root, &*context()).await;
    let entry = entries
        .iter()
        .find(|entry| entry.id == entry_id)
        .expect("the user entry");
    assert_eq!(tests_entry_text(entry).as_deref(), Some("hi"));
    let live = harness
        .snapshot(
            &kiana_durable::types::DocTarget::conversation(
                &kiana_durable::harness::live::LIVE_DOC,
                root.id(),
            ),
            &*context(),
        )
        .await
        .expect("live snapshot")
        .expect("the live document");
    let run = live.get("run").expect("a running generation");
    let submission_id = kiana_durable::harness::types::Submission::id(&*submission).get();
    assert_eq!(
        run.get("inputs").cloned().unwrap_or(JsonValue::Null),
        serde_json::json!([submission_id])
    );
    let task_id = run
        .get("taskId")
        .and_then(JsonValue::as_i64)
        .expect("the run task");
    let task = harness
        .get_task(kiana_durable::id_from_number(task_id), &*context())
        .await
        .expect("task lookup")
        .expect("the task record");
    assert_eq!(task.kind, "kiana.generation");

    // whenBusy: reject 的提交被拒绝，且什么都不写。
    let mut rejected_draft: InputSubmissionDraft = harness_support::text_input("again");
    rejected_draft.when_busy = Some(kiana_durable::harness::types::WhenBusy::Reject);
    let error = match root.submit(rejected_draft.into_draft(), &*context()).await {
        Ok(_) => panic!("the busy input is rejected"),
        Err(error) => error,
    };
    let busy_error = error
        .downcast_ref::<kiana_durable::types::ConversationBusy>()
        .unwrap_or_else(|| panic!("a ConversationBusy: {error:?}"));
    assert_eq!(busy_error.conversation_id, root.id());
    assert_eq!(
        error.to_string(),
        format!("Conversation {} is busy", root.id().get())
    );
    harness.close(&*context()).await;
}

/// 条目第一条模型消息的文本（用户条目）。
fn tests_entry_text(entry: &EntryRecord) -> Option<String> {
    entry
        .model
        .as_ref()
        .and_then(|model| model.first())
        .and_then(text_of_message)
}
