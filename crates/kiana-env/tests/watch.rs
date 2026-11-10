//! `watch`（docs/protocol.md、docs/semantics.md）：`ready`/`change` 事件、polling 模式、取消后正常结束、
//! error 事件、目录预算。

mod common;

use common::{Daemon, EVENT, TempDir, assert_error, error};
use serde_json::json;
use std::time::{Duration, Instant};

/// 等 `watch` 请求的 `ready` 事件并返回它的 JSON。
fn ready(daemon: &mut Daemon, id: u32) -> serde_json::Value {
    let event = daemon.wait_event(id, Duration::from_secs(30));
    assert_eq!(event.json["kind"], json!("ready"), "event {:?}", event.json);
    event.json
}

/// 等一个 `change` 事件（跳过其它事件），返回它的 JSON。
fn change(daemon: &mut Daemon, id: u32, timeout: Duration) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let event = daemon.wait_event(id, remaining);
        if event.json["kind"] == json!("change") {
            return event.json;
        }
    }
}

/// 轮询直到 `change` 事件提到 `paths`（原生事件可能晚到）。
fn change_mentioning(daemon: &mut Daemon, id: u32, path: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let event = change(daemon, id, remaining);
        if let Some(paths) = event["paths"].as_array()
            && paths.iter().any(|candidate| candidate == path)
        {
            return event;
        }
    }
}

#[test]
fn reports_changes_and_finishes_with_an_empty_result_when_cancelled() {
    let dir = TempDir::new("watch-native");
    dir.write("a.txt", b"one");
    let root = dir.str();
    let file = dir.join("a.txt");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "watch",
        json!({ "targets": [{ "path": root, "recursive": true }] }),
    );
    let event = ready(&mut daemon, 1);
    assert_eq!(event["mode"], json!("native"));
    std::fs::write(&file, b"two").unwrap();
    let event = change_mentioning(&mut daemon, 1, &file);
    assert!(event.get("overflow").is_none());
    // A new file below the watched directory also shows up.
    dir.write("b.txt", b"new");
    change_mentioning(&mut daemon, 1, &dir.join("b.txt"));
    // Cancellation ends the watcher normally: `{}`, not an aborted error.
    daemon.cancel(1, false);
    let result = daemon.reply(1);
    assert_eq!(result.kind, common::RESULT);
    assert_eq!(result.json, json!({}));
}

#[test]
fn polling_mode_scans_on_its_interval() {
    let dir = TempDir::new("watch-polling");
    dir.write("a.txt", b"one");
    let file = dir.join("a.txt");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "watch",
        json!({
            "targets": [{ "path": dir.str(), "recursive": false }],
            "mode": "polling",
            "pollIntervalMs": 100,
        }),
    );
    let event = ready(&mut daemon, 1);
    assert_eq!(event["mode"], json!("polling"));
    std::fs::write(&file, b"two").unwrap();
    change_mentioning(&mut daemon, 1, &file);
    daemon.cancel(1, false);
    assert_eq!(daemon.reply(1).json, json!({}));
}

#[test]
fn reports_only_changes_inside_the_targets() {
    let dir = TempDir::new("watch-scope");
    dir.write("inside/a.txt", b"one");
    dir.write("outside/b.txt", b"one");
    let inside = dir.join("inside/a.txt");
    let outside = dir.join("outside/b.txt");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "watch",
        json!({
            "targets": [{ "path": dir.join("inside"), "recursive": true }],
            "mode": "polling",
            "pollIntervalMs": 100,
        }),
    );
    ready(&mut daemon, 1);
    std::fs::write(&outside, b"two").unwrap();
    std::fs::write(&inside, b"two").unwrap();
    let event = change(&mut daemon, 1, Duration::from_secs(30));
    let paths = event["paths"].as_array().expect("paths");
    assert!(paths.iter().any(|path| path == &json!(inside)));
    assert!(!paths.iter().any(|path| path == &json!(outside)));
    daemon.cancel(1, false);
    daemon.reply(1);
}

#[test]
fn exclude_hidden_and_names_are_honoured() {
    let dir = TempDir::new("watch-exclude");
    dir.write("keep.txt", b"one");
    dir.write("skip.txt", b"one");
    dir.write(".hidden.txt", b"one");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "watch",
        json!({
            "targets": [{
                "path": dir.str(),
                "recursive": true,
                "exclude": { "hidden": true, "names": ["skip.txt"] },
            }],
            "mode": "polling",
            "pollIntervalMs": 100,
        }),
    );
    ready(&mut daemon, 1);
    std::fs::write(dir.join("keep.txt"), b"two").unwrap();
    std::fs::write(dir.join("skip.txt"), b"two").unwrap();
    std::fs::write(dir.join(".hidden.txt"), b"two").unwrap();
    let event = change_mentioning(&mut daemon, 1, &dir.join("keep.txt"));
    let paths = event["paths"].as_array().unwrap();
    assert!(
        !paths
            .iter()
            .any(|path| path == &json!(dir.join("skip.txt")))
    );
    assert!(
        !paths
            .iter()
            .any(|path| path == &json!(dir.join(".hidden.txt")))
    );
    daemon.cancel(1, false);
    daemon.reply(1);
}

#[test]
fn a_missing_target_waits_to_be_created() {
    let dir = TempDir::new("watch-created");
    let file = dir.join("later.txt");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "watch",
        json!({
            "targets": [{ "path": file, "recursive": false }],
            "mode": "polling",
            "pollIntervalMs": 100,
        }),
    );
    ready(&mut daemon, 1);
    dir.write("later.txt", b"now");
    change_mentioning(&mut daemon, 1, &file);
    daemon.cancel(1, false);
    daemon.reply(1);
}

#[test]
fn a_directory_budget_reports_an_error() {
    let dir = TempDir::new("watch-budget");
    dir.write("a/b/c/f.txt", b"");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let failure = daemon.call_error(
        1,
        "watch",
        json!({
            "targets": [{ "path": dir.str(), "recursive": true }],
            "mode": "polling",
            "maxDirectories": 1,
        }),
    );
    assert_error(&failure, "EINVAL", "Watched paths exceed 1 directories");
}

#[test]
fn requires_targets() {
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    assert_error(
        &daemon.call_error(1, "watch", json!({})),
        "EINVAL",
        "watch needs targets",
    );
    assert_error(
        &daemon.call_error(2, "watch", json!({ "targets": [{}] })),
        "EINVAL",
        "watch needs targets",
    );
    // `exclude.names` drops non-string entries instead of failing.
    let dir = TempDir::new("watch-loose");
    dir.write("a.txt", b"");
    daemon.request(
        3,
        "watch",
        json!({
            "targets": [{ "path": dir.str(), "exclude": { "names": ["a.txt", 7] } }],
            "mode": "polling",
            "pollIntervalMs": 100,
        }),
    );
    let event = daemon.wait_event(3, Duration::from_secs(30));
    assert_eq!(event.json["kind"], json!("ready"));
    daemon.cancel(3, false);
    daemon.reply(3);
}

/// 只在 unix 上改权限；Windows 没有 0o000 这回事（root 也能读，用例随后直接返回）。
fn permission_mode(path: &str, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(mode);
        std::fs::set_permissions(path, permissions).unwrap();
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

#[test]
fn an_unreadable_target_reports_an_error() {
    let dir = TempDir::new("watch-denied");
    dir.write("locked/a.txt", b"");
    let locked = dir.join("locked");
    permission_mode(&locked, 0o000);
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let failure = daemon.call_error(
        1,
        "watch",
        json!({
            "targets": [{ "path": locked, "recursive": true }],
            "mode": "polling",
            "pollIntervalMs": 100,
        }),
    );
    permission_mode(&locked, 0o700);
    // Running as root (CI containers) can read it anyway; then the watcher simply starts.
    if failure["code"] == json!("permission_denied") || failure["code"] == json!("EACCES") {
        return;
    }
    assert_eq!(failure["code"], json!("EACCES"), "failure {failure}");
    assert!(failure["message"].is_string());
    let _ = error;
}

#[test]
fn a_target_that_is_a_file_is_watched_directly() {
    let dir = TempDir::new("watch-file");
    dir.write("a.txt", b"one");
    let file = dir.join("a.txt");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(1, "watch", json!({ "targets": [{ "path": file }] }));
    ready(&mut daemon, 1);
    std::fs::write(&file, b"two").unwrap();
    change_mentioning(&mut daemon, 1, &file);
    daemon.cancel(1, false);
    daemon.reply(1);
}

#[test]
fn removing_a_watched_file_is_a_change() {
    let dir = TempDir::new("watch-remove");
    dir.write("a.txt", b"one");
    let file = dir.join("a.txt");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        1,
        "watch",
        json!({ "targets": [{ "path": dir.str(), "recursive": true }], "mode": "polling", "pollIntervalMs": 100 }),
    );
    ready(&mut daemon, 1);
    std::fs::remove_file(&file).unwrap();
    change_mentioning(&mut daemon, 1, &file);
    daemon.cancel(1, false);
    daemon.reply(1);
}

#[test]
fn events_of_a_watch_request_carry_its_id() {
    let dir = TempDir::new("watch-id");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(
        9,
        "watch",
        json!({ "targets": [{ "path": dir.str() }], "mode": "polling" }),
    );
    let event = daemon.wait_event(9, Duration::from_secs(30));
    assert_eq!(event.kind, EVENT);
    assert_eq!(event.id, 9);
    daemon.cancel(9, false);
    daemon.reply(9);
}
