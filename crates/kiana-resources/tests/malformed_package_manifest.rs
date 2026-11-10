//! `packages/coding-agent/test/suite/regressions/7187-malformed-package-manifest.test.ts`
//! 的 Rust 移植：`package.json#kiana` 里的非法字段被忽略，但同文件里的合法字段仍然生效。

#![cfg(unix)]
#![allow(clippy::await_holding_lock)] // TestEnv 在 await 期间持有环境锁以串行化进程环境。

mod common;

use std::path::Path;

use common::TestEnv;
use kiana_config::settings::schema::{PackageSource, Settings};
use kiana_resources::package::PackageManager;

#[tokio::test]
async fn ignores_invalid_resource_fields_without_dropping_valid_fields() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.agent_dir).join("npm/node_modules/bad-package");
    let skill_path = package_dir.join("skills/bad/SKILL.md");
    let prompt_path = package_dir.join("prompts/valid.md");
    env.write(
        &skill_path,
        "---\nname: bad\ndescription: Must not load\n---\n",
    );
    env.write(&prompt_path, "Valid prompt\n");
    env.write(
        package_dir.join("package.json"),
        "{\"name\":\"bad-package\",\"version\":\"1.0.0\",\"kiana\":{\"skills\":\"./skills\",\"prompts\":[\"./prompts\"]}}",
    );
    let settings_manager = env.settings_manager(Settings {
        packages: Some(vec![PackageSource::Text("npm:bad-package".to_string())]),
        ..Settings::default()
    });
    let package_manager = env.manager(&settings_manager);

    let resources = package_manager.resolve(None).await.expect("resolve");
    assert!(
        !resources
            .skills
            .iter()
            .any(|resource| resource.path == skill_path.to_string_lossy())
    );
    assert!(
        resources
            .prompts
            .iter()
            .any(|resource| resource.path == prompt_path.to_string_lossy())
    );
}
