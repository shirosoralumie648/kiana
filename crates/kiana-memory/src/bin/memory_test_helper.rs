//! 并发测试辅助进程。仅供 `tests/` 使用；不属于产品 API。
//!
//! 用法：
//!
//! ```text
//! memory_test_helper write <memory-root> <name> <body> <lock-timeout-ms>
//! memory_test_helper hold-lock <memory-root> <hold-ms>
//! memory_test_helper claim <memory-root> <owner> <hold-ms> <ttl-ms>
//! ```
//!
//! `write` 在锁竞争失败时退出码 2，stdout 打印 `err lock timed out`。
//! `claim` 打印 `claimed <expires>` 或 `busy <owner>`，退出码始终 0（除非参数/IO 错误）。

use std::process::ExitCode;
use std::time::Duration;

use kiana_memory::{
    ClaimOutcome, FileExtractionStore, FileLock, MemoryConfig, MemoryEntryDraft, MemoryScope,
    MemoryService, MemoryStore, MemoryType, ScopePaths, WriteBatch, WriteSource,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            println!("err {message}");
            ExitCode::from(code)
        }
    }
}

fn run(args: &[String]) -> Result<(), (u8, String)> {
    match args.first().map(String::as_str) {
        Some("write") => {
            let root = arg(args, 1)?;
            let name = arg(args, 2)?;
            let body = arg(args, 3)?;
            let timeout_ms = arg(args, 4)?
                .parse::<u64>()
                .map_err(|_| (64, "lock-timeout-ms must be an integer".to_string()))?;
            let config = MemoryConfig {
                lock_timeout: Duration::from_millis(timeout_ms),
                ..MemoryConfig::default()
            };
            let paths = ScopePaths::at_root(MemoryScope::Project, root);
            let store = MemoryStore::open(paths, config).map_err(|error| (1, error.to_string()))?;
            let service = MemoryService::trusted(store);
            let report = service
                .apply(
                    WriteBatch::new().add(MemoryEntryDraft::new(
                        name,
                        "helper-created entry",
                        MemoryType::User,
                        body,
                    )),
                    WriteSource::Test,
                )
                .map_err(|error| (2, error.to_string()))?;
            println!("ok {} {}", report.revision, report.applied.len());
            Ok(())
        }
        Some("hold-lock") => {
            let root = arg(args, 1)?;
            let hold_ms = arg(args, 2)?
                .parse::<u64>()
                .map_err(|_| (64, "hold-ms must be an integer".to_string()))?;
            let paths = ScopePaths::at_root(MemoryScope::Project, root);
            paths
                .ensure_dirs()
                .map_err(|error| (1, error.to_string()))?;
            let lock = FileLock::acquire(&paths.lock_path, Duration::from_secs(2))
                .map_err(|error| (2, error.to_string()))?;
            println!("locked");
            std::thread::sleep(Duration::from_millis(hold_ms));
            drop(lock);
            println!("released");
            Ok(())
        }
        Some("claim") => {
            let root = arg(args, 1)?;
            let owner = arg(args, 2)?;
            let hold_ms = arg(args, 3)?
                .parse::<u64>()
                .map_err(|_| (64, "hold-ms must be an integer".to_string()))?;
            let ttl_ms = arg(args, 4)?
                .parse::<u64>()
                .map_err(|_| (64, "ttl-ms must be an integer".to_string()))?;
            let paths = ScopePaths::at_root(MemoryScope::Project, root);
            paths
                .ensure_dirs()
                .map_err(|error| (1, error.to_string()))?;
            let state = FileExtractionStore::for_scope(&paths, Duration::from_secs(2));
            match state
                .claim(&owner, ttl_ms)
                .map_err(|error| (1, error.to_string()))?
            {
                ClaimOutcome::Claimed { lease, .. } => {
                    println!(
                        "claimed {} {} {}",
                        lease.owner, lease.expires_at_ms, lease.attempt
                    );
                    std::thread::sleep(Duration::from_millis(hold_ms));
                    let _ = state.release(&owner);
                }
                ClaimOutcome::Busy { lease } => {
                    println!("busy {} {}", lease.owner, lease.expires_at_ms);
                }
            }
            Ok(())
        }
        _ => Err((64, "usage: write|hold-lock|claim ...".to_string())),
    }
}

fn arg(args: &[String], index: usize) -> Result<String, (u8, String)> {
    args.get(index)
        .cloned()
        .ok_or_else(|| (64, format!("missing argument {}", index + 1)))
}
