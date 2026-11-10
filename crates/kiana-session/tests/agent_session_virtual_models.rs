//! `test/suite/virtual-models.test.ts`（AgentSession 级虚拟模型用例）的 Rust 移植。
//!
//! 语义与 TS 逐条一致：路由原因（user/continuation/retry/direct）、按路由模型计算的上下文
//! 上限、压缩检查/跨 turn 压缩、路由失败以错误 assistant 消息结束 run、路由状态条目。
//!
//! 无法移植的一条：TS 用 `vi.spyOn(sessionManager, "buildSessionProjection")` 统计投影次数；
//! Rust 的 `SessionManager` 是具体类型，没有可监视的 mock 层（见报告）。
#![allow(clippy::arc_with_non_send_sync)]

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::types::{AgentTool, AgentToolError, AgentToolExecute, BoxFuture};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxModelDefinition, FauxResponseInput, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use kiana_ai::types::{Model, ModelThinkingLevel, StopReason, UserContent, UserContentBlock};
use kiana_compaction::compaction::CompactionResult;
use kiana_ext_api::events::{
    BeforeAgentStartEventResult, BoundaryResult, CompactionReason, CustomMessageEntryDraft,
    CustomMessageEntryDraftType, ExtensionEvent, ExtensionEventKind, ExtensionEventResult,
    SessionBeforeCompactResult, SessionBoundaryDraft, SessionEvent,
};
use kiana_ext_api::types::{
    CustomMessagePayload, ExtensionApi, ExtensionContextApi, ExtensionVirtualModel,
};
use kiana_session::agent_session::{AgentSessionEvent, ModelMutationOptions, PromptOptions};
use kiana_session_core::session_manager::SessionEntry;
use kiana_session_core::virtual_models::{
    ModelRoute, ModelRouteReason, ModelRouteRequest, RouteFuture, RouteModelRef,
    VIRTUAL_MODEL_STATE_ENTRY, VirtualModelRouteError,
};
use serde_json::{Value, json};

// ============================================================================
// 工具 / 路由
// ============================================================================

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// TS 的 `echoTool`。
struct EchoTool;

impl AgentToolExecute for EchoTool {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<kiana_agent_core::types::AgentToolResult<Value>, AgentToolError>>
    {
        Box::pin(async move {
            Ok(kiana_agent_core::types::AgentToolResult {
                content: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text: "echoed".to_string(),
                    text_signature: None,
                })],
                details: json!({}),
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
        parameters: json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
            "additionalProperties": false
        }),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(EchoTool),
    }
}

type RouteFn =
    Arc<dyn Fn(ModelRouteRequest, Arc<dyn ExtensionContextApi>) -> RouteFuture + Send + Sync>;

fn find_model(ctx: &dyn ExtensionContextApi, id: &str) -> Model {
    ctx.model_registry()
        .find("faux", id)
        .unwrap_or_else(|| panic!("missing faux model {id}"))
}

/// TS 的 `defaultRoute`：新用户 turn 按 thinking level 选模型，其余请求保持 sticky。
fn default_route_result(
    request: &ModelRouteRequest,
    ctx: &dyn ExtensionContextApi,
) -> Result<ModelRoute, VirtualModelRouteError> {
    if request.reason == ModelRouteReason::Direct {
        return Ok(ModelRoute {
            model: find_model(ctx, "large"),
            thinking_level: ModelThinkingLevel::Low,
            state: None,
        });
    }
    let sticky = request
        .failed
        .as_ref()
        .map(|failed| RouteModelRef {
            model: failed.model.clone(),
            thinking_level: failed.thinking_level,
        })
        .or_else(|| request.previous.clone());
    if request.reason != ModelRouteReason::User
        && let Some(sticky) = sticky
    {
        return Ok(ModelRoute {
            model: sticky.model,
            thinking_level: sticky.thinking_level.unwrap_or(ModelThinkingLevel::High),
            state: None,
        });
    }
    if request.thinking_level == ModelThinkingLevel::High {
        Ok(ModelRoute {
            model: find_model(ctx, "large"),
            thinking_level: ModelThinkingLevel::High,
            state: None,
        })
    } else {
        Ok(ModelRoute {
            model: find_model(ctx, "small"),
            thinking_level: ModelThinkingLevel::Off,
            state: None,
        })
    }
}

fn default_route(request: ModelRouteRequest, ctx: Arc<dyn ExtensionContextApi>) -> RouteFuture {
    let result = default_route_result(&request, ctx.as_ref());
    Box::pin(async move { result })
}

// ============================================================================
// 夹具
// ============================================================================

struct RoutedHarness {
    harness: Harness,
    requests: Arc<Mutex<Vec<ModelRouteRequest>>>,
}

impl RoutedHarness {
    fn reasons(&self) -> Vec<ModelRouteReason> {
        lock(&self.requests)
            .iter()
            .map(|request| request.reason)
            .collect()
    }

    /// 每个响应上记录的物理模型与 thinking level。
    fn dispatched(&self) -> Vec<String> {
        self.harness
            .session
            .messages()
            .into_iter()
            .filter_map(|message| {
                let assistant = message.as_assistant()?;
                let level = serde_json::to_value(
                    assistant.thinking_level.unwrap_or(ModelThinkingLevel::Off),
                )
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "off".to_string());
                Some(format!(
                    "{}/{}:{}",
                    assistant.provider, assistant.model, level
                ))
            })
            .collect()
    }

    fn compaction_reasons(&self) -> Vec<CompactionReason> {
        self.harness
            .events_of_type("compaction_start")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::CompactionStart { reason } => Some(reason),
                _ => None,
            })
            .collect()
    }

    async fn prompt(&self, text: impl Into<String>) {
        self.harness
            .session
            .prompt(text.into(), PromptOptions::default())
            .await
            .expect("prompt");
    }
}

fn virtual_model_factory(
    requests: Arc<Mutex<Vec<ModelRouteRequest>>>,
    route: RouteFn,
) -> kiana_ext_api::types::ExtensionFactory {
    factory(move |kiana: Arc<dyn ExtensionApi>| {
        kiana.register_virtual_model(ExtensionVirtualModel {
            provider: "router".to_string(),
            id: "auto".to_string(),
            name: "Auto".to_string(),
            thinking_levels: Some(vec![ModelThinkingLevel::Low, ModelThinkingLevel::High]),
            context_window: Some(1000),
            max_tokens: None,
            input: None,
            route: Arc::new(move |request, ctx| {
                lock(&requests).push(request.clone());
                route(request, ctx)
            }),
        });
    })
}

async fn create_routed_harness(route: RouteFn, mut options: HarnessOptions) -> RoutedHarness {
    let requests: Arc<Mutex<Vec<ModelRouteRequest>>> = Arc::new(Mutex::new(Vec::new()));
    options.models = Some(vec![
        FauxModelDefinition {
            id: "small".to_string(),
            context_window: Some(1000),
            ..Default::default()
        },
        FauxModelDefinition {
            id: "large".to_string(),
            context_window: Some(50_000),
            max_tokens: Some(4000),
            reasoning: Some(true),
            ..Default::default()
        },
    ]);
    options.tools = Some(vec![echo_tool()]);
    let mut factories = options.extension_factories;
    factories.push(virtual_model_factory(
        Arc::clone(&requests),
        Arc::clone(&route),
    ));
    options.extension_factories = factories;
    let harness = create_harness(options).await;
    // TS: `harness.session.agent.streamFunction = (m, c, o) => runtime.streamSimple(m, c, o)`。
    let runtime = harness.session.model_runtime().clone();
    {
        let runtime = runtime.clone();
        harness
            .agent
            .set_stream_function(Arc::new(move |model, context, options| {
                let runtime = runtime.clone();
                Box::pin(async move {
                    Ok(runtime.stream_simple(
                        &model,
                        context.as_context(),
                        options.map(Into::into).unwrap_or_default(),
                    ))
                })
            }));
    }
    let virtual_model = runtime
        .get_model("router", "auto")
        .expect("registered virtual model");
    harness
        .session
        .set_model(virtual_model, ModelMutationOptions::default())
        .await
        .expect("set virtual model");
    harness
        .session
        .set_thinking_level(ModelThinkingLevel::High, ModelMutationOptions::default());
    RoutedHarness { harness, requests }
}

fn settings(value: Value) -> kiana_config::settings::Settings {
    settings_from_json(value)
}

/// `session_before_compact` 扩展：返回固定摘要。
fn extension_compaction_factory(summary: &'static str) -> kiana_ext_api::types::ExtensionFactory {
    factory(move |kiana: Arc<dyn ExtensionApi>| {
        let _ = kiana.on(
            ExtensionEventKind::SessionBeforeCompact,
            standard(move |event, _ctx| {
                Box::pin(async move {
                    let ExtensionEvent::Session(SessionEvent::BeforeCompact(before)) = event else {
                        return Ok(None);
                    };
                    Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                        SessionBeforeCompactResult {
                            cancel: None,
                            compaction: Some(CompactionResult {
                                summary: summary.to_string(),
                                first_kept_entry_id: before.preparation.first_kept_entry_id.clone(),
                                tokens_before: before.preparation.tokens_before,
                                estimated_tokens_after: None,
                                usage: None,
                                details: None,
                            }),
                        },
                    )))
                })
            }),
        );
    })
}

fn text_response(text: &str) -> FauxResponseStep {
    faux_text_response(text)
}

fn error_response(message: &str) -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message(
        "",
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some(message.to_string()),
            ..Default::default()
        },
    ))
}

fn echo_response() -> FauxResponseStep {
    faux_tool_response(
        faux_tool_call("echo", kiana_ai::types::JsonObject::new()),
        StopReason::ToolUse,
    )
}

// ============================================================================
// 用例
// ============================================================================

/// TS: "routes each request, including retries, while the selection stays virtual"
#[tokio::test]
async fn routes_each_request_including_retries_while_the_selection_stays_virtual() {
    run(async {
        let routed = create_routed_harness(
            Arc::new(default_route),
            HarnessOptions {
                settings: Some(settings(json!({
                    "retry": {"enabled": true, "maxRetries": 3, "baseDelayMs": 1}
                }))),
                ..Default::default()
            },
        )
        .await;
        routed.harness.set_responses(vec![
            error_response("overloaded_error"),
            echo_response(),
            text_response("done"),
        ]);

        routed.prompt("hello").await;

        assert_eq!(
            routed.reasons(),
            vec![
                ModelRouteReason::User,
                ModelRouteReason::Retry,
                ModelRouteReason::Continuation
            ]
        );
        let requests = lock(&routed.requests).clone();
        let failed = requests[1].failed.as_ref().expect("failed request");
        assert_eq!(
            failed.message.error_message.as_deref(),
            Some("overloaded_error")
        );
        assert_eq!(
            requests[2].previous.as_ref().expect("previous").model.id,
            "large"
        );
        assert_eq!(
            routed.dispatched(),
            vec!["faux/large:high".to_string(), "faux/large:high".to_string()]
        );
        let model = routed.harness.session.model().expect("model");
        assert_eq!(
            (model.provider.as_str(), model.id.as_str()),
            ("router", "auto")
        );
        assert_eq!(
            routed.harness.session.thinking_level(),
            ModelThinkingLevel::High
        );
        assert_eq!(
            routed
                .harness
                .session
                .get_context_usage()
                .expect("context usage")
                .context_window,
            50_000
        );
    })
    .await;
}

/// TS: "retries the first request of a turn on the model routed for that turn"
#[tokio::test]
async fn retries_the_first_request_of_a_turn_on_the_model_routed_for_that_turn() {
    run(async {
        let routed = create_routed_harness(
            Arc::new(default_route),
            HarnessOptions {
                settings: Some(settings(json!({
                    "retry": {"enabled": true, "maxRetries": 3, "baseDelayMs": 1}
                }))),
                ..Default::default()
            },
        )
        .await;
        routed
            .harness
            .session
            .set_thinking_level(ModelThinkingLevel::Low, ModelMutationOptions::default());
        routed.harness.set_responses(vec![
            text_response("easy answer"),
            error_response("overloaded_error"),
            text_response("hard answer"),
        ]);
        routed.prompt("easy").await;
        routed
            .harness
            .session
            .set_thinking_level(ModelThinkingLevel::High, ModelMutationOptions::default());

        routed.prompt("hard").await;

        assert_eq!(
            routed.reasons(),
            vec![
                ModelRouteReason::User,
                ModelRouteReason::User,
                ModelRouteReason::Retry
            ]
        );
        let requests = lock(&routed.requests).clone();
        assert_eq!(
            requests[2].failed.as_ref().expect("failed").model.id,
            "large"
        );
        assert_eq!(
            requests[2].previous.as_ref().expect("previous").model.id,
            "small"
        );
        assert_eq!(
            routed.dispatched(),
            vec!["faux/small:off".to_string(), "faux/large:high".to_string()]
        );
    })
    .await;
}

/// TS: "routes the compact-and-retry after a truncated response as a retry"
#[tokio::test]
async fn routes_the_compact_and_retry_after_a_truncated_response_as_a_retry() {
    run(async {
        let routed = create_routed_harness(
            Arc::new(default_route),
            HarnessOptions {
                settings: Some(settings(json!({
                    "compaction": {"keepRecentTokens": 1, "reserveTokens": 0}
                }))),
                extension_factories: vec![extension_compaction_factory("overflow compacted")],
                ..Default::default()
            },
        )
        .await;
        let truncated = faux_assistant_message(
            "x".repeat(64),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Length),
                ..Default::default()
            },
        );
        let mut truncated = truncated;
        truncated.timestamp += 10_000;
        routed.harness.set_responses(vec![
            FauxResponseStep::Message(truncated),
            text_response("done"),
        ]);

        routed.prompt("x".repeat(5000)).await;

        assert_eq!(
            routed.compaction_reasons(),
            vec![CompactionReason::Overflow]
        );
        assert_eq!(
            routed.reasons(),
            vec![ModelRouteReason::User, ModelRouteReason::Retry]
        );
        let requests = lock(&routed.requests).clone();
        let failed = requests[1].failed.as_ref().expect("failed");
        assert_eq!(failed.model.id, "large");
        assert_eq!(failed.message.stop_reason, StopReason::Length);
    })
    .await;
}

/// TS: "routes requests after extension messages as continuations"
#[tokio::test]
async fn routes_requests_after_extension_messages_as_continuations() {
    run(async {
        let continued = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&continued);
        let routed = create_routed_harness(
            Arc::new(default_route),
            HarnessOptions {
                extension_factories: vec![factory(move |kiana: Arc<dyn ExtensionApi>| {
                    let flag = Arc::clone(&flag);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentBeforeSettle,
                        standard(move |_event, _ctx| {
                            let first = !flag.swap(true, Ordering::SeqCst);
                            Box::pin(async move {
                                if !first {
                                    return Ok(None);
                                }
                                Ok(Some(ExtensionEventResult::AgentBeforeSettle(
                                    BoundaryResult {
                                        entries: Some(vec![SessionBoundaryDraft::CustomMessage(
                                            CustomMessageEntryDraft {
                                                event_type:
                                                    CustomMessageEntryDraftType::CustomMessage,
                                                custom_type: "nudge".to_string(),
                                                content: UserContent::Text(
                                                    "Keep going.".to_string(),
                                                ),
                                                display: false,
                                                details: None,
                                            },
                                        )]),
                                        r#continue: Some(true),
                                    },
                                )))
                            })
                        }),
                    );
                })],
                ..Default::default()
            },
        )
        .await;
        routed
            .harness
            .set_responses(vec![text_response("first"), text_response("second")]);

        routed.prompt("hello").await;

        // 隐藏的 custom 消息对模型是 user 消息，但用户没有写它。
        assert_eq!(
            routed.reasons(),
            vec![ModelRouteReason::User, ModelRouteReason::Continuation]
        );
    })
    .await;
}

/// TS: "routes the first request of a prompt as a user turn when extension messages follow the prompt"
#[tokio::test]
async fn routes_the_first_request_of_a_prompt_as_user_when_extension_messages_follow() {
    run(async {
        let routed = create_routed_harness(
            Arc::new(default_route),
            HarnessOptions {
                extension_factories: vec![factory(|kiana: Arc<dyn ExtensionApi>| {
                    let _ = kiana.on(
                        ExtensionEventKind::BeforeAgentStart,
                        standard(|_event, _ctx| {
                            Box::pin(async move {
                                Ok(Some(ExtensionEventResult::BeforeAgentStart(
                                    BeforeAgentStartEventResult {
                                        message: Some(CustomMessagePayload {
                                            custom_type: "context".to_string(),
                                            content: UserContent::Text(
                                                "Extra context.".to_string(),
                                            ),
                                            display: false,
                                            details: None,
                                        }),
                                        system_prompt: None,
                                    },
                                )))
                            })
                        }),
                    );
                })],
                ..Default::default()
            },
        )
        .await;
        routed
            .harness
            .set_responses(vec![text_response("first"), text_response("second")]);

        routed.prompt("hello").await;
        routed.prompt("again").await;

        let messages = routed.harness.session.messages();
        assert_eq!(
            messages[messages.len() - 2].role(),
            "custom",
            "context ends with the extension message"
        );
        assert_eq!(
            routed.reasons(),
            vec![ModelRouteReason::User, ModelRouteReason::User]
        );
    })
    .await;
}

/// TS: "ends the run with an error response when routing fails and keeps the last physical limits"
#[tokio::test]
async fn ends_the_run_with_an_error_response_when_routing_fails() {
    run(async {
        let fail = Arc::new(AtomicBool::new(false));
        let fail_for_route = Arc::clone(&fail);
        let route: RouteFn = Arc::new(move |request, ctx| {
            if fail_for_route.load(Ordering::SeqCst) {
                return Box::pin(async { Err(VirtualModelRouteError::new("router unavailable")) });
            }
            default_route(request, ctx)
        });
        let routed = create_routed_harness(route, HarnessOptions::default()).await;
        routed.harness.set_responses(vec![text_response("answer")]);
        routed.prompt("hello").await;

        fail.store(true, Ordering::SeqCst);
        routed.prompt("again").await;

        let messages = routed.harness.session.messages();
        let last = messages.last().expect("last message");
        let assistant = last.as_assistant().expect("assistant");
        assert_eq!(assistant.provider, "router");
        assert_eq!(assistant.model, "auto");
        assert_eq!(assistant.stop_reason, StopReason::Error);
        assert!(
            assistant
                .error_message
                .as_deref()
                .unwrap_or_default()
                .contains("router unavailable"),
            "error message: {:?}",
            assistant.error_message
        );
        assert_eq!(routed.harness.faux.state().call_count(), 1);
        assert_eq!(
            routed
                .harness
                .session
                .get_context_usage()
                .expect("context usage")
                .context_window,
            50_000
        );
    })
    .await;
}

/// TS: "checks compaction against the physical model that produced the response"
#[tokio::test]
async fn checks_compaction_against_the_physical_model_that_produced_the_response() {
    run(async {
        let routed =
            create_routed_harness(Arc::new(default_route), HarnessOptions::default()).await;
        routed.harness.set_responses(vec![
            text_response("short answer"),
            text_response("long answer"),
        ]);
        routed.prompt("hello").await;

        // 约 20k token 超过虚拟模型声明的 1k 窗口，但装得下 large 的 50k。
        routed.prompt("x".repeat(80_000)).await;

        assert!(routed.compaction_reasons().is_empty());
    })
    .await;
}

/// TS: "compacts before a request routed to a model with a smaller window"
#[tokio::test]
async fn compacts_before_a_request_routed_to_a_model_with_a_smaller_window() {
    run(async {
        let routed = create_routed_harness(
            Arc::new(default_route),
            HarnessOptions {
                settings: Some(settings(json!({
                    "compaction": {"keepRecentTokens": 1, "reserveTokens": 0}
                }))),
                extension_factories: vec![extension_compaction_factory("compacted")],
                ..Default::default()
            },
        )
        .await;
        let compacted_before_small = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&compacted_before_small);
        routed.harness.set_responses(vec![
            text_response(&"y".repeat(8000)),
            FauxResponseStep::message_factory(move |_input: FauxResponseInput| {
                let flag = Arc::clone(&flag);
                async move {
                    flag.store(true, Ordering::SeqCst);
                    faux_assistant_message("small answer", Default::default())
                }
            }),
        ]);
        routed.prompt("hello").await;
        assert!(routed.compaction_reasons().is_empty());

        // 约 2k token 装得下刚回答的 large，但装不下 small 的 1k 窗口。
        routed
            .harness
            .session
            .set_thinking_level(ModelThinkingLevel::Low, ModelMutationOptions::default());
        routed.prompt("next").await;

        assert_eq!(
            routed.compaction_reasons(),
            vec![CompactionReason::Threshold]
        );
        assert!(
            compacted_before_small.load(Ordering::SeqCst),
            "compaction finished before the small request"
        );
        assert_eq!(
            routed.dispatched().last().map(String::as_str),
            Some("faux/small:off")
        );
    })
    .await;
}

/// TS: "compacts between turns of a run when the next request is routed to a smaller window"
#[tokio::test]
async fn compacts_between_turns_when_the_next_request_is_routed_to_a_smaller_window() {
    run(async {
        let route: RouteFn = Arc::new(|request, ctx| {
            if request.reason == ModelRouteReason::Continuation {
                return Box::pin(async move {
                    Ok(ModelRoute {
                        model: find_model(ctx.as_ref(), "small"),
                        thinking_level: ModelThinkingLevel::Off,
                        state: None,
                    })
                });
            }
            default_route(request, ctx)
        });
        let routed = create_routed_harness(
            route,
            HarnessOptions {
                settings: Some(settings(json!({
                    "compaction": {"keepRecentTokens": 1, "reserveTokens": 0}
                }))),
                extension_factories: vec![extension_compaction_factory("compacted")],
                ..Default::default()
            },
        )
        .await;
        let compacted_before_small = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&compacted_before_small);
        routed.harness.set_responses(vec![
            echo_response(),
            FauxResponseStep::message_factory(move |_input: FauxResponseInput| {
                let flag = Arc::clone(&flag);
                async move {
                    flag.store(true, Ordering::SeqCst);
                    faux_assistant_message("small answer", Default::default())
                }
            }),
        ]);

        // 约 2k token 装得下第一个 turn 的 large，但装不下第二个 turn 的 small。
        routed.prompt("x".repeat(8000)).await;

        assert_eq!(
            routed.compaction_reasons(),
            vec![CompactionReason::Threshold]
        );
        assert!(
            compacted_before_small.load(Ordering::SeqCst),
            "compaction finished between the turns"
        );
        assert_eq!(
            routed.dispatched(),
            vec!["faux/large:high".to_string(), "faux/small:off".to_string()]
        );
    })
    .await;
}

/// TS: "stores router state on the branch and passes it to later requests"
#[tokio::test]
async fn stores_router_state_on_the_branch_and_passes_it_to_later_requests() {
    run(async {
        let states: Arc<Mutex<Vec<Option<Value>>>> = Arc::new(Mutex::new(Vec::new()));
        let states_for_route = Arc::clone(&states);
        let route: RouteFn = Arc::new(move |request, ctx| {
            lock(&states_for_route).push(request.state.clone());
            let turns = request
                .state
                .as_ref()
                .and_then(|state| state.get("turns"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let mut result = default_route_result(&request, ctx.as_ref()).expect("route");
            if request.reason == ModelRouteReason::Continuation {
                // 返回 request.state 表示“保持不变”，不再存一次。
                result.state = request.state.clone();
            } else {
                result.state = Some(json!({"turns": turns + 1}));
            }
            Box::pin(async move { Ok(result) })
        });
        let routed = create_routed_harness(
            route,
            HarnessOptions {
                settings: Some(settings(json!({
                    "compaction": {"keepRecentTokens": 1}
                }))),
                ..Default::default()
            },
        )
        .await;
        routed.harness.set_responses(vec![
            echo_response(),
            text_response("first"),
            text_response("second"),
            text_response("summary"),
            text_response("summary"),
        ]);

        routed.prompt("one").await;
        routed.prompt("two").await;
        let stored = || -> Vec<Value> {
            routed
                .harness
                .session_manager()
                .get_branch(None)
                .into_iter()
                .filter_map(|entry| match entry {
                    SessionEntry::Custom(entry)
                        if entry.custom_type == VIRTUAL_MODEL_STATE_ENTRY =>
                    {
                        entry.data
                    }
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            stored(),
            vec![
                json!({"provider": "router", "modelId": "auto", "state": {"turns": 1}}),
                json!({"provider": "router", "modelId": "auto", "state": {"turns": 2}}),
            ]
        );

        let _ = routed.harness.session.compact(None).await.expect("compact");

        assert_eq!(
            routed.reasons(),
            vec![
                ModelRouteReason::User,
                ModelRouteReason::Continuation,
                ModelRouteReason::User,
                ModelRouteReason::Direct
            ]
        );
        assert_eq!(
            lock(&states).clone(),
            vec![
                None,
                Some(json!({"turns": 1})),
                Some(json!({"turns": 1})),
                None
            ]
        );
        assert_eq!(stored().len(), 2);
    })
    .await;
}

/// TS: "does not route compactions that an extension supplies"
#[tokio::test]
async fn does_not_route_compactions_that_an_extension_supplies() {
    run(async {
        let route: RouteFn = Arc::new(|request, ctx| {
            if request.reason == ModelRouteReason::Direct {
                return Box::pin(async { Err(VirtualModelRouteError::new("router unavailable")) });
            }
            default_route(request, ctx)
        });
        let routed = create_routed_harness(
            route,
            HarnessOptions {
                settings: Some(settings(json!({
                    "compaction": {"keepRecentTokens": 1}
                }))),
                extension_factories: vec![extension_compaction_factory("extension summary")],
                ..Default::default()
            },
        )
        .await;
        routed.harness.set_responses(vec![
            text_response("first answer"),
            text_response("second answer"),
        ]);
        routed.prompt("first").await;
        routed.prompt("second").await;

        let result = routed.harness.session.compact(None).await.expect("compact");

        assert_eq!(result.summary, "extension summary");
        assert_eq!(
            routed.reasons(),
            vec![ModelRouteReason::User, ModelRouteReason::User]
        );
    })
    .await;
}

/// TS: "routes compaction summaries to the resolved model"
#[tokio::test]
async fn routes_compaction_summaries_to_the_resolved_model() {
    run(async {
        let routed = create_routed_harness(
            Arc::new(default_route),
            HarnessOptions {
                settings: Some(settings(json!({
                    "compaction": {"keepRecentTokens": 1}
                }))),
                ..Default::default()
            },
        )
        .await;
        let summaries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let summaries_for_factory = Arc::clone(&summaries);
        let summary = FauxResponseStep::message_factory(move |input: FauxResponseInput| {
            let summaries = Arc::clone(&summaries_for_factory);
            async move {
                let reasoning = match input.options.as_ref().and_then(|options| options.reasoning) {
                    Some(level) => serde_json::to_value(level)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_string))
                        .unwrap_or_else(|| "off".to_string()),
                    None => "off".to_string(),
                };
                lock(&summaries).push(format!("{}:{}", input.model.id, reasoning));
                faux_assistant_message("summary", Default::default())
            }
        });
        routed.harness.set_responses(vec![
            text_response("first answer"),
            text_response("second answer"),
            summary.clone(),
            summary,
        ]);
        routed.prompt("first").await;
        routed.prompt("second").await;

        let result = routed.harness.session.compact(None).await.expect("compact");

        assert!(result.summary.contains("summary"));
        assert_eq!(
            routed.reasons(),
            vec![
                ModelRouteReason::User,
                ModelRouteReason::User,
                ModelRouteReason::Direct
            ]
        );
        assert_eq!(
            lock(&summaries).clone(),
            vec!["large:low".to_string(), "large:low".to_string()]
        );
    })
    .await;
}
