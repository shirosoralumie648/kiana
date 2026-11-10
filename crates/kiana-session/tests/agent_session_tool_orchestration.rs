//! `test/suite/agent-session-tool-orchestration.test.ts` 的 Rust 移植。
//!
//! 未移植：`registers codemode and tool_search inactive until they are named`——两个扩展位于
//! `kiana-codemode` crate（kiana-session 不依赖它），属于该单元的集成测试范围。

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use indexmap::IndexMap;
use kiana_agent_core::types::{AgentMessage, AgentToolError, BoxFuture};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message,
};
use kiana_ai::types::{Message, StopReason, TextContent, TextKind, Tool, UserContentBlock};
use kiana_ext_api::tool_types::{
    PrepareLoadoutFn, ToolDefinitionExecute, ToolExposure, ToolLoadoutChanges,
};
use kiana_ext_api::types::ExtensionApi;
use kiana_session::agent_session::PromptOptions;
use kiana_session_core::session_manager::SessionEntry;
use serde_json::json;

struct EchoExecute;

impl ToolDefinitionExecute for EchoExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
        _ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> BoxFuture<
        'a,
        Result<kiana_agent_core::types::AgentToolResult<serde_json::Value>, AgentToolError>,
    > {
        Box::pin(async move {
            let text = params
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Ok(tool_result(format!("echo: {text}")))
        })
    }
}

struct HelperExecute;

impl ToolDefinitionExecute for HelperExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
        _ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> BoxFuture<
        'a,
        Result<kiana_agent_core::types::AgentToolResult<serde_json::Value>, AgentToolError>,
    > {
        Box::pin(async move { Ok(tool_result("helped".to_string())) })
    }
}

struct RunToolsExecute;

impl ToolDefinitionExecute for RunToolsExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
        ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> BoxFuture<
        'a,
        Result<kiana_agent_core::types::AgentToolResult<serde_json::Value>, AgentToolError>,
    > {
        Box::pin(async move {
            let helper = ctx.execute_tool("helper", json!({}), None).await;
            let echo = ctx.execute_tool("echo", json!({"text": "hi"}), None).await;
            let self_call = ctx.execute_tool("run_tools", json!({}), None).await;
            let text = [helper, echo, self_call]
                .iter()
                .map(|outcome| {
                    outcome
                        .result
                        .content
                        .first()
                        .and_then(|block| match block {
                            UserContentBlock::Text(text) => Some(text.text.clone()),
                            _ => None,
                        })
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(" | ");
            Ok(tool_result(text))
        })
    }
}

fn tool_result(text: String) -> kiana_agent_core::types::AgentToolResult<serde_json::Value> {
    kiana_agent_core::types::AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text,
            text_signature: None,
        })],
        details: serde_json::Value::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

fn tool_definition(
    name: &str,
    exposure: Option<ToolExposure>,
    prepare_loadout: Option<PrepareLoadoutFn>,
    execute: Arc<dyn ToolDefinitionExecute>,
) -> kiana_extensions::ToolDefinition {
    kiana_extensions::ToolDefinition {
        name: name.to_string(),
        label: name.to_string(),
        description: name.to_string(),
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: json!({"type": "object", "properties": {}}),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        exposure,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: None,
        prepare_loadout,
        execution_mode: None,
        execute,
        render_call: None,
        render_result: None,
    }
}

fn orchestrator_extension(kiana: Arc<dyn ExtensionApi>) {
    kiana.register_tool(tool_definition("echo", None, None, Arc::new(EchoExecute)));
    kiana.register_tool(tool_definition(
        "helper",
        Some(ToolExposure::Codemode),
        None,
        Arc::new(HelperExecute),
    ));
    let prepare_loadout: PrepareLoadoutFn = Arc::new(|loadout| {
        let callable: Vec<String> = loadout
            .callable()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        let mut descriptions = IndexMap::new();
        descriptions.insert(
            "run_tools".to_string(),
            format!("Runs tools: {}", callable.join(", ")),
        );
        descriptions.insert(
            "echo".to_string(),
            "Echo text (also callable from run_tools).".to_string(),
        );
        Some(ToolLoadoutChanges {
            descriptions: Some(descriptions),
            hidden_declarations: Some(vec!["echo".to_string()]),
        })
    });
    kiana.register_tool(tool_definition(
        "run_tools",
        Some(ToolExposure::ModelOnly),
        Some(prepare_loadout),
        Arc::new(RunToolsExecute),
    ));
}

fn tool_text(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Standard(Message::ToolResult(result)) => result
            .content
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// TS: "supports tools that call other tools under any name through the extension API"
#[tokio::test]
async fn supports_tools_that_call_other_tools_through_the_extension_api() {
    run(async {
        let tool_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&tool_calls);
        let harness = create_harness(HarnessOptions {
            initial_active_tool_names: Some(Vec::new()),
            extension_factories: vec![
                factory(orchestrator_extension),
                factory(move |kiana| {
                    let sink = Arc::clone(&sink);
                    let _ = kiana.on(
                        kiana_ext_api::events::ExtensionEventKind::ToolCall,
                        standard(move |event, _ctx| {
                            let sink = Arc::clone(&sink);
                            Box::pin(async move {
                                if let kiana_ext_api::events::ExtensionEvent::ToolCall(
                                    kiana_ext_api::events::ToolCallEvent::Custom(call),
                                ) = &event
                                {
                                    sink.lock().unwrap().push(format!(
                                        "{}:{}",
                                        call.tool_name,
                                        call.parent_tool_call_id.as_deref().unwrap_or("top")
                                    ));
                                }
                                Ok(None)
                            })
                        }),
                    );
                }),
            ],
            ..Default::default()
        })
        .await;
        harness.session.bind_extensions(Default::default()).await;

        assert_eq!(
            harness.session.get_active_tool_names(),
            vec!["echo".to_string(), "run_tools".to_string()]
        );
        assert_eq!(
            harness.session.get_callable_tool_names(),
            vec!["echo".to_string(), "helper".to_string()]
        );
        let find = |name: &str| {
            harness
                .agent
                .tools()
                .into_iter()
                .find(|tool| tool.name == name)
        };
        assert_eq!(
            find("run_tools").expect("run_tools").description,
            "Runs tools: echo, helper"
        );
        assert_eq!(
            find("echo").expect("echo").description,
            "Echo text (also callable from run_tools)."
        );

        let request_tools: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = Arc::clone(&request_tools);
        harness.set_responses(vec![
            FauxResponseStep::message_factory(move |input| {
                let request_sink = Arc::clone(&request_sink);
                let tools =
                    kiana_ai::utils::transcript::get_current_tools(&input.context.0.messages);
                async move {
                    request_sink
                        .lock()
                        .unwrap()
                        .push(tools.into_iter().map(|tool: Tool| tool.name).collect());
                    faux_assistant_message(
                        kiana_ai::providers::faux::faux_tool_call("run_tools", IndexMap::new()),
                        FauxAssistantMessageOptions {
                            stop_reason: Some(StopReason::ToolUse),
                            ..Default::default()
                        },
                    )
                }
            }),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("go".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(
            request_tools.lock().unwrap()[0],
            vec!["run_tools".to_string()]
        );
        let result = harness
            .session
            .messages()
            .into_iter()
            .find(|message| message.role() == "toolResult")
            .expect("tool result");
        assert_eq!(
            tool_text(&result),
            "helped | echo: hi | Tool run_tools not found"
        );
        let parent = match &result {
            AgentMessage::Standard(Message::ToolResult(result)) => result.tool_call_id.clone(),
            _ => unreachable!(),
        };
        assert_eq!(
            *tool_calls.lock().unwrap(),
            vec![
                "run_tools:top".to_string(),
                format!("helper:{parent}"),
                format!("echo:{parent}"),
            ]
        );
        let AgentMessage::Standard(Message::ToolResult(result)) = &result else {
            unreachable!()
        };
        let nested = result.nested_calls.clone().expect("nested calls");
        let calls: Vec<(String, String, String)> = nested
            .calls
            .iter()
            .map(|call| {
                (
                    call.get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap()
                        .to_string(),
                    call.get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap()
                        .to_string(),
                    call.get("status")
                        .and_then(serde_json::Value::as_str)
                        .unwrap()
                        .to_string(),
                )
            })
            .collect();
        assert_eq!(
            calls,
            vec![
                (
                    format!("{parent}/1"),
                    "helper".to_string(),
                    "ok".to_string()
                ),
                (format!("{parent}/2"), "echo".to_string(), "ok".to_string()),
                (
                    format!("{parent}/3"),
                    "run_tools".to_string(),
                    "error".to_string()
                ),
            ]
        );
        let persisted = harness
            .session_manager()
            .get_branch(None)
            .into_iter()
            .find_map(|entry| match entry {
                SessionEntry::Message(message) if message.message.role() == "toolResult" => {
                    match message.message {
                        AgentMessage::Standard(Message::ToolResult(result)) => Some(result),
                        _ => None,
                    }
                }
                _ => None,
            })
            .expect("persisted tool result");
        assert_eq!(persisted.nested_calls, Some(nested));
        harness.cleanup();
    })
    .await;
}

/// TS: "leaves results without nested calls unchanged"
#[tokio::test]
async fn leaves_results_without_nested_calls_unchanged() {
    run(async {
        let harness = create_harness(HarnessOptions {
            initial_active_tool_names: Some(Vec::new()),
            extension_factories: vec![factory(orchestrator_extension)],
            ..Default::default()
        })
        .await;
        harness.session.bind_extensions(Default::default()).await;
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                kiana_ai::providers::faux::faux_tool_call("echo", {
                    let mut args = IndexMap::new();
                    args.insert("text".to_string(), json!("x"));
                    args
                }),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("go".to_string(), PromptOptions::default())
            .await
            .unwrap();
        let result = harness
            .session
            .messages()
            .into_iter()
            .find(|message| message.role() == "toolResult")
            .expect("tool result");
        let AgentMessage::Standard(Message::ToolResult(result)) = &result else {
            unreachable!()
        };
        assert!(result.nested_calls.is_none());
        harness.cleanup();
    })
    .await;
}
