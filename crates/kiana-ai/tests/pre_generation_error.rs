//! `pre-generation-error.test.ts` 的移植说明与共享层等价断言。
//!
//! ## TS 测试的内容
//!
//! 它对 7 个适配器（anthropic / azure-openai-responses / google-generative-ai /
//! mistral-conversations / openai-codex-responses / openai-completions / openai-responses）
//! 直接调用导出的 `streamSimple(model, context, {})`，断言**同步抛出**
//! `No API key for provider: test-provider`（适配器在返回流之前就解析 API key）。
//!
//! ## Rust 侧的等价语义（两个层次）
//!
//! **1. 直接适配器入口（TS 的同步抛错）。** `spec/ai-api-anthropic-openai.md` §1.3/§8.5 要求
//! 适配器模块级入口保留这一语义：Rust 没有异常，等价物是
//! `pub fn stream_simple(...) -> Result<AssistantMessageEventStream, AiError>`（panic-free 的
//! `Err`）在创建流之前返回 `No API key for provider: <provider>`。这些函数的签名与实现属于
//! 各适配器单元（`api-anthropic-codex` / `api-openai` / `api-other`）。**本单元无法在此刻
//! 断言它**：本副本里的适配器文件还是空模块，函数尚不存在；本文件在适配器落地前只能锁定
//! 下面第 2 层。
//!
//! **2. 注册表 / `Models` 路径。** 冻结的 [`ProviderStreams`] 签名同步返回
//! [`AssistantMessageEventStream`]（不能带 `Err`），与 TS 的 `ProviderStreams` 接口相同：
//! TS 的 `Models.streamSimple` 把 `provider.streamSimple(...)` 放在 `lazyStream` 的 async
//! setup 里，因此适配器的同步 throw 在 `Models` 层本来就会变成 setup rejection → `error`
//! 事件。Rust 的 `lazy_api` 用 `load().await` 表达同一路径。本文件锁定这一半：setup 阶段的
//! 任何失败（含逐字的 `No API key for provider: test-provider`）经 [`lazy_stream`] 变成
//! `error` 事件，`errorMessage` **逐字**保留，绝不被共享层改写。
//!
//! [`lazy_stream`]: kiana_ai::api::lazy::lazy_stream
//! [`AssistantMessageEventStream`]: kiana_ai::event_stream::AssistantMessageEventStream

use kiana_ai::api::lazy::{LazyApiCapabilities, lazy_api, lazy_stream};
use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::models::ProviderStreams;
use kiana_ai::types::{
    AssistantMessage, AssistantRole, Context, ErrorReason, Model, SimpleStreamOptions, StopReason,
    TranscriptContext,
};

/// TS 测试里的 `model(api)`。
fn model(api: &str) -> Model {
    Model {
        id: "test-model".into(),
        name: "Test".into(),
        api: api.into(),
        provider: "test-provider".into(),
        base_url: "https://example.invalid".into(),
        reasoning: Some(false),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(1_000),
        max_tokens: Some(100),
        ..Model::default()
    }
}

fn context() -> TranscriptContext {
    TranscriptContext::from_normalized(Context {
        messages: vec![],
        ..Context::default()
    })
}

const MISSING_AUTH: &str = "No API key for provider: test-provider";

#[tokio::test]
async fn missing_auth_text_is_preserved_verbatim_in_the_error_event() {
    // 注册表路径：适配器的同步 `Err`（或 setup 失败）在 `lazy_api` 里变成 error 事件，
    // 文本按原样进入 error 事件（当前稳定契约，改动需同步测试）。
    let stream = lazy_stream(
        &model("anthropic-messages"),
        Box::pin(async move { Err(AiError::new(AiErrorKind::Auth, MISSING_AUTH)) }),
        None,
    );

    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    assert_eq!(events.len(), 1);
    let kiana_ai::types::AssistantMessageEvent::Error { reason, error } = &events[0] else {
        panic!("expected an error event");
    };
    assert_eq!(*reason, ErrorReason::Error);
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(error.role, AssistantRole::Assistant);
    assert_eq!(error.error_message.as_deref(), Some(MISSING_AUTH));

    let result = stream.result().await.expect("result");
    assert_eq!(result.error_message.as_deref(), Some(MISSING_AUTH));
}

#[tokio::test]
async fn lazy_api_reports_missing_auth_for_every_adapter_api() {
    // 7 个适配器都由 `lazyApi(() => import(...))` 包装；加载/鉴权失败在任何 api 名下都进入
    // `error` 事件，文本不变（与 TS 的 `Models.streamSimple` 经由 lazyStream 的行为一致）。
    for api in [
        "anthropic-messages",
        "azure-openai-responses",
        "google-generative-ai",
        "mistral-conversations",
        "openai-codex-responses",
        "openai-completions",
        "openai-responses",
    ] {
        let load: kiana_ai::api::lazy::LazyApiLoad = std::sync::Arc::new(|| {
            Box::pin(async move { Err(AiError::new(AiErrorKind::Auth, MISSING_AUTH)) })
        });
        let implementation: std::sync::Arc<dyn ProviderStreams> =
            lazy_api(load, Some(LazyApiCapabilities::default()));
        let result = implementation
            .stream_simple(&model(api), &context(), SimpleStreamOptions::default())
            .result()
            .await
            .expect("result");
        assert_eq!(
            result.error_message.as_deref(),
            Some(MISSING_AUTH),
            "api {api}"
        );
        assert_eq!(result.api, api);
        assert_eq!(result.provider, "test-provider");
        assert_eq!(result.model, "test-model");
    }
}

/// `AssistantMessage` 的默认 `usage` 与合成错误消息一致（全 0）。
#[test]
fn setup_error_message_usage_is_all_zero() {
    let message = kiana_ai::api::lazy::create_setup_error_message(
        &model("test-api"),
        &AiError::new(AiErrorKind::Auth, MISSING_AUTH),
        42,
    );
    let expected = AssistantMessage {
        role: AssistantRole::Assistant,
        api: "test-api".into(),
        provider: "test-provider".into(),
        model: "test-model".into(),
        usage: Default::default(),
        stop_reason: StopReason::Error,
        error_message: Some(MISSING_AUTH.into()),
        timestamp: 42,
        ..AssistantMessage::default()
    };
    assert_eq!(message, expected);
    assert_eq!(message.usage.total_tokens, 0);
    assert_eq!(message.usage.cost.total, 0.0);
}
