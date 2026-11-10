//! api/cloudflare.ts / api/cloudflare-ai-binding.ts。
//!
//! 前半：Cloudflare 的五个 base URL 常量（`src/providers/cloudflare-stream.ts` 的
//! `resolveCloudflareModel()` 在派发前替换 `{CLOUDFLARE_ACCOUNT_ID}` / `{CLOUDFLARE_GATEWAY_ID}`）。
//!
//! 后半：AI Gateway 走 Workers AI binding（`env.AI.fetch()`）的传输层。binding 的
//! `fetch` 直通 `https://workers-binding.ai/ai-gateway/gateways/{gateway}/{provider}/...`，
//! 请求原样透传（方法、headers、query、body 流都不改写），API 实现的行为在两种传输上一致。
//!
//! Rust 侧：TS 的 `FetchFunction` 就是 [`HttpClient`]（`ARCHITECTURE.md` §4.9 冻结的契约），
//! 因此 [`create_ai_binding_fetch`] 返回一个把请求原样交给 binding 的 [`HttpClient`]。
//!
//! TS: `packages/ai/src/api/cloudflare.ts`、`cloudflare-ai-binding.ts`。
//!
//! # 在系统中的位置
//!
//! - 地址常量：Cloudflare 的模型 `baseUrl` 是带 `{CLOUDFLARE_ACCOUNT_ID}` /
//!   `{CLOUDFLARE_GATEWAY_ID}` 占位符的模板；派发前
//!   `providers::cloudflare_stream::resolve_cloudflare_model` 会在 provider env 提供
//!   对应值时就地替换。模型目录生成器（`model_catalog::generator`）内另有一份同值常量。
//! - binding 传输：[`create_ai_binding_fetch`] 返回的 [`HttpClient`] 通过
//!   `StreamOptions.fetch` 注入给**接受自定义 fetch** 的 API 适配器，适配器本身不知道
//!   底层是 binding。并非所有适配器都接受：Google Generative AI
//!   （`api/google_generative_ai.rs:178` 的 `run_stream` 对任何 `Some(fetch)` 都返回
//!   `"Custom fetch is not supported by the Google Generative AI adapter"`）与 Google
//!   Vertex（`api/google_vertex.rs:176` 的 `run_stream` 返回 `"Custom fetch is not
//!   supported by the Google Vertex adapter"`）会直接拒绝，因此 binding 传输不能用于
//!   这两个适配器。
//!
//! 当前 crate 内没有代码直接引用本文件导出的地址常量；它们是与 TS 对齐的公开契约，
//! 改动这里的值不会自动改变模型目录里的 `baseUrl`。
//!
//! # 一次 binding 请求的执行流程
//!
//! 1. 调用方用宿主提供的 `env.AI.fetch` 构造 [`AiBinding`]；
//! 2. [`create_ai_binding_fetch`] 在**构造时**检查 `fetch` 是否存在，返回
//!    `Arc<dyn HttpClient>`；
//! 3. 适配器拼好方法、URL、头、体与超时后调用 [`HttpClient::execute`]；
//! 4. `AiBindingHttpClient` 把 [`HttpRequest`] 与取消令牌原样转给 binding，再把
//!    binding 的响应（含流式 body）原样返回。
//!
//! # 重要约束
//!
//! - 请求**不**改写、不缓冲、不重编码：方法、headers、query 与 body 流与适配器交给
//!   [`HttpClient`] 时完全一致。
//! - binding 调用已经由宿主鉴权，但适配器在派发前仍要求「有 API key 或受认可的
//!   鉴权头」，因此要额外传 [`CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL`] 占位；它必须
//!   与 `Authorization: null` / `x-api-key: null` 配对，否则 SDK 生成的占位鉴权头会被
//!   网关当成 BYOK key（`tests/cloudflare_ai_binding.rs` 端到端锁定）。
//! - 取消：本文件只把 `CancellationToken` 克隆后交给 binding；是否以及多快响应取消，
//!   由宿主的 binding 实现决定。

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::error::{AiError, AiErrorKind};
use crate::http::{BoxFuture, HttpClient, HttpError, HttpRequest, HttpResponse};

/// Workers AI direct endpoint.
pub const CLOUDFLARE_WORKERS_AI_BASE_URL: &str =
    "https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai/v1";

/// Workers AI REST endpoint root; model runs are posted to `run`.
pub const CLOUDFLARE_WORKERS_AI_REST_BASE_URL: &str =
    "https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai";

/// AI Gateway Unified API. <https://developers.cloudflare.com/ai-gateway/usage/unified-api/>
pub const CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL: &str =
    "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/compat";

/// AI Gateway → OpenAI passthrough. Used until /compat supports /v1/responses.
pub const CLOUDFLARE_AI_GATEWAY_OPENAI_BASE_URL: &str =
    "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/openai";

/// AI Gateway → Anthropic passthrough.
pub const CLOUDFLARE_AI_GATEWAY_ANTHROPIC_BASE_URL: &str = "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/anthropic";

/// `createAiBindingFetch` 在缺少 `fetch` 时的错误文本。
pub const AI_BINDING_MISSING_FETCH_MESSAGE: &str =
    "createAiBindingFetch: the AI binding does not expose fetch()";

/// Placeholder value for auth headers on binding-routed requests.
///
/// API implementations require an API key or a recognized auth header (`authorization`,
/// `x-api-key`, `cf-aig-authorization`) before dispatch; binding calls are pre-authenticated, so
/// pass `cf-aig-authorization: Bearer cloudflare-gateway-binding` to satisfy the check. The
/// gateway ignores (and strips) `cf-aig-authorization` on binding-routed requests.
pub const CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL: &str = "cloudflare-gateway-binding";

/// The Workers AI binding's `fetch`（`env.AI.fetch()`）的结构化形态。
///
/// `Arc<dyn Fn>` 让 [`create_ai_binding_fetch`] 能把句柄克隆进返回的 [`HttpClient`]；
/// 返回类型直接复用 [`HttpClient::execute`] 的 `BoxFuture`，不需要再包一层 async 适配器。
///
/// 参数是适配器已经拼好的 [`HttpRequest`]（方法、URL、头、体、超时）与可选取消令牌；
/// 实现应把它当成一次普通 fetch。响应体按 [`HttpResponse`] 的流式形态返回，读取阶段的
/// 错误类型是 [`HttpError`]。
pub type AiBindingFetchFn = Arc<
    dyn Fn(
            HttpRequest,
            Option<CancellationToken>,
        ) -> BoxFuture<'static, Result<HttpResponse, HttpError>>
        + Send
        + Sync,
>;

/// The Workers AI binding (`env.AI`), described structurally.
///
/// TS 的 `AiBinding` 是「`aiGatewayLogId` 必填、`fetch` 可选」的对象；Rust 侧用同样的字段
/// 结构表达（`Option` 对应 `fetch?`），[`create_ai_binding_fetch`] 在构造时检查一次。
///
/// Rust 侧的 `ai_gateway_log_id` 是 `Option<String>`：它只用于把结构钉在 AI binding 上，
/// 构造函数不读取它，也没有「必须存在」的编译期或运行期校验。
#[derive(Clone, Default)]
pub struct AiBinding {
    /// `Ai` 上唯一独有的成员（类型钉住用）。
    pub ai_gateway_log_id: Option<String>,
    /// `Ai#fetch`；旧运行时可能没有（TS 的类型里仍是可选）。
    pub fetch: Option<AiBindingFetchFn>,
}

// `AiBindingFetchFn` 是 trait object，无法 derive `Debug`；手写实现只报告 `fetch` 是否存在，
// 避免尝试格式化闭包捕获的环境。
impl std::fmt::Debug for AiBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiBinding")
            .field("ai_gateway_log_id", &self.ai_gateway_log_id)
            .field("has_fetch", &self.fetch.is_some())
            .finish()
    }
}

/// 把请求原样交给 binding 的 [`HttpClient`]。
struct AiBindingHttpClient {
    fetch: AiBindingFetchFn,
}

impl HttpClient for AiBindingHttpClient {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
        signal: Option<&'a CancellationToken>,
    ) -> BoxFuture<'a, Result<HttpResponse, HttpError>> {
        // `fetch` 在构造时已绑定（TS 里 `binding.fetch.bind(binding)`），之后不再读取属性。
        // 返回的 future 借用 `&'a self`，而 binding 的 `fetch` 返回 `'static` future，
        // 因此先把 `Arc` 与取消令牌克隆出来，再在 async 块里调用。
        let fetch = Arc::clone(&self.fetch);
        let signal = signal.cloned();
        Box::pin(async move { fetch(request, signal).await })
    }
}

/// Create a `fetch` backed by the AI binding, for models whose `baseUrl` already names a route
/// the binding serves. Requests pass through untouched.
///
/// 构造时检查 `fetch` 是否存在（TS 的 `TypeError` 语义）。
///
/// # 参数与返回
///
/// - `binding` 按值接收：`fetch` 被取出后移入返回的客户端（每次请求再克隆一次 `Arc`），
///   调用方之后无法再通过原对象替换它；
/// - 返回值可直接放进 `StreamOptions.fetch`（`Option<Arc<dyn HttpClient>>`），供接受
///   自定义 fetch 的 API 适配器使用；Google Generative AI 与 Vertex 适配器拒绝任何
///   `Some(fetch)`（见模块文档）。
///
/// # 错误
///
/// 只在构造时失败：`fetch` 为 `None` 时返回 [`AiError`]，`kind` 为
/// [`AiErrorKind::Other`]、`name` 为 `"TypeError"`、消息逐字为
/// [`AI_BINDING_MISSING_FETCH_MESSAGE`]。`tests/cloudflare_ai_binding.rs` 的
/// `rejects_a_binding_with_no_fetch_at_construction_not_on_first_request` 锁定了消息
/// 片段与 `name`。
///
/// `spec/ai-api-other.md` §2.8 规定检查发生在构造时（**不是**首次请求时）。把失败提前到
/// 配置阶段、避免错误出现在离配置很远的地方，是便于定位问题的设计推断：spec 未给出该
/// 理由，`tests/cloudflare_ai_binding.rs` 的测试名与注释也只强调 early 与“点名原因”。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持原样。
#[allow(clippy::result_large_err)]
pub fn create_ai_binding_fetch(binding: AiBinding) -> Result<Arc<dyn HttpClient>, AiError> {
    let Some(fetch) = binding.fetch else {
        return Err(
            AiError::new(AiErrorKind::Other, AI_BINDING_MISSING_FETCH_MESSAGE)
                .with_name("TypeError"),
        );
    };
    Ok(Arc::new(AiBindingHttpClient { fetch }))
}
