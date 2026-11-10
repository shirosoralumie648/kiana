//! `index.ts` 再导出面（`@earendil-works/kiana-ai` 的根入口）。
//!
//! TS 源：`packages/ai/src/index.ts`（50 行）。本模块只做 `pub use`，**没有任何运行时
//! 副作用**：不构造生成目录、不注册 provider 工厂、不初始化 api-registry、不装配 OAuth、
//! 不引入 compat（与 TS 注释「Core only, side-effect free」一致）。
//!
//! 分节顺序与 TS 源码行号顺序一致，便于逐行核对：
//!
//! 1. `typebox` 的 `Static`/`TSchema`/`Type`——Rust 侧没有 TypeBox；JSON Schema 由
//!    [`crate::utils::typebox_helpers`] 与验证模块承担，因此这三项没有对应符号（见
//!    `ARCHITECTURE.md` §8）。
//! 2. `api/lazy.ts` 的全部导出。
//! 3. 各 `api/*.ts` 的 `*Options` 类型（type-only；Rust 侧是普通结构体）。
//! 4. `auth/*`、`models.ts`、`models-store.ts`、`providers/faux.ts`、
//!    `session-resources.ts`、`types.ts` 与 `utils/*` 的 `export *`。
//! 5. 选出的具名导出（`contentText`、`getSystemMessageText`、
//!    `renderSystemMessageUpdate`、`uuidv7`、`KIANA_AI_VERSION`）。
//! 6. 旧扩展 OAuth 类型（`compat/extension-oauth-types.ts` 的 6 个类型；**不含**
//!    `OAuthCredentials`，它经 `auth/types.ts` 的 `export *` 提供）。
//!
//! 命名差异：TS 的 camelCase 在这里是 snake_case（Rust 命名约定），例如
//! `lazyStream` → [`lazy_stream`]、`uuidv7` 保持不变（本来就是小写+数字）。
//!
//! # 阅读与维护提示
//!
//! 本文件不定义任何运行时行为，只是名字绑定：想知道某个符号是否属于“根入口”，查这里
//! 即可；真正的模块地图与依赖关系在 `lib.rs` 的 crate 文档里。`pub use` 在编译期解析，
//! **不会**触发目标模块里的 `LazyLock`/`OnceLock` 初始化，也不会执行注册副作用——副作用
//! 仍只发生在首次访问对应静态量时（见各模块文档）。
//!
//! 新增导出前先确认 TS `index.ts` 是否公开同一符号；Rust 的公开面比 TS 大或小都会造成
//! 契约漂移。`//` 分节注释的编号与顺序对应 TS 源文件的导出行，仅用于核对，不是行为约定。

// ---------------------------------------------------------------------------
// 1. typebox（无 Rust 等价物，见模块文档）
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 2. ./api/lazy.ts
// ---------------------------------------------------------------------------
pub use crate::api::lazy::{
    LazyApiCapabilities, LazyApiLoad, LazyEventCallback, LazyResultCallback, LazySetup,
    LazyStreamObserver, create_setup_error_message, lazy_api, lazy_stream, lazy_stream_error,
};

// ---------------------------------------------------------------------------
// 3. api/* 的 *Options 类型（type-only in TS）
// ---------------------------------------------------------------------------
pub use crate::api::anthropic_messages::{
    AnthropicEffort, AnthropicOptions, AnthropicThinkingDisplay,
};
pub use crate::api::azure_openai_responses::AzureOpenAIResponsesOptions;
pub use crate::api::bedrock_converse_stream::{BedrockOptions, BedrockThinkingDisplay};
pub use crate::api::google_generative_ai::GoogleOptions;
pub use crate::api::google_shared::{GoogleApiThinkingLevel, ResolvedGoogleThinkingLevel};
pub use crate::api::google_vertex::GoogleVertexOptions;
pub use crate::api::mistral_conversations::MistralOptions;
pub use crate::api::openai_codex_responses::{
    OpenAICodexResponsesOptions, OpenAICodexWebSocketDebugStats,
};
pub use crate::api::openai_completions::OpenAICompletionsOptions;
pub use crate::api::openai_responses::OpenAIResponsesOptions;
// `KianaMessagesEvent` 是 TS 的 SSE 事件联合类型；Rust 的 `api::kiana_messages` 以
// [`serde_json::Value`] 承载原始事件（解析函数见该模块），没有同名的具体结构体。
pub use crate::api::kiana_messages::{KianaMessagesOptions, KianaMessagesRewriteImpact};

// ---------------------------------------------------------------------------
// 4. export * 的模块（顺序与 index.ts 相同）
// ---------------------------------------------------------------------------
pub use crate::auth::context::*;
pub use crate::auth::credential_store::*;
pub use crate::auth::helpers::*;
pub use crate::auth::types::*;
pub use crate::models::store::*;
pub use crate::models::*;
pub use crate::providers::faux::*;
pub use crate::session_resources::*;
pub use crate::types::*;
pub use crate::utils::diagnostics::*;
pub use crate::utils::event_stream::*;
pub use crate::utils::identity::*;
pub use crate::utils::json_parse::*;
pub use crate::utils::overflow::*;
pub use crate::utils::retry::*;
// TS 的 `streaming-json.ts` 只导出 `JsonPath` / `JsonChange` / `StreamingJsonParser` /
// `ToolCallArgumentsStream`；Rust 模块另有内部辅助（`parse_tool_call_arguments` 与
// `json_parse.ts` 的同名函数冲突），因此这里逐项列出 TS 的那 4 个符号。
pub use crate::utils::streaming_json::{
    JsonChange, JsonPath, StreamingJsonParser, ToolCallArgumentsStream,
};
pub use crate::utils::transcript::*;
pub use crate::utils::typebox_helpers::*;
pub use crate::utils::validation::*;

// ---------------------------------------------------------------------------
// 5. 选出的具名导出
// ---------------------------------------------------------------------------
// TS 的 `contentText(content, separator)` 是单个重载函数（按内容联合分派）。Rust 侧
// `utils::text` 按内容类型拆成 5 个函数（[`content_text_from_blocks`] 等），这里全部再导出；
// `getSystemMessageText` / `renderSystemMessageUpdate` 同名对应。
pub use crate::utils::text::{
    content_text_assistant, content_text_from_blocks, content_text_system, content_text_user,
    content_text_user_content, get_system_message_text, render_system_message_update,
};
pub use crate::utils::uuid::uuidv7;
pub use crate::utils::version::KIANA_AI_VERSION;

// ---------------------------------------------------------------------------
// 6. 旧扩展 OAuth 类型（type-only）
// ---------------------------------------------------------------------------
pub use crate::compat::extension_oauth_types::{
    OAuthAuthInfo, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt, OAuthSelectOption,
    OAuthSelectPrompt,
};

// `crate::error` 的 `AiError`/`AiErrorKind`/`ModelsError`/`ModelsErrorCode` 由 `models::*`
// 再导出（`models.ts` 的 `export *` 面包含它们）；`event_stream.rs` 的流类型同时由
// `utils/event_stream.ts` 的 `export *` 与 crate 根提供。TS 的 `index.ts` 没有单独的
// `error.ts` / `event-stream.ts`，这里不额外添加 TS 不存在的条目。
