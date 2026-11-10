//! `bash-executor.rs` 的测试。
//!
//! 覆盖：
//! - 从 TS 参考实现生成的黄金样本（ANSI 跨块切分、U+FFFD flush、二进制清理、UTF-8 跨块）；
//! - `tools.test.ts` 的截断/完整输出路径（等价 `seq 3000`）；
//! - 取消与错误传播语义。

use std::error::Error;
use std::sync::{Arc, Mutex};

use kiana_agent_core::types::BoxFuture;
use kiana_ext_api::tool_result::{
    BashExecOptions, BashExecResult, BashExecutionStatus, BashOperations,
};
use kiana_tools::bash_executor::{BashExecutorOptions, execute_bash_with_operations};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

const GOLDEN: &str = include_str!("fixtures/primitives/bash.golden.json");

struct ScriptedOps {
    chunks: Vec<Vec<u8>>,
    exit_code: Option<i32>,
    error: Option<String>,
    cancel_signal: bool,
}

impl BashOperations for ScriptedOps {
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _cwd: &'a str,
        options: BashExecOptions<'a>,
    ) -> BoxFuture<'a, Result<BashExecResult, Box<dyn Error + Send + Sync>>> {
        for chunk in &self.chunks {
            (options.on_data)(chunk);
        }
        if self.cancel_signal
            && let Some(signal) = &options.signal
        {
            signal.cancel();
        }
        let exit_code = self.exit_code;
        let error = self.error.clone();
        Box::pin(async move {
            match error {
                Some(message) => {
                    Err(Box::new(std::io::Error::other(message)) as Box<dyn Error + Send + Sync>)
                }
                None => Ok(BashExecResult {
                    exit_code,
                    execution_status: None,
                }),
            }
        })
    }
}

fn decode_chunks(value: &Value) -> Vec<Vec<u8>> {
    value
        .as_array()
        .expect("chunks")
        .iter()
        .map(|chunk| {
            chunk
                .as_array()
                .expect("bytes")
                .iter()
                .map(|byte| byte.as_u64().expect("byte") as u8)
                .collect()
        })
        .collect()
}

#[tokio::test]
async fn matches_the_ts_golden_samples() {
    let cases: Vec<Value> = serde_json::from_str(GOLDEN).expect("fixture");
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let chunks = decode_chunks(&case["chunks"]);
        let streamed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&streamed);
        let on_chunk: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |chunk: &str| {
            sink.lock().expect("lock").push(chunk.to_string());
        });
        let operations = ScriptedOps {
            chunks,
            exit_code: case["exitCode"].as_i64().map(|code| code as i32),
            error: None,
            cancel_signal: false,
        };
        let result = execute_bash_with_operations(
            "cmd",
            "/tmp",
            &operations,
            Some(BashExecutorOptions {
                on_chunk: Some(on_chunk),
                signal: None,
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{name}: {error}"));

        assert_eq!(
            result.output,
            case["output"].as_str().expect("output"),
            "{name}"
        );
        assert_eq!(
            streamed.lock().expect("lock").concat(),
            case["streamed"].as_str().expect("streamed"),
            "{name}"
        );
        assert_eq!(
            result.exit_code,
            case["exitCode"].as_i64().map(|code| code as i32),
            "{name}"
        );
        assert_eq!(
            result.cancelled,
            case["cancelled"].as_bool().expect("cancelled"),
            "{name}"
        );
        assert_eq!(
            result.truncated,
            case["truncated"].as_bool().expect("truncated"),
            "{name}"
        );
        assert_eq!(
            result.full_output_path.is_some(),
            case["hasFullOutputPath"]
                .as_bool()
                .expect("hasFullOutputPath"),
            "{name}"
        );
    }
}

#[tokio::test]
async fn truncates_by_line_count_and_persists_the_full_output() {
    let mut payload = Vec::new();
    for index in 1..=4000 {
        payload.extend_from_slice(format!("line-{index}\n").as_bytes());
    }
    let operations = ScriptedOps {
        chunks: vec![payload],
        exit_code: Some(0),
        error: None,
        cancel_signal: false,
    };
    let result = execute_bash_with_operations("seq 4000", "/tmp", &operations, None)
        .await
        .expect("bash");

    assert!(result.truncated);
    assert!(result.output.contains("line-2001"));
    assert!(result.output.contains("line-4000"));
    assert!(!result.output.contains("line-2000"));
    let path = result.full_output_path.expect("full output path");
    let full = std::fs::read_to_string(&path).expect("read full output");
    assert!(full.starts_with("line-1\nline-2\nline-3"));
    assert!(full.ends_with("line-3998\nline-3999\nline-4000\n"));
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn cancellation_keeps_partial_output_and_drops_the_exit_code() {
    let signal = CancellationToken::new();
    let operations = ScriptedOps {
        chunks: vec![b"partial\n".to_vec()],
        exit_code: Some(0),
        error: None,
        cancel_signal: true,
    };
    let result = execute_bash_with_operations(
        "cmd",
        "/tmp",
        &operations,
        Some(BashExecutorOptions {
            on_chunk: None,
            signal: Some(signal),
        }),
    )
    .await
    .expect("bash");

    assert!(result.cancelled);
    assert_eq!(result.exit_code, None);
    assert_eq!(result.output, "partial\n");
}

#[tokio::test]
async fn errors_are_rethrown_when_not_cancelled() {
    let operations = ScriptedOps {
        chunks: vec![b"before\n".to_vec()],
        exit_code: None,
        error: Some("boom".to_string()),
        cancel_signal: false,
    };
    let error = execute_bash_with_operations("cmd", "/tmp", &operations, None)
        .await
        .expect_err("must fail");
    assert_eq!(error.to_string(), "boom");
}

#[tokio::test]
async fn errors_are_swallowed_after_cancellation() {
    let signal = CancellationToken::new();
    let operations = ScriptedOps {
        chunks: vec![b"before\n".to_vec()],
        exit_code: None,
        error: Some("boom".to_string()),
        cancel_signal: true,
    };
    let result = execute_bash_with_operations(
        "cmd",
        "/tmp",
        &operations,
        Some(BashExecutorOptions {
            on_chunk: None,
            signal: Some(signal),
        }),
    )
    .await
    .expect("cancelled run keeps output");

    assert!(result.cancelled);
    assert_eq!(result.exit_code, None);
    assert_eq!(result.output, "before\n");
}

#[tokio::test]
async fn byte_limit_truncation_keeps_the_tail() {
    let mut payload = Vec::new();
    for _ in 0..6000 {
        payload.extend_from_slice(b"0123456789\n");
    }
    let operations = ScriptedOps {
        chunks: vec![payload],
        exit_code: Some(0),
        error: None,
        cancel_signal: false,
    };
    let result = execute_bash_with_operations("cmd", "/tmp", &operations, None)
        .await
        .expect("bash");

    assert!(result.truncated);
    assert!(result.output.len() <= 50 * 1024);
    assert!(result.output.ends_with("0123456789"));
    assert!(result.full_output_path.is_some());
    if let Some(path) = &result.full_output_path {
        let _ = std::fs::remove_file(path);
    }
}

#[tokio::test]
async fn unconfirmed_abort_survives_cancellation() {
    // relay 启动命令后被 kill：`aborted|unconfirmed` 是“命令可能已经执行”的安全
    // 结论。取消 token 已置位也不能把它吞成 Ok(cancelled)，否则调用方会重试。
    let signal = CancellationToken::new();
    let operations = ScriptedOps {
        chunks: vec![b"started\n".to_vec()],
        exit_code: None,
        error: Some("aborted|unconfirmed".to_string()),
        cancel_signal: true,
    };
    let error = execute_bash_with_operations(
        "cmd",
        "/tmp",
        &operations,
        Some(BashExecutorOptions {
            on_chunk: None,
            signal: Some(signal),
        }),
    )
    .await
    .expect_err("unconfirmed abort must not be discarded");

    assert!(
        error.to_string().contains("unconfirmed"),
        "unconfirmed signal must reach the caller, got {error}"
    );
}

#[tokio::test]
async fn sandbox_status_and_denial_errors_survive_cancellation() {
    for message in [
        "Unconfirmed: The command may have already executed, but its exit status could not be confirmed (relay killed). Do not retry automatically; inspect the workspace first.",
        "BackendUnavailable: The command was not executed: the sandbox backend is unavailable (bwrap missing). Do not fall back to running it unsandboxed.",
        "Interrupted: The command was interrupted before it started (cancelled); it did not run.",
        "READ_ONLY: /etc/passwd is not writable: the sandbox policy is read-only",
        "OUTSIDE_WORKSPACE: /etc/x is outside the writable roots",
        "PROTECTED_PATH: .git/config is inside a protected path",
        "timeout:1.5|unconfirmed",
    ] {
        let signal = CancellationToken::new();
        let operations = ScriptedOps {
            chunks: vec![b"before\n".to_vec()],
            exit_code: None,
            error: Some(message.to_string()),
            cancel_signal: true,
        };
        let result = execute_bash_with_operations(
            "cmd",
            "/tmp",
            &operations,
            Some(BashExecutorOptions {
                on_chunk: None,
                signal: Some(signal),
            }),
        )
        .await;
        match result {
            Ok(result) => panic!(
                "safety conclusion must survive cancellation ({message}), got Ok({result:?})"
            ),
            Err(error) => assert_eq!(error.to_string(), message),
        }
    }
}

#[tokio::test]
async fn plain_abort_and_timeout_are_still_swallowed_after_cancellation() {
    for message in ["aborted", "timeout:2"] {
        let signal = CancellationToken::new();
        let operations = ScriptedOps {
            chunks: vec![b"partial\n".to_vec()],
            exit_code: None,
            error: Some(message.to_string()),
            cancel_signal: true,
        };
        let result = execute_bash_with_operations(
            "cmd",
            "/tmp",
            &operations,
            Some(BashExecutorOptions {
                on_chunk: None,
                signal: Some(signal),
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{message}: cancelled run keeps output: {error}"));

        assert!(result.cancelled);
        assert_eq!(result.exit_code, None);
        assert_eq!(result.output, "partial\n");
    }
}

/// `execute_bash_with_operations` 把 `BashExecResult.execution_status` 原样复制到
/// `BashResult`（成功路径）。
#[tokio::test]
async fn execution_status_is_copied_to_the_bash_result() {
    struct StatusOps;

    impl BashOperations for StatusOps {
        fn exec<'a>(
            &'a self,
            _command: &'a str,
            _cwd: &'a str,
            _options: BashExecOptions<'a>,
        ) -> BoxFuture<'a, Result<BashExecResult, Box<dyn Error + Send + Sync>>> {
            Box::pin(async {
                Ok(BashExecResult {
                    exit_code: Some(9),
                    execution_status: Some(BashExecutionStatus::ConfirmedExit { code: 9 }),
                })
            })
        }
    }

    let result = execute_bash_with_operations("cmd", "/tmp", &StatusOps, None)
        .await
        .expect("exec");
    assert_eq!(result.exit_code, Some(9));
    assert_eq!(
        result.execution_status,
        Some(BashExecutionStatus::ConfirmedExit { code: 9 })
    );
}
