//! `test/github-copilot-oauth.test.ts` 的移植（13 个用例）。
//!
//! provider/store 相关断言（`githubCopilotProvider().getModels()`、`InMemoryCredentialStore`
//! 持久化）由 ai-core/provider 单元覆盖；这里用 `providers/catalog.rs` 的内置 Copilot 表
//! 取得同样的模型 id，并直接断言凭证字段。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_ai::auth::oauth::GitHubCopilotOAuth;
use kiana_ai::auth::oauth::testing::{MockFetch, MockInteraction, VirtualClock, responding_prompt};
use kiana_ai::auth::oauth::{OAuthFetch, OAuthFetchRequest, OAuthFetchResponse};
use kiana_ai::auth::types::{AuthEvent, OAuthAuth, OAuthCredential};
use kiana_ai::providers::catalog;
use kiana_ai::utils::abort::AbortScope;
use serde_json::{Value, json};

const TEST_COPILOT_ACCESS_TOKEN: &str =
    "tid=test;exp=9999999999;proxy-ep=proxy.individual.githubcopilot.com;";
const TEST_COPILOT_MODELS_URL: &str = "https://api.individual.githubcopilot.com/models";
/// 2026-03-09T00:00:00Z。
const START_MS: f64 = 1_773_014_400_000.0;

fn json_response(body: Value, status: u16, headers: &[(&str, &str)]) -> OAuthFetchResponse {
    let mut response = OAuthFetchResponse::json_response(status, &body);
    for (name, value) in headers {
        response = response.with_header(*name, *value);
    }
    response
}

fn catalog_model_ids() -> Vec<String> {
    catalog::get_models("github-copilot")
        .into_iter()
        .filter_map(|model| model.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn require_model_id(ids: &[String], index: usize) -> String {
    ids.get(index)
        .unwrap_or_else(|| panic!("Expected a GitHub Copilot model at index {index}"))
        .clone()
}

fn oauth_with(fetch: &Arc<MockFetch>, clock: Arc<VirtualClock>) -> GitHubCopilotOAuth {
    let fetch: Arc<dyn OAuthFetch> = fetch.clone();
    GitHubCopilotOAuth::with_dependencies(fetch, clock)
}

/// `refreshGitHubCopilotModelsForTest`：token + models 两个请求。
async fn refresh_models(
    data: Value,
    proxy_host: &str,
) -> Result<OAuthCredential, kiana_ai::auth::types::AuthError> {
    let access_token = format!("tid=test;exp=9999999999;proxy-ep={proxy_host};");
    let models_url = format!(
        "https://{}/models",
        proxy_host.replacen("proxy.", "api.", 1)
    );
    let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
        let url = request.url.clone();
        if url.contains("/copilot_internal/v2/token") {
            let access_token = access_token.clone();
            return Box::pin(async move {
                Ok(json_response(
                    json!({"token": access_token, "expires_at": 9999999999i64}),
                    200,
                    &[],
                ))
            });
        }
        if url == models_url {
            let access_token = access_token.clone();
            assert_eq!(
                request.header_value("Authorization"),
                Some(format!("Bearer {access_token}").as_str())
            );
            let data = data.clone();
            return Box::pin(async move { Ok(json_response(json!({"data": data}), 200, &[])) });
        }
        panic!("Unexpected fetch URL: {url}");
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    oauth
        .refresh(
            &OAuthCredential::new("old-access-token", "ghu_refresh_token", 0.0),
            &AbortScope::new(),
        )
        .await
}

fn available_model_ids(credential: &OAuthCredential) -> Vec<String> {
    credential
        .get("availableModelIds")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn device_code_info(
    interaction: &MockInteraction,
) -> Vec<(String, String, Option<f64>, Option<f64>)> {
    interaction
        .events()
        .into_iter()
        .filter_map(|event| match event {
            AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                interval_seconds,
                expires_in_seconds,
                ..
            } => Some((
                user_code,
                verification_uri,
                interval_seconds,
                expires_in_seconds,
            )),
            _ => None,
        })
        .collect()
}

async fn wait_for_device_code(interaction: &MockInteraction) {
    for _ in 0..100_000 {
        if !device_code_info(interaction).is_empty() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("device_code event was not emitted");
}

async fn advance_step(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

/// 模型目录响应工厂。
type ModelsFn = Arc<dyn Fn() -> OAuthFetchResponse + Send + Sync>;
/// 策略响应工厂。
type PolicyFn = Arc<dyn Fn(&str) -> OAuthFetchResponse + Send + Sync>;

/// 登录使用的 fetch 路由（`stubGitHubCopilotLoginFetch`）。
struct LoginFetchConfig {
    models: ModelsFn,
    policy: Option<PolicyFn>,
}

fn login_fetch(config: LoginFetchConfig) -> Arc<MockFetch> {
    MockFetch::new(move |request: OAuthFetchRequest| {
        let url = request.url.clone();
        if url.ends_with("/login/device/code") {
            return Box::pin(async {
                Ok(json_response(
                    json!({
                        "device_code": "device-code",
                        "user_code": "ABCD-EFGH",
                        "verification_uri": "https://github.com/login/device",
                        "interval": 1,
                        "expires_in": 900,
                    }),
                    200,
                    &[],
                ))
            });
        }
        if url.ends_with("/login/oauth/access_token") {
            return Box::pin(async {
                Ok(json_response(
                    json!({"access_token": "ghu_refresh_token"}),
                    200,
                    &[],
                ))
            });
        }
        if url.contains("/copilot_internal/v2/token") {
            return Box::pin(async {
                Ok(json_response(
                    json!({"token": TEST_COPILOT_ACCESS_TOKEN, "expires_at": 9999999999i64}),
                    200,
                    &[],
                ))
            });
        }
        if url == TEST_COPILOT_MODELS_URL {
            // TS：/models 的 init 自带 Accept（Rust 不额外再加，避免重复）。
            assert_eq!(request.header_value("Accept"), Some("application/json"));
            let response = (config.models)();
            return Box::pin(async move { Ok(response) });
        }
        if url.starts_with(&format!("{TEST_COPILOT_MODELS_URL}/")) && url.ends_with("/policy") {
            // TS 的 /policy init 没有 Accept（传输层补 `*/*`）；Rust 只转发调用方头。
            assert!(
                request.header_value("Accept").is_none(),
                "policy requests must not add an Accept header"
            );
            let model_id = url
                [format!("{TEST_COPILOT_MODELS_URL}/").len()..url.len() - "/policy".len()]
                .to_string();
            let Some(policy) = config.policy.clone() else {
                panic!("Unexpected policy request: {url}");
            };
            let response = policy(&model_id);
            return Box::pin(async move { Ok(response) });
        }
        panic!("Unexpected fetch URL: {url}");
    })
}

fn login_interaction() -> Arc<MockInteraction> {
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| responding_prompt(""));
    interaction
}

#[tokio::test]
async fn filters_models_to_the_authenticated_account_picker_catalog() {
    let ids = catalog_model_ids();
    let picker = require_model_id(&ids, 0);
    let disabled = require_model_id(&ids, 1);
    let hidden = require_model_id(&ids, 2);
    let credentials = refresh_models(
        json!([
            {"id": picker, "model_picker_enabled": true, "capabilities": {"supports": {"tool_calls": true}}},
            {"id": disabled, "model_picker_enabled": true, "policy": {"state": "disabled"}, "capabilities": {"supports": {"tool_calls": true}}},
            {"id": hidden, "model_picker_enabled": false, "policy": {"state": "enabled"}, "capabilities": {"supports": {"tool_calls": true}}}
        ]),
        "proxy.individual.githubcopilot.com",
    )
    .await
    .expect("refresh");
    assert_eq!(available_model_ids(&credentials), vec![picker]);
}

#[tokio::test]
async fn falls_back_to_explicitly_enabled_policy_models_when_the_picker_catalog_is_empty() {
    let enabled = require_model_id(&catalog_model_ids(), 0);
    let credentials = refresh_models(
        json!([
            {"id": enabled, "model_picker_enabled": false, "policy": {"state": "enabled"}, "capabilities": {"supports": {"tool_calls": true}}},
            {"id": "policy-disabled-model", "model_picker_enabled": false, "policy": {"state": "disabled"}, "capabilities": {"supports": {"tool_calls": true}}},
            {"id": "unconfigured-model", "model_picker_enabled": false, "capabilities": {"supports": {"tool_calls": true}}},
            {"id": "tool-incapable-model", "model_picker_enabled": false, "policy": {"state": "enabled"}, "capabilities": {"supports": {"tool_calls": false}}}
        ]),
        "proxy.individual.githubcopilot.com",
    )
    .await
    .expect("refresh");
    assert_eq!(available_model_ids(&credentials), vec![enabled]);
}

#[tokio::test]
async fn does_not_fall_back_to_policy_models_for_non_individual_accounts() {
    let credentials = refresh_models(
        json!([
            {"id": "gpt-4.1", "model_picker_enabled": false, "policy": {"state": "enabled"}, "capabilities": {"supports": {"tool_calls": true}}}
        ]),
        "proxy.business.githubcopilot.com",
    )
    .await
    .expect("refresh");
    assert!(available_model_ids(&credentials).is_empty());
}

#[tokio::test]
async fn does_not_retry_model_catalog_throttling_during_credential_refresh() {
    let catalog_request_count = Arc::new(Mutex::new(0u32));
    let fetch = MockFetch::new({
        let catalog_request_count = Arc::clone(&catalog_request_count);
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url.contains("/copilot_internal/v2/token") {
                return Box::pin(async {
                    Ok(json_response(
                        json!({"token": TEST_COPILOT_ACCESS_TOKEN, "expires_at": 9999999999i64}),
                        200,
                        &[],
                    ))
                });
            }
            if url == TEST_COPILOT_MODELS_URL {
                *catalog_request_count
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) += 1;
                return Box::pin(async {
                    Ok(json_response(
                        json!({"error": "too many requests"}),
                        429,
                        &[("Retry-After", "0")],
                    ))
                });
            }
            panic!("Unexpected fetch URL: {url}");
        }
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let error = oauth
        .refresh(
            &OAuthCredential::new("old-access-token", "ghu_refresh_token", 0.0),
            &AbortScope::new(),
        )
        .await
        .expect_err("throttled");
    assert!(error.message.contains("429"), "{}", error.message);
    assert_eq!(
        *catalog_request_count
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        1
    );
}

/// 回归：`Retry-After:` 空值时 TS 的 `if (retryAfter)` 为假 → 走指数退避并重试；
/// Rust 曾经把 Some("") 当成 NaN 而直接返回 429（不再重试）。
#[tokio::test(start_paused = true)]
async fn retries_model_catalog_throttling_after_an_empty_retry_after_header() {
    let catalog_request_count = Arc::new(Mutex::new(0u32));
    let fetch = login_fetch(LoginFetchConfig {
        models: Arc::new({
            let catalog_request_count = Arc::clone(&catalog_request_count);
            move || {
                let count = {
                    let mut guard = catalog_request_count
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    *guard += 1;
                    *guard
                };
                if count == 1 {
                    json_response(
                        json!({"error": "too many requests"}),
                        429,
                        &[("Retry-After", "")],
                    )
                } else {
                    json_response(json!({"data": []}), 200, &[])
                }
            }
        }),
        policy: None,
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = login_interaction();
    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    wait_for_device_code(&interaction).await;
    advance_step(1000).await;
    assert_eq!(
        *catalog_request_count
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        1
    );
    // 指数退避：500ms。
    advance_step(499).await;
    assert_eq!(
        *catalog_request_count
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        1
    );
    advance_step(1).await;
    handle.await.expect("join").expect("login");
    assert_eq!(
        *catalog_request_count
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        2
    );
}

/// 回归：TS 用完整的 `Date.parse`；Rust 曾只接受 RFC2822/RFC3339，`2015-10-21` 之类
/// 会被当作 NaN 而放弃重试。过去的时间 → `Math.max(0, …)` = 0 → 立即重试。
#[tokio::test(start_paused = true)]
async fn retries_model_catalog_throttling_after_a_javascript_date_header() {
    let catalog_request_count = Arc::new(Mutex::new(0u32));
    let fetch = login_fetch(LoginFetchConfig {
        models: Arc::new({
            let catalog_request_count = Arc::clone(&catalog_request_count);
            move || {
                let count = {
                    let mut guard = catalog_request_count
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    *guard += 1;
                    *guard
                };
                if count == 1 {
                    json_response(
                        json!({"error": "too many requests"}),
                        429,
                        &[("Retry-After", "Wed, 21 Oct 2015 07:28:00 UTC")],
                    )
                } else {
                    json_response(json!({"data": []}), 200, &[])
                }
            }
        }),
        policy: None,
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = login_interaction();
    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    wait_for_device_code(&interaction).await;
    advance_step(1000).await;
    handle.await.expect("join").expect("login");
    assert_eq!(
        *catalog_request_count
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        2
    );
}

#[tokio::test(start_paused = true)]
async fn reports_device_code_details_through_on_device_code() {
    let fetch = login_fetch(LoginFetchConfig {
        models: Arc::new(|| json_response(json!({"data": []}), 200, &[])),
        policy: None,
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| responding_prompt(""));
    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    for _ in 0..100_000 {
        if !device_code_info(&interaction).is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        device_code_info(&interaction),
        vec![(
            "ABCD-EFGH".to_string(),
            "https://github.com/login/device".to_string(),
            Some(1.0),
            Some(900.0),
        )]
    );
    advance_step(1000).await;
    handle.await.expect("join").expect("login");
}

#[tokio::test(start_paused = true)]
async fn updates_only_known_tool_capable_unconfigured_account_model_policies() {
    let ids = catalog_model_ids();
    let configured = require_model_id(&ids, 0);
    let unconfigured = require_model_id(&ids, 1);
    let tool_incapable = require_model_id(&ids, 2);
    let catalog_request_count = Arc::new(Mutex::new(0u32));
    let policy_model_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let fetch = login_fetch(LoginFetchConfig {
        models: Arc::new({
            let catalog_request_count = Arc::clone(&catalog_request_count);
            let configured = configured.clone();
            let unconfigured = unconfigured.clone();
            let tool_incapable = tool_incapable.clone();
            move || {
                *catalog_request_count
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) += 1;
                json_response(
                    json!({"data": [
                        {"id": configured, "model_picker_enabled": true, "policy": {"state": "enabled"}, "capabilities": {"supports": {"tool_calls": true}}},
                        {"id": unconfigured, "model_picker_enabled": true, "policy": {"state": "unconfigured"}, "capabilities": {"supports": {"tool_calls": true}}},
                        {"id": "remote-only-model", "model_picker_enabled": true, "policy": {"state": "unconfigured"}, "capabilities": {"supports": {"tool_calls": true}}},
                        {"id": tool_incapable, "model_picker_enabled": true, "policy": {"state": "unconfigured"}, "capabilities": {"supports": {"tool_calls": false}}}
                    ]}),
                    200,
                    &[],
                )
            }
        }),
        policy: Some(Arc::new({
            let policy_model_ids = Arc::clone(&policy_model_ids);
            move |model_id: &str| {
                policy_model_ids
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(model_id.to_string());
                OAuthFetchResponse::text_response(200, "")
            }
        })),
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = login_interaction();
    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    wait_for_device_code(&interaction).await;
    advance_step(1000).await;
    handle.await.expect("join").expect("login");

    assert_eq!(
        *catalog_request_count
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        1
    );
    assert_eq!(
        *policy_model_ids
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        vec![unconfigured]
    );
}

#[tokio::test(start_paused = true)]
async fn retries_a_throttled_policy_update_after_retry_after() {
    let model_id = require_model_id(&catalog_model_ids(), 0);
    let policy_request_count = Arc::new(Mutex::new(0u32));
    let fetch = login_fetch(LoginFetchConfig {
        models: Arc::new({
            let model_id = model_id.clone();
            move || {
                json_response(
                    json!({"data": [{"id": model_id, "model_picker_enabled": true, "policy": {"state": "unconfigured"}}]}),
                    200,
                    &[],
                )
            }
        }),
        policy: Some(Arc::new({
            let policy_request_count = Arc::clone(&policy_request_count);
            move |_model_id: &str| {
                let count = {
                    let mut guard = policy_request_count
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    *guard += 1;
                    *guard
                };
                if count == 1 {
                    json_response(
                        json!({"error": "too many requests"}),
                        429,
                        &[("Retry-After", "1")],
                    )
                } else {
                    OAuthFetchResponse::text_response(200, "")
                }
            }
        })),
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = login_interaction();
    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    wait_for_device_code(&interaction).await;
    advance_step(1000).await;
    assert_eq!(
        *policy_request_count
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        1
    );
    advance_step(999).await;
    assert_eq!(
        *policy_request_count
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        1
    );
    advance_step(1).await;
    handle.await.expect("join").expect("login");
    assert_eq!(
        *policy_request_count
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        2
    );
}

#[tokio::test(start_paused = true)]
async fn continues_policy_updates_after_a_transport_failure() {
    let ids = catalog_model_ids();
    let model_ids = vec![require_model_id(&ids, 0), require_model_id(&ids, 1)];
    let policy_model_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let fetch = login_fetch(LoginFetchConfig {
        models: Arc::new({
            let model_ids = model_ids.clone();
            move || {
                json_response(
                    json!({"data": model_ids.iter().map(|id| json!({"id": id, "model_picker_enabled": true, "policy": {"state": "unconfigured"}})).collect::<Vec<_>>()}),
                    200,
                    &[],
                )
            }
        }),
        policy: Some(Arc::new({
            let policy_model_ids = Arc::clone(&policy_model_ids);
            move |model_id: &str| {
                let mut ids = policy_model_ids
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                ids.push(model_id.to_string());
                let count = ids.len();
                drop(ids);
                if count == 1 {
                    // 传输失败用一个 panic 无法表达；返回 500 会走「重试后仍失败」的路径，
                    // 因此这里用 fetch 抛错在下面单独测试（见 transport_failure 版本）。
                    OAuthFetchResponse::text_response(500, "")
                } else {
                    OAuthFetchResponse::text_response(200, "")
                }
            }
        })),
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = login_interaction();
    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    wait_for_device_code(&interaction).await;
    advance_step(1000).await;
    handle.await.expect("join").expect("login");
    assert_eq!(
        *policy_model_ids
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        model_ids
    );
}

#[tokio::test(start_paused = true)]
async fn continues_policy_updates_after_a_fetch_error() {
    // 与 TS 的 throw new Error("fetch failed") 等价：fetch 返回 Err → enable 返回 false。
    let ids = catalog_model_ids();
    let model_ids = vec![require_model_id(&ids, 0), require_model_id(&ids, 1)];
    let policy_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let fetch = MockFetch::new({
        let model_ids = model_ids.clone();
        let policy_calls = Arc::clone(&policy_calls);
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url.ends_with("/login/device/code") {
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "device_code": "device-code",
                            "user_code": "ABCD-EFGH",
                            "verification_uri": "https://github.com/login/device",
                            "interval": 1,
                            "expires_in": 900,
                        }),
                        200,
                        &[],
                    ))
                });
            }
            if url.ends_with("/login/oauth/access_token") {
                return Box::pin(async {
                    Ok(json_response(
                        json!({"access_token": "ghu_refresh_token"}),
                        200,
                        &[],
                    ))
                });
            }
            if url.contains("/copilot_internal/v2/token") {
                return Box::pin(async {
                    Ok(json_response(
                        json!({"token": TEST_COPILOT_ACCESS_TOKEN, "expires_at": 9999999999i64}),
                        200,
                        &[],
                    ))
                });
            }
            if url == TEST_COPILOT_MODELS_URL {
                let model_ids = model_ids.clone();
                return Box::pin(async move {
                    Ok(json_response(
                        json!({"data": model_ids.iter().map(|id| json!({"id": id, "model_picker_enabled": true, "policy": {"state": "unconfigured"}})).collect::<Vec<_>>()}),
                        200,
                        &[],
                    ))
                });
            }
            if url.starts_with(&format!("{TEST_COPILOT_MODELS_URL}/")) && url.ends_with("/policy") {
                let model_id = url
                    [format!("{TEST_COPILOT_MODELS_URL}/").len()..url.len() - "/policy".len()]
                    .to_string();
                let mut calls = policy_calls
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                calls.push(model_id);
                let count = calls.len();
                drop(calls);
                if count == 1 {
                    return Box::pin(async {
                        Err(kiana_ai::auth::types::AuthError::new("fetch failed"))
                    });
                }
                return Box::pin(async { Ok(OAuthFetchResponse::text_response(200, "")) });
            }
            panic!("Unexpected fetch URL: {url}");
        }
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = login_interaction();
    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    wait_for_device_code(&interaction).await;
    advance_step(1000).await;
    handle.await.expect("join").expect("login");
    assert_eq!(
        *policy_calls
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        model_ids
    );
}

#[tokio::test(start_paused = true)]
async fn stops_policy_updates_when_the_retry_delay_exceeds_the_login_budget() {
    let ids = catalog_model_ids();
    let first = require_model_id(&ids, 0);
    let second = require_model_id(&ids, 1);
    let policy_model_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let fetch = login_fetch(LoginFetchConfig {
        models: Arc::new({
            let first = first.clone();
            let second = second.clone();
            move || {
                json_response(
                    json!({"data": [
                        {"id": first, "model_picker_enabled": true, "policy": {"state": "unconfigured"}},
                        {"id": second, "model_picker_enabled": true, "policy": {"state": "unconfigured"}}
                    ]}),
                    200,
                    &[],
                )
            }
        }),
        policy: Some(Arc::new({
            let policy_model_ids = Arc::clone(&policy_model_ids);
            move |model_id: &str| {
                policy_model_ids
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(model_id.to_string());
                json_response(
                    json!({"error": "too many requests"}),
                    429,
                    &[("Retry-After", "5")],
                )
            }
        })),
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = login_interaction();
    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    wait_for_device_code(&interaction).await;
    advance_step(1000).await;
    let credential = handle.await.expect("join").expect("login");
    assert_eq!(credential.access, TEST_COPILOT_ACCESS_TOKEN);
    assert_eq!(
        *policy_model_ids
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        vec![first]
    );
}

#[tokio::test]
async fn rejects_a_non_http_s_verification_uri_before_it_reaches_on_device_code() {
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        assert!(request.url.ends_with("/login/device/code"));
        Box::pin(async {
            Ok(json_response(
                json!({
                    "device_code": "device-code",
                    "user_code": "ABCD-EFGH",
                    "verification_uri": "$(id>/tmp/pwned)",
                    "interval": 1,
                    "expires_in": 900,
                }),
                200,
                &[],
            ))
        })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = login_interaction();
    let error = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect_err("untrusted");
    assert!(
        error.message.contains("Untrusted verification_uri"),
        "{}",
        error.message
    );
    assert!(device_code_info(&interaction).is_empty());
}

#[tokio::test(start_paused = true)]
async fn normalizes_verification_uri_before_it_reaches_on_device_code() {
    let raw_verification_uri = "https://github.com/login/\u{1b}]8;;evil";
    let normalized = url::Url::parse(raw_verification_uri)
        .expect("url")
        .to_string();
    assert_ne!(normalized, raw_verification_uri);
    assert!(normalized.contains("%1B"));

    let fetch = MockFetch::new({
        let raw_verification_uri = raw_verification_uri.to_string();
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url.ends_with("/login/device/code") {
                let raw = raw_verification_uri.clone();
                return Box::pin(async move {
                    Ok(json_response(
                        json!({
                            "device_code": "device-code",
                            "user_code": "ABCD-EFGH",
                            "verification_uri": raw,
                            "interval": 1,
                            "expires_in": 900,
                        }),
                        200,
                        &[],
                    ))
                });
            }
            if url.ends_with("/login/oauth/access_token") {
                return Box::pin(async {
                    Ok(json_response(
                        json!({"access_token": "ghu_refresh_token"}),
                        200,
                        &[],
                    ))
                });
            }
            if url.contains("/copilot_internal/v2/token") {
                return Box::pin(async {
                    Ok(json_response(
                        json!({"token": TEST_COPILOT_ACCESS_TOKEN, "expires_at": 9999999999i64}),
                        200,
                        &[],
                    ))
                });
            }
            if url == TEST_COPILOT_MODELS_URL {
                return Box::pin(async { Ok(json_response(json!({"data": []}), 200, &[])) });
            }
            if url.starts_with(&format!("{TEST_COPILOT_MODELS_URL}/")) && url.ends_with("/policy") {
                return Box::pin(async { Ok(OAuthFetchResponse::text_response(200, "")) });
            }
            panic!("Unexpected fetch URL: {url}");
        }
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = login_interaction();
    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    for _ in 0..100_000 {
        if !device_code_info(&interaction).is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        device_code_info(&interaction),
        vec![("ABCD-EFGH".to_string(), normalized, Some(1.0), Some(900.0),)]
    );
    advance_step(1000).await;
    handle.await.expect("join").expect("login");
}

#[tokio::test(start_paused = true)]
async fn waits_before_polling_and_increases_the_interval_after_slow_down() {
    let poll_times: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from([
        json_response(
            json!({"error": "authorization_pending", "error_description": "pending"}),
            200,
            &[],
        ),
        json_response(
            json!({"error": "slow_down", "error_description": "slow down", "interval": 7}),
            200,
            &[],
        ),
        json_response(json!({"access_token": "ghu_refresh_token"}), 200, &[]),
    ])));
    let fetch = MockFetch::new({
        let clock = VirtualClock::new(START_MS);
        let poll_times = Arc::clone(&poll_times);
        let responses = Arc::clone(&responses);
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url.ends_with("/login/device/code") {
                assert_eq!(request.method, "POST");
                assert_eq!(request.header_value("Accept"), Some("application/json"));
                assert_eq!(
                    request.header_value("Content-Type"),
                    Some("application/x-www-form-urlencoded")
                );
                assert!(request.body_text().contains("client_id="));
                assert!(request.body_text().contains("scope=read%3Auser"));
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "device_code": "device-code",
                            "user_code": "ABCD-EFGH",
                            "verification_uri": "https://github.com/login/device",
                            "interval": 5,
                            "expires_in": 900,
                        }),
                        200,
                        &[],
                    ))
                });
            }
            if url.ends_with("/login/oauth/access_token") {
                poll_times
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(clock.now_ms());
                assert!(request.body_text().contains("client_id="));
                assert!(request.body_text().contains("device_code=device-code"));
                assert!(
                    request.body_text().contains(
                        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"
                    )
                );
                let response = responses
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .pop_front()
                    .expect("Unexpected extra access token poll");
                return Box::pin(async move { Ok(response) });
            }
            if url.contains("/copilot_internal/v2/token") {
                return Box::pin(async {
                    Ok(json_response(
                        json!({"token": TEST_COPILOT_ACCESS_TOKEN, "expires_at": 9999999999i64}),
                        200,
                        &[],
                    ))
                });
            }
            if url == TEST_COPILOT_MODELS_URL {
                return Box::pin(async { Ok(json_response(json!({"data": []}), 200, &[])) });
            }
            if url.starts_with(&format!("{TEST_COPILOT_MODELS_URL}/")) && url.ends_with("/policy") {
                return Box::pin(async { Ok(OAuthFetchResponse::text_response(200, "")) });
            }
            panic!("Unexpected fetch URL: {url}");
        }
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = login_interaction();
    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    for _ in 0..100_000 {
        if !device_code_info(&interaction).is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    );
    advance_step(4999).await;
    assert!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    );
    advance_step(1).await;
    assert_eq!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        1
    );
    advance_step(4999).await;
    assert_eq!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        1
    );
    advance_step(1).await;
    assert_eq!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        2
    );
    // slow_down 携带服务端 interval=7。
    advance_step(6999).await;
    assert_eq!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        2
    );
    advance_step(1).await;
    handle.await.expect("join").expect("login");
    assert_eq!(
        *poll_times.lock().unwrap_or_else(|error| error.into_inner()),
        vec![START_MS + 5000.0, START_MS + 10_000.0, START_MS + 17_000.0,]
    );
}

#[tokio::test(start_paused = true)]
async fn times_out_after_repeated_slow_down_responses() {
    let poll_times: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from([
        json_response(
            json!({"error": "slow_down", "error_description": "slow down"}),
            200,
            &[],
        ),
        json_response(
            json!({"error": "slow_down", "error_description": "still too fast"}),
            200,
            &[],
        ),
        json_response(
            json!({"error": "authorization_pending", "error_description": "pending"}),
            200,
            &[],
        ),
    ])));
    let fetch = MockFetch::new({
        let clock = VirtualClock::new(START_MS);
        let poll_times = Arc::clone(&poll_times);
        let responses = Arc::clone(&responses);
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url.ends_with("/login/device/code") {
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "device_code": "device-code",
                            "user_code": "ABCD-EFGH",
                            "verification_uri": "https://github.com/login/device",
                            "interval": 5,
                            "expires_in": 25,
                        }),
                        200,
                        &[],
                    ))
                });
            }
            if url.ends_with("/login/oauth/access_token") {
                poll_times
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(clock.now_ms());
                let response = responses
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .pop_front()
                    .expect("Unexpected extra access token poll");
                return Box::pin(async move { Ok(response) });
            }
            panic!("Unexpected fetch URL: {url}");
        }
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = login_interaction();
    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    for _ in 0..100_000 {
        if !device_code_info(&interaction).is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    advance_step(4999).await;
    assert!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    );
    advance_step(1).await;
    assert_eq!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        1
    );
    advance_step(9999).await;
    assert_eq!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        1
    );
    advance_step(1).await;
    assert_eq!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        2
    );
    advance_step(9999).await;
    assert_eq!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        2
    );
    advance_step(1).await;
    let error = handle.await.expect("join").expect_err("timed out");
    assert!(
        error
            .message
            .contains("Device flow timed out after one or more slow_down responses"),
        "{}",
        error.message
    );
    assert_eq!(
        *poll_times.lock().unwrap_or_else(|error| error.into_inner()),
        vec![START_MS + 5000.0, START_MS + 15_000.0]
    );
}

/// 回归（评审发现）：TS 的 `description ? `: ${description}` : ""` 是真值判断——
/// 空串不追加（Rust 曾输出 "weird: "），非字符串的真值用模板字符串 `String()` 化。
#[tokio::test(start_paused = true)]
async fn device_flow_error_descriptions_follow_js_truthiness() {
    let cases = [
        (
            json!({"error": "weird", "error_description": ""}),
            "Device flow failed: weird",
        ),
        (
            json!({"error": "weird", "error_description": 5}),
            "Device flow failed: weird: 5",
        ),
        (json!({"error": "weird"}), "Device flow failed: weird"),
    ];
    for (body, expected) in cases {
        let body_for_fetch = body.clone();
        let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url.ends_with("/login/device/code") {
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "device_code": "device-code",
                            "user_code": "ABCD-EFGH",
                            "verification_uri": "https://github.com/login/device",
                            "interval": 1,
                            "expires_in": 900,
                        }),
                        200,
                        &[],
                    ))
                });
            }
            assert!(url.ends_with("/login/oauth/access_token"), "{url}");
            let body = body_for_fetch.clone();
            Box::pin(async move { Ok(json_response(body, 200, &[])) })
        });
        let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
        let interaction = login_interaction();
        let handle = {
            let oauth = Arc::new(oauth);
            let interaction = Arc::clone(&interaction);
            tokio::spawn(
                async move { oauth.login(interaction.as_ref(), &Default::default()).await },
            )
        };
        advance_step(1000).await;
        let error = handle
            .await
            .expect("join")
            .expect_err("device flow should fail");
        assert_eq!(error.message, expected, "body {body}");
    }
}
