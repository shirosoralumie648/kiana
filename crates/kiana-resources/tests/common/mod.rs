//! 包管理集成测试的共享夹具：PATH 注入的假 npm/git、环境变量锁、settings 构造。
//!
//! 所有测试共享进程环境（`PATH` / `HOME` / `KIANA_OFFLINE` / `KIANA_FAKE_*`），因此每个用到
//! 环境的测试都必须持有 [`TestEnv`]（内部持全局 `ENV_LOCK`），同一测试二进制内串行执行。
//! 不同测试文件是独立进程，互不干扰。
//!
//! 假可执行文件见 `tests/fixtures/package/fake-cli.sh`；它把每次调用的 argv 追加到
//! `$KIANA_FAKE_LOG`，测试逐项断言。

#![allow(dead_code)]

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use kiana_config::settings::SettingsManager;
use kiana_config::settings::schema::{PackageSource, Settings};
use kiana_resources::package::{DefaultPackageManager, PackageManagerOptions, ResolvedPaths};

/// 串行化改动进程环境的测试。
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 一次假 CLI 调用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// 可执行文件名（`npm` / `git` / `mise` / ...）。
    pub program: String,
    /// 工作目录。
    pub cwd: String,
    /// argv（不含程序名）。
    pub args: Vec<String>,
}

impl Invocation {
    /// argv 的完整字符串（断言错误文案时用）。
    pub fn command_line(&self) -> String {
        format!("{} {}", self.program, self.args.join(" "))
    }
}

/// 测试环境守卫：持有期间独占进程环境，drop 时恢复。
pub struct TestEnv {
    _guard: MutexGuard<'static, ()>,
    /// 临时根目录（`temp.path()` 是 cwd）。
    pub temp: tempfile::TempDir,
    /// `<temp>/agent`。
    pub agent_dir: String,
    /// `<temp>/home`（`$HOME`）。
    pub home_dir: String,
    /// `<temp>/bin`（PATH 首位）。
    pub bin_dir: String,
    /// argv 日志路径。
    pub log_path: String,
    saved: RefCell<Vec<(String, Option<OsString>)>>,
}

impl TestEnv {
    /// 创建临时目录、PATH 注入的假可执行文件与干净的环境。
    pub fn new() -> Self {
        Self::build(true)
    }

    /// 创建临时目录与干净的环境，但保留真实的 PATH（给需要真实 `git` 的测试用）。
    pub fn new_with_real_binaries() -> Self {
        Self::build(false)
    }

    fn build(inject_fake_path: bool) -> Self {
        let guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let agent_dir = temp.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        let home_dir = temp.path().join("home");
        std::fs::create_dir_all(&home_dir).expect("home dir");
        let bin_dir = temp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).expect("bin dir");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("package")
            .join("fake-cli.sh");
        if inject_fake_path {
            for name in ["npm", "pnpm", "bun", "corepack", "mise", "git"] {
                let link = bin_dir.join(name);
                #[cfg(unix)]
                std::os::unix::fs::symlink(&fixture, &link).expect("fixture symlink");
                #[cfg(not(unix))]
                {
                    let _ = (name, &link);
                    panic!("package-manager tests require a Unix PATH-injected fixture");
                }
            }
        }
        let log_path = temp.path().join("argv.log").to_string_lossy().into_owned();

        let saved = RefCell::new(Vec::new());
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        if inject_fake_path {
            let new_path = format!(
                "{}:{}",
                bin_dir.to_string_lossy(),
                old_path.to_string_lossy()
            );
            Self::set_var(&saved, "PATH", Some(new_path.into()));
        }
        Self::set_var(&saved, "HOME", Some(home_dir.clone().into_os_string()));
        Self::set_var(&saved, "KIANA_OFFLINE", None);
        Self::set_var(&saved, "KIANA_FAKE_LOG", Some(log_path.clone().into()));
        Self::set_var(&saved, "KIANA_FAKE_SLEEP_SECONDS", None);
        for key in FAKE_ENV_KEYS {
            Self::set_var(&saved, key, None);
        }

        Self {
            _guard: guard,
            temp,
            agent_dir: agent_dir.to_string_lossy().into_owned(),
            home_dir: home_dir.to_string_lossy().into_owned(),
            bin_dir: bin_dir.to_string_lossy().into_owned(),
            log_path,
            saved,
        }
    }

    fn set_var(
        saved: &RefCell<Vec<(String, Option<OsString>)>>,
        key: &str,
        value: Option<OsString>,
    ) {
        {
            let mut saved = saved.borrow_mut();
            if !saved.iter().any(|(existing, _)| existing == key) {
                saved.push((key.to_string(), std::env::var_os(key)));
            }
        }
        match value {
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    /// 设置环境变量（drop 时恢复）。
    pub fn set_env(&self, key: &str, value: &str) {
        Self::set_var(&self.saved, key, Some(OsString::from(value)));
    }

    /// 删除环境变量（drop 时恢复）。
    pub fn unset_env(&self, key: &str) {
        Self::set_var(&self.saved, key, None);
    }

    /// 打开离线模式。
    pub fn set_offline(&self, offline: bool) {
        if offline {
            self.set_env("KIANA_OFFLINE", "1");
        } else {
            self.unset_env("KIANA_OFFLINE");
        }
    }

    /// 清空 argv 日志。
    pub fn clear_log(&self) {
        let _ = std::fs::remove_file(&self.log_path);
    }

    /// 解析全部假 CLI 调用。
    pub fn invocations(&self) -> Vec<Invocation> {
        let Ok(content) = std::fs::read_to_string(&self.log_path) else {
            return Vec::new();
        };
        let mut invocations = Vec::new();
        let mut current: Option<Invocation> = None;
        for line in content.lines() {
            if let Some(program) = line.strip_prefix("CMD ") {
                current = Some(Invocation {
                    program: program.to_string(),
                    cwd: String::new(),
                    args: Vec::new(),
                });
            } else if let Some(cwd) = line.strip_prefix("CWD ") {
                if let Some(current) = current.as_mut() {
                    current.cwd = cwd.to_string();
                }
            } else if let Some(argument) = line.strip_prefix("ARG ") {
                if let Some(current) = current.as_mut() {
                    current.args.push(argument.to_string());
                }
            } else if line == "END"
                && let Some(current) = current.take()
            {
                invocations.push(current);
            }
        }
        invocations
    }

    /// 所有指定程序的调用。
    pub fn calls(&self, program: &str) -> Vec<Invocation> {
        self.invocations()
            .into_iter()
            .filter(|invocation| invocation.program == program)
            .collect()
    }

    /// 第一次指定程序的调用。
    pub fn first_call(&self, program: &str) -> Option<Invocation> {
        self.calls(program).into_iter().next()
    }

    /// 最后一次指定程序的调用。
    pub fn last_call(&self, program: &str) -> Option<Invocation> {
        self.calls(program).into_iter().last()
    }

    /// 临时目录（绝对路径）。
    pub fn root(&self) -> String {
        self.temp.path().to_string_lossy().into_owned()
    }

    /// 构造 `SettingsManager::in_memory(settings)`（默认信任项目）。
    pub fn settings_manager(&self, settings: Settings) -> SettingsManager {
        SettingsManager::in_memory(settings)
    }

    /// 构造包管理器；`cwd` 是测试临时目录。
    pub fn manager(&self, settings_manager: &SettingsManager) -> DefaultPackageManager {
        DefaultPackageManager::new(PackageManagerOptions {
            cwd: self.root(),
            agent_dir: self.agent_dir.clone(),
            settings_manager: settings_manager.clone(),
            builtin_extensions: Vec::new(),
        })
    }

    /// 构造包管理器并指定 cwd / builtinExtensions。
    pub fn manager_with(
        &self,
        cwd: &str,
        settings_manager: &SettingsManager,
        builtin_extensions: Vec<String>,
    ) -> DefaultPackageManager {
        DefaultPackageManager::new(PackageManagerOptions {
            cwd: cwd.to_string(),
            agent_dir: self.agent_dir.clone(),
            settings_manager: settings_manager.clone(),
            builtin_extensions,
        })
    }

    /// 写入文件（自动创建父目录）。
    pub fn write(&self, path: impl AsRef<Path>, content: &str) {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, content).expect("write");
    }

    /// `Settings { packages: Some(...), .. }`。
    pub fn settings_with_packages(&self, packages: Vec<PackageSource>) -> Settings {
        Settings {
            packages: Some(packages),
            ..Settings::default()
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        for (key, value) in self.saved.get_mut().drain(..).rev() {
            match value {
                Some(value) => unsafe { std::env::set_var(&key, value) },
                None => unsafe { std::env::remove_var(&key) },
            }
        }
    }
}

/// 假 CLI 读取的全部环境变量（每个测试开始前清空）。
pub const FAKE_ENV_KEYS: &[&str] = &[
    "KIANA_FAKE_SLEEP_SECONDS",
    "KIANA_FAKE_FAIL_MATCH",
    "KIANA_FAKE_FAIL_STDERR",
    "KIANA_FAKE_EXIT",
    "KIANA_FAKE_VIEW_OUTPUT",
    "KIANA_FAKE_VIEW_FAIL",
    "KIANA_FAKE_VIEW_MAP",
    "KIANA_FAKE_NPM_ROOT",
    "KIANA_FAKE_PM_BIN",
    "KIANA_FAKE_MISE_ROOT_MAP",
    "KIANA_FAKE_PNPM_LIST",
    "KIANA_FAKE_INSTALL_PACKAGE_JSON",
    "KIANA_FAKE_INSTALL_FILE",
    "KIANA_FAKE_GIT_CLONE_FAIL",
    "KIANA_FAKE_GIT_CLONE_PACKAGE_JSON",
    "KIANA_FAKE_GIT_CLONE_FILE",
    "KIANA_FAKE_GIT_CLONE_FILE_CONTENT",
    "KIANA_FAKE_GIT_REV_PARSE_FAIL",
    "KIANA_FAKE_GIT_LOCAL_HEAD",
    "KIANA_FAKE_GIT_UPSTREAM",
    "KIANA_FAKE_GIT_UPSTREAM_HEAD",
    "KIANA_FAKE_GIT_TARGET_HEAD",
    "KIANA_FAKE_GIT_SYMBOLIC_REF",
    "KIANA_FAKE_GIT_LS_REMOTE",
    "KIANA_FAKE_GIT_CLEAN_FAIL",
    "KIANA_FAKE_GIT_CLEAN_WRITE_PATH",
    "KIANA_FAKE_GIT_CLEAN_WRITE_CONTENT",
    "KIANA_FAKE_GIT_RESET_WRITE_PATH",
    "KIANA_FAKE_GIT_RESET_WRITE_CONTENT",
];

/// `getTemporaryDir` 的独立复刻（测试需要预置 / 检查临时 checkout）。
pub fn temporary_git_dir(agent_dir: &str, host: &str, path: &str, r#ref: Option<&str>) -> String {
    let prefix = format!("git-{host}");
    let input = format!(
        "{prefix}-{path}{}",
        match r#ref {
            Some(value) => format!("@{value}"),
            None => String::new(),
        }
    );
    let hash = kiana_ai::model_catalog::sha256_hex(&input);
    Path::new(agent_dir)
        .join("tmp/extensions")
        .join(&prefix)
        .join(&hash[..8])
        .join(path)
        .to_string_lossy()
        .into_owned()
}

/// 路径以 `suffix` 结尾且已启用。
pub fn is_enabled(resources: &[kiana_resources::package::ResolvedResource], suffix: &str) -> bool {
    resources
        .iter()
        .any(|resource| resource.path.ends_with(suffix) && resource.enabled)
}

/// 路径以 `suffix` 结尾且被禁用。
pub fn is_disabled(resources: &[kiana_resources::package::ResolvedResource], suffix: &str) -> bool {
    resources
        .iter()
        .any(|resource| resource.path.ends_with(suffix) && !resource.enabled)
}

/// 路径包含 `needle` 且已启用。
pub fn is_enabled_includes(
    resources: &[kiana_resources::package::ResolvedResource],
    needle: &str,
) -> bool {
    resources
        .iter()
        .any(|resource| resource.path.contains(needle) && resource.enabled)
}

/// 路径包含 `needle` 且被禁用。
pub fn is_disabled_includes(
    resources: &[kiana_resources::package::ResolvedResource],
    needle: &str,
) -> bool {
    resources
        .iter()
        .any(|resource| resource.path.contains(needle) && !resource.enabled)
}

/// 路径规范化（Windows 反斜杠 → `/`）。
pub fn normalize_for_match(value: &str) -> String {
    value.replace('\\', "/")
}

/// 所有资源的路径集合。
pub fn all_paths(resolved: &ResolvedPaths) -> Vec<String> {
    resolved
        .extensions
        .iter()
        .chain(resolved.skills.iter())
        .chain(resolved.prompts.iter())
        .chain(resolved.themes.iter())
        .map(|resource| resource.path.clone())
        .collect()
}
