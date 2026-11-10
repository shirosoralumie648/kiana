//! `test/package-command-paths.test.ts`（40 例）的 Rust 版本。
//!
//! TS 测试通过 `main([...])` 驱动（读 `process.cwd()`/`getAgentDir()`/`process.exitCode`）；
//! Rust 直接把 cwd / agent dir / 输出通道传给 [`handle_package_command`]，并按
//! `PackageCommandOutcome` 断言退出码。
//!
//! 覆盖分组：
//! 1. 纯解析与帮助/错误文案（含 TS 生成的逐字 golden）；
//! 2. 信任与设置持久化（install/remove/list/update 的 trust 覆盖、project_trust 扩展）；
//! 3. managed-install（marker、BOM、prune、ELOCKED、--force、失败回滚、cleanup）；
//! 4. 自更新（版本检查、forced/renamed 安装、pnpm 提示、重试、不可自更新时的入口位置）。
//!
//! 所有改动进程级全局状态（env / latest-version URL / exec path）的用例共用一把静态锁。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use kiana_coding_agent::cli::setup::Output;
use kiana_coding_agent::package_manager_cli::{
    PackageCommandOutcome, PackageCommandRuntimeOptions, cleanup_managed_install,
    handle_config_command, handle_package_command,
};
use kiana_config::trust::manager::ProjectTrustStore;
use kiana_core::config::{PACKAGE_NAME, VERSION};
use kiana_ext_api::events::{
    ExtensionEventKind, ExtensionHandler, ProjectTrustContext, ProjectTrustEvent,
    ProjectTrustEventDecision, ProjectTrustEventResult, ProjectTrustHandler,
};
use kiana_ext_api::types::{ExtensionFactory, InlineExtension};
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 串行化的进程环境守卫：保存并恢复所有被改动的变量。
struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn new() -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut guard = Self {
            _lock: lock,
            saved: Vec::new(),
        };
        // TS 的 `beforeEach` 调用 `allowNetwork()`（`vi.stubEnv("KIANA_OFFLINE", undefined)`）；
        // 继承来的离线模式会让 `getLatestKianaRelease` 直接返回 None，自更新用例因此失败。
        guard.remove("KIANA_OFFLINE");
        guard
    }

    fn set(&mut self, key: &'static str, value: impl AsRef<std::ffi::OsStr>) {
        if !self.saved.iter().any(|(saved, _)| *saved == key) {
            self.saved.push((key, std::env::var_os(key)));
        }
        unsafe { std::env::set_var(key, value) };
    }

    fn remove(&mut self, key: &'static str) {
        if !self.saved.iter().any(|(saved, _)| *saved == key) {
            self.saved.push((key, std::env::var_os(key)));
        }
        unsafe { std::env::remove_var(key) };
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..).rev() {
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

struct Workspace {
    _temp: tempfile::TempDir,
    agent_dir: PathBuf,
    project_dir: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let agent_dir = temp.path().join("agent");
        let project_dir = temp.path().join("project");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        std::fs::create_dir_all(&project_dir).expect("project dir");
        Self {
            _temp: temp,
            agent_dir,
            project_dir,
        }
    }

    fn runtime(&self, output: &Output) -> PackageCommandRuntimeOptions {
        PackageCommandRuntimeOptions {
            cwd: self.project_dir.to_string_lossy().into_owned(),
            agent_dir: self.agent_dir.to_string_lossy().into_owned(),
            extension_factories: Vec::new(),
            output: output.clone(),
        }
    }

    fn settings_path(&self) -> PathBuf {
        self.agent_dir.join("settings.json")
    }

    fn project_settings_path(&self) -> PathBuf {
        self.project_dir.join(".kiana").join("settings.json")
    }

    fn read_settings(&self) -> Value {
        let content = std::fs::read_to_string(self.settings_path()).expect("settings.json");
        serde_json::from_str(&content).expect("valid settings json")
    }

    fn read_project_settings(&self) -> Value {
        let content =
            std::fs::read_to_string(self.project_settings_path()).expect("project settings.json");
        serde_json::from_str(&content).expect("valid project settings json")
    }

    fn write_global_settings(&self, value: &Value) {
        std::fs::write(
            self.settings_path(),
            serde_json::to_string_pretty(value).expect("json"),
        )
        .expect("write settings");
    }

    fn write_project_settings(&self, value: &Value) {
        std::fs::create_dir_all(self.project_dir.join(".kiana")).expect("create .kiana");
        std::fs::write(
            self.project_settings_path(),
            serde_json::to_string_pretty(value).expect("json"),
        )
        .expect("write project settings");
    }
}

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

async fn run(workspace: &Workspace, values: &[&str]) -> (PackageCommandOutcome, String, String) {
    run_with_extensions(workspace, values, Vec::new()).await
}

async fn run_with_extensions(
    workspace: &Workspace,
    values: &[&str],
    extension_factories: Vec<InlineExtension>,
) -> (PackageCommandOutcome, String, String) {
    let output = Output::capture();
    let mut runtime = workspace.runtime(&output);
    runtime.extension_factories = extension_factories;
    let outcome = handle_package_command(&args(values), runtime).await;
    (outcome, output.captured_stdout(), output.captured_stderr())
}

/// `kiana.on("project_trust", () => ({ trusted: "yes" }))` 的 Rust 等价工厂。
fn trusting_extension_factory(called: Arc<AtomicBool>) -> ExtensionFactory {
    Arc::new(move |kiana| {
        let called = Arc::clone(&called);
        Box::pin(async move {
            let handler: ProjectTrustHandler = Arc::new(
                move |_event: ProjectTrustEvent, _context: ProjectTrustContext| {
                    called.store(true, Ordering::SeqCst);
                    Box::pin(async {
                        ProjectTrustEventResult {
                            trusted: ProjectTrustEventDecision::Yes,
                            remember: None,
                        }
                    })
                },
            );
            let _ = kiana.on(
                ExtensionEventKind::ProjectTrust,
                ExtensionHandler::ProjectTrust(handler),
            );
        })
    })
}

fn realpath(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).expect("canonicalize")
}

// ---------------------------------------------------------------------------
// 1. 纯解析与帮助/错误文案
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shows_install_subcommand_help_verbatim() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    let (outcome, stdout, stderr) = run(&workspace, &["install", "--help"]).await;
    assert_eq!(outcome, PackageCommandOutcome::HANDLED);
    assert_eq!(
        stdout,
        include_str!("golden/package_help/install.txt").to_string()
    );
    assert_eq!(stderr, "");
}

#[tokio::test]
async fn shows_all_subcommand_help_verbatim() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    for (command, golden) in [
        ("remove", include_str!("golden/package_help/remove.txt")),
        ("update", include_str!("golden/package_help/update.txt")),
        ("list", include_str!("golden/package_help/list.txt")),
    ] {
        let (outcome, stdout, stderr) = run(&workspace, &[command, "--help"]).await;
        assert_eq!(outcome, PackageCommandOutcome::HANDLED, "{command}");
        assert_eq!(stdout, golden, "{command}");
        assert_eq!(stderr, "", "{command}");
    }
}

#[tokio::test]
async fn shows_config_subcommand_help_verbatim() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    let output = Output::capture();
    let outcome =
        handle_config_command(&args(&["config", "--help"]), workspace.runtime(&output)).await;
    assert_eq!(outcome, PackageCommandOutcome::HANDLED);
    assert_eq!(
        output.captured_stdout(),
        include_str!("golden/package_help/config.txt")
    );
    assert_eq!(output.captured_stderr(), "");

    // `config` 不是 package 命令。
    let (outcome, _, _) = run(&workspace, &["config", "--help"]).await;
    assert_eq!(outcome, PackageCommandOutcome::NOT_HANDLED);
}

#[tokio::test]
async fn shows_a_friendly_error_for_unknown_install_options() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    let (outcome, stdout, stderr) = run(&workspace, &["install", "--unknown"]).await;
    assert_eq!(outcome.exit_code, 1);
    assert!(stdout.is_empty());
    assert!(stderr.contains("Unknown option --unknown for \"install\"."));
    assert!(stderr.contains(
        "Use \"kiana --help\" or \"kiana install <source> [-l] [--approve|--no-approve]\"."
    ));
}

#[tokio::test]
async fn shows_a_friendly_error_for_missing_install_source() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    let (outcome, _, stderr) = run(&workspace, &["install"]).await;
    assert_eq!(outcome.exit_code, 1);
    assert!(stderr.contains("Missing install source."));
    assert!(stderr.contains("Usage: kiana install <source> [-l] [--approve|--no-approve]"));
}

#[tokio::test]
async fn rejects_unexpected_arguments_and_missing_option_values() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();

    let (outcome, _, stderr) = run(&workspace, &["install", "a", "b"]).await;
    assert_eq!(outcome.exit_code, 1);
    assert!(stderr.contains("Unexpected argument b."));

    let (outcome, _, stderr) = run(&workspace, &["update", "--extension"]).await;
    assert_eq!(outcome.exit_code, 1);
    assert!(stderr.contains("Missing value for --extension."));

    let (outcome, _, stderr) = run(&workspace, &["update", "--models", "--self"]).await;
    assert_eq!(outcome.exit_code, 1);
    assert!(stderr.contains("--models cannot be combined with --self"));
}

#[tokio::test]
async fn returns_not_handled_for_other_commands() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    let (outcome, stdout, stderr) = run(&workspace, &["auth", "login"]).await;
    assert_eq!(outcome, PackageCommandOutcome::NOT_HANDLED);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

// ---------------------------------------------------------------------------
// 2. 信任与设置持久化
// ---------------------------------------------------------------------------

#[tokio::test]
async fn persists_global_relative_local_package_paths_relative_to_settings_json() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    let relative_pkg_dir = workspace.project_dir.join("packages").join("local-package");
    std::fs::create_dir_all(&relative_pkg_dir).expect("package dir");

    let (outcome, stdout, stderr) = run(&workspace, &["install", "./packages/local-package"]).await;
    assert_eq!(outcome, PackageCommandOutcome::HANDLED, "stderr: {stderr}");
    assert!(stdout.contains("Installed ./packages/local-package"));

    let settings = workspace.read_settings();
    let packages = settings
        .get("packages")
        .and_then(Value::as_array)
        .expect("packages array");
    assert_eq!(packages.len(), 1);
    let stored = packages[0].as_str().expect("string package");
    let resolved = realpath(&workspace.agent_dir.join(stored));
    assert_eq!(resolved, realpath(&relative_pkg_dir));
}

#[tokio::test]
async fn removes_local_packages_using_a_path_with_a_trailing_slash() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    let package_dir = workspace.project_dir.join("local-package");
    std::fs::create_dir_all(&package_dir).expect("package dir");
    let install_source = format!("{}/", package_dir.display());

    let (outcome, _, stderr) = run(&workspace, &["install", &install_source]).await;
    assert_eq!(outcome, PackageCommandOutcome::HANDLED, "stderr: {stderr}");
    let installed = workspace.read_settings();
    assert_eq!(
        installed
            .get("packages")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );

    let (outcome, _, stderr) = run(&workspace, &["remove", &install_source]).await;
    assert_eq!(outcome, PackageCommandOutcome::HANDLED, "stderr: {stderr}");
    let removed = workspace.read_settings();
    assert_eq!(
        removed
            .get("packages")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        0
    );
}

#[tokio::test]
async fn skips_untrusted_project_package_settings() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    workspace.write_project_settings(&serde_json::json!({ "packages": ["npm:@project/pkg"] }));

    let (outcome, stdout, stderr) = run(&workspace, &["list"]).await;
    assert_eq!(outcome, PackageCommandOutcome::HANDLED);
    assert_eq!(outcome.exit_code, 0);
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("No packages installed."));
    assert!(!stdout.contains("Project packages:"));
}

#[tokio::test]
async fn uses_remembered_project_trust_for_list() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    workspace.write_project_settings(&serde_json::json!({ "packages": ["npm:@project/pkg"] }));
    ProjectTrustStore::new(&workspace.agent_dir.to_string_lossy())
        .set(&workspace.project_dir.to_string_lossy(), Some(true))
        .expect("set trust");

    let (outcome, stdout, stderr) = run(&workspace, &["list"]).await;
    assert_eq!(outcome.exit_code, 0);
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("Project packages:"));
    assert!(stdout.contains("npm:@project/pkg"));
    assert!(!stdout.contains("No packages installed."));
}

#[tokio::test]
async fn overrides_remembered_trust_for_list_with_no_approve() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    workspace.write_project_settings(&serde_json::json!({ "packages": ["npm:@project/pkg"] }));
    ProjectTrustStore::new(&workspace.agent_dir.to_string_lossy())
        .set(&workspace.project_dir.to_string_lossy(), Some(true))
        .expect("set trust");

    let (outcome, stdout, _) = run(&workspace, &["list", "--no-approve"]).await;
    assert_eq!(outcome.exit_code, 0);
    assert!(stdout.contains("No packages installed."));
    assert!(!stdout.contains("Project packages:"));
}

#[tokio::test]
async fn approves_project_trust_for_list_with_approve() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    workspace.write_project_settings(&serde_json::json!({ "packages": ["npm:@project/pkg"] }));

    let (outcome, stdout, _) = run(&workspace, &["list", "--approve"]).await;
    assert_eq!(outcome.exit_code, 0);
    assert!(stdout.contains("Project packages:"));
    assert!(stdout.contains("npm:@project/pkg"));
}

#[tokio::test]
async fn uses_default_project_trust_for_list() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    workspace.write_global_settings(&serde_json::json!({ "defaultProjectTrust": "always" }));
    workspace.write_project_settings(&serde_json::json!({ "packages": ["npm:@project/pkg"] }));

    let (outcome, stdout, _) = run(&workspace, &["list"]).await;
    assert_eq!(outcome.exit_code, 0);
    assert!(stdout.contains("Project packages:"));
    assert!(stdout.contains("npm:@project/pkg"));
}

#[tokio::test]
async fn lets_trust_json_override_default_project_trust() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    workspace.write_global_settings(&serde_json::json!({ "defaultProjectTrust": "always" }));
    workspace.write_project_settings(&serde_json::json!({ "packages": ["npm:@project/pkg"] }));
    ProjectTrustStore::new(&workspace.agent_dir.to_string_lossy())
        .set(&workspace.project_dir.to_string_lossy(), Some(false))
        .expect("set trust");

    let (outcome, stdout, _) = run(&workspace, &["list"]).await;
    assert_eq!(outcome.exit_code, 0);
    assert!(stdout.contains("No packages installed."));
    assert!(!stdout.contains("Project packages:"));
}

#[tokio::test]
async fn blocks_local_package_changes_when_project_is_untrusted() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    workspace.write_project_settings(&serde_json::json!({}));

    let (outcome, _, stderr) = run(&workspace, &["install", "-l", "./local-package"]).await;
    assert_eq!(outcome.exit_code, 1);
    assert!(
        stderr.contains("Project is not trusted. Use --approve to modify local package config.")
    );
}

#[tokio::test]
async fn allows_local_package_install_to_initialize_fresh_project_settings() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    let package_dir = workspace.project_dir.join("local-package");
    std::fs::create_dir_all(&package_dir).expect("package dir");

    let source = package_dir.to_string_lossy().into_owned();
    let (outcome, _, stderr) = run(&workspace, &["install", "-l", &source]).await;
    assert_eq!(outcome, PackageCommandOutcome::HANDLED, "stderr: {stderr}");
    assert_eq!(outcome.exit_code, 0);

    let settings = workspace.read_project_settings();
    let packages = settings
        .get("packages")
        .and_then(Value::as_array)
        .expect("packages array");
    assert_eq!(packages.len(), 1);
    let stored = packages[0].as_str().expect("string package");
    let resolved = realpath(&workspace.project_dir.join(".kiana").join(stored));
    assert_eq!(resolved, realpath(&package_dir));
}

/// 空字符串在 TS 里是假值：`kiana install ""` 必须报 “Missing install source.”，而不是把
/// `""` 交给 package manager。
#[tokio::test]
async fn treats_an_empty_install_source_as_missing() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    let (outcome, stdout, stderr) = run(&workspace, &["install", ""]).await;
    assert_eq!(outcome.exit_code, 1);
    assert!(stderr.contains("Missing install source."), "{stderr}");
    assert!(
        stderr.contains("Usage: kiana install <source> [-l] [--approve|--no-approve]"),
        "{stderr}"
    );
    assert!(stdout.is_empty());
    assert!(!workspace.settings_path().exists());
}

/// 按 `spec/design-extension-runtime.md` §9.2 的顺序装配 JS 引擎（project_trust 派发需要）。
fn install_extension_engine() -> std::rc::Rc<kiana_extensions::engine::JsEngine> {
    use kiana_extensions::engine::{self, JsEngine};
    use kiana_extensions::loader;
    let engine = std::rc::Rc::new(JsEngine::new().expect("create engine"));
    loader::js_host::install_intl(&engine).expect("install intl");
    engine::install(&engine).expect("install prelude");
    loader::js_host::install(&engine).expect("install node globals");
    kiana_extensions::virtual_modules::register_module_sources(&engine)
        .expect("register virtual sources");
    loader::install(std::rc::Rc::clone(&engine)).expect("install extension loader + api.js");
    kiana_extensions::virtual_modules::install(&engine).expect("install virtual modules");
    kiana_extensions::runner::dispatch::install(std::rc::Rc::clone(&engine))
        .expect("install dispatch");
    engine
}

/// `project_trust` 扩展返回 `trusted: "yes"` 时，`kiana list` 应按已信任项目读取项目包。
#[test]
fn uses_project_trust_extensions_for_package_commands() {
    let _env = EnvGuard::new();
    let _engine = install_extension_engine();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async {
        let workspace = Workspace::new();
        workspace.write_project_settings(&serde_json::json!({ "packages": ["npm:@project/pkg"] }));
        let called = Arc::new(AtomicBool::new(false));
        let factory = trusting_extension_factory(Arc::clone(&called));
        let output = Output::capture();
        let mut options = workspace.runtime(&output);
        options.extension_factories = vec![InlineExtension::Factory(factory)];

        let outcome = handle_package_command(&args(&["list"]), options).await;

        assert_eq!(outcome.exit_code, 0, "stderr: {}", output.captured_stderr());
        assert!(
            called.load(Ordering::SeqCst),
            "project_trust handler must run; stderr={:?}",
            output.captured_stderr()
        );
        let stdout = output.captured_stdout();
        assert!(stdout.contains("Project packages:"), "{stdout}");
        assert!(stdout.contains("npm:@project/pkg"), "{stdout}");
        assert!(!stdout.contains("No packages installed."), "{stdout}");
    });
}

/// `update` 使用 `useSavedProjectTrustOnly`：不加载扩展、不向扩展询问信任。
#[cfg(unix)]
#[tokio::test]
async fn does_not_prompt_or_ask_extensions_for_project_trust_during_update() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    let record_path = workspace._temp.path().join("project-update.txt");
    let npm_path = workspace._temp.path().join("fake-project-npm.sh");
    write_executable(
        &npm_path,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 0\n",
            record_path.display()
        ),
    );
    workspace.write_global_settings(&serde_json::json!({
        "defaultProjectTrust": "always",
        "npmCommand": [npm_path.to_string_lossy()],
    }));
    workspace.write_project_settings(&serde_json::json!({
        "packages": ["npm:fake-package"],
    }));
    let extension_asked = Arc::new(AtomicBool::new(false));
    let factory = trusting_extension_factory(Arc::clone(&extension_asked));

    let (outcome, _, stderr) = run_with_extensions(
        &workspace,
        &["update", "--extensions"],
        vec![InlineExtension::Factory(factory)],
    )
    .await;
    assert_eq!(outcome.exit_code, 0, "stderr: {stderr}");
    assert!(
        !extension_asked.load(Ordering::SeqCst),
        "update must not ask extensions for project trust"
    );
    assert!(
        !record_path.exists(),
        "project packages must not be updated"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn uses_saved_project_trust_during_update() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    let record_path = workspace._temp.path().join("trusted-project-update.txt");
    let npm_path = workspace._temp.path().join("fake-trusted-npm.sh");
    write_executable(
        &npm_path,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 0\n",
            record_path.display()
        ),
    );
    // kiana-resources 的 `get_npm_command` 目前只读全局设置（TS 读合并设置），因此假 npm
    // 放在全局作用域；项目作用域仍然只声明要更新的包，保存的信任决定它们是否被更新。
    workspace.write_global_settings(&serde_json::json!({
        "npmCommand": [npm_path.to_string_lossy()],
    }));
    workspace.write_project_settings(&serde_json::json!({
        "packages": ["npm:fake-package"],
    }));
    ProjectTrustStore::new(&workspace.agent_dir.to_string_lossy())
        .set(&workspace.project_dir.to_string_lossy(), Some(true))
        .expect("set trust");

    let (outcome, _, stderr) = run(&workspace, &["update", "--extensions"]).await;
    assert_eq!(outcome.exit_code, 0, "stderr: {stderr}");
    assert!(
        record_path.exists(),
        "saved trust must enable project packages"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn suggests_the_configured_source_when_update_input_omits_the_npm_prefix() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    workspace.write_global_settings(&serde_json::json!({ "packages": ["npm:kiana-formatter"] }));

    let (outcome, stdout, stderr) = run(&workspace, &["update", "kiana-formatter"]).await;
    assert_eq!(outcome.exit_code, 1);
    assert!(
        stderr.contains("Did you mean npm:kiana-formatter?"),
        "{stderr}"
    );
    assert!(!stdout.contains("Updated kiana-formatter"));

    let settings = workspace.read_settings();
    assert!(
        settings
            .get("packages")
            .and_then(Value::as_array)
            .is_some_and(|packages| packages
                .iter()
                .any(|package| package == "npm:kiana-formatter"))
    );
}

// ---------------------------------------------------------------------------
// managed install
// ---------------------------------------------------------------------------

fn newer_patch_version() -> String {
    let mut parts = VERSION.split('.');
    let major = parts.next().unwrap_or("0");
    let minor = parts.next().unwrap_or("0");
    let patch: u64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    format!("{major}.{minor}.{}", patch + 1)
}

#[cfg(unix)]
fn write_executable(path: &Path, content: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, content).expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// 与 TS 的 `prepareManagedInstall` 等价的测试装置。
struct ManagedInstall {
    managed_root: PathBuf,
    self_package_dir: PathBuf,
    npm_record_path: PathBuf,
    bin_dir: PathBuf,
}

#[cfg(unix)]
fn prepare_managed_install(
    workspace: &Workspace,
    target_version: &str,
    npm_exit_code: i32,
) -> ManagedInstall {
    let managed_root = workspace.agent_dir.join("install");
    let active_release = managed_root.join("releases").join(VERSION);
    let self_package_dir = active_release
        .join("node_modules")
        .join("@earendil-works")
        .join("kiana-coding-agent");
    std::fs::create_dir_all(&self_package_dir).expect("self package dir");
    std::fs::write(active_release.join("active.txt"), "active").expect("active marker");
    std::fs::write(managed_root.join("current-version"), format!("{VERSION}\n"))
        .expect("current version");
    std::fs::write(
        managed_root.join("managed-install.json"),
        format!(
            "{}\n",
            serde_json::json!({
                "kind": "kiana-managed-install",
                "schemaVersion": 1,
                "layout": "releases-v1",
            })
        ),
    )
    .expect("marker");

    let bin_dir = workspace._temp.path().join("managed-bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    let npm_record_path = workspace._temp.path().join("managed-npm-record.txt");
    let npm_script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{record}'\nmkdir -p node_modules/.bin\ncat > node_modules/.bin/kiana <<'PISCRIPT'\n#!/bin/sh\necho {target}\nPISCRIPT\nchmod +x node_modules/.bin/kiana\nif [ {code} -ne 0 ]; then exit {code}; fi\nexit 0\n",
        record = npm_record_path.display(),
        target = target_version,
        code = npm_exit_code,
    );
    write_executable(&bin_dir.join("npm"), &npm_script);

    ManagedInstall {
        managed_root,
        self_package_dir,
        npm_record_path,
        bin_dir,
    }
}

#[cfg(unix)]
fn managed_env(
    guard: &mut EnvGuard,
    workspace: &Workspace,
    install: &ManagedInstall,
    server: &MockServer,
) {
    guard.set("KIANA_MANAGED_INSTALL_ROOT", &install.managed_root);
    guard.set(
        "KIANA_INSTALLER_API_BASE",
        format!("{}/api/installer/releases", server.uri()),
    );
    guard.set("KIANA_PACKAGE_DIR", &install.self_package_dir);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut combined = OsString::from(install.bin_dir.as_os_str());
    combined.push(":");
    combined.push(path);
    guard.set("PATH", combined);
    let _ = workspace;
}

#[cfg(unix)]
async fn mount_release_artifacts(server: &MockServer, target_version: &str) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/installer/releases/{target_version}/package.json"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/installer/releases/{target_version}/package-lock.json"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(server)
        .await;
}

#[cfg(unix)]
async fn mount_latest_version(server: &MockServer, version: &str) {
    Mock::given(method("GET"))
        .and(path("/api/latest-version"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "version": version })),
        )
        .mount(server)
        .await;
}

#[cfg(unix)]
#[tokio::test]
async fn updates_installer_managed_kiana_through_a_staged_immutable_release() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let target_version = newer_patch_version();
    let install = prepare_managed_install(&workspace, &target_version, 0);
    let server = MockServer::start().await;
    managed_env(&mut env, &workspace, &install, &server);
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));
    mount_latest_version(&server, &target_version).await;
    mount_release_artifacts(&server, &target_version).await;

    // 过期锁与遗留 staging 目录应当被清理。
    let abandoned_stage = install
        .managed_root
        .join("staging")
        .join("update-abandoned");
    std::fs::create_dir_all(&abandoned_stage).expect("abandoned stage");
    std::fs::write(abandoned_stage.join("partial"), "partial").expect("partial");
    let abandoned_lock = install.managed_root.join("update.lock");
    std::fs::create_dir_all(&abandoned_lock).expect("abandoned lock");
    let epoch = std::time::UNIX_EPOCH;
    let file = std::fs::File::open(&abandoned_lock).expect("open lock");
    file.set_times(std::fs::FileTimes::new().set_modified(epoch))
        .expect("set stale mtime");
    drop(file);

    let (outcome, stdout, stderr) = run(&workspace, &["update", "--self"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains(&format!("Updated kiana from {VERSION} to {target_version}")));
    assert_eq!(
        std::fs::read_to_string(install.managed_root.join("current-version")).expect("current"),
        format!("{target_version}\n")
    );
    assert!(
        install
            .managed_root
            .join("releases")
            .join(&target_version)
            .exists()
    );
    assert!(
        install
            .managed_root
            .join("releases")
            .join(VERSION)
            .join("active.txt")
            .exists()
    );
    let staging: Vec<String> = std::fs::read_dir(install.managed_root.join("staging"))
        .expect("staging")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(staging.is_empty(), "staging not empty: {staging:?}");

    let recorded = std::fs::read_to_string(&install.npm_record_path).expect("npm record");
    assert!(recorded.lines().any(|line| line == "ci"));
    assert!(recorded.lines().any(|line| line == "--ignore-scripts"));
}

/// `mkdtempSync` 的失败文案：Node 的 syscall 是 `mkdtemp`，路径是带 `XXXXXX` 的模板
/// （不是 `mkdir '<root>/update-'`）。
#[cfg(unix)]
#[tokio::test]
async fn reports_mkdtemp_failures_with_the_node_mkdtemp_syscall() {
    use std::os::unix::fs::PermissionsExt;

    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let target_version = newer_patch_version();
    let install = prepare_managed_install(&workspace, &target_version, 0);
    let server = MockServer::start().await;
    managed_env(&mut env, &workspace, &install, &server);
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));
    mount_latest_version(&server, &target_version).await;

    // 只读的 staging 目录让 `mkdtempSync` 以 EACCES 失败。
    let staging = install.managed_root.join("staging");
    std::fs::create_dir_all(&staging).expect("staging");
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o555))
        .expect("chmod staging");
    // root（容器里常见）无视目录权限，无法制造 EACCES。
    if std::fs::write(staging.join("probe"), b"").is_ok() {
        let _ = std::fs::remove_file(staging.join("probe"));
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))
            .expect("restore staging");
        return;
    }

    let (outcome, _, stderr) = run(&workspace, &["update", "--self"]).await;
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))
        .expect("restore staging");
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 1, "stderr: {stderr}");
    let template = staging.join("update-XXXXXX");
    assert!(
        stderr.contains(&format!(
            "EACCES: permission denied, mkdtemp '{}'",
            template.display()
        )),
        "{stderr}"
    );
}

/// proper-lockfile 用 `rmdir` 删除过期锁：锁路径被普通文件占据时，Node 的文案是
/// `ENOTDIR: not a directory, rmdir '<lock>'`（不是 `mkdir`）。
#[cfg(unix)]
#[tokio::test]
async fn reports_stale_lock_removal_failures_with_the_rmdir_syscall() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let target_version = newer_patch_version();
    let install = prepare_managed_install(&workspace, &target_version, 0);
    let server = MockServer::start().await;
    managed_env(&mut env, &workspace, &install, &server);
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));
    mount_latest_version(&server, &target_version).await;

    let lock = install.managed_root.join("update.lock");
    std::fs::write(&lock, "not a directory").expect("lock file");
    let file = std::fs::File::open(&lock).expect("open lock");
    file.set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .expect("backdate lock");
    drop(file);

    let (outcome, _, stderr) = run(&workspace, &["update", "--self"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 1, "stderr: {stderr}");
    assert!(
        stderr.contains(&format!(
            "ENOTDIR: not a directory, rmdir '{}'",
            lock.display()
        )),
        "{stderr}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn accepts_a_managed_install_marker_with_a_utf8_bom() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let target_version = newer_patch_version();
    let install = prepare_managed_install(&workspace, &target_version, 0);
    std::fs::write(
        install.managed_root.join("managed-install.json"),
        format!(
            "\u{feff}{}\r\n",
            serde_json::json!({
                "kind": "kiana-managed-install",
                "schemaVersion": 1,
                "layout": "releases-v1",
            })
        ),
    )
    .expect("marker with bom");
    let server = MockServer::start().await;
    managed_env(&mut env, &workspace, &install, &server);
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));
    mount_latest_version(&server, &target_version).await;
    mount_release_artifacts(&server, &target_version).await;

    let (outcome, _, stderr) = run(&workspace, &["update", "--self"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert_eq!(
        std::fs::read_to_string(install.managed_root.join("current-version")).expect("current"),
        format!("{target_version}\n")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn removes_older_managed_releases_after_an_update() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let target_version = newer_patch_version();
    let install = prepare_managed_install(&workspace, &target_version, 0);
    let releases_root = install.managed_root.join("releases");
    std::fs::create_dir_all(releases_root.join("0.0.1").join("node_modules")).expect("old");
    std::fs::create_dir_all(releases_root.join("0.0.2-beta.1")).expect("old beta");
    std::fs::create_dir_all(releases_root.join("not-a-release")).expect("not a release");
    let server = MockServer::start().await;
    managed_env(&mut env, &workspace, &install, &server);
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));
    mount_latest_version(&server, &target_version).await;
    mount_release_artifacts(&server, &target_version).await;

    let (outcome, _, stderr) = run(&workspace, &["update", "--self"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);
    assert_eq!(outcome.exit_code, 0, "stderr: {stderr}");

    let mut entries: Vec<String> = std::fs::read_dir(&releases_root)
        .expect("releases")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    let mut expected = vec![
        VERSION.to_string(),
        "not-a-release".to_string(),
        target_version.clone(),
    ];
    expected.sort();
    assert_eq!(entries, expected);
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_a_concurrent_managed_update() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let target_version = newer_patch_version();
    let install = prepare_managed_install(&workspace, &target_version, 0);
    let server = MockServer::start().await;
    managed_env(&mut env, &workspace, &install, &server);
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));
    mount_latest_version(&server, &target_version).await;
    mount_release_artifacts(&server, &target_version).await;

    // 新鲜锁目录（mtime 现在是 proper-lockfile 的 probe 值附近）。
    std::fs::create_dir_all(install.managed_root.join("update.lock")).expect("lock");

    let (outcome, stdout, stderr) = run(&workspace, &["update", "--self"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 1);
    assert!(!stdout.contains("Updated kiana from"));
    assert!(
        stderr.contains("Another managed Kiana update is already running."),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(install.managed_root.join("current-version")).expect("current"),
        format!("{VERSION}\n")
    );
    assert!(!install.npm_record_path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_forced_managed_reinstalls() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let target_version = newer_patch_version();
    let install = prepare_managed_install(&workspace, &target_version, 0);
    let server = MockServer::start().await;
    managed_env(&mut env, &workspace, &install, &server);
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));

    let (outcome, _, stderr) = run(&workspace, &["update", "--self", "--force"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 1);
    assert!(
        stderr.contains("Managed kiana installations do not support --force"),
        "{stderr}"
    );
    assert!(!install.npm_record_path.exists());
    assert_eq!(
        std::fs::read_to_string(install.managed_root.join("current-version")).expect("current"),
        format!("{VERSION}\n")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn keeps_the_managed_release_active_when_its_update_fails() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let target_version = newer_patch_version();
    let install = prepare_managed_install(&workspace, &target_version, 23);
    std::fs::create_dir_all(install.managed_root.join("releases").join("0.0.1")).expect("old");
    let server = MockServer::start().await;
    managed_env(&mut env, &workspace, &install, &server);
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));
    mount_latest_version(&server, &target_version).await;
    mount_release_artifacts(&server, &target_version).await;

    let (outcome, stdout, stderr) = run(&workspace, &["update", "--self"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 1);
    assert!(!stdout.contains("Updated kiana from"));
    assert!(stderr.contains("exited with code 23"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(install.managed_root.join("current-version")).expect("current"),
        format!("{VERSION}\n")
    );
    assert!(
        !install
            .managed_root
            .join("releases")
            .join(&target_version)
            .exists()
    );
    assert!(install.managed_root.join("releases").join("0.0.1").exists());
    let staging: Vec<String> = std::fs::read_dir(install.managed_root.join("staging"))
        .expect("staging")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(staging.is_empty(), "staging not empty: {staging:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn reports_invalid_managed_install_markers() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let target_version = newer_patch_version();
    let install = prepare_managed_install(&workspace, &target_version, 0);
    let marker_path = install.managed_root.join("managed-install.json");
    std::fs::write(&marker_path, "{\"kind\":\"other\"}").expect("invalid marker");
    let server = MockServer::start().await;
    managed_env(&mut env, &workspace, &install, &server);

    let (outcome, _, stderr) = run(&workspace, &["update", "--self"]).await;
    assert_eq!(outcome.exit_code, 1);
    assert!(
        stderr.contains(&format!(
            "Managed install marker is missing or invalid: {}",
            marker_path.display()
        )),
        "{stderr}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cleanup_managed_install_skips_a_locked_staging_directory() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let target_version = newer_patch_version();
    let install = prepare_managed_install(&workspace, &target_version, 0);
    let staging = install
        .managed_root
        .join("staging")
        .join("update-abandoned");
    std::fs::create_dir_all(&staging).expect("staging");
    std::fs::write(staging.join("partial"), "partial").expect("partial");
    std::fs::create_dir_all(install.managed_root.join("update.lock")).expect("lock");
    env.set("KIANA_MANAGED_INSTALL_ROOT", &install.managed_root);
    env.set("KIANA_PACKAGE_DIR", &install.self_package_dir);

    cleanup_managed_install();

    assert!(staging.exists(), "locked staging must be preserved");
}

#[cfg(unix)]
#[tokio::test]
async fn cleanup_managed_install_cleans_staging_with_a_stale_lock() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let target_version = newer_patch_version();
    let install = prepare_managed_install(&workspace, &target_version, 0);
    let staging = install
        .managed_root
        .join("staging")
        .join("update-abandoned");
    std::fs::create_dir_all(&staging).expect("staging");
    std::fs::write(staging.join("partial"), "partial").expect("partial");
    let lock = install.managed_root.join("update.lock");
    std::fs::create_dir_all(&lock).expect("lock");
    let file = std::fs::File::open(&lock).expect("open lock");
    file.set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .expect("stale mtime");
    drop(file);
    env.set("KIANA_MANAGED_INSTALL_ROOT", &install.managed_root);
    env.set("KIANA_PACKAGE_DIR", &install.self_package_dir);

    cleanup_managed_install();

    assert!(!staging.exists(), "stale staging should be cleaned");
}

#[cfg(unix)]
#[tokio::test]
async fn cleanup_managed_install_is_silent_for_invalid_markers() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let target_version = newer_patch_version();
    let install = prepare_managed_install(&workspace, &target_version, 0);
    std::fs::write(
        install.managed_root.join("managed-install.json"),
        "not json",
    )
    .expect("invalid marker");
    env.set("KIANA_MANAGED_INSTALL_ROOT", &install.managed_root);
    env.set("KIANA_PACKAGE_DIR", &install.self_package_dir);

    // 不 panic、不打印。
    cleanup_managed_install();
}

// ---------------------------------------------------------------------------
// 自更新计划
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_explicit_self_update_checks_when_automatic_checks_are_disabled() {
    let mut env = EnvGuard::new();
    env.set("KIANA_SKIP_VERSION_CHECK", "1");
    env.remove("KIANA_MANAGED_INSTALL_ROOT");
    let workspace = Workspace::new();
    let server = MockServer::start().await;
    mount_latest_version(&server, VERSION).await;
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));

    let (outcome, stdout, stderr) = run(&workspace, &["update", "--self"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(
        stdout.contains(&format!("kiana is already up to date (v{VERSION})")),
        "{stdout}"
    );
}

#[tokio::test]
async fn reports_self_update_check_failures() {
    let mut env = EnvGuard::new();
    env.remove("KIANA_MANAGED_INSTALL_ROOT");
    env.remove("KIANA_SKIP_VERSION_CHECK");
    let workspace = Workspace::new();
    let server = MockServer::start().await;
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));

    let (outcome, _, stderr) = run(&workspace, &["update", "--self"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 1);
    // wiremock 对未挂载路径返回 404 → `getLatestKianaRelease` 返回 None。
    assert!(
        stderr.contains("Could not determine latest kiana version."),
        "{stderr}"
    );
}

// 该用例会真正调用模型目录刷新（`kiana update --models` → ModelRuntime.refresh →
// api.anthropic.com / models.dev），沙箱/CI 无法访问这两个端点时必然失败。按项目规则
// （Live-API 测试用 #[ignore] 并在支持联网的环境手动运行）标记为忽略。
#[ignore = "live network: model catalog refresh requires api.anthropic.com/models.dev access"]
#[tokio::test]
async fn refreshes_only_model_catalogs_with_update_models() {
    let _env = EnvGuard::new();
    let workspace = Workspace::new();
    let server = MockServer::start().await;
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));
    let (outcome, stdout, stderr) = run(&workspace, &["update", "--models"]).await;
    // TS 断言 `ModelRuntime.create` / `refresh` 的参数；Rust 侧没有 spy，这里改为断言
    // `--models` 不会触发自更新的版本检查（URL 指向 wiremock，未挂载任何路径）。
    let requests = server
        .received_requests()
        .await
        .map(|requests| requests.len());
    kiana_core::changelog::set_latest_version_url_for_tests(None);
    assert_eq!(outcome.exit_code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("Model catalogs refreshed"), "{stdout}");
    assert_eq!(
        requests,
        Some(0),
        "--models must not run a self-update check"
    );
    assert!(!stdout.contains("Updating kiana"), "{stdout}");
}

// ---------------------------------------------------------------------------
// 自更新执行（npm / pnpm / 版本检查）
// ---------------------------------------------------------------------------

/// 受控的假 npm：`root -g` 返回 `<prefix>/lib/node_modules`，其它调用记录参数后按需失败。
#[cfg(unix)]
struct FakeNpm {
    script: PathBuf,
    calls_dir: PathBuf,
}

#[cfg(unix)]
impl FakeNpm {
    fn calls(&self) -> Vec<Vec<String>> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.calls_dir)
            .expect("calls dir")
            .flatten()
            .map(|entry| entry.path())
            .collect();
        files.sort();
        files
            .iter()
            .map(|path| {
                std::fs::read_to_string(path)
                    .expect("call record")
                    .lines()
                    .map(str::to_string)
                    .collect()
            })
            .collect()
    }

    fn command(&self, prefix: &Path) -> Value {
        serde_json::json!([
            self.script.to_string_lossy().into_owned(),
            "--prefix",
            prefix.to_string_lossy().into_owned(),
        ])
    }
}

#[cfg(unix)]
fn write_fake_npm(dir: &Path, fail_on_install: bool) -> FakeNpm {
    let script = dir.join("fake-npm.sh");
    let calls_dir = dir.join("npm-calls");
    std::fs::create_dir_all(&calls_dir).expect("calls dir");
    let counter = dir.join("npm-call-count");
    let fail = if fail_on_install { "exit 23" } else { "exit 0" };
    let content = format!(
        r#"#!/bin/sh
prefix=''
prev=''
for arg in "$@"; do
  if [ "$prev" = "--prefix" ]; then prefix="$arg"; fi
  prev="$arg"
done
for arg in "$@"; do
  if [ "$arg" = "root" ]; then
    printf '%s\n' "$prefix/lib/node_modules"
    exit 0
  fi
done
count=0
if [ -f '{counter}' ]; then count=$(cat '{counter}'); fi
count=$((count + 1))
printf '%s' "$count" > '{counter}'
printf '%s\n' "$@" > '{calls}/call-'"$count"
for arg in "$@"; do
  if [ "$arg" = "install" ]; then {fail}; fi
done
exit 0
"#,
        counter = counter.display(),
        calls = calls_dir.display(),
        fail = fail,
    );
    write_executable(&script, &content);
    FakeNpm { script, calls_dir }
}

#[cfg(unix)]
struct NpmInstall {
    prefix: PathBuf,
    package_dir: PathBuf,
    fake: FakeNpm,
}

#[cfg(unix)]
fn prepare_npm_install(
    workspace: &Workspace,
    package_name: &str,
    fail_on_install: bool,
) -> NpmInstall {
    let prefix = workspace._temp.path().join("global-prefix");
    let mut package_dir = prefix.join("lib").join("node_modules");
    for part in package_name.split('/') {
        package_dir = package_dir.join(part);
    }
    std::fs::create_dir_all(&package_dir).expect("package dir");
    let fake = write_fake_npm(workspace._temp.path(), fail_on_install);
    NpmInstall {
        prefix,
        package_dir,
        fake,
    }
}

/// TS: "keeps npm self-updates non-managed when the managed environment is inherited"。
#[cfg(unix)]
#[tokio::test]
async fn keeps_npm_self_updates_non_managed_when_the_managed_environment_is_inherited() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let install = prepare_npm_install(&workspace, PACKAGE_NAME, false);
    let project_prefix = workspace._temp.path().join("project-prefix");

    // 继承来的 KIANA_MANAGED_INSTALL_ROOT 指向的 releases 不含当前包目录 → 不是 managed 安装。
    let inherited_root = workspace._temp.path().join("inherited-managed-install");
    std::fs::create_dir_all(inherited_root.join("releases")).expect("releases");
    std::fs::write(
        inherited_root.join("managed-install.json"),
        serde_json::json!({
            "kind": "kiana-managed-install",
            "schemaVersion": 1,
            "layout": "releases-v1",
        })
        .to_string(),
    )
    .expect("marker");
    env.set("KIANA_MANAGED_INSTALL_ROOT", &inherited_root);
    env.set("KIANA_PACKAGE_DIR", &install.package_dir);

    workspace.write_global_settings(
        &serde_json::json!({ "npmCommand": install.fake.command(&install.prefix) }),
    );
    workspace.write_project_settings(&serde_json::json!({
        "npmCommand": [install.fake.script.to_string_lossy().into_owned(), "--prefix", project_prefix.to_string_lossy().into_owned()],
    }));

    let server = MockServer::start().await;
    mount_latest_version(&server, VERSION).await;
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));

    let (outcome, stdout, stderr) = run(&workspace, &["update", "--self", "--force"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains(&format!("Updated kiana from {VERSION} to {VERSION}")));
    let calls = install.fake.calls();
    assert_eq!(calls.len(), 1, "only the install step runs: {calls:?}");
    let recorded = &calls[0];
    assert!(
        recorded
            .iter()
            .any(|arg| arg == &install.prefix.to_string_lossy().into_owned())
    );
    assert!(recorded.contains(&format!("{PACKAGE_NAME}@{VERSION}")));
    assert!(!recorded.contains(&PACKAGE_NAME.to_string()));
    assert!(!recorded.contains(&project_prefix.to_string_lossy().into_owned()));
}

/// TS: "uses the current package name when the update check omits packageName"。
#[cfg(unix)]
#[tokio::test]
async fn uses_the_current_package_name_when_the_update_check_omits_package_name() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let install = prepare_npm_install(&workspace, PACKAGE_NAME, false);
    env.remove("KIANA_MANAGED_INSTALL_ROOT");
    env.set("KIANA_PACKAGE_DIR", &install.package_dir);
    workspace.write_global_settings(
        &serde_json::json!({ "npmCommand": install.fake.command(&install.prefix) }),
    );
    let target_version = newer_patch_version();

    let server = MockServer::start().await;
    mount_latest_version(&server, &target_version).await;
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));

    let (outcome, stdout, stderr) = run(&workspace, &["update", "--self"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains(&format!("Updated kiana from {VERSION} to {target_version}")));
    let calls = install.fake.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].contains(&format!("{PACKAGE_NAME}@{target_version}")));
    assert!(!calls[0].contains(&PACKAGE_NAME.to_string()));
}

/// TS: "installs the active package name from the update check during self-update"。
#[cfg(unix)]
#[tokio::test]
async fn installs_the_active_package_name_from_the_update_check_during_self_update() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let install = prepare_npm_install(&workspace, PACKAGE_NAME, false);
    env.remove("KIANA_MANAGED_INSTALL_ROOT");
    env.set("KIANA_PACKAGE_DIR", &install.package_dir);
    workspace.write_global_settings(
        &serde_json::json!({ "npmCommand": install.fake.command(&install.prefix) }),
    );
    let active_package_name = if PACKAGE_NAME == "@new-scope/kiana" {
        "@newer-scope/kiana"
    } else {
        "@new-scope/kiana"
    };

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/latest-version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "packageName": active_package_name,
            "version": "0.73.0",
        })))
        .mount(&server)
        .await;
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));

    let (outcome, _, stderr) = run(&workspace, &["update", "--self"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 0, "stderr: {stderr}");
    let calls = install.fake.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].contains(&"uninstall".to_string()));
    assert!(calls[0].contains(&PACKAGE_NAME.to_string()));
    assert!(calls[1].contains(&"install".to_string()));
    assert!(calls[1].contains(&format!("{active_package_name}@0.73.0")));
}

/// TS: "fails self-update when renamed npm package installation fails"。
#[cfg(unix)]
#[tokio::test]
async fn fails_self_update_when_renamed_npm_package_installation_fails() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let install = prepare_npm_install(&workspace, PACKAGE_NAME, true);
    env.remove("KIANA_MANAGED_INSTALL_ROOT");
    env.set("KIANA_PACKAGE_DIR", &install.package_dir);
    workspace.write_global_settings(
        &serde_json::json!({ "npmCommand": install.fake.command(&install.prefix) }),
    );
    let active_package_name = if PACKAGE_NAME == "@new-scope/kiana" {
        "@newer-scope/kiana"
    } else {
        "@new-scope/kiana"
    };

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/latest-version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "packageName": active_package_name,
            "version": "0.73.0",
        })))
        .mount(&server)
        .await;
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));

    let (outcome, stdout, stderr) = run(&workspace, &["update", "--self"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 1);
    assert!(!stdout.contains("Updated kiana"), "{stdout}");
    assert!(stderr.contains("exited with code 23"), "{stderr}");
    let calls = install.fake.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].contains(&"uninstall".to_string()));
    assert!(calls[1].contains(&format!("{active_package_name}@0.73.0")));
}

/// TS: "prints a pnpm metadata hint when self-update fails"。
#[cfg(unix)]
#[tokio::test]
async fn prints_a_pnpm_metadata_hint_when_self_update_fails() {
    let mut env = EnvGuard::new();
    let workspace = Workspace::new();
    let global_root = workspace
        ._temp
        .path()
        .join("pnpm")
        .join("global")
        .join("v11");
    let package_dir = global_root
        .join("node_modules")
        .join("@earendil-works")
        .join("kiana-coding-agent");
    std::fs::create_dir_all(&package_dir).expect("package dir");
    let bin_dir = workspace._temp.path().join("pnpm-bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    let pnpm_path = bin_dir.join("pnpm");
    write_executable(
        &pnpm_path,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"root\" ] && [ \"$2\" = \"-g\" ]; then\n  printf '%s\\n' '{}'\n  exit 0\nfi\nexit 23\n",
            global_root.display()
        ),
    );
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut combined = OsString::from(bin_dir.as_os_str());
    combined.push(":");
    combined.push(path);
    env.set("PATH", combined);
    env.remove("KIANA_MANAGED_INSTALL_ROOT");
    env.set("KIANA_PACKAGE_DIR", &package_dir);
    let target_version = newer_patch_version();

    let server = MockServer::start().await;
    mount_latest_version(&server, &target_version).await;
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));

    let (outcome, stdout, stderr) = run(&workspace, &["update", "--self"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 1);
    assert!(!stdout.contains("Updated kiana"), "{stdout}");
    assert!(stderr.contains("exited with code 23"), "{stderr}");
    assert!(
        stderr.contains("If pnpm reports missing package versions"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Run `pnpm store prune` and retry `kiana update --self`."),
        "{stderr}"
    );
}

/// TS: "retries a transient self-update version check"。
#[tokio::test]
async fn retries_a_transient_self_update_version_check() {
    struct TransientVersionCheck {
        calls: AtomicUsize,
        version: String,
    }

    impl Respond for TransientVersionCheck {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
            if attempt < 2 {
                ResponseTemplate::new(500)
            } else {
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "version": self.version }))
            }
        }
    }

    let mut env = EnvGuard::new();
    env.remove("KIANA_MANAGED_INSTALL_ROOT");
    env.remove("KIANA_SKIP_VERSION_CHECK");
    let workspace = Workspace::new();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/latest-version"))
        .respond_with(TransientVersionCheck {
            calls: AtomicUsize::new(0),
            version: VERSION.to_string(),
        })
        .mount(&server)
        .await;
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));

    let (outcome, stdout, stderr) = run(&workspace, &["update", "--self"]).await;
    let requests = server
        .received_requests()
        .await
        .map(|requests| requests.len());
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert_eq!(requests, Some(3), "two failures then success");
    assert!(
        stdout.contains(&format!("kiana is already up to date (v{VERSION})")),
        "{stdout}"
    );
}

/// `printSelfUpdateUnavailable` 的入口行与 Node 的 `process.argv[1]` 等价物（原生二进制为
/// 可执行文件自身，即 `current_exe()`）。
#[tokio::test]
async fn reports_self_update_unavailable_with_the_executable_location() {
    let mut env = EnvGuard::new();
    env.remove("KIANA_MANAGED_INSTALL_ROOT");
    let workspace = Workspace::new();
    // 不在 node_modules/npm/pnpm 路径下 → 安装方式未知 → 无法自更新。
    env.set("KIANA_PACKAGE_DIR", workspace._temp.path());
    let target_version = newer_patch_version();
    let server = MockServer::start().await;
    mount_latest_version(&server, &target_version).await;
    kiana_core::changelog::set_latest_version_url_for_tests(Some(format!(
        "{}/api/latest-version",
        server.uri()
    )));

    let (outcome, _, stderr) = run(&workspace, &["update", "--self"]).await;
    kiana_core::changelog::set_latest_version_url_for_tests(None);

    assert_eq!(outcome.exit_code, 1);
    assert!(
        stderr.contains("error: kiana cannot self-update this installation."),
        "{stderr}"
    );
    let entrypoint = std::env::current_exe()
        .ok()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty());
    if let Some(entrypoint) = entrypoint {
        assert!(
            stderr.contains(&format!("Location of kiana executable: {entrypoint}")),
            "{stderr}"
        );
    }
}
