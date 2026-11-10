//! providers/mistral.ts：`mistral_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/mistral.ts`
//!
//! 固定参数：id `mistral`、base URL `https://api.mistral.ai`、环境变量鉴权
//! `MISTRAL_API_KEY`、chat 实现 `mistral-conversations`（Mistral 自己的 Conversations
//! 协议，不是 OpenAI 兼容层）。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use crate::api::mistral_conversations::mistral_conversations_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/mistral.ts`
///
/// 构造并返回 `mistral` provider；只解析目录与组装参数，不发起网络请求。
pub fn mistral_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "mistral".into(),
        name: Some("Mistral".into()),
        base_url: Some("https://api.mistral.ai".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Mistral API key", &["MISTRAL_API_KEY"])),
            oauth: None,
        },
        models: catalog_chat_models("mistral"),
        api: Some(mistral_conversations_api().into()),
        ..Default::default()
    })
    .expect("mistral provider declares an api implementation")
}
