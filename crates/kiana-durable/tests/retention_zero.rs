//! 验收项 `KIANA_DURABLE_DOCUMENT_RETENTION_MS=0` 的等价模式（`spec/durable-harness.md` §8.5）：
//! TS 的 vitest 夹具 `test/setup-document-retention.ts` 在 `KIANA_DURABLE_DOCUMENT_RETENTION_MS=0`
//! 时对所有测试文件调用 `setDefaultDocumentRetention(0)`，整套测试再跑一遍。Rust 侧由
//! [`set_default_document_retention`] 提供同一开关；本文件是它的独立测试二进制（进程级开关，
//! 不与其他测试并行共享状态），在缓存全部失效、每次读取都从存储重新加载的路径上重跑核心流程。

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

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
    Conversation as ConversationTrait, InputSubmissionDraft, ToolExecutionApi, ToolExecutionResult,
    ToolRegistration,
};
use kiana_durable::session::set_default_document_retention;
use kiana_durable::types::{
    BoxError, EntryRecord, JsonObject, JsonValue, Storage, SubmissionStatus,
};

use chat_support::{ChatSetup, context, open_chat};
use support::MemoryStorage;

fn tool(name: &str, text: String) -> ToolRegistration {
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
        execute: Some(Arc::new(
            move |_args: JsonValue, _api: Arc<dyn ToolExecutionApi>, _context: Arc<dyn Context>| {
                let text = text.clone();
                Box::pin(async move {
                    Ok(ToolExecutionResult {
                        output: Some(vec![kiana_ai::types::UserContentBlock::Text(TextContent {
                            kind_text: TextKind::Text,
                            text,
                            text_signature: None,
                        })]),
                        ..Default::default()
                    })
                })
                    as Pin<Box<dyn Future<Output = Result<ToolExecutionResult, BoxError>> + Send>>
            },
        )),
    }
}

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

fn answer(text: String) -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message(
        faux_text(text),
        FauxAssistantMessageOptions::default(),
    ))
}

fn sized_text(label: &str, tokens: usize) -> String {
    let length = tokens * 4;
    format!(
        "{label} {}",
        "x".repeat(length.saturating_sub(label.len() + 1))
    )
}

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

fn text_of(message: &Message) -> Option<String> {
    chat_support::text_of(Some(message))
}

async fn submit_and_wait(
    root: &Arc<dyn ConversationTrait>,
    text: &str,
) -> kiana_durable::harness::types::SettledSubmissionRecord {
    let draft: InputSubmissionDraft = harness_support::text_input(text);
    root.submit(draft.into_draft(), &*context())
        .await
        .expect("submit")
        .wait(&*context())
        .await
        .expect("settled")
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_a_tool_round_and_a_manual_compaction_with_document_retention_zero() {
    set_default_document_retention(0);

    // 工具轮：每一份文档都不留在缓存里，读取走存储。
    let setup = ChatSetup::new();
    harness_support::add_tool(&setup.registry, tool("echo", "echo hi".to_owned()));
    setup
        .faux
        .set_responses(vec![calls(&[("echo", JsonObject::new(), "c1")]), done()]);
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
    let results = results(&entries);
    assert_eq!(results.len(), 1);
    assert!(!results[0].is_error);
    assert_eq!(result_text(&results[0]), "echo hi");
    harness.close(&*context()).await;

    // 手动压缩：选择范围、摘要请求与放置都在 retention = 0 下重放。
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
        answer(sized_text("a1", 100)),
        answer(sized_text("a2", 100)),
        answer(sized_text("a3", 100)),
        answer("SUMMARY".to_owned()),
    ]);
    let (harness, root) =
        open_chat(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &setup).await;
    for user in [
        sized_text("u1", 100),
        sized_text("u2", 100),
        sized_text("u3", 100),
    ] {
        assert_eq!(
            submit_and_wait(&root, &user).await.status,
            SubmissionStatus::Done
        );
    }
    let before = chat_support::all_entries(&root, &*context()).await;
    let u3 = before
        .iter()
        .find(|entry| {
            entry
                .model
                .as_ref()
                .and_then(|model| model.first())
                .and_then(text_of)
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
    let after = chat_support::all_entries(&root, &*context()).await;
    assert_eq!(&after[..before.len()], &before[..]);
    let marker = after.last().expect("the summary entry");
    assert_eq!(marker.kind, "kiana.compaction");
    assert_eq!(marker.head, Some(u3));
    assert_eq!(
        marker.data.clone().unwrap_or(JsonValue::Null),
        serde_json::json!({"reason": "manual"})
    );
    let marker_text = text_of(&marker.model.as_ref().expect("model")[0]).expect("text");
    assert_eq!(
        marker_text,
        "The conversation history before this point was compacted into the following summary:\n\n<summary>\nSUMMARY\n</summary>"
    );
    let view = root
        .context(
            &*context(),
            kiana_durable::harness::types::ContextViewOptions::default(),
        )
        .await
        .expect("context view");
    let texts: Vec<String> = view.messages.iter().filter_map(text_of).collect();
    assert_eq!(
        texts,
        vec![marker_text, sized_text("u3", 100), sized_text("a3", 100)]
    );
    harness.close(&*context()).await;
}
