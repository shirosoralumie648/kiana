//! `test/harness-failed-run-inbox.test.ts` 的 Rust 端口（可移植的持久结果）。
//!
//! TS 测试通过 `Harness` 的 `afterFailedRun` 策略驱动失败运行；运行循环属于其他单元。这里端口
//! 策略对 inbox 的持久效果：默认什么都不做、`continue` 让下一次运行放置排队项、`withdraw` 只把
//! 排队输入落定为 `run_failed` 并保留排队写入。

mod support;

use std::sync::Arc;

use kiana_ai::{Message, UserContent, UserMessage, UserRole};
use kiana_chord::context::BACKGROUND_CONTEXT;
use kiana_durable::harness::inbox::{
    BoundaryAt, INBOX_DOC, InboxItem, InboxState, InputMode, QueueModes, apply_boundary,
    prepare_boundary, withdraw_queued_inputs,
};
use kiana_durable::harness::types::QueueMode;
use kiana_durable::session::SessionImpl;
use kiana_durable::types::{
    DocDraft, DocTarget, EntryDraft, JsonValue, SubmissionCreate, SubmissionId, SubmissionKind,
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

    async fn enqueue_input(&self, text: &str, mode: InputMode) -> SubmissionId {
        let conversation = self.conversation;
        let content = text.to_owned();
        commit(&self.session, move |transaction| {
            let content = content.clone();
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
                    entry: None,
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

    async fn enqueue_write(&self, draft: EntryDraft) -> SubmissionId {
        let conversation = self.conversation;
        commit(&self.session, move |transaction| {
            let draft = draft.clone();
            Box::pin(async move {
                let record = transaction
                    .create_submission(SubmissionCreate {
                        trace: None,
                        conversation_id: conversation,
                        request_id: None,
                        kind: SubmissionKind::Write,
                        status: SubmissionStatus::Queued,
                        entry: None,
                        answer: None,
                        reason: None,
                        detail: None,
                    })
                    .await?;
                let target = DocTarget::conversation(&INBOX_DOC, conversation);
                let mut draft_doc = transaction.doc(&target).await?;
                let mut items = read_items(&draft_doc)?;
                items.push(InboxItem::Write {
                    id: record.id,
                    entry: draft,
                });
                draft_doc
                    .value_mut()
                    .insert("items".to_owned(), serde_json::to_value(&items)?);
                Ok(record.id)
            })
        })
        .await
        .expect("write enqueues")
    }

    async fn withdraw(&self, reason: Option<&str>) {
        let conversation = self.conversation;
        let reason = reason.map(str::to_owned);
        commit(&self.session, move |transaction| {
            let reason = reason.clone();
            Box::pin(async move {
                withdraw_queued_inputs(transaction, conversation, reason.as_deref()).await
            })
        })
        .await
        .expect("withdraw succeeds");
    }

    async fn boundary(&self) {
        let conversation = self.conversation;
        commit(&self.session, move |transaction| {
            Box::pin(async move {
                let modes = QueueModes {
                    steering_mode: QueueMode::OneAtATime,
                    follow_up_mode: QueueMode::OneAtATime,
                };
                let mut boundary = prepare_boundary(transaction, conversation, modes).await?;
                apply_boundary(transaction, &mut boundary, BoundaryAt::Final, 7).await
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

fn note(text: &str) -> EntryDraft {
    EntryDraft {
        key_order: None,
        kind: "test.note".to_owned(),
        model: Some(vec![Message::User(UserMessage {
            role: UserRole::default(),
            content: UserContent::Text(text.to_owned()),
            timestamp: 1,
        })]),
        data: None,
        head: None,
        edits: None,
    }
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn queued_inputs_stay_queued_by_default() {
    let fixture = Fixture::open().await;
    let queued = fixture
        .enqueue_input("Email me the summary", InputMode::FollowUp)
        .await;
    // 失败的运行不触碰 inbox：没有下一个边界，队列与状态都不变。
    let items = fixture.items().await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id(), queued);
    assert_eq!(
        fixture.submission(queued).await.status,
        SubmissionStatus::Queued
    );
    assert!(fixture.entries().await.is_empty());
}

#[tokio::test]
async fn continue_places_queued_items_with_the_next_run() {
    let fixture = Fixture::open().await;
    let queued = fixture
        .enqueue_input("Email me the summary", InputMode::FollowUp)
        .await;
    let write = fixture.enqueue_write(note("a note the model sees")).await;
    // `continue` 用一个最终边界把队列交给下一次运行：写入先落地，输入随后。
    fixture.boundary().await;
    assert!(fixture.items().await.is_empty());
    let write_record = fixture.submission(write).await;
    assert_eq!(write_record.status, SubmissionStatus::Done);
    let queued_record = fixture.submission(queued).await;
    assert_eq!(queued_record.status, SubmissionStatus::Placed);
    let entries = fixture.entries().await;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].kind, "test.note");
    assert_eq!(entries[1].kind, "kiana.user");
}

#[tokio::test]
async fn withdraw_settles_queued_inputs_as_run_failed_and_keeps_queued_writes() {
    let fixture = Fixture::open().await;
    let queued = fixture
        .enqueue_input("Email me the summary", InputMode::FollowUp)
        .await;
    let write = fixture.enqueue_write(note("a note the model sees")).await;
    fixture.withdraw(Some("run_failed")).await;
    let queued_record = fixture.submission(queued).await;
    assert_eq!(queued_record.status, SubmissionStatus::Unanswered);
    assert_eq!(queued_record.reason.as_deref(), Some("run_failed"));
    assert_eq!(
        fixture.submission(write).await.status,
        SubmissionStatus::Queued
    );
    let items = fixture.items().await;
    assert_eq!(items.len(), 1);
    assert!(items[0].is_write());
    assert_eq!(items[0].id(), write);
    // 写入不因撤回而落地，也不产生任何条目。
    assert!(fixture.entries().await.is_empty());
}

#[tokio::test]
async fn withdraw_writes_nothing_to_the_transcript() {
    let fixture = Fixture::open().await;
    fixture
        .enqueue_input("Analyze this report", InputMode::FollowUp)
        .await;
    fixture.withdraw(Some("run_failed")).await;
    assert!(fixture.items().await.is_empty());
    assert!(fixture.entries().await.is_empty());
}
