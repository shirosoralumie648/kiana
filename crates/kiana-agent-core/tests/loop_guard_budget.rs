//! agent loop 的 loop guard / budget / context report 集成测试。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_agent_core::agent_loop::{AgentEventSink, AgentLoopError, run_agent_loop};
use kiana_agent_core::budget::{
    BudgetConfig, BudgetController, BudgetStatus, BudgetStopReason, model_cost_known,
};
use kiana_agent_core::context_report::{
    CondensationPhase, ContextReportOptions, build_context_report, observe_condensation,
};
use kiana_agent_core::loop_guard::{LoopDetectorKind, LoopGuard, LoopGuardConfig};
use kiana_agent_core::types::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentTool, AgentToolResult, StreamFn,
};
use kiana_ai::event_stream::create_assistant_message_event_stream;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, Context,
    DoneReason, Message, Model, SimpleStreamOptions, StopReason, TextContent, TextKind, ToolCall,
    ToolCallKind, TranscriptContext, Usage, UserContentBlock, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::{get_current_tools, normalize_context};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// 测试夹具
// ---------------------------------------------------------------------------

fn model() -> Model {
    Model {
        id: "test-model".to_string(),
        name: "Test".to_string(),
        api: "test".to_string(),
        provider: "test".to_string(),
        context_window: Some(100_000),
        max_tokens: Some(8_000),
        ..Model::default()
    }
}

fn usage(total: u64) -> Usage {
    Usage {
        input: total,
        total_tokens: total,
        ..Usage::default()
    }
}

fn text_block(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })
}

fn tool_call(name: &str, arguments: Value) -> AssistantContentBlock {
    let arguments: IndexMap<String, Value> = match arguments {
        Value::Object(map) => map.into_iter().collect(),
        other => panic!("expected object, got {other}"),
    };
    AssistantContentBlock::ToolCall(ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: format!("call-{name}"),
        name: name.to_string(),
        arguments,
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    })
}

fn assistant(blocks: Vec<AssistantContentBlock>, stop: StopReason, total: u64) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: blocks,
        api: "test".to_string(),
        provider: "test".to_string(),
        model: "test-model".to_string(),
        usage: usage(total),
        stop_reason: stop,
        timestamp: 1,
        ..Default::default()
    }
}

fn done_reason(reason: StopReason) -> DoneReason {
    match reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        _ => DoneReason::Stop,
    }
}

fn stream_for(message: AssistantMessage) -> kiana_ai::event_stream::AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let reason = done_reason(message.stop_reason);
    let _ = stream.push(AssistantMessageEvent::Done {
        reason,
        message: Box::new(message.clone()),
    });
    stream.end(message);
    stream
}

type Script = Arc<Mutex<VecDeque<AssistantMessage>>>;
type Requests = Arc<Mutex<Vec<TranscriptContext>>>;

fn scripted_stream(script: Script, requests: Requests) -> StreamFn {
    Arc::new(move |_model, context, _options| {
        requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(context.clone());
        let message = script
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .unwrap_or_else(|| assistant(vec![text_block("done")], StopReason::Stop, 1));
        Box::pin(async move { Ok(stream_for(message)) })
    })
}

fn simple_tool(
    name: &str,
    schema: Value,
    execute: Arc<dyn Fn(Value) -> AgentToolResult<Value> + Send + Sync>,
) -> AgentTool {
    struct Exec {
        f: Arc<dyn Fn(Value) -> AgentToolResult<Value> + Send + Sync>,
    }
    impl kiana_agent_core::types::AgentToolExecute for Exec {
        fn execute<'a>(
            &'a self,
            _tool_call_id: &'a str,
            params: Value,
            _signal: Option<&'a tokio_util::sync::CancellationToken>,
            _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
        ) -> kiana_agent_core::types::BoxFuture<
            'a,
            Result<AgentToolResult<Value>, kiana_agent_core::types::AgentToolError>,
        > {
            let result = (self.f)(params);
            Box::pin(async move { Ok(result) })
        }
    }
    AgentTool {
        name: name.to_string(),
        label: name.to_string(),
        description: format!("{name} tool"),
        parameters: schema,
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(Exec { f: execute }),
    }
}

fn result_text(text: &str) -> AgentToolResult<Value> {
    AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_string(),
            text_signature: None,
        })],
        details: Value::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

fn bash_tool(counter: Arc<AtomicUsize>, outputs: Arc<Mutex<Vec<String>>>) -> AgentTool {
    simple_tool(
        "bash",
        json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"]
        }),
        Arc::new(move |_params| {
            let index = counter.fetch_add(1, Ordering::SeqCst);
            let output = outputs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(index)
                .cloned()
                .unwrap_or_else(|| format!("output {index}"));
            result_text(&output)
        }),
    )
}

fn read_tool() -> AgentTool {
    simple_tool(
        "read",
        json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }),
        Arc::new(|params| {
            let path = params.get("path").and_then(Value::as_str).unwrap_or("file");
            result_text(path)
        }),
    )
}

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text(text.to_string()),
        timestamp: 1,
    }))
}

fn identity_convert() -> Arc<dyn Fn(Vec<AgentMessage>) -> Vec<Message> + Send + Sync> {
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
        model: model(),
        convert_to_llm: identity_convert(),
        ..Default::default()
    }
}

async fn run(
    context: AgentContext,
    config: AgentLoopConfig,
    stream_fn: StreamFn,
) -> (Result<Vec<AgentMessage>, AgentLoopError>, Vec<AgentEvent>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = events.clone();
    let emit: AgentEventSink = Arc::new(move |event| {
        sink_events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
        Box::pin(async {})
    });
    let result = run_agent_loop(
        vec![user_message("go")],
        context,
        config,
        emit,
        None,
        Some(stream_fn),
    )
    .await;
    let events = events
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    (result, events)
}

fn last_message_text(context: &TranscriptContext) -> String {
    context
        .as_context()
        .messages
        .last()
        .map(|message| match message {
            Message::System(system) => kiana_ai::utils::text::get_system_message_text(system),
            Message::User(user) => {
                kiana_ai::utils::text::content_text_user_content(&user.content, "\n")
            }
            Message::Assistant(assistant) => {
                kiana_ai::utils::text::content_text_assistant(&assistant.content, "\n")
            }
            Message::ToolResult(result) => {
                kiana_ai::utils::text::content_text_user(&result.content, "\n")
            }
        })
        .unwrap_or_default()
}

fn tool_result_messages(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|event| match event {
            AgentEvent::MessageEnd {
                message: AgentMessage::Standard(Message::ToolResult(_)),
            } => true,
            _ => false,
        })
        .count()
}

// ---------------------------------------------------------------------------
// loop guard 集成
// ---------------------------------------------------------------------------

#[tokio::test]
async fn consecutive_bash_calls_warn_at_three_and_stop_at_five() {
    let calls = Arc::new(AtomicUsize::new(0));
    let tool = bash_tool(calls.clone(), Arc::new(Mutex::new(Vec::new())));
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![tool]),
    };
    let guard = Arc::new(Mutex::new(LoopGuard::new(LoopGuardConfig::default())));
    let config = base_config().with_loop_guard(guard.clone());

    let script: Script = Arc::new(Mutex::new(
        (0..10)
            .map(|_| {
                assistant(
                    vec![tool_call("bash", json!({"command": "echo hi"}))],
                    StopReason::ToolUse,
                    1,
                )
            })
            .collect(),
    ));
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let stream_fn = scripted_stream(script, requests.clone());

    let (result, events) = run(context, config, stream_fn).await;
    match result {
        Err(AgentLoopError::LoopDetected(detection)) => {
            assert_eq!(detection.kind, LoopDetectorKind::ConsecutiveToolCalls);
            assert_eq!(detection.count, 5);
        }
        other => panic!("expected LoopDetected, got {other:?}"),
    }

    // 5 次 assistant 请求；第 5 次在工具执行前硬停。
    let requests = requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(requests.len(), 5);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert!(tool_result_messages(&events) >= 3);
    // 第 3 次调用产生的 reminder 出现在第 4 次请求末尾。
    assert!(last_message_text(&requests[3]).contains("loop-guard"));
}

#[tokio::test]
async fn twenty_distinct_actions_do_not_warn() {
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![read_tool()]),
    };
    let guard = Arc::new(Mutex::new(LoopGuard::new(LoopGuardConfig::default())));
    let config = base_config().with_loop_guard(guard.clone());
    let script: Script = Arc::new(Mutex::new(
        (0..20)
            .map(|index| {
                assistant(
                    vec![tool_call(
                        "read",
                        json!({"path": format!("src/file-{index}.rs")}),
                    )],
                    StopReason::ToolUse,
                    1,
                )
            })
            .collect(),
    ));
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let stream_fn = scripted_stream(script, requests);
    let (result, _) = run(context, config, stream_fn).await;
    assert!(result.is_ok());
    let guard = guard
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(guard.snapshot().pending_reminder.is_none());
    assert!(guard.snapshot().stopped.is_none());
}

#[tokio::test]
async fn stateful_poll_with_changing_output_does_not_warn() {
    let calls = Arc::new(AtomicUsize::new(0));
    let outputs = Arc::new(Mutex::new(vec![
        "state 1".to_string(),
        "state 2".to_string(),
    ]));
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![bash_tool(calls.clone(), outputs)]),
    };
    let guard = Arc::new(Mutex::new(LoopGuard::new(LoopGuardConfig::default())));
    let config = base_config().with_loop_guard(guard.clone());
    let script: Script = Arc::new(Mutex::new(
        (0..2)
            .map(|_| {
                assistant(
                    vec![tool_call("bash", json!({"command": "poll status"}))],
                    StopReason::ToolUse,
                    1,
                )
            })
            .chain(std::iter::once(assistant(
                vec![text_block("done")],
                StopReason::Stop,
                1,
            )))
            .collect(),
    ));
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let stream_fn = scripted_stream(script, requests);
    let (result, _) = run(context, config, stream_fn).await;
    assert!(result.is_ok());
    let guard = guard
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(guard.snapshot().pending_reminder.is_none());
    assert!(guard.snapshot().stopped.is_none());
}

// ---------------------------------------------------------------------------
// budget 集成
// ---------------------------------------------------------------------------

fn budget_handle(config: BudgetConfig) -> kiana_agent_core::budget::BudgetHandle {
    Arc::new(Mutex::new(BudgetController::new(config, &model())))
}

#[tokio::test]
async fn max_turns_three_produces_exactly_three_assistant_turns_then_turn_capped() {
    let calls = Arc::new(AtomicUsize::new(0));
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![bash_tool(
            calls.clone(),
            Arc::new(Mutex::new(Vec::new())),
        )]),
    };
    let budget = budget_handle(BudgetConfig::new().with_max_turns(3));
    let config = base_config().with_budget(budget.clone());
    let script: Script = Arc::new(Mutex::new(
        (0..10)
            .map(|_| {
                assistant(
                    vec![tool_call("bash", json!({"command": "echo hi"}))],
                    StopReason::ToolUse,
                    1,
                )
            })
            .collect(),
    ));
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let stream_fn = scripted_stream(script, requests.clone());
    let (result, events) = run(context, config, stream_fn).await;
    assert!(result.is_ok());
    assert_eq!(requests.lock().unwrap().len(), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let assistant_messages = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::MessageEnd {
                    message: AgentMessage::Standard(Message::Assistant(_))
                }
            )
        })
        .count();
    assert_eq!(assistant_messages, 3);
    // turn cap 在第 3 个 assistant turn 之后、下一请求之前停止；此时没有新的
    // assistant 消息可写 stop_reason，精确 cap 由控制器快照给出。
    let budget = budget.lock().unwrap();
    assert_eq!(budget.stop_reason(), Some(BudgetStopReason::TurnCapped));
    assert_eq!(
        budget.snapshot().status,
        BudgetStatus::Capped {
            reason: BudgetStopReason::TurnCapped
        }
    );
}

#[tokio::test]
async fn token_budget_forces_final_turn_without_tools() {
    let calls = Arc::new(AtomicUsize::new(0));
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![bash_tool(
            calls.clone(),
            Arc::new(Mutex::new(Vec::new())),
        )]),
    };
    let budget = budget_handle(BudgetConfig::new().with_max_tokens(100));
    let config = base_config().with_budget(budget.clone());
    let script: Script = Arc::new(Mutex::new(VecDeque::from(vec![
        assistant(
            vec![tool_call("bash", json!({"command": "step 1"}))],
            StopReason::ToolUse,
            60,
        ),
        assistant(
            vec![tool_call("bash", json!({"command": "step 2"}))],
            StopReason::ToolUse,
            60,
        ),
        assistant(vec![text_block("final answer")], StopReason::Stop, 5),
    ])));
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let stream_fn = scripted_stream(script, requests.clone());
    let (result, events) = run(context, config, stream_fn).await;
    assert!(result.is_ok());
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    // 50% 提醒在第 2 次请求末尾注入；100% 的 final 指令在第 3 次请求末尾。
    assert!(last_message_text(&requests[1]).contains("budget-warning"));
    assert!(last_message_text(&requests[2]).contains("budget-exhausted"));
    // 最后一轮请求里没有任何可用工具。
    assert!(get_current_tools(requests[2].as_context().messages.as_slice()).is_empty());
    // final turn 的 assistant 消息以精确的 cap 变体收尾；rawStopReason 保留同一原因的
    // camelCase 文本，供只读字符串的消费方使用。
    let last_assistant = events
        .iter()
        .rev()
        .find_map(|event| match event {
            AgentEvent::MessageEnd {
                message: AgentMessage::Standard(Message::Assistant(assistant)),
            } => Some(assistant.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(last_assistant.stop_reason, StopReason::TokenCapped);
    assert_eq!(
        last_assistant.raw_stop_reason.as_deref(),
        Some("tokenCapped")
    );
    assert_eq!(
        budget.lock().unwrap().stop_reason(),
        Some(BudgetStopReason::TokenCapped)
    );
}

#[test]
fn budget_never_overrides_completed_or_failed() {
    let mut controller = BudgetController::new(BudgetConfig::new().with_max_tokens(1), &model());
    let decision = controller.begin_turn();
    // 首次 begin_turn 尚未超限；手工塞入用量后进入 final turn。
    assert!(decision.force_final.is_none());
    controller.record_usage(&usage(10));
    let decision = controller.begin_turn();
    assert_eq!(decision.force_final, Some(BudgetStopReason::TokenCapped));
    // failed / aborted 不被预算覆盖。
    assert_eq!(controller.finalize(StopReason::Error), None);
    assert_eq!(controller.finalize(StopReason::Aborted), None);
    // completed（Stop）才采用 cap。
    assert_eq!(
        controller.finalize(StopReason::Stop),
        Some(BudgetStopReason::TokenCapped)
    );
    assert_eq!(
        controller.stop_reason(),
        Some(BudgetStopReason::TokenCapped)
    );
}

#[test]
fn budget_stop_reasons_map_to_typed_stop_reasons() {
    assert_eq!(
        BudgetStopReason::TurnCapped.stop_reason(),
        StopReason::TurnCapped
    );
    assert_eq!(
        BudgetStopReason::TokenCapped.stop_reason(),
        StopReason::TokenCapped
    );
    assert_eq!(
        BudgetStopReason::CostCapped.stop_reason(),
        StopReason::CostCapped
    );
    assert_eq!(
        BudgetStopReason::TimeCapped.stop_reason(),
        StopReason::TimeCapped
    );
    assert_eq!(BudgetStopReason::TurnCapped.as_str(), "turnCapped");
}

#[test]
fn unknown_price_reports_cost_unknown_instead_of_zero() {
    let free_model = Model {
        cost: Default::default(),
        ..model()
    };
    assert!(!model_cost_known(&free_model));
    let mut controller =
        BudgetController::new(BudgetConfig::new().with_max_cost_usd(1.0), &free_model);
    let decision = controller.begin_turn();
    assert!(decision.cost_unknown);
    assert_eq!(controller.snapshot().status, BudgetStatus::CostUnknown);
    controller.record_usage(&usage(1_000));
    let snapshot = controller.snapshot();
    assert!(snapshot.cost_unknown);
    assert_eq!(snapshot.cost_usd, 0.0);
    assert_eq!(controller.stop_reason(), None);

    let priced = Model {
        cost: kiana_ai::types::ModelCost {
            input: 3.0,
            output: 15.0,
            ..Default::default()
        },
        ..model()
    };
    assert!(model_cost_known(&priced));
    let mut priced_controller =
        BudgetController::new(BudgetConfig::new().with_max_cost_usd(0.000001), &priced);
    let decision = priced_controller.begin_turn();
    assert!(!decision.cost_unknown);
    let mut expensive = usage(1_000_000);
    expensive.cost.total = 0.02;
    priced_controller.record_usage(&expensive);
    let decision = priced_controller.begin_turn();
    assert_eq!(decision.force_final, Some(BudgetStopReason::CostCapped));
}

// ---------------------------------------------------------------------------
// context report
// ---------------------------------------------------------------------------

#[tokio::test]
async fn context_report_matches_provider_reported_input_within_ten_percent() {
    let context = AgentContext {
        messages: vec![
            user_message("first request"),
            AgentMessage::Standard(Message::Assistant(assistant(
                vec![text_block("first answer")],
                StopReason::Stop,
                120,
            ))),
            user_message("second request with some more text"),
        ],
        tools: Some(vec![read_tool()]),
    };
    let report = build_context_report(
        &context,
        &model(),
        &ContextReportOptions::new("stand-in prompt for the next request"),
    );
    assert!(report.total_tokens > 0);
    assert!(!report.prompt.is_empty() || report.prompt_tokens > 0);

    // 用 mock provider 记录同一份「下一次请求」的 provider-reported input。
    let admin = kiana_ai::providers::mock::MockLlmAdmin::new();
    admin.register(
        "report",
        kiana_ai::providers::mock::MockTrajectory::new().then_text("ok"),
    );
    let handle = admin
        .provider("report", Default::default())
        .expect("mock provider");
    use kiana_ai::models::ProviderStreams;
    let mut messages: Vec<Message> = context
        .messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::Standard(message) => Some(message.clone()),
            AgentMessage::Custom(_) => None,
        })
        .collect();
    messages.push(Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text(
            "stand-in prompt for the next request".to_string(),
        ),
        timestamp: 2,
    }));
    let normalized = normalize_context(&Context {
        system_prompt: None,
        messages,
        tools: None,
    });
    let stream =
        handle
            .core
            .stream_simple(&handle.model, &normalized, SimpleStreamOptions::default());
    let provider_message = stream.result().await.expect("mock result");
    let provider_input = provider_message.usage.input;
    assert!(provider_input > 0);
    let difference = (report.total_tokens as f64 - provider_input as f64).abs();
    assert!(
        difference / (provider_input as f64) < 0.10,
        "report {} vs provider {}",
        report.total_tokens,
        provider_input
    );
}

#[tokio::test(start_paused = true)]
async fn manual_condensation_settles_after_token_drop() {
    let mut values = vec![1000u64, 900, 900, 900].into_iter();
    let event = observe_condensation(
        1000,
        move || {
            let value = values.next().unwrap_or(900);
            std::future::ready(Some(value))
        },
        kiana_agent_core::context_report::CondensationConfig::default(),
    )
    .await;
    assert_eq!(event.phase, CondensationPhase::Settled);
    assert_eq!(event.before_tokens, 1000);
    assert_eq!(event.after_tokens, 900);
    assert_eq!(event.saved_tokens, 100);
    assert!(event.is_settled());
}

#[tokio::test(start_paused = true)]
async fn rising_token_counts_reset_the_settle_window() {
    let mut polls = 0u64;
    let event = observe_condensation(
        1000,
        move || {
            polls += 1;
            let value = if polls == 1 { 900 } else { 1200 };
            std::future::ready(Some(value))
        },
        kiana_agent_core::context_report::CondensationConfig::default(),
    )
    .await;
    assert_eq!(
        event.phase,
        CondensationPhase::Failed,
        "回落后又回升不能算收敛"
    );
}

#[tokio::test(start_paused = true)]
async fn manual_condensation_timeout_is_failed() {
    let event = observe_condensation(
        1000,
        || std::future::ready(Some(1000)),
        kiana_agent_core::context_report::CondensationConfig::default(),
    )
    .await;
    assert_eq!(event.phase, CondensationPhase::Failed);
    assert_eq!(event.saved_tokens, 0);
    assert!(event.error.is_some());
}
