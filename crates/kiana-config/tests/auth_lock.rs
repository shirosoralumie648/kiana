//! `proper-lockfile` 等价物（`crates/kiana-config/src/auth/lock.rs`）的规范测试。
//!
//! TS 侧没有对应测试文件（锁行为只通过 auth/settings/trust 的集成测试间接覆盖），
//! 这里按 `spec/ca-config-models.md` §8.3 第 5 条、§2.3 的锁语义逐条断言：
//! 互斥、`<target>.lock` 目录的创建/删除、10×20 ms ELOCKED 重试、非 ELOCKED 错误直通、
//! 注入时钟的 30 s 陈旧锁接管、`onCompromised` 文案、`lockfilePath` 覆盖。
//!
//! 全部使用 `tempfile::tempdir()`，绝不触碰 `~/.kiana`。

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kiana_config::auth::lock::{
    LOCK_COMPROMISED_MESSAGE, LockError, LockOptions, acquire_lock_sync_with_retry_at, lock_async,
    lock_sync, try_lock_async, try_lock_async_with_clock, try_lock_sync,
};
use tokio_util::sync::CancellationToken;

#[test]
fn excludes_other_holders() {
    let directory = tempfile::tempdir().expect("temp dir");
    let target = directory.path().join("auth.json");

    let guard = try_lock_sync(&target, &LockOptions::sync_default()).expect("first lock");
    let error = try_lock_sync(&target, &LockOptions::sync_default()).expect_err("second lock");
    assert!(error.is_locked(), "unexpected error: {error}");
    assert_eq!(error.to_string(), "Lock file is already being held");

    guard.release();
    let guard = try_lock_sync(&target, &LockOptions::sync_default()).expect("lock after release");
    guard.release();
}

#[test]
fn creates_and_removes_the_lock_directory() {
    let directory = tempfile::tempdir().expect("temp dir");
    let target = directory.path().join("settings.json");
    fs::write(&target, "{}").expect("write settings");
    let lock_directory = directory.path().join("settings.json.lock");

    assert!(!lock_directory.exists());
    let guard = try_lock_sync(&target, &LockOptions::sync_default()).expect("lock");
    assert!(lock_directory.is_dir(), "lock must be a directory");
    assert_eq!(guard.path(), lock_directory.as_path());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&lock_directory)
            .expect("lock metadata")
            .permissions()
            .mode();
        // proper-lockfile 用 mkdir 的默认模式（0o777 & ~umask）；owner 必须可读写进入。
        assert_eq!(
            mode & 0o700,
            0o700,
            "unexpected lock directory mode {mode:o}"
        );
    }

    guard.release();
    assert!(
        !lock_directory.exists(),
        "release must remove the lock directory"
    );
}

#[test]
fn retries_elocked_ten_times_then_succeeds() {
    let directory = tempfile::tempdir().expect("temp dir");
    let target = directory.path().join("auth.json");
    let lock_directory = directory.path().join("auth.json.lock");
    fs::create_dir(&lock_directory).expect("held lock");

    let releasing_directory = lock_directory.clone();
    let holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(55));
        fs::remove_dir(&releasing_directory).expect("release held lock");
    });

    let started = Instant::now();
    let guard = lock_sync(&target, &LockOptions::sync_default()).expect("lock after retries");
    holder.join().expect("holder thread");
    // 55 ms 的持锁要求至少两次 20 ms 的退避。
    assert!(
        started.elapsed() >= Duration::from_millis(40),
        "expected ELOCKED retries, took {:?}",
        started.elapsed()
    );
    assert!(lock_directory.is_dir());
    guard.release();
    assert!(!lock_directory.exists());
}

#[test]
fn passes_through_non_elocked_errors() {
    let directory = tempfile::tempdir().expect("temp dir");
    // 父目录不存在 → create_dir 返回 NotFound（不是 ELOCKED），必须立即冒泡。
    let target = directory.path().join("missing-parent").join("auth.json");

    let error = lock_sync(&target, &LockOptions::sync_default()).expect_err("parent is missing");
    assert!(
        matches!(error, LockError::Io { .. }),
        "unexpected error: {error:?}"
    );
    assert_eq!(error.io_kind(), Some(std::io::ErrorKind::NotFound));
}

#[test]
fn locks_the_target_with_a_custom_lockfile_path() {
    // trust manager: 锁的是目录，但锁路径是 `${trust.json}.lock`。
    let directory = tempfile::tempdir().expect("temp dir");
    let trust_directory = directory.path().join("agent");
    fs::create_dir(&trust_directory).expect("trust dir");
    let trust_file = trust_directory.join("trust.json");
    let lockfile_path = trust_directory.join("trust.json.lock");

    let guard =
        acquire_lock_sync_with_retry_at(&trust_directory, &lockfile_path).expect("trust lock");
    assert!(lockfile_path.is_dir());
    assert!(
        !directory.path().join("agent.lock").exists(),
        "default <target>.lock must not be used when lockfilePath is set"
    );
    assert_eq!(trust_file.parent(), Some(trust_directory.as_path()));

    guard.release();
    assert!(!lockfile_path.exists());
}

#[tokio::test]
async fn async_lock_acquires_and_releases() {
    let directory = tempfile::tempdir().expect("temp dir");
    let target = directory.path().join("auth.json");
    let lock_directory = directory.path().join("auth.json.lock");

    let guard = try_lock_async(&target, &LockOptions::async_default())
        .await
        .expect("async lock");
    assert!(lock_directory.is_dir());

    let error = try_lock_async(&target, &LockOptions::async_default())
        .await
        .expect_err("second async lock");
    assert!(error.is_locked(), "unexpected error: {error}");

    guard.release().await;
    assert!(!lock_directory.exists());
}

#[tokio::test]
async fn treats_locks_older_than_thirty_seconds_as_stale_with_injected_clock() {
    let directory = tempfile::tempdir().expect("temp dir");
    let target = directory.path().join("auth.json");
    let lock_directory = directory.path().join("auth.json.lock");
    fs::create_dir(&lock_directory).expect("held lock");

    // 真实时钟：锁是新鲜的，单次尝试必须尊重它。
    let error = try_lock_async(&target, &LockOptions::async_default())
        .await
        .expect_err("fresh lock must be respected");
    assert!(error.is_locked());

    // 注入时钟：now 比锁目录 mtime 晚 31 s（> 30 s stale 阈值）→ 删除并接管。
    let real_now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64;
    let clock = move || real_now_ms + 31_000;
    let guard = try_lock_async_with_clock(&target, &LockOptions::async_default(), &clock)
        .await
        .expect("stale lock is taken over");
    assert!(lock_directory.is_dir());

    guard.release().await;
    assert!(!lock_directory.exists());
}

#[tokio::test]
async fn reports_compromised_when_the_lock_directory_disappears() {
    let directory = tempfile::tempdir().expect("temp dir");
    let target = directory.path().join("auth.json");
    let lock_directory = directory.path().join("auth.json.lock");

    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let callback_captured = Arc::clone(&captured);
    let options = LockOptions::async_default()
        .with_update_ms(5)
        .with_on_compromised(Arc::new(move |error| {
            *callback_captured.lock().expect("callback slot") = Some(error.to_string());
        }));

    let guard = try_lock_async(&target, &options).await.expect("acquire");
    fs::remove_dir(&lock_directory).expect("remove held lock");

    let deadline = Instant::now() + Duration::from_secs(2);
    while captured.lock().expect("callback slot").is_none() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(
        captured.lock().expect("callback slot").as_deref(),
        Some(LOCK_COMPROMISED_MESSAGE)
    );
    assert!(guard.is_compromised());
    assert_eq!(
        guard
            .compromised_error()
            .map(|error| error.to_string())
            .as_deref(),
        Some(LOCK_COMPROMISED_MESSAGE)
    );

    guard.release().await;
    assert!(
        !lock_directory.exists(),
        "compromised release must not recreate the lock"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn async_retry_gives_up_at_the_deadline_without_stealing_a_fresh_lock() {
    use std::time::SystemTime;

    let directory = tempfile::tempdir().expect("temp dir");
    let target = directory.path().join("auth.json");
    let lock_directory = directory.path().join("auth.json.lock");
    fs::create_dir(&lock_directory).expect("held lock");

    // 把 mtime 推到未来，保证 deadline 边缘也不会被陈旧判定接管。
    let lock_file = fs::File::open(&lock_directory).expect("open lock dir");
    lock_file
        .set_times(
            fs::FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(3_600)),
        )
        .expect("set lock mtime");

    let options = LockOptions::async_default().with_stale_ms(80);
    let started = Instant::now();
    let error = lock_async(&target, &options, None)
        .await
        .expect_err("contended lock");
    assert!(error.is_locked(), "unexpected error: {error}");
    assert!(
        started.elapsed() >= Duration::from_millis(60),
        "retry loop should consume the stale budget, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn aborts_when_the_signal_is_cancelled() {
    let directory = tempfile::tempdir().expect("temp dir");
    let target = directory.path().join("auth.json");
    let signal = CancellationToken::new();
    signal.cancel();

    let error = lock_async(&target, &LockOptions::async_default(), Some(&signal))
        .await
        .expect_err("cancelled lock");
    assert!(
        matches!(error, LockError::Aborted),
        "unexpected error: {error}"
    );
    assert!(!directory.path().join("auth.json.lock").exists());
}

// ---------------------------------------------------------------------------
// 回归测试：Node/libuv 错误文案、proper-lockfile 的选项钳制与心跳重试语义。
// ---------------------------------------------------------------------------

/// Node 的 fs 错误文案是 `<ERRNO>: <strerror>, <syscall> '<path>'`
/// （ARCHITECTURE §4.1：公开错误的 `Display` 必须等于 TS 的 `error.message`）。
#[test]
fn formats_io_errors_like_node_libuv() {
    let directory = tempfile::tempdir().expect("temp dir");
    let target = directory.path().join("missing-parent").join("auth.json");

    let error = lock_sync(&target, &LockOptions::sync_default()).expect_err("parent is missing");
    assert!(
        matches!(error, LockError::Io { .. }),
        "unexpected: {error:?}"
    );

    let mut lock_path = target.into_os_string();
    lock_path.push(".lock");
    assert_eq!(
        error.to_string(),
        format!(
            "ENOENT: no such file or directory, mkdir '{}'",
            std::path::PathBuf::from(lock_path).display()
        )
    );
}

/// EACCES 的文案（`permission denied`，小写开头，与 libuv 一致）。
#[cfg(unix)]
#[test]
fn formats_permission_denied_like_node_libuv() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temp dir");
    let readonly = directory.path().join("readonly");
    fs::create_dir(&readonly).expect("create readonly");
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o500))
        .expect("drop write permission");

    let target = readonly.join("auth.json");
    let result = lock_sync(&target, &LockOptions::sync_default());
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o700)).expect("restore permission");

    match result {
        // root 不受权限位约束：跳过（测试仍编译并覆盖普通用户路径）。
        Ok(guard) => guard.release(),
        Err(error) => assert_eq!(
            error.to_string(),
            format!(
                "EACCES: permission denied, mkdir '{}'",
                readonly.join("auth.json.lock").display()
            )
        ),
    }
}

/// proper-lockfile 的 `stale = Math.max(stale || 0, 2000)`：`stale: 5` 必须按 2000 ms 生效。
#[tokio::test]
async fn clamps_stale_thresholds_like_proper_lockfile() {
    let directory = tempfile::tempdir().expect("temp dir");
    let target = directory.path().join("auth.json");
    let lock_directory = directory.path().join("auth.json.lock");
    fs::create_dir(&lock_directory).expect("held lock");

    let real_now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64;
    // 1 s 的偏移：未钳制（5 ms）会判为陈旧并接管，钳制到 2000 ms 后必须保留。
    let clock = move || real_now_ms + 1_000;
    let options = LockOptions::async_default().with_stale_ms(5);

    let error = try_lock_async_with_clock(&target, &options, &clock)
        .await
        .expect_err("lock newer than the clamped stale threshold must be respected");
    assert!(error.is_locked(), "unexpected error: {error}");
    assert!(lock_directory.is_dir());
}

/// `stale: 0` 在 proper-lockfile 里被钳制到 2000 ms，而不是“禁用陈旧判定”。
#[cfg(unix)]
#[test]
fn treats_zero_stale_as_the_proper_lockfile_minimum() {
    let directory = tempfile::tempdir().expect("temp dir");
    let target = directory.path().join("auth.json");
    let lock_directory = directory.path().join("auth.json.lock");
    fs::create_dir(&lock_directory).expect("held lock");

    // mtime = 1970：在钳制后的 2000 ms 阈值下必然陈旧。
    let handle = fs::File::open(&lock_directory).expect("open lock dir");
    handle
        .set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .expect("set lock mtime");
    drop(handle);

    let options = LockOptions::sync_default().with_stale_ms(0);
    let guard = try_lock_sync(&target, &options).expect("stale lock must be taken over");
    assert!(lock_directory.is_dir());
    guard.release();
    assert!(!lock_directory.exists());
}

/// proper-lockfile 的 `update = max(min(update, stale/2), 1000)`：`update: 5` 实际按 1000 ms 运行。
#[tokio::test]
async fn clamps_heartbeat_update_interval_to_one_second() {
    let directory = tempfile::tempdir().expect("temp dir");
    let target = directory.path().join("auth.json");
    let lock_directory = directory.path().join("auth.json.lock");

    let guard = try_lock_async(&target, &LockOptions::async_default().with_update_ms(5))
        .await
        .expect("acquire");
    let initial = fs::metadata(&lock_directory)
        .expect("stat lock dir")
        .modified()
        .expect("lock mtime");

    tokio::time::sleep(Duration::from_millis(300)).await;
    let during = fs::metadata(&lock_directory)
        .expect("stat lock dir")
        .modified()
        .expect("lock mtime");
    assert_eq!(
        during, initial,
        "heartbeat must not run faster than the clamped 1000 ms interval"
    );

    tokio::time::sleep(Duration::from_millis(1_000)).await;
    let after = fs::metadata(&lock_directory)
        .expect("stat lock dir")
        .modified()
        .expect("lock mtime");
    assert!(
        after > during,
        "heartbeat must refresh the mtime after the clamped interval"
    );

    guard.release().await;
}

/// proper-lockfile 的 `updateLock`：stat/utimes 的非 ENOENT 错误在
/// `lastUpdate + stale` 阈值内按 1 s 重试，越过阈值才 `onCompromised`。
#[cfg(unix)]
#[tokio::test]
async fn heartbeat_retries_transient_errors_until_the_stale_threshold() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temp dir");
    let root = directory.path().to_path_buf();
    let target = root.join("auth.json");
    // stale 0 → 钳制到 2000 ms；update 5 → 钳制到 1000 ms。
    let options = LockOptions::async_default()
        .with_stale_ms(0)
        .with_update_ms(5);
    let guard = try_lock_async(&target, &options).await.expect("acquire");
    let lock_directory = root.join("auth.json.lock");

    fs::set_permissions(&root, fs::Permissions::from_mode(0o000))
        .expect("drop search permission on the lock parent");
    if fs::metadata(&lock_directory).is_ok() {
        // root 用户不受权限位约束：本条只在普通用户下有效。
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("restore permission");
        guard.release().await;
        return;
    }

    // 第一次心跳（~1 s）遇到 EACCES：阈值内必须重试，而不是立刻 compromised。
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert!(
        !guard.is_compromised(),
        "transient fs errors must not compromise before the stale threshold"
    );

    // lastUpdate + 2000 ms 之后的重试（~3 s）才判为破坏。
    let deadline = Instant::now() + Duration::from_secs(6);
    while !guard.is_compromised() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("restore permission");

    assert!(
        guard.is_compromised(),
        "heartbeat must compromise after the stale threshold"
    );
    assert_eq!(
        guard
            .compromised_error()
            .map(|error| error.to_string())
            .as_deref(),
        Some(LOCK_COMPROMISED_MESSAGE)
    );

    guard.release().await;
}
