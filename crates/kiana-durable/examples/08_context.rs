//! 移植自 `packages/durable/test/examples/08-context.ts`：转录历史与模型上下文。
//!
//! TS 示例通过 `Conversation.context()` 读取；本单元的上层 `harness.rs` 尚未实现，因此这里用
//! [`SessionImpl`] 追加与 TS 相同的条目，再用 [`read_context`] 得到同一份模型请求消息。

use std::sync::Arc;

use kiana_ai::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, StopReason, SystemContent,
    SystemMessage, SystemRole, TextContent, TextKind, ToolCall, ToolCallKind, ToolResultMessage,
    ToolResultRole, Usage, UserContent, UserContentBlock, UserMessage, UserRole,
};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::context::read_context;
use kiana_durable::session::{SessionImpl, SessionOptions};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::types::{
    BoxError, ContextEdit, ContextEditAction, ConversationId, ConversationOwnership,
    ConversationRecord, CreateConversationOptions, EntryDraft, EntryHead, EntryId, EntryQuery,
    EntryRecord, JsonValue, ScanOrder, Storage,
};

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::default(),
        content: UserContent::Text(text.to_owned()),
        timestamp: 1,
    })
}

fn assistant(text: &str, calls: &[&str], stop_reason: StopReason) -> Message {
    let mut content: Vec<AssistantContentBlock> = Vec::new();
    if !text.is_empty() {
        content.push(AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::default(),
            text: text.to_owned(),
            text_signature: None,
        }));
    }
    for call in calls {
        content.push(AssistantContentBlock::ToolCall(ToolCall {
            kind_tool_call: ToolCallKind::default(),
            id: (*call).to_owned(),
            name: "read".to_owned(),
            arguments: Default::default(),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        }));
    }
    Message::Assistant(AssistantMessage {
        role: AssistantRole::default(),
        content,
        usage: Usage::default(),
        stop_reason,
        timestamp: 2,
        ..Default::default()
    })
}

fn tool_result(id: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::default(),
        tool_call_id: id.to_owned(),
        tool_name: "read".to_owned(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::default(),
            text: format!("file {id}"),
            text_signature: None,
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 3,
        duration_ms: None,
    })
}

fn system_cwd(cwd: &str) -> Message {
    let mut sections = indexmap::IndexMap::new();
    sections.insert("cwd".to_owned(), Some(cwd.to_owned()));
    Message::System(SystemMessage {
        role: SystemRole::default(),
        content: SystemContent::Text(String::new()),
        sections: Some(sections),
        tools_added: None,
        tools_removed: None,
        timestamp: 4,
    })
}

fn draft(
    kind: &str,
    model: Option<Vec<Message>>,
    data: Option<JsonValue>,
    head: Option<EntryHead>,
    edits: Option<Vec<ContextEdit>>,
) -> EntryDraft {
    EntryDraft {
        kind: kind.to_owned(),
        model,
        data,
        head,
        edits,
    }
}

/// 与 TS `show()` 相同的可读描述（合成的缺失结果标成 error）。
fn show(message: &Message) -> String {
    match message {
        Message::User(user) => match &user.content {
            UserContent::Text(text) => format!("user: {text}"),
            UserContent::Blocks(_) => "user: [blocks]".to_owned(),
        },
        Message::System(system) => format!(
            "system: {}",
            serde_json::to_string(&system.sections).unwrap_or_else(|_| "null".to_owned())
        ),
        Message::Assistant(assistant) => {
            let parts: Vec<String> = assistant
                .content
                .iter()
                .map(|block| match block {
                    AssistantContentBlock::Text(text) => text.text.clone(),
                    AssistantContentBlock::ToolCall(call) => format!("call({})", call.id),
                    _ => String::new(),
                })
                .filter(|part| !part.is_empty())
                .collect();
            format!("assistant: {}", parts.join(" "))
        }
        Message::ToolResult(result) => format!(
            "result({}){}",
            result.tool_call_id,
            if result.is_error { " error" } else { "" }
        ),
    }
}

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

async fn append(
    session: &SessionImpl,
    conversation_id: ConversationId,
    value: EntryDraft,
    context: &dyn Context,
) -> Result<EntryRecord, BoxError> {
    session
        .commit_with(
            move |tx| Box::pin(async move { tx.append_entry(conversation_id, value).await }),
            context,
            Default::default(),
        )
        .await
}

async fn fork_at(
    session: &SessionImpl,
    parent: ConversationId,
    at: EntryId,
    context: &dyn Context,
) -> Result<ConversationRecord, BoxError> {
    session
        .commit_with(
            move |tx| {
                Box::pin(async move {
                    tx.fork_conversation(
                        parent,
                        at,
                        &CreateConversationOptions {
                            ownership: ConversationOwnership::Ownerless,
                        },
                    )
                    .await
                })
            },
            context,
            Default::default(),
        )
        .await
}

fn kinds(entries: &[EntryRecord]) -> Vec<String> {
    entries.iter().map(|entry| entry.kind.clone()).collect()
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let session = SessionImpl::new(Arc::new(MemoryStorage::new()), SessionOptions::default());
    let storage: &dyn Storage = session.storage().as_ref();
    let chat = create_ownerless(&session, context).await?;

    // 条目不可变。`model` 是条目贡献给下一次模型请求的消息；`data` 只给应用看。
    // context() 把已存转录变成这些请求消息：
    //   - 带 `head` 的条目开启新上下文，更旧的条目仍被存储，
    //   - `edits` 替换或省略更早条目的贡献，
    //   - aborted/error/deferred 的 assistant 消息不会被发送，
    //   - 工具结果紧跟调用、按调用顺序发送，
    //   - 没有结果的调用会得到一条合成的 error 结果。
    let question = append(
        &session,
        chat.id,
        draft(
            "message",
            Some(vec![user("read a and b")]),
            None,
            None,
            None,
        ),
        context,
    )
    .await?;
    append(
        &session,
        chat.id,
        draft(
            "message",
            Some(vec![assistant("I crashed", &[], StopReason::Aborted)]),
            None,
            None,
            None,
        ),
        context,
    )
    .await?;
    let calls = append(
        &session,
        chat.id,
        draft(
            "message",
            Some(vec![assistant("reading", &["a", "b"], StopReason::ToolUse)]),
            None,
            None,
            None,
        ),
        context,
    )
    .await?;
    // 结果乱序完成。
    append(
        &session,
        chat.id,
        draft("message", Some(vec![tool_result("b")]), None, None, None),
        context,
    )
    .await?;
    append(
        &session,
        chat.id,
        draft(
            "kiana.system",
            Some(vec![system_cwd("<cwd>/repo")]),
            None,
            None,
            None,
        ),
        context,
    )
    .await?;
    append(
        &session,
        chat.id,
        draft("message", Some(vec![tool_result("a")]), None, None, None),
        context,
    )
    .await?;
    append(
        &session,
        chat.id,
        draft(
            "message",
            Some(vec![assistant("a and b look fine", &[], StopReason::Stop)]),
            None,
            None,
            None,
        ),
        context,
    )
    .await?;
    append(
        &session,
        chat.id,
        draft(
            "edit",
            None,
            Some(JsonValue::from("user fixed a typo")),
            None,
            Some(vec![ContextEdit {
                target: question.id,
                action: ContextEditAction::Replace,
                messages: Some(vec![user("read files a and b")]),
            }]),
        ),
        context,
    )
    .await?;
    append(
        &session,
        chat.id,
        draft(
            "note",
            None,
            Some(JsonValue::from("display only")),
            None,
            None,
        ),
        context,
    )
    .await?;

    let view = read_context(&session, storage, chat.id, context, None).await?;
    println!("raw active entries: {:?}", kinds(&view.entries));
    println!(
        "request messages: {:?}",
        view.messages.iter().map(show).collect::<Vec<_>>()
    );

    // 在工具调用处 fork 时还没有结果；context() 会补上。
    let cut = fork_at(&session, chat.id, calls.id, context).await?;
    let fork_view = read_context(&session, storage, cut.id, context, None).await?;
    println!(
        "fork messages: {:?}",
        fork_view.messages.iter().map(show).collect::<Vec<_>>()
    );

    // 带 head 的摘要替换它指向的条目之前的全部内容（"self" 指向摘要条目自己）。
    append(
        &session,
        chat.id,
        draft(
            "summary",
            Some(vec![user("Summary: a and b are fine.")]),
            None,
            Some(EntryHead::SelfHead),
            None,
        ),
        context,
    )
    .await?;
    let view = read_context(&session, storage, chat.id, context, None).await?;
    println!(
        "after summary: {:?} {:?}",
        view.head.as_ref().map(|head| head.kind.clone()),
        view.messages.iter().map(show).collect::<Vec<_>>()
    );

    // entries() 从新到旧分页存储的转录，包括继承的父条目；head 与 edit 从不删除任何东西。
    let history = storage
        .scan_entries(
            &EntryQuery {
                conversation_id: chat.id,
                min_entry_id: None,
                max_entry_id: None,
                order: Some(ScanOrder::Descending),
            },
            3,
            None,
            context,
        )
        .await?;
    println!(
        "newest stored entries: {:?} more: {}",
        kinds(&history.items),
        history.next.is_some()
    );

    session.close(context).await;
    Ok(())
}
