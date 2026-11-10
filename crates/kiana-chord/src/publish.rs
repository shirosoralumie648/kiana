//! `publish.ts`：**进程级** FIFO 发布队列。
//!
//! 所有观察者通知（state 事件、shape 推送、注册表表更换、listener 调用）都经过这一个队列。
//!
//! ## 问题与解法（TS 注释逐字）
//!
//! 问题：在写操作内部同步通知观察者，会让重入的写操作**越过**正在被发布的写操作。
//! 例：订阅者 A 看到 n = 1 后写 n = 2，订阅者 B 于是先看到 2 再看到 1。
//! 解法：一次发布先接纳（adopt）它产生的所有修订（cell 与 derivation），然后把通知入队。通知按入队
//! 顺序执行，因此每个观察者都按接纳顺序看到修订。`hold_publications` 把投递推迟到多步变更（reload
//! commit）完成之后。
//!
//! ## 语义要点
//!
//! - 回调**同步**执行（不在微任务里）：队列空闲时 `enqueue` 立即执行任务。
//! - `hold_publications` 期间只入队，结束后（`finally`）一次性按序 drain。
//! - drain 期间重入的 `enqueue` 直接排到队尾（不递归），由外层循环继续消费，因此同一 drain 循环内
//!   仍是 FIFO。
//! - 任务不得抛错（自行 report）。任务 panic 时 drain 中断，剩余任务留在队列里（与 TS 的 `finally`
//!   复位 `draining` 后异常继续向上一致）。
//! - 队列是**整个进程共享**的，不是每 Host 一个（spec §12：慢观察者会拖慢进程里其后所有通知）。
//!
//! ## 线程与执行上下文
//!
//! - 任务在**触发 drain 的线程**上同步执行：可能是调用 `enqueue` 的线程、最后一个
//!   `hold_publications` 守卫 drop 的线程，或已经在执行 drain 的另一个线程。队列不创建
//!   线程，也不依赖 async 运行时。
//! - 并发 `enqueue` 的先后由队列锁的获取顺序决定（每个 `push_back` 都在锁内完成），不保证与
//!   调用发生的墙钟顺序一致。
//! - `HELD` 是**进程级**计数而不是线程局部：一个线程处在 `hold_publications` 作用域内时，
//!   其它线程的 `enqueue` 通常只入队不投递（`drain` 入口看到 `HELD > 0` 直接返回）。TS 的
//!   模块状态在单个 JS 线程内共享，Rust 的 `static` 却可被所有线程看到，这是逐字移植后
//!   多出来的可见性。
//! - `hold_publications` **不是跨线程屏障**：`drain()` 只在入口读一次 `HELD`（而且在拿队列锁
//!   之前），循环体内不再检查。已经在运行的 drain 会取走 hold 期间入队的任务；新的 drain 也
//!   可能在“读到 `HELD == 0`”与“hold 递增”之间的窗口里启动。需要“期间绝不投递”的强保证时，
//!   调用方必须自己串行化（或只在单线程上使用这个队列）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};

/// 一次通知（TS: `() => void`）。任务必须 `Send`：队列是进程级的。
pub type PublishTask = Box<dyn FnOnce() + Send + 'static>;

struct Queue {
    /// TS: `const queue: (() => void)[]`。
    tasks: VecDeque<PublishTask>,
    /// TS: `let draining = false`。放在锁内，避免两次并发 drain 同时消费队列。
    draining: bool,
}

static QUEUE: LazyLock<Mutex<Queue>> = LazyLock::new(|| {
    Mutex::new(Queue {
        tasks: VecDeque::new(),
        draining: false,
    })
});

/// TS: `let held = 0`（`hold_publications` 的嵌套深度）。
static HELD: AtomicUsize = AtomicUsize::new(0);

/// 任务 panic 后仍要复位 `draining`（TS 的 `finally`），因此用守卫。
struct DrainGuard;

impl Drop for DrainGuard {
    fn drop(&mut self) {
        queue().draining = false;
    }
}

/// `hold_publications` 的深度守卫：离开作用域时 `held--`，再 `drain()`（TS 的 `finally`）。
struct HoldGuard;

impl Drop for HoldGuard {
    fn drop(&mut self) {
        HELD.fetch_sub(1, Ordering::SeqCst);
        drain();
    }
}

fn queue() -> MutexGuard<'static, Queue> {
    // 任务在锁外执行，因此这里不会因为任务 panic 而中毒；唯一可能的中毒源是调用 `enqueue`
    // 的线程在 `push_back` 期间 panic（只有一次分配）。若仍中毒，恢复内部状态而不是让每次通知
    // 都 panic。
    QUEUE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 在 `task` 之前入队的所有任务之后运行它。
///
/// 任务不得抛错（报告错误而不是 panic）。队列空闲时**同步**执行 `task`。
///
/// TS: `enqueue(task)`。
pub fn enqueue(task: impl FnOnce() + Send + 'static) {
    queue().tasks.push_back(Box::new(task));
    drain();
}

/// `f` 在作用域内时，新启动的 drain 通常在入口看到 `HELD > 0` 就直接返回；`f` 返回后按序
/// 投递它入队的一切。已在其它线程运行的 drain 不受此限制（见下）。
///
/// TS: `holdPublications(fn)`。嵌套是允许的（`held` 是计数）。守卫在 `f` 返回或 panic 展开时
/// 递减计数并尝试 drain，`f` 的返回值原样传出。计数是进程级的：作用域内其它线程的 `enqueue`
/// 通常只入队；若已有 drain 在运行，它仍可能在 hold 期间取走并投递（见模块文档的跨线程说明）。
pub fn hold_publications<T>(f: impl FnOnce() -> T) -> T {
    HELD.fetch_add(1, Ordering::SeqCst);
    let _guard = HoldGuard;
    f()
}

/// `drain()`：`draining || held > 0` 时直接返回；否则依次执行队列里的任务。
///
/// 重入的 `enqueue` 在 `draining` 期间只入队，由本循环继续消费。
fn drain() {
    if HELD.load(Ordering::SeqCst) > 0 {
        return;
    }
    {
        let mut state = queue();
        if state.draining {
            return;
        }
        state.draining = true;
    }
    let _guard = DrainGuard;
    loop {
        // 锁只在取任务时持有：任务本身可能重入 `enqueue`（它会拿锁）。
        let task = {
            let mut state = queue();
            state.tasks.pop_front()
        };
        match task {
            // TS 里任务抛错会离开 `try`，`finally` 复位 `draining` 后继续向上抛；剩余任务留在队列
            // 里，由下一次 `enqueue`/`hold_publications` 继续消费。Rust 的栈展开会 drop
            // `DrainGuard`，顺序相同。
            Some(task) => task(),
            None => break,
        }
    }
}

/// 队列中等待的任务数（仅测试与诊断用；TS 没有对应 API）。
pub fn pending_publications() -> usize {
    queue().tasks.len()
}

/// 队列是否正在投递（仅测试与诊断用）。
pub fn is_draining() -> bool {
    queue().draining
}

/// 当前 `hold_publications` 嵌套深度（仅测试与诊断用）。
pub fn hold_depth() -> usize {
    HELD.load(Ordering::SeqCst)
}

/// 队列空闲且没有 `hold_publications` 在作用域里（仅测试用）。
pub fn idle() -> bool {
    let state = queue();
    !state.draining && state.tasks.is_empty() && HELD.load(Ordering::SeqCst) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn enqueue_runs_synchronously_when_idle() {
        let _serial = serial();
        let seen = Arc::new(AtomicUsize::new(0));
        let s = Arc::clone(&seen);
        enqueue(move || {
            s.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        assert!(idle());
    }

    #[test]
    fn hold_defers_then_flushes_in_order() {
        let _serial = serial();
        let order = Arc::new(Mutex::new(Vec::new()));
        let o = Arc::clone(&order);
        hold_publications(|| {
            for i in 0..3 {
                let o = Arc::clone(&o);
                enqueue(move || o.lock().unwrap().push(i));
            }
            assert!(order.lock().unwrap().is_empty());
        });
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn reentrant_enqueue_goes_to_the_tail() {
        let _serial = serial();
        let order = Arc::new(Mutex::new(Vec::new()));
        let o = Arc::clone(&order);
        let inner = Arc::clone(&order);
        enqueue(move || {
            o.lock().unwrap().push(1);
            // 重入：排到队尾，由外层 drain 循环继续消费（不递归）。
            enqueue(move || inner.lock().unwrap().push(3));
            // 同一任务里的后续工作仍先于重入的任务执行。
            o.lock().unwrap().push(2);
        });
        assert_eq!(*order.lock().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn nested_hold_keeps_tasks_queued() {
        let _serial = serial();
        let order = Arc::new(Mutex::new(Vec::new()));
        let o = Arc::clone(&order);
        hold_publications(|| {
            hold_publications(|| {
                let o = Arc::clone(&o);
                enqueue(move || o.lock().unwrap().push(1));
            });
            assert!(
                order.lock().unwrap().is_empty(),
                "inner hold released, outer still holds"
            );
        });
        assert_eq!(*order.lock().unwrap(), vec![1]);
    }
}
