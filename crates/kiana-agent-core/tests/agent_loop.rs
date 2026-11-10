//! `packages/agent/test/agent-loop.test.ts` 的 Rust 移植（36 个 `it` 声明 / 38 次运行）。
//!
//! 组织与 TS 一一对应：
//! - `default stream function compatibility`（1）
//! - `agentLoop with AgentMessage`（30，其中两个 `it.each` 各展开成两个 Rust 测试）
//! - `agentLoopContinue with AgentMessage`（3）
//! - `runToolCall`（2）
//!
//! 每个测试的文档注释里保留 TS 的 `it` 标题原文（Rust 函数名 snake_case）。
//! 每个测试都用超时等待流结果，避免实现回归时把测试进程挂死。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kiana_agent_core::agent_loop::{
    AgentEventSink, AgentEventStream, RunToolCallOptions, ToolUpdateSink, agent_loop,
    agent_loop_continue, run_agent_loop, run_tool_call,
};
use kiana_agent_core::stream_fn::{clear_default_stream_fn, set_default_stream_fn};
use kiana_agent_core::types::{
    AfterToolCallResult, AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentTool,
    AgentToolCall, AgentToolError, AgentToolExecute, AgentToolResult, AgentToolUpdateCallback,
    BeforeToolCallResult, BoxFuture, JsonObject, JsonValue, StreamFn, ToolExecutionMode,
};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, DoneReason,
    ErrorReason, Message, Model, ModelInputModality, SimpleStreamOptions, StopReason, TextContent,
    TextKind, ToolCall, ToolCallKind, ToolResultMessage, TranscriptContext, Usage, UsageCost,
    UserContent, UserContentBlock, UserMessage, UserRole,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// 辅助（对应 TS 测试文件顶部的 createUsage/createModel/createAssistantMessage/...）
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn create_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
            total_from_provider: None,
        },
    }
}

fn create_model() -> Model {
    Model {
        id: "mock".to_string(),
        name: "mock".to_string(),
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        base_url: "https://example.invalid".to_string(),
        input: vec![ModelInputModality::Text],
        cost: Default::default(),
        ..Default::default()
    }
}

fn text(text: impl Into<String>) -> TextContent {
    TextContent {
        kind_text: TextKind::Text,
        text: text.into(),
        text_signature: None,
    }
}

fn text_block(text: impl Into<String>) -> AssistantContentBlock {
    AssistantContentBlock::Text(self::text(text))
}

fn create_assistant_message(
    content: Vec<AssistantContentBlock>,
    stop_reason: StopReason,
) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        model: "mock".to_string(),
        usage: create_usage(),
        stop_reason,
        timestamp: now_ms(),
        ..Default::default()
    }
}

fn create_user_message(content: impl Into<String>) -> AgentMessage {
    AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(content.into()),
        timestamp: now_ms(),
    }))
}

fn create_system_message(
    content: impl Into<String>,
    tools_added: Option<Vec<kiana_ai::types::Tool>>,
) -> AgentMessage {
    AgentMessage::Standard(Message::System(kiana_ai::types::SystemMessage {
        role: Default::default(),
        content: kiana_ai::types::SystemContent::Text(content.into()),
        sections: None,
        tools_added,
        tools_removed: None,
        timestamp: 1,
    }))
}

fn tool_call(id: &str, name: &str, arguments: JsonValue) -> AgentToolCall {
    ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: id.to_string(),
        name: name.to_string(),
        arguments: object_arguments(arguments),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    }
}

fn object_arguments(value: JsonValue) -> JsonObject {
    match value {
        JsonValue::Object(map) => map.into_iter().collect(),
        _ => JsonObject::new(),
    }
}

/// `identityConverter`：只保留四种标准消息。
fn identity_converter() -> Arc<dyn Fn(Vec<AgentMessage>) -> Vec<Message> + Send + Sync> {
    Arc::new(|messages| {
        messages
            .into_iter()
            .filter_map(|message| match message {
                AgentMessage::Standard(message) => Some(message),
                AgentMessage::Custom(_) => None,
            })
            .collect()
    })
}

fn base_config() -> AgentLoopConfig {
    AgentLoopConfig {
        model: create_model(),
        convert_to_llm: identity_converter(),
        ..Default::default()
    }
}

fn echo_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "properties": { "value": { "type": "string" } },
        "required": ["value"]
    })
}

fn empty_schema() -> JsonValue {
    serde_json::json!({ "type": "object", "properties": {} })
}

type ExecuteFn = Arc<
    dyn Fn(
            String,
            JsonValue,
            Option<CancellationToken>,
            Option<AgentToolUpdateCallback>,
        ) -> BoxFuture<'static, Result<AgentToolResult<JsonValue>, AgentToolError>>
        + Send
        + Sync,
>;

struct TestToolExecute {
    execute: ExecuteFn,
}

impl AgentToolExecute for TestToolExecute {
    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        on_update: Option<AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        let execute = self.execute.clone();
        let tool_call_id = tool_call_id.to_string();
        let signal = signal.cloned();
        Box::pin(async move { execute(tool_call_id, params, signal, on_update).await })
    }
}

fn agent_tool(name: &str, parameters: JsonValue, execute: ExecuteFn) -> AgentTool {
    AgentTool {
        name: name.to_string(),
        label: name.to_string(),
        description: format!("{name} tool"),
        parameters,
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(TestToolExecute { execute }),
    }
}

fn result_with_text(text_value: impl Into<String>) -> AgentToolResult<JsonValue> {
    AgentToolResult {
        content: vec![UserContentBlock::Text(text(text_value))],
        details: JsonValue::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

fn done_stream_with_reason(
    message: AssistantMessage,
    reason: DoneReason,
) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let _ = stream.push(AssistantMessageEvent::Done {
        reason,
        message: Box::new(message),
    });
    stream
}

fn done_stream(message: AssistantMessage) -> AssistantMessageEventStream {
    let reason = match message.stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        _ => DoneReason::Stop,
    };
    done_stream_with_reason(message, reason)
}

fn error_stream(message: AssistantMessage, reason: ErrorReason) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let _ = stream.push(AssistantMessageEvent::Error {
        reason,
        error: Box::new(message),
    });
    stream
}

fn stream_fn_with<F>(f: F) -> StreamFn
where
    F: Fn(Model, TranscriptContext, Option<SimpleStreamOptions>) -> AssistantMessageEventStream
        + Send
        + Sync
        + 'static,
{
    Arc::new(move |model, context, options| {
        let stream = f(model, context, options);
        Box::pin(async move { Ok(stream) })
    })
}

fn done_stream_fn(message: AssistantMessage) -> StreamFn {
    stream_fn_with(move |_model, _context, _options| done_stream(message.clone()))
}

async fn collect_events(stream: &AgentEventStream) -> Vec<AgentEvent> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut events = Vec::new();
        while let Some(event) = stream.recv().await {
            events.push(event);
        }
        events
    })
    .await
    .expect("event stream did not end")
}

async fn await_result(stream: &AgentEventStream) -> Vec<AgentMessage> {
    tokio::time::timeout(Duration::from_secs(10), stream.result())
        .await
        .expect("stream result timed out")
}

fn roles(messages: &[AgentMessage]) -> Vec<&str> {
    messages.iter().map(AgentMessage::role).collect()
}

fn message_end_tool_result_ids(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageEnd {
                message: AgentMessage::Standard(Message::ToolResult(result)),
            } => Some(result.tool_call_id.clone()),
            _ => None,
        })
        .collect()
}

fn tool_execution_end_ids(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect()
}

fn contains_user_text(context: &kiana_ai::types::Context, expected: &str) -> bool {
    context.messages.iter().any(|message| {
        matches!(
            message,
            Message::User(UserMessage {
                content: UserContent::Text(content),
                ..
            }) if content == expected
        )
    })
}

fn message_start_sequence(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageStart { message } => match message {
                AgentMessage::Standard(Message::ToolResult(result)) => {
                    Some(format!("tool:{}", result.tool_call_id))
                }
                AgentMessage::Standard(Message::User(UserMessage {
                    content: UserContent::Text(content),
                    ..
                })) => Some(content.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// default stream function compatibility
// ---------------------------------------------------------------------------

/// TS: "uses the configured default when a legacy caller omits streamFn"
#[tokio::test]
async fn uses_the_configured_default_when_a_legacy_caller_omits_stream_fn() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    set_default_stream_fn(stream_fn_with(move |_model, _context, _options| {
        calls_for_fn.fetch_add(1, Ordering::SeqCst);
        done_stream(create_assistant_message(
            vec![text_block("fallback")],
            StopReason::Stop,
        ))
    }));

    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(Vec::new()),
    };
    let config = base_config();
    let stream = agent_loop(
        vec![create_user_message("Hello")],
        context,
        config,
        None,
        None,
    );
    let messages = await_result(&stream).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(roles(&messages), vec!["user", "assistant"]);
    clear_default_stream_fn();
}

// ---------------------------------------------------------------------------
// agentLoop with AgentMessage
// ---------------------------------------------------------------------------

/// TS: "should emit events with AgentMessage types"
#[tokio::test]
async fn should_emit_events_with_agent_message_types() {
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(Vec::new()),
    };
    let stream = agent_loop(
        vec![create_user_message("Hello")],
        context,
        base_config(),
        None,
        Some(done_stream_fn(create_assistant_message(
            vec![text_block("Hi there!")],
            StopReason::Stop,
        ))),
    );

    let events = collect_events(&stream).await;
    let messages = await_result(&stream).await;

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role(), "user");
    assert_eq!(messages[1].role(), "assistant");

    let event_types: Vec<&str> = events.iter().map(AgentEvent::kind).collect();
    for expected in [
        "agent_start",
        "turn_start",
        "message_start",
        "message_end",
        "turn_end",
        "agent_end",
    ] {
        assert!(
            event_types.contains(&expected),
            "expected event type {expected} in {event_types:?}"
        );
    }
}

/// TS: "should build provider context exclusively from transcript messages"
#[tokio::test]
async fn should_build_provider_context_exclusively_from_transcript_messages() {
    let initial_system = create_system_message("Transcript prompt", Some(Vec::new()));
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(Vec::new()),
    };
    let seen: Arc<Mutex<Option<TranscriptContext>>> = Arc::new(Mutex::new(None));
    let seen_for_fn = seen.clone();
    let stream_fn = stream_fn_with(move |_model, provider_context, _options| {
        *seen_for_fn.lock().unwrap() = Some(provider_context);
        done_stream(create_assistant_message(
            vec![text_block("done")],
            StopReason::Stop,
        ))
    });

    let stream = agent_loop(
        vec![initial_system.clone(), create_user_message("Hello")],
        context,
        base_config(),
        None,
        Some(stream_fn),
    );
    let _ = await_result(&stream).await;

    let observed = seen.lock().unwrap().clone().expect("provider context");
    let observed = observed.as_context();
    // provider 只收到 transcript：没有顶层 prompt / tool 字段。
    assert!(observed.system_prompt.is_none());
    assert!(observed.tools.is_none());
    assert_eq!(observed.messages.len(), 2);
    let expected_first = match &initial_system {
        AgentMessage::Standard(message) => message.clone(),
        AgentMessage::Custom(_) => unreachable!(),
    };
    assert_eq!(observed.messages[0], expected_first);
}

/// TS: "should handle custom message types via convertToLlm"
#[tokio::test]
async fn should_handle_custom_message_types_via_convert_to_llm() {
    let mut notification = JsonObject::new();
    notification.insert("role".to_string(), serde_json::json!("notification"));
    notification.insert(
        "text".to_string(),
        serde_json::json!("This is a notification"),
    );
    notification.insert("timestamp".to_string(), serde_json::json!(now_ms()));

    let context = AgentContext {
        messages: vec![AgentMessage::Custom(Box::new(notification))],
        tools: Some(Vec::new()),
    };
    let converted: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(Vec::new()));
    let converted_for_convert = converted.clone();
    let mut config = base_config();
    config.convert_to_llm = Arc::new(move |messages| {
        let filtered: Vec<Message> = messages
            .into_iter()
            .filter(|message| message.role() != "notification")
            .filter_map(|message| match message {
                AgentMessage::Standard(message) => Some(message),
                AgentMessage::Custom(_) => None,
            })
            .collect();
        *converted_for_convert.lock().unwrap() = filtered.clone();
        filtered
    });

    let stream = agent_loop(
        vec![create_user_message("Hello")],
        context,
        config,
        None,
        Some(done_stream_fn(create_assistant_message(
            vec![text_block("Response")],
            StopReason::Stop,
        ))),
    );
    let _ = collect_events(&stream).await;

    let converted = converted.lock().unwrap().clone();
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0].role(), "user");
}

/// TS: "should apply transformContext before convertToLlm"
#[tokio::test]
async fn should_apply_transform_context_before_convert_to_llm() {
    let context = AgentContext {
        messages: vec![
            create_user_message("old message 1"),
            AgentMessage::Standard(Message::Assistant(create_assistant_message(
                vec![text_block("old response 1")],
                StopReason::Stop,
            ))),
            create_user_message("old message 2"),
            AgentMessage::Standard(Message::Assistant(create_assistant_message(
                vec![text_block("old response 2")],
                StopReason::Stop,
            ))),
        ],
        tools: Some(Vec::new()),
    };
    let transformed: Arc<Mutex<Vec<AgentMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let converted: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(Vec::new()));
    let transformed_for_hook = transformed.clone();
    let converted_for_convert = converted.clone();
    let mut config = base_config();
    config.transform_context = Some(Arc::new(move |messages, _signal| {
        let kept = messages[messages.len().saturating_sub(2)..].to_vec();
        *transformed_for_hook.lock().unwrap() = kept.clone();
        Box::pin(async move { kept })
    }));
    config.convert_to_llm = Arc::new(move |messages| {
        let converted_messages: Vec<Message> = messages
            .into_iter()
            .filter_map(|message| match message {
                AgentMessage::Standard(message) => Some(message),
                AgentMessage::Custom(_) => None,
            })
            .collect();
        *converted_for_convert.lock().unwrap() = converted_messages.clone();
        converted_messages
    });

    let stream = agent_loop(
        vec![create_user_message("new message")],
        context,
        config,
        None,
        Some(done_stream_fn(create_assistant_message(
            vec![text_block("Response")],
            StopReason::Stop,
        ))),
    );
    let _ = collect_events(&stream).await;

    assert_eq!(transformed.lock().unwrap().len(), 2);
    assert_eq!(converted.lock().unwrap().len(), 2);
}

/// TS: "should handle tool calls and results"
#[tokio::test]
async fn should_handle_tool_calls_and_results() {
    let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let tool_usage = Usage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        cache_write1h: None,
        reasoning: None,
        total_tokens: 10,
        cost: UsageCost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.3,
            cache_write: 0.4,
            total: 1.0,
            total_from_provider: None,
        },
    };
    let patched_tool_usage = Usage {
        input: 5,
        output: 6,
        cache_read: 7,
        cache_write: 8,
        cache_write1h: None,
        reasoning: None,
        total_tokens: 26,
        cost: UsageCost {
            input: 0.5,
            output: 0.6,
            cache_read: 0.7,
            cache_write: 0.8,
            total: 2.6,
            total_from_provider: None,
        },
    };
    let observed_usage: Arc<Mutex<Option<Usage>>> = Arc::new(Mutex::new(None));

    let executed_for_tool = executed.clone();
    let tool_usage_for_tool = tool_usage.clone();
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            let executed = executed_for_tool.clone();
            let usage = tool_usage_for_tool.clone();
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                executed.lock().unwrap().push(value.clone());
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(format!("echoed: {value}")))],
                    details: serde_json::json!({ "value": value }),
                    structured_content: None,
                    usage: Some(usage),
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );

    let observed_for_hook = observed_usage.clone();
    let patched_for_hook = patched_tool_usage.clone();
    let mut config = base_config();
    config.after_tool_call = Some(Arc::new(move |context, _signal| {
        let observed = observed_for_hook.clone();
        let patched = patched_for_hook.clone();
        Box::pin(async move {
            *observed.lock().unwrap() = context.result.usage.clone();
            Some(AfterToolCallResult {
                usage: Some(patched),
                ..Default::default()
            })
        })
    }));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let index = calls_for_fn.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "echo",
                    serde_json::json!({ "value": "hello" }),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;

    assert_eq!(*executed.lock().unwrap(), vec!["hello".to_string()]);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolExecutionStart { .. }))
    );
    let tool_end = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolExecutionEnd { is_error, .. } => Some(*is_error),
            _ => None,
        })
        .expect("tool_execution_end");
    assert!(!tool_end);
    assert_eq!(observed_usage.lock().unwrap().clone(), Some(tool_usage));

    let messages = await_result(&stream).await;
    let tool_result = messages
        .iter()
        .find_map(|message| match message {
            AgentMessage::Standard(Message::ToolResult(result)) => Some(result.clone()),
            _ => None,
        })
        .expect("tool result message");
    assert_eq!(tool_result.usage, Some(patched_tool_usage));
}

/// TS: "records how long execute() took on the tool result, excluding hooks"
#[tokio::test]
async fn records_how_long_execute_took_on_the_tool_result_excluding_hooks() {
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                let value = params["value"].as_str().unwrap_or_default().to_string();
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(value.clone()))],
                    details: serde_json::json!({ "value": value }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );
    let mut config = base_config();
    config.before_tool_call = Some(Arc::new(|context, _signal| {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if context.tool_call.id == "blocked" {
                Some(BeforeToolCallResult {
                    block: Some(true),
                    reason: Some("no".to_string()),
                    terminate: None,
                })
            } else {
                None
            }
        })
    }));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let index = calls_for_fn.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            done_stream(create_assistant_message(
                vec![
                    AssistantContentBlock::ToolCall(tool_call(
                        "ran",
                        "echo",
                        serde_json::json!({ "value": "a" }),
                    )),
                    AssistantContentBlock::ToolCall(tool_call(
                        "blocked",
                        "echo",
                        serde_json::json!({ "value": "b" }),
                    )),
                ],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("go")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect_events(&stream).await;
    let messages = await_result(&stream).await;
    let results: Vec<ToolResultMessage> = messages
        .into_iter()
        .filter_map(|message| match message {
            AgentMessage::Standard(Message::ToolResult(result)) => Some(result),
            _ => None,
        })
        .collect();
    let ran = results
        .iter()
        .find(|result| result.tool_call_id == "ran")
        .expect("ran");
    let blocked = results
        .iter()
        .find(|result| result.tool_call_id == "blocked")
        .expect("blocked");
    let duration = ran.duration_ms.expect("durationMs");
    assert!(duration >= 25.0, "durationMs {duration} should be >= 25");
    assert!(duration < 100.0, "durationMs {duration} should be < 100");
    assert!(blocked.is_error);
    assert_eq!(blocked.duration_ms, None);
}

/// TS: "should not execute tool calls from a length-truncated assistant message"
#[tokio::test]
async fn should_not_execute_tool_calls_from_a_length_truncated_assistant_message() {
    let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_for_tool = executed.clone();
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            let executed = executed_for_tool.clone();
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                executed.lock().unwrap().push(value);
                Ok(result_with_text("should not run"))
            })
        }),
    );

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let index = calls_for_fn.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            done_stream_with_reason(
                create_assistant_message(
                    vec![AssistantContentBlock::ToolCall(tool_call(
                        "tool-1",
                        "echo",
                        serde_json::json!({ "value": "hel" }),
                    ))],
                    StopReason::Length,
                ),
                DoneReason::Length,
            )
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;

    // 工具绝不能带着可能被截断的参数执行。
    assert!(executed.lock().unwrap().is_empty());

    let tool_end = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolExecutionEnd {
                is_error, result, ..
            } => Some((*is_error, result.clone())),
            _ => None,
        })
        .expect("tool_execution_end");
    assert!(tool_end.0);
    let text = tool_end.1["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        text,
        "Tool call \"echo\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments."
    );

    // 循环继续，让模型可以重发这个工具调用。
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let messages = await_result(&stream).await;
    assert_eq!(messages.last().map(AgentMessage::role), Some("assistant"));
}

/// TS: "should execute mutated beforeToolCall args without revalidation"
///
/// TS 的 `beforeToolCall` 收到的是校验后参数的**同一对象引用**：钩子就地修改
/// (`mutableArgs.value = 123`) 后，`execute()` 直接收到修改后的值，不重新校验。
/// Rust 侧用 [`kiana_agent_core::types::ToolArguments`] 句柄承载同一份参数。
#[tokio::test]
async fn should_execute_mutated_before_tool_call_args_without_revalidation() {
    let executed: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_for_tool = executed.clone();
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            let executed = executed_for_tool.clone();
            Box::pin(async move {
                executed.lock().unwrap().push(params["value"].clone());
                Ok(result_with_text("ok"))
            })
        }),
    );

    let mut config = base_config();
    config.before_tool_call = Some(Arc::new(|context, _signal| {
        Box::pin(async move {
            // TS: `mutableArgs.value = 123; return undefined;`
            context
                .args
                .update(|args| args["value"] = serde_json::json!(123));
            None
        })
    }));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let index = calls_for_fn.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "echo",
                    serde_json::json!({ "value": "hello" }),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect_events(&stream).await;

    // 修改后的 123 不满足 schema（value: string），execute 仍收到它 → 没有重新校验。
    assert_eq!(*executed.lock().unwrap(), vec![serde_json::json!(123)]);
}

/// TS: "should prepare tool arguments for validation"
#[tokio::test]
async fn should_prepare_tool_arguments_for_validation() {
    let executed: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_for_tool = executed.clone();
    let mut tool = agent_tool(
        "edit",
        serde_json::json!({
            "type": "object",
            "properties": {
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string" },
                            "newText": { "type": "string" }
                        },
                        "required": ["oldText", "newText"]
                    }
                }
            },
            "required": ["edits"]
        }),
        Arc::new(move |_id, params, _signal, _on_update| {
            let executed = executed_for_tool.clone();
            Box::pin(async move {
                executed.lock().unwrap().push(params["edits"].clone());
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(format!(
                        "edited {}",
                        params["edits"].as_array().map(Vec::len).unwrap_or(0)
                    )))],
                    details: serde_json::json!({ "count": params["edits"].as_array().map(Vec::len).unwrap_or(0) }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );
    tool.prepare_arguments = Some(Arc::new(|args: &mut JsonValue| {
        let Some(object) = args.as_object() else {
            // TS: `return args as ...`（返回同一对象）。
            return None;
        };
        let (Some(old_text), Some(new_text)) = (
            object.get("oldText").and_then(JsonValue::as_str),
            object.get("newText").and_then(JsonValue::as_str),
        ) else {
            return None;
        };
        let mut edits = object
            .get("edits")
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_default();
        edits.push(serde_json::json!({ "oldText": old_text, "newText": new_text }));
        Some(serde_json::json!({ "edits": edits }))
    }));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let index = calls_for_fn.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "edit",
                    serde_json::json!({ "oldText": "before", "newText": "after" }),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("edit something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let _ = collect_events(&stream).await;

    assert_eq!(
        *executed.lock().unwrap(),
        vec![serde_json::json!([{ "oldText": "before", "newText": "after" }])]
    );
}

/// TS: "should emit tool_execution_end in completion order but persist tool results in source order"
#[tokio::test]
async fn should_emit_tool_execution_end_in_completion_order_but_persist_tool_results_in_source_order()
 {
    let first_resolved = Arc::new(AtomicBool::new(false));
    let parallel_observed = Arc::new(AtomicBool::new(false));
    let release_first = Arc::new(Notify::new());

    let first_resolved_for_tool = first_resolved.clone();
    let parallel_for_tool = parallel_observed.clone();
    let release_for_tool = release_first.clone();
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            let first_resolved = first_resolved_for_tool.clone();
            let parallel_observed = parallel_for_tool.clone();
            let release_first = release_for_tool.clone();
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                if value == "first" {
                    release_first.notified().await;
                    first_resolved.store(true, Ordering::SeqCst);
                }
                if value == "second" && !first_resolved.load(Ordering::SeqCst) {
                    parallel_observed.store(true, Ordering::SeqCst);
                }
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(format!("echoed: {value}")))],
                    details: serde_json::json!({ "value": value }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );

    let mut config = base_config();
    config.tool_execution = Some(ToolExecutionMode::Parallel);

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let release_for_fn = release_first.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let index = calls_for_fn.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            let release = release_for_fn.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                release.notify_one();
            });
            done_stream(create_assistant_message(
                vec![
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-1",
                        "echo",
                        serde_json::json!({ "value": "first" }),
                    )),
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-2",
                        "echo",
                        serde_json::json!({ "value": "second" }),
                    )),
                ],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("echo both")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;

    let tool_execution_end_ids = tool_execution_end_ids(&events);
    let tool_result_ids = message_end_tool_result_ids(&events);
    let turn_tool_result_ids: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TurnEnd { tool_results, .. } => Some(
                tool_results
                    .iter()
                    .map(|result| result.tool_call_id.clone())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect();

    assert!(parallel_observed.load(Ordering::SeqCst));
    assert_eq!(tool_execution_end_ids, vec!["tool-2", "tool-1"]);
    assert_eq!(tool_result_ids, vec!["tool-1", "tool-2"]);
    assert_eq!(turn_tool_result_ids, vec!["tool-1", "tool-2"]);
}

/// TS: "should inject queued messages after all tool calls complete"
#[tokio::test]
async fn should_inject_queued_messages_after_all_tool_calls_complete() {
    let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_for_tool = executed.clone();
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            let executed = executed_for_tool.clone();
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                executed.lock().unwrap().push(value.clone());
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(format!("ok:{value}")))],
                    details: serde_json::json!({ "value": value }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );

    let queued_delivered = Arc::new(AtomicBool::new(false));
    let queued_for_hook = queued_delivered.clone();
    let executed_for_hook = executed.clone();
    let mut config = base_config();
    config.tool_execution = Some(ToolExecutionMode::Sequential);
    config.get_steering_messages = Some(Arc::new(move || {
        let executed = executed_for_hook.clone();
        let queued_delivered = queued_for_hook.clone();
        Box::pin(async move {
            if !executed.lock().unwrap().is_empty()
                && !queued_delivered.swap(true, Ordering::SeqCst)
            {
                vec![create_user_message("interrupt")]
            } else {
                Vec::new()
            }
        })
    }));

    let saw_interrupt_in_context = Arc::new(AtomicBool::new(false));
    let saw_for_fn = saw_interrupt_in_context.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, context, _options| {
        let index = calls_for_fn.fetch_add(1, Ordering::SeqCst);
        if index == 1 {
            let has_interrupt = context.as_context().messages.iter().any(|message| {
                matches!(
                    message,
                    Message::User(UserMessage {
                        content: UserContent::Text(content),
                        ..
                    }) if content == "interrupt"
                )
            });
            saw_for_fn.store(has_interrupt, Ordering::SeqCst);
        }
        if index == 0 {
            done_stream(create_assistant_message(
                vec![
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-1",
                        "echo",
                        serde_json::json!({ "value": "first" }),
                    )),
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-2",
                        "echo",
                        serde_json::json!({ "value": "second" }),
                    )),
                ],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("start")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;

    // 两条工具都执行完才注入 steering。
    assert_eq!(
        *executed.lock().unwrap(),
        vec!["first".to_string(), "second".to_string()]
    );
    let tool_ends: Vec<bool> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolExecutionEnd { is_error, .. } => Some(*is_error),
            _ => None,
        })
        .collect();
    assert_eq!(tool_ends.len(), 2);
    assert!(!tool_ends[0]);
    assert!(!tool_ends[1]);

    let sequence = message_start_sequence(&events);
    let interrupt_index = sequence
        .iter()
        .position(|item| item == "interrupt")
        .expect("interrupt message_start");
    let first_tool_index = sequence
        .iter()
        .position(|item| item == "tool:tool-1")
        .expect("tool-1 message_start");
    let second_tool_index = sequence
        .iter()
        .position(|item| item == "tool:tool-2")
        .expect("tool-2 message_start");
    assert!(first_tool_index < interrupt_index);
    assert!(second_tool_index < interrupt_index);
    assert!(saw_interrupt_in_context.load(Ordering::SeqCst));
}

/// TS: "should force sequential execution when a tool has executionMode=sequential even with default parallel config"
#[tokio::test]
async fn should_force_sequential_execution_when_a_tool_has_execution_mode_sequential_even_with_default_parallel_config()
 {
    let first_resolved = Arc::new(AtomicBool::new(false));
    let parallel_observed = Arc::new(AtomicBool::new(false));
    let release_first = Arc::new(Notify::new());

    let first_resolved_for_tool = first_resolved.clone();
    let parallel_for_tool = parallel_observed.clone();
    let release_for_tool = release_first.clone();
    let mut tool = agent_tool(
        "slow",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            let first_resolved = first_resolved_for_tool.clone();
            let parallel_observed = parallel_for_tool.clone();
            let release_first = release_for_tool.clone();
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                if value == "first" {
                    release_first.notified().await;
                    first_resolved.store(true, Ordering::SeqCst);
                }
                if value == "second" && !first_resolved.load(Ordering::SeqCst) {
                    parallel_observed.store(true, Ordering::SeqCst);
                }
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(format!("slow: {value}")))],
                    details: serde_json::json!({ "value": value }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );
    tool.execution_mode = Some(ToolExecutionMode::Sequential);

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let release_for_fn = release_first.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let index = calls_for_fn.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            let release = release_for_fn.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                release.notify_one();
            });
            done_stream(create_assistant_message(
                vec![
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-1",
                        "slow",
                        serde_json::json!({ "value": "first" }),
                    )),
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-2",
                        "slow",
                        serde_json::json!({ "value": "second" }),
                    )),
                ],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("run both")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;

    // 顺序执行时第二个工具不会在第一个完成前启动。
    assert!(!parallel_observed.load(Ordering::SeqCst));
    assert_eq!(
        message_end_tool_result_ids(&events),
        vec!["tool-1", "tool-2"]
    );
}

/// TS: "should force sequential execution when one of multiple tools has executionMode=sequential"
#[tokio::test]
async fn should_force_sequential_execution_when_one_of_multiple_tools_has_execution_mode_sequential()
 {
    let execution_order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let release_slow = Arc::new(Notify::new());

    let order_for_slow = execution_order.clone();
    let release_for_slow = release_slow.clone();
    let mut slow_tool = agent_tool(
        "slow",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            let execution_order = order_for_slow.clone();
            let release_slow = release_for_slow.clone();
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                execution_order
                    .lock()
                    .unwrap()
                    .push(format!("slow:{value}"));
                if value == "a" {
                    release_slow.notified().await;
                }
                Ok(result_with_text(format!("slow: {value}")))
            })
        }),
    );
    slow_tool.execution_mode = Some(ToolExecutionMode::Sequential);

    let order_for_fast = execution_order.clone();
    let fast_tool = agent_tool(
        "fast",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            let execution_order = order_for_fast.clone();
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                execution_order
                    .lock()
                    .unwrap()
                    .push(format!("fast:{value}"));
                Ok(result_with_text(format!("fast: {value}")))
            })
        }),
    );

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let release_for_fn = release_slow.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let index = calls_for_fn.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            let release = release_for_fn.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                release.notify_one();
            });
            done_stream(create_assistant_message(
                vec![
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-1",
                        "slow",
                        serde_json::json!({ "value": "a" }),
                    )),
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-2",
                        "fast",
                        serde_json::json!({ "value": "b" }),
                    )),
                ],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("run both")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![slow_tool, fast_tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let _ = collect_events(&stream).await;

    let order = execution_order.lock().unwrap().clone();
    assert_eq!(order.first().map(String::as_str), Some("slow:a"));
    assert!(order.contains(&"fast:b".to_string()));
}

/// TS: "should allow parallel execution when all tools have executionMode=parallel"
#[tokio::test]
async fn should_allow_parallel_execution_when_all_tools_have_execution_mode_parallel() {
    let first_resolved = Arc::new(AtomicBool::new(false));
    let parallel_observed = Arc::new(AtomicBool::new(false));
    let release_first = Arc::new(Notify::new());

    let first_resolved_for_tool = first_resolved.clone();
    let parallel_for_tool = parallel_observed.clone();
    let release_for_tool = release_first.clone();
    let mut tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            let first_resolved = first_resolved_for_tool.clone();
            let parallel_observed = parallel_for_tool.clone();
            let release_first = release_for_tool.clone();
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                if value == "first" {
                    release_first.notified().await;
                    first_resolved.store(true, Ordering::SeqCst);
                }
                if value == "second" && !first_resolved.load(Ordering::SeqCst) {
                    parallel_observed.store(true, Ordering::SeqCst);
                }
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(format!("echoed: {value}")))],
                    details: serde_json::json!({ "value": value }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );
    tool.execution_mode = Some(ToolExecutionMode::Parallel);

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let release_for_fn = release_first.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let index = calls_for_fn.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            let release = release_for_fn.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                release.notify_one();
            });
            done_stream(create_assistant_message(
                vec![
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-1",
                        "echo",
                        serde_json::json!({ "value": "first" }),
                    )),
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-2",
                        "echo",
                        serde_json::json!({ "value": "second" }),
                    )),
                ],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("echo both")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let _ = collect_events(&stream).await;

    assert!(parallel_observed.load(Ordering::SeqCst));
}

/// TS: "runs finishTurn after tool-result messages and before turn_end"
#[tokio::test]
async fn runs_finish_turn_after_tool_result_messages_and_before_turn_end() {
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(value.clone()))],
                    details: serde_json::json!({ "value": value }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(true),
                })
            })
        }),
    );

    let ordering: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let ordering_for_hook = ordering.clone();
    let mut config = base_config();
    config.finish_turn = Some(Arc::new(move |turn, _signal| {
        let ordering = ordering_for_hook.clone();
        Box::pin(async move {
            ordering.lock().unwrap().push("finishTurn".to_string());
            assert_eq!(turn.tool_results.len(), 1);
            assert_eq!(
                turn.context.messages.last().map(AgentMessage::role),
                Some("toolResult")
            );
            None
        })
    }));

    let ordering_for_sink = ordering.clone();
    let emit: AgentEventSink = Arc::new(move |event| {
        let ordering = ordering_for_sink.clone();
        Box::pin(async move {
            match event {
                AgentEvent::MessageEnd { message } => {
                    ordering
                        .lock()
                        .unwrap()
                        .push(format!("message_end:{}", message.role()));
                }
                AgentEvent::TurnEnd { .. } => ordering.lock().unwrap().push("turn_end".to_string()),
                _ => {}
            }
        })
    });

    run_agent_loop(
        vec![create_user_message("echo")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        emit,
        None,
        Some(done_stream_fn(create_assistant_message(
            vec![AssistantContentBlock::ToolCall(tool_call(
                "tool-1",
                "echo",
                serde_json::json!({ "value": "hello" }),
            ))],
            StopReason::ToolUse,
        ))),
    )
    .await
    .expect("runAgentLoop");

    let ordering = ordering.lock().unwrap().clone();
    let tail = &ordering[ordering.len() - 3..];
    assert_eq!(tail, ["message_end:toolResult", "finishTurn", "turn_end"]);
}

/// `it.each(["error", "aborted"])` 的公共实现。
async fn hard_exit_case(reason: StopReason) {
    let ordering: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let steering_polls = Arc::new(AtomicUsize::new(0));
    let follow_up_polls = Arc::new(AtomicUsize::new(0));

    let ordering_for_hook = ordering.clone();
    let steering_for_hook = steering_polls.clone();
    let follow_up_for_hook = follow_up_polls.clone();
    let mut config = base_config();
    config.finish_turn = Some(Arc::new(move |turn, _signal| {
        let ordering = ordering_for_hook.clone();
        Box::pin(async move {
            assert_eq!(turn.message.stop_reason, reason);
            ordering.lock().unwrap().push("finishTurn".to_string());
            Some(kiana_agent_core::types::AgentTurnDecision::Continue)
        })
    }));
    config.get_steering_messages = Some(Arc::new(move || {
        let steering_polls = steering_for_hook.clone();
        Box::pin(async move {
            steering_polls.fetch_add(1, Ordering::SeqCst);
            Vec::new()
        })
    }));
    config.get_follow_up_messages = Some(Arc::new(move || {
        let follow_up_polls = follow_up_for_hook.clone();
        Box::pin(async move {
            follow_up_polls.fetch_add(1, Ordering::SeqCst);
            vec![create_user_message("queued")]
        })
    }));

    let ordering_for_sink = ordering.clone();
    let emit: AgentEventSink = Arc::new(move |event| {
        let ordering = ordering_for_sink.clone();
        Box::pin(async move {
            if matches!(event, AgentEvent::TurnEnd { .. }) {
                ordering.lock().unwrap().push("turn_end".to_string());
            }
        })
    });

    let provider_calls_for_fn = provider_calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        provider_calls_for_fn.fetch_add(1, Ordering::SeqCst);
        let mut message = create_assistant_message(Vec::new(), reason);
        message.error_message = Some(match reason {
            StopReason::Aborted => "aborted".to_string(),
            _ => "error".to_string(),
        });
        let error_reason = if reason == StopReason::Aborted {
            ErrorReason::Aborted
        } else {
            ErrorReason::Error
        };
        error_stream(message, error_reason)
    });

    run_agent_loop(
        vec![create_user_message("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(Vec::new()),
        },
        config,
        emit,
        None,
        Some(stream_fn),
    )
    .await
    .expect("runAgentLoop");

    assert_eq!(*ordering.lock().unwrap(), vec!["finishTurn", "turn_end"]);
    assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
    assert_eq!(steering_polls.load(Ordering::SeqCst), 1);
    assert_eq!(follow_up_polls.load(Ordering::SeqCst), 0);
}

/// TS: "runs finishTurn for a error assistant before turn_end without changing the hard exit"
#[tokio::test]
async fn runs_finish_turn_for_a_error_assistant_before_turn_end_without_changing_the_hard_exit() {
    hard_exit_case(StopReason::Error).await;
}

/// TS: "runs finishTurn for a aborted assistant before turn_end without changing the hard exit"
#[tokio::test]
async fn runs_finish_turn_for_a_aborted_assistant_before_turn_end_without_changing_the_hard_exit() {
    hard_exit_case(StopReason::Aborted).await;
}

/// TS: "action:end skips queue polling and next-turn preparation"
#[tokio::test]
async fn action_end_skips_queue_polling_and_next_turn_preparation() {
    let tool = agent_tool(
        "noop",
        empty_schema(),
        Arc::new(|_id, _params, _signal, _on_update| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text("done"))],
                    details: JsonValue::Null,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );

    let provider_calls = Arc::new(AtomicUsize::new(0));
    let steering_polls = Arc::new(AtomicUsize::new(0));
    let follow_up_polls = Arc::new(AtomicUsize::new(0));
    let prepare_next_turn_calls = Arc::new(AtomicUsize::new(0));

    let steering_for_hook = steering_polls.clone();
    let follow_up_for_hook = follow_up_polls.clone();
    let prepare_for_hook = prepare_next_turn_calls.clone();
    let mut config = base_config();
    config.finish_turn = Some(Arc::new(|_turn, _signal| {
        Box::pin(async move { Some(kiana_agent_core::types::AgentTurnDecision::End) })
    }));
    config.prepare_next_turn = Some(Arc::new(move |_turn| {
        let prepare_next_turn_calls = prepare_for_hook.clone();
        Box::pin(async move {
            prepare_next_turn_calls.fetch_add(1, Ordering::SeqCst);
            None
        })
    }));
    config.get_steering_messages = Some(Arc::new(move || {
        let steering_polls = steering_for_hook.clone();
        Box::pin(async move {
            steering_polls.fetch_add(1, Ordering::SeqCst);
            Vec::new()
        })
    }));
    config.get_follow_up_messages = Some(Arc::new(move || {
        let follow_up_polls = follow_up_for_hook.clone();
        Box::pin(async move {
            follow_up_polls.fetch_add(1, Ordering::SeqCst);
            vec![create_user_message("queued")]
        })
    }));

    let provider_for_fn = provider_calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        provider_for_fn.fetch_add(1, Ordering::SeqCst);
        done_stream(create_assistant_message(
            vec![AssistantContentBlock::ToolCall(tool_call(
                "tool-1",
                "noop",
                serde_json::json!({}),
            ))],
            StopReason::ToolUse,
        ))
    });

    let stream = agent_loop(
        vec![create_user_message("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = await_result(&stream).await;

    assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
    assert_eq!(steering_polls.load(Ordering::SeqCst), 1);
    assert_eq!(follow_up_polls.load(Ordering::SeqCst), 0);
    assert_eq!(prepare_next_turn_calls.load(Ordering::SeqCst), 0);
}

/// TS: "makes exactly one context-only request when no natural request satisfies continuation"
#[tokio::test]
async fn makes_exactly_one_context_only_request_when_no_natural_request_satisfies_continuation() {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let finish_calls = Arc::new(AtomicUsize::new(0));

    let finish_for_hook = finish_calls.clone();
    let mut config = base_config();
    config.finish_turn = Some(Arc::new(move |_turn, _signal| {
        let finish_calls = finish_for_hook.clone();
        Box::pin(async move {
            let count = finish_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if count == 1 {
                Some(kiana_agent_core::types::AgentTurnDecision::Continue)
            } else {
                None
            }
        })
    }));

    let provider_for_fn = provider_calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let count = provider_for_fn.fetch_add(1, Ordering::SeqCst) + 1;
        done_stream(create_assistant_message(
            vec![text_block(format!("response {count}"))],
            StopReason::Stop,
        ))
    });

    let stream = agent_loop(
        vec![create_user_message("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(Vec::new()),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = await_result(&stream).await;

    assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
    assert_eq!(finish_calls.load(Ordering::SeqCst), 2);
}

/// TS: "lets a natural tool-result request satisfy continuation"
#[tokio::test]
async fn lets_a_natural_tool_result_request_satisfy_continuation() {
    let tool = agent_tool(
        "noop",
        empty_schema(),
        Arc::new(|_id, _params, _signal, _on_update| {
            Box::pin(async move { Ok(result_with_text("done")) })
        }),
    );

    let provider_calls = Arc::new(AtomicUsize::new(0));
    let finish_calls = Arc::new(AtomicUsize::new(0));

    let finish_for_hook = finish_calls.clone();
    let mut config = base_config();
    config.finish_turn = Some(Arc::new(move |_turn, _signal| {
        let finish_calls = finish_for_hook.clone();
        Box::pin(async move {
            let count = finish_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if count == 1 {
                Some(kiana_agent_core::types::AgentTurnDecision::Continue)
            } else {
                None
            }
        })
    }));

    let provider_for_fn = provider_calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let count = provider_for_fn.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 1 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "noop",
                    serde_json::json!({}),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = await_result(&stream).await;

    assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
    assert_eq!(finish_calls.load(Ordering::SeqCst), 2);
}

/// `it.each(["steering", "follow-up"])` 的公共实现。
async fn natural_queue_satisfies_continuation(queue_kind: &str) {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let finish_calls = Arc::new(AtomicUsize::new(0));
    let steering_polls = Arc::new(AtomicUsize::new(0));
    let follow_up_delivered = Arc::new(AtomicBool::new(false));
    let second_request_users: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let steering_for_hook = steering_polls.clone();
    let follow_up_for_hook = follow_up_delivered.clone();
    let finish_for_hook = finish_calls.clone();
    let queue_kind = queue_kind.to_string();
    let steering_kind = queue_kind.clone();
    let follow_up_kind = queue_kind.clone();
    let mut config = base_config();
    config.finish_turn = Some(Arc::new(move |_turn, _signal| {
        let finish_calls = finish_for_hook.clone();
        Box::pin(async move {
            let count = finish_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if count == 1 {
                Some(kiana_agent_core::types::AgentTurnDecision::Continue)
            } else {
                None
            }
        })
    }));
    config.get_steering_messages = Some(Arc::new(move || {
        let steering_polls = steering_for_hook.clone();
        let steering_kind = steering_kind.clone();
        Box::pin(async move {
            let count = steering_polls.fetch_add(1, Ordering::SeqCst) + 1;
            if steering_kind == "steering" && count == 2 {
                vec![create_user_message(steering_kind.clone())]
            } else {
                Vec::new()
            }
        })
    }));
    config.get_follow_up_messages = Some(Arc::new(move || {
        let follow_up_delivered = follow_up_for_hook.clone();
        let follow_up_kind = follow_up_kind.clone();
        Box::pin(async move {
            if follow_up_kind != "follow-up" || follow_up_delivered.swap(true, Ordering::SeqCst) {
                return Vec::new();
            }
            vec![create_user_message(follow_up_kind.clone())]
        })
    }));

    let provider_for_fn = provider_calls.clone();
    let second_users_for_fn = second_request_users.clone();
    let stream_fn = stream_fn_with(move |_model, context, _options| {
        let count = provider_for_fn.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 2 {
            let users: Vec<String> = context
                .as_context()
                .messages
                .iter()
                .filter_map(|message| match message {
                    Message::User(UserMessage {
                        content: UserContent::Text(content),
                        ..
                    }) => Some(content.clone()),
                    _ => None,
                })
                .collect();
            *second_users_for_fn.lock().unwrap() = users;
        }
        done_stream(create_assistant_message(
            vec![text_block("done")],
            StopReason::Stop,
        ))
    });

    let stream = agent_loop(
        vec![create_user_message("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(Vec::new()),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = await_result(&stream).await;

    assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
    assert_eq!(finish_calls.load(Ordering::SeqCst), 2);
    assert!(second_request_users.lock().unwrap().contains(&queue_kind));
}

/// TS: "lets a natural steering request satisfy continuation"
#[tokio::test]
async fn lets_a_natural_steering_request_satisfy_continuation() {
    natural_queue_satisfies_continuation("steering").await;
}

/// TS: "lets a natural follow-up request satisfy continuation"
#[tokio::test]
async fn lets_a_natural_follow_up_request_satisfy_continuation() {
    natural_queue_satisfies_continuation("follow-up").await;
}

/// TS: "prepares the initial request after pending messages and can replace request state"
#[tokio::test]
async fn prepares_the_initial_request_after_pending_messages_and_can_replace_request_state() {
    let replacement_model = Model {
        id: "replacement".to_string(),
        name: "replacement".to_string(),
        ..create_model()
    };
    let canonical_message = create_user_message("canonical projection");
    let steering_message = create_user_message("steering");
    let completed_messages: Arc<Mutex<Vec<AgentMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let steering_delivered = Arc::new(AtomicBool::new(false));
    let prepare_calls = Arc::new(AtomicUsize::new(0));

    let steering_for_hook = steering_delivered.clone();
    let steering_message_for_hook = steering_message.clone();
    let prepare_for_hook = prepare_calls.clone();
    let completed_for_hook = completed_messages.clone();
    let canonical_for_hook = canonical_message.clone();
    let replacement_for_hook = replacement_model.clone();
    let expected_steering = steering_message.clone();
    let mut config = base_config();
    config.get_steering_messages = Some(Arc::new(move || {
        let steering_delivered = steering_for_hook.clone();
        let steering_message = steering_message_for_hook.clone();
        Box::pin(async move {
            if steering_delivered.swap(true, Ordering::SeqCst) {
                Vec::new()
            } else {
                vec![steering_message]
            }
        })
    }));
    config.prepare_request = Some(Arc::new(move |context, _signal| {
        let prepare_calls = prepare_for_hook.clone();
        let completed_messages = completed_for_hook.clone();
        let canonical_message = canonical_for_hook.clone();
        let replacement_model = replacement_for_hook.clone();
        let expected_steering = expected_steering.clone();
        Box::pin(async move {
            prepare_calls.fetch_add(1, Ordering::SeqCst);
            assert!(
                completed_messages
                    .lock()
                    .unwrap()
                    .contains(&expected_steering)
            );
            assert!(context.context.messages.contains(&expected_steering));
            Some(kiana_agent_core::types::AgentRequestUpdate {
                context: Some(AgentContext {
                    messages: vec![canonical_message],
                    tools: None,
                }),
                model: Some(replacement_model),
                thinking_level: Some(kiana_agent_core::types::ThinkingLevel::High),
            })
        })
    }));

    let seen_model: Arc<Mutex<Option<Model>>> = Arc::new(Mutex::new(None));
    let seen_messages: Arc<Mutex<Option<Vec<Message>>>> = Arc::new(Mutex::new(None));
    let seen_reasoning: Arc<Mutex<Option<kiana_agent_core::types::AiThinkingLevel>>> =
        Arc::new(Mutex::new(None));
    let model_for_fn = seen_model.clone();
    let messages_for_fn = seen_messages.clone();
    let reasoning_for_fn = seen_reasoning.clone();
    let stream_fn = stream_fn_with(move |model, context, options| {
        *model_for_fn.lock().unwrap() = Some(model);
        *messages_for_fn.lock().unwrap() = Some(context.as_context().messages.clone());
        *reasoning_for_fn.lock().unwrap() = options.as_ref().and_then(|options| options.reasoning);
        done_stream(create_assistant_message(
            vec![text_block("done")],
            StopReason::Stop,
        ))
    });

    let completed_for_sink = completed_messages.clone();
    let emit: AgentEventSink = Arc::new(move |event| {
        let completed_messages = completed_for_sink.clone();
        Box::pin(async move {
            if let AgentEvent::MessageEnd { message } = event {
                completed_messages.lock().unwrap().push(message);
            }
        })
    });

    run_agent_loop(
        vec![create_user_message("prompt")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(Vec::new()),
        },
        config,
        emit,
        None,
        Some(stream_fn),
    )
    .await
    .expect("runAgentLoop");

    assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
    assert_eq!(*seen_model.lock().unwrap(), Some(replacement_model));
    let canonical: Vec<Message> = vec![match &canonical_message {
        AgentMessage::Standard(message) => message.clone(),
        AgentMessage::Custom(_) => unreachable!(),
    }];
    assert_eq!(*seen_messages.lock().unwrap(), Some(canonical));
    assert_eq!(
        *seen_reasoning.lock().unwrap(),
        Some(kiana_agent_core::types::AiThinkingLevel::High)
    );
}

/// TS: "does not poll steering after prepareRequest"
#[tokio::test]
async fn does_not_poll_steering_after_prepare_request() {
    let queued: Arc<Mutex<Vec<AgentMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let late_steering = create_user_message("late steering");
    let request_included_steering: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let request_preparations = Arc::new(AtomicUsize::new(0));
    let steering_polls = Arc::new(AtomicUsize::new(0));

    let queued_for_hook = queued.clone();
    let steering_polls_for_hook = steering_polls.clone();
    let queued_for_prepare = queued.clone();
    let late_for_prepare = late_steering.clone();
    let preparations_for_hook = request_preparations.clone();
    let mut config = base_config();
    config.get_steering_messages = Some(Arc::new(move || {
        let queued = queued_for_hook.clone();
        let steering_polls = steering_polls_for_hook.clone();
        Box::pin(async move {
            steering_polls.fetch_add(1, Ordering::SeqCst);
            std::mem::take(&mut *queued.lock().unwrap())
        })
    }));
    config.prepare_request = Some(Arc::new(move |_context, _signal| {
        let queued = queued_for_prepare.clone();
        let late_steering = late_for_prepare.clone();
        let preparations = preparations_for_hook.clone();
        Box::pin(async move {
            let count = preparations.fetch_add(1, Ordering::SeqCst) + 1;
            if count == 1 {
                queued.lock().unwrap().push(late_steering);
            }
            None
        })
    }));

    let included_for_fn = request_included_steering.clone();
    let stream_fn = stream_fn_with(move |_model, context, _options| {
        let included = contains_user_text(context.as_context(), "late steering");
        included_for_fn.lock().unwrap().push(included);
        done_stream(create_assistant_message(
            vec![text_block("done")],
            StopReason::Stop,
        ))
    });

    let stream = agent_loop(
        vec![create_user_message("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(Vec::new()),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = await_result(&stream).await;

    assert_eq!(
        *request_included_steering.lock().unwrap(),
        vec![false, true]
    );
    assert_eq!(request_preparations.load(Ordering::SeqCst), 2);
    // 启动、turn 后投递、以及最终自然停止各一次。
    assert_eq!(steering_polls.load(Ordering::SeqCst), 3);
}

/// TS: "should use prepareNextTurn snapshot before continuing"
#[tokio::test]
async fn should_use_prepare_next_turn_snapshot_before_continuing() {
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(format!("echoed: {value}")))],
                    details: serde_json::json!({ "value": value }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );

    let converted_second_turn_has_update = Arc::new(AtomicBool::new(false));
    let prepare_calls = Arc::new(AtomicUsize::new(0));
    let prepared = Arc::new(AtomicBool::new(false));

    let prepare_calls_for_hook = prepare_calls.clone();
    let prepared_for_hook = prepared.clone();
    let mut config = base_config();
    config.prepare_next_turn = Some(Arc::new(move |turn| {
        let prepare_calls = prepare_calls_for_hook.clone();
        let prepared = prepared_for_hook.clone();
        Box::pin(async move {
            prepare_calls.fetch_add(1, Ordering::SeqCst);
            if prepared.swap(true, Ordering::SeqCst) {
                return None;
            }
            Some(kiana_agent_core::types::AgentLoopTurnUpdate {
                context: Some(AgentContext {
                    messages: turn.context.messages.clone(),
                    tools: turn.context.tools.clone(),
                }),
                messages: Some(vec![create_system_message("updated guidance", None)]),
                model: None,
                thinking_level: None,
            })
        })
    }));

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let flag_for_fn = converted_second_turn_has_update.clone();
    let calls_for_fn = llm_calls.clone();
    let stream_fn = stream_fn_with(move |_model, context, _options| {
        let count = calls_for_fn.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 2 {
            let has_update = context.as_context().messages.iter().any(|message| {
                matches!(
                    message,
                    Message::System(kiana_ai::types::SystemMessage {
                        content: kiana_ai::types::SystemContent::Text(content),
                        ..
                    }) if content == "updated guidance"
                )
            });
            flag_for_fn.store(has_update, Ordering::SeqCst);
        }
        if count == 1 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "echo",
                    serde_json::json!({ "value": "hello" }),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect_events(&stream).await;

    assert_eq!(llm_calls.load(Ordering::SeqCst), 2);
    assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
    assert!(converted_second_turn_has_update.load(Ordering::SeqCst));
}

/// TS: "picks up steering queued during prepareNextTurn before the next request"
#[tokio::test]
async fn picks_up_steering_queued_during_prepare_next_turn_before_the_next_request() {
    let tool = agent_tool(
        "noop",
        empty_schema(),
        Arc::new(|_id, _params, _signal, _on_update| {
            Box::pin(async move { Ok(result_with_text("done")) })
        }),
    );

    let queued: Arc<Mutex<Vec<AgentMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let late_steering = create_user_message("late steering");
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let second_request_included_steering = Arc::new(AtomicBool::new(false));

    let queued_for_prepare = queued.clone();
    let late_for_prepare = late_steering.clone();
    let queued_for_steering = queued.clone();
    let mut config = base_config();
    config.prepare_next_turn = Some(Arc::new(move |_turn| {
        let queued = queued_for_prepare.clone();
        let late_steering = late_for_prepare.clone();
        Box::pin(async move {
            queued.lock().unwrap().push(late_steering);
            None
        })
    }));
    config.get_steering_messages = Some(Arc::new(move || {
        let queued = queued_for_steering.clone();
        Box::pin(async move { std::mem::take(&mut *queued.lock().unwrap()) })
    }));

    let calls_for_fn = provider_calls.clone();
    let flag_for_fn = second_request_included_steering.clone();
    let stream_fn = stream_fn_with(move |_model, context, _options| {
        let count = calls_for_fn.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 2 {
            flag_for_fn.store(
                contains_user_text(context.as_context(), "late steering"),
                Ordering::SeqCst,
            );
        }
        if count == 1 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "noop",
                    serde_json::json!({}),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = await_result(&stream).await;

    assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
    assert!(second_request_included_steering.load(Ordering::SeqCst));
}

/// TS: "action:end receives finalized turn context and stops before queue polling"
#[tokio::test]
async fn action_end_receives_finalized_turn_context_and_stops_before_queue_polling() {
    let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_for_tool = executed.clone();
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            let executed = executed_for_tool.clone();
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                executed.lock().unwrap().push(value.clone());
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(format!("echoed: {value}")))],
                    details: serde_json::json!({ "value": value }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );

    let steering_polls = Arc::new(AtomicUsize::new(0));
    let follow_up_polls = Arc::new(AtomicUsize::new(0));
    let callback_tool_result_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let callback_context_roles: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let steering_for_hook = steering_polls.clone();
    let follow_up_for_hook = follow_up_polls.clone();
    let ids_for_hook = callback_tool_result_ids.clone();
    let roles_for_hook = callback_context_roles.clone();
    let mut config = base_config();
    config.finish_turn = Some(Arc::new(move |turn, _signal| {
        let ids_for_hook = ids_for_hook.clone();
        let roles_for_hook = roles_for_hook.clone();
        Box::pin(async move {
            assert_eq!(turn.message.role, AssistantRole::Assistant);
            *ids_for_hook.lock().unwrap() = turn
                .tool_results
                .iter()
                .map(|result| result.tool_call_id.clone())
                .collect();
            *roles_for_hook.lock().unwrap() = turn
                .context
                .messages
                .iter()
                .map(|message| message.role().to_string())
                .collect();
            Some(kiana_agent_core::types::AgentTurnDecision::End)
        })
    }));
    config.get_steering_messages = Some(Arc::new(move || {
        let steering_polls = steering_for_hook.clone();
        Box::pin(async move {
            steering_polls.fetch_add(1, Ordering::SeqCst);
            Vec::new()
        })
    }));
    config.get_follow_up_messages = Some(Arc::new(move || {
        let follow_up_polls = follow_up_for_hook.clone();
        Box::pin(async move {
            follow_up_polls.fetch_add(1, Ordering::SeqCst);
            vec![create_user_message("follow up should stay queued")]
        })
    }));

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = llm_calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let count = calls_for_fn.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 1 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "echo",
                    serde_json::json!({ "value": "hello" }),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("should not run")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;
    let messages = await_result(&stream).await;

    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(*executed.lock().unwrap(), vec!["hello".to_string()]);
    assert_eq!(steering_polls.load(Ordering::SeqCst), 1);
    assert_eq!(follow_up_polls.load(Ordering::SeqCst), 0);
    assert_eq!(*callback_tool_result_ids.lock().unwrap(), vec!["tool-1"]);
    assert_eq!(
        *callback_context_roles.lock().unwrap(),
        vec!["system", "user", "assistant", "toolResult"]
    );
    // 上下文没有声明工具，因此循环用一条 system 消息宣布装载。
    assert_eq!(
        roles(&messages),
        vec!["system", "user", "assistant", "toolResult"]
    );
    let event_types: Vec<&str> = events.iter().map(AgentEvent::kind).collect();
    assert_eq!(
        event_types,
        vec![
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "tool_execution_start",
            "tool_execution_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
}

/// TS: "should stop after a tool batch when every tool result sets terminate=true"
#[tokio::test]
async fn should_stop_after_a_tool_batch_when_every_tool_result_sets_terminate_true() {
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(format!("echoed: {value}")))],
                    details: serde_json::json!({ "value": value }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(true),
                })
            })
        }),
    );

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = llm_calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        calls_for_fn.fetch_add(1, Ordering::SeqCst);
        done_stream(create_assistant_message(
            vec![AssistantContentBlock::ToolCall(tool_call(
                "tool-1",
                "echo",
                serde_json::json!({ "value": "hello" }),
            ))],
            StopReason::ToolUse,
        ))
    });

    let stream = agent_loop(
        vec![create_user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;
    let messages = await_result(&stream).await;

    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        roles(&messages),
        vec!["system", "user", "assistant", "toolResult"]
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::TurnEnd { .. }))
            .count(),
        1
    );
}

/// TS: "should stop after a blocked tool call when beforeToolCall sets terminate=true"
#[tokio::test]
async fn should_stop_after_a_blocked_tool_call_when_before_tool_call_sets_terminate_true() {
    let executed = Arc::new(AtomicBool::new(false));
    let executed_for_tool = executed.clone();
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, _params, _signal, _on_update| {
            let executed = executed_for_tool.clone();
            Box::pin(async move {
                executed.store(true, Ordering::SeqCst);
                Ok(result_with_text("should not execute"))
            })
        }),
    );

    let mut config = base_config();
    config.before_tool_call = Some(Arc::new(|_context, _signal| {
        Box::pin(async move {
            Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some("Blocked by policy".to_string()),
                terminate: Some(true),
            })
        })
    }));

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = llm_calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let count = calls_for_fn.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 1 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "echo",
                    serde_json::json!({ "value": "hello" }),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("should not run")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect_events(&stream).await;
    let messages = await_result(&stream).await;

    let tool_result = messages
        .iter()
        .find_map(|message| match message {
            AgentMessage::Standard(Message::ToolResult(result)) => Some(result.clone()),
            _ => None,
        })
        .expect("tool result");
    assert!(!executed.load(Ordering::SeqCst));
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert!(tool_result.is_error);
    assert_eq!(
        tool_result.content,
        vec![UserContentBlock::Text(text("Blocked by policy"))]
    );
}

/// TS: "should continue after a mixed batch with one terminating blocked call"
#[tokio::test]
async fn should_continue_after_a_mixed_batch_with_one_terminating_blocked_call() {
    let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_for_tool = executed.clone();
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            let executed = executed_for_tool.clone();
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                executed.lock().unwrap().push(value.clone());
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(format!("echoed: {value}")))],
                    details: serde_json::json!({ "value": value }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );

    let mut config = base_config();
    config.tool_execution = Some(ToolExecutionMode::Parallel);
    config.before_tool_call = Some(Arc::new(|context, _signal| {
        Box::pin(async move {
            let args = context.args.get();
            let value = args["value"].as_str().unwrap_or_default().to_string();
            if value == "first" {
                Some(BeforeToolCallResult {
                    block: Some(true),
                    reason: Some("Blocked first".to_string()),
                    terminate: Some(true),
                })
            } else {
                None
            }
        })
    }));

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = llm_calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let count = calls_for_fn.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 1 {
            done_stream(create_assistant_message(
                vec![
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-1",
                        "echo",
                        serde_json::json!({ "value": "first" }),
                    )),
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-2",
                        "echo",
                        serde_json::json!({ "value": "second" }),
                    )),
                ],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("echo both")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect_events(&stream).await;

    assert_eq!(*executed.lock().unwrap(), vec!["second".to_string()]);
    assert_eq!(llm_calls.load(Ordering::SeqCst), 2);
}

/// TS: "should continue after parallel tool calls when not all tool results terminate"
#[tokio::test]
async fn should_continue_after_parallel_tool_calls_when_not_all_tool_results_terminate() {
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(format!("echoed: {value}")))],
                    details: serde_json::json!({ "value": value }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(value == "first"),
                })
            })
        }),
    );

    let mut config = base_config();
    config.tool_execution = Some(ToolExecutionMode::Parallel);

    let call_index = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = call_index.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let index = calls_for_fn.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            done_stream(create_assistant_message(
                vec![
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-1",
                        "echo",
                        serde_json::json!({ "value": "first" }),
                    )),
                    AssistantContentBlock::ToolCall(tool_call(
                        "tool-2",
                        "echo",
                        serde_json::json!({ "value": "second" }),
                    )),
                ],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("echo both")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect_events(&stream).await;
    let messages = await_result(&stream).await;

    assert_eq!(call_index.load(Ordering::SeqCst), 2);
    assert_eq!(
        roles(&messages),
        vec![
            "system",
            "user",
            "assistant",
            "toolResult",
            "toolResult",
            "assistant"
        ]
    );
}

/// TS: "should allow afterToolCall to mark a tool batch as terminating"
#[tokio::test]
async fn should_allow_after_tool_call_to_mark_a_tool_batch_as_terminating() {
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, params, _signal, _on_update| {
            Box::pin(async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(format!("echoed: {value}")))],
                    details: serde_json::json!({ "value": value }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );

    let mut config = base_config();
    config.after_tool_call = Some(Arc::new(|_context, _signal| {
        Box::pin(async move {
            Some(AfterToolCallResult {
                terminate: Some(true),
                ..Default::default()
            })
        })
    }));

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = llm_calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        calls_for_fn.fetch_add(1, Ordering::SeqCst);
        done_stream(create_assistant_message(
            vec![AssistantContentBlock::ToolCall(tool_call(
                "tool-1",
                "echo",
                serde_json::json!({ "value": "hello" }),
            ))],
            StopReason::ToolUse,
        ))
    });

    let stream = agent_loop(
        vec![create_user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect_events(&stream).await;

    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// agentLoopContinue with AgentMessage
// ---------------------------------------------------------------------------

/// TS: "should throw when context has no messages"
#[tokio::test]
async fn should_throw_when_context_has_no_messages() {
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(Vec::new()),
    };
    let stream_fn = stream_fn_with(
        |_model, _context, _options| -> AssistantMessageEventStream {
            panic!("Unexpected stream call")
        },
    );
    match agent_loop_continue(context, base_config(), None, Some(stream_fn)) {
        Err(error) => assert_eq!(error.to_string(), "Cannot continue: no messages in context"),
        Ok(_) => panic!("agentLoopContinue should have thrown"),
    }
}

/// TS: "should continue from existing context without emitting user message events"
#[tokio::test]
async fn should_continue_from_existing_context_without_emitting_user_message_events() {
    let context = AgentContext {
        messages: vec![create_user_message("Hello")],
        tools: Some(Vec::new()),
    };
    let stream = agent_loop_continue(
        context,
        base_config(),
        None,
        Some(done_stream_fn(create_assistant_message(
            vec![text_block("Response")],
            StopReason::Stop,
        ))),
    )
    .expect("agentLoopContinue");

    let events = collect_events(&stream).await;
    let messages = await_result(&stream).await;

    // 只返回新的 assistant 消息（不含已有 user 消息）。
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role(), "assistant");

    // 不应有 user 消息事件（与 agentLoop 的关键区别）。
    let message_end_events: Vec<&AgentEvent> = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::MessageEnd { .. }))
        .collect();
    assert_eq!(message_end_events.len(), 1);
    match message_end_events[0] {
        AgentEvent::MessageEnd { message } => assert_eq!(message.role(), "assistant"),
        _ => unreachable!(),
    }
}

/// TS: "should allow custom message types as last message (caller responsibility)"
#[tokio::test]
async fn should_allow_custom_message_types_as_last_message() {
    let mut custom_message = JsonObject::new();
    custom_message.insert("role".to_string(), serde_json::json!("custom"));
    custom_message.insert("text".to_string(), serde_json::json!("Hook content"));
    custom_message.insert("timestamp".to_string(), serde_json::json!(now_ms()));

    let context = AgentContext {
        messages: vec![AgentMessage::Custom(Box::new(custom_message))],
        tools: Some(Vec::new()),
    };
    let mut config = base_config();
    config.convert_to_llm = Arc::new(|messages| {
        messages
            .into_iter()
            .map(|message| match message {
                AgentMessage::Custom(custom)
                    if custom.get("role").and_then(JsonValue::as_str) == Some("custom") =>
                {
                    Message::User(UserMessage {
                        role: UserRole::User,
                        content: UserContent::Text(
                            custom
                                .get("text")
                                .and_then(JsonValue::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        ),
                        timestamp: custom
                            .get("timestamp")
                            .and_then(JsonValue::as_i64)
                            .unwrap_or_default(),
                    })
                }
                AgentMessage::Standard(message) => message,
                AgentMessage::Custom(_) => Message::User(UserMessage {
                    role: UserRole::User,
                    content: UserContent::Text(String::new()),
                    timestamp: 0,
                }),
            })
            .filter(|message| {
                message.role() == "user"
                    || message.role() == "assistant"
                    || message.role() == "toolResult"
            })
            .collect()
    });

    let stream = agent_loop_continue(
        context,
        config,
        None,
        Some(done_stream_fn(create_assistant_message(
            vec![text_block("Response to custom message")],
            StopReason::Stop,
        ))),
    )
    .expect("agentLoopContinue");

    let _ = collect_events(&stream).await;
    let messages = await_result(&stream).await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role(), "assistant");
}

// ---------------------------------------------------------------------------
// runToolCall
// ---------------------------------------------------------------------------

fn echo_run_tool_call_tool() -> AgentTool {
    let mut tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(|_id, params, _signal, on_update| {
            Box::pin(async move {
                if let Some(on_update) = on_update {
                    on_update(AgentToolResult {
                        content: vec![UserContentBlock::Text(text("partial"))],
                        details: serde_json::json!({}),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                    });
                }
                let value = params["value"].clone();
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text(
                        params["value"].as_str().unwrap_or_default(),
                    ))],
                    details: serde_json::json!({}),
                    structured_content: Some(serde_json::json!({ "value": value })),
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );
    tool.output_schema = Some(serde_json::json!({
        "type": "object",
        "properties": { "value": { "type": "string" } },
        "required": ["value"]
    }));
    tool
}

fn failing_run_tool_call_tool() -> AgentTool {
    agent_tool(
        "failing",
        empty_schema(),
        Arc::new(|_id, _params, _signal, _on_update| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(text("bad"))],
                    details: serde_json::json!({ "partial": true }),
                    structured_content: None,
                    usage: None,
                    is_error: Some(true),
                    terminate: None,
                })
            })
        }),
    )
}

/// TS: "validates, runs the hooks, and reports failures as error outcomes"
#[tokio::test]
async fn validates_runs_the_hooks_and_reports_failures_as_error_outcomes() {
    let hook_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let updates: Arc<Mutex<Vec<AgentToolResult<JsonValue>>>> = Arc::new(Mutex::new(Vec::new()));

    let hook_calls_for_before = hook_calls.clone();
    let hook_calls_for_after = hook_calls.clone();
    let updates_for_sink = updates.clone();
    let options = RunToolCallOptions {
        tools: vec![echo_run_tool_call_tool(), failing_run_tool_call_tool()],
        assistant_message: create_assistant_message(Vec::new(), StopReason::Stop),
        context: AgentContext {
            messages: Vec::new(),
            tools: None,
        },
        signal: None,
        on_update: Some(Arc::new(move |partial| -> BoxFuture<'static, ()> {
            let updates = updates_for_sink.clone();
            Box::pin(async move {
                updates.lock().unwrap().push(partial);
            })
        }) as ToolUpdateSink),
        before_tool_call: Some(Arc::new(move |context, _signal| {
            let hook_calls = hook_calls_for_before.clone();
            Box::pin(async move {
                hook_calls
                    .lock()
                    .unwrap()
                    .push(format!("before {}", context.tool_call.id));
                if context.args.get()["value"].as_str() == Some("blocked") {
                    return Some(BeforeToolCallResult {
                        block: Some(true),
                        reason: Some("nope".to_string()),
                        terminate: None,
                    });
                }
                None
            })
        })),
        after_tool_call: Some(Arc::new(move |context, _signal| {
            let hook_calls = hook_calls_for_after.clone();
            Box::pin(async move {
                hook_calls
                    .lock()
                    .unwrap()
                    .push(format!("after {}", context.tool_call.id));
                None
            })
        })),
    };

    let outcome = run_tool_call(
        tool_call("a", "echo", serde_json::json!({ "value": "a" })),
        options.clone(),
    )
    .await;
    assert_eq!(outcome.tool_call.id, "a");
    assert_eq!(
        outcome.result.structured_content,
        Some(serde_json::json!({ "value": "a" }))
    );
    assert!(!outcome.is_error);

    let outcome = run_tool_call(
        tool_call(
            "b",
            "echo",
            serde_json::json!({ "value": { "nested": true } }),
        ),
        options.clone(),
    )
    .await;
    assert!(outcome.is_error);

    let outcome = run_tool_call(
        tool_call("c", "echo", serde_json::json!({ "value": "blocked" })),
        options.clone(),
    )
    .await;
    assert_eq!(
        outcome.result.content,
        vec![UserContentBlock::Text(text("nope"))]
    );
    assert!(outcome.is_error);

    let outcome = run_tool_call(
        tool_call("d", "missing", serde_json::json!({})),
        options.clone(),
    )
    .await;
    assert_eq!(
        outcome.result.content,
        vec![UserContentBlock::Text(text("Tool missing not found"))]
    );
    assert!(outcome.is_error);

    // 错误结果保留自己的 details。
    let outcome = run_tool_call(
        tool_call("e", "failing", serde_json::json!({})),
        options.clone(),
    )
    .await;
    assert_eq!(
        outcome.result.details,
        serde_json::json!({ "partial": true })
    );
    assert!(outcome.is_error);

    let updates = updates.lock().unwrap().clone();
    assert_eq!(updates.len(), 1);
    assert_eq!(
        updates[0].content,
        vec![UserContentBlock::Text(text("partial"))]
    );
    assert_eq!(updates[0].details, serde_json::json!({}));

    // 校验失败与未知工具不会触达钩子；被拦截的调用跳过 afterToolCall。
    assert_eq!(
        *hook_calls.lock().unwrap(),
        vec!["before a", "after a", "before c", "before e", "after e"]
    );
}

/// TS: "lets afterToolCall replace structured content and drops it when only content is replaced"
#[tokio::test]
async fn lets_after_tool_call_replace_structured_content_and_drops_it_when_only_content_is_replaced()
 {
    let redacted = vec![UserContentBlock::Text(text("redacted"))];
    let after_results = vec![
        AfterToolCallResult {
            content: Some(redacted.clone()),
            ..Default::default()
        },
        AfterToolCallResult {
            structured_content: Some(serde_json::json!({ "value": "replaced" })),
            ..Default::default()
        },
        AfterToolCallResult {
            content: Some(redacted.clone()),
            structured_content: Some(serde_json::json!({ "value": "both" })),
            ..Default::default()
        },
        AfterToolCallResult {
            details: Some(serde_json::json!({ "note": "kept" })),
            ..Default::default()
        },
    ];

    let mut seen = Vec::new();
    for after_result in after_results {
        let after_for_hook = after_result.clone();
        let outcome = run_tool_call(
            tool_call("x", "echo", serde_json::json!({ "value": "original" })),
            RunToolCallOptions {
                tools: vec![echo_run_tool_call_tool()],
                assistant_message: create_assistant_message(Vec::new(), StopReason::Stop),
                context: AgentContext {
                    messages: Vec::new(),
                    tools: None,
                },
                signal: None,
                on_update: None,
                before_tool_call: None,
                after_tool_call: Some(Arc::new(move |_context, _signal| {
                    let after_result = after_for_hook.clone();
                    Box::pin(async move { Some(after_result) })
                })),
            },
        )
        .await;
        seen.push(outcome.result.structured_content);
    }
    assert_eq!(
        seen,
        vec![
            None,
            Some(serde_json::json!({ "value": "replaced" })),
            Some(serde_json::json!({ "value": "both" })),
            Some(serde_json::json!({ "value": "original" })),
        ]
    );
}

// ---------------------------------------------------------------------------
// 追加覆盖：流式 partial 事件（TS 测试集没有覆盖 start/delta 分支，这里按
// `upstream-reference` 里的行为探针对齐，锁住 `streamAssistantResponse` 的事件语义）。
// ---------------------------------------------------------------------------

fn event_kinds(events: &[AgentEvent]) -> Vec<&'static str> {
    events.iter().map(AgentEvent::kind).collect()
}

fn assistant_text(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => assistant
            .content
            .iter()
            .map(|block| match block {
                AssistantContentBlock::Text(text) => text.text.clone(),
                _ => String::new(),
            })
            .collect(),
        _ => String::new(),
    }
}

/// 追加用例：`start` 推入 partial，`text_delta` 替换并发出 `message_update`，
/// `done` 用最终消息收尾（与 TS 探针输出逐事件一致）。
#[tokio::test]
async fn partial_streaming_events_replace_the_draft_and_emit_message_update() {
    let stream_fn = stream_fn_with(|_model, _context, _options| {
        let stream = AssistantMessageEventStream::new();
        let _ = stream.push(AssistantMessageEvent::Start {
            partial: Box::new(create_assistant_message(Vec::new(), StopReason::Stop)),
        });
        let _ = stream.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "Hel".to_string(),
            partial: Box::new(create_assistant_message(
                vec![text_block("Hel")],
                StopReason::Stop,
            )),
        });
        let _ = stream.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: Box::new(create_assistant_message(
                vec![text_block("Hello")],
                StopReason::Stop,
            )),
        });
        stream
    });

    let stream = agent_loop(
        vec![create_user_message("Hello")],
        AgentContext {
            messages: Vec::new(),
            tools: None,
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;

    assert_eq!(
        event_kinds(&events),
        vec![
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_update",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );

    // `message_start` 携带的是 `start` 里的 partial（空内容）；之后不再有第二个 assistant 的
    // `message_start`，因为 `done` 走的是替换草稿的分支。
    match &events[4] {
        AgentEvent::MessageStart { message } => assert_eq!(assistant_text(message), ""),
        other => panic!("unexpected event: {other:?}"),
    }

    // `message_update.message` 是 delta 的 partial，内嵌事件原样携带。
    match &events[5] {
        AgentEvent::MessageUpdate {
            message,
            assistant_message_event,
        } => {
            assert_eq!(assistant_text(message), "Hel");
            match assistant_message_event {
                AssistantMessageEvent::TextDelta {
                    content_index,
                    delta,
                    partial,
                } => {
                    assert_eq!(*content_index, 0);
                    assert_eq!(delta, "Hel");
                    assert_eq!(partial.content.len(), 1);
                }
                other => panic!("unexpected assistantMessageEvent: {other:?}"),
            }
        }
        other => panic!("unexpected event: {other:?}"),
    }

    match &events[6] {
        AgentEvent::MessageEnd { message } => assert_eq!(assistant_text(message), "Hello"),
        other => panic!("unexpected event: {other:?}"),
    }

    let messages = await_result(&stream).await;
    assert_eq!(roles(&messages), vec!["user", "assistant"]);
    assert_eq!(assistant_text(&messages[1]), "Hello");
}

/// 追加用例：没有任何事件、直接用 `end(result)` 结束的流，仍会补发
/// `message_start` + `message_end`（TS 探针 `end-without-events` 的行为）。
#[tokio::test]
async fn a_stream_ended_without_events_emits_message_start_for_the_result() {
    let stream_fn = stream_fn_with(|_model, _context, _options| {
        let stream = AssistantMessageEventStream::new();
        stream.end(create_assistant_message(
            vec![text_block("Quiet")],
            StopReason::Stop,
        ));
        stream
    });

    let stream = agent_loop(
        vec![create_user_message("Hello")],
        AgentContext {
            messages: Vec::new(),
            tools: None,
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;

    assert_eq!(
        event_kinds(&events),
        vec![
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
    match &events[4] {
        AgentEvent::MessageStart { message } => assert_eq!(assistant_text(message), "Quiet"),
        other => panic!("unexpected event: {other:?}"),
    }
    match &events[5] {
        AgentEvent::MessageEnd { message } => assert_eq!(assistant_text(message), "Quiet"),
        other => panic!("unexpected event: {other:?}"),
    }

    let messages = await_result(&stream).await;
    assert_eq!(roles(&messages), vec!["user", "assistant"]);
    assert_eq!(assistant_text(&messages[1]), "Quiet");
}

/// 追加用例：`start` 之后收到 `error` 事件时替换草稿并且**不**再发 `message_start`，
/// 循环按 error 的 stop_reason 直接结束（TS 探针 `start-then-error` 的行为）。
#[tokio::test]
async fn an_error_event_after_start_replaces_the_draft_without_a_second_message_start() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        calls_for_fn.fetch_add(1, Ordering::SeqCst);
        let stream = AssistantMessageEventStream::new();
        let _ = stream.push(AssistantMessageEvent::Start {
            partial: Box::new(create_assistant_message(Vec::new(), StopReason::Stop)),
        });
        let _ = stream.push(AssistantMessageEvent::Error {
            reason: ErrorReason::Error,
            error: Box::new(create_assistant_message(
                vec![text_block("oops")],
                StopReason::Error,
            )),
        });
        stream
    });

    let stream = agent_loop(
        vec![create_user_message("Hello")],
        AgentContext {
            messages: Vec::new(),
            tools: None,
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;

    assert_eq!(
        event_kinds(&events),
        vec![
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
    match &events[4] {
        AgentEvent::MessageStart { message } => assert_eq!(assistant_text(message), ""),
        other => panic!("unexpected event: {other:?}"),
    }
    match &events[5] {
        AgentEvent::MessageEnd { message } => {
            assert_eq!(assistant_text(message), "oops");
            match message {
                AgentMessage::Standard(Message::Assistant(assistant)) => {
                    assert_eq!(assistant.stop_reason, StopReason::Error)
                }
                other => panic!("unexpected message: {other:?}"),
            }
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // error 的 stop_reason 走硬退出：只请求一次，不再轮询。
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let messages = await_result(&stream).await;
    assert_eq!(roles(&messages), vec!["user", "assistant"]);
    assert_eq!(assistant_text(&messages[1]), "oops");
}

// ---------------------------------------------------------------------------
// 回归（reviewer findings）：锁定与 TS 逐字等价的关键行为
// ---------------------------------------------------------------------------

fn custom_message_from_json(value: JsonValue) -> AgentMessage {
    match value {
        JsonValue::Object(object) => {
            // serde_json 开了 preserve_order，按插入序收进 IndexMap。
            let object: JsonObject = object.into_iter().collect();
            AgentMessage::Custom(Box::new(object))
        }
        other => panic!("custom message must be a JSON object: {other}"),
    }
}

/// 回归（critical）：`tool_execution_update` 在工具运行期间送达。
///
/// TS 在工具回调里同步调用 `emit(...)`（agent-loop.ts:789-798、831-867），事件在
/// `execute()` 尚未返回时就到达消费者。这里让工具发出 update 后阻塞，直到测试在事件流里
/// 读到这条 update 才放行；若实现把 update 推迟到 `execute()` 返回之后，测试会超时失败。
#[tokio::test]
async fn emits_tool_execution_update_while_the_tool_is_running() {
    let release = Arc::new(Notify::new());
    let release_for_tool = release.clone();
    let tool = agent_tool(
        "streamer",
        empty_schema(),
        Arc::new(move |_id, _params, _signal, on_update| {
            let release = release_for_tool.clone();
            Box::pin(async move {
                let on_update = on_update.expect("tool update callback");
                on_update(AgentToolResult {
                    content: vec![UserContentBlock::Text(text("partial-1"))],
                    details: serde_json::json!({ "n": 1 }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                });
                // 只有 update 事件已经送达，测试才会放行；否则一直等到超时。
                release.notified().await;
                Ok(result_with_text("ok"))
            })
        }),
    );

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        if calls_for_fn.fetch_add(1, Ordering::SeqCst) == 0 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "streamer",
                    serde_json::json!({}),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("go")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );

    let mut update_seen = false;
    let mut update_before_end = false;
    let events = tokio::time::timeout(Duration::from_secs(10), async {
        let mut events = Vec::new();
        let mut end_seen = false;
        while let Some(event) = stream.recv().await {
            match &event {
                AgentEvent::ToolExecutionUpdate {
                    tool_call_id,
                    tool_name,
                    args,
                    partial_result,
                } => {
                    update_seen = true;
                    assert_eq!(tool_call_id, "tool-1");
                    assert_eq!(tool_name, "streamer");
                    assert_eq!(args, &serde_json::json!({}));
                    assert_eq!(partial_result["content"][0]["text"], "partial-1");
                    update_before_end = !end_seen;
                    release.notify_one();
                }
                AgentEvent::ToolExecutionEnd { .. } => end_seen = true,
                _ => {}
            }
            events.push(event);
        }
        events
    })
    .await
    .expect("tool_execution_update 没有在 execute() 返回前送达（被推迟到工具结束后）");

    assert!(update_seen);
    assert!(update_before_end);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolExecutionEnd { .. }))
    );
}

/// 回归（major）：`finishTurn` / `prepareRequest` / `beforeToolCall` / `afterToolCall`
/// 都收到本次 run 的取消令牌（TS 的同名第二个参数 `signal`，agent-loop.ts:228-235、
/// 261、295、739-747、882-892）。
#[tokio::test]
async fn forwards_the_abort_signal_to_finish_prepare_and_tool_hooks() {
    let seen: SeenHookSignals = Arc::new(Mutex::new(Vec::new()));
    let token = CancellationToken::new();

    let mut config = base_config();
    let seen_for_prepare = seen.clone();
    config.prepare_request = Some(Arc::new(move |_context, signal| {
        let seen = seen_for_prepare.clone();
        record_hook_signal(seen, "prepareRequest", signal)
    }));
    let seen_for_before = seen.clone();
    config.before_tool_call = Some(Arc::new(move |_context, signal| {
        let seen = seen_for_before.clone();
        record_hook_signal(seen, "beforeToolCall", signal)
    }));
    let seen_for_after = seen.clone();
    config.after_tool_call = Some(Arc::new(move |_context, signal| {
        let seen = seen_for_after.clone();
        record_hook_signal(seen, "afterToolCall", signal)
    }));
    let seen_for_finish = seen.clone();
    config.finish_turn = Some(Arc::new(move |_turn, signal| {
        let seen = seen_for_finish.clone();
        record_hook_signal(seen, "finishTurn", signal)
    }));

    let tool = agent_tool(
        "noop",
        empty_schema(),
        Arc::new(|_id, _params, _signal, _on_update| {
            Box::pin(async move { Ok(result_with_text("ok")) })
        }),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        if calls_for_fn.fetch_add(1, Ordering::SeqCst) == 0 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "noop",
                    serde_json::json!({}),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("go")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        Some(token),
        Some(stream_fn),
    );
    let _ = collect_events(&stream).await;

    let recorded = seen.lock().unwrap().clone();
    for name in [
        "prepareRequest",
        "beforeToolCall",
        "afterToolCall",
        "finishTurn",
    ] {
        let entry = recorded
            .iter()
            .find(|(hook, _, _)| *hook == name)
            .unwrap_or_else(|| panic!("{name} 没有被调用"));
        assert!(entry.1, "{name} 没有收到 signal");
        assert!(!entry.2, "{name} 收到的 signal 不应处于已取消状态");
    }
}

/// 单个钩子的 signal 观测：(钩子名, 是否收到 signal, signal 是否已取消)。
type HookSignalRecord = (&'static str, bool, bool);
/// 四个钩子的 signal 观测集合。
type SeenHookSignals = Arc<Mutex<Vec<HookSignalRecord>>>;

fn record_hook_signal<T: Send + 'static>(
    seen: SeenHookSignals,
    name: &'static str,
    signal: Option<CancellationToken>,
) -> BoxFuture<'static, Option<T>> {
    Box::pin(async move {
        seen.lock().unwrap().push((
            name,
            signal.is_some(),
            signal.as_ref().is_some_and(CancellationToken::is_cancelled),
        ));
        None
    })
}

/// 回归（major）：钩子拿到的是这次 run 的**同一个**取消令牌；在 `prepareRequest` 里取消后，
/// `finishTurn` 能看到同一令牌已取消（TS 传的是同一个 AbortSignal）。
#[tokio::test]
async fn hook_signals_are_the_same_cancellation_token() {
    let token = CancellationToken::new();
    let mut config = base_config();
    config.prepare_request = Some(Arc::new(|_context, signal| {
        if let Some(signal) = signal {
            signal.cancel();
        }
        Box::pin(async { None })
    }));
    let finish_saw_cancelled = Arc::new(AtomicBool::new(false));
    let flag = finish_saw_cancelled.clone();
    config.finish_turn = Some(Arc::new(move |_turn, signal| {
        let flag = flag.clone();
        Box::pin(async move {
            flag.store(
                signal.as_ref().is_some_and(CancellationToken::is_cancelled),
                Ordering::SeqCst,
            );
            None
        })
    }));

    let stream = agent_loop(
        vec![create_user_message("go")],
        AgentContext {
            messages: Vec::new(),
            tools: None,
        },
        config,
        Some(token),
        Some(done_stream_fn(create_assistant_message(
            vec![text_block("done")],
            StopReason::Stop,
        ))),
    );
    let _ = collect_events(&stream).await;
    assert!(finish_saw_cancelled.load(Ordering::SeqCst));
}

/// 取 agent_end / 返回值里所有 assistant toolCall 的 `arguments`。
fn assistant_tool_call_arguments(messages: &[AgentMessage]) -> Vec<JsonValue> {
    messages
        .iter()
        .filter_map(AgentMessage::as_assistant)
        .flat_map(assistant_message_tool_call_arguments)
        .collect()
}

/// 取单条 assistant 消息里所有 toolCall 的 `arguments`。
fn assistant_message_tool_call_arguments(message: &AssistantMessage) -> Vec<JsonValue> {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            AssistantContentBlock::ToolCall(tool_call) => {
                Some(arguments_to_json_value(&tool_call.arguments))
            }
            _ => None,
        })
        .collect()
}

fn arguments_to_json_value(arguments: &JsonObject) -> JsonValue {
    serde_json::to_value(arguments).unwrap_or(JsonValue::Null)
}

/// 回归（major）：`prepareArguments` 对参数的**就地改写**（返回同一对象）必须保留。
///
/// TS 的闭包收到 `toolCall.arguments` 的同一个对象引用：`args.value = ...; return args` 之后，
/// `tool_execution_update.args`、`afterToolCall.toolCall.arguments` 与 `agent_end` 里的
/// assistant 消息都携带改写后的值；只有准备之前发出的 `tool_execution_start.args` 是原始值。
/// TS: agent-loop.ts:704-716、629-650；edit 工具 packages/coding-agent/src/core/tools/edit.ts:103-134。
#[tokio::test]
async fn prepare_arguments_in_place_mutation_is_visible_in_updates_hooks_and_messages() {
    let executed: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_for_tool = executed.clone();
    let mut tool = agent_tool(
        "prep",
        echo_schema(),
        Arc::new(move |_id, params, _signal, on_update| {
            let executed = executed_for_tool.clone();
            Box::pin(async move {
                if let Some(on_update) = on_update {
                    on_update(AgentToolResult {
                        content: vec![UserContentBlock::Text(text("partial"))],
                        details: serde_json::json!({}),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                    });
                }
                executed.lock().unwrap().push(params["value"].clone());
                Ok(result_with_text("ok"))
            })
        }),
    );
    // TS: `args.value = "x-mutated"; return args;`（返回同一对象 → Rust 返回 None）。
    tool.prepare_arguments = Some(Arc::new(|args: &mut JsonValue| {
        if args.get("value").and_then(JsonValue::as_str) == Some("x") {
            args["value"] = serde_json::json!("x-mutated");
        }
        None
    }));

    // (ctx.toolCall.arguments, ctx.args, ctx.assistantMessage 里的 toolCall.arguments)
    let seen: Arc<Mutex<Vec<(JsonValue, JsonValue, JsonValue)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_for_hook = seen.clone();
    let mut config = base_config();
    config.tool_execution = Some(ToolExecutionMode::Parallel);
    config.after_tool_call = Some(Arc::new(move |context, _signal| {
        let seen = seen_for_hook.clone();
        Box::pin(async move {
            let message_arguments =
                assistant_message_tool_call_arguments(&context.assistant_message);
            seen.lock().unwrap().push((
                arguments_to_json_value(&context.tool_call.arguments),
                context.args,
                message_arguments
                    .first()
                    .cloned()
                    .unwrap_or(JsonValue::Null),
            ));
            None
        })
    }));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        if calls_for_fn.fetch_add(1, Ordering::SeqCst) == 0 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "prep",
                    serde_json::json!({ "value": "x" }),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("go")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;
    let messages = await_result(&stream).await;

    // `tool_execution_start` 在准备之前：原始参数。
    let start_args = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolExecutionStart { args, .. } => Some(args.clone()),
            _ => None,
        })
        .expect("tool_execution_start");
    assert_eq!(start_args, serde_json::json!({ "value": "x" }));

    // `tool_execution_update` 在准备之后：就地改写后的参数。
    let update_args = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolExecutionUpdate { args, .. } => Some(args.clone()),
            _ => None,
        })
        .expect("tool_execution_update");
    assert_eq!(update_args, serde_json::json!({ "value": "x-mutated" }));

    // 钩子看到的是同一个（改写后的）参数对象，assistantMessage 里的内容块同样已改写。
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(
            serde_json::json!({ "value": "x-mutated" }),
            serde_json::json!({ "value": "x-mutated" }),
            serde_json::json!({ "value": "x-mutated" })
        )]
    );
    // execute 收到改写后的参数（不重新校验）。
    assert_eq!(
        *executed.lock().unwrap(),
        vec![serde_json::json!("x-mutated")]
    );
    // agent_end / 返回值里的 assistant 消息保留同一份改写后的参数。
    assert_eq!(
        assistant_tool_call_arguments(&messages),
        vec![serde_json::json!({ "value": "x-mutated" })]
    );
}

/// 回归（major）：`prepareArguments` 返回**新对象**时，原始工具调用保持模型给出的参数。
///
/// TS 的 `prepareToolCall` 保留 `toolCall`（原始对象）用于 `afterToolCall`、`runToolCall`
/// 的返回值与 assistant 消息；`{...toolCall, arguments: preparedArguments}` 只用于校验与
/// 执行（agent-loop.ts:704-716、744-745、776、885、915）。
#[tokio::test]
async fn prepare_arguments_replacement_object_keeps_the_raw_tool_call_in_hooks_and_messages() {
    let executed: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_for_tool = executed.clone();
    let mut tool = agent_tool(
        "prep",
        serde_json::json!({
            "type": "object",
            "properties": {
                "value": { "type": "string" },
                "extra": { "type": "string" }
            },
            "required": ["value", "extra"]
        }),
        Arc::new(move |_id, params, _signal, _on_update| {
            let executed = executed_for_tool.clone();
            Box::pin(async move {
                executed.lock().unwrap().push(params.clone());
                Ok(result_with_text("ok"))
            })
        }),
    );
    // 不改写入参，返回一个全新的对象。
    tool.prepare_arguments = Some(Arc::new(|_args: &mut JsonValue| {
        Some(serde_json::json!({ "value": "x", "extra": "prepared" }))
    }));

    let seen: Arc<Mutex<Vec<(JsonValue, JsonValue)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_for_hook = seen.clone();
    let mut config = base_config();
    config.after_tool_call = Some(Arc::new(move |context, _signal| {
        let seen = seen_for_hook.clone();
        Box::pin(async move {
            seen.lock().unwrap().push((
                arguments_to_json_value(&context.tool_call.arguments),
                context.args,
            ));
            None
        })
    }));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        if calls_for_fn.fetch_add(1, Ordering::SeqCst) == 0 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "prep",
                    serde_json::json!({ "value": "x" }),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("go")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;
    let messages = await_result(&stream).await;

    // tool_execution_update 的 args 是原始对象（闭包没有就地改写它）。
    let update_args = events.iter().find_map(|event| match event {
        AgentEvent::ToolExecutionUpdate { args, .. } => Some(args.clone()),
        _ => None,
    });
    assert!(update_args.is_none(), "该工具没有触发 update");

    // afterToolCall：toolCall 是原始调用，args 是改写后的参数。
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(
            serde_json::json!({ "value": "x" }),
            serde_json::json!({ "value": "x", "extra": "prepared" })
        )]
    );
    // execute 收到改写后的参数。
    assert_eq!(
        *executed.lock().unwrap(),
        vec![serde_json::json!({ "value": "x", "extra": "prepared" })]
    );
    // assistant 消息保留原始参数。
    assert_eq!(
        assistant_tool_call_arguments(&messages),
        vec![serde_json::json!({ "value": "x" })]
    );
}

/// 回归（major）：`prepareArguments` 既就地改写、又返回**新对象**时（edit 工具的 legacy
/// 分支：`args.edits = parsed` 后 `return {...rest, edits}`），两边的效果都要保留：
/// 原始调用/消息/update 用就地改写后的值，校验与执行用返回的新对象。
#[tokio::test]
async fn prepare_arguments_in_place_mutation_and_replacement_object_are_both_visible() {
    let executed: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_for_tool = executed.clone();
    let mut tool = agent_tool(
        "prep",
        serde_json::json!({
            "type": "object",
            "properties": {
                "value": { "type": "string" },
                "extra": { "type": "string" }
            },
            "required": ["value", "extra"]
        }),
        Arc::new(move |_id, params, _signal, _on_update| {
            let executed = executed_for_tool.clone();
            Box::pin(async move {
                executed.lock().unwrap().push(params.clone());
                Ok(result_with_text("ok"))
            })
        }),
    );
    // TS: `args.value = "x-mutated"; return { value: "x-prepared", extra: "prepared" };`
    tool.prepare_arguments = Some(Arc::new(|args: &mut JsonValue| {
        args["value"] = serde_json::json!("x-mutated");
        Some(serde_json::json!({ "value": "x-prepared", "extra": "prepared" }))
    }));

    let seen: Arc<Mutex<Vec<(JsonValue, JsonValue)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_for_hook = seen.clone();
    let mut config = base_config();
    config.after_tool_call = Some(Arc::new(move |context, _signal| {
        let seen = seen_for_hook.clone();
        Box::pin(async move {
            seen.lock().unwrap().push((
                arguments_to_json_value(&context.tool_call.arguments),
                context.args,
            ));
            None
        })
    }));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        if calls_for_fn.fetch_add(1, Ordering::SeqCst) == 0 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "prep",
                    serde_json::json!({ "value": "x" }),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("go")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _events = collect_events(&stream).await;
    let messages = await_result(&stream).await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec![(
            serde_json::json!({ "value": "x-mutated" }),
            serde_json::json!({ "value": "x-prepared", "extra": "prepared" })
        )]
    );
    assert_eq!(
        *executed.lock().unwrap(),
        vec![serde_json::json!({ "value": "x-prepared", "extra": "prepared" })]
    );
    assert_eq!(
        assistant_tool_call_arguments(&messages),
        vec![serde_json::json!({ "value": "x-mutated" })]
    );
}

/// 回归（major）：`runToolCall` 的 `outcome.toolCall` 是**原始**调用。
///
/// TS: `finalizeExecutedToolCall` 返回 `toolCall: prepared.toolCall`（原始对象），
/// `afterToolCall` 的 `ctx.toolCall` 同理；只有 `ctx.args` 是 `prepareArguments` 改写后的值
/// （agent-loop.ts:885、915）。
#[tokio::test]
async fn run_tool_call_keeps_the_raw_tool_call_when_prepare_arguments_returns_a_new_object() {
    let mut tool = agent_tool(
        "echo",
        serde_json::json!({
            "type": "object",
            "properties": {
                "value": { "type": "string" },
                "extra": { "type": "string" }
            },
            "required": ["value", "extra"]
        }),
        Arc::new(|_id, params, _signal, _on_update| {
            Box::pin(async move {
                assert_eq!(
                    params,
                    serde_json::json!({ "value": "x", "extra": "prepared" })
                );
                Ok(result_with_text("ok"))
            })
        }),
    );
    tool.prepare_arguments = Some(Arc::new(|_args: &mut JsonValue| {
        Some(serde_json::json!({ "value": "x", "extra": "prepared" }))
    }));

    let seen: Arc<Mutex<Vec<(JsonValue, JsonValue)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_for_hook = seen.clone();
    let assistant_message = create_assistant_message(
        vec![AssistantContentBlock::ToolCall(tool_call(
            "t1",
            "echo",
            serde_json::json!({ "value": "x" }),
        ))],
        StopReason::ToolUse,
    );
    let outcome = run_tool_call(
        tool_call("t1", "echo", serde_json::json!({ "value": "x" })),
        RunToolCallOptions {
            tools: vec![tool],
            assistant_message,
            context: AgentContext {
                messages: Vec::new(),
                tools: None,
            },
            signal: None,
            on_update: None,
            before_tool_call: None,
            after_tool_call: Some(Arc::new(move |context, _signal| {
                let seen = seen_for_hook.clone();
                Box::pin(async move {
                    seen.lock().unwrap().push((
                        arguments_to_json_value(&context.tool_call.arguments),
                        context.args,
                    ));
                    None
                })
            })),
        },
    )
    .await;

    assert_eq!(
        arguments_to_json_value(&outcome.tool_call.arguments),
        serde_json::json!({ "value": "x" })
    );
    assert!(outcome.duration_ms.is_some());
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(
            serde_json::json!({ "value": "x" }),
            serde_json::json!({ "value": "x", "extra": "prepared" })
        )]
    );
}

/// 回归（minor）：`prepareArguments` 返回非对象时交给校验器，产生稳定格式的
/// 校验错误结果，而不是静默用原始参数执行。
///
/// TS: `{...toolCall, arguments: preparedArguments}` → `validateToolArguments` 抛
/// `Validation failed for tool "X":\n  - root: must be object\n\nReceived arguments:\n<pretty>`
/// （agent-loop.ts:704-716；packages/ai/src/utils/validation.ts:317-352）。
#[tokio::test]
async fn prepare_arguments_non_object_result_is_reported_as_a_validation_error() {
    let executed = Arc::new(AtomicUsize::new(0));
    let executed_for_tool = executed.clone();
    let mut tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, _params, _signal, _on_update| {
            let executed = executed_for_tool.clone();
            Box::pin(async move {
                executed.fetch_add(1, Ordering::SeqCst);
                Ok(result_with_text("ok"))
            })
        }),
    );
    tool.prepare_arguments = Some(Arc::new(|_args: &mut JsonValue| {
        Some(serde_json::json!("garbage"))
    }));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        if calls_for_fn.fetch_add(1, Ordering::SeqCst) == 0 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "echo",
                    serde_json::json!({ "value": "raw" }),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("go")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;

    let (is_error, result) = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolExecutionEnd {
                is_error, result, ..
            } => Some((*is_error, result.clone())),
            _ => None,
        })
        .expect("tool_execution_end");
    assert!(is_error);
    assert_eq!(
        result["content"][0]["text"],
        serde_json::json!(
            "Validation failed for tool \"echo\":\n  - root: must be object\n\nReceived arguments:\n\"garbage\""
        )
    );
    assert_eq!(executed.load(Ordering::SeqCst), 0, "工具不应执行");
}

/// 回归（minor）：`agent_end` 一入流，`done` 与 `result()` 就提交（TS `createAgentStream`
/// 以 `event.type === "agent_end"` 为完成条件）。
///
/// TS: agent-loop.ts:162-167；packages/ai/src/utils/event-stream.ts:42-55。
#[tokio::test]
async fn agent_event_stream_completes_when_agent_end_is_pushed() {
    let stream = AgentEventStream::new();
    let message = create_user_message("go");
    stream.push(AgentEvent::AgentStart);
    stream.push(AgentEvent::AgentEnd {
        messages: vec![message.clone()],
    });
    // 没有调用 `end()`：`result()` 仍应立即就绪。
    let result = tokio::time::timeout(Duration::from_secs(5), stream.result())
        .await
        .expect("agent_end 应提交 result()");
    assert_eq!(result.len(), 1);

    // `done` 之后的 push 被静默忽略；已有事件仍按序消费，随后 recv 返回 None。
    stream.push(AgentEvent::TurnStart);
    let kinds: Vec<&str> = vec![
        stream.recv().await.expect("agent_start").kind(),
        stream.recv().await.expect("agent_end").kind(),
    ];
    assert_eq!(kinds, vec!["agent_start", "agent_end"]);
    assert!(stream.recv().await.is_none());
}

/// 回归（minor）：pending 消息里 `role === "system"` 的自定义 AgentMessage 被当作
/// 「待合并的 system 消息」：工具差量用 `withToolChanges` 写进它本身，而不是另插一条标准
/// system 消息（TS 只看 role，agent-loop.ts:342-372）。
#[tokio::test]
async fn declare_tool_changes_merges_tool_deltas_into_a_custom_system_message() {
    let tool = agent_tool(
        "noop",
        empty_schema(),
        Arc::new(|_id, _params, _signal, _on_update| {
            Box::pin(async move { Ok(result_with_text("ok")) })
        }),
    );
    let custom = custom_message_from_json(serde_json::json!({
        "role": "system",
        "content": "custom prompt",
        "timestamp": 5,
        "extra": 1
    }));
    let stream_fn = done_stream_fn(create_assistant_message(
        vec![text_block("done")],
        StopReason::Stop,
    ));

    let stream = agent_loop(
        vec![custom, create_user_message("go")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;
    let messages = await_result(&stream).await;

    // 只有初始两条消息与 assistant 的 message_start/message_end；没有额外插入 system 消息。
    let message_events = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::MessageStart { .. } | AgentEvent::MessageEnd { .. }
            )
        })
        .count();
    assert_eq!(message_events, 6);
    assert_eq!(roles(&messages), vec!["system", "user", "assistant"]);

    let AgentMessage::Custom(merged) = &messages[0] else {
        panic!("expected the custom message to stay custom");
    };
    // TS 的解构 + 展开顺序：其余键保持原序，toolsAdded 追加在末尾。
    assert_eq!(
        merged.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["role", "content", "timestamp", "extra", "toolsAdded"]
    );
    assert_eq!(
        merged.get("content").and_then(JsonValue::as_str),
        Some("custom prompt")
    );
    assert_eq!(merged.get("timestamp"), Some(&serde_json::json!(5)));
    assert_eq!(merged.get("extra"), Some(&serde_json::json!(1)));
    let tools_added = merged
        .get("toolsAdded")
        .and_then(JsonValue::as_array)
        .expect("toolsAdded");
    assert_eq!(tools_added.len(), 1);
    assert_eq!(tools_added[0]["name"], "noop");
    assert!(!merged.contains_key("toolsRemoved"));
}

/// 回归（minor）：已提交在 `context.messages` 里的自定义 system 消息同样参与
/// `getCurrentTools` 回放：它声明的工具与可执行集合一致时不再插入新的 system 消息。
#[tokio::test]
async fn declare_tool_changes_replays_tools_from_committed_custom_system_messages() {
    let tool = agent_tool(
        "noop",
        empty_schema(),
        Arc::new(|_id, _params, _signal, _on_update| {
            Box::pin(async move { Ok(result_with_text("ok")) })
        }),
    );
    let custom = custom_message_from_json(serde_json::json!({
        "role": "system",
        "content": "custom prompt",
        "timestamp": 5,
        "toolsAdded": [{
            "name": "noop",
            "description": "noop tool",
            "parameters": { "type": "object", "properties": {} }
        }]
    }));
    let stream_fn = done_stream_fn(create_assistant_message(
        vec![text_block("done")],
        StopReason::Stop,
    ));

    let stream = agent_loop(
        vec![create_user_message("go")],
        AgentContext {
            messages: vec![custom],
            tools: Some(vec![tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;
    let messages = await_result(&stream).await;

    // 工具声明没有变化：不插入新的 system 消息（只有 user 一条初始消息）。
    assert_eq!(roles(&messages), vec!["user", "assistant"]);
    assert_eq!(
        event_kinds(&events),
        vec![
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
}

/// 回归（minor）：TS 的 `!pending.toolsAdded?.length` 是 JS `.length` 真值判断，
/// 不是「非数组即空」。
///
/// 字符串 `"x"`、`{length:1}`、非空数组这类值的 `.length` 为真，TS 走
/// `withToolChanges(pending, changes)` 重建路径，把伪字段丢掉；空数组、空串、`null`
/// 的 `.length` 为假（或不存在），TS 原样返回调用方对象（保留该字段）。
///
/// 注：保留下来的字段随后会被 TS 的 `getCurrentTools` 用 `for ... of` 迭代，数字/布尔
/// 等非可迭代值会抛未捕获的 TypeError；Rust 的回放投影忽略非数组值。这类值超出 TS
/// 类型（`toolsAdded: Tool[]`），本测试不锁定。
#[tokio::test]
async fn declare_tool_changes_uses_js_length_truthiness_for_custom_tool_lists() {
    for (field, value, dropped) in [
        ("toolsAdded", serde_json::json!("x"), true),
        ("toolsAdded", serde_json::json!({ "length": 1 }), true),
        ("toolsAdded", serde_json::json!([1]), true),
        ("toolsAdded", serde_json::json!([]), false),
        ("toolsAdded", serde_json::json!(""), false),
        ("toolsAdded", serde_json::json!(null), false),
        ("toolsRemoved", serde_json::json!("x"), true),
        ("toolsRemoved", serde_json::json!({ "length": 1 }), true),
        ("toolsRemoved", serde_json::json!([]), false),
        ("toolsRemoved", serde_json::json!(""), false),
    ] {
        let mut object = JsonObject::new();
        object.insert("role".to_string(), serde_json::json!("system"));
        object.insert("content".to_string(), serde_json::json!("custom prompt"));
        object.insert("timestamp".to_string(), serde_json::json!(5));
        object.insert(field.to_string(), value.clone());
        let custom = AgentMessage::Custom(Box::new(object));
        let stream_fn = done_stream_fn(create_assistant_message(
            vec![text_block("done")],
            StopReason::Stop,
        ));

        // 可执行工具集为空：工具差量没有变化，是否重建只取决于伪字段的 `.length`。
        let stream = agent_loop(
            vec![custom, create_user_message("go")],
            AgentContext {
                messages: Vec::new(),
                tools: None,
            },
            base_config(),
            None,
            Some(stream_fn),
        );
        let events = collect_events(&stream).await;
        let messages = await_result(&stream).await;

        let AgentMessage::Custom(rendered) = &messages[0] else {
            panic!("expected the custom message to stay custom");
        };
        let keys = rendered.keys().map(String::as_str).collect::<Vec<_>>();
        if dropped {
            assert_eq!(
                keys,
                vec!["role", "content", "timestamp"],
                "{field}={value} must be dropped by withToolChanges"
            );
            assert!(
                !rendered.contains_key(field),
                "{field}={value} must not survive the rebuild"
            );
        } else {
            assert_eq!(
                keys,
                vec!["role", "content", "timestamp", field],
                "{field}={value} must be kept by returning pendingMessages"
            );
            assert_eq!(rendered.get(field), Some(&value));
        }

        // `message_start` 里回放的就是重建/保留后的同一条消息。
        let start = events.iter().find_map(|event| match event {
            AgentEvent::MessageStart { message } => Some(message.clone()),
            _ => None,
        });
        assert_eq!(start.as_ref(), Some(&messages[0]));
    }
}

// ---------------------------------------------------------------------------
// 追加覆盖：spec §3.2 里其余可经公开 API 触达的逐字错误消息。
// ---------------------------------------------------------------------------

/// spec §3.2 / TS `agentLoopContinue()`：末条是 assistant 时同步抛错。
///
/// TS 只在 `agent-loop.test.ts` 里测了「没有消息」分支；这条 message 属于
/// spec §3.2 的验收项，单独锁住错误文本。
#[tokio::test]
async fn agent_loop_continue_rejects_an_assistant_tail() {
    let context = AgentContext {
        messages: vec![AgentMessage::Standard(Message::Assistant(
            create_assistant_message(vec![text_block("hi")], StopReason::Stop),
        ))],
        tools: Some(Vec::new()),
    };
    let stream_fn = stream_fn_with(
        |_model, _context, _options| -> AssistantMessageEventStream {
            panic!("Unexpected stream call")
        },
    );
    match agent_loop_continue(context, base_config(), None, Some(stream_fn)) {
        Err(error) => assert_eq!(
            error.to_string(),
            "Cannot continue from message role: assistant"
        ),
        Ok(_) => panic!("agentLoopContinue should have thrown"),
    }
}

/// spec §3.2：`beforeToolCall` 返回 `block` 但没有 reason（或 reason 为空串）时，
/// 使用默认文案 `Tool execution was blocked`（TS: `beforeResult.reason || "..."`）。
#[tokio::test]
async fn blocked_tool_calls_without_a_reason_use_the_default_message() {
    for reason in [None, Some(String::new())] {
        let executed = Arc::new(AtomicBool::new(false));
        let executed_for_tool = executed.clone();
        let tool = agent_tool(
            "echo",
            echo_schema(),
            Arc::new(move |_id, _params, _signal, _on_update| {
                let executed = executed_for_tool.clone();
                Box::pin(async move {
                    executed.store(true, Ordering::SeqCst);
                    Ok(result_with_text("should not run"))
                })
            }),
        );
        let outcome = run_tool_call(
            tool_call("blocked", "echo", serde_json::json!({ "value": "x" })),
            RunToolCallOptions {
                tools: vec![tool],
                assistant_message: create_assistant_message(Vec::new(), StopReason::Stop),
                context: AgentContext {
                    messages: Vec::new(),
                    tools: None,
                },
                signal: None,
                on_update: None,
                before_tool_call: Some(Arc::new(move |_context, _signal| {
                    let reason = reason.clone();
                    Box::pin(async move {
                        Some(BeforeToolCallResult {
                            block: Some(true),
                            reason,
                            terminate: None,
                        })
                    })
                })),
                after_tool_call: None,
            },
        )
        .await;

        assert!(!executed.load(Ordering::SeqCst));
        assert!(outcome.is_error);
        assert_eq!(
            outcome.result.content,
            vec![UserContentBlock::Text(text("Tool execution was blocked"))]
        );
        assert_eq!(outcome.duration_ms, None);
    }
}

/// spec §3.2：`runToolCall` 收到已取消的 signal 时不执行工具，返回
/// `Operation aborted` 的错误结果（TS `prepareToolCall` 的最后一个 abort 检查）。
#[tokio::test]
async fn run_tool_call_with_an_aborted_signal_reports_operation_aborted() {
    let executed = Arc::new(AtomicBool::new(false));
    let executed_for_tool = executed.clone();
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, _params, _signal, _on_update| {
            let executed = executed_for_tool.clone();
            Box::pin(async move {
                executed.store(true, Ordering::SeqCst);
                Ok(result_with_text("should not run"))
            })
        }),
    );
    let signal = CancellationToken::new();
    signal.cancel();

    let outcome = run_tool_call(
        tool_call("aborted", "echo", serde_json::json!({ "value": "x" })),
        RunToolCallOptions {
            tools: vec![tool],
            assistant_message: create_assistant_message(Vec::new(), StopReason::Stop),
            context: AgentContext {
                messages: Vec::new(),
                tools: None,
            },
            signal: Some(signal),
            on_update: None,
            before_tool_call: None,
            after_tool_call: None,
        },
    )
    .await;

    assert!(!executed.load(Ordering::SeqCst));
    assert!(outcome.is_error);
    assert_eq!(
        outcome.result.content,
        vec![UserContentBlock::Text(text("Operation aborted"))]
    );
    assert_eq!(outcome.duration_ms, None);
}

/// spec §3.2：`beforeToolCall` 期间取消 signal → 该调用立即以 `Operation aborted`
/// 收尾（TS `prepareToolCall` 在钩子返回后的 abort 检查），工具不再执行。
#[tokio::test]
async fn aborting_during_before_tool_call_short_circuits_the_tool() {
    let executed = Arc::new(AtomicBool::new(false));
    let executed_for_tool = executed.clone();
    let tool = agent_tool(
        "echo",
        echo_schema(),
        Arc::new(move |_id, _params, _signal, _on_update| {
            let executed = executed_for_tool.clone();
            Box::pin(async move {
                executed.store(true, Ordering::SeqCst);
                Ok(result_with_text("should not run"))
            })
        }),
    );
    let mut config = base_config();
    config.before_tool_call = Some(Arc::new(|_context, signal| {
        Box::pin(async move {
            if let Some(signal) = signal {
                signal.cancel();
            }
            None
        })
    }));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        if calls_for_fn.fetch_add(1, Ordering::SeqCst) == 0 {
            done_stream(create_assistant_message(
                vec![AssistantContentBlock::ToolCall(tool_call(
                    "tool-1",
                    "echo",
                    serde_json::json!({ "value": "hello" }),
                ))],
                StopReason::ToolUse,
            ))
        } else {
            done_stream(create_assistant_message(
                vec![text_block("done")],
                StopReason::Stop,
            ))
        }
    });

    let stream = agent_loop(
        vec![create_user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        config,
        Some(CancellationToken::new()),
        Some(stream_fn),
    );
    let events = collect_events(&stream).await;

    assert!(!executed.load(Ordering::SeqCst));
    let (is_error, result) = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolExecutionEnd {
                is_error, result, ..
            } => Some((*is_error, result.clone())),
            _ => None,
        })
        .expect("tool_execution_end");
    assert!(is_error);
    assert_eq!(result["content"][0]["text"], "Operation aborted");
}
