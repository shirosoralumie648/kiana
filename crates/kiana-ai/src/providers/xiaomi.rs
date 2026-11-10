//! providers/xiaomi.ts：`xiaomi_provider` provider 工厂。
//!
//! 这个文件只做组装：Xiaomi 的 MiMo 平台使用 OpenAI 兼容接口，模型目录来自
//! `crates/kiana-ai/data/xiaomi.json`（chat 表），鉴权为 `XIAOMI_API_KEY`。工厂由
//! [`crate::providers::all::builtin_providers`] 按 `spec/ai-core.md` §2.3.1 的固定顺序调用。
//!
//! # 与同族文件的关系
//!
//! `xiaomi-token-plan-{ams,cn,sgp}` 三个工厂与这里只有 id、展示名、base URL 和环境变量
//! 不同，chat 实现与目录机制完全相同；改动 API/鉴权行为时要一起检查这四个文件，以及
//! `crates/kiana-ai/data/` 下各自的数据文件（`tests/xiaomi_models.rs` 会遍历四个 id）。
//!
//! # 约束与失败模式
//!
//! - `id` 必须与数据文件名、目录条目的 `provider` 字段一致。
//! - `openai_completions_api()` 以 `ProviderApiImpl::Single` 形式挂载，所有 chat 模型都由
//!   它处理，不按 `model.api` 二次分派；目录里的 `api` 字段仍随模型返回，调用方可用
//!   它识别协议。
//! - `base_url` 是 provider 级默认值；模型条目自带 `baseUrl`。
//! - 目录解析失败会 panic（数据由 `build.rs` 校验，属于构建期错误）。
//! - `create_provider` 的 `expect` 不会触发：本工厂恒提供 `api`。
//!
//! TS: `packages/ai/src/providers/xiaomi.ts`

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// 构造内置的 Xiaomi provider。
///
/// 返回 `Arc<dyn Provider>`：`id = "xiaomi"`、展示名 `Xiaomi`、默认 base URL
/// `https://api.xiaomimimo.com/v1`，chat 实现为 `openai-completions`，模型目录为
/// `xiaomi` 的全部 chat 模型。鉴权只声明 `XIAOMI_API_KEY`；环境变量的读取发生在请求前
/// 的 auth 解析阶段。
///
/// # Panics
///
/// 仅当 `create_provider` 拒绝构造时 panic；本工厂恒提供 `api`。
///
/// TS: `providers/xiaomi.ts`
pub fn xiaomi_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "xiaomi".into(),
        name: Some("Xiaomi".into()),
        base_url: Some("https://api.xiaomimimo.com/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Xiaomi API key", &["XIAOMI_API_KEY"])),
            oauth: None,
        },
        models: catalog_chat_models("xiaomi"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("xiaomi provider declares an api implementation")
}
