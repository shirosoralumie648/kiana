//! 移植 `packages/coding-agent/test/crash-log.test.ts`（5 个测试），
//! 并补充 `readCrashLog` / `writeCrashLog` / `recordCrash` / `takeUnnotifiedCrash` 的语义测试
//! （`ca-resources.md` §2.14）。

use std::path::Path;

use kiana_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use kiana_resources::crash_log::{
    CrashInput, CrashKind, CrashRecord, ExtensionStackMetadata, MAX_AGE_MS, MAX_CRASH_RECORDS,
    clear_crash_log, find_extension_stack_matches, read_crash_log, record_crash,
    take_unnotified_crash,
};
use serde_json::json;

fn package_extension(source: &str, base_dir: &str, entry: &str) -> ExtensionStackMetadata {
    let normalized_base_dir = base_dir.replace('\\', "/");
    let resolved_path = format!("{normalized_base_dir}/{entry}");
    ExtensionStackMetadata {
        path: resolved_path.clone(),
        resolved_path: resolved_path.clone(),
        source_info: SourceInfo {
            path: resolved_path,
            source: source.to_string(),
            scope: SourceScope::User,
            origin: SourceOrigin::Package,
            base_dir: Some(base_dir.to_string()),
        },
    }
}

// ---------------------------------------------------------------------------
// extension crash attribution
// ---------------------------------------------------------------------------

#[test]
fn matches_stack_frames_beneath_loaded_package_roots() {
    let memory = package_extension(
        "npm:kiana-observational-memory",
        "/home/fedora/.kiana/agent/npm/node_modules/kiana-observational-memory",
        "extensions/index.ts",
    );
    let unrelated = package_extension(
        "npm:unrelated",
        "/home/fedora/.kiana/agent/npm/node_modules/unrelated",
        "extensions/index.ts",
    );
    let stack = concat!(
        "TypeError: Cannot read properties of undefined (reading 'runtime')\n",
        "    at streamSimple (file:///home/fedora/.local/lib/node_modules/@earendil-works/kiana-coding-agent/dist/bundle/chunks/chunk-CMRUVXTE.js:1093:16944)\n",
        "    at /home/fedora/.kiana/agent/npm/node_modules/kiana-observational-memory/src/agents/worker-stream.ts:43:45"
    );
    assert_eq!(
        find_extension_stack_matches(Some(stack), &[memory, unrelated]),
        vec!["npm:kiana-observational-memory"]
    );
}

#[test]
fn normalizes_windows_paths_and_deduplicates_package_extensions() {
    let root = "C:\\Users\\reporter\\.kiana\\agent\\npm\\node_modules\\@scope\\memory";
    let first = package_extension("npm:@scope/memory", root, "extensions/first.ts");
    let second = package_extension("npm:@scope/memory", root, "extensions/second.ts");
    let stack = concat!(
        "Error: broken\n",
        "    at run (c:\\users\\reporter\\.kiana\\agent\\npm\\node_modules\\@scope\\memory\\src\\worker.ts:4:2)"
    );
    assert_eq!(
        find_extension_stack_matches(Some(stack), &[first, second]),
        vec!["npm:@scope/memory"]
    );
}

#[test]
fn does_not_attribute_sibling_single_file_packages() {
    let extension = |name: &str| ExtensionStackMetadata {
        path: format!("/plugins/{name}.ts"),
        resolved_path: format!("/plugins/{name}.ts"),
        source_info: SourceInfo {
            path: format!("/plugins/{name}.ts"),
            source: format!("/plugins/{name}.ts"),
            scope: SourceScope::User,
            origin: SourceOrigin::Package,
            base_dir: Some("/plugins".to_string()),
        },
    };
    let stack = "Error: broken\n    at run (file:///plugins/b.ts:4:2)";
    assert_eq!(
        find_extension_stack_matches(Some(stack), &[extension("a"), extension("b")]),
        vec!["/plugins/b.ts"]
    );
}

#[test]
fn ignores_extension_paths_in_the_error_message() {
    let extension = package_extension("npm:memory", "/tmp/node_modules/memory", "index.ts");
    let stack = format!(
        "Error: Failed to read {}\n    at run (file:///opt/kiana/dist/core.js:4:2)",
        extension.resolved_path
    );
    assert!(find_extension_stack_matches(Some(&stack), &[extension]).is_empty());
}

#[test]
fn decodes_frame_paths_independently_from_malformed_error_text() {
    let extension = ExtensionStackMetadata {
        path: "/Users/reporter/.kiana/agent/extensions/local memory/index.ts".to_string(),
        resolved_path: "/Users/reporter/.kiana/agent/extensions/local memory/index.ts".to_string(),
        source_info: SourceInfo {
            path: "/Users/reporter/.kiana/agent/extensions/local memory/index.ts".to_string(),
            source: "local".to_string(),
            scope: SourceScope::User,
            origin: SourceOrigin::TopLevel,
            base_dir: Some("/Users/reporter/.kiana/agent/extensions".to_string()),
        },
    };
    let stack = "Error: progress 100%\n    at run (file:///Users/reporter/.kiana/agent/extensions/local%20memory/worker.ts:4:2)";
    assert_eq!(
        find_extension_stack_matches(Some(stack), std::slice::from_ref(&extension)),
        vec![extension.path.clone()]
    );
}

// ---------------------------------------------------------------------------
// read / write / record / take
// ---------------------------------------------------------------------------

#[test]
fn read_crash_log_filters_invalid_records_and_does_not_strip_bom() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("crashes.json");
    std::fs::write(
        &path,
        serde_json::to_string(&json!([
            { "timestamp": "2026-01-01T00:00:00.000Z", "message": "ok" },
            { "timestamp": 1, "message": "bad" },
            { "message": "missing timestamp" },
            "not an object",
            { "timestamp": "x", "message": "y", "extra": 1 }
        ]))
        .expect("json"),
    )
    .expect("write");
    let records = read_crash_log(path.to_str().expect("path"));
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].message(), Some("ok"));
    assert_eq!(records[1].as_map().get("extra"), Some(&json!(1)));

    std::fs::write(&path, "\u{FEFF}[]").expect("write bom");
    assert!(read_crash_log(path.to_str().expect("path")).is_empty());
    std::fs::write(&path, "not json").expect("write");
    assert!(read_crash_log(path.to_str().expect("path")).is_empty());
}

#[test]
fn record_crash_keeps_the_last_five_and_writes_two_space_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("crashes.json");
    for index in 0..7 {
        let message = format!("crash {index}");
        record_crash(
            CrashInput {
                kind: CrashKind::UncaughtException,
                message: &message,
                error_name: None,
                stack: None,
                session_file: None,
                cwd: "/repo",
            },
            path.to_str().expect("path"),
        );
    }
    let records = read_crash_log(path.to_str().expect("path"));
    assert_eq!(records.len(), MAX_CRASH_RECORDS);
    assert_eq!(records[4].message(), Some("crash 6"));
    let raw = std::fs::read_to_string(&path).expect("read");
    assert!(raw.starts_with("[\n  {\n    \"timestamp\": \""));
    assert!(raw.ends_with("}\n]\n"));
}

#[test]
fn record_crash_falls_back_to_error_name_and_keeps_nulls() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("crashes.json");
    let record = record_crash(
        CrashInput {
            kind: CrashKind::FatalError,
            message: "",
            error_name: Some("TypeError"),
            stack: None,
            session_file: None,
            cwd: "/repo",
        },
        path.to_str().expect("path"),
    )
    .expect("record");
    assert_eq!(record.message(), Some("TypeError"));
    assert_eq!(record.kind(), Some("fatal_error"));
    assert_eq!(record.as_map().get("stack"), Some(&serde_json::Value::Null));
    assert_eq!(
        record.as_map().get("sessionFile"),
        Some(&serde_json::Value::Null)
    );
    assert_eq!(
        record.as_map().get("version"),
        Some(&json!(kiana_core::config::VERSION))
    );
}

#[test]
fn take_unnotified_crash_marks_all_pending_records_and_returns_the_newest() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("crashes.json");
    let now = chrono::Utc::now().timestamp_millis();
    let old = now - 1000;
    let write = |records: serde_json::Value| {
        std::fs::write(&path, records.to_string()).expect("write");
    };
    write(json!([
        { "timestamp": chrono::DateTime::from_timestamp_millis(old).unwrap().to_rfc3339_opts(chrono::SecondsFormat::Millis, true), "message": "old" },
        { "timestamp": chrono::DateTime::from_timestamp_millis(now - 500).unwrap().to_rfc3339_opts(chrono::SecondsFormat::Millis, true), "message": "new" }
    ]));

    let crash = take_unnotified_crash(path.to_str().expect("path"), Some(now)).expect("crash");
    assert_eq!(crash.message(), Some("new"));
    assert!(!crash.notified());
    let records = read_crash_log(path.to_str().expect("path"));
    assert!(records.iter().all(CrashRecord::notified));
    assert!(take_unnotified_crash(path.to_str().expect("path"), Some(now)).is_none());
}

#[test]
fn take_unnotified_crash_ignores_unparseable_and_stale_timestamps() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("crashes.json");
    let now = chrono::Utc::now().timestamp_millis();
    let stale = now - MAX_AGE_MS - 1;
    std::fs::write(
        &path,
        json!([
            { "timestamp": "not a date", "message": "invalid" },
            { "timestamp": chrono::DateTime::from_timestamp_millis(stale).unwrap().to_rfc3339_opts(chrono::SecondsFormat::Millis, true), "message": "stale" }
        ])
        .to_string(),
    )
    .expect("write");
    assert!(take_unnotified_crash(path.to_str().expect("path"), Some(now)).is_none());
}

#[test]
fn clear_crash_log_removes_the_file_and_ignores_errors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("crashes.json");
    std::fs::write(&path, "[]").expect("write");
    clear_crash_log(path.to_str().expect("path"));
    assert!(!path.exists());
    // 再次删除不存在文件不应 panic。
    clear_crash_log(path.to_str().expect("path"));
}

// ---------------------------------------------------------------------------
// takeUnnotifiedCrash: JS 真值与 Date.parse（对 npm/Node 行为的回归测试）
// ---------------------------------------------------------------------------

fn write_records(path: &Path, records: serde_json::Value) {
    std::fs::write(path, format!("{}\n", records)).expect("write crashes");
}

#[test]
fn take_unnotified_crash_uses_js_truthiness_for_notified() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("crashes.json");
    write_records(
        &path,
        json!([
            {
                "timestamp": "2026-01-01T00:00:00.000Z",
                "version": "1",
                "kind": "fatal_error",
                "message": "truthy-notified",
                "stack": null,
                "sessionFile": null,
                "cwd": "/",
                "notified": 1
            },
            {
                "timestamp": "2026-01-01T00:00:01.000Z",
                "version": "1",
                "kind": "fatal_error",
                "message": "falsy-notified",
                "stack": null,
                "sessionFile": null,
                "cwd": "/",
                "notified": 0
            }
        ]),
    );
    let now = 1_767_312_000_000; // 2026-01-02T00:00:00Z
    let crash = take_unnotified_crash(path.to_str().unwrap(), Some(now)).expect("crash");
    assert_eq!(crash.message(), Some("falsy-notified"));

    let records = read_crash_log(path.to_str().unwrap());
    // 真值 notified（数字 1）保持原样；假值 0 被改写成 true。
    assert_eq!(records[0].as_map().get("notified"), Some(&json!(1)));
    assert_eq!(records[1].as_map().get("notified"), Some(&json!(true)));
}

#[test]
fn take_unnotified_crash_accepts_date_only_timestamps() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("crashes.json");
    write_records(
        &path,
        json!([{
            "timestamp": "2026-01-01",
            "version": "1",
            "kind": "fatal_error",
            "message": "date-only",
            "stack": null,
            "sessionFile": null,
            "cwd": "/"
        }]),
    );
    let now = 1_767_312_000_000; // 2026-01-02T00:00:00Z（7 天内）
    let crash = take_unnotified_crash(path.to_str().unwrap(), Some(now)).expect("crash");
    assert_eq!(crash.message(), Some("date-only"));
    assert!(read_crash_log(path.to_str().unwrap())[0].notified());
}

#[test]
fn take_unnotified_crash_parses_short_fractional_seconds_like_v8() {
    // 回归：`Date.parse("2026-01-01T00:00:00.1Z")` 是 100ms，且 `+05:00` 不会触发
    // 调试构建里的整数下溢 panic。期望值由 node 26 的 `Date.parse` 生成。
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("crashes.json");
    for (timestamp, now, expected) in [
        ("2026-01-01T00:00:00.1Z", 1_767_225_600_100, "one-digit-utc"),
        (
            "2026-01-01T00:00:00.12Z",
            1_767_225_600_120,
            "two-digit-utc",
        ),
        (
            "2026-01-01T00:00:00.1+05:00",
            1_767_207_600_100,
            "one-digit-offset",
        ),
    ] {
        write_records(
            &path,
            json!([{
                "timestamp": timestamp,
                "version": "1",
                "kind": "fatal_error",
                "message": expected,
                "stack": null,
                "sessionFile": null,
                "cwd": "/"
            }]),
        );
        let crash = take_unnotified_crash(path.to_str().unwrap(), Some(now)).expect("crash");
        assert_eq!(crash.message(), Some(expected), "timestamp {timestamp}");
    }
}

// ---------------------------------------------------------------------------
// 回归：JSON.parse 的数字语义（JS 只有 f64）
// ---------------------------------------------------------------------------

#[test]
fn read_crash_log_uses_js_f64_number_semantics() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("crashes.json");
    std::fs::write(
        &path,
        r#"[{"timestamp":"2026-01-01T00:00:00.000Z","message":"m","big":12345678901234567890,"unsafe":9007199254740993,"neg":-0,"small":1}]"#,
    )
    .expect("write");
    let records = read_crash_log(path.to_str().expect("path"));
    assert_eq!(records.len(), 1);
    let map = records[0].as_map();

    // JSON.parse -> 12345678901234567000（f64），而不是精确的 u64。
    let big = map.get("big").expect("big");
    assert_eq!(big.as_f64(), Some(1.2345678901234567e19));
    assert!(big.as_u64().is_none(), "big must be represented as f64");
    // 2^53 + 1 在 JS 里舍入到 2^53。
    assert_eq!(
        map.get("unsafe").and_then(serde_json::Value::as_f64),
        Some(9007199254740992.0)
    );
    // -0 保留符号。
    let negative_zero = map
        .get("neg")
        .and_then(serde_json::Value::as_f64)
        .expect("neg");
    assert_eq!(negative_zero, 0.0);
    assert!(negative_zero.is_sign_negative());
    // 2^53 以内的整数仍可读作 u64（ARCHITECTURE §4.3 第 4 条的整数表示）。
    assert_eq!(
        map.get("small").and_then(serde_json::Value::as_u64),
        Some(1)
    );
}
