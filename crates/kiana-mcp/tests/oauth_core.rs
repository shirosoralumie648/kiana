//! OAuth 的 types/errors/discovery（`spec/telemetry-mcp.md` §18.1–§18.3、§27.2、§27.5）。
//!
//! 黄金数据由 TS 参照实现（`packages/mcp/src/oauth/{types,discovery}.ts`，Node 26）生成，
//! 见 `tests/fixtures/oauth_*.json`。覆盖：
//! - `parseWwwAuthenticate` 的正则怪癖向量（§27.5）；
//! - 四个 `parse*` 的成功形状（含未知字段与 JS 键序）与逐字错误消息；
//! - `buildAuthorizationServerDiscoveryUrls`、`selectResource`、`resourceUrlFromServerUrl`；
//! - `blob:` origin 与 `file:` 前导斜杠（`oauth_origin_golden.json`，后者带已知差异标签）；
//! - `fetchMetadata` 的 `Headers.append` 头校验（非法值在发请求前抛 `TypeError`）；
//! - 用可替换的 `McpFetch` 验证发现流程的 URL、头、回退、错误消息与 issuer 校验；
//! - 两条移植自 TS `test/oauth.test.ts` 的用例（issuer 不匹配、拒绝非 HTTP 端点）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use http::{HeaderMap, HeaderValue};
use kiana_mcp::auth_provider::{FetchInit, FetchResponse, McpFetch, McpTransportError};
use kiana_mcp::oauth::discovery::{
    DiscoverAuthorizationServerOptions, DiscoverOAuthServerInfoOptions,
    DiscoverProtectedResourceOptions, build_authorization_server_discovery_urls,
    discover_authorization_server_metadata, discover_oauth_server_info,
    discover_protected_resource_metadata, parse_www_authenticate, resource_url_from_server_url,
    select_resource,
};
use kiana_mcp::oauth::errors::OAuthFailure;
use kiana_mcp::oauth::types::{
    AuthorizationServerMetadata, OAuthProtectedResourceMetadata,
    parse_authorization_server_metadata, parse_client_information, parse_oauth_tokens,
    parse_protected_resource_metadata,
};
use kiana_mcp::protocol_types::LATEST_PROTOCOL_VERSION;
use serde_json::{Value, json};

const WWW_GOLDEN: &str = include_str!("fixtures/oauth_www_authenticate_golden.json");
const WWW_EDGES_GOLDEN: &str = include_str!("fixtures/oauth_www_authenticate_edges_golden.json");
const WWW_FUZZ_GOLDEN: &str = include_str!("fixtures/oauth_www_authenticate_fuzz_golden.json");
const WWW_UNICODE_GOLDEN: &str =
    include_str!("fixtures/oauth_www_authenticate_unicode_golden.json");
const TYPES_GOLDEN: &str = include_str!("fixtures/oauth_types_golden.json");
const TYPES_FUZZ_GOLDEN: &str = include_str!("fixtures/oauth_types_fuzz_golden.json");
const URLS_GOLDEN: &str = include_str!("fixtures/oauth_discovery_urls_golden.json");
const SELECT_GOLDEN: &str = include_str!("fixtures/oauth_select_resource_golden.json");
const URL_NORMALIZATION_GOLDEN: &str = include_str!("fixtures/oauth_url_normalization_golden.json");
const ORIGIN_GOLDEN: &str = include_str!("fixtures/oauth_origin_golden.json");

// ---------------------------------------------------------------------------
// 可替换的 fetch
// ---------------------------------------------------------------------------

/// 记录下来的一个请求。
#[derive(Debug, Clone)]
struct SeenRequest {
    url: String,
    method: String,
    accept: Option<String>,
    protocol_version: Option<String>,
}

/// 按 URL 返回预置响应的 `McpFetch`；未匹配的 URL 一律 404。
///
/// `status_kind`：`"json"`（正常 JSON 响应）或 `"error"`（错误的状态码 + 空体）。
struct ScriptedFetch {
    seen: Mutex<Vec<SeenRequest>>,
    routes: Vec<(String, u16, String)>,
    /// `TypeError`（网络失败）的 URL 集合：模拟 fetch 抛错。
    network_errors: Vec<String>,
}

impl ScriptedFetch {
    fn new(routes: Vec<(&str, u16, &str)>) -> Self {
        ScriptedFetch {
            seen: Mutex::new(Vec::new()),
            routes: routes
                .into_iter()
                .map(|(url, status, body)| (url.to_owned(), status, body.to_owned()))
                .collect(),
            network_errors: Vec::new(),
        }
    }

    fn with_network_errors(mut self, urls: Vec<&str>) -> Self {
        self.network_errors = urls.into_iter().map(str::to_owned).collect();
        self
    }

    fn requests(&self) -> Vec<SeenRequest> {
        self.seen.lock().unwrap().clone()
    }

    fn urls(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .map(|request| request.url)
            .collect()
    }
}

#[async_trait]
impl McpFetch for ScriptedFetch {
    async fn fetch(
        &self,
        url: &url::Url,
        init: FetchInit,
    ) -> Result<FetchResponse, McpTransportError> {
        let accept = init.headers.get("accept").map(str::to_owned);
        let protocol_version = init.headers.get("mcp-protocol-version").map(str::to_owned);
        self.seen.lock().unwrap().push(SeenRequest {
            url: url.as_str().to_owned(),
            method: init.method.as_str().to_owned(),
            accept,
            protocol_version,
        });
        if init
            .signal
            .as_ref()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(McpTransportError::aborted());
        }
        if self.network_errors.iter().any(|bad| bad == url.as_str()) {
            return Err(McpTransportError::network("fetch failed"));
        }
        let (status, body) = self
            .routes
            .iter()
            .find(|(route, _, _)| route == url.as_str())
            .map(|(_, status, body)| (*status, body.clone()))
            .unwrap_or((404, String::new()));
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        Ok(FetchResponse::from_bytes(
            status,
            headers,
            body.into_bytes(),
        ))
    }
}

fn shared(fetch: ScriptedFetch) -> Arc<dyn McpFetch> {
    Arc::new(fetch)
}

fn message(error: OAuthFailure) -> String {
    error.message()
}

fn message_of(error: &OAuthFailure) -> String {
    error.message()
}

// ---------------------------------------------------------------------------
// WWW-Authenticate（§27.5）
// ---------------------------------------------------------------------------

#[test]
fn parse_www_authenticate_matches_the_typescript_golden_vectors() {
    for (golden, minimum) in [
        (WWW_GOLDEN, 20usize),
        (WWW_EDGES_GOLDEN, 40),
        (WWW_FUZZ_GOLDEN, 800),
        (WWW_UNICODE_GOLDEN, 24),
    ] {
        check_www_authenticate_golden(golden, minimum);
    }
}

fn check_www_authenticate_golden(golden: &str, minimum: usize) {
    let cases: Vec<Value> = serde_json::from_str(golden).unwrap();
    assert!(cases.len() >= minimum);
    for case in cases {
        let header = case.get("header").and_then(Value::as_str);
        let challenge = parse_www_authenticate(header);
        let text = |key: &str| case.get(key).and_then(Value::as_str).map(str::to_owned);
        assert_eq!(
            challenge
                .resource_metadata_url
                .as_ref()
                .map(url::Url::as_str)
                .map(str::to_owned),
            text("resourceMetadataUrl"),
            "resourceMetadataUrl for {header:?}"
        );
        assert_eq!(challenge.scope, text("scope"), "scope for {header:?}");
        assert_eq!(challenge.error, text("error"), "error for {header:?}");
        assert_eq!(
            challenge.error_description,
            text("errorDescription"),
            "errorDescription for {header:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// parse*（§18.1）
// ---------------------------------------------------------------------------

fn run_parser(name: &str, input: &Value) -> Result<String, String> {
    match name {
        "parseProtectedResourceMetadata" => parse_protected_resource_metadata(input)
            .map(|metadata| kiana_json::js_object::stringify(&metadata.to_value()))
            .map_err(|error| error.message()),
        "parseAuthorizationServerMetadata" => parse_authorization_server_metadata(input)
            .map(|metadata| kiana_json::js_object::stringify(&metadata.to_value()))
            .map_err(|error| error.message()),
        "parseOAuthTokens" => parse_oauth_tokens(input)
            .map(|tokens| kiana_json::js_object::stringify(&tokens.to_value()))
            .map_err(|error| error.message()),
        "parseClientInformation" => parse_client_information(input)
            .map(|information| kiana_json::js_object::stringify(&information.to_value()))
            .map_err(|error| error.message()),
        other => panic!("unknown parser {other}"),
    }
}

#[test]
fn metadata_parsers_match_the_typescript_golden_vectors() {
    check_parser_golden(TYPES_GOLDEN, 1);
}

/// 确定性伪随机生成的输入（`gen_types_fuzz.mjs`，种子 424242）：覆盖标量/数组/缺失键的组合。
#[test]
fn metadata_parsers_match_the_typescript_fuzz_vectors() {
    check_parser_golden(TYPES_FUZZ_GOLDEN, 800);
}

fn check_parser_golden(golden: &str, minimum: usize) {
    let golden: Value = serde_json::from_str(golden).unwrap();
    let mut total = 0usize;
    for parser in golden["parsers"].as_array().unwrap() {
        let name = parser["functionName"].as_str().unwrap();
        for case in parser["cases"].as_array().unwrap() {
            let input = &case["input"];
            let result = &case["result"];
            total += 1;
            let outcome = run_parser(name, input);
            if result["ok"].as_bool().unwrap() {
                let expected = result["json"].as_str().unwrap();
                assert_eq!(
                    outcome
                        .as_deref()
                        .unwrap_or_else(|error| panic!("{name} failed on {input}: {error}")),
                    expected,
                    "{name}({input})"
                );
            } else {
                let expected = result["message"].as_str().unwrap();
                assert_eq!(
                    outcome.unwrap_err(),
                    expected,
                    "{name}({input}) should fail with the TS message"
                );
                assert_eq!(result["name"].as_str().unwrap(), "Error");
            }
        }
    }
    assert!(total >= minimum, "golden had only {total} cases");
}

// ---------------------------------------------------------------------------
// 发现 URL 与资源选择
// ---------------------------------------------------------------------------

#[test]
fn discovery_urls_match_the_typescript_golden_vectors() {
    let cases: Vec<Value> = serde_json::from_str(URLS_GOLDEN).unwrap();
    for case in cases {
        let input = case["input"].as_str().unwrap();
        let built = build_authorization_server_discovery_urls(input).unwrap();
        let actual: Vec<Value> = built
            .iter()
            .map(|entry| json!({"href": entry.url.as_str(), "type": entry.kind.as_str()}))
            .collect();
        assert_eq!(
            actual,
            case["urls"].as_array().unwrap().clone(),
            "for {input}"
        );
    }
}

#[test]
fn select_resource_matches_the_typescript_golden_vectors() {
    let cases: Vec<Value> = serde_json::from_str(SELECT_GOLDEN).unwrap();
    for case in cases {
        let server_url = case["serverUrl"].as_str().unwrap();
        let metadata = match &case["metadata"] {
            Value::Null => None,
            value => {
                let mut metadata = OAuthProtectedResourceMetadata::new();
                metadata.set("resource", value["resource"].clone());
                Some(metadata)
            }
        };
        assert_eq!(
            resource_url_from_server_url(server_url)
                .map(|url| url.as_str().to_owned())
                .ok(),
            case["requested"].as_str().map(str::to_owned),
            "requested URL for {server_url}"
        );
        let result = &case["result"];
        let outcome = select_resource(server_url, metadata.as_ref());
        if result["ok"].as_bool().unwrap() {
            assert_eq!(
                outcome.unwrap(),
                result["value"].as_str().map(str::to_owned),
                "selectResource({server_url})"
            );
        } else {
            assert_eq!(
                message(outcome.unwrap_err()),
                result["message"].as_str().unwrap(),
                "selectResource({server_url})"
            );
        }
    }
}

/// `blob:`/`file:` 的 origin 与路径语义（评审 finding #1、#2）。
///
/// - `blob:` 的 origin 只在内层 URL 的 scheme 是 `http`/`https`/`file` 时取内层 origin，
///   其余（`ftp`/`ws`/`wss`/嵌套 `blob:`）是 `"null"` —— `url` crate 的 `Url::origin` 会把
///   `blob:ftp://x/y` 认成 `ftp://x`，发现 URL 会直接用错 base；`selectResource` 的 origin
///   比较也改用同一份判定（那里最终结论通常由路径检查决定，但共用同一套 origin 语义）；
/// - `file:` 的**重复前导斜杠**是已知无法复刻的 `url::Url` 限制（见 `js_url` 模块文档）。
///   黄金数据里带 `"rust"` 的用例给出 Rust 侧归一化后的钉住值，其余用例两边必须一致。
#[test]
fn blob_and_file_url_semantics_match_the_typescript_golden_vectors() {
    let golden: Value = serde_json::from_str(ORIGIN_GOLDEN).unwrap();

    // buildAuthorizationServerDiscoveryUrls：blob base / 不透明 origin。
    for case in golden["build"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let expected = &case["result"];
        let outcome = build_authorization_server_discovery_urls(input);
        if expected["ok"].as_bool().unwrap() {
            let actual: Vec<Value> = outcome
                .unwrap_or_else(|error| panic!("{input}: unexpected error {error}"))
                .iter()
                .map(|entry| json!({"href": entry.url.as_str(), "type": entry.kind.as_str()}))
                .collect();
            assert_eq!(Value::Array(actual), expected["urls"], "for {input}");
        } else {
            let error = outcome.expect_err(&format!("{input}: expected a TypeError"));
            assert!(error.is_type_error(), "{input}: {}", error.name());
            assert_eq!(
                message(error),
                expected["message"].as_str().unwrap(),
                "{input}"
            );
        }
    }

    // resourceUrlFromServerUrl：`file:` 前导斜杠的已知差异。
    for case in golden["resources"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let expected = case.get("rust").unwrap_or(&case["result"]);
        let outcome = resource_url_from_server_url(input);
        if expected["ok"].as_bool().unwrap() {
            assert_eq!(
                outcome
                    .unwrap_or_else(|error| panic!("{input}: unexpected error {error}"))
                    .as_str(),
                expected["value"].as_str().unwrap(),
                "for {input}"
            );
        } else {
            let error = outcome.expect_err(input);
            assert!(error.is_type_error(), "{input}");
            assert_eq!(message(error), expected["message"].as_str().unwrap());
        }
    }

    // parseWwwAuthenticate 的 resourceMetadataUrl：`file:` 前导斜杠的已知差异。
    for case in golden["www"].as_array().unwrap() {
        let header = case["header"].as_str().unwrap();
        let expected = case
            .get("rust")
            .and_then(|rust| rust["resourceMetadataUrl"].as_str())
            .or_else(|| case["resourceMetadataUrl"].as_str());
        assert_eq!(
            parse_www_authenticate(Some(header))
                .resource_metadata_url
                .as_ref()
                .map(url::Url::as_str),
            expected,
            "resourceMetadataUrl for {header}"
        );
    }

    // selectResource：origin 是**字符串**比较（`null` == `null`），blob/file 的判定与消息。
    for case in golden["select"].as_array().unwrap() {
        let server_url = case["serverUrl"].as_str().unwrap();
        let mut metadata = OAuthProtectedResourceMetadata::new();
        metadata.set("resource", case["metadata"]["resource"].clone());
        let override_ = case.get("rust");
        let expected_requested = override_
            .map(|rust| &rust["requested"])
            .unwrap_or(&case["requested"]);
        let expected = override_
            .map(|rust| &rust["result"])
            .unwrap_or(&case["result"]);
        assert_eq!(
            resource_url_from_server_url(server_url)
                .map(|url| url.as_str().to_owned())
                .ok(),
            expected_requested.as_str().map(str::to_owned),
            "requested URL for {server_url}"
        );
        let outcome = select_resource(server_url, Some(&metadata));
        if expected["ok"].as_bool().unwrap() {
            assert_eq!(
                outcome.unwrap(),
                expected["value"].as_str().map(str::to_owned),
                "selectResource({server_url})"
            );
        } else {
            assert_eq!(
                message(outcome.expect_err(server_url)),
                expected["message"].as_str().unwrap(),
                "selectResource({server_url})"
            );
        }
    }
}

/// `fetchMetadata` 的头校验（评审 finding #3）：TS 的 `fetch(url, { headers: {...} })` 在
/// `Request` 构造时按 `new Headers(record)` 逐对 `append`，非法值在**发请求前**抛
/// `TypeError`（`Headers.append: …` / ByteString 文案逐字），底层 fetch 不会被调用。
#[tokio::test]
async fn fetch_metadata_validates_headers_before_sending() {
    let golden: Value = serde_json::from_str(ORIGIN_GOLDEN).unwrap();
    for case in golden["fetchHeaders"].as_array().unwrap() {
        let value = case["value"].as_str().unwrap();
        let fetch = Arc::new(ScriptedFetch::new(vec![(
            "https://x.example/.well-known/oauth-protected-resource",
            200,
            r#"{"resource":"https://x.example"}"#,
        )]));
        let outcome = discover_protected_resource_metadata(
            "https://x.example",
            &DiscoverProtectedResourceOptions {
                fetch: Some(fetch.clone()),
                protocol_version: Some(value.to_owned()),
                ..DiscoverProtectedResourceOptions::default()
            },
        )
        .await;
        if case["ok"].as_bool().unwrap() {
            outcome
                .unwrap_or_else(|error| panic!("{value:?}: unexpected error {}", error.message()));
            assert_eq!(
                fetch.requests()[0].protocol_version.as_deref(),
                case["stored"].as_str(),
                "stored header value for {value:?}"
            );
        } else {
            let error = match outcome {
                Ok(_) => panic!("{value:?}: expected a TypeError"),
                Err(error) => error,
            };
            assert!(error.is_type_error(), "{value:?}: {}", error.name());
            assert_eq!(error.name(), case["name"].as_str().unwrap(), "{value:?}");
            assert_eq!(
                error.message(),
                case["message"].as_str().unwrap(),
                "{value:?}"
            );
            assert!(
                fetch.requests().is_empty(),
                "{value:?}: no request may be sent"
            );
        }
    }
}

/// `blob:` server URL 的受保护资源发现（评审 finding #2）：TS 用 `server.origin` 作 base，
/// `blob:https://example.com/uuid` 的 origin 是 `https://example.com`，因此**真的会发请求**，
/// 而不是像非特殊 scheme 那样抛 `TypeError: Invalid URL`。
#[tokio::test]
async fn protected_resource_discovery_uses_the_blob_origin_as_base() {
    let url = "https://example.com/.well-known/oauth-protected-resourcehttps://example.com/uuid";
    let fetch = Arc::new(ScriptedFetch::new(vec![(
        url,
        200,
        r#"{"resource":"https://example.com"}"#,
    )]));
    let metadata = discover_protected_resource_metadata(
        "blob:https://example.com/uuid",
        &DiscoverProtectedResourceOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverProtectedResourceOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(metadata.resource(), Some("https://example.com"));
    assert_eq!(fetch.urls(), vec![url]);
}

/// `new URL()` 的 WHATWG 修正：特殊 scheme 路径里的 `^` → `%5E`，以及以 `//` 开头的
/// well-known 路径是**协议相对引用**（换主机 / 主机非法时 `TypeError`）。
///
/// 黄金数据由 TS 参照实现（Node 26）生成，见
/// `tests/fixtures/oauth_url_normalization_golden.json`。带 `"rust"` 标签的用例是**已知的、
/// 无法在 `Url` 上复刻的 IDNA 数据版本差异**（见 `js_url` 模块文档）：
/// `"typeError"` 表示 Node 接受而 Rust 只能报 `TypeError`，`"accepts"` 表示反向。
#[test]
fn url_normalization_matches_the_typescript_golden_vectors() {
    let golden: Value = serde_json::from_str(URL_NORMALIZATION_GOLDEN).unwrap();
    fn known_divergence(case: &Value) -> Option<&str> {
        case["rust"].as_str()
    }

    // buildAuthorizationServerDiscoveryUrls：含 `//` 前缀与 `^` 路径。
    for case in golden["build"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let expected = &case["result"];
        let outcome = build_authorization_server_discovery_urls(input);
        match known_divergence(case) {
            Some("typeError") => {
                assert!(
                    outcome.is_err_and(|error| error.is_type_error()),
                    "{input}: documented IDNA divergence must surface as a TypeError"
                );
                continue;
            }
            Some("accepts") => {
                assert!(
                    outcome.is_ok(),
                    "{input}: documented IDNA divergence accepts"
                );
                continue;
            }
            _ => {}
        }
        if expected["ok"].as_bool().unwrap() {
            let actual: Vec<Value> = outcome
                .unwrap_or_else(|error| panic!("{input}: unexpected error {error}"))
                .iter()
                .map(|entry| json!({"href": entry.url.as_str(), "type": entry.kind.as_str()}))
                .collect();
            assert_eq!(Value::Array(actual), expected["value"], "for {input}");
        } else {
            let error = outcome.expect_err(&format!("{input}: expected a TypeError"));
            assert!(error.is_type_error(), "{input}: {}", error.name());
            assert_eq!(
                message(error),
                expected["message"].as_str().unwrap(),
                "{input}"
            );
        }
    }

    // parseWwwAuthenticate 的 resourceMetadataUrl 也走 `new URL`。
    for case in golden["www"].as_array().unwrap() {
        let header = case["header"].as_str().unwrap();
        let challenge = parse_www_authenticate(Some(header));
        match known_divergence(case) {
            Some("typeError") => {
                // Node 解析成功，Rust 的 `new URL` 失败 → `resourceMetadataUrl` 是 `undefined`。
                assert!(
                    challenge.resource_metadata_url.is_none(),
                    "{header}: documented IDNA divergence drops the URL"
                );
                continue;
            }
            Some("accepts") => {
                assert!(
                    challenge.resource_metadata_url.is_some(),
                    "{header}: documented IDNA divergence keeps the URL"
                );
                continue;
            }
            _ => {}
        }
        assert_eq!(
            challenge
                .resource_metadata_url
                .as_ref()
                .map(url::Url::as_str),
            case["resourceMetadataUrl"].as_str(),
            "resourceMetadataUrl for {header}"
        );
        assert_eq!(
            challenge.scope.as_deref(),
            case["scope"].as_str(),
            "scope for {header}"
        );
        assert_eq!(
            challenge.error.as_deref(),
            case["error"].as_str(),
            "error for {header}"
        );
        assert_eq!(
            challenge.error_description.as_deref(),
            case["errorDescription"].as_str(),
            "errorDescription for {header}"
        );
    }

    // resourceUrlFromServerUrl：`hash` 被清掉，其余按 WHATWG 规范化。
    for case in golden["resources"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let expected = &case["result"];
        let outcome = resource_url_from_server_url(input);
        if let Some("typeError") = known_divergence(case) {
            assert!(
                outcome.is_err_and(|error| error.is_type_error()),
                "{input}: documented IDNA divergence must surface as a TypeError"
            );
            continue;
        }
        if expected["ok"].as_bool().unwrap() {
            assert_eq!(
                outcome
                    .unwrap_or_else(|error| panic!("{input}: unexpected error {error}"))
                    .as_str(),
                expected["value"].as_str().unwrap(),
                "for {input}"
            );
        } else {
            let error = outcome.expect_err(input);
            assert!(error.is_type_error(), "{input}");
            assert_eq!(message(error), expected["message"].as_str().unwrap());
        }
    }

    // selectResource：`^` 与 `%5E` 必须被当作同一个 origin/路径。
    for case in golden["select"].as_array().unwrap() {
        let server_url = case["serverUrl"].as_str().unwrap();
        let mut metadata = OAuthProtectedResourceMetadata::new();
        metadata.set("resource", case["metadata"]["resource"].clone());
        let requested = &case["requested"];
        assert_eq!(
            resource_url_from_server_url(server_url)
                .map(|url| url.as_str().to_owned())
                .ok(),
            requested["value"].as_str().map(str::to_owned),
            "requested URL for {server_url}"
        );
        let expected = &case["result"];
        let outcome = select_resource(server_url, Some(&metadata));
        if expected["ok"].as_bool().unwrap() {
            assert_eq!(
                outcome.unwrap(),
                expected["value"].as_str().map(str::to_owned),
                "selectResource({server_url})"
            );
        } else {
            assert_eq!(
                message(outcome.expect_err(server_url)),
                expected["message"].as_str().unwrap(),
                "selectResource({server_url})"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// discoverProtectedResourceMetadata
// ---------------------------------------------------------------------------

#[tokio::test]
async fn protected_resource_discovery_uses_the_path_suffix() {
    // 根路径：`pathSuffix("/")` 为空 → 裸 well-known 路径。
    let fetch = Arc::new(ScriptedFetch::new(vec![(
        "https://x.example/.well-known/oauth-protected-resource",
        200,
        r#"{"resource":"https://x.example"}"#,
    )]));
    let metadata = discover_protected_resource_metadata(
        "https://x.example",
        &DiscoverProtectedResourceOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverProtectedResourceOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(metadata.resource(), Some("https://x.example"));
    assert_eq!(
        fetch.urls(),
        vec!["https://x.example/.well-known/oauth-protected-resource"]
    );
}

#[tokio::test]
async fn protected_resource_discovery_headers_and_fallback() {
    // 记录请求的 fetch：首轮 404 → 回退到裸 well-known 路径。
    struct Recorder {
        seen: Mutex<Vec<SeenRequest>>,
    }
    #[async_trait]
    impl McpFetch for Recorder {
        async fn fetch(
            &self,
            url: &url::Url,
            init: FetchInit,
        ) -> Result<FetchResponse, McpTransportError> {
            self.seen.lock().unwrap().push(SeenRequest {
                url: url.as_str().to_owned(),
                method: init.method.as_str().to_owned(),
                accept: init.headers.get("accept").map(str::to_owned),
                protocol_version: init.headers.get("mcp-protocol-version").map(str::to_owned),
            });
            let (status, body) = if url.path().ends_with("/mcp") {
                (404, String::new())
            } else {
                (200, r#"{"resource":"https://x.example"}"#.to_owned())
            };
            let mut headers = HeaderMap::new();
            headers.insert("content-type", HeaderValue::from_static("application/json"));
            Ok(FetchResponse::from_bytes(
                status,
                headers,
                body.into_bytes(),
            ))
        }
    }
    let recorder = Arc::new(Recorder {
        seen: Mutex::new(Vec::new()),
    });
    let metadata = discover_protected_resource_metadata(
        "https://x.example/mcp",
        &DiscoverProtectedResourceOptions {
            fetch: Some(recorder.clone()),
            protocol_version: Some("2024-11-05".to_owned()),
            ..DiscoverProtectedResourceOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(metadata.resource(), Some("https://x.example"));
    let seen = recorder.seen.lock().unwrap().clone();
    assert_eq!(
        seen.iter()
            .map(|request| request.url.as_str())
            .collect::<Vec<_>>(),
        vec![
            "https://x.example/.well-known/oauth-protected-resource/mcp",
            "https://x.example/.well-known/oauth-protected-resource",
        ]
    );
    for request in &seen {
        assert_eq!(request.method, "GET");
        assert_eq!(request.accept.as_deref(), Some("application/json"));
        assert_eq!(request.protocol_version.as_deref(), Some("2024-11-05"));
    }
    // 缺省协议版本是 LATEST_PROTOCOL_VERSION。
    assert_eq!(LATEST_PROTOCOL_VERSION, "2025-11-25");
}

#[tokio::test]
async fn protected_resource_discovery_does_not_fall_back_for_the_root_path_or_explicit_urls() {
    // 根路径：只有裸 well-known 一次请求，404 → 报错（不回退、不重试）。
    let fetch = Arc::new(ScriptedFetch::new(Vec::new()));
    let error = discover_protected_resource_metadata(
        "https://x.example/",
        &DiscoverProtectedResourceOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverProtectedResourceOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        message(error),
        "HTTP 404 loading OAuth protected resource metadata"
    );
    assert_eq!(
        fetch.urls(),
        vec!["https://x.example/.well-known/oauth-protected-resource"]
    );

    // 显式 URL：不回退，即便路径非根、状态是 404。
    let fetch = Arc::new(ScriptedFetch::new(Vec::new()));
    let error = discover_protected_resource_metadata(
        "https://x.example/mcp",
        &DiscoverProtectedResourceOptions {
            resource_metadata_url: Some(url::Url::parse("https://meta.example/doc").unwrap()),
            fetch: Some(fetch.clone()),
            ..DiscoverProtectedResourceOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        message(error),
        "HTTP 404 loading OAuth protected resource metadata"
    );
    assert_eq!(fetch.urls(), vec!["https://meta.example/doc"]);
}

#[tokio::test]
async fn protected_resource_discovery_reports_non_ok_statuses() {
    for (status, expected) in [
        (500u16, "HTTP 500 loading OAuth protected resource metadata"),
        (403, "HTTP 403 loading OAuth protected resource metadata"),
        (502, "HTTP 502 loading OAuth protected resource metadata"),
    ] {
        let fetch = ScriptedFetch::new(vec![(
            "https://x.example/.well-known/oauth-protected-resource",
            status,
            "",
        )]);
        let error = discover_protected_resource_metadata(
            "https://x.example/",
            &DiscoverProtectedResourceOptions {
                fetch: Some(shared(fetch)),
                ..DiscoverProtectedResourceOptions::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(message(error), expected);
    }
}

#[tokio::test]
async fn protected_resource_discovery_propagates_parse_errors() {
    let fetch = ScriptedFetch::new(vec![(
        "https://x.example/.well-known/oauth-protected-resource",
        200,
        r#"{"resource":"not a url"}"#,
    )]);
    let error = discover_protected_resource_metadata(
        "https://x.example/",
        &DiscoverProtectedResourceOptions {
            fetch: Some(shared(fetch)),
            ..DiscoverProtectedResourceOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        message(error),
        "Invalid OAuth protected resource metadata resource"
    );
}

#[tokio::test]
async fn protected_resource_discovery_honours_the_cancellation_token() {
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    // 取消在 fetch 之前生效：平台 fetch 报 `DOMException(name = "AbortError")`。
    let fetch = Arc::new(ScriptedFetch::new(vec![(
        "https://x.example/.well-known/oauth-protected-resource",
        200,
        r#"{"resource":"https://x.example"}"#,
    )]));
    let error = discover_protected_resource_metadata(
        "https://x.example/",
        &DiscoverProtectedResourceOptions {
            fetch: Some(fetch.clone()),
            signal: Some(token),
            ..DiscoverProtectedResourceOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.name(), "AbortError");
    // 请求仍然带着 signal 发出去了（取消由 fetch 实现观察）。
    assert_eq!(fetch.urls().len(), 1);
}

// ---------------------------------------------------------------------------
// discoverAuthorizationServerMetadata
// ---------------------------------------------------------------------------

#[tokio::test]
async fn authorization_server_discovery_tries_candidates_in_order_and_skips_misses() {
    let fetch = Arc::new(ScriptedFetch::new(vec![(
        "https://x.example/.well-known/openid-configuration/tenant",
        200,
        r#"{"issuer":"https://x.example/tenant","authorization_endpoint":"https://x.example/authorize","token_endpoint":"https://x.example/token","response_types_supported":["code"]}"#,
    )]));
    let metadata = discover_authorization_server_metadata(
        "https://x.example/tenant/",
        &DiscoverAuthorizationServerOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverAuthorizationServerOptions::default()
        },
    )
    .await
    .unwrap()
    .expect("metadata");
    assert_eq!(metadata.issuer(), Some("https://x.example/tenant"));
    assert_eq!(
        fetch.urls(),
        vec![
            "https://x.example/.well-known/oauth-authorization-server/tenant",
            "https://x.example/.well-known/openid-configuration/tenant",
        ]
    );
}

#[tokio::test]
async fn authorization_server_discovery_returns_none_when_all_candidates_miss() {
    let fetch = Arc::new(ScriptedFetch::new(Vec::new()));
    let metadata = discover_authorization_server_metadata(
        "https://x.example/tenant",
        &DiscoverAuthorizationServerOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverAuthorizationServerOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(metadata.is_none());
    assert_eq!(
        fetch.urls(),
        vec![
            "https://x.example/.well-known/oauth-authorization-server/tenant",
            "https://x.example/.well-known/openid-configuration/tenant",
            "https://x.example/tenant/.well-known/openid-configuration",
        ]
    );
}

#[tokio::test]
async fn authorization_server_discovery_reports_non_miss_failures() {
    let fetch = ScriptedFetch::new(vec![(
        "https://x.example/.well-known/oauth-authorization-server",
        500,
        "",
    )]);
    let error = discover_authorization_server_metadata(
        "https://x.example",
        &DiscoverAuthorizationServerOptions {
            fetch: Some(shared(fetch)),
            ..DiscoverAuthorizationServerOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        message(error),
        "HTTP 500 loading authorization server metadata from \
         https://x.example/.well-known/oauth-authorization-server"
    );
}

/// 移植自 TS：`rejects authorization metadata whose issuer does not match discovery`。
#[tokio::test]
async fn rejects_authorization_metadata_whose_issuer_does_not_match_discovery() {
    let fetch = ScriptedFetch::new(vec![(
        "https://x.example/.well-known/oauth-authorization-server",
        200,
        r#"{"issuer":"https://attacker.example","authorization_endpoint":"https://x.example/authorize","token_endpoint":"https://x.example/token","response_types_supported":["code"]}"#,
    )]);
    let error = discover_authorization_server_metadata(
        "https://x.example",
        &DiscoverAuthorizationServerOptions {
            fetch: Some(shared(fetch)),
            ..DiscoverAuthorizationServerOptions::default()
        },
    )
    .await
    .unwrap_err();
    let mismatch = error
        .as_issuer_mismatch()
        .expect("OAuthIssuerMismatchError");
    assert_eq!(mismatch.expected.as_deref(), Some("https://x.example"));
    assert_eq!(
        mismatch.received.as_deref(),
        Some("https://attacker.example")
    );
    assert_eq!(
        message(error),
        "OAuth issuer mismatch: expected \"https://x.example\", received \"https://attacker.example\""
    );
}

#[tokio::test]
async fn issuer_validation_ignores_exactly_one_trailing_slash_and_can_be_skipped() {
    // 裸 origin 期望值 vs 元数据里带尾斜杠的 issuer：两侧各去掉一个 → 相等。
    let fetch = ScriptedFetch::new(vec![(
        "https://x.example/.well-known/oauth-authorization-server",
        200,
        r#"{"issuer":"https://x.example/","authorization_endpoint":"https://x.example/authorize","token_endpoint":"https://x.example/token","response_types_supported":["code"]}"#,
    )]);
    let metadata = discover_authorization_server_metadata(
        "https://x.example",
        &DiscoverAuthorizationServerOptions {
            fetch: Some(shared(fetch)),
            ..DiscoverAuthorizationServerOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(metadata.is_some());

    // 双尾斜杠只去掉一个 → 不匹配。
    let fetch = ScriptedFetch::new(vec![(
        "https://x.example/.well-known/oauth-authorization-server",
        200,
        r#"{"issuer":"https://x.example//","authorization_endpoint":"https://x.example/authorize","token_endpoint":"https://x.example/token","response_types_supported":["code"]}"#,
    )]);
    let error = discover_authorization_server_metadata(
        "https://x.example",
        &DiscoverAuthorizationServerOptions {
            fetch: Some(shared(fetch)),
            ..DiscoverAuthorizationServerOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(error.as_issuer_mismatch().is_some());

    // skipIssuerValidation 跳过校验。
    let fetch = ScriptedFetch::new(vec![(
        "https://x.example/.well-known/oauth-authorization-server",
        200,
        r#"{"issuer":"https://attacker.example","authorization_endpoint":"https://x.example/authorize","token_endpoint":"https://x.example/token","response_types_supported":["code"]}"#,
    )]);
    let metadata = discover_authorization_server_metadata(
        "https://x.example",
        &DiscoverAuthorizationServerOptions {
            fetch: Some(shared(fetch)),
            skip_issuer_validation: true,
            ..DiscoverAuthorizationServerOptions::default()
        },
    )
    .await
    .unwrap()
    .expect("metadata");
    assert_eq!(metadata.issuer(), Some("https://attacker.example"));
}

/// 移植自 TS：`rejects non-HTTP authorization endpoints, which the platform URL handler would launch`。
#[tokio::test]
async fn rejects_non_http_authorization_endpoints() {
    for endpoint in [
        "file://attacker.example/share/x.exe",
        "search-ms:query=x",
        "ms-msdt:/id",
    ] {
        let body = json!({
            "issuer": "https://x.example",
            "authorization_endpoint": endpoint,
            "token_endpoint": "https://x.example/token",
            "response_types_supported": ["code"],
        })
        .to_string();
        let fetch = ScriptedFetch::new(vec![(
            "https://x.example/.well-known/oauth-authorization-server",
            200,
            &body,
        )]);
        let error = discover_authorization_server_metadata(
            "https://x.example",
            &DiscoverAuthorizationServerOptions {
                fetch: Some(shared(fetch)),
                ..DiscoverAuthorizationServerOptions::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            message_of(&error),
            "Invalid authorization endpoint",
            "{endpoint}"
        );
        assert!(!error.is_type_error());
    }
}

#[tokio::test]
async fn authorization_server_discovery_propagates_network_type_errors() {
    let fetch = ScriptedFetch::new(Vec::new()).with_network_errors(vec![
        "https://x.example/.well-known/oauth-authorization-server",
    ]);
    let error = discover_authorization_server_metadata(
        "https://x.example",
        &DiscoverAuthorizationServerOptions {
            fetch: Some(shared(fetch)),
            ..DiscoverAuthorizationServerOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(error.is_type_error());
    assert_eq!(error.name(), "TypeError");
}

// ---------------------------------------------------------------------------
// discoverOAuthServerInfo
// ---------------------------------------------------------------------------

#[tokio::test]
async fn server_info_uses_the_first_advertised_authorization_server() {
    let fetch = Arc::new(ScriptedFetch::new(vec![
        (
            "https://x.example/.well-known/oauth-protected-resource/mcp",
            200,
            r#"{"resource":"https://x.example/mcp","authorization_servers":["https://as.example","https://other.example"]}"#,
        ),
        (
            "https://as.example/.well-known/oauth-authorization-server",
            200,
            r#"{"issuer":"https://as.example","authorization_endpoint":"https://as.example/authorize","token_endpoint":"https://as.example/token","response_types_supported":["code"]}"#,
        ),
    ]));
    let info = discover_oauth_server_info(
        "https://x.example/mcp",
        &DiscoverOAuthServerInfoOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(info.authorization_server_url, "https://as.example");
    assert_eq!(
        info.authorization_server_metadata
            .as_ref()
            .and_then(AuthorizationServerMetadata::issuer),
        Some("https://as.example")
    );
    assert_eq!(
        info.resource_metadata
            .as_ref()
            .and_then(OAuthProtectedResourceMetadata::resource),
        Some("https://x.example/mcp")
    );
    assert!(
        fetch
            .urls()
            .contains(&"https://as.example/.well-known/oauth-authorization-server".to_owned())
    );
}

#[tokio::test]
async fn server_info_falls_back_to_the_server_origin() {
    let fetch = Arc::new(ScriptedFetch::new(vec![
        (
            "https://x.example/.well-known/oauth-protected-resource/mcp",
            404,
            "",
        ),
        (
            "https://x.example/.well-known/oauth-protected-resource",
            200,
            r#"{"resource":"https://x.example"}"#,
        ),
        (
            "https://x.example/.well-known/oauth-authorization-server",
            404,
            "",
        ),
        (
            "https://x.example/.well-known/openid-configuration",
            404,
            "",
        ),
    ]));
    let info = discover_oauth_server_info(
        "https://x.example/mcp",
        &DiscoverOAuthServerInfoOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(info.authorization_server_url, "https://x.example/");
    assert!(info.authorization_server_metadata.is_none());
    assert!(info.resource_metadata.is_some());
}

#[tokio::test]
async fn server_info_swallows_non_type_errors_from_resource_discovery() {
    // 资源元数据非法 → 被吞掉；授权服务器发现继续（用 origin）。
    let fetch = Arc::new(ScriptedFetch::new(vec![
        ("https://x.example/", 200, r#"{"resource":5}"#),
        (
            "https://x.example/.well-known/oauth-authorization-server",
            404,
            "",
        ),
        (
            "https://x.example/.well-known/openid-configuration",
            200,
            r#"{"issuer":"https://x.example","authorization_endpoint":"https://x.example/authorize","token_endpoint":"https://x.example/token","response_types_supported":["code"]}"#,
        ),
    ]));
    let info = discover_oauth_server_info(
        "https://x.example/",
        &DiscoverOAuthServerInfoOptions {
            fetch: Some(fetch),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(info.resource_metadata.is_none());
    assert_eq!(info.authorization_server_url, "https://x.example/");
    assert!(info.authorization_server_metadata.is_some());
}

#[tokio::test]
async fn server_info_propagates_network_type_errors_from_resource_discovery() {
    let fetch = ScriptedFetch::new(Vec::new()).with_network_errors(vec![
        "https://x.example/.well-known/oauth-protected-resource",
    ]);
    let error = discover_oauth_server_info(
        "https://x.example/",
        &DiscoverOAuthServerInfoOptions {
            fetch: Some(shared(fetch)),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(error.is_type_error());
}

#[tokio::test]
async fn server_info_uses_a_configured_metadata_document_as_is() {
    let fetch = Arc::new(ScriptedFetch::new(vec![
        (
            "https://x.example/.well-known/oauth-protected-resource/mcp",
            200,
            r#"{"resource":"https://x.example/mcp","authorization_servers":["https://x.example"]}"#,
        ),
        (
            "https://idp.example/metadata.json",
            200,
            r#"{"issuer":"https://idp.example","authorization_endpoint":"https://idp.example/authorize","token_endpoint":"https://idp.example/token","response_types_supported":["code"]}"#,
        ),
    ]));
    let info = discover_oauth_server_info(
        "https://x.example/mcp",
        &DiscoverOAuthServerInfoOptions {
            authorization_server_metadata_url: Some(
                url::Url::parse("https://idp.example/metadata.json").unwrap(),
            ),
            fetch: Some(fetch.clone()),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap();
    // 配置的文档不校验 issuer：issuer 是 `https://idp.example`，与 URL origin 不同也照用。
    assert_eq!(info.authorization_server_url, "https://idp.example");
    assert_eq!(
        info.authorization_server_metadata
            .as_ref()
            .and_then(AuthorizationServerMetadata::issuer),
        Some("https://idp.example")
    );
    // 只请求受保护资源元数据与配置的文档，不做授权服务器发现。
    assert_eq!(
        fetch.urls(),
        vec![
            "https://x.example/.well-known/oauth-protected-resource/mcp",
            "https://idp.example/metadata.json",
        ]
    );
    // 配置文档的请求用 LATEST_PROTOCOL_VERSION。
    let requests = fetch.requests();
    assert_eq!(
        requests[1].protocol_version.as_deref(),
        Some(LATEST_PROTOCOL_VERSION)
    );
}

#[tokio::test]
async fn server_info_reports_configured_metadata_document_failures() {
    let fetch = ScriptedFetch::new(vec![(
        "https://x.example/",
        200,
        r#"{"resource":"https://x.example"}"#,
    )]);
    let error = discover_oauth_server_info(
        "https://x.example/",
        &DiscoverOAuthServerInfoOptions {
            authorization_server_metadata_url: Some(
                url::Url::parse("https://idp.example/metadata.json").unwrap(),
            ),
            fetch: Some(shared(fetch)),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        message(error),
        "HTTP 404 loading authorization server metadata from https://idp.example/metadata.json"
    );
}

#[tokio::test]
async fn server_info_skips_resource_discovery_with_an_explicit_metadata_url() {
    let fetch = Arc::new(ScriptedFetch::new(vec![
        (
            "https://meta.example/doc",
            200,
            r#"{"resource":"https://x.example/mcp"}"#,
        ),
        (
            "https://x.example/.well-known/oauth-authorization-server",
            404,
            "",
        ),
        (
            "https://x.example/.well-known/openid-configuration",
            404,
            "",
        ),
    ]));
    let info = discover_oauth_server_info(
        "https://x.example/mcp",
        &DiscoverOAuthServerInfoOptions {
            resource_metadata_url: Some(url::Url::parse("https://meta.example/doc").unwrap()),
            fetch: Some(fetch.clone()),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(info.authorization_server_url, "https://x.example/");
    assert_eq!(fetch.urls()[0], "https://meta.example/doc");
}

// ---------------------------------------------------------------------------
// 已知边界（本机 Node 26 实测，见 probe_more*.mjs 的结论）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn server_info_falls_back_to_the_origin_when_authorization_servers_is_empty() {
    let fetch = Arc::new(ScriptedFetch::new(vec![
        (
            "https://x.example/.well-known/oauth-protected-resource/mcp",
            200,
            r#"{"resource":"https://x.example/mcp","authorization_servers":[]}"#,
        ),
        (
            "https://x.example/.well-known/oauth-authorization-server",
            200,
            r#"{"issuer":"https://x.example","authorization_endpoint":"https://x.example/a","token_endpoint":"https://x.example/t","response_types_supported":["code"]}"#,
        ),
    ]));
    let info = discover_oauth_server_info(
        "https://x.example/mcp",
        &DiscoverOAuthServerInfoOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(info.authorization_server_url, "https://x.example/");
    assert!(info.resource_metadata.is_some());
    assert_eq!(
        fetch.urls(),
        vec![
            "https://x.example/.well-known/oauth-protected-resource/mcp",
            "https://x.example/.well-known/oauth-authorization-server",
        ]
    );
}

#[tokio::test]
async fn server_info_swallows_invalid_authorization_servers_entries() {
    // `authorization_servers: ["not a url"]` 让资源元数据解析失败（Invalid authorization server URL）；
    // 该普通 Error 被吞掉 → 回退到 origin。
    let fetch = Arc::new(ScriptedFetch::new(vec![
        (
            "https://x.example/.well-known/oauth-protected-resource/mcp",
            200,
            r#"{"resource":"https://x.example/mcp","authorization_servers":["not a url"]}"#,
        ),
        (
            "https://x.example/.well-known/oauth-authorization-server",
            404,
            "",
        ),
        (
            "https://x.example/.well-known/openid-configuration",
            404,
            "",
        ),
    ]));
    let info = discover_oauth_server_info(
        "https://x.example/mcp",
        &DiscoverOAuthServerInfoOptions {
            fetch: Some(fetch),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(info.resource_metadata.is_none());
    assert_eq!(info.authorization_server_url, "https://x.example/");
}

#[tokio::test]
async fn server_info_swallows_a_non_json_metadata_body() {
    // `response.json()` 的 SyntaxError 不是 TypeError → 被吞掉。
    let fetch = Arc::new(ScriptedFetch::new(vec![
        (
            "https://x.example/.well-known/oauth-protected-resource/mcp",
            200,
            "not json",
        ),
        (
            "https://x.example/.well-known/oauth-authorization-server",
            404,
            "",
        ),
        (
            "https://x.example/.well-known/openid-configuration",
            404,
            "",
        ),
    ]));
    let info = discover_oauth_server_info(
        "https://x.example/mcp",
        &DiscoverOAuthServerInfoOptions {
            fetch: Some(fetch),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(info.resource_metadata.is_none());
}

#[tokio::test]
async fn server_info_rejects_non_special_server_urls_with_a_type_error() {
    // `new URL('/', 'null')` 在 TS 里抛 TypeError: Invalid URL（orgin 为 "null"）。
    for server_url in ["com.example.app:/callback", "not a url"] {
        let fetch = Arc::new(ScriptedFetch::new(Vec::new()));
        let error = discover_oauth_server_info(
            server_url,
            &DiscoverOAuthServerInfoOptions {
                fetch: Some(fetch.clone()),
                ..DiscoverOAuthServerInfoOptions::default()
            },
        )
        .await
        .unwrap_err();
        assert!(error.is_type_error(), "{server_url}");
        assert_eq!(error.name(), "TypeError");
        assert!(
            fetch.urls().is_empty(),
            "{server_url} must not send a request"
        );
    }
}

#[tokio::test]
async fn server_info_keeps_userinfo_in_the_origin_fallback() {
    // TS: `String(new URL("/", serverUrl))` 保留 userinfo 与端口。
    let fetch = Arc::new(ScriptedFetch::new(Vec::new()));
    let info = discover_oauth_server_info(
        "https://user:pass@x.example:8443/a/b",
        &DiscoverOAuthServerInfoOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        info.authorization_server_url,
        "https://user:pass@x.example:8443/"
    );
    // 发现请求不带 userinfo（`Url::origin()` 的序列化结果）。
    assert_eq!(
        fetch.urls(),
        vec![
            "https://x.example:8443/.well-known/oauth-protected-resource/a/b",
            "https://x.example:8443/.well-known/oauth-protected-resource",
            "https://x.example:8443/.well-known/oauth-authorization-server",
            "https://x.example:8443/.well-known/openid-configuration",
        ]
    );
}

#[tokio::test]
async fn server_info_keeps_the_ipv6_host_in_the_origin_fallback() {
    let fetch = Arc::new(ScriptedFetch::new(Vec::new()));
    let info = discover_oauth_server_info(
        "http://[::1]:1234/mcp",
        &DiscoverOAuthServerInfoOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(info.authorization_server_url, "http://[::1]:1234/");
    assert_eq!(
        fetch.urls()[0],
        "http://[::1]:1234/.well-known/oauth-protected-resource/mcp"
    );
}

#[tokio::test]
async fn select_resource_rejects_metadata_without_a_usable_resource() {
    // 实测 TS：`new URL(metadata.resource)` 对 `undefined`/数字/`null` 都是 `TypeError: Invalid URL`。
    for resource in [Value::Null, json!(5), json!("undefined")] {
        let mut metadata = OAuthProtectedResourceMetadata::new();
        metadata.set("resource", resource.clone());
        let error = select_resource("https://x.example/mcp", Some(&metadata)).unwrap_err();
        assert!(error.is_type_error(), "resource = {resource}");
        assert_eq!(error.message(), "Invalid URL");
    }
    let metadata = OAuthProtectedResourceMetadata::new();
    assert!(
        select_resource("https://x.example/mcp", Some(&metadata))
            .unwrap_err()
            .is_type_error()
    );
    // 服务器 URL 先求值：非法 serverUrl 直接 TypeError，与 metadata 无关。
    let mut metadata = OAuthProtectedResourceMetadata::new();
    metadata.set("resource", json!("https://x.example/mcp"));
    let error = select_resource("not a url", Some(&metadata)).unwrap_err();
    assert!(error.is_type_error());
    assert_eq!(error.message(), "Invalid URL");
}

// ---------------------------------------------------------------------------
// `new URL()` 修正对发现请求的影响（期望值由 TS 参照实现 `tsprobe` 实测得出）
// ---------------------------------------------------------------------------

/// `https://x.example//mcp` 的第三个候选是协议相对引用：TS 请求 `https://mcp/...`，
/// 服务端在该主机上给出的 issuer 不匹配 → `OAuthIssuerMismatchError`。
#[tokio::test]
async fn authorization_server_discovery_follows_a_protocol_relative_candidate() {
    let fetch = Arc::new(ScriptedFetch::new(vec![
        (
            "https://mcp/.well-known/openid-configuration",
            200,
            r#"{"issuer":"https://mcp","authorization_endpoint":"https://mcp/auth","token_endpoint":"https://mcp/token","response_types_supported":["code"]}"#,
        ),
        (
            "https://x.example//mcp/.well-known/openid-configuration",
            200,
            r#"{"issuer":"https://x.example//mcp","authorization_endpoint":"https://x.example/auth","token_endpoint":"https://x.example/token","response_types_supported":["code"]}"#,
        ),
    ]));
    let error = discover_authorization_server_metadata(
        "https://x.example//mcp",
        &DiscoverAuthorizationServerOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverAuthorizationServerOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.name(), "OAuthIssuerMismatchError");
    assert_eq!(
        error.message(),
        "OAuth issuer mismatch: expected \"https://x.example//mcp\", received \"https://mcp\""
    );
    assert_eq!(
        fetch.urls(),
        vec![
            "https://x.example/.well-known/oauth-authorization-server//mcp",
            "https://x.example/.well-known/openid-configuration//mcp",
            "https://mcp/.well-known/openid-configuration",
        ]
    );

    // 只把文档放在 `x.example//mcp` 上时，TS 从不请求它（第三个候选已换到 `mcp` 主机），
    // 全部候选都是 discovery miss → `None`。
    let fetch = Arc::new(ScriptedFetch::new(vec![(
        "https://x.example//mcp/.well-known/openid-configuration",
        200,
        r#"{"issuer":"https://x.example//mcp","authorization_endpoint":"https://x.example/auth","token_endpoint":"https://x.example/token","response_types_supported":["code"]}"#,
    )]));
    assert_eq!(
        discover_authorization_server_metadata(
            "https://x.example//mcp",
            &DiscoverAuthorizationServerOptions {
                fetch: Some(fetch.clone()),
                ..DiscoverAuthorizationServerOptions::default()
            },
        )
        .await
        .unwrap(),
        None
    );
    assert!(
        !fetch
            .urls()
            .contains(&"https://x.example//mcp/.well-known/openid-configuration".to_owned())
    );
}

/// 受保护资源发现对 `//mcp` 路径先把后缀原样拼进 well-known 路径，回退到裸路径后仍 404。
#[tokio::test]
async fn protected_resource_discovery_keeps_a_double_slash_suffix() {
    let fetch = Arc::new(ScriptedFetch::new(Vec::new()));
    let error = discover_protected_resource_metadata(
        "https://x.example//mcp",
        &DiscoverProtectedResourceOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverProtectedResourceOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.message(),
        "HTTP 404 loading OAuth protected resource metadata"
    );
    assert_eq!(
        fetch.urls(),
        vec![
            "https://x.example/.well-known/oauth-protected-resource//mcp",
            "https://x.example/.well-known/oauth-protected-resource",
        ]
    );
}

/// `discoverOAuthServerInfo` 会按资源元数据里广告的 `//` issuer 去发现（换到 `mcp` 主机）。
#[tokio::test]
async fn server_info_queries_a_protocol_relative_advertised_issuer() {
    let fetch = Arc::new(ScriptedFetch::new(vec![(
        "https://x.example/.well-known/oauth-protected-resource/mcp",
        200,
        r#"{"resource":"https://x.example","authorization_servers":["https://x.example//mcp"]}"#,
    )]));
    let info = discover_oauth_server_info(
        "https://x.example/mcp",
        &DiscoverOAuthServerInfoOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(info.authorization_server_url, "https://x.example//mcp");
    assert!(info.authorization_server_metadata.is_none());
    assert_eq!(
        fetch.urls(),
        vec![
            "https://x.example/.well-known/oauth-protected-resource/mcp",
            "https://x.example/.well-known/oauth-authorization-server//mcp",
            "https://x.example/.well-known/openid-configuration//mcp",
            "https://mcp/.well-known/openid-configuration",
        ]
    );
}

/// WHATWG 把特殊 scheme 路径里的 `^` 编码成 `%5E`：请求 URL、issuer 比较与回退都以它为准。
#[tokio::test]
async fn discovery_urls_encode_a_caret_in_special_scheme_paths() {
    let fetch = Arc::new(ScriptedFetch::new(vec![(
        "https://x.example/.well-known/oauth-protected-resource/a%5Eb",
        200,
        r#"{"resource":"https://x.example/a%5Eb"}"#,
    )]));
    let metadata = discover_protected_resource_metadata(
        "https://x.example/a^b",
        &DiscoverProtectedResourceOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverProtectedResourceOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(metadata.resource(), Some("https://x.example/a%5Eb"));
    assert_eq!(
        fetch.urls(),
        vec!["https://x.example/.well-known/oauth-protected-resource/a%5Eb"]
    );

    // 404 → 回退到裸路径；元数据里回写的是服务器给的 `^` 原文。
    let fetch = Arc::new(ScriptedFetch::new(vec![(
        "https://x.example/.well-known/oauth-protected-resource",
        200,
        r#"{"resource":"https://x.example/a^b"}"#,
    )]));
    let metadata = discover_protected_resource_metadata(
        "https://x.example/a^b",
        &DiscoverProtectedResourceOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverProtectedResourceOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(metadata.resource(), Some("https://x.example/a^b"));
    assert_eq!(
        fetch.urls(),
        vec![
            "https://x.example/.well-known/oauth-protected-resource/a%5Eb",
            "https://x.example/.well-known/oauth-protected-resource",
        ]
    );

    // issuer 比较用的是原样的 `authorizationServerUrl` 字符串（`^` 不被规范化）。
    let fetch = Arc::new(ScriptedFetch::new(vec![(
        "https://x.example/.well-known/oauth-authorization-server/a%5Eb",
        200,
        r#"{"issuer":"https://x.example/a%5Eb","authorization_endpoint":"https://x.example/auth","token_endpoint":"https://x.example/token","response_types_supported":["code"]}"#,
    )]));
    let error = discover_authorization_server_metadata(
        "https://x.example/a^b",
        &DiscoverAuthorizationServerOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverAuthorizationServerOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.message(),
        "OAuth issuer mismatch: expected \"https://x.example/a^b\", received \"https://x.example/a%5Eb\""
    );
    assert_eq!(
        fetch.urls(),
        vec!["https://x.example/.well-known/oauth-authorization-server/a%5Eb"]
    );

    // `discoverOAuthServerInfo` 的 origin 回退对 `^` 路径不受影响。
    let fetch = Arc::new(ScriptedFetch::new(Vec::new()));
    let info = discover_oauth_server_info(
        "https://x.example/a^b",
        &DiscoverOAuthServerInfoOptions {
            fetch: Some(fetch.clone()),
            ..DiscoverOAuthServerInfoOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(info.authorization_server_url, "https://x.example/");
    assert_eq!(
        fetch.urls(),
        vec![
            "https://x.example/.well-known/oauth-protected-resource/a%5Eb",
            "https://x.example/.well-known/oauth-protected-resource",
            "https://x.example/.well-known/oauth-authorization-server",
            "https://x.example/.well-known/openid-configuration",
        ]
    );
}
