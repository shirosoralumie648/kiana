//! providers/kimi-coding.ts：`kimi_coding_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/kimi-coding.ts`
//!
//! 固定参数：id `kimi-coding`、base URL `https://api.kimi.com/coding/v1`、chat 实现
//! `openai-completions`。鉴权有两条路：环境变量 `KIMI_API_KEY`（提示文案
//! `Enter Kimi API key`），或 Kimi Code 订阅 OAuth（展示名 `Kimi Code (subscription)`、
//! 登录标签 `Sign in with Kimi Code`）。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::helpers::{LazyOAuthInput, lazy_oauth};
use crate::auth::oauth::load::load_kimi_coding_oauth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/kimi-coding.ts`
///
/// 构造并返回 `kimi-coding` provider；OAuth 实现通过 `load_kimi_coding_oauth` 惰性加载，
/// 构造阶段不会触发它。
pub fn kimi_coding_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "kimi-coding".into(),
        name: Some("Kimi For Coding".into()),
        base_url: Some("https://api.kimi.com/coding/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Kimi API key", &["KIMI_API_KEY"])),
            oauth: Some(lazy_oauth(LazyOAuthInput {
                name: "Kimi Code (subscription)".into(),
                is_subscription: Some(true),
                login_label: Some("Sign in with Kimi Code".into()),
                load: Arc::new(load_kimi_coding_oauth),
            })),
        },
        models: catalog_chat_models("kimi-coding"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("kimi-coding provider declares an api implementation")
}
