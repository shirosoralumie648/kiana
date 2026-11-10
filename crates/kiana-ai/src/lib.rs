//! 对应 packages/ai（含 ai-auth 与全部 api/provider 实现）。
//!
//! TS 源：`packages/ai/src/**`。模块树与 TS 文件一一对应，见 `ARCHITECTURE.md` §5.1。
//!
//! 本 crate 是**单一 crate**：`api/*` 与 `models.ts` 之间存在真实环（`api/lazy.ts` 引用全部
//! api 模块，api 模块又引用 `models.ts` 的 `calculateCost`/`clampThinkingLevel`），
//! 拆成多个 crate 会强制引入 trait 转发层。详见 `ARCHITECTURE.md` §3.1 的 SCC 分析。
//!
//! # 这个 crate 解决什么问题
//!
//! Kiana 的 **AI 客户端层**：把「给某家大模型服务商（provider）发请求、收流式响应、登录
//! 鉴权、模型目录管理、按 token 计费」这些与具体服务商打交道的工作，统一封装成一个库。
//! 上层（agent、会话、配置、扩展等）只依赖本 crate 暴露的少量契约（消息/事件类型、错误、
//! provider 工厂、`fetch` 抽象），不必关心每家服务商各自的 HTTP 细节。
//!
//! 通俗地说：如果把 Kiana 比作一个会写代码的助手，那么本 crate 就是它「给各家 AI 服务商
//! 打电话」的电话本和话务规则——知道每家该怎么拨号（请求/响应格式）、怎么验明身份
//! （OAuth / API key）、怎么计价（cost）、以及该拨哪个号码（模型目录）。
//!
//! # 在系统中的位置
//!
//! 上游（本 crate 依赖的仓库内 crate）：
//!
//! - `kiana-json`：提供 JS JSON 语义的等价实现（数字最短往返、对象键序、错误文本逐字
//!   对齐）；孤立代理项与非有限数是有记录的被迫偏差（见 `kiana_json::js_parse` 模块文档）。
//!   本 crate 的 JSON 修复/严格解析（`utils/json_parse.rs`）与数字文本格式化
//!   （`kiana_json::js_num`）基于它；流式 JSON（`utils/streaming_json.rs`）是独立状态机，
//!   只沿用相同的数值/代理项约定。
//! - `kiana-telemetry`：提供 span/metrics 的适配器契约，本 crate 用它埋点。
//!
//! 上游外部依赖（节选，完整见 `Cargo.toml`）：`reqwest`（HTTP）、`tokio-tungstenite`
//! （Codex 的 WebSocket 传输）、`zstd`（Codex 请求体压缩）、`hmac`+`crc32fast`（Bedrock
//! 手写 SigV4 签名与 event-stream 校验，不引入 AWS SDK）。
//!
//! 下游（依赖本 crate 的共 18 个 crate）：`kiana-agent-core`、`kiana-core`、
//! `kiana-session-core`、`kiana-config`、`kiana-durable`、`kiana-extensions`、
//! `kiana-interactive`、`kiana-coding-agent`、`kiana-tools` 等。几乎所有需要「调用模型」的
//! 上层都经由本 crate，因此本 crate 的公开签名属于关键契约，改动前必须确认下游编译与行为。
//!
//! # 核心概念与类型
//!
//! - 错误：`AiError` / `AiErrorKind`（crate 统一错误），`ModelsError` / `ModelsErrorCode`
//!   是带 code 的结构化错误（`model_source`/`model_validation`/`provider`/`stream`/`auth`/`oauth`）。
//!   `provider`/`stream`/`auth`/`oauth` 四种 code 在仓库内实际构造；`model_source` /
//!   `model_validation` 是为与 TS 的六个字面量对齐而保留的变体，当前只出现在 code 映射与
//!   序列化测试（`tests/models_error.rs`）中。模型目录与模型存储的错误分别走
//!   `model_catalog::CatalogError` 与 `AiError`，不经过 `ModelsError`。
//! - 流：`AssistantMessageEventStream` 与 `create_assistant_message_event_stream`，把服务商
//!   的原始流式响应（SSE、Codex 的 WebSocket、Bedrock 的二进制 event-stream）统一成
//!   Kiana 内部事件流。`StreamPushError` 是 `push` 返回的错误类型，但当前实现不会产生它：
//!   `end()` 之后 `push` 静默返回 `Ok(())`，事件队列无上限；`Closed`/`Full`/`Ended`
//!   三个变体都只为按名字匹配保留（见 `event_stream` 模块文档）。
//! - 契约类型：[`types`] 模块承载跨 crate 共享的消息/模型/流事件形状。
//! - `fetch` 注入点：[`http`] 模块定义 `HttpClient` trait（TS `FetchFunction` 的等价物），
//!   默认实现是 `http::reqwest_client`（按 `fetch`/undici 语义解压响应体，而不是 reqwest 内建解压）。
//!
//! # 入口：库与可执行文件
//!
//! 本 crate 同时产出库与一个可执行文件（`Cargo.toml` 的 `[[bin]]`）：
//!
//! - **库入口就是本文件**。下面那组 `pub mod` 声明是模块树的唯一清单。上层取符号有两条
//!   路径：crate 根的直接再导出（`pub use types::*` 等，下游因此能写
//!   `kiana_ai::Message`、`kiana_ai::AiError`），以及 [`index`] 模块（逐行对应 TS 的
//!   `src/index.ts`，用于核对 TS 的导出面）。两条路径覆盖的符号不完全相同（以 TS 导出面
//!   为准的是 [`index`]），但同名符号指向同一份定义；[`index`] 自身只做 `pub use`、
//!   不定义新类型。
//! - **可执行文件 `kiana-ai`**（`src/bin/kiana-ai.rs`）是 OAuth 登录开发 CLI 的薄壳，
//!   只调用 [`cli::main`]；命令、逐字输出与退出码契约都在 [`cli`]。它不参与库的调用链，
//!   排查模型请求时不需要读它。
//!
//! # 一次典型调用
//!
//! 1. 上层调用 `models::create_models` 得到 `Arc<dyn MutableModels>`，再用 `set_provider`
//!    注册 provider。
//! 2. 调用 `Models::stream`，同步返回 `AssistantMessageEventStream`。返回前 `lazy_stream`
//!    会同步 poll setup 一次（复刻 TS `setup()` 的同步前缀）：`require_chat_provider`
//!    与 `apply_auth` 的前缀先执行。`apply_auth` 内部的鉴权解析被 spawn 成独立任务
//!    （`auth::resolve::resolve_provider_auth`），可能在本调用返回前就开始 OAuth 刷新
//!    等网络请求，因此不要依赖“返回时尚未鉴权、未发请求”。
//! 3. setup 的剩余部分、`provider.stream` 与事件转发在后台任务中完成；setup 失败
//!    （未知 provider、非 chat 模型、鉴权失败）会变成流上的终态 error 事件，而不是 `Err`。
//! 4. 服务商事件被转成 `AssistantMessageEvent` 写入 `AssistantMessageEventStream`；`result()`
//!    等待流结束并取得最终的 `AssistantMessage`（运行期错误以终态 error 事件/消息体现，
//!    而不是 `Err`）。
//!
//! # 模块地图（建议阅读顺序）
//!
//! 1. [`types`] 与 [`error`] —— 先读：所有模块共享的类型与错误形状。
//! 2. [`api`] 是各家服务商的请求/响应适配器（体量最大的一块）；[`providers`] 是 42 个
//!    builtin provider 工厂与目录级辅助模块。
//! 3. [`auth`] —— OAuth 与 API key 鉴权（`auth::oauth::*` 是各家的 OAuth 流程）。
//! 4. [`models`] 与 [`model_catalog`] —— 模型目录、计费、provider 注册。
//! 5. [`http`] —— `fetch` 抽象与默认 reqwest 实现。
//! 6. [`utils`] —— 流式 JSON 解析、重试、校验等基础设施。
//! 7. [`index`] —— 对外再导出面（副作用为零的 `pub use` 集合，对应 TS 的根入口）。
//! 8. 其余兼容/辅助模块：[`bedrock_provider`]、[`bun_oauth`]、[`legacy_api_aliases`] 与
//!    `auth::oauth_compat` 是 TS 入口的占位或再导出；[`cli`] 是 `kiana-ai` 二进制；
//!    [`compat`] 是旧全局 API 的兼容层（迁移完成后删除）；[`env_api_keys`]、
//!    [`image_models`]、[`session_resources`]、[`telemetry`] 各自见其文件开头的 `//!`。
//!
//! # 边界与失败模式
//!
//! - 本 crate 只管「一次与某家服务商的交互」；会话的持久化、多轮上下文管理不在这里。
//! - 错误按层表示：`HttpClient` 层的失败是 `http::HttpError`；适配器把需要暴露的失败转成
//!   `AiError`，或以流上的 error 事件承载（如 `api::kiana_messages` 的
//!   `KianaMessagesResponseError` 经内部 `StreamFailure` 变成终态 error 事件）。`Models::stream`
//!   的 setup 错误（未知 provider、非 chat 模型、鉴权失败）一律成为终态 error 事件，不返回
//!   `Err`；`generate_images`/`classify` 把错误折叠进返回值。适配器模块级入口里，`stream`
//!   直接返回流（错误只走 error 事件），`stream_simple` 保留 `Result` 用于同步校验失败
//!   （如 `api::anthropic_messages::stream_simple` 缺鉴权时返回 `Err`；`api::kiana_messages`
//!   的实现当前永远返回 `Ok`）。
//! - 重试判定/退避原语集中在 `utils::{retry, provider_retry}`；但部分适配器保留自己的重试
//!   循环（`api/google_shared.rs`、`api/openai_codex_responses.rs`、
//!   `auth/oauth/github_copilot.rs`），`api/kiana_messages.rs` 明确不重试。
//! - 真实网络、实时 SSE 等「在线」行为大多只由测试里的 mock 与固定矢量（fixture）覆盖，
//!   live 路径需要网络与凭据。
//!
//! # 与实现相关的编译期约定
//!
//! 本 crate 的 `build.rs` 会按 `.manifest.json` 校验模型目录（sha256、structureHash），
//! `Date.parse` 的无时区时间按构建机本地时区解释（与 Node 行为一致）。
//!
//! # 修改本文件时的约束
//!
//! - 新增顶层源文件必须在这里补 `pub mod`（嵌套模块由父模块的 `mod.rs` 声明），否则文件
//!   不参与编译；是否对外可见也由这里的 `pub` 决定。
//! - 给模块写的文档请放进模块文件开头的 `//!`。本文件部分 `pub mod` 前有历史遗留的外层
//!   `///`（`error`、`event_stream`、`http`、`types` 等）；新增模块不要再复制这种做法：
//!   外层 `///` 与模块内 `//!` 会被 rustdoc 合并到父作用域解析链接，短链接会失效
//!   （见 `docs/COMMENT_STYLE.md` §4.2）。
//! - `pub use types::*;` 是 glob：`types` 里新增任何 `pub` 类型都会自动成为 crate 根的
//!   公开 API。改 `types` 的公开面等于改本 crate 的根 API，要同步核对 [`index`] 与下游。

/// crate 级错误类型。
pub mod error;
/// `utils/event-stream.ts` 的等价实现。
pub mod event_stream;
/// provider `fetch` 注入点（TS 的 `FetchFunction`）；默认实现见 `http::reqwest_client`。
pub mod http;
/// `types.ts`：跨 crate 共享的消息/模型/流事件契约。
pub mod types;

pub mod api;
pub mod auth;
/// `oauth.ts`（`@earendil-works/kiana-ai/oauth`）的仅类型 barrel（等同于 TS 的 `./oauth` 入口）。
pub use auth::oauth_compat;
pub mod bedrock_provider;
/// `bun-oauth.ts`：静态 OAuth 流登记的显式初始化点。
pub mod bun_oauth;
pub mod cli;
pub mod compat;
pub mod env_api_keys;
pub mod image_models;
pub mod index;
/// `legacy-api-aliases.ts`：16 个 `@deprecated` 的旧全局流函数别名（实现由 compat-entrypoints 单元负责）。
pub mod legacy_api_aliases;
pub mod model_catalog;
pub mod models;
pub mod providers;
pub mod request_policy;
/// `session-resources.ts`：会话资源清理注册表。
pub mod session_resources;
pub mod telemetry;
pub mod utils;

// 根级短路径再导出：与 `index` 模块（TS 根入口的逐项镜像）指向同一批定义，只是路径更短。
// `types::*` 是 glob：`types` 新增的公开类型会自动出现在 crate 根（见上方“修改本文件时的约束”）。
pub use error::{AiError, AiErrorKind, ModelsError, ModelsErrorCode};
pub use event_stream::{
    AssistantMessageEventStream, StreamPushError, create_assistant_message_event_stream,
};
pub use types::*;
pub use utils::streaming_json::{
    ToolCallArgumentsFinish, ToolCallArgumentsReplace, ToolCallArgumentsStream,
};
pub use utils::version::KIANA_AI_VERSION;
