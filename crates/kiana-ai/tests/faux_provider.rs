//! `packages/ai/test/faux-provider.test.ts` 的 Rust 移植。
//!
//! TS 测试用 `registerFauxProvider` 把核心注册进全局 api-registry；Rust 侧用
//! [`faux_provider`] + `createModels().setProvider(...)`（与 TS 的 `fauxProvider()` 用法
//! 等价）。`unregister` 通过 `kiana_ai::compat::register_faux_provider` 的注册句柄验证
//! （compat 单元已合入）。

use std::sync::Arc;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use kiana_ai::models::{
    CreateModelsOptions, ModelsSimpleStreamOptions, ModelsStreamOptions, MutableModels,
    create_models,
};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxContent, FauxDeferredOptions, FauxModelDefinition,
    FauxProviderHandle, FauxResponseStep, FauxTokenSize, FauxToolCallOptions,
    RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_provider_with,
    faux_text, faux_thinking, faux_tool_call, faux_tool_call_with,
};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, DeferredRequest,
    ErrorReason, JsonObject, Message, Model, ProviderResponse, SimpleStreamOptions, StopReason,
    StreamOptions, TextContent, TextKind, Tool, UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::diagnostics::ThrownValue;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn models_with(handle: &FauxProviderHandle) -> Arc<dyn MutableModels> {
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(handle.provider.clone());
    models
}

fn user_message(content: impl Into<String>) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(content.into()),
        timestamp: now(),
    })
}

fn user_block_message(blocks: Vec<kiana_ai::types::UserContentBlock>) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(blocks),
        timestamp: 1,
    })
}

fn context(messages: Vec<Message>) -> Context {
    Context {
        system_prompt: None,
        messages,
        tools: None,
    }
}

fn user_context(content: &str) -> Context {
    context(vec![user_message(content)])
}

async fn complete(
    models: &Arc<dyn MutableModels>,
    model: &Model,
    context: &Context,
    options: StreamOptions,
) -> AssistantMessage {
    models
        .complete(model, context, ModelsStreamOptions::from(options))
        .await
}

async fn collect_events(
    models: &Arc<dyn MutableModels>,
    model: &Model,
    context: &Context,
    options: StreamOptions,
) -> Vec<AssistantMessageEvent> {
    let stream = models.stream(model, context, ModelsStreamOptions::from(options));
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    events
}

fn obj(value: Value) -> JsonObject {
    serde_json::from_value(value).expect("JSON object")
}

fn text_content(text: impl Into<String>) -> AssistantContentBlock {
    AssistantContentBlock::Text(faux_text(text))
}

fn thinking_content(thinking: impl Into<String>) -> AssistantContentBlock {
    AssistantContentBlock::Thinking(faux_thinking(thinking))
}

fn tool_call_content(tool_call: kiana_ai::types::ToolCall) -> AssistantContentBlock {
    AssistantContentBlock::ToolCall(tool_call)
}

fn text_block(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })
}

#[test]
fn treats_an_empty_model_id_as_missing_in_get_model() {
    // TS: `if (!requestedModelId) return models[0]` —— 空字符串为假，返回第一个模型。
    let faux = faux_provider();
    let first = faux.models()[0].id.clone();
    assert_eq!(
        faux.get_model(None).map(|model| model.id.clone()),
        Some(first.clone())
    );
    assert_eq!(
        faux.get_model(Some("")).map(|model| model.id.clone()),
        Some(first)
    );
    assert!(faux.get_model(Some("missing-model")).is_none());
}

#[tokio::test]
async fn registers_a_custom_provider_and_estimates_usage() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message("hello world", FauxAssistantMessageOptions::default()).into(),
    ]);

    let context = Context {
        system_prompt: Some("Be concise.".into()),
        messages: vec![user_message("hi there")],
        tools: None,
    };

    let response = complete(
        &models,
        faux.get_model(None).unwrap(),
        &context,
        StreamOptions::default(),
    )
    .await;
    assert_eq!(response.content, vec![text_block("hello world")]);
    assert!(response.usage.input > 0);
    assert!(response.usage.output > 0);
    assert_eq!(
        response.usage.total_tokens,
        response.usage.input + response.usage.output
    );
    assert_eq!(faux.state().call_count(), 1);
}

#[tokio::test]
async fn supports_helper_blocks_for_text_thinking_and_tool_calls() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message(
            FauxContent::Blocks(vec![
                thinking_content("think"),
                tool_call_content(faux_tool_call("echo", obj(json!({ "text": "hi" })))),
                text_content("done"),
            ]),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
        )
        .into(),
    ]);

    let response = complete(
        &models,
        faux.get_model(None).unwrap(),
        &user_context("hi"),
        StreamOptions::default(),
    )
    .await;

    assert_eq!(response.content.len(), 3);
    match &response.content[0] {
        AssistantContentBlock::Thinking(thinking) => assert_eq!(thinking.thinking, "think"),
        other => panic!("expected thinking block, got {other:?}"),
    }
    match &response.content[1] {
        AssistantContentBlock::ToolCall(tool_call) => {
            assert!(!tool_call.id.is_empty());
            assert_eq!(tool_call.name, "echo");
            assert_eq!(tool_call.arguments, obj(json!({ "text": "hi" })));
        }
        other => panic!("expected tool call block, got {other:?}"),
    }
    assert_eq!(response.content[2], text_block("done"));
    assert_eq!(response.stop_reason, StopReason::ToolUse);
}

#[tokio::test]
async fn supports_multiple_models_with_per_model_reasoning() {
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        models: Some(vec![
            FauxModelDefinition {
                id: "faux-fast".into(),
                name: Some("Faux Fast".into()),
                reasoning: Some(false),
                ..Default::default()
            },
            FauxModelDefinition {
                id: "faux-thinker".into(),
                name: Some("Faux Thinker".into()),
                reasoning: Some(true),
                ..Default::default()
            },
        ]),
        ..Default::default()
    });
    let models = models_with(&faux);
    let factory = || {
        FauxResponseStep::message_factory(|input| async move {
            faux_assistant_message(
                format!(
                    "{}:{}",
                    input.model.id,
                    input.model.reasoning.unwrap_or(false)
                ),
                FauxAssistantMessageOptions::default(),
            )
        })
    };
    faux.set_responses(vec![factory(), factory()]);

    let ids: Vec<&str> = faux
        .models()
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(ids, vec!["faux-fast", "faux-thinker"]);
    assert_eq!(faux.get_model(None).unwrap().id, "faux-fast");
    assert_eq!(
        faux.get_model(Some("faux-fast")).unwrap().reasoning,
        Some(false)
    );
    assert_eq!(
        faux.get_model(Some("faux-thinker")).unwrap().reasoning,
        Some(true)
    );

    let fast = complete(
        &models,
        faux.get_model(Some("faux-fast")).unwrap(),
        &user_context("hi"),
        StreamOptions::default(),
    )
    .await;
    let thinker = complete(
        &models,
        faux.get_model(Some("faux-thinker")).unwrap(),
        &user_context("hi"),
        StreamOptions::default(),
    )
    .await;

    assert_eq!(fast.content, vec![text_block("faux-fast:false")]);
    assert_eq!(thinker.content, vec![text_block("faux-thinker:true")]);
}

#[tokio::test]
async fn rewrites_api_provider_and_model_on_returned_messages() {
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        api: Some("faux:test".into()),
        provider: Some("faux-provider".into()),
        models: Some(vec![FauxModelDefinition::new("faux-model")]),
        ..Default::default()
    });
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message("hello", FauxAssistantMessageOptions::default()).into(),
    ]);

    let response = complete(
        &models,
        faux.get_model(None).unwrap(),
        &user_context("hi"),
        StreamOptions::default(),
    )
    .await;

    assert_eq!(response.api, "faux:test");
    assert_eq!(response.provider, "faux-provider");
    assert_eq!(response.model, "faux-model");
}

#[tokio::test]
async fn consumes_queued_responses_in_order_and_errors_when_exhausted() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message("first", FauxAssistantMessageOptions::default()).into(),
        faux_assistant_message("second", FauxAssistantMessageOptions::default()).into(),
    ]);

    let context = user_context("hi");
    let model = faux.get_model(None).unwrap().clone();
    let first = complete(&models, &model, &context, StreamOptions::default()).await;
    let second = complete(&models, &model, &context, StreamOptions::default()).await;
    let exhausted = complete(&models, &model, &context, StreamOptions::default()).await;

    assert_eq!(first.content, vec![text_block("first")]);
    assert_eq!(second.content, vec![text_block("second")]);
    assert_eq!(exhausted.stop_reason, StopReason::Error);
    assert_eq!(
        exhausted.error_message.as_deref(),
        Some("No more faux responses queued")
    );
    assert_eq!(faux.get_pending_response_count(), 0);
    assert_eq!(faux.state().call_count(), 3);
}

#[tokio::test]
async fn can_replace_and_append_queued_responses() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message("first", FauxAssistantMessageOptions::default()).into(),
    ]);

    let context = user_context("hi");
    let model = faux.get_model(None).unwrap().clone();
    assert_eq!(
        complete(&models, &model, &context, StreamOptions::default())
            .await
            .content,
        vec![text_block("first")]
    );
    assert_eq!(faux.get_pending_response_count(), 0);

    faux.set_responses(vec![
        faux_assistant_message("second", FauxAssistantMessageOptions::default()).into(),
    ]);
    assert_eq!(faux.get_pending_response_count(), 1);
    assert_eq!(
        complete(&models, &model, &context, StreamOptions::default())
            .await
            .content,
        vec![text_block("second")]
    );

    faux.append_responses(vec![
        faux_assistant_message("third", FauxAssistantMessageOptions::default()).into(),
        faux_assistant_message("fourth", FauxAssistantMessageOptions::default()).into(),
    ]);
    assert_eq!(faux.get_pending_response_count(), 2);
    assert_eq!(
        complete(&models, &model, &context, StreamOptions::default())
            .await
            .content,
        vec![text_block("third")]
    );
    assert_eq!(
        complete(&models, &model, &context, StreamOptions::default())
            .await
            .content,
        vec![text_block("fourth")]
    );
    assert_eq!(faux.get_pending_response_count(), 0);
}

#[tokio::test]
async fn supports_async_response_factories() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![FauxResponseStep::message_factory(
        |input| async move {
            faux_assistant_message(
                format!(
                    "{}:{}",
                    input.context.as_context().messages.len(),
                    input.state.call_count()
                ),
                FauxAssistantMessageOptions::default(),
            )
        },
    )]);

    let response = complete(
        &models,
        faux.get_model(None).unwrap(),
        &user_context("hi"),
        StreamOptions::default(),
    )
    .await;

    assert_eq!(response.content, vec![text_block("1:1")]);
}

#[tokio::test]
async fn emits_an_error_when_a_response_factory_throws() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![FauxResponseStep::factory(|_input| async move {
        Err(ThrownValue::from_text("boom"))
    })]);

    let events = collect_events(
        &models,
        faux.get_model(None).unwrap(),
        &user_context("hi"),
        StreamOptions::default(),
    )
    .await;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind(), "error");
    match &events[0] {
        AssistantMessageEvent::Error { error, .. } => {
            assert_eq!(error.stop_reason, StopReason::Error);
            assert_eq!(error.error_message.as_deref(), Some("boom"));
        }
        other => panic!("expected error event, got {other:?}"),
    }
}

#[tokio::test]
async fn rejects_a_queued_response_without_a_terminal_stop_reason() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message(
            "partial",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Pending),
                ..Default::default()
            },
        )
        .into(),
    ]);

    let events = collect_events(
        &models,
        faux.get_model(None).unwrap(),
        &user_context("hi"),
        StreamOptions::default(),
    )
    .await;

    assert!(!events.iter().any(|event| event.kind() == "done"));
    let terminal = events.last().expect("terminal event");
    assert_eq!(terminal.kind(), "error");
    match terminal {
        AssistantMessageEvent::Error { error, .. } => {
            assert_eq!(error.stop_reason, StopReason::Error);
            assert_eq!(
                error.error_message.as_deref(),
                Some("Faux response ended without a stop reason")
            );
        }
        other => panic!("expected error event, got {other:?}"),
    }
}

#[tokio::test]
async fn estimates_prompt_and_output_tokens_from_serialized_context() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message("done", FauxAssistantMessageOptions::default()).into(),
    ]);

    let tool = Tool {
        name: "echo".into(),
        description: "Echo back text".into(),
        parameters: json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
        }),
        constrained_sampling: None,
    };
    let context = Context {
        system_prompt: Some("sys".into()),
        messages: vec![
            user_block_message(vec![
                kiana_ai::types::UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "hello".into(),
                    text_signature: None,
                }),
                kiana_ai::types::UserContentBlock::Image(kiana_ai::types::ImageContent {
                    kind_image: kiana_ai::types::ImageKind::Image,
                    data: "abcd".into(),
                    mime_type: "image/png".into(),
                }),
            ]),
            Message::Assistant(faux_assistant_message(
                "prior",
                FauxAssistantMessageOptions::default(),
            )),
            Message::ToolResult(kiana_ai::types::ToolResultMessage {
                role: kiana_ai::types::ToolResultRole::ToolResult,
                tool_call_id: "tool-1".into(),
                tool_name: "echo".into(),
                content: vec![kiana_ai::types::UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "tool out".into(),
                    text_signature: None,
                })],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 2,
                duration_ms: None,
            }),
        ],
        tools: Some(vec![tool.clone()]),
    };

    let response = complete(
        &models,
        faux.get_model(None).unwrap(),
        &context,
        StreamOptions::default(),
    )
    .await;

    let tool_json =
        serde_json::to_string(&serde_json::to_value(&tool).expect("tool serializes to JSON"))
            .expect("tool JSON text");
    let prompt_text = [
        "system:sys".to_string(),
        "user:hello\n[image:image/png:4]".to_string(),
        "assistant:prior".to_string(),
        "toolResult:echo\ntool out".to_string(),
        format!("tools:[{tool_json}]"),
    ]
    .join("\n\n");
    let expected_prompt_tokens = prompt_text.chars().count().div_ceil(4) as u64;
    let expected_output_tokens = 1u64;

    assert_eq!(response.usage.input, expected_prompt_tokens);
    assert_eq!(response.usage.output, expected_output_tokens);
    assert_eq!(response.usage.cache_read, 0);
    assert_eq!(response.usage.cache_write, 0);
    assert_eq!(
        response.usage.total_tokens,
        expected_prompt_tokens + expected_output_tokens
    );
}

#[tokio::test]
async fn does_not_share_cache_across_sessions_or_requests_without_session_id() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message("first", FauxAssistantMessageOptions::default()).into(),
        faux_assistant_message("second", FauxAssistantMessageOptions::default()).into(),
        faux_assistant_message("third", FauxAssistantMessageOptions::default()).into(),
    ]);

    let mut context = user_context("hello");
    let model = faux.get_model(None).unwrap().clone();
    let cached = |session_id: &str| StreamOptions {
        session_id: Some(session_id.into()),
        cache_retention: Some(kiana_ai::types::CacheRetention::Short),
        ..Default::default()
    };

    let first = complete(&models, &model, &context, cached("session-1")).await;
    assert!(first.usage.cache_write > 0);
    context.messages.push(Message::Assistant(first.clone()));
    context.messages.push(user_message("follow up"));

    let second = complete(&models, &model, &context, cached("session-2")).await;
    assert_eq!(second.usage.cache_read, 0);
    assert!(second.usage.cache_write > 0);

    let third = complete(&models, &model, &context, StreamOptions::default()).await;
    assert_eq!(third.usage.cache_read, 0);
    assert_eq!(third.usage.cache_write, 0);
}

#[tokio::test]
async fn simulates_prompt_caching_per_session_id() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message("first", FauxAssistantMessageOptions::default()).into(),
        faux_assistant_message("second", FauxAssistantMessageOptions::default()).into(),
    ]);

    let mut context = Context {
        system_prompt: Some("Be concise.".into()),
        messages: vec![user_message("hello")],
        tools: None,
    };
    let model = faux.get_model(None).unwrap().clone();
    let options = || StreamOptions {
        session_id: Some("session-1".into()),
        cache_retention: Some(kiana_ai::types::CacheRetention::Short),
        ..Default::default()
    };

    let first = complete(&models, &model, &context, options()).await;
    assert_eq!(first.usage.cache_read, 0);
    assert!(first.usage.cache_write > 0);

    context.messages.push(Message::Assistant(first.clone()));
    context.messages.push(user_message("follow up"));

    let second = complete(&models, &model, &context, options()).await;
    assert!(second.usage.cache_read > 0);
    assert!(second.usage.input + second.usage.cache_read > second.usage.input);
}

#[tokio::test]
async fn counts_cached_characters_up_to_the_first_difference_in_the_joined_prompt() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message("a", FauxAssistantMessageOptions::default()).into(),
        faux_assistant_message("b", FauxAssistantMessageOptions::default()).into(),
        faux_assistant_message("c", FauxAssistantMessageOptions::default()).into(),
    ]);
    let model = faux.get_model(None).unwrap().clone();
    let options = || StreamOptions {
        session_id: Some("session-1".into()),
        cache_retention: Some(kiana_ai::types::CacheRetention::Short),
        ..Default::default()
    };

    // Prompt texts: "user:hello world" (16 characters), then 16 + 2 + "user:next" (9) = 27.
    let first = complete(
        &models,
        &model,
        &context(vec![user_message("hello world")]),
        options(),
    )
    .await;
    assert_eq!(first.usage.input, 0);
    assert_eq!(first.usage.cache_read, 0);
    assert_eq!(first.usage.cache_write, 4);

    let extended = complete(
        &models,
        &model,
        &context(vec![user_message("hello world"), user_message("next")]),
        options(),
    )
    .await;
    assert_eq!(extended.usage.input, 0);
    assert_eq!(extended.usage.cache_read, 4);
    assert_eq!(extended.usage.cache_write, 3);

    // The first message now differs after "user:hello w" (12 characters): "user:hello wide" + 2 + 9 = 26.
    let edited = complete(
        &models,
        &model,
        &context(vec![user_message("hello wide"), user_message("next")]),
        options(),
    )
    .await;
    assert_eq!(edited.usage.input, 0);
    assert_eq!(edited.usage.cache_read, 3);
    assert_eq!(edited.usage.cache_write, 4);
}

#[tokio::test]
async fn does_not_simulate_caching_when_cache_retention_is_none() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message("first", FauxAssistantMessageOptions::default()).into(),
        faux_assistant_message("second", FauxAssistantMessageOptions::default()).into(),
    ]);

    let mut context = user_context("hello");
    let model = faux.get_model(None).unwrap().clone();
    let options = || StreamOptions {
        session_id: Some("session-1".into()),
        cache_retention: Some(kiana_ai::types::CacheRetention::None),
        ..Default::default()
    };

    let first = complete(&models, &model, &context, options()).await;
    context.messages.push(Message::Assistant(first));
    context.messages.push(user_message("follow up"));
    let second = complete(&models, &model, &context, options()).await;
    assert_eq!(second.usage.cache_read, 0);
    assert_eq!(second.usage.cache_write, 0);
}

#[tokio::test]
async fn streams_thinking_text_and_partial_tool_call_deltas() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message(
            FauxContent::Blocks(vec![
                thinking_content("thinking text"),
                text_content("answer text"),
                tool_call_content(faux_tool_call_with(
                    "echo",
                    obj(json!({ "text": "hi", "count": 12 })),
                    FauxToolCallOptions {
                        id: Some("tool-1".into()),
                    },
                )),
            ]),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
        )
        .into(),
    ]);

    let events = collect_events(
        &models,
        faux.get_model(None).unwrap(),
        &user_context("hi"),
        StreamOptions::default(),
    )
    .await;

    let kinds: Vec<&str> = events.iter().map(AssistantMessageEvent::kind).collect();
    for expected in [
        "thinking_start",
        "thinking_delta",
        "text_start",
        "text_delta",
        "toolcall_start",
        "toolcall_delta",
        "toolcall_end",
    ] {
        assert!(kinds.contains(&expected), "missing {expected} in {kinds:?}");
    }
    let tool_call_deltas: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AssistantMessageEvent::ToolCallDelta { delta, .. } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert!(tool_call_deltas.len() > 1);
    let arguments: Value =
        serde_json::from_str(&tool_call_deltas.concat()).expect("partial arguments are valid JSON");
    assert_eq!(arguments, json!({ "text": "hi", "count": 12 }));
}

#[tokio::test]
async fn streams_an_exact_event_order_for_fixed_size_chunks() {
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    });
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message(
            FauxContent::Blocks(vec![
                thinking_content("go"),
                text_content("ok"),
                tool_call_content(faux_tool_call_with(
                    "echo",
                    JsonObject::new(),
                    FauxToolCallOptions {
                        id: Some("tool-1".into()),
                    },
                )),
            ]),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
        )
        .into(),
    ]);

    let events = collect_events(
        &models,
        faux.get_model(None).unwrap(),
        &user_context("hi"),
        StreamOptions::default(),
    )
    .await;

    match &events[0] {
        AssistantMessageEvent::Start { partial } => {
            assert_eq!(partial.stop_reason, StopReason::Pending)
        }
        other => panic!("expected start event, got {other:?}"),
    }
    let kinds: Vec<&str> = events.iter().map(AssistantMessageEvent::kind).collect();
    assert_eq!(
        kinds,
        vec![
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "text_start",
            "text_delta",
            "text_end",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_end",
            "done",
        ]
    );
}

#[tokio::test]
async fn streams_multiple_tool_calls_in_one_message() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message(
            FauxContent::Blocks(vec![
                tool_call_content(faux_tool_call_with(
                    "echo",
                    obj(json!({ "text": "one" })),
                    FauxToolCallOptions {
                        id: Some("tool-1".into()),
                    },
                )),
                tool_call_content(faux_tool_call_with(
                    "echo",
                    obj(json!({ "text": "two" })),
                    FauxToolCallOptions {
                        id: Some("tool-2".into()),
                    },
                )),
            ]),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
        )
        .into(),
    ]);

    let events = collect_events(
        &models,
        faux.get_model(None).unwrap(),
        &user_context("hi"),
        StreamOptions::default(),
    )
    .await;

    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind() == "toolcall_start")
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind() == "toolcall_end")
            .count(),
        2
    );
}

#[tokio::test]
async fn streams_an_explicit_assistant_error_message_as_a_terminal_error() {
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        token_size: Some(FauxTokenSize {
            min: Some(2),
            max: Some(2),
        }),
        ..Default::default()
    });
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message(
            "partial",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("upstream failed".into()),
                ..Default::default()
            },
        )
        .into(),
    ]);

    let events = collect_events(
        &models,
        faux.get_model(None).unwrap(),
        &user_context("hi"),
        StreamOptions::default(),
    )
    .await;

    let kinds: Vec<&str> = events.iter().map(AssistantMessageEvent::kind).collect();
    assert_eq!(
        kinds,
        vec!["start", "text_start", "text_delta", "text_end", "error"]
    );
    match events.last().unwrap() {
        AssistantMessageEvent::Error { reason, error } => {
            assert_eq!(*reason, ErrorReason::Error);
            assert_eq!(error.stop_reason, StopReason::Error);
            assert_eq!(error.error_message.as_deref(), Some("upstream failed"));
        }
        other => panic!("expected error event, got {other:?}"),
    }
}

#[tokio::test]
async fn streams_an_explicit_assistant_aborted_message_as_a_terminal_error() {
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        token_size: Some(FauxTokenSize {
            min: Some(2),
            max: Some(2),
        }),
        ..Default::default()
    });
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message(
            "partial",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Aborted),
                error_message: Some("Request was aborted".into()),
                ..Default::default()
            },
        )
        .into(),
    ]);

    let events = collect_events(
        &models,
        faux.get_model(None).unwrap(),
        &user_context("hi"),
        StreamOptions::default(),
    )
    .await;

    let kinds: Vec<&str> = events.iter().map(AssistantMessageEvent::kind).collect();
    assert_eq!(
        kinds,
        vec!["start", "text_start", "text_delta", "text_end", "error"]
    );
    match events.last().unwrap() {
        AssistantMessageEvent::Error { reason, error } => {
            assert_eq!(*reason, ErrorReason::Aborted);
            assert_eq!(error.stop_reason, StopReason::Aborted);
            assert_eq!(error.error_message.as_deref(), Some("Request was aborted"));
        }
        other => panic!("expected error event, got {other:?}"),
    }
}

#[tokio::test]
async fn supports_aborting_before_the_first_chunk() {
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        tokens_per_second: Some(50.0),
        token_size: Some(FauxTokenSize {
            min: Some(3),
            max: Some(3),
        }),
        ..Default::default()
    });
    faux.set_responses(vec![
        faux_assistant_message(
            "abcdefghijklmnopqrstuvwxyz",
            FauxAssistantMessageOptions::default(),
        )
        .into(),
    ]);

    // TS 的 compat `stream` 直接调用 provider（不进 Models 的 auth 流程），因此这里也直接
    // 调用 provider：已中止的 signal 必须由 faux 自己映射成 aborted 事件。
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let transcript = kiana_ai::utils::transcript::normalize_context(&user_context("hi"));
    let stream = faux.provider.stream(
        faux.get_model(None).unwrap(),
        &transcript,
        StreamOptions {
            signal: Some(cancellation),
            ..Default::default()
        },
    );
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }

    assert_eq!(events.len(), 1);
    match &events[0] {
        AssistantMessageEvent::Error { reason, error } => {
            assert_eq!(*reason, ErrorReason::Aborted);
            assert_eq!(error.stop_reason, StopReason::Aborted);
        }
        other => panic!("expected error event, got {other:?}"),
    }
}

#[tokio::test]
async fn supports_aborting_mid_text_stream_when_paced() {
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        tokens_per_second: Some(100.0),
        token_size: Some(FauxTokenSize {
            min: Some(3),
            max: Some(3),
        }),
        ..Default::default()
    });
    faux.set_responses(vec![
        faux_assistant_message(
            "abcdefghijklmnopqrstuvwxyz",
            FauxAssistantMessageOptions::default(),
        )
        .into(),
    ]);

    let models = models_with(&faux);
    let cancellation = CancellationToken::new();
    let stream = models.stream(
        faux.get_model(None).unwrap(),
        &user_context("hi"),
        ModelsStreamOptions::from(StreamOptions {
            signal: Some(cancellation.clone()),
            ..Default::default()
        }),
    );
    let mut kinds = Vec::new();
    let mut text_delta_count = 0;
    while let Some(event) = stream.recv().await {
        if event.kind() == "text_delta" {
            text_delta_count += 1;
            cancellation.cancel();
        }
        kinds.push(event.kind().to_string());
    }

    assert_eq!(text_delta_count, 1);
    assert!(kinds.iter().any(|kind| kind == "text_start"));
    assert!(kinds.iter().any(|kind| kind == "text_delta"));
    assert!(kinds.iter().any(|kind| kind == "error"));
    assert!(!kinds.iter().any(|kind| kind == "text_end"));
}

#[tokio::test]
async fn supports_aborting_mid_thinking_stream_when_paced() {
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        tokens_per_second: Some(100.0),
        token_size: Some(FauxTokenSize {
            min: Some(3),
            max: Some(3),
        }),
        ..Default::default()
    });
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message(
            FauxContent::Blocks(vec![thinking_content("abcdefghijklmnopqrstuvwxyz")]),
            FauxAssistantMessageOptions::default(),
        )
        .into(),
    ]);

    let cancellation = CancellationToken::new();
    let stream = models.stream(
        faux.get_model(None).unwrap(),
        &user_context("hi"),
        ModelsStreamOptions::from(StreamOptions {
            signal: Some(cancellation.clone()),
            ..Default::default()
        }),
    );
    let mut kinds = Vec::new();
    let mut thinking_delta_count = 0;
    while let Some(event) = stream.recv().await {
        if event.kind() == "thinking_delta" {
            thinking_delta_count += 1;
            cancellation.cancel();
        }
        kinds.push(event.kind().to_string());
    }

    assert_eq!(thinking_delta_count, 1);
    assert!(kinds.iter().any(|kind| kind == "thinking_start"));
    assert!(kinds.iter().any(|kind| kind == "thinking_delta"));
    assert!(kinds.iter().any(|kind| kind == "error"));
    assert!(!kinds.iter().any(|kind| kind == "thinking_end"));
}

#[tokio::test]
async fn supports_aborting_mid_toolcall_stream_when_paced() {
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        tokens_per_second: Some(100.0),
        token_size: Some(FauxTokenSize {
            min: Some(3),
            max: Some(3),
        }),
        ..Default::default()
    });
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message(
            FauxContent::Blocks(vec![tool_call_content(faux_tool_call_with(
                "echo",
                obj(json!({
                    "text": "abcdefghijklmnopqrstuvwxyz",
                    "count": 123456789
                })),
                FauxToolCallOptions {
                    id: Some("tool-1".into()),
                },
            ))]),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
        )
        .into(),
    ]);

    let cancellation = CancellationToken::new();
    let stream = models.stream(
        faux.get_model(None).unwrap(),
        &user_context("hi"),
        ModelsStreamOptions::from(StreamOptions {
            signal: Some(cancellation.clone()),
            ..Default::default()
        }),
    );
    let mut kinds = Vec::new();
    let mut tool_call_delta_count = 0;
    while let Some(event) = stream.recv().await {
        if event.kind() == "toolcall_delta" {
            tool_call_delta_count += 1;
            cancellation.cancel();
        }
        kinds.push(event.kind().to_string());
    }

    assert_eq!(tool_call_delta_count, 1);
    assert!(kinds.iter().any(|kind| kind == "toolcall_start"));
    assert!(kinds.iter().any(|kind| kind == "toolcall_delta"));
    assert!(kinds.iter().any(|kind| kind == "error"));
    assert!(!kinds.iter().any(|kind| kind == "toolcall_end"));
}

#[tokio::test]
async fn unregisters_the_provider() {
    let registration =
        kiana_ai::compat::register_faux_provider(RegisterFauxProviderOptions::default());
    registration.set_responses(vec![
        faux_assistant_message("hello", FauxAssistantMessageOptions::default()).into(),
    ]);
    let api = registration.api().to_string();
    let model = registration.get_model(None).expect("model").clone();
    registration.unregister();

    let message = kiana_ai::compat::complete(
        &model,
        &context(vec![user_message("hi")]),
        StreamOptions::default(),
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some(format!("No API provider registered for api: {api}").as_str())
    );
}

/// 延迟响应：提交后返回 handle；`pendingFetches` 期间重复返回 handle，之后返回最终消息。
#[tokio::test]
async fn supports_deferred_responses() {
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        deferred: Some(FauxDeferredOptions {
            pending_fetches: Some(1.0),
            poll_after_ms: Some(25.0),
        }),
        ..Default::default()
    });
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message("deferred answer", FauxAssistantMessageOptions::default()).into(),
    ]);

    let model = faux.get_model(None).unwrap().clone();
    let options = kiana_ai::types::SimpleStreamOptions {
        deferred: Some(kiana_ai::types::DeferredRequest::Enabled(true)),
        ..Default::default()
    };
    let submitted = models
        .complete_simple(
            &model,
            &user_context("hi"),
            kiana_ai::models::ModelsSimpleStreamOptions::from(options),
        )
        .await;
    assert_eq!(submitted.stop_reason, StopReason::Deferred);
    let handle = submitted.deferred.clone().expect("deferred handle");
    assert_eq!(handle.poll_after_ms, Some(25.0));

    let pending = models
        .fetch_deferred(&model, &handle, Default::default())
        .await;
    assert_eq!(pending.stop_reason, StopReason::Deferred);

    let final_message = models
        .fetch_deferred(&model, &handle, Default::default())
        .await;
    assert_eq!(final_message.stop_reason, StopReason::Stop);
    assert_eq!(final_message.content, vec![text_block("deferred answer")]);
    assert_eq!(faux.state().deferred_fetch_count(), 2);

    // 后续轮询复用缓存，不重算。
    let cached = models
        .fetch_deferred(&model, &handle, Default::default())
        .await;
    assert_eq!(cached.content, final_message.content);
}

/// 取消后 `fetchDeferred` 返回错误消息。
#[tokio::test]
async fn cancels_deferred_responses() {
    let faux = faux_provider();
    let models = models_with(&faux);
    faux.set_responses(vec![
        faux_assistant_message("answer", FauxAssistantMessageOptions::default()).into(),
    ]);

    let model = faux.get_model(None).unwrap().clone();
    let submitted = models
        .complete_simple(
            &model,
            &user_context("hi"),
            kiana_ai::models::ModelsSimpleStreamOptions::from(
                kiana_ai::types::SimpleStreamOptions {
                    deferred: Some(kiana_ai::types::DeferredRequest::Enabled(true)),
                    ..Default::default()
                },
            ),
        )
        .await;
    let handle = submitted.deferred.clone().expect("deferred handle");
    models
        .cancel_deferred(&model, &handle, Default::default())
        .await
        .expect("cancel succeeds");
    assert_eq!(faux.state().cancelled_deferred().len(), 1);

    let message = models
        .fetch_deferred(&model, &handle, Default::default())
        .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some(format!("Faux deferred response was cancelled: {}", handle.id).as_str())
    );
}

/// 工厂在 `options` 里看到的载体（TS: 工厂收到原始 `SimpleStreamOptions`）。
#[derive(Debug, Clone, Default)]
struct ObservedFactoryOptions {
    present: bool,
    deferred: Option<DeferredRequest>,
    signal_present: bool,
    signal_matches: bool,
    on_response: bool,
    on_payload: bool,
    on_provider_stream_event: bool,
}

fn observe_factory_options(
    input: &kiana_ai::providers::faux::FauxResponseInput,
    expected_signal: Option<&CancellationToken>,
) -> ObservedFactoryOptions {
    let options = input.options.as_ref();
    ObservedFactoryOptions {
        present: options.is_some(),
        deferred: options.and_then(|options| options.deferred),
        signal_present: options.is_some_and(|options| options.signal.is_some()),
        signal_matches: match (
            options.and_then(|options| options.signal.as_ref()),
            expected_signal,
        ) {
            (Some(actual), Some(expected)) => actual == expected,
            (None, None) => true,
            _ => false,
        },
        on_response: options.is_some_and(|options| options.on_response.is_some()),
        on_payload: options.is_some_and(|options| options.on_payload.is_some()),
        on_provider_stream_event: options
            .is_some_and(|options| options.on_provider_stream_event.is_some()),
    }
}

/// 回归（reviewer finding）：TS 的响应工厂收到原始 `SimpleStreamOptions`，能读取
/// `deferred`/`signal`/`onResponse`。Rust 的 `FauxStreamOptions` 视图曾把这些载体丢掉。
#[tokio::test]
async fn passes_stream_option_carriers_to_response_factories() {
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    });
    let models = models_with(&faux);
    let signal = CancellationToken::new();
    let on_response_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed: Arc<std::sync::Mutex<Option<ObservedFactoryOptions>>> =
        Arc::new(std::sync::Mutex::new(None));

    faux.set_responses(vec![FauxResponseStep::message_factory({
        let observed = observed.clone();
        let expected_signal = signal.clone();
        move |input| {
            let observed = observed.clone();
            let expected_signal = expected_signal.clone();
            async move {
                let options = input.options.clone();
                *observed.lock().expect("observed lock") =
                    Some(observe_factory_options(&input, Some(&expected_signal)));
                // 工厂可以再次调用 onResponse（流本身在工厂之前已调用一次）。
                if let Some(on_response) = options
                    .as_ref()
                    .and_then(|options| options.on_response.clone())
                {
                    on_response(
                        ProviderResponse {
                            status: 200,
                            headers: Default::default(),
                        },
                        &input.model,
                    );
                }
                faux_assistant_message("ok", FauxAssistantMessageOptions::default())
            }
        }
    })]);

    let on_response = {
        let calls = on_response_calls.clone();
        Arc::new(move |_response: ProviderResponse, _model: &Model| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    };
    let model = faux.get_model(None).unwrap().clone();
    let message = models
        .complete_simple(
            &model,
            &user_context("hi"),
            ModelsSimpleStreamOptions::from(SimpleStreamOptions {
                base: StreamOptions {
                    signal: Some(signal.clone()),
                    on_response: Some(on_response),
                    on_payload: Some(Arc::new(
                        |_payload: Value, _model: &Model| -> Option<Value> { None },
                    )),
                    on_provider_stream_event: Some(Arc::new(|_data: Value, _model: &Model| {})),
                    ..Default::default()
                },
                // `deferred: false` 走普通流路径（TS 的 `if (streamOptions?.deferred)` 为假），
                // 但工厂仍能在 options 里看到它。
                deferred: Some(DeferredRequest::Enabled(false)),
                ..Default::default()
            }),
        )
        .await;
    assert_eq!(message.content, vec![text_block("ok")]);

    let observed = observed
        .lock()
        .expect("observed lock")
        .clone()
        .expect("factory ran");
    assert!(observed.present, "factory sees options");
    assert_eq!(observed.deferred, Some(DeferredRequest::Enabled(false)));
    // 工厂看到的是请求里的同一个取消令牌（同一棵取消树）。
    assert!(observed.signal_present);
    assert!(observed.signal_matches);
    assert!(observed.on_response);
    assert!(observed.on_payload);
    assert!(observed.on_provider_stream_event);
    // 流的一次 + 工厂的一次。
    assert_eq!(
        on_response_calls.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
}

/// TS 在延迟响应的兑换路径上解构掉 `deferred`/`signal`/`onResponse` 再调用工厂，
/// 其余载体（`onPayload` 等）保留。
#[tokio::test]
async fn strips_deferred_signal_and_on_response_for_deferred_redemptions() {
    let faux = faux_provider();
    let models = models_with(&faux);
    let signal = CancellationToken::new();
    let observed: Arc<std::sync::Mutex<Option<ObservedFactoryOptions>>> =
        Arc::new(std::sync::Mutex::new(None));

    faux.set_responses(vec![FauxResponseStep::message_factory({
        let observed = observed.clone();
        move |input| {
            let observed = observed.clone();
            async move {
                *observed.lock().expect("observed lock") =
                    Some(observe_factory_options(&input, None));
                faux_assistant_message("redeemed", FauxAssistantMessageOptions::default())
            }
        }
    })]);

    let model = faux.get_model(None).unwrap().clone();
    let submitted = models
        .complete_simple(
            &model,
            &user_context("hi"),
            ModelsSimpleStreamOptions::from(SimpleStreamOptions {
                base: StreamOptions {
                    signal: Some(signal.clone()),
                    on_response: Some(Arc::new(|_response: ProviderResponse, _model: &Model| {})),
                    on_payload: Some(Arc::new(
                        |_payload: Value, _model: &Model| -> Option<Value> { None },
                    )),
                    ..Default::default()
                },
                deferred: Some(DeferredRequest::Enabled(true)),
                ..Default::default()
            }),
        )
        .await;
    assert_eq!(submitted.stop_reason, StopReason::Deferred);
    let handle = submitted.deferred.clone().expect("deferred handle");
    let message = models
        .fetch_deferred(&model, &handle, Default::default())
        .await;
    assert_eq!(message.content, vec![text_block("redeemed")]);

    let observed = observed
        .lock()
        .expect("observed lock")
        .clone()
        .expect("factory ran");
    assert_eq!(observed.deferred, None);
    assert!(!observed.signal_present);
    assert!(!observed.on_response);
    assert!(
        observed.on_payload,
        "TS keeps onPayload in submission options"
    );
}
