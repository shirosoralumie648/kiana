//! providers/github-copilot.ts：`github_copilot_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/github-copilot.ts`
//!
//! 与其他薄工厂的差别：
//!
//! - 同时注册 `anthropic-messages`、`openai-completions`、`openai-responses` 三种 chat
//!   实现，按 `model.api` 精确分派；目录里若出现第四种 api，只会在 `stream` 时得到错误流；
//! - 鉴权既支持环境变量 `COPILOT_GITHUB_TOKEN`，也支持 GitHub Copilot 订阅 OAuth；
//! - `filter_models` 在凭证是 OAuth 且带 `availableModelIds`（全字符串数组）时，把目录
//!   裁剪成账号实际可用的模型；字段缺失、不是数组或含非字符串元素时**原样返回**（不做
//!   部分过滤），空数组则会得到空目录。该字段由 OAuth 登录/刷新写入
//!   （`auth/oauth/github_copilot.rs`）。
//!
//! OAuth 凭证解析成功时还会带每凭证的 `baseUrl`：由 Copilot token 里的 `proxy-ep`
//! 推导（`proxy.x` 映射为 `api.x`，其他 host 原样使用）；无 `proxy-ep` 时依次回落到
//! 企业域 `https://copilot-api.<domain>` 和默认 `https://api.individual.githubcopilot.com`。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::anthropic_messages::anthropic_messages_api;
use crate::api::openai_completions::openai_completions_api;
use crate::api::openai_responses::openai_responses_api;
use crate::auth::helpers::{LazyOAuthInput, env_api_key_auth, lazy_oauth};
use crate::auth::oauth::load::load_github_copilot_oauth;
use crate::auth::types::{Credential, ProviderAuth};
use crate::models::provider::ProviderApiImpl;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/github-copilot.ts#githubCopilotProvider`
///
/// 构造并返回 `github-copilot` provider。`filter_models` 闭包在 `Models` 计算可用模型
/// 时按当前凭证执行（`get_available`，以及默认 `filter_all_models` 的回退）；这里只在
/// 构造期安装闭包，不读取凭证。
pub fn github_copilot_provider() -> Arc<dyn Provider> {
    let mut api = IndexMap::new();
    api.insert("anthropic-messages".to_string(), anthropic_messages_api());
    api.insert("openai-completions".to_string(), openai_completions_api());
    api.insert("openai-responses".to_string(), openai_responses_api());
    create_provider(CreateProviderOptions {
        id: "github-copilot".into(),
        name: Some("GitHub Copilot".into()),
        base_url: Some("https://api.individual.githubcopilot.com".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth(
                "GitHub Copilot token",
                &["COPILOT_GITHUB_TOKEN"],
            )),
            oauth: Some(lazy_oauth(LazyOAuthInput {
                name: "GitHub Copilot".into(),
                is_subscription: Some(true),
                login_label: None,
                load: Arc::new(load_github_copilot_oauth),
            })),
        },
        models: catalog_chat_models("github-copilot"),
        // 与 TS 相同的三段判断：非 OAuth 不过滤；availableModelIds 缺失/非数组/含非字符串
        // 也退化为不过滤（不做部分过滤）。空数组是合法输入，结果为「一个模型都不可用」。
        filter_models: Some(Arc::new(|models, credential| {
            let Some(Credential::OAuth(credential)) = credential else {
                return models;
            };
            let available_model_ids = credential.get("availableModelIds");
            let Some(values) = available_model_ids.and_then(serde_json::Value::as_array) else {
                return models;
            };
            if !values.iter().all(serde_json::Value::is_string) {
                return models;
            }
            let available: HashSet<&str> = values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect();
            models
                .into_iter()
                .filter(|model| available.contains(model.id.as_str()))
                .collect()
        })),
        api: Some(ProviderApiImpl::ByApi(api)),
        ..Default::default()
    })
    .expect("github-copilot provider declares an api implementation")
}
