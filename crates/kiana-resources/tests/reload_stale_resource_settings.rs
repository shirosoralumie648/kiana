//! 移植 `packages/coding-agent/test/suite/regressions/2753-reload-stale-resource-settings.test.ts`。
//!
//! TS 的回归测试走完整的 `AgentSessionRuntime`（`kiana-session`/`kiana-coding-agent`），本单元
//! 只拥有资源加载器，因此这里在加载器层复刻同一因果链：
//! `reload()` 必须重新读取 settings，顶层的 `prompts` 过滤设置要在第二次 reload 后生效。
//! 会话层断言（`session.promptTemplates` / `services.settingsManager`）属于 `kiana-session`。

#[path = "resource_loader_support/mod.rs"]
mod support;

use std::path::Path;

use kiana_config::settings::SettingsManager;

use support::{TestDirs, new_loader, write_file};

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[tokio::test(flavor = "current_thread")]
async fn applies_updated_top_level_prompt_settings_on_reload_after_startup() {
    let dirs = TestDirs::new();
    write_file(&dirs.agent_dir, "prompts/test.md", "Echo test prompt\n");

    let settings_manager = SettingsManager::create(&s(&dirs.cwd), &s(&dirs.agent_dir));
    let loader = new_loader(&dirs, |options| {
        options.settings_manager = Some(settings_manager.clone());
    });

    loader.reload(None).await.expect("initial reload");
    assert!(
        loader
            .get_prompts()
            .prompts
            .iter()
            .any(|prompt| prompt.name == "test")
    );

    write_file(
        &dirs.agent_dir,
        "settings.json",
        &format!(
            "{}\n",
            serde_json::to_string(&serde_json::json!({ "prompts": ["-prompts/test.md"] }))
                .expect("serialize settings")
        ),
    );

    loader
        .reload(None)
        .await
        .expect("reload after settings change");

    assert_eq!(
        settings_manager.get_global_settings().prompts,
        Some(vec!["-prompts/test.md".to_string()])
    );
    assert!(
        !loader
            .get_prompts()
            .prompts
            .iter()
            .any(|prompt| prompt.name == "test")
    );
}
