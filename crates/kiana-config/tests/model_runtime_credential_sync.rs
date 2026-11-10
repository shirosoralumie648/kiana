//! `model-runtime-credential-sync.test.ts` 的移植（10 个用例）。

// `ModelRuntimeError`/`CredentialSynchronizationError` 的 Err 变体由冻结类型决定其大小。
#![allow(clippy::result_large_err)]

#[path = "fixtures/model_runtime/mod.rs"]
mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{
    ApiKeyCredential, AuthCheck, AuthError, AuthInteraction, AuthOperationOptions, AuthType,
    Credential, CredentialInfo, CredentialModifyFn, CredentialStore, LoginOptions,
};
use kiana_ai::error::ModelsErrorCode;
use kiana_ai::models::{Models, ModelsRefreshOptions, Provider, ai_error_from_models_error};
use kiana_ai::utils::abort::AbortScope;
use kiana_ai::utils::models_error::{TsModelsErrorCode, models_error};
use kiana_config::models::composer::AuthStatusSource;
use kiana_config::models::runtime::{
    CreateModelRuntimeOptions, CredentialSynchronizationError, CredentialSynchronizationOperation,
    ModelRuntime, ModelRuntimeError, ModelsPathOption,
};
use support::*;
use tokio::sync::Semaphore;

fn key_of(credential: &Credential) -> Option<&str> {
    match credential {
        Credential::ApiKey(api_key) => api_key.key.as_deref(),
        Credential::OAuth(_) => None,
    }
}

fn api_key_auth(auth: TestApiKeyAuth) -> kiana_ai::auth::types::ProviderAuth {
    Arc::new(auth).provider_auth()
}

/// 存储凭证则报告的 check（TS `credential ? { type: "api_key", source } : undefined`）。
fn stored_check(source: &'static str) -> CheckFn {
    Arc::new(move |credential| {
        Box::pin(async move {
            Ok(credential.and_then(|credential| {
                credential.key.map(|_| AuthCheck {
                    source: Some(source.to_string()),
                    r#type: AuthType::ApiKey,
                })
            }))
        })
    })
}

/// 同步 gate（permit 可在等待前预存）。
fn gate() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(0))
}

/// 等待 gate（5 秒超时，避免串行化实现导致测试挂死）。
async fn wait_gate(gate: &Semaphore) {
    let permit = tokio::time::timeout(Duration::from_secs(5), gate.acquire())
        .await
        .expect("gate timed out")
        .expect("gate closed");
    permit.forget();
}

/// `runtimeWithProvider(registered, credentials)`。
async fn runtime_with_provider(
    provider: Arc<dyn Provider>,
    credentials: Arc<dyn CredentialStore>,
) -> Arc<ModelRuntime> {
    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(credentials),
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
        .expect("refresh");
    runtime
}

/// `runtime.login(provider, "api_key", { prompt, notify })`。
async fn login_key(
    runtime: &ModelRuntime,
    provider_id: &str,
    interaction: &TestInteraction,
) -> Result<Credential, ModelRuntimeError> {
    runtime
        .login(
            provider_id,
            AuthType::ApiKey,
            interaction,
            LoginOptions::default(),
        )
        .await
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn publishes_locally_consistent_availability_before_login_and_logout_resolve() {
    let credentials = Arc::new(kiana_config::auth::storage::AuthStorage::in_memory(
        kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
    ));
    let auth = TestApiKeyAuth::new("API key")
        .with_login(Arc::new(|| {
            Box::pin(async {
                Ok(ApiKeyCredential {
                    key: Some("dynamic-key".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }))
        .with_check(stored_check("stored"));
    let provider = TestProvider::dynamic("dynamic", api_key_auth(auth)).into_provider();
    let runtime = runtime_with_provider(
        provider,
        Arc::clone(&credentials) as Arc<dyn CredentialStore>,
    )
    .await;
    let interaction = TestInteraction::new();

    let credential = login_key(&runtime, "dynamic", &interaction)
        .await
        .expect("login");
    assert_eq!(key_of(&credential), Some("dynamic-key"));
    assert!(runtime.has_configured_auth("dynamic"));
    assert!(
        runtime
            .get_available_snapshot()
            .iter()
            .any(|model| model.id == "dynamic")
    );
    let stored = credentials
        .read("dynamic", AuthOperationOptions::default())
        .await
        .expect("read")
        .expect("stored");
    assert_eq!(key_of(&stored), Some("dynamic-key"));

    runtime
        .logout("dynamic", AuthOperationOptions::default())
        .await
        .expect("logout");
    assert!(!runtime.has_configured_auth("dynamic"));
    assert!(
        !runtime
            .get_available_snapshot()
            .iter()
            .any(|model| model.provider == "dynamic")
    );
    assert!(
        credentials
            .read("dynamic", AuthOperationOptions::default())
            .await
            .expect("read")
            .is_none()
    );
}

#[tokio::test]
async fn orders_same_provider_credential_operations_through_local_synchronization() {
    let started = gate();
    let blocked = gate();
    let started_login = Arc::clone(&started);
    let blocked_login = Arc::clone(&blocked);
    let auth = TestApiKeyAuth::new("API key")
        .with_login(Arc::new(move || {
            let started = Arc::clone(&started_login);
            let blocked = Arc::clone(&blocked_login);
            Box::pin(async move {
                started.add_permits(1);
                wait_gate(&blocked).await;
                Ok(ApiKeyCredential {
                    key: Some("ordered-key".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }))
        .with_check(stored_check("stored"));
    let provider = TestProvider::dynamic("ordered", api_key_auth(auth)).into_provider();
    let credentials = Arc::new(kiana_config::auth::storage::AuthStorage::in_memory(
        kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
    ));
    let runtime = runtime_with_provider(
        provider,
        Arc::clone(&credentials) as Arc<dyn CredentialStore>,
    )
    .await;

    let login_runtime = Arc::clone(&runtime);
    let login = tokio::spawn(async move {
        let interaction = TestInteraction::new();
        login_key(&login_runtime, "ordered", &interaction).await
    });
    wait_gate(&started).await;

    let logout_runtime = Arc::clone(&runtime);
    let logout = tokio::spawn(async move {
        logout_runtime
            .logout("ordered", AuthOperationOptions::default())
            .await
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        credentials
            .read("ordered", AuthOperationOptions::default())
            .await
            .expect("read")
            .is_none()
    );

    blocked.add_permits(1);
    let login_result = login.await.expect("join").expect("login");
    assert_eq!(key_of(&login_result), Some("ordered-key"));
    logout.await.expect("join").expect("logout");

    assert!(
        credentials
            .read("ordered", AuthOperationOptions::default())
            .await
            .expect("read")
            .is_none()
    );
    assert!(!runtime.has_configured_auth("ordered"));
}

#[tokio::test]
async fn allows_different_providers_to_run_credential_flows_concurrently() {
    let first_started = gate();
    let second_started = gate();
    let blocked = gate();

    let make_auth = |started: Arc<Semaphore>, blocked: Arc<Semaphore>, key: &'static str| {
        let auth = TestApiKeyAuth::new("API key").with_login(Arc::new(move || {
            let started = Arc::clone(&started);
            let blocked = Arc::clone(&blocked);
            Box::pin(async move {
                started.add_permits(1);
                wait_gate(&blocked).await;
                Ok(ApiKeyCredential {
                    key: Some(key.to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }));
        api_key_auth(auth)
    };

    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::new(
                kiana_config::auth::storage::AuthStorage::in_memory(
                    kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
                ),
            )),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            ..Default::default()
        })
        .await
        .expect("runtime"),
    );
    runtime
        .register_native_provider(
            TestProvider::dynamic(
                "one",
                make_auth(Arc::clone(&first_started), Arc::clone(&blocked), "one"),
            )
            .into_provider(),
        )
        .expect("register one");
    runtime
        .register_native_provider(
            TestProvider::dynamic(
                "two",
                make_auth(Arc::clone(&second_started), Arc::clone(&blocked), "two"),
            )
            .into_provider(),
        )
        .expect("register two");
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["one".to_string(), "two".to_string()]),
            ..Default::default()
        })
        .await
        .expect("refresh");

    let one_runtime = Arc::clone(&runtime);
    let one = tokio::spawn(async move {
        let interaction = TestInteraction::new();
        login_key(&one_runtime, "one", &interaction).await
    });
    let two_runtime = Arc::clone(&runtime);
    let two = tokio::spawn(async move {
        let interaction = TestInteraction::new();
        login_key(&two_runtime, "two", &interaction).await
    });

    wait_gate(&first_started).await;
    wait_gate(&second_started).await;
    blocked.add_permits(2);
    one.await.expect("join").expect("login one");
    two.await.expect("join").expect("login two");
}

#[tokio::test]
async fn does_not_wait_for_unrelated_provider_availability_during_local_synchronization() {
    let stall_unrelated = Arc::new(AtomicBool::new(false));
    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::new(
                kiana_config::auth::storage::AuthStorage::in_memory(
                    kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
                ),
            )),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            ..Default::default()
        })
        .await
        .expect("runtime"),
    );
    runtime
        .register_native_provider(
            TestProvider::dynamic(
                "target",
                api_key_auth(
                    TestApiKeyAuth::new("API key")
                        .with_login(Arc::new(|| {
                            Box::pin(async {
                                Ok(ApiKeyCredential {
                                    key: Some("target-key".to_string()),
                                    env: None,
                                    extra: IndexMap::new(),
                                })
                            })
                        }))
                        .with_check(stored_check("stored")),
                ),
            )
            .into_provider(),
        )
        .expect("register target");
    let stall = Arc::clone(&stall_unrelated);
    runtime
        .register_native_provider(
            TestProvider::dynamic(
                "unrelated",
                api_key_auth(TestApiKeyAuth::new("API key").with_check(Arc::new(
                    move |_credential| {
                        let stall = Arc::clone(&stall);
                        Box::pin(async move {
                            if stall.load(Ordering::SeqCst) {
                                std::future::pending::<()>().await;
                            }
                            Ok(None)
                        })
                    },
                ))),
            )
            .into_provider(),
        )
        .expect("register unrelated");
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["target".to_string(), "unrelated".to_string()]),
            ..Default::default()
        })
        .await
        .expect("refresh");

    let interaction = TestInteraction::new();
    // 注册会触发后台全量刷新（TS 的 `void refresh({ allowNetwork: false })`）；
    // 等它们跑完再打开 unrelated 的阻塞开关，避免后台刷新与登录的 provider 级刷新竞争序号。
    tokio::time::sleep(Duration::from_millis(200)).await;
    stall_unrelated.store(true, Ordering::SeqCst);
    login_key(&runtime, "target", &interaction)
        .await
        .expect("login");
    assert!(runtime.has_configured_auth("target"));

    let result = runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["target".to_string()]),
            ..Default::default()
        })
        .await
        .expect("refresh");
    assert!(!result.aborted);
}

#[tokio::test]
async fn reports_cancellation_that_occurs_during_provider_scoped_availability() {
    let block_availability = Arc::new(AtomicBool::new(false));
    let started = gate();
    let started_check = Arc::clone(&started);
    let block = Arc::clone(&block_availability);
    let auth = TestApiKeyAuth::new("API key")
        .with_login(Arc::new(|| {
            Box::pin(async {
                Ok(ApiKeyCredential {
                    key: Some("key".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }))
        .with_check(Arc::new(move |credential| {
            let started = Arc::clone(&started_check);
            let block = Arc::clone(&block);
            Box::pin(async move {
                if block.load(Ordering::SeqCst) {
                    started.add_permits(1);
                    std::future::pending::<()>().await;
                }
                Ok(credential.and_then(|credential| {
                    credential.key.map(|_| AuthCheck {
                        source: Some("stored".to_string()),
                        r#type: AuthType::ApiKey,
                    })
                }))
            })
        }));
    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::new(
                kiana_config::auth::storage::AuthStorage::in_memory(
                    kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
                ),
            )),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            ..Default::default()
        })
        .await
        .expect("runtime"),
    );
    runtime
        .register_native_provider(
            TestProvider::dynamic("cancelled-availability", api_key_auth(auth)).into_provider(),
        )
        .expect("register");
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["cancelled-availability".to_string()]),
            ..Default::default()
        })
        .await
        .expect("refresh");
    runtime
        .set_runtime_api_key(
            "cancelled-availability",
            "key",
            AuthOperationOptions::default(),
        )
        .await
        .expect("runtime key");
    block_availability.store(true, Ordering::SeqCst);

    let controller = AbortScope::new();
    let refresh_runtime = Arc::clone(&runtime);
    let signal = controller.clone();
    let refresh = tokio::spawn(async move {
        refresh_runtime
            .refresh(ModelsRefreshOptions {
                allow_network: Some(false),
                providers: Some(vec!["cancelled-availability".to_string()]),
                signal: Some(signal),
                ..Default::default()
            })
            .await
    });
    wait_gate(&started).await;
    controller.abort();

    let result = refresh.await.expect("join").expect("refresh");
    assert!(result.aborted);
}

#[tokio::test]
async fn does_not_run_network_refresh_inside_the_credential_operation_chain() {
    let network_refresh = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&network_refresh);
    let auth = TestApiKeyAuth::new("API key")
        .with_login(Arc::new(|| {
            Box::pin(async {
                Ok(ApiKeyCredential {
                    key: Some("local-only-key".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }))
        .with_check(stored_check("stored"));
    let provider = TestProvider::dynamic("local-only", api_key_auth(auth))
        .with_refresh_models(Arc::new(move |context| {
            let counter = Arc::clone(&counter);
            Box::pin(async move {
                if context.allow_network {
                    counter.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<()>().await;
                }
                Ok(())
            })
        }))
        .into_provider();
    let runtime = runtime_with_provider(
        provider,
        Arc::new(kiana_config::auth::storage::AuthStorage::in_memory(
            kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
        )),
    )
    .await;

    let interaction = TestInteraction::new();
    login_key(&runtime, "local-only", &interaction)
        .await
        .expect("login");
    assert_eq!(network_refresh.load(Ordering::SeqCst), 0);
    assert!(runtime.has_configured_auth("local-only"));
}

#[tokio::test]
async fn keeps_provider_scoped_refreshes_from_superseding_unrelated_providers() {
    let started = gate();
    let blocked = gate();
    let first_signal: Arc<Mutex<Option<AbortScope>>> = Arc::new(Mutex::new(None));

    let started_refresh = Arc::clone(&started);
    let blocked_refresh = Arc::clone(&blocked);
    let signal_slot = Arc::clone(&first_signal);
    let one_auth = TestApiKeyAuth::new("API key").with_resolve(Arc::new(|_credential| {
        Box::pin(async {
            Ok(Some(kiana_ai::auth::types::AuthResult::new(
                kiana_ai::auth::types::ModelAuth {
                    api_key: Some("key".to_string()),
                    ..Default::default()
                },
            )))
        })
    }));
    let one = TestProvider::dynamic("one", api_key_auth(one_auth))
        .with_refresh_models(Arc::new(move |context| {
            let started = Arc::clone(&started_refresh);
            let blocked = Arc::clone(&blocked_refresh);
            let signal_slot = Arc::clone(&signal_slot);
            Box::pin(async move {
                if !context.allow_network {
                    return Ok(());
                }
                *signal_slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    Some(context.signal.clone());
                started.add_permits(1);
                wait_gate(&blocked).await;
                Ok(())
            })
        }))
        .into_provider();
    let two = TestProvider::dynamic(
        "two",
        api_key_auth(
            TestApiKeyAuth::new("API key").with_resolve(Arc::new(|_credential| {
                Box::pin(async {
                    Ok(Some(kiana_ai::auth::types::AuthResult::new(
                        kiana_ai::auth::types::ModelAuth {
                            api_key: Some("key".to_string()),
                            ..Default::default()
                        },
                    )))
                })
            })),
        ),
    )
    .into_provider();

    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::new(
                kiana_config::auth::storage::AuthStorage::in_memory(
                    kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
                ),
            )),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            ..Default::default()
        })
        .await
        .expect("runtime"),
    );
    runtime.register_native_provider(one).expect("register one");
    runtime.register_native_provider(two).expect("register two");
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["one".to_string(), "two".to_string()]),
            ..Default::default()
        })
        .await
        .expect("refresh");
    runtime
        .set_runtime_api_key("one", "one-key", AuthOperationOptions::default())
        .await
        .expect("key one");
    runtime
        .set_runtime_api_key("two", "two-key", AuthOperationOptions::default())
        .await
        .expect("key two");

    let first_runtime = Arc::clone(&runtime);
    let first = tokio::spawn(async move {
        first_runtime
            .refresh(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec!["one".to_string()]),
                ..Default::default()
            })
            .await
    });
    wait_gate(&started).await;

    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(true),
            providers: Some(vec!["two".to_string()]),
            ..Default::default()
        })
        .await
        .expect("refresh two");
    let signal = first_signal
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("signal captured");
    assert!(!signal.is_aborted());

    blocked.add_permits(1);
    first.await.expect("join").expect("refresh one");
}

/// TS 的 `delayed-commit` 自定义 `CredentialStore`。
struct DelayedCommitStore {
    stored: Mutex<Option<Credential>>,
    committed: Arc<Semaphore>,
    mutation_finished: Arc<Semaphore>,
}

#[async_trait]
impl CredentialStore for DelayedCommitStore {
    async fn read(
        &self,
        _provider_id: &str,
        _options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        Ok(self
            .stored
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone())
    }

    async fn list(&self, _options: AuthOperationOptions) -> Result<Vec<CredentialInfo>, AuthError> {
        let stored = self
            .stored
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        Ok(stored
            .map(|credential| {
                vec![CredentialInfo {
                    provider_id: "delayed-commit".to_string(),
                    r#type: credential.type_(),
                }]
            })
            .unwrap_or_default())
    }

    async fn modify(
        &self,
        _provider_id: &str,
        f: CredentialModifyFn,
        _options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        let current = self
            .stored
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let next = f(current).await?;
        if let Some(next) = &next {
            *self
                .stored
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(next.clone());
        }
        self.committed.add_permits(1);
        wait_gate(&self.mutation_finished).await;
        Ok(self
            .stored
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone())
    }

    async fn delete(
        &self,
        _provider_id: &str,
        _options: AuthOperationOptions,
    ) -> Result<(), AuthError> {
        *self
            .stored
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        Ok(())
    }
}

#[tokio::test]
async fn waits_for_a_committed_credential_mutation_to_settle_before_reporting_cancellation() {
    let store = Arc::new(DelayedCommitStore {
        stored: Mutex::new(None),
        committed: gate(),
        mutation_finished: gate(),
    });
    let auth = TestApiKeyAuth::new("API key")
        .with_login(Arc::new(|| {
            Box::pin(async {
                Ok(ApiKeyCredential {
                    key: Some("delayed-commit-key".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }))
        .with_check(stored_check("stored"));
    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::clone(&store) as Arc<dyn CredentialStore>),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            ..Default::default()
        })
        .await
        .expect("runtime"),
    );
    runtime
        .register_native_provider(
            TestProvider::dynamic("delayed-commit", api_key_auth(auth)).into_provider(),
        )
        .expect("register");
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["delayed-commit".to_string()]),
            ..Default::default()
        })
        .await
        .expect("refresh");

    let controller = AbortScope::new();
    let interaction = TestInteraction::with_signal(controller.clone());
    let login_runtime = Arc::clone(&runtime);
    let login =
        tokio::spawn(
            async move { login_key(&login_runtime, "delayed-commit", &interaction).await },
        );
    let settled = Arc::new(AtomicBool::new(false));
    let settled_flag = Arc::clone(&settled);
    let outcome = tokio::spawn(async move {
        let result = login.await.expect("join");
        settled_flag.store(true, Ordering::SeqCst);
        result
    });
    wait_gate(&store.committed).await;
    controller.abort();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!settled.load(Ordering::SeqCst));

    store.mutation_finished.add_permits(1);
    let outcome = outcome.await.expect("join");
    let error = outcome.expect_err("credential synchronization error");
    match &error {
        ModelRuntimeError::CredentialSync(CredentialSynchronizationError {
            provider_id,
            operation,
            credential,
            ..
        }) => {
            assert_eq!(provider_id, "delayed-commit");
            assert_eq!(*operation, CredentialSynchronizationOperation::Login);
            let credential = credential.as_ref().expect("credential");
            assert_eq!(key_of(credential), Some("delayed-commit-key"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let stored = store
        .stored
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert_eq!(key_of(&stored.expect("stored")), Some("delayed-commit-key"));
}

#[tokio::test]
async fn reports_a_typed_error_when_cancellation_interrupts_post_commit_synchronization() {
    let block_cache_refresh = Arc::new(AtomicBool::new(false));
    let started = gate();
    let started_refresh = Arc::clone(&started);
    let block = Arc::clone(&block_cache_refresh);
    let auth = TestApiKeyAuth::new("API key")
        .with_login(Arc::new(|| {
            Box::pin(async {
                Ok(ApiKeyCredential {
                    key: Some("cancelled-sync-key".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }))
        .with_check(stored_check("stored"));
    let provider = TestProvider::dynamic("cancelled-sync", api_key_auth(auth))
        .with_refresh_models(Arc::new(move |context| {
            let started = Arc::clone(&started_refresh);
            let block = Arc::clone(&block);
            Box::pin(async move {
                if !context.allow_network && block.load(Ordering::SeqCst) {
                    started.add_permits(1);
                    std::future::pending::<()>().await;
                }
                Ok(())
            })
        }))
        .into_provider();
    let credentials = Arc::new(kiana_config::auth::storage::AuthStorage::in_memory(
        kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
    ));
    let runtime = runtime_with_provider(
        provider,
        Arc::clone(&credentials) as Arc<dyn CredentialStore>,
    )
    .await;
    block_cache_refresh.store(true, Ordering::SeqCst);

    let controller = AbortScope::new();
    let interaction = TestInteraction::with_signal(controller.clone());
    let login_runtime = Arc::clone(&runtime);
    let login =
        tokio::spawn(
            async move { login_key(&login_runtime, "cancelled-sync", &interaction).await },
        );
    wait_gate(&started).await;
    controller.abort();

    let error = login.await.expect("join").expect_err("login error");
    match &error {
        ModelRuntimeError::CredentialSync(CredentialSynchronizationError {
            provider_id,
            operation,
            credential,
            ..
        }) => {
            assert_eq!(provider_id, "cancelled-sync");
            assert_eq!(*operation, CredentialSynchronizationOperation::Login);
            let credential = credential.as_ref().expect("credential");
            assert_eq!(key_of(credential), Some("cancelled-sync-key"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let stored = credentials
        .read("cancelled-sync", AuthOperationOptions::default())
        .await
        .expect("read")
        .expect("stored");
    assert_eq!(key_of(&stored), Some("cancelled-sync-key"));
}

#[tokio::test]
async fn reports_committed_credentials_when_local_synchronization_fails() {
    let fail_cache_refresh = Arc::new(AtomicBool::new(false));
    let fail = Arc::clone(&fail_cache_refresh);
    let auth = TestApiKeyAuth::new("API key")
        .with_login(Arc::new(|| {
            Box::pin(async {
                Ok(ApiKeyCredential {
                    key: Some("broken-sync-key".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }))
        .with_check(stored_check("stored"));
    let provider = TestProvider::dynamic("broken-sync", api_key_auth(auth))
        .with_refresh_models(Arc::new(move |context| {
            let fail = Arc::clone(&fail);
            Box::pin(async move {
                if !context.allow_network && fail.load(Ordering::SeqCst) {
                    return Err(kiana_ai::error::AiError::new(
                        kiana_ai::error::AiErrorKind::Other,
                        "cache restore failed",
                    ));
                }
                Ok(())
            })
        }))
        .into_provider();
    let credentials = Arc::new(kiana_config::auth::storage::AuthStorage::in_memory(
        kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
    ));
    let runtime = runtime_with_provider(
        provider,
        Arc::clone(&credentials) as Arc<dyn CredentialStore>,
    )
    .await;
    fail_cache_refresh.store(true, Ordering::SeqCst);

    let interaction = TestInteraction::new();
    let error = login_key(&runtime, "broken-sync", &interaction)
        .await
        .expect_err("login error");
    match &error {
        ModelRuntimeError::CredentialSync(CredentialSynchronizationError {
            provider_id,
            operation,
            credential,
            ..
        }) => {
            assert_eq!(provider_id, "broken-sync");
            assert_eq!(*operation, CredentialSynchronizationOperation::Login);
            let credential = credential.as_ref().expect("credential");
            assert_eq!(key_of(credential), Some("broken-sync-key"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let stored = credentials
        .read("broken-sync", AuthOperationOptions::default())
        .await
        .expect("read")
        .expect("stored");
    assert_eq!(key_of(&stored), Some("broken-sync-key"));
}

// ---------------------------------------------------------------------------
// 复审回归：取消/错误语义（TS model-runtime.ts §2.20.7 / §2.20.10 的 catch 时刻检查）
// ---------------------------------------------------------------------------

/// TS 在 provider 级可用性阶段的 catch 里检查 `!options.signal?.aborted`：调用方在阶段中途
/// 中止时，错误被抑制（`errors` 为空），而不是记录成 provider 失败。
#[tokio::test]
async fn suppresses_provider_availability_errors_when_the_caller_aborts() {
    let block = Arc::new(AtomicBool::new(false));
    let started = gate();
    let release = gate();
    let started_check = Arc::clone(&started);
    let release_check = Arc::clone(&release);
    let blocked = Arc::clone(&block);
    let auth = TestApiKeyAuth::new("API key")
        .with_login(Arc::new(|| {
            Box::pin(async {
                Ok(ApiKeyCredential {
                    key: Some("key".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }))
        .with_check(Arc::new(move |credential| {
            let started = Arc::clone(&started_check);
            let release = Arc::clone(&release_check);
            let blocked = Arc::clone(&blocked);
            Box::pin(async move {
                if blocked.load(Ordering::SeqCst) {
                    started.add_permits(1);
                    wait_gate(&release).await;
                    return Err(AuthError::new("provider check failed"));
                }
                Ok(credential.and_then(|credential| {
                    credential.key.map(|_| AuthCheck {
                        source: Some("stored".to_string()),
                        r#type: AuthType::ApiKey,
                    })
                }))
            })
        }));
    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::new(
                kiana_config::auth::storage::AuthStorage::in_memory(
                    kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
                ),
            )),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            ..Default::default()
        })
        .await
        .expect("runtime"),
    );
    runtime
        .register_native_provider(
            TestProvider::dynamic("abort-suppressed", api_key_auth(auth)).into_provider(),
        )
        .expect("register");
    // 注册触发的后台全量刷新先跑完，再打开阻塞开关，避免它抢在测试场景前报错。
    tokio::time::sleep(Duration::from_millis(200)).await;
    block.store(true, Ordering::SeqCst);

    let controller = AbortScope::new();
    let signal = controller.clone();
    let refresh_runtime = Arc::clone(&runtime);
    let refresh = tokio::spawn(async move {
        refresh_runtime
            .refresh(ModelsRefreshOptions {
                allow_network: Some(false),
                providers: Some(vec!["abort-suppressed".to_string()]),
                signal: Some(signal),
                ..Default::default()
            })
            .await
    });
    wait_gate(&started).await;
    controller.abort();
    release.add_permits(1);

    let result = refresh.await.expect("join").expect("refresh");
    assert!(result.aborted);
    assert!(
        result.errors.is_empty(),
        "aborted caller must suppress availability errors: {:?}",
        result.errors.keys().collect::<Vec<_>>()
    );
    assert_eq!(runtime.get_error(), None);
}

/// TS 的 `login` operation 在 enqueue 时就独立于调用方 promise：调用方 `Promise.race` 超时
/// 放弃等待后，后台任务仍会把凭证提交到 store。
#[tokio::test]
async fn completes_a_login_after_the_callers_future_is_dropped() {
    let auth = TestApiKeyAuth::new("API key")
        .with_login(Arc::new(|| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(150)).await;
                Ok(ApiKeyCredential {
                    key: Some("slow-key".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }))
        .with_check(stored_check("stored"));
    let credentials = Arc::new(kiana_config::auth::storage::AuthStorage::in_memory(
        kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
    ));
    let runtime = runtime_with_provider(
        TestProvider::dynamic("slow-login", api_key_auth(auth)).into_provider(),
        Arc::clone(&credentials) as Arc<dyn CredentialStore>,
    )
    .await;

    let interaction: Arc<dyn AuthInteraction> = Arc::new(TestInteraction::new());
    let timed_out = tokio::time::timeout(
        Duration::from_millis(10),
        runtime.login(
            "slow-login",
            AuthType::ApiKey,
            interaction,
            LoginOptions::default(),
        ),
    )
    .await;
    assert!(
        timed_out.is_err(),
        "login should outlive the caller's timeout"
    );

    // 后台任务继续跑：凭证最终写入 store，同步阶段也完成（provider 已配置）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let stored = loop {
        let stored = credentials
            .read("slow-login", AuthOperationOptions::default())
            .await
            .expect("read");
        if stored.is_some() && runtime.has_configured_auth("slow-login") {
            break stored;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "detached login never committed its credential"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        key_of(&stored.expect("stored credential")),
        Some("slow-key")
    );
}

/// 排队中的凭据操作也独立于调用方：排队期间调用方 future 被丢弃，轮到它时仍然执行。
#[tokio::test]
async fn queued_credential_operations_complete_after_the_callers_future_is_dropped() {
    let started = gate();
    let started_login = Arc::clone(&started);
    let auth = TestApiKeyAuth::new("API key")
        .with_login(Arc::new(move || {
            let started = Arc::clone(&started_login);
            Box::pin(async move {
                started.add_permits(1);
                tokio::time::sleep(Duration::from_millis(120)).await;
                Ok(ApiKeyCredential {
                    key: Some("holder-key".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }))
        .with_check(stored_check("stored"));
    let credentials = Arc::new(kiana_config::auth::storage::AuthStorage::in_memory(
        kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
    ));
    let runtime = runtime_with_provider(
        TestProvider::dynamic("queued-ops", api_key_auth(auth)).into_provider(),
        Arc::clone(&credentials) as Arc<dyn CredentialStore>,
    )
    .await;

    let interaction: Arc<dyn AuthInteraction> = Arc::new(TestInteraction::new());
    let login_runtime = Arc::clone(&runtime);
    let login = tokio::spawn(async move {
        login_runtime
            .login(
                "queued-ops",
                AuthType::ApiKey,
                interaction,
                LoginOptions::default(),
            )
            .await
    });
    // 登录任务已获得 provider 许可（持有凭据锁），后续操作只能排队。
    wait_gate(&started).await;
    let timed_out = tokio::time::timeout(
        Duration::from_millis(20),
        runtime.set_runtime_api_key("queued-ops", "queued-key", AuthOperationOptions::default()),
    )
    .await;
    assert!(
        timed_out.is_err(),
        "setRuntimeApiKey should still be queued"
    );
    login.await.expect("join").expect("login");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if runtime.get_provider_auth_status("queued-ops").source == Some(AuthStatusSource::Runtime)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "queued credential operation was cancelled with its caller"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `CredentialSynchronizationError` 经由冻结的 `Models` trait 传播时保留 `cause`（以及
/// `ModelsError.code`），与 TS 的 `super(message, { cause })` 一致。
#[tokio::test]
async fn models_trait_login_preserves_the_synchronization_cause() {
    let fail_cache_refresh = Arc::new(AtomicBool::new(false));
    let fail = Arc::clone(&fail_cache_refresh);
    let auth = TestApiKeyAuth::new("API key")
        .with_login(Arc::new(|| {
            Box::pin(async {
                Ok(ApiKeyCredential {
                    key: Some("cause-key".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }))
        .with_check(stored_check("stored"));
    let provider = TestProvider::dynamic("cause-sync", api_key_auth(auth))
        .with_refresh_models(Arc::new(move |context| {
            let fail = Arc::clone(&fail);
            Box::pin(async move {
                if !context.allow_network && fail.load(Ordering::SeqCst) {
                    return Err(ai_error_from_models_error(models_error(
                        TsModelsErrorCode::Provider,
                        "cache restore failed",
                        None,
                    )));
                }
                Ok(())
            })
        }))
        .into_provider();
    let credentials = Arc::new(kiana_config::auth::storage::AuthStorage::in_memory(
        kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
    ));
    let runtime = runtime_with_provider(
        provider,
        Arc::clone(&credentials) as Arc<dyn CredentialStore>,
    )
    .await;
    fail_cache_refresh.store(true, Ordering::SeqCst);

    let interaction = TestInteraction::new();
    let error = Models::login(
        runtime.as_ref(),
        "cause-sync",
        AuthType::ApiKey,
        &interaction,
        LoginOptions::default(),
    )
    .await
    .expect_err("login error");
    assert_eq!(error.name, "CredentialSynchronizationError");
    assert_eq!(
        error.message,
        "Credential login committed for cause-sync, but local synchronization failed"
    );
    let cause = error.cause().expect("cause is preserved through Models");
    assert_eq!(cause.message, "cache restore failed");
    assert_eq!(cause.code, Some(ModelsErrorCode::Provider));
}
