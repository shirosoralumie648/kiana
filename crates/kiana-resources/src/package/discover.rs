//! `collectFiles` / `collectSkillEntries` / auto entries / ancestor agents。
//!
//! 被 `super::resolve` 调用（settings 顶层路径、包内约定目录与自动发现路径都要先拿到
//! 文件列表，再交给解析器）；ignore 匹配复用 `crate::skills::ignore_match::NpmIgnore`。
//!
//! TS: `package-manager.ts` 的 `collectFiles` / `collectSkillEntries` /
//! `collectAutoSkillEntries` / `findGitRepoRoot` / `collectAncestorAgentsSkillDirs` /
//! `collectAutoPromptEntries` / `collectAutoThemeEntries` / `resolveExtensionEntries` /
//! `collectAutoExtensionEntries` / `collectResourceFiles` 与 ignore 相关内部函数
//! （`toPosixPath` / `prefixIgnorePattern` / `addIgnoreRules`，最初来自 `skills.ts`）。
//!
//! 递归顺序：每个目录先处理本层条目（按 Node `readdirSync` 的文件名字节序），目录条目
//! 就地递归。`std::fs::read_dir` 返回 getdents 原始顺序，与 Node 不同，因此统一走
//! `read_dir_sorted`。
//!
//! # 忽略规则如何生效
//! 每访问一个目录，都会读取其中的 `.gitignore` / `.ignore` / `.fdignore`，把模式改写成
//! 相对扫描根的形式（`prefix_ignore_pattern`）后追加到同一个 `IgnoreMatcher`。
//! 这个 matcher 沿递归向下传递、在整个扫描过程中持续累积，因此后访问目录的规则会与前面
//! 目录的规则共存；由于每条规则都带自己的根相对前缀，效果等同于一个 `ignore()`
//! 实例被逐层 `add`。规则顺序（含 `!` 否定）必须保留，`NpmIgnore` 依赖它实现“后者覆盖
//! 前者”。
//!
//! # 遍历与返回顺序
//! 目录项一律经 `read_dir_sorted`（文件名字节序）。目录条目在它的排序位置就地递归，
//! 文件在该位置直接追加，因此结果不是“先所有文件、再所有目录”。`collect_skill_entries`
//! 是例外：第一遍找到 `SKILL.md` 就立即 `return`，整棵子树只产生一个条目。
//!
//! # 失败模式
//! 这些函数都返回 `Vec<String>` / `Option<String>`，不返回错误：目录不存在或不可读、
//! 单个条目的 `file_type` 查询失败、断链符号链接、ignore 文件读取失败都会被静默跳过，
//! 调用方拿到的是已经收集到的部分结果（对应 TS 中 `try/catch` 后返回已积累结果）。
//! 扫描没有 visited 集合，符号链接会被跟随，修改目录内容时需注意这一点。

use crate::skills::ignore_match::NpmIgnore;

use super::exec::js_trim;
use super::paths::{
    dirname, join, path_exists, read_dir_sorted, relative, resolve_join, to_posix_path,
};
use super::resolve::ResourceType;

/// `IGNORE_FILE_NAMES`。
///
/// 数组顺序就是读取顺序：同名目录下三个文件都存在时，先读 `.gitignore`、再读 `.ignore`、
/// 最后 `.fdignore`。由于后加入的规则可以覆盖前面的，顺序有语义，不能改成无序集合。
pub(crate) const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

/// 技能发现模式。TS: `type SkillDiscoveryMode = "kiana" | "agents"`。
///
/// 两种模式的唯一分歧是散装 `.md` 收哪一层（见 [`collect_skill_entries_inner`]）：
/// `Kiana` 模式对应 `.kiana/skills/`，约定散装文件只放根层；`Agents` 模式对应
/// `.agents/skills/`，散装文件只允许出现在子目录（根层留给 `SKILL.md` 技能目录）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillDiscoveryMode {
    /// `.kiana` 的 `skills/`：只在根目录收散装 `.md`。
    Kiana,
    /// `.agents/skills`：只在非根目录收散装 `.md`。
    Agents,
}

/// 累积 .gitignore 规则的匹配器（等价于 npm `ignore()` 实例）。
///
/// TS 的 `addIgnoreRules` 直接 `ig.add(patterns)`，即 npm `ignore` 7.0.8：默认
/// **大小写不敏感**（`ignorecase: true`）、brace 字面量、支持 POSIX 字符类。
/// Rust `ignore` crate 的 globset 语义在这些点上与 npm `ignore` 不同，因此复用
/// `skills::ignore_match::NpmIgnore`（该模块按 `node_modules/ignore/index.js` 逐段移植）。
pub(crate) struct IgnoreMatcher {
    inner: NpmIgnore,
}

impl IgnoreMatcher {
    /// 以 `root` 为相对基准创建空匹配器（npm `ignore` 的规则已由
    /// [`add_ignore_rules`] 加上相对 root 的前缀，实例本身不保存 root）。
    pub(crate) fn new(_root: &str) -> Self {
        Self {
            inner: NpmIgnore::new(),
        }
    }

    /// 添加一条规则（非法规则静默忽略：TS 的 `ig.add` 在 `try/catch` 内）。
    pub(crate) fn add_line(&mut self, line: &str) {
        self.inner.add(&[line.to_string()]);
    }

    /// TS 的 `ig.ignores(isDir ? `${relPath}/` : relPath)`；`path` 是相对 root 的
    /// POSIX 路径。目录必须带尾部 `/`，这是 npm `ignore` 判定目录规则的依据。
    pub(crate) fn ignores(&mut self, path: &str, is_dir: bool) -> bool {
        if is_dir {
            self.inner.ignores(&format!("{path}/"))
        } else {
            self.inner.ignores(path)
        }
    }
}

/// `prefixIgnorePattern(line, prefix)`。
///
/// 返回 `None` 表示该行不产生规则：按 JS `trim` 后为空的行，以及 `#` 开头（但
/// 不是 `\#`）的注释行。其余行以**原始未 trim 的 `line`** 为模式主体：去掉可选的
/// `!`（记为否定）或 `\!` 的第一个反斜杠、去掉可选的锚定前导 `/`，再拼上根相对前缀。
/// 因此行首空白不会在这里被删除；`trimmed` 只用于空行/注释的判定。
pub(crate) fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = js_trim(line);
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("\\#") {
        return None;
    }

    let mut pattern = line;
    let mut negated = false;
    if let Some(rest) = pattern.strip_prefix('!') {
        negated = true;
        pattern = rest;
    } else if pattern.starts_with("\\!") {
        // TS 只删掉反斜杠（`pattern.slice(1)`），保留字面量 `!`。
        pattern = &pattern[1..];
    }
    if let Some(rest) = pattern.strip_prefix('/') {
        pattern = rest;
    }

    let prefixed = if prefix.is_empty() {
        pattern.to_string()
    } else {
        format!("{prefix}{pattern}")
    };
    Some(if negated {
        format!("!{prefixed}")
    } else {
        prefixed
    })
}

/// `addIgnoreRules(ig, dir, rootDir)`：每个层级的规则都加上相对 root 的前缀后累积。
///
/// 规则追加到调用方传入的同一个 matcher，不返回新对象；递归扫描因此能看到祖先目录与
/// 已访问兄弟目录的全部规则。`dir == root_dir` 时前缀为空串，否则前缀是以 `/` 结尾的
/// 根相对 POSIX 路径。文件不存在或读取失败直接跳过；没有 ignore 文件的目录是正常情况，
/// 不是错误。
pub(crate) fn add_ignore_rules(matcher: &mut IgnoreMatcher, dir: &str, root_dir: &str) {
    let relative_dir = relative(root_dir, dir);
    let prefix = if relative_dir.is_empty() {
        String::new()
    } else {
        format!("{}/", to_posix_path(&relative_dir))
    };

    for filename in IGNORE_FILE_NAMES {
        let ignore_path = join(&[dir, filename]);
        if !path_exists(&ignore_path) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&ignore_path) else {
            continue;
        };
        let content = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = content.split('\n').collect();
        for (index, line) in lines.iter().enumerate() {
            // `split(/\r?\n/)`：只有后接 `\n` 的 `\r` 属于分隔符；文件末尾孤立的
            // `\r` 是普通字符（与 `skills::discover` 的实现一致）。
            let line = if index + 1 < lines.len() {
                line.strip_suffix('\r').unwrap_or(line)
            } else {
                line
            };
            if let Some(pattern) = prefix_ignore_pattern(line, &prefix) {
                matcher.add_line(&pattern);
            }
        }
    }
}

/// 返回 `(is_dir, is_file)`；无法判定时 `None`，调用方应跳过该条目。
///
/// 符号链接走 `metadata`（跟随目标），与 TS 的 `statSync` 一致：断链的链接会失败并
/// 被跳过，而指向目录的链接会被当作目录继续递归。普通条目用 `file_type()`（readdir
/// 自带的类型，不额外 stat），只有符号链接才付一次 `stat` 的代价。
fn entry_kind(entry: &std::fs::DirEntry) -> Option<(bool, bool)> {
    let file_type = entry.file_type().ok()?;
    if file_type.is_symlink() {
        let metadata = std::fs::metadata(entry.path()).ok()?;
        Some((metadata.is_dir(), metadata.is_file()))
    } else {
        Some((file_type.is_dir(), file_type.is_file()))
    }
}

/// 资源类型对应的文件名判定（`FILE_PATTERNS` 的等价物）。
///
/// 供 [`collect_files_inner`] 在递归中过滤文件；扩展类型在递归路径中不会出现——
/// [`collect_resource_files`] 把扩展分发给 manifest/入口文件语义。
pub(crate) fn matches_file_pattern(name: &str, resource_type: ResourceType) -> bool {
    match resource_type {
        ResourceType::Extensions => name.ends_with(".ts") || name.ends_with(".js"),
        ResourceType::Skills | ResourceType::Prompts => name.ends_with(".md"),
        ResourceType::Themes => name.ends_with(".json"),
    }
}

/// `collect_files` 的递归实现。
///
/// 阶段：目录不存在直接返回 → 把本目录的 ignore 规则追加到共享 matcher → 按文件名序
/// 读取条目 → 逐个跳过隐藏项（`.` 开头）与 `node_modules`（受 `skip_node_modules`
/// 控制）→ 解析类型（跟随符号链接）→ 用相对 `root` 的 POSIX 路径查询 matcher →
/// 目录就地递归，匹配后缀的文件追加到 `files`。
///
/// `matcher` 与 `root` 在整个递归中保持不变，规则只会增加；每条规则都带根相对前缀，
/// 因此累积不会改变其它子树的结果（见模块文档）。读取目录失败时该子树被静默放弃，
/// 已收集的结果保留。
fn collect_files_inner(
    dir: &str,
    resource_type: ResourceType,
    skip_node_modules: bool,
    matcher: &mut IgnoreMatcher,
    root: &str,
    files: &mut Vec<String>,
) {
    if !path_exists(dir) {
        return;
    }
    add_ignore_rules(matcher, dir, root);
    let Some(entries) = read_dir_sorted(dir) else {
        return;
    };
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if skip_node_modules && name == "node_modules" {
            continue;
        }
        let full_path = join(&[dir, &name]);
        let Some((is_dir, is_file)) = entry_kind(&entry) else {
            continue;
        };
        let rel_path = to_posix_path(&relative(root, &full_path));
        if matcher.ignores(&rel_path, is_dir) {
            continue;
        }
        if is_dir {
            collect_files_inner(
                &full_path,
                resource_type,
                skip_node_modules,
                matcher,
                root,
                files,
            );
        } else if is_file && matches_file_pattern(&name, resource_type) {
            files.push(full_path);
        }
    }
}

/// `collect_files(dir, filePattern, skipNodeModules = true, ignoreMatcher?, rootDir?)`。
///
/// 递归收集 `dir` 下匹配 `resource_type` 后缀的文件（含 ignore 过滤），返回绝对路径，
/// 顺序为“目录条目在排序位置就地递归”的深度优先序。Rust 调用点固定
/// `skip_node_modules = true`、root 即 `dir`；TS 里其余参数组合只在
/// `skills.ts` 等其它模块出现，不经过这里。
pub(crate) fn collect_files(dir: &str, resource_type: ResourceType) -> Vec<String> {
    let mut files = Vec::new();
    if !path_exists(dir) {
        return files;
    }
    let mut matcher = IgnoreMatcher::new(dir);
    collect_files_inner(dir, resource_type, true, &mut matcher, dir, &mut files);
    files
}

/// `collect_skill_entries` 的递归实现。
///
/// 两遍扫描：
/// 1. 找名为 `SKILL.md` 的**文件**（符号链接按目标类型判断），未被 ignore 命中就追加并
///    立即 `return`——一个 `SKILL.md` 代表整个技能目录，其子目录不再扫描；
/// 2. 没有 `SKILL.md` 时，收集本目录的散装 `.md`：`Kiana` 模式只认扫描根这一层，
///    `Agents` 模式只认非根目录；随后对未忽略的子目录递归。
///
/// 第一遍不需要隐藏项 / `node_modules` 检查：它只处理名字精确等于 `SKILL.md` 的条目。
/// 第二遍才应用 `.` 开头与 `node_modules` 的剪枝。
fn collect_skill_entries_inner(
    dir: &str,
    mode: SkillDiscoveryMode,
    matcher: &mut IgnoreMatcher,
    root: &str,
    entries: &mut Vec<String>,
) {
    if !path_exists(dir) {
        return;
    }
    add_ignore_rules(matcher, dir, root);
    let Some(dir_entries) = read_dir_sorted(dir) else {
        return;
    };

    // 第一遍：找 `SKILL.md` 并立即停止递归。
    for entry in &dir_entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != "SKILL.md" {
            continue;
        }
        let full_path = join(&[dir, &name]);
        let Some((_, is_file)) = entry_kind(entry) else {
            continue;
        };
        let rel_path = to_posix_path(&relative(root, &full_path));
        if is_file && !matcher.ignores(&rel_path, false) {
            entries.push(full_path);
            return;
        }
    }

    // 第二遍：本目录散装 `.md` 与子目录。
    for entry in &dir_entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if name == "node_modules" {
            continue;
        }
        let full_path = join(&[dir, &name]);
        let Some((is_dir, is_file)) = entry_kind(entry) else {
            continue;
        };
        let rel_path = to_posix_path(&relative(root, &full_path));
        let should_include_markdown_file = is_file
            && name.ends_with(".md")
            && !matcher.ignores(&rel_path, false)
            && ((mode == SkillDiscoveryMode::Kiana && dir == root)
                || (mode == SkillDiscoveryMode::Agents && dir != root));
        if should_include_markdown_file {
            entries.push(full_path);
            continue;
        }
        if !is_dir {
            continue;
        }
        if matcher.ignores(&rel_path, true) {
            continue;
        }
        collect_skill_entries_inner(&full_path, mode, matcher, root, entries);
    }
}

/// `collectSkillEntries(dir, mode, ignoreMatcher?, rootDir?)`。
///
/// 收集 `dir` 下的技能入口：每个含 `SKILL.md` 的目录贡献该 `SKILL.md` 的路径并停止
/// 下探，散装 `.md` 按 [`SkillDiscoveryMode`] 决定收根层还是非根层。返回绝对路径，
/// 顺序是深度优先序。`resolve` 把这些路径放进 `ResolvedPaths`；真正的 frontmatter
/// 解析由 `loader` 调用 `skills::load_skills` 完成（`skills::discover` 同样以
/// “`SKILL.md` 命中即返回”为约定）。
pub(crate) fn collect_skill_entries(dir: &str, mode: SkillDiscoveryMode) -> Vec<String> {
    let mut entries = Vec::new();
    if !path_exists(dir) {
        return entries;
    }
    let mut matcher = IgnoreMatcher::new(dir);
    collect_skill_entries_inner(dir, mode, &mut matcher, dir, &mut entries);
    entries
}

/// `collectAutoSkillEntries(dir, mode)`。
///
/// 当前实现与 [`collect_skill_entries`] 完全相同——TS 中该函数同样是直接转调
/// （`package-manager.ts` 的 `collectAutoSkillEntries` 只有一行 `return`）；保留独立
/// 名字是为了与 TS 函数一一对应：`resolve.rs` 的自动发现路径调用
/// `collect_auto_skill_entries`，[`collect_skill_entries`] 则经 `collect_resource_files`
/// 走包内约定目录。
pub(crate) fn collect_auto_skill_entries(dir: &str, mode: SkillDiscoveryMode) -> Vec<String> {
    collect_skill_entries(dir, mode)
}

/// `findGitRepoRoot(startDir)`：`.git` 是文件也算。
///
/// 从 `start_dir`（先经 [`resolve_join`] 转绝对路径）逐级向上找名为 `.git` 的条目；
/// `.git` 文件（worktree/submodule 里的 gitdir 指针）同样算仓库根。到达文件系统根
/// （`dirname` 不再变化）仍没找到时返回 `None`，调用方
/// （[`collect_ancestor_agents_skill_dirs`]）据此只走到文件系统根为止。
pub(crate) fn find_git_repo_root(start_dir: &str) -> Option<String> {
    let mut dir = resolve_join(&[start_dir]);
    loop {
        if path_exists(&join(&[&dir, ".git"])) {
            return Some(dir);
        }
        let parent = dirname(&dir);
        if parent == dir {
            return None;
        }
        dir = parent;
    }
}

/// `collectAncestorAgentsSkillDirs(startDir)`：cwd 最近 → 最远，包含 git repo root。
///
/// 返回从 `start_dir` 向上每层的 `<dir>/.agents/skills`，最近的最先。终止条件是
/// “本层就是 git 仓库根”，没有仓库根时退到文件系统根（`dirname` 不再变化）。
/// `resolve` 按返回顺序逐个调用 [`collect_auto_skill_entries`]（Agents 模式），因此
/// 近处目录的技能会先进入累积器。
pub(crate) fn collect_ancestor_agents_skill_dirs(start_dir: &str) -> Vec<String> {
    let mut skill_dirs = Vec::new();
    let resolved_start_dir = resolve_join(&[start_dir]);
    let git_repo_root = find_git_repo_root(&resolved_start_dir);

    let mut dir = resolved_start_dir;
    loop {
        skill_dirs.push(join(&[&dir, ".agents", "skills"]));
        if git_repo_root.as_deref() == Some(dir.as_str()) {
            break;
        }
        let parent = dirname(&dir);
        if parent == dir {
            break;
        }
        dir = parent;
    }
    skill_dirs
}

/// 非递归收集 `dir` 直接子项中指定后缀的文件。
///
/// 只加载 `dir` 自身的 ignore 规则（不向下累积，因为没有递归）；隐藏项与
/// `node_modules` 一律跳过，符号链接按目标类型判断。返回顺序即文件名序，调用方不应重排。
/// 这是 [`collect_auto_prompt_entries`] 与 [`collect_auto_theme_entries`] 的共同实现。
fn collect_auto_direct_entries(dir: &str, extension: &str) -> Vec<String> {
    let mut entries = Vec::new();
    if !path_exists(dir) {
        return entries;
    }
    let mut matcher = IgnoreMatcher::new(dir);
    add_ignore_rules(&mut matcher, dir, dir);
    let Some(dir_entries) = read_dir_sorted(dir) else {
        return entries;
    };
    for entry in dir_entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if name == "node_modules" {
            continue;
        }
        let full_path = join(&[dir, &name]);
        let Some((_, is_file)) = entry_kind(&entry) else {
            continue;
        };
        let rel_path = to_posix_path(&relative(dir, &full_path));
        if matcher.ignores(&rel_path, false) {
            continue;
        }
        if is_file && name.ends_with(extension) {
            entries.push(full_path);
        }
    }
    entries
}

/// `collectAutoPromptEntries(dir)`：非递归，收 `.md`。
///
/// 自动发现的 prompt 目录约定散装文件直接放顶层，因此不需要递归；返回路径按文件名
/// 排序，`resolve` 按该顺序生成 prompt 模板条目。
pub(crate) fn collect_auto_prompt_entries(dir: &str) -> Vec<String> {
    collect_auto_direct_entries(dir, ".md")
}

/// `collectAutoThemeEntries(dir)`：非递归，收 `.json`。
///
/// 与 [`collect_auto_prompt_entries`] 同构，主题文件的约定后缀是 `.json`。
pub(crate) fn collect_auto_theme_entries(dir: &str) -> Vec<String> {
    collect_auto_direct_entries(dir, ".json")
}

/// `resolveExtensionEntries(dir)`：`package.json#kiana.extensions` → `index.ts` → `index.js`。
///
/// 三级回退，先命中先赢：
/// 1. manifest 的 `kiana.extensions` 数组——数组里的路径逐个 `resolve_join` 后仍要求
///    存在，不存在的条目被静默丢弃；全部丢弃（或数组为空）则继续下一级；
/// 2. `index.ts`；
/// 3. `index.js`。
/// 都没有时返回 `None`，调用方 [`collect_auto_extension_entries`] 把它当作“该目录
/// 不是扩展包”，继续扫描子目录。
pub(crate) fn resolve_extension_entries(dir: &str) -> Option<Vec<String>> {
    let package_json_path = join(&[dir, "package.json"]);
    if path_exists(&package_json_path)
        && let Some(entries) = kiana_core::kiana_manifest::read_kiana_manifest(
            std::path::Path::new(&package_json_path),
        )
        .and_then(|manifest| manifest.extensions)
        && !entries.is_empty()
    {
        let mut resolved_entries = Vec::new();
        for ext_path in &entries {
            let resolved_ext_path = resolve_join(&[dir, ext_path]);
            if path_exists(&resolved_ext_path) {
                resolved_entries.push(resolved_ext_path);
            }
        }
        if !resolved_entries.is_empty() {
            return Some(resolved_entries);
        }
    }

    let index_ts = join(&[dir, "index.ts"]);
    let index_js = join(&[dir, "index.js"]);
    if path_exists(&index_ts) {
        return Some(vec![index_ts]);
    }
    if path_exists(&index_js) {
        return Some(vec![index_js]);
    }
    None
}

/// `collectAutoExtensionEntries(dir)`：目录自身 manifest/index 优先，否则只下探一层。
///
/// 如果 `dir` 自己就是扩展包（[`resolve_extension_entries`] 命中），返回它的入口并
/// 不再扫描；否则扫 `dir` 的直接子项：散装 `.ts`/`.js` 直接收，子目录再给它一次
/// [`resolve_extension_entries`] 的机会（不继续深入第二层）。ignore 规则只读 `dir`
/// 本层的，与其它非递归收集一致。
pub(crate) fn collect_auto_extension_entries(dir: &str) -> Vec<String> {
    let mut entries = Vec::new();
    if !path_exists(dir) {
        return entries;
    }
    if let Some(root_entries) = resolve_extension_entries(dir) {
        return root_entries;
    }

    let mut matcher = IgnoreMatcher::new(dir);
    add_ignore_rules(&mut matcher, dir, dir);
    let Some(dir_entries) = read_dir_sorted(dir) else {
        return entries;
    };
    for entry in dir_entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if name == "node_modules" {
            continue;
        }
        let full_path = join(&[dir, &name]);
        let Some((is_dir, is_file)) = entry_kind(&entry) else {
            continue;
        };
        let rel_path = to_posix_path(&relative(dir, &full_path));
        if matcher.ignores(&rel_path, is_dir) {
            continue;
        }
        if is_file && (name.ends_with(".ts") || name.ends_with(".js")) {
            entries.push(full_path);
        } else if is_dir && let Some(resolved_entries) = resolve_extension_entries(&full_path) {
            entries.extend(resolved_entries);
        }
    }
    entries
}

/// `collectResourceFiles(dir, resourceType)`。
///
/// 按资源类型分发到不同的收集策略：技能走“SKILL.md 目录”语义（[`collect_skill_entries`]，
/// Kiana 模式），扩展走 manifest/入口文件语义（[`collect_auto_extension_entries`]），
/// prompt 与主题走普通递归后缀过滤（[`collect_files`]）。`resolve` 中三处针对包根下的
/// `<type>/` 约定目录（manifest 缺省或不可读时的回退与默认收集），另有一处在
/// `collect_files_from_paths` 中对 settings 提供的目录复用同一分发。有 manifest 条目时
/// 优先走 manifest，没有才扫目录。
pub(crate) fn collect_resource_files(dir: &str, resource_type: ResourceType) -> Vec<String> {
    match resource_type {
        ResourceType::Skills => collect_skill_entries(dir, SkillDiscoveryMode::Kiana),
        ResourceType::Extensions => collect_auto_extension_entries(dir),
        _ => collect_files(dir, resource_type),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &std::path::Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    #[test]
    fn prefix_ignore_pattern_matches_ts() {
        assert_eq!(prefix_ignore_pattern("  ", ""), None);
        assert_eq!(prefix_ignore_pattern("# comment", ""), None);
        assert_eq!(
            prefix_ignore_pattern("\\# not comment", ""),
            Some("\\# not comment".into())
        );
        assert_eq!(
            prefix_ignore_pattern("!keep", "sub/"),
            Some("!sub/keep".into())
        );
        assert_eq!(
            prefix_ignore_pattern("\\!literal", "sub/"),
            Some("sub/!literal".into())
        );
        assert_eq!(
            prefix_ignore_pattern("/anchored", "sub/"),
            Some("sub/anchored".into())
        );
    }

    #[test]
    fn collect_skill_entries_stops_at_skill_md() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join("root-skill").join("SKILL.md"), "x");
        write(
            &root.join("root-skill").join("nested").join("SKILL.md"),
            "y",
        );
        write(&root.join("loose.md"), "z");
        let entries =
            collect_skill_entries(root.to_str().expect("utf8"), SkillDiscoveryMode::Kiana);
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .any(|entry| entry.ends_with("root-skill/SKILL.md"))
        );
        assert!(entries.iter().any(|entry| entry.ends_with("loose.md")));
        assert!(!entries.iter().any(|entry| entry.contains("nested")));
    }

    #[test]
    fn agents_mode_only_collects_loose_markdown_below_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join("root-file.md"), "x");
        write(&root.join("nested-skill").join("SKILL.md"), "y");
        write(&root.join("third-party").join("child-skill.md"), "z");
        let entries =
            collect_skill_entries(root.to_str().expect("utf8"), SkillDiscoveryMode::Agents);
        assert!(!entries.iter().any(|entry| entry.ends_with("root-file.md")));
        assert!(
            entries
                .iter()
                .any(|entry| entry.ends_with("nested-skill/SKILL.md"))
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.ends_with("child-skill.md"))
        );
    }

    #[test]
    fn readdir_order_is_sorted_like_node() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        // 按非字典序创建，确认遍历不看文件系统顺序。
        write(&root.join("z.md"), "z");
        write(&root.join("a.md"), "a");
        write(&root.join("m.md"), "m");
        write(&root.join("z-skill").join("SKILL.md"), "z");
        write(&root.join("a-skill").join("SKILL.md"), "a");
        write(&root.join("m-dir").join("child.md"), "m");

        let root_text = root.to_str().expect("utf8");
        assert_eq!(
            collect_auto_prompt_entries(root_text),
            vec![
                root.join("a.md").to_string_lossy().into_owned(),
                root.join("m.md").to_string_lossy().into_owned(),
                root.join("z.md").to_string_lossy().into_owned(),
            ]
        );
        // 目录条目在原位置递归：a-skill -> a.md -> m-dir -> m.md -> z-skill -> z.md。
        assert_eq!(
            collect_skill_entries(root_text, SkillDiscoveryMode::Kiana),
            vec![
                root.join("a-skill/SKILL.md").to_string_lossy().into_owned(),
                root.join("a.md").to_string_lossy().into_owned(),
                root.join("m.md").to_string_lossy().into_owned(),
                root.join("z-skill/SKILL.md").to_string_lossy().into_owned(),
                root.join("z.md").to_string_lossy().into_owned(),
            ]
        );
        assert_eq!(
            collect_files(root_text, ResourceType::Prompts),
            vec![
                root.join("a-skill/SKILL.md").to_string_lossy().into_owned(),
                root.join("a.md").to_string_lossy().into_owned(),
                root.join("m-dir/child.md").to_string_lossy().into_owned(),
                root.join("m.md").to_string_lossy().into_owned(),
                root.join("z-skill/SKILL.md").to_string_lossy().into_owned(),
                root.join("z.md").to_string_lossy().into_owned(),
            ]
        );
    }

    #[test]
    fn gitignore_is_prefixed_and_respected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join(".gitignore"), "venv\n");
        write(&root.join("good").join("SKILL.md"), "x");
        write(&root.join("venv").join("bad").join("SKILL.md"), "y");
        let entries =
            collect_skill_entries(root.to_str().expect("utf8"), SkillDiscoveryMode::Kiana);
        assert!(entries.iter().any(|entry| entry.contains("good")));
        assert!(!entries.iter().any(|entry| entry.contains("venv")));
    }

    #[test]
    fn ignore_matcher_matches_node_ignore_defaults() {
        // npm `ignore()` 默认 `ignorecase: true`（package-manager.ts 的 `ignore()` 无参数）。
        let mut matcher = IgnoreMatcher::new("/root");
        matcher.add_line("foo");
        assert!(matcher.ignores("foo", false));
        assert!(matcher.ignores("FOO", false));
        assert!(matcher.ignores("sub/Foo", false));
        assert!(!matcher.ignores("foobar", false));

        // brace 是字面量（不展开）。
        let mut matcher = IgnoreMatcher::new("/root");
        matcher.add_line("a{b,c}");
        assert!(matcher.ignores("a{b,c}", false));
        assert!(!matcher.ignores("ab", false));
        assert!(!matcher.ignores("ac", false));

        // POSIX 字符类按 git wildmatch 解释。
        let mut matcher = IgnoreMatcher::new("/root");
        matcher.add_line("a[[:alpha:]]");
        assert!(matcher.ignores("ab", false));
        assert!(!matcher.ignores("a1", false));

        // 目录规则只在目录路径（带尾部 `/`）上生效。
        let mut matcher = IgnoreMatcher::new("/root");
        matcher.add_line("foo/");
        assert!(matcher.ignores("foo", true));
        assert!(!matcher.ignores("foo", false));

        // 非 basename 模式的结尾断言不匹配后代路径。
        let mut matcher = IgnoreMatcher::new("/root");
        matcher.add_line("a/**");
        matcher.add_line("!a/b");
        assert!(!matcher.ignores("a/b", false));
        assert!(matcher.ignores("a/b/c", false));
    }

    #[test]
    fn gitignore_matching_is_case_insensitive_during_discovery() {
        // node-ignore 默认 ignorecase；混合大小写目录也必须被剪枝。
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join(".gitignore"), "venv\n");
        write(&root.join("good").join("SKILL.md"), "x");
        write(&root.join("VENV").join("bad").join("SKILL.md"), "y");
        let entries =
            collect_skill_entries(root.to_str().expect("utf8"), SkillDiscoveryMode::Kiana);
        assert!(entries.iter().any(|entry| entry.contains("good")));
        assert!(!entries.iter().any(|entry| entry.contains("VENV")));
    }
}
