//! Vertex ADC 默认凭据链的回归测试。
//!
//! 评审发现：Rust 侧曾只支持 `authorized_user`，其余 ADC（service account 的 RS256 JWT、
//! `external_account` 的 STS token exchange、GCE metadata server）全部返回「不支持」。
//! TS 走 google-auth-library 的 `GoogleAuth.getClient()`，这三条路径都是真实生产路径。
//!
//! 测试直接调用公开的 [`request_stream`] 并注入 [`StreamOptions::fetch`]：token endpoint
//! 的网络请求全部落在 mock [`HttpClient`] 上（不会解析真实 DNS）。

use std::sync::{Arc, Mutex};

use kiana_ai::api::google_shared::{GoogleClientConfig, request_stream};
use kiana_ai::http::{ByteStream, HttpClient, HttpError, HttpRequest, HttpResponse};
use kiana_ai::types::{ProviderEnv, ProviderResponse, StreamOptions};
use serde_json::{Value, json};

const SERVICE_ACCOUNT_KEY: &str =
    include_str!("fixtures/api-other/google_adc_service_account_key.pem");

type Captured = Arc<Mutex<Vec<HttpRequest>>>;
type MockResponse = (u16, Vec<(String, String)>, Vec<u8>);
type MockHandler = Box<dyn Fn(&HttpRequest) -> MockResponse + Send + Sync>;

struct MockClient {
    requests: Captured,
    handler: MockHandler,
}

impl HttpClient for MockClient {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> kiana_ai::http::BoxFuture<'a, Result<HttpResponse, HttpError>> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request.clone());
        let (status, headers, body) = (self.handler)(&request);
        Box::pin(async move {
            let body: ByteStream = Box::pin(futures::stream::iter(vec![Ok(body)]));
            Ok(HttpResponse {
                response: ProviderResponse {
                    status,
                    headers: headers.into_iter().collect(),
                },
                body,
            })
        })
    }
}

fn mock(
    handler: impl Fn(&HttpRequest) -> MockResponse + Send + Sync + 'static,
) -> (Arc<dyn HttpClient>, Captured) {
    let requests: Captured = Arc::new(Mutex::new(Vec::new()));
    let client: Arc<dyn HttpClient> = Arc::new(MockClient {
        requests: requests.clone(),
        handler: Box::new(handler),
    });
    (client, requests)
}

fn json_response(body: &str) -> MockResponse {
    (
        200,
        vec![("content-type".into(), "application/json".into())],
        body.as_bytes().to_vec(),
    )
}

fn sse_response() -> MockResponse {
    (
        200,
        vec![("content-type".into(), "text/event-stream".into())],
        Vec::new(),
    )
}

struct TempFile(std::path::PathBuf);

impl TempFile {
    fn new(name: &str, contents: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "kiana_ai_adc_{}_{}_{}",
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

fn credentials_file(contents: &Value) -> TempFile {
    TempFile::new("adc.json", &kiana_json::js_object::stringify(contents))
}

fn params() -> Value {
    json!({
        "model": "gemini-3-flash-preview",
        "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
    })
}

fn client_with_credentials(path: &str) -> GoogleClientConfig {
    GoogleClientConfig {
        vertexai: true,
        api_key: None,
        project: Some("test-project".into()),
        location: Some("us-central1".into()),
        api_version: Some("v1".into()),
        google_auth_key_filename: Some(path.into()),
        http_options: None,
    }
}

fn options(fetch: Arc<dyn HttpClient>) -> StreamOptions {
    StreamOptions {
        fetch: Some(fetch),
        ..Default::default()
    }
}

fn form_pairs(body: &[u8]) -> Vec<(String, String)> {
    let text = std::str::from_utf8(body).expect("utf-8 form body");
    text.split('&')
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (
                percent_encoding::percent_decode_str(key)
                    .decode_utf8_lossy()
                    .into_owned(),
                percent_encoding::percent_decode_str(value)
                    .decode_utf8_lossy()
                    .into_owned(),
            )
        })
        .collect()
}

fn form_value(pairs: &[(String, String)], key: &str) -> String {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| panic!("missing form field {key}"))
}

fn header(request: &HttpRequest, name: &str) -> Option<String> {
    request
        .headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn decode_base64url(value: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .expect("base64url")
}

fn authorization(request: &HttpRequest) -> String {
    header(request, "authorization").expect("authorization header")
}

#[tokio::test]
async fn service_account_adc_signs_a_jwt_bearer_grant_and_uses_the_returned_token() {
    let credentials = credentials_file(&json!({
        "type": "service_account",
        "client_email": "svc@test-project.iam.gserviceaccount.com",
        "private_key": SERVICE_ACCOUNT_KEY,
        "token_uri": "https://oauth2.googleapis.com/token",
    }));
    let (fetch, requests) = mock(|request| {
        if request.url == "https://oauth2.googleapis.com/token" {
            json_response(r#"{"access_token":"sa-token","expires_in":3600,"token_type":"Bearer"}"#)
        } else {
            sse_response()
        }
    });
    let client = client_with_credentials(credentials.path());
    let _stream = request_stream(&client, &params(), &options(fetch))
        .await
        .expect("stream");

    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "token request + stream request");
    let token_request = &requests[0];
    assert_eq!(token_request.method, "POST");
    assert_eq!(token_request.url, "https://oauth2.googleapis.com/token");
    assert_eq!(
        header(token_request, "content-type").as_deref(),
        Some("application/x-www-form-urlencoded")
    );
    let pairs = form_pairs(token_request.body.as_deref().expect("form body"));
    assert_eq!(
        form_value(&pairs, "grant_type"),
        "urn:ietf:params:oauth:grant-type:jwt-bearer"
    );

    // gtoken 的 assertion 是 RS256 JWS；claims 顺序与取值见 `buildPayloadForJwsSign`。
    let assertion = form_value(&pairs, "assertion");
    let parts: Vec<&str> = assertion.split('.').collect();
    assert_eq!(parts.len(), 3, "JWS has header.payload.signature");
    let header_json: Value =
        serde_json::from_slice(&decode_base64url(parts[0])).expect("JWS header");
    assert_eq!(header_json, json!({"alg": "RS256"}));
    let claims: Value = serde_json::from_slice(&decode_base64url(parts[1])).expect("JWS claims");
    assert_eq!(
        claims["iss"],
        json!("svc@test-project.iam.gserviceaccount.com")
    );
    assert_eq!(
        claims["scope"],
        json!("https://www.googleapis.com/auth/cloud-platform")
    );
    assert_eq!(claims["aud"], json!("https://oauth2.googleapis.com/token"));
    let iat = claims["iat"].as_i64().expect("iat");
    let exp = claims["exp"].as_i64().expect("exp");
    assert_eq!(exp - iat, 3600);
    assert!(claims.get("sub").is_none());
    assert_eq!(decode_base64url(parts[2]).len(), 256, "2048-bit RS256");

    let stream_request = &requests[1];
    assert_eq!(authorization(stream_request), "Bearer sa-token");
    assert!(
        stream_request.url.ends_with(
            "/publishers/google/models/gemini-3-flash-preview:streamGenerateContent?alt=sse"
        ),
        "unexpected stream url: {}",
        stream_request.url
    );
}

#[tokio::test]
async fn external_account_adc_exchanges_the_subject_token_with_sts() {
    let subject = TempFile::new("subject.txt", "external-subject-token");
    let credentials = credentials_file(&json!({
        "type": "external_account",
        "audience": "//iam.googleapis.com/projects/123456/locations/global/workloadIdentityPools/pool/providers/provider",
        "subject_token_type": "urn:ietf:params:oauth:token-type:jwt",
        "token_url": "https://sts.example.test/v1/token",
        "credential_source": {
            "file": subject.path(),
            "format": {"type": "text"},
        },
    }));
    let (fetch, requests) = mock(|request| {
        if request.url == "https://sts.example.test/v1/token" {
            json_response(r#"{"access_token":"sts-token","expires_in":3600}"#)
        } else {
            sse_response()
        }
    });
    let client = client_with_credentials(credentials.path());
    let _stream = request_stream(&client, &params(), &options(fetch))
        .await
        .expect("stream");

    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let pairs = form_pairs(requests[0].body.as_deref().expect("form body"));
    assert_eq!(
        form_value(&pairs, "grant_type"),
        "urn:ietf:params:oauth:grant-type:token-exchange"
    );
    assert_eq!(
        form_value(&pairs, "audience"),
        "//iam.googleapis.com/projects/123456/locations/global/workloadIdentityPools/pool/providers/provider"
    );
    assert_eq!(
        form_value(&pairs, "scope"),
        "https://www.googleapis.com/auth/cloud-platform"
    );
    assert_eq!(
        form_value(&pairs, "requested_token_type"),
        "urn:ietf:params:oauth:token-type:access_token"
    );
    assert_eq!(
        form_value(&pairs, "subject_token"),
        "external-subject-token"
    );
    assert_eq!(
        form_value(&pairs, "subject_token_type"),
        "urn:ietf:params:oauth:token-type:jwt"
    );
    assert_eq!(authorization(&requests[1]), "Bearer sts-token");
}

#[tokio::test]
async fn external_account_adc_impersonates_the_service_account_when_configured() {
    let subject = TempFile::new("subject.txt", "external-subject-token");
    let credentials = credentials_file(&json!({
        "type": "external_account",
        "audience": "//iam.googleapis.com/projects/123456/locations/global/workloadIdentityPools/pool/providers/provider",
        "subject_token_type": "urn:ietf:params:oauth:token-type:jwt",
        "token_url": "https://sts.example.test/v1/token",
        "service_account_impersonation_url": "https://iamcredentials.example.test/v1/projects/-/serviceAccounts/sa@test.iam.gserviceaccount.com:generateAccessToken",
        "credential_source": {"file": subject.path(), "format": {"type": "text"}},
    }));
    let (fetch, requests) = mock(|request| {
        if request.url == "https://sts.example.test/v1/token" {
            json_response(r#"{"access_token":"sts-token"}"#)
        } else if request.url.contains("generateAccessToken") {
            json_response(
                r#"{"accessToken":"impersonated-token","expireTime":"2030-01-01T00:00:00Z"}"#,
            )
        } else {
            sse_response()
        }
    });
    let client = client_with_credentials(credentials.path());
    let _stream = request_stream(&client, &params(), &options(fetch))
        .await
        .expect("stream");

    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    let impersonation = &requests[1];
    assert_eq!(authorization(impersonation), "Bearer sts-token");
    assert_eq!(
        header(impersonation, "content-type").as_deref(),
        Some("application/json")
    );
    let body: Value =
        serde_json::from_slice(impersonation.body.as_deref().expect("body")).expect("json body");
    assert_eq!(
        body,
        json!({
            "scope": ["https://www.googleapis.com/auth/cloud-platform"],
            "lifetime": "3600s",
        })
    );
    assert_eq!(authorization(&requests[2]), "Bearer impersonated-token");
}

#[tokio::test]
async fn falls_back_to_the_gce_metadata_server_when_no_credentials_file_exists() {
    let empty_home = TempFile::new("home", "");
    let mut env = ProviderEnv::new();
    env.insert(
        "GCE_METADATA_HOST".into(),
        "http://metadata.example.test".into(),
    );
    env.insert("METADATA_SERVER_DETECTION".into(), "ping-only".into());
    // 把 well-known ADC 路径固定到空目录，避免机器上真实的 gcloud 凭据干扰。
    env.insert("HOME".into(), empty_home.path().into());
    let (fetch, requests) = mock(|request| {
        if request.url == "http://metadata.example.test/computeMetadata/v1/instance" {
            (
                200,
                vec![("metadata-flavor".into(), "Google".into())],
                b"{}".to_vec(),
            )
        } else if request
            .url
            .starts_with("http://metadata.example.test/computeMetadata/v1/instance/service-accounts/default/token")
        {
            json_response(r#"{"access_token":"metadata-token","expires_in":3600}"#)
        } else {
            sse_response()
        }
    });
    let client = GoogleClientConfig {
        vertexai: true,
        api_key: None,
        project: Some("test-project".into()),
        location: Some("us-central1".into()),
        api_version: Some("v1".into()),
        google_auth_key_filename: None,
        http_options: None,
    };
    let options = StreamOptions {
        env: Some(env),
        fetch: Some(fetch),
        ..Default::default()
    };
    let _stream = request_stream(&client, &params(), &options)
        .await
        .expect("stream");

    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3, "ping + token + stream");
    assert_eq!(
        header(&requests[0], "metadata-flavor").as_deref(),
        Some("Google")
    );
    assert_eq!(
        requests[1].url,
        "http://metadata.example.test/computeMetadata/v1/instance/service-accounts/default/token?scopes=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcloud-platform"
    );
    assert_eq!(authorization(&requests[2]), "Bearer metadata-token");
}

#[tokio::test]
async fn reports_the_google_auth_library_no_adc_found_error() {
    let empty_home = TempFile::new("home", "");
    let mut env = ProviderEnv::new();
    env.insert("METADATA_SERVER_DETECTION".into(), "none".into());
    env.insert("HOME".into(), empty_home.path().into());
    let (fetch, requests) = mock(|_| sse_response());
    let client = GoogleClientConfig {
        vertexai: true,
        api_key: None,
        project: Some("test-project".into()),
        location: Some("us-central1".into()),
        api_version: Some("v1".into()),
        google_auth_key_filename: None,
        http_options: None,
    };
    let options = StreamOptions {
        env: Some(env),
        fetch: Some(fetch),
        ..Default::default()
    };
    let error = match request_stream(&client, &params(), &options).await {
        Ok(_) => panic!("expected the ADC lookup to fail"),
        Err(error) => error,
    };
    assert_eq!(
        error.message,
        "Could not load the default credentials. Browse to https://cloud.google.com/docs/authentication/getting-started for more information."
    );
    assert!(requests.lock().unwrap().is_empty());
}
