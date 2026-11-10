//! core/tools/renderers 目录（TS 的 `renderers/index.ts` 落在这里）。
//!
//! 内置工具渲染器的聚合面：只依赖展示层，不加载工具执行路径。
//!
//! TS 的 `createAllToolRenderers(): Record<ToolName, ToolRenderers>` 用
//! [`IndexMap`] 表达（键顺序 read/bash/powershell/edit/write/grep/find/ls）。
//! `withBuiltInRenderers` 只合并三个渲染字段，保留定义自己的 `renderShell`。
//!
//! # 职责边界
//! 渲染层只把「工具调用参数」与「执行结果」翻译成 `Component` 树：不做工具执行、不写文件、
//! 不落盘；唯一的文件读取是 `edit` 预览通过 [`crate::edit_diff::compute_edits_diff`]
//! 只读目标文件算 diff。同一套工厂同时服务实时执行与会话回放，因此渲染器必须只依赖传入的
//! `args` / `result` / `context` 工作；回放时若没有调用记录，由 `render_result` 兜底绘制。
//!
//! # 调用方
//! - `kiana-tools` 内各工具的 `create_*_tool_definition` 直接引用单个工厂（例如 `bash.rs`
//!   用 [`create_shell_renderers`]、`grep.rs` 用 [`grep_renderers`]）。
//! - `kiana-interactive` 的 `get_registered_tool_definition` 用
//!   [`with_built_in_renderers`] 给「只覆盖了部分渲染槽」的工具定义补齐内置槽位。
//!
//! # 注意事项
//! 每个工厂都返回带全新 `Arc` 闭包的集合，没有跨调用共享的可变状态；因此
//! [`create_all_tool_renderers`] 每次调用都会重新构造整张表，调用方拿到的是一份独立快照，
//! 可以安全地按值交给 `ToolExecutionComponent`。

use indexmap::IndexMap;

pub use kiana_ext_api::tool_types::ToolRenderers;

use crate::index::ToolName;

// --- 子模块 ---
pub mod bash;
pub(crate) mod details;
pub mod edit;
pub mod find;
pub mod grep;
pub mod ls;
pub mod read;
pub mod write;

pub use bash::create_shell_renderers;
pub use edit::edit_renderers;
pub use find::find_renderers;
pub use grep::grep_renderers;
pub use ls::ls_renderers;
pub use read::read_renderers;
pub use write::write_renderers;

/// 全部内置工具的渲染器，键顺序 read/bash/powershell/edit/write/grep/find/ls。
///
/// 每次调用都新建 8 个渲染器并写入一张新的 [`IndexMap`]；键顺序与此函数的插入顺序一致，
/// `kiana-tools/tests/tool_renderers.rs` 对其做了逐项断言。
///
/// 每个条目都同时带 `render_call` 与 `render_result`；`render_shell` 只有 `edit` 为
/// `Some(RenderShell::SelfRender)`，其余为 `None`（消费方按默认外壳处理）。
pub fn create_all_tool_renderers() -> IndexMap<ToolName, ToolRenderers> {
    let mut renderers = IndexMap::new();
    renderers.insert(ToolName::Read, read_renderers());
    renderers.insert(ToolName::Bash, create_shell_renderers("$"));
    renderers.insert(ToolName::PowerShell, create_shell_renderers("PS>"));
    renderers.insert(ToolName::Edit, edit_renderers());
    renderers.insert(ToolName::Write, write_renderers());
    renderers.insert(ToolName::Grep, grep_renderers());
    renderers.insert(ToolName::Find, find_renderers());
    renderers.insert(ToolName::Ls, ls_renderers());
    renderers
}

/// 把内置渲染器合并进一个没有提供对应渲染槽的定义。
///
/// TS: `withBuiltInRenderers(toolName, definition)`。`definition == None` 等价 TS 的
/// `undefined`：未知工具名返回 `None`（没有内置可回退）；已知工具名返回内置渲染器。
///
/// 合并只在内置条目存在时发生，逐字段规则：
/// - `render_shell` 恒取自调用方的 `definition`；内置的 `edit` 虽是 self-render，但用户
///   覆盖定义没写 `render_shell` 时合并结果仍是 `None`，不会继承 self-render；
/// - `render_call` / `render_result` 用 `Option::or`：调用方提供的槽优先，缺省槽才由
///   内置补齐。
///
/// 未知工具名时原样返回调用方的定义（哪怕只填了部分槽），不做任何内置回退。
///
/// 查找路径会调用 [`create_all_tool_renderers`] 重建整张表再按键取走；内置渲染器本身
/// 无状态，重建不影响正确性。
pub fn with_built_in_renderers(
    tool_name: &str,
    definition: Option<ToolRenderers>,
) -> Option<ToolRenderers> {
    let built_in = tool_name
        .parse::<ToolName>()
        .ok()
        .and_then(|name| create_all_tool_renderers().shift_remove(&name));
    match (definition, built_in) {
        (None, built_in) => built_in,
        (definition @ Some(_), None) => definition,
        (Some(definition), Some(built_in)) => Some(ToolRenderers {
            render_shell: definition.render_shell,
            render_call: definition.render_call.or(built_in.render_call),
            render_result: definition.render_result.or(built_in.render_result),
        }),
    }
}
