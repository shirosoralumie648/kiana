//! providers/together.ts：`together_provider` provider 工厂。
//!
//! 这个文件只做组装：把 Together 的固定配置（id、展示名、base URL、鉴权方式、模型目录、
//! chat 实现）交给 [`create_provider`]，本身没有网络、缓存或状态。请求构建与流解析在
//! [`crate::api::openai_completions`]。
//!
//! 工厂由 [`crate::providers::all::builtin_providers`] 按 `spec/ai-core.md` §2.3.1 的固定
//! 顺序调用（`together` 在该表第 33 位），CLI 的 provider 列表与 help 输出依赖这个顺序。
//!
//! # 约束
//!
//! - `id` 必须与 `crates/kiana-ai/data/together.json` 及目录条目的 `provider` 字段一致；
//!   不一致时，`get_builtin_models("together")` 与工厂实例会指向不同的 provider 名。
//! - 模型来自编译期静态表：JSON 文档本身由 `LazyLock` 只解析一次；每次调用
//!   [`crate::providers::catalog_chat_models`] 会把静态表中该 provider 的每个模型条目重新
//!   反序列化为 `Model`（结果无缓存），顺序沿用生成表的 `IndexMap` 插入序。
//! - `base_url` 是 provider 级默认值；目录里的模型条目自带 `baseUrl`（当前数据与工厂值一致）。
//!
//! # 失败模式
//!
//! `create_provider` 只在 `api`/`images`/`classifiers` 一个都没有时返回 `Err`；本工厂提供
//! `api`，所以末尾的 `expect` 不会触发。目录 JSON 损坏导致的 panic 属于构建期数据错误
//! （`build.rs` 会先校验）。
//!
//! TS: `packages/ai/src/providers/together.ts`

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// 构造内置的 Together provider。
///
/// 返回 `Arc<dyn Provider>`：`id = "together"`、展示名 `Together`、默认 base URL
/// `https://api.together.ai/v1`，chat 实现固定为 `openai-completions`，模型目录为
/// `together` 的全部 chat 模型。鉴权只声明 `TOGETHER_API_KEY`；环境变量的实际读取发生在
/// 请求前的 auth 解析阶段，不在工厂里。
///
/// # Panics
///
/// 仅当 `create_provider` 拒绝构造（没有任何 api/images/classifiers 实现）时 panic；
/// 本工厂恒提供 `api`。
///
/// TS: `providers/together.ts`
pub fn together_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "together".into(),
        name: Some("Together".into()),
        base_url: Some("https://api.together.ai/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Together API key", &["TOGETHER_API_KEY"])),
            oauth: None,
        },
        models: catalog_chat_models("together"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("together provider declares an api implementation")
}
