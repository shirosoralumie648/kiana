//! `test/auth-check.test.ts` 的逐条移植。
//!
//! TS 用 `vi.fn` 替换 provider 的 `oauth.refresh`；Rust 侧用 `register_native_provider`
//! 注册一个 OAuth 刷新返回固定 token 的测试 provider，效果相同（并额外验证刷新结果已持久化）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{
    AuthError, AuthInteraction, AuthOperationOptions, AuthType, CredentialStore, LoginOptions,
    ModelAuth, OAuthAuth, OAuthCredential, ProviderAuth,
};
use kiana_ai::models::store::InMemoryModelsStore;
use kiana_ai::models::{CreateProviderOptions, ProviderApiImpl, create_provider};
use kiana_ai::providers::faux::{RegisterFauxProviderOptions, create_faux_core};
use kiana_ai::types::ProviderHeaders;
use kiana_ai::utils::abort::AbortScope;
use kiana_coding_agent::cli::args::parse_args;
use kiana_coding_agent::cli::auth_check::{
    AuthCheckOptions, AuthCheckReason, AuthCheckStatus, check_provider_auth,
    create_auth_check_model_runtime, get_provider_credential,
};
use kiana_coding_agent::cli::auth_command::parse_auth_command;
use kiana_config::auth::storage::{AuthStorage, ReadOnlyAuthStorage};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use serde_json::{Value, json};
use tempfile::TempDir;

/// TS 测试里 `runtime.getProvider("openai-codex").auth.oauth.refresh = vi.fn(...)` 的等价物：
/// 一个可由测试注册的原生 provider，其 OAuth `refresh` 返回固定 token 并持久化。
const MOCK_OAUTH_PROVIDER: &str = "mock-oauth";
const FAR_FUTURE_MS: f64 = 4_102_444_800_000.0;

struct MockOAuth;

#[async_trait]
impl OAuthAuth for MockOAuth {
    fn name(&self) -> &str {
        "Mock OAuth"
    }

    async fn login(
        &self,
        _interaction: &dyn AuthInteraction,
        _options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        Err(AuthError::new("login is not used by this test"))
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        _signal: &AbortScope,
    ) -> Result<OAuthCredential, AuthError> {
        Ok(OAuthCredential::new(
            "fresh-token",
            credential.refresh.clone(),
            FAR_FUTURE_MS,
        ))
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        let mut headers: ProviderHeaders = IndexMap::new();
        headers.insert(
            "Authorization".to_string(),
            Some(format!("Bearer {}", credential.access)),
        );
        Ok(ModelAuth {
            api_key: None,
            headers: Some(headers),
            base_url: None,
        })
    }
}

fn register_mock_oauth_provider(runtime: &ModelRuntime) {
    let core = create_faux_core(RegisterFauxProviderOptions {
        provider: Some(MOCK_OAUTH_PROVIDER.to_string()),
        ..Default::default()
    });
    let provider = create_provider(CreateProviderOptions {
        id: MOCK_OAUTH_PROVIDER.to_string(),
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(Arc::new(MockOAuth)),
        },
        api: Some(ProviderApiImpl::Single(Arc::new(core))),
        ..Default::default()
    })
    .expect("mock provider");
    runtime
        .register_native_provider(provider)
        .expect("register mock provider");
}

/// 进程环境变量（`KIANA_CODING_AGENT_DIR`）在测试内是全局状态。
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn parse(args: &[&str]) -> kiana_coding_agent::cli::args::Args {
    parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
}

fn auth_storage(entries: &[(&str, Value)]) -> AuthStorage {
    let mut data: IndexMap<String, Value> = IndexMap::new();
    for (provider, credential) in entries {
        data.insert((*provider).to_string(), credential.clone());
    }
    AuthStorage::in_memory(data)
}

async fn create_runtime(credentials: Arc<dyn CredentialStore>) -> ModelRuntime {
    ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credentials),
        models_path: ModelsPathOption::Disabled,
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        allow_model_network: false,
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .expect("model runtime")
}

fn oauth_credential(access: &str, expires: f64) -> Value {
    json!({
        "type": "oauth",
        "access": access,
        "refresh": "refresh-token",
        "expires": expires,
    })
}

#[tokio::test]
async fn reports_a_configured_provider_as_ready() {
    let storage = auth_storage(&[("openai", json!({"type": "api_key", "key": "test-key"}))]);
    let runtime = create_runtime(Arc::new(storage)).await;

    let result = check_provider_auth(
        &parse(&["--provider", "openai"]),
        &runtime,
        AuthCheckOptions::default(),
    )
    .await
    .expect("check");
    assert_eq!(result.status, AuthCheckStatus::Ready);
    assert_eq!(result.provider, "openai");
    assert_eq!(result.reason, None);
    assert_eq!(result.auth_type, Some(AuthType::ApiKey));
}

#[tokio::test]
async fn resolves_the_provider_from_model() {
    let storage = auth_storage(&[("openai", json!({"type": "api_key", "key": "test-key"}))]);
    let runtime = create_runtime(Arc::new(storage)).await;

    let by_model = check_provider_auth(
        &parse(&["--model", "openai/gpt-5.5"]),
        &runtime,
        AuthCheckOptions::default(),
    )
    .await
    .expect("check");
    assert_eq!(by_model.status, AuthCheckStatus::Ready);
    assert_eq!(by_model.provider, "openai");
    assert_eq!(by_model.auth_type, Some(AuthType::ApiKey));

    let by_both = check_provider_auth(
        &parse(&["--provider", "openai", "--model", "gpt-5.5"]),
        &runtime,
        AuthCheckOptions::default(),
    )
    .await
    .expect("check");
    assert_eq!(by_both.status, AuthCheckStatus::Ready);
    assert_eq!(by_both.provider, "openai");
}

#[tokio::test]
async fn reads_credentials_without_refreshing_oauth_when_requested() {
    let api_storage = auth_storage(&[("openai", json!({"type": "api_key", "key": "test-key"}))]);
    let api_runtime = create_runtime(Arc::new(api_storage.clone())).await;
    assert_eq!(
        get_provider_credential(
            "openai",
            &api_runtime,
            &api_storage,
            AuthCheckOptions::default()
        )
        .await
        .expect("credential"),
        Some("test-key".to_string())
    );

    let oauth_storage = auth_storage(&[("openai-codex", oauth_credential("old-token", 0.0))]);
    let oauth_runtime = create_runtime(Arc::new(oauth_storage.clone())).await;
    assert!(oauth_runtime.get_provider("openai-codex").is_some());
    assert_eq!(
        get_provider_credential(
            "openai-codex",
            &oauth_runtime,
            &oauth_storage,
            AuthCheckOptions::default()
        )
        .await
        .expect("credential"),
        Some("old-token".to_string())
    );
}

#[tokio::test]
async fn refreshes_oauth_by_default() {
    let storage = auth_storage(&[(MOCK_OAUTH_PROVIDER, oauth_credential("old-token", 0.0))]);
    let runtime = create_runtime(Arc::new(storage.clone())).await;
    register_mock_oauth_provider(&runtime);

    let result = check_provider_auth(
        &parse(&["--provider", MOCK_OAUTH_PROVIDER]),
        &runtime,
        AuthCheckOptions { refresh: true },
    )
    .await
    .expect("check");
    assert_eq!(result.status, AuthCheckStatus::Ready);
    assert_eq!(result.auth_type, Some(AuthType::OAuth));

    // TS 断言 `refresh` 被调用一次；Rust 侧以“刷新结果已写回凭证存储”为可观察等价物。
    let stored = storage
        .read(MOCK_OAUTH_PROVIDER, AuthOperationOptions::default())
        .await
        .expect("read");
    let access = stored
        .as_ref()
        .and_then(|credential| credential.as_oauth())
        .map(|oauth| oauth.access.clone());
    assert_eq!(access.as_deref(), Some("fresh-token"));
}

#[tokio::test]
async fn reports_an_unknown_provider_as_not_ready() {
    let runtime = create_runtime(Arc::new(auth_storage(&[]))).await;

    let result = check_provider_auth(
        &parse(&["--provider", "not-installed"]),
        &runtime,
        AuthCheckOptions::default(),
    )
    .await
    .expect("check");
    assert_eq!(result.status, AuthCheckStatus::NotReady);
    assert_eq!(result.provider, "not-installed");
    assert_eq!(result.reason, Some(AuthCheckReason::ProviderNotFound));
}

#[tokio::test]
async fn does_not_treat_an_unresolved_stored_environment_reference_as_configured() {
    let temp_dir = TempDir::new().expect("temp dir");
    let auth_path = temp_dir.path().join("auth.json");
    std::fs::write(
        &auth_path,
        r#"{"openai":{"type":"api_key","key":"$MISSING_AUTH_CHECK_KEY"}}"#,
    )
    .expect("write auth");
    let storage = ReadOnlyAuthStorage::new(&auth_path.to_string_lossy());
    let runtime = create_runtime(Arc::new(storage)).await;

    let result = check_provider_auth(
        &parse(&["--provider", "openai"]),
        &runtime,
        AuthCheckOptions::default(),
    )
    .await
    .expect("check");
    assert_eq!(result.status, AuthCheckStatus::NotReady);
    assert_eq!(result.provider, "openai");
    assert_eq!(
        result.reason,
        Some(AuthCheckReason::CredentialsNotConfigured)
    );
}

#[tokio::test]
async fn reports_malformed_auth_state_as_invalid() {
    let temp_dir = TempDir::new().expect("temp dir");
    let auth_path = temp_dir.path().join("auth.json");
    std::fs::write(&auth_path, "{invalid-json").expect("write auth");
    let storage = ReadOnlyAuthStorage::new(&auth_path.to_string_lossy());
    let runtime = create_runtime(Arc::new(storage)).await;

    let result = check_provider_auth(
        &parse(&["--provider", "openai"]),
        &runtime,
        AuthCheckOptions::default(),
    )
    .await
    .expect("check");
    assert_eq!(result.status, AuthCheckStatus::Invalid);
    assert_eq!(result.provider, "openai");
    assert_eq!(result.reason, Some(AuthCheckReason::InvalidState));
}

#[tokio::test]
async fn does_not_create_an_auth_file_or_its_parent_directory() {
    let temp_dir = TempDir::new().expect("temp dir");
    let auth_path = temp_dir.path().join("agent").join("auth.json");
    let storage = ReadOnlyAuthStorage::new(&auth_path.to_string_lossy());
    let runtime = create_runtime(Arc::new(storage)).await;

    let result = check_provider_auth(
        &parse(&["--provider", "openai"]),
        &runtime,
        AuthCheckOptions::default(),
    )
    .await
    .expect("check");
    assert_eq!(result.status, AuthCheckStatus::NotReady);
    assert_eq!(
        result.reason,
        Some(AuthCheckReason::CredentialsNotConfigured)
    );
    assert!(!auth_path.exists());
    assert!(!temp_dir.path().join("agent").exists());
}

#[test]
fn accepts_optional_json_output_credential_output_and_no_refresh() {
    assert_eq!(
        parse_auth_command(&[
            "auth".to_string(),
            "check".to_string(),
            "--provider".to_string(),
            "openai".to_string()
        ])
        .expect("parse")
        .expect("command"),
        kiana_coding_agent::cli::auth_command::AuthCommand {
            kind: kiana_coding_agent::cli::auth_command::AuthCommandKind::Check,
            args: vec!["--provider".to_string(), "openai".to_string()],
            json: false,
            credentials: false,
            no_refresh: false,
            min_expiry_ms: None,
        }
    );
    let command = parse_auth_command(
        &[
            "auth",
            "check",
            "--json",
            "--credentials",
            "--no-refresh",
            "--provider",
            "openai",
        ]
        .iter()
        .map(|arg| arg.to_string())
        .collect::<Vec<_>>(),
    )
    .expect("parse")
    .expect("command");
    assert_eq!(
        command.kind,
        kiana_coding_agent::cli::auth_command::AuthCommandKind::Check
    );
    assert_eq!(
        command.args,
        vec!["--provider".to_string(), "openai".to_string()]
    );
    assert!(command.json);
    assert!(command.credentials);
    assert!(command.no_refresh);
}

#[tokio::test]
async fn creates_an_auth_check_runtime_without_catalog_storage() {
    let temp_dir = TempDir::new().expect("temp dir");
    // 只在修改环境变量时持有 std Mutex；await 期间不跨锁（本文件没有其它环境变量测试）。
    let previous = {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var_os("KIANA_CODING_AGENT_DIR");
        // SAFETY: 测试用 ENV_LOCK 串行化进程环境变量的读写。
        unsafe {
            std::env::set_var("KIANA_CODING_AGENT_DIR", temp_dir.path());
        }
        previous
    };
    let result = create_auth_check_model_runtime(Arc::new(auth_storage(&[]))).await;
    {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: 同上。
        unsafe {
            match previous {
                Some(value) => std::env::set_var("KIANA_CODING_AGENT_DIR", value),
                None => std::env::remove_var("KIANA_CODING_AGENT_DIR"),
            }
        }
    }
    let runtime = result.expect("runtime");
    assert!(runtime.get_provider("openai").is_some());
}

#[test]
fn auth_check_json_uses_ts_field_order() {
    use kiana_coding_agent::cli::auth_check::{
        AuthCheckJson, AuthCheckReason, AuthCheckResult, AuthCheckStatus,
    };

    let ready = AuthCheckResult {
        status: AuthCheckStatus::Ready,
        provider: "openai".to_string(),
        reason: None,
        auth_type: Some(AuthType::ApiKey),
    };
    assert_eq!(
        AuthCheckJson::from_result(&ready, Some("sk-test")).to_json(),
        r#"{"status":"ready","provider":"openai","authType":"api_key","credentials":"sk-test"}"#
    );

    let not_ready = AuthCheckResult {
        status: AuthCheckStatus::NotReady,
        provider: "not-installed".to_string(),
        reason: Some(AuthCheckReason::ProviderNotFound),
        auth_type: None,
    };
    assert_eq!(
        AuthCheckJson::from_result(&not_ready, None).to_json(),
        r#"{"status":"not_ready","provider":"not-installed","reason":"provider_not_found"}"#
    );

    let invalid = AuthCheckResult {
        status: AuthCheckStatus::Invalid,
        provider: "openai".to_string(),
        reason: Some(AuthCheckReason::InvalidState),
        auth_type: None,
    };
    assert_eq!(
        AuthCheckJson::from_result(&invalid, None).to_json(),
        r#"{"status":"invalid","provider":"openai","reason":"invalid_state"}"#
    );
}
