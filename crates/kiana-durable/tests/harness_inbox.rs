//! `test/harness-inbox.test.ts` 的 Rust 端口。
//!
//! TS 测试围绕完整的 `Harness`/`Conversation`；本单元只拥有 inbox 本身，因此这里用
//! [`SessionImpl`] 提交与 TS 相同的排队项，再直接调用 [`prepare_boundary`]/[`apply_boundary`]
//! 验证放置顺序、reset、stale 与撤回语义。依赖运行循环（生成、工具、onYield、重开）的用例
//! 留给上层单元。

mod support;

use std::sync::Arc;

use kiana_ai::{Message, UserContent, UserMessage, UserRole};
use kiana_chord::context::BACKGROUND_CONTEXT;
use kiana_durable::harness::inbox::{
    BoundaryAt, BoundaryResult, INBOX_DOC, InboxItem, InboxState, InputMode, QueueModes,
    append_input, apply_boundary, prepare_boundary, remove_inbox_item, withdraw_queued_inputs,
};
use kiana_durable::harness::types::{InputEntry, QueueMode};
use kiana_durable::session::SessionImpl;
use kiana_durable::types::{
    DocDraft, DocTarget, EntryDraft, EntryHead, EntryQuery, JsonValue, ScanOrder, SubmissionCreate,
    SubmissionId, SubmissionKind, SubmissionRecord, SubmissionStatus,
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

    async fn append(&self, draft: EntryDraft) -> EntryRecord {
        let conversation = self.conversation;
        commit(&self.session, move |transaction| {
            let draft = draft.clone();
            Box::pin(async move { transaction.append_entry(conversation, draft).await })
        })
        .await
        .expect("append succeeds")
    }

    async fn append_note(&self, kind: &str) -> EntryRecord {
        self.append(EntryDraft {
            key_order: None,
            kind: kind.to_owned(),
            model: None,
            data: None,
            head: None,
            edits: None,
        })
        .await
    }

    async fn append_reset(&self) -> EntryRecord {
        self.append(EntryDraft {
            key_order: None,
            kind: "kiana.reset".to_owned(),
            model: None,
            data: None,
            head: Some(EntryHead::SelfHead),
            edits: None,
        })
        .await
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

    async fn boundary(
        &self,
        at: BoundaryAt,
        steering_mode: QueueMode,
        follow_up_mode: QueueMode,
    ) -> BoundaryResult {
        let conversation = self.conversation;
        commit(&self.session, move |transaction| {
            Box::pin(async move {
                let modes = QueueModes {
                    steering_mode,
                    follow_up_mode,
                };
                let mut boundary = prepare_boundary(transaction, conversation, modes).await?;
                apply_boundary(transaction, &mut boundary, at, 7).await
            })
        })
        .await
        .expect("boundary applies")
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
        kind: "note".to_owned(),
        model: Some(vec![Message::User(UserMessage {
            role: UserRole::default(),
            content: UserContent::Text(text.to_owned()),
            timestamp: 0,
        })]),
        data: None,
        head: None,
        edits: None,
    }
}

fn user_text(message: &Message) -> Option<&str> {
    match message {
        Message::User(user) => match &user.content {
            UserContent::Text(text) => Some(text),
            UserContent::Blocks(_) => None,
        },
        _ => None,
    }
}

/// 每个条目的 `kind:text`（用户文本）或 `kind`。
fn transcript(entries: &[EntryRecord]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| entry.kind != "kiana.system")
        .map(
            |entry| match entry.model.as_ref().and_then(|messages| messages.first()) {
                Some(message) => match user_text(message) {
                    Some(text) => format!("{}:{}", entry.kind, text),
                    None => entry.kind.clone(),
                },
                None => entry.kind.clone(),
            },
        )
        .collect()
}

fn item_ids(items: &[InboxItem]) -> Vec<SubmissionId> {
    items.iter().map(InboxItem::id).collect()
}

fn one_at_a_time() -> (QueueMode, QueueMode) {
    (QueueMode::OneAtATime, QueueMode::OneAtATime)
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn places_writes_before_user_items_at_the_final_boundary_and_one_follow_up_per_run() {
    let fixture = Fixture::open().await;
    let f1 = fixture.enqueue_input("f1", InputMode::FollowUp, None).await;
    let write = fixture.enqueue_write(note("w")).await;
    let f2 = fixture.enqueue_input("f2", InputMode::FollowUp, None).await;

    let (steering, follow_up) = one_at_a_time();
    assert_eq!(
        fixture
            .boundary(BoundaryAt::PostTools, steering, follow_up)
            .await,
        BoundaryResult {
            users: Vec::new(),
            reset: false,
        }
    );
    assert_eq!(item_ids(&fixture.items().await), vec![f1, f2]);
    let placed = fixture.submission(write).await;
    // 写入落地即完成；输入是被放置、等待回答的。
    assert_eq!(placed.status, SubmissionStatus::Done);
    assert!(placed.entry.is_some());

    assert_eq!(
        fixture
            .boundary(BoundaryAt::Final, steering, follow_up)
            .await,
        BoundaryResult {
            users: vec![f1],
            reset: false,
        }
    );
    assert_eq!(item_ids(&fixture.items().await), vec![f2]);
    assert_eq!(
        fixture
            .boundary(BoundaryAt::Final, steering, follow_up)
            .await,
        BoundaryResult {
            users: vec![f2],
            reset: false,
        }
    );
    assert!(fixture.items().await.is_empty());

    assert_eq!(
        transcript(&fixture.entries().await),
        vec!["note:w", "kiana.user:f1", "kiana.user:f2"]
    );
    let f1_record = fixture.submission(f1).await;
    assert_eq!(f1_record.status, SubmissionStatus::Placed);
    let f2_record = fixture.submission(f2).await;
    assert_eq!(f2_record.status, SubmissionStatus::Placed);
}

#[tokio::test]
async fn places_every_follow_up_in_one_successor_run_when_the_mode_is_all() {
    let fixture = Fixture::open().await;
    let f1 = fixture.enqueue_input("f1", InputMode::FollowUp, None).await;
    let f2 = fixture.enqueue_input("f2", InputMode::FollowUp, None).await;
    assert_eq!(
        fixture
            .boundary(BoundaryAt::Final, QueueMode::OneAtATime, QueueMode::All)
            .await,
        BoundaryResult {
            users: vec![f1, f2],
            reset: false,
        }
    );
    assert!(fixture.items().await.is_empty());
    assert_eq!(
        transcript(&fixture.entries().await),
        vec!["kiana.user:f1", "kiana.user:f2"]
    );
}

#[tokio::test]
async fn adds_steers_at_the_post_tools_boundary_and_holds_follow_ups_for_the_final_one() {
    let fixture = Fixture::open().await;
    let steer = fixture.enqueue_input("s", InputMode::Steer, None).await;
    let follow_up = fixture.enqueue_input("f", InputMode::FollowUp, None).await;
    assert_eq!(
        fixture
            .boundary(
                BoundaryAt::PostTools,
                QueueMode::OneAtATime,
                QueueMode::OneAtATime,
            )
            .await,
        BoundaryResult {
            users: vec![steer],
            reset: false,
        }
    );
    assert_eq!(item_ids(&fixture.items().await), vec![follow_up]);
    assert_eq!(
        fixture
            .boundary(
                BoundaryAt::Final,
                QueueMode::OneAtATime,
                QueueMode::OneAtATime,
            )
            .await,
        BoundaryResult {
            users: vec![follow_up],
            reset: false,
        }
    );
    assert_eq!(
        transcript(&fixture.entries().await),
        vec!["kiana.user:s", "kiana.user:f"]
    );
}

#[tokio::test]
async fn all_steers_join_the_run_at_the_post_tools_boundary_in_id_order() {
    let fixture = Fixture::open().await;
    let s1 = fixture.enqueue_input("s1", InputMode::Steer, None).await;
    let follow_up = fixture.enqueue_input("f", InputMode::FollowUp, None).await;
    let s2 = fixture.enqueue_input("s2", InputMode::Steer, None).await;
    assert_eq!(
        fixture
            .boundary(BoundaryAt::PostTools, QueueMode::All, QueueMode::OneAtATime)
            .await,
        BoundaryResult {
            users: vec![s1, s2],
            reset: false,
        }
    );
    assert_eq!(item_ids(&fixture.items().await), vec![follow_up]);
    assert_eq!(
        transcript(&fixture.entries().await),
        vec!["kiana.user:s1", "kiana.user:s2"]
    );
}

#[tokio::test]
async fn a_queued_reset_ends_the_run_after_tools_and_earlier_follow_ups_run_in_the_new_context() {
    let fixture = Fixture::open().await;
    let follow_up = fixture.enqueue_input("f", InputMode::FollowUp, None).await;
    let reset = fixture
        .enqueue_write(EntryDraft {
            key_order: None,
            kind: "kiana.reset".to_owned(),
            model: None,
            data: None,
            head: Some(EntryHead::SelfHead),
            edits: None,
        })
        .await;
    assert_eq!(
        fixture
            .boundary(
                BoundaryAt::PostTools,
                QueueMode::OneAtATime,
                QueueMode::OneAtATime,
            )
            .await,
        BoundaryResult {
            users: vec![follow_up],
            reset: true,
        }
    );
    assert!(fixture.items().await.is_empty());
    let entries = fixture.entries().await;
    assert_eq!(transcript(&entries), vec!["kiana.reset", "kiana.user:f"]);
    let reset_record = entries
        .iter()
        .find(|entry| entry.kind == "kiana.reset")
        .expect("a reset entry");
    assert_eq!(reset_record.head, Some(reset_record.id));
    assert_eq!(
        fixture.submission(reset).await.status,
        SubmissionStatus::Done
    );
}

#[tokio::test]
async fn a_queued_head_write_is_stale_before_the_active_range_and_kept_inside_it() {
    let fixture = Fixture::open().await;
    let old = fixture.append_note("note").await;
    let reset = fixture.append_reset().await;

    let stale = fixture
        .enqueue_write(EntryDraft {
            key_order: None,
            kind: "summary".to_owned(),
            model: None,
            data: None,
            head: Some(EntryHead::Id(old.id)),
            edits: None,
        })
        .await;
    let fresh = fixture
        .enqueue_write(EntryDraft {
            key_order: None,
            kind: "summary".to_owned(),
            model: None,
            data: None,
            head: Some(EntryHead::Id(reset.id)),
            edits: None,
        })
        .await;
    let (steering, follow_up) = one_at_a_time();
    assert_eq!(
        fixture
            .boundary(BoundaryAt::Final, steering, follow_up)
            .await,
        BoundaryResult {
            users: Vec::new(),
            reset: false,
        }
    );
    assert!(fixture.items().await.is_empty());
    let stale_record = fixture.submission(stale).await;
    assert_eq!(stale_record.status, SubmissionStatus::Unanswered);
    assert_eq!(stale_record.reason.as_deref(), Some("stale"));
    let fresh_record = fixture.submission(fresh).await;
    assert_eq!(fresh_record.status, SubmissionStatus::Done);
    assert!(fresh_record.entry.is_some());

    // 就在活动范围内的目标是新鲜的，即使它比标记本身更旧。
    let inside = fixture.append_note("kiana.user").await;
    let kept = fixture
        .enqueue_write(EntryDraft {
            key_order: None,
            kind: "summary".to_owned(),
            model: None,
            data: None,
            head: Some(EntryHead::Id(inside.id)),
            edits: None,
        })
        .await;
    fixture
        .boundary(BoundaryAt::Final, steering, follow_up)
        .await;
    let kept_record = fixture.submission(kept).await;
    assert_eq!(kept_record.status, SubmissionStatus::Done);
}

#[tokio::test]
async fn a_reset_placed_earlier_in_the_same_boundary_makes_a_later_head_write_stale() {
    let fixture = Fixture::open().await;
    let old = fixture.append_note("note").await;
    // 同一队列里 reset 的 ID 更小：它先落地并把活动范围推到旧条目之后。
    let reset = fixture
        .enqueue_write(EntryDraft {
            key_order: None,
            kind: "kiana.reset".to_owned(),
            model: None,
            data: None,
            head: Some(EntryHead::SelfHead),
            edits: None,
        })
        .await;
    let summary = fixture
        .enqueue_write(EntryDraft {
            key_order: None,
            kind: "summary".to_owned(),
            model: None,
            data: None,
            head: Some(EntryHead::Id(old.id)),
            edits: None,
        })
        .await;
    let (steering, follow_up) = one_at_a_time();
    assert_eq!(
        fixture
            .boundary(BoundaryAt::Final, steering, follow_up)
            .await,
        BoundaryResult {
            users: Vec::new(),
            reset: true,
        }
    );
    assert_eq!(
        fixture.submission(reset).await.status,
        SubmissionStatus::Done
    );
    let summary_record = fixture.submission(summary).await;
    assert_eq!(summary_record.status, SubmissionStatus::Unanswered);
    assert_eq!(summary_record.reason.as_deref(), Some("stale"));
}

#[tokio::test]
async fn withdraws_every_queued_input_and_keeps_queued_writes() {
    let fixture = Fixture::open().await;
    let f1 = fixture.enqueue_input("f1", InputMode::FollowUp, None).await;
    let write = fixture.enqueue_write(note("kept")).await;
    let f2 = fixture.enqueue_input("f2", InputMode::Steer, None).await;

    let conversation = fixture.conversation;
    commit(&fixture.session, move |transaction| {
        Box::pin(async move { withdraw_queued_inputs(transaction, conversation, None).await })
    })
    .await
    .expect("withdraw succeeds");

    assert_eq!(item_ids(&fixture.items().await), vec![write]);
    for id in [f1, f2] {
        let record = fixture.submission(id).await;
        assert_eq!(record.status, SubmissionStatus::Unanswered);
        assert_eq!(record.reason.as_deref(), Some("aborted"));
    }
    assert_eq!(
        fixture.submission(write).await.status,
        SubmissionStatus::Queued
    );

    let (steering, follow_up) = one_at_a_time();
    fixture
        .boundary(BoundaryAt::Final, steering, follow_up)
        .await;
    assert_eq!(transcript(&fixture.entries().await), vec!["note:kept"]);
}

#[tokio::test]
async fn withdraws_with_an_explicit_reason() {
    let fixture = Fixture::open().await;
    let f = fixture.enqueue_input("f", InputMode::FollowUp, None).await;
    let conversation = fixture.conversation;
    commit(&fixture.session, move |transaction| {
        Box::pin(async move {
            withdraw_queued_inputs(transaction, conversation, Some("withdrawn")).await
        })
    })
    .await
    .expect("withdraw succeeds");
    let record = fixture.submission(f).await;
    assert_eq!(record.status, SubmissionStatus::Unanswered);
    assert_eq!(record.reason.as_deref(), Some("withdrawn"));
}

#[tokio::test]
async fn removes_one_named_item_without_touching_the_others() {
    let fixture = Fixture::open().await;
    let x = fixture.enqueue_input("x", InputMode::FollowUp, None).await;
    let y = fixture.enqueue_input("y", InputMode::FollowUp, None).await;
    let z = fixture.enqueue_input("z", InputMode::FollowUp, None).await;
    let conversation = fixture.conversation;
    commit(&fixture.session, move |transaction| {
        Box::pin(async move { remove_inbox_item(transaction, conversation, y).await })
    })
    .await
    .expect("removal succeeds");
    assert_eq!(item_ids(&fixture.items().await), vec![x, z]);
    // 落定由调用方负责；这个提交仍然保持 queued。
    assert_eq!(fixture.submission(y).await.status, SubmissionStatus::Queued);
}

#[tokio::test]
async fn appends_input_as_a_user_entry_or_the_input_entry() {
    let fixture = Fixture::open().await;
    let conversation = fixture.conversation;
    let plain = commit(&fixture.session, move |transaction| {
        Box::pin(async move {
            append_input(
                transaction,
                conversation,
                &UserContent::Text("plain".to_owned()),
                None,
                7,
            )
            .await
        })
    })
    .await
    .expect("plain append");
    assert_eq!(plain.kind, "kiana.user");
    let messages = plain.model.as_ref().expect("model messages");
    assert_eq!(user_text(&messages[0]), Some("plain"));
    assert_eq!(messages.len(), 1);

    let conversation = fixture.conversation;
    let typed = commit(&fixture.session, move |transaction| {
        Box::pin(async move {
            append_input(
                transaction,
                conversation,
                &UserContent::Text("values".to_owned()),
                Some(&InputEntry {
                    kind: "app.note".to_owned(),
                    data: Some(serde_json::json!({"x": 1})),
                }),
                7,
            )
            .await
        })
    })
    .await
    .expect("typed append");
    assert_eq!(typed.kind, "app.note");
    assert_eq!(typed.data, Some(serde_json::json!({"x": 1})));
    let messages = typed.model.as_ref().expect("model messages");
    assert_eq!(user_text(&messages[0]), Some("values"));
    assert_eq!(
        messages[0],
        Message::User(UserMessage {
            role: UserRole::default(),
            content: UserContent::Text("values".to_owned()),
            timestamp: 7,
        })
    );
    assert_eq!(
        transcript(&fixture.entries().await),
        vec!["kiana.user:plain", "app.note:values"]
    );
}

#[tokio::test]
async fn a_queued_steer_behind_waiting_items_places_with_the_first_follow_up_in_id_order() {
    let fixture = Fixture::open().await;
    let f = fixture.enqueue_input("f", InputMode::FollowUp, None).await;
    let steer = fixture.enqueue_input("s", InputMode::Steer, None).await;
    assert_eq!(
        fixture
            .boundary(
                BoundaryAt::Final,
                QueueMode::OneAtATime,
                QueueMode::OneAtATime,
            )
            .await,
        BoundaryResult {
            users: vec![f, steer],
            reset: false,
        }
    );
    assert_eq!(
        transcript(&fixture.entries().await),
        vec!["kiana.user:f", "kiana.user:s"]
    );
}

#[tokio::test]
async fn repeated_request_ids_resolve_through_the_storage_index() {
    // TS 的 `submit` 用会话作用域的 requestId 去重；去重本身在 submissions.ts，这里确认
    // Session 写出的记录能被 `submission_by_request` 按同一个键找到。
    let fixture = Fixture::open().await;
    let conversation = fixture.conversation;
    let id = commit(&fixture.session, move |transaction| {
        Box::pin(async move {
            let record = transaction
                .create_submission(SubmissionCreate {
                    trace: None,
                    conversation_id: conversation,
                    request_id: Some("r".to_owned()),
                    kind: SubmissionKind::Input,
                    status: SubmissionStatus::Queued,
                    entry: None,
                    answer: None,
                    reason: None,
                    detail: None,
                })
                .await?;
            Ok(record.id)
        })
    })
    .await
    .expect("submission creates");
    let stored = fixture
        .storage
        .submission_by_request(fixture.conversation, "r", &**BACKGROUND_CONTEXT)
        .await
        .expect("request lookup")
        .expect("a record");
    assert_eq!(stored.id, id);
}

#[tokio::test]
async fn the_inbox_document_round_trips_queued_items_in_id_order() {
    let fixture = Fixture::open().await;
    let first = fixture.enqueue_input("a", InputMode::Steer, None).await;
    let second = fixture.enqueue_input("b", InputMode::FollowUp, None).await;
    let items = fixture.items().await;
    assert_eq!(item_ids(&items), vec![first, second]);
    let json = serde_json::to_value(&items).expect("serialize");
    assert_eq!(
        json,
        serde_json::json!([
            {"id": first.get(), "mode": "steer", "content": "a"},
            {"id": second.get(), "mode": "followUp", "content": "b"}
        ])
    );
}

// ---------------------------------------------------------------------------
// 未类型化调用方：TS 直接 splice 文档里的普通 JSON 项，未选中的项原样保留。
// ---------------------------------------------------------------------------

/// 直接写入 inbox 的原始 `items` 数组（模拟未类型化调用方或旧文档）。
async fn write_raw_inbox(fixture: &Fixture, items: JsonValue) {
    let conversation = fixture.conversation;
    commit(&fixture.session, move |transaction| {
        let items = items.clone();
        Box::pin(async move {
            let target = DocTarget::conversation(&INBOX_DOC, conversation);
            let mut draft = transaction.doc(&target).await?;
            draft.value_mut().insert("items".to_owned(), items);
            Ok(())
        })
    })
    .await
    .expect("raw inbox writes");
}

async fn raw_inbox(fixture: &Fixture) -> JsonValue {
    let conversation = fixture.conversation;
    commit(&fixture.session, move |transaction| {
        Box::pin(async move {
            let target = DocTarget::conversation(&INBOX_DOC, conversation);
            let draft = transaction.doc(&target).await?;
            Ok(draft
                .value()
                .get("items")
                .cloned()
                .unwrap_or(JsonValue::Array(Vec::new())))
        })
    })
    .await
    .expect("raw inbox reads")
}

/// 直接创建一个 `queued` 提交记录（inbox 项的 ID 必须指向它，放置/落定才能成功）。
async fn create_queued_submission(fixture: &Fixture, kind: SubmissionKind) -> SubmissionId {
    let conversation = fixture.conversation;
    commit(&fixture.session, move |transaction| {
        Box::pin(async move {
            Ok(transaction
                .create_submission(SubmissionCreate {
                    trace: None,
                    conversation_id: conversation,
                    request_id: None,
                    kind,
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

// 回归（minor）：TS 的 `items` 是文档里的普通 JSON 数组，`applyBoundary()` 只 splice 选中的项；
// 未知 mode 的项不参与选择（也不报错），未选中项上的额外字段、`entry: null`、`data: null`
// 都原样保留。类型化往返会丢掉它们。
#[tokio::test]
async fn a_boundary_preserves_untyped_items_verbatim() {
    let fixture = Fixture::open().await;
    let items = serde_json::json!([
        {"id": 1, "mode": "sideways", "content": "x", "extra": {"a": 1}},
        {"id": 2, "mode": "followUp", "content": "y", "entry": null, "extra": "keep"},
        {"id": 3, "mode": "followUp", "content": "z", "entry": {"kind": "app.note", "data": null}}
    ]);
    write_raw_inbox(&fixture, items.clone()).await;

    // PostTools 只选 steer；这里没有 steer，未知 mode 的项也不被选中。
    fixture
        .boundary(BoundaryAt::PostTools, QueueMode::All, QueueMode::All)
        .await;
    assert_eq!(raw_inbox(&fixture).await, items);
}

// 回归（minor）：`withdrawQueuedInputs()` 只跳过 `mode === "write"`，未知 mode 的项会以
// `unanswered` 落定并移除；排队的写入保留（连同额外字段）。
#[tokio::test]
async fn withdrawing_settles_unknown_modes_and_keeps_writes_verbatim() {
    let fixture = Fixture::open().await;
    let unknown = create_queued_submission(&fixture, SubmissionKind::Input).await;
    let write = serde_json::json!({
        "id": 999, "mode": "write", "entry": {"kind": "note", "extra": true}, "extra": 5
    });
    write_raw_inbox(
        &fixture,
        serde_json::json!([
            {"id": unknown.get(), "mode": "sideways", "content": "x"},
            write.clone()
        ]),
    )
    .await;

    let conversation = fixture.conversation;
    commit(&fixture.session, move |transaction| {
        Box::pin(async move { withdraw_queued_inputs(transaction, conversation, None).await })
    })
    .await
    .expect("withdraw succeeds");

    assert_eq!(raw_inbox(&fixture).await, serde_json::json!([write]));
    let record = fixture.submission(unknown).await;
    assert_eq!(record.status, SubmissionStatus::Unanswered);
    assert_eq!(record.reason.as_deref(), Some("aborted"));
}

// 回归（minor）：`pickInputEntry()` 保留 `data: null`（只有 `undefined` 才省略）；类型化的
// `InputEntry` 反序列化会把 null 归一成 None，从而丢掉这个键。
#[tokio::test]
async fn a_placed_input_keeps_entry_data_null() {
    let fixture = Fixture::open().await;
    let submission = create_queued_submission(&fixture, SubmissionKind::Input).await;
    write_raw_inbox(
        &fixture,
        serde_json::json!([{
            "id": submission.get(),
            "mode": "steer",
            "content": "x",
            "entry": {"kind": "app.note", "data": null}
        }]),
    )
    .await;

    fixture
        .boundary(BoundaryAt::PostTools, QueueMode::All, QueueMode::All)
        .await;
    assert_eq!(raw_inbox(&fixture).await, serde_json::json!([]));
    let placed = fixture
        .entries()
        .await
        .into_iter()
        .find(|entry| entry.kind == "app.note")
        .expect("the input is placed as its entry");
    assert_eq!(placed.data, Some(JsonValue::Null));
}
