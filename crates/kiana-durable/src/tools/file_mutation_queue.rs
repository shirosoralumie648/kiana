//! `tools/file-mutation-queue.ts`：同一文件系统上同一规范路径的 `edit`/`write` 串行化。
//!
//! TS 用一张进程级 `Map<string, Promise<void>>` 链表；Rust 用一张 key 到
//! `tokio::sync::Mutex` 的表。`tokio` 的互斥锁是公平（FIFO）的，因此并发调用按入队顺序执行，
//! 与 TS 的“槽位在 await 之前占据”一致。表项用 `Weak` 保存，最后一个持有者退出后自动清理。
//!
//! # 为什么需要它
//! `edit` 是"读 → 改 → 写"三步。两个并发调用若交错执行，后写的结果会覆盖先写的修改。
//! 对同一文件系统上同一规范路径的变更必须串行；不同文件、不同文件系统则不应互相等待。
//!
//! # key 与路径规范化
//! key 是 `${env.id()}\0${canonical(absolute_path)}`。`canonical` 有三种结果：
//! 1. `canonical_path` 成功：使用它的返回值。符号链接会被解析到真实路径，因此通过链接与
//!    通过真实路径的编辑共用同一个 key（`tests/tools.rs` 的
//!    `serializes_concurrent_edits_through_canonical_and_symlink_paths` 覆盖）。
//! 2. `NotSupported`：退化为绝对路径。
//! 3. `NotFound`：文件尚不存在（典型是 `write` 新建文件）。把父目录递归规范化后与文件名
//!    拼接，这样"在符号链接目录里新建文件"与之后对该文件的编辑共用一个 key。
//!    其他错误原样返回，调用方的闭包不会执行。
//!
//! # 加锁与顺序
//! 1. 在表锁内 `Weak::upgrade` 取现有互斥量，取不到就插入新的 `Arc<tokio::sync::Mutex>`。
//!    这一步不发生 `await`，所以槽位在等待前已经占据。
//! 2. `lock().await` 取得该 key 的令牌；`tokio` 互斥量按请求顺序授予。
//! 3. 执行闭包，然后释放令牌并清理表项。
//!
//! # 表项清理与取消
//! 表只存 `Weak`。正常返回时，在表锁内检查该 `Weak::strong_count() == 0` 才删除；如果
//! 新调用先拿到表锁并把表项换成自己的 cell，旧清理看到的强引用不为 0，不会误删。
//! 若返回的 future 在持锁期间被丢弃，收尾代码不执行：丢弃时若没有其他强引用（例如没有
//! 同 key 的调用正阻塞在 `cell.lock().await`），表项成为死 `Weak`，下一次调用会替换它；
//! 若仍有等待者（每个等待调用都持有自己的 `Arc` 强引用），表项继续有效，由最后一个正常
//! 返回的调用按 `strong_count() == 0` 清理。
//!
//! # 不提供的保证
//! - 只在本进程内生效：其他进程的 `edit`/`write`、以及 `bash` 里执行的命令都不经过这张表。
//! - 不检查上下文是否已取消；取消由调用方的闭包自己检查（`edit`/`write` 在读写前后检查）。
//! - 不保证跨文件系统的全局互斥：key 里包含 `env.id()`，不同文件系统互不等待。

use std::collections::HashMap;
use std::future::Future;
use std::sync::{LazyLock, Mutex, Weak};

use kiana_chord::context::Context;
use tokio::sync::Mutex as AsyncMutex;

use crate::env::{ExecutionEnv, FileErrorCode};
use crate::types::BoxError;

/// 每个 key 的互斥量；`Weak` 让空槽不留在表里。
static QUEUES: LazyLock<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// 表锁只在短小的 HashMap 操作期间持有。中毒（持锁线程 panic）时取回内部数据继续：表只是
// `Weak` 缓存，没有需要靠 panic 保护的不变量。
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `mutationKey`：`` `${env.id}\0${canonical(abs)}` ``。
///
/// 用 NUL 分隔 id 与路径，避免两个字符串直接拼接产生歧义（TS 用同样的分隔符）。
/// `absolute_path` 或 `canonical` 失败时返回错误，调用方不会进入队列。
async fn mutation_key(
    env: &dyn ExecutionEnv,
    path: &str,
    context: &dyn Context,
) -> Result<String, BoxError> {
    let absolute_path = env.absolute_path(path, context).await?;
    let canonical = canonical(env, &absolute_path, context).await?;
    Ok(format!("{}\0{}", env.id(), canonical))
}

/// 规范路径；对尚不存在的文件，用规范化的父目录拼接文件名，这样通过符号链接目录创建的文件
/// 与之后的变更共用一个 key。
async fn canonical(
    env: &dyn ExecutionEnv,
    absolute_path: &str,
    context: &dyn Context,
) -> Result<String, BoxError> {
    match env.canonical_path(absolute_path, context).await {
        Ok(value) => Ok(value),
        // 环境不提供规范化：只能退回绝对路径，同一文件的不同拼写可能得到不同 key。
        Err(error) if error.code == FileErrorCode::NotSupported => Ok(absolute_path.to_owned()),
        // 除 NotFound 之外的错误（权限等）向上传播，不进入队列。
        Err(error) if error.code != FileErrorCode::NotFound => Err(Box::new(error)),
        // 文件系统会拆分路径，因此名字里可能含有在别处是分隔符的字符。
        Err(_) => {
            // 父目录 = join(路径, "..")；若环境返回的父目录等于自身或不是前缀，无法安全
            // 拆分，直接退回绝对路径。这个守卫也让递归在根目录处停止。
            let parent = env
                .join_path(&[absolute_path.to_owned(), "..".to_owned()], context)
                .await?;
            if parent == absolute_path || !absolute_path.starts_with(&parent) {
                return Ok(absolute_path.to_owned());
            }
            // 取父目录之后的部分作为文件名。父目录以 '/' 或 '\\' 结尾时 rest 已经不含
            // 分隔符；否则去掉一个前导分隔符。只去掉一个，与 TS 的 regex 语义一致。
            let rest = &absolute_path[parent.len()..];
            let name = if parent.ends_with('/') || parent.ends_with('\\') {
                rest
            } else {
                rest.strip_prefix('/')
                    .or_else(|| rest.strip_prefix('\\'))
                    .unwrap_or(rest)
            };
            // 父目录可能是符号链接目录或本身也不存在，递归得到它的规范形式。
            let canonical_parent = canonical_boxed(env, &parent, context).await?;
            Ok(env
                .join_path(&[canonical_parent, name.to_owned()], context)
                .await?)
        }
    }
}

// 递归 `async fn` 的 future 大小无法静态确定，用 `Box` 打断类型递归。
fn canonical_boxed<'a>(
    env: &'a dyn ExecutionEnv,
    absolute_path: &'a str,
    context: &'a dyn Context,
) -> std::pin::Pin<Box<dyn Future<Output = Result<String, BoxError>> + Send + 'a>> {
    Box::pin(canonical(env, absolute_path, context))
}

/// 在本进程内串行化同一文件系统 id 与规范路径的 `edit`/`write`：哪个环境对象发的调用都一样。
/// 其他文件、其他文件系统从不等待。并发调用按 key 解析的顺序执行。不是对 `bash` 或其他进程的锁。
///
/// # 参数
/// - `env`/`path`：用于计算 key；`path` 会先 `absolute_path`，再规范化。
/// - `function`：拿到该 key 的锁后执行一次（`FnOnce`），锁覆盖它的整个 future。
/// - `context`：只用于路径解析时的取消检查（透传给环境）。
///
/// # 错误
/// key 解析失败时直接返回，`function` 不会执行。`function` 返回 `Err` 时锁照常释放，
/// 错误原样返回（`result` 先保存、清理后再返回）。
///
/// # 取消
/// 本函数不主动检查 `context`：调用方闭包里的取消检查负责在读写前后退出。若整个 future
/// 被丢弃，`tokio` 的锁守卫随局部变量一起释放，但函数末尾的表项清理不会执行（见模块文档）。
pub async fn with_file_mutation_queue<T, F, Fut>(
    env: &dyn ExecutionEnv,
    path: &str,
    function: F,
    context: &dyn Context,
) -> Result<T, BoxError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, BoxError>>,
{
    // 先解析 key：失败就直接返回，不占槽位，也不执行闭包。
    let key = mutation_key(env, path, context).await?;
    // 不 await 地占据槽位，别的调用无法在中间拿走它。
    let cell = {
        let mut queues = lock(&QUEUES);
        let existing = queues.get(&key).and_then(Weak::upgrade);
        match existing {
            Some(cell) => cell,
            None => {
                // 新 cell 的 Weak 立刻入表；本调用持有强引用，所以并发调用能 upgrade 到
                // 同一个锁，而不会各建一个。
                let cell = std::sync::Arc::new(AsyncMutex::new(()));
                queues.insert(key.clone(), std::sync::Arc::downgrade(&cell));
                cell
            }
        }
    };
    // 从这一行到闭包结束，同 key 的其他调用都在这里按顺序排队。
    let guard = cell.lock().await;
    let result = function().await;
    drop(guard);
    drop(cell);
    {
        // 只在没有其他强引用时删除表项；若另一个调用已经接手，保留它。
        let mut queues = lock(&QUEUES);
        if queues
            .get(&key)
            .is_some_and(|weak| weak.strong_count() == 0)
        {
            queues.remove(&key);
        }
    }
    result
}
