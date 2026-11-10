//! `packages/coding-agent/test/settings-manager-bug.test.ts` 的移植（4 个用例）
//! 以及选择性持久化的逐字节黄金测试。
//!
//! 4 个用例来自外部编辑保留的修复：`save()` 只覆盖本会话修改过的字段，因此外部对其它
//! 字段（含数组）的编辑不会被内存里的旧快照覆盖。
//!
//! 黄金测试读取 [`fixtures/settings/persist_golden.json`]：用例由 TS 参考实现
//! （`/home/ctyun/kiana-work/upstream-reference` 的 `settings-manager.ts`）生成，逐字节记录
//! `JSON.stringify(v, null, 2)` 的输出（键序与“无结尾换行”）。Rust 侧执行同样的动作序列，
//! 断言落盘字节完全一致。

use std::fs;
use std::path::{Path, PathBuf};

use kiana_ai::ModelThinkingLevel;
use kiana_config::settings::schema::{MermaidRenderingMode, PackageSource};
use kiana_config::settings::{SettingsManager, SettingsManagerCreateOptions};
use serde_json::Value;

struct TestDirs {
    _temp: tempfile::TempDir,
    agent_dir: PathBuf,
    project_dir: PathBuf,
}

impl TestDirs {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_path_buf();
        let agent_dir = root.join("agent");
        let project_dir = root.join("project");
        fs::create_dir_all(&agent_dir).expect("agent dir");
        fs::create_dir_all(project_dir.join(".kiana")).expect("project dir");
        Self {
            _temp: temp,
            agent_dir,
            project_dir,
        }
    }

    fn agent(&self) -> &str {
        self.agent_dir.to_str().expect("utf-8 path")
    }

    fn project(&self) -> &str {
        self.project_dir.to_str().expect("utf-8 path")
    }

    fn global_settings_path(&self) -> PathBuf {
        self.agent_dir.join("settings.json")
    }

    fn project_settings_path(&self) -> PathBuf {
        self.project_dir.join(".kiana").join("settings.json")
    }
}

fn write_file(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write file");
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).expect("read file")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&read_text(path)).expect("parse json")
}

// TS: "should preserve file changes to packages array when changing unrelated setting"
#[tokio::test]
async fn should_preserve_file_changes_to_packages_array_when_changing_unrelated_setting() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();

    write_file(
        &settings_path,
        &serde_json::to_string(
            &serde_json::json!({ "theme": "dark", "packages": ["npm:kiana-mcp-adapter"] }),
        )
        .expect("serialize"),
    );

    let manager = SettingsManager::create(dirs.project(), dirs.agent());

    assert_eq!(
        manager.get_packages(),
        vec![PackageSource::Text("npm:kiana-mcp-adapter".to_string())]
    );

    let mut current = read_json(&settings_path);
    current["packages"] = serde_json::json!([]);
    write_file(
        &settings_path,
        &serde_json::to_string_pretty(&current).expect("pretty"),
    );
    assert_eq!(read_json(&settings_path)["packages"], serde_json::json!([]));

    manager.set_theme("light".to_string());
    manager.flush().await;

    let saved = read_json(&settings_path);
    assert_eq!(saved["packages"], serde_json::json!([]));
    assert_eq!(saved["theme"], serde_json::json!("light"));
}

// TS: "should preserve file changes to extensions array when changing unrelated setting"
#[tokio::test]
async fn should_preserve_file_changes_to_extensions_array_when_changing_unrelated_setting() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();

    write_file(
        &settings_path,
        &serde_json::to_string(
            &serde_json::json!({ "theme": "dark", "extensions": ["/old/extension.ts"] }),
        )
        .expect("serialize"),
    );

    let manager = SettingsManager::create(dirs.project(), dirs.agent());

    let mut current = read_json(&settings_path);
    current["extensions"] = serde_json::json!(["/new/extension.ts"]);
    write_file(
        &settings_path,
        &serde_json::to_string_pretty(&current).expect("pretty"),
    );

    manager.set_default_thinking_level(ModelThinkingLevel::High);
    manager.flush().await;

    assert_eq!(
        read_json(&settings_path)["extensions"],
        serde_json::json!(["/new/extension.ts"])
    );
}

// TS: "should preserve external project settings changes when updating unrelated project field"
#[tokio::test]
async fn should_preserve_external_project_settings_changes_when_updating_unrelated_project_field() {
    let dirs = TestDirs::new();
    let project_settings_path = dirs.project_settings_path();
    write_file(
        &project_settings_path,
        &serde_json::to_string(&serde_json::json!({
            "extensions": ["./old-extension.ts"],
            "prompts": ["./old-prompt.md"],
        }))
        .expect("serialize"),
    );

    let manager = SettingsManager::create(dirs.project(), dirs.agent());

    let mut current = read_json(&project_settings_path);
    current["prompts"] = serde_json::json!(["./new-prompt.md"]);
    write_file(
        &project_settings_path,
        &serde_json::to_string_pretty(&current).expect("pretty"),
    );

    manager
        .set_project_extension_paths(vec!["./updated-extension.ts".to_string()])
        .expect("project trusted");
    manager.flush().await;

    let saved = read_json(&project_settings_path);
    assert_eq!(saved["prompts"], serde_json::json!(["./new-prompt.md"]));
    assert_eq!(
        saved["extensions"],
        serde_json::json!(["./updated-extension.ts"])
    );
}

// TS: "should let in-memory project changes override external changes for the same project field"
#[tokio::test]
async fn should_let_in_memory_project_changes_override_external_changes_for_the_same_project_field()
{
    let dirs = TestDirs::new();
    let project_settings_path = dirs.project_settings_path();
    write_file(
        &project_settings_path,
        &serde_json::to_string(&serde_json::json!({
            "extensions": ["./initial-extension.ts"],
        }))
        .expect("serialize"),
    );

    let manager = SettingsManager::create(dirs.project(), dirs.agent());

    let mut current = read_json(&project_settings_path);
    current["extensions"] = serde_json::json!(["./external-extension.ts"]);
    write_file(
        &project_settings_path,
        &serde_json::to_string_pretty(&current).expect("pretty"),
    );

    manager
        .set_project_extension_paths(vec!["./in-memory-extension.ts".to_string()])
        .expect("project trusted");
    manager.flush().await;

    assert_eq!(
        read_json(&project_settings_path)["extensions"],
        serde_json::json!(["./in-memory-extension.ts"])
    );
}

// ---------------------------------------------------------------------------
// 黄金测试：选择性持久化 + 迁移的逐字节输出（由 TS 参考实现生成）
// ---------------------------------------------------------------------------

fn apply_action(manager: &SettingsManager, action: &Value) {
    let action_type = action["type"].as_str().expect("action type");
    match action_type {
        "setTheme" => manager.set_theme(action["value"].as_str().expect("theme").to_string()),
        "setDefaultThinkingLevel" => {
            let level: ModelThinkingLevel =
                serde_json::from_value(action["value"].clone()).expect("thinking level");
            manager.set_default_thinking_level(level);
        }
        "setCompactionEnabled" => manager
            .set_compaction_enabled(action["value"].as_bool().expect("bool"))
            .expect("valid compaction parent"),
        "setRetryEnabled" => manager
            .set_retry_enabled(action["value"].as_bool().expect("bool"))
            .expect("valid retry parent"),
        "setShowImages" => manager
            .set_show_images(action["value"].as_bool().expect("bool"))
            .expect("valid terminal parent"),
        "setMermaidRenderingMode" => {
            let mode: MermaidRenderingMode =
                serde_json::from_value(action["value"].clone()).expect("mermaid mode");
            manager
                .set_mermaid_rendering_mode(mode)
                .expect("valid markdown parent");
        }
        "setHttpIdleTimeoutMs" => {
            manager
                .set_http_idle_timeout_ms(action["value"].as_f64().expect("timeout"))
                .expect("valid timeout");
        }
        "setProjectPackages" => {
            let packages: Vec<PackageSource> =
                serde_json::from_value(action["value"].clone()).expect("packages");
            manager
                .set_project_packages(packages)
                .expect("project trusted");
        }
        "setProjectExtensionPaths" => {
            let paths: Vec<String> =
                serde_json::from_value(action["value"].clone()).expect("paths");
            manager
                .set_project_extension_paths(paths)
                .expect("project trusted");
        }
        "setModelThinkingLevel" => {
            let level: ModelThinkingLevel =
                serde_json::from_value(action["value"].clone()).expect("thinking level");
            manager
                .set_model_thinking_level(
                    action["provider"].as_str().expect("provider"),
                    action["modelId"].as_str().expect("model id"),
                    level,
                )
                .expect("valid modelThinkingLevels parent");
        }
        other => panic!("unknown fixture action: {other}"),
    }
}

#[tokio::test]
async fn persist_golden_fixture_matches_typescript_bytes() {
    let fixture = include_str!("fixtures/settings/persist_golden.json");
    let fixture: Value = serde_json::from_str(fixture).expect("fixture json");
    let cases = fixture["cases"].as_array().expect("cases");

    for case in cases {
        let name = case["name"].as_str().expect("name");
        let dirs = TestDirs::new();
        let global_path = dirs.global_settings_path();
        let project_path = dirs.project_settings_path();
        if let Some(initial) = case["initial"].as_str() {
            write_file(&global_path, initial);
        }
        if let Some(project) = case["project"].as_str() {
            write_file(&project_path, project);
        }
        let project_trusted = case
            .get("projectTrusted")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let manager = SettingsManager::create_with_options(
            dirs.project(),
            dirs.agent(),
            SettingsManagerCreateOptions {
                project_trusted: Some(project_trusted),
            },
        );
        for action in case["actions"].as_array().expect("actions") {
            apply_action(&manager, action);
        }
        manager.flush().await;

        if let Some(expected) = case["expectedGlobal"].as_str() {
            assert_eq!(
                read_text(&global_path),
                expected,
                "global settings bytes differ for case {name:?}"
            );
        }
        if let Some(expected) = case["expectedProject"].as_str() {
            assert_eq!(
                read_text(&project_path),
                expected,
                "project settings bytes differ for case {name:?}"
            );
        }
    }
}
