//! providers/vercel-ai-gateway.ts：`vercel_ai_gateway_provider` provider 工厂。
//!
//! 这个文件只做组装：Vercel AI Gateway 用 Anthropic Messages 协议代理多家模型，同时提供
//! TypeSafe System One 分类接口（TS 源码注释：AI Gateway serves TypeSafe's System One
//! protocol at /typesafe/v1/systemone）。
//!
//! 与只读一张目录的工厂不同，这里把两张静态表拼成 `models`：chat 表
//! （`api: "anthropic-messages"`）在前，classifier 表（`api: "typesafe-system-one"`）
//! 在后，两表内部顺序沿用生成表。chat 实现以 `ProviderApiImpl::Single` 形式挂载，所有
//! chat 模型都走 [`anthropic_messages_api`]（不按 `model.api` 再查表）；分类实现放在 map
//! 里，由 `create_provider` 按 `model.api` 查找，命中 [`typesafe_system_one_api`]。
//!
//! 工厂由 [`crate::providers::all::builtin_providers`] 按 `spec/ai-core.md` §2.3.1 的固定
//! 顺序调用。base URL 固定为 `https://ai-gateway.vercel.sh`，鉴权只声明
//! `AI_GATEWAY_API_KEY`（工厂不读环境变量）。
//!
//! # 约束与失败模式
//!
//! - `classifiers` 的键必须与 classifier 模型条目的 `api` 逐字相同
//!   （`typesafe-system-one`），否则分类请求找不到实现。
//! - 本工厂同时提供 `api` 与 `classifiers`，满足 `create_provider`「至少一个实现」的校验，
//!   末尾 `expect` 不会触发。
//! - 目录解析失败会 panic（`catalog_*` 的既有契约，数据由 `build.rs` 校验）。
//!
//! TS: `packages/ai/src/providers/vercel-ai-gateway.ts`

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::anthropic_messages::anthropic_messages_api;
use crate::api::typesafe_system_one::typesafe_system_one_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::{catalog_chat_models, catalog_classifier_models};

/// 构造内置的 Vercel AI Gateway provider。
///
/// 返回 `Arc<dyn Provider>`：`id = "vercel-ai-gateway"`、展示名 `Vercel AI Gateway`，
/// chat 实现为 `anthropic-messages`，默认 base URL `https://ai-gateway.vercel.sh`。
/// `models` 是 chat 表与 classifier 表的拼接；`classifiers` 注册 `typesafe-system-one`。
/// 鉴权只声明 `AI_GATEWAY_API_KEY`，实际读取在请求前的 auth 解析阶段。
///
/// # Panics
///
/// 仅当 `create_provider` 拒绝构造（没有任何实现）时 panic；本工厂至少提供 chat 实现与
/// 一个 classifier。
///
/// TS: `providers/vercel-ai-gateway.ts#vercelAIGatewayProvider`
pub fn vercel_ai_gateway_provider() -> Arc<dyn Provider> {
    let mut models = catalog_chat_models("vercel-ai-gateway");
    // chat 与 classifier 共用同一个 `models` 列表：前者供 `get_models()`/流式请求，
    // 后者供 `get_all_models()`/`Models.classify`；两张表的顺序都保留。
    models.extend(catalog_classifier_models("vercel-ai-gateway"));
    let mut classifiers = IndexMap::new();
    // 键必须与 classifier 模型条目的 `api` 逐字一致；`create_provider` 按 `model.api` 查表。
    classifiers.insert(
        "typesafe-system-one".to_string(),
        typesafe_system_one_api() as Arc<dyn crate::models::ProviderClassifier>,
    );
    create_provider(CreateProviderOptions {
        id: "vercel-ai-gateway".into(),
        name: Some("Vercel AI Gateway".into()),
        base_url: Some("https://ai-gateway.vercel.sh".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth(
                "Vercel AI Gateway API key",
                &["AI_GATEWAY_API_KEY"],
            )),
            oauth: None,
        },
        models,
        api: Some(anthropic_messages_api().into()),
        classifiers: Some(classifiers),
        ..Default::default()
    })
    .expect("vercel-ai-gateway provider declares an api implementation")
}
