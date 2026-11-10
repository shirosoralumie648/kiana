//! 移植自 `packages/durable/test/examples/19-inbox.ts`：会话忙碌时提交都去了哪里。
//!
//! TS 示例通过 `Conversation.submit()` 在运行中排队；本单元的上层 `harness.rs`/任务运行尚未实现，
//! 因此这里用 [`SessionImpl`] 建立同样的排队提交与 inbox 文档，再直接调用边界的
//! [`prepare_boundary`]/[`apply_boundary`] 展示放置顺序与撤回语义。

use std::sync::Arc;

use kiana_ai::UserContent;
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::inbox::{
    BoundaryAt, INBOX_DOC, InboxItem, InboxState, InputMode, QueueModes, apply_boundary,
    prepare_boundary, remove_inbox_item, withdraw_queued_inputs,
};
use kiana_durable::harness::types::QueueMode;
use kiana_durable::session::{SessionImpl, SessionOptions};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::types::{
    BoxError, ConversationId, ConversationOwnership, ConversationRecord, CreateConversationOptions,
    DocTarget, EntryDraft, EntryQuery, JsonValue, ScanOrder, Storage, SubmissionCreate,
    SubmissionId, SubmissionKind, SubmissionSettlement, SubmissionStatus,
};

async fn create_ownerless(
    session: &SessionImpl,
    context: &dyn Context,
) -> Result<ConversationRecord, BoxError> {
    session
        .commit_with(
            |tx| {
                Box::pin(async move {
                    tx.create_conversation(&CreateConversationOptions {
                        ownership: ConversationOwnership::Ownerless,
                    })
                    .await
                })
            },
            context,
            Default::default(),
        )
        .await
}

async fn read_items(
    session: &SessionImpl,
    conversation: ConversationId,
    context: &dyn Context,
) -> Result<Vec<InboxItem>, BoxError> {
    let value = session
        .snapshot(&DocTarget::conversation(&INBOX_DOC, conversation), context)
        .await?
        .unwrap_or_default();
    let state: InboxState = serde_json::from_value(JsonValue::Object(value.into_iter().collect()))?;
    Ok(state.items)
}

/// 排队一次输入（TS: `submit({ type: "input", ... })` 在忙碌时写进 inbox）。
async fn enqueue_input(
    session: &SessionImpl,
    conversation: ConversationId,
    text: &str,
    mode: InputMode,
    context: &dyn Context,
) -> Result<SubmissionId, BoxError> {
    let content = text.to_owned();
    session
        .commit_with(
            move |tx| {
                let content = content.clone();
                Box::pin(async move {
                    let record = tx
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
                    let mut draft = tx.doc(&target).await?;
                    let mut items: InboxState = serde_json::from_value(JsonValue::Object(
                        draft.value().clone().into_iter().collect(),
                    ))?;
                    items.items.push(InboxItem::Input {
                        id: record.id,
                        mode,
                        content: UserContent::Text(content),
                        entry: None,
                    });
                    draft
                        .value_mut()
                        .insert("items".to_owned(), serde_json::to_value(&items.items)?);
                    Ok(record.id)
                })
            },
            context,
            Default::default(),
        )
        .await
}

/// 排队一次条目写入（TS: `submit({ type: "write", entry })`）。
async fn enqueue_write(
    session: &SessionImpl,
    conversation: ConversationId,
    entry: EntryDraft,
    context: &dyn Context,
) -> Result<SubmissionId, BoxError> {
    session
        .commit_with(
            move |tx| {
                let entry = entry.clone();
                Box::pin(async move {
                    let record = tx
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
                    let mut draft = tx.doc(&target).await?;
                    let mut items: InboxState = serde_json::from_value(JsonValue::Object(
                        draft.value().clone().into_iter().collect(),
                    ))?;
                    items.items.push(InboxItem::Write {
                        id: record.id,
                        entry: entry.clone(),
                    });
                    draft
                        .value_mut()
                        .insert("items".to_owned(), serde_json::to_value(&items.items)?);
                    Ok(record.id)
                })
            },
            context,
            Default::default(),
        )
        .await
}

async fn boundary(
    session: &SessionImpl,
    conversation: ConversationId,
    at: BoundaryAt,
    modes: QueueModes,
    context: &dyn Context,
) -> Result<(Vec<SubmissionId>, bool), BoxError> {
    session
        .commit_with(
            move |tx| {
                Box::pin(async move {
                    let mut boundary = prepare_boundary(tx, conversation, modes).await?;
                    let result = apply_boundary(tx, &mut boundary, at, 7).await?;
                    Ok((result.users, result.reset))
                })
            },
            context,
            Default::default(),
        )
        .await
}

fn describe_item(item: &InboxItem) -> String {
    match item {
        InboxItem::Input { id, mode, .. } => format!("{id} {}", mode.as_str()),
        InboxItem::Write { id, .. } => format!("{id} write"),
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let session = SessionImpl::new(Arc::new(MemoryStorage::new()), SessionOptions::default());
    let storage: &dyn Storage = session.storage().as_ref();
    let chat = create_ownerless(&session, context).await?;

    // 忙碌时，输入排成 follow-up（默认）或 steer，写入也排队。
    let follow_up = enqueue_input(
        &session,
        chat.id,
        "A follow-up",
        InputMode::FollowUp,
        context,
    )
    .await?;
    let steer = enqueue_input(&session, chat.id, "A steer", InputMode::Steer, context).await?;
    let note = enqueue_write(
        &session,
        chat.id,
        EntryDraft {
            kind: "app.note".to_owned(),
            model: None,
            data: Some(JsonValue::from("noted while busy")),
            head: None,
            edits: None,
        },
        context,
    )
    .await?;
    let withdrawn = enqueue_input(
        &session,
        chat.id,
        "Never mind",
        InputMode::FollowUp,
        context,
    )
    .await?;

    // TS 里 `whenBusy: "reject"` 会直接拒绝而不是排队；拒绝发生在运行循环里（上层单元）。
    // 一个可撤回的排队提交在边界放置它之前可以被撤回。
    session
        .commit_with(
            move |tx| {
                Box::pin(async move {
                    tx.settle_submission(
                        withdrawn,
                        SubmissionSettlement::Unanswered {
                            reason: "aborted".to_owned(),
                            detail: None,
                        },
                    );
                    remove_inbox_item(tx, chat.id, withdrawn).await
                })
            },
            context,
            Default::default(),
        )
        .await?;
    println!("withdraw: true");

    let inbox = read_items(&session, chat.id, context).await?;
    println!(
        "inbox: {:?}",
        inbox.iter().map(describe_item).collect::<Vec<_>>()
    );

    // 第一次回答在 final 边界结束这次运行：先放置写入，再放置 steer 与 follow-up，后者一起开始下一轮。
    // 设置 followUpMode: "all" 让所有排队的 follow-up 一次放置。
    let modes = QueueModes {
        steering_mode: QueueMode::OneAtATime,
        follow_up_mode: QueueMode::All,
    };
    let (users, reset) = boundary(&session, chat.id, BoundaryAt::Final, modes, context).await?;
    println!("placed users: {users:?} reset: {reset}");

    let status = |id: SubmissionId, name: &str| {
        let name = name.to_owned();
        async move {
            let record = storage
                .submission(id, &**BACKGROUND_CONTEXT)
                .await
                .expect("submission reads")
                .expect("submission exists");
            let detail = record.reason.clone().unwrap_or_default();
            println!("{name}: {:?} {detail}", record.status);
        }
    };
    status(follow_up, "follow-up").await;
    status(steer, "steer").await;
    status(note, "note").await;
    status(withdrawn, "withdrawn").await;

    let page = storage
        .scan_entries(
            &EntryQuery {
                conversation_id: chat.id,
                min_entry_id: None,
                max_entry_id: None,
                order: Some(ScanOrder::Descending),
            },
            20,
            None,
            context,
        )
        .await?;
    let kinds: Vec<&str> = page.items.iter().map(|entry| entry.kind.as_str()).collect();
    println!("transcript: {kinds:?}");

    // 撤回所有仍然排队的输入（abort 级联），写入留在原处等待稍后放置。
    let late = enqueue_input(
        &session,
        chat.id,
        "Late follow-up",
        InputMode::FollowUp,
        context,
    )
    .await?;
    enqueue_write(
        &session,
        chat.id,
        EntryDraft {
            kind: "app.note".to_owned(),
            model: None,
            data: Some(JsonValue::from("kept after abort")),
            head: None,
            edits: None,
        },
        context,
    )
    .await?;
    let queued = read_items(&session, chat.id, context).await?;
    println!(
        "queued before abort: {:?}",
        queued.iter().map(describe_item).collect::<Vec<_>>()
    );
    session
        .commit_with(
            move |tx| Box::pin(async move { withdraw_queued_inputs(tx, chat.id, None).await }),
            context,
            Default::default(),
        )
        .await?;
    let inbox = read_items(&session, chat.id, context).await?;
    println!(
        "inbox after abort: {:?}",
        inbox.iter().map(describe_item).collect::<Vec<_>>()
    );
    status(late, "late follow-up").await;

    session.close(context).await;
    Ok(())
}
