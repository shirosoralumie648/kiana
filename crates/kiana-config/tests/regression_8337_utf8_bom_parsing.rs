//! `packages/coding-agent/test/suite/regressions/8337-utf8-bom-parsing.test.ts` 的
//! `SettingsManager` 部分。
//!
//! `splitBom` / `parseFrontmatter` 属于 `kiana-core`（`utils/text.ts`、`utils/frontmatter.ts`），
//! 已由 `kiana-core` 的回归测试覆盖；这里只验证 settings.json 的 BOM 读取，以及写回时不带 BOM。

use std::fs;
use std::path::PathBuf;

use kiana_config::settings::SettingsManager;

// TS: "loads frontmatter and settings with a leading BOM"（SettingsManager 部分）
#[tokio::test]
async fn loads_settings_with_a_leading_bom_and_writes_without_bom() {
    let temp = tempfile::tempdir().expect("tempdir");
    let test_dir = temp.path().to_path_buf();
    let agent_dir = test_dir.join("agent");
    let project_dir = test_dir.join("project");
    fs::create_dir_all(project_dir.join(".kiana")).expect("project dir");
    fs::create_dir_all(&agent_dir).expect("agent dir");
    let global_settings_path: PathBuf = agent_dir.join("settings.json");
    fs::write(
        &global_settings_path,
        "\u{FEFF}{\"defaultModel\":\"global-model\"}",
    )
    .expect("write global settings");
    fs::write(
        project_dir.join(".kiana").join("settings.json"),
        "\u{FEFF}{\"defaultProvider\":\"project-provider\"}",
    )
    .expect("write project settings");

    let settings = SettingsManager::create(
        project_dir.to_str().expect("utf-8"),
        agent_dir.to_str().expect("utf-8"),
    );
    assert_eq!(
        settings.get_default_model(),
        Some("global-model".to_string())
    );
    assert_eq!(
        settings.get_default_provider(),
        Some("project-provider".to_string())
    );

    settings.set_theme("dark".to_string());
    settings.flush().await;
    let written = fs::read_to_string(&global_settings_path).expect("read settings");
    assert!(
        !written.starts_with('\u{FEFF}'),
        "settings.json must not be written with a BOM"
    );
}
