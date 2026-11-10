//! `7027-credential-refresh-hang.test.ts` 的 ModelRuntime 级用例（仅第 1 个）。

#[path = "fixtures/model_runtime/mod.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use kiana_ai::auth::types::{ApiKeyCredential, AuthCheck, AuthType, CredentialStore, LoginOptions};
use kiana_ai::models::ModelsRefreshOptions;
use kiana_ai::types::Model;
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use serde_json::json;
use support::*;
use tokio::sync::Semaphore;

fn dynamic_model() -> Model {
    Model {
        provider: "stalled-login".to_string(),
        ..chat_model("stalled-login", "dynamic")
    }
}

#[tokio::test]
async fn does_not_hold_login_behind_an_older_stalled_network_catalog_refresh() {
    let started = Arc::new(Semaphore::new(0));
    let started_refresh = Arc::clone(&started);
    let auth = TestApiKeyAuth::new("API key")
        .with_login(Arc::new(|| {
            Box::pin(async {
                Ok(ApiKeyCredential {
                    key: Some("secret".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }))
        .with_check(Arc::new(|credential| {
            Box::pin(async move {
                Ok(credential.and_then(|credential| {
                    credential.key.map(|_| AuthCheck {
                        source: Some("stored key".to_string()),
                        r#type: AuthType::ApiKey,
                    })
                }))
            })
        }))
        .with_resolve(Arc::new(|credential| {
            Box::pin(async move {
                Ok(Some(kiana_ai::auth::types::AuthResult::new(
                    kiana_ai::auth::types::ModelAuth {
                        api_key: Some(
                            credential
                                .and_then(|credential| credential.key)
                                .unwrap_or_else(|| "ambient-key".to_string()),
                        ),
                        ..Default::default()
                    },
                )))
            })
        }));
    let provider = TestProvider::dynamic("stalled-login", Arc::new(auth).provider_auth())
        .with_name("Stalled Login")
        .with_refresh_models(Arc::new(move |context| {
            let started = Arc::clone(&started_refresh);
            Box::pin(async move {
                if !context.allow_network {
                    return Ok(());
                }
                started.add_permits(1);
                std::future::pending::<()>().await;
                Ok(())
            })
        }))
        .into_provider();
    let provider_id = provider.id().to_string();

    let credentials = Arc::new(AuthStorage::in_memory(auth_data_from_value(json!({}))));
    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::clone(&credentials) as Arc<dyn CredentialStore>),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            ..Default::default()
        })
        .await
        .expect("runtime"),
    );
    runtime
        .register_native_provider(provider)
        .expect("register provider");
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec![provider_id.clone()]),
            ..Default::default()
        })
        .await
        .expect("refresh");

    let refresh_runtime = Arc::clone(&runtime);
    let refresh_provider = provider_id.clone();
    let stalled_refresh = tokio::spawn(async move {
        refresh_runtime
            .refresh(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec![refresh_provider]),
                ..Default::default()
            })
            .await
    });
    let permit = tokio::time::timeout(Duration::from_secs(5), started.acquire())
        .await
        .expect("network refresh started")
        .expect("semaphore");
    permit.forget();

    let interaction = TestInteraction::new();
    let credential = runtime
        .login(
            &provider_id,
            AuthType::ApiKey,
            &interaction,
            LoginOptions::default(),
        )
        .await
        .expect("login");
    match &credential {
        kiana_ai::auth::types::Credential::ApiKey(api_key) => {
            assert_eq!(api_key.key.as_deref(), Some("secret"))
        }
        other => panic!("unexpected credential: {other:?}"),
    }

    assert!(
        runtime
            .get_available_snapshot()
            .iter()
            .any(|model| model.id == dynamic_model().id)
    );
    let stored = credentials
        .read(&provider_id, Default::default())
        .await
        .expect("read")
        .expect("stored");
    match stored {
        kiana_ai::auth::types::Credential::ApiKey(api_key) => {
            assert_eq!(api_key.key.as_deref(), Some("secret"))
        }
        other => panic!("unexpected credential: {other:?}"),
    }

    let result = tokio::time::timeout(Duration::from_secs(5), stalled_refresh)
        .await
        .expect("stalled refresh resolves")
        .expect("join")
        .expect("refresh result");
    assert!(!result.aborted);
}
