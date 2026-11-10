//! split-turn 摘要模板与 prior summary 合并规则。
//!
//! 设计来源（只借鉴行为，不复制代码）：
//! - `reference_project/ClaudeCode/src/services/compact/prompt.ts`：结构化摘要模板与
//!   “增量更新旧摘要”的语义；
//! - `reference_project/oh-my-pi/packages/agent/src/compaction/` 的摘要细节；
//! - 任务规定的合并规则：未完成项 carry forward、新对话冲突优先、完成项移动。
//!
//! # 为什么单独一个模块
//!
//! `compaction.rs` 里现有的 `SUMMARIZATION_PROMPT` 是 TS 逐字移植的冻结契约（有字节级
//! 测试）。第 6 周的新模板不替换旧函数，而是供新的压缩流水线
//! （[`crate::pipeline`]）使用：
//!
//! - [`STRUCTURED_SUMMARY_INSTRUCTIONS`] 要求模型输出七个 section；
//! - [`StructuredSummary`] 可解析、可渲染、可合并；
//! - [`merge_prior_summaries`] 是确定性的（不调用 LLM），用于把旧摘要与新摘要合并。
//!
//! # section 语法
//!
//! ```text
//! ## Objective
//! ...
//! ## Details
//! - ...
//! ## Completed
//! - ...
//! ## Active
//! - ...
//! ## Blocked
//! - ...
//! ## Next
//! - ...
//! ## Files
//! - path (read|modified)
//! ```
//!
//! 解析对大小写、前后空白、`#`/`##` 数量都宽容；未知 section 归入 `Details`。

use std::collections::HashSet;

/// 新压缩流水线使用的摘要指令（拼在对话/旧摘要之后发给模型）。
pub const STRUCTURED_SUMMARY_INSTRUCTIONS: &str = "\
Produce a structured summary of the conversation above for continuing the task.
Use exactly these sections, in this order, and keep every list item on one line:

## Objective
One sentence describing what the user is trying to achieve.

## Details
Facts, decisions, constraints, and important context that are not captured elsewhere.

## Completed
Work that is finished and verified. Keep it short; do not re-explain it later.

## Active
Work in progress. Include the exact next concrete step if known.

## Blocked
Work that cannot proceed and why (missing input, failing tool, user decision).

## Next
Ordered next steps for the assistant.

## Files
Paths touched, one per line, with `(read)` or `(modified)`. Include only real paths.

Rules:
- Preserve exact identifiers, file paths, commands, and error messages.
- Do not invent facts. If a section has no content, write `- None`.
- Merge the previous summary (if provided) into this update: carry forward every
  unfinished item, let the new conversation win on conflicts, and move finished
  items into Completed.";

/// 摘要 section。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SummarySection {
    /// 目标。
    Objective,
    /// 细节。
    Details,
    /// 完成项。
    Completed,
    /// 进行中。
    Active,
    /// 阻塞项。
    Blocked,
    /// 下一步。
    Next,
    /// 文件。
    Files,
}

impl SummarySection {
    /// 标题文本。
    pub fn title(self) -> &'static str {
        match self {
            SummarySection::Objective => "Objective",
            SummarySection::Details => "Details",
            SummarySection::Completed => "Completed",
            SummarySection::Active => "Active",
            SummarySection::Blocked => "Blocked",
            SummarySection::Next => "Next",
            SummarySection::Files => "Files",
        }
    }

    /// 从标题解析（忽略大小写与空白）。
    pub fn parse(title: &str) -> Option<Self> {
        let normalized = title
            .trim()
            .trim_start_matches('#')
            .trim()
            .to_ascii_lowercase();
        match normalized.as_str() {
            "objective" | "goal" => Some(SummarySection::Objective),
            "details" | "context" => Some(SummarySection::Details),
            "completed" | "done" => Some(SummarySection::Completed),
            "active" | "in progress" | "in-progress" => Some(SummarySection::Active),
            "blocked" | "blockers" => Some(SummarySection::Blocked),
            "next" | "next steps" => Some(SummarySection::Next),
            "files" | "file changes" => Some(SummarySection::Files),
            _ => None,
        }
    }
}

/// 解析后的结构化摘要。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuredSummary {
    /// 目标（单行；多行时用空格连接）。
    pub objective: String,
    /// 细节条目。
    pub details: Vec<String>,
    /// 完成项。
    pub completed: Vec<String>,
    /// 进行中。
    pub active: Vec<String>,
    /// 阻塞项。
    pub blocked: Vec<String>,
    /// 下一步。
    pub next: Vec<String>,
    /// 文件（保持原始文本，例如 `src/lib.rs (read)`）。
    pub files: Vec<String>,
}

impl StructuredSummary {
    /// 从模型文本解析。没有可识别 section 时返回 `None`。
    pub fn parse(text: &str) -> Option<Self> {
        let mut summary = StructuredSummary::default();
        let mut current: Option<SummarySection> = None;
        let mut found_section = false;
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.starts_with('#') {
                let title = line.trim_start_matches('#').trim();
                if let Some(section) = SummarySection::parse(title) {
                    current = Some(section);
                    found_section = true;
                    continue;
                }
            }
            let Some(section) = current else {
                continue;
            };
            let item = line
                .strip_prefix("- ")
                .or_else(|| line.strip_prefix("* "))
                .unwrap_or(line)
                .trim();
            if item.is_empty() || item.eq_ignore_ascii_case("none") {
                continue;
            }
            match section {
                SummarySection::Objective => {
                    if summary.objective.is_empty() {
                        summary.objective = item.to_string();
                    } else {
                        summary.objective.push(' ');
                        summary.objective.push_str(item);
                    }
                }
                SummarySection::Details => summary.details.push(item.to_string()),
                SummarySection::Completed => summary.completed.push(item.to_string()),
                SummarySection::Active => summary.active.push(item.to_string()),
                SummarySection::Blocked => summary.blocked.push(item.to_string()),
                SummarySection::Next => summary.next.push(item.to_string()),
                SummarySection::Files => summary.files.push(item.to_string()),
            }
        }
        if found_section { Some(summary) } else { None }
    }

    /// 渲染成模板文本。
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("## Objective\n");
        if self.objective.trim().is_empty() {
            out.push_str("- None\n");
        } else {
            out.push_str(&format!("- {}\n", self.objective.trim()));
        }
        render_list(&mut out, SummarySection::Details, &self.details);
        render_list(&mut out, SummarySection::Completed, &self.completed);
        render_list(&mut out, SummarySection::Active, &self.active);
        render_list(&mut out, SummarySection::Blocked, &self.blocked);
        render_list(&mut out, SummarySection::Next, &self.next);
        render_list(&mut out, SummarySection::Files, &self.files);
        out
    }

    /// 所有“未完成”项（active + blocked）。
    pub fn unfinished(&self) -> Vec<&str> {
        self.active
            .iter()
            .chain(self.blocked.iter())
            .map(String::as_str)
            .collect()
    }
}

fn render_list(out: &mut String, section: SummarySection, items: &[String]) {
    out.push_str(&format!("## {}\n", section.title()));
    if items.is_empty() {
        out.push_str("- None\n");
        return;
    }
    for item in items {
        out.push_str("- ");
        out.push_str(item.trim());
        out.push('\n');
    }
}

/// 合并旧摘要与新摘要（确定性，无 LLM）。
///
/// 规则：
/// 1. **未完成 carry forward**：旧摘要里 active/blocked、且新摘要没有提到的条目，
///    原样保留；
/// 2. **新对话冲突优先**：同名条目在新摘要里处于 active/completed/blocked 时，
///    以新摘要的 section 为准；
/// 3. **完成项移动**：旧 active/blocked 在新摘要里变成 completed 时，从旧集合移除、
///    只出现在 Completed；
/// 4. Objective 取新值（为空时保留旧值）；Details 去重后旧在前、新在后；
///    Files 按路径去重合并。
pub fn merge_prior_summaries(previous: &str, new: &str) -> String {
    let parsed_previous = StructuredSummary::parse(previous);
    let parsed_new = StructuredSummary::parse(new);
    match (parsed_previous, parsed_new) {
        (Some(previous), Some(new)) => merge_structured(&previous, &new).render(),
        (Some(previous), None) => {
            // 新摘要没有按模板输出（模型忽略指令）：不要丢弃它，退化为文本追加。
            let mut merged = previous.render();
            merged.push_str("\n\n---\n\n");
            merged.push_str(new);
            merged
        }
        (None, Some(new)) => {
            if previous.trim().is_empty() {
                new.render()
            } else {
                format!("{}\n\n---\n\n{}", previous.trim(), new.render())
            }
        }
        (None, None) => {
            if previous.trim().is_empty() {
                new.to_string()
            } else {
                format!("{}\n\n---\n\n{}", previous.trim(), new)
            }
        }
    }
}

fn key(item: &str) -> String {
    item.trim().to_ascii_lowercase()
}

fn merge_structured(previous: &StructuredSummary, new: &StructuredSummary) -> StructuredSummary {
    let new_completed: HashSet<String> = new.completed.iter().map(|item| key(item)).collect();
    let new_active: HashSet<String> = new.active.iter().map(|item| key(item)).collect();
    let new_blocked: HashSet<String> = new.blocked.iter().map(|item| key(item)).collect();
    let new_mentioned: HashSet<String> = new_completed
        .iter()
        .chain(new_active.iter())
        .chain(new_blocked.iter())
        .cloned()
        .collect();

    let mut active: Vec<String> = new.active.clone();
    let mut blocked: Vec<String> = new.blocked.clone();
    let mut completed: Vec<String> = new.completed.clone();

    for item in &previous.active {
        if new_mentioned.contains(&key(item)) {
            continue;
        }
        if !active.iter().any(|existing| key(existing) == key(item)) {
            active.push(item.clone());
        }
    }
    for item in &previous.blocked {
        if new_mentioned.contains(&key(item)) {
            continue;
        }
        if !blocked.iter().any(|existing| key(existing) == key(item))
            && !active.iter().any(|existing| key(existing) == key(item))
        {
            blocked.push(item.clone());
        }
    }
    // 旧完成项：新对话若重新激活/阻塞，则移动；否则保留（去重）。
    for item in &previous.completed {
        let item_key = key(item);
        if new_active.contains(&item_key) || new_blocked.contains(&item_key) {
            continue;
        }
        if !completed.iter().any(|existing| key(existing) == item_key) {
            completed.push(item.clone());
        }
    }

    // 同一项不可能同时出现在 active/blocked/completed（新对话优先 + 清理旧项）。
    active.retain(|item| !completed.iter().any(|done| key(done) == key(item)));
    blocked.retain(|item| {
        !completed.iter().any(|done| key(done) == key(item))
            && !active.iter().any(|running| key(running) == key(item))
    });

    let mut details = previous.details.clone();
    for item in &new.details {
        if !details.iter().any(|existing| key(existing) == key(item)) {
            details.push(item.clone());
        }
    }

    let mut files = previous.files.clone();
    for item in &new.files {
        if !files.iter().any(|existing| key(existing) == key(item)) {
            files.push(item.clone());
        }
    }

    StructuredSummary {
        objective: if new.objective.trim().is_empty() {
            previous.objective.clone()
        } else {
            new.objective.clone()
        },
        details,
        completed,
        active,
        blocked,
        next: new.next.clone(),
        files,
    }
}

/// 为 LLM 构造带旧摘要的完整指令。
pub fn structured_summary_prompt(previous_summary: Option<&str>) -> String {
    match previous_summary.filter(|summary| !summary.trim().is_empty()) {
        Some(previous) => format!(
            "{STRUCTURED_SUMMARY_INSTRUCTIONS}\n\n<previous-summary>\n{previous}\n</previous-summary>"
        ),
        None => STRUCTURED_SUMMARY_INSTRUCTIONS.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEW: &str = "\
## Objective
Ship week 6
## Details
- Uses artifact store
## Completed
- Added pruning
## Active
- Wire recovery
## Blocked
- None
## Next
- Run tests
## Files
- src/lib.rs (modified)
";

    const OLD: &str = "\
## Objective
Old objective
## Details
- Uses artifact store
- Old detail
## Completed
- Old done
## Active
- Old active
- Wire recovery
## Blocked
- Waiting for API key
## Next
- Old next
## Files
- src/old.rs (read)
- src/lib.rs (modified)
";

    #[test]
    fn parse_and_render_roundtrip() {
        let parsed = StructuredSummary::parse(NEW).unwrap();
        assert_eq!(parsed.objective, "Ship week 6");
        assert_eq!(parsed.completed, vec!["Added pruning"]);
        assert_eq!(parsed.active, vec!["Wire recovery"]);
        assert_eq!(parsed.files, vec!["src/lib.rs (modified)"]);
        let roundtrip = StructuredSummary::parse(&parsed.render()).unwrap();
        assert_eq!(roundtrip, parsed);
    }

    #[test]
    fn merge_carries_forward_unfinished_and_moves_completed() {
        let merged = merge_prior_summaries(OLD, NEW);
        let parsed = StructuredSummary::parse(&merged).unwrap();
        // 新 objective 优先。
        assert_eq!(parsed.objective, "Ship week 6");
        // 旧 active 未在新对话出现 → carry forward；同名项不重复。
        assert!(parsed.active.contains(&"Old active".to_string()));
        assert_eq!(
            parsed
                .active
                .iter()
                .filter(|item| item.as_str() == "Wire recovery")
                .count(),
            1
        );
        // 旧 blocked 未完成 → carry forward。
        assert!(parsed.blocked.contains(&"Waiting for API key".to_string()));
        // 旧完成项保留。
        assert!(parsed.completed.contains(&"Old done".to_string()));
        assert!(parsed.completed.contains(&"Added pruning".to_string()));
        // files 合并去重。
        assert!(parsed.files.contains(&"src/old.rs (read)".to_string()));
        assert_eq!(
            parsed
                .files
                .iter()
                .filter(|item| item.as_str() == "src/lib.rs (modified)")
                .count(),
            1
        );
        // details 去重。
        assert_eq!(
            parsed
                .details
                .iter()
                .filter(|item| item.as_str() == "Uses artifact store")
                .count(),
            1
        );
    }

    #[test]
    fn new_conversation_wins_on_conflicts() {
        let previous = "\
## Objective
x
## Active
- Fix bug A
## Blocked
- Fix bug B
## Completed
- Old item
";
        let new = "\
## Objective
y
## Completed
- Fix bug A
## Active
- Old item
";
        let parsed = StructuredSummary::parse(&merge_prior_summaries(previous, new)).unwrap();
        assert!(parsed.completed.contains(&"Fix bug A".to_string()));
        assert!(!parsed.active.contains(&"Fix bug A".to_string()));
        // 旧完成项被新对话重新激活 → 移动到 active，且不再出现在 completed。
        assert!(parsed.active.contains(&"Old item".to_string()));
        assert!(!parsed.completed.contains(&"Old item".to_string()));
        // 旧 blocked 未提及 → carry forward。
        assert!(parsed.blocked.contains(&"Fix bug B".to_string()));
    }

    #[test]
    fn prompt_contains_all_sections_and_previous_summary() {
        let prompt = structured_summary_prompt(Some("old summary"));
        for title in [
            "Objective",
            "Details",
            "Completed",
            "Active",
            "Blocked",
            "Next",
            "Files",
        ] {
            assert!(prompt.contains(&format!("## {title}")));
        }
        assert!(prompt.contains("<previous-summary>\nold summary\n</previous-summary>"));
    }

    #[test]
    fn unstructured_new_summary_is_not_dropped() {
        let merged = merge_prior_summaries(OLD, "plain prose summary without headings");
        assert!(merged.contains("Old objective"));
        assert!(merged.contains("plain prose summary without headings"));
        let merged = merge_prior_summaries("plain previous", "plain new");
        assert!(merged.contains("plain previous"));
        assert!(merged.contains("plain new"));
    }
}
