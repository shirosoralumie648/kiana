//! providers/nvidia.ts：`nvidia_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/nvidia.ts`
//!
//! 固定参数：id `nvidia`、base URL `https://integrate.api.nvidia.com/v1`、环境变量鉴权
//! `NVIDIA_API_KEY`、chat 实现 `openai-completions`（NVIDIA 的 OpenAI 兼容端点）。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/nvidia.ts`
///
/// 构造并返回 `nvidia` provider；只解析目录与组装参数，不发起网络请求。
pub fn nvidia_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "nvidia".into(),
        name: Some("NVIDIA".into()),
        base_url: Some("https://integrate.api.nvidia.com/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("NVIDIA API key", &["NVIDIA_API_KEY"])),
            oauth: None,
        },
        models: catalog_chat_models("nvidia"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("nvidia provider declares an api implementation")
}
