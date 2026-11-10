//! core/tools/index.ts。
//!
//! 内置工具的统一注册面：工具名、`ToolsOptions`、按名创建定义/工具，以及
//! coding / read-only / all 三组集合。
//!
//! # 已知不一致（照抄 TS）
//!
//! `create_coding_*` / `create_read_only_*` **不含** powershell，而
//! [`create_tool_definition`] / [`create_tool`] / `create_all_*` 含。测试
//! `tool-renderer-examples` 与 `default-tools-setting`（`can select powershell instead of bash`）
//! 依赖此行为。
//!
//! # 未知工具名
//!
//! TS 的 `switch` default 分支 `throw new Error("Unknown tool name: ${toolName}")` 在类型上
//! 不可达；Rust 用 [`ToolNameError`] 把这个运行时错误显式化。
//!
//! # 三组集合
//!
//! - coding：[`create_coding_tool_definitions`] / [`create_coding_tools`] → read、bash、
//!   edit、write。
//! - read-only：[`create_read_only_tool_definitions`] / [`create_read_only_tools`] →
//!   read、grep、find、ls。
//! - all：[`create_all_tool_definitions`] / [`create_all_tools`] → 全部 8 个工具，顺序
//!   read、bash、powershell、edit、write、grep、find、ls。
//!
//! `all` 组用 `IndexMap`（别名 [`ToolDefinitions`] / [`Tools`]）而不是 `HashMap`，因为
//! `Record<ToolName, …>` 的键序在 TS 里可观察：`create_all_*` 的插入顺序与
//! [`ToolName::ALL`] 相同；测试 `tool_sets_and_key_order_match_ts` 用 `keys()` 锁定
//! 该顺序（黄金文件只比较按 `ToolName::ALL` 顺序重建的定义 JSON）。
//!
//! # 选项传递
//!
//! [`ToolsOptions`] 是所有工具选项的并集，按工具名分派：[`create_tool_definition`] /
//! [`create_tool`] 只 `take()` 被选中工具的那一个字段，其余字段随 `options` 一起丢弃。
//! 因此同一份 `ToolsOptions` 可以先后传给不同工具名（测试
//! `options_are_threaded_to_the_matching_tool_only`）。
//!
//! # 调用方与导出面
//!
//! 本文件是 crate 根的再导出面：`lib.rs` 的 `pub use index::*;` 让这里的所有名字出现在
//! `kiana_tools::` 下。`kiana-coding-agent/src/sdk_exports.rs` 以 TS 命名
//! （`createCodingTools` / `createReadOnlyTools` / `createGrepTool` 等）再导出，
//! `kiana-session/src/sdk.rs` 再导出 `create_coding_tools` / `create_read_only_tools`。
//!
//! # 修改影响
//!
//! 调整集合成员或顺序会同时改变 `ToolDefinitions` 的键序、黄金测试的 JSON 输出，以及
//! 依赖这些集合的会话与扩展端；[`ToolName::as_str`] 的返回值是模型可见的工具名，不能改。

use indexmap::IndexMap;

use kiana_agent_core::AgentTool;
use kiana_ext_api::tool_types::ToolDefinition;

// 第 7 周的编辑体验模块。`lib.rs` 是跨周共享文件，本单元不直接修改；这里用
// `#[path]` 让它们挂在注册面上（`lib.rs` 的 `pub use index::*;` 会把模块名一起
// 再导出，因此 `kiana_tools::multi_edit` / `kiana_tools::apply_patch` 也可用）。
#[path = "apply_patch.rs"]
pub mod apply_patch;
#[path = "multi_edit.rs"]
pub mod multi_edit;
#[path = "streaming_parser.rs"]
pub mod streaming_parser;

pub use crate::apply_patch::{
    APPLY_PATCH_PROMPT_GUIDELINES, APPLY_PATCH_PROMPT_SNIPPET, APPLY_PATCH_TOOL_DESCRIPTION,
    APPLY_PATCH_TOOL_NAME, ApplyPatchError, ApplyPatchInput, ApplyPatchMetadata,
    ApplyPatchOperations, ApplyPatchSettings, ApplyPatchToolOptions, LocalApplyPatchOperations,
    apply_patch_parameters_schema, create_apply_patch_tool, create_apply_patch_tool_definition,
    model_matches_pattern, prepare_apply_patch_arguments, preview_patch,
};
pub use crate::multi_edit::{
    AppliedEdit, EditError, EditStatus, EditStrategy, EditStrategyEvent, FileEditOutcome,
    FileEditPlan, FileEditRejection, LineRange, NearestMatch, plan_file_edits,
};

pub use crate::bash::{
    BashOperations, BashRenderState, BashSpawnContext, BashSpawnHook, BashToolDetails,
    BashToolInput, BashToolOptions, BashToolOutput, ShellToolConfig, create_bash_tool,
    create_bash_tool_definition, create_local_bash_operations, create_local_shell_operations,
    create_local_shell_operations_with_sandbox, create_shell_tool_definition,
};
pub use crate::edit::{
    EditOperations, EditToolDetails, EditToolInput, EditToolOptions, create_edit_tool,
    create_edit_tool_definition,
};
pub use crate::file_mutation_queue::with_file_mutation_queue;
// 第 6 周：工具输出预算适配器（artifact 落盘 + middle-out 预览）。
pub use crate::definition::{
    OutputBudgetOptions, wrap_agent_tool_with_output_budget,
    wrap_tool_definition_with_output_budget,
};
pub use crate::find::{
    FindGlobOptions, FindOperations, FindPathModule, FindToolDetails, FindToolInput,
    FindToolOptions, POSIX_PATH, WINDOWS_PATH, create_find_tool, create_find_tool_definition,
    platform_path_module, relativize_find_result_path, relativize_find_result_path_platform,
};
pub use crate::grep::{
    GrepOperations, GrepToolDetails, GrepToolInput, GrepToolOptions, create_grep_tool,
    create_grep_tool_definition,
};
pub use crate::ls::{
    LsOperations, LsToolDetails, LsToolOptions, create_ls_tool, create_ls_tool_definition,
};
pub use crate::powershell::{
    PowerShellOperations, PowerShellSpawnContext, PowerShellSpawnHook, PowerShellToolDetails,
    PowerShellToolInput, PowerShellToolOptions, create_local_power_shell_operations,
    create_power_shell_tool, create_power_shell_tool_definition,
};
pub use crate::read::{
    ReadOperations, ReadToolDetails, ReadToolInput, ReadToolOptions, RestoreOutcome,
    create_read_tool, create_read_tool_definition,
};
pub use crate::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncationOptions, TruncationResult, format_size,
    truncate_head, truncate_line, truncate_tail,
};
pub use crate::write::{
    WriteOperations, WriteToolOptions, create_write_tool, create_write_tool_definition,
};
pub use kiana_ext_api::tool_types::{LsToolInput, WriteToolInput};

/// `Tool = AgentTool<any>`。
pub type Tool = AgentTool;
/// `ToolDef = ToolDefinition<any, any>`。
pub type ToolDef = ToolDefinition;

/// `ToolName = "read" | "bash" | "powershell" | "edit" | "write" | "grep" | "find" | "ls"`。
///
/// 线格式（serde）与 [`ToolName::as_str`] 都是小写形式；`ToolName::from_str` 只做
/// 精确匹配，`"Bash"`、`"read "` 之类会被拒绝并经 [`ToolNameError`] 报出 TS 的
/// `Unknown tool name: ...` 消息。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolName {
    /// `read`。
    Read,
    /// `bash`。
    Bash,
    /// `powershell`。
    PowerShell,
    /// `edit`。
    Edit,
    /// `write`。
    Write,
    /// `grep`。
    Grep,
    /// `find`。
    Find,
    /// `ls`。
    Ls,
}

impl ToolName {
    /// 全部工具名，顺序 `read, bash, powershell, edit, write, grep, find, ls`。
    ///
    /// 顺序即 TS `allToolNames` 的 Set 插入序，也与 `create_all_*` 的插入顺序一致；
    /// 单元测试 `all_tool_names_preserve_ts_order` 逐项断言。
    pub const ALL: [ToolName; 8] = [
        ToolName::Read,
        ToolName::Bash,
        ToolName::PowerShell,
        ToolName::Edit,
        ToolName::Write,
        ToolName::Grep,
        ToolName::Find,
        ToolName::Ls,
    ];

    /// 工具名的线格式字符串。
    ///
    /// 与 `Display` 共用同一份映射；这些字符串是模型可见的工具名（也用于序列化），不能改。
    pub fn as_str(self) -> &'static str {
        match self {
            ToolName::Read => "read",
            ToolName::Bash => "bash",
            ToolName::PowerShell => "powershell",
            ToolName::Edit => "edit",
            ToolName::Write => "write",
            ToolName::Grep => "grep",
            ToolName::Find => "find",
            ToolName::Ls => "ls",
        }
    }
}

impl std::str::FromStr for ToolName {
    type Err = ToolNameError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        // 精确匹配：大小写、首尾空白都不做归一化（测试断言 "Bash"/"read " 会被拒绝）。
        match name {
            "read" => Ok(ToolName::Read),
            "bash" => Ok(ToolName::Bash),
            "powershell" => Ok(ToolName::PowerShell),
            "edit" => Ok(ToolName::Edit),
            "write" => Ok(ToolName::Write),
            "grep" => Ok(ToolName::Grep),
            "find" => Ok(ToolName::Find),
            "ls" => Ok(ToolName::Ls),
            _ => Err(ToolNameError {
                name: name.to_string(),
            }),
        }
    }
}

impl std::fmt::Display for ToolName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `allToolNames`（`Set<ToolName>` 的插入序）。
pub const ALL_TOOL_NAMES: [ToolName; 8] = ToolName::ALL;

/// `Unknown tool name: {name}`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolNameError {
    /// 调用方提供的名字。
    pub name: String,
}

impl std::fmt::Display for ToolNameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Unknown tool name: {}", self.name)
    }
}

impl std::error::Error for ToolNameError {}

/// `ToolsOptions`：每个工具的可选配置（字段顺序即 TS 声明顺序）。
///
/// 这是一个「并集」结构而不是按工具分开的参数：`create_tool_definition` /
/// `create_tool` 只会取走被选中工具对应的字段（`Option::take`），其余字段被丢弃，
/// 所以传一份只填了不相干工具的选项也是合法的（测试
/// `options_are_threaded_to_the_matching_tool_only`）。
#[derive(Clone, Default)]
pub struct ToolsOptions {
    /// `read` 选项。
    pub read: Option<ReadToolOptions>,
    /// `bash` 选项。
    pub bash: Option<BashToolOptions>,
    /// `powershell` 选项。
    pub powershell: Option<PowerShellToolOptions>,
    /// `write` 选项。
    pub write: Option<WriteToolOptions>,
    /// `edit` 选项。
    pub edit: Option<EditToolOptions>,
    /// `grep` 选项。
    pub grep: Option<GrepToolOptions>,
    /// `find` 选项。
    pub find: Option<FindToolOptions>,
    /// `ls` 选项。
    pub ls: Option<LsToolOptions>,
}

// 手写 Debug：只输出每个字段是否配置（不展开内部选项），与各 `*ToolOptions` 的
// 手写 Debug 保持同样粒度。
impl std::fmt::Debug for ToolsOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolsOptions")
            .field("read", &self.read.is_some())
            .field("bash", &self.bash.is_some())
            .field("powershell", &self.powershell.is_some())
            .field("write", &self.write.is_some())
            .field("edit", &self.edit.is_some())
            .field("grep", &self.grep.is_some())
            .field("find", &self.find.is_some())
            .field("ls", &self.ls.is_some())
            .finish()
    }
}

/// `Record<ToolName, ToolDef>`（键序 read/bash/powershell/edit/write/grep/find/ls）。
///
/// 用 `IndexMap` 保留插入顺序：键序在 TS 里可观察（对象键的枚举顺序）。下游
/// （如 kiana-session 按迭代顺序重建注册表）依赖它；黄金测试不读取迭代顺序。
pub type ToolDefinitions = IndexMap<ToolName, ToolDefinition>;
/// `Record<ToolName, Tool>`（键序同上）。
pub type Tools = IndexMap<ToolName, AgentTool>;

/// `createToolDefinition(toolName, cwd, options?)`。
///
/// `tool_name` 必须是 [`ToolName`] 的线格式字符串；未知名字返回 [`ToolNameError`]，
/// 消息为 `Unknown tool name: {name}`（`tests/index.rs` 锁定）。工具构造本身不会失败，
/// 所以这个 `Result` 只表达名字错误；`options` 中只有被选中工具的字段会被消费。
pub fn create_tool_definition(
    tool_name: &str,
    cwd: impl Into<String>,
    options: Option<ToolsOptions>,
) -> Result<ToolDefinition, ToolNameError> {
    let name: ToolName = tool_name
        .parse()
        .map_err(|_: ToolNameError| ToolNameError {
            name: tool_name.to_string(),
        })?;
    let mut options = options.unwrap_or_default();
    Ok(match name {
        ToolName::Read => create_read_tool_definition(cwd, options.read.take()),
        ToolName::Bash => create_bash_tool_definition(cwd, options.bash.take()),
        ToolName::PowerShell => create_power_shell_tool_definition(cwd, options.powershell.take()),
        ToolName::Edit => create_edit_tool_definition(cwd, options.edit.take()),
        ToolName::Write => create_write_tool_definition(cwd, options.write.take()),
        ToolName::Grep => create_grep_tool_definition(cwd, options.grep.take()),
        ToolName::Find => create_find_tool_definition(cwd, options.find.take()),
        ToolName::Ls => create_ls_tool_definition(cwd, options.ls.take()),
    })
}

/// `createTool(toolName, cwd, options?)`。
///
/// 名字校验与选项分派同 [`create_tool_definition`]，但返回 [`AgentTool`]：每个
/// `create_*_tool` 都经 `wrap_tool_definition(..., None)` 投影，prompt 元数据与渲染器
/// 不进入结果（见 `definition.rs` 的模块文档）。
pub fn create_tool(
    tool_name: &str,
    cwd: impl Into<String>,
    options: Option<ToolsOptions>,
) -> Result<AgentTool, ToolNameError> {
    let name: ToolName = tool_name
        .parse()
        .map_err(|_: ToolNameError| ToolNameError {
            name: tool_name.to_string(),
        })?;
    let mut options = options.unwrap_or_default();
    Ok(match name {
        ToolName::Read => create_read_tool(cwd, options.read.take()),
        ToolName::Bash => create_bash_tool(cwd, options.bash.take()),
        ToolName::PowerShell => create_power_shell_tool(cwd, options.powershell.take()),
        ToolName::Edit => create_edit_tool(cwd, options.edit.take()),
        ToolName::Write => create_write_tool(cwd, options.write.take()),
        ToolName::Grep => create_grep_tool(cwd, options.grep.take()),
        ToolName::Find => create_find_tool(cwd, options.find.take()),
        ToolName::Ls => create_ls_tool(cwd, options.ls.take()),
    })
}

/// `createCodingToolDefinitions(cwd, options?)` → `[read, bash, edit, write]`。
///
/// 顺序固定；故意不含 powershell（见模块文档的「已知不一致」）。
pub fn create_coding_tool_definitions(
    cwd: impl Into<String>,
    options: Option<ToolsOptions>,
) -> Vec<ToolDefinition> {
    let cwd = cwd.into();
    let mut options = options.unwrap_or_default();
    vec![
        create_read_tool_definition(cwd.clone(), options.read.take()),
        create_bash_tool_definition(cwd.clone(), options.bash.take()),
        create_edit_tool_definition(cwd.clone(), options.edit.take()),
        create_write_tool_definition(cwd, options.write.take()),
    ]
}

/// `createReadOnlyToolDefinitions(cwd, options?)` → `[read, grep, find, ls]`。
///
/// 顺序固定；故意不含 powershell（见模块文档的「已知不一致」）。
pub fn create_read_only_tool_definitions(
    cwd: impl Into<String>,
    options: Option<ToolsOptions>,
) -> Vec<ToolDefinition> {
    let cwd = cwd.into();
    let mut options = options.unwrap_or_default();
    vec![
        create_read_tool_definition(cwd.clone(), options.read.take()),
        create_grep_tool_definition(cwd.clone(), options.grep.take()),
        create_find_tool_definition(cwd.clone(), options.find.take()),
        create_ls_tool_definition(cwd, options.ls.take()),
    ]
}

/// `createAllToolDefinitions(cwd, options?)`。
///
/// 返回 8 个工具，键序为 read、bash、powershell、edit、write、grep、find、ls
/// （[`ToolName::ALL`]）。黄金测试
/// `all_tool_definitions_match_the_ts_golden_byte_for_byte` 逐字节比较整份定义 JSON。
pub fn create_all_tool_definitions(
    cwd: impl Into<String>,
    options: Option<ToolsOptions>,
) -> ToolDefinitions {
    let cwd = cwd.into();
    let mut options = options.unwrap_or_default();
    let mut definitions = IndexMap::new();
    definitions.insert(
        ToolName::Read,
        create_read_tool_definition(cwd.clone(), options.read.take()),
    );
    definitions.insert(
        ToolName::Bash,
        create_bash_tool_definition(cwd.clone(), options.bash.take()),
    );
    definitions.insert(
        ToolName::PowerShell,
        create_power_shell_tool_definition(cwd.clone(), options.powershell.take()),
    );
    definitions.insert(
        ToolName::Edit,
        create_edit_tool_definition(cwd.clone(), options.edit.take()),
    );
    definitions.insert(
        ToolName::Write,
        create_write_tool_definition(cwd.clone(), options.write.take()),
    );
    definitions.insert(
        ToolName::Grep,
        create_grep_tool_definition(cwd.clone(), options.grep.take()),
    );
    definitions.insert(
        ToolName::Find,
        create_find_tool_definition(cwd.clone(), options.find.take()),
    );
    definitions.insert(
        ToolName::Ls,
        create_ls_tool_definition(cwd, options.ls.take()),
    );
    definitions
}

/// `createCodingTools(cwd, options?)` → `[read, bash, edit, write]`。
///
/// 顺序固定；返回的是 `wrap_tool_definition` 投影后的 [`AgentTool`]（不带 prompt 元数据
/// 与渲染器）。
pub fn create_coding_tools(
    cwd: impl Into<String>,
    options: Option<ToolsOptions>,
) -> Vec<AgentTool> {
    let cwd = cwd.into();
    let mut options = options.unwrap_or_default();
    vec![
        create_read_tool(cwd.clone(), options.read.take()),
        create_bash_tool(cwd.clone(), options.bash.take()),
        create_edit_tool(cwd.clone(), options.edit.take()),
        create_write_tool(cwd, options.write.take()),
    ]
}

/// `createReadOnlyTools(cwd, options?)` → `[read, grep, find, ls]`。
///
/// 顺序固定；返回的是投影后的 [`AgentTool`]。
pub fn create_read_only_tools(
    cwd: impl Into<String>,
    options: Option<ToolsOptions>,
) -> Vec<AgentTool> {
    let cwd = cwd.into();
    let mut options = options.unwrap_or_default();
    vec![
        create_read_tool(cwd.clone(), options.read.take()),
        create_grep_tool(cwd.clone(), options.grep.take()),
        create_find_tool(cwd.clone(), options.find.take()),
        create_ls_tool(cwd, options.ls.take()),
    ]
}

/// `createAllTools(cwd, options?)`。
///
/// 返回 [`Tools`]（`IndexMap<ToolName, AgentTool>`），键序与
/// [`create_all_tool_definitions`] 相同；每个值都是投影后的 [`AgentTool`]。
pub fn create_all_tools(cwd: impl Into<String>, options: Option<ToolsOptions>) -> Tools {
    let cwd = cwd.into();
    let mut options = options.unwrap_or_default();
    let mut tools = IndexMap::new();
    tools.insert(
        ToolName::Read,
        create_read_tool(cwd.clone(), options.read.take()),
    );
    tools.insert(
        ToolName::Bash,
        create_bash_tool(cwd.clone(), options.bash.take()),
    );
    tools.insert(
        ToolName::PowerShell,
        create_power_shell_tool(cwd.clone(), options.powershell.take()),
    );
    tools.insert(
        ToolName::Edit,
        create_edit_tool(cwd.clone(), options.edit.take()),
    );
    tools.insert(
        ToolName::Write,
        create_write_tool(cwd.clone(), options.write.take()),
    );
    tools.insert(
        ToolName::Grep,
        create_grep_tool(cwd.clone(), options.grep.take()),
    );
    tools.insert(
        ToolName::Find,
        create_find_tool(cwd.clone(), options.find.take()),
    );
    tools.insert(ToolName::Ls, create_ls_tool(cwd, options.ls.take()));
    tools
}

/// 当前模型应看到的编辑工具名。
///
/// `apply_patch` 只对 `tools.apply_patch.models` 里配置的模型暴露（`*` 通配、大小写
/// 不敏感）；其余模型继续使用 `edit`。两者**不会同时**出现在同一套工具里，避免模型
/// 在两种编辑格式之间选错（见本周计划的「可能卡住的地方」）。
pub fn edit_tool_name_for_model(
    model: Option<&str>,
    settings: &ApplyPatchSettings,
) -> &'static str {
    match model {
        Some(model) if settings.is_enabled_for(model) => APPLY_PATCH_TOOL_NAME,
        _ => "edit",
    }
}

/// 当前模型是否应使用 apply_patch。
pub fn should_expose_apply_patch(model: Option<&str>, settings: &ApplyPatchSettings) -> bool {
    model.is_some_and(|model| settings.is_enabled_for(model))
}

/// `createCodingToolDefinitions` 的模型路由版本：[read, bash, edit|apply_patch, write]。
///
/// 顺序与 [`create_coding_tool_definitions`] 一致，只在 edit 的位置替换成
/// `apply_patch`。`apply_patch_options` 只在该模型启用 apply_patch 时被消费。
pub fn create_coding_tool_definitions_for_model(
    cwd: impl Into<String>,
    options: Option<ToolsOptions>,
    model: Option<&str>,
    settings: &ApplyPatchSettings,
    apply_patch_options: Option<ApplyPatchToolOptions>,
) -> Vec<ToolDefinition> {
    let cwd = cwd.into();
    let mut options = options.unwrap_or_default();
    let editor = if should_expose_apply_patch(model, settings) {
        create_apply_patch_tool_definition(cwd.clone(), apply_patch_options)
    } else {
        create_edit_tool_definition(cwd.clone(), options.edit.take())
    };
    vec![
        create_read_tool_definition(cwd.clone(), options.read.take()),
        create_bash_tool_definition(cwd.clone(), options.bash.take()),
        editor,
        create_write_tool_definition(cwd, options.write.take()),
    ]
}

/// [`create_coding_tool_definitions_for_model`] 的 [`AgentTool`] 版本。
pub fn create_coding_tools_for_model(
    cwd: impl Into<String>,
    options: Option<ToolsOptions>,
    model: Option<&str>,
    settings: &ApplyPatchSettings,
    apply_patch_options: Option<ApplyPatchToolOptions>,
) -> Vec<AgentTool> {
    create_coding_tool_definitions_for_model(cwd, options, model, settings, apply_patch_options)
        .into_iter()
        .map(|definition| crate::definition::wrap_tool_definition(definition, None))
        .collect()
}

/// `createAllToolDefinitions` 的模型路由版本。
///
/// 键序保持 `read, bash, powershell, edit|apply_patch, write, grep, find, ls`；启用
/// apply_patch 的模型不会同时看到 edit。
pub fn create_all_tool_definitions_for_model(
    cwd: impl Into<String>,
    options: Option<ToolsOptions>,
    model: Option<&str>,
    settings: &ApplyPatchSettings,
    apply_patch_options: Option<ApplyPatchToolOptions>,
) -> ToolDefinitions {
    let cwd = cwd.into();
    let mut options = options.unwrap_or_default();
    let mut definitions = IndexMap::new();
    definitions.insert(
        ToolName::Read,
        create_read_tool_definition(cwd.clone(), options.read.take()),
    );
    definitions.insert(
        ToolName::Bash,
        create_bash_tool_definition(cwd.clone(), options.bash.take()),
    );
    definitions.insert(
        ToolName::PowerShell,
        create_power_shell_tool_definition(cwd.clone(), options.powershell.take()),
    );
    definitions.insert(
        ToolName::Edit,
        if should_expose_apply_patch(model, settings) {
            create_apply_patch_tool_definition(cwd.clone(), apply_patch_options)
        } else {
            create_edit_tool_definition(cwd.clone(), options.edit.take())
        },
    );
    definitions.insert(
        ToolName::Write,
        create_write_tool_definition(cwd.clone(), options.write.take()),
    );
    definitions.insert(
        ToolName::Grep,
        create_grep_tool_definition(cwd.clone(), options.grep.take()),
    );
    definitions.insert(
        ToolName::Find,
        create_find_tool_definition(cwd.clone(), options.find.take()),
    );
    definitions.insert(
        ToolName::Ls,
        create_ls_tool_definition(cwd, options.ls.take()),
    );
    definitions
}

/// [`create_all_tool_definitions_for_model`] 的 [`AgentTool`] 版本。
pub fn create_all_tools_for_model(
    cwd: impl Into<String>,
    options: Option<ToolsOptions>,
    model: Option<&str>,
    settings: &ApplyPatchSettings,
    apply_patch_options: Option<ApplyPatchToolOptions>,
) -> Tools {
    create_all_tool_definitions_for_model(cwd, options, model, settings, apply_patch_options)
        .into_iter()
        .map(|(name, definition)| {
            (
                name,
                crate::definition::wrap_tool_definition(definition, None),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_tool_name_error_matches_ts() {
        let error = create_tool_definition("nope", "/", None).unwrap_err();
        assert_eq!(error.to_string(), "Unknown tool name: nope");
        let error = create_tool("nope", "/", None).unwrap_err();
        assert_eq!(error.to_string(), "Unknown tool name: nope");
    }

    #[test]
    fn all_tool_names_preserve_ts_order() {
        let names: Vec<&str> = ALL_TOOL_NAMES.iter().map(|name| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "read",
                "bash",
                "powershell",
                "edit",
                "write",
                "grep",
                "find",
                "ls"
            ]
        );
    }
}
