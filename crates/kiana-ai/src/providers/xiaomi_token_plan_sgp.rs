//! providers/xiaomi-token-plan-sgp.ts：`xiaomi_token_plan_sgp_provider` provider 工厂。
//!
//! 这个文件只做组装：Xiaomi Token Plan SGP 是带区域后缀（ams/cn/sgp）的三个 Token Plan
//! 端点之一，使用 OpenAI 兼容接口；模型目录来自
//! `crates/kiana-ai/data/xiaomi-token-plan-sgp.json`（chat 表），鉴权为
//! `XIAOMI_TOKEN_PLAN_SGP_API_KEY`。工厂由 [`crate::providers::all::builtin_providers`]
//! 按 `spec/ai-core.md` §2.3.1 的固定顺序调用。
//!
//! # 与同族文件的关系
//!
//! `xiaomi` 与 `xiaomi-token-plan-{ams,cn,sgp}` 四个工厂只有 id、展示名、base URL 和环境
//! 变量不同；改动 API/鉴权行为时要一起检查。`tests/xiaomi_models.rs` 把四个 id 作为一个
//! 集合断言模型目录（例如已弃用模型必须不出现）。
//!
//! # 约束与失败模式
//!
//! - `id` 必须与数据文件名、目录条目的 `provider` 字段一致。
//! - `openai_completions_api()` 以 `ProviderApiImpl::Single` 形式挂载，所有 chat 模型都由
//!   它处理，不按 `model.api` 二次分派；目录里的 `api` 字段仍随模型返回，调用方可用
//!   它识别协议。
//! - `base_url` 是 provider 级默认值；模型条目自带 `baseUrl`。
//! - 目录解析失败会 panic（数据由 `build.rs` 校验）；`create_provider` 的 `expect` 不会
//!   触发（恒有 `api`）。
//!
//! TS: `packages/ai/src/providers/xiaomi-token-plan-sgp.ts`

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// 构造内置的 Xiaomi Token Plan SGP provider。
///
/// 返回 `Arc<dyn Provider>`：`id = "xiaomi-token-plan-sgp"`、展示名
/// `Xiaomi Token Plan SGP`、默认 base URL `https://token-plan-sgp.xiaomimimo.com/v1`，
/// chat 实现为 `openai-completions`，模型目录为该 provider 的全部 chat 模型。鉴权只声明
/// `XIAOMI_TOKEN_PLAN_SGP_API_KEY`；环境变量的读取发生在请求前的 auth 解析阶段。
///
/// # Panics
///
/// 仅当 `create_provider` 拒绝构造时 panic；本工厂恒提供 `api`。
///
/// TS: `providers/xiaomi-token-plan-sgp.ts`
pub fn xiaomi_token_plan_sgp_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "xiaomi-token-plan-sgp".into(),
        name: Some("Xiaomi Token Plan SGP".into()),
        base_url: Some("https://token-plan-sgp.xiaomimimo.com/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth(
                "Xiaomi Token Plan SGP API key",
                &["XIAOMI_TOKEN_PLAN_SGP_API_KEY"],
            )),
            oauth: None,
        },
        models: catalog_chat_models("xiaomi-token-plan-sgp"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("xiaomi-token-plan-sgp provider declares an api implementation")
}
