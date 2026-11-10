//! `sys/windows.rs`：Windows 平台层，逐条跟随 libuv（`src/win/error.c`、`src/win/process.c`、`src/win/fs.c`）
//! 与 Node 的 `child_process`。
//!
//! TS 源：packages/env/daemon/src/sys/windows.rs（原 daemon 本身就是 Rust；逐字移植）。
//! 覆盖：`GetLastError` 错误名表、`CreateFileW` 模式映射、`quote_argument`、`child_environment`、
//! `search_path`、Job Object、`taskkill /F /T`、`=C:` 盘符工作目录、`GetFileInformationByHandle`。
//! Windows, following libuv (`src/win/error.c`, `src/win/process.c`, `src/win/fs.c`) and Node's `child_process`.
//!
//! # 为什么单独成文件
//! Windows 的系统调用与 POSIX 差异很大（路径分隔符、错误码、`CreateFileW` 的打开模式、
//! 没有 `fork`/`exec`、没有进程组信号），而守护进程仍要对齐 Node/libuv 在 Windows 上的
//! 行为。本文件是 `cfg(windows)` 下唯一的平台实现，其他平台不会编译它，因此其中的单元
//! 测试也只在 Windows 目标上运行。
//!
//! # 进程生命周期
//! 1. `configure` 把每个参数经 `quote_argument` 转义后用 `raw_arg` 拼进命令行，并按 libuv 的
//!    规则生成排序、去重、补齐必需变量后的环境块；`CREATE_NO_WINDOW` 避免弹出控制台窗口。
//! 2. `Command::spawn` 之后 `after_spawn` 把子进程加入进程级 Job Object；Job 设置了
//!    `KILL_ON_JOB_CLOSE`，守护进程退出、最后一个 Job 句柄关闭时，系统会结束其中所有进程。
//! 3. `read_pipe` 用阻塞 `read` 读管道；收尾时 `interrupt_reader` 用 `CancelSynchronousIo`
//!    打断阻塞读，让读线程能看到 `stop` 并关闭管道。
//! 4. `kill_tree` 调用 `taskkill /F /T` 杀进程树，不等待其结束（与 Node 的 `killProcessTree` 一致）。
//!
//! # 注意事项
//! - 本文件不实现 PTY：`pty.rs` 的非 Unix 分支直接返回 `ENOTSUP`；
//! - Job Object 是进程级单例（`OnceLock`），所有子进程共享；加入 Job 的调用失败会被忽略，
//!   因此“退出时清理子进程”不是硬保证；
//! - `open_reader` 的符号链接检查是先查后开，不是原子的，只能拒绝检查时已是链接的路径；
//! - 路径统一以 UTF-16 传给 Win32，`wide` 拒绝内嵌 NUL 字节；
//! - 所有 `unsafe` 块都注明了句柄所有权、缓冲区长度与调用期间的生命周期前提，改动时不得
//!   删除这些前提说明。

use super::{OpenMode, ShellConfig, path_exists, synthetic};
use crate::errors::Failure;
use serde_json::{Map, Value};
use std::collections::hash_map::RandomState;
use std::ffi::OsStr;
use std::fs::{File, Metadata};
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::FileExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::UNIX_EPOCH;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CREATE_ALWAYS, CreateFileW, FILE_APPEND_DATA,
    FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_WRITE_DATA, GetFileInformationByHandle, MOVEFILE_REPLACE_EXISTING, MoveFileExW,
    OPEN_ALWAYS, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

/// libuv's `uv_translate_sys_error` for the errors file and process operations produce.
///
/// 把 Win32 错误码（通常来自 `GetLastError`，即 `io::Error::raw_os_error()`）翻译成 libuv
/// 使用的 `errno` 风格大写名字（如 `ENOENT`）；多个 Win32 码可能映射到同一个名字（如各种“找不到”都归
/// `ENOENT`），表里没有的码返回 `UNKNOWN`。客户端只看错误 `code`，所以这里的映射决定了
/// 远端 Windows 上的文件操作错误形状。
pub fn os_error_name(error: &io::Error) -> &'static str {
    let Some(code) = error.raw_os_error() else {
        return "UNKNOWN";
    };
    match code {
        1 => "EISDIR",                             // ERROR_INVALID_FUNCTION
        2 | 3 | 15 | 123 | 161 | 4392 => "ENOENT", // FILE_NOT_FOUND, PATH_NOT_FOUND, INVALID_DRIVE, INVALID_NAME, BAD_PATHNAME, INVALID_REPARSE_DATA
        4 => "EMFILE",                             // ERROR_TOO_MANY_OPEN_FILES
        5 | 1314 => "EPERM",                       // ACCESS_DENIED, PRIVILEGE_NOT_HELD
        6 => "EBADF",                              // ERROR_INVALID_HANDLE
        8 | 14 => "ENOMEM",                        // NOT_ENOUGH_MEMORY, OUTOFMEMORY
        17 => "EXDEV",                             // ERROR_NOT_SAME_DEVICE
        19 => "EROFS",                             // ERROR_WRITE_PROTECT
        32 | 33 => "EBUSY",                        // SHARING_VIOLATION, LOCK_VIOLATION
        39 | 82 | 112 => "ENOSPC",                 // HANDLE_DISK_FULL, CANNOT_MAKE, DISK_FULL
        50 => "ENOTSUP",                           // ERROR_NOT_SUPPORTED
        80 | 183 => "EEXIST",                      // FILE_EXISTS, ALREADY_EXISTS
        13 | 87 | 122 | 131 | 1464 => "EINVAL", // INVALID_DATA, INVALID_PARAMETER, INSUFFICIENT_BUFFER, NEGATIVE_SEEK, SYMLINK_NOT_SUPPORTED
        126 => "ENOENT",                        // ERROR_MOD_NOT_FOUND
        145 => "ENOTEMPTY",                     // ERROR_DIR_NOT_EMPTY
        206 => "ENAMETOOLONG",                  // ERROR_FILENAME_EXCED_RANGE
        267 => "ENOENT",                        // ERROR_DIRECTORY (libuv maps it to ENOENT)
        998 => "EFAULT",                        // ERROR_NOACCESS
        740 | 1920 => "EACCES",                 // ELEVATION_REQUIRED, CANT_ACCESS_FILE
        1921 => "ELOOP",                        // ERROR_CANT_RESOLVE_FILENAME
        _ => "UNKNOWN",
    }
}

/// Windows 路径分隔符。`watch.rs` 用它拼目录前缀、`main.rs` 在 `hello.separator` 里上报。
pub const PATH_SEPARATOR: &str = "\\";

/// 定位读：`seek_read` 从 `offset` 读且不移动文件指针。
///
/// 与 Unix 的 `pread` 对应，可能短读；`fs::pread` 负责循环补齐。
pub fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    file.seek_read(buffer, offset)
}

/// Modification time as seconds and nanoseconds since the epoch; before 1970 the seconds are negative.
///
/// `modified()` 不受支持时退化为 `(0, 0)`；1970 年前的时刻用“负秒 + 正纳秒”表示，
/// 例如 -1.5 秒记作 `(-2, 500_000_000)`，保证 `secs * 1e9 + nanos` 仍等于真实时间。
pub fn mtime(metadata: &Metadata) -> (i64, i64) {
    let Ok(time) = metadata.modified() else {
        return (0, 0);
    };
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => (after.as_secs() as i64, i64::from(after.subsec_nanos())),
        Err(before) => {
            let before = before.duration();
            let nanos = i64::from(before.subsec_nanos());
            if nanos == 0 {
                (-(before.as_secs() as i64), 0)
            } else {
                (-(before.as_secs() as i64) - 1, 1_000_000_000 - nanos)
            }
        }
    }
}

/// File index numbers need a handle per file; the creation time tells a replaced file or directory apart instead.
///
/// `fs::info` 只拿得到 metadata，因此 Windows 上的 `dev`/`ino` 用创建时间近似；NTFS 的
/// tunneling 可能让新建文件继承旧文件的创建时间，所以 `watch.rs` 调用的 `path_identity`
/// 才会打开句柄取卷序列号 + 文件索引。
pub fn identity(metadata: &Metadata) -> (u64, u64) {
    use std::os::windows::fs::MetadataExt;
    (0, metadata.creation_time())
}

/// Node's `os.tmpdir()` on Windows.
///
/// 依次取 `TEMP`、`TMP`，都没有时用 `%SystemRoot%\temp`（`windir` 兜底）。保留盘符根
/// `C:\` 的尾反斜杠，其他路径最多去掉一个尾 `\`。
pub fn tmpdir() -> String {
    let variable = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
    let mut path = variable("TEMP")
        .or_else(|| variable("TMP"))
        .unwrap_or_else(|| {
            format!(
                "{}\\temp",
                variable("SystemRoot")
                    .or_else(|| variable("windir"))
                    .unwrap_or_default()
            )
        });
    if path.len() > 1 && path.ends_with('\\') && !path.ends_with(":\\") {
        path.pop();
    }
    path
}

/// libuv's `uv_os_homedir`: `USERPROFILE` first.
///
/// 没有 `USERPROFILE` 时返回空串，`hello.home` 原样上报。
pub fn home() -> String {
    std::env::var("USERPROFILE").unwrap_or_default()
}

/// A NUL-terminated wide path; an embedded NUL fails like the standard library's path conversion (Node rejects such
/// paths before any system call, which the client reports with the same code).
///
/// 所有直接调用 Win32 的函数（`open`、`rename`、`path_identity`）都先把 `&str` 转成这里的
/// `Vec<u16>`；返回的缓冲区以 0 结尾，调用期间必须保持存活。含内嵌 NUL 的路径在发起系统
/// 调用前就失败，错误消息为 `path contains a NUL byte`。
fn wide(text: &str) -> io::Result<Vec<u16>> {
    if text.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains a NUL byte",
        ));
    }
    Ok(OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect())
}

/// libuv's `fs__mkdtemp`: `prefix` plus six characters from `[a-zA-Z0-9]`, retried while the name exists.
///
/// 六位字符由 `RandomState` 散列计数器得到，不是密码学随机；名字已存在就换一组重试，最多
/// 1000 次，用尽后返回 Win32 码 80（`ERROR_FILE_EXISTS`，`error_name` 翻译为 `EEXIST`）。
/// `prefix` 是完整路径前缀，父目录必须已存在——这与 POSIX `mkdtemp(3)` 的调用约定一致。
pub fn mkdtemp(prefix: &str) -> io::Result<String> {
    const CHARACTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let state = RandomState::new();
    for _ in 0..1000 {
        let mut hasher = state.build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        let mut random = hasher.finish();
        let mut path = prefix.to_string();
        for _ in 0..6 {
            path.push(CHARACTERS[(random % CHARACTERS.len() as u64) as usize] as char);
            random /= CHARACTERS.len() as u64;
        }
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::from_raw_os_error(80))
}

/// Windows cannot open a directory as a file and has no `O_NOFOLLOW`: check the path first, as `NodeExecutionEnv` does
/// there (not race-free).
///
/// 顺序：`no_follow` 时先用 `symlink_metadata` 判断最终组件是否为链接，是则返回 `SYMLINK`；
/// 再用跟随链接的 `metadata` 判断目录，是则返回 `EISDIR`；最后 `File::open`。检查与打开
/// 之间路径可能被替换，所以这不是原子的拒绝，只能覆盖“检查时已是链接”的情况。
pub fn open_reader(path: &str, no_follow: bool) -> Result<File, Failure> {
    if no_follow
        && std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(Failure::new("SYMLINK", "Refusing to follow a symbolic link").path(path));
    }
    if std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(
            Failure::new("EISDIR", "EISDIR: illegal operation on a directory, read").path(path),
        );
    }
    File::open(path).map_err(|error| Failure::io(&error, "open", path))
}

/// libuv's `fs__open` for Node's write modes: `CreateFileW` with its access, sharing and disposition, always with
/// `FILE_FLAG_BACKUP_SEMANTICS`, so a directory opens and later calls on it fail as in Node; creating a file where a
/// directory exists reports `EISDIR`. (Rust's `File::create` opens with `OPEN_ALWAYS` and truncates afterwards.)
///
/// 映射关系：`CREATE_ALWAYS` 对应 `w`（截断或新建），`OPEN_ALWAYS` + `FILE_APPEND_DATA` 对应
/// `a`（不存在则建、写入总在末尾），`OPEN_EXISTING` 对应 `r`/`r+`（不创建）。始终共享读写与
/// 删除权限，打开后允许其他进程继续读写或删除；`FILE_FLAG_BACKUP_SEMANTICS` 让目录也能由
/// 本函数打开；`open(path, "r")` 操作会把目录信息原样上报，读取时才失败。`openBinaryReader`
/// 走 `sys::open_reader`/`fs::open_reader`，由后者自行检查目录并报 `EISDIR`。创建时遇到
/// `ERROR_FILE_EXISTS`（80）说明路径其实是目录，按 libuv 的行为改报 `EISDIR`。
pub fn open(path: &str, mode: OpenMode) -> io::Result<File> {
    let (access, disposition) = match mode {
        OpenMode::Read => (FILE_GENERIC_READ, OPEN_EXISTING),
        OpenMode::Write => (FILE_GENERIC_WRITE, CREATE_ALWAYS),
        OpenMode::Append => (
            (FILE_GENERIC_WRITE & !FILE_WRITE_DATA) | FILE_APPEND_DATA,
            OPEN_ALWAYS,
        ),
        OpenMode::ReadWrite => (FILE_GENERIC_READ | FILE_GENERIC_WRITE, OPEN_EXISTING),
    };
    let name = wide(path)?;
    // SAFETY: a NUL-terminated wide path; the returned handle is owned by the File on success.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            disposition,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        // ERROR_FILE_EXISTS while creating means the path is a directory.
        if error.raw_os_error() == Some(80) && matches!(mode, OpenMode::Write | OpenMode::Append) {
            return Err(synthetic("EISDIR"));
        }
        return Err(error);
    }
    // SAFETY: `handle` is a valid, owned file handle.
    Ok(unsafe { File::from_raw_handle(handle as _) })
}

/// libuv's `uv_fs_realpath`: the final path without the `\\?\` prefix.
///
/// `canonicalize` 返回的是 Win32 扩展路径 `\\?\C:\...` 或 `\\?\UNC\server\share`；Node 的
/// `realpath` 不带这个前缀，所以这里去掉它，并给 UNC 情况补回开头的 `\\`。
pub fn realpath(path: &str) -> io::Result<String> {
    let resolved = std::fs::canonicalize(path)?.to_string_lossy().into_owned();
    Ok(if let Some(unc) = resolved.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{unc}")
    } else {
        resolved
            .strip_prefix("\\\\?\\")
            .map(str::to_string)
            .unwrap_or(resolved)
    })
}

/// libuv's `fs__rename`: `MoveFileExW` replacing an existing destination.
///
/// `MOVEFILE_REPLACE_EXISTING` 让目标存在时被替换；跨卷时系统拒绝（`ERROR_NOT_SAME_DEVICE`，
/// 翻译为 `EXDEV`），这里不做复制回退。返回值 0 表示失败，错误码取 `last_os_error`。
pub fn rename(from: &str, to: &str) -> io::Result<()> {
    let (from, to) = (wide(from)?, wide(to)?);
    // SAFETY: both arguments are NUL-terminated wide strings that outlive the call.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Node's `rm` on Windows: a symbolic link or junction to a directory is removed as a directory entry (libuv's
/// `fs__unlink`), and a read-only file is retried after making it writable.
///
/// Windows 的 `remove_file` 不能删除目录符号链接/junction，而 Node 的 `rm` 期望删掉链接本身，
/// 所以先判 `symlink_dir` 再走 `remove_dir`；删除遇到 `PermissionDenied` 时先去掉只读属性
/// 再重试一次，其他错误原样返回给 `fs::rm` 翻译。
pub fn remove_file(path: &str) -> io::Result<()> {
    use std::os::windows::fs::FileTypeExt;
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink_dir()) {
        return std::fs::remove_dir(path);
    }
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            let mut permissions = std::fs::metadata(path)?.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(path, permissions)?;
            std::fs::remove_file(path)
        }
        other => other,
    }
}

/// A pipe a reader thread reads with blocking calls; `interrupt_reader` cancels a blocked read.
///
/// Windows 版只要求 `io::Read`（没有 `AsRawFd`），因为读线程不做 `poll`，而是阻塞在 `read`
/// 上，靠 `CancelSynchronousIo` 打断。
pub trait Pipe: io::Read {}
impl<T: io::Read> Pipe for T {}

/// Read `pipe` until it ends, `on_data` returns false, or `stop` is set; the pipe closes when this returns.
///
/// 与 Unix 不同，这里没有 `poll`：每轮先检查 `stop`，然后阻塞读最多 64 KiB。因此 `stop` 只在
/// 两次读之间生效，命令收尾时必须由 `interrupt_reader` 打断正在阻塞的 `ReadFile`。读到 EOF
/// （`Ok(0)`）、任何读错误或 `on_data` 返回 false 都结束；`pipe` 按值传入，函数返回即关闭。
pub fn read_pipe(mut pipe: impl Pipe, stop: &AtomicBool, mut on_data: impl FnMut(&[u8]) -> bool) {
    let mut buffer = vec![0u8; 64 * 1024];
    while !stop.load(Ordering::SeqCst) {
        match pipe.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if !on_data(&buffer[..read]) {
                    return;
                }
            }
        }
    }
}

/// Cancel a reader blocked in `ReadFile`, so it sees `stop` and closes its pipe.
///
/// 对目标线程调用 `CancelSynchronousIo`：若它正阻塞在 `ReadFile`（`Read` 的实现），该调用
/// 返回错误，`read_pipe` 随即退出；没有挂起 I/O 时调用无副作用。`JoinHandle` 在 join 前
/// 一直持有线程句柄，所以对已结束但尚未 join 的线程调用也是安全的（前提见 SAFETY 注释）。
pub fn interrupt_reader(reader: &std::thread::JoinHandle<()>) {
    // SAFETY: the handle of a live or finished thread; cancelling no pending I/O is harmless.
    unsafe {
        CancelSynchronousIo(reader.as_raw_handle() as _);
    }
}

/// libuv's `search_path` for an argv command: a name with a directory is tried as given, otherwise the working
/// directory and then each `PATH` entry; a name without an extension also tries `.com` and `.exe`. Node refuses
/// batch files without a shell.
///
/// 具体规则：
/// - `program` 含 `\`、`/` 或 `:` 时视为路径，用 `cwd.join(program)` 得到候选（`join` 遇到
///   绝对路径会直接替换 `cwd`）；
/// - 否则先查 `cwd`，再按 `PATH` 的每个目录查找（`PATH` 优先取请求 `env` 里大小写不敏感的
///   同名项，否则用守护进程自己的 `PATH`）；
/// - 每个候选目录生成“原路径（仅当已有扩展名）+ 原路径`.com` + 原路径`.exe`”，因此无扩展名
///   的 `foo` 实际匹配 `foo.com`/`foo.exe`；
/// - 找到 `.bat`/`.cmd` 时拒绝：Windows 不能不经 shell 直接执行批处理，错误是
///   `spawn_error` + 文本 `spawn {program} EINVAL`（注意不是错误码 `EINVAL`）；
/// - 找不到报 `spawn_error` + `spawn {program} ENOENT`。
pub fn resolve_program(
    program: &str,
    cwd: &str,
    env: &Map<String, Value>,
) -> Result<String, Failure> {
    let not_found = || Failure::new("spawn_error", format!("spawn {program} ENOENT"));
    let has_extension = Path::new(program).extension().is_some();
    let candidates = |base: &Path| -> Vec<std::path::PathBuf> {
        let mut names = Vec::new();
        // 只有本来带扩展名才按原样尝试；无扩展名时按 libuv 的规则只补 .com/.exe。
        if has_extension {
            names.push(base.to_path_buf());
        }
        for extension in ["com", "exe"] {
            let mut name = base.as_os_str().to_owned();
            name.push(".");
            name.push(extension);
            names.push(name.into());
        }
        names
    };
    let found = if program.contains(['\\', '/', ':']) {
        candidates(&Path::new(cwd).join(program))
            .into_iter()
            .find(|path| path.is_file())
    } else {
        let path_variable = env
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
            .and_then(|(_, value)| value.as_str().map(str::to_string))
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default();
        std::iter::once(std::path::PathBuf::from(cwd))
            .chain(std::env::split_paths(&path_variable))
            .flat_map(|directory| candidates(&directory.join(program)))
            .find(|path| path.is_file())
    };
    let found = found.ok_or_else(not_found)?;
    let lower = found.to_string_lossy().to_lowercase();
    if lower.ends_with(".bat") || lower.ends_with(".cmd") {
        return Err(Failure::new(
            "spawn_error",
            format!("spawn {program} EINVAL"),
        ));
    }
    Ok(found.to_string_lossy().into_owned())
}

/// Node's `isLegacyWslBashPath`: `^[a-z]:\\windows\\(system32|sysnative)\\bash\.exe$`, case-insensitive.
///
/// 旧版 WSL 的 `bash.exe` 用 `-s` 并从 stdin 读脚本，而不是把命令作为 `-c` 的参数，
/// 因此需要在解析 shell 时单独识别。先把 `/` 统一成 `\` 再转小写，然后要求“盘符 +
/// `\windows\system32\bash.exe`”或“盘符 + `\windows\sysnative\bash.exe`”的精确形状。
fn is_legacy_wsl_bash(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_lowercase();
    let mut characters = normalized.chars();
    characters
        .next()
        .is_some_and(|drive| drive.is_ascii_lowercase())
        && matches!(
            characters.as_str(),
            ":\\windows\\system32\\bash.exe" | ":\\windows\\sysnative\\bash.exe"
        )
}

/// 根据 `program` 是否旧版 WSL `bash.exe` 生成 `ShellConfig`：旧版用 `-s` + stdin，
/// 其他 bash 用 `-c` + 命令行参数。
fn bash_config(program: &str) -> ShellConfig {
    if is_legacy_wsl_bash(program) {
        ShellConfig {
            program: program.into(),
            args: vec!["-s".into()],
            command_on_stdin: true,
        }
    } else {
        ShellConfig {
            program: program.into(),
            args: vec!["-c".into()],
            command_on_stdin: false,
        }
    }
}

/// 用 `where bash.exe` 在 `PATH` 中查找 bash：无窗口、静默 stderr，只取 stdout 第一行；
/// 命令失败或结果路径不存在都返回 `None`。
fn where_bash() -> Option<String> {
    let output = Command::new("where")
        .arg("bash.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let first = String::from_utf8_lossy(&output.stdout)
        .trim()
        .lines()
        .next()?
        .trim()
        .to_string();
    path_exists(&first).then_some(first)
}

/// Node's shell resolution on Windows: a configured shell, Git Bash, `where bash.exe`, or `shell_unavailable`.
///
/// 顺序：显式 `shellPath`（存在则按 `bash_config` 判断是否旧版 WSL）→ `%ProgramFiles%` 与
/// `%ProgramFiles(x86)%` 下的 `Git\bin\bash.exe` → `where bash.exe` 的第一个结果 → 失败。
/// 失败消息里会列出搜索过的 Git Bash 路径与安装建议（消息只作诊断，客户端只看
/// `shell_unavailable`）。argv 命令不经过这里：`exec.rs` 对 argv 直接调用 `resolve_program`。
pub fn shell_config(shell_path: Option<&str>) -> Result<ShellConfig, Failure> {
    if let Some(configured) = shell_path {
        if path_exists(configured) {
            return Ok(bash_config(configured));
        }
        return Err(Failure::new(
            "shell_unavailable",
            format!("Custom shell path not found: {configured}"),
        ));
    }
    let mut candidates = Vec::new();
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(directory) = std::env::var(variable) {
            candidates.push(format!("{directory}\\Git\\bin\\bash.exe"));
        }
    }
    if let Some(found) = candidates.iter().find(|candidate| path_exists(candidate)) {
        return Ok(bash_config(found));
    }
    if let Some(found) = where_bash() {
        return Ok(bash_config(&found));
    }
    let searched: String = candidates
        .iter()
        .map(|candidate| format!("  {candidate}\n"))
        .collect();
    Err(Failure::new(
        "shell_unavailable",
        format!(
            "No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Configure an explicit shellPath\n\nSearched Git Bash in:\n{}",
            searched.trim_end()
        ),
    ))
}

/// libuv's `quote_cmd_arg`: how one argument is written into the command line the child splits again.
///
/// 为什么需要它：Windows 进程只收到一整条命令行字符串，子进程的 C 运行时会再按 MSVC 规则
/// 拆分；`Command::raw_arg` 原样写入、不会代为加引号，所以这里逐字复刻 libuv 的转义算法。
/// 规则：空串变成 `""`；不含空格/制表符/双引号时原样；含空格但不含双引号与反斜杠时整体
/// 加引号；其余情况反向扫描，把引号前的反斜杠加倍、把引号写成 `\"`，最后整体加引号。
pub fn quote_argument(argument: &str) -> String {
    if argument.is_empty() {
        return "\"\"".into();
    }
    if !argument.contains([' ', '\t', '"']) {
        return argument.into();
    }
    if !argument.contains(['"', '\\']) {
        return format!("\"{argument}\"");
    }
    // Walk backwards: backslashes before a quote (or the closing quote) are doubled, and quotes are escaped.
    let mut reversed = Vec::new();
    let mut quote_hit = true;
    for character in argument.chars().rev() {
        reversed.push(character);
        if quote_hit && character == '\\' {
            reversed.push('\\');
        } else if character == '"' {
            quote_hit = true;
            reversed.push('\\');
        } else {
            quote_hit = false;
        }
    }
    let mut quoted = String::from("\"");
    quoted.extend(reversed.into_iter().rev());
    quoted.push('"');
    quoted
}

/// Variables libuv copies from the parent into every child environment that lacks them.
const REQUIRED_VARIABLES: [&str; 11] = [
    "HOMEDRIVE",
    "HOMEPATH",
    "LOGONSERVER",
    "PATH",
    "SYSTEMDRIVE",
    "SYSTEMROOT",
    "TEMP",
    "USERDOMAIN",
    "USERNAME",
    "USERPROFILE",
    "WINDIR",
];

/// The child environment as Node builds it on Windows: the variables in object order, sorted by UTF-16 code units,
/// keeping the first of names that differ only in case; then libuv adds required variables the result lacks.
///
/// 细节：`set` 只合并**完全相同**的键（后写覆盖先写、位置不变）；排序按 UTF-16 码元
/// （`encode_utf16` 的比较），与 JavaScript 的字符串比较一致；`retain` 按 `to_uppercase()`
/// 去重，保留排序后第一个，因此 `Path` 与 `PATH` 同时存在时只留下一个；最后把
/// `REQUIRED_VARIABLES` 中缺失且能从守护进程环境取到的项追加到末尾。`inherit_env = false`
/// 时起点为空表，但仍会补齐这些必需变量（libuv 的行为）。
fn child_environment(env: &Map<String, Value>, inherit_env: bool) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut set =
        |key: String, value: String| match entries.iter_mut().find(|(name, _)| *name == key) {
            Some(entry) => entry.1 = value,
            None => entries.push((key, value)),
        };
    if inherit_env {
        for (key, value) in std::env::vars_os() {
            set(
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            );
        }
    }
    for (key, value) in env {
        if let Some(value) = value.as_str() {
            set(key.clone(), value.to_string());
        }
    }
    entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
    let mut seen = std::collections::HashSet::new();
    entries.retain(|(name, _)| seen.insert(name.to_uppercase()));
    for required in REQUIRED_VARIABLES {
        if !entries
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(required))
            && let Ok(value) = std::env::var(required)
        {
            entries.push((required.to_string(), value));
        }
    }
    entries
}

/// Arguments quoted as libuv quotes them, Node's environment, and a hidden console.
///
/// 参数必须先用 `quote_argument` 转义，因为 `raw_arg` 会原样拼进命令行；`env_clear` 后只
/// 写入 `child_environment` 的结果，避免继承多余变量；`CREATE_NO_WINDOW` 让命令不创建
/// 控制台窗口。
pub fn configure(
    command: &mut Command,
    args: &[String],
    env: &Map<String, Value>,
    inherit_env: bool,
) {
    for argument in args {
        command.raw_arg(quote_argument(argument));
    }
    command.env_clear();
    for (key, value) in child_environment(env, inherit_env) {
        command.env(key, value);
    }
    command.creation_flags(CREATE_NO_WINDOW);
}

/// libuv puts every child it starts into one job that ends its processes when the daemon exits.
///
/// Job 只在第一次调用时创建（`OnceLock`），句柄保存为 `isize` 以便放进静态单元；多个命令
/// 线程并发首次调用时，`OnceLock` 保证只创建一个 Job，其余线程等待初始化完成。限制标志：
/// `KILL_ON_JOB_CLOSE` 保证最后一个 Job 句柄关闭（守护进程退出）时结束其中所有进程；
/// `BREAKAWAY_OK`/`SILENT_BREAKAWAY_OK` 允许子进程自行脱离 Job；
/// `DIE_ON_UNHANDLED_EXCEPTION` 让未处理异常直接终止进程。创建失败返回 `None`，
/// 之后所有子进程都不进 Job。
fn job() -> Option<isize> {
    static JOB: OnceLock<Option<isize>> = OnceLock::new();
    *JOB.get_or_init(|| {
        // SAFETY: plain Win32 calls with a zeroed, correctly sized information struct.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK
                | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK
                | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
                | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&information as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            Some(job as isize)
        }
    })
}

/// 把刚 spawn 的子进程加入进程级 Job，使守护进程退出时系统能兜底结束它。
///
/// `AssignProcessToJobObject` 的返回值被忽略：调用失败时这个子进程没有退出兜底，只能依赖
/// `kill_tree` 或进程自身结束；Job 创建失败时同样如此（`job()` 返回 `None`，这里直接跳过）。
pub fn after_spawn(child: &Child) {
    if let Some(job) = job() {
        // SAFETY: both handles are valid for the duration of the call.
        unsafe {
            AssignProcessToJobObject(job as _, child.as_raw_handle() as _);
        }
    }
}

/// Node's `killProcessTree` on Windows: `taskkill /F /T`, not waited for.
///
/// 参数：`/F` 强制结束，`/T` 连同子进程树，`/PID` 指定根进程。程序路径取 `%SystemRoot%`
/// （缺失时退回 `C:\Windows`），`CREATE_NO_WINDOW` 且三个标准流都丢弃，`spawn` 后不 `wait`：
/// 只要求把 kill 发出去，不关心 `taskkill` 自身的退出码。
pub fn kill_tree(pid: u32) {
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    let _ = Command::new(format!("{system_root}\\System32\\taskkill.exe"))
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Exit codes are unsigned 32-bit values on Windows (an access violation is 3221225477).
///
/// `ExitStatus::code()` 给出的是 i32，这里按 `u32` 重新解释，才能得到 Node 看到的无符号
/// 数值（例如访问冲突 `0xC0000005` → 3221225477）。`code()` 为 `None` 的分支只是防御性
/// 兜底，返回 1。
pub fn exit_code(status: ExitStatus) -> i64 {
    i64::from(status.code().unwrap_or(1) as u32)
}

/// Windows listings keep the file system's order; nothing sorts by raw names.
///
/// 返回 `None` 表示目录条目不携带原始字节名：`fs::dir_entry` 因此不会加 `raw` 字段，
/// 条目按文件系统返回的顺序送给客户端；Unix 版才有 `raw`。
pub fn raw_name(_name: &OsStr) -> Option<Vec<u8>> {
    None
}

/// The per-drive working directories of the `=C:` variables, which Node's `path.resolve` uses for drive-relative paths.
///
/// Windows 为每个盘符维护一个隐藏环境变量 `=C:`，记录该盘的当前目录；Node 的 `path.resolve`
/// 处理 `C:foo` 这类盘符相对路径时需要它。这里只收集名字恰好是 `=X:` 的变量，把键截成
/// 大写盘符（如 `C:`），值按字符串上报给 `hello.driveCwds`。
pub fn drive_cwds() -> Map<String, Value> {
    std::env::vars_os()
        .filter_map(|(key, value)| {
            let key = key.to_str()?;
            let bytes = key.as_bytes();
            (bytes.len() == 3
                && bytes[0] == b'='
                && bytes[1].is_ascii_alphabetic()
                && bytes[2] == b':')
                .then(|| {
                    (
                        key[1..].to_ascii_uppercase(),
                        Value::String(value.to_string_lossy().into_owned()),
                    )
                })
        })
        .collect()
}

/// libuv's `scandir` and `opendir` report a file as `ENOTDIR` (elsewhere `ERROR_DIRECTORY` is `ENOENT`).
///
/// Win32 的 `ERROR_DIRECTORY`（267）在通用错误表里映射为 `ENOENT`，但目录枚举 API 对“路径
/// 是文件”应按 libuv 的 `scandir`/`opendir` 报 `ENOTDIR`；所以这里只在这一个调用点改写，
/// 其他错误原样交给 `main.rs` 包装。
pub fn read_dir(path: &str) -> io::Result<std::fs::ReadDir> {
    std::fs::read_dir(path).map_err(|error| {
        if error.raw_os_error() == Some(267) {
            synthetic("ENOTDIR")
        } else {
            error
        }
    })
}

/// Volume serial number and file index of `path`, as libuv's `stat` reports `dev` and `ino`: the creation time is not
/// enough, because NTFS gives a file created under a recently removed name the old one's creation time ("tunneling").
/// Opens a handle, like libuv; falls back to the metadata when the path cannot be opened.
///
/// 步骤：以 0 访问权限打开句柄（只查询元数据；`follow = false` 时加 `FILE_FLAG_OPEN_REPARSE_POINT`
/// 打开链接本身而不是目标），`GetFileInformationByHandle` 取卷序列号与 64 位文件索引，随后
/// 关闭句柄；打开或查询失败则退回 `identity(metadata)` 的 `(0, creation_time)`。文件索引
/// 必须打开句柄才能拿到，这正是 `path_identity` 比 `identity(metadata)` 多出 `path`/`follow`
/// 的原因（Unix 版接受同一签名但忽略它们）。
pub fn path_identity(path: &str, metadata: &Metadata, follow: bool) -> (u64, u64) {
    let Ok(name) = wide(path) else {
        return identity(metadata);
    };
    let flags = FILE_FLAG_BACKUP_SEMANTICS
        | if follow {
            0
        } else {
            FILE_FLAG_OPEN_REPARSE_POINT
        };
    // SAFETY: a NUL-terminated wide path; the handle is closed below.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            flags,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return identity(metadata);
    }
    // SAFETY: a valid handle and a zeroed buffer for the call to fill.
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let found = unsafe { GetFileInformationByHandle(handle, &mut information) } != 0;
    // SAFETY: the handle opened above.
    unsafe { CloseHandle(handle) };
    if !found {
        return identity(metadata);
    }
    (
        u64::from(information.dwVolumeSerialNumber),
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    )
}

#[cfg(test)]
mod tests {
    //! 只覆盖 `quote_argument`：这是本文件里纯计算、无需系统调用的部分；其余函数多数依赖
    //! 真实 Win32 句柄与内核对象，因此未在这里覆盖；少数纯函数（如 `os_error_name`、
    //! `is_legacy_wsl_bash`、`bash_config`、`exit_code`）也暂未测试（本文件本身也只在
    //! Windows 目标上编译）。

    use super::quote_argument;

    #[test]
    fn quotes_arguments_like_libuv() {
        assert_eq!(quote_argument(""), "\"\"");
        assert_eq!(quote_argument("plain"), "plain");
        assert_eq!(quote_argument("a b"), "\"a b\"");
        assert_eq!(quote_argument("*.txt"), "*.txt");
        assert_eq!(quote_argument("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(
            quote_argument("C:\\dir with space\\"),
            "\"C:\\dir with space\\\\\""
        );
        assert_eq!(quote_argument("a\\\\b c"), "\"a\\\\b c\"");
    }
}
