//! `test/suite/agent-session-bash-persistence.test.ts` 的 Rust 移植。

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::types::{
    AgentMessage, AgentTool, AgentToolExecute, AgentToolResult, AgentToolUpdateCallback,
};
use kiana_ai::types::{JsonValue, StopReason, TextContent, TextKind, UserContentBlock};
use kiana_ext_api::tool_result::{BashExecOptions, BashExecResult, BashOperations, BashResult};
use kiana_session::agent_session::ExecuteBashOptions;
use kiana_session_core::messages::{CustomMessage, CustomRole};
use tokio_util::sync::CancellationToken;

type ExecFuture<'a> = kiana_agent_core::types::BoxFuture<
    'a,
    Result<BashExecResult, Box<dyn std::error::Error + Send + Sync>>,
>;

fn text_block(text: &str) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })
}

fn record(harness: &Harness, command: &str, output: &str) {
    harness.session.record_bash_result(
        command.to_string(),
        &BashResult {
            output: output.to_string(),
            exit_code: Some(0),
            cancelled: false,
            truncated: false,
            full_output_path: None,
            execution_status: None,
        },
        None,
    );
}

/// 可控的 `BashOperations`（对应 TS 的 `createControlledBashOperations`）。
struct ControlledBash {
    invocations: Mutex<Vec<(Option<CancellationToken>, tokio::sync::oneshot::Sender<()>)>>,
}

impl BashOperations for ControlledBash {
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _cwd: &'a str,
        options: BashExecOptions<'a>,
    ) -> ExecFuture<'a> {
        let signal = options.signal.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.invocations.lock().unwrap().push((signal, sender));
        Box::pin(async move {
            let _ = receiver.await;
            Ok(BashExecResult {
                exit_code: Some(0),
                execution_status: None,
            })
        })
    }
}

struct EchoTool;

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
        Box::pin(async move {
            let text = params
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            Ok(AgentToolResult {
                content: vec![text_block(&format!("echo:{text}"))],
                details: serde_json::json!({ "text": text }),
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn echo_tool() -> AgentTool {
    AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo text back".to_string(),
        parameters: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(EchoTool),
    }
}

fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> kiana_ai::types::ToolCall {
    kiana_ai::types::ToolCall {
        kind_tool_call: kiana_ai::types::ToolCallKind::ToolCall,
        id: id.to_string(),
        name: name.to_string(),
        arguments: arguments
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    }
}

fn entry_types(harness: &Harness) -> Vec<String> {
    harness
        .session_entries()
        .iter()
        .map(|entry| entry.kind().to_string())
        .collect()
}

/// TS: "records bash results immediately while idle"
#[tokio::test]
async fn records_bash_results_immediately_while_idle() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        record(&harness, "echo hi", "hi");
        assert!(!harness.session.has_pending_bash_messages());
        assert_eq!(
            harness
                .session
                .messages()
                .last()
                .map(|message| message.role()),
            Some("bashExecution")
        );
        assert!(entry_types(&harness).contains(&"message".to_string()));
        harness.cleanup();
    })
    .await;
}

/// TS: "defers bash results while streaming and flushes them before the next prompt"
#[tokio::test]
async fn defers_bash_results_while_streaming_and_flushes_before_next_prompt() {
    run(async {
        let (wait_tool, release) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            faux_tool_response(
                tool_call("call-1", "wait", serde_json::json!({})),
                StopReason::ToolUse,
            ),
            faux_text_response("done"),
            faux_text_response("after flush"),
        ]);
        let saw_start = Arc::new(tokio::sync::Notify::new());
        let sink = Arc::clone(&saw_start);
        let _unsubscribe = harness.session.subscribe(Arc::new(move |event| {
            if matches!(
                event,
                kiana_session::AgentSessionEvent::ToolExecutionStart { .. }
            ) {
                sink.notify_one();
            }
        }));
        let first_prompt = {
            let session = harness.session.clone();
            tokio::task::spawn_local(async move {
                session
                    .prompt("start".to_string(), Default::default())
                    .await
            })
        };
        saw_start.notified().await;
        record(&harness, "echo hi", "hi");
        assert!(harness.session.has_pending_bash_messages());
        assert!(
            !harness
                .session
                .messages()
                .iter()
                .any(|message| message.role() == "bashExecution")
        );
        release.notify_one();
        first_prompt.await.unwrap().unwrap();
        assert!(!harness.session.has_pending_bash_messages());
        assert!(
            harness
                .session
                .messages()
                .iter()
                .any(|message| message.role() == "bashExecution")
        );
        harness
            .session
            .prompt("next turn".to_string(), Default::default())
            .await
            .unwrap();
        assert!(!harness.session.has_pending_bash_messages());
        assert!(
            harness
                .session
                .messages()
                .iter()
                .any(|message| message.role() == "bashExecution")
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "executes bash commands and records the result"
#[cfg(unix)]
#[tokio::test]
async fn executes_bash_commands_and_records_the_result() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let result = harness
            .session
            .execute_bash("printf 'hello'".to_string(), None, None)
            .await
            .unwrap();
        assert!(result.output.contains("hello"));
        assert_eq!(
            harness
                .session
                .messages()
                .last()
                .map(|message| message.role()),
            Some("bashExecution")
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "cancels running bash commands with abortBash"
#[tokio::test]
async fn cancels_running_bash_commands_with_abort_bash() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        struct AbortingOps;
        impl BashOperations for AbortingOps {
            fn exec<'a>(
                &'a self,
                _command: &'a str,
                _cwd: &'a str,
                options: BashExecOptions<'a>,
            ) -> ExecFuture<'a> {
                let signal = options.signal.clone();
                Box::pin(async move {
                    if let Some(signal) = signal {
                        signal.cancelled().await;
                    }
                    Err("aborted".into())
                })
            }
        }
        let session = harness.session.clone();
        let bash = tokio::task::spawn_local(async move {
            session
                .execute_bash(
                    "sleep".to_string(),
                    None,
                    Some(ExecuteBashOptions {
                        operations: Some(Arc::new(AbortingOps)),
                        ..Default::default()
                    }),
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(harness.session.is_bash_running());
        harness.session.abort_bash();
        let result = bash.await.unwrap().unwrap();
        assert!(result.cancelled);
        assert!(!harness.session.is_bash_running());
        harness.cleanup();
    })
    .await;
}

/// 取消不能吞掉 `aborted|unconfirmed`：`execute_bash` 必须把“命令可能已经执行、
/// 不要自动重试”的结论交给调用方，而不是降级成 `Ok(cancelled)`。
#[tokio::test]
async fn unconfirmed_bash_abort_surfaces_to_the_caller() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        struct UnconfirmedOps;
        impl BashOperations for UnconfirmedOps {
            fn exec<'a>(
                &'a self,
                _command: &'a str,
                _cwd: &'a str,
                options: BashExecOptions<'a>,
            ) -> ExecFuture<'a> {
                let signal = options.signal.clone();
                Box::pin(async move {
                    if let Some(signal) = signal {
                        signal.cancelled().await;
                    }
                    Err("aborted|unconfirmed".into())
                })
            }
        }
        let session = harness.session.clone();
        let bash = tokio::task::spawn_local(async move {
            session
                .execute_bash(
                    "sleep".to_string(),
                    None,
                    Some(ExecuteBashOptions {
                        operations: Some(Arc::new(UnconfirmedOps)),
                        ..Default::default()
                    }),
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(harness.session.is_bash_running());
        harness.session.abort_bash();
        let error = bash
            .await
            .unwrap()
            .expect_err("unconfirmed abort must not be reported as cancelled");
        assert!(
            error.to_string().contains("unconfirmed"),
            "caller must see the unconfirmed signal, got {error}"
        );
        assert!(!harness.session.is_bash_running());
        harness.cleanup();
    })
    .await;
}

/// TS: "keeps newer bash execution tracked when an older execution finishes"
#[tokio::test]
async fn keeps_newer_bash_execution_tracked_when_older_finishes() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let operations = Arc::new(ControlledBash {
            invocations: Mutex::new(Vec::new()),
        });
        let first = {
            let session = harness.session.clone();
            let operations = Arc::clone(&operations);
            tokio::task::spawn_local(async move {
                session
                    .execute_bash(
                        "first".to_string(),
                        None,
                        Some(ExecuteBashOptions {
                            operations: Some(operations),
                            ..Default::default()
                        }),
                    )
                    .await
            })
        };
        let second = {
            let session = harness.session.clone();
            let operations = Arc::clone(&operations);
            tokio::task::spawn_local(async move {
                session
                    .execute_bash(
                        "second".to_string(),
                        None,
                        Some(ExecuteBashOptions {
                            operations: Some(operations),
                            ..Default::default()
                        }),
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        {
            let mut invocations = operations.invocations.lock().unwrap();
            let (_, sender) = invocations.remove(0);
            let _ = sender.send(());
        }
        let first_result = first.await.unwrap().unwrap();
        assert!(!first_result.cancelled);
        assert!(harness.session.is_bash_running());
        harness.session.abort_bash();
        let second_was_aborted = {
            let invocations = operations.invocations.lock().unwrap();
            invocations[0]
                .0
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
        };
        {
            let mut invocations = operations.invocations.lock().unwrap();
            let (_, sender) = invocations.remove(0);
            let _ = sender.send(());
        }
        let second_result = second.await.unwrap().unwrap();
        assert!(second_was_aborted);
        assert!(second_result.cancelled);
        assert!(!harness.session.is_bash_running());
        harness.cleanup();
    })
    .await;
}

/// TS: "aborts all active bash executions"
#[tokio::test]
async fn aborts_all_active_bash_executions() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let operations = Arc::new(ControlledBash {
            invocations: Mutex::new(Vec::new()),
        });
        let spawn = |command: &'static str| {
            let session = harness.session.clone();
            let operations = Arc::clone(&operations);
            tokio::task::spawn_local(async move {
                session
                    .execute_bash(
                        command.to_string(),
                        None,
                        Some(ExecuteBashOptions {
                            operations: Some(operations),
                            ..Default::default()
                        }),
                    )
                    .await
            })
        };
        let first = spawn("first");
        let second = spawn("second");
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        harness.session.abort_bash();
        let aborted: Vec<bool> = {
            let invocations = operations.invocations.lock().unwrap();
            invocations
                .iter()
                .map(|(signal, _)| signal.as_ref().is_some_and(CancellationToken::is_cancelled))
                .collect()
        };
        {
            let mut invocations = operations.invocations.lock().unwrap();
            while let Some((_, sender)) = invocations.pop() {
                let _ = sender.send(());
            }
        }
        let results = (
            first.await.unwrap().unwrap(),
            second.await.unwrap().unwrap(),
        );
        assert_eq!(aborted, vec![true, true]);
        assert!(results.0.cancelled);
        assert!(results.1.cancelled);
        assert!(!harness.session.is_bash_running());
        harness.cleanup();
    })
    .await;
}

/// TS: "persists user, assistant, toolResult, and custom messages in order"
#[tokio::test]
async fn persists_user_assistant_tool_result_and_custom_messages_in_order() {
    run(async {
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![echo_tool()]),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            faux_tool_response(
                tool_call("call-1", "echo", serde_json::json!({ "text": "hello" })),
                StopReason::ToolUse,
            ),
            faux_text_response("done"),
        ]);
        harness
            .session
            .send_custom_message(
                CustomMessage {
                    role: CustomRole::Custom,
                    custom_type: "note".to_string(),
                    content: serde_json::json!("hello"),
                    display: true,
                    details: Some(serde_json::json!({ "a": 1 })),
                    timestamp: now_ms(),
                },
                None,
                None,
            )
            .await
            .unwrap();
        harness
            .session
            .prompt("start".to_string(), Default::default())
            .await
            .unwrap();
        assert_eq!(
            entry_types(&harness),
            vec![
                "custom_message",
                "message",
                "message",
                "message",
                "message",
                "message"
            ]
        );
        let roles: Vec<String> = harness
            .session
            .messages()
            .iter()
            .map(|message| message.role().to_string())
            .collect();
        assert_eq!(
            roles,
            vec![
                "custom",
                "system",
                "user",
                "assistant",
                "toolResult",
                "assistant"
            ]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "does not emit message_end for bash execution messages"
#[tokio::test]
async fn does_not_emit_message_end_for_bash_execution_messages() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let roles = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&roles);
        let _unsubscribe = harness.session.subscribe(Arc::new(move |event| {
            if let kiana_session::AgentSessionEvent::MessageEnd { message } = event {
                sink.lock().unwrap().push(message.role().to_string());
            }
        }));
        record(&harness, "echo hi", "hi");
        assert!(roles.lock().unwrap().is_empty());
        harness.cleanup();
    })
    .await;
}

/// TS: "persists aborted assistant messages"
#[tokio::test]
async fn persists_aborted_assistant_messages() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("x".repeat(20_000))]);
        let saw_update = Arc::new(tokio::sync::Notify::new());
        let sink = Arc::clone(&saw_update);
        let _unsubscribe = harness.session.subscribe(Arc::new(move |event| {
            if matches!(
                event,
                kiana_session::AgentSessionEvent::MessageUpdate { .. }
            ) {
                sink.notify_one();
            }
        }));
        let session = harness.session.clone();
        let prompt = tokio::task::spawn_local(async move {
            session.prompt("hi".to_string(), Default::default()).await
        });
        saw_update.notified().await;
        harness.session.abort().await;
        prompt.await.unwrap().unwrap();
        let entries = harness.session_entries();
        let last = entries.last().expect("last entry");
        assert_eq!(last.kind(), "message");
        if let kiana_session_core::session_manager::SessionEntry::Message(entry) = last {
            assert_eq!(entry.message.role(), "assistant");
            if let AgentMessage::Standard(kiana_ai::types::Message::Assistant(assistant)) =
                &entry.message
            {
                assert_eq!(assistant.stop_reason, StopReason::Aborted);
            }
        }
        harness.cleanup();
    })
    .await;
}

/// TS: "records bash output through custom operations"
#[tokio::test]
async fn records_bash_output_through_custom_operations() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        struct DataOps;
        impl BashOperations for DataOps {
            fn exec<'a>(
                &'a self,
                _command: &'a str,
                _cwd: &'a str,
                options: BashExecOptions<'a>,
            ) -> ExecFuture<'a> {
                (options.on_data)(b"hello from custom ops");
                Box::pin(async {
                    Ok(BashExecResult {
                        exit_code: Some(0),
                        execution_status: None,
                    })
                })
            }
        }
        let result = harness
            .session
            .execute_bash(
                "custom".to_string(),
                None,
                Some(ExecuteBashOptions {
                    operations: Some(Arc::new(DataOps)),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        assert!(result.output.contains("hello from custom ops"));
        assert_eq!(
            harness
                .session
                .messages()
                .last()
                .map(|message| message.role()),
            Some("bashExecution")
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "streams bash output to the callback and session events"
#[tokio::test]
async fn streams_bash_output_to_callback_and_session_events() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let callback_deltas: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        #[allow(clippy::type_complexity)]
        let event_updates: Arc<Mutex<Vec<(Option<String>, String)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&event_updates);
        let _unsubscribe = harness.session.subscribe(Arc::new(move |event| {
            if let kiana_session::AgentSessionEvent::BashExecutionUpdate { id, delta } = event {
                sink.lock().unwrap().push((id.clone(), delta.clone()));
            }
        }));
        struct ChunkOps;
        impl BashOperations for ChunkOps {
            fn exec<'a>(
                &'a self,
                _command: &'a str,
                _cwd: &'a str,
                options: BashExecOptions<'a>,
            ) -> ExecFuture<'a> {
                (options.on_data)(b"hello ");
                (options.on_data)(b"world");
                Box::pin(async {
                    Ok(BashExecResult {
                        exit_code: Some(0),
                        execution_status: None,
                    })
                })
            }
        }
        let deltas = Arc::clone(&callback_deltas);
        harness
            .session
            .execute_bash(
                "custom".to_string(),
                Some(Arc::new(move |delta: &str| {
                    deltas.lock().unwrap().push(delta.to_string());
                })),
                Some(ExecuteBashOptions {
                    id: Some("bash-1".to_string()),
                    operations: Some(Arc::new(ChunkOps)),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        assert_eq!(*callback_deltas.lock().unwrap(), vec!["hello ", "world"]);
        assert_eq!(
            *event_updates.lock().unwrap(),
            vec![
                (Some("bash-1".to_string()), "hello ".to_string()),
                (Some("bash-1".to_string()), "world".to_string())
            ]
        );
        harness.cleanup();
    })
    .await;
}

async fn run_chunks(chunks: Vec<Vec<u8>>) -> (String, String, Option<String>) {
    let harness = create_harness(HarnessOptions::default()).await;
    struct Chunks(Vec<Vec<u8>>);
    impl BashOperations for Chunks {
        fn exec<'a>(
            &'a self,
            _command: &'a str,
            _cwd: &'a str,
            options: BashExecOptions<'a>,
        ) -> ExecFuture<'a> {
            for chunk in &self.0 {
                (options.on_data)(chunk);
            }
            Box::pin(async {
                Ok(BashExecResult {
                    exit_code: Some(0),
                    execution_status: None,
                })
            })
        }
    }
    let deltas: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&deltas);
    let result = harness
        .session
        .execute_bash(
            "custom".to_string(),
            Some(Arc::new(move |delta: &str| {
                sink.lock().unwrap().push(delta.to_string());
            })),
            Some(ExecuteBashOptions {
                operations: Some(Arc::new(Chunks(chunks))),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    let streamed = deltas.lock().unwrap().join("");
    let recorded = harness.session.messages().last().and_then(|message| {
        if message.role() == "bashExecution" {
            match message {
                AgentMessage::Custom(object) => object
                    .get("output")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                _ => None,
            }
        } else {
            None
        }
    });
    harness.cleanup();
    (result.output, streamed, recorded)
}

/// TS: "strips a color reset split inside its parameters"
#[tokio::test]
async fn strips_a_color_reset_split_inside_its_parameters() {
    run(async {
        let (output, streamed, recorded) = run_chunks(vec![
            b"\x1b[31mERROR: file.py:1\x1b[0".to_vec(),
            b"m\n".to_vec(),
        ])
        .await;
        assert_eq!(output, "ERROR: file.py:1\n");
        assert_eq!(streamed, "ERROR: file.py:1\n");
        assert_eq!(recorded.as_deref(), Some("ERROR: file.py:1\n"));
    })
    .await;
}

/// TS: "strips a color code split right after ESC"
#[tokio::test]
async fn strips_a_color_code_split_right_after_esc() {
    run(async {
        let (output, streamed, _) =
            run_chunks(vec![b"before\x1b".to_vec(), b"[32mafter\n".to_vec()]).await;
        assert_eq!(output, "beforeafter\n");
        assert_eq!(streamed, "beforeafter\n");
    })
    .await;
}

/// TS: "strips an OSC sequence split before its terminator"
#[tokio::test]
async fn strips_an_osc_sequence_split_before_its_terminator() {
    run(async {
        let (output, _, _) = run_chunks(vec![
            b"a\x1b]0;window ".to_vec(),
            b"title\x1b".to_vec(),
            b"\\b\n".to_vec(),
        ])
        .await;
        assert_eq!(output, "ab\n");
    })
    .await;
}

/// TS: "flushes an incomplete multi-byte character at the end of output"
#[tokio::test]
async fn flushes_an_incomplete_multi_byte_character_at_the_end_of_output() {
    run(async {
        let full = "é".as_bytes().to_vec();
        let (output, streamed, _) = run_chunks(vec![b"ok".to_vec(), full[..1].to_vec()]).await;
        assert_eq!(output, "ok\u{FFFD}");
        assert_eq!(streamed, "ok\u{FFFD}");
    })
    .await;
}

/// TS: "does not hold back output behind a long unterminated sequence"
#[tokio::test]
async fn does_not_hold_back_output_behind_a_long_unterminated_sequence() {
    run(async {
        let long = "x".repeat(300);
        let (_, streamed, _) = run_chunks(vec![format!("\x1b]{long}").into_bytes()]).await;
        assert_eq!(streamed, format!("]{long}"));
    })
    .await;
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}
