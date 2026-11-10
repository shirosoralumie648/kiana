//! `fetch-option.test.ts` 的 Rust 移植（共享层部分）。
//!
//! TS 用 `vi.stubGlobal("fetch", fallback)` + 自定义 `fetch` 断言「适配器只调用
//! `options.fetch`，绝不碰 ambient `globalThis.fetch`」。Rust 侧没有全局 fetch：
//! [`StreamOptions::fetch`] 是显式的 `Arc<dyn HttpClient>` 句柄，适配器只能拿到它。
//!
//! 本单元负责的是把 `SimpleStreamOptions.base.fetch` 原样带进 `buildBaseOptions` 的结果
//! （共享层）。各适配器是否真的用它发请求属于适配器单元；这里用 [`Arc::ptr_eq`] 锁定
//! 「同一个句柄、没有被替换/丢弃」，并确认未注入时保持 `None`（适配器回落到默认
//! `reqwest` 实现，而不是某个进程级全局）。
//!
//! Google 适配器拒绝自定义 fetch（`Custom fetch is not supported by ...`）是适配器行为，
//! 见 `google-generative-ai` / `google-vertex` 单元。
//!
//! ## 跨单元待办（TS 的 6 条）
//!
//! - `api-anthropic-codex`：Anthropic SDK 注入（1）、Codex SSE 分支的 `fetch` 注入点（1）。
//! - `api-openai`：OpenAI SDK 三适配器 completions/responses/azure 的循环（1）。
//! - `api-other`：Mistral + Codex SSE + kiana-messages（1）、Google 拒绝自定义 fetch（1）、
//!   Google 接受 `globalThis.fetch`（1）。
//! - `api-classifier-images`：图片生成路径（1）。
//!
//! 合并时请核对这些单元是否移植了 `test/fetch-option.test.ts` 的适配器级断言。

use std::sync::Arc;

use kiana_ai::api::simple_options::build_base_options;
use kiana_ai::http::{ByteStream, HttpClient, HttpError, HttpRequest, HttpResponse};
use kiana_ai::types::{
    Context, Model, SimpleStreamOptions, StreamOptions, TranscriptContext, UserContent,
    UserMessage, UserRole,
};

struct RecordingClient;

impl HttpClient for RecordingClient {
    fn execute<'a>(
        &'a self,
        _request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> kiana_ai::http::BoxFuture<'a, Result<HttpResponse, HttpError>> {
        Box::pin(async move {
            Err(HttpError::new(
                "custom fetch must be used by the adapter, not by this test",
            ))
        })
    }
}

// 仅用于确认 `ByteStream` 类型可用（trait 实现的签名约束）。
#[allow(dead_code)]
fn empty_stream() -> ByteStream {
    Box::pin(futures::stream::empty())
}

fn context() -> TranscriptContext {
    TranscriptContext::from_normalized(Context {
        messages: vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    })
}

fn model() -> Model {
    serde_json::from_value(serde_json::json!({
        "id": "test-model",
        "name": "Test Model",
        "api": "anthropic-messages",
        "provider": "test-provider",
        "baseUrl": "https://upstream.test/v1",
        "reasoning": false,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 10000,
        "maxTokens": 1000
    }))
    .expect("model")
}

#[test]
fn passes_fetch_through_build_base_options() {
    let custom: Arc<dyn HttpClient> = Arc::new(RecordingClient);
    let options = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("test-key".into()),
            fetch: Some(custom.clone()),
            max_retries: Some(0),
            ..StreamOptions::default()
        },
        ..SimpleStreamOptions::default()
    };

    let base = build_base_options(&model(), &context(), Some(&options), None);
    let forwarded = base.fetch.expect("fetch forwarded");
    assert!(
        Arc::ptr_eq(&custom, &forwarded),
        "buildBaseOptions must forward the exact fetch handle (TS passes the same function)"
    );
    // `maxRetries: 0` 必须保留（`??` 语义，不是 `||`）。
    assert_eq!(base.max_retries, Some(0));
}

#[test]
fn leaves_fetch_unset_when_not_injected() {
    let base = build_base_options(&model(), &context(), None, None);
    assert!(
        base.fetch.is_none(),
        "no process-wide fetch fallback: adapters use their default HttpClient"
    );
}

#[test]
fn fetch_flows_through_simple_stream_options_base() {
    let custom: Arc<dyn HttpClient> = Arc::new(RecordingClient);
    let mut options = SimpleStreamOptions::default();
    options.base.fetch = Some(custom.clone());
    let base = build_base_options(&model(), &context(), Some(&options), Some("key"));
    assert!(Arc::ptr_eq(&custom, base.fetch.as_ref().expect("fetch")));
    assert_eq!(base.api_key.as_deref(), Some("key"));
}
