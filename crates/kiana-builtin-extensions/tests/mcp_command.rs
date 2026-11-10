//! `packages/coding-agent/test/mcp-command.test.ts`：`kiana mcp` 子命令。
//!
//! 真实 `node` 跑 `tests/fixtures/mcp_cli/stdio-server.mjs`（TS 用 `process.execPath`；
//! Rust 测试进程不是 node，因此直接用 PATH 上的 `node`）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kiana_builtin_extensions::mcp::cli::{McpCommandOptions, run_mcp_command};
use serde_json::{Value, json};

fn fixture() -> String {
    format!(
        "{}/tests/fixtures/mcp_cli/stdio-server.mjs",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn require_node() {
    let available = std::process::Command::new("node")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    assert!(
        available,
        "these tests spawn `node`; install node or run them in an environment that has it"
    );
}

fn fixture_server() -> Value {
    json!({"command": "node", "args": [fixture()]})
}

fn servers() -> Value {
    json!({
        "fixture": fixture_server(),
        "broken": {"command": "kiana-test-missing-mcp-server"},
        "parked": {"command": "node", "args": [fixture()], "enabled": false},
        "bad": {"args": ["no command"]},
    })
}

struct RunResult {
    exit_code: i32,
    output: String,
}

async fn run_in(dir: &Path, args: &[&str], servers: Option<Value>) -> RunResult {
    if let Some(servers) = servers {
        std::fs::write(
            dir.join("mcp.json"),
            serde_json::to_string(&json!({"mcpServers": servers})).unwrap(),
        )
        .unwrap();
    }
    let output: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log_sink = Arc::clone(&output);
    let log: Arc<dyn Fn(&str) + Send + Sync> =
        Arc::new(move |line: &str| log_sink.lock().unwrap().push(line.to_string()));
    let error_sink = Arc::clone(&output);
    let error: Arc<dyn Fn(&str) + Send + Sync> =
        Arc::new(move |line: &str| error_sink.lock().unwrap().push(line.to_string()));
    let exit_code = run_mcp_command(
        args.iter().map(|arg| (*arg).to_string()).collect(),
        McpCommandOptions {
            cwd: dir.to_path_buf(),
            agent_dir: dir.to_path_buf(),
            credentials: None,
            open_url: None,
            log: Some(log),
            error: Some(error),
        },
    )
    .await;
    let output = output.lock().unwrap().join("\n");
    RunResult { exit_code, output }
}

fn read_config(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[tokio::test]
async fn lists_servers_with_their_state_tools_and_errors_and_fails_while_anything_is_wrong() {
    require_node();
    let dir = tempfile::tempdir().unwrap();
    let result = run_in(dir.path(), &["list"], Some(servers())).await;
    assert_eq!(result.exit_code, 1);
    assert!(
        result
            .output
            .contains("fixture: connected, 1 tool (codemode, global)\n"),
        "{}",
        result.output
    );
    assert!(result.output.contains("  tools: echo"), "{}", result.output);
    assert!(
        result.output.contains(
            "broken: failed (codemode, global)\n  kiana-test-missing-mcp-server\n  spawn kiana-test-missing-mcp-server ENOENT"
        ),
        "{}",
        result.output
    );
    assert!(
        result
            .output
            .contains("parked: disabled (codemode, global)"),
        "{}",
        result.output
    );
    assert!(
        result.output.contains("config error: "),
        "{}",
        result.output
    );
    assert!(
        result
            .output
            .contains("server \"bad\" needs either \"command\""),
        "{}",
        result.output
    );

    let ok = run_in(
        dir.path(),
        &["list"],
        Some(json!({"fixture": fixture_server()})),
    )
    .await;
    assert_eq!(ok.exit_code, 0, "{}", ok.output);
}

/// 评审回归：`describeTransport` 按 `"url" in config` 返回原始 `config.url`。
/// stdio 条目带 `url: null` 时人类输出（模板字符串）是 `null`、JSON 输出保持 `null`；
/// 带字符串 url 时两条路径都显示 URL（而不是 `command args`）。
#[tokio::test]
async fn lists_the_raw_url_of_stdio_configs_that_carry_a_url_key() {
    let dir = tempfile::tempdir().unwrap();
    let result = run_in(
        dir.path(),
        &["list"],
        Some(json!({
            "nullurl": {"command": "sh", "args": ["-c", "exit 1"], "url": null, "enabled": false},
            "stray": {
                "type": "stdio",
                "command": "sh",
                "args": ["-c", "exit 1"],
                "url": "https://example.com/mcp",
                "enabled": false
            },
        })),
    )
    .await;
    assert_eq!(result.exit_code, 0, "{}", result.output);
    assert!(
        result
            .output
            .contains("nullurl: disabled (codemode, global)\n  null"),
        "{}",
        result.output
    );
    assert!(
        result
            .output
            .contains("stray: disabled (codemode, global)\n  https://example.com/mcp"),
        "{}",
        result.output
    );

    let json = run_in(dir.path(), &["list", "--json"], None).await;
    assert_eq!(json.exit_code, 0, "{}", json.output);
    let parsed: Value = serde_json::from_str(&json.output).unwrap();
    assert_eq!(parsed["servers"][0]["transport"], Value::Null);
    assert_eq!(parsed["servers"][1]["transport"], "https://example.com/mcp");
}

#[tokio::test]
async fn prints_json_for_scripts() {
    require_node();
    let dir = tempfile::tempdir().unwrap();
    let result = run_in(
        dir.path(),
        &["list", "--json"],
        Some(json!({"fixture": fixture_server(), "parked": {"command": "node", "args": [fixture()], "enabled": false}})),
    )
    .await;
    assert_eq!(result.exit_code, 0, "{}", result.output);
    let parsed: Value = serde_json::from_str(&result.output).expect("output is JSON");
    let summary: Vec<(String, String, Vec<String>)> = parsed["servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|server| {
            (
                server["name"].as_str().unwrap().to_string(),
                server["state"].as_str().unwrap().to_string(),
                server["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|tool| tool.as_str().unwrap().to_string())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "fixture".to_string(),
                "connected".to_string(),
                vec!["echo".to_string()]
            ),
            ("parked".to_string(), "disabled".to_string(), Vec::new())
        ]
    );
}

#[tokio::test]
async fn rejects_unknown_servers_and_servers_without_oauth_for_login_and_logout() {
    require_node();
    let dir = tempfile::tempdir().unwrap();
    let unknown = run_in(dir.path(), &["login", "nope"], Some(servers())).await;
    assert_eq!(unknown.exit_code, 1);
    assert_eq!(
        unknown.output,
        "No MCP server named \"nope\". Configured: fixture, broken, parked."
    );
    let logout = run_in(dir.path(), &["logout", "fixture"], None).await;
    assert_eq!(logout.exit_code, 1);
    assert_eq!(
        logout.output,
        "MCP server \"fixture\" does not use OAuth. Only HTTP servers without an Authorization header do."
    );
    assert_eq!(run_in(dir.path(), &["frobnicate"], None).await.exit_code, 1);
}

#[tokio::test]
async fn adds_stdio_servers_and_passes_options_after_the_command_through() {
    let dir = tempfile::tempdir().unwrap();
    let added = run_in(
        dir.path(),
        &[
            "add", "--env", "A=1", "--env", "B=x=y", "files", "--", "npx", "-y", "server",
            "--root", ".",
        ],
        None,
    )
    .await;
    assert_eq!(added.exit_code, 0, "{}", added.output);
    assert!(
        added.output.contains("Added global MCP server \"files\""),
        "{}",
        added.output
    );
    assert_eq!(
        read_config(&dir.path().join("mcp.json")),
        json!({
            "mcpServers": {
                "files": {"command": "npx", "args": ["-y", "server", "--root", "."], "env": {"A": "1", "B": "x=y"}},
            }
        })
    );

    let replaced = run_in(
        dir.path(),
        &["add", "files", "node", "server.js", "--port", "1"],
        None,
    )
    .await;
    assert!(
        replaced
            .output
            .contains("Replaced global MCP server \"files\""),
        "{}",
        replaced.output
    );
    assert_eq!(
        read_config(&dir.path().join("mcp.json")),
        json!({"mcpServers": {"files": {"command": "node", "args": ["server.js", "--port", "1"]}}})
    );
}

#[tokio::test]
async fn adds_http_servers_and_keeps_other_content_of_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let result = run_in(
        dir.path(),
        &[
            "add",
            "docs",
            "--url",
            "https://example.com/mcp",
            "--bearer-token-env-var",
            "DOCS_TOKEN",
            "--header",
            "X-Team=core",
            "--exposure",
            "direct",
            "--description",
            "Product docs",
        ],
        Some(json!({"fixture": fixture_server()})),
    )
    .await;
    assert_eq!(result.exit_code, 0, "{}", result.output);
    assert!(!result.output.contains("mcp login"), "{}", result.output);
    assert_eq!(
        read_config(&dir.path().join("mcp.json")),
        json!({
            "mcpServers": {
                "fixture": fixture_server(),
                "docs": {
                    "url": "https://example.com/mcp",
                    "headers": {"X-Team": "core", "Authorization": "Bearer ${DOCS_TOKEN}"},
                    "exposure": "direct",
                    "description": "Product docs",
                },
            }
        })
    );

    let oauth = run_in(
        dir.path(),
        &[
            "add",
            "sentry",
            "--url",
            "https://mcp.sentry.dev/mcp",
            "--oauth-client-id",
            "kiana",
            "--oauth-client-name",
            "Claude Code",
        ],
        None,
    )
    .await;
    assert!(
        oauth
            .output
            .contains("If it requires sign-in: kiana mcp login sentry"),
        "{}",
        oauth.output
    );
    let config = read_config(&dir.path().join("mcp.json"));
    assert_eq!(
        config["mcpServers"]["sentry"],
        json!({"url": "https://mcp.sentry.dev/mcp", "oauth": {"clientId": "kiana", "clientName": "Claude Code"}})
    );
}

#[tokio::test]
async fn rejects_invalid_add_invocations_without_writing() {
    let cases: Vec<Vec<&str>> = vec![
        vec!["add", "x"],
        vec!["add", "x", "--url", "https://example.com", "--", "cmd"],
        vec!["add", "bad name", "--", "cmd"],
        vec!["add", "x", "--url", "ftp://example.com"],
        vec!["add", "x", "--env", "A=1", "--url", "https://example.com"],
        vec!["add", "x", "--header", "A=1", "--", "cmd"],
        vec!["add", "x", "--env", "NOVALUE", "--", "cmd"],
        vec!["add", "x", "--exposure", "loud", "--", "cmd"],
    ];
    for args in cases {
        let dir = tempfile::tempdir().unwrap();
        let result = run_in(dir.path(), &args, None).await;
        assert_eq!(result.exit_code, 1, "{}", args.join(" "));
        assert!(!dir.path().join("mcp.json").exists(), "{}", args.join(" "));
    }
}

#[tokio::test]
async fn adds_and_removes_project_servers() {
    let dir = tempfile::tempdir().unwrap();
    let added = run_in(
        dir.path(),
        &["add", "-l", "local", "--", "node", "server.js"],
        None,
    )
    .await;
    assert!(
        added.output.contains("The project is not trusted"),
        "{}",
        added.output
    );
    let project_config = dir.path().join(".kiana").join("mcp.json");
    assert_eq!(
        read_config(&project_config),
        json!({"mcpServers": {"local": {"command": "node", "args": ["server.js"]}}})
    );

    let wrong_scope = run_in(dir.path(), &["remove", "local"], None).await;
    assert_eq!(wrong_scope.exit_code, 1);
    assert!(
        wrong_scope.output.contains(&format!(
            "It is defined in {}; use --local.",
            project_config.to_string_lossy()
        )),
        "{}",
        wrong_scope.output
    );

    let removed = run_in(dir.path(), &["remove", "local", "--local"], None).await;
    assert_eq!(removed.exit_code, 0, "{}", removed.output);
    assert!(
        removed
            .output
            .contains("Removed project MCP server \"local\""),
        "{}",
        removed.output
    );
    assert_eq!(read_config(&project_config), json!({"mcpServers": {}}));
}

#[tokio::test]
async fn removes_global_servers() {
    require_node();
    let dir = tempfile::tempdir().unwrap();
    let first = run_in(dir.path(), &["remove", "broken"], Some(servers())).await;
    assert_eq!(first.exit_code, 0, "{}", first.output);
    let config = read_config(&dir.path().join("mcp.json"));
    let keys: Vec<String> = config["mcpServers"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, vec!["fixture", "parked", "bad"]);

    let missing = run_in(dir.path(), &["remove", "broken"], None).await;
    assert_eq!(missing.exit_code, 1);
    assert!(
        missing
            .output
            .contains("No global MCP server named \"broken\""),
        "{}",
        missing.output
    );
}

/// 未使用（保持 `PathBuf` 导入）。
#[allow(dead_code)]
fn _pathbuf(_: PathBuf) {}

/// TS 里 `credentials.remove()` 直接调用 `backend.withLock`，损坏的 `mcp-auth.json` 会让
/// `JSON.parse` 抛出；命令不会打印 "No stored credentials" 静默成功。
#[tokio::test]
async fn logout_fails_loudly_when_the_credentials_file_is_corrupt() {
    use kiana_builtin_extensions::mcp::oauth::McpOAuthCredentialStore;
    use kiana_config::auth::storage::{AuthStorageBackend, FileAuthStorageBackend};

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("mcp.json"),
        serde_json::to_string(&json!({
            "mcpServers": {"remote": {"url": "https://mcp.example.com/mcp"}}
        }))
        .unwrap(),
    )
    .unwrap();
    let auth_path = dir.path().join("mcp-auth.json");
    std::fs::write(&auth_path, "not json").unwrap();
    let credentials = Arc::new(McpOAuthCredentialStore::new(
        Some(AuthStorageBackend::File(FileAuthStorageBackend::new(
            &auth_path.to_string_lossy(),
        ))),
        None,
    ));

    let output: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log_sink = Arc::clone(&output);
    let log: Arc<dyn Fn(&str) + Send + Sync> =
        Arc::new(move |line: &str| log_sink.lock().unwrap().push(line.to_string()));
    let error_sink = Arc::clone(&output);
    let error: Arc<dyn Fn(&str) + Send + Sync> =
        Arc::new(move |line: &str| error_sink.lock().unwrap().push(line.to_string()));
    let exit_code = run_mcp_command(
        vec!["logout".to_string(), "remote".to_string()],
        McpCommandOptions {
            cwd: dir.path().to_path_buf(),
            agent_dir: dir.path().to_path_buf(),
            credentials: Some(credentials),
            open_url: None,
            log: Some(log),
            error: Some(error),
        },
    )
    .await;
    assert_eq!(exit_code, 1);
    let output = output.lock().unwrap().join("\n");
    assert!(output.contains("Unexpected token"), "{output}");
    assert!(output.contains("is not valid JSON"), "{output}");
    assert!(!output.contains("No stored credentials"), "{output}");
    assert_eq!(std::fs::read_to_string(&auth_path).unwrap(), "not json");
}

/// TS 的 `ProjectTrustStore.get` 对损坏的 trust.json 抛错：命令本身失败，不会把它当成
/// “项目未受信任”继续执行。
#[tokio::test]
async fn fails_instead_of_treating_an_unreadable_trust_store_as_untrusted() {
    let dir = tempfile::tempdir().unwrap();
    let trust_path = dir.path().join("trust.json");
    std::fs::write(&trust_path, "not json").unwrap();

    let result = run_in(dir.path(), &["list"], None).await;
    assert_eq!(result.exit_code, 1, "{}", result.output);
    assert!(
        result.output.contains(&format!(
            "Failed to read trust store {}:",
            trust_path.to_string_lossy()
        )),
        "{}",
        result.output
    );
    assert!(
        !result.output.contains("No MCP servers configured"),
        "{}",
        result.output
    );
}

/// 与 TS 的顺序一致：`add -l` 先写出文件并 log "Added"，之后读取 trust 文件失败才报错，
/// 因此文件已经更新，且不再输出 not-trusted 提示或 "Check it with"。
#[tokio::test]
async fn add_local_writes_the_entry_before_reporting_an_unreadable_trust_store() {
    let dir = tempfile::tempdir().unwrap();
    let trust_path = dir.path().join("trust.json");
    std::fs::write(&trust_path, "not json").unwrap();

    let result = run_in(
        dir.path(),
        &["add", "-l", "local", "--", "node", "server.js"],
        None,
    )
    .await;
    assert_eq!(result.exit_code, 1, "{}", result.output);
    assert!(
        result.output.contains("Added project MCP server \"local\""),
        "{}",
        result.output
    );
    assert!(
        result.output.contains("Failed to read trust store"),
        "{}",
        result.output
    );
    assert!(
        !result.output.contains("The project is not trusted"),
        "{}",
        result.output
    );
    assert!(
        !result.output.contains("Check it with"),
        "{}",
        result.output
    );
    let project_config = dir.path().join(".kiana").join("mcp.json");
    assert_eq!(
        read_config(&project_config),
        json!({"mcpServers": {"local": {"command": "node", "args": ["server.js"]}}})
    );
}

/// TS: `AbortSignal.timeout(ms)` 在 ms 非整数或超过 4294967295 时抛 RangeError；login 打印
/// "Sign-in ... failed: <message>"。
#[tokio::test]
async fn login_rejects_timeouts_that_abort_signal_timeout_rejects() {
    use kiana_builtin_extensions::mcp::oauth::McpOAuthCredentialStore;
    use kiana_config::auth::storage::InMemoryAuthStorageBackend;

    let mock_server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(wiremock::ResponseTemplate::new(401))
        .mount(&mock_server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("mcp.json"),
        serde_json::to_string(&json!({
            "mcpServers": {"remote": {"url": format!("{}/mcp", mock_server.uri())}}
        }))
        .unwrap(),
    )
    .unwrap();

    let cases = [
        (
            "0.0001",
            "The value of \"delay\" is out of range. It must be an integer. Received 0.1",
        ),
        (
            "5000000",
            "The value of \"delay\" is out of range. It must be >= 0 && <= 4294967295. Received 5_000_000_000",
        ),
    ];
    for (timeout, expected) in cases {
        let output: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log_sink = Arc::clone(&output);
        let log: Arc<dyn Fn(&str) + Send + Sync> =
            Arc::new(move |line: &str| log_sink.lock().unwrap().push(line.to_string()));
        let error_sink = Arc::clone(&output);
        let error: Arc<dyn Fn(&str) + Send + Sync> =
            Arc::new(move |line: &str| error_sink.lock().unwrap().push(line.to_string()));
        let exit_code = run_mcp_command(
            vec![
                "login".to_string(),
                "remote".to_string(),
                "--timeout".to_string(),
                timeout.to_string(),
            ],
            McpCommandOptions {
                cwd: dir.path().to_path_buf(),
                agent_dir: dir.path().to_path_buf(),
                credentials: Some(Arc::new(McpOAuthCredentialStore::new(
                    Some(InMemoryAuthStorageBackend::new().into()),
                    None,
                ))),
                open_url: None,
                log: Some(log),
                error: Some(error),
            },
        )
        .await;
        let output = output.lock().unwrap().join("\n");
        assert_eq!(exit_code, 1, "{timeout}: {output}");
        assert!(
            output.contains(&format!(
                "Sign-in to MCP server \"remote\" failed: {expected}"
            )),
            "{timeout}: {output}"
        );
    }
}
