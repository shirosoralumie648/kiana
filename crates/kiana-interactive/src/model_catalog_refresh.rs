//! modes/interactive/model-catalog-refresh.ts。
//!
//! 交互模式里所有「刷新全部模型目录」请求的进程内共享协调器：同一个 `ModelRuntime`
//! 上的并发刷新共享一次底层 `refresh`，但每个调用方保留独立的取消语义。
//!
//! TS: `packages/coding-agent/src/modes/interactive/model-catalog-refresh.ts`
//!
//! ## WeakMap 的 Rust 等价物
//!
//! TS 用 `WeakMap<ModelCatalogRuntime, ActiveModelCatalogRefresh>` 以**对象同一性**作键。
//! [`ModelRuntime`] 在 Rust 里是 `Arc<ModelRuntimeInner>` 的克隆句柄，因此用
//! [`ModelRuntime::identity`]（内部 `Arc` 数据地址）作键。[`ModelCatalogRuntime`] 是
//! `Pick<ModelRuntime, "refresh">` 的等价物：协调器只需要 `refresh` 与稳定身份；测试可以
//! 用实现该 trait 的假运行时替代真实运行时（对应 TS 的 `vi.spyOn(runtime, "refresh")`）。
//!
//! ## 生命周期与取消
//!
//! - 共享操作在 `tokio::spawn` 的独立任务里运行；放弃等待**不会**取消它（仍被驱动到结束），
//!   与 TS 的 `raceWithAbortSignal` 一致。
//! - 每个调用方的等待与自己的 `CancellationToken` 竞速；最后一个等待者离开时中止共享
//!   `AbortScope`，下一次调用会新建一次刷新。
//! - 完成（成功、失败或中止）时，共享任务在发布结果前从映射里删除**仍指向自己**的条目；
//!   之后的调用者一定会发起新的刷新。
//!
//! ## 一次调用的执行流程
//! 1. 调用方 token 已取消时立即返回 `AbortError`（对应 `signal.throwIfAborted()`）。
//! 2. 以 `identity()` 为键查全局表：命中就复用，未命中就创建条目并 `tokio::spawn` 共享
//!    任务；共享任务再用一个内部任务真正执行 `refresh_catalogs`。
//! 3. 等待者计数 +1，并用 `WaiterGuard` 保证任何返回路径（包括取消）都会在 `Drop`
//!    里递减。
//! 4. 订阅 `watch` 结果通道：已有结果直接克隆；否则与自己的取消 token 竞速。
//! 5. 共享任务结束时（操作完成或共享 `AbortScope` 中止），在**同一把锁**里删除仍指向
//!    自己的表项并发布结果，保证删除之后到达的调用一定重新发起刷新。
//!
//! ## 注意事项
//! - 全局表是进程级单例（`OnceLock<Mutex<IndexMap<…>>>`），键是运行时的 `Arc` 数据地址，
//!   因此不同 `ModelRuntime` 实例互不影响，同一实例的所有克隆共享一次刷新。
//! - 锁内只做映射的查找/插入/删除与非阻塞的结果发布（`watch::Sender::send_replace`），
//!   不跨 `await` 持有；共享刷新在锁外运行。锁中毒时用
//!   `into_inner` 继续使用内层数据，而不是 panic。
//! - `refresh_catalogs` 返回的 future 必须 `Send`（类型别名已要求），因为它会被移进
//!   `tokio::spawn` 的任务；实现者不能假设自己运行在调用方的任务里。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use indexmap::IndexMap;
use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::models::{ModelsRefreshOptions, ModelsRefreshResult};
use kiana_ai::utils::abort::{AbortReason, AbortScope};
use kiana_config::models::runtime::ModelRuntime;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// `refresh_catalogs` 的返回 future（TS 的 `Promise<ModelsRefreshResult>`）。
pub type ModelCatalogRefreshFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ModelsRefreshResult, AiError>> + Send + 'a>>;

/// TS: `type ModelCatalogRuntime = Pick<ModelRuntime, "refresh">`。
///
/// `identity` 是 Rust 侧额外需要的「对象同一性」（TS 的 WeakMap 键）。
///
/// # 实现契约
/// - `identity` 必须在运行时的整个生命周期内稳定，同一实例的所有克隆返回同一个值，不同
///   实例返回不同的值（`ModelRuntime` 用内部 `Arc` 数据地址实现）。返回不稳定的值会让
///   并发调用无法共享同一次刷新，甚至把不同运行时误并成一次。
/// - `refresh_catalogs` 的 future 会被移进 `tokio::spawn` 的独立任务，因此必须是
///   `Send`（[`ModelCatalogRefreshFuture`] 已声明）。调用方取消不会中止这个 future，
///   是否响应 `signal` 由实现者决定。
// `Err` 变体是 kiana-ai 冻结的 `AiError`（`Models`/`Auth` 的错误签名不可改），与 kiana-ai 同类的
// 公开签名保持原样并关闭该 lint。
#[allow(clippy::result_large_err)]
pub trait ModelCatalogRuntime: Send + Sync {
    /// 运行时的稳定身份（同一运行时的所有克隆返回同一个值）。
    fn identity(&self) -> usize;

    /// `modelRuntime.refresh({ signal })`。
    ///
    /// `signal` 是共享刷新的 `AbortScope`，只在没有任何等待者时被协调器中止；实现应把它
    /// 透传给底层刷新（`ModelRuntime` 的实现即如此），并在其中止时尽快返回错误。
    fn refresh_catalogs(&self, signal: Option<AbortScope>) -> ModelCatalogRefreshFuture<'_>;
}

impl ModelCatalogRuntime for ModelRuntime {
    fn identity(&self) -> usize {
        ModelRuntime::identity(self)
    }

    fn refresh_catalogs(&self, signal: Option<AbortScope>) -> ModelCatalogRefreshFuture<'_> {
        Box::pin(async move {
            ModelRuntime::refresh(
                self,
                ModelsRefreshOptions {
                    signal,
                    ..Default::default()
                },
            )
            .await
        })
    }
}

/// `Arc<T>` 转发（组件内部用 `Arc<dyn ModelSelectorRuntime>` 持有运行时）。
impl<T> ModelCatalogRuntime for Arc<T>
where
    T: ModelCatalogRuntime + ?Sized,
{
    fn identity(&self) -> usize {
        (**self).identity()
    }

    fn refresh_catalogs(&self, signal: Option<AbortScope>) -> ModelCatalogRefreshFuture<'_> {
        (**self).refresh_catalogs(signal)
    }
}

/// 共享结果（`Arc` 让多个等待者廉价共享）。
type SharedOutcome = Arc<Result<ModelsRefreshResult, AiError>>;

/// TS: `ActiveModelCatalogRefresh`。
struct ActiveRefresh {
    /// TS 的 `AbortController`（`abort()` 后 `raceWithAbortSignal` 以 AbortError 结束）。
    scope: AbortScope,
    /// 尚未离开的等待者数量。
    waiters: AtomicUsize,
    /// 共享 promise 的结果；`None` 表示仍在进行。
    outcome: watch::Sender<Option<SharedOutcome>>,
}

/// 全局协调器（进程内单例；对应 TS 模块级的 `modelCatalogRefreshCoordinator`）。
fn registry() -> &'static Mutex<IndexMap<usize, Arc<ActiveRefresh>>> {
    static REGISTRY: OnceLock<Mutex<IndexMap<usize, Arc<ActiveRefresh>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(IndexMap::new()))
}

fn lock_registry() -> MutexGuard<'static, IndexMap<usize, Arc<ActiveRefresh>>> {
    registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `abortReason(scope)`：中止原因缺省时是 `DOMException("This operation was aborted",
/// "AbortError")`（Node 的 `AbortController.abort()` 缺省 reason）。
fn abort_reason_error(scope: &AbortScope) -> AiError {
    scope
        .reason()
        .unwrap_or_else(AbortReason::default_abort_error)
        .to_error()
}

/// 调用方 token 已中止时的错误（`signal.throwIfAborted()`）。
fn aborted_error() -> AiError {
    AbortReason::default_abort_error().to_error()
}

/// 等待者离开时递减计数；最后一个离开者中止共享刷新。
struct WaiterGuard {
    key: usize,
    entry: Arc<ActiveRefresh>,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        // TS: `active.waiters--; if (active.waiters === 0 && map.get(runtime) === active) abort()`。
        // 先递减再判断：并发离开时只有一个线程会看到 `previous == 1`，因此 `abort()` 至多
        // 触发一次。随后在锁内确认表项仍是自己，避免误中止后来者的新刷新。
        let previous = self.entry.waiters.fetch_sub(1, Ordering::SeqCst);
        if previous != 1 {
            return;
        }
        let still_current = {
            let registry = lock_registry();
            registry
                .get(&self.key)
                .is_some_and(|entry| Arc::ptr_eq(entry, &self.entry))
        };
        if still_current {
            self.entry.scope.abort();
        }
    }
}

/// 启动一次共享刷新任务（TS 的 `raceWithAbortSignal(operation, controller.signal).finally(...)`）。
///
/// 分两层任务：外层协调「操作完成」与「共享 `AbortScope` 中止」，内层真正执行
/// `refresh_catalogs`。中止时外层 `select!` 直接放弃等待，而内层任务脱离 `JoinHandle`
/// 继续运行到 settle——这正是 TS `raceWithAbortSignal` 的观察语义。
fn spawn_shared<R>(runtime: R, key: usize, entry: Arc<ActiveRefresh>)
where
    R: ModelCatalogRuntime + 'static,
{
    tokio::spawn(async move {
        let scope = entry.scope.clone();
        // 操作自己 spawn：中止只放弃等待，operation 继续运行到 settle（TS 的观察语义）。
        let mut handle = tokio::spawn(async move { runtime.refresh_catalogs(Some(scope)).await });
        let outcome = tokio::select! {
            joined = &mut handle => match joined {
                Ok(result) => result,
                Err(error) if error.is_panic() => {
                    // TS 里 operation 的未处理 rejection 会传播给等待者；这里退化成可观测错误，
                    // 避免等待者永远挂起（ModelRuntime::refresh 不应 panic）。
                    let _ = error.into_panic();
                    Err(AiError::new(
                        AiErrorKind::Other,
                        "operation task was cancelled",
                    ))
                }
                Err(_) => Err(AiError::new(
                    AiErrorKind::Other,
                    "operation task was cancelled",
                )),
            },
            _ = entry.scope.cancelled() => Err(abort_reason_error(&entry.scope)),
        };

        // `.finally(() => { if (map.get(runtime) === created) map.delete(runtime); })`
        // 与结果发布在同一次临界区内完成：删除之后到达的调用者一定会新建刷新。
        {
            let mut registry = lock_registry();
            if registry
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &entry))
            {
                registry.shift_remove(&key);
            }
            entry.outcome.send_replace(Some(Arc::new(outcome)));
        }
    });
}

/// TS: `refreshModelCatalogs(modelRuntime, signal)`。
///
/// 共享同一 `ModelRuntime` 上的并发刷新，同时保持每个调用方的取消独立。
///
/// # 参数与返回
/// - `model_runtime`：只用于取身份与发起底层刷新；要求 `Clone + 'static`，因为共享任务
///   需要拥有一个 `'static` 的运行时句柄。
/// - `signal`：调用方自己的取消令牌；`None` 表示不可取消。
/// - 成功时返回共享结果的克隆，所有等待者看到同一份数据；共享刷新失败或调用方取消时
///   返回对应的 `AiError`（取消为 `AbortError`）。
///
/// # 取消语义
/// 调用方取消只结束**自己的等待**，不会中止仍在被其他等待者使用的底层刷新。只有最后一个
/// 等待者离开且表项仍指向本次刷新时，共享 `AbortScope` 才会中止；此后表项被删除，新调用
/// 会发起一次新刷新，而中止过程中加入的调用会收到 `AbortError`。
///
/// # panic 处理
/// 共享任务 panic 时等待者收到 `AiErrorKind::Other`（"operation task was cancelled"），
/// 而不是永久挂起；panic 本身不会被重新抛出。
#[allow(clippy::result_large_err)]
pub async fn refresh_model_catalogs<R>(
    model_runtime: &R,
    signal: Option<&CancellationToken>,
) -> Result<ModelsRefreshResult, AiError>
where
    R: ModelCatalogRuntime + Clone + 'static,
{
    // TS: `signal.throwIfAborted()`。
    if signal.is_some_and(CancellationToken::is_cancelled) {
        return Err(aborted_error());
    }

    let key = model_runtime.identity();
    // 查表与建表在同一把锁内完成，避免两个并发调用各建一个共享任务。
    let (entry, created) = {
        let mut registry = lock_registry();
        match registry.get(&key).cloned() {
            Some(entry) => (entry, false),
            None => {
                let (outcome, _receiver) = watch::channel::<Option<SharedOutcome>>(None);
                let entry = Arc::new(ActiveRefresh {
                    scope: AbortScope::new(),
                    waiters: AtomicUsize::new(0),
                    outcome,
                });
                registry.insert(key, Arc::clone(&entry));
                (entry, true)
            }
        }
    };
    if created {
        spawn_shared(model_runtime.clone(), key, Arc::clone(&entry));
    }

    // TS: `active.waiters++`。命中计数与守卫在同一临界区内建立：任何提前返回都会在 Drop 里
    // 递减（对应 `.finally(...)` 总会执行）。
    entry.waiters.fetch_add(1, Ordering::SeqCst);
    let _guard = WaiterGuard {
        key,
        entry: Arc::clone(&entry),
    };

    let mut receiver = entry.outcome.subscribe();
    // 先取出当前值：`watch::Ref` 临时量不能跨 match 分支存活（分支里还要可变借用 receiver）。
    let current = receiver.borrow().clone();
    let shared = match current {
        Some(shared) => shared,
        None => {
            tokio::select! {
                changed = receiver.changed() => {
                    // 共享任务在发布结果前先删除映射；`send_replace` 不会失败，因此这里必有值。
                    let _ = changed;
                    receiver
                        .borrow()
                        .clone()
                        .expect("shared refresh publishes an outcome before finishing")
                }
                _ = cancel_signal(signal) => return Err(aborted_error()),
            }
        }
    };
    (*shared).clone()
}

/// 等待调用方 token 取消；`None` 时永不完成。
///
/// 独立成函数是为了让 `select!` 分支在 `signal == None` 时挂起而不是提前结束，
/// 与 TS 的 `raceWithAbortSignal(signal ?? undefined)`（无信号即不可取消）对应。
async fn cancel_signal(signal: Option<&CancellationToken>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending().await,
    }
}
