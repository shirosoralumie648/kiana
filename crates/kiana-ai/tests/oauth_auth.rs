//! `test/oauth-auth.test.ts` 的移植：OAuthAuth 适配器与 lazy 加载链。
//!
//! 含第 12、13 个用例（`OAuth through Models.getAuth` 的 lazy 加载链，models 单元已合入）。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use kiana_ai::auth::credential_store::InMemoryCredentialStore;
use kiana_ai::auth::types::{Credential, CredentialStore};
use kiana_ai::models::{CreateModelsOptions, GetAuthTarget, create_models};
use kiana_ai::providers::anthropic::anthropic_provider;
use kiana_ai::providers::github_copilot::github_copilot_provider;

use kiana_ai::auth::oauth::device_code::{AuthClock, SystemAuthClock};
use kiana_ai::auth::oauth::testing::{MockFetch, respond};
use kiana_ai::auth::oauth::{
    OAuthFetchRequest, OAuthFetchResponse, anthropic, github_copilot, kimi_coding, meta,
    openai_chatgpt, openai_codex, openrouter, xai,
};
use kiana_ai::auth::types::{OAuthAuth, OAuthCredential};
use kiana_ai::utils::abort::AbortScope;
use serde_json::{Value, json};

fn credential(access: &str) -> OAuthCredential {
    OAuthCredential::new(access, "r", 0.0)
}

fn json_response(body: Value, status: u16) -> OAuthFetchResponse {
    OAuthFetchResponse::json_response(status, &body)
}

#[test]
fn identifies_only_subscription_backed_oauth_flows_as_subscriptions() {
    let flows: Vec<Arc<dyn OAuthAuth>> = vec![
        Arc::new(anthropic::AnthropicOAuth::new()),
        Arc::new(openai_chatgpt::OpenAIChatGPTOAuth::new()),
        Arc::new(openai_codex::OpenAICodexOAuth::new()),
        Arc::new(github_copilot::GitHubCopilotOAuth::new()),
        Arc::new(kimi_coding::KimiCodingOAuth::new()),
        Arc::new(xai::XaiOAuth::new()),
    ];
    for oauth in flows {
        assert!(oauth.is_subscription(), "{}", oauth.name());
    }
    assert!(!openrouter::OpenRouterOAuth::new().is_subscription());
    // meta 也是订阅（TS 的列表漏了它，不要断言数量）。
    assert!(meta::MetaOAuth::new().is_subscription());
}

#[tokio::test]
async fn anthropic_to_auth_derives_the_api_key_from_the_access_token() {
    let auth = anthropic::AnthropicOAuth::new()
        .to_auth(&credential("token"))
        .await
        .expect("auth");
    assert_eq!(auth.api_key.as_deref(), Some("token"));
    assert!(auth.base_url.is_none());
    assert!(auth.headers.is_none());
}

#[tokio::test]
async fn openai_chatgpt_flow_exposes_subscription_metadata() {
    let oauth = openai_chatgpt::OpenAIChatGPTOAuth::new();
    assert_eq!(oauth.name(), "OpenAI (ChatGPT subscription)");
    assert!(oauth.is_subscription());
    assert_eq!(oauth.login_label(), Some("Sign in with ChatGPT"));
}

#[tokio::test]
async fn openai_codex_to_auth_derives_the_api_key_from_the_access_token() {
    let auth = openai_codex::OpenAICodexOAuth::new()
        .to_auth(&credential("token"))
        .await
        .expect("auth");
    assert_eq!(auth.api_key.as_deref(), Some("token"));
}

#[tokio::test]
async fn openrouter_derives_the_api_key_and_keeps_the_permanent_credential_on_refresh() {
    let oauth = openrouter::OpenRouterOAuth::new();
    let mut permanent = OAuthCredential::new("token", "", 9_007_199_254_740_991.0);
    permanent.access = "token".into();
    let auth = oauth.to_auth(&permanent).await.expect("auth");
    assert_eq!(auth.api_key.as_deref(), Some("token"));
    let refreshed = oauth
        .refresh(&permanent, &AbortScope::new())
        .await
        .expect("refresh");
    assert_eq!(refreshed, permanent);
}

#[tokio::test]
async fn xai_to_auth_derives_the_api_key_from_the_access_token() {
    let auth = xai::XaiOAuth::new()
        .to_auth(&credential("token"))
        .await
        .expect("auth");
    assert_eq!(auth.api_key.as_deref(), Some("token"));
}

#[tokio::test]
async fn github_copilot_to_auth_derives_base_url_from_the_token_proxy_endpoint() {
    let access = "tid=abc;exp=123;proxy-ep=proxy.enterprise.example;rest";
    let auth = github_copilot::GitHubCopilotOAuth::new()
        .to_auth(&credential(access))
        .await
        .expect("auth");
    assert_eq!(auth.api_key.as_deref(), Some(access));
    assert_eq!(
        auth.base_url.as_deref(),
        Some("https://api.enterprise.example")
    );
}

#[tokio::test]
async fn github_copilot_to_auth_falls_back_to_the_enterprise_domain_then_individual() {
    let mut enterprise = credential("no-proxy-ep");
    enterprise.extra.insert(
        "enterpriseUrl".into(),
        Value::String("https://company.ghe.com".into()),
    );
    let auth = github_copilot::GitHubCopilotOAuth::new()
        .to_auth(&enterprise)
        .await
        .expect("auth");
    assert_eq!(
        auth.base_url.as_deref(),
        Some("https://copilot-api.company.ghe.com")
    );

    let individual = github_copilot::GitHubCopilotOAuth::new()
        .to_auth(&credential("no-proxy-ep"))
        .await
        .expect("auth");
    assert_eq!(
        individual.base_url.as_deref(),
        Some("https://api.individual.githubcopilot.com")
    );
}

#[tokio::test]
async fn anthropic_refresh_exchanges_the_refresh_token_and_returns_a_typed_credential() {
    let fetch = MockFetch::new(respond(json_response(
        json!({"access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600}),
        200,
    )));
    let fetch: Arc<dyn kiana_ai::auth::oauth::OAuthFetch> = fetch.clone();
    let oauth = anthropic::AnthropicOAuth::with_fetch(fetch);
    let before = SystemAuthClock.now_ms();
    let refreshed = oauth
        .refresh(
            &OAuthCredential::new("old", "old-r", 0.0),
            &AbortScope::new(),
        )
        .await
        .expect("refresh");
    assert_eq!(refreshed.access, "new-access");
    assert_eq!(refreshed.refresh, "new-refresh");
    assert!(refreshed.expires > before);
}

#[tokio::test]
async fn github_copilot_refresh_preserves_the_enterprise_domain() {
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        let url = request.url.clone();
        Box::pin(async move {
            if url.ends_with("/models") {
                Ok(json_response(json!({"data": []}), 200))
            } else {
                Ok(json_response(
                    json!({"token": "new-token", "expires_at": 9999999999i64}),
                    200,
                ))
            }
        })
    });
    let fetch_handle: Arc<dyn kiana_ai::auth::oauth::OAuthFetch> = fetch.clone();
    let oauth = github_copilot::GitHubCopilotOAuth::with_fetch(fetch_handle);
    let mut stored = OAuthCredential::new("old", "gh-token", 0.0);
    stored.extra.insert(
        "enterpriseUrl".into(),
        Value::String("company.ghe.com".into()),
    );
    let refreshed = oauth
        .refresh(&stored, &AbortScope::new())
        .await
        .expect("refresh");
    assert_eq!(refreshed.access, "new-token");
    assert_eq!(refreshed.get_str("enterpriseUrl"), Some("company.ghe.com"));
    let urls: Vec<String> = fetch
        .requests()
        .into_iter()
        .map(|request| request.url)
        .collect();
    assert!(urls[0].contains("api.company.ghe.com"));
}

/// 黄金 fixtures：`URLSearchParams` 序列化与 `atob` 样本由 Node 26 生成；凭证形状按
/// **Rust 的固定序列化顺序**（`access`/`refresh`/`expires`/`extra`，见 `auth/types.rs` 与
/// spec/ai-auth.md §4.1）。TS 的 anthropic/meta/github-copilot 以 `refresh` 开头，这是
/// 冻结类型上有意选择的 spec-over-TS 顺序（fixture 里的 `github-copilot` 条目因此不是
/// TS 的逐字节快照；见 `golden.json` 的 `note`）。
#[test]
fn golden_fixtures_match_typescript() {
    let fixture: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/auth-oauth-flows/golden.json"
    )))
    .expect("golden fixture");

    for case in fixture["formUrlencoded"].as_array().expect("form cases") {
        let pairs: Vec<(String, String)> = case["pairs"]
            .as_array()
            .expect("pairs")
            .iter()
            .map(|pair| {
                let pair = pair.as_array().expect("pair");
                (
                    pair[0].as_str().expect("key").to_string(),
                    pair[1].as_str().expect("value").to_string(),
                )
            })
            .collect();
        let refs: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        assert_eq!(
            kiana_ai::auth::oauth::form_urlencode(&refs),
            case["expected"].as_str().expect("expected"),
            "form case {pairs:?}"
        );
    }

    let credential_json = |credential: OAuthCredential| {
        let value = serde_json::to_value(kiana_ai::auth::types::Credential::OAuth(credential))
            .expect("serialize credential");
        kiana_json::js_object::stringify(&value)
    };
    let mut codex = OAuthCredential::new("access", "refresh", 1_779_238_800_000.0);
    codex
        .extra
        .insert("accountId".into(), Value::String("acct".into()));
    let mut chatgpt = OAuthCredential::new("access", "refresh", 1_779_238_800_000.0);
    chatgpt
        .extra
        .insert("clientId".into(), Value::String("oaiapp".into()));
    chatgpt
        .extra
        .insert("scopes".into(), json!(["openid", "profile"]));
    let openrouter = OAuthCredential::new("sk-or", "", 9_007_199_254_740_991.0);
    let mut radius = OAuthCredential::new("access", "refresh", 1_779_238_740_000.0);
    radius.extra.insert(
        "scope".into(),
        Value::String("gateway offline_access".into()),
    );
    let xai = OAuthCredential::new("access", "refresh", 1_779_238_500_000.0);
    let kimi = OAuthCredential::new("access", "refresh", 1_779_238_800_000.0);
    let mut copilot = OAuthCredential::new(
        "tid=t;exp=1;proxy-ep=proxy.individual.githubcopilot.com;",
        "ghu",
        9_999_996_990_000.0,
    );
    copilot.extra.insert(
        "enterpriseUrl".into(),
        Value::String("company.ghe.com".into()),
    );
    copilot
        .extra
        .insert("availableModelIds".into(), json!(["a", "b"]));

    let cases = [
        ("openai-codex", codex),
        ("openai-chatgpt", chatgpt),
        ("openrouter", openrouter),
        ("radius", radius),
        ("xai", xai),
        ("kimi-coding", kimi),
        ("github-copilot", copilot),
    ];
    for (flow, credential) in cases {
        assert_eq!(
            credential_json(credential),
            fixture["credentials"][flow]
                .as_str()
                .expect("credential fixture"),
            "credential shape for {flow}"
        );
    }

    for case in fixture["atobSamples"].as_array().expect("atob samples") {
        let case = case.as_array().expect("sample");
        let input = case[0].as_str().expect("input");
        let expected = case[1].as_str();
        let decoded = kiana_ai::auth::oauth::atob_standard(input)
            .map(|bytes| bytes.iter().map(|byte| *byte as char).collect::<String>());
        assert_eq!(decoded.as_deref(), expected, "atob({input:?})");
    }
}

#[tokio::test]
async fn loaders_expose_the_builtin_flows() {
    use kiana_ai::auth::oauth::load;
    let anthropic = load::load_anthropic_oauth().await.expect("anthropic");
    assert_eq!(anthropic.name(), "Anthropic (Claude Pro/Max)");
    let radius = load::load_radius_oauth(kiana_ai::auth::oauth::RadiusOAuthOptions {
        name: "Company".into(),
        gateway: "https://radius.example".into(),
    })
    .await
    .expect("radius");
    assert_eq!(radius.name(), "Company");
    assert!(load::get_oauth_auth("openai").is_some());
}

/// TS `expires: Date.now() + 10 * 60_000`：超出 `getAuth()` 的 5 分钟刷新窗口。
fn unexpired_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as f64
        + 10.0 * 60_000.0
}

#[tokio::test]
async fn resolves_stored_anthropic_oauth_credentials_via_the_lazy_flow_import() {
    let credentials = InMemoryCredentialStore::new();
    credentials
        .modify(
            "anthropic",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "oauth-access-token",
                        "r",
                        unexpired_ms(),
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
    models.set_provider(anthropic_provider());

    let result = models
        .get_auth(GetAuthTarget::Provider("anthropic"), Default::default())
        .await
        .unwrap()
        .expect("stored oauth credential resolves");

    assert_eq!(result.auth.api_key.as_deref(), Some("oauth-access-token"));
    assert_eq!(result.source.as_deref(), Some("OAuth"));
}

#[tokio::test]
async fn resolves_stored_github_copilot_oauth_credentials_including_per_credential_base_url() {
    const ACCESS: &str = "tid=abc;exp=123;proxy-ep=proxy.business.githubcopilot.com;rest";
    let credentials = InMemoryCredentialStore::new();
    credentials
        .modify(
            "github-copilot",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        ACCESS,
                        "r",
                        unexpired_ms(),
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
    models.set_provider(github_copilot_provider());

    let result = models
        .get_auth(
            GetAuthTarget::Provider("github-copilot"),
            Default::default(),
        )
        .await
        .unwrap()
        .expect("stored oauth credential resolves");

    assert_eq!(result.auth.api_key.as_deref(), Some(ACCESS));
    assert_eq!(
        result.auth.base_url.as_deref(),
        Some("https://api.business.githubcopilot.com")
    );
}
