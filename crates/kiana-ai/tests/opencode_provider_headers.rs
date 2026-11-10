//! `packages/ai/test/opencode-provider-headers.test.ts` 的 Rust 移植。
//!
//! TS 用普通对象实现 `ProviderStreams` 并记录 options；Rust 用 [`RecordingStreams`]
//! 的 `Mutex` 槽位捕获请求头。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_ai::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use kiana_ai::models::ProviderStreams;
use kiana_ai::providers::faux::faux_assistant_message;
use kiana_ai::providers::opencode_headers::with_open_code_session_header;
use kiana_ai::types::{
    AssistantMessageEvent, Context, DeferredCancelOptions, DeferredFetchOptions, DeferredHandle,
    DoneReason, Model, ModelCost, ModelInputModality, ProviderHeaders, SimpleStreamOptions,
    StreamOptions, TranscriptContext,
};
use kiana_ai::utils::transcript::normalize_context;

fn model() -> Model {
    Model {
        id: "test-model".into(),
        name: "Test model".into(),
        api: "test-api".into(),
        provider: "opencode".into(),
        base_url: "https://opencode.ai/zen/v1".into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(1000),
        max_tokens: Some(100),
        ..Default::default()
    }
}

fn context() -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![kiana_ai::types::Message::User(
            kiana_ai::types::UserMessage {
                role: kiana_ai::types::UserRole::User,
                content: kiana_ai::types::UserContent::Text("hi".into()),
                timestamp: 0,
            },
        )],
        tools: None,
    })
}

fn completed_stream() -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let message = faux_assistant_message(
        "ok",
        kiana_ai::providers::faux::FauxAssistantMessageOptions::default(),
    );
    let _ = stream.push(AssistantMessageEvent::Start {
        partial: Box::new(message.clone()),
    });
    let _ = stream.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: Box::new(message.clone()),
    });
    stream.end(message);
    stream
}

#[derive(Default)]
struct RecordingStreams {
    stream_headers: Mutex<Option<Option<ProviderHeaders>>>,
    simple_headers: Mutex<Option<Option<ProviderHeaders>>>,
}

#[async_trait]
impl ProviderStreams for RecordingStreams {
    fn stream(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        *self.stream_headers.lock().unwrap() = Some(options.headers.clone());
        completed_stream()
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        *self.simple_headers.lock().unwrap() = Some(options.base.headers.clone());
        completed_stream()
    }

    fn has_fetch_deferred(&self) -> bool {
        false
    }

    fn fetch_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        None
    }

    fn has_cancel_deferred(&self) -> bool {
        false
    }

    async fn cancel_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: DeferredCancelOptions,
    ) -> Option<Result<(), kiana_ai::error::AiError>> {
        None
    }
}

fn headers(pairs: &[(&str, Option<&str>)]) -> ProviderHeaders {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), value.map(str::to_string)))
        .collect()
}

#[test]
fn maps_session_id_for_stream_requests_even_without_cache_retention() {
    let recording = Arc::new(RecordingStreams::default());
    let streams: Arc<dyn ProviderStreams> = recording.clone();
    let wrapped = with_open_code_session_header(streams);
    let _ = wrapped.stream(
        &model(),
        &context(),
        StreamOptions {
            session_id: Some("conversation-1".into()),
            cache_retention: Some(kiana_ai::types::CacheRetention::None),
            ..Default::default()
        },
    );
    assert_eq!(
        recording.stream_headers.lock().unwrap().clone(),
        Some(Some(headers(&[(
            "x-opencode-session",
            Some("conversation-1")
        )])))
    );
}

#[test]
fn maps_session_id_for_stream_simple_requests_even_without_cache_retention() {
    let recording = Arc::new(RecordingStreams::default());
    let streams: Arc<dyn ProviderStreams> = recording.clone();
    let wrapped = with_open_code_session_header(streams);
    let _ = wrapped.stream_simple(
        &model(),
        &context(),
        SimpleStreamOptions {
            base: StreamOptions {
                session_id: Some("conversation-1".into()),
                cache_retention: Some(kiana_ai::types::CacheRetention::None),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert_eq!(
        recording.simple_headers.lock().unwrap().clone(),
        Some(Some(headers(&[(
            "x-opencode-session",
            Some("conversation-1")
        )])))
    );
}

#[test]
fn preserves_a_case_insensitive_caller_override() {
    for (caller_headers, expected) in [
        (
            headers(&[("X-OpenCode-Session", Some("caller-value"))]),
            headers(&[("X-OpenCode-Session", Some("caller-value"))]),
        ),
        (
            headers(&[("X-OpenCode-Session", None)]),
            headers(&[("X-OpenCode-Session", None)]),
        ),
    ] {
        let recording = Arc::new(RecordingStreams::default());
        let streams: Arc<dyn ProviderStreams> = recording.clone();
        let wrapped = with_open_code_session_header(streams);
        let _ = wrapped.stream_simple(
            &model(),
            &context(),
            SimpleStreamOptions {
                base: StreamOptions {
                    session_id: Some("generated-value".into()),
                    headers: Some(caller_headers),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert_eq!(
            recording.simple_headers.lock().unwrap().clone(),
            Some(Some(expected))
        );
    }
}

#[test]
fn does_not_fabricate_a_session_header_when_session_id_is_absent() {
    let recording = Arc::new(RecordingStreams::default());
    let streams: Arc<dyn ProviderStreams> = recording.clone();
    let wrapped = with_open_code_session_header(streams);
    let _ = wrapped.stream_simple(
        &model(),
        &context(),
        SimpleStreamOptions {
            base: StreamOptions {
                headers: Some(headers(&[("x-custom", Some("value"))])),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert_eq!(
        recording.simple_headers.lock().unwrap().clone(),
        Some(Some(headers(&[("x-custom", Some("value"))])))
    );
}
