//! providers/xai.ts：`xai_provider` provider 工厂。
//!
//! 这个文件只做组装：xAI 的 chat 走 `openai-responses` 协议，鉴权同时声明 API key 与
//! 订阅制 OAuth。工厂由 [`crate::providers::all::builtin_providers`] 按
//! `spec/ai-core.md` §2.3.1 的固定顺序调用。
//!
//! # 两种鉴权
//!
//! - API key：`XAI_API_KEY`（[`crate::auth::helpers::env_api_key_auth`]）；
//! - OAuth：[`crate::auth::helpers::lazy_oauth`] 包装
//!   [`crate::auth::oauth::load::load_xai_oauth`]，`is_subscription = true`（订阅制），
//!   登录标签 `Sign in with SuperGrok or X Premium`。加载是惰性的：首次用到 OAuth 时才
//!   执行 loader，成功/失败都只执行一次并被记住。
//!
//! 工厂只声明这两种方式，不在构造期读取环境或发起登录；具体使用哪一种由 `Models` 的
//! auth 解析层按凭证/环境决定。
//!
//! # 约束与失败模式
//!
//! - 模型来自 `crates/kiana-ai/data/xai.json`（chat 表）。`openai_responses_api()` 以
//!   `ProviderApiImpl::Single` 形式挂载，所有 chat 模型都由它处理，不按 `model.api`
//!   二次分派；目录里的 `api` 字段仍随模型返回，调用方可用它识别协议。
//! - `base_url` 是 `https://api.x.ai/v1` 的默认值；模型条目自带 `baseUrl`。
//! - `create_provider` 只在没有 `api`/`images`/`classifiers` 时返回 `Err`；本工厂提供
//!   `api`，末尾 `expect` 不会触发。
//!
//! TS: `packages/ai/src/providers/xai.ts`

use std::sync::Arc;

use crate::api::openai_responses::openai_responses_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::helpers::{LazyOAuthInput, lazy_oauth};
use crate::auth::oauth::load::load_xai_oauth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// 构造内置的 xAI provider。
///
/// 返回 `Arc<dyn Provider>`：`id = "xai"`、展示名 `xAI`、默认 base URL
/// `https://api.x.ai/v1`，chat 实现为 `openai-responses`。鉴权同时提供 API key
/// （`XAI_API_KEY`）与惰性 OAuth（订阅制，登录标签 `Sign in with SuperGrok or X Premium`）。
/// 两种方式都在请求前的 auth 解析阶段生效，工厂不读取环境也不登录。
///
/// # Panics
///
/// 仅当 `create_provider` 拒绝构造时 panic；本工厂恒提供 `api`。
///
/// TS: `providers/xai.ts`
pub fn xai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "xai".into(),
        name: Some("xAI".into()),
        base_url: Some("https://api.x.ai/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("xAI API key", &["XAI_API_KEY"])),
            oauth: Some(lazy_oauth(LazyOAuthInput {
                name: "xAI (Grok/X subscription)".into(),
                // 订阅制（SuperGrok / X Premium），供上层（auth 命令、配置）区分登录类型。
                is_subscription: Some(true),
                login_label: Some("Sign in with SuperGrok or X Premium".into()),
                // 惰性加载：首次用到该 OAuth 方式时才执行，结果（含失败）只计算一次。
                load: Arc::new(load_xai_oauth),
            })),
        },
        models: catalog_chat_models("xai"),
        api: Some(openai_responses_api().into()),
        ..Default::default()
    })
    .expect("xai provider declares an api implementation")
}
