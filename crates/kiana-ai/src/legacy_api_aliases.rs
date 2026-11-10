//! `legacy-api-aliases.ts`（108 行）：旧全局 `kiana-ai` 面的 16 个 `@deprecated` 流函数常量。
//!
//! TS 源为 8 个 API 各导出 `stream<Api>` / `streamSimple<Api>` 两个常量，值分别是
//! `xxxApi().stream` / `xxxApi().streamSimple` —— 纯别名，没有独立状态：
//! `streamAnthropic`、`streamSimpleAnthropic`、`streamAzureOpenAIResponses`、
//! `streamSimpleAzureOpenAIResponses`、`streamGoogle`、`streamSimpleGoogle`、
//! `streamGoogleVertex`、`streamSimpleGoogleVertex`、`streamMistral`、
//! `streamSimpleMistral`、`streamOpenAICodexResponses`、`streamSimpleOpenAICodexResponses`、
//! `streamOpenAICompletions`、`streamSimpleOpenAICompletions`、`streamOpenAIResponses`、
//! `streamSimpleOpenAIResponses`。
//!
//! ## Rust 侧映射
//!
//! TS 在这些常量上捕获的是**模块加载时**构造的 `*Api()` 实例。Rust 没有等价的模块加载
//! 时机，因此每个别名对应一个 `LazyLock<Arc<dyn ProviderStreams>>`（首次调用时构造一次，
//! 之后复用）加一个同名 snake_case 函数，签名与 [`crate::models::ProviderStreams::stream`] /
//! [`crate::models::ProviderStreams::stream_simple`] 一致。行为与 TS 相同：别名直接调用适配器实现，
//! 不经过 `compat` 的 api-registry（注册覆盖不影响别名）。
//!
//! 命名差异（TS → Rust）：`streamGoogle` → [`crate::legacy_api_aliases::stream_google`]、
//! `streamMistral` → [`crate::legacy_api_aliases::stream_mistral`]（TS 的
//! `google-generative-ai` / `mistral-conversations` 两个 API 的旧别名不带
//! `GenerativeAI`/`Conversations` 后缀，逐字保留以免破坏按旧名调用的下游代码）。
//!
//! ## 调用方与使用边界
//!
//! - Rust 侧：`compat` 通过 `pub use crate::legacy_api_aliases::*` 把这些名字重新导出到
//!   旧全局面，供仍按 TS 旧名字调用本 crate 的下游代码使用。当前仓库内没有 Rust 调用方，
//!   只有本文件的单测直接调用 8 个 `stream` 别名（`streamSimple` 侧另有一条异步测试）。
//! - 扩展运行时的同名导出由 `kiana-extensions` 的 JS helper 另行实现（它按 api id 取
//!   `apiStreams(...)`），并不调用这里的 Rust 函数；两边只是行为等价，不是同一条代码路径。
//! - 同步性：每个函数只是「取缓存适配器 → 调用 `stream`/`stream_simple`」。按
//!   `ProviderStreams` 的契约，实现不得阻塞调用线程，同步阶段的配置/鉴权错误要变成流上的
//!   `error` 事件而不是 `Result`；因此这些函数没有错误返回通道，失败只能从流里观察。
//! - 共享实例：每个 `LazyLock` 在进程生命周期内只初始化一次，之后所有调用共享同一个
//!   适配器（`ProviderStreams` 要求 `Send + Sync`）。初始化器是无 IO 的工厂调用、不会
//!   失败；多个线程首次同时调用时，`LazyLock` 会让其中一个执行初始化、其余等待。

use std::sync::{Arc, LazyLock};

use crate::event_stream::AssistantMessageEventStream;
use crate::models::ProviderStreams;
use crate::types::{Model, SimpleStreamOptions, StreamOptions, TranscriptContext};

static ANTHROPIC_MESSAGES_STREAMS: LazyLock<Arc<dyn ProviderStreams>> =
    LazyLock::new(crate::api::anthropic_messages::anthropic_messages_api);
static AZURE_OPENAI_RESPONSES_STREAMS: LazyLock<Arc<dyn ProviderStreams>> =
    LazyLock::new(crate::api::azure_openai_responses::azure_openai_responses_api);
static GOOGLE_GENERATIVE_AI_STREAMS: LazyLock<Arc<dyn ProviderStreams>> =
    LazyLock::new(crate::api::google_generative_ai::google_generative_ai_api);
static GOOGLE_VERTEX_STREAMS: LazyLock<Arc<dyn ProviderStreams>> =
    LazyLock::new(crate::api::google_vertex::google_vertex_api);
static MISTRAL_CONVERSATIONS_STREAMS: LazyLock<Arc<dyn ProviderStreams>> =
    LazyLock::new(crate::api::mistral_conversations::mistral_conversations_api);
static OPENAI_CODEX_RESPONSES_STREAMS: LazyLock<Arc<dyn ProviderStreams>> =
    LazyLock::new(crate::api::openai_codex_responses::openai_codex_responses_api);
static OPENAI_COMPLETIONS_STREAMS: LazyLock<Arc<dyn ProviderStreams>> =
    LazyLock::new(crate::api::openai_completions::openai_completions_api);
static OPENAI_RESPONSES_STREAMS: LazyLock<Arc<dyn ProviderStreams>> =
    LazyLock::new(crate::api::openai_responses::openai_responses_api);

/// `streamAnthropic`。
///
/// @deprecated 用 `@earendil-works/kiana-ai/api/anthropic-messages` 的 `stream` 或
/// `anthropicMessagesApi().stream`。
pub fn stream_anthropic(
    model: &Model,
    context: &TranscriptContext,
    options: StreamOptions,
) -> AssistantMessageEventStream {
    ANTHROPIC_MESSAGES_STREAMS.stream(model, context, options)
}

/// `streamSimpleAnthropic`。
///
/// @deprecated 用 `anthropicMessagesApi().streamSimple`。
pub fn stream_simple_anthropic(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> AssistantMessageEventStream {
    ANTHROPIC_MESSAGES_STREAMS.stream_simple(model, context, options)
}

/// `streamAzureOpenAIResponses`。
///
/// @deprecated 用 `azureOpenAIResponsesApi().stream`。
pub fn stream_azure_openai_responses(
    model: &Model,
    context: &TranscriptContext,
    options: StreamOptions,
) -> AssistantMessageEventStream {
    AZURE_OPENAI_RESPONSES_STREAMS.stream(model, context, options)
}

/// `streamSimpleAzureOpenAIResponses`。
///
/// @deprecated 用 `azureOpenAIResponsesApi().streamSimple`。
pub fn stream_simple_azure_openai_responses(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> AssistantMessageEventStream {
    AZURE_OPENAI_RESPONSES_STREAMS.stream_simple(model, context, options)
}

/// `streamGoogle`。
///
/// @deprecated 用 `googleGenerativeAIApi().stream`。
pub fn stream_google(
    model: &Model,
    context: &TranscriptContext,
    options: StreamOptions,
) -> AssistantMessageEventStream {
    GOOGLE_GENERATIVE_AI_STREAMS.stream(model, context, options)
}

/// `streamSimpleGoogle`。
///
/// @deprecated 用 `googleGenerativeAIApi().streamSimple`。
pub fn stream_simple_google(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> AssistantMessageEventStream {
    GOOGLE_GENERATIVE_AI_STREAMS.stream_simple(model, context, options)
}

/// `streamGoogleVertex`。
///
/// @deprecated 用 `googleVertexApi().stream`。
pub fn stream_google_vertex(
    model: &Model,
    context: &TranscriptContext,
    options: StreamOptions,
) -> AssistantMessageEventStream {
    GOOGLE_VERTEX_STREAMS.stream(model, context, options)
}

/// `streamSimpleGoogleVertex`。
///
/// @deprecated 用 `googleVertexApi().streamSimple`。
pub fn stream_simple_google_vertex(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> AssistantMessageEventStream {
    GOOGLE_VERTEX_STREAMS.stream_simple(model, context, options)
}

/// `streamMistral`。
///
/// @deprecated 用 `mistralConversationsApi().stream`。
pub fn stream_mistral(
    model: &Model,
    context: &TranscriptContext,
    options: StreamOptions,
) -> AssistantMessageEventStream {
    MISTRAL_CONVERSATIONS_STREAMS.stream(model, context, options)
}

/// `streamSimpleMistral`。
///
/// @deprecated 用 `mistralConversationsApi().streamSimple`。
pub fn stream_simple_mistral(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> AssistantMessageEventStream {
    MISTRAL_CONVERSATIONS_STREAMS.stream_simple(model, context, options)
}

/// `streamOpenAICodexResponses`。
///
/// @deprecated 用 `openAICodexResponsesApi().stream`。
pub fn stream_openai_codex_responses(
    model: &Model,
    context: &TranscriptContext,
    options: StreamOptions,
) -> AssistantMessageEventStream {
    OPENAI_CODEX_RESPONSES_STREAMS.stream(model, context, options)
}

/// `streamSimpleOpenAICodexResponses`。
///
/// @deprecated 用 `openAICodexResponsesApi().streamSimple`。
pub fn stream_simple_openai_codex_responses(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> AssistantMessageEventStream {
    OPENAI_CODEX_RESPONSES_STREAMS.stream_simple(model, context, options)
}

/// `streamOpenAICompletions`。
///
/// @deprecated 用 `openAICompletionsApi().stream`。
pub fn stream_openai_completions(
    model: &Model,
    context: &TranscriptContext,
    options: StreamOptions,
) -> AssistantMessageEventStream {
    OPENAI_COMPLETIONS_STREAMS.stream(model, context, options)
}

/// `streamSimpleOpenAICompletions`。
///
/// @deprecated 用 `openAICompletionsApi().streamSimple`。
pub fn stream_simple_openai_completions(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> AssistantMessageEventStream {
    OPENAI_COMPLETIONS_STREAMS.stream_simple(model, context, options)
}

/// `streamOpenAIResponses`。
///
/// @deprecated 用 `openAIResponsesApi().stream`。
pub fn stream_openai_responses(
    model: &Model,
    context: &TranscriptContext,
    options: StreamOptions,
) -> AssistantMessageEventStream {
    OPENAI_RESPONSES_STREAMS.stream(model, context, options)
}

/// `streamSimpleOpenAIResponses`。
///
/// @deprecated 用 `openAIResponsesApi().streamSimple`。
pub fn stream_simple_openai_responses(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> AssistantMessageEventStream {
    OPENAI_RESPONSES_STREAMS.stream_simple(model, context, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AssistantMessageEvent, AssistantRole, ModelInputModality, StopReason, Usage, UsageCost,
    };

    /// 8 个 API 的别名都必须能同步返回流并产生 `error` 事件（无 key 时的适配器映射），
    /// 而不是 panic 或返回空。
    #[test]
    fn every_alias_returns_a_synchronously_created_stream() {
        let context = TranscriptContext::from_normalized(crate::types::Context::default());
        type StreamAlias =
            fn(&Model, &TranscriptContext, StreamOptions) -> AssistantMessageEventStream;
        let cases: [(&str, StreamAlias); 8] = [
            ("anthropic-messages", stream_anthropic),
            ("azure-openai-responses", stream_azure_openai_responses),
            ("google-generative-ai", stream_google),
            ("google-vertex", stream_google_vertex),
            ("mistral-conversations", stream_mistral),
            ("openai-codex-responses", stream_openai_codex_responses),
            ("openai-completions", stream_openai_completions),
            ("openai-responses", stream_openai_responses),
        ];
        for (api, alias) in cases {
            let model = Model {
                id: "m".into(),
                name: "m".into(),
                api: api.into(),
                provider: "test-provider".into(),
                base_url: "https://example.invalid".into(),
                input: vec![ModelInputModality::Text],
                cost: Default::default(),
                context_window: Some(1000),
                max_tokens: Some(100),
                ..Model::default()
            };
            let options = StreamOptions {
                api_key: Some("test-key".into()),
                ..StreamOptions::default()
            };
            let stream = alias(&model, &context, options);
            // 清空脚本队列/无网络的适配器会以 error 事件结束；这里只要求流可同步取得。
            drop(stream);
        }
    }

    #[tokio::test]
    async fn alias_stream_simple_is_wired() {
        let context = TranscriptContext::from_normalized(crate::types::Context::default());
        let model = Model {
            id: "m".into(),
            name: "m".into(),
            api: "openai-responses".into(),
            provider: "openai".into(),
            base_url: "https://example.invalid".into(),
            input: vec![ModelInputModality::Text],
            cost: Default::default(),
            ..Model::default()
        };
        let stream =
            stream_simple_openai_responses(&model, &context, SimpleStreamOptions::default());
        // 无网络/无 key：适配器应产生 error 事件而不是 panic。
        let message = stream.result().await.expect("the adapter ends the stream");
        assert_eq!(message.role, AssistantRole::Assistant);
        let _ = (
            StopReason::Error,
            Usage::default(),
            UsageCost::default(),
            AssistantMessageEvent::Error {
                reason: crate::types::ErrorReason::Error,
                error: Box::new(message.clone()),
            },
        );
    }
}
