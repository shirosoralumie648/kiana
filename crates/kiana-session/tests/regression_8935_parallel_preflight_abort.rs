//! `test/suite/regressions/8935-parallel-preflight-abort.test.ts` 的 Rust 移植。

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::types::{
    AgentMessage, AgentTool, AgentToolExecute, AgentToolResult, AgentToolUpdateCallback,
};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxContent, FauxResponseStep, faux_assistant_message,
    faux_tool_call_with,
};
use kiana_ai::types::{AssistantContentBlock, StopReason, UserContentBlock};
use kiana_ext_api::events::ExtensionEventKind;
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions};
use serde_json::json;

struct ExternalWriteTool {
    executions: Arc<Mutex<Vec<String>>>,
}

impl AgentToolExecute for ExternalWriteTool {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<AgentToolResult<serde_json::Value>, kiana_agent_core::types::AgentToolError>,
    > {
        Box::pin(async move {
            let value = params
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            self.executions.lock().unwrap().push(value.clone());
            Ok(AgentToolResult {
                content: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text: value.clone(),
                    text_signature: None,
                })],
                details: json!({ "value": value }),
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn external_write_tool(executions: Arc<Mutex<Vec<String>>>) -> AgentTool {
    AgentTool {
        name: "external_write".to_string(),
        label: "External write".to_string(),
        description: "Perform an external write".to_string(),
        parameters: json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"]
        }),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(ExternalWriteTool { executions }),
    }
}

fn event_input_value(event: &kiana_ext_api::events::ToolCallEvent) -> String {
    match event {
        kiana_ext_api::events::ToolCallEvent::Custom(event) => event
            .input
            .get("value")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// TS: "does not start prepared tools after a later preflight aborts"
#[tokio::test]
async fn does_not_start_prepared_tools_after_a_later_preflight_aborts() {
    run(async {
        let executions: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let preflights: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let result_hooks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![external_write_tool(Arc::clone(&executions))]),
            extension_factories: vec![factory({
                let preflights = Arc::clone(&preflights);
                let result_hooks = Arc::clone(&result_hooks);
                move |kiana| {
                    let preflights = Arc::clone(&preflights);
                    let _ = kiana.on(
                        ExtensionEventKind::ToolCall,
                        standard(move |event, ctx| {
                            let preflights = Arc::clone(&preflights);
                            Box::pin(async move {
                                let kiana_ext_api::events::ExtensionEvent::ToolCall(event) = event
                                else {
                                    return Ok(None);
                                };
                                let value = event_input_value(&event);
                                preflights.lock().unwrap().push(value.clone());
                                if value == "second" {
                                    ctx.abort();
                                }
                                Ok(None)
                            })
                        }),
                    );
                    let result_hooks = Arc::clone(&result_hooks);
                    let _ = kiana.on(
                        ExtensionEventKind::ToolResult,
                        standard(move |event, _ctx| {
                            let result_hooks = Arc::clone(&result_hooks);
                            Box::pin(async move {
                                let kiana_ext_api::events::ExtensionEvent::ToolResult(
                                    kiana_ext_api::events::ToolResultEvent::Custom(event),
                                ) = event
                                else {
                                    return Ok(None);
                                };
                                result_hooks
                                    .lock()
                                    .unwrap()
                                    .push(event.tool_call_id.clone());
                                Ok(None)
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        let mut first_args = kiana_ai::types::JsonObject::new();
        first_args.insert("value".to_string(), json!("first"));
        let mut second_args = kiana_ai::types::JsonObject::new();
        second_args.insert("value".to_string(), json!("second"));
        harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxContent::Blocks(vec![
                AssistantContentBlock::ToolCall(faux_tool_call_with(
                    "external_write",
                    first_args,
                    Default::default(),
                )),
                AssistantContentBlock::ToolCall(faux_tool_call_with(
                    "external_write",
                    second_args,
                    Default::default(),
                )),
            ]),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
        ))]);

        harness
            .session
            .prompt("run both writes".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(*preflights.lock().unwrap(), vec!["first", "second"]);
        assert!(executions.lock().unwrap().is_empty());
        assert!(result_hooks.lock().unwrap().is_empty());

        let starts: Vec<String> = harness
            .events_of_type("tool_execution_start")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::ToolExecutionStart { tool_call_id, .. } => Some(tool_call_id),
                _ => None,
            })
            .collect();
        let ends: Vec<(String, bool)> = harness
            .events_of_type("tool_execution_end")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::ToolExecutionEnd {
                    tool_call_id,
                    is_error,
                    ..
                } => Some((tool_call_id, is_error)),
                _ => None,
            })
            .collect();
        assert_eq!(starts.len(), 2);
        assert_eq!(ends.len(), 2);
        assert!(ends.iter().all(|(_, is_error)| *is_error));
        let mut end_ids: Vec<String> = ends.iter().map(|(id, _)| id.clone()).collect();
        end_ids.sort();
        let mut start_ids = starts.clone();
        start_ids.sort();
        assert_eq!(end_ids, start_ids);

        let tool_results: Vec<_> = harness
            .session
            .messages()
            .into_iter()
            .filter_map(|message| match message {
                AgentMessage::Standard(kiana_ai::types::Message::ToolResult(result)) => {
                    Some(result)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            tool_results
                .iter()
                .map(|result| result.tool_call_id.clone())
                .collect::<Vec<_>>(),
            starts
        );
        for result in &tool_results {
            assert_eq!(
                get_message_text(&AgentMessage::Standard(
                    kiana_ai::types::Message::ToolResult(result.clone())
                )),
                "Operation aborted"
            );
        }
        harness.cleanup();
    })
    .await;
}
