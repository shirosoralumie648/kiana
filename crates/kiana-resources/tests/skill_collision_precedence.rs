//! 移植 `packages/coding-agent/test/suite/regressions/2781-skill-collision-precedence.test.ts`
//! （4 个测试）。
//!
//! 用户/项目的自动发现技能必须优先于包内技能；冲突诊断把包内技能记为 loser。

#[path = "resource_loader_support/mod.rs"]
mod support;

use std::path::Path;

use support::{TestDirs, new_loader, write_file};

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

struct Fixture {
    dirs: TestDirs,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dirs: TestDirs::new(),
        }
    }

    fn create_package_with_skill(&self, name: &str, description: &str) -> std::path::PathBuf {
        let pkg_dir = self.dirs.temp_path(&format!("fake-package-{name}"));
        write_file(
            &pkg_dir,
            "package.json",
            &format!(
                "{{\n  \"name\": \"fake-pkg-{name}\",\n  \"version\": \"1.0.0\",\n  \"kiana\": {{ \"skills\": [\"skills/{name}\"] }}\n}}"
            ),
        );
        write_file(
            &pkg_dir,
            &format!("skills/{name}/SKILL.md"),
            &format!("---\nname: {name}\ndescription: {description}\n---\nPackage skill content"),
        );
        pkg_dir
    }

    fn create_user_skill(&self, name: &str, description: &str) -> std::path::PathBuf {
        write_file(
            &self.dirs.agent_dir,
            &format!("skills/{name}/SKILL.md"),
            &format!("---\nname: {name}\ndescription: {description}\n---\nUser skill content"),
        )
    }

    fn create_project_skill(&self, name: &str, description: &str) -> std::path::PathBuf {
        write_file(
            &self.dirs.cwd,
            &format!(".kiana/skills/{name}/SKILL.md"),
            &format!("---\nname: {name}\ndescription: {description}\n---\nProject skill content"),
        )
    }

    fn create_settings_with_package(&self, pkg_dir: &Path, scope: &str) {
        let settings_dir = if scope == "user" {
            self.dirs.agent_dir.clone()
        } else {
            self.dirs.cwd.join(".kiana")
        };
        write_file(
            &settings_dir,
            "settings.json",
            &format!(
                "{{\n  \"packages\": [{}]\n}}",
                serde_json::to_string(&s(pkg_dir)).expect("quote")
            ),
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn user_auto_discovered_skill_should_override_package_skill_with_same_name() {
    let fixture = Fixture::new();
    let pkg_dir = fixture.create_package_with_skill("web-fetch", "Package web-fetch skill");
    let user_skill_path = fixture.create_user_skill("web-fetch", "User web-fetch override");
    fixture.create_settings_with_package(&pkg_dir, "user");

    let loader = new_loader(&fixture.dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let skill = loader
        .get_skills()
        .skills
        .into_iter()
        .find(|skill| skill.name == "web-fetch")
        .expect("web-fetch skill");
    assert_eq!(skill.file_path, s(&user_skill_path));
    assert_eq!(skill.description, "User web-fetch override");
}

#[tokio::test(flavor = "current_thread")]
async fn project_auto_discovered_skill_should_override_package_skill_with_same_name() {
    let fixture = Fixture::new();
    let pkg_dir = fixture.create_package_with_skill("web-fetch", "Package web-fetch skill");
    let project_skill_path =
        fixture.create_project_skill("web-fetch", "Project web-fetch override");
    fixture.create_settings_with_package(&pkg_dir, "user");

    let loader = new_loader(&fixture.dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let skill = loader
        .get_skills()
        .skills
        .into_iter()
        .find(|skill| skill.name == "web-fetch")
        .expect("web-fetch skill");
    assert_eq!(skill.file_path, s(&project_skill_path));
    assert_eq!(skill.description, "Project web-fetch override");
}

#[tokio::test(flavor = "current_thread")]
async fn project_skill_should_override_user_skill_which_should_override_package_skill() {
    let fixture = Fixture::new();
    let pkg_dir = fixture.create_package_with_skill("web-fetch", "Package web-fetch skill");
    fixture.create_user_skill("web-fetch", "User web-fetch override");
    let project_skill_path =
        fixture.create_project_skill("web-fetch", "Project web-fetch override");
    fixture.create_settings_with_package(&pkg_dir, "user");

    let loader = new_loader(&fixture.dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let skill = loader
        .get_skills()
        .skills
        .into_iter()
        .find(|skill| skill.name == "web-fetch")
        .expect("web-fetch skill");
    assert_eq!(skill.file_path, s(&project_skill_path));
    assert_eq!(skill.description, "Project web-fetch override");
}

#[tokio::test(flavor = "current_thread")]
async fn collision_diagnostics_should_report_package_skill_as_loser_when_user_skill_wins() {
    let fixture = Fixture::new();
    let pkg_dir = fixture.create_package_with_skill("web-fetch", "Package web-fetch skill");
    fixture.create_user_skill("web-fetch", "User web-fetch override");
    fixture.create_settings_with_package(&pkg_dir, "user");

    let loader = new_loader(&fixture.dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let diagnostics = loader.get_skills().diagnostics;
    let collision = diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.kind == kiana_core::diagnostics::DiagnosticKind::Collision
                && diagnostic
                    .collision
                    .as_ref()
                    .is_some_and(|collision| collision.name == "web-fetch")
        })
        .expect("web-fetch collision diagnostic");
    assert!(
        collision
            .collision
            .as_ref()
            .expect("collision")
            .loser_path
            .contains("fake-package")
    );
}
