//! `components/config-selector.ts` 的移植测试。
//!
//! 覆盖 `package-command-paths.test.ts` 中与 `ConfigSelectorComponent` 相关的用例
//! （project/global 资源路径分组、builtin 开关、本地覆盖、显示名），其余 32 个用例由
//! `kiana-resources` 的包管理测试覆盖。另附从 TS 参考实现生成的 80 列黄金渲染。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Once;

use kiana_config::settings::SettingsManager;
use kiana_config::settings::schema::{PackageSource, PackageSourceObject, Settings};
use kiana_core::source_info::{PathMetadata, SourceOrigin, SourceScope};
use kiana_interactive::components::config_selector::{
    ConfigSelectorComponent, ConfigSelectorOptions, ConfigWriteScope, ScopedResolvedPaths,
};
use kiana_resources::package::types::{ResolvedPaths, ResolvedResource};
use kiana_tui::keybindings::{KeybindingsConfig, KeybindingsManager};
use kiana_tui::{Component, Focusable, TerminalCapabilities, set_capabilities, set_keybindings};

const AGENT: &str = "/opt/kiana-agent";
const CWD: &str = "/work/project";
const PKG: &str = "/opt/pkg/kiana-tools";

fn setup() {
    static THEME_ONCE: Once = Once::new();
    THEME_ONCE.call_once(|| kiana_theme::init_theme(Some("dark"), false));
    set_capabilities(TerminalCapabilities {
        images: None,
        true_color: true,
        hyperlinks: false,
    });
    set_keybindings(KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        KeybindingsConfig::new(),
    ));
}

fn res(
    path: &str,
    enabled: bool,
    source: &str,
    scope: SourceScope,
    origin: SourceOrigin,
    base_dir: Option<&str>,
) -> ResolvedResource {
    ResolvedResource {
        path: path.to_string(),
        enabled,
        metadata: PathMetadata {
            source: source.to_string(),
            scope,
            origin,
            base_dir: base_dir.map(str::to_string),
            package_root: None,
        },
    }
}

fn extension_paths(
    package_root: &str,
    source: &str,
    scope: SourceScope,
    names: &[&str],
) -> ResolvedPaths {
    ResolvedPaths {
        extensions: names
            .iter()
            .map(|name| {
                res(
                    &format!("{package_root}/extensions/{name}"),
                    true,
                    source,
                    scope,
                    SourceOrigin::Package,
                    Some(package_root),
                )
            })
            .collect(),
        ..ResolvedPaths::default()
    }
}

fn probe_global_paths() -> ResolvedPaths {
    ResolvedPaths {
        extensions: vec![
            res(
                &format!("{PKG}/extensions/bar.ts"),
                true,
                "npm:kiana-tools",
                SourceScope::User,
                SourceOrigin::Package,
                Some(PKG),
            ),
            res(
                &format!("{PKG}/extensions/foo.ts"),
                false,
                "npm:kiana-tools",
                SourceScope::User,
                SourceOrigin::Package,
                Some(PKG),
            ),
            res(
                &format!("{AGENT}/extensions/ext1.ts"),
                true,
                "auto",
                SourceScope::User,
                SourceOrigin::TopLevel,
                Some(&format!("{AGENT}/extensions")),
            ),
            res(
                "builtin:read",
                true,
                "builtin",
                SourceScope::User,
                SourceOrigin::TopLevel,
                None,
            ),
            res(
                &format!("{AGENT}/extensions/local.ts"),
                true,
                "local",
                SourceScope::User,
                SourceOrigin::TopLevel,
                None,
            ),
        ],
        skills: vec![
            res(
                &format!("{PKG}/skills/alpha/SKILL.md"),
                true,
                "npm:kiana-tools",
                SourceScope::User,
                SourceOrigin::Package,
                Some(PKG),
            ),
            res(
                &format!("{AGENT}/skills/beta.md"),
                true,
                "auto",
                SourceScope::User,
                SourceOrigin::TopLevel,
                Some(&format!("{AGENT}/skills")),
            ),
        ],
        prompts: vec![
            res(
                &format!("{PKG}/prompts/review.md"),
                true,
                "npm:kiana-tools",
                SourceScope::User,
                SourceOrigin::Package,
                Some(PKG),
            ),
            res(
                &format!("{AGENT}/prompts/prompt1.md"),
                false,
                "auto",
                SourceScope::User,
                SourceOrigin::TopLevel,
                Some(&format!("{AGENT}/prompts")),
            ),
        ],
        themes: vec![res(
            &format!("{PKG}/themes/solar.json"),
            true,
            "npm:kiana-tools",
            SourceScope::User,
            SourceOrigin::Package,
            Some(PKG),
        )],
        install_failures: None,
    }
}

fn probe_project_paths() -> ResolvedPaths {
    ResolvedPaths {
        extensions: vec![
            res(
                &format!("{PKG}/extensions/bar.ts"),
                true,
                "npm:kiana-tools",
                SourceScope::User,
                SourceOrigin::Package,
                Some(PKG),
            ),
            res(
                &format!("{CWD}/.kiana/extensions/proj.ts"),
                true,
                "auto",
                SourceScope::Project,
                SourceOrigin::TopLevel,
                Some(&format!("{CWD}/.kiana/extensions")),
            ),
            res(
                "builtin:mcp",
                true,
                "builtin",
                SourceScope::Project,
                SourceOrigin::TopLevel,
                None,
            ),
        ],
        ..ResolvedPaths::default()
    }
}

fn scoped(global: ResolvedPaths, project: ResolvedPaths) -> ScopedResolvedPaths {
    let mut paths = ScopedResolvedPaths::new();
    paths.insert(ConfigWriteScope::Global, global);
    paths.insert(ConfigWriteScope::Project, project);
    paths
}

fn settings_manager(
    packages: Option<Vec<PackageSource>>,
    extensions: Option<Vec<String>>,
) -> SettingsManager {
    SettingsManager::in_memory(Settings {
        packages,
        extensions,
        ..Settings::default()
    })
}

fn probe_settings_manager() -> SettingsManager {
    settings_manager(
        Some(vec![
            PackageSource::Text("npm:kiana-tools".to_string()),
            PackageSource::Object(PackageSourceObject {
                source: Some("npm:other".to_string()),
                extensions: Some(vec!["-x.ts".to_string()]),
                ..PackageSourceObject::default()
            }),
        ]),
        Some(vec!["-builtin:read".to_string()]),
    )
}

fn noop() -> Rc<dyn Fn()> {
    Rc::new(|| {})
}

fn make_component(
    paths: &ScopedResolvedPaths,
    settings_manager: SettingsManager,
    write_scope: ConfigWriteScope,
    project_mode_available: bool,
) -> ConfigSelectorComponent {
    ConfigSelectorComponent::new(
        paths,
        settings_manager,
        CWD,
        AGENT,
        noop(),
        noop(),
        noop(),
        ConfigSelectorOptions {
            terminal_height: Some(24),
            write_scope,
            project_mode_available,
        },
    )
}

fn assert_lines(actual: Vec<String>, expected: &[&str]) {
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_string()).collect();
    assert_eq!(actual, expected);
}

fn json_packages(manager: &SettingsManager) -> serde_json::Value {
    serde_json::to_value(manager.get_project_settings().packages).expect("packages serialize")
}

fn json_extensions(value: Option<Vec<String>>) -> serde_json::Value {
    serde_json::to_value(value).expect("extensions serialize")
}

// ---------------------------------------------------------------------------
// 黄金渲染
// ---------------------------------------------------------------------------

#[test]
fn golden_global_component_matches_ts_reference() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let mut component = make_component(
        &paths,
        probe_settings_manager(),
        ConfigWriteScope::Global,
        true,
    );
    assert_lines(component.render(80), GOLDEN_GLOBAL_COMPONENT_80);
}

#[test]
fn golden_tall_global_component_shows_every_group() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let mut component = ConfigSelectorComponent::new(
        &paths,
        probe_settings_manager(),
        CWD,
        AGENT,
        noop(),
        noop(),
        noop(),
        ConfigSelectorOptions {
            terminal_height: Some(40),
            write_scope: ConfigWriteScope::Global,
            project_mode_available: true,
        },
    );
    assert_lines(component.render(100), GOLDEN_GLOBAL_TALL_COMPONENT_100);
}

#[test]
fn golden_project_component_matches_ts_reference() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let mut component = make_component(
        &paths,
        probe_settings_manager(),
        ConfigWriteScope::Project,
        true,
    );
    assert_lines(component.render(80), GOLDEN_PROJECT_COMPONENT_80);
}

#[test]
fn golden_narrow_component_matches_ts_reference() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let mut component = make_component(
        &paths,
        probe_settings_manager(),
        ConfigWriteScope::Global,
        true,
    );
    assert_lines(component.render(30), GOLDEN_GLOBAL_NARROW_30_COMPONENT);
    let mut list = component.get_resource_list();
    assert_lines(list.render(30), GOLDEN_GLOBAL_NARROW_30_LIST);
}

#[test]
fn golden_filtered_global_list_matches_ts_reference() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let component = make_component(
        &paths,
        probe_settings_manager(),
        ConfigWriteScope::Global,
        true,
    );
    let mut list = component.get_resource_list();
    list.handle_input("b");
    assert_lines(list.render(80), GOLDEN_GLOBAL_FILTER_B_80);
}

#[test]
fn golden_page_up_and_down_match_ts_reference() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let component = make_component(
        &paths,
        probe_settings_manager(),
        ConfigWriteScope::Project,
        true,
    );
    let mut list = component.get_resource_list();
    // pageUp 向上找**下一个** item（源码方向与直觉相反）。
    list.handle_input("\x1b[5~");
    assert_lines(list.render(80), GOLDEN_PROJECT_PAGEUP_80);
    // pageDown 向下找**前一个** item。
    list.handle_input("\x1b[6~");
    assert_lines(list.render(80), GOLDEN_PROJECT_PAGEDOWN_80);
}

// ---------------------------------------------------------------------------
// package-command-paths.test.ts 的 ConfigSelector 用例
// ---------------------------------------------------------------------------

#[test]
fn tab_on_resource_list_switches_scope_synchronously() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let renders = Rc::new(RefCell::new(0usize));
    let renders_sink = Rc::clone(&renders);
    let mut component = ConfigSelectorComponent::new(
        &paths,
        probe_settings_manager(),
        CWD,
        AGENT,
        noop(),
        noop(),
        Rc::new(move || *renders_sink.borrow_mut() += 1),
        ConfigSelectorOptions {
            terminal_height: Some(24),
            write_scope: ConfigWriteScope::Global,
            project_mode_available: true,
        },
    );

    // TS 的 CLI 宿主把焦点直接设到 `selector.getResourceList()`；Rust 里对应
    // `get_resource_list()` 守卫内的 handle_input。Tab 必须在此**同步**完成
    // scope 切换、表头更新、列表重建与一次 requestRender。
    {
        let mut list = component.get_resource_list();
        list.handle_input("\t");
        assert_eq!(list.write_scope(), ConfigWriteScope::Project);
    }
    assert_eq!(*renders.borrow(), 1);
    assert_eq!(component.write_scope(), ConfigWriteScope::Project);

    let lines = component.render(80);
    assert!(
        lines[3].contains("Project Local Resources"),
        "header should switch: {:?}",
        lines[3]
    );

    // 不能把切换泄漏到下一次组件输入（旧实现把标志留到下一次 handle_input）。
    component.handle_input("x");
    assert_eq!(component.write_scope(), ConfigWriteScope::Project);
    assert_eq!(*renders.borrow(), 1);

    // 再按一次 Tab 回到 global。
    {
        let mut list = component.get_resource_list();
        list.handle_input("\t");
        assert_eq!(list.write_scope(), ConfigWriteScope::Global);
    }
    assert_eq!(*renders.borrow(), 2);
    assert!(component.render(80)[3].contains("Global Resources"));
}

#[test]
fn tab_is_ignored_when_project_mode_is_unavailable() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let renders = Rc::new(RefCell::new(0usize));
    let renders_sink = Rc::clone(&renders);
    let component = ConfigSelectorComponent::new(
        &paths,
        probe_settings_manager(),
        CWD,
        AGENT,
        noop(),
        noop(),
        Rc::new(move || *renders_sink.borrow_mut() += 1),
        ConfigSelectorOptions {
            terminal_height: Some(24),
            write_scope: ConfigWriteScope::Global,
            project_mode_available: false,
        },
    );
    let mut list = component.get_resource_list();
    list.handle_input("\t");
    assert_eq!(list.write_scope(), ConfigWriteScope::Global);
    drop(list);
    assert_eq!(*renders.borrow(), 0);
}

#[test]
fn cycles_project_package_overrides_in_config_local_mode() {
    setup();
    let settings_manager = settings_manager(
        Some(vec![PackageSource::Text("npm:kiana-tools".to_string())]),
        None,
    );
    let resolved = extension_paths(
        "/work/project/pkg",
        "npm:kiana-tools",
        SourceScope::User,
        &["bar.ts"],
    );
    let paths = scoped(resolved.clone(), resolved);
    let component = make_component(
        &paths,
        settings_manager.clone(),
        ConfigWriteScope::Project,
        true,
    );
    let mut list = component.get_resource_list();

    list.handle_input(" ");
    assert_eq!(
        json_packages(&settings_manager),
        serde_json::json!([{
            "source": "npm:kiana-tools",
            "autoload": false,
            "extensions": ["-extensions/bar.ts"],
        }])
    );

    list.handle_input(" ");
    assert_eq!(
        json_packages(&settings_manager),
        serde_json::json!([{
            "source": "npm:kiana-tools",
            "autoload": false,
            "extensions": ["+extensions/bar.ts"],
        }])
    );

    list.handle_input(" ");
    assert_eq!(json_packages(&settings_manager), serde_json::json!([]));
}

#[test]
fn toggles_builtin_extensions_in_config_global_mode() {
    setup();
    let settings_manager = settings_manager(None, None);
    let resolved = ResolvedPaths {
        extensions: vec![
            res(
                "builtin:llama.cpp",
                true,
                "builtin",
                SourceScope::User,
                SourceOrigin::TopLevel,
                None,
            ),
            res(
                "builtin:mcp",
                true,
                "builtin",
                SourceScope::User,
                SourceOrigin::TopLevel,
                None,
            ),
        ],
        ..ResolvedPaths::default()
    };
    let paths = scoped(resolved.clone(), resolved);
    let component = make_component(
        &paths,
        settings_manager.clone(),
        ConfigWriteScope::Global,
        true,
    );
    let mut list = component.get_resource_list();

    let rendered = kiana_tui::strip_terminal_sequences(&list.render(80).join("\n"));
    assert!(rendered.contains("Built-in"));
    assert!(rendered.contains("llama.cpp"));

    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_global_settings().extensions),
        serde_json::json!(["-builtin:llama.cpp"])
    );
    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_global_settings().extensions),
        serde_json::json!(["+builtin:llama.cpp"])
    );
}

#[test]
fn cycles_project_builtin_extension_overrides_in_config_local_mode() {
    setup();
    // TS `package-command-paths.test.ts`「cycles project built-in extension overrides in
    // config local mode」：global 设置里 `-builtin:mcp` 让 DefaultPackageManager 把该
    // built-in 解析成 user scope、enabled=false 的继承资源（测试里 global/project 共用
    // 同一份 ResolvedPaths）。项目侧从 inherit 出发的三态循环是 +、-、[]。
    let settings_manager = settings_manager(None, Some(vec!["-builtin:mcp".to_string()]));
    let resolved = ResolvedPaths {
        extensions: vec![res(
            "builtin:mcp",
            false,
            "builtin",
            SourceScope::User,
            SourceOrigin::TopLevel,
            None,
        )],
        ..ResolvedPaths::default()
    };
    let paths = scoped(resolved.clone(), resolved);
    let component = make_component(
        &paths,
        settings_manager.clone(),
        ConfigWriteScope::Project,
        true,
    );
    let mut list = component.get_resource_list();

    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_project_settings().extensions),
        serde_json::json!(["+builtin:mcp"])
    );
    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_project_settings().extensions),
        serde_json::json!(["-builtin:mcp"])
    );
    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_project_settings().extensions),
        serde_json::json!([])
    );
}

#[test]
fn cycles_project_top_level_inherited_overrides() {
    setup();
    let a_path = format!("{AGENT}/extensions/a.ts");
    let p_path = format!("{CWD}/.kiana/extensions/p.ts");
    let settings_manager = settings_manager(None, Some(vec![a_path.clone()]));
    let resolved = ResolvedPaths {
        extensions: vec![
            res(
                &a_path,
                true,
                "auto",
                SourceScope::User,
                SourceOrigin::TopLevel,
                Some(&format!("{AGENT}/extensions")),
            ),
            res(
                &p_path,
                true,
                "auto",
                SourceScope::Project,
                SourceOrigin::TopLevel,
                Some(&format!("{CWD}/.kiana/extensions")),
            ),
        ],
        ..ResolvedPaths::default()
    };
    let paths = scoped(resolved.clone(), resolved);
    let component = make_component(
        &paths,
        settings_manager.clone(),
        ConfigWriteScope::Project,
        true,
    );
    let mut list = component.get_resource_list();

    // 继承的全局项：unload → load → inherit（inherit 会清掉所有覆盖条目）。
    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_project_settings().extensions),
        serde_json::json!([a_path.clone(), format!("-{a_path}")])
    );
    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_project_settings().extensions),
        serde_json::json!([a_path.clone(), format!("+{a_path}")])
    );
    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_project_settings().extensions),
        serde_json::json!([])
    );

    // 项目级资源走同样的三态循环，但条目点名绝对路径。
    list.handle_input("\x1b[B");
    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_project_settings().extensions),
        serde_json::json!([p_path.clone(), format!("-{p_path}")])
    );
    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_project_settings().extensions),
        serde_json::json!([p_path.clone(), format!("+{p_path}")])
    );
    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_project_settings().extensions),
        serde_json::json!([])
    );
}

#[test]
fn toggles_global_top_level_paths_with_patterns() {
    setup();
    let settings_manager = settings_manager(None, None);
    let resolved = ResolvedPaths {
        extensions: vec![
            res(
                &format!("{AGENT}/extensions/a.ts"),
                true,
                "auto",
                SourceScope::User,
                SourceOrigin::TopLevel,
                Some(&format!("{AGENT}/extensions")),
            ),
            res(
                "/other/abs.ts",
                false,
                "local",
                SourceScope::User,
                SourceOrigin::TopLevel,
                None,
            ),
        ],
        ..ResolvedPaths::default()
    };
    let paths = scoped(resolved.clone(), resolved);
    let component = make_component(
        &paths,
        settings_manager.clone(),
        ConfigWriteScope::Global,
        true,
    );
    let mut list = component.get_resource_list();

    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_global_settings().extensions),
        serde_json::json!(["-a.ts"])
    );
    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_global_settings().extensions),
        serde_json::json!(["+a.ts"])
    );
    list.handle_input("\x1b[B");
    list.handle_input(" ");
    assert_eq!(
        json_extensions(settings_manager.get_global_settings().extensions),
        serde_json::json!(["+a.ts", "+../../other/abs.ts"])
    );
}

#[test]
fn creates_local_package_override_source_for_project_resources() {
    setup();
    let settings_manager = settings_manager(
        Some(vec![PackageSource::Text("../local-pkg".to_string())]),
        None,
    );
    let resolved = ResolvedPaths {
        extensions: vec![res(
            "/opt/local-pkg/extensions/a.ts",
            true,
            "../local-pkg",
            SourceScope::User,
            SourceOrigin::Package,
            Some("/opt/local-pkg"),
        )],
        ..ResolvedPaths::default()
    };
    let paths = scoped(resolved.clone(), resolved);
    let component = make_component(
        &paths,
        settings_manager.clone(),
        ConfigWriteScope::Project,
        true,
    );
    let mut list = component.get_resource_list();

    list.handle_input(" ");
    assert_eq!(
        json_packages(&settings_manager),
        serde_json::json!([{
            "source": "../../../opt/local-pkg",
            "autoload": false,
            "extensions": ["-extensions/a.ts"],
        }])
    );
    list.handle_input(" ");
    assert_eq!(
        json_packages(&settings_manager),
        serde_json::json!([{
            "source": "../../../opt/local-pkg",
            "autoload": false,
            "extensions": ["+extensions/a.ts"],
        }])
    );
    list.handle_input(" ");
    assert_eq!(json_packages(&settings_manager), serde_json::json!([]));
}

#[test]
fn toggles_global_package_resources_in_place() {
    setup();
    let settings_manager = settings_manager(
        Some(vec![PackageSource::Object(PackageSourceObject {
            source: Some("npm:kiana-tools".to_string()),
            themes: Some(vec!["-a.json".to_string()]),
            ..PackageSourceObject::default()
        })]),
        None,
    );
    let resolved = ResolvedPaths {
        extensions: vec![res(
            "/opt/pkg/extensions/a.ts",
            true,
            "npm:kiana-tools",
            SourceScope::User,
            SourceOrigin::Package,
            Some("/opt/pkg"),
        )],
        ..ResolvedPaths::default()
    };
    let paths = scoped(resolved.clone(), resolved);
    let component = make_component(
        &paths,
        settings_manager.clone(),
        ConfigWriteScope::Global,
        true,
    );
    let mut list = component.get_resource_list();

    list.handle_input(" ");
    assert_eq!(
        serde_json::to_value(settings_manager.get_global_settings().packages).unwrap(),
        serde_json::json!([{
            "source": "npm:kiana-tools",
            "themes": ["-a.json"],
            "extensions": ["-extensions/a.ts"],
        }])
    );
    list.handle_input(" ");
    assert_eq!(
        serde_json::to_value(settings_manager.get_global_settings().packages).unwrap(),
        serde_json::json!([{
            "source": "npm:kiana-tools",
            "themes": ["-a.json"],
            "extensions": ["+extensions/a.ts"],
        }])
    );
}

// ---------------------------------------------------------------------------
// 头部 / 搜索 / 焦点
// ---------------------------------------------------------------------------

#[test]
fn header_switches_between_scopes_with_tab() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let renders = Rc::new(RefCell::new(0usize));
    let request_render = {
        let renders = Rc::clone(&renders);
        Rc::new(move || *renders.borrow_mut() += 1)
    };
    let mut component = ConfigSelectorComponent::new(
        &paths,
        probe_settings_manager(),
        CWD,
        AGENT,
        noop(),
        noop(),
        request_render,
        ConfigSelectorOptions {
            terminal_height: Some(24),
            write_scope: ConfigWriteScope::Global,
            project_mode_available: true,
        },
    );

    assert_eq!(component.write_scope(), ConfigWriteScope::Global);
    component.handle_input("\t");
    assert_eq!(component.write_scope(), ConfigWriteScope::Project);
    assert_eq!(*renders.borrow(), 1);
    let rendered = kiana_tui::strip_terminal_sequences(&component.render(80).join("\n"));
    assert!(rendered.contains("Project Local Resources"));
    assert!(rendered.contains(".kiana/settings.json · inherited global resources are dimmed"));

    component.handle_input("\t");
    assert_eq!(component.write_scope(), ConfigWriteScope::Global);
    assert_eq!(*renders.borrow(), 2);
    let rendered = kiana_tui::strip_terminal_sequences(&component.render(80).join("\n"));
    assert!(rendered.contains("Global Resources"));
    assert!(rendered.contains("~/.kiana/agent/settings.json"));
}

#[test]
fn header_hides_switch_hint_when_project_mode_is_unavailable() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let mut component = make_component(
        &paths,
        probe_settings_manager(),
        ConfigWriteScope::Global,
        false,
    );
    let rendered = kiana_tui::strip_terminal_sequences(&component.render(80).join("\n"));
    assert!(!rendered.contains("switch mode"));
    assert!(rendered.contains("space toggle"));
    assert!(rendered.contains("esc close"));

    component.handle_input("\t");
    assert_eq!(component.write_scope(), ConfigWriteScope::Global);
}

#[test]
fn empty_resolved_paths_render_no_resources() {
    setup();
    let paths = scoped(ResolvedPaths::default(), ResolvedPaths::default());
    let component = make_component(
        &paths,
        settings_manager(None, None),
        ConfigWriteScope::Global,
        true,
    );
    let mut list = component.get_resource_list();
    let rendered = kiana_tui::strip_terminal_sequences(&list.render(80).join("\n"));
    assert!(rendered.contains("  No resources found"));
}

#[test]
fn search_filter_and_empty_state() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let component = make_component(
        &paths,
        probe_settings_manager(),
        ConfigWriteScope::Global,
        true,
    );
    let mut list = component.get_resource_list();
    for character in "zzz".chars() {
        list.handle_input(&character.to_string());
    }
    assert_eq!(list.search_value(), "zzz");
    let rendered = kiana_tui::strip_terminal_sequences(&list.render(80).join("\n"));
    assert!(rendered.contains("  No resources found"));
    for _ in 0..3 {
        list.handle_input("\x7f");
    }
    assert_eq!(list.search_value(), "");
    let rendered = kiana_tui::strip_terminal_sequences(&list.render(80).join("\n"));
    assert!(rendered.contains("npm:kiana-tools (user)"));
    assert!(rendered.contains("bar.ts"));
}

#[test]
fn switching_scope_keeps_the_search_query() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let mut component = make_component(
        &paths,
        probe_settings_manager(),
        ConfigWriteScope::Global,
        true,
    );
    {
        let mut list = component.get_resource_list();
        list.handle_input("b");
        assert_eq!(list.search_value(), "b");
    }
    component.handle_input("\t");
    let mut list = component.get_resource_list();
    assert_eq!(list.write_scope(), ConfigWriteScope::Project);
    assert_eq!(list.search_value(), "b");
    let rendered = kiana_tui::strip_terminal_sequences(&list.render(80).join("\n"));
    // project 侧命中：bar.ts（继承）与 builtin mcp 的路径都不含 "b"？proj.ts 不含；
    // bar.ts 的 displayName 含 "b"。
    assert!(rendered.contains("bar.ts"));
    assert!(!rendered.contains("proj.ts"));
}

#[test]
fn focus_is_proxied_to_the_resource_list() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let mut component = make_component(
        &paths,
        probe_settings_manager(),
        ConfigWriteScope::Global,
        true,
    );
    assert!(!component.focused());
    component.set_focused(true);
    assert!(component.focused());
    let list = component.get_resource_list();
    assert!(list.focused());
}

#[test]
fn toggling_requests_a_render() {
    setup();
    let paths = scoped(probe_global_paths(), probe_project_paths());
    let renders = Rc::new(RefCell::new(0usize));
    let request_render = {
        let renders = Rc::clone(&renders);
        Rc::new(move || *renders.borrow_mut() += 1)
    };
    let component = ConfigSelectorComponent::new(
        &paths,
        settings_manager(None, None),
        CWD,
        AGENT,
        noop(),
        noop(),
        request_render,
        ConfigSelectorOptions {
            terminal_height: Some(24),
            write_scope: ConfigWriteScope::Global,
            project_mode_available: true,
        },
    );
    let mut list = component.get_resource_list();
    list.handle_input(" ");
    assert_eq!(*renders.borrow(), 1);
}
const GOLDEN_GLOBAL_COMPONENT_80: &[&str] = &[
    "",
    "\x1b[38;2;95;168;204m────────────────────────────────────────────────────────────────────────────────\x1b[39m",
    "",
    "\x1b[1mGlobal Resources\x1b[22m                      \x1b[38;2;126;136;142mtab\x1b[39m\x1b[38;2;157;165;169m switch mode\x1b[39m\x1b[38;2;157;165;169m · \x1b[39m\x1b[38;2;126;136;142mspace\x1b[39m\x1b[38;2;157;165;169m toggle\x1b[39m\x1b[38;2;157;165;169m · \x1b[39m\x1b[38;2;126;136;142mesc\x1b[39m\x1b[38;2;157;165;169m close\x1b[39m",
    "\x1b[38;2;157;165;169m~/.kiana/agent/settings.json\x1b[39m",
    "",
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                                                             ",
    "",
    "  \x1b[38;2;167;152;215m\x1b[1mnpm:kiana-tools (user)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    ">     \x1b[38;2;104;183;141m[x]\x1b[39m \x1b[1mbar.ts\x1b[22m",
    "      \x1b[38;2;126;136;142m[ ]\x1b[39m foo.ts",
    "    \x1b[38;2;157;165;169mSkills\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m alpha",
    "    \x1b[38;2;157;165;169mPrompts\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m review.md",
    "    \x1b[38;2;157;165;169mThemes\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m solar.json",
    "  \x1b[38;2;167;152;215m\x1b[1mUser (/opt/kiana-agent/extensions/)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m ext1.ts",
    "  \x1b[38;2;167;152;215m\x1b[1mUser (/opt/kiana-agent/skills/)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mSkills\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m beta.md",
    "\x1b[38;2;126;136;142m  (1/10)\x1b[39m",
    "",
    "\x1b[38;2;95;168;204m────────────────────────────────────────────────────────────────────────────────\x1b[39m",
];

const GOLDEN_PROJECT_COMPONENT_80: &[&str] = &[
    "",
    "\x1b[38;2;95;168;204m────────────────────────────────────────────────────────────────────────────────\x1b[39m",
    "",
    "\x1b[1mProject Local Resources\x1b[22m    \x1b[38;2;126;136;142mtab\x1b[39m\x1b[38;2;157;165;169m switch mode\x1b[39m\x1b[38;2;157;165;169m · \x1b[39m\x1b[38;2;126;136;142mspace\x1b[39m\x1b[38;2;157;165;169m cycle inherit/+/-\x1b[39m\x1b[38;2;157;165;169m · \x1b[39m\x1b[38;2;126;136;142mesc\x1b[39m\x1b[38;2;157;165;169m close\x1b[39m",
    "\x1b[38;2;157;165;169m.kiana/settings.json · inherited global resources are dimmed\x1b[39m",
    "",
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                                                             ",
    "",
    "  \x1b[38;2;126;136;142m\x1b[1mnpm:kiana-tools (user) · inherited global\x1b[22m\x1b[39m",
    "    \x1b[38;2;126;136;142mExtensions\x1b[39m",
    ">     \x1b[38;2;126;136;142m[x]\x1b[39m \x1b[38;2;126;136;142mbar.ts\x1b[39m\x1b[38;2;126;136;142m  inherited global\x1b[39m",
    "  \x1b[38;2;167;152;215m\x1b[1mProject (/work/project/.kiana/extensions/)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    "      \x1b[38;2;126;136;142m[x]\x1b[39m proj.ts",
    "  \x1b[38;2;167;152;215m\x1b[1mBuilt-in (project override)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    "      \x1b[38;2;126;136;142m[x]\x1b[39m mcp",
    "",
    "\x1b[38;2;95;168;204m────────────────────────────────────────────────────────────────────────────────\x1b[39m",
];

const GOLDEN_GLOBAL_FILTER_B_80: &[&str] = &[
    "> b\x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                                                            ",
    "",
    "  \x1b[38;2;167;152;215m\x1b[1mnpm:kiana-tools (user)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    ">     \x1b[38;2;104;183;141m[x]\x1b[39m \x1b[1mbar.ts\x1b[22m",
    "  \x1b[38;2;167;152;215m\x1b[1mUser (/opt/kiana-agent/skills/)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mSkills\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m beta.md",
    "  \x1b[38;2;167;152;215m\x1b[1mBuilt-in\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m read",
];

const GOLDEN_PROJECT_PAGEUP_80: &[&str] = &[
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                                                             ",
    "",
    "  \x1b[38;2;126;136;142m\x1b[1mnpm:kiana-tools (user) · inherited global\x1b[22m\x1b[39m",
    "    \x1b[38;2;126;136;142mExtensions\x1b[39m",
    ">     \x1b[38;2;126;136;142m[x]\x1b[39m \x1b[38;2;126;136;142mbar.ts\x1b[39m\x1b[38;2;126;136;142m  inherited global\x1b[39m",
    "  \x1b[38;2;167;152;215m\x1b[1mProject (/work/project/.kiana/extensions/)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    "      \x1b[38;2;126;136;142m[x]\x1b[39m proj.ts",
    "  \x1b[38;2;167;152;215m\x1b[1mBuilt-in (project override)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    "      \x1b[38;2;126;136;142m[x]\x1b[39m mcp",
];

const GOLDEN_PROJECT_PAGEDOWN_80: &[&str] = &[
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                                                             ",
    "",
    "  \x1b[38;2;126;136;142m\x1b[1mnpm:kiana-tools (user) · inherited global\x1b[22m\x1b[39m",
    "    \x1b[38;2;126;136;142mExtensions\x1b[39m",
    "      \x1b[38;2;126;136;142m[x]\x1b[39m \x1b[38;2;126;136;142mbar.ts\x1b[39m\x1b[38;2;126;136;142m  inherited global\x1b[39m",
    "  \x1b[38;2;167;152;215m\x1b[1mProject (/work/project/.kiana/extensions/)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    "      \x1b[38;2;126;136;142m[x]\x1b[39m proj.ts",
    "  \x1b[38;2;167;152;215m\x1b[1mBuilt-in (project override)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    ">     \x1b[38;2;126;136;142m[x]\x1b[39m \x1b[1mmcp\x1b[22m",
];
const GOLDEN_GLOBAL_TALL_COMPONENT_100: &[&str] = &[
    "",
    "\x1b[38;2;95;168;204m────────────────────────────────────────────────────────────────────────────────────────────────────\x1b[39m",
    "",
    "\x1b[1mGlobal Resources\x1b[22m                                          \x1b[38;2;126;136;142mtab\x1b[39m\x1b[38;2;157;165;169m switch mode\x1b[39m\x1b[38;2;157;165;169m · \x1b[39m\x1b[38;2;126;136;142mspace\x1b[39m\x1b[38;2;157;165;169m toggle\x1b[39m\x1b[38;2;157;165;169m · \x1b[39m\x1b[38;2;126;136;142mesc\x1b[39m\x1b[38;2;157;165;169m close\x1b[39m",
    "\x1b[38;2;157;165;169m~/.kiana/agent/settings.json\x1b[39m",
    "",
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                                                                                 ",
    "",
    "  \x1b[38;2;167;152;215m\x1b[1mnpm:kiana-tools (user)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    ">     \x1b[38;2;104;183;141m[x]\x1b[39m \x1b[1mbar.ts\x1b[22m",
    "      \x1b[38;2;126;136;142m[ ]\x1b[39m foo.ts",
    "    \x1b[38;2;157;165;169mSkills\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m alpha",
    "    \x1b[38;2;157;165;169mPrompts\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m review.md",
    "    \x1b[38;2;157;165;169mThemes\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m solar.json",
    "  \x1b[38;2;167;152;215m\x1b[1mUser (/opt/kiana-agent/extensions/)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m ext1.ts",
    "  \x1b[38;2;167;152;215m\x1b[1mUser (/opt/kiana-agent/skills/)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mSkills\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m beta.md",
    "  \x1b[38;2;167;152;215m\x1b[1mUser (/opt/kiana-agent/prompts/)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mPrompts\x1b[39m",
    "      \x1b[38;2;126;136;142m[ ]\x1b[39m prompt1.md",
    "  \x1b[38;2;167;152;215m\x1b[1mBuilt-in\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m read",
    "  \x1b[38;2;167;152;215m\x1b[1mUser settings\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m local.ts",
    "",
    "\x1b[38;2;95;168;204m────────────────────────────────────────────────────────────────────────────────────────────────────\x1b[39m",
];
const GOLDEN_GLOBAL_NARROW_30_COMPONENT: &[&str] = &[
    "",
    "\x1b[38;2;95;168;204m──────────────────────────────\x1b[39m",
    "",
    "\x1b[1mGlobal Resources\x1b[22m \x1b[38;2;126;136;142mtab\x1b[39m\x1b[38;2;157;165;169m switch mo\x1b[0m",
    "\x1b[38;2;157;165;169m~/.kiana/agent/settings.json\x1b[39m",
    "",
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                           ",
    "",
    "  \x1b[38;2;167;152;215m\x1b[1mnpm:kiana-tools (user)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    ">     \x1b[38;2;104;183;141m[x]\x1b[39m \x1b[1mbar.ts\x1b[22m",
    "      \x1b[38;2;126;136;142m[ ]\x1b[39m foo.ts",
    "    \x1b[38;2;157;165;169mSkills\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m alpha",
    "    \x1b[38;2;157;165;169mPrompts\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m review.md",
    "    \x1b[38;2;157;165;169mThemes\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m solar.json",
    "  \x1b[38;2;167;152;215m\x1b[1mUser (/opt/kiana-agent/exten\x1b[0m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m ext1.ts",
    "  \x1b[38;2;167;152;215m\x1b[1mUser (/opt/kiana-agent/skill\x1b[0m",
    "    \x1b[38;2;157;165;169mSkills\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m beta.md",
    "\x1b[38;2;126;136;142m  (1/10)\x1b[39m",
    "",
    "\x1b[38;2;95;168;204m──────────────────────────────\x1b[39m",
];

const GOLDEN_GLOBAL_NARROW_30_LIST: &[&str] = &[
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                           ",
    "",
    "  \x1b[38;2;167;152;215m\x1b[1mnpm:kiana-tools (user)\x1b[22m\x1b[39m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    ">     \x1b[38;2;104;183;141m[x]\x1b[39m \x1b[1mbar.ts\x1b[22m",
    "      \x1b[38;2;126;136;142m[ ]\x1b[39m foo.ts",
    "    \x1b[38;2;157;165;169mSkills\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m alpha",
    "    \x1b[38;2;157;165;169mPrompts\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m review.md",
    "    \x1b[38;2;157;165;169mThemes\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m solar.json",
    "  \x1b[38;2;167;152;215m\x1b[1mUser (/opt/kiana-agent/exten\x1b[0m",
    "    \x1b[38;2;157;165;169mExtensions\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m ext1.ts",
    "  \x1b[38;2;167;152;215m\x1b[1mUser (/opt/kiana-agent/skill\x1b[0m",
    "    \x1b[38;2;157;165;169mSkills\x1b[39m",
    "      \x1b[38;2;104;183;141m[x]\x1b[39m beta.md",
    "\x1b[38;2;126;136;142m  (1/10)\x1b[39m",
];
