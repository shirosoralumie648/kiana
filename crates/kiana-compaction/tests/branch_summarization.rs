//! `branch-summarization.test.ts` 的 Rust 移植，以及 `collectEntriesForBranchSummary` /
//! `prepareBranchEntries` 的规格派生测试。
//!
//! TS: `packages/coding-agent/test/branch-summarization.test.ts`
//!
//! 说明：TS 没有为 `collectEntriesForBranchSummary` 与 `prepareBranchEntries` 直接编写测试
//! （它们只被 agent-session 的树导航测试间接覆盖），这里按 `spec/ca-tools-compaction-export.md`
//! §2.21.2 / §2.21.4 的语义补充用例。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kiana_agent_core::types::{AgentMessage, StreamFn};
use kiana_ai::event_stream::create_assistant_message_event_stream;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, CacheRetention,
    DoneReason, JsonValue, Message, Model, ModelCost, ModelInputModality, StopReason, TextContent,
    TextKind, ToolCall, ToolCallKind, ToolChoice, TranscriptContext, Usage, UserContent,
    UserMessage, UserRole,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_ai::utils::retry::{RetryCallbacks, RetryPolicy};
use kiana_compaction::{
    BranchPreparation, CollectEntriesResult, FileValue, GenerateBranchSummaryOptions,
    SUMMARIZATION_SYSTEM_PROMPT, collect_entries_for_branch_summary, compute_file_lists,
    format_file_operations, generate_branch_summary, prepare_branch_entries,
};
use kiana_session_core::session_manager::{
    BranchSummaryEntry, BranchSummaryType, CompactionEntry, CompactionType, SessionEntry,
    SessionManager, SessionMessageEntry, SessionMessageType,
};

fn create_model() -> Model {
    Model {
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        base_url: "https://api.anthropic.com".to_string(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(200_000),
        max_tokens: Some(8192),
        ..Model::default()
    }
}

fn message_entry(id: &str, parent_id: Option<&str>, message: AgentMessage) -> SessionEntry {
    SessionEntry::Message(SessionMessageEntry {
        kind: SessionMessageType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "1970-01-01T00:00:00.001Z".to_string(),
        message,
        raw_message: None,
    })
}

fn branch_entries() -> Vec<SessionEntry> {
    vec![message_entry(
        "branch-user",
        None,
        AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("Abandoned request".to_string()),
            timestamp: 1,
        })),
    )]
}

fn text_content(text: &str) -> Vec<AssistantContentBlock> {
    vec![AssistantContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })]
}

fn response(content: Vec<AssistantContentBlock>) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: create_model().api,
        provider: create_model().provider,
        model: create_model().id,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        timestamp: 0,
        ..AssistantMessage::default()
    }
}

#[derive(Debug, Clone, Default)]
struct RecordedOptions {
    cache_retention: Option<CacheRetention>,
    session_id: Option<String>,
    tool_choice: Option<ToolChoice>,
    reasoning: Option<kiana_ai::types::ThinkingLevel>,
    api_key: Option<String>,
}

#[derive(Default)]
struct Recorder {
    options: Vec<RecordedOptions>,
    contexts: Vec<TranscriptContext>,
}

/// 假 `StreamFn`：记录请求选项并同步推入一条 `done` 事件。
fn recording_stream_fn(response: AssistantMessage) -> (StreamFn, Arc<Mutex<Recorder>>) {
    let recorder: Arc<Mutex<Recorder>> = Arc::new(Mutex::new(Recorder::default()));
    let recorder_for_fn = Arc::clone(&recorder);
    let stream_fn: StreamFn = Arc::new(move |_model, context, options| {
        {
            let mut state = recorder_for_fn.lock().expect("recorder");
            if let Some(options) = options.as_ref() {
                state.options.push(RecordedOptions {
                    cache_retention: options.base.cache_retention,
                    session_id: options.base.session_id.clone(),
                    tool_choice: options.tool_choice,
                    reasoning: options.reasoning,
                    api_key: options.base.api_key.clone(),
                });
            }
            state.contexts.push(context.clone());
        }
        let stream = create_assistant_message_event_stream();
        let _ = stream.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: Box::new(response.clone()),
        });
        Box::pin(async move { Ok(stream) })
    });
    (stream_fn, recorder)
}

fn recorded(recorder: &Arc<Mutex<Recorder>>) -> Vec<RecordedOptions> {
    recorder.lock().expect("recorder").options.clone()
}

fn recorded_contexts(recorder: &Arc<Mutex<Recorder>>) -> Vec<TranscriptContext> {
    recorder.lock().expect("recorder").contexts.clone()
}

/// 摘要请求的 system prompt 与 user prompt 文本。
fn recorded_prompts(recorder: &Arc<Mutex<Recorder>>) -> (String, String) {
    let contexts = recorded_contexts(recorder);
    let context = contexts.first().expect("request context");
    let context = context.as_context();
    // `normalizeContext` 把 systemPrompt 转成首部 system 消息。
    let system = context
        .messages
        .iter()
        .find_map(|message| match message {
            Message::System(system) => Some(kiana_ai::utils::text::content_text_system(
                &system.content,
                "\n",
            )),
            _ => None,
        })
        .unwrap_or_default();
    let prompt = context
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(kiana_ai::utils::text::content_text_user_content(
                &user.content,
                "",
            )),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    (system, prompt)
}

fn options(stream_fn: StreamFn) -> GenerateBranchSummaryOptions {
    GenerateBranchSummaryOptions {
        model: create_model(),
        api_key: None,
        headers: None,
        env: None,
        signal: AbortScope::new(),
        custom_instructions: None,
        replace_instructions: None,
        reserve_tokens: None,
        stream_fn: Some(stream_fn),
        retry: None,
        callbacks: None,
    }
}

/// TS: it("does not override tool choice for branch summaries")
#[tokio::test]
async fn does_not_override_tool_choice_for_branch_summaries() {
    let (stream_fn, recorder) = recording_stream_fn(response(text_content("summary")));

    generate_branch_summary(&branch_entries(), &options(stream_fn))
        .await
        .expect("branch summary");

    let calls = recorded(&recorder);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].tool_choice, None);
    // 分支摘要的请求选项只有 apiKey/headers/env/signal；reasoning 与 sessionId 不由调用方设置
    // （completeSummarization 会补 cacheRetention:"none" 与新的 sessionId）。
    assert_eq!(calls[0].reasoning, None);
    assert_eq!(calls[0].api_key, None);
    assert_eq!(calls[0].cache_retention, Some(CacheRetention::None));
    assert!(calls[0].session_id.is_some());
}

/// TS: it("rejects tool calls from branch summaries")
#[tokio::test]
async fn rejects_tool_calls_from_branch_summaries() {
    let tool_call_response = response(vec![AssistantContentBlock::ToolCall(ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: "tool-call-1".to_string(),
        name: "read".to_string(),
        arguments: indexmap::IndexMap::from([(
            "path".to_string(),
            JsonValue::String("README.md".to_string()),
        )]),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    })]);
    let (stream_fn, _recorder) = recording_stream_fn(tool_call_response);

    let result = generate_branch_summary(&branch_entries(), &options(stream_fn))
        .await
        .expect("branch summary");

    assert_eq!(
        result.error.as_deref(),
        Some("Branch summarization attempted to call a tool")
    );
}

/// TS: it("rejects length-limited branch summaries")
#[tokio::test]
async fn rejects_length_limited_branch_summaries() {
    let mut length_response = response(text_content("partial"));
    length_response.stop_reason = StopReason::Length;
    let (stream_fn, _recorder) = recording_stream_fn(length_response);

    let result = generate_branch_summary(&branch_entries(), &options(stream_fn))
        .await
        .expect("branch summary");

    assert_eq!(
        result.error.as_deref(),
        Some(
            "Branch summarization failed: generation hit the token cap and the summary is incomplete"
        )
    );
}

/// TS: `response.stopReason === "aborted"` 时只返回 `{ aborted: true }`。
#[tokio::test]
async fn aborted_branch_summaries_report_aborted() {
    let mut aborted = response(text_content("partial"));
    aborted.stop_reason = StopReason::Aborted;
    let (stream_fn, _recorder) = recording_stream_fn(aborted);

    let result = generate_branch_summary(&branch_entries(), &options(stream_fn))
        .await
        .expect("branch summary");

    assert_eq!(result.aborted, Some(true));
    assert!(result.summary.is_none());
    assert!(result.error.is_none());
    assert!(result.usage.is_none());
    assert!(result.read_files.is_none());
}

/// TS: stopReason "error" 时返回 `Branch summarization failed: <errorMessage>`。
#[tokio::test]
async fn branch_summaries_report_error_stop_reason() {
    let mut failure = response(text_content("partial"));
    failure.stop_reason = StopReason::Error;
    failure.error_message = Some("boom".to_string());
    let (stream_fn, _recorder) = recording_stream_fn(failure);

    let result = generate_branch_summary(&branch_entries(), &options(stream_fn))
        .await
        .expect("branch summary");

    assert_eq!(
        result.error.as_deref(),
        Some("Branch summarization failed: boom")
    );
    assert!(result.summary.is_none());
    assert!(result.aborted.is_none());
}

#[tokio::test]
async fn appends_the_preamble_and_file_lists_in_order() {
    let assistant = AgentMessage::Standard(Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![
            AssistantContentBlock::ToolCall(ToolCall {
                kind_tool_call: ToolCallKind::ToolCall,
                id: "call-read".to_string(),
                name: "read".to_string(),
                arguments: indexmap::IndexMap::from([(
                    "path".to_string(),
                    JsonValue::String("a.ts".to_string()),
                )]),
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                kind_tool_call: ToolCallKind::ToolCall,
                id: "call-edit".to_string(),
                name: "edit".to_string(),
                arguments: indexmap::IndexMap::from([(
                    "path".to_string(),
                    JsonValue::String("b.ts".to_string()),
                )]),
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            }),
        ],
        api: "anthropic".to_string(),
        provider: "anthropic".to_string(),
        model: "test".to_string(),
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        timestamp: 0,
        ..AssistantMessage::default()
    }));
    let entries = vec![
        message_entry(
            "branch-user",
            None,
            AgentMessage::Standard(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("Abandoned request".to_string()),
                timestamp: 1,
            })),
        ),
        message_entry("branch-assistant", Some("branch-user"), assistant),
    ];
    let (stream_fn, _recorder) = recording_stream_fn(response(text_content("summary text")));

    let result = generate_branch_summary(&entries, &options(stream_fn))
        .await
        .expect("branch summary");

    assert_eq!(
        result.summary.as_deref(),
        Some(concat!(
            "The user explored a different conversation branch before returning here.\n",
            "Summary of that exploration:\n",
            "\n",
            "summary text",
            "\n\n<read-files>\na.ts\n</read-files>\n\n<modified-files>\nb.ts\n</modified-files>"
        ))
    );
    assert_eq!(result.read_files, Some(vec![FileValue::from("a.ts")]));
    assert_eq!(result.modified_files, Some(vec![FileValue::from("b.ts")]));
    assert!(result.error.is_none());
    assert!(result.aborted.is_none());
}

/// TS: `generateBranchSummary` 的 prompt 组装（spec §2.21.6 第 3/6/7 步）。
#[tokio::test]
async fn sends_the_ts_prompt_format_and_system_prompt() {
    let (stream_fn, recorder) = recording_stream_fn(response(text_content("summary")));

    generate_branch_summary(&branch_entries(), &options(stream_fn))
        .await
        .expect("branch summary");

    let (system, prompt) = recorded_prompts(&recorder);
    assert_eq!(system, SUMMARIZATION_SYSTEM_PROMPT);
    assert!(prompt.starts_with(
        "<conversation>\n[User]: Abandoned request\n</conversation>\n\nCreate a structured summary of this conversation branch"
    ));
    assert!(prompt.ends_with(
        "Keep each section concise. Preserve exact file paths, function names, and error messages."
    ));
    // 没有自定义指令时不追加 Additional focus。
    assert!(!prompt.contains("Additional focus:"));
}

/// TS: `customInstructions` 默认追加到 prompt；`replaceInstructions` 为 true 时完全替换。
#[tokio::test]
async fn custom_instructions_are_appended_unless_replaced() {
    let (stream_fn, recorder) = recording_stream_fn(response(text_content("summary")));
    let mut appended = options(stream_fn);
    appended.custom_instructions = Some("focus on migrations".to_string());

    generate_branch_summary(&branch_entries(), &appended)
        .await
        .expect("branch summary");

    let (_, prompt) = recorded_prompts(&recorder);
    assert!(prompt.ends_with(
        "Keep each section concise. Preserve exact file paths, function names, and error messages.\n\nAdditional focus: focus on migrations"
    ));

    let (stream_fn, recorder) = recording_stream_fn(response(text_content("summary")));
    let mut replaced = options(stream_fn);
    replaced.custom_instructions = Some("only this".to_string());
    replaced.replace_instructions = Some(true);

    generate_branch_summary(&branch_entries(), &replaced)
        .await
        .expect("branch summary");

    let (_, prompt) = recorded_prompts(&recorder);
    assert!(prompt.ends_with("</conversation>\n\nonly this"));
    assert!(!prompt.contains("Additional focus:"));
}

/// TS: `messages.length === 0` 时不调用 LLM，返回固定文本且无 usage/readFiles。
#[tokio::test]
async fn empty_entries_return_no_content_to_summarize() {
    let (stream_fn, recorder) = recording_stream_fn(response(text_content("unused")));

    let result = generate_branch_summary(&[], &options(stream_fn))
        .await
        .expect("branch summary");

    assert_eq!(result.summary.as_deref(), Some("No content to summarize"));
    assert!(result.usage.is_none());
    assert!(result.read_files.is_none());
    assert!(result.modified_files.is_none());
    assert!(result.error.is_none());
    assert!(result.aborted.is_none());
    assert!(recorded(&recorder).is_empty());
}

/// TS: `tokenBudget = (model.contextWindow || 128000) - reserveTokens`。
///
/// 预算不足时只保留最新的消息；`contextWindow` 缺省时回退到 128000，全部保留。
#[tokio::test]
async fn token_budget_uses_context_window_and_reserve_tokens() {
    let entries = vec![
        user_entry("old", None, "old message that is long"),
        user_entry("newest", Some("old"), "newest"),
    ];

    let mut small_window = options(recording_stream_fn(response(text_content("summary"))).0);
    small_window.model.context_window = Some(30);
    small_window.reserve_tokens = Some(25);
    let (stream_fn, recorder) = recording_stream_fn(response(text_content("summary")));
    small_window.stream_fn = Some(stream_fn);

    generate_branch_summary(&entries, &small_window)
        .await
        .expect("branch summary");

    let (_, prompt) = recorded_prompts(&recorder);
    assert!(prompt.contains("[User]: newest"));
    assert!(!prompt.contains("old message"));

    // `contextWindow` 为 0 或缺失时都回退到 128000，预算足够包含两条消息。
    for context_window in [None, Some(0)] {
        let (stream_fn, recorder) = recording_stream_fn(response(text_content("summary")));
        let mut generous = options(stream_fn);
        generous.model.context_window = context_window;
        generate_branch_summary(&entries, &generous)
            .await
            .expect("branch summary");
        let (_, prompt) = recorded_prompts(&recorder);
        assert!(prompt.contains("old message that is long"));
        assert!(prompt.contains("[User]: newest"));
    }
}

// ============================================================================
// Spec-derived tests for collectEntriesForBranchSummary
// ============================================================================

fn in_memory_session() -> SessionManager {
    SessionManager::in_memory("", None, None).expect("in-memory session")
}

fn append_user(session: &mut SessionManager, text: &str) -> String {
    session
        .append_message(AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(text.to_string()),
            timestamp: 0,
        })))
        .expect("append message")
}

#[test]
fn collect_entries_returns_empty_without_an_old_leaf() {
    let session = in_memory_session();
    let result = collect_entries_for_branch_summary(&session, None, "target");
    assert_eq!(result, CollectEntriesResult::default());
    let result = collect_entries_for_branch_summary(&session, Some(""), "target");
    assert_eq!(result, CollectEntriesResult::default());
}

#[test]
fn collect_entries_walks_back_to_the_deepest_common_ancestor() {
    let mut session = in_memory_session();
    let first = append_user(&mut session, "first");
    let second = append_user(&mut session, "second");
    let _main = append_user(&mut session, "main branch");

    // 从 second 处分叉，追加 new branch。
    session.branch(&second).expect("branch");
    let new_branch = append_user(&mut session, "new branch");

    let result = collect_entries_for_branch_summary(&session, Some(&new_branch), &first);

    assert_eq!(result.common_ancestor_id.as_deref(), Some(first.as_str()));
    // second 在共同祖先之后，必须被收集；顺序为时间顺序。
    let ids: Vec<&str> = result.entries.iter().map(SessionEntry::id).collect();
    assert_eq!(ids, vec![second.as_str(), new_branch.as_str()]);
}

#[test]
fn collect_entries_does_not_stop_at_compaction_boundaries() {
    let mut session = in_memory_session();
    let first = append_user(&mut session, "before compaction");
    let second = append_user(&mut session, "kept");
    let compaction = session
        .append_compaction("summary", Some(&first), 1234.0, None, None, None)
        .expect("append compaction");
    let after = append_user(&mut session, "after compaction");

    let result = collect_entries_for_branch_summary(&session, Some(&after), &first);

    assert_eq!(result.common_ancestor_id.as_deref(), Some(first.as_str()));
    let ids: Vec<&str> = result.entries.iter().map(SessionEntry::id).collect();
    assert_eq!(
        ids,
        vec![second.as_str(), compaction.as_str(), after.as_str()]
    );
    // 压缩条目本身被保留（其摘要作为上下文一起总结）。
    assert_eq!(result.entries[1].kind(), "compaction");
}

// ============================================================================
// Spec-derived tests for prepareBranchEntries
// ============================================================================

fn user_entry(id: &str, parent_id: Option<&str>, text: &str) -> SessionEntry {
    message_entry(
        id,
        parent_id,
        AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(text.to_string()),
            timestamp: 0,
        })),
    )
}

fn compaction_entry(id: &str, parent_id: Option<&str>, summary: &str) -> SessionEntry {
    SessionEntry::Compaction(CompactionEntry {
        kind: CompactionType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "1970-01-01T00:00:00.001Z".to_string(),
        summary: summary.to_string(),
        first_kept_entry_id: id.to_string(),
        tokens_before: 1000.0,
        details: None,
        usage: None,
        from_hook: None,
        system_message: None,
        first_kept_entry_index: None,
        raw_entry: None,
    })
}

fn branch_summary_entry(
    id: &str,
    parent_id: Option<&str>,
    summary: &str,
    details: Option<JsonValue>,
    from_hook: Option<bool>,
) -> SessionEntry {
    SessionEntry::BranchSummary(BranchSummaryEntry {
        kind: BranchSummaryType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "1970-01-01T00:00:00.001Z".to_string(),
        from_id: "x".to_string(),
        summary: summary.to_string(),
        details,
        usage: None,
        from_hook,
        raw_entry: None,
    })
}

fn message_texts(preparation: &BranchPreparation) -> Vec<String> {
    preparation
        .messages
        .iter()
        .map(|message| match message {
            AgentMessage::Standard(Message::User(user)) => match &user.content {
                UserContent::Text(text) => text.clone(),
                UserContent::Blocks(_) => String::new(),
            },
            AgentMessage::Custom(object) => object
                .get("summary")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string(),
            _ => String::new(),
        })
        .collect()
}

#[test]
fn prepare_branch_entries_keeps_the_newest_messages_in_chronological_order() {
    let entries = vec![
        user_entry("a", None, "old"),
        user_entry("b", Some("a"), "middle"),
        user_entry("c", Some("b"), "newest"),
    ];

    // "old"/"middle"/"newest" 各 2 token；预算 2 只放得下最新一条。
    let preparation = prepare_branch_entries(&entries, 2);
    assert_eq!(message_texts(&preparation), vec!["newest"]);
    assert_eq!(preparation.total_tokens, 2.0);
}

#[test]
fn prepare_branch_entries_keeps_summary_entries_when_below_ninety_percent() {
    // 400 字符摘要 = 100 token；用户消息 18 token；预算 110。
    let entries = vec![
        compaction_entry("compaction", None, &"s".repeat(400)),
        user_entry("user", Some("compaction"), &"u".repeat(72)),
    ];

    let preparation = prepare_branch_entries(&entries, 110);

    assert_eq!(preparation.messages.len(), 2);
    assert_eq!(message_texts(&preparation)[0], "s".repeat(400));
    assert_eq!(preparation.total_tokens, 118.0);
}

#[test]
fn prepare_branch_entries_drops_summary_entries_at_the_ninety_percent_boundary() {
    // 18 token 已用，恰好是预算 20 的 90%：`totalTokens < tokenBudget * 0.9` 为 false。
    let entries = vec![
        compaction_entry("compaction", None, &"s".repeat(400)),
        user_entry("user", Some("compaction"), &"u".repeat(72)),
    ];

    let preparation = prepare_branch_entries(&entries, 20);

    assert_eq!(message_texts(&preparation), vec!["u".repeat(72)]);
    assert_eq!(preparation.total_tokens, 18.0);
}

#[test]
fn prepare_branch_entries_treats_zero_and_negative_budgets_as_unlimited() {
    let entries = vec![
        user_entry("a", None, "old"),
        user_entry("b", Some("a"), "newest"),
    ];

    for budget in [0, -1, -100_000] {
        let preparation = prepare_branch_entries(&entries, budget);
        assert_eq!(
            message_texts(&preparation),
            vec!["old".to_string(), "newest".to_string()]
        );
        // "old" 3 字符 = 1 token；"newest" 6 字符 = 2 token。
        assert_eq!(preparation.total_tokens, 3.0);
    }
}

#[test]
fn prepare_branch_entries_collects_file_ops_from_branch_summary_details() {
    let details = serde_json::json!({
        "readFiles": ["read.ts"],
        "modifiedFiles": ["modified.ts"],
    });
    let entries = vec![
        branch_summary_entry("summary", None, "branch summary text", Some(details), None),
        user_entry("user", Some("summary"), "user text"),
    ];

    let preparation = prepare_branch_entries(&entries, 0);

    assert!(preparation.file_ops.read.contains("read.ts"));
    // modifiedFiles 只进入 edited（与 TS 代码一致）。
    assert!(preparation.file_ops.edited.contains("modified.ts"));
    assert!(!preparation.file_ops.written.contains("modified.ts"));
}

#[test]
fn prepare_branch_entries_skips_hook_generated_summary_details() {
    let details = serde_json::json!({ "readFiles": ["hook.ts"] });
    let entries = vec![branch_summary_entry(
        "summary",
        None,
        "hook summary",
        Some(details),
        Some(true),
    )];

    let preparation = prepare_branch_entries(&entries, 0);

    assert!(!preparation.file_ops.read.contains("hook.ts"));
}

/// Rust-only regression：`details.readFiles` / `modifiedFiles` 里的非字符串元素保留原始 JSON。
///
/// TS 只检查 `Array.isArray`，元素原样进 `Set`，渲染时按 JS 规则转换：`null` 的排序键是
/// `"null"`、`join` 却是空串；数字 `123` 与字符串 `"123"` 是两个元素。期望值由 TS 参考
/// 实现生成（`prepareBranchEntries` -> `computeFileLists` -> `formatFileOperations`）。
#[test]
fn prepare_branch_entries_keeps_non_string_file_details() {
    let details = serde_json::json!({
        "readFiles": [null, 123, "123", true, ["a", null], { "x": 1 }, "a.ts"],
        "modifiedFiles": [null, 123, "123"],
    });
    let entries = vec![branch_summary_entry(
        "summary",
        None,
        "branch summary text",
        Some(details),
        None,
    )];

    let preparation = prepare_branch_entries(&entries, 0);

    assert_eq!(
        preparation
            .file_ops
            .read
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            JsonValue::Null,
            serde_json::json!(123),
            serde_json::json!("123"),
            serde_json::json!(true),
            serde_json::json!(["a", null]),
            serde_json::json!({ "x": 1 }),
            serde_json::json!("a.ts"),
        ]
    );
    assert_eq!(
        preparation
            .file_ops
            .edited
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            JsonValue::Null,
            serde_json::json!(123),
            serde_json::json!("123"),
        ]
    );

    let lists = compute_file_lists(&preparation.file_ops);
    assert_eq!(
        lists.read_files,
        vec![
            serde_json::json!({ "x": 1 }),
            serde_json::json!(["a", null]),
            serde_json::json!("a.ts"),
            serde_json::json!(true),
        ]
    );
    assert_eq!(
        lists.modified_files,
        vec![
            serde_json::json!(123),
            serde_json::json!("123"),
            JsonValue::Null,
        ]
    );
    assert_eq!(
        format_file_operations(&lists.read_files, &lists.modified_files),
        "\n\n<read-files>\n[object Object]\na,\na.ts\ntrue\n</read-files>\n\n<modified-files>\n123\n123\n\n</modified-files>"
    );
}

/// 记录重试回调事件（`scheduled` / `attempt` / `finished:<ok>`）。
fn recording_retry_callbacks(events: Arc<Mutex<Vec<String>>>) -> RetryCallbacks {
    let scheduled = Arc::clone(&events);
    let started = Arc::clone(&events);
    let finished = Arc::clone(&events);
    RetryCallbacks {
        on_retry_scheduled: Some(Arc::new(move |_, _, _, _| {
            scheduled
                .lock()
                .expect("events")
                .push("scheduled".to_string());
            Box::pin(async {})
        })),
        on_retry_attempt_start: Some(Arc::new(move || {
            started.lock().expect("events").push("attempt".to_string());
            Box::pin(async {})
        })),
        on_retry_finished: Some(Arc::new(move |success, _, _| {
            finished
                .lock()
                .expect("events")
                .push(format!("finished:{success}"));
            Box::pin(async {})
        })),
    }
}

/// Rust-only regression（TS 参考 probe）：`streamFn` 拒绝时 `generateBranchSummary` 的
/// Promise reject，原始错误文本冒泡；`retryAssistantCall` 不重试、不发 `onRetryFinished`。
#[tokio::test]
async fn hard_stream_errors_reject_branch_summaries() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        calls_for_fn.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(kiana_ai::AiError::new(
                kiana_ai::AiErrorKind::Other,
                "rate limit exceeded boom",
            ))
        })
    });
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut branch_options = options(stream_fn);
    branch_options.retry = Some(RetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 1,
        max_agent_delay_ms: None,
    });
    branch_options.callbacks = Some(recording_retry_callbacks(Arc::clone(&events)));

    let error = generate_branch_summary(&branch_entries(), &branch_options)
        .await
        .expect_err("hard stream error must reject");

    assert_eq!(error.to_string(), "rate limit exceeded boom");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(events.lock().expect("events").is_empty());
}

/// Rust-only regression：TS 的 `getMessageFromEntry` case `"branch_summary"` 无条件构造
/// `createBranchSummaryMessage`，空摘要也生成消息并进入 prompt。数字时间戳让 kiana-session-core
/// 降级为 `Unknown`，`session_entry_to_context_messages` 又跳过假值摘要；这里验证补回。
///
/// 期望值由 TS 参考生成：roles `[user, branchSummary, user]`、`totalTokens = 3`、
/// `timestamp = 123`（`new Date(123).getTime()`）。
#[test]
fn prepare_branch_entries_keeps_empty_summary_for_unknown_branch_summary() {
    let entries = unknown_empty_branch_summary_entries();
    assert!(matches!(entries[1], SessionEntry::Unknown(_)));

    let preparation = prepare_branch_entries(&entries, 0);

    assert_eq!(
        preparation
            .messages
            .iter()
            .map(AgentMessage::role)
            .collect::<Vec<_>>(),
        vec!["user", "branchSummary", "user"]
    );
    assert_eq!(preparation.total_tokens, 3.0);
    let agent_message = match &preparation.messages[1] {
        AgentMessage::Custom(object) => object,
        other => panic!(
            "expected branchSummary custom message, got {}",
            other.role()
        ),
    };
    assert_eq!(
        agent_message.get("summary"),
        Some(&JsonValue::String(String::new()))
    );
    assert_eq!(
        agent_message.get("fromId"),
        Some(&JsonValue::String("root".to_string()))
    );
    // `new Date(123).getTime()` 是 number 123（Rust 侧以 f64 数值承载）。
    assert_eq!(
        agent_message.get("timestamp").and_then(JsonValue::as_f64),
        Some(123.0)
    );
}

/// 空摘要的 `Unknown` `branch_summary` 仍出现在分支摘要 prompt 中（当前稳定行为，改动需同步测试）。
#[tokio::test]
async fn includes_empty_summary_for_unknown_branch_summary_in_prompt() {
    let entries = unknown_empty_branch_summary_entries();
    let (stream_fn, recorder) = recording_stream_fn(response(text_content("summary")));

    generate_branch_summary(&entries, &options(stream_fn))
        .await
        .expect("branch summary");

    let (_, prompt) = recorded_prompts(&recorder);
    assert!(prompt.contains(
        "[User]: hello\n\n[User]: The following is a summary of a branch that this conversation came back from:\n\n<summary>\n</summary>\n\n[User]: next"
    ));
}

fn unknown_empty_branch_summary_entries() -> Vec<SessionEntry> {
    serde_json::from_value(serde_json::json!([
        {
            "type": "message", "id": "u1", "parentId": null,
            "timestamp": "2025-01-01T00:00:00.000Z",
            "message": { "role": "user", "content": "hello", "timestamp": 1 }
        },
        {
            "type": "branch_summary", "id": "b1", "parentId": "u1",
            "timestamp": 123, "fromId": "root", "summary": ""
        },
        {
            "type": "message", "id": "u2", "parentId": "b1",
            "timestamp": "2025-01-01T00:00:02.000Z",
            "message": { "role": "user", "content": "next", "timestamp": 2 }
        }
    ]))
    .expect("entries")
}

/// Rust-only regression：kiana-session-core 降级为 `Unknown` 的 `message` 条目仍参与分支摘要。
///
/// TS 参考（`prepareBranchEntries(entries, 0)`）：2 条消息（user/assistant），
/// `totalTokens` 10，assistant 的 `read` toolCall 进入 `fileOps.read`。
#[test]
fn prepare_branch_entries_includes_entries_kiana_session_core_cannot_type() {
    let entries: Vec<SessionEntry> = serde_json::from_value(serde_json::json!([
        {
            "type": "message", "id": "u1", "parentId": null,
            "timestamp": 123456,
            "message": { "role": "user", "content": "hello world" }
        },
        {
            "type": "message", "id": "a1", "parentId": "u1",
            "timestamp": 123457,
            "message": {
                "role": "assistant",
                "content": [
                    { "type": "text", "text": "hi there" },
                    { "type": "toolCall", "id": "t", "name": "read", "arguments": { "path": "x.ts" } }
                ],
                "stopReason": "stop"
            }
        }
    ]))
    .expect("entries");
    assert!(matches!(entries[0], SessionEntry::Unknown(_)));
    assert!(matches!(entries[1], SessionEntry::Unknown(_)));

    let preparation = prepare_branch_entries(&entries, 0);

    assert_eq!(preparation.messages.len(), 2);
    assert_eq!(preparation.messages[0].role(), "user");
    assert_eq!(preparation.messages[1].role(), "assistant");
    assert_eq!(preparation.total_tokens, 10.0);
    assert!(preparation.file_ops.read.contains("x.ts"));
}

#[test]
fn prepare_branch_entries_extracts_tool_calls_even_from_the_message_that_exceeds_the_budget() {
    let assistant = AgentMessage::Standard(Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(ToolCall {
            kind_tool_call: ToolCallKind::ToolCall,
            id: "call-1".to_string(),
            name: "read".to_string(),
            arguments: indexmap::IndexMap::from([(
                "path".to_string(),
                JsonValue::String("excluded.ts".to_string()),
            )]),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        })],
        api: "anthropic".to_string(),
        provider: "anthropic".to_string(),
        model: "test".to_string(),
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        timestamp: 0,
        ..AssistantMessage::default()
    }));
    let entries = vec![message_entry("assistant", None, assistant)];

    let preparation = prepare_branch_entries(&entries, 1);

    // 消息放不进预算，但文件操作在预算检查之前提取（当前稳定行为）。
    assert!(preparation.messages.is_empty());
    assert!(preparation.file_ops.read.contains("excluded.ts"));
}

#[test]
fn prepare_branch_entries_skips_tool_results() {
    let tool_result =
        AgentMessage::Standard(Message::ToolResult(kiana_ai::types::ToolResultMessage {
            role: kiana_ai::types::ToolResultRole::ToolResult,
            tool_call_id: "call-1".to_string(),
            tool_name: "read".to_string(),
            content: vec![kiana_ai::types::UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "result".to_string(),
                text_signature: None,
            })],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 0,
            duration_ms: None,
        }));
    let entries = vec![message_entry("tool-result", None, tool_result)];

    let preparation = prepare_branch_entries(&entries, 0);

    assert!(preparation.messages.is_empty());
}

/// Rust-only regression：只有 `entry.type === "branch_summary"` 的条目才从 `details`
/// 收集文件列表。无法 typed 解析的非 branch_summary 条目（这里是数字 timestamp 导致的
/// `session_info` / `custom`，Rust 侧落在 `SessionEntry::Unknown`）在 TS 里同样不参与。
///
/// 同时确认 malformed 但 type 正确的 `branch_summary` 仍被收集（TS 只检查 type）。
#[test]
fn prepare_branch_entries_only_reads_details_from_branch_summary_entries() {
    let entries: Vec<SessionEntry> = serde_json::from_value(serde_json::json!([
        message_entry_json("e0", serde_json::Value::Null, "hello"),
        {
            "type": "session_info",
            "id": "e1",
            "parentId": "e0",
            "timestamp": 123,
            "name": "n",
            "details": {
                "readFiles": ["info-read.ts"],
                "modifiedFiles": ["info-mod.ts"]
            }
        },
        {
            "type": "custom",
            "id": "e2",
            "parentId": "e1",
            "timestamp": 456,
            "customType": "x",
            "data": {},
            "details": { "readFiles": ["custom-read.ts"] }
        },
        {
            "type": "branch_summary",
            "id": "e3",
            "parentId": "e2",
            "timestamp": 789,
            "summary": "raw branch summary",
            "fromId": "e2",
            "details": { "readFiles": ["raw-branch.ts"] }
        },
        {
            "type": "branch_summary",
            "id": "e4",
            "parentId": "e3",
            "timestamp": 790,
            "summary": "hook branch summary",
            "fromId": "e3",
            "fromHook": 1,
            "details": { "readFiles": ["raw-hook.ts"] }
        },
        message_entry_json("e5", serde_json::Value::String("e4".to_string()), "tail"),
    ]))
    .expect("entries");

    // 数字 timestamp 使 typed 解析失败，TS 仍然按 `type` 读取原始对象。
    assert!(matches!(entries[1], SessionEntry::Unknown(_)));
    assert!(matches!(entries[2], SessionEntry::Unknown(_)));
    assert_eq!(entries[3].kind(), "branch_summary");
    assert_eq!(entries[4].kind(), "branch_summary");

    let preparation = prepare_branch_entries(&entries, 0);

    assert!(!preparation.file_ops.read.contains("info-read.ts"));
    assert!(!preparation.file_ops.edited.contains("info-mod.ts"));
    assert!(!preparation.file_ops.read.contains("custom-read.ts"));
    assert!(preparation.file_ops.read.contains("raw-branch.ts"));
    // `fromHook: 1` 是 JS 真值：扩展生成的摘要 details 被跳过。
    assert!(!preparation.file_ops.read.contains("raw-hook.ts"));
}

/// `fromHook` 的 JS 真值判断（typed 能解析的 `false` / 缺省与 raw 的非布尔真值）。
#[test]
fn prepare_branch_entries_treats_truthy_non_boolean_from_hook_as_hook_generated() {
    fn branch_summary_json(
        id: &str,
        from_hook: Option<serde_json::Value>,
        read: &str,
    ) -> serde_json::Value {
        let mut entry = serde_json::json!({
            "type": "branch_summary",
            "id": id,
            "parentId": null,
            "timestamp": "1970-01-01T00:00:00.001Z",
            "summary": "branch summary",
            "fromId": "x",
            "details": { "readFiles": [read] }
        });
        if let Some(from_hook) = from_hook {
            entry["fromHook"] = from_hook;
        }
        entry
    }

    for (from_hook, collects) in [
        (Some(serde_json::json!(true)), false),
        (Some(serde_json::json!(1)), false),
        (Some(serde_json::json!("yes")), false),
        (Some(serde_json::json!(false)), true),
        (Some(serde_json::json!(0)), true),
        (Some(serde_json::Value::Null), true),
        (None, true),
    ] {
        let entries: Vec<SessionEntry> = serde_json::from_value(serde_json::json!([
            branch_summary_json("e0", from_hook.clone(), "details.ts"),
            message_entry_json("e1", serde_json::Value::String("e0".to_string()), "tail"),
        ]))
        .expect("entries");
        let preparation = prepare_branch_entries(&entries, 0);
        assert_eq!(
            preparation.file_ops.read.contains("details.ts"),
            collects,
            "fromHook={from_hook:?}"
        );
    }
}

/// 供上面的 JSON 场景使用的 message 条目字面量。
fn message_entry_json(id: &str, parent_id: serde_json::Value, text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "message",
        "id": id,
        "parentId": parent_id,
        "timestamp": "1970-01-01T00:00:00.001Z",
        "message": { "role": "user", "content": text, "timestamp": 1 }
    })
}
