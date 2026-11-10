//! `core/extensions/wrapper.ts`。
//!
//! 两个函数把扩展注册的 [`RegisteredTool`] 适配成 agent loop 使用的
//! [`AgentTool`]：只补"执行时的扩展上下文"，工具调用/结果的拦截由 `AgentSession` 通过
//! agent-core 的 hook 完成（TS 注释逐字）。
//!
//! 适配细节全部由 `kiana-tools` 的 [`wrap_tool_definition`] 负责（`ca-tools-compaction-export`
//! 的 `tool-definition-wrapper.ts`）：字段投影（name/label/description/parameters/
//! outputSchema/constrainedSampling/prepareArguments/executionMode）与"每次执行都调用
//! 工厂"的语义都在那里。本模块只负责把 `runner.createToolContext(toolCallId, signal)`
//! 包成工厂。
//!
//! # 每次执行一个新上下文（设计文档 §4.7）
//!
//! TS 的 `wrapToolDefinition` 在 `execute` 内部调用 `ctxFactory?.(toolCallId, signal)`，
//! 因此同一次工具调用的 `ctx.tools()` / `ctx.executeTool()` 与事件处理器看到的是同一套
//! 上下文语义，但**不同调用之间互不共享**。Rust 的 `kiana-tools` 实现保持同一语义：工厂在
//! `AgentToolExecute::execute` 里逐次调用。本模块不能在外层缓存 ctx。
//!
//! # 为什么参数是 `&Arc<ExtensionRunner>`
//!
//! 工厂闭包必须是 `'static + Send + Sync`，而 `ExtensionRunner` 拥有线程本地的扩展表
//! （`Drop` 时注销），不能按值捕获也不能克隆出第二份 runner。`ca-extensions.md` §5.2 的
//! 冻结签名因此是 `&Arc<ExtensionRunner>`；调用方（`kiana-session`）把同一个 runner 的 `Arc`
//! 传进来即可。
//!
//! # 字段投影会丢掉什么
//!
//! `wrap_tool_definition` 只复制 `AgentTool` 拥有的 8 个字段（name / label / description /
//! parameters / constrained_sampling / output_schema / prepare_arguments / execution_mode），
//! 并把 `execute` 换成适配器；`prompt_snippet`、`prompt_guidelines`、`render_shell`、
//! `render_call`、`render_result`、`exposure`、`namespace`、`annotations`、
//! `default_active`、`prepare_loadout`、`reaches_tools` 不会出现在返回的 `AgentTool` 上
//! （`kiana-tools` 的 `wrap_tool_definition` 把这些字段统一置 `None`）。
//! `RegisteredTool.source_info` 也不参与包装——需要扩展来源的调用方必须自己保留原
//! `RegisteredTool`（`tests/wrapper.rs` 的
//! `wrap_registered_tool_projects_every_agent_tool_field` 锁定字段投影）。
//!
//! # 调用方
//!
//! - `kiana-session` 的 `AgentSession` 在刷新工具注册表时用 [`wrap_registered_tools`]
//!   包装 `all_custom_tools`（扩展注册的工具与 SDK 自定义工具）与 `builtin_registered`
//!   （内置工具）；
//! - crate 根经 [`crate::index`] 再导出；`kiana-coding-agent` 的 SDK 面另以
//!   `wrapRegisteredTool` / `wrapRegisteredTools` 的 TS 名字导出。
//!
//! # 错误与取消
//!
//! 包装本身不执行工具，也没有 `Result`：定义、参数与返回值原样透传，错误只可能来自被
//! 包装的 `ToolDefinition::execute`。执行时的取消令牌会同时交给上下文工厂（`cloned()`）
//! 和原 `execute`（原引用），两者看到的是同一个 `tokio_util::sync::CancellationToken`。

use std::sync::Arc;

use kiana_agent_core::AgentTool;
use kiana_ext_api::types::RegisteredTool;
use kiana_tools::definition::wrap_tool_definition;

use crate::runner::ExtensionRunner;

/// 把 `RegisteredTool` 包装成 `AgentTool`（TS: `wrapRegisteredTool`）。
///
/// 使用 runner 的 `createToolContext()`，与事件处理器保持一致的上下文语义。
///
/// # 参数与所有权
///
/// `registered_tool.definition` 会被克隆进返回的 `AgentTool`（`execute`、
/// `prepare_arguments` 等字段是 `Arc`，参数与 schema 则是 JSON 值按值复制）；
/// `source_info` 不参与包装。`runner` 的 `Arc` 由工厂闭包持有，因此返回的工具不借用
/// `registered_tool` 或 `runner`。
///
/// # 执行语义
///
/// 每次调用 `AgentTool::execute` 都会：
///
/// 1. 用本次的 `tool_call_id` 与取消令牌调用
///    [`ExtensionRunner::create_tool_context`]，得到新的
///    `Arc<dyn ExtensionToolContextApi>`（不复用上一次的上下文）；
/// 2. 把参数、取消令牌与 `on_update` 原样转给被包装定义的
///    `ToolDefinitionExecute::execute`；
/// 3. 原样返回其结果，成功与错误都不改写。
///
/// 工具不执行时不会创建上下文，所以包装动作本身没有副作用。
pub fn wrap_registered_tool(
    registered_tool: &RegisteredTool,
    runner: &Arc<ExtensionRunner>,
) -> AgentTool {
    let runner = Arc::clone(runner);
    wrap_tool_definition(
        registered_tool.definition.clone(),
        Some(Arc::new(
            move |tool_call_id: &str, signal: Option<&tokio_util::sync::CancellationToken>| {
                runner.create_tool_context(tool_call_id, signal.cloned())
            },
        )),
    )
}

/// 批量版本（TS: `wrapRegisteredTools`）。
///
/// 返回与输入等长、同序的 `Vec`；空输入得到空 `Vec`。每项独立包装（各自捕获 runner 的
/// 一个 `Arc` 克隆），包装不是可失败操作。
pub fn wrap_registered_tools(
    registered_tools: &[RegisteredTool],
    runner: &Arc<ExtensionRunner>,
) -> Vec<AgentTool> {
    registered_tools
        .iter()
        .map(|tool| wrap_registered_tool(tool, runner))
        .collect()
}
