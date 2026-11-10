//! `packages/env/test/ssh-external.test.ts` 的移植：真实 SSH 服务器（由 CI 的 `KIANA_ENV_SSH_HOST`
//! 等变量配置）。
//!
//! 未设置 `KIANA_ENV_SSH_HOST` 时两个套件都跳过（TS 的 `describe.skipIf(host === undefined)` 与
//! `if (host !== undefined) registerEnvConformance(...)`）。
//!
//! 与 TS 的差异：Rust 没有共享的 `beforeAll`，两个 `#[tokio::test]` 各自建立连接（都只在设置了
//! `KIANA_ENV_SSH_HOST` 时运行，因此正常 CI 里不会重复连接）。
//!
//! TS: packages/env/test/ssh-external.test.ts

mod common;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use kiana_durable::env::{ExecutionEnv, FileSystem, RemoveOptions};
use kiana_durable::testing::assertions::PanicAssertions;
use kiana_durable::testing::create_env_conformance;
use kiana_durable::testing::types::{EnvConformanceCase, EnvConformanceOptions, EnvUse};
use kiana_env_client::{
    Connection, RemoteExecutionEnv, RemoteExecutionEnvOptions, RemoteWatchOptions,
    SshConnectOptions, SshTarget, accept_host_key, connect_ssh, deploy_daemon, detect_platform,
    node_platform, scan_host_key,
};

fn target(root: &common::TempDir) -> Option<SshTarget> {
    let host = std::env::var("KIANA_ENV_SSH_HOST")
        .ok()
        .filter(|host| !host.is_empty())?;
    let port = std::env::var("KIANA_ENV_SSH_PORT")
        .ok()
        .and_then(|port| port.trim().parse::<u32>().ok());
    SshTarget {
        host,
        user: std::env::var("KIANA_ENV_SSH_USER").ok(),
        port,
        identity_file: std::env::var("KIANA_ENV_SSH_KEY").ok(),
        known_hosts_file: root.join("known_hosts"),
        host_key_alias: "kiana-env-external".to_owned(),
        ssh: std::env::var("KIANA_ENV_SSH_PROGRAM").ok(),
        config_file: None,
    }
    .into()
}

fn log(text: &str) {
    eprintln!("[ssh-external {}] {text}", std::process::id());
}

/// `connect()`：信任主机 key、探测平台、部署 daemon，然后连接。
async fn connect() -> Option<(
    Arc<Connection>,
    kiana_env_client::ssh::RemotePlatform,
    common::TempDir,
)> {
    let root = common::TempDir::new("kiana-env-ssh-external-");
    let target = target(&root)?;
    let daemon = common::daemon_path();
    log("scanning the host key");
    let (lines, _fingerprints) = match scan_host_key(&target).await {
        Ok(scanned) => scanned,
        Err(error) => panic!("scanHostKey failed: {error}"),
    };
    if let Err(error) = accept_host_key(&target, &lines).await {
        panic!("acceptHostKey failed: {error}");
    }
    log("detecting the platform");
    let remote = match detect_platform(&target).await {
        Ok(remote) => remote,
        Err(error) => panic!("detectPlatform failed: {error}"),
    };
    log("deploying");
    let deployed = match deploy_daemon(&target, &remote, Some(&daemon)).await {
        Ok(deployed) => deployed,
        Err(error) => panic!("deployDaemon failed: {error}"),
    };
    log(&format!("deployed {deployed}; connecting"));
    let mut options = SshConnectOptions::new(target);
    options.binary = Some(daemon);
    options.on_log = Some(Arc::new(|text: &str| {
        log(&format!("daemon: {}", text.trim()));
    }));
    let connected = match connect_ssh(options).await {
        Ok(connected) => connected,
        Err(error) => panic!("connectSsh failed: {error}"),
    };
    log("connected; saying hello");
    Some((connected.connection, remote, root))
}

/// `reaches the remote system through its default shell`
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reaches_the_remote_system_through_its_default_shell() {
    const NAME: &str = "reaches the remote system through its default shell";
    if std::env::var_os("KIANA_ENV_SSH_HOST").is_none() {
        eprintln!("skipping: KIANA_ENV_SSH_HOST is not set");
        return;
    }
    common::titled(NAME, async {
        let Some((connection, remote, _root)) = connect().await else {
            return;
        };
        let info = match connection.info().await {
            Ok(info) => info,
            Err(error) => panic!("{NAME}: info failed: {error}"),
        };
        assert_eq!(
            node_platform(&info).platform,
            common::node_process_platform(),
            "{NAME}"
        );
        if cfg!(windows) {
            let expected =
                std::env::var("KIANA_ENV_SSH_SHELL").unwrap_or_else(|_| "cmd".to_owned());
            assert_eq!(
                remote.shell.map(|shell| shell.as_str().to_owned()),
                Some(expected),
                "{NAME}: the daemon's shell"
            );
        }
        connection.close();
    })
    .await;
}

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

/// `RemoteExecutionEnv over SSH`：`cwd` 由远端 `createTempDir("kiana-env-conformance-")` 给出，
/// `watch: { pollIntervalMs: 100 }`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_execution_env_over_ssh() {
    let suite = "RemoteExecutionEnv over SSH";
    if std::env::var_os("KIANA_ENV_SSH_HOST").is_none() {
        eprintln!("skipping: KIANA_ENV_SSH_HOST is not set");
        return;
    }
    let Some((connection, _remote, _root)) = connect().await else {
        return;
    };
    let info = match connection.info().await {
        Ok(info) => info,
        Err(error) => panic!("{suite}: info failed: {error}"),
    };
    let home = info.home.clone();
    let provider_connection = Arc::clone(&connection);
    let provider_home = home.clone();
    let with_env: kiana_durable::testing::types::EnvConformanceProvider =
        Arc::new(move |use_env: EnvUse| {
            let connection = Arc::clone(&provider_connection);
            let home = provider_home.clone();
            Box::pin(async move {
                let root_env = RemoteExecutionEnv::new(RemoteExecutionEnvOptions {
                    connection: Arc::clone(&connection),
                    id: "kiana-env:external".to_owned(),
                    cwd: home,
                    shell_path: None,
                    shell_env: None,
                    watch: Some(RemoteWatchOptions {
                        poll_interval_ms: Some(100.0),
                        ..RemoteWatchOptions::default()
                    }),
                });
                let cwd = match FileSystem::create_temp_dir(
                    &root_env,
                    Some("kiana-env-conformance-"),
                    common::context(),
                )
                .await
                {
                    Ok(cwd) => cwd,
                    Err(error) => panic!("createTempDir failed: {}", error.message),
                };
                let env = RemoteExecutionEnv::new(RemoteExecutionEnvOptions {
                    connection: Arc::clone(&connection),
                    id: "kiana-env:external".to_owned(),
                    cwd: cwd.clone(),
                    shell_path: None,
                    shell_env: None,
                    watch: Some(RemoteWatchOptions {
                        poll_interval_ms: Some(100.0),
                        ..RemoteWatchOptions::default()
                    }),
                });
                let env: Arc<dyn ExecutionEnv> = Arc::new(env);
                use_env(env).await;
                let options = RemoveOptions {
                    recursive: Some(true),
                    force: Some(true),
                };
                let _ =
                    FileSystem::remove(&root_env, &cwd, Some(&options), common::context()).await;
            })
        });
    let options = EnvConformanceOptions {
        assertions: Arc::new(PanicAssertions),
        with_env,
        shell: windows_shell(),
        symlinks: if cfg!(windows) { Some(false) } else { None },
        list_files: Some(true),
    };
    let cases = create_env_conformance(&options);
    assert!(
        !cases.is_empty(),
        "env conformance suite `{suite}` must expose cases"
    );
    for case in cases {
        run_case(suite, case).await;
    }
    connection.close();
}
