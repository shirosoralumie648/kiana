//! 对应 `packages/env/test/terminal.test.ts`（7 个用例；整个套件仅在非 Windows
//! 上运行，与 TS 的 `describe.skipIf(process.platform === "win32")` 相同）。
//!
//! 与 TS 的差异：
//! - `collector()` 的 `waitFor` 用 `Notify` + 10 ms 轮询（TS 用挂起的 promise 列表），20 s 后
//!   panic 并带上等待的文本。
//! - `Buffer.toString("latin1")` 用逐字节转 `char` 实现（逐字节比较）。
//! - 保留的 `sleep 100` 子进程由 `close()`/连接关闭清理，与 TS 相同。
//!
//! TS: packages/env/test/terminal.test.ts

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use kiana_env_client::connection::Json;
use kiana_env_client::{
    Connection, RemoteTerminal, RequestOptions, TerminalOptions, open_terminal,
};
use tokio::sync::Notify;

/// `collector()`：收集原始字节，`waitFor(text)` 在 latin1 文本里等待它出现。
#[derive(Clone)]
struct Collector {
    bytes: Arc<Mutex<Vec<u8>>>,
    notify: Arc<Notify>,
}

impl Collector {
    fn new() -> Self {
        Collector {
            bytes: Arc::new(Mutex::new(Vec::new())),
            notify: Arc::new(Notify::new()),
        }
    }

    fn on_data(&self) -> Arc<dyn Fn(Vec<u8>) + Send + Sync> {
        let bytes = Arc::clone(&self.bytes);
        let notify = Arc::clone(&self.notify);
        Arc::new(move |chunk: Vec<u8>| {
            bytes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(&chunk);
            notify.notify_waiters();
        })
    }

    fn bytes(&self) -> Vec<u8> {
        self.bytes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// `Buffer.concat(chunks).toString("latin1")`。
    fn text(&self) -> String {
        self.bytes().iter().map(|byte| *byte as char).collect()
    }

    async fn wait_for(&self, text: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if self.text().contains(text) {
                return;
            }
            let notified = self.notify.notified();
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                panic!("terminal output did not contain {text:?} within 20s");
            }
            let _ = tokio::time::timeout(remaining.min(Duration::from_millis(10)), notified).await;
        }
    }
}

fn environment() -> (Arc<Connection>, common::TempDir) {
    let dir = common::TempDir::new("kiana-env-terminal-");
    let connection = common::connection();
    (connection, dir)
}

fn terminal_env() -> IndexMap<String, String> {
    let mut env = IndexMap::new();
    env.insert("PS1".to_owned(), "$ ".to_owned());
    env.insert("TERM".to_owned(), "xterm-256color".to_owned());
    env
}

/// 20 s 的用例超时（TS 的 `it(..., 20_000)`）。
async fn within_20_seconds<F>(name: &str, run: F)
where
    F: std::future::Future<Output = ()>,
{
    match tokio::time::timeout(Duration::from_secs(20), common::titled(name, run)).await {
        Ok(()) => {}
        Err(_) => panic!("terminal test `{name}` timed out after 20s"),
    }
}

/// `runs a program in a terminal with input, resizing and its exit code`
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_a_program_in_a_terminal_with_input_resizing_and_its_exit_code() {
    const NAME: &str = "runs a program in a terminal with input, resizing and its exit code";
    within_20_seconds(NAME, async {
        let (connection, dir) = environment();
        let output = Collector::new();
        let terminal = open_terminal(
            Arc::clone(&connection),
            TerminalOptions {
                argv: Some(vec!["/bin/sh".to_owned()]),
                env: Some(terminal_env()),
                ..TerminalOptions::new(dir.path(), 80, 24, output.on_data())
            },
        )
        .await
        .expect("open a terminal");
        assert!(terminal.pid > 0, "{NAME}: the pid is positive");
        terminal.write("test -t 0 && echo IS-A-TTY; stty size; echo $TERM\n");
        output.wait_for("IS-A-TTY").await;
        output.wait_for("24 80").await;
        output.wait_for("xterm-256color").await;
        terminal.resize(100, 30);
        terminal.write("stty size\n");
        output.wait_for("30 100").await;
        terminal.write("exit 3\n");
        assert_eq!(
            terminal.exited().await.expect("the exit status"),
            3,
            "{NAME}"
        );
        connection.close();
    })
    .await;
}

/// `reports the job in the foreground, not the shell at its prompt`
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_the_job_in_the_foreground_not_the_shell_at_its_prompt() {
    const NAME: &str = "reports the job in the foreground, not the shell at its prompt";
    within_20_seconds(NAME, async {
        let (connection, dir) = environment();
        let output = Collector::new();
        let terminal = open_terminal(
            Arc::clone(&connection),
            TerminalOptions {
                // Interactive, for job control: the shell puts each job in front of the terminal.
                argv: Some(vec!["/bin/sh".to_owned(), "-i".to_owned()]),
                env: Some(terminal_env()),
                ..TerminalOptions::new(dir.path(), 80, 24, output.on_data())
            },
        )
        .await
        .expect("open a terminal");
        terminal.write("echo READY\n");
        output.wait_for("READY").await;
        assert_eq!(
            terminal.foreground().await.expect("the foreground job"),
            None,
            "{NAME}: the shell at its prompt has no foreground job"
        );
        terminal.write("sleep 100\n");
        // The shell forks before it execs: wait until the foreground job is the command itself, not
        // the forked shell.
        let named = cfg!(any(target_os = "macos", target_os = "linux"));
        let mut foreground = terminal.foreground().await.expect("the foreground job");
        let mut tries = 0;
        while (foreground.is_none()
            || (named && foreground.as_ref().and_then(|job| job.name.as_deref()) != Some("sleep")))
            && tries < 100
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
            foreground = terminal.foreground().await.expect("the foreground job");
            tries += 1;
        }
        let foreground = foreground.expect("a foreground job runs");
        assert!(foreground.pid > 0, "{NAME}: the foreground pid is positive");
        assert_ne!(
            foreground.pid, terminal.pid,
            "{NAME}: the job is not the shell"
        );
        if named {
            assert_eq!(foreground.name.as_deref(), Some("sleep"), "{NAME}");
        }
        terminal.close();
        match terminal.exited().await {
            Ok(code) => panic!("{NAME}: close must abort, got exit {code}"),
            Err(error) => assert_eq!(error.code, "aborted", "{NAME}: {}", error.message()),
        }
        match terminal.foreground().await {
            Ok(foreground) => {
                panic!("{NAME}: a closed terminal has no foreground job: {foreground:?}")
            }
            Err(error) => assert_eq!(error.code, "EBADF", "{NAME}: {}", error.message()),
        }
        connection.close();
    })
    .await;
}

/// `delivers raw bytes, not decoded text`
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivers_raw_bytes_not_decoded_text() {
    const NAME: &str = "delivers raw bytes, not decoded text";
    within_20_seconds(NAME, async {
        let (connection, dir) = environment();
        let output = Collector::new();
        let terminal = open_terminal(
            Arc::clone(&connection),
            TerminalOptions {
                argv: Some(vec![
                    "sh".to_owned(),
                    "-c".to_owned(),
                    "printf 'a\\377b'".to_owned(),
                ]),
                ..TerminalOptions::new(dir.path(), 80, 24, output.on_data())
            },
        )
        .await
        .expect("open a terminal");
        assert_eq!(
            terminal.exited().await.expect("the exit status"),
            0,
            "{NAME}"
        );
        assert_eq!(output.bytes(), vec![0x61, 0xff, 0x62], "{NAME}");
        connection.close();
    })
    .await;
}

/// `keeps input in order`
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_input_in_order() {
    const NAME: &str = "keeps input in order";
    within_20_seconds(NAME, async {
        let (connection, dir) = environment();
        let output = Collector::new();
        let terminal = open_terminal(
            Arc::clone(&connection),
            TerminalOptions {
                argv: Some(vec![
                    "sh".to_owned(),
                    "-c".to_owned(),
                    "head -c 40000 > input.txt".to_owned(),
                ]),
                ..TerminalOptions::new(dir.path(), 80, 24, output.on_data())
            },
        )
        .await
        .expect("open a terminal");
        let mut text = String::new();
        for index in 0..4000 {
            let piece = format!("{index:09}\n");
            text.push_str(&piece);
            terminal.write(&piece);
        }
        assert_eq!(
            terminal.exited().await.expect("the exit status"),
            0,
            "{NAME}"
        );
        let written = std::fs::read_to_string(dir.join("input.txt")).expect("input.txt");
        assert_eq!(written, text, "{NAME}");
        connection.close();
    })
    .await;
}

/// `kills the program on close`
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kills_the_program_on_close() {
    const NAME: &str = "kills the program on close";
    within_20_seconds(NAME, async {
        let (connection, dir) = environment();
        let terminal: RemoteTerminal = open_terminal(
            Arc::clone(&connection),
            TerminalOptions {
                argv: Some(vec!["sleep".to_owned(), "100".to_owned()]),
                ..TerminalOptions::new(dir.path(), 80, 24, Arc::new(|_bytes: Vec<u8>| {}))
            },
        )
        .await
        .expect("open a terminal");
        terminal.close();
        match terminal.exited().await {
            Ok(code) => panic!("{NAME}: close must abort, got exit {code}"),
            Err(error) => assert_eq!(error.code, "aborted", "{NAME}: {}", error.message()),
        }
        connection.close();
    })
    .await;
}

/// `fails to open in a missing directory`
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fails_to_open_in_a_missing_directory() {
    const NAME: &str = "fails to open in a missing directory";
    within_20_seconds(NAME, async {
        let (connection, dir) = environment();
        let result = open_terminal(
            Arc::clone(&connection),
            TerminalOptions::new(dir.join("missing"), 80, 24, Arc::new(|_bytes: Vec<u8>| {})),
        )
        .await;
        match result {
            Ok(_) => panic!("{NAME}: opening in a missing directory must fail"),
            Err(_error) => {}
        }
        connection.close();
    })
    .await;
}

/// `rejects writes to a terminal that does not exist`
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_writes_to_a_terminal_that_does_not_exist() {
    const NAME: &str = "rejects writes to a terminal that does not exist";
    within_20_seconds(NAME, async {
        let (connection, _dir) = environment();
        let mut body = Json::new();
        body.insert("target".to_owned(), serde_json::Value::from(999));
        match connection
            .request("ptyWrite", body, RequestOptions::default())
            .await
        {
            Ok(_) => panic!("{NAME}: writing to an unknown terminal must fail"),
            Err(error) => assert_eq!(error.code, "EBADF", "{NAME}: {}", error.message()),
        }
        connection.close();
    })
    .await;
}

/// 回归（review finding）：`openTerminal` 只由 `started` 事件完成；pty 请求不带 started
/// 结算时，TS 的 promise 永不结算（terminal.ts:95-126），而不是报连接丢失。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_terminal_does_not_settle_without_a_started_event() {
    const NAME: &str = "a terminal without a started event never settles";
    within_20_seconds(NAME, async {
        let dir = common::TempDir::new("kiana-env-fake-terminal-");
        let log = dir.join("frames.log");
        let config = common::FakeDaemonConfig {
            log: Some(log),
            pty_no_started: true,
            pty_result: true,
            ..common::FakeDaemonConfig::default()
        };
        let config_file = common::write_fake_daemon_config(&dir, &config);
        let connection = Arc::new(Connection::new(common::fake_daemon_command(&config_file)));
        let opening = {
            let connection = Arc::clone(&connection);
            tokio::spawn(async move {
                open_terminal(
                    connection,
                    TerminalOptions::new("/work", 80, 24, Arc::new(|_bytes: Vec<u8>| {})),
                )
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !opening.is_finished(),
            "{NAME}: the open must stay pending without a started event"
        );
        opening.abort();
        let _ = opening.await;
        connection.close();
    })
    .await;
}
