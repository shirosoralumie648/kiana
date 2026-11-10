//! providers/qwen-token-plan-individual.ts：`qwen_token_plan_individual_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/qwen-token-plan-individual.ts`
//!
//! # 这个文件做什么
//!
//! 构造内置 provider `qwen-token-plan-individual`：Qwen Token Plan 的**个人版**。
//! 它与国际站 provider 使用同一个端点（`token-plan.ap-southeast-1.maas.aliyuncs.com`）
//! 和同一个环境变量 `QWEN_TOKEN_PLAN_API_KEY`，但 provider id、展示名与模型目录不同
//! （`data/qwen-token-plan-individual.json`，只有 chat 模型）。
//!
//! `spec/ai-core.md`（约 3703-3704 行）说明了两者的目录差异：Individual 只暴露其订阅
//! 文档中的模型，existing（即 `qwen-token-plan`）保留更宽目录以向后兼容；凭证按
//! provider 作用域存储。
//!
//! # 边界与约束
//!
//! - `Single` 适配器，所有模型共用 `openai_completions_api()`；
//! - 没有 image/classifier、没有 OAuth；
//! - provider 级 `base_url` 与模型条目的 `baseUrl` 相同，实际请求使用模型字段。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// 构造 Qwen Token Plan 个人版 provider（id `"qwen-token-plan-individual"`）。
///
/// 目录只有 chat 模型，全部走 `openai-completions`；鉴权读取
/// `QWEN_TOKEN_PLAN_API_KEY`（与国际站共用）。端点、`base_url` 与国际站相同，区别只在
/// id、展示名和目录。
///
/// # Panics
///
/// `create_provider` 只在三类实现全为空时返回 `Err`；这里 `api` 非空，`expect`
/// 不可达。触发即说明注册被改坏。
///
/// TS: `providers/qwen-token-plan-individual.ts`
pub fn qwen_token_plan_individual_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "qwen-token-plan-individual".into(),
        name: Some("Qwen Token Plan Individual".into()),
        base_url: Some(
            "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1".into(),
        ),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth(
                "Qwen Token Plan Individual API key",
                &["QWEN_TOKEN_PLAN_API_KEY"],
            )),
            oauth: None,
        },
        models: catalog_chat_models("qwen-token-plan-individual"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("qwen-token-plan-individual provider declares an api implementation")
}
