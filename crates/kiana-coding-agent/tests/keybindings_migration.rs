//! `test/keybindings-migration.test.ts` 的逐条移植。

use std::ffi::OsString;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use kiana_coding_agent::migrations::run_migrations;
use kiana_config::keybindings::KeybindingsManager;
use kiana_tui::keybindings::KeyIdList;
use serde_json::{Value, json};
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct AgentDirEnv {
    previous: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl AgentDirEnv {
    fn set(path: &Path) -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var_os(kiana_core::config::ENV_AGENT_DIR);
        // SAFETY: ENV_LOCK 串行化本二进制内所有会修改 KIANA_CODING_AGENT_DIR 的测试。
        unsafe {
            std::env::set_var(kiana_core::config::ENV_AGENT_DIR, path);
        }
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for AgentDirEnv {
    fn drop(&mut self) {
        // SAFETY: 同上，测试仍在 ENV_LOCK 保护下。
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var(kiana_core::config::ENV_AGENT_DIR, value),
                None => std::env::remove_var(kiana_core::config::ENV_AGENT_DIR),
            }
        }
    }
}

fn create_agent_dir(config: &Value) -> TempDir {
    let agent_dir = TempDir::new().expect("agent dir");
    std::fs::write(
        agent_dir.path().join("keybindings.json"),
        format!("{}\n", serde_json::to_string_pretty(config).expect("json")),
    )
    .expect("write keybindings");
    agent_dir
}

fn key_ids(value: &KeyIdList) -> Vec<String> {
    match value {
        KeyIdList::One(key) => vec![key.clone()],
        KeyIdList::Many(keys) => keys.clone(),
    }
}

#[test]
fn rewrites_old_key_names_to_namespaced_ids() {
    let agent_dir = create_agent_dir(&json!({
        "cursorUp": ["up", "ctrl+p"],
        "expandTools": "ctrl+x",
    }));
    {
        let _env = AgentDirEnv::set(agent_dir.path());
        run_migrations(agent_dir.path());
    }

    let migrated: Value = serde_json::from_str(
        &std::fs::read_to_string(agent_dir.path().join("keybindings.json")).expect("read"),
    )
    .expect("parse");
    assert_eq!(
        migrated,
        json!({
            "tui.editor.cursorUp": ["up", "ctrl+p"],
            "app.tools.expand": "ctrl+x",
        })
    );
}

#[test]
fn keeps_the_namespaced_value_when_old_and_new_names_both_exist() {
    let agent_dir = create_agent_dir(&json!({
        "expandTools": "ctrl+x",
        "app.tools.expand": "ctrl+y",
    }));
    {
        let _env = AgentDirEnv::set(agent_dir.path());
        run_migrations(agent_dir.path());
    }

    let migrated: Value = serde_json::from_str(
        &std::fs::read_to_string(agent_dir.path().join("keybindings.json")).expect("read"),
    )
    .expect("parse");
    assert_eq!(migrated, json!({"app.tools.expand": "ctrl+y"}));
}

#[test]
fn loads_old_key_names_in_memory_before_the_file_is_rewritten() {
    let agent_dir = create_agent_dir(&json!({
        "selectConfirm": "enter",
        "interrupt": "ctrl+x",
    }));

    let keybindings = KeybindingsManager::create(agent_dir.path());

    let user_bindings = keybindings.get_user_bindings();
    assert_eq!(
        user_bindings.get("tui.select.confirm").map(key_ids),
        Some(vec!["enter".to_string()])
    );
    assert_eq!(
        user_bindings.get("app.interrupt").map(key_ids),
        Some(vec!["ctrl+x".to_string()])
    );
    let effective = keybindings.get_effective_config();
    assert_eq!(
        effective.get("tui.select.confirm").map(key_ids),
        Some(vec!["enter".to_string()])
    );
    assert_eq!(
        effective.get("app.interrupt").map(key_ids),
        Some(vec!["ctrl+x".to_string()])
    );
}
