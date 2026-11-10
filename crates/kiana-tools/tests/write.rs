//! `write.ts` 的移植测试。
//!
//! 覆盖 `tools.test.ts` 的 write 用例、`file-mutation-queue.test.ts` 的
//! 「中止的写入仍锁住队列」语义，以及 `tool-system-prompt-contributions` 的定义一致性。

mod file_tools_test_support;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use file_tools_test_support::{TestContext, execute, execute_with_signal, text_output};
use kiana_ai::types::JsonValue;
use kiana_tools::write::{
    LocalWriteOperations, WriteOperations, WriteToolOptions, create_write_tool,
    create_write_tool_definition, write_tool_system_prompt_contribution,
};
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn writes_file_contents_and_creates_parent_directories() {
    let temp = TempDir::new().expect("temp");
    let definition = create_write_tool_definition(temp.path().to_string_lossy(), None);
    let ctx = TestContext::new(temp.path());

    let file = temp.path().join("write-test.txt");
    let result = execute(
        &definition,
        json!({ "path": file.to_string_lossy(), "content": "Test content" }),
        &ctx,
    )
    .await
    .expect("write");
    assert_eq!(
        text_output(&result),
        format!("Successfully wrote to {}", file.display())
    );
    assert_eq!(result.details, JsonValue::Null);
    assert_eq!(fs::read_to_string(&file).expect("read"), "Test content");

    let nested = temp.path().join("nested").join("dir").join("test.txt");
    let result = execute(
        &definition,
        json!({ "path": nested.to_string_lossy(), "content": "Nested content" }),
        &ctx,
    )
    .await
    .expect("write nested");
    assert!(text_output(&result).contains("Successfully wrote"));
    assert_eq!(fs::read_to_string(&nested).expect("read"), "Nested content");
}

#[tokio::test]
async fn success_text_uses_the_raw_path_argument() {
    let temp = TempDir::new().expect("temp");
    let definition = create_write_tool_definition(temp.path().to_string_lossy(), None);
    let ctx = TestContext::new(temp.path());
    let result = execute(
        &definition,
        json!({ "path": "relative.txt", "content": "x" }),
        &ctx,
    )
    .await
    .expect("write");
    assert_eq!(text_output(&result), "Successfully wrote to relative.txt");
}

#[tokio::test]
async fn write_uses_the_context_cwd() {
    let temp = TempDir::new().expect("temp");
    let definition = create_write_tool_definition("/", None);
    let ctx = TestContext::new(temp.path());
    execute(
        &definition,
        json!({ "path": "ctx-cwd-write.txt", "content": "written via ctx.cwd" }),
        &ctx,
    )
    .await
    .expect("write");
    assert_eq!(
        fs::read_to_string(temp.path().join("ctx-cwd-write.txt")).expect("read"),
        "written via ctx.cwd"
    );
}

#[tokio::test]
async fn pre_aborted_write_rejects_without_touching_the_file() {
    let temp = TempDir::new().expect("temp");
    let definition = create_write_tool_definition(temp.path().to_string_lossy(), None);
    let ctx = TestContext::new(temp.path());
    let signal = CancellationToken::new();
    signal.cancel();
    let error = execute_with_signal(
        &definition,
        json!({ "path": "aborted.txt", "content": "x" }),
        &signal,
        &ctx,
    )
    .await
    .expect_err("aborted");
    assert_eq!(error.message, "Operation aborted");
    assert!(!temp.path().join("aborted.txt").exists());
}

/// 记录调用顺序的操作实现。
struct RecordingOperations {
    calls: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl WriteOperations for RecordingOperations {
    async fn write_file(
        &self,
        absolute_path: &std::path::Path,
        content: &str,
    ) -> Result<(), kiana_tools::read::ToolOperationError> {
        self.calls
            .lock()
            .expect("lock")
            .push(format!("write:{}:{content}", absolute_path.display()));
        tokio::fs::write(absolute_path, content)
            .await
            .map_err(|error| {
                kiana_tools::read::ToolOperationError::from_io("open", absolute_path, error)
            })
    }

    async fn mkdir(
        &self,
        dir: &std::path::Path,
    ) -> Result<(), kiana_tools::read::ToolOperationError> {
        self.calls
            .lock()
            .expect("lock")
            .push(format!("mkdir:{}", dir.display()));
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|error| kiana_tools::read::ToolOperationError::from_io("mkdir", dir, error))
    }
}

#[tokio::test]
async fn operations_are_called_in_order() {
    let temp = TempDir::new().expect("temp");
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let definition = create_write_tool_definition(
        temp.path().to_string_lossy(),
        Some(WriteToolOptions {
            operations: Some(Arc::new(RecordingOperations {
                calls: Arc::clone(&calls),
            })),
            ..WriteToolOptions::default()
        }),
    );
    execute(
        &definition,
        json!({ "path": "a/b.txt", "content": "hi" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("write");
    let calls = calls.lock().expect("lock").clone();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].starts_with("mkdir:"));
    assert!(calls[1].starts_with("write:"));
    assert!(calls[1].ends_with(":hi"));
}

/// 第一次写入会等待 `release` 的操作实现（移植 `file-mutation-queue.test.ts`）。
struct GatedOperations {
    first_started: Arc<Notify>,
    release_first: Arc<Notify>,
    second_started: Arc<Notify>,
    first_settled: Arc<AtomicBool>,
}

#[async_trait]
impl WriteOperations for GatedOperations {
    async fn write_file(
        &self,
        absolute_path: &std::path::Path,
        content: &str,
    ) -> Result<(), kiana_tools::read::ToolOperationError> {
        if content == "first\n" {
            self.first_started.notify_one();
            self.release_first.notified().await;
            tokio::fs::write(absolute_path, content)
                .await
                .map_err(|error| {
                    kiana_tools::read::ToolOperationError::from_io("open", absolute_path, error)
                })?;
            self.first_settled.store(true, Ordering::SeqCst);
            return Ok(());
        }
        if content == "second\n" {
            assert!(
                self.first_settled.load(Ordering::SeqCst),
                "second write must wait for the aborted first write"
            );
            self.second_started.notify_one();
        }
        tokio::fs::write(absolute_path, content)
            .await
            .map_err(|error| {
                kiana_tools::read::ToolOperationError::from_io("open", absolute_path, error)
            })
    }

    async fn mkdir(
        &self,
        _dir: &std::path::Path,
    ) -> Result<(), kiana_tools::read::ToolOperationError> {
        Ok(())
    }
}

#[tokio::test]
async fn keeps_the_write_queue_locked_while_an_aborted_write_is_in_flight() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("abort-write.txt");
    let first_started = Arc::new(Notify::new());
    let release_first = Arc::new(Notify::new());
    let second_started = Arc::new(Notify::new());
    let first_settled = Arc::new(AtomicBool::new(false));
    let definition = create_write_tool_definition(
        temp.path().to_string_lossy(),
        Some(WriteToolOptions {
            operations: Some(Arc::new(GatedOperations {
                first_started: Arc::clone(&first_started),
                release_first: Arc::clone(&release_first),
                second_started: Arc::clone(&second_started),
                first_settled: Arc::clone(&first_settled),
            })),
            ..WriteToolOptions::default()
        }),
    );
    let ctx = TestContext::new(temp.path());
    let signal = CancellationToken::new();

    let first = tokio::spawn({
        let definition = definition.clone();
        let ctx = Arc::clone(&ctx);
        let signal = signal.clone();
        let file = file.clone();
        async move {
            execute_with_signal(
                &definition,
                json!({ "path": file.to_string_lossy(), "content": "first\n" }),
                &signal,
                &ctx,
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), first_started.notified())
        .await
        .expect("first write started");
    signal.cancel();

    let second = tokio::spawn({
        let definition = definition.clone();
        let ctx = Arc::clone(&ctx);
        let file = file.clone();
        async move {
            execute(
                &definition,
                json!({ "path": file.to_string_lossy(), "content": "second\n" }),
                &ctx,
            )
            .await
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), second_started.notified())
            .await
            .is_err(),
        "second write must stay blocked while the first is in flight"
    );

    release_first.notify_one();
    let first_error = first.await.expect("join").expect_err("aborted");
    assert_eq!(first_error.message, "Operation aborted");
    second.await.expect("join").expect("second write");
    assert_eq!(fs::read_to_string(&file).expect("read"), "second\n");
}

#[test]
fn definition_matches_the_prompt_contribution() {
    let definition = create_write_tool_definition("/workspace", None);
    let contribution = write_tool_system_prompt_contribution();
    assert_eq!(
        definition.prompt_snippet.as_deref(),
        Some(contribution.snippet)
    );
    assert_eq!(
        definition.prompt_guidelines,
        Some(
            contribution
                .guidelines
                .iter()
                .map(|guideline| (*guideline).to_string())
                .collect()
        )
    );
}

#[test]
fn parameters_schema_is_verbatim() {
    let definition = create_write_tool_definition("/", None);
    assert_eq!(
        definition.parameters,
        json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": { "type": "string", "description": "Path to the file to write (relative or absolute)" },
                "content": { "type": "string", "description": "Content to write to the file" }
            }
        })
    );
    assert_eq!(definition.output_schema, None);
    assert!(definition.constrained_sampling.is_some());
}

#[test]
fn agent_tool_projection() {
    let tool = create_write_tool("/", None);
    assert_eq!(tool.name, "write");
    let _ = LocalWriteOperations;
}

// ---------------------------------------------------------------------------
// realpath key errors（file-mutation-queue.ts#getMutationQueueKey 的 rethrow）
// ---------------------------------------------------------------------------

/// TS 的 `withFileMutationQueue` 在 `getMutationQueueKey` 抛错时 reject，`fn` 不执行。
/// `edit`/`write` 必须报告 Node 的原始 `realpath` 文案，且不能有 `mkdir` 副作用。
#[cfg(unix)]
#[tokio::test]
async fn realpath_errors_abort_before_any_side_effect() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("temp");
    let definition = create_write_tool_definition(temp.path().to_string_lossy(), None);
    let ctx = TestContext::new(temp.path());

    // 符号链接环：realpath 返回 ELOOP。
    let loop_path = temp.path().join("loop");
    symlink(&loop_path, &loop_path).expect("symlink loop");
    let error = execute(
        &definition,
        json!({ "path": loop_path.to_string_lossy(), "content": "x" }),
        &ctx,
    )
    .await
    .expect_err("ELOOP");
    assert_eq!(
        error.message,
        format!(
            "ELOOP: too many symbolic links encountered, realpath '{}'",
            loop_path.display()
        )
    );

    // 含 NUL 的路径：Node 在 realpath 之前用 ERR_INVALID_ARG_VALUE 拒绝，父目录不会被创建。
    let nul_parent = temp.path().join("nul-parent");
    let nul_path = format!("{}/sub\0/file.txt", nul_parent.display());
    let error = execute(
        &definition,
        json!({ "path": nul_path, "content": "x" }),
        &ctx,
    )
    .await
    .expect_err("NUL path");
    assert_eq!(
        error.message,
        format!(
            "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received '{}/sub\\x00/file.txt'",
            nul_parent.display()
        )
    );
    assert!(
        !nul_parent.exists(),
        "the parent directory must not be created"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn overlong_paths_report_the_realpath_error() {
    let temp = TempDir::new().expect("temp");
    let definition = create_write_tool_definition(temp.path().to_string_lossy(), None);
    let ctx = TestContext::new(temp.path());
    let long = temp.path().join("x".repeat(300));
    let error = execute(
        &definition,
        json!({ "path": long.to_string_lossy(), "content": "x" }),
        &ctx,
    )
    .await
    .expect_err("ENAMETOOLONG");
    assert_eq!(
        error.message,
        format!("ENAMETOOLONG: name too long, realpath '{}'", long.display())
    );
    assert!(!long.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn unsearchable_parent_reports_the_realpath_error() {
    use std::os::unix::fs::PermissionsExt;

    // root 无视权限位，无法构造 EACCES。
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let temp = TempDir::new().expect("temp");
    let locked = temp.path().join("locked");
    fs::create_dir(&locked).expect("mkdir");
    let file = locked.join("f.txt");
    fs::write(&file, "hello").expect("write");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");

    let definition = create_write_tool_definition(temp.path().to_string_lossy(), None);
    let outcome = execute(
        &definition,
        json!({ "path": file.to_string_lossy(), "content": "x" }),
        &TestContext::new(temp.path()),
    )
    .await;
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("restore chmod");

    let error = outcome.expect_err("EACCES");
    assert_eq!(
        error.message,
        format!("EACCES: permission denied, realpath '{}'", file.display())
    );
    assert_eq!(fs::read_to_string(&file).expect("read"), "hello");
}
