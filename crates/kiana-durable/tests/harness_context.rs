//! `test/harness-context.test.ts` 的 Rust 端口（可移植部分）。
//!
//! TS 测试通过 `Harness`/`Conversation` 读上下文；本单元的上层 `harness.rs` 尚未实现，因此这里
//! 直接用 [`SessionImpl`] 追加条目、用 [`read_context`]/[`read_context_from`] 读同一份派生逻辑。
//! 依赖任务运行（`root.context()` 的缓存、保留时长与冻结）的用例见报告的 remainingGaps。

mod support;

use std::sync::Arc;

use kiana_ai::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, StopReason, SystemContent,
    SystemMessage, SystemRole, TextContent, TextKind, ToolCall, ToolCallKind, ToolResultMessage,
    ToolResultRole, Usage, UserContent, UserContentBlock, UserMessage, UserRole,
};
use kiana_chord::context::BACKGROUND_CONTEXT;
use kiana_durable::harness::context::{
    ContextRange, active_entries, derive_context, order_tool_results, read_context,
    read_context_from,
};
use kiana_durable::session::SessionImpl;
use kiana_durable::types::{ContextEdit, ContextEditAction, EntryDraft, EntryHead, EntryRecord};
use kiana_durable::{
    BoxError, ConversationId, EntryId, Storage, TypedEntryTx, UserEntry, id_from_number,
};
use support::{commit, create_conversation, open_test_session};

struct Fixture {
    storage: Arc<support::ControlledStorage>,
    session: SessionImpl,
    conversation: ConversationId,
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

    async fn message(&self, message: Message) -> EntryRecord {
        self.append(draft("message", vec![message])).await
    }

    async fn system(&self, message: Message) -> EntryRecord {
        self.append(draft("kiana.system", vec![message])).await
    }

    async fn view(&self, at: Option<EntryId>) -> kiana_durable::harness::types::ContextView {
        read_context(
            &self.session,
            &*self.storage as &dyn Storage,
            self.conversation,
            &**BACKGROUND_CONTEXT,
            at,
        )
        .await
        .expect("context read succeeds")
    }
}

fn draft(kind: &str, model: Vec<Message>) -> EntryDraft {
    EntryDraft {
        key_order: None,
        kind: kind.to_owned(),
        model: Some(model),
        data: None,
        head: None,
        edits: None,
    }
}

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::default(),
        content: UserContent::Text(text.to_owned()),
        timestamp: 0,
    })
}

fn describe_content(content: &UserContent) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                UserContentBlock::Text(text) => text.text.clone(),
                UserContentBlock::Image(_) => "[image]".to_owned(),
            })
            .collect(),
    }
}

fn assistant_with(text: &str, calls: &[&str], stop_reason: StopReason) -> Message {
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
            name: (*call).to_owned(),
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
        ..Default::default()
    })
}

fn assistant(text: &str) -> Message {
    assistant_with(text, &[], StopReason::Stop)
}

fn tool_result(id: &str, text: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::default(),
        tool_call_id: id.to_owned(),
        tool_name: id.to_owned(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::default(),
            text: text.to_owned(),
            text_signature: None,
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    })
}

fn system_section(key: &str, value: &str) -> Message {
    system_update(key, Some(value))
}

/// `kiana.system` 的分段补丁：`None` 删除该分段（TS 的 `null`）。
fn system_update(key: &str, value: Option<&str>) -> Message {
    let mut sections = indexmap::IndexMap::new();
    sections.insert(key.to_owned(), value.map(str::to_owned));
    Message::System(SystemMessage {
        role: SystemRole::default(),
        content: SystemContent::Text(String::new()),
        sections: Some(sections),
        tools_added: None,
        tools_removed: None,
        timestamp: 0,
    })
}

fn edit(target: EntryId, action: ContextEditAction, messages: Option<Vec<Message>>) -> EntryDraft {
    EntryDraft {
        key_order: None,
        kind: "edit".to_owned(),
        model: None,
        data: None,
        head: None,
        edits: Some(vec![ContextEdit {
            target,
            action,
            messages,
        }]),
    }
}

fn describe(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| match message {
            Message::System(system) => match &system.sections {
                Some(sections) => format!(
                    "system:{}",
                    sections.keys().cloned().collect::<Vec<_>>().join(",")
                ),
                None => "system".to_owned(),
            },
            Message::User(user) => format!("user:{}", describe_content(&user.content)),
            Message::Assistant(assistant) => {
                let text = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                format!("assistant:{text}")
            }
            Message::ToolResult(result) => {
                if result.is_error {
                    return format!("result:{}:error", result.tool_call_id);
                }
                let text = result
                    .content
                    .iter()
                    .map(|block| match block {
                        UserContentBlock::Text(text) => text.text.clone(),
                        UserContentBlock::Image(_) => "[image]".to_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join("");
                format!("result:{}:{text}", result.tool_call_id)
            }
        })
        .collect()
}

fn ids(entries: &[EntryRecord]) -> Vec<EntryId> {
    entries.iter().map(|entry| entry.id).collect()
}

#[tokio::test]
async fn returns_the_whole_transcript_and_excludes_model_less_entries_from_messages() {
    let fixture = Fixture::open().await;
    let first = fixture.message(user("hi")).await;
    let note = fixture
        .append(EntryDraft {
            key_order: None,
            kind: "note".to_owned(),
            model: None,
            data: Some(serde_json::json!({"text": "display only"})),
            head: None,
            edits: None,
        })
        .await;
    let answer = fixture.message(assistant("hello")).await;
    let view = fixture.view(None).await;
    assert!(view.head.is_none());
    assert_eq!(ids(&view.entries), vec![first.id, note.id, answer.id]);
    assert_eq!(describe(&view.messages), vec!["user:hi", "assistant:hello"]);
    assert_eq!(view.contributions[1], Vec::<Message>::new());
}

#[tokio::test]
async fn excludes_aborted_error_and_deferred_assistant_messages_but_keeps_their_entries() {
    let fixture = Fixture::open().await;
    fixture.message(user("q")).await;
    let aborted = fixture
        .message(assistant_with("partial", &[], StopReason::Aborted))
        .await;
    fixture
        .message(assistant_with("failed", &[], StopReason::Error))
        .await;
    fixture
        .message(assistant_with("later", &[], StopReason::Deferred))
        .await;
    fixture
        .message(assistant_with("done", &[], StopReason::Length))
        .await;
    let view = fixture.view(None).await;
    assert_eq!(view.entries.len(), 5);
    assert_eq!(view.entries[1].id, aborted.id);
    assert_eq!(describe(&view.messages), vec!["user:q", "assistant:done"]);
}

#[tokio::test]
async fn resolves_self_heads_and_uses_the_newest_head_marker() {
    let fixture = Fixture::open().await;
    fixture.message(user("old")).await;
    let reset = fixture
        .append(EntryDraft {
            key_order: None,
            kind: "reset".to_owned(),
            model: Some(vec![user("fresh start")]),
            data: None,
            head: Some(EntryHead::SelfHead),
            edits: None,
        })
        .await;
    assert_eq!(reset.head, Some(reset.id));
    let after = fixture.message(assistant("after reset")).await;
    let view = fixture.view(None).await;
    assert_eq!(view.head.as_ref().unwrap().id, reset.id);
    assert_eq!(ids(&view.entries), vec![reset.id, after.id]);
    assert_eq!(
        describe(&view.messages),
        vec!["user:fresh start", "assistant:after reset"]
    );

    let summary = fixture
        .append(EntryDraft {
            key_order: None,
            kind: "summary".to_owned(),
            model: Some(vec![user("summary")]),
            data: None,
            head: Some(EntryHead::Id(after.id)),
            edits: None,
        })
        .await;
    let tail = fixture.message(user("next")).await;
    let view = fixture.view(None).await;
    assert_eq!(view.head.as_ref().unwrap().id, summary.id);
    assert_eq!(ids(&view.entries), vec![summary.id, after.id, tail.id]);
    assert_eq!(
        describe(&view.messages),
        vec!["user:summary", "assistant:after reset", "user:next"]
    );
}

#[tokio::test]
async fn applies_the_newest_edit_per_target_within_the_active_range() {
    let fixture = Fixture::open().await;
    let first = fixture.message(user("first")).await;
    let second = fixture.message(user("second")).await;
    fixture
        .append(edit(
            first.id,
            ContextEditAction::Replace,
            Some(vec![user("first v2")]),
        ))
        .await;
    fixture
        .append(edit(
            first.id,
            ContextEditAction::Replace,
            Some(vec![user("first v3")]),
        ))
        .await;
    fixture
        .append(edit(second.id, ContextEditAction::Omit, None))
        .await;
    let view = fixture.view(None).await;
    assert_eq!(view.entries.len(), 5);
    assert_eq!(describe(&view.messages), vec!["user:first v3"]);

    let reset = fixture
        .append(EntryDraft {
            key_order: None,
            kind: "reset".to_owned(),
            model: None,
            data: None,
            head: Some(EntryHead::Id(second.id)),
            edits: None,
        })
        .await;
    let view = fixture.view(None).await;
    assert_eq!(view.head.as_ref().unwrap().id, reset.id);
    assert!(describe(&view.messages).is_empty());

    fixture
        .append(edit(
            second.id,
            ContextEditAction::Replace,
            Some(vec![user("second v2")]),
        ))
        .await;
    let view = fixture.view(None).await;
    assert_eq!(describe(&view.messages), vec!["user:second v2"]);
}

#[tokio::test]
async fn keeps_positional_system_messages_and_orders_tool_results_by_call_order() {
    let fixture = Fixture::open().await;
    fixture
        .system(system_section("preamble", "You help."))
        .await;
    fixture.message(user("run tools")).await;
    fixture
        .message(assistant_with("calling", &["b", "a"], StopReason::ToolUse))
        .await;
    fixture.message(tool_result("a", "result a")).await;
    fixture.system(system_section("cwd", "/repo")).await;
    fixture.message(tool_result("b", "result b")).await;
    fixture.message(tool_result("zz", "stray")).await;
    fixture.message(assistant("done")).await;
    let view = fixture.view(None).await;
    assert_eq!(
        describe(&view.messages),
        vec![
            "system:preamble",
            "user:run tools",
            "assistant:calling",
            "result:b:result b",
            "result:a:result a",
            "system:cwd",
            "assistant:done",
        ]
    );
}

#[tokio::test]
async fn leads_with_a_system_message_that_only_user_messages_precede() {
    let fixture = Fixture::open().await;
    fixture.message(user("first")).await;
    fixture.message(user("steered")).await;
    fixture
        .system(system_section("preamble", "You help."))
        .await;
    fixture.message(assistant("answer")).await;
    fixture.message(user("next")).await;
    fixture.system(system_section("cwd", "/repo")).await;
    let view = fixture.view(None).await;
    assert_eq!(
        describe(&view.messages),
        vec![
            "system:preamble",
            "user:first",
            "user:steered",
            "assistant:answer",
            "user:next",
            "system:cwd",
        ]
    );
    let first_contributions: Vec<String> = describe(
        &view
            .contributions
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>(),
    )
    .into_iter()
    .take(3)
    .collect();
    assert_eq!(
        first_contributions,
        vec!["user:first", "user:steered", "system:preamble"]
    );
}

#[tokio::test]
async fn leads_with_the_baseline_after_a_compaction_that_keeps_assistant_messages() {
    let fixture = Fixture::open().await;
    fixture.message(user("old")).await;
    let kept = fixture.message(user("kept")).await;
    fixture
        .message(assistant_with("calling", &["a"], StopReason::ToolUse))
        .await;
    fixture.message(tool_result("a", "result a")).await;
    fixture.message(assistant("answer")).await;
    fixture
        .append(EntryDraft {
            key_order: None,
            kind: "summary".to_owned(),
            model: Some(vec![user("summary")]),
            data: None,
            head: Some(EntryHead::Id(kept.id)),
            edits: None,
        })
        .await;
    fixture.message(user("next")).await;
    fixture
        .system(system_section("preamble", "Baseline."))
        .await;
    let view = fixture.view(None).await;
    assert_eq!(
        describe(&view.messages),
        vec![
            "system:preamble",
            "user:summary",
            "user:kept",
            "assistant:calling",
            "result:a:result a",
            "assistant:answer",
            "user:next",
        ]
    );

    fixture.message(assistant("done")).await;
    fixture.system(system_section("cwd", "/repo")).await;
    let view = fixture.view(None).await;
    let described = describe(&view.messages);
    assert_eq!(&described[..2], ["system:preamble", "user:summary"]);
    assert_eq!(
        &described[described.len() - 2..],
        ["assistant:done", "system:cwd"]
    );
}

#[tokio::test]
async fn a_baseline_written_after_the_first_answers_leads_when_nothing_earlier_was_a_system_message()
 {
    let fixture = Fixture::open().await;
    fixture
        .append(draft("kiana.user", vec![user("first")]))
        .await;
    fixture
        .append(draft("kiana.assistant", vec![assistant("answer")]))
        .await;
    fixture
        .append(draft("kiana.user", vec![user("next")]))
        .await;
    fixture.system(system_section("plan", "Plan.")).await;
    let view = fixture.view(None).await;
    assert_eq!(
        describe(&view.messages),
        vec!["system:plan", "user:first", "assistant:answer", "user:next"]
    );
}

#[tokio::test]
async fn reads_the_context_as_of_an_earlier_entry() {
    // TS: "reads the context as of an earlier entry, as a fork at that entry starts"（#10512）。
    let fixture = Fixture::open().await;
    let first = fixture.message(user("first")).await;
    let call = fixture
        .message(assistant_with("calling", &["x", "y"], StopReason::ToolUse))
        .await;
    let result = fixture.message(tool_result("x", "result x")).await;
    fixture.message(tool_result("y", "result y")).await;
    let edit = fixture
        .append(edit(
            first.id,
            ContextEditAction::Replace,
            Some(vec![user("first v2")]),
        ))
        .await;
    let reset = fixture
        .append(EntryDraft {
            key_order: None,
            kind: "reset".to_owned(),
            model: Some(vec![user("fresh start")]),
            data: None,
            head: Some(EntryHead::SelfHead),
            edits: None,
        })
        .await;
    let tail = fixture.message(assistant("after reset")).await;

    // 在 `call` 处截断：编辑与 reset 都不生效，被切掉调用得到合成的结果。
    let at_call = fixture.view(Some(call.id)).await;
    assert_eq!(
        describe(&at_call.messages),
        vec![
            "user:first",
            "assistant:calling",
            "result:x:error",
            "result:y:error",
        ]
    );
    // 在 `result` 处截断：x 有真实结果，y 缺失被合成。
    let at_result = fixture.view(Some(result.id)).await;
    assert_eq!(
        describe(&at_result.messages),
        vec![
            "user:first",
            "assistant:calling",
            "result:x:result x",
            "result:y:error",
        ]
    );
    // 在 `edit` 处截断：替换生效，reset 尚未发生。
    let at_edit = fixture.view(Some(edit.id)).await;
    assert_eq!(
        describe(&at_edit.messages),
        vec![
            "user:first v2",
            "assistant:calling",
            "result:x:result x",
            "result:y:result y",
        ]
    );
    // 在 `reset` 处截断：活动范围从 reset 自己开始。
    let at_reset = fixture.view(Some(reset.id)).await;
    assert_eq!(describe(&at_reset.messages), vec!["user:fresh start"]);
    // 尾部与完整读取一致。
    assert_eq!(
        describe(&fixture.view(Some(tail.id)).await.messages),
        describe(&fixture.view(None).await.messages)
    );

    let error = read_context(
        &fixture.session,
        &*fixture.storage as &dyn Storage,
        fixture.conversation,
        &**BACKGROUND_CONTEXT,
        Some(id_from_number(999)),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "Entry 999 is not visible from conversation {}",
            fixture.conversation
        )
    );
}

#[tokio::test]
async fn an_unknown_at_is_rejected() {
    let fixture = Fixture::open().await;
    fixture.message(user("only")).await;
    let other = create_conversation(&fixture.session).await;
    let entry = fixture.message(user("elsewhere")).await;
    // 同一次 get 里把条目写进另一个会话不会发生；这里直接读一个不可见条目。
    let error = read_context(
        &fixture.session,
        &*fixture.storage as &dyn Storage,
        other,
        &**BACKGROUND_CONTEXT,
        Some(entry.id),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("is not visible from conversation")
    );
}

#[tokio::test]
async fn an_extended_read_matches_a_full_read() {
    let fixture = Fixture::open().await;
    for index in 0..10 {
        fixture.message(user(&format!("message {index}"))).await;
    }
    let ctx = &**BACKGROUND_CONTEXT;
    let storage = &*fixture.storage as &dyn Storage;
    let (first_view, first_range) = read_context_from(
        &fixture.session,
        storage,
        fixture.conversation,
        ctx,
        None,
        None,
    )
    .await
    .expect("first read");
    let range: ContextRange = first_range.expect("a range is returned");
    assert_eq!(first_view.messages.len(), 10);

    for index in 10..14 {
        fixture.message(user(&format!("message {index}"))).await;
    }
    let (extended, extended_range) = read_context_from(
        &fixture.session,
        storage,
        fixture.conversation,
        ctx,
        None,
        Some(&range),
    )
    .await
    .expect("extended read");
    let full = fixture.view(None).await;
    assert_eq!(describe(&extended.messages), describe(&full.messages));
    assert_eq!(ids(&extended.entries), ids(&full.entries));
    assert_eq!(extended_range.unwrap().entries.len(), 14);
    assert_eq!(describe(&extended.messages).len(), 14);
}

#[tokio::test]
async fn the_same_tail_reuses_the_previous_range() {
    let fixture = Fixture::open().await;
    fixture.message(user("one")).await;
    fixture.message(user("two")).await;
    let ctx = &**BACKGROUND_CONTEXT;
    let storage = &*fixture.storage as &dyn Storage;
    let (_, range) = read_context_from(
        &fixture.session,
        storage,
        fixture.conversation,
        ctx,
        None,
        None,
    )
    .await
    .expect("first read");
    let range = range.expect("a range is returned");
    let (view, again) = read_context_from(
        &fixture.session,
        storage,
        fixture.conversation,
        ctx,
        None,
        Some(&range),
    )
    .await
    .expect("second read");
    assert_eq!(view.entries.len(), 2);
    assert_eq!(again.unwrap().entries.len(), 2);
}

#[tokio::test]
async fn a_shorter_tail_rederives_the_range() {
    let fixture = Fixture::open().await;
    let first = fixture.message(user("one")).await;
    fixture.message(user("two")).await;
    let ctx = &**BACKGROUND_CONTEXT;
    let storage = &*fixture.storage as &dyn Storage;
    let (_, range) = read_context_from(
        &fixture.session,
        storage,
        fixture.conversation,
        ctx,
        None,
        None,
    )
    .await
    .expect("first read");
    let range = range.expect("a range is returned");
    let (view, shorter) = read_context_from(
        &fixture.session,
        storage,
        fixture.conversation,
        ctx,
        Some(first.id),
        Some(&range),
    )
    .await
    .expect("truncated read");
    assert_eq!(view.entries.len(), 1);
    assert_eq!(shorter.unwrap().entries.len(), 1);
    assert_eq!(describe(&view.messages), vec!["user:one"]);
}

#[tokio::test]
async fn active_entries_and_derive_context_agree_on_the_entries() {
    let fixture = Fixture::open().await;
    fixture.message(user("one")).await;
    let reset = fixture
        .append(EntryDraft {
            key_order: None,
            kind: "reset".to_owned(),
            model: Some(vec![user("fresh")]),
            data: None,
            head: Some(EntryHead::SelfHead),
            edits: None,
        })
        .await;
    fixture.message(user("after")).await;
    let bounds = kiana_durable::harness::context::capture_context_bounds(
        &*fixture.storage as &dyn Storage,
        fixture.conversation,
        &**BACKGROUND_CONTEXT,
        None,
    )
    .await
    .expect("bounds")
    .expect("some bounds");
    let active = active_entries(
        &*fixture.storage as &dyn Storage,
        fixture.conversation,
        Some(&bounds),
        &**BACKGROUND_CONTEXT,
    )
    .await
    .expect("active entries");
    assert_eq!(active[0].id, reset.id);
    let view = derive_context(
        &*fixture.storage as &dyn Storage,
        fixture.conversation,
        Some(&bounds),
        &**BACKGROUND_CONTEXT,
    )
    .await
    .expect("derive");
    assert_eq!(ids(&view.entries), ids(&active));
}

#[tokio::test]
async fn order_tool_results_is_idempotent() {
    let messages = vec![
        user("q"),
        assistant_with("calling", &["b", "a"], StopReason::ToolUse),
        tool_result("b", "result b"),
        tool_result("a", "result a"),
        assistant("done"),
    ];
    let once = order_tool_results(&messages);
    let twice = order_tool_results(&once);
    assert_eq!(once, twice);
}

#[tokio::test]
async fn a_missing_result_is_synthesized_after_a_reset() {
    let fixture = Fixture::open().await;
    fixture
        .message(assistant_with("calling", &["lost"], StopReason::ToolUse))
        .await;
    fixture
        .append(EntryDraft {
            key_order: None,
            kind: "reset".to_owned(),
            model: Some(vec![user("cut")]),
            data: None,
            head: Some(EntryHead::SelfHead),
            edits: None,
        })
        .await;
    let view = fixture.view(None).await;
    // 切割后的范围里没有这次调用，因此没有结果。
    assert_eq!(describe(&view.messages), vec!["user:cut"]);
}

#[tokio::test]
async fn user_entry_tokens_append_the_same_draft() {
    let fixture = Fixture::open().await;
    let conversation = fixture.conversation;
    let record: EntryRecord = commit(&fixture.session, move |transaction| {
        Box::pin(async move {
            Ok(transaction
                .append_entry_typed(
                    &UserEntry,
                    conversation,
                    kiana_durable::TypedEntryDraft {
                        model: Some(vec![user("token")]),
                        data: kiana_durable::NoData,
                        head: None,
                        edits: None,
                    },
                )
                .await?
                .record)
        })
    })
    .await
    .expect("typed append succeeds");
    assert_eq!(record.kind, "kiana.user");
    let view = fixture.view(None).await;
    assert_eq!(describe(&view.messages), vec!["user:token"]);
}

#[tokio::test]
async fn read_context_without_entries_returns_an_empty_view() -> Result<(), BoxError> {
    let fixture = Fixture::open().await;
    let view = fixture.view(None).await;
    assert!(view.head.is_none());
    assert!(view.entries.is_empty());
    assert!(view.messages.is_empty());
    Ok(())
}

// TS: "keeps system messages other than the baseline in place once an assistant message precedes them"（#10904）。
#[tokio::test]
async fn keeps_system_messages_other_than_the_baseline_in_place_after_an_assistant() {
    let fixture = Fixture::open().await;
    fixture.message(user("first")).await;
    fixture.message(assistant("answer")).await;
    // 另一种 kind 的 system 消息是位置性的；在它之后的基线不是第一条 system 消息。
    fixture
        .append(draft(
            "host.note",
            vec![system_section("note", "From here on.")],
        ))
        .await;
    fixture
        .system(system_section("preamble", "Baseline."))
        .await;
    assert_eq!(
        describe(&fixture.view(None).await.messages),
        vec![
            "user:first",
            "assistant:answer",
            "system:note",
            "system:preamble",
        ]
    );

    // 保留另一种 kind 的 system 消息的压缩之后，新基线不会移到它前面。
    let kept = fixture.message(user("kept")).await;
    fixture.message(assistant("kept answer")).await;
    fixture
        .append(draft("host.note", vec![system_section("mode", "plan")]))
        .await;
    fixture
        .append(EntryDraft {
            key_order: None,
            kind: "summary".to_owned(),
            model: Some(vec![user("summary")]),
            data: None,
            head: Some(EntryHead::Id(kept.id)),
            edits: None,
        })
        .await;
    fixture.message(user("next")).await;
    fixture
        .system(system_section("preamble", "Baseline."))
        .await;
    assert_eq!(
        describe(&fixture.view(None).await.messages),
        vec![
            "user:summary",
            "user:kept",
            "assistant:kept answer",
            "system:mode",
            "user:next",
            "system:preamble",
        ]
    );
}

// TS: "does not lead with a first system entry written after another system message, even once that is omitted"（#10904）。
#[tokio::test]
async fn does_not_lead_with_a_system_entry_written_after_another_system_message() {
    let fixture = Fixture::open().await;
    let host = fixture
        .append(draft(
            "host.system",
            vec![system_section("legacy", "Use the legacy format.")],
        ))
        .await;
    fixture.message(user("first")).await;
    fixture.message(assistant("answer")).await;
    // generation 针对宿主的消息写了这一条：它只移除一个分段，因此不是基线。
    fixture.system(system_update("legacy", None)).await;
    fixture
        .append(edit(host.id, ContextEditAction::Omit, None))
        .await;
    assert_eq!(
        describe(&fixture.view(None).await.messages),
        vec!["user:first", "assistant:answer", "system:legacy"]
    );

    // 编辑放在它前面的一条 system 消息也一样。
    let other = Fixture::open().await;
    let first = other.message(user("first")).await;
    other
        .append(edit(
            first.id,
            ContextEditAction::Replace,
            Some(vec![user("first"), system_section("note", "Note.")]),
        ))
        .await;
    other.message(assistant("answer")).await;
    other.system(system_update("note", None)).await;
    other
        .append(edit(
            first.id,
            ContextEditAction::Replace,
            Some(vec![user("first")]),
        ))
        .await;
    assert_eq!(
        describe(&other.view(None).await.messages),
        vec!["user:first", "assistant:answer", "system:note"]
    );
}

// TS: "does not lead with a retained, edited, or later system entry in place of the baseline"（#10904）。
#[tokio::test]
async fn does_not_lead_with_a_retained_edited_or_later_system_entry() {
    let fixture = Fixture::open().await;
    let kept = fixture.message(user("kept")).await;
    fixture.message(assistant("answer")).await;
    let retained = fixture.system(system_section("cwd", "/repo")).await;
    // generation 在 head 标记之后写基线之前，保留的 system 条目保持原位。
    fixture
        .append(EntryDraft {
            key_order: None,
            kind: "summary".to_owned(),
            model: Some(vec![user("summary")]),
            data: None,
            head: Some(EntryHead::Id(kept.id)),
            edits: None,
        })
        .await;
    fixture.message(user("next")).await;
    assert_eq!(
        describe(&fixture.view(None).await.messages),
        vec![
            "user:summary",
            "user:kept",
            "assistant:answer",
            "system:cwd",
            "user:next",
        ]
    );

    // 基线省略保留的条目（generation 写它时如此），并领到最前。
    let baseline = fixture
        .append(EntryDraft {
            key_order: None,
            kind: "kiana.system".to_owned(),
            model: Some(vec![system_section("preamble", "Baseline.")]),
            data: None,
            head: None,
            edits: Some(vec![ContextEdit {
                target: retained.id,
                action: ContextEditAction::Omit,
                messages: None,
            }]),
        })
        .await;
    fixture.message(assistant("done")).await;
    fixture.system(system_update("preamble", None)).await;
    assert_eq!(
        describe(&fixture.view(None).await.messages),
        vec![
            "system:preamble",
            "user:summary",
            "user:kept",
            "assistant:answer",
            "user:next",
            "assistant:done",
            "system:preamble",
        ]
    );

    // 被省略的基线没有可领的消息；它之后的增量不是基线，保持原位。
    fixture
        .append(edit(baseline.id, ContextEditAction::Omit, None))
        .await;
    assert_eq!(
        describe(&fixture.view(None).await.messages),
        vec![
            "user:summary",
            "user:kept",
            "assistant:answer",
            "user:next",
            "assistant:done",
            "system:preamble",
        ]
    );

    // 基线被替换也不是它。
    fixture
        .append(edit(
            baseline.id,
            ContextEditAction::Replace,
            Some(vec![system_section("other", "Other.")]),
        ))
        .await;
    assert_eq!(
        describe(&fixture.view(None).await.messages),
        vec![
            "user:summary",
            "user:kept",
            "assistant:answer",
            "user:next",
            "system:other",
            "assistant:done",
            "system:preamble",
        ]
    );
}

// 回归（minor）：TS 用对象身份（`system !== baseline`）判断第一条 system 消息是不是基线；值相等
// 的、由编辑替换产生的 system 消息不是基线，即使前面已有 assistant 也保持原位。
#[tokio::test]
async fn a_value_identical_system_message_from_an_edit_is_not_the_baseline() {
    let fixture = Fixture::open().await;
    let kept = fixture.message(user("kept")).await;
    // head 标记：findBaseline 只看它之后的条目。
    fixture
        .append(EntryDraft {
            key_order: None,
            kind: "summary".to_owned(),
            model: Some(vec![user("summary")]),
            data: None,
            head: Some(EntryHead::Id(kept.id)),
            edits: None,
        })
        .await;
    let answer = fixture.message(assistant("a")).await;
    // 编辑把一条与基线**值完全相同**的 system 消息放到 assistant 之后。
    fixture
        .append(edit(
            answer.id,
            ContextEditAction::Replace,
            Some(vec![
                assistant("a2"),
                system_section("preamble", "Baseline."),
            ]),
        ))
        .await;
    // 基线：head 之后第一条 kiana.system 条目，内容与上一条完全相同。
    fixture
        .system(system_section("preamble", "Baseline."))
        .await;

    assert_eq!(
        describe(&fixture.view(None).await.messages),
        vec![
            "user:summary",
            "user:kept",
            "assistant:a2",
            "system:preamble",
            "system:preamble",
        ]
    );
}

// 回归（minor）：`ContextRange.entries` 必须是 `selectActive` 之前的原始范围；否则被过滤掉的
// 旧 head 标记携带的 edits 会在截断/扩展读取里丢失，结果与全新读取不同。
#[tokio::test]
async fn a_truncated_read_keeps_edits_carried_by_older_head_markers() {
    let fixture = Fixture::open().await;
    let one = fixture.message(user("one")).await;
    // 旧 head 标记（id 大于 e1），同时用 edit 省略 e1。
    fixture
        .append(EntryDraft {
            key_order: None,
            kind: "summary".to_owned(),
            model: Some(vec![user("summary old")]),
            data: None,
            head: Some(EntryHead::Id(one.id)),
            edits: Some(vec![ContextEdit {
                target: one.id,
                action: ContextEditAction::Omit,
                messages: None,
            }]),
        })
        .await;
    fixture.message(user("after")).await;
    // 更新的 head 标记，指向更早的条目（覆盖旧 head 标记，但旧标记仍在范围里）。
    fixture
        .append(EntryDraft {
            key_order: None,
            kind: "summary".to_owned(),
            model: Some(vec![user("summary new")]),
            data: None,
            head: Some(EntryHead::Id(one.id)),
            edits: None,
        })
        .await;
    let tail1 = fixture.message(user("tail1")).await;
    fixture.message(user("tail2")).await;

    let ctx = &**BACKGROUND_CONTEXT;
    let storage = &*fixture.storage as &dyn Storage;
    let (_, range) = read_context_from(
        &fixture.session,
        storage,
        fixture.conversation,
        ctx,
        None,
        None,
    )
    .await
    .expect("first read");
    let range = range.expect("a range is returned");
    // 原始范围包含两个 head 标记。
    assert_eq!(range.entries.len(), 6);

    let (view, _) = read_context_from(
        &fixture.session,
        storage,
        fixture.conversation,
        ctx,
        Some(tail1.id),
        Some(&range),
    )
    .await
    .expect("truncated read");
    assert_eq!(
        describe(&view.messages),
        vec!["user:summary new", "user:after", "user:tail1"]
    );
    // 与一次全新的截断读取一致。
    assert_eq!(
        describe(&fixture.view(Some(tail1.id)).await.messages),
        describe(&view.messages)
    );
}
