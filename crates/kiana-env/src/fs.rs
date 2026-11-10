//! `fs.rs`：与 Node 语义对齐的文件操作（标准库不同之处见 docs/semantics.md）。
//!
//! TS 源：packages/env/daemon/src/fs.rs（原 daemon 本身就是 Rust；逐字移植）。
//! File operations with Node's semantics where the Rust standard library differs (docs/semantics.md).
//!
//! # 这个文件在系统中的位置
//! `main.rs` 的 `dispatch` 把线协议里的文件操作（`lstat`、`write`、`truncate`、`fsync`、`rename`、`mkdir`、
//! `rm`、`mkdtemp`、`open`、`pread`、`fstat`、`readdir` 等）转成本模块的函数；返回的 JSON 与 payload 由
//! frame 层封帧回给客户端。本模块只封装“一次文件系统操作”的语义，不持有连接状态：句柄表、写入排队都在
//! `main.rs` 的 `Server` 中。
//!
//! # 为什么需要它（而不是直接调用 `std::fs`）
//! 客户端（Durable 的 `NodeExecutionEnv`）按 Node/libuv 的行为断言结果：错误码（`ENOENT`、`EEXIST`、
//! `ERR_FS_EISDIR` 等）、错误消息里的 syscall 名、递归 `mkdir` 的边角情况、`mkdtemp` 的后缀等。标准库在
//! 若干处与 Node 不同，因此这里逐个对齐；差异清单见 docs/semantics.md 的
//! “Rules the standard libraries do not follow”。
//!
//! # 主要流程
//! 1. `main.rs` 校验 JSON 字段（路径、偏移、长度等）后调用本模块的函数；
//! 2. 函数把 `io::Error` 经 `Failure::io` 转成 `{ code, message, syscall?, path? }`（错误文案由 errors.rs
//!    统一提供），或把元数据转成 `info` 结构；
//! 3. 成功时通常返回 `serde_json::Value`；`pread` / `read` 返回原始字节 `Vec<u8>`，需要继续读写文件的函数返回
//!    打开好的 `File`，由调用方放进句柄表。
//!
//! # 设计原因与约束
//! - 所有路径都是客户端解析好的绝对路径字符串；本模块不做 `~`、相对路径或 `file://` 展开
//!   （见 docs/semantics.md 的 “Split of work”）。
//! - 函数假定由 `main.rs` 的 worker 线程池调用。同一个 `File` 可能被多个线程使用：定位读（`pread`）不共享
//!   文件偏移；`writeChunk` 的顺序由 `main.rs` 的 `Serial` 保证，本模块不负责串行化。
//! - 错误消息里的 “lstat” 是 Node 的 syscall 名，不一定意味着这里真的调用了 `lstat(2)`；例如 `rm` 的预检查
//!   也用这个名字（见对应函数）。
//!
//! # 阅读顺序
//! `info` → `lstat` / `realpath` → `mkdir_recursive` / `mkdir` / `write` → `truncate` / `fsync` / `rename` /
//! `rm` / `mkdtemp` → `open_reader` / `pread` / `read` / `dir_entry`。

use crate::errors::Failure;
use crate::sys;
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::path::Path;

/// 本模块统一的返回类型：成功结果可以是 JSON 值、原始字节（`pread` / `read` 返回 `Vec<u8>`）或已打开的文件，
/// 失败是 Node 风格的 `Failure`。泛型 `T` 由各函数自行决定。
pub type Outcome<T> = Result<T, Failure>;

/// `{ name, kind, size, mtimeSec, mtimeNsec, dev, ino }` of lstat or fstat metadata.
///
/// 把 `metadata`（可能来自 `lstat` 或 `fstat`）转成客户端解析的 `info` 对象：
/// - `name` 取路径的最后一段；根路径（如 `/`）没有最后一段，得到空字符串而不是报错。
/// - `kind` 是四分类：`file`、`directory`、`symlink`、`other`（命名管道 FIFO、socket、设备等）。这里只看
///   `file_type()`，不访问文件系统，所以传入 `lstat` 的元数据时 `kind` 可能是 `symlink`。
/// - `mtimeSec`/`mtimeNsec` 与 `dev`/`ino` 由平台层 `sys::mtime` / `sys::identity` 提供；Windows 上
///   “inode” 实际用创建时间代替（见 sys/windows.rs），字段名仍保持 Node 的形状。
/// - `size` 是元数据里的字节数；对目录、设备等没有统一的“内容大小”含义。
pub fn info(path: &str, metadata: &Metadata) -> Value {
    let file_type = metadata.file_type();
    let kind = if file_type.is_file() {
        "file"
    } else if file_type.is_dir() {
        "directory"
    } else if file_type.is_symlink() {
        "symlink"
    } else {
        "other"
    };
    let name = Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (mtime_sec, mtime_nsec) = sys::mtime(metadata);
    let (dev, ino) = sys::identity(metadata);
    json!({
        "name": name,
        "kind": kind,
        "size": metadata.len(),
        "mtimeSec": mtime_sec,
        "mtimeNsec": mtime_nsec,
        "dev": dev,
        "ino": ino,
    })
}

/// 对路径做 `lstat`：不跟随最后一段符号链接，返回 `info(...)`。
///
/// 失败时 syscall 名为 `lstat`（Node 的 `fs.lstat` 文案），例如 `ENOENT: no such file or directory,
/// lstat '/x'`。这里只读元数据、不打开文件，因此不会像 `open` 那样被 FIFO 阻塞。
pub fn lstat(path: &str) -> Outcome<Value> {
    fs::symlink_metadata(path)
        .map(|metadata| info(path, &metadata))
        .map_err(|error| Failure::io(&error, "lstat", path))
}

/// 解析符号链接与 `.`/`..`，返回 `{ "path": <解析后的绝对路径> }`。
///
/// 实现是平台层的 `sys::realpath`（Unix 系平台上调用 `std::fs::canonicalize`；Windows 额外去掉 `\\?\`
/// 前缀）。路径不存在、链接成环等失败按 syscall `realpath` 报错。
pub fn realpath(path: &str) -> Outcome<Value> {
    sys::realpath(path)
        .map(|resolved| json!({ "path": resolved }))
        .map_err(|error| Failure::io(&error, "realpath", path))
}

/// Node's recursive `mkdir`: an existing directory is fine, an existing file is `EEXIST`, a file in the way of a
/// parent is `ENOTDIR`.
///
/// 这是 Node 递归 `mkdir` 的逐分支复刻：目标已是目录视为成功；目标已是文件得到 `EEXIST`；父路径被文件挡住
/// 得到 `ENOTDIR`。
///
/// 为什么不用 `std::fs::create_dir_all`：这里需要逐分支复刻 Node/libuv 的语义——目标已是目录视为成功、目标已是
/// 文件得到 `EEXIST`、父路径被文件挡住得到 `ENOTDIR`——因此显式区分这三种情况，而不是把整个递归交给标准库。
///
/// `ENOTDIR` 的合成是有条件的：只有递归创建父路径返回 `AlreadyExists`（即父路径本身是文件）时才走到
/// `sys::synthetic("ENOTDIR")`。某些平台的 `create_dir` 对“父级是文件”报 `NotFound`，递归到父级后才得到
/// `AlreadyExists`，这时才需要合成；在 Unix 上 `create_dir` 对这种路径通常直接返回 `ENOTDIR`，不会进入该分支。
/// 各平台的具体 errno 需以实测为准（待核实）。
///
/// 返回 `io::Result<()>`，只区分成功与失败；错误的具体 code 由上层 `mkdir` 经 `Failure::io` 转换。
pub fn mkdir_recursive(path: &Path) -> io::Result<()> {
    let is_dir = |path: &Path| {
        fs::metadata(path)
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
    };
    // 先直接尝试创建：目标不存在、父目录已存在时在这里就成功。
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        // 目标已存在：只有它确实是目录才算成功（Node 对已存在的目录不报错），否则保留 EEXIST。
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if is_dir(path) {
                Ok(())
            } else {
                Err(error)
            }
        }
        // 父目录缺失；某些平台在“父级是文件”时也报 NotFound，所以这一支并不只表示“父目录不存在”。
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            else {
                // 没有父路径（如空路径）：无法继续向上创建，原样返回 NotFound。
                return Err(error);
            };
            // 先保证父目录存在；父级本身是文件时，递归调用会得到 AlreadyExists。
            match mkdir_recursive(parent) {
                Err(parent_error) if parent_error.kind() == io::ErrorKind::AlreadyExists => {
                    // libuv 对“父级是文件”报 ENOTDIR；系统调用在这里给的是 EEXIST，因此合成错误。
                    return Err(sys::synthetic("ENOTDIR"));
                }
                other => other?,
            }
            // 父目录建好后重试。期间可能有其他进程抢先创建目标，所以重复 AlreadyExists 的判断。
            match fs::create_dir(path) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists && is_dir(path) => {
                    Ok(())
                }
                other => other,
            }
        }
        Err(error) => Err(error),
    }
}

/// `mkdir { path, recursive }`：`recursive` 为 true 时走 `mkdir_recursive`（逐级创建、已存在目录可接受），
/// 否则只创建最后一级，父目录缺失就报 `ENOENT`。
///
/// 两种模式都按 syscall `mkdir` 包装错误；成功返回 `{}`（协议里的空结果对象）。
pub fn mkdir(path: &str, recursive: bool) -> Outcome<Value> {
    let result = if recursive {
        mkdir_recursive(Path::new(path))
    } else {
        fs::create_dir(path)
    };
    result
        .map(|()| json!({}))
        .map_err(|error| Failure::io(&error, "mkdir", path))
}

/// `writeFile`/`appendFile`: create missing parents like Node's recursive `mkdir`, then write. Returns the open file
/// for further chunks.
///
/// `append` 为 true 时用 `a`（追加、不存在则创建），否则用 `w`（创建并截断）；`parents` 为 true 时先按 Node 的
/// 递归 `mkdir` 规则补出父目录，为 false 时缺失父目录会在 `open` 阶段报 `ENOENT`。
///
/// 返回值是仍打开着的 `File`：`main.rs` 在 `keep: true` 时把它放进句柄表，后续 `writeChunk` 直接往这个句柄
/// 追加，避免重复打开路径。错误按发生阶段标为 `mkdir` / `open` / `write`，客户端据此区分失败原因。
pub fn write(path: &str, append: bool, parents: bool, content: &[u8]) -> Outcome<File> {
    // 只处理非空父路径（如相对路径 "a.txt" 没有目录部分）；根路径 "/" 不是空串，会进入 mkdir_recursive 并
    // 被“已存在的目录”分支接受。
    if let Some(parent) = Path::new(path)
        .parent()
        .filter(|_| parents)
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        mkdir_recursive(parent)
            .map_err(|error| Failure::io(&error, "mkdir", &parent.to_string_lossy()))?;
    }
    let mode = if append {
        sys::OpenMode::Append
    } else {
        sys::OpenMode::Write
    };
    let mut file = sys::open(path, mode).map_err(|error| Failure::io(&error, "open", path))?;
    file.write_all(content)
        .map_err(|error| Failure::io(&error, "write", path))?;
    Ok(file)
}

/// 以 `r+` 语义打开一个已存在的文件：可读可写，但不创建、不截断。
///
/// `truncate` 与 `fsync` 都必须“文件不存在就报 `ENOENT`”，所以不能用 `w`（会创建）；Node/libuv 对这两个
/// 操作也是先按 `r+` 打开，见 `sys::OpenMode::ReadWrite`。
fn open_existing_for_write(path: &str) -> Outcome<File> {
    sys::open(path, sys::OpenMode::ReadWrite).map_err(|error| Failure::io(&error, "open", path))
}

/// `truncate`：把已存在文件截断或扩展到 `size` 字节；**不会创建**文件。
///
/// 打开失败按 syscall `open` 报错；`set_len` 失败按 libuv 的 `ftruncate` 报错（Node 的消息使用
/// `ftruncate` 而不是 `truncate`）。成功返回 `{}`。
pub fn truncate(path: &str, size: u64) -> Outcome<Value> {
    let file = open_existing_for_write(path)?;
    file.set_len(size)
        .map_err(|error| Failure::io(&error, "ftruncate", path))?;
    Ok(json!({}))
}

/// `fsync`：把已存在文件的脏页刷到存储；**不会创建**文件。
///
/// 先检查目标是不是目录：POSIX 不允许以写方式打开目录，直接打开会得到平台相关的错误；这里提前返回 Node 的
/// `EISDIR`，让 Windows 也报同一个 code。该错误带 `path` 但不带 `syscall`（下方测试锁定），因为它在发起
/// 系统调用之前就产生了。
/// 随后以 `r+` 打开并调用 `sync_all`：打开失败报 `open`，同步失败报 `fsync`。
pub fn fsync(path: &str) -> Outcome<Value> {
    // POSIX refuses to open a directory for writing; check first so Windows reports the same.
    if fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(Failure::new(
            "EISDIR",
            format!("EISDIR: illegal operation on a directory, open '{path}'"),
        )
        .path(path));
    }
    let file = open_existing_for_write(path)?;
    file.sync_all()
        .map_err(|error| Failure::io(&error, "fsync", path))?;
    Ok(json!({}))
}

/// `rename`：在文件系统内重命名或移动 `from` 到 `to`；目标已存在时覆盖（Windows 平台层用
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` 达到与 Unix 相同的覆盖语义）。
///
/// 失败时消息为 `CODE: ..., rename 'from' -> 'to'`，错误对象的 `path` 是源路径；成功返回 `{}`。
pub fn rename(from: &str, to: &str) -> Outcome<Value> {
    sys::rename(from, to)
        .map(|()| json!({}))
        .map_err(|error| Failure::io_between(&error, "rename", from, Some(to)))
}

/// Node's `fs.rm`: missing paths fail unless `force`; a directory without `recursive` fails with `ERR_FS_EISDIR`.
///
/// 具体规则：
/// - 用 `symlink_metadata`（lstat 语义）先看路径本身，因此“指向目录的符号链接”被当作非目录，删除的是链接；
/// - `force` 为 false 且路径不存在：报 `ENOENT`（syscall `lstat`）；`force` 为 true：直接成功，操作可重试；
/// - 目标是目录且 `recursive` 为 false：返回 Node 特有的 `ERR_FS_EISDIR`（不带 syscall）；
/// - 目录且 `recursive`：`remove_dir_all`；其他类型：`sys::remove_file`（Windows 上还会处理目录联接与只读属性，
///   见 sys/windows.rs）。
pub fn rm(path: &str, recursive: bool, force: bool) -> Outcome<Value> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        // `force` 让“删除不存在的路径”成为成功的空操作，客户端可以安全重试。
        Err(error) if force && error.kind() == io::ErrorKind::NotFound => return Ok(json!({})),
        Err(error) => return Err(Failure::io(&error, "lstat", path)),
    };
    let result = if metadata.is_dir() {
        if !recursive {
            return Err(Failure::new(
                "ERR_FS_EISDIR",
                format!("Path is a directory: rm returned EISDIR (is a directory) {path}"),
            )
            .path(path));
        }
        fs::remove_dir_all(path)
    } else {
        // 包括符号链接本身：lstat 的 file_type 不跟随链接，所以不会递归进链接指向的目录。
        sys::remove_file(path)
    };
    result
        .map(|()| json!({}))
        .map_err(|error| Failure::io(&error, "rm", path))
}

/// A new directory named `prefix` plus six random characters, as libuv's `mkdtemp` creates it.
///
/// 语义等同 `mkdtemp(3)`：`prefix` 是完整路径前缀（客户端用 `path.join` 拼好，允许 `../x` 这类形式），
/// 返回新建目录的路径。Unix 平台层直接调用 `libc::mkdtemp`（追加 `XXXXXX`，由系统填入六个随机字符）；
/// Windows 平台层自己生成六个 `[A-Za-z0-9]` 字符并在 `EEXIST` 时重试（见 sys/windows.rs）。
///
/// 失败消息里的路径写成模板 `<prefix>XXXXXX`，与 Node 的 `fs.mkdtemp` 报错一致（测试锁定），因为失败时
/// 并没有得到可用的随机名。
pub fn mkdtemp(prefix: &str) -> Outcome<Value> {
    sys::mkdtemp(prefix)
        .map(|path| json!({ "path": path }))
        .map_err(|error| Failure::io(&error, "mkdtemp", &format!("{prefix}XXXXXX")))
}

/// Open a regular file for positional reads; directories fail with `EISDIR`, other non-regular files with
/// `NOT_REGULAR`, and with `no_follow` a final symbolic link with `SYMLINK`.
///
/// 这是 `openBinaryReader` 的实现：先打开，**再用同一个句柄的 fstat 判定类型**（而不是对路径另行 lstat），
/// 使类型判定与后续读取针对同一个已打开对象。
///
/// - `sys::open_reader` 在 Unix 上带 `O_NONBLOCK`，FIFO 不会把 worker 线程卡在 `open` 上；`no_follow` 时带
///   `O_NOFOLLOW`，最后一段是符号链接会得到 `SYMLINK`（见 sys/unix.rs）。Windows 没有 `O_NOFOLLOW`，平台层
///   先 `symlink_metadata` 预检查再打开，因此这个拒绝不是原子的（见 sys/windows.rs）。
/// - 目录报 `EISDIR`；FIFO、设备、socket 等非常规文件报 `NOT_REGULAR`。客户端把 `NOT_REGULAR` 映射为
///   `invalid` 文件错误（见 kiana-env-client 的 `to_file_error`），不会为它切换读取方式。
/// - 返回 `(File, info)`，`info` 来自同一个句柄的元数据，供客户端直接使用。
/// - 错误 syscall：打开阶段是 `open`，取元数据阶段是 `fstat`。
pub fn open_reader(path: &str, no_follow: bool) -> Outcome<(File, Value)> {
    let file = sys::open_reader(path, no_follow)?;
    let metadata = file
        .metadata()
        .map_err(|error| Failure::io(&error, "fstat", path))?;
    // 给客户端的错误文案沿用 Node 的 readFile：目录是 "illegal operation on a directory, read"。
    if metadata.is_dir() {
        return Err(
            Failure::new("EISDIR", "EISDIR: illegal operation on a directory, read").path(path),
        );
    }
    if !metadata.is_file() {
        return Err(Failure::new("NOT_REGULAR", "Not a regular file").path(path));
    }
    let info = info(path, &metadata);
    Ok((file, info))
}

/// Up to `length` bytes at `offset`; fewer only at the end of the file.
///
/// 从 `offset` 开始读最多 `length` 字节；循环到读满或 `read_at` 返回 0（EOF）为止，因此常规文件只在末尾出现
/// 短读。`Interrupted`（EINTR）重试，其他错误按 syscall `read` 报出。
///
/// 使用平台层的定位读（Unix 的 `pread`，Windows 的 `seek_read`），**不改变文件的当前偏移**，所以同一句柄上
/// 的多个定位读不会互相挪动游标。复杂度 O(length)，只分配一个 `length` 字节的缓冲区；调用方已把 `length`
/// 限制在 `MAX_PAYLOAD` 以内（见 main.rs 的 `pread` 分支）。
pub fn pread(file: &File, path: &str, offset: u64, length: usize) -> Outcome<Vec<u8>> {
    let mut buffer = vec![0u8; length];
    let mut filled = 0;
    while filled < length {
        match sys::read_at(file, &mut buffer[filled..], offset + filled as u64) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(Failure::io(&error, "read", path)),
        }
    }
    buffer.truncate(filled);
    Ok(buffer)
}

/// Up to `length` bytes from the current position: one `read` call, as Node's `readFile` makes for files of unknown
/// size (FIFOs, devices).
///
/// 与 `pread` 的区别：这里**只调用一次** `read`，短读不代表 EOF。典型用途是以 `mode: "read"` 打开的非普通
/// 文件（FIFO、字符设备等）：这些对象没有固定大小，一次能读多少取决于当时可用的数据。调用方需要更多数据时
/// 应再次请求，而不是假设一次请求就能读满。
///
/// `Interrupted` 会重试；其他错误按 syscall `read` 报出。调用方传入的 `file` 以 `&File` 接收，但内部仍是
/// `Read for &File` 的实现，读取会推进共享的文件偏移。
pub fn read(mut file: &File, path: &str, length: usize) -> Outcome<Vec<u8>> {
    let mut buffer = vec![0u8; length];
    loop {
        match file.read(&mut buffer) {
            Ok(read) => {
                buffer.truncate(read);
                return Ok(buffer);
            }
            // 信号中断不算失败：重试一次，避免把 EINTR 泄漏给客户端。
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(Failure::io(&error, "read", path)),
        }
    }
}

/// One directory entry as Node lists it: the name decoded as UTF-8 (invalid bytes replaced), and lstat of the
/// directory joined with that decoded name, so a name that is not valid UTF-8 reports `ENOENT` as in Node. `raw`
/// carries the original bytes of such a name, which libuv sorts by.
///
/// 返回 `readdir` 的一项：`{ name, raw?, info }`，或 lstat 失败时的 `{ name, raw?, error }`。
///
/// - `name` 用 `to_string_lossy` 解码，非法 UTF-8 字节变成 U+FFFD，与 Node 把名字交给 JS 的方式一致。
/// - 随后的 lstat 使用**解码后的名字**拼路径：磁盘上不存在的解码路径会得到 `ENOENT`，这正是 Node 的行为
///   （测试 `dir_entry_carries_raw_bytes_of_a_name_that_is_not_utf8` 锁定）。
/// - 仅当原始字节与解码结果不同时附带 `raw`（Unix 由 `sys::raw_name` 提供字节；Windows 返回 `None`），
///   客户端用它按原始字节排序。
/// - 本函数不返回 `Result`：单个条目的失败是数据的一部分，不应中断整个 `readdir`。
pub fn dir_entry(dir: &str, file_name: &OsStr) -> Value {
    let name = file_name.to_string_lossy().into_owned();
    // 拼路径时用的是解码后的名字；非 UTF-8 名字因此指向一个通常不存在的路径，得到 ENOENT。
    let entry_path = Path::new(dir).join(&name).to_string_lossy().into_owned();
    let mut entry = match fs::symlink_metadata(&entry_path) {
        Ok(metadata) => json!({ "name": name, "info": info(&entry_path, &metadata) }),
        Err(error) => {
            json!({ "name": name, "error": Failure::io(&error, "lstat", &entry_path).to_json() })
        }
    };
    // 只有“解码会丢信息”的名字才带 raw：字节相同说明名字本来就是合法 UTF-8，客户端无需原始字节。
    if let Some(raw) = sys::raw_name(file_name).filter(|raw| *raw != name.as_bytes()) {
        entry["raw"] = json!(raw);
    }
    entry
}

// 单元测试：用自清理的临时目录 `Tree` 锁定 Node/libuv 的错误码、消息与边界语义；跨进程的帧协议在 tests/ 下。
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A temporary directory that cleans itself up, without a dev-dependency.
    struct Tree(std::path::PathBuf);

    impl Tree {
        fn new(tag: &str) -> Tree {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "kiana-env-fs-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Tree(path)
        }

        fn str(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }

        fn join(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().into_owned()
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn code(failure: &Value) -> &str {
        failure["code"].as_str().expect("code")
    }

    #[test]
    fn info_names_the_file_and_a_root_has_no_name() {
        let tree = Tree::new("info");
        fs::write(tree.join("a.txt"), b"hello").unwrap();
        let metadata = fs::symlink_metadata(tree.join("a.txt")).unwrap();
        let value = info(&tree.join("a.txt"), &metadata);
        assert_eq!(value["name"], json!("a.txt"));
        assert_eq!(value["kind"], json!("file"));
        assert_eq!(value["size"], json!(5));
        assert!(value["mtimeSec"].is_i64());
        assert!(value["mtimeNsec"].is_i64());
        assert!(value["dev"].is_u64());
        assert!(value["ino"].is_u64());
        // This daemon's insertion order (serde_json with `preserve_order`). The reference daemon's
        // serde_json sorts keys (`dev`, `ino`, `kind`, `mtimeNsec`, `mtimeSec`, `name`, `size`); key
        // order is not part of the wire contract because clients parse the JSON object.
        assert_eq!(
            value.as_object().unwrap().keys().collect::<Vec<_>>(),
            [
                "name",
                "kind",
                "size",
                "mtimeSec",
                "mtimeNsec",
                "dev",
                "ino"
            ]
        );
        // A path with no final component yields an empty name, not a panic.
        let root = fs::symlink_metadata("/").unwrap();
        assert_eq!(info("/", &root)["name"], json!(""));
        assert_eq!(info("/", &root)["kind"], json!("directory"));
    }

    #[test]
    fn lstat_and_realpath_name_their_syscalls() {
        let tree = Tree::new("lstat");
        fs::write(tree.join("a.txt"), b"").unwrap();
        let missing = tree.join("missing");
        let failure = lstat(&missing).unwrap_err().to_json();
        assert_eq!(code(&failure), "ENOENT");
        assert_eq!(failure["syscall"], json!("lstat"));
        assert_eq!(
            failure["message"],
            json!(format!(
                "ENOENT: no such file or directory, lstat '{missing}'"
            ))
        );
        let failure = realpath(&missing).unwrap_err().to_json();
        assert_eq!(code(&failure), "ENOENT");
        assert_eq!(failure["syscall"], json!("realpath"));
        let resolved = realpath(&tree.join("a.txt")).unwrap();
        assert_eq!(
            resolved["path"],
            json!(
                fs::canonicalize(tree.join("a.txt"))
                    .unwrap()
                    .to_string_lossy()
            )
        );
    }

    #[test]
    fn mkdir_recursive_accepts_directories_and_refuses_files() {
        let tree = Tree::new("mkdir");
        // The whole chain is created, and an existing directory is fine.
        mkdir_recursive(Path::new(&tree.join("a/b/c"))).unwrap();
        assert!(Path::new(&tree.join("a/b/c")).is_dir());
        mkdir_recursive(Path::new(&tree.join("a/b/c"))).unwrap();
        // An existing file in the way is EEXIST from `create_dir`.
        fs::write(tree.join("file"), b"").unwrap();
        let failure = mkdir(&tree.join("file"), true).unwrap_err().to_json();
        assert_eq!(code(&failure), "EEXIST");
        assert_eq!(failure["syscall"], json!("mkdir"));
        // A file in the way of a parent is synthetic ENOTDIR, as libuv reports.
        let through = tree.join("file/x");
        let failure = mkdir(&through, true).unwrap_err().to_json();
        assert_eq!(code(&failure), "ENOTDIR");
        assert_eq!(
            failure["message"],
            json!(format!("ENOTDIR: not a directory, mkdir '{through}'"))
        );
        // Without `recursive`, one missing parent is enough to fail with ENOENT.
        let failure = mkdir(&tree.join("no/parent"), false).unwrap_err().to_json();
        assert_eq!(code(&failure), "ENOENT");
    }

    #[test]
    fn mkdir_recursive_returns_the_original_error_at_the_root() {
        // The NotFound branch recurses into the parent; without one it returns the error unchanged.
        let error = mkdir_recursive(Path::new("")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn write_creates_parents_and_reports_the_failing_syscall() {
        let tree = Tree::new("write");
        let nested = tree.join("a/b/c.txt");
        write(&nested, false, true, b"one").unwrap();
        assert_eq!(fs::read(&nested).unwrap(), b"one");
        write(&nested, true, true, b"two").unwrap();
        assert_eq!(fs::read(&nested).unwrap(), b"onetwo");
        write(&nested, false, true, b"x").unwrap();
        assert_eq!(fs::read(&nested).unwrap(), b"x");
        // A missing parent with `parents: false` fails under `open`.
        let failure = write(&tree.join("no/x"), false, false, b"x")
            .err()
            .unwrap()
            .to_json();
        assert_eq!(code(&failure), "ENOENT");
        assert_eq!(failure["syscall"], json!("open"));
        // A parent that is a file fails under `mkdir` with EEXIST naming that parent (the reference daemon does
        // the same: it makes the target's parent, and an existing file there is not a directory to reuse).
        fs::write(tree.join("file"), b"").unwrap();
        let failure = write(&tree.join("file/x"), false, true, b"x")
            .err()
            .unwrap()
            .to_json();
        assert_eq!(code(&failure), "EEXIST");
        assert_eq!(failure["syscall"], json!("mkdir"));
        assert_eq!(failure["path"], json!(tree.join("file")));
        // One level deeper, `create_dir` itself reports ENOTDIR, naming the path it tried to make.
        let failure = write(&tree.join("file/x/y"), false, true, b"x")
            .err()
            .unwrap()
            .to_json();
        assert_eq!(code(&failure), "ENOTDIR");
        assert_eq!(failure["syscall"], json!("mkdir"));
        assert_eq!(failure["path"], json!(tree.join("file/x")));
        // Writing to a directory fails under `open`.
        fs::create_dir(tree.join("dir")).unwrap();
        let failure = write(&tree.join("dir"), false, false, b"x")
            .err()
            .unwrap()
            .to_json();
        assert_eq!(code(&failure), "EISDIR");
        assert_eq!(failure["syscall"], json!("open"));
    }

    #[test]
    fn truncate_and_fsync_never_create_a_file() {
        let tree = Tree::new("truncate");
        let path = tree.join("a.txt");
        write(&path, false, true, b"abcdef").unwrap();
        truncate(&path, 3).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"abc");
        let missing = tree.join("missing");
        let failure = truncate(&missing, 0).unwrap_err().to_json();
        assert_eq!(code(&failure), "ENOENT");
        assert_eq!(failure["syscall"], json!("open"));
        assert!(!Path::new(&missing).exists());
        // A directory is EISDIR with "open" in the message even before the syscall.
        let failure = fsync(&tree.str()).unwrap_err().to_json();
        assert_eq!(code(&failure), "EISDIR");
        assert_eq!(
            failure["message"],
            json!(format!(
                "EISDIR: illegal operation on a directory, open '{}'",
                tree.str()
            ))
        );
        assert_eq!(failure["path"], json!(tree.str()));
        assert!(failure.get("syscall").is_none());
        let failure = fsync(&missing).unwrap_err().to_json();
        assert_eq!(code(&failure), "ENOENT");
        assert_eq!(failure["syscall"], json!("open"));
        fsync(&path).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn truncate_errors_name_ftruncate() {
        // `/dev/full` opens for read+write but refuses `ftruncate` with EINVAL, as the reference daemon reports.
        open_existing_for_write("/dev/full").unwrap();
        let failure = truncate("/dev/full", 0).unwrap_err().to_json();
        assert_eq!(code(&failure), "EINVAL");
        assert_eq!(failure["syscall"], json!("ftruncate"));
        assert_eq!(
            failure["message"],
            json!("EINVAL: invalid argument, ftruncate '/dev/full'")
        );
        assert_eq!(failure["path"], json!("/dev/full"));
    }

    #[test]
    fn rm_follows_node() {
        let tree = Tree::new("rm");
        fs::create_dir_all(tree.join("tree/inner")).unwrap();
        fs::write(tree.join("tree/inner/a"), b"").unwrap();
        let failure = rm(&tree.join("tree"), false, false).unwrap_err().to_json();
        assert_eq!(code(&failure), "ERR_FS_EISDIR");
        assert_eq!(
            failure["message"],
            json!(format!(
                "Path is a directory: rm returned EISDIR (is a directory) {}",
                tree.join("tree")
            ))
        );
        assert_eq!(failure["path"], json!(tree.join("tree")));
        assert!(failure.get("syscall").is_none());
        rm(&tree.join("tree"), true, false).unwrap();
        assert!(!Path::new(&tree.join("tree")).exists());
        // Missing paths fail under lstat unless `force`.
        let missing = tree.join("missing");
        let failure = rm(&missing, false, false).unwrap_err().to_json();
        assert_eq!(code(&failure), "ENOENT");
        assert_eq!(failure["syscall"], json!("lstat"));
        assert_eq!(rm(&missing, false, true).unwrap(), json!({}));
        // A file is removed without `recursive`.
        fs::write(tree.join("f"), b"").unwrap();
        rm(&tree.join("f"), false, false).unwrap();
        assert!(!Path::new(&tree.join("f")).exists());
    }

    #[test]
    fn rename_names_both_paths() {
        let tree = Tree::new("rename");
        write(&tree.join("a"), false, true, b"x").unwrap();
        rename(&tree.join("a"), &tree.join("b")).unwrap();
        assert_eq!(fs::read(tree.join("b")).unwrap(), b"x");
        let from = tree.join("missing");
        let to = tree.join("b");
        let failure = rename(&from, &to).unwrap_err().to_json();
        assert_eq!(code(&failure), "ENOENT");
        assert_eq!(failure["syscall"], json!("rename"));
        assert_eq!(
            failure["message"],
            json!(format!(
                "ENOENT: no such file or directory, rename '{from}' -> '{to}'"
            ))
        );
    }

    #[test]
    fn mkdtemp_appends_the_template_to_the_reported_path() {
        let tree = Tree::new("mkdtemp");
        let created = mkdtemp(&tree.join("pre-")).unwrap();
        let path = created["path"].as_str().unwrap();
        assert!(path.starts_with(&tree.join("pre-")));
        assert_eq!(path.len(), tree.join("pre-").len() + 6);
        assert!(Path::new(path).is_dir());
        let missing = tree.join("missing/pre-");
        let failure = mkdtemp(&missing).unwrap_err().to_json();
        assert_eq!(code(&failure), "ENOENT");
        assert_eq!(failure["syscall"], json!("mkdtemp"));
        assert_eq!(failure["path"], json!(format!("{missing}XXXXXX")));
    }

    #[test]
    fn open_reader_rejects_directories_and_irregular_files() {
        let tree = Tree::new("open-reader");
        fs::write(tree.join("a.txt"), b"hello").unwrap();
        let (_, value) = open_reader(&tree.join("a.txt"), false).unwrap();
        assert_eq!(value["kind"], json!("file"));
        assert_eq!(value["size"], json!(5));
        let failure = open_reader(&tree.str(), false).unwrap_err().to_json();
        assert_eq!(code(&failure), "EISDIR");
        assert_eq!(
            failure["message"],
            json!("EISDIR: illegal operation on a directory, read")
        );
        assert_eq!(failure["path"], json!(tree.str()));
        let failure = open_reader("/dev/null", false).unwrap_err().to_json();
        assert_eq!(code(&failure), "NOT_REGULAR");
        assert_eq!(failure["message"], json!("Not a regular file"));
        assert_eq!(failure["path"], json!("/dev/null"));
        let missing = tree.join("missing");
        let failure = open_reader(&missing, false).unwrap_err().to_json();
        assert_eq!(code(&failure), "ENOENT");
        assert_eq!(failure["syscall"], json!("open"));
    }

    #[cfg(unix)]
    #[test]
    fn open_reader_refuses_a_final_symbolic_link_with_no_follow() {
        let tree = Tree::new("nofollow");
        fs::write(tree.join("a.txt"), b"hello").unwrap();
        std::os::unix::fs::symlink(tree.join("a.txt"), tree.join("link")).unwrap();
        let failure = open_reader(&tree.join("link"), true).unwrap_err().to_json();
        assert_eq!(code(&failure), "SYMLINK");
        assert_eq!(
            failure["message"],
            json!("Refusing to follow a symbolic link")
        );
        assert_eq!(failure["path"], json!(tree.join("link")));
        // The default follows it.
        let (_, value) = open_reader(&tree.join("link"), false).unwrap();
        assert_eq!(value["size"], json!(5));
    }

    #[test]
    fn pread_stops_at_the_end_and_read_makes_one_call() {
        let tree = Tree::new("pread");
        write(&tree.join("a.txt"), false, true, b"hello").unwrap();
        let (file, _) = open_reader(&tree.join("a.txt"), false).unwrap();
        assert_eq!(pread(&file, "a.txt", 1, 3).unwrap(), b"ell");
        assert_eq!(pread(&file, "a.txt", 3, 100).unwrap(), b"lo");
        assert_eq!(pread(&file, "a.txt", 5, 4).unwrap(), b"");
        // `read` starts at the current position, which is where `open` left it (0).
        assert_eq!(read(&file, "a.txt", 100).unwrap(), b"hello");
        // Reading a directory reports the syscall.
        let dir = std::fs::File::open(tree.str()).unwrap();
        let failure = pread(&dir, "dir", 0, 4).unwrap_err().to_json();
        assert_eq!(code(&failure), "EISDIR");
        assert_eq!(failure["syscall"], json!("read"));
    }

    #[test]
    fn dir_entry_reports_lstat_failures_and_raw_names() {
        let tree = Tree::new("dir-entry");
        fs::write(tree.join("a.txt"), b"hi").unwrap();
        let entry = dir_entry(&tree.str(), OsStr::new("a.txt"));
        assert_eq!(entry["name"], json!("a.txt"));
        assert_eq!(entry["info"]["size"], json!(2));
        assert!(entry.get("raw").is_none());
        // A name that is not on disk reports the lstat error under the joined path.
        let entry = dir_entry(&tree.str(), OsStr::new("gone"));
        assert_eq!(entry["error"]["code"], json!("ENOENT"));
        assert_eq!(
            entry["error"]["message"],
            json!(format!(
                "ENOENT: no such file or directory, lstat '{}'",
                tree.join("gone")
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn dir_entry_carries_raw_bytes_of_a_name_that_is_not_utf8() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let tree = Tree::new("raw-name");
        let raw = OsString::from_vec(b"bad\xff".to_vec());
        fs::write(tree.0.join(&raw), b"").unwrap();
        let entry = dir_entry(&tree.str(), &raw);
        assert_eq!(entry["name"], json!("bad\u{fffd}"));
        assert_eq!(entry["raw"], json!([98, 97, 100, 255]));
        // lstat runs under the decoded name, so the entry reports ENOENT as Node does.
        assert_eq!(entry["error"]["code"], json!("ENOENT"));
    }
}
