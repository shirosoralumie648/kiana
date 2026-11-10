//! `packages/ai/test/cloudflare-stream.test.ts` 的 Rust 移植。
//!
//! TS 被测对象：`providers/cloudflare-stream.ts` 的 `cloudflareStreams`。它在把请求交给
//! 底层 api 实现之前，用解析后的 provider env 展开 `baseUrl` 里的
//! `{CLOUDFLARE_ACCOUNT_ID}` / `{CLOUDFLARE_GATEWAY_ID}` 占位符；env 缺失时保留占位符。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::ProviderStreams;
use kiana_ai::providers::cloudflare_stream::cloudflare_streams;
use kiana_ai::types::{Model, ProviderEnv, SimpleStreamOptions, StreamOptions, TranscriptContext};
use kiana_ai::utils::transcript::normalize_context;

const BASE_URL: &str =
    "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/openai";

fn model() -> Model {
    Model {
        id: "model".into(),
        name: "model".into(),
        api: "openai-completions".into(),
        provider: "cloudflare-ai-gateway".into(),
        base_url: BASE_URL.into(),
        reasoning: Some(false),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(1000),
        max_tokens: Some(100),
        ..Default::default()
    }
}

fn env(pairs: &[(&str, &str)]) -> ProviderEnv {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// 记录收到的 `baseUrl` 的底层实现（TS 的 `stream`/`streamSimple` 闭包）。
#[derive(Clone, Default)]
struct CaptureStreams {
    captured: Arc<Mutex<Vec<String>>>,
}

impl CaptureStreams {
    fn urls(&self) -> Vec<String> {
        self.captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[async_trait]
impl ProviderStreams for CaptureStreams {
    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        self.captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(model.base_url.clone());
        AssistantMessageEventStream::new()
    }

    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(model.base_url.clone());
        AssistantMessageEventStream::new()
    }
}

#[test]
fn materializes_the_model_endpoint_before_dispatch() {
    let capture = CaptureStreams::default();
    let streams = cloudflare_streams(Arc::new(capture.clone()));
    let context = normalize_context(&kiana_ai::types::Context::default());
    let resolved_env = env(&[
        ("CLOUDFLARE_ACCOUNT_ID", "account"),
        ("CLOUDFLARE_GATEWAY_ID", "gateway"),
    ]);

    let options = StreamOptions {
        env: Some(resolved_env.clone()),
        ..Default::default()
    };
    streams.stream(&model(), &context, options);
    streams.stream_simple(
        &model(),
        &context,
        SimpleStreamOptions {
            base: StreamOptions {
                env: Some(resolved_env),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    assert_eq!(
        capture.urls(),
        vec![
            "https://gateway.ai.cloudflare.com/v1/account/gateway/openai".to_string(),
            "https://gateway.ai.cloudflare.com/v1/account/gateway/openai".to_string(),
        ]
    );
}

#[test]
fn keeps_placeholders_when_the_provider_env_does_not_resolve_them() {
    let capture = CaptureStreams::default();
    let streams = cloudflare_streams(Arc::new(capture.clone()));
    let context = normalize_context(&kiana_ai::types::Context::default());

    streams.stream_simple(&model(), &context, SimpleStreamOptions::default());

    assert_eq!(capture.urls(), vec![BASE_URL.to_string()]);
}
