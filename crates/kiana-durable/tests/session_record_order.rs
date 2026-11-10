//! 持久记录的键序（TS 构造序）。
//!
//! 回归：`Tx.#appendEntry` 用 `copyJson({ ...rest, id, conversationId, head, byTaskId })`
//! 构造记录，键序来自调用点的对象字面量；`classify` 用赋值把 `thinkingLevel` 插到消息末尾；
//! `applySubmissionChange` 的展开语义把新键追加到记录末尾。条目与提交记录因此携带隐藏的
//! `key_order` 提示，落盘（JSONL/SQLite 都走 serde）必须写出 TS 的字节序。

mod support;

use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, ModelThinkingLevel,
    ProviderId, Usage, UserMessage, UserRole,
};
use kiana_durable::types::{
    EntryDraft, EntryHead, EntryId, JsonValue, SubmissionCreate, SubmissionId, SubmissionKind,
    SubmissionStatus, id_from_number,
};
use kiana_durable::{AssistantEntry, CompactionEntry, Storage, TypedEntryTx, UserEntry};
use serde_json::json;

use support::{commit, create_conversation, open_test_session};

fn keys(value: &serde_json::Value) -> Vec<&str> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect()
}

fn user_message(text: &str, timestamp: i64) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text(text.to_owned()),
        timestamp,
    })
}

#[tokio::test]
async fn bare_draft_entries_keep_the_draft_literal_key_order() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let entry = commit(&session, move |tx| {
        Box::pin(async move {
            // TS `placeSummary`：`{ kind, head, model, data }`。
            let draft = EntryDraft {
                kind: CompactionEntry.kind.to_string(),
                model: Some(vec![user_message("summary", 7)]),
                data: Some(json!({"reason": "manual"})),
                head: Some(EntryHead::Id(id_from_number(1))),
                edits: None,
                key_order: Some(
                    ["kind", "head", "model", "data"]
                        .iter()
                        .map(|key| (*key).to_owned())
                        .collect(),
                ),
            };
            tx.append_entry(conversation_id, draft).await
        })
    })
    .await
    .expect("the commit succeeds");

    let value = serde_json::to_value(&entry).expect("serializes");
    assert_eq!(
        keys(&value),
        ["kind", "model", "data", "id", "conversationId", "head"]
    );
    assert_eq!(value["kind"], json!("kiana.compaction"));
    assert_eq!(value["data"], json!({"reason": "manual"}));
}

#[tokio::test]
async fn bare_input_entries_put_data_before_the_model_message() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let entry = commit(&session, move |tx| {
        Box::pin(async move {
            // TS `appendInput`：`{ ...pickInputEntry(entry), model }` → `{ kind, data, model }`。
            let draft = EntryDraft {
                kind: "custom.input".to_owned(),
                model: Some(vec![user_message("hi", 3)]),
                data: Some(json!({"n": 1})),
                head: None,
                edits: None,
                key_order: Some(
                    ["kind", "data", "model"]
                        .iter()
                        .map(|key| (*key).to_owned())
                        .collect(),
                ),
            };
            tx.append_entry(conversation_id, draft).await
        })
    })
    .await
    .expect("the commit succeeds");

    let value = serde_json::to_value(&entry).expect("serializes");
    assert_eq!(
        keys(&value),
        ["kind", "data", "model", "id", "conversationId"]
    );
}

#[tokio::test]
async fn typed_entries_keep_the_typed_key_order() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let entry = commit(&session, move |tx| {
        Box::pin(async move {
            tx.append_entry_typed(
                &UserEntry,
                conversation_id,
                kiana_durable::types::TypedEntryDraft::<kiana_durable::types::NoData> {
                    model: Some(vec![user_message("hi", 5)]),
                    data: kiana_durable::types::NoData,
                    head: None,
                    edits: None,
                },
            )
            .await
        })
    })
    .await
    .expect("the commit succeeds");

    let value = serde_json::to_value(&entry.record).expect("serializes");
    assert_eq!(keys(&value), ["model", "kind", "id", "conversationId"]);
}

#[tokio::test]
async fn assistant_entries_serialize_thinking_level_last() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let message = Message::Assistant(AssistantMessage {
        role: AssistantRole::default(),
        content: vec![AssistantContentBlock::Text(kiana_ai::types::TextContent {
            kind_text: Default::default(),
            text: "done".to_owned(),
            text_signature: None,
        })],
        api: Default::default(),
        provider: ProviderId::default(),
        model: "faux-1".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        // `classify` 在 provider 构造之后赋值：JSON.stringify 把它放在末尾。
        thinking_level: Some(ModelThinkingLevel::Off),
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: kiana_ai::types::StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 11,
        duration_ms: Some(2.0),
    });
    let entry = commit(&session, move |tx| {
        Box::pin(async move {
            tx.append_entry_typed(
                &AssistantEntry,
                conversation_id,
                kiana_durable::types::TypedEntryDraft::<kiana_durable::types::NoData> {
                    model: Some(vec![message]),
                    data: kiana_durable::types::NoData,
                    head: None,
                    edits: None,
                },
            )
            .await
        })
    })
    .await
    .expect("the commit succeeds");

    let value = serde_json::to_value(&entry.record).expect("serializes");
    let message = &value["model"][0];
    let message_keys = keys(message);
    assert_eq!(message_keys.last(), Some(&"thinkingLevel"));
    assert_eq!(message_keys[message_keys.len() - 2], "durationMs");
    assert_eq!(message_keys[message_keys.len() - 3], "timestamp");
    assert!(!message_keys[..message_keys.len() - 3].contains(&"thinkingLevel"));
}

#[tokio::test]
async fn queued_submissions_placed_later_append_entry_after_id() {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let id: SubmissionId = commit(&session, move |tx| {
        Box::pin(async move {
            let queued = tx
                .create_submission(SubmissionCreate {
                    trace: None,
                    conversation_id,
                    request_id: None,
                    kind: SubmissionKind::Write,
                    status: SubmissionStatus::Queued,
                    entry: None,
                    answer: None,
                    reason: None,
                    detail: None,
                })
                .await?;
            tx.place_submission(queued.id, id_from_number(9));
            Ok(queued.id)
        })
    })
    .await
    .expect("the commit succeeds");

    let record = storage
        .submission(id, &**kiana_chord::context::BACKGROUND_CONTEXT)
        .await
        .expect("reads")
        .expect("the submission exists");
    let value = serde_json::to_value(&record).expect("serializes");
    assert_eq!(
        keys(&value),
        ["conversationId", "type", "status", "id", "entry"]
    );
    assert_eq!(value["status"], json!("done"));
}

#[tokio::test]
async fn directly_created_placed_submissions_put_entry_before_id() {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let id: SubmissionId = commit(&session, move |tx| {
        Box::pin(async move {
            let placed = tx
                .create_submission(SubmissionCreate {
                    trace: None,
                    conversation_id,
                    request_id: None,
                    kind: SubmissionKind::Input,
                    status: SubmissionStatus::Placed,
                    entry: Some(id_from_number(9)),
                    answer: None,
                    reason: None,
                    detail: None,
                })
                .await?;
            Ok(placed.id)
        })
    })
    .await
    .expect("the commit succeeds");

    let record = storage
        .submission(id, &**kiana_chord::context::BACKGROUND_CONTEXT)
        .await
        .expect("reads")
        .expect("the submission exists");
    let value = serde_json::to_value(&record).expect("serializes");
    assert_eq!(
        keys(&value),
        ["conversationId", "type", "status", "entry", "id"]
    );
}

#[tokio::test]
async fn settled_submissions_append_their_terminal_fields() {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let id: SubmissionId = commit(&session, move |tx| {
        Box::pin(async move {
            let placed = tx
                .create_submission(SubmissionCreate {
                    trace: None,
                    conversation_id,
                    request_id: None,
                    kind: SubmissionKind::Input,
                    status: SubmissionStatus::Placed,
                    entry: Some(id_from_number(9)),
                    answer: None,
                    reason: None,
                    detail: None,
                })
                .await?;
            tx.settle_submission(
                placed.id,
                kiana_durable::types::SubmissionSettlement::Done {
                    answer: id_from_number(10),
                },
            );
            Ok(placed.id)
        })
    })
    .await
    .expect("the commit succeeds");

    let record = storage
        .submission(id, &**kiana_chord::context::BACKGROUND_CONTEXT)
        .await
        .expect("reads")
        .expect("the submission exists");
    let value = serde_json::to_value(&record).expect("serializes");
    assert_eq!(
        keys(&value),
        ["conversationId", "type", "status", "entry", "id", "answer"]
    );
}

#[tokio::test]
async fn entry_records_round_trip_their_stored_key_order() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let entry = commit(&session, move |tx| {
        Box::pin(async move {
            let draft = EntryDraft {
                kind: "kiana.compaction".to_owned(),
                model: None,
                data: Some(JsonValue::Null),
                head: None,
                edits: None,
                key_order: None,
            };
            tx.append_entry(conversation_id, draft).await
        })
    })
    .await
    .expect("the commit succeeds");

    let bytes = serde_json::to_string(&entry).expect("serializes");
    let reread: kiana_durable::types::EntryRecord =
        serde_json::from_str(&bytes).expect("deserializes");
    let again = serde_json::to_string(&reread).expect("serializes");
    assert_eq!(bytes, again);
    let entry_id: EntryId = entry.id;
    assert_eq!(reread.id, entry_id);
}
