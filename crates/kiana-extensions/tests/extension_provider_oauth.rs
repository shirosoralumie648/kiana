//! Regression for ProviderConfig.oauth discarded by registerProvider(name, config).
//! TS provenance: coding-agent/src/core/provider-composer.ts#adaptOAuth.
//! All providers and credentials are local, in-memory test doubles.
#![cfg(feature = "runtime")]

#[path = "fixtures/extensions/support.rs"]
mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_ai::auth::types::{
    AuthError, AuthEvent, AuthInteraction, AuthOperationOptions, AuthPromptRequest, AuthType,
    Credential, LoginOptions,
};
use kiana_ai::models::ModelsRefreshOptions;
use kiana_ai::utils::abort::AbortScope;
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelsPathOption};
use kiana_extensions::loader::load_extensions;
use support::{LoaderHarness, temp_root, write_file};

#[derive(Default)]
struct Interaction {
    signal: AbortScope,
    prompts: Mutex<Vec<String>>,
}

#[async_trait]
impl AuthInteraction for Interaction {
    fn signal(&self) -> &AbortScope {
        &self.signal
    }

    async fn prompt(&self, request: AuthPromptRequest) -> Result<String, AuthError> {
        self.prompts
            .lock()
            .unwrap()
            .push(request.prompt.message().into());
        Ok("selected".into())
    }

    fn notify(&self, _event: &AuthEvent) {}
}

#[test]
fn js_register_provider_oauth_login_refresh_get_api_key_and_modify_models() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async {
        let _harness = LoaderHarness::new();
        let root = temp_root();
        let path = write_file(
            root.path(),
            "extensions/oauth.ts",
            r#"
export default function(kiana) {
  kiana.registerProvider("oauth-test", {
    name: "OAuth test",
    baseUrl: "https://mock.invalid/v1",
    api: "openai-completions",
    models: [{
      id: "base", name: "Base", api: "openai-completions",
      baseUrl: "https://mock.invalid/v1", input: ["text"], reasoning: false,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 1000, maxTokens: 100,
    }],
    oauth: {
      name: "Mock OAuth",
      async login(callbacks) {
        callbacks.onProgress("login started");
        const selected = await callbacks.onSelect({
          message: "Choose a mock account",
          options: [{ id: "one", label: "One" }],
        });
        if (selected !== "selected") throw new Error("onSelect was not adapted");
        if (!callbacks.signal) throw new Error("signal was not adapted");
        return { access: "initial-access", refresh: "refresh-token", expires: 1 };
      },
      async refreshToken(credentials, signal) {
        if (credentials.refresh !== "refresh-token") throw new Error("wrong refresh input");
        if (!signal) throw new Error("refresh signal missing");
        return {
          ...credentials, access: "refreshed-access",
          expires: Date.now() + 3600000, credentialsModified: true,
        };
      },
      getApiKey(credentials) { return `key:${credentials.access}`; },
      modifyModels(models, credentials) {
        return models.map(model => ({ ...model, id: `oauth-${credentials.access}` }));
      },
    },
  });
}
"#,
        );
        let runtime = load_extensions(
            &[path.to_string_lossy().into_owned()],
            &root.path().to_string_lossy(),
            None,
            None,
        )
        .await;
        assert!(runtime.errors.is_empty(), "{:?}", runtime.errors);
        let registrations = runtime.runtime.pending_provider_registrations.clone();
        assert_eq!(
            registrations.len(),
            1,
            "config errors: {:?}, runtime flags: {:?}",
            kiana_extensions::loader::runtime_provider_config_errors(&runtime.runtime),
            runtime.runtime.flag_values
        );
        assert!(registrations[0].config.oauth.is_some());

        let auth = Arc::new(AuthStorage::in_memory(auth_data_from_value(
            serde_json::json!({}),
        )));
        let models =
            kiana_config::models::runtime::ModelRuntime::create(CreateModelRuntimeOptions {
                credentials: Some(auth.clone()),
                models_path: ModelsPathOption::Disabled,
                allow_model_network: false,
                ..Default::default()
            })
            .await
            .unwrap();
        models
            .register_provider(
                "oauth-test",
                kiana_extensions::runner::provider_config_input(&registrations[0].config),
            )
            .unwrap();
        assert_eq!(
            models
                .get_provider("oauth-test")
                .unwrap()
                .auth()
                .oauth
                .as_ref()
                .unwrap()
                .name(),
            "Mock OAuth"
        );

        let credential = models
            .login(
                "oauth-test",
                AuthType::OAuth,
                &Interaction::default(),
                LoginOptions::default(),
            )
            .await
            .unwrap();
        assert!(matches!(credential, Credential::OAuth(_)));
        let stored = auth
            .read_credential("oauth-test", &AuthOperationOptions::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.as_oauth().unwrap().access, "initial-access");

        // Login's expires=1 makes the first request resolve refreshToken, then getApiKey.
        let resolved = models
            .get_auth_for_provider("oauth-test", Default::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved.auth.api_key.as_deref(),
            Some("key:refreshed-access")
        );

        // Exercise the actual detached credential-storage path as well: this future
        // is Send and executes outside the host task's LocalSet context.
        let spawned_models = models.clone();
        let spawned = tokio::spawn(async move {
            spawned_models
                .get_auth_for_provider("oauth-test", Default::default())
                .await
                .unwrap()
                .unwrap()
                .auth
                .api_key
        })
        .await
        .unwrap();
        assert_eq!(spawned.as_deref(), Some("key:refreshed-access"));
        assert_eq!(
            resolved.auth.api_key.as_deref(),
            Some("key:refreshed-access")
        );
        models
            .refresh(ModelsRefreshOptions {
                allow_network: Some(false),
                providers: Some(vec!["oauth-test".into()]),
                ..Default::default()
            })
            .await
            .unwrap();
        let available = models
            .get_all_available(Some("oauth-test"), Default::default())
            .await
            .unwrap();
        assert_eq!(available[0].model().id, "oauth-refreshed-access");
    });
}
