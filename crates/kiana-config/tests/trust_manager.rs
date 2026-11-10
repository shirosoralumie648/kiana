//! `core/trust-manager.ts` 的测试移植（2 条），另加 trust.json 的字节格式断言。

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use kiana_config::trust::manager::{
    ProjectTrustStore, ProjectTrustUpdate, get_project_trust_options,
    has_trust_requiring_project_resources, normalize_cwd, render_trust_store_json,
};
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// HOME 覆盖的 RAII guard（panic 时也恢复）。
struct HomeGuard {
    original: Option<String>,
}

impl HomeGuard {
    fn set(value: &str) -> Self {
        let original = std::env::var("HOME").ok();
        // SAFETY: 测试内串行持有 ENV_LOCK；integration test 二进制内没有其他 HOME 读者。
        unsafe { std::env::set_var("HOME", value) };
        Self { original }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// stores decisions and inherits from parent directories
#[test]
fn stores_decisions_and_inherits_from_parent_directories() {
    let temp_dir = TempDir::new().unwrap();
    let agent_dir = temp_dir.path().join("agent");
    let cwd = temp_dir.path().join("project");
    fs::create_dir_all(&agent_dir).unwrap();
    fs::create_dir_all(&cwd).unwrap();

    let store = ProjectTrustStore::new(&path_string(&agent_dir));
    let parent_dir = temp_dir.path().join("trusted-parent");
    let child_dir = parent_dir.join("project");
    fs::create_dir_all(&child_dir).unwrap();

    assert_eq!(store.get(&path_string(&child_dir)).unwrap(), None);
    store.set(&path_string(&parent_dir), Some(true)).unwrap();
    assert_eq!(store.get(&path_string(&child_dir)).unwrap(), Some(true));
    store.set(&path_string(&child_dir), Some(false)).unwrap();
    assert_eq!(store.get(&path_string(&child_dir)).unwrap(), Some(false));
    store.set(&path_string(&child_dir), None).unwrap();
    assert_eq!(store.get(&path_string(&child_dir)).unwrap(), Some(true));

    // getEntry 返回决定所在目录。
    let entry = store
        .get_entry(&path_string(&child_dir))
        .unwrap()
        .expect("inherited entry");
    assert_eq!(entry.path, normalize_cwd(&path_string(&parent_dir)));
    assert!(entry.decision);
}

/// trust.json 的字节格式：键按 JS UTF-16 码元排序、2 空格缩进、末尾换行。
#[test]
fn writes_trust_json_with_sorted_keys_and_trailing_newline() {
    let temp_dir = TempDir::new().unwrap();
    let agent_dir = temp_dir.path().join("agent");
    fs::create_dir_all(&agent_dir).unwrap();
    let store = ProjectTrustStore::new(&path_string(&agent_dir));

    let root = temp_dir.path().join("z-root");
    let other = temp_dir.path().join("a-other");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&other).unwrap();

    store
        .set_many(&[
            ProjectTrustUpdate {
                path: path_string(&root),
                decision: Some(true),
            },
            ProjectTrustUpdate {
                path: path_string(&other),
                decision: Some(false),
            },
        ])
        .unwrap();

    let written = fs::read_to_string(store.trust_path()).unwrap();
    assert!(written.ends_with('\n'));
    // 2 空格缩进 + 合法 JSON + 两个布尔值。
    let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();
    let object = parsed.as_object().unwrap();
    assert_eq!(object.len(), 2);
    assert!(object.values().all(serde_json::Value::is_boolean));

    // 键按 JS 默认字符串排序（UTF-16 码元）。
    let keys: Vec<String> = object.keys().cloned().collect();
    let mut sorted = keys.clone();
    sorted.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    assert_eq!(keys, sorted, "keys must be JS-sorted");
}

/// detects trust-requiring project resources
#[test]
fn detects_trust_requiring_project_resources() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_dir = TempDir::new().unwrap();
    let cwd = temp_dir.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    let _home = HomeGuard::set(&path_string(temp_dir.path()));

    fs::create_dir_all(temp_dir.path().join(".kiana").join("agent")).unwrap();
    fs::create_dir_all(temp_dir.path().join(".agents").join("skills")).unwrap();
    assert!(!has_trust_requiring_project_resources(&path_string(
        temp_dir.path()
    )));
    assert!(!has_trust_requiring_project_resources(&path_string(&cwd)));

    fs::write(temp_dir.path().join(".kiana").join("settings.json"), "{}").unwrap();
    assert!(has_trust_requiring_project_resources(&path_string(
        temp_dir.path()
    )));
    fs::remove_file(temp_dir.path().join(".kiana").join("settings.json")).unwrap();

    fs::create_dir_all(cwd.join(".kiana")).unwrap();
    fs::write(cwd.join(".kiana").join("settings.json"), "{}").unwrap();
    assert!(has_trust_requiring_project_resources(&path_string(&cwd)));

    fs::remove_dir_all(cwd.join(".kiana")).unwrap();
    fs::create_dir_all(cwd.join(".agents").join("skills")).unwrap();
    assert!(has_trust_requiring_project_resources(&path_string(&cwd)));
}

/// trust.json 的渲染字节与 TS `writeTrustFile` 一致（黄金测试；含 UTF-16 排序）。
///
/// fixture 由 `/home/ctyun/kiana-work/scratch/kiana-config__auth-trust/gen-fixtures.mjs` 生成。
#[test]
fn renders_trust_json_matching_ts_golden_fixtures() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("fixtures/trust/trust-json-cases.json")).unwrap();
    for case in cases {
        let entries: Vec<(String, Option<bool>)> = case["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                let key = entry[0].as_str().unwrap().to_string();
                let decision = match &entry[1] {
                    serde_json::Value::Bool(value) => Some(*value),
                    serde_json::Value::Null => None,
                    other => panic!("unexpected decision {other:?}"),
                };
                (key, decision)
            })
            .collect();
        assert_eq!(
            render_trust_store_json(&entries),
            case["expected"].as_str().unwrap(),
            "case: {}",
            case["name"]
        );
    }
}

/// trust.json 的解析错误文案：V8 JSON.parse + `JSON.stringify(key)` 转义。
#[test]
fn reports_v8_json_parse_errors_for_trust_store() {
    let temp_dir = TempDir::new().unwrap();
    let agent_dir = temp_dir.path().join("agent");
    fs::create_dir_all(&agent_dir).unwrap();
    let store = ProjectTrustStore::new(&path_string(&agent_dir));
    let cwd = temp_dir.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    store.set(&path_string(&cwd), Some(true)).unwrap();
    let trust_path = store.trust_path().to_string();

    for (content, expected) in [
        (
            "{oops",
            "Expected property name or '}' in JSON at position 1 (line 1 column 2)",
        ),
        ("", "Unexpected end of JSON input"),
        (
            "{} x",
            "Unexpected non-whitespace character after JSON at position 3 (line 1 column 4)",
        ),
    ] {
        fs::write(&trust_path, content).unwrap();
        let error = store.get(&path_string(&cwd)).unwrap_err();
        assert_eq!(
            error.message,
            format!("Failed to read trust store {trust_path}: {expected}"),
            "content: {content:?}"
        );
    }

    // 非法值：键按 JSON.stringify 加引号并转义。
    fs::write(&trust_path, r#"{"a\"b":5}"#).unwrap();
    let error = store.get(&path_string(&cwd)).unwrap_err();
    assert_eq!(
        error.message,
        format!(
            "Invalid trust store {trust_path}: value for \"a\\\"b\" must be true, false, or null"
        )
    );

    // trust.json 是目录：EISDIR 的 read 阶段文案不带 path。
    fs::remove_file(&trust_path).unwrap();
    fs::create_dir(&trust_path).unwrap();
    let error = store.get(&path_string(&cwd)).unwrap_err();
    assert_eq!(
        error.message,
        format!(
            "Failed to read trust store {trust_path}: EISDIR: illegal operation on a directory, read"
        )
    );
    fs::remove_dir(&trust_path).unwrap();
}

/// trust.json 的 I/O 错误文案是 Node 风格（`CODE: message, syscall 'path'`）。
#[cfg(unix)]
#[test]
fn reports_node_style_trust_store_io_errors() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().unwrap();
    let agent_dir = temp_dir.path().join("agent");
    fs::create_dir_all(&agent_dir).unwrap();
    let store = ProjectTrustStore::new(&path_string(&agent_dir));
    let cwd = temp_dir.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    store.set(&path_string(&cwd), Some(true)).unwrap();
    let trust_path = store.trust_path().to_string();

    // 读 EACCES：包裹 `Failed to read trust store {path}: ...`。
    fs::set_permissions(&trust_path, fs::Permissions::from_mode(0o000)).unwrap();
    let error = store.get(&path_string(&cwd)).unwrap_err();
    assert_eq!(
        error.message,
        format!(
            "Failed to read trust store {trust_path}: EACCES: permission denied, open '{trust_path}'"
        )
    );

    // 写 EACCES：原始 Node 文案（不包裹）。
    fs::set_permissions(&trust_path, fs::Permissions::from_mode(0o444)).unwrap();
    let error = store.set(&path_string(&cwd), Some(false)).unwrap_err();
    assert_eq!(
        error.message,
        format!("EACCES: permission denied, open '{trust_path}'")
    );
    fs::set_permissions(&trust_path, fs::Permissions::from_mode(0o644)).unwrap();

    // 父路径是文件：`ENOTDIR: not a directory, mkdir '<dir>'`。
    let blocked = temp_dir.path().join("blocked");
    fs::write(&blocked, "x").unwrap();
    let blocked_agent = blocked.join("agent");
    let store = ProjectTrustStore::new(&path_string(&blocked_agent));
    let error = store.get(&path_string(&cwd)).unwrap_err();
    assert_eq!(
        error.message,
        format!(
            "ENOTDIR: not a directory, mkdir '{}'",
            path_string(&blocked_agent)
        )
    );
}

/// getProjectTrustOptions 的标签/顺序与 parent 逻辑
#[test]
fn builds_project_trust_options_in_order() {
    let temp_dir = TempDir::new().unwrap();
    let cwd = temp_dir.path().join("a").join("b");
    fs::create_dir_all(&cwd).unwrap();
    let cwd_string = path_string(&cwd);
    let normalized = normalize_cwd(&cwd_string);
    let parent = normalize_cwd(&path_string(cwd.parent().unwrap()));

    let options = get_project_trust_options(&cwd_string, true);
    let labels: Vec<String> = options.iter().map(|option| option.label.clone()).collect();
    assert_eq!(
        labels,
        vec![
            "Trust".to_string(),
            format!("Trust parent folder ({parent})"),
            "Trust (this session only)".to_string(),
            "Do not trust".to_string(),
            "Do not trust (this session only)".to_string(),
        ]
    );
    assert_eq!(options[0].saved_path.as_deref(), Some(normalized.as_str()));
    assert_eq!(options[1].saved_path.as_deref(), Some(parent.as_str()));
    assert_eq!(options[1].updates.len(), 2);
    assert_eq!(options[1].updates[1].decision, None);
    assert_eq!(options[2].updates.len(), 0);
    assert!(!options[3].trusted);
    assert!(!options[4].trusted);

    let without_session = get_project_trust_options(&cwd_string, false);
    assert_eq!(without_session.len(), 3);
    assert_eq!(without_session[2].label, "Do not trust");
}
