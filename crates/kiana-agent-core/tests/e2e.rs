//! `packages/agent/test/e2e.test.ts` 的 Rust 移植（10 个 `it`，416 行）。
//!
//! 每个测试都用 kiana-ai 的 faux provider（`register_faux_provider` + `stream_simple`）驱动
//! 真实 [`Agent`]：`describe("Agent integration with faux provider")` 6 个、
//! `describe("Agent.continue() with faux provider")` 4 个。TS 的 `afterEach` 注销 provider
//! 在这里由 [`Faux`] 的 `Drop` 完成。
//!
//! 不访问网络、不读凭证：faux provider 是纯进程内的脚本化流。

mod util;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_agent_core::agent::{Agent, AgentInitialState, AgentListener, AgentOptions};
use kiana_agent_core::types::{
    AgentEvent, AgentMessage, AgentTool, BoxFuture, StreamFn, ThinkingLevel,
};
use kiana_ai::compat::{FauxProviderRegistration, register_faux_provider};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxContent, FauxModelDefinition, FauxResponseStep, FauxTokenSize,
    FauxToolCallOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_text,
    faux_thinking, faux_tool_call_with,
};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, Model, StopReason,
    ToolResultMessage, ToolResultRole, UserContent, UserContentBlock, UserMessage, UserRole,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use util::{calculate_tool, now_unix_millis, text_block};

// ---------------------------------------------------------------------------
// faux provider 夹具
// ---------------------------------------------------------------------------

/// `registerFauxProvider(...)` + `afterEach` 注销。
struct Faux {
    registration: FauxProviderRegistration,
}

impl Faux {
    fn new(options: RegisterFauxProviderOptions) -> Self {
        Faux {
            registration: register_faux_provider(options),
        }
    }

    fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        self.registration.set_responses(responses);
    }

    /// `faux.getModel()`（无参 → `models[0]`）。
    fn model(&self) -> Model {
        self.registration
            .get_model(None)
            .expect("faux provider has a default model")
            .clone()
    }
}

impl Drop for Faux {
    fn drop(&mut self) {
        self.registration.unregister();
    }
}

/// `streamFn: streamSimple`。
fn stream_fn() -> StreamFn {
    Arc::new(|model, context, options| {
        Box::pin(async move {
            Ok(kiana_ai::compat::stream_simple(
                &model,
                context.as_context(),
                options.unwrap_or_default(),
            ))
        })
    })
}

fn create_agent(
    model: Model,
    system_prompt: &str,
    thinking_level: ThinkingLevel,
    tools: Vec<AgentTool>,
) -> Agent {
    Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some(system_prompt.to_string()),
            model: Some(model),
            thinking_level: Some(thinking_level),
            tools: Some(tools),
            messages: None,
        }),
        stream_fn: Some(stream_fn()),
        ..Default::default()
    })
    .expect("agent construction")
}

/// TS 的 `agent.subscribe((event) => ...)`。
fn listener<F>(f: F) -> AgentListener
where
    F: for<'a> Fn(AgentEvent, &'a CancellationToken) -> BoxFuture<'a, ()> + Send + Sync + 'static,
{
    Arc::new(f)
}

// ---------------------------------------------------------------------------
// 断言辅助
// ---------------------------------------------------------------------------

/// `getTextContent(message)`：拼接文本块（`join("\n")`）。
fn get_text_content(message: &AgentMessage) -> String {
    let blocks: Vec<&str> = match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => assistant
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect(),
        AgentMessage::Standard(Message::ToolResult(result)) => result
            .content
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.as_str()),
                UserContentBlock::Image(_) => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    blocks.join("\n")
}

fn assistant(message: &AgentMessage) -> &AssistantMessage {
    message.as_assistant().expect("expected assistant message")
}

fn user_message(text: &str) -> UserMessage {
    UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(vec![text_block(text)]),
        timestamp: now_unix_millis(),
    }
}

/// `fauxToolCall(name, { expression }, { id })` 的参数对象（`JsonObject` 是 `IndexMap`，
/// 不能直接从 `serde_json::Map` 克隆，后者在 1.0.154 里是独立的 newtype）。
fn tool_arguments(expression: &str) -> kiana_ai::types::JsonObject {
    let mut arguments = kiana_ai::types::JsonObject::new();
    arguments.insert("expression".to_string(), json!(expression));
    arguments
}

fn zero_assistant(model: &Model, content: Vec<AssistantContentBlock>) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        stop_reason: StopReason::Stop,
        timestamp: now_unix_millis(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// describe("Agent integration with faux provider")
// ---------------------------------------------------------------------------

async fn basic_prompt(model: Model) {
    let agent = create_agent(
        model,
        "You are a helpful assistant. Keep your responses concise.",
        ThinkingLevel::Off,
        Vec::new(),
    );

    agent
        .prompt_text("What is 2+2? Answer with just the number.")
        .await
        .expect("prompt");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert_eq!(state.messages.len(), 3);
    assert_eq!(state.messages[0].role(), "system");
    assert_eq!(state.messages[1].role(), "user");
    assert_eq!(state.messages[2].role(), "assistant");
    assert!(get_text_content(&state.messages[2]).contains('4'));
}

#[tokio::test]
async fn handles_a_basic_text_prompt() {
    let faux = Faux::new(RegisterFauxProviderOptions::default());
    faux.set_responses(vec![
        faux_assistant_message("4", FauxAssistantMessageOptions::default()).into(),
    ]);
    basic_prompt(faux.model()).await;
}

/// `(事件类型, 该时刻的 pendingToolCalls)`。
type PendingToolCallEvents = Arc<Mutex<Vec<(String, Vec<String>)>>>;

async fn tool_execution(model: Model) {
    let agent = create_agent(
        model,
        "You are a helpful assistant. Always use the calculator tool for math.",
        ThinkingLevel::Off,
        vec![calculate_tool()],
    );

    // `pendingToolCallsDuringEvents`: 事件派发时刻的 pendingToolCalls 快照。
    let observed: PendingToolCallEvents = Arc::new(Mutex::new(Vec::new()));
    let weak = agent.downgrade();
    let sink = Arc::clone(&observed);
    let _subscription = agent.subscribe(listener(move |event, _signal| {
        let weak = weak.clone();
        let sink = Arc::clone(&sink);
        Box::pin(async move {
            let kind = match &event {
                AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
                AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
                _ => return,
            };
            let ids = weak
                .upgrade()
                .map(|agent| agent.pending_tool_calls().into_iter().collect())
                .unwrap_or_default();
            sink.lock().unwrap().push((kind.to_string(), ids));
        })
    }));

    agent
        .prompt_text("Calculate 123 * 456 using the calculator tool.")
        .await
        .expect("prompt");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert!(
        state.messages.len() >= 4,
        "messages: {:?}",
        state.messages.len()
    );

    let tool_result = state
        .messages
        .iter()
        .find(|message| message.role() == "toolResult")
        .expect("tool result message");
    assert!(get_text_content(tool_result).contains("123 * 456 = 56088"));

    let final_message = state.messages.last().expect("final message");
    // TS: `if (finalMessage.role !== "assistant") throw new Error("Expected final assistant message")`
    assert_eq!(
        final_message.role(),
        "assistant",
        "Expected final assistant message"
    );
    assert!(get_text_content(final_message).contains("56088"));
    assert!(state.pending_tool_calls.is_empty());
    assert_eq!(
        *observed.lock().unwrap(),
        vec![
            (
                "tool_execution_start".to_string(),
                vec!["calc-1".to_string()]
            ),
            ("tool_execution_end".to_string(), Vec::new()),
        ]
    );
}

#[tokio::test]
async fn executes_tools_and_tracks_pending_tool_calls() {
    let faux = Faux::new(RegisterFauxProviderOptions::default());
    faux.set_responses(vec![
        faux_assistant_message(
            FauxContent::Blocks(vec![
                AssistantContentBlock::Text(faux_text("Let me calculate that.")),
                AssistantContentBlock::ToolCall(faux_tool_call_with(
                    "calculate",
                    tool_arguments("123 * 456"),
                    FauxToolCallOptions {
                        id: Some("calc-1".to_string()),
                    },
                )),
            ]),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
        )
        .into(),
        faux_assistant_message(
            "The result is 56088.",
            FauxAssistantMessageOptions::default(),
        )
        .into(),
    ]);
    tool_execution(faux.model()).await;
}

async fn abort_execution(model: Model) {
    let agent = create_agent(
        model,
        "You are a helpful assistant.",
        ThinkingLevel::Off,
        Vec::new(),
    );

    let abort_agent = agent.clone();
    let prompt = agent.prompt_text("Count slowly from 1 to 20.");
    let abort_task = tokio::spawn(async move {
        // TS: setTimeout(() => agent.abort(), 30)
        tokio::time::sleep(Duration::from_millis(30)).await;
        abort_agent.abort();
    });
    prompt.await.expect("prompt");
    abort_task.await.expect("abort task");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert!(state.messages.len() >= 2);

    let last_message = state.messages.last().expect("last message");
    let last_assistant = assistant(last_message);
    assert_eq!(last_assistant.stop_reason, StopReason::Aborted);
    assert!(last_assistant.error_message.is_some());
    assert_eq!(state.error_message, last_assistant.error_message);
}

#[tokio::test]
async fn handles_abort_during_streaming() {
    let faux = Faux::new(RegisterFauxProviderOptions {
        tokens_per_second: Some(20.0),
        token_size: Some(FauxTokenSize {
            min: Some(2),
            max: Some(2),
        }),
        ..Default::default()
    });
    faux.set_responses(vec![
        faux_assistant_message(
            "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen",
            FauxAssistantMessageOptions::default(),
        )
        .into(),
    ]);
    abort_execution(faux.model()).await;
}

async fn state_updates(model: Model) {
    let agent = create_agent(
        model,
        "You are a helpful assistant.",
        ThinkingLevel::Off,
        Vec::new(),
    );

    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let _subscription = agent.subscribe(listener(move |event, _signal| {
        let sink = Arc::clone(&sink);
        Box::pin(async move {
            sink.lock().unwrap().push(event.kind().to_string());
        })
    }));

    agent
        .prompt_text("Count from 1 to 5.")
        .await
        .expect("prompt");

    let events = events.lock().unwrap().clone();
    for expected in [
        "agent_start",
        "turn_start",
        "message_start",
        "message_update",
        "message_end",
        "turn_end",
        "agent_end",
    ] {
        assert!(
            events.iter().any(|event| event == expected),
            "missing {expected}: {events:?}"
        );
    }
    let index = |name: &str| {
        events
            .iter()
            .position(|event| event == name)
            .unwrap_or_else(|| panic!("missing {name}: {events:?}"))
    };
    assert!(index("agent_start") < index("message_start"));
    assert!(index("message_start") < index("message_end"));
    assert!(
        index("message_end")
            < events
                .iter()
                .rposition(|event| event == "agent_end")
                .expect("agent_end")
    );

    let state = agent.state();
    assert!(!state.is_streaming);
    assert_eq!(state.messages.len(), 3);
}

#[tokio::test]
async fn emits_lifecycle_updates_while_streaming() {
    let faux = Faux::new(RegisterFauxProviderOptions {
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    });
    faux.set_responses(vec![
        faux_assistant_message("1 2 3 4 5", FauxAssistantMessageOptions::default()).into(),
    ]);
    state_updates(faux.model()).await;
}

async fn multi_turn_conversation(model: Model) {
    let agent = create_agent(
        model,
        "You are a helpful assistant.",
        ThinkingLevel::Off,
        Vec::new(),
    );

    agent
        .prompt_text("My name is Alice.")
        .await
        .expect("prompt 1");
    assert_eq!(agent.messages().len(), 3);

    agent
        .prompt_text("What is my name?")
        .await
        .expect("prompt 2");
    assert_eq!(agent.messages().len(), 5);

    let last_message = agent.messages()[4].clone();
    // TS: `if (lastMessage.role !== "assistant") throw new Error("Expected assistant message")`
    assert_eq!(
        last_message.role(),
        "assistant",
        "Expected assistant message"
    );
    assert!(
        get_text_content(&last_message)
            .to_lowercase()
            .contains("alice")
    );
}

#[tokio::test]
async fn maintains_context_across_multiple_turns() {
    let faux = Faux::new(RegisterFauxProviderOptions::default());
    faux.set_responses(vec![
        faux_assistant_message(
            "Nice to meet you, Alice.",
            FauxAssistantMessageOptions::default(),
        )
        .into(),
        // `(context) => …`：检查历史里是否还有 Alice。
        FauxResponseStep::message_factory(|input| async move {
            let has_alice = input.context.0.messages.iter().any(|message| {
                match message {
                Message::User(user) => match &user.content {
                    UserContent::Text(text) => text.contains("Alice"),
                    UserContent::Blocks(blocks) => blocks.iter().any(|block| {
                        matches!(block, UserContentBlock::Text(text) if text.text.contains("Alice"))
                    }),
                },
                _ => false,
            }
            });
            faux_assistant_message(
                if has_alice {
                    "Your name is Alice."
                } else {
                    "I do not know your name."
                },
                FauxAssistantMessageOptions::default(),
            )
        }),
    ]);
    multi_turn_conversation(faux.model()).await;
}

#[tokio::test]
async fn preserves_thinking_content_blocks() {
    let faux = Faux::new(RegisterFauxProviderOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-reasoning".to_string(),
            reasoning: Some(true),
            ..Default::default()
        }]),
        ..Default::default()
    });
    faux.set_responses(vec![
        faux_assistant_message(
            FauxContent::Blocks(vec![
                AssistantContentBlock::Thinking(faux_thinking("step by step")),
                AssistantContentBlock::Text(faux_text("4")),
            ]),
            FauxAssistantMessageOptions::default(),
        )
        .into(),
    ]);

    let agent = create_agent(
        faux.model(),
        "You are a helpful assistant.",
        ThinkingLevel::Low,
        Vec::new(),
    );

    agent.prompt_text("What is 2+2?").await.expect("prompt");

    let messages = agent.messages();
    let assistant_message = messages[2].as_assistant().expect("assistant message");
    assert_eq!(
        assistant_message.content,
        vec![
            AssistantContentBlock::Thinking(faux_thinking("step by step")),
            AssistantContentBlock::Text(faux_text("4")),
        ]
    );
}

// ---------------------------------------------------------------------------
// describe("Agent.continue() with faux provider")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn throws_when_no_messages_in_context() {
    let faux = Faux::new(RegisterFauxProviderOptions::default());
    let agent = create_agent(faux.model(), "Test", ThinkingLevel::Off, Vec::new());

    let error = agent
        .continue_()
        .await
        .expect_err("no messages to continue from");
    assert_eq!(error.to_string(), "No messages to continue from");
}

#[tokio::test]
async fn throws_when_last_message_is_assistant() {
    let faux = Faux::new(RegisterFauxProviderOptions::default());
    let model = faux.model();
    let agent = create_agent(model.clone(), "Test", ThinkingLevel::Off, Vec::new());

    let assistant_message = zero_assistant(
        &model,
        vec![AssistantContentBlock::Text(faux_text("Hello"))],
    );
    agent.set_messages(vec![AgentMessage::Standard(Message::Assistant(
        assistant_message,
    ))]);

    let error = agent
        .continue_()
        .await
        .expect_err("last message is assistant");
    assert_eq!(
        error.to_string(),
        "Cannot continue from message role: assistant"
    );
}

#[tokio::test]
async fn continues_and_gets_a_response_when_last_message_is_user() {
    let faux = Faux::new(RegisterFauxProviderOptions::default());
    faux.set_responses(vec![
        faux_assistant_message("HELLO WORLD", FauxAssistantMessageOptions::default()).into(),
    ]);
    let agent = create_agent(
        faux.model(),
        "You are a helpful assistant. Follow instructions exactly.",
        ThinkingLevel::Off,
        Vec::new(),
    );

    agent.set_messages(vec![AgentMessage::Standard(Message::User(user_message(
        "Say exactly: HELLO WORLD",
    )))]);

    agent.continue_().await.expect("continue");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert_eq!(state.messages.len(), 2);
    assert_eq!(state.messages[0].role(), "user");
    assert_eq!(state.messages[1].role(), "assistant");
    assert!(
        get_text_content(&state.messages[1])
            .to_uppercase()
            .contains("HELLO WORLD")
    );
}

#[tokio::test]
async fn continues_and_processes_tool_results() {
    let faux = Faux::new(RegisterFauxProviderOptions::default());
    let model = faux.model();
    faux.set_responses(vec![
        faux_assistant_message("The answer is 8.", FauxAssistantMessageOptions::default()).into(),
    ]);
    let agent = create_agent(
        model.clone(),
        "You are a helpful assistant. After getting a calculation result, state the answer clearly.",
        ThinkingLevel::Off,
        vec![calculate_tool()],
    );

    let user = AgentMessage::Standard(Message::User(user_message("What is 5 + 3?")));
    let assistant_message = zero_assistant(
        &model,
        vec![
            AssistantContentBlock::Text(faux_text("Let me calculate that.")),
            AssistantContentBlock::ToolCall(faux_tool_call_with(
                "calculate",
                tool_arguments("5 + 3"),
                FauxToolCallOptions {
                    id: Some("calc-1".to_string()),
                },
            )),
        ],
    );
    let mut assistant_message = assistant_message;
    assistant_message.stop_reason = StopReason::ToolUse;
    let tool_result = ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "calc-1".to_string(),
        tool_name: "calculate".to_string(),
        content: vec![text_block("5 + 3 = 8")],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: now_unix_millis(),
        duration_ms: None,
    };

    agent.set_messages(vec![
        user,
        AgentMessage::Standard(Message::Assistant(assistant_message)),
        AgentMessage::Standard(Message::ToolResult(tool_result)),
    ]);

    agent.continue_().await.expect("continue");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert!(state.messages.len() >= 4);

    let last_message = state.messages.last().expect("last message");
    assert_eq!(last_message.role(), "assistant");
    assert!(get_text_content(last_message).contains('8'));
}
