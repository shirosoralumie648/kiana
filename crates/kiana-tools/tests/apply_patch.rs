//! 第 7 周 `apply_patch` 的集成测试。
//!
//! - 黄金 fixture（`fixtures/apply_patch.golden.json`）覆盖 add / update / delete / move、
//!   多文件、多 hunk、CRLF、Unicode 标点、EOF、重复 context、空 patch、lenient heredoc；
//!   初始文件与期望文件都按**字节**比较；
//! - stale context：任何 hunk 失败时所有文件零写入；
//! - diff preview 与最终写入一致；
//! - 写失败回滚；
//! - `tools.apply_patch.models` 的模型路由（只对配置的模型暴露 apply_patch）。

mod file_tools_test_support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use file_tools_test_support::{TestContext, execute, text_output};
use kiana_ext_api::tool_types::ToolDefinition;
use kiana_tools::apply_patch::{
    ApplyPatchMetadata, ApplyPatchOperations, ApplyPatchSettings, ApplyPatchToolOptions,
    apply_patch_parameters_schema, create_apply_patch_tool_definition,
    prepare_apply_patch_arguments, preview_patch,
};
use kiana_tools::index::{
    create_all_tool_definitions_for_model, create_coding_tool_definitions_for_model,
    edit_tool_name_for_model, should_expose_apply_patch,
};
use kiana_tools::read::ToolOperationError;
use serde::Deserialize;
use serde_json::json;
use tempfile::TempDir;

fn definition(dir: &Path, options: Option<ApplyPatchToolOptions>) -> ToolDefinition {
    create_apply_patch_tool_definition(dir.to_string_lossy(), options)
}

#[derive(Debug, Deserialize)]
struct Fixture {
    cases: Vec<FixtureCase>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureCase {
    name: String,
    patch: String,
    #[serde(default)]
    files: BTreeMap<String, String>,
    #[serde(default)]
    expected_files: BTreeMap<String, String>,
    #[serde(default)]
    absent: Vec<String>,
    #[serde(default)]
    summary_contains: Vec<String>,
}

fn load_fixture() -> Fixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/apply_patch.golden.json");
    let text = fs::read_to_string(&path).expect("read fixture");
    serde_json::from_str(&text).expect("parse fixture")
}

async fn run_fixture_case(case: &FixtureCase) {
    let temp = TempDir::new().expect("temp");
    for (path, contents) in &case.files {
        let absolute = temp.path().join(path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&absolute, contents).expect("write initial");
    }
    let result = execute(
        &definition(temp.path(), None),
        json!({ "patch": case.patch }),
        &TestContext::new(temp.path()),
    )
    .await
    .unwrap_or_else(|error| panic!("{} failed: {}", case.name, error.message));
    let summary = text_output(&result);
    for needle in &case.summary_contains {
        assert!(
            summary.contains(needle),
            "{} summary missing {needle:?}: {summary}",
            case.name
        );
    }
    for (path, expected) in &case.expected_files {
        let actual = fs::read(temp.path().join(path))
            .unwrap_or_else(|error| panic!("{}: read {path}: {error}", case.name));
        let expected = expected.as_bytes();
        assert!(
            actual.as_slice() == expected,
            "{}: {path} content mismatch\nactual: {:?}\nexpected: {:?}",
            case.name,
            String::from_utf8_lossy(&actual),
            String::from_utf8_lossy(expected)
        );
    }
    for path in &case.absent {
        assert!(
            !temp.path().join(path).exists(),
            "{}: {path} should have been deleted",
            case.name
        );
    }
}

#[tokio::test]
async fn golden_fixtures_match_byte_for_byte() {
    let fixture = load_fixture();
    assert!(fixture.cases.len() >= 10);
    for case in &fixture.cases {
        run_fixture_case(case).await;
    }
}

#[tokio::test]
async fn stale_context_writes_nothing_across_files() {
    let temp = TempDir::new().expect("temp");
    fs::write(temp.path().join("a.txt"), "a\n").expect("a");
    fs::write(temp.path().join("b.txt"), "b\n").expect("b");
    let patch = "*** Begin Patch\n\
                 *** Update File: a.txt\n\
                 @@\n\
                 -a\n\
                 +A\n\
                 *** Update File: b.txt\n\
                 @@\n\
                 -stale\n\
                 +B\n\
                 *** End Patch\n";
    let error = execute(
        &definition(temp.path(), None),
        json!({ "patch": patch }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("stale hunk");
    assert!(
        error
            .message
            .contains("Failed to find expected lines in b.txt"),
        "{}",
        error.message
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("a.txt")).unwrap(),
        "a\n"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("b.txt")).unwrap(),
        "b\n"
    );
}

/// 极简 unified diff 重放（只用于断言 preview 与最终写入一致）。
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
        let target = if count == 0 { start } else { start - 1 };
        output.extend_from_slice(&original_lines[cursor..target]);
        cursor = target;
        while index < patch_lines.len() && !patch_lines[index].starts_with("@@") {
            let text_line = patch_lines[index];
            index += 1;
            if text_line.is_empty() {
                continue;
            }
            match text_line.as_bytes()[0] {
                b' ' => {
                    output.push(&text_line[1..]);
                    cursor += 1;
                }
                b'-' => cursor += 1,
                b'+' => output.push(&text_line[1..]),
                _ => {}
            }
        }
    }
    output.extend_from_slice(&original_lines[cursor..]);
    output.join("\n")
}

#[tokio::test]
async fn diff_preview_matches_the_final_write() {
    let temp = TempDir::new().expect("temp");
    let original = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\n";
    fs::write(temp.path().join("preview.txt"), original).expect("write");
    let patch =
        "*** Begin Patch\n*** Update File: preview.txt\n@@\n-line 3\n+LINE 3\n*** End Patch\n";
    let result = execute(
        &definition(temp.path(), None),
        json!({ "patch": patch }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("apply");
    let diff = result.details["files"][0]["diff"]
        .as_str()
        .expect("file diff");
    let replayed = apply_unified_patch(original, diff);
    let final_content = fs::read_to_string(temp.path().join("preview.txt")).expect("read");
    assert_eq!(replayed, final_content);
    assert_eq!(
        result.details["files"][0]["newContent"],
        json!(final_content)
    );
}

struct FailingOperations {
    files: Mutex<BTreeMap<PathBuf, String>>,
    fail_write: PathBuf,
    fail_create_dir: Option<PathBuf>,
}

#[async_trait]
impl ApplyPatchOperations for FailingOperations {
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, ToolOperationError> {
        let files = self.files.lock().expect("lock");
        match files.get(path) {
            Some(contents) => Ok(contents.as_bytes().to_vec()),
            None => Err(ToolOperationError::with_code(
                "ENOENT",
                format!(
                    "ENOENT: no such file or directory, open '{}'",
                    path.display()
                ),
            )),
        }
    }

    async fn write_file(&self, path: &Path, contents: &[u8]) -> Result<(), ToolOperationError> {
        if path == self.fail_write {
            return Err(ToolOperationError::new("disk full"));
        }
        self.files.lock().expect("lock").insert(
            path.to_path_buf(),
            String::from_utf8_lossy(contents).into_owned(),
        );
        Ok(())
    }

    async fn remove_file(&self, path: &Path) -> Result<(), ToolOperationError> {
        self.files.lock().expect("lock").remove(path);
        Ok(())
    }

    async fn create_dir_all(&self, path: &Path) -> Result<(), ToolOperationError> {
        if self.fail_create_dir.as_deref() == Some(path) {
            return Err(ToolOperationError::new("not a directory"));
        }
        Ok(())
    }

    async fn metadata(&self, path: &Path) -> Result<ApplyPatchMetadata, ToolOperationError> {
        let exists = self.files.lock().expect("lock").contains_key(path);
        Ok(ApplyPatchMetadata {
            exists,
            is_file: exists,
        })
    }
}

#[tokio::test]
async fn failed_write_rolls_back_earlier_files() {
    let temp = TempDir::new().expect("temp");
    let a = temp.path().join("a.txt");
    let b = temp.path().join("b.txt");
    let mut files = BTreeMap::new();
    files.insert(a.clone(), "a\n".to_string());
    files.insert(b.clone(), "b\n".to_string());
    let operations = Arc::new(FailingOperations {
        files: Mutex::new(files),
        fail_write: b.clone(),
        fail_create_dir: None,
    });
    let patch = "*** Begin Patch\n\
                 *** Update File: a.txt\n\
                 @@\n\
                 -a\n\
                 +A\n\
                 *** Update File: b.txt\n\
                 @@\n\
                 -b\n\
                 +B\n\
                 *** End Patch\n";
    let error = execute(
        &definition(
            temp.path(),
            Some(ApplyPatchToolOptions {
                operations: Some(operations.clone()),
                ..ApplyPatchToolOptions::default()
            }),
        ),
        json!({ "patch": patch }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("write failure");
    assert!(error.message.contains("disk full"), "{}", error.message);
    let files = operations.files.lock().expect("lock");
    assert_eq!(files.get(&a).map(String::as_str), Some("a\n"));
    assert_eq!(files.get(&b).map(String::as_str), Some("b\n"));
}

#[tokio::test]
async fn rollback_failures_are_reported_instead_of_claiming_atomicity() {
    let temp = TempDir::new().expect("temp");
    let a = temp.path().join("a.txt");
    let b = temp.path().join("b.txt");
    let mut files = BTreeMap::new();
    files.insert(a.clone(), "a\n".to_string());
    files.insert(b, "b\n".to_string());
    let operations = Arc::new(FailingOperations {
        files: Mutex::new(files),
        // 写入 a 失败后回滚也会尝试写 a，并再次失败。
        fail_write: a.clone(),
        fail_create_dir: None,
    });
    let patch = "*** Begin Patch\n\
                 *** Update File: a.txt\n\
                 @@\n\
                 -a\n\
                 +A\n\
                 *** Update File: b.txt\n\
                 @@\n\
                 -b\n\
                 +B\n\
                 *** End Patch\n";
    let error = execute(
        &definition(
            temp.path(),
            Some(ApplyPatchToolOptions {
                operations: Some(operations.clone()),
                ..ApplyPatchToolOptions::default()
            }),
        ),
        json!({ "patch": patch }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("write failure");
    assert!(
        error.message.contains("rollback failed"),
        "回滚失败必须出现在错误里: {}",
        error.message
    );
}

#[tokio::test]
async fn failed_directory_creation_rolls_back_earlier_files() {
    let temp = TempDir::new().expect("temp");
    let a = temp.path().join("a.txt");
    let nested = temp.path().join("new-dir");
    let b = nested.join("b.txt");
    let mut files = BTreeMap::new();
    files.insert(a.clone(), "a\n".to_string());
    let operations = Arc::new(FailingOperations {
        files: Mutex::new(files),
        fail_write: PathBuf::from("/unused"),
        fail_create_dir: Some(nested.clone()),
    });
    let patch = "*** Begin Patch\n\
                 *** Update File: a.txt\n\
                 @@\n\
                 -a\n\
                 +A\n\
                 *** Add File: new-dir/b.txt\n\
                 +b\n\
                 *** End Patch\n";
    let error = execute(
        &definition(
            temp.path(),
            Some(ApplyPatchToolOptions {
                operations: Some(operations.clone()),
                ..ApplyPatchToolOptions::default()
            }),
        ),
        json!({ "patch": patch }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("directory creation failure");
    assert!(
        error.message.contains("not a directory"),
        "{}",
        error.message
    );
    let files = operations.files.lock().expect("lock");
    assert_eq!(
        files.get(&a).map(String::as_str),
        Some("a\n"),
        "目录创建失败时已写入的文件必须回滚"
    );
    assert!(!files.contains_key(&b));
}

#[tokio::test]
async fn adding_an_existing_file_is_rejected_without_touching_it() {
    let temp = TempDir::new().expect("temp");
    let path = temp.path().join("notes.txt");
    fs::write(&path, "original\n").expect("write");
    let patch = "*** Begin Patch\n*** Add File: notes.txt\n+replacement\n*** End Patch\n";
    let error = execute(
        &definition(temp.path(), None),
        json!({ "patch": patch }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("adding an existing file must fail");
    assert!(
        error.message.contains("already exists"),
        "{}",
        error.message
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), "original\n");
}

/// Move 覆盖已存在的目标文件是允许的（与 codex 一致），但被覆盖的旧内容必须出现在
/// diff 里，模型/用户不能只看到“干净的 move”。
#[tokio::test]
async fn moving_over_an_existing_file_reports_the_overwritten_content() {
    let temp = TempDir::new().expect("temp");
    fs::write(temp.path().join("a.txt"), "old source line\n").expect("a");
    fs::write(temp.path().join("b.txt"), "old destination line\n").expect("b");
    let patch = "*** Begin Patch\n\
                 *** Update File: a.txt\n\
                 *** Move to: b.txt\n\
                 @@\n\
                 -old source line\n\
                 +new source line\n\
                 *** End Patch\n";
    let result = execute(
        &definition(temp.path(), None),
        json!({ "patch": patch }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("move overwrites");
    let details = serde_json::to_string(&result.details).expect("details");
    assert!(
        details.contains("-old destination line"),
        "overwritten destination content must be visible: {details}"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("b.txt")).expect("b"),
        "new source line\n"
    );
    assert!(!temp.path().join("a.txt").exists());
}

#[tokio::test]
async fn duplicate_add_file_hunks_in_one_patch_are_rejected() {
    let temp = TempDir::new().expect("temp");
    let patch = "*** Begin Patch\n\
                 *** Add File: dup.txt\n\
                 +first\n\
                 *** Add File: dup.txt\n\
                 +second\n\
                 *** End Patch\n";
    let error = execute(
        &definition(temp.path(), None),
        json!({ "patch": patch }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("the second Add must not silently overwrite the first");
    assert!(
        error.message.contains("already exists"),
        "{}",
        error.message
    );
    assert!(!temp.path().join("dup.txt").exists());
}

#[tokio::test]
async fn add_file_conflicting_with_an_earlier_move_is_rejected() {
    let temp = TempDir::new().expect("temp");
    fs::write(temp.path().join("a.txt"), "a\n").expect("write");
    let patch = "*** Begin Patch\n\
                 *** Update File: a.txt\n\
                 *** Move to: b.txt\n\
                 @@\n\
                 -a\n\
                 +A\n\
                 *** Add File: b.txt\n\
                 +clobber\n\
                 *** End Patch\n";
    let error = execute(
        &definition(temp.path(), None),
        json!({ "patch": patch }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("Add must not clobber the move destination");
    assert!(
        error.message.contains("already exists"),
        "{}",
        error.message
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("a.txt")).expect("a"),
        "a\n"
    );
    assert!(!temp.path().join("b.txt").exists());
}

#[tokio::test]
async fn deleting_a_missing_file_is_an_error_and_writes_nothing() {
    let temp = TempDir::new().expect("temp");
    fs::write(temp.path().join("keep.txt"), "keep\n").expect("write");
    let patch = "*** Begin Patch\n*** Delete File: missing.txt\n*** End Patch\n";
    let error = execute(
        &definition(temp.path(), None),
        json!({ "patch": patch }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("missing delete target");
    assert!(
        error
            .message
            .contains("Delete File Error: missing.txt does not exist"),
        "{}",
        error.message
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("keep.txt")).unwrap(),
        "keep\n"
    );
}

#[tokio::test]
async fn strict_mode_rejects_heredoc_wrappers() {
    let temp = TempDir::new().expect("temp");
    let patch = "<<'EOF'\n*** Begin Patch\n*** Add File: f.txt\n+x\n*** End Patch\nEOF\n";
    let temp_strict = TempDir::new().expect("temp");
    let strict_definition = create_apply_patch_tool_definition(
        temp_strict.path().to_string_lossy(),
        Some(ApplyPatchToolOptions {
            parse_mode: Some(kiana_tools::streaming_parser::ParseMode::Strict),
            ..ApplyPatchToolOptions::default()
        }),
    );
    let error = execute(
        &strict_definition,
        json!({ "patch": patch }),
        &TestContext::new(temp_strict.path()),
    )
    .await
    .expect_err("strict rejects heredoc");
    assert!(
        error
            .message
            .contains("The first line of the patch must be"),
        "{}",
        error.message
    );
    let okay = execute(
        &definition(temp.path(), None),
        json!({ "patch": patch }),
        &TestContext::new(temp.path()),
    )
    .await;
    assert!(okay.is_ok());
}

#[test]
fn prepare_arguments_accepts_input_and_array_forms() {
    let mut raw = json!("*** Begin Patch\n*** End Patch");
    let prepared = prepare_apply_patch_arguments(&mut raw).expect("raw string");
    assert_eq!(
        prepared,
        json!({ "patch": "*** Begin Patch\n*** End Patch" })
    );

    let mut input = json!({ "input": "*** Begin Patch\n*** End Patch" });
    let prepared = prepare_apply_patch_arguments(&mut input).expect("input key");
    assert_eq!(
        prepared,
        json!({ "patch": "*** Begin Patch\n*** End Patch" })
    );

    let mut array = json!({ "patch": ["*** Begin Patch", "*** End Patch"] });
    let prepared = prepare_apply_patch_arguments(&mut array).expect("array");
    assert_eq!(
        prepared,
        json!({ "patch": "*** Begin Patch\n*** End Patch" })
    );

    let mut canonical = json!({ "patch": "x" });
    assert!(prepare_apply_patch_arguments(&mut canonical).is_none());
}

#[test]
fn schema_is_an_object_requiring_patch() {
    let schema = apply_patch_parameters_schema();
    assert_eq!(schema["type"], json!("object"));
    assert_eq!(schema["required"][0], json!("patch"));
}

#[test]
fn preview_parses_without_touching_files() {
    let hunks = preview_patch(
        "*** Begin Patch\n*** Add File: f\n+x\n*** End Patch",
        kiana_tools::streaming_parser::ParseMode::Lenient,
    )
    .expect("preview");
    assert_eq!(hunks.len(), 1);
}

// ---------------------------------------------------------------------------
// 模型路由
// ---------------------------------------------------------------------------

#[test]
fn apply_patch_only_routes_to_configured_models() {
    let settings = ApplyPatchSettings {
        models: vec!["gpt-5*".to_string(), "codex*".to_string()],
    };
    assert_eq!(
        edit_tool_name_for_model(Some("gpt-5.2-codex"), &settings),
        "apply_patch"
    );
    assert_eq!(
        edit_tool_name_for_model(Some("codex-mini"), &settings),
        "apply_patch"
    );
    assert_eq!(
        edit_tool_name_for_model(Some("claude-opus-4"), &settings),
        "edit"
    );
    assert_eq!(edit_tool_name_for_model(None, &settings), "edit");
    assert!(!should_expose_apply_patch(None, &settings));
    assert!(!should_expose_apply_patch(
        Some("claude-opus-4"),
        &ApplyPatchSettings::default()
    ));
}

#[test]
fn coding_tool_set_swaps_edit_for_apply_patch_per_model() {
    let settings = ApplyPatchSettings {
        models: vec!["gpt-5*".to_string()],
    };
    let codex = create_coding_tool_definitions_for_model(
        "/tmp",
        None,
        Some("gpt-5.1-codex"),
        &settings,
        None,
    );
    let names: Vec<&str> = codex
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    assert_eq!(names, ["read", "bash", "apply_patch", "write"]);

    let claude = create_coding_tool_definitions_for_model(
        "/tmp",
        None,
        Some("claude-opus-4"),
        &settings,
        None,
    );
    let names: Vec<&str> = claude
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    assert_eq!(names, ["read", "bash", "edit", "write"]);
}

#[test]
fn all_tool_set_keeps_order_when_routing_apply_patch() {
    let settings = ApplyPatchSettings {
        models: vec!["gpt-5*".to_string()],
    };
    let definitions =
        create_all_tool_definitions_for_model("/tmp", None, Some("gpt-5.2"), &settings, None);
    let names: Vec<&str> = definitions
        .values()
        .map(|definition| definition.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "read",
            "bash",
            "powershell",
            "apply_patch",
            "write",
            "grep",
            "find",
            "ls"
        ]
    );
}
