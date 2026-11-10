//! providers/ant-ling.ts：`ant_ling_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/ant-ling.ts`
//!
//! # 这个文件负责什么
//!
//! 把 Ant Ling 的 OpenAI 兼容端点登记为一个内置 provider：目录模型来自
//! `providers/data/ant-ling.json`（经 [`catalog_chat_models`] 解析成 `AnyModel`），
//! 所有 chat 请求交给 `openai-completions` 适配器；鉴权是标准的
//! 「存储 key 优先、否则按顺序读环境变量」（[`env_api_key_auth`]，这里只有
//! `ANT_LING_API_KEY` 一个变量）。
//!
//! 工厂本身是纯构造：不发网络请求、不读磁盘（目录数据在编译期已生成），每次调用返回
//! 独立实例。被 [`crate::providers::all::builtin_providers`] 按固定顺序调用。
//!
//! # 注意事项
//!
//! - `base_url` 是厂商默认地址；目录里的模型条目可以各自带 `baseUrl` 覆盖它。
//! - 末尾的 `expect` 分支不可达：`create_provider` 只在 api/images/classifiers 全为空时
//!   返回 `Err`，而这里声明了 `api`；若将来删掉 `api` 字段会立即在注册时 panic。

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// 构造 `ant-ling` provider（展示名 `Ant Ling`，默认端点 `https://api.ant-ling.com/v1`）。
///
/// 目录取 `ant-ling` 的 chat 表；鉴权变量 `ANT_LING_API_KEY`；api 固定为
/// `openai-completions`。返回的 `Arc<dyn Provider>` 可直接交给 `Models::set_provider`。
///
/// TS: `providers/ant-ling.ts`
pub fn ant_ling_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "ant-ling".into(),
        name: Some("Ant Ling".into()),
        base_url: Some("https://api.ant-ling.com/v1".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Ant Ling API key", &["ANT_LING_API_KEY"])),
            oauth: None,
        },
        models: catalog_chat_models("ant-ling"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("ant-ling provider declares an api implementation")
}
