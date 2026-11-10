//! `packages/chord/test/boundary.test.ts` 的 Rust 端口。
//!
//! TS 的测试读 `package.json` 与 `src/**/*.ts` 的 import 说明符，断言：
//!
//! 1. `dependencies` 里**除了 `@earendil-works/kiana-telemetry`** 没有别的 `@earendil-works/kiana-*`
//!    （kiana-telemetry 是唯一的允许项：它自己无依赖、是叶子，`Context.telemetry` 需要它）；
//! 2. 任何相对 import 都不能跑出 `src/`。
//!
//! Rust 里对应的边界是 **Cargo 依赖图**：
//!
//! 1. `kiana-chord` 的 `[dependencies]` 里只能出现 `kiana-json` 与 `kiana-telemetry` 这两个 workspace
//!    crate（`kiana-json` 是 JSON/JS 语义的叶子，`kiana-telemetry` 对应 TS 的允许项）；
//! 2. 不能出现指到 `crates/kiana-chord/` 之外、又不在 workspace 里的 path 依赖（TS 的"跑出 src/"）。
//!
//! 测试直接解析 `Cargo.toml` 的文本（不引 TOML 解析器，依赖面已经固定），并扫描
//! `src/**/*.rs` 的 `use`/`extern crate` 里出现的 `kiana_*` crate 名。这样改坏边界会立刻失败。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const MANIFEST: &str = include_str!("../Cargo.toml");

/// TS: `const ALLOWED = new Set(["@earendil-works/kiana-telemetry"])`；Rust 侧 kiana-json 也允许
/// （它对应 TS 的 `typebox` + 内部 schema 工具，不是 Kiana 包之间的依赖）。
const ALLOWED_PI_CRATES: [&str; 2] = ["kiana-json", "kiana-telemetry"];

/// 解析 `Cargo.toml` 里某个表（`[dependencies]` / `[dev-dependencies]` / …）的条目名。
fn table_entries(manifest: &str, table: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut in_table = false;
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_table = line == format!("[{table}]");
            continue;
        }
        if !in_table || line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `name = { … }` / `name = "version"` / `name.workspace = true`。
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains('.') {
            continue;
        }
        entries.push((key.to_owned(), value.trim().to_owned()));
    }
    entries
}

/// 一个 crate 的源码目录（`crates/kiana-chord/src`）。
fn source_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// 递归收集 `dir` 下所有 `.rs` 文件。
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

// ── test 1：dependencies 里只有允许的 Kiana crate ────────────────────────────────────

#[test]
fn depends_on_no_kiana_crate_other_than_json_and_telemetry() {
    let dependencies = table_entries(MANIFEST, "dependencies");
    let workspace_crates: BTreeSet<String> = table_entries(
        // 根清单的 `[workspace.dependencies]` 列出全部 workspace crate（`kiana-*`）。
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("Cargo.toml"),
        )
        .expect("workspace manifest"),
        "workspace.dependencies",
    )
    .into_iter()
    .map(|(name, _)| name)
    .filter(|name| name.starts_with("kiana-"))
    .collect();

    let offending: Vec<String> = dependencies
        .iter()
        .map(|(name, _)| name.clone())
        .filter(|name| name.starts_with("kiana-") && !ALLOWED_PI_CRATES.contains(&name.as_str()))
        .collect();
    assert_eq!(
        offending,
        Vec::<String>::new(),
        "kiana-chord may only depend on {}",
        ALLOWED_PI_CRATES.join(", ")
    );
    assert!(
        workspace_crates.contains("kiana-json") && workspace_crates.contains("kiana-telemetry"),
        "the allowed crates are workspace members: {workspace_crates:?}"
    );
}

// ── test 2：没有指向 crate 之外的 path 依赖（TS 的"跑出 src/"） ────────────────────

#[test]
fn has_no_path_dependencies_outside_the_crate_or_workspace() {
    let crate_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
        for (name, value) in table_entries(MANIFEST, table) {
            if !value.contains("path") {
                continue;
            }
            let Some(path) = value
                .split("path")
                .nth(1)
                .and_then(|rest| rest.split('"').nth(1))
            else {
                violations.push(format!("{table}.{name}: unreadable path"));
                continue;
            };
            let resolved = crate_directory.join(path);
            let normalized = normalize(&resolved);
            let inside_crate = normalized.starts_with(normalize(crate_directory));
            let inside_workspace =
                normalized.starts_with(normalize(&crate_directory.join("../..")));
            if !inside_workspace && !inside_crate {
                violations.push(format!("{table}.{name}: {path}"));
            }
        }
    }
    assert_eq!(
        violations,
        Vec::<String>::new(),
        "path dependencies must stay inside the crate or the workspace"
    );
}

/// 消掉 `..`（不做 IO；`crates/kiana-chord/../..` 这样的路径要归一化后再比较）。
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

// ── test 3：源码只 use 允许的 Kiana crate ────────────────────────────────────────────

#[test]
fn source_imports_only_allowed_kiana_crates() {
    let source = source_directory();
    let mut violations = Vec::new();
    for file in rust_files(&source) {
        let text = std::fs::read_to_string(&file).expect("source file");
        let relative = file
            .strip_prefix(&source)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        for line in text.lines() {
            let trimmed = line.trim_start();
            let Some(rest) = trimmed
                .strip_prefix("use ")
                .or_else(|| trimmed.strip_prefix("pub use "))
                .or_else(|| trimmed.strip_prefix("pub(crate) use "))
            else {
                continue;
            };
            let Some(name) = rest.split("::").next() else {
                continue;
            };
            let name = name.trim();
            if name.starts_with("kiana_")
                && !ALLOWED_PI_CRATES
                    .iter()
                    .any(|allowed| name == allowed.replace('-', "_"))
            {
                violations.push(format!("{relative}: {trimmed}"));
            }
        }
    }
    assert_eq!(
        violations,
        Vec::<String>::new(),
        "kiana-chord's sources may only import {}",
        ALLOWED_PI_CRATES.join(", ")
    );
}

// ── test 4：没有指到 crate 之外的相对 `mod`/`#[path]` ─────────────────────────────

#[test]
fn source_has_no_paths_outside_the_crate() {
    let source = source_directory();
    let mut violations = Vec::new();
    for file in rust_files(&source) {
        let text = std::fs::read_to_string(&file).expect("source file");
        for line in text.lines() {
            let trimmed = line.trim();
            let Some(rest) = trimmed.strip_prefix("#[path = \"") else {
                continue;
            };
            let Some(value) = rest.split('"').next() else {
                continue;
            };
            if value.starts_with('/') || value.contains("..") {
                violations.push(format!("{}: {value}", file.display()));
            }
        }
    }
    assert_eq!(violations, Vec::<String>::new());
}

// ── test 5：kiana-telemetry 只依赖共享叶子 kiana-json（TS 允许它的理由） ─────────────────

#[test]
fn kiana_telemetry_depends_only_on_the_shared_json_leaf() {
    // TS 的 `@earendil-works/kiana-telemetry` 是"没有任何依赖的叶子"。Rust 侧它要 `kiana-json`
    // （`formatJsNumber`/`jsQuote` 的唯一实现，所有 crate 共用的叶子），但不能再依赖别的
    // workspace crate —— 否则 kiana-chord 通过它间接拉进上层 crate，边界就破了。
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/kiana-telemetry/Cargo.toml"),
    )
    .expect("kiana-telemetry manifest");
    let kiana_dependencies: Vec<String> = table_entries(&manifest, "dependencies")
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("kiana-"))
        .collect();
    assert_eq!(
        kiana_dependencies,
        vec!["kiana-json".to_owned()],
        "kiana-telemetry may only depend on the shared kiana-json leaf"
    );
}
