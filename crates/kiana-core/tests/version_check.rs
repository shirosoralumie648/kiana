//! `utils/version-check.ts` 的移植测试（`test/version-check.test.ts`）。
//!
//! TS 用 `vi.stubGlobal("fetch")`；Rust 侧用 wiremock + 文档化的
//! `set_latest_version_url_for_tests` 覆盖 `KIANA_API_BASE_URL/api/latest-version`。
//! `KIANA_OFFLINE` / `KIANA_SKIP_VERSION_CHECK` 与 URL 覆盖都是进程级全局状态，
//! 本文件所有测试共用一把锁串行执行。

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kiana_core::changelog::{
    LatestKianaRelease, VersionCheckOptions, check_for_new_kiana_version, compare_package_versions,
    format_version_check_error, get_latest_kiana_release, get_latest_kiana_version,
    is_newer_package_version, set_latest_version_url_for_tests,
};
use kiana_core::management_http::{FetchError, FetchErrorCause};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static TEST_LOCK: Mutex<()> = Mutex::new(());

// `KIANA_OFFLINE`/`KIANA_SKIP_VERSION_CHECK` 之外也清掉代理变量：共享 client 首次构造时读环境，
// 本地 wiremock 请求不应经过宿主代理。
const ENV_KEYS: [&str; 9] = [
    "KIANA_OFFLINE",
    "KIANA_SKIP_VERSION_CHECK",
    "KIANA_API_BASE_URL",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "http_proxy",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
];

fn lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn clear(keys: &[&'static str]) -> Self {
        let saved = keys
            .iter()
            .map(|key| (*key, std::env::var_os(key)))
            .collect::<Vec<_>>();
        for key in keys {
            // SAFETY: 本文件所有测试持有 `TEST_LOCK`。
            unsafe { std::env::remove_var(key) };
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                // SAFETY: 测试串行化。
                Some(value) => unsafe { std::env::set_var(key, value) },
                // SAFETY: 测试串行化。
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

struct UrlGuard;

impl UrlGuard {
    fn none() -> Self {
        set_latest_version_url_for_tests(None);
        Self
    }
}

impl Drop for UrlGuard {
    fn drop(&mut self) {
        set_latest_version_url_for_tests(None);
    }
}

fn endpoint(server: &MockServer) -> String {
    format!("{}/api/latest-version", server.uri())
}

/// 原始 TCP 服务：前 `failures` 个连接建立后立刻关闭（等价 TS 里 rejecting 的 `fetch`），
/// 之后返回带 `body` 的 200 响应。返回 `(url, 连接计数)`。
fn spawn_flaky_http_server(failures: usize, body: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind flaky server");
    let address = listener.local_addr().expect("flaky addr");
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let index = counter.fetch_add(1, Ordering::SeqCst);
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            if index < failures {
                // 直接关闭：reqwest 报传输错误，`fetchWithRetry` 需要重试。
                continue;
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://{address}/api/latest-version"), connections)
}

/// 声明 `content-length: 100` 但只发几个字节就关闭（undici：`terminated`）。
fn spawn_truncated_body_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind truncated server");
    let address = listener.local_addr().expect("truncated addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\nconnection: close\r\n\r\n{\"version\":",
            );
            let _ = stream.flush();
        }
    });
    format!("http://{address}/api/latest-version")
}

/// 及时返回响应头，但 body 一直挂着（TS 的 `AbortSignal.timeout` 必须中止 body 读取）。
fn spawn_stalled_body_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stalled server");
    let address = listener.local_addr().expect("stalled addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\nconnection: close\r\n\r\n{",
            );
            let _ = stream.flush();
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    });
    format!("http://{address}/api/latest-version")
}

async fn mount_json(server: &MockServer, value: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/api/latest-version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(value))
        .mount(server)
        .await;
}

#[test]
fn compares_package_versions() {
    assert_eq!(compare_package_versions("0.70.6", "0.70.5"), Some(1));
    assert_eq!(compare_package_versions("0.70.5", "0.70.5"), Some(0));
    assert_eq!(compare_package_versions("0.70.4", "0.70.5"), Some(-1));
    assert_eq!(
        compare_package_versions("5.0.0-beta.20", "5.0.0-beta.9"),
        Some(1)
    );
    // node-semver 的 `valid()` 接受单个小写前导 v，拒绝 `=` 前缀。
    assert_eq!(compare_package_versions("v1.2.3", "1.2.3"), Some(0));
    assert_eq!(compare_package_versions("=1.2.3", "1.2.3"), None);
    // node-semver 的 compare 忽略 build metadata（含前导零的 build 也合法）。
    assert_eq!(compare_package_versions("1.2.3+build", "1.2.3"), Some(0));
    assert_eq!(compare_package_versions("1.2.3+01", "1.2.3"), Some(0));
    // node-semver 拒绝大于 Number.MAX_SAFE_INTEGER 的数字段。
    assert_eq!(
        compare_package_versions("9007199254740992.0.0", "1.0.0"),
        None
    );
    assert_eq!(
        compare_package_versions("9007199254740991.0.0", "1.0.0"),
        Some(1)
    );
    assert_eq!(compare_package_versions("not-a-version", "1.2.3"), None);
    // node-semver 的 MAX_LENGTH=256：超长输入在正则匹配前抛错 → `valid()` 为 null。
    let too_long = format!("1.2.3-{}", "a".repeat(251));
    assert_eq!(too_long.len(), 257);
    assert_eq!(compare_package_versions(&too_long, "1.2.3"), None);
    // 恰好 256 个字符仍然合法（边界）。
    let max_length = format!("1.2.3-{}", "a".repeat(250));
    assert_eq!(max_length.len(), 256);
    assert_eq!(compare_package_versions(&max_length, "1.2.3"), Some(-1));
    // 数字 prerelease 标识符按 JS Number（f64）比较：2^53 以上丢失精度后相等。
    assert_eq!(
        compare_package_versions("1.0.0-9007199254740993", "1.0.0-9007199254740992"),
        Some(0)
    );
    assert_eq!(
        compare_package_versions("1.0.0-18446744073709551616", "1.0.0-18446744073709551617"),
        Some(0)
    );
    // 数字标识符小于字母数字标识符；字母标识符按 ASCII 比较。
    assert_eq!(compare_package_versions("1.0.0-1", "1.0.0-alpha"), Some(-1));
    assert_eq!(
        compare_package_versions("1.0.0-alpha", "1.0.0-beta"),
        Some(-1)
    );
    assert_eq!(compare_package_versions("1.0.0-alpha", "1.0.0"), Some(-1));
    // 短 prerelease 序列更小（`a === b` 后一方先结束）。
    assert_eq!(
        compare_package_versions("1.0.0-alpha", "1.0.0-alpha.1"),
        Some(-1)
    );
    assert!(!is_newer_package_version("0.70.5", "0.70.5"));
    assert!(is_newer_package_version("0.70.6", "0.70.5"));
    // 不可比较时回退到 trim 后字符串不等。
    assert!(is_newer_package_version("foo", "bar"));
    assert!(!is_newer_package_version(" foo ", "foo"));
    // 超长版本在 TS 里 `valid()` 为 null → 回退到字符串比较：1.2.3-<251 a> 被当作“更新”，
    // 而不是 semver 意义上的更旧。
    assert!(is_newer_package_version(&too_long, "2.0.0"));
}

#[derive(serde::Deserialize)]
struct VersionCompareCase {
    left: String,
    right: String,
    expected: Option<i32>,
}

/// 黄金用例：用 TS 参考实现（`semver` npm 包的 `valid()` + `compare()`，
/// 即 `version-check.ts#comparePackageVersions`）生成，覆盖 MAX_LENGTH、2^53 精度、
/// prerelease 次序与无效输入。
#[test]
fn matches_the_node_semver_golden_fixture() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/version_compare.json"
    );
    let raw = std::fs::read_to_string(path).expect("read version compare fixture");
    let cases: Vec<VersionCompareCase> =
        serde_json::from_str(&raw).expect("parse version compare fixture");
    assert_eq!(cases.len(), 67);
    for case in cases {
        assert_eq!(
            compare_package_versions(&case.left, &case.right),
            case.expected,
            "comparePackageVersions({:?}, {:?})",
            case.left,
            case.right
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // 全局 env/URL 覆盖必须串行；block_on 在测试线程上执行。
async fn skips_when_no_version_endpoint_is_configured() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();

    assert_eq!(
        get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
            .await
            .expect("an unconfigured endpoint is not an error"),
        None
    );
    assert!(!kiana_core::changelog::is_version_check_configured());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // 全局 env/URL 覆盖必须串行；block_on 在测试线程上执行。
async fn returns_only_newer_versions() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();
    let server = MockServer::start().await;
    mount_json(&server, json!({ "version": "1.2.3" })).await;
    set_latest_version_url_for_tests(Some(endpoint(&server)));

    assert_eq!(check_for_new_kiana_version("1.2.3").await, None);
    assert_eq!(
        check_for_new_kiana_version("1.2.2").await,
        Some(LatestKianaRelease {
            version: "1.2.3".to_string(),
            package_name: None,
            note: None,
        })
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn uses_the_version_check_api_with_a_kiana_user_agent() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();
    let server = MockServer::start().await;
    mount_json(&server, json!({ "version": "1.2.4" })).await;
    set_latest_version_url_for_tests(Some(endpoint(&server)));

    assert_eq!(
        get_latest_kiana_version("1.2.3", &VersionCheckOptions::default())
            .await
            .expect("request succeeds"),
        Some("1.2.4".to_string())
    );

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    let user_agent = requests[0]
        .headers
        .get("user-agent")
        .expect("user-agent header")
        .to_str()
        .expect("ascii");
    assert!(
        user_agent.starts_with("kiana/1.2.3 "),
        "unexpected user agent {user_agent}"
    );
    assert_eq!(
        requests[0]
            .headers
            .get("accept")
            .expect("accept header")
            .to_str()
            .expect("ascii"),
        "application/json"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn retries_a_transient_version_request_when_explicitly_requested() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();
    // TS：fetch 连续两次 reject（传输失败），第三次成功 → 共 3 次调用。
    let (url, connections) = spawn_flaky_http_server(2, r#"{"version":"1.2.4"}"#);
    set_latest_version_url_for_tests(Some(url));

    let release = get_latest_kiana_release(
        "1.2.3",
        &VersionCheckOptions {
            retry: Some(true),
            ..VersionCheckOptions::default()
        },
    )
    .await
    .expect("request succeeds after two transport failures");
    assert_eq!(
        release,
        Some(LatestKianaRelease {
            version: "1.2.4".to_string(),
            package_name: None,
            note: None,
        })
    );
    assert_eq!(connections.load(Ordering::SeqCst), 3);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn keeps_automatic_version_checks_to_one_request() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();
    // TS：fetch 一直 reject；自动检查只调用一次并吞掉异常。
    let (url, connections) = spawn_flaky_http_server(usize::MAX, "");
    set_latest_version_url_for_tests(Some(url));

    assert_eq!(check_for_new_kiana_version("1.2.3").await, None);
    assert_eq!(connections.load(Ordering::SeqCst), 1);
}

#[test]
fn formats_nested_network_error_details() {
    let error = FetchError::Transport {
        message: "fetch failed".to_string(),
        cause: Some(vec![
            FetchErrorCause {
                message: "connect timeout".to_string(),
                code: Some("ETIMEDOUT".to_string()),
            },
            FetchErrorCause {
                message: "network unreachable".to_string(),
                code: Some("ENETUNREACH".to_string()),
            },
        ]),
    };
    assert_eq!(
        format_version_check_error(&error),
        "fetch failed (ETIMEDOUT, ENETUNREACH)"
    );

    // code 去重、保持首次出现顺序。
    let duplicated = FetchError::Transport {
        message: "fetch failed".to_string(),
        cause: Some(vec![
            FetchErrorCause {
                message: "a".to_string(),
                code: Some("ECONNRESET".to_string()),
            },
            FetchErrorCause {
                message: "b".to_string(),
                code: Some("ECONNRESET".to_string()),
            },
        ]),
    };
    assert_eq!(
        format_version_check_error(&duplicated),
        "fetch failed (ECONNRESET)"
    );

    // 无 code → 第一条非空 message 走 cause 分支。
    let with_message = FetchError::Transport {
        message: "fetch failed".to_string(),
        cause: Some(vec![FetchErrorCause {
            message: "connect ECONNREFUSED 127.0.0.1:1".to_string(),
            code: None,
        }]),
    };
    assert_eq!(
        format_version_check_error(&with_message),
        "fetch failed (cause: connect ECONNREFUSED 127.0.0.1:1)"
    );

    // 没有 cause → 只有根消息。
    assert_eq!(
        format_version_check_error(&FetchError::Timeout),
        "The operation was aborted due to timeout"
    );
    assert_eq!(
        format_version_check_error(&FetchError::Aborted),
        "This operation was aborted"
    );
    assert_eq!(
        format_version_check_error(&FetchError::transport("plain", None)),
        "plain"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn returns_the_active_package_metadata_from_the_version_check_api() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();
    let server = MockServer::start().await;
    mount_json(
        &server,
        json!({ "packageName": "@new-scope/kiana", "version": "1.2.4" }),
    )
    .await;
    set_latest_version_url_for_tests(Some(endpoint(&server)));

    assert_eq!(
        get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
            .await
            .expect("request succeeds"),
        Some(LatestKianaRelease {
            version: "1.2.4".to_string(),
            package_name: Some("@new-scope/kiana".to_string()),
            note: None,
        })
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn returns_trimmed_update_notes_from_the_version_check_api() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();
    let server = MockServer::start().await;
    mount_json(
        &server,
        json!({ "note": " **Read this** ", "version": "1.2.4" }),
    )
    .await;
    set_latest_version_url_for_tests(Some(endpoint(&server)));

    assert_eq!(
        get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
            .await
            .expect("request succeeds"),
        Some(LatestKianaRelease {
            version: "1.2.4".to_string(),
            package_name: None,
            note: Some("**Read this**".to_string()),
        })
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn skips_automatic_api_calls_when_version_checks_are_disabled() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();
    let server = MockServer::start().await;
    mount_json(&server, json!({ "version": "1.2.4" })).await;
    set_latest_version_url_for_tests(Some(endpoint(&server)));
    // SAFETY: 测试串行化。
    unsafe { std::env::set_var("KIANA_SKIP_VERSION_CHECK", "1") };

    assert_eq!(check_for_new_kiana_version("1.2.3").await, None);
    assert_eq!(server.received_requests().await.expect("requests").len(), 0);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn allows_direct_api_calls_when_automatic_version_checks_are_disabled() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();
    let server = MockServer::start().await;
    mount_json(&server, json!({ "version": "1.2.4" })).await;
    set_latest_version_url_for_tests(Some(endpoint(&server)));
    // SAFETY: 测试串行化。
    unsafe { std::env::set_var("KIANA_SKIP_VERSION_CHECK", "1") };

    assert_eq!(
        get_latest_kiana_version("1.2.3", &VersionCheckOptions::default())
            .await
            .expect("request succeeds"),
        Some("1.2.4".to_string())
    );
    assert_eq!(server.received_requests().await.expect("requests").len(), 1);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn kiana_offline_uses_javascript_truthiness() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();
    let server = MockServer::start().await;
    mount_json(&server, json!({ "version": "1.2.4" })).await;
    set_latest_version_url_for_tests(Some(endpoint(&server)));

    // 任意非空值（含 "0"）都禁用版本检查。
    // SAFETY: 测试串行化。
    unsafe { std::env::set_var("KIANA_OFFLINE", "0") };
    assert_eq!(
        get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
            .await
            .expect("offline is not an error"),
        None
    );
    assert_eq!(server.received_requests().await.expect("requests").len(), 0);

    // 空值视为未设置（TS 里 `""` 是假值）。
    // SAFETY: 测试串行化。
    unsafe { std::env::set_var("KIANA_OFFLINE", "") };
    assert_eq!(
        get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
            .await
            .expect("request succeeds"),
        Some(LatestKianaRelease {
            version: "1.2.4".to_string(),
            package_name: None,
            note: None,
        })
    );
    assert_eq!(server.received_requests().await.expect("requests").len(), 1);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn returns_none_for_non_ok_responses() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/latest-version"))
        .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
        .mount(&server)
        .await;
    set_latest_version_url_for_tests(Some(endpoint(&server)));

    assert_eq!(
        get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
            .await
            .expect("http error status is not an exception"),
        None
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn ignores_payloads_without_a_non_empty_version_string() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();

    for payload in [
        json!({}),
        json!({ "version": "" }),
        json!({ "version": "   " }),
        json!({ "version": 42 }),
        json!([1, 2, 3]),
        json!("1.2.4"),
    ] {
        let server = MockServer::start().await;
        mount_json(&server, payload.clone()).await;
        set_latest_version_url_for_tests(Some(endpoint(&server)));
        assert_eq!(
            get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
                .await
                .expect("shape issues are not exceptions"),
            None,
            "payload: {payload}"
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn propagates_v8_json_parse_errors() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();

    // 非 JSON 文本：V8 的 `Unexpected token ... is not valid JSON` 文案（ARCHITECTURE §4.3）。
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/latest-version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    set_latest_version_url_for_tests(Some(endpoint(&server)));

    let error = get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
        .await
        .expect_err("invalid json is an error");
    assert_eq!(
        error.to_string(),
        "Unexpected token 'o', \"not json\" is not valid JSON"
    );

    // 空 body：`Unexpected end of JSON input`。
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/latest-version"))
        .respond_with(ResponseTemplate::new(200).set_body_string(""))
        .mount(&server)
        .await;
    set_latest_version_url_for_tests(Some(endpoint(&server)));

    let error = get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
        .await
        .expect_err("empty body is an error");
    assert_eq!(error.to_string(), "Unexpected end of JSON input");

    // V8 接受 200 层嵌套（serde_json 在 128 层会拒绝）；没有 version 字段 → Ok(None)。
    let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/latest-version"))
        .respond_with(ResponseTemplate::new(200).set_body_string(deep))
        .mount(&server)
        .await;
    set_latest_version_url_for_tests(Some(endpoint(&server)));

    assert_eq!(
        get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
            .await
            .expect("V8 accepts 200-deep nesting"),
        None
    );
}

/// `response.json()` 的 body 读取失败在 TS 里是 `TypeError("terminated", {cause})`，
/// `formatVersionCheckError` 渲染为 `terminated (UND_ERR_RES_CONTENT_LENGTH_MISMATCH)`。
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn maps_truncated_response_bodies_to_undici_terminated() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();
    set_latest_version_url_for_tests(Some(spawn_truncated_body_server()));

    let error = get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
        .await
        .expect_err("truncated body is an error");
    assert_eq!(error.to_string(), "terminated");
    assert_eq!(
        format_version_check_error(&error),
        "terminated (UND_ERR_RES_CONTENT_LENGTH_MISMATCH)"
    );
}

/// `timeoutMs` 是覆盖 body 读取的总预算：响应头及时到达、body 挂起时必须报超时
/// （TS 的 `AbortSignal.timeout` 与 `response.json()` 的 AbortError）。
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn times_out_stalled_body_reads() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();
    set_latest_version_url_for_tests(Some(spawn_stalled_body_server()));

    let started = std::time::Instant::now();
    let error = get_latest_kiana_release(
        "1.2.3",
        &VersionCheckOptions {
            timeout_ms: Some(300),
            retry: None,
        },
    )
    .await
    .expect_err("stalled body times out");
    assert_eq!(error, FetchError::Timeout);
    assert_eq!(
        error.to_string(),
        "The operation was aborted due to timeout"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_millis(2000),
        "body read should stop at the timeout budget, took {:?}",
        started.elapsed()
    );
}

/// `formatVersionCheckError` 读取 `cause.code`：连接被拒 → `fetch failed (ECONNREFUSED)`，
/// DNS 失败 → `fetch failed (ENOTFOUND)`（与 Node 26 / undici 8.10.2 实测一致）。
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn maps_transport_errnos_like_node() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();

    // 取一个刚释放的回环端口（连接会被拒绝）。
    let closed = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = closed.local_addr().expect("addr");
    drop(closed);
    set_latest_version_url_for_tests(Some(format!("http://{address}/api/latest-version")));

    let error = get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
        .await
        .expect_err("refused connection is an error");
    assert_eq!(
        format_version_check_error(&error),
        "fetch failed (ECONNREFUSED)"
    );

    // RFC 2606 的 `.invalid` 顶级域：DNS 解析必定失败。
    set_latest_version_url_for_tests(Some(
        "http://nonexistent-host-zz.invalid/api/latest-version".to_string(),
    ));
    let error = get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
        .await
        .expect_err("dns failure is an error");
    assert_eq!(
        format_version_check_error(&error),
        "fetch failed (ENOTFOUND)"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn propagates_the_json_null_type_error() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let _url = UrlGuard::none();
    let server = MockServer::start().await;
    mount_json(&server, json!(null)).await;
    set_latest_version_url_for_tests(Some(endpoint(&server)));

    let error = get_latest_kiana_release("1.2.3", &VersionCheckOptions::default())
        .await
        .expect_err("null payload throws like TS");
    assert_eq!(
        error.to_string(),
        "Cannot read properties of null (reading 'version')"
    );
}
