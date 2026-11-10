//! providers/huggingface.ts：`huggingface_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/huggingface.ts`
//!
//! 固定参数：id `huggingface`、base URL `https://router.huggingface.co/v1`、环境变量鉴权
//! `HF_TOKEN`、chat 实现 `openai-completions`。提示文案与 TS 相同，为
//! `Enter Hugging Face token`（由 `env_api_key_auth` 按 `name` 生成）。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/huggingface.ts`
///
/// 构造并返回 `huggingface` provider；只解析目录与组装参数，不发起网络请求。
pub fn huggingface_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "huggingface".into(),
        name: Some("Hugging Face".into()),
        base_url: Some("https://router.huggingface.co/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Hugging Face token", &["HF_TOKEN"])),
            oauth: None,
        },
        models: catalog_chat_models("huggingface"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("huggingface provider declares an api implementation")
}
