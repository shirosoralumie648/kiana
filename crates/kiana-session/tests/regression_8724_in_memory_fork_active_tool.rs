//! regression #8724: in-memory fork during an active tool turn does not append the aborted turn
//! to the replacement session.
#![allow(clippy::arc_with_non_send_sync)]

mod common;
mod session_support;

use std::sync::Arc;

use common::{HarnessOptions, create_harness, faux_tool_response, run};
use kiana_agent_core::types::{
    AgentTool, AgentToolExecute, AgentToolResult, AgentToolUpdateCallback,
};
use kiana_ai::providers::faux::{FauxResponseStep, faux_assistant_message, faux_tool_call};
use kiana_ai::types::JsonValue;
use kiana_session::agent_session::PromptOptions;
use kiana_session::agent_session_runtime::{
    AgentSessionRuntime, AgentSessionRuntimeError, CreateAgentSessionRuntimeFactory,
    CreateAgentSessionRuntimeFactoryOptions, CreateAgentSessionRuntimeResult,
    create_agent_session_from_services,
};
use kiana_session::agent_session_services::{
    AgentSessionServices, CreateAgentSessionFromServicesOptions,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct BlockingTool {
    started: Arc<tokio::sync::Notify>,
}

impl AgentToolExecute for BlockingTool {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        signal: Option<&'a CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<AgentToolResult<JsonValue>, kiana_agent_core::types::AgentToolError>,
    > {
        let started = Arc::clone(&self.started);
        Box::pin(async move {
            started.notify_one();
            if let Some(signal) = signal {
                signal.cancelled().await;
            }
            Ok(AgentToolResult {
                content: vec![kiana_ai::types::UserContentBlock::Text(
                    kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text: "tool aborted".to_string(),
                        text_signature: None,
                    },
                )],
                details: JsonValue::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn blocking_tool() -> (AgentTool, Arc<tokio::sync::Notify>) {
    let started = Arc::new(tokio::sync::Notify::new());
    let tool = AgentTool {
        name: "block".to_string(),
        label: "Block".to_string(),
        description: "Wait until aborted".to_string(),
        parameters: json!({"type": "object", "properties": {}}),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(BlockingTool {
            started: Arc::clone(&started),
        }),
    };
    (tool, started)
}

#[tokio::test]
async fn does_not_append_the_aborted_turn_to_the_replacement_session() {
    run(async {
        let (tool, tool_started) = blocking_tool();
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![tool]),
            ..Default::default()
        })
        .await;
        let cwd = harness.temp_dir.path().to_string_lossy().into_owned();
        let services = AgentSessionServices {
            cwd: cwd.clone(),
            agent_dir: cwd.clone(),
            model_runtime: harness.session.model_runtime().clone(),
            settings_manager: harness.settings_manager.clone(),
            resource_loader: harness.session.resource_loader(),
            diagnostics: Vec::new(),
        };
        let model = harness.get_model(None).expect("faux model");
        let factory: CreateAgentSessionRuntimeFactory = {
            let services = services.clone();
            Arc::new(move |input: CreateAgentSessionRuntimeFactoryOptions| {
                let services = services.clone();
                let model = model.clone();
                Box::pin(async move {
                    let created =
                        create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                            services: services.clone(),
                            session_manager: input.session_manager,
                            session_start_event: input.session_start_event,
                            model: Some(model),
                            thinking_level: None,
                            scoped_models: Vec::new(),
                            tools: None,
                            exclude_tools: None,
                            no_tools: Some(kiana_session::sdk::NoTools::All),
                            custom_tools: Vec::new(),

                            loop_guard: None,
                            budget_config: None,
                        })
                        .await
                        .map_err(|error| AgentSessionRuntimeError::Other(error.to_string()))?;
                    Ok(CreateAgentSessionRuntimeResult {
                        session: created.session,
                        extensions_result: created.extensions_result,
                        model_fallback_message: created.model_fallback_message,
                        diagnostics: Vec::new(),
                        services,
                    })
                })
            })
        };
        let runtime =
            AgentSessionRuntime::new(harness.session.clone(), services, factory, Vec::new(), None);

        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message("first response", Default::default())),
            faux_tool_response(
                faux_tool_call("block", kiana_ai::types::JsonObject::new()),
                kiana_ai::types::StopReason::ToolUse,
            ),
            FauxResponseStep::Message(faux_assistant_message(
                "unused after abort",
                Default::default(),
            )),
        ]);
        runtime
            .session()
            .prompt("first prompt".to_string(), PromptOptions::default())
            .await
            .expect("first prompt");
        let first_user_entry_id = runtime.session().get_user_messages_for_forking()[0]
            .0
            .clone();

        let outgoing = runtime.session();
        let outgoing_prompt = tokio::task::spawn_local({
            let outgoing = outgoing.clone();
            async move {
                outgoing
                    .prompt("start blocking tool".to_string(), PromptOptions::default())
                    .await
            }
        });
        tool_started.notified().await;
        let fork_result = runtime
            .fork(&first_user_entry_id, None)
            .await
            .expect("fork");
        let prompt_result = outgoing_prompt.await.expect("join");
        let _ = prompt_result;
        runtime.session().bind_extensions(Default::default()).await;

        assert!(!fork_result.cancelled);
        assert_eq!(fork_result.selected_text.as_deref(), Some("first prompt"));
        let roles: Vec<String> = runtime
            .session()
            .messages()
            .iter()
            .map(|message| message.role().to_string())
            .collect();
        assert_eq!(roles, vec!["system"]);
        let persisted: Vec<String> = runtime
            .session()
            .session_manager()
            .get_entries()
            .into_iter()
            .filter_map(|entry| match entry {
                kiana_session_core::session_manager::SessionEntry::Message(entry) => {
                    Some(entry.message.role().to_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(persisted, vec!["system"]);

        let captured: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        harness.set_responses(vec![FauxResponseStep::factory(move |input| {
            let sink = Arc::clone(&sink);
            async move {
                *sink.lock().unwrap() = input
                    .context
                    .as_context()
                    .messages
                    .iter()
                    .map(|message| message.role().to_string())
                    .collect();
                Ok(faux_assistant_message("next response", Default::default()))
            }
        })]);
        runtime
            .session()
            .prompt("next prompt".to_string(), PromptOptions::default())
            .await
            .expect("next prompt");
        assert_eq!(
            captured.lock().unwrap().clone(),
            vec![
                "system".to_string(),
                "system".to_string(),
                "user".to_string()
            ]
        );
        runtime.dispose().await;
        harness.cleanup();
    })
    .await;
}
