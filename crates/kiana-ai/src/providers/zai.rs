//! providers/zai.ts：`zai_provider` provider 工厂。
//!
//! 这个文件只做组装：Z.AI 的 coding 端点使用 OpenAI 兼容接口（base URL 含
//! `/api/coding/paas/v4`，即 coding 端点路径），模型目录来自
//! `crates/kiana-ai/data/zai.json`（chat 表），鉴权为 `ZAI_API_KEY`。工厂由
//! [`crate::providers::all::builtin_providers`] 按 `spec/ai-core.md` §2.3.1 的固定顺序调用。
//!
//! # 与 zai-coding-cn 的关系
//!
//! `zai-coding-cn` 是同一路径结构的 `-cn` 变体（host `open.bigmodel.cn`）；两个文件只有
//! id、展示名、base URL 与环境变量不同。改 chat 实现或目录来源时要一起检查。
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
//! TS: `packages/ai/src/providers/zai.ts`

use std::sync::Arc;

use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;

/// 构造内置的 Z.AI provider。
///
/// 返回 `Arc<dyn Provider>`：`id = "zai"`、展示名 `Z.AI`、默认 base URL
/// `https://api.z.ai/api/coding/paas/v4`，chat 实现为 `openai-completions`，模型目录为
/// `zai` 的全部 chat 模型。鉴权只声明 `ZAI_API_KEY`；环境变量的读取发生在请求前的
/// auth 解析阶段。
///
/// # Panics
///
/// 仅当 `create_provider` 拒绝构造时 panic；本工厂恒提供 `api`。
///
/// TS: `providers/zai.ts`
pub fn zai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "zai".into(),
        name: Some("Z.AI".into()),
        base_url: Some("https://api.z.ai/api/coding/paas/v4".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Z.AI API key", &["ZAI_API_KEY"])),
            oauth: None,
        },
        models: catalog_chat_models("zai"),
        api: Some(openai_completions_api().into()),
        ..Default::default()
    })
    .expect("zai provider declares an api implementation")
}
