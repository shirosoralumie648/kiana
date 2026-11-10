//! providers/typesafe.ts：`typesafe_provider` provider 工厂。
//!
//! 这个文件只做组装：把 TypeSafe 的固定配置交给 [`create_provider`]。与多数工厂不同，
//! 它**没有 chat/stream 实现**，只声明一个分类器实现 `typesafe-system-one`；因此
//! `get_models()`（chat 目录）恒为空，模型只出现在 `get_all_models()` 的
//! `AnyModel::Classifier` 条目里，供 `Models.classify` 使用。
//!
//! 工厂由 [`crate::providers::all::builtin_providers`] 按 `spec/ai-core.md` §2.3.1 的固定
//! 顺序调用。没有 `base_url`：每条分类模型的 `baseUrl` 来自目录数据
//! （`crates/kiana-ai/data/typesafe.json`）。
//!
//! # 约束
//!
//! - `classifiers` 的键必须与目录模型条目的 `api` 逐字相同（`typesafe-system-one`），
//!   否则分类请求找不到实现。
//! - 模型只取 classifier 表；TS 的同名分片还导出 `TYPESAFE_MODELS`/`TYPESAFE_IMAGE_MODELS`，
//!   但本 provider 按 TS 源码只使用 `TYPESAFE_CLASSIFIER_MODELS`。
//! - 鉴权只声明 `TYPESAFE_API_KEY`，工厂不读取环境变量。
//!
//! # 失败模式
//!
//! `create_provider` 的校验是「`api`/`images`/`classifiers` 至少一个非空」，本工厂提供
//! 一个 classifier，满足条件。末尾 `expect` 的提示文案沿用了其他工厂的
//! “declares an api implementation”，但实际不变量是「至少一个实现」，删掉 `classifiers`
//! 才会 panic。
//!
//! TS: `packages/ai/src/providers/typesafe.ts`

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::typesafe_system_one::typesafe_system_one_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_classifier_models;

/// 构造内置的 TypeSafe provider（纯分类器 provider，无 chat 流）。
///
/// 返回 `Arc<dyn Provider>`：`id = "typesafe"`、展示名 `TypeSafe`，模型目录为 classifier
/// 表，`classifiers` map 中注册 `typesafe-system-one`。由于没有 `api`，对该 provider 的
/// chat 请求不会在构造期失败，而是在请求时得到
/// `Provider typesafe has no API implementation for "<api>"` 的错误事件（`stream` 错误码）。
///
/// # Panics
///
/// 仅当 `create_provider` 拒绝构造时 panic；本工厂至少有一个 classifier 实现。
///
/// TS: `providers/typesafe.ts#typesafeProvider`
pub fn typesafe_provider() -> Arc<dyn Provider> {
    let mut classifiers = IndexMap::new();
    // 键必须与目录模型条目的 `api` 逐字一致；`create_provider` 按 `model.api` 查这张表。
    classifiers.insert(
        "typesafe-system-one".to_string(),
        typesafe_system_one_api() as Arc<dyn crate::models::ProviderClassifier>,
    );
    create_provider(CreateProviderOptions {
        id: "typesafe".into(),
        name: Some("TypeSafe".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("TypeSafe API key", &["TYPESAFE_API_KEY"])),
            oauth: None,
        },
        models: catalog_classifier_models("typesafe"),
        classifiers: Some(classifiers),
        ..Default::default()
    })
    .expect("typesafe provider declares an api implementation")
}
