//! providers/google.ts：`google_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/google.ts`
//!
//! 固定参数：id `google`、base URL `https://generativelanguage.googleapis.com/v1beta`、
//! 环境变量鉴权 `GEMINI_API_KEY`、chat 实现 `google-generative-ai`，模型目录来自编译期
//! 静态表。Google AI Studio 只有 API key，没有 OAuth 分支。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]；`expect` 的依据见 `providers`
//! 模块文档（每个工厂都至少有一个 chat 实现）。

use std::sync::Arc;

use crate::api::google_generative_ai::google_generative_ai_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/google.ts`
///
/// 构造并返回 `google` provider；只解析目录与组装参数，不发起网络请求。
pub fn google_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "google".into(),
        name: Some("Google".into()),
        base_url: Some("https://generativelanguage.googleapis.com/v1beta".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Gemini API key", &["GEMINI_API_KEY"])),
            oauth: None,
        },
        models: catalog_chat_models("google"),
        api: Some(google_generative_ai_api().into()),
        ..Default::default()
    })
    .expect("google provider declares an api implementation")
}
