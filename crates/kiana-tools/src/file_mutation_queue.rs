//! core/tools/file-mutation-queue.ts。
//!
//! 把针对**同一真实路径**的文件变更操作串行化；不同路径仍然并行。
//!
//! TS 用两个模块级状态：
//!
//! - `fileMutationQueues: Map<string, Promise<void>>`（key -> 队尾 promise）；
//! - `registrationQueue: Promise<void>`（把「计算 key + 入队」本身也串行化）。
//!
//! Rust 侧逐条复刻：
//!
//! - key 的计算（含 `realpath`）在注册锁内完成，避免两个并发调用对同一文件拿到不同 key；
//! - 每次注册都建立一条 `previous -> release` 链（`Gate`），后注册者等待前一个释放，
//!   因此执行顺序 = 注册顺序，与调度的先后无关（不只是「tokio Mutex 恰好公平」）；
//! - 只有当 map 里仍是本次注册的队尾时才删除表项（不误删后来者的链）；
//! - `fn` panic 时 `ReleaseGuard` 仍会释放链路（等价 TS 的 `finally`）；
//! - **等待中的 future 被 drop（取消）时也会释放链路并清理表项**：TS 的 promise 链不可取消，
//!   `finally` 总会执行；Rust 的 future 可以取消，所以释放守卫必须在 `await` 之前就存在，
//!   否则取消会留下一个永远不开的 gate，后续同路径调用全部挂死（见
//!   `dropping_a_waiting_registration_releases_the_queue` 测试）。
//!
//! # 错误通道
//!
//! TS 的 `getMutationQueueKey` 对 `ENOENT`/`ENOTDIR` 以外的 `realpath` 错误（如 `EACCES`、
//! `ELOOP`、`ENAMETOOLONG`，以及 Node 在 syscall 之前拒绝的含 NUL 路径）**重新抛出**，
//! 整个 `withFileMutationQueue` 随之 reject，`fn` 不会执行、也不会有任何副作用。
//!
//! 因此 [`with_file_mutation_queue`] / [`FileMutationQueue::with`] 返回
//! `Result<T, ToolOperationError>`（错误逐字：`ELOOP: too many symbolic links encountered,
//! realpath '<abs>'` / `ERR_INVALID_ARG_VALUE`）。[`try_with_file_mutation_queue`] /
//! [`FileMutationQueue::try_with`] 是同一语义的保留别名，供 `edit`/`write` 使用。
//!
//! # 执行流程
//!
//! 1. [`FileMutationQueue::with`] 先获取 `registration` 异步锁，在锁内计算 key（含
//!    `realpath`）并把本次注册挂到 map 队尾（`enqueue_key`）；
//! 2. 释放注册锁后进入 `run`：先创建 `ReleaseGuard`，再等待前一条链（从最早的注册开始
//!    依次等待每个 `Gate`）；
//! 3. 执行 `function().await`，完成后 drop 守卫：打开自己的 gate 并（若自己仍是队尾）
//!    删除 map 表项，后来者开始执行；
//! 4. 取消/panic 时守卫同样 drop，因此链不会永久卡死。
//!
//! # 失败与边界
//!
//! - key 计算错误只影响本次调用：`function` 不执行，返回 [`ToolOperationError`]；
//! - 路径不存在时 `canonicalize` 返回 ENOENT/ENOTDIR，key 退化为**词法**绝对路径
//!   （父目录的符号链接不会被展开，因此同一未来文件的两种拼写可能分到不同队列）；
//! - `resolve_absolute` 用 `to_string_lossy`：非 UTF-8 文件名会变成 U+FFFD，不同字节序列
//!   可能得到同一个 key（后果是多串行，不会放松互斥）；
//! - `Gate` 用 `Notify` + `AtomicBool`：`notify_waiters` 不保存许可，所以 `wait()` 先
//!   `enable()` 再复查 `is_open()`，避免漏唤醒；
//! - `tails` 用同步 `Mutex`（清理发生在 `Drop` 里，不能 await）；`registration` 用
//!   `tokio::sync::Mutex`（临界区里要 await `canonicalize`）。

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::read::ToolOperationError;

/// 进程级队列（TS 的模块级 `Map`）。
static GLOBAL_QUEUE: LazyLock<FileMutationQueue> = LazyLock::new(FileMutationQueue::new);

/// 一个可等待的开关：`open()` 之后所有 `wait()` 立即返回；支持任意多个等待者。
struct Gate {
    /// 是否已打开。`open()` 用 `Release`、读取用 `Acquire`；`Notify` 只负责唤醒，
    /// 不保存许可。
    open: AtomicBool,
    /// 等待者通知；`open()` 时唤醒当前已注册的全部等待者。
    notify: Notify,
}

impl Gate {
    fn new() -> Self {
        Self {
            open: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    fn open(&self) {
        self.open.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    async fn wait(&self) {
        if self.is_open() {
            return;
        }
        // 先注册再复查，避免 `open()` 落在检查与真正挂起之间。
        // `Notify::notify_waiters` 只唤醒**已注册**的等待者、不保存许可，因此 `enable()`
        // 之后的第二次 `is_open()` 是必要的：若 open() 已发生，这里直接返回。
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.is_open() {
            return;
        }
        notified.await;
    }
}

/// 一条注册链：`previous` 是上一个注册的队尾；本节点的 `release` 由本次操作完成时打开。
struct QueueTail {
    previous: Option<Arc<QueueTail>>,
    release: Gate,
}

impl QueueTail {
    /// 等待整条链（从最早的注册到本次）依次完成。等价 TS 的 `await currentQueue`。
    fn wait(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            // 从本节点沿 previous 回溯到最早的注册，再从最早开始依次等待：
            // 与 TS 的 promise 链一致，先注册者先获准执行。
            let mut chain: Vec<&QueueTail> = Vec::new();
            let mut node = Some(self);
            while let Some(current) = node {
                chain.push(current);
                node = current.previous.as_deref();
            }
            for current in chain.iter().rev() {
                current.release.wait().await;
            }
        })
    }

    /// 整条链（本节点及所有前驱）的释放开关是否都已打开。
    ///
    /// 只有在这种情况下删表项才安全：若还有前驱正在运行或等待，后来者必须通过本节点
    /// 链到它；提前删表会让后来者误以为队空，与仍在运行的写者并发。
    fn is_fully_released(&self) -> bool {
        let mut node = Some(self);
        while let Some(current) = node {
            if !current.release.is_open() {
                return false;
            }
            node = current.previous.as_deref();
        }
        true
    }
}

/// `panic` / 取消时也打开释放开关并清理表项（等价 TS 的 `finally { releaseNext(); ... }`）。
///
/// 守卫在等待前一个注册**之前**创建：future 在 `previous.wait().await` 或 `function().await`
/// 中被 drop 时，`Drop` 仍会打开本次的 gate，等待它的后来者继续前进。
struct ReleaseGuard<'a> {
    tail: Option<Arc<QueueTail>>,
    key: &'a Path,
    tails: &'a Mutex<HashMap<PathBuf, Arc<QueueTail>>>,
}

impl Drop for ReleaseGuard<'_> {
    fn drop(&mut self) {
        if let Some(tail) = self.tail.take() {
            // 先 open 再清理：顺序与 TS 的 `releaseNext()` / `delete` 一致。
            tail.release.open();
            let mut tails = lock_tails(self.tails);
            // 只有当前队尾的整条链都已释放时才删表项。
            //
            // 旧实现只在 `self` 仍是队尾时删除：若一个**等待中**的注册（B）在轮到自己
            // 之前被取消，B 恰是队尾，删除后表项丢失；此时 A 仍在运行，后来的注册（C）
            // 看到空表就会立刻执行，与 A 并发。保留表项则由 C 的 `previous` 链回 A，
            // `QueueTail::wait` 会先等 A 释放。
            if tails
                .get(self.key)
                .is_some_and(|current| current.is_fully_released())
            {
                tails.remove(self.key);
            }
        }
    }
}

/// 取 map 锁；中毒时沿用内部数据（本模块的临界区不会在持锁时 panic）。
fn lock_tails(
    tails: &Mutex<HashMap<PathBuf, Arc<QueueTail>>>,
) -> std::sync::MutexGuard<'_, HashMap<PathBuf, Arc<QueueTail>>> {
    tails
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 可注入的文件变更队列（测试用）。生产路径用 [`with_file_mutation_queue`] /
/// [`try_with_file_mutation_queue`] 的进程级实例。
pub struct FileMutationQueue {
    /// 串行化「计算 key + 入队」本身（TS 的 `registrationQueue`）。
    registration: AsyncMutex<()>,
    /// key -> 队尾。只用同步锁：注册临界区里要 `await` `realpath`，而清理发生在 `Drop`
    /// （同步）中，两者不能共用一个 async mutex。
    tails: Mutex<HashMap<PathBuf, Arc<QueueTail>>>,
}

impl Default for FileMutationQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl FileMutationQueue {
    /// 空队列。
    pub fn new() -> Self {
        Self {
            registration: AsyncMutex::new(()),
            tails: Mutex::new(HashMap::new()),
        }
    }

    /// 在队列内执行 `function`。同一 key 串行、不同 key 并行。
    ///
    /// 顺序保证：同一 key 的执行顺序 = **注册顺序**（由链式 gate 保证，不依赖调度器的
    /// 唤醒顺序）。`function` 是 `FnOnce`，只在轮到本次注册且 future 未被取消时调用一次。
    ///
    /// key 计算失败时返回错误且**不执行** `function`（TS 的 `withFileMutationQueue` 里
    /// `getMutationQueueKey` 抛出的错误会 reject 整个调用，见模块文档）。
    ///
    /// future 可以在等待期间被 drop（取消）：本次注册的 gate 会被打开、表项被清理，
    /// `function` 不会执行；若取消发生在 `function` 执行中，它自身的 future 被 drop，
    /// 已产生的副作用不回滚，但队列不会卡死。
    pub async fn with<F, Fut, T>(
        &self,
        file_path: &Path,
        function: F,
    ) -> Result<T, ToolOperationError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        // 注册（含 key 计算）全程持有注册锁：TS 的 `registrationQueue`。
        // 锁的范围覆盖 `canonicalize` 的 await，因此同一队列的所有注册是串行的。
        let (key, previous, tail) = {
            let _registration = self.registration.lock().await;
            let key = mutation_key_checked(file_path).await?;
            self.enqueue_key(key)
        };
        Ok(self.run(key, previous, tail, function).await)
    }

    /// [`Self::with`] 的别名（保留给既有的 `edit`/`write` 调用点，语义完全相同）。
    ///
    /// TS: `withFileMutationQueue`。
    pub async fn try_with<F, Fut, T>(
        &self,
        file_path: &Path,
        function: F,
    ) -> Result<T, ToolOperationError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        self.with(file_path, function).await
    }

    /// 把本次注册挂到 key 的队尾。调用方必须持有注册锁。
    fn enqueue_key(&self, key: PathBuf) -> (PathBuf, Option<Arc<QueueTail>>, Arc<QueueTail>) {
        let mut tails = lock_tails(&self.tails);
        let previous = tails.get(&key).cloned();
        let tail = Arc::new(QueueTail {
            previous: previous.clone(),
            release: Gate::new(),
        });
        tails.insert(key.clone(), Arc::clone(&tail));
        (key, previous, tail)
    }

    /// 等待前序注册、执行 `function` 并释放链路（TS 的 `await currentQueue; try { fn } finally { ... }`）。
    async fn run<F, Fut, T>(
        &self,
        key: PathBuf,
        previous: Option<Arc<QueueTail>>,
        tail: Arc<QueueTail>,
        function: F,
    ) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        // 守卫必须在任何 await 之前就位：future 被取消时也要释放链路（TS 的 finally）。
        // 若这里尚未执行到（例如在 `registration.lock()` 或 key 计算期间被取消），本次
        // 注册还没有进入 map，无需释放。
        let guard = ReleaseGuard {
            tail: Some(Arc::clone(&tail)),
            key: key.as_path(),
            tails: &self.tails,
        };
        if let Some(previous) = previous {
            previous.wait().await;
        }
        let result = function().await;
        // 正常路径显式释放；panic 时由 unwind 走 Drop，效果相同。
        drop(guard);
        result
    }
}

/// 对同一文件串行化变更操作。TS: `withFileMutationQueue(filePath, fn)`。
///
/// key 计算失败（`ENOENT`/`ENOTDIR` 以外的 `realpath` 错误，或含 NUL 的路径）时返回错误，
/// `function` 不会执行（TS 的 reject 语义）。
pub async fn with_file_mutation_queue<F, Fut, T>(
    file_path: impl AsRef<Path>,
    function: F,
) -> Result<T, ToolOperationError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    GLOBAL_QUEUE.with(file_path.as_ref(), function).await
}

/// 同 [`with_file_mutation_queue`]（保留别名，`edit`/`write` 使用）。
pub async fn try_with_file_mutation_queue<F, Fut, T>(
    file_path: impl AsRef<Path>,
    function: F,
) -> Result<T, ToolOperationError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    GLOBAL_QUEUE.with(file_path.as_ref(), function).await
}

/// `getMutationQueueKey` 的严格版本：`ENOENT`/`ENOTDIR` 以外的 `realpath` 错误原样返回。
///
/// 顺序与 Node 一致：先 `validatePath`（NUL 检查），再 `realpath`。`canonicalize` 会解析
/// 符号链接并要求整条路径存在；只有「路径尚不存在」的错误回退到 `resolve_absolute` 的词法
/// 结果，其它错误（`EACCES`/`ELOOP`/`ENAMETOOLONG` 等）转成带 Node 文案的
/// [`ToolOperationError`]（`realpath '<abs>'`）。
async fn mutation_key_checked(file_path: &Path) -> Result<PathBuf, ToolOperationError> {
    let resolved = resolve_absolute(file_path);
    // Node 的 `validatePath` 在 syscall 之前拒绝含 NUL 的路径（`ERR_INVALID_ARG_VALUE`）。
    if resolved.as_os_str().to_string_lossy().contains('\0') {
        return Err(crate::read::invalid_path_error(&resolved));
    }
    match tokio::fs::canonicalize(&resolved).await {
        Ok(canonical) => Ok(strip_verbatim_prefix(canonical)),
        Err(error) if is_missing_path_error(&error) => Ok(resolved),
        Err(error) => Err(ToolOperationError::from_io("realpath", &resolved, error)),
    }
}

/// TS 的 `isMissingPathError`：`ENOENT` / `ENOTDIR` 视为「路径尚不存在」。
fn is_missing_path_error(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    if let Some(errno) = error.raw_os_error() {
        return errno == libc::ENOENT || errno == libc::ENOTDIR;
    }
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// `path.resolve(filePath)`：按进程 cwd 词法解析（不展开 `~`、不做 Unicode 归一；保持纯词法、可预测的路径语义）。
///
/// 基准取 `std::env::current_dir()`，取不到时用 `.`；解析本身只做词法层面的
/// `path.resolve`（`kiana_core::paths::resolve_path`），不会检查目标路径是否存在。
fn resolve_absolute(file_path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd = cwd.to_string_lossy().into_owned();
    let options = kiana_core::paths::PathInputOptions {
        expand_tilde: false,
        ..kiana_core::paths::PathInputOptions::default()
    };
    let input = file_path.to_string_lossy();
    // TS 的 `node:path.resolve` 把 `file://…` 当普通相对路径；`kiana_core::paths::resolve_path`
    // 的 `normalize_path` 会把它改写成 `fileURLToPath` 结果。加 `./` 前缀让输入保持相对，
    // 于是 `node_resolve(cwd, "./file:///x")` 与 `path.resolve(cwd, "file:///x")` 一致。
    let input = if input.starts_with("file://") {
        format!("./{input}")
    } else {
        input.into_owned()
    };
    PathBuf::from(kiana_core::paths::resolve_path(&input, &cwd, &options))
}

/// Windows 的 `canonicalize` 会加 `\\?\` 前缀；剥掉它，让所有 key 形态一致。
///
/// 只影响 key 的文本形态（`\\?\UNC\server\share` 还原成 `\\server\share`），不改变指向的
/// 文件；非 Windows 平台原样返回。
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
        return path;
    }
    #[cfg(not(windows))]
    {
        path
    }
}
