//! 对应 `packages/env/test/remote.test.ts`（9 个用例）。
//!
//! 与 TS 的差异：
//! - `process.kill(pid, "SIGKILL")` 用 `kill -9`（仅 unix；Windows 上该用例跳过）。
//! - 旧 daemon 经 `sh` 包装重新执行本测试二进制（见 `common`）。
//! - `env.exec` 的参数是 [`ShellCommand`]；`withAbortSignal` → [`common::context_with_signal`]。
//! - 另有回归用例 `cleanup_kills_running_commands_without_aborting_them`：TS 没有覆盖
//!   `cleanup()` 的 kill 语义（CANCEL 帧的 `{mode:"kill"}`）。

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kiana_chord::context::{AbortSignal, Context};
use kiana_durable::env::{
    ExecutionErrorCode, FileContent, FileErrorCode, FileSystem, ListFilesOptions, Shell,
    ShellCommand, ShellExecOptions, ShellOutputInfo, ShellOutputWindow,
};
// 只由脚本 daemon 的 unix 回归用例使用。
#[cfg(unix)]
use kiana_durable::env::{FileError, LineRange};
use kiana_env_client::connection::ConnectionCommand;
use kiana_env_client::{
    Connection, RemoteExecutionEnv, RemoteExecutionEnvOptions, RemoteInfo, node_platform,
};

fn environment() -> (Arc<RemoteExecutionEnv>, Arc<Connection>, common::TempDir) {
    let dir = common::TempDir::new("kiana-env-remote-");
    let connection = common::connection();
    let env = Arc::new(RemoteExecutionEnv::new(RemoteExecutionEnvOptions {
        connection: Arc::clone(&connection),
        id: "kiana-env:test".to_owned(),
        cwd: dir.path().to_owned(),
        shell_path: None,
        shell_env: None,
        watch: None,
    }));
    (env, connection, dir)
}

/// transfers only about a window of output a caller keeps, with exact counts
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transfers_only_about_a_window_of_output_a_caller_keeps_with_exact_counts() {
    const NAME: &str = "transfers only about a window of output a caller keeps, with exact counts";
    common::titled(NAME, async {
        let (env, connection, _dir) = environment();
        let total: usize = 5_000_000;
        let delivered = Arc::new(AtomicUsize::new(0));
        let skipped = Arc::new(AtomicUsize::new(0));
        let tail = Arc::new(Mutex::new(String::new()));
        let callback_delivered = Arc::clone(&delivered);
        let callback_skipped = Arc::clone(&skipped);
        let callback_tail = Arc::clone(&tail);
        let options = ShellExecOptions {
            window: Some(ShellOutputWindow {
                max_bytes: 1000,
                max_lines: 20,
                min_interval_ms: 20.0,
                bytes_per_second: 1_000_000.0,
            }),
            on_output: Some(Arc::new(
                move |text: &str, _context: &dyn Context, info: &ShellOutputInfo| {
                    callback_delivered.fetch_add(text.len(), Ordering::SeqCst);
                    if let Some(skipped) = info.skipped {
                        callback_skipped.fetch_add(skipped.bytes, Ordering::SeqCst);
                        callback_tail.lock().expect("tail lock").clear();
                    }
                    callback_tail.lock().expect("tail lock").push_str(text);
                },
            )),
            ..ShellExecOptions::default()
        };
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            env.exec(
                ShellCommand::Argv(vec![
                    "sh".to_owned(),
                    "-c".to_owned(),
                    format!("yes 0123456789abcdef | head -c {total}"),
                ]),
                Some(&options),
                common::context(),
            ),
        )
        .await
        .expect("exec settles")
        .expect("exec");
        assert_eq!(result.exit_code, 0);
        let delivered = delivered.load(Ordering::SeqCst);
        let skipped = skipped.load(Ordering::SeqCst);
        assert_eq!(delivered + skipped, total);
        assert!(skipped > 0);
        assert!(delivered < total / 10);
        // After the last skip comes everything to the end, more than the window.
        let repeated = "0123456789abcdef\n".repeat(total.div_ceil(17));
        let full: String = repeated.chars().take(total).collect();
        let tail = tail.lock().expect("tail lock").clone();
        assert!(full.ends_with(&tail));
        assert!(tail.len() > 1000 || tail.matches('\n').count() > 20);
        connection.close();
    })
    .await;
}

/// aborts commands cancelled right after they are sent
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborts_commands_cancelled_right_after_they_are_sent() {
    const NAME: &str = "aborts commands cancelled right after they are sent";
    common::titled(NAME, async {
        let (env, connection, _dir) = environment();
        let started = Instant::now();
        for attempt in 0..20 {
            let controller = AbortSignal::new();
            let context = common::context_with_signal(&controller);
            let env = Arc::clone(&env);
            let running = tokio::spawn(async move {
                env.exec(
                    ShellCommand::Argv(vec!["sleep".to_owned(), "5".to_owned()]),
                    None,
                    context.as_ref(),
                )
                .await
            });
            if attempt % 2 == 0 {
                controller.abort_without_reason();
            } else {
                let controller = controller.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(attempt % 5)).await;
                    controller.abort_without_reason();
                });
            }
            let result = tokio::time::timeout(Duration::from_secs(10), running)
                .await
                .expect("the command settles")
                .expect("the task runs");
            match result {
                Ok(result) => panic!(
                    "attempt {attempt} must abort, got exit {}",
                    result.exit_code
                ),
                Err(error) => assert_eq!(
                    error.code,
                    ExecutionErrorCode::Aborted,
                    "attempt {attempt}: {}",
                    error.message
                ),
            }
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        connection.close();
    })
    .await;
}

/// reports the remote system in Node's names
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_the_remote_system_in_node_s_names() {
    const NAME: &str = "reports the remote system in Node's names";
    common::titled(NAME, async {
        let (_env, connection, _dir) = environment();
        let platform = node_platform(&connection.info().await.expect("info"));
        assert_eq!(platform.platform, common::node_process_platform());
        // CI runs the darwin-x64 release daemon on arm64 macOS through Rosetta, so it reports x64 to
        // arm64 Node.
        let rosetta = common::node_process_platform() == "darwin"
            && common::node_process_arch() == "arm64"
            && platform.arch == "x64";
        if !rosetta {
            assert_eq!(platform.arch, common::node_process_arch());
        }
        let info = RemoteInfo {
            os: "windows".to_owned(),
            arch: "aarch64".to_owned(),
            ..RemoteInfo::default()
        };
        let translated = node_platform(&info);
        assert_eq!(translated.platform, "win32");
        assert_eq!(translated.arch, "arm64");
        let info = RemoteInfo {
            os: "macos".to_owned(),
            arch: "x86_64".to_owned(),
            ..RemoteInfo::default()
        };
        let translated = node_platform(&info);
        assert_eq!(translated.platform, "darwin");
        assert_eq!(translated.arch, "x64");
        connection.close();
    })
    .await;
}

/// reports listFiles as not supported by daemons released before it
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_list_files_as_not_supported_by_daemons_released_before_it() {
    const NAME: &str = "reports listFiles as not supported by daemons released before it";
    common::titled(NAME, async {
        if !common::old_daemon_supported() {
            eprintln!("skipping: the old daemon needs a POSIX `sh` wrapper");
            return;
        }
        let (env, connection, _dir) = environment();
        let features = env.connection.info().await.expect("info").features;
        assert!(
            features
                .as_ref()
                .is_some_and(|features| features.iter().any(|feature| feature == "listFiles")),
            "the real daemon lists files"
        );
        let old_connection = Arc::new(Connection::new(common::old_daemon_command()));
        let old = RemoteExecutionEnv::new(RemoteExecutionEnvOptions {
            connection: Arc::clone(&old_connection),
            id: "kiana-env:old".to_owned(),
            cwd: env.cwd().to_owned(),
            shell_path: None,
            shell_env: None,
            watch: None,
        });
        let listed = old.list_files(".", None, common::context()).await;
        match listed {
            Ok(_) => panic!("an old daemon must not list files"),
            Err(error) => assert_eq!(error.code, FileErrorCode::NotSupported),
        }
        old_connection.close();
        connection.close();
    })
    .await;
}

/// cancels a listing in the daemon
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancels_a_listing_in_the_daemon() {
    const NAME: &str = "cancels a listing in the daemon";
    common::titled(NAME, async {
        let (env, connection, dir) = environment();
        for directory in 0..100 {
            let path = dir.join(&format!("d{directory}"));
            std::fs::create_dir(&path).expect("mkdir");
            for file in 0..100 {
                std::fs::write(format!("{path}/f{file}"), "").expect("write");
            }
        }
        // The cancel follows the request at once, bypassing the client's own abort check, so the daemon
        // stops the walk.
        let controller = AbortSignal::new();
        controller.abort_without_reason();
        let mut body = kiana_env_client::connection::Json::new();
        body.insert(
            "path".to_owned(),
            serde_json::Value::String(env.cwd().to_owned()),
        );
        let cancelled = connection
            .request(
                "listFiles",
                body,
                kiana_env_client::RequestOptions {
                    signal: Some(controller),
                    ..kiana_env_client::RequestOptions::default()
                },
            )
            .await;
        match cancelled {
            Ok(_) => panic!("an aborted listing must fail"),
            Err(error) => assert_eq!(error.code, "aborted"),
        }
        let listed = tokio::time::timeout(
            Duration::from_secs(30),
            env.list_files(".", None, common::context()),
        )
        .await
        .expect("listFiles settles")
        .expect("listFiles");
        assert_eq!(listed.entries.len(), 10_100);
        assert!(!listed.truncated);
        let options = ListFilesOptions {
            max_entries: Some(5000),
            ..ListFilesOptions::default()
        };
        let bounded = env
            .list_files(".", Some(&options), common::context())
            .await
            .expect("listFiles");
        assert_eq!(bounded.entries.len(), 5000);
        assert!(bounded.truncated);
        connection.close();
    })
    .await;
}

/// times out a command that produces no output
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn times_out_a_command_that_produces_no_output() {
    const NAME: &str = "times out a command that produces no output";
    common::titled(NAME, async {
        let (env, connection, _dir) = environment();
        let started = Instant::now();
        let options = ShellExecOptions {
            timeout: Some(0.3),
            ..ShellExecOptions::default()
        };
        let result = env
            .exec(
                ShellCommand::Argv(vec!["sleep".to_owned(), "5".to_owned()]),
                Some(&options),
                common::context(),
            )
            .await;
        match result {
            Ok(result) => panic!("the command must time out, got exit {}", result.exit_code),
            Err(error) => assert_eq!(error.code, ExecutionErrorCode::Timeout),
        }
        assert!(started.elapsed() < Duration::from_secs(3));
        connection.close();
    })
    .await;
}

/// fails requests on handles of a lost connection instead of reaching the new daemon
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fails_requests_on_handles_of_a_lost_connection_instead_of_reaching_the_new_daemon() {
    const NAME: &str =
        "fails requests on handles of a lost connection instead of reaching the new daemon";
    common::titled(NAME, async {
        let (env, connection, _dir) = environment();
        env.write_file(
            "a.txt",
            FileContent::Text("AAA".to_owned()),
            common::context(),
        )
        .await
        .expect("write a");
        env.write_file(
            "b.txt",
            FileContent::Text("BBB".to_owned()),
            common::context(),
        )
        .await
        .expect("write b");
        let a = env
            .open_binary_reader("a.txt", None, common::context())
            .await
            .expect("reader a");
        let pid = connection.info().await.expect("info").pid;
        let status = std::process::Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .status()
            .expect("kill runs");
        assert!(status.success(), "kill -9 succeeds");
        tokio::time::sleep(Duration::from_millis(300)).await;
        // The new daemon numbers its handles from 1 again.
        let b = env
            .open_binary_reader("b.txt", None, common::context())
            .await
            .expect("reader b");
        assert_ne!(connection.info().await.expect("info").pid, pid);
        let stale = a.read(0, 3, common::context()).await;
        match stale {
            Ok(_) => panic!("a handle of the old session must fail"),
            Err(error) => assert_eq!(error.code, FileErrorCode::Unknown),
        }
        assert_eq!(
            b.read(0, 3, common::context()).await.expect("read b"),
            b"BBB".to_vec()
        );
        a.close(common::context()).await;
        b.close(common::context()).await;
        connection.close();
    })
    .await;
}

/// computes the command at each start and retries a start that failed
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn computes_the_command_at_each_start_and_retries_a_start_that_failed() {
    const NAME: &str = "computes the command at each start and retries a start that failed";
    common::titled(NAME, async {
        let dir = common::TempDir::new("kiana-env-remote-");
        let starts = Arc::new(AtomicUsize::new(0));
        let callback_starts = Arc::clone(&starts);
        let daemon = common::daemon_path();
        let command = ConnectionCommand::Dynamic(Arc::new(move || {
            let starts = Arc::clone(&callback_starts);
            let daemon = daemon.clone();
            Box::pin(async move {
                let start = starts.fetch_add(1, Ordering::SeqCst) + 1;
                if start == 1 {
                    return Err(Box::<dyn std::error::Error + Send + Sync>::from(
                        "ssh gpu-box failed: Network is unreachable",
                    ));
                }
                Ok(vec![daemon])
            })
        }));
        let connection = Arc::new(Connection::new(command));
        let env = RemoteExecutionEnv::new(RemoteExecutionEnvOptions {
            connection: Arc::clone(&connection),
            id: "kiana-env:test".to_owned(),
            cwd: dir.path().to_owned(),
            shell_path: None,
            shell_env: None,
            watch: None,
        });
        let offline = env.read_text_file("missing.txt", common::context()).await;
        match offline {
            Ok(_) => panic!("the first start fails"),
            Err(error) => {
                assert_eq!(error.code, FileErrorCode::Unknown);
                assert_eq!(error.message, "ssh gpu-box failed: Network is unreachable");
            }
        }
        let command = env
            .exec(
                ShellCommand::Argv(vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()]),
                None,
                common::context(),
            )
            .await;
        match command {
            Ok(result) => assert_eq!(result.exit_code, 0),
            Err(error) => panic!("the retried start runs: {}", error.message),
        }
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        connection.close();
    })
    .await;
}

/// writes and reads files larger than one transfer chunk in order
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writes_and_reads_files_larger_than_one_transfer_chunk_in_order() {
    const NAME: &str = "writes and reads files larger than one transfer chunk in order";
    common::titled(NAME, async {
        let (env, connection, _dir) = environment();
        let content: Vec<u8> = (0..3_500_017u32)
            .map(|index| ((u64::from(index) * 7919) % 251) as u8)
            .collect();
        let started = Instant::now();
        env.write_file(
            "big.bin",
            FileContent::Bytes(content.clone()),
            common::context(),
        )
        .await
        .expect("write");
        env.append_file(
            "big.bin",
            FileContent::Bytes(content.clone()),
            common::context(),
        )
        .await
        .expect("append");
        let read = env
            .read_binary_file("big.bin", common::context())
            .await
            .expect("read");
        assert_eq!(read.len(), content.len() * 2);
        assert_eq!(&read[..content.len()], &content[..]);
        assert_eq!(&read[content.len()..], &content[..]);
        let reader = env
            .open_binary_reader("big.bin", None, common::context())
            .await
            .expect("reader");
        let range = reader
            .read(1_000_003, 2_000_000, common::context())
            .await
            .expect("range");
        reader.close(common::context()).await;
        assert_eq!(&range[..], &read[1_000_003..3_000_003]);
        assert!(started.elapsed() < Duration::from_secs(60));
        connection.close();
    })
    .await;
}

/// 回归（review finding）：`cleanup()` 用 CANCEL `{mode:"kill"}` 结束在跑的命令，命令以**被杀的
/// 退出码**结算（`Ok`），而不是作为 `aborted` 错误（remote-env.ts:950-954）。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cleanup_kills_running_commands_without_aborting_them() {
    const NAME: &str = "cleanup kills running commands without aborting them";
    common::titled(NAME, async {
        let (env, connection, _dir) = environment();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<()>();
        let running = {
            let env = Arc::clone(&env);
            tokio::spawn(async move {
                let options = ShellExecOptions {
                    on_output: Some(Arc::new(
                        move |_text: &str, _context: &dyn Context, _info: &ShellOutputInfo| {
                            let _ = sender.send(());
                        },
                    )),
                    ..ShellExecOptions::default()
                };
                env.exec(
                    ShellCommand::Argv(vec![
                        "sh".to_owned(),
                        "-c".to_owned(),
                        "echo started; exec sleep 30".to_owned(),
                    ]),
                    Some(&options),
                    common::context(),
                )
                .await
            })
        };
        tokio::time::timeout(Duration::from_secs(20), receiver.recv())
            .await
            .expect("the command starts and produces output")
            .expect("the output channel stays open");
        FileSystem::cleanup(&*env, common::context()).await;
        let result = tokio::time::timeout(Duration::from_secs(20), running)
            .await
            .expect("the command settles after cleanup")
            .expect("the exec task does not panic");
        match result {
            Ok(result) => assert_eq!(result.exit_code, 137, "{NAME}: the killed process's status"),
            Err(error) => panic!(
                "{NAME}: cleanup must kill, not abort: {} ({:?})",
                error.message, error.code
            ),
        }
        connection.close();
    })
    .await;
}

// ---------------------------------------------------------------------------
// review findings 的回归：脚本 daemon（tests/common::FakeDaemonConfig）
// ---------------------------------------------------------------------------

/// 脚本 daemon 上的 `RemoteExecutionEnv`，cwd 是假的 `/work`。
#[cfg(unix)]
fn fake_environment(
    config: common::FakeDaemonConfig,
) -> (
    Arc<RemoteExecutionEnv>,
    Arc<Connection>,
    common::TempDir,
    String,
) {
    let dir = common::TempDir::new("kiana-env-fake-");
    let log = dir.join("frames.log");
    let mut config = config;
    config.log = Some(log.clone());
    let config_file = common::write_fake_daemon_config(&dir, &config);
    let connection = Arc::new(Connection::new(common::fake_daemon_command(&config_file)));
    let env = Arc::new(RemoteExecutionEnv::new(RemoteExecutionEnvOptions {
        connection: Arc::clone(&connection),
        id: "kiana-env:test".to_owned(),
        cwd: "/work".to_owned(),
        shell_path: None,
        shell_env: None,
        watch: None,
    }));
    (env, connection, dir, log)
}

/// 客户端发给脚本 daemon 的帧（`{kind,id,json}`），按到达顺序。
#[cfg(unix)]
fn read_frames(log: &str) -> Vec<serde_json::Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(unix)]
fn frame_op(frame: &serde_json::Value) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(frame.get("json")?.as_str()?).ok()?;
    json.get("op")?.as_str().map(str::to_owned)
}

#[cfg(unix)]
fn assert_invalid<T: std::fmt::Debug>(
    result: &Result<T, FileError>,
    path: &str,
    message: &str,
    what: &str,
) {
    match result {
        Ok(value) => panic!("{what} must fail before a request, got {value:?}"),
        Err(error) => {
            assert_eq!(error.code, FileErrorCode::Invalid, "{what}: code");
            assert_eq!(error.message, message, "{what}: message");
            assert_eq!(error.path.as_deref(), Some(path), "{what}: path");
        }
    }
}

/// 回归（review finding）：`Number.isSafeInteger` 在 `u64`/`usize` 上**仍可达**（`2^53` 以上
/// 的值能传进来）。TS 在发请求前以 `invalid` 拒绝（remote-env.ts:190-192,216-221,258-260,644-646）。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_parameters_above_the_safe_integer_range_without_a_request() {
    const NAME: &str = "rejects parameters above Number.MAX_SAFE_INTEGER before sending a request";
    common::titled(NAME, async {
        let (env, connection, _dir, log) = fake_environment(common::FakeDaemonConfig::default());
        const MAX: u64 = 9_007_199_254_740_991;
        let reader = env
            .open_binary_reader("a", None, common::context())
            .await
            .expect("open a binary reader");
        assert_invalid(
            &reader.read(MAX + 1, 1, common::context()).await,
            "/work/a",
            "Offset and length must be non-negative safe integers",
            "read with an unsafe offset",
        );
        assert_invalid(
            &reader.read(0, MAX as usize + 1, common::context()).await,
            "/work/a",
            "Offset and length must be non-negative safe integers",
            "read with an unsafe length",
        );
        assert_invalid(
            &reader
                .scan_lines(
                    LineRange {
                        start_line: MAX + 1,
                        end_line: None,
                    },
                    common::context(),
                )
                .await,
            "/work/a",
            "Invalid line range",
            "scanLines with an unsafe start",
        );
        assert_invalid(
            &reader
                .scan_lines(
                    LineRange {
                        start_line: 0,
                        end_line: Some(MAX + 1),
                    },
                    common::context(),
                )
                .await,
            "/work/a",
            "Invalid line range",
            "scanLines with an unsafe end",
        );
        reader.close(common::context()).await;
        let directory = env
            .open_dir_reader(".", common::context())
            .await
            .expect("open a directory reader");
        assert_invalid(
            &directory.next(MAX as usize + 1, common::context()).await,
            "/work",
            "maxEntries must be a positive safe integer",
            "next with an unsafe maxEntries",
        );
        directory.close(common::context()).await;
        assert_invalid(
            &env.truncate_file("a", MAX + 1, common::context()).await,
            "/work/a",
            "File size must be a non-negative safe integer",
            "truncateFile with an unsafe size",
        );
        // None of the rejected calls may reach the daemon.
        let frames = read_frames(&log);
        for frame in &frames {
            if frame.get("kind").and_then(serde_json::Value::as_u64) != Some(1) {
                continue;
            }
            let op = frame_op(frame).unwrap_or_default();
            assert!(
                !matches!(op.as_str(), "pread" | "scanLines" | "truncate" | "readdir"),
                "the rejected {op} request must not be sent"
            );
        }
        assert!(
            !frames
                .iter()
                .any(|frame| frame.get("kind").and_then(serde_json::Value::as_u64) == Some(5)),
            "no CANCEL frame is sent"
        );
        connection.close();
    })
    .await;
}

/// 回归（review finding）：`listDir` 的条目错误属于**条目**，不是目录
/// （remote-env.ts:712-718：`toFileError(new RemoteError(entry.error), entryPath)`）。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_dir_reports_entry_errors_against_the_entry_path() {
    const NAME: &str = "listDir reports an entry error against the entry path";
    common::titled(NAME, async {
        let (env, connection, _dir, _log) = fake_environment(common::FakeDaemonConfig {
            readdir_entry_error: true,
            ..common::FakeDaemonConfig::default()
        });
        match env.list_dir(".", common::context()).await {
            Ok(infos) => panic!("{NAME}: the entry error must fail the listing, got {infos:?}"),
            Err(error) => {
                assert_eq!(error.code, FileErrorCode::PermissionDenied);
                assert_eq!(error.message, "permission denied");
                assert_eq!(error.path.as_deref(), Some("/work/bad"));
            }
        }
        connection.close();
    })
    .await;
}

/// 回归（review finding）：`listDir` 在**每个条目**前检查 abort（remote-env.ts:712-713），
/// 因此 listing 进行中被 abort 时，abort 胜过条目错误，路径是目录。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_dir_honors_an_abort_before_an_entry_error() {
    const NAME: &str = "listDir honors an abort before an entry error";
    common::titled(NAME, async {
        let (env, connection, _dir, _log) = fake_environment(common::FakeDaemonConfig {
            readdir_delay_ms: 500,
            readdir_entry_error: true,
            ..common::FakeDaemonConfig::default()
        });
        let controller = AbortSignal::new();
        let context = common::context_with_signal(&controller);
        let aborting = {
            let controller = controller.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                controller.abort_without_reason();
            })
        };
        let result = env.list_dir(".", context.as_ref()).await;
        aborting.await.expect("the abort task runs");
        match result {
            Ok(infos) => panic!("{NAME}: an aborted listing must fail, got {infos:?}"),
            Err(error) => {
                assert_eq!(error.code, FileErrorCode::Aborted);
                assert_eq!(error.message, "aborted");
                assert_eq!(error.path.as_deref(), Some("/work"));
            }
        }
        connection.close();
    })
    .await;
}

/// 回归（review finding）：`BinaryReader.info` 的 `fstat` 与 `DirReader.next` 的 `readdir`
/// 不带调用方的 signal（remote-env.ts:180,266）；abort 只在之前/之后检查。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readers_do_not_cancel_the_daemons_fstat_or_readdir() {
    const NAME: &str = "reader requests do not carry the caller's abort signal";
    common::titled(NAME, async {
        let (env, connection, _dir, log) = fake_environment(common::FakeDaemonConfig {
            fstat_delay_ms: 500,
            readdir_delay_ms: 500,
            cancel_errors: true,
            readdir_entries: Some(serde_json::json!([{
                "name": "x",
                "info": {"kind": "file", "size": 1, "mtimeSec": 0, "mtimeNsec": 0},
            }])),
            ..common::FakeDaemonConfig::default()
        });
        let reader = env
            .open_binary_reader("a", None, common::context())
            .await
            .expect("open a binary reader");
        let controller = AbortSignal::new();
        let context = common::context_with_signal(&controller);
        let aborting = {
            let controller = controller.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                controller.abort_without_reason();
            })
        };
        // TS 只在 fstat 之前检查 abort：慢的 fstat 完成后仍然返回 FileInfo。
        let info = reader.info(context.as_ref()).await;
        aborting.await.expect("the abort task runs");
        match info {
            Ok(info) => {
                assert_eq!(info.path, "/work/a");
                assert_eq!(info.name, "a");
            }
            Err(error) => panic!(
                "{NAME}: fstat must not be cancelled, got {} ({:?})",
                error.message, error.code
            ),
        }
        reader.close(common::context()).await;

        let directory = env
            .open_dir_reader(".", common::context())
            .await
            .expect("open a directory reader");
        let controller = AbortSignal::new();
        let context = common::context_with_signal(&controller);
        let aborting = {
            let controller = controller.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                controller.abort_without_reason();
            })
        };
        // TS 在每页之后检查 abort：这一页仍然完成，结果以 aborted 结束。
        let page = directory.next(10, context.as_ref()).await;
        aborting.await.expect("the abort task runs");
        match page {
            Ok(page) => panic!("{NAME}: the abort after the page must win, got {page:?}"),
            Err(error) => {
                assert_eq!(error.code, FileErrorCode::Aborted);
                assert_eq!(error.path.as_deref(), Some("/work"));
            }
        }
        directory.close(common::context()).await;

        let frames = read_frames(&log);
        assert!(
            !frames
                .iter()
                .any(|frame| frame.get("kind").and_then(serde_json::Value::as_u64) == Some(5)),
            "fstat/readdir carry no signal, so no CANCEL frame is sent"
        );
        connection.close();
    })
    .await;
}

/// 回归（review finding）：`setInterval(…, 5000)` 的第一次触发在 5 s 之后
/// （connection.ts:301-306）；客户端在 hello 之后不会立刻发一个 PING。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn does_not_ping_immediately_after_hello() {
    const NAME: &str = "the first ping waits five seconds";
    common::titled(NAME, async {
        let (env, connection, _dir, log) = fake_environment(common::FakeDaemonConfig::default());
        env.file_info("a", common::context())
            .await
            .expect("lstat over the scripted daemon");
        tokio::time::sleep(Duration::from_millis(300)).await;
        let frames = read_frames(&log);
        assert!(
            !frames
                .iter()
                .any(|frame| frame.get("kind").and_then(serde_json::Value::as_u64) == Some(6)),
            "the first PING must arrive after 5 s, not immediately"
        );
        connection.close();
    })
    .await;
}

/// 回归（review finding）：`RemoteWatcher.open` 只由 `ready` 事件完成；请求不带 ready
/// 结算时，TS 的 promise 永不结算（watch.ts:66-118），而不是当作已打开。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_open_does_not_settle_without_a_ready_event() {
    const NAME: &str = "a watch without a ready event never settles";
    common::titled(NAME, async {
        let (env, connection, _dir, _log) = fake_environment(common::FakeDaemonConfig {
            watch_no_ready: true,
            watch_result: true,
            ..common::FakeDaemonConfig::default()
        });
        let watching = tokio::spawn(async move {
            env.watch(
                &[],
                std::sync::Arc::new(|_change: kiana_durable::env::WatchChange| {}),
                common::context(),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !watching.is_finished(),
            "{NAME}: the open must stay pending without a ready event"
        );
        watching.abort();
        let _ = watching.await;
        connection.close();
    })
    .await;
}

/// 回归（review finding）：`onOutput` 回调抛错时，TS 的 `ExecutionError` 的 cause 是那个错误
/// （remote-env.ts:913-919：`new ExecutionError("callback_error", cause.message, cause)`）。
/// Rust 的回调只能 panic，panic payload 的文本就是 cause.message。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_keeps_the_panicking_callback_as_the_cause() {
    const NAME: &str = "exec keeps the panicking callback as the cause";
    common::titled(NAME, async {
        let (env, connection, _dir) = environment();
        let options = ShellExecOptions {
            on_output: Some(Arc::new(
                |_text: &str, _context: &dyn Context, _info: &ShellOutputInfo| {
                    panic!("callback boom")
                },
            )),
            ..ShellExecOptions::default()
        };
        // `Shell` 由 daemon 的默认 shell 执行，两个平台都能产生输出。
        let result = env
            .exec(
                ShellCommand::Shell("echo hi".to_owned()),
                Some(&options),
                common::context(),
            )
            .await;
        match result {
            Ok(result) => {
                panic!("{NAME}: the callback error must fail the command, got {result:?}")
            }
            Err(error) => {
                assert_eq!(error.code, ExecutionErrorCode::CallbackError);
                assert_eq!(error.message, "callback boom");
                let cause = error.cause.expect("the callback error is the cause");
                assert_eq!(cause.to_string(), "callback boom");
            }
        }
        connection.close();
    })
    .await;
}
