//! BuildSystemPromptOptions/NormalizedBuildSystemPromptOptions 声明，另含 `Skill`。
//!
//! TS 源：`core/system-prompt.ts`（选项类型）与 `core/skills.ts`（`Skill` 接口）。
//!
//! # 为什么 `Skill` 在这里
//!
//! `BuildSystemPromptOptions.skills` 需要 `core/skills.ts#Skill`，而 `skills.ts` 属于
//! `kiana-resources`（L10）；`kiana-ext-api` 是 L7，不能反向依赖。`ARCHITECTURE.md` §3.5 的
//! “契约类型上移”把**只读接口**移到这里，`kiana-resources::skills` 只再导出。
//! `LoadSkillsResult`/`loadSkills`/`formatSkillsForPrompt`/`SkillFrontmatter` 仍留在
//! `kiana-resources`。
//!
//! `normalize_build_system_prompt_options`/`build_system_prompt` 的实现留在
//! `kiana-resources::system_prompt`，本模块只有声明。
//!
//! # 在系统中的位置
//!
//! `kiana-session` 组装 [`BuildSystemPromptOptions`]（工具清单、guidelines、cwd、技能与
//! 上下文文件），交给 `kiana-resources::system_prompt` 的
//! `normalize_build_system_prompt_options` / `build_system_prompt_sections` 渲染。
//! `kiana-extensions` 还会把归一化结果序列化成 JSON 交给扩展的 JS 侧
//! （`renderSystemPrompt` 再从 JSON 反序列化回来），所以字段名（camelCase）与
//! “可选键省略”规则同时是跨语言契约。
//!
//! # 形状约定
//!
//! - 字段声明顺序 = TS 接口顺序，也就是 JSON 键顺序；不要重排。
//! - `Option` + `skip_serializing_if = "Option::is_none"` 对应 TS 的可选属性：
//!   `None` 时省略键（等价 `undefined`）。[`NormalizedBuildSystemPromptOptions`] 的集合
//!   字段是必填，归一化负责补默认值。
//! - `cwd` 在输入与归一化形状里都必填；`Default` 派生的空串只用于 `..Default::default()`
//!   构造，空 cwd 会被渲染器原样写进 `<cwd>` 段。
//! - 字段全部 `pub`，扩展/会话可直接读写；新增字段时还要同步
//!   `kiana-resources` 与 `kiana-extensions::runner` 的两份归一化映射，否则字段会在
//!   归一化时被丢弃。

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use kiana_core::source_info::SourceInfo;

/// 一个已加载的技能。
///
/// TS: `core/skills.ts#Skill`。字段顺序逐字为
/// `name`、`description`、`filePath`、`baseDir`、`sourceInfo`、`disableModelInvocation`。
///
/// 本结构只是数据载体：字段命名与长度限制由 `kiana-resources::skills::validate` 在加载时
/// 校验，这里不会拒绝不合规的值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    /// 技能名（Agent Skills 规范：小写 a-z、0-9、连字符，最长 64）。
    pub name: String,
    /// 描述（最长 1024）。
    pub description: String,
    /// `SKILL.md` 的绝对路径。
    pub file_path: String,
    /// 技能目录（`SKILL.md` 所在目录）。
    pub base_dir: String,
    /// 来源信息。
    pub source_info: SourceInfo,
    /// 为 true 时该技能不参与模型可见的技能列表。
    pub disable_model_invocation: bool,
}

/// 已预加载的上下文文件（`contextFiles?: Array<{path, content}>`）。
///
/// TS: `core/system-prompt.ts#BuildSystemPromptOptions.contextFiles` 的内联对象。
/// Rust-only helper：给内联对象一个字面名字，字段顺序即 TS 对象键顺序。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextFile {
    /// 文件路径（原样进入 `<project_instructions path="...">`）。
    pub path: String,
    /// 文件内容；渲染时原样嵌入 `<project_instructions>`，不做 XML 转义。
    pub content: String,
}

/// 构建系统提示的输入选项。
///
/// TS: `core/system-prompt.ts#BuildSystemPromptOptions`。字段顺序即 TS 声明顺序；
/// 可选字段序列化时省略（`undefined`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildSystemPromptOptions {
    /// 自定义系统提示（替换默认前缀）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_prompt: Option<String>,
    /// `before_agent_start` handler 设置的完整提示替换。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_system_prompt: Option<String>,
    /// 要包含进提示的工具。缺省 `[read, bash, edit, write]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_tools: Option<Vec<String>>,
    /// 声明被 `prepareLoadout` 隐藏的工具（只能经其它工具触达）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_tools: Option<Vec<String>>,
    /// 每个工具的一行摘要。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_snippets: Option<IndexMap<String, String>>,
    /// 每个工具贡献的 guideline 条目。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_guidelines: Option<IndexMap<String, Vec<String>>>,
    /// 追加到默认规则后的 guideline 条目。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_guidelines: Option<Vec<String>>,
    /// 用户配置追加在 project context/skills/cwd 之前的文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append_system_prompt: Option<String>,
    /// 按 tag 名追加的 XML 包裝 prompt 段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<IndexMap<String, String>>,
    /// 工作目录。
    pub cwd: String,
    /// 预加载的上下文文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_files: Option<Vec<ContextFile>>,
    /// 预加载的技能。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<Skill>>,
}

/// 归一化后的系统提示选项：集合字段补全（可变、供扩展修改）。
///
/// TS: `core/system-prompt.ts#NormalizedBuildSystemPromptOptions`。
///
/// 归一化规则（实现位于 `kiana-resources::system_prompt#normalize_build_system_prompt_options`）：
/// `selectedTools` 缺省 `["read","bash","edit","write"]`，其余集合缺省为空，
/// `appendSystemPrompt` 缺省空串；`customPrompt`/`forceSystemPrompt` 保持可选。
///
/// 归一化只做“补默认值 + 克隆”，不校验字段、不去重、不排序；自定义 section 名的合法性
/// 要到 `build_system_prompt_sections` 才检查（非法时返回错误）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedBuildSystemPromptOptions {
    /// 自定义系统提示（替换默认前缀）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_prompt: Option<String>,
    /// `before_agent_start` handler 设置的完整提示替换。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_system_prompt: Option<String>,
    /// 要包含进提示的工具（必填集合）。
    pub selected_tools: Vec<String>,
    /// 声明被隐藏的工具（必填集合）。
    pub hidden_tools: Vec<String>,
    /// 每个工具的一行摘要（必填映射）。
    pub tool_snippets: IndexMap<String, String>,
    /// 每个工具贡献的 guideline 条目（必填映射）。
    pub tool_guidelines: IndexMap<String, Vec<String>>,
    /// 追加的 guideline 条目（必填集合）。
    pub prompt_guidelines: Vec<String>,
    /// 追加文本（必填，缺省空串）。
    pub append_system_prompt: String,
    /// 自定义 XML 段（必填映射）。
    pub sections: IndexMap<String, String>,
    /// 工作目录。
    pub cwd: String,
    /// 上下文文件（必填集合）。
    pub context_files: Vec<ContextFile>,
    /// 技能（必填集合）。
    pub skills: Vec<Skill>,
}
