//! providers/cerebras.ts：`cerebras_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/cerebras.ts`
//!
//! # 这个文件负责什么
//!
//! 把 Cerebras 的 OpenAI 兼容端点登记为内置 provider：目录来自
//! `providers/data/cerebras.json`（经 [`catalog_chat_models`] 解析），chat 请求走
//! `openai-completions` 适配器，鉴权按「存储 key → `CEREBRAS_API_KEY`」解析
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

/// 构造 `cerebras` provider（展示名 `Cerebras`，默认端点 `https://api.cerebras.ai/v1`）。
///
/// 目录取 `cerebras` 的 chat 表；鉴权变量 `CEREBRAS_API_KEY`；api 固定为
/// `openai-completions`。
///
/// TS: `providers/cerebras.ts`
pub fn cerebras_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "cerebras".into(),
        name: Some("Cerebras".into()),
        base_url: Some("https://api.cerebras.ai/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Cerebras API key", &["CEREBRAS_API_KEY"])),
            oauth: None,
        },
        models: catalog_chat_models("cerebras"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("cerebras provider declares an api implementation")
}
