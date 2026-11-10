//! `compat-env.test.ts` 的 Rust 移植（离线）。
//!
//! 验证旧全局分派（`compat.complete`）对**非内置 provider** 走 api-registry：注册一个
//! `openai-responses` 的覆盖实现，确认请求里的 `apiKey` 原样传给 stream 函数。
//!
//! 与原测试的差异：TS 的 `afterEach(resetApiProviders)` 在 Rust 里同样在测试末尾执行
//! （`reset_api_providers()`）；注册表是进程全局状态，因此本文件只有一个测试，避免并行
//! 测试互相覆盖。

mod common;

use std::sync::{Arc, Mutex};

use kiana_ai::compat::{ApiProvider, complete, register_api_provider, reset_api_providers};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, Context,
    DoneReason, Model, ModelCost, ModelInputModality, StopReason, TextContent, TextKind,
    TranscriptContext, Usage, UsageCost, UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::event_stream::AssistantMessageEventStream;

fn model() -> Model {
    Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: "openai-responses".into(),
        provider: "custom-openai".into(),
        base_url: "https://example.test/v1".into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(128_000),
        max_tokens: Some(4096),
        ..Model::default()
    }
}

fn message(model: &Model) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "ok".into(),
            text_signature: None,
        })],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: Usage {
            cost: UsageCost::default(),
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        timestamp: common::event_stream::now_ms(),
        ..AssistantMessage::default()
    }
}

/// TS 里自定义 `stream` 的行为：推 `start`、`done`，再 `end(output)`。
fn ok_stream(model: &Model) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let output = message(model);
    let _ = stream.push(AssistantMessageEvent::Start {
        partial: Box::new(output.clone()),
    });
    let _ = stream.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: Box::new(output.clone()),
    });
    stream.end(output);
    stream
}

fn context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hi".into()),
            timestamp: 0,
        })],
        tools: None,
    }
}

#[tokio::test]
async fn dispatches_unknown_providers_through_the_legacy_api_registry() {
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let capture_stream = Arc::clone(&captured);
    let capture_simple = Arc::clone(&captured);
    let stream_model = model();
    let simple_model = model();
    let provider = ApiProvider {
        api: "openai-responses".into(),
        stream: Arc::new(
            move |_model: &Model, _context: &TranscriptContext, options| {
                *capture_stream
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = options.api_key.clone();
                ok_stream(&stream_model)
            },
        ),
        stream_simple: Arc::new(
            move |_model: &Model, _context: &TranscriptContext, options| {
                *capture_simple
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = options.base.api_key.clone();
                ok_stream(&simple_model)
            },
        ),
    };
    register_api_provider(provider, None);

    let options = kiana_ai::types::StreamOptions {
        api_key: Some("request-key".into()),
        ..kiana_ai::types::StreamOptions::default()
    };
    let response = complete(&model(), &context(), options).await;

    assert_eq!(
        captured
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_deref(),
        Some("request-key")
    );
    assert_eq!(response.stop_reason, StopReason::Stop);

    reset_api_providers();
}
