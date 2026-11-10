//! `core/runtime-credentials.ts` 的测试移植。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_ai::auth::types::{
    ApiKeyCredential, AuthError, AuthOperationOptions, AuthType, Credential, CredentialInfo,
    CredentialModifyFn, CredentialStore,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_config::auth::runtime_credentials::RuntimeCredentials;
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn api_key(key: &str) -> Credential {
    Credential::ApiKey(ApiKeyCredential {
        key: Some(key.to_string()),
        env: None,
        extra: indexmap::IndexMap::new(),
    })
}

fn options(signal: Option<AbortScope>) -> AuthOperationOptions {
    AuthOperationOptions { signal }
}

fn modify_fn<F, Fut>(f: F) -> CredentialModifyFn
where
    F: FnOnce(Option<Credential>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<Option<Credential>, AuthError>> + Send + 'static,
{
    Box::new(move |current| Box::pin(f(current)))
}

/// runtime overrides mask stored credentials without persisting
#[tokio::test]
async fn runtime_overrides_mask_stored_credentials_without_persisting() {
    let storage = Arc::new(AuthStorage::in_memory(auth_data_from_value(
        json!({"anthropic": {"type": "api_key", "key": "stored-key"}}),
    )));
    let credentials = RuntimeCredentials::new(storage.clone());

    credentials.set_runtime_api_key("anthropic", "runtime-key");
    assert_eq!(
        credentials.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("runtime-key"))
    );
    assert_eq!(
        storage.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("stored-key"))
    );

    credentials.remove_runtime_api_key("anthropic");
    assert_eq!(
        credentials.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("stored-key"))
    );
}

/// enumeration merges overrides without exposing keys
#[tokio::test]
async fn enumeration_merges_overrides_without_exposing_keys() {
    let storage = Arc::new(AuthStorage::in_memory(auth_data_from_value(json!({
        "anthropic": {"type": "oauth", "access": "access", "refresh": "refresh", "expires": 1767225600000i64}
    }))));
    let credentials = RuntimeCredentials::new(storage.clone());
    credentials.set_runtime_api_key("anthropic", "runtime-key");
    credentials.set_runtime_api_key("openai", "other-runtime-key");

    assert_eq!(
        credentials.list(options(None)).await.unwrap(),
        vec![
            CredentialInfo {
                provider_id: "anthropic".into(),
                r#type: AuthType::ApiKey,
            },
            CredentialInfo {
                provider_id: "openai".into(),
                r#type: AuthType::ApiKey,
            },
        ]
    );
    // OAuth 凭证本身未被覆盖（仅 list 报告 api_key）。
    assert!(matches!(
        storage.read("anthropic", options(None)).await.unwrap(),
        Some(Credential::OAuth(_))
    ));
}

/// forwards operation signals to the persistent store
#[tokio::test]
async fn forwards_operation_signals_to_the_persistent_store() {
    #[derive(Default)]
    struct RecordingStore {
        received: Mutex<Vec<Option<CancellationToken>>>,
    }

    fn record(slot: &Mutex<Vec<Option<CancellationToken>>>, options: &AuthOperationOptions) {
        slot.lock()
            .unwrap()
            .push(options.signal.as_ref().map(AbortScope::token));
    }

    #[async_trait]
    impl CredentialStore for RecordingStore {
        async fn read(
            &self,
            _provider_id: &str,
            options: AuthOperationOptions,
        ) -> Result<Option<Credential>, AuthError> {
            record(&self.received, &options);
            Ok(None)
        }

        async fn list(
            &self,
            options: AuthOperationOptions,
        ) -> Result<Vec<CredentialInfo>, AuthError> {
            record(&self.received, &options);
            Ok(Vec::new())
        }

        async fn modify(
            &self,
            _provider_id: &str,
            _f: CredentialModifyFn,
            options: AuthOperationOptions,
        ) -> Result<Option<Credential>, AuthError> {
            record(&self.received, &options);
            Ok(None)
        }

        async fn delete(
            &self,
            _provider_id: &str,
            options: AuthOperationOptions,
        ) -> Result<(), AuthError> {
            record(&self.received, &options);
            Ok(())
        }
    }

    let store = Arc::new(RecordingStore::default());
    let credentials = RuntimeCredentials::new(store.clone());
    let controller = AbortScope::new();

    credentials
        .read("anthropic", options(Some(controller.clone())))
        .await
        .unwrap();
    credentials
        .list(options(Some(controller.clone())))
        .await
        .unwrap();
    credentials
        .modify(
            "anthropic",
            modify_fn(|_| async { Ok(None) }),
            options(Some(controller.clone())),
        )
        .await
        .unwrap();
    credentials
        .delete("anthropic", options(Some(controller.clone())))
        .await
        .unwrap();

    let received = store.received.lock().unwrap().clone();
    assert_eq!(received.len(), 4);
    assert!(received.iter().all(Option::is_some));
    assert!(!controller.is_aborted());
    controller.abort();
    for token in received.into_iter().flatten() {
        assert!(token.is_cancelled());
    }
}

/// keeps a runtime override when persistent deletion is cancelled
#[tokio::test]
async fn keeps_a_runtime_override_when_persistent_deletion_is_cancelled() {
    struct FailingDeleteStore {
        inner: AuthStorage,
        calls: Mutex<usize>,
    }

    #[async_trait]
    impl CredentialStore for FailingDeleteStore {
        async fn read(
            &self,
            provider_id: &str,
            options: AuthOperationOptions,
        ) -> Result<Option<Credential>, AuthError> {
            self.inner.read(provider_id, options).await
        }

        async fn list(
            &self,
            options: AuthOperationOptions,
        ) -> Result<Vec<CredentialInfo>, AuthError> {
            self.inner.list(options).await
        }

        async fn modify(
            &self,
            provider_id: &str,
            f: CredentialModifyFn,
            options: AuthOperationOptions,
        ) -> Result<Option<Credential>, AuthError> {
            self.inner.modify(provider_id, f, options).await
        }

        async fn delete(
            &self,
            _provider_id: &str,
            _options: AuthOperationOptions,
        ) -> Result<(), AuthError> {
            *self.calls.lock().unwrap() += 1;
            let mut error = AuthError::new("cancelled");
            error.name = "AbortError".into();
            Err(error)
        }
    }

    let store = Arc::new(FailingDeleteStore {
        inner: AuthStorage::in_memory(auth_data_from_value(
            json!({"anthropic": {"type": "api_key", "key": "stored-key"}}),
        )),
        calls: Mutex::new(0),
    });
    let credentials = RuntimeCredentials::new(store.clone());
    credentials.set_runtime_api_key("anthropic", "runtime-key");

    let error = credentials
        .delete("anthropic", options(Some(AbortScope::new())))
        .await
        .unwrap_err();
    assert_eq!(error.name, "AbortError");
    assert_eq!(*store.calls.lock().unwrap(), 1);
    assert_eq!(
        credentials.read("anthropic", options(None)).await.unwrap(),
        Some(api_key("runtime-key"))
    );
}

/// delete clears both the override and persisted credential
#[tokio::test]
async fn delete_clears_both_the_override_and_persisted_credential() {
    let storage = Arc::new(AuthStorage::in_memory(auth_data_from_value(
        json!({"anthropic": {"type": "api_key", "key": "stored-key"}}),
    )));
    let credentials = RuntimeCredentials::new(storage);
    credentials.set_runtime_api_key("anthropic", "runtime-key");

    credentials
        .delete("anthropic", options(None))
        .await
        .unwrap();

    assert_eq!(
        credentials.read("anthropic", options(None)).await.unwrap(),
        None
    );
    assert_eq!(credentials.list(options(None)).await.unwrap(), Vec::new());
}
