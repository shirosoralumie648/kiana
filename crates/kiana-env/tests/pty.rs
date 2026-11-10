//! `pty`（docs/protocol.md）：非 unix 一律 `ENOTSUP`；unix 上覆盖 `started` 事件、原始字节、exitCode、
//! 终端结束后 `EBADF`、缺失 cwd 的文案、前台进程组与输入顺序。
//!
//! 对应 packages/env/test/terminal.test.ts（7 个用例，逐条移植）；另有一条不经过行规程缓冲的
//! 全速输入顺序回归（`delivers_input_in_arrival_order_at_full_speed`），理由见 `keeps_input_in_order`。

mod common;

use common::{Daemon, TempDir, assert_error};
use serde_json::json;

#[test]
fn requires_a_working_directory_and_terminal_size() {
    let dir = TempDir::new("pty-args");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    assert_error(
        &daemon.call_error(1, "pty", json!({ "cols": 80, "rows": 24 })),
        "EINVAL",
        "pty needs cwd",
    );
    assert_error(
        &daemon.call_error(2, "pty", json!({ "cwd": dir.str(), "cols": 0, "rows": 24 })),
        "EINVAL",
        "cols must be between 1 and 4096",
    );
    assert_error(
        &daemon.call_error(
            3,
            "pty",
            json!({ "cwd": dir.str(), "cols": 80, "rows": 5000 }),
        ),
        "EINVAL",
        "rows must be between 1 and 4096",
    );
    assert_error(
        &daemon.call_error(
            4,
            "pty",
            json!({ "cwd": dir.str(), "cols": 80, "rows": 24, "argv": [] }),
        ),
        "EINVAL",
        "argv must be a non-empty array of strings",
    );
}

#[test]
fn rejects_writes_to_a_terminal_that_does_not_exist() {
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    for op in ["ptyWrite", "ptyResize", "ptyForeground"] {
        assert_error(
            &daemon.call_error(1, op, json!({ "target": 999 })),
            "EBADF",
            "no running terminal with this id",
        );
    }
    assert_error(
        &daemon.call_error(2, "ptyWrite", json!({})),
        "EINVAL",
        "missing field target",
    );
}

#[cfg(not(unix))]
#[test]
fn terminals_are_not_supported_on_this_system_yet() {
    let dir = TempDir::new("pty-windows");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    assert_error(
        &daemon.call_error(
            1,
            "pty",
            json!({ "cwd": dir.str(), "cols": 80, "rows": 24, "argv": ["cmd"] }),
        ),
        "ENOTSUP",
        "Terminals are not supported on this system yet",
    );
}

#[cfg(unix)]
mod unix {
    use super::*;
    use common::EVENT;
    use serde_json::Value;
    use std::time::{Duration, Instant};

    /// 收集一个 pty 请求的输出字节，直到它结束。
    struct Terminal {
        events: Vec<common::Frame>,
        reply: common::Frame,
    }

    fn run(daemon: &mut Daemon, id: u32) -> Terminal {
        let (events, reply) = daemon.reply_with_events(id);
        Terminal { events, reply }
    }

    fn output(events: &[common::Frame]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for event in events {
            if event.json["kind"] == json!("output") {
                bytes.extend_from_slice(&event.payload);
            }
        }
        bytes
    }

    #[test]
    fn runs_a_program_with_input_resizing_and_its_exit_code() {
        let dir = TempDir::new("pty-shell");
        let mut daemon = Daemon::start(&[]);
        daemon.handshake();
        daemon.request(
            1,
            "pty",
            json!({
                "cwd": dir.str(),
                "argv": ["/bin/sh"],
                "env": { "PS1": "$ ", "TERM": "xterm-256color" },
                "cols": 80,
                "rows": 24,
            }),
        );
        let started = daemon.wait_event(1, Duration::from_secs(30));
        assert_eq!(started.json["kind"], json!("started"));
        let pid = started.json["pid"].as_u64().expect("pid");
        assert!(pid > 0);
        // `ptyWrite` carries its payload as the frame payload.
        let mut payload = Vec::new();
        payload.extend_from_slice(b"test -t 0 && echo IS-A-TTY; stty size; echo $TERM\n");
        daemon.send_frame(
            common::REQUEST,
            2,
            &json!({ "op": "ptyWrite", "target": 1 }),
            &payload,
        );
        let mut collected = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !String::from_utf8_lossy(&collected).contains("xterm-256color") {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = daemon.wait_event(1, remaining);
            if event.json["kind"] == json!("output") {
                collected.extend_from_slice(&event.payload);
            }
        }
        let text = String::from_utf8_lossy(&collected);
        assert!(text.contains("IS-A-TTY"), "output: {text:?}");
        assert!(text.contains("24 80"), "output: {text:?}");
        // `ptyResize { cols, rows }` changes the size; `stty size` prints "rows cols".
        daemon.send_json(
            common::REQUEST,
            3,
            &json!({ "op": "ptyResize", "target": 1, "cols": 100, "rows": 30 }),
        );
        assert_eq!(ok_reply(&mut daemon, 3), json!({}));
        daemon.send_frame(
            common::REQUEST,
            4,
            &json!({ "op": "ptyWrite", "target": 1 }),
            b"stty size\n",
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = daemon.wait_event(1, remaining);
            if event.json["kind"] == json!("output") {
                collected.extend_from_slice(&event.payload);
                if String::from_utf8_lossy(&collected).contains("30 100") {
                    break;
                }
            }
        }
        daemon.send_frame(
            common::REQUEST,
            5,
            &json!({ "op": "ptyWrite", "target": 1 }),
            b"exit 3\n",
        );
        let terminal = run(&mut daemon, 1);
        assert_eq!(terminal.reply.json["exitCode"], json!(3));
        // After the terminal ends, a write reports EBADF.
        assert_error(
            &daemon.call_error(6, "ptyWrite", json!({ "target": 1 })),
            "EBADF",
            "no running terminal with this id",
        );
    }

    fn ok_reply(daemon: &mut Daemon, id: u32) -> Value {
        let frame = daemon.reply(id);
        assert_eq!(frame.kind, common::RESULT, "reply {:?}", frame.json);
        frame.json
    }

    #[test]
    fn delivers_raw_bytes_not_decoded_text() {
        let dir = TempDir::new("pty-raw");
        let mut daemon = Daemon::start(&[]);
        daemon.handshake();
        daemon.request(
            1,
            "pty",
            json!({ "cwd": dir.str(), "argv": ["sh", "-c", "printf 'a\\377b'"], "cols": 80, "rows": 24 }),
        );
        let terminal = run(&mut daemon, 1);
        assert_eq!(terminal.reply.json["exitCode"], json!(0));
        let bytes = output(&terminal.events);
        assert_eq!(bytes, vec![0x61, 0xff, 0x62]);
    }

    #[test]
    fn keeps_input_in_order() {
        // terminal.test.ts, "keeps input in order": 4000 writes of 9 bytes reach the program in order.
        // The terminal stays in its default canonical mode, as in the TS test: every chunk the daemon
        // receives on the reader thread is written to the master in arrival order.
        //
        // One run, one assertion: a daemon that reorders or drops input fails the test, and nothing
        // retries it. Writes are paced, as in the base port, because the TS case is intermittently
        // red: at full speed the canonical-mode line discipline drops input once its 4096-byte
        // buffer fills before the program reads it. Unpaced, this scenario corrupts `input.txt` at a
        // rate that swings with load and with how fast the harness feeds frames (measured here: 4 of
        // 30 with this port, 3 of 20 and 18 of 20 with the reference daemon in two harnesses);
        // paced at 200 µs every 10 pieces it was 0 of 150. So the test exercises the daemon's
        // ordering, not the kernel buffer. `delivers_input_in_arrival_order_at_full_speed` covers
        // the same ordering unpaced in raw mode, where the line discipline has no canonical buffer
        // to overflow.
        let dir = TempDir::new("pty-input");
        let mut daemon = Daemon::start(&[]);
        daemon.handshake();
        daemon.request(
            1,
            "pty",
            json!({
                "cwd": dir.str(),
                "argv": ["sh", "-c", "head -c 40000 > input.txt"],
                "cols": 80,
                "rows": 24,
            }),
        );
        daemon.wait_event(1, Duration::from_secs(30));
        let mut expected = String::new();
        for index in 0..4000 {
            let piece = format!("{index:09}\n");
            expected.push_str(&piece);
            daemon.send_frame(
                common::REQUEST,
                2,
                &json!({ "op": "ptyWrite", "target": 1 }),
                piece.as_bytes(),
            );
            if index % 10 == 9 {
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
        }
        let terminal = run(&mut daemon, 1);
        assert_eq!(terminal.reply.json["exitCode"], json!(0));
        let written = std::fs::read_to_string(dir.join("input.txt")).expect("input.txt");
        assert_eq!(written, expected);
    }

    #[test]
    fn delivers_input_in_arrival_order_at_full_speed() {
        // The order guarantee behind terminal.test.ts's "keeps input in order", without the
        // canonical-mode buffer race: in raw mode the line discipline hands every byte on (and makes
        // the writer wait) instead of dropping input, so one unpaced run is deterministic. 40,000
        // bytes of 9-byte writes must reach the program in exactly the order they were sent.
        let dir = TempDir::new("pty-raw-input");
        let mut daemon = Daemon::start(&[]);
        daemon.handshake();
        daemon.request(
            1,
            "pty",
            json!({
                "cwd": dir.str(),
                // The marker file says raw mode is on before the first write arrives, so no input
                // can still be subject to the canonical buffer.
                "argv": ["sh", "-c", "stty raw -echo; : > ready; head -c 40000 > input.txt"],
                "cols": 80,
                "rows": 24,
            }),
        );
        daemon.wait_event(1, Duration::from_secs(30));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !std::path::Path::new(&dir.join("ready")).exists() {
            assert!(
                Instant::now() < deadline,
                "the program never switched the terminal to raw mode"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut expected = String::new();
        for index in 0..4000 {
            let piece = format!("{index:09}\n");
            expected.push_str(&piece);
            daemon.send_frame(
                common::REQUEST,
                2,
                &json!({ "op": "ptyWrite", "target": 1 }),
                piece.as_bytes(),
            );
        }
        let terminal = run(&mut daemon, 1);
        assert_eq!(terminal.reply.json["exitCode"], json!(0));
        let written = std::fs::read_to_string(dir.join("input.txt")).expect("input.txt");
        assert_eq!(written, expected);
    }

    #[test]
    fn reports_the_job_in_the_foreground_not_the_shell_at_its_prompt() {
        let dir = TempDir::new("pty-foreground");
        let mut daemon = Daemon::start(&[]);
        daemon.handshake();
        daemon.request(
            1,
            "pty",
            json!({
                "cwd": dir.str(),
                "argv": ["/bin/sh", "-i"],
                "env": { "PS1": "$ ", "TERM": "xterm-256color" },
                "cols": 80,
                "rows": 24,
            }),
        );
        let started = daemon.wait_event(1, Duration::from_secs(30));
        assert_eq!(started.json["kind"], json!("started"));
        let shell_pid = started.json["pid"].as_u64().unwrap();
        daemon.send_frame(
            common::REQUEST,
            2,
            &json!({ "op": "ptyWrite", "target": 1 }),
            b"echo READY\n",
        );
        // Wait for the prompt's output before asking about the foreground.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut collected = Vec::new();
        while !String::from_utf8_lossy(&collected).contains("READY") {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = daemon.wait_event(1, remaining);
            if event.json["kind"] == json!("output") {
                collected.extend_from_slice(&event.payload);
            }
        }
        let quiet = daemon.call(3, "ptyForeground", json!({ "target": 1 }));
        assert_eq!(quiet, json!({ "foreground": null }));
        daemon.send_frame(
            common::REQUEST,
            4,
            &json!({ "op": "ptyWrite", "target": 1 }),
            b"sleep 100\n",
        );
        // The shell forks before it execs: wait until the foreground job is the command itself, not
        // the forked shell. terminal.test.ts asserts the name only on darwin and linux
        // (`const named = process.platform === "darwin" || process.platform === "linux"`); elsewhere
        // it checks the pid alone, because `process_name` returns None there.
        let named = cfg!(any(target_vendor = "apple", target_os = "linux"));
        let mut foreground =
            daemon.call(5, "ptyForeground", json!({ "target": 1 }))["foreground"].clone();
        for _ in 0..100 {
            let ready = foreground.is_object() && (!named || foreground["name"] == json!("sleep"));
            if ready {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
            foreground =
                daemon.call(5, "ptyForeground", json!({ "target": 1 }))["foreground"].clone();
        }
        let pid = foreground["pid"].as_u64().expect("foreground pid");
        assert!(pid > 0);
        assert_ne!(pid, shell_pid);
        if named {
            assert_eq!(foreground["name"], json!("sleep"));
        }
        // Closing aborts the request.
        daemon.cancel(1, false);
        let reply = daemon.reply(1);
        assert_eq!(reply.kind, common::ERROR);
        assert_eq!(reply.json["code"], json!("aborted"));
        assert_error(
            &daemon.call_error(6, "ptyForeground", json!({ "target": 1 })),
            "EBADF",
            "no running terminal with this id",
        );
    }

    #[test]
    fn kills_the_program_on_close() {
        let dir = TempDir::new("pty-kill");
        let mut daemon = Daemon::start(&[]);
        daemon.handshake();
        daemon.request(
            1,
            "pty",
            json!({ "cwd": dir.str(), "argv": ["sleep", "100"], "cols": 80, "rows": 24 }),
        );
        daemon.wait_event(1, Duration::from_secs(30));
        daemon.cancel(1, false);
        let reply = daemon.reply(1);
        assert_eq!(reply.json["code"], json!("aborted"));
    }

    #[test]
    fn fails_to_open_in_a_missing_directory() {
        let dir = TempDir::new("pty-missing");
        let missing = dir.join("missing");
        let mut daemon = Daemon::start(&[]);
        daemon.handshake();
        // Unlike `exec`, the message has no second line.
        assert_error(
            &daemon.call_error(
                1,
                "pty",
                json!({ "cwd": missing, "argv": ["sh"], "cols": 80, "rows": 24 }),
            ),
            "spawn_error",
            &format!("Working directory does not exist: {missing}"),
        );
    }

    #[test]
    fn runs_the_shell_as_a_login_shell_without_argv() {
        let dir = TempDir::new("pty-login");
        let mut daemon = Daemon::start(&[]);
        daemon.handshake();
        daemon.request(
            1,
            "pty",
            json!({
                "cwd": dir.str(),
                "env": { "SHELL": "/bin/sh", "TERM": "xterm" },
                "cols": 80,
                "rows": 24,
            }),
        );
        let started = daemon.wait_event(1, Duration::from_secs(30));
        assert_eq!(started.json["kind"], json!("started"));
        daemon.send_frame(
            common::REQUEST,
            2,
            &json!({ "op": "ptyWrite", "target": 1 }),
            b"echo done; exit 0\n",
        );
        let terminal = run(&mut daemon, 1);
        assert_eq!(terminal.reply.json["exitCode"], json!(0));
        let text = String::from_utf8_lossy(&output(&terminal.events)).into_owned();
        assert!(text.contains("done"), "output: {text:?}");
    }

    #[test]
    fn a_terminal_request_that_is_cancelled_before_it_starts_is_aborted() {
        let dir = TempDir::new("pty-early-cancel");
        let mut daemon = Daemon::start(&[]);
        daemon.handshake();
        daemon.request(
            1,
            "pty",
            json!({ "cwd": dir.str(), "argv": ["sleep", "100"], "cols": 80, "rows": 24 }),
        );
        daemon.cancel(1, false);
        let reply = daemon.reply(1);
        assert_eq!(reply.kind, common::ERROR);
        assert_eq!(reply.json["code"], json!("aborted"));
    }

    #[test]
    fn output_events_carry_the_request_id() {
        let dir = TempDir::new("pty-ids");
        let mut daemon = Daemon::start(&[]);
        daemon.handshake();
        daemon.request(
            7,
            "pty",
            json!({ "cwd": dir.str(), "argv": ["sh", "-c", "printf x"], "cols": 80, "rows": 24 }),
        );
        let event = daemon.wait_event(7, Duration::from_secs(30));
        assert_eq!(event.id, 7);
        let terminal = run(&mut daemon, 7);
        assert_eq!(terminal.reply.id, 7);
        let _ = EVENT;
    }
}
