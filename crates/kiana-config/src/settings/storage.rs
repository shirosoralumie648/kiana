//! `FileSettingsStorage` / `InMemorySettingsStorage` / `SettingsStorage` trait。
//!
//! TS: `packages/coding-agent/src/core/settings-manager.ts` 的
//! `SettingsStorage` / `FileSettingsStorage` / `InMemorySettingsStorage`。
//!
//! 与 TS 一样，存储层是**同步**的：`with_lock(scope, fn)` 读出当前文件内容（文件不存在
//! 时为 `None`；存在但 `open`/`read` 失败则返回错误而不是 `None`），交给 `fn`，`fn` 返回
//! `Some(next)` 时写回。TS 的 `fn` 可能抛错（`JSON.parse` 失败等），Rust 侧对应闭包返回
//! `Result`。
//!
//! `FileSettingsStorage` 的加锁语义（`ca-config-models.md` §2.9.3）：
//!
//! 1. 只有文件**已存在**时才在读取前加锁（不存在时不创建锁文件）；
//! 2. 需要写时先 `mkdir -p` 父目录，再（若尚未加锁）加锁，最后写入；
//! 3. 锁是 `proper-lockfile` 的目录锁（`<path>.lock`，`realpath: false`，10×20 ms 重试，
//!    只重试 ELOCKED）——复用 [`crate::auth::lock`] 的实现；
//! 4. 写入 UTF-8、无 BOM、不追加结尾换行（由调用方给出的字符串决定）。
//!
//! # 调用方
//!
//! `settings::mod` 的 [`crate::settings::SettingsManager`] 通过
//! `Arc<dyn SettingsStorage>` 持有一个后端实例：
//!
//! - 加载走只读回调：`with_lock(scope, |current| { ...; Ok(None) })`；
//! - 写盘走读写回调：`with_lock(scope, |current| { ...; Ok(Some(next)) })`，回调在锁内
//!   完成「读当前文件 → 迁移 → 覆盖 modified 字段」的全部计算（见
//!   `SettingsManager::persist_scoped_settings`）。
//!
//! 测试用 [`InMemorySettingsStorage`] 预置 JSON 字符串来跳过真实文件系统。
//!
//! # 注意
//!
//! - `with_lock` 是**同步** API，会在调用线程上做文件 I/O；`SettingsManager` 的写 worker
//!   在 tokio 任务里直接调用它，因此写设置的 I/O 会阻塞该 worker；getter 只读内存快照，
//!   不参与这条 I/O 路径。
//! - 存储层不负责 JSON 解析/校验，也不保证并发调用之间的顺序；互斥只来自
//!   [`FileSettingsStorage`] 在文件已存在时取得的目录锁与 [`InMemorySettingsStorage`]
//!   的 `Mutex`（文件不存在的只读调用不取锁）。

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use kiana_core::config::CONFIG_DIR_NAME;
use kiana_core::paths::{PathInputOptions, resolve_path};

use crate::auth::lock::{acquire_lock_sync_with_retry, node_io_error_parts};
use crate::settings::SettingsScope;

/// 存储层的错误（任意 `Error`；`Display` 是 Node 风格的错误文案）。
///
/// TS 的 `withLock` 可以抛任意 `Error`；Rust 侧用装箱错误透传，`to_string()` 即
/// `error.message`。
pub type SettingsStorageError = Box<dyn std::error::Error + Send + Sync>;

/// `withLock` 的回调：`current` 是文件的当前内容（文件不存在时为 `None`），
/// 返回 `Some(next)` 表示写回。
pub type SettingsLockCallback<'a> =
    dyn FnMut(Option<&str>) -> Result<Option<String>, SettingsStorageError> + 'a;

/// `SettingsStorage`：`withLock(scope, fn)`。
///
/// `fn(current)` 的 `current` 是文件的当前内容（文件不存在时为 `None`）；返回
/// `Some(next)` 表示写回。闭包返回 `Err` 时错误原样抛给调用方（TS 的 `JSON.parse` 抛错），
/// 且后端必须保持原内容不变。
///
/// 约束 `Send + Sync + 'static` 是因为实例以 `Arc<dyn SettingsStorage>` 的形式被
/// `SettingsManager` 的所有克隆共享（包括写 worker 任务），需要能跨线程使用并在 `Arc`
/// 里长期存活。
pub trait SettingsStorage: Send + Sync + 'static {
    /// 在 `scope` 对应的存储上持锁执行 `f`。
    fn with_lock(
        &self,
        scope: SettingsScope,
        f: &mut SettingsLockCallback<'_>,
    ) -> Result<(), SettingsStorageError>;
}

/// `FileSettingsStorage`：`<agent-dir>/settings.json` 与 `<cwd>/.kiana/settings.json`。
///
/// 两个路径在构造时一次性解析成字符串并保存；之后再改变进程 cwd 或传入相对路径的基准
/// 都不会影响已有实例。`SettingsManager::create*` 用同一对路径写进错误诊断，所以两处
/// 必须用相同的解析规则（本模块的 `resolve_from_cwd` + `join_path`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSettingsStorage {
    global_settings_path: String,
    project_settings_path: String,
}

impl FileSettingsStorage {
    /// TS: `constructor(cwd, agentDir)`：两者都先 `resolvePath`，再 `path.join`。
    pub fn new(cwd: &str, agent_dir: &str) -> Self {
        let resolved_cwd = resolve_from_cwd(cwd);
        let resolved_agent_dir = resolve_from_cwd(agent_dir);
        Self {
            global_settings_path: join_path(&resolved_agent_dir, &["settings.json"]),
            project_settings_path: join_path(&resolved_cwd, &[CONFIG_DIR_NAME, "settings.json"]),
        }
    }

    /// 全局 settings.json 的绝对路径。
    pub fn global_settings_path(&self) -> &str {
        &self.global_settings_path
    }

    /// 项目 settings.json 的绝对路径。
    pub fn project_settings_path(&self) -> &str {
        &self.project_settings_path
    }

    fn path_for(&self, scope: SettingsScope) -> &str {
        match scope {
            SettingsScope::Global => &self.global_settings_path,
            SettingsScope::Project => &self.project_settings_path,
        }
    }
}

impl SettingsStorage for FileSettingsStorage {
    fn with_lock(
        &self,
        scope: SettingsScope,
        f: &mut SettingsLockCallback<'_>,
    ) -> Result<(), SettingsStorageError> {
        let path = PathBuf::from(self.path_for(scope));
        // `exists()` 与随后的加锁/读取是彼此独立的系统调用，中间文件可能被创建或删除；
        // 本实现按**检查时刻**的结果决定走「已有文件」还是「新建文件」分支。
        let file_exists = path.exists();

        // 只有文件存在时才加锁：读取一个不存在的文件不需要锁，也不创建 `.kiana` 目录。
        let mut guard = if file_exists {
            Some(
                acquire_lock_sync_with_retry(&path)
                    .map_err(|error| -> SettingsStorageError { Box::new(error) })?,
            )
        } else {
            None
        };

        let current = if file_exists {
            Some(read_utf8_lossy(&path)?)
        } else {
            None
        };

        // 回调在锁内执行（文件存在时已经持锁）；返回 `Some(next)` 才写回，返回 `None` 的
        // 只读调用不会创建目录、也不加锁。
        let next = f(current.as_deref())?;
        if let Some(next) = next {
            // 只有真正要写时才创建目录。
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
                && !parent.exists()
            {
                fs::create_dir_all(parent)
                    .map_err(|error| node_fs_error_with_path(&error, "mkdir", parent))?;
            }
            // 文件在检查时刻不存在，因此到这里才第一次加锁。若期间别的进程创建了该文件，
            // 本次回调拿到的 `current` 仍是 `None`，写回的内容只基于这个 `None` 快照。
            if guard.is_none() {
                guard = Some(
                    acquire_lock_sync_with_retry(&path)
                        .map_err(|error| -> SettingsStorageError { Box::new(error) })?,
                );
            }
            write_utf8(&path, &next)?;
        }

        // `LockGuard` 的 `Drop` 会释放锁；显式 drop 让释放发生在返回之前。
        drop(guard);
        Ok(())
    }
}

/// `InMemorySettingsStorage`：两个作用域各存一份 JSON 字符串。
///
/// 两个作用域各有一把 `Mutex`，互不阻塞；`with_lock` 在持锁期间执行回调，回调成功返回
/// `Some` 才替换字符串，返回 `Err` 时内容保持不变。锁中毒（回调 panic）后继续使用内部值。
#[derive(Debug, Default)]
pub struct InMemorySettingsStorage {
    global: Mutex<Option<String>>,
    project: Mutex<Option<String>>,
}

impl InMemorySettingsStorage {
    /// 空的存储。
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&self, scope: SettingsScope) -> &Mutex<Option<String>> {
        match scope {
            SettingsScope::Global => &self.global,
            SettingsScope::Project => &self.project,
        }
    }
}

impl SettingsStorage for InMemorySettingsStorage {
    fn with_lock(
        &self,
        scope: SettingsScope,
        f: &mut SettingsLockCallback<'_>,
    ) -> Result<(), SettingsStorageError> {
        let mut slot = self
            .slot(scope)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 回调在持锁期间执行；`?` 让错误原样返回，且不会修改已存的内容。
        let next = f(slot.as_deref())?;
        if let Some(next) = next {
            *slot = Some(next);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

/// `resolvePath(input)`（`baseDir` 默认 `process.cwd()`）。
///
/// 相对路径按**调用时**的进程 cwd 解析；cwd 读取失败时退回 `"."` 继续（Node 的
/// `process.cwd()` 在这种情况下会抛错；该分支未在本文件的测试中覆盖）。
pub(crate) fn resolve_from_cwd(input: &str) -> String {
    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    resolve_path(input, &cwd, &PathInputOptions::default())
}

/// `path.join(base, parts...)`（posix/Windows 的原生分隔符）。
pub(crate) fn join_path(base: &str, parts: &[&str]) -> String {
    let mut path = PathBuf::from(base);
    for part in parts {
        path.push(part);
    }
    path.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// 文件 I/O
// ---------------------------------------------------------------------------

/// `readFileSync(path, "utf-8")`：Node 用 UTF-8 解码，非法字节替换为 U+FFFD。
///
/// Node 先 `open` 再 `read`：`open` 失败时错误带路径（`syscall: "open"`），`read` 失败时
/// 不带路径（例如读目录：`EISDIR: illegal operation on a directory, read`）。
fn read_utf8_lossy(path: &Path) -> Result<String, SettingsStorageError> {
    let mut file =
        fs::File::open(path).map_err(|error| node_fs_error_with_path(&error, "open", path))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| node_fs_error_without_path(&error, "read"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `writeFileSync(path, contents, "utf-8")`：UTF-8，无 BOM，不追加换行。
///
/// `open` 失败带路径；`write` 失败不带路径（Node/libuv 的
/// `ENOSPC: no space left on device, write`）。
///
/// 不是原子写：`File::create` 先截断已有内容，`write_all` 中途失败时磁盘上可能留下
/// 空文件或半截内容（没有临时文件 + rename 的兜底）。
fn write_utf8(path: &Path, contents: &str) -> Result<(), SettingsStorageError> {
    let mut file =
        fs::File::create(path).map_err(|error| node_fs_error_with_path(&error, "open", path))?;
    file.write_all(contents.as_bytes())
        .map_err(|error| node_fs_error_without_path(&error, "write"))
}

/// Node/libuv 风格的 `CODE: message, syscall 'path'`（`open` / `mkdir` 等带路径的调用）。
fn node_fs_error_with_path(
    error: &io::Error,
    syscall: &'static str,
    path: &Path,
) -> SettingsStorageError {
    let (code, message) = node_io_error_parts(error);
    Box::new(FsStorageError {
        message: format!("{code}: {message}, {syscall} '{}'", path.display()),
    })
}

/// Node/libuv 风格的 `CODE: message, syscall`（`read` / `write` 不带路径）。
fn node_fs_error_without_path(error: &io::Error, syscall: &'static str) -> SettingsStorageError {
    let (code, message) = node_io_error_parts(error);
    Box::new(FsStorageError {
        message: format!("{code}: {message}, {syscall}"),
    })
}

#[derive(Debug)]
struct FsStorageError {
    message: String,
}

impl std::fmt::Display for FsStorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FsStorageError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn node_fs_errors_use_node_code_description_and_syscall() {
        let open_path = Path::new("/tmp/settings.json");
        let no_entry = node_fs_error_with_path(
            &io::Error::from_raw_os_error(libc::ENOENT),
            "open",
            open_path,
        );
        assert_eq!(
            no_entry.to_string(),
            "ENOENT: no such file or directory, open '/tmp/settings.json'"
        );

        let eacces = node_fs_error_with_path(
            &io::Error::from_raw_os_error(libc::EACCES),
            "open",
            open_path,
        );
        assert_eq!(
            eacces.to_string(),
            "EACCES: permission denied, open '/tmp/settings.json'"
        );

        // 读目录：Node 在 read 阶段报错，不带路径。
        let eisdir_read =
            node_fs_error_without_path(&io::Error::from_raw_os_error(libc::EISDIR), "read");
        assert_eq!(
            eisdir_read.to_string(),
            "EISDIR: illegal operation on a directory, read"
        );

        // 打开/写入目录：Node 在 open 阶段报错，带路径。
        let eisdir_open = node_fs_error_with_path(
            &io::Error::from_raw_os_error(libc::EISDIR),
            "open",
            open_path,
        );
        assert_eq!(
            eisdir_open.to_string(),
            "EISDIR: illegal operation on a directory, open '/tmp/settings.json'"
        );

        // 写满的磁盘：Node 在 write 阶段报错，不带路径（`/dev/full` 实测）。
        let enospc =
            node_fs_error_without_path(&io::Error::from_raw_os_error(libc::ENOSPC), "write");
        assert_eq!(enospc.to_string(), "ENOSPC: no space left on device, write");

        let eio = node_fs_error_without_path(&io::Error::from_raw_os_error(libc::EIO), "read");
        assert_eq!(eio.to_string(), "EIO: i/o error, read");

        let erofs = node_fs_error_with_path(
            &io::Error::from_raw_os_error(libc::EROFS),
            "open",
            open_path,
        );
        assert_eq!(
            erofs.to_string(),
            "EROFS: read-only file system, open '/tmp/settings.json'"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reading_a_directory_matches_the_node_message() {
        let directory = tempfile::tempdir().expect("tempdir");
        let error = read_utf8_lossy(directory.path()).expect_err("reading a directory fails");
        assert_eq!(
            error.to_string(),
            "EISDIR: illegal operation on a directory, read"
        );

        write_utf8(directory.path(), "x").expect_err("writing a directory fails");
        let error = write_utf8(directory.path(), "x").expect_err("writing a directory fails");
        assert!(
            error
                .to_string()
                .starts_with("EISDIR: illegal operation on a directory"),
            "unexpected message: {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn writing_to_dev_full_matches_the_node_message() {
        let dev_full = Path::new("/dev/full");
        if !dev_full.exists() {
            return;
        }
        let error = write_utf8(dev_full, "x").expect_err("writing /dev/full fails");
        assert_eq!(error.to_string(), "ENOSPC: no space left on device, write");
    }
}
