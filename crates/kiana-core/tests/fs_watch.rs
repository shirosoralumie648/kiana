//! `utils/fs-watch.ts` 的测试（TS 无对应测试文件）。
//!
//! 用 `tempfile` 目录，绝不触碰真实 `~`（`ARCHITECTURE.md` §4.8 第 8 条）。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kiana_core::fs_watch::{
    FS_WATCH_RETRY_DELAY_MS, WatchEventType, close_watcher, watch_with_error_handler,
};

#[test]
fn retry_delay_matches_typescript() {
    assert_eq!(FS_WATCH_RETRY_DELAY_MS, 5000);
}

#[test]
fn event_type_uses_the_node_strings() {
    assert_eq!(WatchEventType::Rename.as_str(), "rename");
    assert_eq!(WatchEventType::Change.as_str(), "change");
    assert_eq!(WatchEventType::Rename.to_string(), "rename");
    assert_eq!(WatchEventType::Change.to_string(), "change");
}

#[test]
fn reports_events_for_a_watched_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("watched.txt");
    std::fs::write(&file, b"one").expect("seed file");

    let (sender, receiver) = mpsc::channel();
    let errors = Arc::new(AtomicUsize::new(0));
    let error_counter = Arc::clone(&errors);
    let watcher = watch_with_error_handler(
        dir.path(),
        move |event_type, filename| {
            let _ = sender.send((event_type, filename));
        },
        move || {
            error_counter.fetch_add(1, Ordering::SeqCst);
        },
    )
    .expect("watcher is created");

    std::fs::write(&file, b"two").expect("modify file");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = false;
    while Instant::now() < deadline {
        match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok((_, Some(name))) if name == "watched.txt" => {
                seen = true;
                break;
            }
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(seen, "expected an event naming watched.txt");
    assert_eq!(errors.load(Ordering::SeqCst), 0);
    close_watcher(Some(watcher));
}

#[test]
fn reports_events_for_a_watched_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("tables.list");
    std::fs::write(&file, b"refs").expect("seed file");

    let (sender, receiver) = mpsc::channel();
    let watcher = watch_with_error_handler(
        &file,
        move |_event_type, filename| {
            let _ = sender.send(filename);
        },
        || {},
    )
    .expect("watcher is created");

    std::fs::write(&file, b"refs changed").expect("modify file");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = false;
    while Instant::now() < deadline {
        match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(Some(name)) if name == "tables.list" => {
                seen = true;
                break;
            }
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(seen, "expected an event naming tables.list");
    close_watcher(Some(watcher));
}

#[test]
fn synchronous_watch_failure_calls_the_error_handler_and_returns_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("does-not-exist");
    let errors = Arc::new(AtomicUsize::new(0));
    let error_counter = Arc::clone(&errors);

    let watcher = watch_with_error_handler(
        &missing,
        |_, _| {},
        move || {
            error_counter.fetch_add(1, Ordering::SeqCst);
        },
    );

    assert!(watcher.is_none(), "watching a missing path must fail");
    assert!(
        errors.load(Ordering::SeqCst) >= 1,
        "onError must be invoked"
    );
}

#[test]
fn close_watcher_accepts_none_and_stops_delivering_events() {
    close_watcher(None);

    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("watched.txt");
    std::fs::write(&file, b"one").expect("seed file");

    let (sender, receiver) = mpsc::channel();
    let watcher = watch_with_error_handler(
        dir.path(),
        move |_event_type, filename| {
            let _ = sender.send(filename);
        },
        || {},
    )
    .expect("watcher is created");

    close_watcher(Some(watcher));

    std::fs::write(&file, b"two").expect("modify after close");
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(filename) => {
                assert_ne!(
                    filename.as_deref(),
                    Some("watched.txt"),
                    "closed watcher still delivered"
                );
            }
            Err(_) => break,
        }
    }
}
