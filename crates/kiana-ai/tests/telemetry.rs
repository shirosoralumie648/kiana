//! `telemetry.test.ts` 的 Rust 移植。
//!
//! 与 TS 的差异（可观察行为以外）：
//! - `performance.now()` 的注入点在 Rust 是 [`std::time::Instant`]，无法像 `vi.spyOn` 那样
//!   伪造；「monotonic durations」测试断言单位、单调性与完整的 metric 属性值。
//! - 堆内存断言（`getHeapStatistics`）在 Rust 无对应物；改为断言错误消息被截断到 500 个
//!   UTF-16 码元。
//! - `{ ...span, metrics: undefined }` 用 [`TracingOnlySpan`] 包装复刻。

mod models_support;

use std::sync::Arc;

use async_trait::async_trait;
use kiana_ai::models::{
    AI_TELEMETRY_SCHEMA, CreateModelsOptions, CreateProviderOptions, ModelsDeferredFetchOptions,
    ModelsSimpleStreamOptions, ModelsStreamOptions, ProviderApiImpl, ProviderStreams,
    create_models, create_provider,
};
use kiana_ai::telemetry::{ObserveModelRequestOptions, observe_model_request};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, DeferredHandle,
    DoneReason, ErrorReason, Model, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    ThinkingContent, ToolCall, Usage, UsageCost,
};
use kiana_telemetry::index::{
    SpanAttributes, SpanContextId, SpanOptions, SpanStatus, StatusError, TelemetryContext,
    TelemetryPropagation, TelemetrySpan,
};
use kiana_telemetry::memory::InMemoryTelemetryContext;
use kiana_telemetry::{AttributeValue, SpanLink, TelemetryError};

use models_support::{chat, wait_for};

fn telemetry_model() -> Model {
    Model {
        id: "model".into(),
        name: "Model".into(),
        api: "telemetry-test".into(),
        provider: "test".into(),
        base_url: "https://example.test".into(),
        reasoning: Some(true),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        cost: kiana_ai::types::ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 3.0,
            cache_write: 4.0,
            tiers: None,
        },
        context_window: Some(1000),
        max_tokens: Some(100),
        ..Model::default()
    }
}

fn deferred_handle() -> DeferredHandle {
    DeferredHandle {
        provider: "test".into(),
        model_id: "model".into(),
        api: "telemetry-test".into(),
        id: "deferred-secret".into(),
        ..Default::default()
    }
}

fn message(stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        api: "telemetry-test".into(),
        provider: "test".into(),
        model: "model".into(),
        content: vec![
            AssistantContentBlock::Text(TextContent {
                kind_text: Default::default(),
                text: "text-secret".into(),
                text_signature: None,
            }),
            AssistantContentBlock::Thinking(ThinkingContent {
                kind_thinking: Default::default(),
                thinking: "thinking-secret".into(),
                thinking_signature: None,
                redacted: None,
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                kind_tool_call: Default::default(),
                id: "call-secret".into(),
                name: "lookup".into(),
                arguments: [("value".to_string(), serde_json::json!("argument-secret"))]
                    .into_iter()
                    .collect(),
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            }),
        ],
        usage: Usage {
            input: 11,
            output: 22,
            cache_read: 33,
            cache_write: 44,
            total_tokens: 110,
            cost: UsageCost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.3,
                cache_write: 0.4,
                total: 1.0,
                total_from_provider: None,
            },
            ..Usage::default()
        },
        response_id: Some("response".into()),
        stop_reason,
        timestamp: 0,
        ..AssistantMessage::default()
    }
}

fn events(result: &AssistantMessage) -> Vec<AssistantMessageEvent> {
    let tool_call = match &result.content[2] {
        AssistantContentBlock::ToolCall(tool_call) => tool_call.clone(),
        _ => panic!("Missing tool call"),
    };
    let partial = || Box::new(result.clone());
    let mut events = vec![
        AssistantMessageEvent::Start { partial: partial() },
        AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "text-secret".into(),
            partial: partial(),
        },
        AssistantMessageEvent::TextEnd {
            content_index: 0,
            content: "text-secret".into(),
            partial: partial(),
        },
        AssistantMessageEvent::ThinkingEnd {
            content_index: 1,
            content: "thinking-secret".into(),
            partial: partial(),
        },
        AssistantMessageEvent::ToolCallDelta {
            content_index: 2,
            delta: "{\"value\":".into(),
            partial: partial(),
        },
        AssistantMessageEvent::ToolCallDelta {
            content_index: 2,
            delta: "\"argument-secret\"}".into(),
            partial: partial(),
        },
        AssistantMessageEvent::ToolCallEnd {
            content_index: 2,
            tool_call: tool_call.clone(),
            partial: partial(),
        },
    ];
    match result.stop_reason {
        StopReason::Error => events.push(AssistantMessageEvent::Error {
            reason: ErrorReason::Error,
            error: partial(),
        }),
        StopReason::Aborted => events.push(AssistantMessageEvent::Error {
            reason: ErrorReason::Aborted,
            error: partial(),
        }),
        StopReason::Pending | StopReason::Deferred => events.push(AssistantMessageEvent::Done {
            reason: DoneReason::Deferred,
            message: partial(),
        }),
        StopReason::Stop
        | StopReason::Length
        | StopReason::ToolUse
        | StopReason::TurnCapped
        | StopReason::TokenCapped
        | StopReason::CostCapped
        | StopReason::TimeCapped => events.push(AssistantMessageEvent::Done {
            reason: match result.stop_reason {
                StopReason::Stop => DoneReason::Stop,
                StopReason::Length => DoneReason::Length,
                _ => DoneReason::ToolUse,
            },
            message: partial(),
        }),
    }
    events
}

/// provider 的响应：把预生成的事件推进流并 end(result)。
struct Responder {
    result: AssistantMessage,
    events: Vec<AssistantMessageEvent>,
}

impl Responder {
    fn respond(&self, _model: &Model) -> kiana_ai::event_stream::AssistantMessageEventStream {
        let stream = kiana_ai::event_stream::AssistantMessageEventStream::new();
        for event in &self.events {
            let _ = stream.push(event.clone());
        }
        stream.end(self.result.clone());
        stream
    }
}

#[async_trait]
impl ProviderStreams for Responder {
    fn stream(
        &self,
        model: &Model,
        _context: &kiana_ai::types::TranscriptContext,
        _options: StreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        self.respond(model)
    }
    fn stream_simple(
        &self,
        model: &Model,
        _context: &kiana_ai::types::TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        self.respond(model)
    }
    fn has_fetch_deferred(&self) -> bool {
        true
    }
    fn fetch_deferred(
        &self,
        model: &Model,
        _handle: &DeferredHandle,
        _options: kiana_ai::types::DeferredFetchOptions,
    ) -> Option<kiana_ai::event_stream::AssistantMessageEventStream> {
        Some(self.respond(model))
    }
}

fn setup(
    result: AssistantMessage,
) -> (
    Arc<dyn kiana_ai::models::MutableModels>,
    Vec<AssistantMessageEvent>,
) {
    let model = telemetry_model();
    let stream_events = events(&result);
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "test".into(),
            models: vec![chat(model)],
            auth: kiana_ai::auth::types::ProviderAuth {
                api_key: Some(Arc::new(TestAuth)),
                oauth: None,
            },
            api: Some(ProviderApiImpl::Single(Arc::new(Responder {
                result,
                events: stream_events.clone(),
            }))),
            ..Default::default()
        })
        .unwrap(),
    );
    (models, stream_events)
}

struct TestAuth;

#[async_trait]
impl kiana_ai::auth::types::ApiKeyAuth for TestAuth {
    fn name(&self) -> &str {
        "Test"
    }
    async fn resolve(
        &self,
        _input: kiana_ai::auth::types::ApiKeyAuthInput<'_>,
    ) -> Result<Option<kiana_ai::auth::types::AuthResult>, kiana_ai::auth::types::AuthError> {
        Ok(Some(kiana_ai::auth::types::AuthResult::new(
            kiana_ai::auth::types::ModelAuth::default(),
        )))
    }
}

/// 在 `caller` span 里执行一次请求，返回 span 数量与调用结果。
async fn in_caller_span<T, F>(telemetry: &InMemoryTelemetryContext, callback: F) -> T
where
    F: FnOnce(Arc<dyn TelemetrySpan>) -> futures::future::BoxFuture<'static, T> + Send + 'static,
    T: Send + 'static,
{
    let context: Arc<dyn TelemetryContext> = Arc::new(telemetry.clone());
    let starter = kiana_telemetry::index::create_typed_span_starter(context, &[]);
    starter
        .start("caller", SpanAttributes::new(), move |span, _child| {
            callback(span)
        })
        .await
        .expect("caller span")
}

async fn settled(telemetry: &InMemoryTelemetryContext) {
    let telemetry = telemetry.clone();
    wait_for(move || telemetry.get_spans().iter().all(|span| span.settled)).await;
}

fn attribute_string(
    span: &kiana_telemetry::memory::RecordedTelemetrySpan,
    key: &str,
) -> Option<String> {
    match span.attributes.get(key) {
        Some(AttributeValue::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn attribute_number(
    span: &kiana_telemetry::memory::RecordedTelemetrySpan,
    key: &str,
) -> Option<f64> {
    match span.attributes.get(key) {
        Some(AttributeValue::Number(value)) => Some(*value),
        _ => None,
    }
}

/// 期望的 metric 属性集（字符串值），对应 TS 的 `toEqual({ provider, model, stop })`。
fn attributes(pairs: &[(&str, &str)]) -> SpanAttributes {
    pairs
        .iter()
        .map(|(key, value)| {
            (
                (*key).to_owned(),
                AttributeValue::String((*value).to_owned()),
            )
        })
        .collect()
}

async fn observe_method(method: &str, result: AssistantMessage) -> InMemoryTelemetryContext {
    let telemetry = InMemoryTelemetryContext::new();
    let (models, stream_events) = setup(result.clone());
    let model = telemetry_model();
    let handle = deferred_handle();
    let method = method.to_owned();
    let result_for_span = result.clone();
    in_caller_span(&telemetry, move |span| {
        let models = Arc::clone(&models);
        let model = model.clone();
        let handle = handle.clone();
        let method = method.clone();
        let stream_events = stream_events.clone();
        let result = result_for_span.clone();
        Box::pin(async move {
            let telemetry = Some(Arc::clone(&span));
            let stream = match method.as_str() {
                "stream" => models.stream(
                    &model,
                    &kiana_ai::types::Context::default(),
                    ModelsStreamOptions {
                        base: StreamOptions {
                            telemetry,
                            ..Default::default()
                        },
                        reasoning: Some("high".into()),
                        ..Default::default()
                    },
                ),
                "streamSimple" => models.stream_simple(
                    &model,
                    &kiana_ai::types::Context::default(),
                    ModelsSimpleStreamOptions {
                        base: SimpleStreamOptions {
                            base: StreamOptions {
                                telemetry,
                                ..Default::default()
                            },
                            reasoning: Some(kiana_ai::types::ThinkingLevel::High),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                ),
                "streamDeferred" => models.stream_deferred(
                    &model,
                    &handle,
                    ModelsDeferredFetchOptions {
                        base: kiana_ai::types::DeferredFetchOptions {
                            base: StreamOptions {
                                telemetry,
                                ..Default::default()
                            },
                            wait: None,
                        },
                        ..Default::default()
                    },
                ),
                other => panic!("unknown method {other}"),
            };
            let mut received = Vec::new();
            while let Some(event) = stream.recv().await {
                received.push(event);
            }
            assert_eq!(received.len(), stream_events.len());
            for (index, event) in received.iter().enumerate() {
                assert_eq!(event, &stream_events[index]);
            }
            let message = stream.result().await.unwrap();
            assert_eq!(message.stop_reason, result.stop_reason);
            assert_eq!(message.usage.input, result.usage.input);
        })
    })
    .await;
    telemetry
}

#[tokio::test]
async fn observes_stream_without_changing_delivered_events_or_results() {
    let telemetry = observe_method("stream", message(StopReason::Stop)).await;
    settled(&telemetry).await;
    let spans = telemetry.get_spans();
    let parent = &spans[0];
    let span = &spans[1];
    assert_eq!(span.name, "kiana.ai.request");
    assert_eq!(span.parent_id, Some(parent.id));
    assert_eq!(attribute_string(span, "provider").as_deref(), Some("test"));
    assert_eq!(
        attribute_string(span, "api").as_deref(),
        Some("telemetry-test")
    );
    assert_eq!(attribute_string(span, "model").as_deref(), Some("model"));
    assert_eq!(attribute_string(span, "reasoning").as_deref(), Some("high"));
    assert_eq!(attribute_string(span, "stop").as_deref(), Some("stop"));
    assert_eq!(attribute_number(span, "usage.input"), Some(11.0));
    assert_eq!(attribute_number(span, "usage.output"), Some(22.0));
    assert_eq!(attribute_number(span, "usage.cache_read"), Some(33.0));
    assert_eq!(attribute_number(span, "usage.cache_write"), Some(44.0));
    assert_eq!(attribute_number(span, "cost.total"), Some(1.0));
    assert_eq!(
        attribute_string(span, "response.id").as_deref(),
        Some("response")
    );
    let events: Vec<(String, SpanAttributes)> = span
        .events
        .iter()
        .map(|event| (event.name.clone(), event.attributes.clone()))
        .collect();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].0, "kiana.ai.first_chunk");
    assert!(events[0].1.is_empty());
    assert_eq!(
        events[1].1.get("type"),
        Some(&AttributeValue::String("text".into()))
    );
    assert_eq!(events[1].1.get("index"), Some(&AttributeValue::Number(0.0)));
    assert_eq!(
        events[1].1.get("chars"),
        Some(&AttributeValue::Number(11.0))
    );
    assert_eq!(
        events[2].1.get("chars"),
        Some(&AttributeValue::Number(15.0))
    );
    assert_eq!(
        events[3].1.get("chars"),
        Some(&AttributeValue::Number(27.0))
    );
    assert_eq!(
        events[3].1.get("tool"),
        Some(&AttributeValue::String("lookup".into()))
    );
    let rendered = serde_json::to_string(&span).unwrap();
    assert!(!rendered.contains("text-secret"));
    assert!(!rendered.contains("thinking-secret"));
    assert!(!rendered.contains("argument-secret"));
    assert!(!rendered.contains("call-secret"));
    assert!(!rendered.contains("deferred-secret"));

    let measurements = telemetry.get_metrics();
    assert_eq!(measurements.len(), 7);
    let counters: Vec<f64> = measurements
        .iter()
        .filter(|measurement| measurement.kind == kiana_telemetry::MetricKind::Counter)
        .map(|measurement| measurement.value)
        .collect();
    assert_eq!(counters, [11.0, 22.0, 33.0, 44.0, 1.0]);
    for measurement in &measurements {
        let mut keys: Vec<String> = measurement.attributes.keys().cloned().collect();
        keys.sort();
        let expected: Vec<&str> = if measurement.name == "kiana.ai.tokens" {
            vec!["kind", "model", "provider"]
        } else if measurement.name == "kiana.ai.request.duration" {
            vec!["model", "provider", "stop"]
        } else {
            vec!["model", "provider"]
        };
        assert_eq!(keys, expected);
        if measurement.kind == kiana_telemetry::MetricKind::Histogram {
            assert_eq!(measurement.context, Some(span.context.clone()));
        }
    }
}

#[tokio::test]
async fn observes_stream_simple_without_changing_delivered_events_or_results() {
    let telemetry = observe_method("streamSimple", message(StopReason::Stop)).await;
    settled(&telemetry).await;
    let spans = telemetry.get_spans();
    let span = &spans[1];
    assert_eq!(span.name, "kiana.ai.request");
    assert_eq!(attribute_string(span, "reasoning").as_deref(), Some("high"));
}

#[tokio::test]
async fn observes_stream_deferred_without_changing_delivered_events_or_results() {
    let telemetry = observe_method("streamDeferred", message(StopReason::Stop)).await;
    settled(&telemetry).await;
    let spans = telemetry.get_spans();
    let span = &spans[1];
    assert_eq!(span.name, "kiana.ai.poll");
    assert!(!span.attributes.contains_key("reasoning"));
}

async fn status_for(
    reason: StopReason,
    error_message: &str,
) -> (InMemoryTelemetryContext, SpanStatus) {
    let telemetry = InMemoryTelemetryContext::new();
    let mut result = message(reason);
    result.error_message = Some(error_message.into());
    let (models, _) = setup(result);
    let model = telemetry_model();
    in_caller_span(&telemetry, move |span| {
        let models = Arc::clone(&models);
        let model = model.clone();
        Box::pin(async move {
            let message = models
                .complete_simple(
                    &model,
                    &kiana_ai::types::Context::default(),
                    ModelsSimpleStreamOptions {
                        base: SimpleStreamOptions {
                            base: StreamOptions {
                                telemetry: Some(Arc::clone(&span)),
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )
                .await;
            assert_eq!(message.stop_reason, reason_for(reason));
        })
    })
    .await;
    settled(&telemetry).await;
    let span = telemetry.get_spans()[1].clone();
    (telemetry, span.status)
}

fn reason_for(reason: StopReason) -> StopReason {
    reason
}

#[tokio::test]
async fn reports_error_status_without_error_attributes_or_events() {
    let (telemetry, status) = status_for(StopReason::Error, "error-secret").await;
    assert_eq!(
        status,
        SpanStatus::Error {
            error: Some(StatusError {
                name: "provider_error".into(),
                message: "error-secret".into()
            })
        }
    );
    let span = &telemetry.get_spans()[1];
    assert_eq!(attribute_string(span, "stop").as_deref(), Some("error"));
    assert!(
        !serde_json::to_string(&span.attributes)
            .unwrap()
            .contains("error-secret")
    );
    assert!(
        !serde_json::to_string(&span.events)
            .unwrap()
            .contains("error-secret")
    );
}

#[tokio::test]
async fn reports_aborted_status_without_error_attributes_or_events() {
    let (telemetry, status) = status_for(StopReason::Aborted, "error-secret").await;
    assert_eq!(status, SpanStatus::Ok);
    let span = &telemetry.get_spans()[1];
    assert_eq!(attribute_string(span, "stop").as_deref(), Some("aborted"));
    let rendered = serde_json::to_string(span).unwrap();
    assert!(!rendered.contains("error-secret"));
}

#[tokio::test]
async fn bounds_poll_error_messages_including_missing_messages() {
    for missing in [false, true] {
        let telemetry = InMemoryTelemetryContext::new();
        let mut result = message(StopReason::Error);
        result.error_message = if missing {
            None
        } else {
            Some(format!("{}discarded-tail", "x".repeat(500)))
        };
        let (models, _) = setup(result);
        let model = telemetry_model();
        let handle = deferred_handle();
        in_caller_span(&telemetry, move |span| {
            let models = Arc::clone(&models);
            let model = model.clone();
            let handle = handle.clone();
            Box::pin(async move {
                models
                    .fetch_deferred(
                        &model,
                        &handle,
                        ModelsDeferredFetchOptions {
                            base: kiana_ai::types::DeferredFetchOptions {
                                base: StreamOptions {
                                    telemetry: Some(Arc::clone(&span)),
                                    ..Default::default()
                                },
                                wait: None,
                            },
                            ..Default::default()
                        },
                    )
                    .await;
            })
        })
        .await;
        settled(&telemetry).await;
        let span = &telemetry.get_spans()[1];
        assert_eq!(
            span.status,
            SpanStatus::Error {
                error: Some(StatusError {
                    name: "provider_error".into(),
                    message: if missing {
                        String::new()
                    } else {
                        "x".repeat(500)
                    },
                })
            }
        );
        let rendered = serde_json::to_string(span).unwrap();
        assert!(!rendered.contains("discarded-tail"));
        assert!(
            !serde_json::to_string(&span.attributes)
                .unwrap()
                .contains("xxx")
        );
        assert!(!serde_json::to_string(&span.events).unwrap().contains("xxx"));
    }
}

#[tokio::test]
async fn includes_setup_failures_and_omits_native_reasoning_objects() {
    let telemetry = InMemoryTelemetryContext::new();
    let models = create_models(CreateModelsOptions::default());
    let model = telemetry_model();
    in_caller_span(&telemetry, move |span| {
        let models = Arc::clone(&models);
        let model = model.clone();
        Box::pin(async move {
            let result = models
                .complete(
                    &model,
                    &kiana_ai::types::Context::default(),
                    ModelsStreamOptions {
                        base: StreamOptions {
                            telemetry: Some(Arc::clone(&span)),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )
                .await;
            assert_eq!(result.stop_reason, StopReason::Error);
        })
    })
    .await;
    settled(&telemetry).await;
    let span = &telemetry.get_spans()[1];
    assert!(!span.attributes.contains_key("reasoning"));
    assert_eq!(span.events.len(), 1);
    assert_eq!(span.events[0].name, "kiana.ai.first_chunk");
    assert_eq!(
        span.status,
        SpanStatus::Error {
            error: Some(StatusError {
                name: "provider_error".into(),
                message: "Unknown provider: test".into()
            })
        }
    );
    assert!(
        !serde_json::to_string(&span.attributes)
            .unwrap()
            .contains("Unknown provider")
    );
}

#[tokio::test]
async fn counts_interleaved_tool_deltas_independently_without_reading_arguments() {
    let telemetry = InMemoryTelemetryContext::new();
    let result = message(StopReason::Stop);
    let model = telemetry_model();
    in_caller_span(&telemetry, move |span| {
        let result = result.clone();
        let model = model.clone();
        Box::pin(async move {
            let observer = observe_model_request(
                &model,
                ObserveModelRequestOptions {
                    telemetry: Some(Arc::clone(&span)),
                    reasoning: None,
                },
                false,
            );
            let tool_call = ToolCall {
                kind_tool_call: Default::default(),
                id: "call".into(),
                name: "lookup".into(),
                arguments: Default::default(),
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            };
            let partial = Box::new(result.clone());
            for (index, delta) in [(2usize, "ab"), (3, "123"), (2, "c")] {
                if let Some(on_event) = observer.on_event.as_ref() {
                    on_event(&AssistantMessageEvent::ToolCallDelta {
                        content_index: index,
                        delta: delta.into(),
                        partial: partial.clone(),
                    });
                }
            }
            for index in [3usize, 2, 4] {
                if let Some(on_event) = observer.on_event.as_ref() {
                    on_event(&AssistantMessageEvent::ToolCallEnd {
                        content_index: index,
                        tool_call: tool_call.clone(),
                        partial: partial.clone(),
                    });
                }
            }
            if let Some(on_result) = observer.on_result.as_ref() {
                on_result(Some(&result));
            }
        })
    })
    .await;
    settled(&telemetry).await;
    let span = &telemetry.get_spans()[1];
    let block_events: Vec<_> = span.events[1..].to_vec();
    assert_eq!(block_events.len(), 3);
    assert_eq!(
        block_events[0].attributes.get("index"),
        Some(&AttributeValue::Number(3.0))
    );
    assert_eq!(
        block_events[0].attributes.get("chars"),
        Some(&AttributeValue::Number(3.0))
    );
    assert_eq!(
        block_events[1].attributes.get("index"),
        Some(&AttributeValue::Number(2.0))
    );
    assert_eq!(
        block_events[2].attributes.get("index"),
        Some(&AttributeValue::Number(4.0))
    );
    assert_eq!(
        block_events[2].attributes.get("chars"),
        Some(&AttributeValue::Number(0.0))
    );
}

/// `{ ...span, metrics: undefined }`：委托除 `metrics()` 外的全部方法。
struct TracingOnlySpan(Arc<dyn TelemetrySpan>);

impl TelemetryContext for TracingOnlySpan {
    fn metrics(&self) -> Option<Arc<dyn kiana_telemetry::TelemetryMetrics>> {
        None
    }
    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation>> {
        self.0.propagation()
    }
    fn start_span<'a>(
        &'a self,
        options: SpanOptions,
        callback: kiana_telemetry::SpanCallback<'a>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<kiana_telemetry::index::ErasedValue, TelemetryError>,
                > + Send
                + 'a,
        >,
    > {
        self.0.start_span(options, callback)
    }
    fn span_context_id(&self) -> SpanContextId {
        self.0.span_context_id()
    }
}

impl TelemetrySpan for TracingOnlySpan {
    fn add_link(&self, link: SpanLink) {
        self.0.add_link(link);
    }
    fn add_event(&self, name: &str, attributes: Option<SpanAttributes>) {
        self.0.add_event(name, attributes);
    }
    fn set_attributes(&self, attributes: SpanAttributes) {
        self.0.set_attributes(attributes);
    }
    fn set_status(&self, status: SpanStatus) {
        self.0.set_status(status);
    }
}

#[tokio::test]
async fn supports_a_tracing_only_span_without_metric_capabilities() {
    let telemetry = InMemoryTelemetryContext::new();
    let (models, _) = setup(message(StopReason::Stop));
    let model = telemetry_model();
    in_caller_span(&telemetry, move |span| {
        let models = Arc::clone(&models);
        let model = model.clone();
        Box::pin(async move {
            let tracing_only: Arc<dyn TelemetrySpan> = Arc::new(TracingOnlySpan(span));
            models
                .complete_simple(
                    &model,
                    &kiana_ai::types::Context::default(),
                    ModelsSimpleStreamOptions {
                        base: SimpleStreamOptions {
                            base: StreamOptions {
                                telemetry: Some(tracing_only),
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )
                .await;
        })
    })
    .await;
    settled(&telemetry).await;
    assert_eq!(telemetry.get_spans().len(), 2);
    assert!(telemetry.get_metrics().is_empty());
}

#[tokio::test]
async fn defaults_to_noop_telemetry() {
    let (models, _) = setup(message(StopReason::Stop));
    let result = models
        .complete_simple(
            &telemetry_model(),
            &kiana_ai::types::Context::default(),
            ModelsSimpleStreamOptions::default(),
        )
        .await;
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert!(AI_TELEMETRY_SCHEMA.spans["kiana.ai.request"].start_attributes["provider"].required);
}

#[tokio::test]
async fn records_low_cardinality_metrics_with_monotonic_durations() {
    let telemetry = InMemoryTelemetryContext::new();
    let result = message(StopReason::Stop);
    let model = telemetry_model();
    in_caller_span(&telemetry, move |span| {
        let result = result.clone();
        let model = model.clone();
        Box::pin(async move {
            let observer = observe_model_request(
                &model,
                ObserveModelRequestOptions {
                    telemetry: Some(Arc::clone(&span)),
                    reasoning: None,
                },
                false,
            );
            if let Some(on_event) = observer.on_event.as_ref() {
                on_event(&AssistantMessageEvent::Start {
                    partial: Box::new(result.clone()),
                });
            }
            if let Some(on_result) = observer.on_result.as_ref() {
                on_result(Some(&result));
            }
        })
    })
    .await;
    settled(&telemetry).await;
    let metrics = telemetry.get_metrics();
    let names: Vec<&str> = metrics.iter().map(|metric| metric.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "kiana.ai.first_chunk.duration",
            "kiana.ai.request.duration",
            "kiana.ai.tokens",
            "kiana.ai.tokens",
            "kiana.ai.tokens",
            "kiana.ai.tokens",
            "kiana.ai.cost"
        ]
    );
    // TS 用假时钟断言精确的 25/75 ms；Rust 的 `Instant` 时钟不可注入，改为断言与 TS 相同的
    // 单位、单调性（request 在 first_chunk 之后记录，因此不会更小）与**完整属性值**。
    let first_chunk = &metrics[0];
    let request = &metrics[1];
    assert_eq!(first_chunk.unit.as_deref(), Some("ms"));
    assert_eq!(request.unit.as_deref(), Some("ms"));
    assert!(first_chunk.value.is_finite() && first_chunk.value >= 0.0);
    assert!(request.value.is_finite() && request.value >= first_chunk.value);
    assert_eq!(
        first_chunk.attributes,
        attributes(&[("provider", "test"), ("model", "model")])
    );
    assert_eq!(
        request.attributes,
        attributes(&[("provider", "test"), ("model", "model"), ("stop", "stop")])
    );
    let token_kinds: Vec<String> = metrics[2..6]
        .iter()
        .map(|metric| {
            assert_eq!(
                metric.attributes.get("provider"),
                Some(&AttributeValue::String("test".into()))
            );
            assert_eq!(
                metric.attributes.get("model"),
                Some(&AttributeValue::String("model".into()))
            );
            assert_eq!(metric.kind, kiana_telemetry::MetricKind::Counter);
            assert_eq!(metric.unit.as_deref(), Some("token"));
            match metric.attributes.get("kind") {
                Some(AttributeValue::String(kind)) => kind.clone(),
                other => panic!("missing token kind: {other:?}"),
            }
        })
        .collect();
    assert_eq!(
        token_kinds,
        ["input", "output", "cache_read", "cache_write"]
    );
    assert_eq!(metrics[6].unit.as_deref(), Some("USD"));
    assert_eq!(metrics[6].value, 1.0);
    assert_eq!(
        metrics[6].attributes,
        attributes(&[("provider", "test"), ("model", "model")])
    );
    assert!(metrics[..2].iter().all(|metric| metric.context.is_some()));
}

#[tokio::test]
async fn records_usage_for_a_poll_only_when_final() {
    for reason in [
        StopReason::Pending,
        StopReason::Deferred,
        StopReason::Stop,
        StopReason::Error,
    ] {
        let telemetry = InMemoryTelemetryContext::new();
        let (models, _) = setup(message(reason));
        let model = telemetry_model();
        let handle = deferred_handle();
        in_caller_span(&telemetry, move |span| {
            let models = Arc::clone(&models);
            let model = model.clone();
            let handle = handle.clone();
            Box::pin(async move {
                models
                    .fetch_deferred(
                        &model,
                        &handle,
                        ModelsDeferredFetchOptions {
                            base: kiana_ai::types::DeferredFetchOptions {
                                base: StreamOptions {
                                    telemetry: Some(Arc::clone(&span)),
                                    ..Default::default()
                                },
                                wait: None,
                            },
                            ..Default::default()
                        },
                    )
                    .await;
            })
        })
        .await;
        settled(&telemetry).await;
        let counters = telemetry
            .get_metrics()
            .into_iter()
            .filter(|metric| metric.kind == kiana_telemetry::MetricKind::Counter)
            .count();
        let expected = if matches!(reason, StopReason::Pending | StopReason::Deferred) {
            0
        } else {
            5
        };
        assert_eq!(counters, expected, "reason {reason:?}");
    }
}

#[tokio::test]
async fn creates_instruments_once_per_metrics_object_across_root_and_child_requests() {
    let telemetry = InMemoryTelemetryContext::new();
    let (models, _) = setup(message(StopReason::Stop));
    let model = telemetry_model();
    let handle = deferred_handle();
    let models_for_span = Arc::clone(&models);
    let model_for_span = model.clone();
    let handle_for_span = handle.clone();
    in_caller_span(&telemetry, move |span| {
        let models = Arc::clone(&models_for_span);
        let model = model_for_span.clone();
        let handle = handle_for_span.clone();
        Box::pin(async move {
            models
                .complete_simple(
                    &model,
                    &kiana_ai::types::Context::default(),
                    ModelsSimpleStreamOptions {
                        base: SimpleStreamOptions {
                            base: StreamOptions {
                                telemetry: Some(Arc::clone(&span)),
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )
                .await;
            models
                .fetch_deferred(
                    &model,
                    &handle,
                    ModelsDeferredFetchOptions {
                        base: kiana_ai::types::DeferredFetchOptions {
                            base: StreamOptions {
                                telemetry: Some(Arc::clone(&span)),
                                ..Default::default()
                            },
                            wait: None,
                        },
                        ..Default::default()
                    },
                )
                .await;
            models
                .complete(
                    &model,
                    &kiana_ai::types::Context::default(),
                    ModelsStreamOptions {
                        base: StreamOptions {
                            telemetry: Some(Arc::clone(&span)),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )
                .await;
        })
    })
    .await;
    settled(&telemetry).await;
    // 3 次请求 × 7 个测量（2 histogram + 4 token counter + 1 cost counter）。
    let metrics = telemetry.get_metrics();
    assert_eq!(metrics.len(), 21);
    let names: Vec<&str> = metrics.iter().map(|metric| metric.name.as_str()).collect();
    let per_request = [
        "kiana.ai.first_chunk.duration",
        "kiana.ai.request.duration",
        "kiana.ai.tokens",
        "kiana.ai.tokens",
        "kiana.ai.tokens",
        "kiana.ai.tokens",
        "kiana.ai.cost",
    ];
    for chunk in names.chunks(7) {
        assert_eq!(chunk, per_request);
    }
    // instrument 的创建次数不可 spy（`InMemoryTelemetryContext` 不记录 instrument 构造），
    // 由 `src/telemetry.rs` 的 `instrument_cache_is_keyed_by_the_metrics_object_identity`
    // 直接断言 WeakMap 缓存按 adapter 复用同一组 instrument（TS 的 `vi.spyOn` 断言）。
}

/// 与 TS 参考实现生成的 `AI_TELEMETRY_SCHEMA` 黄金文件逐字节比较（JS JSON 语义）。
#[test]
fn schema_matches_the_typescript_golden() {
    let golden: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/models-runtime/ai-telemetry-schema.json"
    ))
    .unwrap();
    let actual = serde_json::to_value(&*AI_TELEMETRY_SCHEMA).unwrap();
    assert_eq!(
        kiana_json::js_object::stringify(&actual),
        kiana_json::js_object::stringify(&golden)
    );
}

/// TS 的 `startAttributes = { ...{provider, model}, api, ...{reasoning} }`：InMemory recorder
/// 保留插入序，因此记录下来的 span 属性键序必须是 provider、model、api、reasoning、……
#[tokio::test]
async fn records_start_attributes_in_the_ts_key_order() {
    let telemetry = observe_method("stream", message(StopReason::Stop)).await;
    settled(&telemetry).await;
    let span = &telemetry.get_spans()[1];
    let keys: Vec<&str> = span.attributes.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "provider",
            "model",
            "api",
            "reasoning",
            "stop",
            "usage.input",
            "usage.output",
            "usage.cache_read",
            "usage.cache_write",
            "cost.total",
            "response.id",
        ]
    );
}

// ---------------------------------------------------------------------------
// telemetry.test.ts 的 `describe("lazyStream observation")`。
//
// Rust 的 `LazySetup` 总是返回 `AssistantMessageEventStream`（自带 `result()`），因此 TS 里
// 「没有 result 方法的 async iterable」这一形态不存在；其余三条逐条对应。TS 断言里依赖
// microtask 的 `["event", "resolved", "result"]` 交错顺序在 Rust 的 sync 回调 + tokio
// 调度下不可观察（`on_event` 不能同步 await `result()`），改为断言可观察的等价事实。
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lazy_stream_observes_after_push_and_reports_the_terminal_result_once() {
    use kiana_ai::api::lazy::{LazySetup, LazyStreamObserver, lazy_stream};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let result = message(StopReason::Stop);
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let iterators = Arc::new(AtomicUsize::new(0));
    let source_result = result.clone();
    let source_iterators = Arc::clone(&iterators);
    let setup: LazySetup = Box::pin(async move {
        source_iterators.fetch_add(1, Ordering::SeqCst);
        let inner = kiana_ai::event_stream::AssistantMessageEventStream::new();
        let _ = inner.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: Box::new(source_result.clone()),
        });
        inner.end(source_result);
        Ok(inner)
    });
    let events_order = Arc::clone(&order);
    let result_order = Arc::clone(&order);
    let expected = result.clone();
    let observer = LazyStreamObserver {
        on_event: Some(Box::new(move |_event| {
            events_order
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push("event".into());
        })),
        on_result: Some(Box::new(move |received| {
            assert_eq!(received, Some(&expected));
            result_order
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push("result".into());
        })),
    };

    let stream = lazy_stream(&telemetry_model(), setup, Some(observer));
    // 同步返回的流可以在观察者回调之外读取结果。
    let received = stream.result().await.unwrap();
    assert_eq!(received, result);
    wait_for(|| {
        order
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
            == 2
    })
    .await;
    assert_eq!(
        *order.lock().unwrap_or_else(|error| error.into_inner()),
        ["event", "result"]
    );
    assert_eq!(iterators.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn lazy_stream_reports_a_terminal_result_from_events_when_the_source_has_no_result() {
    use kiana_ai::api::lazy::{LazySetup, LazyStreamObserver, lazy_stream};
    use std::sync::Mutex;

    // TS 的「没有 result 方法的 iterable」在 Rust 里等价于：流只推送 done 事件并由
    // `end_without_result()` 结束（result 只来自事件）。
    let result = message(StopReason::Stop);
    let observed: Arc<Mutex<Vec<Option<AssistantMessage>>>> = Arc::new(Mutex::new(Vec::new()));
    let source_result = result.clone();
    let setup: LazySetup = Box::pin(async move {
        let inner = kiana_ai::event_stream::AssistantMessageEventStream::new();
        let _ = inner.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: Box::new(source_result.clone()),
        });
        inner.end_without_result();
        Ok(inner)
    });
    let observed_for_observer = Arc::clone(&observed);
    let observer = LazyStreamObserver {
        on_event: None,
        on_result: Some(Box::new(move |received| {
            observed_for_observer
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(received.cloned());
        })),
    };
    let stream = lazy_stream(&telemetry_model(), setup, Some(observer));
    assert_eq!(stream.result().await.unwrap(), result);
    wait_for(|| {
        !observed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    })
    .await;
    assert_eq!(
        *observed.lock().unwrap_or_else(|error| error.into_inner()),
        [Some(result)]
    );
}

#[tokio::test]
async fn lazy_stream_observes_a_result_only_source_once() {
    use kiana_ai::api::lazy::{LazySetup, LazyStreamObserver, lazy_stream};
    use std::sync::Mutex;

    let result = message(StopReason::Stop);
    let observed: Arc<Mutex<Vec<Option<AssistantMessage>>>> = Arc::new(Mutex::new(Vec::new()));
    let source_result = result.clone();
    let setup: LazySetup = Box::pin(async move {
        let inner = kiana_ai::event_stream::AssistantMessageEventStream::new();
        inner.end(source_result);
        Ok(inner)
    });
    let observed_for_observer = Arc::clone(&observed);
    let observer = LazyStreamObserver {
        on_event: None,
        on_result: Some(Box::new(move |received| {
            observed_for_observer
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(received.cloned());
        })),
    };
    let stream = lazy_stream(&telemetry_model(), setup, Some(observer));
    assert_eq!(stream.result().await.unwrap(), result);
    wait_for(|| {
        !observed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    })
    .await;
    assert_eq!(
        *observed.lock().unwrap_or_else(|error| error.into_inner()),
        [Some(result)]
    );
}

#[tokio::test]
async fn lazy_stream_reports_setup_failure_events_and_the_result_once() {
    use kiana_ai::api::lazy::{LazySetup, LazyStreamObserver, lazy_stream};
    use std::sync::Mutex;

    let observed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let setup: LazySetup = Box::pin(async {
        Err(kiana_ai::error::AiError::new(
            kiana_ai::error::AiErrorKind::Other,
            "setup-secret",
        ))
    });
    let events_observed = Arc::clone(&observed);
    let result_observed = Arc::clone(&observed);
    let observer = LazyStreamObserver {
        on_event: Some(Box::new(move |event| {
            events_observed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(match event {
                    AssistantMessageEvent::Error { .. } => "error".into(),
                    other => format!("{other:?}"),
                });
        })),
        on_result: Some(Box::new(move |received| {
            result_observed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(
                    received
                        .map(|message| format!("{:?}", message.stop_reason))
                        .unwrap_or_else(|| "missing".into()),
                );
        })),
    };
    let stream = lazy_stream(&telemetry_model(), setup, Some(observer));
    let result = stream.result().await.unwrap();
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("setup-secret"));
    wait_for(|| {
        observed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
            == 2
    })
    .await;
    let observed = observed.lock().unwrap_or_else(|error| error.into_inner());
    assert_eq!(observed[0], "error");
    assert_eq!(observed[1], "Error");
}
