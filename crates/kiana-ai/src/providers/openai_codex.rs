//! providers/openai-codex.ts：`openai_codex_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/openai-codex.ts`
//!
//! 与 `openai` provider 的区别：
//!
//! - 这是旧版 Codex 入口，base URL 是 ChatGPT 后端 `https://chatgpt.com/backend-api`，
//!   chat 实现是 `openai-codex-responses`（SSE + WebSocket + zstd 压缩）；
//! - **只有 OAuth**（展示名 `OpenAI (ChatGPT Plus/Pro)`），没有 API key 分支：
//!   `ProviderAuth.api_key` 为 `None`，未登录时只能走 OAuth 登录；
//! - 模型目录由 `providers/data/openai-codex.json` 生成，与 `openai` 的目录相互独立。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use crate::api::openai_codex_responses::openai_codex_responses_api;
use crate::auth::helpers::{LazyOAuthInput, lazy_oauth};
use crate::auth::oauth::load::load_openai_codex_oauth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/openai-codex.ts#openaiCodexProvider`
///
/// 构造并返回 `openai-codex` provider；OAuth 实现由 `load_openai_codex_oauth` 惰性加载，
/// 构造阶段不触发凭据读取或网络请求。
pub fn openai_codex_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "openai-codex".into(),
        name: Some("OpenAI Codex (legacy)".into()),
        base_url: Some("https://chatgpt.com/backend-api".into()),
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(lazy_oauth(LazyOAuthInput {
                name: "OpenAI (ChatGPT Plus/Pro)".into(),
                is_subscription: Some(true),
                login_label: None,
                load: Arc::new(load_openai_codex_oauth),
            })),
        },
        models: catalog_chat_models("openai-codex"),
        api: Some(openai_codex_responses_api().into()),
        ..Default::default()
    })
    .expect("openai-codex provider declares an api implementation")
}
