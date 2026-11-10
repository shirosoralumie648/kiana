//! core/tools/ls.ts。
//!
//! `ls` 工具：按大小写不敏感的 `localeCompare` 排序，目录加 `/` 后缀；条目数上限
//! （默认 500）与 50 KiB 字节截断各自产生逐字通知。
//!
//! # 排序
//!
//! TS: `entries.sort((a, b) => a.toLowerCase().localeCompare(b.toLowerCase()))`。
//! Rust 侧用 [`kiana_tui::autocomplete::locale_compare`]（ICU4X，按进程 locale，与 Node 26
//! 的 `Intl.Collator` 对拍过），比较的是 `to_lowercase()` 结果；`sort_by` 是稳定排序，
//! 与 ES2019 起的 `Array.prototype.sort` 一致。已知偏差：Rust 的 `to_lowercase` 不做
//! 词尾 sigma 的上下文替换，非 ASCII 希腊语用例可能不同（单元测试用 ASCII）。
//!
//! 默认操作的 `readdir` 复刻 Node 26（libuv `uv_fs_scandir`）的行为：先按原始字节序
//! 返回条目；`localeCompare` 折叠后相等的条目（如 `Apple` / `apple`）因此保持字节序。
//!
//! # notices 顺序（逐字）
//!
//! `[{limit} entries limit reached. Use limit={2*limit} for more]. [50.0KB limit reached]`
//! ——两条用 `. ` 连接后包在 `[...]` 里；空目录（含 `limit: 0`）直接是 `(empty directory)`。
//!
//! # 执行流程
//!
//! 1. `LsExecute::execute` 先看 `signal` 是否已经取消（是则 `Operation aborted`），
//!    再把 JSON 反序列化为 `LsToolInput`；反序列化失败直接作为工具错误返回。
//! 2. `run` 解析目录：`ctx.cwd()` 优先（占位上下文或空 cwd 时回落到创建工具时的
//!    `cwd`），`path` 缺省为 `"."`，经 [`resolve_to_cwd`] 规范化。
//! 3. 依次检查 [`LsOperations::exists`]（`Ok(false)` → `Path not found`）与
//!    [`LsOperations::stat_is_directory`]（`Ok(false)` → `Not a directory`）；两个方法
//!    返回的 `Err` 则只取 message 作为工具错误。
//! 4. [`LsOperations::readdir`] 取回文件名后做大小写不敏感的稳定排序，再逐条 `stat`
//!    决定是否加 `/`。条目数上限在 `stat` **之前**判断，达到上限后不会再触碰剩余条目。
//! 5. 结果为空 → `(empty directory)`（`limit <= 0` 也走这里）。非空则 `join("\n")`，
//!    用 [`truncate_head`] 按 50 KiB 字节上限截断（行数上限是 `MAX_SAFE_INTEGER`），
//!    最后拼 notices 与 `details`。
//!
//! # 边界与失败模式
//!
//! - `LocalLsOperations::exists` 把任何 `fs` 错误都当作「不存在」，所以权限不足等
//!   错误也表现为 `Path not found`；`stat_is_directory` 的错误不会被这样吞掉，而是
//!   原样成为工具错误。
//! - 单个条目 `stat` 失败时该条目被静默跳过（不进入结果、不占用条目上限）；若所有
//!   条目都失败，输出是 `(empty directory)` 而不是错误。
//! - `limit` 不做 `max(1, …)` 归一化：0/负数直接得到空输出；小数用
//!   `results.len() as f64 >= limit` 比较，`limit: 2.5` 会返回 3 条并触发通知
//!   （`tests/ls.rs` 的 `fractional_limits_use_js_number_semantics`）。
//! - 取消：`execute` 用 `tokio::select!` 在 `run` 与 `token.cancelled()` 之间竞争，
//!   取消时 `run` 的 future 被丢弃并返回 `Operation aborted`；没有 `signal` 时直接
//!   等待 `run`。这与 grep 的处理不同（grep 只在 spawn 子进程之后才观察取消）。
//! - 条目上限与字节上限互相独立：条目先到就截条目，字节先到就截断文本；两者都命中时
//!   通知按 `entries limit reached. 50.0KB limit reached` 的顺序拼接。
//!
//! # 修改影响
//!
//! [`LS_TOOL_DESCRIPTION`]、[`ls_parameters_schema`] 与通知文案都是逐字契约
//! （`tests/ls.rs` 的 `definition_shape_matches_the_reference` /
//! `parameters_schema_is_verbatim` 与 `tests/fixtures` 里的黄金用例），改动会破坏与
//! TS 的输出对齐。`details` 的键序由插入顺序决定（TS 的赋值顺序），不是
//! `LsToolDetails` 的字段序。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use kiana_agent_core::{AgentTool, AgentToolError, AgentToolResult};
use kiana_ai::types::{JsonValue, TextContent, TextKind, UserContentBlock};
pub use kiana_ext_api::tool_types::LsToolDetails;
use kiana_ext_api::tool_types::{
    LsToolInput, ToolDefinition, ToolDefinitionExecute, ToolRenderers,
};
use kiana_ext_api::types::ExtensionToolContextApi;
use tokio_util::sync::CancellationToken;

use crate::definition::wrap_tool_definition;
use crate::path_utils::{path_exists, resolve_to_cwd};
use crate::read::{ToolOperationError, effective_cwd};
use crate::renderers::ls::ls_renderers;
use crate::truncate::{DEFAULT_MAX_BYTES, TruncationOptions, format_size, truncate_head};

/// `lsToolSystemPromptContribution` 的 Rust 形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LsToolSystemPromptContribution {
    /// 系统提示 "Available tools" 段的一行摘要。
    pub snippet: &'static str,
    /// 追加到 Guidelines 段的条目（`ls` 为空数组）。
    pub guidelines: &'static [&'static str],
}

/// `lsToolSystemPromptContribution`。
///
/// 返回值只用于系统提示：`snippet` 进 "Available tools" 段，`guidelines` 是空数组。
/// 注意 [`create_ls_tool_definition`] 把定义上的 `prompt_guidelines` 设为 `None`
/// （TS 里该属性是 `undefined`），这是 TS 自身的不一致，不是遗漏。
pub fn ls_tool_system_prompt_contribution() -> LsToolSystemPromptContribution {
    LsToolSystemPromptContribution {
        snippet: "List directory contents",
        guidelines: &[],
    }
}

/// `ls` 的工具描述（逐字）。
///
/// 这段文本原样进入模型可见的工具定义，`tests/ls.rs` 的
/// `definition_shape_matches_the_reference` 与 `tests/fixtures/tool-definitions.golden.json`
/// 锁定其字节，改动会导致黄金测试失败。
pub const LS_TOOL_DESCRIPTION: &str = "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first).";

/// `DEFAULT_LIMIT`。
///
/// 500 是**条目数**上限（不是字节数）。用 `f64` 是因为 TS 的 `limit` 是 `number`：
/// 小数会原样参与 `>=` 比较，并出现在通知文案与 `details.entryLimitReached` 中。
/// 与 grep 不同，`ls` 不会把 `limit` 夹到 `>= 1`：0 或负数直接得到空输出。
pub const DEFAULT_LIMIT: f64 = 500.0;

/// `Number.MAX_SAFE_INTEGER`（2^53 - 1）；它会原样进 `details.truncation.maxLines`。
///
/// 64 位目标直接用 TS 的数值；32 位目标的 `usize` 放不下 2^53-1，退化为 `usize::MAX`
/// （同样表示“没有行数上限”，且远超任何真实文件行数）。
#[cfg(target_pointer_width = "64")]
const MAX_SAFE_INTEGER: usize = 9_007_199_254_740_991;
/// 32 位目标的“没有行数上限”哨兵（见 [`MAX_SAFE_INTEGER`]）。
#[cfg(not(target_pointer_width = "64"))]
const MAX_SAFE_INTEGER: usize = usize::MAX;

/// 可插拔的目录枚举操作。TS: `LsOperations`。
///
/// 三个方法都用 [`crate::read::ToolOperationError`] 表达失败，但 `run` 的处理并不
/// 相同：`exists` 的 `Ok(false)` 映射为 `Path not found`，`stat_is_directory` 的
/// `Ok(false)` 映射为 `Not a directory`；这三者返回的 `Err` 都只取 message 作为工具
/// 错误，其中 `readdir` 的错误还会加 `Cannot read directory: ` 前缀。
///
/// 自定义实现替换的只是**文件系统访问**；排序、条目上限、目录后缀、截断与 notices
/// 仍由 `run` 统一完成，因此替换实现不会改变输出格式。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性（见 kiana-ai 同类处理）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait LsOperations: Send + Sync {
    /// 路径是否存在（任何错误都算不存在，与 `pathExists` 一致）。
    async fn exists(&self, absolute_path: &Path) -> Result<bool, ToolOperationError>;
    /// 路径是否是目录；文件不存在时返回错误。
    async fn stat_is_directory(&self, absolute_path: &Path) -> Result<bool, ToolOperationError>;
    /// 读取目录条目（未排序）。
    async fn readdir(&self, absolute_path: &Path) -> Result<Vec<String>, ToolOperationError>;
}

/// 默认操作：本地文件系统（`pathExists` / `fs.stat` / `fs.readdir`）。
///
/// `exists` 走 [`path_exists`]（`metadata(...).is_ok()`，任何错误都算不存在）；
/// `stat_is_directory` 的错误会保留 Node 风格的消息与错误码（见
/// [`ToolOperationError::from_io`]）；`readdir` 的排序行为见其实现上的说明。
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalLsOperations;

#[async_trait]
impl LsOperations for LocalLsOperations {
    async fn exists(&self, absolute_path: &Path) -> Result<bool, ToolOperationError> {
        Ok(path_exists(absolute_path).await)
    }

    async fn stat_is_directory(&self, absolute_path: &Path) -> Result<bool, ToolOperationError> {
        tokio::fs::metadata(absolute_path)
            .await
            .map(|metadata| metadata.is_dir())
            .map_err(|error| ToolOperationError::from_io("stat", absolute_path, error))
    }

    async fn readdir(&self, absolute_path: &Path) -> Result<Vec<String>, ToolOperationError> {
        let mut reader = tokio::fs::read_dir(absolute_path)
            .await
            .map_err(|error| ToolOperationError::from_io("scandir", absolute_path, error))?;
        let mut paths = Vec::new();
        loop {
            match reader.next_entry().await {
                Ok(Some(entry)) => paths.push(entry.file_name()),
                Ok(None) => break,
                Err(error) => {
                    return Err(ToolOperationError::from_io("scandir", absolute_path, error));
                }
            }
        }
        // Node 26 的 `fs.readdir` 走 libuv `uv_fs_scandir`，底层用 C `scandir(3)` 的
        // `alphasort`，返回按原始 UTF-8 字节序排列的条目（而不是文件系统哈希序）。
        // 这个顺序决定了下文 `localeCompare` 排序中「折叠后相等」条目的稳定次序
        // （例如 `Apple` 与 `apple`），黄金用例证明了这一点。排序在 lossy 转换前
        // 按原始字节进行，与 C 侧一致。
        paths.sort();
        Ok(paths
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect())
    }
}

/// `ls` 工具的选项。
///
/// `Arc<dyn LsOperations>` 让选项被 clone 后仍共享同一个操作对象；`None` 表示在
/// [`create_ls_tool_definition`] 里换成 `LocalLsOperations`。
#[derive(Clone, Default)]
pub struct LsToolOptions {
    /// 自定义目录枚举操作。默认本地文件系统。
    pub operations: Option<Arc<dyn LsOperations>>,
}

// 手写 Debug：`dyn LsOperations` 本身不实现 Debug，这里只报告是否配置了自定义操作。
impl std::fmt::Debug for LsToolOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LsToolOptions")
            .field("operations", &self.operations.is_some())
            .finish()
    }
}

/// `lsSchema`（TypeBox 序列化结果，逐字节）。
pub fn ls_parameters_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Directory to list (default: current directory)" },
            "limit": { "type": "number", "description": "Maximum number of entries to return (default: 500)" }
        }
    })
}

/// 取消时的工具错误；`message` 必须逐字为 `Operation aborted`（测试与调用方依赖）。
fn aborted_error() -> AgentToolError {
    AgentToolError {
        message: "Operation aborted".to_string(),
        name: None,
        stack: None,
    }
}

/// 把操作层错误降级为工具错误：只保留 `message`，丢弃 Node 错误码 `code`
/// （`ls` 的输出与判断都不使用错误码）。
fn operation_error(error: ToolOperationError) -> AgentToolError {
    AgentToolError {
        message: error.message,
        name: None,
        stack: None,
    }
}

/// JS `String(number)` / `JSON.stringify(number)` 的数值 JSON。
///
/// 实现经由 `kiana_json::js_num::format_js_number`：非有限值（`NaN`/`±Infinity`）
/// 先变成文本 `null`，再解析成 [`JsonValue::Null`]，与 `JSON.stringify` 的语义一致。
fn js_number_value(value: f64) -> JsonValue {
    let text = kiana_json::js_num::format_js_number(value);
    serde_json::from_str(&text).unwrap_or(JsonValue::Null)
}

/// JS `String(number)`。
///
/// 复用 JSON 数字格式化，因此非有限值得到的是 `"null"`，而不是 JS `String()` 的
/// `"NaN"`/`"Infinity"`/`"-Infinity"`——这是一处已知的实现差异。调用点只在「已达到
/// 条目上限」时用它拼通知；对 `NaN` 与 `+Infinity`，`results.len() as f64 >= limit`
/// 永远为假，不会触发该分支。
fn js_number_text(value: f64) -> String {
    kiana_json::js_num::format_js_number(value)
}

/// `ls` 的 `execute` 实现。
struct LsExecute {
    /// 创建工具时绑定的工作目录；占位上下文（`wrap_tool_definition` 无工厂）或
    /// `ctx.cwd()` 为空时作为回退值。
    cwd: String,
    /// 目录枚举实现；构造时已把 `None` 解析为 `LocalLsOperations`，所以这里不是
    /// `Option`。
    operations: Arc<dyn LsOperations>,
}

// `ToolDefinitionExecute::execute` 的通用契约见 kiana-ext-api：`params` 由 agent loop
// 做过 schema 校验，但直接调用 trait 时没有这个保证，所以这里仍把反序列化失败转成
// 工具错误。取消有两条路径：进入时 token 已取消 → 直接 `Operation aborted`；
// 运行中取消 → `select!` 丢弃 `run` 的 future 并返回同样的错误。
impl ToolDefinitionExecute for LsExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        _on_update: Option<kiana_agent_core::AgentToolUpdateCallback>,
        ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> kiana_agent_core::types::BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>>
    {
        Box::pin(async move {
            if signal.is_some_and(CancellationToken::is_cancelled) {
                return Err(aborted_error());
            }
            let input = match serde_json::from_value::<LsToolInput>(params) {
                Ok(input) => input,
                Err(error) => {
                    return Err(AgentToolError {
                        message: error.to_string(),
                        name: None,
                        stack: None,
                    });
                }
            };

            match signal {
                Some(token) => tokio::select! {
                    _ = token.cancelled() => Err(aborted_error()),
                    result = self.run(input, &ctx) => result,
                },
                None => self.run(input, &ctx).await,
            }
        })
    }
}

impl LsExecute {
    /// 执行一次 `ls`：解析目录、枚举、排序、补目录后缀、截断、组装结果。
    ///
    /// 所有错误都是 [`AgentToolError`]（`message` 即模型可见文本）；成功时 `details`
    /// 只在命中条目上限或字节截断时非 `Null`。
    async fn run(
        &self,
        input: LsToolInput,
        ctx: &Arc<dyn ExtensionToolContextApi>,
    ) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
        // 阶段 1：解析目录与上限。`path` 缺省 "."；`limit` 缺省 500，不取整、不夹取。
        let cwd = effective_cwd(ctx, &self.cwd);
        let dir_path: PathBuf = resolve_to_cwd(input.path.as_deref().unwrap_or("."), &cwd);
        let effective_limit = input.limit.unwrap_or(DEFAULT_LIMIT);

        // 阶段 2：存在性与目录类型。只有 `Ok(false)` 会产生下面的固定文案；操作返回的
        // `Err` 走 `operation_error`，保留实现给的 message。
        if !self
            .operations
            .exists(&dir_path)
            .await
            .map_err(operation_error)?
        {
            return Err(AgentToolError {
                message: format!("Path not found: {}", dir_path.display()),
                name: None,
                stack: None,
            });
        }

        if !self
            .operations
            .stat_is_directory(&dir_path)
            .await
            .map_err(operation_error)?
        {
            return Err(AgentToolError {
                message: format!("Not a directory: {}", dir_path.display()),
                name: None,
                stack: None,
            });
        }

        // 阶段 3：枚举文件名。`readdir` 返回的是目录内的名字（不含父路径），其顺序由
        // 实现决定；默认实现已按原始字节排序，这里再用 localeCompare 统一重排。
        let mut entries = match self.operations.readdir(&dir_path).await {
            Ok(entries) => entries,
            Err(error) => {
                return Err(AgentToolError {
                    message: format!("Cannot read directory: {}", error.message),
                    name: None,
                    stack: None,
                });
            }
        };

        // 大小写不敏感的 localeCompare（稳定排序）。折叠后相等的名字（如 `Apple` /
        // `apple`）因此保持 `readdir` 的原始字节序——默认实现的排序就是为此存在。
        entries.sort_by(|left, right| {
            kiana_tui::autocomplete::locale_compare(&left.to_lowercase(), &right.to_lowercase())
        });

        // 阶段 4：按排好序的顺序取前 limit 条并补 "/" 后缀。上限判断在 stat 之前：
        // 达到上限后不再对剩余条目做系统调用。stat 失败的条目被静默跳过，且不占名额。
        let mut results: Vec<String> = Vec::new();
        let mut entry_limit_reached = false;
        for entry in entries {
            if results.len() as f64 >= effective_limit {
                entry_limit_reached = true;
                break;
            }

            let full_path = dir_path.join(&entry);
            let mut suffix = "";
            match self.operations.stat_is_directory(&full_path).await {
                Ok(is_directory) => {
                    if is_directory {
                        suffix = "/";
                    }
                }
                // 无法 stat 的条目直接跳过。
                Err(_) => continue,
            }
            results.push(format!("{entry}{suffix}"));
        }

        // 阶段 5：空结果（空目录、limit <= 0，或所有条目都被 stat 跳过）返回固定文本；
        // 此时 `details` 为 `Null`。
        if results.is_empty() {
            return Ok(AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "(empty directory)".to_string(),
                    text_signature: None,
                })],
                details: JsonValue::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            });
        }

        // 阶段 6：字节截断。每行一个条目，用 "\n" 连接；`truncate_head` 的字节上限取
        // `TruncationOptions` 的默认值（50 KiB），且从不返回半行。
        let raw_output = results.join("\n");
        // 条目数已经单独限制，这里不再有行数上限（`maxLines: Number.MAX_SAFE_INTEGER`）。
        // `maxLines` 会原样进 `details.truncation`，所以必须是 2^53 - 1 而不是 usize::MAX。
        let truncation = truncate_head(
            &raw_output,
            &TruncationOptions::default().with_max_lines(MAX_SAFE_INTEGER),
        );
        let mut output = truncation.content.clone();

        // 阶段 7：组装 notices 与 details。通知按 `. ` 连接后包进 `[...]`，追加在正文
        // 之后、空两行（`\n\n`）。
        // 键顺序固定：entryLimitReached 先插入，truncation 后插入（当前稳定契约，改动需同步测试）。
        let mut details = serde_json::Map::new();
        let mut notices: Vec<String> = Vec::new();
        if entry_limit_reached {
            notices.push(format!(
                "{} entries limit reached. Use limit={} for more",
                js_number_text(effective_limit),
                js_number_text(effective_limit * 2.0)
            ));
            details.insert(
                "entryLimitReached".to_string(),
                js_number_value(effective_limit),
            );
        }
        if truncation.truncated {
            notices.push(format!(
                "{} limit reached",
                format_size(DEFAULT_MAX_BYTES as u64)
            ));
            details.insert(
                "truncation".to_string(),
                serde_json::to_value(&truncation).unwrap_or(JsonValue::Null),
            );
        }
        if !notices.is_empty() {
            output.push_str(&format!("\n\n[{}]", notices.join(". ")));
        }

        let details_value = if details.is_empty() {
            JsonValue::Null
        } else {
            JsonValue::Object(details)
        };
        Ok(AgentToolResult {
            content: vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: output,
                text_signature: None,
            })],
            details: details_value,
            structured_content: None,
            usage: None,
            is_error: None,
            terminate: None,
        })
    }
}

/// 创建 `ls` 的 [`ToolDefinition`]。
///
/// 固定的字段：`name`/`label` 为 `"ls"`，`description` 是 [`LS_TOOL_DESCRIPTION`]，
/// `parameters` 是 [`ls_parameters_schema`]，`prompt_snippet` 取 contribution 的
/// snippet；`prompt_guidelines` 与 `constrained_sampling` 为 `None`，其余可选字段
/// 也保持缺省。渲染器来自 `ls_renderers()`。`execute` 持有 `cwd` 和目录枚举实现
/// （`options.operations` 为 `None` 时用 `LocalLsOperations`）。
///
/// TS: `createLsToolDefinition(cwd, options?)`。注意 TS 的 definition **没有**
/// `constrainedSampling`（与其它内置工具不同）。
pub fn create_ls_tool_definition(
    cwd: impl Into<String>,
    options: Option<LsToolOptions>,
) -> ToolDefinition {
    let options = options.unwrap_or_default();
    let contribution = ls_tool_system_prompt_contribution();
    let renderers: ToolRenderers = ls_renderers();
    ToolDefinition {
        name: "ls".to_string(),
        label: "ls".to_string(),
        description: LS_TOOL_DESCRIPTION.to_string(),
        prompt_snippet: Some(contribution.snippet.to_string()),
        // TS 的 `createLsToolDefinition` 不设置 `promptGuidelines`（contribution 常量里是
        // 空数组，但定义对象上该属性是 `undefined`）。
        prompt_guidelines: None,
        parameters: ls_parameters_schema(),
        constrained_sampling: None,
        render_shell: renderers.render_shell,
        prepare_arguments: None,
        output_schema: None,
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(LsExecute {
            cwd: cwd.into(),
            operations: options
                .operations
                .unwrap_or_else(|| Arc::new(LocalLsOperations)),
        }),
        render_call: renderers.render_call,
        render_result: renderers.render_result,
    }
}

/// 创建 `ls` 的 [`AgentTool`]（`wrapToolDefinition` 投影）。
///
/// 投影只保留 `name`/`label`/`description`/`parameters`/`output_schema`/
/// `constrained_sampling`/`prepare_arguments`/`execution_mode` 与执行器；prompt
/// 元数据和渲染器不会进入 [`AgentTool`]（原因见 `definition.rs` 的模块文档）。
pub fn create_ls_tool(cwd: impl Into<String>, options: Option<LsToolOptions>) -> AgentTool {
    wrap_tool_definition(create_ls_tool_definition(cwd, options), None)
}
