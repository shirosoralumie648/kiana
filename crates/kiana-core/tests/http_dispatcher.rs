//! `core/http-dispatcher.ts` 的测试。
//!
//! TS 测试的 5 个用例里：
//! - 3 个 `applyHttpProxySettings` 用例直接移植；
//! - `tunnels proxied HTTP origins`（undici `proxyTunnel: true`，HTTP 源也发 `CONNECT`）在
//!   `reqwest` 下不可复刻：reqwest 对 HTTP 目标以 absolute-form 请求行直连代理，只有 HTTPS
//!   目标才 `CONNECT`。这里用本地代理确认**代理确实被使用**，并断言 absolute-form 的请求行
//!   （差异记录在模块文档）；
//! - `autoSelectFamilyAttemptTimeout = 2000` 在 `reqwest 0.13.5` 不可观测，不移植。
//!
//! `parse`/`format`/`CHOICES`/无效 timeout/`NO_PROXY`/共享 client 复用是新增测试（TS 没有）。
//! `NO_PROXY` 按 undici `EnvHttpProxyAgent` 的语义（`host:port`、`*.domain`、`[::1]:443`、
//! 空白分隔、CIDR 不匹配）由 `src/http_dispatcher.rs` 的单元测试覆盖；这里补两条端到端回归：
//! `NO_PROXY=127.0.0.1:<origin port>` 与 `NO_PROXY=::1`（IPv6 字面量）时请求必须**直连
//! origin**、不经过代理。
//!
//! 代理 URL 校验、SOCKS5 路由与 undici fetch 默认请求头是审计后新增的回归测试（TS 没有直接
//! 用例，但行为来自 undici `EnvHttpProxyAgent`/`ProxyAgent` 与 `undici.fetch`）。
//!
//! 环境变量与进程级共享 client 都是全局状态：本文件所有测试共用一把锁串行执行。

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kiana_core::http_dispatcher::{
    DEFAULT_HTTP_IDLE_TIMEOUT_MS, HTTP_IDLE_TIMEOUT_CHOICES, apply_http_proxy_settings,
    configure_http_dispatcher, format_http_idle_timeout_ms, parse_http_idle_timeout_ms,
    shared_http_client,
};
use serde_json::json;

static ENV_LOCK: Mutex<()> = Mutex::new(());

const PROXY_KEYS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "http_proxy",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
];

fn lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 保存并在作用域内移除一组环境变量。
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
            // SAFETY: 本文件所有测试持有 `ENV_LOCK`；这是启动期配置函数的测试替身。
            unsafe { std::env::remove_var(key) };
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                // SAFETY: 同 `EnvGuard::clear`（测试串行化）。
                Some(value) => unsafe { std::env::set_var(key, value) },
                // SAFETY: 同上。
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

#[test]
fn parses_http_idle_timeouts_like_javascript() {
    use parse_http_idle_timeout_ms as parse;

    assert_eq!(parse(&json!("300000")), Some(300_000.0));
    assert_eq!(parse(&json!(" 300000 ")), Some(300_000.0));
    assert_eq!(parse(&json!("disabled")), Some(0.0));
    assert_eq!(parse(&json!("DISABLED")), Some(0.0));
    assert_eq!(parse(&json!(" Disabled ")), Some(0.0));
    assert_eq!(parse(&json!("")), None);
    assert_eq!(parse(&json!("   ")), None);
    assert_eq!(parse(&json!("abc")), None);
    // ECMAScript Number(string)：0x/0b/0o、指数、无符号前缀。
    assert_eq!(parse(&json!("0x10")), Some(16.0));
    assert_eq!(parse(&json!("0b101")), Some(5.0));
    assert_eq!(parse(&json!("0o17")), Some(15.0));
    assert_eq!(parse(&json!("1e5")), Some(100_000.0));
    assert_eq!(parse(&json!("1.9")), Some(1.0));
    assert_eq!(parse(&json!("-5")), None);
    assert_eq!(parse(&json!("Infinity")), None);
    assert_eq!(parse(&json!("1e400")), None);
    assert_eq!(parse(&json!("+12")), Some(12.0));
    // 数字分支：有限且 >= 0，floor；其它类型 None。
    assert_eq!(parse(&json!(300_000)), Some(300_000.0));
    assert_eq!(parse(&json!(1500.75)), Some(1500.0));
    assert_eq!(parse(&json!(-1)), None);
    assert_eq!(parse(&json!(true)), None);
    assert_eq!(parse(&json!(null)), None);
    assert_eq!(parse(&json!([1])), None);
    assert_eq!(parse(&json!({"a": 1})), None);
    // JS 的 number 是 f64：Math.floor 不把 >= 2^63 的值截断到 i64::MAX（旧实现会）。
    assert_eq!(parse(&json!(1e19)), Some(1e19));
    assert_eq!(parse(&json!("1e19")), Some(1e19));
    assert_eq!(parse(&json!("1e300")), Some(1e300));
}

#[test]
fn formats_http_idle_timeouts_like_javascript() {
    assert_eq!(format_http_idle_timeout_ms(30_000.0), "30 sec");
    assert_eq!(format_http_idle_timeout_ms(60_000.0), "1 min");
    assert_eq!(format_http_idle_timeout_ms(120_000.0), "2 min");
    assert_eq!(format_http_idle_timeout_ms(300_000.0), "5 min");
    assert_eq!(format_http_idle_timeout_ms(0.0), "disabled");
    assert_eq!(format_http_idle_timeout_ms(45_000.0), "45 sec");
    assert_eq!(format_http_idle_timeout_ms(1_500.0), "1.5 sec");
    assert_eq!(format_http_idle_timeout_ms(100.0), "0.1 sec");
    assert_eq!(format_http_idle_timeout_ms(999.0), "0.999 sec");
    assert_eq!(format_http_idle_timeout_ms(-1_500.0), "-1.5 sec");
    // `${ms / 1000} sec` 用 JS Number::toString：1e19/1000 = 1e16，1e300/1000 = 1e+297。
    assert_eq!(format_http_idle_timeout_ms(1e19), "10000000000000000 sec");
    assert_eq!(format_http_idle_timeout_ms(1e300), "1e+297 sec");
}

#[test]
fn exposes_the_frozen_choice_table() {
    assert_eq!(DEFAULT_HTTP_IDLE_TIMEOUT_MS, 300_000);
    let labels: Vec<(&str, i64)> = HTTP_IDLE_TIMEOUT_CHOICES
        .iter()
        .map(|choice| (choice.label, choice.timeout_ms))
        .collect();
    assert_eq!(
        labels,
        vec![
            ("30 sec", 30_000),
            ("1 min", 60_000),
            ("2 min", 120_000),
            ("5 min", 300_000),
            ("disabled", 0),
        ]
    );
}

#[test]
fn applies_the_proxy_to_http_and_https() {
    let _guard = lock();
    let _env = EnvGuard::clear(&PROXY_KEYS);

    apply_http_proxy_settings(Some("http://127.0.0.1:7890"));

    assert_eq!(
        std::env::var("HTTP_PROXY").as_deref(),
        Ok("http://127.0.0.1:7890")
    );
    assert_eq!(
        std::env::var("HTTPS_PROXY").as_deref(),
        Ok("http://127.0.0.1:7890")
    );
}

#[test]
fn does_not_override_existing_proxy_environment_variables() {
    let _guard = lock();
    let _env = EnvGuard::clear(&PROXY_KEYS);
    // SAFETY: 测试持锁串行。
    unsafe {
        std::env::set_var("HTTP_PROXY", "http://env-http:8080");
        std::env::set_var("HTTPS_PROXY", "http://env-https:8080");
    }

    apply_http_proxy_settings(Some("http://settings:7890"));

    assert_eq!(
        std::env::var("HTTP_PROXY").as_deref(),
        Ok("http://env-http:8080")
    );
    assert_eq!(
        std::env::var("HTTPS_PROXY").as_deref(),
        Ok("http://env-https:8080")
    );
}

#[test]
fn ignores_empty_proxy_values() {
    let _guard = lock();
    let _env = EnvGuard::clear(&PROXY_KEYS);

    apply_http_proxy_settings(Some("   "));
    apply_http_proxy_settings(None);

    assert!(std::env::var_os("HTTP_PROXY").is_none());
    assert!(std::env::var_os("HTTPS_PROXY").is_none());
}

#[test]
fn treats_an_empty_existing_environment_variable_as_present() {
    let _guard = lock();
    let _env = EnvGuard::clear(&PROXY_KEYS);
    // `process.env.HTTP_PROXY ??= proxy`：空串不是 null/undefined，不会被覆盖。
    // SAFETY: 测试持锁串行。
    unsafe { std::env::set_var("HTTP_PROXY", "") };

    apply_http_proxy_settings(Some("http://settings:7890"));

    assert_eq!(std::env::var("HTTP_PROXY").as_deref(), Ok(""));
    assert_eq!(
        std::env::var("HTTPS_PROXY").as_deref(),
        Ok("http://settings:7890")
    );
}

#[test]
fn rejects_invalid_idle_timeouts() {
    let _guard = lock();
    let _env = EnvGuard::clear(&PROXY_KEYS);

    let error = configure_http_dispatcher(-5).expect_err("negative timeout is invalid");
    assert_eq!(error.to_string(), "Invalid HTTP idle timeout: -5");

    let error = configure_http_dispatcher(i64::MIN).expect_err("negative timeout is invalid");
    assert_eq!(
        error.to_string(),
        format!("Invalid HTTP idle timeout: {}", i64::MIN)
    );
}

#[test]
fn accepts_zero_as_disabled_and_rebuilds_the_shared_client() {
    let _guard = lock();
    let _env = EnvGuard::clear(&PROXY_KEYS);

    configure_http_dispatcher(0).expect("zero is the disabled choice");
    let disabled = shared_http_client();
    configure_http_dispatcher(DEFAULT_HTTP_IDLE_TIMEOUT_MS).expect("default timeout");
    let default = shared_http_client();

    assert!(!Arc::ptr_eq(&disabled, &default));
}

#[test]
fn returns_the_same_shared_client_instance() {
    let _guard = lock();
    let _env = EnvGuard::clear(&PROXY_KEYS);

    let first = shared_http_client();
    let second = shared_http_client();
    assert!(Arc::ptr_eq(&first, &second));

    configure_http_dispatcher(DEFAULT_HTTP_IDLE_TIMEOUT_MS).expect("configure");
    let configured = shared_http_client();
    assert!(!Arc::ptr_eq(&first, &configured));
    assert!(Arc::ptr_eq(&configured, &shared_http_client()));
}

/// 极简 HTTP 代理：接受连接，记录请求行，返回固定响应。
fn spawn_canned_proxy(body: &'static str) -> (String, Arc<Mutex<Vec<String>>>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
    let address = listener.local_addr().expect("proxy addr");
    let request_lines = Arc::new(Mutex::new(Vec::new()));
    let lines = Arc::clone(&request_lines);
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buffer = vec![0u8; 8192];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let text = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let request_line = text.lines().next().unwrap_or("").to_string();
            lines.lock().expect("proxy lines").push(request_line);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://{address}"), request_lines, connections)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// 本文件所有测试共享进程级环境变量与共享 client，必须串行；`block_on` 在测试线程上执行，
// guard 不会跨线程。
#[allow(clippy::await_holding_lock)]
async fn routes_requests_through_the_http_proxy() {
    let _guard = lock();
    let _env = EnvGuard::clear(&PROXY_KEYS);
    let (proxy_url, request_lines, connections) = spawn_canned_proxy("proxied");
    // SAFETY: 测试持锁串行。
    unsafe { std::env::set_var("HTTP_PROXY", &proxy_url) };

    configure_http_dispatcher(DEFAULT_HTTP_IDLE_TIMEOUT_MS).expect("configure");
    let client = shared_http_client();
    let body = client
        .get("http://example.invalid/thing")
        .send()
        .await
        .expect("proxy answers")
        .text()
        .await
        .expect("body");

    assert_eq!(body, "proxied");
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    let lines = request_lines.lock().expect("lines");
    assert_eq!(
        lines.as_slice(),
        ["GET http://example.invalid/thing HTTP/1.1".to_string()],
        "reqwest sends absolute-form to an HTTP proxy (undici proxyTunnel would CONNECT)"
    );
}

/// `NO_PROXY=host:port` 必须让请求直连 origin（undici `#shouldProxy` 的端口匹配）。
///
/// 回归：reqwest 的 `NoProxy`（hyper-util）只按逗号切分、忽略 `host:port` 的端口，
/// 会把本应直连的请求发给代理。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn no_proxy_host_and_port_bypasses_the_proxy() {
    let _guard = lock();
    let _env = EnvGuard::clear(&PROXY_KEYS);
    let (proxy_url, proxy_lines, proxy_connections) = spawn_canned_proxy("proxied");

    // origin：记录连接数并返回固定 body。
    let origin_listener = TcpListener::bind("127.0.0.1:0").expect("bind origin");
    let origin_address = origin_listener.local_addr().expect("origin addr");
    let origin_connections = Arc::new(AtomicUsize::new(0));
    let origin_counter = Arc::clone(&origin_connections);
    std::thread::spawn(move || {
        for stream in origin_listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            origin_counter.fetch_add(1, Ordering::SeqCst);
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let body = "origin";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    // SAFETY: 本文件所有测试持有 `ENV_LOCK`，串行执行。
    unsafe {
        std::env::set_var("HTTP_PROXY", &proxy_url);
        std::env::set_var("NO_PROXY", format!("127.0.0.1:{}", origin_address.port()));
    }
    configure_http_dispatcher(DEFAULT_HTTP_IDLE_TIMEOUT_MS).expect("configure");
    let client = shared_http_client();
    let body = client
        .get(format!("http://127.0.0.1:{}/thing", origin_address.port()))
        .send()
        .await
        .expect("origin answers")
        .text()
        .await
        .expect("body");

    assert_eq!(body, "origin");
    assert_eq!(origin_connections.load(Ordering::SeqCst), 1);
    assert_eq!(proxy_connections.load(Ordering::SeqCst), 0);
    assert!(proxy_lines.lock().expect("proxy lines").is_empty());
}

#[test]
fn rejects_invalid_proxy_urls_with_the_undici_messages() {
    let _guard = lock();
    let _env = EnvGuard::clear(&PROXY_KEYS);

    // `new URL(uri)` 失败 → `TypeError: Invalid URL`（undici 不给缺协议的值补 http://）。
    for value in [
        "127.0.0.1:7890",
        "http://",
        "://bad",
        "http://example.com:99999",
    ] {
        // SAFETY: 测试持锁串行。
        unsafe { std::env::set_var("HTTP_PROXY", value) };
        let error = configure_http_dispatcher(DEFAULT_HTTP_IDLE_TIMEOUT_MS).expect_err(value);
        assert_eq!(error.to_string(), "Invalid URL", "value: {value}");
    }

    // 语法合法但协议不是 http/https/socks5 → undici `parseURL` 的 InvalidArgumentError。
    for value in [
        "localhost:3128",
        "proxy:8080",
        "ftp://example.com:21",
        "socks4://127.0.0.1:1080",
        "socks5h://127.0.0.1:1080",
    ] {
        // SAFETY: 测试持锁串行。
        unsafe { std::env::set_var("HTTP_PROXY", value) };
        let error = configure_http_dispatcher(DEFAULT_HTTP_IDLE_TIMEOUT_MS).expect_err(value);
        assert_eq!(
            error.to_string(),
            "Invalid URL protocol: the URL must start with `http:` or `https:`.",
            "value: {value}"
        );
    }
}

/// 极简 SOCKS5 代理：完成无认证握手 + CONNECT（记录 ATYP/目标），随后把隧道里的 HTTP
/// 请求当作普通请求处理并返回固定 body。
///
/// 返回 `(proxy_url, 目标记录, 连接数)`；目标记录形如 `domain:port`（ATYP=3）或
/// `ipv4:a.b.c.d:port`。
fn spawn_socks5_proxy(body: &'static str) -> (String, Arc<Mutex<Vec<String>>>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind socks proxy");
    let address = listener.local_addr().expect("socks addr");
    let targets = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&targets);
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buffer = [0u8; 512];
            let mut read_more = |stream: &mut std::net::TcpStream, data: &mut Vec<u8>| -> bool {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => false,
                    Ok(read) => {
                        data.extend_from_slice(&buffer[..read]);
                        true
                    }
                }
            };

            // 问候：[VER, NMETHODS, METHODS...]
            let mut data = Vec::new();
            while data.len() < 2 || data.len() < 2 + data[1] as usize {
                if !read_more(&mut stream, &mut data) {
                    break;
                }
            }
            if data.len() < 2 {
                continue;
            }
            let methods = data[1] as usize;
            let mut request: Vec<u8> = data.split_off(2 + methods);
            let _ = stream.write_all(&[0x05, 0x00]);

            while request.len() < 5 {
                if !read_more(&mut stream, &mut request) {
                    break;
                }
            }
            if request.len() < 5 {
                continue;
            }
            let address_type = request[3];
            let (target, port_offset) = match address_type {
                0x01 => {
                    while request.len() < 10 {
                        if !read_more(&mut stream, &mut request) {
                            break;
                        }
                    }
                    if request.len() < 10 {
                        continue;
                    }
                    let host = format!(
                        "{}.{}.{}.{}",
                        request[4], request[5], request[6], request[7]
                    );
                    (format!("ipv4:{host}"), 8)
                }
                0x03 => {
                    while request.len() < 5 {
                        if !read_more(&mut stream, &mut request) {
                            break;
                        }
                    }
                    if request.len() < 5 {
                        continue;
                    }
                    let length = request[4] as usize;
                    while request.len() < 5 + length + 2 {
                        if !read_more(&mut stream, &mut request) {
                            break;
                        }
                    }
                    if request.len() < 5 + length + 2 {
                        continue;
                    }
                    let host = String::from_utf8_lossy(&request[5..5 + length]).into_owned();
                    (format!("domain:{host}"), 5 + length)
                }
                0x04 => {
                    while request.len() < 22 {
                        if !read_more(&mut stream, &mut request) {
                            break;
                        }
                    }
                    if request.len() < 22 {
                        continue;
                    }
                    let host = request[4..20]
                        .chunks(2)
                        .map(|pair| format!("{:x}{:x}", pair[0], pair[1]))
                        .collect::<Vec<_>>()
                        .join(":");
                    (format!("ipv6:{host}"), 20)
                }
                _ => continue,
            };
            let port = u16::from_be_bytes([request[port_offset], request[port_offset + 1]]);
            recorded
                .lock()
                .expect("socks targets")
                .push(format!("{target}:{port}"));
            let _ = stream.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
            // 隧道内是普通 HTTP 请求（origin-form）。
            let mut http = request.split_off(port_offset + 2);
            loop {
                if http.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
                if !read_more(&mut stream, &mut http) {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (address.to_string(), targets, connections)
}

/// undici 的 `socks5:`/`socks:` 走 `Socks5ProxyAgent`：连接代理、把**目标主机名**以
/// ATYP=DOMAIN 交给代理解析，再在隧道里发 origin-form 请求。`reqwest` 侧用 `socks5h:` 复刻。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn routes_requests_through_a_socks5_proxy() {
    let _guard = lock();
    let _env = EnvGuard::clear(&PROXY_KEYS);
    let (proxy_address, targets, connections) = spawn_socks5_proxy("socks-proxied");

    // undici 对 `socks5:` 与 `socks:` 都走 `Socks5ProxyAgent`。
    for scheme in ["socks5", "socks"] {
        // SAFETY: 测试持锁串行。
        unsafe { std::env::set_var("HTTP_PROXY", format!("{scheme}://{proxy_address}")) };

        configure_http_dispatcher(DEFAULT_HTTP_IDLE_TIMEOUT_MS).expect("configure");
        let client = shared_http_client();
        let body = client
            .get("http://some.example.host:1234/thing")
            .send()
            .await
            .expect("socks proxy answers")
            .text()
            .await
            .expect("body");

        assert_eq!(body, "socks-proxied", "scheme: {scheme}");
    }

    assert_eq!(connections.load(Ordering::SeqCst), 2);
    assert_eq!(
        targets.lock().expect("targets").as_slice(),
        [
            "domain:some.example.host:1234".to_string(),
            "domain:some.example.host:1234".to_string()
        ],
        "undici sends the target hostname to the SOCKS5 proxy (ATYP=DOMAIN)"
    );
}

/// `NO_PROXY=::1` / `[::1]` 必须让 IPv6 字面量直连（undici 在 `#shouldProxy` 前剥掉方括号）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn no_proxy_ipv6_literals_bypass_the_proxy() {
    let _guard = lock();
    let origin_listener = match TcpListener::bind("[::1]:0") {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("skipping: IPv6 loopback is unavailable ({error})");
            return;
        }
    };
    let origin_address = origin_listener.local_addr().expect("origin addr");
    let origin_connections = Arc::new(AtomicUsize::new(0));
    let origin_counter = Arc::clone(&origin_connections);
    std::thread::spawn(move || {
        for stream in origin_listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            origin_counter.fetch_add(1, Ordering::SeqCst);
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let body = "ipv6-origin";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    for no_proxy in ["::1", "[::1]"] {
        let _env = EnvGuard::clear(&PROXY_KEYS);
        let (proxy_url, proxy_lines, proxy_connections) = spawn_canned_proxy("proxied");
        // SAFETY: 本文件所有测试持有 `ENV_LOCK`，串行执行。
        unsafe {
            std::env::set_var("HTTP_PROXY", &proxy_url);
            std::env::set_var("NO_PROXY", no_proxy);
        }
        configure_http_dispatcher(DEFAULT_HTTP_IDLE_TIMEOUT_MS).expect("configure");
        let client = shared_http_client();
        let body = client
            .get(format!("http://[::1]:{}/thing", origin_address.port()))
            .send()
            .await
            .expect("origin answers")
            .text()
            .await
            .expect("body");

        assert_eq!(body, "ipv6-origin", "NO_PROXY={no_proxy}");
        assert_eq!(
            proxy_connections.load(Ordering::SeqCst),
            0,
            "NO_PROXY={no_proxy} must bypass the proxy"
        );
        assert!(proxy_lines.lock().expect("proxy lines").is_empty());
    }
    assert_eq!(origin_connections.load(Ordering::SeqCst), 2);
}

/// 共享 client 复刻 undici fetch 的默认请求头。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn shared_client_sends_undici_fetch_default_headers() {
    let _guard = lock();
    let _env = EnvGuard::clear(&PROXY_KEYS);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind origin");
    let address = listener.local_addr().expect("origin addr");
    let recorded = Arc::new(Mutex::new(String::new()));
    let recorder = Arc::clone(&recorded);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 8192];
            let read = stream.read(&mut buffer).unwrap_or(0);
            *recorder.lock().expect("request") =
                String::from_utf8_lossy(&buffer[..read]).into_owned();
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
            let _ = stream.flush();
        }
    });

    configure_http_dispatcher(DEFAULT_HTTP_IDLE_TIMEOUT_MS).expect("configure");
    let client = shared_http_client();
    client
        .get(format!("http://{address}/x"))
        .send()
        .await
        .expect("origin answers")
        .text()
        .await
        .expect("body");

    let request = recorded.lock().expect("request");
    let headers: Vec<String> = request
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .map(|line| line.to_ascii_lowercase())
        .collect();
    for expected in [
        "user-agent: undici",
        "accept: */*",
        "accept-language: *",
        "sec-fetch-mode: cors",
        "accept-encoding: gzip, deflate",
    ] {
        assert!(
            headers.iter().any(|header| header == expected),
            "missing {expected:?} in {headers:?}"
        );
    }
}
