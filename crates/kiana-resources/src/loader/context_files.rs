//! 上下文文件（`AGENTS.md` / `CLAUDE.md`）发现与 worktree 去重。
//!
//! TS: `packages/coding-agent/src/core/resource-loader.ts` 的
//! `loadContextFileFromDir` / `findShadowedContextFile` / `loadProjectContextFiles`。
//!
//! worktree 逻辑的三处比较都是**字符串**比较（`spec/ca-resources.md` §8.4 第 8 条）：
//! `worktreeRoot.startsWith(mainRepoRoot + sep)`、`canonicalizePath(join(mainRepoRoot, ".git"))
//! !== commonGitDir`、`join(canonicalizePath(currentDir), basename(contextFile.path)) ===
//! shadowedContextFile`。Rust 的 `Path::starts_with` 按路径分量匹配，**不**等价。
//!
//! # 负责的边界
//! 只做“找文件 + 读内容 + 去重”，把结果交回 `loader`：本模块不缓存、不执行 override、
//! 不写文件系统。crate 内唯一调用方是 `DefaultResourceLoader::reload` 的第 24 步；
//! `no_context_files` 为真时调用方直接跳过本模块，而 `agents_files_override` 在拿到本模块
//! 结果之后才执行。`load_project_context_files` 同时经 `index` 再导出为公开 API
//! （`kiana-coding-agent` 的 SDK 导出也引用它），因此文件名顺序与去重结果同样是可观察契约。
//!
//! # 一次加载的执行流程
//! 1. 先读 `agent_dir` 下的全局上下文文件，命中则放进结果最前面；
//! 2. 用 [`find_shadowed_context_file`] 判断 cwd 是否在“嵌套 linked worktree”里；
//! 3. 从 `cwd` 逐级向上直到根，每层读一个候选文件；收集时头插，因此祖先链结果是
//!    “根祖先 → … → cwd”（越靠近 cwd 越靠后）；
//! 4. 最后把全局文件接在祖先链前面。
//!
//! # 注意事项
//! - 去重键是**原始路径字符串**（`seen_paths`），不是 canonicalize 后的真实路径；同一个
//!   文件经符号链接以不同字符串出现时可能重复收录。
//! - 候选文件名、顺序由集成测试（`tests/resource_loader.rs`）锁定；大小写变体与读取失败时
//!   的警告文案与 TS 对齐（`spec/ca-resources.md` §2.8.4），但目前没有测试锁定。

use std::collections::HashSet;
use std::path::Path;

use kiana_core::paths::{PathInputOptions, canonicalize_path, resolve_path};
use kiana_core::text::strip_bom;
use kiana_ext_api::system_prompt::ContextFile;

use crate::footer_data_provider::find_git_paths;
use crate::loader::source_info_map::{basename, dirname, join_path, node_error_to_string};

/// 每个目录的候选文件（顺序即优先级；大小写变体逐字保留，以匹配不同文件系统上的实际文件名）。
/// TS: `resource-loader.ts#loadContextFileFromDir` 的 `candidates`。
const CONTEXT_FILE_CANDIDATES: [&str; 5] = [
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

/// 读取一个目录里的上下文文件；`None` 表示该目录没有可读的候选文件。
/// TS: `resource-loader.ts#loadContextFileFromDir`。
///
/// 候选存在但不是普通文件时**静默跳过**；读取失败时输出
/// `Warning: Could not read <path>: <error>`（TS 用 `console.error`，`${error}` 是
/// `Error.toString()`，如 `Error: EACCES: permission denied, open '<path>'`）后继续下一个候选。
///
/// 候选顺序由 `CONTEXT_FILE_CANDIDATES` 固定，返回**第一个**可读的候选；因此同一目录同时
/// 有 `AGENTS.override.md` 与 `AGENTS.md` 时只有前者生效（后者不会被读取）。
///
/// 流程：`Path::exists` 预检 → `metadata` 判断是否普通文件 → `read` 读原始字节。三步中
/// 任何一步失败都只跳过当前候选继续下一个；全部候选耗尽返回 `None`。`exists` 会跟随符号
/// 链接，断链视为不存在（静默跳过，不产生警告）。内容按 UTF-8 **有损**解码（非法字节换成
/// U+FFFD）后去掉 BOM，不做换行转换。
pub fn load_context_file_from_dir(dir: &str) -> Option<ContextFile> {
    for filename in CONTEXT_FILE_CANDIDATES {
        let file_path = join_path(dir, filename);
        if !Path::new(&file_path).exists() {
            continue;
        }
        match std::fs::metadata(&file_path) {
            Ok(stat) => {
                if !stat.is_file() {
                    continue;
                }
            }
            Err(error) => {
                let message = node_error_to_string(&error, "stat", &file_path);
                tracing::warn!("Warning: Could not read {file_path}: {message}");
                continue;
            }
        }
        match std::fs::read(&file_path) {
            Ok(bytes) => {
                let content = String::from_utf8_lossy(&bytes);
                return Some(ContextFile {
                    path: file_path,
                    content: strip_bom(&content),
                });
            }
            Err(error) => {
                let message = node_error_to_string(&error, "open", &file_path);
                tracing::warn!("Warning: Could not read {file_path}: {message}");
            }
        }
    }
    None
}

/// 嵌套 linked worktree 的上下文文件是否遮蔽主仓库根目录的同名文件。
/// TS: `resource-loader.ts#findShadowedContextFile`。
///
/// 返回主仓库根目录下**同名**文件的路径（已 canonicalize）。普通仓库、兄弟 worktree、
/// bare 布局（`.bare` + `main`）与 submodule 都返回 `None`。
///
/// 判定分三步，全部走字符串比较：
/// 1. `find_git_paths` 找不到仓库（含损坏的 `.git`）→ `None`，普通目录不做遮蔽判断；
/// 2. `worktree_root` 必须是 `main_repo_root` 的严格后代（以 `main_repo_root + 分隔符`
///    为前缀）；普通仓库两者相同、兄弟 worktree 不在其下，都返回 `None`；
/// 3. `canonicalize_path(main_repo_root/.git)` 必须等于 `common_git_dir`，用于排除 bare
///    布局与 submodule（它们的 `.git` 不是同一仓库的检出）。
///
/// 通过后读取 **worktree 根目录**（不是 cwd）的上下文文件：没有则返回 `None`，主仓库的
/// 文件不会被遮蔽；有则返回“主仓库根 + 同名 basename”。返回值只是字符串拼接，不保证
/// 指向存在的文件，由调用方比较。
pub fn find_shadowed_context_file(cwd: &str) -> Option<String> {
    let git_paths = find_git_paths(Path::new(cwd))?;
    let common_git_dir = canonicalize_path(&git_paths.common_git_dir.to_string_lossy());
    let worktree_root = canonicalize_path(&git_paths.repo_dir.to_string_lossy());
    let main_repo_root = dirname(&common_git_dir);
    // 字符串前缀比较：普通仓库（两者相同）与兄弟 worktree（主仓库不是祖先）都为 false。
    let sep = std::path::MAIN_SEPARATOR;
    if !worktree_root.starts_with(&format!("{main_repo_root}{sep}")) {
        return None;
    }
    // `dirname(commonGitDir)` 只有在它本身是同一仓库的检出时才是主 worktree 根：
    // bare 布局里它只是 `.bare` 的父目录；submodule 的 gitdir 没有 `commondir`，
    // 落在 `.git/modules` 下。两种情况的 `.git` 都不等于 `commonGitDir`。
    if canonicalize_path(&join_path(&main_repo_root, ".git")) != common_git_dir {
        return None;
    }
    let worktree_context_file = load_context_file_from_dir(&worktree_root)?;
    Some(join_path(
        &main_repo_root,
        &basename(&worktree_context_file.path),
    ))
}

/// 加载全局（agentDir）与 cwd 祖先链上的上下文文件，并按 worktree 规则去重。
/// TS: `resource-loader.ts#loadProjectContextFiles`。
///
/// 顺序：全局 → 根祖先 → … → cwd 自身（最近的最后）。`seenPaths` 用原始路径
/// （未 canonicalize），因此同一文件经不同路径（符号链接）可能重复加载。
///
/// `cwd` / `agent_dir` 先以进程当前目录为基准、用 `resolve_path` 默认选项解析（展开 `~`、
/// 转换 `file://`），之后才参与比较。循环终止条件是 `dirname(current) == current`
/// （到达文件系统根），因此会越过普通仓库根继续向上，直到根。
///
/// 遮蔽判断比较的是**当前目录的真实路径 + 文件名**，而不是文件路径本身：worktree 的
/// `AGENTS.md` 若是指向主仓库文件的符号链接，`canonicalize(current_dir)` 后拼出的路径会
/// 等于 shadowed 路径，从而被跳过。祖先链使用头插，最终顺序即“根祖先 → … → cwd”。
///
/// 全局文件与祖先链共用同一个 `seen_paths`；若两者恰好是同一原始路径字符串，全局那份
/// 保留在结果最前面，祖先链中的重复项被跳过。
pub fn load_project_context_files(cwd: &str, agent_dir: &str) -> Vec<ContextFile> {
    let resolved_cwd = resolve_path(
        cwd,
        &crate::loader::source_info_map::process_cwd(),
        &PathInputOptions::default(),
    );
    let resolved_agent_dir = resolve_path(
        agent_dir,
        &crate::loader::source_info_map::process_cwd(),
        &PathInputOptions::default(),
    );

    let mut context_files: Vec<ContextFile> = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();

    if let Some(global_context) = load_context_file_from_dir(&resolved_agent_dir) {
        seen_paths.insert(global_context.path.clone());
        context_files.push(global_context);
    }

    let shadowed_context_file = find_shadowed_context_file(&resolved_cwd);
    let mut ancestor_context_files: Vec<ContextFile> = Vec::new();
    let mut current_dir = resolved_cwd;

    loop {
        let context_file = load_context_file_from_dir(&current_dir);
        // 比较目录的真实路径而不是文件：worktree 的 AGENTS.md 若符号链接到主仓库文件，
        // 会解析成 shadowed 路径本身。
        let is_shadowed = match (&shadowed_context_file, &context_file) {
            (Some(shadowed), Some(file)) => {
                let directory = canonicalize_path(&current_dir);
                join_path(&directory, &basename(&file.path)) == *shadowed
            }
            _ => false,
        };
        if let Some(file) = context_file
            && !is_shadowed
            && !seen_paths.contains(&file.path)
        {
            seen_paths.insert(file.path.clone());
            ancestor_context_files.insert(0, file);
        }

        let parent_dir = dirname(&current_dir);
        if parent_dir == current_dir {
            break;
        }
        current_dir = parent_dir;
    }

    context_files.extend(ancestor_context_files);
    context_files
}
