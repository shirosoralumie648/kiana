//! `load_skills_from_dir_internal` / `load_skill_from_file`。
//!
//! 逐字移植 `packages/coding-agent/src/core/skills.ts#loadSkillsFromDirInternal` /
//! `#loadSkillFromFile` / ignore 辅助函数（`toPosixPath` / `prefixIgnorePattern` /
//! `addIgnoreRules`）。
//!
//! ignore 规则使用 `crate::skills::ignore_match` 复刻 npm `ignore` 7.0.8 的语义
//! （`ignore` crate 的 glob 解析与其不同）：每条模式先按 `prefixIgnorePattern` 加上相对
//! root 的目录前缀，再交给 matcher（`ca-resources.md` §8.4 第 4 条）。
//!
//! # 为什么需要这个文件
//! `crate::skills` 的上层只决定“扫哪些目录、如何按名字去重”，本文件承担一次目录扫描的
//! 全部文件系统细节：逐层读取 ignore 文件、递归子目录、跟随符号链接、识别 `SKILL.md`、
//! 解析单个 Markdown 的 frontmatter。拆出来是为了能按 TS 的
//! `loadSkillsFromDirInternal` / `loadSkillFromFile` 分段对照。
//!
//! # 调用关系
//! - `crate::skills::load_skills_from_dir`：普通单目录入口（`include_root_files = true`，
//!   不传已有 ignore 状态）。
//! - `crate::skills::load_skills`：对默认目录和显式目录路径调用；显式 `.md` 文件则直接
//!   调用 [`load_skill_from_file`]。
//! 本文件不做同名去重：同一批结果里可以出现同名 `Skill`（来自不同子目录），冲突检测由
//! `crate::skills::load_skills` 完成。
//!
//! # 一次目录扫描的顺序
//! 1. 目录不存在直接返回空结果；否则先读取本层 ignore 规则，再列目录。
//! 2. `read_dir` 的条目按文件名排序，对齐 Node `readdirSync`（libuv `uv_fs_scandir`）
//!    返回的顺序。
//! 3. 第一遍找本目录的 `SKILL.md`：命中即加载并**立即返回**，不再递归——所以目录根的
//!    技能优先于其子目录里的技能。
//! 4. 第二遍遍历其余条目：跳过点开头的隐藏条目与 `node_modules`，按累积的 ignore 规则
//!    过滤；目录递归（此后 `include_root_files = false`），普通文件只在允许根散装文件时
//!    加载 `.md`。
//!
//! # 注意事项
//! - 所有 IO 失败都降级为“跳过”或 warning，函数本身没有 `Result`：目录不可读、条目的
//!   `file_type()` 失败、ignore 文件读失败都不会中止扫描。
//! - 符号链接通过 `fs::metadata` 跟随目标；断链被静默跳过（`continue`）。
//! - 参与 ignore 匹配的路径统一先转成相对 root 的 POSIX 路径。

use crate::skills::ignore_match::NpmIgnore;
use kiana_core::diagnostics::ResourceDiagnostic;
use kiana_core::frontmatter::parse_frontmatter;
use serde_json::Value;

use crate::skills::validate::{validate_description, validate_name};
use crate::skills::{
    IGNORE_FILES, LoadSkillsResult, Skill, SkillFrontmatter, basename, create_skill_source_info,
    dirname, join_path, node_read_error_message, relative_path, warning,
};

/// 累积的 ignore 规则（每个递归层级追加）。
///
/// 一次扫描（一个 root）共用一个实例：进入某目录时，把该层 `IGNORE_FILES` 里的规则按
/// “相对 root 加前缀”后追加进来，因此子目录规则只作用于它的子树。`root` 是扫描起点
/// （`load_skills_from_dir_internal` 的 `root_dir`），递归过程中保持不变。
pub(crate) struct IgnoreState {
    root: String,
    matcher: NpmIgnore,
}

impl IgnoreState {
    fn new(root: &str) -> Self {
        IgnoreState {
            root: root.to_string(),
            matcher: NpmIgnore::new(),
        }
    }

    /// TS: `addIgnoreRules(ig, dir, rootDir)`。
    ///
    /// 依次读取 `IGNORE_FILES` 中的每个文件（顺序固定），把每行改写成相对 root 的模式后
    /// 追加到共享 matcher。规则只增不减：后加入的规则可以否定先前的规则，这正是 npm
    /// `ignore` 的“后者覆盖前者”语义。
    ///
    /// 文件不存在或读取失败时静默跳过：ignore 文件是可选配置，缺失不应让扫描失败。
    /// 内容按 `from_utf8_lossy` 解码，与 Node 读文本时的替换字符行为一致。
    fn add_rules(&mut self, dir: &str) {
        let relative_dir = relative_path(&self.root, dir);
        let prefix = if relative_dir.is_empty() {
            String::new()
        } else {
            format!("{}/", to_posix_path(&relative_dir))
        };

        for filename in IGNORE_FILES {
            let ignore_path = join_path(dir, filename);
            if !std::path::Path::new(&ignore_path).exists() {
                continue;
            }
            let Ok(bytes) = std::fs::read(&ignore_path) else {
                continue;
            };
            let content = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = content.split('\n').collect();
            let mut patterns: Vec<String> = Vec::new();
            for (index, line) in lines.iter().enumerate() {
                // `split(/\r?\n/)`：只有后接 `\n` 的 `\r` 属于分隔符；文件末尾孤立的
                // `\r` 是普通字符。
                let line = if index + 1 < lines.len() {
                    line.strip_suffix('\r').unwrap_or(line)
                } else {
                    line
                };
                if let Some(pattern) = prefix_ignore_pattern(line, &prefix) {
                    patterns.push(pattern);
                }
            }
            if !patterns.is_empty() {
                self.matcher.add(&patterns);
            }
        }
    }

    /// TS: `ig.ignores(relPath)`。目录路径带尾部 `/`（`isDirectory ? `${relPath}/` : relPath`）。
    ///
    /// `rel_path` 必须是相对 `root` 的 POSIX 路径；目录必须补尾斜杠，否则 `foo/` 这类
    /// 目录规则不会命中（`NpmIgnore::ignores` 的匹配要求）。
    fn ignores(&self, rel_path: &str, is_dir: bool) -> bool {
        if is_dir {
            self.matcher.ignores(&format!("{rel_path}/"))
        } else {
            self.matcher.ignores(rel_path)
        }
    }
}

/// `p.split(sep).join("/")`。
///
/// 只替换当前平台的 `MAIN_SEPARATOR`：Windows 上把 `\` 换成 `/`，Unix 上是恒等变换。
/// 因此 Unix 路径里字面的 `\` 不会被改写（与 TS 在 POSIX 平台上的行为一致）。
fn to_posix_path(path: &str) -> String {
    path.split(std::path::MAIN_SEPARATOR)
        .collect::<Vec<_>>()
        .join("/")
}

/// TS: `skills.ts#prefixIgnorePattern`。
///
/// 把一行 ignore 文本改写成“相对扫描 root”的模式，步骤与 TS 一一对应：
/// 1. 用 JS `trim` 后的副本判断空行/注释（`#` 开头且不是 `\#`）→ 丢弃；
/// 2. 前导 `!` 表示否定，先摘下，最后再拼回最前面；
/// 3. 前导 `\!` 只去掉反斜杠，留下字符 `!`；若此时 `prefix` 为空，返回的模式以 `!` 开头，
///    matcher 会把它当否定规则（回归测试 `escaped_bang_ignore_rules_keep_the_literal_bang`
///    锁定）；在嵌套目录里它会被拼成 `sub/!`，就不再是否定前缀了；
/// 4. 前导 `/` 去掉：npm 里它表示“锚定 root”，而这里的模式本来就从 root 起算；
/// 5. 非空 `prefix`（形如 `sub/`）前接。
///
/// 注意判空/判注释用的是 trim 后的副本，实际模式却取自原始 `line`：行内空白会保留，
/// 这是 TS 的逐字行为。
fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = kiana_json::js_string::js_trim(line);
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("\\#") {
        return None;
    }

    let mut pattern = line.to_string();
    let mut negated = false;

    if let Some(rest) = pattern.strip_prefix('!') {
        negated = true;
        pattern = rest.to_string();
    } else if pattern.starts_with("\\!") {
        // TS: `pattern = pattern.slice(1)` —— 只去掉反斜杠，`!` 留在模式开头；
        // 返回后 matcher 会把开头的 `!` 解析为否定规则（tests 的 `\!x/` 用例）。
        pattern = pattern[1..].to_string();
    }

    if let Some(rest) = pattern.strip_prefix('/') {
        pattern = rest.to_string();
    }

    let prefixed = if prefix.is_empty() {
        pattern
    } else {
        format!("{prefix}{pattern}")
    };
    Some(if negated {
        format!("!{prefixed}")
    } else {
        prefixed
    })
}

/// `loadSkillsFromDirInternal(dir, source, includeRootFiles, ignoreMatcher?, rootDir?)`。
///
/// 递归扫描 `dir` 并合并结果。参数含义：
/// - `source`：原样传给 [`load_skill_from_file`]，用于构造技能的 `source_info`；
/// - `include_root_files`：是否把本层散装 `.md` 当作技能；只有最外层为 `true`，递归调用
///   一律传 `false`（子目录只认 `SKILL.md`）；
/// - `ignore_matcher` / `root_dir`：递归内部使用。根调用传 `None`，函数用
///   `root_dir.unwrap_or(dir)` 建状态；递归调用传同一个状态，使 ignore 规则跨层级累积。
///
/// 没有错误通道：目录不存在或不可读都返回空结果。返回值保持发现顺序，且**不**按名字去重。
pub(crate) fn load_skills_from_dir_internal(
    dir: &str,
    source: &str,
    include_root_files: bool,
    ignore_matcher: Option<&mut IgnoreState>,
    root_dir: Option<&str>,
) -> LoadSkillsResult {
    let mut skills: Vec<Skill> = Vec::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    if !std::path::Path::new(dir).exists() {
        return LoadSkillsResult {
            skills,
            diagnostics,
        };
    }

    let root = root_dir.unwrap_or(dir).to_string();
    let mut owned_state;
    let state: &mut IgnoreState = match ignore_matcher {
        Some(state) => state,
        None => {
            owned_state = IgnoreState::new(&root);
            &mut owned_state
        }
    };
    // 先读规则再列目录：本层 `SKILL.md` 也要受本层 ignore 规则约束。
    state.add_rules(dir);

    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return LoadSkillsResult {
            skills,
            diagnostics,
        };
    };
    // Node 的 `readdirSync` 由 libuv `uv_fs_scandir` 按文件名（strcmp）排序后返回。
    // 这里用 `String` 的排序：它按 UTF-8 字节比较；文件名是合法 UTF-8 时与 strcmp 的
    // 逐字节顺序一致（文件名含非法 UTF-8 时，`to_string_lossy` 的替换字符可能使顺序
    // 偏离 Node）。
    let mut entries: Vec<(String, std::fs::FileType)> = Vec::new();
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        match entry.file_type() {
            Ok(file_type) => entries.push((name, file_type)),
            Err(_) => continue,
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // 第一遍：目录里的 SKILL.md 是技能根；加载后立即返回，不再递归。
    //
    // 一个目录最多只有一个名为 `SKILL.md` 的条目，所以“命中即返回”等价于“本目录有
    // SKILL.md 时，子目录里的技能全部不参与本次扫描”。
    for (name, file_type) in &entries {
        if name != "SKILL.md" {
            continue;
        }
        let full_path = join_path(dir, name);

        // 符号链接不能只信 `file_type`：用 `fs::metadata` 跟随目标重新判定；断链或
        // 目标不可访问时静默跳过。
        let mut is_file = file_type.is_file();
        if file_type.is_symlink() {
            match std::fs::metadata(&full_path) {
                Ok(metadata) => is_file = metadata.is_file(),
                Err(_) => continue,
            }
        }

        let rel_path = to_posix_path(&relative_path(&root, &full_path));
        if !is_file || state.ignores(&rel_path, false) {
            continue;
        }

        let (skill, skill_diagnostics) = load_skill_from_file(&full_path, source);
        if let Some(skill) = skill {
            skills.push(skill);
        }
        diagnostics.extend(skill_diagnostics);
        return LoadSkillsResult {
            skills,
            diagnostics,
        };
    }

    // 第二遍：递归子目录；根目录的散装 .md 仅在 includeRootFiles 时加载。
    //
    // 点开头的隐藏条目与 `node_modules` 整棵跳过：这是硬编码规则，不受 ignore 文件影响，
    // 也不会被告警或诊断。
    for (name, file_type) in &entries {
        if name.starts_with('.') {
            continue;
        }
        if name == "node_modules" {
            continue;
        }

        let full_path = join_path(dir, name);

        // 与第一遍相同：符号链接先跟随目标，再决定按目录递归还是按文件加载。
        let mut is_directory = file_type.is_dir();
        let mut is_file = file_type.is_file();
        if file_type.is_symlink() {
            match std::fs::metadata(&full_path) {
                Ok(metadata) => {
                    is_directory = metadata.is_dir();
                    is_file = metadata.is_file();
                }
                Err(_) => continue,
            }
        }

        let rel_path = to_posix_path(&relative_path(&root, &full_path));
        if state.ignores(&rel_path, is_directory) {
            continue;
        }

        if is_directory {
            let sub =
                load_skills_from_dir_internal(&full_path, source, false, Some(state), Some(&root));
            skills.extend(sub.skills);
            diagnostics.extend(sub.diagnostics);
            continue;
        }

        // 递归层拿到的 `include_root_files` 恒为 `false`，所以子目录里的普通 `.md`
        // 不会被当作技能；只有本次扫描的最外层（或其根调用）允许散装 Markdown。
        if !is_file || !include_root_files || !name.ends_with(".md") {
            continue;
        }

        let (skill, skill_diagnostics) = load_skill_from_file(&full_path, source);
        if let Some(skill) = skill {
            skills.push(skill);
        }
        diagnostics.extend(skill_diagnostics);
    }

    LoadSkillsResult {
        skills,
        diagnostics,
    }
}

/// 单个文件的加载结果。TS: `loadSkillFromFile` 的返回形状。
///
/// 第一个元素为 `None` 表示该文件没有产生技能（缺 description、读失败、frontmatter
/// 解析失败等）；第二个元素即使技能加载失败也可能非空——调用方必须两条一起处理，
/// 不能因为拿不到技能就丢掉诊断。
pub type LoadedSkill = (Option<Skill>, Vec<ResourceDiagnostic>);

/// 从单个 Markdown 文件加载技能。TS: `skills.ts#loadSkillFromFile`。
///
/// 有 description 但 name 非法时**仍然加载**（只发 warning）；description 缺失/为空时不加载。
///
/// 判定顺序固定：
/// 1. 读文件失败 → 一条 warning，返回 `(None, ...)`；
/// 2. frontmatter 解析失败 → 只有文件名恰好是 `SKILL.md` 时才发 warning，普通 `.md`
///    静默返回；
/// 3. 非 `SKILL.md` 且没有非空 description → 直接返回，不产生任何诊断；
/// 4. description 校验错误逐条转 warning（缺 description 时先得到
///    “description is required”）；
/// 5. name 取 frontmatter 里的非空字符串，否则退回父目录名，再逐条校验转 warning；
/// 6. 最终仍无 description → 返回 `(None, ...)`，否则构造技能。
///
/// `file_path` 原样写入 `Skill::file_path`，`base_dir` 取它的 `dirname`；`description`
/// 保留原始文本（不 trim）；`disable_model_invocation` 只在 YAML 值为布尔 `true` 时为真。
pub fn load_skill_from_file(file_path: &str, source: &str) -> LoadedSkill {
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let is_declared_skill = basename(file_path) == "SKILL.md";

    let bytes = match std::fs::read(file_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            diagnostics.push(warning(
                node_read_error_message(&error, "open", file_path),
                file_path,
            ));
            return (None, diagnostics);
        }
    };
    // Node 的 `readFileSync(path, "utf-8")` 对非法 UTF-8 使用替换字符。
    let raw_content = String::from_utf8_lossy(&bytes);

    let parsed = match parse_frontmatter(&raw_content) {
        Ok(parsed) => parsed,
        Err(error) => {
            if is_declared_skill {
                diagnostics.push(warning(error.to_string(), file_path));
            }
            return (None, diagnostics);
        }
    };

    let frontmatter = SkillFrontmatter::from_map(&parsed.frontmatter);
    let description = frontmatter.description.clone();
    let has_description = description
        .as_ref()
        .and_then(Value::as_str)
        .is_some_and(|text| !kiana_json::js_string::js_trim(text).is_empty());

    if !is_declared_skill && !has_description {
        return (None, diagnostics);
    }

    let skill_dir = dirname(file_path);
    let parent_dir_name = basename(&skill_dir);

    for error in validate_description(description.as_ref()) {
        diagnostics.push(warning(error, file_path));
    }

    let frontmatter_name = frontmatter.name.as_ref().and_then(Value::as_str);
    let name = frontmatter_name
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or(parent_dir_name);

    for error in validate_name(&name) {
        diagnostics.push(warning(error, file_path));
    }

    if !has_description {
        return (None, diagnostics);
    }

    let description = description
        .as_ref()
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let skill = Skill {
        name,
        description,
        file_path: file_path.to_string(),
        base_dir: skill_dir.clone(),
        source_info: create_skill_source_info(file_path, &skill_dir, source),
        disable_model_invocation: frontmatter.disable_model_invocation == Some(Value::Bool(true)),
    };

    (Some(skill), diagnostics)
}
