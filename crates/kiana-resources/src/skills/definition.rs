//! 扩展技能定义：frontmatter 元数据、正文与条件可见性。
//!
//! `kiana_ext_api::system_prompt::Skill` 是冻结契约定（只有 name/description/file_path/
//! base_dir/source_info/disable_model_invocation）。本模块在旁边增加一层
//! [`SkillDefinition`]：保留正文与扩展 frontmatter，供 `skill` 工具按需加载；不修改
//! `Skill` 本身，因此现有调用方（session / system prompt）零改动。
//!
//! # frontmatter 扩展
//!
//! 参考 Claude Code `loadSkillsDir.ts` 与 oh-my-pi `extensibility/skills.ts`：
//!
//! - `allowed-tools`：激活期间授予的权限规则（`Bash(git status)` / `Read(src/**)` / 工具名）；
//! - `model` / `effort`：激活期间覆盖模型与 thinking effort；
//! - `context: inline|fork` + `agent`：正文在主会话执行还是 fork 子代理；
//! - `hooks`：激活期间安装的 hooks 配置（原样交给 hooks 层）；
//! - `paths`：条件可见性 glob（没碰过匹配文件前不进入系统提示索引）；
//! - `user-invocable` / `disable-model-invocation`：用户/模型两条触发通道；
//! - `arguments` / `argument-hint`：参数声明与补全提示。
//!
//! 未知字段仍留在 [`crate::skills::SkillFrontmatter::extra`]，不报错。

use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde_json::Value;

use kiana_core::diagnostics::ResourceDiagnostic;
use kiana_core::frontmatter::parse_frontmatter;

use crate::skills::discover::load_skill_from_file;
use crate::skills::{
    LoadSkillsFromDirOptions, LoadSkillsOptions, Skill, SkillFrontmatter, load_skills,
    load_skills_from_dir,
};

/// 技能正文在 `inline`（默认）还是 `fork` 子代理里执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillContext {
    /// 主会话注入。
    #[default]
    Inline,
    /// fork 一个子代理执行（`agent` 指定代理名）。
    Fork,
}

impl SkillContext {
    /// frontmatter 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            SkillContext::Inline => "inline",
            SkillContext::Fork => "fork",
        }
    }

    /// 解析：只有 `"fork"` 是 fork，其它值（含缺失/非法）都是 inline。
    pub fn from_value(value: Option<&Value>) -> Self {
        match value.and_then(Value::as_str) {
            Some("fork") => SkillContext::Fork,
            _ => SkillContext::Inline,
        }
    }
}

/// 一个声明的技能参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillArgument {
    /// 参数名。
    pub name: String,
    /// 是否必填。
    pub required: bool,
    /// 说明。
    pub description: Option<String>,
}

impl SkillArgument {
    /// 只按名字声明（非必填）。
    pub fn named(name: impl Into<String>) -> Self {
        SkillArgument {
            name: name.into(),
            required: false,
            description: None,
        }
    }
}

/// 扩展后的技能元数据（全部类型化，非法值按缺省处理）。
#[derive(Debug, Clone, PartialEq)]
pub struct SkillMetadata {
    /// `allowed-tools`。
    pub allowed_tools: Vec<String>,
    /// `model`。
    pub model: Option<String>,
    /// `effort`。
    pub effort: Option<String>,
    /// `context`。
    pub context: SkillContext,
    /// `agent`（`context: fork` 时的子代理名）。
    pub agent: Option<String>,
    /// `hooks` 原始 JSON。
    pub hooks: Option<Value>,
    /// `paths` glob。
    pub paths: Vec<String>,
    /// `user-invocable`（默认 true）。
    pub user_invocable: bool,
    /// `disable-model-invocation`。
    pub disable_model_invocation: bool,
    /// `arguments`。
    pub arguments: Vec<SkillArgument>,
    /// `argument-hint`。
    pub argument_hint: Option<String>,
}

impl Default for SkillMetadata {
    /// 与 frontmatter 缺省一致：`user-invocable` 默认 `true`，其余字段取类型默认值。
    ///
    /// 加载失败时的兜底定义（文件在两次读取之间消失）也使用本默认值，不能把
    /// “没读到 metadata”误判成“作者显式禁止用户调用”。
    fn default() -> Self {
        SkillMetadata {
            allowed_tools: Vec::new(),
            model: None,
            effort: None,
            context: SkillContext::default(),
            agent: None,
            hooks: None,
            paths: Vec::new(),
            user_invocable: true,
            disable_model_invocation: false,
            arguments: Vec::new(),
            argument_hint: None,
        }
    }
}

impl SkillMetadata {
    /// 从 `SkillFrontmatter` 解析。布尔字段只接受 YAML 布尔值（与现有
    /// `disable-model-invocation` 的行为一致）；`user-invocable` 缺省为 `true`。
    pub fn from_frontmatter(frontmatter: &SkillFrontmatter) -> Self {
        SkillMetadata {
            allowed_tools: string_list(frontmatter.allowed_tools.as_ref()),
            model: string_value(frontmatter.model.as_ref()),
            effort: string_value(frontmatter.effort.as_ref()),
            context: SkillContext::from_value(frontmatter.context.as_ref()),
            agent: string_value(frontmatter.agent.as_ref()),
            hooks: frontmatter.hooks.clone(),
            paths: string_list(frontmatter.paths.as_ref()),
            user_invocable: frontmatter.user_invocable.as_ref().and_then(Value::as_bool)
                != Some(false),
            disable_model_invocation: frontmatter.disable_model_invocation
                == Some(Value::Bool(true)),
            arguments: parse_arguments(frontmatter.arguments.as_ref()),
            argument_hint: string_value(frontmatter.argument_hint.as_ref()),
        }
    }
}

/// 一个加载后的技能：现有 [`Skill`] + 扩展元数据 + 正文。
#[derive(Debug, Clone, PartialEq)]
pub struct SkillDefinition {
    /// 冻结契约的技能记录（name/description/路径/来源/模型可见性）。
    pub skill: Skill,
    /// 扩展 frontmatter。
    pub metadata: SkillMetadata,
    /// 去掉 frontmatter 后的正文（已按 `parse_frontmatter` 归一换行并 trim）。
    pub content: String,
}

impl SkillDefinition {
    /// 技能名。
    pub fn name(&self) -> &str {
        &self.skill.name
    }

    /// `SKILL.md` 路径。
    pub fn file_path(&self) -> &str {
        &self.skill.file_path
    }

    /// 技能目录。
    pub fn base_dir(&self) -> &str {
        &self.skill.base_dir
    }

    /// 是否 bundled（来源标签为 `builtin`）。
    ///
    /// bundled 技能在索引预算里**不截断**；这是唯一需要判断来源的地方。
    pub fn is_bundled(&self) -> bool {
        self.skill.source_info.source == "builtin"
            || self.skill.source_info.path.starts_with("builtin:")
    }
}

/// [`load_skill_definitions`] 的结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadSkillDefinitionsResult {
    /// 定义（发现顺序）。
    pub definitions: Vec<SkillDefinition>,
    /// 诊断。
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// 从单个文件加载扩展定义。
///
/// 复用 [`load_skill_from_file`] 的校验/诊断；只有它返回技能时才再读一次正文与
/// frontmatter。两次读取之间文件被替换属于外部竞争，失败时仍返回技能但正文为空。
pub fn load_skill_definition_from_file(
    file_path: &str,
    source: &str,
) -> (Option<SkillDefinition>, Vec<ResourceDiagnostic>) {
    let (skill, diagnostics) = load_skill_from_file(file_path, source);
    let Some(skill) = skill else {
        return (None, diagnostics);
    };
    let bytes = std::fs::read(file_path).unwrap_or_default();
    let raw = String::from_utf8_lossy(&bytes);
    let (frontmatter, content) = match parse_frontmatter(&raw) {
        Ok(parsed) => (SkillFrontmatter::from_map(&parsed.frontmatter), parsed.body),
        Err(_) => (SkillFrontmatter::default(), String::new()),
    };
    let metadata = SkillMetadata::from_frontmatter(&frontmatter);
    (
        Some(SkillDefinition {
            skill,
            metadata,
            content,
        }),
        diagnostics,
    )
}

/// 扫描一个目录，返回扩展定义（顺序、去重与诊断与 [`load_skills_from_dir`] 一致）。
pub fn load_skill_definitions_from_dir(
    options: LoadSkillsFromDirOptions,
) -> LoadSkillDefinitionsResult {
    let source = options.source.clone();
    let result = load_skills_from_dir(options);
    definitions_from_skills(result.skills, &source, result.diagnostics)
}

/// 按配置位置汇总加载扩展定义。
pub fn load_skill_definitions(options: LoadSkillsOptions) -> LoadSkillDefinitionsResult {
    let result = load_skills(options);
    // `load_skills` 的 `source` 没有单一值：从 `Skill::source_info` 反推每个技能自己的
    // 来源标签，以便诊断/重新读取时保持一致。
    let mut definitions = Vec::with_capacity(result.skills.len());
    let mut diagnostics = result.diagnostics;
    for skill in result.skills {
        let source = if skill.source_info.source == "local" {
            match skill.source_info.scope {
                kiana_core::source_info::SourceScope::User => "user",
                kiana_core::source_info::SourceScope::Project => "project",
                kiana_core::source_info::SourceScope::Temporary => "path",
            }
        } else {
            skill.source_info.source.as_str()
        };
        let (definition, extra) = load_skill_definition_from_file(&skill.file_path, source);
        diagnostics.extend(extra);
        match definition {
            Some(definition) => definitions.push(definition),
            // 文件在两次读取之间消失：保留基础 Skill，正文为空，避免丢技能。
            None => definitions.push(SkillDefinition {
                skill,
                metadata: SkillMetadata::default(),
                content: String::new(),
            }),
        }
    }
    LoadSkillDefinitionsResult {
        definitions,
        diagnostics,
    }
}

fn definitions_from_skills(
    skills: Vec<Skill>,
    source: &str,
    diagnostics: Vec<ResourceDiagnostic>,
) -> LoadSkillDefinitionsResult {
    let mut definitions = Vec::with_capacity(skills.len());
    for skill in skills {
        let (definition, extra) = load_skill_definition_from_file(&skill.file_path, source);
        definitions.push(definition.unwrap_or(SkillDefinition {
            skill,
            metadata: SkillMetadata::default(),
            content: String::new(),
        }));
        // 第二次读取不会产生新的诊断（校验已在 `load_skills_from_dir` 完成），忽略即可。
        drop(extra);
    }
    LoadSkillDefinitionsResult {
        definitions,
        diagnostics,
    }
}

/// 模型可见性：`disable-model-invocation` 为真时永远不进入索引；有 `paths` 时只有
/// 触达过匹配文件才可见。
///
/// `touched` 是本次会话已读/写/搜索过的路径（相对或绝对均可）。绝对路径会同时按原值与
/// 去掉当前目录前缀后的相对值匹配，调用方不需要预先把路径转成相对。
pub fn skill_visible_for_model(definition: &SkillDefinition, touched: &[PathBuf]) -> bool {
    skill_visible_for_model_in(definition, touched, None)
}

/// [`skill_visible_for_model`] 的会话 cwd 版本。
///
/// `cwd` 是**会话**工作目录（不是进程 cwd）。touched 路径是绝对路径时用这个前缀归一化；
/// 两者不一致（宿主 chdir 过、子代理/临时目录会话）时仍能命中 `paths` glob。
pub fn skill_visible_for_model_in(
    definition: &SkillDefinition,
    touched: &[PathBuf],
    cwd: Option<&Path>,
) -> bool {
    if definition.metadata.disable_model_invocation {
        return false;
    }
    if definition.metadata.paths.is_empty() {
        return true;
    }
    paths_match_in(&definition.metadata.paths, touched, cwd)
}

/// 条件路径匹配（`paths` 里任意模式命中任意 touched 路径）。
pub fn paths_match(patterns: &[String], touched: &[PathBuf]) -> bool {
    paths_match_in(patterns, touched, None)
}

/// [`paths_match`] 的会话 cwd 版本；`cwd = None` 时回落到进程 cwd。
pub fn paths_match_in(patterns: &[String], touched: &[PathBuf], cwd: Option<&Path>) -> bool {
    if patterns.is_empty() || touched.is_empty() {
        return false;
    }
    let mut builder = GlobSetBuilder::new();
    let mut any = false;
    for pattern in patterns {
        if let Ok(glob) = GlobBuilder::new(pattern).literal_separator(false).build() {
            builder.add(glob);
            any = true;
        }
    }
    if !any {
        return false;
    }
    let Ok(set) = builder.build() else {
        return false;
    };
    let cwd = cwd
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok());
    touched.iter().any(|path| {
        if set.is_match(path) {
            return true;
        }
        if let Some(cwd) = &cwd
            && let Ok(relative) = path.strip_prefix(cwd)
        {
            return set.is_match(relative);
        }
        false
    })
}

/// 技能目录清单（相对路径、排序、最多 [`MAX_SKILL_DIRECTORY_ENTRIES`] 条）。
///
/// 只列文件，不跟随目录符号链接，跳过隐藏文件（`.` 开头）与 `SKILL.md` 本身之外的
/// frontmatter 内容并不读取。排序用 `/` 分隔的相对路径字节序，跨平台稳定。
pub fn list_skill_directory(base_dir: &Path) -> Vec<String> {
    let mut entries = Vec::new();
    collect_files(base_dir, base_dir, &mut entries);
    entries.sort();
    if entries.len() > MAX_SKILL_DIRECTORY_ENTRIES {
        entries.truncate(MAX_SKILL_DIRECTORY_ENTRIES);
        entries.push("…".to_string());
    }
    entries
}

/// 目录清单上限（防止技能目录里有巨大的 node_modules 之类）。
pub const MAX_SKILL_DIRECTORY_ENTRIES: usize = 200;

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    if out.len() > MAX_SKILL_DIRECTORY_ENTRIES {
        return;
    }
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<_> = read_dir.filter_map(Result::ok).collect();
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_files(root, &path, out);
        } else if file_type.is_file() {
            if let Ok(relative) = path.strip_prefix(root) {
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

/// 字符串或字符串数组 → `Vec<String>`。
///
/// 字符串按逗号/换行切分；如果既没有逗号也没有换行，整串作为一个条目（这样
/// `Bash(git status)` 不会被空格拆坏）。数组里非字符串元素被跳过。
fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(text)) => text
            .split([',', '\n'])
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// 非空字符串 → `Some`。
fn string_value(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// `arguments` 支持三种形状：
/// - `["topic", "style"]`（非必填）；
/// - `[{ "name": "topic", "required": true, "description": "..." }]`；
/// - `{ "topic": "说明", "style": { "required": true } }`。
fn parse_arguments(value: Option<&Value>) -> Vec<SkillArgument> {
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(name) if !name.trim().is_empty() => {
                    Some(SkillArgument::named(name.trim()))
                }
                Value::Object(object) => {
                    let name = object.get("name").and_then(Value::as_str)?.trim();
                    if name.is_empty() {
                        return None;
                    }
                    Some(SkillArgument {
                        name: name.to_string(),
                        required: object.get("required").and_then(Value::as_bool) == Some(true),
                        description: object
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                }
                _ => None,
            })
            .collect(),
        Value::Object(object) => object
            .iter()
            .filter_map(|(name, spec)| {
                if name.trim().is_empty() {
                    return None;
                }
                let (required, description) = match spec {
                    Value::String(text) => (false, Some(text.clone())),
                    Value::Object(object) => (
                        object.get("required").and_then(Value::as_bool) == Some(true),
                        object
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    ),
                    Value::Bool(true) => (true, None),
                    _ => (false, None),
                };
                Some(SkillArgument {
                    name: name.clone(),
                    required,
                    description,
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// 给测试/宿主用：`paths` 编译成 [`GlobSet`]（非法模式被忽略）。
pub fn compile_paths(patterns: &[String]) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let mut any = false;
    for pattern in patterns {
        if let Ok(glob) = GlobBuilder::new(pattern).literal_separator(false).build() {
            builder.add(glob);
            any = true;
        }
    }
    if any { builder.build().ok() } else { None }
}
