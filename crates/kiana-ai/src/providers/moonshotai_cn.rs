//! providers/moonshotai-cn.ts：`moonshotai_cn_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/moonshotai-cn.ts`
//!
//! 固定参数：id `moonshotai-cn`、base URL `https://api.moonshot.cn/v1`、环境变量鉴权
//! `MOONSHOT_API_KEY`（与国际站 [`crate::providers::moonshotai`] 同名）、chat 实现
//! `openai-completions`。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/moonshotai-cn.ts`
///
/// 构造并返回 `moonshotai-cn` provider；只解析目录与组装参数，不发起网络请求。
pub fn moonshotai_cn_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "moonshotai-cn".into(),
        name: Some("Moonshot AI CN".into()),
        base_url: Some("https://api.moonshot.cn/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth(
                "Moonshot AI API key",
                &["MOONSHOT_API_KEY"],
            )),
            oauth: None,
        },
        models: catalog_chat_models("moonshotai-cn"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("moonshotai-cn provider declares an api implementation")
}
