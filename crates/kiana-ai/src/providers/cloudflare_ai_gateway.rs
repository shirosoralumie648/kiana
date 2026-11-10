//! providers/cloudflare-ai-gateway.ts：`cloudflare_ai_gateway_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/cloudflare-ai-gateway.ts`
//!
//! # 这个 provider 的组成
//!
//! - 目录：编译期生成的 `cloudflare-ai-gateway` chat 表。目录模型的 `api` 决定走哪个
//!   上游协议，`baseUrl` 里带账户/网关占位符；
//! - api map：`anthropic-messages`、`openai-completions`、`openai-responses` 三项，全部用
//!   [`cloudflare_streams`] 包装，在分派前把占位符替换成真实的 account/gateway id；
//! - 鉴权：[`cloudflare_ai_gateway_auth`]——`cf-aig-authorization: Bearer <key>` 加抑制
//!   `Authorization`/`x-api-key`，并把 account/gateway id 作为 provider env 返回。
//!
//! 三个 api 是**固定**登记的，不从目录推断，原因见下面工厂函数的注释（models.dev 的目录
//! 会周期性增删 `workers-ai/*` 条目）。
//!
//! # 注意事项
//!
//! - provider 不设 `base_url`：每个模型的端点来自目录模板 + 鉴权阶段解析出的 env；
//! - api map 的键必须与模型目录里的 `api` 逐字相同，否则命中的模型会拿到错误流
//!   （`ProviderApiImpl::ByApi` 精确查找，区分大小写）。

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::anthropic_messages::anthropic_messages_api;
use crate::api::openai_completions::openai_completions_api;
use crate::api::openai_responses::openai_responses_api;
use crate::auth::types::ProviderAuth;
use crate::models::provider::ProviderApiImpl;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;
use crate::providers::cloudflare_auth::cloudflare_ai_gateway_auth;
use crate::providers::cloudflare_stream::cloudflare_streams;

/// The api map is pinned to all three APIs: models.dev's gateway catalog drops and
/// restores `workers-ai/*` (openai-completions) entries over time, and inference from
/// `models` alone would otherwise reject the openai-completions entry whenever the
/// generated catalog happens to contain none.
///
/// 三个实现都先经 [`cloudflare_streams`] 包装，再由 `Models` 按 `model.api` 分派；
/// 鉴权用 [`cloudflare_ai_gateway_auth`]（返回 `cf-aig-authorization` 头 + account/gateway
/// env）。`expect` 分支不可达：api map 非空。
///
/// TS: `providers/cloudflare-ai-gateway.ts#cloudflareAIGatewayProvider`
pub fn cloudflare_ai_gateway_provider() -> Arc<dyn Provider> {
    let mut api = IndexMap::new();
    api.insert(
        "anthropic-messages".to_string(),
        cloudflare_streams(anthropic_messages_api()),
    );
    api.insert(
        "openai-completions".to_string(),
        cloudflare_streams(openai_completions_api()),
    );
    api.insert(
        "openai-responses".to_string(),
        cloudflare_streams(openai_responses_api()),
    );
    create_provider(CreateProviderOptions {
        id: "cloudflare-ai-gateway".into(),
        name: Some("Cloudflare AI Gateway".into()),
        auth: ProviderAuth {
            api_key: Some(cloudflare_ai_gateway_auth()),
            oauth: None,
        },
        models: catalog_chat_models("cloudflare-ai-gateway"),
        api: Some(ProviderApiImpl::ByApi(api)),
        ..Default::default()
    })
    .expect("cloudflare-ai-gateway provider declares an api implementation")
}
