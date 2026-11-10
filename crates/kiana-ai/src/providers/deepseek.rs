//! providers/deepseek.ts：`deepseek_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/deepseek.ts`
//!
//! 工厂只做参数组装：provider id `deepseek`、base URL `https://api.deepseek.com`、环境变量
//! 鉴权 `DEEPSEEK_API_KEY`、chat 实现 `openai-completions`，模型目录来自
//! `providers/data/deepseek.json`（由 `build.rs` 编成静态表）。DeepSeek 没有 OAuth 分支。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。`create_provider` 的唯一失败条件
//! （没有任何 api/images/classifiers 实现）在本工厂不成立，因此末尾用 `expect` 断言。

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// TS: `providers/deepseek.ts`
///
/// 构造并返回 `deepseek` provider。每次调用都会重新解析模型目录并新建鉴权对象；
/// 不发起网络请求，也不注册到任何全局表（注册由 `builtin_providers`/`Models` 负责）。
pub fn deepseek_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "deepseek".into(),
        name: Some("DeepSeek".into()),
        base_url: Some("https://api.deepseek.com".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("DeepSeek API key", &["DEEPSEEK_API_KEY"])),
            oauth: None,
        },
        models: catalog_chat_models("deepseek"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("deepseek provider declares an api implementation")
}
