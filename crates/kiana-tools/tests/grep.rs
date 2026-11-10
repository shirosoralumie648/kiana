//! `core/tools/grep.ts` 的移植测试。
//!
//! TS 参考：`test/tools.test.ts` 的 "grep tool" 段（单文件文件名、context+limit、
//! RIPGREP_CONFIG_PATH、flag-like pattern）与 "tool cwd resolution" 的 grep 用例。
//! 外部 `rg` 由 [`with_fake_tool`] 放进 `KIANA_CODING_AGENT_DIR/bin`；黄金文件由 TS
//! 参考生成（`tests/fixtures/search-shell/behavior.golden.json`）。

mod search_shell_test_support;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use kiana_ai::types::JsonValue;
use kiana_ext_api::tool_types::{GrepToolDetails, ToolDefinition};
use kiana_tools::grep::{
    GrepOperations, GrepToolOptions, LocalGrepOperations, create_grep_tool_definition,
    grep_tool_system_prompt_contribution,
};
use kiana_tools::read::ToolOperationError;
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use search_shell_test_support::{
    TestContext, execute, execute_with_signal, golden, text_output, with_fake_tool,
};

/// 假的 `rg`：`$last` 是工具传入的搜索路径（argv 以 `-- PATTERN PATH` 结尾）。
const FAKE_RG: &str = r#"#!/bin/sh
for last; do :; done
if [ -n "$FAKE_RG_ARGV_FILE" ]; then
  printf '%s\n' "$@" > "$FAKE_RG_ARGV_FILE"
fi
mode="${FAKE_RG_MODE:-match}"
path="${FAKE_RG_EVENT_PATH:-$last}"
case "$mode" in
  none) exit 1 ;;
  error) echo "fake rg failure" >&2; exit 2 ;;
  error-silent) exit 2 ;;
  argv) exit 1 ;;
  context)
    printf '{"type":"match","data":{"path":{"text":"%s"},"lines":{"text":"match one\\n"},"line_number":2,"absolute_offset":0,"submatches":[]}}\n' "$path"
    exit 0 ;;
  two)
    printf '{"type":"match","data":{"path":{"text":"%s"},"lines":{"text":"match one\\n"},"line_number":2,"absolute_offset":0,"submatches":[]}}\n' "$path"
    printf '{"type":"match","data":{"path":{"text":"%s"},"lines":{"text":"match two\\n"},"line_number":5,"absolute_offset":0,"submatches":[]}}\n' "$path"
    exit 0 ;;
  long)
    long=$(printf 'x%.0s' $(seq 1 600))
    printf '{"type":"match","data":{"path":{"text":"%s"},"lines":{"text":"%s\\n"},"line_number":1,"absolute_offset":0,"submatches":[]}}\n' "$path" "$long"
    exit 0 ;;
  bytes)
    i=1
    while [ "$i" -le 3000 ]; do
      printf '{"type":"match","data":{"path":{"text":"%s"},"lines":{"text":"%04d yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy\\n"},"line_number":%d,"absolute_offset":0,"submatches":[]}}\n' "$path" "$i" "$i"
      i=$((i+1))
    done
    exit 0 ;;
  bytes-path)
    # rg 对非 UTF-8 文件名只给 `path.bytes`（没有 `text`）。
    printf '{"type":"match","data":{"path":{"bytes":[98,97,100]},"lines":{"text":"match line\\n"},"line_number":2,"absolute_offset":0,"submatches":[]}}\n'
    exit 0 ;;
  bytes-then-match)
    printf '{"type":"match","data":{"path":{"bytes":[98,97,100]},"lines":{"text":"match line\\n"},"line_number":2,"absolute_offset":0,"submatches":[]}}\n'
    printf '{"type":"match","data":{"path":{"text":"%s"},"lines":{"text":"match two\\n"},"line_number":2,"absolute_offset":0,"submatches":[]}}\n' "$path"
    exit 0 ;;
  no-path)
    printf '{"type":"match","data":{"line_number":2,"lines":{"text":"match line\\n"}}}\n'
    exit 0 ;;
  raw-bytes-then-match)
    # 包装过的 rg 可能输出非 UTF-8 字节；Node readline 替换为 U+FFFD 后继续。
    printf 'garbage:\377\n'
    printf '{"type":"match","data":{"path":{"text":"%s"},"lines":{"text":"match two\\n"},"line_number":2,"absolute_offset":0,"submatches":[]}}\n' "$path"
    exit 0 ;;
  line4)
    printf '{"type":"match","data":{"path":{"text":"%s"},"lines":{"text":"l4\\n"},"line_number":4,"absolute_offset":0,"submatches":[]}}\n' "$path"
    exit 0 ;;
  many-long)
    long=$(printf 'y%.0s' $(seq 1 600))
    i=1
    while [ "$i" -le 100 ]; do
      printf '{"type":"match","data":{"path":{"text":"%s"},"lines":{"text":"%s\\n"},"line_number":%d,"absolute_offset":0,"submatches":[]}}\n' "$path" "$long" "$i"
      i=$((i+1))
    done
    exit 0 ;;
  *)
    printf '{"type":"match","data":{"path":{"text":"%s"},"lines":{"text":"match line\\n"},"line_number":2,"absolute_offset":0,"submatches":[]}}\n' "$path"
    exit 0 ;;
esac
"#;

fn grep_definition(
    cwd: impl Into<String>,
    operations: Option<Arc<dyn GrepOperations>>,
) -> ToolDefinition {
    create_grep_tool_definition(cwd, Some(GrepToolOptions { operations }))
}

fn details(result: &kiana_agent_core::AgentToolResult<JsonValue>) -> JsonValue {
    result.details.clone()
}

#[tokio::test]
async fn should_include_filename_when_searching_a_single_file() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("example.txt");
    std::fs::write(&file, "first line\nmatch line\nlast line").expect("write");
    let definition = grep_definition("/", None);
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[("FAKE_RG_MODE", "match".to_string())],
        || async {
            execute(
                &definition,
                json!({ "pattern": "match", "path": file.to_string_lossy() }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let expected = &golden()["grep_file_match"];
    assert_eq!(text_output(&result), expected["text"].as_str().unwrap());
    assert_eq!(details(&result), expected["details"]);
}

#[tokio::test]
async fn should_respect_global_limit_and_include_context_lines() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("context.txt");
    std::fs::write(
        &file,
        [
            "before",
            "match one",
            "after",
            "middle",
            "match two",
            "after two",
        ]
        .join("\n"),
    )
    .expect("write");
    let definition = grep_definition("/", None);
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[("FAKE_RG_MODE", "context".to_string())],
        || async {
            execute(
                &definition,
                json!({
                    "pattern": "match",
                    "path": file.to_string_lossy(),
                    "limit": 1,
                    "context": 1
                }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let expected = &golden()["grep_context_limit"];
    assert_eq!(text_output(&result), expected["text"].as_str().unwrap());
    assert_eq!(details(&result), expected["details"]);
    assert!(!text_output(&result).contains("match two"));
}

#[tokio::test]
async fn no_matches_is_a_successful_empty_result() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("example.txt");
    std::fs::write(&file, "first line\nmatch line\nlast line").expect("write");
    let definition = grep_definition("/", None);
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[("FAKE_RG_MODE", "none".to_string())],
        || async {
            execute(
                &definition,
                json!({ "pattern": "zzz", "path": file.to_string_lossy() }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let expected = &golden()["grep_no_matches"];
    assert_eq!(text_output(&result), expected["text"].as_str().unwrap());
    assert_eq!(details(&result), expected["details"]);
    assert_eq!(result.is_error, None);
}

#[tokio::test]
async fn truncates_long_lines_and_reports_it() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("example.txt");
    std::fs::write(&file, "first line\nmatch line\nlast line").expect("write");
    let definition = grep_definition("/", None);
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[("FAKE_RG_MODE", "long".to_string())],
        || async {
            execute(
                &definition,
                json!({ "pattern": "x", "path": file.to_string_lossy() }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let expected = &golden()["grep_long_line"];
    assert_eq!(text_output(&result), expected["text"].as_str().unwrap());
    assert_eq!(details(&result), expected["details"]);
}

#[tokio::test]
async fn stops_at_the_match_limit_without_reading_the_rest() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("example.txt");
    std::fs::write(&file, "first line\nmatch line\nlast line").expect("write");
    let definition = grep_definition("/", None);
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[("FAKE_RG_MODE", "two".to_string())],
        || async {
            execute(
                &definition,
                json!({ "pattern": "match", "path": file.to_string_lossy(), "limit": 1 }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let expected = &golden()["grep_two_limit"];
    assert_eq!(text_output(&result), expected["text"].as_str().unwrap());
    assert_eq!(details(&result), expected["details"]);
}

#[tokio::test]
async fn passes_no_config_and_a_double_dash_before_the_pattern() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("target.txt");
    std::fs::write(&file, "target\n").expect("write");
    let argv_file = temp.path().join("argv.txt");
    let config_file = temp.path().join("rg-config");
    std::fs::write(&config_file, "--fixed-strings\n").expect("write config");

    let definition = grep_definition("/", None);
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[
            ("FAKE_RG_MODE", "argv".to_string()),
            (
                "FAKE_RG_ARGV_FILE",
                argv_file.to_string_lossy().into_owned(),
            ),
            (
                "RIPGREP_CONFIG_PATH",
                config_file.to_string_lossy().into_owned(),
            ),
        ],
        || async {
            execute(
                &definition,
                json!({
                    "pattern": "--pre=/tmp/payload.sh",
                    "path": file.to_string_lossy(),
                    "literal": false
                }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    assert_eq!(text_output(&result), "No matches found");

    let argv: Vec<String> = std::fs::read_to_string(&argv_file)
        .expect("argv file")
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(argv[0], "--no-config");
    assert!(argv.contains(&"--json".to_string()));
    assert!(argv.contains(&"--line-number".to_string()));
    let separator = argv
        .iter()
        .position(|arg| arg == "--")
        .expect("-- separator");
    assert_eq!(argv[separator + 1], "--pre=/tmp/payload.sh");
    assert_eq!(argv[separator + 2], file.to_string_lossy());
}

#[tokio::test]
async fn passes_ignore_case_fixed_strings_and_glob() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("target.txt");
    std::fs::write(&file, "target\n").expect("write");
    let argv_file = temp.path().join("argv.txt");
    let definition = grep_definition("/", None);
    with_fake_tool(
        "rg",
        FAKE_RG,
        &[
            ("FAKE_RG_MODE", "argv".to_string()),
            (
                "FAKE_RG_ARGV_FILE",
                argv_file.to_string_lossy().into_owned(),
            ),
        ],
        || async {
            execute(
                &definition,
                json!({
                    "pattern": "A.B",
                    "path": file.to_string_lossy(),
                    "ignoreCase": true,
                    "literal": true,
                    "glob": "*.txt"
                }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let argv = std::fs::read_to_string(&argv_file).expect("argv file");
    assert!(argv.contains("--ignore-case\n"));
    assert!(argv.contains("--fixed-strings\n"));
    assert!(argv.contains("--glob\n*.txt\n"));
}

#[tokio::test]
async fn reports_path_not_found_before_spawning_rg() {
    let temp = TempDir::new().expect("temp");
    let definition = grep_definition("/", None);
    let missing = temp.path().join("missing");
    let error = with_fake_tool("rg", FAKE_RG, &[], || async {
        execute(
            &definition,
            json!({ "pattern": "x", "path": missing.to_string_lossy() }),
            &TestContext::new(temp.path()),
        )
        .await
        .expect_err("must fail")
    })
    .await;
    assert_eq!(
        error.message,
        format!("Path not found: {}", missing.to_string_lossy())
    );
}

#[tokio::test]
async fn surfaces_rg_stderr_and_exit_code_errors() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("target.txt");
    std::fs::write(&file, "target\n").expect("write");
    let definition = grep_definition("/", None);

    let error = with_fake_tool(
        "rg",
        FAKE_RG,
        &[("FAKE_RG_MODE", "error".to_string())],
        || async {
            execute(
                &definition,
                json!({ "pattern": "x", "path": file.to_string_lossy() }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect_err("must fail")
        },
    )
    .await;
    assert_eq!(error.message, "fake rg failure");

    let error = with_fake_tool(
        "rg",
        FAKE_RG,
        &[("FAKE_RG_MODE", "error-silent".to_string())],
        || async {
            execute(
                &definition,
                json!({ "pattern": "x", "path": file.to_string_lossy() }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect_err("must fail")
        },
    )
    .await;
    assert_eq!(error.message, "ripgrep exited with code 2");
}

#[tokio::test]
async fn reports_the_50kb_limit_and_uses_max_safe_integer_for_max_lines() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("target.txt");
    std::fs::write(&file, "target\n").expect("write");
    let definition = grep_definition("/", None);
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[("FAKE_RG_MODE", "bytes".to_string())],
        || async {
            execute(
                &definition,
                json!({
                    "pattern": "x",
                    "path": file.to_string_lossy(),
                    "limit": 5000
                }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let output = text_output(&result);
    assert!(output.contains("[50.0KB limit reached]"));
    let details: GrepToolDetails =
        serde_json::from_value(details(&result)).expect("details deserialize");
    let truncation = details.truncation.expect("truncation");
    assert!(truncation.truncated);
    assert_eq!(truncation.max_lines, 9_007_199_254_740_991);
    assert_eq!(details.match_limit_reached, None);
}

#[tokio::test]
async fn uses_ctx_cwd_when_provided() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("ctx-cwd-grep.txt");
    std::fs::write(&file, "match in ctx.cwd").expect("write");
    let definition = grep_definition("/", None);
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[
            ("FAKE_RG_MODE", "match".to_string()),
            ("FAKE_RG_EVENT_PATH", file.to_string_lossy().into_owned()),
        ],
        || async {
            execute(
                &definition,
                json!({ "pattern": "match" }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    assert!(
        text_output(&result).contains("ctx-cwd-grep.txt:2: match line"),
        "output: {}",
        text_output(&result)
    );
}

#[tokio::test]
async fn formats_directory_results_as_relative_paths() {
    let temp = TempDir::new().expect("temp");
    let nested = temp.path().join("sub");
    std::fs::create_dir_all(&nested).expect("mkdir");
    let file = nested.join("file.txt");
    std::fs::write(&file, "match\n").expect("write");
    let definition = grep_definition("/", None);
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[
            ("FAKE_RG_MODE", "match".to_string()),
            ("FAKE_RG_EVENT_PATH", file.to_string_lossy().into_owned()),
        ],
        || async {
            execute(
                &definition,
                json!({ "pattern": "match", "path": temp.path().to_string_lossy() }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    assert_eq!(text_output(&result), "sub/file.txt:2: match line");
}

#[derive(Clone)]
struct FixedOps {
    is_directory: bool,
}

#[async_trait]
impl GrepOperations for FixedOps {
    async fn is_directory(&self, _absolute_path: &Path) -> Result<bool, ToolOperationError> {
        Ok(self.is_directory)
    }

    async fn read_file(&self, absolute_path: &Path) -> Result<String, ToolOperationError> {
        std::fs::read_to_string(absolute_path).map_err(|error| ToolOperationError {
            message: error.to_string(),
            code: None,
        })
    }
}

#[tokio::test]
async fn custom_operations_supply_directory_and_filesystem_reads() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("context.txt");
    std::fs::write(&file, "before\nmatch one\nafter").expect("write");
    let definition = grep_definition(
        "/",
        Some(Arc::new(FixedOps {
            is_directory: false,
        })),
    );
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[
            ("FAKE_RG_MODE", "context".to_string()),
            ("FAKE_RG_EVENT_PATH", file.to_string_lossy().into_owned()),
        ],
        || async {
            execute(
                &definition,
                json!({
                    "pattern": "match",
                    "path": file.to_string_lossy(),
                    "context": 1
                }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    assert_eq!(
        text_output(&result),
        "context.txt-1- before\ncontext.txt:2: match one\ncontext.txt-3- after"
    );
}

#[tokio::test]
async fn aborted_signal_fails_before_spawning() {
    let temp = TempDir::new().expect("temp");
    let definition = grep_definition("/", None);
    let signal = CancellationToken::new();
    signal.cancel();
    let error = execute_with_signal(
        &definition,
        json!({ "pattern": "x" }),
        &signal,
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("must abort");
    assert_eq!(error.message, "Operation aborted");
}

/// 在 `isDirectory()`（spawn 之前）里取消 token 的操作。
struct CancellingOps {
    signal: CancellationToken,
}

#[async_trait]
impl GrepOperations for CancellingOps {
    async fn is_directory(&self, _absolute_path: &Path) -> Result<bool, ToolOperationError> {
        self.signal.cancel();
        Ok(false)
    }

    async fn read_file(&self, absolute_path: &Path) -> Result<String, ToolOperationError> {
        Ok(std::fs::read_to_string(absolute_path).unwrap_or_default())
    }
}

/// 回归：TS 只在 spawn 之后注册 abort 监听器（`grep.ts:194`），而 `addEventListener`
/// 在已经 aborted 的 signal 上不会触发。因此在 `ensureTool` / `isDirectory` 期间到达
/// 的取消被忽略，rg 跑完并正常返回匹配结果（而不是 "Operation aborted"）。
#[tokio::test]
async fn abort_before_the_child_is_spawned_is_ignored_like_ts() {
    let temp = TempDir::new().expect("temp");
    let signal = CancellationToken::new();
    let definition = grep_definition(
        "/",
        Some(Arc::new(CancellingOps {
            signal: signal.clone(),
        })),
    );
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[
            ("FAKE_RG_MODE", "match".to_string()),
            ("FAKE_RG_EVENT_PATH", "whatever-file.txt".to_string()),
        ],
        || async {
            execute_with_signal(
                &definition,
                json!({ "pattern": "match", "path": "/whatever-file.txt" }),
                &signal,
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    assert!(signal.is_cancelled());
    assert_eq!(text_output(&result), "whatever-file.txt:2: match line");
}

/// 回归：`rg` 对非 UTF-8 文件名输出 `path.bytes`（没有 `text`）。TS 先 `matchCount++`，
/// 再把不可格式化的匹配跳过；只要计数 > 0 就不返回 "No matches found"。
#[tokio::test]
async fn match_events_without_utf8_path_text_are_counted_but_not_formatted() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("sample.txt");
    std::fs::write(&file, "first\nmatch\n").expect("write");
    let definition = grep_definition("/", None);
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[("FAKE_RG_MODE", "bytes-path".to_string())],
        || async {
            execute(
                &definition,
                json!({ "pattern": "match", "path": file.to_string_lossy() }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    assert_eq!(text_output(&result), "");
    assert_eq!(details(&result), JsonValue::Null);

    // 没有 `path` 的 match 事件同样只计数。
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[("FAKE_RG_MODE", "no-path".to_string())],
        || async {
            execute(
                &definition,
                json!({ "pattern": "match", "path": file.to_string_lossy() }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    assert_eq!(text_output(&result), "");
    assert_eq!(details(&result), JsonValue::Null);
}

/// 回归：无法格式化的匹配也占用 limit（TS 的 `matchCount++` 在检查 path 之前）。
#[tokio::test]
async fn unformattable_matches_still_consume_the_limit() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("sample.txt");
    std::fs::write(&file, "first\nmatch\n").expect("write");
    let definition = grep_definition("/", None);
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[
            ("FAKE_RG_MODE", "bytes-then-match".to_string()),
            ("FAKE_RG_EVENT_PATH", file.to_string_lossy().into_owned()),
        ],
        || async {
            execute(
                &definition,
                json!({ "pattern": "match", "path": file.to_string_lossy(), "limit": 1 }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    // TS（真实 rg）实测：text 只剩 limit 通知，details 为 { matchLimitReached: 1 }。
    assert_eq!(
        text_output(&result),
        "\n\n[1 matches limit reached. Use limit=2 for more, or refine pattern]"
    );
    assert_eq!(details(&result), json!({ "matchLimitReached": 1 }));
}

/// 回归：rg 的 stdout 出现非法 UTF-8 时不能终止读取（`BufReader::lines()` 会返回错误）。
/// Node 的 readline 用 `StringDecoder` 替换为 U+FFFD 后继续，后续事件仍要解析。
#[tokio::test]
async fn invalid_utf8_on_rg_stdout_does_not_stop_the_event_stream() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("sample.txt");
    std::fs::write(&file, "first\nmatch\n").expect("write");
    let definition = grep_definition("/", None);
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[
            ("FAKE_RG_MODE", "raw-bytes-then-match".to_string()),
            ("FAKE_RG_EVENT_PATH", file.to_string_lossy().into_owned()),
        ],
        || async {
            execute(
                &definition,
                json!({ "pattern": "match", "path": file.to_string_lossy() }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    assert_eq!(text_output(&result), "sample.txt:2: match two");
}

/// 回归：`if (glob)` 的假值判断——空字符串不追加 `--glob`。
#[tokio::test]
async fn empty_glob_is_not_forwarded_to_rg() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("target.txt");
    std::fs::write(&file, "target\n").expect("write");
    let argv_file = temp.path().join("argv.txt");
    let definition = grep_definition("/", None);
    with_fake_tool(
        "rg",
        FAKE_RG,
        &[
            ("FAKE_RG_MODE", "argv".to_string()),
            (
                "FAKE_RG_ARGV_FILE",
                argv_file.to_string_lossy().into_owned(),
            ),
        ],
        || async {
            execute(
                &definition,
                json!({ "pattern": "x", "path": file.to_string_lossy(), "glob": "" }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let argv: Vec<String> = std::fs::read_to_string(&argv_file)
        .expect("argv file")
        .lines()
        .map(str::to_string)
        .collect();
    assert!(!argv.iter().any(|arg| arg == "--glob"));
}

/// 回归：`context` 是未取整的 number。TS 用小数做循环变量，`lines[current - 1]` 因此是
/// `undefined`（`?? ""`），匹配行本身也不会出现。用真实 rg 跑 TS 参考得到的输出逐字对比。
#[tokio::test]
async fn fractional_context_keeps_ts_number_semantics() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("f.txt");
    std::fs::write(&file, "l1\nl2\nl3\nl4\nl5\n").expect("write");
    let definition = grep_definition("/", None);

    for (context, expected) in [
        (1.5, "f.txt-2.5- \nf.txt-3.5- \nf.txt-4.5- \nf.txt-5.5- "),
        (0.5, "f.txt-3.5- \nf.txt-4.5- "),
    ] {
        let result = with_fake_tool(
            "rg",
            FAKE_RG,
            &[
                ("FAKE_RG_MODE", "line4".to_string()),
                ("FAKE_RG_EVENT_PATH", file.to_string_lossy().into_owned()),
            ],
            || async {
                execute(
                    &definition,
                    json!({ "pattern": "l4", "path": file.to_string_lossy(), "context": context }),
                    &TestContext::new(temp.path()),
                )
                .await
                .expect("execute")
            },
        )
        .await;
        assert_eq!(text_output(&result), expected, "context {context}");
    }
}

/// 回归：details 的键序须与 TS 的赋值顺序一致（matchLimitReached → truncation →
/// linesTruncated），而不是 `GrepToolDetails` 的结构体字段序。
#[tokio::test]
async fn details_key_order_matches_ts_for_all_three_notices() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("target.txt");
    std::fs::write(&file, "target\n").expect("write");
    let definition = grep_definition("/", None);
    let result = with_fake_tool(
        "rg",
        FAKE_RG,
        &[
            ("FAKE_RG_MODE", "many-long".to_string()),
            ("FAKE_RG_EVENT_PATH", file.to_string_lossy().into_owned()),
        ],
        || async {
            execute(
                &definition,
                json!({ "pattern": "x", "path": file.to_string_lossy() }),
                &TestContext::new(temp.path()),
            )
            .await
            .expect("execute")
        },
    )
    .await;
    let output = text_output(&result);
    assert!(output.contains("100 matches limit reached"));
    assert!(output.contains("50.0KB limit reached"));
    assert!(output.contains("Some lines truncated to 500 chars"));
    let details = result.details.as_object().expect("details object");
    assert_eq!(
        details.keys().map(String::as_str).collect::<Vec<_>>(),
        ["matchLimitReached", "truncation", "linesTruncated"]
    );
    assert_eq!(details["matchLimitReached"], json!(100));
    assert_eq!(details["linesTruncated"], json!(true));
}

#[test]
fn contribution_and_schema_match_ts() {
    let contribution = grep_tool_system_prompt_contribution();
    assert_eq!(
        contribution.snippet,
        "Search file contents for patterns (respects .gitignore)"
    );
    let schema = kiana_tools::grep::grep_parameters_schema();
    assert_eq!(schema["required"], json!(["pattern"]));
    let definition = create_grep_tool_definition("/", None);
    assert_eq!(definition.name, "grep");
    assert_eq!(
        definition.prompt_snippet.as_deref(),
        Some(contribution.snippet)
    );
    assert_eq!(definition.constrained_sampling, None);
    assert!(definition.render_call.is_some());
    assert_eq!(
        grep_definition("/", Some(Arc::new(LocalGrepOperations))).name,
        "grep"
    );
}
