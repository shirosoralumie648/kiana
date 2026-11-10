//! providers/cloudflare-stream.ts：Cloudflare endpoint 占位符的解析包装。
//!
//! TS: `packages/ai/src/providers/cloudflare-stream.ts`
//!
//! # 为什么需要这个包装
//!
//! Cloudflare 的目录模型 base URL 是模板，例如
//! `https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/...`。
//! 占位符的真实值来自鉴权阶段返回的 provider env（见
//! [`crate::providers::cloudflare_auth`]），而通用 API 适配器不认识它们。本模块在把请求
//! 交给内层实现之前，用 env 做一次字面量字符串替换。
//!
//! # 规则与边界
//!
//! - `env` 为 `None`：原样返回模型，不做任何替换；
//! - env 里缺某个键：该占位符保持原样（不会被替换），不会 panic，也不会回退到进程
//!   环境——env 已在鉴权阶段合并完成；
//! - 替换是 `str::replace`（所有出现都替换、按字面量匹配），不是 URL 解析，因此不会
//!   做百分号编码或规范化；
//! - 只有 `base_url` 会变；模型其它字段原样复制。
//!
//! # 能力边界（为什么只实现三个方法）
//!
//! [`CloudflareStreams`] 只转发 `stream`/`stream_simple`，[`CloudflareClassifier`] 只转发
//! `classify`。TS 的包装对象也只展开这两个方法，因此 Rust 侧同样不转发
//! `fetch_deferred`/`cancel_deferred`：`ProviderStreams::has_fetch_deferred` 等谓词保持
//! trait 默认值 `false`，`create_provider` 在构造时快照为「不支持延迟响应」。
//!
//! # 调用方
//!
//! [`crate::providers::cloudflare_ai_gateway`] 与
//! [`crate::providers::cloudflare_workers_ai`] 的 provider 工厂。

use std::sync::Arc;

use async_trait::async_trait;

use crate::event_stream::AssistantMessageEventStream;
use crate::models::{ClassifierOptions, ProviderClassifier, ProviderStreams};
use crate::types::{
    ClassifierContext, ClassifierResult, Model, ProviderEnv, SimpleStreamOptions, StreamOptions,
    TranscriptContext,
};

const CLOUDFLARE_ACCOUNT_ID: &str = "CLOUDFLARE_ACCOUNT_ID";
const CLOUDFLARE_GATEWAY_ID: &str = "CLOUDFLARE_GATEWAY_ID";

/// `resolveCloudflareModel(model, env)`：把 `{CLOUDFLARE_ACCOUNT_ID}` /
/// `{CLOUDFLARE_GATEWAY_ID}` 替换为解析后的 env 值。
///
/// 返回新的 [`Model`]（`base_url` 可能未变）；`env` 为 `None` 或键缺失时保留对应占位符。
/// 只修改 `base_url`，`id`/`provider`/`api` 等字段不变。
pub fn resolve_cloudflare_model(model: &Model, env: Option<&ProviderEnv>) -> Model {
    let Some(env) = env else {
        return model.clone();
    };
    let account_placeholder = format!("{{{CLOUDFLARE_ACCOUNT_ID}}}");
    let gateway_placeholder = format!("{{{CLOUDFLARE_GATEWAY_ID}}}");
    let account = env
        .get(CLOUDFLARE_ACCOUNT_ID)
        .map_or(account_placeholder.as_str(), String::as_str);
    let gateway = env
        .get(CLOUDFLARE_GATEWAY_ID)
        .map_or(gateway_placeholder.as_str(), String::as_str);
    let base_url = model
        .base_url
        .replace(&account_placeholder, account)
        .replace(&gateway_placeholder, gateway);
    if base_url == model.base_url {
        return model.clone();
    }
    let mut resolved = model.clone();
    resolved.base_url = base_url;
    resolved
}

/// Wrap an API implementation so Cloudflare account/gateway endpoint
/// placeholders materialize from the resolved provider env before dispatch.
///
/// TS 返回的对象只有 `stream`/`streamSimple`（没有展开被包装实现），因此不转发
/// `fetchDeferred`/`cancelDeferred`。
///
/// `stream` 从 `options.env` 取 env，`stream_simple` 从 `options.base.env` 取；两者都在
/// dispatch 前调用 [`resolve_cloudflare_model`]，内层实现拿到的已经是替换后的 `Model`。
pub struct CloudflareStreams {
    inner: Arc<dyn ProviderStreams>,
}

impl CloudflareStreams {
    /// 包装一个实现。
    ///
    /// 构造不调用内层；内层通常是 `openai-completions` / `anthropic-messages` /
    /// `openai-responses` 适配器。
    pub fn new(inner: Arc<dyn ProviderStreams>) -> Self {
        CloudflareStreams { inner }
    }
}

#[async_trait]
impl ProviderStreams for CloudflareStreams {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        // 解析发生在同步调用路径上（纯字符串替换，没有 I/O），因此不需要 lazy 包装。
        let resolved = resolve_cloudflare_model(model, options.env.as_ref());
        self.inner.stream(&resolved, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        // SimpleStreamOptions 把通用选项放在 `base` 里，env 也从那里取。
        let resolved = resolve_cloudflare_model(model, options.base.env.as_ref());
        self.inner.stream_simple(&resolved, context, options)
    }
}

/// `cloudflareStreams(streams)`。
///
/// 把任意 [`ProviderStreams`] 包成占位符感知版本；每次调用新建包装器，内层 `Arc` 共享。
pub fn cloudflare_streams(streams: Arc<dyn ProviderStreams>) -> Arc<dyn ProviderStreams> {
    Arc::new(CloudflareStreams::new(streams))
}

/// Classifier counterpart of [`cloudflare_streams`].
///
/// 分类请求的 env 在 `ClassifierOptions`（即 `StreamOptions`）里；`options` 为 `None` 时
/// 无法解析占位符，模型原样传给内层。
pub struct CloudflareClassifier {
    inner: Arc<dyn ProviderClassifier>,
}

impl CloudflareClassifier {
    /// 包装一个实现。
    pub fn new(inner: Arc<dyn ProviderClassifier>) -> Self {
        CloudflareClassifier { inner }
    }
}

#[async_trait]
impl ProviderClassifier for CloudflareClassifier {
    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        // 与 stream 相同：先从选项里取 env 做替换，再把结果交给内层分类器。
        let env = options.and_then(|options| options.env.as_ref());
        let resolved = resolve_cloudflare_model(model, env);
        self.inner.classify(&resolved, context, options).await
    }
}

/// `cloudflareClassifier(classifier)`。
///
/// 分类器的占位符包装；每次调用新建包装器。
pub fn cloudflare_classifier(
    classifier: Arc<dyn ProviderClassifier>,
) -> Arc<dyn ProviderClassifier> {
    Arc::new(CloudflareClassifier::new(classifier))
}
