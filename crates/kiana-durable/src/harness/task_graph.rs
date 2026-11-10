//! `harness/task-graph.ts`：Session 全部活动任务的图视图。
//!
//! TS: `packages/durable/src/harness/task-graph.ts`。图在 Session 的变更线上由第一个观察者构建，
//! 随之后的每一次已存提交推进（只改文档的内存提交不出现在图里），随最后一个观察者消失。
//!
//! # 为什么需要它
//! `TaskRecord` 是持久化事实，但宿主需要的是一份“现在有哪些活任务、它们卡在哪里”的只读
//! 快照。图不写回任何记录，也没有自己的持久状态：第一个观察者出现时从 `Storage` 扫描构建，
//! 之后只跟随 Session 的已存储提交增量推进，最后一个观察者离开时丢弃。否则每个观察者都要
//! 各扫一遍 `Storage`，共享挂载让一次扫描的结果被所有观察者复用。
//!
//! # 结构
//! - [`TaskGraphView`]：挂在 Harness 上的单例，注册提交监听、blocked 订阅与关闭监听。
//! - `Mount`：所有观察者共享的当前值 [`TaskGraph`]、每个活任务的 `BlockedSubject` 快照，
//!   以及观察者表；第一个观察者创建，最后一个观察者注销时丢弃。
//! - [`TaskGraphBlocking`]：blocked 判定完全委托给调度器，本模块只缓存结果。
//! - [`TaskGraphNode`]：由 `node_of` 从 `TaskRecord` 与它拥有的会话列表投影而来。
//!
//! # 一次推进
//! 1. 提交监听只处理已存储提交；`memory` 提交只改文档，直接返回。
//! 2. 先遍历任务变化：终态记录删除节点，其余重新计算 `state`/`blocked`，节点真的变化时
//!    才产出 `Set`。
//! 3. 再遍历会话变化：`owner` 非空的会话并入 owner 节点的 `conversations`，追加后重新排序。
//! 4. `publish` 把 `Op` 应用到序列化的图、反序列化回 [`TaskGraph`]，再逐个通知观察者。
//! 5. registry 变化（`subscribe_blocked`）不经过提交，单独在变更线上重新派生 `blocked`。
//!
//! # 约束与注意事项
//! - 键是任务 ID 的十进制字符串；构建时按数值 `TaskId` 升序插入，之后增量加入的节点由
//!   `apply_immutable` 追加到末尾，所以 `IndexMap` 的迭代顺序不是字典序（`10` 排在 `2`
//!   之后）。
//! - `conversations` 必须保持 ID 升序：同一提交内的变化顺序未定义（见 `CommitPublication`
//!   的 `changes` 注释），所以每次合并后重新排序。
//! - 任务必须先于会话处理，否则同一提交里“创建会话 + 它的 owner 任务”会找不到 owner 节点。
//! - `blocked` 回调在提交推进与 blocked 刷新时于挂载锁内同步调用，必须快速返回且不得
//!   重入本模块的锁。
//! - 提交监听里的推进失败会让 Session 失败（`fail_with_context`）；blocked 刷新路径则吞掉
//!   发布错误——两者对错误的处理不同，改动时要分别考虑。
//! - 句柄应显式 `stop()`：只丢弃返回的 `Arc` 不会触发 detach，观察者会一直留在挂载里直到关闭。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use indexmap::IndexMap;
use kiana_chord::context::{BACKGROUND_CONTEXT, Context, ContextRef};
use kiana_chord::delta::Op;
use kiana_chord::delta::index::apply_immutable;

use crate::harness::scheduler::blocked_subject;
use crate::harness::types::{BlockedReason, BlockedSubject};
use crate::harness::util::{closed_error, scan_all};
use crate::harness::view::JsonWatch;
use crate::session::SessionImpl;
use crate::session::session::{ContextSource, ObservedCommit, contained};
use crate::types::{
    BoxError, CommitChange, ConversationId, ConversationQuery, Cursor, JsonValue, Storage,
    TableCommitChange, TaskOutcome, TaskOutcomeStatus, TaskQuery, TaskRecord, TaskStatus,
};

/// 仍是活动的任务状态（TS: `LIVE_STATUSES`）。
///
/// 终态不在其中：终态记录在 `advance` 里被从图中删除。`build` 按这五种状态分别扫描
/// `Storage`，把结果合并后按数值 `TaskId` 升序插入。
const LIVE_STATUSES: [TaskStatus; 5] = [
    TaskStatus::Pending,
    TaskStatus::Running,
    TaskStatus::Waiting,
    TaskStatus::Sleeping,
    TaskStatus::Completing,
];

/// 一轮扫描的页大小。
///
/// 只决定每次 `Storage::scan_tasks`/`scan_conversations` 取多少条；`scan_all` 会沿
/// `Page::next` 一直取到结束，所以它不影响结果的完整性。
const SCAN_PAGE_SIZE: usize = 256;

// TS 的四个类型定义在 `task-graph.ts`；Rust 侧为了打破模块环放在 `harness/types.rs`，这里按
// TS 的模块归属重新导出。
pub use crate::harness::types::{TaskGraph, TaskGraphNode, TaskGraphState, TaskGraphWatch};

/// 图向调度器询问的东西：一个任务的 blocked 原因，以及原因可能在无提交时变化的时机。
///
/// 本模块不判断“为什么不能跑”，只把调度器的答复缓存进节点；判定逻辑在
/// `harness/scheduler.rs` 的 `blocked_reason` 里。两个回调都由 Harness 在构造时从
/// `TaskScheduler` 取得（见 `harness.rs` 的 `TaskGraphView::new` 调用）。
///
/// 约束：`blocked` 会在挂载锁（`MountState` 的 `Mutex`）内被同步调用（`advance` 与
/// `RefreshHandle::refresh`；`build` 在挂载建立前调用，不持锁），因此必须快速返回、
/// 不得阻塞，也不得再次进入本模块的挂载锁。
pub struct TaskGraphBlocking {
    /// 一个任务的 blocked 原因。
    ///
    /// 返回 `None` 表示这个任务在此调度器里没有被阻塞（或判定不适用）；`Some` 的每个取值
    /// 含义见 `harness/types.rs` 的 `BlockedReason`。
    pub blocked: Arc<dyn Fn(&BlockedSubject) -> Option<BlockedReason> + Send + Sync>,
    /// 注册一个 blocked 原因可能变化的监听器。
    pub subscribe_blocked:
        Arc<dyn Fn(Arc<dyn Fn() + Send + Sync>) -> crate::types::Unsubscribe + Send + Sync>,
}

impl Clone for TaskGraphBlocking {
    fn clone(&self) -> Self {
        TaskGraphBlocking {
            blocked: Arc::clone(&self.blocked),
            subscribe_blocked: Arc::clone(&self.subscribe_blocked),
        }
    }
}

/// 一个观察者：`JsonWatch` 持有初始修订，之后接收每一帧。
type Observer = Arc<JsonWatch<TaskGraph>>;

/// 一次挂载：当前修订、每个活动任务的 blocked 输入，以及观察者。
///
/// 只有第一个观察者到来时才构建；所有观察者共享同一个 `Mount`，因此提交被应用一次、
/// 各观察者拿到同一份修订。关闭时整个挂载被从槽里取走，观察者随后被结束。
struct Mount {
    state: Mutex<MountState>,
}

struct MountState {
    /// 最近一次成功发布后的图；新观察者拿到的初始值也来自这里。
    value: TaskGraph,
    /// 每个活动任务里被 `blockedReason` 读取的片段，让 registry 变化时无需负载即可重新派生。
    ///
    /// 键正常情况下与 `value.tasks` 相同；终态任务从这里移除，刷新时不会再为它们计算原因。
    records: IndexMap<String, BlockedSubject>,
    /// 观察者 id → watch。id 单调递增，`release` 用它在表里精确删除，而不是按引用比较。
    observers: IndexMap<u64, Observer>,
    /// 下一个观察者 id；先自增再使用，所以第一个观察者的 id 是 1。
    next_observer_id: u64,
}

/// 取挂载锁；互斥锁中毒时取出内部值继续用。
///
/// 中毒说明上一次持锁的线程在持锁期间 panic 了，此时状态可能只更新了一部分；继续使用是
/// 为了不让一次 panic 把之后所有 `watch` 永久变成新的 panic。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Harness 的任务图挂载（TS: `TaskGraphView`）。
///
/// 一个 Harness 只建一个；它本身不是观察者，只负责在 Session 上注册监听、按需构建挂载。
/// 三个 `_*_unsubscribe` 字段只保存注销函数、本模块从不调用它们：`Unsubscribe` 是
/// `Box<dyn FnOnce()>`，字段被丢弃时不会自动执行。提交/关闭监听随 Session 关闭清除，
/// blocked 监听随调度器释放；字段前的下划线表示它们只被持有、不被读取。
pub struct TaskGraphView {
    /// Session：注册提交/关闭监听，并在变更线上构建挂载。
    session: SessionImpl,
    /// 受保护的 Storage 句柄；只在 `build` 扫描任务与会话时使用。
    storage: Arc<dyn Storage>,
    /// 调度器提供的 blocked 判定与变更通知。
    blocking: TaskGraphBlocking,
    /// `None` 表示没有已注册的观察者；关闭时即使还有观察者也会被取走并结束。
    mount: Arc<Mutex<Option<Arc<Mount>>>>,
    /// 关闭监听运行后置真；之后的 `attach` 不再注册观察者。
    closed: Arc<AtomicBool>,
    /// 观察者 watch 的报告出口；Harness 传入的闭包会把它记成 `listener_error`。
    report: Arc<dyn Fn(BoxError) + Send + Sync>,
    /// 提交监听的注销函数（未调用，见类型说明）。
    _commit_unsubscribe: crate::types::Unsubscribe,
    /// blocked 监听的注销函数（未调用，见类型说明）。
    _blocked_unsubscribe: crate::types::Unsubscribe,
    /// 关闭监听的注销函数（未调用，见类型说明）。
    _close_unsubscribe: crate::types::Unsubscribe,
}

impl TaskGraphView {
    /// 新建并挂上 Session 的提交观察、blocked 订阅与关闭观察。
    ///
    /// # 流程
    /// 1. 注册提交监听：`memory` 提交直接跳过；有挂载时把本次发布交给 `advance`，失败则用
    ///    发布上下文调用 `Session::fail_with_context`（对应 TS 从内部监听器抛错）。
    /// 2. 注册 blocked 监听：任何“原因可能变了”的通知都触发一次挂载范围的重新派生。
    /// 3. 注册关闭监听：置 `closed`、取走挂载，并以 Session 的 `failure()` 结束每个观察者。
    ///
    /// # Errors
    /// Session 已失败或正在关闭时，`observe_commits`/`subscribe_close` 会拒绝注册并报错；
    /// `subscribe_blocked` 自身不会失败，失败只可能来自前两者。
    pub fn new(
        session: SessionImpl,
        storage: Arc<dyn Storage>,
        blocking: TaskGraphBlocking,
        report: Arc<dyn Fn(BoxError) + Send + Sync>,
    ) -> Result<Self, BoxError> {
        let mount: Arc<Mutex<Option<Arc<Mount>>>> = Arc::new(Mutex::new(None));
        let closed = Arc::new(AtomicBool::new(false));
        let listener_mount = Arc::clone(&mount);
        let listener_blocking = blocking.clone();
        let listener_report = Arc::clone(&report);
        let listener_session = session.clone();
        let listener: crate::session::session::InternalListener =
            Arc::new(move |publication, context| {
                // 只在内存里持有的提交只改文档，图不显示。
                if publication.kind() == "memory" {
                    return;
                }
                let current = listener_mount
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                let Some(current) = current else {
                    return;
                };
                // TS：`applyImmutable` 抛出时从内部监听器抛出，Session 以该错误失败。
                // 事件序是“先有已存储提交，再有图”：此处失败说明派生的 op 与图结构不再匹配，
                // 继续运行会让观察者看到与提交线脱节的图。
                if let Err(error) = advance(
                    &current,
                    publication,
                    context,
                    &listener_blocking,
                    &listener_report,
                ) {
                    listener_session.fail_with_context(error, &**context);
                }
            });
        let commit_unsubscribe = session.observe_commits(listener)?;

        let refresh_view = Arc::new(RefreshHandle {
            session: session.clone(),
            mount: Arc::clone(&mount),
            closed: Arc::clone(&closed),
            blocking: blocking.clone(),
            report: Arc::clone(&report),
        });
        let refresh = Arc::clone(&refresh_view);
        let blocked_unsubscribe = (blocking.subscribe_blocked)(Arc::new(move || {
            refresh.refresh();
        }));

        let close_mount = Arc::clone(&mount);
        let close_closed = Arc::clone(&closed);
        let close_session = session.clone();
        let close_report = Arc::clone(&report);
        let close_unsubscribe = session.subscribe_close(Arc::new(move || {
            close_closed.store(true, Ordering::SeqCst);
            let failure = close_session.failure();
            let current = close_mount
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            let Some(current) = current else {
                return;
            };
            let observers: Vec<Observer> = current
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .observers
                .values()
                .cloned()
                .collect();
            for observer in observers {
                let report = Arc::clone(&close_report);
                contained(|| observer.end(failure.clone()), move |error| report(error));
            }
        }))?;

        Ok(TaskGraphView {
            session,
            storage,
            blocking,
            mount,
            closed,
            report,
            _commit_unsubscribe: commit_unsubscribe,
            _blocked_unsubscribe: blocked_unsubscribe,
            _close_unsubscribe: close_unsubscribe,
        })
    }

    /// 图的串行 watch；取消 `context` 会停止它。
    ///
    /// 返回的句柄以当前修订为初始值：`value()` 在调用 `start()` 之前就能读到这份快照，
    /// 但监听器要等调用方 `start()` 之后才会收到帧（`start()` 之前的帧会折叠成一次投递）。
    /// `context` 带中止信号时，取消会让 watch 以 `Cancelled` 终止；若信号在 `attach` 返回时
    /// 已经中止，则直接返回中止错误，不留下观察者。
    ///
    /// 句柄应当显式 `stop()`：`TaskGraphWatch` 是 `Arc<dyn WatchHandle<TaskGraph>>`，只提供
    /// `value`/`start`/`stop`/`closed`，没有 `cancel()`；取消由创建时传入的 `context` 的中止
    /// 信号触发（见上文）。只丢弃返回的 `Arc` 不会触发 watch 的 detach，观察者会一直留在
    /// 挂载里直到 Harness 关闭。
    ///
    /// # Errors
    /// - Session 已失败或正在关闭：`read_on_line` 的可接纳性检查先返回 `SessionFailed` /
    ///   `SessionClosed`；若关闭发生在首次构建挂载期间，则由 `closed_error` 生成错误；
    /// - 上下文已中止：`abort_error`（`attach` 内检查一次，返回后 `watch` 再检查一次）；
    /// - 首次构建要扫描 Storage，扫描错误原样返回。
    pub async fn watch(&self, context: &dyn Context) -> Result<TaskGraphWatch, BoxError> {
        let report = Arc::clone(&self.report);
        let (observer, _detach) = self
            .attach(
                move |value, release| {
                    JsonWatch::new(
                        value,
                        Box::new(move || release()),
                        Some(report),
                        Arc::new(|graph: &TaskGraph| {
                            serde_json::to_value(graph).unwrap_or(serde_json::Value::Null)
                        }),
                    )
                },
                context,
            )
            .await?;
        if let Some(signal) = context.abort_signal()
            && signal.is_aborted()
        {
            observer.cancel();
            return Err(Box::new(kiana_chord::context::abort_error(&signal)));
        }
        if let Some(signal) = context.abort_signal() {
            observer.observe_cancellation(&signal);
        }
        Ok(Arc::clone(&observer) as TaskGraphWatch)
    }

    /// 在 Session 线上原子地注册一个从当前修订构造的观察者。
    ///
    /// 整段任务通过 `read_on_line` 运行，因此“构建挂载、创建 watch、插入观察者”不会与提交
    /// 监听交错。返回的 `release` 已经被 watch 持有（终止时执行一次），调用方不需要再调用。
    ///
    /// 顺序细节：先 `create` 出 watch（它捕获 `release`），再检查关闭与中止。检查失败时
    /// 观察者尚未插入 `observers`；若是本次新建的挂载，也尚未写入槽位。直接丢弃不会留下
    /// 残留状态。
    ///
    /// # Errors
    /// `read_on_line` 的可接纳性检查错误（`SessionFailed` / `SessionClosed`）、`build` 的
    /// Storage 扫描错误、构建期间观察到关闭时的 `closed_error`、或上下文的中止错误。
    async fn attach<F>(
        &self,
        create: F,
        context: &dyn Context,
    ) -> Result<(Observer, crate::harness::view::Detach), BoxError>
    where
        F: FnOnce(TaskGraph, crate::harness::view::Detach) -> Observer + Send,
    {
        let storage = Arc::clone(&self.storage);
        let mount_slot = Arc::clone(&self.mount);
        let closed = Arc::clone(&self.closed);
        let session = self.session.clone();
        let blocking = self.blocking.clone();
        let context_ref = context.context_ref();
        self.session
            .read_on_line(async move {
                // 复用已有挂载；没有才构建。整段在变更线上，构建期间不会有提交插进来。
                let existing = mount_slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                let mount = match existing {
                    Some(mount) => mount,
                    None => {
                        let built =
                            build(&session, storage.as_ref(), &blocking, &*context_ref).await?;
                        Arc::new(Mount {
                            state: Mutex::new(built),
                        })
                    }
                };
                let observer_id = {
                    // 先占 id 再创建：release 只用 id 做精确删除，不依赖 watch 的引用身份。
                    let mut state = lock(&mount.state);
                    state.next_observer_id += 1;
                    state.next_observer_id
                };
                let detach_mount = Arc::clone(&mount);
                let detach_slot = Arc::clone(&mount_slot);
                // watch 终止（stop/cancel/end）时执行：移出自己；最后一个观察者离开时把挂载
                // 从槽里清掉，但只清“同一个”挂载，避免误删之后新建的挂载。
                let release: crate::harness::view::Detach = Arc::new(move || {
                    let empty = {
                        let mut state = lock(&detach_mount.state);
                        state.observers.shift_remove(&observer_id);
                        state.observers.is_empty()
                    };
                    if empty {
                        let mut slot = detach_slot
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if slot
                            .as_ref()
                            .is_some_and(|mounted| Arc::ptr_eq(mounted, &detach_mount))
                        {
                            *slot = None;
                        }
                    }
                });
                let value = lock(&mount.state).value.clone();
                let observer = create(value, Arc::clone(&release));
                // 关闭或取消可能发生在构建挂载期间：此时不注册，返回对应错误。
                if closed.load(Ordering::SeqCst) {
                    return Err(closed_error(&session));
                }
                if let Some(signal) = context_ref.abort_signal()
                    && signal.is_aborted()
                {
                    return Err(Box::new(kiana_chord::context::abort_error(&signal)));
                }
                *mount_slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::clone(&mount));
                lock(&mount.state)
                    .observers
                    .insert(observer_id, Arc::clone(&observer));
                Ok((observer, release))
            })
            .await
    }
}

/// blocked 原因的重新派生句柄（`subscribeBlocked` 的监听器只持有它）。
///
/// 监听器是同步回调，不能直接 `await`；`refresh` 因此把工作 `spawn` 到当前 Tokio 运行时，
/// 再经 `read_on_line` 排到 Session 变更线执行。
struct RefreshHandle {
    session: SessionImpl,
    mount: Arc<Mutex<Option<Arc<Mount>>>>,
    closed: Arc<AtomicBool>,
    blocking: TaskGraphBlocking,
    report: Arc<dyn Fn(BoxError) + Send + Sync>,
}

impl RefreshHandle {
    /// 在 Session 线上重新派生每个节点的 blocked 原因，并发布变化了的那些。
    ///
    /// 流程：先做快速检查（已关闭 / 没有挂载就直接返回），再 spawn 一个任务，
    /// 在 `read_on_line` 里把 `records` 中每个 `BlockedSubject` 的当前答复与
    /// `value.tasks[key].blocked` 比较，只对变化生成 `Set`，对消失生成
    /// `Delete(["tasks", key, "blocked"])`。
    ///
    /// 不重新扫描 Storage：`records` 已保存 `blockedReason` 需要的那几项，所以 registry
    /// 变化时无需加载任务或会话。刷新使用 `BACKGROUND_CONTEXT`（没有关联的调用方上下文）。
    ///
    /// 两个降级行为：没有 Tokio 运行时时 `Handle::try_current` 失败，这次刷新被直接放弃；
    /// `publish` 的错误被吞掉（TS 的 `.catch(() => {})`），不会让 Session 失败——这与提交
    /// 监听器路径不同。
    fn refresh(self: &Arc<Self>) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        if self
            .mount
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_none()
        {
            return;
        }
        let handle = Arc::clone(self);
        let session = self.session.clone();
        let context: ContextRef = Arc::clone(&BACKGROUND_CONTEXT);
        // 拒绝只会在 Session 关闭或失败时发生。
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = session
                    .read_on_line(async move {
                        let Some(mount) = handle
                            .mount
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .clone()
                        else {
                            return Ok(());
                        };
                        let state = lock(&mount.state);
                        // 只为 blocked 字段生成 op；节点其余字段来自提交路径，刷新不碰。
                        let mut ops: Vec<Op> = Vec::new();
                        for (key, record) in &state.records {
                            let blocked = (handle.blocking.blocked)(record);
                            let current = state.value.tasks.get(key).and_then(|node| node.blocked);
                            if blocked == current {
                                continue;
                            }
                            let path = vec![
                                kiana_chord::delta::Seg::Key("tasks".to_owned()),
                                kiana_chord::delta::Seg::Key(key.clone()),
                                kiana_chord::delta::Seg::Key("blocked".to_owned()),
                            ];
                            match blocked {
                                None => ops.push(Op::Delete(path)),
                                Some(blocked) => ops.push(Op::Set(
                                    path,
                                    serde_json::to_value(blocked).unwrap_or(JsonValue::Null),
                                )),
                            }
                        }
                        drop(state);
                        // TS 的 `publish` 在这里抛出时只让 `readOnLine` 的任务失败并被吞掉
                        // （`.catch(() => {})`），不让 Session 失败。
                        publish(&mount, ops, &context, &handle.report)?;
                        Ok(())
                    })
                    .await;
            });
        }
    }
}

/// 从 Storage 构建初始挂载（TS: `#build`）。
///
/// 对 `LIVE_STATUSES` 的每个状态分页扫描任务（每页 `SCAN_PAGE_SIZE`），合并后按数值 `TaskId`
/// 升序排序；再为每个任务分页扫描 `ownerTaskId == id` 的会话，收集 ID 并排序。每个节点
/// 询问一次 `blocking.blocked`，并把同一份 `BlockedSubject` 留在 `records` 里供后续刷新。
///
/// 所有扫描都用调用方上下文（也就是发起这次 `watch` 的上下文），任一页失败即整体失败，
/// 不会返回只构建了一半的图。`session` 参数当前没有用到（末尾的 `let _ = session;`）。
async fn build(
    session: &SessionImpl,
    storage: &dyn Storage,
    blocking: &TaskGraphBlocking,
    context: &dyn Context,
) -> Result<MountState, BoxError> {
    let mut nodes: IndexMap<String, TaskGraphNode> = IndexMap::new();
    // live：之后刷新 blocked 所需的记录片段；records：本次扫描到的完整任务记录。
    let mut live: IndexMap<String, BlockedSubject> = IndexMap::new();
    let mut records: Vec<TaskRecord> = Vec::new();
    for status in LIVE_STATUSES {
        let query = TaskQuery {
            status: Some(status),
            ..TaskQuery::default()
        };
        let found = scan_all(|cursor: Option<Cursor>| {
            let query = query.clone();
            async move {
                storage
                    .scan_tasks(&query, SCAN_PAGE_SIZE, cursor.as_ref(), context)
                    .await
            }
        })
        .await?;
        records.extend(found);
    }
    records.sort_by_key(|record| record.id);
    for record in records {
        let conversation_query = ConversationQuery {
            owner_task_id: Some(record.id),
            ..ConversationQuery::default()
        };
        let owned = scan_all(|cursor: Option<Cursor>| {
            let query = conversation_query.clone();
            async move {
                storage
                    .scan_conversations(&query, SCAN_PAGE_SIZE, cursor.as_ref(), context)
                    .await
            }
        })
        .await?;
        let mut conversations: Vec<ConversationId> = owned
            .into_iter()
            .map(|conversation| conversation.id)
            .collect();
        conversations.sort();
        let subject = blocked_subject(&record);
        let key = record.id.get().to_string();
        nodes.insert(
            key.clone(),
            node_of(&record, conversations, (blocking.blocked)(&subject)),
        );
        live.insert(key, subject);
    }
    let _ = session;
    Ok(MountState {
        value: TaskGraph { tasks: nodes },
        records: live,
        observers: IndexMap::new(),
        next_observer_id: 0,
    })
}

/// 由一次发布派生挂载的 op、应用它们，并把修订交给每个观察者（TS: `advance`）。
///
/// 两轮遍历 `publication.changes()`，因为提交内的变化顺序未定义：
/// 1. 任务：终态记录从 `records` 删除，节点存在时产出删除节点的 op；其余记录刷新
///    `records` 里的 `BlockedSubject`，重新 `node_of` 并与当前节点比较，只在节点真的变化
///    时产出 `Set`。
/// 2. 会话：`owner` 非空的会话并入 owner 节点的 `conversations`（追加后排序），前提是
///    owner 节点在本次提交或现有图里存在。
///
/// `changed` 记录本次发布已经 Set/Delete 的节点，使同一发布内后续比较看到新值而不是旧值。
///
/// 注意：`state.records` 在生成 op 时就已经更新，而 `state.value` 要等 `publish` 成功才
/// 更新；`publish` 失败时两者会短暂不一致。提交监听器路径会立即让 Session 失败，不会继续
/// 使用该挂载；刷新路径则吞掉错误（见 `RefreshHandle::refresh`）。
fn advance(
    mount: &Mount,
    publication: &ObservedCommit,
    context: &ContextRef,
    blocking: &TaskGraphBlocking,
    report: &Arc<dyn Fn(BoxError) + Send + Sync>,
) -> Result<(), BoxError> {
    let mut ops: Vec<Op> = Vec::new();
    {
        let mut state = lock(&mount.state);
        // 本次发布 set 或删除的节点（相对挂载的值）。
        let mut changed: IndexMap<String, Option<TaskGraphNode>> = IndexMap::new();
        let node =
            |state: &MountState, changed: &IndexMap<String, Option<TaskGraphNode>>, key: &str| {
                match changed.get(key) {
                    Some(node) => node.clone(),
                    None => state.value.tasks.get(key).cloned(),
                }
            };
        for change in publication.changes() {
            let CommitChange::Table(TableCommitChange::Task { value: record }) = change else {
                continue;
            };
            let key = record.id.get().to_string();
            let previous = node(&state, &changed, &key);
            if record.state.status() == TaskStatus::Terminal {
                state.records.shift_remove(&key);
                // 图上没有这个节点就没有可删的 op（这份挂载从未见过该任务）。
                let Some(previous) = previous else {
                    continue;
                };
                let _ = previous;
                ops.push(Op::Delete(vec![
                    kiana_chord::delta::Seg::Key("tasks".to_owned()),
                    kiana_chord::delta::Seg::Key(key.clone()),
                ]));
                changed.insert(key, None);
                continue;
            }
            let subject = blocked_subject(&record);
            state.records.insert(key.clone(), subject.clone());
            let conversations = previous
                .as_ref()
                .map(|previous| previous.conversations.clone())
                .unwrap_or_default();
            let next = node_of(&record, conversations, (blocking.blocked)(&subject));
            // 按完整节点值比较（`TaskGraphNode` 的 `PartialEq`）：相等就不发布，避免观察者
            // 收到内容没变的帧。这里持有挂载锁，`blocking.blocked` 也在锁内被调用。
            if previous.as_ref() == Some(&next) {
                continue;
            }
            ops.push(Op::Set(
                vec![
                    kiana_chord::delta::Seg::Key("tasks".to_owned()),
                    kiana_chord::delta::Seg::Key(key.clone()),
                ],
                serde_json::to_value(&next).unwrap_or(JsonValue::Null),
            ));
            changed.insert(key, Some(next));
        }
        // 任务之后处理会话：同一提交里创建会话与其 owner 任务时，owner 节点已存在；提交内 change
        // 顺序未定义，故每个 owner 的列表重新排序。
        let mut created: IndexMap<String, Vec<ConversationId>> = IndexMap::new();
        for change in publication.changes() {
            let CommitChange::Table(TableCommitChange::Conversation {
                value: conversation,
            }) = change
            else {
                continue;
            };
            let Some(owner) = &conversation.owner else {
                continue;
            };
            let key = owner.task_id.get().to_string();
            if node(&state, &changed, &key).is_some() {
                created.entry(key).or_default().push(conversation.id);
            }
        }
        for (key, ids) in created {
            let mut conversations = node(&state, &changed, &key)
                .map(|node| node.conversations)
                .unwrap_or_default();
            conversations.extend(ids);
            conversations.sort();
            ops.push(Op::Set(
                vec![
                    kiana_chord::delta::Seg::Key("tasks".to_owned()),
                    kiana_chord::delta::Seg::Key(key),
                    kiana_chord::delta::Seg::Key("conversations".to_owned()),
                ],
                serde_json::to_value(conversations).unwrap_or(JsonValue::Null),
            ));
        }
    }
    publish(mount, ops, context, report)
}

/// 应用 `ops` 并把修订交给每个观察者；一个观察者抛错被报告，其他仍收到（TS: `publish`）。
///
/// `ops` 为空时直接返回，不通知观察者（提交里没有节点变化就不产生帧）。
///
/// 应用方式是先把 `state.value` 序列化成 JSON、用 `apply_immutable` 应用、再反序列化回
/// `TaskGraph`。观察者拿到的值因此与它们将要重放的 op 严格对应；反序列化同时校验了 op
/// 产物的形状。
///
/// `applyImmutable` 失败在 TS 里从 `publish` 抛出（不报告、不通知观察者）；由调用方决定是让
/// Session 失败（提交监听器）还是只让当前 `readOnLine`（Rust 的 `read_on_line`）任务失败
/// （blocked 刷新）。
///
/// 观察者在释放挂载锁之后被调用；每个调用包在 `contained` 里，panic 转成 `report`，一个
/// 观察者的失败不会阻止其余观察者收到同一修订。帧上下文去掉中止信号，避免提交方的取消
/// 传播给 watch 的监听器。
fn publish(
    mount: &Mount,
    ops: Vec<Op>,
    context: &ContextRef,
    report: &Arc<dyn Fn(BoxError) + Send + Sync>,
) -> Result<(), BoxError> {
    if ops.is_empty() {
        return Ok(());
    }
    let value = {
        let mut state = lock(&mount.state);
        let encoded = serde_json::to_value(&state.value).unwrap_or(JsonValue::Null);
        let next = apply_immutable(encoded, &ops)
            .map_err(|error| Box::new(error) as BoxError)
            .and_then(|next| {
                serde_json::from_value::<TaskGraph>(next)
                    .map_err(|error| Box::new(error) as BoxError)
            })?;
        state.value = next;
        state.value.clone()
    };
    let frame_context = kiana_chord::context::without_abort_signal(Arc::clone(context));
    let observers: Vec<Observer> = {
        let state = lock(&mount.state);
        state.observers.values().cloned().collect()
    };
    for observer in observers {
        let report = Arc::clone(report);
        contained(
            || observer.advance(value.clone(), &ops, Arc::clone(&frame_context)),
            move |error| report(error),
        );
    }
    Ok(())
}

/// 把一个任务记录投影成图节点（TS: `nodeOf`）。
///
/// 只复制图需要的字段；`state` 由 `state_of` 派生，`blocked` 由调度器回调给出，
/// `conversations` 由调用方保证已按 ID 升序。字段顺序即序列化顺序（`conversations` 在
/// `blocked` 之前），定义在 `harness/types.rs` 的 `TaskGraphNode` 上。
fn node_of(
    record: &TaskRecord,
    conversations: Vec<ConversationId>,
    blocked: Option<BlockedReason>,
) -> TaskGraphNode {
    TaskGraphNode {
        id: record.id,
        kind: record.kind.clone(),
        conversation_id: record.conversation_id,
        owner: record.owner,
        background: record.background,
        abort_requested: record.abort_requested,
        state: state_of(record),
        blocked,
        conversations,
    }
}

/// 把持久状态投影成图状态，去掉 checkpoint 与 outcome 的 payload（TS: `stateOf`）。
///
/// `phase` 来自 checkpoint 的 `phase` 字段（见 `phase_of`）。`Waiting` 额外保留 `on` 与
/// `policy`，`Sleeping` 保留 `until`。`Terminal` 与 `Completing` 都映射为 `Completing`
/// 只是类型兜底：终态记录在 `advance` 里已被删除，不会走到这里。
fn state_of(record: &TaskRecord) -> TaskGraphState {
    let phase = phase_of(record);
    match &record.state {
        crate::types::TaskState::Pending { .. } => TaskGraphState::Pending { phase },
        crate::types::TaskState::Running { .. } => TaskGraphState::Running { phase },
        crate::types::TaskState::Waiting { on, policy, .. } => TaskGraphState::Waiting {
            phase,
            on: on.clone(),
            policy: *policy,
        },
        crate::types::TaskState::Sleeping { until, .. } => TaskGraphState::Sleeping {
            phase,
            until: *until,
        },
        // 终态记录不会到这里：它们离开图。
        crate::types::TaskState::Completing { outcome }
        | crate::types::TaskState::Terminal { outcome } => TaskGraphState::Completing {
            outcome: outcome_status(outcome),
        },
    }
}

/// `TaskOutcome` → 图中的结果判别值（TS: `state.outcome.status`）。
///
/// 图只暴露结果状态；错误文本、结果值等 payload 留在持久记录里，不复制到图上。
fn outcome_status(outcome: &TaskOutcome) -> TaskOutcomeStatus {
    match outcome {
        TaskOutcome::Completed { .. } => TaskOutcomeStatus::Completed,
        TaskOutcome::Failed { .. } => TaskOutcomeStatus::Failed,
        TaskOutcome::Aborted { .. } => TaskOutcomeStatus::Aborted,
        TaskOutcome::Orphaned { .. } => TaskOutcomeStatus::Orphaned,
        TaskOutcome::Faulted { .. } => TaskOutcomeStatus::Faulted,
    }
}

/// 取 checkpoint 里的 `phase`（TS: `phaseOf`）。
///
/// 缺失或不是字符串时返回空串：`TaskGraphState::phase` 是非可选 `String`，而 TS 侧该值
/// 可能是 `undefined`。任务定义负责在 checkpoint 里写 `{"phase": ...}`（测试夹具的
/// initial 就是 `{"phase": "work"}`），没有写入 phase 的记录会落到空串。
fn phase_of(record: &TaskRecord) -> String {
    record
        .state
        .checkpoint()
        .and_then(|checkpoint| checkpoint.get("phase"))
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_owned()
}
