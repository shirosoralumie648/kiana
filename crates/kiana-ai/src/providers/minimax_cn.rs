//! providers/minimax-cn.ts：`minimax_cn_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/minimax-cn.ts`
//!
//! 固定参数：id `minimax-cn`、base URL `https://api.minimaxi.com/anthropic`、环境变量鉴权
//! `MINIMAX_CN_API_KEY`、chat 实现 `anthropic-messages`。与 [`crate::providers::minimax`]
//! 的差别是域名、id、展示名与环境变量名；两者是独立 provider，模型目录也各自生成。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use crate::api::anthropic_messages::anthropic_messages_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/minimax-cn.ts`
///
/// 构造并返回 `minimax-cn` provider；只解析目录与组装参数，不发起网络请求。
pub fn minimax_cn_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "minimax-cn".into(),
        name: Some("MiniMax CN".into()),
        base_url: Some("https://api.minimaxi.com/anthropic".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth(
                "MiniMax CN API key",
                &["MINIMAX_CN_API_KEY"],
            )),
            oauth: None,
        },
        models: catalog_chat_models("minimax-cn"),
        api: Some(anthropic_messages_api().into()),
        ..Default::default()
    })
    .expect("minimax-cn provider declares an api implementation")
}
