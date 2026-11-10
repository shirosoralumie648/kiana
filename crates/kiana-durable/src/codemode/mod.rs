//! codemode 目录（按 TS 源 1:1 映射）。
//!
//! # 这个目录负责什么
//! code mode 让模型用一段 JavaScript 调用会话里注册的工具，而不是逐个发起工具调用。
//! 本目录是 `packages/durable/src/codemode/` 的 1:1 移植，四个源文件分工如下：
//! - `codemode.rs`：执行器本身（解析脚本、沙箱、journal、重放、store 与扩展注册）。
//! - `describe.rs`：模型读到的两段文本——工具自己的固定描述，以及系统提示里按命名空间
//!   列出工具声明的 `codemode` 段。
//! - `models.rs`：脚本的 `models.*` 全局函数，以及 `models.classify()`/
//!   `models.generateImages()` 背后的内部工具。
//! - `output.rs`：把脚本结果格式化成模型看到的一条工具结果（头、文本项、console、
//!   图片、截断与溢出文件）。
//!
//! # 调用关系
//! `describe` 与 `output` 在 crate 内部只被 `codemode.rs` 消费；`models` 还被 `describe.rs`
//! 用来拼 `MODELS_DESCRIPTION`（`codemode_description()` 在 `models` 开关打开时插入它）。
//! 工具描述在扩展创建时生成，提示段在每次渲染时按当时的可调用工具生成，模型内部工具在
//! `call_tool()` 里执行，结果格式化在 `complete()` 里完成。这三个模块的类型与函数是
//! `pub` 的，crate 外的测试直接引用 `describe`/`output`（例如
//! `codemode::describe::CodemodeNamespace` 与 `codemode::output::format_result`），没有直接
//! import `models`；因此改 `describe`/`output` 的公开签名会同时影响执行器与测试。
//!
//! # 注意
//! 子模块的说明写在各自文件的 `//!` 里。不要给下面的 `pub mod` 声明新增外层 `///`：
//! 当“父模块里的外层文档”与“文件内的 `//!`”同时存在时，rustdoc 会把合并后的文档放到
//! 父模块作用域解析链接，使子模块 `//!` 里指向本模块条目的短链接全部失效。

// --- 子模块 ---
/// TS 文件名即 `codemode.ts`，模块名与父模块同名是刻意的（保持 1:1 映射）。
#[allow(clippy::module_inception)]
pub mod codemode;
pub mod describe;
pub mod models;
pub mod output;
