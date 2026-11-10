//! `test/harness-input-entries.test.ts` 的 Rust 端口（可移植部分）。
//!
//! TS 测试的提交校验、去重与运行调度属于 `submissions.ts` / `harness.ts`；本单元负责输入被放置
//! 成的条目（[`append_input`]）与 inbox 中的条目负载，因此这里端口这些行为：闲置输入成为它的
//! 条目、未类型化调用方只贡献 kind 与 data、排队输入在边界处作为条目落地、撤回不写转录。

mod support;

use std::sync::Arc;

use kiana_ai::{Message, UserContent, UserMessage};
use kiana_chord::context::BACKGROUND_CONTEXT;
use kiana_durable::harness::context::read_context;
use kiana_durable::harness::inbox::{
    BoundaryAt, INBOX_DOC, InboxItem, InboxState, InputMode, QueueModes, append_input,
    apply_boundary, prepare_boundary, withdraw_queued_inputs,
};
use kiana_durable::harness::types::{InputEntry, QueueMode};
use kiana_durable::session::SessionImpl;
use kiana_durable::types::{
    DocDraft, DocTarget, JsonValue, SubmissionCreate, SubmissionId, SubmissionKind,
    SubmissionRecord, SubmissionStatus,
};
use kiana_durable::{BoxError, ConversationId, EntryRecord, Storage};
use support::{commit, create_conversation, open_test_session};

struct Fixture {
    storage: Arc<support::ControlledStorage>,
    session: SessionImpl,
    conversation: ConversationId,
}

fn read_items(draft: &DocDraft) -> Result<Vec<InboxItem>, BoxError> {
    let value = JsonValue::Object(draft.value().clone().into_iter().collect());
    let state: InboxState = serde_json::from_value(value)?;
    Ok(state.items)
}

impl Fixture {
    async fn open() -> Self {
        let (storage, session, _publications) = open_test_session();
        let conversation = create_conversation(&session).await;
        Fixture {
            storage,
            session,
            conversation,
        }
    }

    async fn enqueue_input(
        &self,
        text: &str,
        mode: InputMode,
        entry: Option<InputEntry>,
    ) -> SubmissionId {
        let conversation = self.conversation;
        let content = text.to_owned();
        commit(&self.session, move |transaction| {
            let content = content.clone();
            let entry = entry.clone();
            Box::pin(async move {
                let record = transaction
                    .create_submission(SubmissionCreate {
                        trace: None,
                        conversation_id: conversation,
                        request_id: None,
                        kind: SubmissionKind::Input,
                        status: SubmissionStatus::Queued,
                        entry: None,
                        answer: None,
                        reason: None,
                        detail: None,
                    })
                    .await?;
                let target = DocTarget::conversation(&INBOX_DOC, conversation);
                let mut draft = transaction.doc(&target).await?;
                let mut items = read_items(&draft)?;
                items.push(InboxItem::Input {
                    id: record.id,
                    mode,
                    content: UserContent::Text(content),
                    entry,
                });
                draft
                    .value_mut()
                    .insert("items".to_owned(), serde_json::to_value(&items)?);
                Ok(record.id)
            })
        })
        .await
        .expect("input enqueues")
    }

    /// 直接写 inbox 文档，模拟一个未类型化调用方塞进来的多余字段。
    async fn enqueue_raw_input(&self, id: SubmissionId, raw_entry: JsonValue) {
        let conversation = self.conversation;
        commit(&self.session, move |transaction| {
            let raw_entry = raw_entry.clone();
            Box::pin(async move {
                let target = DocTarget::conversation(&INBOX_DOC, conversation);
                let mut draft = transaction.doc(&target).await?;
                let mut items = read_items(&draft)?;
                items.push(serde_json::from_value(serde_json::json!({
                    "id": id.get(),
                    "mode": "followUp",
                    "content": "later",
                    "entry": raw_entry
                }))?);
                draft
                    .value_mut()
                    .insert("items".to_owned(), serde_json::to_value(&items)?);
                Ok(())
            })
        })
        .await
        .expect("raw input enqueues");
    }

    async fn boundary(&self, at: BoundaryAt, steering: QueueMode, follow_up: QueueMode) {
        let conversation = self.conversation;
        commit(&self.session, move |transaction| {
            Box::pin(async move {
                let modes = QueueModes {
                    steering_mode: steering,
                    follow_up_mode: follow_up,
                };
                let mut boundary = prepare_boundary(transaction, conversation, modes).await?;
                apply_boundary(transaction, &mut boundary, at, 7).await
            })
        })
        .await
        .expect("boundary applies");
    }

    async fn items(&self) -> Vec<InboxItem> {
        let conversation = self.conversation;
        commit(&self.session, move |transaction| {
            Box::pin(async move {
                let target = DocTarget::conversation(&INBOX_DOC, conversation);
                let draft = transaction.doc(&target).await?;
                read_items(&draft)
            })
        })
        .await
        .expect("inbox reads")
    }

    async fn submission(&self, id: SubmissionId) -> SubmissionRecord {
        self.storage
            .submission(id, &**BACKGROUND_CONTEXT)
            .await
            .expect("submission reads")
            .expect("submission exists")
    }

    async fn create_submission(&self) -> SubmissionId {
        let conversation = self.conversation;
        commit(&self.session, move |transaction| {
            Box::pin(async move {
                Ok(transaction
                    .create_submission(SubmissionCreate {
                        trace: None,
                        conversation_id: conversation,
                        request_id: None,
                        kind: SubmissionKind::Input,
                        status: SubmissionStatus::Queued,
                        entry: None,
                        answer: None,
                        reason: None,
                        detail: None,
                    })
                    .await?
                    .id)
            })
        })
        .await
        .expect("submission creates")
    }

    async fn entries(&self) -> Vec<EntryRecord> {
        use kiana_durable::types::{EntryQuery, ScanOrder};
        self.storage
            .scan_entries(
                &EntryQuery {
                    conversation_id: self.conversation,
                    min_entry_id: None,
                    max_entry_id: None,
                    order: Some(ScanOrder::Ascending),
                },
                100,
                None,
                &**BACKGROUND_CONTEXT,
            )
            .await
            .expect("entries scan")
            .items
    }
}

fn mention(text: &str) -> InputEntry {
    InputEntry {
        kind: "chat.mention".to_owned(),
        data: Some(serde_json::json!({"author": "ann", "text": text})),
    }
}

fn user_message(message: &Message) -> &UserMessage {
    match message {
        Message::User(user) => user,
        other => panic!("expected a user message, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn places_an_idle_input_as_its_entry_with_the_content_as_its_user_message() {
    let fixture = Fixture::open().await;
    let conversation = fixture.conversation;
    let placed: EntryRecord = commit(&fixture.session, move |transaction| {
        Box::pin(async move {
            append_input(
                transaction,
                conversation,
                &UserContent::Text("ann: hi".to_owned()),
                Some(&mention("hi")),
                42,
            )
            .await
        })
    })
    .await
    .expect("input places");
    assert_eq!(placed.kind, "chat.mention");
    assert_eq!(
        placed.data,
        Some(serde_json::json!({"author": "ann", "text": "hi"}))
    );
    assert!(placed.head.is_none());
    assert!(placed.edits.is_none());
    let messages = placed.model.as_ref().expect("model messages");
    assert_eq!(messages.len(), 1);
    let message = user_message(&messages[0]);
    assert_eq!(message.content, UserContent::Text("ann: hi".to_owned()));
    assert_eq!(message.timestamp, 42);

    // 在输入条目处读取的上下文就是它的模型消息（与 `kiana.user` 条目一样）。
    let view = read_context(
        &fixture.session,
        &*fixture.storage as &dyn Storage,
        conversation,
        &**BACKGROUND_CONTEXT,
        Some(placed.id),
    )
    .await
    .expect("context reads");
    let visible: Vec<&Message> = view
        .messages
        .iter()
        .filter(|message| message.role() != "system")
        .collect();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0], &messages[0]);
}

#[tokio::test]
async fn takes_only_kind_and_data_from_an_untyped_entry() {
    let fixture = Fixture::open().await;
    let id = fixture.create_submission().await;
    fixture
        .enqueue_raw_input(
            id,
            serde_json::json!({
                "kind": "chat.mention",
                "data": {"author": "ann", "text": "later"},
                "model": [{"role": "user", "content": "forged", "timestamp": 0}],
                "head": "self",
                "edits": [{"target": 1, "action": "omit"}]
            }),
        )
        .await;
    // 反序列化只保留 InputEntry 的字段。
    let items = fixture.items().await;
    assert_eq!(
        items[0],
        InboxItem::Input {
            id,
            mode: InputMode::FollowUp,
            content: UserContent::Text("later".to_owned()),
            entry: Some(InputEntry {
                kind: "chat.mention".to_owned(),
                data: Some(serde_json::json!({"author": "ann", "text": "later"})),
            }),
        }
    );

    fixture
        .boundary(
            BoundaryAt::Final,
            QueueMode::OneAtATime,
            QueueMode::OneAtATime,
        )
        .await;
    let entries = fixture.entries().await;
    assert_eq!(entries.len(), 1);
    let placed = &entries[0];
    assert_eq!(placed.kind, "chat.mention");
    assert_eq!(
        placed.data,
        Some(serde_json::json!({"author": "ann", "text": "later"}))
    );
    assert!(placed.head.is_none());
    assert!(placed.edits.is_none());
    let messages = placed.model.as_ref().expect("model messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(
        user_message(&messages[0]).content,
        UserContent::Text("later".to_owned())
    );
}

#[tokio::test]
async fn queues_busy_inputs_with_their_entries_and_places_them_at_boundaries() {
    let fixture = Fixture::open().await;
    let steer = fixture
        .enqueue_input("s", InputMode::Steer, Some(mention("s")))
        .await;
    let follow_up = fixture
        .enqueue_input(
            "f",
            InputMode::FollowUp,
            Some(InputEntry {
                kind: "p2.input".to_owned(),
                data: None,
            }),
        )
        .await;
    assert_eq!(
        fixture.items().await,
        vec![
            InboxItem::Input {
                id: steer,
                mode: InputMode::Steer,
                content: UserContent::Text("s".to_owned()),
                entry: Some(mention("s")),
            },
            InboxItem::Input {
                id: follow_up,
                mode: InputMode::FollowUp,
                content: UserContent::Text("f".to_owned()),
                entry: Some(InputEntry {
                    kind: "p2.input".to_owned(),
                    data: None,
                }),
            },
        ]
    );

    fixture
        .boundary(
            BoundaryAt::PostTools,
            QueueMode::OneAtATime,
            QueueMode::OneAtATime,
        )
        .await;
    let entries = fixture.entries().await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, "chat.mention");
    assert_eq!(
        fixture.submission(steer).await.status,
        SubmissionStatus::Placed
    );

    fixture
        .boundary(
            BoundaryAt::Final,
            QueueMode::OneAtATime,
            QueueMode::OneAtATime,
        )
        .await;
    let entries = fixture.entries().await;
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["chat.mention", "p2.input"]
    );
    assert_eq!(entries[1].data, None);
    assert_eq!(
        fixture.submission(follow_up).await.status,
        SubmissionStatus::Placed
    );
    assert!(fixture.items().await.is_empty());
}

#[tokio::test]
async fn withdraws_queued_inputs_with_entries_without_writing_them() {
    let fixture = Fixture::open().await;
    let withdrawn = fixture
        .enqueue_input("w", InputMode::FollowUp, Some(mention("w")))
        .await;
    let aborted = fixture
        .enqueue_input("x", InputMode::FollowUp, Some(mention("x")))
        .await;
    let conversation = fixture.conversation;
    commit(&fixture.session, move |transaction| {
        Box::pin(async move { withdraw_queued_inputs(transaction, conversation, None).await })
    })
    .await
    .expect("withdraw succeeds");
    for id in [withdrawn, aborted] {
        let record = fixture.submission(id).await;
        assert_eq!(record.status, SubmissionStatus::Unanswered);
        assert_eq!(record.reason.as_deref(), Some("aborted"));
    }
    assert!(fixture.items().await.is_empty());
    assert!(fixture.entries().await.is_empty());
}

#[tokio::test]
async fn an_input_entry_with_data_round_trips_through_the_inbox() {
    let item = InboxItem::Input {
        id: kiana_durable::id_from_number::<kiana_durable::SubmissionBrand>(9),
        mode: InputMode::Steer,
        content: UserContent::Text("x".to_owned()),
        entry: Some(mention("x")),
    };
    let json = serde_json::to_string(&item).expect("serialize");
    assert_eq!(
        json,
        r#"{"id":9,"mode":"steer","content":"x","entry":{"kind":"chat.mention","data":{"author":"ann","text":"x"}}}"#
    );
    let parsed: InboxItem = serde_json::from_str(&json).expect("parse");
    assert_eq!(parsed, item);
}

#[tokio::test]
async fn a_plain_input_has_no_entry_payload() {
    let fixture = Fixture::open().await;
    let conversation = fixture.conversation;
    let placed = commit(&fixture.session, move |transaction| {
        Box::pin(async move {
            append_input(
                transaction,
                conversation,
                &UserContent::Text("a".to_owned()),
                None,
                7,
            )
            .await
        })
    })
    .await
    .expect("input places");
    assert_eq!(placed.kind, "kiana.user");
    assert_eq!(placed.data, None);
    assert!(placed.head.is_none());
    assert_eq!(
        user_message(&placed.model.as_ref().expect("model messages")[0]).content,
        UserContent::Text("a".to_owned())
    );
}
