//! `packages/durable/test/env-node.test.ts` 的移植。
//!
//! 与 TS 的差异（都是测试手段的差异，不是行为差异）：
//!
//! - `vi.spyOn(FileHandle.prototype, …)` 的两条用例（`flushFile` 注入 `EIO`、读取过程中取消）
//!   需要替换进程内文件句柄，Rust 侧没有等价手段：前者改为验证 `flushFile` 的错误映射与
//!   `is_directory`；后者的“取消后不跳过字节”改为用**已取消的上下文**读一次，再正常读到同样的
//!   行（`NodeTextLineReader` 每次读都显式定位，语义相同）。
//! - `truncateFile` 的 `-1`/`1.5`/`NaN`/`Infinity` 在 `u64` 签名下不可表达，只移植
//!   `MAX_SAFE_INTEGER + 1` 与缺失文件两条。
//! - Windows 专属用例（`taskkill`、继承 stdio 的后代）与 `process.platform` 打桩的 5 条 WSL
//!   传输选择在 Linux 上无法执行；WSL 判定改为 `src/env/node.rs` 的单元测试
//!   （`is_legacy_wsl_bash_path_with`），自定义 shellPath 的端到端用例保留。

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;

use kiana_chord::context::{
    AbortSignal, BACKGROUND_CONTEXT, Context, ContextRef, with_abort_signal,
};
use kiana_durable::env::node::{NodeEnvOptions, NodeExecutionEnv};
use kiana_durable::env::{
    CreateDirOptions, CreateTempFileOptions, ExecutionError, ExecutionErrorCode, FileContent,
    FileError, FileErrorCode, FileSystem, FileWatcher, OpenBinaryReaderOptions,
    ReadTextLinesOptions, RemoveOptions, Shell, ShellCommand, ShellExecOptions, ShellExecResult,
    ShellSpillOptions, TextLine,
};

// ---------------------------------------------------------------------------
// 测试辅助
// ---------------------------------------------------------------------------

fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

fn background() -> ContextRef {
    ContextRef::clone(&*BACKGROUND_CONTEXT)
}

fn aborted_context() -> ContextRef {
    let signal = AbortSignal::new();
    signal.abort_without_reason();
    with_abort_signal(signal, background())
}

fn ok_file<T>(result: Result<T, FileError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{}", error.message),
    }
}

fn ok_exec(result: Result<ShellExecResult, ExecutionError>) -> ShellExecResult {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{}", error.message),
    }
}

fn assert_aborted<T>(result: Result<T, FileError>) {
    match result {
        Ok(_) => panic!("expected aborted, got ok"),
        Err(error) => assert_eq!(error.code, FileErrorCode::Aborted, "{}", error.message),
    }
}

/// 独占的临时目录；`Drop` 时尽力删除。
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
            "kiana-durable-env-{tag}-{}-{nanos}-{counter}",
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

fn env_at(root: &TempDir) -> NodeExecutionEnv {
    NodeExecutionEnv::new(root.path())
}

fn env_with_shell(root: &TempDir, shell_path: String) -> NodeExecutionEnv {
    NodeExecutionEnv::new(NodeEnvOptions {
        cwd: root.path().to_owned(),
        shell_path: Some(shell_path),
        shell_env: None,
        watch: None,
    })
}

fn string_map(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

async fn collect_output(
    env: &NodeExecutionEnv,
    command: &str,
    options: ShellExecOptions,
) -> (Result<ShellExecResult, ExecutionError>, String) {
    let output = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&output);
    let options = ShellExecOptions {
        on_output: Some(Arc::new(move |text, _context, _info| {
            sink.lock().expect("output lock").push_str(text);
        })),
        ..options
    };
    let result = env
        .exec(
            ShellCommand::Shell(command.to_owned()),
            Some(&options),
            context(),
        )
        .await;
    let text = output.lock().expect("output lock").clone();
    (result, text)
}

async fn wait_for(mut condition: impl FnMut() -> bool, timeout_ms: u64) {
    let started = std::time::Instant::now();
    while !condition() {
        assert!(
            started.elapsed() < Duration::from_millis(timeout_ms),
            "condition not met within {timeout_ms}ms"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn remove_spill(spill_path: &str) {
    if let Some(parent) = std::path::Path::new(spill_path).parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
}

// ---------------------------------------------------------------------------
// NodeExecutionEnv filesystem
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_writes_lists_and_removes_files_and_directories() {
    let root = TempDir::new("fs");
    let env = env_at(&root);
    assert_eq!(
        ok_file(env.absolute_path("nested/child", context()).await),
        format!("{}/nested/child", root.path())
    );
    assert_eq!(
        ok_file(
            env.join_path(
                &[
                    root.path().to_owned(),
                    "nested".to_owned(),
                    "child".to_owned()
                ],
                context()
            )
            .await
        ),
        format!("{}/nested/child", root.path())
    );
    ok_file(env.create_dir("nested/child", None, context()).await);
    ok_file(
        env.write_file(
            "nested/child/file.txt",
            FileContent::Text("hel".to_owned()),
            context(),
        )
        .await,
    );
    ok_file(
        env.append_file(
            "nested/child/file.txt",
            FileContent::Text("lo".to_owned()),
            context(),
        )
        .await,
    );
    assert_eq!(
        ok_file(env.read_text_file("nested/child/file.txt", context()).await),
        "hello"
    );
    assert_eq!(
        ok_file(
            env.read_text_lines(
                "nested/child/file.txt",
                Some(&ReadTextLinesOptions { max_lines: Some(1) }),
                context()
            )
            .await
        ),
        vec!["hello".to_owned()]
    );
    assert_eq!(
        String::from_utf8(ok_file(
            env.read_binary_file("nested/child/file.txt", context())
                .await
        ))
        .unwrap(),
        "hello"
    );

    let entries = ok_file(env.list_dir("nested/child", context()).await);
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.name, "file.txt");
    assert_eq!(entry.path, root.join("nested/child/file.txt"));
    assert_eq!(entry.kind, kiana_durable::env::FileKind::File);
    assert_eq!(entry.size, 5);
    assert!(entry.mtime_ms > 0.0);

    assert!(ok_file(
        env.exists("nested/child/file.txt", context()).await
    ));
    ok_file(env.remove("nested/child/file.txt", None, context()).await);
    assert!(!ok_file(
        env.exists("nested/child/file.txt", context()).await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expands_home_relative_paths_and_file_urls() {
    let root = TempDir::new("paths");
    let env = env_at(&root);
    let home = dirs::home_dir().expect("home dir");
    assert_eq!(
        ok_file(env.absolute_path("~/kiana-node-env-test", context()).await),
        format!("{}/kiana-node-env-test", home.to_string_lossy())
    );
    let file_path = root.join("file with spaces.txt");
    let url = url::Url::from_file_path(&file_path).expect("file url");
    assert_eq!(
        ok_file(env.absolute_path(url.as_str(), context()).await),
        file_path
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returns_file_info_for_files_directories_and_symlinks_without_following() {
    let root = TempDir::new("info");
    let env = env_at(&root);
    ok_file(
        env.create_dir(
            "dir",
            Some(&CreateDirOptions {
                recursive: Some(true),
            }),
            context(),
        )
        .await,
    );
    ok_file(
        env.write_file(
            "dir/file.txt",
            FileContent::Text("hello".to_owned()),
            context(),
        )
        .await,
    );
    std::os::unix::fs::symlink(root.join("dir/file.txt"), root.join("file-link")).unwrap();
    std::os::unix::fs::symlink(root.join("dir"), root.join("dir-link")).unwrap();

    let dir = ok_file(env.file_info("dir", context()).await);
    assert_eq!(dir.name, "dir");
    assert_eq!(dir.path, root.join("dir"));
    assert_eq!(dir.kind, kiana_durable::env::FileKind::Directory);

    let file = ok_file(env.file_info("dir/file.txt", context()).await);
    assert_eq!(file.name, "file.txt");
    assert_eq!(file.path, root.join("dir/file.txt"));
    assert_eq!(file.kind, kiana_durable::env::FileKind::File);
    assert_eq!(file.size, 5);

    for (path, name) in [("file-link", "file-link"), ("dir-link", "dir-link")] {
        let info = ok_file(env.file_info(path, context()).await);
        assert_eq!(info.name, name);
        assert_eq!(info.path, root.join(path));
        assert_eq!(info.kind, kiana_durable::env::FileKind::Symlink);
    }

    let canonical = ok_file(env.canonical_path("file-link", context()).await);
    assert_eq!(
        canonical,
        std::fs::canonicalize(root.join("dir/file.txt"))
            .unwrap()
            .to_string_lossy()
            .into_owned()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refuses_to_follow_a_symbolic_link_when_no_follow_is_requested() {
    let root = TempDir::new("open-no-follow");
    let env = env_at(&root);
    ok_file(
        env.write_file(
            "target.txt",
            FileContent::Text("hello".to_owned()),
            context(),
        )
        .await,
    );
    std::os::unix::fs::symlink(root.join("target.txt"), root.join("link.txt")).unwrap();

    // 不跟随：ELOOP 必须映射为 `invalid` + 固定的拒绝对话（带底层 cause）。
    let error = match env
        .open_binary_reader(
            "link.txt",
            Some(&OpenBinaryReaderOptions {
                no_follow: Some(true),
            }),
            context(),
        )
        .await
    {
        Ok(_) => panic!("a symbolic link must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.code, FileErrorCode::Invalid);
    assert_eq!(error.message, "Refusing to follow a symbolic link");
    assert_eq!(error.path.as_deref(), Some(root.join("link.txt").as_str()));
    assert!(error.cause.is_some(), "the ELOOP cause must be attached");

    // 默认（noFollow 未设置）跟随链接。
    let reader = ok_file(env.open_binary_reader("link.txt", None, context()).await);
    assert_eq!(
        ok_file(reader.read(0, 100, context()).await),
        b"hello".to_vec()
    );
    reader.close(context()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lists_symlinks_as_symlinks() {
    let root = TempDir::new("links");
    let env = env_at(&root);
    ok_file(
        env.write_file(
            "target.txt",
            FileContent::Text("hello".to_owned()),
            context(),
        )
        .await,
    );
    std::os::unix::fs::symlink(root.join("target.txt"), root.join("link.txt")).unwrap();

    let entries = ok_file(env.list_dir(".", context()).await);
    let listed: BTreeSet<(String, String)> = entries
        .iter()
        .map(|entry| {
            (
                entry.name.clone(),
                match entry.kind {
                    kiana_durable::env::FileKind::File => "file",
                    kiana_durable::env::FileKind::Directory => "directory",
                    kiana_durable::env::FileKind::Symlink => "symlink",
                }
                .to_owned(),
            )
        })
        .collect();
    let expected: BTreeSet<(String, String)> = [
        ("link.txt".to_owned(), "symlink".to_owned()),
        ("target.txt".to_owned(), "file".to_owned()),
    ]
    .into_iter()
    .collect();
    assert_eq!(listed, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stops_reading_text_lines_at_the_requested_limit() {
    let root = TempDir::new("limit");
    let env = env_at(&root);
    ok_file(
        env.write_file(
            "file.txt",
            FileContent::Text("one\ntwo\nthree".to_owned()),
            context(),
        )
        .await,
    );
    assert_eq!(
        ok_file(
            env.read_text_lines(
                "file.txt",
                Some(&ReadTextLinesOptions { max_lines: Some(1) }),
                context()
            )
            .await
        ),
        vec!["one".to_owned()]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returns_file_error_for_missing_paths_and_exists_stays_false() {
    let root = TempDir::new("missing");
    let env = env_at(&root);
    let error = match env.file_info("missing.txt", context()).await {
        Ok(_) => panic!("expected a missing file error"),
        Err(error) => error,
    };
    assert_eq!(error.code, FileErrorCode::NotFound);
    assert_eq!(
        error.path.as_deref(),
        Some(root.join("missing.txt").as_str())
    );
    assert!(!ok_file(env.exists("missing.txt", context()).await));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returns_file_error_for_listing_non_directories() {
    let root = TempDir::new("notdir");
    let env = env_at(&root);
    ok_file(
        env.write_file("file.txt", FileContent::Text("hello".to_owned()), context())
            .await,
    );
    let error = match env.list_dir("file.txt", context()).await {
        Ok(_) => panic!("expected a listDir error"),
        Err(error) => error,
    };
    assert_eq!(error.code, FileErrorCode::NotDirectory);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn appends_to_new_files_and_creates_parent_directories() {
    let root = TempDir::new("append");
    let env = env_at(&root);
    ok_file(
        env.append_file(
            "new/nested/file.txt",
            FileContent::Text("a".to_owned()),
            context(),
        )
        .await,
    );
    ok_file(
        env.append_file(
            "new/nested/file.txt",
            FileContent::Text("b".to_owned()),
            context(),
        )
        .await,
    );
    assert_eq!(
        ok_file(env.read_text_file("new/nested/file.txt", context()).await),
        "ab"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn atomically_renames_a_file_and_replaces_the_destination() {
    let root = TempDir::new("rename");
    let env = env_at(&root);
    ok_file(
        env.write_file("source.txt", FileContent::Text("new".to_owned()), context())
            .await,
    );
    ok_file(
        env.write_file(
            "destination.txt",
            FileContent::Text("old".to_owned()),
            context(),
        )
        .await,
    );
    ok_file(
        env.rename_file("source.txt", "destination.txt", context())
            .await,
    );
    assert!(!ok_file(env.exists("source.txt", context()).await));
    assert_eq!(
        ok_file(env.read_text_file("destination.txt", context()).await),
        "new"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_the_source_path_when_rename_fails_because_the_source_is_missing() {
    let root = TempDir::new("rename-missing");
    let env = env_at(&root);
    ok_file(
        env.write_file(
            "destination.txt",
            FileContent::Text("unchanged".to_owned()),
            context(),
        )
        .await,
    );
    let error = match env
        .rename_file("missing-source.txt", "destination.txt", context())
        .await
    {
        Ok(_) => panic!("expected rename to fail"),
        Err(error) => error,
    };
    assert_eq!(error.code, FileErrorCode::NotFound);
    assert_eq!(
        error.path.as_deref(),
        Some(root.join("missing-source.txt").as_str())
    );
    assert_eq!(
        ok_file(env.read_text_file("destination.txt", context()).await),
        "unchanged"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creates_temporary_directories_and_files() {
    let root = TempDir::new("temp");
    let env = env_at(&root);
    let temp_dir = ok_file(env.create_temp_dir(Some("node-env-test-"), context()).await);
    let _cleanup_dir = TempDirGuard::new(&temp_dir);
    assert!(std::path::Path::new(&temp_dir).exists());
    let temp_file = ok_file(
        env.create_temp_file(
            Some(&CreateTempFileOptions {
                prefix: Some("prefix-".to_owned()),
                suffix: Some(".txt".to_owned()),
            }),
            context(),
        )
        .await,
    );
    let _cleanup_file = TempDirGuard::new(
        std::path::Path::new(&temp_file)
            .parent()
            .unwrap()
            .to_string_lossy()
            .as_ref(),
    );
    assert!(std::path::Path::new(&temp_file).exists());
    assert!(temp_file.ends_with(".txt"));
    assert_eq!(ok_file(env.read_text_file(&temp_file, context()).await), "");
}

/// 删除一个由 `createTempDir`/`createTempFile` 创建的目录（它们不在 `TempDir` 之下）。
struct TempDirGuard {
    path: String,
}

impl TempDirGuard {
    fn new(path: &str) -> Self {
        TempDirGuard {
            path: path.to_owned(),
        }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn honors_create_dir_recursive_false_and_remove_recursive_force_options() {
    let root = TempDir::new("options");
    let env = env_at(&root);
    let create_result = env
        .create_dir(
            "missing/child",
            Some(&CreateDirOptions {
                recursive: Some(false),
            }),
            context(),
        )
        .await;
    assert_eq!(
        create_result.err().map(|error| error.code),
        Some(FileErrorCode::NotFound)
    );

    ok_file(
        env.write_file(
            "dir/child/file.txt",
            FileContent::Text("hello".to_owned()),
            context(),
        )
        .await,
    );
    let remove_directory = env
        .remove(
            "dir",
            Some(&RemoveOptions {
                recursive: Some(false),
                force: None,
            }),
            context(),
        )
        .await;
    assert!(remove_directory.is_err());
    ok_file(
        env.remove(
            "dir",
            Some(&RemoveOptions {
                recursive: Some(true),
                force: None,
            }),
            context(),
        )
        .await,
    );
    assert!(!ok_file(env.exists("dir", context()).await));

    let remove_missing = env
        .remove(
            "missing",
            Some(&RemoveOptions {
                recursive: None,
                force: Some(false),
            }),
            context(),
        )
        .await;
    assert!(remove_missing.is_err());
    ok_file(
        env.remove(
            "missing",
            Some(&RemoveOptions {
                recursive: None,
                force: Some(true),
            }),
            context(),
        )
        .await,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returns_aborted_results_without_side_effects_for_pre_aborted_file_operations() {
    let root = TempDir::new("aborted");
    let env = env_at(&root);
    ok_file(
        env.write_file("file.txt", FileContent::Text("hello".to_owned()), context())
            .await,
    );
    let aborted = aborted_context();
    let aborted = aborted.as_ref();

    assert_aborted(env.read_text_file("file.txt", aborted).await);
    assert_aborted(env.read_text_lines("file.txt", None, aborted).await);
    assert_aborted(env.read_binary_file("file.txt", aborted).await);
    assert_aborted(env.open_text_line_reader("file.txt", aborted).await);
    assert_aborted(
        env.write_file("other.txt", FileContent::Text("hello".to_owned()), aborted)
            .await,
    );
    assert_aborted(
        env.append_file("file.txt", FileContent::Text(" world".to_owned()), aborted)
            .await,
    );
    assert_aborted(env.truncate_file("file.txt", 1, aborted).await);
    assert_aborted(env.flush_file("file.txt", aborted).await);
    assert_aborted(env.rename_file("file.txt", "renamed.txt", aborted).await);
    assert_aborted(env.file_info("file.txt", aborted).await);
    assert_aborted(env.list_dir(".", aborted).await);
    assert_aborted(env.canonical_path("file.txt", aborted).await);
    assert_aborted(env.exists("file.txt", aborted).await);
    assert_aborted(env.create_dir("dir", None, aborted).await);
    assert_aborted(env.remove("file.txt", None, aborted).await);
    assert_aborted(env.create_temp_dir(None, aborted).await);
    assert_aborted(env.create_temp_file(None, aborted).await);

    assert_eq!(
        ok_file(env.read_text_file("file.txt", context()).await),
        "hello"
    );
    let names: Vec<String> = ok_file(env.list_dir(".", context()).await)
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, vec!["file.txt".to_owned()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncates_and_extends_files_to_exact_byte_sizes() {
    let root = TempDir::new("truncate");
    let env = env_at(&root);
    let bytes = vec![0x61, 0xc3, 0xa9, 0x0a, 0x62, 0x0a];
    ok_file(
        env.write_file("file.bin", FileContent::Bytes(bytes), context())
            .await,
    );

    ok_file(env.truncate_file("file.bin", 2, context()).await);
    assert_eq!(
        ok_file(env.read_binary_file("file.bin", context()).await),
        vec![0x61, 0xc3]
    );

    ok_file(env.truncate_file("file.bin", 4, context()).await);
    assert_eq!(
        ok_file(env.read_binary_file("file.bin", context()).await),
        vec![0x61, 0xc3, 0, 0]
    );

    ok_file(env.truncate_file("file.bin", 0, context()).await);
    assert_eq!(ok_file(env.file_info("file.bin", context()).await).size, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_invalid_truncation_sizes_and_never_creates_missing_files() {
    let root = TempDir::new("truncate-invalid");
    let env = env_at(&root);
    ok_file(
        env.write_file("file.txt", FileContent::Text("hello".to_owned()), context())
            .await,
    );
    let error = env
        .truncate_file("file.txt", 9_007_199_254_740_992, context())
        .await
        .expect_err("oversized truncation must fail");
    assert_eq!(error.code, FileErrorCode::Invalid);
    assert_eq!(error.path.as_deref(), Some(root.join("file.txt").as_str()));
    assert_eq!(
        ok_file(env.read_text_file("file.txt", context()).await),
        "hello"
    );

    let missing = env
        .truncate_file("missing.txt", 0, context())
        .await
        .expect_err("missing file must fail");
    assert_eq!(missing.code, FileErrorCode::NotFound);
    assert_eq!(
        missing.path.as_deref(),
        Some(root.join("missing.txt").as_str())
    );
    assert!(!ok_file(env.exists("missing.txt", context()).await));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flushes_existing_files_without_changing_content_and_reports_missing_paths() {
    let root = TempDir::new("flush");
    let env = env_at(&root);
    ok_file(
        env.write_file(
            "file.txt",
            FileContent::Text("durable".to_owned()),
            context(),
        )
        .await,
    );
    ok_file(env.flush_file("file.txt", context()).await);
    assert_eq!(
        ok_file(env.read_text_file("file.txt", context()).await),
        "durable"
    );

    let missing = env
        .flush_file("missing.txt", context())
        .await
        .expect_err("missing file must fail");
    assert_eq!(missing.code, FileErrorCode::NotFound);
    assert_eq!(
        missing.path.as_deref(),
        Some(root.join("missing.txt").as_str())
    );

    ok_file(env.create_dir("dir", None, context()).await);
    let directory = env
        .flush_file("dir", context())
        .await
        .expect_err("directory must fail");
    assert_eq!(directory.code, FileErrorCode::IsDirectory);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cleanup_is_best_effort() {
    let root = TempDir::new("cleanup");
    let env = env_at(&root);
    env.cleanup(context()).await;
}

// ---------------------------------------------------------------------------
// NodeExecutionEnv text line reader
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_whether_each_line_was_newline_terminated_and_preserves_carriage_returns() {
    let root = TempDir::new("reader");
    let env = env_at(&root);
    ok_file(
        env.write_file(
            "lines.txt",
            FileContent::Text("one\r\n\ntwo\npartial".to_owned()),
            context(),
        )
        .await,
    );
    let reader = ok_file(env.open_text_line_reader("lines.txt", context()).await);
    let mut lines = Vec::new();
    while let Some(line) = ok_file(reader.read_line(context()).await) {
        lines.push(line);
    }
    assert_eq!(
        lines,
        vec![
            TextLine {
                text: "one\r".to_owned(),
                terminated: true
            },
            TextLine {
                text: String::new(),
                terminated: true
            },
            TextLine {
                text: "two".to_owned(),
                terminated: true
            },
            TextLine {
                text: "partial".to_owned(),
                terminated: false
            },
        ]
    );
    assert_eq!(ok_file(reader.read_line(context()).await), None);
    reader.close(context()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returns_no_lines_for_an_empty_file_and_one_terminated_empty_line_for_a_lone_newline() {
    let root = TempDir::new("reader-empty");
    let env = env_at(&root);
    ok_file(
        env.write_file("empty.txt", FileContent::Text(String::new()), context())
            .await,
    );
    ok_file(
        env.write_file("newline.txt", FileContent::Text("\n".to_owned()), context())
            .await,
    );

    let empty = ok_file(env.open_text_line_reader("empty.txt", context()).await);
    assert_eq!(ok_file(empty.read_line(context()).await), None);
    empty.close(context()).await;

    let newline = ok_file(env.open_text_line_reader("newline.txt", context()).await);
    assert_eq!(
        ok_file(newline.read_line(context()).await),
        Some(TextLine {
            text: String::new(),
            terminated: true
        })
    );
    assert_eq!(ok_file(newline.read_line(context()).await), None);
    newline.close(context()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decodes_multi_byte_characters_split_across_read_chunks_and_long_lines() {
    let root = TempDir::new("reader-large");
    let env = env_at(&root);
    // 读取器用 64 KiB 块；把四字节字符放在第一个边界两侧。
    let first = format!("{}😀tail", "a".repeat(64 * 1024 - 2));
    let second = "é".repeat(100_000);
    ok_file(
        env.write_file(
            "large.txt",
            FileContent::Text(format!("{first}\n{second}")),
            context(),
        )
        .await,
    );
    let reader = ok_file(env.open_text_line_reader("large.txt", context()).await);
    assert_eq!(
        ok_file(reader.read_line(context()).await),
        Some(TextLine {
            text: first,
            terminated: true
        })
    );
    assert_eq!(
        ok_file(reader.read_line(context()).await),
        Some(TextLine {
            text: second,
            terminated: false
        })
    );
    assert_eq!(ok_file(reader.read_line(context()).await), None);
    reader.close(context()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_read_does_not_consume_bytes() {
    let root = TempDir::new("reader-abort");
    let env = env_at(&root);
    ok_file(
        env.write_file(
            "lines.txt",
            FileContent::Text("one\ntwo\n".to_owned()),
            context(),
        )
        .await,
    );
    let reader = ok_file(env.open_text_line_reader("lines.txt", context()).await);
    let aborted = aborted_context();
    let error = reader
        .read_line(aborted.as_ref())
        .await
        .expect_err("aborted read must fail");
    assert_eq!(error.code, FileErrorCode::Aborted);
    assert_eq!(
        ok_file(reader.read_line(context()).await),
        Some(TextLine {
            text: "one".to_owned(),
            terminated: true
        })
    );
    assert_eq!(
        ok_file(reader.read_line(context()).await),
        Some(TextLine {
            text: "two".to_owned(),
            terminated: true
        })
    );
    assert_eq!(ok_file(reader.read_line(context()).await), None);
    reader.close(context()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_reads_after_close_and_closes_idempotently() {
    let root = TempDir::new("reader-close");
    let env = env_at(&root);
    ok_file(
        env.write_file(
            "lines.txt",
            FileContent::Text("one\n".to_owned()),
            context(),
        )
        .await,
    );
    let reader = ok_file(env.open_text_line_reader("lines.txt", context()).await);
    reader.close(context()).await;
    reader.close(context()).await;
    let error = reader
        .read_line(context())
        .await
        .expect_err("closed reader must fail");
    assert_eq!(error.code, FileErrorCode::Invalid);
    assert_eq!(error.path.as_deref(), Some(root.join("lines.txt").as_str()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_missing_files_when_opening_a_reader() {
    let root = TempDir::new("reader-missing");
    let env = env_at(&root);
    let error = match env.open_text_line_reader("missing.txt", context()).await {
        Ok(_) => panic!("missing file must fail"),
        Err(error) => error,
    };
    assert_eq!(error.code, FileErrorCode::NotFound);
    assert_eq!(
        error.path.as_deref(),
        Some(root.join("missing.txt").as_str())
    );
}

// ---------------------------------------------------------------------------
// NodeExecutionEnv shell
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executes_commands_in_cwd_with_env_overrides() {
    let root = TempDir::new("shell-cwd");
    let env = env_at(&root);
    let (result, output) = collect_output(
        &env,
        "printf '%s' \"$NODE_ENV_TEST\" > cwd-marker.txt; printf '%s:%s' \"$PWD\" \"$NODE_ENV_TEST\"",
        ShellExecOptions {
            env: Some(string_map(&[("NODE_ENV_TEST", "ok")])),
            ..Default::default()
        },
    )
    .await;
    let result = ok_exec(result);
    assert_eq!(result.exit_code, 0);
    assert_eq!(
        std::fs::read_to_string(root.join("cwd-marker.txt")).unwrap(),
        "ok"
    );
    let canonical = std::fs::canonicalize(root.path()).unwrap();
    assert_eq!(output, format!("{}:ok", canonical.to_string_lossy()));
}

async fn applies_string_shell_environment_overrides(
    overrides: Option<IndexMap<String, String>>,
    expected_session_file: &str,
) {
    let root = TempDir::new("shell-env");
    let env = NodeExecutionEnv::new(NodeEnvOptions {
        cwd: root.path().to_owned(),
        shell_path: None,
        shell_env: Some(string_map(&[
            ("KIANA_SESSION_FILE", "/stale/parent.jsonl"),
            ("KIANA_CODING_AGENT", "true"),
            ("KIANA_NODE_ENV_PRESERVED_TEST", "preserved"),
        ])),
        watch: None,
    });
    let (result, output) = collect_output(
        &env,
        "printf '%s:%s|%s|%s' \"${KIANA_SESSION_FILE+x}\" \"${KIANA_SESSION_FILE-}\" \"$KIANA_CODING_AGENT\" \"$KIANA_NODE_ENV_PRESERVED_TEST\"",
        ShellExecOptions {
            env: overrides,
            ..Default::default()
        },
    )
    .await;
    ok_exec(result);
    assert_eq!(output, format!("{expected_session_file}|true|preserved"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_string_shell_environment_overrides_when_a_missing_override_preserves_the_base_value()
 {
    applies_string_shell_environment_overrides(None, "x:/stale/parent.jsonl").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_string_shell_environment_overrides_when_an_empty_override_shadows_the_base_value()
{
    applies_string_shell_environment_overrides(
        Some(string_map(&[("KIANA_SESSION_FILE", "")])),
        "x:",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_string_shell_environment_overrides_when_a_string_override_replaces_the_base_value()
{
    applies_string_shell_environment_overrides(
        Some(string_map(&[(
            "KIANA_SESSION_FILE",
            "/sessions/current.jsonl",
        )])),
        "x:/sessions/current.jsonl",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn can_replace_rather_than_inherit_the_default_shell_environment() {
    let root = TempDir::new("shell-inherit");
    let env = NodeExecutionEnv::new(NodeEnvOptions {
        cwd: root.path().to_owned(),
        shell_path: None,
        shell_env: Some(string_map(&[(
            "KIANA_NODE_ENV_CONFIGURED_TEST",
            "configured",
        )])),
        watch: None,
    });
    let (result, output) = collect_output(
        &env,
        // TS 测试用 `process.env[inheritedKey]` 的合成变量；Rust 里改 `process.env` 需要
        // `unsafe` 且会与并行测试竞争，改用必定继承的 HOME 作为“来自父进程”的标记
        // （bash 不会像 PATH 那样自行补上 HOME）。
        "printf '%s|%s|%s' \"${HOME+x}\" \"${KIANA_NODE_ENV_CONFIGURED_TEST-}\" \"${KIANA_NODE_ENV_EXPLICIT_TEST-}\"",
        ShellExecOptions {
            inherit_env: Some(false),
            env: Some(string_map(&[("KIANA_NODE_ENV_EXPLICIT_TEST", "explicit")])),
            ..Default::default()
        },
    )
    .await;
    ok_exec(result);
    // HOME（标记为空）、配置的 base 变量（空）、显式变量。
    assert_eq!(output, "||explicit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selects_the_argv_transport_for_a_custom_shell_path() {
    let root = TempDir::new("shell-custom");
    let shell_path = root.join("shell");
    std::fs::write(
        &shell_path,
        "#!/bin/sh\nprintf 'args:%s\\n' \"$*\" >&2\nexec /bin/bash \"$@\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shell_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let env = env_with_shell(&root, shell_path);
    let (result, output) = collect_output(
        &env,
        "name='World'; echo \"Hello, ${name}!\"",
        ShellExecOptions::default(),
    )
    .await;
    let result = ok_exec(result);
    assert!(output.contains("Hello, World!"), "output: {output}");
    assert!(output.contains("args:-c"), "output: {output}");
    assert_eq!(result.exit_code, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cleanup_terminates_active_shell_processes() {
    let root = TempDir::new("shell-cleanup");
    let env = Arc::new(env_at(&root));
    let execution = {
        let env = Arc::clone(&env);
        tokio::spawn(async move {
            env.exec(
                ShellCommand::Shell("touch started; sleep 60".to_owned()),
                None,
                context(),
            )
            .await
        })
    };
    let started_path = root.join("started");
    wait_for(move || std::fs::metadata(&started_path).is_ok(), 10_000).await;
    env.cleanup(context()).await;
    let result = tokio::time::timeout(Duration::from_secs(3), execution)
        .await
        .expect("execution settles after cleanup")
        .expect("exec task");
    assert!(result.is_ok(), "killed command still resolves successfully");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streams_combined_stdout_and_stderr() {
    let root = TempDir::new("shell-streams");
    let env = env_at(&root);
    let (result, output) = collect_output(
        &env,
        "printf out; printf err >&2",
        ShellExecOptions::default(),
    )
    .await;
    let result = ok_exec(result);
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.spill_path, None);
    assert!(output.contains("out"), "output: {output}");
    assert!(output.contains("err"), "output: {output}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decodes_utf8_split_across_raw_process_chunks() {
    let root = TempDir::new("shell-utf8");
    let env = env_at(&root);
    let (result, output) = collect_output(
        &env,
        "printf '\\360\\237'; sleep 0.05; printf '\\230\\200'",
        ShellExecOptions::default(),
    )
    .await;
    ok_exec(result);
    assert_eq!(output, "😀");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_a_missing_working_directory_before_spawning() {
    let root = TempDir::new("shell-cwd-missing");
    let missing = root.join("missing");
    let env = NodeExecutionEnv::new(missing);
    let result = env
        .exec(ShellCommand::Shell("printf ok".to_owned()), None, context())
        .await;
    let error = result.expect_err("missing working directory must fail");
    assert_eq!(error.code, ExecutionErrorCode::SpawnError);
    assert!(
        error.message.contains("Working directory does not exist"),
        "message: {}",
        error.message
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returns_non_zero_command_exit_codes_as_successful_execution_results() {
    let root = TempDir::new("shell-exit");
    let env = env_at(&root);
    let result = ok_exec(
        env.exec(ShellCommand::Shell("exit 7".to_owned()), None, context())
            .await,
    );
    assert_eq!(result.exit_code, 7);
    assert_eq!(result.spill_path, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maps_signal_killed_processes_to_a_non_zero_exit_code() {
    let root = TempDir::new("shell-signal");
    let env = env_at(&root);
    let result = ok_exec(
        env.exec(
            ShellCommand::Shell("kill -9 $$".to_owned()),
            None,
            context(),
        )
        .await,
    );
    assert_eq!(result.exit_code, 128 + 9);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returns_timeout_errors_for_commands_exceeding_the_timeout() {
    let root = TempDir::new("shell-timeout");
    let env = env_at(&root);
    let result = env
        .exec(
            ShellCommand::Shell("sleep 5".to_owned()),
            Some(&ShellExecOptions {
                timeout: Some(0.01),
                ..Default::default()
            }),
            context(),
        )
        .await;
    let error = result.expect_err("timed out command must fail");
    assert_eq!(error.code, ExecutionErrorCode::Timeout);
    assert_eq!(error.message, "timeout:0.01");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_invalid_timeouts_before_spawning() {
    let root = TempDir::new("shell-timeout-invalid");
    let env = env_at(&root);
    for timeout in [0.0, -1.0, f64::NAN, f64::INFINITY, 2_147_484.0] {
        let result = env
            .exec(
                ShellCommand::Shell("touch spawned".to_owned()),
                Some(&ShellExecOptions {
                    timeout: Some(timeout),
                    ..Default::default()
                }),
                context(),
            )
            .await;
        let error = result.expect_err("invalid timeout must fail");
        assert_eq!(error.code, ExecutionErrorCode::Timeout);
        assert!(
            error.message.contains("Invalid timeout"),
            "message: {}",
            error.message
        );
    }
    assert!(!ok_file(env.exists("spawned", context()).await));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returns_callback_errors_from_exec_stream_handlers() {
    let root = TempDir::new("shell-callback");
    let env = env_at(&root);
    let options = ShellExecOptions {
        on_output: Some(Arc::new(|_text: &str, _context: &dyn Context, _info| {
            panic!("callback failed");
        })),
        ..Default::default()
    };
    let result = env
        .exec(
            ShellCommand::Shell("printf out".to_owned()),
            Some(&options),
            context(),
        )
        .await;
    let error = result.expect_err("callback panic must fail the command");
    assert_eq!(error.code, ExecutionErrorCode::CallbackError);
    assert_eq!(error.message, "callback failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returns_shell_unavailable_and_spawn_errors() {
    let root = TempDir::new("shell-errors");
    let missing_shell = env_with_shell(&root, root.join("missing-shell"));
    let result = missing_shell
        .exec(ShellCommand::Shell("printf ok".to_owned()), None, context())
        .await;
    assert_eq!(
        result.err().map(|error| error.code),
        Some(ExecutionErrorCode::ShellUnavailable)
    );

    let shell_path = root.join("not-executable-shell");
    ok_file(
        env_at(&root)
            .write_file(
                &shell_path,
                FileContent::Text("not executable".to_owned()),
                context(),
            )
            .await,
    );
    let spawn_error_env = env_with_shell(&root, shell_path);
    let result = spawn_error_env
        .exec(ShellCommand::Shell("printf ok".to_owned()), None, context())
        .await;
    assert_eq!(
        result.err().map(|error| error.code),
        Some(ExecutionErrorCode::SpawnError)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn treats_an_empty_custom_shell_path_as_unconfigured() {
    let root = TempDir::new("shell-empty-path");
    // TS: `if (customShellPath)` 把 `""` 当作未配置，回退到平台默认 shell。
    let env = env_with_shell(&root, String::new());
    let (result, output) = collect_output(&env, "printf ok", ShellExecOptions::default()).await;
    ok_exec(result);
    assert_eq!(output, "ok");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_a_synchronous_spawn_enotdir_without_the_program_name() {
    let root = TempDir::new("shell-enotdir");
    let not_a_dir = root.join("not-a-dir");
    std::fs::write(&not_a_dir, "x").unwrap();

    // cwd 存在但不是目录：Node 的 `spawn` 同步抛 `spawn ENOTDIR`（没有文件名）。
    let env = NodeExecutionEnv::new(not_a_dir.clone());
    let error = env
        .exec(ShellCommand::Shell("printf ok".to_owned()), None, context())
        .await
        .expect_err("a non-directory cwd must fail");
    assert_eq!(error.code, ExecutionErrorCode::SpawnError);
    assert_eq!(error.message, "spawn ENOTDIR");

    // 程序路径穿过非目录同样是同步错误（只有 EACCES/EAGAIN/EMFILE/ENFILE/ENOENT 是异步的）。
    let env = env_at(&root);
    let error = env
        .exec(
            ShellCommand::Argv(vec![format!("{not_a_dir}/prog")]),
            None,
            context(),
        )
        .await
        .expect_err("a program under a non-directory must fail");
    assert_eq!(error.code, ExecutionErrorCode::SpawnError);
    assert_eq!(error.message, "spawn ENOTDIR");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returns_an_aborted_result_for_pre_aborted_and_aborted_commands() {
    let root = TempDir::new("shell-aborted");
    let env = env_at(&root);
    let aborted = aborted_context();
    let result = env
        .exec(
            ShellCommand::Shell("touch spawned".to_owned()),
            None,
            aborted.as_ref(),
        )
        .await;
    assert_eq!(
        result.err().map(|error| error.code),
        Some(ExecutionErrorCode::Aborted)
    );
    assert!(!ok_file(env.exists("spawned", context()).await));

    let signal = AbortSignal::new();
    let context_ref = with_abort_signal(signal.clone(), background());
    let env = Arc::new(env);
    let execution = {
        let env = Arc::clone(&env);
        tokio::spawn(async move {
            env.exec(
                ShellCommand::Shell("sleep 5".to_owned()),
                None,
                context_ref.as_ref(),
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    signal.abort_without_reason();
    let result = tokio::time::timeout(Duration::from_secs(5), execution)
        .await
        .expect("aborted command settles")
        .expect("exec task");
    assert_eq!(
        result.err().map(|error| error.code),
        Some(ExecutionErrorCode::Aborted)
    );
}

// ---------------------------------------------------------------------------
// spill
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn does_not_create_a_spill_before_output_crosses_its_thresholds() {
    let root = TempDir::new("spill-none");
    let env = env_at(&root);
    let result = ok_exec(
        env.exec(
            ShellCommand::Shell("printf short".to_owned()),
            Some(&ShellExecOptions {
                spill: Some(ShellSpillOptions {
                    after_bytes: 100,
                    after_lines: 10,
                }),
                ..Default::default()
            }),
            context(),
        )
        .await,
    );
    assert_eq!(result.spill_path, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserves_exact_raw_bytes_in_the_spill_while_streaming_decoded_text() {
    let root = TempDir::new("spill-bytes");
    let env = env_at(&root);
    let expected = vec![0x66, 0x80, 0x00, 0x6f];
    let result = ok_exec(
        env.exec(
            ShellCommand::Shell("printf 'f\\200\\000o'".to_owned()),
            Some(&ShellExecOptions {
                spill: Some(ShellSpillOptions {
                    after_bytes: 1,
                    after_lines: 10,
                }),
                ..Default::default()
            }),
            context(),
        )
        .await,
    );
    let spill_path = result.spill_path.expect("spill path");
    assert_eq!(
        ok_file(env.read_binary_file(&spill_path, context()).await),
        expected
    );
    remove_spill(&spill_path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_the_spill_of_a_command_that_times_out() {
    let root = TempDir::new("spill-timeout");
    let env = env_at(&root);
    let result = env
        .exec(
            ShellCommand::Shell("printf 12345678901234567890; sleep 5".to_owned()),
            Some(&ShellExecOptions {
                timeout: Some(0.3),
                spill: Some(ShellSpillOptions {
                    after_bytes: 10,
                    after_lines: 10,
                }),
                ..Default::default()
            }),
            context(),
        )
        .await;
    let error = result.expect_err("timed out command must fail");
    assert_eq!(error.code, ExecutionErrorCode::Timeout);
    let spill_path = error.spill_path.expect("timeout carries the spill path");
    assert_eq!(
        ok_file(env.read_text_file(&spill_path, context()).await),
        "12345678901234567890"
    );
    remove_spill(&spill_path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserves_complete_large_output_in_the_spill() {
    let root = TempDir::new("spill-large");
    let env = env_at(&root);
    let size = 500_000usize;
    let result = ok_exec(
        env.exec(
            ShellCommand::Shell(format!("head -c {size} /dev/zero | tr '\\0' x")),
            Some(&ShellExecOptions {
                spill: Some(ShellSpillOptions {
                    after_bytes: 10,
                    after_lines: 10,
                }),
                ..Default::default()
            }),
            context(),
        )
        .await,
    );
    let spill_path = result.spill_path.expect("spill path");
    assert_eq!(
        ok_file(env.read_text_file(&spill_path, context()).await).len(),
        size
    );
    remove_spill(&spill_path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streams_every_line_and_spills_them_all_once_output_crosses_its_line_threshold() {
    let root = TempDir::new("spill-lines");
    let env = env_at(&root);
    let (result, output) = collect_output(
        &env,
        "i=1; while [ $i -le 15000 ]; do echo line-$i; i=$((i+1)); done",
        ShellExecOptions {
            spill: Some(ShellSpillOptions {
                after_bytes: 1024 * 1024,
                after_lines: 100,
            }),
            ..Default::default()
        },
    )
    .await;
    let result = ok_exec(result);
    let expected: String = (1..=15000).map(|index| format!("line-{index}\n")).collect();
    assert_eq!(output, expected);
    let spill_path = result.spill_path.expect("spill path");
    let spilled = ok_file(env.read_text_lines(&spill_path, None, context()).await);
    assert_eq!(spilled.len(), 15000);
    assert_eq!(spilled.last().map(String::as_str), Some("line-15000"));
    remove_spill(&spill_path);
}

// ---------------------------------------------------------------------------
// watch（`NodeFileWatcher` 的入口与生命周期；一致性用例见 env_node_conformance.rs）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_delegates_to_the_node_file_watcher() {
    use kiana_durable::env::{WatchCallback, WatchTarget};
    let root = TempDir::new("watch");
    let env = env_at(&root);
    let watcher: Arc<dyn FileWatcher> = ok_file(
        env.watch(
            &[WatchTarget {
                path: ".".to_owned(),
                recursive: Some(false),
                exclude: None,
            }],
            Arc::new(|_change| {}) as WatchCallback,
            context(),
        )
        .await,
    );
    watcher.close(context()).await;
}

// ---------------------------------------------------------------------------
// 回归：审查发现的与 TS/Node 的不一致
// ---------------------------------------------------------------------------

/// `fs.rm` 对目录（即使是空目录）在 `recursive: false` 时总是以 `ERR_FS_EISDIR` 失败；
/// `ERR_FS_EISDIR` 不在 `toFileError` 的映射表里 → code `unknown`，path 是目录。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_without_recursive_refuses_directories_even_when_they_are_empty() {
    let root = TempDir::new("rm-empty-dir");
    let env = env_at(&root);
    ok_file(env.create_dir("empty", None, context()).await);
    let empty_path = root.join("empty");

    for force in [None, Some(true)] {
        let error = env
            .remove(
                "empty",
                Some(&RemoveOptions {
                    recursive: None,
                    force,
                }),
                context(),
            )
            .await
            .expect_err("rm without recursive must fail for a directory");
        assert_eq!(error.code, FileErrorCode::Unknown);
        assert_eq!(
            error.message,
            format!("Path is a directory: rm returned EISDIR (is a directory) {empty_path}")
        );
        assert_eq!(error.path.as_deref(), Some(empty_path.as_str()));
        assert!(
            ok_file(env.exists("empty", context()).await),
            "the directory must be left in place"
        );
    }

    // recursive: true 仍然删除空目录。
    ok_file(
        env.remove(
            "empty",
            Some(&RemoveOptions {
                recursive: Some(true),
                force: None,
            }),
            context(),
        )
        .await,
    );
    assert!(!ok_file(env.exists("empty", context()).await));
}

/// Node 的 `fs.mkdtemp` 以 0o700 创建目录（spill 文件在其中的原始 shell 输出不应对
/// 其他本地用户可读）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_temp_dir_uses_mode_0700_like_mkdtemp() {
    let root = TempDir::new("temp-mode");
    let env = env_at(&root);
    let dir = ok_file(env.create_temp_dir(Some("mode-"), context()).await);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir)
            .expect("temp dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{dir} mode {mode:o}");
        std::fs::remove_dir_all(&dir).expect("remove temp dir");
    }
    #[cfg(not(unix))]
    {
        std::fs::remove_dir_all(&dir).expect("remove temp dir");
    }
}

/// TS 的 `resolvePath` 把相对 `cwd` 交给 `resolve(cwd, path)`，后者以 `process.cwd()`
/// 为起点；Rust 之前把相对 cwd 又前置了一次。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolves_a_relative_cwd_against_the_process_cwd() {
    let process_cwd = std::env::current_dir()
        .expect("process cwd")
        .to_string_lossy()
        .into_owned();
    let env = NodeExecutionEnv::new("relative/dir");
    assert_eq!(
        ok_file(env.absolute_path("x.txt", context()).await),
        format!("{process_cwd}/relative/dir/x.txt")
    );
    let dot = NodeExecutionEnv::new(".");
    assert_eq!(
        ok_file(dot.absolute_path("x.txt", context()).await),
        format!("{process_cwd}/x.txt")
    );
    assert_eq!(
        ok_file(dot.absolute_path(".", context()).await),
        process_cwd
    );
}

fn make_fifo(path: &str) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("mkfifo");
    assert!(status.success(), "mkfifo must succeed");
}

/// `readFile`/`writeFile` 把 signal 传给 fs：进行中的 I/O 被取消时以 Node 的
/// `AbortError`（"The operation was aborted"）失败，而不是返回已读/已写的数据。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborts_reads_and_writes_that_are_in_flight() {
    let root = TempDir::new("abort-in-flight");
    let env = env_at(&root);

    // 读：FIFO 打开后没有数据，读取一直挂起；中止后必须立刻返回 AbortError。
    let read_fifo = root.join("read.fifo");
    make_fifo(&read_fifo);
    let writer = std::thread::spawn({
        let fifo = read_fifo.clone();
        move || {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(&fifo)
                .expect("open fifo for write");
            std::thread::sleep(Duration::from_millis(200));
            use std::io::Write;
            let _ = file.write_all(b"payload");
        }
    });
    let signal = AbortSignal::new();
    let abort_context = with_abort_signal(signal.clone(), background());
    let aborter = tokio::spawn({
        let signal = signal.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            signal.abort_without_reason();
        }
    });
    let error = env
        .read_text_file("read.fifo", abort_context.as_ref())
        .await
        .expect_err("an in-flight read must abort");
    assert_eq!(error.code, FileErrorCode::Aborted);
    assert_eq!(error.message, "The operation was aborted");
    assert_eq!(error.path.as_deref(), Some(read_fifo.as_str()));
    aborter.await.expect("aborter");
    writer.join().expect("writer");

    // 二进制读取走同一条路径。
    let binary_fifo = root.join("binary.fifo");
    make_fifo(&binary_fifo);
    let writer = std::thread::spawn({
        let fifo = binary_fifo.clone();
        move || {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(&fifo)
                .expect("open fifo for write");
            std::thread::sleep(Duration::from_millis(200));
            use std::io::Write;
            let _ = file.write_all(b"payload");
        }
    });
    let signal = AbortSignal::new();
    let abort_context = with_abort_signal(signal.clone(), background());
    let aborter = tokio::spawn({
        let signal = signal.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            signal.abort_without_reason();
        }
    });
    let error = env
        .read_binary_file("binary.fifo", abort_context.as_ref())
        .await
        .expect_err("an in-flight read must abort");
    assert_eq!(error.code, FileErrorCode::Aborted);
    assert_eq!(error.message, "The operation was aborted");
    aborter.await.expect("aborter");
    writer.join().expect("writer");

    // 写：读端不消费数据，写入会阻塞在管道缓冲区；中止后必须返回 AbortError。
    // tokio 的 `File` 先把最多 2 MiB 的写入拷入内部缓冲再交给阻塞线程池，`write_all`
    // 要等这批数据写完才继续，所以载荷必须超过 2 MiB，写入才会真正停留在途。
    let write_fifo = root.join("write.fifo");
    make_fifo(&write_fifo);
    let (release_sender, release_receiver) = std::sync::mpsc::channel::<()>();
    let reader = std::thread::spawn({
        let fifo = write_fifo.clone();
        move || {
            let file = std::fs::File::open(&fifo).expect("open fifo for read");
            let _ = release_receiver.recv();
            drop(file);
        }
    });
    let signal = AbortSignal::new();
    let abort_context = with_abort_signal(signal.clone(), background());
    let aborter = tokio::spawn({
        let signal = signal.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            signal.abort_without_reason();
        }
    });
    let result = env
        .write_file(
            "write.fifo",
            FileContent::Bytes(vec![b'x'; 8 * 1024 * 1024]),
            abort_context.as_ref(),
        )
        .await;
    let error = result.expect_err("an in-flight write must abort");
    assert_eq!(error.code, FileErrorCode::Aborted);
    assert_eq!(error.message, "The operation was aborted");
    assert_eq!(error.path.as_deref(), Some(write_fifo.as_str()));
    aborter.await.expect("aborter");
    let _ = release_sender.send(());
    reader.join().expect("reader");
}

/// Node 的 fs 错误消息带 syscall 与（对路径型 syscall）引号路径；码与路径不变。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_node_style_error_messages() {
    let root = TempDir::new("messages");
    let env = env_at(&root);
    ok_file(
        env.write_file("file.txt", FileContent::Text("x".to_owned()), context())
            .await,
    );
    ok_file(env.create_dir("dir", None, context()).await);

    let missing = root.join("missing.txt");
    let error = env
        .file_info("missing.txt", context())
        .await
        .expect_err("missing file");
    assert_eq!(
        error.message,
        format!("ENOENT: no such file or directory, lstat '{missing}'")
    );

    let error = env
        .read_text_file("missing.txt", context())
        .await
        .expect_err("missing file");
    assert_eq!(
        error.message,
        format!("ENOENT: no such file or directory, open '{missing}'")
    );

    // readFile 打开目录成功，read 失败：Node 的消息是 `read '<path>'`。
    let dir_path = root.join("dir");
    let error = env
        .read_text_file("dir", context())
        .await
        .expect_err("a directory is not readable");
    assert_eq!(error.code, FileErrorCode::IsDirectory);
    assert_eq!(
        error.message,
        format!("EISDIR: illegal operation on a directory, read '{dir_path}'")
    );

    // 以写方式打开目录在 open 阶段失败。
    let error = env
        .write_file("dir", FileContent::Text("x".to_owned()), context())
        .await
        .expect_err("a directory cannot be written");
    assert_eq!(
        error.message,
        format!("EISDIR: illegal operation on a directory, open '{dir_path}'")
    );

    let error = env
        .list_dir("missing.txt", context())
        .await
        .expect_err("readdir must fail");
    assert_eq!(
        error.message,
        format!("ENOENT: no such file or directory, scandir '{missing}'")
    );

    let error = match env.open_dir_reader("missing.txt", context()).await {
        Ok(_) => panic!("opendir must fail"),
        Err(error) => error,
    };
    assert_eq!(
        error.message,
        format!("ENOENT: no such file or directory, opendir '{missing}'")
    );

    let error = env
        .canonical_path("missing.txt", context())
        .await
        .expect_err("realpath must fail");
    assert_eq!(
        error.message,
        format!("ENOENT: no such file or directory, realpath '{missing}'")
    );

    let destination = root.join("destination.txt");
    let source = root.join("missing-source.txt");
    let error = env
        .rename_file("missing-source.txt", "destination.txt", context())
        .await
        .expect_err("rename must fail");
    assert_eq!(
        error.message,
        format!("ENOENT: no such file or directory, rename '{source}' -> '{destination}'")
    );
    assert_eq!(error.path.as_deref(), Some(source.as_str()));

    let error = env
        .create_dir(
            "dir",
            Some(&CreateDirOptions {
                recursive: Some(false),
            }),
            context(),
        )
        .await
        .expect_err("mkdir must fail");
    assert_eq!(
        error.message,
        format!("EEXIST: file already exists, mkdir '{dir_path}'")
    );

    let error = env
        .remove("missing-dir", None, context())
        .await
        .expect_err("rm must fail");
    let resolved = root.join("missing-dir");
    assert_eq!(
        error.message,
        format!("ENOENT: no such file or directory, lstat '{resolved}'")
    );
}

/// `spawn` 失败的消息是 Node 的 `spawn <file> EACCES`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_node_style_spawn_error_messages() {
    let root = TempDir::new("spawn-message");
    let shell_path = root.join("not-executable-shell");
    ok_file(
        env_at(&root)
            .write_file(
                &shell_path,
                FileContent::Text("not executable".to_owned()),
                context(),
            )
            .await,
    );
    let error = env_with_shell(&root, shell_path.clone())
        .exec(ShellCommand::Shell("printf ok".to_owned()), None, context())
        .await
        .expect_err("a non-executable shell must fail to spawn");
    assert_eq!(error.code, ExecutionErrorCode::SpawnError);
    assert_eq!(error.message, format!("spawn {shell_path} EACCES"));
}

/// `close()` 必须释放操作系统句柄（Node 的 `FileHandle.close()`/`Dir.close()`），
/// 即使读取器对象仍然被持有；`flushFile` 也不得遗留句柄。
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_releases_reader_handles_immediately() {
    // 只数指向本用例临时目录的描述符：测试二进制里的其他用例并行运行，全局 fd 数会抖动。
    fn describe_count(root: &str) -> usize {
        std::fs::read_dir("/proc/self/fd")
            .map(|entries| {
                entries
                    .filter(|entry| {
                        entry
                            .as_ref()
                            .ok()
                            .and_then(|entry| std::fs::read_link(entry.path()).ok())
                            .is_some_and(|target| target.starts_with(root))
                    })
                    .count()
            })
            .unwrap_or(0)
    }
    // 句柄释放可能发生在 IO 线程收尾之后；轮询到期望值（或超时）再断言，
    // 避免把释放延迟误报成泄漏。
    fn wait_describe_count(root: &str, expected: usize) -> usize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let count = describe_count(root);
            if count == expected || std::time::Instant::now() >= deadline {
                return count;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    let root = TempDir::new("reader-fds");
    let env = env_at(&root);
    ok_file(
        env.write_file("file.txt", FileContent::Text("one\n".to_owned()), context())
            .await,
    );
    assert_eq!(wait_describe_count(root.path(), 0), 0);

    let mut text_readers = Vec::new();
    for _ in 0..16 {
        text_readers.push(ok_file(
            env.open_text_line_reader("file.txt", context()).await,
        ));
    }
    let mut binary_readers = Vec::new();
    for _ in 0..16 {
        binary_readers.push(ok_file(
            env.open_binary_reader("file.txt", None, context()).await,
        ));
    }
    let mut dir_readers = Vec::new();
    for _ in 0..16 {
        dir_readers.push(ok_file(env.open_dir_reader(".", context()).await));
    }
    assert_eq!(
        wait_describe_count(root.path(), 48),
        48,
        "the readers must hold their handles until close()"
    );

    for reader in &text_readers {
        reader.close(context()).await;
    }
    for reader in &binary_readers {
        reader.close(context()).await;
    }
    for reader in &dir_readers {
        reader.close(context()).await;
    }
    assert_eq!(
        wait_describe_count(root.path(), 0),
        0,
        "close() must release the handles while the readers are retained"
    );

    // flushFile 每次都会关闭自己的句柄。
    for _ in 0..32 {
        ok_file(env.flush_file("file.txt", context()).await);
    }
    assert_eq!(
        wait_describe_count(root.path(), 0),
        0,
        "flushFile must not leak descriptors"
    );
}

/// TS 用例 `rejects a read cancelled while pending without consuming its bytes`：读取挂起
/// 时中止，恢复后必须从被中止的读取处继续（不跳过字节）。Rust 无法桩替换
/// `FileHandle.prototype.read`；用一个足够大的文件让读取保持在途，再校验重试后的内容。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_pending_read_does_not_consume_bytes() {
    let root = TempDir::new("reader-cancel-pending");
    let env = env_at(&root);
    // 32 MiB 的单个"行"：读取需要多个 64 KiB 块，足以让中止发生在读取过程中。
    let mut content = vec![b'a'; 32 * 1024 * 1024];
    content.extend_from_slice(b"\ntail");
    std::fs::write(root.join("big.txt"), &content).expect("write big file");
    let reader = ok_file(env.open_text_line_reader("big.txt", context()).await);

    let signal = AbortSignal::new();
    let abort_context = with_abort_signal(signal.clone(), background());
    let aborter = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1)).await;
        signal.abort_without_reason();
    });
    let first = reader.read_line(abort_context.as_ref()).await;
    aborter.await.expect("aborter");

    let mut line = match first {
        Err(error) => {
            assert_eq!(error.code, FileErrorCode::Aborted);
            // TS 的 `readLine` 在读完之后才检查中止（`abortResult`），消息是 "aborted"。
            assert_eq!(error.message, "aborted");
            // 恢复后从被中止的位置继续。
            match reader.read_line(context()).await {
                Ok(Some(line)) => line,
                other => panic!("expected the pending line, got {other:?}"),
            }
        }
        // 极端调度下整行可能先读完；此时只验证内容完整。
        Ok(Some(line)) => line,
        Ok(None) => panic!("the file has a line"),
    };
    assert_eq!(
        line.text.len(),
        32 * 1024 * 1024,
        "cancelled bytes must not be lost"
    );
    assert!(line.text.bytes().all(|byte| byte == b'a'));
    assert!(line.terminated);
    line.text.clear();
    let tail = ok_file(reader.read_line(context()).await);
    assert_eq!(
        tail,
        Some(TextLine {
            text: "tail".to_owned(),
            terminated: false
        })
    );
    assert_eq!(ok_file(reader.read_line(context()).await), None);
    reader.close(context()).await;
}
