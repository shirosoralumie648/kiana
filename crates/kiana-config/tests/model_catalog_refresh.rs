//! `model-catalog-refresh.test.ts` 的运行时等价物（3 个用例）。
//!
//! TS 文件测试的是 `modes/interactive/model-catalog-refresh.ts` 的调用方共享逻辑，该 helper
//! 不在本单元范围内。这里按 `spec/ca-config-models.md` §2.20.13 的 `refresh()` 语义移植同一组
//! 可观察保证：中止的调用报告 `aborted`、已发起的 provider 刷新收到取消信号、放弃后可以重新
//! 启动刷新。

#[path = "fixtures/model_runtime/mod.rs"]
mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use kiana_ai::models::{
    ModelsPublication, ModelsPublicationPersist, ModelsRefreshOptions, ModelsStoreEntry,
    RefreshModelsContext,
};
use kiana_ai::types::AnyModel;
use kiana_ai::utils::abort::AbortScope;
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use serde_json::json;
use support::*;
use tokio::sync::Semaphore;

fn memory_auth() -> Arc<AuthStorage> {
    Arc::new(AuthStorage::in_memory(auth_data_from_value(json!({}))))
}

async fn runtime_with_catalog_provider(
    provider: Arc<dyn kiana_ai::models::Provider>,
) -> Arc<ModelRuntime> {
    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(memory_auth() as Arc<dyn kiana_ai::auth::types::CredentialStore>),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            ..Default::default()
        })
        .await
        .expect("runtime"),
    );
    let provider_id = provider.id().to_string();
    runtime
        .register_native_provider(provider)
        .expect("register provider");
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec![provider_id]),
            ..Default::default()
        })
        .await
        .expect("initial refresh");
    runtime
}

/// apiKey auth：总是已配置，避免可用性检查引入额外噪声。
fn always_configured_auth() -> kiana_ai::auth::types::ProviderAuth {
    let auth = TestApiKeyAuth::new("API key").with_resolve(Arc::new(|_credential| {
        Box::pin(async {
            Ok(Some(kiana_ai::auth::types::AuthResult::new(
                kiana_ai::auth::types::ModelAuth {
                    api_key: Some("key".to_string()),
                    ..Default::default()
                },
            )))
        })
    }));
    Arc::new(auth).provider_auth()
}

#[tokio::test]
async fn refresh_with_a_pre_aborted_signal_reports_aborted() {
    let seen_allow_network = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&seen_allow_network);
    let provider = TestProvider::dynamic("catalog", always_configured_auth())
        .with_refresh_models(Arc::new(move |context| {
            let flag = Arc::clone(&flag);
            Box::pin(async move {
                if !context.allow_network {
                    flag.store(false, Ordering::SeqCst);
                }
                Ok(())
            })
        }))
        .into_provider();
    let runtime = runtime_with_catalog_provider(provider).await;

    let controller = AbortScope::new();
    controller.abort();
    let result = runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["catalog".to_string()]),
            signal: Some(controller),
            ..Default::default()
        })
        .await
        .expect("refresh");
    assert!(result.aborted);
}

#[tokio::test]
async fn an_abandoned_refresh_reports_aborted_and_observes_cancellation() {
    let started = Arc::new(Semaphore::new(0));
    let started_refresh = Arc::clone(&started);
    let signal_seen: Arc<Mutex<Option<AbortScope>>> = Arc::new(Mutex::new(None));
    let signal_slot = Arc::clone(&signal_seen);
    let provider = TestProvider::dynamic("catalog", always_configured_auth())
        .with_refresh_models(Arc::new(move |context| {
            let started = Arc::clone(&started_refresh);
            let signal_slot = Arc::clone(&signal_slot);
            Box::pin(async move {
                if !context.allow_network {
                    return Ok(());
                }
                *signal_slot.lock().unwrap() = Some(context.signal.clone());
                started.add_permits(1);
                context.signal.cancelled().await;
                Ok(())
            })
        }))
        .into_provider();
    let runtime = runtime_with_catalog_provider(provider).await;

    let controller = AbortScope::new();
    let refresh_runtime = Arc::clone(&runtime);
    let signal = controller.clone();
    let refresh = tokio::spawn(async move {
        refresh_runtime
            .refresh(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec!["catalog".to_string()]),
                signal: Some(signal),
                ..Default::default()
            })
            .await
    });
    let permit = tokio::time::timeout(Duration::from_secs(5), started.acquire())
        .await
        .expect("provider refresh started")
        .expect("semaphore");
    permit.forget();

    controller.abort();
    let result = tokio::time::timeout(Duration::from_secs(5), refresh)
        .await
        .expect("refresh resolves")
        .expect("join")
        .expect("refresh result");
    assert!(result.aborted);
    let provider_signal = signal_seen.lock().unwrap().clone().expect("signal");
    assert!(provider_signal.is_aborted());
}

#[tokio::test]
async fn a_later_refresh_starts_after_an_abandoned_one() {
    let started = Arc::new(Semaphore::new(0));
    let started_refresh = Arc::clone(&started);
    let stall = Arc::new(AtomicBool::new(true));
    let stall_refresh = Arc::clone(&stall);
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_refresh = Arc::clone(&calls);
    let provider = TestProvider::dynamic("catalog", always_configured_auth())
        .with_refresh_models(Arc::new(move |context| {
            let started = Arc::clone(&started_refresh);
            let stall = Arc::clone(&stall_refresh);
            let calls = Arc::clone(&calls_refresh);
            Box::pin(async move {
                if !context.allow_network {
                    return Ok(());
                }
                calls.fetch_add(1, Ordering::SeqCst);
                if stall.load(Ordering::SeqCst) {
                    started.add_permits(1);
                    context.signal.cancelled().await;
                }
                Ok(())
            })
        }))
        .into_provider();
    let runtime = runtime_with_catalog_provider(provider).await;

    let controller = AbortScope::new();
    let refresh_runtime = Arc::clone(&runtime);
    let signal = controller.clone();
    let first = tokio::spawn(async move {
        refresh_runtime
            .refresh(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec!["catalog".to_string()]),
                signal: Some(signal),
                ..Default::default()
            })
            .await
    });
    let permit = tokio::time::timeout(Duration::from_secs(5), started.acquire())
        .await
        .expect("provider refresh started")
        .expect("semaphore");
    permit.forget();
    controller.abort();
    let first_result = tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .expect("first refresh resolves")
        .expect("join")
        .expect("refresh result");
    assert!(first_result.aborted);

    stall.store(false, Ordering::SeqCst);
    let second_result = runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(true),
            providers: Some(vec!["catalog".to_string()]),
            ..Default::default()
        })
        .await
        .expect("second refresh");
    assert!(!second_result.aborted);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// TS 的 `void this.refresh({ allowNetwork: false })` 与调用方执行环境无关：在没有 tokio
/// 运行时的纯 `std::thread` 里做注册，也必须最终重读 models.json 并刷新快照。
#[tokio::test]
async fn registration_outside_a_tokio_runtime_still_reloads_models_json() {
    let dir = tempfile::tempdir().expect("temp dir");
    let models_path = dir.path().join("models.json");
    std::fs::write(
        &models_path,
        serde_json::to_string(&json!({"providers": {}})).unwrap(),
    )
    .expect("write models.json");
    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(memory_auth() as Arc<dyn kiana_ai::auth::types::CredentialStore>),
            models_path: ModelsPathOption::Path(models_path.to_string_lossy().into_owned()),
            allow_model_network: false,
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await
        .expect("runtime"),
    );
    assert!(runtime.get_models(Some("background")).is_empty());

    std::fs::write(
        &models_path,
        serde_json::to_string(&json!({
            "providers": {
                "background": {
                    "baseUrl": "https://background.test/v1",
                    "api": "openai-completions",
                    "models": [{
                        "id": "background-model",
                        "name": "Background",
                        "contextWindow": 1000,
                        "maxTokens": 100
                    }]
                }
            }
        }))
        .unwrap(),
    )
    .expect("rewrite models.json");

    // 纯 std::thread：`Handle::try_current()` 为 Err。
    let trigger = Arc::clone(&runtime);
    std::thread::spawn(move || {
        assert!(tokio::runtime::Handle::try_current().is_err());
        trigger.unregister_provider("not-registered");
    })
    .join()
    .expect("registration thread");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !runtime.get_models(Some("background")).is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "background refresh was skipped without a tokio runtime"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// provider 刷新发布的模型（写入 models store 的 `models-store.json`）。
fn publishing_provider(
    provider_id: &str,
    model: kiana_ai::types::Model,
) -> Arc<dyn kiana_ai::models::Provider> {
    TestProvider::dynamic(provider_id, always_configured_auth())
        .with_refresh_models(Arc::new(move |context: RefreshModelsContext| {
            let model = model.clone();
            let publish = context.publish;
            Box::pin(async move {
                publish(ModelsPublication {
                    persist: ModelsPublicationPersist::Entry(ModelsStoreEntry {
                        models: vec![AnyModel::Chat(model)],
                        raw_models: None,
                        last_modified: None,
                        checked_at: None,
                        etag: None,
                        extra: IndexMap::new(),
                    }),
                    update: None,
                })
                .await
                .map(|_| ())
            })
        }))
        .into_provider()
}

/// 以给定 models 路径创建 runtime，注册一个会发布的 provider 并刷新一次。
async fn refresh_with_models_path(
    models_path: ModelsPathOption,
    store_path: &std::path::Path,
) -> Arc<ModelRuntime> {
    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(memory_auth() as Arc<dyn kiana_ai::auth::types::CredentialStore>),
            models_path,
            models_store_path: Some(store_path.to_string_lossy().into_owned()),
            allow_model_network: false,
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await
        .expect("runtime"),
    );
    runtime
        .register_native_provider(publishing_provider(
            "store-probe",
            chat_model("store-probe", "published"),
        ))
        .expect("register provider");
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["store-probe".to_string()]),
            ..Default::default()
        })
        .await
        .expect("refresh");
    runtime
}

/// 回归：TS 的 store 选择是 `modelsPath ? new FileModelsStore(...) : new
/// InMemoryCodingAgentModelsStore()`——`modelsPath` 为空串时是 falsy，目录刷新保持在内存里，
/// 不写 `models-store.json`（`ModelConfig.load("")` 同样当作未给出）。
#[tokio::test]
async fn empty_models_path_selects_the_in_memory_store() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store_path = dir.path().join("models-store.json");

    refresh_with_models_path(ModelsPathOption::Path(String::new()), &store_path).await;

    assert!(
        !store_path.exists(),
        "an empty modelsPath must not persist the catalog to disk"
    );
}

/// 对照：非空 modelsPath 时同一发布确实落盘（证明上面的断言不是空转）。
#[tokio::test]
async fn non_empty_models_path_selects_the_file_store() {
    let dir = tempfile::tempdir().expect("temp dir");
    let models_path = dir.path().join("models.json");
    let store_path = dir.path().join("nested").join("models-store.json");

    refresh_with_models_path(
        ModelsPathOption::Path(models_path.to_string_lossy().into_owned()),
        &store_path,
    )
    .await;

    assert!(
        store_path.exists(),
        "a non-empty modelsPath persists the catalog next to models.json"
    );
}
