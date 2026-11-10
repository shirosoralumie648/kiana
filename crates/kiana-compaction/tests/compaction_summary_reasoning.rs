//! `compaction-summary-reasoning.test.ts` 的 Rust 移植。
//!
//! TS 用 `vi.mock` 替换 `completeSimple`；Rust 侧改为注入假的 `StreamFn`，并断言它收到的
//! `SimpleStreamOptions`。
//!
//! TS: `packages/coding-agent/test/compaction-summary-reasoning.test.ts`

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kiana_agent_core::types::{AgentMessage, StreamFn, ThinkingLevel};
use kiana_ai::event_stream::create_assistant_message_event_stream;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, CacheRetention,
    Context, DoneReason, ErrorReason, JsonValue, Message, Model, ModelCost, ModelInputModality,
    SimpleStreamOptions, StopReason, TextContent, TextKind, ToolCall, ToolCallKind, ToolChoice,
    TranscriptContext, Usage, UsageCost, UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::retry::{RetryCallbacks, RetryPolicy};
use kiana_ai::utils::transcript::normalize_context;
use kiana_compaction::{
    CompactionPreparation, CompactionSettings, FileOperations, FileValue, GenerateSummaryOptions,
    compact, complete_summarization, generate_summary, generate_summary_with_usage,
};

fn create_model(reasoning: bool, max_tokens: u64, compat: Option<JsonValue>) -> Model {
    Model {
        id: if reasoning {
            "reasoning-model".to_string()
        } else {
            "non-reasoning-model".to_string()
        },
        name: if reasoning {
            "Reasoning Model".to_string()
        } else {
            "Non-reasoning Model".to_string()
        },
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        base_url: "https://api.anthropic.com".to_string(),
        reasoning: Some(reasoning),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(200_000),
        max_tokens: Some(max_tokens),
        compat,
        ..Model::default()
    }
}

fn mock_summary_response() -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "## Goal\nTest summary".to_string(),
            text_signature: None,
        })],
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: "claude-sonnet-4-5".to_string(),
        usage: Usage {
            input: 10,
            output: 10,
            total_tokens: 20,
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        timestamp: 0,
        ..AssistantMessage::default()
    }
}

fn mock_tool_call_response() -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContentBlock::ToolCall(ToolCall {
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
        })],
        stop_reason: StopReason::ToolUse,
        ..mock_summary_response()
    }
}

fn messages() -> Vec<AgentMessage> {
    vec![AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text("Summarize this.".to_string()),
        timestamp: 0,
    }))]
}

/// 记录下来的请求选项（`SimpleStreamOptions` 不是 `Clone`，只保留断言需要的字段）。
#[derive(Debug, Clone, Default)]
struct RecordedOptions {
    cache_retention: Option<CacheRetention>,
    session_id: Option<String>,
    tool_choice: Option<ToolChoice>,
    reasoning: Option<kiana_ai::types::ThinkingLevel>,
    api_key: Option<String>,
    has_refusal_fallbacks: bool,
}

impl From<&SimpleStreamOptions> for RecordedOptions {
    fn from(options: &SimpleStreamOptions) -> Self {
        RecordedOptions {
            cache_retention: options.base.cache_retention,
            session_id: options.base.session_id.clone(),
            tool_choice: options.tool_choice,
            reasoning: options.reasoning,
            api_key: options.base.api_key.clone(),
            has_refusal_fallbacks: options.base.extra.contains_key("refusalFallbacks"),
        }
    }
}

#[derive(Default)]
struct Recorder {
    options: Vec<RecordedOptions>,
    contexts: Vec<TranscriptContext>,
}

type SharedRecorder = Arc<Mutex<Recorder>>;

/// 构造假 `StreamFn`：记录请求选项与上下文，并同步推入一条 `done` 事件。
fn recording_stream_fn(response: AssistantMessage) -> (StreamFn, SharedRecorder) {
    let recorder: SharedRecorder = Arc::new(Mutex::new(Recorder::default()));
    let recorder_for_fn = Arc::clone(&recorder);
    let stream_fn: StreamFn = Arc::new(move |_model, context, options| {
        {
            let mut state = recorder_for_fn.lock().expect("recorder");
            if let Some(options) = options.as_ref() {
                state.options.push(RecordedOptions::from(options));
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

fn recorded(recorder: &SharedRecorder) -> Vec<RecordedOptions> {
    recorder.lock().expect("recorder").options.clone()
}

fn recorded_contexts(recorder: &SharedRecorder) -> Vec<TranscriptContext> {
    recorder.lock().expect("recorder").contexts.clone()
}

fn prompt_json(context: &TranscriptContext) -> String {
    serde_json::to_string(&context.as_context().messages).expect("serialize messages")
}

fn summary_options(
    model: Model,
    api_key: &str,
    thinking: Option<ThinkingLevel>,
) -> GenerateSummaryOptions {
    GenerateSummaryOptions {
        model,
        api_key: Some(api_key.to_string()),
        thinking_level: thinking,
        ..GenerateSummaryOptions::default()
    }
}

fn split_turn_preparation(messages_to_summarize: Vec<AgentMessage>) -> CompactionPreparation {
    CompactionPreparation {
        first_kept_entry_id: "entry-keep".to_string(),
        messages_to_summarize,
        turn_prefix_messages: messages(),
        is_split_turn: true,
        tokens_before: 100,
        previous_summary: Some("previous checkpoint".to_string()),
        file_ops: FileOperations::default(),
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 2000,
            keep_recent_tokens: 20,
        },
    }
}

/// 记录重试回调事件（`scheduled` / `attempt` / `finished:<ok>`）。
fn recording_callbacks(events: Arc<Mutex<Vec<String>>>) -> RetryCallbacks {
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

fn retry_policy() -> RetryPolicy {
    RetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 1,
        max_agent_delay_ms: None,
    }
}

/// Rust-only regression（TS 参考 probe）：TS 的 `produce` 是
/// `await streamFn(...).result()`，没有 catch；抛错原样 reject，`retryAssistantCall` 不重试、
/// 也不发 `onRetryFinished`。错误文本不能被包成 `"Summarization failed: ..."`，即使文本命中
/// 可重试模式（rate limit）也只调用一次。
#[tokio::test]
async fn propagates_hard_stream_errors_without_retry_or_callbacks() {
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

    let error = generate_summary_with_usage(
        &messages(),
        GenerateSummaryOptions {
            stream_fn: Some(stream_fn),
            retry: Some(retry_policy()),
            callbacks: Some(recording_callbacks(Arc::clone(&events))),
            ..summary_options(create_model(false, 8192, None), "test-key", None)
        },
    )
    .await
    .expect_err("stream error must reject");

    assert_eq!(error.to_string(), "rate limit exceeded boom");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(events.lock().expect("events").is_empty());
}

/// Rust-only regression（TS 参考 probe）：第一次返回可重试的 error 消息、第二次 `produce`
/// 抛错时，TS 只发出 `onRetryScheduled` + `onRetryAttemptStart`，随后 promise reject，
/// **不**发 `onRetryFinished`，原始错误文本冒泡。
#[tokio::test]
async fn a_throwing_retry_attempt_is_not_retried_and_skips_retry_finished() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let attempt = calls_for_fn.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            let mut message = mock_summary_response();
            message.stop_reason = StopReason::Error;
            message.error_message = Some("rate limit exceeded".to_string());
            let stream = create_assistant_message_event_stream();
            let _ = stream.push(AssistantMessageEvent::Error {
                reason: ErrorReason::Error,
                error: Box::new(message),
            });
            return Box::pin(async move { Ok(stream) });
        }
        Box::pin(async { Err(kiana_ai::AiError::new(kiana_ai::AiErrorKind::Other, "boom")) })
    });
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let error = generate_summary_with_usage(
        &messages(),
        GenerateSummaryOptions {
            stream_fn: Some(stream_fn),
            retry: Some(retry_policy()),
            callbacks: Some(recording_callbacks(Arc::clone(&events))),
            ..summary_options(create_model(false, 8192, None), "test-key", None)
        },
    )
    .await
    .expect_err("second attempt must reject");

    assert_eq!(error.to_string(), "boom");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        events.lock().expect("events").as_slice(),
        ["scheduled".to_string(), "attempt".to_string()]
    );
}

/// TS: it("uses the provided thinking level for reasoning-capable models")
#[tokio::test]
async fn uses_the_provided_thinking_level_for_reasoning_capable_models() {
    let (stream_fn, recorder) = recording_stream_fn(mock_summary_response());

    let result = generate_summary_with_usage(
        &messages(),
        GenerateSummaryOptions {
            stream_fn: Some(stream_fn),
            ..summary_options(
                create_model(true, 8192, None),
                "test-key",
                Some(ThinkingLevel::Medium),
            )
        },
    )
    .await
    .expect("summary");

    assert_eq!(result.text, "## Goal\nTest summary");
    assert_eq!(result.usage, mock_summary_response().usage);

    let calls = recorded(&recorder);
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].reasoning,
        Some(kiana_ai::types::ThinkingLevel::Medium)
    );
    assert_eq!(calls[0].api_key.as_deref(), Some("test-key"));
}

/// TS: it("preserves the string result from generateSummary")
#[tokio::test]
async fn preserves_the_string_result_from_generate_summary() {
    let (stream_fn, _recorder) = recording_stream_fn(mock_summary_response());
    let result = generate_summary(
        &messages(),
        GenerateSummaryOptions {
            stream_fn: Some(stream_fn),
            ..summary_options(create_model(false, 8192, None), "test-key", None)
        },
    )
    .await
    .expect("summary");
    assert_eq!(result, "## Goal\nTest summary");
}

/// TS: it("uses fresh routing sessions without prompt caching")
#[tokio::test]
async fn uses_fresh_routing_sessions_without_prompt_caching() {
    let (stream_fn, recorder) = recording_stream_fn(mock_summary_response());
    for _ in 0..2 {
        generate_summary(
            &messages(),
            GenerateSummaryOptions {
                stream_fn: Some(stream_fn.clone()),
                ..summary_options(create_model(false, 8192, None), "test-key", None)
            },
        )
        .await
        .expect("summary");
    }

    let request_options = recorded(&recorder);
    assert_eq!(request_options.len(), 2);
    assert!(
        request_options
            .iter()
            .all(|options| options.cache_retention == Some(CacheRetention::None))
    );

    let session_ids: Vec<&str> = request_options
        .iter()
        .map(|options| options.session_id.as_deref().unwrap_or_default())
        .collect();
    assert_ne!(session_ids[0], session_ids[1]);
}

/// TS: it("honors caller-supplied routing session and tool choice without prompt caching")
#[tokio::test]
async fn honors_caller_supplied_routing_session_and_tool_choice_without_prompt_caching() {
    let (stream_fn, recorder) = recording_stream_fn(mock_summary_response());

    let mut options = SimpleStreamOptions::default();
    options.base.session_id = Some("current-routing-session".to_string());
    options.base.cache_retention = Some(CacheRetention::Long);
    options.tool_choice = Some(ToolChoice::Auto);

    complete_summarization(
        &create_model(false, 8192, None),
        normalize_context(&Context {
            system_prompt: Some("Summarize".to_string()),
            messages: Vec::new(),
            tools: None,
        }),
        options,
        Some(stream_fn),
        None,
        None,
    )
    .await
    .expect("summarization");

    let request_options = recorded(&recorder);
    assert_eq!(
        request_options[0].session_id.as_deref(),
        Some("current-routing-session")
    );
    assert_eq!(
        request_options[0].cache_retention,
        Some(CacheRetention::None)
    );
    assert_eq!(request_options[0].tool_choice, Some(ToolChoice::Auto));
}

/// TS: it("preserves the previous summary without an empty history request for a split turn")
#[tokio::test]
async fn preserves_the_previous_summary_without_an_empty_history_request_for_a_split_turn() {
    let (stream_fn, recorder) = recording_stream_fn(mock_summary_response());
    let preparation = split_turn_preparation(Vec::new());

    let result = compact(
        &preparation,
        &create_model(false, 8192, None),
        Some("test-key"),
        None,
        None,
        None,
        None,
        Some(stream_fn),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("compaction");

    assert_eq!(recorded(&recorder).len(), 1);
    assert!(result.summary.contains("previous checkpoint"));
    let request_context = recorded_contexts(&recorder);
    let prompt = prompt_json(&request_context[0]);
    // Regression test for #9652: clear boundaries and continuation wording avoid the
    // reasoning-extraction false positive.
    assert!(prompt.contains("# Conversation\\n[User]: Summarize this."));
    assert!(prompt.contains(
        "# Instructions\\nThe messages above are earlier context from an ongoing conversation."
    ));
}

/// TS: `compact()` 的 split-turn 合并文本在没有 previousSummary 时以 "No prior history." 开头。
#[tokio::test]
async fn split_turn_without_previous_summary_uses_no_prior_history() {
    let (stream_fn, _recorder) = recording_stream_fn(mock_summary_response());
    let preparation = CompactionPreparation {
        first_kept_entry_id: "entry-keep".to_string(),
        messages_to_summarize: Vec::new(),
        turn_prefix_messages: messages(),
        is_split_turn: true,
        tokens_before: 100,
        previous_summary: None,
        file_ops: FileOperations::default(),
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 2000,
            keep_recent_tokens: 20,
        },
    };

    let result = compact(
        &preparation,
        &create_model(false, 8192, None),
        Some("test-key"),
        None,
        None,
        None,
        None,
        Some(stream_fn),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("compaction");

    assert_eq!(
        result.summary,
        "No prior history.\n\n---\n\n**Turn Context (split turn):**\n\n## Goal\nTest summary"
    );
}

/// Rust-only regression：`compact()` 把准备阶段收集的文件条目写进摘要文本与 `details`，
/// 非字符串元素保持原始 JSON 类型（TS 的 `FileOperations` 是运行期未校验的 `Set`）。
///
/// 期望值由 TS 参考生成：read `[null, 123, "123", true]`、edited `[123, "123"]` →
/// `readFiles=[null,true]`、`modifiedFiles=[123,"123"]`，
/// 渲染为 `"...<read-files>\n\ntrue\n</read-files>\n\n<modified-files>\n123\n123\n</modified-files>"`。
#[tokio::test]
async fn compact_writes_file_lists_for_non_string_details() {
    let (stream_fn, _recorder) = recording_stream_fn(mock_summary_response());
    let mut file_ops = FileOperations::default();
    for value in [
        JsonValue::Null,
        serde_json::json!(123),
        serde_json::json!("123"),
        serde_json::json!(true),
    ] {
        file_ops.read.insert(FileValue::from(value));
    }
    file_ops
        .edited
        .insert(FileValue::from(serde_json::json!(123)));
    file_ops
        .edited
        .insert(FileValue::from(serde_json::json!("123")));
    let preparation = CompactionPreparation {
        first_kept_entry_id: "entry-keep".to_string(),
        messages_to_summarize: messages(),
        turn_prefix_messages: Vec::new(),
        is_split_turn: false,
        tokens_before: 10,
        previous_summary: None,
        file_ops,
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 0,
            keep_recent_tokens: 0,
        },
    };

    let result = compact(
        &preparation,
        &create_model(false, 8192, None),
        Some("test-key"),
        None,
        None,
        None,
        None,
        Some(stream_fn),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("compaction");

    assert!(result.summary.ends_with(
        "\n\n<read-files>\n\ntrue\n</read-files>\n\n<modified-files>\n123\n123\n</modified-files>"
    ));
    let details = result.details.expect("details");
    assert_eq!(details["readFiles"], serde_json::json!([null, true]));
    assert_eq!(details["modifiedFiles"], serde_json::json!([123, "123"]));
}

/// TS: it("applies custom instructions to the turn prefix summary of a split turn")
#[tokio::test]
async fn applies_custom_instructions_to_the_turn_prefix_summary_of_a_split_turn() {
    // Regression test for #11058
    let (stream_fn, recorder) = recording_stream_fn(mock_summary_response());
    let preparation = split_turn_preparation(Vec::new());

    compact(
        &preparation,
        &create_model(false, 8192, None),
        Some("test-key"),
        None,
        Some("keep migration decisions"),
        None,
        None,
        Some(stream_fn),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("compaction");

    assert_eq!(recorded(&recorder).len(), 1);
    let request_context = recorded_contexts(&recorder);
    assert!(
        prompt_json(&request_context[0]).contains("Additional focus: keep migration decisions")
    );
}

/// TS: it("rejects tool calls from conversation summaries")
#[tokio::test]
async fn rejects_tool_calls_from_conversation_summaries() {
    let (stream_fn, _recorder) = recording_stream_fn(mock_tool_call_response());

    let error = generate_summary_with_usage(
        &messages(),
        GenerateSummaryOptions {
            stream_fn: Some(stream_fn),
            ..summary_options(create_model(false, 8192, None), "test-key", None)
        },
    )
    .await
    .expect_err("tool call must be rejected");

    assert_eq!(error.to_string(), "Summarization attempted to call a tool");
}

/// TS: it("rejects tool calls from split-turn summaries")
#[tokio::test]
async fn rejects_tool_calls_from_split_turn_summaries() {
    let (stream_fn, _recorder) = recording_stream_fn(mock_tool_call_response());
    let preparation = split_turn_preparation(Vec::new());

    let error = compact(
        &preparation,
        &create_model(false, 8192, None),
        Some("test-key"),
        None,
        None,
        None,
        None,
        Some(stream_fn),
        None,
        None,
        None,
        None,
    )
    .await
    .expect_err("tool call must be rejected");

    assert_eq!(
        error.to_string(),
        "Turn prefix summarization attempted to call a tool"
    );
}

/// TS: it("rejects a length-limited history summary")
#[tokio::test]
async fn rejects_a_length_limited_history_summary() {
    let mut response = mock_summary_response();
    response.stop_reason = StopReason::Length;
    response.content = vec![AssistantContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: "partial".to_string(),
        text_signature: None,
    })];
    let (stream_fn, _recorder) = recording_stream_fn(response);

    let error = generate_summary_with_usage(
        &messages(),
        GenerateSummaryOptions {
            stream_fn: Some(stream_fn),
            ..summary_options(create_model(false, 8192, None), "test-key", None)
        },
    )
    .await
    .expect_err("length stop must be rejected");

    assert!(error.to_string().contains("generation hit the token cap"));
}

/// TS: it("rejects a length-limited split-turn summary")
#[tokio::test]
async fn rejects_a_length_limited_split_turn_summary() {
    let mut response = mock_summary_response();
    response.stop_reason = StopReason::Length;
    response.content = vec![AssistantContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: "partial".to_string(),
        text_signature: None,
    })];
    let (stream_fn, _recorder) = recording_stream_fn(response);
    let preparation = split_turn_preparation(Vec::new());

    let error = compact(
        &preparation,
        &create_model(false, 8192, None),
        Some("test-key"),
        None,
        None,
        None,
        None,
        Some(stream_fn),
        None,
        None,
        None,
        None,
    )
    .await
    .expect_err("length stop must be rejected");

    assert!(error.to_string().contains("generation hit the token cap"));
}

/// TS: it("does not set reasoning when thinking is off")
#[tokio::test]
async fn does_not_set_reasoning_when_thinking_is_off() {
    let (stream_fn, recorder) = recording_stream_fn(mock_summary_response());

    generate_summary(
        &messages(),
        GenerateSummaryOptions {
            stream_fn: Some(stream_fn),
            ..summary_options(
                create_model(true, 8192, None),
                "test-key",
                Some(ThinkingLevel::Off),
            )
        },
    )
    .await
    .expect("summary");

    let calls = recorded(&recorder);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].api_key.as_deref(), Some("test-key"));
    assert_eq!(calls[0].reasoning, None);
}

/// TS: it("does not set reasoning for non-reasoning models")
#[tokio::test]
async fn does_not_set_reasoning_for_non_reasoning_models() {
    let (stream_fn, recorder) = recording_stream_fn(mock_summary_response());

    generate_summary(
        &messages(),
        GenerateSummaryOptions {
            stream_fn: Some(stream_fn),
            ..summary_options(
                create_model(false, 8192, None),
                "test-key",
                Some(ThinkingLevel::Medium),
            )
        },
    )
    .await
    .expect("summary");

    let calls = recorded(&recorder);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].api_key.as_deref(), Some("test-key"));
    assert_eq!(calls[0].reasoning, None);
}

/// TS: it("leaves Anthropic refusal fallback handling to kiana-ai model metadata")
#[tokio::test]
async fn leaves_anthropic_refusal_fallback_handling_to_kiana_ai_model_metadata() {
    let compat = serde_json::json!({
        "allowedFallbackModels": [
            {
                "provider": "anthropic",
                "model": "claude-opus-4-8",
                "cost": { "input": 5, "output": 25, "cacheRead": 0.5, "cacheWrite": 6.25 },
            }
        ]
    });
    let (stream_fn, recorder) = recording_stream_fn(mock_summary_response());

    generate_summary(
        &messages(),
        GenerateSummaryOptions {
            stream_fn: Some(stream_fn),
            ..summary_options(create_model(true, 8192, Some(compat)), "test-key", None)
        },
    )
    .await
    .expect("summary");

    let calls = recorded(&recorder);
    assert_eq!(calls.len(), 1);
    assert!(!calls[0].has_refusal_fallbacks);
}

/// TS: it("does not set Anthropic refusal fallback for models without allowed fallback targets")
#[tokio::test]
async fn does_not_set_anthropic_refusal_fallback_for_models_without_allowed_fallback_targets() {
    let (stream_fn, recorder) = recording_stream_fn(mock_summary_response());

    generate_summary(
        &messages(),
        GenerateSummaryOptions {
            stream_fn: Some(stream_fn),
            ..summary_options(create_model(true, 8192, None), "test-key", None)
        },
    )
    .await
    .expect("summary");

    let calls = recorded(&recorder);
    assert_eq!(calls.len(), 1);
    assert!(!calls[0].has_refusal_fallbacks);
}

/// TS: it("combines usage from split-turn summaries")
#[tokio::test]
async fn combines_usage_from_split_turn_summaries() {
    let (stream_fn, _recorder) = recording_stream_fn(mock_summary_response());
    let preparation = CompactionPreparation {
        first_kept_entry_id: "entry-keep".to_string(),
        messages_to_summarize: messages(),
        turn_prefix_messages: messages(),
        is_split_turn: true,
        tokens_before: 600_000,
        previous_summary: None,
        file_ops: FileOperations::default(),
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 16384,
            keep_recent_tokens: 20000,
        },
    };

    let result = compact(
        &preparation,
        &create_model(false, 8192, None),
        Some("test-key"),
        None,
        None,
        None,
        None,
        Some(stream_fn),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("compaction");

    assert_eq!(
        result.usage,
        Some(Usage {
            input: 20,
            output: 20,
            total_tokens: 40,
            cost: UsageCost {
                total_from_provider: Some(false),
                ..UsageCost::default()
            },
            ..Usage::default()
        })
    );
    // 合并文本逐字使用 TS 的分隔符（两次摘要都以同一 mock 响应生成；文件列表为空）。
    assert_eq!(
        result.summary,
        "## Goal\nTest summary\n\n---\n\n**Turn Context (split turn):**\n\n## Goal\nTest summary"
    );
}
