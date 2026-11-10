//! `core/tools/find.ts` 的移植测试。
//!
//! TS 参考：`test/tools.test.ts` 的 "find tool" 与 "tool cwd resolution" 段、
//! `test/suite/regressions/6104-find-root-relativization.test.ts`（表驱动）、
//! `3302-find-path-glob.test.ts` / `3303-find-nested-gitignore.test.ts` 的 argv 形状
//! （真实 `fd` 的 gitignore 行为由外部二进制的黄金用例覆盖）。
//! 黄金文件由 TS 参考生成（`tests/fixtures/search-shell/behavior.golden.json`）。

mod search_shell_test_support;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use kiana_ai::types::JsonValue;
use kiana_ext_api::tool_types::ToolDefinition;
use kiana_tools::find::{
    FindGlobOptions, FindOperations, FindToolOptions, POSIX_PATH, WINDOWS_PATH,
    create_find_tool_definition, find_tool_system_prompt_contribution, relativize_find_result_path,
};
use kiana_tools::read::ToolOperationError;
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use search_shell_test_support::{
    TestContext, execute, execute_with_signal, golden, text_output, with_fake_tool,
};

/// 假的 `fd`：`$last` 是工具传入的搜索目录。
const FAKE_FD: &str = r#"#!/bin/sh
if [ -n "$FAKE_FD_ARGV_FILE" ]; then
  printf '%s\n' "$@" > "$FAKE_FD_ARGV_FILE"
fi
mode="${FAKE_FD_MODE:-files}"
case "$mode" in
  error) echo "fake fd failure" >&2; exit 2 ;;
  partial) echo "alpha.txt"; echo "fake fd failure" >&2; exit 2 ;;
  non-utf8)
    # 非 UTF-8 文件名（Linux 上合法）：fd 原样打印路径字节。后面跟足够多的输出，
    # 任何提前停止读取的实现都会让 fd 阻塞在写端（管道容量约 64 KiB）。
    printf 'bad\377.txt\n'
    i=1
    while [ "$i" -le 10000 ]; do
      printf 'ok-%05d.txt\n' "$i"
      i=$((i+1))
    done
    exit 0 ;;
  big-lines)
    i=1
    while [ "$i" -le 10 ]; do
      printf '%s-%d.txt\n' "$(printf 'z%.0s' $(seq 1 6000))" "$i"
      i=$((i+1))
    done
    exit 0 ;;
esac
if [ -n "$FAKE_FD_OUTPUT" ]; then
  printf '%s\n' "$FAKE_FD_OUTPUT"
  exit "${FAKE_FD_EXIT:-0}"
fi
printf '%s\n' "alpha.txt" "beta.txt"
"#;

fn find_definition(
    cwd: impl Into<String>,
    operations: Option<Arc<dyn FindOperations>>,
) -> ToolDefinition {
    create_find_tool_definition(cwd, Some(FindToolOptions { operations }))
}

fn details(result: &kiana_agent_core::AgentToolResult<JsonValue>) -> JsonValue {
    result.details.clone()
}

fn argv(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("argv file")
        .lines()
        .map(str::to_string)
        .collect()
}

/// issue #6104 的表驱动用例（`node:path` 的 posix/win32 两个实现）。
#[test]
fn relativizes_root_search_paths_like_ts() {
    let win32 =
        |result: &str, search: &str| relativize_find_result_path(result, search, &WINDOWS_PATH);
    let posix =
        |result: &str, search: &str| relativize_find_result_path(result, search, &POSIX_PATH);

    // Windows drive root。
    assert_eq!(
        win32("I:\\AI\\Models\\TextGen\\gemma4\\", "I:\\"),
        "AI/Models/TextGen/gemma4/"
    );
    assert_eq!(
        win32("I:/AI/Models/TextGen/gemma4/", "I:\\"),
        "AI/Models/TextGen/gemma4/"
    );
    assert_eq!(win32("I:\\AI\\Models\\", "I:\\AI"), "Models/");
    assert_eq!(
        win32("I:\\AI\\Models2\\file.txt", "I:\\AI\\Models"),
        "../Models2/file.txt"
    );
    assert_eq!(
        win32("AI\\Models\\TextGen\\gemma4\\", "I:\\"),
        "AI/Models/TextGen/gemma4/"
    );

    // POSIX root。
    assert_eq!(posix("/home/user/file.txt", "/"), "home/user/file.txt");
    assert_eq!(posix("/home/user/project/", "/"), "home/user/project/");
    assert_eq!(posix("/home/user/file\\", "/home/user"), "file\\");

    // 搜索路径之外的绝对结果回落到 path.relative。
    assert_eq!(
        posix("/tmp/results/file.txt", "/workspace/project"),
        "../../tmp/results/file.txt"
    );
    assert_eq!(
        posix("/tmp/results/dir/", "/workspace/project"),
        "../../tmp/results/dir/"
    );
}

/// 回归：win32 UNC 路径的 root 是 `\\server`（Node 的 `win32.relative` 去掉前导
/// `\\` 后按组件比较）。不同 server 是不同 root，返回绝对目标；同一 server 的不同
/// share 仍按段计算相对路径。黄金文件由 `node:path.win32` 逐字生成。
#[test]
fn win32_unc_relativization_matches_node() {
    let fixture: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("fixtures/search-shell/win32-relativize.json"))
            .expect("valid win32 fixture");
    assert!(fixture.len() > 400);
    for case in fixture {
        let result_path = case["result"].as_str().expect("result");
        let search_path = case["search"].as_str().expect("search");
        let expected = case["expected"].as_str().expect("expected");
        assert_eq!(
            relativize_find_result_path(result_path, search_path, &WINDOWS_PATH),
            expected,
            "relativize({result_path:?}, {search_path:?})"
        );
    }
}

#[derive(Clone, Default)]
struct FixedOps {
    paths: Vec<String>,
    exists: bool,
    glob_error: Option<String>,
}

#[async_trait]
impl FindOperations for FixedOps {
    async fn exists(&self, _absolute_path: &Path) -> Result<bool, ToolOperationError> {
        Ok(self.exists)
    }

    async fn glob(
        &self,
        _pattern: &str,
        _cwd: &Path,
        _options: FindGlobOptions,
    ) -> Result<Vec<String>, ToolOperationError> {
        match &self.glob_error {
            Some(message) => Err(ToolOperationError {
                message: message.clone(),
                code: None,
            }),
            None => Ok(self.paths.clone()),
        }
    }
}

#[tokio::test]
async fn relativizes_custom_glob_results_against_a_root_search_path() {
    let definition = find_definition(
        "/",
        Some(Arc::new(FixedOps {
            exists: true,
            paths: vec![
                "/home/user/project/".to_string(),
                "/home/user/project/file.txt".to_string(),
            ],
            glob_error: None,
        })),
    );
    // TS 的回归测试传 `{} as ctx`；Rust 用空 cwd 的上下文等价。
    let result = execute(
        &definition,
        json!({ "pattern": "**" }),
        &TestContext::new(""),
    )
    .await
    .expect("execute");
    let expected = &golden()["find_custom_glob_root"];
    assert_eq!(text_output(&result), expected["text"].as_str().unwrap());
    assert_eq!(details(&result), expected["details"]);
}

#[tokio::test]
async fn custom_operations_limit_notice_has_no_hint() {
    let temp = TempDir::new().expect("temp");
    let definition = find_definition(
        temp.path().to_string_lossy().into_owned(),
        Some(Arc::new(FixedOps {
            exists: true,
            paths: vec![
                "a.txt".to_string(),
                "b.txt".to_string(),
                "c.txt".to_string(),
            ],
            glob_error: None,
        })),
    );
    let result = execute(
        &definition,
        json!({ "pattern": "*.txt", "limit": 2 }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("execute");
    let expected = &golden()["find_custom_limit"];
    assert_eq!(text_output(&result), expected["text"].as_str().unwrap());
    assert_eq!(details(&result), expected["details"]);
    assert!(!text_output(&result).contains("Use limit="));
}

#[tokio::test]
async fn custom_operations_report_path_not_found() {
    let temp = TempDir::new().expect("temp");
    let definition = find_definition(
        temp.path().to_string_lossy().into_owned(),
        Some(Arc::new(FixedOps {
            exists: false,
            ..FixedOps::default()
        })),
    );
    let missing = temp.path().join("missing");
    let error = execute(
        &definition,
        json!({ "pattern": "*.txt", "path": missing.to_string_lossy() }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("must fail");
    assert_eq!(
        error.message,
        format!("Path not found: {}", missing.to_string_lossy())
    );
}

#[tokio::test]
async fn custom_operations_errors_propagate() {
    let temp = TempDir::new().expect("temp");
    let definition = find_definition(
        temp.path().to_string_lossy().into_owned(),
        Some(Arc::new(FixedOps {
            exists: true,
            glob_error: Some("glob exploded".to_string()),
            ..FixedOps::default()
        })),
    );
    let error = execute(
        &definition,
        json!({ "pattern": "*.txt" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("must fail");
    assert_eq!(error.message, "glob exploded");
}

#[tokio::test]
async fn custom_operations_reject_abort_at_each_checkpoint() {
    let temp = TempDir::new().expect("temp");
    let definition = find_definition(
        temp.path().to_string_lossy().into_owned(),
        Some(Arc::new(FixedOps {
            exists: true,
            ..FixedOps::default()
        })),
    );
    let signal = CancellationToken::new();
    signal.cancel();
    let error = execute_with_signal(
        &definition,
        json!({ "pattern": "*.txt" }),
        &signal,
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("must abort");
    assert_eq!(error.message, "Operation aborted");
}

#[tokio::test]
async fn fd_branch_relativizes_absolute_output() {
    let temp = TempDir::new().expect("temp");
    let definition = find_definition(temp.path().to_string_lossy().into_owned(), None);
    let output = format!(
        "{}/alpha.txt\n{}/beta.txt",
        temp.path().to_string_lossy(),
        temp.path().to_string_lossy()
    );
    let result = with_fake_tool(
        "fd",
        FAKE_FD,
        &[
            ("FAKE_FD_MODE", "files".to_string()),
            ("FAKE_FD_OUTPUT", output),
        ],
        || async {
            execute(
                &definition,
                json!({ "pattern": "*.txt" }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let expected = &golden()["find_fd"];
    assert_eq!(text_output(&result), expected["text"].as_str().unwrap());
    assert_eq!(details(&result), expected["details"]);
}

#[tokio::test]
async fn fd_branch_reports_the_limit_with_a_hint() {
    let temp = TempDir::new().expect("temp");
    let definition = find_definition(temp.path().to_string_lossy().into_owned(), None);
    let output = format!(
        "{0}/alpha.txt\n{0}/beta.txt\n{0}/gamma.txt",
        temp.path().to_string_lossy()
    );
    let result = with_fake_tool(
        "fd",
        FAKE_FD,
        &[
            ("FAKE_FD_MODE", "files".to_string()),
            ("FAKE_FD_OUTPUT", output),
        ],
        || async {
            execute(
                &definition,
                json!({ "pattern": "*.txt", "limit": 2 }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let expected = &golden()["find_fd_limit"];
    assert_eq!(text_output(&result), expected["text"].as_str().unwrap());
    assert_eq!(details(&result), expected["details"]);
}

#[tokio::test]
async fn fd_argv_uses_full_path_and_double_star_for_slash_patterns() {
    let temp = TempDir::new().expect("temp");
    let search = temp.path().join("project");
    std::fs::create_dir_all(&search).expect("mkdir");
    let argv_file = temp.path().join("argv.txt");
    let definition = find_definition(search.to_string_lossy().into_owned(), None);
    with_fake_tool(
        "fd",
        FAKE_FD,
        &[(
            "FAKE_FD_ARGV_FILE",
            argv_file.to_string_lossy().into_owned(),
        )],
        || async {
            execute(
                &definition,
                json!({ "pattern": "src/**/*.spec.ts" }),
                &TestContext::new(&search),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let mut expected: Vec<String> = [
        "--glob",
        "--color=never",
        "--hidden",
        "--no-require-git",
        "--max-results",
        "1000",
        "--full-path",
        "--",
        "**/src/**/*.spec.ts",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    expected.push(search.to_string_lossy().into_owned());
    assert_eq!(argv(&argv_file), expected);
}

#[tokio::test]
async fn fd_argv_skips_full_path_for_basename_patterns() {
    let temp = TempDir::new().expect("temp");
    let argv_file = temp.path().join("argv.txt");
    let definition = find_definition(temp.path().to_string_lossy().into_owned(), None);
    with_fake_tool(
        "fd",
        FAKE_FD,
        &[(
            "FAKE_FD_ARGV_FILE",
            argv_file.to_string_lossy().into_owned(),
        )],
        || async {
            execute(
                &definition,
                json!({ "pattern": "*.txt", "limit": 2.5 }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let mut expected: Vec<String> = [
        "--glob",
        "--color=never",
        "--hidden",
        "--no-require-git",
        "--max-results",
        "2.5",
        "--",
        "*.txt",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    expected.push(temp.path().to_string_lossy().into_owned());
    assert_eq!(argv(&argv_file), expected);
}

#[tokio::test]
async fn fd_argv_drops_no_require_git_inside_a_git_repository() {
    let temp = TempDir::new().expect("temp");
    std::fs::create_dir_all(temp.path().join(".git")).expect("mkdir .git");
    let argv_file = temp.path().join("argv.txt");
    let definition = find_definition(temp.path().to_string_lossy().into_owned(), None);
    with_fake_tool(
        "fd",
        FAKE_FD,
        &[(
            "FAKE_FD_ARGV_FILE",
            argv_file.to_string_lossy().into_owned(),
        )],
        || async {
            execute(
                &definition,
                json!({ "pattern": "*.txt" }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let argv = argv(&argv_file);
    assert!(!argv.iter().any(|arg| arg == "--no-require-git"));
    assert_eq!(&argv[..3], ["--glob", "--color=never", "--hidden"]);
}

#[tokio::test]
async fn fd_error_without_output_is_an_error() {
    let temp = TempDir::new().expect("temp");
    let definition = find_definition(temp.path().to_string_lossy().into_owned(), None);
    let error = with_fake_tool(
        "fd",
        FAKE_FD,
        &[("FAKE_FD_MODE", "error".to_string())],
        || async {
            execute(
                &definition,
                json!({ "pattern": "[" }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect_err("must fail")
        },
    )
    .await;
    assert_eq!(error.message, "fake fd failure");
}

#[tokio::test]
async fn fd_error_with_output_keeps_the_results() {
    let temp = TempDir::new().expect("temp");
    let definition = find_definition(temp.path().to_string_lossy().into_owned(), None);
    let result = with_fake_tool(
        "fd",
        FAKE_FD,
        &[("FAKE_FD_MODE", "partial".to_string())],
        || async {
            execute(
                &definition,
                json!({ "pattern": "*.txt" }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    assert_eq!(text_output(&result), "alpha.txt");
}

/// 回归：`fd` 会原样打印非 UTF-8 文件名（Linux 上合法）。TS 的 readline 用 `StringDecoder`
/// 把非法字节替换为 U+FFFD 后继续；`BufReader::lines()` 会在第一行直接报错，若据此停止读取，
/// 后续结果丢失，且 `fd` 写满管道后 `child.wait()` 永久阻塞。
#[tokio::test]
async fn fd_output_with_invalid_utf8_is_decoded_lossily_and_fully_read() {
    let temp = TempDir::new().expect("temp");
    let definition = find_definition(temp.path().to_string_lossy().into_owned(), None);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        with_fake_tool(
            "fd",
            FAKE_FD,
            &[("FAKE_FD_MODE", "non-utf8".to_string())],
            || async {
                execute(
                    &definition,
                    json!({ "pattern": "*.txt" }),
                    &TestContext::new(temp.path()),
                )
                .await
            },
        ),
    )
    .await
    .expect("find must not deadlock on non-UTF-8 output")
    .expect("execute");

    let output = text_output(&result);
    assert!(
        output.starts_with("bad\u{FFFD}.txt\nok-00001.txt\n"),
        "output: {}",
        &output[..output.len().min(120)]
    );
    assert!(
        output.contains("50.0KB limit reached"),
        "len={} tail={:?}",
        output.len(),
        &output[output.len().saturating_sub(200)..]
    );
}

/// 回归：details 的键序须与 TS 的赋值顺序一致（`resultLimitReached` → `truncation`），
/// 而不是 `FindToolDetails` 的结构体字段序。
#[tokio::test]
async fn details_key_order_matches_ts_when_limit_and_truncation_both_hit() {
    let temp = TempDir::new().expect("temp");
    let definition = find_definition(temp.path().to_string_lossy().into_owned(), None);
    let result = with_fake_tool(
        "fd",
        FAKE_FD,
        &[("FAKE_FD_MODE", "big-lines".to_string())],
        || async {
            execute(
                &definition,
                json!({ "pattern": "*.txt", "limit": 5 }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let details = result.details.as_object().expect("details object");
    assert_eq!(
        details.keys().map(String::as_str).collect::<Vec<_>>(),
        ["resultLimitReached", "truncation"]
    );
    assert_eq!(details["resultLimitReached"], json!(5));
}

/// 自定义操作还 pending 时中止必须立即 reject（TS 在 execute 开头就注册 onAbort）。
struct SlowOps;

#[async_trait]
impl FindOperations for SlowOps {
    async fn exists(&self, _absolute_path: &Path) -> Result<bool, ToolOperationError> {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        Ok(true)
    }

    async fn glob(
        &self,
        _pattern: &str,
        _cwd: &Path,
        _options: FindGlobOptions,
    ) -> Result<Vec<String>, ToolOperationError> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn abort_interrupts_pending_custom_operations() {
    let temp = TempDir::new().expect("temp");
    let definition = find_definition(
        temp.path().to_string_lossy().into_owned(),
        Some(Arc::new(SlowOps)),
    );
    let signal = CancellationToken::new();
    let canceller = signal.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        canceller.cancel();
    });
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        execute_with_signal(
            &definition,
            json!({ "pattern": "*.txt" }),
            &signal,
            &TestContext::new(temp.path()),
        ),
    )
    .await
    .expect("abort must be observed while exists() is pending")
    .expect_err("must abort");
    assert_eq!(error.message, "Operation aborted");
}

#[tokio::test]
async fn uses_ctx_cwd_when_provided() {
    let temp = TempDir::new().expect("temp");
    let argv_file = temp.path().join("argv.txt");
    let definition = find_definition("/", None);
    with_fake_tool(
        "fd",
        FAKE_FD,
        &[(
            "FAKE_FD_ARGV_FILE",
            argv_file.to_string_lossy().into_owned(),
        )],
        || async {
            execute(
                &definition,
                json!({ "pattern": "ctx-cwd-find.txt" }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let argv = argv(&argv_file);
    assert_eq!(
        argv.last().map(String::as_str),
        Some(temp.path().to_string_lossy().as_ref())
    );
    // 默认输出（alpha.txt/beta.txt）相对搜索目录展示。
    assert_eq!(argv[argv.len() - 3], "--");
}

#[test]
fn contribution_schema_and_definition_match_ts() {
    let contribution = find_tool_system_prompt_contribution();
    assert_eq!(
        contribution.snippet,
        "Find files by glob pattern (respects .gitignore)"
    );
    let definition = create_find_tool_definition("/", None);
    assert_eq!(definition.name, "find");
    assert_eq!(definition.label, "find");
    assert_eq!(
        definition.prompt_snippet.as_deref(),
        Some(contribution.snippet)
    );
    assert_eq!(definition.constrained_sampling, None);
    assert_eq!(definition.parameters["required"], json!(["pattern"]));
    // TS 的 find 定义没有 promptGuidelines 属性（`undefined`）；contribution 常量是空数组。
    assert_eq!(definition.prompt_guidelines, None);
}
