//! `applyPatterns` / `matchesAnyPattern` / overrides / `expandPackageGlob`。
//!
//! TS: `package-manager.ts` 的 `isPattern` / `isOverridePattern` / `hasGlobPattern` /
//! `expandPackageGlob` / `splitPatterns` / `matchesAnyPattern` / `normalizeExactPattern` /
//! `matchesAnyExactPattern` / `getOverridePatterns` / `isEnabledByOverrides` / `applyPatterns` /
//! `applyAutoloadDisabledPatterns` / `resourcePrecedenceRank`。
//!
//! ## MiniMatch
//!
//! TS 用 `minimatch@10`（默认选项：`dot:false`、`nocase:false`、`nocomment:false`、
//! `nonegate:false`）。`globset` 缺少前导 `!` 否定、`#` 注释与 dot 规则，
//! 因此这里实现一个私有子集：`*`、`?`、`**`（整段）、`{a,b}`、`{a..c}`、`[abc]`、`[!abc]`、
//! 反斜杠转义、前导 `!` 否定、`#` 注释。匹配按 UTF-16 code unit 进行（与 JS 正则的
//! 无 `u` 模式一致）。
//!
//! ## 为什么需要这个模块
//!
//! 解析包时，`resolve` 要把 settings 与 `package.json` 里的过滤模式应用到磁盘上收集到的
//! 文件列表，决定每个资源启用还是禁用；自动发现的资源还要判断是否被 overrides 排除。
//! TS 版直接复用 Node 的 `minimatch` 与 `fs.globSync`，Rust 依赖里没有语义等价物
//! （`globset` 缺前导 `!`、`#` 注释与 dot 规则），所以这里自带两个兼容实现：
//! 纯内存的 `MiniMatch` 与遍历文件系统的 `expand_package_glob`。任何与 Node 不一致的
//! 细节都会让同一份配置在 TS 与 Rust 版本下启用不同的扩展 / 技能 / prompt。
//!
//! ## 调用关系与边界
//!
//! 本文件只被同 crate 的 `super::resolve` 使用：`split_patterns` 与 `apply_patterns`
//! 处理 settings / manifest 条目，`is_enabled_by_overrides` 处理自动发现资源，
//! `expand_package_glob` 展开 manifest 里的 glob 条目，`resource_precedence_rank` 则是
//! `map_to_resolved` 的稳定排序键。本文件不读 settings、不安装包；只有
//! `expand_package_glob` 会遍历文件系统。
//!
//! ## 两条处理链
//!
//! 1. 过滤链（纯字符串）：`split_patterns` 按 `is_pattern` 把条目分成普通路径与模式；
//!    `apply_patterns` 按 includes → excludes → force-includes（精确）→ force-excludes
//!    （精确）四步产出启用集合；`is_enabled_by_overrides` 默认启用，`!` 关闭，
//!    `+` 重新打开，`-` 最后关闭。
//! 2. glob 链（文件系统）：`expand_package_glob` 先把 `\` 归一为 `/`、做花括号展开；
//!    每个展开结果再按 `/` 切段、词法归一 `.` / `..`，然后递归遍历目录；命中路径拼到
//!    `root` 下、过滤含 `.` 开头段的相对路径、按 UTF-16 码元序排序并去重。
//!
//! ## 重要约束
//!
//! - 匹配单位是 UTF-16 code unit（对应 JS 正则的无 `u` 模式），不是 Unicode 标量：
//!   BMP 之外的字符占两个单位，`?` 只吃一个单位。
//! - dot 规则：默认选项下 `*`、`?`、`[...]`、`**` 都不匹配 `.` 开头的段；只有模式显式
//!   以字面量 `.` 开头（包括被折叠成字面量的 `[.]`）才允许。
//! - 文件系统错误一律静默：读目录失败视为空目录、`metadata` 失败视为不存在，因此
//!   `expand_package_glob` 目前不会返回 `Err`；调用方用 `if let Ok(...)` 接收。
//! - `**` 遍历不进入符号链接目录；只有模式里显式的字面量段命中链接目录时，才会用跟随
//!   链接的 `metadata` 继续下探。
//! - 测试用 `tests/fixtures/package/minimatch_golden.json`（参考实现生成的用例表）与
//!   若干临时目录用例锁定上述行为；改动匹配语义必须同步核对它们。
//!
//! ## 阅读顺序
//!
//! 先读过滤链（`is_pattern` → `split_patterns` → `matches_any_pattern` /
//! `apply_patterns` / `is_enabled_by_overrides`），再读 glob 链
//! （`expand_package_glob` → `parse_glob_pattern` → `glob_walk`），最后读 `MiniMatch`
//! 的原子解析与匹配、以及 brace 展开。`resource_precedence_rank` 是独立的排序键，可最后看。

use indexmap::{IndexMap, IndexSet};

use super::PackageManagerError;
use super::paths::{
    basename, dirname, join, js_string_cmp, read_dir_sorted, relative, resolve_join, sep,
    to_posix_path,
};
use super::types::{PathMetadata, SourceScope};

/// `resourcePrecedenceRank(m)`：数字越小优先级越高。
///
/// 取值只由 `PathMetadata` 的三个字段决定：
/// - 0：project 作用域 + `source == "local"`（settings 里的显式条目）；
/// - 1：project 作用域的其它 source（自动发现等）；
/// - 2：非 project 作用域（user / temporary）+ `local`；
/// - 3：非 project 作用域（user / temporary）的其它 source；
/// - 4：`origin == Package`（包内资源，低于所有顶层来源）；
/// - 5：`source == "builtin"`。
///
/// `builtin` 与 package 两条判断先于作用域，因此它们不参与上面的组合。`resolve.rs` 的
/// `map_to_resolved` 把本函数用作稳定升序排序键，随后按 canonical 路径去重保留首个，
/// 所以 rank 小（优先级高）的资源最终胜出。
pub(crate) fn resource_precedence_rank(metadata: &PathMetadata) -> i32 {
    // 内置扩展在文件与包扩展之后加载（无论哪个作用域启用）。
    if metadata.source == "builtin" {
        return 5;
    }
    if metadata.origin == kiana_core::source_info::SourceOrigin::Package {
        return 4;
    }
    let scope_base = if metadata.scope == SourceScope::Project {
        0
    } else {
        2
    };
    scope_base + if metadata.source == "local" { 0 } else { 1 }
}

/// `isPattern(s)`：`!`/`+`/`-` 开头，或含 `*`/`?`。
///
/// 这是「条目需要进入模式处理」的判定：前缀形式表示 overrides，通配符表示 glob。
/// 它比 [`is_override_pattern`] 宽，因此 `split_patterns` 会把两类条目都划到 patterns。
pub(crate) fn is_pattern(value: &str) -> bool {
    value.starts_with('!')
        || value.starts_with('+')
        || value.starts_with('-')
        || value.contains('*')
        || value.contains('?')
}

/// `isOverridePattern(s)`：`!`/`+`/`-` 开头。
///
/// 三个前缀的语义见 [`apply_patterns`]：`!` 排除、`+` 强制包含、`-` 强制排除。
pub(crate) fn is_override_pattern(value: &str) -> bool {
    value.starts_with('!') || value.starts_with('+') || value.starts_with('-')
}

/// `hasGlobPattern(s)`：含 `*` 或 `?`。
pub(crate) fn has_glob_pattern(value: &str) -> bool {
    value.contains('*') || value.contains('?')
}

/// `splitPatterns(entries)`：按 [`is_pattern`] 分成普通条目与模式。
///
/// 返回 `(普通条目, 模式条目)`，两组都保持输入顺序。调用方 `resolve` 用普通条目
/// 解析成磁盘路径再收集文件，用模式条目对收集结果做启用/禁用过滤（`resolve.rs` 的
/// `resolve_local_entries`），因此这里只做分类，不做任何路径解析。
pub(crate) fn split_patterns(entries: &[String]) -> (Vec<String>, Vec<String>) {
    let mut plain = Vec::new();
    let mut patterns = Vec::new();
    for entry in entries {
        if is_pattern(entry) {
            patterns.push(entry.clone());
        } else {
            plain.push(entry.clone());
        }
    }
    (plain, patterns)
}

/// `getOverridePatterns(entries)`：只保留 `!`/`+`/`-` 开头的条目。
pub(crate) fn get_override_patterns(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .filter(|pattern| is_override_pattern(pattern))
        .cloned()
        .collect()
}

/// `expandPackageGlob(pattern, root)`：Node `fs.globSync` 的等价物（`dot:false`）。
///
/// 结果按 JS 字符串序（UTF-16 code unit）升序，并去重；相对路径里的 `.` 开头段被过滤
/// （`..` 允许，虽然 glob 不会产生它）。
///
/// 返回值中的每个元素都是 `resolve_join([root, 相对命中])` 后的路径；绝对 pattern
/// 命中时该路径本身就是绝对路径。调用方 `resolve.rs` 只做 `if let Ok(...)` 接收，
/// 因为按上述语义当前实现不会返回 `Err`。
///
/// 语义按 Node 的 `internal/fs/glob` 实现：
/// - 每段用 minimatch 默认选项匹配（`**` 只在整段时才是递归通配）；
/// - 花括号在整条 pattern 上先展开（单个分支的 `{x}` 保持字面量）；
/// - `**` 不跟随符号链接目录，但**显式字面量**命名到的符号链接目录会继续遍历；
/// - 非法 pattern（如 `[`）返回空列表而不是错误。
pub(crate) fn expand_package_glob(
    pattern: &str,
    root: &str,
) -> Result<Vec<String>, PackageManagerError> {
    // Node 的 fs.glob 用 `windowsPathsNoEscape: true` 的 minimatch：先把 `\` 换成 `/`，
    // 再展开花括号。
    let pattern = pattern.replace('\\', "/");
    let mut patterns = Vec::new();
    expand_braces(&pattern, &mut patterns);
    let mut matches: Vec<String> = Vec::new();
    for expanded in &patterns {
        glob_walk_pattern(expanded, root, &mut matches);
    }
    let mut result: Vec<String> = matches
        .into_iter()
        .map(|matched| resolve_join(&[root, &matched]))
        .filter(|path| {
            let relative_path = relative(root, path);
            relative_path.is_empty()
                || relative_path
                    .split(sep())
                    .all(|segment| segment == ".." || !segment.starts_with('.'))
        })
        .collect();
    result.sort_by(|a, b| js_string_cmp(a, b));
    result.dedup();
    Ok(result)
}

/// 展开后 pattern 的一段。
#[derive(Debug, Clone, PartialEq, Eq)]
enum GlobSegment {
    /// 整段 `**`。
    Globstar,
    /// 无通配符的字面量（单字符类如 `[.]` 已折叠为字符）。
    Literal(String),
    /// 含 `*`/`?`/多字符字符类的段。
    Magic(String),
    /// 末尾 `/` 产生的目录占位段。
    DirectoryOnly,
}

/// 解析后的 glob pattern。
struct GlobPattern {
    /// 是否绝对路径（`/...`）。
    absolute: bool,
    /// 原始 pattern 是否以 `./` 开头（影响尾部 `**` 是否包含起始目录）。
    starts_with_dot: bool,
    segments: Vec<GlobSegment>,
}

/// 按 Node/minimatch 的规则拆分 pattern。
///
/// 处理顺序：识别绝对路径与 `./` 前缀；按 `/` 切段并丢弃空段与内部 `.`；对 `a/..`
/// 做词法回退（紧跟在 `**` 之后的 `..` 保留，因为无法确定目录深度）；末尾 `/` 追加
/// 一个 `DirectoryOnly` 段。每段再交给 [`canonical_literal_segment`] 判断能否折叠为
/// 字面量，能折叠的走 `Literal` 分支，其余记为 `Magic`。
fn parse_glob_pattern(pattern: &str) -> GlobPattern {
    let absolute = pattern.starts_with('/');
    let starts_with_dot = pattern.starts_with("./");
    let mut parts: Vec<&str> = pattern.split('/').collect();
    let mut trailing_slash = false;
    while parts.len() > 1 && parts.last() == Some(&"") {
        trailing_slash = true;
        parts.pop();
    }
    let mut normalized: Vec<String> = Vec::new();
    for part in parts {
        if part.is_empty() || part == "." {
            // `//` 折叠；内部与首部 `.` 段被 minimatch 的预处理丢弃。
            continue;
        }
        if part == ".." && normalized.last().is_some_and(|last| last != "**") {
            // minimatch 的词法归一：`a/../b` -> `b`（`**` 之后的 `..` 保留）。
            normalized.pop();
            continue;
        }
        if part == ".." && normalized.is_empty() && absolute {
            continue;
        }
        normalized.push(part.to_string());
    }
    if trailing_slash && !normalized.is_empty() {
        normalized.push(String::new());
    }
    let segments = normalized
        .into_iter()
        .map(|segment| {
            if segment == "**" {
                GlobSegment::Globstar
            } else if segment.is_empty() {
                GlobSegment::DirectoryOnly
            } else if let Some(literal) = canonical_literal_segment(&segment) {
                GlobSegment::Literal(literal)
            } else {
                GlobSegment::Magic(segment)
            }
        })
        .collect();
    GlobPattern {
        absolute,
        starts_with_dot,
        segments,
    }
}

/// 若一段是 minimatch 意义上的字面量（`hasMagic === false`），返回折叠后的文本。
///
/// minimatch 会把单字符字符类（如 `[a]`、`[.]`）编译成字面量字符，因此
/// `a[.]b` 等价于 `a.b`；`[abc]`、`[!a]`、`[a-z]` 仍是通配符。
fn canonical_literal_segment(segment: &str) -> Option<String> {
    let chars: Vec<char> = segment.chars().collect();
    let mut literal = String::new();
    let mut index = 0usize;
    while index < chars.len() {
        match chars[index] {
            '\\' => {
                // 转义字符在 minimatch 里不是魔法：`\*` 是字面量 `*`。
                if index + 1 < chars.len() {
                    literal.push(chars[index + 1]);
                    index += 2;
                } else {
                    literal.push('\\');
                    index += 1;
                }
            }
            '*' | '?' => return None,
            '@' | '+' | '!' if chars.get(index + 1) == Some(&'(') => return None,
            '[' => {
                let Some(close) = class_end(&chars, index) else {
                    literal.push('[');
                    index += 1;
                    continue;
                };
                let content = &chars[index + 1..close];
                if content.len() == 1 && content[0] != '!' && content[0] != '^' {
                    literal.push(content[0]);
                } else {
                    return None;
                }
                index = close + 1;
            }
            character => {
                literal.push(character);
                index += 1;
            }
        }
    }
    Some(literal)
}

/// 找到 `[` 对应的 `]`（首字符位置的 `]` 不算结束）。
fn class_end(chars: &[char], open: usize) -> Option<usize> {
    let mut index = open + 1;
    if index < chars.len() && (chars[index] == '!' || chars[index] == '^') {
        index += 1;
    }
    if index < chars.len() && chars[index] == ']' {
        index += 1;
    }
    while index < chars.len() {
        if chars[index] == ']' {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// 在 `root` 下执行一次 glob，把 Node 风格的相对结果（`.` 起始）追加到 `out`。
fn glob_walk_pattern(pattern: &str, root: &str, out: &mut Vec<String>) {
    let parsed = parse_glob_pattern(pattern);
    if parsed.segments.is_empty() {
        return;
    }
    let (start_path, start_rel) = if parsed.absolute {
        (String::from(sep()), String::from(sep()))
    } else {
        (root.to_string(), String::from("."))
    };
    glob_walk(&start_path, &start_rel, &parsed.segments, 0, &parsed, out);
}

/// `glob_walk_pattern` 的递归主体：当前目录 `dir`、其相对起点 `rel`、
/// 待匹配段 `segments[index..]`。
///
/// 按段类型分派：
/// - `DirectoryOnly`（末尾 `/`）：仅当 `dir` 本身是目录时把 `rel` 记入 `out`；
/// - `Globstar`：末段时记录 `rel` 及其下所有非 `.` 开头后代（递归只进入真实目录）；
///   非末段时先尝试“零个目录”，再对每个真实目录递归；`.` 开头的目录只有在下一段
///   能匹配它时才下探；
/// - `Literal`：用跟随符号链接的 `metadata` 检查存在性，命中末段则记录，否则下探；
/// - `Magic`：读目录逐个用 `match_glob_segment` 匹配，末段直接记录，否则仅对真实
///   目录递归。
///
/// 任何文件系统失败（读目录、`metadata`）都被当作“没有条目/不存在”静默跳过，
/// 所以本函数没有 `Result`，上层 [`expand_package_glob`] 也不会因 IO 错误失败。
fn glob_walk(
    dir: &str,
    rel: &str,
    segments: &[GlobSegment],
    index: usize,
    parsed: &GlobPattern,
    out: &mut Vec<String>,
) {
    let Some(segment) = segments.get(index) else {
        return;
    };
    let last = index + 1 == segments.len();
    match segment {
        GlobSegment::DirectoryOnly => {
            // Node 的 `last === ""` 分支：p 为空时只有目录本身可以命中。
            if path_is_dir(dir) {
                out.push(rel.to_string());
            }
        }
        GlobSegment::Globstar => {
            if last {
                // 尾部 `**` 包含起始目录本身（Node：path !== "." 或 pattern 以 "." 开头，
                // 或 pattern 就是 "**"）。
                if rel != "." || parsed.starts_with_dot || index == 0 {
                    out.push(rel.to_string());
                }
                let Some(entries) = read_dir_sorted(dir) else {
                    return;
                };
                for entry in entries {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        continue;
                    }
                    let child_rel = join(&[rel, &name]);
                    let is_dir = entry_is_real_dir(&entry);
                    out.push(child_rel.clone());
                    if is_dir {
                        let child_path = join(&[dir, &name]);
                        glob_walk(&child_path, &child_rel, segments, index, parsed, out);
                    }
                }
            } else {
                // 零个目录。
                glob_walk(dir, rel, segments, index + 1, parsed, out);
                // 一个或多个目录：`**` 只下探真实目录（不跟随符号链接）；
                // 但能匹配下一段的点开头目录仍可继续（Node 的 matchesDot 分支）。
                let Some(entries) = read_dir_sorted(dir) else {
                    return;
                };
                for entry in entries {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !entry_is_real_dir(&entry) {
                        continue;
                    }
                    if name.starts_with('.') && !segment_matches_at(segments, index + 1, &name) {
                        continue;
                    }
                    let child_rel = join(&[rel, &name]);
                    let child_path = join(&[dir, &name]);
                    glob_walk(&child_path, &child_rel, segments, index, parsed, out);
                }
            }
        }
        GlobSegment::Literal(value) => {
            let child_rel = if value.is_empty() {
                rel.to_string()
            } else {
                join(&[rel, value])
            };
            let child_path = if value.is_empty() {
                dir.to_string()
            } else {
                join(&[dir, value])
            };
            // 字面量用 stat（跟随符号链接）：显式命名的链接目录可以继续遍历。
            if !path_exists(&child_path) {
                return;
            }
            if last {
                if !value.is_empty() || path_is_dir(&child_path) {
                    out.push(child_rel);
                }
            } else {
                glob_walk(&child_path, &child_rel, segments, index + 1, parsed, out);
            }
        }
        GlobSegment::Magic(pattern) => {
            if !path_is_dir(dir) {
                return;
            }
            let Some(entries) = read_dir_sorted(dir) else {
                return;
            };
            for entry in entries {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !match_glob_segment(pattern, &name) {
                    continue;
                }
                let child_rel = join(&[rel, &name]);
                if last {
                    out.push(child_rel);
                } else if entry_is_real_dir(&entry) {
                    let child_path = join(&[dir, &name]);
                    glob_walk(&child_path, &child_rel, segments, index + 1, parsed, out);
                }
            }
        }
    }
}

/// 下一段是否会匹配 `name`（只用于 `**` 下探点开头目录的判定）。
fn segment_matches_at(segments: &[GlobSegment], index: usize, name: &str) -> bool {
    match segments.get(index) {
        Some(GlobSegment::Literal(value)) => value == name,
        Some(GlobSegment::Magic(pattern)) => match_glob_segment(pattern, name),
        _ => false,
    }
}

/// 目录项是否为真实目录：`DirEntry::file_type()` 不跟随符号链接，因此链接目录
/// 在这里返回 `false`。`**` 与魔法段只用它决定“是否继续下探”，保证自动遍历不会
/// 进入链接目录（显式字面量段例外，见 `glob_walk`）。
fn entry_is_real_dir(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().is_ok_and(|file_type| file_type.is_dir())
}

/// `fs.stat` 成功即视为存在（跟随符号链接；断链与无权限都返回 `false`）。
fn path_exists(path: &str) -> bool {
    std::fs::metadata(path).is_ok()
}

/// `fs.stat` 成功且目标是目录（跟随符号链接）。
///
/// 与 [`entry_is_real_dir`] 的区别是这里走 `metadata`：显式字面量段命名到链接目录时
/// 仍能继续遍历。
fn path_is_dir(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

/// `normalizeExactPattern(pattern)`：去掉 `./` / `.\` 前缀，展开 `~`，转 POSIX 分隔符。
pub(crate) fn normalize_exact_pattern(pattern: &str) -> String {
    let normalized = if pattern.starts_with("./") || pattern.starts_with(".\\") {
        &pattern[2..]
    } else {
        pattern
    };
    to_posix_path(&kiana_core::paths::normalize_path(
        normalized,
        &kiana_core::paths::PathInputOptions::default(),
    ))
}

/// `matchesAnyPattern(filePath, patterns, baseDir)`。
///
/// 对每个 pattern 生成一个 [`MiniMatch`]，再对同一路径的三种文本分别尝试匹配：
/// 相对 `base_dir` 的 POSIX 路径、basename、完整 POSIX 路径；任一命中即返回 `true`。
/// 若 basename 是 `SKILL.md`，还把父目录的相对路径、父目录名与完整父目录路径纳入
/// 候选——这让 `skills/keep` 这类目录级模式可以选中目录里的 `SKILL.md`。
///
/// pattern 先用 `normalize_path` 的默认选项处理（展开 `~`、Windows 下做 shell 路径
/// 归一），再转成 POSIX 分隔符。本函数只判断“是否命中”，不区分 include/exclude；
/// `!`/`+`/`-` 前缀由调用方先剥离。
pub(crate) fn matches_any_pattern(file_path: &str, patterns: &[String], base_dir: &str) -> bool {
    let rel = to_posix_path(&relative(base_dir, file_path));
    let name = basename(file_path);
    let file_path_posix = to_posix_path(file_path);
    let is_skill_file = name == "SKILL.md";
    let parent_dir = if is_skill_file {
        Some(dirname(file_path))
    } else {
        None
    };
    let parent_rel = parent_dir
        .as_deref()
        .map(|dir| to_posix_path(&relative(base_dir, dir)));
    let parent_name = parent_dir.as_deref().map(basename);
    let parent_dir_posix = parent_dir.as_deref().map(to_posix_path);

    patterns.iter().any(|pattern| {
        let normalized_pattern = to_posix_path(&kiana_core::paths::normalize_path(
            pattern,
            &kiana_core::paths::PathInputOptions::default(),
        ));
        let matcher = MiniMatch::new(&normalized_pattern);
        if matcher.matches(&rel) || matcher.matches(&name) || matcher.matches(&file_path_posix) {
            return true;
        }
        if !is_skill_file {
            return false;
        }
        let Some(parent_rel) = parent_rel.as_deref() else {
            return false;
        };
        matcher.matches(parent_rel)
            || parent_name
                .as_deref()
                .is_some_and(|parent_name| matcher.matches(parent_name))
            || parent_dir_posix
                .as_deref()
                .is_some_and(|parent_dir_posix| matcher.matches(parent_dir_posix))
    })
}

/// `matchesAnyExactPattern(filePath, patterns, baseDir)`：不做 minimatch，直接字符串相等。
///
/// 候选集合与 [`matches_any_pattern`] 类似但不含 basename（相对路径、完整路径），
/// basename 只用于判定文件是否为 `SKILL.md`。每个 pattern 先经
/// [`normalize_exact_pattern`] 去掉 `./` 前缀并展开 `~`，然后只做 `==`
/// 比较；`SKILL.md` 额外与父目录的相对路径、完整父目录路径比较（不比较父目录名）。
/// `patterns` 为空时直接返回 `false`，避免构造无意义的比较。
pub(crate) fn matches_any_exact_pattern(
    file_path: &str,
    patterns: &[String],
    base_dir: &str,
) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let rel = to_posix_path(&relative(base_dir, file_path));
    let name = basename(file_path);
    let file_path_posix = to_posix_path(file_path);
    let is_skill_file = name == "SKILL.md";
    let parent_dir = if is_skill_file {
        Some(dirname(file_path))
    } else {
        None
    };
    let parent_rel = parent_dir
        .as_deref()
        .map(|dir| to_posix_path(&relative(base_dir, dir)));
    let parent_dir_posix = parent_dir.as_deref().map(to_posix_path);

    patterns.iter().any(|pattern| {
        let normalized = normalize_exact_pattern(pattern);
        if normalized == rel || normalized == file_path_posix {
            return true;
        }
        if !is_skill_file {
            return false;
        }
        normalized == parent_rel.as_deref().unwrap_or("")
            || normalized == parent_dir_posix.as_deref().unwrap_or("")
    })
}

/// `isEnabledByOverrides(filePath, patterns, baseDir)`。
///
/// 默认启用，然后按固定顺序应用三类 override（前缀在匹配前被剥掉）：
/// 1. `!` 用 [`matches_any_pattern`] 匹配，命中则关闭；
/// 2. `+` 用 [`matches_any_exact_pattern`] 精确匹配，命中则重新打开；
/// 3. `-` 用精确匹配，命中则最后关闭。
///
/// 因此对同一路径 `+` 能救回被 `!` 关掉的资源，而 `-` 又能最终否决它。不含前缀的
/// 普通模式在这里被忽略（不参与启用判定）。
pub(crate) fn is_enabled_by_overrides(
    file_path: &str,
    patterns: &[String],
    base_dir: &str,
) -> bool {
    let overrides = get_override_patterns(patterns);
    let excludes: Vec<String> = overrides
        .iter()
        .filter(|pattern| pattern.starts_with('!'))
        .map(|pattern| pattern[1..].to_string())
        .collect();
    let force_includes: Vec<String> = overrides
        .iter()
        .filter(|pattern| pattern.starts_with('+'))
        .map(|pattern| pattern[1..].to_string())
        .collect();
    let force_excludes: Vec<String> = overrides
        .iter()
        .filter(|pattern| pattern.starts_with('-'))
        .map(|pattern| pattern[1..].to_string())
        .collect();

    let mut enabled = true;
    if !excludes.is_empty() && matches_any_pattern(file_path, &excludes, base_dir) {
        enabled = false;
    }
    if !force_includes.is_empty() && matches_any_exact_pattern(file_path, &force_includes, base_dir)
    {
        enabled = true;
    }
    if !force_excludes.is_empty() && matches_any_exact_pattern(file_path, &force_excludes, base_dir)
    {
        enabled = false;
    }
    enabled
}

/// `applyPatterns(allPaths, patterns, baseDir)`：四步固定顺序，返回保留首次出现顺序的集合。
///
/// 顺序是 includes → excludes → force-includes → force-excludes：没有 include 时
/// 以全部路径起步；`!` 用 glob 删除；`+` 把 `all_paths` 中尚未入选且精确匹配的路径
/// 追加回来；`-` 最后用精确匹配删除。返回 `IndexSet` 的迭代顺序即最终展示顺序：
/// include 阶段继承 `all_paths` 的顺序，`+` 追加的条目按 `all_paths` 顺序排在后面。
pub(crate) fn apply_patterns(
    all_paths: &[String],
    patterns: &[String],
    base_dir: &str,
) -> IndexSet<String> {
    let mut includes: Vec<String> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut force_includes: Vec<String> = Vec::new();
    let mut force_excludes: Vec<String> = Vec::new();

    for pattern in patterns {
        if let Some(rest) = pattern.strip_prefix('+') {
            force_includes.push(rest.to_string());
        } else if let Some(rest) = pattern.strip_prefix('-') {
            force_excludes.push(rest.to_string());
        } else if let Some(rest) = pattern.strip_prefix('!') {
            excludes.push(rest.to_string());
        } else {
            includes.push(pattern.clone());
        }
    }

    let mut result: Vec<String> = if includes.is_empty() {
        all_paths.to_vec()
    } else {
        all_paths
            .iter()
            .filter(|file_path| matches_any_pattern(file_path, &includes, base_dir))
            .cloned()
            .collect()
    };

    if !excludes.is_empty() {
        result.retain(|file_path| !matches_any_pattern(file_path, &excludes, base_dir));
    }

    if !force_includes.is_empty() {
        for file_path in all_paths {
            if !result.iter().any(|existing| existing == file_path)
                && matches_any_exact_pattern(file_path, &force_includes, base_dir)
            {
                result.push(file_path.clone());
            }
        }
    }

    if !force_excludes.is_empty() {
        result.retain(|file_path| !matches_any_exact_pattern(file_path, &force_excludes, base_dir));
    }

    result.into_iter().collect()
}

/// `applyAutoloadDisabledPatterns(allPaths, patterns, baseDir)`：后写覆盖先写，键序保留。
///
/// 与 [`apply_patterns`] 不同，这里返回“命中文件 → 是否启用”的映射，而不是只返回
/// 启用集合；调用方可以区分“模式未命中”（`None`，应回退到默认启用）与“显式启用”。
/// 无前缀与 `!` 用 [`matches_any_pattern`]（glob），`+`/`-` 用
/// [`matches_any_exact_pattern`]（精确）；同一文件被多条模式命中时，`IndexMap::insert`
/// 更新值但保留该键第一次插入的位置，所以迭代顺序仍是首次命中的顺序。
pub(crate) fn apply_autoload_disabled_patterns(
    all_paths: &[String],
    patterns: &[String],
    base_dir: &str,
) -> IndexMap<String, bool> {
    let mut result: IndexMap<String, bool> = IndexMap::new();
    for pattern in patterns {
        let target =
            if pattern.starts_with('+') || pattern.starts_with('-') || pattern.starts_with('!') {
                pattern[1..].to_string()
            } else {
                pattern.clone()
            };
        let enabled = !pattern.starts_with('-') && !pattern.starts_with('!');
        let exact = pattern.starts_with('+') || pattern.starts_with('-');
        for file_path in all_paths {
            let matched = if exact {
                matches_any_exact_pattern(file_path, std::slice::from_ref(&target), base_dir)
            } else {
                matches_any_pattern(file_path, std::slice::from_ref(&target), base_dir)
            };
            if matched {
                result.insert(file_path.clone(), enabled);
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// MiniMatch（minimatch 10.2.6 默认选项的等价实现）
// ---------------------------------------------------------------------------

/// `.` 的 UTF-16 code unit。
const DOT: u16 = b'.' as u16;

/// POSIX 字符类（`[[:alpha:]]`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PosixClass {
    Alnum,
    Alpha,
    Ascii,
    Blank,
    Cntrl,
    Digit,
    Graph,
    Lower,
    Print,
    Punct,
    Space,
    Upper,
    Word,
    Xdigit,
}

impl PosixClass {
    /// `[:name:]` 的名字（不含方括号/冒号）。
    fn from_name(name: &[u16]) -> Option<Self> {
        let name = String::from_utf16(name).ok()?;
        Some(match name.as_str() {
            "alnum" => Self::Alnum,
            "alpha" => Self::Alpha,
            "ascii" => Self::Ascii,
            "blank" => Self::Blank,
            "cntrl" => Self::Cntrl,
            "digit" => Self::Digit,
            "graph" => Self::Graph,
            "lower" => Self::Lower,
            "print" => Self::Print,
            "punct" => Self::Punct,
            "space" => Self::Space,
            "upper" => Self::Upper,
            "word" => Self::Word,
            "xdigit" => Self::Xdigit,
            _ => return None,
        })
    }

    /// 字符类是否匹配该 code unit。
    ///
    /// 与 `minimatch@10.2.6` 的 `brace-expressions` 表逐项一致（JS 正则的 Unicode
    /// 属性语义）；Rust 的 `char::is_numeric` / `is_alphabetic` 等近似会接受
    /// `\p{No}`、`Other_Alphabetic` 等 minimatch 不接受的码点，因此这里用
    /// `regex` 的 `\p{...}` 精确求值。
    ///
    /// `Graph` 在 minimatch 里是**取反**类（源码为 `[^\p{Z}\p{C}]`），这里返回其
    /// 取反前集合（`Z ∪ C`）的成员关系，再由 [`ClassSpec`] 统一取反。
    fn matches(self, unit: u16) -> bool {
        let Some(character) = char::from_u32(u32::from(unit)) else {
            // 代理 code unit（astral 字符的一半）。JS 的 /u 模式按 code point 匹配，
            // 这里拿不到完整 code point；按 Cs（属于 C）处理。
            return matches!(self, Self::Graph | Self::Print);
        };
        let mut buffer = [0u8; 4];
        posix_class_regex(self).is_match(character.encode_utf8(&mut buffer))
    }
}

/// minimatch 10.2.6 `posixClasses` 表的正则形式，顺序与 [`PosixClass`] 的变体一致。
const POSIX_CLASS_PATTERNS: [&str; 14] = [
    r"^[\p{L}\p{Nl}\p{Nd}]$",       // Alnum
    r"^[\p{L}\p{Nl}]$",             // Alpha
    r"^[\x00-\x7f]$",               // Ascii
    r"^[\p{Zs}\t]$",                // Blank
    r"^[\p{Cc}]$",                  // Cntrl
    r"^[\p{Nd}]$",                  // Digit
    r"^[\p{Z}\p{C}]$",              // Graph（取反类：成员是 Z ∪ C）
    r"^[\p{Ll}]$",                  // Lower
    r"^[\p{C}]$",                   // Print
    r"^[\p{P}]$",                   // Punct
    r"^[\p{Z}\t\r\n\x0b\x0c]$",     // Space
    r"^[\p{Lu}]$",                  // Upper
    r"^[\p{L}\p{Nl}\p{Nd}\p{Pc}]$", // Word
    r"^[A-Fa-f0-9]$",               // Xdigit
];

/// 预编译 [`POSIX_CLASS_PATTERNS`]，下标与 [`PosixClass`] 的 `as usize` 一一对应；
/// 首次使用时初始化一次，之后每次字符类判定直接复用编译结果。
static POSIX_CLASS_REGEXES: std::sync::LazyLock<Vec<regex::Regex>> =
    std::sync::LazyLock::new(|| {
        POSIX_CLASS_PATTERNS
            .iter()
            .map(|pattern| regex::Regex::new(pattern).expect("valid POSIX class regex"))
            .collect()
    });

/// 按下标取预编译正则。`PosixClass` 的变体顺序必须与 [`POSIX_CLASS_PATTERNS`]
/// 保持一致，否则会把字符类判成另一个集合。
fn posix_class_regex(class: PosixClass) -> &'static regex::Regex {
    &POSIX_CLASS_REGEXES[class as usize]
}

/// 字符类里的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassItem {
    /// 单个字符。
    Char(u16),
    /// 闭区间 `a-z`。
    Range(u16, u16),
    /// POSIX 类。
    Posix(PosixClass),
}

impl ClassItem {
    fn matches(&self, unit: u16) -> bool {
        match self {
            Self::Char(value) => *value == unit,
            Self::Range(start, end) => *start <= unit && unit <= *end,
            Self::Posix(class) => class.matches(unit),
        }
    }
}

/// 解析后的字符类。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClassSpec {
    /// 外层取反（`[!...]` / `[^...]`）。
    negate: bool,
    /// 正向项。
    positives: Vec<ClassItem>,
    /// `[:graph:]` 这类本身带取反语义的 POSIX 类。
    negatives: Vec<PosixClass>,
}

impl ClassSpec {
    fn matches(&self, unit: u16) -> bool {
        let positive = self.positives.iter().any(|item| item.matches(unit));
        let negative = self.negatives.iter().any(|class| class.matches(unit));
        // minimatch 把正类与「取反类」分别编译成 `[P]` / `[^N]`（外层取反时交换），
        // 二者都有时拼成 `(sranges|snegs)`。
        if !self.positives.is_empty() && !self.negatives.is_empty() {
            if self.negate {
                !positive || negative
            } else {
                positive || !negative
            }
        } else if !self.positives.is_empty() {
            if self.negate { !positive } else { positive }
        } else if !self.negatives.is_empty() {
            if self.negate { negative } else { !negative }
        } else {
            false
        }
    }

    /// 单字符且不取反的类会折叠成字面量（minimatch 的 `hasMagic === false`）。
    fn single_literal(&self) -> Option<u16> {
        if self.negate || !self.negatives.is_empty() {
            return None;
        }
        match self.positives.as_slice() {
            [ClassItem::Char(value)] => Some(*value),
            _ => None,
        }
    }
}

/// extglob 的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtKind {
    /// `@(a|b)`
    At,
    /// `?(a|b)`
    Q,
    /// `*(a|b)`
    Star,
    /// `+(a|b)`
    Plus,
    /// `!(a|b)`
    Not,
}

/// 段内的一个原子。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Atom {
    /// 字面 UTF-16 code unit。
    Lit(u16),
    /// `?`
    Qmark,
    /// `*`（段内连续 `*` 合并为一个）。
    Star,
    /// `[...]`
    Class(ClassSpec),
    /// extglob。
    Ext(ExtKind, Vec<Vec<Atom>>),
}

/// pattern 的一段（`**` 走单独的 globstar 逻辑）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// 整段 `**`。
    Globstar,
    /// 其余段。
    Atoms(Vec<Atom>),
}

/// 一条 brace 展开后的 pattern（已按 `/` 切分并做 minimatch 的 levelOne 优化）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PatternSet {
    segments: Vec<Segment>,
}

/// minimatch 的 `*` / `?` / `**` / `{a,b}` / `[abc]` / `@(a|b)` 等子集。
///
/// 按 UTF-16 code unit 匹配（与 JS 正则的无 `u` 模式一致）；默认选项
/// `dot:false` / `nocase:false` / `nocomment:false` / `nonegate:false`。
#[derive(Debug, Clone)]
pub(crate) struct MiniMatch {
    sets: Vec<PatternSet>,
    negate: bool,
    comment: bool,
}

impl MiniMatch {
    /// 解析一个 pattern（不做 `~` 展开；调用方已用 `normalizePath` 处理）。
    pub(crate) fn new(pattern: &str) -> Self {
        // minimatch 的注释检查发生在剥离前导 `!` 之前。
        if pattern.starts_with('#') {
            return Self {
                sets: Vec::new(),
                negate: false,
                comment: true,
            };
        }
        let mut rest = pattern;
        let mut negate = false;
        while let Some(stripped) = rest.strip_prefix('!') {
            negate = !negate;
            rest = stripped;
        }
        let mut expanded = Vec::new();
        expand_braces(rest, &mut expanded);
        let mut sets: Vec<PatternSet> = Vec::new();
        for alternative in expanded {
            let set = build_pattern_set(&alternative);
            if !sets.contains(&set) {
                sets.push(set);
            }
        }
        Self {
            sets,
            negate,
            comment: false,
        }
    }

    /// 匹配一个路径。
    pub(crate) fn matches(&self, path: &str) -> bool {
        if self.comment {
            return false;
        }
        let path_parts = split_components(path);
        let matched = self
            .sets
            .iter()
            .any(|set| match_set(&set.segments, &path_parts));
        if self.negate { !matched } else { matched }
    }
}

/// 匹配 glob 遍历中的一个段（不做整条 pattern 的否定/注释处理）。
///
/// minimatch 只在整条 pattern 上解析前导 `!` 与 `#`；路径中间段里的它们是字面量。
fn match_glob_segment(pattern: &str, name: &str) -> bool {
    let set = build_pattern_set(pattern);
    match_set(&set.segments, &split_components(name))
}

/// JS `string.split(/\/+/)`：连续 `/` 折叠；首尾 `/` 产生空段。
fn split_components(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut previous_was_slash = false;
    let mut first = true;
    for character in text.chars() {
        if character == '/' {
            if first {
                parts.push(String::new());
            } else if !previous_was_slash {
                parts.push(std::mem::take(&mut current));
            }
            previous_was_slash = true;
        } else {
            previous_was_slash = false;
            current.push(character);
        }
        first = false;
    }
    if previous_was_slash {
        parts.push(String::new());
    } else {
        parts.push(current);
    }
    parts
}

/// minimatch 的 `levelOneOptimize`：合并相邻 `**`，消掉可回退的 `..`。
fn level_one_optimize(parts: Vec<String>) -> Vec<String> {
    let mut optimized: Vec<String> = Vec::new();
    for part in parts {
        if part == "**" && optimized.last().is_some_and(|last| last == "**") {
            continue;
        }
        if part == ".."
            && let Some(previous) = optimized.last()
            && previous != ".."
            && previous != "."
            && previous != "**"
        {
            optimized.pop();
            continue;
        }
        optimized.push(part);
    }
    if optimized.is_empty() {
        optimized.push(String::new());
    }
    optimized
}

/// 构造一条 pattern set：brace 展开后的整条 pattern。
fn build_pattern_set(pattern: &str) -> PatternSet {
    let parts = level_one_optimize(split_components(pattern));
    let segments = parts
        .into_iter()
        .map(|part| {
            if part == "**" {
                Segment::Globstar
            } else {
                Segment::Atoms(parse_segment(&part))
            }
        })
        .collect();
    PatternSet { segments }
}

/// 解析一段 pattern 为原子序列。
fn parse_segment(segment: &str) -> Vec<Atom> {
    let units: Vec<u16> = segment.encode_utf16().collect();
    let mut atoms = parse_atoms(&units, 0, false).0;
    // minimatch 把「整段只有一个空 extglob」当作字面量（`@()` / `?()` / `*()` / `+()`）。
    if let [Atom::Ext(kind, alternatives)] = atoms.as_slice()
        && *kind != ExtKind::Not
        && alternatives.len() == 1
        && alternatives[0].is_empty()
    {
        atoms = units.iter().map(|unit| Atom::Lit(*unit)).collect();
    }
    atoms
}

/// 解析直到字符串结尾或（`in_group` 时）顶层 `|` / `)`，返回原子序列与停止位置。
///
/// 识别顺序决定优先级：`\\` 转义先于一切；`*(`/`?(`/`+(`/`@(`/`!(` 先尝试按 extglob
/// 解析（未闭合则退回普通通配符或字面量处理）；`*` 合并相邻重复；`[` 只有在能解析出完整字符类时
/// 才成为 `Class`，否则按字面量 `[` 处理；其余 code unit 一律是 `Lit`。
fn parse_atoms(units: &[u16], start: usize, in_group: bool) -> (Vec<Atom>, usize) {
    let mut atoms = Vec::new();
    let mut index = start;
    while index < units.len() {
        let unit = units[index];
        if in_group && (unit == b'|' as u16 || unit == b')' as u16) {
            break;
        }
        if unit == b'\\' as u16 {
            if index + 1 < units.len() {
                atoms.push(Atom::Lit(units[index + 1]));
                index += 2;
            } else {
                atoms.push(Atom::Lit(unit));
                index += 1;
            }
            continue;
        }
        if unit == b'*' as u16 {
            if units.get(index + 1) == Some(&(b'(' as u16))
                && let Some((alternatives, next)) = parse_extglob(units, index)
            {
                push_ext(&mut atoms, ExtKind::Star, alternatives);
                index = next;
                continue;
            }
            if !matches!(atoms.last(), Some(Atom::Star)) {
                atoms.push(Atom::Star);
            }
            index += 1;
            continue;
        }
        if unit == b'?' as u16 {
            if units.get(index + 1) == Some(&(b'(' as u16))
                && let Some((alternatives, next)) = parse_extglob(units, index)
            {
                push_ext(&mut atoms, ExtKind::Q, alternatives);
                index = next;
                continue;
            }
            atoms.push(Atom::Qmark);
            index += 1;
            continue;
        }
        if (unit == b'+' as u16 || unit == b'@' as u16 || unit == b'!' as u16)
            && units.get(index + 1) == Some(&(b'(' as u16))
            && let Some((alternatives, next)) = parse_extglob(units, index)
        {
            let kind = match unit as u8 {
                b'+' => ExtKind::Plus,
                b'@' => ExtKind::At,
                _ => ExtKind::Not,
            };
            if kind == ExtKind::Not {
                let (kind, alternatives) = normalize_not_extglob(alternatives);
                push_ext(&mut atoms, kind, alternatives);
            } else {
                push_ext(&mut atoms, kind, alternatives);
            }
            index = next;
            continue;
        }
        if unit == b'[' as u16
            && let Some((spec, consumed)) = parse_class(units, index)
        {
            if let Some(literal) = spec.single_literal() {
                atoms.push(Atom::Lit(literal));
            } else {
                atoms.push(Atom::Class(spec));
            }
            index += consumed;
            continue;
        }
        atoms.push(Atom::Lit(unit));
        index += 1;
    }
    (atoms, index)
}

/// 合并相邻的同类 extglob 原子（minimatch 的 flatten 会把它们串起来；这里保持顺序）。
fn push_ext(atoms: &mut Vec<Atom>, kind: ExtKind, alternatives: Vec<Vec<Atom>>) {
    atoms.push(Atom::Ext(kind, alternatives));
}

/// 复刻 minimatch `AST#flatten` 对 `!(...)` 的改写。
///
/// minimatch 的 extglob AST 在生成正则前会做 adopt / adoptWithSpace / usurp：
/// - `!( @(x) )`：`!` 收养 `@`，否定体展开为 `x`；
/// - `!( ?(x) )`：`!` 收养 `?` 并补一个空分支；
/// - `!( !(x) )`：唯一分支是 `!` 时整个 `!` 被 usurp 成 `@`，即 `@(x)`；
/// - 若 `!` 的正文没有任何顶层字面量/分支（`#emptyExt`，来自解析器的
///   `acc === "" && ast.parts.length === 0`），正则退化为 `starNoEmpty`，
///   忽略正文——但被 usurp 后不再是 `!`，正文生效。
///
/// 返回改写后的 extglob 种类与分支。
fn normalize_not_extglob(mut alternatives: Vec<Vec<Atom>>) -> (ExtKind, Vec<Vec<Atom>>) {
    // 解析时就已经确定的 `#emptyExt`：minimatch 在遇到 `)` 时检查当前分支累积的
    // 字面文本 `acc === ""`。因此「最后一个分支为空」或「最后一个分支以 extglob
    // 结尾」时为 true（`!()`、`!(@(a))`、`!(a|@(b))`、`!(a|)`），否则为 false。
    let empty_ext = alternatives
        .last()
        .is_some_and(|last| last.is_empty() || matches!(last.last(), Some(Atom::Ext(_, _))));

    // minimatch 最多做 10 轮 flatten；收养会展开分支，需要重复到不动点。
    for _ in 0..10 {
        let mut changed = false;
        let mut index = 0;
        while index < alternatives.len() {
            let replacement = match alternatives[index].as_slice() {
                [Atom::Ext(ExtKind::At, inner)] => Some(inner.clone()),
                [Atom::Ext(ExtKind::Q, inner)] => {
                    let mut branches = inner.clone();
                    branches.push(Vec::new());
                    Some(branches)
                }
                _ => None,
            };
            match replacement {
                Some(branches) => {
                    alternatives.splice(index..=index, branches);
                    changed = true;
                }
                None => index += 1,
            }
        }
        if !changed {
            break;
        }
    }

    // `!( !(x) )` 的唯一分支是另一个 `!`：minimatch 把外层 usurp 成 `@`。
    if alternatives.len() == 1
        && let [Atom::Ext(ExtKind::Not, inner)] = alternatives[0].as_slice()
    {
        return (ExtKind::At, inner.clone());
    }

    if empty_ext {
        // `!()` 等价于 starNoEmpty：忽略（已收养的）正文。
        return (ExtKind::Not, vec![Vec::new()]);
    }
    (ExtKind::Not, alternatives)
}

/// 解析一个 extglob：`type(...)`，返回各分支与 `)` 之后的位置；未闭合返回 `None`。
fn parse_extglob(units: &[u16], start: usize) -> Option<(Vec<Vec<Atom>>, usize)> {
    let mut alternatives = Vec::new();
    let mut index = start + 2;
    loop {
        let (atoms, next) = parse_atoms(units, index, true);
        alternatives.push(atoms);
        if next >= units.len() {
            return None;
        }
        match units[next] {
            value if value == b')' as u16 => return Some((alternatives, next + 1)),
            _ => index = next + 1,
        }
    }
}

/// 解析 `[...]` 字符类；返回规范与消耗的 UTF-16 unit 数。
fn parse_class(units: &[u16], open: usize) -> Option<(ClassSpec, usize)> {
    let mut index = open + 1;
    let mut negate = false;
    if units.get(index) == Some(&(b'!' as u16)) || units.get(index) == Some(&(b'^' as u16)) {
        negate = true;
        index += 1;
    }
    let mut positives: Vec<ClassItem> = Vec::new();
    let mut negatives: Vec<PosixClass> = Vec::new();
    let mut saw_start = false;
    let mut escaping = false;
    let mut range_start: Option<u16> = None;
    let mut closed = false;
    while index < units.len() {
        let unit = units[index];
        if unit == b']' as u16 && saw_start && !escaping {
            closed = true;
            index += 1;
            break;
        }
        saw_start = true;
        if unit == b'\\' as u16 && !escaping {
            escaping = true;
            index += 1;
            continue;
        }
        if unit == b'[' as u16
            && !escaping
            && let Some((class, consumed)) = parse_posix_class(units, index)
        {
            if range_start.is_some() {
                // `[a-[:alpha:]]` 整个类失效（minimatch 返回 `$.`）。
                return Some((
                    ClassSpec {
                        negate: false,
                        positives: Vec::new(),
                        negatives: Vec::new(),
                    },
                    index + consumed - open,
                ));
            }
            index += consumed;
            match class {
                Ok(class) => positives.push(ClassItem::Posix(class)),
                Err(class) => negatives.push(class),
            }
            continue;
        }
        escaping = false;
        if let Some(start) = range_start.take() {
            if unit > start {
                positives.push(ClassItem::Range(start, unit));
            } else if unit == start {
                positives.push(ClassItem::Char(unit));
            }
            index += 1;
            continue;
        }
        if units.get(index + 1) == Some(&(b'-' as u16))
            && units.get(index + 2) == Some(&(b']' as u16))
        {
            positives.push(ClassItem::Char(unit));
            positives.push(ClassItem::Char(b'-' as u16));
            index += 2;
            continue;
        }
        if units.get(index + 1) == Some(&(b'-' as u16)) {
            range_start = Some(unit);
            index += 2;
            continue;
        }
        positives.push(ClassItem::Char(unit));
        index += 1;
    }
    if !closed {
        return None;
    }
    Some((
        ClassSpec {
            negate,
            positives,
            negatives,
        },
        index - open,
    ))
}

/// 解析 `[:name:]`；`Ok` 表示正向类，`Err` 表示 `[:graph:]` 这类自带取反的类。
///
/// minimatch 的表里只有 `graph` 带 `negated` 标记（`[^\p{Z}\p{C}]`）；`print` 是
/// 正向的 `\p{C}`（虽然与直觉相反，但这是 10.2.6 的实际语义）。
fn parse_posix_class(
    units: &[u16],
    start: usize,
) -> Option<(Result<PosixClass, PosixClass>, usize)> {
    if units.get(start + 1) != Some(&(b':' as u16)) {
        return None;
    }
    let mut index = start + 2;
    while index + 1 < units.len() {
        if units[index] == b':' as u16 && units[index + 1] == b']' as u16 {
            let name = &units[start + 2..index];
            let class = PosixClass::from_name(name)?;
            let negative = matches!(class, PosixClass::Graph);
            return Some((
                if negative { Err(class) } else { Ok(class) },
                index + 2 - start,
            ));
        }
        index += 1;
    }
    None
}

/// 序列是否在段首对 `.` / `..` 加 `startNoTraversal`（minimatch 的特殊分支）。
fn sequence_blocks_traversal(atoms: &[Atom]) -> bool {
    if !matches!(atoms.first(), Some(Atom::Lit(unit)) if *unit == DOT) {
        return false;
    }
    let wildcard_shaped = |atom: &Atom| matches!(atom, Atom::Star | Atom::Qmark | Atom::Class(_));
    match atoms.get(1) {
        Some(atom) if wildcard_shaped(atom) => true,
        Some(Atom::Lit(unit)) if *unit == DOT => atoms.get(2).is_some_and(wildcard_shaped),
        _ => false,
    }
}

/// 该段是否恰好是 `.` 或 `..`（UTF-16 code unit 形式）。
fn is_dot_or_dotdot(units: &[u16]) -> bool {
    units == [DOT] || units == [DOT, DOT]
}

/// 匹配一个段的原子序列。
fn match_segment(atoms: &[Atom], name: &str) -> bool {
    let units: Vec<u16> = name.encode_utf16().collect();
    // `noEmpty`：整段只有 `*` 时至少匹配一个字符（minimatch 的 starNoEmpty）。
    if units.is_empty() && !atoms.is_empty() && atoms.iter().all(|atom| matches!(atom, Atom::Star))
    {
        return false;
    }
    match_sequence(atoms, &units, 0, true, true)
}

/// 依次匹配原子序列与后续（`rest`）。
///
/// `at_start` 仅用于序列首原子的段首规则；一旦进入 `rest`，不再处于段首。
fn match_sequence(atoms: &[Atom], name: &[u16], pos: usize, at_start: bool, at_end: bool) -> bool {
    if at_start && pos < name.len() && name[pos] == DOT {
        match atoms.first() {
            None => return pos == name.len(),
            Some(Atom::Lit(unit)) => {
                if *unit != DOT {
                    return false;
                }
                if sequence_blocks_traversal(atoms) && is_dot_or_dotdot(&name[pos..]) {
                    return false;
                }
            }
            Some(Atom::Ext(ExtKind::Not, _)) => return false,
            Some(Atom::Ext(_, _)) => {}
            Some(_) => return false,
        }
    }
    let Some((first, rest)) = atoms.split_first() else {
        return pos == name.len();
    };
    match first {
        Atom::Lit(unit) => {
            pos < name.len()
                && name[pos] == *unit
                && match_sequence(rest, name, pos + 1, false, at_end)
        }
        Atom::Qmark => pos < name.len() && match_sequence(rest, name, pos + 1, false, at_end),
        Atom::Star => {
            let mut end = pos;
            loop {
                if match_sequence(rest, name, end, false, at_end) {
                    return true;
                }
                if end >= name.len() {
                    return false;
                }
                end += 1;
            }
        }
        Atom::Class(spec) => {
            pos < name.len()
                && spec.matches(name[pos])
                && match_sequence(rest, name, pos + 1, false, at_end)
        }
        Atom::Ext(kind, alternatives) => {
            let alternative_at_end = at_end && rest.is_empty();
            match kind {
                ExtKind::At => alternatives.iter().any(|alternative| {
                    match_then(
                        alternative,
                        rest,
                        name,
                        pos,
                        at_start,
                        alternative_at_end,
                        at_end,
                    )
                }),
                ExtKind::Q => {
                    match_sequence(rest, name, pos, false, at_end)
                        || alternatives.iter().any(|alternative| {
                            match_then(
                                alternative,
                                rest,
                                name,
                                pos,
                                at_start,
                                alternative_at_end,
                                at_end,
                            )
                        })
                }
                ExtKind::Star => {
                    match_repetition(alternatives, rest, name, pos, at_start, 0, at_end)
                }
                ExtKind::Plus => {
                    match_repetition(alternatives, rest, name, pos, at_start, 1, at_end)
                }
                ExtKind::Not => match_negation(
                    alternatives,
                    rest,
                    name,
                    pos,
                    at_start,
                    alternative_at_end,
                    at_end,
                ),
            }
        }
    }
}

/// `a` 从 `pos` 开始匹配后再匹配 `b`。
///
/// `a_at_end` 表示 `a` 是段的最后一个 AST 节点（minimatch 的 `isEnd`）；
/// 只有此时 `*` 才会编译成 `starNoEmpty`。
fn match_then(
    a: &[Atom],
    b: &[Atom],
    name: &[u16],
    pos: usize,
    at_start: bool,
    a_at_end: bool,
    b_at_end: bool,
) -> bool {
    if a.is_empty() {
        return match_sequence(b, name, pos, false, b_at_end);
    }
    if a_at_end && a.iter().all(|atom| matches!(atom, Atom::Star)) && name[pos..].is_empty() {
        return false;
    }
    let mut combined = Vec::with_capacity(a.len() + b.len());
    combined.extend_from_slice(a);
    combined.extend_from_slice(b);
    match_sequence(&combined, name, pos, at_start, b_at_end)
}

/// `*(...)` / `+(...)` 的重复匹配。
fn match_repetition(
    alternatives: &[Vec<Atom>],
    rest: &[Atom],
    name: &[u16],
    pos: usize,
    at_start: bool,
    minimum: usize,
    at_end: bool,
) -> bool {
    if minimum == 0 && match_sequence(rest, name, pos, false, at_end) {
        return true;
    }
    for end in (pos + 1)..=name.len() {
        for alternative in alternatives {
            if alternative.is_empty() {
                continue;
            }
            if match_sequence(alternative, &name[pos..end], 0, at_start, false)
                && match_repetition(
                    alternatives,
                    rest,
                    name,
                    end,
                    false,
                    minimum.saturating_sub(1),
                    at_end,
                )
            {
                return true;
            }
        }
    }
    false
}

/// `!(...)` 的负向匹配（含 minimatch 把后续模式并入否定体的语义）。
fn match_negation(
    alternatives: &[Vec<Atom>],
    rest: &[Atom],
    name: &[u16],
    pos: usize,
    at_start: bool,
    alternative_at_end: bool,
    at_end: bool,
) -> bool {
    // 空体 `!()`：等价于 starNoEmpty（至少一个字符，段首禁止点）。
    if alternatives.len() == 1 && alternatives[0].is_empty() {
        for end in (pos + 1)..=name.len() {
            if match_sequence(rest, name, end, false, at_end) {
                return true;
            }
        }
        return false;
    }
    // 负向先行断言：任一分支（连同后续模式）完整匹配剩余段则失败。
    for alternative in alternatives {
        if alternative_at_end
            && alternative.iter().all(|atom| matches!(atom, Atom::Star))
            && name[pos..].is_empty()
        {
            continue;
        }
        let mut combined = Vec::with_capacity(alternative.len() + rest.len());
        combined.extend_from_slice(alternative);
        combined.extend_from_slice(rest);
        if match_sequence(&combined, &name[pos..], 0, at_start, at_end) {
            return false;
        }
    }
    // 否定部分本身是 `[^/]*?`：吞掉任意长度后继续。
    for end in pos..=name.len() {
        if match_sequence(rest, name, end, false, at_end) {
            return true;
        }
    }
    false
}

/// 匹配一条 pattern set（含 `**` 递归）。
///
/// 路径已按 `/` 切成段。普通段逐段消耗；`**` 作为末段时要求剩余所有段都不以 `.`
/// 开头，作为中间段时可以吞掉零个或多个非 `.` 开头的段。pattern 消耗完时，path
/// 也必须消耗完，或只剩一个空段（对应路径末尾的 `/`）。
fn match_set(pattern: &[Segment], path: &[String]) -> bool {
    fn rec(pattern: &[Segment], path: &[String], pattern_index: usize, path_index: usize) -> bool {
        if pattern_index == pattern.len() {
            return path_index == path.len()
                || (path_index + 1 == path.len() && path[path_index].is_empty());
        }
        match &pattern[pattern_index] {
            Segment::Globstar => {
                if pattern_index + 1 == pattern.len() {
                    if path_index >= path.len() {
                        return false;
                    }
                    return path[path_index..]
                        .iter()
                        .all(|segment| !segment.starts_with('.'));
                }
                let mut index = path_index;
                loop {
                    if rec(pattern, path, pattern_index + 1, index) {
                        return true;
                    }
                    if index >= path.len() || path[index].starts_with('.') {
                        return false;
                    }
                    index += 1;
                }
            }
            Segment::Atoms(atoms) => {
                if path_index >= path.len() || !match_segment(atoms, &path[path_index]) {
                    return false;
                }
                rec(pattern, path, pattern_index + 1, path_index + 1)
            }
        }
    }
    rec(pattern, path, 0, 0)
}

// ---------------------------------------------------------------------------
// Brace expansion（brace-expansion@5 的等价实现）
// ---------------------------------------------------------------------------

/// brace-expansion 的 `EXPANSION_MAX`：防止恶意 pattern 撑爆内存。
const EXPANSION_MAX: usize = 100_000;

/// 展开 `{a,b}` / `{1..3}`；单分支保留花括号（但内部仍展开）。
///
/// 与 minimatch 的 `braceExpand` 一致：没有 `{...}` 时原样返回（不做转义处理）；
/// 一旦运行 brace-expansion，`\\` / `\{` / `\}` / `\,` / `\.` 会去掉反斜杠。
pub(crate) fn expand_braces(pattern: &str, out: &mut Vec<String>) {
    if !has_brace_pair(pattern) {
        out.push(pattern.to_string());
        return;
    }
    let mut results = Vec::new();
    expand_braces_rec(pattern, true, &mut results);
    if first_group_is_options(pattern) {
        results.retain(|value| !value.is_empty());
    }
    out.extend(results.into_iter().map(|value| unescape_braces(&value)));
}

/// minimatch 的 brace 预检：`/\{(?:(?!\{).)*\}/`（忽略转义）。
fn has_brace_pair(pattern: &str) -> bool {
    let units: Vec<u16> = pattern.encode_utf16().collect();
    let open = b'{' as u16;
    let close = b'}' as u16;
    for start in 0..units.len() {
        if units[start] != open {
            continue;
        }
        for unit in &units[start + 1..] {
            if *unit == open {
                break;
            }
            if *unit == close {
                return true;
            }
        }
    }
    false
}

/// `brace-expansion` 的 `unescapeBraces`：只去掉这五种转义的反斜杠。
fn unescape_braces(value: &str) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    let mut result = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        if units[index] == b'\\' as u16
            && index + 1 < units.len()
            && matches!(units[index + 1] as u8, b'\\' | b'{' | b'}' | b',' | b'.')
        {
            result.push(units[index + 1]);
            index += 2;
        } else {
            result.push(units[index]);
            index += 1;
        }
    }
    String::from_utf16_lossy(&result)
}

/// 第一个顶层 brace 组是否是逗号可选组。
fn first_group_is_options(pattern: &str) -> bool {
    let units: Vec<u16> = pattern.encode_utf16().collect();
    let Some((open, close)) = find_brace_group(&units) else {
        return false;
    };
    let body = &units[open + 1..close];
    if sequence_values(body).is_some() {
        return false;
    }
    has_top_level_comma(body)
}

/// [`expand_braces`] 的递归实现：每次处理第一个顶层 `{...}`。
///
/// 三类分支：
/// - 序列体（`{1..3}` / `{a..c}`）：逐个值替换后继续递归；
/// - 顶层逗号组：逐个分支替换后继续递归；
/// - 单分支：保留花括号本身，但分别递归展开花括号内部与后缀，再对两组结果做笛卡尔积。
///
/// `out` 达到 [`EXPANSION_MAX`] 时直接返回（截断结果而不是报错），防止恶意 pattern
/// 撑爆内存。`is_top` 目前不参与逻辑，只为与调用形态保持一致而保留。
fn expand_braces_rec(pattern: &str, is_top: bool, out: &mut Vec<String>) {
    let units: Vec<u16> = pattern.encode_utf16().collect();
    let Some((open, close)) = find_brace_group(&units) else {
        out.push(pattern.to_string());
        return;
    };
    let prefix = String::from_utf16_lossy(&units[..open]);
    let body = &units[open + 1..close];
    let suffix = String::from_utf16_lossy(&units[close + 1..]);
    if let Some(values) = sequence_values(body) {
        for value in values {
            if out.len() >= EXPANSION_MAX {
                return;
            }
            expand_braces_rec(&format!("{prefix}{value}{suffix}"), false, out);
        }
        return;
    }
    if has_top_level_comma(body) {
        for alternative in split_top_level_commas(body) {
            if out.len() >= EXPANSION_MAX {
                return;
            }
            let alternative = String::from_utf16_lossy(&alternative);
            expand_braces_rec(&format!("{prefix}{alternative}{suffix}"), false, out);
        }
        return;
    }
    // 单分支：保留花括号本身，但内部与后缀仍继续展开。
    let body_text = String::from_utf16_lossy(body);
    let mut inner_results = Vec::new();
    expand_braces_rec(&body_text, false, &mut inner_results);
    let mut suffix_results = Vec::new();
    expand_braces_rec(&suffix, false, &mut suffix_results);
    for inner in &inner_results {
        for suffix in &suffix_results {
            if out.len() >= EXPANSION_MAX {
                return;
            }
            out.push(format!("{prefix}{{{inner}}}{suffix}"));
        }
    }
    let _ = is_top;
}

/// 找到第一个顶层 `{...}`（跳过转义；嵌套深度参与匹配）。
fn find_brace_group(units: &[u16]) -> Option<(usize, usize)> {
    let open = {
        let mut index = 0;
        loop {
            if index >= units.len() {
                return None;
            }
            match units[index] {
                value if value == b'\\' as u16 => index += 2,
                value if value == b'{' as u16 => break index,
                _ => index += 1,
            }
        }
    };
    let mut depth = 0usize;
    let mut index = open;
    while index < units.len() {
        match units[index] {
            value if value == b'\\' as u16 => {
                index += 2;
                continue;
            }
            value if value == b'{' as u16 => depth += 1,
            value if value == b'}' as u16 => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((open, index));
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// 顶层是否有未转义的逗号。
fn has_top_level_comma(body: &[u16]) -> bool {
    let mut depth = 0usize;
    let mut index = 0;
    while index < body.len() {
        match body[index] {
            value if value == b'\\' as u16 => index += 2,
            value if value == b'{' as u16 => {
                depth += 1;
                index += 1;
            }
            value if value == b'}' as u16 => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            value if value == b',' as u16 && depth == 0 => return true,
            _ => index += 1,
        }
        if index > body.len() {
            break;
        }
    }
    false
}

/// 在顶层逗号处切分（保留转义与嵌套花括号）。
fn split_top_level_commas(body: &[u16]) -> Vec<Vec<u16>> {
    let mut alternatives = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0usize;
    let mut index = 0;
    while index < body.len() {
        match body[index] {
            value if value == b'\\' as u16 && index + 1 < body.len() => {
                current.push(value);
                current.push(body[index + 1]);
                index += 2;
            }
            value if value == b'{' as u16 => {
                depth += 1;
                current.push(value);
                index += 1;
            }
            value if value == b'}' as u16 => {
                depth = depth.saturating_sub(1);
                current.push(value);
                index += 1;
            }
            value if value == b',' as u16 && depth == 0 => {
                alternatives.push(std::mem::take(&mut current));
                index += 1;
            }
            value => {
                current.push(value);
                index += 1;
            }
        }
    }
    alternatives.push(current);
    alternatives
}

/// ASCII 数字串（可选前导 `-`，至少一位数字）。
fn is_integer_text(units: &[u16]) -> bool {
    let digits = if units.first() == Some(&(b'-' as u16)) {
        &units[1..]
    } else {
        units
    };
    !digits.is_empty()
        && digits
            .iter()
            .all(|unit| (b'0' as u16..=b'9' as u16).contains(unit))
}

/// `brace-expansion` 的 `isPadded`：`^-?0\d`。
fn is_padded(units: &[u16]) -> bool {
    let rest = if units.first() == Some(&(b'-' as u16)) {
        &units[1..]
    } else {
        units
    };
    rest.len() >= 2 && rest[0] == b'0' as u16 && (b'0' as u16..=b'9' as u16).contains(&rest[1])
}

/// `{1..3}` / `{a..c}` 序列展开。
fn sequence_values(body: &[u16]) -> Option<Vec<String>> {
    let parts: Vec<&[u16]> = split_sequence_parts(body);
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let step = match parts.get(2) {
        Some(step) => {
            if !is_integer_text(step) {
                return None;
            }
            Some(parse_integer(step)?)
        }
        None => None,
    };
    // 整数端点：展开方向由起止大小决定，步长取绝对值（步长 0 按 1 处理，
    // 与 `{1..3..0}` 展开为 1,2,3 的参考行为一致）；补零宽度取两个端点的较大长度。
    if parts.iter().take(2).all(|part| is_integer_text(part)) {
        let start = parse_integer(parts[0])?;
        let end = parse_integer(parts[1])?;
        let width = parts[0].len().max(parts[1].len());
        let padded = parts.iter().any(|part| is_padded(part));
        let increment = step.map_or(1, |value| value.unsigned_abs().max(1));
        let forward = start <= end;
        let mut values = Vec::new();
        let mut current = start;
        while ((forward && current <= end) || (!forward && current >= end))
            && values.len() < EXPANSION_MAX
        {
            values.push(format_integer(current, width, padded));
            current = if forward {
                current.saturating_add(increment as i64)
            } else {
                current.saturating_sub(increment as i64)
            };
            if increment == 0 {
                break;
            }
        }
        return Some(values);
    }
    // 字母序列：只允许单个 ASCII 字母端点。
    let single_letter = |part: &[u16]| {
        part.len() == 1
            && ((b'a' as u16..=b'z' as u16).contains(&part[0])
                || (b'A' as u16..=b'Z' as u16).contains(&part[0]))
    };
    if single_letter(parts[0]) && single_letter(parts[1]) {
        let start = i64::from(parts[0][0]);
        let end = i64::from(parts[1][0]);
        let increment = step.map_or(1, |value| value.unsigned_abs().max(1));
        let forward = start <= end;
        let mut values = Vec::new();
        let mut current = start;
        while ((forward && current <= end) || (!forward && current >= end))
            && values.len() < EXPANSION_MAX
        {
            let character = char::from_u32(u32::try_from(current).ok()?)?;
            values.push(if character == '\\' {
                String::new()
            } else {
                character.to_string()
            });
            current = if forward {
                current.saturating_add(increment as i64)
            } else {
                current.saturating_sub(increment as i64)
            };
            if increment == 0 {
                break;
            }
        }
        return Some(values);
    }
    None
}

/// 在顶层 `..` 处切分序列体。
fn split_sequence_parts(body: &[u16]) -> Vec<&[u16]> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    while index + 1 < body.len() {
        if body[index] == b'.' as u16 && body[index + 1] == b'.' as u16 {
            parts.push(&body[start..index]);
            index += 2;
            start = index;
        } else {
            index += 1;
        }
    }
    parts.push(&body[start..]);
    parts
}

/// ASCII 十进制文本转 `i64`；含非数字、空串或溢出时返回 `None`。
fn parse_integer(units: &[u16]) -> Option<i64> {
    String::from_utf16(units).ok()?.parse::<i64>().ok()
}

/// 把序列值格式化为文本：`padded` 为真时按 `width` 补零，负数的符号仍在前
/// （如 `-01` 而不是 `0-1`）；`padded` 为假时原样输出。
fn format_integer(value: i64, width: usize, padded: bool) -> String {
    let text = value.to_string();
    if !padded {
        return text;
    }
    let need = width.saturating_sub(text.len());
    if need == 0 {
        return text;
    }
    let zeros = "0".repeat(need);
    if value < 0 {
        format!("-{zeros}{}", &text[1..])
    } else {
        format!("{zeros}{text}")
    }
}

/// `minimatch(pattern)` 的便捷入口（默认选项、不做 `~` 展开）。
#[cfg(test)]
pub(crate) fn minimatch(path: &str, pattern: &str) -> bool {
    MiniMatch::new(pattern).matches(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimatch_matches_the_generated_reference_fixture() {
        let data = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/package/minimatch_golden.json"
        ));
        let cases: Vec<(String, String, serde_json::Value)> =
            serde_json::from_str(data).expect("parse fixture");
        assert!(!cases.is_empty());
        for (path, pattern, expected) in cases {
            let expected = expected.as_bool().expect("boolean fixture value");
            assert_eq!(
                mm(&path, &pattern),
                expected,
                "minimatch({path:?}, {pattern:?})"
            );
        }
    }

    #[test]
    fn brace_expansion_matches_brace_expansion_package() {
        let expand = |pattern: &str| {
            let mut values = Vec::new();
            expand_braces(pattern, &mut values);
            values
        };
        assert_eq!(expand("{01..3}"), vec!["01", "02", "03"]);
        assert_eq!(expand("{001..3}"), vec!["001", "002", "003"]);
        assert_eq!(expand("{1..03}"), vec!["01", "02", "03"]);
        assert_eq!(expand("{-01..3}"), vec!["-01", "000", "001", "002", "003"]);
        assert_eq!(expand("{01..3..2}"), vec!["01", "03"]);
        assert_eq!(
            expand("{9..1}"),
            vec!["9", "8", "7", "6", "5", "4", "3", "2", "1"]
        );
        assert_eq!(expand("{A..c}").len(), 35);
        assert_eq!(expand("{1..a}"), vec!["{1..a}"]);
        assert_eq!(expand("{a..1}"), vec!["{a..1}"]);
        assert_eq!(expand("{1..3..0}"), vec!["1", "2", "3"]);
        assert_eq!(expand("{,}"), Vec::<String>::new());
        assert_eq!(expand("{a,}"), vec!["a"]);
        assert_eq!(expand("a{,}"), vec!["a", "a"]);
        assert_eq!(
            expand("x{a,b}y{c,d}z"),
            vec!["xaycz", "xaydz", "xbycz", "xbydz"]
        );
        assert_eq!(expand("{a{b,c}}"), vec!["{ab}", "{ac}"]);
        assert_eq!(expand("{{a,b}}"), vec!["{a}", "{b}"]);
        assert_eq!(expand("{a\\,b,c}"), vec!["a,b", "c"]);
        assert_eq!(expand("\\{a,b\\}"), vec!["{a,b}"]);
        assert_eq!(expand("{a..\\}"), vec!["{a..}"]);
    }

    fn mm(path: &str, pattern: &str) -> bool {
        minimatch(path, pattern)
    }

    #[test]
    fn minimatch_matches_the_js_reference_table() {
        let cases = [
            ("extensions/keep.ts", "extensions", false),
            ("a/b/c.ts", "**/*.ts", true),
            ("c.ts", "**/*.ts", true),
            ("a/.h.ts", "**/*.ts", false),
            ("skills/bad-skill/SKILL.md", "**/bad-skill", false),
            ("bad-skill", "**/bad-skill", true),
            ("skills/bad-skill", "**/bad-skill", true),
            ("skills/bad-skill", "bad-skill", false),
            (".hidden.ts", "*.ts", false),
            (".hidden.ts", ".*.ts", true),
            ("nested/.hidden.ts", "**/.hidden.ts", true),
            ("a/b", "a/**", true),
            ("a", "a/**", false),
            ("a/b", "**", true),
            (".x", "**", false),
            ("a/b", "a/*", true),
            ("a/b/c", "a/*", false),
            ("a/x.ts", "{a,b}/*.ts", true),
            ("a/x.ts", "{a,b}/[xy].ts", true),
            ("a/x.ts", "{a,b}/[!y].ts", true),
            ("a/x.ts", "*/?.ts", true),
            ("#x", "#x", false),
            ("foo", "#foo", false),
            ("foo", "!foo", false),
            ("foo", "!bar", true),
            ("a/b", "a/**/b", true),
            ("a/x/y/b", "a/**/b", true),
            ("a/x", "a/**/", false),
            ("a/b/", "a/**/", true),
            ("foo/bar", "**/bar", true),
            ("foo/bar/baz", "**/bar", false),
            ("a//b", "a/**/b", true),
            ("a.ts", "**.ts", true),
            ("a/b.ts", "**.ts", false),
        ];
        for (path, pattern, expected) in cases {
            assert_eq!(
                mm(path, pattern),
                expected,
                "minimatch({path:?}, {pattern:?})"
            );
        }
    }

    #[test]
    fn brace_expansion_supports_ranges() {
        assert!(mm("b", "{a..c}"));
        assert!(mm("2", "{1..3}"));
        assert!(!mm("4", "{1..3}"));
    }

    #[test]
    fn apply_patterns_follows_the_four_step_order() {
        let all = vec![
            "/root/extensions/a.ts".to_string(),
            "/root/extensions/b.ts".to_string(),
            "/root/extensions/c.ts".to_string(),
        ];
        let patterns = vec![
            "!extensions/*.ts".to_string(),
            "+extensions/b.ts".to_string(),
        ];
        let enabled = apply_patterns(&all, &patterns, "/root");
        assert!(!enabled.contains("/root/extensions/a.ts"));
        assert!(enabled.contains("/root/extensions/b.ts"));
        assert!(!enabled.contains("/root/extensions/c.ts"));

        let no_patterns = apply_patterns(&all, &[], "/root");
        assert_eq!(no_patterns.len(), 3);
    }

    #[test]
    fn exact_pattern_expands_tilde_and_skill_parent() {
        let file = "/home/u/.kiana/skills/keep/SKILL.md";
        assert!(matches_any_exact_pattern(
            file,
            &["skills/keep".to_string()],
            "/home/u/.kiana"
        ));
        assert!(matches_any_exact_pattern(
            file,
            &["./skills/keep/SKILL.md".to_string()],
            "/home/u/.kiana"
        ));
    }

    #[test]
    fn precedence_ranks_match_the_table() {
        let metadata = |source: &str, scope: SourceScope, origin| PathMetadata {
            source: source.to_string(),
            scope,
            origin,
            base_dir: None,
            package_root: None,
        };
        use kiana_core::source_info::SourceOrigin;
        assert_eq!(
            resource_precedence_rank(&metadata(
                "local",
                SourceScope::Project,
                SourceOrigin::TopLevel
            )),
            0
        );
        assert_eq!(
            resource_precedence_rank(&metadata(
                "auto",
                SourceScope::Project,
                SourceOrigin::TopLevel
            )),
            1
        );
        assert_eq!(
            resource_precedence_rank(&metadata(
                "local",
                SourceScope::User,
                SourceOrigin::TopLevel
            )),
            2
        );
        assert_eq!(
            resource_precedence_rank(&metadata("auto", SourceScope::User, SourceOrigin::TopLevel)),
            3
        );
        assert_eq!(
            resource_precedence_rank(&metadata("npm:x", SourceScope::User, SourceOrigin::Package)),
            4
        );
        assert_eq!(
            resource_precedence_rank(&metadata(
                "builtin",
                SourceScope::User,
                SourceOrigin::TopLevel
            )),
            5
        );
    }

    #[test]
    fn minimatch_brace_expansion_only_expands_comma_or_range_groups() {
        // minimatch：`*.{json}` 不展开（单分支花括号保持字面量）。
        assert!(!mm("dark.json", "*.{json}"));
        assert!(mm("dark.json", "*.json"));
        assert!(mm("x.ts", "*.{ts,js}"));
        assert!(mm("x.js", "*.{ts,js}"));
        assert!(!mm("x.js", "*.{ts}"));
        // 单分支保留，但同级/嵌套的可展开组仍会展开。
        assert!(mm("{a}x.ts", "{a}{x,y}.ts"));
        assert!(mm("{a}y.ts", "{a}{x,y}.ts"));
        assert!(!mm("ab", "{a{b,c}}"));
        assert!(mm("{ab}", "{a{b,c}}"));
        assert!(mm("{ac}", "{a{b,c}}"));
        assert!(mm("a", "{a,b}"));
        assert!(mm("b", "{a..c}"));
        assert!(!mm("4", "{1..3}"));
    }

    #[test]
    fn minimatch_dot_rule_allows_single_dot_character_class() {
        // minimatch 把 `[.]` 折叠成字面量 `.`，但其它以 `[` 开头的类仍然禁止点开头。
        assert!(mm(".hidden.json", "[.]hidden.json"));
        assert!(mm(".hidden.json", "[.]*"));
        assert!(!mm(".hidden.json", "[._]*"));
        assert!(!mm(".hidden.json", "[a.]*"));
        assert!(!mm(".hidden.json", "?hidden.json"));
        assert!(!mm(".hidden.json", "*.json"));
        assert!(mm("a.b", "a[.]b"));
        assert!(mm(".hidden.json", ".*.json"));
    }

    #[test]
    fn minimatch_matches_minimatch_10_edge_cases() {
        // (a) 转义的前导点可以匹配 dotfile。
        assert!(mm(".a.ts", "\\.a.ts"));
        assert!(mm(".a", "\\.*"));
        // (b) minimatch 对 pattern 侧做 /+ 折叠与 levelOne `..` 归一；path 侧只折叠 /+。
        assert!(mm("a/b", "a//b"));
        assert!(mm("a/b", "a/../a/b"));
        assert!(!mm("a//b", "a/*/b"));
        // (c) 零填充数字区间按较长的端点补零。
        assert!(mm("01", "{01..3}"));
        assert!(mm("03", "{01..3}"));
        assert!(!mm("1", "{001..3}"));
        assert!(!mm("3", "{01..3}"));
        // (d) extglob 不再按字面量处理。
        assert!(mm("a.ts", "@(a|b).ts"));
        assert!(mm("b.ts", "@(a|b).ts"));
        assert!(!mm("c.ts", "@(a|b).ts"));
        assert!(mm("foo", "f+(o)"));
        assert!(mm("ac", "a!(b)c"));
        assert!(!mm("abc", "a!(b)c"));
        assert!(mm("a", "@(*)"));
        assert!(!mm("", "@(*)"));
        assert!(mm("", "?(*)"));
    }

    #[test]
    fn expand_glob_expands_zero_padded_brace_ranges() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for name in ["01.ts", "02.ts", "03.ts", "1.ts", "2.ts", "3.ts"] {
            write_file(&root.join(name), "");
        }
        assert_eq!(
            glob_relative(root, "{01..3}.ts"),
            vec!["01.ts", "02.ts", "03.ts"]
        );
        assert_eq!(glob_relative(root, "{001..3}.ts"), Vec::<String>::new());
        // Node fs.glob 的 `windowsPathsNoEscape: true`：反斜杠先变成 `/`。
        std::fs::create_dir_all(root.join("a")).expect("mkdir");
        write_file(&root.join("a/x.ts"), "");
        assert_eq!(glob_relative(root, "a\\?.ts"), vec!["a/x.ts"]);
    }

    fn glob_relative(dir: &std::path::Path, pattern: &str) -> Vec<String> {
        let root = dir.to_string_lossy().into_owned();
        expand_package_glob(pattern, &root)
            .expect("glob")
            .into_iter()
            .map(|path| relative(&root, &path))
            .collect()
    }

    fn write_file(path: &std::path::Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    #[test]
    fn expand_glob_trailing_globstar_matches_files_like_node() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_file(&root.join("extensions/a.ts"), "");
        write_file(&root.join("extensions/b.ts"), "");
        write_file(&root.join("extensions/keep.tmp"), "");
        write_file(&root.join("extensions/sub/c.ts"), "");

        // Node `fs.globSync("extensions/**")` 返回目录本身与全部后代（含顶层文件）。
        assert_eq!(
            glob_relative(root, "extensions/**"),
            vec![
                "extensions",
                "extensions/a.ts",
                "extensions/b.ts",
                "extensions/keep.tmp",
                "extensions/sub",
                "extensions/sub/c.ts",
            ]
        );
        assert_eq!(
            glob_relative(root, "extensions/**/*.ts"),
            vec!["extensions/a.ts", "extensions/b.ts", "extensions/sub/c.ts",]
        );
        // 末尾 `/` 只返回目录。
        assert_eq!(glob_relative(root, "extensions/*/"), vec!["extensions/sub"]);
    }

    #[test]
    fn expand_glob_expands_braces_like_node() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_file(&root.join("ext/a.ts"), "");
        write_file(&root.join("ext/b.ts"), "");
        write_file(&root.join("ext/c.ts"), "");
        write_file(&root.join("ext/d.js"), "");

        assert_eq!(
            glob_relative(root, "ext/{a,b}*.ts"),
            vec!["ext/a.ts", "ext/b.ts"]
        );
        assert_eq!(
            glob_relative(root, "ext/*.{ts,js}"),
            vec!["ext/a.ts", "ext/b.ts", "ext/c.ts", "ext/d.js"]
        );
        // 单分支花括号不展开：`*.{json}` 匹配不到 `.json` 文件。
        assert!(glob_relative(root, "ext/*.{json}").is_empty());
    }

    #[test]
    fn expand_glob_handles_non_segment_globstars_and_malformed_patterns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_file(&root.join("a"), "");
        write_file(&root.join("xxa"), "");
        write_file(&root.join("a.ts"), "");

        // `**` 只在整段时才是递归通配；段内 `**` 等同 `*`。
        assert_eq!(glob_relative(root, "**a"), vec!["a", "xxa"]);
        assert_eq!(glob_relative(root, "a**"), vec!["a", "a.ts"]);
        assert!(glob_relative(root, "**a/**").is_empty());
        // 非法字符类返回空列表（Node 不抛错）。
        assert!(glob_relative(root, "[").is_empty());
        assert!(glob_relative(root, "a[").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn expand_glob_does_not_follow_symlinks_discovered_by_globstar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_file(&root.join("plugins/real/skills/SKILL.md"), "x");
        std::os::unix::fs::symlink(root.join("plugins/real"), root.join("plugins/linked"))
            .expect("symlink");

        // `**` 发现链接目录时只把它当作结果，不进入其中。
        assert_eq!(
            glob_relative(root, "plugins/**"),
            vec![
                "plugins",
                "plugins/linked",
                "plugins/real",
                "plugins/real/skills",
                "plugins/real/skills/SKILL.md",
            ]
        );
        assert_eq!(
            glob_relative(root, "plugins/**/*.md"),
            vec!["plugins/real/skills/SKILL.md"]
        );
        // 但显式字面量命名的链接目录会继续遍历（stat 跟随链接）。
        assert_eq!(
            glob_relative(root, "plugins/linked/**"),
            vec![
                "plugins/linked",
                "plugins/linked/skills",
                "plugins/linked/skills/SKILL.md",
            ]
        );
        // 魔法段本身也不下探链接目录。
        assert_eq!(
            glob_relative(root, "plugins/*/skills"),
            vec!["plugins/real/skills"]
        );
    }

    #[test]
    fn expand_glob_normalizes_dot_segments_and_supports_absolute_patterns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_file(&root.join("a/b/c.ts"), "");
        assert_eq!(glob_relative(root, "./a/./b/*.ts"), vec!["a/b/c.ts"]);
        // `./` 与 `.` 返回空（Node 的 glob 不产生起始目录）。
        assert!(glob_relative(root, ".").is_empty());
        assert!(glob_relative(root, "./").is_empty());

        let absolute_pattern = format!("{}/a/b/*.ts", root.to_string_lossy());
        let matches =
            expand_package_glob(&absolute_pattern, &root.to_string_lossy()).expect("absolute glob");
        assert_eq!(matches.len(), 1);
        assert!(matches[0].ends_with("a/b/c.ts"));
    }

    #[test]
    fn expand_glob_sorts_and_skips_dot_segments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let files = dir.path().join("extension-files");
        std::fs::create_dir_all(files.join("nested")).expect("mkdir");
        std::fs::write(files.join("z.ts"), "").expect("write");
        std::fs::write(files.join("a.ts"), "").expect("write");
        std::fs::write(files.join(".ignored.ts"), "").expect("write");
        std::fs::write(files.join("nested").join(".hidden.ts"), "").expect("write");
        let root = dir.path().to_string_lossy().into_owned();
        let matches = expand_package_glob("./extension-files/*.ts", &root).expect("glob");
        let relative: Vec<String> = matches.iter().map(|path| relative(&root, path)).collect();
        assert_eq!(
            relative,
            vec!["extension-files/a.ts", "extension-files/z.ts"]
        );

        // `**` 可以匹配零个目录，但 `.ignored.ts` 以 `.` 开头，仍被隐藏文件规则过滤。
        let hidden = expand_package_glob("./extension-files/**/.ignored.ts", &root).expect("glob");
        assert!(hidden.is_empty());

        // 与 TS 的过滤一致：即使 glob 能匹配到，含 `.` 开头段的路径也被丢弃。
        let literal_dot =
            expand_package_glob("./extension-files/nested/.hidden.ts", &root).expect("glob");
        assert!(literal_dot.is_empty());
    }
}
