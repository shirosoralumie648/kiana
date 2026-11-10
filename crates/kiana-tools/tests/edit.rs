//! `edit.ts` 的移植测试。
//!
//! 覆盖 `tools.test.ts` 的 edit / fuzzy / CRLF 用例、`edit-tool-legacy-input.test.ts`
//! 的 `prepareArguments` 兼容行为、`file-mutation-queue.test.ts` 的共享队列与中止语义、
//! 以及 diff 预览（`computeEditsDiff`）的错误文案。

mod file_tools_test_support;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use file_tools_test_support::{TestContext, execute, execute_with_signal, text_output};
use kiana_ai::types::JsonValue;
use kiana_ext_api::tool_types::ReplaceEdit;
use kiana_tools::edit::{
    EditOperations, EditToolOptions, create_edit_tool, create_edit_tool_definition,
    edit_parameters_schema, edit_tool_system_prompt_contribution, prepare_edit_arguments,
};
use kiana_tools::edit_diff::compute_edits_diff;
use kiana_tools::read::ToolOperationError;
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

fn write(path: &Path, content: &str) {
    fs::write(path, content).expect("write fixture");
}

/// 模拟模型在 edit 之前用 read 工具读过整个文件。
///
/// 第 2 周起 edit/write 有 read-before-write 门禁；测试 fixture 直接落盘后需要显式建立
/// 读记录，否则会（正确地）返回 `ReadBeforeEdit`。目标不存在/是目录时没有读记录，
/// 由 access/read 产生原来的 ENOENT/EISDIR 文案。
fn record_read(path: &Path) {
    let _ = kiana_tools::read::ReadStateRegistry::global().record_full_read(path);
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("read result")
}

fn definition(dir: &Path) -> kiana_ext_api::tool_types::ToolDefinition {
    create_edit_tool_definition(dir.to_string_lossy(), None)
}

async fn edit(
    dir: &Path,
    path: &Path,
    edits: Vec<ReplaceEdit>,
) -> Result<kiana_agent_core::AgentToolResult<JsonValue>, kiana_agent_core::AgentToolError> {
    record_read(path);
    execute(
        &definition(dir),
        json!({ "path": path.to_string_lossy(), "edits": edits }),
        &TestContext::new(dir),
    )
    .await
}

fn replace(old: &str, new: &str) -> ReplaceEdit {
    ReplaceEdit {
        old_text: old.to_string(),
        new_text: new.to_string(),
    }
}

/// `diff` npm 包 `applyPatch` 的最小等价实现（仅用于统一 diff 格式的测试断言）。
///
/// TS 的模糊用例断言 `applyPatch(originalContent, details.patch) === expectedContent`；
/// Rust 没有 jsdiff 依赖，这里按 `generate_unified_patch` 的输出格式重放 hunk，验证补丁
/// 真的能还原编辑结果（而不仅是文件内容正确）。
fn apply_unified_patch(original: &str, patch: &str) -> String {
    let patch_lines: Vec<&str> = patch.split('\n').collect();
    let original_lines: Vec<&str> = original.split('\n').collect();
    let mut output: Vec<&str> = Vec::new();
    let mut cursor = 0usize;
    let mut index = 0usize;
    while index < patch_lines.len() {
        let line = patch_lines[index];
        index += 1;
        if !line.starts_with("@@") {
            continue;
        }
        let range = line
            .strip_prefix("@@ -")
            .and_then(|rest| rest.split(" +").next())
            .expect("hunk header");
        let (start, count) = range.split_once(',').expect("hunk range");
        let start: usize = start.parse().expect("hunk start");
        let count: usize = count.parse().expect("hunk count");
        // 统一 diff 的 0 长度侧写作 `-0,0`；此时起点不偏移。
        let target = if count == 0 { start } else { start - 1 };
        output.extend_from_slice(&original_lines[cursor..target]);
        cursor = target;
        while index < patch_lines.len() && !patch_lines[index].starts_with("@@") {
            let text_line = patch_lines[index];
            index += 1;
            if text_line.is_empty() {
                continue; // 补丁末尾的换行
            }
            match text_line.as_bytes()[0] {
                b' ' => {
                    output.push(&text_line[1..]);
                    cursor += 1;
                }
                b'-' => cursor += 1,
                b'+' => output.push(&text_line[1..]),
                _ => {} // `\ No newline at end of file`
            }
        }
    }
    output.extend_from_slice(&original_lines[cursor..]);
    output.join("\n")
}

#[tokio::test]
async fn replaces_text_and_returns_diff_and_patch() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("edit-test.txt");
    write(&file, "Hello, world!");
    let result = edit(temp.path(), &file, vec![replace("world", "testing")])
        .await
        .expect("edit");
    assert!(text_output(&result).contains("Successfully replaced"));
    let details = result.details.as_object().expect("details");
    let diff = details["diff"].as_str().expect("diff");
    let patch = details["patch"].as_str().expect("patch");
    assert!(diff.contains("testing"));
    assert!(patch.contains("--- "));
    assert!(patch.contains("+++ "));
    assert!(patch.contains("@@"));
    assert!(patch.contains("-Hello, world!"));
    assert!(patch.contains("+Hello, testing!"));
    assert_eq!(read(&file), "Hello, testing!");
    // 键顺序 diff/patch/firstChangedLine。
    let serialized = serde_json::to_string(&result.details).expect("serialize");
    assert!(serialized.starts_with("{\"diff\":"));
    assert!(serialized.contains(",\"patch\":"));
    assert!(serialized.contains(",\"firstChangedLine\":"));
}

#[tokio::test]
async fn missing_text_reports_the_exact_error() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("edit-test.txt");
    write(&file, "Hello, world!");
    let error = edit(temp.path(), &file, vec![replace("nonexistent", "testing")])
        .await
        .expect_err("not found");
    // 新错误契约：路径 + edit 下标 + 最近片段 + 明确的「不要重发」。
    assert!(
        error
            .message
            .starts_with(&format!("Edit 0 of 1 failed for {}:", file.display())),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("Nearest matches"),
        "{}",
        error.message
    );
    assert!(error.message.contains("line 1"), "{}", error.message);
    assert!(error.message.contains("Do not resend"), "{}", error.message);
    assert_eq!(read(&file), "Hello, world!");
}

#[tokio::test]
async fn missing_edit_target_reports_enoent() {
    let temp = TempDir::new().expect("temp");
    let missing = temp.path().join("missing.txt");
    let error = edit(temp.path(), &missing, vec![replace("hello", "world")])
        .await
        .expect_err("enoent");
    assert_eq!(
        error.message,
        format!(
            "Could not edit file: {}. Error code: ENOENT.",
            missing.display()
        )
    );
}

#[tokio::test]
async fn duplicate_text_is_rejected() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("edit-test.txt");
    write(&file, "foo foo foo");
    let error = edit(temp.path(), &file, vec![replace("foo", "bar")])
        .await
        .expect_err("duplicate");
    assert!(
        error
            .message
            .contains("oldText matches 3 times at line 1, line 1, line 1"),
        "{}",
        error.message
    );
    assert!(error.message.contains("Do not resend"), "{}", error.message);
    assert_eq!(read(&file), "foo foo foo");
}

#[tokio::test]
async fn replaces_multiple_disjoint_regions_in_one_call() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("edit-multi.txt");
    write(&file, "alpha\nbeta\ngamma\ndelta\n");
    let result = edit(
        temp.path(),
        &file,
        vec![replace("alpha\n", "ALPHA\n"), replace("gamma\n", "GAMMA\n")],
    )
    .await
    .expect("edit");
    assert!(text_output(&result).contains("Successfully replaced 2 block(s)"));
    assert_eq!(read(&file), "ALPHA\nbeta\nGAMMA\ndelta\n");
    let diff = result.details["diff"].as_str().expect("diff");
    assert!(diff.contains("ALPHA"));
    assert!(diff.contains("GAMMA"));
}

#[tokio::test]
async fn collapses_large_unchanged_gaps_in_multi_edit_diffs() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("edit-multi-large-gap.txt");
    let lines: Vec<String> = (1..=600).map(|index| format!("line {index:03}")).collect();
    write(&file, &format!("{}\n", lines.join("\n")));
    let result = edit(
        temp.path(),
        &file,
        vec![
            replace("line 100\n", "LINE 100\n"),
            replace("line 300\n", "LINE 300\n"),
            replace("line 500\n", "LINE 500\n"),
        ],
    )
    .await
    .expect("edit");
    let diff = result.details["diff"].as_str().expect("diff");
    assert!(diff.contains("LINE 100"));
    assert!(diff.contains("LINE 300"));
    assert!(diff.contains("LINE 500"));
    assert!(diff.contains("..."));
    assert!(!diff.contains("line 250"));
    assert!(diff.split('\n').count() < 50, "{diff}");
}

#[tokio::test]
async fn matches_edits_against_the_original_file() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("edit-multi-original.txt");
    write(&file, "foo\nbar\nbaz\n");
    edit(
        temp.path(),
        &file,
        vec![replace("foo\n", "foo bar\n"), replace("bar\n", "BAR\n")],
    )
    .await
    .expect("edit");
    assert_eq!(read(&file), "foo bar\nBAR\nbaz\n");
}

#[tokio::test]
async fn empty_edits_are_rejected() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("edit-empty-edits.txt");
    write(&file, "hello\nworld\n");
    let error = execute(
        &definition(temp.path()),
        json!({ "path": file.to_string_lossy(), "edits": [] }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("empty edits");
    assert_eq!(
        error.message,
        "Edit tool input is invalid. edits must contain at least one replacement."
    );
}

#[tokio::test]
async fn overlapping_edits_are_rejected() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("edit-overlap.txt");
    write(&file, "one\ntwo\nthree\n");
    let error = edit(
        temp.path(),
        &file,
        vec![
            replace("one\ntwo\n", "ONE\nTWO\n"),
            replace("two\nthree\n", "TWO\nTHREE\n"),
        ],
    )
    .await
    .expect_err("overlap");
    assert!(
        error.message.contains("overlaps edits[0]"),
        "{}",
        error.message
    );
    assert!(error.message.contains("Do not resend"), "{}", error.message);
    assert_eq!(read(&file), "one\ntwo\nthree\n");
}

#[tokio::test]
async fn does_not_partially_apply_edits_when_one_fails() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("edit-no-partial.txt");
    let original = "alpha\nbeta\ngamma\n";
    write(&file, original);
    edit(
        temp.path(),
        &file,
        vec![
            replace("alpha\n", "ALPHA\n"),
            replace("missing\n", "MISSING\n"),
        ],
    )
    .await
    .expect_err("not found");
    assert_eq!(read(&file), original);
}

#[tokio::test]
async fn unreadable_preview_reports_eacces() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("unreadable-preview.txt");
    write(&file, "hello\n");
    let mut permissions = fs::metadata(&file).expect("metadata").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o222);
    fs::set_permissions(&file, permissions).expect("chmod");
    // root 会忽略权限位：此时跳过（CI 以普通用户运行）。
    if fs::read(&file).is_ok() {
        return;
    }
    let error = compute_edits_diff(
        &file.to_string_lossy(),
        &[replace("hello", "world")],
        &temp.path().to_string_lossy(),
    )
    .await
    .expect_err("eacces");
    assert_eq!(
        error.error,
        format!(
            "Could not edit file: {}. Error code: EACCES.",
            file.display()
        )
    );
}

#[tokio::test]
async fn readonly_file_reports_eacces() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("edit-readonly.txt");
    write(&file, "hello\n");
    let mut permissions = fs::metadata(&file).expect("metadata").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o444);
    fs::set_permissions(&file, permissions).expect("chmod");
    if fs::OpenOptions::new().write(true).open(&file).is_ok() {
        return;
    }
    let error = edit(temp.path(), &file, vec![replace("hello", "world")])
        .await
        .expect_err("eacces");
    assert_eq!(
        error.message,
        format!(
            "Could not edit file: {}. Error code: EACCES.",
            file.display()
        )
    );
}

#[tokio::test]
async fn unknown_access_errors_use_the_stringified_error() {
    struct FailingAccess;

    #[async_trait]
    impl EditOperations for FailingAccess {
        async fn read_file(&self, _path: &Path) -> Result<Vec<u8>, ToolOperationError> {
            Ok(b"hello\n".to_vec())
        }

        async fn write_file(&self, _path: &Path, _content: &str) -> Result<(), ToolOperationError> {
            Ok(())
        }

        async fn access(&self, _path: &Path) -> Result<(), ToolOperationError> {
            Err(ToolOperationError::new("disk offline"))
        }
    }

    let temp = TempDir::new().expect("temp");
    let definition = create_edit_tool_definition(
        temp.path().to_string_lossy(),
        Some(EditToolOptions {
            operations: Some(Arc::new(FailingAccess)),
            ..EditToolOptions::default()
        }),
    );
    let error = execute(
        &definition,
        json!({ "path": "broken.txt", "edits": [{ "oldText": "hello", "newText": "world" }] }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("access");
    assert_eq!(
        error.message,
        "Could not edit file: broken.txt. Error: disk offline."
    );
}

#[tokio::test]
async fn diff_preview_reports_enoent() {
    let temp = TempDir::new().expect("temp");
    let missing = temp.path().join("missing-preview.txt");
    let error = compute_edits_diff(
        &missing.to_string_lossy(),
        &[replace("hello", "world")],
        &temp.path().to_string_lossy(),
    )
    .await
    .expect_err("enoent");
    assert_eq!(
        error.error,
        format!(
            "Could not edit file: {}. Error code: ENOENT.",
            missing.display()
        )
    );
}

// ---------------------------------------------------------------------------
// fuzzy matching（tools.test.ts 的 12 个用例）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn matches_text_with_trailing_whitespace_stripped() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("trailing-ws.txt");
    write(&file, "line one   \nline two  \nline three\n");
    edit(
        temp.path(),
        &file,
        vec![replace("line one\nline two\n", "replaced\n")],
    )
    .await
    .expect("fuzzy");
    assert_eq!(read(&file), "replaced\nline three\n");
}

#[tokio::test]
async fn matches_fullwidth_punctuation_in_chinese_text() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("chinese-punctuation.txt");
    write(&file, "你好，世界\n你好（世界）\n");
    edit(
        temp.path(),
        &file,
        vec![replace(
            "你好,世界\n你好(世界)\n",
            "你好，kiana\n你好(kiana)\n",
        )],
    )
    .await
    .expect("fuzzy");
    assert_eq!(read(&file), "你好，kiana\n你好(kiana)\n");
}

#[tokio::test]
async fn matches_compatibility_equivalent_unicode_forms() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("unicode-compatibility.txt");
    write(&file, "ＡＢＣ１２３\ncafe\u{0301}\n");
    edit(
        temp.path(),
        &file,
        vec![replace("ABC123\ncafé\n", "XYZ789\ncoffee\n")],
    )
    .await
    .expect("fuzzy");
    assert_eq!(read(&file), "XYZ789\ncoffee\n");
}

#[tokio::test]
async fn matches_smart_single_and_double_quotes() {
    let temp = TempDir::new().expect("temp");
    let single = temp.path().join("smart-quotes.txt");
    write(&single, "console.log(\u{2018}hello\u{2019});\n");
    edit(
        temp.path(),
        &single,
        vec![replace("console.log('hello');", "console.log('world');")],
    )
    .await
    .expect("fuzzy single");
    assert!(read(&single).contains("world"));

    let double = temp.path().join("smart-double-quotes.txt");
    write(&double, "const msg = \u{201c}Hello World\u{201d};\n");
    edit(
        temp.path(),
        &double,
        vec![replace(
            "const msg = \"Hello World\";",
            "const msg = \"Goodbye\";",
        )],
    )
    .await
    .expect("fuzzy double");
    assert!(read(&double).contains("Goodbye"));
}

#[tokio::test]
async fn matches_unicode_dashes_and_nbsp() {
    let temp = TempDir::new().expect("temp");
    let dashes = temp.path().join("unicode-dashes.txt");
    write(&dashes, "range: 1\u{2013}5\nbreak\u{2014}here\n");
    edit(
        temp.path(),
        &dashes,
        vec![replace(
            "range: 1-5\nbreak-here",
            "range: 10-50\nbreak--here",
        )],
    )
    .await
    .expect("fuzzy dashes");
    assert!(read(&dashes).contains("10-50"));

    let nbsp = temp.path().join("nbsp.txt");
    write(&nbsp, "hello\u{00a0}world\n");
    edit(
        temp.path(),
        &nbsp,
        vec![replace("hello world", "hello universe")],
    )
    .await
    .expect("fuzzy nbsp");
    assert!(read(&nbsp).contains("universe"));
}

#[tokio::test]
async fn prefers_exact_matches() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("exact-preferred.txt");
    write(&file, "const x = 'exact';\nconst y = 'other';\n");
    edit(
        temp.path(),
        &file,
        vec![replace("const x = 'exact';", "const x = 'changed';")],
    )
    .await
    .expect("exact");
    assert_eq!(read(&file), "const x = 'changed';\nconst y = 'other';\n");
}

#[tokio::test]
async fn still_fails_when_text_is_not_found_even_with_fuzzy_matching() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("no-match.txt");
    write(&file, "completely different content\n");
    let error = edit(
        temp.path(),
        &file,
        vec![replace("this does not exist", "replacement")],
    )
    .await
    .expect_err("not found");
    assert!(
        error.message.starts_with("Edit 0 of 1 failed for "),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn detects_duplicates_after_fuzzy_normalization() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("fuzzy-dups.txt");
    write(&file, "hello world   \nhello world\n");
    let error = edit(temp.path(), &file, vec![replace("hello world", "replaced")])
        .await
        .expect_err("duplicate");
    assert!(
        error.message.contains("matches 2 times"),
        "{}",
        error.message
    );
}

/// 纯空白的 `oldText`：精确匹配能定位就按出现次数处理；定位不到且模糊归一化后为空
/// 时按 `EmptyReplacement` 拒绝（不再复刻 TS 的 UTF-16 code unit trick）。
#[tokio::test]
async fn whitespace_only_old_text_is_rejected_or_counted() {
    let temp = TempDir::new().expect("temp");

    let single = temp.path().join("ws-single.txt");
    write(&single, "a");
    let error = edit(temp.path(), &single, vec![replace(" ", "X")])
        .await
        .expect_err("empty after normalization");
    assert!(
        error.message.contains("oldText must not be empty"),
        "{}",
        error.message
    );
    assert_eq!(read(&single), "a");

    let spaced = temp.path().join("ws-spaced.txt");
    write(&spaced, "a b c");
    let error = edit(temp.path(), &spaced, vec![replace(" ", "X")])
        .await
        .expect_err("two matches");
    assert!(
        error.message.starts_with("Edit 0 of 1 failed for ")
            && error.message.contains("matches 2 times"),
        "{}",
        error.message
    );

    let emoji = temp.path().join("ws-emoji.txt");
    write(&emoji, "😀😀");
    let error = edit(temp.path(), &emoji, vec![replace(" ", "X")])
        .await
        .expect_err("empty after normalization");
    assert!(
        error.message.contains("oldText must not be empty"),
        "{}",
        error.message
    );
}

/// Node 的 `fs.access(R_OK|W_OK)` 对可写目录成功；失败发生在 `readFile` 的 read 阶段，
/// 错误原样抛出（不套 `Could not edit file`）。
#[tokio::test]
async fn editing_a_directory_reports_the_readfile_stage_error() {
    let temp = TempDir::new().expect("temp");
    let directory = temp.path().join("sub");
    fs::create_dir(&directory).expect("mkdir");
    let error = edit(temp.path(), &directory, vec![replace("a", "b")])
        .await
        .expect_err("directory");
    assert_eq!(
        error.message,
        format!(
            "EISDIR: illegal operation on a directory, read '{}'",
            directory.display()
        )
    );
}

#[tokio::test]
async fn edit_preview_of_a_directory_reports_the_read_stage_error() {
    let temp = TempDir::new().expect("temp");
    let directory = temp.path().join("preview-dir");
    fs::create_dir(&directory).expect("mkdir");
    let result = compute_edits_diff(
        directory.to_string_lossy().as_ref(),
        &[replace("a", "b")],
        temp.path().to_string_lossy().as_ref(),
    )
    .await
    .expect_err("directory preview");
    // TS 的 access(R_OK) 对目录成功，readFile 抛 `EISDIR ..., read '<abs path>'`。
    assert_eq!(
        result.error,
        format!(
            "EISDIR: illegal operation on a directory, read '{}'",
            directory.display()
        )
    );
}

#[tokio::test]
async fn supports_fuzzy_matching_in_multi_edit_mode() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("fuzzy-multi.txt");
    write(
        &file,
        "console.log(\u{2018}hello\u{2019});\nhello\u{00a0}world\n",
    );
    edit(
        temp.path(),
        &file,
        vec![
            replace("console.log('hello');\n", "console.log('world');\n"),
            replace("hello world\n", "hello universe\n"),
        ],
    )
    .await
    .expect("fuzzy multi");
    assert_eq!(read(&file), "console.log('world');\nhello universe\n");
}

#[tokio::test]
async fn preserves_the_correct_occurrence_when_fuzzy_replacement_equals_a_nearby_line() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("fuzzy-preserve-duplicate-line.txt");
    let original = "replace me   \nafter   \n";
    write(&file, original);
    let result = edit(temp.path(), &file, vec![replace("replace me\n", "after\n")])
        .await
        .expect("fuzzy");
    let expected = "after\nafter   \n";
    assert_eq!(read(&file), expected);
    let patch = result
        .details
        .get("patch")
        .and_then(JsonValue::as_str)
        .expect("patch");
    assert_eq!(apply_unified_patch(original, patch), expected);
    assert_eq!(result.details["firstChangedLine"], json!(1));
}

#[tokio::test]
async fn preserves_untouched_lines_for_fuzzy_multi_edits() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("fuzzy-preserve-multi.txt");
    let original = [
        "keep before  ",
        "first target  ",
        "first after",
        "keep middle   ",
        "second target  ",
        "second after",
        "keep after  ",
        "",
    ]
    .join("\n");
    write(&file, &original);
    let result = edit(
        temp.path(),
        &file,
        vec![
            replace("first target\nfirst after", "FIRST\nFIRST2"),
            replace("second target\nsecond after", "SECOND\nSECOND2"),
        ],
    )
    .await
    .expect("fuzzy multi");
    let expected = [
        "keep before  ",
        "FIRST",
        "FIRST2",
        "keep middle   ",
        "SECOND",
        "SECOND2",
        "keep after  ",
        "",
    ]
    .join("\n");
    assert_eq!(read(&file), expected);
    let patch = result
        .details
        .get("patch")
        .and_then(JsonValue::as_str)
        .expect("patch");
    assert_eq!(apply_unified_patch(&original, patch), expected);
    assert_eq!(result.details["firstChangedLine"], json!(2));
}

// ---------------------------------------------------------------------------
// CRLF 与 BOM（tools.test.ts 的 6 个用例）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn matches_lf_old_text_against_crlf_content() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("crlf-test.txt");
    write(&file, "line one\r\nline two\r\nline three\r\n");
    edit(
        temp.path(),
        &file,
        vec![replace("line two\n", "replaced line\n")],
    )
    .await
    .expect("crlf");
    assert_eq!(read(&file), "line one\r\nreplaced line\r\nline three\r\n");
}

#[tokio::test]
async fn preserves_crlf_line_endings() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("crlf-preserve.txt");
    write(&file, "first\r\nsecond\r\nthird\r\n");
    edit(temp.path(), &file, vec![replace("second\n", "REPLACED\n")])
        .await
        .expect("crlf preserve");
    assert_eq!(read(&file), "first\r\nREPLACED\r\nthird\r\n");
}

#[tokio::test]
async fn preserves_lf_line_endings() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("lf-preserve.txt");
    write(&file, "first\nsecond\nthird\n");
    edit(temp.path(), &file, vec![replace("second\n", "REPLACED\n")])
        .await
        .expect("lf");
    assert_eq!(read(&file), "first\nREPLACED\nthird\n");
}

#[tokio::test]
async fn detects_duplicates_across_crlf_and_lf_variants() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("mixed-endings.txt");
    write(&file, "hello\r\nworld\r\n---\r\nhello\nworld\n");
    let error = edit(
        temp.path(),
        &file,
        vec![replace("hello\nworld\n", "replaced\n")],
    )
    .await
    .expect_err("duplicate");
    assert!(
        error.message.contains("matches 2 times"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn preserves_utf8_bom() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("bom-test.txt");
    write(&file, "\u{feff}first\r\nsecond\r\nthird\r\n");
    edit(temp.path(), &file, vec![replace("second\n", "REPLACED\n")])
        .await
        .expect("bom");
    assert_eq!(read(&file), "\u{feff}first\r\nREPLACED\r\nthird\r\n");
}

#[tokio::test]
async fn preserves_utf8_bom_and_crlf_multi_edit() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("bom-crlf-multi.txt");
    write(&file, "\u{feff}first\r\nsecond\r\nthird\r\nfourth\r\n");
    edit(
        temp.path(),
        &file,
        vec![
            replace("second\n", "SECOND\n"),
            replace("fourth\n", "FOURTH\n"),
        ],
    )
    .await
    .expect("bom crlf");
    assert_eq!(
        read(&file),
        "\u{feff}first\r\nSECOND\r\nthird\r\nFOURTH\r\n"
    );
}

// ---------------------------------------------------------------------------
// prepareArguments（edit-tool-legacy-input.test.ts）
// ---------------------------------------------------------------------------

#[test]
fn schema_keeps_legacy_fields_out() {
    let parameters = edit_parameters_schema();
    let properties = parameters["properties"].as_object().expect("properties");
    assert!(!properties.contains_key("oldText"));
    assert!(!properties.contains_key("newText"));
}

#[test]
fn prepare_arguments_folds_legacy_fields_and_stringified_edits() {
    let definition = create_edit_tool_definition("/", None);
    let prepare = definition.prepare_arguments.expect("prepareArguments");
    // legacy 折叠返回**新对象**（TS 的 `{...rest, edits}`），原对象只保留步骤 1-2 的改写。
    let mut input = json!({ "path": "file.txt", "oldText": "before", "newText": "after" });
    let prepared = prepare(&mut input).expect("new object");
    assert_eq!(
        prepared,
        json!({ "path": "file.txt", "edits": [{ "oldText": "before", "newText": "after" }] })
    );
    assert_eq!(
        input,
        json!({ "path": "file.txt", "oldText": "before", "newText": "after" })
    );

    let mut input = json!({
        "path": "file.txt",
        "edits": [{ "oldText": "a", "newText": "b" }],
        "oldText": "c",
        "newText": "d",
    });
    let prepared = prepare(&mut input).expect("new object");
    assert_eq!(
        prepared,
        json!({
            "path": "file.txt",
            "edits": [
                { "oldText": "a", "newText": "b" },
                { "oldText": "c", "newText": "d" }
            ]
        })
    );
    assert_eq!(input.get("oldText"), Some(&json!("c")));
    assert_eq!(input.get("newText"), Some(&json!("d")));

    let mut input =
        json!({ "path": "file.txt", "edits": "[{\"oldText\":\"a\",\"newText\":\"b\"}]" });
    assert!(prepare(&mut input).is_none());
    assert_eq!(
        input,
        json!({ "path": "file.txt", "edits": [{ "oldText": "a", "newText": "b" }] })
    );

    let mut input = json!({ "path": "file.txt", "edits": "not json" });
    assert!(prepare(&mut input).is_none());
    assert_eq!(input, json!({ "path": "file.txt", "edits": "not json" }));

    // 组合：edits 是 JSON 字符串 + legacy 顶层字段 + 额外键。TS 先在原对象上把 edits 解析成
    // 数组（键位置不变），再用 `{...rest, edits}` 生成新对象（edits 保持原位，extra 在其后）。
    let mut input = json!({
        "path": "file.txt",
        "edits": "[{\"oldText\":\"a\",\"newText\":\"b\"}]",
        "oldText": "c",
        "newText": "d",
        "extra": 1,
    });
    let prepared = prepare(&mut input).expect("new object");
    assert_eq!(
        serde_json::to_string(&prepared).expect("serialize"),
        r#"{"path":"file.txt","edits":[{"oldText":"a","newText":"b"},{"oldText":"c","newText":"d"}],"extra":1}"#
    );
    assert_eq!(
        serde_json::to_string(&input).expect("serialize"),
        r#"{"path":"file.txt","edits":[{"oldText":"a","newText":"b"}],"oldText":"c","newText":"d","extra":1}"#
    );
}

/// TS: `args.edits = [args.edits]` —— 单个 edit 对象原样包裹，保留额外属性与键顺序。
#[test]
fn prepare_arguments_wraps_a_single_edit_object_unchanged() {
    let definition = create_edit_tool_definition("/", None);
    let prepare = definition.prepare_arguments.expect("prepareArguments");

    let mut input = json!({ "path": "f", "edits": { "newText": "b", "oldText": "a" } });
    assert!(prepare(&mut input).is_none());
    assert_eq!(
        serde_json::to_string(&input["edits"]).expect("serialize"),
        "[{\"newText\":\"b\",\"oldText\":\"a\"}]"
    );

    let mut input = json!({ "path": "f", "edits": { "oldText": "a", "newText": "b", "x": 1 } });
    assert!(prepare(&mut input).is_none());
    assert_eq!(
        serde_json::to_string(&input["edits"]).expect("serialize"),
        "[{\"oldText\":\"a\",\"newText\":\"b\",\"x\":1}]"
    );

    // JSON 字符串形式的单个 edit：解析后原样包裹（额外属性保留）。
    let mut input = json!({
        "path": "f",
        "edits": "{\"newText\":\"b\",\"oldText\":\"a\",\"x\":1}",
    });
    assert!(prepare(&mut input).is_none());
    assert_eq!(
        serde_json::to_string(&input["edits"]).expect("serialize"),
        "[{\"newText\":\"b\",\"oldText\":\"a\",\"x\":1}]"
    );
}

#[tokio::test]
async fn prepared_legacy_arguments_execute_correctly() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("legacy.txt");
    write(&file, "before\n");
    let definition = create_edit_tool_definition(temp.path().to_string_lossy(), None);
    let prepare = definition
        .prepare_arguments
        .clone()
        .expect("prepareArguments");
    record_read(&file);
    let mut input = json!({ "path": "legacy.txt", "oldText": "before", "newText": "after" });
    let prepared = prepare(&mut input).expect("legacy fold returns a new object");
    let result = execute(&definition, prepared, &TestContext::new(temp.path()))
        .await
        .expect("execute");
    assert_eq!(
        text_output(&result),
        "Successfully replaced 1 block(s) in legacy.txt."
    );
    assert_eq!(read(&file), "after\n");
}

#[test]
fn prepare_edit_arguments_is_public() {
    let mut input = json!({ "path": "f", "oldText": "a", "newText": "b" });
    let prepared = prepare_edit_arguments(&mut input).expect("legacy fold");
    assert!(prepared.get("edits").is_some());
    // 原对象保持 TS 语义（顶层 legacy 字段仍在，返回的是新对象）。
    assert_eq!(input.get("oldText"), Some(&json!("a")));
}

// ---------------------------------------------------------------------------
// mutation queue 与中止
// ---------------------------------------------------------------------------

#[tokio::test]
async fn uses_the_context_cwd() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("ctx-cwd-edit.txt");
    write(&file, "old text");
    record_read(&file);
    let definition = create_edit_tool_definition("/", None);
    execute(
        &definition,
        json!({ "path": "ctx-cwd-edit.txt", "edits": [{ "oldText": "old text", "newText": "new text" }] }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("edit");
    assert_eq!(read(&file), "new text");
}

#[tokio::test]
async fn pre_aborted_edit_rejects() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("aborted.txt");
    write(&file, "hello\n");
    let signal = CancellationToken::new();
    signal.cancel();
    let error = execute_with_signal(
        &definition(temp.path()),
        json!({ "path": file.to_string_lossy(), "edits": [{ "oldText": "hello", "newText": "world" }] }),
        &signal,
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("aborted");
    assert_eq!(error.message, "Operation aborted");
    assert_eq!(read(&file), "hello\n");
}

/// 慢速读写操作（模拟 in-flight 文件操作）。
struct SlowOperations {
    first_started: Arc<Notify>,
    release_first: Arc<Notify>,
    second_started: Arc<Notify>,
    first_settled: Arc<AtomicBool>,
}

#[async_trait]
impl EditOperations for SlowOperations {
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, ToolOperationError> {
        tokio::fs::read(path)
            .await
            .map_err(|error| ToolOperationError::from_io("open", path, error))
    }

    async fn write_file(&self, path: &Path, content: &str) -> Result<(), ToolOperationError> {
        if content == "ALPHA\nbeta\n" {
            self.first_started.notify_one();
            self.release_first.notified().await;
            tokio::fs::write(path, content)
                .await
                .map_err(|error| ToolOperationError::from_io("open", path, error))?;
            self.first_settled.store(true, Ordering::SeqCst);
            return Ok(());
        }
        if content == "ALPHA\nBETA\n" || content == "alpha\nBETA\n" {
            assert!(
                self.first_settled.load(Ordering::SeqCst),
                "second edit must wait for the aborted first edit"
            );
            self.second_started.notify_one();
        }
        tokio::fs::write(path, content)
            .await
            .map_err(|error| ToolOperationError::from_io("open", path, error))
    }

    async fn access(&self, path: &Path) -> Result<(), ToolOperationError> {
        tokio::fs::metadata(path)
            .await
            .map(|_| ())
            .map_err(|error| ToolOperationError::from_io("access", path, error))
    }
}

#[tokio::test]
async fn keeps_the_edit_queue_locked_while_an_aborted_edit_write_is_in_flight() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("abort-edit.txt");
    write(&file, "alpha\nbeta\n");
    record_read(&file);
    let first_started = Arc::new(Notify::new());
    let release_first = Arc::new(Notify::new());
    let second_started = Arc::new(Notify::new());
    let first_settled = Arc::new(AtomicBool::new(false));
    let definition = create_edit_tool_definition(
        temp.path().to_string_lossy(),
        Some(EditToolOptions {
            operations: Some(Arc::new(SlowOperations {
                first_started: Arc::clone(&first_started),
                release_first: Arc::clone(&release_first),
                second_started: Arc::clone(&second_started),
                first_settled: Arc::clone(&first_settled),
            })),
            ..EditToolOptions::default()
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
                json!({ "path": file.to_string_lossy(), "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] }),
                &signal,
                &ctx,
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), first_started.notified())
        .await
        .expect("first edit started");
    signal.cancel();

    let second = tokio::spawn({
        let definition = definition.clone();
        let ctx = Arc::clone(&ctx);
        let file = file.clone();
        async move {
            execute(
                &definition,
                json!({ "path": file.to_string_lossy(), "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
                &ctx,
            )
            .await
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), second_started.notified())
            .await
            .is_err(),
        "second edit must stay blocked while the first is in flight"
    );

    release_first.notify_one();
    let error = first.await.expect("join").expect_err("aborted");
    assert_eq!(error.message, "Operation aborted");
    second.await.expect("join").expect("second edit");
    assert_eq!(read(&file), "ALPHA\nBETA\n");
}

#[tokio::test]
async fn preserves_both_parallel_edits_on_the_same_file() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("parallel-edit.txt");
    write(&file, "alpha\nbeta\ngamma\n");
    record_read(&file);
    let definition = definition(temp.path());
    let ctx = TestContext::new(temp.path());
    let (first, second) = tokio::join!(
        execute(
            &definition,
            json!({ "path": file.to_string_lossy(), "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] }),
            &ctx,
        ),
        execute(
            &definition,
            json!({ "path": file.to_string_lossy(), "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
            &ctx,
        )
    );
    first.expect("first");
    second.expect("second");
    assert_eq!(read(&file), "ALPHA\nBETA\ngamma\n");
}

#[test]
fn definition_and_contribution_agree() {
    let definition = create_edit_tool_definition("/workspace", None);
    let contribution = edit_tool_system_prompt_contribution();
    assert_eq!(
        definition.prompt_snippet.as_deref(),
        Some(contribution.snippet)
    );
    assert_eq!(definition.prompt_guidelines.as_ref().map(Vec::len), Some(4));
    assert_eq!(
        definition.render_shell,
        Some(kiana_ext_api::tool_types::RenderShell::SelfRender)
    );
    assert!(definition.prepare_arguments.is_some());
    let tool = create_edit_tool("/", None);
    assert_eq!(tool.name, "edit");
}

// ---------------------------------------------------------------------------
// realpath key errors（file-mutation-queue.ts#getMutationQueueKey 的 rethrow）
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn realpath_errors_abort_before_the_edit_callback() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("temp");
    let loop_path = temp.path().join("loop");
    symlink(&loop_path, &loop_path).expect("symlink loop");
    let error = edit(temp.path(), &loop_path, vec![replace("a", "b")])
        .await
        .expect_err("ELOOP");
    assert_eq!(
        error.message,
        format!(
            "ELOOP: too many symbolic links encountered, realpath '{}'",
            loop_path.display()
        ),
        "the queue error must not be rewritten as `Could not edit file: ...`"
    );
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
    write(&file, "hello");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");

    let outcome = edit(temp.path(), &file, vec![replace("hello", "world")]).await;
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("restore chmod");

    let error = outcome.expect_err("EACCES");
    assert_eq!(
        error.message,
        format!("EACCES: permission denied, realpath '{}'", file.display())
    );
    assert_eq!(read(&file), "hello");
}

// ---------------------------------------------------------------------------
// 第 7 周：错误契约、逐 edit 状态、策略 metadata、hunk/片段
// ---------------------------------------------------------------------------

#[tokio::test]
async fn not_found_reports_three_nearest_line_ranges() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("nearest.txt");
    write(
        &file,
        "fn alpha() {\n    run_alpha();\n}\n\n\
         fn beta() {\n    run_beta();\n}\n\n\
         fn gamma() {\n    run_gamma();\n}\n",
    );
    let error = edit(
        temp.path(),
        &file,
        vec![replace(
            "fn alpha() {\n    totally_different_call(42);\n}",
            "x",
        )],
    )
    .await
    .expect_err("not found");
    assert!(
        error.message.contains("Nearest matches"),
        "{}",
        error.message
    );
    // 最多 3 个候选，每个都带 1-based 行区间与相似度。
    for rank in 1..=3 {
        assert!(
            error.message.contains(&format!("  {rank}. lines ")),
            "{}",
            error.message
        );
    }
    assert!(error.message.contains("similarity"), "{}", error.message);
    assert!(error.message.contains("Do not resend"), "{}", error.message);
    assert_eq!(
        read(&file),
        "fn alpha() {\n    run_alpha();\n}\n\n\
         fn beta() {\n    run_beta();\n}\n\n\
         fn gamma() {\n    run_gamma();\n}\n"
    );
}

#[tokio::test]
async fn not_unique_lists_every_occurrence_line() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("dups.txt");
    write(&file, "alpha\nbeta\nalpha\ngamma\nalpha\n");
    let error = edit(temp.path(), &file, vec![replace("alpha\n", "x\n")])
        .await
        .expect_err("duplicate");
    assert!(
        error
            .message
            .contains("matches 3 times at line 1, line 3, line 5"),
        "{}",
        error.message
    );
    assert_eq!(read(&file), "alpha\nbeta\nalpha\ngamma\nalpha\n");
}

#[tokio::test]
async fn already_applied_is_reported_without_writing() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("already.txt");
    write(&file, "const answer = 42;\n");
    let error = edit(
        temp.path(),
        &file,
        vec![replace("const answer = 41;", "const answer = 42;")],
    )
    .await
    .expect_err("already applied");
    assert!(
        error.message.contains("already applied"),
        "{}",
        error.message
    );
    assert!(error.message.contains("line 1"), "{}", error.message);
    assert!(error.message.contains("Do not resend"), "{}", error.message);
    assert_eq!(read(&file), "const answer = 42;\n");
}

#[tokio::test]
async fn multi_edit_failure_reports_per_edit_status_and_writes_nothing() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("per-edit.txt");
    write(&file, "alpha\nbeta\ngamma\n");
    let error = edit(
        temp.path(),
        &file,
        vec![
            replace("alpha\n", "ALPHA\n"),
            replace("missing\n", "MISSING\n"),
            replace("gamma\n", "GAMMA\n"),
        ],
    )
    .await
    .expect_err("one bad edit");
    assert!(
        error.message.contains("edits[0]: matched"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("edits[1]: failed"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("edits[2]: matched"),
        "{}",
        error.message
    );
    assert!(error.message.contains("not written"), "{}", error.message);
    assert_eq!(read(&file), "alpha\nbeta\ngamma\n");
}

#[tokio::test]
async fn fuzzy_strategy_and_hunk_snippet_appear_in_details() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("strategy.txt");
    write(&file, "line one   \nline two  \nline three\n");
    let result = edit(
        temp.path(),
        &file,
        vec![replace("line one\nline two\n", "replaced\n")],
    )
    .await
    .expect("fuzzy");
    assert_eq!(result.details["strategy"], json!("fuzzy"));
    assert_eq!(result.details["strategyVariant"], json!("Fuzzy"));
    assert_eq!(
        result.details["editStrategies"][0]["strategy"],
        json!("fuzzy")
    );
    assert_eq!(result.details["editStrategies"][0]["editIndex"], json!(0));
    let hunk = result.details["hunks"][0]["hunk"]
        .as_str()
        .expect("hunk string");
    assert!(hunk.starts_with("@@ -"), "{hunk}");
    assert!(hunk.contains("-line one"), "{hunk}");
    assert!(hunk.contains("+replaced"), "{hunk}");
    let snippet = result.details["edits"][0]["snippet"]
        .as_str()
        .expect("snippet");
    assert!(snippet.contains(">1 | replaced"), "{snippet}");
    assert!(snippet.contains(" 2 | line three"), "{snippet}");
    assert_eq!(read(&file), "replaced\nline three\n");
}

#[tokio::test]
async fn exact_strategy_is_reported() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("exact-strategy.txt");
    write(&file, "hello\nworld\n");
    let result = edit(temp.path(), &file, vec![replace("hello", "HELLO")])
        .await
        .expect("exact");
    assert_eq!(result.details["strategy"], json!("exact"));
}

#[tokio::test]
async fn quote_normalized_strategy_is_reported() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("quote-strategy.txt");
    write(&file, "console.log(\u{2018}hello\u{2019});\n");
    let result = edit(
        temp.path(),
        &file,
        vec![replace("console.log('hello');", "console.log('world');")],
    )
    .await
    .expect("quote normalized");
    assert_eq!(result.details["strategy"], json!("quoteNormalized"));
}

#[tokio::test]
async fn file_changed_since_read_uses_file_changed_error() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("changed.txt");
    write(&file, "original\n");
    record_read(&file);
    // 模拟外部（用户/linter）在 read 之后改写文件。
    write(&file, "modified externally\n");
    let error = execute(
        &definition(temp.path()),
        json!({
            "path": file.to_string_lossy(),
            "edits": [{ "oldText": "original\n", "newText": "updated\n" }]
        }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("file changed");
    assert!(
        error.message.contains("FileChangedSinceRead"),
        "{}",
        error.message
    );
    assert!(error.message.contains("do not resend"), "{}", error.message);
    assert_eq!(read(&file), "modified externally\n");
}
