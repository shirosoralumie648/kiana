//! 第 2 周：read-before-write 门禁的集成测试。
//!
//! 覆盖计划里的行为契约：
//! - 没读过文件就 edit/write 返回 `ReadBeforeEdit`；
//! - 读后外部修改返回 `FileChangedSinceRead` / `FILE_CHANGED_SINCE_READ`；
//! - 截断读只能改看到过的行；
//! - 两个并行写同一文件被串行化；
//! - 成功写入后旧快照失效（按写出的字节重建基线）。

mod file_tools_test_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use file_tools_test_support::{TestContext, execute};
use kiana_ext_api::tool_types::ToolDefinition;
use kiana_sandbox::{ExecutionPolicy, SandboxPolicy};
use kiana_tools::apply_patch::{ApplyPatchToolOptions, create_apply_patch_tool_definition};
use kiana_tools::edit::{EditToolOptions, create_edit_tool_definition};
use kiana_tools::read::{
    PriorReadStatus, ReadStateRegistry, ReadToolOptions, create_read_tool_definition,
    fingerprint_bytes,
};
use kiana_tools::write::{WriteToolOptions, create_write_tool_definition};
use serde_json::json;
use tempfile::TempDir;

/// 1x1 红色 PNG（最小合法 PNG）：read 的图片分支会把它记为非文本读取。
const TINY_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==";

/// 一套隔离的测试环境：独立临时目录、独立读记录表、DangerFullAccess 策略
/// （读后写门禁与路径策略无关，这里只测门禁本身）。
struct Harness {
    _temp: TempDir,
    root: PathBuf,
    registry: Arc<ReadStateRegistry>,
    policy: Arc<ExecutionPolicy>,
}

impl Harness {
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let root = temp.path().to_path_buf();
        let registry = Arc::new(ReadStateRegistry::new());
        let policy = Arc::new(ExecutionPolicy::new(
            SandboxPolicy::danger_full_access(),
            root.clone(),
        ));
        Self {
            _temp: temp,
            root,
            registry,
            policy,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn read_definition(&self) -> ToolDefinition {
        create_read_tool_definition(
            self.root.to_string_lossy(),
            Some(ReadToolOptions {
                read_state: Some(Arc::clone(&self.registry)),
                ..ReadToolOptions::default()
            }),
        )
    }

    fn write_definition(&self) -> ToolDefinition {
        create_write_tool_definition(
            self.root.to_string_lossy(),
            Some(WriteToolOptions {
                execution_policy: Some(Arc::clone(&self.policy)),
                read_state: Some(Arc::clone(&self.registry)),
                ..WriteToolOptions::default()
            }),
        )
    }

    fn edit_definition(&self) -> ToolDefinition {
        create_edit_tool_definition(
            self.root.to_string_lossy(),
            Some(EditToolOptions {
                execution_policy: Some(Arc::clone(&self.policy)),
                read_state: Some(Arc::clone(&self.registry)),
                ..EditToolOptions::default()
            }),
        )
    }

    fn apply_patch_definition(&self) -> ToolDefinition {
        create_apply_patch_tool_definition(
            self.root.to_string_lossy(),
            Some(ApplyPatchToolOptions {
                execution_policy: Some(Arc::clone(&self.policy)),
                read_state: Some(Arc::clone(&self.registry)),
                ..ApplyPatchToolOptions::default()
            }),
        )
    }

    fn ctx(&self) -> Arc<dyn kiana_ext_api::types::ExtensionToolContextApi> {
        TestContext::new(&self.root)
    }

    fn write_file(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.file(name);
        fs::write(&path, contents).expect("fixture");
        path
    }
}

fn write_params(path: &Path, content: &str) -> serde_json::Value {
    json!({ "path": path.to_string_lossy(), "content": content })
}

fn edit_params(path: &Path, old: &str, new: &str) -> serde_json::Value {
    json!({
        "path": path.to_string_lossy(),
        "edits": [{ "oldText": old, "newText": new }],
    })
}

async fn read(harness: &Harness, _path: &Path, params: serde_json::Value) -> String {
    let result = execute(&harness.read_definition(), params, &harness.ctx())
        .await
        .expect("read");
    file_tools_test_support::text_output(&result)
}

#[tokio::test]
async fn write_to_an_existing_file_requires_a_prior_read() {
    let harness = Harness::new();
    let path = harness.write_file("notes.txt", "original\n");

    let error = execute(
        &harness.write_definition(),
        write_params(&path, "new\n"),
        &harness.ctx(),
    )
    .await
    .expect_err("unread file must be rejected");
    assert!(
        error.message.contains("ReadBeforeEdit"),
        "{}",
        error.message
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), "original\n");
}

#[tokio::test]
async fn edit_requires_a_prior_read() {
    let harness = Harness::new();
    let path = harness.write_file("notes.txt", "original\n");

    let error = execute(
        &harness.edit_definition(),
        edit_params(&path, "original", "changed"),
        &harness.ctx(),
    )
    .await
    .expect_err("unread file must be rejected");
    assert!(
        error.message.contains("ReadBeforeEdit"),
        "{}",
        error.message
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), "original\n");
}

#[tokio::test]
async fn reading_records_device_inode_fingerprint_and_ranges() {
    let harness = Harness::new();
    let path = harness.write_file("notes.txt", "one\ntwo\nthree\n");

    read(&harness, &path, json!({ "path": path.to_string_lossy() })).await;

    match harness.registry.check_prior_read(&path) {
        PriorReadStatus::Fresh(record) => {
            assert_ne!(record.ino, 0, "Linux should expose inode identity");
            assert_eq!(record.size, fs::metadata(&path).expect("stat").len());
            assert_eq!(
                record.content_hash,
                fingerprint_bytes(fs::read(&path).expect("read").as_slice())
            );
            assert_eq!(record.ranges.len(), 1);
            assert_eq!((record.ranges[0].start, record.ranges[0].end), (1, 4));
            assert!(record.text);
        }
        other => panic!("expected a fresh read record, got {other:?}"),
    }
}

#[tokio::test]
async fn reading_with_the_read_tool_unlocks_edit_and_write() {
    let harness = Harness::new();
    let path = harness.write_file("notes.txt", "one\ntwo\n");

    read(&harness, &path, json!({ "path": path.to_string_lossy() })).await;

    execute(
        &harness.edit_definition(),
        edit_params(&path, "two", "TWO"),
        &harness.ctx(),
    )
    .await
    .expect("edit after read");
    assert_eq!(fs::read_to_string(&path).expect("read"), "one\nTWO\n");

    execute(
        &harness.write_definition(),
        write_params(&path, "rewritten\n"),
        &harness.ctx(),
    )
    .await
    .expect("write after read");
    assert_eq!(fs::read_to_string(&path).expect("read"), "rewritten\n");
}

#[tokio::test]
async fn external_modification_after_read_returns_file_changed_since_read() {
    let harness = Harness::new();
    let path = harness.write_file("notes.txt", "original\n");
    read(&harness, &path, json!({ "path": path.to_string_lossy() })).await;

    // 外部进程改写：mtime/size/内容哈希都会变。
    fs::write(&path, "changed elsewhere\n").expect("external write");

    let error = execute(
        &harness.edit_definition(),
        edit_params(&path, "changed", "mine"),
        &harness.ctx(),
    )
    .await
    .expect_err("stale read must be rejected");
    assert!(
        error.message.contains("FileChangedSinceRead"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("FILE_CHANGED_SINCE_READ"),
        "{}",
        error.message
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        "changed elsewhere\n"
    );
}

#[tokio::test]
async fn external_modification_with_same_size_and_mtime_is_caught_by_the_content_hash() {
    let harness = Harness::new();
    let path = harness.write_file("notes.txt", "aaaa\n");
    read(&harness, &path, json!({ "path": path.to_string_lossy() })).await;
    let original_mtime = fs::metadata(&path)
        .expect("stat")
        .modified()
        .expect("mtime");

    // 同长度替换；把 mtime 恢复成读记录时的值，只有内容哈希能发现。
    fs::write(&path, "bbbb\n").expect("external write");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open");
    file.set_modified(original_mtime).expect("restore mtime");

    let error = execute(
        &harness.edit_definition(),
        edit_params(&path, "bbbb", "CCCC"),
        &harness.ctx(),
    )
    .await
    .expect_err("hash mismatch must be rejected");
    assert!(
        error.message.contains("FileChangedSinceRead"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn truncated_read_only_allows_editing_the_seen_lines() {
    let harness = Harness::new();
    let path = harness.write_file("big.txt", "line1\nline2\nline3\nline4\nline5\nline6\n");

    // 只读前两行。
    read(
        &harness,
        &path,
        json!({ "path": path.to_string_lossy(), "limit": 2 }),
    )
    .await;

    // 未看到的第 5 行不能被修改。
    let error = execute(
        &harness.edit_definition(),
        edit_params(&path, "line5", "LINE5"),
        &harness.ctx(),
    )
    .await
    .expect_err("unseen lines must stay read-only");
    assert!(
        error.message.contains("READ_RANGE_NOT_SEEN"),
        "{}",
        error.message
    );
    assert!(fs::read_to_string(&path).expect("read").contains("line5"));

    // 看到过的第 1 行可以修改。
    execute(
        &harness.edit_definition(),
        edit_params(&path, "line1", "LINE1"),
        &harness.ctx(),
    )
    .await
    .expect("seen line is editable");
    assert!(
        fs::read_to_string(&path)
            .expect("read")
            .starts_with("LINE1\n")
    );
}

#[tokio::test]
async fn truncated_read_cannot_overwrite_the_whole_file() {
    let harness = Harness::new();
    let path = harness.write_file("big.txt", "a\nb\nc\n");
    read(
        &harness,
        &path,
        json!({ "path": path.to_string_lossy(), "limit": 1 }),
    )
    .await;

    let error = execute(
        &harness.write_definition(),
        write_params(&path, "x\n"),
        &harness.ctx(),
    )
    .await
    .expect_err("partial read must not unlock a full overwrite");
    assert!(
        error.message.contains("ReadBeforeEdit"),
        "{}",
        error.message
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), "a\nb\nc\n");
}

#[tokio::test]
async fn parallel_edits_on_the_same_file_are_serialized_and_both_apply() {
    let harness = Harness::new();
    let path = harness.write_file("parallel.txt", "alpha\nbeta\ngamma\n");
    read(&harness, &path, json!({ "path": path.to_string_lossy() })).await;

    let definition = harness.edit_definition();
    let ctx = harness.ctx();
    let (first, second) = tokio::join!(
        execute(&definition, edit_params(&path, "alpha", "ALPHA"), &ctx,),
        execute(&definition, edit_params(&path, "beta", "BETA"), &ctx)
    );
    first.expect("first edit");
    second.expect("second edit");
    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        "ALPHA\nBETA\ngamma\n"
    );
}

#[tokio::test]
async fn parallel_writes_on_the_same_file_are_serialized() {
    let harness = Harness::new();
    let path = harness.write_file("parallel-write.txt", "seed\n");
    read(&harness, &path, json!({ "path": path.to_string_lossy() })).await;

    let definition = harness.write_definition();
    let ctx = harness.ctx();
    let (first, second) = tokio::join!(
        execute(&definition, write_params(&path, "first\n"), &ctx),
        execute(&definition, write_params(&path, "second\n"), &ctx)
    );
    first.expect("first write");
    second.expect("second write");
    let final_content = fs::read_to_string(&path).expect("read");
    assert!(
        final_content == "first\n" || final_content == "second\n",
        "one complete write must win, not a torn mixture: {final_content:?}"
    );
}

#[tokio::test]
async fn invalidating_the_record_forces_a_re_read() {
    let harness = Harness::new();
    let path = harness.write_file("notes.txt", "hello\n");
    read(&harness, &path, json!({ "path": path.to_string_lossy() })).await;
    harness.registry.invalidate(&path);

    let error = execute(
        &harness.edit_definition(),
        edit_params(&path, "hello", "bye"),
        &harness.ctx(),
    )
    .await
    .expect_err("invalidated record must not authorize a write");
    assert!(
        error.message.contains("ReadBeforeEdit"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn writing_a_new_file_needs_no_prior_read() {
    let harness = Harness::new();
    let path = harness.file("brand-new.txt");
    execute(
        &harness.write_definition(),
        write_params(&path, "seed\n"),
        &harness.ctx(),
    )
    .await
    .expect("new file");
    assert_eq!(fs::read_to_string(&path).expect("read"), "seed\n");
}

#[tokio::test]
async fn edit_and_write_agree_on_read_before_write() {
    let harness = Harness::new();
    let path = harness.write_file("shared.txt", "one\ntwo\n");

    let write_error = execute(
        &harness.write_definition(),
        write_params(&path, "x"),
        &harness.ctx(),
    )
    .await
    .expect_err("write needs a read");
    let edit_error = execute(
        &harness.edit_definition(),
        edit_params(&path, "one", "1"),
        &harness.ctx(),
    )
    .await
    .expect_err("edit needs a read");
    assert!(write_error.message.contains("ReadBeforeEdit"));
    assert!(edit_error.message.contains("ReadBeforeEdit"));
}

#[tokio::test]
async fn re_reading_after_an_external_change_unlocks_the_write() {
    let harness = Harness::new();
    let path = harness.write_file("notes.txt", "one\n");
    read(&harness, &path, json!({ "path": path.to_string_lossy() })).await;

    fs::write(&path, "two\n").expect("external write");
    let error = execute(
        &harness.edit_definition(),
        edit_params(&path, "two", "TWO"),
        &harness.ctx(),
    )
    .await
    .expect_err("stale read must be rejected");
    assert!(
        error.message.contains("FileChangedSinceRead"),
        "{}",
        error.message
    );

    // 错误文案要求重新 read；重新读取后同一个 edit 应该成功。
    read(&harness, &path, json!({ "path": path.to_string_lossy() })).await;
    execute(
        &harness.edit_definition(),
        edit_params(&path, "two", "TWO"),
        &harness.ctx(),
    )
    .await
    .expect("edit after re-read");
    assert_eq!(fs::read_to_string(&path).expect("read"), "TWO\n");
}

#[tokio::test]
async fn a_non_text_read_does_not_unlock_edit_or_write() {
    let harness = Harness::new();
    let path = harness.file("picture.png");
    let png = kiana_tui::terminal_image::base64_decode(TINY_PNG_BASE64);
    fs::write(&path, &png).expect("fixture");

    // read 的图片分支：内容被当作附件，记录为 text=false。
    read(&harness, &path, json!({ "path": path.to_string_lossy() })).await;

    let edit_error = execute(
        &harness.edit_definition(),
        edit_params(&path, "not-there", "x"),
        &harness.ctx(),
    )
    .await
    .expect_err("an image read must not unlock edit");
    assert!(
        edit_error.message.contains("READ_NOT_TEXT"),
        "{}",
        edit_error.message
    );

    let write_error = execute(
        &harness.write_definition(),
        write_params(&path, "text"),
        &harness.ctx(),
    )
    .await
    .expect_err("an image read must not unlock write");
    assert!(
        write_error.message.contains("READ_NOT_TEXT"),
        "{}",
        write_error.message
    );

    assert_eq!(
        fs::read(&path).expect("read"),
        png,
        "image must be untouched"
    );
}

/// apply_patch 不要求先 read，因此修改已有文件只能按“部分覆盖”记录；后续 edit 落到
/// 补丁未出现的行时，仍必须被 read-before-write 拒绝。
#[tokio::test]
async fn apply_patch_does_not_mark_unseen_lines_as_read() {
    let harness = Harness::new();
    let path = harness.write_file("notes.txt", "alpha\nbeta\ngamma\n");
    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-alpha\n+ALPHA\n*** End Patch\n",
        path.display()
    );
    execute(
        &harness.apply_patch_definition(),
        json!({ "patch": patch }),
        &harness.ctx(),
    )
    .await
    .expect("apply_patch works without a prior read");

    let error = execute(
        &harness.edit_definition(),
        edit_params(&path, "beta", "BETA"),
        &harness.ctx(),
    )
    .await
    .expect_err("editing a region the patch never showed must be rejected");
    assert_eq!(error.name.as_deref(), Some("ReadRangeNotSeen"), "{error:?}");
}

/// 补丁里真正新增的行属于模型刚写过的内容，apply_patch 后可以直接 edit；
/// 同一文件里补丁没覆盖的行仍被门禁拦住（见上一个测试）。
#[tokio::test]
async fn apply_patch_marks_patched_lines_as_read() {
    let harness = Harness::new();
    let path = harness.write_file("notes.txt", "alpha\nbeta\ngamma\n");
    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-alpha\n+ALPHA\n*** End Patch\n",
        path.display()
    );
    execute(
        &harness.apply_patch_definition(),
        json!({ "patch": patch }),
        &harness.ctx(),
    )
    .await
    .expect("apply_patch works without a prior read");

    execute(
        &harness.edit_definition(),
        edit_params(&path, "ALPHA", "ALPHA2"),
        &harness.ctx(),
    )
    .await
    .expect("the patched line was written by the model itself");
}

/// 新建文件由模型提供全文，apply_patch 后可以直接 edit。
#[tokio::test]
async fn apply_patch_add_file_can_be_edited_immediately() {
    let harness = Harness::new();
    let path = harness.file("created.txt");
    let patch = format!(
        "*** Begin Patch\n*** Add File: {}\n+first\n+second\n*** End Patch\n",
        path.display()
    );
    execute(
        &harness.apply_patch_definition(),
        json!({ "patch": patch }),
        &harness.ctx(),
    )
    .await
    .expect("add file");

    execute(
        &harness.edit_definition(),
        edit_params(&path, "second", "SECOND"),
        &harness.ctx(),
    )
    .await
    .expect("full content was supplied by the model");
}
