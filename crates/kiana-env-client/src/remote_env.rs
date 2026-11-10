//! remote-env.ts（957 行，客户端语义核心）：另一台机器上的 `ExecutionEnv`，经 kiana-env daemon 访问。
//!
//! 结果与那台机器上运行 `NodeExecutionEnv` 相同：系统调用由 daemon 执行，本类应用 Node 的路径、
//! 错误与结果规则。逐函数对照 TS 源；关键语义见每条注释。
//!
//! 与 TS 的差异（Rust 类型系统/冻结契约导致的，均已就地注明）：
//! - `string | Uint8Array` → [`FileContent`]；`Result<T, E>` → `Result<T, E>`。
//! - TS 的 `Number.isSafeInteger(x)`（`x >= 0`）在 Rust 的 `u64`/`usize` 上仍**可达**：
//!   `2^53` 以上的值可以传进来，因此逐个检查（见 `MAX_SAFE_INTEGER`），与 TS 一样在发请求前
//!   以 `invalid` 失败。
//! - `cwd` 是可写的 TS 属性；冻结的 `FileSystem::cwd()` 返回 `&str`，用 `Box::leak` 实现
//!   （与 `kiana_durable::env::node` 相同）。
//! - `StringDecoder` → [`kiana_durable::env::decode::StreamDecoder`]；`readFile` 的编码路径用
//!   [`kiana_durable::env::decode::RangeDecoder`]（Node 的 `StringDecoder` **保留** BOM）。
//!
//! # 职责与边界
//!
//! 本文件实现 `kiana_durable::env` 的 [`FileSystem`] 与 [`Shell`]（两者合起来即 `ExecutionEnv`），
//! 但自身不执行系统调用：文件与命令操作最终都编码成 [`Connection`] 的请求帧发给远端 daemon，
//! 再把 daemon 的 JSON 回复还原成 Rust 类型。因此这里集中了**客户端侧**的三类规则：
//! - 路径：按远端平台选用 posix/win32 规则（`~`、`file://`、绝对/相对路径、Windows 盘符相对路径）；
//! - 错误：带路径的 `file_op` 操作会把 daemon 的错误码经 `to_file_error` 映射成 `FileError`，
//!   并以解析后的远端路径作为 fallback；`join_path`/`create_temp_dir`/`watch` 不设 fallback
//!   path，`list_files` 的解析失败归属原始路径；
//! - 数据搬运：大文件按固定块大小分块，多个请求同时在途，并在块与块之间检查 abort。
//!
//! [`RemoteExecutionEnv`] 与 [`RemoteExecutionEnvOptions`] 由 `crate::index` 再导出；在本仓库里，
//! 直接构造它们的是本 crate 的集成测试（如 `tests/remote.rs`），上层按 `ExecutionEnv`
//! （`FileSystem + Shell`）的契约使用。
//!
//! # 一次文件操作的执行流程
//!
//! 1. 调用方给出（可能相对的）路径；`resolve` 先用 `connection.info()` 取得远端平台、home 与
//!    cwd，再按 `resolve_path` 的规则得到绝对路径。解析失败时错误归属原始路径。
//! 2. abort 检查（`abort_result`）。
//! 3. 发送一条或多条请求。`open`/`opendir` 返回的 handle 绑定在回复的 daemon session 上，
//!    后续 handle 请求都固定这个 session；连接重启后这类请求直接以 `lost` 失败，不会把旧句柄
//!    编号发给新的 daemon。
//! 4. 回复里的错误对象包装成 `RemoteError`，最终经 `to_file_error` 变成 `FileError`；调用方
//!    看到的错误码由 `to_file_error` 的映射表决定（例如 `ENOENT` → `NotFound`）。
//!
//! # 可变状态与并发
//!
//! 本模块的可变状态：`RemoteExecutionEnv` 自身只有 `cwd_override`（`Mutex`，`set_cwd` 每次
//! 写入都把新值 `Box::leak` 成 `&'static str`）与 `running`（`Mutex<IndexMap>`，记录尚未结算的
//! `exec` 请求 id 与 session，供 `cleanup` 杀死）两个 `Mutex`；三个 reader（文本行/二进制/目录）
//! 是各自打开方法返回的独立对象，各自持有 `closed` 原子标志，`RemoteDirReader` 另有 `done`，
//! `RemoteTextLineReader` 另有 `state` Mutex（解码器、偏移、缓冲与结束标记）。
//! 锁只在短临界区持有，不在
//! `await` 期间持锁；文本行读取器因此存在并发调用的窗口（见该类型的注释）。`FileSystem`/
//! `Shell` 要求 `Send + Sync`，实现满足这一点才能作为 `ExecutionEnv` 使用。
//!
//! # 注意事项（修改前先读）
//!
//! - handle 类操作（binary/line/dir reader）失败后调用方应 `close`（`DirReader` 的 trait 文档
//!   明确要求）；`close` 幂等。
//! - abort 只在代码里的检查点生效：`fstat`/`readdir`/`pread` 等请求本身不带 `signal`，已经
//!   发出的请求不会被取消，只有在块与块之间或请求前后才能观察到 abort。
//! - 收尾代码（`close`、排空在途请求）只在函数正常/错误返回时执行；future 被调用方直接丢弃
//!   时不会运行，daemon 端的 handle 会留到 session 结束。
//! - 本文件的 JSON 字段名属于 daemon 协议（`mtimeSec`、`startLine`、`writeChunk` 等），改动会
//!   破坏与已部署 daemon 的兼容性；帧与操作的完整定义见 `crates/kiana-env/docs/protocol.md`。
//! - 冻结的 `FileSystem::cwd()` 返回 `&str`，`set_cwd` 的旧值无法回收（每次泄漏一个小字符串）。
//! - [`RemoteExecutionEnv`] 与 [`RemoteExecutionEnvOptions`] 在 crate 根再导出；修改公开字段或
//!   方法签名属于公开 API 变化，会影响所有使用 `kiana-env-client` 的调用方。
//!
//! # 阅读顺序
//!
//! 常量与 [`RemoteExecutionEnvOptions`] → `resolve_path`/`file_url_to_path`（路径规则）→
//! `file_op`/`read_file`/`write_content`（请求与分块）→ 三个 reader → [`Shell::exec`]
//! （事件循环与错误映射）→ 末尾的测试（锁定消息文本与 Node 行为）。
//!
//! TS: packages/env/src/remote-env.ts

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_chord::context::{AbortSignal, Context};
use kiana_durable::env::decode::{RangeDecoder, StreamDecoder};
use kiana_durable::env::path::{PathFlavor, posix, win32};
use kiana_durable::env::{
    BinaryReader, CreateDirOptions, CreateTempFileOptions, DirPage, DirReader, ExecutionError,
    ExecutionErrorCode, FileContent, FileError, FileErrorCode, FileInfo, FileKind, FileListing,
    FileSystem, FileWatcher, LineRange, LineScan, ListFilesOptions, OpenBinaryReaderOptions,
    ReadTextLinesOptions, RemoveOptions, Shell, ShellCommand, ShellExecOptions, ShellExecResult,
    ShellOutputInfo, ShellOutputSkip, ShellOutputStream, TextLine, TextLineReader, WatchCallback,
    WatchTarget,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::connection::{Connection, Json, RemoteError, node_platform};
use crate::errors::{EnvError, abort_result, to_file_error};
use crate::list_files::list_files_over;
use crate::watch::{RemoteWatchOptions, RemoteWatcher};

/// `remote-env.ts` 的 `export type { RemoteInfo }`：模块级再导出（包入口仍只导出一次）。
pub use crate::connection::RemoteInfo;

// `exec` 的 timeout 上限（2^31-1 毫秒）；timeout 以秒传入，超过这个值的秒数在发请求前被
// 拒绝，错误消息里用 `MAX_TIMEOUT_SECONDS` 的十进制拼写（测试锁定文案）。
const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
const MAX_TIMEOUT_SECONDS: f64 = MAX_TIMEOUT_MS / 1000.0;
/// 二进制读取器每次 `pread` 的字节数；同时有多个在途。
const READ_CHUNK: usize = 256 * 1024;
const READ_DEPTH: usize = 8;
/// Node 的 `writeFile` 每次调用最多写这么多，并在每块前检查 abort。
const WRITE_CHUNK: usize = 512 * 1024;
const WRITE_DEPTH: usize = 8;
/// Node 的 `readFile`：已知大小的读块、未知大小的读块，以及它能读的最大文件。
const READ_FILE_CHUNK: usize = 512 * 1024;
const READ_FILE_UNKNOWN_CHUNK: usize = 64 * 1024;
const READ_FILE_MAX: u64 = 2_147_483_647;
/// `NodeTextLineReader` 的读取块。
const LINE_CHUNK: usize = 64 * 1024;
/// JS 的 `Number.MAX_SAFE_INTEGER`：`Number.isSafeInteger` 的上界（`2^53 - 1`）。
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// 一次 [`read_range`] 里在途的读取：期望长度与**已经发出**的请求。
type InFlight = (usize, crate::connection::ReplyNow);

/// 取锁；中毒（持锁线程 panic）时取回内部值继续，而不是把一次 panic 变成所有后续调用的
/// panic。这里保护的都是普通数据（cwd 指针、请求表、行读取状态），中毒后仍可安全使用。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `new RemoteExecutionEnv(options)` 的选项。
///
/// 字段在构造时读取：`watch: None` 会换成 `RemoteWatchOptions::default()`，其余字段原样保存
/// （`cwd` 之后仍可被 `set_cwd` 覆盖）。
///
/// TS: packages/env/src/remote-env.ts#RemoteExecutionEnvOptions
#[derive(Clone)]
pub struct RemoteExecutionEnvOptions {
    /// 到 daemon 的连接。
    pub connection: Arc<Connection>,
    /// 文件命名空间：相同的 id 看到相同的文件（`FileSystem.id`），例如 `kiana-env:<env name>`。
    pub id: String,
    /// 当前工作目录。相对路径解析的基准，也是 `exec` 没给 `cwd` 时直接使用的工作目录
    /// （这个默认值不再经过 `resolve`）。
    pub cwd: String,
    /// 显式的 shell 路径。`Some` 时作为 `shellPath` 字段发送，`None` 时不发送。
    pub shell_path: Option<String>,
    /// 加到每条继承环境的命令上的环境变量；仅在 `inherit_env` 为 true 时并入，
    /// `ShellExecOptions::env` 随后写入，因此同名键以调用方为准。
    pub shell_env: Option<IndexMap<String, String>>,
    /// daemon 的观察选项。`None` 在构造 `RemoteExecutionEnv` 时换成 `RemoteWatchOptions::default()`；
    /// `RemoteExecutionEnvOptions` 本身保持 `None`。
    pub watch: Option<RemoteWatchOptions>,
}

impl RemoteExecutionEnvOptions {
    /// 只给出连接、id 与 cwd 的选项；其余字段为 `None`（`watch: None` 在构造
    /// `RemoteExecutionEnv` 时取默认值）。
    pub fn new(connection: Arc<Connection>, id: impl Into<String>, cwd: impl Into<String>) -> Self {
        RemoteExecutionEnvOptions {
            connection,
            id: id.into(),
            cwd: cwd.into(),
            shell_path: None,
            shell_env: None,
            watch: None,
        }
    }
}

/// daemon 发来的 info 记录。
///
/// daemon 的 info 记录是 `{ name, kind, size, mtimeSec, mtimeNsec, dev, ino }`；这里只反序列化
/// `kind`/`size`/`mtimeSec`/`mtimeNsec`，`name`（由 `to_info` 用 path 的 basename 计算）与
/// `dev`/`ino` 都被忽略。数值用 `f64` 承载 JSON number；`size`/`mtime` 缺失时由 serde 填 0；
/// `kind` 没有默认值，缺失会让整个解析失败。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteFileInfo {
    kind: String,
    #[serde(default)]
    size: f64,
    #[serde(default)]
    mtime_sec: f64,
    #[serde(default)]
    mtime_nsec: f64,
}

/// `readdir` 的一个条目：daemon 要么给 `info`（lstat 成功），要么给 `error`（lstat 失败）。
///
/// `raw` 只在名字不是合法 UTF-8、解码会丢信息时出现，保存原始字节供 POSIX 排序使用。
#[derive(Debug, Clone)]
struct RemoteDirEntry {
    name: String,
    raw: Option<Vec<u8>>,
    info: Option<Value>,
    error: Option<Value>,
}

/// 宽松解析 `entries` 数组，保持 daemon 给的顺序。
///
/// `entries` 缺失或不是数组时返回空表；单个条目缺 `name` 时用空串，`raw` 只保留数字元素
/// 并截断成 `u8`（daemon 发的是 0..=255）。`info`/`error` 原样保留，由调用方决定如何报告。
fn parse_dir_entries(value: Option<&Value>) -> Vec<RemoteDirEntry> {
    let Some(Value::Array(entries)) = value else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|entry| RemoteDirEntry {
            name: entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            raw: entry.get("raw").and_then(Value::as_array).map(|raw| {
                raw.iter()
                    .filter_map(|byte| byte.as_u64().map(|byte| byte as u8))
                    .collect()
            }),
            info: entry.get("info").cloned(),
            error: entry.get("error").cloned(),
        })
        .collect()
}

/// Node 的 `path` 规则按远端系统选择。
///
/// 先取 `connection.info()`（必要时会建立连接并启动 daemon），把 daemon 报告的 Rust 系统名
/// 经 `node_platform` 换成 Node 的拼写（`windows` → `win32` 等），再决定用 posix 还是 win32
/// 规则。调用方拿到 [`PathFlavor`] 后自己分派到 `posix`/`win32` 函数。
///
/// # 错误
///
/// `connection.info()` 的失败原样返回（连接/部署失败，而不是文件错误）。
///
/// TS: packages/env/src/remote-env.ts#remotePath
pub async fn remote_path(connection: &Connection) -> Result<PathFlavor, RemoteError> {
    Ok(PathFlavor::from_node_platform(
        &node_platform(&connection.info().await?).platform,
    ))
}

// 以下四个函数把路径规则按远端平台分派到 `kiana_durable::env::path` 的 posix/win32 实现。
// 它们只在已经取得 `PathFlavor` 之后使用；取得平台本身要经过 `remote_path`（一次 `info` 请求）。

fn path_join(flavor: PathFlavor, parts: &[&str]) -> String {
    match flavor {
        PathFlavor::Posix => posix::join(parts),
        PathFlavor::Win32 => win32::join(parts),
    }
}

fn path_basename(flavor: PathFlavor, path: &str) -> String {
    match flavor {
        PathFlavor::Posix => posix::basename(path, None),
        PathFlavor::Win32 => win32::basename(path, None),
    }
}

fn path_is_absolute(flavor: PathFlavor, path: &str) -> bool {
    match flavor {
        PathFlavor::Posix => posix::is_absolute(path),
        PathFlavor::Win32 => win32::is_absolute(path),
    }
}

/// TS 的 `paths.resolve(parts)`：所有参数都是相对路径时，最后一级回退是**客户端进程**的 cwd
/// （Node 的 `process.cwd()`），不是远端 daemon 的 `info.cwd`。无盘符的 win32 根路径（`\foo`）
/// 也由它补盘符，因此客户端的平台与 cwd 都会影响结果。
fn path_resolve(flavor: PathFlavor, parts: &[&str]) -> String {
    match flavor {
        PathFlavor::Posix => posix::resolve_string(parts),
        PathFlavor::Win32 => win32::resolve_string(parts),
    }
}

/// `fileURLToPath(url, { windows })`；失败时返回 `None`，调用方保留原字符串（与 Node 一致）。
///
/// 复刻 Node `internal/url` 的校验（WHATWG 解析由 `url` crate 完成）：
/// - 路径里不允许编码的分隔符：`%2f`（两种平台），Windows 还拒绝 `%5c`；
/// - POSIX：host 必须为空（`localhost` 已被 WHATWG 归一为空串），否则 `ERR_INVALID_FILE_URL_HOST`；
/// - Windows：非空 host 组成 UNC 路径 `\\host\path`；否则要求盘符绝对路径（去掉前导 `\`），
///   没有盘符就抛 `ERR_INVALID_FILE_URL_PATH`；
/// - `decodeURIComponent` 的失败（坏的 `%` 转义、非法 UTF-8）同样对应 `None`。
///
/// WHATWG 的路径状态还把首段的 `C|` 归一成 `C:`（Node 的 URL 解析器做了，`url` crate 没有）。
/// 唯一没有复刻的细节：Node 对 UNC host 调 `domainToUnicode`（punycode → Unicode），这里保留
/// `url` crate 的 punycode 拼写（需要 `idna` 依赖，不在本 crate 的允许清单里）。
fn file_url_to_path(text: &str, windows: bool) -> Option<String> {
    let url = url::Url::parse(text).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    let pathname = url.path();
    if has_encoded_separator(pathname, windows) {
        return None;
    }
    // `file:///C|/x` 的 URL 路径是 `/C:/x`：首段恰好是 `<letter>|` 时把 `|` 当作盘符冒号。
    let pathname = if is_drive_pipe_segment(pathname) {
        let mut normalized = pathname.to_owned();
        normalized.replace_range(2..3, ":");
        normalized
    } else {
        pathname.to_owned()
    };
    let host = url.host_str().unwrap_or("");
    if !windows {
        // `file://server/share` 在 POSIX 上抛 `ERR_INVALID_FILE_URL_HOST`。
        if !host.is_empty() {
            return None;
        }
        return decode_percent(&pathname);
    }
    // Node 先把 `/` 换成 `\` 再解码；编码的 `\`/`/` 已经在上面被拒绝。
    let path = decode_percent(&pathname.replace('/', "\\"))?;
    if !host.is_empty() {
        // UNC：路径以 `\` 开头（来自前导 `/`），拼成 `\\host\share\…`。
        return Some(format!("\\\\{host}{path}"));
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'\\' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        return Some(path[1..].to_owned());
    }
    // 相对路径（没有盘符）在 Windows 上抛 `ERR_INVALID_FILE_URL_PATH`。
    None
}

/// 路径首段是否恰好是 `<ASCII 字母>|`（WHATWG 会把其中的 `|` 归一成 `:`）。
fn is_drive_pipe_segment(pathname: &str) -> bool {
    let bytes = pathname.as_bytes();
    bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b'|'
        && (bytes.len() == 3 || bytes[3] == b'/')
}

/// Node 在 `url.pathname` 里拒绝编码的分隔符：`%2f`，Windows 还有 `%5c`。
fn has_encoded_separator(pathname: &str, windows: bool) -> bool {
    let bytes = pathname.as_bytes();
    let mut index = 0usize;
    while index + 2 < bytes.len() {
        if bytes[index] == b'%' {
            let first = bytes[index + 1].to_ascii_lowercase();
            let second = bytes[index + 2].to_ascii_lowercase();
            if (first == b'2' && second == b'f') || (windows && first == b'5' && second == b'c') {
                return true;
            }
        }
        index += 1;
    }
    false
}

/// `decodeURIComponent` 的最小实现：只处理 `%HH` 转义，其余字节原样保留。
///
/// 坏的转义（不足两位、非十六进制）或解码后不是合法 UTF-8 时返回 `None`；Node 在这里抛
/// `URIError`，调用方把失败当成“不能转换的 file URL”。
fn decode_percent(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `^([a-zA-Z]:)(?![\\/])`：Windows 的盘符相对路径。
fn drive_relative(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let next = bytes.get(2).copied();
        if next != Some(b'\\') && next != Some(b'/') {
            return Some(path[..2].to_owned());
        }
    }
    None
}

/// 把 daemon 的 info JSON 转成 [`FileInfo`]；`name` 取 `path` 的 basename。
///
/// - `kind` 只接受 `file`/`directory`/`symlink`，其余（包括 daemon 对 FIFO、socket、设备
///   报的 `other`）一律返回 `invalid` + `"Unsupported file type"`，并带上 `path`；
/// - `size` 缺失、非有限或为负时按 0 处理；
/// - `mtime_ms = mtime_sec * 1000 + mtime_nsec / 1e6`，可以是小数。
///
/// 反序列化失败（缺 `kind`、字段类型不对）与不支持的 kind 走同一条 `invalid` 路径。
fn to_info(path: &str, remote: &Value, flavor: PathFlavor) -> Result<FileInfo, FileError> {
    let Ok(remote) = serde_json::from_value::<RemoteFileInfo>(remote.clone()) else {
        return Err(
            FileError::new(FileErrorCode::Invalid, "Unsupported file type").with_path(path),
        );
    };
    let kind = match remote.kind.as_str() {
        "file" => FileKind::File,
        "directory" => FileKind::Directory,
        "symlink" => FileKind::Symlink,
        // `"other"` (and anything an older daemon might send) is not part of the contract.
        _ => {
            return Err(
                FileError::new(FileErrorCode::Invalid, "Unsupported file type").with_path(path),
            );
        }
    };
    let size = if remote.size.is_finite() && remote.size >= 0.0 {
        remote.size as u64
    } else {
        0
    };
    Ok(FileInfo {
        name: path_basename(flavor, path),
        path: path.to_owned(),
        kind,
        size,
        mtime_ms: remote.mtime_sec * 1000.0 + remote.mtime_nsec / 1e6,
    })
}

/// 拼接分块；只有一个块时直接克隆该块。
fn concat(chunks: &[Vec<u8>], total: usize) -> Vec<u8> {
    if chunks.len() == 1 {
        return chunks[0].clone();
    }
    let mut bytes = Vec::with_capacity(total);
    for chunk in chunks {
        bytes.extend_from_slice(chunk);
    }
    bytes
}

/// 从 `offset` 起按最多 `chunk` 字节的连续读取取 `length` 字节，多个读取在途。读到文件末尾提前结束；
/// 短读从它结束处继续（与顺序读一致）。每块后检查 `aborted`；`None` 表示被中断。
///
/// 返回 `(chunks, total)`，`total` 可能小于 `length`（文件比请求短）。`chunk` 是单次 `pread`
/// 的上限，最多 `READ_DEPTH` 个请求同时在途；每个请求在**排队时**就已发出，`aborted` 只在
/// 回复到达后调用，已经发出的请求不会被撤回。
///
/// 短读（回复短于请求长度）会让之前按“整块”假设排队的请求失效：清空在途队列，并从实际
/// 结束位置 `offset + total` 继续，避免重复或跳过字节。
async fn read_range(
    handle: &Handle,
    offset: u64,
    length: u64,
    chunk: usize,
    aborted: &(dyn Fn() -> bool + Send + Sync),
) -> Result<Option<(Vec<Vec<u8>>, usize)>, RemoteError> {
    let end = offset.saturating_add(length);
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut total = 0usize;
    let mut next = offset;
    let mut in_flight: std::collections::VecDeque<InFlight> = std::collections::VecDeque::new();
    while (total as u64) < length {
        while in_flight.len() < READ_DEPTH && next < end {
            let size = std::cmp::min(end - next, chunk as u64) as usize;
            // 排队时就发出（TS 的 `read(next, size)` 立即开始）；最多 READ_DEPTH 个在途。
            in_flight.push_back((size, handle.pread_now(Some(next), size)));
            next += size as u64;
        }
        let Some((expected, reply)) = in_flight.pop_front() else {
            break;
        };
        let bytes = reply.await?.payload;
        if aborted() {
            return Ok(None);
        }
        if bytes.is_empty() {
            break;
        }
        total += bytes.len();
        let short = bytes.len() < expected;
        chunks.push(bytes);
        if short {
            // Reads already in flight assumed a full read; continue right after this one.
            in_flight.clear();
            next = offset + total as u64;
        }
    }
    Ok(Some((chunks, total)))
}

/// 一个 daemon handle：只在打开它的 session 里有效。
///
/// `id` 是 daemon 句柄表里的编号，`session` 是 `open`/`opendir` 回复里的 session id：每个
/// handle 请求都带上 `session`，连接丢失并重启后请求直接以 `lost` 失败，而不是把旧编号发给
/// 新 daemon。`path` 是打开时的解析结果，用于错误归属与 basename 计算。
struct Handle {
    connection: Arc<Connection>,
    id: u32,
    session: u64,
    path: String,
}

impl Handle {
    /// 发一个带 `handle` 字段的请求：`json` 里的同名键会被当前句柄编号覆盖。
    async fn request(
        &self,
        op: &str,
        json: Json,
        signal: Option<AbortSignal>,
        payload: Option<Vec<u8>>,
    ) -> Result<crate::connection::Reply, RemoteError> {
        let mut json = json;
        json.insert("handle".to_owned(), Value::from(self.id));
        self.connection
            .request(
                op,
                json,
                crate::connection::RequestOptions {
                    signal,
                    payload,
                    session: Some(self.session),
                    ..crate::connection::RequestOptions::default()
                },
            )
            .await
    }

    /// 立即发出、只等待回复的请求（见 [`crate::connection::Connection::request_now`]）。
    ///
    /// TS 的 `Handle.request` 在调用时就写出帧；`pread`/`writeChunk` 依赖这一点同时保持多个
    /// 请求在途，并保持 `writeChunk` 的发送顺序（daemon 按到达顺序追加）。
    fn request_now(
        &self,
        op: &str,
        json: Json,
        payload: Option<Vec<u8>>,
    ) -> crate::connection::ReplyNow {
        let mut json = json;
        json.insert("handle".to_owned(), Value::from(self.id));
        self.connection.request_now(
            self.session,
            op,
            json,
            crate::connection::RequestOptions {
                payload,
                session: Some(self.session),
                ..crate::connection::RequestOptions::default()
            },
        )
    }

    /// 位置读取：给了 `offset` 就发送 `offset`；`None` 时省略该字段，daemon 从句柄的当前
    /// 文件位置顺序读（大小未知的文件用这种方式读到 EOF）。
    async fn pread(&self, offset: Option<u64>, length: usize) -> Result<Vec<u8>, RemoteError> {
        let mut json = Json::new();
        if let Some(offset) = offset {
            json.insert("offset".to_owned(), Value::from(offset));
        }
        json.insert("length".to_owned(), Value::from(length));
        Ok(self.request("pread", json, None, None).await?.payload)
    }

    /// 立即发出的 `pread`，返回请求的回复 future。
    fn pread_now(&self, offset: Option<u64>, length: usize) -> crate::connection::ReplyNow {
        let mut json = Json::new();
        if let Some(offset) = offset {
            json.insert("offset".to_owned(), Value::from(offset));
        }
        json.insert("length".to_owned(), Value::from(length));
        self.request_now("pread", json, None)
    }

    async fn close(&self) {
        let _ = self.request("close", Json::new(), None, None).await;
    }
}

/// daemon 端已打开文件的二进制读取器；`closed` 由 `close` 置位，之后的操作用 `invalid` 拒绝。
struct RemoteBinaryReader {
    handle: Handle,
    closed: AtomicBool,
}

impl RemoteBinaryReader {
    fn path(&self) -> &str {
        &self.handle.path
    }

    /// 已关闭时返回 `invalid` 错误（消息 `Binary reader is closed`，带打开时的路径）。
    fn closed_result<T>(&self) -> Option<Result<T, FileError>> {
        if self.closed.load(Ordering::SeqCst) {
            Some(Err(FileError::new(
                FileErrorCode::Invalid,
                "Binary reader is closed",
            )
            .with_path(self.path())))
        } else {
            None
        }
    }
}

#[async_trait]
impl BinaryReader for RemoteBinaryReader {
    async fn info(&self, context: &dyn Context) -> Result<FileInfo, FileError> {
        let signal = context.abort_signal();
        if let Some(early) = abort_result::<FileInfo>(signal.as_ref(), Some(self.path()))
            .or_else(|| self.closed_result())
        {
            return early;
        }
        // fstat 针对的是已打开的对象（路径之后被改名也不影响）；`name` 取打开路径的
        // basename，而不是路径现在指向的文件。
        let result = async {
            // TS 的 `#handle.request("fstat")` 不带 signal：abort 只在这之前检查。
            let reply = self
                .handle
                .request("fstat", Json::new(), None, None)
                .await
                .map_err(EnvError::Remote)?;
            let flavor = remote_path(&self.handle.connection)
                .await
                .map_err(EnvError::Remote)?;
            Ok(to_info(self.path(), &Value::Object(reply.json), flavor)?)
        }
        .await;
        result.map_err(|error| to_file_error(error, Some(self.path())))
    }

    async fn read(
        &self,
        offset: u64,
        length: usize,
        context: &dyn Context,
    ) -> Result<Vec<u8>, FileError> {
        let signal = context.abort_signal();
        if let Some(early) = abort_result::<Vec<u8>>(signal.as_ref(), Some(self.path()))
            .or_else(|| self.closed_result())
        {
            return early;
        }
        // `Number.isSafeInteger(offset) || offset < 0 || ...`：负数在 `u64`/`usize` 上不可达，
        // 但 `2^53` 以上的值可达，TS 在发请求前就以 `invalid` 拒绝。
        if offset > MAX_SAFE_INTEGER || (length as u64) > MAX_SAFE_INTEGER {
            return Err(FileError::new(
                FileErrorCode::Invalid,
                "Offset and length must be non-negative safe integers",
            )
            .with_path(self.path()));
        }
        // 每个 pread 请求本身不带 signal；abort 只在 read_range 的块与块之间观察，因此中止
        // 可能晚于若干块的数据传输。
        let aborted = || signal.as_ref().is_some_and(AbortSignal::is_aborted);
        let result = read_range(&self.handle, offset, length as u64, READ_CHUNK, &aborted).await;
        match result {
            Ok(None) => {
                Err(FileError::new(FileErrorCode::Aborted, "aborted").with_path(self.path()))
            }
            Ok(Some((chunks, total))) => Ok(concat(&chunks, total)),
            Err(error) => Err(to_file_error(EnvError::Remote(error), Some(self.path()))),
        }
    }

    async fn scan_lines(
        &self,
        range: LineRange,
        context: &dyn Context,
    ) -> Result<LineScan, FileError> {
        let signal = context.abort_signal();
        if let Some(early) = abort_result::<LineScan>(signal.as_ref(), Some(self.path()))
            .or_else(|| self.closed_result())
        {
            return early;
        }
        // `!Number.isSafeInteger(startLine) || startLine < 0 || (endLine !== undefined &&
        // (!Number.isSafeInteger(endLine) || endLine <= startLine))`
        if range.start_line > MAX_SAFE_INTEGER
            || range
                .end_line
                .is_some_and(|end| end > MAX_SAFE_INTEGER || end <= range.start_line)
        {
            return Err(
                FileError::new(FileErrorCode::Invalid, "Invalid line range").with_path(self.path())
            );
        }
        // 请求带 signal：中止会向 daemon 发 cancel。回复经 RawLineScan 解析；缺字段或类型不
        // 对时 serde 错误被包成 `Other`，最终映射为 `unknown` 文件错误。
        let mut json = Json::new();
        json.insert("startLine".to_owned(), Value::from(range.start_line));
        if let Some(end_line) = range.end_line {
            json.insert("endLine".to_owned(), Value::from(end_line));
        }
        let result = async {
            let reply = self
                .handle
                .request("scanLines", json, signal, None)
                .await
                .map_err(EnvError::Remote)?;
            let scan: RawLineScan = serde_json::from_value(Value::Object(reply.json))
                .map_err(|error| EnvError::Other(error.to_string()))?;
            Ok(scan.into())
        }
        .await;
        result.map_err(|error| to_file_error(error, Some(self.path())))
    }

    async fn close(&self, _context: &dyn Context) {
        // 幂等：只有第一次 close 真正发请求。
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.handle.close().await;
    }
}

/// `LineScan` 没有 serde 派生（冻结契约），用一个私有镜像解析。
///
/// 字段与 daemon 的 `scanLines` 回复一一对应（camelCase）；没有 `default`，缺字段会解析
/// 失败，而不是得到 0。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLineScan {
    newlines: u64,
    start: usize,
    end: usize,
    first_line_end: usize,
    last_line_start: usize,
    selected_bytes: usize,
    first_line_bytes: usize,
}

impl From<RawLineScan> for LineScan {
    fn from(scan: RawLineScan) -> Self {
        LineScan {
            newlines: scan.newlines,
            start: scan.start,
            end: scan.end,
            first_line_end: scan.first_line_end,
            last_line_start: scan.last_line_start,
            selected_bytes: scan.selected_bytes,
            first_line_bytes: scan.first_line_bytes,
        }
    }
}

/// daemon 端已打开目录的分页读取器。
///
/// `done` 记录上一页是否已经到末尾；`closed` 同 binary reader。一页的条目数可能少于请求的
/// `max_entries`：`error` 条目（`ENOENT` 除外）会让整次 `next` 失败，无法识别的类型则被跳过，
/// 所以“条目数”只按能转换的条目计。
struct RemoteDirReader {
    handle: Handle,
    done: AtomicBool,
    closed: AtomicBool,
}

#[async_trait]
impl DirReader for RemoteDirReader {
    async fn next(&self, max_entries: usize, context: &dyn Context) -> Result<DirPage, FileError> {
        // 检查顺序：abort → closed → maxEntries；之后按页读，直到凑够条目或 done。
        let path = &self.handle.path;
        let signal = context.abort_signal();
        if let Some(aborted) = abort_result::<DirPage>(signal.as_ref(), Some(path)) {
            return aborted;
        }
        if self.closed.load(Ordering::SeqCst) {
            return Err(
                FileError::new(FileErrorCode::Invalid, "Directory reader is closed")
                    .with_path(path),
            );
        }
        // `!Number.isSafeInteger(maxEntries) || maxEntries <= 0`。
        if max_entries == 0 || (max_entries as u64) > MAX_SAFE_INTEGER {
            return Err(FileError::new(
                FileErrorCode::Invalid,
                "maxEntries must be a positive safe integer",
            )
            .with_path(path));
        }
        let result = async {
            let flavor = remote_path(&self.handle.connection)
                .await
                .map_err(EnvError::Remote)?;
            let mut entries: Vec<FileInfo> = Vec::new();
            // Like `NodeDirReader`, count only entries that are part of the listing.
            while !self.done.load(Ordering::SeqCst) && entries.len() < max_entries {
                let mut json = Json::new();
                json.insert("max".to_owned(), Value::from(max_entries - entries.len()));
                // TS 的 `#handle.request("readdir", …)` 不带 signal：abort 只在每页之后检查。
                let reply = self
                    .handle
                    .request("readdir", json, None, None)
                    .await
                    .map_err(EnvError::Remote)?;
                if let Some(aborted) = abort_result::<DirPage>(signal.as_ref(), Some(path)) {
                    return aborted.map_err(EnvError::File);
                }
                let reply_json = Value::Object(reply.json);
                for entry in parse_dir_entries(reply_json.get("entries")) {
                    let entry_path = path_resolve(flavor, &[path, &entry.name]);
                    if let Some(error) = &entry.error {
                        let error = match error {
                            Value::Object(fields) => RemoteError::new(fields.clone()),
                            other => {
                                return Err(EnvError::File(
                                    FileError::new(FileErrorCode::Unknown, other.to_string())
                                        .with_path(entry_path),
                                ));
                            }
                        };
                        // Removed between enumeration and lstat: not part of the listing any more.
                        if error.code == "ENOENT" {
                            continue;
                        }
                        return Err(EnvError::File(to_file_error(
                            EnvError::Remote(error),
                            Some(&entry_path),
                        )));
                    }
                    if let Some(info) = &entry.info
                        && let Ok(info) = to_info(&entry_path, info, flavor)
                    {
                        entries.push(info);
                    }
                }
                // done 只在本页全部处理完后更新；条目错误会让 next 提前返回，按 trait 约定
                // 调用方应关闭这个 reader。
                self.done.store(
                    reply_json.get("done") == Some(&Value::Bool(true)),
                    Ordering::SeqCst,
                );
            }
            Ok(DirPage {
                entries,
                done: self.done.load(Ordering::SeqCst),
            })
        }
        .await;
        result.map_err(|error| to_file_error(error, Some(path)))
    }

    async fn close(&self, _context: &dyn Context) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.handle.close().await;
    }
}

/// 逐行读取器的可变状态，全部由 `RemoteTextLineReader::state` 的 `Mutex` 保护。
///
/// 可以按下面四种情况理解它：
/// - `buffered` 里有换行 → 直接切出一行返回，不读 daemon；
/// - `ended` 且 `buffered` 为空 → 已到 EOF，返回 `None`；
/// - `ended` 且还有剩余内容 → 返回最后一行，`terminated = false`；
/// - 其它 → 从 `offset` 再 `pread` 一块，解码后追加到 `buffered`。
///
/// `decoder` 暂存被块边界截断的 UTF-8 字节；`offset` 是下一个未读取的字节位置。
struct LineReaderState {
    decoder: StreamDecoder,
    offset: u64,
    buffered: String,
    ended: bool,
}

/// daemon 端文本文件的逐行读取器。
///
/// 解码用 `StreamDecoder`：它会在**流首**剥掉字节序标记（BOM），这与 `readFile` 的
/// `RangeDecoder` 不同。`state` 的锁只在检查缓冲/推进偏移时持有，`pread().await` 期间不持锁；
/// 因此并发调用 `read_line` 时，两个调用可能用同一个 `offset` 各读一遍并把相同字节各追加一次。
/// 代码没有为并发读取去重，调用方应按顺序读取。
struct RemoteTextLineReader {
    handle: Handle,
    state: Mutex<LineReaderState>,
    closed: AtomicBool,
}

#[async_trait]
impl TextLineReader for RemoteTextLineReader {
    async fn read_line(&self, context: &dyn Context) -> Result<Option<TextLine>, FileError> {
        let path = &self.handle.path;
        let signal = context.abort_signal();
        if let Some(aborted) = abort_result::<Option<TextLine>>(signal.as_ref(), Some(path)) {
            return aborted;
        }
        if self.closed.load(Ordering::SeqCst) {
            return Err(
                FileError::new(FileErrorCode::Invalid, "Text line reader is closed")
                    .with_path(path),
            );
        }
        let result = async {
            // 阶段 1：先从缓冲里切行或处理 EOF；阶段 2：需要时按 offset 读一块；阶段 3：推进
            // offset，把解码结果追加回缓冲。
            loop {
                let offset = {
                    let mut state = lock(&self.state);
                    if let Some(newline) = state.buffered.find('\n') {
                        let text = state.buffered[..newline].to_owned();
                        state.buffered.drain(..newline + 1);
                        return Ok(Some(TextLine {
                            text,
                            terminated: true,
                        }));
                    }
                    if state.ended {
                        if state.buffered.is_empty() {
                            return Ok(None);
                        }
                        let text = std::mem::take(&mut state.buffered);
                        return Ok(Some(TextLine {
                            text,
                            terminated: false,
                        }));
                    }
                    state.offset
                };
                let bytes = self
                    .handle
                    .pread(Some(offset), LINE_CHUNK)
                    .await
                    .map_err(EnvError::Remote)?;
                if let Some(aborted) = abort_result::<Option<TextLine>>(signal.as_ref(), Some(path))
                {
                    return aborted.map_err(EnvError::File);
                }
                let mut state = lock(&self.state);
                state.offset += bytes.len() as u64;
                // 空块表示 daemon 端已到 EOF：冲刷解码器残留的不完整字符并标记结束。
                if bytes.is_empty() {
                    let tail = state.decoder.decode(None);
                    state.buffered.push_str(&tail);
                    state.ended = true;
                } else {
                    let text = state.decoder.decode(Some(&bytes));
                    state.buffered.push_str(&text);
                }
            }
        }
        .await;
        result.map_err(|error| to_file_error(error, Some(path)))
    }

    async fn close(&self, _context: &dyn Context) {
        // 先清空缓冲再关闭 daemon 句柄；之后的 read_line 会看到 closed 并返回 invalid。
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        lock(&self.state).buffered.clear();
        self.handle.close().await;
    }
}

/// 读文件的结果（TS 的 `Uint8Array | string`）：由 `read_file` 的 `encoding` 参数决定分支，
/// 不做自动转换。
enum ReadFileResult {
    Text(String),
    Bytes(Vec<u8>),
}

/// 另一台机器上的 `ExecutionEnv`，经 kiana-env daemon 访问。
///
/// 同时实现 [`FileSystem`] 与 [`Shell`]，可以当作 `NodeExecutionEnv`/`LocalExecutionEnv` 的
/// 远端替身交给上层。所有请求都经公开字段 `connection` 发往同一个 daemon；本类型不缓存远端
/// 的 stat 或目录内容，也不在本地执行任何文件操作。
///
/// 配置字段没有 setter，`cwd` 可经 `set_cwd` 覆盖；`connection` 是公开字段，调用方可替换或
/// 共享它。`id` 是文件命名空间标识，`shell_env` 与观察选项在构造时固定。
///
/// TS: packages/env/src/remote-env.ts#RemoteExecutionEnv
pub struct RemoteExecutionEnv {
    /// `FileSystem::id` 返回的文件命名空间标识（构造时给定）。
    id: String,
    /// 构造时的初始 cwd；`cwd_override` 为空时由 `cwd()` 返回。
    cwd: String,
    /// `set_cwd` 写入的覆盖值；`&'static str` 来自 `Box::leak`，旧值不回收。
    cwd_override: Mutex<Option<&'static str>>,
    /// 连接。公开字段：调用方可以克隆 `Arc` 共享，或自行调用 `Connection::close`。
    pub connection: Arc<Connection>,
    /// `exec` 请求里可选发送的 `shellPath`。
    shell_path: Option<String>,
    /// 合并进 `exec` 的额外环境变量（仅在 `inherit_env` 时）。
    shell_env: Option<IndexMap<String, String>>,
    /// 传给 `RemoteWatcher::open` 的观察选项；构造时 `None` 已换成默认值。
    watch_options: RemoteWatchOptions,
    /// 本环境启动、仍在运行的命令（`cleanup()` 用）：请求 id → daemon session。
    ///
    /// 请求在 `on_start` 时登记，结算后移除；`cleanup()` 逐个 `kill`。用 `Arc` 是因为
    /// `exec` 的 `on_start` 回调必须是 `'static`，需要一份能活到请求结束的共享引用。
    running: Arc<Mutex<IndexMap<u32, u64>>>,
}

impl std::fmt::Debug for RemoteExecutionEnv {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteExecutionEnv")
            .field("id", &self.id)
            .field("cwd", &self.cwd())
            .finish_non_exhaustive()
    }
}

/// `file_op` 是否在 `run` 之后再做一次 abort 检查。
///
/// `Before` 只在执行前检查；`Both` 在执行后再检查一次，当前用于 append、truncate、fsync。
/// 这三种操作的请求内部不再观察 abort（或请求本身不带 signal），所以中止通常在远端已经
/// 生效之后才被报告；结果是 `aborted` 错误，而不是“没执行”。
#[derive(Clone, Copy, PartialEq, Eq)]
enum AbortCheck {
    Before,
    Both,
}

/// [`RemoteExecutionEnv::resolve`] 的纯计算部分：`info` 是 daemon 的报告，`cwd` 是本实例的工作
/// 目录（`set_cwd` 可改写）。
///
/// 规则按顺序应用：
/// 1. `~` → `info.home`；`~/…`（Windows 还有 `~\…`）→ `join(home, 剩余部分)`；
/// 2. `file://` 且 `file_url_to_path` 成功 → 转换结果；转换失败时保留原字符串，
///    继续走后面的规则（它不是绝对路径，最终会按相对路径拼到 `cwd` 上）；
/// 3. 绝对路径 → `path_resolve([normalized])`，最后回退是**客户端进程**的 cwd；
/// 4. Windows 的盘符相对路径（`D:foo`）且盘符与 `cwd` 不同 → 用 `info.drive_cwds["D:"]`
///    （缺失时退回 `info.cwd`）做基准；基准盘符仍不同且形如 `X:\…` 时，改用 `D:\`；
/// 5. 其余相对路径 → `path_resolve([cwd, normalized])`。
///
/// TS: packages/env/src/remote-env.ts#RemoteExecutionEnv.#resolve
fn resolve_path(info: &RemoteInfo, cwd: &str, path: &str) -> String {
    let windows = node_platform(info).platform == "win32";
    let flavor = PathFlavor::from_node_platform(&node_platform(info).platform);
    let mut normalized = path.to_owned();
    if normalized == "~" {
        normalized = info.home.clone();
    } else if normalized.starts_with("~/") || (windows && normalized.starts_with("~\\")) {
        normalized = path_join(flavor, &[&info.home, &normalized[2..]]);
    } else if normalized.starts_with("file://")
        && let Some(converted) = file_url_to_path(&normalized, windows)
    {
        normalized = converted;
    }
    if path_is_absolute(flavor, &normalized) {
        // TS: `if (paths.isAbsolute(normalized)) return paths.resolve(normalized);`
        // 回退基准是客户端进程的 `process.cwd()`，不是 daemon 报告的远端 cwd。
        return path_resolve(flavor, &[&normalized]);
    }
    if windows {
        // A drive-relative path on another drive: Node on Windows resolves it against that
        // drive's working directory (`=D:`), else its own working directory if on that drive,
        // else the drive's root.
        if let Some(drive) = drive_relative(&normalized) {
            let cwd_drive: String = cwd.chars().take(2).collect();
            if drive.to_lowercase() != cwd_drive.to_lowercase() {
                let mut base = info
                    .drive_cwds
                    .get(&drive.to_uppercase())
                    .cloned()
                    .unwrap_or_else(|| info.cwd.clone());
                let base_drive: String = base.chars().take(2).collect();
                if base_drive.to_lowercase() != drive.to_lowercase()
                    && base.as_bytes().get(2) == Some(&b'\\')
                {
                    base = format!("{drive}\\");
                }
                return path_resolve(flavor, &[&base, &normalized]);
            }
        }
    }
    path_resolve(flavor, &[cwd, &normalized])
}

impl RemoteExecutionEnv {
    /// `new RemoteExecutionEnv({ connection, id, cwd, shellPath?, shellEnv?, watch? })`。
    ///
    /// 构造时不发任何请求；daemon 连接在第一次操作（`resolve` 或实际请求）时才建立。
    pub fn new(options: RemoteExecutionEnvOptions) -> Self {
        RemoteExecutionEnv {
            id: options.id,
            cwd: options.cwd,
            cwd_override: Mutex::new(None),
            connection: options.connection,
            shell_path: options.shell_path,
            shell_env: options.shell_env,
            watch_options: options.watch.unwrap_or_default(),
            running: Arc::new(Mutex::new(IndexMap::new())),
        }
    }

    /// Node 的 `resolvePath`（远端系统的 home 与工作目录规则）。
    ///
    /// 每次调用都会取 `connection.info()`（需要时会启动 daemon），然后把信息交给纯函数
    /// `resolve_path`。失败是 `RemoteError`；调用方用 `to_file_error` 决定文件错误码与路径。
    async fn resolve(&self, path: &str) -> Result<String, RemoteError> {
        let info = self.connection.info().await?;
        Ok(resolve_path(&info, self.cwd(), path))
    }

    /// 路径解析（失败 → `toFileError(error, path)`）→ abort → 执行 →（`both` 时）再 abort；
    /// 执行异常 → `toFileError(error, resolved)`。
    ///
    /// `run` 拿到的是解析后的绝对路径，错误归属也用它；只有解析本身失败时归属原始 `path`。
    /// `EnvError::File` 会被 `to_file_error` 原样返回，因此 fallback path 只影响 `Remote`/
    /// `Other` 错误——reader 内部已经带路径的 `FileError` 不会被这里覆盖。
    async fn file_op<'a, T, F>(
        &'a self,
        path: &str,
        context: &'a dyn Context,
        run: F,
        check: AbortCheck,
    ) -> Result<T, FileError>
    where
        F: FnOnce(String) -> BoxFuture<'a, Result<T, EnvError>>,
    {
        let resolved = match self.resolve(path).await {
            Ok(resolved) => resolved,
            Err(error) => return Err(to_file_error(EnvError::Remote(error), Some(path))),
        };
        let signal = context.abort_signal();
        if let Some(aborted) = abort_result::<T>(signal.as_ref(), Some(&resolved)) {
            return aborted;
        }
        let result = run(resolved.clone()).await;
        match result {
            Ok(value) => {
                if check == AbortCheck::Both
                    && let Some(aborted) = abort_result::<T>(signal.as_ref(), Some(&resolved))
                {
                    return aborted;
                }
                Ok(value)
            }
            Err(error) => Err(to_file_error(error, Some(&resolved))),
        }
    }

    /// 发送 `open` 请求（`path` + 调用方给的 `mode`/`noFollow` 等字段），返回句柄与完整回复。
    ///
    /// 这里用默认 `RequestOptions`（不固定 session），必要时 `Connection` 会先启动 daemon。
    /// 句柄编号取自回复的 `handle`，缺失或不是数字时按 0 处理；回复的 `session` 记进
    /// `Handle`，后续请求都固定在这个 session 上。
    async fn open_handle(&self, resolved: &str, json: Json) -> Result<(Handle, Json), EnvError> {
        let mut body = Json::new();
        body.insert("path".to_owned(), Value::String(resolved.to_owned()));
        for (key, value) in json {
            body.insert(key, value);
        }
        let reply = self
            .connection
            .request("open", body, crate::connection::RequestOptions::default())
            .await
            .map_err(EnvError::Remote)?;
        let id = match reply.json.get("handle") {
            Some(Value::Number(handle)) => handle.as_u64().unwrap_or(0) as u32,
            _ => 0,
        };
        Ok((
            Handle {
                connection: Arc::clone(&self.connection),
                id,
                session: reply.session,
                path: resolved.to_owned(),
            },
            reply.json,
        ))
    }

    /// Node 的 `readFile`（带或不带 UTF-8 解码），文件像 `open(path, "r")` 一样打开。
    ///
    /// # 分支
    ///
    /// `open` 的回复里带 `stat`，fstat 失败时带 `statError`（文件已打开但取不到元数据）。
    /// 只有 `stat.kind == "file"` 才相信 `stat.size`，其它 kind 一律按“大小未知”处理：
    /// - 大小未知或 0：`pread(None, …)` 顺序读到空块（0 字节的普通文件与 FIFO、设备等
    ///   非常规文件都走这里）；
    /// - 已知大小且不要文本：≤512 KiB 一次读完，否则 `read_range` 按 512 KiB 分块；
    /// - 已知大小且要文本：以 `min(size, 512 KiB)` 为块长做流水线读取，`RangeDecoder`
    ///   跨块解码（保留 BOM），读到 size、短读或 EOF 为止。
    ///
    /// 只有已知大小的普通文件（`stat.kind == "file"` 且带 `size`）会在读取前因超过
    /// `READ_FILE_MAX`（2^31-1 字节）被拒绝（`unknown` 错误）；size 为 0/未知的路径走流式读取，
    /// 没有这个上限检查。成功与否都会 `close` daemon 句柄；`open` 本身失败时没有句柄可关。
    async fn read_file(
        &self,
        path: &str,
        encoding: bool,
        context: &dyn Context,
    ) -> Result<ReadFileResult, FileError> {
        let signal = context.abort_signal();
        self.file_op(
            path,
            context,
            |resolved| {
                let signal = signal.clone();
                Box::pin(async move {
                    let (handle, json) =
                        self.open_handle(&resolved, Self::read_mode_json()).await?;
                    let aborted = || signal.as_ref().is_some_and(AbortSignal::is_aborted);
                    let abort = || {
                        EnvError::File(
                            FileError::new(FileErrorCode::Aborted, "The operation was aborted")
                                .with_path(resolved.clone()),
                        )
                    };
                    let result: Result<ReadFileResult, EnvError> = async {
                        // 文件已打开但 fstat 失败：daemon 把错误对象放在 `statError` 里。
                        if let Some(stat_error) = json.get("statError") {
                            let fields = match stat_error {
                                Value::Object(fields) => fields.clone(),
                                _ => Json::new(),
                            };
                            return Err(EnvError::Remote(RemoteError::new(fields)));
                        }
                        if aborted() {
                            return Err(abort());
                        }
                        // 只有普通文件才相信 size；`stat` 缺失时按 Null，size 变成 0。
                        let stat = json.get("stat").cloned().unwrap_or(Value::Null);
                        let size = stat
                            .get("kind")
                            .and_then(Value::as_str)
                            .filter(|kind| *kind == "file")
                            .and_then(|_| stat.get("size"))
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0);
                        let size = if size.is_finite() && size >= 0.0 {
                            size as u64
                        } else {
                            0
                        };
                        if size > READ_FILE_MAX {
                            return Err(EnvError::File(
                                FileError::new(
                                    FileErrorCode::Unknown,
                                    format!("File size ({size}) is greater than 2 GiB"),
                                )
                                .with_path(resolved.clone()),
                            ));
                        }
                        if size == 0 {
                            // Unknown size: sequential reads until the end, as for FIFOs and devices.
                            let mut decoder = RangeDecoder::new();
                            let mut chunks: Vec<Vec<u8>> = Vec::new();
                            let mut total = 0usize;
                            let mut text = String::new();
                            // 没有 offset 的 pread 由 daemon 按句柄的当前文件位置读并推进；
                            // 空块即 EOF。abort 在每次读之前检查。
                            loop {
                                if aborted() {
                                    return Err(abort());
                                }
                                let bytes = handle
                                    .pread(None, READ_FILE_UNKNOWN_CHUNK)
                                    .await
                                    .map_err(EnvError::Remote)?;
                                if bytes.is_empty() {
                                    break;
                                }
                                total += bytes.len();
                                if encoding {
                                    text.push_str(&decoder.decode(Some(&bytes)));
                                }
                                chunks.push(bytes);
                            }
                            if !encoding {
                                return Ok(ReadFileResult::Bytes(concat(&chunks, total)));
                            }
                            if total == 0 {
                                return Ok(ReadFileResult::Text(String::new()));
                            }
                            text.push_str(&decoder.decode(None));
                            return Ok(ReadFileResult::Text(text));
                        }
                        if !encoding {
                            // A single read of the whole size, or reads of 512 KiB until the size or
                            // the end of the file.
                            if size <= READ_FILE_CHUNK as u64 {
                                let bytes = handle
                                    .pread(Some(0), size as usize)
                                    .await
                                    .map_err(EnvError::Remote)?;
                                return Ok(ReadFileResult::Bytes(bytes));
                            }
                            let range =
                                read_range(&handle, 0, size, READ_FILE_CHUNK, &aborted).await;
                            return match range {
                                Ok(Some((chunks, total))) => {
                                    Ok(ReadFileResult::Bytes(concat(&chunks, total)))
                                }
                                Ok(None) => Err(abort()),
                                Err(error) => Err(EnvError::Remote(error)),
                            };
                        }
                        // Decoding: reads of min(size, 512 KiB) until the size, a short read, or the
                        // end of the file.
                        let length = std::cmp::min(size, READ_FILE_CHUNK as u64) as usize;
                        let mut decoder = RangeDecoder::new();
                        let mut text = String::new();
                        let mut total = 0u64;
                        let mut first = true;
                        let mut in_flight: std::collections::VecDeque<InFlight> =
                            std::collections::VecDeque::new();
                        let mut next = 0u64;
                        // 一次排 READ_DEPTH 个读取；满足结束条件后，队列里剩余请求（可能读到
                        // size 之后）随 future 一起被丢弃。
                        loop {
                            while in_flight.len() < READ_DEPTH {
                                // 排队时即发出（TS 的 `handle.pread(next, length)`）。
                                in_flight.push_back((length, handle.pread_now(Some(next), length)));
                                next += length as u64;
                            }
                            let Some((_, reply)) = in_flight.pop_front() else {
                                break;
                            };
                            let bytes = reply.await.map_err(EnvError::Remote)?.payload;
                            if aborted() {
                                return Err(abort());
                            }
                            total += bytes.len() as u64;
                            if bytes.is_empty() || total == size || bytes.len() != length {
                                if first {
                                    let mut decoder = RangeDecoder::new();
                                    let mut whole = decoder.decode(Some(&bytes));
                                    whole.push_str(&decoder.decode(None));
                                    return Ok(ReadFileResult::Text(whole));
                                }
                                text.push_str(&decoder.decode(Some(&bytes)));
                                text.push_str(&decoder.decode(None));
                                return Ok(ReadFileResult::Text(text));
                            }
                            text.push_str(&decoder.decode(Some(&bytes)));
                            first = false;
                        }
                        #[allow(unreachable_code)]
                        Err(abort())
                    }
                    .await;
                    // 所有分支（成功或错误）都在这里关闭 daemon 句柄。
                    handle.close().await;
                    result
                })
            },
            AbortCheck::Before,
        )
        .await
    }

    /// `open(path, "r")` 的请求体。
    fn read_mode_json() -> Json {
        let mut json = Json::new();
        json.insert("mode".to_owned(), Value::String("read".to_owned()));
        json
    }

    /// `writeFile`/`appendFile` 的实现：第一块放在 `write` 请求的 payload 里，剩余块用
    /// `writeChunk` 写到同一个 daemon 句柄。
    ///
    /// - `write` 的 JSON 带 `path`/`append`/`keep`；只有后面还有块时才需要句柄。
    /// - 后续块在**排队时**就发出（`request_now`），最多 `WRITE_DEPTH` 个在途；daemon 按到达
    ///   顺序追加，所以帧的发送顺序必须等于块的顺序。
    /// - 非 append 时每块前检查 abort；中止后先等在途块结算，有错误报第一个错误，否则报
    ///   `Aborted`。append 时内部不检查 abort（`signal` 为 `None`），只在 `file_op` 的前后
    ///   检查（`AbortCheck::Both`）——中止可能发生在写入完成之后。
    /// - 返回前总会 `close` 句柄，并排空所有在途请求；首块失败时根本不会创建句柄。
    ///
    /// 某个 `writeChunk` 失败后，daemon 会把该句柄标记为失败并拒绝后续块（`EBADF`），因此
    /// 文件不会在失败块之后出现“跳过后继续写”的空洞。
    async fn write_content(
        &self,
        path: &str,
        content: Vec<u8>,
        append: bool,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        let signal = if append { None } else { context.abort_signal() };
        self.file_op(
            path,
            context,
            |resolved| {
                let bytes = content;
                let signal = signal.clone();
                Box::pin(async move {
                    // 第一块随 `write` 请求发送；`keep` 表示还有后续块，需要在回复里拿句柄。
                    let first = &bytes[..std::cmp::min(bytes.len(), WRITE_CHUNK)];
                    let keep = bytes.len() > WRITE_CHUNK;
                    let mut json = Json::new();
                    json.insert("path".to_owned(), Value::String(resolved.clone()));
                    json.insert("append".to_owned(), Value::Bool(append));
                    json.insert("keep".to_owned(), Value::Bool(keep));
                    let reply = self
                        .connection
                        .request(
                            "write",
                            json,
                            crate::connection::RequestOptions {
                                payload: Some(first.to_vec()),
                                ..crate::connection::RequestOptions::default()
                            },
                        )
                        .await
                        .map_err(EnvError::Remote)?;
                    if !keep {
                        // 只有一块：daemon 写完就关闭文件，不需要句柄。
                        return Ok(());
                    }
                    let id = match reply.json.get("handle") {
                        Some(Value::Number(handle)) => handle.as_u64().unwrap_or(0) as u32,
                        _ => 0,
                    };
                    let handle = Handle {
                        connection: Arc::clone(&self.connection),
                        id,
                        session: reply.session,
                        path: resolved.clone(),
                    };
                    let mut in_flight: std::collections::VecDeque<crate::connection::ReplyNow> =
                        std::collections::VecDeque::new();
                    let mut outcome: Result<(), EnvError> = Ok(());
                    let aborted = || signal.as_ref().is_some_and(AbortSignal::is_aborted);
                    // 后续块：队列满 WRITE_DEPTH 时等待队首，既保持发送顺序，也限制在途内存。
                    'chunks: {
                        let mut offset = WRITE_CHUNK;
                        while offset < bytes.len() {
                            if aborted() {
                                // A chunk that failed before this check fails the write, as it
                                // would have in sequence.
                                let mut first_error: Option<RemoteError> = None;
                                while let Some(chunk) = in_flight.pop_front() {
                                    if let Err(error) = chunk.await
                                        && first_error.is_none()
                                    {
                                        first_error = Some(error);
                                    }
                                }
                                outcome = Err(match first_error {
                                    Some(error) => EnvError::Remote(error),
                                    None => EnvError::File(
                                        FileError::new(
                                            FileErrorCode::Aborted,
                                            "The operation was aborted",
                                        )
                                        .with_path(resolved.clone()),
                                    ),
                                });
                                break 'chunks;
                            }
                            let chunk =
                                &bytes[offset..std::cmp::min(offset + WRITE_CHUNK, bytes.len())];
                            let chunk = chunk.to_vec();
                            // 排队时即发出：daemon 按到达顺序追加，帧的顺序就是调用顺序。
                            in_flight.push_back(handle.request_now(
                                "writeChunk",
                                Json::new(),
                                Some(chunk),
                            ));
                            offset += WRITE_CHUNK;
                            if in_flight.len() >= WRITE_DEPTH
                                && let Some(chunk) = in_flight.pop_front()
                                && let Err(error) = chunk.await
                            {
                                outcome = Err(EnvError::Remote(error));
                                break 'chunks;
                            }
                        }
                        // The first failure; later chunks are refused by the daemon, so the file has
                        // no gaps.
                        while let Some(chunk) = in_flight.pop_front() {
                            if let Err(error) = chunk.await {
                                outcome = Err(EnvError::Remote(error));
                                break;
                            }
                        }
                    }
                    // `Promise.allSettled(inFlight)` then `handle.close()`.
                    while let Some(chunk) = in_flight.pop_front() {
                        let _ = chunk.await;
                    }
                    handle.close().await;
                    outcome
                })
            },
            if append {
                AbortCheck::Both
            } else {
                AbortCheck::Before
            },
        )
        .await
    }

    /// `readdir` + `lstat` 的整目录列表：打开目录句柄，按每页 1000 条读到 `done`，关闭句柄，
    /// 然后在**客户端**按 libuv 顺序排序（仅 POSIX），最后把条目转换成 [`FileInfo`]。
    ///
    /// 与 `RemoteDirReader::next` 的差异：这里不跳过 `ENOENT` 条目——任何条目错误都会让整次
    /// `list_dir` 失败，并归属到该条目路径；无法识别的类型仍然跳过。
    ///
    /// abort 只在拿到全部分页、逐条目转换时检查（`readdir` 请求不带 signal），因此大目录的
    /// 枚举阶段不会被中止；空目录时甚至不会观察 abort。句柄在正常返回与错误返回路径上都会
    /// 关闭（future 被丢弃时不会）。
    async fn list_dir_impl(
        &self,
        resolved: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<Vec<FileInfo>, EnvError> {
        let mut json = Json::new();
        json.insert("path".to_owned(), Value::String(resolved.to_owned()));
        let reply = self
            .connection
            .request(
                "opendir",
                json,
                crate::connection::RequestOptions::default(),
            )
            .await
            .map_err(EnvError::Remote)?;
        let id = match reply.json.get("handle") {
            Some(Value::Number(handle)) => handle.as_u64().unwrap_or(0) as u32,
            _ => 0,
        };
        let handle = Handle {
            connection: Arc::clone(&self.connection),
            id,
            session: reply.session,
            path: resolved.to_owned(),
        };
        let mut entries: Vec<RemoteDirEntry> = Vec::new();
        // TS 的 `try { … } finally { await handle.close(); }`：念完目录（或中途失败）都要关掉
        // daemon 端的 handle，否则它的表项与文件描述符会留到 session 结束。
        let paged: Result<(), EnvError> = async {
            let mut done = false;
            while !done {
                let mut json = Json::new();
                json.insert("max".to_owned(), Value::from(1000));
                let reply = handle
                    .request("readdir", json, None, None)
                    .await
                    .map_err(EnvError::Remote)?;
                let reply = Value::Object(reply.json);
                entries.extend(parse_dir_entries(reply.get("entries")));
                done = reply.get("done") == Some(&Value::Bool(true));
            }
            Ok(())
        }
        .await;
        handle.close().await;
        paged?;
        let flavor = remote_path(&self.connection)
            .await
            .map_err(EnvError::Remote)?;
        // Node's `readdir` fails on any entry it cannot lstat. libuv sorts names by their bytes on
        // POSIX; on Windows it keeps the file system's order, which the daemon reports.
        if flavor == PathFlavor::Posix {
            entries.sort_by_key(dir_entry_key);
        }
        let mut infos = Vec::with_capacity(entries.len());
        for entry in entries {
            // TS: `if (context.abortSignal?.aborted) throw new FileError("aborted", "aborted", resolved);`
            // —— 在**每个条目**前检查，因此 abort 胜过该条目（以及后续条目）的错误。
            if signal.is_some_and(AbortSignal::is_aborted) {
                return Err(EnvError::File(
                    FileError::new(FileErrorCode::Aborted, "aborted")
                        .with_path(resolved.to_owned()),
                ));
            }
            let entry_path = path_resolve(flavor, &[resolved, &entry.name]);
            if let Some(error) = &entry.error {
                let error = match error {
                    Value::Object(fields) => RemoteError::new(fields.clone()),
                    other => {
                        return Err(EnvError::Other(other.to_string()));
                    }
                };
                // TS: `throw toFileError(new RemoteError(entry.error), entryPath)`：错误属于条目，
                // 不是目录。
                return Err(EnvError::File(to_file_error(
                    EnvError::Remote(error),
                    Some(&entry_path),
                )));
            }
            if let Some(info) = &entry.info {
                // TS: `const info = toInfo(entryPath, entry.info!, paths); if (info.ok) infos.push(info.value);`
                // —— 不支持的类型（FIFO、socket、设备）只是被跳过，不让整个列表失败。
                if let Ok(info) = to_info(&entry_path, info, flavor) {
                    infos.push(info);
                }
            }
        }
        Ok(infos)
    }
}

/// 排序键：优先用 daemon 给的原始名字字节（`raw`，libuv 在 POSIX 上按字节排序），缺失时
/// 退回解码后名字的 UTF-8 字节。
fn dir_entry_key(entry: &RemoteDirEntry) -> Vec<u8> {
    match &entry.raw {
        Some(raw) => raw.clone(),
        None => entry.name.as_bytes().to_vec(),
    }
}

/// 手写 UUID v4（`uuid` 不是本 crate 允许的依赖，ARCHITECTURE.md §4.7）。
///
/// 16 字节来自操作系统 RNG；`getrandom::fill` 失败时 `expect` 直接 panic——本函数不返回
/// `Result`，调用方 `create_temp_file` 也没有处理这个失败的分支。
fn random_uuid() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the operating system provides random bytes");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

// FileSystem 中带路径的异步操作大多经 `file_op` 完成路径解析、abort 检查与 `to_file_error`
// 映射（错误路径通常是解析后的远端路径）。`id`/`cwd`/`set_cwd` 是同步方法；`list_files` 只做
// `resolve` 后交给 `list_files_over`；`cleanup` 只调用 `cleanup_running`；`absolute_path`、
// `join_path`、`rename_file`、`create_temp_dir`/`create_temp_file` 与 `watch` 也各自处理路径
// 与错误归属，都不经过 `file_op`。下面只在方法有额外语义（默认值、校验、句柄关闭、错误归属）
// 时单独说明。

#[async_trait]
impl FileSystem for RemoteExecutionEnv {
    fn id(&self) -> &str {
        &self.id
    }

    /// `set_cwd` 的覆盖值优先；否则返回构造时的 cwd。
    fn cwd(&self) -> &str {
        if let Some(cwd) = *lock(&self.cwd_override) {
            return cwd;
        }
        &self.cwd
    }

    fn set_cwd(&self, cwd: String) {
        // `cwd()` 返回 `&str`（冻结的 trait 签名），因此旧值必须保持存活；每次设置泄漏一个
        // 小字符串。`set_cwd` 在实际使用中很少调用。
        *lock(&self.cwd_override) = Some(Box::leak(cwd.into_boxed_str()));
    }

    async fn absolute_path(&self, path: &str, _context: &dyn Context) -> Result<String, FileError> {
        // 走完整的 resolve（`~`、`file://`、绝对路径规范化）；失败时错误归属原始输入路径。
        match self.resolve(path).await {
            Ok(resolved) => Ok(resolved),
            Err(error) => Err(to_file_error(EnvError::Remote(error), Some(path))),
        }
    }

    async fn join_path(
        &self,
        parts: &[String],
        _context: &dyn Context,
    ) -> Result<String, FileError> {
        // 只做平台相关的拼接，不展开 `~`/`file://`，也不把相对结果变成绝对路径。
        let result = async {
            let flavor = remote_path(&self.connection)
                .await
                .map_err(EnvError::Remote)?;
            let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
            Ok(path_join(flavor, &parts))
        }
        .await;
        // The error path does not pass a fallback path (FileError.path stays None).
        result.map_err(|error| to_file_error(error, None))
    }

    async fn read_text_file(&self, path: &str, context: &dyn Context) -> Result<String, FileError> {
        // `encoding = true` 时 `read_file` 的每个分支都返回 Text；Bytes 分支只是防御性兜底，
        // 用整块宽松解码（非法序列变 U+FFFD，保留 BOM）。
        match self.read_file(path, true, context).await? {
            ReadFileResult::Text(text) => Ok(text),
            ReadFileResult::Bytes(bytes) => Ok(crate::decode_utf8_lossy(&bytes)),
        }
    }

    async fn open_text_line_reader(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Arc<dyn TextLineReader>, FileError> {
        // 以 `mode: "read"` 打开；打开成功后如果调用方已经 abort，先关闭 daemon 句柄再把
        // aborted 返回，避免把句柄留给调用方。
        let opened = self
            .file_op(
                path,
                context,
                |resolved| {
                    Box::pin(async move {
                        let (handle, _json) =
                            self.open_handle(&resolved, Self::read_mode_json()).await?;
                        Ok(handle)
                    })
                },
                AbortCheck::Before,
            )
            .await?;
        let signal = context.abort_signal();
        if let Some(aborted) =
            abort_result::<Arc<dyn TextLineReader>>(signal.as_ref(), Some(&opened.path))
        {
            opened.close().await;
            return aborted;
        }
        Ok(Arc::new(RemoteTextLineReader {
            handle: opened,
            state: Mutex::new(LineReaderState {
                decoder: StreamDecoder::new(),
                offset: 0,
                buffered: String::new(),
                ended: false,
            }),
            closed: AtomicBool::new(false),
        }))
    }

    async fn read_text_lines(
        &self,
        path: &str,
        options: Option<&ReadTextLinesOptions>,
        context: &dyn Context,
    ) -> Result<Vec<String>, FileError> {
        // `maxLines == 0`：不打开文件直接返回空。
        if options
            .and_then(|options| options.max_lines)
            .is_some_and(|max_lines| max_lines == 0)
        {
            return Ok(Vec::new());
        }
        let reader = self.open_text_line_reader(path, context).await?;
        let result = async {
            let mut lines = Vec::new();
            loop {
                if options
                    .and_then(|options| options.max_lines)
                    .is_some_and(|max_lines| lines.len() >= max_lines)
                {
                    break;
                }
                match reader.read_line(context).await {
                    Ok(Some(line)) => lines.push(line.text),
                    Ok(None) => break,
                    Err(error) => return Err(error),
                }
            }
            Ok(lines)
        }
        .await;
        // 成功、读取错误或整体中止都要关闭 reader。
        reader.close(context).await;
        result
    }

    async fn read_binary_file(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Vec<u8>, FileError> {
        // `encoding = false` 时不会返回 Text；Text 分支只是类型兜底。
        match self.read_file(path, false, context).await? {
            ReadFileResult::Bytes(bytes) => Ok(bytes),
            ReadFileResult::Text(text) => Ok(text.into_bytes()),
        }
    }

    async fn open_binary_reader(
        &self,
        path: &str,
        options: Option<&OpenBinaryReaderOptions>,
        context: &dyn Context,
    ) -> Result<Arc<dyn BinaryReader>, FileError> {
        // `noFollow` 默认 false，字段总会发送；打开后再次检查 abort，已中止时关闭句柄。
        let mut json = Json::new();
        json.insert(
            "noFollow".to_owned(),
            Value::Bool(options.and_then(|options| options.no_follow) == Some(true)),
        );
        let opened = self
            .file_op(
                path,
                context,
                |resolved| {
                    Box::pin(async move {
                        let (handle, _json) = self.open_handle(&resolved, json).await?;
                        Ok(handle)
                    })
                },
                AbortCheck::Before,
            )
            .await?;
        let signal = context.abort_signal();
        if let Some(aborted) =
            abort_result::<Arc<dyn BinaryReader>>(signal.as_ref(), Some(&opened.path))
        {
            opened.close().await;
            return aborted;
        }
        Ok(Arc::new(RemoteBinaryReader {
            handle: opened,
            closed: AtomicBool::new(false),
        }))
    }

    async fn write_file(
        &self,
        path: &str,
        content: FileContent,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        self.write_content(path, content_bytes(content), false, context)
            .await
    }

    async fn append_file(
        &self,
        path: &str,
        content: FileContent,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        self.write_content(path, content_bytes(content), true, context)
            .await
    }

    async fn truncate_file(
        &self,
        path: &str,
        size: u64,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        // `truncate` 既可截断也可扩展文件（daemon 侧用 `set_len`）；请求不带 signal，中止在
        // 完成后才通过 `AbortCheck::Both` 观察。
        self.file_op(
            path,
            context,
            |resolved| {
                Box::pin(async move {
                    // `!Number.isSafeInteger(size) || size < 0`：`2^53` 以上的值在 Rust 可达。
                    if size > MAX_SAFE_INTEGER {
                        return Err(EnvError::File(
                            FileError::new(
                                FileErrorCode::Invalid,
                                "File size must be a non-negative safe integer",
                            )
                            .with_path(resolved.clone()),
                        ));
                    }
                    let mut json = Json::new();
                    json.insert("path".to_owned(), Value::String(resolved));
                    json.insert("size".to_owned(), Value::from(size));
                    self.connection
                        .request(
                            "truncate",
                            json,
                            crate::connection::RequestOptions::default(),
                        )
                        .await
                        .map_err(EnvError::Remote)?;
                    Ok(())
                })
            },
            AbortCheck::Both,
        )
        .await
    }

    async fn flush_file(&self, path: &str, context: &dyn Context) -> Result<(), FileError> {
        // 对远端路径做 fsync；请求不带 signal，操作完成后才检查 abort（`AbortCheck::Both`）。
        self.file_op(
            path,
            context,
            |resolved| {
                Box::pin(async move {
                    let mut json = Json::new();
                    json.insert("path".to_owned(), Value::String(resolved));
                    self.connection
                        .request("fsync", json, crate::connection::RequestOptions::default())
                        .await
                        .map_err(EnvError::Remote)?;
                    Ok(())
                })
            },
            AbortCheck::Both,
        )
        .await
    }

    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        // 两个路径分别解析；注意目的路径解析失败时错误归属**源**路径，而 abort 检查用的是
        // 目的路径。请求字段是 `path` + `to`，daemon 的错误统一归属源路径。
        let source = match self.resolve(source_path).await {
            Ok(resolved) => resolved,
            Err(error) => {
                return Err(to_file_error(EnvError::Remote(error), Some(source_path)));
            }
        };
        let destination = match self.resolve(destination_path).await {
            Ok(resolved) => resolved,
            Err(error) => {
                return Err(to_file_error(EnvError::Remote(error), Some(source_path)));
            }
        };
        let signal = context.abort_signal();
        if let Some(aborted) = abort_result::<()>(signal.as_ref(), Some(&destination)) {
            return aborted;
        }
        let mut json = Json::new();
        json.insert("path".to_owned(), Value::String(source.clone()));
        json.insert("to".to_owned(), Value::String(destination));
        match self
            .connection
            .request("rename", json, crate::connection::RequestOptions::default())
            .await
        {
            Ok(_) => Ok(()),
            // Note: the error is reported against the source path.
            Err(error) => Err(to_file_error(EnvError::Remote(error), Some(&source))),
        }
    }

    async fn file_info(&self, path: &str, context: &dyn Context) -> Result<FileInfo, FileError> {
        // lstat 不跟随最后一个符号链接（daemon 的 `lstat`），再用远端平台计算 basename；
        // abort 只在请求前检查。
        self.file_op(
            path,
            context,
            |resolved| {
                Box::pin(async move {
                    let mut json = Json::new();
                    json.insert("path".to_owned(), Value::String(resolved.clone()));
                    let reply = self
                        .connection
                        .request("lstat", json, crate::connection::RequestOptions::default())
                        .await
                        .map_err(EnvError::Remote)?;
                    let flavor = remote_path(&self.connection)
                        .await
                        .map_err(EnvError::Remote)?;
                    Ok(to_info(&resolved, &Value::Object(reply.json), flavor)?)
                })
            },
            AbortCheck::Before,
        )
        .await
    }

    async fn list_dir(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Vec<FileInfo>, FileError> {
        // 把 signal 带进 `list_dir_impl`：分页读完后在逐条目循环里用它检查中止。
        let signal = context.abort_signal();
        self.file_op(
            path,
            context,
            |resolved| {
                let signal = signal.clone();
                Box::pin(async move { self.list_dir_impl(&resolved, signal.as_ref()).await })
            },
            AbortCheck::Before,
        )
        .await
    }

    async fn list_files(
        &self,
        path: &str,
        options: Option<&ListFilesOptions>,
        context: &dyn Context,
    ) -> Result<FileListing, FileError> {
        // 递归列表由 `list_files_over` 实现（参数校验、能力探测与 abort 都在那里）；这里只
        // 负责把路径解析成远端的绝对路径。
        let resolved = match self.resolve(path).await {
            Ok(resolved) => resolved,
            Err(error) => return Err(to_file_error(EnvError::Remote(error), Some(path))),
        };
        list_files_over(&self.connection, &resolved, options, context).await
    }

    async fn open_dir_reader(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Arc<dyn DirReader>, FileError> {
        // 打开后再次检查 abort；已中止则关闭 daemon 句柄。`done`/`closed` 由 reader 自己维护。
        let opened = self
            .file_op(
                path,
                context,
                |resolved| {
                    Box::pin(async move {
                        let mut json = Json::new();
                        json.insert("path".to_owned(), Value::String(resolved.clone()));
                        let reply = self
                            .connection
                            .request(
                                "opendir",
                                json,
                                crate::connection::RequestOptions::default(),
                            )
                            .await
                            .map_err(EnvError::Remote)?;
                        let id = match reply.json.get("handle") {
                            Some(Value::Number(handle)) => handle.as_u64().unwrap_or(0) as u32,
                            _ => 0,
                        };
                        Ok(Handle {
                            connection: Arc::clone(&self.connection),
                            id,
                            session: reply.session,
                            path: resolved,
                        })
                    })
                },
                AbortCheck::Before,
            )
            .await?;
        let signal = context.abort_signal();
        if let Some(aborted) =
            abort_result::<Arc<dyn DirReader>>(signal.as_ref(), Some(&opened.path))
        {
            opened.close().await;
            return aborted;
        }
        Ok(Arc::new(RemoteDirReader {
            handle: opened,
            done: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }))
    }

    async fn canonical_path(&self, path: &str, context: &dyn Context) -> Result<String, FileError> {
        // daemon 的 realpath 结果原样返回；回复缺 `path` 时得到空字符串（不报错）。
        self.file_op(
            path,
            context,
            |resolved| {
                Box::pin(async move {
                    let mut json = Json::new();
                    json.insert("path".to_owned(), Value::String(resolved));
                    let reply = self
                        .connection
                        .request(
                            "realpath",
                            json,
                            crate::connection::RequestOptions::default(),
                        )
                        .await
                        .map_err(EnvError::Remote)?;
                    Ok(reply
                        .json
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned())
                })
            },
            AbortCheck::Before,
        )
        .await
    }

    async fn exists(&self, path: &str, context: &dyn Context) -> Result<bool, FileError> {
        // `NotFound` → false；其它错误（权限、连接失败）照常返回，不吞掉。
        match self.file_info(path, context).await {
            Ok(_) => Ok(true),
            Err(error) if error.code == FileErrorCode::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn create_dir(
        &self,
        path: &str,
        options: Option<&CreateDirOptions>,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        // `recursive` 默认 **true**（与 remove 相反），字段总会发送。
        let recursive = options
            .and_then(|options| options.recursive)
            .unwrap_or(true);
        self.file_op(
            path,
            context,
            |resolved| {
                Box::pin(async move {
                    let mut json = Json::new();
                    json.insert("path".to_owned(), Value::String(resolved));
                    json.insert("recursive".to_owned(), Value::Bool(recursive));
                    self.connection
                        .request("mkdir", json, crate::connection::RequestOptions::default())
                        .await
                        .map_err(EnvError::Remote)?;
                    Ok(())
                })
            },
            AbortCheck::Before,
        )
        .await
    }

    async fn remove(
        &self,
        path: &str,
        options: Option<&RemoveOptions>,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        // 两个选项默认都是 false，字段总会发送。
        let recursive = options
            .and_then(|options| options.recursive)
            .unwrap_or(false);
        let force = options.and_then(|options| options.force).unwrap_or(false);
        self.file_op(
            path,
            context,
            |resolved| {
                Box::pin(async move {
                    let mut json = Json::new();
                    json.insert("path".to_owned(), Value::String(resolved));
                    json.insert("recursive".to_owned(), Value::Bool(recursive));
                    json.insert("force".to_owned(), Value::Bool(force));
                    self.connection
                        .request("rm", json, crate::connection::RequestOptions::default())
                        .await
                        .map_err(EnvError::Remote)?;
                    Ok(())
                })
            },
            AbortCheck::Before,
        )
        .await
    }

    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        context: &dyn Context,
    ) -> Result<String, FileError> {
        // 先检查 abort（没有 path）。目录名的前缀是 `prefix`，默认 `"tmp-"`，拼在远端的
        // tmpdir 下；daemon 的 mkdtemp 会在前缀后追加六个随机字符并返回实际路径。
        let signal = context.abort_signal();
        // `abortResult<string>(context.abortSignal)`：没有 path。
        if let Some(aborted) = abort_result::<String>(signal.as_ref(), None) {
            return aborted;
        }
        let result = async {
            let info = self.connection.info().await.map_err(EnvError::Remote)?;
            let flavor = remote_path(&self.connection)
                .await
                .map_err(EnvError::Remote)?;
            let mut json = Json::new();
            json.insert(
                "path".to_owned(),
                Value::String(path_join(flavor, &[&info.tmpdir, prefix.unwrap_or("tmp-")])),
            );
            let reply = self
                .connection
                .request(
                    "mkdtemp",
                    json,
                    crate::connection::RequestOptions::default(),
                )
                .await
                .map_err(EnvError::Remote)?;
            Ok(reply
                .json
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned())
        }
        .await;
        result.map_err(|error| to_file_error(error, None))
    }

    async fn create_temp_file(
        &self,
        options: Option<&CreateTempFileOptions>,
        context: &dyn Context,
    ) -> Result<String, FileError> {
        // 先在远端建一个临时目录（失败直接返回），再在其中用 `prefix + UUID + suffix` 建文件。
        // `write` 显式 `parents: false`，不会自动创建父目录。
        let dir = self.create_temp_dir(Some("tmp-"), context).await?;
        let mut file_path = String::new();
        let result = async {
            let flavor = remote_path(&self.connection)
                .await
                .map_err(EnvError::Remote)?;
            let name = format!(
                "{}{}{}",
                options
                    .and_then(|options| options.prefix.clone())
                    .unwrap_or_default(),
                random_uuid(),
                options
                    .and_then(|options| options.suffix.clone())
                    .unwrap_or_default()
            );
            file_path = path_join(flavor, &[&dir, &name]);
            // Node's `writeFile(filePath, "")`, which does not create parents.
            let mut json = Json::new();
            json.insert("path".to_owned(), Value::String(file_path.clone()));
            json.insert("append".to_owned(), Value::Bool(false));
            json.insert("parents".to_owned(), Value::Bool(false));
            self.connection
                .request(
                    "write",
                    json,
                    crate::connection::RequestOptions {
                        payload: Some(Vec::new()),
                        ..crate::connection::RequestOptions::default()
                    },
                )
                .await
                .map_err(EnvError::Remote)?;
            Ok(file_path.clone())
        }
        .await;
        // 错误归属刚拼出的文件路径；若在取远端平台时就失败（`file_path` 还是空串），
        // fallback path 也会是空串。
        result.map_err(|error| to_file_error(error, Some(&file_path)))
    }

    async fn watch(
        &self,
        targets: &[WatchTarget],
        on_change: WatchCallback,
        context: &dyn Context,
    ) -> Result<Arc<dyn FileWatcher>, FileError> {
        // 每个 target 的 path 先逐个解析（`~`/file URL/相对路径），解析后再检查一次 abort；
        // watcher 建立后再检查一次，已中止则先 close。错误不带 fallback path。
        let signal = context.abort_signal();
        if let Some(aborted) = abort_result::<Arc<dyn FileWatcher>>(signal.as_ref(), None) {
            return aborted;
        }
        let result = async {
            let mut resolved = Vec::with_capacity(targets.len());
            for target in targets {
                let mut target = target.clone();
                target.path = self.resolve(&target.path).await.map_err(EnvError::Remote)?;
                resolved.push(target);
            }
            if let Some(aborted) = abort_result::<Arc<dyn FileWatcher>>(signal.as_ref(), None) {
                return aborted.map_err(EnvError::File);
            }
            let watcher = RemoteWatcher::open(
                Arc::clone(&self.connection),
                resolved,
                on_change,
                self.watch_options.clone(),
            )
            .await
            .map_err(EnvError::Remote)?;
            if let Some(aborted) = abort_result::<Arc<dyn FileWatcher>>(signal.as_ref(), None) {
                watcher.close().await;
                return aborted.map_err(EnvError::File);
            }
            Ok(Arc::new(watcher) as Arc<dyn FileWatcher>)
        }
        .await;
        // No fallback path, even though `#resolve` knows one.
        result.map_err(|error| to_file_error(error, None))
    }

    async fn cleanup(&self, _context: &dyn Context) {
        // `FileSystem::cleanup` 与 `Shell::cleanup` 做同一件事：杀死本环境启动的仍在运行
        // 的命令（不会动 daemon 上的其他命令）。
        self.cleanup_running();
    }
}

impl RemoteExecutionEnv {
    /// 对每个在跑的命令调 `connection.kill`（不发 abort，命令以被杀的退出码结束，与
    /// `NodeExecutionEnv` 一致）；清空表。
    ///
    /// 先把表整体取走再逐条 kill，且不等待命令结算：清理开始之后才注册的 `exec` 不在本次
    /// 范围内。`kill` 只对仍存活且 session 匹配的连接生效（见 `Connection::kill`）。
    fn cleanup_running(&self) {
        let running: Vec<(u32, u64)> = lock(&self.running).drain(..).collect();
        for (id, session) in running {
            self.connection.kill(id, session);
        }
    }
}

fn content_bytes(content: FileContent) -> Vec<u8> {
    match content {
        FileContent::Text(text) => text.into_bytes(),
        FileContent::Bytes(bytes) => bytes,
    }
}

#[async_trait]
impl Shell for RemoteExecutionEnv {
    async fn exec(
        &self,
        command: ShellCommand,
        options: Option<&ShellExecOptions>,
        context: &dyn Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        // 阶段 1：调用方 abort（进入时）与 timeout 校验。timeout 的单位是秒；daemon 收毫秒，
        // 所以下面用 `js_number(timeout * 1000.0)` 序列化（整数值不能写成 `300.0`）。
        let signal = context.abort_signal();
        if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
            return Err(ExecutionError::new(ExecutionErrorCode::Aborted, "aborted"));
        }
        let timeout = options.and_then(|options| options.timeout);
        if let Some(timeout) = timeout {
            if !timeout.is_finite() || timeout <= 0.0 {
                return Err(ExecutionError::new(
                    ExecutionErrorCode::Timeout,
                    "Invalid timeout: must be a finite number of seconds",
                ));
            }
            if timeout * 1000.0 > MAX_TIMEOUT_MS {
                return Err(ExecutionError::new(
                    ExecutionErrorCode::Timeout,
                    format!("Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"),
                ));
            }
        }
        // 阶段 2：工作目录。显式 cwd 要按远端规则解析；解析失败只保留 message，错误码是
        // `unknown`。省略时直接使用 `cwd()`，不再做规范化。
        let inherit_env = options
            .and_then(|options| options.inherit_env)
            .unwrap_or(true);
        let cwd = match options.and_then(|options| options.cwd.clone()) {
            Some(cwd) => match self.resolve(&cwd).await {
                Ok(resolved) => resolved,
                Err(error) => {
                    return Err(ExecutionError::new(
                        ExecutionErrorCode::Unknown,
                        error.message().to_owned(),
                    ));
                }
            },
            None => self.cwd().to_owned(),
        };
        if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
            return Err(ExecutionError::new(ExecutionErrorCode::Aborted, "aborted"));
        }
        // 阶段 3：合并环境变量。`shell_env` 只在 inherit_env 时并入；调用方的 env 最后写入，
        // 因此同名键以 `ShellExecOptions::env` 为准。
        let mut env: IndexMap<String, String> = IndexMap::new();
        if inherit_env && let Some(shell_env) = &self.shell_env {
            for (key, value) in shell_env {
                env.insert(key.clone(), value.clone());
            }
        }
        if let Some(options_env) = options.and_then(|options| options.env.clone()) {
            for (key, value) in options_env {
                env.insert(key, value);
            }
        }
        // TS: `signal?.addEventListener("abort", onAbort, { once: true })`，紧接在最后一次
        // `signal.aborted` 检查之后。JS 是单线程的，这两步之间不可能插入 abort；Rust 可以在任意
        // 时刻从别的线程 abort，而 `on_abort` 不回放订阅前的事件，所以订阅后还要再检查一次。
        // 阶段 4：内部 controller 把调用方的 abort 转成请求的 cancel。`forward_abort` 在订阅
        // 之后会补查一次已经发生的 abort（`on_abort` 不回放），返回的订阅必须活到请求结算：
        // drop 即停止转发（对应 TS 的 removeEventListener）。
        let controller = AbortSignal::new();
        let subscription = forward_abort(signal.as_ref(), &controller);

        // 阶段 5：请求体。命令二选一（shell 字符串 / argv 数组）；spill 与 window 只在调用方
        // 给出时才发送。
        let mut body = Json::new();
        match &command {
            ShellCommand::Shell(command) => {
                body.insert("command".to_owned(), Value::String(command.clone()));
            }
            ShellCommand::Argv(argv) => {
                body.insert(
                    "argv".to_owned(),
                    Value::Array(argv.iter().cloned().map(Value::String).collect()),
                );
            }
        }
        body.insert("cwd".to_owned(), Value::String(cwd));
        let mut env_json = Json::new();
        for (key, value) in &env {
            env_json.insert(key.clone(), Value::String(value.clone()));
        }
        body.insert("env".to_owned(), Value::Object(env_json));
        body.insert("inheritEnv".to_owned(), Value::Bool(inherit_env));
        if let Some(shell_path) = &self.shell_path {
            body.insert("shellPath".to_owned(), Value::String(shell_path.clone()));
        }
        if let Some(timeout) = timeout {
            body.insert("timeoutMs".to_owned(), crate::js_number(timeout * 1000.0));
        }
        if let Some(spill) = options.and_then(|options| options.spill) {
            let mut json = Json::new();
            json.insert("afterBytes".to_owned(), Value::from(spill.after_bytes));
            json.insert("afterLines".to_owned(), Value::from(spill.after_lines));
            body.insert("spill".to_owned(), Value::Object(json));
        }
        if let Some(window) = options.and_then(|options| options.window) {
            let mut json = Json::new();
            json.insert("maxBytes".to_owned(), Value::from(window.max_bytes));
            json.insert("maxLines".to_owned(), Value::from(window.max_lines));
            json.insert(
                "minIntervalMs".to_owned(),
                crate::js_number(window.min_interval_ms),
            );
            json.insert(
                "bytesPerSecond".to_owned(),
                crate::js_number(window.bytes_per_second),
            );
            body.insert("window".to_owned(), Value::Object(json));
        }

        // 阶段 6：事件与回复的竞速。输出事件先进无界 channel，`exec` 在同一个任务里处理回调；
        // `on_start` 记下请求 id 与 session 供 `cleanup` 杀死。
        let mut callback_error: Option<ExecutionError> = None;
        let (event_sender, mut event_receiver) = mpsc::unbounded_channel::<(Json, Vec<u8>)>();
        let running = Arc::new(Mutex::new(None::<u32>));
        let running_callback = Arc::clone(&running);
        let running_map = Arc::clone(&self.running);
        let request = self.connection.request(
            "exec",
            body,
            crate::connection::RequestOptions {
                signal: Some(controller.clone()),
                on_start: Some(Arc::new(move |id, session| {
                    *lock(&running_callback) = Some(id);
                    lock(&running_map).insert(id, session);
                })),
                on_event: Some(Arc::new(move |json: Json, payload: Vec<u8>| {
                    let _ = event_sender.send((json, payload));
                })),
                ..crate::connection::RequestOptions::default()
            },
        );
        tokio::pin!(request);
        // 事件处理：只有 `kind == "output"` 的分发；回调 panic 用 `catch_unwind` 接住，转成
        // `callback_error`（错误码 `CallbackError`）并中止请求。
        let handle_event =
            |json: &Json, payload: &[u8], callback_error: &mut Option<ExecutionError>| {
                // 第一个回调错误生效；之后到达的事件不再分发。
                if callback_error.is_some() {
                    return;
                }
                if json.get("kind").and_then(Value::as_str) != Some("output") {
                    return;
                }
                let text = crate::decode_utf8_lossy(payload);
                if text.is_empty() {
                    return;
                }
                let Some(on_output) = options.and_then(|options| options.on_output.clone()) else {
                    return;
                };
                let mut info = ShellOutputInfo {
                    stream: if json.get("stream").and_then(Value::as_str) == Some("stderr") {
                        ShellOutputStream::Stderr
                    } else {
                        ShellOutputStream::Stdout
                    },
                    skipped: None,
                };
                if let Some(Value::Object(skipped)) = json.get("skipped") {
                    info.skipped = Some(ShellOutputSkip {
                        bytes: skipped.get("bytes").and_then(Value::as_u64).unwrap_or(0) as usize,
                        newlines: skipped.get("newlines").and_then(Value::as_u64).unwrap_or(0)
                            as usize,
                        ends_with_newline: skipped.get("endsWithNewline")
                            == Some(&Value::Bool(true)),
                    });
                }
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    on_output(&text, context, &info)
                }));
                if let Err(payload) = outcome {
                    let message = panic_message(payload);
                    // TS: `new ExecutionError("callback_error", cause.message, cause)`：cause 是
                    // 回调抛出的错误。Rust 的回调只能 panic，payload 的文本就是 cause.message。
                    *callback_error = Some(
                        ExecutionError::new(ExecutionErrorCode::CallbackError, message.clone())
                            .with_cause(Arc::new(CallbackCause(message))),
                    );
                    controller.abort_without_reason();
                }
            };
        // `biased` 让已排队的输出事件先于回复处理；事件按 channel 的入队顺序（即 daemon
        // 帧顺序）分发。回复到达后循环退出，剩下的事件在下面排空。
        let result = loop {
            tokio::select! {
                biased;
                Some((json, payload)) = event_receiver.recv() => {
                    handle_event(&json, &payload, &mut callback_error);
                }
                reply = &mut request => break reply,
            }
        };
        // Events are dispatched in frame order before the reply; drain whatever is still queued.
        while let Ok((json, payload)) = event_receiver.try_recv() {
            handle_event(&json, &payload, &mut callback_error);
        }
        // 请求已结算：从 running 表移除（cleanup 可能已经 kill 并清空过这个表），再解除
        // abort 转发。
        let running_id = *lock(&running);
        if let Some(id) = running_id {
            lock(&self.running).shift_remove(&id);
        }
        drop(subscription);
        // 回调错误优先于请求结果：即使请求以 aborted/exit code 结算，也把回调失败交给调用方。
        if let Some(error) = callback_error {
            return Err(error);
        }
        match result {
            Ok(reply) => {
                // `exitCode` 缺失按 0 处理；`spillPath` 只在回复里是字符串时带上。
                let exit_code = match reply.json.get("exitCode") {
                    Some(Value::Number(code)) => code.as_f64().unwrap_or(0.0) as i32,
                    _ => 0,
                };
                let spill_path = match reply.json.get("spillPath") {
                    Some(Value::String(path)) => Some(path.clone()),
                    _ => None,
                };
                Ok(ShellExecResult {
                    exit_code,
                    spill_path,
                })
            }
            Err(error) => {
                // 只映射下面四个 daemon 错误码；其余（包括连接丢失）一律 `unknown`。错误对象
                // 里的 `spillPath` 仍会保留。
                let failure = match error.code.as_str() {
                    "timeout" => ExecutionError::new(
                        ExecutionErrorCode::Timeout,
                        format!("timeout:{}", timeout_text(timeout)),
                    ),
                    "aborted" => ExecutionError::new(ExecutionErrorCode::Aborted, "aborted"),
                    "shell_unavailable" => ExecutionError::new(
                        ExecutionErrorCode::ShellUnavailable,
                        error.message().to_owned(),
                    ),
                    "spawn_error" => ExecutionError::new(
                        ExecutionErrorCode::SpawnError,
                        error.message().to_owned(),
                    ),
                    _ => {
                        ExecutionError::new(ExecutionErrorCode::Unknown, error.message().to_owned())
                    }
                };
                let failure = match error.fields.get("spillPath") {
                    Some(Value::String(path)) => failure.with_spill_path(path.clone()),
                    _ => failure,
                };
                Err(failure)
            }
        }
    }

    async fn cleanup(&self, _context: &dyn Context) {
        self.cleanup_running();
    }
}

/// `onOutput` 回调 panic 时 `ExecutionError` 的 cause（TS 的 `cause.message`）。
///
/// `ExecutionError.cause` 要求 `Arc<dyn Error>`，所以用一个只携带文本的小类型承载；它的
/// `Display` 就是 panic payload 的文本，调用方从 `ExecutionError.cause` 字段取回。
#[derive(Debug)]
struct CallbackCause(String);

impl std::fmt::Display for CallbackCause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for CallbackCause {}

/// 把调用方 signal 的 abort 转发给内部 controller，并补上 `on_abort` 不回放的那段订阅窗口：
/// 订阅之后重新检查一次已经 abort 的 signal（TS 的同步代码里没有这个窗口）。
///
/// 返回的订阅必须由调用方保留到不再需要转发为止：drop 即注销该监听器；返回 `None` 表示
/// 调用方没有 signal。
fn forward_abort(
    signal: Option<&AbortSignal>,
    controller: &AbortSignal,
) -> Option<kiana_chord::context::AbortSubscription> {
    let subscription = signal.map(|signal| {
        let controller = controller.clone();
        signal.on_abort(move |_reason| controller.abort_without_reason())
    });
    if signal.is_some_and(AbortSignal::is_aborted) {
        controller.abort_without_reason();
    }
    subscription
}

/// 超时错误消息的片段：调用方给的秒数按 JS 的 `String(number)` 格式化（例如 `30` 而不是
/// `30.0`）；没有给超时则写 `undefined`。
fn timeout_text(timeout: Option<f64>) -> String {
    match timeout {
        Some(timeout) => crate::format_js_number(timeout),
        None => "undefined".to_owned(),
    }
}

/// 取出 panic payload 的文本：`panic!("{}", …)` 等格式化调用给出 `String`，
/// `panic!("literal")` 给出 `&str`；其它类型回退为占位文本。
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
        })
        .unwrap_or_else(|| "(non-string panic payload)".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_relative_recognizes_only_a_drive_without_a_separator() {
        assert_eq!(drive_relative("C:foo"), Some("C:".to_owned()));
        assert_eq!(drive_relative("C:\\foo"), None);
        assert_eq!(drive_relative("C:/foo"), None);
        assert_eq!(drive_relative("C:"), Some("C:".to_owned()));
        assert_eq!(drive_relative("foo"), None);
        assert_eq!(drive_relative("1:foo"), None);
    }

    /// 回归（review finding）：绝对路径分支的回退基准是**客户端**的 `process.cwd()`
    /// （Node 的 `path.win32.resolve` 只从 process.cwd() 取盘符），不是远端 daemon 的
    /// `info.cwd` —— 后者会把 `\foo` 变成 `C:\foo`。期望值来自 Linux 上的 Node 26，
    /// 该机器的 cwd 没有盘符。
    #[cfg(not(windows))]
    #[test]
    fn absolute_windows_paths_use_the_client_cwd() {
        let info = RemoteInfo {
            os: "windows".to_owned(),
            home: "C:\\Users\\me".to_owned(),
            cwd: "C:\\work".to_owned(),
            ..RemoteInfo::default()
        };
        assert_eq!(resolve_path(&info, "C:\\work", "\\foo"), "\\foo");
        assert_eq!(resolve_path(&info, "C:\\work", "/foo"), "\\foo");
        assert_eq!(resolve_path(&info, "C:\\work", "C:\\foo"), "C:\\foo");
        assert_eq!(
            resolve_path(&info, "C:\\work", "//server/share/x"),
            "\\\\server\\share\\x"
        );
        // 远端 cwd 若被误用（`C:\work`），`\foo` 会变成 `C:\foo`。
        assert_ne!(resolve_path(&info, "C:\\work", "\\foo"), "C:\\foo");

        let posix = RemoteInfo {
            os: "linux".to_owned(),
            home: "/home/me".to_owned(),
            cwd: "/work".to_owned(),
            ..RemoteInfo::default()
        };
        assert_eq!(resolve_path(&posix, "/work", "/foo"), "/foo");
        // 相对路径仍以实例的 cwd 为基准（TS: `paths.resolve(this.cwd, normalized)`）。
        assert_eq!(resolve_path(&posix, "/work", "foo"), "/work/foo");
    }

    /// 期望值来自 Node 的 `fileURLToPath`（node:url），逐个用真实 Node 跑过。
    #[test]
    fn file_urls_follow_node_rules() {
        // POSIX。
        let posix = [
            ("file:///tmp/x", Some("/tmp/x")),
            ("file://localhost/tmp/x", Some("/tmp/x")),
            ("file:tmp/x", Some("/tmp/x")),
            ("FILE:///tmp/x", Some("/tmp/x")),
            ("file:///tmp/x?q=1#frag", Some("/tmp/x")),
            ("file:///tmp/%E2%82%AC", Some("/tmp/€")),
            ("file:///tmp/a%5Cb", Some("/tmp/a\\b")),
            ("file:///tmp/a%20b", Some("/tmp/a b")),
            // WHATWG 解析把 file URL 里的 `\` 当作 `/`。
            ("file:///tmp\\a", Some("/tmp/a")),
            ("file:///C:/tmp/x", Some("/C:/tmp/x")),
            ("file:///C|/tmp/x", Some("/C:/tmp/x")),
            ("file://", Some("/")),
            ("file:///", Some("/")),
            ("file:///tmp/a%2Fb", None),
            ("file:///tmp/x%2fY", None),
            ("file:///tmp/a%zzb", None),
            ("file://server/share/x", None),
            ("http://x/y", None),
            ("not a url", None),
        ];
        for (text, expected) in posix {
            assert_eq!(
                file_url_to_path(text, false).as_deref(),
                expected,
                "POSIX {text}"
            );
        }
        // Windows。
        let windows = [
            ("file:///C:/work/x", Some("C:\\work\\x")),
            ("file:///c:/work/x", Some("c:\\work\\x")),
            ("file://C:/work/x", Some("C:\\work\\x")),
            ("file://localhost/C:/x", Some("C:\\x")),
            ("file:///C|/x", Some("C:\\x")),
            ("file:///c|/x", Some("c:\\x")),
            ("file:///C|", Some("C:")),
            ("file://server/share/x", Some("\\\\server\\share\\x")),
            ("file:///C:/tmp/x", Some("C:\\tmp\\x")),
            ("file:///C:\\tmp\\x", Some("C:\\tmp\\x")),
            ("file:///tmp/a%20b", None),
            ("file:///tmp/x", None),
            ("file:///tmp/a%2Fb", None),
            ("file:///tmp/a%5Cb", None),
            ("file:///C%7C/x", None),
            ("file:///C|foo/x", None),
            ("file:///tmp/a%zzb", None),
            ("file://", None),
        ];
        for (text, expected) in windows {
            assert_eq!(
                file_url_to_path(text, true).as_deref(),
                expected,
                "Windows {text}"
            );
        }
    }

    #[test]
    fn to_info_uses_the_basename_and_millisecond_mtime() {
        let remote = serde_json::json!({
            "name": "ignored",
            "kind": "file",
            "size": 12,
            "mtimeSec": 1,
            "mtimeNsec": 500_000_000,
        });
        let info = to_info("/a/b.txt", &remote, PathFlavor::Posix).expect("info");
        assert_eq!(info.name, "b.txt");
        assert_eq!(info.path, "/a/b.txt");
        assert_eq!(info.kind, FileKind::File);
        assert_eq!(info.size, 12);
        assert_eq!(info.mtime_ms, 1500.0);
    }

    #[test]
    fn to_info_rejects_unsupported_kinds() {
        let remote = serde_json::json!({ "kind": "other", "size": 1 });
        let error = to_info("/a", &remote, PathFlavor::Posix).expect_err("other");
        assert_eq!(error.code, FileErrorCode::Invalid);
        assert_eq!(error.message, "Unsupported file type");
        assert_eq!(error.path.as_deref(), Some("/a"));
    }

    #[test]
    fn timeout_validation_messages_are_verbatim() {
        assert_eq!(
            format!("Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"),
            "Invalid timeout: maximum is 2147483.647 seconds"
        );
    }

    #[test]
    fn uuids_have_the_version_4_shape() {
        let uuid = random_uuid();
        assert_eq!(uuid.len(), 36);
        let bytes = uuid.as_bytes();
        assert_eq!(bytes[8], b'-');
        assert_eq!(bytes[13], b'-');
        assert_eq!(bytes[18], b'-');
        assert_eq!(bytes[23], b'-');
        assert_eq!(uuid.chars().nth(14), Some('4'));
        assert!(matches!(uuid.chars().nth(19), Some('8' | '9' | 'a' | 'b')));
        assert_ne!(uuid, random_uuid());
    }

    /// 回归（review finding）：`AbortSignal::on_abort` 不回放订阅前的 abort（DOM 语义），
    /// 而 TS 在最后一次检查后同步注册监听器。`forward_abort` 订阅后重新检查，堵住这段窗口。
    #[test]
    fn forwarding_an_abort_replays_one_that_already_happened() {
        let signal = AbortSignal::new();
        signal.abort_without_reason();
        let controller = AbortSignal::new();
        let subscription = forward_abort(Some(&signal), &controller);
        assert!(controller.is_aborted());
        assert!(subscription.is_some());
    }

    #[test]
    fn forwarding_an_abort_subscribes_before_the_abort_happens() {
        let signal = AbortSignal::new();
        let controller = AbortSignal::new();
        let subscription = forward_abort(Some(&signal), &controller);
        assert!(!controller.is_aborted());
        signal.abort_without_reason();
        assert!(controller.is_aborted());
        drop(subscription);
        // Dropping the subscription removes the listener, like `removeEventListener`.
        let after = AbortSignal::new();
        let controller = AbortSignal::new();
        drop(forward_abort(Some(&after), &controller));
        after.abort_without_reason();
        assert!(!controller.is_aborted());
    }

    #[test]
    fn a_missing_signal_forwards_nothing() {
        let controller = AbortSignal::new();
        assert!(forward_abort(None, &controller).is_none());
        assert!(!controller.is_aborted());
    }

    #[test]
    fn line_scan_mirror_parses_the_daemon_shape() {
        let scan: RawLineScan = serde_json::from_value(serde_json::json!({
            "newlines": 3,
            "start": 0,
            "end": 10,
            "firstLineEnd": 4,
            "lastLineStart": 8,
            "selectedBytes": 10,
            "firstLineBytes": 4,
        }))
        .expect("scan");
        let scan: LineScan = scan.into();
        assert_eq!(scan.newlines, 3);
        assert_eq!(scan.first_line_end, 4);
        assert_eq!(scan.last_line_start, 8);
    }
}
