//! `utils/tools-manager.ts` 的移植测试（`test/tools-manager.test.ts`）。
//!
//! TS 用 `vi.mock` 替换 `fs`/`os`/`child_process` 与全局 `fetch`；Rust 侧通过文档化的
//! [`ToolsManagerEnv`] 注入 platform/arch/bin_dir/release base/commandExists，HTTP 用 wiremock。
//! 进程级环境变量与共享 client 用一把锁串行。
//!
//! 成功路径在 unix 上用系统 `tar` 现场生成真实 `.tar.gz`（TS 的测试同样依赖真实解压）；
//! 没有 `tar` 时跳过该用例。

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use kiana_core::http_dispatcher::configure_http_dispatcher;
use kiana_core::tools_manager::{
    ToolStatus, ToolStatusKind, ToolsManagerEnv, ensure_tool_with, get_latest_version_with_base,
    get_tool_path_with,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static TEST_LOCK: Mutex<()> = Mutex::new(());

const ENV_KEYS: [&str; 8] = [
    "KIANA_OFFLINE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "http_proxy",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
    "SystemRoot",
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

fn injected_env(platform: &str, architecture: &str, bin_dir: &Path) -> ToolsManagerEnv {
    ToolsManagerEnv {
        platform: Some(platform.to_string()),
        arch: Some(architecture.to_string()),
        bin_dir: Some(bin_dir.to_path_buf()),
        release_base: None,
        command_exists: Some(Arc::new(|_| false)),
    }
}

async fn mount_redirect(server: &MockServer, repo: &str, location: &str, status: u16, body: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/{repo}/releases/latest")))
        .respond_with(
            ResponseTemplate::new(status)
                .insert_header("location", location)
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

async fn mount_status(server: &MockServer, repo: &str, status: u16) {
    Mock::given(method("GET"))
        .and(path(format!("/{repo}/releases/latest")))
        .respond_with(ResponseTemplate::new(status))
        .mount(server)
        .await;
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn get_latest_version_resolves_the_release_redirect() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");
    let server = MockServer::start().await;
    mount_redirect(
        &server,
        "sharkdp/fd",
        "https://github.com/sharkdp/fd/releases/tag/v10.4.2",
        302,
        "",
    )
    .await;

    let version = get_latest_version_with_base("sharkdp/fd", &server.uri())
        .await
        .expect("redirect resolves");
    assert_eq!(version, "10.4.2");

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/sharkdp/fd/releases/latest");
    let user_agent = requests[0]
        .headers
        .get("user-agent")
        .expect("user agent")
        .to_str()
        .expect("ascii");
    assert!(
        user_agent.starts_with("kiana/"),
        "unexpected UA {user_agent}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn get_latest_version_keeps_tags_without_a_v_prefix() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");
    let server = MockServer::start().await;
    mount_redirect(
        &server,
        "BurntSushi/ripgrep",
        "https://github.com/BurntSushi/ripgrep/releases/tag/15.2.0",
        302,
        "",
    )
    .await;

    assert_eq!(
        get_latest_version_with_base("BurntSushi/ripgrep", &server.uri())
            .await
            .expect("redirect resolves"),
        "15.2.0"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn get_latest_version_resolves_relative_redirect_targets() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");
    let server = MockServer::start().await;
    mount_redirect(
        &server,
        "sharkdp/fd",
        "/sharkdp/fd/releases/tag/v10.4.2",
        302,
        "",
    )
    .await;

    assert_eq!(
        get_latest_version_with_base("sharkdp/fd", &server.uri())
            .await
            .expect("redirect resolves"),
        "10.4.2"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn get_latest_version_discards_the_redirect_response_body() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");
    let server = MockServer::start().await;
    mount_redirect(
        &server,
        "sharkdp/fd",
        "https://github.com/sharkdp/fd/releases/tag/v10.4.2",
        302,
        "<html></html>",
    )
    .await;

    assert_eq!(
        get_latest_version_with_base("sharkdp/fd", &server.uri())
            .await
            .expect("redirect resolves"),
        "10.4.2"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn get_latest_version_fails_clearly_without_a_redirect() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");
    let server = MockServer::start().await;
    mount_status(&server, "sharkdp/fd", 404).await;

    let error = get_latest_version_with_base("sharkdp/fd", &server.uri())
        .await
        .expect_err("404 has no redirect");
    assert_eq!(
        error.to_string(),
        "Failed to resolve latest sharkdp/fd release: HTTP 404 without redirect"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn get_latest_version_treats_an_empty_location_as_missing() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");
    let server = MockServer::start().await;
    mount_redirect(&server, "sharkdp/fd", "", 302, "").await;

    let error = get_latest_version_with_base("sharkdp/fd", &server.uri())
        .await
        .expect_err("empty location is falsy in TS");
    assert_eq!(
        error.to_string(),
        "Failed to resolve latest sharkdp/fd release: HTTP 302 without redirect"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn get_latest_version_fails_clearly_for_non_release_redirects() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");
    let server = MockServer::start().await;
    mount_redirect(&server, "sharkdp/fd", "https://github.com/login", 302, "").await;

    let error = get_latest_version_with_base("sharkdp/fd", &server.uri())
        .await
        .expect_err("login page is not a release tag");
    assert_eq!(
        error.to_string(),
        "Failed to resolve latest sharkdp/fd release: unexpected redirect to https://github.com/login"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn get_latest_version_decodes_percent_encoded_tags() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");
    let server = MockServer::start().await;
    mount_redirect(
        &server,
        "sharkdp/fd",
        "https://github.com/sharkdp/fd/releases/tag/v10.4.2%2Brc1",
        302,
        "",
    )
    .await;

    assert_eq!(
        get_latest_version_with_base("sharkdp/fd", &server.uri())
            .await
            .expect("redirect resolves"),
        "10.4.2+rc1"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn get_latest_version_reports_malformed_percent_escapes() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");
    let server = MockServer::start().await;
    mount_redirect(
        &server,
        "sharkdp/fd",
        "https://github.com/sharkdp/fd/releases/tag/v1%zz",
        302,
        "",
    )
    .await;

    let error = get_latest_version_with_base("sharkdp/fd", &server.uri())
        .await
        .expect_err("decodeURIComponent throws URIError");
    assert_eq!(error.to_string(), "URI malformed");
}

#[test]
fn get_tool_path_prefers_the_local_bin_directory() {
    let directory = tempfile::tempdir().expect("temp dir");
    let bin_dir = directory.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    std::fs::write(bin_dir.join("fd"), b"binary").expect("local fd");
    let env = ToolsManagerEnv {
        bin_dir: Some(bin_dir.clone()),
        ..ToolsManagerEnv::default()
    };

    assert_eq!(
        get_tool_path_with("fd", &env),
        Some(bin_dir.join("fd").to_string_lossy().into_owned())
    );
}

#[test]
fn get_tool_path_uses_exe_for_windows() {
    let directory = tempfile::tempdir().expect("temp dir");
    let bin_dir = directory.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    std::fs::write(bin_dir.join("fd.exe"), b"binary").expect("local fd.exe");
    let env = ToolsManagerEnv {
        platform: Some("win32".to_string()),
        bin_dir: Some(bin_dir.clone()),
        ..ToolsManagerEnv::default()
    };

    assert_eq!(
        get_tool_path_with("fd", &env),
        Some(bin_dir.join("fd.exe").to_string_lossy().into_owned())
    );
}

#[test]
fn get_tool_path_falls_back_to_system_binary_names() {
    let directory = tempfile::tempdir().expect("temp dir");
    let env = ToolsManagerEnv {
        bin_dir: Some(directory.path().join("bin")),
        command_exists: Some(Arc::new(|command| command == "fdfind")),
        ..ToolsManagerEnv::default()
    };

    assert_eq!(get_tool_path_with("fd", &env), Some("fdfind".to_string()));
}

#[test]
fn get_tool_path_uses_the_binary_name_for_ripgrep() {
    let directory = tempfile::tempdir().expect("temp dir");
    let env = ToolsManagerEnv {
        bin_dir: Some(directory.path().join("bin")),
        command_exists: Some(Arc::new(|command| command == "rg")),
        ..ToolsManagerEnv::default()
    };

    assert_eq!(get_tool_path_with("rg", &env), Some("rg".to_string()));
}

#[test]
fn get_tool_path_returns_none_for_unknown_or_missing_tools() {
    let directory = tempfile::tempdir().expect("temp dir");
    let env = ToolsManagerEnv {
        bin_dir: Some(directory.path().join("bin")),
        command_exists: Some(Arc::new(|_| false)),
        ..ToolsManagerEnv::default()
    };

    assert_eq!(get_tool_path_with("fd", &env), None);
    assert_eq!(get_tool_path_with("rg", &env), None);
    assert_eq!(get_tool_path_with("unknown", &env), None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn ensure_tool_reports_offline_mode_through_the_callback() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    // SAFETY: 测试串行化。
    unsafe { std::env::set_var("KIANA_OFFLINE", "1") };
    let directory = tempfile::tempdir().expect("temp dir");
    let env = injected_env("linux", "x64", directory.path());

    let mut statuses: Vec<ToolStatus> = Vec::new();
    let result = ensure_tool_with("fd", Some(&mut |status| statuses.push(status)), &env).await;

    assert_eq!(result, None);
    assert_eq!(
        statuses,
        vec![ToolStatus {
            kind: ToolStatusKind::Warning,
            message: "fd not found. Offline mode enabled, skipping download.".to_string(),
        }]
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn ensure_tool_treats_truthy_offline_values() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let directory = tempfile::tempdir().expect("temp dir");
    let env = injected_env("linux", "x64", directory.path());

    for value in ["1", "true", "TRUE", "yes", "Yes"] {
        // SAFETY: 测试串行化。
        unsafe { std::env::set_var("KIANA_OFFLINE", value) };
        let mut statuses: Vec<ToolStatus> = Vec::new();
        let result = ensure_tool_with("fd", Some(&mut |status| statuses.push(status)), &env).await;
        assert_eq!(result, None, "KIANA_OFFLINE={value}");
        assert_eq!(statuses.len(), 1, "KIANA_OFFLINE={value}");
    }

    // "0"/"no" 不是离线（与 version-check 的真值判定不同）。
    for value in ["0", "no", ""] {
        // SAFETY: 测试串行化。
        unsafe { std::env::set_var("KIANA_OFFLINE", value) };
        let mut statuses: Vec<ToolStatus> = Vec::new();
        let env = ToolsManagerEnv {
            release_base: Some("http://127.0.0.1:1".to_string()),
            ..injected_env("linux", "x64", directory.path())
        };
        let _ = ensure_tool_with("fd", Some(&mut |status| statuses.push(status)), &env).await;
        assert!(
            statuses
                .iter()
                .all(|status| !status.message.contains("Offline mode")),
            "KIANA_OFFLINE={value} must not be treated as offline"
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn ensure_tool_does_not_download_x86_64_binaries_on_other_architectures() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temp dir");

    for architecture in ["arm", "ia32"] {
        let env = ToolsManagerEnv {
            release_base: Some(server.uri()),
            ..injected_env("linux", architecture, directory.path())
        };
        let mut statuses: Vec<ToolStatus> = Vec::new();
        let result = ensure_tool_with("rg", Some(&mut |status| statuses.push(status)), &env).await;

        assert_eq!(result, None);
        assert_eq!(
            statuses,
            vec![ToolStatus {
                kind: ToolStatusKind::Warning,
                message: format!(
                    "ripgrep not found. Automatic download is not supported on linux/{architecture}. Install ripgrep with your system package manager."
                ),
            }]
        );
    }
    assert_eq!(server.received_requests().await.expect("requests").len(), 0);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn ensure_tool_uses_the_termux_install_hint_on_android() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let directory = tempfile::tempdir().expect("temp dir");
    let env = injected_env("android", "arm64", directory.path());

    let mut statuses: Vec<ToolStatus> = Vec::new();
    let result = ensure_tool_with("rg", Some(&mut |status| statuses.push(status)), &env).await;

    assert_eq!(result, None);
    assert_eq!(
        statuses,
        vec![ToolStatus {
            kind: ToolStatusKind::Warning,
            message: "ripgrep not found. Install with: pkg install ripgrep".to_string(),
        }]
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn ensure_tool_surfaces_the_error_cause_chain_when_a_download_fails() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");
    let server = MockServer::start().await;
    mount_status(&server, "sharkdp/fd", 500).await;
    let directory = tempfile::tempdir().expect("temp dir");
    let env = ToolsManagerEnv {
        release_base: Some(server.uri()),
        ..injected_env("linux", "x64", directory.path())
    };

    let mut statuses: Vec<ToolStatus> = Vec::new();
    let result = ensure_tool_with("fd", Some(&mut |status| statuses.push(status)), &env).await;

    assert_eq!(result, None);
    assert_eq!(
        statuses,
        vec![
            ToolStatus {
                kind: ToolStatusKind::Info,
                message: "fd not found. Downloading...".to_string(),
            },
            ToolStatus {
                kind: ToolStatusKind::Warning,
                message: "Failed to download fd: Failed to resolve latest sharkdp/fd release: HTTP 500 without redirect".to_string(),
            },
        ]
    );
}

/// TS 用例（`surfaces the error cause chain when a download fails`）用
/// `TypeError("fetch failed", {cause: Error("connect ETIMEDOUT 140.82.113.3:443")})` 断言精确的
/// cause 链；Rust 侧用真实的连接拒绝复刻：Node 的文案是
/// `connect ECONNREFUSED <host>:<port>`（`management_http` 的 errno 映射）。
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn ensure_tool_includes_the_transport_cause_in_the_warning() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");
    // 取一个刚释放的回环端口（连接会被拒绝）。
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = closed.local_addr().expect("addr");
    drop(closed);
    let directory = tempfile::tempdir().expect("temp dir");
    let env = ToolsManagerEnv {
        release_base: Some(format!("http://{address}")),
        ..injected_env("linux", "x64", directory.path())
    };

    let mut statuses: Vec<ToolStatus> = Vec::new();
    let result = ensure_tool_with("fd", Some(&mut |status| statuses.push(status)), &env).await;

    assert_eq!(result, None);
    assert_eq!(
        statuses,
        vec![
            ToolStatus {
                kind: ToolStatusKind::Info,
                message: "fd not found. Downloading...".to_string(),
            },
            ToolStatus {
                kind: ToolStatusKind::Warning,
                message: format!(
                    "Failed to download fd: fetch failed: connect ECONNREFUSED {address}"
                ),
            },
        ]
    );
}

#[cfg(unix)]
fn build_fd_archive(asset_stem: &str) -> Vec<u8> {
    let workspace = tempfile::tempdir().expect("archive workspace");
    let source = workspace.path().join("src");
    let inner = source.join(asset_stem);
    std::fs::create_dir_all(&inner).expect("archive source");
    std::fs::write(inner.join("fd"), b"#!/bin/sh\necho fd\n").expect("binary");
    let archive = workspace.path().join("archive.tar.gz");
    let status = std::process::Command::new("tar")
        .arg("czf")
        .arg(&archive)
        .arg("-C")
        .arg(&source)
        .arg(asset_stem)
        .status()
        .expect("run tar");
    assert!(status.success(), "tar failed");
    std::fs::read(archive).expect("read archive")
}

#[cfg(unix)]
fn tar_available() -> bool {
    std::process::Command::new("tar")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn ensure_tool_downloads_extracts_and_installs_the_binary() {
    if !tar_available() {
        eprintln!("skipping: tar is not available");
        return;
    }
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");

    let asset_stem = "fd-v10.4.2-x86_64-unknown-linux-musl";
    let asset_name = format!("{asset_stem}.tar.gz");
    let archive = build_fd_archive(asset_stem);

    let server = MockServer::start().await;
    let location = format!("{}/sharkdp/fd/releases/tag/v10.4.2", server.uri());
    mount_redirect(&server, "sharkdp/fd", &location, 302, "").await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/sharkdp/fd/releases/download/v10.4.2/{asset_name}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
        .mount(&server)
        .await;

    let directory = tempfile::tempdir().expect("temp dir");
    let bin_dir = directory.path().join("bin");
    let env = ToolsManagerEnv {
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        bin_dir: Some(bin_dir.clone()),
        release_base: Some(server.uri()),
        command_exists: Some(Arc::new(|_| false)),
    };

    let mut statuses: Vec<ToolStatus> = Vec::new();
    let result = ensure_tool_with("fd", Some(&mut |status| statuses.push(status)), &env).await;

    let expected_path = bin_dir.join("fd");
    assert_eq!(result, Some(expected_path.to_string_lossy().into_owned()));
    assert_eq!(
        statuses,
        vec![
            ToolStatus {
                kind: ToolStatusKind::Info,
                message: "fd not found. Downloading...".to_string(),
            },
            ToolStatus {
                kind: ToolStatusKind::Info,
                message: format!("fd installed to {}", expected_path.to_string_lossy()),
            },
        ]
    );
    assert_eq!(
        std::fs::read(&expected_path).expect("installed binary"),
        b"#!/bin/sh\necho fd\n"
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&expected_path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);

    // finally：归档与解压目录都被清理，bin 目录只剩二进制。
    let entries: Vec<String> = std::fs::read_dir(&bin_dir)
        .expect("bin dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(entries, vec!["fd".to_string()]);
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn ensure_tool_prefers_an_existing_bin_over_downloading() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    let directory = tempfile::tempdir().expect("temp dir");
    let bin_dir = directory.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    std::fs::write(bin_dir.join("rg"), b"binary").expect("rg");
    let env = ToolsManagerEnv {
        bin_dir: Some(bin_dir.clone()),
        command_exists: Some(Arc::new(|_| false)),
        ..ToolsManagerEnv::default()
    };

    let mut statuses: Vec<ToolStatus> = Vec::new();
    let result = ensure_tool_with("rg", Some(&mut |status| statuses.push(status)), &env).await;

    assert_eq!(
        result,
        Some(bin_dir.join("rg").to_string_lossy().into_owned())
    );
    assert!(statuses.is_empty());
}

/// 发送 `content-length: 100` 但只写 7 字节就关闭连接的服务器（模拟下载截断）。
fn spawn_content_length_mismatch_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind server");
    let address = listener.local_addr().expect("server addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\nconnection: close\r\n\r\npartial",
            );
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    });
    format!("http://{address}")
}

/// 回归：undici 的 `TypeError("terminated")` 带 `ResponseContentLengthMismatchError` cause，
/// `ensureTool` 的 cause 链应显示 `terminated: Response body length does not match ...`。
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn ensure_tool_reports_the_truncated_body_cause_chain() {
    let _guard = lock();
    let _env = EnvGuard::clear(&ENV_KEYS);
    configure_http_dispatcher(300_000).expect("configure HTTP client");
    let release_base = spawn_content_length_mismatch_server();
    let directory = tempfile::tempdir().expect("temp dir");
    // fd + darwin/x64 → 固定 10.3.0，跳过 release 重定向查询，直接进入下载。
    let env = ToolsManagerEnv {
        platform: Some("darwin".to_string()),
        arch: Some("x64".to_string()),
        bin_dir: Some(directory.path().join("bin")),
        release_base: Some(release_base),
        command_exists: Some(Arc::new(|_| false)),
    };

    let mut statuses: Vec<ToolStatus> = Vec::new();
    let result = ensure_tool_with("fd", Some(&mut |status| statuses.push(status)), &env).await;

    assert_eq!(result, None);
    assert_eq!(
        statuses,
        vec![
            ToolStatus {
                kind: ToolStatusKind::Info,
                message: "fd not found. Downloading...".to_string(),
            },
            ToolStatus {
                kind: ToolStatusKind::Warning,
                message: "Failed to download fd: terminated: Response body length does not match content-length header".to_string(),
            },
        ]
    );
}
