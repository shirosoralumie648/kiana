//! `models-runtime.test.ts` 的 Rust 移植。
//!
//! TS 用普通对象 + `vi.fn()`；Rust 用 [`models_support::TestProvider`] 与闭包捕获。

mod models_support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::future::BoxFuture;
use kiana_ai::auth::credential_store::InMemoryCredentialStore;
use kiana_ai::auth::resolve::AuthResolutionOverrides;
use kiana_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthError, AuthInteraction,
    AuthOperationOptions, AuthResult, AuthType, Credential, CredentialInfo, CredentialModifyFn,
    CredentialStore, LoginOptions, ModelAuth, OAuthCredential, ProviderAuth,
};
use kiana_ai::error::{AiError, ModelsErrorCode};
use kiana_ai::models::{
    CreateModelsOptions, CreateProviderOptions, GetAuthTarget, ModelsPublication,
    ModelsPublicationPersist, ModelsRefreshOptions, ModelsSimpleStreamOptions, ModelsStreamOptions,
    ProviderApiImpl, calculate_cost, create_models, create_provider, has_api,
};
use kiana_ai::models::{
    InMemoryModelsStore, ModelsStore, ModelsStoreEntry, ModelsStoreOperationOptions,
};
use kiana_ai::types::{
    AssistantMessageEvent, Context, Model, ModelCost, ModelCostTier, ProviderHeaders,
    SimpleStreamOptions, StreamOptions, Usage, UsageCost,
};
use kiana_ai::utils::abort::{AbortReason, AbortScope};

use models_support::*;

fn context() -> Context {
    Context {
        messages: vec![],
        ..Context::default()
    }
}

fn simple_options() -> ModelsSimpleStreamOptions {
    ModelsSimpleStreamOptions::default()
}

#[tokio::test]
async fn enumerates_credential_metadata_without_exposing_secrets() {
    let credentials = InMemoryCredentialStore::new();
    credentials
        .modify(
            "api-provider",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("secret".into()),
                        ..Default::default()
                    })))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    credentials
        .modify(
            "oauth-provider",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "access",
                        "refresh",
                        now_ms() as f64 + 60_000.0,
                    ))))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    let listed = credentials.list(Default::default()).await.unwrap();
    assert_eq!(
        listed,
        vec![
            CredentialInfo {
                provider_id: "api-provider".into(),
                r#type: AuthType::ApiKey
            },
            CredentialInfo {
                provider_id: "oauth-provider".into(),
                r#type: AuthType::OAuth
            },
        ]
    );
}

#[tokio::test]
async fn applies_request_wide_pricing_tiers_above_the_configured_input_threshold() {
    let mut model = test_model("openai", "gpt-5.6-sol");
    model.cost = ModelCost {
        input: 5.0,
        output: 30.0,
        cache_read: 0.5,
        cache_write: 6.25,
        tiers: Some(vec![ModelCostTier {
            input_tokens_above: 272_000.0,
            input: 10.0,
            output: 45.0,
            cache_read: 1.0,
            cache_write: 12.5,
        }]),
    };
    let create_usage = |cache_write: u64| Usage {
        input: 200_000,
        output: 100_000,
        cache_read: 72_000,
        cache_write,
        total_tokens: 372_000 + cache_write,
        cost: UsageCost::default(),
        ..Usage::default()
    };

    let mut short = create_usage(0);
    let cost = calculate_cost(&model, &mut short);
    assert_eq!(cost.input, 1.0);
    assert_eq!(cost.output, 3.0);
    assert!((cost.cache_read - 0.036).abs() < 1e-12);
    assert_eq!(cost.cache_write, 0.0);

    let mut long = create_usage(1);
    let cost = calculate_cost(&model, &mut long);
    assert_eq!(cost.input, 2.0);
    assert_eq!(cost.output, 4.5);
    assert!((cost.cache_read - 0.072).abs() < 1e-12);
    assert!((cost.cache_write - 0.0000125).abs() < 1e-15);
}

#[tokio::test]
async fn marks_catalog_derived_totals_as_not_provider_reported() {
    let mut usage = Usage {
        input: 1_000_000,
        cost: UsageCost {
            total: 9.0,
            total_from_provider: Some(true),
            ..UsageCost::default()
        },
        ..Usage::default()
    };
    calculate_cost(&test_model("openai", "gpt-5.6-sol"), &mut usage);
    assert_eq!(usage.cost.total_from_provider, Some(false));
}

#[tokio::test]
async fn registers_replaces_and_deletes_providers() {
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(TestProvider::new("p1")));
    models.set_provider(Arc::new(TestProvider::new("p2")));
    assert_eq!(
        models
            .get_providers()
            .iter()
            .map(|provider| provider.id().to_owned())
            .collect::<Vec<_>>(),
        ["p1", "p2"]
    );

    let replacement: Arc<dyn kiana_ai::models::Provider> = Arc::new(TestProvider::new("p1"));
    models.set_provider(Arc::clone(&replacement));
    assert!(Arc::ptr_eq(
        &models.get_provider("p1").unwrap(),
        &replacement
    ));
    assert_eq!(models.get_providers().len(), 2);

    models.delete_provider("p1");
    assert!(models.get_provider("p1").is_none());

    models.clear_providers();
    assert!(models.get_providers().is_empty());
}

#[tokio::test]
async fn lists_and_finds_models_per_provider() {
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(TestProvider::with_models(
        "p1",
        vec![test_model("p1", "m1"), test_model("p1", "m2")],
    )));
    models.set_provider(Arc::new(TestProvider::with_models(
        "p2",
        vec![test_model("p2", "m3")],
    )));

    assert_eq!(
        models
            .get_models(None)
            .iter()
            .map(|model| model.id.clone())
            .collect::<Vec<_>>(),
        ["m1", "m2", "m3"]
    );
    assert_eq!(models.get_models(Some("nope")).len(), 0);
    assert_eq!(models.get_model("p2", "m3").unwrap().id, "m3");
    assert!(models.get_model("p2", "missing").is_none());

    let found = models.get_model("p2", "m3");
    assert!(found.is_some());
    let found = found.unwrap();
    // `hasApi()` 是运行时检查（Rust 没有类型收窄，但语义相同）。
    let any = kiana_ai::types::AnyModel::Chat(found.clone());
    assert!(!has_api(&any, "openai-completions"));
    assert!(has_api(&any, "test-api"));
}

#[tokio::test]
async fn keeps_chat_reads_independent_from_the_all_model_catalog() {
    let provider = TestProvider::new("chat-only").with_get_all_models(Arc::new(|| {
        panic!("all models unavailable");
    }));
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(provider));

    assert_eq!(models.get_models(Some("chat-only"))[0].id, "model-a");
    assert_eq!(
        models.get_model("chat-only", "model-a").unwrap().id,
        "model-a"
    );
    assert_eq!(
        models
            .get_available(Some("chat-only"), Default::default())
            .await
            .unwrap()[0]
            .id,
        "model-a"
    );
    assert!(models.get_all_models(Some("chat-only")).is_empty());
}

#[tokio::test]
async fn swallows_provider_source_failures_for_both_all_provider_and_single_provider_listing() {
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(TestProvider::with_get_models(
        "broken",
        Arc::new(|| panic!("boom")),
    )));
    models.set_provider(Arc::new(TestProvider::with_models(
        "ok",
        vec![test_model("ok", "m1")],
    )));

    assert_eq!(models.get_models(None)[0].id, "m1");
    assert!(models.get_models(Some("broken")).is_empty());
    // precise failures come from the provider directly
    let broken = models.get_provider("broken").unwrap();
    let panics = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| broken.get_models()));
    assert!(panics.is_err());
}

#[tokio::test]
async fn refresh_updates_every_configured_dynamic_provider_and_reports_failures() {
    let list = Arc::new(Mutex::new(vec![test_model("dyn", "before")]));
    let refreshes = Arc::new(AtomicUsize::new(0));
    let models = create_models(CreateModelsOptions::default());
    let list_for_refresh = Arc::clone(&list);
    let refreshes_for_provider = Arc::clone(&refreshes);
    models.set_provider(Arc::new(
        TestProvider::with_get_models("dyn", {
            let list = Arc::clone(&list);
            Arc::new(move || list.lock().unwrap_or_else(|e| e.into_inner()).clone())
        })
        .with_refresh(Arc::new(move |refresh| {
            let list = Arc::clone(&list_for_refresh);
            let refreshes = Arc::clone(&refreshes_for_provider);
            Box::pin(async move {
                if !refresh.allow_network {
                    return Ok(());
                }
                refreshes.fetch_add(1, Ordering::SeqCst);
                (refresh.publish)(kiana_ai::models::ModelsPublication {
                    persist: ModelsPublicationPersist::Unchanged,
                    update: Some(Box::new(move || {
                        *list.lock().unwrap_or_else(|e| e.into_inner()) =
                            vec![test_model("dyn", "after")];
                    })),
                })
                .await?;
                Ok(())
            })
        })),
    ));
    models.set_provider(Arc::new(TestProvider::with_models(
        "static",
        vec![test_model("static", "s1")],
    )));

    assert!(models.get_model("dyn", "before").is_some());
    let first = models.refresh(Default::default()).await.unwrap();
    assert_eq!(first.errors.len(), 0);
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert!(models.get_model("dyn", "after").is_some());
    assert!(models.get_model("dyn", "before").is_none());

    models.set_provider(Arc::new(TestProvider::new("flaky").with_refresh(Arc::new(
        |refresh| {
            Box::pin(async move {
                if refresh.allow_network {
                    return Err(AiError::new(
                        kiana_ai::error::AiErrorKind::Other,
                        "fetch failed",
                    ));
                }
                Ok(())
            })
        },
    ))));
    let second = models.refresh(Default::default()).await.unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 2);
    assert_eq!(second.errors.get("flaky").unwrap().message, "fetch failed");
}

#[tokio::test]
async fn restricts_refresh_work_to_selected_providers() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let models = create_models(CreateModelsOptions::default());
    for id in ["one", "two"] {
        let calls = Arc::clone(&calls);
        let id_owned = id.to_owned();
        models.set_provider(Arc::new(TestProvider::new(id).with_refresh(Arc::new(
            move |refresh| {
                let calls = Arc::clone(&calls);
                let id = id_owned.clone();
                Box::pin(async move {
                    calls
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(format!(
                            "{id}:{}",
                            if refresh.allow_network {
                                "network"
                            } else {
                                "cache"
                            }
                        ));
                    Ok(())
                })
            },
        ))));
    }

    let result = models
        .refresh(ModelsRefreshOptions {
            providers: Some(vec!["two".into(), "unknown".into()]),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.errors.len(), 0);
    assert_eq!(
        *calls.lock().unwrap_or_else(|e| e.into_inner()),
        ["two:cache", "two:network"]
    );
}

struct BlockedAuth {
    started: Arc<tokio::sync::Notify>,
    finish: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ApiKeyAuth for BlockedAuth {
    fn name(&self) -> &str {
        "Blocked auth"
    }
    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        self.started.notify_one();
        self.finish.notified().await;
        Ok(Some(AuthResult::new(ModelAuth {
            api_key: Some("key".into()),
            ..Default::default()
        })))
    }
}

#[tokio::test]
async fn restores_cached_models_before_waiting_for_network_auth() {
    let store = InMemoryModelsStore::new();
    store
        .write(
            "dynamic",
            ModelsStoreEntry {
                models: vec![chat(test_model("dynamic", "cached"))],
                ..Default::default()
            },
            Default::default(),
        )
        .await
        .unwrap();
    let auth_started = Arc::new(tokio::sync::Notify::new());
    let auth_finish = Arc::new(tokio::sync::Notify::new());
    let provider = create_provider(CreateProviderOptions {
        id: "dynamic".into(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(BlockedAuth {
                started: Arc::clone(&auth_started),
                finish: Arc::clone(&auth_finish),
            })),
            oauth: None,
        },
        models: vec![],
        fetch_models: Some(Arc::new(|_context| {
            Box::pin(async {
                Err(AiError::new(
                    kiana_ai::error::AiErrorKind::Other,
                    "must not fetch",
                ))
            })
        })),
        api: Some(ProviderApiImpl::Single(Arc::new(SilentStreams))),
        ..Default::default()
    })
    .unwrap();
    let models = create_models(CreateModelsOptions {
        models_store: Some(Arc::new(store)),
        ..Default::default()
    });
    models.set_provider(provider);
    let controller = AbortScope::new();
    let models_for_refresh = Arc::clone(&models);
    let signal = controller.clone();
    let pending = tokio::spawn(async move {
        models_for_refresh
            .refresh(ModelsRefreshOptions {
                providers: Some(vec!["dynamic".into()]),
                signal: Some(signal),
                ..Default::default()
            })
            .await
    });
    auth_started.notified().await;
    assert!(models.get_model("dynamic", "cached").is_some());
    controller.abort();
    assert!(pending.await.unwrap().unwrap().aborted);
    auth_finish.notify_one();
}

struct RecordingStore {
    entry: Mutex<Option<ModelsStoreEntry>>,
}

#[async_trait]
impl ModelsStore for RecordingStore {
    async fn read(
        &self,
        _provider_id: &str,
        _options: ModelsStoreOperationOptions,
    ) -> Result<Option<ModelsStoreEntry>, AiError> {
        Ok(self.entry.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }
    async fn write(
        &self,
        _provider_id: &str,
        entry: ModelsStoreEntry,
        _options: ModelsStoreOperationOptions,
    ) -> Result<(), AiError> {
        *self.entry.lock().unwrap_or_else(|e| e.into_inner()) = Some(entry);
        Ok(())
    }
    async fn delete(
        &self,
        _provider_id: &str,
        _options: ModelsStoreOperationOptions,
    ) -> Result<(), AiError> {
        *self.entry.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }
}

#[tokio::test]
async fn lets_providers_choose_persistent_deletion_and_ephemeral_publication_atomically() {
    let store = Arc::new(RecordingStore {
        entry: Mutex::new(Some(ModelsStoreEntry {
            models: vec![chat(test_model("dynamic", "stored"))],
            ..Default::default()
        })),
    });
    let state = Arc::new(Mutex::new("initial".to_owned()));
    let models = create_models(CreateModelsOptions {
        models_store: Some(Arc::clone(&store) as Arc<dyn ModelsStore>),
        ..Default::default()
    });
    let store_for_provider = Arc::clone(&store);
    let state_for_provider = Arc::clone(&state);
    models.set_provider(Arc::new(TestProvider::new("dynamic").with_refresh(
        Arc::new(move |context| {
            let store = Arc::clone(&store_for_provider);
            let state = Arc::clone(&state_for_provider);
            Box::pin(async move {
                assert_eq!(
                    context.stored.as_ref().unwrap().models[0].model().id,
                    "stored"
                );
                let published = (context.publish)(kiana_ai::models::ModelsPublication {
                    persist: ModelsPublicationPersist::Delete,
                    update: Some(Box::new({
                        let store = Arc::clone(&store);
                        let state = Arc::clone(&state);
                        move || {
                            assert!(
                                store
                                    .entry
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .is_none()
                            );
                            *state.lock().unwrap_or_else(|e| e.into_inner()) = "deleted".into();
                        }
                    })),
                })
                .await?;
                assert!(published);
                (context.publish)(kiana_ai::models::ModelsPublication {
                    persist: ModelsPublicationPersist::Unchanged,
                    update: Some(Box::new(move || {
                        *state.lock().unwrap_or_else(|e| e.into_inner()) = "ephemeral".into();
                    })),
                })
                .await?;
                Ok(())
            })
        }),
    )));

    let result = models
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.errors.len(), 0);
    assert!(
        store
            .entry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none()
    );
    assert_eq!(
        *state.lock().unwrap_or_else(|e| e.into_inner()),
        "ephemeral"
    );
}

type FetchModelsFn = Arc<dyn Fn() -> BoxFuture<'static, Result<Vec<Model>, AiError>> + Send + Sync>;

fn dynamic_provider(fetch_models: Option<FetchModelsFn>) -> Arc<dyn kiana_ai::models::Provider> {
    create_provider(CreateProviderOptions {
        id: "dynamic".into(),
        auth: env_key_auth(None),
        models: vec![],
        fetch_models: fetch_models.map(|fetch| {
            let fetch: FetchModelsFn = fetch;
            Arc::new(move |_context: kiana_ai::models::RefreshModelsContext| {
                let fetch = Arc::clone(&fetch);
                let future: BoxFuture<'static, Result<Vec<kiana_ai::types::AnyModel>, AiError>> =
                    Box::pin(async move { Ok(fetch().await?.into_iter().map(chat).collect()) });
                future
            })
                as Arc<
                    dyn Fn(
                            kiana_ai::models::RefreshModelsContext,
                        )
                            -> BoxFuture<'static, Result<Vec<kiana_ai::types::AnyModel>, AiError>>
                        + Send
                        + Sync,
                >
        }),
        api: Some(ProviderApiImpl::Single(Arc::new(SilentStreams))),
        ..Default::default()
    })
    .unwrap()
}

#[tokio::test]
async fn persists_dynamic_catalogs_and_restores_them_without_network_access() {
    let credentials = InMemoryCredentialStore::new();
    credentials
        .modify(
            "dynamic",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("key".into()),
                        ..Default::default()
                    })))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    let models_store = Arc::new(InMemoryModelsStore::new());

    let online = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials.clone())),
        models_store: Some(models_store.clone()),
        ..Default::default()
    });
    online.set_provider(dynamic_provider(Some(Arc::new(|| {
        Box::pin(async { Ok(vec![test_model("dynamic", "fetched")]) })
    }))));
    assert_eq!(
        online
            .refresh(Default::default())
            .await
            .unwrap()
            .errors
            .len(),
        0
    );
    assert!(online.get_model("dynamic", "fetched").is_some());

    let offline = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials)),
        models_store: Some(models_store),
        ..Default::default()
    });
    offline.set_provider(dynamic_provider(Some(Arc::new(|| {
        Box::pin(async {
            Err(AiError::new(
                kiana_ai::error::AiErrorKind::Other,
                "must not fetch",
            ))
        })
    }))));
    assert_eq!(
        offline
            .refresh(ModelsRefreshOptions {
                allow_network: Some(false),
                ..Default::default()
            })
            .await
            .unwrap()
            .errors
            .len(),
        0
    );
    assert!(offline.get_model("dynamic", "fetched").is_some());
}

#[tokio::test]
async fn passes_effective_api_key_credentials_and_refresh_options_while_skipping_unconfigured_providers()
 {
    let effective: Arc<Mutex<Option<Credential>>> = Arc::new(Mutex::new(None));
    let force: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let unconfigured_refreshes = Arc::new(AtomicUsize::new(0));
    let models = create_models(CreateModelsOptions::default());

    let effective_for = Arc::clone(&effective);
    let force_for = Arc::clone(&force);
    models.set_provider(Arc::new(
        TestProvider::new("configured")
            .with_auth(env_key_auth(Some("ambient-key")))
            .with_refresh(Arc::new(move |refresh| {
                let effective = Arc::clone(&effective_for);
                let force = Arc::clone(&force_for);
                Box::pin(async move {
                    if !refresh.allow_network {
                        return Ok(());
                    }
                    *effective.lock().unwrap_or_else(|e| e.into_inner()) =
                        refresh.credential.clone();
                    *force.lock().unwrap_or_else(|e| e.into_inner()) = refresh.force;
                    Ok(())
                })
            })),
    ));
    let unconfigured_for = Arc::clone(&unconfigured_refreshes);
    models.set_provider(Arc::new(
        TestProvider::new("unconfigured")
            .with_auth(env_key_auth(None))
            .with_refresh(Arc::new(move |refresh| {
                let unconfigured = Arc::clone(&unconfigured_for);
                Box::pin(async move {
                    if refresh.allow_network {
                        unconfigured.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(())
                })
            })),
    ));

    models
        .refresh(ModelsRefreshOptions {
            force: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    let expected = Credential::ApiKey(ApiKeyCredential {
        key: Some("ambient-key".into()),
        env: None,
        ..Default::default()
    });
    assert_eq!(
        *effective.lock().unwrap_or_else(|e| e.into_inner()),
        Some(expected)
    );
    assert_eq!(*force.lock().unwrap_or_else(|e| e.into_inner()), Some(true));
    assert_eq!(unconfigured_refreshes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn passes_the_registered_providers_base_url_including_wrapper_overrides_to_refresh_models() {
    let base_urls: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let base_urls_for = Arc::clone(&base_urls);
    let mut provider = TestProvider::new("dynamic").with_refresh(Arc::new(move |context| {
        let base_urls = Arc::clone(&base_urls_for);
        Box::pin(async move {
            base_urls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(context.base_url);
            Ok(())
        })
    }));
    provider.base_url = Some("https://regional.example.test/v1".into());
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(provider));

    models.refresh(Default::default()).await.unwrap();
    assert_eq!(
        *base_urls.lock().unwrap_or_else(|e| e.into_inner()),
        [
            Some("https://regional.example.test/v1".to_owned()),
            Some("https://regional.example.test/v1".to_owned())
        ]
    );
}

#[tokio::test]
async fn refreshes_expired_oauth_before_refreshing_models() {
    let credentials = InMemoryCredentialStore::new();
    let model_refresh_credential: Arc<Mutex<Option<Credential>>> = Arc::new(Mutex::new(None));
    credentials
        .modify(
            "oauth-dynamic",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "expired", "refresh", 0.0,
                    ))))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials.clone())),
        ..Default::default()
    });
    let oauth = TestOAuth::with_refresh(Arc::new(|_credential| {
        Box::pin(async {
            Ok(OAuthCredential::new(
                "fresh",
                "rotated",
                now_ms() as f64 + 60_000.0,
            ))
        })
    }));
    let captured = Arc::clone(&model_refresh_credential);
    models.set_provider(Arc::new(
        TestProvider::new("oauth-dynamic")
            .with_auth(oauth_auth(oauth))
            .with_refresh(Arc::new(move |context| {
                let captured = Arc::clone(&captured);
                Box::pin(async move {
                    if context.allow_network {
                        *captured.lock().unwrap_or_else(|e| e.into_inner()) =
                            context.credential.clone();
                    }
                    Ok(())
                })
            })),
    ));

    assert_eq!(
        models
            .refresh(Default::default())
            .await
            .unwrap()
            .errors
            .len(),
        0
    );
    match model_refresh_credential
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .unwrap()
    {
        Credential::OAuth(credential) => {
            assert_eq!(credential.access, "fresh");
            assert_eq!(credential.refresh, "rotated");
        }
        other => panic!("expected oauth credential, got {other:?}"),
    }
    match credentials
        .read("oauth-dynamic", Default::default())
        .await
        .unwrap()
    {
        Some(Credential::OAuth(credential)) => {
            assert_eq!(credential.access, "fresh");
            assert_eq!(credential.refresh, "rotated");
        }
        other => panic!("expected stored oauth credential, got {other:?}"),
    }
}

#[tokio::test]
async fn persists_an_oauth_refresh_that_started_before_the_model_refresh_was_cancelled() {
    let credentials = InMemoryCredentialStore::new();
    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "old",
                        "old-refresh",
                        0.0,
                    ))))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    let controller = AbortScope::new();
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials.clone())),
        ..Default::default()
    });
    let controller_for_refresh = controller.clone();
    let oauth = TestOAuth::with_refresh(Arc::new(move |credential| {
        let controller = controller_for_refresh.clone();
        Box::pin(async move {
            controller.abort();
            Ok(OAuthCredential {
                access: "new".into(),
                refresh: "new-refresh".into(),
                expires: now_ms() as f64 + 60_000.0,
                ..credential
            })
        })
    }));
    models.set_provider(Arc::new(
        TestProvider::new("p1")
            .with_auth(oauth_auth(oauth))
            .with_refresh(Arc::new(|_context| Box::pin(async { Ok(()) }))),
    ));

    let result = models
        .refresh(ModelsRefreshOptions {
            signal: Some(controller.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(result.aborted);
    let credentials_for_wait = credentials.clone();
    wait_for(move || {
        let credentials = credentials_for_wait.clone();
        let value = futures::executor::block_on(credentials.read("p1", Default::default()))
            .ok()
            .flatten();
        matches!(
            value,
            Some(Credential::OAuth(OAuthCredential { refresh, .. })) if refresh == "new-refresh"
        )
    })
    .await;
}

#[tokio::test]
async fn always_gives_providers_a_concrete_signal() {
    let received: Arc<Mutex<Option<AbortScope>>> = Arc::new(Mutex::new(None));
    let models = create_models(CreateModelsOptions::default());
    let received_for = Arc::clone(&received);
    models.set_provider(Arc::new(TestProvider::new("dynamic").with_refresh(
        Arc::new(move |context| {
            let received = Arc::clone(&received_for);
            Box::pin(async move {
                *received.lock().unwrap_or_else(|e| e.into_inner()) = Some(context.signal.clone());
                Ok(())
            })
        }),
    )));
    let result = models.refresh(Default::default()).await.unwrap();
    assert!(!result.aborted);
    let signal = received
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("provider receives a signal");
    assert!(!signal.is_aborted());
}

struct SignalStore {
    signals: Arc<Mutex<Vec<AbortScope>>>,
}

#[async_trait]
impl ModelsStore for SignalStore {
    async fn read(
        &self,
        _provider_id: &str,
        options: ModelsStoreOperationOptions,
    ) -> Result<Option<ModelsStoreEntry>, AiError> {
        if let Some(signal) = options.signal {
            self.signals
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(signal);
        }
        Ok(None)
    }
    async fn write(
        &self,
        _provider_id: &str,
        _entry: ModelsStoreEntry,
        options: ModelsStoreOperationOptions,
    ) -> Result<(), AiError> {
        if let Some(signal) = options.signal {
            self.signals
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(signal);
        }
        Ok(())
    }
    async fn delete(
        &self,
        _provider_id: &str,
        options: ModelsStoreOperationOptions,
    ) -> Result<(), AiError> {
        if let Some(signal) = options.signal {
            self.signals
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(signal);
        }
        Ok(())
    }
}

#[tokio::test]
async fn binds_model_store_waits_to_the_provider_refresh_signal() {
    let signals = Arc::new(Mutex::new(Vec::new()));
    let provider_signal: Arc<Mutex<Option<AbortScope>>> = Arc::new(Mutex::new(None));
    let models = create_models(CreateModelsOptions {
        models_store: Some(Arc::new(SignalStore {
            signals: Arc::clone(&signals),
        })),
        ..Default::default()
    });
    let provider_signal_for = Arc::clone(&provider_signal);
    models.set_provider(Arc::new(
        TestProvider::new("dynamic")
            .with_auth(env_key_auth(Some("key")))
            .with_refresh(Arc::new(move |context| {
                let provider_signal = Arc::clone(&provider_signal_for);
                Box::pin(async move {
                    *provider_signal.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(context.signal.clone());
                    if !context.allow_network {
                        return Ok(());
                    }
                    (context.publish)(kiana_ai::models::ModelsPublication {
                        persist: ModelsPublicationPersist::Entry(ModelsStoreEntry {
                            models: vec![chat(test_model("dynamic", "fresh"))],
                            ..Default::default()
                        }),
                        update: None,
                    })
                    .await?;
                    Ok(())
                })
            })),
    ));

    let result = models
        .refresh(ModelsRefreshOptions {
            providers: Some(vec!["dynamic".into()]),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.errors.len(), 0);
    let signals = signals.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(signals.len(), 3);
    // 同一 signal 的克隆共享取消令牌：中止 provider 看到的 signal 会让全部记录一起中止。
    let provider_signal = provider_signal
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap();
    provider_signal.abort();
    assert!(signals.iter().all(AbortScope::is_aborted));
}

#[tokio::test]
async fn returns_aborted_state_without_reporting_cancellation_as_a_provider_error() {
    let controller = AbortScope::new();
    let models = create_models(CreateModelsOptions::default());
    let controller_for = controller.clone();
    models.set_provider(Arc::new(TestProvider::new("dynamic").with_refresh(
        Arc::new(move |context| {
            let controller = controller_for.clone();
            Box::pin(async move {
                controller.abort();
                if context.signal.is_aborted() {
                    return Ok(());
                }
                Ok(())
            })
        }),
    )));
    let result = models
        .refresh(ModelsRefreshOptions {
            signal: Some(controller),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(result.aborted);
    assert_eq!(result.errors.len(), 0);
}

#[tokio::test]
async fn stops_waiting_on_abort_when_a_provider_ignores_its_signal() {
    let controller = AbortScope::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    let wait = Arc::new(tokio::sync::Mutex::new(Some(wait)));
    let calls = Arc::new(AtomicUsize::new(0));
    let models = create_models(CreateModelsOptions::default());
    let started_for = Arc::clone(&started);
    let calls_for = Arc::clone(&calls);
    let wait_for = Arc::clone(&wait);
    models.set_provider(Arc::new(TestProvider::new("dynamic").with_refresh(
        Arc::new(move |_context| {
            let started = Arc::clone(&started_for);
            let calls = Arc::clone(&calls_for);
            let wait = Arc::clone(&wait_for);
            Box::pin(async move {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                if call != 0 {
                    return Ok(());
                }
                started.notify_one();
                if let Some(wait) = wait.lock().await.take() {
                    let _ = wait.await;
                }
                Ok(())
            })
        }),
    )));

    let models_for_refresh = Arc::clone(&models);
    let signal = controller.clone();
    let pending = tokio::spawn(async move {
        models_for_refresh
            .refresh(ModelsRefreshOptions {
                signal: Some(signal),
                ..Default::default()
            })
            .await
    });
    started.notified().await;
    controller.abort();
    let result = pending.await.unwrap().unwrap();
    assert!(result.aborted);
    assert_eq!(result.errors.len(), 0);

    let _ = release.send(());
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert_eq!(result.errors.len(), 0);
}

#[tokio::test]
async fn rejects_late_publication_from_a_superseded_non_cooperative_provider() {
    let store = Arc::new(InMemoryModelsStore::new());
    let state = Arc::new(Mutex::new("initial".to_owned()));
    let calls = Arc::new(AtomicUsize::new(0));
    let first_started = Arc::new(tokio::sync::Notify::new());
    let (finish_first, first_blocked) = tokio::sync::oneshot::channel::<()>();
    let first_blocked = Arc::new(tokio::sync::Mutex::new(Some(first_blocked)));
    let models = create_models(CreateModelsOptions {
        models_store: Some(store.clone()),
        ..Default::default()
    });
    let state_for = Arc::clone(&state);
    let calls_for = Arc::clone(&calls);
    let first_started_for = Arc::clone(&first_started);
    let first_blocked_for = Arc::clone(&first_blocked);
    models.set_provider(Arc::new(TestProvider::new("dynamic").with_refresh(
        Arc::new(move |context| {
            let state = Arc::clone(&state_for);
            let calls = Arc::clone(&calls_for);
            let first_started = Arc::clone(&first_started_for);
            let first_blocked = Arc::clone(&first_blocked_for);
            Box::pin(async move {
                if !context.allow_network {
                    return Ok(());
                }
                let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if call == 1 {
                    first_started.notify_one();
                    if let Some(blocked) = first_blocked.lock().await.take() {
                        let _ = blocked.await;
                    }
                }
                let value = format!("generation-{call}");
                (context.publish)(kiana_ai::models::ModelsPublication {
                    persist: ModelsPublicationPersist::Entry(ModelsStoreEntry {
                        models: vec![chat(test_model("dynamic", &value))],
                        ..Default::default()
                    }),
                    update: Some(Box::new({
                        let state = Arc::clone(&state);
                        let value = value.clone();
                        move || {
                            *state.lock().unwrap_or_else(|e| e.into_inner()) = value;
                        }
                    })),
                })
                .await?;
                Ok(())
            })
        }),
    )));

    let first_models = Arc::clone(&models);
    let first = tokio::spawn(async move {
        first_models
            .refresh(ModelsRefreshOptions {
                providers: Some(vec!["dynamic".into()]),
                ..Default::default()
            })
            .await
    });
    first_started.notified().await;
    let second_models = Arc::clone(&models);
    tokio::spawn(async move {
        second_models
            .refresh(ModelsRefreshOptions {
                providers: Some(vec!["dynamic".into()]),
                ..Default::default()
            })
            .await
    })
    .await
    .unwrap()
    .unwrap();
    let _ = first.await;
    let _ = finish_first.send(());
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    assert_eq!(
        *state.lock().unwrap_or_else(|e| e.into_inner()),
        "generation-2"
    );
    assert_eq!(
        store
            .read("dynamic", Default::default())
            .await
            .unwrap()
            .unwrap()
            .models[0]
            .model()
            .id,
        "generation-2"
    );
}

struct CapturingAuth {
    received: Arc<Mutex<Vec<AbortScope>>>,
    login_credential: Option<ApiKeyCredential>,
}

#[async_trait]
impl ApiKeyAuth for CapturingAuth {
    fn name(&self) -> &str {
        "Signal auth"
    }
    fn has_login(&self) -> bool {
        self.login_credential.is_some()
    }
    fn has_check(&self) -> bool {
        true
    }
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, AuthError> {
        self.received
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(interaction.signal().clone());
        Ok(self.login_credential.clone().unwrap_or_default())
    }
    async fn check(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthCheck>, AuthError> {
        self.received
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(input.signal.clone());
        Ok(Some(AuthCheck {
            source: None,
            r#type: AuthType::ApiKey,
        }))
    }
    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        self.received
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(input.signal.clone());
        Ok(Some(AuthResult::new(ModelAuth {
            api_key: Some("resolved".into()),
            ..Default::default()
        })))
    }
}

struct TestInteraction {
    signal: AbortScope,
}

#[async_trait]
impl AuthInteraction for TestInteraction {
    fn signal(&self) -> &AbortScope {
        &self.signal
    }
    async fn prompt(
        &self,
        _request: kiana_ai::auth::types::AuthPromptRequest,
    ) -> Result<String, AuthError> {
        Err(AuthError::new("unused"))
    }
    fn notify(&self, _event: &kiana_ai::auth::types::AuthEvent) {}
}

#[tokio::test]
async fn passes_caller_signals_to_provider_auth_callbacks() {
    let controller = AbortScope::new();
    let received = Arc::new(Mutex::new(Vec::new()));
    let auth = ProviderAuth {
        api_key: Some(Arc::new(CapturingAuth {
            received: Arc::clone(&received),
            login_credential: Some(ApiKeyCredential {
                key: Some("saved".into()),
                ..Default::default()
            }),
        })),
        oauth: None,
    };
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(TestProvider::new("p1").with_auth(auth)));

    models
        .check_auth(
            "p1",
            AuthOperationOptions {
                signal: Some(controller.clone()),
            },
        )
        .await
        .unwrap();
    models
        .get_auth(
            GetAuthTarget::Provider("p1"),
            AuthResolutionOverrides {
                signal: Some(controller.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    models
        .login(
            "p1",
            AuthType::ApiKey,
            &TestInteraction {
                signal: controller.clone(),
            },
            LoginOptions::default(),
        )
        .await
        .unwrap();

    let received = received.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(received.len(), 3);
    controller.abort();
    assert!(received.iter().all(AbortScope::is_aborted));
}

struct BlockedCheckAuth {
    check_started: Arc<tokio::sync::Notify>,
    check_finish: Arc<tokio::sync::Notify>,
    resolve_started: Arc<tokio::sync::Notify>,
    resolve_finish: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ApiKeyAuth for BlockedCheckAuth {
    fn name(&self) -> &str {
        "Blocked auth"
    }
    fn has_check(&self) -> bool {
        true
    }
    async fn check(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthCheck>, AuthError> {
        self.check_started.notify_one();
        self.check_finish.notified().await;
        Ok(Some(AuthCheck {
            source: None,
            r#type: AuthType::ApiKey,
        }))
    }
    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        self.resolve_started.notify_one();
        self.resolve_finish.notified().await;
        Ok(Some(AuthResult::new(ModelAuth {
            api_key: Some("key".into()),
            ..Default::default()
        })))
    }
}

#[tokio::test]
async fn stops_waiting_for_non_cooperative_auth_callbacks() {
    let check_started = Arc::new(tokio::sync::Notify::new());
    let check_finish = Arc::new(tokio::sync::Notify::new());
    let resolve_started = Arc::new(tokio::sync::Notify::new());
    let resolve_finish = Arc::new(tokio::sync::Notify::new());
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(TestProvider::new("p1").with_auth(ProviderAuth {
        api_key: Some(Arc::new(BlockedCheckAuth {
            check_started: Arc::clone(&check_started),
            check_finish: Arc::clone(&check_finish),
            resolve_started: Arc::clone(&resolve_started),
            resolve_finish: Arc::clone(&resolve_finish),
        })),
        oauth: None,
    })));

    let available_controller = AbortScope::new();
    let models_for_available = Arc::clone(&models);
    let available_signal = available_controller.clone();
    let available = tokio::spawn(async move {
        models_for_available
            .get_available(
                None,
                AuthOperationOptions {
                    signal: Some(available_signal),
                },
            )
            .await
    });
    check_started.notified().await;
    available_controller.abort();
    let error = available.await.unwrap().unwrap_err();
    assert_eq!(error.name, "AbortError");

    let auth_controller = AbortScope::new();
    let models_for_auth = Arc::clone(&models);
    let auth_signal = auth_controller.clone();
    let auth = tokio::spawn(async move {
        models_for_auth
            .get_auth(
                GetAuthTarget::Provider("p1"),
                AuthResolutionOverrides {
                    signal: Some(auth_signal),
                    ..Default::default()
                },
            )
            .await
    });
    resolve_started.notified().await;
    auth_controller.abort();
    let error = auth.await.unwrap().unwrap_err();
    assert_eq!(error.name, "AbortError");

    check_finish.notify_one();
    resolve_finish.notify_one();
}

#[tokio::test]
async fn cancels_queued_credential_mutations_without_running_them_later() {
    let credentials = InMemoryCredentialStore::new();
    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    let wait = Arc::new(tokio::sync::Mutex::new(Some(wait)));
    let wait_for_first = Arc::clone(&wait);
    let first = credentials.modify(
        "p1",
        Box::new(move |_| {
            let wait = Arc::clone(&wait_for_first);
            Box::pin(async move {
                if let Some(wait) = wait.lock().await.take() {
                    let _ = wait.await;
                }
                Ok(Some(Credential::ApiKey(ApiKeyCredential {
                    key: Some("first".into()),
                    ..Default::default()
                })))
            })
        }),
        Default::default(),
    );
    let second_ran = Arc::new(AtomicBool::new(false));
    let second_ran_for = Arc::clone(&second_ran);
    let controller = AbortScope::new();
    let second = credentials.modify(
        "p1",
        Box::new(move |_| {
            second_ran_for.store(true, Ordering::SeqCst);
            Box::pin(async {
                Ok(Some(Credential::ApiKey(ApiKeyCredential {
                    key: Some("second".into()),
                    ..Default::default()
                })))
            })
        }),
        AuthOperationOptions {
            signal: Some(controller.clone()),
        },
    );

    controller.abort();
    let error = second.await.unwrap_err();
    assert_eq!(error.name, "AbortError");
    let _ = release.send(());
    first.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert!(!second_ran.load(Ordering::SeqCst));
    assert_eq!(
        credentials.read("p1", Default::default()).await.unwrap(),
        Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("first".into()),
            ..Default::default()
        }))
    );
}

#[tokio::test]
async fn persists_an_oauth_refresh_that_started_before_the_request_was_cancelled() {
    let credentials = InMemoryCredentialStore::new();
    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "old",
                        "old-refresh",
                        0.0,
                    ))))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    let controller = AbortScope::new();
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials.clone())),
        ..Default::default()
    });
    let controller_for_refresh = controller.clone();
    let oauth = TestOAuth::with_refresh(Arc::new(move |credential| {
        let controller = controller_for_refresh.clone();
        Box::pin(async move {
            controller.abort();
            Ok(OAuthCredential {
                access: "new".into(),
                refresh: "new-refresh".into(),
                expires: now_ms() as f64 + 60_000.0,
                ..credential
            })
        })
    }));
    models.set_provider(Arc::new(
        TestProvider::new("p1").with_auth(oauth_auth(oauth)),
    ));

    let error = models
        .get_auth(
            GetAuthTarget::Provider("p1"),
            AuthResolutionOverrides {
                signal: Some(controller.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.name, "AbortError");
    let credentials_for_wait = credentials.clone();
    wait_for(move || {
        let credentials = credentials_for_wait.clone();
        let value = futures::executor::block_on(credentials.read("p1", Default::default()))
            .ok()
            .flatten();
        matches!(
            value,
            Some(Credential::OAuth(OAuthCredential { refresh, .. })) if refresh == "new-refresh"
        )
    })
    .await;
}

#[tokio::test]
async fn resolves_auth_stored_credential_owns_the_provider_ambient_only_when_nothing_stored() {
    let credentials = InMemoryCredentialStore::new();
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials.clone())),
        ..Default::default()
    });
    models.set_provider(Arc::new(TestProvider::new("p1").with_auth(ProviderAuth {
        api_key: Some(Arc::new(EnvKeyAuth {
            key: Some("env-key".into()),
        })),
        oauth: Some(test_oauth()),
    })));
    let model = test_model("p1", "model-a");

    assert_eq!(
        models
            .get_auth(GetAuthTarget::Model(&model), Default::default())
            .await
            .unwrap()
            .unwrap()
            .auth
            .api_key
            .as_deref(),
        Some("env-key")
    );
    assert_eq!(
        models
            .get_auth(GetAuthTarget::Provider("p1"), Default::default())
            .await
            .unwrap()
            .unwrap()
            .auth
            .api_key
            .as_deref(),
        Some("env-key")
    );
    assert_eq!(
        models
            .get_auth(
                GetAuthTarget::Model(&model),
                AuthResolutionOverrides {
                    api_key: Some("explicit-key".into()),
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .unwrap()
            .auth
            .api_key
            .as_deref(),
        Some("explicit-key")
    );

    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "oauth-token",
                        "r",
                        now_ms() as f64 + 10.0 * 60_000.0,
                    ))))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    let resolution = models
        .get_auth(GetAuthTarget::Provider("p1"), Default::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolution.auth.api_key.as_deref(), Some("oauth-token"));
    assert_eq!(resolution.source.as_deref(), Some("OAuth"));

    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("stored-key".into()),
                        ..Default::default()
                    })))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    let resolution = models
        .get_auth(GetAuthTarget::Provider("p1"), Default::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolution.auth.api_key.as_deref(), Some("stored-key"));
    assert_eq!(resolution.source.as_deref(), Some("stored"));
}

#[tokio::test]
async fn checks_provider_auth_without_refreshing_oauth_and_filters_available_models() {
    let credentials = InMemoryCredentialStore::new();
    let refreshes = Arc::new(AtomicUsize::new(0));
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials.clone())),
        ..Default::default()
    });
    models.set_provider(Arc::new(
        TestProvider::new("ambient").with_auth(env_key_auth(Some("env-key"))),
    ));
    models.set_provider(Arc::new(
        TestProvider::new("missing").with_auth(env_key_auth(None)),
    ));
    let refreshes_for = Arc::clone(&refreshes);
    let oauth = TestOAuth::with_refresh(Arc::new(move |credential| {
        refreshes_for.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(credential) })
    }));
    models.set_provider(Arc::new(
        TestProvider::new("oauth").with_auth(oauth_auth(oauth)),
    ));
    credentials
        .modify(
            "oauth",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "expired", "refresh", 0.0,
                    ))))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();

    assert_eq!(
        models
            .check_auth("ambient", Default::default())
            .await
            .unwrap(),
        Some(AuthCheck {
            source: Some("env".into()),
            r#type: AuthType::ApiKey
        })
    );
    assert!(
        models
            .check_auth("missing", Default::default())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        models
            .check_auth("oauth", Default::default())
            .await
            .unwrap(),
        Some(AuthCheck {
            source: Some("OAuth".into()),
            r#type: AuthType::OAuth
        })
    );
    assert_eq!(refreshes.load(Ordering::SeqCst), 0);
    assert_eq!(
        models
            .get_available(None, Default::default())
            .await
            .unwrap()
            .iter()
            .map(|model| model.provider.clone())
            .collect::<Vec<_>>(),
        ["ambient", "oauth"]
    );
    assert_eq!(
        models
            .get_available(Some("ambient"), Default::default())
            .await
            .unwrap()
            .iter()
            .map(|model| model.provider.clone())
            .collect::<Vec<_>>(),
        ["ambient"]
    );
}

struct LoginAuth;

#[async_trait]
impl ApiKeyAuth for LoginAuth {
    fn name(&self) -> &str {
        "Test API key"
    }
    fn has_login(&self) -> bool {
        true
    }
    async fn login(
        &self,
        _interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, AuthError> {
        Ok(ApiKeyCredential {
            key: Some("logged-in".into()),
            ..Default::default()
        })
    }
    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(None)
    }
}

#[tokio::test]
async fn runs_provider_login_and_logout_through_the_credential_store() {
    let credentials = InMemoryCredentialStore::new();
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials.clone())),
        ..Default::default()
    });
    models.set_provider(Arc::new(TestProvider::new("p1").with_auth(ProviderAuth {
        api_key: Some(Arc::new(LoginAuth)),
        oauth: None,
    })));

    let interaction = TestInteraction {
        signal: AbortScope::new(),
    };
    let credential = models
        .login(
            "p1",
            AuthType::ApiKey,
            &interaction,
            LoginOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        credential,
        Credential::ApiKey(ApiKeyCredential {
            key: Some("logged-in".into()),
            ..Default::default()
        })
    );
    assert_eq!(
        credentials.read("p1", Default::default()).await.unwrap(),
        Some(credential)
    );

    models.logout("p1", Default::default()).await.unwrap();
    assert!(
        credentials
            .read("p1", Default::default())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_stored_credential_without_a_matching_handler_blocks_ambient_fallback() {
    let credentials = InMemoryCredentialStore::new();
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials.clone())),
        ..Default::default()
    });
    models.set_provider(Arc::new(
        TestProvider::new("p1").with_auth(env_key_auth(Some("env-key"))),
    ));
    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async { Ok(Some(Credential::OAuth(OAuthCredential::new("a", "r", 0.0)))) })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    assert!(
        models
            .get_auth(GetAuthTarget::Provider("p1"), Default::default())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn refreshes_expired_oauth_credentials_and_persists_the_rotated_credential() {
    let credentials = InMemoryCredentialStore::new();
    let oauth = TestOAuth::with_refresh(Arc::new(|credential| {
        Box::pin(async move {
            Ok(OAuthCredential {
                access: "new-token".into(),
                expires: now_ms() as f64 + 60.0 * 60_000.0,
                ..credential
            })
        })
    }));
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials.clone())),
        ..Default::default()
    });
    models.set_provider(Arc::new(
        TestProvider::new("p1").with_auth(oauth_auth(oauth)),
    ));
    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "old-token",
                        "r",
                        0.0,
                    ))))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();

    let resolution = models
        .get_auth(GetAuthTarget::Provider("p1"), Default::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolution.auth.api_key.as_deref(), Some("new-token"));
    match credentials.read("p1", Default::default()).await.unwrap() {
        Some(Credential::OAuth(credential)) => assert_eq!(credential.access, "new-token"),
        other => panic!("expected oauth credential, got {other:?}"),
    }
}

#[tokio::test]
async fn refreshes_oauth_credentials_with_less_than_five_minutes_remaining() {
    let credentials = InMemoryCredentialStore::new();
    let refresh = Arc::new(AtomicUsize::new(0));
    let refresh_for = Arc::clone(&refresh);
    let oauth = TestOAuth::with_refresh(Arc::new(move |credential| {
        refresh_for.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(OAuthCredential {
                access: "new-token".into(),
                expires: now_ms() as f64 + 60.0 * 60_000.0,
                ..credential
            })
        })
    }));
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials.clone())),
        ..Default::default()
    });
    models.set_provider(Arc::new(
        TestProvider::new("p1").with_auth(oauth_auth(oauth)),
    ));
    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "old-token",
                        "r",
                        now_ms() as f64 + 60_000.0,
                    ))))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();

    assert_eq!(
        models
            .get_auth(GetAuthTarget::Provider("p1"), Default::default())
            .await
            .unwrap()
            .unwrap()
            .auth
            .api_key
            .as_deref(),
        Some("new-token")
    );
    assert_eq!(refresh.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn honors_a_callers_longer_oauth_minimum_validity() {
    let credentials = InMemoryCredentialStore::new();
    let refresh = Arc::new(AtomicUsize::new(0));
    let refresh_for = Arc::clone(&refresh);
    let oauth = TestOAuth::with_refresh(Arc::new(move |credential| {
        refresh_for.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(OAuthCredential {
                access: "new-token".into(),
                expires: now_ms() as f64 + 60.0 * 60_000.0,
                ..credential
            })
        })
    }));
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials.clone())),
        ..Default::default()
    });
    models.set_provider(Arc::new(
        TestProvider::new("p1").with_auth(oauth_auth(oauth)),
    ));
    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "old-token",
                        "r",
                        now_ms() as f64 + 10.0 * 60_000.0,
                    ))))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();

    assert_eq!(
        models
            .get_auth(
                GetAuthTarget::Provider("p1"),
                AuthResolutionOverrides {
                    min_oauth_validity_ms: Some(30 * 60_000),
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .unwrap()
            .auth
            .api_key
            .as_deref(),
        Some("new-token")
    );
    assert_eq!(refresh.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rejects_with_code_oauth_when_refresh_fails_preserving_the_stored_credential() {
    let credentials = InMemoryCredentialStore::new();
    let oauth = TestOAuth::with_refresh(Arc::new(|_credential| {
        Box::pin(async { Err(AuthError::new("invalid_grant")) })
    }));
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials.clone())),
        ..Default::default()
    });
    models.set_provider(Arc::new(
        TestProvider::new("p1").with_auth(oauth_auth(oauth)),
    ));
    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "old", "r", 0.0,
                    ))))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();

    let error = models
        .get_auth(GetAuthTarget::Provider("p1"), Default::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, Some(ModelsErrorCode::OAuth));
    match credentials.read("p1", Default::default()).await.unwrap() {
        Some(Credential::OAuth(credential)) => assert_eq!(credential.access, "old"),
        other => panic!("expected preserved oauth credential, got {other:?}"),
    }
}

#[tokio::test]
async fn serializes_concurrent_oauth_refreshes_through_store_modify_without_double_refresh() {
    let credentials = InMemoryCredentialStore::new();
    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "old", "r1", 0.0,
                    ))))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    let refreshes = Arc::new(AtomicUsize::new(0));
    let refreshes_for = Arc::clone(&refreshes);
    let oauth = TestOAuth::with_refresh(Arc::new(move |_credential| {
        let refreshes = Arc::clone(&refreshes_for);
        Box::pin(async move {
            let count = refreshes.fetch_add(1, Ordering::SeqCst) + 1;
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            Ok(OAuthCredential::new(
                format!("new-{count}"),
                "r2",
                now_ms() as f64 + 60.0 * 60_000.0,
            ))
        })
    }));
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials)),
        ..Default::default()
    });
    models.set_provider(Arc::new(
        TestProvider::new("p1").with_auth(oauth_auth(oauth)),
    ));
    let (a, b) = tokio::join!(
        models.get_auth(GetAuthTarget::Provider("p1"), Default::default()),
        models.get_auth(GetAuthTarget::Provider("p1"), Default::default()),
    );
    let a = a.unwrap().unwrap();
    let b = b.unwrap().unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(a.auth.api_key.as_deref(), Some("new-1"));
    assert_eq!(b.auth.api_key.as_deref(), Some("new-1"));
}

struct CountingStore {
    base: InMemoryCredentialStore,
    modifies: Arc<AtomicUsize>,
}

#[async_trait]
impl CredentialStore for CountingStore {
    async fn read(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        self.base.read(provider_id, options).await
    }
    async fn list(&self, options: AuthOperationOptions) -> Result<Vec<CredentialInfo>, AuthError> {
        self.base.list(options).await
    }
    async fn modify(
        &self,
        provider_id: &str,
        f: CredentialModifyFn,
        options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        self.modifies.fetch_add(1, Ordering::SeqCst);
        self.base.modify(provider_id, f, options).await
    }
    async fn delete(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<(), AuthError> {
        self.base.delete(provider_id, options).await
    }
}

#[tokio::test]
async fn valid_oauth_tokens_resolve_without_touching_modify() {
    let base = InMemoryCredentialStore::new();
    let modifies = Arc::new(AtomicUsize::new(0));
    let credentials = CountingStore {
        base: base.clone(),
        modifies: Arc::clone(&modifies),
    };
    base.modify(
        "p1",
        Box::new(|_| {
            Box::pin(async {
                Ok(Some(Credential::OAuth(OAuthCredential::new(
                    "valid",
                    "r",
                    now_ms() as f64 + 10.0 * 60_000.0,
                ))))
            })
        }),
        Default::default(),
    )
    .await
    .unwrap();
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials)),
        ..Default::default()
    });
    models.set_provider(Arc::new(
        TestProvider::new("p1").with_auth(oauth_auth(test_oauth())),
    ));
    assert_eq!(
        models
            .get_auth(GetAuthTarget::Provider("p1"), Default::default())
            .await
            .unwrap()
            .unwrap()
            .auth
            .api_key
            .as_deref(),
        Some("valid")
    );
    assert_eq!(modifies.load(Ordering::SeqCst), 0);
}

struct FailingReadStore;

#[async_trait]
impl CredentialStore for FailingReadStore {
    async fn read(
        &self,
        _provider_id: &str,
        _options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        Err(AuthError::new("disk on fire"))
    }
    async fn list(&self, _options: AuthOperationOptions) -> Result<Vec<CredentialInfo>, AuthError> {
        Ok(vec![])
    }
    async fn modify(
        &self,
        _provider_id: &str,
        _f: CredentialModifyFn,
        _options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        Ok(None)
    }
    async fn delete(
        &self,
        _provider_id: &str,
        _options: AuthOperationOptions,
    ) -> Result<(), AuthError> {
        Ok(())
    }
}

struct FailingModifyStore;

#[async_trait]
impl CredentialStore for FailingModifyStore {
    async fn read(
        &self,
        _provider_id: &str,
        _options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        Ok(Some(Credential::OAuth(OAuthCredential::new(
            "old", "r", 0.0,
        ))))
    }
    async fn list(&self, _options: AuthOperationOptions) -> Result<Vec<CredentialInfo>, AuthError> {
        Ok(vec![CredentialInfo {
            provider_id: "p1".into(),
            r#type: AuthType::OAuth,
        }])
    }
    async fn modify(
        &self,
        _provider_id: &str,
        _f: CredentialModifyFn,
        _options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        Err(AuthError::new("disk on fire"))
    }
    async fn delete(
        &self,
        _provider_id: &str,
        _options: AuthOperationOptions,
    ) -> Result<(), AuthError> {
        Ok(())
    }
}

#[tokio::test]
async fn wraps_credential_store_failures_in_models_error() {
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(FailingReadStore)),
        ..Default::default()
    });
    models.set_provider(Arc::new(
        TestProvider::new("p1").with_auth(env_key_auth(Some("env-key"))),
    ));
    let error = models
        .get_auth(GetAuthTarget::Provider("p1"), Default::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, Some(ModelsErrorCode::Auth));

    // TS: readCredential 抛 ModelsError("auth")，checkAuth/getAvailable/getAllAvailable 必须
    // 保留同一 code（getAuth 由上面的断言覆盖）。
    let error = models
        .check_auth("p1", Default::default())
        .await
        .unwrap_err();
    assert_eq!(error.name, "ModelsError");
    assert_eq!(error.code, Some(ModelsErrorCode::Auth));
    assert_eq!(
        error.message,
        "Credential store read failed for p1: disk on fire"
    );
    let error = models
        .get_available(None, Default::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, Some(ModelsErrorCode::Auth));
    let error = models
        .get_all_available(None, Default::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, Some(ModelsErrorCode::Auth));
    let error = models
        .get_available_of_type(kiana_ai::types::ModelType::Chat, None, Default::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, Some(ModelsErrorCode::Auth));

    let oauth_models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(FailingModifyStore)),
        ..Default::default()
    });
    oauth_models.set_provider(Arc::new(
        TestProvider::new("p1").with_auth(oauth_auth(test_oauth())),
    ));
    let error = oauth_models
        .get_auth(GetAuthTarget::Provider("p1"), Default::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, Some(ModelsErrorCode::Auth));
}

#[tokio::test]
async fn keeps_the_underlying_reason_in_wrapped_oauth_refresh_errors() {
    let credentials = InMemoryCredentialStore::new();
    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "old", "r", 0.0,
                    ))))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials)),
        ..Default::default()
    });
    let oauth = TestOAuth::with_refresh(Arc::new(|_credential| {
        Box::pin(async { Err(AuthError::new("token refresh failed (400): invalid_grant")) })
    }));
    models.set_provider(Arc::new(
        TestProvider::new("p1").with_auth(oauth_auth(oauth)),
    ));
    let error = models
        .get_auth(GetAuthTarget::Provider("p1"), Default::default())
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        "OAuth refresh failed for p1: token refresh failed (400): invalid_grant"
    );
}

struct FailingResolveAuth;

#[async_trait]
impl ApiKeyAuth for FailingResolveAuth {
    fn name(&self) -> &str {
        "Failing"
    }
    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Err(AuthError::new("nope"))
    }
}

#[tokio::test]
async fn wraps_api_key_auth_failures_in_models_error() {
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(TestProvider::new("p1").with_auth(ProviderAuth {
        api_key: Some(Arc::new(FailingResolveAuth)),
        oauth: None,
    })));
    let error = models
        .get_auth(GetAuthTarget::Provider("p1"), Default::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, Some(ModelsErrorCode::Auth));
}

struct ScopedAuth;

#[async_trait]
impl ApiKeyAuth for ScopedAuth {
    fn name(&self) -> &str {
        "Scoped"
    }
    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        let account = match input.credential.and_then(|c| c.env.as_ref()) {
            Some(env) => env.get("ACCOUNT_ID").cloned(),
            None => input.ctx.env("ACCOUNT_ID").await,
        };
        let key = input
            .credential
            .and_then(|credential| credential.key.clone());
        let (Some(key), Some(account)) = (key, account) else {
            return Ok(None);
        };
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(key),
                base_url: Some(format!("https://example.test/{account}")),
                ..Default::default()
            },
            env: Some([("ACCOUNT_ID".to_string(), account)].into_iter().collect()),
            source: None,
        }))
    }
}

#[tokio::test]
async fn uses_explicit_request_api_key_and_env_during_provider_auth_resolution() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(
        TestProvider::new("p1")
            .with_auth(ProviderAuth {
                api_key: Some(Arc::new(ScopedAuth)),
                oauth: None,
            })
            .with_calls(Arc::clone(&calls)),
    ));
    let model = test_model("p1", "model-a");

    models
        .complete_simple(
            &model,
            &context(),
            ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        api_key: Some("explicit-key".into()),
                        env: Some(
                            [("ACCOUNT_ID".to_string(), "acct".to_string())]
                                .into_iter()
                                .collect(),
                        ),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;

    let calls = calls.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls[0].model.base_url, "https://example.test/acct");
    assert_eq!(calls[0].options.api_key.as_deref(), Some("explicit-key"));
    assert_eq!(
        calls[0]
            .options
            .env
            .as_ref()
            .and_then(|env| env.get("ACCOUNT_ID"))
            .map(String::as_str),
        Some("acct")
    );
}

struct HeaderAuth;

#[async_trait]
impl ApiKeyAuth for HeaderAuth {
    fn name(&self) -> &str {
        "Test"
    }
    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        let mut headers = ProviderHeaders::new();
        headers.insert("Authorization".into(), Some("Bearer resolved-key".into()));
        headers.insert("x-a".into(), Some("auth".into()));
        headers.insert("x-b".into(), Some("auth".into()));
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some("resolved-key".into()),
                headers: Some(headers),
                base_url: Some("https://auth.test/v1".into()),
            },
            env: None,
            source: None,
        }))
    }
}

#[tokio::test]
async fn merges_resolved_auth_into_stream_options_explicit_options_win_per_field() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(
        TestProvider::new("p1")
            .with_auth(ProviderAuth {
                api_key: Some(Arc::new(HeaderAuth)),
                oauth: None,
            })
            .with_calls(Arc::clone(&calls)),
    ));
    let model = test_model("p1", "model-a");

    let mut explicit_headers = ProviderHeaders::new();
    explicit_headers.insert("authorization".into(), Some("Explicit token".into()));
    explicit_headers.insert("x-b".into(), Some("explicit".into()));
    let result = models
        .complete_simple(
            &model,
            &context(),
            ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        api_key: Some("explicit-key".into()),
                        headers: Some(explicit_headers),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    {
        let first_calls = calls.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(first_calls.len(), 1);
        assert_eq!(
            first_calls[0].options.api_key.as_deref(),
            Some("explicit-key")
        );
        let headers = first_calls[0].options.headers.as_ref().unwrap();
        assert_eq!(
            headers.get("authorization").cloned().flatten().as_deref(),
            Some("Explicit token")
        );
        assert_eq!(
            headers.get("x-a").cloned().flatten().as_deref(),
            Some("auth")
        );
        assert_eq!(
            headers.get("x-b").cloned().flatten().as_deref(),
            Some("explicit")
        );
        assert_eq!(first_calls[0].model.base_url, "https://auth.test/v1");
    }

    // without explicit options, resolved auth applies
    let result2 = models
        .complete_simple(&model, &context(), simple_options())
        .await;
    assert_eq!(result2.stop_reason, kiana_ai::types::StopReason::Stop);
    let calls = calls.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls[1].options.api_key.as_deref(), Some("resolved-key"));
}

#[tokio::test]
async fn adds_model_headers_only_for_model_auth_and_transforms_assembled_headers_once() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(
        TestProvider::new("p1")
            .with_auth(env_key_auth(Some("key")))
            .with_calls(Arc::clone(&calls)),
    ));
    let mut model = test_model("p1", "model-a");
    let mut model_headers = indexmap::IndexMap::new();
    model_headers.insert("x-model".to_string(), "model".to_string());
    model_headers.insert("x-shared".to_string(), "model".to_string());
    model.headers = Some(model_headers);

    assert!(
        models
            .get_auth(GetAuthTarget::Provider("p1"), Default::default())
            .await
            .unwrap()
            .unwrap()
            .auth
            .headers
            .is_none()
    );
    let model_auth = models
        .get_auth(GetAuthTarget::Model(&model), Default::default())
        .await
        .unwrap()
        .unwrap();
    let model_headers = model_auth.auth.headers.unwrap();
    assert_eq!(
        model_headers.get("x-model").cloned().flatten().as_deref(),
        Some("model")
    );
    assert_eq!(
        model_headers.get("x-shared").cloned().flatten().as_deref(),
        Some("model")
    );

    let transforms = Arc::new(AtomicUsize::new(0));
    let transforms_for = Arc::clone(&transforms);
    let mut explicit_headers = ProviderHeaders::new();
    explicit_headers.insert("x-explicit".into(), Some("explicit".into()));
    explicit_headers.insert("X-Shared".into(), Some("explicit".into()));
    models
        .complete_simple(
            &model,
            &context(),
            ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        headers: Some(explicit_headers),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transforms: kiana_ai::models::ModelsRequestTransforms {
                    transform_headers: Some(Arc::new(move |headers: ProviderHeaders| {
                        transforms_for.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(
                            headers
                                .iter()
                                .map(|(name, value)| (
                                    name.clone(),
                                    value.clone().unwrap_or_default()
                                ))
                                .collect::<Vec<_>>(),
                            vec![
                                ("x-model".to_string(), "model".to_string()),
                                ("x-explicit".to_string(), "explicit".to_string()),
                                ("X-Shared".to_string(), "explicit".to_string()),
                            ]
                        );
                        let mut headers = headers;
                        headers.insert("x-transformed".into(), Some("yes".into()));
                        Box::pin(async move { headers })
                    })),
                },
                signal_reason: None,
            },
        )
        .await;

    assert_eq!(transforms.load(Ordering::SeqCst), 1);
    let calls = calls.lock().unwrap_or_else(|e| e.into_inner());
    let headers = calls[0].options.headers.as_ref().unwrap();
    assert_eq!(
        headers.get("x-transformed").cloned().flatten().as_deref(),
        Some("yes")
    );
    assert_eq!(
        headers.get("x-explicit").cloned().flatten().as_deref(),
        Some("explicit")
    );
}

#[tokio::test]
async fn produces_an_error_stream_for_unknown_providers_instead_of_throwing() {
    let models = create_models(CreateModelsOptions::default());
    let result = models
        .complete_simple(
            &test_model("ghost", "model-a"),
            &context(),
            simple_options(),
        )
        .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap()
            .contains("Unknown provider: ghost")
    );
}

#[tokio::test]
async fn streams_through_the_provider() {
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(TestProvider::new("p1")));
    let model = test_model("p1", "model-a");

    let stream = models.stream_simple(&model, &context(), simple_options());
    let events = collect_events(&stream).await;
    assert_eq!(
        events
            .iter()
            .map(AssistantMessageEvent::kind)
            .collect::<Vec<_>>(),
        ["start", "done"]
    );
    let message = stream.result().await.unwrap();
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Stop);
}

// ---------------------------------------------------------------------------
// 回归测试（reviewer findings）
// ---------------------------------------------------------------------------

/// `ModelsApiStreamOptions<TApi>` 的 API 专属/任意额外字段必须原样到达 provider
/// （保持扁平展开、不做筛选）；`ModelsStreamOptions.reasoning` 既用于 telemetry
/// 也必须透传。
#[tokio::test]
async fn forwards_per_api_and_extra_stream_options_to_the_provider() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(
        TestProvider::new("p1").with_calls(Arc::clone(&calls)),
    ));

    let mut extra = kiana_ai::types::JsonObject::new();
    extra.insert("reasoningEffort".into(), serde_json::json!("xhigh"));
    extra.insert("thinkingEnabled".into(), serde_json::json!(true));
    extra.insert("serviceTier".into(), serde_json::json!("priority"));

    models
        .complete(
            &test_model("p1", "model-a"),
            &context(),
            ModelsStreamOptions {
                base: StreamOptions {
                    extra,
                    ..Default::default()
                },
                reasoning: Some("high".into()),
                ..Default::default()
            },
        )
        .await;

    let calls = calls.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 1);
    let options = &calls[0].options;
    assert_eq!(
        options.extra.get("reasoningEffort"),
        Some(&serde_json::json!("xhigh"))
    );
    assert_eq!(
        options.extra.get("thinkingEnabled"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        options.extra.get("serviceTier"),
        Some(&serde_json::json!("priority"))
    );
    // TS 的 providerOptions 展开包含同一对象上的 `reasoning`。
    assert_eq!(
        options.extra.get("reasoning"),
        Some(&serde_json::json!("high"))
    );
}

/// TS 的 `AbortController.abort()` 缺省 reason 是 `"This operation was aborted"`；
/// 请求路径（applyAuth）必须报告该文案，而不是 `abortReason()` 的回退文案。
#[tokio::test]
async fn request_setup_reports_the_node_default_abort_reason() {
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(TestProvider::new("p1")));
    let scope = AbortScope::new();
    scope.abort();

    let result = models
        .complete(
            &test_model("p1", "model-a"),
            &context(),
            ModelsStreamOptions {
                base: StreamOptions {
                    signal: Some(scope.token()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("This operation was aborted")
    );
}

/// TS 的 `controller.abort(new Error("Cancelled"))`：请求路径必须把自定义 reason 的
/// `message` 作为流错误（冻结的 `StreamOptions.signal` 只带 token，reason 由
/// `signal_reason` 伴生字段传入）。
#[tokio::test]
async fn a_custom_abort_reason_is_surfaced_when_auth_is_aborted() {
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(TestProvider::new("p1")));
    let scope = AbortScope::new();
    scope.abort_with(AbortReason {
        name: "Error".into(),
        message: "Cancelled".into(),
    });
    let reason = AbortReason {
        name: "Error".into(),
        message: "Cancelled".into(),
    };

    let result = models
        .complete(
            &test_model("p1", "model-a"),
            &context(),
            ModelsStreamOptions {
                base: StreamOptions {
                    signal: Some(scope.token()),
                    ..Default::default()
                },
                signal_reason: Some(reason),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("Cancelled"));
}

/// 门闩式 api-key auth：`resolve` 进入时放行 `started`，然后等待 `gate`（模拟慢 auth）。
struct GatedAuth {
    started: Arc<tokio::sync::Semaphore>,
    gate: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl ApiKeyAuth for GatedAuth {
    fn name(&self) -> &str {
        "Gated"
    }
    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        self.started.add_permits(1);
        let permit = self
            .gate
            .acquire()
            .await
            .map_err(|_| AuthError::new("gate closed"))?;
        permit.forget();
        Ok(Some(AuthResult::new(ModelAuth::default())))
    }
}

fn gated_auth() -> (
    ProviderAuth,
    Arc<tokio::sync::Semaphore>,
    Arc<tokio::sync::Semaphore>,
) {
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let auth = ProviderAuth {
        api_key: Some(Arc::new(GatedAuth {
            started: Arc::clone(&started),
            gate: Arc::clone(&gate),
        })),
        oauth: None,
    };
    (auth, started, gate)
}

/// `generateImages` 的 catch 分支按**错误发生时**的 signal 判定 `aborted`
/// （TS: `options?.signal?.aborted` in catch），auth 期间中止必须得到 stopReason "aborted"。
#[tokio::test]
async fn image_generation_aborted_during_auth_is_aborted() {
    use kiana_ai::models::{ImagesOptions, ProviderImages};
    use kiana_ai::types::{AssistantImages, ImagesContext, ImagesStopReason, ModelType};

    struct UnreachableImages;
    #[async_trait]
    impl ProviderImages for UnreachableImages {
        async fn generate_images(
            &self,
            _model: &Model,
            _context: &ImagesContext,
            _options: Option<&ImagesOptions>,
        ) -> AssistantImages {
            AssistantImages {
                stop_reason: ImagesStopReason::Stop,
                ..Default::default()
            }
        }
    }

    let (auth, started, gate) = gated_auth();
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "p1".into(),
            auth,
            models: vec![],
            images: Some(
                [(
                    "test-images".to_string(),
                    Arc::new(UnreachableImages) as Arc<dyn ProviderImages>,
                )]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        })
        .unwrap(),
    );

    let model = Model {
        id: "im".into(),
        name: "IM".into(),
        api: "test-images".into(),
        provider: "p1".into(),
        base_url: "https://example.test".into(),
        model_type: Some(ModelType::Image),
        ..Model::default()
    };
    let scope = AbortScope::new();
    let token = scope.token();
    let models_for_task = Arc::clone(&models);
    let task = tokio::spawn(async move {
        models_for_task
            .generate_images(
                &model,
                &ImagesContext::default(),
                kiana_ai::models::ModelsImagesOptions {
                    base: StreamOptions {
                        signal: Some(token),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
    });

    started.acquire().await.unwrap().forget();
    scope.abort();
    gate.add_permits(1);
    let result = task.await.unwrap();
    assert_eq!(result.stop_reason, ImagesStopReason::Aborted);
    assert_eq!(
        result.error_message.as_deref(),
        Some("This operation was aborted")
    );
}

/// `classify` 同上。
#[tokio::test]
async fn classifier_aborted_during_auth_is_aborted() {
    use kiana_ai::models::{ClassifierOptions, ProviderClassifier};
    use kiana_ai::types::{ClassifierContext, ClassifierResult, ClassifierStopReason, ModelType};

    struct UnreachableClassifier;
    #[async_trait]
    impl ProviderClassifier for UnreachableClassifier {
        async fn classify(
            &self,
            _model: &Model,
            _context: &ClassifierContext,
            _options: Option<&ClassifierOptions>,
        ) -> ClassifierResult {
            ClassifierResult::default()
        }
    }

    let (auth, started, gate) = gated_auth();
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "p1".into(),
            auth,
            models: vec![],
            classifiers: Some(
                [(
                    "test-classifiers".to_string(),
                    Arc::new(UnreachableClassifier) as Arc<dyn ProviderClassifier>,
                )]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        })
        .unwrap(),
    );

    let model = Model {
        id: "cm".into(),
        name: "CM".into(),
        api: "test-classifiers".into(),
        provider: "p1".into(),
        base_url: "https://example.test".into(),
        model_type: Some(ModelType::Classifier),
        ..Model::default()
    };
    let scope = AbortScope::new();
    let token = scope.token();
    let models_for_task = Arc::clone(&models);
    let task = tokio::spawn(async move {
        models_for_task
            .classify(
                &model,
                &ClassifierContext::default(),
                kiana_ai::models::ModelsClassifierOptions {
                    base: StreamOptions {
                        signal: Some(token),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
    });

    started.acquire().await.unwrap().forget();
    scope.abort();
    gate.add_permits(1);
    let result = task.await.unwrap();
    assert_eq!(result.stop_reason, ClassifierStopReason::Aborted);
    assert_eq!(
        result.error_message.as_deref(),
        Some("This operation was aborted")
    );
}

/// 空串 providerId 在 TS 里是 falsy：`getAvailable("")`/`getAllAvailable("")` 扫描所有 provider。
#[tokio::test]
async fn an_empty_provider_id_scans_every_provider() {
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(TestProvider::new("")));
    models.set_provider(Arc::new(TestProvider::new("normal")));

    let available: Vec<String> = models
        .get_available(Some(""), Default::default())
        .await
        .unwrap()
        .into_iter()
        .map(|model| model.provider)
        .collect();
    assert_eq!(available, ["", "normal"]);

    let all: Vec<String> = models
        .get_all_available(Some(""), Default::default())
        .await
        .unwrap()
        .into_iter()
        .map(|model| model.model().provider.clone())
        .collect();
    assert_eq!(all, ["", "normal"]);

    // `getModels`/`getAllModels` 在 TS 里用 `provider !== undefined`，空串是真实查找。
    assert!(models.get_models(Some("")).len() == 1);
    assert!(models.get_all_models(Some("")).len() == 1);
}

/// TS 的 `getAllModels?.() ?? getModels()`：显式实现且返回空数组的 provider 不贡献 chat 模型。
#[tokio::test]
async fn get_all_available_keeps_an_empty_explicit_all_model_catalog() {
    let models = create_models(CreateModelsOptions::default());
    let get_all: GetAllModelsFn = Arc::new(Vec::new);
    models.set_provider(Arc::new(
        TestProvider::new("empty-all").with_get_all_models(get_all),
    ));
    models.set_provider(Arc::new(TestProvider::new("normal")));

    let all: Vec<String> = models
        .get_all_available(None, Default::default())
        .await
        .unwrap()
        .into_iter()
        .map(|model| format!("{}/{}", model.model().provider, model.model().id))
        .collect();
    assert_eq!(all, ["normal/model-a"]);
}

/// `getAvailableOfType` 与 `getModelsOfType` 返回同一形状（非 chat 模型补齐内层 `type`）。
#[tokio::test]
async fn get_available_of_type_normalizes_the_model_type_like_get_models_of_type() {
    use kiana_ai::types::{AnyModel, ModelType};

    let image = Model {
        id: "im".into(),
        name: "IM".into(),
        api: "test-images".into(),
        provider: "p1".into(),
        base_url: "https://example.test".into(),
        model_type: None, // 防御性：内层 Model 未带 type，由 AnyModel 变体表达
        ..Model::default()
    };
    let get_all: GetAllModelsFn = {
        let image = image.clone();
        Arc::new(move || vec![AnyModel::Image(image.clone())])
    };
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(
        TestProvider::with_models("p1", Vec::new()).with_get_all_models(get_all),
    ));

    let available = models
        .get_available_of_type(ModelType::Image, None, Default::default())
        .await
        .unwrap();
    assert_eq!(available.len(), 1);
    assert_eq!(available[0].model_type, Some(ModelType::Image));
    assert_eq!(
        models.get_models_of_type(ModelType::Image, Some("p1"))[0].model_type,
        Some(ModelType::Image)
    );
}

/// `refresh()` 必须在返回前同步开始每个 provider 的刷新（代际提升 + controller 安装），
/// 这样紧随其后的 `setProvider` 能取代该刷新，过期的发布不会写盘。
#[tokio::test(flavor = "current_thread")]
async fn refresh_supersedes_a_provider_before_its_task_is_first_polled() {
    let store = Arc::new(InMemoryModelsStore::new());
    let models = create_models(CreateModelsOptions {
        models_store: Some(store.clone() as Arc<dyn ModelsStore>),
        ..Default::default()
    });
    let fetch: FetchModelsFn =
        Arc::new(|| Box::pin(async { Ok(vec![test_model("dynamic", "stale")]) }));
    models.set_provider(dynamic_provider(Some(fetch)));

    let refresh = models.refresh(ModelsRefreshOptions::default());
    tokio::pin!(refresh);
    // 第一次轮询：provider 任务已 spawn，但 current_thread 下不会在本次轮询中运行。
    assert!(futures::poll!(refresh.as_mut()).is_pending());
    // 取代进行中的刷新（旧实现里 begin 在任务内，会覆盖这次代际提升并成功发布）。
    models.set_provider(dynamic_provider(None));

    let result = refresh.await.unwrap();
    assert_eq!(result.errors.len(), 0);
    assert!(
        store
            .read("dynamic", Default::default())
            .await
            .unwrap()
            .is_none(),
        "superseded refresh must not publish a stale catalog"
    );
}

/// TS 的 `Promise.all`：所有 provider 的凭证读取与 auth 检查并发执行。
#[tokio::test]
async fn provider_auth_checks_run_concurrently() {
    struct BlockingCheck {
        started: Arc<tokio::sync::Semaphore>,
        gate: Arc<tokio::sync::Semaphore>,
    }
    #[async_trait]
    impl ApiKeyAuth for BlockingCheck {
        fn name(&self) -> &str {
            "Blocking"
        }
        fn has_check(&self) -> bool {
            true
        }
        async fn check(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthCheck>, AuthError> {
            self.started.add_permits(1);
            self.gate
                .acquire()
                .await
                .map_err(|_| AuthError::new("gate closed"))?
                .forget();
            Ok(Some(AuthCheck {
                source: Some("blocking".into()),
                r#type: AuthType::ApiKey,
            }))
        }
        async fn resolve(
            &self,
            _input: ApiKeyAuthInput<'_>,
        ) -> Result<Option<AuthResult>, AuthError> {
            Ok(None)
        }
    }

    let checked = Arc::new(AtomicBool::new(false));
    struct MarkCheck(Arc<AtomicBool>);
    #[async_trait]
    impl ApiKeyAuth for MarkCheck {
        fn name(&self) -> &str {
            "Mark"
        }
        fn has_check(&self) -> bool {
            true
        }
        async fn check(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthCheck>, AuthError> {
            self.0.store(true, Ordering::SeqCst);
            Ok(None)
        }
        async fn resolve(
            &self,
            _input: ApiKeyAuthInput<'_>,
        ) -> Result<Option<AuthResult>, AuthError> {
            Ok(None)
        }
    }

    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(Arc::new(TestProvider::new("blocking").with_auth(
        ProviderAuth {
            api_key: Some(Arc::new(BlockingCheck {
                started: Arc::clone(&started),
                gate: Arc::clone(&gate),
            })),
            oauth: None,
        },
    )));
    models.set_provider(Arc::new(TestProvider::new("mark").with_auth(
        ProviderAuth {
            api_key: Some(Arc::new(MarkCheck(Arc::clone(&checked)))),
            oauth: None,
        },
    )));

    let models_for_task = Arc::clone(&models);
    let task = tokio::spawn(async move {
        models_for_task
            .get_available(None, Default::default())
            .await
    });
    started.acquire().await.unwrap().forget();
    // 顺序执行时第二个 provider 的检查永远不会开始（2s 超时即失败）。
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !checked.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("second provider's auth check must run while the first is blocked");

    gate.add_permits(1);
    let available = task.await.unwrap().unwrap();
    assert_eq!(
        available
            .iter()
            .map(|model| model.provider.clone())
            .collect::<Vec<_>>(),
        ["blocking"]
    );
}

/// 回归：TS 用「方法是否存在」（`provider.refreshModels !== undefined` 等）判定可选操作。
/// Rust 无法探测 trait 方法的覆盖，因此可选操作的 `has_*` 谓词默认 `true`、`Models` 对
/// `getAllModels`/`filterModels`/`filterAllModels` 直接调用（默认实现复刻 TS 的 `??` 回退）。
/// 这些测试覆盖「只实现操作、不覆盖谓词」的 provider。
mod capability_dispatch {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use kiana_ai::auth::types::{Credential, ProviderAuth};
    use kiana_ai::error::AiError;
    use kiana_ai::event_stream::AssistantMessageEventStream;
    use kiana_ai::models::{
        ClassifierOptions, CreateModelsOptions, ImagesOptions, ModelsClassifierOptions,
        ModelsDeferredCancelOptions, ModelsDeferredFetchOptions, ModelsImagesOptions,
        ModelsRefreshOptions, Provider, RefreshModelsContext, create_models,
    };
    use kiana_ai::types::{
        AnyModel, AssistantImages, ClassifierContext, ClassifierResult, ClassifierStopReason,
        DeferredCancelOptions, DeferredFetchOptions, DeferredHandle, ImagesContext,
        ImagesStopReason, Model, ModelType, SimpleStreamOptions, StreamOptions, TranscriptContext,
    };

    use super::models_support::{ambient_auth, chat, response_stream, test_model};

    /// 覆盖了**所有**可选操作、但没有覆盖任何 `has_*` 谓词的 provider。
    struct BareProvider {
        id: String,
        auth: ProviderAuth,
        models: Vec<Model>,
        all_models: Vec<AnyModel>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl BareProvider {
        fn new() -> Self {
            let chat_model = test_model("bare", "chat-a");
            Self {
                id: "bare".into(),
                auth: ambient_auth(),
                models: vec![chat_model.clone()],
                all_models: vec![
                    chat(chat_model),
                    AnyModel::Image(Model {
                        id: "image-a".into(),
                        api: "bare-images".into(),
                        model_type: Some(ModelType::Image),
                        ..test_model("bare", "image-a")
                    }),
                ],
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn record(&self, call: &str) {
            self.calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(call.to_owned());
        }
    }

    #[async_trait]
    impl Provider for BareProvider {
        fn id(&self) -> &str {
            &self.id
        }
        fn name(&self) -> &str {
            &self.id
        }
        fn auth(&self) -> &ProviderAuth {
            &self.auth
        }
        fn get_models(&self) -> Vec<Model> {
            self.models.clone()
        }
        // 注意：不覆盖 has_get_all_models / has_refresh_models / has_filter_models /
        // has_filter_all_models / has_fetch_deferred / has_cancel_deferred /
        // has_generate_images / has_classify。
        fn get_all_models(&self) -> Vec<AnyModel> {
            self.all_models.clone()
        }
        async fn refresh_models(&self, context: RefreshModelsContext) -> Result<(), AiError> {
            self.record(&format!("refresh:{}", context.allow_network));
            Ok(())
        }
        fn filter_models(
            &self,
            models: Vec<Model>,
            _credential: Option<&Credential>,
        ) -> Vec<Model> {
            self.record("filter_models");
            models
                .into_iter()
                .filter(|model| model.id != "hidden")
                .collect()
        }
        fn filter_all_models(
            &self,
            models: Vec<AnyModel>,
            _credential: Option<&Credential>,
        ) -> Vec<AnyModel> {
            self.record("filter_all_models");
            models
                .into_iter()
                .filter(|model| model.model().id != "hidden-all")
                .collect()
        }
        fn stream(
            &self,
            model: &Model,
            _context: &TranscriptContext,
            _options: StreamOptions,
        ) -> AssistantMessageEventStream {
            response_stream(model, "ok")
        }
        fn stream_simple(
            &self,
            model: &Model,
            _context: &TranscriptContext,
            _options: SimpleStreamOptions,
        ) -> AssistantMessageEventStream {
            response_stream(model, "ok")
        }
        fn fetch_deferred(
            &self,
            model: &Model,
            _handle: &DeferredHandle,
            _options: DeferredFetchOptions,
        ) -> Option<AssistantMessageEventStream> {
            self.record("fetch_deferred");
            Some(response_stream(model, "deferred"))
        }
        async fn cancel_deferred(
            &self,
            _model: &Model,
            _handle: &DeferredHandle,
            _options: DeferredCancelOptions,
        ) -> Result<(), AiError> {
            self.record("cancel_deferred");
            Ok(())
        }
        async fn generate_images(
            &self,
            model: &Model,
            _context: &ImagesContext,
            _options: Option<&ImagesOptions>,
        ) -> AssistantImages {
            self.record("generate_images");
            AssistantImages {
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                stop_reason: ImagesStopReason::Stop,
                ..Default::default()
            }
        }
        async fn classify(
            &self,
            model: &Model,
            _context: &ClassifierContext,
            _options: Option<&ClassifierOptions>,
        ) -> ClassifierResult {
            self.record("classify");
            ClassifierResult {
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                stop_reason: ClassifierStopReason::Stop,
                ..Default::default()
            }
        }
    }

    /// 只覆盖 `filter_models` 的 provider：验证 `getAllAvailable` 的 TS 回退分支
    /// （`filterAllModels` 缺失时把 `filterModels` 应用到 `getModels()` 的结果）。
    struct FilterOnlyProvider {
        id: String,
        auth: ProviderAuth,
    }

    #[async_trait]
    impl Provider for FilterOnlyProvider {
        fn id(&self) -> &str {
            &self.id
        }
        fn name(&self) -> &str {
            &self.id
        }
        fn auth(&self) -> &ProviderAuth {
            &self.auth
        }
        fn get_models(&self) -> Vec<Model> {
            vec![
                test_model(&self.id, "visible"),
                test_model(&self.id, "hidden"),
            ]
        }
        fn filter_models(
            &self,
            models: Vec<Model>,
            _credential: Option<&Credential>,
        ) -> Vec<Model> {
            models
                .into_iter()
                .filter(|model| model.id != "hidden")
                .collect()
        }
        fn stream(
            &self,
            model: &Model,
            _context: &TranscriptContext,
            _options: StreamOptions,
        ) -> AssistantMessageEventStream {
            response_stream(model, "ok")
        }
        fn stream_simple(
            &self,
            model: &Model,
            _context: &TranscriptContext,
            _options: SimpleStreamOptions,
        ) -> AssistantMessageEventStream {
            response_stream(model, "ok")
        }
    }

    #[tokio::test]
    async fn operations_dispatch_even_when_capability_predicates_are_not_overridden() {
        let provider = Arc::new(BareProvider::new());
        let calls = Arc::clone(&provider.calls);
        let models = create_models(CreateModelsOptions::default());
        models.set_provider(provider.clone());

        // getAllModels：覆盖即生效（旧实现会因 has_get_all_models == false 退回 chat 列表）。
        let all = models.get_all_models(Some("bare"));
        assert_eq!(
            all.iter()
                .map(|model| model.model().id.as_str())
                .collect::<Vec<_>>(),
            ["chat-a", "image-a"]
        );

        // getAllAvailable 使用 getAllModels + filterAllModels。
        let available = models
            .get_all_available(None, Default::default())
            .await
            .unwrap();
        assert_eq!(
            available
                .iter()
                .map(|model| model.model().id.as_str())
                .collect::<Vec<_>>(),
            ["chat-a", "image-a"]
        );

        // getAvailable 使用 filterModels（即使谓词没有覆盖）。
        let chat_available = models
            .get_available(Some("bare"), Default::default())
            .await
            .unwrap();
        assert_eq!(chat_available.len(), 1);

        // refresh 调用覆盖的 refreshModels。
        let result = models
            .refresh(ModelsRefreshOptions::default())
            .await
            .unwrap();
        assert!(result.errors.is_empty());
        assert!(
            calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .any(|call| call == "refresh:true"),
            "refreshModels must be called"
        );

        // fetchDeferred / cancelDeferred 分派到覆盖的实现。
        let chat_model = test_model("bare", "chat-a");
        let handle = DeferredHandle::default();
        let deferred = models
            .fetch_deferred(&chat_model, &handle, ModelsDeferredFetchOptions::default())
            .await;
        assert_eq!(deferred.stop_reason, kiana_ai::types::StopReason::Stop);
        models
            .cancel_deferred(&chat_model, &handle, ModelsDeferredCancelOptions::default())
            .await
            .unwrap();

        // generateImages / classify 分派到覆盖的实现。
        let image_model = Model {
            id: "image-a".into(),
            api: "bare-images".into(),
            model_type: Some(ModelType::Image),
            ..test_model("bare", "image-a")
        };
        let images = models
            .generate_images(
                &image_model,
                &ImagesContext::default(),
                ModelsImagesOptions::default(),
            )
            .await;
        assert_eq!(images.stop_reason, ImagesStopReason::Stop);
        let classifier_model = Model {
            id: "classifier-a".into(),
            api: "bare-classifiers".into(),
            model_type: Some(ModelType::Classifier),
            ..test_model("bare", "classifier-a")
        };
        let classified = models
            .classify(
                &classifier_model,
                &ClassifierContext::default(),
                ModelsClassifierOptions::default(),
            )
            .await;
        assert_eq!(classified.stop_reason, ClassifierStopReason::Stop);

        let recorded = calls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        for expected in [
            "fetch_deferred",
            "cancel_deferred",
            "generate_images",
            "classify",
        ] {
            assert!(
                recorded.iter().any(|call| call == expected),
                "missing {expected} in {recorded:?}"
            );
        }
    }

    #[tokio::test]
    async fn get_all_available_applies_filter_models_fallback_without_a_predicate() {
        let models = create_models(CreateModelsOptions::default());
        models.set_provider(Arc::new(FilterOnlyProvider {
            id: "filter-only".into(),
            auth: ambient_auth(),
        }));

        let all = models
            .get_all_available(None, Default::default())
            .await
            .unwrap();
        assert_eq!(
            all.iter()
                .map(|model| model.model().id.as_str())
                .collect::<Vec<_>>(),
            ["visible"]
        );
        let chat_available = models
            .get_available(None, Default::default())
            .await
            .unwrap();
        assert_eq!(
            chat_available
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["visible"]
        );
    }
}

/// TS 的 `publish()` 是立即执行的 async IIFE：provider 即使丢弃返回的 promise，发布也会
/// 完成（持久化 + 更新）。Rust 的 future 惰性，因此 `PublishFn` 在调用时就把发布任务
/// spawn 出去，返回的 future 只等待结果。
#[tokio::test]
async fn a_fire_and_forget_publication_still_persists_and_updates() {
    let store = Arc::new(InMemoryModelsStore::new());
    let state = Arc::new(Mutex::new("initial".to_owned()));
    let models = create_models(CreateModelsOptions {
        models_store: Some(Arc::clone(&store) as Arc<dyn ModelsStore>),
        ..Default::default()
    });
    let state_for_provider = Arc::clone(&state);
    models.set_provider(Arc::new(TestProvider::new("dynamic").with_refresh(
        Arc::new(move |context| {
            let state = Arc::clone(&state_for_provider);
            Box::pin(async move {
                if !context.allow_network {
                    return Ok(());
                }
                let publication = ModelsPublication {
                    persist: ModelsPublicationPersist::Entry(ModelsStoreEntry {
                        models: vec![chat(test_model("dynamic", "fresh"))],
                        ..Default::default()
                    }),
                    update: Some(Box::new(move || {
                        *state.lock().unwrap_or_else(|error| error.into_inner()) =
                            "updated".to_owned();
                    })),
                };
                // 故意不 await：TS 里 publish() 已经开始执行。
                drop((context.publish)(publication));
                Ok(())
            })
        }),
    )));

    let result = models
        .refresh(ModelsRefreshOptions::default())
        .await
        .unwrap();
    assert!(result.errors.is_empty());
    let state_for_wait = Arc::clone(&state);
    wait_for(move || {
        *state_for_wait
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            == "updated"
    })
    .await;
    let stored = store
        .read("dynamic", ModelsStoreOperationOptions::default())
        .await
        .unwrap();
    assert_eq!(
        stored.unwrap().models[0].model().id,
        "fresh",
        "publication must persist even when its future is dropped"
    );
}

/// 回归：`createProvider` 的能力判定。TS 用 `streams.some(e => e.fetchDeferred !== undefined)`
/// 决定 provider 级是否有 `fetchDeferred`；Rust 的实现即使不覆盖 `has_*` 谓词也必须在调用时
/// 被分派，同时保留 TS 的两条错误文案（通用错误 / `for "api"`）。
mod provider_stream_capabilities {
    use std::sync::Arc;

    use async_trait::async_trait;

    use kiana_ai::error::AiError;
    use kiana_ai::event_stream::AssistantMessageEventStream;
    use kiana_ai::models::{
        CreateProviderOptions, ModelsDeferredFetchOptions, ProviderApiImpl, ProviderStreams,
        create_models, create_provider,
    };
    use kiana_ai::types::{
        DeferredCancelOptions, DeferredFetchOptions, DeferredHandle, Model, SimpleStreamOptions,
        StreamOptions, TranscriptContext,
    };

    use super::models_support::{SilentStreams, ambient_auth, response_stream, test_model};

    /// 覆盖了 deferred 操作；`declare` 决定是否覆盖 `has_*` 谓词。
    struct DeferredStreams {
        declare: bool,
    }

    #[async_trait]
    impl ProviderStreams for DeferredStreams {
        fn stream(
            &self,
            model: &Model,
            _context: &TranscriptContext,
            _options: StreamOptions,
        ) -> AssistantMessageEventStream {
            response_stream(model, "ok")
        }
        fn stream_simple(
            &self,
            model: &Model,
            _context: &TranscriptContext,
            _options: SimpleStreamOptions,
        ) -> AssistantMessageEventStream {
            response_stream(model, "ok")
        }
        fn has_fetch_deferred(&self) -> bool {
            self.declare
        }
        fn fetch_deferred(
            &self,
            model: &Model,
            _handle: &DeferredHandle,
            _options: DeferredFetchOptions,
        ) -> Option<AssistantMessageEventStream> {
            Some(response_stream(model, "deferred"))
        }
        fn has_cancel_deferred(&self) -> bool {
            self.declare
        }
        async fn cancel_deferred(
            &self,
            _model: &Model,
            _handle: &DeferredHandle,
            _options: DeferredCancelOptions,
        ) -> Option<Result<(), AiError>> {
            Some(Ok(()))
        }
    }

    fn provider(api: ProviderApiImpl) -> Arc<dyn kiana_ai::models::Provider> {
        create_provider(CreateProviderOptions {
            id: "p".into(),
            auth: ambient_auth(),
            models: vec![kiana_ai::types::AnyModel::Chat(test_model("p", "model-a"))],
            api: Some(api),
            ..Default::default()
        })
        .expect("provider")
    }

    #[tokio::test]
    async fn deferred_overrides_dispatch_without_the_capability_predicate() {
        let models = create_models(Default::default());
        models.set_provider(provider(ProviderApiImpl::Single(Arc::new(
            DeferredStreams { declare: false },
        ))));
        let result = models
            .fetch_deferred(
                &test_model("p", "model-a"),
                &DeferredHandle::default(),
                ModelsDeferredFetchOptions::default(),
            )
            .await;
        assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
        models
            .cancel_deferred(
                &test_model("p", "model-a"),
                &DeferredHandle::default(),
                Default::default(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn unsupported_deferred_keeps_the_ts_error_messages() {
        // provider 级完全没有 deferred 支持：通用错误（TS 在鉴权前判定）。
        let models = create_models(Default::default());
        models.set_provider(provider(ProviderApiImpl::Single(Arc::new(SilentStreams))));
        let result = models
            .fetch_deferred(
                &test_model("p", "model-a"),
                &DeferredHandle::default(),
                ModelsDeferredFetchOptions::default(),
            )
            .await;
        assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
        assert_eq!(
            result.error_message.as_deref(),
            Some("Provider p does not support deferred responses")
        );

        // provider 级有支持、某个 api 没有：`for "api"` 文案。
        let mut by_api = indexmap::IndexMap::new();
        by_api.insert(
            "test-api".to_string(),
            Arc::new(DeferredStreams { declare: true }) as Arc<dyn ProviderStreams>,
        );
        by_api.insert(
            "other-api".to_string(),
            Arc::new(SilentStreams) as Arc<dyn ProviderStreams>,
        );
        let models = create_models(Default::default());
        models.set_provider(provider(ProviderApiImpl::ByApi(by_api)));
        let other = Model {
            api: "other-api".into(),
            ..test_model("p", "model-a")
        };
        let result = models
            .fetch_deferred(
                &other,
                &DeferredHandle::default(),
                ModelsDeferredFetchOptions::default(),
            )
            .await;
        assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
        assert_eq!(
            result.error_message.as_deref(),
            Some("Provider p does not support deferred responses for \"other-api\"")
        );
    }
}

/// `ApiKeyAuth::login` 在返回凭证的同一轮询里同步中止了 signal（TS 的 `controller.abort()`
/// 发生在 async 函数的第一段同步代码里）。TS 的 `raceWithAbortSignal` 先检查
/// `signal.aborted`，因此以中止拒绝而不是返回凭证；Rust 在 select 之后复查 signal。
struct AbortingLogin;

#[async_trait]
impl ApiKeyAuth for AbortingLogin {
    fn name(&self) -> &str {
        "Aborting"
    }
    fn has_login(&self) -> bool {
        true
    }
    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(None)
    }
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, AuthError> {
        interaction.signal().abort();
        Ok(ApiKeyCredential {
            key: Some("must-not-be-saved".into()),
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn login_rejects_when_the_provider_aborts_synchronously_before_returning() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::clone(&credentials) as Arc<dyn CredentialStore>),
        ..Default::default()
    });
    models.set_provider(Arc::new(TestProvider::new("p1").with_auth(ProviderAuth {
        api_key: Some(Arc::new(AbortingLogin)),
        oauth: None,
    })));
    let interaction = TestInteraction {
        signal: AbortScope::new(),
    };

    let error = models
        .login(
            "p1",
            AuthType::ApiKey,
            &interaction,
            LoginOptions::default(),
        )
        .await
        .expect_err("an abort that races the credential must reject");
    assert!(error.is_aborted(), "expected an AbortError, got {error:?}");
    assert_eq!(error.message, "This operation was aborted");
    // 凭证没有被写入。
    assert!(
        credentials
            .read("p1", Default::default())
            .await
            .unwrap()
            .is_none()
    );
}
