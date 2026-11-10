//! providers/baseten.ts：`baseten_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/baseten.ts`
//!
//! # 这个文件负责什么
//!
//! 把 Baseten 的 OpenAI 兼容推理端点登记为内置 provider：目录来自
//! `providers/data/baseten.json`（经 [`catalog_chat_models`] 解析），chat 请求走
//! `openai-completions` 适配器，鉴权按「存储 key → `BASETEN_API_KEY`」解析
//! （[`env_api_key_auth`]）。
//!
//! 工厂只在构造期工作（解析目录、组装 provider），不发网络请求；每次调用返回独立实例，
//! 由 [`crate::providers::all::builtin_providers`] 按固定顺序调用。
//!
//! # 注意事项
//!
//! - `base_url` 是厂商默认值，模型条目可自带 `baseUrl` 覆盖。
//! - `expect` 分支不可达：provider 声明了 `api`，`create_provider` 不会返回 `Err`。

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// 构造 `baseten` provider（展示名 `Baseten`，默认端点 `https://inference.baseten.co/v1`）。
///
/// 目录取 `baseten` 的 chat 表；鉴权变量 `BASETEN_API_KEY`；api 固定为
/// `openai-completions`。
///
/// TS: `providers/baseten.ts`
pub fn baseten_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "baseten".into(),
        name: Some("Baseten".into()),
        base_url: Some("https://inference.baseten.co/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Baseten API key", &["BASETEN_API_KEY"])),
            oauth: None,
        },
        models: catalog_chat_models("baseten"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("baseten provider declares an api implementation")
}
