//! 快捷键解析测试：移植 `packages/coding-agent/test/extensions-runner.test.ts` 的
//! `describe("shortcut conflicts")` 8 例，并补充 `buildBuiltinKeybindings` 的单元测试。
//!
//! 期望值来自 TS 参考实现生成的黄金夹具
//! `tests/fixtures/runner_registry/shortcut_cases.json`（由
//! `ExtensionRunner.getShortcuts(defaultKeybindings)` 对同一组输入产出）。
//! `default_keybindings.json` 是 `new KeybindingsManager().getEffectiveConfig()` 的逐字输出。

use std::sync::Arc;

use indexmap::IndexMap;
use kiana_config::keybindings::{KeyIdList, KeybindingsConfig};
use kiana_core::diagnostics::DiagnosticKind;
use kiana_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use kiana_ext_api::types::ShortcutHandler;
use kiana_ext_api::{Extension, ExtensionShortcut};
use kiana_extensions::runner::keybindings::{
    BuiltinKeybinding, RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS, build_builtin_keybindings,
    get_shortcuts,
};
use serde_json::Value;

const DEFAULT_KEYBINDINGS_JSON: &str =
    include_str!("fixtures/runner_registry/default_keybindings.json");
const SHORTCUT_CASES_JSON: &str = include_str!("fixtures/runner_registry/shortcut_cases.json");

/// 解析 `default_keybindings.json`（`KeybindingsConfig` 没有 Deserialize，因为 `KeyIdList`
/// 是 string | string[] 的联合）。
fn default_keybindings() -> KeybindingsConfig {
    let raw: Value = serde_json::from_str(DEFAULT_KEYBINDINGS_JSON).expect("fixture parses");
    let mut config = KeybindingsConfig::new();
    for (key, value) in raw.as_object().expect("fixture is an object") {
        let keys = match value {
            Value::String(key) => KeyIdList::One(key.clone()),
            Value::Array(keys) => KeyIdList::Many(
                keys.iter()
                    .map(|key| key.as_str().expect("key is a string").to_string())
                    .collect(),
            ),
            other => panic!("unexpected binding for {key}: {other}"),
        };
        config.insert(key.clone(), keys);
    }
    config
}

/// `{ ...defaultKeybindings, [id]: value }`。
fn with_key(id: &str, value: &[&str]) -> KeybindingsConfig {
    let mut config = default_keybindings();
    let keys = match value {
        [key] => KeyIdList::One((*key).to_string()),
        keys => KeyIdList::Many(keys.iter().map(|key| (*key).to_string()).collect()),
    };
    config.insert(id.to_string(), keys);
    config
}

fn base_extension(path: &str) -> Extension {
    Extension {
        path: path.to_string(),
        resolved_path: path.to_string(),
        hidden: None,
        replaceable: None,
        source_info: SourceInfo {
            path: path.to_string(),
            source: "local".to_string(),
            scope: SourceScope::User,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        },
        handlers: IndexMap::new(),
        tools: IndexMap::new(),
        message_renderers: IndexMap::new(),
        tool_renderers: None,
        markdown_transformer: None,
        entry_renderers: None,
        commands: IndexMap::new(),
        flags: IndexMap::new(),
        shortcuts: IndexMap::new(),
    }
}

/// 只填 `shortcuts` 的最小 `Extension`（与 TS 测试里 `kiana.registerShortcut` 的结果等价）。
fn extension(path: &str, shortcuts: &[(&str, &str)]) -> Extension {
    let mut map = IndexMap::new();
    for (key, description) in shortcuts {
        let handler: ShortcutHandler = Arc::new(|_ctx| Box::pin(async {}));
        map.insert(
            (*key).to_string(),
            ExtensionShortcut {
                shortcut: (*key).to_string(),
                description: Some((*description).to_string()),
                handler,
                extension_path: path.to_string(),
            },
        );
    }
    let mut ext = base_extension(path);
    ext.shortcuts = map;
    ext
}

fn shortcut_cases() -> Value {
    serde_json::from_str(SHORTCUT_CASES_JSON).expect("fixture parses")
}

/// 用黄金夹具逐字断言一次 `getShortcuts` 的结果（无 UI，因此 `warnings` 也应写入）。
fn assert_case(case: &str, extensions: &[Extension], keybindings: &KeybindingsConfig) {
    let cases = shortcut_cases();
    let expected = &cases[case];
    let resolution = get_shortcuts(extensions, keybindings, false);

    let keys: Vec<&str> = resolution.shortcuts.keys().map(String::as_str).collect();
    let expected_keys: Vec<&str> = expected["keys"]
        .as_array()
        .expect("keys array")
        .iter()
        .map(|key| key.as_str().expect("key string"))
        .collect();
    assert_eq!(keys, expected_keys, "shortcut keys for {case}");

    let entries: Vec<(String, Option<String>, String)> = resolution
        .shortcuts
        .iter()
        .map(|(key, shortcut)| {
            (
                key.clone(),
                shortcut.description.clone(),
                shortcut.extension_path.clone(),
            )
        })
        .collect();
    let expected_entries: Vec<(String, Option<String>, String)> = expected["entries"]
        .as_array()
        .expect("entries array")
        .iter()
        .map(|entry| {
            let pair = entry.as_array().expect("entry pair");
            (
                pair[0].as_str().expect("key").to_string(),
                pair[1]["description"].as_str().map(str::to_string),
                pair[1]["extensionPath"]
                    .as_str()
                    .expect("extensionPath")
                    .to_string(),
            )
        })
        .collect();
    assert_eq!(entries, expected_entries, "shortcut entries for {case}");

    let diagnostics = serde_json::to_value(&resolution.diagnostics).expect("diagnostics serialize");
    assert_eq!(
        diagnostics, expected["diagnostics"],
        "diagnostics for {case}"
    );

    let expected_warnings: Vec<String> = expected["warnings"]
        .as_array()
        .expect("warnings array")
        .iter()
        .map(|warning| warning.as_str().expect("warning string").to_string())
        .collect();
    assert_eq!(
        resolution.warnings, expected_warnings,
        "warnings for {case}"
    );
}

// TS: "warns when extension shortcut conflicts with built-in"
#[test]
fn warns_when_extension_shortcut_conflicts_with_built_in() {
    let extensions = [extension(
        "/extensions/conflict.ts",
        &[("ctrl+c", "Conflicts with built-in")],
    )];
    assert_case(
        "conflicts-with-built-in",
        &extensions,
        &default_keybindings(),
    );
    let resolution = get_shortcuts(&extensions, &default_keybindings(), false);
    assert_eq!(
        resolution.diagnostics[0].kind,
        DiagnosticKind::Warning,
        "诊断级别是 warning"
    );
    assert_eq!(
        resolution.diagnostics[0].path.as_deref(),
        Some("/extensions/conflict.ts")
    );
}

// TS: "allows a shortcut when the reserved set no longer contains the default key"
#[test]
fn allows_a_shortcut_when_the_reserved_set_no_longer_contains_the_default_key() {
    let extensions = [extension(
        "/extensions/rebinding.ts",
        &[("ctrl+p", "Uses freed default")],
    )];
    assert_case(
        "allows-freed-reserved-key",
        &extensions,
        &with_key("app.model.cycleForward", &["ctrl+n"]),
    );
    let resolution = get_shortcuts(
        &extensions,
        &with_key("app.model.cycleForward", &["ctrl+n"]),
        false,
    );
    assert!(
        !resolution
            .warnings
            .iter()
            .any(|warning| warning.contains("conflicts with built-in")),
        "ctrl+p 不再被保留项占用"
    );
}

// TS: "warns but allows when extension uses non-reserved built-in shortcut"
#[test]
fn warns_but_allows_when_extension_uses_non_reserved_built_in_shortcut() {
    let extensions = [extension(
        "/extensions/non-reserved.ts",
        &[("ctrl+v", "Overrides non-reserved")],
    )];
    assert_case("non-reserved-built-in", &extensions, &default_keybindings());
    let resolution = get_shortcuts(&extensions, &default_keybindings(), false);
    assert!(
        resolution.warnings[0].contains("built-in shortcut for app.clipboard.pasteImage"),
        "警告里给出内建 keybinding id"
    );
    assert!(resolution.shortcuts.contains_key("ctrl+v"));
}

// TS: "blocks shortcuts for reserved actions even when rebound"
#[test]
fn blocks_shortcuts_for_reserved_actions_even_when_rebound() {
    let extensions = [extension(
        "/extensions/rebound-reserved.ts",
        &[("ctrl+x", "Conflicts with rebound reserved")],
    )];
    let keybindings = with_key("app.interrupt", &["ctrl+x"]);
    assert_case("rebound-reserved", &extensions, &keybindings);
    assert!(
        get_shortcuts(&extensions, &keybindings, false)
            .shortcuts
            .is_empty()
    );
}

// TS: "blocks shortcuts when reserved key is also bound to non-reserved actions"
#[test]
fn blocks_shortcuts_when_reserved_key_is_also_bound_to_non_reserved_actions() {
    let extensions = [extension(
        "/extensions/shared-reserved.ts",
        &[("ctrl+p", "Conflicts with shared reserved default")],
    )];
    assert_case("shared-reserved", &extensions, &default_keybindings());
}

// TS: "blocks shortcuts when reserved action has multiple keys"
#[test]
fn blocks_shortcuts_when_reserved_action_has_multiple_keys() {
    let extensions = [extension(
        "/extensions/multi-reserved.ts",
        &[("ctrl+y", "Conflicts with multi-key reserved")],
    )];
    assert_case(
        "multi-reserved",
        &extensions,
        &with_key("app.clear", &["ctrl+x", "ctrl+y"]),
    );
}

// TS: "warns but allows when non-reserved action has multiple keys"
#[test]
fn warns_but_allows_when_non_reserved_action_has_multiple_keys() {
    let extensions = [extension(
        "/extensions/multi-non-reserved.ts",
        &[("ctrl+y", "Overrides multi-key non-reserved")],
    )];
    assert_case(
        "multi-non-reserved",
        &extensions,
        &with_key("app.clipboard.pasteImage", &["ctrl+x", "ctrl+y"]),
    );
}

// TS: "warns when two extensions register same shortcut"
#[test]
fn warns_when_two_extensions_register_same_shortcut() {
    let extensions = [
        extension(
            "/extensions/ext1.ts",
            &[("ctrl+shift+x", "First extension")],
        ),
        extension(
            "/extensions/ext2.ts",
            &[("ctrl+shift+x", "Second extension")],
        ),
    ];
    assert_case(
        "same-shortcut-two-extensions",
        &extensions,
        &default_keybindings(),
    );
    // 后注册者胜出。
    let resolution = get_shortcuts(&extensions, &default_keybindings(), false);
    let winner = &resolution.shortcuts["ctrl+shift+x"];
    assert_eq!(winner.extension_path, "/extensions/ext2.ts");
    assert_eq!(winner.description.as_deref(), Some("Second extension"));
}

/// 同一条捷径同时触发“内建非保留”与“扩展重复”时，两条诊断都按 TS 顺序发出。
#[test]
fn reports_builtin_and_extension_duplicate_diagnostics_together() {
    let extensions = [
        extension(
            "/extensions/paste-a.ts",
            &[("ctrl+v", "First paste override")],
        ),
        extension(
            "/extensions/paste-b.ts",
            &[("ctrl+v", "Second paste override")],
        ),
    ];
    assert_case(
        "builtin-and-extension-duplicate",
        &extensions,
        &default_keybindings(),
    );
    let resolution = get_shortcuts(&extensions, &default_keybindings(), false);
    assert_eq!(resolution.diagnostics.len(), 3);
    assert_eq!(resolution.warnings.len(), 3);
}

/// 非法大小写的扩展键会先 `toLowerCase()` 再与内建表比较（TS 同款行为）。
#[test]
fn lowercases_extension_shortcut_keys() {
    let extensions = [extension(
        "/extensions/uppercase.ts",
        &[("CTRL+C", "Uppercase reserved")],
    )];
    assert_case(
        "lowercases-extension-key",
        &extensions,
        &default_keybindings(),
    );
}

/// `hasUI() == true` 时诊断照旧，只是不再 `console.warn`。
#[test]
fn with_ui_only_collects_diagnostics() {
    let extensions = [extension(
        "/extensions/conflict.ts",
        &[("ctrl+c", "Conflicts with built-in")],
    )];
    let resolution = get_shortcuts(&extensions, &default_keybindings(), true);
    assert_eq!(resolution.diagnostics.len(), 1);
    assert!(resolution.warnings.is_empty());
    assert!(resolution.shortcuts.is_empty());
}

/// 18 个保留 id 的名称与顺序逐字。
#[test]
fn reserved_keybindings_are_the_eighteen_ids_in_source_order() {
    assert_eq!(RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS.len(), 18);
    assert_eq!(
        RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS,
        [
            "app.interrupt",
            "app.clear",
            "app.exit",
            "app.suspend",
            "app.thinking.cycle",
            "app.model.cycleForward",
            "app.model.cycleBackward",
            "app.model.select",
            "app.tools.expand",
            "app.thinking.toggle",
            "app.editor.external",
            "app.message.copy",
            "app.message.followUp",
            "tui.input.submit",
            "tui.select.confirm",
            "tui.select.cancel",
            "tui.input.copy",
            "tui.editor.deleteToLineEnd",
        ]
    );
}

fn builtin(keybinding: &str, restrict_override: bool) -> BuiltinKeybinding {
    BuiltinKeybinding {
        keybinding: keybinding.to_string(),
        restrict_override,
    }
}

#[test]
fn build_builtin_keybindings_applies_the_reserved_wins_rule_in_both_orders() {
    // 非保留项先写、保留项后写：保留项覆盖。
    let mut config = KeybindingsConfig::new();
    config.insert(
        "app.clipboard.pasteImage".to_string(),
        KeyIdList::One("ctrl+c".to_string()),
    );
    config.insert(
        "app.clear".to_string(),
        KeyIdList::One("ctrl+c".to_string()),
    );
    assert_eq!(
        build_builtin_keybindings(&config)["ctrl+c"],
        builtin("app.clear", true)
    );

    // 保留项先写、非保留项后写：非保留项被跳过。
    let mut config = KeybindingsConfig::new();
    config.insert(
        "app.clear".to_string(),
        KeyIdList::One("ctrl+c".to_string()),
    );
    config.insert(
        "app.clipboard.pasteImage".to_string(),
        KeyIdList::One("ctrl+c".to_string()),
    );
    assert_eq!(
        build_builtin_keybindings(&config)["ctrl+c"],
        builtin("app.clear", true)
    );
}

#[test]
fn build_builtin_keybindings_handles_lists_lowercase_and_absent_keys() {
    let mut config = KeybindingsConfig::new();
    config.insert(
        "app.clear".to_string(),
        KeyIdList::Many(vec!["CTRL+X".to_string(), "Ctrl+Y".to_string()]),
    );
    let builtins = build_builtin_keybindings(&config);
    assert_eq!(builtins["ctrl+x"], builtin("app.clear", true));
    assert_eq!(builtins["ctrl+y"], builtin("app.clear", true));
    // KeybindingsConfig 没有 `undefined`：键不存在即 TS 的默认值（不覆盖）。
    assert!(!builtins.contains_key("ctrl+p"));

    // 非保留动作的多个键都是 restrict_override == false。
    let mut config = KeybindingsConfig::new();
    config.insert(
        "app.clipboard.pasteImage".to_string(),
        KeyIdList::Many(vec!["ctrl+v".to_string(), "alt+v".to_string()]),
    );
    let builtins = build_builtin_keybindings(&config);
    assert_eq!(
        builtins["alt+v"],
        builtin("app.clipboard.pasteImage", false)
    );
}

#[test]
fn build_builtin_keybindings_keeps_last_non_reserved_action_for_a_key() {
    // TS: 同一物理键被多个非保留动作绑定时，后者覆盖前者（顺序见配置）。
    let mut config = KeybindingsConfig::new();
    config.insert(
        "app.session.togglePath".to_string(),
        KeyIdList::One("ctrl+p".to_string()),
    );
    config.insert(
        "app.models.toggleProvider".to_string(),
        KeyIdList::One("ctrl+p".to_string()),
    );
    assert_eq!(
        build_builtin_keybindings(&config)["ctrl+p"],
        builtin("app.models.toggleProvider", false)
    );
}

#[test]
fn empty_keybindings_produce_no_builtins() {
    assert!(build_builtin_keybindings(&KeybindingsConfig::new()).is_empty());
}
