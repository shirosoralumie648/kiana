//! providers/opencode-go.ts：`opencode_go_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/opencode-go.ts`
//!
//! # 这个文件做什么
//!
//! 构造内置 provider `opencode-go`（展示名 "OpenCode Go"）。它是 OpenCode 的另一个
//! 网关端点：模型目录与 OpenCode Zen 分开（`data/opencode-go.json`），但同样是聚合型
//! 网关，chat 请求按模型条目的 `api` 字段在四个协议适配器之间分派。
//!
//! # 与 `opencode.rs` 的差别
//!
//! - 没有 classifier 表（本 provider 只导出 chat 模型），也没有 image；
//! - 目录键、id、展示名不同；
//! - 约束相同：`ByApi` 的键必须与目录里的 `api` 逐字一致，四个适配器都要经过
//!   `with_open_code_session_header` 包装，否则会话路由头会缺失；
//! - 同样不设 provider 级 `base_url`，请求地址来自模型条目的 `baseUrl`。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]。

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::anthropic_messages::anthropic_messages_api;
use crate::api::google_generative_ai::google_generative_ai_api;
use crate::api::openai_completions::openai_completions_api;
use crate::api::openai_responses::openai_responses_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::provider::ProviderApiImpl;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;
use crate::providers::opencode_headers::with_open_code_session_header;

/// 构造 OpenCode Go 的 provider（id `"opencode-go"`）。
///
/// 目录只有 chat 模型；四个 `api` 值各对应一个包了 `x-opencode-session` 的适配器，
/// 鉴权读取 `OPENCODE_API_KEY`（与 `opencode` 共用同一个环境变量，但 provider id 与
/// 目录不同）。
///
/// # Panics
///
/// 与 `opencode_provider` 相同：`create_provider` 只有在三类实现全为空时才返回
/// `Err`，这里 `api` 非空，`expect` 不可达；触发即表示实现注册被改坏。
///
/// TS: `providers/opencode-go.ts#opencodeGoProvider`
pub fn opencode_go_provider() -> Arc<dyn Provider> {
    let mut api = IndexMap::new();
    // 键是目录里的 `api` 值；`*_api()` 只是惰性适配器，构造本身不建立连接。
    api.insert(
        "anthropic-messages".to_string(),
        with_open_code_session_header(anthropic_messages_api()),
    );
    api.insert(
        "google-generative-ai".to_string(),
        with_open_code_session_header(google_generative_ai_api()),
    );
    api.insert(
        "openai-completions".to_string(),
        with_open_code_session_header(openai_completions_api()),
    );
    api.insert(
        "openai-responses".to_string(),
        with_open_code_session_header(openai_responses_api()),
    );
    create_provider(CreateProviderOptions {
        id: "opencode-go".into(),
        name: Some("OpenCode Go".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("OpenCode API key", &["OPENCODE_API_KEY"])),
            oauth: None,
        },
        models: catalog_chat_models("opencode-go"),
        api: Some(ProviderApiImpl::ByApi(api)),
        ..Default::default()
    })
    .expect("opencode-go provider declares an api implementation")
}
