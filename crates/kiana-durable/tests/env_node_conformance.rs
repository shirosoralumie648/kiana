//! `packages/durable/test/env-node-conformance.test.ts` 的移植。
//!
//! - `registerEnvConformance` 两次：默认（native）与 `{ mode: "polling", pollIntervalMs: 100 }`。
//! - `describe("NodeExecutionEnv watch limits")` 与 `describe("NodeExecutionEnv readers")`。
//!
//! Rust 没有动态测试注册，用例数组由 [`create_env_conformance`] 返回，这里逐个执行并把用例名
//! 放进失败消息（与 `register_*` 的 `it` 标题一致）。

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::FutureExt;

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::env::node::{NodeEnvOptions, NodeExecutionEnv};
use kiana_durable::env::node_watch::NodeWatchOptions;
use kiana_durable::env::{
    ExecutionEnv, FileContent, FileError, FileErrorCode, FileSystem, RemoveOptions, WatchChange,
    WatchTarget,
};
use kiana_durable::testing::assertions::PanicAssertions;
use kiana_durable::testing::create_env_conformance;
use kiana_durable::testing::types::{EnvConformanceCase, EnvConformanceOptions, EnvUse};

// ---------------------------------------------------------------------------
// 测试辅助
// ---------------------------------------------------------------------------

fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

fn ok_file<T>(result: Result<T, FileError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{}", error.message),
    }
}

struct TempDir {
    path: String,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "kiana-durable-env-conformance-{tag}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir {
            path: path.to_string_lossy().into_owned(),
        }
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn join(&self, name: &str) -> String {
        format!("{}/{}", self.path, name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn env_options(watch: Option<NodeWatchOptions>) -> EnvConformanceOptions {
    EnvConformanceOptions {
        assertions: Arc::new(PanicAssertions),
        with_env: Arc::new(move |use_env: EnvUse| {
            let watch = watch.clone();
            Box::pin(async move {
                let root = TempDir::new("case");
                let env = NodeExecutionEnv::new(NodeEnvOptions {
                    cwd: root.path().to_owned(),
                    shell_path: None,
                    shell_env: None,
                    watch,
                });
                let env: Arc<dyn ExecutionEnv> = Arc::new(env);
                use_env(env).await;
            })
        }),
        // TS 的 `{ shell, symlinks, listFiles }`；Linux 上默认 `sh -c`、`ln -s` 可用。
        shell: None,
        symlinks: Some(true),
        list_files: None,
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

async fn run_case(case: EnvConformanceCase) {
    let name = case.name.clone();
    let timeout_ms = case.timeout_ms;
    let run = (case.run)();
    let guarded = AssertUnwindSafe(run).catch_unwind();
    let outcome = match timeout_ms {
        Some(timeout_ms) => {
            match tokio::time::timeout(Duration::from_millis(timeout_ms), guarded).await {
                Ok(outcome) => outcome,
                Err(_) => panic!("env conformance case `{name}` timed out after {timeout_ms}ms"),
            }
        }
        None => guarded.await,
    };
    if let Err(payload) = outcome {
        panic!(
            "env conformance case `{name}` failed: {}",
            panic_message(payload)
        );
    }
}

async fn run_conformance(watch: Option<NodeWatchOptions>) {
    let cases = create_env_conformance(&env_options(watch));
    assert!(!cases.is_empty(), "conformance suite must expose cases");
    for case in cases {
        run_case(case).await;
    }
}

// ---------------------------------------------------------------------------
// env-node-conformance.test.ts
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn node_execution_env_conformance() {
    run_conformance(None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn node_execution_env_conformance_with_polling_watches() {
    run_conformance(Some(NodeWatchOptions {
        mode: Some(kiana_durable::env::WatchMode::Polling),
        poll_interval_ms: Some(100.0),
        max_directories: None,
    }))
    .await;
}

fn watch_target(path: &str) -> WatchTarget {
    WatchTarget {
        path: path.to_owned(),
        recursive: Some(true),
        exclude: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refuses_a_tree_over_the_directory_budget_and_stops_with_an_error_when_one_grows_past_it() {
    let root = TempDir::new("watch-limits");
    let env = NodeExecutionEnv::new(NodeEnvOptions {
        cwd: root.path().to_owned(),
        shell_path: None,
        shell_env: None,
        watch: Some(NodeWatchOptions {
            mode: None,
            poll_interval_ms: None,
            max_directories: Some(3),
        }),
    });
    ok_file(env.create_dir("tree/a/b", None, context()).await);
    ok_file(env.create_dir("tree/c", None, context()).await);
    let refused = env
        .watch(&[watch_target("tree")], Arc::new(|_change| {}), context())
        .await;
    let error = match refused {
        Ok(_) => panic!("a tree over the directory budget must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.code, FileErrorCode::Invalid);

    ok_file(
        env.remove(
            "tree/c",
            Some(&RemoveOptions {
                recursive: Some(true),
                force: None,
            }),
            context(),
        )
        .await,
    );
    let changes: Arc<Mutex<Vec<WatchChange>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&changes);
    let watcher = ok_file(
        env.watch(
            &[watch_target("tree")],
            Arc::new(move |change| sink.lock().expect("changes lock").push(change)),
            context(),
        )
        .await,
    );
    ok_file(env.create_dir("tree/d", None, context()).await);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !changes
        .lock()
        .expect("changes lock")
        .iter()
        .any(|change| matches!(change, WatchChange::Error(_)))
    {
        assert!(
            Instant::now() < deadline,
            "the watcher must report the directory budget error"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    {
        let changes = changes.lock().expect("changes lock");
        match changes.last() {
            Some(WatchChange::Error(error)) => assert_eq!(error.code, FileErrorCode::Invalid),
            other => panic!("expected a trailing error change, got {other:?}"),
        }
    }
    watcher.close(context()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_ranges_spanning_several_internal_chunks_exactly() {
    let root = TempDir::new("readers");
    let length = (2.5 * 1024.0 * 1024.0) as usize;
    let bytes: Vec<u8> = (0..length)
        .map(|index| ((index * 31) % 251) as u8)
        .collect();
    std::fs::write(root.join("big.bin"), &bytes).unwrap();
    let env = NodeExecutionEnv::new(root.path());
    let reader = ok_file(env.open_binary_reader("big.bin", None, context()).await);
    let all = ok_file(reader.read(0, bytes.len() + 10, context()).await);
    assert_eq!(all.len(), bytes.len());
    assert_eq!(all, bytes);
    let middle = ok_file(reader.read(1024 * 1024 - 3, 7, context()).await);
    assert_eq!(middle, bytes[1024 * 1024 - 3..1024 * 1024 + 4].to_vec());
    reader.close(context()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refuses_a_fifo_without_waiting_for_a_writer() {
    let root = TempDir::new("fifo");
    let status = std::process::Command::new("mkfifo")
        .arg(root.join("pipe"))
        .status()
        .expect("mkfifo");
    assert!(status.success(), "mkfifo must succeed");
    let env = NodeExecutionEnv::new(root.path());
    let result = env.open_binary_reader("pipe", None, context()).await;
    match result {
        Ok(_) => panic!("a FIFO must be refused"),
        Err(error) => assert_eq!(error.code, FileErrorCode::Invalid),
    }
}

// ---------------------------------------------------------------------------
// 回归：原生 watcher 必须静止，且只报告 Create/Modify/Remove
// ---------------------------------------------------------------------------

fn native_watch_env(root: &TempDir) -> NodeExecutionEnv {
    NodeExecutionEnv::new(NodeEnvOptions {
        cwd: root.path().to_owned(),
        shell_path: None,
        shell_env: None,
        watch: Some(NodeWatchOptions {
            mode: Some(kiana_durable::env::WatchMode::Native),
            // 即使环境把 watcher 降级为 polling，也用小间隔让用例及时结束。
            poll_interval_ms: Some(50.0),
            max_directories: None,
        }),
    })
}

fn paths_changes(changes: &Mutex<Vec<WatchChange>>) -> Vec<Vec<String>> {
    changes
        .lock()
        .expect("changes lock")
        .iter()
        .filter_map(|change| match change {
            WatchChange::Paths(paths) => Some(paths.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_native_watcher_reports_nothing_while_the_tree_is_idle_and_is_not_triggered_by_reads() {
    let root = TempDir::new("native-idle");
    let env = native_watch_env(&root);
    ok_file(env.create_dir("tree", None, context()).await);
    ok_file(
        env.write_file(
            "tree/file.txt",
            FileContent::Text("one".to_owned()),
            context(),
        )
        .await,
    );
    let changes: Arc<Mutex<Vec<WatchChange>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&changes);
    let watcher = ok_file(
        env.watch(
            &[watch_target("tree")],
            Arc::new(move |change| sink.lock().expect("changes lock").push(change)),
            context(),
        )
        .await,
    );

    // notify 的 inotify 后端订阅 IN_OPEN：扫描的 readdir 本身就会打开被监视目录。
    // 不过滤 Access 事件时，这里会每 50 ms 收到一次 Paths([tree])。
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        changes.lock().expect("changes lock").is_empty(),
        "an idle tree must not report changes: {:?}",
        changes.lock().expect("changes lock")
    );

    // 读取被监视的文件（open/close）同样是 Access 事件，不是变化。
    ok_file(env.read_text_file("tree/file.txt", context()).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        changes.lock().expect("changes lock").is_empty(),
        "reading a watched file must not report a change: {:?}",
        changes.lock().expect("changes lock")
    );

    // 真正的写入必须报告，并且去抖之后要再次静止。
    ok_file(
        env.write_file(
            "tree/file.txt",
            FileContent::Text("two".to_owned()),
            context(),
        )
        .await,
    );
    let file_path = root.join("tree/file.txt");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if paths_changes(&changes)
            .iter()
            .any(|paths| paths.iter().any(|path| path == &file_path))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a write must be reported; got {:?}",
            changes.lock().expect("changes lock")
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let reported = paths_changes(&changes).len();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        paths_changes(&changes).len(),
        reported,
        "the watcher must quiesce after the change"
    );

    watcher.close(context()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_the_failing_target_path_when_the_initial_scan_is_denied() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let root = TempDir::new("watch-denied");
        let locked = root.join("locked");
        std::fs::create_dir_all(&locked).expect("create locked dir");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
            .expect("chmod denied");
        let env = NodeExecutionEnv::new(root.path());
        let target = root.join("locked/missing");
        let result = env
            .watch(
                &[WatchTarget {
                    path: target.clone(),
                    recursive: None,
                    exclude: None,
                }],
                Arc::new(|_| {}),
                context(),
            )
            .await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
            .expect("chmod back");
        let error = match result {
            Ok(_) => panic!("a denied target must fail the watch"),
            Err(error) => error,
        };
        assert_eq!(error.code, FileErrorCode::PermissionDenied);
        // TS 重新抛出原始 Node 错误，`toFileError` 从 `nodeError.path` 取 target 路径。
        assert_eq!(error.path.as_deref(), Some(target.as_str()));
        assert_eq!(
            error.message,
            format!("EACCES: permission denied, stat '{target}'")
        );
    }
}
