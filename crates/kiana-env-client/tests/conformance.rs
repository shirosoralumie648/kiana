//! `packages/env/test/conformance.test.ts` 的移植。
//!
//! 两个套件：`RemoteExecutionEnv over a pipe` 与 `LocalExecutionEnv`，都用
//! `watch: { pollIntervalMs: 100 }`。Rust 没有动态测试注册，用例数组由
//! [`create_env_conformance`] 返回，这里逐个执行并把套件名与用例名放进失败消息（与
//! `registerEnvConformance` 的 `it` 标题一致）。
//!
//! 与 TS 的差异：
//! - TS 让两个套件共享一个 `Connection`（`afterAll` 关闭）；Rust 每个 `#[tokio::test]` 各建一个，
//!   结束时关闭（行为上不可观察）。
//! - 清理用 `tempfile::TempDir` 的 Drop（等价于 `rmSync(..., { recursive: true, force: true,
//!   maxRetries: 50, retryDelay: 100 })`）。
//! - Windows 上 shell 用 `%ProgramFiles%\Git\bin\bash.exe -c`、`symlinks: false`（Git Bash 的
//!   `ln -s` 会复制而不是链接），与 TS 相同。
//!
//! TS: packages/env/test/conformance.test.ts

mod common;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use kiana_durable::env::ExecutionEnv;
use kiana_durable::env::node::NodeWatchOptions;
use kiana_durable::testing::assertions::PanicAssertions;
use kiana_durable::testing::create_env_conformance;
use kiana_durable::testing::types::{EnvConformanceCase, EnvConformanceOptions, EnvUse};
use kiana_env_client::{
    Connection, LocalExecutionEnv, LocalExecutionEnvOptions, RemoteExecutionEnv,
    RemoteExecutionEnvOptions, RemoteWatchOptions,
};

fn windows_shell() -> Option<Vec<String>> {
    if cfg!(windows) {
        let program_files =
            std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".to_owned());
        Some(vec![
            std::path::Path::new(&program_files)
                .join("Git")
                .join("bin")
                .join("bash.exe")
                .to_string_lossy()
                .into_owned(),
            "-c".to_owned(),
        ])
    } else {
        None
    }
}

/// `{ listFiles: true }`（Windows 上再加 Git Bash 与 `symlinks: false`）。
fn options(
    with_env: kiana_durable::testing::types::EnvConformanceProvider,
) -> EnvConformanceOptions {
    EnvConformanceOptions {
        assertions: Arc::new(PanicAssertions),
        with_env,
        shell: windows_shell(),
        symlinks: if cfg!(windows) { Some(false) } else { None },
        list_files: Some(true),
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
        })
        .unwrap_or_else(|| "(non-string panic payload)".to_owned())
}

async fn run_case(suite: &str, case: EnvConformanceCase) {
    let name = case.name.clone();
    let timeout_ms = case.timeout_ms;
    let run = (case.run)();
    let guarded = AssertUnwindSafe(run).catch_unwind();
    let outcome = match timeout_ms {
        Some(timeout_ms) => {
            match tokio::time::timeout(Duration::from_millis(timeout_ms), guarded).await {
                Ok(outcome) => outcome,
                Err(_) => panic!(
                    "env conformance case `{name}` of `{suite}` timed out after {timeout_ms}ms"
                ),
            }
        }
        None => guarded.await,
    };
    if let Err(payload) = outcome {
        panic!(
            "env conformance case `{name}` of `{suite}` failed: {}",
            panic_message(payload)
        );
    }
}

async fn run_conformance(suite: &str, options: EnvConformanceOptions) {
    let cases = create_env_conformance(&options);
    assert!(
        !cases.is_empty(),
        "env conformance suite `{suite}` must expose cases"
    );
    for case in cases {
        run_case(suite, case).await;
    }
}

fn directory_path(directory: &tempfile::TempDir) -> String {
    directory.path().to_string_lossy().into_owned()
}

/// `RemoteExecutionEnv over a pipe`：`RemoteExecutionEnv({ connection, id: "kiana-env:test", cwd,
/// watch: { pollIntervalMs: 100 } })`，`cwd = mkdtemp("kiana-env-conformance-")`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_execution_env_over_a_pipe() {
    let suite = "RemoteExecutionEnv over a pipe";
    let connection = Arc::new(Connection::new(vec![common::daemon_path()]));
    let provider_connection = Arc::clone(&connection);
    let with_env: kiana_durable::testing::types::EnvConformanceProvider =
        Arc::new(move |use_env: EnvUse| {
            let connection = Arc::clone(&provider_connection);
            Box::pin(async move {
                let cwd = tempfile::Builder::new()
                    .prefix("kiana-env-conformance-")
                    .tempdir()
                    .expect("create a temporary directory");
                let env = RemoteExecutionEnv::new(RemoteExecutionEnvOptions {
                    connection,
                    id: "kiana-env:test".to_owned(),
                    cwd: directory_path(&cwd),
                    shell_path: None,
                    shell_env: None,
                    watch: Some(RemoteWatchOptions {
                        poll_interval_ms: Some(100.0),
                        ..RemoteWatchOptions::default()
                    }),
                });
                let env: Arc<dyn ExecutionEnv> = Arc::new(env);
                use_env(env).await;
                drop(cwd);
            })
        });
    run_conformance(suite, options(with_env)).await;
    connection.close();
}

/// `LocalExecutionEnv`：`LocalExecutionEnv({ connection, cwd, watch: { pollIntervalMs: 100 } })`，
/// `cwd = mkdtemp("kiana-env-local-conformance-")`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_execution_env() {
    let suite = "LocalExecutionEnv";
    let connection = Arc::new(Connection::new(vec![common::daemon_path()]));
    let provider_connection = Arc::clone(&connection);
    let with_env: kiana_durable::testing::types::EnvConformanceProvider =
        Arc::new(move |use_env: EnvUse| {
            let connection = Arc::clone(&provider_connection);
            Box::pin(async move {
                let cwd = tempfile::Builder::new()
                    .prefix("kiana-env-local-conformance-")
                    .tempdir()
                    .expect("create a temporary directory");
                let env = LocalExecutionEnv::new(LocalExecutionEnvOptions {
                    connection,
                    cwd: directory_path(&cwd),
                    shell_path: None,
                    shell_env: None,
                    watch: Some(NodeWatchOptions {
                        poll_interval_ms: Some(100.0),
                        ..NodeWatchOptions::default()
                    }),
                });
                let env: Arc<dyn ExecutionEnv> = Arc::new(env);
                use_env(env).await;
                drop(cwd);
            })
        });
    run_conformance(suite, options(with_env)).await;
    connection.close();
}
