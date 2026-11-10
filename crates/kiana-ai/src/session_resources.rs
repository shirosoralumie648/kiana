//! `session-resources.ts`：`SessionResourceCleanup` 注册表与 `cleanupSessionResources`。
//!
//! TS: `packages/ai/src/session-resources.ts`。
//!
//! TS 用一个模块级 `Set` 保存清理回调；Rust 用 `Mutex<Vec<..>>` 保存注册项，注册时返回
//! **显式**的注销函数（对应 TS 返回的 `() => void`）。注意：注销**不**随返回值析构发生——
//! TS 的调用方（`providers/openai-codex-responses.ts`）直接丢弃返回值，Rust 若用 Drop 自动
//! 注销会立刻把清理项删掉。
//!
//! `cleanup_session_resources` 逐个调用并收集错误；有错误时返回
//! [`crate::session_resources::SessionResourceCleanupError`]（对应 TS 的 `AggregateError`，`message` 逐字
//! `"Failed to cleanup session resources"`，`errors` 保留每个抛出值）。
//!
//! `Set` 的语义逐条保留：
//! - **身份去重**：TS 的 `Set` 按函数对象身份去重，Rust 用 `Arc::ptr_eq` 表达；同一 `Arc`
//!   重复注册只有一项（位置不变），任意一次注销都会删掉它（注销闭包按身份删除，因此对
//!   「删除后重新注册的同一 `Arc`」同样生效）。
//! - **活迭代**：`for (const cleanup of set)` 会访问迭代过程中新加入的项，因此这里不用快照，
//!   而是按注册 id 递增的游标逐个取（调用回调时不持锁，回调可安全重入注册/注销）；
//!   本轮新注册的项会在同一轮被访问，调用前已注销的项被跳过。
//! - 插入顺序：注销后重新注册的项追加到末尾。
//!
//! ## 调用方与进程级边界
//!
//! - 注册：`api/openai_codex_responses.rs` 的 `ensure_cleanup_registered()` 用 `OnceLock`
//!   保证进程内只注册一次，回调是 `close_openai_codex_websocket_sessions`；它故意丢弃返回的
//!   注销句柄（丢弃不注销），所以 Codex WebSocket 的清理在整个进程生命周期内有效。
//! - 触发：`kiana-session` 的 `AgentSession::dispose` 在拆除会话时调用
//!   `cleanup_session_resources(Some(session_id))`，并用 `let _ =` 忽略返回的错误。
//! - 注册表是**进程级**的，不是「每个 session 一个 Set」：`session_id` 只作为参数传给每个
//!   回调，由回调自己决定要清理哪些资源（Codex 的实现按 session id 筛选连接池）。
//! - 扩展 JS 面的同名函数（`kiana-extensions` 的 `kiana-ai-helpers.js`）是另一份独立的
//!   JS `Set` 实现，与本注册表不互通；不要把两边的注册混为一谈。
//!
//! ## 并发与失败模式
//!
//! - 锁只保护 `Vec` 的读写，回调一律在锁外执行：回调可以重入注册/注销，也不会因为回调
//!   panic 而毒化注册表。所有 `lock()` 都用 `unwrap_or_else(|e| e.into_inner())` 吞掉
//!   中毒状态（防御性处理，正常路径不应发生）。
//! - 若两个线程同时调用 `cleanup_session_resources`，同一个回调可能被并发执行（本模块
//!   不做互斥）；TS 的单线程事件循环没有这个窗口，因此回调实现需要自己保证幂等/线程安全。
//! - 回调返回 `Err` 会被收集；**panic 不会**被收集——这里没有 `catch_unwind`，panic 会
//!   直接沿调用栈展开（TS 的 try/catch 只对应 Rust 的 `Result` 通道）。
//! - 清理**不清空**注册表：没有注销的回调在每次调用时都会再次执行（TS 的 `Set` 同样不
//!   clear），需要一次性清理的调用方应自己保存并调用注销句柄。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::utils::diagnostics::ThrownValue;

/// TS 的 `SessionResourceCleanup`：`(sessionId?: string) => void`。
///
/// Rust 侧用 `Result` 表达 TS 的抛出；抛出值统一为 [`ThrownValue`]（`Error` / 字符串 / JSON）。
/// TS 的「函数对象身份」对应 [`Arc`] 指针身份（见 [`register_session_resource_cleanup`]）。
pub type SessionResourceCleanup =
    Arc<dyn Fn(Option<&str>) -> Result<(), ThrownValue> + Send + Sync>;

/// `AggregateError` 的消息文本。
const CLEANUP_FAILED_MESSAGE: &str = "Failed to cleanup session resources";

/// 注册表项。
struct Registration {
    /// 单调递增的注册序号：插入顺序 = id 升序，`cleanup_session_resources` 的活迭代游标用它。
    id: u64,
    cleanup: SessionResourceCleanup,
}

/// 模块级注册表（`new Set<SessionResourceCleanup>()`）。
///
/// 进程级全局状态：所有 session 共用这一张表，按 session 区分是回调自己的职责。
/// 测试通过 `TEST_LOCK` 串行执行并清空它，避免用例之间互相看见残留项。
static REGISTRY: Mutex<Vec<Registration>> = Mutex::new(Vec::new());

/// 单调递增的注册 id（只用于活迭代游标；注销按回调身份匹配）。
///
/// `Relaxed` 足够：id 的分配与读取都发生在持有 `REGISTRY` 锁的代码里，互斥锁本身已经
/// 提供跨线程同步；`AtomicU64` 只用来在没有额外锁的情况下取得唯一序号。
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// `cleanupSessionResources` 抛出的聚合错误（TS 的 `AggregateError`）。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionResourceCleanupError {
    /// 每个 cleanup 抛出的值，按注册顺序。
    pub errors: Vec<ThrownValue>,
    /// 逐字 `"Failed to cleanup session resources"`。
    pub message: String,
}

impl SessionResourceCleanupError {
    /// 用收集到的抛出值构造（`message` 固定）。
    pub fn new(errors: Vec<ThrownValue>) -> Self {
        SessionResourceCleanupError {
            errors,
            message: CLEANUP_FAILED_MESSAGE.to_string(),
        }
    }
}

impl std::fmt::Display for SessionResourceCleanupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SessionResourceCleanupError {}

/// `registerSessionResourceCleanup(cleanup)`：注册清理回调，返回注销函数。
///
/// 与 TS 的 `Set.add` 一致：按回调身份（[`Arc::ptr_eq`]）去重，同一回调重复注册不会产生第二项，
/// 也不会改变它的位置。注销函数等价于 `set.delete(cleanup)`——按身份删除，可重复调用
/// （第二次起是 no-op），并且对「删除后重新注册的同一回调」同样生效；
/// **丢弃返回值不会注销**。
///
/// # 实现约束
///
/// 去重检查、id 分配与 `push` 在同一把 `REGISTRY` 锁内完成，因此 `Vec` 中的条目始终按
/// id 升序排列——`cleanup_session_resources` 的「活迭代」游标依赖这个不变量。
///
/// 返回的注销闭包持有回调的 `Arc` 强引用：注册表与句柄各持一份。注销后注册表不再持有，
/// 但只要句柄还活着，回调（以及它捕获的资源）就无法释放；长期保存注销句柄会延长这些
/// 资源的生命周期。
pub fn register_session_resource_cleanup(
    cleanup: SessionResourceCleanup,
) -> Box<dyn Fn() + Send + Sync> {
    {
        let mut registry = REGISTRY.lock().unwrap_or_else(|error| error.into_inner());
        let already_registered = registry
            .iter()
            .any(|registration| Arc::ptr_eq(&registration.cleanup, &cleanup));
        if !already_registered {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            registry.push(Registration {
                id,
                cleanup: Arc::clone(&cleanup),
            });
        }
    }
    Box::new(move || {
        let mut registry = REGISTRY.lock().unwrap_or_else(|error| error.into_inner());
        registry.retain(|registration| !Arc::ptr_eq(&registration.cleanup, &cleanup));
    })
}

/// `cleanupSessionResources(sessionId?)`：依次调用全部清理回调并收集错误。
///
/// 全部成功 → `Ok(())`；任一回调返回 `Err` → 收齐后返回
/// [`SessionResourceCleanupError`]（即使某些回调失败，其余回调仍会执行，避免资源泄漏）。
///
/// # 行为细节
///
/// - `session_id` 原样传给每个回调（`None` 也照传），本函数不按它筛选任何注册项。
/// - 回调在锁外执行，因此可以安全地在清理过程中注册/注销；遍历按 id 升序推进，遍历期间
///   新注册的项（id 更大）会在同一轮执行，遍历到之前已被注销的项自然消失。
/// - 注册表不会因为本次清理而清空：没有注销的回调在下次调用时仍会执行。
/// - 回调若在每次执行时注册一个新的回调（新的 `Arc`），或先注销再重新注册自己，新 id 会
///   大于游标并在同一轮被反复访问（与 TS 的活 `Set` 迭代一致），可能导致本轮永远不结束；
///   清理回调不应自我重注册。（`register_session_resource_cleanup` 按 `Arc::ptr_eq` 去重，
///   同一个 `Arc` 重复注册不会分配新 id。）
/// - 错误只汇总 `Err`；回调 panic 会直接展开到调用方，不会进入 [`SessionResourceCleanupError`]。
pub fn cleanup_session_resources(
    session_id: Option<&str>,
) -> Result<(), SessionResourceCleanupError> {
    let mut errors = Vec::new();
    // 活迭代：每步只持锁取「下一个 id 大于游标的项」，调用回调时不持锁，因此回调可以重入
    // 注册/注销——新注册的项（id 更大）会在同一轮被访问，调用前被注销的项自然消失。
    let mut cursor: Option<u64> = None;
    loop {
        let next = {
            let registry = REGISTRY.lock().unwrap_or_else(|error| error.into_inner());
            registry
                .iter()
                .find(|registration| cursor.is_none_or(|id| registration.id > id))
                .map(|registration| (registration.id, Arc::clone(&registration.cleanup)))
        };
        let Some((id, cleanup)) = next else {
            break;
        };
        cursor = Some(id);
        if let Err(error) = cleanup(session_id) {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(SessionResourceCleanupError::new(errors))
    }
}

#[cfg(test)]
// 清理回调返回 `Result<(), ThrownValue>`：`ThrownValue` 内联 `DiagnosticErrorInfo`，尺寸超过
// clippy 的 128 字节阈值。它由 TS 的 `unknown` 抛出值形状决定，测试里不能改签名。
#[allow(clippy::result_large_err)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// 注册表是进程级全局状态：本模块的测试串行执行并在开始前清空。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// 取测试锁并清空注册表（避免测试间互相看见残留项）。
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let mut registry = REGISTRY.lock().unwrap_or_else(|error| error.into_inner());
        registry.clear();
        guard
    }

    fn recording_cleanup(
        log: Arc<Mutex<Vec<Option<String>>>>,
        result: Result<(), ThrownValue>,
    ) -> SessionResourceCleanup {
        Arc::new(move |session_id| {
            log.lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(session_id.map(str::to_string));
            result.clone()
        })
    }

    #[test]
    fn runs_every_registered_cleanup_in_registration_order() {
        let _guard = lock();
        let log = Arc::new(Mutex::new(Vec::new()));
        let first = register_session_resource_cleanup(recording_cleanup(Arc::clone(&log), Ok(())));
        let second = register_session_resource_cleanup(recording_cleanup(Arc::clone(&log), Ok(())));
        assert_eq!(cleanup_session_resources(Some("session-1")), Ok(()));
        assert_eq!(
            *log.lock().unwrap_or_else(|error| error.into_inner()),
            vec![Some("session-1".to_string()), Some("session-1".to_string())]
        );
        first();
        second();
        log.lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        assert_eq!(cleanup_session_resources(None), Ok(()));
        assert_eq!(
            *log.lock().unwrap_or_else(|error| error.into_inner()),
            Vec::<Option<String>>::new()
        );
    }

    #[test]
    fn passes_the_session_id_through_to_every_cleanup() {
        let _guard = lock();
        let log = Arc::new(Mutex::new(Vec::new()));
        let handle = register_session_resource_cleanup(recording_cleanup(Arc::clone(&log), Ok(())));
        assert_eq!(cleanup_session_resources(None), Ok(()));
        assert_eq!(
            *log.lock().unwrap_or_else(|error| error.into_inner()),
            vec![None]
        );
        handle();
    }

    #[test]
    fn collects_errors_and_still_runs_the_rest() {
        let _guard = lock();
        let calls = Arc::new(AtomicUsize::new(0));
        let log = Arc::new(Mutex::new(Vec::new()));
        let failing = register_session_resource_cleanup(Arc::new({
            let calls = Arc::clone(&calls);
            move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(ThrownValue::from_text("boom"))
            }
        }));
        let succeeding =
            register_session_resource_cleanup(recording_cleanup(Arc::clone(&log), Ok(())));
        let error = cleanup_session_resources(Some("s")).expect_err("aggregate error");
        assert_eq!(error.message, "Failed to cleanup session resources");
        assert_eq!(error.to_string(), "Failed to cleanup session resources");
        assert_eq!(error.errors, vec![ThrownValue::from_text("boom")]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // 失败之后仍继续执行其余清理。
        assert_eq!(
            *log.lock().unwrap_or_else(|error| error.into_inner()),
            vec![Some("s".to_string())]
        );
        failing();
        succeeding();
    }

    #[test]
    fn aggregates_every_thrown_value_in_registration_order() {
        let _guard = lock();
        let first = register_session_resource_cleanup(Arc::new(|_| {
            Err(ThrownValue::from_json(serde_json::json!({"code": "first"})))
        }));
        let second =
            register_session_resource_cleanup(Arc::new(|_| Err(ThrownValue::from_text("second"))));
        let error = cleanup_session_resources(None).expect_err("aggregate error");
        assert_eq!(
            error.errors,
            vec![
                ThrownValue::from_json(serde_json::json!({"code": "first"})),
                ThrownValue::from_text("second"),
            ]
        );
        first();
        second();
    }

    #[test]
    fn unregistering_is_idempotent_and_removes_the_cleanup() {
        let _guard = lock();
        let calls = Arc::new(AtomicUsize::new(0));
        let unregister = register_session_resource_cleanup(Arc::new({
            let calls = Arc::clone(&calls);
            move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }));
        unregister();
        unregister();
        assert_eq!(cleanup_session_resources(None), Ok(()));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn dropping_the_unregister_function_keeps_the_cleanup_registered() {
        let _guard = lock();
        let calls = Arc::new(AtomicUsize::new(0));
        let unregister = register_session_resource_cleanup(Arc::new({
            let calls = Arc::clone(&calls);
            move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }));
        // TS 的 `registerSessionResourceCleanup(...)` 调用点（openai-codex-responses）就丢弃了
        // 返回值；Rust 若用 Drop 自动注销就会立刻失效。
        drop(unregister);
        assert_eq!(cleanup_session_resources(None), Ok(()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn runs_cleanups_registered_during_the_same_pass() {
        let _guard = lock();
        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let first = register_session_resource_cleanup(Arc::new({
            let log = Arc::clone(&log);
            move |_| {
                log.lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push("first");
                // 清理过程中新注册的回调：TS 的活 `Set` 会在同一轮访问它。
                let _late = register_session_resource_cleanup(Arc::new({
                    let log = Arc::clone(&log);
                    move |_| {
                        log.lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .push("late");
                        Ok(())
                    }
                }));
                Ok(())
            }
        }));
        assert_eq!(cleanup_session_resources(None), Ok(()));
        assert_eq!(
            *log.lock().unwrap_or_else(|error| error.into_inner()),
            vec!["first", "late"]
        );
        first();
    }

    #[test]
    fn skips_a_cleanup_unregistered_before_it_is_visited() {
        let _guard = lock();
        type Unregister = Box<dyn Fn() + Send + Sync>;
        let slot: Arc<Mutex<Option<Unregister>>> = Arc::new(Mutex::new(None));
        let second_calls = Arc::new(AtomicUsize::new(0));
        let first = register_session_resource_cleanup(Arc::new({
            let slot = Arc::clone(&slot);
            move |_| {
                // 游标还没走到第二项就把它注销掉：TS 的活 `Set` 迭代会跳过它。
                if let Some(unregister) = slot
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                {
                    unregister();
                }
                Ok(())
            }
        }));
        let second = register_session_resource_cleanup(Arc::new({
            let second_calls = Arc::clone(&second_calls);
            move |_| {
                second_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }));
        *slot.lock().unwrap_or_else(|error| error.into_inner()) = Some(second);
        assert_eq!(cleanup_session_resources(None), Ok(()));
        assert_eq!(second_calls.load(Ordering::SeqCst), 0);
        first();
    }

    #[test]
    fn deduplicates_a_callback_by_identity_like_a_js_set() {
        let _guard = lock();
        let calls = Arc::new(AtomicUsize::new(0));
        let cleanup: SessionResourceCleanup = Arc::new({
            let calls = Arc::clone(&calls);
            move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        let first = register_session_resource_cleanup(Arc::clone(&cleanup));
        let second = register_session_resource_cleanup(Arc::clone(&cleanup));
        drop(second);
        // 同一个函数对象在 `Set` 里只有一项。
        assert_eq!(cleanup_session_resources(None), Ok(()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 注销闭包按身份删除（`set.delete(cleanup)`），对删除后重新注册的同一回调也生效。
        first();
        assert_eq!(cleanup_session_resources(None), Ok(()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let third = register_session_resource_cleanup(Arc::clone(&cleanup));
        first();
        assert_eq!(cleanup_session_resources(None), Ok(()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        third();
    }
}
