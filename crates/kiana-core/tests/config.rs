//! `config.ts` 的移植测试（`test/config.test.ts`）。
//!
//! 环境变量与进程级测试钩子是进程全局状态：本文件用 [`ENV_LOCK`] 串行化所有用例，
//! 并在 [`EnvScope`] 的 `Drop` 里恢复。

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use kiana_core::config::{
    APP_NAME, APP_TITLE, CONFIG_DIR_NAME, ENV_AGENT_DIR, ENV_SESSION_DIR, InstallChange,
    InstallMethod, PACKAGE_NAME, SelfUpdatePackageTarget, VERSION,
    clear_entrypoint_override_for_test, clear_exec_path_override_for_test, detect_install_change,
    detect_install_method, detect_install_method_from, find_node_package_dir,
    get_self_update_command, get_self_update_unavailable_instruction, get_update_instruction,
    set_entrypoint_override_for_test, set_exec_path_override_for_test,
};
use tempfile::{Builder, TempDir};

static ENV_LOCK: Mutex<()> = Mutex::new(());

const PACKAGE: &str = "@earendil-works/kiana-coding-agent";

/// 串行化 env 修改并恢复原始值。
struct EnvScope {
    _guard: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvScope {
    fn new() -> Self {
        let guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = [
            "KIANA_PACKAGE_DIR",
            "KIANA_API_BASE_URL",
            "KIANA_SHARE_VIEWER_URL",
            "PATH",
            "PNPM_HOME",
        ]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect();
        clear_exec_path_override_for_test();
        clear_entrypoint_override_for_test();
        Self {
            _guard: guard,
            saved,
        }
    }

    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) {
        // SAFETY: 所有会写 env 的测试都持有 `ENV_LOCK`，不会有并发数据竞争。
        unsafe { std::env::set_var(key, value) };
    }

    fn remove(key: &'static str) {
        // SAFETY: 同上。
        unsafe { std::env::remove_var(key) };
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(value) => Self::set(key, value),
                None => Self::remove(key),
            }
        }
        clear_exec_path_override_for_test();
        clear_entrypoint_override_for_test();
    }
}

fn set_exec_path(value: &Path) {
    set_exec_path_override_for_test(Some(value.to_path_buf()));
}

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn set_permissions(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

fn make_executable(path: &Path) {
    set_permissions(path, 0o755);
}

/// `mkdtempSync(join(tmpdir(), template))`。
fn make_temp(template: &str) -> TempDir {
    Builder::new().prefix(template).tempdir().expect("temp dir")
}

/// 只含一个空目录的临时目录，用作不含任何 package manager 的 PATH。
fn empty_path_dir() -> TempDir {
    make_temp("kiana-empty-path-")
}

// =============================================================================
// fixtures（照搬 config.test.ts 的 create*Install）
// =============================================================================

struct NpmPrefixInstall {
    _temp: TempDir,
    prefix: PathBuf,
    #[allow(dead_code)]
    package_dir: PathBuf,
}

fn create_npm_prefix_install(template: &str) -> NpmPrefixInstall {
    let temp = make_temp(template);
    let prefix = temp.path().to_path_buf();
    let root = prefix.join("lib").join("node_modules");
    let package_dir = root.join("@earendil-works").join("kiana-coding-agent");
    fs::create_dir_all(&package_dir).unwrap();
    EnvScope::set("KIANA_PACKAGE_DIR", &package_dir);
    set_exec_path(&package_dir.join("dist").join("cli.js"));
    NpmPrefixInstall {
        _temp: temp,
        prefix,
        package_dir,
    }
}

fn fake_pnpm_script(root: &Path) -> String {
    let escaped = s(root).replace('\'', "'\\''");
    format!(
        "#!/bin/sh\nif [ \"$1\" = \"root\" ] && [ \"$2\" = \"-g\" ]; then\n\tprintf '%s\\n' '{escaped}'\n\texit 0\nfi\nexit 1\n"
    )
}

fn fake_yarn_script(global_dir: &Path) -> String {
    let escaped = s(global_dir).replace('\'', "'\\''");
    format!(
        "#!/bin/sh\nif [ \"$1\" = \"global\" ] && [ \"$2\" = \"dir\" ]; then\n\tprintf '%s\\n' '{escaped}'\n\texit 0\nfi\nexit 1\n"
    )
}

fn fake_bun_script(bun_bin: &Path) -> String {
    let escaped = s(bun_bin).replace('\'', "'\\''");
    format!(
        "#!/bin/sh\nif [ \"$1\" = \"pm\" ] && [ \"$2\" = \"bin\" ] && [ \"$3\" = \"-g\" ]; then\n\tprintf '%s\\n' '{escaped}'\n\texit 0\nfi\nexit 1\n"
    )
}

/// 假 npm：`npm --prefix <p> root -g` 打印 `<p>/lib/node_modules`（与真实 npm 一致）。
fn fake_npm_script() -> String {
    "#!/bin/sh\nif [ \"$1\" = \"--prefix\" ] && [ \"$3\" = \"root\" ] && [ \"$4\" = \"-g\" ]; then\n\tprintf '%s\\n' \"$2/lib/node_modules\"\n\texit 0\nfi\nexit 1\n".to_string()
}

fn install_fake_bin(bin_dir: &Path, name: &str, script: &str) {
    fs::create_dir_all(bin_dir).unwrap();
    let path = bin_dir.join(name);
    fs::write(&path, script).unwrap();
    make_executable(&path);
}

fn prepend_path(bin_dir: &Path) {
    let original = std::env::var_os("PATH").unwrap_or_default();
    let mut combined = OsString::from(bin_dir);
    combined.push(":");
    combined.push(original);
    EnvScope::set("PATH", combined);
}

struct PnpmGlobalInstall {
    _temp: TempDir,
    #[allow(dead_code)]
    root: PathBuf,
    #[allow(dead_code)]
    package_dir: PathBuf,
}

fn create_pnpm_global_install() -> PnpmGlobalInstall {
    let temp = make_temp("kiana-pnpm-");
    let bin_dir = temp.path().join("bin");
    let root = temp
        .path()
        .join("pnpm")
        .join("global")
        .join("5")
        .join("node_modules");
    let package_dir = root.join("@mariozechner").join("kiana-coding-agent");
    fs::create_dir_all(&package_dir).unwrap();
    install_fake_bin(&bin_dir, "pnpm", &fake_pnpm_script(&root));
    prepend_path(&bin_dir);
    EnvScope::set("KIANA_PACKAGE_DIR", &package_dir);
    set_exec_path(
        &root
            .join(".pnpm")
            .join("@mariozechner+kiana-coding-agent@0.0.0")
            .join("node_modules")
            .join("@mariozechner")
            .join("kiana-coding-agent")
            .join("dist")
            .join("cli.js"),
    );
    PnpmGlobalInstall {
        _temp: temp,
        root,
        package_dir,
    }
}

struct YarnGlobalInstall {
    _temp: TempDir,
    #[allow(dead_code)]
    global_dir: PathBuf,
    #[allow(dead_code)]
    package_dir: PathBuf,
}

fn create_yarn_global_install() -> YarnGlobalInstall {
    let temp = make_temp("kiana-yarn-");
    let bin_dir = temp.path().join("bin");
    let global_dir = temp.path().join("yarn").join("global");
    let package_dir = global_dir
        .join("node_modules")
        .join("@mariozechner")
        .join("kiana-coding-agent");
    fs::create_dir_all(&package_dir).unwrap();
    install_fake_bin(&bin_dir, "yarn", &fake_yarn_script(&global_dir));
    prepend_path(&bin_dir);
    EnvScope::set("KIANA_PACKAGE_DIR", &package_dir);
    set_exec_path(
        &global_dir
            .join(".yarn")
            .join("@mariozechner")
            .join("kiana-coding-agent")
            .join("dist")
            .join("cli.js"),
    );
    YarnGlobalInstall {
        _temp: temp,
        global_dir,
        package_dir,
    }
}

struct BunGlobalInstall {
    _temp: TempDir,
    #[allow(dead_code)]
    package_dir: PathBuf,
}

fn create_bun_global_install() -> BunGlobalInstall {
    let temp = make_temp("kiana-bun-");
    let prefix = temp.path().join(".bun");
    let bun_bin = prefix.join("bin");
    let root = prefix.join("install").join("global").join("node_modules");
    let package_dir = root.join("@earendil-works").join("kiana-coding-agent");
    fs::create_dir_all(&package_dir).unwrap();
    install_fake_bin(&bun_bin, "bun", &fake_bun_script(&bun_bin));
    prepend_path(&bun_bin);
    EnvScope::set("KIANA_PACKAGE_DIR", &package_dir);
    set_exec_path(&package_dir.join("dist").join("cli.js"));
    BunGlobalInstall {
        _temp: temp,
        package_dir,
    }
}

fn package_target(name: &str, install_spec: Option<&str>) -> SelfUpdatePackageTarget {
    SelfUpdatePackageTarget::Target {
        package_name: name.to_string(),
        install_spec: install_spec.map(|value| value.to_string()),
    }
}

// =============================================================================
// constants
// =============================================================================

#[test]
fn constants_are_the_frozen_literals() {
    assert_eq!(PACKAGE_NAME, "@earendil-works/kiana-coding-agent");
    assert_eq!(APP_NAME, "kiana");
    assert_eq!(APP_TITLE, "Kiana");
    assert_eq!(CONFIG_DIR_NAME, ".kiana");
    assert_eq!(VERSION, "1.4.0");
    assert_eq!(ENV_AGENT_DIR, "KIANA_CODING_AGENT_DIR");
    assert_eq!(ENV_SESSION_DIR, "KIANA_CODING_AGENT_SESSION_DIR");
}

// =============================================================================
// findNodePackageDir
// =============================================================================

#[test]
fn find_node_package_dir_skips_binary_metadata_copied_into_dist() {
    let temp = make_temp("kiana-package-dir-");
    let dist_dir = temp.path().join("dist");
    let bundle_dir = dist_dir.join("bundle");
    fs::create_dir_all(&bundle_dir).unwrap();
    fs::write(temp.path().join("package.json"), "{}").unwrap();
    fs::write(dist_dir.join("package.json"), "{}").unwrap();

    assert_eq!(find_node_package_dir(&bundle_dir), temp.path());
}

#[test]
fn find_node_package_dir_returns_start_dir_when_missing() {
    let temp = make_temp("kiana-package-dir-");
    let start = temp.path().join("nested").join("dir");
    fs::create_dir_all(&start).unwrap();
    assert_eq!(find_node_package_dir(&start), start);
}

// =============================================================================
// detectInstallMethod
// =============================================================================

#[test]
fn detects_pnpm_from_windows_pnpm_install_paths() {
    let exec_path = "C:\\Users\\Admin\\Documents\\pnpm-repository\\global\\5\\.pnpm\\@earendil-works+kiana-coding-agent@0.67.68\\node_modules\\@earendil-works\\kiana-coding-agent\\dist\\cli.js";
    assert_eq!(
        detect_install_method_from(
            "C:\\Users\\Admin\\Documents\\pnpm-repository\\global\\5\\.pnpm\\@earendil-works+kiana-coding-agent@0.67.68\\node_modules\\@earendil-works\\kiana-coding-agent",
            Some(exec_path)
        ),
        InstallMethod::Pnpm
    );
}

#[test]
fn detects_install_method_from_paths() {
    let cases = [
        (
            "/usr/local/lib/node_modules/@earendil-works/kiana",
            Some("/usr/local/lib/node_modules/.pnpm/x"),
        ),
        (
            "/opt/homebrew/lib/node_modules/@earendil-works/kiana",
            Some("/opt/homebrew/lib/node_modules/.pnpm/x"),
        ),
    ];
    assert_eq!(
        detect_install_method_from(cases[0].0, cases[0].1),
        InstallMethod::Pnpm
    );
    assert_eq!(
        detect_install_method_from("/home/me/.yarn/global/node_modules/x", None),
        InstallMethod::Yarn
    );
    assert_eq!(
        detect_install_method_from("/home/me/.bun/install/global/node_modules/x", None),
        InstallMethod::Bun
    );
    assert_eq!(
        detect_install_method_from("/usr/local/lib/node_modules/x", None),
        InstallMethod::Npm
    );
    assert_eq!(
        detect_install_method_from("/usr/local/bin", Some("/usr/local/bin/node")),
        InstallMethod::Unknown
    );
}

#[test]
fn windows_pnpm_install_uses_the_run_prefix_instruction() {
    let _env = EnvScope::new();
    let empty = empty_path_dir();
    EnvScope::set("PATH", empty.path());
    let package_dir = make_temp("kiana-windows-pnpm-");
    EnvScope::set("KIANA_PACKAGE_DIR", package_dir.path());
    set_exec_path(Path::new(
        "C:\\Users\\Admin\\Documents\\pnpm-repository\\global\\5\\.pnpm\\@earendil-works+kiana-coding-agent@0.67.68\\node_modules\\@earendil-works\\kiana-coding-agent\\dist\\cli.js",
    ));

    assert_eq!(detect_install_method(), InstallMethod::Pnpm);
    assert_eq!(
        get_update_instruction(PACKAGE),
        "Run: pnpm install -g --ignore-scripts --config.minimumReleaseAge=0 @earendil-works/kiana-coding-agent"
    );
}

#[test]
fn does_not_self_update_unknown_wrapper_installs() {
    let _env = EnvScope::new();
    let empty = empty_path_dir();
    EnvScope::set("PATH", empty.path());
    let package_dir = make_temp("kiana-unknown-");
    EnvScope::set("KIANA_PACKAGE_DIR", package_dir.path());
    set_exec_path(Path::new("/usr/local/bin/node"));

    assert_eq!(detect_install_method(), InstallMethod::Unknown);
    assert_eq!(get_self_update_command(PACKAGE, None, None), None);
    assert_eq!(
        get_update_instruction(PACKAGE),
        "Update @earendil-works/kiana-coding-agent using the package manager, wrapper, or source checkout that provides this installation."
    );
}

#[test]
fn self_updates_npm_installs_from_custom_prefixes() {
    let _env = EnvScope::new();
    let empty = empty_path_dir();
    EnvScope::set("PATH", empty.path());
    let install = create_npm_prefix_install("kiana-prefix-");
    let prefix = s(&install.prefix);

    let command = get_self_update_command(PACKAGE, None, None).expect("command");

    assert_eq!(detect_install_method(), InstallMethod::Npm);
    assert_eq!(command.command, "npm");
    assert_eq!(
        command.args,
        vec![
            "--prefix".to_string(),
            prefix.clone(),
            "install".to_string(),
            "-g".to_string(),
            "--ignore-scripts".to_string(),
            "--min-release-age=0".to_string(),
            PACKAGE.to_string(),
        ]
    );
    assert_eq!(
        command.display,
        format!("npm --prefix {prefix} install -g --ignore-scripts --min-release-age=0 {PACKAGE}")
    );
    assert_eq!(command.steps, None);
}

#[test]
fn self_updates_exact_npm_versions_without_uninstalling() {
    let _env = EnvScope::new();
    let empty = empty_path_dir();
    EnvScope::set("PATH", empty.path());
    let install = create_npm_prefix_install("kiana-prefix-");
    let prefix = s(&install.prefix);

    let command = get_self_update_command(
        PACKAGE,
        None,
        Some(package_target(
            PACKAGE,
            Some("@earendil-works/kiana-coding-agent@1.2.3"),
        )),
    )
    .expect("command");

    assert_eq!(
        command.args,
        vec![
            "--prefix".to_string(),
            prefix.clone(),
            "install".to_string(),
            "-g".to_string(),
            "--ignore-scripts".to_string(),
            "--min-release-age=0".to_string(),
            "@earendil-works/kiana-coding-agent@1.2.3".to_string(),
        ]
    );
    assert_eq!(command.steps, None);
}

#[test]
fn self_updates_renamed_packages_from_the_current_install_prefix() {
    let _env = EnvScope::new();
    let empty = empty_path_dir();
    EnvScope::set("PATH", empty.path());
    let install = create_npm_prefix_install("kiana-prefix-");
    let prefix = s(&install.prefix);

    let command = get_self_update_command(
        "@mariozechner/kiana-coding-agent",
        None,
        Some(SelfUpdatePackageTarget::Name(
            "@new-scope/kiana".to_string(),
        )),
    )
    .expect("command");

    assert_eq!(
        command.args,
        vec![
            "--prefix".to_string(),
            prefix.clone(),
            "install".to_string(),
            "-g".to_string(),
            "--ignore-scripts".to_string(),
            "--min-release-age=0".to_string(),
            "@new-scope/kiana".to_string(),
        ]
    );
    assert_eq!(
        command.display,
        format!(
            "npm --prefix {prefix} uninstall -g @mariozechner/kiana-coding-agent && npm --prefix {prefix} install -g --ignore-scripts --min-release-age=0 @new-scope/kiana"
        )
    );
    let steps = command.steps.expect("steps");
    assert_eq!(
        steps[0].args,
        vec![
            "--prefix".to_string(),
            prefix.clone(),
            "uninstall".to_string(),
            "-g".to_string(),
            "@mariozechner/kiana-coding-agent".to_string(),
        ]
    );
    assert_eq!(
        steps[1].args,
        vec![
            "--prefix".to_string(),
            prefix,
            "install".to_string(),
            "-g".to_string(),
            "--ignore-scripts".to_string(),
            "--min-release-age=0".to_string(),
            "@new-scope/kiana".to_string(),
        ]
    );
}

#[test]
fn self_update_respects_configured_npm_command() {
    let _env = EnvScope::new();
    let bin_dir = make_temp("kiana-fake-npm-bin-");
    install_fake_bin(bin_dir.path(), "npm", &fake_npm_script());
    prepend_path(bin_dir.path());
    let install = create_npm_prefix_install("kiana-prefix-");
    let prefix = s(&install.prefix);

    let npm_command = vec!["npm".to_string(), "--prefix".to_string(), prefix.clone()];
    let command = get_self_update_command(PACKAGE, Some(&npm_command), None).expect("command");

    assert_eq!(
        command.args,
        vec![
            "--prefix".to_string(),
            prefix.clone(),
            "install".to_string(),
            "-g".to_string(),
            "--ignore-scripts".to_string(),
            "--min-release-age=0".to_string(),
            PACKAGE.to_string(),
        ]
    );
    assert_eq!(
        command.display,
        format!("npm --prefix {prefix} install -g --ignore-scripts --min-release-age=0 {PACKAGE}")
    );
}

#[test]
fn self_update_treats_empty_npm_command_as_unset() {
    let _env = EnvScope::new();
    let empty = empty_path_dir();
    EnvScope::set("PATH", empty.path());
    let install = create_npm_prefix_install("kiana-prefix-");
    let prefix = s(&install.prefix);

    let command = get_self_update_command(PACKAGE, Some(&[]), None).expect("command");
    assert_eq!(
        command.args,
        vec![
            "--prefix".to_string(),
            prefix,
            "install".to_string(),
            "-g".to_string(),
            "--ignore-scripts".to_string(),
            "--min-release-age=0".to_string(),
            PACKAGE.to_string(),
        ]
    );
}

#[test]
fn quotes_npm_self_update_display_paths() {
    let _env = EnvScope::new();
    let empty = empty_path_dir();
    EnvScope::set("PATH", empty.path());
    let install = create_npm_prefix_install("kiana prefix ");
    let prefix = s(&install.prefix);
    assert!(prefix.contains(' '));

    let command = get_self_update_command(PACKAGE, None, None).expect("command");
    assert_eq!(
        command.display,
        format!(
            "npm --prefix \"{prefix}\" install -g --ignore-scripts --min-release-age=0 {PACKAGE}"
        )
    );
}

#[test]
fn does_not_infer_windows_npm_custom_prefixes_from_package_paths() {
    let _env = EnvScope::new();
    let empty = empty_path_dir();
    EnvScope::set("PATH", empty.path());
    let package_dir =
        "C:\\Users\\Admin\\npm prefix\\node_modules\\@earendil-works\\kiana-coding-agent";
    EnvScope::set("KIANA_PACKAGE_DIR", package_dir);
    set_exec_path(Path::new(&format!("{package_dir}\\dist\\cli.js")));

    assert_eq!(detect_install_method(), InstallMethod::Npm);
    assert_eq!(
        get_update_instruction(PACKAGE),
        "Run: npm install -g --ignore-scripts --min-release-age=0 @earendil-works/kiana-coding-agent"
    );
}

#[test]
fn self_updates_bun_global_installs_from_bun_pm_bin() {
    let _env = EnvScope::new();
    let _install = create_bun_global_install();

    let command = get_self_update_command(PACKAGE, None, None).expect("command");

    assert_eq!(detect_install_method(), InstallMethod::Bun);
    assert_eq!(command.command, "bun");
    assert_eq!(
        command.args,
        vec![
            "install".to_string(),
            "-g".to_string(),
            "--ignore-scripts".to_string(),
            "--minimum-release-age=0".to_string(),
            PACKAGE.to_string(),
        ]
    );
    assert_eq!(
        command.display,
        "bun install -g --ignore-scripts --minimum-release-age=0 @earendil-works/kiana-coding-agent"
    );
}

#[test]
fn self_updates_renamed_pnpm_global_installs_by_removing_the_old_package_first() {
    let _env = EnvScope::new();
    let _install = create_pnpm_global_install();

    let command = get_self_update_command(
        "@mariozechner/kiana-coding-agent",
        None,
        Some(SelfUpdatePackageTarget::Name(
            "@new-scope/kiana".to_string(),
        )),
    )
    .expect("command");

    assert_eq!(detect_install_method(), InstallMethod::Pnpm);
    assert_eq!(command.command, "pnpm");
    assert_eq!(
        command.args,
        vec![
            "install".to_string(),
            "-g".to_string(),
            "--ignore-scripts".to_string(),
            "--config.minimumReleaseAge=0".to_string(),
            "@new-scope/kiana".to_string(),
        ]
    );
    assert_eq!(
        command.display,
        "pnpm remove -g @mariozechner/kiana-coding-agent && pnpm install -g --ignore-scripts --config.minimumReleaseAge=0 @new-scope/kiana"
    );
    let steps = command.steps.expect("steps");
    assert_eq!(
        steps[0].args,
        vec![
            "remove".to_string(),
            "-g".to_string(),
            "@mariozechner/kiana-coding-agent".to_string()
        ]
    );
    assert_eq!(
        steps[1].args,
        vec![
            "install".to_string(),
            "-g".to_string(),
            "--ignore-scripts".to_string(),
            "--config.minimumReleaseAge=0".to_string(),
            "@new-scope/kiana".to_string(),
        ]
    );
}

#[test]
fn self_updates_pnpm_v11_global_installs_resolved_through_the_store() {
    let _env = EnvScope::new();
    let temp = make_temp("kiana-pnpm11-");
    let bin_dir = temp.path().join("bin");
    let root = temp
        .path()
        .join("Library")
        .join("pnpm")
        .join("global")
        .join("v11");
    let global_package_dir = root
        .join("11e9a")
        .join("node_modules")
        .join("@earendil-works")
        .join("kiana-coding-agent");
    let store_package_dir = temp
        .path()
        .join("Library")
        .join("pnpm")
        .join("store")
        .join("v11")
        .join("links")
        .join("@earendil-works")
        .join("kiana-coding-agent")
        .join("0.75.0")
        .join("hash")
        .join("node_modules")
        .join("@earendil-works")
        .join("kiana-coding-agent");
    fs::create_dir_all(&global_package_dir).unwrap();
    fs::create_dir_all(&store_package_dir).unwrap();
    install_fake_bin(&bin_dir, "pnpm", &fake_pnpm_script(&root));
    fs::write(global_package_dir.join("package.json"), "{}").unwrap();
    prepend_path(&bin_dir);
    EnvScope::set("KIANA_PACKAGE_DIR", &store_package_dir);
    set_entrypoint_override_for_test(Some(global_package_dir.join("dist").join("cli.js")));
    set_exec_path(&store_package_dir.join("dist").join("cli.js"));

    let command = get_self_update_command(PACKAGE, None, None).expect("command");

    assert_eq!(detect_install_method(), InstallMethod::Pnpm);
    assert_eq!(command.command, "pnpm");
    assert_eq!(
        command.args,
        vec![
            "install".to_string(),
            "-g".to_string(),
            "--ignore-scripts".to_string(),
            "--config.minimumReleaseAge=0".to_string(),
            PACKAGE.to_string(),
        ]
    );
    assert_eq!(
        command.display,
        "pnpm install -g --ignore-scripts --config.minimumReleaseAge=0 @earendil-works/kiana-coding-agent"
    );
}

#[test]
fn self_updates_renamed_yarn_global_installs_by_removing_the_old_package_first() {
    let _env = EnvScope::new();
    let _install = create_yarn_global_install();

    let command = get_self_update_command(
        "@mariozechner/kiana-coding-agent",
        None,
        Some(SelfUpdatePackageTarget::Name(
            "@new-scope/kiana".to_string(),
        )),
    )
    .expect("command");

    assert_eq!(detect_install_method(), InstallMethod::Yarn);
    assert_eq!(command.command, "yarn");
    assert_eq!(
        command.args,
        vec![
            "global".to_string(),
            "add".to_string(),
            "--ignore-scripts".to_string(),
            "@new-scope/kiana".to_string(),
        ]
    );
    assert_eq!(
        command.display,
        "yarn global remove @mariozechner/kiana-coding-agent && yarn global add --ignore-scripts @new-scope/kiana"
    );
    let steps = command.steps.expect("steps");
    assert_eq!(
        steps[0].args,
        vec![
            "global".to_string(),
            "remove".to_string(),
            "@mariozechner/kiana-coding-agent".to_string()
        ]
    );
    assert_eq!(
        steps[1].args,
        vec![
            "global".to_string(),
            "add".to_string(),
            "--ignore-scripts".to_string(),
            "@new-scope/kiana".to_string(),
        ]
    );
}

#[test]
fn self_updates_renamed_bun_global_installs_by_removing_the_old_package_first() {
    let _env = EnvScope::new();
    let _install = create_bun_global_install();

    let command = get_self_update_command(
        "@mariozechner/kiana-coding-agent",
        None,
        Some(SelfUpdatePackageTarget::Name(
            "@new-scope/kiana".to_string(),
        )),
    )
    .expect("command");

    assert_eq!(detect_install_method(), InstallMethod::Bun);
    assert_eq!(
        command.args,
        vec![
            "install".to_string(),
            "-g".to_string(),
            "--ignore-scripts".to_string(),
            "--minimum-release-age=0".to_string(),
            "@new-scope/kiana".to_string(),
        ]
    );
    assert_eq!(
        command.display,
        "bun uninstall -g @mariozechner/kiana-coding-agent && bun install -g --ignore-scripts --minimum-release-age=0 @new-scope/kiana"
    );
    let steps = command.steps.expect("steps");
    assert_eq!(
        steps[0].args,
        vec![
            "uninstall".to_string(),
            "-g".to_string(),
            "@mariozechner/kiana-coding-agent".to_string()
        ]
    );
}

#[cfg(unix)]
#[test]
fn does_not_self_update_when_npm_install_path_is_not_writable() {
    let _env = EnvScope::new();
    let empty = empty_path_dir();
    EnvScope::set("PATH", empty.path());
    let install = create_npm_prefix_install("kiana-prefix-");
    set_permissions(&install.package_dir, 0o500);

    let command = get_self_update_command(PACKAGE, None, None);
    let instruction = get_self_update_unavailable_instruction(PACKAGE, None, None);

    set_permissions(&install.package_dir, 0o700);

    assert_eq!(command, None);
    assert!(
        instruction.contains("the install path is not writable"),
        "instruction was {instruction:?}"
    );
}

// =============================================================================
// 路径常量/目录
// =============================================================================

#[test]
fn package_dir_reads_the_environment_on_every_call() {
    let _env = EnvScope::new();
    let first = make_temp("kiana-pkg-a-");
    let second = make_temp("kiana-pkg-b-");
    EnvScope::set("KIANA_PACKAGE_DIR", first.path());
    assert_eq!(kiana_core::config::get_package_dir(), first.path());
    EnvScope::set("KIANA_PACKAGE_DIR", second.path());
    assert_eq!(kiana_core::config::get_package_dir(), second.path());
}

#[test]
fn themes_dir_uses_src_or_dist() {
    let _env = EnvScope::new();
    let with_src = make_temp("kiana-pkg-src-");
    fs::create_dir_all(with_src.path().join("src")).unwrap();
    EnvScope::set("KIANA_PACKAGE_DIR", with_src.path());
    assert_eq!(
        kiana_core::config::get_themes_dir(),
        with_src
            .path()
            .join("src")
            .join("modes")
            .join("interactive")
            .join("theme")
    );
    assert_eq!(
        kiana_core::config::get_export_template_dir(),
        with_src.path().join("src").join("core").join("export-html")
    );
    assert_eq!(
        kiana_core::config::get_interactive_assets_dir(),
        with_src
            .path()
            .join("src")
            .join("modes")
            .join("interactive")
            .join("assets")
    );

    let without_src = make_temp("kiana-pkg-dist-");
    EnvScope::set("KIANA_PACKAGE_DIR", without_src.path());
    assert_eq!(
        kiana_core::config::get_themes_dir(),
        without_src
            .path()
            .join("dist")
            .join("modes")
            .join("interactive")
            .join("theme")
    );
    assert_eq!(
        kiana_core::config::get_bundled_interactive_asset_path("x.js"),
        without_src
            .path()
            .join("dist")
            .join("modes")
            .join("interactive")
            .join("assets")
            .join("x.js")
    );
}

#[test]
fn agent_dir_uses_the_environment_and_expands_tilde() {
    let _env = EnvScope::new();
    EnvScope::set("KIANA_CODING_AGENT_DIR", "/custom/agent");
    assert_eq!(
        kiana_core::config::get_agent_dir(),
        Path::new("/custom/agent")
    );
    assert_eq!(
        kiana_core::config::get_settings_path(),
        Path::new("/custom/agent/settings.json")
    );
    assert_eq!(
        kiana_core::config::get_sessions_dir(),
        Path::new("/custom/agent/sessions")
    );
    assert_eq!(
        kiana_core::config::get_debug_log_path(),
        Path::new("/custom/agent/kiana-debug.log")
    );

    EnvScope::set("KIANA_CODING_AGENT_DIR", "~/kiana-agent-x");
    let home = dirs::home_dir().expect("home");
    assert_eq!(
        kiana_core::config::get_agent_dir(),
        home.join("kiana-agent-x")
    );

    EnvScope::remove("KIANA_CODING_AGENT_DIR");
    assert_eq!(
        kiana_core::config::get_agent_dir(),
        home.join(".kiana").join("agent")
    );
}

// getReadmePath/getDocsPath/getExamplesPath/getChangelogPath 用 `resolve(join(...))`，
// 相对 KIANA_PACKAGE_DIR 会得到绝对路径（TS 的 path.resolve 语义）。
#[test]
fn relative_package_dir_yields_absolute_resolved_asset_paths() {
    let _env = EnvScope::new();
    EnvScope::set("KIANA_PACKAGE_DIR", "rel/pkg");
    let process_cwd = std::env::current_dir().expect("cwd");
    assert_eq!(
        kiana_core::config::get_readme_path(),
        process_cwd.join("rel").join("pkg").join("README.md")
    );
    assert_eq!(
        kiana_core::config::get_docs_path(),
        process_cwd.join("rel").join("pkg").join("docs")
    );
    assert_eq!(
        kiana_core::config::get_examples_path(),
        process_cwd.join("rel").join("pkg").join("examples")
    );
    assert_eq!(
        kiana_core::config::get_changelog_path(),
        process_cwd.join("rel").join("pkg").join("CHANGELOG.md")
    );
    // path.join（无 resolve）保持相对路径。
    assert_eq!(
        kiana_core::config::get_themes_dir(),
        PathBuf::from("rel/pkg/dist/modes/interactive/theme")
    );
}

#[test]
fn share_viewer_url_uses_the_environment() {
    let _env = EnvScope::new();
    EnvScope::remove("KIANA_SHARE_VIEWER_URL");
    EnvScope::remove("KIANA_API_BASE_URL");
    assert_eq!(kiana_core::config::get_share_viewer_url("abc"), None);

    EnvScope::set("KIANA_API_BASE_URL", "https://api.example.test/");
    assert_eq!(
        kiana_core::config::get_api_base_url().as_deref(),
        Some("https://api.example.test")
    );
    assert_eq!(
        kiana_core::config::get_share_viewer_url("abc"),
        Some("https://api.example.test/session/#abc".to_string())
    );

    EnvScope::set("KIANA_SHARE_VIEWER_URL", "https://example.test/share/");
    assert_eq!(
        kiana_core::config::get_share_viewer_url("abc"),
        Some("https://example.test/share/#abc".to_string())
    );
}

#[test]
fn package_json_and_readme_paths_follow_the_package_dir() {
    let _env = EnvScope::new();
    let temp = make_temp("kiana-pkg-paths-");
    EnvScope::set("KIANA_PACKAGE_DIR", temp.path());
    assert_eq!(
        kiana_core::config::get_package_json_path(),
        temp.path().join("package.json")
    );
    assert_eq!(
        kiana_core::config::get_readme_path(),
        temp.path().join("README.md")
    );
    assert_eq!(
        kiana_core::config::get_docs_path(),
        temp.path().join("docs")
    );
    assert_eq!(
        kiana_core::config::get_changelog_path(),
        temp.path().join("CHANGELOG.md")
    );
}

// TS 的目录辅助函数用 `path.join`，会折叠 `//`、`.` 与 `..`；PathBuf::join 不会。
#[test]
fn directory_helpers_use_node_path_join_normalization() {
    let _env = EnvScope::new();
    let temp = make_temp("kiana-join-normalize-");
    let package_dir = format!("{}//a/./b/../c", temp.path().display());
    let expected_root = temp.path().join("a").join("c");
    EnvScope::set("KIANA_PACKAGE_DIR", &package_dir);

    assert_eq!(
        kiana_core::config::get_themes_dir(),
        expected_root
            .join("dist")
            .join("modes")
            .join("interactive")
            .join("theme")
    );
    assert_eq!(
        kiana_core::config::get_export_template_dir(),
        expected_root.join("dist").join("core").join("export-html")
    );
    assert_eq!(
        kiana_core::config::get_interactive_assets_dir(),
        expected_root
            .join("dist")
            .join("modes")
            .join("interactive")
            .join("assets")
    );
    assert_eq!(
        kiana_core::config::get_bundled_interactive_asset_path("x.js"),
        expected_root
            .join("dist")
            .join("modes")
            .join("interactive")
            .join("assets")
            .join("x.js")
    );
    assert_eq!(
        kiana_core::config::get_package_json_path(),
        expected_root.join("package.json")
    );
    assert_eq!(
        kiana_core::config::get_readme_path(),
        expected_root.join("README.md")
    );
    assert_eq!(
        kiana_core::config::get_docs_path(),
        expected_root.join("docs")
    );
    assert_eq!(
        kiana_core::config::get_examples_path(),
        expected_root.join("examples")
    );
    assert_eq!(
        kiana_core::config::get_changelog_path(),
        expected_root.join("CHANGELOG.md")
    );

    let agent_dir = format!("{}//agent/./x", temp.path().display());
    let expected_agent = temp.path().join("agent").join("x");
    EnvScope::set("KIANA_CODING_AGENT_DIR", &agent_dir);
    assert_eq!(
        kiana_core::config::get_custom_themes_dir(),
        expected_agent.join("themes")
    );
    assert_eq!(
        kiana_core::config::get_models_path(),
        expected_agent.join("models.json")
    );
    assert_eq!(
        kiana_core::config::get_auth_path(),
        expected_agent.join("auth.json")
    );
    assert_eq!(
        kiana_core::config::get_settings_path(),
        expected_agent.join("settings.json")
    );
    assert_eq!(
        kiana_core::config::get_tools_dir(),
        expected_agent.join("tools")
    );
    assert_eq!(
        kiana_core::config::get_bin_dir(),
        expected_agent.join("bin")
    );
    assert_eq!(
        kiana_core::config::get_prompts_dir(),
        expected_agent.join("prompts")
    );
    assert_eq!(
        kiana_core::config::get_sessions_dir(),
        expected_agent.join("sessions")
    );
    assert_eq!(
        kiana_core::config::get_debug_log_path(),
        expected_agent.join("kiana-debug.log")
    );
}

#[test]
fn quickjs_wasm_path_prefers_the_embedded_path() {
    // 只解析一次并缓存：本进程第一次调用先设置 embedded 路径。
    let path = std::path::PathBuf::from("/tmp/embedded-quickjs.wasm");
    kiana_core::config::set_embedded_quickjs_wasm_path(path.clone());
    assert_eq!(kiana_core::config::get_quickjs_wasm_path(), Some(path));
}

// =============================================================================
// detectInstallChange
// =============================================================================

#[test]
fn detect_install_change_reports_a_removed_install() {
    let temp = make_temp("kiana-install-change-");
    let install_dir = temp.path().join("global").join("hash");
    fs::create_dir_all(&install_dir).unwrap();
    fs::write(
        temp.path().join("package.json"),
        serde_json::json!({ "version": "0.0.1" }).to_string(),
    )
    .unwrap();
    fs::remove_dir_all(&install_dir).unwrap();
    assert_eq!(
        detect_install_change(Some(&install_dir.join("package.json"))),
        Some(InstallChange::Removed)
    );
}

#[test]
fn detect_install_change_reports_updates_and_ignores_the_current_version() {
    let temp = make_temp("kiana-install-change-");
    let newer = temp.path().join("newer.json");
    fs::write(
        &newer,
        serde_json::json!({ "version": "9.9.9" }).to_string(),
    )
    .unwrap();
    assert_eq!(
        detect_install_change(Some(&newer)),
        Some(InstallChange::Updated {
            version: "9.9.9".to_string()
        })
    );

    let same = temp.path().join("same.json");
    fs::write(&same, serde_json::json!({ "version": VERSION }).to_string()).unwrap();
    assert_eq!(detect_install_change(Some(&same)), None);

    let empty = temp.path().join("empty-version.json");
    fs::write(&empty, serde_json::json!({ "version": "" }).to_string()).unwrap();
    assert_eq!(detect_install_change(Some(&empty)), None);

    let invalid = temp.path().join("invalid.json");
    fs::write(&invalid, "not json").unwrap();
    assert_eq!(detect_install_change(Some(&invalid)), None);
}

#[test]
fn detect_install_change_strips_a_bom() {
    let temp = make_temp("kiana-install-change-");
    let path = temp.path().join("bom.json");
    fs::write(
        &path,
        format!("\u{FEFF}{}", serde_json::json!({ "version": "2.0.0" })),
    )
    .unwrap();
    assert_eq!(
        detect_install_change(Some(&path)),
        Some(InstallChange::Updated {
            version: "2.0.0".to_string()
        })
    );
}

// TS 的 `installed.version && installed.version !== VERSION` 接受任意 truthy JSON 值。
#[test]
fn detect_install_change_accepts_truthy_non_string_versions() {
    let temp = make_temp("kiana-install-change-");
    let path = temp.path().join("version.json");

    fs::write(&path, r#"{"version":42}"#).unwrap();
    assert_eq!(
        detect_install_change(Some(&path)),
        Some(InstallChange::Updated {
            version: "42".to_string()
        })
    );

    fs::write(&path, r#"{"version":true}"#).unwrap();
    assert_eq!(
        detect_install_change(Some(&path)),
        Some(InstallChange::Updated {
            version: "true".to_string()
        })
    );

    fs::write(&path, r#"{"version":1.5}"#).unwrap();
    assert_eq!(
        detect_install_change(Some(&path)),
        Some(InstallChange::Updated {
            version: "1.5".to_string()
        })
    );

    // JS 的 `installed.version !== VERSION` 是严格不等：非字符串永远算“变更”，即使
    // `String(value)` 恰好等于 VERSION。
    fs::write(&path, r#"{"version":["1.4.0"]}"#).unwrap();
    assert_eq!(
        detect_install_change(Some(&path)),
        Some(InstallChange::Updated {
            version: "1.4.0".to_string()
        }),
        "an array version must still count as a change"
    );

    // JS 会把 9007199254740993 解析成 f64 9007199254740992，`String(value)` 也如此。
    fs::write(&path, r#"{"version":9007199254740993}"#).unwrap();
    assert_eq!(
        detect_install_change(Some(&path)),
        Some(InstallChange::Updated {
            version: "9007199254740992".to_string()
        })
    );

    // `1e400` 超出 f64：TS 的 JSON.parse 得到 Infinity（truthy 且 !== VERSION），但
    // serde_json 无法表示非有限数（解析失败）→ None。工作区级强制偏差。
    fs::write(&path, r#"{"version":1e400}"#).unwrap();
    assert_eq!(detect_install_change(Some(&path)), None);

    for falsy in [
        r#"{"version":false}"#,
        r#"{"version":0}"#,
        r#"{"version":null}"#,
        r#"{"version":""}"#,
    ] {
        fs::write(&path, falsy).unwrap();
        assert_eq!(
            detect_install_change(Some(&path)),
            None,
            "version was {falsy}"
        );
    }
}

// TS 的 `JSON.parse` 接受 `serde_json` 拒绝的三类输入：数值溢出（`1e400` → Infinity）、
// 孤立代理项转义（`"\ud800"`）、超过 128 层的嵌套。`version` 本身是普通字符串，
// 因此这三种文件都必须照常报告 updated（回归：interactive-mode 的安装变更提示不再被静默跳过）。
#[test]
fn detect_install_change_uses_json_parse_semantics() {
    let temp = make_temp("kiana-install-change-");
    let path = temp.path().join("json-parse.json");

    fs::write(&path, r#"{"version":"2.0.0","x":1e400}"#).unwrap();
    assert_eq!(
        detect_install_change(Some(&path)),
        Some(InstallChange::Updated {
            version: "2.0.0".to_string()
        }),
        "a non-finite sibling value must not abort JSON.parse"
    );

    fs::write(&path, r#"{"version":"2.0.0","s":"\ud800"}"#).unwrap();
    assert_eq!(
        detect_install_change(Some(&path)),
        Some(InstallChange::Updated {
            version: "2.0.0".to_string()
        }),
        "a lone-surrogate escape must not abort JSON.parse"
    );

    let deep = format!(
        "{{\"version\":\"2.0.0\",\"x\":{}{}}}",
        "[".repeat(200),
        "]".repeat(200)
    );
    fs::write(&path, deep).unwrap();
    assert_eq!(
        detect_install_change(Some(&path)),
        Some(InstallChange::Updated {
            version: "2.0.0".to_string()
        }),
        "nesting deeper than serde_json's 128-level recursion limit must parse"
    );
}

#[test]
fn detect_install_change_without_a_path_uses_the_startup_package_json() {
    let _env = EnvScope::new();
    let temp = make_temp("kiana-startup-package-");
    fs::write(
        temp.path().join("package.json"),
        serde_json::json!({ "version": "9.9.9" }).to_string(),
    )
    .unwrap();
    EnvScope::set("KIANA_PACKAGE_DIR", temp.path());
    // STARTUP 在首次调用时捕获（本进程里只有本用例调用 None 形式）。
    assert_eq!(
        detect_install_change(None),
        Some(InstallChange::Updated {
            version: "9.9.9".to_string()
        })
    );
}
