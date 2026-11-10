//! api 目录：每个 API 实现一个文件。
//!
//! TS: `packages/ai/src/api/*.ts`。映射规则见 `ARCHITECTURE.md` §5：
//! 一个 TS 文件一个 `.rs` 文件；唯一合并是 `cloudflare.rs`（同时承载 `cloudflare.ts` 与
//! `cloudflare-ai-binding.ts`）。15 个 `*.lazy.ts` 的动态 import 包装合并进 [`lazy`]
//! （[`lazy::lazy_api`]），各 API 的 `xxxApi()` 工厂与对应适配器文件一起交付。
//!
//! 共享层（本单元 `api-shared` 负责）：
//! - [`lazy`]：`lazyStream`（同步返回、异步 setup）与 `lazyApi`；
//! - [`simple_options`]：`buildBaseOptions`、`resolveSamplingParams`、思考预算；
//! - [`transform_messages`]：请求前的消息归一化；
//! - [`constrained_sampling`]：strict JSON schema 与 grammar 约束；
//! - [`openai_prompt_cache`]、[`github_copilot_headers`]、[`azure_openai_config`]。
//!
//! 适配器文件由 `ai-api-*` 各单元实现；本 crate 内不得出现第二份实现。
//!
//! ## 适配器入口的同步错误契约（`spec/ai-api-anthropic-openai.md` §1.3 / §8.5）
//!
//! TS 的 `stream` / `streamSimple` 同步返回流，**唯一例外**是缺少鉴权时 `streamSimple`
//! **同步抛出** `No API key for provider: <provider>`（`pre_generation_error.test.ts` 对 7 个
//! 适配器逐一断言）。Rust 没有异常，等价物是适配器模块级入口返回
//! `Result<AssistantMessageEventStream, AiError>`（panic-free 的 `Err`，错误消息是稳定契约），
//! 例如 `api::anthropic_messages::stream_simple(...)`。包装成冻结的
//! [`ProviderStreams`](crate::models::ProviderStreams)（签名同步返回流、不能带 `Err`）时，
//! 适配器把 `Err` 映射成 [`lazy::lazy_stream_error`]（`ProviderStreams` 签名只能返回流，
//! `Err` 必须以 `error` 事件的形式进入流）。
//!
//! 共享层无法替适配器单元实现或断言这一层：`api/mod.rs` 只声明模块，入口函数由各自的
//! 单元交付；`pre_generation_error.rs` 目前锁定的是上面那条映射之后的错误事件语义。
//!
//! # 设计原因
//!
//! 本文件只做模块声明，不承担运行时分派：provider 保存单一实现
//! （`ProviderApiImpl::Single`）或按 `model.api` 的映射（`ProviderApiImpl::ByApi`）；
//! `ByApi` 的映射由 provider 工厂在构造时准备好，请求路径上由 `models` 按 `model.api`
//! 查表（`Single` 不查表），错误处理由 `models` 与 [`lazy`] 完成。
//! 这样保持「一个 TS 文件一个 Rust 文件」的可对照性，不引入注册表或动态插件机制；每个
//! 适配器的协议差异只存在于自己的文件里。
//!
//! # 阅读顺序
//!
//! 1. [`lazy`]：所有适配器共享的「同步返回、异步 setup」骨架。
//! 2. [`classifier_shared`]：分类器共用的 HTTP/重试/错误归一化；读 `llama_cpp_classify`、
//!    `typesafe_system_one`、`openai_decisions` 时从这里开始。
//! 3. 具体适配器：`anthropic_messages`、`openai_responses`、`google_*`、`bedrock_*` 等，
//!    每个文件的 `//!` 说明其协议与差异。
//!
//! # 注意事项
//!
//! - 子模块都是 `pub`，`kiana_ai::api::<module>` 是 crate 外可见路径；重命名/移动属于
//!   公开 API 变更，需同步 `lib.rs` 的文档与下游（映射规则见 `ARCHITECTURE.md` §5）。
//! - 本文件不重导出子模块的符号，调用方一律使用完整路径；新增模块时只加 `pub mod`，
//!   不要在声明之间插入初始化代码或 `use`。
//! - 分类器（[`llama_cpp_classify`]、`openai_decisions`、`typesafe_system_one`，实现
//!   `ProviderClassifier`）与聊天流（`anthropic_messages` 等，实现 `ProviderStreams`）
//!   的错误模型不同：前者把错误折叠进结果对象，后者把错误变成流上的 `error` 事件，
//!   不要混用。

// --- 子模块（顺序与 TS 目录一致）---
pub mod anthropic_messages;
pub mod azure_openai_config;
pub mod azure_openai_responses;
pub mod bedrock_converse_stream;
pub mod classifier_shared;
pub mod cloudflare;
pub mod cloudflare_workers_ai_system_one;
pub mod constrained_sampling;
pub mod github_copilot_headers;
pub mod google_generative_ai;
pub mod google_shared;
pub mod google_vertex;
pub mod kiana_messages;
pub mod lazy;
pub mod llama_cpp_classify;
pub mod mistral_conversations;
pub mod openai_codex_responses;
pub mod openai_completions;
pub mod openai_decisions;
pub mod openai_prompt_cache;
pub mod openai_responses;
pub mod openai_responses_shared;
pub mod openrouter_images;
pub mod simple_options;
pub mod system_one_shared;
pub mod transform_messages;
pub mod typesafe_system_one;
