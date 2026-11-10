//! providers/groq.ts：`groq_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/groq.ts`
//!
//! 固定参数：id `groq`、base URL `https://api.groq.com/openai/v1`、环境变量鉴权
//! `GROQ_API_KEY`、chat 实现 `openai-completions`。目录里的每个模型都声明自己的 `api`，
//! 单一实现（`Single`）不检查它。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]；`expect` 的依据见 `providers`
//! 模块文档。

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/groq.ts`
///
/// 构造并返回 `groq` provider；只解析目录与组装参数，不发起网络请求。
pub fn groq_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "groq".into(),
        name: Some("Groq".into()),
        base_url: Some("https://api.groq.com/openai/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Groq API key", &["GROQ_API_KEY"])),
            oauth: None,
        },
        models: catalog_chat_models("groq"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("groq provider declares an api implementation")
}
