//! `test/harness-callers.test.ts` 的 Rust 端口：谁可以调用工具。
//!
//! 模型只被提供 `callers` 含 `model` 的工具；工具只能嵌套调用 `callers` 含 `tools` 的工具；
//! `modelTools` 进一步收窄模型可见集合，但不影响工具可调用集合；`addTools` 也扩到
//! `modelTools`。

#![allow(clippy::await_holding_lock)]

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use kiana_ai::providers::faux::FauxResponseStep;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, JsonObject, Message, StopReason,
    TextContent, TextKind, UserContentBlock,
};
use kiana_chord::context::Context;
use kiana_durable::Session;
use kiana_durable::harness::agent::AGENT_DOC;
use kiana_durable::harness::types::{
    AgentChange, Conversation as ConversationTrait, ModelToolChange, NestedToolExecutionResult,
    NestedToolOptions, ToolCaller, ToolChange, ToolControl, ToolExecuteFn, ToolExecutionResult,
    ToolRegistration,
};
use kiana_durable::types::{BoxError, DocTarget, DocumentReader, JsonValue};

use chat_support::{ChatSetup, context, open_chat};
use harness_support::{add_tool, text_input, tool as base_tool};

fn memory() -> Arc<dyn kiana_durable::types::Storage> {
    Arc::new(kiana_durable::storage::memory::MemoryStorage::new())
        as Arc<dyn kiana_durable::types::Storage>
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn text_output(text: &str) -> Vec<UserContentBlock> {
    vec![UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_owned(),
        text_signature: None,
    })]
}

/// `tool(name, extra)`：工具默认执行返回 `${name} ran`。
fn tool(name: &str, callers: Option<Vec<ToolCaller>>, run_name: Option<&str>) -> ToolRegistration {
    let mut registration = base_tool(name);
    registration.callers = callers;
    let output_text = format!("{} ran", run_name.unwrap_or(name));
    registration.execute = Some(Arc::new(
        move |_arguments: JsonValue,
              _api: Arc<dyn kiana_durable::harness::types::ToolExecutionApi>,
              _context: Arc<dyn Context>| {
            let output = text_output(&output_text);
            Box::pin(async move {
                Ok(ToolExecutionResult {
                    output: Some(output),
                    ..Default::default()
                })
            })
                as Pin<
                    Box<
                        dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>>
                            + Send,
                    >,
                >
        },
    ) as ToolExecuteFn);
    registration
}

fn assistant_with_call(name: &str) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(
            kiana_ai::providers::faux::faux_tool_call_with(
                name,
                JsonObject::new(),
                kiana_ai::providers::faux::FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                },
            ),
        )],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        usage: Default::default(),
        stop_reason: StopReason::ToolUse,
        timestamp: 2,
        ..Default::default()
    }
}

fn done() -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "done".to_owned(),
            text_signature: None,
        })],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        usage: Default::default(),
        stop_reason: StopReason::Stop,
        timestamp: 3,
        ..Default::default()
    }
}

/// 一个把 `names` 逐个作为嵌套调用执行并记录结果的道具工具。
struct Probe {
    registration: ToolRegistration,
    received: Arc<Mutex<HashMap<String, NestedToolExecutionResult>>>,
}

fn prober(names: &[&str]) -> Probe {
    let received: Arc<Mutex<HashMap<String, NestedToolExecutionResult>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    let sink = Arc::clone(&received);
    let execute: ToolExecuteFn = Arc::new(move |_arguments, api, context| {
        let names = names.clone();
        let sink = Arc::clone(&sink);
        Box::pin(async move {
            for name in names {
                let result = Arc::clone(&api)
                    .execute_tool(
                        &name,
                        JsonObject::new(),
                        &*context,
                        NestedToolOptions::default(),
                    )
                    .await?;
                lock(&sink).insert(name, result);
            }
            Ok(ToolExecutionResult::default())
        })
            as Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    let mut registration = base_tool("probe");
    registration.tool.description = "probe".to_owned();
    registration.execute = Some(execute);
    Probe {
        registration,
        received,
    }
}

async fn offered(root: &Arc<dyn ConversationTrait>) -> Vec<String> {
    root.agent(&*context())
        .await
        .expect("the agent")
        .tools
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect()
}

async fn callable(root: &Arc<dyn ConversationTrait>) -> Vec<String> {
    root.agent(&*context())
        .await
        .expect("the agent")
        .callable
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect()
}

fn setup_with(tools: Vec<ToolRegistration>) -> ChatSetup {
    let setup = ChatSetup::new();
    for each in tools {
        let _ = add_tool(&setup.registry, each);
    }
    setup
}

async fn run_once(root: &Arc<dyn ConversationTrait>) {
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    submission
        .wait(&*context())
        .await
        .expect("the submission settles");
}

#[tokio::test(flavor = "multi_thread")]
async fn offers_the_model_only_tools_it_may_call_and_lets_tools_call_only_tools_they_may_call() {
    let script = tool("script", Some(vec![ToolCaller::Model]), None);
    let hidden = tool("hidden", Some(vec![ToolCaller::Tools]), None);
    let both = tool("both", None, None);
    let probe = prober(&["script", "hidden", "both"]);
    let setup = setup_with(vec![script, hidden, both, probe.registration.clone()]);
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(assistant_with_call("probe")),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    assert_eq!(offered(&root).await, vec!["script", "both", "probe"]);
    assert_eq!(callable(&root).await, vec!["hidden", "both", "probe"]);
    run_once(&root).await;
    let received = probe.received.lock().unwrap();
    let script_result = received.get("script").expect("script result");
    assert_eq!(
        script_result
            .diagnostics
            .first()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some("tool_unavailable")
    );
    let hidden_result = received.get("hidden").expect("hidden result");
    assert!(!hidden_result.is_error);
    assert_eq!(
        hidden_result.structured_output,
        Some(JsonValue::String("hidden ran".to_owned()))
    );
    let both_result = received.get("both").expect("both result");
    assert!(!both_result.is_error);
    assert_eq!(
        both_result.structured_output,
        Some(JsonValue::String("both ran".to_owned()))
    );
    drop(received);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn narrows_what_the_model_is_offered_with_model_tools_leaving_the_rest_callable_by_tools() {
    let read = tool("read", None, None);
    let bash = tool("bash", None, None);
    let probe = prober(&["read", "bash"]);
    let setup = setup_with(vec![read, bash.clone(), probe.registration.clone()]);
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(assistant_with_call("probe")),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    root.configure(
        AgentChange {
            model_tools: Some(ModelToolChange::Set(vec![probe.registration.clone()])),
            ..Default::default()
        },
        &*context(),
    )
    .await
    .expect("configure");
    assert_eq!(offered(&root).await, vec!["probe"]);
    assert_eq!(callable(&root).await, vec!["read", "bash", "probe"]);
    run_once(&root).await;
    let received = probe.received.lock().unwrap();
    assert!(!received.get("read").expect("read result").is_error);
    assert!(!received.get("bash").expect("bash result").is_error);
    drop(received);

    root.configure(
        AgentChange {
            model_tools: Some(ModelToolChange::Edit {
                remove: Some(vec![bash.clone()]),
                add: None,
            }),
            ..Default::default()
        },
        &*context(),
    )
    .await
    .expect("configure");
    assert_eq!(offered(&root).await, vec!["read", "probe"]);
    let snapshot = harness
        .snapshot(&DocTarget::conversation(&AGENT_DOC, root.id()), &*context())
        .await
        .expect("snapshot")
        .expect("the agent doc");
    assert_eq!(
        snapshot.get("modelTools"),
        Some(&serde_json::json!({"remove": ["bash"]}))
    );
    root.configure(
        AgentChange {
            model_tools: Some(ModelToolChange::Clear),
            ..Default::default()
        },
        &*context(),
    )
    .await
    .expect("configure");
    assert_eq!(offered(&root).await, vec!["read", "bash", "probe"]);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn never_offers_a_tool_that_is_not_enabled_nor_lets_tools_call_it() {
    let read = tool("read", None, None);
    let bash = tool("bash", None, None);
    let probe = prober(&["bash"]);
    let setup = setup_with(vec![read, bash.clone(), probe.registration.clone()]);
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(assistant_with_call("probe")),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    root.configure(
        AgentChange {
            tools: Some(ToolChange::Remove {
                remove: vec![bash.clone()],
            }),
            model_tools: Some(ModelToolChange::Set(vec![bash, probe.registration.clone()])),
            ..Default::default()
        },
        &*context(),
    )
    .await
    .expect("configure");
    assert_eq!(offered(&root).await, vec!["probe"]);
    assert_eq!(callable(&root).await, vec!["read", "probe"]);
    run_once(&root).await;
    let received = probe.received.lock().unwrap();
    assert_eq!(
        received
            .get("bash")
            .expect("bash result")
            .diagnostics
            .first()
            .and_then(|diagnostic| diagnostic.code.as_deref()),
        Some("tool_unavailable")
    );
    drop(received);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_a_model_issued_call_of_a_tool_it_was_not_offered_as_unavailable() {
    let read = tool("read", None, None);
    let bash = tool("bash", None, None);
    let setup = setup_with(vec![read.clone(), bash]);
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(assistant_with_call("bash")),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    root.configure(
        AgentChange {
            model_tools: Some(ModelToolChange::Set(vec![read])),
            ..Default::default()
        },
        &*context(),
    )
    .await
    .expect("configure");
    run_once(&root).await;
    let page = root
        .entries(Default::default(), 100, None, &*context())
        .await
        .expect("entries");
    let result = page
        .items
        .iter()
        .find(|entry| entry.kind == "kiana.tool-result")
        .expect("the tool result");
    let model = result.model.as_ref().expect("the result message");
    let serialized = match model.first().expect("one message") {
        Message::ToolResult(result) => serde_json::to_string(result).expect("json"),
        other => panic!("unexpected message: {other:?}"),
    };
    assert!(
        serialized.contains("Tool bash is not available"),
        "{serialized}"
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn applies_add_tools_to_model_tools_too_so_the_model_is_offered_the_added_tools() {
    let extra = tool("extra", None, None);
    let grow = {
        let mut registration = base_tool("grow");
        registration.execute = Some(Arc::new(
            |_arguments: JsonValue,
             _api: Arc<dyn kiana_durable::harness::types::ToolExecutionApi>,
             _context: Arc<dyn Context>| {
                Box::pin(async {
                    Ok(ToolExecutionResult {
                        output: Some(Vec::new()),
                        control: Some(ToolControl {
                            add_tools: Some(vec!["extra".to_owned()]),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                })
                    as Pin<
                        Box<
                            dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>>
                                + Send,
                        >,
                    >
            },
        ) as ToolExecuteFn);
        registration
    };
    let setup = setup_with(vec![extra, grow.clone()]);
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(assistant_with_call("grow")),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    root.configure(
        AgentChange {
            model_tools: Some(ModelToolChange::Set(vec![grow])),
            ..Default::default()
        },
        &*context(),
    )
    .await
    .expect("configure");
    run_once(&root).await;
    let snapshot = harness
        .snapshot(&DocTarget::conversation(&AGENT_DOC, root.id()), &*context())
        .await
        .expect("snapshot")
        .expect("the agent doc");
    assert_eq!(
        snapshot.get("modelTools"),
        Some(&serde_json::json!(["grow", "extra"]))
    );
    assert_eq!(offered(&root).await, vec!["grow", "extra"]);
    harness.close(&*context()).await;
}
