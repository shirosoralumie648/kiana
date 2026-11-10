//! `packages/agent/test/agent.test.ts` 的 Rust 移植（36 个 `it` 声明 / 41 次运行）。
//!
//! 每个测试的文档注释里保留 TS 的 `it` 标题原文（Rust 函数名 snake_case）。`it.each` 的
//! 两个用例展开成两个 Rust 测试（名字里带上用例参数）。
//!
//! 形态差异（见 `src/agent.rs` 模块文档）：`agent.state` 是快照，等价的写入口是 `set_*` /
//! `push_message`；`pendingToolCalls` 只有值语义；`continue()` 在 Rust 里叫 `continue_()`。

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kiana_agent_core::agent::{
    Agent, AgentInitialState, AgentListener, AgentOptions, AgentPrepareNextTurnFn,
    AgentPrepareNextTurnWithContextFn, Subscription,
};
use kiana_agent_core::stream_fn::{clear_default_stream_fn, set_default_stream_fn};
use kiana_agent_core::types::{
    AgentEvent, AgentMessage, AgentTool, AgentToolError, AgentToolExecute, AgentToolResult,
    AgentToolUpdateCallback, AgentTurnContext, AgentTurnDecision, BoxFuture, FinishTurnFn,
    JsonObject, JsonValue, QueueMode, StreamFn, ThinkingLevel,
};
use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole,
    ClientAttribution, ClientIdentity, DoneReason, ErrorReason, Message, Model,
    SimpleStreamOptions, StopReason, SystemContent, SystemMessage, SystemRole, TextContent,
    TextKind, Tool, ToolCall, ToolCallKind, ToolReference, ToolResultMessage, ToolResultRole,
    TranscriptContext, Usage, UsageCost, UserAgentMode, UserContent, UserContentBlock, UserMessage,
    UserRole,
};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// 辅助（对应 TS 测试文件顶部的 createUserMessage / createAssistantMessage / ...）
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

fn user_text_block(text: impl Into<String>) -> UserContentBlock {
    UserContentBlock::Text(self::text(text))
}

/// `createUserMessage(text)`：`{ role: "user", content: text, timestamp: Date.now() }`。
fn create_user_message(text: impl Into<String>) -> AgentMessage {
    AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp: now_ms(),
    }))
}

/// `createUserMessage` 的数组内容形态（`content: [{type:"text",text}]`）。
fn create_user_message_blocks(text: impl Into<String>) -> AgentMessage {
    AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(vec![user_text_block(text)]),
        timestamp: now_ms(),
    }))
}

fn create_assistant_message(text: impl Into<String>) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![text_block(text)],
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        model: "mock".to_string(),
        usage: create_usage(),
        stop_reason: StopReason::Stop,
        timestamp: now_ms(),
        ..Default::default()
    }
}

/// `createAssistantToolUseMessage(content)`。
fn create_assistant_tool_use_message(content: Vec<ToolCall>) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: content
            .into_iter()
            .map(AssistantContentBlock::ToolCall)
            .collect(),
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        model: "mock".to_string(),
        usage: create_usage(),
        stop_reason: StopReason::ToolUse,
        timestamp: now_ms(),
        ..Default::default()
    }
}

fn empty_schema() -> JsonValue {
    serde_json::json!({ "type": "object", "properties": {} })
}

fn tool_call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: id.to_string(),
        name: name.to_string(),
        arguments: JsonObject::new(),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
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
    done_stream_with_reason(message, DoneReason::Stop)
}

fn error_stream(message: AssistantMessage, reason: ErrorReason) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let _ = stream.push(AssistantMessageEvent::Error {
        reason,
        error: Box::new(message),
    });
    stream
}

/// 构造 `StreamFn`：`f` 同步返回一个事件流。
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

/// `unusedStreamFunction`：测试里不应该被调用。
fn unused_stream_function() -> StreamFn {
    Arc::new(|_model, _context, _options| {
        Box::pin(async { Err(AiError::new(AiErrorKind::Other, "Unexpected stream call")) })
    })
}

/// `createDeferred()`：一次性的 barrier（resolve 之后再 wait 也立即返回）。
#[derive(Clone)]
struct Deferred {
    sender: tokio::sync::watch::Sender<bool>,
    keepalive: tokio::sync::watch::Receiver<bool>,
}

impl Deferred {
    fn new() -> Self {
        let (sender, keepalive) = tokio::sync::watch::channel(false);
        Deferred { sender, keepalive }
    }

    fn resolve(&self) {
        let _ = self.sender.send(true);
    }

    async fn wait(&self) {
        let mut receiver = self.keepalive.clone();
        let _ = receiver.wait_for(|done| *done).await;
    }
}

fn listener<F>(f: F) -> AgentListener
where
    F: for<'a> Fn(AgentEvent, &'a CancellationToken) -> BoxFuture<'a, ()> + Send + Sync + 'static,
{
    Arc::new(f)
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

fn agent_tool_with(name: &str, description: &str, execute: ExecuteFn) -> AgentTool {
    AgentTool {
        name: name.to_string(),
        label: name.to_string(),
        description: description.to_string(),
        parameters: empty_schema(),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(TestToolExecute { execute }),
    }
}

/// `createTool(name)`：返回文本即工具名、`details: {}`。
fn create_tool(name: &str) -> AgentTool {
    let name = name.to_string();
    agent_tool_with(name.as_str(), &format!("{name} tool"), {
        let name = name.clone();
        Arc::new(move |_id, _params, _signal, _on_update| {
            let name = name.clone();
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![user_text_block(name)],
                    details: serde_json::json!({}),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        })
    })
}

fn assistant_of(message: &AgentMessage) -> Option<&AssistantMessage> {
    message.as_assistant()
}

fn system_of(message: &AgentMessage) -> Option<&SystemMessage> {
    match message {
        AgentMessage::Standard(Message::System(system)) => Some(system),
        _ => None,
    }
}

fn custom_object(message: &AgentMessage) -> Option<&JsonObject> {
    match message {
        AgentMessage::Custom(object) => Some(object),
        _ => None,
    }
}

/// 自定义 `role: "system"` 消息（coding-agent 的 sections / 工具变更载体）。
fn custom_system_message(
    content: &str,
    sections: Option<Vec<(&str, &str)>>,
    tools_added: Option<Vec<Tool>>,
    tools_removed: Option<Vec<ToolReference>>,
    timestamp: i64,
) -> AgentMessage {
    let mut object = JsonObject::new();
    object.insert("role".to_string(), serde_json::json!("system"));
    object.insert("content".to_string(), serde_json::json!(content));
    if let Some(sections) = sections {
        let sections: serde_json::Map<String, JsonValue> = sections
            .into_iter()
            .map(|(name, value)| (name.to_string(), serde_json::json!(value)))
            .collect();
        object.insert("sections".to_string(), JsonValue::Object(sections));
    }
    if let Some(tools_added) = tools_added {
        object.insert(
            "toolsAdded".to_string(),
            serde_json::to_value(tools_added).unwrap(),
        );
    }
    if let Some(tools_removed) = tools_removed {
        object.insert(
            "toolsRemoved".to_string(),
            serde_json::to_value(tools_removed).unwrap(),
        );
    }
    object.insert("timestamp".to_string(), serde_json::json!(timestamp));
    AgentMessage::Custom(Box::new(object))
}

fn tool_declaration(name: &str) -> Tool {
    Tool {
        name: name.to_string(),
        description: format!("{name} tool"),
        parameters: empty_schema(),
        constrained_sampling: None,
    }
}

/// `context.messages` 里 system 消息的 `+toolsAdded` / `-toolsRemoved` 标记。
fn tool_change_markers(context: &TranscriptContext) -> Vec<String> {
    let mut markers = Vec::new();
    for message in &context.as_context().messages {
        if let Message::System(system) = message {
            let joined = |names: Vec<&str>| names.join(",");
            let added = system
                .tools_added
                .as_ref()
                .map(|tools| joined(tools.iter().map(|tool| tool.name.as_str()).collect()));
            let removed = system
                .tools_removed
                .as_ref()
                .map(|tools| joined(tools.iter().map(|tool| tool.name.as_str()).collect()));
            markers.push(format!("+{}", added.unwrap_or_default()));
            markers.push(format!("-{}", removed.unwrap_or_default()));
        }
    }
    markers
}

/// `context.messages` 里字符串内容的 user 文本（TS 的 `typeof message.content === "string"`）。
fn user_texts(context: &TranscriptContext) -> Vec<String> {
    context
        .as_context()
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => match &user.content {
                UserContent::Text(text) => Some(text.clone()),
                UserContent::Blocks(_) => None,
            },
            _ => None,
        })
        .collect()
}

fn user_message_contains(message: &AgentMessage, needle: &str) -> bool {
    let AgentMessage::Standard(Message::User(user)) = message else {
        return false;
    };
    match &user.content {
        UserContent::Text(text) => text == needle,
        UserContent::Blocks(blocks) => blocks.iter().any(|block| match block {
            UserContentBlock::Text(text) => text.text == needle,
            UserContentBlock::Image(_) => false,
        }),
    }
}

fn roles(messages: &[AgentMessage]) -> Vec<String> {
    messages
        .iter()
        .map(|message| message.role().to_string())
        .collect()
}

type RequestLog = Arc<Mutex<Vec<Vec<String>>>>;

fn record_requests() -> RequestLog {
    Arc::new(Mutex::new(Vec::new()))
}

// ---------------------------------------------------------------------------
// 1. uses the configured default when a legacy caller omits streamFn
// ---------------------------------------------------------------------------

/// 全局默认流函数是进程级单例；涉及它的测试用 tokio Mutex 串行化。
static DEFAULT_STREAM_FN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_the_configured_default_when_a_legacy_caller_omits_stream_fn() {
    // TS: it("uses the configured default when a legacy caller omits streamFn")
    let _guard = DEFAULT_STREAM_FN_LOCK.lock().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = Arc::clone(&calls);
    set_default_stream_fn(stream_fn_with(move |_model, _context, _options| {
        calls_for_fn.fetch_add(1, Ordering::SeqCst);
        done_stream(create_assistant_message("fallback"))
    }));

    let outcome: Result<(), String> = async {
        // TS: `Reflect.construct(Agent, [{}])` —— 旧调用方省略 options/streamFn。
        let agent = Agent::new(AgentOptions::default()).map_err(|error| error.to_string())?;
        agent
            .prompt_text("Hello")
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }
    .await;
    clear_default_stream_fn();
    outcome.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// 构造期急切求值：未配置默认流函数时 `new Agent` 立即失败（TS 的 `getDefaultStreamFn()`
/// 在构造函数里 throw）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_construction_when_no_default_stream_fn_is_configured() {
    let _guard = DEFAULT_STREAM_FN_LOCK.lock().await;
    clear_default_stream_fn();
    let error = Agent::new(AgentOptions::default())
        .err()
        .expect("must fail");
    assert_eq!(
        error.to_string(),
        "No default stream function configured. Pass streamFn explicitly or call setDefaultStreamFn()."
    );
}

// ---------------------------------------------------------------------------
// 2. should create an agent instance with default state
// ---------------------------------------------------------------------------

/// TS: it("should create an agent instance with default state")
#[tokio::test]
async fn should_create_an_agent_instance_with_default_state() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .unwrap();

    let state = agent.state();
    let model = state.model.as_ref().expect("model is defined");
    assert_eq!(model.id, "unknown");
    assert_eq!(model.name, "unknown");
    assert_eq!(model.api, "unknown");
    assert_eq!(model.provider, "unknown");
    assert_eq!(model.base_url, "");
    assert_eq!(model.reasoning, Some(false));
    assert!(model.input.is_empty());
    assert_eq!(model.context_window, Some(0));
    assert_eq!(model.max_tokens, Some(0));
    assert_eq!(state.thinking_level, ThinkingLevel::Off);
    assert!(state.tools.is_empty());
    assert!(state.messages.is_empty());
    assert!(!state.is_streaming);
    assert!(state.streaming_message.is_none());
    assert_eq!(state.pending_tool_calls, BTreeSet::new());
    assert!(state.error_message.is_none());
    assert_eq!(state.system_prompt, "");
}

// ---------------------------------------------------------------------------
// 3. should create an agent instance with custom initial state
// ---------------------------------------------------------------------------

/// TS: it("should create an agent instance with custom initial state")
#[tokio::test]
async fn should_create_an_agent_instance_with_custom_initial_state() {
    // 与 TS 相同：`getModel("openai", "gpt-4o-mini")`。
    let custom_model = kiana_ai::compat::get_model("openai", "gpt-4o-mini")
        .expect("gpt-4o-mini must be in the builtin catalog");
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are a helpful assistant.".to_string()),
            model: Some(custom_model.clone()),
            thinking_level: Some(ThinkingLevel::Low),
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap();

    let messages = agent.messages();
    assert_eq!(messages.len(), 1);
    let system = system_of(&messages[0]).expect("leading system message");
    assert_eq!(
        system.content,
        SystemContent::Text("You are a helpful assistant.".to_string())
    );
    assert_eq!(system.timestamp, 0);
    assert!(system.tools_added.is_none());
    assert_eq!(agent.model(), custom_model);
    assert_eq!(agent.thinking_level(), ThinkingLevel::Low);
}

// ---------------------------------------------------------------------------
// 4. converts initial prompt and tools into transcript state
// ---------------------------------------------------------------------------

/// TS: it("converts initial prompt and tools into transcript state")
#[tokio::test]
async fn converts_initial_prompt_and_tools_into_transcript_state() {
    let tool = agent_tool_with(
        "echo",
        "Echo input",
        Arc::new(|_id, _params, _signal, _on_update| {
            Box::pin(async {
                Ok(AgentToolResult {
                    content: vec![user_text_block("echo")],
                    details: serde_json::json!({}),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are helpful.".to_string()),
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .unwrap();

    let messages = agent.messages();
    let initial = system_of(&messages[0]).expect("expected initial system message");
    assert_eq!(
        initial.content,
        SystemContent::Text("You are helpful.".to_string())
    );
    let added = initial.tools_added.as_ref().expect("toolsAdded");
    assert_eq!(
        added
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["echo"]
    );
    // 工具声明不含 `execute`（TS 断言 `not.toHaveProperty("execute")`）。
    let json = serde_json::to_value(&added[0]).unwrap();
    assert!(json.get("execute").is_none());
    assert!(json.get("label").is_none());
}

// ---------------------------------------------------------------------------
// 5. declares tool loadout changes to the model before the next request
// ---------------------------------------------------------------------------

/// TS: it("declares tool loadout changes to the model before the next request")
#[tokio::test]
async fn declares_tool_loadout_changes_to_the_model_before_the_next_request() {
    let first = create_tool("first");
    let second = create_tool("second");
    let recorded = record_requests();
    let requests = Arc::clone(&recorded);
    let stream_fn = stream_fn_with(move |_model, context, _options| {
        requests.lock().unwrap().push(tool_change_markers(&context));
        done_stream(create_assistant_message("done"))
    });
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are helpful.".to_string()),
            tools: Some(vec![first]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();

    agent.prompt_text("one").await.unwrap();
    agent.set_tools(vec![second]);
    agent.prompt_text("two").await.unwrap();
    agent.prompt_text("three").await.unwrap();

    assert_eq!(
        *recorded.lock().unwrap(),
        vec![
            vec!["+first".to_string(), "-".to_string()],
            vec![
                "+first".to_string(),
                "-".to_string(),
                "+second".to_string(),
                "-first".to_string()
            ],
            vec![
                "+first".to_string(),
                "-".to_string(),
                "+second".to_string(),
                "-first".to_string()
            ],
        ]
    );

    let update = agent
        .messages()
        .into_iter()
        .find_map(|message| match system_of(&message) {
            Some(system)
                if system
                    .tools_removed
                    .as_ref()
                    .is_some_and(|removed| !removed.is_empty()) =>
            {
                Some(system.clone())
            }
            _ => None,
        })
        .expect("tool update system message");
    // TS 用 `expect(update).toEqual({...})` 钉住整个对象的键集合（不能多出 `sections`），
    // 而 `toEqual` 不比较键序；这里额外断言键序 = `SystemMessage` 的字段声明序
    // （= TS interface 声明序，ARCHITECTURE §4.3.3）。durable 的 `kiana.system` 持久化序不同，
    // 由 `kiana-durable::types::persisted_message_json()` 单独重排。
    let update_json = serde_json::to_value(&update).unwrap();
    assert_eq!(
        update_json
            .as_object()
            .expect("system message serializes as an object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["role", "content", "toolsAdded", "toolsRemoved", "timestamp"]
    );
    assert_eq!(update.content, SystemContent::Text(String::new()));
    assert!(update.timestamp > 0);
    let added = update.tools_added.as_ref().expect("toolsAdded");
    assert_eq!(added[0].name, "second");
    assert_eq!(added[0].description, "second tool");
    assert_eq!(
        serde_json::to_value(&added[0].parameters).unwrap(),
        empty_schema()
    );
    let removed = update.tools_removed.as_ref().expect("toolsRemoved");
    assert_eq!(removed[0].name, "first");

    let initial = system_of(&agent.messages()[0])
        .expect("expected initial system message")
        .clone();
    assert!(
        serde_json::to_value(&initial.tools_added.unwrap()[0])
            .unwrap()
            .get("execute")
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// 6. merges tool changes into a pending system message
// ---------------------------------------------------------------------------

/// TS: it("merges tool changes into a pending system message")
#[tokio::test]
async fn merges_tool_changes_into_a_pending_system_message() {
    let tool = agent_tool_with(
        "echo",
        "Echo input",
        Arc::new(|_id, _params, _signal, _on_update| {
            Box::pin(async {
                Ok(AgentToolResult {
                    content: vec![user_text_block("echo")],
                    details: serde_json::json!({}),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );
    let stream_fn = stream_fn_with(|_model, context, _options| {
        let system_count = context
            .as_context()
            .messages
            .iter()
            .filter(|message| matches!(message, Message::System(_)))
            .count();
        assert_eq!(system_count, 2);
        done_stream(create_assistant_message("done"))
    });
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are helpful.".to_string()),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();

    agent.set_tools(vec![tool]);
    agent
        .prompt_messages(vec![
            custom_system_message(
                "",
                Some(vec![("skills", "<skills>x</skills>")]),
                None,
                None,
                1,
            ),
            create_user_message("hi"),
        ])
        .await
        .unwrap();

    let messages = agent.messages();
    let update = custom_object(&messages[1]).expect("pending message stays custom");
    // TS 用 `expect(agent.state.messages[1]).toEqual({...})` 钉住整个对象的键集合
    // （不能多出 `toolsRemoved`）。
    assert_eq!(
        update.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["role", "content", "sections", "timestamp", "toolsAdded"]
    );
    assert_eq!(update.get("role"), Some(&serde_json::json!("system")));
    assert_eq!(update.get("content"), Some(&serde_json::json!("")));
    assert_eq!(
        update.get("sections"),
        Some(&serde_json::json!({ "skills": "<skills>x</skills>" }))
    );
    assert_eq!(update.get("timestamp"), Some(&serde_json::json!(1)));
    let added = update
        .get("toolsAdded")
        .and_then(JsonValue::as_array)
        .expect("toolsAdded");
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].get("name"), Some(&serde_json::json!("echo")));
    assert_eq!(
        added[0].get("description"),
        Some(&serde_json::json!("Echo input"))
    );
    assert_eq!(added[0].get("parameters"), Some(&empty_schema()));
}

// ---------------------------------------------------------------------------
// 7. rewrites pending tool declarations to match the executable set
// ---------------------------------------------------------------------------

/// TS: it("rewrites pending tool declarations to match the executable set")
#[tokio::test]
async fn rewrites_pending_tool_declarations_to_match_the_executable_set() {
    let stream_fn = done_stream_fn(create_assistant_message("done"));
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are helpful.".to_string()),
            tools: Some(vec![create_tool("first")]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();

    // 待处理的 system 消息声称添加 `second`、移除 `first`，但可执行集合仍有 `first`、没有
    // `second`：以可执行集合为准。
    agent
        .prompt_messages(vec![
            custom_system_message(
                "",
                Some(vec![("note", "<note>x</note>")]),
                Some(vec![tool_declaration("second")]),
                Some(vec![ToolReference {
                    name: "first".to_string(),
                }]),
                1,
            ),
            create_user_message("hi"),
        ])
        .await
        .unwrap();

    let messages = agent.messages();
    let update = custom_object(&messages[1]).expect("pending message stays custom");
    assert_eq!(
        *update.keys().collect::<Vec<_>>(),
        vec!["role", "content", "sections", "timestamp"]
    );
    assert_eq!(update.get("content"), Some(&serde_json::json!("")));
    assert_eq!(
        update.get("sections"),
        Some(&serde_json::json!({ "note": "<note>x</note>" }))
    );
    assert_eq!(update.get("timestamp"), Some(&serde_json::json!(1)));

    let initial = system_of(&messages[0]).expect("initial system message");
    assert_eq!(
        initial
            .tools_added
            .as_ref()
            .unwrap()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["first"]
    );
}

// ---------------------------------------------------------------------------
// 8. restores the transcript baseline when reset
// ---------------------------------------------------------------------------

/// TS: it("restores the transcript baseline when reset")
#[tokio::test]
async fn restores_the_transcript_baseline_when_reset() {
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are helpful.".to_string()),
            tools: Some(vec![create_tool("echo")]),
            messages: Some(vec![create_user_message("old")]),
            ..Default::default()
        }),
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .unwrap();

    agent.reset().unwrap();

    let messages = agent.messages();
    assert_eq!(messages.len(), 1);
    let initial = system_of(&messages[0]).expect("expected initial system message");
    assert_eq!(
        initial.content,
        SystemContent::Text("You are helpful.".to_string())
    );
    assert_eq!(
        initial
            .tools_added
            .as_ref()
            .unwrap()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["echo"]
    );
}

// ---------------------------------------------------------------------------
// 9. should subscribe to events
// ---------------------------------------------------------------------------

/// TS: it("should subscribe to events")
#[tokio::test]
async fn should_subscribe_to_events() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .unwrap();

    let event_count = Arc::new(AtomicUsize::new(0));
    let count_for_listener = Arc::clone(&event_count);
    let subscription = agent.subscribe(listener(move |_event, _signal| {
        count_for_listener.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }));

    // 订阅本身不发事件。
    assert_eq!(event_count.load(Ordering::SeqCst), 0);

    // 状态修改不发事件。
    agent.set_thinking_level(ThinkingLevel::Low);
    assert_eq!(event_count.load(Ordering::SeqCst), 0);
    assert_eq!(agent.thinking_level(), ThinkingLevel::Low);

    // 退订生效。
    subscription.unsubscribe();
    agent.set_thinking_level(ThinkingLevel::High);
    assert_eq!(event_count.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------
// 10. emits full lifecycle events for thrown run failures
// ---------------------------------------------------------------------------

/// TS: it("emits full lifecycle events for thrown run failures")
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emits_full_lifecycle_events_for_thrown_run_failures() {
    let stream_fn: StreamFn = Arc::new(|_model, _context, _options| {
        Box::pin(async { Err(AiError::new(AiErrorKind::Other, "provider exploded")) })
    });
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let events_for_listener = Arc::clone(&events);
    let _subscription = agent.subscribe(listener(move |event, _signal| {
        events_for_listener
            .lock()
            .unwrap()
            .push(event.kind().to_string());
        Box::pin(async {})
    }));

    agent.prompt_text("hello").await.unwrap();

    assert_eq!(
        *events.lock().unwrap(),
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
    let messages = agent.messages();
    let last_message = messages.last().expect("last message");
    let assistant = assistant_of(last_message).expect("Expected assistant message");
    assert_eq!(assistant.stop_reason, StopReason::Error);
    assert_eq!(
        assistant.error_message.as_deref(),
        Some("provider exploded")
    );
    assert_eq!(assistant.api, "unknown");
    assert_eq!(assistant.provider, "unknown");
    assert_eq!(assistant.model, "unknown");
    assert_eq!(assistant.usage, create_usage());
    assert_eq!(assistant.content, vec![text_block("")]);
    assert_eq!(agent.error_message().as_deref(), Some("provider exploded"));
}

// ---------------------------------------------------------------------------
// 11. should await async subscribers before prompt resolves
// ---------------------------------------------------------------------------

/// TS: it("should await async subscribers before prompt resolves")
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_await_async_subscribers_before_prompt_resolves() {
    let barrier = Deferred::new();
    let agent_end_entered = Deferred::new();
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(done_stream_fn(create_assistant_message("ok"))),
        ..Default::default()
    })
    .unwrap();

    let listener_finished = Arc::new(AtomicBool::new(false));
    let barrier_for_listener = barrier.clone();
    let finished_for_listener = Arc::clone(&listener_finished);
    let entered_for_listener = agent_end_entered.clone();
    let _subscription = agent.subscribe(listener(move |event, _signal| {
        let barrier = barrier_for_listener.clone();
        let finished = Arc::clone(&finished_for_listener);
        let entered = entered_for_listener.clone();
        Box::pin(async move {
            if event.kind() == "agent_end" {
                entered.resolve();
                barrier.wait().await;
                finished.store(true, Ordering::SeqCst);
            }
        })
    }));

    let prompt_agent = agent.clone();
    let prompt = tokio::spawn(async move { prompt_agent.prompt_text("hello").await });

    agent_end_entered.wait().await;
    assert!(!prompt.is_finished());
    assert!(!listener_finished.load(Ordering::SeqCst));
    assert!(agent.is_streaming());

    barrier.resolve();
    prompt.await.unwrap().unwrap();

    assert!(listener_finished.load(Ordering::SeqCst));
    assert!(!agent.is_streaming());
}

// ---------------------------------------------------------------------------
// 12. waitForIdle should wait for async subscribers
// ---------------------------------------------------------------------------

/// TS: it("waitForIdle should wait for async subscribers")
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_for_idle_should_wait_for_async_subscribers() {
    let barrier = Deferred::new();
    let message_end_entered = Deferred::new();
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(done_stream_fn(create_assistant_message("ok"))),
        ..Default::default()
    })
    .unwrap();

    let barrier_for_listener = barrier.clone();
    let entered_for_listener = message_end_entered.clone();
    let _subscription = agent.subscribe(listener(move |event, _signal| {
        let barrier = barrier_for_listener.clone();
        let entered = entered_for_listener.clone();
        Box::pin(async move {
            if event.kind() == "message_end" && event_message_role(&event) == Some("assistant") {
                entered.resolve();
                barrier.wait().await;
            }
        })
    }));

    let prompt_agent = agent.clone();
    let prompt = tokio::spawn(async move { prompt_agent.prompt_text("hello").await });
    message_end_entered.wait().await;

    let idle_agent = agent.clone();
    let idle = tokio::spawn(async move { idle_agent.wait_for_idle().await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!idle.is_finished());
    assert!(agent.is_streaming());

    barrier.resolve();
    prompt.await.unwrap().unwrap();
    idle.await.unwrap();

    assert!(!agent.is_streaming());
}

/// `event.message?.role`。
fn event_message_role(event: &AgentEvent) -> Option<&str> {
    match event {
        AgentEvent::MessageStart { message }
        | AgentEvent::MessageUpdate { message, .. }
        | AgentEvent::MessageEnd { message }
        | AgentEvent::TurnEnd { message, .. } => Some(message.role()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 13. should pass the active abort signal to subscribers
// ---------------------------------------------------------------------------

/// 一个在 abort 前一直挂起的流：`start` 之后每 5ms 检查一次取消令牌，cancel 后推 `error`。
fn abort_aware_stream_fn(signal_slot: Arc<Mutex<Option<CancellationToken>>>) -> StreamFn {
    stream_fn_with(move |_model, _context, options| {
        let signal = options
            .as_ref()
            .and_then(|options| options.base.signal.clone());
        *signal_slot.lock().unwrap() = signal.clone();
        let stream = AssistantMessageEventStream::new();
        if let Some(signal) = signal {
            let stream_for_task = stream.clone();
            tokio::spawn(async move {
                let _ = stream_for_task.push(AssistantMessageEvent::Start {
                    partial: Box::new(create_assistant_message("")),
                });
                loop {
                    if signal.is_cancelled() {
                        let _ = stream_for_task.push(AssistantMessageEvent::Error {
                            reason: ErrorReason::Aborted,
                            error: Box::new(create_assistant_message("Aborted")),
                        });
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            });
        }
        stream
    })
}

/// TS: it("should pass the active abort signal to subscribers")
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_pass_the_active_abort_signal_to_subscribers() {
    let agent_start_entered = Deferred::new();
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(abort_aware_stream_fn(Arc::new(Mutex::new(None)))),
        ..Default::default()
    })
    .unwrap();

    let received_signal: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));
    let received_for_listener = Arc::clone(&received_signal);
    let entered_for_listener = agent_start_entered.clone();
    let _subscription = agent.subscribe(listener(move |event, signal| {
        if event.kind() == "agent_start" {
            *received_for_listener.lock().unwrap() = Some(signal.clone());
            entered_for_listener.resolve();
        }
        Box::pin(async {})
    }));

    let prompt_agent = agent.clone();
    let prompt = tokio::spawn(async move { prompt_agent.prompt_text("hello").await });
    agent_start_entered.wait().await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    let received = received_signal
        .lock()
        .unwrap()
        .clone()
        .expect("receivedSignal toBeDefined");
    assert!(!received.is_cancelled());

    agent.abort();
    prompt.await.unwrap().unwrap();

    assert!(received.is_cancelled());
}

// ---------------------------------------------------------------------------
// 14. should ignore tool updates after the tool execution settles
// ---------------------------------------------------------------------------

/// TS: it("should ignore tool updates after the tool execution settles")
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_ignore_tool_updates_after_the_tool_execution_settles() {
    let delayed_update: Arc<Mutex<Option<AgentToolUpdateCallback>>> = Arc::new(Mutex::new(None));
    let tool = agent_tool_with("delayed_tool", "Captures progress callbacks", {
        let delayed_update = Arc::clone(&delayed_update);
        Arc::new(move |_id, _params, _signal, on_update| {
            *delayed_update.lock().unwrap() = on_update.clone();
            if let Some(callback) = &on_update {
                callback(AgentToolResult {
                    content: vec![user_text_block("running")],
                    details: serde_json::json!({ "status": "running" }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                });
            }
            Box::pin(async {
                Ok(AgentToolResult {
                    content: vec![user_text_block("ok")],
                    details: serde_json::json!({ "status": "done" }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(true),
                })
            })
        })
    });
    let stream_fn = done_stream_fn(create_assistant_tool_use_message(vec![tool_call(
        "call-1",
        "delayed_tool",
    )]));
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();
    let events = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let events_for_listener = Arc::clone(&events);
    let _subscription = agent.subscribe(listener(move |event, _signal| {
        events_for_listener.lock().unwrap().push(event);
        Box::pin(async {})
    }));

    agent.prompt_text("run tool").await.unwrap();
    let event_count_after_prompt = events.lock().unwrap().len();

    let late_update = delayed_update.lock().unwrap().clone().expect("captured");
    late_update(AgentToolResult {
        content: vec![user_text_block("late")],
        details: serde_json::json!({ "status": "late" }),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    });
    tokio::time::sleep(Duration::from_millis(0)).await;

    let recorded = events.lock().unwrap();
    assert_eq!(
        recorded
            .iter()
            .filter(|event| event.kind() == "tool_execution_update")
            .count(),
        1
    );
    assert_eq!(recorded.len(), event_count_after_prompt);
}

// ---------------------------------------------------------------------------
// 15. should ignore a settled parallel tool update while another tool is still running
// ---------------------------------------------------------------------------

/// TS: it("should ignore a settled parallel tool update while another tool is still running")
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_ignore_a_settled_parallel_tool_update_while_another_tool_is_still_running() {
    let slow_started = Deferred::new();
    let settled_tool_ended = Deferred::new();
    let release_slow = Deferred::new();
    let settled_tool_update: Arc<Mutex<Option<AgentToolUpdateCallback>>> =
        Arc::new(Mutex::new(None));

    let settled_tool = agent_tool_with("settled_tool", "Captures progress callbacks", {
        let settled_tool_update = Arc::clone(&settled_tool_update);
        Arc::new(move |_id, _params, _signal, on_update| {
            *settled_tool_update.lock().unwrap() = on_update;
            Box::pin(async {
                Ok(AgentToolResult {
                    content: vec![user_text_block("done")],
                    details: serde_json::json!({ "status": "done" }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(true),
                })
            })
        })
    });
    let slow_tool = agent_tool_with("slow_tool", "Keeps the agent run active", {
        let slow_started = slow_started.clone();
        let release_slow = release_slow.clone();
        Arc::new(move |_id, _params, _signal, _on_update| {
            let slow_started = slow_started.clone();
            let release_slow = release_slow.clone();
            Box::pin(async move {
                slow_started.resolve();
                release_slow.wait().await;
                Ok(AgentToolResult {
                    content: vec![user_text_block("done")],
                    details: serde_json::json!({ "status": "done" }),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(true),
                })
            })
        })
    });
    let stream_fn = done_stream_fn(create_assistant_tool_use_message(vec![
        tool_call("call-1", "settled_tool"),
        tool_call("call-2", "slow_tool"),
    ]));
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![settled_tool, slow_tool]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();

    let events = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let events_for_listener = Arc::clone(&events);
    let ended_for_listener = settled_tool_ended.clone();
    let _subscription = agent.subscribe(listener(move |event, _signal| {
        events_for_listener.lock().unwrap().push(event.clone());
        if let AgentEvent::ToolExecutionEnd { tool_call_id, .. } = &event
            && tool_call_id == "call-1"
        {
            ended_for_listener.resolve();
        }
        Box::pin(async {})
    }));

    let prompt_agent = agent.clone();
    let prompt = tokio::spawn(async move { prompt_agent.prompt_text("run tools").await });
    slow_started.wait().await;
    settled_tool_ended.wait().await;
    let event_count_before_late_update = events.lock().unwrap().len();

    let late_update = settled_tool_update
        .lock()
        .unwrap()
        .clone()
        .expect("captured");
    late_update(AgentToolResult {
        content: vec![user_text_block("late")],
        details: serde_json::json!({ "status": "late" }),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    });
    tokio::time::sleep(Duration::from_millis(0)).await;
    assert_eq!(events.lock().unwrap().len(), event_count_before_late_update);

    release_slow.resolve();
    prompt.await.unwrap().unwrap();
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.kind() == "tool_execution_update")
            .count(),
        0
    );
}

// ---------------------------------------------------------------------------
// 16. should update state with mutators
// ---------------------------------------------------------------------------

/// TS: it("should update state with mutators")
#[tokio::test]
async fn should_update_state_with_mutators() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .unwrap();

    // Test setModel
    let new_model =
        kiana_ai::compat::get_model("google", "gemini-2.5-flash").expect("catalog model");
    agent.set_model(new_model.clone());
    assert_eq!(agent.model(), new_model);

    // Test setThinkingLevel
    agent.set_thinking_level(ThinkingLevel::High);
    assert_eq!(agent.thinking_level(), ThinkingLevel::High);

    // Test setTools（TS 断言 `not.toBe(tools)`：赋值复制顶层数组；Vec 天然是副本）。
    let tools = vec![create_tool("test")];
    agent.set_tools(tools.clone());
    assert_eq!(agent.tools().len(), tools.len());
    assert_eq!(agent.tools()[0].name, "test");

    // Test replaceMessages
    let messages = vec![create_user_message("Hello")];
    agent.set_messages(messages.clone());
    assert_eq!(agent.messages(), messages);

    // Test appendMessage（TS 对活数组 `state.messages.push(...)`；Rust 用 push_message）。
    let new_message = AgentMessage::Standard(Message::Assistant(create_assistant_message("Hi")));
    agent.push_message(new_message.clone());
    assert_eq!(agent.messages().len(), 2);
    assert_eq!(agent.messages()[1], new_message);

    // Test clearMessages
    agent.set_messages(Vec::new());
    assert!(agent.messages().is_empty());
}

// ---------------------------------------------------------------------------
// 17/18. should support steering / follow-up message queue
// ---------------------------------------------------------------------------

/// TS: it("should support steering message queue")
#[tokio::test]
async fn should_support_steering_message_queue() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .unwrap();

    let message = create_user_message("Steering message");
    agent.steer(message.clone());

    // 消息已入队，但还没进 state.messages。
    assert!(!agent.messages().contains(&message));
    assert!(agent.has_queued_messages());
    assert_eq!(agent.peek_queued_messages(), vec![message]);
}

/// TS: it("should support follow-up message queue")
#[tokio::test]
async fn should_support_follow_up_message_queue() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .unwrap();

    let message = create_user_message("Follow-up message");
    agent.follow_up(message.clone());

    // 消息已入队，但还没进 state.messages。
    assert!(!agent.messages().contains(&message));
    assert!(agent.has_queued_messages());
    assert_eq!(agent.peek_queued_messages(), vec![message]);
}

// ---------------------------------------------------------------------------
// 19. should handle abort controller
// ---------------------------------------------------------------------------

/// TS: it("should handle abort controller")
#[tokio::test]
async fn should_handle_abort_controller() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .unwrap();

    // 没有运行时应是 no-op，不 panic。
    agent.abort();
    assert!(agent.signal().is_none());
}

// ---------------------------------------------------------------------------
// 20. should reject reset while processing without corrupting the transcript
// ---------------------------------------------------------------------------

/// TS: it("should reject reset while processing without corrupting the transcript")
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_reject_reset_while_processing_without_corrupting_the_transcript() {
    let stream_started = Deferred::new();
    let release_response = Deferred::new();
    let stream_fn = {
        let stream_started = stream_started.clone();
        let release_response = release_response.clone();
        stream_fn_with(move |_model, _context, _options| {
            let stream = AssistantMessageEventStream::new();
            let stream_for_task = stream.clone();
            let started = stream_started.clone();
            let release = release_response.clone();
            tokio::spawn(async move {
                let _ = stream_for_task.push(AssistantMessageEvent::Start {
                    partial: Box::new(create_assistant_message("")),
                });
                started.resolve();
                release.wait().await;
                let _ = stream_for_task.push(AssistantMessageEvent::Done {
                    reason: DoneReason::Stop,
                    message: Box::new(create_assistant_message("Done")),
                });
            });
            stream
        })
    };
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();

    let prompt_agent = agent.clone();
    let prompt = tokio::spawn(async move { prompt_agent.prompt_text("Hello").await });
    stream_started.wait().await;

    assert!(agent.is_streaming());
    assert_eq!(roles(&agent.messages()), vec!["user"]);
    let error = agent.reset().expect_err("reset must fail while processing");
    assert_eq!(
        error.to_string(),
        "Agent is already processing. Wait for completion before resetting."
    );
    assert!(agent.is_streaming());
    assert_eq!(roles(&agent.messages()), vec!["user"]);

    release_response.resolve();
    prompt.await.unwrap().unwrap();

    assert!(!agent.is_streaming());
    assert_eq!(roles(&agent.messages()), vec!["user", "assistant"]);
}

// ---------------------------------------------------------------------------
// 21. should throw when prompt() called while streaming
// ---------------------------------------------------------------------------

/// TS: it("should throw when prompt() called while streaming")
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_throw_when_prompt_called_while_streaming() {
    let abort_signal: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(abort_aware_stream_fn(Arc::clone(&abort_signal))),
        ..Default::default()
    })
    .unwrap();

    // 第一个 prompt 不 await（它会一直挂到 abort）。
    let prompt_agent = agent.clone();
    let first_prompt = tokio::spawn(async move { prompt_agent.prompt_text("First message").await });

    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(agent.is_streaming());

    let error = agent
        .prompt_text("Second message")
        .await
        .expect_err("second prompt must reject");
    assert_eq!(
        error.to_string(),
        "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion."
    );

    // 清理：abort 停止流。
    agent.abort();
    let _ = first_prompt.await.unwrap();
}

// ---------------------------------------------------------------------------
// 22. should throw when continue() called while streaming
// ---------------------------------------------------------------------------

/// TS: it("should throw when continue() called while streaming")
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_throw_when_continue_called_while_streaming() {
    let abort_signal: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(abort_aware_stream_fn(Arc::clone(&abort_signal))),
        ..Default::default()
    })
    .unwrap();

    let prompt_agent = agent.clone();
    let first_prompt = tokio::spawn(async move { prompt_agent.prompt_text("First message").await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(agent.is_streaming());

    let error = agent.continue_().await.expect_err("continue must reject");
    assert_eq!(
        error.to_string(),
        "Agent is already processing. Wait for completion before continuing."
    );

    agent.abort();
    let _ = first_prompt.await.unwrap();
}

// ---------------------------------------------------------------------------
// 23. continue() should process queued follow-up messages after an assistant turn
// ---------------------------------------------------------------------------

/// TS: it("continue() should process queued follow-up messages after an assistant turn")
#[tokio::test]
async fn continue_should_process_queued_follow_up_messages_after_an_assistant_turn() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(done_stream_fn(create_assistant_message("Processed"))),
        ..Default::default()
    })
    .unwrap();

    agent.set_messages(vec![
        create_user_message_blocks("Initial"),
        AgentMessage::Standard(Message::Assistant(create_assistant_message(
            "Initial response",
        ))),
    ]);
    agent.follow_up(create_user_message_blocks("Queued follow-up"));

    agent.continue_().await.unwrap();

    let messages = agent.messages();
    let has_queued_follow_up = messages
        .iter()
        .any(|message| user_message_contains(message, "Queued follow-up"));
    assert!(has_queued_follow_up);
    assert_eq!(messages.last().unwrap().role(), "assistant");
}

// ---------------------------------------------------------------------------
// 24. continue() keeps $mode steering semantics for assistant-tail fallback
// ---------------------------------------------------------------------------

async fn continue_keeps_steering_semantics_for_assistant_tail_fallback(
    mode: QueueMode,
    expected_requests: usize,
) {
    let requests: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let requests_for_fn = Arc::clone(&requests);
    let stream_fn = stream_fn_with(move |_model, context, _options| {
        requests_for_fn.lock().unwrap().push(user_texts(&context));
        done_stream(create_assistant_message("Processed"))
    });
    let agent = Agent::new(AgentOptions {
        steering_mode: Some(mode),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();
    agent.set_messages(vec![
        create_user_message("Initial"),
        AgentMessage::Standard(Message::Assistant(create_assistant_message(
            "Initial response",
        ))),
    ]);
    agent.steer(create_user_message("Steering 1"));
    agent.steer(create_user_message("Steering 2"));

    agent.continue_().await.unwrap();

    let recorded = requests.lock().unwrap().clone();
    assert_eq!(recorded.len(), expected_requests);
    assert!(recorded[0].contains(&"Steering 1".to_string()));
    if mode == QueueMode::OneAtATime {
        assert!(!recorded[0].contains(&"Steering 2".to_string()));
        assert!(recorded[1].contains(&"Steering 2".to_string()));
    } else {
        assert!(recorded[0].contains(&"Steering 2".to_string()));
    }
}

/// TS: it.each([{mode:"one-at-a-time"},{mode:"all"}])("continue() keeps $mode steering
/// semantics for assistant-tail fallback")
#[tokio::test]
async fn continue_keeps_one_at_a_time_steering_semantics_for_assistant_tail_fallback() {
    continue_keeps_steering_semantics_for_assistant_tail_fallback(QueueMode::OneAtATime, 2).await;
}

/// TS: it.each 的 `mode: "all"` 用例。
#[tokio::test]
async fn continue_keeps_all_steering_semantics_for_assistant_tail_fallback() {
    continue_keeps_steering_semantics_for_assistant_tail_fallback(QueueMode::All, 1).await;
}

// ---------------------------------------------------------------------------
// 25. rejects a queued continuation from $name context without draining queues
// ---------------------------------------------------------------------------

fn system_message(content: impl Into<String>, timestamp: i64) -> AgentMessage {
    AgentMessage::Standard(Message::System(SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(content.into()),
        sections: None,
        tools_added: None,
        tools_removed: None,
        timestamp,
    }))
}

async fn rejects_a_queued_continuation_without_draining_queues(messages: Vec<AgentMessage>) {
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            messages: Some(messages),
            ..Default::default()
        }),
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .unwrap();
    let steering = create_user_message("steering");
    let follow_up = create_user_message("follow-up");
    agent.steer(steering.clone());
    agent.follow_up(follow_up.clone());

    let error = agent.continue_().await.expect_err("continue must reject");
    assert_eq!(error.to_string(), "No messages to continue from");
    assert_eq!(agent.peek_queued_messages(), vec![steering]);
    agent.clear_steering_queue();
    assert_eq!(agent.peek_queued_messages(), vec![follow_up]);
}

/// TS: it.each([{name:"empty",messages:[]}])("rejects a queued continuation from $name context
/// without draining queues")
#[tokio::test]
async fn rejects_a_queued_continuation_from_empty_context_without_draining_queues() {
    rejects_a_queued_continuation_without_draining_queues(Vec::new()).await;
}

/// TS: it.each 的 `name: "system-only"` 用例。
#[tokio::test]
async fn rejects_a_queued_continuation_from_system_only_context_without_draining_queues() {
    rejects_a_queued_continuation_without_draining_queues(vec![system_message("system only", 1)])
        .await;
}

// ---------------------------------------------------------------------------
// 26. defers follow-up input on the first continuation request from a $name tail
// ---------------------------------------------------------------------------

async fn defers_follow_up_input_on_the_first_continuation_request(messages: Vec<AgentMessage>) {
    let requests: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let requests_for_fn = Arc::clone(&requests);
    let stream_fn = stream_fn_with(move |_model, context, _options| {
        requests_for_fn.lock().unwrap().push(user_texts(&context));
        done_stream(create_assistant_message("done"))
    });
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            messages: Some(messages),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();
    agent.follow_up(create_user_message("follow-up"));

    agent.continue_().await.unwrap();

    let recorded = requests.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2);
    assert!(!recorded[0].contains(&"follow-up".to_string()));
    assert!(recorded[1].contains(&"follow-up".to_string()));
}

fn tool_result_message() -> AgentMessage {
    AgentMessage::Standard(Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "call-1".to_string(),
        tool_name: "noop".to_string(),
        content: vec![user_text_block("done")],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 1,
        duration_ms: None,
    }))
}

/// TS: it.each([{name:"user",...}])("defers follow-up input on the first continuation request
/// from a $name tail")
#[tokio::test]
async fn defers_follow_up_input_on_the_first_continuation_request_from_a_user_tail() {
    defers_follow_up_input_on_the_first_continuation_request(vec![create_user_message(
        "existing user",
    )])
    .await;
}

/// TS: it.each 的 `name: "toolResult"` 用例。
#[tokio::test]
async fn defers_follow_up_input_on_the_first_continuation_request_from_a_tool_result_tail() {
    defers_follow_up_input_on_the_first_continuation_request(vec![
        create_user_message("existing user"),
        AgentMessage::Standard(Message::Assistant(create_assistant_tool_use_message(vec![
            tool_call("call-1", "noop"),
        ]))),
        tool_result_message(),
    ])
    .await;
}

// ---------------------------------------------------------------------------
// 27. polls $mode steering at continuation startup
// ---------------------------------------------------------------------------

async fn polls_steering_at_continuation_startup(mode: QueueMode, expected_requests: usize) {
    let requests: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let requests_for_fn = Arc::clone(&requests);
    let stream_fn = stream_fn_with(move |_model, context, _options| {
        requests_for_fn.lock().unwrap().push(user_texts(&context));
        done_stream(create_assistant_message("done"))
    });
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            messages: Some(vec![create_user_message("existing")]),
            ..Default::default()
        }),
        steering_mode: Some(mode),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();
    agent.steer(create_user_message("first"));
    agent.steer(create_user_message("second"));

    agent.continue_().await.unwrap();

    let recorded = requests.lock().unwrap().clone();
    assert_eq!(recorded.len(), expected_requests);
    assert!(recorded[0].contains(&"first".to_string()));
    if mode == QueueMode::OneAtATime {
        assert!(!recorded[0].contains(&"second".to_string()));
        assert!(recorded[1].contains(&"second".to_string()));
    } else {
        assert!(recorded[0].contains(&"second".to_string()));
    }
}

/// TS: it.each([{mode:"one-at-a-time"}])("polls $mode steering at continuation startup")
#[tokio::test]
async fn polls_one_at_a_time_steering_at_continuation_startup() {
    polls_steering_at_continuation_startup(QueueMode::OneAtATime, 2).await;
}

/// TS: it.each 的 `mode: "all"` 用例。
#[tokio::test]
async fn polls_all_steering_at_continuation_startup() {
    polls_steering_at_continuation_startup(QueueMode::All, 1).await;
}

// ---------------------------------------------------------------------------
// 28. keeps queues on a %s response even when finishTurn requests continuation
// ---------------------------------------------------------------------------

async fn keeps_queues_on_a_response_even_when_finish_turn_requests_continuation(
    stop_reason: StopReason,
    error_reason: ErrorReason,
) {
    let queued_during_response = create_user_message("steering");
    let follow_up = create_user_message("follow-up");
    let finish_turn: FinishTurnFn =
        Arc::new(|_turn, _signal| Box::pin(async { Some(AgentTurnDecision::Continue) }));
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let mut message = create_assistant_message(match stop_reason {
            StopReason::Aborted => "aborted",
            _ => "error",
        });
        message.stop_reason = stop_reason;
        message.error_message = Some(match stop_reason {
            StopReason::Aborted => "aborted".to_string(),
            _ => "error".to_string(),
        });
        error_stream(message, error_reason)
    });
    let agent = Agent::new(AgentOptions {
        finish_turn: Some(finish_turn),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();
    agent.follow_up(follow_up.clone());

    let weak = agent.downgrade();
    let queued_for_listener = queued_during_response.clone();
    let _subscription = agent.subscribe(listener(move |event, _signal| {
        if event.kind() == "message_end" && event_message_role(&event) == Some("assistant") {
            weak.steer(queued_for_listener.clone());
        }
        Box::pin(async {})
    }));

    agent.prompt_text("start").await.unwrap();

    assert_eq!(agent.peek_queued_messages(), vec![queued_during_response]);
    agent.clear_steering_queue();
    assert_eq!(agent.peek_queued_messages(), vec![follow_up]);
}

/// TS: it.each(["error","aborted"])("keeps queues on a %s response even when finishTurn
/// requests continuation")
#[tokio::test]
async fn keeps_queues_on_a_error_response_even_when_finish_turn_requests_continuation() {
    keeps_queues_on_a_response_even_when_finish_turn_requests_continuation(
        StopReason::Error,
        ErrorReason::Error,
    )
    .await;
}

/// TS: it.each 的 `"aborted"` 用例。
#[tokio::test]
async fn keeps_queues_on_a_aborted_response_even_when_finish_turn_requests_continuation() {
    keeps_queues_on_a_response_even_when_finish_turn_requests_continuation(
        StopReason::Aborted,
        ErrorReason::Aborted,
    )
    .await;
}

// ---------------------------------------------------------------------------
// 29. keeps legacy prepareNextTurn signal callback behavior
// ---------------------------------------------------------------------------

fn noop_tool() -> AgentTool {
    agent_tool_with(
        "noop",
        "Noop tool",
        Arc::new(|_id, _params, _signal, _on_update| {
            Box::pin(async {
                Ok(AgentToolResult {
                    content: vec![user_text_block("ok")],
                    details: serde_json::json!({}),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    )
}

/// TS: it("keeps legacy prepareNextTurn signal callback behavior")
#[tokio::test]
async fn keeps_legacy_prepare_next_turn_signal_callback_behavior() {
    let request_count = Arc::new(AtomicUsize::new(0));
    let saw_abort_signal = Arc::new(AtomicBool::new(false));
    let saw_for_hook = Arc::clone(&saw_abort_signal);
    let prepare_next_turn: AgentPrepareNextTurnFn = Arc::new(move |signal| {
        saw_for_hook.store(signal.is_some(), Ordering::SeqCst);
        Box::pin(async { None })
    });
    let count_for_stream = Arc::clone(&request_count);
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let index = count_for_stream.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            done_stream_with_reason(
                create_assistant_tool_use_message(vec![tool_call("tool-1", "noop")]),
                DoneReason::ToolUse,
            )
        } else {
            done_stream(create_assistant_message("done"))
        }
    });
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![noop_tool()]),
            ..Default::default()
        }),
        prepare_next_turn: Some(prepare_next_turn),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();

    agent.prompt_text("start").await.unwrap();

    assert_eq!(request_count.load(Ordering::SeqCst), 2);
    assert!(saw_abort_signal.load(Ordering::SeqCst));
}

// ---------------------------------------------------------------------------
// 30. forwards finishTurn through AgentOptions with the active abort signal
// ---------------------------------------------------------------------------

/// TS: it("forwards finishTurn through AgentOptions with the active abort signal")
#[tokio::test]
async fn forwards_finish_turn_through_agent_options_with_the_active_abort_signal() {
    let request_count = Arc::new(AtomicUsize::new(0));
    let saw_abort_signal = Arc::new(AtomicBool::new(false));
    let callback_context_roles: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let saw_for_hook = Arc::clone(&saw_abort_signal);
    let roles_for_hook = Arc::clone(&callback_context_roles);
    let finish_turn: FinishTurnFn = Arc::new(move |turn: AgentTurnContext, signal| {
        saw_for_hook.store(signal.is_some(), Ordering::SeqCst);
        *roles_for_hook.lock().unwrap() = turn
            .context
            .messages
            .iter()
            .map(|message| message.role().to_string())
            .collect();
        Box::pin(async { Some(AgentTurnDecision::End) })
    });
    let tool = agent_tool_with(
        "noop",
        "Noop tool",
        Arc::new(|_id, _params, _signal, _on_update| {
            Box::pin(async {
                Ok(AgentToolResult {
                    content: vec![user_text_block("tool complete")],
                    details: serde_json::json!({}),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );
    let count_for_stream = Arc::clone(&request_count);
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let index = count_for_stream.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            done_stream_with_reason(
                create_assistant_tool_use_message(vec![tool_call("tool-1", "noop")]),
                DoneReason::ToolUse,
            )
        } else {
            done_stream(create_assistant_message("should not run"))
        }
    });
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        finish_turn: Some(finish_turn),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();

    agent.prompt_text("start").await.unwrap();

    assert_eq!(request_count.load(Ordering::SeqCst), 1);
    assert!(saw_abort_signal.load(Ordering::SeqCst));
    assert_eq!(
        *callback_context_roles.lock().unwrap(),
        vec!["system", "user", "assistant", "toolResult"]
    );
}

// ---------------------------------------------------------------------------
// 31. keeps steering ahead of follow-up from a non-assistant continuation tail
// ---------------------------------------------------------------------------

/// TS: it("keeps steering ahead of follow-up from a non-assistant continuation tail")
#[tokio::test]
async fn keeps_steering_ahead_of_follow_up_from_a_non_assistant_continuation_tail() {
    let requests: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let requests_for_fn = Arc::clone(&requests);
    let stream_fn = stream_fn_with(move |_model, context, _options| {
        requests_for_fn.lock().unwrap().push(user_texts(&context));
        done_stream(create_assistant_message("done"))
    });
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            messages: Some(vec![create_user_message("existing")]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();
    agent.steer(create_user_message("steering"));
    agent.follow_up(create_user_message("follow-up"));

    agent.continue_().await.unwrap();

    let recorded = requests.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2);
    assert!(recorded[0].contains(&"steering".to_string()));
    assert!(!recorded[0].contains(&"follow-up".to_string()));
    assert!(recorded[1].contains(&"follow-up".to_string()));
}

// ---------------------------------------------------------------------------
// 32. keeps queues when finishTurn ends the run
// ---------------------------------------------------------------------------

/// TS: it("keeps queues when finishTurn ends the run")
#[tokio::test]
async fn keeps_queues_when_finish_turn_ends_the_run() {
    let queued_during_response = create_user_message("steering");
    let follow_up = create_user_message("follow-up");
    let finish_turn: FinishTurnFn =
        Arc::new(|_turn, _signal| Box::pin(async { Some(AgentTurnDecision::End) }));
    let agent = Agent::new(AgentOptions {
        finish_turn: Some(finish_turn),
        stream_fn: Some(done_stream_fn(create_assistant_message("done"))),
        ..Default::default()
    })
    .unwrap();
    agent.follow_up(follow_up.clone());

    let weak = agent.downgrade();
    let queued_for_listener = queued_during_response.clone();
    let _subscription = agent.subscribe(listener(move |event, _signal| {
        if event.kind() == "message_end" && event_message_role(&event) == Some("assistant") {
            weak.steer(queued_for_listener.clone());
        }
        Box::pin(async {})
    }));

    agent.prompt_text("start").await.unwrap();

    assert_eq!(agent.peek_queued_messages(), vec![queued_during_response]);
    agent.clear_steering_queue();
    assert_eq!(agent.peek_queued_messages(), vec![follow_up]);
}

// ---------------------------------------------------------------------------
// 33. previews the next selected queued messages without consuming them
// ---------------------------------------------------------------------------

/// TS: it("previews the next selected queued messages without consuming them")
#[tokio::test]
async fn previews_the_next_selected_queued_messages_without_consuming_them() {
    let agent = Agent::new(AgentOptions {
        steering_mode: Some(QueueMode::OneAtATime),
        follow_up_mode: Some(QueueMode::All),
        stream_fn: Some(stream_fn_with(|_model, _context, _options| {
            create_assistant_message_event_stream()
        })),
        ..Default::default()
    })
    .unwrap();
    let first = create_user_message("first steering");
    let second = create_user_message("second steering");
    let follow_up = create_user_message("follow-up");
    agent.steer(first.clone());
    agent.steer(second);
    agent.follow_up(follow_up.clone());

    assert_eq!(agent.peek_queued_messages(), vec![first.clone()]);
    assert_eq!(agent.peek_queued_messages(), vec![first]);
    agent.clear_steering_queue();
    assert_eq!(agent.peek_queued_messages(), vec![follow_up]);
}

// ---------------------------------------------------------------------------
// 34. forwards provider stream event observers through AgentOptions
// ---------------------------------------------------------------------------

/// TS: it("forwards provider stream event observers through AgentOptions")
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forwards_provider_stream_event_observers_through_agent_options() {
    let provider_events: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
    let events_for_option = Arc::clone(&provider_events);
    let stream_fn = stream_fn_with(|model, _context, options| {
        let handler = options
            .as_ref()
            .and_then(|options| options.base.on_provider_stream_event.clone());
        let stream = AssistantMessageEventStream::new();
        let stream_for_task = stream.clone();
        tokio::spawn(async move {
            if let Some(handler) = handler {
                handler(serde_json::json!({ "request_cost": 0.01 }), &model);
            }
            let _ = stream_for_task.push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: Box::new(create_assistant_message("ok")),
            });
        });
        stream
    });
    let agent = Agent::new(AgentOptions {
        on_provider_stream_event: Some(Arc::new(move |data: JsonValue, _model: &Model| {
            events_for_option.lock().unwrap().push(data);
        })),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();

    agent.prompt_text("hello").await.unwrap();

    assert_eq!(
        *provider_events.lock().unwrap(),
        vec![serde_json::json!({ "request_cost": 0.01 })]
    );
}

// ---------------------------------------------------------------------------
// 35. forwards sessionId to streamFunction options
// ---------------------------------------------------------------------------

/// TS: it("forwards sessionId to streamFunction options")
#[tokio::test]
async fn forwards_session_id_to_stream_function_options() {
    let received_session_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let received_for_fn = Arc::clone(&received_session_id);
    let stream_fn = stream_fn_with(move |_model, _context, options| {
        *received_for_fn.lock().unwrap() = options
            .as_ref()
            .and_then(|options| options.base.session_id.clone());
        done_stream(create_assistant_message("ok"))
    });
    let agent = Agent::new(AgentOptions {
        session_id: Some("session-abc".to_string()),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();

    agent.prompt_text("hello").await.unwrap();
    assert_eq!(
        received_session_id.lock().unwrap().as_deref(),
        Some("session-abc")
    );

    // Test setter
    agent.set_session_id(Some("session-def".to_string()));
    assert_eq!(agent.session_id().as_deref(), Some("session-def"));

    agent.prompt_text("hello again").await.unwrap();
    assert_eq!(
        received_session_id.lock().unwrap().as_deref(),
        Some("session-def")
    );
}

// ---------------------------------------------------------------------------
// 36. attributes requests to kiana by default and merges a configured identity over it
// ---------------------------------------------------------------------------

/// TS: it("attributes requests to kiana by default and merges a configured identity over it")
#[tokio::test]
async fn attributes_requests_to_kiana_by_default_and_merges_a_configured_identity_over_it() {
    let received: Arc<Mutex<Vec<Option<ClientIdentity>>>> = Arc::new(Mutex::new(Vec::new()));
    let received_for_fn = Arc::clone(&received);
    let stream_fn = stream_fn_with(move |_model, _context, options| {
        received_for_fn.lock().unwrap().push(
            options
                .as_ref()
                .and_then(|options| options.base.identity.clone()),
        );
        done_stream(create_assistant_message("ok"))
    });
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();

    agent.prompt_text("hello").await.unwrap();
    let acme = ClientAttribution {
        name: "acme".to_string(),
        url: "https://acme.example".to_string(),
        categories: None,
    };
    agent.set_identity(Some(ClientIdentity {
        user_agent: Some(UserAgentMode::Short("short".to_string())),
        attribution: Some(Some(acme.clone())),
        version: None,
    }));
    agent.prompt_text("hello again").await.unwrap();
    agent.set_identity(Some(ClientIdentity {
        user_agent: Some(UserAgentMode::Short("short".to_string())),
        attribution: None,
        version: None,
    }));
    agent.prompt_text("and again").await.unwrap();

    let recorded = received.lock().unwrap().clone();
    assert_eq!(recorded.len(), 3);
    assert_eq!(
        recorded[0],
        Some(ClientIdentity {
            version: None,
            user_agent: None,
            attribution: Some(Some(kiana_ai::types::kiana_attribution())),
        })
    );
    assert_eq!(
        recorded[1],
        Some(ClientIdentity {
            version: None,
            user_agent: Some(UserAgentMode::Short("short".to_string())),
            attribution: Some(Some(acme)),
        })
    );
    assert_eq!(
        recorded[2],
        Some(ClientIdentity {
            version: None,
            user_agent: Some(UserAgentMode::Short("short".to_string())),
            attribution: Some(Some(kiana_ai::types::kiana_attribution())),
        })
    );
}

// ---------------------------------------------------------------------------
// 修复回归（reviewer 报告与 TS promise / Set 语义的差异）
// ---------------------------------------------------------------------------

/// 修复回归：TS 的 `prompt()` 是 JS promise，调用即开始运行；调用方丢弃/不 `await` 返回的
/// promise 时 run 仍会跑完（`finally { finishRun() }` 一定执行）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discarding_the_prompt_future_still_runs_to_completion() {
    let stream_started = Deferred::new();
    let release_response = Deferred::new();
    let stream_fn = {
        let stream_started = stream_started.clone();
        let release_response = release_response.clone();
        stream_fn_with(move |_model, _context, _options| {
            let stream = AssistantMessageEventStream::new();
            let stream_for_task = stream.clone();
            let started = stream_started.clone();
            let release = release_response.clone();
            tokio::spawn(async move {
                let _ = stream_for_task.push(AssistantMessageEvent::Start {
                    partial: Box::new(create_assistant_message("")),
                });
                started.resolve();
                release.wait().await;
                let _ = stream_for_task.push(AssistantMessageEvent::Done {
                    reason: DoneReason::Stop,
                    message: Box::new(create_assistant_message("Done")),
                });
            });
            stream
        })
    };
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();

    // fire-and-forget：立即 drop 返回的 future，但 run 已经同步登记并 spawn。
    drop(agent.prompt_text("Hello"));
    tokio::time::timeout(Duration::from_secs(5), stream_started.wait())
        .await
        .expect("a discarded prompt future must still start the run");
    assert!(agent.is_streaming());
    assert!(agent.signal().is_some());

    release_response.resolve();
    tokio::time::timeout(Duration::from_secs(5), agent.wait_for_idle())
        .await
        .expect("discarded prompt future must not wedge waitForIdle");

    assert!(!agent.is_streaming());
    assert!(agent.signal().is_none());
    assert_eq!(roles(&agent.messages()), vec!["user", "assistant"]);

    // Agent 仍然可用。
    agent.prompt_text("Again").await.unwrap();
    assert_eq!(
        roles(&agent.messages()),
        vec!["user", "assistant", "user", "assistant"]
    );
}

/// 修复回归：丢弃 run 中（已开始执行）的 prompt future（`select!`/`timeout`/任务取消）不会
/// 让 `activeRun` 永远残留；run 继续跑完并清理，`waitForIdle()` 正常返回。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_prompt_future_mid_run_lets_the_run_finish() {
    let stream_started = Deferred::new();
    let release_response = Deferred::new();
    let stream_fn = {
        let stream_started = stream_started.clone();
        let release_response = release_response.clone();
        stream_fn_with(move |_model, _context, _options| {
            let stream = AssistantMessageEventStream::new();
            let stream_for_task = stream.clone();
            let started = stream_started.clone();
            let release = release_response.clone();
            tokio::spawn(async move {
                let _ = stream_for_task.push(AssistantMessageEvent::Start {
                    partial: Box::new(create_assistant_message("")),
                });
                started.resolve();
                release.wait().await;
                let _ = stream_for_task.push(AssistantMessageEvent::Done {
                    reason: DoneReason::Stop,
                    message: Box::new(create_assistant_message("Done")),
                });
            });
            stream
        })
    };
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();

    let mut prompt = agent.prompt_text("Hello");
    tokio::select! {
        result = &mut prompt => panic!("prompt should still be running: {result:?}"),
        _ = stream_started.wait() => {}
    }
    drop(prompt);

    assert!(agent.is_streaming());
    release_response.resolve();
    tokio::time::timeout(Duration::from_secs(5), agent.wait_for_idle())
        .await
        .expect("dropped prompt future must not wedge waitForIdle");

    assert!(!agent.is_streaming());
    assert_eq!(roles(&agent.messages()), vec!["user", "assistant"]);
    // 后续 run 不会命中 "Agent is already processing"。
    agent.prompt_text("Again").await.unwrap();
    assert_eq!(roles(&agent.messages()).len(), 4);
}

/// 修复回归：TS `listeners` 是 `Set`——同一个 listener（同一 `Arc`）订阅两次只保留一个条目，
/// 每次事件只调用一次。
#[tokio::test]
async fn subscribing_the_same_listener_twice_fires_it_once() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(done_stream_fn(create_assistant_message("ok"))),
        ..Default::default()
    })
    .unwrap();

    let duplicate_count = Arc::new(AtomicUsize::new(0));
    let duplicate_listener = listener({
        let count = Arc::clone(&duplicate_count);
        move |_event, _signal| {
            count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }
    });
    let _first = agent.subscribe(duplicate_listener.clone());
    let _second = agent.subscribe(duplicate_listener.clone());

    let reference_count = Arc::new(AtomicUsize::new(0));
    let reference_listener = listener({
        let count = Arc::clone(&reference_count);
        move |_event, _signal| {
            count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }
    });
    let _reference = agent.subscribe(reference_listener);

    agent.prompt_text("hello").await.unwrap();

    let expected = reference_count.load(Ordering::SeqCst);
    assert!(expected > 0, "the run must emit events");
    assert_eq!(duplicate_count.load(Ordering::SeqCst), expected);
}

/// 修复回归：TS 的 `Set` 迭代是活的——在某个事件派发中途新订阅的 listener 会收到**当前**
/// 事件（而不是从下一个事件才开始）。
#[tokio::test]
async fn a_listener_added_during_dispatch_receives_the_current_event() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(done_stream_fn(create_assistant_message("ok"))),
        ..Default::default()
    })
    .unwrap();

    let late_events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let late_events_for_listener = Arc::clone(&late_events);
    let late = listener(move |event, _signal| {
        late_events_for_listener
            .lock()
            .unwrap()
            .push(event.kind().to_string());
        Box::pin(async {})
    });
    let late_slot = Arc::new(Mutex::new(Some(late)));
    let agent_for_listener = agent.clone();
    let adder = listener(move |event, _signal| {
        if event.kind() == "agent_start"
            && let Some(late) = late_slot.lock().unwrap().take()
        {
            // 丢弃返回的 Subscription：TS 里丢弃 unsubscribe 闭包后 listener 仍然有效。
            agent_for_listener.subscribe(late);
        }
        Box::pin(async {})
    });
    let _adder = agent.subscribe(adder);

    agent.prompt_text("hello").await.unwrap();

    let events = late_events.lock().unwrap().clone();
    assert_eq!(events.first().map(String::as_str), Some("agent_start"));
    assert_eq!(events.last().map(String::as_str), Some("agent_end"));
}

/// 修复回归：活迭代——在轮到自己之前被退订的 listener 不会被当前事件调用。
#[tokio::test]
async fn a_listener_unsubscribed_during_dispatch_is_skipped() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(done_stream_fn(create_assistant_message("ok"))),
        ..Default::default()
    })
    .unwrap();

    let victim_count = Arc::new(AtomicUsize::new(0));
    // remover 先订阅（seq 更小），在 agent_start 派发时先执行并退订 victim。
    let victim_slot: Arc<Mutex<Option<Subscription>>> = Arc::new(Mutex::new(None));
    let victim_slot_for_remover = Arc::clone(&victim_slot);
    let remover = listener(move |event, _signal| {
        if event.kind() == "agent_start"
            && let Some(subscription) = victim_slot_for_remover.lock().unwrap().take()
        {
            subscription.unsubscribe();
        }
        Box::pin(async {})
    });
    let _remover = agent.subscribe(remover);

    let count_for_victim = Arc::clone(&victim_count);
    let victim = listener(move |_event, _signal| {
        count_for_victim.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    });
    let victim_subscription = agent.subscribe(victim);
    *victim_slot.lock().unwrap() = Some(victim_subscription);

    agent.prompt_text("hello").await.unwrap();

    assert_eq!(
        victim_count.load(Ordering::SeqCst),
        0,
        "a listener removed before its turn must not receive the current event"
    );
}

/// 修复回归：TS 里 `agent.subscribe(f)` 丢弃返回值时 listener 仍然注册；Rust 的
/// [`kiana_agent_core::agent::Subscription`] 因此不在 drop 时退订。
#[tokio::test]
async fn dropping_the_subscription_handle_keeps_the_listener_registered() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(done_stream_fn(create_assistant_message("ok"))),
        ..Default::default()
    })
    .unwrap();

    let count = Arc::new(AtomicUsize::new(0));
    let count_for_listener = Arc::clone(&count);
    agent.subscribe(listener(move |_event, _signal| {
        count_for_listener.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }));

    agent.prompt_text("hello").await.unwrap();

    assert!(count.load(Ordering::SeqCst) > 0);
}

/// 修复回归：TS 的 `getCurrentSystemMessage` 在没有任何 system 消息带 `timestamp`（且没有
/// 工具）时返回 `undefined`——即使自定义 system 消息带了 content。此时 `state.systemPrompt`
/// 是空串，`reset()` 清空 transcript。
#[tokio::test]
async fn a_timestamp_less_custom_system_message_is_an_empty_baseline() {
    let mut object = JsonObject::new();
    object.insert("role".to_string(), serde_json::json!("system"));
    object.insert("content".to_string(), serde_json::json!("custom prompt"));
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            messages: Some(vec![AgentMessage::Custom(Box::new(object))]),
            ..Default::default()
        }),
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .unwrap();

    assert_eq!(agent.system_prompt(), "");
    assert_eq!(agent.state().system_prompt, "");

    agent.reset().unwrap();
    assert!(agent.messages().is_empty());
}

/// 修复回归：replay 使用第一条**带 `timestamp` 属性**的 system 消息的时间戳（缺失
/// `timestamp` 的自定义 system 消息不参与），content 仍然按顺序拼接。
#[tokio::test]
async fn replayed_baseline_uses_the_first_timestamped_system_message() {
    let mut object = JsonObject::new();
    object.insert("role".to_string(), serde_json::json!("system"));
    object.insert("content".to_string(), serde_json::json!("a"));
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            messages: Some(vec![
                AgentMessage::Custom(Box::new(object)),
                AgentMessage::Standard(Message::System(SystemMessage {
                    role: SystemRole::System,
                    content: SystemContent::Text("b".to_string()),
                    sections: None,
                    tools_added: None,
                    tools_removed: None,
                    timestamp: 5,
                })),
            ]),
            ..Default::default()
        }),
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .unwrap();

    assert_eq!(agent.system_prompt(), "a\n\nb");

    agent.reset().unwrap();
    let messages = agent.messages();
    assert_eq!(messages.len(), 1);
    let baseline = system_of(&messages[0]).expect("replayed system message");
    assert_eq!(baseline.content, SystemContent::Text("a\n\nb".to_string()));
    assert_eq!(baseline.timestamp, 5);
}

/// 修复回归：没有 `timestamp` 但有 `toolsAdded` 的自定义 system 消息仍然参与回放
/// （TS: `timestamp === undefined && tools.length === 0` 才返回 undefined）。
#[tokio::test]
async fn a_timestamp_less_system_message_with_tools_is_not_dropped() {
    let mut object = JsonObject::new();
    object.insert("role".to_string(), serde_json::json!("system"));
    object.insert("content".to_string(), serde_json::json!("with tools"));
    object.insert(
        "toolsAdded".to_string(),
        serde_json::to_value(vec![tool_declaration("echo")]).unwrap(),
    );
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            messages: Some(vec![AgentMessage::Custom(Box::new(object))]),
            ..Default::default()
        }),
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .unwrap();

    assert_eq!(agent.system_prompt(), "with tools");

    agent.reset().unwrap();
    let messages = agent.messages();
    assert_eq!(messages.len(), 1);
    let baseline = system_of(&messages[0]).expect("replayed system message");
    assert_eq!(baseline.timestamp, 0);
    assert_eq!(
        baseline
            .tools_added
            .as_ref()
            .unwrap()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["echo"]
    );
}

/// 修复回归：TS 的 `createLoopConfig` 只在创建时决定是否提供 `prepareNextTurn` 闭包，
/// 闭包内部**每次调用**都重新读取 `prepareNextTurnWithContext` / `prepareNextTurn`。
/// run 中替换钩子必须从下一轮开始生效。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_next_turn_hooks_are_read_live_on_each_turn() {
    let first_calls = Arc::new(AtomicUsize::new(0));
    let second_calls = Arc::new(AtomicUsize::new(0));

    let first_calls_for_hook = Arc::clone(&first_calls);
    let first: AgentPrepareNextTurnWithContextFn = Arc::new(move |_context, _signal| {
        first_calls_for_hook.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { None })
    });
    let second_calls_for_hook = Arc::clone(&second_calls);
    let second: AgentPrepareNextTurnWithContextFn = Arc::new(move |_context, _signal| {
        second_calls_for_hook.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { None })
    });

    let request_count = Arc::new(AtomicUsize::new(0));
    let request_count_for_stream = Arc::clone(&request_count);
    let stream_fn = stream_fn_with(move |_model, _context, _options| {
        let index = request_count_for_stream.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            done_stream_with_reason(
                create_assistant_tool_use_message(vec![tool_call("tool-1", "noop")]),
                DoneReason::ToolUse,
            )
        } else {
            done_stream(create_assistant_message("done"))
        }
    });
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![noop_tool()]),
            ..Default::default()
        }),
        prepare_next_turn_with_context: Some(first),
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .unwrap();

    let second_slot = Arc::new(Mutex::new(Some(second)));
    let agent_for_listener = agent.clone();
    let _subscription = agent.subscribe(listener(move |event, _signal| {
        if event.kind() == "turn_end"
            && let Some(replacement) = second_slot.lock().unwrap().take()
        {
            agent_for_listener.set_prepare_next_turn_with_context(Some(replacement));
        }
        Box::pin(async {})
    }));

    agent.prompt_text("start").await.unwrap();

    assert_eq!(request_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        first_calls.load(Ordering::SeqCst),
        0,
        "the hook replaced during turn 1 must not be called"
    );
    assert_eq!(
        second_calls.load(Ordering::SeqCst),
        1,
        "the replacement hook must be read at the next turn"
    );
}

/// 修复回归：`timestamp ??= message.timestamp` 的 nullish 语义。`null` 会被后续 system 消息
/// 覆盖；最终仍是 `undefined`（最后一条 system 消息缺少 `timestamp`）且没有工具时结果才是
/// `undefined`。对照 TS（已用 runnable reference 验证）：
/// `[null, missing] → undefined`、`[missing, null] → timestamp 0`、`[null, 5] → 5`。
#[tokio::test]
async fn custom_system_timestamps_follow_nullish_coalescing() {
    fn custom(content: &str, timestamp: Option<JsonValue>) -> AgentMessage {
        let mut object = JsonObject::new();
        object.insert("role".to_string(), serde_json::json!("system"));
        object.insert("content".to_string(), serde_json::json!(content));
        if let Some(timestamp) = timestamp {
            object.insert("timestamp".to_string(), timestamp);
        }
        AgentMessage::Custom(Box::new(object))
    }

    fn agent_with(messages: Vec<AgentMessage>) -> Agent {
        Agent::new(AgentOptions {
            initial_state: Some(AgentInitialState {
                messages: Some(messages),
                ..Default::default()
            }),
            stream_fn: Some(unused_stream_function()),
            ..Default::default()
        })
        .unwrap()
    }

    // [null, missing] → timestamp 最终被覆盖回 undefined → 没有工具 → undefined。
    let null_then_missing = agent_with(vec![
        custom("a", Some(serde_json::json!(null))),
        custom("b", None),
    ]);
    assert_eq!(null_then_missing.system_prompt(), "");
    null_then_missing.reset().unwrap();
    assert!(null_then_missing.messages().is_empty());

    // [missing, null] → null 不是 undefined → 返回 timestamp 0。
    let missing_then_null = agent_with(vec![
        custom("a", None),
        custom("b", Some(serde_json::json!(null))),
    ]);
    assert_eq!(missing_then_null.system_prompt(), "a\n\nb");
    missing_then_null.reset().unwrap();
    let messages = missing_then_null.messages();
    assert_eq!(messages.len(), 1);
    let baseline = system_of(&messages[0]).expect("replayed system message");
    assert_eq!(baseline.timestamp, 0);

    // [null, 5] → null 被后续数值覆盖 → timestamp 5。
    let null_then_five = agent_with(vec![
        custom("a", Some(serde_json::json!(null))),
        custom("b", Some(serde_json::json!(5))),
    ]);
    assert_eq!(null_then_five.system_prompt(), "a\n\nb");
    null_then_five.reset().unwrap();
    let messages = null_then_five.messages();
    assert_eq!(messages.len(), 1);
    let baseline = system_of(&messages[0]).expect("replayed system message");
    assert_eq!(baseline.timestamp, 5);
}

// ---------------------------------------------------------------------------
// 回归：run 生命周期（review 发现 1 / 2，无对应 TS 测试）
// ---------------------------------------------------------------------------

/// TS 的 `prompt()` 是 async 函数：调用后同步执行到第一个 `await`，因此 `agent_start` 的
/// listener 在 `prompt()` 返回前已经运行（到各自的第一个 `await`），`isStreaming` 也已置位。
/// Rust 侧在调用线程上把 run 执行到同一个挂起点（见 `Agent::begin_run`）。
#[tokio::test]
async fn dispatches_agent_start_before_prompt_returns() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(done_stream_fn(create_assistant_message("ok"))),
        ..Default::default()
    })
    .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&events);
    let _subscription = agent.subscribe(listener(move |event, _signal| {
        recorded.lock().unwrap().push(event.kind().to_string());
        Box::pin(async {})
    }));

    let prompt = agent.prompt_text("hello");
    // TS 探针（runnable reference）在同一位置观察到：`prompt()` 同步返回时只有 `agent_start`。
    assert_eq!(
        *events.lock().unwrap(),
        vec!["agent_start".to_string()],
        "agent_start listeners must run during prompt(), and nothing else yet"
    );
    assert!(agent.is_streaming());
    assert!(agent.signal().is_some());

    prompt.await.unwrap();
    assert!(!agent.is_streaming());
    assert!(agent.signal().is_none());
}

/// 运行时在 run 任务首次被 poll 之前关闭时，任务 future 被 Drop；`finally { finishRun() }`
/// 的等价清理仍必须发生（`activeRun` 清空、`isStreaming` 复位），否则 Agent 会永久停在
/// processing 状态，`waitForIdle()` 也永远不会 resolve。
#[test]
fn cleans_up_run_state_when_the_runtime_drops_the_task_before_it_runs() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = Arc::clone(&events);
    let in_block = Arc::clone(&events);
    let agent = runtime.block_on(async move {
        let agent = Agent::new(AgentOptions {
            stream_fn: Some(done_stream_fn(create_assistant_message("ok"))),
            ..Default::default()
        })
        .unwrap();
        let _subscription = agent.subscribe(listener(move |event, _signal| {
            recorded.lock().unwrap().push(event.kind().to_string());
            Box::pin(async {})
        }));

        // 同步前缀只执行到 agent_start；返回的 future 被丢弃不会中断 run，run 任务仍在
        // 当前线程运行时的队列里。
        let prompt = agent.prompt_text("hello");
        assert_eq!(*in_block.lock().unwrap(), vec!["agent_start".to_string()]);
        assert!(agent.is_streaming());
        drop(prompt);
        agent
    });

    // 运行时 shutdown 时 Drop 掉从未被再次 poll 的任务 future（没有 turn_start）。
    drop(runtime);

    assert_eq!(
        *events.lock().unwrap(),
        vec!["agent_start".to_string()],
        "the spawned task must never have resumed"
    );
    assert!(!agent.is_streaming(), "run state must be cleaned up");
    assert!(
        agent.signal().is_none(),
        "activeRun must be cleared so the agent is usable again"
    );
}
