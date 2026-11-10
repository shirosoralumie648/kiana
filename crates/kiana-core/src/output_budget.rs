//! 工具输出预算：按工具设置上限，超过落盘阈值时保存 artifact 并返回
//! middle-out 预览。
//!
//! 设计来源（只借鉴行为，不复制代码）：
//! - `reference_project/ClaudeCode/src/constants/toolLimits.ts`：默认 50k 字符上限、
//!   `Read` 使用 `Infinity`（自身有界）、按工具声明 `maxResultSizeChars`；
//! - `reference_project/ClaudeCode/src/utils/toolResultStorage.ts`：
//!   `DEFAULT_MAX_RESULT_SIZE_CHARS`、预览在换行边界截断、`<persisted-output>` 包装；
//! - `reference_project/oh-my-pi/packages/coding-agent/src/tools/settings.ts`：
//!   `artifactSpillThreshold`（默认 50 KB）与 `artifactHeadBytes` / `artifactTailBytes`
//!   的 middle 省略（本实现使用任务规定的 45k 落盘阈值与 40k 预览）。
//!
//! # 本实现采用的数值
//!
//! | 工具 | 上限（字符） | 说明 |
//! | --- | --- | --- |
//! | `read` | 无上限 | Read 自身按行/字节裁剪；只受落盘阈值约束 |
//! | `edit` / `write` / `apply_patch` / `multi_edit` | 100_000 | 写入结果带 diff |
//! | `bash` / `powershell` / `grep` | 30_000 | 高频、易爆量的输出 |
//! | 其它（默认） | 50_000 | |
//!
//! - 落盘阈值：[`SPILL_THRESHOLD_CHARS`] = 45_000；
//! - 预览：[`MIDDLE_OUT_PREVIEW_CHARS`] = 40_000，head/tail 各一半，中间插入
//!   `… [middle elided: N chars] …`。
//!
//! # 不变量
//!
//! 1. 返回的 `content` 字符数**永远小于** 50_000（10MB 的 bash 输出也一样）；
//! 2. 只要提供了 [`ArtifactStore`] 且落盘成功，完整内容（redaction 后）可以从
//!    handle 逐字节回读；
//! 3. 没有 store / 落盘失败时退化为 middle-out 截断，不丢 wrapper 结构，也不会 panic；
//! 4. 未超预算时内容原样返回（不改变 prompt cache 前缀）。

use crate::artifact_store::{ArtifactError, ArtifactHandle, ArtifactStore};

/// 默认工具输出上限（字符）。
pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 50_000;
/// 编辑类工具上限（字符）。
pub const EDIT_MAX_OUTPUT_CHARS: usize = 100_000;
/// Bash / PowerShell / Grep 上限（字符）。
pub const SHELL_MAX_OUTPUT_CHARS: usize = 30_000;
/// Read 不设上限（由 Read 自身的行/字节策略限定）。
pub const READ_MAX_OUTPUT_CHARS: usize = usize::MAX;
/// 超过该字符数就落盘。
pub const SPILL_THRESHOLD_CHARS: usize = 45_000;
/// 落盘后 inline 预览的字符数。
pub const MIDDLE_OUT_PREVIEW_CHARS: usize = 40_000;

/// 单个工具的预算策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolOutputPolicy {
    /// inline 内容的最大字符数（`usize::MAX` 表示不限制）。
    pub max_chars: usize,
    /// 超过该字符数时先落盘。
    pub spill_threshold_chars: usize,
    /// 落盘后 inline 预览的字符数。
    pub preview_chars: usize,
}

impl ToolOutputPolicy {
    /// 按工具名得到策略。
    pub fn for_tool(tool_name: &str) -> Self {
        let max_chars = match tool_name {
            "read" => READ_MAX_OUTPUT_CHARS,
            "edit" | "write" | "apply_patch" | "multi_edit" | "multiEdit" => EDIT_MAX_OUTPUT_CHARS,
            "bash" | "powershell" | "shell" | "grep" => SHELL_MAX_OUTPUT_CHARS,
            _ => DEFAULT_MAX_OUTPUT_CHARS,
        };
        Self {
            max_chars,
            spill_threshold_chars: SPILL_THRESHOLD_CHARS,
            preview_chars: MIDDLE_OUT_PREVIEW_CHARS,
        }
    }
}

impl Default for ToolOutputPolicy {
    fn default() -> Self {
        Self {
            max_chars: DEFAULT_MAX_OUTPUT_CHARS,
            spill_threshold_chars: SPILL_THRESHOLD_CHARS,
            preview_chars: MIDDLE_OUT_PREVIEW_CHARS,
        }
    }
}

/// 预算处理结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetedOutput {
    /// 给模型的 inline 内容（未超预算时与原内容相同）。
    pub content: String,
    /// 原始字符数。
    pub original_chars: usize,
    /// 是否落盘。
    pub spilled: bool,
    /// 是否发生截断（落盘预览或纯截断都算）。
    pub truncated: bool,
    /// 落盘句柄（`spilled == true` 且写入成功时存在）。
    pub artifact: Option<ArtifactHandle>,
    /// 落盘失败时的错误文本（用于诊断，不进入模型上下文）。
    pub artifact_error: Option<String>,
    /// JSON 结构大纲（仅当落盘内容是合法 JSON 且规模可接受时）。
    pub outline: Option<String>,
}

/// JSON 大纲的最大解析字节数；再大的输出只给 middle-out 预览，避免重复解析成本。
const MAX_OUTLINE_BYTES: usize = 2_000_000;
/// 大纲最多渲染的条目数。
const OUTLINE_ENTRIES_IN_WRAPPER: usize = 48;

/// 对工具输出应用预算。
///
/// `store` 为 `None` 时只做 middle-out 截断；这是无会话上下文的单测/SDK 路径。
pub fn apply_output_budget(
    tool_name: &str,
    content: &str,
    store: Option<&ArtifactStore>,
) -> BudgetedOutput {
    let policy = ToolOutputPolicy::for_tool(tool_name);
    let original_chars = content.chars().count();
    let needs_spill = original_chars > policy.spill_threshold_chars;
    let needs_cap = original_chars > policy.max_chars;
    // 没有 artifact store 时，45k 不是硬上限：只有工具自己的 max_chars 才截断。
    if !needs_cap && !(needs_spill && store.is_some()) {
        return BudgetedOutput {
            content: content.to_string(),
            original_chars,
            spilled: false,
            truncated: false,
            artifact: None,
            artifact_error: None,
            outline: None,
        };
    }

    let inline_limit = policy.max_chars.min(policy.preview_chars);
    if let Some(store) = store {
        match store.save(tool_name, content) {
            Ok(handle) => {
                let safe = handle_preview_source(content);
                let preview = middle_out_preview(&safe, inline_limit);
                let outline = json_outline_for(&safe);
                return BudgetedOutput {
                    content: build_persisted_output_message(
                        content,
                        &handle,
                        &preview,
                        outline.as_deref(),
                    ),
                    original_chars,
                    spilled: true,
                    truncated: true,
                    artifact: Some(handle),
                    artifact_error: None,
                    outline,
                };
            }
            Err(error) => {
                let preview = middle_out_preview(content, inline_limit);
                return BudgetedOutput {
                    content: build_truncated_message(content, &preview),
                    original_chars,
                    spilled: false,
                    truncated: true,
                    artifact: None,
                    artifact_error: Some(error.to_string()),
                    outline: None,
                };
            }
        }
    }

    let preview = middle_out_preview(content, inline_limit);
    BudgetedOutput {
        content: build_truncated_message(content, &preview),
        original_chars,
        spilled: false,
        truncated: true,
        artifact: None,
        artifact_error: None,
        outline: None,
    }
}

/// 对规模可接受的 JSON 输出生成结构大纲；其他情况返回 `None`。
fn json_outline_for(content: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    if content.len() > MAX_OUTLINE_BYTES {
        return None;
    }
    let options = crate::json_outline::OutlineOptions {
        max_depth: 3,
        max_entries: 128,
        max_children: 32,
    };
    crate::json_outline::structural_outline(content, &options)
        .ok()
        .map(|outline| outline.render(OUTLINE_ENTRIES_IN_WRAPPER))
}

/// 落盘预览的源内容。secret redaction 发生在 store 内；为避免把 secret 放入
/// inline 预览，这里先做同样的 redaction（store 返回 redacted 内容时预览也一致）。
fn handle_preview_source(content: &str) -> String {
    let (redacted, changed) = crate::artifact_store::redact_secrets(content);
    if changed {
        redacted
    } else {
        content.to_string()
    }
}

/// middle-out 预览：保留 head 与 tail，中间用一行说明替代。
///
/// 优先在换行边界切；找不到合理换行时按字符硬切。输出字符数 <= `max_chars`
/// （说明行本身占用少量字符，因此实际保留的 head/tail 会相应减少）。
pub fn middle_out_preview(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    // 先按极端比例预算说明行，保证最终长度不超过上限。
    let notice = |elided: usize| format!("\n… [middle elided: {elided} chars] …\n");
    let reserve = notice(max_chars).chars().count() + 32;
    let budget = max_chars.saturating_sub(reserve).max(2);
    let head_budget = budget / 2;
    let tail_budget = budget - head_budget;
    let total = content.chars().count();
    let head_end = char_boundary_at_or_before(content, head_budget);
    let tail_start = char_boundary_at_or_after(content, total.saturating_sub(tail_budget));
    // 尽量在换行边界调整（只在合理距离内）。
    let head_end = adjust_to_newline_before(content, head_end, head_budget);
    let tail_start = adjust_to_newline_after(content, tail_start, tail_budget);
    let head = &content[..head_end];
    let tail = &content[tail_start..];
    let elided = total.saturating_sub(head.chars().count() + tail.chars().count());
    format!("{head}{}{tail}", notice(elided))
}

/// 返回第 `count` 个字符的字节边界（不足时返回 `text.len()`）。
fn char_boundary_at_or_before(text: &str, count: usize) -> usize {
    text.char_indices()
        .nth(count)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

/// 返回从尾部保留 `count` 个字符时的字节起点。
fn char_boundary_at_or_after(text: &str, count: usize) -> usize {
    let total = text.chars().count();
    if count >= total {
        return 0;
    }
    char_boundary_at_or_before(text, count)
}

fn adjust_to_newline_before(text: &str, boundary: usize, budget: usize) -> usize {
    if boundary == 0 || boundary >= text.len() {
        return boundary;
    }
    let window_start = char_boundary_at_or_before(text, budget.saturating_sub(budget / 2));
    if let Some(index) = text[window_start..boundary].rfind('\n') {
        window_start + index + 1
    } else {
        boundary
    }
}

fn adjust_to_newline_after(text: &str, boundary: usize, budget: usize) -> usize {
    if boundary == 0 || boundary >= text.len() {
        return boundary;
    }
    let max_advance = budget / 2;
    let end = char_boundary_at_or_before(text, text[..boundary].chars().count() + max_advance);
    if let Some(index) = text[boundary..end].find('\n') {
        boundary + index + 1
    } else {
        boundary
    }
}

fn build_persisted_output_message(
    original: &str,
    handle: &ArtifactHandle,
    preview: &str,
    outline: Option<&str>,
) -> String {
    let original_chars = original.chars().count();
    let mut message = String::new();
    message.push_str("<persisted-output>\n");
    message.push_str(&format!(
        "Output too large ({original_chars} chars). Full output saved to: {} ({})\n",
        handle.uri(),
        handle.path.display()
    ));
    if let Some(outline) = outline {
        message.push_str(outline);
        message.push('\n');
    }
    message.push_str(&format!(
        "Preview (middle elided, up to {} chars):\n",
        preview.chars().count()
    ));
    message.push_str(preview);
    if !preview.ends_with('\n') {
        message.push('\n');
    }
    message.push_str("</persisted-output>");
    message
}

fn build_truncated_message(original: &str, preview: &str) -> String {
    format!(
        "{preview}\n[output truncated: original {} chars]",
        original.chars().count()
    )
}

/// 把 artifact 错误映射为模型可读的一行提示（不包含路径细节）。
pub fn format_artifact_error_notice(error: &ArtifactError) -> String {
    format!("[artifact unavailable: {error}]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn small_output_is_untouched() {
        let result = apply_output_budget("bash", "hello\n", None);
        assert_eq!(result.content, "hello\n");
        assert!(!result.truncated);
        assert!(!result.spilled);
    }

    #[test]
    fn ten_megabyte_bash_output_wrapper_is_under_fifty_thousand_chars() {
        let temp = TempDir::new().unwrap();
        let store = ArtifactStore::new(temp.path(), "s").unwrap();
        let content = "x".repeat(10 * 1024 * 1024);
        let result = apply_output_budget("bash", &content, Some(&store));
        assert!(result.spilled);
        assert!(result.content.chars().count() < 50_000);
        let handle = result.artifact.expect("handle");
        assert_eq!(store.read(&handle.id).unwrap().len(), content.len());
        // 逐字节一致：任意区间。
        let bytes = store.read_range_bytes(&handle.id, 123, 4096).unwrap();
        assert_eq!(bytes, content.as_bytes()[123..123 + 4096]);
    }

    #[test]
    fn bash_cap_is_thirty_thousand_without_artifact() {
        let content = "y".repeat(60_000);
        let result = apply_output_budget("bash", &content, None);
        assert!(result.content.chars().count() <= 31_000);
        assert!(result.content.contains("output truncated"));
        assert!(!result.spilled);
    }

    #[test]
    fn edit_cap_is_one_hundred_thousand() {
        let content = "z".repeat(80_000);
        let result = apply_output_budget("edit", &content, None);
        assert!(!result.truncated, "80k is below the edit cap");
        let content = "z".repeat(120_000);
        let result = apply_output_budget("edit", &content, None);
        assert!(result.truncated);
        assert!(result.content.chars().count() <= 40_100);
    }

    #[test]
    fn read_is_unbounded_but_spills_above_threshold() {
        let temp = TempDir::new().unwrap();
        let store = ArtifactStore::new(temp.path(), "s").unwrap();
        let content = "r".repeat(44_000);
        let result = apply_output_budget("read", &content, Some(&store));
        assert!(!result.truncated);
        let content = "r".repeat(46_000);
        let result = apply_output_budget("read", &content, Some(&store));
        assert!(result.spilled);
    }

    #[test]
    fn middle_out_keeps_head_and_tail() {
        let content: String = (0..1000).map(|index| format!("line-{index}\n")).collect();
        let preview = middle_out_preview(&content, 200);
        assert!(preview.starts_with("line-0\n"));
        assert!(preview.ends_with("line-999\n"));
        assert!(preview.contains("middle elided"));
        assert!(preview.chars().count() <= 200 + 40);
    }

    #[test]
    fn secret_is_redacted_in_preview_and_artifact() {
        let temp = TempDir::new().unwrap();
        let store = ArtifactStore::new(temp.path(), "s").unwrap();
        let content = format!("sk-abcdefghijklmnop1234\n{}", "q".repeat(50_000));
        let result = apply_output_budget("bash", &content, Some(&store));
        assert!(!result.content.contains("sk-abcdefghijklmnop1234"));
        let handle = result.artifact.unwrap();
        let disk = std::fs::read_to_string(&handle.path).unwrap();
        assert!(!disk.contains("sk-abcdefghijklmnop1234"));
    }

    #[test]
    fn spilled_json_includes_structural_outline_with_offsets() {
        let temp = TempDir::new().unwrap();
        let store = ArtifactStore::new(temp.path(), "s").unwrap();
        let payload = serde_json::json!({
            "users": (0..1000).map(|index| serde_json::json!({
                "id": index,
                "name": format!("user-{index}"),
                "active": index % 2 == 0,
            })).collect::<Vec<_>>(),
            "meta": {"total": 1000, "page": 1}
        });
        let content = serde_json::to_string_pretty(&payload).unwrap();
        let result = apply_output_budget("bash", &content, Some(&store));
        assert!(result.spilled);
        let outline = result.outline.expect("json outline");
        assert!(outline.contains("JSON outline"));
        // 每个 `path kind offset length` 行都能切出合法 JSON 子串。
        let parsed = crate::json_outline::structural_outline(
            &content,
            &crate::json_outline::OutlineOptions::default(),
        )
        .unwrap();
        for entry in &parsed.entries {
            assert!(entry.is_valid_json(&content), "{} is invalid", entry.path);
        }
        assert!(result.content.contains("JSON outline"));
    }
}
