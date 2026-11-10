//! core/tools/grep.ts。
//!
//! `grep` 工具：调用外部 `ripgrep`（通过 `kiana_core::tools_manager::ensure_tool` 解析，必要时
//! 下载），解析 `--json` 事件流，按 match/context 格式化，并做匹配数、字节、单行长度三层
//! 截断与 notices。
//!
//! 为什么必须 spawn `rg`：`.gitignore` 语义、`--json` 事件形状、`--max-results`、
//! 隐藏文件规则都由 ripgrep 决定（spec §8.2 #12）。`ignore`/`globset` crate 不得用于本工具。
//!
//! # 执行流程
//!
//! 1. `GrepExecute::execute` 检查 `signal` 是否已取消（是则 `Operation aborted`），
//!    然后把 `params` 反序列化为 [`GrepToolInput`]；反序列化失败直接作为工具错误返回。
//! 2. `run` 先调用 `ensure_tool_without_status("rg")` 解析可执行文件（本地没有时会尝试
//!    下载）。返回 `None` → `ripgrep (rg) is not available and could not be downloaded`。
//! 3. 解析搜索根：`ctx.cwd()` 优先（占位上下文或空 cwd 时回落到创建工具时的 `cwd`），
//!    `path` 缺省 `"."`，经 `resolve_to_cwd` 规范化；再用 [`GrepOperations::is_directory`]
//!    判断它是目录还是单文件。该调用的**任何**错误都会变成 `Path not found: {path}`。
//! 4. 拼 argv：固定 `--no-config --json --line-number --color=never --hidden`，按需追加
//!    `--ignore-case`、`--fixed-strings`、`--glob <glob>`，最后是 `--`、pattern、搜索
//!    路径。`--` 保证以 `-` 开头的 pattern 不会被 ripgrep 当成选项。
//! 5. spawn 之后由独立任务读空 stderr（避免子进程因 stderr 管道写满而阻塞），主循环用
//!    `LossyLines`（Node readline 语义）逐行读 stdout；只有 `type == "match"` 的事件
//!    计入匹配数，达到 `effective_limit` 时 kill 子进程并停止读取。
//! 6. 等待子进程退出：被 limit 杀掉时忽略退出码；否则只接受 0（有匹配）与 1（无匹配），
//!    其余把 JS trim 后的 stderr（非空时）或 `ripgrep exited with code {n}` 作为错误。
//! 7. 匹配数为 0 → `No matches found`（成功结果）。否则格式化每条匹配，按 50 KiB 字节
//!    截断，最后拼 notices 与 `details`。
//!
//! # 输出契约
//!
//! - 匹配行为 `路径:行号: 文本`，上下文行为 `路径-行号- 文本`。目录搜索时路径是相对
//!   搜索根的 posix 路径（相对结果为空或以 `..` 开头时退回 basename）；搜索单个文件时
//!   只给 basename。
//! - `context` 为 0（缺省）且事件自带 `lines.text` 时走快路径，不读文件；否则读文件
//!   取上下文行（同一路径只读一次）。每行按 UTF-16 code unit 截到 500 字符。
//! - notices 顺序固定为 `matchLimitReached` → 50 KiB 截断 → `linesTruncated`，用 `. `
//!   连接后包在 `[...]` 内追加到正文末尾；`details` 的键序与之一致，因此这里手工构造
//!   `serde_json::Map`，而不是序列化 `GrepToolDetails`（后者的字段序不同）。
//!
//! # 取消语义（与 TS 对齐的回归点）
//!
//! 取消检查只有两处：进入 `execute` 时（已取消直接返回 `Operation aborted`）和 spawn
//! **之后**。TS 只在 spawn 后注册 abort 监听器，而已经 aborted 的 signal 不会再触发
//! 监听器，所以 `ensureTool`/`isDirectory` 期间到达的取消被忽略，rg 会继续跑完并正常
//! 返回匹配结果。本文件用 `observe_abort` 复刻这一点，`tests/grep.rs` 的
//! `abort_before_the_child_is_spawned_is_ignored_like_ts` 锁定该行为。
//!
//! # 注意事项
//!
//! - [`GREP_TOOL_DESCRIPTION`]、schema、通知文案与 `details` 键序都是逐字契约
//!   （`tests/grep.rs` 与 `tests/fixtures` 的黄金文件）。
//! - 搜索语义（`.gitignore`、隐藏文件、`--glob`）必须继续由 ripgrep 决定；不要用
//!   `ignore`/`globset` 重写这部分逻辑。

use std::collections::HashMap;
use std::path::{Component as PathComponent, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use kiana_agent_core::{AgentTool, AgentToolError, AgentToolResult};
use kiana_ai::types::{JsonValue, TextContent, TextKind, UserContentBlock};
use kiana_core::child_process::{SpawnOptions, StdioMode, spawn_process};
use kiana_core::tools_manager::ensure_tool_without_status;
pub use kiana_ext_api::tool_types::{GrepToolDetails, GrepToolInput};
use kiana_ext_api::tool_types::{ToolDefinition, ToolDefinitionExecute, ToolRenderers};
use kiana_ext_api::types::ExtensionToolContextApi;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::definition::wrap_tool_definition;
use crate::path_utils::resolve_to_cwd;
use crate::read::ToolOperationError;
use crate::renderers::grep::grep_renderers;
use crate::truncate::{
    DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH, TruncationOptions, format_size, truncate_head,
    truncate_line,
};

/// `DEFAULT_LIMIT = 100`。
///
/// 表示「最多返回多少条匹配」，用 `f64` 保留 TS `number` 语义：小数不取整，会原样
/// 出现在 `effective_limit` 的比较、通知文案与 `details.matchLimitReached` 中。
pub const DEFAULT_LIMIT: f64 = 100.0;

/// `grepToolSystemPromptContribution` 的 Rust 形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrepToolSystemPromptContribution {
    /// 系统提示 "Available tools" 段的一行摘要。
    pub snippet: &'static str,
    /// 追加到 Guidelines 段的条目。
    pub guidelines: &'static [&'static str],
}

/// `grepToolSystemPromptContribution`。
///
/// 返回值只用于系统提示：`snippet` 进 "Available tools" 段，`guidelines` 是空数组。
/// [`create_grep_tool_definition`] 会把定义上的 `prompt_guidelines` 设为 `None`
/// （TS 里该属性是 `undefined`），两者在 TS 里本来就不一致。
pub fn grep_tool_system_prompt_contribution() -> GrepToolSystemPromptContribution {
    GrepToolSystemPromptContribution {
        snippet: "Search file contents for patterns (respects .gitignore)",
        guidelines: &[],
    }
}

/// `grep` 的工具描述（逐字）。
///
/// 这段文本原样进入模型可见的工具定义，`tests/fixtures/tool-definitions.golden.json`
/// 锁定其字节，改动会导致黄金测试失败。
pub const GREP_TOOL_DESCRIPTION: &str = "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars.";

/// `grepSchema`（TypeBox 序列化结果，逐字节）。
///
/// `pattern` 是唯一必填字段；其余字段的 description 会原样展示给模型，属于提示契约。
/// `context`/`limit` 声明为 `number`，小数与负数不在 schema 层拒绝，而是在
/// `effective_context` / `effective_limit` 里按 TS 语义归一化。
pub fn grep_parameters_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "required": ["pattern"],
        "properties": {
            "pattern": { "type": "string", "description": "Search pattern (regex or literal string)" },
            "path": { "type": "string", "description": "Directory or file to search (default: current directory)" },
            "glob": { "type": "string", "description": "Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'" },
            "ignoreCase": { "type": "boolean", "description": "Case-insensitive search (default: false)" },
            "literal": { "type": "boolean", "description": "Treat pattern as literal string instead of regex (default: false)" },
            "context": { "type": "number", "description": "Number of lines to show before and after each match (default: 0)" },
            "limit": { "type": "number", "description": "Maximum number of matches to return (default: 100)" }
        }
    })
}

/// 可插拔的 grep 操作（例如把搜索委托给远端系统）。
///
/// TS: `GrepOperations`。`is_directory` 在路径不存在时返回错误；`read_file` 用于读取
/// context 行，失败时按「无法读取文件」处理。
///
/// `is_directory` 的任何错误在 `run` 里都会变成 `Path not found: ...`（不只是
/// ENOENT），自定义实现要注意错误分类。`read_file` 只在需要 context 行或事件缺少
/// `lines.text` 时调用；失败结果会被按路径缓存成空内容，并在输出里留一行
/// `路径:行号: (unable to read file)`。搜索本身仍由 ripgrep 完成，这个 trait 只能替换
/// 目录判定与文件读取，不能替换搜索算法。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait GrepOperations: Send + Sync {
    /// 路径是否是目录（不存在时错误）。
    async fn is_directory(&self, absolute_path: &Path) -> Result<bool, ToolOperationError>;
    /// 读取文件内容（UTF-8 文本）。
    async fn read_file(&self, absolute_path: &Path) -> Result<String, ToolOperationError>;
}

/// 默认操作：本地文件系统 + ripgrep。
///
/// `is_directory` 用 `fs::metadata`，错误带 Node 风格消息（`stat 'path'`）；`read_file`
/// 复刻 `fs.readFile(p, "utf-8")` 的解码行为（见方法上的注释）。
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalGrepOperations;

#[async_trait]
impl GrepOperations for LocalGrepOperations {
    async fn is_directory(&self, absolute_path: &Path) -> Result<bool, ToolOperationError> {
        match tokio::fs::metadata(absolute_path).await {
            Ok(metadata) => Ok(metadata.is_dir()),
            Err(error) => Err(ToolOperationError::from_io("stat", absolute_path, error)),
        }
    }

    async fn read_file(&self, absolute_path: &Path) -> Result<String, ToolOperationError> {
        let bytes = tokio::fs::read(absolute_path)
            .await
            .map_err(|error| ToolOperationError::from_read_file_io(absolute_path, error))?;
        // `fs.readFile(p, "utf-8")`：WHATWG UTF-8（非法序列替换为 U+FFFD），不剥 BOM。
        Ok(encoding_rs::UTF_8
            .decode_without_bom_handling(&bytes)
            .0
            .into_owned())
    }
}

/// `GrepToolOptions`。
///
/// `Arc<dyn GrepOperations>` 让选项被 clone 后仍共享同一个操作对象；`None` 表示在
/// `run` 里临时换成 `LocalGrepOperations`（`ls` 是在构造时解析，这里是惰性解析）。
#[derive(Clone, Default)]
pub struct GrepToolOptions {
    /// 自定义搜索操作。默认本地文件系统 + ripgrep。
    pub operations: Option<Arc<dyn GrepOperations>>,
}

// 手写 Debug：`dyn GrepOperations` 本身不实现 Debug，这里只报告是否配置了自定义操作。
impl std::fmt::Debug for GrepToolOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrepToolOptions")
            .field("operations", &self.operations.is_some())
            .finish()
    }
}

/// JS `Math.max(1, limit ?? 100)`（NaN 保持 NaN，与 JS 一致）。
///
/// 结果不取整：`limit: 2.5` 仍按 2.5 参与 `match_count >= effective_limit`，因此最多
/// 返回 3 条匹配。NaN 原样返回；任何 `>= NaN` 比较都为假，所以实际「永不达到上限」。
fn effective_limit(limit: Option<f64>) -> f64 {
    match limit {
        None => DEFAULT_LIMIT,
        Some(limit) if limit.is_nan() => f64::NAN,
        Some(limit) => limit.max(1.0),
    }
}

/// `context && context > 0 ? context : 0`。
///
/// 0、负数与 NaN 都归一为 0（走不读文件的快路径）；正的小数原样保留，因为 TS 直接拿
/// 它当循环变量，会产生小数行号与空文本（见 [`format_block`]）。
fn effective_context(context: Option<f64>) -> f64 {
    match context {
        Some(context) if context > 0.0 => context,
        _ => 0.0,
    }
}

/// `path.relative(from, to)`（两者都按已解析的绝对路径处理）。
fn js_relative(from: &Path, to: &Path) -> PathBuf {
    let from_components: Vec<PathComponent<'_>> = from.components().collect();
    let to_components: Vec<PathComponent<'_>> = to.components().collect();
    let mut common = 0usize;
    while common < from_components.len()
        && common < to_components.len()
        && from_components[common] == to_components[common]
    {
        common += 1;
    }
    let mut result = PathBuf::new();
    for _ in common..from_components.len() {
        result.push("..");
    }
    for component in &to_components[common..] {
        result.push(component.as_os_str());
    }
    result
}

/// `path.basename(filePath)`。
fn js_basename(file_path: &Path) -> String {
    file_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `formatPath(filePath)`：目录搜索时给出相对路径（posix 分隔符），否则只给 basename。
///
/// 只有 `is_directory` 为真时才尝试相对化；相对结果为空（搜索根自身）或以 `..` 开头
/// （文件在搜索根之外）时退回 basename。相对结果里的 `\` 统一替换成 `/`，对应 TS 的
/// `relative.replace(/\\/g, "/")`。
fn format_path(file_path: &Path, search_path: &Path, is_directory: bool) -> String {
    if is_directory {
        let relative = js_relative(search_path, file_path);
        let relative_text = relative.to_string_lossy().into_owned();
        if !relative_text.is_empty() && !relative_text.starts_with("..") {
            return relative_text.replace('\\', "/");
        }
    }
    js_basename(file_path)
}

/// 把文件内容按 TS 的规则拆行：先 CRLF→LF、再 CR→LF。
fn split_file_lines(content: &str) -> Vec<String> {
    content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(str::to_string)
        .collect()
}

/// JS 数组下标访问（`lines[current - 1]`）：只有非负整数下标且越界时返回 `None`。
///
/// `context` 是未取整的 `number`（TS 允许 `1.5`），行号也可能是小数，此时 `lines[1.5]`
/// 是 `undefined`（TS 用 `?? ""`）。
fn js_array_get(lines: &[String], index: f64) -> Option<&str> {
    if !index.is_finite() || index.fract() != 0.0 || index < 0.0 {
        return None;
    }
    lines.get(index as usize).map(String::as_str)
}

/// `formatBlock(filePath, lineNumber)`：匹配行与 context 行。
///
/// 参数数量与 TS 函数的自由变量一一对应；拆成结构体只会掩盖调用点。
///
/// 文件内容按路径缓存（键是 lossy 后的绝对路径），同一文件的多次匹配只读一次；读取
/// 失败时缓存空内容并输出固定的 `路径:行号: (unable to read file)`。`lines_truncated`
/// 是跨调用累加的「有行被截断」标志，最终变成 `linesTruncated` 通知与 details 字段。
#[allow(clippy::too_many_arguments)]
async fn format_block(
    operations: &Arc<dyn GrepOperations>,
    file_cache: &mut HashMap<String, Vec<String>>,
    file_path: &Path,
    line_number: f64,
    context_value: f64,
    search_path: &Path,
    is_directory: bool,
    lines_truncated: &mut bool,
) -> Vec<String> {
    let relative_path = format_path(file_path, search_path, is_directory);
    // 同一文件可能出现在多条匹配里；缓存避免重复读盘，也让「读取失败」的结果保持一致。
    let cache_key = file_path.to_string_lossy().into_owned();
    let lines = if let Some(lines) = file_cache.get(&cache_key) {
        lines.clone()
    } else {
        let lines = match operations.read_file(file_path).await {
            Ok(content) => split_file_lines(&content),
            Err(_) => Vec::new(),
        };
        file_cache.insert(cache_key, lines.clone());
        lines
    };
    // 空 Vec 只可能来自读取失败：即使文件为空，`split_file_lines("")` 也会返回一个空行。
    if lines.is_empty() {
        return vec![format!(
            "{relative_path}:{}: (unable to read file)",
            js_number(line_number)
        )];
    }
    let mut block: Vec<String> = Vec::new();
    // TS 不取整：`context: 1.5` 会产生 2.5/3.5/… 的小数行号与空文本（`lines[1.5]` 是 undefined）。
    let start = if context_value > 0.0 {
        (line_number - context_value).max(1.0)
    } else {
        line_number
    };
    let end = if context_value > 0.0 {
        (line_number + context_value).min(lines.len() as f64)
    } else {
        line_number
    };
    let mut current = start;
    // 按 JS number 逐格递增（可能是小数）；非整数下标在 `js_array_get` 里返回 None，
    // 文本退化为空串，对应 TS 的 `lines[current - 1] ?? ""`。
    while current <= end {
        let line_text = js_array_get(&lines, current - 1.0).unwrap_or_default();
        let sanitized = line_text.replace('\r', "");
        let truncated = truncate_line(&sanitized, GREP_MAX_LINE_LENGTH);
        if truncated.was_truncated {
            *lines_truncated = true;
        }
        // 匹配行用 `path:line: text`，上下文行用 `path-line- text`（TS 的两种模板）。
        if current == line_number {
            block.push(format!(
                "{relative_path}:{}: {}",
                js_number(current),
                truncated.text
            ));
        } else {
            block.push(format!(
                "{relative_path}-{}- {}",
                js_number(current),
                truncated.text
            ));
        }
        current += 1.0;
    }
    block
}

/// 一条 `rg --json` 匹配事件中可格式化的部分。
struct RgMatch {
    /// `data.path.text`（非空；空串在 TS 里是假值，不会进入列表）。
    file_path: String,
    /// `data.line_number`（TS 只要求 `typeof === "number"`，允许小数）。
    line_number: f64,
    /// `data.lines.text`。
    ///
    /// 缺失时调用方会退回读文件渲染（即使 `context` 为 0）。
    line_text: Option<String>,
}

/// `rg --json` 事件流的一行。
enum RgEvent {
    /// `type === "match"`。`None` 表示缺少可用的 `path.text`/`line_number`
    /// （TS 仍会 `matchCount++`，只是不把匹配放进列表）。
    Match(Option<RgMatch>),
    /// 其它事件、空白行或无法解析的 JSON。
    Other,
}

/// 解析 `rg --json` 的一行。
///
/// 只有 `type` 严格等于 `"match"` 才进入 `Match`；非 JSON 与其它 `type` 的事件都归为
/// `Other`，既不计数也不格式化。`path.text` 为空串时按缺失处理（TS 用真值判断），
/// `line_number` 必须是 JSON 数字。缺少这两者之一时返回 `Match(None)`：调用方仍会
/// `match_count += 1`，因此这类匹配会占用 limit。
fn parse_event(line: &str) -> RgEvent {
    let Ok(event) = serde_json::from_str::<JsonValue>(line) else {
        return RgEvent::Other;
    };
    if event.get("type").and_then(JsonValue::as_str) != Some("match") {
        return RgEvent::Other;
    }
    let data = event.get("data");
    let file_path = data
        .and_then(|data| data.get("path"))
        .and_then(|path| path.get("text"))
        .and_then(JsonValue::as_str)
        .filter(|path| !path.is_empty());
    let line_number = data
        .and_then(|data| data.get("line_number"))
        .and_then(JsonValue::as_f64);
    let Some((file_path, line_number)) = file_path.zip(line_number) else {
        return RgEvent::Match(None);
    };
    let line_text = data
        .and_then(|data| data.get("lines"))
        .and_then(|lines| lines.get("text"))
        .and_then(JsonValue::as_str)
        .map(str::to_string);
    RgEvent::Match(Some(RgMatch {
        file_path: file_path.to_string(),
        line_number,
        line_text,
    }))
}

/// `grep` 的 `execute` 实现。
struct GrepExecute {
    /// 创建工具时绑定的工作目录；占位上下文或 `ctx.cwd()` 为空时作为回退值。
    cwd: String,
    /// 自定义操作；`None` 时在 `run` 里构造 `LocalGrepOperations`。
    operations: Option<Arc<dyn GrepOperations>>,
}

// `ToolDefinitionExecute::execute` 的通用契约见 kiana-ext-api。这里不做 `select!`：
// 取消由 `run` 在 spawn 之后自行观察（见模块文档的「取消语义」），
// 这样 `ensureTool`/`isDirectory` 期间的取消会被有意忽略，与 TS 行为一致。
impl ToolDefinitionExecute for GrepExecute {
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
                return Err(tool_error("Operation aborted"));
            }
            let input = match serde_json::from_value::<GrepToolInput>(params) {
                Ok(input) => input,
                Err(error) => return Err(tool_error(error.to_string())),
            };
            self.run(input, signal, &ctx).await
        })
    }
}

impl GrepExecute {
    /// 执行一次 `grep`：解析 rg、拼参数、spawn、消费事件流、格式化与截断。
    ///
    /// 错误都是 [`AgentToolError`]（`message` 即模型可见文本）。成功的「无匹配」是
    /// `No matches found` 文本，而不是错误；`details` 只在有通知时非 `Null`。
    async fn run(
        &self,
        input: GrepToolInput,
        signal: Option<&CancellationToken>,
        ctx: &Arc<dyn ExtensionToolContextApi>,
    ) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
        // 阶段 1：解析 rg。`ensure_tool_without_status` 在工具缺失时会尝试下载，因此这里
        // 可能耗时；最终不可用只返回 None，统一映射成同一条错误。
        let Some(rg_path) = ensure_tool_without_status("rg").await else {
            return Err(tool_error(
                "ripgrep (rg) is not available and could not be downloaded",
            ));
        };

        // 阶段 2：解析搜索根与操作实现。顺序是「先确保 rg、再检查路径」，两者同时失败
        // 时报的是 rg 不可用；`is_directory` 的任何错误都被归为 `Path not found`。
        let cwd = crate::read::effective_cwd(ctx, &self.cwd);
        let search_path = resolve_to_cwd(input.path.as_deref().unwrap_or("."), &cwd);
        let search_path_text = search_path.to_string_lossy().into_owned();
        let operations: Arc<dyn GrepOperations> = self
            .operations
            .clone()
            .unwrap_or_else(|| Arc::new(LocalGrepOperations));
        let is_directory = match operations.is_directory(&search_path).await {
            Ok(is_directory) => is_directory,
            Err(_) => {
                return Err(tool_error(format!("Path not found: {search_path_text}")));
            }
        };

        let context_value = effective_context(input.context);
        let effective_limit = effective_limit(input.limit);

        // 阶段 3：拼 argv。前缀顺序固定：argv[0] 是 `--no-config`，它让
        // RIPGREP_CONFIG_PATH 被忽略（测试断言）；`--` 之后的第一个参数是 pattern，保证 `-x`
        // 形式的搜索文本不会被 ripgrep 当成选项。
        let mut args: Vec<String> = [
            "--no-config",
            "--json",
            "--line-number",
            "--color=never",
            "--hidden",
        ]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
        if input.ignore_case.unwrap_or(false) {
            args.push("--ignore-case".to_string());
        }
        if input.literal.unwrap_or(false) {
            args.push("--fixed-strings".to_string());
        }
        // `if (glob)`：空字符串在 TS 里是假值，不追加 `--glob`。
        if let Some(glob) = input.glob.as_ref().filter(|glob| !glob.is_empty()) {
            args.push("--glob".to_string());
            args.push(glob.clone());
        }
        args.push("--".to_string());
        args.push(input.pattern.clone());
        args.push(search_path_text.clone());

        // 阶段 4：spawn。stdin 丢弃、stdout/stderr 走管道；stderr 由独立任务读空，
        // 避免子进程因 stderr 管道写满而阻塞。该任务的读取错误被忽略，只保留文本。
        let spawn_options = SpawnOptions {
            stdin: StdioMode::Ignore,
            stdout: StdioMode::Pipe,
            stderr: StdioMode::Pipe,
            ..SpawnOptions::default()
        };
        let mut child = spawn_process(&rg_path, &args, &spawn_options).map_err(|error| {
            tool_error(format!(
                "Failed to run ripgrep: {}",
                node_error_message(&error)
            ))
        })?;

        let stderr_task = child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                let mut text = Vec::new();
                let _ = stderr.read_to_end(&mut text).await;
                String::from_utf8_lossy(&text).into_owned()
            })
        });

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| tool_error("Failed to run ripgrep: stdout is unavailable"))?;
        let mut lines = crate::find::LossyLines::new(tokio::io::BufReader::new(stdout));

        let mut match_count: f64 = 0.0;
        let mut match_limit_reached = false;
        let mut killed_due_to_limit = false;
        let mut aborted = false;
        let mut matches: Vec<RgMatch> = Vec::new();
        // TS 只在 spawn 之后才注册 abort 监听器（grep.ts:194），而 `addEventListener` 在
        // 已经 aborted 的 signal 上不会触发：`await ensureTool("rg")` 或
        // `await ops.isDirectory()` 期间发生的取消被忽略，rg 会跑完并返回匹配结果。
        // 因此在 spawn 时已经取消的 token 必须永远不进入下面的 abort 分支。
        let observe_abort = signal.is_some_and(|signal| !signal.is_cancelled());
        let abort = async move {
            match signal {
                Some(signal) if observe_abort => signal.cancelled().await,
                _ => std::future::pending::<()>().await,
            }
        };
        let mut abort = std::pin::pin!(abort);

        // 阶段 5：事件循环。空行（JS trim 后为空）跳过；只有 match 事件计数。abort 分支
        // 的 `if !aborted` guard 保证 abort 只处理一次；之后循环继续读 stdout，直到
        // EOF、读错误或达到 limit（后者会再次 kill 并 break）。
        loop {
            tokio::select! {
                _ = abort.as_mut(), if !aborted => {
                    aborted = true;
                    let _ = child.start_kill();
                }
                line = lines.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            if kiana_json::js_string::js_trim(&line).is_empty() {
                                continue;
                            }
                            // TS 先 `matchCount++`，再检查 path/lineNumber：`rg` 对非 UTF-8
                            // 文件名只给 `path.bytes`，这类匹配仍占用 limit。
                            if let RgEvent::Match(entry) = parse_event(&line) {
                                match_count += 1.0;
                                if let Some(entry) = entry {
                                    matches.push(entry);
                                }
                                if match_count >= effective_limit {
                                    match_limit_reached = true;
                                    killed_due_to_limit = true;
                                    let _ = child.start_kill();
                                    break;
                                }
                            }
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
            }
        }

        // 阶段 6：回收子进程并取回 stderr。即使已经 abort/kill 也要等 `wait` 返回，
        // 之后才等待 stderr 任务的最终文本。
        let status = child.wait().await;
        let stderr = match stderr_task {
            Some(task) => task.await.unwrap_or_default(),
            None => String::new(),
        };
        let exit_code = match &status {
            Ok(status) => status.code(),
            Err(_) => None,
        };

        // 阶段 7：判定结果。abort 优先于退出码；被 limit 杀掉时直接跳过退出码检查
        // （此时 `status.code()` 可能为 `None`）；其余只接受 0（有匹配）与 1（无匹配）。
        if aborted {
            return Err(tool_error("Operation aborted"));
        }
        if !killed_due_to_limit && exit_code != Some(0) && exit_code != Some(1) {
            let stderr = kiana_json::js_string::js_trim(&stderr);
            let error_message = if !stderr.is_empty() {
                stderr.to_string()
            } else {
                format!("ripgrep exited with code {}", js_exit_code(exit_code))
            };
            return Err(tool_error(error_message));
        }
        // 阶段 8：0 条匹配是成功结果（rg 无匹配时以 1 退出，上面已放行），details 为 Null。
        if match_count == 0.0 {
            return Ok(AgentToolResult {
                content: vec![text_block("No matches found")],
                details: JsonValue::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            });
        }

        // 阶段 9：格式化。context 为 0 且事件自带 `lines.text` 时走快路径（不读文件）；
        // 否则读文件展开匹配行与上下文行，逐行截到 500 字符。
        let mut output_lines: Vec<String> = Vec::new();
        let mut lines_truncated = false;
        let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();
        for entry in &matches {
            let file_path = PathBuf::from(&entry.file_path);
            if context_value == 0.0
                && let Some(line_text) = &entry.line_text
            {
                let relative_path = format_path(&file_path, &search_path, is_directory);
                let sanitized = line_text.replace("\r\n", "\n").replace('\r', "");
                let sanitized = sanitized.strip_suffix('\n').unwrap_or(&sanitized);
                let truncated = truncate_line(sanitized, GREP_MAX_LINE_LENGTH);
                if truncated.was_truncated {
                    lines_truncated = true;
                }
                output_lines.push(format!(
                    "{relative_path}:{}: {}",
                    js_number(entry.line_number),
                    truncated.text
                ));
            } else {
                let block = format_block(
                    &operations,
                    &mut file_cache,
                    &file_path,
                    entry.line_number,
                    context_value,
                    &search_path,
                    is_directory,
                    &mut lines_truncated,
                )
                .await;
                output_lines.extend(block);
            }
        }

        // 阶段 10：字节截断（行数上限用 MAX_SAFE_INTEGER，只为让
        // `details.truncation.maxLines` 表示“无行限制”且保持当前稳定值；字节上限取默认 50 KiB）。
        let raw_output = output_lines.join("\n");
        let truncation = truncate_head(
            &raw_output,
            &TruncationOptions {
                max_lines: Some(crate::find::max_safe_integer_usize()),
                max_bytes: None,
            },
        );
        let mut output = truncation.content.clone();
        // 阶段 11：按 TS 的赋值顺序组装通知与 details：
        // matchLimitReached → truncation → linesTruncated，通知用 `. ` 连接后包进 `[...]`。
        // TS 的键序是赋值顺序：matchLimitReached → truncation → linesTruncated。
        // `GrepToolDetails` 的字段序不同，因此手工构造 JSON 对象（结构体在冻结契约
        // `kiana-ext-api` 中，不改其字段序）。
        let mut details = serde_json::Map::new();
        let mut notices: Vec<String> = Vec::new();
        if match_limit_reached {
            notices.push(format!(
                "{} matches limit reached. Use limit={} for more, or refine pattern",
                js_number(effective_limit),
                js_number(effective_limit * 2.0)
            ));
            details.insert(
                "matchLimitReached".to_string(),
                js_number_value(effective_limit),
            );
        }
        if truncation.truncated {
            notices.push(format!(
                "{} limit reached",
                format_size(DEFAULT_MAX_BYTES as u64)
            ));
            if let Ok(value) = serde_json::to_value(&truncation) {
                details.insert("truncation".to_string(), value);
            }
        }
        if lines_truncated {
            notices.push(format!(
                "Some lines truncated to {GREP_MAX_LINE_LENGTH} chars. Use read tool to see full lines"
            ));
            details.insert("linesTruncated".to_string(), JsonValue::Bool(true));
        }
        if !notices.is_empty() {
            output.push_str(&format!("\n\n[{}]", notices.join(". ")));
        }

        Ok(AgentToolResult {
            content: vec![text_block(output)],
            details: if details.is_empty() {
                JsonValue::Null
            } else {
                JsonValue::Object(details)
            },
            structured_content: None,
            usage: None,
            is_error: None,
            terminate: None,
        })
    }
}

/// JS `String(number)`。
fn js_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value == f64::INFINITY {
        return "Infinity".to_string();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_string();
    }
    kiana_json::js_num::format_js_number(value)
}

/// 把 `f64` 按 JS `JSON.stringify` 的数字文本变成 JSON 值（`NaN`/`±Infinity` → `null`）。
fn js_number_value(value: f64) -> JsonValue {
    serde_json::from_str(&kiana_json::js_num::format_js_number(value)).unwrap_or(JsonValue::Null)
}

/// TS 模板 `${code}`（`null` → `"null"`）。
fn js_exit_code(code: Option<i32>) -> String {
    code.map(|code| code.to_string())
        .unwrap_or_else(|| "null".to_string())
}

/// Node 的 `error.message` 形如 `spawn rg ENOENT`；Rust 的 `io::Error` 文本已由 kiana-core 对齐。
fn node_error_message(error: &std::io::Error) -> String {
    error.to_string()
}

/// 工具错误（`Error.message` 逐字）。
fn tool_error(message: impl Into<String>) -> AgentToolError {
    AgentToolError {
        message: message.into(),
        name: None,
        stack: None,
    }
}

/// 一个 text 内容块。
fn text_block(text: impl Into<String>) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.into(),
        text_signature: None,
    })
}

// ===========================================================================
// 定义与工厂
// ===========================================================================

/// `createGrepToolDefinition(cwd, options?)`。
///
/// 固定字段：`name`/`label` 为 `"grep"`，`description` 是 [`GREP_TOOL_DESCRIPTION`]，
/// `parameters` 是 [`grep_parameters_schema`]，`prompt_snippet` 取 contribution 的
/// snippet；`prompt_guidelines` 与 `constrained_sampling` 为 `None`，其余可选字段保持
/// 缺省，渲染器来自 `grep_renderers()`。`execute` 持有 `cwd` 与可选的自定义操作。
pub fn create_grep_tool_definition(
    cwd: impl Into<String>,
    options: Option<GrepToolOptions>,
) -> ToolDefinition {
    let options = options.unwrap_or_default();
    let contribution = grep_tool_system_prompt_contribution();
    let renderers: ToolRenderers = grep_renderers();
    ToolDefinition {
        name: "grep".to_string(),
        label: "grep".to_string(),
        description: GREP_TOOL_DESCRIPTION.to_string(),
        prompt_snippet: Some(contribution.snippet.to_string()),
        // TS 的 `createGrepToolDefinition` 不设置 `promptGuidelines`（contribution 常量里是
        // 空数组，但定义对象上该属性是 `undefined`）。
        prompt_guidelines: None,
        parameters: grep_parameters_schema(),
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
        execute: Arc::new(GrepExecute {
            cwd: cwd.into(),
            operations: options.operations,
        }),
        render_call: renderers.render_call,
        render_result: renderers.render_result,
    }
}

/// `createGrepTool(cwd, options?)`。
///
/// 经 `wrap_tool_definition(..., None)` 投影为 [`AgentTool`]：只保留 `name`/`label`/
/// `description`/`parameters` 等执行所需字段，prompt 元数据与渲染器不进入
/// [`AgentTool`]（原因见 `definition.rs` 的模块文档）。
pub fn create_grep_tool(cwd: impl Into<String>, options: Option<GrepToolOptions>) -> AgentTool {
    wrap_tool_definition(create_grep_tool_definition(cwd, options), None)
}
