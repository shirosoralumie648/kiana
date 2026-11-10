//! `sys/`：平台层（unix / windows）。
//!
//! TS 源：packages/env/daemon/src/sys/{mod.rs,unix.rs,windows.rs}（该 daemon 本来就是 Rust）。
//!
//! 划分：
//! - `unix.rs`：错误名、定位读、临时目录、打开读者、启动与杀进程；
//! - `windows.rs`：Job Object、`CreateFileW` 模式映射、`search_path`、`quote_argument`、`GetLastError` 表。
//!
//! 两者都只被 `main.rs` / `exec.rs` / `pty.rs` / `fs.rs` / `watch.rs` 调用；公共签名放在本文件，
//! `cfg` 分派到唯一实现。
//! What differs between operating systems: error names, positional reads, temporary directories, opening readers,
//! and starting and killing commands. Each implementation follows what Node (libuv) does on that system.
//!
//! # 这个模块解决什么问题
//! 守护进程在远端机器上替 Node 的 `NodeExecutionEnv` 干活：文件错误码、`stat` 语义、进程组、
//! 管道读法都必须和 Node/libuv 在**同一台机器**上的表现一致。Rust 标准库与 libuv 有不少差异
//! （例如 `File::create` 的打开方式、信号掩码、进程组），这些差异按平台集中在这里实现，
//! 上层 `fs` / `exec` / `watch` 的平台敏感操作都经本模块；PTY 是例外，它在 `pty.rs` 里另有
//! 自己的 `cfg(unix)` 子模块。
//!
//! # 编译期分派
//! `unix` 与 `windows` 两个子模块由 `cfg` 二选一，并通过 `pub use ...::*` 重导出到 `sys`。
//! 共享代码按**同一组**公开函数（名字与签名一致）调用它们，某一平台缺少其中一个就会在
//! 该平台编译失败；本文件顶层的类型与辅助函数（`ShellConfig`、`OpenMode`、`Synthetic`、
//! `error_name`、`path_exists`、`random_bytes`、`cwd`）与平台无关，所有平台共用。
//! 目前没有第三个平台的实现：支持的远端系统只有 Linux、macOS、Android 与 Windows
//! （见 crate 的 README），新增平台必须补齐整套函数。
//!
//! # 主要调用关系
//! - `fs.rs`：`mtime`/`identity`/`realpath`/`rename`/`remove_file`/`open`/`open_reader`/
//!   `read_at`/`raw_name`/`mkdtemp`/`synthetic`；
//! - `exec.rs`：`shell_config`/`resolve_program`/`configure`/`after_spawn`/`read_pipe`/
//!   `interrupt_reader`/`kill_tree`/`exit_code`/`random_bytes`；
//! - `pty.rs`：`path_exists`、`exit_code`；
//! - `watch.rs`：`path_identity`/`mtime`/`PATH_SEPARATOR`；
//! - `main.rs`：`home`/`tmpdir`/`cwd`/`drive_cwds`/`PATH_SEPARATOR`/`open`/`read_dir`/`read_at`
//!   （`hello` 帧与 `opendir`/`scanLines`）。
//!
//! # 错误约定
//! 系统调用错误统一用 `io::Error` 向上传递，再由 `errors::Failure::io` 经 `error_name` 转成
//! libuv 错误名（客户端只认 `code`）。少数“libuv 报的名字与系统调用本身不同”的情况用
//! `Synthetic` 伪造一个错误码（例如递归 `mkdir` 遇到文件报 `ENOTDIR`，或 Windows 在目录位置
//! 创建文件报 `EISDIR`）；无法识别的错误统一为 `UNKNOWN`。
//!
//! # 注意事项
//! - 本模块的函数大多**不做类型安全检查**：目录/文件类型由上层 `fs.rs` 复核；
//! - `random_bytes` 只有 16 字节且不是密码学随机数，只用于临时文件名（spill 输出）；
//! - `cwd()` 失败返回空串（`unwrap_or_default`），不是 `"."`；`hello` 帧的调用方必须能接受空串。

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// How a string command runs: the shell, its arguments before the command, and whether the command goes to its
/// stdin instead of its arguments (legacy WSL `bash.exe`).
///
/// 字符串形式的 `exec` 请求（`command` 字段）不是直接交给系统，而是由 `exec.rs` 先用
/// `shell_config` 解析出一个 shell，再把命令正文拼进去。Unix 通常是 `bash -c <命令>`；
/// 旧版 WSL 的 `bash.exe` 走 `-s` 并从 stdin 读脚本，所以需要 `command_on_stdin` 区分。
pub struct ShellConfig {
    /// 要执行的 shell 程序。Unix 可能是交给 `execvp` 搜索的名字（如 `sh`）；Windows 是搜索后的路径。
    pub program: String,
    /// 命令正文之前的固定参数（`-c`，旧版 WSL 为 `-s`）。
    pub args: Vec<String>,
    /// 为 true 时命令正文写入子进程 stdin；为 false 时正文作为 `args` 的最后一个元素。
    pub command_on_stdin: bool,
}

/// How Node opens files: `r`, `writeFile` (`w`), `appendFile` (`a`), and `r+` for truncate and fsync.
///
/// OS 的打开标志与 Node 的字符串模式并不一一对应（尤其是 Windows），所以上层只传这个枚举，
/// 由各平台映射。它只描述“怎么打开”，不负责创建父目录——那一步由 `fs.rs` 按 Node 的
/// 递归 `mkdir` 语义先做。
#[derive(Clone, Copy)]
pub enum OpenMode {
    /// Node's `open(path, "r")`: any kind of file, blocking reads.
    Read,
    /// Node 的 `w`：不存在则创建，存在则截断为 0 字节。
    Write,
    /// Node 的 `a`：不存在则创建，写入总是追加到末尾，不截断。
    Append,
    /// Node 的 `r+`：必须已存在，可读可写、不截断；`fs.rs` 的 `truncate`/`fsync` 用它拿可写句柄。
    ReadWrite,
}

/// An error code libuv reports where the system call itself reported another one, such as `ENOTDIR` from a recursive
/// `mkdir` through a file, or `EISDIR` when Windows creates a file where a directory exists.
///
/// `io::Error` 只能携带一个 OS 错误码，表达不了“libuv 改写了错误名”的情况；这个类型让调用方
/// 可以伪造一个只带 libuv 名字的错误，`error_name` 会优先识别它。元组里唯一的字段就是
/// libuv 错误名（如 `ENOTDIR`），`Display` 直接输出它。
#[derive(Debug)]
pub struct Synthetic(pub &'static str);

impl std::fmt::Display for Synthetic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for Synthetic {}

/// 构造一个只带 libuv 错误名、没有 OS errno 的 `io::Error`。
///
/// 注意错误对象里没有 `raw_os_error()`，所以平台错误表查不到它；`error_name` 会先做
/// `Synthetic` 的 downcast，命中就直接返回这个名字。`fs.rs` 的递归 `mkdir` 与 Windows
/// 的 `open` 依赖这一点来上报 libuv 特有的错误码。
pub fn synthetic(code: &'static str) -> std::io::Error {
    std::io::Error::other(Synthetic(code))
}

/// The libuv name of an error: a synthetic code, or the system's code translated per platform.
///
/// 返回 `&'static str`（错误名来自静态表），未知错误一律为 `UNKNOWN`。`errors::Failure::io`
/// 用它作为客户端可见的错误 `code`，所以这里的映射必须和远端 Node 的表现一致。
pub fn error_name(error: &std::io::Error) -> &'static str {
    if let Some(Synthetic(code)) = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<Synthetic>())
    {
        return code;
    }
    os_error_name(error)
}

/// Whether a path exists, following symbolic links, like Node's `access(path, F_OK)`.
///
/// 实现是 `Path::metadata().is_ok()`：`metadata` 跟随符号链接，所以悬空链接返回 false；
/// 父目录不可搜索等“取元数据失败”的情况同样返回 false，它不区分“不存在”和“没权限”。
/// `shell_config` 用它校验显式 shell 路径，`pty.rs` 用它过滤 `$SHELL`。
pub fn path_exists(path: &str) -> bool {
    std::path::Path::new(path).metadata().is_ok()
}

/// Sixteen random bytes for temporary names (not for secrets).
///
/// 实现：用 `RandomState` 的 `BuildHasher`（标准库默认散列器）把静态计数器与当前时间纳秒
/// 散列两次，每次取 8 字节拼成 16 字节。计数器使同一进程内连续两次调用的输入必然不同（散列
/// 输出理论上仍可能碰撞，但可忽略），随机种子与时间戳让不同进程之间也不易重合。它
/// **不是密码学随机数**，只用于 `exec.rs` 生成 spill 文件名（`random_uuid`），不要用于
/// 令牌或密钥。
pub fn random_bytes() -> [u8; 16] {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let state = std::collections::hash_map::RandomState::new();
    let mut bytes = [0u8; 16];
    for half in 0..2 {
        let mut hasher = state.build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|time| time.as_nanos())
                .unwrap_or(0),
        );
        bytes[half * 8..half * 8 + 8].copy_from_slice(&hasher.finish().to_le_bytes());
    }
    bytes
}

/// The daemon's working directory, the `process.cwd()` Node's `path.resolve` falls back to.
///
/// 取不到当前目录时返回**空串**而不是 `"."`（`unwrap_or_default`）；`hello` 帧把它原样上报，
/// 调用方需要能接受空值。路径经 `to_string_lossy` 转换，非 UTF-8 部分会被替换字符替代。
pub fn cwd() -> String {
    std::env::current_dir()
        .map(|cwd| cwd.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_synthetic_code_names_its_error() {
        let error = synthetic("ENOTDIR");
        assert_eq!(error.to_string(), "ENOTDIR");
        assert_eq!(error_name(&error), "ENOTDIR");
        // Without one, the OS error name is used.
        #[cfg(unix)]
        assert_eq!(
            error_name(&std::io::Error::from_raw_os_error(libc::ENOENT)),
            "ENOENT"
        );
        assert_eq!(
            error_name(&std::io::Error::other("mine")),
            "UNKNOWN",
            "an error with no OS code has no libuv name"
        );
    }

    #[test]
    fn path_exists_follows_links() {
        let base = std::env::temp_dir().join(format!("kiana-env-sysmod-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("a");
        std::fs::write(&file, b"").unwrap();
        assert!(path_exists(&file.to_string_lossy()));
        assert!(!path_exists(&base.join("missing").to_string_lossy()));
        #[cfg(unix)]
        {
            let link = base.join("link");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert!(path_exists(&link.to_string_lossy()));
            let dangling = base.join("dangling");
            std::os::unix::fs::symlink(base.join("missing"), &dangling).unwrap();
            assert!(!path_exists(&dangling.to_string_lossy()));
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn random_bytes_are_sixteen_and_differ_between_calls() {
        // Only sixteen random bytes for temporary names; two calls must not repeat.
        let first = random_bytes();
        let second = random_bytes();
        assert_eq!(first.len(), 16);
        assert_ne!(first, second);
    }

    #[test]
    fn cwd_is_the_process_directory_not_a_dot() {
        let cwd = cwd();
        assert_eq!(
            cwd,
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        );
    }
}
