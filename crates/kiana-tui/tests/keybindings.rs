//! `test/keybindings.test.ts` 的逐条移植（7 个 it）。
//!
//! 全局单例（`setKeybindings`）是 `thread_local`，因此涉及全局的用例不在此文件；
//! 这里只测 `KeybindingsManager` 实例：全局单例是 `thread_local`，跨用例隔离困难，实例测试已覆盖管理器的全部行为。

use kiana_tui::keybindings::{KeybindingsConfig, KeybindingsManager, tui_keybindings_static};

fn config(entries: &[(&str, &[&str])]) -> KeybindingsConfig {
    let mut config = KeybindingsConfig::new();
    for (id, keys) in entries {
        let keys: Vec<String> = keys.iter().map(|key| (*key).to_string()).collect();
        config.insert((*id).to_string(), keys.into());
    }
    config
}

/// `binds Ctrl+J as a default newline alias`
#[test]
fn binds_ctrl_j_as_a_default_newline_alias() {
    let keybindings = KeybindingsManager::with_defaults();
    assert_eq!(
        keybindings.get_keys("tui.input.newLine"),
        vec!["shift+enter".to_string(), "ctrl+j".to_string()]
    );
    assert!(keybindings.matches("\n", "tui.input.newLine"));
    assert!(keybindings.matches("\x1b[106;5u", "tui.input.newLine"));
}

/// `binds modified and unmodified editor viewport navigation`
#[test]
fn binds_modified_and_unmodified_editor_viewport_navigation() {
    let keybindings = KeybindingsManager::with_defaults();
    assert_eq!(
        keybindings.get_keys("tui.editor.cursorLineStart"),
        vec!["home".to_string(), "ctrl+a".to_string()]
    );
    assert_eq!(
        keybindings.get_keys("tui.editor.cursorLineEnd"),
        vec!["end".to_string(), "ctrl+e".to_string()]
    );
    assert_eq!(
        keybindings.get_keys("tui.editor.pageUp"),
        vec!["pageUp".to_string(), "ctrl+pageUp".to_string()]
    );
    assert_eq!(
        keybindings.get_keys("tui.editor.pageDown"),
        vec!["pageDown".to_string(), "ctrl+pageDown".to_string()]
    );
}

/// `leaves dedicated prompt history navigation unbound by default`
#[test]
fn leaves_dedicated_prompt_history_navigation_unbound_by_default() {
    let keybindings = KeybindingsManager::with_defaults();
    assert!(
        keybindings
            .get_keys("tui.editor.historyPrevious")
            .is_empty()
    );
    assert!(keybindings.get_keys("tui.editor.historyNext").is_empty());
}

/// `binds unmodified terminal viewport shortcuts to alternate-screen navigation`
#[test]
fn binds_unmodified_terminal_viewport_shortcuts_to_alternate_screen_navigation() {
    let keybindings = KeybindingsManager::with_defaults();
    assert_eq!(keybindings.get_keys("tui.altScreen.pageUp"), vec!["pageUp"]);
    assert_eq!(
        keybindings.get_keys("tui.altScreen.pageDown"),
        vec!["pageDown"]
    );
    assert!(keybindings.get_keys("tui.altScreen.halfPageUp").is_empty());
    assert!(
        keybindings
            .get_keys("tui.altScreen.halfPageDown")
            .is_empty()
    );
    assert!(keybindings.get_keys("tui.altScreen.lineUp").is_empty());
    assert!(keybindings.get_keys("tui.altScreen.lineDown").is_empty());
    assert_eq!(
        keybindings.get_keys("tui.altScreen.previousPrompt"),
        vec!["ctrl+shift+up".to_string(), "ctrl+up".to_string()]
    );
    assert_eq!(
        keybindings.get_keys("tui.altScreen.nextPrompt"),
        vec!["ctrl+shift+down".to_string(), "ctrl+down".to_string()]
    );
    assert_eq!(
        keybindings.get_keys("tui.altScreen.search"),
        vec!["ctrl+shift+f"]
    );
    assert_eq!(
        keybindings.get_keys("tui.altScreen.searchNext"),
        vec!["enter".to_string(), "ctrl+g".to_string()]
    );
    assert_eq!(
        keybindings.get_keys("tui.altScreen.searchPrevious"),
        vec!["shift+enter".to_string(), "ctrl+shift+g".to_string()]
    );
    assert_eq!(
        keybindings.get_keys("tui.altScreen.searchClose"),
        vec!["escape"]
    );
    assert_eq!(keybindings.get_keys("tui.altScreen.top"), vec!["ctrl+home"]);
    assert_eq!(
        keybindings.get_keys("tui.altScreen.bottom"),
        vec!["ctrl+end"]
    );
}

/// `does not evict selector confirm when input submit is rebound`
#[test]
fn does_not_evict_selector_confirm_when_input_submit_is_rebound() {
    let keybindings = KeybindingsManager::new(
        tui_keybindings_static().clone(),
        config(&[("tui.input.submit", &["enter", "ctrl+enter"])]),
    );
    assert_eq!(
        keybindings.get_keys("tui.input.submit"),
        vec!["enter".to_string(), "ctrl+enter".to_string()]
    );
    assert_eq!(keybindings.get_keys("tui.select.confirm"), vec!["enter"]);
}

/// `does not evict cursor bindings when another action reuses the same key`
#[test]
fn does_not_evict_cursor_bindings_when_another_action_reuses_the_same_key() {
    let keybindings = KeybindingsManager::new(
        tui_keybindings_static().clone(),
        config(&[("tui.select.up", &["up", "ctrl+p"])]),
    );
    assert_eq!(
        keybindings.get_keys("tui.select.up"),
        vec!["up".to_string(), "ctrl+p".to_string()]
    );
    assert_eq!(keybindings.get_keys("tui.editor.cursorUp"), vec!["up"]);
}

/// `still reports direct user binding conflicts without evicting defaults`
#[test]
fn still_reports_direct_user_binding_conflicts_without_evicting_defaults() {
    let keybindings = KeybindingsManager::new(
        tui_keybindings_static().clone(),
        config(&[
            ("tui.input.submit", &["ctrl+x"]),
            ("tui.select.confirm", &["ctrl+x"]),
        ]),
    );
    let conflicts = keybindings.get_conflicts();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].key, "ctrl+x");
    assert_eq!(
        conflicts[0].keybindings,
        vec![
            "tui.input.submit".to_string(),
            "tui.select.confirm".to_string()
        ]
    );
    assert_eq!(
        keybindings.get_keys("tui.editor.cursorLeft"),
        vec!["left".to_string(), "ctrl+b".to_string()]
    );
}
