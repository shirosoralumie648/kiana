//! `packages/coding-agent/test/rpc-client-process-exit.test.ts` 的 Rust 移植。
//!
//! TS 用临时 `child.mjs`（`process.stdin.once("data", () => process.exit(43))`）作为
//! `cliPath`，断言 `start()` 成功、随后 `getCommands()` 被拒绝且文本含
//! `Agent process exited (code=43 signal=null)`。
//!
//! Rust 侧用等价的 `#!/bin/sh` 脚本（读一行后 `exit 43`），可执行文件由
//! [`RpcClientOptions::cli_path`] 指定。仅 Unix（脚本依赖 shebang 与可执行位）。

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kiana_rpc_print::{RpcClient, RpcClientOptions};

/// 测试用临时目录（Drop 时递归删除）。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        );
        let path = std::env::temp_dir().join(format!("kiana-rpc-client-{tag}-{unique}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 写一个可执行 shell 脚本（TS 的 `writeChildScript`）。
fn write_child_script(dir: &Path, contents: &str) -> PathBuf {
    let path = dir.join("child.sh");
    std::fs::write(&path, contents).expect("write child script");
    let mut permissions = std::fs::metadata(&path)
        .expect("stat child script")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("chmod child script");
    path
}

/// 在单线程 runtime + `LocalSet` 上跑 future（客户端任务用 `spawn_local`）。
fn run_local<F: std::future::Future>(future: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime");
    tokio::task::LocalSet::new().block_on(&runtime, future)
}

#[test]
fn rejects_in_flight_request_when_child_exits() {
    run_local(async {
        let dir = TempDir::new("exit");
        let script = write_child_script(dir.path(), "#!/bin/sh\nread -r line\nexit 43\n");
        let client = RpcClient::new(RpcClientOptions {
            cli_path: Some(script),
            ..RpcClientOptions::default()
        });

        client.start().await.expect("start should succeed");

        let error = client
            .get_commands()
            .await
            .expect_err("get_commands should be rejected after the child exits");
        let message = error.to_string();
        assert!(
            message.starts_with("Agent process exited (code=43 signal=null). Stderr: "),
            "unexpected error text: {message:?}"
        );

        client.stop().await.expect("stop");
    });
}

/// 评审发现 5：子进程在 `stop()` 之前已经退出时，TS 的 `exit` 监听器不会再触发，
/// `stop()` 必须等满 1000ms 的 SIGKILL 宽限期（实测 TS 客户端 1002ms）。
#[test]
fn stop_waits_the_grace_period_when_the_child_already_exited() {
    run_local(async {
        let dir = TempDir::new("stop-exited");
        // 被信号杀死：子进程的 `exitCode` 为 null，`start()` 因此不抛错。
        let script = write_child_script(dir.path(), "#!/bin/sh\nkill -KILL $$\n");
        let client = RpcClient::new(RpcClientOptions {
            cli_path: Some(script),
            ..RpcClientOptions::default()
        });
        client
            .start()
            .await
            .expect("start succeeds for a signal exit");

        // `start()` 已经等了 100ms，supervisor 必然记录了退出信息。
        let started = std::time::Instant::now();
        client.stop().await.expect("stop");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(900),
            "stop() must wait the 1000ms grace period for an already-exited child: {elapsed:?}"
        );
    });
}
