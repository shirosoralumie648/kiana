//! providers/cloudflare-workers-ai.ts：`cloudflare_workers_ai_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/cloudflare-workers-ai.ts`
//!
//! # 这个 provider 的组成
//!
//! - 模型列表 = chat 表 + classifier 表。TS 的工厂把两个分片一起展开进 `models`；Rust 侧
//!   同样把它们都放进静态基线（`AnyModel::Chat` 与 `AnyModel::Classifier`），因此
//!   `Provider::get_all_models()` 能看到 classifier 条目，而 `get_models()` 仍只返回 chat
//!   模型（`CreatedProvider` 会按 `ModelType::Chat` 过滤）；
//! - chat api：`openai-completions`，外面包 [`cloudflare_streams`]——目录 baseUrl 里的
//!   `{CLOUDFLARE_ACCOUNT_ID}` 由它在分派前替换；
//! - classifier：key 为 `cloudflare-workers-ai-system-one`（与目录模型的 `api` 逐字对应），
//!   值用 [`cloudflare_classifier`] 包装同一个占位符解析逻辑；
//! - 鉴权：[`cloudflare_workers_ai_auth`]，返回 `ModelAuth.api_key` + account id env。
//!
//! 注意 catalog 分片的常量名前缀是 `CLOUDFLARE_WORKERS_AI_`（不是 `WORKERS_AI_`），这里
//! 通过 provider id `cloudflare-workers-ai` 查询，无需关心前缀。
//!
//! # 注意事项
//!
//! - provider 不设 `base_url`；端点完全来自目录模板与鉴权 env；
//! - classifier map 的键必须与模型 `api` 精确匹配，模型才能分派到分类实现。

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::cloudflare_workers_ai_system_one::cloudflare_workers_ai_system_one_api;
use crate::api::openai_completions::openai_completions_api;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::cloudflare_auth::cloudflare_workers_ai_auth;
use crate::providers::cloudflare_stream::{cloudflare_classifier, cloudflare_streams};
use crate::providers::{catalog_chat_models, catalog_classifier_models};

/// 构造 `cloudflare-workers-ai` provider（展示名 `Cloudflare Workers AI`）。
///
/// 模型 = chat 表 + classifier 表；chat 走包装后的 `openai-completions`，classifier 走
/// 包装后的 `cloudflare-workers-ai-system-one` 实现；鉴权用
/// [`cloudflare_workers_ai_auth`]。`expect` 分支不可达：chat api 与 classifier 都已声明。
///
/// TS: `providers/cloudflare-workers-ai.ts#cloudflareWorkersAIProvider`
pub fn cloudflare_workers_ai_provider() -> Arc<dyn Provider> {
    let mut models = catalog_chat_models("cloudflare-workers-ai");
    models.extend(catalog_classifier_models("cloudflare-workers-ai"));
    let mut classifiers = IndexMap::new();
    classifiers.insert(
        "cloudflare-workers-ai-system-one".to_string(),
        cloudflare_classifier(cloudflare_workers_ai_system_one_api()),
    );
    create_provider(CreateProviderOptions {
        id: "cloudflare-workers-ai".into(),
        name: Some("Cloudflare Workers AI".into()),
        auth: ProviderAuth {
            api_key: Some(cloudflare_workers_ai_auth()),
            oauth: None,
        },
        models,
        api: Some(cloudflare_streams(openai_completions_api()).into()),
        classifiers: Some(classifiers),
        ..Default::default()
    })
    .expect("cloudflare-workers-ai provider declares an api implementation")
}
