//! `run_with_concurrency`（TS worker 池的失败语义）。
//!
//! 被 `super::mod`（`DefaultPackageManager` 的 update / check_for_available_updates 流程）
//! 调用：`run_with_concurrency` 并发跑多个包的更新检查或 git 更新，`join_first_error`
//! 等 user / project / git 三个并发任务里最先失败的那个（见 `mod.rs` 的
//! `UPDATE_CHECK_CONCURRENCY` / `GIT_UPDATE_CONCURRENCY`）。
//!
//! TS: `package-manager.ts#runWithConcurrency`。JS 的 worker 池在线程安全的意义上是
//! 单线程的（`nextIndex += 1` 在 await 点之间安全），任一 task 抛错时该 worker 的循环
//! 退出、`Promise.all` 立即 reject；**其余 worker 仍会继续领取并跑完剩余任务**。
//! Rust 侧同样记录第一个错误，但继续轮询所有 worker 直到结束，再返回该错误——避免
//! 提前丢弃 future 把 git/npm 更新留在半完成状态。
//!
//! # 执行流程
//! 1. 为每个任务建立一个 `Option<F>` 槽位，并共享一个 `next_index`；
//! 2. `worker_count = limit.max(1).min(tasks.len())`，即 `limit == 0` 按 1 处理；
//! 3. worker 短暂持锁领取下一个下标并 `take` 掉槽位（同一任务不会被领取两次），
//!    释放锁后才 `task().await`；
//! 4. 成功结果写回同下标的 `results` 槽位；任一任务返回 `Err` 时该 worker 退出，
//!    外层 `poll_fn` 记录第一个错误并继续轮询其它 worker；
//! 5. 全部 worker 结束后，若有错误则返回第一个，否则按输入下标收集成功结果。
//!
//! # 并发模型
//! 所有 worker 都是同一个 `poll_fn` 里轮询的 future，运行在调用方当前任务上，不创建线程、
//! 也不 `tokio::spawn`。因此这里的“并发”是协作式的：某个任务若长时间不让出（同步阻塞或
//! 纯 CPU 工作），其它 worker 都不会被轮询。两把 `Mutex` 都只在很短的作用域内持有
//! （领取任务 / 写结果），跨 `await` 不持锁，因此任务执行不会互相阻塞。
//!
//! # 失败模式
//! 返回第一个错误**之前**会等待其余 worker 完成（与 `Promise.all` 让已启动任务继续跑完的
//! 语义一致）；错误 worker 不再领取新任务，其它 worker 仍可继续领取。

use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use super::PackageManagerError;

/// 每个任务的输出槽位（`Option` 表示尚未完成）。
type Results<T> = Arc<Mutex<Vec<Option<Result<T, PackageManagerError>>>>>;

/// worker future（借用 task 闭包，不要求 `'static`）。
type WorkerFuture<'a> = Pin<Box<dyn Future<Output = Result<(), PackageManagerError>> + Send + 'a>>;

/// 加锁并忽略锁中毒：锁内只有索引自增与 `Option` 槽位读写，没有需要靠 panic 传播
/// 保护的不变量，因此某个 worker 的 panic 不应让后续 worker 也失败。
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `runWithConcurrency(tasks, limit)`。
///
/// 任务按索引顺序启动；`limit` 至少为 1。任一任务失败时立即返回该错误，且不会再启动
/// 队列里的新任务；结果按输入顺序返回。
pub(crate) async fn run_with_concurrency<T, F, Fut>(
    tasks: Vec<F>,
    limit: usize,
) -> Result<Vec<T>, PackageManagerError>
where
    T: Send,
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = Result<T, PackageManagerError>> + Send,
{
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    let task_count = tasks.len();
    let worker_count = limit.max(1).min(task_count);

    let slots: Vec<Option<F>> = tasks.into_iter().map(Some).collect();
    let tasks = Arc::new(Mutex::new((slots, 0usize)));
    let results: Results<T> = Arc::new(Mutex::new((0..task_count).map(|_| None).collect()));

    let mut workers: Vec<WorkerFuture<'_>> = Vec::new();
    for _ in 0..worker_count {
        let tasks = Arc::clone(&tasks);
        let results = Arc::clone(&results);
        workers.push(Box::pin(async move {
            loop {
                // 锁内只做下标自增与 `take`；离开这个块后锁即释放，任务执行期间不持锁。
                let next = {
                    let mut guard = lock(&tasks);
                    let (slots, next_index) = &mut *guard;
                    if *next_index >= slots.len() {
                        break;
                    }
                    let index = *next_index;
                    *next_index += 1;
                    (index, slots[index].take())
                };
                let (index, task) = next;
                let Some(task) = task else {
                    continue;
                };
                let outcome = task().await;
                if let Err(error) = outcome {
                    // 失败不写结果槽位：外层会立刻返回这个错误，调用方读不到半成品列表。
                    return Err(error);
                }
                lock(&results)[index] = Some(outcome);
            }
            Ok(())
        }));
    }

    // 单线程轮询所有 worker：任一 worker 失败时记录第一个错误并让该 worker 退出，
    // 但**继续轮询其它 worker 直到全部结束**（TS 的 `Promise.all` 虽然立即 reject，
    // 其余 Promise/worker 仍会跑完；提前丢弃 future 会把 git/npm 更新留在半完成状态）。
    let mut completed = vec![false; workers.len()];
    let mut first_error: Option<PackageManagerError> = None;
    poll_fn(|context| {
        let mut all_done = true;
        for (index, worker) in workers.iter_mut().enumerate() {
            if completed[index] {
                continue;
            }
            match worker.as_mut().poll(context) {
                Poll::Ready(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    completed[index] = true;
                }
                Poll::Ready(Ok(())) => completed[index] = true,
                Poll::Pending => all_done = false,
            }
        }
        if all_done {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    })
    .await?;
    if let Some(error) = first_error {
        return Err(error);
    }

    // 走到这里说明所有 worker 都正常结束：每个任务槽位都已被领取且成功返回，因此
    // `results` 里全部是 `Some(Ok(..))`；下面收集循环里的 `Err` / `None` 分支在该前提下
    // 不可达，仅为类型完整而保留。
    let outputs = {
        let mut guard = lock(&results);
        std::mem::take(&mut *guard)
    };
    let mut values = Vec::with_capacity(task_count);
    for output in outputs {
        match output {
            Some(Ok(value)) => values.push(value),
            Some(Err(error)) => return Err(error),
            None => {}
        }
    }
    Ok(values)
}

/// 并发等待最多三个 future，返回**最先完成**的错误（等价 TS 的 `Promise.all`）。
///
/// # 顺序与取消
/// 每一轮按 `first` / `second` / `third` 的顺序 poll；若同一轮里有多个任务同时就绪且
/// 都是错误，参数靠前的那个胜出（与它们在墙钟时间上的先后无关）。记录第一个错误后
/// 继续轮询直到三个 future 全部结束，再返回该错误——与 TS 中 `Promise.all` 立即 reject
/// 但其余 Promise 继续运行的行为一致。
///
/// # 数量固定
/// 只接受三个 future；调用方需要等待其它数量的任务时用 `run_with_concurrency` 或另行组合。
pub(crate) async fn join_first_error<A, B, C>(
    first: A,
    second: B,
    third: C,
) -> Result<(), PackageManagerError>
where
    A: Future<Output = Result<(), PackageManagerError>>,
    B: Future<Output = Result<(), PackageManagerError>>,
    C: Future<Output = Result<(), PackageManagerError>>,
{
    let mut first = Box::pin(first);
    let mut second = Box::pin(second);
    let mut third = Box::pin(third);
    let mut first_done = false;
    let mut second_done = false;
    let mut third_done = false;
    let mut first_error: Option<PackageManagerError> = None;
    poll_fn(|context| {
        let mut all_done = true;
        if !first_done {
            match first.as_mut().poll(context) {
                Poll::Ready(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    first_done = true;
                }
                Poll::Ready(Ok(())) => first_done = true,
                Poll::Pending => all_done = false,
            }
        }
        if !second_done {
            match second.as_mut().poll(context) {
                Poll::Ready(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    second_done = true;
                }
                Poll::Ready(Ok(())) => second_done = true,
                Poll::Pending => all_done = false,
            }
        }
        if !third_done {
            match third.as_mut().poll(context) {
                Poll::Ready(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    third_done = true;
                }
                Poll::Ready(Ok(())) => third_done = true,
                Poll::Pending => all_done = false,
            }
        }
        if all_done {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    })
    .await?;
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn runs_all_tasks_and_preserves_order() {
        let tasks: Vec<_> = (0..10)
            .map(|index| move || async move { Ok(index * 2) })
            .collect();
        let results = run_with_concurrency(tasks, 3).await.expect("results");
        assert_eq!(results, (0..10).map(|index| index * 2).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn stops_starting_tasks_after_the_first_error() {
        let started = Arc::new(AtomicUsize::new(0));
        type Task = Box<
            dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<u32, PackageManagerError>> + Send>>
                + Send,
        >;
        let mut tasks: Vec<Task> = Vec::new();
        for index in 0..8u32 {
            let started = Arc::clone(&started);
            tasks.push(Box::new(move || {
                Box::pin(async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    if index == 2 {
                        return Err(PackageManagerError::new("boom"));
                    }
                    Ok(index)
                })
            }));
        }
        // 单 worker：任务 0、1 成功，任务 2 失败后不再领取 3..8。
        let error = run_with_concurrency(tasks, 1).await.expect_err("error");
        assert_eq!(error.message(), "boom");
        assert_eq!(started.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn waits_for_other_workers_after_the_first_error() {
        type Task = Box<
            dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<u32, PackageManagerError>> + Send>>
                + Send,
        >;
        let completed = Arc::new(AtomicUsize::new(0));
        let completed_b = Arc::clone(&completed);
        let tasks: Vec<Task> = vec![
            Box::new(|| Box::pin(async { Err(PackageManagerError::new("boom")) })),
            Box::new(move || {
                Box::pin(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                    completed_b.fetch_add(1, Ordering::SeqCst);
                    Ok(2)
                })
            }),
        ];
        let started = std::time::Instant::now();
        let error = run_with_concurrency(tasks, 2).await.expect_err("error");
        assert_eq!(error.message(), "boom");
        // 旧实现会在第一个错误时丢弃第二个 worker；新实现等它跑完。
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        assert!(started.elapsed() >= std::time::Duration::from_millis(25));
    }

    #[tokio::test]
    async fn join_first_error_prefers_the_first_failure_in_time() {
        let slow = async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            Err(PackageManagerError::new("slow"))
        };
        let fast = async {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            Err(PackageManagerError::new("fast"))
        };
        let ok = async { Ok(()) };
        let error = join_first_error(slow, fast, ok).await.expect_err("error");
        assert_eq!(error.message(), "fast");
    }
}
