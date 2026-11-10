//! `test/suite/regressions/5303-bash-output-truncation.test.ts`。
//!
//! #5303：`waitForChildProcess` 曾在 `exit` 时武装固定 100ms 定时器并在到点时销毁
//! stdio 流；孙进程持有 stdout 时，`close` 永不触发，于是 exit 后 100ms 之后的输出被
//! 静默截断。修复是“每个 chunk 都重置 100ms 空闲计时器”。
//!
//! 用 `tokio::io::duplex`（mock reader）+ `tokio::time::pause` 的虚拟时间驱动，避免真实
//! 子进程的写入与定时器竞争。

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_core::child_process::wait_for_child_process_parts_observed;
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

type Output = Arc<Mutex<String>>;

fn spawn_waiter(
    exit_rx: oneshot::Receiver<io::Result<Option<i32>>>,
    stdout: Option<tokio::io::DuplexStream>,
    stderr: Option<tokio::io::DuplexStream>,
    output: Output,
) -> tokio::task::JoinHandle<io::Result<Option<i32>>> {
    tokio::spawn(async move {
        wait_for_child_process_parts_observed(
            async move {
                match exit_rx.await {
                    Ok(result) => result,
                    Err(_) => Err(io::Error::other("exit sender dropped")),
                }
            },
            stdout,
            stderr,
            move |is_stdout, data| {
                if is_stdout {
                    output
                        .lock()
                        .unwrap()
                        .push_str(&String::from_utf8_lossy(data));
                }
            },
        )
        .await
    })
}

#[tokio::test(start_paused = true)]
async fn captures_output_emitted_after_exit_while_a_descendant_holds_stdout_open() {
    let (mut writer, reader) = tokio::io::duplex(4096);
    let (exit_tx, exit_rx) = oneshot::channel();
    let output: Output = Arc::new(Mutex::new(String::new()));
    let handle = spawn_waiter(exit_rx, Some(reader), None, output.clone());

    writer.write_all(b"HEAD\n").await.unwrap();
    exit_tx.send(Ok(Some(0))).unwrap();
    tokio::task::yield_now().await;

    for index in 1..=6 {
        tokio::time::advance(Duration::from_millis(50)).await;
        writer
            .write_all(format!("TICK{index}\n").as_bytes())
            .await
            .unwrap();
        tokio::task::yield_now().await;
        assert!(
            !handle.is_finished(),
            "resolved too early after TICK{index}"
        );
    }

    tokio::time::advance(Duration::from_millis(99)).await;
    assert!(!handle.is_finished(), "resolved before the grace elapsed");
    tokio::time::advance(Duration::from_millis(1)).await;

    let code = handle.await.expect("join").expect("wait");
    assert_eq!(code, Some(0));
    let output = output.lock().unwrap().clone();
    assert!(output.contains("HEAD"), "output was {output:?}");
    assert!(output.contains("TICK6"), "output was {output:?}");
}

#[tokio::test(start_paused = true)]
async fn resolves_after_the_grace_when_a_descendant_holds_stdout_open_but_stays_quiet() {
    let (mut writer, reader) = tokio::io::duplex(4096);
    let (exit_tx, exit_rx) = oneshot::channel();
    let output: Output = Arc::new(Mutex::new(String::new()));
    let handle = spawn_waiter(exit_rx, Some(reader), None, output.clone());

    writer.write_all(b"DONE\n").await.unwrap();
    exit_tx.send(Ok(Some(0))).unwrap();
    tokio::task::yield_now().await;

    tokio::time::advance(Duration::from_millis(99)).await;
    assert!(!handle.is_finished(), "resolved before the grace elapsed");
    tokio::time::advance(Duration::from_millis(1)).await;

    let code = handle.await.expect("join").expect("wait");
    assert_eq!(code, Some(0));
    assert!(output.lock().unwrap().contains("DONE"));
}

#[tokio::test(start_paused = true)]
async fn resolves_immediately_when_both_streams_end_after_exit() {
    let (writer, reader) = tokio::io::duplex(4096);
    let (exit_tx, exit_rx) = oneshot::channel();
    let output: Output = Arc::new(Mutex::new(String::new()));
    let handle = spawn_waiter(exit_rx, Some(reader), None, output.clone());

    drop(writer); // EOF
    exit_tx.send(Ok(Some(4))).unwrap();

    let code = handle.await.expect("join").expect("wait");
    assert_eq!(code, Some(4));
}

#[tokio::test(start_paused = true)]
async fn exits_before_stream_data_is_still_returned() {
    let (mut writer, reader) = tokio::io::duplex(4096);
    let (exit_tx, exit_rx) = oneshot::channel();
    let output: Output = Arc::new(Mutex::new(String::new()));
    let handle = spawn_waiter(exit_rx, Some(reader), None, output.clone());

    writer.write_all(b"early\n").await.unwrap();
    exit_tx.send(Ok(Some(2))).unwrap();
    drop(writer);

    let code = handle.await.expect("join").expect("wait");
    assert_eq!(code, Some(2));
    assert!(output.lock().unwrap().contains("early"));
}

#[tokio::test(start_paused = true)]
async fn propagates_the_exit_future_error() {
    let (exit_tx, exit_rx) = oneshot::channel();
    let output: Output = Arc::new(Mutex::new(String::new()));
    let handle = spawn_waiter(exit_rx, None, None, output);

    exit_tx.send(Err(io::Error::other("spawn failed"))).unwrap();
    let error = handle.await.expect("join").expect_err("must reject");
    assert_eq!(error.to_string(), "spawn failed");
}
