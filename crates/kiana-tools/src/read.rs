//! core/tools/read.ts。
//!
//! `read` 工具：文本按行/字节双重截断，图片按魔数识别后交给
//! [`kiana_core::image_process::process_image`]；返回值同时给出面向模型的 content 与
//! 面向程序调用者的 `structuredContent`。
//!
//! # 跨文件契约
//!
//! [`ToolOperationError`] 是 read/write/edit/ls 四个工具的自定义操作错误类型：
//! TS 的 operations 可以抛任意 `unknown`，`edit` 还会读取 `error.code` 决定
//! `Error code: ENOENT` 文案。这里保留 `message` 与可选的 `code`；默认操作把
//! `std::io::Error` 的 errno 映射成 libuv 的 `{code, description}`，消息逐字复刻
//! Node 的 `uvException()`：`ENOENT: no such file or directory, open 'x'`。
//! syscall 名（`open`/`read`/`access`/`stat`/`scandir`/`mkdir`/`write`）与是否带
//! path 取决于失败发生在哪个系统调用：`fs.readFile` 的 read 阶段带 path，
//! `FileHandle.read` 的 read 阶段不带，写阶段（`ENOSPC` 等）不带。
//!
//! # JS 语义
//!
//! - `offset ? Math.max(0, offset - 1) : 0`：`0`/`-0`/`NaN` 走 0 分支；负数经 `Math.max` 变 0。
//! - `Array.prototype.slice` 接受小数与负索引：实现见 `js_slice`（`ToIntegerOrInfinity`）。
//! - 行号与剩余行数在输出里用 JS `Number::toString` 格式化（小数 offset 会原样出现）。
//! - `Buffer.toString("utf-8")` 用 WHATWG maximal-subpart 规则：`encoding_rs` 的
//!   `UTF_8.decode_without_bom_handling`，不做 BOM 剥离（BOM 是文本的一部分）。
//! - 每个 `await` 之后检查中止（read/ls 用 `select!` 立即中止，对应 TS 在 abort
//!   监听里直接 reject 的行为）。
//!
//! # 职责与边界
//!
//! 本文件实现完整的 `read` 工具：参数 schema、输入/输出类型、图片与文本两条执行分支，
//! 以及一个可替换的文件系统后端 [`ReadOperations`]。工具本身不接触会话或 UI：
//!
//! - 上游：`crate::index` 的按名创建函数/工具集合用 `ToolName::Read` 构造本工具；
//!   `kiana-session` 与扩展桥再把它注册进 agent loop。
//! - 下游：路径解析交给 [`crate::path_utils::resolve_read_path_async`]（含 macOS 命名变体
//!   回退），文本截断交给 [`crate::truncate::truncate_head`]，UTF-8 解码交给
//!   `crate::edit_diff::decode_utf8`，图片规范化/缩放交给
//!   [`kiana_core::image_process::process_image`]。
//! - 输入/详情类型 [`ReadToolInput`] / [`ReadToolDetails`] 定义在 `kiana-ext-api` 的
//!   `tool_types`（扩展 API 的共享类型模块），本模块只 re-export。
//! - [`ToolOperationError`]（TS 的 read/write/edit/ls 共享错误类型）在 Rust 侧还被
//!   grep/bash 与 `crate::file_mutation_queue` 复用；改动它的消息拼装会同时影响这些调用方。
//!
//! # 一次执行的流程
//!
//! 1. `execute`：已取消的请求直接以 `Operation aborted` 失败；其余反序列化为
//!    [`ReadToolInput`]，并保留原始 JSON（`offset`/`limit` 需要区分缺省与 null）。
//! 2. 解析 cwd 与路径，再做可读性预检（默认实现 unix 上是 `access(2)`）。
//! 3. 用魔数嗅探决定分支：命中受支持的图片 → 整文件读取后交给 `process_image`；
//!    否则整文件读取、按 WHATWG UTF-8 解码、以 `\n` 切行。
//! 4. 文本分支按 offset/limit 的 JS 语义选行，再交给 `truncate_head`（默认 2000 行 /
//!    50 KiB）截断，并拼接当前稳定格式的脚注（改动需同步测试）。
//! 5. 组装 [`AgentToolResult`]：`content` 给模型；`structuredContent` 由
//!    [`to_read_output`] 从 content 投影；`details` 只在发生截断时携带
//!    [`ReadToolDetails`]。
//!
//! # 注意事项
//!
//! - 文本分支是“整文件读入内存 + 整体解码 + 收集全部行”，`offset`/`limit` 只减少输出，
//!   不减少读取与解码量。
//! - 所有文件系统错误最终只保留 `message`（[`AgentToolError`] 的 `name`/`stack` 为
//!   `None`）；唯一的例外是小数 offset 命中超长首行时模拟的 `TypeError`。
//! - 取消通过 `tokio::select!` 丢弃 `run` 的 future 实现，后端方法拿不到取消令牌。
//! - 魔数嗅探默认走同步 `std::fs`（只读文件前 4100 字节），没有 await 点；取消只能在
//!   该同步调用返回后才被观察到。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::SystemTime;

use async_trait::async_trait;
use kiana_agent_core::{AgentTool, AgentToolError, AgentToolResult};
use kiana_ai::types::{
    ConstrainedSampling, ImageContent, ImageKind, JsonValue, Model, ModelImageResizeOptions,
    TextContent, TextKind, UserContentBlock,
};
use kiana_core::file_read_cache::{
    DEFAULT_PER_FILE_CHARS, FileReadSnapshot, ReadLineRange, truncate_chars,
};
use kiana_core::image_process::{
    ImageResizeOptions, ProcessImageOptions, ProcessImageResult, process_image,
};
pub use kiana_ext_api::tool_types::{ReadToolDetails, ReadToolInput};
use kiana_ext_api::tool_types::{ToolDefinition, ToolDefinitionExecute, ToolRenderers};
use kiana_ext_api::types::ExtensionToolContextApi;
use tokio_util::sync::CancellationToken;

use crate::definition::{is_missing_tool_context, wrap_tool_definition};
use crate::path_utils::resolve_read_path_async;
use crate::renderers::read::read_renderers;
use crate::truncate::{DEFAULT_MAX_BYTES, TruncationOptions, format_size, truncate_head};

/// `readToolSystemPromptContribution` 的 Rust 形状。
///
/// 两个字段会原样进入 [`ToolDefinition`] 的 `prompt_snippet` 与 `prompt_guidelines`：
/// 前者是系统提示 “Available tools” 段的一行摘要，后者追加到 Guidelines 段。
/// 这些字符串是模型可见的冻结文案，修改会直接改变提示内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadToolSystemPromptContribution {
    /// 系统提示 "Available tools" 段的一行摘要。
    pub snippet: &'static str,
    /// 追加到 Guidelines 段的条目。
    pub guidelines: &'static [&'static str],
}

/// `readToolSystemPromptContribution`。
///
/// 返回值全部是 `&'static str`，每次调用不分配；[`create_read_tool_definition`] 用它填充
/// 工具定义的提示元数据。
pub fn read_tool_system_prompt_contribution() -> ReadToolSystemPromptContribution {
    ReadToolSystemPromptContribution {
        snippet: "Read file contents",
        guidelines: &["Use read to examine files instead of cat or sed."],
    }
}

/// `read` 的工具描述（逐字）。
pub const READ_TOOL_DESCRIPTION: &str = "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.";

/// `[Current model does not support images. The image will be omitted from this request.]`
pub const NON_VISION_IMAGE_NOTE: &str =
    "[Current model does not support images. The image will be omitted from this request.]";

// ===========================================================================
// 操作错误
// ===========================================================================

/// 工具文件系统操作的错误：消息 + 可选的 Node 错误码。
///
/// `message` 由 `operation_error` 逐字搬进 [`AgentToolError::message`]；`code` 不进入工具
/// 错误，被 `edit` 与 `edit_diff` 的文案逻辑用来区分 `Error code: ENOENT` 与
/// `Error: <message>`（执行路径见 `crate::edit::format_edit_access_error`，
/// `compute_edits_diff` 预览路径见 `crate::edit_diff::access_error_message`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOperationError {
    /// 用户可见的错误消息。
    pub message: String,
    /// Node `err.code`（例如 `ENOENT` / `EACCES`）；自定义操作通常没有。
    pub code: Option<String>,
}

/// `fs.constants.R_OK`。
#[cfg(unix)]
pub(crate) const R_OK: i32 = libc::R_OK;
/// `fs.constants.W_OK`。
#[cfg(unix)]
pub(crate) const W_OK: i32 = libc::W_OK;
/// 非 unix 平台的值（POSIX 定义，`access` 的语义由调用方保证）。
#[cfg(not(unix))]
pub(crate) const R_OK: i32 = 4;
/// 非 unix 平台的值。
#[cfg(not(unix))]
pub(crate) const W_OK: i32 = 2;

/// libuv 的 errno → `(uv_err_name, uv_strerror)` 表（Node 的 `Error.message` 由它拼出）。
///
/// Node 通过 `uvException()` 生成 `${code}: ${description}, ${syscall} '${path}'`；
/// `uv_strerror` 的文案与 `strerror(3)` 不同（例如 `EBUSY` 是 "resource busy or locked"），
/// 未收录的 errno 返回 `UNKNOWN: unknown error`。
#[cfg(unix)]
fn errno_details(errno: i32) -> (&'static str, &'static str) {
    match errno {
        libc::EPERM => ("EPERM", "operation not permitted"),
        libc::ENOENT => ("ENOENT", "no such file or directory"),
        libc::EIO => ("EIO", "i/o error"),
        libc::ENXIO => ("ENXIO", "no such device or address"),
        libc::EBADF => ("EBADF", "bad file descriptor"),
        libc::EAGAIN => ("EAGAIN", "resource temporarily unavailable"),
        libc::ENOMEM => ("ENOMEM", "not enough memory"),
        libc::EACCES => ("EACCES", "permission denied"),
        libc::EFAULT => ("EFAULT", "bad address in system call argument"),
        libc::EBUSY => ("EBUSY", "resource busy or locked"),
        libc::EEXIST => ("EEXIST", "file already exists"),
        libc::EXDEV => ("EXDEV", "cross-device link not permitted"),
        libc::ENODEV => ("ENODEV", "no such device"),
        libc::ENOTDIR => ("ENOTDIR", "not a directory"),
        libc::EISDIR => ("EISDIR", "illegal operation on a directory"),
        libc::EINVAL => ("EINVAL", "invalid argument"),
        libc::ENFILE => ("ENFILE", "file table overflow"),
        libc::EMFILE => ("EMFILE", "too many open files"),
        libc::ENOTTY => ("ENOTTY", "inappropriate ioctl for device"),
        libc::ETXTBSY => ("ETXTBSY", "text file is busy"),
        libc::EFBIG => ("EFBIG", "file too large"),
        libc::ENOSPC => ("ENOSPC", "no space left on device"),
        libc::ESPIPE => ("ESPIPE", "invalid seek"),
        libc::EROFS => ("EROFS", "read-only file system"),
        libc::EMLINK => ("EMLINK", "too many links"),
        libc::EPIPE => ("EPIPE", "broken pipe"),
        libc::EDOM => ("EDOM", "argument outside domain"),
        libc::ERANGE => ("ERANGE", "result too large"),
        libc::EDEADLK => ("EDEADLK", "resource deadlock would occur"),
        libc::ENAMETOOLONG => ("ENAMETOOLONG", "name too long"),
        libc::ENOLCK => ("ENOLCK", "no locks available"),
        libc::ENOSYS => ("ENOSYS", "function not implemented"),
        libc::ENOTEMPTY => ("ENOTEMPTY", "directory not empty"),
        libc::ELOOP => ("ELOOP", "too many symbolic links encountered"),
        libc::EOVERFLOW => ("EOVERFLOW", "value too large for defined data type"),
        libc::EILSEQ => ("EILSEQ", "illegal byte sequence"),
        libc::ENOTSOCK => ("ENOTSOCK", "socket operation on non-socket"),
        libc::EDESTADDRREQ => ("EDESTADDRREQ", "destination address required"),
        libc::EMSGSIZE => ("EMSGSIZE", "message too long"),
        libc::EPROTOTYPE => ("EPROTOTYPE", "protocol wrong type for socket"),
        libc::ENOPROTOOPT => ("ENOPROTOOPT", "protocol not available"),
        libc::EPROTONOSUPPORT => ("EPROTONOSUPPORT", "protocol not supported"),
        libc::EOPNOTSUPP => ("EOPNOTSUPP", "operation not supported on socket"),
        libc::EAFNOSUPPORT => ("EAFNOSUPPORT", "address family not supported"),
        libc::EADDRINUSE => ("EADDRINUSE", "address already in use"),
        libc::EADDRNOTAVAIL => ("EADDRNOTAVAIL", "address not available"),
        libc::ENETDOWN => ("ENETDOWN", "network is down"),
        libc::ENETUNREACH => ("ENETUNREACH", "network is unreachable"),
        libc::ECONNABORTED => ("ECONNABORTED", "software caused connection abort"),
        libc::ECONNRESET => ("ECONNRESET", "connection reset by peer"),
        libc::ENOBUFS => ("ENOBUFS", "no buffer space available"),
        libc::EISCONN => ("EISCONN", "socket is already connected"),
        libc::ENOTCONN => ("ENOTCONN", "socket is not connected"),
        libc::ESHUTDOWN => ("ESHUTDOWN", "cannot send after transport endpoint shutdown"),
        libc::ETIMEDOUT => ("ETIMEDOUT", "connection timed out"),
        libc::ECONNREFUSED => ("ECONNREFUSED", "connection refused"),
        libc::EHOSTUNREACH => ("EHOSTUNREACH", "host is unreachable"),
        libc::EALREADY => ("EALREADY", "connection already in progress"),
        libc::EINPROGRESS => ("EINPROGRESS", "operation in progress"),
        libc::ECANCELED => ("ECANCELED", "operation canceled"),
        _ => ("UNKNOWN", "unknown error"),
    }
}

/// 没有 errno 时按 [`std::io::ErrorKind`] 回退（文案与 libuv 表一致）。
fn error_kind_details(kind: std::io::ErrorKind) -> Option<(&'static str, &'static str)> {
    use std::io::ErrorKind;

    let details = match kind {
        ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        ErrorKind::AlreadyExists => ("EEXIST", "file already exists"),
        ErrorKind::InvalidInput => ("EINVAL", "invalid argument"),
        ErrorKind::InvalidData => ("EINVAL", "invalid argument"),
        ErrorKind::WriteZero => ("EIO", "i/o error"),
        ErrorKind::UnexpectedEof => ("EIO", "i/o error"),
        ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory"),
        ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
        ErrorKind::ReadOnlyFilesystem => ("EROFS", "read-only file system"),
        ErrorKind::StorageFull => ("ENOSPC", "no space left on device"),
        ErrorKind::FileTooLarge => ("EFBIG", "file too large"),
        ErrorKind::ResourceBusy => ("EBUSY", "resource busy or locked"),
        ErrorKind::TooManyLinks => ("EMLINK", "too many links"),
        ErrorKind::DirectoryNotEmpty => ("ENOTEMPTY", "directory not empty"),
        ErrorKind::Deadlock => ("EDEADLK", "resource deadlock would occur"),
        ErrorKind::OutOfMemory => ("ENOMEM", "not enough memory"),
        _ => return None,
    };
    Some(details)
}

/// errno（优先）或 `ErrorKind` 对应的 Node `{code, description}`。
fn node_error_details(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    #[cfg(unix)]
    if let Some(errno) = error.raw_os_error() {
        return Some(errno_details(errno));
    }
    error_kind_details(error.kind())
}

/// 错误是否发生在 `read(2)` 阶段。只有目录会让 `open(2)` 成功、`read(2)` 失败（EISDIR）。
fn is_read_stage_error(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::EISDIR) {
        return true;
    }
    error.kind() == std::io::ErrorKind::IsADirectory
}

impl ToolOperationError {
    /// 只有消息（等价 TS 里没有 `code` 的 Error）。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }

    /// 带 Node 错误码。
    pub fn with_code(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: Some(code.into()),
        }
    }

    /// 把 `std::io::Error` 映射成 Node 风格的错误（`ENOENT: no such file or directory, open 'x'`）。
    ///
    /// `operation` 是 Node 的 syscall 名（`open`/`read`/`access`/`stat`/…），会原样出现在
    /// 消息里；路径也用 `Path::display()` 原样拼入。
    pub fn from_io(operation: &str, path: &Path, error: std::io::Error) -> Self {
        Self::from_io_parts(operation, Some(path), error)
    }

    /// 同 [`ToolOperationError::from_io`]，但错误消息不带路径（Node 里不带 path 的 syscall，
    /// 例如 `FileHandle.read` 的 `EISDIR: illegal operation on a directory, read`）。
    pub fn from_io_without_path(operation: &str, error: std::io::Error) -> Self {
        Self::from_io_parts(operation, None, error)
    }

    /// 逐字复刻 Node `uvException()`：`{code}: {description}, {syscall} '{path}'`。
    ///
    /// 无法识别的 errno/`ErrorKind` 回落为 `error.to_string()` 且不设 `code`——此时消息
    /// 不再是 Node 格式，只保留底层错误文本。
    pub fn from_io_parts(operation: &str, path: Option<&Path>, error: std::io::Error) -> Self {
        let Some((code, description)) = node_error_details(&error) else {
            return Self {
                message: error.to_string(),
                code: None,
            };
        };
        let mut message = format!("{code}: {description}, {operation}");
        if let Some(path) = path {
            message.push_str(&format!(" '{}'", path.display()));
        }
        Self::with_code(code, message)
    }

    /// `fs.readFile`：`open(2)` 与 `read(2)` 阶段的错误都带 path。
    pub fn from_read_file_io(path: &Path, error: std::io::Error) -> Self {
        let operation = if is_read_stage_error(&error) {
            "read"
        } else {
            "open"
        };
        Self::from_io(operation, path, error)
    }

    /// `FileHandle.read`（图片魔数嗅探）：只有 `open(2)` 阶段的错误带 path。
    pub fn from_file_handle_io(path: &Path, error: std::io::Error) -> Self {
        if is_read_stage_error(&error) {
            Self::from_io_without_path("read", error)
        } else {
            Self::from_io("open", path, error)
        }
    }
}

/// `fs.access(path, mode)`：直接调用 `access(2)`。
///
/// Node/libuv 的 `fs.access` 就是 `access(2)`：对目录只检查权限位，不会像
/// `open(O_RDWR)` 那样因 `EISDIR` 失败；错误消息的 syscall 是 `access`。
pub(crate) fn access_path(path: &Path, mode: i32) -> Result<(), ToolOperationError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let raw = path.as_os_str().as_bytes();
        // Node 在进入 syscall 前就拒绝含 NUL 的路径；Rust 的 OsStr 允许内嵌 NUL，
        // 这里在构造 C 字符串失败时给出同一文案。
        let Ok(c_path) = std::ffi::CString::new(raw) else {
            return Err(invalid_path_error(path));
        };
        // SAFETY: `c_path` 是以 NUL 结尾的合法 C 字符串，`access(2)` 不保留指针。
        if unsafe { libc::access(c_path.as_ptr(), mode) } == 0 {
            return Ok(());
        }
        Err(ToolOperationError::from_io(
            "access",
            path,
            std::io::Error::last_os_error(),
        ))
    }
    #[cfg(not(unix))]
    {
        // 非 unix 没有 access(2)：先 stat，再对普通文件用 open 探测读/写；目录直接放行
        // （不检查 mode，权限语义与 unix 分支不完全相同）。错误消息仍以 access 为 syscall。
        let metadata = std::fs::metadata(path)
            .map_err(|error| ToolOperationError::from_io("access", path, error))?;
        if metadata.is_file() {
            if mode & R_OK != 0 {
                std::fs::File::open(path)
                    .map_err(|error| ToolOperationError::from_io("access", path, error))?;
            }
            if mode & W_OK != 0 {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .map_err(|error| ToolOperationError::from_io("access", path, error))?;
            }
        }
        Ok(())
    }
}

/// Node 在 syscall 之前用 `ERR_INVALID_ARG_VALUE` 拒绝含 NUL 的路径。
///
/// 同时被 [`crate::file_mutation_queue`] 的严格 key 计算复用（TS 的 `realpath` 会先做
/// `validatePath`）。
pub(crate) fn invalid_path_error(path: &Path) -> ToolOperationError {
    let rendered = path.to_string_lossy().replace('\0', "\\x00");
    ToolOperationError::with_code(
        "ERR_INVALID_ARG_VALUE",
        format!(
            "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received '{rendered}'"
        ),
    )
}

// ===========================================================================
// read-before-write：会话级读记录
// ===========================================================================
//
// 设计参考（只借鉴行为，不复制代码）：
// - qwen-code `services/fileReadCache.ts` + `tools/priorReadEnforcement.ts`：
//   以 `(dev, ino)` 作为文件身份、记录 `(mtime, size)` 指纹、`ino == 0` 时 fail-closed、
//   读取失败一律拒绝而不是猜测「大概率没变」。
// - Claude Code `readFileState`：任意一次成功读取即可解锁写入门禁；截断读只覆盖
//   实际展示过的行范围。
//
// 与参考实现的差异（Kiana 自己加严的部分）：
// - 指纹除 `(mtime, size)` 外还包含**内容哈希**：mtime/size 相同但内容被替换时仍能
//   识别为 `FileChangedSinceRead`。
// - 记录精确的 1-based 行范围：截断读（`offset`/`limit`/`truncate_head`）只允许修改
//   看到过的行；`write`（整文件覆盖）要求记录覆盖整个文件。
// - 成功写入后旧的读快照立即失效，并用**写出的字节**建立新基线（等价 qwen 的
//   `recordWrite`）：这既避免模型用旧快照继续修改，也让队列内排队等待的第二个
//   并行写看到「刚写完」的当前内容，而不是被自己的上一个写误判为外部修改。

/// 一段 1-based、闭区间的行范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LineRange {
    /// 起始行（1-based）。
    pub start: u64,
    /// 结束行（1-based，含）。
    pub end: u64,
}

impl LineRange {
    /// 构造一个范围；`end < start` 时归一为只含 `start` 的一行。
    pub fn new(start: u64, end: u64) -> Self {
        Self {
            start,
            end: end.max(start),
        }
    }

    /// 是否覆盖 `start..=end`。
    pub fn contains(&self, start: u64, end: u64) -> bool {
        self.start <= start && end <= self.end
    }
}

/// 一次成功读取（或写入）留下的文件快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRecord {
    /// 设备号。
    pub dev: u64,
    /// inode 号（`0` 表示文件系统不提供可验证身份）。
    pub ino: u64,
    /// 归一化后的真实路径（诊断与按路径失效用）。
    pub real_path: PathBuf,
    /// 记录时的 mtime。
    pub mtime: Option<SystemTime>,
    /// 记录时的文件大小（字节）。
    pub size: u64,
    /// 记录内容的 FNV-1a 64 位哈希。
    pub content_hash: u64,
    /// 内容按 `\n` 切分的行数（非文本读取为 0）。
    pub line_count: u64,
    /// 模型实际看到过的行范围。
    pub ranges: Vec<LineRange>,
    /// 这次读取是否是可编辑的文本（图片等多模态读取为 `false`）。
    pub text: bool,
    /// 记录创建/刷新时间（Unix 毫秒）；压缩恢复按它选“最近 N 个文件”。
    pub captured_at_ms: i64,
}

impl ReadRecord {
    /// 记录是否覆盖 `start..=end` 行。
    pub fn covers(&self, start: u64, end: u64) -> bool {
        self.ranges.iter().any(|range| range.contains(start, end))
    }

    /// 记录是否覆盖整个文件。
    pub fn covers_all_lines(&self) -> bool {
        self.text && self.covers(1, self.line_count.max(1))
    }
}

/// 压缩后恢复读记录的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// 指纹匹配，读记录已恢复（写入门禁放行）。
    Restored,
    /// 文件已不存在。
    Missing,
    /// 文件内容与快照不一致（编辑会被门禁拒绝，直到重新 read）。
    Changed(String),
    /// 无法验证（权限、inode 不可用等）。
    Unverifiable(String),
}

/// 写入门禁的检查结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriorReadStatus {
    /// 有当前读记录，且磁盘指纹仍然匹配。
    Fresh(ReadRecord),
    /// 本会话从未读取过该路径。
    Missing,
    /// 读记录存在，但文件已被外部修改/替换。
    Changed(String),
    /// 无法验证（stat/读失败、inode 不可用、不是普通文件）。
    Unverifiable(String),
    /// 读过，但内容不是可编辑文本。
    NotText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

#[derive(Debug, Default)]
struct ReadStateMap {
    by_identity: HashMap<FileIdentity, ReadRecord>,
    by_path: HashMap<PathBuf, FileIdentity>,
}

/// 会话级读记录表。
///
/// 生产路径使用进程级共享实例（[`ReadStateRegistry::global`]）；测试可以构造独立实例，
/// 避免并行测试互相污染。
#[derive(Debug, Default)]
pub struct ReadStateRegistry {
    inner: Mutex<ReadStateMap>,
}

/// 进程级共享读记录表。
static GLOBAL_READ_STATE: LazyLock<Arc<ReadStateRegistry>> =
    LazyLock::new(|| Arc::new(ReadStateRegistry::new()));

/// 单表最多保留的快照数；超出后淘汰任意一项（读多写少，容量不是正确性问题）。
const MAX_READ_RECORDS: usize = 4096;

impl ReadStateRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 进程级共享注册表。
    pub fn global() -> Arc<Self> {
        Arc::clone(&GLOBAL_READ_STATE)
    }

    fn lock_state(&self) -> MutexGuard<'_, ReadStateMap> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 导出最近的读取快照（最多 `limit` 个，按 `captured_at_ms` 从新到旧）。
    ///
    /// 压缩前调用，把结果交给 `kiana-compaction` 的恢复预算；压缩后由
    /// [`Self::restore_snapshot`] 重新 arm read gate。快照里的 `content_excerpt`
    /// 只用于恢复提示，最多 [`DEFAULT_PER_FILE_CHARS`] 字符。
    pub fn snapshots(&self, limit: usize) -> Vec<FileReadSnapshot> {
        let records: Vec<ReadRecord> = {
            let map = self.lock_state();
            map.by_identity.values().cloned().collect()
        };
        let mut records = records;
        records.sort_by_key(|left| std::cmp::Reverse(left.captured_at_ms));
        records.truncate(limit);
        records
            .into_iter()
            .map(|record| {
                let excerpt = read_prefix(&record.real_path, SNAPSHOT_PREFIX_BYTES)
                    .ok()
                    .map(|bytes| {
                        truncate_chars(&String::from_utf8_lossy(&bytes), DEFAULT_PER_FILE_CHARS)
                    })
                    .unwrap_or_default();
                FileReadSnapshot {
                    path: record.real_path.clone(),
                    content_hash: record.content_hash,
                    line_count: record.line_count,
                    ranges: record
                        .ranges
                        .iter()
                        .map(|range| ReadLineRange::new(range.start, range.end))
                        .collect(),
                    text: record.text,
                    captured_at_ms: record.captured_at_ms,
                    content_excerpt: excerpt,
                }
            })
            .collect()
    }

    /// 用压缩前的快照重新 arm read gate。
    ///
    /// 只有当磁盘当前内容与快照指纹一致时才恢复，避免把“模型看过旧版本”伪装成
    /// “看过新版本”。文件缺失/被改动时返回对应 outcome（调用方只 warning，不阻塞
    /// 压缩）；未恢复的路径会被写入门禁拒绝，直到模型重新 `read`。
    pub fn restore_snapshot(&self, snapshot: &FileReadSnapshot) -> RestoreOutcome {
        let digest = match fingerprint_file(&snapshot.path) {
            Ok(digest) => digest,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return RestoreOutcome::Missing;
            }
            Err(error) => return RestoreOutcome::Unverifiable(error.to_string()),
        };
        if digest.hash != snapshot.content_hash {
            return RestoreOutcome::Changed(snapshot.path.display().to_string());
        }
        let ranges = snapshot
            .ranges
            .iter()
            .map(|range| LineRange::new(range.start, range.end))
            .collect();
        let line_count = if snapshot.text { digest.line_count } else { 0 };
        match self.store_digest(
            &snapshot.path,
            digest.hash,
            line_count,
            ranges,
            snapshot.text,
        ) {
            Some(_) => RestoreOutcome::Restored,
            None => RestoreOutcome::Unverifiable("read record was not storable".to_string()),
        }
    }

    /// 记录一次成功读取。
    ///
    /// `contents` 是读取到的**原始字节**（文本分支含 BOM/非法 UTF-8 也按原样哈希）；
    /// `ranges` 是模型实际看到的行范围；`text` 表示内容可作为文本编辑。路径不存在、
    /// 不是普通文件、或文件系统不提供 inode（`ino == 0`）时不记录（写入门禁随后会
    /// fail-closed）。
    pub fn record_read(
        &self,
        absolute_path: &Path,
        contents: &[u8],
        ranges: Vec<LineRange>,
        text: bool,
    ) -> Option<ReadRecord> {
        self.store(absolute_path, contents, ranges, text)
    }

    /// 便捷入口：按「整文件文本读取」记录（测试与工具辅助用）。
    pub fn record_full_read(&self, absolute_path: &Path) -> Option<ReadRecord> {
        let contents = std::fs::read(absolute_path).ok()?;
        let line_count = count_lines(&contents);
        self.record_read(
            absolute_path,
            &contents,
            vec![LineRange::new(1, line_count)],
            true,
        )
    }

    /// 记录一次成功写入。
    ///
    /// 写出的字节成为新的基线；`full` 为真时记录覆盖整个文件（`write` 或读记录已覆盖
    /// 全文件时的 `edit`），否则只清空范围、要求后续操作重新 `read`。
    pub fn record_write(
        &self,
        absolute_path: &Path,
        contents: &[u8],
        full: bool,
    ) -> Option<ReadRecord> {
        let line_count = count_lines(contents);
        let ranges = if full {
            vec![LineRange::new(1, line_count)]
        } else {
            Vec::new()
        };
        self.store(absolute_path, contents, ranges, true)
    }

    /// 记录一次**部分覆盖**的写入。
    ///
    /// `ranges` 是这次写入真正让模型看到的**新文件**行范围（1-based、闭区间）；只
    /// 有这些行会被视为已读，其余行仍需重新 `read` 才能编辑。`apply_patch` 用它把
    /// 补丁里的新增行登记为已读，避免“刚改完马上 edit 同一行”被误拒，同时不放开
    /// 补丁从未涉及的区域。
    pub fn record_write_ranges(
        &self,
        absolute_path: &Path,
        contents: &[u8],
        ranges: Vec<LineRange>,
    ) -> Option<ReadRecord> {
        self.store(absolute_path, contents, ranges, true)
    }

    fn store(
        &self,
        absolute_path: &Path,
        contents: &[u8],
        ranges: Vec<LineRange>,
        text: bool,
    ) -> Option<ReadRecord> {
        let content_hash = fingerprint_bytes(contents);
        let line_count = if text { count_lines(contents) } else { 0 };
        self.store_digest(absolute_path, content_hash, line_count, ranges, text)
    }

    /// 与 [`Self::store`] 相同，但使用调用方预先算好的内容指纹与行数。
    ///
    /// 压缩恢复只需要校验指纹，不应为了重新计算哈希把大文件整体读进内存；调用方用
    /// [`fingerprint_file`] 流式计算后再走这里。
    fn store_digest(
        &self,
        absolute_path: &Path,
        content_hash: u64,
        line_count: u64,
        ranges: Vec<LineRange>,
        text: bool,
    ) -> Option<ReadRecord> {
        let real_path = kiana_sandbox::resolve_real_path(absolute_path).ok()?;
        let metadata = std::fs::symlink_metadata(&real_path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        let (dev, ino) = metadata_file_identity(&metadata);
        if ino == 0 {
            // FAT/exFAT/SMB 等文件系统不提供可靠 inode；宁可让写入门禁拒绝，也不猜。
            return None;
        }
        let identity = FileIdentity { dev, ino };
        let mtime = metadata.modified().ok();
        let size = metadata.len();
        let ranges = normalize_ranges(ranges, line_count);

        let mut map = self.lock_state();
        // 同一路径现在指向不同 inode：旧快照已经失效（文件被替换）。
        if let Some(previous) = map.by_path.get(&real_path).copied()
            && previous != identity
        {
            map.by_identity.remove(&previous);
        }

        let record = match map.by_identity.get(&identity) {
            Some(existing)
                if existing.size == size
                    && existing.mtime == mtime
                    && existing.content_hash == content_hash =>
            {
                // 同一份字节的再次读取：保留既有范围（sticky 语义），仅并集新范围。
                let mut merged = existing.ranges.clone();
                if text {
                    for range in ranges {
                        merge_range(&mut merged, range);
                    }
                }
                ReadRecord {
                    dev,
                    ino,
                    real_path: real_path.clone(),
                    mtime,
                    size,
                    content_hash,
                    line_count,
                    ranges: merged,
                    text: existing.text || text,
                    captured_at_ms: now_epoch_millis(),
                }
            }
            _ => ReadRecord {
                dev,
                ino,
                real_path: real_path.clone(),
                mtime,
                size,
                content_hash,
                line_count,
                ranges,
                text,
                captured_at_ms: now_epoch_millis(),
            },
        };

        if map.by_identity.len() >= MAX_READ_RECORDS
            && !map.by_identity.contains_key(&identity)
            && let Some(oldest) = map.by_identity.keys().next().copied()
        {
            map.by_identity.remove(&oldest);
            // `by_path` 只做路径 → 身份的索引；身份被淘汰后必须同步清除，否则下次读取
            // 该路径会命中指向已删除记录的索引（或被误判为同一文件）。
            map.by_path.retain(|_, identity| *identity != oldest);
        }
        map.by_path.insert(real_path, identity);
        map.by_identity.insert(identity, record.clone());
        Some(record)
    }

    /// 检查写入门禁。
    ///
    /// 顺序固定：先解析真实路径（符号链接/大小写），再 stat，再比较文件身份与
    /// `(mtime, size, content hash)`。任何无法验证的情况都返回拒绝信号，绝不默认放行。
    pub fn check_prior_read(&self, absolute_path: &Path) -> PriorReadStatus {
        let real_path = match kiana_sandbox::resolve_real_path(absolute_path) {
            Ok(path) => path,
            Err(error) => {
                return PriorReadStatus::Unverifiable(format!(
                    "cannot resolve {}: {error}",
                    absolute_path.display()
                ));
            }
        };
        let metadata = match std::fs::symlink_metadata(&real_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let map = self.lock_state();
                return if map.by_path.contains_key(&real_path) {
                    PriorReadStatus::Changed("the file disappeared after it was read".to_string())
                } else {
                    PriorReadStatus::Missing
                };
            }
            Err(error) => {
                return PriorReadStatus::Unverifiable(format!(
                    "cannot stat {}: {error}",
                    real_path.display()
                ));
            }
        };
        if !metadata.is_file() {
            return PriorReadStatus::Unverifiable(format!(
                "{} is not a regular file",
                real_path.display()
            ));
        }
        let (dev, ino) = metadata_file_identity(&metadata);
        if ino == 0 {
            return PriorReadStatus::Unverifiable(format!(
                "{} does not expose a verifiable inode identity",
                real_path.display()
            ));
        }
        let identity = FileIdentity { dev, ino };
        let (record, path_known) = {
            let map = self.lock_state();
            (
                map.by_identity.get(&identity).cloned(),
                map.by_path.contains_key(&real_path),
            )
        };
        let Some(record) = record else {
            return if path_known {
                PriorReadStatus::Changed(
                    "the file was replaced since it was read (inode changed)".to_string(),
                )
            } else {
                PriorReadStatus::Missing
            };
        };

        if record.size != metadata.len() {
            return PriorReadStatus::Changed(format!(
                "size changed from {} to {} bytes",
                record.size,
                metadata.len()
            ));
        }
        let current_mtime = metadata.modified().ok();
        if record.mtime != current_mtime {
            return PriorReadStatus::Changed("mtime changed".to_string());
        }
        let contents = match std::fs::read(&real_path) {
            Ok(contents) => contents,
            Err(error) => {
                return PriorReadStatus::Unverifiable(format!(
                    "cannot read {} to verify its content: {error}",
                    real_path.display()
                ));
            }
        };
        if fingerprint_bytes(&contents) != record.content_hash {
            return PriorReadStatus::Changed("content hash changed".to_string());
        }
        if !record.text {
            return PriorReadStatus::NotText;
        }
        PriorReadStatus::Fresh(record)
    }

    /// 让某个路径的读记录失效（成功写入、压缩删除 read 结果、外部删除时调用）。
    pub fn invalidate(&self, absolute_path: &Path) {
        let real_path = kiana_sandbox::resolve_real_path(absolute_path)
            .unwrap_or_else(|_| absolute_path.to_path_buf());
        let mut map = self.lock_state();
        let mut identities: Vec<FileIdentity> = Vec::new();
        if let Some(identity) = map.by_path.remove(&real_path) {
            identities.push(identity);
        }
        identities.extend(
            map.by_identity
                .iter()
                .filter(|(_, record)| record.real_path == real_path)
                .map(|(identity, _)| *identity),
        );
        for identity in identities {
            map.by_identity.remove(&identity);
        }
    }

    /// 清空全部记录（测试/会话关闭）。
    pub fn clear(&self) {
        let mut map = self.lock_state();
        map.by_identity.clear();
        map.by_path.clear();
    }

    /// 当前记录数（诊断/测试）。
    pub fn len(&self) -> usize {
        self.lock_state().by_identity.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 快照预览最多读取的前缀字节数（`content_excerpt` 只需要几千字符）。
const SNAPSHOT_PREFIX_BYTES: usize = 64 * 1024;

/// FNV-1a 64 位初始值。
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 位乘数。
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64 位哈希：只用于同一进程内的快照比较，不用于密码学场景。
pub fn fingerprint_bytes(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// 流式计算文件的 FNV-1a 哈希与行数，内存占用固定为 64 KiB。
fn fingerprint_file(path: &Path) -> std::io::Result<FileDigest> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut reader = file.take(u64::MAX);
    let mut buffer = [0u8; 64 * 1024];
    let mut hash = FNV_OFFSET_BASIS;
    let mut line_count = 1u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        for byte in &buffer[..read] {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
            if *byte == b'\n' {
                line_count += 1;
            }
        }
    }
    Ok(FileDigest { hash, line_count })
}

/// 读取文件前 `limit` 个字节（不足则读取全部）。
fn read_prefix(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut reader = file.take(limit as u64);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// [`fingerprint_file`] 的结果。
struct FileDigest {
    /// FNV-1a 64 位哈希。
    hash: u64,
    /// 按 `\n` 计的行数（空文件为 1）。
    line_count: u64,
}

/// 按 `\n` 计行（与 read 工具 `split('\n')` 的行数一致：空文件为 1 行）。
fn count_lines(contents: &[u8]) -> u64 {
    contents.iter().filter(|byte| **byte == b'\n').count() as u64 + 1
}

/// 当前 Unix 毫秒；用于读记录的恢复排序。
fn now_epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

fn normalize_ranges(ranges: Vec<LineRange>, line_count: u64) -> Vec<LineRange> {
    let mut normalized = Vec::with_capacity(ranges.len());
    for range in ranges {
        if line_count == 0 {
            continue;
        }
        let start = range.start.max(1);
        let end = range.end.min(line_count);
        if start <= end {
            merge_range(&mut normalized, LineRange::new(start, end));
        }
    }
    normalized
}

/// 插入并合并相邻/重叠范围。
fn merge_range(ranges: &mut Vec<LineRange>, range: LineRange) {
    ranges.push(range);
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut merged: Vec<LineRange> = Vec::with_capacity(ranges.len());
    for range in ranges.drain(..) {
        match merged.last_mut() {
            Some(last) if range.start <= last.end.saturating_add(1) => {
                last.end = last.end.max(range.end);
            }
            _ => merged.push(range),
        }
    }
    *ranges = merged;
}

#[cfg(unix)]
fn metadata_file_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

#[cfg(windows)]
fn metadata_file_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::windows::fs::MetadataExt;
    (
        u64::from(metadata.volume_serial_number().unwrap_or(0)),
        metadata.file_index().unwrap_or(0),
    )
}

#[cfg(not(any(unix, windows)))]
fn metadata_file_identity(_metadata: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

/// 沙箱拒绝 → 工具错误。`OUTSIDE_WORKSPACE` / `PROTECTED_PATH` / `READ_ONLY` 等错误码
/// 是给模型/程序化调用者的稳定契约。
pub(crate) fn sandbox_denied_error(denial: kiana_sandbox::SandboxDenial) -> AgentToolError {
    AgentToolError {
        message: denial.message(),
        name: Some("SandboxDenied".to_string()),
        stack: None,
    }
}

/// `ReadBeforeEdit`：没有当前读记录就修改已存在的文件。
pub(crate) fn read_before_write_error(path: &Path) -> AgentToolError {
    AgentToolError {
        message: format!(
            "ReadBeforeEdit: {} has not been read in this session. Use the read tool to view \
             the current content before modifying it.",
            path.display()
        ),
        name: Some("ReadBeforeEdit".to_string()),
        stack: None,
    }
}

/// `ReadBeforeEdit`：只读过文件的一部分，不能整文件覆盖。
pub(crate) fn read_before_write_partial_error(path: &Path, line_count: u64) -> AgentToolError {
    AgentToolError {
        message: format!(
            "ReadBeforeEdit: {} has only been partially read in this session ({} lines total). \
             A full overwrite requires reading the whole file first; use edit for changes to the \
             part you have seen, or read the remaining range.",
            path.display(),
            line_count.max(1)
        ),
        name: Some("ReadBeforeEdit".to_string()),
        stack: None,
    }
}

/// `FileChangedSinceRead` / `FILE_CHANGED_SINCE_READ`：读完之后文件被外部改过。
pub(crate) fn file_changed_since_read_error(path: &Path, reason: &str) -> AgentToolError {
    AgentToolError {
        message: format!(
            "FileChangedSinceRead (FILE_CHANGED_SINCE_READ): {} changed on disk since it was \
             read ({reason}). Re-read it with the read tool before modifying it; do not retry \
             the previous modification.",
            path.display()
        ),
        name: Some("FileChangedSinceRead".to_string()),
        stack: None,
    }
}

/// 截断读试图修改未看到的部分。
pub(crate) fn read_range_not_seen_error(path: &Path, start: u64, end: u64) -> AgentToolError {
    AgentToolError {
        message: format!(
            "READ_RANGE_NOT_SEEN: the requested change to {} touches lines {start}-{end}, \
             which were not included in the previous (partial) read. Read that range with the \
             read tool first.",
            path.display()
        ),
        name: Some("ReadRangeNotSeen".to_string()),
        stack: None,
    }
}

/// 读到了非文本内容（图片等），不能当作文本编辑。
pub(crate) fn read_not_text_error(path: &Path) -> AgentToolError {
    AgentToolError {
        message: format!(
            "READ_NOT_TEXT: {} was read as a non-text payload, so the edit/write tools cannot \
             safely modify it as text. Use a different mechanism if you need to change it.",
            path.display()
        ),
        name: Some("ReadNotText".to_string()),
        stack: None,
    }
}

/// 无法验证读记录（stat/读失败、inode 不可用）。fail-closed。
pub(crate) fn prior_read_verification_error(path: &Path, reason: &str) -> AgentToolError {
    AgentToolError {
        message: format!(
            "PRIOR_READ_VERIFICATION_FAILED: cannot verify a prior read of {} ({reason}). \
             Read it again with the read tool before modifying it.",
            path.display()
        ),
        name: Some("PriorReadVerificationFailed".to_string()),
        stack: None,
    }
}

/// `fs.readFile`：`open(2)` 之后读完全文，错误按发生的阶段标记 syscall。
///
/// 整个文件一次读入内存、没有大小上限；`offset`/`limit` 的解码与选行在调用方完成。
pub(crate) async fn read_file_bytes(path: &Path) -> Result<Vec<u8>, ToolOperationError> {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| ToolOperationError::from_io("open", path, error))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .await
        .map_err(|error| ToolOperationError::from_io("read", path, error))?;
    Ok(bytes)
}

/// `fs.writeFile`：`open(2)` 之后写完；写阶段的错误不带 path（Node: `ENOSPC: ..., write`）。
pub(crate) async fn write_file_bytes(
    path: &Path,
    content: &[u8],
) -> Result<(), ToolOperationError> {
    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|error| ToolOperationError::from_io("open", path, error))?;
    file.write_all(content)
        .await
        .map_err(|error| ToolOperationError::from_io_without_path("write", error))?;
    // tokio 的 `File::write` 把写操作交给 blocking pool 后立即返回 Ready；必须 `flush`
    // 等到它真正完成（否则调用方可能在数据落盘前读到空文件，写错误也会被吞掉）。
    // Node 的 writeFile 同样在 write 完成后才 resolve，且写错误不带 path。
    file.flush()
        .await
        .map_err(|error| ToolOperationError::from_io_without_path("write", error))?;
    Ok(())
}

impl std::fmt::Display for ToolOperationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ToolOperationError {}

/// 把操作错误转成工具错误（消息逐字）。
fn operation_error(error: ToolOperationError) -> AgentToolError {
    AgentToolError {
        message: error.message,
        name: None,
        stack: None,
    }
}

/// `new Error("Operation aborted")`。
fn aborted_error() -> AgentToolError {
    AgentToolError {
        message: "Operation aborted".to_string(),
        name: None,
        stack: None,
    }
}

/// 参数解析失败时的工具错误（正常管线里 schema 校验更早失败，不会到这里）。
fn invalid_input_error(error: impl std::fmt::Display) -> AgentToolError {
    AgentToolError {
        message: error.to_string(),
        name: None,
        stack: None,
    }
}

// ===========================================================================
// 操作 trait
// ===========================================================================

/// 可插拔的文件读取操作，默认实现是本地文件系统。
///
/// TS: `ReadOperations`。`detect_image_mime_type` 的默认实现返回 `None`，
/// 对应 TS 里该字段为 `undefined`（不检测图片）。
///
/// # 取消
///
/// 方法签名没有取消令牌；调用方用 `tokio::select!` 让整个 `run` 与取消信号竞速，取消时
/// future 被丢弃。自定义实现不应假设方法一定运行到完成（例如不要在 `Drop` 里安排必须
/// 完成的副作用）。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性（见 kiana-ai 同类处理）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ReadOperations: Send + Sync {
    /// 读取文件内容。
    async fn read_file(&self, absolute_path: &Path) -> Result<Vec<u8>, ToolOperationError>;
    /// 检查文件可读（不可读时返回错误）。
    async fn access(&self, absolute_path: &Path) -> Result<(), ToolOperationError>;
    /// 按魔数检测图片 MIME 类型；非图片返回 `None`。
    ///
    /// trait 的默认实现直接返回 `Ok(None)`，不做任何检测（对应 TS 里该字段为
    /// `undefined`）；真正读取文件前 4100 字节做嗅探的是 [`LocalReadOperations`]
    /// 的覆写，目录也是在覆写里以 `EISDIR` 失败（对应 TS 的 `FileHandle.read`，
    /// 错误不带 path）。
    async fn detect_image_mime_type(
        &self,
        _absolute_path: &Path,
    ) -> Result<Option<String>, ToolOperationError> {
        Ok(None)
    }
}

/// 默认操作：本地文件系统（`fs.readFile` / `fs.access(R_OK)` / 魔数嗅探）。
///
/// `detect_image_mime_type` 使用 `kiana_core::mime` 的同步文件嗅探（前 4100 字节），
/// 错误经 [`ToolOperationError::from_file_handle_io`] 映射成 Node 的 `open`/`read` 文案。
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalReadOperations;

#[async_trait]
impl ReadOperations for LocalReadOperations {
    async fn read_file(&self, absolute_path: &Path) -> Result<Vec<u8>, ToolOperationError> {
        read_file_bytes(absolute_path).await
    }

    async fn access(&self, absolute_path: &Path) -> Result<(), ToolOperationError> {
        access_path(absolute_path, R_OK)
    }

    async fn detect_image_mime_type(
        &self,
        absolute_path: &Path,
    ) -> Result<Option<String>, ToolOperationError> {
        // `detectSupportedImageMimeTypeFromFile` 用 `FileHandle.read`：目录在 read 阶段
        // 失败，Node 的这条错误不带 path。
        kiana_core::mime::detect_supported_image_mime_type_from_file(absolute_path)
            .map_err(|error| ToolOperationError::from_file_handle_io(absolute_path, error))
    }
}

/// `read` 工具的选项。
///
/// [`create_read_tool_definition`] 接受 `Option<Self>`，`None` 等价于
/// [`ReadToolOptions::default`]。
#[derive(Clone)]
pub struct ReadToolOptions {
    /// 是否自动缩放图片。默认 `true`。
    ///
    /// 为 `false` 时 `process_image` 跳过缩放，但仍会做格式规范化，并按（规范化后的）
    /// 原始字节生成 base64。
    pub auto_resize_images: bool,
    /// 执行上下文没有模型元数据时的回退缩放档。
    ///
    /// 优先级：模型目录的 `input_limits.images.resize` → 本字段 → 内置默认值。
    /// `auto_resize_images` 为 `false` 时该字段不会产生效果。
    pub resize_options: Option<ModelImageResizeOptions>,
    /// 自定义文件读取操作。默认本地文件系统。
    ///
    /// `None` 时由 [`LocalReadOperations`] 顶替；测试通过它注入自定义实现。
    pub operations: Option<Arc<dyn ReadOperations>>,
    /// 读记录表。`None` 时使用进程级共享实例。
    ///
    /// read 在每次成功读取后写入记录；write/edit 用同一张表判断是否允许修改。
    pub read_state: Option<Arc<ReadStateRegistry>>,
}

impl Default for ReadToolOptions {
    fn default() -> Self {
        Self {
            auto_resize_images: true,
            resize_options: None,
            operations: None,
            read_state: None,
        }
    }
}

impl std::fmt::Debug for ReadToolOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadToolOptions")
            .field("auto_resize_images", &self.auto_resize_images)
            .field("resize_options", &self.resize_options)
            .field("operations", &self.operations.is_some())
            .field("read_state", &self.read_state.is_some())
            .finish()
    }
}

// ===========================================================================
// ReadToolOutput
// ===========================================================================

/// 图片分支的 `structuredContent` 形状（键顺序 type/data/mimeType/note）。
///
/// `note` 是图片分支生成的文本块内容（`Read image file [...]` 加上可选提示）；
/// 图片处理失败时只有文本块、不会产生本结构，`structuredContent` 退化为字符串。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReadImageOutput {
    /// 固定判别值 `"image"`。
    #[serde(rename = "type")]
    pub kind: ImageKind,
    /// base64 编码的图片数据。
    pub data: String,
    /// MIME 类型。
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    /// 随图片附带的文本说明。
    pub note: String,
}

/// `ReadToolOutput`：文本文件返回字符串，图片返回 [`ReadImageOutput`]。
///
/// `#[serde(untagged)]` 让线格式与 TS 的联合类型一致：文本直接是 JSON string，图片才是
/// 对象；反序列化也按形状自动选择变体。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ReadToolOutput {
    /// 文本输出。
    Text(String),
    /// 图片输出。
    Image(ReadImageOutput),
}

impl ReadToolOutput {
    /// 转成 `structuredContent` 的 JSON 值。
    ///
    /// 结构是自描述的（string 或对象），序列化不应失败；万一失败回落为 JSON `null`。
    pub fn to_json_value(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or(JsonValue::Null)
    }
}

/// `toReadOutput(content)`：第一个 text 块作为 note，有 image 块则返回图片对象。
///
/// 选区规则固定：文本取第一个 Text 块（没有则空串），图片取第一个 Image 块。图片分支的
/// `note` 因此来自第一个文本块（本文件构造时为图片块之前的说明）。没有 image 块时返回
/// [`ReadToolOutput::Text`]，即使文本是空串。
pub fn to_read_output(content: &[UserContentBlock]) -> ReadToolOutput {
    let text = content
        .iter()
        .find_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.clone()),
            UserContentBlock::Image(_) => None,
        })
        .unwrap_or_default();
    let image = content.iter().find_map(|block| match block {
        UserContentBlock::Image(image) => Some(image),
        UserContentBlock::Text(_) => None,
    });
    match image {
        Some(image) => ReadToolOutput::Image(ReadImageOutput {
            kind: ImageKind::Image,
            data: image.data.clone(),
            mime_type: image.mime_type.clone(),
            note: text,
        }),
        None => ReadToolOutput::Text(text),
    }
}

/// `getNonVisionImageNote(model)`：模型不支持图片时返回逐字提示。
fn get_non_vision_image_note(model: Option<&Model>) -> Option<&'static str> {
    let model = model?;
    if model
        .input
        .iter()
        .any(|modality| matches!(modality, kiana_ai::types::ModelInputModality::Image))
    {
        return None;
    }
    Some(NON_VISION_IMAGE_NOTE)
}

// ===========================================================================
// 参数 schema
// ===========================================================================

/// `readSchema`（TypeBox 序列化结果，逐字节）。
///
/// `offset`/`limit` 声明为 `number`（不是 integer，也没有最小值），所以小数与负数能通过
/// schema；它们的取舍由 `ReadExecute::run` 里的 JS 兼容逻辑处理。`path` 是唯一必填字段。
pub fn read_parameters_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": { "type": "string", "description": "Path to the file to read (relative or absolute)" },
            "offset": { "type": "number", "description": "Line number to start reading from (1-indexed)" },
            "limit": { "type": "number", "description": "Maximum number of lines to read" }
        }
    })
}

/// `readOutputSchema`（TypeBox 序列化结果，逐字节）。
///
/// 联合类型：文本分支是 JSON string；图片分支是要求 `type`/`data`/`mimeType`/`note` 四个
/// 键的对象。由 [`create_read_tool_definition`] 填入 `output_schema`。
pub fn read_output_schema() -> JsonValue {
    serde_json::json!({
        "anyOf": [
            { "type": "string" },
            {
                "type": "object",
                "required": ["type", "data", "mimeType", "note"],
                "properties": {
                    "type": { "type": "string", "const": "image" },
                    "data": { "type": "string" },
                    "mimeType": { "type": "string" },
                    "note": { "type": "string" }
                }
            }
        ]
    })
}

/// `{ type: "json_schema", strict: "prefer" }`。
fn json_schema_prefer() -> ConstrainedSampling {
    ConstrainedSampling::Config(kiana_ai::types::ConstrainedSamplingConfig::JsonSchema {
        strict: kiana_ai::types::StrictMode::Prefer,
    })
}

// ===========================================================================
// execute
// ===========================================================================

/// `ctx?.cwd || cwd`：占位上下文（TS 的 `ctx === undefined`）与空 cwd 都回落到工具 cwd。
///
/// 必须先按指针识别占位实例（[`is_missing_tool_context`]）：它的 `cwd` 是**进程当前目录**
/// 而不是空串，只看 `is_empty()` 会把“没有上下文”误判成“有上下文”。该函数被
/// `read`/`bash`/`edit`/`find`/`grep`/`ls`/`write` 共用。
pub(crate) fn effective_cwd(ctx: &Arc<dyn ExtensionToolContextApi>, fallback: &str) -> String {
    if is_missing_tool_context(ctx) {
        return fallback.to_string();
    }
    let cwd = ctx.cwd();
    if cwd.as_os_str().is_empty() {
        fallback.to_string()
    } else {
        cwd.to_string_lossy().into_owned()
    }
}

/// `read` 的 `execute` 实现。
///
/// 字段在构造时冻结（cwd、缩放开关、回退缩放档、后端），执行期间不共享可变状态；
/// 后端 trait 本身要求 `Send + Sync`。
struct ReadExecute {
    cwd: String,
    auto_resize_images: bool,
    resize_options: Option<ModelImageResizeOptions>,
    operations: Arc<dyn ReadOperations>,
    read_state: Arc<ReadStateRegistry>,
}

impl ToolDefinitionExecute for ReadExecute {
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
            // TS 在 abort 监听里直接 reject；已取消的调用不解析参数、不访问文件。
            if signal.is_some_and(CancellationToken::is_cancelled) {
                return Err(aborted_error());
            }
            // 保留原始 JSON：ReadToolInput 把 offset/limit 折成 Option<f64>，丢失了
            // “键缺省”与“显式 null”的区别，而 TS 的 `!== undefined` 需要区分。
            let raw_params = params.clone();
            let input = match serde_json::from_value::<ReadToolInput>(params) {
                Ok(input) => input,
                Err(error) => return Err(invalid_input_error(error)),
            };

            // 整个 run 与取消信号竞速：取消时 select! 丢弃 run 的 future，
            // 后端实现必须能安全 drop。
            match signal {
                Some(token) => tokio::select! {
                    _ = token.cancelled() => Err(aborted_error()),
                    result = self.run(input, raw_params, &ctx) => result,
                },
                None => self.run(input, raw_params, &ctx).await,
            }
        })
    }
}

/// `offset` 的 JS 语义：`Option<f64>` + 显式 null 区分。
#[derive(Debug, Clone, Copy)]
enum JsOptionalNumber {
    Undefined,
    Null,
    Number(f64),
}

impl JsOptionalNumber {
    fn from_json(value: Option<&JsonValue>) -> Self {
        match value {
            None => JsOptionalNumber::Undefined,
            Some(JsonValue::Null) => JsOptionalNumber::Null,
            Some(other) => match other.as_f64() {
                Some(number) => JsOptionalNumber::Number(number),
                // schema 校验会拦住非数字；直接调用时按 TS 的 `Number(value)` 语义兜底。
                None => JsOptionalNumber::Number(js_to_number(other)),
            },
        }
    }

    /// `value !== undefined`（null 也算有值）。
    ///
    /// 因此显式 `"limit": null` 与缺省不同：它按 0 行处理，随后仍可能追加
    /// “more lines” 脚注；缺省 limit（或显式 limit 足够大覆盖到末尾）才会读到文件末尾。
    fn is_defined(self) -> bool {
        !matches!(self, JsOptionalNumber::Undefined)
    }

    /// 参与算术运算时的数值（null → 0）。
    fn as_number(self) -> f64 {
        match self {
            JsOptionalNumber::Undefined | JsOptionalNumber::Null => 0.0,
            JsOptionalNumber::Number(number) => number,
        }
    }
}

/// JS `Number(value)` 的近似（仅用于直接调用、未经 schema 校验的输入）。
fn js_to_number(value: &JsonValue) -> f64 {
    match value {
        JsonValue::String(text) => kiana_json::js_num::parse_js_number(text),
        JsonValue::Bool(flag) => {
            if *flag {
                1.0
            } else {
                0.0
            }
        }
        _ => f64::NAN,
    }
}

/// JS 的 `ToIntegerOrInfinity`。
fn to_integer_or_infinity(value: f64) -> f64 {
    if value.is_nan() {
        0.0
    } else if value.is_infinite() {
        value
    } else {
        value.trunc()
    }
}

/// JS `Array.prototype.slice(begin, end)`（元素为字符串切片）。
///
/// 两个端点先做 `ToIntegerOrInfinity`（`NaN → 0`，`±Infinity` 保留），负数从末尾折算，
/// 再夹到 `[0, len]`；折算后 `end <= start` 返回空 `Vec`。因此小数索引会向零截断
/// （负数小数同样向零，如 -1.5 → -1），而属性访问（[`js_array_get`]）不会。
#[cfg(test)]
fn js_slice<'a>(items: &'a [&'a str], start: f64, end: Option<f64>) -> Vec<&'a str> {
    let (actual_start, actual_end) = js_slice_bounds(items.len(), start, end);
    if actual_end <= actual_start {
        return Vec::new();
    }
    items[actual_start..actual_end].to_vec()
}

/// [`js_slice`] 的边界计算：返回 `(start, end)`（0-based 半开区间，已夹到 `[0, len]`）。
///
/// 读记录需要知道模型实际看到了哪些行，必须与 `slice` 用同一套端点折算；这里把折算
/// 逻辑抽出来，避免两处漂移。
fn js_slice_bounds(items_len: usize, start: f64, end: Option<f64>) -> (usize, usize) {
    let length = items_len as f64;
    let clamp = |relative: f64| -> usize {
        let relative = to_integer_or_infinity(relative);
        if relative == f64::NEG_INFINITY {
            0
        } else if relative < 0.0 {
            ((length + relative).max(0.0)) as usize
        } else {
            relative.min(length) as usize
        }
    };
    let actual_start = clamp(start).min(items_len);
    let actual_end = match end {
        Some(end) => clamp(end).min(items_len),
        None => items_len,
    };
    (actual_start, actual_end)
}

impl ReadExecute {
    async fn run(
        &self,
        input: ReadToolInput,
        raw_params: JsonValue,
        ctx: &Arc<dyn ExtensionToolContextApi>,
    ) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
        let cwd = effective_cwd(ctx, &self.cwd);
        let path = input.path.as_str();

        // 阶段 1：解析路径。解析器不会因不存在而报错——它返回原路径（或 macOS 命名变体）；
        // 文件不存在/不可读由下一步的 access 报出（默认后端给出 Node 风格的 ENOENT/EACCES）。
        let absolute_path = resolve_read_path_async(path, &cwd).await;
        // 阶段 2：可读性预检。默认实现是 access(2)（R_OK），对目录会成功；目录要到魔数
        // 嗅探的 FileHandle.read 才以 EISDIR 失败（测试
        // `reading_a_directory_reports_the_file_handle_read_error_without_a_path` 锁定该文案）。
        self.operations
            .access(&absolute_path)
            .await
            .map_err(operation_error)?;

        // 阶段 3：魔数决定分支。只看内容前缀、不看扩展名：改名的图片仍走图片分支，
        // 扩展名像图片的文本文件仍走文本分支。
        let mime_type = self
            .operations
            .detect_image_mime_type(&absolute_path)
            .await
            .map_err(operation_error)?;
        let model = ctx.model();
        let non_vision_image_note = get_non_vision_image_note(model.as_ref());

        let mut details: Option<ReadToolDetails> = None;
        let content: Vec<UserContentBlock>;

        if let Some(mime_type) = mime_type {
            // 图片分支：整文件读入内存后交给 process_image 做规范化与可选缩放。
            let buffer = self
                .operations
                .read_file(&absolute_path)
                .await
                .map_err(operation_error)?;
            // 缩放配置优先级：模型目录的 images.resize → 工具选项 → 内置默认值。
            let resize_options = model
                .as_ref()
                .and_then(|model| model.input_limits.as_ref())
                .and_then(|limits| limits.images.as_ref())
                .and_then(|images| images.resize.clone())
                .or_else(|| self.resize_options.clone());
            // process_image 先规范化格式（例如 BMP→PNG），auto_resize_images 时再缩放；
            // 失败返回逐字文案，由下面的分支转成纯文本结果。
            let processed = process_image(
                &buffer,
                &mime_type,
                Some(ProcessImageOptions {
                    auto_resize_images: Some(self.auto_resize_images),
                    resize_options: resize_options.as_ref().map(image_resize_options_from_model),
                }),
            );
            match processed {
                ProcessImageResult::Err { message } => {
                    let mut text_note = format!("Read image file [{mime_type}]\n{message}");
                    if let Some(note) = non_vision_image_note {
                        text_note.push('\n');
                        text_note.push_str(note);
                    }
                    content = vec![UserContentBlock::Text(TextContent {
                        kind_text: TextKind::Text,
                        text: text_note,
                        text_signature: None,
                    })];
                }
                ProcessImageResult::Ok {
                    data,
                    mime_type: processed_mime_type,
                    hints,
                } => {
                    // note 依次追加转换/尺寸提示与非视觉模型提示，多条提示用换行分隔。
                    let mut text_note = format!("Read image file [{processed_mime_type}]");
                    if !hints.is_empty() {
                        text_note.push('\n');
                        text_note.push_str(&hints.join("\n"));
                    }
                    if let Some(note) = non_vision_image_note {
                        text_note.push('\n');
                        text_note.push_str(note);
                    }
                    // content 顺序固定为 [文本说明, 图片]：第一个文本块会作为
                    // structuredContent 的 note。
                    content = vec![
                        UserContentBlock::Text(TextContent {
                            kind_text: TextKind::Text,
                            text: text_note,
                            text_signature: None,
                        }),
                        UserContentBlock::Image(ImageContent {
                            kind_image: ImageKind::Image,
                            data,
                            mime_type: processed_mime_type,
                        }),
                    ];
                }
            }
            // 图片按「非文本」记录：允许后续 read，但 edit/write 不能把它当文本改写。
            self.read_state
                .record_read(&absolute_path, &buffer, Vec::new(), false);
        } else {
            // 文本分支：整文件读入 → WHATWG UTF-8 解码（不剥 BOM）→ 按 '\n' 切行。
            // Rust 的 split 对空文件返回一个空元素，所以空文件的 total_file_lines 是 1。
            let buffer = self
                .operations
                .read_file(&absolute_path)
                .await
                .map_err(operation_error)?;
            let text_content = crate::edit_diff::decode_utf8(&buffer);
            let all_lines: Vec<&str> = text_content.split('\n').collect();
            let total_file_lines = all_lines.len();

            // `offset`/`limit` 需要区分 undefined 与 null（`limit !== undefined`）。
            let offset = JsOptionalNumber::from_json(raw_params.get("offset"));
            let limit = JsOptionalNumber::from_json(raw_params.get("limit"));

            // `offset ? Math.max(0, offset - 1) : 0`：0/null/NaN 走 0 分支；负数先减 1
            // 再被 max(0) 折回 0；小数 offset 保留小数，显示值与 slice 都按 JS 语义处理。
            let start_line = if offset.is_defined() && js_truthy(offset.as_number()) {
                (offset.as_number() - 1.0).max(0.0)
            } else {
                0.0
            };
            let start_line_display = start_line + 1.0;

            // 超出末尾直接报错（在 limit 与截断处理之前），消息里的 offset 用 JS Number
            // 格式化，因此小数/Infinity 会原样出现（Infinity 显示为 `Infinity`；
            // NaN 的 offset 走 js_truthy 假分支，start_line 为 0，进不了这条错误）。
            if start_line >= total_file_lines as f64 {
                let offset_display = if offset.is_defined() {
                    js_number_text(offset.as_number())
                } else {
                    "undefined".to_string()
                };
                return Err(AgentToolError {
                    message: format!(
                        "Offset {offset_display} is beyond end of file ({total_file_lines} lines total)"
                    ),
                    name: None,
                    stack: None,
                });
            }

            // limit 的端点 = Math.min(startLine + limit, 总行数)，NaN 经 js_math_min
            // 传播；user_limited_lines 由已与总行数取 min 的端点减去 start_line 得到，
            // 可能为负数（limit<0）或小数——上界已被夹到总行数，这里只是不把它下夹到
            // 0（也不取整），脚注继续按 JS 算术使用。
            let mut user_limited_lines: Option<f64> = None;
            let limit_end = if limit.is_defined() {
                // TS: `Math.min(startLine + limit, allLines.length)`（NaN 会传播，
                // 随后的 `slice(startLine, NaN)` 返回空数组）。
                let end_line = js_math_min(start_line + limit.as_number(), total_file_lines as f64);
                user_limited_lines = Some(end_line - start_line);
                Some(end_line)
            } else {
                None
            };
            // 本次实际选中的行范围（0-based 半开）；读记录只覆盖这部分。
            let (selected_start, selected_end) =
                js_slice_bounds(all_lines.len(), start_line, limit_end);
            let selected_content = if selected_end <= selected_start {
                String::new()
            } else {
                all_lines[selected_start..selected_end].join("\n")
            };

            // 截断只针对“选中内容”。因此 details.truncation 的 totalLines/totalBytes 是
            // 选中内容的统计，而脚注里的 “of N” 是整文件行数。
            let truncation = truncate_head(&selected_content, &TruncationOptions::default());
            // 模型实际看到的行范围（truncate_head 只保留完整行，不会出现半行）：
            // - 首行超限：没有任何内容返回，范围为空；
            // - 触顶截断：只看到选中区间的前 `output_lines` 行；
            // - 未截断：就是本次选中的全部行。
            let shown_lines = if truncation.first_line_exceeds_limit {
                0
            } else if truncation.truncated {
                truncation.output_lines as usize
            } else {
                selected_end.saturating_sub(selected_start)
            };
            let seen_ranges = if shown_lines == 0 || selected_start >= all_lines.len() {
                Vec::new()
            } else {
                let end = (selected_start + shown_lines).min(all_lines.len());
                vec![LineRange::new(selected_start as u64 + 1, end as u64)]
            };
            let mut output_text: String;
            if truncation.first_line_exceeds_limit {
                // TS: `Buffer.byteLength(allLines[startLine], "utf-8")`。小数索引不是数组
                // 元素（`allLines[2.5] === undefined`），`Buffer.byteLength(undefined)`
                // 抛 TypeError，整个调用 reject。
                let Some(first_line) = js_array_get(&all_lines, start_line) else {
                    return Err(AgentToolError {
                        message: "The \"string\" argument must be of type string or an instance of Buffer or ArrayBuffer. Received undefined".to_string(),
                        name: Some("TypeError".to_string()),
                        stack: None,
                    });
                };
                // first_line_size 是 UTF-8 字节数（等价 Buffer.byteLength）；脚注里的
                // path 是用户传入的原文，不是解析后的绝对路径。
                let first_line_size = format_size(first_line.len() as u64);
                let max_bytes_display = format_size(DEFAULT_MAX_BYTES as u64);
                output_text = format!(
                    "[Line {} is {first_line_size}, exceeds {max_bytes_display} limit. Use bash: sed -n '{}p' {path} | head -c {DEFAULT_MAX_BYTES}]",
                    js_number_text(start_line_display),
                    js_number_text(start_line_display),
                );
                details = Some(ReadToolDetails {
                    truncation: Some(truncation),
                });
            } else if truncation.truncated {
                // 按行或按字节触顶：按字节时脚注额外标注 50.0KB 上限，其余文案逐字对齐 TS。
                let end_line_display = start_line_display + truncation.output_lines as f64 - 1.0;
                let next_offset = end_line_display + 1.0;
                output_text = truncation.content.clone();
                match truncation.truncated_by {
                    Some(kiana_ext_api::tool_types::TruncatedBy::Lines) => {
                        output_text.push_str(&format!(
                            "\n\n[Showing lines {}-{} of {total_file_lines}. Use offset={} to continue.]",
                            js_number_text(start_line_display),
                            js_number_text(end_line_display),
                            js_number_text(next_offset),
                        ));
                    }
                    _ => {
                        let max_bytes_display = format_size(DEFAULT_MAX_BYTES as u64);
                        output_text.push_str(&format!(
                            "\n\n[Showing lines {}-{} of {total_file_lines} ({max_bytes_display} limit). Use offset={} to continue.]",
                            js_number_text(start_line_display),
                            js_number_text(end_line_display),
                            js_number_text(next_offset),
                        ));
                    }
                }
                details = Some(ReadToolDetails {
                    truncation: Some(truncation),
                });
            } else if let Some(user_limited_lines) = user_limited_lines
                && start_line + user_limited_lines < total_file_lines as f64
            {
                // 没有截断但用户 limit 没覆盖到文件末尾：脚注给出剩余行数与下一 offset。
                // 条件与数值都用 JS 算术，负数/小数 limit 的结果见 golden 的
                // limit-negative / offset-fraction 用例。
                let remaining = total_file_lines as f64 - (start_line + user_limited_lines);
                let next_offset = start_line + user_limited_lines + 1.0;
                output_text = format!(
                    "{}\n\n[{} more lines in file. Use offset={} to continue.]",
                    truncation.content,
                    js_number_text(remaining),
                    js_number_text(next_offset),
                );
            } else {
                output_text = truncation.content;
            }

            content = vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: output_text,
                text_signature: None,
            })];
            // 记录读到的原始字节与可见行范围；write/edit 的写入门禁以此为准。
            self.read_state
                .record_read(&absolute_path, &buffer, seen_ranges, true);
        }

        // structuredContent 从 content 投影：文本分支就是文本本身，图片分支是
        // {type,data,mimeType,note} 对象。details 只在截断分支有值，其余为 JSON null。
        let structured_content = to_read_output(&content).to_json_value();
        let details_value = match &details {
            Some(details) => serde_json::to_value(details).unwrap_or(JsonValue::Null),
            None => JsonValue::Null,
        };
        Ok(AgentToolResult {
            content,
            details: details_value,
            structured_content: Some(structured_content),
            usage: None,
            is_error: None,
            terminate: None,
        })
    }
}

/// `ModelImageResizeOptions` → [`ImageResizeOptions`]（缺省字段回落到内置默认值）。
///
/// 与 `kiana_core::image_process::normalize_tool_result_images` 内部的映射一致（TS 的
/// `image-resize.ts` 只有一份实现，Rust 侧的转换函数未公开）。
///
/// `max_width`/`max_height` 做 `u64 → u32` 的饱和转换（超出范围取 `u32::MAX`），
/// `max_bytes` 保持 `u64`；未设置的字段回落到 [`ImageResizeOptions::default`]。
fn image_resize_options_from_model(model: &ModelImageResizeOptions) -> ImageResizeOptions {
    let defaults = ImageResizeOptions::default();
    ImageResizeOptions {
        max_width: model
            .max_width
            .map(|value| u32::try_from(value).unwrap_or(u32::MAX))
            .unwrap_or(defaults.max_width),
        max_height: model
            .max_height
            .map(|value| u32::try_from(value).unwrap_or(u32::MAX))
            .unwrap_or(defaults.max_height),
        max_bytes: model.max_bytes.unwrap_or(defaults.max_bytes),
        jpeg_quality: model.jpeg_quality,
    }
}

/// JS 真值判断（只处理数字）：`0`、`-0`、`NaN` 为假。
fn js_truthy(value: f64) -> bool {
    !(value == 0.0 || value.is_nan())
}

/// JS `Math.min(a, b)`：任一参数为 `NaN` 时结果为 `NaN`。
///
/// Rust 的 `f64::min` 在任一侧为 `NaN` 时返回**另一个**参数，与 JS 不同；`read.ts` 的
/// `Math.min(startLine + limit, allLines.length)` 需要 `NaN` 传播（随后 `slice` 得空数组）。
fn js_math_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.min(b)
    }
}

/// JS `String(number)`。
///
/// 只用于拼装用户可见文本（脚注与错误）；有限值委托 `kiana_json::js_num::format_js_number`
/// 对齐 JS 的 `Number::toString`。
fn js_number_text(value: f64) -> String {
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

/// JS 的 `array[index]`：小数、负数、NaN、Infinity 都不是数组下标，返回 `undefined`。
///
/// 注意与 [`js_slice`] 不同：`slice` 会先做 `ToIntegerOrInfinity`，而属性访问不会。
fn js_array_get<'a>(items: &'a [&'a str], index: f64) -> Option<&'a str> {
    if !index.is_finite() || index.fract() != 0.0 || index < 0.0 {
        return None;
    }
    items.get(index as usize).copied()
}

// ===========================================================================
// 定义
// ===========================================================================

/// 创建 `read` 的 [`ToolDefinition`]。
///
/// TS: `createReadToolDefinition(cwd, options?)`。
///
/// 组装固定值：`name`/`label` 都是 `"read"`，description 取 [`READ_TOOL_DESCRIPTION`]，
/// prompt 元数据取 [`read_tool_system_prompt_contribution`]，参数/输出 schema 取
/// [`read_parameters_schema`] / [`read_output_schema`]，受限采样固定为
/// `{ type: "json_schema", strict: "prefer" }`。渲染器来自
/// [`crate::renderers::read::read_renderers`]。
///
/// `cwd` 只在上下文缺失或为空时作为回退（`effective_cwd`）；`options` 为 `None` 等价于
/// [`ReadToolOptions::default`]：自动缩放开启、使用 [`LocalReadOperations`]。
pub fn create_read_tool_definition(
    cwd: impl Into<String>,
    options: Option<ReadToolOptions>,
) -> ToolDefinition {
    let options = options.unwrap_or_default();
    let contribution = read_tool_system_prompt_contribution();
    let renderers: ToolRenderers = read_renderers();
    ToolDefinition {
        name: "read".to_string(),
        label: "read".to_string(),
        description: READ_TOOL_DESCRIPTION.to_string(),
        prompt_snippet: Some(contribution.snippet.to_string()),
        prompt_guidelines: Some(
            contribution
                .guidelines
                .iter()
                .map(|guideline| (*guideline).to_string())
                .collect(),
        ),
        parameters: read_parameters_schema(),
        constrained_sampling: Some(json_schema_prefer()),
        render_shell: renderers.render_shell,
        prepare_arguments: None,
        output_schema: Some(read_output_schema()),
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(ReadExecute {
            cwd: cwd.into(),
            auto_resize_images: options.auto_resize_images,
            resize_options: options.resize_options,
            operations: options
                .operations
                .unwrap_or_else(|| Arc::new(LocalReadOperations)),
            read_state: options.read_state.unwrap_or_else(ReadStateRegistry::global),
        }),
        render_call: renderers.render_call,
        render_result: renderers.render_result,
    }
}

/// 创建 `read` 的 [`AgentTool`]（`wrapToolDefinition` 投影）。
///
/// 与 [`create_read_tool_definition`] 的区别：这里不传上下文工厂（[`wrap_tool_definition`]
/// 的第二个参数是 `None`），每次执行都使用占位上下文，因此 cwd 恒为构造时的值、
/// `ctx.model()` 恒为 `None`——不追加非视觉提示，也不会按模型配置缩放图片；
/// [`ReadToolOptions::resize_options`] 仍作为回退生效。
pub fn create_read_tool(cwd: impl Into<String>, options: Option<ReadToolOptions>) -> AgentTool {
    wrap_tool_definition(create_read_tool_definition(cwd, options), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_slice_matches_array_prototype_slice() {
        let items = ["a", "b", "c", "d"];
        assert_eq!(js_slice(&items, 0.0, None), vec!["a", "b", "c", "d"]);
        assert_eq!(js_slice(&items, 1.5, Some(4.5)), vec!["b", "c", "d"]);
        assert_eq!(js_slice(&items, 0.0, Some(-2.0)), vec!["a", "b"]);
        assert_eq!(js_slice(&items, -2.0, None), vec!["c", "d"]);
        assert_eq!(
            js_slice(&items, -100.0, Some(100.0)),
            vec!["a", "b", "c", "d"]
        );
        assert_eq!(js_slice(&items, 2.0, Some(1.0)), Vec::<&str>::new());
        assert_eq!(js_slice(&items, -2.0, Some(-1.0)), vec!["c"]);
    }

    #[test]
    fn offset_truthiness_follows_js() {
        assert!(!js_truthy(0.0));
        assert!(!js_truthy(-0.0));
        assert!(!js_truthy(f64::NAN));
        assert!(js_truthy(-1.0));
        assert!(js_truthy(0.5));
    }

    #[test]
    fn nan_limit_propagates_through_math_min_like_js() {
        // TS: `Math.min(startLine + limit, allLines.length)` 在 limit 为 NaN 时得 NaN，
        // `slice(startLine, NaN)` 为空数组，`userLimitedLines` 也是 NaN（不输出 more lines）。
        let items = ["a", "b", "c"];
        let start_line = 0.0;
        let end_line = js_math_min(start_line + f64::NAN, items.len() as f64);
        assert!(end_line.is_nan());
        let user_limited_lines = end_line - start_line;
        assert!(user_limited_lines.is_nan());
        assert!(js_slice(&items, start_line, Some(end_line)).is_empty());
        let within_file = start_line + user_limited_lines < items.len() as f64;
        assert!(!within_file);
        // Rust 的 `f64::min` 会错误地返回另一侧（这里不是 JS 语义）。
        assert_eq!((f64::NAN).min(3.0), 3.0);
    }
}
