//! providers/minimax.ts：`minimax_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/minimax.ts`
//!
//! 固定参数：id `minimax`、base URL `https://api.minimax.io/anthropic`、环境变量鉴权
//! `MINIMAX_API_KEY`。注意 chat 实现是 `anthropic-messages` 而不是 OpenAI 兼容层：
//! MiniMax 的 `/anthropic` 端点实现 Anthropic Messages 协议，目录里的模型 `api` 也据此声明。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use crate::api::anthropic_messages::anthropic_messages_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/minimax.ts`
///
/// 构造并返回 `minimax` provider；只解析目录与组装参数，不发起网络请求。
pub fn minimax_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "minimax".into(),
        name: Some("MiniMax".into()),
        base_url: Some("https://api.minimax.io/anthropic".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("MiniMax API key", &["MINIMAX_API_KEY"])),
            oauth: None,
        },
        models: catalog_chat_models("minimax"),
        api: Some(anthropic_messages_api().into()),
        ..Default::default()
    })
    .expect("minimax provider declares an api implementation")
}
