//! `sys/unix.rs`：Linux、macOS 与 Android 的平台层。
//!
//! TS 源：packages/env/daemon/src/sys/unix.rs（原 daemon 本身就是 Rust；逐字移植）。
//! 覆盖：`pre_exec` 设置（`setsid` / 信号屏蔽）、进程组与 `kill(-pgid)`、pty、`/proc` 读取、`stat` 语义。
//! Linux, macOS and Android.
//!
//! 注：上方覆盖列表中的 `pty` 与 `/proc` 读取，在本仓库里位于 `pty.rs` 的 `unix` 子模块
//! （见其中的 `set_size`/`foreground`/`process_name`）；本文件只保留 `sys` 层被
//! `fs`/`exec`/`watch`/`main` 共用的部分。
//!
//! # 为什么单独成文件
//! Linux、macOS、Android 共享 POSIX/libc，平台差异集中在 errno 编号与少量 API（`/proc`、
//! Termux 路径）上；把这些差异隔离在这里，上层不必为它们单独写 `#[cfg]`。errno 一律用
//! `libc::` 常量而不是硬编码数字，因为同一错误在不同 Unix 上的编号并不相同。
//!
//! # 进程与并发模型
//! - `configure` 在 `pre_exec`（fork 之后、exec 之前）里调用 `setsid()`，子进程因此成为新会话
//!   与新进程组的组长，进程组 id 等于 pid；`kill_tree` 用 `kill(-pid, SIGKILL)` 一次杀掉整组。
//! - `read_pipe` 在读线程里把 fd 设为非阻塞并每 100ms `poll` 一次，所以 `stop` 标志最多延迟
//!   约一个轮询周期生效；`interrupt_reader` 在 Unix 是空实现，不需要取消阻塞读。
//! - 所有 `fcntl`/`poll`/`kill`/signal 调用都直接走 `libc`，每个 `unsafe` 块都注明了前提。
//!
//! # 调用方
//! `fs.rs`（打开、定位读、目录、时间与 inode）、`exec.rs`（shell、环境、管道、进程组与退出码）、
//! `pty.rs`（`path_exists`、`exit_code`）、`watch.rs`（`path_identity`/`mtime`/`PATH_SEPARATOR`）、
//! `main.rs`（`tmpdir`/`home`/`cwd` 等 `hello` 数据与 `opendir`）。
//!
//! # 注意事项
//! - 这里只实现守护进程用到的子集，不追求覆盖 libuv 的全部行为；
//! - `open_reader` 的 `O_NONBLOCK` 主要用于让 FIFO 打开不阻塞；普通文件的定位读本身是同步的；
//! - 路径一律以 UTF-8 `String` 传递，非 UTF-8 文件名只在 `raw_name` 里以原始字节出现。

use super::{OpenMode, ShellConfig, path_exists};
use crate::errors::Failure;
use serde_json::{Map, Value};
use std::ffi::{CString, OsStr};
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

/// libuv's name for an OS error.
///
/// 覆盖 libuv 表里的 25 个错误名（spec §2.2.9）；没有 `raw_os_error()`（例如 `io::Error::other`）
/// 或表里没有的 errno 都返回 `UNKNOWN`。用 `libc::` 常量匹配而不是硬编码数字，跨 Unix 平台
/// 才不会因 errno 编号不同而错位。
pub fn os_error_name(error: &io::Error) -> &'static str {
    let Some(errno) = error.raw_os_error() else {
        return "UNKNOWN";
    };
    match errno {
        libc::EPERM => "EPERM",
        libc::ENOENT => "ENOENT",
        libc::EIO => "EIO",
        libc::EBADF => "EBADF",
        libc::EAGAIN => "EAGAIN",
        libc::ENOMEM => "ENOMEM",
        libc::EACCES => "EACCES",
        libc::EBUSY => "EBUSY",
        libc::EEXIST => "EEXIST",
        libc::EXDEV => "EXDEV",
        libc::ENOTDIR => "ENOTDIR",
        libc::EISDIR => "EISDIR",
        libc::EINVAL => "EINVAL",
        libc::ENFILE => "ENFILE",
        libc::EMFILE => "EMFILE",
        libc::ETXTBSY => "ETXTBSY",
        libc::EFBIG => "EFBIG",
        libc::ENOSPC => "ENOSPC",
        libc::EROFS => "EROFS",
        libc::EMLINK => "EMLINK",
        libc::ENAMETOOLONG => "ENAMETOOLONG",
        libc::ENOTEMPTY => "ENOTEMPTY",
        libc::ELOOP => "ELOOP",
        libc::ENOSYS => "ENOSYS",
        libc::ENOTSUP => "ENOTSUP",
        _ => "UNKNOWN",
    }
}

/// POSIX 路径分隔符。`watch.rs` 用它拼目录前缀、`main.rs` 在 `hello.separator` 里上报。
pub const PATH_SEPARATOR: &str = "/";

/// 定位读（pread）：从 `offset` 开始读，不改变文件游标。
///
/// 可能短读，`fs::pread` 会在循环里补齐；`main.rs` 的 `scanLines` 则自己推进偏移。
pub fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    file.read_at(buffer, offset)
}

/// Modification time as seconds and nanoseconds since the epoch.
///
/// 与 `identity` 一起构成 watch 快照里用于比较的字段：目录不比较 mtime，文件比较
/// `(秒, 纳秒)` 以发现同一秒内的修改。
pub fn mtime(metadata: &Metadata) -> (i64, i64) {
    (metadata.mtime(), metadata.mtime_nsec())
}

/// Device and inode, which identify a file across renames.
///
/// rename 前后 `dev`/`ino` 不变，可用于确认同一对象；watch 只用它判断同一路径是否被替换、
/// 祖先是否变化。watch 的快照以路径为键、事件仍是路径集合，不区分移动与删除 + 新建。
pub fn identity(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

/// Node's `os.tmpdir()`; Termux's Node falls back to `$PREFIX/tmp`.
///
/// 依次取第一个**非空**的 `TMPDIR`、`TMP`、`TEMP`；都没有时 Android 用 `$PREFIX/tmp`
/// （默认 `/data/data/com.termux/files/usr`），其他系统用 `/tmp`。只去掉一个尾 `/`，
/// 所以根目录 `"/"` 保持不变。`main.rs::serve` 启动时调用一次，结果用于 `hello.tmpdir`
/// 与 spill 文件所在的临时目录。
pub fn tmpdir() -> String {
    let configured = ["TMPDIR", "TMP", "TEMP"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()));
    let mut path = configured.unwrap_or_else(|| {
        if cfg!(target_os = "android") {
            format!(
                "{}/tmp",
                std::env::var("PREFIX")
                    .unwrap_or_else(|_| "/data/data/com.termux/files/usr".into())
            )
        } else {
            "/tmp".into()
        }
    });
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    path
}

/// 远端用户主目录：只读 `HOME`，没有时返回空串（`hello.home` 原样上报）。
pub fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// `mkdtemp(3)` of `prefix`, as libuv calls it: six random characters from `[A-Za-z0-9]`.
///
/// 模板是 `prefix + "XXXXXX"`，交给 libc 原地替换并创建目录；`CString` 构造失败（前缀含
/// 内嵌 NUL）时返回 `EINVAL`，与 libuv 一致。成功返回实际创建的路径，调用方无需再检查
/// 目录是否存在——`fs::mkdtemp` 与 `exec.rs` 的 spill 目录都依赖这个“创建即成功”的语义。
pub fn mkdtemp(prefix: &str) -> io::Result<String> {
    let mut template = prefix.as_bytes().to_vec();
    template.extend_from_slice(b"XXXXXX");
    let template =
        CString::new(template).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let mut buffer = template.into_bytes_with_nul();
    // SAFETY: `buffer` is a NUL-terminated, writable template that mkdtemp fills in place.
    let created = unsafe { libc::mkdtemp(buffer.as_mut_ptr().cast()) };
    if created.is_null() {
        return Err(io::Error::last_os_error());
    }
    buffer.pop();
    Ok(OsStr::from_bytes(&buffer).to_string_lossy().into_owned())
}

/// Open a regular file for positional reads, without blocking on FIFOs and, with `no_follow`, without following a
/// final symbolic link.
///
/// `O_NONBLOCK` 防止打开没有写端的 FIFO 时一直阻塞；`O_NOFOLLOW` 只作用于最后一个路径组件，
/// 不同系统用 `ELOOP` 或 `EMLINK` 报告“遇到链接”，两者都翻译成 `SYMLINK`，让客户端能区分
/// “拒绝跟随链接”和普通打开失败。文件类型（目录/非普通文件）由 `fs::open_reader` 在打开后复核。
pub fn open_reader(path: &str, no_follow: bool) -> Result<File, Failure> {
    let mut flags = libc::O_NONBLOCK;
    if no_follow {
        flags |= libc::O_NOFOLLOW;
    }
    OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .map_err(|error| {
            if no_follow && matches!(error.raw_os_error(), Some(libc::ELOOP) | Some(libc::EMLINK)) {
                Failure::new("SYMLINK", "Refusing to follow a symbolic link").path(path)
            } else {
                Failure::io(&error, "open", path)
            }
        })
}

/// Open a file in one of Node's write modes.
///
/// 与 Node 的 `fs.open` 对齐：`Read`/`ReadWrite`（`r`/`r+`）**不创建**文件，缺失就是 `ENOENT`；
/// `Write`（`w`）创建并截断；`Append`（`a`）创建且写入只追加。父目录不会自动创建——需要时
/// 由 `fs::write` 先按 Node 的递归 `mkdir` 建好。
pub fn open(path: &str, mode: OpenMode) -> io::Result<File> {
    let mut options = OpenOptions::new();
    match mode {
        OpenMode::Read => options.read(true),
        OpenMode::Write => options.write(true).create(true).truncate(true),
        OpenMode::Append => options.append(true).create(true),
        OpenMode::ReadWrite => options.read(true).write(true),
    };
    options.open(path)
}

/// `std::fs::canonicalize`：解析符号链接与 `..`，返回绝对路径。
///
/// 结果要与 Node 的 `realpath` 对齐；Unix 的 canonicalize 已经是最终形式，Windows 版还要
/// 额外去掉 `\\?\` 扩展前缀。
pub fn realpath(path: &str) -> io::Result<String> {
    std::fs::canonicalize(path).map(|resolved| resolved.to_string_lossy().into_owned())
}

/// 转发 `std::fs::rename`：同一文件系统内是原子替换，目标已存在会被替换。
///
/// 跨设备时内核返回 `EXDEV`，由 `fs.rs` 经 `error_name` 上报；这里不做复制回退。
pub fn rename(from: &str, to: &str) -> io::Result<()> {
    std::fs::rename(from, to)
}

/// 转发 `std::fs::remove_file`：只删除非目录项（目录会失败）。
///
/// 错误码由平台 errno 经 `error_name` 翻译；`fs.rs` 的 `rm` 先用 `symlink_metadata` 区分
/// 目录与文件，只有非目录才走到这里。
pub fn remove_file(path: &str) -> io::Result<()> {
    std::fs::remove_file(path)
}

/// A pipe a reader thread can poll, so that it stops when the command settles.
///
/// `exec::spawn_reader` 用 `impl sys::Pipe + Send + 'static` 接收 stdout/stderr；Unix 额外要求
/// `AsRawFd`，因为 `read_pipe` 要 `poll` 这个 fd，而 Windows 版是阻塞读、只需要 `io::Read`。
pub trait Pipe: io::Read + std::os::fd::AsRawFd {}
impl<T: io::Read + std::os::fd::AsRawFd> Pipe for T {}

/// Read `pipe` until it ends, `on_data` returns false, or `stop` is set; the pipe closes when this returns.
///
/// 分三步：
/// 1. 用 `fcntl` 在原有标志上追加 `O_NONBLOCK`，避免后续 `read` 阻塞住轮询；
/// 2. 循环 `poll(POLLIN, 100ms)`：`stop` 为 true 时退出，超时或 `EINTR` 重新检查，
///    其他 `poll` 错误直接返回；
/// 3. 一次最多读 64 KiB 交给 `on_data`：返回 false、读到 EOF（`Ok(0)`）或其他读错误都结束，
///    `WouldBlock`/`Interrupted` 则继续。
///
/// `stop` 以 `SeqCst` 读取，命令收尾时 `exec.rs` 置位，读线程最多一个轮询周期后退出；
/// `on_data` 返回 false 表示下游通道已关闭（例如客户端断开），停止读取以免无人消费的输出堆积。
/// `pipe` 按值传入，函数返回时被 drop，fd 随之关闭。
pub fn read_pipe(mut pipe: impl Pipe, stop: &AtomicBool, mut on_data: impl FnMut(&[u8]) -> bool) {
    let fd = pipe.as_raw_fd();
    // SAFETY: fcntl on a descriptor this thread owns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let mut buffer = vec![0u8; 64 * 1024];
    while !stop.load(Ordering::SeqCst) {
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, 100) };
        if ready == 0
            || (ready < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted)
        {
            continue;
        }
        if ready < 0 {
            return;
        }
        match pipe.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => {
                if !on_data(&buffer[..read]) {
                    return;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return,
        }
    }
}

/// Readers poll `stop` every 100 ms.
///
/// 因此 Unix 不需要从外部打断阻塞读：这里保留空实现只是为了让 `exec.rs` 调用
/// `sys::interrupt_reader` 时不必按平台分支。Windows 版才有实际动作（取消阻塞中的 `ReadFile`）。
pub fn interrupt_reader(_reader: &std::thread::JoinHandle<()>) {}

/// The program an argv command runs; `Command` searches `PATH` like `execvp`, as libuv does.
///
/// Unix 上 `Command::spawn` 交给 `execvp`，`PATH` 查找由它完成，所以这里原样返回程序名；
/// `cwd` 与 `env` 只是为与 Windows 版保持同一签名（Windows 必须自己搜索）。返回类型是
/// `Result`，但 Unix 分支不会失败。
pub fn resolve_program(
    program: &str,
    _cwd: &str,
    _env: &Map<String, Value>,
) -> Result<String, Failure> {
    Ok(program.to_string())
}

/// 用 `which bash` 在 `PATH` 中查找 bash：静默 stderr，只取 stdout 第一行；命令失败或
/// 结果路径不存在都返回 `None`（由 `shell_config` 回退到 `"sh"`）。
fn which_bash() -> Option<String> {
    let output = Command::new("which")
        .arg("bash")
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
        .to_string();
    path_exists(&first).then_some(first)
}

/// Node's shell resolution: a configured shell must exist; otherwise `/bin/bash`, `which bash`, then `sh`.
///
/// 顺序固定：显式 `shellPath`（`path_exists` 跟随符号链接，必须存在）→ `/bin/bash` → `which bash`
/// 的第一个结果（仍需 `path_exists`）→ `"sh"`。显式路径不存在时返回 `shell_unavailable`
/// （消息里带原路径）；后两级只是回退，若 `sh` 也不存在，错误会在 `Command::spawn` 时以
/// `spawn_error` 出现。`which` 使用守护进程自己的 `PATH`，不读请求里的 `env`。
pub fn shell_config(shell_path: Option<&str>) -> Result<ShellConfig, Failure> {
    let shell = |program: &str| ShellConfig {
        program: program.into(),
        args: vec!["-c".into()],
        command_on_stdin: false,
    };
    if let Some(configured) = shell_path {
        if path_exists(configured) {
            return Ok(shell(configured));
        }
        return Err(Failure::new(
            "shell_unavailable",
            format!("Custom shell path not found: {configured}"),
        ));
    }
    if path_exists("/bin/bash") {
        return Ok(shell("/bin/bash"));
    }
    Ok(shell(&which_bash().unwrap_or_else(|| "sh".into())))
}

/// Arguments, environment and process setup as libuv applies them.
///
/// 环境处理：`inherit_env` 为 false 时先 `env_clear`，否则保留守护进程环境；随后逐条写入请求
/// 的 `env`，只接受 JSON 字符串值，非字符串值被忽略。
///
/// `pre_exec` 在 fork 之后、exec 之前的子进程里运行，此处只能调用 async-signal-safe 的函数：
/// `setsid` 建立新会话（同时成为进程组组长，供 `kill_tree` 整组回收），然后按 libuv 的做法
/// 把编号 1 到 31 的信号（`SIGKILL`/`SIGSTOP` 除外）恢复默认处置并清空信号掩码，避免
/// 守护进程的忽略/阻塞状态遗传给命令。这些调用失败不会中止 spawn，返回值都被忽略；
/// Unix 不设置 umask。
pub fn configure(
    command: &mut Command,
    args: &[String],
    env: &Map<String, Value>,
    inherit_env: bool,
) {
    command.args(args);
    if !inherit_env {
        command.env_clear();
    }
    for (key, value) in env {
        if let Some(value) = value.as_str() {
            command.env(key, value);
        }
    }
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(|| {
            // A new session, so a kill reaches every descendant; default signal handling and an empty mask, as libuv sets.
            libc::setsid();
            for signal in 1..32 {
                if signal != libc::SIGKILL && signal != libc::SIGSTOP {
                    libc::signal(signal, libc::SIG_DFL);
                }
            }
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigprocmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut());
            Ok(())
        });
    }
}

/// Unix 版没有 spawn 后的额外步骤：进程组已在 `pre_exec` 的 `setsid` 中建立。
///
/// 与 Windows 的 Job Object 对应；保留这个空函数让 `exec::spawn` 两端调用一致。
pub fn after_spawn(_child: &Child) {}

/// Kill the command's process group, or the process if the group is gone.
///
/// `setsid` 让 pid 同时是进程组 id，所以 `-pid` 覆盖 shell 派生出的所有后代。先杀整组，
/// 失败时再直接杀 pid；两次结果都忽略。`exec.rs` 的 timeout/abort/cleanup、`pty.rs` 的
/// abort/kill，以及 `main.rs` 断线时的 `kill_all`（均经 `exec::kill_group`）最终调用这里。
pub fn kill_tree(pid: u32) {
    let pid = pid as i32;
    // SAFETY: plain signal delivery.
    unsafe {
        if libc::kill(-pid, libc::SIGKILL) != 0 {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

/// A process killed by a signal has no exit code; report 128 + the signal, as the shell does.
///
/// 正常退出取 `code()`；被信号杀死时取 `128 + signal`（`SIGKILL` → 137，`SIGTERM` → 143），
/// 这样客户端看到的结果与 Node 的 `exitCode` 一致。`signal()` 也为 `None` 时按 128 处理。
pub fn exit_code(status: ExitStatus) -> i64 {
    i64::from(
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
    )
}

/// The bytes of a file name, which libuv sorts directory listings by.
///
/// `fs::dir_entry` 在与 UTF-8 解码结果不同时把原始字节放进条目的 `raw` 字段，供按 libuv
/// 的字节序处理目录名；合法 UTF-8 名字不需要额外字段。
pub fn raw_name(name: &OsStr) -> Option<Vec<u8>> {
    Some(name.as_bytes().to_vec())
}

/// Windows only: the per-drive working directories of the `=C:` variables.
///
/// Unix 没有盘符概念，返回空表，`hello.driveCwds` 因此是 `{}`；与 Windows 版保持同一签名，
/// `main.rs` 因此无需条件编译。
pub fn drive_cwds() -> Map<String, Value> {
    Map::new()
}

/// 转发 `std::fs::read_dir`，返回惰性迭代器。
///
/// `main.rs` 的 `opendir` 把迭代器存进目录句柄，`readdir` 每次按 `max` 取一页，因而保持
/// 文件系统返回的顺序；错误由调用方包装成 `Failure::io(..., "opendir")`。
pub fn read_dir(path: &str) -> io::Result<std::fs::ReadDir> {
    std::fs::read_dir(path)
}

/// Device and inode of `path`; the metadata already has them.
///
/// Unix 的 `dev`/`ino` 就在 `lstat`/`stat` 得到的 `Metadata` 里，不需要再打开路径；
/// `path` 与 `follow` 参数是为 Windows 版保留的同一签名。`follow` 的语义已经体现在调用方
/// 传入的 metadata 来自 `stat` 还是 `lstat`。
pub fn path_identity(_path: &str, metadata: &Metadata, _follow: bool) -> (u64, u64) {
    identity(metadata)
}

#[cfg(test)]
mod tests {
    //! 锁定平台层的关键语义：libuv 错误名表、tmpdir 形状、mkdtemp 的六个随机字符与 NUL 拒绝、
    //! 信号退出码、Node 的打开模式、`O_NOFOLLOW` 到 `SYMLINK` 的翻译、shell 解析顺序，以及
    //! `read_pipe` 的三种结束条件（EOF、`stop`、回调返回 false）。这些测试不依赖外部服务。

    use super::*;

    // The libuv table (spec §2.2.9): 25 codes, every other errno "UNKNOWN".
    const NAMES: [(&str, i32); 25] = [
        ("EPERM", libc::EPERM),
        ("ENOENT", libc::ENOENT),
        ("EIO", libc::EIO),
        ("EBADF", libc::EBADF),
        ("EAGAIN", libc::EAGAIN),
        ("ENOMEM", libc::ENOMEM),
        ("EACCES", libc::EACCES),
        ("EBUSY", libc::EBUSY),
        ("EEXIST", libc::EEXIST),
        ("EXDEV", libc::EXDEV),
        ("ENOTDIR", libc::ENOTDIR),
        ("EISDIR", libc::EISDIR),
        ("EINVAL", libc::EINVAL),
        ("ENFILE", libc::ENFILE),
        ("EMFILE", libc::EMFILE),
        ("ETXTBSY", libc::ETXTBSY),
        ("EFBIG", libc::EFBIG),
        ("ENOSPC", libc::ENOSPC),
        ("EROFS", libc::EROFS),
        ("EMLINK", libc::EMLINK),
        ("ENAMETOOLONG", libc::ENAMETOOLONG),
        ("ENOTEMPTY", libc::ENOTEMPTY),
        ("ELOOP", libc::ELOOP),
        ("ENOSYS", libc::ENOSYS),
        ("ENOTSUP", libc::ENOTSUP),
    ];

    #[test]
    fn names_every_libuv_error_and_unknown_otherwise() {
        for (name, errno) in NAMES {
            let error = io::Error::from_raw_os_error(errno);
            assert_eq!(os_error_name(&error), name, "errno {errno}");
        }
        assert_eq!(os_error_name(&io::Error::from_raw_os_error(999)), "UNKNOWN");
        assert_eq!(os_error_name(&io::Error::other("mine")), "UNKNOWN");
    }

    #[test]
    fn tmpdir_falls_back_through_the_environment_and_drops_one_trailing_slash() {
        // The daemon reads TMPDIR, TMP, TEMP; the tests run in one process, so only the shape is fixed here.
        let path = tmpdir();
        assert!(!path.is_empty());
        assert!(
            path.len() == 1 || !path.ends_with('/'),
            "trailing slash removed: {path:?}"
        );
    }

    #[test]
    fn mkdtemp_creates_a_directory_with_six_random_characters() {
        let base =
            std::env::temp_dir().join(format!("kiana-env-unix-mkdtemp-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let prefix = format!("{}/pre-", base.to_string_lossy());
        let created = mkdtemp(&prefix).unwrap();
        assert!(created.starts_with(&prefix));
        assert_eq!(created.len(), prefix.len() + 6);
        assert!(std::path::Path::new(&created).is_dir());
        // A prefix with a NUL byte cannot be a C string: EINVAL as libuv reports.
        let error = mkdtemp("bad\0prefix").unwrap_err();
        assert_eq!(os_error_name(&error), "EINVAL");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn exit_code_is_the_code_or_128_plus_the_signal() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(exit_code(ExitStatus::from_raw(0)), 0);
        assert_eq!(exit_code(ExitStatus::from_raw(3 << 8)), 3);
        // Killed by SIGTERM: 128 + 15.
        assert_eq!(exit_code(ExitStatus::from_raw(15)), 143);
        assert_eq!(exit_code(ExitStatus::from_raw(9)), 137);
    }

    #[test]
    fn open_modes_follow_node() {
        let base = std::env::temp_dir().join(format!("kiana-env-unix-open-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("a.txt").to_string_lossy().into_owned();
        // `w` creates and truncates; `a` appends without truncating.
        let mut file = open(&path, OpenMode::Write).unwrap();
        use std::io::Write;
        file.write_all(b"abc").unwrap();
        drop(file);
        let mut file = open(&path, OpenMode::Append).unwrap();
        file.write_all(b"d").unwrap();
        drop(file);
        assert_eq!(std::fs::read(&path).unwrap(), b"abcd");
        let file = open(&path, OpenMode::Write).unwrap();
        drop(file);
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        // `r` and `r+` never create the file.
        let missing = base.join("missing").to_string_lossy().into_owned();
        assert_eq!(
            os_error_name(&open(&missing, OpenMode::Read).unwrap_err()),
            "ENOENT"
        );
        assert_eq!(
            os_error_name(&open(&missing, OpenMode::ReadWrite).unwrap_err()),
            "ENOENT"
        );
        open(&path, OpenMode::ReadWrite).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reader_errors_become_symlink_for_a_final_link() {
        let base =
            std::env::temp_dir().join(format!("kiana-env-unix-reader-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("a.txt");
        std::fs::write(&file, b"hello").unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let failure = open_reader(&link.to_string_lossy(), true).unwrap_err();
        assert_eq!(failure.code, "SYMLINK");
        assert_eq!(failure.message, "Refusing to follow a symbolic link");
        assert_eq!(
            failure.to_json()["path"],
            serde_json::json!(link.to_string_lossy())
        );
        open_reader(&link.to_string_lossy(), false).unwrap();
        let missing = open_reader(&base.join("missing").to_string_lossy(), false).unwrap_err();
        assert_eq!(missing.code, "ENOENT");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn shell_config_prefers_an_existing_path_then_bash_then_sh() {
        let configured = shell_config(Some("/bin/sh")).unwrap();
        assert_eq!(configured.program, "/bin/sh");
        assert_eq!(configured.args, ["-c"]);
        assert!(!configured.command_on_stdin);
        let Err(failure) = shell_config(Some("/does/not/exist")) else {
            panic!("a missing custom shell is an error");
        };
        assert_eq!(failure.code, "shell_unavailable");
        assert_eq!(
            failure.message,
            "Custom shell path not found: /does/not/exist"
        );
        assert!(!shell_config(None).unwrap().program.is_empty());
    }

    #[test]
    fn resolve_program_and_raw_name_leave_them_to_execvp() {
        assert_eq!(
            resolve_program("sh", "/", &Map::new()).unwrap(),
            "sh",
            "the Unix daemon leaves PATH lookup to Command"
        );
        assert_eq!(raw_name(OsStr::new("a.txt")).unwrap(), b"a.txt");
        assert!(drive_cwds().is_empty());
    }

    #[test]
    fn read_pipe_stops_at_the_end_of_the_pipe() {
        use std::os::unix::net::UnixStream;
        let (mut writer, reader) = UnixStream::pair().unwrap();
        use std::io::Write;
        writer.write_all(b"hello").unwrap();
        drop(writer);
        let stop = AtomicBool::new(false);
        let mut collected = Vec::new();
        read_pipe(reader, &stop, |bytes| {
            collected.extend_from_slice(bytes);
            true
        });
        assert_eq!(collected, b"hello");
        // `stop` ends the loop without draining the pipe.
        let (mut writer, reader) = UnixStream::pair().unwrap();
        writer.write_all(b"more").unwrap();
        let stop = AtomicBool::new(true);
        read_pipe(reader, &stop, |_| panic!("on_data runs despite stop"));
        // A callback returning false stops the reader too.
        let (mut writer, reader) = UnixStream::pair().unwrap();
        writer.write_all(b"again").unwrap();
        let stop = AtomicBool::new(false);
        let mut seen = 0;
        read_pipe(reader, &stop, |_| {
            seen += 1;
            false
        });
        assert_eq!(seen, 1);
        interrupt_reader(&std::thread::spawn(|| {}));
    }
}
