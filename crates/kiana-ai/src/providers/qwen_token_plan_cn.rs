//! providers/qwen-token-plan-cn.ts：`qwen_token_plan_cn_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/qwen-token-plan-cn.ts`
//!
//! # 这个文件做什么
//!
//! 构造内置 provider `qwen-token-plan-cn`：Qwen Token Plan 的**中国站**端点
//! （`token-plan.cn-beijing.maas.aliyuncs.com`），OpenAI 兼容协议。目录来自
//! `data/qwen-token-plan-cn.json`，只有 chat 模型；没有 image/classifier、没有 OAuth。
//!
//! # 边界与约束
//!
//! - `Single` 适配器，所有模型共用 `openai_completions_api()`；
//! - 鉴权只有 API key：`QWEN_TOKEN_PLAN_CN_API_KEY`。注意与另外两个 Qwen Token Plan
//!   provider 不同——中国站用独立的环境变量，国际站与个人版共用
//!   `QWEN_TOKEN_PLAN_API_KEY`；
//! - provider 级 `base_url` 与目录里模型条目的 `baseUrl` 相同，实际请求使用模型字段。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// 构造 Qwen Token Plan 中国站 provider（id `"qwen-token-plan-cn"`）。
///
/// 目录只有 chat 模型，全部走 `openai-completions`；鉴权读取
/// `QWEN_TOKEN_PLAN_CN_API_KEY`（与另外两个 Qwen Token Plan provider 的变量不同）。
///
/// # Panics
///
/// `create_provider` 只在三类实现全为空时返回 `Err`；这里 `api` 非空，`expect`
/// 不可达。触发即说明注册被改坏。
///
/// TS: `providers/qwen-token-plan-cn.ts`
pub fn qwen_token_plan_cn_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "qwen-token-plan-cn".into(),
        name: Some("Qwen Token Plan CN".into()),
        base_url: Some("https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth(
                "Qwen Token Plan CN API key",
                &["QWEN_TOKEN_PLAN_CN_API_KEY"],
            )),
            oauth: None,
        },
        models: catalog_chat_models("qwen-token-plan-cn"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("qwen-token-plan-cn provider declares an api implementation")
}
