//! providers/opencode.ts：`opencode_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/opencode.ts`
//!
//! # 这个文件做什么
//!
//! 构造内置 provider `opencode`（展示名 "OpenCode Zen"）。OpenCode Zen 是一个把多家
//! 模型（Anthropic、Google、OpenAI 以及部分开源模型）聚合到同一个 API key 下的网关，
//! 所以它不属于「一个 provider 对应一种协议」的常见形态：chat 请求按模型目录条目里的
//! `api` 字段分派到四个适配器（`anthropic-messages`、`google-generative-ai`、
//! `openai-completions`、`openai-responses`），分类模型则走 `typesafe-system-one`。
//!
//! # 上下游
//!
//! - 调用方：[`crate::providers::all::builtin_providers`]（内置 provider 列表）；
//! - 模型数据：`providers::catalog` 在编译期由 `data/opencode.json` 生成，本文件不写死
//!   任何模型；
//! - 下游：`create_provider` 把实现装进 `ProviderApiImpl::ByApi`；请求时 `Models` 用
//!   `model.api` 精确查表分派。
//!
//! # 关键约束
//!
//! - `ByApi` 的键必须与目录里每条模型的 `api` 逐字相同（区分大小写）。新增或改名一个
//!   api 时这里必须同步改；否则该模型的请求会在流上得到
//!   `no API implementation for "..."` 错误——这是流内的错误事件，不是同步 `Err`。
//! - 四个 chat 适配器都必须经过 `with_open_code_session_header` 包装。OpenCode 依赖
//!   `x-opencode-session` 识别同一会话（会话亲和）；漏包不会报错，只会让行为悄悄退化。
//! - provider 级**不设** `base_url`：目录里每条模型自带 `baseUrl`（OpenCode 的推理端点，
//!   按模型/协议不同），适配器请求时读取的是模型字段而不是 provider 默认值。
//! - 没有 image 实现；classifier 只有 `typesafe-system-one` 一个键。
//! - 鉴权只有 API key（`OPENCODE_API_KEY`），没有 OAuth。

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::anthropic_messages::anthropic_messages_api;
use crate::api::google_generative_ai::google_generative_ai_api;
use crate::api::openai_completions::openai_completions_api;
use crate::api::openai_responses::openai_responses_api;
use crate::api::typesafe_system_one::typesafe_system_one_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::provider::ProviderApiImpl;
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::opencode_headers::with_open_code_session_header;
use crate::providers::{catalog_chat_models, catalog_classifier_models};

/// 构造 OpenCode Zen 的 provider（id `"opencode"`）。
///
/// 模型目录 = chat 模型 + classifier 模型（没有 image 模型）。chat 侧按四个 `api` 值
/// 各注册一个适配器，并统一包上 `x-opencode-session` 头；classifier 侧只注册
/// `typesafe-system-one`。鉴权用 [`crate::auth::helpers::env_api_key_auth`] 读取
/// `OPENCODE_API_KEY`。
///
/// 每次调用都新建一套目录与适配器，实例之间不共享可变状态。
///
/// # Panics
///
/// `create_provider` 只在「api/images/classifiers 三类实现全为空」时返回 `Err`；
/// 这里 `api` 有 4 个条目、classifiers 有 1 个（本 provider 没有 image），所以下面的
/// `expect` 不可达：仅删掉 `api` map 时 classifier 仍非空，不会返回 `Err`。只有同时删掉
/// classifier 注册（三类全空）才会触发 `expect`，这是编程错误而不是运行期输入问题。
///
/// TS: `providers/opencode.ts#opencodeProvider`
pub fn opencode_provider() -> Arc<dyn Provider> {
    let mut models = catalog_chat_models("opencode");
    // classifier 与 chat 是目录里两张独立的表；本 provider 没有 image 表。
    models.extend(catalog_classifier_models("opencode"));
    let mut api = IndexMap::new();
    // 键是模型目录里的 `api` 字段值（不是 URL，也不是展示名）；顺序只影响遍历顺序，
    // 查找是按字符串精确匹配。四个 `*_api()` 返回惰性适配器，构造时不发请求。
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
    let mut classifiers = IndexMap::new();
    // 键同样必须与目录中 `typesafe-system-one` 模型的 `api` 值一致。
    classifiers.insert(
        "typesafe-system-one".to_string(),
        typesafe_system_one_api() as Arc<dyn crate::models::ProviderClassifier>,
    );
    create_provider(CreateProviderOptions {
        id: "opencode".into(),
        name: Some("OpenCode Zen".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("OpenCode API key", &["OPENCODE_API_KEY"])),
            oauth: None,
        },
        models,
        api: Some(ProviderApiImpl::ByApi(api)),
        classifiers: Some(classifiers),
        ..Default::default()
    })
    .expect("opencode provider declares an api implementation")
}
