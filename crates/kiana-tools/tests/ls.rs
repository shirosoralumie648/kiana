//! `ls.ts` 的移植测试。
//!
//! 主体是 `tests/fixtures/file-tools/read_ls.golden.json` 里的 ls 黄金用例（由 TS 参考实现
//! 生成）：排序 / 目录后缀 / 隐藏文件、`(empty directory)`、条目数限制、`Path not found`、
//! `Not a directory`、500 条上限。另覆盖 50KB 字节截断、分数 limit 的 JS 数值语义、
//! 自定义操作、中途中止与定义形状。

mod file_tools_test_support;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use file_tools_test_support::{TestContext, execute, execute_with_signal, text_output};
use kiana_ai::types::JsonValue;
use kiana_tools::ls::{
    LocalLsOperations, LsOperations, LsToolOptions, create_ls_tool, create_ls_tool_definition,
    ls_parameters_schema, ls_tool_system_prompt_contribution,
};
use kiana_tools::read::ToolOperationError;
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn golden() -> JsonValue {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/file-tools/read_ls.golden.json"
    );
    serde_json::from_str(&fs::read_to_string(path).expect("read golden")).expect("parse golden")
}

/// 重建生成黄金文件时的目录树，返回（golden 根目录, 本地根目录）。
fn build_fixture_tree(temp: &Path) -> (String, String) {
    let golden = golden();
    let basic = golden["lsBasicDirName"].as_str().expect("lsBasicDirName");
    let original_case = golden["ls"]
        .as_array()
        .expect("ls cases")
        .iter()
        .find(|case| case["name"] == "basic")
        .expect("basic case");
    let original_basic = original_case["path"].as_str().expect("path");
    let original_root = original_basic
        .strip_suffix(&format!("/{basic}"))
        .expect("golden root")
        .to_string();

    let basic_dir = temp.join(basic);
    fs::create_dir(&basic_dir).expect("mkdir ls-basic");
    for name in [
        ".hidden-file",
        "Apple",
        "apple",
        "Banana",
        "zeta",
        "0num",
        "_under",
        "file.txt",
    ] {
        fs::write(basic_dir.join(name), "x").expect("write fixture file");
    }
    for name in [".hidden-dir", "subdir", "Zdir"] {
        fs::create_dir(basic_dir.join(name)).expect("mkdir fixture dir");
    }
    fs::create_dir(temp.join("ls-empty")).expect("mkdir ls-empty");
    let big = temp.join("ls-big");
    fs::create_dir(&big).expect("mkdir ls-big");
    for index in 0..700 {
        fs::write(
            big.join(format!("entry-{index:04}-{}", "y".repeat(60))),
            "x",
        )
        .expect("write big entry");
    }
    (original_root, temp.to_string_lossy().into_owned())
}

#[tokio::test]
async fn ls_matches_the_reference_goldens() {
    let golden = golden();
    let cases = golden["ls"].as_array().expect("ls cases");
    assert_eq!(cases.len(), 7);
    let temp = TempDir::new().expect("temp");
    let (original_root, local_root) = build_fixture_tree(temp.path());

    for case in cases {
        let name = case["name"].as_str().expect("name");
        let original_path = case["path"].as_str().expect("path");
        let local_path = original_path.replacen(&original_root, &local_root, 1);
        let mut params = json!({ "path": local_path });
        if let Some(limit) = case["limit"].as_f64() {
            params["limit"] = json!(limit);
        }
        let definition = create_ls_tool_definition(temp.path().to_string_lossy(), None);
        let result = execute(&definition, params, &TestContext::new(temp.path())).await;

        if let Some(expected) = case["error"].as_str() {
            let error = result.expect_err(name);
            assert_eq!(
                error.message,
                expected.replacen(&original_root, &local_root, 1),
                "error mismatch for {name}"
            );
            continue;
        }

        let result = result.unwrap_or_else(|error| panic!("{name}: {}", error.message));
        assert_eq!(
            text_output(&result),
            case["text"].as_str().expect("text"),
            "text mismatch for {name}"
        );
        assert_eq!(
            result.details, case["details"],
            "details mismatch for {name}"
        );
        assert_eq!(result.structured_content, None, "{name}");
    }
}

#[tokio::test]
async fn lists_dotfiles_and_directories() {
    let temp = TempDir::new().expect("temp");
    fs::write(temp.path().join(".hidden-file"), "secret").expect("write");
    fs::create_dir(temp.path().join(".hidden-dir")).expect("mkdir");
    let definition = create_ls_tool_definition(temp.path().to_string_lossy(), None);
    let result = execute(&definition, json!({}), &TestContext::new(temp.path()))
        .await
        .expect("ls");
    let output = text_output(&result);
    assert_eq!(output, ".hidden-dir/\n.hidden-file");
}

#[tokio::test]
async fn sorts_case_insensitively_and_appends_directory_slashes() {
    let temp = TempDir::new().expect("temp");
    for name in ["b", "A", "c", "a"] {
        fs::write(temp.path().join(name), "x").expect("write");
    }
    fs::create_dir(temp.path().join("B")).expect("mkdir");
    let definition = create_ls_tool_definition(temp.path().to_string_lossy(), None);
    let result = execute(&definition, json!({}), &TestContext::new(temp.path()))
        .await
        .expect("ls");
    assert_eq!(text_output(&result), "A\na\nB/\nb\nc");
    assert_eq!(result.details, JsonValue::Null);
}

#[tokio::test]
async fn empty_directory_text_is_verbatim() {
    let temp = TempDir::new().expect("temp");
    let empty = temp.path().join("empty");
    fs::create_dir(&empty).expect("mkdir");
    let definition = create_ls_tool_definition(temp.path().to_string_lossy(), None);
    let result = execute(
        &definition,
        json!({ "path": empty }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("ls");
    assert_eq!(text_output(&result), "(empty directory)");
    assert_eq!(result.details, JsonValue::Null);
}

#[tokio::test]
async fn fractional_limits_use_js_number_semantics() {
    let temp = TempDir::new().expect("temp");
    for index in 0..10 {
        fs::write(temp.path().join(format!("file-{index}")), "x").expect("write");
    }
    let definition = create_ls_tool_definition(temp.path().to_string_lossy(), None);
    let result = execute(
        &definition,
        json!({ "limit": 2.5 }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("ls");
    assert_eq!(
        text_output(&result),
        "file-0\nfile-1\nfile-2\n\n[2.5 entries limit reached. Use limit=5 for more]"
    );
    assert_eq!(result.details, json!({ "entryLimitReached": 2.5 }));
}

#[tokio::test]
async fn negative_and_zero_limits_produce_the_empty_directory_text() {
    let temp = TempDir::new().expect("temp");
    fs::write(temp.path().join("file.txt"), "x").expect("write");
    let definition = create_ls_tool_definition(temp.path().to_string_lossy(), None);
    for limit in [0.0, -1.0] {
        let result = execute(
            &definition,
            json!({ "limit": limit }),
            &TestContext::new(temp.path()),
        )
        .await
        .expect("ls");
        assert_eq!(text_output(&result), "(empty directory)");
        assert_eq!(result.details, JsonValue::Null);
    }
}

/// 名称很长、数量很多的目录：条目数上限未触发时先触发 50KB 截断。
#[tokio::test]
async fn byte_truncation_reports_the_50kb_limit() {
    let temp = TempDir::new().expect("temp");
    for index in 0..700 {
        fs::write(
            temp.path()
                .join(format!("entry-{index:04}-{}", "y".repeat(100))),
            "x",
        )
        .expect("write");
    }
    let definition = create_ls_tool_definition(temp.path().to_string_lossy(), None);
    let result = execute(
        &definition,
        json!({ "limit": 1000 }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("ls");
    let output = text_output(&result);
    assert!(output.ends_with("\n\n[50.0KB limit reached]"), "{output}");
    assert!(output.len() < 51 * 1024);
    let details = result.details.as_object().expect("details");
    assert_eq!(details["truncation"]["truncated"], json!(true));
    // TS 传 `maxLines: Number.MAX_SAFE_INTEGER`；它原样进 details（不是 usize::MAX）。
    assert_eq!(
        details["truncation"]["maxLines"],
        json!(9_007_199_254_740_991_i64)
    );
}

#[tokio::test]
async fn entry_limit_notice_precedes_the_truncation_notice() {
    let temp = TempDir::new().expect("temp");
    for index in 0..700 {
        fs::write(
            temp.path()
                .join(format!("entry-{index:04}-{}", "y".repeat(100))),
            "x",
        )
        .expect("write");
    }
    let definition = create_ls_tool_definition(temp.path().to_string_lossy(), None);
    let result = execute(&definition, json!({}), &TestContext::new(temp.path()))
        .await
        .expect("ls");
    let output = text_output(&result);
    assert!(
        output.ends_with(
            "\n\n[500 entries limit reached. Use limit=1000 for more. 50.0KB limit reached]"
        ),
        "{}",
        &output[output.len().saturating_sub(200)..]
    );
    // 键顺序：entryLimitReached 先插入，truncation 后插入。
    let serialized = serde_json::to_string(&result.details).expect("serialize");
    assert!(serialized.starts_with("{\"entryLimitReached\":500,\"truncation\":"));
}

#[tokio::test]
async fn path_not_found_and_not_a_directory_match_the_reference() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("file.txt");
    fs::write(&file, "x").expect("write");
    let definition = create_ls_tool_definition(temp.path().to_string_lossy(), None);
    let ctx = TestContext::new(temp.path());

    let missing = temp.path().join("nope");
    let error = execute(&definition, json!({ "path": missing }), &ctx)
        .await
        .expect_err("not found");
    assert_eq!(
        error.message,
        format!("Path not found: {}", missing.display())
    );

    let error = execute(&definition, json!({ "path": file }), &ctx)
        .await
        .expect_err("not a directory");
    assert_eq!(
        error.message,
        format!("Not a directory: {}", file.display())
    );
}

#[tokio::test]
async fn abbreviates_the_home_directory_shorthand() {
    // `resolve_to_cwd` 语义由 path_utils 的测试覆盖；这里只确认相对路径相对 cwd。
    let temp = TempDir::new().expect("temp");
    let nested = temp.path().join("nested");
    fs::create_dir(&nested).expect("mkdir");
    fs::write(nested.join("inner.txt"), "x").expect("write");
    let definition = create_ls_tool_definition(temp.path().to_string_lossy(), None);
    let result = execute(
        &definition,
        json!({ "path": "nested" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("ls");
    assert_eq!(text_output(&result), "inner.txt");
}

#[tokio::test]
async fn ls_uses_the_context_cwd() {
    let temp = TempDir::new().expect("temp");
    fs::write(temp.path().join("ctx-cwd-ls.txt"), "list me").expect("write");
    let definition = create_ls_tool_definition("/", None);
    let result = execute(&definition, json!({}), &TestContext::new(temp.path()))
        .await
        .expect("ls");
    assert_eq!(text_output(&result), "ctx-cwd-ls.txt");
}

/// 可编排的目录枚举操作。
struct ScriptedOperations {
    dir_path: std::path::PathBuf,
    entries: Vec<String>,
    stat_error: Option<String>,
}

#[async_trait]
impl LsOperations for ScriptedOperations {
    async fn exists(&self, _absolute_path: &Path) -> Result<bool, ToolOperationError> {
        Ok(true)
    }

    async fn stat_is_directory(&self, absolute_path: &Path) -> Result<bool, ToolOperationError> {
        if let Some(message) = &self.stat_error
            && absolute_path
                .file_name()
                .is_some_and(|name| name == "broken")
        {
            return Err(ToolOperationError::new(message.clone()));
        }
        Ok(absolute_path == self.dir_path
            || absolute_path.file_name().is_some_and(|name| name == "dir"))
    }

    async fn readdir(&self, _absolute_path: &Path) -> Result<Vec<String>, ToolOperationError> {
        Ok(self.entries.clone())
    }
}

#[tokio::test]
async fn custom_operations_control_entries_and_skipped_stats() {
    let temp = TempDir::new().expect("temp");
    let definition = create_ls_tool_definition(
        temp.path().to_string_lossy(),
        Some(LsToolOptions {
            operations: Some(Arc::new(ScriptedOperations {
                dir_path: temp.path().to_path_buf(),
                entries: vec![
                    "zeta".to_string(),
                    "broken".to_string(),
                    "alpha".to_string(),
                    "dir".to_string(),
                ],
                stat_error: Some("EIO: i/o error".to_string()),
            })),
        }),
    );
    let result = execute(&definition, json!({}), &TestContext::new(temp.path()))
        .await
        .expect("ls");
    assert_eq!(text_output(&result), "alpha\ndir/\nzeta");
}

#[tokio::test]
async fn readdir_errors_are_wrapped_verbatim() {
    struct FailingReaddir;

    #[async_trait]
    impl LsOperations for FailingReaddir {
        async fn exists(&self, _absolute_path: &Path) -> Result<bool, ToolOperationError> {
            Ok(true)
        }

        async fn stat_is_directory(
            &self,
            _absolute_path: &Path,
        ) -> Result<bool, ToolOperationError> {
            Ok(true)
        }

        async fn readdir(&self, _absolute_path: &Path) -> Result<Vec<String>, ToolOperationError> {
            Err(ToolOperationError::new("EACCES: permission denied"))
        }
    }

    let temp = TempDir::new().expect("temp");
    let definition = create_ls_tool_definition(
        temp.path().to_string_lossy(),
        Some(LsToolOptions {
            operations: Some(Arc::new(FailingReaddir)),
        }),
    );
    let error = execute(&definition, json!({}), &TestContext::new(temp.path()))
        .await
        .expect_err("readdir");
    assert_eq!(
        error.message,
        "Cannot read directory: EACCES: permission denied"
    );
}

#[tokio::test]
async fn pre_aborted_ls_rejects_with_operation_aborted() {
    let temp = TempDir::new().expect("temp");
    let definition = create_ls_tool_definition(temp.path().to_string_lossy(), None);
    let signal = CancellationToken::new();
    signal.cancel();
    let error = execute_with_signal(
        &definition,
        json!({}),
        &signal,
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("aborted");
    assert_eq!(error.message, "Operation aborted");
}

/// 在 `exists` 里挂起的操作。
struct HangingOperations {
    started: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl LsOperations for HangingOperations {
    async fn exists(&self, _absolute_path: &Path) -> Result<bool, ToolOperationError> {
        self.started.notify_waiters();
        std::future::pending::<()>().await;
        Ok(true)
    }

    async fn stat_is_directory(&self, _absolute_path: &Path) -> Result<bool, ToolOperationError> {
        Ok(true)
    }

    async fn readdir(&self, _absolute_path: &Path) -> Result<Vec<String>, ToolOperationError> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn abort_during_an_in_flight_ls_rejects_immediately() {
    let temp = TempDir::new().expect("temp");
    let started = Arc::new(tokio::sync::Notify::new());
    let definition = create_ls_tool_definition(
        temp.path().to_string_lossy(),
        Some(LsToolOptions {
            operations: Some(Arc::new(HangingOperations {
                started: Arc::clone(&started),
            })),
        }),
    );
    let ctx = TestContext::new(temp.path());
    let signal = CancellationToken::new();
    let waiting = started.notified();
    let task = tokio::spawn({
        let definition = definition.clone();
        let ctx = Arc::clone(&ctx);
        let signal = signal.clone();
        async move { execute_with_signal(&definition, json!({}), &signal, &ctx).await }
    });
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("exists started");
    signal.cancel();
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("abort resolves")
        .expect("join")
        .expect_err("aborted");
    assert_eq!(error.message, "Operation aborted");
}

#[test]
fn definition_shape_matches_the_reference() {
    let definition = create_ls_tool_definition("/workspace", None);
    assert_eq!(definition.name, "ls");
    assert_eq!(definition.label, "ls");
    assert_eq!(
        definition.description,
        "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first)."
    );
    let contribution = ls_tool_system_prompt_contribution();
    assert_eq!(
        definition.prompt_snippet.as_deref(),
        Some(contribution.snippet)
    );
    // TS 的 ls 定义没有 promptGuidelines 属性（`undefined`）；contribution 常量是空数组。
    assert_eq!(definition.prompt_guidelines, None);
    assert_eq!(definition.parameters, ls_parameters_schema());
    // TS 的 ls definition 没有 constrainedSampling。
    assert_eq!(definition.constrained_sampling, None);
    assert_eq!(definition.output_schema, None);
    assert!(definition.prepare_arguments.is_none());
    let _ = LocalLsOperations;
}

#[test]
fn parameters_schema_is_verbatim() {
    assert_eq!(
        ls_parameters_schema(),
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to list (default: current directory)" },
                "limit": { "type": "number", "description": "Maximum number of entries to return (default: 500)" }
            }
        })
    );
}

#[test]
fn agent_tool_projection() {
    let tool = create_ls_tool("/", None);
    assert_eq!(tool.name, "ls");
    assert_eq!(tool.label, "ls");
}
