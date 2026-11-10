//! providers/openai.ts：`openai_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/openai.ts`
//!
//! 与其余薄工厂的差别：
//!
//! - 模型列表 = chat 目录 + classifier 目录（`catalog_classifier_models("openai")`）；
//! - chat 实现是 `openai-responses`，classifier 实现固定为 `openai-decisions`
//!   （`POST /v1/decisions`）；
//! - `filter_all_models` 在凭证是 OAuth（Sign in with ChatGPT）时剔除 classifier：
//!   ChatGPT token 只能走 Responses API，Decisions API 会拒绝它（见
//!   `api/openai_decisions.rs` 的模块文档）；API key 凭证不受影响；
//! - 鉴权同时支持 `OPENAI_API_KEY` 与 ChatGPT 订阅 OAuth（展示名
//!   `OpenAI (ChatGPT subscription)`，登录标签 `Sign in with ChatGPT`）。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::openai_decisions::openai_decisions_api;
use crate::api::openai_responses::openai_responses_api;
use crate::auth::helpers::{LazyOAuthInput, env_api_key_auth, lazy_oauth};
use crate::auth::oauth::load::load_openai_chatgpt_oauth;
use crate::auth::types::{Credential, ProviderAuth};
use crate::models::{CreateProviderOptions, Provider, create_provider, is_model_type};
use crate::providers::{catalog_chat_models, catalog_classifier_models};
use crate::types::ModelType;

/// TS: `providers/openai.ts#openaiProvider`
///
/// 构造并返回 `openai` provider；`filter_all_models` 闭包在查询可用模型时执行，构造阶段
/// 不读取凭证；OAuth 实现由 `load_openai_chatgpt_oauth` 惰性加载。
pub fn openai_provider() -> Arc<dyn Provider> {
    let mut models = catalog_chat_models("openai");
    models.extend(catalog_classifier_models("openai"));
    let mut classifiers = IndexMap::new();
    classifiers.insert(
        "openai-decisions".to_string(),
        openai_decisions_api() as Arc<dyn crate::models::ProviderClassifier>,
    );
    create_provider(CreateProviderOptions {
        id: "openai".into(),
        name: Some("OpenAI".into()),
        base_url: Some("https://api.openai.com/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("OpenAI API key", &["OPENAI_API_KEY"])),
            oauth: Some(lazy_oauth(LazyOAuthInput {
                name: "OpenAI (ChatGPT subscription)".into(),
                is_subscription: Some(true),
                login_label: Some("Sign in with ChatGPT".into()),
                load: Arc::new(load_openai_chatgpt_oauth),
            })),
        },
        models,
        // Sign in with ChatGPT tokens only reach the Responses API; the Decisions API rejects them.
        filter_all_models: Some(Arc::new(|models, credential| {
            if matches!(credential, Some(Credential::OAuth(_))) {
                models
                    .into_iter()
                    .filter(|model| !is_model_type(model, ModelType::Classifier))
                    .collect()
            } else {
                models
            }
        })),
        api: Some(openai_responses_api().into()),
        classifiers: Some(classifiers),
        ..Default::default()
    })
    .expect("openai provider declares an api implementation")
}
