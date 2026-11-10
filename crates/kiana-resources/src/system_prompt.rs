//! core/system-prompt.ts。
//!
//! 逐字移植 `packages/coding-agent/src/core/system-prompt.ts`（229 行）。
//!
//! 结构：`preamble` 无标签，其余非空 section 包成 `<name>\n…\n</name>`；空 section 被丢弃；
//! 顺序为 `preamble`、`tools`、`rules`、`docs`、`addendum`、`project_context`、`skills`、`cwd`
//! 与自定义（插入序）。`SystemPromptSections` 用 [`IndexMap`] 保序。
//!
//! # 为什么需要结构化 section
//! 首次请求要发送完整 system prompt；会话中途若工具/技能/上下文变化，只需要告诉模型
//! “哪一段变了”。把 prompt 拆成有名字的 section，`diff_system_prompt_sections` 才能生成
//! 增量 patch，而不必重发整段文本。`preamble` 是唯一的例外：它没有 `<...>` 标签，直接
//! 作为正文开头。
//!
//! # 构建流程（`build_system_prompt_sections`）
//! 1. `normalize_build_system_prompt_options` 补齐缺省值（默认工具 `read`/`bash`/`edit`/
//!    `write`，集合字段默认空）；
//! 2. 按 JS `Object.keys` 的顺序校验自定义 section 名：非法或名为 `preamble` 立即报错；
//! 3. 生成内置 section：`preamble`（自定义 prompt 或默认）、`tools`、`rules`、`docs`；
//! 4. 按需追加 `addendum`、`project_context`、`skills`；
//! 5. 固定插入 `cwd`，再合并自定义 section（空内容丢弃）；
//! 6. 除 `preamble` 外，每段包成 `<name>\n...\n</name>`。
//!
//! # 边界与约束
//! - `hidden_tools` 只影响可见性：被隐藏的工具不出现在 `tools` 与 `rules` 里，但技能段仍
//!   会保留（intro 不再点名具体工具，见下方的 reader 选择）；
//! - 自定义 section 与内置 section 同名时会**覆盖内容但保留内置位置**（`IndexMap` 插入
//!   语义）；`preamble` 不允许自定义；
//! - 唯一的错误类型是 [`InvalidSystemPromptSectionName`]，只在自定义 section 名校验失败时
//!   出现；其余输入问题（空集合等）都通过缺省值/空段处理，不报错；
//! - 默认 docs 段里的路径由 `kiana_core::config` 解析，`LazyLock` 保证进程内只求值一次。

use std::sync::LazyLock;

use indexmap::IndexMap;
use kiana_ai::types::{SystemContent, SystemMessage, SystemRole};
use kiana_ai::utils::text::get_system_message_text;
use kiana_ext_api::system_prompt::{
    BuildSystemPromptOptions, ContextFile, NormalizedBuildSystemPromptOptions,
};

use crate::skills::prompt::{SkillReader, format_skills_for_prompt};

/// 有序的系统提示 section 表。TS: `system-prompt.ts#SystemPromptSections`。
pub type SystemPromptSections = IndexMap<String, String>;

/// section 名校验错误。`Display` 逐字等于 TS 的
/// `Invalid system prompt section name: ${name}`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSystemPromptSectionName {
    /// 非法的 section 名。
    pub name: String,
}

impl std::fmt::Display for InvalidSystemPromptSectionName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Invalid system prompt section name: {}",
            self.name
        )
    }
}

impl std::error::Error for InvalidSystemPromptSectionName {}

impl From<InvalidSystemPromptSectionName> for String {
    fn from(error: InvalidSystemPromptSectionName) -> Self {
        error.to_string()
    }
}

/// `buildSystemPromptState` 的结果。TS: `{ content: string; sections?: SystemPromptSections }`。
#[derive(Debug, Clone, PartialEq)]
pub struct SystemPromptState {
    /// 强制 prompt 的正文（非强制时为空串）。
    pub content: String,
    /// 结构化 section（仅非强制时存在）。
    pub sections: Option<SystemPromptSections>,
}

/// JS 规范数组索引字符串（`"0"`、`"1"`…；无前导零；`< 2^32 - 1`）。
///
/// 用来复刻 JS 对象的键枚举顺序：数组索引键按数值升序排在前面，其余键保持插入序。
/// 判定与 `Array.prototype` 一致：空串、非纯 ASCII 数字、多位数带前导零、以及
/// `2^32 - 1` 本身都不算索引。
fn js_array_index(key: &str) -> Option<u64> {
    if key.is_empty() || key.len() > 10 || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u64 = key.parse().ok()?;
    (value < u64::from(u32::MAX)).then_some(value)
}

/// `SYSTEM_PROMPT_SECTION_NAME`：`^[a-z][a-z0-9_-]*$`（JS 无 `m` 时 `$` 只匹配输入末尾）。
///
/// 逐字符实现与正则等价：首字符必须是小写 ASCII 字母，其余只允许小写字母、数字、`_`、`-`；
/// 空串不合法。注意只接受 ASCII，非 ASCII 小写字母也不通过。
fn is_valid_section_name(name: &str) -> bool {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    characters.all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == '_'
            || character == '-'
    })
}

/// 把输入归一化为集合字段齐全、可变（供扩展修改）的形状。
///
/// TS: `system-prompt.ts#normalizeBuildSystemPromptOptions`。
///
/// `selected_tools` 缺省为 `["read", "bash", "edit", "write"]`；`hidden_tools`、
/// `tool_snippets`、`tool_guidelines`、`prompt_guidelines`、`append_system_prompt`、
/// `sections`、`context_files`、`skills` 缺省为空；`custom_prompt`、`force_system_prompt`
/// 与 `cwd` 原样透传。返回值各字段都是 `Vec`/`IndexMap`，扩展可在再次构建前直接修改。
pub fn normalize_build_system_prompt_options(
    input: &BuildSystemPromptOptions,
) -> NormalizedBuildSystemPromptOptions {
    NormalizedBuildSystemPromptOptions {
        custom_prompt: input.custom_prompt.clone(),
        force_system_prompt: input.force_system_prompt.clone(),
        selected_tools: input.selected_tools.clone().unwrap_or_else(|| {
            ["read", "bash", "edit", "write"]
                .map(str::to_string)
                .to_vec()
        }),
        hidden_tools: input.hidden_tools.clone().unwrap_or_default(),
        tool_snippets: input.tool_snippets.clone().unwrap_or_default(),
        tool_guidelines: input.tool_guidelines.clone().unwrap_or_default(),
        prompt_guidelines: input.prompt_guidelines.clone().unwrap_or_default(),
        append_system_prompt: input.append_system_prompt.clone().unwrap_or_default(),
        sections: input.sections.clone().unwrap_or_default(),
        cwd: input.cwd.clone(),
        context_files: input.context_files.clone().unwrap_or_default(),
        skills: input.skills.clone().unwrap_or_default(),
    }
}

/// TS: `system-prompt.ts#renderProjectContext`。
///
/// 首行是固定标题，之后每个文件一段 `<project_instructions path="...">`；段与段之间用空行
/// 连接。`path` 与 `content` 原样拼入，**不做 XML 转义**（转义会改变指令文本）；内容里若含
/// `</project_instructions>` 会破坏结构，调用方需自行保证来源可信。
fn render_project_context(context_files: &[ContextFile]) -> String {
    let mut parts: Vec<String> = vec!["Project-specific instructions and guidelines:".to_string()];
    for file in context_files {
        parts.push(format!(
            "<project_instructions path=\"{}\">\n{}\n</project_instructions>",
            file.path, file.content
        ));
    }
    parts.join("\n\n")
}

/// TS: `system-prompt.ts#buildRules`。
///
/// 规则按固定顺序收集：
/// 1. shell 提示：选中 `bash` 和/或 `powershell`，且都没有选中 `grep`/`find`/`ls` 时，
///    提示用 shell 做文件查找；
/// 2. 每个已声明工具自己的指南（按 `selected_tools` 顺序取 `tool_guidelines`）；
/// 3. `prompt_guidelines`；
/// 4. 两条固定默认规则（简洁、显示文件路径）。
///
/// 每条先做 JS 语义的 `trim`，空串与“trim 后完全相同”的重复项跳过（首次出现的保留）。
/// 输出为每行 `- <rule>`，用换行连接。
fn build_rules(
    selected_tools: &[String],
    tool_guidelines: &IndexMap<String, Vec<String>>,
    prompt_guidelines: &[String],
) -> String {
    let mut rules: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let add_rule =
        |rule: &str, rules: &mut Vec<String>, seen: &mut std::collections::HashSet<String>| {
            let normalized = kiana_json::js_string::js_trim(rule).to_string();
            if normalized.is_empty() || !seen.insert(normalized.clone()) {
                return;
            }
            rules.push(normalized);
        };

    let has_bash = selected_tools.iter().any(|name| name == "bash");
    let has_power_shell = selected_tools.iter().any(|name| name == "powershell");
    let has_grep = selected_tools.iter().any(|name| name == "grep");
    let has_find = selected_tools.iter().any(|name| name == "find");
    let has_ls = selected_tools.iter().any(|name| name == "ls");

    if (has_bash || has_power_shell) && !has_grep && !has_find && !has_ls {
        if has_bash && has_power_shell {
            add_rule(
                "Use bash or PowerShell for file operations like listing, searching, and finding files",
                &mut rules,
                &mut seen,
            );
        } else if has_power_shell {
            add_rule(
                "Use PowerShell for file operations like listing, searching, and finding files",
                &mut rules,
                &mut seen,
            );
        } else {
            add_rule(
                "Use bash for file operations like ls, rg, find",
                &mut rules,
                &mut seen,
            );
        }
    }

    for name in selected_tools {
        if let Some(guidelines) = tool_guidelines.get(name) {
            for rule in guidelines {
                add_rule(rule, &mut rules, &mut seen);
            }
        }
    }
    for rule in prompt_guidelines {
        add_rule(rule, &mut rules, &mut seen);
    }
    add_rule("Be concise in your responses", &mut rules, &mut seen);
    add_rule(
        "Show file paths clearly when working with files",
        &mut rules,
        &mut seen,
    );
    rules
        .iter()
        .map(|rule| format!("- {rule}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 默认 preamble；固定文案，首次构建时分配一次（`LazyLock`）。
static DEFAULT_PREAMBLE: LazyLock<String> = LazyLock::new(|| {
    "You are an expert coding assistant operating inside kiana, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.".to_string()
});

/// 默认 docs 段；路径由 `kiana_core::config` 解析，`LazyLock` 保证只求值一次，避免每次
/// 构建 prompt 都重复解析。
static DEFAULT_DOCS: LazyLock<String> = LazyLock::new(|| {
    format!(
        "Kiana documentation (read only when the user asks about kiana itself, its SDK, extensions, themes, skills, or TUI):
- Main documentation: {}
- Additional docs: {}
- Examples: {} (extensions, custom tools, SDK)
- When reading kiana docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory
- When asked about: extensions (docs/extensions.md, examples/extensions/), themes (docs/themes.md), skills (docs/skills.md), prompt templates (docs/prompt-templates.md), TUI components (docs/tui.md), keybindings (docs/keybindings.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md), adding models (docs/models.md), kiana packages (docs/packages.md), environment variables (docs/environment-variables.md), MCP servers (docs/mcp.md), codemode scripts and non-LLM models such as classifiers and image models (docs/codemode.md)
- When working on kiana topics, read the docs and examples, and follow .md cross-references before implementing
- Always read kiana .md files completely and follow links to related docs (e.g., tui.md for TUI API details)",
        kiana_core::config::get_readme_path().to_string_lossy(),
        kiana_core::config::get_docs_path().to_string_lossy(),
        kiana_core::config::get_examples_path().to_string_lossy(),
    )
});

/// 构造有序、可独立替换的系统提示 section。
///
/// TS: `system-prompt.ts#buildSystemPromptSections`。自定义 section 名非法（或为 `preamble`）
/// 时返回 [`InvalidSystemPromptSectionName`]。
///
/// 返回的 map 中，`preamble` 的值是裸文本；其它值都已包成 `<name>\n...\n</name>`。
/// `force_system_prompt` 在这里**不生效**（由 [`build_system_prompt_state`] 提前短路），
/// 调用方若想尊重强制 prompt 应使用后者。
///
/// 空内容的自定义 section 会被丢弃；与内置 section 同名的自定义项会覆盖内容但保留内置
/// 位置。校验顺序复刻 JS `Object.keys`：数组索引形式的键按数值升序先校验，其余按插入序，
/// 多个非法名同时存在时报告第一个。
pub fn build_system_prompt_sections(
    input: &BuildSystemPromptOptions,
) -> Result<SystemPromptSections, InvalidSystemPromptSectionName> {
    let options = normalize_build_system_prompt_options(input);
    let NormalizedBuildSystemPromptOptions {
        custom_prompt,
        selected_tools,
        hidden_tools,
        tool_snippets,
        tool_guidelines,
        prompt_guidelines,
        append_system_prompt,
        sections: custom_sections,
        cwd,
        context_files,
        skills,
        ..
    } = options;

    // `Object.keys` 的顺序：数组索引键（0..2^32-2）按数值升序在前，其余保持插入序。
    // 多个非法名字同时存在时按上述键序报第一个；顺序必须固定（测试锁定）。
    let mut section_keys: Vec<(u64, usize, &String)> = custom_sections
        .keys()
        .enumerate()
        .map(|(position, name)| match js_array_index(name) {
            Some(index) => (index, position, name),
            None => (u64::MAX, position, name),
        })
        .collect();
    section_keys.sort_by_key(|(index, position, _)| (*index, *position));
    for (_, _, name) in section_keys {
        if !is_valid_section_name(name) || name == "preamble" {
            return Err(InvalidSystemPromptSectionName { name: name.clone() });
        }
    }

    // “已声明”= 被选中且未被隐藏；后续 tools / rules / skills 都以它为准，顺序保持
    // `selected_tools` 的原顺序。
    let declared_tools: Vec<&String> = selected_tools
        .iter()
        .filter(|name| !hidden_tools.contains(name))
        .collect();

    let mut prompt_sections: IndexMap<String, String> = IndexMap::new();
    // 自定义 prompt 非空时整个替换默认 preamble，并且不再生成 tools/rules/docs；
    // 空串按“未提供”处理，回落到默认 preamble。
    match custom_prompt {
        Some(custom_prompt) if !custom_prompt.is_empty() => {
            prompt_sections.insert("preamble".to_string(), custom_prompt);
        }
        _ => {
            prompt_sections.insert("preamble".to_string(), (*DEFAULT_PREAMBLE).clone());
            // 只有提供非空 snippet 的工具才值得列给模型；没有则显示 `(none)`。
            let visible_tools: Vec<&&String> = declared_tools
                .iter()
                .filter(|name| {
                    tool_snippets
                        .get(name.as_str())
                        .is_some_and(|snippet| !snippet.is_empty())
                })
                .collect();
            let tools = if visible_tools.is_empty() {
                "(none)".to_string()
            } else {
                visible_tools
                    .iter()
                    .map(|name| {
                        format!(
                            "- {}: {}",
                            name,
                            tool_snippets
                                .get(name.as_str())
                                .map(String::as_str)
                                .unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            prompt_sections.insert(
                "tools".to_string(),
                format!(
                    "{tools}\n\nIn addition to the tools above, you may have access to other custom tools depending on the project."
                ),
            );
            // rules 同样只看已声明的可见工具；被隐藏工具的指南不会泄漏进来。
            prompt_sections.insert(
                "rules".to_string(),
                build_rules(
                    &declared_tools
                        .iter()
                        .map(|name| (*name).clone())
                        .collect::<Vec<_>>(),
                    &tool_guidelines,
                    &prompt_guidelines,
                ),
            );
            prompt_sections.insert("docs".to_string(), (*DEFAULT_DOCS).clone());
        }
    }

    if !append_system_prompt.is_empty() {
        prompt_sections.insert("addendum".to_string(), append_system_prompt);
    }
    if !context_files.is_empty() {
        prompt_sections.insert(
            "project_context".to_string(),
            render_project_context(&context_files),
        );
    }

    // 隐藏的 reader 仍能经另一个工具触达：技能保留，但 intro 不点名工具。
    // 选择顺序：优先可见的 `read`，其次可见的 `bash`；若两者都被隐藏但至少有一个仍被
    // 选中，则用 `Indirect`（技能段照常输出，只是文案不点名工具）；两者都没选中时不生成
    // 技能段。
    let readers = ["read", "bash"];
    let skill_file_read_tool = readers
        .iter()
        .find(|tool| declared_tools.iter().any(|name| name.as_str() == **tool))
        .copied()
        .map(SkillReader::from_tool)
        .or_else(|| {
            readers
                .iter()
                .any(|tool| selected_tools.iter().any(|name| name == tool))
                .then_some(SkillReader::Indirect)
        });
    if let Some(reader) = skill_file_read_tool
        && !skills.is_empty()
    {
        let skills_prompt =
            kiana_json::js_string::js_trim(&format_skills_for_prompt(&skills, reader)).to_string();
        if !skills_prompt.is_empty() {
            prompt_sections.insert("skills".to_string(), skills_prompt);
        }
    }

    // 纯文本替换：把 Windows 反斜杠统一成 `/` 便于展示，不做任何路径解析。
    prompt_sections.insert("cwd".to_string(), cwd.replace('\\', "/"));
    // 自定义 section 最后合并：空内容丢弃；同名会覆盖内置内容但保留内置位置
    // （IndexMap::insert 对已存在的键只替换值，不改变顺序）。
    for (name, content) in custom_sections {
        if !content.is_empty() {
            prompt_sections.insert(name, content);
        }
    }

    // `preamble` 理论上一定存在（上面的分支都会插入）；这里保留防御式回退：若没有，
    // 返回空表而不是 panic。
    let Some(preamble) = prompt_sections.get("preamble").cloned() else {
        return Ok(SystemPromptSections::new());
    };
    // 除 preamble 外统一包标签；标签名直接来自已校验的 section 名。
    let mut sections: SystemPromptSections = IndexMap::new();
    sections.insert("preamble".to_string(), preamble);
    for (name, content) in prompt_sections {
        if name != "preamble" {
            sections.insert(name.clone(), format!("<{name}>\n{content}\n</{name}>"));
        }
    }
    Ok(sections)
}

/// 完整 prompt 状态：强制 prompt 不透明地放在 `content` 且没有 sections；否则 `content` 为空、
/// 结构化 sections 承载 prompt。
///
/// TS: `system-prompt.ts#buildSystemPromptState`。
///
/// `force_system_prompt` 为 `Some` 时直接返回：即使内容是空串也照用，不校验 section、
/// 不包装标签、不生成 sections；否则 `content` 为空串，结构化结果放在 `sections`。
pub fn build_system_prompt_state(
    input: &BuildSystemPromptOptions,
) -> Result<SystemPromptState, InvalidSystemPromptSectionName> {
    if let Some(force_system_prompt) = &input.force_system_prompt {
        return Ok(SystemPromptState {
            content: force_system_prompt.clone(),
            sections: None,
        });
    }
    Ok(SystemPromptState {
        content: String::new(),
        sections: Some(build_system_prompt_sections(input)?),
    })
}

/// 构造系统提示文本（与 transcript 回放 system 消息的渲染完全一致）。
///
/// TS: `system-prompt.ts#buildSystemPrompt`。
///
/// 实现上先取 [`build_system_prompt_state`]，再把 sections 转成
/// `IndexMap<String, Option<String>>`（全部为 `Some`），交给
/// `kiana_ai::utils::text::get_system_message_text` 渲染：正文在前，各 section 按顺序用
/// 空行连接，空段被过滤。`timestamp` 只是占位，本函数不把它写入会话历史。
pub fn build_system_prompt(
    input: &BuildSystemPromptOptions,
) -> Result<String, InvalidSystemPromptSectionName> {
    let state = build_system_prompt_state(input)?;
    let sections: Option<IndexMap<String, Option<String>>> = state.sections.map(|sections| {
        sections
            .into_iter()
            .map(|(key, value)| (key, Some(value)))
            .collect()
    });
    let message = SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(state.content),
        sections,
        tools_added: None,
        tools_removed: None,
        timestamp: 0,
    };
    Ok(get_system_message_text(&message))
}

/// 对比模型当前持有的 sections 与期望 sections，生成 `SystemMessage.sections` patch；
/// 无变化时返回 `None`。`None` 值表示删除该 section。
///
/// TS: `system-prompt.ts#diffSystemPromptSections`。patch 键序：先新增/修改（按 `current`
/// 顺序），再删除（按 `previous` 顺序）。
///
/// `previous` 的值是模型当前持有的内容：`None` 表示该 section 已被删除，`Some(text)` 表示
/// 存在。只有 `Some(相同文本)` 才视为未变；因此“已删除 → 重新出现”也会进 patch。
///
/// patch 的值直接取自 `current`（对非 `preamble` 项已经含 `<name>` 包装），删除项为
/// `None`；调用方按顺序覆盖即可。完全没有变化时返回 `None`，避免发送空更新。
pub fn diff_system_prompt_sections(
    previous: &IndexMap<String, Option<String>>,
    current: &SystemPromptSections,
) -> Option<IndexMap<String, Option<String>>> {
    let mut patch: IndexMap<String, Option<String>> = IndexMap::new();
    for (name, text) in current {
        if previous.get(name) != Some(&Some(text.clone())) {
            patch.insert(name.clone(), Some(text.clone()));
        }
    }
    for name in previous.keys() {
        if !current.contains_key(name) {
            patch.insert(name.clone(), None);
        }
    }
    if patch.is_empty() { None } else { Some(patch) }
}

/// 生成「必须用某语言回答」的 system prompt 片段。
///
/// `en` 返回 `None`（英文是默认，不需要额外规则）。未知语言返回通用规则。
///
/// 这是会话层（`AgentSession::rebuild_system_prompt`）与 TUI i18n 共用的唯一实现：
/// `output_language` 与 UI locale 分离，只影响模型回答语言。
pub fn output_language_system_rule(language: &str) -> Option<String> {
    let normalized = language.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "en" | "en-us" | "english" => None,
        "zh" | "zh-cn" | "chinese" | "中文" | "简体中文" => Some(
            "Always respond in Chinese (简体中文), even when the user writes in another language. \
             Keep code, file paths, and command names unchanged."
                .to_string(),
        ),
        "zh-tw" | "zh-hk" | "繁體中文" | "繁体中文" => Some(
            "Always respond in Traditional Chinese (繁體中文), even when the user writes in \
             another language. Keep code, file paths, and command names unchanged."
                .to_string(),
        ),
        "ja" | "ja-jp" | "japanese" => Some(
            "Always respond in Japanese (日本語), even when the user writes in another language. \
             Keep code, file paths, and command names unchanged."
                .to_string(),
        ),
        "de" | "de-de" | "german" => Some(
            "Always respond in German, even when the user writes in another language. \
             Keep code, file paths, and command names unchanged."
                .to_string(),
        ),
        "fr" | "fr-fr" | "french" => Some(
            "Always respond in French, even when the user writes in another language. \
             Keep code, file paths, and command names unchanged."
                .to_string(),
        ),
        "pt" | "pt-br" | "portuguese" => Some(
            "Always respond in Portuguese, even when the user writes in another language. \
             Keep code, file paths, and command names unchanged."
                .to_string(),
        ),
        "ru" | "ru-ru" | "russian" => Some(
            "Always respond in Russian, even when the user writes in another language. \
             Keep code, file paths, and command names unchanged."
                .to_string(),
        ),
        other => Some(format!(
            "Always respond in the language identified by `{other}`, even when the user writes \
             in another language. Keep code, file paths, and command names unchanged."
        )),
    }
}
