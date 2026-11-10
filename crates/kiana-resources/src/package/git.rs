//! `installGit` / `ensureGitRef` / marker / prune / remote head。
//!
//! TS: `package-manager.ts` 的 `installGit` / `updateGit` / `hasMissingGitDependencies` /
//! `repairMissingGitDependencies` / `getGitUpdateMarkerPath` /
//! `cleanAndInstallGitDependencies` / `ensureGitRef` / `refreshTemporaryGitSource` /
//! `removeGit` / `pruneEmptyGitParents` / `getRemoteGitHead` / `getLocalGitUpdateTarget` /
//! `getGitUpstreamRef` / `runGitRemoteCommand` / `gitHasAvailableUpdate`。
//!
//! git 命令形态逐字（§2.10.19）：`git clone <repo> <targetDir>`、`git fetch origin <ref>`、
//! `git reset --hard <ref>^{commit}`、`git clean -fdx`、`git ls-remote origin <ref|HEAD>`。
//!
//! # 这个文件负责什么
//!
//! 它实现 git 包源的安装、更新、删除，以及支撑更新的 marker 恢复机制。输入是已经解析好
//! 的 `GitSource`（`repo`、`host`、`path`、`r#ref`、`pinned`）和作用域 `SourceScope`；
//! 输出只有 `Result<(), PackageManagerError>` 或少量探测结果（远端 HEAD、是否有更新）。
//! URL / ref 的解析与安全校验属于 `kiana_core::git`，本文件不重复做。
//!
//! 安装目录布局由 `paths` 子模块决定：user 作用域是 `<agent_dir>/git/<host>/<path>`，
//! project 作用域是 `<cwd>/.kiana/git/<host>/<path>`，temporary 作用域在 agent 临时目录
//! 下按 `host/path/ref` 生成 hash 目录。本文件只调用 `get_git_install_path` /
//! `get_git_install_root`，不自行拼装安装根。
//!
//! # 调用关系
//!
//! - `resolve` 子模块在包缺失时调用 `install_git`；temporary 作用域的缓存已存在、在线且
//!   不是固定 ref 时，调用 `refresh_temporary_git_source` 原地刷新。
//! - `mod.rs` 的 `PackageManager::install` / `remove` 分别调用 `install_git` / `remove_git`，
//!   `update_configured_sources` 调用 `update_git`，`check_package_update` 调用
//!   `git_has_available_update`。
//! - checkout 里的 JavaScript 依赖要执行 npm 命令，所以本文件调用 `npm` 子模块的
//!   `get_git_dependency_install_args` / `run_npm_command`。
//!
//! # 安装与更新流程
//!
//! `install_git`：
//! 1. 计算目标目录；目录已存在时不再 clone，而是走“对齐现有 checkout”分支：固定 `ref`
//!    执行 `fetch origin <ref>`，否则先解析 upstream / `origin/HEAD`。
//! 2. 目录不存在时准备 git 安装根：非 temporary 作用域先写 `.gitignore`，创建目标目录的
//!    父目录，并删除可能残留的更新 marker。
//! 3. 依次执行 `git clone`、可选的 `git checkout <ref>`、以及 `package.json` 存在时的 npm
//!    依赖安装。任一步失败都会尽力删除目标目录并清理空父目录，再把原错误向上抛出。
//!
//! `update_git`：目标不存在时退化为安装；有固定 ref 时执行 `fetch origin <ref>` 后交给
//! `ensure_git_ref`；没有 ref 时由 `get_local_git_update_target` 解析出 upstream 或
//! `origin/HEAD` 作为目标。
//!
//! # marker 状态机（`ensure_git_ref`）
//!
//! marker 是目标目录父目录下名为 `.<basename>.kiana-update-incomplete` 的空文件。
//! 1. 先 fetch，再比较本地 `HEAD` 与目标 `<ref>^{commit}`。
//! 2. 相等且 marker 存在：上一次更新在 reset 之后、依赖安装完成之前中断，需要执行完整的
//!    `git clean -fdx` 与依赖安装；相等且没有 marker：只补装缺失依赖。
//! 3. 不相等：先写 marker，再 `reset --hard`，最后执行清理与依赖安装；全部成功后才删除
//!    marker。因此在 reset 之后失败会留下 marker，供下一次调用重做恢复步骤。
//!
//! `remove_git` 与全新安装都会删除 marker；marker 放在父目录而不是目标目录内部，因此
//! 删除目标目录不会连带删掉它。
//!
//! # 关键约束与失败模式
//!
//! - 所有 git 操作都是外部进程，错误文本沿用 `exec` 子模块的
//!   `"{command} {args} failed ..."` 形式；`PackageManagerError` 不携带错误码，调用方只能
//!   依靠文本和上下文区分失败原因。
//! - 探测命令（`rev-parse` / `ls-remote`）统一使用 10 秒超时（`NETWORK_TIMEOUT_MS`）；
//!   clone / fetch / checkout / reset / clean 走 `run_command`，没有超时机制。
//! - 离线检查不在本文件：`install_git` / `update_git` 自身不读 `KIANA_OFFLINE`；只有
//!   `git_has_available_update` 与 `refresh_temporary_git_source` 在入口检查并直接返回。
//!   离线时是否跳过安装由 `resolve` / `update` 的调用方决定。
//! - `git_has_available_update` 把任何底层错误折叠成 `false`（“没有更新”），调用方无法
//!   区分网络失败与“确实没有新提交”。
//! - `prune_empty_git_parents` 用字符串 `starts_with` 判断是否仍在安装根内，且只删除空
//!   目录；它不解析符号链接。
//!
//! # 阅读顺序
//!
//! 先看 `install_git` / `update_git` 掌握正常路径，再看 `ensure_git_ref` 与
//! `clean_and_install_git_dependencies` 理解 marker 恢复，最后看
//! `get_local_git_update_target` / `get_remote_git_head` 了解 upstream 与远端探测。

use std::sync::LazyLock;

use kiana_core::git::GitSource;
use regex::Regex;

use super::exec::{CaptureOptions, NETWORK_TIMEOUT_MS, js_trim, run_command, run_command_capture};
use super::paths::{dirname, join, path_exists, sep};
use super::types::SourceScope;
use super::{DefaultPackageManager, PackageManagerError, err};

// `git ls-remote` 输出按行解析：每行形如 `<40位十六进制 commit>\t<ref>`。
// REMOTE_REF_RE 只要求“行首 40 位十六进制 + 空白”，不校验 ref 名；
// REMOTE_HEAD_RE 额外要求该行以 `HEAD` 结尾（`ls-remote origin HEAD` 的输出形态）。
// 两个正则都用 `(?m)` 多行模式，`^`/`$` 锚定到行而非整串。
static REMOTE_REF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^([0-9a-f]{40})\s+").expect("valid remote ref regex"));
static REMOTE_HEAD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^([0-9a-f]{40})\s+HEAD$").expect("valid remote head regex"));

/// `getLocalGitUpdateTarget(installedPath)` 的结果。
///
/// 三个字段一起描述“把 checkout 对齐到哪”：`fetch_args` 负责把远端引用更新到本地跟踪
/// 分支，`ref` 是随后 `rev-parse` / `reset` 使用的引用名，`head` 是该引用当前指向的
/// commit。`head` 是 capture 输出（`run_command_capture` 成功时已 `js_trim`），
/// `ensure_git_ref` 比较前会再 `js_trim`（逐字对齐 TS 的 `.trim()`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitUpdateTarget {
    /// `ref`（`@{upstream}` 或 `origin/HEAD`）。
    pub r#ref: String,
    /// 目标 commit（capture 输出，`run_command_capture` 成功时已 `js_trim`）。
    pub head: String,
    /// fetch 参数。
    pub fetch_args: Vec<String>,
}

/// 递归删除，忽略不存在与失败（Node 的 `rmSync(..., {recursive:true, force:true})`）。
///
/// 先用 `symlink_metadata` 判定条目类型：符号链接按文件删除，不会跟随到链接目标；目录用
/// `remove_dir_all`。因为所有错误都被丢弃，调用方无法得知删除是否成功；权限不足或目录
/// 被占用时残留内容会留在磁盘上。
pub(crate) fn remove_path_force(path: &str) {
    let metadata = std::fs::symlink_metadata(path);
    match metadata {
        Ok(metadata) if metadata.is_dir() => {
            let _ = std::fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = std::fs::remove_file(path);
        }
        Err(_) => {}
    }
}

/// git 命令的字符串形式（错误文案用）。
fn args_of(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

impl DefaultPackageManager {
    /// `installGit(source, scope)`。
    ///
    /// 目标目录已存在时不会 clone，而是原地对齐：有固定 `ref` 时 fetch 该 ref 并以
    /// `FETCH_HEAD` 为目标，否则调用 `get_local_git_update_target` 解析 upstream 或
    /// `origin/HEAD`。目录不存在时执行 clone、可选 checkout 与 npm 依赖安装；这之后任一
    /// 步骤失败都会尽力删除目标目录并清理空父目录，避免留下半成品 checkout，然后把原
    /// 错误返回。本方法不检查离线模式、不发进度事件（由调用方的 `with_progress` 包装）。
    pub(crate) async fn install_git(
        &self,
        source: &GitSource,
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        let target_dir = self.get_git_install_path(source, scope)?;
        // 已安装：只做引用对齐，不重新 clone；固定 ref 的语义是“checkout 到该 ref”。
        if path_exists(&target_dir) {
            if let Some(r#ref) = &source.r#ref {
                return self
                    .ensure_git_ref(
                        &target_dir,
                        &args_of(&["fetch", "origin", r#ref]),
                        "FETCH_HEAD",
                    )
                    .await;
            }
            let target = self.get_local_git_update_target(&target_dir).await?;
            return self
                .ensure_git_ref(&target_dir, &target.fetch_args, &target.r#ref)
                .await;
        }
        let git_root = self.get_git_install_root(scope)?;
        // temporary 作用域没有安装根（返回 None），也就不写 .gitignore、不做父目录清理。
        if let Some(git_root) = &git_root {
            self.ensure_git_ignore(git_root)?;
        }
        std::fs::create_dir_all(dirname(&target_dir)).map_err(|error| {
            err(super::paths::format_fs_error(
                &error,
                "mkdir",
                &dirname(&target_dir),
            ))
        })?;
        remove_path_force(&self.get_git_update_marker_path(&target_dir));

        // clone → checkout（可选）→ npm 依赖安装视为一个整体：任一步失败都删除目标目录。
        let result: Result<(), PackageManagerError> = async {
            run_command("git", &args_of(&["clone", &source.repo, &target_dir]), None).await?;
            if let Some(r#ref) = &source.r#ref {
                run_command("git", &args_of(&["checkout", r#ref]), Some(&target_dir)).await?;
            }
            let package_json_path = join(&[&target_dir, "package.json"]);
            if path_exists(&package_json_path) {
                let args = self.get_git_dependency_install_args()?;
                self.run_npm_command(&args, Some(&target_dir)).await?;
            }
            Ok(())
        }
        .await;

        if let Err(error) = result {
            remove_path_force(&target_dir);
            self.prune_empty_git_parents(&target_dir, git_root.as_deref());
            return Err(error);
        }
        Ok(())
    }

    /// `updateGit(source, scope)`。
    ///
    /// 目标目录不存在时直接调用 `install_git`（更新与安装的失败语义一致）。存在时只做
    /// 引用对齐：固定 `ref` 用 `fetch origin <ref>` + `FETCH_HEAD`；否则用
    /// `get_local_git_update_target` 解析出的跟踪分支或 `origin/HEAD`。不检查离线模式，
    /// 也不发进度事件。
    pub(crate) async fn update_git(
        &self,
        source: &GitSource,
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        let target_dir = self.get_git_install_path(source, scope)?;
        if !path_exists(&target_dir) {
            return self.install_git(source, scope).await;
        }
        if let Some(r#ref) = &source.r#ref {
            return self
                .ensure_git_ref(
                    &target_dir,
                    &args_of(&["fetch", "origin", r#ref]),
                    "FETCH_HEAD",
                )
                .await;
        }
        let target = self.get_local_git_update_target(&target_dir).await?;
        self.ensure_git_ref(&target_dir, &target.fetch_args, &target.r#ref)
            .await
    }

    /// `hasMissingGitDependencies(targetDir)`。
    ///
    /// 只检查 `package.json` 的 `dependencies` 对象里每个名字对应的
    /// `node_modules/<name>` 是否存在，不校验版本，也不递归检查依赖的依赖。以下情况都返回
    /// `false`（视为“没有缺失”）：文件不存在、读取失败、JSON 解析失败、没有
    /// `dependencies` 或它不是对象。依赖名会先经过词法路径解析，解析结果不在
    /// `node_modules` 之内的条目被跳过（避免 `../x` 之类的名字把检查带到目录外）。
    /// 因此 `node_modules` 整个缺失时，只要有合法依赖名就会返回 `true`。
    pub(crate) fn has_missing_git_dependencies(&self, target_dir: &str) -> bool {
        let package_json_path = join(&[target_dir, "package.json"]);
        if !path_exists(&package_json_path) {
            return false;
        }
        // 读不到或解析不了时无法判断缺失，按“没有缺失”返回 false。
        let Ok(bytes) = std::fs::read(&package_json_path) else {
            return false;
        };
        let content = String::from_utf8_lossy(&bytes);
        let content = kiana_core::text::strip_bom(&content);
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) else {
            return false;
        };
        let Some(dependencies) = manifest.get("dependencies") else {
            return false;
        };
        let Some(dependencies) = dependencies.as_object() else {
            return false;
        };
        let node_modules_dir = super::paths::resolve_join(&[target_dir, "node_modules"]);
        let required_prefix = format!("{node_modules_dir}{}", sep());
        dependencies.keys().any(|name| {
            let dependency_path = super::paths::resolve_join(&[&node_modules_dir, name]);
            // 名字解析后必须仍在 node_modules 目录内；否则跳过该名字，避免目录外探测。
            if !dependency_path.starts_with(&required_prefix) {
                return false;
            }
            !path_exists(&dependency_path)
        })
    }

    /// `repairMissingGitDependencies(targetDir)`。
    ///
    /// 只在 `has_missing_git_dependencies` 为真时执行一次 npm 依赖安装（参数由当前
    /// npmCommand 推导）；否则立即成功。npm 命令失败时错误原样返回，不会回滚已经存在的
    /// `node_modules`。
    pub(crate) async fn repair_missing_git_dependencies(
        &self,
        target_dir: &str,
    ) -> Result<(), PackageManagerError> {
        if !self.has_missing_git_dependencies(target_dir) {
            return Ok(());
        }
        let args = self.get_git_dependency_install_args()?;
        self.run_npm_command(&args, Some(target_dir)).await
    }

    /// `getGitUpdateMarkerPath(targetDir)`：`<parent>/.<name>.kiana-update-incomplete`。
    ///
    /// marker 刻意放在目标目录的父目录：更新过程会清理目标目录内部，放在里面会被一起删掉。
    /// 内容是空文件，存在与否本身就是状态。
    pub(crate) fn get_git_update_marker_path(&self, target_dir: &str) -> String {
        join(&[
            &dirname(target_dir),
            &format!(
                ".{}.kiana-update-incomplete",
                super::paths::basename(target_dir)
            ),
        ])
    }

    /// `cleanAndInstallGitDependencies(targetDir, markerPath)`。
    ///
    /// 更新后的收尾步骤，顺序固定：
    /// 1. `git clean -fdx` 删除所有未跟踪与忽略文件（包括 `node_modules`）；
    /// 2. `package.json` 存在时执行一次 npm 依赖安装；
    /// 3. 两步都成功后才删除 marker。
    ///
    /// 失败语义：`git clean` 失败时先尽力 `repair_missing_git_dependencies`（其结果被忽略），
    /// 再把 `git clean` 的原错误返回；npm 安装失败则直接返回该错误。这两种情况下 marker
    /// 都保留，下一次 `ensure_git_ref` 会据此重做清理与安装。
    pub(crate) async fn clean_and_install_git_dependencies(
        &self,
        target_dir: &str,
        marker_path: &str,
    ) -> Result<(), PackageManagerError> {
        // 清理未跟踪文件；如果失败，先修复被删掉的依赖再抛出原错误。
        if let Err(error) = run_command("git", &args_of(&["clean", "-fdx"]), Some(target_dir)).await
        {
            let _ = self.repair_missing_git_dependencies(target_dir).await;
            return Err(error);
        }
        let package_json_path = join(&[target_dir, "package.json"]);
        if path_exists(&package_json_path) {
            let args = self.get_git_dependency_install_args()?;
            self.run_npm_command(&args, Some(target_dir)).await?;
        }
        remove_path_force(marker_path);
        Ok(())
    }

    /// `ensureGitRef(targetDir, fetchArgs, ref)`。
    ///
    /// 把 checkout 对齐到 `ref`，并保证依赖完整。流程：先按 `fetch_args` fetch；再比较本地
    /// `HEAD` 与 `ref^{commit}`（两者都 trim 后比较，`ref` 可以是 `FETCH_HEAD`、
    /// `@{upstream}` 或 `origin/HEAD`）。
    ///
    /// - 已对齐且 marker 存在：说明上次更新中断，执行完整清理与依赖安装；
    /// - 已对齐且没有 marker：只补装缺失依赖；
    /// - 未对齐：先写 marker，再 `reset --hard <ref>^{commit}`，最后清理与安装。
    ///
    /// 错误来源包括：fetch/rev-parse 子进程失败、marker 写入失败（错误文本按
    /// `format_fs_error` 的 `open` 形态生成）、reset 失败。reset 失败时 marker 已经落盘，
    /// 下一次调用会走恢复分支。
    pub(crate) async fn ensure_git_ref(
        &self,
        target_dir: &str,
        fetch_args: &[String],
        r#ref: &str,
    ) -> Result<(), PackageManagerError> {
        run_command("git", fetch_args, Some(target_dir)).await?;

        // 1) fetch 之后分别取本地 HEAD 与目标 commit；两个 capture 都用 10 秒超时。
        let local_head = run_command_capture(
            "git",
            &args_of(&["rev-parse", "HEAD"]),
            &CaptureOptions {
                cwd: Some(target_dir.to_string()),
                timeout_ms: Some(NETWORK_TIMEOUT_MS),
                env: Vec::new(),
            },
        )
        .await?;
        let commit_ref = format!("{}^{{commit}}", r#ref);
        let target_head = run_command_capture(
            "git",
            &args_of(&["rev-parse", &commit_ref]),
            &CaptureOptions {
                cwd: Some(target_dir.to_string()),
                timeout_ms: Some(NETWORK_TIMEOUT_MS),
                env: Vec::new(),
            },
        )
        .await?;
        let marker_path = self.get_git_update_marker_path(target_dir);
        // 2) HEAD 已对齐：marker 存在说明上次中断，需要完整清理；否则只修依赖。
        if js_trim(&local_head) == js_trim(&target_head) {
            if path_exists(&marker_path) {
                return self
                    .clean_and_install_git_dependencies(target_dir, &marker_path)
                    .await;
            }
            return self.repair_missing_git_dependencies(target_dir).await;
        }

        // 3) 需要移动 HEAD：先写 marker 再 reset，保证中途失败留下的 checkout 能被识别。
        std::fs::write(&marker_path, "")
            .map_err(|error| err(super::paths::format_fs_error(&error, "open", &marker_path)))?;
        run_command(
            "git",
            &args_of(&["reset", "--hard", &commit_ref]),
            Some(target_dir),
        )
        .await?;
        self.clean_and_install_git_dependencies(target_dir, &marker_path)
            .await
    }

    /// `refreshTemporaryGitSource(source, sourceStr)`：失败静默，保留缓存。
    ///
    /// 离线时直接返回；否则以 `Pull` 动作、`Refreshing <sourceStr>...` 文案包住一次
    /// `update_git(Temporary)`。`update_git` 的错误被丢弃，因此本方法**总是**返回
    /// `Ok(())`——调用方无法得知刷新是否成功，旧缓存继续可用。
    pub(crate) async fn refresh_temporary_git_source(
        &self,
        source: &GitSource,
        source_str: &str,
    ) -> Result<(), PackageManagerError> {
        if super::is_offline_mode_enabled() {
            return Ok(());
        }
        let _ = self
            .with_progress(
                super::types::ProgressAction::Pull,
                source_str,
                &format!("Refreshing {source_str}..."),
                || async { self.update_git(source, SourceScope::Temporary).await },
            )
            .await;
        Ok(())
    }

    /// `removeGit(source, scope)`。
    ///
    /// 顺序：删除目标目录、删除 marker（两者的删除错误都被忽略），再计算安装根并清理空
    /// 父目录。`get_git_install_root` 若返回错误（例如 project 未受信），此时目标目录已经
    /// 被删掉，错误仍会向上返回。通过 `PackageManager::remove` 调用时，trust 检查已在
    /// 外层完成。
    pub(crate) async fn remove_git(
        &self,
        source: &GitSource,
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        let target_dir = self.get_git_install_path(source, scope)?;
        remove_path_force(&target_dir);
        remove_path_force(&self.get_git_update_marker_path(&target_dir));
        let install_root = self.get_git_install_root(scope)?;
        self.prune_empty_git_parents(&target_dir, install_root.as_deref());
        Ok(())
    }

    /// `pruneEmptyGitParents(targetDir, installRoot?)`。
    ///
    /// `install_root` 为 `None`（temporary 作用域）时是空操作。否则从目标目录的父目录开始
    /// 向上走，只要仍在安装根之下就尝试删除空目录：目录不存在时不删除、继续向上；遇到
    /// `read_dir` 失败、目录非空或 `remove_dir_all` 失败则立即停止。安装根本身不会被删。
    ///
    /// `startsWith` 是**字符串**前缀比较（不检查分隔符），逐字复刻 TS。
    pub(crate) fn prune_empty_git_parents(&self, target_dir: &str, install_root: Option<&str>) {
        let Some(install_root) = install_root else {
            return;
        };
        let resolved_root = super::paths::path_resolve(&[install_root]);
        let mut current = dirname(target_dir);
        while current.starts_with(&resolved_root) && current != resolved_root {
            if !path_exists(&current) {
                current = dirname(&current);
                continue;
            }
            let entries = match std::fs::read_dir(&current) {
                Ok(entries) => entries.count(),
                Err(_) => break,
            };
            if entries > 0 {
                break;
            }
            if std::fs::remove_dir_all(&current).is_err() {
                break;
            }
            current = dirname(&current);
        }
    }

    /// `getRemoteGitHead(installedPath)`。
    ///
    /// 先尝试跟踪分支：由 `get_git_upstream_ref` 得到 `refs/heads/<branch>`，再执行
    /// `git ls-remote origin <ref>`，用正则取行首 40 位十六进制 commit。拿不到可用的
    /// upstream（命令失败、不是 `origin/` 前缀、分支名为空）或输出不匹配时，退回
    /// `git ls-remote origin HEAD`，要求输出中存在一行以 `HEAD` 结尾。
    ///
    /// 返回纯 commit 字符串（不含换行）。两次 `ls-remote` 都失败时返回子进程错误；命令
    /// 成功但没有匹配行时返回 `Failed to determine remote HEAD`。
    pub(crate) async fn get_remote_git_head(
        &self,
        installed_path: &str,
    ) -> Result<String, PackageManagerError> {
        let upstream_ref = self.get_git_upstream_ref(installed_path).await;
        if let Some(upstream_ref) = upstream_ref {
            let remote_head = self
                .run_git_remote_command(
                    installed_path,
                    &args_of(&["ls-remote", "origin", &upstream_ref]),
                )
                .await?;
            if let Some(captures) = REMOTE_REF_RE.captures(&remote_head)
                && let Some(commit) = captures.get(1)
            {
                return Ok(commit.as_str().to_string());
            }
        }

        let remote_head = self
            .run_git_remote_command(installed_path, &args_of(&["ls-remote", "origin", "HEAD"]))
            .await?;
        match REMOTE_HEAD_RE.captures(&remote_head) {
            Some(captures) => match captures.get(1) {
                Some(commit) => Ok(commit.as_str().to_string()),
                None => Err(err("Failed to determine remote HEAD")),
            },
            None => Err(err("Failed to determine remote HEAD")),
        }
    }

    /// `getLocalGitUpdateTarget(installedPath)`。
    ///
    /// 解析“更新时应把 checkout 对齐到哪个引用”，有两种策略：
    /// 1. 首选当前分支的 upstream：`rev-parse --abbrev-ref @{upstream}` 必须是
    ///    `origin/<branch>` 形式且分支名非空，目标 ref 记为 `@{upstream}`；
    /// 2. 任一前置条件失败（detached HEAD、upstream 缺失或不是 origin、输出为空等），退回
    ///    `origin/HEAD`：先 `git remote set-head origin -a`（失败被忽略），再
    ///    `rev-parse origin/HEAD` 取 commit，并用 `symbolic-ref` 找出它指向的分支，以便
    ///    构造只更新该分支的 fetch 参数；`symbolic-ref` 失败时退化为 `+HEAD:refs/remotes/origin/HEAD`。
    ///
    /// 两种策略都返回 `fetch --prune --no-tags origin <refspec>` 形式的参数。策略 2 中
    /// `rev-parse origin/HEAD` 失败会向上返回错误；策略 1 的错误不会冒泡（被回退逻辑取代）。
    pub(crate) async fn get_local_git_update_target(
        &self,
        installed_path: &str,
    ) -> Result<GitUpdateTarget, PackageManagerError> {
        // 首选策略：跟踪分支的 upstream；任一校验失败就把错误交给下面的回退分支。
        let attempt: Result<GitUpdateTarget, PackageManagerError> = async {
            let upstream = run_command_capture(
                "git",
                &args_of(&["rev-parse", "--abbrev-ref", "@{upstream}"]),
                &CaptureOptions {
                    cwd: Some(installed_path.to_string()),
                    timeout_ms: Some(NETWORK_TIMEOUT_MS),
                    env: Vec::new(),
                },
            )
            .await?;
            let trimmed_upstream = js_trim(&upstream);
            if !trimmed_upstream.starts_with("origin/") {
                return Err(err(format!(
                    "Unsupported upstream remote: {trimmed_upstream}"
                )));
            }
            let branch = &trimmed_upstream["origin/".len()..];
            if branch.is_empty() {
                return Err(err("Missing upstream branch name"));
            }
            let head = run_command_capture(
                "git",
                &args_of(&["rev-parse", "@{upstream}"]),
                &CaptureOptions {
                    cwd: Some(installed_path.to_string()),
                    timeout_ms: Some(NETWORK_TIMEOUT_MS),
                    env: Vec::new(),
                },
            )
            .await?;
            Ok(GitUpdateTarget {
                r#ref: "@{upstream}".to_string(),
                head,
                fetch_args: args_of(&[
                    "fetch",
                    "--prune",
                    "--no-tags",
                    "origin",
                    &format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
                ]),
            })
        }
        .await;

        // 回退策略：以 origin/HEAD 为目标；先尝试让 git 刷新这个符号引用。
        match attempt {
            Ok(target) => Ok(target),
            Err(_) => {
                let _ = run_command(
                    "git",
                    &args_of(&["remote", "set-head", "origin", "-a"]),
                    Some(installed_path),
                )
                .await;
                let head = run_command_capture(
                    "git",
                    &args_of(&["rev-parse", "origin/HEAD"]),
                    &CaptureOptions {
                        cwd: Some(installed_path.to_string()),
                        timeout_ms: Some(NETWORK_TIMEOUT_MS),
                        env: Vec::new(),
                    },
                )
                .await?;
                let origin_head_ref = run_command_capture(
                    "git",
                    &args_of(&["symbolic-ref", "refs/remotes/origin/HEAD"]),
                    &CaptureOptions {
                        cwd: Some(installed_path.to_string()),
                        timeout_ms: Some(NETWORK_TIMEOUT_MS),
                        env: Vec::new(),
                    },
                )
                .await
                .unwrap_or_default();
                // TS 是 `replace(/^refs\/remotes\/origin\//, "")`：没有前缀时保留整串。
                let trimmed_origin_head = js_trim(&origin_head_ref);
                let branch = trimmed_origin_head
                    .strip_prefix("refs/remotes/origin/")
                    .unwrap_or(trimmed_origin_head)
                    .to_string();
                if !branch.is_empty() {
                    return Ok(GitUpdateTarget {
                        r#ref: "origin/HEAD".to_string(),
                        head,
                        fetch_args: args_of(&[
                            "fetch",
                            "--prune",
                            "--no-tags",
                            "origin",
                            &format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
                        ]),
                    });
                }
                Ok(GitUpdateTarget {
                    r#ref: "origin/HEAD".to_string(),
                    head,
                    fetch_args: args_of(&[
                        "fetch",
                        "--prune",
                        "--no-tags",
                        "origin",
                        "+HEAD:refs/remotes/origin/HEAD",
                    ]),
                })
            }
        }
    }

    /// `getGitUpstreamRef(installedPath)`。
    ///
    /// 返回 `refs/heads/<branch>`，仅当当前分支的 upstream 形如 `origin/<branch>` 且分支名
    /// 非空。`rev-parse` 失败（例如没有配置 upstream）或输出不是 `origin/` 前缀时返回
    /// `None`；本方法不产生错误，调用方用 `None` 表示“无法从 upstream 判断远端”。
    pub(crate) async fn get_git_upstream_ref(&self, installed_path: &str) -> Option<String> {
        let upstream = run_command_capture(
            "git",
            &args_of(&["rev-parse", "--abbrev-ref", "@{upstream}"]),
            &CaptureOptions {
                cwd: Some(installed_path.to_string()),
                timeout_ms: Some(NETWORK_TIMEOUT_MS),
                env: Vec::new(),
            },
        )
        .await
        .ok()?;
        let trimmed = js_trim(&upstream);
        let branch = trimmed.strip_prefix("origin/")?;
        if branch.is_empty() {
            None
        } else {
            Some(format!("refs/heads/{branch}"))
        }
    }

    /// `runGitRemoteCommand(installedPath, args)`：`GIT_TERMINAL_PROMPT=0`。
    pub(crate) async fn run_git_remote_command(
        &self,
        installed_path: &str,
        args: &[String],
    ) -> Result<String, PackageManagerError> {
        run_command_capture(
            "git",
            args,
            &CaptureOptions {
                cwd: Some(installed_path.to_string()),
                timeout_ms: Some(NETWORK_TIMEOUT_MS),
                env: vec![("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())],
            },
        )
        .await
    }

    /// `gitHasAvailableUpdate(installedPath)`。
    ///
    /// 比较本地 `HEAD` 与远端 head（经 `get_remote_git_head`）：不相等即视为“有可用更新”。
    /// 两层失败折叠需要区分清楚：
    /// - 离线（`KIANA_OFFLINE`）在入口直接返回 `Ok(false)`，不启动任何子进程；
    /// - 内部比较的任何子错误（`rev-parse` / `ls-remote` 失败、超时、输出无匹配行）都被
    ///   `unwrap_or(false)` 折叠成 `Ok(false)`。
    ///
    /// 因此 `Ok(false)` 无法区分“已是最新”、“离线”与“探测失败”，调用方（如
    /// `check_package_update`）只能把它当作“没有可做的事”。
    pub(crate) async fn git_has_available_update(
        &self,
        installed_path: &str,
    ) -> Result<bool, PackageManagerError> {
        if super::is_offline_mode_enabled() {
            return Ok(false);
        }
        let result: Result<bool, PackageManagerError> = async {
            let local_head = run_command_capture(
                "git",
                &args_of(&["rev-parse", "HEAD"]),
                &CaptureOptions {
                    cwd: Some(installed_path.to_string()),
                    timeout_ms: Some(NETWORK_TIMEOUT_MS),
                    env: Vec::new(),
                },
            )
            .await?;
            let remote_head = self.get_remote_git_head(installed_path).await?;
            Ok(js_trim(&local_head) != js_trim(&remote_head))
        }
        .await;
        Ok(result.unwrap_or(false))
    }
}
