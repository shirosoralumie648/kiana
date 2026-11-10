//! providers/qwen-token-plan.ts：`qwen_token_plan_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/qwen-token-plan.ts`
//!
//! # 这个文件做什么
//!
//! 构造内置 provider `qwen-token-plan`：阿里云 Model Studio 的 Qwen「Token Plan」
//! 国际站端点（`token-plan.ap-southeast-1.maas.aliyuncs.com`），协议为 OpenAI 兼容
//! （`openai-completions`）。目录来自 `data/qwen-token-plan.json`，只有 chat 模型；
//! 没有 image/classifier，也没有 OAuth。
//!
//! # 边界与约束
//!
//! - 使用 `ProviderApiImpl::Single`：所有模型共用 `openai_completions_api()`，不按
//!   `model.api` 查表（因此目录里出现新 `api` 值也不会影响分派）；
//! - provider 级 `base_url` 是默认地址；目录里的模型条目也自带相同的 `baseUrl`，
//!   适配器发请求时读取的是模型字段；
//! - 鉴权只有 API key：`QWEN_TOKEN_PLAN_API_KEY`（解析顺序见
//!   `auth::helpers::env_api_key_auth`）。
//!
//! 同家族的另外两个 provider 见 `qwen_token_plan_cn.rs`（中国站）与
//! `qwen_token_plan_individual.rs`（个人版）。三者代码几乎相同，差异只在 id、展示名、
//! 端点、环境变量与目录键——与 TS 的分片文件一一对应。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// 构造 Qwen Token Plan（国际站）provider（id `"qwen-token-plan"`）。
///
/// 目录只有 chat 模型，全部走 `openai-completions`；鉴权读取
/// `QWEN_TOKEN_PLAN_API_KEY`。`base_url` 与模型目录里的 `baseUrl` 指向同一端点，
/// 两处需保持一致。
///
/// # Panics
///
/// `create_provider` 只在三类实现全为空时返回 `Err`；这里 `api` 非空，`expect`
/// 不可达。触发即说明注册被改坏。
///
/// TS: `providers/qwen-token-plan.ts`
pub fn qwen_token_plan_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "qwen-token-plan".into(),
        name: Some("Qwen Token Plan".into()),
        base_url: Some(
            "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1".into(),
        ),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth(
                "Qwen Token Plan API key",
                &["QWEN_TOKEN_PLAN_API_KEY"],
            )),
            oauth: None,
        },
        models: catalog_chat_models("qwen-token-plan"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("qwen-token-plan provider declares an api implementation")
}
