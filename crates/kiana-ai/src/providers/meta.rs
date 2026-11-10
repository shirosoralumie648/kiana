//! providers/meta.ts：`meta_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/meta.ts`
//!
//! 固定参数：id `meta`、base URL `https://api.meta.ai/v1`、chat 实现 `openai-responses`。
//! 鉴权既有环境变量 `META_API_KEY`（提示 `Enter Meta Model API key`），也有 Meta 订阅
//! OAuth（展示名 `Meta (Muse subscription)`、登录标签 `Sign in with Meta`）。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use crate::api::openai_responses::openai_responses_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::helpers::{LazyOAuthInput, lazy_oauth};
use crate::auth::oauth::load::load_meta_oauth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/meta.ts`
///
/// 构造并返回 `meta` provider；OAuth 实现由 `load_meta_oauth` 惰性加载，构造阶段不触发。
pub fn meta_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "meta".into(),
        name: Some("Meta".into()),
        base_url: Some("https://api.meta.ai/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Meta Model API key", &["META_API_KEY"])),
            oauth: Some(lazy_oauth(LazyOAuthInput {
                name: "Meta (Muse subscription)".into(),
                is_subscription: Some(true),
                login_label: Some("Sign in with Meta".into()),
                load: Arc::new(load_meta_oauth),
            })),
        },
        models: catalog_chat_models("meta"),
        api: Some(openai_responses_api().into()),
        ..Default::default()
    })
    .expect("meta provider declares an api implementation")
}
