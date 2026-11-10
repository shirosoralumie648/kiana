//! `utils/clipboard-command.ts` 的移植测试（TS: `test/clipboard-command.test.ts`）。
//!
//! TS 用 `node -e` 作为可控子进程；Rust 侧用 `sh -c`（宿主命令在 Linux/macOS 可用）。
//! Windows 跳过（`#![cfg(unix)]`）。

#![cfg(unix)]

use std::time::{Duration, Instant};

use kiana_core::clipboard::{ClipboardCommandOptions, run_clipboard_command};

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn with_input(input: &str) -> Option<ClipboardCommandOptions> {
    Some(ClipboardCommandOptions {
        input: Some(input.to_string()),
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// describe("clipboard commands")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preserves_binary_output_and_distinguishes_empty_success_from_failure() {
    assert_eq!(
        run_clipboard_command("sh", &args(&["-c", "printf '\\000\\377\\012'"]), None).await,
        Some(vec![0, 255, 10])
    );
    assert_eq!(
        run_clipboard_command("sh", &args(&["-c", ":"]), None).await,
        Some(Vec::new())
    );
    assert_eq!(
        run_clipboard_command("sh", &args(&["-c", "exit 1"]), None).await,
        None
    );
    assert_eq!(
        run_clipboard_command("kiana-clipboard-command-does-not-exist", &[], None).await,
        None
    );
}

#[tokio::test]
async fn sends_unicode_input_to_clipboard_writers() {
    let script = "input=$(cat); [ \"$input\" = 'café 日本語' ]";
    assert_eq!(
        run_clipboard_command("sh", &args(&["-c", script]), with_input("café 日本語")).await,
        Some(Vec::new())
    );
    // 输入确实被写进 stdin：内容不匹配时脚本以 1 退出。
    assert_eq!(
        run_clipboard_command("sh", &args(&["-c", script]), with_input("wrong")).await,
        None
    );
}

#[tokio::test]
async fn discards_stdout_when_input_is_provided_but_keeps_stdin_open() {
    // 审计修正：stdio 是 ["pipe", input === undefined ? "pipe" : "ignore", "ignore"]——
    // stdin 永远是 pipe（有 input 时仍要能写进去），stdout 在有 input 时是 ignore。
    assert_eq!(
        run_clipboard_command("sh", &args(&["-c", "echo ignored-output"]), with_input("x")).await,
        Some(Vec::new())
    );
    // 没有 input 时 stdout 被收集。
    assert_eq!(
        run_clipboard_command("sh", &args(&["-c", "echo captured"]), None).await,
        Some(b"captured\n".to_vec())
    );
}

#[tokio::test]
async fn times_out_without_blocking_the_event_loop() {
    let started = Instant::now();
    let result = run_clipboard_command(
        "sh",
        &args(&["-c", "sleep 10"]),
        Some(ClipboardCommandOptions {
            timeout_ms: Some(200),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(result, None);
    // 超时（200ms）后立刻返回；如果实现里阻塞等待子进程，这里会接近 10s。
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "elapsed {:?}",
        started.elapsed()
    );
}

// #11179：剪贴板守护进程不能与 kiana 共享进程组，否则 Ctrl+Z 会停掉它们。
#[tokio::test]
async fn runs_commands_outside_kiana_process_group() {
    let child_pgid = run_clipboard_command("sh", &args(&["-c", "ps -o pgid= -p $$"]), None)
        .await
        .expect("child pgid");
    let own_pgid = run_clipboard_command(
        "ps",
        &args(&["-o", "pgid=", "-p", &std::process::id().to_string()]),
        None,
    )
    .await
    .expect("own pgid");

    let child_pgid = String::from_utf8_lossy(&child_pgid).trim().to_string();
    let own_pgid = String::from_utf8_lossy(&own_pgid).trim().to_string();
    assert!(
        !child_pgid.is_empty() && child_pgid.chars().all(|c| c.is_ascii_digit()),
        "child pgid: {child_pgid:?}"
    );
    assert!(
        !own_pgid.is_empty() && own_pgid.chars().all(|c| c.is_ascii_digit()),
        "own pgid: {own_pgid:?}"
    );
    assert_ne!(child_pgid, own_pgid);
}

#[tokio::test]
async fn rejects_output_above_the_buffer_limit() {
    assert_eq!(
        run_clipboard_command(
            "sh",
            &args(&["-c", "head -c 1024 /dev/zero"]),
            Some(ClipboardCommandOptions {
                max_buffer_bytes: Some(16),
                ..Default::default()
            })
        )
        .await,
        None
    );
}

/// 回归（审计）：超过 `maxBufferBytes` 必须像 TS 的 `abort()` 一样**立即** `SIGKILL` 并返回
/// `None`，不能等子进程自己退出、也不能等超时。
#[tokio::test]
async fn aborts_immediately_when_output_exceeds_the_buffer_limit() {
    let started = Instant::now();
    let result = run_clipboard_command(
        "sh",
        &args(&["-c", "head -c 100000 /dev/zero; sleep 5"]),
        Some(ClipboardCommandOptions {
            timeout_ms: Some(3000),
            max_buffer_bytes: Some(16),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(result, None);
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "overflow must abort immediately instead of waiting for the child or the timeout: {elapsed:?}"
    );
}
