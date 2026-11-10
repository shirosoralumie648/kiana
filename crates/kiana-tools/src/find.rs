//! core/tools/find.ts。
//!
//! `find` 工具：调用外部 `fd`（通过 `kiana_core::tools_manager::ensure_tool` 解析，必要时下载），
//! 逐行收集结果并按搜索根做相对化；自定义操作提供 `glob()` 时走内存分支。
//!
//! 为什么必须 spawn `fd`：`.gitignore` 的层级语义、`--max-results`、`--full-path` 的匹配规则
//! 都由 fd 决定（spec §8.2 #13）。`ignore`/`globset` crate 不得用于本工具。
//!
//! # `relativizeFindResultPath` 的可替换 path 模块
//!
//! TS 的第三个参数是 `pathModule: path.PlatformPath`（回归测试传 `path.win32` / `path.posix`）。
//! Rust 用 [`FindPathModule`] 表达同一注入点；[`POSIX_PATH`] / [`WINDOWS_PATH`] 是两种实现，
//! 生产代码用 [`platform_path_module`]。
//!
//! # 执行流程
//!
//! 1. `FindExecute::execute` 先检查取消，再把 JSON 反序列化为 `FindToolInput`
//!    （失败时把 serde 的错误文本作为工具错误），随后调用私有的 `run`。
//! 2. `run` 解析搜索根：`ctx.cwd()` 优先，为空或占位上下文时回落到创建工具时的 `cwd`；
//!    `input.path` 缺省为 `"."`，经 `resolve_to_cwd` 转为路径。
//! 3. 若 `FindToolOptions.operations` 是 `Some`，走自定义分支 `run_custom`：
//!    先 `exists` 检查搜索根，再 `glob` 取结果，逐条相对化。该分支**不会** spawn
//!    `fd`；默认分支也不使用 `LocalFindOperations`，而是在 `run` 里直接启动 fd。
//! 4. 否则是默认分支：`ensure_tool_without_status("fd")` 解析可执行文件（不在本地时
//!    尝试下载），探测搜索根祖先链上是否存在 `.git`，据此决定是否传
//!    `--no-require-git`，然后 spawn `fd`。
//! 5. 读 stdout 用 `LossyLines`（Node readline 语义）；stderr 由独立任务读空，避免
//!    子进程写满管道阻塞。stdout 读完后才 `wait()` 收退出码；取消时 `start_kill`
//!    并继续等到 `wait()` 返回，保证回收子进程。
//! 6. 退出码非 0 且 stdout 为空时返回错误；stdout 非空时保留结果。随后每行去掉行尾
//!    `\r`、做 JS `trim`、跳过空行、相对化。
//! 7. 私有函数 `build_result` 用 `truncate_head`（行数上限为 `MAX_SAFE_INTEGER`，
//!    字节上限取默认 50 KiB）截断，拼出 notices 与 details。
//!
//! # 输出契约
//!
//! - 结果路径统一用 `/` 分隔（[`relativize_find_result_path`] 的返回值），搜索根自身
//!   相对化为空串，不补 `./`。
//! - `limit` 判定是 `lines.len() >= effective_limit`，比较对象是**未取整**的
//!   `limit ?? 1000`；`details.resultLimitReached` 也写这个未取整值。
//! - `details` 的键序固定为 `resultLimitReached` → `truncation`（TS 的赋值顺序），
//!   因此这里手工构造 `serde_json::Map`，而不是序列化 `FindToolDetails` 结构体。
//! - 只有 fd 分支的 limit 通知带 `Use limit=... for more` 提示；自定义分支不带
//!   （由 `build_result` 的 `limit_notice_has_hint` 参数区分）。
//!
//! # 注意事项
//!
//! - 取消检查点分散在多个 await 前后，自定义操作进行中也能立即中止（`abortable`
//!   直接丢弃 future）。被丢弃的 future 不会继续推进：若其内部有副作用（例如
//!   `ensure_tool_without_status` 触发的下载），不会执行清理代码；`kiana-core` 的
//!   `tools_manager` 文档说明这可能留下未下载完的归档。
//! - 搜索根是否存在只在自定义分支显式检查（`Path not found: ...`）；默认分支把它
//!   交给 `fd` 的退出码与 stderr。
//! - `FIND_TOOL_DESCRIPTION`、schema 与通知文案都是逐字契约，改动会破坏与 TS 的
//!   输出对齐（集成测试与黄金文件会失败）。

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use kiana_agent_core::{AgentTool, AgentToolError, AgentToolResult};
use kiana_ai::types::{JsonValue, TextContent, TextKind, UserContentBlock};
use kiana_core::child_process::{SpawnOptions, StdioMode, spawn_process};
use kiana_core::tools_manager::ensure_tool_without_status;
/// `find` 的输入与结果详情类型（定义在冻结契约 crate `kiana_ext_api`）。
///
/// 在模块里重新导出，使本模块的公开面与 TS `find.ts` 的导出保持一致；`crate::index`
/// 也会再导出它们。
pub use kiana_ext_api::tool_types::{FindToolDetails, FindToolInput};
use kiana_ext_api::tool_types::{ToolDefinition, ToolDefinitionExecute, ToolRenderers};
use kiana_ext_api::types::ExtensionToolContextApi;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};
use tokio_util::sync::CancellationToken;

use crate::definition::wrap_tool_definition;
use crate::path_utils::{path_exists, resolve_to_cwd};
use crate::read::ToolOperationError;
use crate::renderers::find::find_renderers;
use crate::truncate::{DEFAULT_MAX_BYTES, TruncationOptions, format_size, truncate_head};

/// `DEFAULT_LIMIT = 1000`。
///
/// 用 `f64` 而不是整数，是为了保留 TS `number` 的语义：模型可以传 `limit: 2.5`，
/// 该值不会被取整，会原样出现在 `--max-results`、通知文案与 `details.resultLimitReached`
/// 中（`kiana-ext-api` 的 `FindToolDetails` 文档与集成测试
/// `fd_argv_skips_full_path_for_basename_patterns` 都锁定了这一点）。
pub const DEFAULT_LIMIT: f64 = 1000.0;

/// `Number.MAX_SAFE_INTEGER`（`truncateHead(..., { maxLines })` 的「无行限制」值）。
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// `MAX_SAFE_INTEGER` 在当前平台的 `usize` 表示（32 位平台退化为 `usize::MAX`）。
///
/// grep 的 `truncateHead(..., { maxLines: Number.MAX_SAFE_INTEGER })` 也用它，
/// 这样 `details.truncation.maxLines` 是当前稳定契约（改动需同步测试）。
pub(crate) fn max_safe_integer_usize() -> usize {
    usize::try_from(MAX_SAFE_INTEGER).unwrap_or(usize::MAX)
}

// ===========================================================================
// Node readline 等价的行读取
// ===========================================================================

/// Node `readline` 的行读取器：按 `\r\n` / `\n` / `\r` 切行，非法 UTF-8 替换为 U+FFFD。
///
/// 为什么不能用 [`tokio::io::AsyncBufReadExt::lines`]：`lines()` 在遇到第一个非法 UTF-8
/// 字节时返回 `InvalidData`。`fd` 会原样打印非 UTF-8 文件名（Linux 上合法），调用方若就此
/// 结束读取，子进程写满管道后会阻塞在写端，`child.wait()` 永不返回；而且后续结果会被静默丢弃。
/// Node 的 `readline` 基于 `StringDecoder`，把非法字节替换为 U+FFFD 后继续（TS: `find.ts`
/// 的 `rl.on("line")`）。
///
/// 行尾判定与默认 `crlfDelay` 一致：`\r` 后紧跟 `\n` 视为一个行尾；输入末尾的孤立 `\r`
/// 也是一个行尾。`\r` 出现在缓冲区末尾时等待下一块数据，避免把 `\r\n` 切成两行。
pub(crate) struct LossyLines<R> {
    reader: R,
    buffer: Vec<u8>,
    eof: bool,
}

impl<R: tokio::io::AsyncBufRead + Unpin> LossyLines<R> {
    /// 包装一个异步缓冲读取器。
    pub(crate) fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
            eof: false,
        }
    }

    /// 下一行（不含行尾）；流结束后返回 `None`。
    pub(crate) async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        loop {
            if let Some((index, terminator_len)) = line_terminator(&self.buffer, self.eof) {
                let bytes: Vec<u8> = self.buffer.drain(..index + terminator_len).collect();
                return Ok(Some(String::from_utf8_lossy(&bytes[..index]).into_owned()));
            }
            if self.eof {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                let bytes = std::mem::take(&mut self.buffer);
                return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
            }
            self.fill().await?;
        }
    }

    async fn fill(&mut self) -> std::io::Result<()> {
        let chunk = self.reader.fill_buf().await?;
        let len = chunk.len();
        if len == 0 {
            self.eof = true;
        } else {
            self.buffer.extend_from_slice(chunk);
            self.reader.consume(len);
        }
        Ok(())
    }
}

/// 在 `buffer` 中找行尾；返回 `(行内容长度, 行尾字节数)`。
fn line_terminator(buffer: &[u8], eof: bool) -> Option<(usize, usize)> {
    for (index, byte) in buffer.iter().enumerate() {
        match byte {
            b'\n' => return Some((index, 1)),
            b'\r' => {
                if index + 1 < buffer.len() {
                    return Some((index, if buffer[index + 1] == b'\n' { 2 } else { 1 }));
                }
                return if eof { Some((index, 1)) } else { None };
            }
            _ => {}
        }
    }
    None
}

/// 等待 `future`，`signal` 取消时立即放弃它（TS 的 `onAbort` 在任意 await 期间 reject）。
///
/// 返回 `None` 表示已中止。丢弃 future 只用于没有子进程的操作（`ensureTool` / 自定义操作）。
pub(crate) async fn abortable<T>(
    future: impl std::future::Future<Output = T>,
    signal: Option<&CancellationToken>,
) -> Option<T> {
    match signal {
        Some(signal) => {
            tokio::pin!(future);
            tokio::select! {
                value = future.as_mut() => Some(value),
                _ = signal.cancelled() => None,
            }
        }
        None => Some(future.await),
    }
}

// ===========================================================================
// relativizeFindResultPath
// ===========================================================================

/// 可替换的 `node:path` 模块（只实现 `relativizeFindResultPath` 需要的三个操作）。
///
/// 这个注入点用来复刻 TS 的第三个参数：生产路径用 [`platform_path_module`]，回归
/// 测试可以直接拿 [`POSIX_PATH`] / [`WINDOWS_PATH`] 在任意宿主上验证另一种平台的语义
/// （两个实现都没有 `cfg` 门控，始终参与编译）。
///
/// 实现约定（[`relativize_find_result_path`] 依赖）：
/// - [`FindPathModule::sep`] 是输出拆分的分隔符：`relative` 的返回值按它切成段，
///   再用 `/` 重新连接。
/// - [`FindPathModule::is_absolute`] 为 `false` 时不会调用 `relative`，路径只做
///   分隔符归一化（再按需补尾 `/`）后返回。
/// - [`FindPathModule::relative`] 必须模拟 `path.posix.relative` / `path.win32.relative`：
///   两路径相同时返回空串，root 不同时返回目标的绝对路径。
pub trait FindPathModule: Send + Sync {
    /// `path.sep`。
    fn sep(&self) -> char;
    /// `path.isAbsolute(p)`。
    fn is_absolute(&self, path: &str) -> bool;
    /// `path.relative(from, to)`。
    fn relative(&self, from: &str, to: &str) -> String;
}

/// POSIX 语义（`path.posix`）。
#[derive(Debug, Clone, Copy, Default)]
pub struct PosixPathModule;

/// Windows 语义（`path.win32`）。
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsPathModule;

/// `path.posix`。
pub static POSIX_PATH: PosixPathModule = PosixPathModule;
/// `path.win32`。
pub static WINDOWS_PATH: WindowsPathModule = WindowsPathModule;

/// 当前平台的 `path` 模块。
///
/// 选择依据是编译期常量 `cfg!(windows)`：Windows 构建得到 [`WINDOWS_PATH`]，其余
/// 平台得到 [`POSIX_PATH`]。因此非 Windows 宿主上，`C:\x` 这类 Windows 风格绝对路径
/// 会因 [`POSIX_PATH`] 的 `is_absolute` 返回 `false` 而不做相对化、原样返回。
pub fn platform_path_module() -> &'static dyn FindPathModule {
    if cfg!(windows) {
        &WINDOWS_PATH
    } else {
        &POSIX_PATH
    }
}

/// 按给定分隔符切分路径的 root 与段。
///
/// 这是 `path.resolve` / `path.relative` 所需子集的字符串级重写：不访问文件系统，
/// 也不探测平台，语义完全由 `separators` 决定（POSIX 传 `['/']`，win32 传
/// `['\\', '/']`）。
///
/// 返回 `(root, segments, absolute)`：
/// - `root`：`""`（相对路径）、`"/"`、`"\"`、`"C:"` / `"C:\"`、UNC 的
///   `"\\server\"`。盘符与 UNC 前缀属于 root，不算段。
/// - `segments`：root 之后的路径段，已按 Node 的 `resolve` 语义折叠重复分隔符、
///   去掉 `.`、按 `..` 回退（绝对路径不会退到 root 之外）。UNC 的 share 仍是第一个段。
/// - `absolute`：存在盘符或前导分隔符时为 `true`。注意单独出现的 `C:` 在这里也是
///   `true`，而 `path.win32.isAbsolute("C:")` 是 `false`；`WindowsPathModule::is_absolute`
///   因此没有复用这个标志。
fn split_path(path: &str, separators: &[char]) -> (String, Vec<String>, bool) {
    let is_sep = |character: char| separators.contains(&character);
    let windows = separators.contains(&'\\');
    let mut chars = path.chars().peekable();
    let mut root = String::new();
    let mut absolute = false;

    // win32 盘符（`C:`）按原样保留；POSIX 只有前导 `/`。
    if windows {
        let mut probe = path.chars();
        if let (Some(drive), Some(':')) = (probe.next(), probe.next())
            && drive.is_ascii_alphabetic()
        {
            root.push(drive);
            root.push(':');
            absolute = true;
            chars.next();
            chars.next();
        }
    }
    // win32 UNC 设备根 `\\server\share`。Node 的 `win32.relative` 去掉前导的 `\\` 后按
    // 组件比较：不同 server 是不同 root（返回绝对目标），同一 server 的不同 share 仍按
    // 段计算相对路径。因此 root 只记录 `\\server\`，share 仍作为第一个段。
    // `win32.resolve` 只在 server 与 share 都非空时按 UNC 处理（`\\s1` 是普通 rooted
    // path），且 `.`/`?` 开头的设备路径另有规则。
    let mut unc_root = false;
    if windows && root.is_empty() {
        let mut probe = chars.clone();
        let mut server = String::new();
        let mut is_unc = false;
        if probe.next().is_some_and(is_sep) && probe.next().is_some_and(is_sep) {
            while let Some(&character) = probe.peek() {
                if is_sep(character) {
                    break;
                }
                server.push(character);
                probe.next();
            }
            if !server.is_empty()
                && server != "."
                && server != "?"
                && probe.peek().is_some_and(|character| is_sep(*character))
            {
                while probe.peek().is_some_and(|character| is_sep(*character)) {
                    probe.next();
                }
                is_unc = probe.peek().is_some_and(|character| !is_sep(*character));
            }
        }
        if is_unc {
            chars.next();
            chars.next();
            for _ in 0..server.chars().count() {
                chars.next();
            }
            root.push('\\');
            root.push('\\');
            root.push_str(&server);
            absolute = true;
            unc_root = true;
        }
    }
    if chars.peek().is_some_and(|character| is_sep(*character)) {
        absolute = true;
        let unc = windows && root.is_empty() && path.starts_with("\\\\");
        // 折叠重复分隔符（Node 的 resolve 语义）。
        while chars.peek().is_some_and(|character| is_sep(*character)) {
            chars.next();
        }
        if unc {
            root.push('\\');
            root.push('\\');
        } else if root.is_empty() {
            root.push(if windows { '\\' } else { '/' });
        } else {
            root.push('\\');
        }
    }

    let mut segments: Vec<String> = Vec::new();
    let mut current = String::new();
    for character in chars {
        if is_sep(character) {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }

    // 规范化 `.` 与 `..`（Node 的 resolve 语义）。
    //
    // UNC 路径的 share 属于设备根（`\\server\share`）：`win32.resolve` 不会把 share
    // 当普通段处理（即使它字面上是 `.` / `..`），`..` 也不能弹出设备根。
    let protected = usize::from(unc_root);
    let mut normalized: Vec<String> = segments[..protected.min(segments.len())].to_vec();
    for segment in &segments[normalized.len()..] {
        match segment.as_str() {
            "." => {}
            ".." => {
                if normalized.len() > protected
                    && normalized.last().is_some_and(|last| last != "..")
                {
                    normalized.pop();
                } else if !absolute {
                    normalized.push(segment.clone());
                }
            }
            _ => normalized.push(segment.clone()),
        }
    }
    (root, normalized, absolute)
}

/// 计算两段已规范化的路径之间的相对路径。
///
/// 输入必须是 `split_path` 的输出（段已规范化），本函数只做前缀比较：
/// - root 不同：回退到拼接绝对路径（`join_path`），对应 Node 的「不同盘符 /
///   不同 UNC server」行为；UNC 设备根在拼接后再补尾分隔符（见函数体注释）。
/// - root 相同：从第一个不同段开始，`from` 的剩余段各出一个 `..`，再接 `to` 的
///   剩余段；两段完全相同时返回空串（调用方据此把搜索根自身相对化为 `""`）。
///
/// `case_insensitive` 只由 [`WindowsPathModule`] 传 `true`（win32 比较盘符与段时
/// 忽略 ASCII 大小写）；POSIX 传 `false`。
fn relative_segments(
    from_root: &str,
    from_segments: &[String],
    to_root: &str,
    to_segments: &[String],
    sep: char,
    case_insensitive: bool,
) -> String {
    let same_root = if case_insensitive {
        from_root.eq_ignore_ascii_case(to_root)
    } else {
        from_root == to_root
    };
    if !same_root {
        let joined = join_path(to_root, to_segments, sep);
        // `win32.resolve` 会把只有 `server\share` 的 UNC 路径规范成以 `\` 结尾的设备根
        //（`\\server\share\`）；不同 root 时 relative 直接返回该规范路径，find.ts 随后
        // 把 `\` 换成 `/`。
        if sep == '\\'
            && to_root.starts_with("\\\\")
            && to_segments.len() == 1
            && !joined.ends_with(sep)
        {
            return format!("{joined}{sep}");
        }
        return joined;
    }
    let mut common = 0usize;
    while common < from_segments.len()
        && common < to_segments.len()
        && (if case_insensitive {
            from_segments[common].eq_ignore_ascii_case(&to_segments[common])
        } else {
            from_segments[common] == to_segments[common]
        })
    {
        common += 1;
    }
    if common == from_segments.len() && common == to_segments.len() {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from_segments.len() {
        parts.push("..".to_string());
    }
    parts.extend(to_segments[common..].iter().cloned());
    parts.join(&sep.to_string())
}

/// 把 root 与段重新拼成绝对路径（`relative` 的「不同 root」回退）。
///
/// root 为空时直接返回段拼接结果；root 已以分隔符结尾（`/`、`C:\`、`\\server\`）
/// 时不重复插入分隔符。只有盘符形式的 root（`C:`，对应 `C:foo` 这种 relative 输入）
/// 也不插分隔符，拼成 `C:foo`。
fn join_path(root: &str, segments: &[String], sep: char) -> String {
    let body = segments.join(&sep.to_string());
    if body.is_empty() {
        return root.to_string();
    }
    let needs_separator = !root.is_empty() && !root.ends_with(sep) && !(root.ends_with(':'));
    format!(
        "{root}{}{body}",
        if needs_separator {
            sep.to_string()
        } else {
            String::new()
        }
    )
}

impl FindPathModule for PosixPathModule {
    fn sep(&self) -> char {
        '/'
    }

    fn is_absolute(&self, path: &str) -> bool {
        path.starts_with('/')
    }

    fn relative(&self, from: &str, to: &str) -> String {
        let (from_root, from_segments, _) = split_path(from, &['/']);
        let (to_root, to_segments, _) = split_path(to, &['/']);
        relative_segments(
            &from_root,
            &from_segments,
            &to_root,
            &to_segments,
            '/',
            false,
        )
    }
}

impl FindPathModule for WindowsPathModule {
    fn sep(&self) -> char {
        '\\'
    }

    fn is_absolute(&self, path: &str) -> bool {
        // `path.win32.isAbsolute`：前导分隔符（`\` 或 `/`）即绝对；盘符需要 `C:\`/`C:/`
        // 三段齐全（`C:` 是相对盘符）。
        let bytes = path.as_bytes();
        let Some(first) = bytes.first() else {
            return false;
        };
        if *first == b'\\' || *first == b'/' {
            return true;
        }
        bytes.len() > 2
            && first.is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/')
    }

    fn relative(&self, from: &str, to: &str) -> String {
        let (from_root, from_segments, _) = split_path(from, &['\\', '/']);
        let (to_root, to_segments, _) = split_path(to, &['\\', '/']);
        relative_segments(
            &from_root,
            &from_segments,
            &to_root,
            &to_segments,
            '\\',
            true,
        )
    }
}

/// `relativizeFindResultPath(resultPath, searchPath, pathModule)`。
///
/// # 参数与返回值
///
/// - `result_path`：fd 的输出行或自定义 `glob` 返回的路径，可能是绝对路径，也可能是
///   相对路径。
/// - `search_path`：搜索根（调用方先用 `resolve_to_cwd` 解析过）。
/// - `path_module`：决定绝对/相对判断与相对化算法，见 [`FindPathModule`]。
///
/// 返回值的分隔符一律是 `/`，便于上层按行展示：
/// 1. 相对路径原样保留，只做分隔符替换；
/// 2. 绝对路径交给 `path_module.relative` 相对化；
/// 3. 若输入以分隔符结尾（win32 下 `/` 也算），而输出没有尾 `/`，则补一个。
///
/// 搜索根自身（`relative` 为空）保持空串，不臆造 `./`。POSIX 下
/// `relativize("/", "/")` 得到 `"/"`：输入 `/` 带尾分隔符，按第 3 条补回（见本文件
/// 测试 `search_root_itself_stays_empty`）。
///
/// 纯字符串运算：不访问文件系统，不检查路径是否存在，也不解析符号链接（`split_path`
/// 只做字面的 `.` / `..` 折叠）。
pub fn relativize_find_result_path(
    result_path: &str,
    search_path: &str,
    path_module: &dyn FindPathModule,
) -> String {
    let sep = path_module.sep();
    let had_trailing_separator =
        result_path.ends_with(sep) || (sep == '\\' && result_path.ends_with('/'));
    let relative_path = if path_module.is_absolute(result_path) {
        path_module.relative(search_path, result_path)
    } else {
        result_path.to_string()
    };
    let posix_path = relative_path.split(sep).collect::<Vec<_>>().join("/");
    if had_trailing_separator && !posix_path.ends_with('/') {
        format!("{posix_path}/")
    } else {
        posix_path
    }
}

/// 平台版的 [`relativize_find_result_path`]。
///
/// 使用 [`platform_path_module`]，是 [`relativize_find_result_path`] 加平台模块的
/// 便捷等价形式；fd 分支与自定义分支各自以 `platform_path_module()` 直接调用
/// [`relativize_find_result_path`]（本包装当前无内部调用方，主要供 crate 外使用）。
/// 平台只影响输入的
/// 绝对/相对判断与拆段方式，返回值本身始终用 `/` 连接各段。
pub fn relativize_find_result_path_platform(result_path: &str, search_path: &str) -> String {
    relativize_find_result_path(result_path, search_path, platform_path_module())
}

// ===========================================================================
// 操作与 schema
// ===========================================================================

/// `findToolSystemPromptContribution` 的 Rust 形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindToolSystemPromptContribution {
    /// 系统提示 "Available tools" 段的一行摘要。
    pub snippet: &'static str,
    /// 追加到 Guidelines 段的条目。
    pub guidelines: &'static [&'static str],
}

/// `findToolSystemPromptContribution`。
///
/// `snippet` 用于系统提示的 "Available tools" 段；`guidelines` 恒为空切片（TS 的
/// contribution 常量是空数组，而工具定义上的 `prompt_guidelines` 仍是 `None`，见
/// [`create_find_tool_definition`] 内的注释）。
pub fn find_tool_system_prompt_contribution() -> FindToolSystemPromptContribution {
    FindToolSystemPromptContribution {
        snippet: "Find files by glob pattern (respects .gitignore)",
        guidelines: &[],
    }
}

/// `find` 的工具描述（逐字）。
///
/// 文案里的 `1000 results` 与 `50KB` 分别对应 [`DEFAULT_LIMIT`] 与 50 KiB 的字节
/// 上限（`DEFAULT_MAX_BYTES`）；调整实现上限时必须同步改这里，否则描述与实际行为不符。
pub const FIND_TOOL_DESCRIPTION: &str = "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first).";

/// `findSchema`（TypeBox 序列化结果，逐字节）。
///
/// `create_find_tool_definition` 把这个值原样放进工具的 `parameters` 字段。
/// `limit` 是没有任何范围约束的 `number`（对应 TS 的 `number`），`required` 只有
/// `pattern`；这与 TS 的定义一致，改动属于对外契约变更。
pub fn find_parameters_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "required": ["pattern"],
        "properties": {
            "pattern": { "type": "string", "description": "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'" },
            "path": { "type": "string", "description": "Directory to search in (default: current directory)" },
            "limit": { "type": "number", "description": "Maximum number of results (default: 1000)" }
        }
    })
}

/// `FindOperations.glob` 的选项（TS 的内联对象 `{ ignore, limit }`）。
///
/// 只由自定义分支 `run_custom` 构造：`ignore` 固定为 `**/node_modules/**` 与
/// `**/.git/**`，`limit` 是未取整的 `limit ?? 1000`。是否真的按这两个值裁剪结果由
/// [`FindOperations`] 的实现决定，本工具不会二次裁剪（超出 `limit` 的结果仍会全部
/// 展示，只在通知与 details 里标明达到上限）。
#[derive(Debug, Clone, PartialEq)]
pub struct FindGlobOptions {
    /// 要忽略的 glob。
    pub ignore: Vec<String>,
    /// 结果上限（TS 的 `number`，未取整）。
    pub limit: f64,
}

/// 可插拔的文件查找操作（例如把搜索委托给远端系统）。
///
/// `find` 工具只在 [`FindToolOptions::operations`] 为 `Some` 时使用本 trait；默认
/// 分支不经过它，直接 spawn `fd`。两个方法都在搜索根经 `resolve_to_cwd` 解析之后调用：
///
/// - [`FindOperations::exists`] 先执行，返回 `false` 时工具报
///   `Path not found: <搜索根>`；
/// - [`FindOperations::glob`] 的返回值可以是相对或绝对路径：绝对路径会被
///   [`relativize_find_result_path`] 相对化，相对路径原样保留。
///
/// 错误处理：`ToolOperationError.message` 会被逐字包装成工具错误
/// （`AgentToolError`），`ToolOperationError.code` 被忽略。取消由调用方负责：
/// `run_custom` 用 `abortable` 包装这两个调用，取消时丢弃 future 并返回
/// `Operation aborted`，实现方不应假设被取消的调用会继续执行。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait FindOperations: Send + Sync {
    /// 路径是否存在。
    async fn exists(&self, absolute_path: &Path) -> Result<bool, ToolOperationError>;
    /// 按 glob 查找文件；返回相对或绝对路径。
    async fn glob(
        &self,
        pattern: &str,
        cwd: &Path,
        options: FindGlobOptions,
    ) -> Result<Vec<String>, ToolOperationError>;
}

/// 默认操作：本地文件系统 + fd。
///
/// `find` 的 `FindToolOptions` 默认是 `None`，`FindExecute::run` 的默认分支不会构造
/// 本类型，而是直接 spawn `fd`。本类型里：
/// - [`FindOperations::exists`] 委托给 `path_exists`（`tokio::fs::metadata` 成功即存在）；
/// - [`FindOperations::glob`] 是返回空 `Vec` 的占位实现：默认分支不会调用它；
///   调用方若把本类型显式放进 `operations`，`glob` 会返回空列表（不会有 fd 结果）。
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalFindOperations;

#[async_trait]
impl FindOperations for LocalFindOperations {
    async fn exists(&self, absolute_path: &Path) -> Result<bool, ToolOperationError> {
        Ok(path_exists(absolute_path).await)
    }

    async fn glob(
        &self,
        _pattern: &str,
        _cwd: &Path,
        _options: FindGlobOptions,
    ) -> Result<Vec<String>, ToolOperationError> {
        // TS 的默认实现是占位 `() => []`：`createFindToolDefinition` 只在**调用方提供**
        // 自定义操作时走 glob 分支，这里的返回值永远不会被读取。
        Ok(Vec::new())
    }
}

/// `FindToolOptions`。
///
/// `operations: Some` 会整体切换到自定义分支（不再解析或启动 `fd`）；`None` 是默认的
/// 本地 `fd` 分支。`Default` 派生给出的就是 `None`。
#[derive(Clone, Default)]
pub struct FindToolOptions {
    /// 自定义查找操作。默认本地文件系统 + fd。
    pub operations: Option<Arc<dyn FindOperations>>,
}

impl std::fmt::Debug for FindToolOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FindToolOptions")
            .field("operations", &self.operations.is_some())
            .finish()
    }
}

/// `limit ?? DEFAULT_LIMIT`（未取整）。
///
/// 不做范围校验：负数与 `0` 不会被拦截，会使 `lines.len() >= effective_limit`
/// 恒真，通知、`details` 与（fd 分支的）`--max-results` 文本里都会原样出现。
/// `NaN` 与 `±Infinity` 无法经 JSON 传入（serde_json 的数字必为有限值）；即便传入，
/// 该比较也恒假，只会出现在 `--max-results` 文本里，不会产生通知或 `details`。
/// 与 TS 的 `??` 一致，只有 `None` 才回落到默认值。
fn effective_limit(limit: Option<f64>) -> f64 {
    limit.unwrap_or(DEFAULT_LIMIT)
}

/// JS `String(number)`。
///
/// 与 `kiana_json::js_num::format_js_number` 的差别只在非有限值：那个函数把
/// `NaN` / `±Infinity` 写成 JSON 字面量 `null`，而 JS 的模板字符串需要
/// `NaN` / `Infinity` / `-Infinity`，所以这里先拦截；有限值直接委托，保持 JS
/// `String(number)` 的数字排版（含 `2.5`、大数的指数形式等）。
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

/// TS 模板 `${code}`（`null` → `"null"`）。
fn js_exit_code(code: Option<i32>) -> String {
    code.map(|code| code.to_string())
        .unwrap_or_else(|| "null".to_string())
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

/// 结果文本 + details 的公共尾部（两条分支的 notices 只有 fd 的 limit 文案不同）。
///
/// details 的键序固定为 `resultLimitReached` → `truncation`（当前稳定契约，改动需同步测试）。
/// `FindToolDetails` 的字段序恰好相反，因此这里手工构造 JSON 对象（结构体定义在冻结契约
/// `kiana-ext-api`，不改其字段序）。
///
/// # 参数与截断
///
/// - `lines`：已经相对化、按 fd/自定义操作输出顺序排列的结果行。
/// - `effective_limit`：未取整的 `limit ?? 1000`。
/// - `limit_notice_has_hint`：fd 分支传 `true`（通知带 `Use limit=... for more`），
///   自定义分支传 `false`。
///
/// 截断参数是 `max_lines = MAX_SAFE_INTEGER`、`max_bytes = None`（即默认 50 KiB）：
/// 行数上限实际关闭，只有字节上限会触发截断，因此通知里固定写
/// `DEFAULT_MAX_BYTES` 对应的 `50.0KB`。`limit` 本身不在这里裁剪——超出上限的行
/// 仍会全部输出，`limit` 只影响通知与 `details`；真正的条目数限制由 fd 的
/// `--max-results` 或自定义操作实现负责。
///
/// `details` 为空时返回 `JsonValue::Null`（黄金文件 `find_fd` / `find_custom_glob_root`
/// 的 `details` 就是 `null`），而不是空对象。
fn build_result(
    lines: &[String],
    effective_limit: f64,
    limit_notice_has_hint: bool,
) -> AgentToolResult<JsonValue> {
    let result_limit_reached = (lines.len() as f64) >= effective_limit;
    let raw_output = lines.join("\n");
    let truncation = truncate_head(
        &raw_output,
        &TruncationOptions {
            max_lines: Some(max_safe_integer_usize()),
            max_bytes: None,
        },
    );
    let mut output = truncation.content.clone();
    let mut details = serde_json::Map::new();
    let mut notices: Vec<String> = Vec::new();
    if result_limit_reached {
        if limit_notice_has_hint {
            notices.push(format!(
                "{} results limit reached. Use limit={} for more, or refine pattern",
                js_number(effective_limit),
                js_number(effective_limit * 2.0)
            ));
        } else {
            notices.push(format!(
                "{} results limit reached",
                js_number(effective_limit)
            ));
        }
        details.insert(
            "resultLimitReached".to_string(),
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
    if !notices.is_empty() {
        output.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }
    AgentToolResult {
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
    }
}

/// 把 `f64` 按 JS `JSON.stringify` 的数字文本变成 JSON 值（`NaN`/`±Infinity` → `null`）。
fn js_number_value(value: f64) -> JsonValue {
    serde_json::from_str(&kiana_json::js_num::format_js_number(value)).unwrap_or(JsonValue::Null)
}

/// `find` 的 `execute` 实现。
///
/// `cwd` 是创建工具时给出的回退目录：`ctx.cwd()` 为空、或上下文是占位实现
/// （TS 的 `ctx === undefined`）时用它。`operations` 决定执行分支：`Some` 走自定义
/// 操作，`None` 直接 spawn `fd`。
struct FindExecute {
    cwd: String,
    operations: Option<Arc<dyn FindOperations>>,
}

impl ToolDefinitionExecute for FindExecute {
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
            // 已取消的调用不再解析参数：否则参数错误会先于“已中止”返回，
            // 调用方看到的是错误文本而不是取消状态。
            if signal.is_some_and(CancellationToken::is_cancelled) {
                return Err(tool_error("Operation aborted"));
            }
            // 参数不符合 schema 时，serde 的错误文本直接作为工具错误返回；
            // 字段值不做额外校验（limit 的范围见 effective_limit）。
            let input = match serde_json::from_value::<FindToolInput>(params) {
                Ok(input) => input,
                Err(error) => return Err(tool_error(error.to_string())),
            };
            let result = self.run(input, signal, &ctx).await;
            // TS 的 IIFE 外层 catch 先检查 `signal?.aborted`：中止与其它错误同时发生时
            // 上报 "Operation aborted"（find.ts:296-302）。
            match result {
                Err(_) if signal.is_some_and(CancellationToken::is_cancelled) => {
                    Err(tool_error("Operation aborted"))
                }
                result => result,
            }
        })
    }
}

impl FindExecute {
    async fn run(
        &self,
        input: FindToolInput,
        signal: Option<&CancellationToken>,
        ctx: &Arc<dyn ExtensionToolContextApi>,
    ) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
        // 阶段 1：确定搜索根。effective_cwd 处理 ctx.cwd() 为空与占位上下文；
        // 未提供 path 时按 "."（即 cwd）解析。
        let cwd = crate::read::effective_cwd(ctx, &self.cwd);
        let search_path = resolve_to_cwd(input.path.as_deref().unwrap_or("."), &cwd);
        let search_path_text = search_path.to_string_lossy().into_owned();
        let effective_limit = effective_limit(input.limit);

        // 分支条件是「调用方是否提供了自定义操作」（TS 的 `customOps?.glob`），
        // 不是 `ops.glob` 是否存在——默认占位 glob 永远不会被调用。
        if let Some(operations) = &self.operations {
            return self
                .run_custom(operations, &input, &search_path, signal, effective_limit)
                .await;
        }

        // 阶段 2（仅默认分支）：解析 fd。None 表示既不在 PATH、也无法下载/不支持自动
        // 下载（见 kiana-core 的 tools_manager 文档）。这个 await 可被取消；取消会
        // 放弃解析，进行中的下载随 future 中止（可能留下部分归档）。
        let Some(fd_path) = abortable(ensure_tool_without_status("fd"), signal).await else {
            return Err(tool_error("Operation aborted"));
        };
        let Some(fd_path) = fd_path else {
            return Err(tool_error(
                "fd is not available and could not be downloaded",
            ));
        };
        if signal.is_some_and(CancellationToken::is_cancelled) {
            return Err(tool_error("Operation aborted"));
        }

        // 参数顺序是行为契约（集成测试按 argv 逐项断言）：--glob 用 glob 而非正则，
        // --color=never 便于按行解析，--hidden 让隐藏文件参与（忽略规则仍交给 fd）。
        let mut args: Vec<String> = ["--glob", "--color=never", "--hidden"]
            .iter()
            .map(|arg| (*arg).to_string())
            .collect();

        // fd 在 git 仓库外默认忽略 .gitignore，因此仓库外补 `--no-require-git`；
        // 仓库内用 fd 的层级 gitignore 语义（#5960：父级规则在嵌套仓库边界停止）。
        let inside_git_repo = {
            let probe = async {
                let mut inside_git_repo = false;
                let mut current = search_path.clone();
                // 从搜索根沿父链逐级检查 `.git`，直到 `parent()` 为 None 或不再变化
                // （文件系统根）。`path_exists` 只要求 metadata 成功，因此目录形式的
                // `.git` 与 gitfile 都算命中。
                loop {
                    if path_exists(current.join(".git")).await {
                        inside_git_repo = true;
                        break;
                    }
                    let Some(parent) = current.parent() else {
                        break;
                    };
                    if parent == current.as_path() {
                        break;
                    }
                    current = parent.to_path_buf();
                }
                inside_git_repo
            };
            let Some(inside_git_repo) = abortable(probe, signal).await else {
                return Err(tool_error("Operation aborted"));
            };
            inside_git_repo
        };
        if !inside_git_repo {
            args.push("--no-require-git".to_string());
        }
        // 未取整的 limit 原样交给 fd（例如 2.5 → `--max-results 2.5`）。
        args.push("--max-results".to_string());
        args.push(js_number(effective_limit));

        // fd --glob 默认只匹配 basename；含 `/` 的模式需要 --full-path，并补 `**/` 前缀。
        let mut effective_pattern = input.pattern.clone();
        if input.pattern.contains('/') {
            args.push("--full-path".to_string());
            // 以 `/` 开头的模式、以 `**/` 开头的模式与 `**` 本身不再补前缀；
            // 其余模式补 `**/`，使其从任意层级开始匹配。
            if !input.pattern.starts_with('/')
                && !input.pattern.starts_with("**/")
                && input.pattern != "**"
            {
                effective_pattern = format!("**/{}", input.pattern);
            }
            if cfg!(windows) {
                // Windows 上把 `/` 换成 glob 字符类 `[/\\]`，让 fd 用任一分隔符匹配。
                effective_pattern = effective_pattern.replace('/', "[/\\\\]");
            }
        }
        // `--` 结束选项解析，避免以 `-` 开头的模式被 fd 当成选项。
        args.push("--".to_string());
        args.push(effective_pattern);
        args.push(search_path_text.clone());

        // stdin 设为 Ignore（空设备）：不让 fd 继承调用方的终端；stdout/stderr 走管道
        // 以便读取。字段来自 kiana-core 的 SpawnOptions/StdioMode。
        let spawn_options = SpawnOptions {
            stdin: StdioMode::Ignore,
            stdout: StdioMode::Pipe,
            stderr: StdioMode::Pipe,
            ..SpawnOptions::default()
        };
        // 进程起不来（如可执行文件被删除、无执行权限）时，把 io::Error 的文本带进错误。
        let mut child = spawn_process(&fd_path, &args, &spawn_options)
            .map_err(|error| tool_error(format!("Failed to run fd: {}", error)))?;

        // stderr 必须同时读：只读 stdout 时，fd 的错误输出写满 stderr 管道会阻塞。
        // 读取失败（或任务 panic）时用空字符串兜底，不影响主结果。
        let stderr_task = child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                let mut text = Vec::new();
                let _ = stderr.read_to_end(&mut text).await;
                String::from_utf8_lossy(&text).into_owned()
            })
        });
        // spawn 时已指定 stdout 为 Pipe，这里只是防御性检查。
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| tool_error("Failed to run fd: stdout is unavailable"))?;
        let mut lines = LossyLines::new(tokio::io::BufReader::new(stdout));
        let mut raw_lines: Vec<String> = Vec::new();
        // 没有 signal 时用 pending() 占位，让 select! 的两个分支形状统一。
        let abort = async {
            match signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        };
        let mut abort = std::pin::pin!(abort);
        let mut aborted = false;

        // 非法 UTF-8 不再终止读取：`LossyLines` 按 Node readline 的语义替换为 U+FFFD 后继续，
        // 否则子进程写满管道后 `wait()` 会永久阻塞。
        //
        // 先持续读 stdout 直到 EOF / 读取错误 / 取消。取消时 kill 子进程再跳出，
        // 后续仍要 wait() 回收，避免僵尸进程。
        loop {
            tokio::select! {
                _ = abort.as_mut(), if !aborted => {
                    aborted = true;
                    let _ = child.start_kill();
                    break;
                }
                line = lines.next_line() => {
                    match line {
                        Ok(Some(line)) => raw_lines.push(line),
                        Ok(None) | Err(_) => break,
                    }
                }
            }
        }
        // 再等退出码：若取消已发生（aborted = true），abort 分支被 guard 关掉，
        // 只等 child.wait() 返回（kill 是异步的，必须等到进程真正退出并被回收）。
        let status = loop {
            tokio::select! {
                status = child.wait() => break status,
                _ = abort.as_mut(), if !aborted => {
                    aborted = true;
                    let _ = child.start_kill();
                }
            }
        };
        // 子进程已退出，stderr 管道已关闭，读取任务此时可以结束；join 失败（panic）
        // 按空串处理。
        let stderr = match stderr_task {
            Some(task) => task.await.unwrap_or_default(),
            None => String::new(),
        };
        // 取消优先：即使 fd 因被 kill 而退出非 0，也报中止而不是 fd 的错误。
        if aborted || signal.is_some_and(CancellationToken::is_cancelled) {
            return Err(tool_error("Operation aborted"));
        }

        let output = raw_lines.join("\n");
        let exit_code = status.ok().and_then(|status| status.code());
        if exit_code != Some(0) {
            // 非 0 退出时先算出错误文案，但只有完全没输出时才返回错误：
            // 有 stdout 就保留部分结果（测试 `fd_error_with_output_keeps_the_results`）。
            // 文案优先用 JS trim 后非空的 stderr，否则用退出码；被信号杀死时没有
            // code，显示 `fd exited with code null`。
            let stderr = kiana_json::js_string::js_trim(&stderr);
            let error_message = if !stderr.is_empty() {
                stderr.to_string()
            } else {
                format!("fd exited with code {}", js_exit_code(exit_code))
            };
            if output.is_empty() {
                return Err(tool_error(error_message));
            }
        }
        // 结果文本为空（没有读到行，或只有恰好一个空行——join 后都是空串）：
        // 返回固定的空结果文案，details 为 null。
        if output.is_empty() {
            return Ok(AgentToolResult {
                content: vec![text_block("No files found matching pattern")],
                details: JsonValue::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            });
        }

        let path_module = platform_path_module();
        let mut relativized: Vec<String> = Vec::new();
        // 逐行清洗：去掉行尾残余 `\r`，再做 JS `trim`（会去掉包括部分 Unicode 空白
        // 在内的 JS 空白字符），空行跳过。相对化保持 fd 的输出顺序：
        // `relativize_find_result_path` 对相对路径原样保留，对绝对路径按搜索根计算。
        for raw_line in &raw_lines {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            let line = kiana_json::js_string::js_trim(line);
            if line.is_empty() {
                continue;
            }
            relativized.push(relativize_find_result_path(
                line,
                &search_path_text,
                path_module,
            ));
        }
        Ok(build_result(&relativized, effective_limit, true))
    }

    async fn run_custom(
        &self,
        operations: &Arc<dyn FindOperations>,
        input: &FindToolInput,
        search_path: &Path,
        signal: Option<&CancellationToken>,
        effective_limit: f64,
    ) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
        // 自定义分支：不解析/启动 fd，搜索根的存在性由 operations.exists 判断。
        let search_path_text = search_path.to_string_lossy().into_owned();
        // TS 在 `exists` / `glob` 还 pending 时就通过 onAbort 立即 reject（find.ts:97-106）。
        let exists = match abortable(operations.exists(search_path), signal).await {
            Some(Ok(exists)) => exists,
            Some(Err(error)) => return Err(tool_error(error.message)),
            None => return Err(tool_error("Operation aborted")),
        };
        // 默认分支没有这一步：搜索根不存在时由 fd 的退出码/stderr 体现，而不是这里
        // 的统一文案。
        if !exists {
            return Err(tool_error(format!("Path not found: {search_path_text}")));
        }
        if signal.is_some_and(CancellationToken::is_cancelled) {
            return Err(tool_error("Operation aborted"));
        }
        // ignore 固定为 node_modules/.git；limit 原样交给实现。工具不按 limit 裁剪
        // 返回值，超出上限只在 build_result 里报告（见 FindGlobOptions 的文档）。
        let results = match abortable(
            operations.glob(
                &input.pattern,
                search_path,
                FindGlobOptions {
                    ignore: vec!["**/node_modules/**".to_string(), "**/.git/**".to_string()],
                    limit: effective_limit,
                },
            ),
            signal,
        )
        .await
        {
            Some(Ok(results)) => results,
            Some(Err(error)) => return Err(tool_error(error.message)),
            None => return Err(tool_error("Operation aborted")),
        };
        if signal.is_some_and(CancellationToken::is_cancelled) {
            return Err(tool_error("Operation aborted"));
        }
        // 空结果与 fd 分支共用同一文案，details 为 null。
        if results.is_empty() {
            return Ok(AgentToolResult {
                content: vec![text_block("No files found matching pattern")],
                details: JsonValue::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            });
        }
        let path_module = platform_path_module();
        let relativized: Vec<String> = results
            .iter()
            .map(|path| relativize_find_result_path(path, &search_path_text, path_module))
            .collect();
        // 自定义分支的 limit 通知不带 `Use limit=...` 提示（测试
        // `custom_operations_limit_notice_has_no_hint`）。
        Ok(build_result(&relativized, effective_limit, false))
    }
}

// ===========================================================================
// 定义与工厂
// ===========================================================================

/// `createFindToolDefinition(cwd, options?)`。
///
/// 装配完整工具定义：`name` / `label` 都是 `"find"`，`description` 用
/// [`FIND_TOOL_DESCRIPTION`]，`prompt_snippet` 取 [`find_tool_system_prompt_contribution`]，
/// `parameters` 取 [`find_parameters_schema`]，渲染器来自 crate 内的 `find_renderers`。
///
/// `cwd` 只作为执行时 `ctx.cwd()` 为空或占位上下文时的回退，不是固定的搜索目录。
/// `options.operations` 被移动进 `FindExecute`，在每次执行时决定走自定义分支还是
/// fd 分支；其余未列出的字段保持 `None`（`prompt_guidelines` 的原因见函数体内注释）。
pub fn create_find_tool_definition(
    cwd: impl Into<String>,
    options: Option<FindToolOptions>,
) -> ToolDefinition {
    let options = options.unwrap_or_default();
    let contribution = find_tool_system_prompt_contribution();
    let renderers: ToolRenderers = find_renderers();
    ToolDefinition {
        name: "find".to_string(),
        label: "find".to_string(),
        description: FIND_TOOL_DESCRIPTION.to_string(),
        prompt_snippet: Some(contribution.snippet.to_string()),
        // TS 的 `createFindToolDefinition` 不设置 `promptGuidelines`（contribution 常量里是
        // 空数组，但定义对象上该属性是 `undefined`）。
        prompt_guidelines: None,
        parameters: find_parameters_schema(),
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
        execute: Arc::new(FindExecute {
            cwd: cwd.into(),
            operations: options.operations,
        }),
        render_call: renderers.render_call,
        render_result: renderers.render_result,
    }
}

/// `createFindTool(cwd, options?)`。
///
/// 把 [`create_find_tool_definition`] 的结果用 `wrap_tool_definition` 包装成
/// `AgentTool`，不提供上下文工厂。`AgentTool` 的执行接口没有上下文参数，因此每次执行
/// 都会传入占位上下文（`missing_tool_context`），`effective_cwd` 于是总是回落到创建
/// 工具时的 `cwd`；需要按调用方 `ctx.cwd()` 解析时，应直接用
/// [`create_find_tool_definition`] 返回的 `ToolDefinition`。
pub fn create_find_tool(cwd: impl Into<String>, options: Option<FindToolOptions>) -> AgentTool {
    wrap_tool_definition(create_find_tool_definition(cwd, options), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posix(result: &str, search: &str) -> String {
        relativize_find_result_path(result, search, &POSIX_PATH)
    }

    fn win32(result: &str, search: &str) -> String {
        relativize_find_result_path(result, search, &WINDOWS_PATH)
    }

    #[test]
    fn root_paths_keep_the_first_segment() {
        assert_eq!(
            win32("I:\\AI\\Models\\TextGen\\gemma4\\", "I:\\"),
            "AI/Models/TextGen/gemma4/"
        );
        assert_eq!(
            win32("I:/AI/Models/TextGen/gemma4/", "I:\\"),
            "AI/Models/TextGen/gemma4/"
        );
        assert_eq!(win32("I:\\AI\\Models\\", "I:\\AI"), "Models/");
        assert_eq!(
            win32("I:\\AI\\Models2\\file.txt", "I:\\AI\\Models"),
            "../Models2/file.txt"
        );
        assert_eq!(
            win32("AI\\Models\\TextGen\\gemma4\\", "I:\\"),
            "AI/Models/TextGen/gemma4/"
        );
        assert_eq!(posix("/home/user/file.txt", "/"), "home/user/file.txt");
        assert_eq!(posix("/home/user/project/", "/"), "home/user/project/");
        assert_eq!(posix("/home/user/file\\", "/home/user"), "file\\");
        assert_eq!(
            posix("/tmp/results/file.txt", "/workspace/project"),
            "../../tmp/results/file.txt"
        );
        assert_eq!(
            posix("/tmp/results/dir/", "/workspace/project"),
            "../../tmp/results/dir/"
        );
    }

    #[test]
    fn search_root_itself_stays_empty() {
        // `path.posix.relative("/", "/")` 是空串，但结果路径带尾分隔符时 TS 会补回 `/`。
        assert_eq!(posix("/", "/"), "/");
        assert_eq!(posix("/a", "/a"), "");
    }

    #[test]
    fn is_absolute_matches_node() {
        assert!(WINDOWS_PATH.is_absolute("I:\\"));
        assert!(WINDOWS_PATH.is_absolute("I:/x"));
        assert!(!WINDOWS_PATH.is_absolute("I:"));
        assert!(!WINDOWS_PATH.is_absolute("AI\\x"));
        assert!(WINDOWS_PATH.is_absolute("\\x"));
        assert!(!POSIX_PATH.is_absolute("a/b"));
    }

    async fn collect_lines(bytes: &[u8]) -> Vec<String> {
        let mut lines = LossyLines::new(bytes);
        let mut collected = Vec::new();
        while let Some(line) = lines.next_line().await.expect("read line") {
            collected.push(line);
        }
        collected
    }

    #[tokio::test]
    async fn lossy_lines_split_like_node_readline() {
        // Node readline 实测：["a","b","c","d","e"]（`\r`、`\r\n`、`\n` 都是行尾）。
        assert_eq!(
            collect_lines(b"a\rb\r\nc\nd\r\re\r").await,
            ["a", "b", "c", "d", "", "e"]
        );
    }

    #[tokio::test]
    async fn lossy_lines_replace_invalid_utf8_and_keep_going() {
        assert_eq!(
            collect_lines(b"bad\xFF.txt\ngood.txt\r\nend").await,
            ["bad\u{FFFD}.txt", "good.txt", "end"]
        );
        assert_eq!(collect_lines(b"").await, Vec::<String>::new());
        assert_eq!(collect_lines(b"a\n").await, ["a"]);
    }
}
