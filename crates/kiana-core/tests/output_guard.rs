//! `core/output-guard.ts` 的公开 API 测试（TS 无对应测试文件，按 `ARCHITECTURE.md` §4.8 第 9 条新写）。
//!
//! 通过 `dup2` 把 fd 1 / fd 2 重定向到临时文件，验证两条通道的真实落点与字节顺序：
//! 未接管时 `write_stdout` 与 `write_raw_stdout` 都写 stdout；接管后 `write_stdout` 写 stderr，
//! 而 `write_raw_stdout` 仍写 stdout。空串写入被丢弃，`flush_raw_stdout` 之后字节全部可见。
//!
//! 整个文件只有一个测试：fd 重定向是进程级的，libtest 的并行测试线程会污染捕获内容。
//!
//! TS: `packages/coding-agent/src/core/output-guard.ts`；契约见 spec/ca-session.md §2.11。

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;

use kiana_core::output_guard::{
    flush_raw_stdout, is_stdout_taken_over, restore_stdout, take_over_stdout,
    wait_for_raw_stdout_backpressure, write_raw_stdout, write_stdout,
};

/// 把 fd 1 / fd 2 重定向到临时文件；`finish`/Drop 恢复。
struct CapturedFds {
    saved_stdout: i32,
    saved_stderr: i32,
    stdout: std::fs::File,
    stderr: std::fs::File,
    restored: bool,
}

impl CapturedFds {
    fn new() -> Self {
        // 先冲掉 Rust 侧缓冲，避免把 libtest 的输出带进捕获文件。
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        let stdout = tempfile::tempfile().expect("tempfile");
        let stderr = tempfile::tempfile().expect("tempfile");
        // SAFETY: `dup`/`dup2` 只操作进程文件描述符；单测试文件内没有并发捕获。
        unsafe {
            let saved_stdout = libc::dup(1);
            let saved_stderr = libc::dup(2);
            assert!(saved_stdout >= 0 && saved_stderr >= 0, "dup failed");
            assert!(libc::dup2(stdout.as_raw_fd(), 1) >= 0, "dup2 stdout failed");
            assert!(libc::dup2(stderr.as_raw_fd(), 2) >= 0, "dup2 stderr failed");
            Self {
                saved_stdout,
                saved_stderr,
                stdout,
                stderr,
                restored: false,
            }
        }
    }

    fn read_stdout(&mut self) -> String {
        let _ = std::io::stdout().flush();
        self.stdout.seek(SeekFrom::Start(0)).expect("seek stdout");
        let mut text = String::new();
        self.stdout.read_to_string(&mut text).expect("read stdout");
        text
    }

    fn read_stderr(&mut self) -> String {
        let _ = std::io::stderr().flush();
        self.stderr.seek(SeekFrom::Start(0)).expect("seek stderr");
        let mut text = String::new();
        self.stderr.read_to_string(&mut text).expect("read stderr");
        text
    }

    fn finish(mut self) -> (String, String) {
        self.restore();
        let stdout = self.read_stdout();
        let stderr = self.read_stderr();
        (stdout, stderr)
    }

    fn restore(&mut self) {
        if self.restored {
            return;
        }
        // SAFETY: 恢复保存的 fd 并关闭副本；只调用一次。
        unsafe {
            libc::dup2(self.saved_stdout, 1);
            libc::dup2(self.saved_stderr, 2);
            libc::close(self.saved_stdout);
            libc::close(self.saved_stderr);
        }
        self.restored = true;
    }
}

impl Drop for CapturedFds {
    fn drop(&mut self) {
        self.restore();
    }
}

/// 非阻塞地读一次管道（最多 64 KiB）；无数据时返回空向量。
fn read_pipe_once(fd: i32) -> Vec<u8> {
    let mut buffer = vec![0u8; 64 * 1024];
    // SAFETY: `buffer` is valid for `buffer.len()` bytes and `fd` is an open pipe read end.
    let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if read <= 0 {
        buffer.clear();
    } else {
        buffer.truncate(read as usize);
    }
    buffer
}

#[tokio::test]
async fn output_guard_routes_stdout_and_raw_stdout_like_the_ts_contract() {
    restore_stdout();
    assert!(!is_stdout_taken_over());

    // ---- Phase 1: channels, takeover, restore -------------------------------------------
    let mut capture = CapturedFds::new();

    // Not taken over: both channels write stdout. `write_raw_stdout("")` is dropped.
    write_stdout("hello");
    write_raw_stdout("raw1");
    write_raw_stdout("");
    write_raw_stdout("raw2");
    flush_raw_stdout().await;
    assert_eq!(capture.read_stdout(), "helloraw1raw2");
    assert_eq!(capture.read_stderr(), "");

    // Taken over: the normal channel goes to stderr; the raw channel still writes fd 1.
    take_over_stdout();
    assert!(is_stdout_taken_over());
    take_over_stdout(); // idempotent
    write_stdout("to-stderr");
    write_raw_stdout("raw3");
    wait_for_raw_stdout_backpressure().await;
    assert_eq!(capture.read_stdout(), "helloraw1raw2raw3");
    assert_eq!(capture.read_stderr(), "to-stderr");

    // Restored: the normal channel returns to stdout; restoring twice is a no-op.
    restore_stdout();
    assert!(!is_stdout_taken_over());
    restore_stdout();
    write_stdout("tail");
    flush_raw_stdout().await;

    let (stdout, stderr) = capture.finish();
    assert_eq!(stdout, "helloraw1raw2raw3tail");
    assert_eq!(stderr, "to-stderr");

    // ---- Phase 2: writes from several threads keep per-writer order ---------------------
    let mut capture = CapturedFds::new();
    let mut handles = Vec::new();
    for writer in 0..4 {
        handles.push(std::thread::spawn(move || {
            for index in 0..25 {
                write_raw_stdout(&format!("<{writer}:{index}>"));
            }
        }));
    }
    for handle in handles {
        handle.join().expect("writer thread");
    }
    flush_raw_stdout().await;
    let content = capture.read_stdout();
    assert_eq!(content.matches('<').count(), 4 * 25);
    for writer in 0..4 {
        let mut last = 0;
        for index in 0..25 {
            let needle = format!("<{writer}:{index}>");
            let position = content
                .find(&needle)
                .unwrap_or_else(|| panic!("missing {needle} in {content:?}"));
            assert!(position >= last, "{needle} is out of order in {content:?}");
            last = position;
        }
    }
    // ---- Phase 3: `wait_for_raw_stdout_backpressure` spins until the tail stops changing ------
    //
    // fd 1 becomes a pipe we drain ourselves. The writer thread blocks once the pipe is full, so
    // the waiter's barrier sits in the queue while a much larger write (`second`) is appended
    // behind it. A barrier-only wait returns as soon as `first` and the barrier are done and
    // leaves nearly all of `second` pending; the TS loop (`while (tail !== rawStdoutWriteTail)`)
    // keeps waiting until `second` is written too.
    let mut pipe_fds = [0i32; 2];
    // SAFETY: pipe/dup/dup2/fcntl/read/close only touch file descriptors; this file has one test.
    let saved_fd1 = unsafe {
        assert_eq!(libc::pipe(pipe_fds.as_mut_ptr()), 0, "pipe");
        let saved = libc::dup(1);
        assert!(saved >= 0, "dup fd 1");
        assert!(
            libc::dup2(pipe_fds[1], 1) >= 0,
            "dup2 pipe write end onto fd 1"
        );
        libc::close(pipe_fds[1]);
        let flags = libc::fcntl(pipe_fds[0], libc::F_GETFL);
        assert!(flags >= 0, "fcntl F_GETFL");
        assert!(
            libc::fcntl(pipe_fds[0], libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0,
            "fcntl F_SETFL"
        );
        saved
    };
    let read_fd = pipe_fds[0];

    // `first` is bigger than a pipe buffer (64 KiB on Linux), so it cannot finish while the test
    // sleeps; `second` is far bigger, so a barrier-only wait cannot have seen most of it.
    let first = "A".repeat(1 << 20);
    let second = "B".repeat(8 << 20);
    write_raw_stdout(&first);
    let wait_task = tokio::spawn(wait_for_raw_stdout_backpressure());
    // Let the waiter snapshot the tail and queue its barrier, then append `second` behind it.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    write_raw_stdout(&second);

    let mut second_bytes = 0usize;
    loop {
        second_bytes += read_pipe_once(read_fd)
            .iter()
            .filter(|byte| **byte == b'B')
            .count();
        if wait_task.is_finished() {
            break;
        }
        tokio::task::yield_now().await;
    }
    wait_task.await.expect("wait task");
    // SAFETY: restore fd 1 and close the pipe read end; only called once.
    unsafe {
        assert!(libc::dup2(saved_fd1, 1) >= 0, "restore fd 1");
        libc::close(saved_fd1);
        libc::close(read_fd);
    }
    assert!(
        second_bytes > (4 << 20),
        "wait_for_raw_stdout_backpressure did not wait for a write appended during the wait \
         (only {second_bytes} of {} bytes of the second write were consumed)",
        second.len()
    );

    let (stdout, stderr) = capture.finish();
    assert_eq!(stdout, content);
    assert_eq!(stderr, "");
}
