//! `core/project-trust.ts` 的决策表测试（TS 侧没有直接测试；七步判定 + 回调语义）。
//!
//! 覆盖：
//! 1. `trustOverride`；
//! 2. 无需要信任的资源；
//! 3. 扩展事件（`trusted === "yes"` / `remember` / 错误回调 / 无结果回退）；
//! 4. trust store 决定；
//! 5. `defaultProjectTrust`；
//! 6. 无 UI；
//! 7. UI 选择与 `saveProjectTrustPromptResult`。另测提示文案逐字。

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_config::settings::schema::DefaultProjectTrust;
use kiana_config::trust::manager::ProjectTrustStore;
use kiana_config::trust::project_trust::{
    EmitProjectTrustResult, ProjectTrustContext, ProjectTrustEmitter, ProjectTrustEvent,
    ProjectTrustEventError, ProjectTrustEventOutcome, ProjectTrustUi, ResolveProjectTrustedOptions,
    format_project_trust_prompt, resolve_project_trusted, save_project_trust_prompt_result,
};
use tempfile::TempDir;

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// 创建带 trust-requiring 资源的项目目录。
fn project_with_resources(temp_dir: &TempDir) -> String {
    let cwd = temp_dir.path().join("project");
    fs::create_dir_all(cwd.join(".kiana")).unwrap();
    fs::write(cwd.join(".kiana").join("settings.json"), "{}").unwrap();
    path_string(&cwd)
}

fn agent_dir(temp_dir: &TempDir) -> String {
    let dir = temp_dir.path().join("agent");
    fs::create_dir_all(&dir).unwrap();
    path_string(&dir)
}

#[derive(Default)]
struct RecordingUi {
    selected: Option<String>,
    calls: Mutex<Vec<(String, Vec<String>)>>,
}

#[async_trait]
impl ProjectTrustUi for RecordingUi {
    async fn select(&self, title: &str, options: Vec<String>) -> Option<String> {
        self.calls
            .lock()
            .unwrap()
            .push((title.to_string(), options));
        self.selected.clone()
    }
}

struct StaticEmitter {
    outcome: EmitProjectTrustResult,
}

#[async_trait]
impl ProjectTrustEmitter for StaticEmitter {
    async fn emit_project_trust(&self, event: ProjectTrustEvent) -> EmitProjectTrustResult {
        assert!(!event.cwd.is_empty());
        self.outcome.clone()
    }
}

fn base_options(cwd: String, trust_store: ProjectTrustStore) -> ResolveProjectTrustedOptions {
    ResolveProjectTrustedOptions {
        cwd,
        trust_store,
        trust_override: None,
        default_project_trust: None,
        project_trust_context: ProjectTrustContext::default(),
        emit_project_trust: None,
        on_extension_error: None,
    }
}

/// 1. trustOverride 优先于一切
#[tokio::test]
async fn trust_override_wins_over_everything() {
    let temp_dir = TempDir::new().unwrap();
    let cwd = project_with_resources(&temp_dir);
    let store = ProjectTrustStore::new(&agent_dir(&temp_dir));

    let mut options = base_options(cwd.clone(), store.clone());
    options.trust_override = Some(true);
    options.default_project_trust = Some(DefaultProjectTrust::Never);
    assert!(resolve_project_trusted(options).await.unwrap());

    let mut options = base_options(cwd, store);
    options.trust_override = Some(false);
    options.default_project_trust = Some(DefaultProjectTrust::Always);
    assert!(!resolve_project_trusted(options).await.unwrap());
}

/// 2. 没有需要信任的项目资源时直接 true
#[tokio::test]
async fn returns_true_without_trust_requiring_resources() {
    let temp_dir = TempDir::new().unwrap();
    let cwd = path_string(&temp_dir.path().join("plain"));
    fs::create_dir_all(&cwd).unwrap();
    let mut options = base_options(cwd, ProjectTrustStore::new(&agent_dir(&temp_dir)));
    options.default_project_trust = Some(DefaultProjectTrust::Never);
    assert!(resolve_project_trusted(options).await.unwrap());
}

/// 3. 扩展事件：yes/no、remember、错误回调、无结果回退
#[tokio::test]
async fn extension_event_outcome_is_applied() {
    let temp_dir = TempDir::new().unwrap();
    let cwd = project_with_resources(&temp_dir);
    let store = ProjectTrustStore::new(&agent_dir(&temp_dir));

    // trusted === "yes" + remember → 写入 true
    let mut options = base_options(cwd.clone(), store.clone());
    options.emit_project_trust = Some(Arc::new(StaticEmitter {
        outcome: EmitProjectTrustResult {
            result: Some(ProjectTrustEventOutcome {
                trusted: "yes".to_string(),
                remember: Some(true),
            }),
            errors: Vec::new(),
        },
    }));
    assert!(resolve_project_trusted(options).await.unwrap());
    assert_eq!(store.get(&cwd).unwrap(), Some(true));

    // trusted === "no"（任意非 "yes"）+ remember → 写入 false
    let mut options = base_options(cwd.clone(), store.clone());
    options.emit_project_trust = Some(Arc::new(StaticEmitter {
        outcome: EmitProjectTrustResult {
            result: Some(ProjectTrustEventOutcome {
                trusted: "no".to_string(),
                remember: Some(true),
            }),
            errors: Vec::new(),
        },
    }));
    assert!(!resolve_project_trusted(options).await.unwrap());
    assert_eq!(store.get(&cwd).unwrap(), Some(false));

    // remember 不是 true（缺失 / false）→ 不写
    store.set(&cwd, None).unwrap();
    let mut options = base_options(cwd.clone(), store.clone());
    options.emit_project_trust = Some(Arc::new(StaticEmitter {
        outcome: EmitProjectTrustResult {
            result: Some(ProjectTrustEventOutcome {
                trusted: "yes".to_string(),
                remember: None,
            }),
            errors: Vec::new(),
        },
    }));
    assert!(resolve_project_trusted(options).await.unwrap());
    assert_eq!(store.get(&cwd).unwrap(), None);

    // 错误回调逐字
    let messages: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let messages_in_callback = Arc::clone(&messages);
    let mut options = base_options(cwd.clone(), store.clone());
    options.emit_project_trust = Some(Arc::new(StaticEmitter {
        outcome: EmitProjectTrustResult {
            result: None,
            errors: vec![ProjectTrustEventError {
                extension_path: "/ext/index.ts".to_string(),
                error: "boom".to_string(),
            }],
        },
    }));
    options.on_extension_error = Some(Arc::new(move |message: &str| {
        messages_in_callback
            .lock()
            .unwrap()
            .push(message.to_string());
    }));
    // 无结果 → 回退到 store / 默认值（这里默认 ask 且无 UI → false）
    options.default_project_trust = Some(DefaultProjectTrust::Ask);
    assert!(!resolve_project_trusted(options).await.unwrap());
    assert_eq!(
        *messages.lock().unwrap(),
        vec!["Extension \"/ext/index.ts\" project_trust error: boom".to_string()]
    );
}

/// 4. trust store 的最近决定优先于 defaultProjectTrust
#[tokio::test]
async fn stored_decision_wins_over_defaults() {
    let temp_dir = TempDir::new().unwrap();
    let cwd = project_with_resources(&temp_dir);
    let store = ProjectTrustStore::new(&agent_dir(&temp_dir));
    store.set(&cwd, Some(true)).unwrap();

    let mut options = base_options(cwd.clone(), store.clone());
    options.default_project_trust = Some(DefaultProjectTrust::Never);
    assert!(resolve_project_trusted(options).await.unwrap());

    store.set(&cwd, Some(false)).unwrap();
    let mut options = base_options(cwd.clone(), store);
    options.default_project_trust = Some(DefaultProjectTrust::Always);
    assert!(!resolve_project_trusted(options).await.unwrap());
}

/// 5. defaultProjectTrust 的 always / never
#[tokio::test]
async fn default_project_trust_setting_is_applied() {
    let temp_dir = TempDir::new().unwrap();
    let cwd = project_with_resources(&temp_dir);
    let store = ProjectTrustStore::new(&agent_dir(&temp_dir));

    let mut options = base_options(cwd.clone(), store.clone());
    options.default_project_trust = Some(DefaultProjectTrust::Always);
    assert!(resolve_project_trusted(options).await.unwrap());

    let mut options = base_options(cwd, store);
    options.default_project_trust = Some(DefaultProjectTrust::Never);
    assert!(!resolve_project_trusted(options).await.unwrap());
}

/// 6. ask + 无 UI → false
#[tokio::test]
async fn ask_without_ui_returns_false() {
    let temp_dir = TempDir::new().unwrap();
    let cwd = project_with_resources(&temp_dir);
    let store = ProjectTrustStore::new(&agent_dir(&temp_dir));
    let mut options = base_options(cwd, store);
    options.default_project_trust = Some(DefaultProjectTrust::Ask);
    options.project_trust_context = ProjectTrustContext {
        has_ui: true,
        ui: None,
    };
    assert!(!resolve_project_trusted(options).await.unwrap());
}

/// 7. UI 选择：保存 updates（会话级选项不写盘），取消 → false
#[tokio::test]
async fn ui_selection_saves_updates() {
    let temp_dir = TempDir::new().unwrap();
    let cwd = project_with_resources(&temp_dir);
    let store = ProjectTrustStore::new(&agent_dir(&temp_dir));

    // 选择 "Trust"：写入 cwd=true。
    let ui = Arc::new(RecordingUi {
        selected: Some("Trust".to_string()),
        ..Default::default()
    });
    let mut options = base_options(cwd.clone(), store.clone());
    options.project_trust_context = ProjectTrustContext {
        has_ui: true,
        ui: Some(ui.clone()),
    };
    assert!(resolve_project_trusted(options).await.unwrap());
    assert_eq!(store.get(&cwd).unwrap(), Some(true));
    {
        let calls = ui.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, format_project_trust_prompt(&cwd));
        let expected_labels: Vec<String> = vec![
            "Trust".to_string(),
            format!(
                "Trust parent folder ({})",
                kiana_config::trust::manager::get_project_trust_parent_path(&cwd).unwrap()
            ),
            "Trust (this session only)".to_string(),
            "Do not trust".to_string(),
            "Do not trust (this session only)".to_string(),
        ];
        assert_eq!(calls[0].1, expected_labels);
    }

    // "Do not trust"：写入 cwd=false（先清掉上一例保存的决定，让流程走到 UI）。
    store.set(&cwd, None).unwrap();
    let ui = Arc::new(RecordingUi {
        selected: Some("Do not trust".to_string()),
        ..Default::default()
    });
    let mut options = base_options(cwd.clone(), store.clone());
    options.project_trust_context = ProjectTrustContext {
        has_ui: true,
        ui: Some(ui),
    };
    assert!(!resolve_project_trusted(options).await.unwrap());
    assert_eq!(store.get(&cwd).unwrap(), Some(false));

    // 会话级选项：updates 为空，不写盘。
    let trust_ui = Arc::new(RecordingUi {
        selected: Some("Trust (this session only)".to_string()),
        ..Default::default()
    });
    store.set(&cwd, None).unwrap();
    let mut options = base_options(cwd.clone(), store.clone());
    options.project_trust_context = ProjectTrustContext {
        has_ui: true,
        ui: Some(trust_ui),
    };
    assert!(resolve_project_trusted(options).await.unwrap());
    assert_eq!(store.get(&cwd).unwrap(), None);

    // 取消（select 返回 None）→ false，不写盘。
    let ui = Arc::new(RecordingUi {
        selected: None,
        ..Default::default()
    });
    let mut options = base_options(cwd.clone(), store.clone());
    options.project_trust_context = ProjectTrustContext {
        has_ui: true,
        ui: Some(ui),
    };
    assert!(!resolve_project_trusted(options).await.unwrap());
    assert_eq!(store.get(&cwd).unwrap(), None);
}

/// formatProjectTrustPrompt 逐字
#[test]
fn formats_the_project_trust_prompt_verbatim() {
    assert_eq!(
        format_project_trust_prompt("/work/project"),
        "Trust project folder?\n/work/project\n\nThis allows kiana to load .kiana settings and resources, install missing project packages, and execute project extensions."
    );
    // 黄金文件（Node 生成）交叉验证。
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/trust/prompt.json")).unwrap();
    assert_eq!(
        format_project_trust_prompt("/work/project"),
        fixture["prompt"].as_str().unwrap()
    );
}

/// saveProjectTrustPromptResult：updates 为空时不写盘
#[test]
fn save_project_trust_prompt_result_only_writes_non_empty_updates() {
    let temp_dir = TempDir::new().unwrap();
    let store = ProjectTrustStore::new(&agent_dir(&temp_dir));
    let session_only = kiana_config::trust::manager::ProjectTrustOption {
        label: "Trust (this session only)".to_string(),
        trusted: true,
        updates: Vec::new(),
        saved_path: None,
    };
    save_project_trust_prompt_result(&store, &session_only).unwrap();
    assert!(!Path::new(store.trust_path()).exists());
}
