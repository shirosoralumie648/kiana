//! HTTP hook 的行为测试：loopback 允许、SSRF、重定向重新校验、跨源剥离凭据、
//! URL 白名单、header 注入清洗。

use std::path::Path;
use std::time::Duration;

use kiana_hooks::{
    HookAuditOutcome, HookDecision, HookEvent, HookInput, HookRegistry, HookRunner,
    HookRunnerConfig, HookSource, HooksConfig, HttpHookPolicy,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn http_runner(
    project_root: &Path,
    url: &str,
    headers: Value,
    policy: HttpHookPolicy,
) -> HookRunner {
    let mut registry = HookRegistry::new();
    let load = HooksConfig::from_value(&json!({
        "PreToolUse": [{
            "hooks": [{
                "type": "http",
                "url": url,
                "headers": headers,
                "allowedEnvVars": []
            }]
        }]
    }));
    assert!(load.diagnostics.is_empty(), "{:?}", load.diagnostics);
    registry.add_source(HookSource::User, load, true);

    let mut config = HookRunnerConfig::new(project_root);
    config.http_policy = policy;
    HookRunner::new(registry, config).expect("runner builds")
}

fn pre_tool_input(root: &Path) -> HookInput {
    HookInput::new(
        HookEvent::PreToolUse,
        "session-http",
        root.to_string_lossy(),
    )
    .with_tool_call("bash", json!({"command": "ls"}))
}

#[tokio::test]
async fn loopback_http_hook_is_allowed_and_json_output_applies() {
    let temp = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"systemMessage":"remote says hi","hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"remote policy"}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;

    let runner = http_runner(
        temp.path(),
        &format!("{}/hook", server.uri()),
        json!({ "X-Test": "a\r\nX-Evil: 1" }),
        HttpHookPolicy::default(),
    );
    let outcome = runner
        .run_event(&pre_tool_input(temp.path()))
        .await
        .unwrap();

    assert_eq!(outcome.decision, HookDecision::Deny);
    assert_eq!(outcome.reason.as_deref(), Some("remote policy"));
    assert_eq!(outcome.system_messages, vec!["remote says hi"]);
    assert_eq!(outcome.runs[0].outcome, HookAuditOutcome::Success);
    assert_eq!(outcome.runs[0].http_status, Some(200));

    let requests = server.received_requests().await.expect("requests recorded");
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["hook_event_name"], "PreToolUse");
    assert_eq!(body["tool_name"], "bash");
    assert_eq!(body["cwd"], temp.path().to_string_lossy().as_ref());
    // CRLF 注入被清洗，不能多出 X-Evil header。
    assert_eq!(
        requests[0]
            .headers
            .get("x-test")
            .and_then(|value| value.to_str().ok()),
        Some("aX-Evil: 1")
    );
    assert!(requests[0].headers.get("x-evil").is_none());
}

#[tokio::test]
async fn cross_origin_redirect_strips_all_configured_headers() {
    let temp = TempDir::new().unwrap();
    let target = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/target"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{}", "application/json"))
        .mount(&target)
        .await;

    let origin = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/start"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/target", target.uri())),
        )
        .mount(&origin)
        .await;

    let runner = http_runner(
        temp.path(),
        &format!("{}/start", origin.uri()),
        json!({
            "Authorization": "Bearer secret",
            "Cookie": "session=abc",
            "X-Keep": "yes"
        }),
        HttpHookPolicy::default(),
    );
    let outcome = runner
        .run_event(&pre_tool_input(temp.path()))
        .await
        .unwrap();
    assert_eq!(outcome.decision, HookDecision::Passthrough);
    assert_eq!(outcome.runs[0].http_status, Some(200));
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("stripped configured headers")),
        "{:?}",
        outcome.warnings
    );

    let origin_requests = origin.received_requests().await.expect("origin requests");
    assert_eq!(origin_requests.len(), 1);
    assert_eq!(
        origin_requests[0]
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer secret")
    );

    let target_requests = target.received_requests().await.expect("target requests");
    assert_eq!(target_requests.len(), 1);
    assert_eq!(target_requests[0].method.as_str(), "GET");
    assert!(target_requests[0].headers.get("authorization").is_none());
    assert!(target_requests[0].headers.get("cookie").is_none());
    assert!(
        target_requests[0].headers.get("x-keep").is_none(),
        "自定义 header 也可能携带凭据，跨源重定向必须全部丢弃"
    );
    // Content-Type 不是凭据，GET 跳转后也不再需要；无论如何不能带回原站凭据。
    assert_eq!(
        target_requests[0]
            .headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        None
    );
}

#[tokio::test]
async fn redirect_to_cloud_metadata_is_rejected() {
    let temp = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/start"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", "http://169.254.169.254/latest/meta-data"),
        )
        .mount(&server)
        .await;

    let runner = http_runner(
        temp.path(),
        &format!("{}/start", server.uri()),
        json!({}),
        HttpHookPolicy::default(),
    );
    let outcome = runner
        .run_event(&pre_tool_input(temp.path()))
        .await
        .unwrap();
    assert_eq!(outcome.decision, HookDecision::Passthrough);
    assert_eq!(outcome.runs[0].outcome, HookAuditOutcome::SpawnError);
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("redirect target rejected")),
        "{:?}",
        outcome.warnings
    );
}

#[tokio::test]
async fn url_allowlist_blocks_non_matching_hosts() {
    let temp = TempDir::new().unwrap();
    let allowed = MockServer::start().await;
    let blocked = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{}", "application/json"))
        .mount(&allowed)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{}", "application/json"))
        .mount(&blocked)
        .await;

    let runner = http_runner(
        temp.path(),
        &format!("{}/hook", blocked.uri()),
        json!({}),
        HttpHookPolicy {
            allowed_url_patterns: vec![format!("{}*", allowed.uri())],
            allowed_env_vars: None,
        },
    );
    let outcome = runner
        .run_event(&pre_tool_input(temp.path()))
        .await
        .unwrap();
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("does not match the allowed URL patterns")),
        "{:?}",
        outcome.warnings
    );
    assert_eq!(
        blocked.received_requests().await.expect("requests").len(),
        0
    );
}

#[tokio::test]
async fn non_success_status_is_a_warning_only() {
    let temp = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("nope"))
        .mount(&server)
        .await;

    let runner = http_runner(
        temp.path(),
        &format!("{}/hook", server.uri()),
        json!({}),
        HttpHookPolicy::default(),
    );
    let outcome = runner
        .run_event(&pre_tool_input(temp.path()))
        .await
        .unwrap();
    assert_eq!(outcome.decision, HookDecision::Passthrough);
    assert_eq!(outcome.runs[0].outcome, HookAuditOutcome::Warning);
    assert_eq!(outcome.runs[0].http_status, Some(500));
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("non-success status 500")),
        "{:?}",
        outcome.warnings
    );
}

#[tokio::test]
async fn http_hook_timeout_is_reported_and_does_not_block() {
    let temp = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(2))
                .set_body_raw("{}", "application/json"),
        )
        .mount(&server)
        .await;

    let mut registry = HookRegistry::new();
    let load = HooksConfig::from_value(&json!({
        "PreToolUse": [{
            "hooks": [{
                "type": "http",
                "url": format!("{}/hook", server.uri()),
                "timeout": 0.2
            }]
        }]
    }));
    assert!(load.diagnostics.is_empty(), "{:?}", load.diagnostics);
    registry.add_source(HookSource::User, load, true);
    let runner = HookRunner::new(registry, HookRunnerConfig::new(temp.path())).unwrap();

    let outcome = runner
        .run_event(&pre_tool_input(temp.path()))
        .await
        .unwrap();
    assert_eq!(outcome.decision, HookDecision::Passthrough);
    assert_eq!(outcome.runs[0].outcome, HookAuditOutcome::Timeout);
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("timed out")),
        "{:?}",
        outcome.warnings
    );
}

#[tokio::test]
async fn plain_text_http_body_is_rejected() {
    let temp = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;

    let runner = http_runner(
        temp.path(),
        &format!("{}/hook", server.uri()),
        json!({}),
        HttpHookPolicy::default(),
    );
    let outcome = runner
        .run_event(&pre_tool_input(temp.path()))
        .await
        .unwrap();
    assert_eq!(outcome.runs[0].outcome, HookAuditOutcome::InvalidOutput);
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("must return JSON")),
        "{:?}",
        outcome.warnings
    );
}
