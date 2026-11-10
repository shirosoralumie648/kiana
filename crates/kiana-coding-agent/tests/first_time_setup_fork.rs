//! `test/first-time-setup-fork.test.ts` 的 Rust 版本。
//!
//! TS 用 `vi.mock("../src/config.ts")` 改写 `PACKAGE_NAME`；Rust 侧把发行版元数据作为
//! [`FirstTimeSetupOptionsInput`] 的显式输入（`should_run_first_time_setup_with`）。

use std::path::Path;

use kiana_coding_agent::cli::startup_ui::{
    FirstTimeSetupOptionsInput, OFFICIAL_APP_NAME, OFFICIAL_CONFIG_DIR_NAME,
    should_run_first_time_setup_with,
};

#[test]
fn returns_false_for_a_forked_package() {
    let temp = tempfile::tempdir().expect("tempdir");
    let settings_path = temp.path().join("settings.json");
    let options = FirstTimeSetupOptionsInput {
        package_name: "@example/kiana-coding-agent".to_string(),
        app_name: OFFICIAL_APP_NAME.to_string(),
        config_dir_name: OFFICIAL_CONFIG_DIR_NAME.to_string(),
        experimental_features: true,
        custom_agent_dir: false,
        settings_path: settings_path.clone(),
    };
    assert!(!should_run_first_time_setup_with(&options));

    // 官方名字 + 相同输入 → true（证明分支确实由包名触发）。
    let official = FirstTimeSetupOptionsInput {
        package_name: "@earendil-works/kiana-coding-agent".to_string(),
        ..options
    };
    assert!(should_run_first_time_setup_with(&official));
    let _ = Path::new(&settings_path);
}
