//! `format_skills_for_prompt` / `escape_xml`。
//!
//! 逐字移植 `packages/coding-agent/src/core/skills.ts#formatSkillsForPrompt` / `#escapeXml`。
//!
//! # 职责边界
//! 这里是纯函数：不读文件、不做 IO，也不排序技能——XML 的先后就是传入切片的先后。
//! 调用方 `crate::system_prompt::build_system_prompt_sections` 先根据“已声明（选中且未
//! 隐藏）”的工具决定 [`SkillReader`]，再把结果 trim 后放进 `skills` section。
//!
//! # 输出结构
//! 有可见技能时返回以两个换行开头的整段文本（便于直接拼接进 prompt）：说明文字 +
//! `<available_skills>`，每个技能一个 `<skill>` 块（`name` / `description` / `location`）。
//! 三个字段都经过 [`escape_xml`]。没有可见技能时返回空串，而不是只含外壳的 XML。
//!
//! # 为什么隐藏技能只影响这里
//! `disable_model_invocation` 只控制“是否把技能告诉模型”，不控制技能是否被发现和加载；
//! 因此过滤放在渲染阶段，`discover` 仍然会产出这些技能。

use crate::skills::Skill;
use crate::skills::definition::SkillDefinition;

/// 技能索引默认预算（字符）：1% × 200k tokens × 4 chars/token。
///
/// 参考 Claude Code `SkillTool/prompt.ts` 的 `DEFAULT_CHAR_BUDGET`。
pub const DEFAULT_SKILL_INDEX_BUDGET_CHARS: usize = 8_000;
/// 单条描述上限（字符）。bundled 技能不截断。
pub const MAX_SKILL_DESCRIPTION_CHARS: usize = 250;
/// 预算不足时，非 bundled 描述至少保留这么多字符；低于这个值只保留名字与位置。
pub const MIN_SKILL_DESCRIPTION_CHARS: usize = 20;

/// 按模型上下文窗口计算技能索引预算：`tokens × 4 × 1%`。
///
/// 未知窗口用 [`DEFAULT_SKILL_INDEX_BUDGET_CHARS`]（200k 窗口的 1% 恰好是 8000）。
pub fn skill_index_budget_chars(context_window_tokens: Option<u64>) -> usize {
    match context_window_tokens {
        Some(tokens) if tokens > 0 => {
            let budget = (tokens as f64 * 4.0 * 0.01).floor();
            if budget.is_finite() && budget > 0.0 {
                budget as usize
            } else {
                DEFAULT_SKILL_INDEX_BUDGET_CHARS
            }
        }
        _ => DEFAULT_SKILL_INDEX_BUDGET_CHARS,
    }
}

/// 按字符截断描述（不是字节；跨平台确定性）。
fn truncate_description(description: &str, max_chars: usize) -> String {
    if description.chars().count() <= max_chars {
        return description.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut truncated: String = description.chars().take(max_chars - 1).collect();
    truncated.push('…');
    truncated
}

/// 加载技能文件的工具（决定 intro 文案）。TS: `formatSkillsForPrompt` 的 `fileReadTool`。
///
/// 调用方按 `read` → `bash` 的优先级从已声明的工具中挑选；若两者都未声明但至少一个被
/// 选中，则用 [`SkillReader::Indirect`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillReader {
    /// `"read"`：read 工具可见。
    #[default]
    Read,
    /// `"bash"`：bash 工具可见（read 被隐藏或未选中）。
    Bash,
    /// `"indirect"`：可见的 reader 被隐藏，但技能仍能经另一个工具触达。
    Indirect,
}

impl SkillReader {
    /// 把工具名映射为 reader 种类（`read` / 其它 → `Read` 由调用方只传 `read`/`bash`）。
    ///
    /// 只识别 `"bash"`，其余（含 `"read"`）一律落到 [`SkillReader::Read`]。
    /// [`SkillReader::Indirect`] 不会由本函数产生，由调用方直接构造。
    pub(crate) fn from_tool(tool: &str) -> Self {
        match tool {
            "bash" => SkillReader::Bash,
            _ => SkillReader::Read,
        }
    }
}

/// 把技能列表格式化为 system prompt 的 XML 片段。
///
/// 隐藏技能（`disable_model_invocation`）不进入列表；没有可见技能时返回空串。
/// TS: `skills.ts#formatSkillsForPrompt`。
///
/// 返回值的第一行是两个换行，调用方通常在插入 section 前自行 trim；函数自身不添加尾随
/// 换行。技能顺序原样保留；非 bundled 描述按 [`MAX_SKILL_DESCRIPTION_CHARS`] 截断，
/// bundled 不截断，总长度按 [`DEFAULT_SKILL_INDEX_BUDGET_CHARS`] 控制。`location` 用的是
/// `Skill::file_path` 而不是 `base_dir`。
pub fn format_skills_for_prompt(skills: &[Skill], reader: SkillReader) -> String {
    // 隐藏只影响“是否告知模型”，技能本身已由加载阶段保留；这里过滤后若为空则整体不发。
    let visible: Vec<&Skill> = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect();

    if visible.is_empty() {
        return String::new();
    }

    let entries: Vec<PromptSkillEntry<'_>> = visible
        .into_iter()
        .map(|skill| PromptSkillEntry {
            name: &skill.name,
            capped_description: Some(cap_description(&skill.description, is_bundled_skill(skill))),
            location: &skill.file_path,
            bundled: is_bundled_skill(skill),
        })
        .collect();
    render_entries(
        &prompt_prefix(reader),
        &entries,
        DEFAULT_SKILL_INDEX_BUDGET_CHARS,
    )
}

/// 一个待渲染的技能条目。
struct PromptSkillEntry<'a> {
    name: &'a str,
    /// 已按单条上限处理过的描述（`None` 表示原始技能没有描述）。
    capped_description: Option<String>,
    location: &'a str,
    bundled: bool,
}

/// 按扩展定义渲染技能索引，并应用预算。
///
/// - `budget_chars`：通常来自 [`skill_index_budget_chars`]；
/// - `touched`：条件 `paths` 的已触达文件；为空时无 `paths` 的技能可见；
/// - bundled 技能永不截断（描述与条目都保留）；
/// - 非 bundled 先按单条 [`MAX_SKILL_DESCRIPTION_CHARS`] 截断，再在总预算内均分描述长度；
///   低于 [`MIN_SKILL_DESCRIPTION_CHARS`] 时只保留 name/location；仍超预算则从尾部丢弃
///   非 bundled 条目。
pub fn format_skill_definitions_for_prompt(
    definitions: &[SkillDefinition],
    reader: SkillReader,
    budget_chars: usize,
    touched: &[std::path::PathBuf],
) -> String {
    format_skill_definitions_for_prompt_with_cwd(definitions, reader, budget_chars, touched, None)
}

/// [`format_skill_definitions_for_prompt`] 的会话 cwd 版本（`paths` 归一化使用它）。
pub fn format_skill_definitions_for_prompt_with_cwd(
    definitions: &[SkillDefinition],
    reader: SkillReader,
    budget_chars: usize,
    touched: &[std::path::PathBuf],
    cwd: Option<&std::path::Path>,
) -> String {
    let visible: Vec<&SkillDefinition> = definitions
        .iter()
        .filter(|definition| {
            crate::skills::definition::skill_visible_for_model_in(definition, touched, cwd)
        })
        .collect();
    let entries: Vec<PromptSkillEntry<'_>> = visible
        .into_iter()
        .map(|definition| PromptSkillEntry {
            name: &definition.skill.name,
            capped_description: Some(cap_description(
                &definition.skill.description,
                definition.is_bundled(),
            )),
            location: &definition.skill.file_path,
            bundled: definition.is_bundled(),
        })
        .collect();
    render_entries(&prompt_prefix(reader), &entries, budget_chars)
}

/// 是否 bundled（技能索引不截断的来源）。
fn is_bundled_skill(skill: &Skill) -> bool {
    skill.source_info.source == "builtin" || skill.source_info.path.starts_with("builtin:")
}

/// 单条描述上限：bundled 原样，其它按 [`MAX_SKILL_DESCRIPTION_CHARS`] 截断。
fn cap_description(description: &str, bundled: bool) -> String {
    if bundled {
        description.to_string()
    } else {
        truncate_description(description, MAX_SKILL_DESCRIPTION_CHARS)
    }
}

fn prompt_prefix(reader: SkillReader) -> Vec<String> {
    let reader_line = match reader {
        SkillReader::Read => {
            "Use the read tool to load a skill's file when the task matches its description."
        }
        SkillReader::Bash => {
            "Use bash to load a skill's file when the task matches its description."
        }
        SkillReader::Indirect => "Load a skill's file when the task matches its description.",
    };
    vec![
        "\n\nThe following skills provide specialized instructions for specific tasks.".to_string(),
        reader_line.to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ]
}

/// 渲染一个技能块。`description: None` 表示省略 `<description>` 行。
fn render_skill_block(name: &str, description: Option<&str>, location: &str) -> Vec<String> {
    let mut lines = vec![
        "  <skill>".to_string(),
        format!("    <name>{}</name>", escape_xml(name)),
    ];
    if let Some(description) = description {
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(description)
        ));
    }
    lines.push(format!("    <location>{}</location>", escape_xml(location)));
    lines.push("  </skill>".to_string());
    lines
}

fn assemble(prefix: &[String], blocks: &[Vec<String>]) -> String {
    let mut lines = prefix.to_vec();
    for block in blocks {
        lines.extend(block.iter().cloned());
    }
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

/// 字符长度（与描述上限一致；确定性、不依赖 locale）。
fn chars_len(text: &str) -> usize {
    text.chars().count()
}

/// 按预算渲染条目。
fn render_entries(
    prefix: &[String],
    entries: &[PromptSkillEntry<'_>],
    budget_chars: usize,
) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let build = |descriptions: &[Option<String>]| -> Vec<Vec<String>> {
        entries
            .iter()
            .zip(descriptions)
            .map(|(entry, description)| {
                render_skill_block(entry.name, description.as_deref(), entry.location)
            })
            .collect()
    };
    // 第一轮：完整描述（非 bundled 已按单条上限处理）。
    let full_descriptions: Vec<Option<String>> = entries
        .iter()
        .map(|entry| entry.capped_description.clone())
        .collect();
    let full_blocks = build(&full_descriptions);
    let full = assemble(prefix, &full_blocks);
    if chars_len(&full) <= budget_chars {
        return full;
    }

    // 第二轮：bundled 完整，剩余预算均分给非 bundled 描述。
    let non_bundled = entries.iter().filter(|entry| !entry.bundled).count();
    if non_bundled == 0 {
        // 全是 bundled：需求明确“bundled 不截断”，允许超出预算。
        return full;
    }
    let bundled_blocks: Vec<Vec<String>> = entries
        .iter()
        .map(|entry| {
            render_skill_block(
                entry.name,
                if entry.bundled {
                    entry.capped_description.as_deref()
                } else {
                    None
                },
                entry.location,
            )
        })
        .collect();
    let base = assemble(prefix, &bundled_blocks);
    let base_len = chars_len(&base);
    let available = budget_chars.saturating_sub(base_len);
    let mut per_description = available / non_bundled;
    per_description = per_description.min(MAX_SKILL_DESCRIPTION_CHARS);

    if per_description >= MIN_SKILL_DESCRIPTION_CHARS {
        // 描述行有固定开销；先按均分裁剪，超了再收敛（最多几轮，结果确定）。
        for _ in 0..8 {
            let descriptions: Vec<Option<String>> = entries
                .iter()
                .map(|entry| {
                    if entry.bundled {
                        entry.capped_description.clone()
                    } else {
                        entry
                            .capped_description
                            .as_deref()
                            .map(|description| truncate_description(description, per_description))
                    }
                })
                .collect();
            let blocks = build(&descriptions);
            let rendered = assemble(prefix, &blocks);
            let length = chars_len(&rendered);
            if length <= budget_chars {
                return rendered;
            }
            let overshoot = length - budget_chars;
            let step = overshoot.div_ceil(non_bundled).max(1);
            if per_description <= MIN_SKILL_DESCRIPTION_CHARS + step {
                break;
            }
            per_description -= step;
        }
    }

    // 第三轮：非 bundled 只保留 name/location；仍超预算时从尾部丢弃非 bundled。
    let mut keep: Vec<bool> = entries.iter().map(|_| true).collect();
    loop {
        let descriptions: Vec<Option<String>> = entries
            .iter()
            .zip(&keep)
            .map(|(entry, kept)| {
                if !kept {
                    None
                } else if entry.bundled {
                    entry.capped_description.clone()
                } else {
                    None
                }
            })
            .collect();
        let active: Vec<usize> = (0..entries.len()).filter(|index| keep[*index]).collect();
        let blocks: Vec<Vec<String>> = active
            .iter()
            .map(|index| {
                render_skill_block(
                    entries[*index].name,
                    descriptions[*index].as_deref(),
                    entries[*index].location,
                )
            })
            .collect();
        let rendered = assemble(prefix, &blocks);
        if chars_len(&rendered) <= budget_chars {
            return rendered;
        }
        let Some(last_non_bundled) = active.iter().rev().find(|index| !entries[**index].bundled)
        else {
            // 只剩 bundled：不截断。
            return rendered;
        };
        keep[*last_non_bundled] = false;
    }
}

/// XML 转义：`&` → `&amp;`，`<` → `&lt;`，`>` → `&gt;`，`"` → `&quot;`，`'` → `&apos;`。
///
/// 顺序固定（先 `&`，否则会二次转义）。TS: `skills.ts#escapeXml`。
///
/// 实现是逐 `char` 的单次映射，每个字符只被替换一次，因此不存在重复替换；语义上等价于
/// “先替换 `&`”的链式写法。`'` 也会转成 `&apos;`（XML 文本节点里并非必需，但保持
/// 全字符转义的一致性）。
pub fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_core::source_info::{SyntheticSourceInfoOptions, create_synthetic_source_info};

    fn skill(name: &str, description: &str, hidden: bool) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            file_path: format!("/path/{name}/SKILL.md"),
            base_dir: format!("/path/{name}"),
            source_info: create_synthetic_source_info(
                &format!("/path/{name}/SKILL.md"),
                SyntheticSourceInfoOptions {
                    source: "test".into(),
                    ..Default::default()
                },
            ),
            disable_model_invocation: hidden,
        }
    }

    #[test]
    fn empty_skill_list_formats_to_empty_string() {
        assert_eq!(format_skills_for_prompt(&[], SkillReader::Read), "");
    }

    #[test]
    fn formats_visible_skills_as_xml() {
        let result = format_skills_for_prompt(
            &[skill("test-skill", "A test skill.", false)],
            SkillReader::Read,
        );
        assert!(result.starts_with("\n\nThe following skills provide specialized instructions"));
        assert!(result.contains("<available_skills>"));
        assert!(result.contains("    <name>test-skill</name>"));
        assert!(result.contains("    <description>A test skill.</description>"));
        assert!(result.contains("    <location>/path/test-skill/SKILL.md</location>"));
        assert!(result.contains("  </skill>"));
    }

    #[test]
    fn escapes_xml_in_all_fields() {
        let result = format_skills_for_prompt(
            &[skill(
                "test-skill",
                "A skill with <special> & \"characters\".",
                false,
            )],
            SkillReader::Bash,
        );
        assert!(result.contains("&lt;special&gt;"));
        assert!(result.contains("&amp;"));
        assert!(result.contains("&quot;characters&quot;"));
        assert!(result.contains("Use bash to load a skill's file"));
    }

    #[test]
    fn excludes_hidden_skills() {
        let result = format_skills_for_prompt(
            &[
                skill("visible-skill", "A visible skill.", false),
                skill("hidden-skill", "A hidden skill.", true),
            ],
            SkillReader::Indirect,
        );
        assert!(result.contains("<name>visible-skill</name>"));
        assert!(!result.contains("hidden-skill"));
        assert_eq!(result.matches("<skill>").count(), 1);
    }

    #[test]
    fn all_hidden_means_empty_string() {
        assert_eq!(
            format_skills_for_prompt(&[skill("hidden", "x", true)], SkillReader::Read),
            ""
        );
    }
}
