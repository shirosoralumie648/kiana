//! env 目录：`ExecutionEnv` 契约、Node 实现、UTF-8 解码与 `node:path` 语义。
//!
//! TS 源：`packages/durable/src/env/{index,decode,line-scan,node,node-watch}.ts`。
//!
//! `path/` 不是 TS 文件：它是 `node:path` 的 posix/win32 移植，**唯一实现位置**记录在
//! `ARCHITECTURE.md` §4.9，由 `kiana-env-client` 复用（见 [`path`] 的模块文档）。
//!
//! 与 TS 的对应关系：
//! - TS 的 `Result<T, E>` 是值联合（`{ok:true,value} | {ok:false,error}`）。Rust 用标准
//!   `Result`，[`ok`]/[`err`] 只是构造器；[`get_or_throw`] 对应 `throw result.error` 的写法，
//!   用 `panic_any` 抛出**原始错误值**（`FileError`），调用方可以 `catch_unwind` + downcast。
//! - TS 的错误是 `Error` 子类，公开 `code`/`path`/`spillPath`。这里的 `FileError` /
//!   `ExecutionError` 是实现了 `std::error::Error` 的结构体，`Display` 就是 `message`，
//!   `cause` 用 `Arc<dyn Error>` 保存以保持 `Clone`（TS 的 `cause` 是只读引用）。
//!
//! # 这个模块解决什么问题
//! 上层（harness、tools、testing）只依赖这里的 trait，不直接调用 `std::fs` / `tokio::process`：
//! 同一份工具代码因此可以跑在本地 Node 语义实现（[`node`] 里的 `NodeExecutionEnv`）或
//! `kiana-env-client` 提供的远程实现上。实现方通过 `testing::env_conformance` 的一致性用例
//! 校验行为；改动这里的 trait 会同时影响所有实现和这些用例。
//!
//! # 核心类型
//! - [`FileSystem`]：文件读写、目录遍历、临时文件、watch、路径规范化。可预期的失败返回
//!   `Result<_, FileError>`，不 panic；错误码见 [`FileErrorCode`]。
//! - [`Shell`]：执行 shell 字符串或 argv，支持输出分块回调、spill、窗口化和超时。失败返回
//!   [`ExecutionError`]，错误码见 [`ExecutionErrorCode`]。
//! - [`ExecutionEnv`]：两者的组合；因为有下面的 blanket impl，任何同时实现 `FileSystem` 与
//!   `Shell` 的类型自动成为 `ExecutionEnv`，不能也没有必要再手写 impl。
//! - 环境创建的句柄 trait：[`TextLineReader`]（逐行读取）、[`BinaryReader`]（按偏移读取并
//!   扫描行）、[`DirReader`]（分页列目录）、[`FileWatcher`]（观察变化）。它们分别由
//!   `open_text_line_reader` / `open_binary_reader` / `open_dir_reader` / `watch` 创建，
//!   用完调用各自的 `close`。
//!
//! # 一次典型调用
//! 1. 工具通过 `ToolExecutionApi::env()` 取得 `Option<Arc<dyn ExecutionEnv>>`；`tools/env.rs`
//!    的 `require_env` 在没有配置环境时报 `No execution environment is configured`；
//! 2. 打开文件：`FileSystem::open_binary_reader` → `BinaryReader::scan_lines` 定位行区间 →
//!    `BinaryReader::read` 按偏移取原始字节（`tools/read.rs` 的读取流程）；
//! 3. 写入走 `write_file` / `append_file` / `rename_file`；执行命令走 `Shell::exec`。
//!
//! # 取消与错误
//! 每个异步方法都接收 `&dyn Context`。Node 实现（见 `node.rs` 中 `FileSystem` 实现的共同
//! 约定）主要用 `context.abort_signal()` 在入口检查取消：`read_text_file` /
//! `read_binary_file` / `write_file` 与取消竞速，打开句柄的操作在打开后补查，
//! `append_file`、`truncate_file`、`flush_file` 在操作完成后补查，各读取器与 `list_dir`
//! 在每块读取或每个目录项之后补查；`absolute_path` / `join_path` 是纯路径计算、不检查
//! 取消，`create_dir` / `remove` / `rename_file` / `canonical_path` 只在入口检查，操作
//! 中途到达的取消不会中断这次调用。`Shell::exec` 在命令运行期间持续观察取消。
//! 命中时返回 `FileErrorCode::Aborted` / `ExecutionErrorCode::Aborted`。错误一律用值
//! 返回，只有 [`get_or_throw`] 会按 TS 的 `getOrThrow` 语义把错误值作为 panic payload
//! 抛出。
//!
//! # 注意事项
//! - 路径一律用字符串传递（不是 `std::path::Path`），平台差异由实现负责；`node:path` 的
//!   posix/win32 语义见 [`path`]。
//! - [`ShellExecOptions::timeout`] 的单位是**秒**（与 TS 字段一致），实现内部换算成毫秒。
//! - [`FileSystem`]、[`Shell`] 以及上面这些句柄 trait 都要求 `Send + Sync`，因此可以放进
//!   `Arc<dyn ...>` 跨任务共享；实现内部仍需自己用互斥锁保护共享状态。
//!
//! # 阅读顺序
//! 本文件的 trait（先看 `FileSystem` 与 `Shell`）→ [`decode`] / [`line_scan`]（字节与行的底层
//! 处理）→ [`node`]（本地实现）→ [`path`] 与 [`node_watch`]（路径语义与文件观察）。

pub mod decode;
pub mod line_scan;
pub mod node;
pub mod node_watch;
pub mod path;

pub use decode::{RangeDecoder, StreamDecoder, decode_whole, range_decoder, starts_with_bom};
pub use line_scan::{InvalidLineRange, LineScanner};

use std::sync::Arc;

use async_trait::async_trait;
use kiana_chord::context::Context;

pub use crate::errors::BoxError;

/// `ok(value)`。
pub fn ok<T, E>(value: T) -> Result<T, E> {
    Ok(value)
}

/// `err(error)`。
pub fn err<T, E>(error: E) -> Result<T, E> {
    Err(error)
}

/// `getOrThrow(result)`：`Err` 时把**原始错误值**作为 panic payload 抛出。
pub fn get_or_throw<T, E>(result: Result<T, E>) -> T
where
    E: std::any::Any + Send + 'static,
{
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(error),
    }
}

/// `getOrUndefined(result)`：`Ok` 取内部值，`Err` 被丢弃并返回 `None`。
pub fn get_or_undefined<T, E>(result: Result<T, E>) -> Option<T> {
    result.ok()
}

/// `toError(error)`：Rust 里错误已经是 `Error`，这里把它装箱并保留类型。
pub fn to_error<E>(error: E) -> BoxError
where
    E: std::error::Error + Send + Sync + 'static,
{
    Box::new(error)
}

/// `FileKind`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    /// 普通文件。
    File,
    /// 目录。
    Directory,
    /// 符号链接（不会被跟随）。
    Symlink,
}

/// `FileErrorCode`。
///
/// serde 表示是 snake_case 字符串（如 `not_found`）；`testing/env_conformance.rs` 的期望 JSON
/// 也使用这些名字，改变枚举变体名会改变对外表现。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileErrorCode {
    /// 上下文已取消。
    Aborted,
    /// 不存在。
    NotFound,
    /// 权限不足。
    PermissionDenied,
    /// 路径的上级不是目录。
    NotDirectory,
    /// 期望文件却是目录。
    IsDirectory,
    /// 参数或目标不合法。
    Invalid,
    /// 环境不支持该操作。
    NotSupported,
    /// 其他失败。
    Unknown,
}

/// `FileError`：文件系统操作的预期失败。
///
/// `Display` 只输出 `message`。`cause` 是共享指针（`Arc`），克隆错误不会深拷贝底层错误。
/// `PartialEq` 比较 `code`、`message`、`path` 以及 `cause` 的 `Display` 文本，而不是 `cause`
/// 的身份——两个底层错误不同但文本相同的 `FileError` 会判为相等（见下方的 `impl`）。
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct FileError {
    /// 机器可读的错误码。
    pub code: FileErrorCode,
    /// 人类可读的消息。
    pub message: String,
    /// 相关路径（如果有）。
    pub path: Option<String>,
    /// 底层原因（如果有）。
    pub cause: Option<Arc<dyn std::error::Error + Send + Sync>>,
}

impl PartialEq for FileError {
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code
            && self.message == other.message
            && self.path == other.path
            && self.cause.as_ref().map(|cause| cause.to_string())
                == other.cause.as_ref().map(|cause| cause.to_string())
    }
}

impl FileError {
    /// `new FileError(code, message)`。
    pub fn new(code: FileErrorCode, message: impl Into<String>) -> Self {
        FileError {
            code,
            message: message.into(),
            path: None,
            cause: None,
        }
    }

    /// 带路径。
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// 带底层原因。
    pub fn with_cause(mut self, cause: Arc<dyn std::error::Error + Send + Sync>) -> Self {
        self.cause = Some(cause);
        self
    }
}

/// `ExecutionErrorCode`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionErrorCode {
    /// 上下文已取消。
    Aborted,
    /// 超时。
    Timeout,
    /// 环境没有 shell。
    ShellUnavailable,
    /// 启动进程失败。
    SpawnError,
    /// 回调抛错。
    CallbackError,
    /// 其他失败。
    Unknown,
}

/// `ExecutionError`。
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ExecutionError {
    /// 机器可读的错误码。
    pub code: ExecutionErrorCode,
    /// 人类可读的消息。
    pub message: String,
    /// 超时或被取消时，输出越过 spill 阈值后写入的临时文件。
    pub spill_path: Option<String>,
    /// 底层原因（如果有）。
    pub cause: Option<Arc<dyn std::error::Error + Send + Sync>>,
}

impl ExecutionError {
    /// `new ExecutionError(code, message)`。
    pub fn new(code: ExecutionErrorCode, message: impl Into<String>) -> Self {
        ExecutionError {
            code,
            message: message.into(),
            spill_path: None,
            cause: None,
        }
    }

    /// 带 spill 文件。
    pub fn with_spill_path(mut self, spill_path: impl Into<String>) -> Self {
        self.spill_path = Some(spill_path.into());
        self
    }

    /// 带底层原因。
    pub fn with_cause(mut self, cause: Arc<dyn std::error::Error + Send + Sync>) -> Self {
        self.cause = Some(cause);
        self
    }
}

/// `FileInfo`。
#[derive(Debug, Clone, PartialEq)]
pub struct FileInfo {
    /// 文件名。
    pub name: String,
    /// 路径。
    pub path: String,
    /// 类型。
    pub kind: FileKind,
    /// 字节数。
    pub size: u64,
    /// 修改时间（Unix 毫秒，可含小数）。
    pub mtime_ms: f64,
}

/// `TextLine`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextLine {
    /// 行内容（不含换行）。
    pub text: String,
    /// 是否以换行结束。
    pub terminated: bool,
}

/// `TextLineReader`。
///
/// 由 [`FileSystem::open_text_line_reader`] 创建。`read_line` 返回的 `text` 不含行尾换行；
/// 最后一行没有换行时 `terminated` 为 `false`（对应 [`TextLine`] 的字段语义）。
#[async_trait]
pub trait TextLineReader: Send + Sync {
    /// 读取下一行；文件结束时返回 `None`。
    async fn read_line(&self, context: &dyn Context) -> Result<Option<TextLine>, FileError>;
    /// 关闭读取器。
    async fn close(&self, context: &dyn Context);
}

/// `scanLines` 的 options。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    /// 起始行（0 起）。
    pub start_line: u64,
    /// 结束行（不含）；`None` 表示到文件末尾。
    pub end_line: Option<u64>,
}

/// `BinaryReader`：对一个已打开普通文件的位置读取；路径改名后仍读同一个文件。
#[async_trait]
pub trait BinaryReader: Send + Sync {
    /// 已打开文件的元数据（不是路径现在指向的东西）。
    async fn info(&self, context: &dyn Context) -> Result<FileInfo, FileError>;
    /// `offset` 起最多 `length` 字节；仅在文件末尾会少。
    async fn read(
        &self,
        offset: u64,
        length: usize,
        context: &dyn Context,
    ) -> Result<Vec<u8>, FileError>;
    /// 一遍扫描 `[startLine, endLine)` 行（0 起，第 `k` 行在第 `k` 个换行字节之后开始）。
    ///
    /// 解码字节数与 `new TextDecoder().decode(file)` 对该区间产出的文本一致，因此文件开头的
    /// 字节序标记不计入。
    ///
    /// 行区间非法（`end_line <= start_line`）时返回 `code = FileErrorCode::Invalid` 的
    /// [`FileError`]，消息为 `Invalid line range`；Node 实现由 `LineScanner::new` 的错误映射而来。
    async fn scan_lines(
        &self,
        range: LineRange,
        context: &dyn Context,
    ) -> Result<LineScan, FileError>;
    /// 关闭读取器。
    async fn close(&self, context: &dyn Context);
}

/// `WatchTarget`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchTarget {
    /// 要观察的路径；可以不存在，创建它就是一次变化。
    pub path: String,
    /// 观察目录下的所有内容（不跟随符号链接）。
    pub recursive: Option<bool>,
    /// 目录下既不观察也不报告的名字。
    pub exclude: Option<WatchExclude>,
}

/// `WatchTarget.exclude`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchExclude {
    /// 排除点号开头的名字。
    pub hidden: Option<bool>,
    /// 排除这些名字。
    pub names: Option<Vec<String>>,
}

/// `WatchChange`。
#[derive(Debug, Clone, PartialEq)]
pub enum WatchChange {
    /// 这些路径下可能发生了变化（目录路径覆盖整棵子树）。
    Paths(Vec<String>),
    /// 覆盖范围一度不确定（丢事件、重连），重扫所有被观察的内容。
    Overflow,
    /// 观察器停止。
    Error(FileError),
}

/// `FileWatcher.mode`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WatchMode {
    /// 文件系统事件：约两秒内送达。
    Native,
    /// 快照轮询：两次快照之间撤销的变化可能遗漏。
    Polling,
}

/// `FileWatcher`。
///
/// 由 [`FileSystem::watch`] 创建；变化通过传给 `watch` 的 [`WatchCallback`] 推送，而不是从本
/// trait 拉取。`close` 返回后不再有回调（Node 实现用关闭标志与取消令牌保证）。
#[async_trait]
pub trait FileWatcher: Send + Sync {
    /// 观察模式。
    fn mode(&self) -> WatchMode;
    /// 停止观察；幂等，返回后不再有 `on_change` 调用。
    async fn close(&self, context: &dyn Context);
}

/// `LineScan`：`BinaryReader.scanLines` 找到的行位置。
///
/// 偏移字段（`start`/`end`/`first_line_end`/`last_line_start`）是**原始字节**偏移；
/// `selected_bytes`/`first_line_bytes` 是**解码后文本**的 UTF-8 字节长度（非法序列按 U+FFFD
/// 记 3 字节），所以可能大于原始跨度。`newlines` 统计整份文件，而不是选区。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineScan {
    /// 整份文件的换行字节数；文件有 `newlines + 1` 行。
    pub newlines: u64,
    /// 所选行的字节区间起点。
    pub start: usize,
    /// 所选行的字节区间终点（不含结束换行）；越过最后一行的选区是文件末尾。
    pub end: usize,
    /// 第一行结束的位置（换行或文件末尾）。
    pub first_line_end: usize,
    /// 最后一行开始的位置。
    pub last_line_start: usize,
    /// 所选区间解码后的 UTF-8 字节长度。
    pub selected_bytes: usize,
    /// 第一行解码后的 UTF-8 字节长度。
    pub first_line_bytes: usize,
}

/// `DirReader.next()` 的一页。
#[derive(Debug, Clone, PartialEq)]
pub struct DirPage {
    /// 这一页的条目。
    pub entries: Vec<FileInfo>,
    /// 是否已到末尾。
    pub done: bool,
}

/// `DirReader`。
#[async_trait]
pub trait DirReader: Send + Sync {
    /// 文件系统顺序的最多 `max_entries` 个条目，接着上次的位置；失败后应关闭。
    async fn next(&self, max_entries: usize, context: &dyn Context) -> Result<DirPage, FileError>;
    /// 关闭读取器。
    async fn close(&self, context: &dyn Context);
}

/// `ListFilesOptions`。
///
/// 每个字段都是 `Option`：`None` 表示使用实现默认值。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListFilesOptions {
    /// 包含点号开头的名字（Windows 上也包含隐藏属性文件）；默认 true。
    pub hidden: Option<bool>,
    /// 在 git 工作树里跳过被忽略的文件；默认 true。
    pub gitignore: Option<bool>,
    /// 连同其下所有内容一起跳过的名字，如 `node_modules`。
    pub exclude: Option<Vec<String>>,
    /// 最多列出多少条目；默认 50 000。
    pub max_entries: Option<usize>,
    /// 列表最多多少字节；默认 4 MiB，上限 15.9 MiB。
    pub max_bytes: Option<usize>,
    /// 列表超时；默认 10 000 ms。
    pub timeout_ms: Option<f64>,
}

/// `FileListEntry`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileListEntry {
    /// 相对被列出目录的路径，一律用 `/` 分隔。
    pub path: String,
    /// 类型；符号链接按符号链接列出，不跟随。
    pub kind: FileKind,
}

/// `FileListing`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileListing {
    /// 深度优先，父目录先于其条目，兄弟按名字字节序。
    pub entries: Vec<FileListEntry>,
    /// 某个上限让列表提前结束。
    pub truncated: bool,
}

/// `readTextLines` 的 options。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadTextLinesOptions {
    /// 最多读取的行数。
    pub max_lines: Option<usize>,
}

/// `openBinaryReader` 的 options。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpenBinaryReaderOptions {
    /// 最后一个路径组件是符号链接时返回 `invalid`，而不是跟随。
    pub no_follow: Option<bool>,
}

/// 写入内容（TS: `string | Uint8Array`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    /// 文本。
    Text(String),
    /// 原始字节。
    Bytes(Vec<u8>),
}

/// `createDir` 的 options。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CreateDirOptions {
    /// 递归创建。
    pub recursive: Option<bool>,
}

/// `remove` 的 options。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemoveOptions {
    /// 递归删除目录。
    pub recursive: Option<bool>,
    /// 不存在时不报错。
    pub force: Option<bool>,
}

/// `createTempFile` 的 options。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateTempFileOptions {
    /// 文件名前缀。
    pub prefix: Option<String>,
    /// 文件名后缀。
    pub suffix: Option<String>,
}

/// 观察回调（TS: `onChange: (change: WatchChange) => void`）。
pub type WatchCallback = Arc<dyn Fn(WatchChange) + Send + Sync>;

/// `ShellSpillOptions`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellSpillOptions {
    /// 输出超过该字节数后写 spill 文件。
    pub after_bytes: usize,
    /// 输出超过该行数后写 spill 文件（可含不完整行）。
    pub after_lines: usize,
}

/// `ShellExecResult`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellExecResult {
    /// 退出码。
    pub exit_code: i32,
    /// 越过 spill 阈值时保存完整原始输出的临时文件。
    pub spill_path: Option<String>,
}

/// 输出流的名字。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellOutputStream {
    /// 标准输出。
    Stdout,
    /// 标准错误。
    Stderr,
}

/// `ShellOutputSkip`：环境省掉的输出，按 **`onOutput` 会收到的解码文本** 计量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ShellOutputSkip {
    /// 被省略文本的 UTF-8 字节长度（每个 U+FFFD 记 3 字节）。
    pub bytes: usize,
    /// 被省略文本里的换行（U+000A）数。
    pub newlines: usize,
    /// 被省略文本是否以换行结尾。
    pub ends_with_newline: bool,
}

/// `ShellOutputWindow`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShellOutputWindow {
    /// 保留在输出末尾的 UTF-8 字节数。
    pub max_bytes: usize,
    /// 保留在输出末尾的行数。
    pub max_lines: usize,
    /// 调用方两次取样之间的最短停顿。
    pub min_interval_ms: f64,
    /// 每次取样还按该速率（字节/秒）让调用方停顿与大小成比例的时间。
    pub bytes_per_second: f64,
}

/// `ShellOutputInfo`。
#[derive(Debug, Clone, PartialEq)]
pub struct ShellOutputInfo {
    /// 数据来自哪个流。
    pub stream: ShellOutputStream,
    /// 紧接本块之前被省略的输出（仅在给出 `window` 时出现）。
    pub skipped: Option<ShellOutputSkip>,
}

/// `onOutput` 回调。
pub type OutputCallback = Arc<dyn Fn(&str, &dyn Context, &ShellOutputInfo) + Send + Sync>;

/// `ShellExecOptions`。
///
/// `Debug` 为回调单独实现：`on_output` 只打印成 `<callback>`，不会把回调当数据展开。
#[derive(Clone, Default)]
pub struct ShellExecOptions {
    /// 工作目录。
    pub cwd: Option<String>,
    /// 额外环境变量。
    pub env: Option<indexmap::IndexMap<String, String>>,
    /// 是否继承环境；默认 true。
    pub inherit_env: Option<bool>,
    /// 超时（**秒**）：实现按 `timeout * 1000` 换算成毫秒。非有限值或 `<= 0` 报
    /// `Invalid timeout: must be a finite number of seconds`；超过 `MAX_TIMEOUT_SECONDS`
    /// （`2_147_483_647 / 1000`）报 `Invalid timeout: maximum is ${MAX_TIMEOUT_SECONDS} seconds`。
    pub timeout: Option<f64>,
    /// stdout/stderr 的每个解码块，按到达顺序。
    pub on_output: Option<OutputCallback>,
    /// 越过阈值后把完整输出写入临时文件。
    pub spill: Option<ShellSpillOptions>,
    /// 调用方只保留的输出尾部。
    pub window: Option<ShellOutputWindow>,
}

impl std::fmt::Debug for ShellExecOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShellExecOptions")
            .field("cwd", &self.cwd)
            .field("env", &self.env)
            .field("inherit_env", &self.inherit_env)
            .field("timeout", &self.timeout)
            .field("on_output", &self.on_output.as_ref().map(|_| "<callback>"))
            .field("spill", &self.spill)
            .field("window", &self.window)
            .finish()
    }
}

/// `exec` 的命令：字符串走环境的 shell，数组直接以 `[0]` 为程序、其余为参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellCommand {
    /// 通过 shell 执行。
    Shell(String),
    /// 直接执行（不经 shell 解析）。
    Argv(Vec<String>),
}

/// `FileSystem`：可移植的文件系统能力。操作返回失败而不是抛出。
///
/// 路径参数与返回值都是环境视角的字符串路径（不是 `std::path::Path`），平台差异由实现负责
/// （`node:path` 语义见 [`path`]）。所有异步方法都接收 `&dyn Context`，实现用它检查取消；
/// 检查的时点因实现和方法而异：例如 Node 实现的纯路径计算（`absolute_path`/`join_path`）
/// 不检查，`create_dir`/`remove`/`rename_file`/`canonical_path` 只在入口检查，操作中途到达
/// 的取消不会中断调用（见 [`node`] 模块中 `FileSystem` 实现的共同约定）。
#[async_trait]
pub trait FileSystem: Send + Sync {
    /// 文件命名空间标识。
    fn id(&self) -> &str;
    /// 当前工作目录。
    fn cwd(&self) -> &str;
    /// 设置工作目录（TS 的可写 `cwd` 属性；实现用内部可变性）。
    fn set_cwd(&self, cwd: String);
    /// 绝对路径。
    async fn absolute_path(&self, path: &str, context: &dyn Context) -> Result<String, FileError>;
    /// 拼接路径。
    async fn join_path(&self, parts: &[String], context: &dyn Context)
    -> Result<String, FileError>;
    /// 读取整个文本文件。
    async fn read_text_file(&self, path: &str, context: &dyn Context) -> Result<String, FileError>;
    /// 打开逐行读取器。
    async fn open_text_line_reader(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Arc<dyn TextLineReader>, FileError>;
    /// 读取文本行。
    async fn read_text_lines(
        &self,
        path: &str,
        options: Option<&ReadTextLinesOptions>,
        context: &dyn Context,
    ) -> Result<Vec<String>, FileError>;
    /// 读取整个二进制文件。
    async fn read_binary_file(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Vec<u8>, FileError>;
    /// 打开有界位置读取器。
    async fn open_binary_reader(
        &self,
        path: &str,
        options: Option<&OpenBinaryReaderOptions>,
        context: &dyn Context,
    ) -> Result<Arc<dyn BinaryReader>, FileError>;
    /// 写文件（覆盖）。
    async fn write_file(
        &self,
        path: &str,
        content: FileContent,
        context: &dyn Context,
    ) -> Result<(), FileError>;
    /// 追加文件。
    async fn append_file(
        &self,
        path: &str,
        content: FileContent,
        context: &dyn Context,
    ) -> Result<(), FileError>;
    /// 把文件截断或扩展到正好 `size` 字节。
    async fn truncate_file(
        &self,
        path: &str,
        size: u64,
        context: &dyn Context,
    ) -> Result<(), FileError>;
    /// 把文件内容与取回所需的元数据刷到磁盘。
    async fn flush_file(&self, path: &str, context: &dyn Context) -> Result<(), FileError>;
    /// 重命名。
    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        context: &dyn Context,
    ) -> Result<(), FileError>;
    /// 元数据。
    async fn file_info(&self, path: &str, context: &dyn Context) -> Result<FileInfo, FileError>;
    /// 列出一层目录。
    async fn list_dir(&self, path: &str, context: &dyn Context)
    -> Result<Vec<FileInfo>, FileError>;
    /// 递归列出文件、目录与符号链接（可选能力；不支持时返回 `not_supported`）。
    async fn list_files(
        &self,
        _path: &str,
        _options: Option<&ListFilesOptions>,
        _context: &dyn Context,
    ) -> Result<FileListing, FileError> {
        Err(FileError::new(
            FileErrorCode::NotSupported,
            "listFiles is not supported by this environment",
        ))
    }
    /// 打开目录分页读取器。
    async fn open_dir_reader(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Arc<dyn DirReader>, FileError>;
    /// 观察文件与目录的变化。
    async fn watch(
        &self,
        targets: &[WatchTarget],
        on_change: WatchCallback,
        context: &dyn Context,
    ) -> Result<Arc<dyn FileWatcher>, FileError>;
    /// 规范化路径。
    async fn canonical_path(&self, path: &str, context: &dyn Context) -> Result<String, FileError>;
    /// 是否存在。
    async fn exists(&self, path: &str, context: &dyn Context) -> Result<bool, FileError>;
    /// 创建目录。
    async fn create_dir(
        &self,
        path: &str,
        options: Option<&CreateDirOptions>,
        context: &dyn Context,
    ) -> Result<(), FileError>;
    /// 删除文件或目录。
    async fn remove(
        &self,
        path: &str,
        options: Option<&RemoveOptions>,
        context: &dyn Context,
    ) -> Result<(), FileError>;
    /// 创建临时目录。
    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        context: &dyn Context,
    ) -> Result<String, FileError>;
    /// 创建临时文件。
    async fn create_temp_file(
        &self,
        options: Option<&CreateTempFileOptions>,
        context: &dyn Context,
    ) -> Result<String, FileError>;
    /// 释放环境。
    async fn cleanup(&self, context: &dyn Context);
}

/// `Shell`。
///
/// 与 [`FileSystem`] 一起组成 [`ExecutionEnv`]：`exec` 的取消/超时只作用于当次命令的进程，
/// `cleanup` 负责该环境仍在运行的所有命令（契约见各方法注释）。
#[async_trait]
pub trait Shell: Send + Sync {
    /// 执行命令。取消上下文或超时只杀死这条命令的进程。
    async fn exec(
        &self,
        command: ShellCommand,
        options: Option<&ShellExecOptions>,
        context: &dyn Context,
    ) -> Result<ShellExecResult, ExecutionError>;
    /// 杀死该环境仍在运行的所有命令；用于其所有者的关闭。
    async fn cleanup(&self, context: &dyn Context);
}

/// `ExecutionEnv extends FileSystem, Shell`。
///
/// # blanket impl
/// 任何实现 `FileSystem` 与 `Shell` 的类型都自动实现本 trait（下面的 impl 带 `?Sized`，
/// 对 trait 对象同样成立），所以下游不需要也不应该为具体类型手写 `impl ExecutionEnv`。
pub trait ExecutionEnv: FileSystem + Shell {}

impl<T: FileSystem + Shell + ?Sized> ExecutionEnv for T {}
