//! `core/keybindings.ts` 的移植测试。
//!
//! TS 源：`packages/coding-agent/test/keybindings.test.ts`（5 个 `it(...)`）。
//! 每个测试的 `TS:` 注释保留原测试标题逐字。
//!
//! 另外用 TS 参考实现生成的四份 golden fixture（`tests/fixtures/keybindings_defaults_*.json`）
//! 逐一校验 `linux` / WSL / `win32` / `darwin` 四种平台变体的完整 90 项默认表
//! （`KEYBINDINGS` 是模块级常量，TS 测试只能覆盖当前平台；这里把全部分支都钉住）。

use std::fs;
use std::path::PathBuf;

use kiana_config::keybindings::{
    KeyIdList, KeybindingDefinition, KeybindingDefinitions, KeybindingEnv, app_keybindings,
    keybindings, keybindings_for, use_windows_keybindings,
};
use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn env(pairs: &[(&str, &str)]) -> KeybindingEnv {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

fn expected_table(variant: &str) -> KeybindingDefinitions {
    let text = fs::read_to_string(fixture(&format!("keybindings_defaults_{variant}.json")))
        .expect("fixture exists");
    let value = kiana_json::js_parse::parse(&text).expect("fixture is valid JSON");
    let mut table = KeybindingDefinitions::new();
    for (id, definition) in value.as_object().expect("fixture object") {
        let description = definition
            .get("description")
            .and_then(Value::as_str)
            .expect("description")
            .to_string();
        let default_keys = match definition.get("defaultKeys").expect("defaultKeys") {
            Value::String(key) => KeyIdList::One(key.clone()),
            Value::Array(keys) => KeyIdList::Many(
                keys.iter()
                    .map(|key| key.as_str().expect("key string").to_string())
                    .collect(),
            ),
            other => panic!("unexpected defaultKeys: {other:?}"),
        };
        table.insert(
            id.clone(),
            KeybindingDefinition {
                default_keys,
                description: Some(description),
            },
        );
    }
    table
}

/// IndexMap 的 `PartialEq` 忽略顺序；默认键序是行为的一部分，这里显式比较。
fn assert_tables_equal(actual: &KeybindingDefinitions, expected: &KeybindingDefinitions) {
    let actual_ids: Vec<&String> = actual.keys().collect();
    let expected_ids: Vec<&String> = expected.keys().collect();
    assert_eq!(actual_ids, expected_ids);
    assert_eq!(actual.len(), expected.len());
    for (id, expected_definition) in expected {
        assert_eq!(
            actual.get(id),
            Some(expected_definition),
            "definition mismatch for {id}"
        );
    }
}

fn keys(definition: &KeybindingDefinition) -> &KeyIdList {
    &definition.default_keys
}

fn definition(id: &str) -> &'static KeybindingDefinition {
    keybindings().get(id).expect("keybinding exists")
}

// TS: "uses Windows keybindings on native Windows"
#[test]
fn uses_windows_keybindings_on_native_windows() {
    assert!(use_windows_keybindings(Some("win32"), Some(&env(&[]))));
}

// TS: "uses Windows keybindings in WSL without relying on Windows Terminal detection"
#[test]
fn uses_windows_keybindings_in_wsl_without_relying_on_windows_terminal_detection() {
    assert!(use_windows_keybindings(
        Some("linux"),
        Some(&env(&[("WSL_DISTRO_NAME", "Ubuntu")]))
    ));
    assert!(use_windows_keybindings(
        Some("linux"),
        Some(&env(&[("WSL_INTEROP", "/run/WSL/123_interop")]))
    ));
}

// TS: "does not use Windows keybindings from WT_SESSION alone"
#[test]
fn does_not_use_windows_keybindings_from_wt_session_alone() {
    assert!(!use_windows_keybindings(
        Some("linux"),
        Some(&env(&[("WT_SESSION", "session")]))
    ));
}

// TS: "keeps non-Windows defaults on other platforms"
#[test]
fn keeps_non_windows_defaults_on_other_platforms() {
    assert!(!use_windows_keybindings(Some("linux"), Some(&env(&[]))));
    assert!(!use_windows_keybindings(Some("darwin"), Some(&env(&[]))));
}

// TS: "applies the detected defaults consistently"
#[test]
fn applies_the_detected_defaults_consistently() {
    let windows_keybindings = use_windows_keybindings(None, None);
    let native_windows = cfg!(windows);

    assert_eq!(
        keys(definition("app.clipboard.pasteImage")),
        &if windows_keybindings {
            KeyIdList::One("alt+v".to_string())
        } else {
            KeyIdList::One("ctrl+v".to_string())
        }
    );
    assert_eq!(
        keys(definition("tui.altScreen.search")),
        &if windows_keybindings {
            KeyIdList::One("ctrl+f".to_string())
        } else {
            KeyIdList::One("ctrl+shift+f".to_string())
        }
    );
    assert_eq!(
        keys(definition("app.message.followUp")),
        &if windows_keybindings {
            KeyIdList::One("ctrl+q".to_string())
        } else {
            KeyIdList::One("alt+enter".to_string())
        }
    );
    assert_eq!(
        keys(definition("app.model.cycleBackward")),
        &if windows_keybindings {
            KeyIdList::One("alt+p".to_string())
        } else {
            KeyIdList::One("shift+ctrl+p".to_string())
        }
    );
    assert_eq!(
        keys(definition("tui.editor.undo")),
        &if native_windows {
            KeyIdList::One("ctrl+z".to_string())
        } else if windows_keybindings {
            KeyIdList::One("alt+z".to_string())
        } else {
            KeyIdList::One("ctrl+-".to_string())
        }
    );
    assert_eq!(
        keys(definition("tui.altScreen.previousPrompt")),
        &if windows_keybindings {
            KeyIdList::One("ctrl+up".to_string())
        } else {
            KeyIdList::Many(vec!["ctrl+shift+up".to_string(), "ctrl+up".to_string()])
        }
    );
    assert_eq!(
        keys(definition("tui.altScreen.nextPrompt")),
        &if windows_keybindings {
            KeyIdList::One("ctrl+down".to_string())
        } else {
            KeyIdList::Many(vec!["ctrl+shift+down".to_string(), "ctrl+down".to_string()])
        }
    );
    assert_eq!(
        keys(definition("app.message.dequeue")),
        &if windows_keybindings {
            KeyIdList::One("alt+q".to_string())
        } else {
            KeyIdList::One("alt+up".to_string())
        }
    );
}

// =============================================================================
// golden fixtures：全平台默认表
// =============================================================================

#[test]
fn matches_golden_defaults_on_linux() {
    let actual = keybindings_for(Some("linux"), Some(&env(&[])));
    assert_tables_equal(&actual, &expected_table("linux"));
}

#[test]
fn matches_golden_defaults_in_wsl() {
    let actual = keybindings_for(Some("linux"), Some(&env(&[("WSL_DISTRO_NAME", "Ubuntu")])));
    assert_tables_equal(&actual, &expected_table("wsl"));
}

#[test]
fn matches_golden_defaults_on_native_windows() {
    let actual = keybindings_for(Some("win32"), Some(&env(&[])));
    assert_tables_equal(&actual, &expected_table("win32"));
}

#[test]
fn matches_golden_defaults_on_macos() {
    let actual = keybindings_for(Some("darwin"), Some(&env(&[])));
    assert_tables_equal(&actual, &expected_table("darwin"));
}

#[test]
fn current_platform_table_matches_its_golden_variant() {
    let current = if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if use_windows_keybindings(None, None) {
        "wsl"
    } else {
        "linux"
    };
    assert_tables_equal(keybindings(), &expected_table(current));
}

#[test]
fn keybindings_is_tui_defaults_then_overrides_then_app_keybindings() {
    let tui = kiana_tui::keybindings::tui_keybindings();
    let actual: Vec<&String> = keybindings().keys().collect();
    let tui_ids: Vec<&String> = tui.keys().collect();
    let app_ids: Vec<&String> = app_keybindings().keys().collect();

    assert_eq!(&actual[..tui.len()], tui_ids.as_slice());
    assert_eq!(&actual[tui.len()..], app_ids.as_slice());
    assert_eq!(tui.len(), 47);
    assert_eq!(app_ids.len(), 43);
    assert_eq!(actual.len(), 90);
}

#[test]
fn overridden_tui_definitions_keep_their_descriptions() {
    for id in [
        "tui.editor.undo",
        "tui.altScreen.previousPrompt",
        "tui.altScreen.nextPrompt",
        "tui.altScreen.search",
    ] {
        let tui = kiana_tui::keybindings::tui_keybindings();
        assert_eq!(
            definition(id).description,
            tui.get(id).expect("tui id").description,
            "{id}"
        );
    }
}

#[test]
fn app_keybindings_are_the_last_forty_three_entries() {
    let app = app_keybindings();
    assert_eq!(app.len(), 43);
    assert_eq!(app.keys().next().map(String::as_str), Some("app.interrupt"));
    assert_eq!(
        app.keys().last().map(String::as_str),
        Some("app.tree.filter.cycleBackward")
    );
}

#[test]
fn sorting_preserves_keybindings_order_for_known_ids() {
    // 由 `order_keybindings_config` 使用的静态表序：TUI id 全部位于 app id 之前。
    let ids: Vec<&str> = keybindings().keys().map(String::as_str).collect();
    let first_app = ids.iter().position(|id| id.starts_with("app.")).unwrap();
    assert!(ids[first_app..].iter().all(|id| id.starts_with("app.")));
    assert!(ids[..first_app].iter().all(|id| id.starts_with("tui.")));
}
