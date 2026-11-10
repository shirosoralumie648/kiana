//! `test/credential-print.test.ts` 的逐条移植。
//!
//! 与 TS 的差异：
//! - “refreshes an expired OAuth token before printing it” 在 TS 里替换 provider 的
//!   `oauth.refresh`；Rust 侧用 `register_native_provider` 注册一个刷新返回固定 token 的
//!   测试 provider（并额外验证刷新结果已持久化）；
//! - “reports unknown auth options like package commands” 在 TS 里经 `main()` 观察
//!   stderr/exitCode；`main` 属于 app-and-sdk 单元，这里用 `parseAuthCommand` +
//!   `parseArgs` + `validateAuthCommandArgs` 断言同样的两条消息。

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{
    AuthError, AuthInteraction, AuthOperationOptions, CredentialStore, LoginOptions, ModelAuth,
    OAuthAuth, OAuthCredential, ProviderAuth,
};
use kiana_ai::models::store::InMemoryModelsStore;
use kiana_ai::models::{CreateProviderOptions, ProviderApiImpl, create_provider};
use kiana_ai::providers::faux::{RegisterFauxProviderOptions, create_faux_core};
use kiana_ai::types::ProviderHeaders;
use kiana_ai::utils::abort::AbortScope;
use kiana_coding_agent::cli::args::parse_args;
use kiana_coding_agent::cli::auth_command::{
    AuthCommand, AuthCommandError, AuthCommandKind, get_auth_command_usage, is_auth_command_help,
    parse_auth_command, validate_auth_command_args,
};
use kiana_coding_agent::cli::credential_print::{
    DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS, resolve_credential_for_print,
};
use kiana_config::auth::storage::AuthStorage;
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use serde_json::{Value, json};
use tempfile::TempDir;

/// TS 测试替换 `oauth.refresh`；Rust 侧注册一个返回固定 token 的原生 provider。
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
            "fresh-test-token",
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

fn parse(args: &[&str]) -> kiana_coding_agent::cli::args::Args {
    parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
}

fn parse_owned(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| arg.to_string()).collect()
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
        "refresh": "test-refresh-token",
        "expires": expires,
    })
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

#[tokio::test]
async fn prints_a_resolved_api_key() {
    let runtime = create_runtime(Arc::new(auth_storage(&[(
        "openai",
        json!({"type": "api_key", "key": "test-api-key"}),
    )])))
    .await;
    let args = parse(&["--provider", "openai"]);
    assert_eq!(
        resolve_credential_for_print(&args, &runtime, AuthCommandKind::ApiKey, None, None)
            .await
            .expect("credential"),
        "test-api-key"
    );
}

#[tokio::test]
async fn resolves_model_without_provider_only_among_catalog_models_of_configured_providers() {
    let runtime = create_runtime(Arc::new(auth_storage(&[
        (
            "openai",
            json!({"type": "api_key", "key": "openai-test-key"}),
        ),
        (
            "anthropic",
            json!({"type": "api_key", "key": "anthropic-test-key"}),
        ),
    ])))
    .await;

    let args = parse(&["--model", "gpt-5.5"]);
    assert_eq!(
        resolve_credential_for_print(&args, &runtime, AuthCommandKind::ApiKey, None, None)
            .await
            .expect("credential"),
        "openai-test-key"
    );
}

#[tokio::test]
async fn prints_bearer_tokens_resolved_from_an_authorization_header() {
    let runtime = create_runtime(Arc::new(auth_storage(&[(
        "kimi-coding",
        oauth_credential("header-test-token", now_ms() + 60.0 * 60.0 * 1000.0),
    )])))
    .await;
    let args = parse(&["--provider", "kimi-coding"]);
    assert_eq!(
        resolve_credential_for_print(&args, &runtime, AuthCommandKind::BearerToken, None, None)
            .await
            .expect("credential"),
        "header-test-token"
    );
}

#[tokio::test]
async fn refreshes_an_expired_oauth_token_before_printing_it() {
    let storage = auth_storage(&[(MOCK_OAUTH_PROVIDER, oauth_credential("old-test-token", 0.0))]);
    let runtime = create_runtime(Arc::new(storage.clone())).await;
    register_mock_oauth_provider(&runtime);
    let args = parse(&["--provider", MOCK_OAUTH_PROVIDER]);
    assert_eq!(
        resolve_credential_for_print(&args, &runtime, AuthCommandKind::BearerToken, None, None)
            .await
            .expect("credential"),
        "fresh-test-token"
    );
    // TS 断言 `refresh` 被调用一次；Rust 侧以刷新后的凭证已写回存储为可观察等价物。
    let stored = storage
        .read(MOCK_OAUTH_PROVIDER, AuthOperationOptions::default())
        .await
        .expect("read");
    let access = stored
        .as_ref()
        .and_then(|credential| credential.as_oauth())
        .map(|oauth| oauth.access.clone());
    assert_eq!(access.as_deref(), Some("fresh-test-token"));
}

#[tokio::test]
async fn reports_unknown_auth_options_like_package_commands() {
    let command = parse_auth_command(&parse_owned(&[
        "auth",
        "check",
        "--provider",
        "openai-codex",
        "--credentails",
    ]))
    .expect("parse")
    .expect("command");
    let args = parse_args(&command.args);
    let error = validate_auth_command_args(&args, command.kind).expect_err("unknown option");
    assert_eq!(
        error.message,
        "Unknown option --credentails for \"auth check\"."
    );
    assert_eq!(
        format!(
            "Use \"kiana --help\" or \"{}\".",
            get_auth_command_usage(AuthCommandKind::Check)
        ),
        "Use \"kiana --help\" or \"kiana auth check --provider <provider> [--json] [--credentials] [--no-refresh]\"."
    );
}

#[tokio::test]
async fn parses_credential_commands_and_rejects_invalid_arguments_or_credential_types() {
    let runtime = create_runtime(Arc::new(auth_storage(&[(
        "openai-codex",
        oauth_credential(
            "test-token-not-to-be-printed",
            now_ms() + 60.0 * 60.0 * 1000.0,
        ),
    )])))
    .await;

    assert_eq!(
        parse_auth_command(&parse_owned(&[
            "auth",
            "print-api-key",
            "--provider",
            "openai"
        ]))
        .expect("parse")
        .expect("command"),
        AuthCommand {
            kind: AuthCommandKind::ApiKey,
            args: vec!["--provider".to_string(), "openai".to_string()],
            json: false,
            credentials: false,
            no_refresh: false,
            min_expiry_ms: None,
        }
    );
    assert_eq!(
        parse_auth_command(&parse_owned(&["auth", "print-bearer-token"]))
            .expect("parse")
            .expect("command")
            .kind,
        AuthCommandKind::BearerToken
    );
    assert_eq!(
        parse_auth_command(&parse_owned(&[
            "auth",
            "print-bearer-token",
            "--min-expiry",
            "30m"
        ]))
        .expect("parse")
        .expect("command"),
        AuthCommand {
            kind: AuthCommandKind::BearerToken,
            args: Vec::new(),
            json: false,
            credentials: false,
            no_refresh: false,
            min_expiry_ms: Some(DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS),
        }
    );
    let error = parse_auth_command(&parse_owned(&[
        "auth",
        "print-api-key",
        "--min-expiry",
        "30m",
    ]))
    .expect_err("min-expiry");
    assert!(
        error
            .message
            .contains("only supported by print-bearer-token")
    );

    assert!(is_auth_command_help(&parse_owned(&["auth", "--help"])));
    assert!(is_auth_command_help(&parse_owned(&[
        "auth",
        "print-api-key",
        "--help"
    ])));
    assert!(is_auth_command_help(&parse_owned(&[
        "auth",
        "print-bearer-token",
        "-h"
    ])));
    assert!(is_auth_command_help(&parse_owned(&[
        "auth", "check", "--help"
    ])));

    assert!(matches!(
        parse_auth_command(&parse_owned(&["auth", "unknown"])),
        Err(AuthCommandError { .. })
    ));

    let error =
        resolve_credential_for_print(&parse(&[]), &runtime, AuthCommandKind::ApiKey, None, None)
            .await
            .expect_err("requires provider");
    assert!(
        error
            .message
            .contains("requires --provider <provider> or --model <model>")
    );

    let error = resolve_credential_for_print(
        &parse(&["--provider", "openai-codex"]),
        &runtime,
        AuthCommandKind::ApiKey,
        None,
        None,
    )
    .await
    .expect_err("configured with oauth");
    assert!(error.message.contains("configured with OAuth"));
}

#[test]
fn auth_command_help_text_matches_ts() {
    use kiana_coding_agent::cli::auth_command::auth_command_help_text;
    // 由 TS 的 `printAuthCommandHelp()` 逐字生成（JSON 转义后比对）。
    assert_eq!(
        auth_command_help_text(),
        "Usage:\n  kiana auth print-api-key [--provider <provider>] [--model <model>]\n  kiana auth print-bearer-token [--provider <provider>] [--model <model>] [--min-expiry <duration>]\n  kiana auth check [--provider <provider>] [--model <model>] [--json] [--credentials] [--no-refresh]\n\nAuth commands require at least one of --provider or --model. Checks refresh expired OAuth credentials by default; --no-refresh prevents this. --credentials emits the credential, or includes it in JSON output.\n"
    );
}

// ---------------------------------------------------------------------------
// 回归：与 TS 参考实现的差分（reviewer findings）
// ---------------------------------------------------------------------------

/// TS `main.ts:204` 只对 `AuthCommandError` 打印原文；凭证存储读取失败等
/// 其它异常统一打印 `"Failed to resolve credential"`。
#[tokio::test]
async fn credential_store_read_failures_print_the_ts_fallback_message() {
    let agent_dir = TempDir::new().expect("temp dir");
    let auth_path = agent_dir.path().join("auth.json");
    // 损坏的 JSON：AuthStorage 的异步重读会返回 AuthError（ModelsError），不是 AuthCommandError。
    std::fs::write(&auth_path, "{\"openai\":").expect("write auth");
    let storage = AuthStorage::create(&auth_path.to_string_lossy());
    let runtime = create_runtime(Arc::new(storage)).await;

    let error = resolve_credential_for_print(
        &parse(&["--provider", "openai"]),
        &runtime,
        AuthCommandKind::ApiKey,
        None,
        None,
    )
    .await
    .expect_err("corrupt auth must fail");
    assert_eq!(error.message, "Failed to resolve credential");
}

fn auth_with_authorization(value: &str) -> kiana_ai::auth::types::AuthResult {
    let mut headers = indexmap::IndexMap::new();
    headers.insert("Authorization".to_string(), Some(value.to_string()));
    kiana_ai::auth::types::AuthResult::new(kiana_ai::auth::types::ModelAuth {
        api_key: None,
        headers: Some(headers),
        base_url: None,
    })
}

/// JS `/^Bearer\s+(.+)$/iu`：`$` 不匹配末尾行终止符之前，`.` 排除行终止符，
/// `\s` 含 U+FEFF 不含 U+0085。
#[test]
fn bearer_token_extraction_matches_the_js_regex() {
    use kiana_coding_agent::cli::auth_command::get_auth_credential;

    let cases: &[(&str, Option<&str>)] = &[
        ("Bearer x", Some("x")),
        ("bearer x", Some("x")),
        ("BEARER x", Some("x")),
        ("Bearer  x ", Some("x ")),
        ("Bearer\tx", Some("x")),
        ("Bearer\u{00a0}x", Some("x")),
        ("Bearer\u{feff}x", Some("x")),
        ("Bearer\nx", Some("x")),
        ("Bearer x\n", None),
        ("Bearer x\r\n", None),
        ("Bearer x\r", None),
        ("Bearer x\u{2028}", None),
        ("Bearer x\u{2029}", None),
        ("Bearer a\rb", None),
        ("Bearer a\nb", None),
        ("Bearer\u{0085}x", None),
        ("Bearer ", None),
        ("XBearer x", None),
    ];
    for (header, expected) in cases {
        let auth = auth_with_authorization(header);
        assert_eq!(
            get_auth_credential(Some(&auth)).as_deref(),
            *expected,
            "{header:?}"
        );
    }
}

/// `Number(digits)` 语义：超长/超范围也接受（TS 得到 3.6e30/Infinity），
/// i64 类型下饱和；旧实现用 `u128` 解析，超过 38 位会误报参数错误。
#[test]
fn min_expiry_uses_js_number_semantics_instead_of_u128() {
    let parse_command = |value: &str| {
        parse_auth_command(&parse_owned(&[
            "auth",
            "print-bearer-token",
            "--provider",
            "x",
            "--min-expiry",
            value,
        ]))
        .expect("parse")
        .expect("command")
        .min_expiry_ms
    };
    assert_eq!(
        parse_command("30m"),
        Some(DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS)
    );
    // TS 的正则用 `/iu` 匹配，但下一步 `unit === "ms" | "s" | "m"` 是区分大小写的，
    // 因此任何非小写单位都落到 3_600_000 的默认分支（`30M` = 30 小时）。
    assert_eq!(parse_command("30M"), Some(108_000_000));
    assert_eq!(parse_command("30H"), Some(108_000_000));
    assert_eq!(parse_command("30S"), Some(108_000_000));
    assert_eq!(parse_command("30MS"), Some(108_000_000));
    assert_eq!(parse_command("30s"), Some(30_000));
    assert_eq!(parse_command("30ms"), Some(30));
    assert!(
        parse_auth_command(&parse_owned(&[
            "auth",
            "print-bearer-token",
            "--provider",
            "x",
            "--min-expiry",
            "30m\n",
        ]))
        .is_err()
    );
    assert_eq!(parse_command("999999999999999999999999h"), Some(i64::MAX));
    let enormous = "9".repeat(400);
    assert_eq!(parse_command(&format!("{enormous}h")), Some(i64::MAX));
}
