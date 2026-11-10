//! `test/model-catalog-refresh.test.ts` 的 Rust 等价物（3 个用例）。
//!
//! TS 的协调器接受 `Pick<ModelRuntime, "refresh">` 并通过 `vi.fn()` 观察调用；Rust 侧用
//! 实现 [`ModelCatalogRuntime`] 的假运行时复刻同一组可观察保证。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use kiana_ai::auth::types::CredentialStore;
use kiana_ai::error::AiError;
use kiana_ai::models::ModelsRefreshResult;
use kiana_ai::utils::abort::AbortScope;
use kiana_config::auth::models_store::InMemoryCodingAgentModelsStore;
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_interactive::model_catalog_refresh::{
    ModelCatalogRefreshFuture, ModelCatalogRuntime, refresh_model_catalogs,
};
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// `createDeferred` 的 gate：`refresh_catalogs` 等到 sender 发送后才返回。
struct FakeState {
    id: usize,
    refresh_count: usize,
    signals: Vec<AbortScope>,
    gate: Option<oneshot::Receiver<()>>,
    result: Result<ModelsRefreshResult, AiError>,
}

#[derive(Clone)]
struct FakeRuntime {
    state: Arc<Mutex<FakeState>>,
}

impl FakeRuntime {
    fn new(gate: Option<oneshot::Receiver<()>>) -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
        FakeRuntime {
            state: Arc::new(Mutex::new(FakeState {
                id: NEXT_ID.fetch_add(1, Ordering::SeqCst),
                refresh_count: 0,
                signals: Vec::new(),
                gate,
                result: Ok(ModelsRefreshResult::default()),
            })),
        }
    }

    fn set_gate(&self, gate: Option<oneshot::Receiver<()>>) {
        self.state.lock().unwrap().gate = gate;
    }

    fn refresh_count(&self) -> usize {
        self.state.lock().unwrap().refresh_count
    }

    fn signals(&self) -> Vec<AbortScope> {
        self.state.lock().unwrap().signals.clone()
    }
}

impl ModelCatalogRuntime for FakeRuntime {
    fn identity(&self) -> usize {
        self.state.lock().unwrap().id
    }

    fn refresh_catalogs(&self, signal: Option<AbortScope>) -> ModelCatalogRefreshFuture<'_> {
        Box::pin(async move {
            let (gate, result) = {
                let mut state = self.state.lock().unwrap();
                state.refresh_count += 1;
                if let Some(signal) = signal {
                    state.signals.push(signal);
                }
                (state.gate.take(), state.result.clone())
            };
            if let Some(gate) = gate {
                let _ = gate.await;
            }
            result
        })
    }
}

/// 把 future 推进到第一次 `Pending`（登记为等待者），不允许它直接完成。
async fn poll_pending<F: Future>(future: Pin<&mut F>) {
    let mut future = future;
    std::future::poll_fn(move |context| match future.as_mut().poll(context) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("future completed before the shared refresh settled"),
    })
    .await;
}

/// 轮询直到条件成立（对应 `vi.waitFor`）。
async fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("condition not met");
}

#[tokio::test]
async fn shares_one_runtime_refresh_between_concurrent_callers() {
    let (sender, gate) = oneshot::channel();
    let runtime = FakeRuntime::new(Some(gate));
    let first_controller = CancellationToken::new();
    let second_controller = CancellationToken::new();

    let first = refresh_model_catalogs(&runtime, Some(&first_controller));
    let second = refresh_model_catalogs(&runtime, Some(&second_controller));
    tokio::pin!(first);
    tokio::pin!(second);
    poll_pending(first.as_mut()).await;
    poll_pending(second.as_mut()).await;

    wait_until(|| runtime.refresh_count() == 1).await;
    sender.send(()).unwrap();
    let (first_result, second_result) = tokio::join!(first, second);
    assert!(first_result.is_ok());
    assert!(second_result.is_ok());
    assert_eq!(runtime.refresh_count(), 1);
}

#[tokio::test]
async fn keeps_the_shared_refresh_alive_when_one_caller_stops_waiting() {
    let (sender, gate) = oneshot::channel();
    let runtime = FakeRuntime::new(Some(gate));
    let first_controller = CancellationToken::new();
    let second_controller = CancellationToken::new();

    let first = refresh_model_catalogs(&runtime, Some(&first_controller));
    let second = refresh_model_catalogs(&runtime, Some(&second_controller));
    tokio::pin!(first);
    tokio::pin!(second);
    poll_pending(first.as_mut()).await;
    poll_pending(second.as_mut()).await;

    first_controller.cancel();
    let first_result = first.await;
    let error = first_result.expect_err("aborted caller rejects");
    assert_eq!(error.name, "AbortError");

    wait_until(|| !runtime.signals().is_empty()).await;
    assert!(!runtime.signals()[0].is_aborted());

    sender.send(()).unwrap();
    assert!(second.await.is_ok());
}

#[tokio::test]
async fn aborts_an_abandoned_refresh_and_allows_a_later_refresh_to_start() {
    let (first_sender, first_gate) = oneshot::channel();
    let runtime = FakeRuntime::new(Some(first_gate));
    let first_controller = CancellationToken::new();
    let first = refresh_model_catalogs(&runtime, Some(&first_controller));
    tokio::pin!(first);
    poll_pending(first.as_mut()).await;

    first_controller.cancel();
    let first_result = first.await;
    assert_eq!(
        first_result.expect_err("aborted caller rejects").name,
        "AbortError"
    );
    wait_until(|| !runtime.signals().is_empty()).await;
    wait_until(|| runtime.signals()[0].is_aborted()).await;

    let (second_sender, second_gate) = oneshot::channel();
    runtime.set_gate(Some(second_gate));
    let second_controller = CancellationToken::new();
    let second = refresh_model_catalogs(&runtime, Some(&second_controller));
    tokio::pin!(second);
    poll_pending(second.as_mut()).await;
    wait_until(|| runtime.refresh_count() == 2).await;
    second_controller.cancel();
    assert_eq!(
        second.await.expect_err("aborted caller rejects").name,
        "AbortError"
    );

    drop(first_sender);
    drop(second_sender);
}

/// 协调器用运行时身份作 `WeakMap` 键：同一运行时的克隆必须共享身份，不同运行时必须不同。
///
/// 这条契约由 `kiana_config::models::runtime::ModelRuntime::identity` 提供（本单元对
/// kiana-config 的唯一非自有改动，见报告）。
#[tokio::test]
async fn model_runtime_identity_is_stable_across_clones() {
    let options = || CreateModelRuntimeOptions {
        credentials: Some(
            Arc::new(AuthStorage::in_memory(auth_data_from_value(json!({}))))
                as Arc<dyn CredentialStore>,
        ),
        models_path: ModelsPathOption::Disabled,
        models_store: Some(Arc::new(InMemoryCodingAgentModelsStore::new())),
        allow_model_network: false,
        ..Default::default()
    };
    let runtime = ModelRuntime::create(options()).await.unwrap();
    let clone = runtime.clone();
    let other = ModelRuntime::create(options()).await.unwrap();
    assert_eq!(runtime.identity(), clone.identity());
    assert_ne!(runtime.identity(), other.identity());
}
