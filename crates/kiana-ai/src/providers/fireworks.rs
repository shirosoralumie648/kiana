//! providers/fireworks.ts：`fireworks_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/fireworks.ts`
//!
//! Fireworks 同时提供 Anthropic Messages 与 OpenAI Completions 兼容接口，因此工厂用
//! `ProviderApiImpl::ByApi` 注册两份实现，由模型目录里的 `api` 字段精确分派。provider 级
//! 默认 base URL 是 `https://api.fireworks.ai/inference`；目录里的模型各自带 `baseUrl`
//! （OpenAI 兼容条目是 `.../inference/v1`，Anthropic 条目没有 `/v1`）。若某个模型的
//! `api` 不在映射中，`stream` 会在 setup 阶段返回 “no API implementation” 错误流，
//! 而不是回退到另一份实现（见 `models/provider.rs` 的 `no_api_stream`）。
//!
//! 鉴权只有环境变量 `FIREWORKS_API_KEY`；调用方是
//! [`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::anthropic_messages::anthropic_messages_api;
use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::provider::ProviderApiImpl;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/fireworks.ts#fireworksProvider`
///
/// 构造并返回 `fireworks` provider；chat 实现按 `model.api` 在
/// `anthropic-messages`/`openai-completions` 之间选择。不发起网络请求。
pub fn fireworks_provider() -> Arc<dyn Provider> {
    let mut api = IndexMap::new();
    api.insert("anthropic-messages".to_string(), anthropic_messages_api());
    api.insert("openai-completions".to_string(), openai_completions_api());
    create_provider(CreateProviderOptions {
        id: "fireworks".into(),
        name: Some("Fireworks".into()),
        base_url: Some("https://api.fireworks.ai/inference".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth(
                "Fireworks API key",
                &["FIREWORKS_API_KEY"],
            )),
            oauth: None,
        },
        models: catalog_chat_models("fireworks"),
        api: Some(ProviderApiImpl::ByApi(api)),
        ..Default::default()
    })
    .expect("fireworks provider declares an api implementation")
}
