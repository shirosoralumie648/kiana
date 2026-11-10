//! `npmCommand` / 安装参数 / `get_latest_npm_version` / 全局根。
//!
//! TS: `package-manager.ts` 的 `getNpmCommand` / `getPackageManagerName` /
//! `getGitDependencyInstallArgs` / `getNpmInstallArgs` / `installNpm` / `uninstallNpm` /
//! `runNpmCommand` / `getGlobalNpmRoot` / `getPnpmGlobalPackagePath` /
//! `getLegacyGlobalNpmInstallPath` / `getLatestNpmVersion` / `installNpmBatch`。
//!
//! argv 形态必须逐字（`spec/ca-resources.md` §2.10.19）：例如
//! `npm install <spec> --prefix <realpath(root)> --legacy-peer-deps`。
//!
//! # 这个文件负责什么
//!
//! 它把 settings 里用户配置的 `npmCommand`（一个 argv 数组，可能是 `npm`、
//! `pnpm`、`bun`，也可能是 `mise exec node@20 -- npm` 这类包装命令）解析成
//! `NpmCommand`，再据此构造各包管理器的安装/卸载参数并执行子进程。输入是
//! settings 快照与 `NpmSource`/spec 列表；输出是 `Result` 型的执行结果或探测值
//! （最新版本号、全局根路径）。git checkout 内依赖的安装参数也由这里提供
//! （`get_git_dependency_install_args`，被 `git` 子模块调用）。
//!
//! # 调用关系
//!
//! - `mod.rs` 的 `PackageManager::install` / `remove` / `update_npm_batch` 调用
//!   `install_npm` / `uninstall_npm` / `install_npm_batch`；`mod.rs` 的
//!   `npm_has_available_update` 调用 `get_latest_npm_version` 探测 registry 目标版本。
//! - `paths.rs` 的 `get_npm_install_path` 在 user 作用域回退时调用
//!   `get_legacy_global_npm_install_path`（会执行 `npm root -g` / `npm list -g` 子进程）。
//! - 本文件调用 `exec` 的 `run_command` / `run_command_capture` / `run_command_sync`
//!   执行子进程，调用 `parse` 的 `max_satisfying` / `highest_version` 比较版本。
//!
//! # 关键约束与失败模式
//!
//! - `npmCommand` 的解析逐字复刻 TS 的两步语义（见 `npm_command_from_values` 文档），
//!   包括字符串按码点展开、falsy 回退默认 npm、不可迭代值抛 V8 同款 TypeError 文案。
//!   错误文案是字节级契约的一部分，调用方（如交互式 UI）依赖它们与 TS 行为一致。
//! - `get_global_npm_root` 的结果缓存在 `DefaultPackageManager` 内的 `Mutex` 里，
//!   缓存 key 是当前 npmCommand；npmCommand 变化或结果为空都不命中缓存。
//! - `run_command_sync`（`npm root -g`、`npm list -g`）没有超时机制；`get_latest_npm_version`
//!   的 `npm view` 使用 10 秒超时（`NETWORK_TIMEOUT_MS`）。
//!
//! # 阅读顺序
//!
//! 先看 `npm_command_from_values` 与 `get_npm_command` 理解配置如何变成 argv，再看
//! `get_package_manager_name` 理解包装命令如何识别出真正的包管理器，最后看各
//! install/uninstall/查询方法。

use indexmap::IndexSet;

use super::exec::{
    CaptureOptions, NETWORK_TIMEOUT_MS, js_trim, run_command_capture, run_command_sync,
};
use super::parse::{NpmRange, NpmSource, highest_version, max_satisfying};
use super::paths::{basename, dirname, join, path_exists};
use super::types::SourceScope;
use super::{DefaultPackageManager, PackageManagerError, err};

/// 规范化后的 npm 命令（settings 的 `npmCommand` argv 拆成 command + 其余参数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NpmCommand {
    /// 可执行文件。
    pub command: String,
    /// 其余 argv（含 `--` 之前的包装参数）。
    pub args: Vec<String>,
}

/// 默认 `npm` 命令（TS 的 `{ command: "npm", args: [] }`）。
fn default_npm_command() -> NpmCommand {
    NpmCommand {
        command: "npm".to_string(),
        args: Vec::new(),
    }
}

/// 把 settings 的 `npmCommand` 值转成 command + args。
///
/// 对应 TS 的两步：
/// 1. `SettingsManager.getNpmCommand()`：`this.settings.npmCommand ? [...value] : undefined`
///    （字符串按码点展开，数组浅拷贝；falsy 值为 `undefined`；其它不可迭代的值抛
///    `TypeError: this.settings.npmCommand is not iterable`）。
/// 2. `DefaultPackageManager.getNpmCommand()`：`const [command, ...args] = configuredCommand`
///    （空数组 → 默认 npm；`!command` → `Invalid npmCommand: ...`）。
///
/// 数组里的非字符串元素：TS 解构后原样保留，`args` 在 `spawnSync` 时按 `String(value)`
/// 强制转换；`command` 非字符串时 Node 的 spawn 抛 `The "file" argument must be of
/// type string. ...`（Rust 的 `NpmCommand` 只能表示字符串，故在此提前抛出同一文案）。
fn npm_command_from_values(
    configured: Vec<serde_json::Value>,
) -> Result<NpmCommand, PackageManagerError> {
    if configured.is_empty() {
        return Ok(default_npm_command());
    }
    let mut iter = configured.into_iter();
    let command = iter.next().unwrap_or(serde_json::Value::Null);
    if !kiana_json::js_string::is_truthy(Some(&command)) {
        return Err(err(
            "Invalid npmCommand: first array entry must be a non-empty command",
        ));
    }
    let command = match command {
        serde_json::Value::String(command) => command,
        other => return Err(err(node_spawn_file_type_error(&other))),
    };
    Ok(NpmCommand {
        command,
        // Node 的 `spawnSync` 对 argv 逐项做 `String(value)`。
        args: iter
            .map(|argument| kiana_json::js_string::js_string(Some(&argument)))
            .collect(),
    })
}

/// Node `child_process.spawnSync` 的 `file` 参数 TypeError 文案。
fn node_spawn_file_type_error(value: &serde_json::Value) -> String {
    const PREFIX: &str = "The \"file\" argument must be of type string.";
    match value {
        serde_json::Value::Null => format!("{PREFIX} Received null"),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => format!(
            "{PREFIX} Received type {} ({})",
            kiana_json::js_string::js_typeof(value).as_str(),
            kiana_json::js_string::js_string(Some(value))
        ),
        serde_json::Value::Array(_) => format!("{PREFIX} Received an instance of Array"),
        serde_json::Value::Object(_) => format!("{PREFIX} Received an instance of Object"),
        // 字符串不会走到这里（调用方已先匹配）。
        serde_json::Value::String(text) => format!("{PREFIX} Received {text}"),
    }
}

/// `basename(command).replace(/\.(cmd|exe)$/i, "")`。
fn normalize_command_name(command: &str) -> String {
    let base = basename(command);
    let lower = base.to_ascii_lowercase();
    if lower.ends_with(".cmd") || lower.ends_with(".exe") {
        base[..base.len() - 4].to_string()
    } else {
        base
    }
}

impl DefaultPackageManager {
    /// `getNpmCommand()`。
    ///
    /// TS 直接使用 settings 里的原始值（`SettingsManager.getNpmCommand()` 的
    /// `this.settings.npmCommand ? [...value] : undefined`）：字符串会被解构为单个字符，
    /// `null`/缺失/空字符串/`false`/`0` 回退到 `["npm"]`；布尔/数字/对象不可迭代，
    /// 展开时抛 TypeError。
    pub(crate) fn get_npm_command(&self) -> Result<NpmCommand, PackageManagerError> {
        // TS 的 `SettingsManager.getNpmCommand()` 读取合并后的有效设置（global + 已信任的
        // 项目设置），不是仅 global。
        let settings = self.settings_manager.get_settings();
        if let Some(configured) = settings.npm_command.as_ref() {
            return npm_command_from_values(
                configured
                    .iter()
                    .map(|value| serde_json::Value::String(value.clone()))
                    .collect(),
            );
        }
        match settings.raw("npmCommand") {
            // `null` / 缺失 / falsy（空字符串、false、0）：`!configuredCommand` → 默认 npm。
            None => Ok(default_npm_command()),
            Some(value) if !kiana_json::js_string::is_truthy(Some(&value)) => {
                Ok(default_npm_command())
            }
            Some(serde_json::Value::String(text)) => npm_command_from_values(
                text.chars()
                    .map(|character| serde_json::Value::String(character.to_string()))
                    .collect(),
            ),
            Some(serde_json::Value::Array(items)) => npm_command_from_values(items),
            // 布尔/数字/对象没有迭代器：TS 的 `[...this.settings.npmCommand]` 会抛
            // `TypeError: this.settings.npmCommand is not iterable`。
            Some(_) => Err(err("this.settings.npmCommand is not iterable")),
        }
    }

    /// `getPackageManagerName()`：`npm` / `pnpm` / `bun`，或包装命令里的最后一个。
    ///
    /// 判定顺序固定：
    /// 1. args 中存在 `--` 分隔符时，取其后第一个参数作为被包装的包管理器（`--` 之后
    ///    为空则退回直接命令）；
    /// 2. 直接命令（去掉路径与 `.cmd`/`.exe` 后缀）本身是受支持的包管理器时直接返回；
    /// 3. 否则扫描 args，收集出现的受支持包管理器（去重、保持首次出现顺序）：出现多于
    ///    一种时报 `Ambiguous npmCommand package managers: ...`，恰好一种时返回它，
    ///    没有则返回直接命令。
    pub(crate) fn get_package_manager_name(&self) -> Result<String, PackageManagerError> {
        let npm_command = self.get_npm_command()?;
        let supported = ["npm", "pnpm", "bun"];
        let direct_command = normalize_command_name(&npm_command.command);
        let separator_index = npm_command.args.iter().rposition(|arg| arg == "--");
        if let Some(separator_index) = separator_index {
            let wrapped_command = npm_command.args.get(separator_index + 1);
            return Ok(match wrapped_command {
                Some(wrapped) if !wrapped.is_empty() => normalize_command_name(wrapped),
                _ => direct_command,
            });
        }
        if supported.contains(&direct_command.as_str()) {
            return Ok(direct_command);
        }

        let mut wrapped_package_managers: IndexSet<String> = IndexSet::new();
        for argument in &npm_command.args {
            let normalized = normalize_command_name(argument);
            if supported.contains(&normalized.as_str()) {
                wrapped_package_managers.insert(normalized);
            }
        }
        if wrapped_package_managers.len() > 1 {
            let joined: Vec<&str> = wrapped_package_managers
                .iter()
                .map(String::as_str)
                .collect();
            return Err(err(format!(
                "Ambiguous npmCommand package managers: {}",
                joined.join(", ")
            )));
        }
        Ok(wrapped_package_managers
            .into_iter()
            .next()
            .unwrap_or(direct_command))
    }

    /// `getGitDependencyInstallArgs()`。
    pub(crate) fn get_git_dependency_install_args(
        &self,
    ) -> Result<Vec<String>, PackageManagerError> {
        Ok(match self.get_package_manager_name()?.as_str() {
            "bun" => vec![
                "install".to_string(),
                "--omit=dev".to_string(),
                "--omit=peer".to_string(),
            ],
            "pnpm" => vec![
                "install".to_string(),
                "--prod".to_string(),
                "--config.auto-install-peers=false".to_string(),
                "--config.strict-peer-dependencies=false".to_string(),
                "--config.strict-dep-builds=false".to_string(),
            ],
            "npm" => vec![
                "install".to_string(),
                "--omit=dev".to_string(),
                "--legacy-peer-deps".to_string(),
            ],
            _ => vec!["install".to_string()],
        })
    }

    /// `getNpmInstallArgs(specs, root)`。
    ///
    /// 构造一次 `install <specs...>` 的完整 argv（不含 `npmCommand` 自身的包装参数，那由
    /// `run_npm_command` 拼接）。`root` 会先做存在性检查再 canonicalize（TS 逐字：对任何
    /// 包管理器都如此）——npm 在 `--prefix` 指向符号链接时会写出损坏的嵌套
    /// package-lock.json 路径，因此已存在的目录必须取真实路径；不存在的路径（首次安装前）
    /// 保持原样。bun 用 `--cwd` 且只加 `--omit=peer`；npm 追加 `--legacy-peer-deps`；pnpm
    /// 用一组 `--config.*` 显式关闭 peer 交互与构建严格检查。
    pub(crate) fn get_npm_install_args(
        &self,
        specs: &[String],
        root: &str,
    ) -> Result<Vec<String>, PackageManagerError> {
        // npm 在 `--prefix` 经过符号链接时会写出损坏的嵌套 package-lock.json 路径。
        let install_root = if path_exists(root) {
            kiana_core::paths::canonicalize_path(root)
        } else {
            root.to_string()
        };
        let package_manager_name = self.get_package_manager_name()?;
        if package_manager_name == "bun" {
            let mut args = vec!["install".to_string()];
            args.extend(specs.iter().cloned());
            args.push("--cwd".to_string());
            args.push(install_root);
            args.push("--omit=peer".to_string());
            return Ok(args);
        }
        if package_manager_name == "pnpm" {
            let mut args = vec!["install".to_string()];
            args.extend(specs.iter().cloned());
            args.push("--prefix".to_string());
            args.push(install_root);
            args.push("--config.auto-install-peers=false".to_string());
            args.push("--config.strict-peer-dependencies=false".to_string());
            args.push("--config.strict-dep-builds=false".to_string());
            return Ok(args);
        }
        let mut args = vec!["install".to_string()];
        args.extend(specs.iter().cloned());
        args.push("--prefix".to_string());
        args.push(install_root);
        args.push("--legacy-peer-deps".to_string());
        Ok(args)
    }

    /// `runNpmCommand(args, options?)`。
    pub(crate) async fn run_npm_command(
        &self,
        args: &[String],
        cwd: Option<&str>,
    ) -> Result<(), PackageManagerError> {
        let npm_command = self.get_npm_command()?;
        let mut full_args = npm_command.args.clone();
        full_args.extend(args.iter().cloned());
        super::exec::run_command(&npm_command.command, &full_args, cwd).await
    }

    /// `runNpmCommandSync(args)`。
    pub(crate) fn run_npm_command_sync(
        &self,
        args: &[String],
    ) -> Result<String, PackageManagerError> {
        let npm_command = self.get_npm_command()?;
        let mut full_args = npm_command.args.clone();
        full_args.extend(args.iter().cloned());
        run_command_sync(&npm_command.command, &full_args)
    }

    /// `installNpm(source, scope, temporary)`。
    ///
    /// 安装单个 npm 源：先按作用域解析安装根（`temporary` 走 `get_temporary_dir`），创建
    /// 项目骨架（`ensure_npm_project`：目录、云同步忽略标记、`.gitignore`、最小
    /// `package.json`），再以该 spec 执行一次安装。npm 命令不带 cwd 运行——安装位置完全
    /// 由 argv 里的 `--prefix` / `--cwd` 决定。
    pub(crate) async fn install_npm(
        &self,
        source: &NpmSource,
        scope: SourceScope,
        temporary: bool,
    ) -> Result<(), PackageManagerError> {
        let install_root = self.get_npm_install_root(scope, temporary)?;
        self.ensure_npm_project(&install_root)?;
        let args = self.get_npm_install_args(std::slice::from_ref(&source.spec), &install_root)?;
        self.run_npm_command(&args, None).await
    }

    /// `uninstallNpm(source, scope)`。
    ///
    /// 从 user/project 作用域的 npm 根卸载一个包。安装根不存在时直接成功（幂等）；
    /// 存在时先 canonicalize 再拼 `uninstall <name>`。pnpm 不追加 `--legacy-peer-deps`
    /// （pnpm 不认识该 flag），bun 走 `--cwd` 形式。
    pub(crate) async fn uninstall_npm(
        &self,
        source: &NpmSource,
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        let root = self.get_npm_install_root(scope, false)?;
        if !path_exists(&root) {
            return Ok(());
        }
        let install_root = kiana_core::paths::canonicalize_path(&root);
        let package_manager_name = self.get_package_manager_name()?;
        if package_manager_name == "bun" {
            return self
                .run_npm_command(
                    &[
                        "uninstall".to_string(),
                        source.name.clone(),
                        "--cwd".to_string(),
                        install_root,
                    ],
                    None,
                )
                .await;
        }
        let mut args = vec![
            "uninstall".to_string(),
            source.name.clone(),
            "--prefix".to_string(),
            install_root,
        ];
        if package_manager_name != "pnpm" {
            args.push("--legacy-peer-deps".to_string());
        }
        self.run_npm_command(&args, None).await
    }

    /// `installNpmBatch(specs, scope)`。
    ///
    /// 在同一条 npm 命令里安装多个 spec（来自 `update_npm_batch`），只做一次
    /// `ensure_npm_project` 与一次子进程调用；任一 spec 失败时整条命令失败。总是使用
    /// 非 temporary 安装根（批量更新只发生在 user/project 作用域）。
    pub(crate) async fn install_npm_batch(
        &self,
        specs: &[String],
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        let install_root = self.get_npm_install_root(scope, false)?;
        self.ensure_npm_project(&install_root)?;
        let args = self.get_npm_install_args(specs, &install_root)?;
        self.run_npm_command(&args, None).await
    }

    /// `getGlobalNpmRoot()`（带 npmCommand 缓存）。
    ///
    /// 返回全局 node_modules 根目录。bun 没有 `npm root -g`，改用 `bun pm bin -g` 再拼
    /// `<bin 目录父目录>/install/global/node_modules`。查询结果按当前 npmCommand 缓存：
    /// key 是 `command + args` 用 `\0` 连接（避免参数内空格造成歧义），key 变化即失效。
    /// 空字符串结果不缓存（TS 的 `if (this.globalNpmRoot && ...)` 把空串当 falsy），下次
    /// 调用会重新执行子进程；测试 `empty_global_npm_root_is_not_cached` 锁定该行为。
    ///
    /// 执行路径是同步子进程（`run_npm_command_sync`），且可能被
    /// `get_npm_install_path` 的回退逻辑在解析资源路径时触发。
    pub(crate) fn get_global_npm_root(&self) -> Result<String, PackageManagerError> {
        let npm_command = self.get_npm_command()?;
        let command_key = std::iter::once(npm_command.command.as_str())
            .chain(npm_command.args.iter().map(String::as_str))
            .collect::<Vec<&str>>()
            .join("\0");
        {
            let cache = lock(self.npm_root_cache());
            if let Some(root) = &cache.root
                && cache.command_key.as_deref() == Some(command_key.as_str())
            {
                return Ok(root.clone());
            }
        }
        let root = if self.get_package_manager_name()? == "bun" {
            let bin_dir = js_trim(&self.run_npm_command_sync(&[
                "pm".to_string(),
                "bin".to_string(),
                "-g".to_string(),
            ])?)
            .to_string();
            join(&[&dirname(&bin_dir), "install", "global", "node_modules"])
        } else {
            js_trim(&self.run_npm_command_sync(&["root".to_string(), "-g".to_string()])?)
                .to_string()
        };
        {
            let mut cache = lock(self.npm_root_cache());
            // TS 的缓存守卫是 `if (this.globalNpmRoot && ...)`：空字符串是 falsy，
            // 因此空结果不会被缓存（下次调用会重新执行 `npm root -g`）。
            cache.root = if root.is_empty() {
                None
            } else {
                Some(root.clone())
            };
            cache.command_key = Some(command_key);
        }
        Ok(root)
    }

    /// `getPnpmGlobalPackagePath(packageName)`。
    ///
    /// 仅当前包管理器是 pnpm 时有意义：执行 `pnpm list -g --depth 0 --json`，在输出的
    /// `dependencies.<name>.path` 里找全局安装位置；输出不是 JSON 数组、找不到该包或
    /// `path` 为空/非字符串时返回 `Ok(None)`。非 pnpm 配置直接返回 `Ok(None)`，不启动
    /// 子进程。`pnpm list` 失败（非零退出）时返回错误。
    pub(crate) fn get_pnpm_global_package_path(
        &self,
        package_name: &str,
    ) -> Result<Option<String>, PackageManagerError> {
        if self.get_package_manager_name()? != "pnpm" {
            return Ok(None);
        }
        let output = self.run_npm_command_sync(&[
            "list".to_string(),
            "-g".to_string(),
            "--depth".to_string(),
            "0".to_string(),
            "--json".to_string(),
        ])?;
        let parsed: serde_json::Value =
            serde_json::from_str(&output).map_err(|error| err(error.to_string()))?;
        let Some(entries) = parsed.as_array() else {
            return Ok(None);
        };
        for entry in entries {
            let Some(dependencies) = entry.get("dependencies") else {
                continue;
            };
            let Some(path) = dependencies
                .get(package_name)
                .and_then(|value| value.get("path"))
            else {
                continue;
            };
            if let Some(path) = path.as_str()
                && !path.is_empty()
            {
                return Ok(Some(path.to_string()));
            }
        }
        Ok(None)
    }

    /// `getLegacyGlobalNpmInstallPath(source)`：pnpm 全局路径优先，否则 `npm root -g`。
    ///
    /// 返回旧版（managed 布局之前）安装位置：pnpm 下是 `pnpm list -g` 报告的 `path`，
    /// 否则是 `<npm root -g>/<包名>`。任何一步失败（子进程错误、无法确定路径）都折叠成
    /// `None`；调用方（`get_npm_install_path` 的 user 作用域回退）把 `None` 解释为
    /// “继续用 managed 路径”。
    pub(crate) fn get_legacy_global_npm_install_path(&self, source: &NpmSource) -> Option<String> {
        match self.get_pnpm_global_package_path(&source.name) {
            Ok(Some(path)) => Some(path),
            Ok(None) => match self.get_global_npm_root() {
                Ok(root) => Some(join(&[&root, &source.name])),
                Err(_) => None,
            },
            Err(_) => None,
        }
    }

    /// `getLatestNpmVersion(packageSpec, range?)`。
    ///
    /// 用 `<npmCommand> view <spec> version --json` 查询 registry。响应有三种合法形态：
    /// JSON 字符串（单版本，直接返回）、JSON 数组（多版本：有 range 时用 `max_satisfying`
    /// 选最高满足版本，否则 `highest_version` 排序取最高）、其余形态一律报
    /// `Unexpected response from npm view`。空输出报 `Empty response from npm view`；
    /// `npm view` 本身失败（网络、非零退出）时错误原样上抛。探测使用 10 秒超时。
    pub(crate) async fn get_latest_npm_version(
        &self,
        package_spec: &str,
        range: Option<&NpmRange>,
    ) -> Result<String, PackageManagerError> {
        let npm_command = self.get_npm_command()?;
        let mut args = npm_command.args.clone();
        args.push("view".to_string());
        args.push(package_spec.to_string());
        args.push("version".to_string());
        args.push("--json".to_string());
        let stdout = run_command_capture(
            &npm_command.command,
            &args,
            &CaptureOptions {
                cwd: Some(self.cwd.clone()),
                timeout_ms: Some(NETWORK_TIMEOUT_MS),
                env: Vec::new(),
            },
        )
        .await?;
        let raw = js_trim(&stdout);
        if raw.is_empty() {
            return Err(err("Empty response from npm view"));
        }
        let parsed: serde_json::Value = match serde_json::from_str(raw) {
            Ok(value) => value,
            Err(error) => return Err(err(error.to_string())),
        };
        if let serde_json::Value::String(value) = &parsed {
            return Ok(value.clone());
        }
        if let serde_json::Value::Array(items) = &parsed {
            let versions: Vec<String> = items
                .iter()
                .filter_map(|item| item.as_str())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect();
            let latest = match range {
                Some(range) => max_satisfying(&versions, range),
                None => highest_version(&versions)?,
            };
            if let Some(latest) = latest {
                return Ok(latest);
            }
        }
        Err(err("Unexpected response from npm view"))
    }
}

/// `Mutex` 的锁获取（ poisoning 不应让包管理崩溃）。
///
/// `DefaultPackageManager` 的几把 `Mutex`（如全局 npm 根缓存）如果在一个持锁线程 panic
/// 过，标准库会把它标记为 poisoned，后续 `lock()` 返回 `Err`。这里选择取回内部数据继续
/// 用：缓存数据本身可以安全重建（最多多跑一次子进程），不值得为它崩溃整个包管理流程。
pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager_with_npm_command(command: Option<Vec<String>>) -> DefaultPackageManager {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = kiana_config::settings::Settings {
            npm_command: command,
            ..kiana_config::settings::Settings::default()
        };
        DefaultPackageManager::new(super::super::PackageManagerOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: dir.path().join("agent").to_string_lossy().into_owned(),
            settings_manager: kiana_config::settings::SettingsManager::in_memory(settings),
            builtin_extensions: vec![],
        })
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn package_manager_name_after_separator_wins() {
        let manager = manager_with_npm_command(Some(strings(&["npm", "exec", "--", "pnpm"])));
        assert_eq!(manager.get_package_manager_name().expect("name"), "pnpm");
        assert_eq!(
            manager.get_git_dependency_install_args().expect("args"),
            strings(&[
                "install",
                "--prod",
                "--config.auto-install-peers=false",
                "--config.strict-peer-dependencies=false",
                "--config.strict-dep-builds=false"
            ])
        );
    }

    #[test]
    fn corepack_wrapper_detects_pnpm_without_separator() {
        let manager = manager_with_npm_command(Some(strings(&["corepack", "pnpm"])));
        assert_eq!(manager.get_package_manager_name().expect("name"), "pnpm");
    }

    #[test]
    fn wrapped_npm_install_args_match_spec() {
        let manager =
            manager_with_npm_command(Some(strings(&["mise", "exec", "node@20", "--", "npm"])));
        let args = manager
            .get_npm_install_args(&strings(&["@scope/pkg"]), "/tmp/root")
            .expect("args");
        assert_eq!(
            args,
            strings(&[
                "install",
                "@scope/pkg",
                "--prefix",
                "/tmp/root",
                "--legacy-peer-deps"
            ])
        );
    }

    #[test]
    fn bun_install_args_use_cwd_and_omit_peer() {
        let manager =
            manager_with_npm_command(Some(strings(&["mise", "exec", "bun@1", "--", "bun"])));
        let args = manager
            .get_npm_install_args(&strings(&["@scope/pkg"]), "/tmp/root")
            .expect("args");
        assert_eq!(
            args,
            strings(&["install", "@scope/pkg", "--cwd", "/tmp/root", "--omit=peer"])
        );
    }

    #[tokio::test]
    async fn get_latest_npm_version_parses_json_strings_and_arrays() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = kiana_config::settings::Settings {
            npm_command: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf '%s' \"$KIANA_TEST_VIEW\"".to_string(),
            ]),
            ..kiana_config::settings::Settings::default()
        };
        let manager = DefaultPackageManager::new(super::super::PackageManagerOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: dir.path().join("agent").to_string_lossy().into_owned(),
            settings_manager: kiana_config::settings::SettingsManager::in_memory(settings),
            builtin_extensions: vec![],
        });

        // JSON string。
        unsafe {
            std::env::set_var("KIANA_TEST_VIEW", "\"1.2.3\"");
        }
        assert_eq!(
            manager
                .get_latest_npm_version("example", None)
                .await
                .expect("latest"),
            "1.2.3"
        );

        // JSON 数组 + range：maxSatisfying 选最高满足版本。
        unsafe {
            std::env::set_var("KIANA_TEST_VIEW", "[\"1.0.0\",\"1.2.0\",\"2.0.0\"]");
        }
        let range = super::super::parse::NpmRange::parse("^1.0.0").expect("range");
        assert_eq!(
            manager
                .get_latest_npm_version("example", Some(&range))
                .await
                .expect("latest"),
            "1.2.0"
        );
        // 无 range：排序取最高。
        assert_eq!(
            manager
                .get_latest_npm_version("example", None)
                .await
                .expect("latest"),
            "2.0.0"
        );

        // 空输出 / 非预期输出。
        unsafe {
            std::env::set_var("KIANA_TEST_VIEW", "");
        }
        assert_eq!(
            manager
                .get_latest_npm_version("example", None)
                .await
                .expect_err("empty")
                .message(),
            "Empty response from npm view"
        );
        unsafe {
            std::env::set_var("KIANA_TEST_VIEW", "42");
        }
        assert_eq!(
            manager
                .get_latest_npm_version("example", None)
                .await
                .expect_err("unexpected")
                .message(),
            "Unexpected response from npm view"
        );
        unsafe {
            std::env::remove_var("KIANA_TEST_VIEW");
        }
    }

    #[test]
    fn string_npm_command_is_spread_into_characters() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut settings = kiana_config::settings::Settings::default();
        settings
            .extra
            .insert("npmCommand".to_string(), serde_json::json!("pnpm"));
        let manager = DefaultPackageManager::new(super::super::PackageManagerOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: dir.path().join("agent").to_string_lossy().into_owned(),
            settings_manager: kiana_config::settings::SettingsManager::in_memory(settings),
            builtin_extensions: vec![],
        });
        let command = manager.get_npm_command().expect("command");
        assert_eq!(command.command, "p");
        assert_eq!(command.args, strings(&["n", "p", "m"]));
    }

    #[test]
    fn empty_global_npm_root_is_not_cached() {
        let dir = tempfile::tempdir().expect("tempdir");
        let counter = dir.path().join("counter");
        let script = format!(
            "count=$(cat '{}' 2>/dev/null || echo 0); count=$((count+1)); echo $count >'{}'; if [ $count -gt 1 ]; then echo /root2; fi",
            counter.to_string_lossy(),
            counter.to_string_lossy()
        );
        let settings = kiana_config::settings::Settings {
            npm_command: Some(vec!["sh".to_string(), "-c".to_string(), script]),
            ..kiana_config::settings::Settings::default()
        };
        let manager = DefaultPackageManager::new(super::super::PackageManagerOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: dir.path().join("agent").to_string_lossy().into_owned(),
            settings_manager: kiana_config::settings::SettingsManager::in_memory(settings),
            builtin_extensions: vec![],
        });
        // 第一次 `npm root -g` 输出空 → 不缓存。
        assert_eq!(manager.get_global_npm_root().expect("root"), "");
        // 第二次重新执行命令并拿到有效根。
        assert_eq!(manager.get_global_npm_root().expect("root"), "/root2");
        assert_eq!(std::fs::read_to_string(&counter).expect("counter"), "2\n");
        // 非空结果会被缓存，不再执行命令。
        assert_eq!(manager.get_global_npm_root().expect("root"), "/root2");
        assert_eq!(std::fs::read_to_string(&counter).expect("counter"), "2\n");
    }

    #[test]
    fn ambiguous_wrapped_managers_throw() {
        let manager = manager_with_npm_command(Some(strings(&["corepack", "pnpm", "npm"])));
        let error = manager.get_package_manager_name().expect_err("ambiguous");
        assert_eq!(
            error.message(),
            "Ambiguous npmCommand package managers: pnpm, npm"
        );
    }

    fn manager_with_raw_npm_command(value: serde_json::Value) -> DefaultPackageManager {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut settings = kiana_config::settings::Settings::default();
        settings.extra.insert("npmCommand".to_string(), value);
        DefaultPackageManager::new(super::super::PackageManagerOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: dir.path().join("agent").to_string_lossy().into_owned(),
            settings_manager: kiana_config::settings::SettingsManager::in_memory(settings),
            builtin_extensions: vec![],
        })
    }

    #[test]
    fn falsy_npm_command_values_fall_back_to_npm() {
        // TS: `this.settings.npmCommand ? [...value] : undefined` —— false/0/""/null 都是
        // falsy，`getNpmCommand()` 返回 undefined，package-manager 使用默认 npm。
        for value in [
            serde_json::Value::Null,
            serde_json::json!(false),
            serde_json::json!(0),
            serde_json::json!(""),
        ] {
            let manager = manager_with_raw_npm_command(value.clone());
            let command = manager.get_npm_command().expect("default command");
            assert_eq!(command.command, "npm", "value {value}");
            assert!(command.args.is_empty(), "value {value}");
        }
    }

    #[test]
    fn truthy_non_iterable_npm_command_matches_v8_error() {
        // TypeError 来自 SettingsManager.getNpmCommand 里的数组展开
        // `[...this.settings.npmCommand]`，V8 的文案就是 `this.settings.npmCommand`。
        for value in [
            serde_json::json!(true),
            serde_json::json!(42),
            serde_json::json!({"a": 1}),
        ] {
            let manager = manager_with_raw_npm_command(value.clone());
            let error = manager.get_npm_command().expect_err("not iterable");
            assert_eq!(
                error.message(),
                "this.settings.npmCommand is not iterable",
                "value {value}"
            );
        }
    }

    #[test]
    fn npm_command_array_entries_are_coerced_like_node_spawn() {
        // TS 解构保留原始元素；Node 的 spawn 在 argv 上做 `String(value)`。
        let manager = manager_with_raw_npm_command(serde_json::json!([
            "npm",
            42,
            true,
            null,
            {"x": 1},
            [1, 2]
        ]));
        let command = manager.get_npm_command().expect("command");
        assert_eq!(command.command, "npm");
        assert_eq!(
            command.args,
            strings(&["42", "true", "null", "[object Object]", "1,2"])
        );
    }

    #[test]
    fn non_string_npm_command_reports_node_spawn_error() {
        let cases = [
            (
                serde_json::json!([42]),
                "The \"file\" argument must be of type string. Received type number (42)",
            ),
            (
                serde_json::json!([true]),
                "The \"file\" argument must be of type string. Received type boolean (true)",
            ),
            (
                serde_json::json!([{"a": 1}]),
                "The \"file\" argument must be of type string. Received an instance of Object",
            ),
        ];
        for (value, expected) in cases {
            let manager = manager_with_raw_npm_command(value.clone());
            let error = manager.get_npm_command().expect_err("non-string command");
            assert_eq!(error.message(), expected, "value {value}");
        }
    }

    #[test]
    fn empty_and_empty_first_entry_npm_command() {
        let manager = manager_with_raw_npm_command(serde_json::json!([]));
        let command = manager.get_npm_command().expect("default command");
        assert_eq!(command.command, "npm");
        assert!(command.args.is_empty());

        for value in [serde_json::json!([""]), serde_json::json!([false])] {
            let manager = manager_with_raw_npm_command(value.clone());
            let error = manager.get_npm_command().expect_err("invalid command");
            assert_eq!(
                error.message(),
                "Invalid npmCommand: first array entry must be a non-empty command",
                "value {value}"
            );
        }
    }
}
