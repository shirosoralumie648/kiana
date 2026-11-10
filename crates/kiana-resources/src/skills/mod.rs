//! `core/skills.ts`：`Skill` / `SkillFrontmatter` / `LoadSkillsResult` / `load_skills`。
//!
//! 逐字移植 `packages/coding-agent/src/core/skills.ts`（514 行）。
//!
//! `Skill` 定义在 [`kiana_ext_api::system_prompt::Skill`]（`ARCHITECTURE.md` §3.5），本模块只再导出，
//! **不得**重新定义。`ResourceDiagnostic` / `ResourceCollision` / `DiagnosticKind` / `ResourceType`
//! 来自 [`kiana_core::diagnostics`]；`SourceInfo` 来自 [`kiana_core::source_info`]。
//!
//! # 这个模块负责什么
//! 对外提供两个入口：[`load_skills`]（按配置位置汇总加载，供资源加载器与 SDK 导出使用）和
//! [`load_skills_from_dir`]（只扫描一个目录）。子模块 [`discover`] 负责递归遍历与单文件解析，
//! [`prompt`] 负责把技能渲染进系统提示，[`validate`] 负责 name/description 校验。
//!
//! # 执行流程（[`load_skills`]）
//! 1. 用 `resolve_path_default` 把 `cwd` / `agent_dir` 归一为绝对路径；
//! 2. `include_defaults` 为真时，依次加载 `<agent_dir>/skills`（source `"user"`）与
//!    `<cwd>/.kiana/skills`（source `"project"`）；
//! 3. 再按 `skill_paths` 的顺序逐个处理：目录递归加载、`.md` 单文件加载，
//!    不存在或不是 Markdown 时只产生 warning；
//! 4. 汇总时按 name 去重（先到先得），同名冲突单独收集，最终排在所有 warning 之后。
//!
//! # 注意事项
//! - [`Skill`] 的字段与 `source_info` 结构由上游 crate 定义，本模块只填充、不改变契约。
//! - 文件下半部分是 node:path 的词法兼容层（posix 与 win32 两套，用 `cfg!(windows)` 选择）：
//!   它只做字符串运算，不访问文件系统，也不解析符号链接。
//! - 递归遍历、ignore 规则、`SKILL.md` 优先等细节在 [`discover`] 中，本文件不重复。

pub mod activation;
pub mod definition;
pub mod discover;
pub(crate) mod ignore_match;
pub mod prompt;
pub mod tool;
pub mod validate;

pub use activation::{
    ParsedAllowedTools, SkillActivation, SkillActivationSet, evaluate_with_activations,
    parse_allowed_tools,
};
pub use definition::{
    LoadSkillDefinitionsResult, SkillArgument, SkillContext, SkillDefinition, SkillMetadata,
    list_skill_directory, load_skill_definition_from_file, load_skill_definitions,
    load_skill_definitions_from_dir, paths_match, paths_match_in, skill_visible_for_model,
    skill_visible_for_model_in,
};
pub use prompt::{
    DEFAULT_SKILL_INDEX_BUDGET_CHARS, MAX_SKILL_DESCRIPTION_CHARS,
    format_skill_definitions_for_prompt, format_skill_definitions_for_prompt_with_cwd,
    skill_index_budget_chars,
};
pub use tool::{
    DEFAULT_SKILL_ACTIVATION_TTL, SKILL_TOOL_NAME, SkillTool, SkillToolOutcome,
    create_skill_tool_definition,
};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use kiana_core::diagnostics::{
    DiagnosticKind, ResourceCollision, ResourceDiagnostic, ResourceType,
};
use kiana_core::source_info::{
    SourceInfo, SourceScope, SyntheticSourceInfoOptions, create_synthetic_source_info,
};
/// 技能类型（定义在 [`kiana_ext_api::system_prompt`]，这里只再导出）。
pub use kiana_ext_api::system_prompt::Skill;

use crate::skills::discover::{load_skill_from_file, load_skills_from_dir_internal};

/// `.gitignore` / `.ignore` / `.fdignore` 文件名（顺序即读取顺序）。
/// TS: `skills.ts#IGNORE_FILE_NAMES`。
///
/// 顺序有意义：`discover` 按该顺序把每个文件的规则追加进同一个 matcher，后读到的规则可以
/// 否定先读到的规则（npm `ignore` 的语义）。
const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

/// 技能 frontmatter。TS: `skills.ts#SkillFrontmatter`。
///
/// TS 的索引签名允许任意未知字段；这里保留在 [`SkillFrontmatter::extra`] 里（插入序）。
///
/// 已知字段的类型用 [`serde_json::Value`] 而不是 `String`/`bool`，因为 TS 运行时不做类型
/// 校验：值可能不是字符串或布尔。原样保留后由加载逻辑按 `as_str` / `== Value::Bool(true)`
/// 判定，非法类型表现为“字段缺失”而不是反序列化失败。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SkillFrontmatter {
    /// `name`（TS 类型是 `string | undefined`；运行期不校验类型）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Value>,
    /// `description`（TS 类型是 `string | undefined`；运行期不校验类型）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Value>,
    /// `disable-model-invocation`（必须是 YAML 布尔 `true` 才生效）。
    #[serde(
        rename = "disable-model-invocation",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub disable_model_invocation: Option<Value>,
    /// `allowed-tools`：激活期间授予的权限规则（字符串数组）。
    #[serde(
        rename = "allowed-tools",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub allowed_tools: Option<Value>,
    /// `model`：激活期间覆盖的模型别名/ID。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Value>,
    /// `effort`：激活期间覆盖的 thinking effort。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Value>,
    /// `context`：`inline`（默认）或 `fork`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
    /// `agent`：fork 时使用的子代理名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Value>,
    /// `hooks`：激活期间安装的 hooks 配置（原样交给 hooks 模块）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Value>,
    /// `paths`：技能可见的条件 glob（字符串或字符串数组）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Value>,
    /// `user-invocable`：是否允许用户通过 `/skill` 触发（默认 `true`）。
    #[serde(
        rename = "user-invocable",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub user_invocable: Option<Value>,
    /// `arguments`：参数声明（字符串数组 / Map / `{name,required}` 数组）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    /// `argument-hint`：命令补全/帮助里显示的参数提示。
    #[serde(
        rename = "argument-hint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub argument_hint: Option<Value>,
    /// 未知字段，原样保留。
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl SkillFrontmatter {
    /// 从 `parse_frontmatter` 的键值表构造。
    ///
    /// 把已知 frontmatter 键搬进具名字段，其余键原样放进 [`SkillFrontmatter::extra`]；
    /// 遍历顺序沿用传入 map 的迭代顺序。
    pub fn from_map(map: &serde_json::Map<String, Value>) -> Self {
        let mut frontmatter = SkillFrontmatter::default();
        for (key, value) in map {
            match key.as_str() {
                "name" => frontmatter.name = Some(value.clone()),
                "description" => frontmatter.description = Some(value.clone()),
                "disable-model-invocation" => {
                    frontmatter.disable_model_invocation = Some(value.clone());
                }
                "allowed-tools" => frontmatter.allowed_tools = Some(value.clone()),
                "model" => frontmatter.model = Some(value.clone()),
                "effort" => frontmatter.effort = Some(value.clone()),
                "context" => frontmatter.context = Some(value.clone()),
                "agent" => frontmatter.agent = Some(value.clone()),
                "hooks" => frontmatter.hooks = Some(value.clone()),
                "paths" => frontmatter.paths = Some(value.clone()),
                "user-invocable" => frontmatter.user_invocable = Some(value.clone()),
                "arguments" => frontmatter.arguments = Some(value.clone()),
                "argument-hint" => frontmatter.argument_hint = Some(value.clone()),
                _ => {
                    frontmatter.extra.insert(key.clone(), value.clone());
                }
            }
        }
        frontmatter
    }
}

/// 技能加载结果。TS: `skills.ts#LoadSkillsResult`。
///
/// [`LoadSkillsResult::skills`] 保持首次发现顺序（按 name 去重后的插入序）；
/// [`LoadSkillsResult::diagnostics`] 中普通 warning 在前、同名冲突统一在最后。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadSkillsResult {
    /// 已加载的技能（发现顺序）。
    pub skills: Vec<Skill>,
    /// 诊断（警告 + 冲突）。
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// `load_skills_from_dir` 的选项。TS: `skills.ts#LoadSkillsFromDirOptions`。
///
/// `source` 会传给 `create_skill_source_info`：`"user"` / `"project"` 映射到对应的
/// `SourceScope`，`"path"` 与其它未知值落到默认的 `Temporary`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadSkillsFromDirOptions {
    /// 要扫描的目录。
    pub dir: String,
    /// 技能来源标识（`"user"` / `"project"` / `"path"` / 其它）。
    pub source: String,
}

/// `load_skills` 的选项。TS: `skills.ts#LoadSkillsOptions`（四个字段均必填）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadSkillsOptions {
    /// 项目本地技能的工作目录；相对路径按进程当前目录解析。
    pub cwd: String,
    /// 全局技能的 agent 配置目录；相对路径按进程当前目录解析。
    pub agent_dir: String,
    /// 显式技能路径（文件或目录），按数组顺序处理；每项先 trim 再相对 `cwd` 解析。
    pub skill_paths: Vec<String>,
    /// 是否包含默认技能目录（`agent_dir/skills` 与 `cwd/.kiana/skills`）。
    pub include_defaults: bool,
}

/// 构造一条 warning 诊断。
pub(crate) fn warning(message: impl Into<String>, path: &str) -> ResourceDiagnostic {
    ResourceDiagnostic {
        kind: DiagnosticKind::Warning,
        message: message.into(),
        path: Some(path.to_string()),
        collision: None,
    }
}

/// 构造技能同名冲突诊断。TS: `skills.ts#loadSkills#addSkills`。
///
/// `existing` 是先加载（胜出）的技能，`skill` 是后来（被丢弃）的技能；
/// [`ResourceCollision::winner_path`] / [`ResourceCollision::loser_path`] 分别取两者的
/// `file_path`，两个 `source` 字段留空（冲突双方来源可能不同，无法归属单一来源）。
pub(crate) fn collision(skill: &Skill, existing: &Skill) -> ResourceDiagnostic {
    ResourceDiagnostic {
        kind: DiagnosticKind::Collision,
        message: format!("name \"{}\" collision", skill.name),
        path: Some(skill.file_path.clone()),
        collision: Some(ResourceCollision {
            resource_type: ResourceType::Skill,
            name: skill.name.clone(),
            winner_path: existing.file_path.clone(),
            loser_path: skill.file_path.clone(),
            winner_source: None,
            loser_source: None,
        }),
    }
}

/// 按 `source` 构造技能来源信息。TS: `skills.ts#createSkillSourceInfo`。
///
/// `"user"` / `"project"` / `"path"` 都把 `source` 固定为 `"local"`，只在 scope 上区分
/// （`User` / `Project` / `None`）；未知值则原样作为 `source`，scope 也用 `None`。
/// `scope: None` 会被 `create_synthetic_source_info`（`kiana-core/src/source_info.rs`）
/// 补成默认的 `Temporary`，所以 `"path"` 与未知值的技能 scope 最终都是 `Temporary`。
/// `base_dir` 记录技能所在目录（`SKILL.md` 的父目录）。
pub(crate) fn create_skill_source_info(
    file_path: &str,
    base_dir: &str,
    source: &str,
) -> SourceInfo {
    match source {
        "user" => create_synthetic_source_info(
            file_path,
            SyntheticSourceInfoOptions {
                source: "local".to_string(),
                scope: Some(SourceScope::User),
                origin: None,
                base_dir: Some(base_dir.to_string()),
            },
        ),
        "project" => create_synthetic_source_info(
            file_path,
            SyntheticSourceInfoOptions {
                source: "local".to_string(),
                scope: Some(SourceScope::Project),
                origin: None,
                base_dir: Some(base_dir.to_string()),
            },
        ),
        "path" => create_synthetic_source_info(
            file_path,
            SyntheticSourceInfoOptions {
                source: "local".to_string(),
                scope: None,
                origin: None,
                base_dir: Some(base_dir.to_string()),
            },
        ),
        other => create_synthetic_source_info(
            file_path,
            SyntheticSourceInfoOptions {
                source: other.to_string(),
                scope: None,
                origin: None,
                base_dir: Some(base_dir.to_string()),
            },
        ),
    }
}

/// 从目录加载技能。TS: `skills.ts#loadSkillsFromDir`。
///
/// 等价于 `load_skills_from_dir_internal(dir, source, true, None, None)`：只扫描这一个目录
/// （会递归子目录），不做同名冲突检查——那一步在 [`load_skills`] 里完成。
/// 目录不存在时返回空结果而不是错误。
pub fn load_skills_from_dir(options: LoadSkillsFromDirOptions) -> LoadSkillsResult {
    load_skills_from_dir_internal(&options.dir, &options.source, true, None, None)
}

/// 从所有配置位置加载技能。TS: `skills.ts#loadSkills`。
///
/// 优先级：`includeDefaults` 时先 `agentDir/skills`（user）再 `cwd/{CONFIG_DIR_NAME}/skills`
/// （project）；显式路径随后。同名技能先到先得，后来者产生 `collision` 诊断；同一文件经符号链接
/// 重复出现时静默跳过。
///
/// 去重分两级：
/// - 名字：比较 [`Skill`] 的 `name`，胜出者保留在结果里；
/// - 真实路径：用 `kiana_core::paths::canonicalize_path` 解析符号链接后比较，重复文件既不加
///   技能也不产生诊断（解析失败时退回原字符串比较）。
///
/// 显式路径的 `source` 判定：`include_defaults` 为真时一律记 `"path"`；为假时，位于
/// `<agent_dir>/skills` 或 `<cwd>/.kiana/skills` 之下的路径仍分别记为 `"user"` / `"project"`。
///
/// 本函数没有错误通道：目录缺失、读取失败、路径不是 `.md` 都不会返回 `Err`。默认目录读取
/// 失败会被静默当作空目录；只有显式路径的 `exists` / `metadata` 失败才转成 warning 诊断。
pub fn load_skills(options: LoadSkillsOptions) -> LoadSkillsResult {
    let resolved_cwd = resolve_path_default(&options.cwd);
    let resolved_agent_dir = resolve_path_default(&options.agent_dir);

    let mut skill_map: IndexMap<String, Skill> = IndexMap::new();
    let mut real_path_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut all_diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let mut collision_diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    // 合并一组加载结果：先收诊断，再按“真实路径”和“名字”两级去重。
    // `real_path_set` 用 realpath，目的是把经符号链接重复出现的同一文件静默去掉；
    // 名字冲突不删除已有技能，只记一条 collision，所以 `skill_map` 里始终是先加载者。
    let add_skills = |result: LoadSkillsResult,
                      skill_map: &mut IndexMap<String, Skill>,
                      real_path_set: &mut std::collections::HashSet<String>,
                      all_diagnostics: &mut Vec<ResourceDiagnostic>,
                      collision_diagnostics: &mut Vec<ResourceDiagnostic>| {
        all_diagnostics.extend(result.diagnostics);
        for skill in result.skills {
            let real_path = kiana_core::paths::canonicalize_path(&skill.file_path);
            if real_path_set.contains(&real_path) {
                continue;
            }
            match skill_map.get(&skill.name) {
                Some(existing) => collision_diagnostics.push(collision(&skill, existing)),
                None => {
                    real_path_set.insert(real_path);
                    skill_map.insert(skill.name.clone(), skill);
                }
            }
        }
    };

    let user_skills_dir = join_path(&resolved_agent_dir, "skills");
    let project_skills_dir = resolve_config_path(&resolved_cwd, "skills");

    // 默认目录顺序固定为 user → project；`add_skills` 按调用顺序写入 skill_map，
    // 因此同名时 user 技能先占位，project 技能只留下 collision 诊断。
    if options.include_defaults {
        let user = load_skills_from_dir_internal(&user_skills_dir, "user", true, None, None);
        add_skills(
            user,
            &mut skill_map,
            &mut real_path_set,
            &mut all_diagnostics,
            &mut collision_diagnostics,
        );
        let project =
            load_skills_from_dir_internal(&project_skills_dir, "project", true, None, None);
        add_skills(
            project,
            &mut skill_map,
            &mut real_path_set,
            &mut all_diagnostics,
            &mut collision_diagnostics,
        );
    }

    // 路径归属判断：`root` 先做词法 resolve，`target` 由调用方保证已是解析后的路径；
    // 比较时按分隔符边界补前缀，避免把 `/a/bc` 误判为 `/a/b` 的子路径。
    let is_under_path = |target: &str, root: &str| -> bool {
        let normalized_root = resolve_path_default(root);
        if target == normalized_root {
            return true;
        }
        let prefix = if normalized_root.ends_with(std::path::MAIN_SEPARATOR) {
            normalized_root
        } else {
            format!("{normalized_root}{}", std::path::MAIN_SEPARATOR)
        };
        target.starts_with(&prefix)
    };

    // include_defaults 为真时默认目录已加载过，显式路径统一记为 "path"；
    // 为假时仍按路径位置推断 user/project，供 source_info 使用。
    let get_source = |resolved_path: &str| -> &'static str {
        if !options.include_defaults {
            if is_under_path(resolved_path, &user_skills_dir) {
                return "user";
            }
            if is_under_path(resolved_path, &project_skills_dir) {
                return "project";
            }
        }
        "path"
    };

    // 显式路径按数组顺序逐项处理，单项失败不中断后续项：
    // - 不存在 → warning；
    // - 目录 → 递归加载；`.md` 文件 → 单文件加载；
    // - 其它（非 `.md` 的普通文件、设备文件等）→ warning。
    // 注意 `.md` 是对已解析的字符串做 ends_with，大小写敏感。
    for raw_path in &options.skill_paths {
        let resolved_path = resolve_path_trimmed(raw_path, &resolved_cwd);
        if !std::path::Path::new(&resolved_path).exists() {
            all_diagnostics.push(warning("skill path does not exist", &resolved_path));
            continue;
        }

        match std::fs::metadata(&resolved_path) {
            Ok(metadata) => {
                let source = get_source(&resolved_path);
                if metadata.is_dir() {
                    let result =
                        load_skills_from_dir_internal(&resolved_path, source, true, None, None);
                    add_skills(
                        result,
                        &mut skill_map,
                        &mut real_path_set,
                        &mut all_diagnostics,
                        &mut collision_diagnostics,
                    );
                } else if metadata.is_file() && resolved_path.ends_with(".md") {
                    let (skill, diagnostics) = load_skill_from_file(&resolved_path, source);
                    match skill {
                        Some(skill) => add_skills(
                            LoadSkillsResult {
                                skills: vec![skill],
                                diagnostics,
                            },
                            &mut skill_map,
                            &mut real_path_set,
                            &mut all_diagnostics,
                            &mut collision_diagnostics,
                        ),
                        None => all_diagnostics.extend(diagnostics),
                    }
                } else {
                    all_diagnostics
                        .push(warning("skill path is not a markdown file", &resolved_path));
                }
            }
            Err(error) => {
                all_diagnostics.push(warning(
                    node_read_error_message(&error, "stat", &resolved_path),
                    &resolved_path,
                ));
            }
        }
    }

    // 冲突诊断统一排在所有普通诊断之后（TS 的 loadSkills 也如此，测试锁定了该顺序）。
    let mut diagnostics = all_diagnostics;
    diagnostics.extend(collision_diagnostics);
    LoadSkillsResult {
        // IndexMap 的 into_values 按插入序产出，即“首次发现顺序”。
        skills: skill_map.into_values().collect(),
        diagnostics,
    }
}

// ---------------------------------------------------------------------------
// 路径辅助（node:path 的词法实现）
// ---------------------------------------------------------------------------
//
// TS 的 skills.ts / prompt-templates.ts 直接用 `node:path` 的 join/basename/dirname/relative。
// Rust 的 `std::path` 语义不同（不折叠 `.`/`..`、`file_name(".")` 为 None、尾随分隔符等），
// 因此这里逐字复制 `kiana-core::paths` 的 node:path 词法实现（posix + win32）。
// ARCHITECTURE §8 第 23 条把唯一实现位置定在 `kiana-durable::env::path`，但 `kiana-resources`
// 不在其依赖图中（§2.2），无法复用；这是本 crate 内的一份最小副本。
//
// 这些函数只做字符串层面的规范化，不访问文件系统：不解析符号链接、不检查存在性。
// 需要真实文件的调用方（`discover`）另行使用 `fs::read_dir` / `fs::metadata`。
// `node_*` 包装通过 `cfg!(windows)` 分派；这是编译期常量，但两套实现都会参与编译，
// 所以两个平台的代码在所有目标上都要能通过类型检查。本文件的单元测试只调用 `node_*`
// 包装，因此只执行当前平台的实现，另一套实现不会被测试覆盖。

/// 进程当前工作目录（`process.cwd()`）；取不到时退回 `"."`。
fn cwd_string() -> String {
    std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

fn is_posix_sep(c: char) -> bool {
    c == '/'
}

fn is_win32_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

/// `path.posix.normalizeString(path, allowAboveRoot)`，不含 `normalize()` 的尾分隔符/`"."` 包装。
fn posix_normalize_string(path: &str, allow_above_root: bool) -> String {
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split(is_posix_sep) {
        match segment {
            "" | "." => {}
            ".." => {
                if let Some(last) = out.last() {
                    if *last != ".." {
                        out.pop();
                    } else if allow_above_root {
                        out.push("..");
                    }
                } else if allow_above_root {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

/// `path.posix.normalize(path)`。
fn posix_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let is_absolute = path.starts_with('/');
    let trailing_separator = path.ends_with('/');
    let mut normalized = posix_normalize_string(path, !is_absolute);
    if normalized.is_empty() {
        return if is_absolute {
            "/".to_string()
        } else if trailing_separator {
            "./".to_string()
        } else {
            ".".to_string()
        };
    }
    if trailing_separator {
        normalized.push('/');
    }
    if is_absolute {
        format!("/{normalized}")
    } else {
        normalized
    }
}

/// `path.posix.join(...)`。
fn posix_join(parts: &[&str]) -> String {
    let mut joined = String::new();
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if joined.is_empty() {
            joined.push_str(part);
        } else {
            joined.push('/');
            joined.push_str(part);
        }
    }
    if joined.is_empty() {
        return ".".to_string();
    }
    posix_normalize(&joined)
}

/// `path.posix.isAbsolute(path)`。
fn posix_is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

/// `path.posix.resolve(...)`，参数按 TS 的顺序（`resolve(a, b, ...)`）。
fn posix_resolve(parts: &[&str]) -> String {
    let mut resolved = String::new();
    let mut absolute = false;
    for part in parts.iter().rev() {
        if part.is_empty() {
            continue;
        }
        if resolved.is_empty() {
            resolved = (*part).to_string();
        } else {
            resolved = format!("{part}/{resolved}");
        }
        absolute = posix_is_absolute(part);
        if absolute {
            break;
        }
    }
    if !absolute {
        let cwd = cwd_string();
        resolved = if resolved.is_empty() {
            cwd
        } else {
            format!("{cwd}/{resolved}")
        };
        // Node 在这里把 cwd 拼到前面，结果**必然**是绝对路径；若只看最后一个输入参数
        // 会漏掉这一步，导致下面把前导 `/` 丢掉（`resolve("x", "rel/base")` 的回归）。
        absolute = posix_is_absolute(&resolved);
    }
    let normalized = posix_normalize_string(&resolved, false);
    if absolute {
        format!("/{normalized}")
    } else if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

/// `path.posix.relative(from, to)`。
fn posix_relative(from: &str, to: &str) -> String {
    let from = posix_resolve(&[from]);
    let to = posix_resolve(&[to]);
    if from == to {
        return String::new();
    }
    let from_parts: Vec<&str> = from.split('/').filter(|s| !s.is_empty()).collect();
    let to_parts: Vec<&str> = to.split('/').filter(|s| !s.is_empty()).collect();
    let common = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut out: Vec<&str> = Vec::new();
    out.extend(std::iter::repeat_n("..", from_parts.len() - common));
    out.extend_from_slice(&to_parts[common..]);
    out.join("/")
}

/// `path.posix.dirname(path)`。
fn posix_dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    // 去掉尾部多余的分隔符（根除外）。
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    match trimmed.rfind('/') {
        None => ".".to_string(),
        // Node 对 "//a" 返回 "//"（POSIX 的双斜杠根特例，对应 `end === 1`）。
        Some(1) if trimmed.starts_with("//") => "//".to_string(),
        Some(0) => "/".to_string(),
        Some(idx) => trimmed[..idx].to_string(),
    }
}

/// `path.posix.basename(path)`。
fn posix_basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    match trimmed.rfind('/') {
        None => trimmed.to_string(),
        Some(idx) => trimmed[idx + 1..].to_string(),
    }
}

/// win32 路径的 root 分解：`(root_len, root_kind)`。
///
/// `root_len` 是 root 在原始字符串中的字节长度（可能包含一个尾分隔符）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Win32RootKind {
    /// 无 root（相对路径）。
    None,
    /// 单个分隔符开头的 root。
    Rooted,
    /// `C:` / `C:\`。
    Drive { absolute: bool },
    /// `\\server\share`。
    Unc { server_end: usize, share_end: usize },
}

/// 识别 win32 路径的 root 形态，并给出各段在原始字符串中的位置。
fn win32_root_kind(path: &str) -> Win32RootKind {
    if path.is_empty() {
        return Win32RootKind::None;
    }
    if is_win32_sep_byte(path, 0) && is_win32_sep_byte(path, 1) {
        // 尝试解析 UNC：两个非空段。
        let mut idx = 2;
        while idx < path.len() && is_win32_sep_byte(path, idx) {
            idx += 1;
        }
        let server_start = idx;
        while idx < path.len() && !is_win32_sep_byte(path, idx) {
            idx += 1;
        }
        let server_end = idx;
        while idx < path.len() && is_win32_sep_byte(path, idx) {
            idx += 1;
        }
        let share_start = idx;
        while idx < path.len() && !is_win32_sep_byte(path, idx) {
            idx += 1;
        }
        let share_end = idx;
        if server_end > server_start && share_end > share_start {
            return Win32RootKind::Unc {
                server_end,
                share_end,
            };
        }
        return Win32RootKind::Rooted;
    }
    if path.len() >= 2
        && is_drive_letter(path.chars().next().unwrap())
        && path.as_bytes()[1] == b':'
    {
        return Win32RootKind::Drive {
            absolute: is_win32_sep_byte(path, 2),
        };
    }
    if is_win32_sep_byte(path, 0) {
        return Win32RootKind::Rooted;
    }
    Win32RootKind::None
}

/// `path.win32.normalize(path)` 的最小实现（见模块文档：UNC 的罕见形态为近似值）。
fn win32_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let root_kind = win32_root_kind(path);
    let (root_norm, root_len, absolute): (String, usize, bool) = match root_kind {
        Win32RootKind::None => (String::new(), 0, false),
        Win32RootKind::Rooted => ("\\".to_string(), 1, true),
        Win32RootKind::Drive { absolute } => {
            let letter = &path[..1];
            if absolute {
                (format!("{letter}:\\"), 3, true)
            } else {
                (format!("{letter}:"), 2, false)
            }
        }
        Win32RootKind::Unc {
            server_end,
            share_end,
        } => {
            let server = &path[2..server_end];
            let share = &path[server_end..share_end].trim_start_matches(is_win32_sep);
            (format!("\\\\{server}\\{share}"), share_end, true)
        }
    };
    let mut rest = &path[root_len..];
    rest = rest.trim_start_matches(is_win32_sep);
    let trailing_separator = rest.ends_with(is_win32_sep);
    let mut out: Vec<&str> = Vec::new();
    for segment in rest.split(is_win32_sep) {
        match segment {
            "" | "." => {}
            ".." => {
                if let Some(last) = out.last() {
                    if *last != ".." {
                        out.pop();
                    } else if !absolute {
                        out.push("..");
                    }
                } else if !absolute {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    // 盘符相对路径（`C:foo`）在盘符与第一段之间**不**插分隔符。
    let drive_relative = matches!(root_kind, Win32RootKind::Drive { absolute: false });
    let mut result = root_norm;
    if !out.is_empty() {
        if !result.is_empty() && !result.ends_with('\\') && !drive_relative {
            result.push('\\');
        }
        result.push_str(&out.join("\\"));
    }
    if result.is_empty() {
        return ".".to_string();
    }
    if trailing_separator && !out.is_empty() {
        result.push('\\');
    }
    if result.starts_with("\\\\") && out.is_empty() && !result.ends_with('\\') {
        // Node 对 UNC root 本身补一个尾分隔符（`//a\b` -> `\\a\b\`）。
        result.push('\\');
    }
    if result.len() == 2 && result.ends_with(':') {
        // Node 对裸盘符返回 "C:."。
        result.push('.');
    }
    // Node 的 CVE-2024-36139 防护：相对路径且未解析出 device 时，如果某段的冒号会被
    // Windows 当成盘符（首段形如 `X:`，或 `:` 位于末尾/分隔符前），必须加 `.\` 前缀。
    // 例：`./D:\Windows\...` -> `.\D:\Windows\...`（否则会被当作绝对盘符路径）。
    if matches!(root_kind, Win32RootKind::None) && path.contains(':') {
        let looks_like_drive = {
            let bytes = result.as_bytes();
            bytes.len() >= 2
                && is_drive_letter(result.chars().next().unwrap_or(' '))
                && bytes[1] == b':'
        };
        let colon_before_separator_or_end = path
            .match_indices(':')
            .any(|(index, _)| index + 1 == path.len() || is_win32_sep_byte(path, index + 1));
        if looks_like_drive || colon_before_separator_or_end {
            return format!(".\\{result}");
        }
    }
    result
}

/// 第 `idx` 个字节是否为 win32 分隔符（`/` 或 `\`）；越界返回 `false`。
fn is_win32_sep_byte(path: &str, idx: usize) -> bool {
    path.as_bytes()
        .get(idx)
        .map(|b| *b == b'/' || *b == b'\\')
        .unwrap_or(false)
}

/// 是否为 ASCII 字母（用作盘符判定）。
fn is_drive_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
}

/// `path.win32.join(...)`。
fn win32_join(parts: &[&str]) -> String {
    let mut joined = String::new();
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if joined.is_empty() {
            joined.push_str(part);
        } else {
            joined.push('\\');
            joined.push_str(part);
        }
    }
    if joined.is_empty() {
        return ".".to_string();
    }
    win32_normalize(&joined)
}

/// `path.win32.isAbsolute(path)`。
fn win32_is_absolute(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let bytes = path.as_bytes();
    if bytes[0] == b'/' || bytes[0] == b'\\' {
        return true;
    }
    bytes.len() > 2
        && is_drive_letter(path.chars().next().unwrap())
        && bytes[1] == b':'
        && is_win32_sep_byte(path, 2)
}

/// `path.win32.dirname(path)`。
fn win32_dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let root_len = match win32_root_kind(path) {
        Win32RootKind::None => 0,
        Win32RootKind::Rooted => 1,
        Win32RootKind::Drive { absolute } => {
            if absolute {
                3
            } else {
                2
            }
        }
        Win32RootKind::Unc { share_end, .. } => {
            // root 含 share 后的那个分隔符（Node 的 dirname 保留它）。
            if share_end < path.len() {
                share_end + 1
            } else {
                share_end
            }
        }
    };
    let trimmed = path.trim_end_matches(is_win32_sep);
    if trimmed.len() <= root_len {
        return path[..root_len.min(path.len())].to_string();
    }
    match trimmed.rfind(is_win32_sep) {
        Some(idx) if idx >= root_len => trimmed[..idx].to_string(),
        Some(_) => path[..root_len.min(path.len())].to_string(),
        None => {
            if root_len > 0 {
                path[..root_len.min(path.len())].to_string()
            } else {
                ".".to_string()
            }
        }
    }
}

/// `path.win32.basename(path)`。
///
/// 逐字复刻 Node：盘符前缀（`C:`）不参与匹配；尾部分隔符被忽略；`C:\` 的 basename 是
/// 空串（不是 `C:`）。
fn win32_basename(path: &str) -> String {
    let bytes = path.as_bytes();
    let start = if bytes.len() >= 2
        && is_drive_letter(path.chars().next().unwrap_or(' '))
        && bytes[1] == b':'
    {
        2
    } else {
        0
    };
    let mut end: Option<usize> = None;
    let mut index = bytes.len();
    while index > start {
        index -= 1;
        if is_win32_sep_byte(path, index) {
            if let Some(end) = end {
                return path[index + 1..end].to_string();
            }
        } else if end.is_none() {
            end = Some(index + 1);
        }
    }
    match end {
        Some(end) => path[start.min(end)..end].to_string(),
        None => String::new(),
    }
}

/// `path.win32.resolve(...)`：未遇到绝对分量时把进程 cwd 拼在最前面。
fn win32_resolve(parts: &[&str]) -> String {
    let mut resolved = String::new();
    let mut absolute = false;
    for part in parts.iter().rev() {
        if part.is_empty() {
            continue;
        }
        if resolved.is_empty() {
            resolved = (*part).to_string();
        } else {
            resolved = format!("{part}\\{resolved}");
        }
        absolute = win32_is_absolute(part);
        if absolute {
            break;
        }
    }
    if !absolute {
        let cwd = cwd_string();
        resolved = if resolved.is_empty() {
            cwd
        } else {
            format!("{cwd}\\{resolved}")
        };
    }
    let normalized = win32_normalize(&resolved);
    if absolute {
        normalized
    } else if normalized == "." {
        ".".to_string()
    } else {
        normalized
    }
}

/// `path.relative(from, to)` 的平台分派（`cfg!(windows)`）。
fn node_relative(from: &str, to: &str) -> String {
    if cfg!(windows) {
        win32_relative(from, to)
    } else {
        posix_relative(from, to)
    }
}

fn win32_relative(from: &str, to: &str) -> String {
    let from = win32_resolve(&[from]);
    let to = win32_resolve(&[to]);
    if from.eq_ignore_ascii_case(&to) {
        return String::new();
    }
    let split = |s: &str| -> Vec<String> {
        s.split(is_win32_sep)
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string())
            .collect()
    };
    let from_parts = split(&from);
    let to_parts = split(&to);
    // 不同盘符（或 UNC 前缀不同）时 Node 返回绝对路径。
    let from_root = from_parts.first().cloned().unwrap_or_default();
    let to_root = to_parts.first().cloned().unwrap_or_default();
    if !from_root.eq_ignore_ascii_case(&to_root) {
        return to;
    }
    let common = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
        .count();
    let mut out: Vec<String> = Vec::new();
    for _ in common..from_parts.len() {
        out.push("..".to_string());
    }
    out.extend(to_parts[common..].iter().cloned());
    out.join("\\")
}

/// `path.dirname` 的平台分派（`cfg!(windows)`）。
fn node_dirname(path: &str) -> String {
    if cfg!(windows) {
        win32_dirname(path)
    } else {
        posix_dirname(path)
    }
}

/// `path.basename` 的平台分派（`cfg!(windows)`）。
fn node_basename(path: &str) -> String {
    if cfg!(windows) {
        win32_basename(path)
    } else {
        posix_basename(path)
    }
}

/// `path.join` 的平台分派（`cfg!(windows)`）。
fn node_join(parts: &[&str]) -> String {
    if cfg!(windows) {
        win32_join(parts)
    } else {
        posix_join(parts)
    }
}

/// `resolve(cwd, CONFIG_DIR_NAME, leaf)`：`path.resolve`。
///
/// `CONFIG_DIR_NAME` 为 `".kiana"`（`kiana-core` 的 `config.rs`），因此结果形如
/// `<cwd>/.kiana/<leaf>`。纯词法解析，不要求该目录存在。
pub(crate) fn resolve_config_path(cwd: &str, leaf: &str) -> String {
    let input = format!("{}/{}", kiana_core::config::CONFIG_DIR_NAME, leaf);
    kiana_core::paths::resolve_path(&input, cwd, &kiana_core::paths::PathInputOptions::default())
}

/// `resolvePath(input)`：缺省 baseDir 为进程 cwd。
///
/// 采用 `PathInputOptions::default()`，因此会展开开头的 `~`（`~user` 不展开），但不 trim。
pub(crate) fn resolve_path_default(input: &str) -> String {
    let cwd = cwd_string();
    kiana_core::paths::resolve_path(input, &cwd, &kiana_core::paths::PathInputOptions::default())
}

/// `resolvePath(input, baseDir, {trim: true})`。
///
/// 仅额外打开 `trim`；`expand_tilde` 等仍沿用默认值（`~` 会被展开）。
pub(crate) fn resolve_path_trimmed(input: &str, base_dir: &str) -> String {
    kiana_core::paths::resolve_path(
        input,
        base_dir,
        &kiana_core::paths::PathInputOptions {
            trim: true,
            ..Default::default()
        },
    )
}

/// `path.join`（node:path，两个以上参数）。
pub(crate) fn join_path(base: &str, name: &str) -> String {
    node_join(&[base, name])
}

/// `basename`（node:path）。
pub(crate) fn basename(path: &str) -> String {
    node_basename(path)
}

/// `dirname`（node:path）。
pub(crate) fn dirname(path: &str) -> String {
    node_dirname(path)
}

/// `path.relative(root, path)`（node:path；相对路径按进程 cwd 解析）。
///
/// Windows 上跨盘符 / 不同 UNC 前缀时返回 `path` 的绝对形式（Node 行为）。
pub(crate) fn relative_path(root: &str, path: &str) -> String {
    node_relative(root, path)
}

/// Node `fs` 读取错误的 `error.message`（`ENOENT: no such file or directory, open '<path>'` 等）。
///
/// 只映射 `NotFound` / `PermissionDenied` / `IsADirectory` / `NotADirectory` 四种
/// [`std::io::ErrorKind`]；其它错误退回 `Display` 文本。`EISDIR` 分支固定输出
/// `EISDIR: illegal operation on a directory, read`，不带 `verb` 与路径。
pub(crate) fn node_read_error_message(error: &std::io::Error, verb: &str, path: &str) -> String {
    use std::io::ErrorKind;
    let (code, description) = match error.kind() {
        ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory"),
        ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
        _ => return error.to_string(),
    };
    if code == "EISDIR" {
        format!("{code}: {description}, read")
    } else {
        format!("{code}: {description}, {verb} '{path}'")
    }
}

// 供 `discover` 使用的常量。
pub(crate) const IGNORE_FILES: [&str; 3] = IGNORE_FILE_NAMES;

#[cfg(test)]
mod path_tests {
    use super::{basename, dirname, join_path, relative_path};

    #[test]
    fn node_path_helpers_match_node_semantics() {
        // node:path.join 折叠 "./"。
        assert_eq!(join_path("./sub", "SKILL.md"), "sub/SKILL.md");
        assert_eq!(join_path(".", "SKILL.md"), "SKILL.md");
        assert_eq!(join_path("/abs/./sub", "SKILL.md"), "/abs/sub/SKILL.md");
        // node:path.basename(".") === "."（Rust 的 file_name() 是 None）。
        assert_eq!(basename("."), ".");
        assert_eq!(basename("./"), ".");
        assert_eq!(basename("/"), "");
        // node:path.dirname。
        assert_eq!(dirname("sub/SKILL.md"), "sub");
        assert_eq!(dirname("SKILL.md"), ".");
        assert_eq!(dirname("/"), "/");
        // node:path.relative：相对路径按 cwd 解析，二者同源时结果与 cwd 无关。
        assert_eq!(relative_path("./sub", "sub/SKILL.md"), "SKILL.md");
        assert_eq!(relative_path(".", "SKILL.md"), "SKILL.md");
        assert_eq!(relative_path("/a/b", "/a/b/c/d.md"), "c/d.md");
        assert_eq!(relative_path("/a/b", "/a/c"), "../c");
    }
}
