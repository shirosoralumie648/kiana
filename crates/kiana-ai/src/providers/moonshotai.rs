//! providers/moonshotai.ts：`moonshotai_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/moonshotai.ts`
//!
//! 固定参数：id `moonshotai`、base URL `https://api.moonshot.ai/v1`、环境变量鉴权
//! `MOONSHOT_API_KEY`、chat 实现 `openai-completions`。国际站与中国站
//! （[`crate::providers::moonshotai_cn`]）使用同一个环境变量名，但主机与 id 不同。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/moonshotai.ts`
///
/// 构造并返回 `moonshotai` provider；只解析目录与组装参数，不发起网络请求。
pub fn moonshotai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "moonshotai".into(),
        name: Some("Moonshot AI".into()),
        base_url: Some("https://api.moonshot.ai/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth(
                "Moonshot AI API key",
                &["MOONSHOT_API_KEY"],
            )),
            oauth: None,
        },
        models: catalog_chat_models("moonshotai"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("moonshotai provider declares an api implementation")
}
