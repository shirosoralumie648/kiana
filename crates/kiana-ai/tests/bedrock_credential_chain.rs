//! AWS 默认凭据链（`@aws-sdk/credential-provider-node`）的回归测试。
//!
//! 评审发现：Rust 侧只读显式 profile 的静态凭据，`default` profile、config 文件里的静态键、
//! `credential_process`、assume-role（source_profile/credential_source）、web identity、
//! ECS 容器凭据、IMDS 与最终错误全部缺失。这里用真实 `stream()` + wiremock 覆盖每条 provider：
//! 断言最终 Bedrock 请求的 `Authorization`（SigV4 凭据/区域）以及各 provider 的线上请求。
//!
//! 进程环境是全局状态：本文件所有用例共享一把锁串行执行，结束时恢复原值。

use std::sync::{Mutex, MutexGuard};

use kiana_ai::api::bedrock_converse_stream::{BedrockOptions, stream};
use kiana_ai::types::{
    Context, Message, Model, ModelInputModality, StopReason, StreamOptions, UserContent,
    UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<String>)>,
}

const ENV_NAMES: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "AWS_SHARED_CREDENTIALS_FILE",
    "AWS_CONFIG_FILE",
    "AWS_ENDPOINT_URL_STS",
    "AWS_STS_REGIONAL_ENDPOINTS",
    "AWS_EC2_METADATA_DISABLED",
    "AWS_EC2_METADATA_SERVICE_ENDPOINT",
    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
    "AWS_CONTAINER_CREDENTIALS_FULL_URI",
    "AWS_CONTAINER_AUTHORIZATION_TOKEN",
    "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
    "AWS_ROLE_ARN",
    "AWS_WEB_IDENTITY_TOKEN_FILE",
    "AWS_ROLE_SESSION_NAME",
    "AWS_BEDROCK_SKIP_AUTH",
    "AWS_BEARER_TOKEN_BEDROCK",
];

impl EnvGuard {
    fn new() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let mut saved = Vec::new();
        for name in ENV_NAMES {
            saved.push((*name, std::env::var(name).ok()));
            // SAFETY: 本文件所有用例都在 ENV_LOCK 内读写环境变量。
            unsafe { std::env::remove_var(name) };
        }
        EnvGuard { _lock: lock, saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..) {
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }
}

struct TempFile(std::path::PathBuf);

impl TempFile {
    fn new(name: &str, contents: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "kiana_ai_bedrock_creds_{}_{}_{}",
            std::process::id(),
            unique,
            name
        ));
        std::fs::write(&path, contents).expect("write temp file");
        TempFile(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().expect("utf-8 temp path")
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn model(base_url: &str) -> Model {
    Model {
        id: "us.anthropic.claude-opus-4-8".into(),
        name: "Claude Opus 4.8".into(),
        api: "bedrock-converse-stream".into(),
        provider: "amazon-bedrock".into(),
        base_url: base_url.into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        context_window: Some(200_000),
        max_tokens: Some(64_000),
        ..Model::default()
    }
}

fn context() -> kiana_ai::types::TranscriptContext {
    normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    })
}

async fn run_stream(server: &MockServer) -> kiana_ai::types::AssistantMessage {
    stream(
        &model(&server.uri()),
        &context(),
        BedrockOptions {
            base: StreamOptions {
                cache_retention: Some(kiana_ai::types::CacheRetention::None),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result")
}

async fn mount_converse_stream(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path_regex("^/model/.*/converse-stream$"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/vnd.amazon.eventstream")
                .set_body_bytes(Vec::new()),
        )
        .mount(server)
        .await;
}

async fn mount_assume_role(server: &MockServer, response: &str) {
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(response))
        .mount(server)
        .await;
}

fn assume_role_xml(access_key_id: &str, session_token: &str) -> String {
    format!(
        "<AssumeRoleResponse><AssumeRoleResult><Credentials>\
         <AccessKeyId>{access_key_id}</AccessKeyId>\
         <SecretAccessKey>assumed-secret</SecretAccessKey>\
         <SessionToken>{session_token}</SessionToken>\
         <Expiration>2030-01-01T00:00:00Z</Expiration>\
         </Credentials></AssumeRoleResult></AssumeRoleResponse>"
    )
}

fn header(request: &wiremock::Request, name: &str) -> Option<String> {
    request
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn request_body(request: &wiremock::Request) -> String {
    String::from_utf8_lossy(&request.body).into_owned()
}

fn stream_request(requests: &[wiremock::Request]) -> &wiremock::Request {
    requests
        .iter()
        .find(|request| request.url.path().contains("converse-stream"))
        .expect("bedrock request")
}

fn authorization(request: &wiremock::Request) -> String {
    header(request, "authorization").expect("authorization header")
}

#[tokio::test]
async fn resolves_static_credentials_from_the_default_profile() {
    let _env = EnvGuard::new();
    let credentials = TempFile::new(
        "credentials",
        "[default]\naws_access_key_id = AKIADEFAULTPROFILE\naws_secret_access_key = default-secret\n",
    );
    let config = TempFile::new("config", "");
    unsafe {
        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", credentials.path());
        std::env::set_var("AWS_CONFIG_FILE", config.path());
    }

    let server = MockServer::start().await;
    mount_converse_stream(&server).await;
    let message = run_stream(&server).await;
    assert_eq!(message.stop_reason, StopReason::Error);

    let requests = server.received_requests().await.expect("requests");
    let auth = authorization(stream_request(&requests));
    assert!(
        auth.contains("Credential=AKIADEFAULTPROFILE/"),
        "default profile credentials were not used: {auth}"
    );
}

#[tokio::test]
async fn reads_credentials_and_region_from_the_selected_profile() {
    let _env = EnvGuard::new();
    let credentials = TempFile::new(
        "credentials",
        "[profiled]\naws_access_key_id = AKIAPROFILED\naws_secret_access_key = profile-secret\n",
    );
    let config = TempFile::new("config", "[profile profiled]\nregion = us-west-2\n");
    unsafe {
        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", credentials.path());
        std::env::set_var("AWS_CONFIG_FILE", config.path());
        std::env::set_var("AWS_PROFILE", "profiled");
    }

    let server = MockServer::start().await;
    mount_converse_stream(&server).await;
    let message = run_stream(&server).await;
    assert_eq!(message.stop_reason, StopReason::Error);

    let requests = server.received_requests().await.expect("requests");
    let auth = authorization(stream_request(&requests));
    assert!(
        auth.contains("Credential=AKIAPROFILED/"),
        "profile credentials were not used: {auth}"
    );
    assert!(
        auth.contains("/us-west-2/bedrock/aws4_request"),
        "region should come from the selected profile: {auth}"
    );
}

#[tokio::test]
async fn assumes_a_role_using_source_profile_credentials() {
    let _env = EnvGuard::new();
    let credentials = TempFile::new(
        "credentials",
        "[base]\naws_access_key_id = AKIABASE\naws_secret_access_key = base-secret\n",
    );
    let config = TempFile::new(
        "config",
        "[profile role]\nrole_arn = arn:aws:iam::123456789012:role/test\nsource_profile = base\nregion = us-west-2\n",
    );
    let server = MockServer::start().await;
    mount_converse_stream(&server).await;
    mount_assume_role(&server, &assume_role_xml("AKIAROLE", "role-session-token")).await;
    unsafe {
        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", credentials.path());
        std::env::set_var("AWS_CONFIG_FILE", config.path());
        std::env::set_var("AWS_PROFILE", "role");
        std::env::set_var("AWS_ENDPOINT_URL_STS", server.uri());
    }

    let message = run_stream(&server).await;
    assert_eq!(message.stop_reason, StopReason::Error);

    let requests = server.received_requests().await.expect("requests");
    let sts = requests
        .iter()
        .find(|request| request.url.path() == "/")
        .expect("sts request");
    let body = request_body(sts);
    assert!(body.contains("Action=AssumeRole"), "{body}");
    assert!(
        body.contains("RoleArn=arn%3Aaws%3Aiam%3A%3A123456789012%3Arole%2Ftest"),
        "{body}"
    );
    assert!(body.contains("RoleSessionName=aws-sdk-js-"), "{body}");
    assert!(
        authorization(sts).contains("Credential=AKIABASE/"),
        "STS request must be signed with the source profile credentials"
    );

    let auth = authorization(stream_request(&requests));
    assert!(
        auth.contains("Credential=AKIAROLE/"),
        "assumed-role credentials were not used: {auth}"
    );
    assert_eq!(
        header(stream_request(&requests), "x-amz-security-token").as_deref(),
        Some("role-session-token")
    );
}

#[tokio::test]
async fn assumes_a_role_with_a_web_identity_token_from_the_profile() {
    let _env = EnvGuard::new();
    let token = TempFile::new("token", "web-identity-token-value\n");
    let credentials = TempFile::new("credentials", "");
    let config = TempFile::new(
        "config",
        &format!(
            "[profile webid]\nrole_arn = arn:aws:iam::123456789012:role/webid\nweb_identity_token_file = {}\nregion = us-west-2\n",
            token.path()
        ),
    );
    let server = MockServer::start().await;
    mount_converse_stream(&server).await;
    mount_assume_role(
        &server,
        &assume_role_xml("AKIAWEBID", "webid-session-token"),
    )
    .await;
    unsafe {
        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", credentials.path());
        std::env::set_var("AWS_CONFIG_FILE", config.path());
        std::env::set_var("AWS_PROFILE", "webid");
        std::env::set_var("AWS_ENDPOINT_URL_STS", server.uri());
    }

    let message = run_stream(&server).await;
    assert_eq!(message.stop_reason, StopReason::Error);

    let requests = server.received_requests().await.expect("requests");
    let sts = requests
        .iter()
        .find(|request| request.url.path() == "/")
        .expect("sts request");
    let body = request_body(sts);
    assert!(body.contains("Action=AssumeRoleWithWebIdentity"), "{body}");
    assert!(
        body.contains("WebIdentityToken=web-identity-token-value"),
        "{body}"
    );
    assert!(
        header(sts, "authorization").is_none(),
        "AssumeRoleWithWebIdentity is unsigned"
    );
    assert!(
        authorization(stream_request(&requests)).contains("Credential=AKIAWEBID/"),
        "web identity credentials were not used"
    );
}

#[tokio::test]
async fn reads_credentials_from_a_profile_credential_process() {
    let _env = EnvGuard::new();
    let credentials = TempFile::new("credentials", "");
    let config = TempFile::new(
        "config",
        "[profile proc]\ncredential_process = printf '%s' '{\"Version\":1,\"AccessKeyId\":\"AKIAPROCESS\",\"SecretAccessKey\":\"process-secret\",\"SessionToken\":\"process-token\"}'\nregion = us-west-2\n",
    );
    unsafe {
        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", credentials.path());
        std::env::set_var("AWS_CONFIG_FILE", config.path());
        std::env::set_var("AWS_PROFILE", "proc");
    }

    let server = MockServer::start().await;
    mount_converse_stream(&server).await;
    let message = run_stream(&server).await;
    assert_eq!(message.stop_reason, StopReason::Error);

    let requests = server.received_requests().await.expect("requests");
    let request = stream_request(&requests);
    assert!(
        authorization(request).contains("Credential=AKIAPROCESS/"),
        "credential_process credentials were not used"
    );
    assert_eq!(
        header(request, "x-amz-security-token").as_deref(),
        Some("process-token")
    );
}

#[tokio::test]
async fn uses_ecs_container_credentials_from_the_full_uri_with_an_authorization_token() {
    let _env = EnvGuard::new();
    let credentials = TempFile::new("credentials", "");
    let config = TempFile::new("config", "");
    let server = MockServer::start().await;
    mount_converse_stream(&server).await;
    Mock::given(method("GET"))
        .and(path("/creds"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"AccessKeyId":"AKIAECS","SecretAccessKey":"ecs-secret","Token":"ecs-token","Expiration":"2030-01-01T00:00:00Z"}"#,
        ))
        .mount(&server)
        .await;
    unsafe {
        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", credentials.path());
        std::env::set_var("AWS_CONFIG_FILE", config.path());
        std::env::set_var(
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
            format!("{}/creds", server.uri()),
        );
        std::env::set_var("AWS_CONTAINER_AUTHORIZATION_TOKEN", "container-token");
    }

    let message = run_stream(&server).await;
    assert_eq!(message.stop_reason, StopReason::Error);

    let requests = server.received_requests().await.expect("requests");
    let ecs = requests
        .iter()
        .find(|request| request.url.path() == "/creds")
        .expect("ecs request");
    assert_eq!(
        header(ecs, "authorization").as_deref(),
        Some("container-token")
    );
    let request = stream_request(&requests);
    assert!(
        authorization(request).contains("Credential=AKIAECS/"),
        "ecs credentials were not used"
    );
    assert_eq!(
        header(request, "x-amz-security-token").as_deref(),
        Some("ecs-token")
    );
}

#[tokio::test]
async fn falls_back_to_the_ec2_instance_metadata_service() {
    let _env = EnvGuard::new();
    let credentials = TempFile::new("credentials", "");
    let config = TempFile::new("config", "");
    let server = MockServer::start().await;
    mount_converse_stream(&server).await;
    Mock::given(method("PUT"))
        .and(path("/latest/api/token"))
        .respond_with(ResponseTemplate::new(200).set_body_string("imds-token"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/latest/meta-data/iam/security-credentials/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("test-role"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/latest/meta-data/iam/security-credentials/test-role"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"AccessKeyId":"AKIAIMDS","SecretAccessKey":"imds-secret","Token":"imds-session-token","Expiration":"2030-01-01T00:00:00Z"}"#,
        ))
        .mount(&server)
        .await;
    unsafe {
        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", credentials.path());
        std::env::set_var("AWS_CONFIG_FILE", config.path());
        std::env::set_var("AWS_EC2_METADATA_SERVICE_ENDPOINT", server.uri());
    }

    let message = run_stream(&server).await;
    assert_eq!(message.stop_reason, StopReason::Error);

    let requests = server.received_requests().await.expect("requests");
    let role_request = requests
        .iter()
        .find(|request| {
            request.url.path() == "/latest/meta-data/iam/security-credentials/test-role"
        })
        .expect("imds role request");
    assert_eq!(
        header(role_request, "x-aws-ec2-metadata-token").as_deref(),
        Some("imds-token"),
        "IMDSv2 token must be sent to the credentials endpoint"
    );
    let request = stream_request(&requests);
    assert!(
        authorization(request).contains("Credential=AKIAIMDS/"),
        "IMDS credentials were not used"
    );
    assert_eq!(
        header(request, "x-amz-security-token").as_deref(),
        Some("imds-session-token")
    );
}

#[tokio::test]
async fn reports_could_not_load_credentials_from_any_providers() {
    let _env = EnvGuard::new();
    let credentials = TempFile::new("credentials", "");
    let config = TempFile::new("config", "");
    unsafe {
        std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", credentials.path());
        std::env::set_var("AWS_CONFIG_FILE", config.path());
        std::env::set_var("AWS_EC2_METADATA_DISABLED", "true");
    }
    let server = MockServer::start().await;
    mount_converse_stream(&server).await;

    let message = run_stream(&server).await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Could not load credentials from any providers")
    );
    let requests = server.received_requests().await.expect("requests");
    assert!(
        requests.is_empty(),
        "no request should be sent without credentials"
    );
}
