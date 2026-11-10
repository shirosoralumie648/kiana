//! auth/credential-store.ts。
//!
//! 默认内存凭证存储：按 `Provider.id` 一条凭证，写入按 provider 串行化。
//! App 注入持久化实现（coding-agent 的 `AuthStorage`）；[`InMemoryCredentialStore`] 用于默认值与测试。
//!
//! TS 的 promise-chain 队列在 Rust 侧映射为「每个 provider 一把异步互斥锁」：
//! - 排队等锁期间可被 signal 中止（`raceWithAbortSignal(queued, signal)`）；
//! - 中止只丢弃等待者，**不打断正在运行的任务**；被取消的排队任务在拿到锁后立刻
//!   `throwIfAborted()`，因此**不会**在稍后执行（`models-runtime.test.ts` 的
//!   "cancels queued credential mutations without running them later"）；
//! - 任务本身继续跑完，其结果被丢弃（与 TS 的 `void queued.catch(() => {})` 一致）。
//!
//! # 为什么需要这一层
//! 凭证（API key 或 OAuth token）的存放介质由宿主决定：本文件只提供进程内存版本，作为
//! `CreateModelsOptions` 的默认值与测试基线；生产环境的 coding-agent 注入 `AuthStorage`
//! 之类的持久化实现。`models` 模块通过 `types` 里的 `CredentialStore` trait 使用存储（调用方
//! 未注入实现时用本文件的默认值），所以「存哪里」（内存 / 磁盘 / 钥匙串）与「怎么鉴权」
//! （api key / OAuth）互不影响。
//!
//! # 数据结构与不变量
//! - `Inner.credentials` 是 `IndexMap<provider_id, Credential>`：用 `IndexMap` 而不是
//!   `HashMap`，因为 `list()` 的输出顺序必须等于 TS `Map` 的首次插入顺序（spec/ai-auth.md
//!   §2.4）。更新已有 key 是原位替换、不改变位置；`delete` 用 `shift_remove`
//!   （`swap_remove` 会把末尾元素挪进空位，破坏其余条目的顺序）。
//! - `Inner.gates` 是 `HashMap<provider_id, Arc<tokio::sync::Mutex<()>>>`：同一 provider 的
//!   `modify` / `delete` 串行，不同 provider 之间可并行。
//! - 两张表各用一把 `std::sync::Mutex` 保护，且从不嵌套持有；凭据表只在同步的 map 操作
//!   期间加锁，绝不跨 `.await`（`MutexGuard` 不是 `Send`，跨 await 会让整个 future 失去
//!   `Send`）。
//!
//! # 并发与取消（逐条对应 TS 的 promise-chain）
//! 1. 写操作进入 `enqueue`：取该 provider 的门 → 检查中止 → 执行任务。
//! 2. 等门阶段可被 signal 中止，此时任务不会运行。拿到门后还有一次 `throw_if_aborted`
//!    兜底：`select!` 无偏向，signal 已中止而门恰好空闲时可能先选中取锁分支，没有这次
//!    检查，被取消的排队任务就会执行（TS 的检查在 `await previous` 之后、`task()` 之前）。
//! 3. 中止不打断**已开始**的任务：`race_with_abort_signal` 只放弃等待，任务继续跑完。
//!    **返回值与副作用可能不一致**：中止与完成同时发生时，调用方可能收到 `AbortError`
//!    而写入已经生效（`race_with_abort_signal` 对已中止的 signal 直接拒绝，不检查任务是否
//!    已经完成）。
//! 4. 回调返回 `Err` 只让该次调用失败，门在任务结束时释放，后续写照常。
//! 5. `read` / `list` 不进队列，只做一次同步读取，因此可能观察到某次进行中的 `modify`
//!    写入之前的旧值（与 TS 的读路径一致）。
//!
//! # 错误传播
//! 回调与门操作产生的 `AuthError` 原样上抛，不做包装；`race_with_abort_signal` 返回的
//! `AiError` 经 `AuthError::from_ai_error` 转换，`name` / `message` 原样保留（`code` /
//! `details` 为空）。默认的 `abort()` 产生 `AbortError`，但 `abort_with` 传入的自定义
//! reason（如 `TimeoutError`）其 `name` 不会被改写；而 `AuthError::is_aborted` 只认
//! `AbortError`，所以判断是否为调用方取消要结合所用 signal 的 reason，不能无条件比较
//! `name`。本类型不重试任何失败，是否重试由调用方决定。
//!
//! # 注意事项
//! - `modify` 的回调持有该 provider 的门：回调里不得再对同一 provider 调用本 store 的
//!   `modify` / `delete`，否则会等待自己（见 `types` 中 `CredentialStore` 的文档）。
//! - `gates` 条目不回收：每个被写过一次的 provider id 永久保留一个 `Arc<AsyncMutex<()>>`。
//!   内置 provider 的 id 来自编译期注册表、数量有界，故未做清理；自定义 provider 的 id
//!   由宿主提供，若可能无限增长，需要重新评估这部分内存。
//! - 所有 `lock()` 都在 poison 时取回内部值（`unwrap_or_else(into_inner)`），一次 panic
//!   不会让后续所有操作级联失败。
//!
//! TS: `src/auth/credential-store.ts`

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use tokio::sync::Mutex as AsyncMutex;

use crate::auth::types::abort_error;
use crate::auth::types::{
    AuthError, AuthOperationOptions, Credential, CredentialInfo, CredentialModifyFn,
    CredentialStore, throw_if_aborted,
};
use crate::utils::abort::{operation_signal, race_with_abort_signal};

// 两张表各用一把锁、从不嵌套持有：`gate()` 只锁 `gates`，凭据读写只锁 `credentials`。
// 回调的 await 期间不持有任何同步锁，map 读写只在同步语句内短暂持锁；因此不存在锁顺序
// 反转，也不会因回调的等待阻塞其他 provider 的读写。
#[derive(Default)]
struct Inner {
    credentials: Mutex<IndexMap<String, Credential>>,
    /// 每个 provider 一把串行门；条目只在首次使用时创建（清理对调用方不可观察）。
    /// 本实现不做回收，见模块文档「注意事项」。
    gates: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

/// 默认内存凭证存储。App 注入持久化实现；见 [`CredentialStore`]。
///
/// 写操作按 provider id 通过 promise-chain（Rust：异步互斥锁）串行化。
///
/// `Clone` 是浅拷贝：所有克隆共享同一份 `Arc<Inner>` 状态，克隆出的句柄对同一张表读写；
/// `Debug` 只输出结构体名与 `..`，不打印凭证内容，避免日志泄漏密钥。
///
/// TS: `src/auth/credential-store.ts#InMemoryCredentialStore`
#[derive(Clone, Default)]
pub struct InMemoryCredentialStore {
    inner: Arc<Inner>,
}

impl fmt::Debug for InMemoryCredentialStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemoryCredentialStore")
            .finish_non_exhaustive()
    }
}

impl InMemoryCredentialStore {
    /// 新建空存储。
    ///
    /// 等价于 `Self::default()`；每个新实例都有独立的 `Inner`（与 `Clone` 的共享语义不同）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 取该 provider 的串行门；首次使用时创建。
    ///
    /// 返回 `Arc` 的克隆，使调用方在释放 `gates` 表的锁之后仍能继续持有门；两个写操作
    /// 只要拿到同一个 `Arc` 就互斥。
    fn gate(&self, provider_id: &str) -> Arc<AsyncMutex<()>> {
        let mut gates = self
            .inner
            .gates
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Arc::clone(
            gates
                .entry(provider_id.to_string())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        )
    }

    /// 按 provider 串行执行任务。
    ///
    /// # 阶段
    /// 1. `operation_signal` 把 `None` 变成永不中止的操作局部 signal，后续代码不必再处理
    ///    `Option`；传入的 signal 则按值克隆一份，与调用方共享同一个取消状态。
    /// 2. 取该 provider 的门：`select!` 让等门可被中止，中止时返回 `abort_error`。
    /// 3. 拿到门后再查一次中止，见模块文档第 2 条（`select!` 无偏向）。
    /// 4. 执行任务并返回其结果。
    ///
    /// 外层再与同一个 signal 做 `race_with_abort_signal`：调用方在中止时立刻拿到
    /// `AbortError`，任务继续跑完。因此返回值与副作用可能不一致（模块文档第 3 条）。
    ///
    /// # 泛型约束
    /// `T`/`F`/`Fut` 都要求 `Send + 'static`：任务会被 `tokio::spawn` 到运行时，不能借用
    /// 调用栈上的临时值（`CredentialModifyFn` 已经把这层约束表达在回调类型上）。调用点因此
    /// 必须处于 Tokio 运行时内，否则 `tokio::spawn` 会 panic。
    ///
    /// TS: `credential-store.ts#enqueue`
    async fn enqueue<T, F, Fut>(
        &self,
        provider_id: &str,
        task: F,
        options: &AuthOperationOptions,
    ) -> Result<T, AuthError>
    where
        T: Send + 'static,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, AuthError>> + Send + 'static,
    {
        let signal = operation_signal(options.signal.clone());
        let gate = self.gate(provider_id);
        let queued_signal = signal.clone();
        let token = queued_signal.token();
        let queued = async move {
            // `await previous.catch(() => {})` + `signal.throwIfAborted()`：等锁可中止。
            let _guard = tokio::select! {
                guard = gate.lock() => guard,
                _ = token.cancelled() => return Err(abort_error(&queued_signal)),
            };
            // `select!` 无偏向：signal 已中止且门恰好空闲时，可能先选中取锁分支。这里再查
            // 一次，保证被取消的排队任务不会执行（TS 的 `throwIfAborted()` 在同一位置：
            // `await previous` 之后、`task()` 之前）。
            throw_if_aborted(&queued_signal)?;
            // `_guard` 存活到 `task()` 结束，串行化因此覆盖整个任务（包括回调的 await）。
            task().await
        };
        // `raceWithAbortSignal(queued, signal)`：signal 已中止时立即拒绝，queued 继续跑完。
        match race_with_abort_signal(queued, &signal).await {
            Ok(result) => result,
            Err(error) => Err(AuthError::from_ai_error(&error)),
        }
    }
}

#[async_trait]
impl CredentialStore for InMemoryCredentialStore {
    /// 读取一份凭证的副本；不进串行门，可能看到进行中写入之前的旧值。
    ///
    /// 返回 `Ok(None)` 表示该 provider 没有存储凭证（不是错误）。`signal` 只在入口检查一次：
    /// 本方法没有 await 点，不存在中途取消的窗口。
    async fn read(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        if let Some(signal) = options.signal.as_ref() {
            throw_if_aborted(signal)?;
        }
        Ok(self
            .inner
            .credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(provider_id)
            .cloned())
    }

    /// 列出所有凭证的元数据（provider id + 类型），不暴露任何密钥；顺序为首次插入顺序。
    ///
    /// 与 `read` 一样不过门，`signal` 只在入口检查一次。
    async fn list(&self, options: AuthOperationOptions) -> Result<Vec<CredentialInfo>, AuthError> {
        if let Some(signal) = options.signal.as_ref() {
            throw_if_aborted(signal)?;
        }
        let credentials = self
            .inner
            .credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Ok(credentials
            .iter()
            .map(|(provider_id, credential)| CredentialInfo {
                provider_id: provider_id.clone(),
                r#type: credential.type_(),
            })
            .collect())
    }

    /// 串行化的 read-modify-write：回调看到当前凭证（`None` 表示未存储），返回 `Some` 才写入，
    /// 返回 `None` 则保持原值。
    ///
    /// 返回值是写后凭证：`f` 返回 `Some(next)` 时是 `next`，否则是读到的 `current`。
    /// 回调之后的第二次 `throw_if_aborted` 在写入前执行：被中止时回调结果被丢弃且**不写入**。
    async fn modify(
        &self,
        provider_id: &str,
        f: CredentialModifyFn,
        options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        let inner = Arc::clone(&self.inner);
        let provider = provider_id.to_string();
        let write_signal = options.signal.clone();
        let task = move || {
            let inner = Arc::clone(&inner);
            let provider = provider.clone();
            let write_signal = write_signal.clone();
            async move {
                // 只读到克隆结束就释放锁；`f` 可能在 await 中运行很久，绝不能持锁等待。
                let current = inner
                    .credentials
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .get(&provider)
                    .cloned();
                let next = f(current.clone()).await?;
                // `options?.signal?.throwIfAborted()`：`fn` 之后、写入之前再检查一次。
                if let Some(signal) = write_signal.as_ref() {
                    throw_if_aborted(signal)?;
                }
                // `None` 表示保持原值：不写入，返回值回退到读到的 current。
                if let Some(next) = next.as_ref() {
                    inner
                        .credentials
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .insert(provider, next.clone());
                }
                Ok(next.or(current))
            }
        };
        self.enqueue(provider_id, task, &options).await
    }

    /// 删除该 provider 的凭证（logout）；与 `modify` 共用同一把串行门。
    ///
    /// 幂等：删除不存在的 provider 是 no-op，不返回错误。
    async fn delete(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<(), AuthError> {
        let inner = Arc::clone(&self.inner);
        let provider = provider_id.to_string();
        let task = move || {
            let inner = Arc::clone(&inner);
            let provider = provider.clone();
            async move {
                // `shift_remove` 保持其余条目的插入顺序（`list` 的可观察语义）；
                // `swap_remove` 会把末尾条目挪进空位。不存在时是 no-op。
                inner
                    .credentials
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .shift_remove(&provider);
                Ok(())
            }
        };
        self.enqueue(provider_id, task, &options).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::auth::types::{ApiKeyCredential, OAuthCredential};
    use crate::utils::abort::AbortScope;

    fn api_key(key: &str) -> Credential {
        Credential::ApiKey(ApiKeyCredential {
            key: Some(key.to_string()),
            env: None,
            extra: IndexMap::new(),
        })
    }

    fn boxed_modify(
        f: impl FnOnce(Option<Credential>) -> crate::auth::types::CredentialModifyFuture
        + Send
        + 'static,
    ) -> CredentialModifyFn {
        Box::new(f)
    }

    #[tokio::test]
    async fn enumerates_credential_metadata_without_exposing_secrets() {
        let credentials = InMemoryCredentialStore::new();
        credentials
            .modify(
                "api-provider",
                boxed_modify(|_| Box::pin(async { Ok(Some(api_key("secret"))) })),
                AuthOperationOptions::default(),
            )
            .await
            .expect("modify");
        credentials
            .modify(
                "oauth-provider",
                boxed_modify(|_| {
                    Box::pin(async {
                        Ok(Some(Credential::OAuth(OAuthCredential::new(
                            "access", "refresh", 1.0,
                        ))))
                    })
                }),
                AuthOperationOptions::default(),
            )
            .await
            .expect("modify");

        let list = credentials
            .list(AuthOperationOptions::default())
            .await
            .expect("list");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].provider_id, "api-provider");
        assert_eq!(list[0].r#type, crate::auth::types::AuthType::ApiKey);
        assert_eq!(list[1].provider_id, "oauth-provider");
        assert_eq!(list[1].r#type, crate::auth::types::AuthType::OAuth);
    }

    #[tokio::test]
    async fn modify_sees_current_credential_and_none_keeps_it() {
        let credentials = InMemoryCredentialStore::new();
        credentials
            .modify(
                "p1",
                boxed_modify(|_| Box::pin(async { Ok(Some(api_key("first"))) })),
                AuthOperationOptions::default(),
            )
            .await
            .expect("modify");
        let observed = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&observed);
        let result = credentials
            .modify(
                "p1",
                boxed_modify(move |current| {
                    *slot.lock().unwrap() = current.clone();
                    Box::pin(async { Ok(None) })
                }),
                AuthOperationOptions::default(),
            )
            .await
            .expect("modify");
        assert_eq!(observed.lock().unwrap().as_ref(), Some(&api_key("first")));
        assert_eq!(result, Some(api_key("first")));
        assert_eq!(
            credentials
                .read("p1", AuthOperationOptions::default())
                .await
                .unwrap(),
            Some(api_key("first"))
        );
    }

    #[tokio::test]
    async fn serializes_writes_per_provider() {
        let credentials = InMemoryCredentialStore::new();
        let first_started = Arc::new(tokio::sync::Notify::new());
        let release_first = Arc::new(tokio::sync::Notify::new());
        let started = Arc::clone(&first_started);
        let release = Arc::clone(&release_first);

        let first = {
            let credentials = credentials.clone();
            tokio::spawn(async move {
                credentials
                    .modify(
                        "p1",
                        boxed_modify(move |_| {
                            Box::pin(async move {
                                started.notify_one();
                                release.notified().await;
                                Ok(Some(api_key("first")))
                            })
                        }),
                        AuthOperationOptions::default(),
                    )
                    .await
            })
        };
        first_started.notified().await;

        let seen = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&seen);
        let second = {
            let credentials = credentials.clone();
            tokio::spawn(async move {
                credentials
                    .modify(
                        "p1",
                        boxed_modify(move |current| {
                            *slot.lock().unwrap() = current;
                            Box::pin(async { Ok(Some(api_key("second"))) })
                        }),
                        AuthOperationOptions::default(),
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert!(
            seen.lock().unwrap().is_none(),
            "second write must wait for the first"
        );
        release_first.notify_one();
        first.await.unwrap().expect("first");
        second.await.unwrap().expect("second");
        // 第二个 modify 看到的是第一个写后的凭证。
        assert_eq!(seen.lock().unwrap().as_ref(), Some(&api_key("first")));
        assert_eq!(
            credentials
                .read("p1", AuthOperationOptions::default())
                .await
                .unwrap(),
            Some(api_key("second"))
        );
    }

    #[tokio::test]
    async fn cancels_queued_credential_mutations_without_running_them_later() {
        let credentials = InMemoryCredentialStore::new();
        let first_started = Arc::new(tokio::sync::Notify::new());
        let release_first = Arc::new(tokio::sync::Notify::new());
        let started = Arc::clone(&first_started);
        let release = Arc::clone(&release_first);

        let first = {
            let credentials = credentials.clone();
            tokio::spawn(async move {
                credentials
                    .modify(
                        "p1",
                        boxed_modify(move |_| {
                            Box::pin(async move {
                                started.notify_one();
                                release.notified().await;
                                Ok(Some(api_key("first")))
                            })
                        }),
                        AuthOperationOptions::default(),
                    )
                    .await
            })
        };
        first_started.notified().await;

        let second_ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&second_ran);
        let controller = AbortScope::new();
        let second = {
            let credentials = credentials.clone();
            let signal = controller.clone();
            tokio::spawn(async move {
                credentials
                    .modify(
                        "p1",
                        boxed_modify(move |_| {
                            flag.store(true, Ordering::SeqCst);
                            Box::pin(async { Ok(Some(api_key("second"))) })
                        }),
                        AuthOperationOptions {
                            signal: Some(signal),
                        },
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;
        controller.abort();
        let error = second.await.unwrap().unwrap_err();
        assert_eq!(error.name, "AbortError");

        release_first.notify_one();
        first.await.unwrap().expect("first");
        tokio::task::yield_now().await;

        assert!(!second_ran.load(Ordering::SeqCst));
        assert_eq!(
            credentials
                .read("p1", AuthOperationOptions::default())
                .await
                .unwrap(),
            Some(api_key("first"))
        );
    }

    #[tokio::test]
    async fn delete_is_serialized_and_idempotent() {
        let credentials = InMemoryCredentialStore::new();
        credentials
            .modify(
                "p1",
                boxed_modify(|_| Box::pin(async { Ok(Some(api_key("first"))) })),
                AuthOperationOptions::default(),
            )
            .await
            .unwrap();
        credentials
            .delete("p1", AuthOperationOptions::default())
            .await
            .unwrap();
        credentials
            .delete("p1", AuthOperationOptions::default())
            .await
            .unwrap();
        assert!(
            credentials
                .read("p1", AuthOperationOptions::default())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn read_and_list_honor_the_signal() {
        let credentials = InMemoryCredentialStore::new();
        let signal = AbortScope::new();
        signal.abort();
        let options = AuthOperationOptions {
            signal: Some(signal),
        };
        assert_eq!(
            credentials
                .read("p1", options.clone())
                .await
                .unwrap_err()
                .name,
            "AbortError"
        );
        assert_eq!(
            credentials.list(options).await.unwrap_err().name,
            "AbortError"
        );
    }

    #[tokio::test]
    async fn failures_propagate_and_do_not_block_later_writes() {
        let credentials = InMemoryCredentialStore::new();
        let error = credentials
            .modify(
                "p1",
                boxed_modify(|_| Box::pin(async { Err(AuthError::new("boom")) })),
                AuthOperationOptions::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.message, "boom");
        credentials
            .modify(
                "p1",
                boxed_modify(|_| Box::pin(async { Ok(Some(api_key("after"))) })),
                AuthOperationOptions::default(),
            )
            .await
            .expect("later write");
        assert_eq!(
            credentials
                .read("p1", AuthOperationOptions::default())
                .await
                .unwrap(),
            Some(api_key("after"))
        );
    }
}
