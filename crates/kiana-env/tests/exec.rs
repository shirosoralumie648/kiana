//! `exec`（docs/semantics.md）：输出事件、exitCode、超时、取消、spill 文件、输出窗口。
//!
//! 对应 packages/env/test/remote.test.ts 里依赖 daemon 的断言（窗口 5 MB、20 次立即取消、无输出超时）。

mod common;

use common::{Daemon, EVENT, TempDir, assert_error, error};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// 收集一个请求的事件与最终帧。
fn collect(daemon: &mut Daemon, id: u32) -> (Vec<Value>, Vec<u8>, Value) {
    let (events, reply) = daemon.reply_with_events(id);
    let payload: Vec<u8> = events
        .iter()
        .flat_map(|event| event.payload.clone())
        .collect();
    let jsons = events.iter().map(|event| event.json.clone()).collect();
    match reply.kind {
        common::RESULT => (jsons, payload, reply.json),
        _ => (jsons, payload, reply.json),
    }
}

#[test]
fn sends_stdout_and_stderr_apart_with_the_exit_code() {
    let dir = TempDir::new("exec-basic");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "exec",
        json!({ "argv": ["sh", "-c", "printf out; printf err >&2; exit 3"], "cwd": dir.str() }),
    );
    let (events, _, result) = collect(&mut daemon, 1);
    assert_eq!(result, json!({ "exitCode": 3 }));
    assert_eq!(events.len(), 2);
    let streams: Vec<&str> = events
        .iter()
        .map(|event| event["stream"].as_str().unwrap())
        .collect();
    assert!(streams.contains(&"stdout"));
    assert!(streams.contains(&"stderr"));
    for event in &events {
        assert_eq!(event["kind"], json!("output"));
        assert!(event.get("skipped").is_none());
    }
}

#[test]
fn a_string_command_runs_through_the_shell() {
    let dir = TempDir::new("exec-shell");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(1, "exec", json!({ "command": "echo hi", "cwd": dir.str() }));
    let (_, payload, result) = collect(&mut daemon, 1);
    assert_eq!(result, json!({ "exitCode": 0 }));
    assert_eq!(payload, b"hi\n");
}

#[test]
fn a_command_killed_by_a_signal_reports_128_plus_the_signal() {
    let dir = TempDir::new("exec-signal");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "exec",
        json!({ "argv": ["sh", "-c", "kill -TERM $$"], "cwd": dir.str() }),
    );
    let (_, _, result) = collect(&mut daemon, 1);
    assert_eq!(result["exitCode"], json!(143));
}

#[test]
fn environment_and_inherit_env_follow_node() {
    let dir = TempDir::new("exec-env");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "exec",
        json!({ "command": "printf %s \"$FOO\"", "cwd": dir.str(), "env": { "FOO": "bar" } }),
    );
    let (_, payload, _) = collect(&mut daemon, 1);
    assert_eq!(payload, b"bar");
    daemon.request(
        2,
        "exec",
        json!({
            "argv": ["sh", "-c", "printf %s \"${HOME:-none}\""],
            "cwd": dir.str(),
            "env": {},
            "inheritEnv": false,
        }),
    );
    let (_, payload, _) = collect(&mut daemon, 2);
    assert_eq!(payload, b"none");
}

#[test]
fn reports_the_documented_spawn_errors() {
    let dir = TempDir::new("exec-errors");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let missing = dir.join("missing");
    assert_error(
        &daemon.call_error(1, "exec", json!({ "command": "true", "cwd": missing })),
        "spawn_error",
        &format!("Working directory does not exist: {missing}\nCannot execute bash commands."),
    );
    assert_error(
        &daemon.call_error(2, "exec", json!({ "argv": [], "cwd": dir.str() })),
        "spawn_error",
        "Empty argv: no program to run",
    );
    assert_error(
        &daemon.call_error(3, "exec", json!({})),
        "EINVAL",
        "exec needs cwd",
    );
    assert_error(
        &daemon.call_error(4, "exec", json!({ "cwd": dir.str() })),
        "EINVAL",
        "exec needs command or argv",
    );
    assert_error(
        &daemon.call_error(
            5,
            "exec",
            json!({ "command": "true", "cwd": dir.str(), "shellPath": "/does/not/exist" }),
        ),
        "shell_unavailable",
        "Custom shell path not found: /does/not/exist",
    );
    let failure = daemon.call_error(
        6,
        "exec",
        json!({ "argv": ["/does/not/exist"], "cwd": dir.str() }),
    );
    assert_eq!(failure["code"], json!("spawn_error"));
}

#[test]
fn times_out_and_still_reports_the_spill_path() {
    let dir = TempDir::new("exec-timeout");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "exec",
        json!({
            "argv": ["sh", "-c", "printf 'a\\nb\\nc\\n'; sleep 5"],
            "cwd": dir.str(),
            "timeoutMs": 100,
            "spill": { "afterBytes": 2, "afterLines": 1 },
        }),
    );
    let started = Instant::now();
    let (_, _, failure) = collect(&mut daemon, 1);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the timeout is honoured"
    );
    assert_error(&failure, "timeout", "timeout");
    let spill = failure["spillPath"].as_str().expect("spillPath");
    let content = std::fs::read(spill).expect("spill file");
    assert_eq!(content, b"a\nb\nc\n");
    assert!(
        spill.contains("/tmp-"),
        "spill file lives in a fresh tmp- directory"
    );
    assert!(spill.ends_with(".log"));
    assert!(spill.contains("kiana-output-"));
    let _ = error;
}

#[test]
fn a_spill_below_the_threshold_leaves_no_file() {
    let dir = TempDir::new("exec-nospill");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "exec",
        json!({
            "argv": ["sh", "-c", "printf hello"],
            "cwd": dir.str(),
            "spill": { "afterBytes": 1000, "afterLines": 1000 },
        }),
    );
    let (_, _, result) = collect(&mut daemon, 1);
    assert_eq!(result, json!({ "exitCode": 0 }));
}

#[test]
fn an_immediate_cancel_settles_as_aborted() {
    let dir = TempDir::new("exec-cancel");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    // The daemon registers each request before running it, so a cancel right behind it is never lost.
    let started = Instant::now();
    for attempt in 0..5u32 {
        let id = attempt + 1;
        daemon.request(
            id,
            "exec",
            json!({ "argv": ["sleep", "5"], "cwd": dir.str() }),
        );
        daemon.cancel(id, false);
        let failure = error(daemon.reply(id));
        assert_eq!(failure["code"], json!("aborted"), "attempt {attempt}");
        assert_eq!(failure["message"], json!("aborted"));
    }
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_kill_cancel_settles_with_the_killed_process_status() {
    let dir = TempDir::new("exec-kill");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "exec",
        json!({ "argv": ["sleep", "5"], "cwd": dir.str() }),
    );
    std::thread::sleep(Duration::from_millis(200));
    daemon.cancel(1, true);
    // `cleanup()` kills without aborting: the command settles with the killed process's status (137).
    let (_, _, result) = collect(&mut daemon, 1);
    assert_eq!(result["exitCode"], json!(137));
}

#[test]
fn times_out_a_command_that_produces_no_output() {
    let dir = TempDir::new("exec-quiet-timeout");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let started = Instant::now();
    let failure = daemon.call_error(
        1,
        "exec",
        json!({ "argv": ["sleep", "5"], "cwd": dir.str(), "timeoutMs": 300 }),
    );
    assert_error(&failure, "timeout", "timeout");
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn transfers_only_about_a_window_of_output_with_exact_counts() {
    // packages/env/test/remote.test.ts, "transfers only about a window of output a caller keeps, with exact counts".
    let dir = TempDir::new("exec-window");
    let total = 5_000_000usize;
    let full: String = "0123456789abcdef\n".repeat(total.div_ceil(17));
    let full = &full[..total];
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "exec",
        json!({
            "argv": ["sh", "-c", format!("yes 0123456789abcdef | head -c {total}")],
            "cwd": dir.str(),
            "window": { "maxBytes": 1000, "maxLines": 20, "minIntervalMs": 20, "bytesPerSecond": 1_000_000 },
        }),
    );
    let (events, reply) = daemon.reply_with_events(1);
    assert_eq!(reply.kind, common::RESULT);
    assert_eq!(reply.json["exitCode"], json!(0));
    let mut counted = 0usize;
    let mut skipped = 0u64;
    let mut after_last_skip = Vec::new();
    let mut saw_skip = false;
    for event in &events {
        assert_eq!(event.json["kind"], json!("output"));
        if let Some(entry) = event.json.get("skipped") {
            saw_skip = true;
            let bytes = entry["bytes"].as_u64().expect("skipped.bytes");
            skipped += bytes;
            counted += bytes as usize;
            assert!(entry["newlines"].is_u64());
            assert!(entry["endsWithNewline"].is_boolean());
            after_last_skip.clear();
        }
        counted += event.payload.len();
        after_last_skip.extend_from_slice(&event.payload);
    }
    assert!(
        saw_skip,
        "output followed by more than the window is skipped"
    );
    assert_eq!(counted, total, "delivered plus skipped covers the stream");
    assert!(skipped > 0);
    let delivered = total - skipped as usize;
    assert!(delivered < total / 10, "delivered {delivered} of {total}");
    let tail = String::from_utf8_lossy(&after_last_skip).into_owned();
    assert!(full.ends_with(&tail));
    assert!(
        tail.len() > 1000 || tail.matches('\n').count() > 20,
        "the tail after the last skip is more than the window"
    );
}

#[test]
fn a_window_without_skips_delivers_each_stream_apart() {
    let dir = TempDir::new("exec-window-small");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "exec",
        json!({
            "argv": ["sh", "-c", "printf a; printf b >&2; printf c"],
            "cwd": dir.str(),
            "window": { "maxBytes": 1000, "maxLines": 1000, "minIntervalMs": 0, "bytesPerSecond": 1e12 },
        }),
    );
    let (events, reply) = daemon.reply_with_events(1);
    assert_eq!(reply.kind, common::RESULT);
    // The two pipes are read concurrently, so only each stream's own order is fixed.
    let mut stdout = String::new();
    let mut stderr = String::new();
    for event in &events {
        assert!(event.json.get("skipped").is_none());
        let text = String::from_utf8_lossy(&event.payload).into_owned();
        match event.json["stream"].as_str().unwrap() {
            "stdout" => stdout.push_str(&text),
            "stderr" => stderr.push_str(&text),
            other => panic!("unexpected stream {other}"),
        }
    }
    assert_eq!((stdout.as_str(), stderr.as_str()), ("ac", "b"));
}

#[test]
fn ignores_a_window_that_is_not_an_object() {
    let dir = TempDir::new("exec-bad-window");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "exec",
        json!({ "argv": ["sh", "-c", "printf hi"], "cwd": dir.str(), "window": { "maxBytes": 10 } }),
    );
    let (events, reply) = daemon.reply_with_events(1);
    assert_eq!(reply.kind, common::RESULT);
    let text: String = events
        .iter()
        .map(|event| String::from_utf8_lossy(&event.payload).into_owned())
        .collect();
    // An incomplete window is no window: output is not coalesced.
    assert_eq!(text, "hi");
}

#[test]
fn a_large_stdout_arrives_in_order() {
    let dir = TempDir::new("exec-big");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "exec",
        json!({ "argv": ["sh", "-c", "seq 1 20000"], "cwd": dir.str() }),
    );
    let (_, payload, result) = collect(&mut daemon, 1);
    assert_eq!(result["exitCode"], json!(0));
    let expected: String = (1..=20000).map(|n| format!("{n}\n")).collect();
    assert_eq!(payload, expected.as_bytes());
}

#[test]
fn events_carry_the_request_id() {
    let dir = TempDir::new("exec-ids");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        77,
        "exec",
        json!({ "argv": ["sh", "-c", "printf x"], "cwd": dir.str() }),
    );
    let event = daemon.wait_event(77, Duration::from_secs(10));
    assert_eq!(event.kind, EVENT);
    let (_events, reply) = daemon.reply_with_events(77);
    assert_eq!(reply.id, 77);
}
