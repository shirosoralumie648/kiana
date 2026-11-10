//! 移植自 `packages/durable/test/storage-runtime-boundary.test.ts`。
//!
//! TS 用源码图检查“可移植的入口不加载 Node 专属实现”。Rust 是单一 crate，模块都挂在
//! `lib.rs` 下，无法逐入口建图，因此按同样的意图检查：
//! 1. crate 根只从 `types` 再导出（可移植核心），不向 `env`/`storage`/`session` 暴露后端；
//! 2. 可移植的 env/范围扫描/类型文件不引用 Node 的进程或文件系统 API；
//! 3. `storage/scan.rs`（内建后端复用的游标逻辑）不依赖任何具体后端。

use std::path::{Path, PathBuf};

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read(relative: &str) -> String {
    let path = source_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn portable_files() -> Vec<(&'static str, String)> {
    let mut files = vec![
        ("types.rs", read("types.rs")),
        ("harness/types.rs", read("harness/types.rs")),
        ("search.rs", read("search.rs")),
        ("truncate.rs", read("truncate.rs")),
        ("timers.rs", read("timers.rs")),
        ("env/mod.rs", read("env/mod.rs")),
        ("env/decode.rs", read("env/decode.rs")),
        ("env/line_scan.rs", read("env/line_scan.rs")),
        ("env/path/mod.rs", read("env/path/mod.rs")),
        ("env/path/posix.rs", read("env/path/posix.rs")),
        ("env/path/win32.rs", read("env/path/win32.rs")),
        ("storage/scan.rs", read("storage/scan.rs")),
    ];
    files.sort_by(|left, right| left.0.cmp(right.0));
    files
}

#[test]
fn keeps_the_package_root_limited_to_portable_core() {
    // 只看生产代码：`lib.rs` 的模块文档会提到 `env::node`/`storage::jsonl` 作为说明，
    // 这些不是根导出。
    let lib = code_only(&read("lib.rs"));
    for line in lib.lines() {
        let line = line.trim();
        if line.starts_with("pub use ") {
            assert!(
                line.starts_with("pub use types::"),
                "root re-export escapes the portable core: {line}"
            );
        }
    }
    assert!(
        !lib.contains("env::node"),
        "root must not reach the Node environment"
    );
    assert!(
        !lib.contains("storage::jsonl"),
        "root must not reach the JSONL backend"
    );
    assert!(
        !lib.contains("storage::sqlite"),
        "root must not reach the SQLite backend"
    );
}

/// 去掉行注释与 `#[cfg(test)]` 之后的测试代码：TS 的检查用 import/export 正则，
/// 注释与测试内的读取都不算生产依赖。
fn code_only(source: &str) -> String {
    let production = source.split("#[cfg(test)]").next().unwrap_or(source);
    production
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn portable_modules_avoid_node_apis() {
    for (name, source) in portable_files() {
        let source = code_only(&source);
        assert!(
            !source.contains("node:"),
            "{name} mentions a Node built-in specifier"
        );
        assert!(
            !source.contains("std::process::Command"),
            "{name} spawns a process"
        );
        assert!(
            !source.contains("tokio::process"),
            "{name} depends on the Node-like process backend"
        );
        assert!(
            !source.contains("std::fs::"),
            "{name} uses the file system directly"
        );
    }
}

#[test]
fn portable_scan_helpers_stay_backend_free() {
    let scan = read("storage/scan.rs");
    for backend in ["jsonl", "sqlite", "memory", "indexeddb", "session"] {
        assert!(
            !scan.contains(&format!("crate::storage::{backend}")),
            "storage/scan.rs depends on the {backend} backend"
        );
    }
}
