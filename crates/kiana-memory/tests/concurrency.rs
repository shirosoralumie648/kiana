//! 真实跨进程并发测试：写锁超时与抽取 lease 的唯一 claim。

use std::process::{Command, Stdio};
use std::time::Duration;

use kiana_memory::{
    ClaimOutcome, FileExtractionStore, FileLock, MemoryConfig, MemoryScope, MemoryStore, ScopePaths,
};

fn helper() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_memory_test_helper"))
}

fn run_helper(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(helper())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn helper");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn store_at(root: &std::path::Path) -> MemoryStore {
    let paths = ScopePaths::at_root(MemoryScope::Project, root);
    MemoryStore::open(paths, MemoryConfig::default()).unwrap()
}

#[test]
fn losing_writer_times_out_and_winner_entry_is_parseable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let paths = ScopePaths::at_root(MemoryScope::Project, &root);
    paths.ensure_dirs().unwrap();

    // 父进程持有写锁，模拟另一个正在写的进程。
    let lock = FileLock::acquire(&paths.lock_path, Duration::from_secs(2)).unwrap();
    let (code, stdout, _stderr) = run_helper(&[
        "write",
        root.to_str().unwrap(),
        "loser",
        "loser body",
        "300",
    ]);
    assert_eq!(code, 2, "loser must exit with the write error code");
    assert!(stdout.contains("lock timed out"), "loser output: {stdout}");

    // 释放后同一个 helper 成功写入；索引必须仍可解析且胜者条目存在。
    drop(lock);
    let (code, stdout, stderr) = run_helper(&[
        "write",
        root.to_str().unwrap(),
        "winner",
        "winner body",
        "3000",
    ]);
    assert_eq!(code, 0, "winner failed: {stdout} {stderr}");
    assert!(stdout.contains("ok"), "winner output: {stdout}");

    let store = store_at(&root);
    let snapshot = store.load_snapshot().unwrap();
    assert_eq!(snapshot.revision, 1);
    let entries = store.list_entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "winner");
}

#[test]
fn two_processes_writing_concurrently_do_not_corrupt_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let mut children = Vec::new();
    for name in ["alpha", "beta", "gamma", "delta"] {
        children.push(
            Command::new(helper())
                .args([
                    "write",
                    root.to_str().unwrap(),
                    name,
                    "concurrent body",
                    "5000",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "child failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let store = store_at(&root);
    let entries = store.list_entries().unwrap();
    let mut names: Vec<String> = entries.iter().map(|entry| entry.name.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["alpha", "beta", "delta", "gamma"]);
    kiana_memory::parse_index(&store.load_snapshot().unwrap().index).unwrap();
}

#[test]
fn simultaneous_extraction_claim_has_exactly_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    ScopePaths::at_root(MemoryScope::Project, &root)
        .ensure_dirs()
        .unwrap();

    let first = Command::new(helper())
        .args(["claim", root.to_str().unwrap(), "owner-a", "1500", "5000"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let second = Command::new(helper())
        .args(["claim", root.to_str().unwrap(), "owner-b", "1500", "5000"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let first_output = first.wait_with_output().unwrap();
    let second_output = second.wait_with_output().unwrap();
    let outputs = [
        String::from_utf8_lossy(&first_output.stdout).to_string(),
        String::from_utf8_lossy(&second_output.stdout).to_string(),
    ];
    let claimed = outputs
        .iter()
        .filter(|output| output.starts_with("claimed"))
        .count();
    let busy = outputs
        .iter()
        .filter(|output| output.starts_with("busy"))
        .count();
    assert_eq!(claimed, 1, "outputs: {outputs:?}");
    assert_eq!(busy, 1, "outputs: {outputs:?}");

    // 胜者退出时释放 lease，之后可以再次 claim。
    let paths = ScopePaths::at_root(MemoryScope::Project, &root);
    let state = FileExtractionStore::for_scope(&paths, Duration::from_secs(2));
    match state.claim("owner-c", 5_000).unwrap() {
        ClaimOutcome::Claimed { .. } => {}
        other => panic!("expected claimable state, got {other:?}"),
    }
}
