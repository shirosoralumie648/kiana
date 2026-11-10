//! `extensions/index.ts`：`builtInExtensions` 清单（顺序与 `replaceable` 语义固定）。
//!
//! TS: `packages/coding-agent/src/extensions/index.ts`。
//!
//! 清单顺序决定 `/tools` 的展示顺序；`replaceable` 标志决定同名扩展的覆盖关系。两者都必须
//! 与 TS 逐项一致：
//! `llama.cpp` 在前且**不可替换**；`codemode`、`tool-search`、`mcp` 都标记 `replaceable`：
//! 注册了同名工具/命令（例如第三方 MCP 扩展注册 `/mcp`）的扩展会**整体替换**内建项，而不是
//! 与之并行运行。替换语义由 `kiana-resources` 的 loader 实现；本模块只负责名字、顺序与标志位。
//!
//! # 这些条目如何被消费
//!
//! 1. `kiana-coding-agent` 的 `run_main` 先调用 [`builtin_extensions`]，把结果放在用户传入的
//!    `options.extension_factories` 之前（TS: `[...builtInExtensions, ...]`）。
//! 2. `kiana-resources` 的 `DefaultResourceLoader::new` 只把 `builtin: Some(true)` 的
//!    `InlineExtension::Named` 认作内置扩展：从普通工厂列表里取出，按名字存进一张「内置扩展」
//!    表。`InlineExtension::Factory` 或 `builtin` 不为 `true` 的条目会走普通内联扩展路径。
//! 3. 资源解析阶段按 `builtin:<name>` 路径逐个实例化工厂；loader 此时会覆盖
//!    `hidden = Some(true)`，并把 `replaceable` 写进加载出的扩展。
//! 4. 全部扩展加载完后，`omit_replaced_extensions` 丢弃被同名扩展占位的 replaceable 内建项，
//!    并为 `builtin:*` 生成一条 warning。
//!
//! # 标志位语义
//!
//! - `builtin: Some(true)`：参与内置扩展的启用/禁用（settings 与 `--no-mcp` 这类 CLI 开关都
//!   按名字过滤 `builtin:<name>`）。
//! - `replaceable: Some(true)`：另一个**非 replaceable** 扩展注册了同名 tool/command/flag 时
//!   整体丢弃本项。`llama.cpp` 不设该标志，因此总会加载。
//! - `hidden: None`：这里不隐藏；经 `builtin:` 路径加载后由 loader 设为 `true`。
//!
//! # 修改须知
//!
//! 名字与顺序是可观察契约：`tests/builtin_registry.rs` 逐项断言名字、顺序与两个标志位，并
//! 实际调用四个工厂验证它们注册的 tool/command。改名还会让 settings/CLI 里已有的
//! `builtin:<name>` 引用变成 `Unknown built-in extension` 加载错误。

use std::sync::Arc;

use kiana_ext_api::types::{ExtensionFactory, InlineExtension};

use crate::codemode::index::{CodemodeExtensionOptions, create_codemode_extension};
use crate::llama::index::llama_extension;
use crate::mcp::index::{McpExtensionOptions, create_mcp_extension};
use crate::tool_search::index::create_tool_search_extension;

/// `llamaExtension(kiana)` 的工厂包装（TS 的 `llamaExtension` 是同步函数）。
///
/// `ExtensionFactory`（`Arc<dyn Fn(Arc<dyn ExtensionApi>) -> LocalBoxFuture<'static, ()>>`）
/// 必须返回 future，而 `llama_extension` 返回 `()`；因此把同步调用放进 `async move`。
/// 包装本身不做别的工作：首次轮询这个 future 时才执行 `llama_extension`，后者注册 provider
/// 与 `/llama` 命令，失败没有返回通道（签名即 `()`）。
fn llama_factory() -> ExtensionFactory {
    Arc::new(|kiana| {
        Box::pin(async move {
            llama_extension(kiana);
        })
    })
}

/// 四个内置扩展，顺序与 TS 逐项一致。
///
/// 返回长度为 4 的 `Vec<InlineExtension>`，元素全部是 `InlineExtension::Named` 且带
/// `builtin: Some(true)`。每次调用都会新建工厂闭包；`codemode` 与 `mcp` 分别使用
/// `CodemodeExtensionOptions::default()` 与 `McpExtensionOptions::default()`（工厂被调用时
/// 再克隆选项）。
///
/// 当调用方按原样把它传给 `DefaultResourceLoader`（`kiana-coding-agent` 的启动路径如此）时，
/// 返回顺序就是内置扩展表的插入顺序，也就是 package manager 枚举 `builtin:<name>` 资源的
/// 顺序；`replaceable` 参与加载完成后的同名占位判定，不改变工厂的执行顺序。名字必须与 settings
/// 中的 `builtin:<name>` 一致。
pub fn builtin_extensions() -> Vec<InlineExtension> {
    vec![
        InlineExtension::Named {
            name: "llama.cpp".to_string(),
            factory: llama_factory(),
            hidden: None,
            replaceable: None,
            builtin: Some(true),
        },
        InlineExtension::Named {
            name: "codemode".to_string(),
            factory: create_codemode_extension(CodemodeExtensionOptions::default()),
            hidden: None,
            replaceable: Some(true),
            builtin: Some(true),
        },
        InlineExtension::Named {
            name: "tool-search".to_string(),
            factory: create_tool_search_extension(),
            hidden: None,
            replaceable: Some(true),
            builtin: Some(true),
        },
        InlineExtension::Named {
            name: "mcp".to_string(),
            factory: create_mcp_extension(McpExtensionOptions::default()),
            hidden: None,
            replaceable: Some(true),
            builtin: Some(true),
        },
    ]
}
