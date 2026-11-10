//! `utils/paths.ts` 的 TS 对拍回归测试。
//!
//! `tests/fixtures/paths_reference.json` 由 TypeScript 参考实现生成（45 个输入 × 8 组
//! `PathInputOptions` 的 `normalizePath`、45×4 的 `resolvePath`、45×3 的
//! `getCwdRelativePath`/`formatPathRelativeToCwdOrAbsolute`，以及 `isLocalPath` 与
//! `normalizeWindowsShellPath`）。
//!
//! 两处**文档化偏差**不参与对拍（见 `src/paths.rs` 的模块文档）：
//!
//! - 非法 `file://` URL（编码过的 `/`、非法百分号转义、非 localhost 主机）在 TS 抛错，
//!   Rust 原样返回输入；
//! - 相对 baseDir 的解析按各自进程的 cwd（TS 默认 `process.cwd()`），语义相同但目录不同。

use kiana_core::paths::{
    PathInputOptions, format_path_relative_to_cwd_or_absolute, get_cwd_relative_path,
    is_local_path, normalize_path, normalize_windows_shell_path, resolve_path,
};

fn fixture() -> serde_json::Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/paths_reference.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json")
}

fn opts_from(value: &serde_json::Value) -> PathInputOptions {
    let mut o = PathInputOptions::default();
    if let Some(v) = value.get("trim").and_then(|v| v.as_bool()) {
        o.trim = v;
    }
    if let Some(v) = value.get("expandTilde").and_then(|v| v.as_bool()) {
        o.expand_tilde = v;
    }
    if let Some(v) = value.get("homeDir").and_then(|v| v.as_str()) {
        o.home_dir = Some(std::path::PathBuf::from(v));
    }
    if let Some(v) = value.get("stripAtPrefix").and_then(|v| v.as_bool()) {
        o.strip_at_prefix = v;
    }
    if let Some(v) = value
        .get("normalizeUnicodeSpaces")
        .and_then(|v| v.as_bool())
    {
        o.normalize_unicode_spaces = v;
    }
    o
}

#[test]
fn normalize_matches_ts() {
    let f = fixture();
    let mut bad = Vec::new();
    for e in f["normalize_opts"].as_array().unwrap() {
        let input = e["input"].as_str().unwrap();
        let options = opts_from(&e["opts"]);
        let actual = normalize_path(input, &options);
        match e.get("output").and_then(|v| v.as_str()) {
            Some(expected) => {
                if actual != expected {
                    bad.push(format!(
                        "normalize({input:?}, {:?}) => {actual:?} != {expected:?}",
                        e["opts"]
                    ));
                }
            }
            None => {
                // TS 抛错：Rust 原样返回输入（文档化偏差）。
                if actual != input {
                    bad.push(format!(
                        "normalize({input:?}) threw in TS but Rust returned {actual:?}"
                    ));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn resolve_relative_format_match_ts() {
    let f = fixture();
    let mut bad = Vec::new();
    for e in f["resolve"].as_array().unwrap() {
        let input = e["input"].as_str().unwrap();
        let base = e["base"].as_str().unwrap();
        // 相对 base 会按各自进程的 cwd 解析（TS 的默认 baseDir 是 process.cwd()），
        // 两边 cwd 不同，跳过（语义相同，见 paths.rs 的 node_path_resolve）。
        if !base.starts_with('/') {
            continue;
        }
        let actual = resolve_path(input, base, &PathInputOptions::default());
        match e.get("output").and_then(|v| v.as_str()) {
            Some(expected) => {
                if actual != expected {
                    bad.push(format!(
                        "resolve({input:?}, {base:?}) => {actual:?} != {expected:?}"
                    ));
                }
            }
            None => {
                if actual != input {
                    bad.push(format!(
                        "resolve({input:?}, {base:?}) threw in TS but Rust returned {actual:?}"
                    ));
                }
            }
        }
    }
    for e in f["relative"].as_array().unwrap() {
        let file = e["filePath"].as_str().unwrap();
        let cwd = e["cwd"].as_str().unwrap();
        if e.get("error").is_some() {
            continue; // TS 抛错（非法 file URL），Rust 原样返回：文档化偏差。
        }
        let actual = get_cwd_relative_path(file, cwd);
        let expected = e.get("output").and_then(|v| v.as_str());
        if actual.as_deref() != expected {
            bad.push(format!(
                "relative({file:?}, {cwd:?}) => {actual:?} != {expected:?}"
            ));
        }
    }
    for e in f["format"].as_array().unwrap() {
        let file = e["filePath"].as_str().unwrap();
        let cwd = e["cwd"].as_str().unwrap();
        if e.get("error").is_some() {
            continue;
        }
        let actual = format_path_relative_to_cwd_or_absolute(file, cwd);
        if let Some(expected) = e.get("output").and_then(|v| v.as_str())
            && actual != expected
        {
            bad.push(format!(
                "format({file:?}, {cwd:?}) => {actual:?} != {expected:?}"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn local_and_windows_shell_match_ts() {
    let f = fixture();
    let mut bad = Vec::new();
    for e in f["local"].as_array().unwrap() {
        let input = e["input"].as_str().unwrap();
        let expected = e["output"].as_bool().unwrap();
        if is_local_path(input) != expected {
            bad.push(format!("is_local_path({input:?})"));
        }
    }
    for e in f["winshell"].as_array().unwrap() {
        let input = e["input"].as_str().unwrap();
        if let Some(expected) = e.get("output").and_then(|v| v.as_str()) {
            let actual = normalize_windows_shell_path(input);
            if actual != expected {
                bad.push(format!(
                    "normalize_windows_shell_path({input:?}) => {actual:?} != {expected:?}"
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}
