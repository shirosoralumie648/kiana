//! `test/suite/agent-session-prompt.test.ts` 的 Rust 移植（14 例）。
//! 其余用例（图像归一化、技能/模板展开、压缩中 prompt 等）见 `agent_session_prompt_extra.rs`。
#![allow(clippy::too_many_arguments)]

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::types::{
    AgentTool, AgentToolExecute, AgentToolResult, AgentToolUpdateCallback,
};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use kiana_ai::types::{AssistantContentBlock, JsonValue, StopReason, UserContent};
use kiana_ext_api::events::{ExtensionEvent, ExtensionEventKind, StreamingBehavior};
use kiana_ext_api::types::RegisterCommandOptions;
use kiana_session::agent_session::PromptOptions;
use kiana_session_core::messages::{CustomMessage, CustomRole};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// 测试工具：把参数里的 `text`/`value` 回显（无延迟）。
struct EchoTool {
    name: String,
    argument: String,
    runs: Arc<Mutex<Vec<String>>>,
}

impl AgentToolExecute for EchoTool {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: JsonValue,
        _signal: Option<&'a CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<AgentToolResult<JsonValue>, kiana_agent_core::types::AgentToolError>,
    > {
        let name = self.name.clone();
        let argument = self.argument.clone();
        let runs = Arc::clone(&self.runs);
        Box::pin(async move {
            let value = params
                .get(&argument)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            runs.lock().unwrap().push(value.clone());
            let text = if name == "echo" {
                format!("echo:{value}")
            } else {
                format!("{name}:{value}")
            };
            Ok(AgentToolResult {
                content: vec![kiana_ai::types::UserContentBlock::Text(
                    kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text,
                        text_signature: None,
                    },
                )],
                details: json!({ argument: value }),
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn tool_args(pairs: &[(&str, &str)]) -> indexmap::IndexMap<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), json!(value)))
        .collect()
}

fn make_tool(name: &str, argument: &str, runs: Arc<Mutex<Vec<String>>>) -> AgentTool {
    AgentTool {
        name: name.to_string(),
        label: name.to_string(),
        description: format!("{name} tool"),
        parameters: json!({"type": "object", "properties": {argument: {"type": "string"}}}),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(EchoTool {
            name: name.to_string(),
            argument: argument.to_string(),
            runs,
        }),
    }
}

/// TS: "prompts while idle and records a single text response"
#[tokio::test]
async fn prompts_while_idle_and_records_a_single_text_response() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("hello")]);
        harness
            .session
            .prompt("hi".to_string(), PromptOptions::default())
            .await
            .unwrap();
        let roles: Vec<String> = harness
            .session
            .messages()
            .iter()
            .map(|message| message.role().to_string())
            .collect();
        assert_eq!(roles, vec!["system", "user", "assistant"]);
        assert_eq!(get_message_text(&harness.session.messages()[1]), "hi");
        assert_eq!(harness.get_pending_response_count(), 0);
        harness.cleanup();
    })
    .await;
}

/// TS: "handles a tool call turn and waits for the follow-up LLM response"
#[tokio::test]
async fn handles_a_tool_call_turn_and_waits_for_the_follow_up_llm_response() {
    run(async {
        let runs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![make_tool("echo", "text", Arc::clone(&runs))]),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                faux_tool_call("echo", tool_args(&[("text", "hello")])),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("start".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(*runs.lock().unwrap(), vec!["hello".to_string()]);
        let roles: Vec<String> = harness
            .session
            .messages()
            .iter()
            .map(|message| message.role().to_string())
            .collect();
        assert_eq!(
            roles,
            vec!["system", "user", "assistant", "toolResult", "assistant"]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "executes multiple tool calls from one response and continues with a single follow-up response"
#[tokio::test]
async fn executes_multiple_tool_calls_from_one_response() {
    run(async {
        let slow_runs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let fast_runs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![
                make_tool("slow", "value", Arc::clone(&slow_runs)),
                make_tool("fast", "value", Arc::clone(&fast_runs)),
            ]),
            ..Default::default()
        })
        .await;
        let slow = faux_tool_call("slow", tool_args(&[("value", "a")]));
        let fast = faux_tool_call("fast", tool_args(&[("value", "b")]));
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                vec![
                    AssistantContentBlock::ToolCall(slow.clone()),
                    AssistantContentBlock::ToolCall(fast.clone()),
                ],
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("start".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(slow_runs.lock().unwrap().len(), 1);
        assert_eq!(fast_runs.lock().unwrap().len(), 1);
        let tool_results = harness
            .session
            .messages()
            .iter()
            .filter(|message| message.role() == "toolResult")
            .count();
        assert_eq!(tool_results, 2);
        harness.cleanup();
    })
    .await;
}

/// TS: "dispatches extension commands without consuming a provider response"
#[tokio::test]
async fn dispatches_extension_commands_without_consuming_a_provider_response() {
    run(async {
        let command_runs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let runs = Arc::clone(&command_runs);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                let runs = Arc::clone(&runs);
                kiana.register_command(
                    "testcmd",
                    RegisterCommandOptions {
                        description: Some("Test command".to_string()),
                        get_argument_completions: None,
                        handler: Arc::new(move |args, _ctx| {
                            let runs = Arc::clone(&runs);
                            Box::pin(async move {
                                runs.lock().unwrap().push(args);
                            })
                        }),
                    },
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("should stay queued")]);
        harness
            .session
            .prompt("/testcmd hello world".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(
            *command_runs.lock().unwrap(),
            vec!["hello world".to_string()]
        );
        assert!(harness.session.messages().is_empty());
        assert_eq!(harness.get_pending_response_count(), 1);
        harness.cleanup();
    })
    .await;
}

/// TS: "sendUserMessage while idle triggers a turn"
#[tokio::test]
async fn send_user_message_while_idle_triggers_a_turn() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("response")]);
        harness
            .session
            .send_user_message(UserContent::Text("from extension".to_string()), None)
            .await
            .unwrap();
        let roles: Vec<String> = harness
            .session
            .messages()
            .iter()
            .map(|message| message.role().to_string())
            .collect();
        assert_eq!(roles, vec!["system", "user", "assistant"]);
        assert_eq!(
            get_message_text(&harness.session.messages()[1]),
            "from extension"
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "does not report streamingBehavior to input handlers while idle"
#[tokio::test]
async fn does_not_report_streaming_behavior_to_input_handlers_while_idle() {
    run(async {
        let behaviors: Arc<Mutex<Vec<Option<StreamingBehavior>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&behaviors);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                let sink = Arc::clone(&sink);
                let _ = kiana.on(
                    ExtensionEventKind::Input,
                    standard(move |event, _ctx| {
                        let sink = Arc::clone(&sink);
                        Box::pin(async move {
                            if let ExtensionEvent::Input(input) = &event {
                                sink.lock().unwrap().push(input.streaming_behavior);
                            }
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("ok")]);
        harness
            .session
            .prompt(
                "idle".to_string(),
                PromptOptions {
                    streaming_behavior: Some(StreamingBehavior::FollowUp),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(*behaviors.lock().unwrap(), vec![None]);
        harness.cleanup();
    })
    .await;
}

/// TS: "reports streamingBehavior to input handlers while streaming"
#[tokio::test]
async fn reports_streaming_behavior_to_input_handlers_while_streaming() {
    run(async {
        let behaviors: Arc<Mutex<Vec<Option<StreamingBehavior>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&behaviors);
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            extension_factories: vec![factory(move |kiana| {
                let sink = Arc::clone(&sink);
                let _ = kiana.on(
                    ExtensionEventKind::Input,
                    standard(move |event, _ctx| {
                        let sink = Arc::clone(&sink);
                        Box::pin(async move {
                            if let ExtensionEvent::Input(input) = &event {
                                sink.lock().unwrap().push(input.streaming_behavior);
                            }
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            tool_call_message_for("wait"),
            faux_text_response("done"),
        ]);
        let session = harness.session.clone();
        let events = Arc::clone(&harness.events);
        let handle = tokio::task::spawn_local(async move {
            session
                .prompt("start".to_string(), PromptOptions::default())
                .await
        });
        wait_for_event(&events, "tool_execution_start").await;
        harness
            .session
            .prompt(
                "queued".to_string(),
                PromptOptions {
                    streaming_behavior: Some(StreamingBehavior::FollowUp),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            *behaviors.lock().unwrap(),
            vec![None, Some(StreamingBehavior::FollowUp)]
        );
        notify.notify_one();
        handle.await.unwrap().unwrap();
        harness.cleanup();
    })
    .await;
}

fn tool_call_message_for(name: &str) -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message(
        faux_tool_call(name, Default::default()),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
}

/// TS: "throws when prompted during streaming without a streamingBehavior"
#[tokio::test]
async fn throws_when_prompted_during_streaming_without_a_streaming_behavior() {
    run(async {
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![tool_call_message_for("wait"), faux_text_response("done")]);
        let session = harness.session.clone();
        let events = Arc::clone(&harness.events);
        let handle = tokio::task::spawn_local(async move {
            session.prompt("start".to_string(), PromptOptions::default()).await
        });
        wait_for_event(&events, "tool_execution_start").await;
        let error = harness
            .session
            .prompt("queued".to_string(), PromptOptions::default())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message."
        );
        notify.notify_one();
        handle.await.unwrap().unwrap();
        harness.cleanup();
    })
    .await;
}

/// TS: "throws when prompting without configured auth"
#[tokio::test]
async fn throws_when_prompting_without_configured_auth() {
    run(async {
        let harness = create_harness(HarnessOptions {
            with_configured_auth: Some(false),
            ..Default::default()
        })
        .await;
        let error = harness
            .session
            .prompt("hi".to_string(), PromptOptions::default())
            .await
            .unwrap_err();
        assert!(error.to_string().starts_with("No API key found for faux."));
        harness.cleanup();
    })
    .await;
}

/// 回归（评审 finding #5）：TS 直接 await `checkAuth`，rejection 会带着原始错误消息
/// reject `prompt()`/`setModel()`；Rust 修复前把它当成“未配置”，改抛 No API key 文案。
#[tokio::test]
async fn propagates_check_auth_rejection_from_prompt_and_set_model() {
    run(async {
        let harness = create_harness(HarnessOptions {
            check_auth_error: Some("API key auth check failed for faux".to_string()),
            ..Default::default()
        })
        .await;
        let prompt_error = harness
            .session
            .prompt("hi".to_string(), PromptOptions::default())
            .await
            .unwrap_err();
        // 原样传播 checkAuth 的错误（kiana-ai 的 ModelsError 文本），而不是 No API key 文案。
        assert!(
            prompt_error
                .to_string()
                .starts_with("API key auth failed for provider faux"),
            "unexpected error: {prompt_error}"
        );

        let model = harness.get_model(None).expect("model");
        let set_error = harness
            .session
            .set_model(
                model,
                kiana_session::agent_session::ModelMutationOptions::default(),
            )
            .await
            .unwrap_err();
        assert!(
            set_error
                .to_string()
                .starts_with("API key auth failed for provider faux"),
            "unexpected error: {set_error}"
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "includes a custom message sent without triggerTurn while the run starts in its first request"
#[tokio::test]
async fn includes_a_custom_message_sent_without_trigger_turn_in_the_first_request() {
    run(async {
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(|kiana| {
                let api = Arc::clone(&kiana);
                let _ = kiana.on(
                    ExtensionEventKind::BeforeAgentStart,
                    standard(move |_event, _ctx| {
                        let api = Arc::clone(&api);
                        Box::pin(async move {
                            api.send_message(kiana_ext_api::types::SendMessageOptions {
                                message: kiana_ext_api::types::CustomMessagePayload {
                                    custom_type: "note".to_string(),
                                    content: UserContent::Text("context from handler".to_string()),
                                    display: true,
                                    details: None,
                                },
                                trigger_turn: Some(false),
                                deliver_as: None,
                            });
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        let seen: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let sink = Arc::clone(&seen);
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let sink = Arc::clone(&sink);
            let messages = input.context.as_context().messages.clone();
            async move {
                *sink.lock().unwrap() = serde_json::to_string(&messages)
                    .unwrap_or_default()
                    .contains("context from handler");
                faux_assistant_message("done", Default::default())
            }
        })]);
        harness
            .session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert!(*seen.lock().unwrap());
        harness.cleanup();
    })
    .await;
}

/// 保持 `CustomMessage`/`CustomRole` 导入被使用（其它用例引用同类构造）。
#[allow(dead_code)]
fn _custom(custom_type: &str) -> CustomMessage {
    CustomMessage {
        role: CustomRole::Custom,
        custom_type: custom_type.to_string(),
        content: json!("x"),
        display: true,
        details: None,
        timestamp: 0.0,
    }
}

/// TS: "subscribe returns an unsubscribe function for this listener"（agent-session.ts#subscribe）
#[tokio::test]
async fn subscribe_unsubscribe_removes_only_that_listener() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("one"), faux_text_response("two")]);
        let first = Arc::new(Mutex::new(Vec::<String>::new()));
        let second = Arc::new(Mutex::new(Vec::<String>::new()));
        let first_sink = Arc::clone(&first);
        let second_sink = Arc::clone(&second);
        let unsubscribe = harness.session.subscribe(Arc::new(move |event| {
            first_sink.lock().unwrap().push(format!("{event:?}"));
        }));
        let _keep = harness.session.subscribe(Arc::new(move |event| {
            second_sink.lock().unwrap().push(format!("{event:?}"));
        }));
        harness
            .session
            .prompt("first".to_string(), PromptOptions::default())
            .await
            .unwrap();
        unsubscribe();
        let first_len = first.lock().unwrap().len();
        let second_len = second.lock().unwrap().len();
        assert!(first_len > 0);
        harness
            .session
            .prompt("second".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(first.lock().unwrap().len(), first_len);
        assert!(second.lock().unwrap().len() > second_len);
        harness.cleanup();
    })
    .await;
}

/// TS: `sendUserMessage` 默认 `expandPromptTemplates: false`（不派发扩展命令、不展开模板）。
#[tokio::test]
async fn send_user_message_does_not_expand_prompt_templates_by_default() {
    run(async {
        let command_runs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let runs = Arc::clone(&command_runs);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                let runs = Arc::clone(&runs);
                kiana.register_command(
                    "testcmd",
                    RegisterCommandOptions {
                        description: Some("Test command".to_string()),
                        get_argument_completions: None,
                        handler: Arc::new(move |args, _ctx| {
                            runs.lock().unwrap().push(args);
                            Box::pin(async {})
                        }),
                    },
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("response")]);
        harness
            .session
            .send_user_message(UserContent::Text("/testcmd literal".to_string()), None)
            .await
            .unwrap();
        assert!(command_runs.lock().unwrap().is_empty());
        assert_eq!(harness.faux.state().call_count(), 1);
        assert!(get_user_texts(&harness).contains(&"/testcmd literal".to_string()));
        harness.cleanup();
    })
    .await;
}

/// TS: 绑定 `abortHandler` 时 `ctx.abort()` 委托给它（agent-session.ts:3502-3508）。
#[tokio::test]
async fn extension_ctx_abort_uses_bound_abort_handler() {
    run(async {
        let handler_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let handler_calls = Arc::clone(&handler_calls);
                move |kiana| {
                    let handler_calls = Arc::clone(&handler_calls);
                    let _ = kiana.on(
                        ExtensionEventKind::SessionStart,
                        standard(move |_event, _ctx| {
                            let handler_calls = Arc::clone(&handler_calls);
                            Box::pin(async move {
                                let _ = handler_calls;
                                Ok(None)
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        // 通过命令上下文调用 abort（命令在 idle 时可执行）。
        let harness2 = create_harness_with_abort_command().await;
        let calls = Arc::clone(&handler_calls);
        harness2
            .session
            .bind_extensions(kiana_session::agent_session::ExtensionBindings {
                abort_handler: Some(Arc::new(move || {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                })),
                ..Default::default()
            })
            .await;
        harness2
            .session
            .prompt("/abortcmd".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(handler_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        harness2.cleanup();
        harness.cleanup();
    })
    .await;
}

/// 辅助：注册一个调用 `ctx.abort()` 的命令。
async fn create_harness_with_abort_command() -> Harness {
    create_harness(HarnessOptions {
        extension_factories: vec![factory(|kiana| {
            kiana.register_command(
                "abortcmd",
                RegisterCommandOptions {
                    description: Some("Abort".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(|_args, ctx| {
                        ctx.abort();
                        Box::pin(async {})
                    }),
                },
            );
        })],
        ..Default::default()
    })
    .await
}

/// TS: 系统提示包含加载的上下文文件（AGENTS.md 等）。
#[tokio::test]
async fn system_prompt_includes_loaded_context_files() {
    run(async {
        let harness = create_harness(HarnessOptions {
            agents_files: Some(vec![kiana_ext_api::system_prompt::ContextFile {
                path: "/project/AGENTS.md".to_string(),
                content: "PROJECT RULE 42".to_string(),
            }]),
            ..Default::default()
        })
        .await;
        let prompt = harness.session.system_prompt();
        assert!(prompt.contains("PROJECT RULE 42"), "prompt: {prompt}");
        assert!(prompt.contains("<project_instructions"), "prompt: {prompt}");
        harness.cleanup();
    })
    .await;
}
