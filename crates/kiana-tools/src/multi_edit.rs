//! 多 edit 的原子校验/应用引擎（设计借鉴 ClaudeCode FileEditTool 的
//! read→validate→patch→write 顺序、gemini-cli edit 的分级匹配与 `EditStrategyEvent`，
//! 以及 aider editblock 的相似度反馈；实现按 Kiana 的类型重写）。
//!
//! # 解决的问题
//!
//! `edit` 工具一次可以收到多个 `{ oldText, newText }`。旧实现（`edit_diff.rs`）在
//! 第一个坏 edit 处直接返回一句模糊错误：模型不知道是哪个 edit、错在文件哪一行、
//! 最近似的文本是什么，也不知道文件是否被写了一半。本模块把流程拆成两个阶段：
//!
//! 1. **全部校验**：每个 edit 独立定位，记录策略与真实行号；任何失败都不会产生
//!    [`FileEditPlan`]，调用方因此不会写盘（「该文件零落盘」）；
//! 2. **一次性应用**：所有 edit 都有效后，检查重叠、逆序替换生成 `new_content`，
//!    并给出每个 edit 的 hunk 与 ±4 行片段。
//!
//! # 匹配策略（按优先级，逐 edit 独立决定）
//!
//! 1. [`EditStrategy::Exact`]：在 LF 归一化内容上精确子串匹配；
//! 2. [`EditStrategy::QuoteNormalized`]：仅当 exact 失败，把弯引号归一化为 ASCII 后
//!    再匹配（保留原文偏移映射，替换仍落在原文字节上）；
//! 3. [`EditStrategy::Fuzzy`]：仅当前两者都失败，按行窗口做归一化（NFKC、行尾空白、
//!    智能引号/破折号/特殊空格）+ 加权编辑距离匹配；要求 `oldText` 至少 10 个字符，
//!    归一化后得分 ≤ 0.1，且只接受**唯一**的非重叠候选。
//!
//! 永远不会「整基底无声切换」：每个 edit 的匹配基底与策略都记录在
//! [`AppliedEdit`] 中并进入工具 details 的 `strategy`/`editStrategies`/
//! `strategyEvents`/`strategyVariant`。
//! [`EditStrategy::Regex`] 预留给未来的显式正则编辑，当前不会自动选用。
//!
//! # 错误契约（模型可见文本）
//!
//! 每个错误都包含：路径、`edits[i]` 下标、真实行号、是否已经应用、以及
//! **不要重发同一个失败编辑**。`NotFound` 还带最多 3 个最近行区间与相似度，
//! `NotUnique` 列出全部出现行号。多 edit 失败时附上逐 edit 状态（哪些 matched、
//! 哪些 failed、哪些 skipped），帮助模型只修正坏的那一条。
//!
//! # 失败模式
//!
//! - `oldText` 为空（或模糊归一化后为空）→ [`EditError::EmptyReplacement`]；
//! - `oldText` 找不到但 `newText` 已在文件中 → [`EditError::AlreadyApplied`]；
//! - 出现多次 → [`EditError::NotUnique`]（不写）；
//! - 两个 edit 的匹配区间重叠 → [`EditError::Overlap`]（不写）；
//! - 文件在 read 之后被外部修改由 `edit.rs` 映射成 [`EditError::FileChanged`]。
//!
//! 以上错误都不会让本模块返回 `new_content`，因此「任一失败该文件零落盘」是类型层面的
//! 保证：只有 [`FileEditOutcome::Applied`] 里才有可写内容。

use serde::{Deserialize, Serialize};

use kiana_ext_api::tool_types::ReplaceEdit;

use crate::edit_diff::{
    generate_diff_string, generate_unified_patch, normalize_for_fuzzy_match, normalize_to_lf,
    split_lines_with_endings,
};

/// 1-based 闭区间行号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineRange {
    /// 起始行（1 起）。
    pub start: u64,
    /// 结束行（1 起，包含）。
    pub end: u64,
}

impl LineRange {
    /// 构造行区间。
    pub const fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }

    /// 区间行数。
    // 行区间永远至少包含一行（`start == end` 时为一行），不存在空区间。
    #[allow(clippy::len_without_is_empty)]
    pub const fn len(self) -> u64 {
        self.end.saturating_sub(self.start) + 1
    }

    /// 是否覆盖某一行。
    pub const fn contains(self, line: u64) -> bool {
        line >= self.start && line <= self.end
    }
}

impl std::fmt::Display for LineRange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.start == self.end {
            write!(formatter, "line {}", self.start)
        } else {
            write!(formatter, "lines {}-{}", self.start, self.end)
        }
    }
}

/// 一次匹配使用的策略。
///
/// 序列化值为 `exact` / `quoteNormalized` / `fuzzy` / `regex`；顺序也是「强度」顺序，
/// 多个 edit 时取最大值作为整体 `strategy`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EditStrategy {
    /// 精确匹配。
    Exact,
    /// 弯引号归一化后匹配。
    QuoteNormalized,
    /// 模糊匹配（NFKC + 行尾空白 + 标点归一化 + 编辑距离）。
    Fuzzy,
    /// 显式正则匹配（保留值；当前不会自动选用）。
    Regex,
}

impl EditStrategy {
    /// 线格式字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            EditStrategy::Exact => "exact",
            EditStrategy::QuoteNormalized => "quoteNormalized",
            EditStrategy::Fuzzy => "fuzzy",
            EditStrategy::Regex => "regex",
        }
    }

    /// Rust 变体名（`Exact` / `Fuzzy` / `QuoteNormalized` / `Regex`）。
    ///
    /// 工具 metadata 同时带线格式 `strategy` 与这个变体名，二者满足不同消费方的命名习惯。
    pub const fn variant_name(self) -> &'static str {
        match self {
            EditStrategy::Exact => "Exact",
            EditStrategy::QuoteNormalized => "QuoteNormalized",
            EditStrategy::Fuzzy => "Fuzzy",
            EditStrategy::Regex => "Regex",
        }
    }
}

impl std::fmt::Display for EditStrategy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 匹配策略事件（写入工具 metadata/telemetry）。
///
/// 每个成功定位的 edit 产生一条；`strategy` 只会是 [`EditStrategy`] 的线格式值。
/// 失败时不会产生事件（失败原因在 [`EditError`] 里）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditStrategyEvent {
    /// 输入数组下标。
    pub edit_index: usize,
    /// 采用的策略。
    pub strategy: EditStrategy,
    /// 真实行区间。
    pub line_range: LineRange,
}

impl EditStrategyEvent {
    /// 构造事件。
    pub const fn new(edit_index: usize, strategy: EditStrategy, line_range: LineRange) -> Self {
        Self {
            edit_index,
            strategy,
            line_range,
        }
    }
}

/// 最近似片段：`(行区间, 片段文本, 相似度 0..=1)`。
pub type NearestMatch = (LineRange, String, f64);

/// `edit` 的结构化错误。
#[derive(Debug, Clone, PartialEq)]
pub enum EditError {
    /// `oldText` 在所有策略下都找不到。
    NotFound {
        /// 文件路径（调用方传入的原始 spelling）。
        path: String,
        /// 该 edit 在输入数组里的下标。
        edit_index: usize,
        /// 本次调用的 edit 总数。
        total_edits: usize,
        /// 最多 3 个最近行区间。
        nearest: Vec<NearestMatch>,
    },
    /// `oldText` 出现多次，无法确定替换哪一处。
    NotUnique {
        /// 文件路径。
        path: String,
        /// edit 下标。
        edit_index: usize,
        /// edit 总数。
        total_edits: usize,
        /// 出现次数。
        occurrences: usize,
        /// 每一处的行区间。
        line_ranges: Vec<LineRange>,
    },
    /// `newText` 已在文件中（编辑已经应用过）。
    AlreadyApplied {
        /// 文件路径。
        path: String,
        /// edit 下标。
        edit_index: usize,
        /// edit 总数。
        total_edits: usize,
        /// `newText` 所在行区间（如果定位得到）。
        line_range: Option<LineRange>,
    },
    /// 两个 edit 的匹配区间重叠。
    Overlap {
        /// 文件路径。
        path: String,
        /// 后一个 edit 的下标（按原始顺序）。
        edit_index: usize,
        /// 与之冲突的前一个 edit 下标。
        conflicts_with: usize,
        /// 重叠处所在行。
        line_range: LineRange,
    },
    /// `oldText` 为空（或模糊归一化后为空），没有可替换的内容。
    EmptyReplacement {
        /// 文件路径。
        path: String,
        /// edit 下标。
        edit_index: usize,
        /// edit 总数。
        total_edits: usize,
    },
    /// 文件在读取之后发生变化（由 `edit.rs` 的读后写门禁映射）。
    FileChanged {
        /// 文件路径。
        path: String,
        /// 变化原因（读记录/内容哈希）。
        reason: String,
    },
}

impl EditError {
    /// 稳定的错误码（写入 telemetry/日志）。
    pub const fn code(&self) -> &'static str {
        match self {
            EditError::NotFound { .. } => "not_found",
            EditError::NotUnique { .. } => "not_unique",
            EditError::AlreadyApplied { .. } => "already_applied",
            EditError::Overlap { .. } => "overlap",
            EditError::EmptyReplacement { .. } => "empty_replacement",
            EditError::FileChanged { .. } => "file_changed",
        }
    }

    /// 错误涉及的路径。
    pub fn path(&self) -> &str {
        match self {
            EditError::NotFound { path, .. }
            | EditError::NotUnique { path, .. }
            | EditError::AlreadyApplied { path, .. }
            | EditError::Overlap { path, .. }
            | EditError::EmptyReplacement { path, .. }
            | EditError::FileChanged { path, .. } => path,
        }
    }

    /// 错误涉及的 edit 下标（`FileChanged` 没有）。
    pub const fn edit_index(&self) -> Option<usize> {
        match self {
            EditError::NotFound { edit_index, .. }
            | EditError::NotUnique { edit_index, .. }
            | EditError::AlreadyApplied { edit_index, .. }
            | EditError::EmptyReplacement { edit_index, .. }
            | EditError::Overlap { edit_index, .. } => Some(*edit_index),
            EditError::FileChanged { .. } => None,
        }
    }
}

/// `NotUnique` 消息里最多列出的行区间数（`occurrences` 仍是完整计数）。
const MAX_LISTED_NOT_UNIQUE_RANGES: usize = 20;

impl std::fmt::Display for EditError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::NotFound {
                path,
                edit_index,
                total_edits,
                nearest,
            } => {
                write!(
                    formatter,
                    "{} failed for {path}: oldText was not found in the current file and newText \
                     is not present either.",
                    edit_label(*edit_index, *total_edits)
                )?;
                if nearest.is_empty() {
                    formatter.write_str(" The file is empty or has no lines to compare.")?;
                } else {
                    write!(
                        formatter,
                        "\nNearest matches (1-based line ranges, similarity 0..1):"
                    )?;
                    for (rank, (range, snippet, similarity)) in nearest.iter().enumerate() {
                        write!(
                            formatter,
                            "\n  {}. {range} (similarity {similarity:.3}):",
                            rank + 1
                        )?;
                        for line in snippet.lines() {
                            write!(formatter, "\n     | {line}")?;
                        }
                    }
                }
                formatter.write_str(
                    "\nDo not resend the same failed edit; re-read the file and adjust oldText \
                     (or use apply_patch).",
                )
            }
            EditError::NotUnique {
                path,
                edit_index,
                total_edits,
                occurrences,
                line_ranges,
            } => {
                let ranges = line_ranges
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                let more = occurrences.saturating_sub(line_ranges.len());
                let more = if more > 0 {
                    format!(
                        " (first {} of {occurrences}; {} more not listed)",
                        line_ranges.len(),
                        more
                    )
                } else {
                    String::new()
                };
                write!(
                    formatter,
                    "{} failed for {path}: oldText matches {occurrences} times at {ranges}{more}. \
                     Each edits[].oldText must be unique; add surrounding context so it matches \
                     exactly once. replace_all is false, so nothing was written. Do not resend \
                     the same failed edit.",
                    edit_label(*edit_index, *total_edits)
                )
            }
            EditError::AlreadyApplied {
                path,
                edit_index,
                total_edits,
                line_range,
            } => {
                let where_text = match line_range {
                    Some(range) => format!(" ({range})"),
                    None => String::new(),
                };
                write!(
                    formatter,
                    "{} for {path} is already applied: newText already exists{where_text} and \
                     oldText is gone. The file was not changed. Do not resend the same failed \
                     edit; verify the current file state first.",
                    edit_label(*edit_index, *total_edits)
                )
            }
            EditError::Overlap {
                path,
                edit_index,
                conflicts_with,
                line_range,
            } => write!(
                formatter,
                "{} overlaps edits[{conflicts_with}] in {path} at {line_range}. Merge them into \
                 one edit or target disjoint regions. The file was not changed. Do not resend \
                 the same failed edit.",
                edit_label(*edit_index, usize::MAX)
            ),
            EditError::EmptyReplacement {
                path,
                edit_index,
                total_edits,
            } => write!(
                formatter,
                "{} failed for {path}: oldText must not be empty (or only whitespace after \
                 normalization). Provide the exact text to replace. Do not resend the same \
                 failed edit.",
                edit_label(*edit_index, *total_edits)
            ),
            EditError::FileChanged { path, reason } => write!(
                formatter,
                "FileChangedSinceRead (FILE_CHANGED_SINCE_READ): {path} changed on disk since it \
                 was read ({reason}). No edits were written. Re-read the file with the read tool \
                 before modifying it; do not resend the same failed edit."
            ),
        }
    }
}

impl std::error::Error for EditError {}

/// `Edit 0 of 1` / `Edit 2 of 5`。
fn edit_label(edit_index: usize, total_edits: usize) -> String {
    if total_edits == 0 || total_edits == usize::MAX {
        format!("Edit[{edit_index}]")
    } else {
        format!("Edit {edit_index} of {total_edits}")
    }
}

/// 单个 edit 的校验状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditStatus {
    /// 已定位、可以应用（整体失败时表示“本可应用但未写”）。
    Matched {
        /// 匹配策略。
        strategy: EditStrategy,
        /// 真实行区间。
        line_range: LineRange,
    },
    /// 校验失败。
    Failed {
        /// [`EditError::code`]。
        code: &'static str,
    },
    /// 因为更早的错误没有检查（当前实现总是全部检查，保留以便扩展）。
    Skipped,
}

impl EditStatus {
    /// 状态短名。
    pub const fn as_str(&self) -> &'static str {
        match self {
            EditStatus::Matched { .. } => "matched",
            EditStatus::Failed { .. } => "failed",
            EditStatus::Skipped => "skipped",
        }
    }
}

/// 一个成功应用的 edit 及其可验证产物。
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedEdit {
    /// 输入数组下标。
    pub edit_index: usize,
    /// 匹配策略。
    pub strategy: EditStrategy,
    /// 原文中的行区间。
    pub line_range: LineRange,
    /// 该 edit 的 unified hunk（含上下文）。
    pub hunk: String,
    /// 应用后文件在新 edit 附近的 ±4 行片段（行号 + `>` 标记变更行）。
    pub snippet: String,
}

/// 全部 edit 成功后得到的计划；`new_content` 是 LF、无 BOM 文本。
#[derive(Debug, Clone, PartialEq)]
pub struct FileEditPlan {
    /// 文件路径（调用方原始 spelling）。
    pub path: String,
    /// LF 归一化后的原文。
    pub base_content: String,
    /// 应用后的内容。
    pub new_content: String,
    /// 每个 edit 的策略/行号/hunk/片段，按输入顺序。
    pub edits: Vec<AppliedEdit>,
    /// 展示 diff。
    pub diff: String,
    /// 新文件第一处变更行（1 起）。
    pub first_changed_line: Option<u64>,
    /// 整文件 unified patch。
    pub patch: String,
    /// 整体策略（各 edit 的最大值）。
    pub strategy: EditStrategy,
}

/// 校验失败时的完整报告。
#[derive(Debug, Clone, PartialEq)]
pub struct FileEditRejection {
    /// 第一条错误（决定工具错误文案）。
    pub error: EditError,
    /// 与输入等长的逐 edit 状态。
    pub statuses: Vec<EditStatus>,
}

impl FileEditRejection {
    /// 错误文本 + 多 edit 时的逐 edit 状态附录。
    pub fn message(&self) -> String {
        let mut message = self.error.to_string();
        if self.statuses.len() > 1 {
            message.push_str("\nPer-edit status (the file was not written):");
            for (index, status) in self.statuses.iter().enumerate() {
                match status {
                    EditStatus::Matched {
                        strategy,
                        line_range,
                    } => message.push_str(&format!(
                        "\n  edits[{index}]: matched (strategy={strategy}, {line_range}) - not written"
                    )),
                    EditStatus::Failed { code } => message.push_str(&format!(
                        "\n  edits[{index}]: failed ({code}) - not written"
                    )),
                    EditStatus::Skipped => message.push_str(&format!(
                        "\n  edits[{index}]: skipped - not written"
                    )),
                }
            }
        }
        message
    }
}

/// 校验 + 应用结果。
#[derive(Debug, Clone, PartialEq)]
pub enum FileEditOutcome {
    /// 所有 edit 均有效且已生成新内容（调用方负责写盘）。
    Applied(FileEditPlan),
    /// 任一 edit 失败/重叠/已是目标状态；没有可写内容。
    Rejected(FileEditRejection),
}

/// 对单个文件规划多 edit。
///
/// `content` 可以是任意行尾；内部先做 LF 归一化。`path` 只用于错误消息与 patch 头。
/// 返回 [`FileEditOutcome::Applied`] 时 `new_content` 一定与原文不同且所有 edit 都通过。
pub fn plan_file_edits(path: &str, content: &str, edits: &[ReplaceEdit]) -> FileEditOutcome {
    let base = normalize_to_lf(content);
    let total = edits.len();
    let mut matches: Vec<Option<CandidateMatch>> = vec![None; total];
    let mut statuses: Vec<EditStatus> = vec![EditStatus::Skipped; total];
    let mut first_error: Option<EditError> = None;

    for (index, edit) in edits.iter().enumerate() {
        let old_text = normalize_to_lf(&edit.old_text);
        let new_text = normalize_to_lf(&edit.new_text);

        if old_text.is_empty() {
            let error = EditError::EmptyReplacement {
                path: path.to_string(),
                edit_index: index,
                total_edits: total,
            };
            statuses[index] = EditStatus::Failed { code: error.code() };
            first_error.get_or_insert(error);
            continue;
        }

        match find_matches(&base, &old_text) {
            MatchSearch::Found(candidates) if candidates.len() == 1 => {
                let candidate = candidates.into_iter().next().expect("len checked");
                statuses[index] = EditStatus::Matched {
                    strategy: candidate.strategy,
                    line_range: candidate.line_range,
                };
                matches[index] = Some(candidate);
            }
            MatchSearch::Found(candidates) => {
                let error = EditError::NotUnique {
                    path: path.to_string(),
                    edit_index: index,
                    total_edits: total,
                    occurrences: candidates.len(),
                    // 只保留前若干处：1MB 文件里的短 oldText 可能有几十万处匹配，
                    // 全部存进错误消息会撑爆内存与模型上下文。
                    line_ranges: candidates
                        .iter()
                        .take(MAX_LISTED_NOT_UNIQUE_RANGES)
                        .map(|candidate| candidate.line_range)
                        .collect(),
                };
                statuses[index] = EditStatus::Failed { code: error.code() };
                first_error.get_or_insert(error);
            }
            MatchSearch::None => {
                if let Some(line_range) = already_applied_range(&base, &new_text) {
                    let error = EditError::AlreadyApplied {
                        path: path.to_string(),
                        edit_index: index,
                        total_edits: total,
                        line_range: Some(line_range),
                    };
                    statuses[index] = EditStatus::Failed { code: error.code() };
                    first_error.get_or_insert(error);
                } else if normalize_for_fuzzy_match(&old_text).is_empty() {
                    let error = EditError::EmptyReplacement {
                        path: path.to_string(),
                        edit_index: index,
                        total_edits: total,
                    };
                    statuses[index] = EditStatus::Failed { code: error.code() };
                    first_error.get_or_insert(error);
                } else {
                    let error = EditError::NotFound {
                        path: path.to_string(),
                        edit_index: index,
                        total_edits: total,
                        nearest: nearest_matches(&base, &old_text, 3),
                    };
                    statuses[index] = EditStatus::Failed { code: error.code() };
                    first_error.get_or_insert(error);
                }
            }
        }
    }

    if let Some(error) = first_error {
        return FileEditOutcome::Rejected(FileEditRejection { error, statuses });
    }

    // 全部定位成功后再检查重叠。按 edit 原始顺序两两比较，错误里保留原始下标。
    for (earlier_index, earlier) in matches.iter().enumerate() {
        let Some(earlier) = earlier else { continue };
        for (later_index, later_match) in matches.iter().enumerate().skip(earlier_index + 1) {
            let Some(later_match) = later_match else {
                continue;
            };
            if earlier.start < later_match.end && later_match.start < earlier.end {
                let overlap_line = earlier.line_range.start.max(later_match.line_range.start);
                let error = EditError::Overlap {
                    path: path.to_string(),
                    edit_index: later_index,
                    conflicts_with: earlier_index,
                    line_range: LineRange::new(overlap_line, overlap_line),
                };
                statuses[later_index] = EditStatus::Failed { code: error.code() };
                return FileEditOutcome::Rejected(FileEditRejection { error, statuses });
            }
        }
    }

    // 构造替换。fuzzy 命中需要把新文本对齐到原文缩进并保留行尾换行。
    let mut replacements: Vec<Replacement> = Vec::with_capacity(total);
    for (index, matched) in matches.iter().enumerate() {
        let Some(matched) = matched else { continue };
        let edit = &edits[index];
        let mut new_text = normalize_to_lf(&edit.new_text);
        if &base[matched.start..matched.end] == new_text.as_str() {
            let error = EditError::AlreadyApplied {
                path: path.to_string(),
                edit_index: index,
                total_edits: total,
                line_range: Some(matched.line_range),
            };
            statuses[index] = EditStatus::Failed { code: error.code() };
            return FileEditOutcome::Rejected(FileEditRejection { error, statuses });
        }
        if matched.strategy == EditStrategy::Fuzzy {
            new_text = align_fuzzy_replacement(&base, matched, &new_text);
        }
        replacements.push(Replacement {
            edit_index: index,
            strategy: matched.strategy,
            line_range: matched.line_range,
            start: matched.start,
            end: matched.end,
            new_text,
        });
    }

    replacements.sort_by_key(|replacement| replacement.start);
    let mut new_content = base.clone();
    for replacement in replacements.iter().rev() {
        new_content.replace_range(replacement.start..replacement.end, &replacement.new_text);
    }
    if new_content == base {
        let error = EditError::AlreadyApplied {
            path: path.to_string(),
            edit_index: 0,
            total_edits: total,
            line_range: matches
                .first()
                .and_then(|matched| matched.as_ref().map(|candidate| candidate.line_range)),
        };
        return FileEditOutcome::Rejected(FileEditRejection { error, statuses });
    }

    let mut applied: Vec<AppliedEdit> = Vec::with_capacity(replacements.len());
    let mut delta: i64 = 0;
    for replacement in &replacements {
        let new_start = (replacement.start as i64 + delta) as usize;
        let new_start_line = line_number_at(&new_content, new_start);
        let hunk = build_hunk(&base, &new_content, replacement, new_start_line);
        let snippet = build_snippet(&new_content, new_start_line, &replacement.new_text);
        applied.push(AppliedEdit {
            edit_index: replacement.edit_index,
            strategy: replacement.strategy,
            line_range: replacement.line_range,
            hunk,
            snippet,
        });
        delta += replacement.new_text.len() as i64 - (replacement.end - replacement.start) as i64;
    }
    applied.sort_by_key(|edit| edit.edit_index);

    let strategy = replacements
        .iter()
        .map(|replacement| replacement.strategy)
        .max()
        .unwrap_or(EditStrategy::Exact);
    let diff = generate_diff_string(&base, &new_content);
    let patch = generate_unified_patch(path, &base, &new_content);

    FileEditOutcome::Applied(FileEditPlan {
        path: path.to_string(),
        base_content: base,
        new_content,
        edits: applied,
        diff: diff.diff,
        first_changed_line: diff.first_changed_line,
        patch,
        strategy,
    })
}

#[derive(Clone)]
struct CandidateMatch {
    strategy: EditStrategy,
    start: usize,
    end: usize,
    line_range: LineRange,
}

struct Replacement {
    edit_index: usize,
    strategy: EditStrategy,
    line_range: LineRange,
    start: usize,
    end: usize,
    new_text: String,
}

enum MatchSearch {
    Found(Vec<CandidateMatch>),
    None,
}

fn find_matches(content: &str, old_text: &str) -> MatchSearch {
    let exact = exact_matches(content, old_text);
    if !exact.is_empty() {
        return MatchSearch::Found(
            exact
                .into_iter()
                .map(|(start, end)| CandidateMatch {
                    strategy: EditStrategy::Exact,
                    start,
                    end,
                    line_range: line_range_of_span(content, start, end),
                })
                .collect(),
        );
    }

    let quoted = quote_normalized_matches(content, old_text);
    if !quoted.is_empty() {
        return MatchSearch::Found(
            quoted
                .into_iter()
                .map(|(start, end)| CandidateMatch {
                    strategy: EditStrategy::QuoteNormalized,
                    start,
                    end,
                    line_range: line_range_of_span(content, start, end),
                })
                .collect(),
        );
    }

    let fuzzy = fuzzy_matches(content, old_text);
    if !fuzzy.is_empty() {
        return MatchSearch::Found(fuzzy);
    }
    MatchSearch::None
}

fn exact_matches(content: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    overlapping_starts(content, needle)
        .into_iter()
        .map(|start| (start, start + needle.len()))
        .collect()
}

/// 所有（含重叠）出现的起始字节。
///
/// `str::match_indices` 只报告不重叠的匹配：`"ababab"` 里的 `"abab"` 只会出现一次，
/// 从而把歧义匹配误判成唯一匹配。这里逐字符边界推进，宁可保守地多报。
fn overlapping_starts(haystack: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut starts = Vec::new();
    let mut from = 0usize;
    while from <= haystack.len() {
        let Some(offset) = haystack[from..].find(needle) else {
            break;
        };
        let start = from + offset;
        starts.push(start);
        from = start + haystack[start..].chars().next().map_or(1, char::len_utf8);
    }
    starts
}

/// 弯引号 → ASCII，并保留每个归一化字符对应的原文区间。
struct QuoteNormalized {
    text: String,
    /// 每个输出字符对应的原文 `[start, end)`。
    origins: Vec<(usize, usize)>,
    /// 每个输出字符的起始字节 + 末尾哨兵。
    char_starts: Vec<usize>,
}

fn quote_normalize(input: &str) -> QuoteNormalized {
    let mut text = String::with_capacity(input.len());
    let mut origins = Vec::with_capacity(input.len());
    let mut char_starts = Vec::with_capacity(input.len() + 1);
    for (start, character) in input.char_indices() {
        let end = start + character.len_utf8();
        char_starts.push(text.len());
        let mapped = match character {
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            other => other,
        };
        text.push(mapped);
        origins.push((start, end));
    }
    char_starts.push(text.len());
    QuoteNormalized {
        text,
        origins,
        char_starts,
    }
}

impl QuoteNormalized {
    fn map_range(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        let first = self.char_starts.partition_point(|offset| *offset < start);
        if self.char_starts.get(first).copied() != Some(start) {
            return None;
        }
        let last = self.char_starts.partition_point(|offset| *offset < end);
        if self.char_starts.get(last).copied() != Some(end) || last <= first {
            return None;
        }
        let original_start = self.origins.get(first)?.0;
        let original_end = self.origins.get(last - 1)?.1;
        Some((original_start, original_end))
    }
}

fn quote_normalized_matches(content: &str, needle: &str) -> Vec<(usize, usize)> {
    let normalized = quote_normalize(content);
    let normalized_needle = quote_normalize(needle);
    if normalized_needle.text.is_empty() {
        return Vec::new();
    }
    overlapping_starts(&normalized.text, &normalized_needle.text)
        .into_iter()
        .filter_map(|start| normalized.map_range(start, start + normalized_needle.text.len()))
        .collect()
}

/// 模糊匹配的阈值（gemini-cli 的 `FUZZY_MATCH_THRESHOLD`）。
const FUZZY_MATCH_THRESHOLD: f64 = 0.1;
/// 空白差异的惩罚系数（gemini-cli 的 `WHITESPACE_PENALTY_FACTOR`）。
const FUZZY_WHITESPACE_PENALTY: f64 = 0.1;
/// 太短的 oldText 不做模糊匹配，避免误命中。
const FUZZY_MIN_CHARS: usize = 10;
/// 模糊搜索的复杂度上限（行数 × 文本长度²）。
const FUZZY_WORK_LIMIT: usize = 400_000_000;

fn fuzzy_matches(content: &str, old_text: &str) -> Vec<CandidateMatch> {
    if old_text.chars().count() < FUZZY_MIN_CHARS {
        return Vec::new();
    }
    let lines = content_lines(content);
    let mut search_lines: Vec<String> = old_text.split('\n').map(ToString::to_string).collect();
    if old_text.ends_with('\n') {
        search_lines.pop();
    }
    let window_size = search_lines.len();
    if window_size == 0 || window_size > lines.len() {
        return Vec::new();
    }
    let normalized_search: Vec<String> = search_lines
        .iter()
        .map(|line| normalize_for_fuzzy_match(line))
        .collect();
    let search_block = normalized_search.join("\n");
    if search_block.is_empty() {
        return Vec::new();
    }
    let search_char_len = search_block.chars().count();
    if lines.len() * search_char_len.saturating_mul(search_char_len) > FUZZY_WORK_LIMIT {
        return Vec::new();
    }
    let normalized_lines: Vec<String> = lines
        .iter()
        .map(|line| normalize_for_fuzzy_match(&line.text))
        .collect();

    let mut candidates: Vec<(usize, f64)> = Vec::new();
    for start in 0..=lines.len().saturating_sub(window_size) {
        let window_block = normalized_lines[start..start + window_size].join("\n");
        let raw_distance = levenshtein_chars(&window_block, &search_block);
        let length_difference = window_block.chars().count().abs_diff(search_char_len);
        if search_char_len > 0
            && length_difference as f64 / search_char_len as f64
                > FUZZY_MATCH_THRESHOLD / FUZZY_WHITESPACE_PENALTY
        {
            continue;
        }
        let stripped_window: String = window_block
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        let stripped_search: String = search_block
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        let normalized_distance = levenshtein_chars(&stripped_window, &stripped_search);
        let weighted_distance = normalized_distance as f64
            + (raw_distance - normalized_distance) as f64 * FUZZY_WHITESPACE_PENALTY;
        let score = weighted_distance / search_char_len as f64;
        if score <= FUZZY_MATCH_THRESHOLD {
            candidates.push((start, score));
        }
    }
    if candidates.is_empty() {
        return Vec::new();
    }
    // 选取得分最高的非重叠窗口（重叠定义：起始行相距 < window_size）。
    candidates.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    let mut selected: Vec<usize> = Vec::new();
    for (start, _) in candidates {
        if selected
            .iter()
            .all(|existing| existing.abs_diff(start) >= window_size)
        {
            selected.push(start);
        }
    }
    selected.sort_unstable();
    selected
        .into_iter()
        .map(|start| CandidateMatch {
            strategy: EditStrategy::Fuzzy,
            start: lines[start].start,
            end: lines[start + window_size - 1].end,
            line_range: LineRange::new(start as u64 + 1, (start + window_size) as u64),
        })
        .collect()
}

struct ContentLine {
    text: String,
    start: usize,
    end: usize,
}

/// 按 `\n` 切行并保留字节区间；`text` 不含行尾。
fn content_lines(content: &str) -> Vec<ContentLine> {
    let mut lines = Vec::new();
    let mut offset = 0usize;
    for line in split_lines_with_endings(content) {
        let start = offset;
        offset += line.len();
        let text = match line.strip_suffix('\n') {
            Some(stripped) => stripped.strip_suffix('\r').unwrap_or(stripped).to_string(),
            None => line.to_string(),
        };
        lines.push(ContentLine {
            text,
            start,
            end: offset,
        });
    }
    lines
}

/// 1-based 行号（`byte_offset` 必须落在 char boundary）。
fn line_number_at(content: &str, byte_offset: usize) -> u64 {
    content[..byte_offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u64
        + 1
}

fn line_range_of_span(content: &str, start: usize, end: usize) -> LineRange {
    let start_line = line_number_at(content, start);
    let mut end_line = line_number_at(content, end);
    if end > start && content[..end].ends_with('\n') {
        end_line = end_line.saturating_sub(1).max(start_line);
    }
    LineRange::new(start_line, end_line)
}

/// `newText`（原样）是否已在文件中；返回第一处行区间。
fn already_applied_range(content: &str, new_text: &str) -> Option<LineRange> {
    if new_text.is_empty() {
        return None;
    }
    if let Some((start, _)) = content.match_indices(new_text).next() {
        return Some(line_range_of_span(content, start, start + new_text.len()));
    }
    quote_normalized_matches(content, new_text)
        .first()
        .map(|(start, end)| line_range_of_span(content, *start, *end))
}

/// fuzzy 命中后把新文本对齐到原文缩进，并保留匹配区域末尾的换行。
fn align_fuzzy_replacement(base: &str, matched: &CandidateMatch, new_text: &str) -> String {
    let first_line_start = base[..matched.start]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let indent: String = base[first_line_start..]
        .chars()
        .take_while(|character| *character == ' ' || *character == '\t')
        .collect();
    let mut text = if indent.is_empty() {
        new_text.to_string()
    } else {
        new_text
            .split('\n')
            .map(|line| {
                if line.is_empty() || line.starts_with(&indent) {
                    line.to_string()
                } else {
                    format!("{indent}{line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    if matched.end > matched.start
        && base[..matched.end].ends_with('\n')
        && !text.is_empty()
        && !text.ends_with('\n')
    {
        text.push('\n');
    }
    text
}

/// 构造单个 edit 的 unified hunk（4 行上下文）。
fn build_hunk(
    base: &str,
    new_content: &str,
    replacement: &Replacement,
    new_start_line: u64,
) -> String {
    let base_lines = content_lines(base);
    let new_lines = content_lines(new_content);
    let old_start = replacement.line_range.start as usize;
    let old_end = replacement.line_range.end as usize;
    let context = 4usize;
    let before_start = (old_start.saturating_sub(1)).saturating_sub(context);
    let before = &base_lines[before_start..old_start - 1];
    let after_end = (old_end + context).min(base_lines.len());
    let after = &base_lines[old_end..after_end];

    let replacement_lines: Vec<&str> = if replacement.new_text.is_empty() {
        Vec::new()
    } else {
        let stripped = replacement
            .new_text
            .strip_suffix('\n')
            .unwrap_or(&replacement.new_text);
        stripped.split('\n').collect()
    };
    let new_count = replacement_lines.len();
    let old_count = old_end - old_start + 1;

    let old_hunk_start = before_start + 1;
    let new_hunk_start = new_start_line.saturating_sub(before.len() as u64).max(1);
    let old_hunk_count = before.len() + old_count + after.len();
    let new_hunk_count = before.len() + new_count + after.len();

    let new_index_start = new_start_line.saturating_sub(1) as usize;
    let hunk_after: Vec<&str> = new_lines
        .iter()
        .skip(new_index_start + new_count)
        .take(context)
        .map(|line| line.text.as_str())
        .collect();

    let mut output =
        format!("@@ -{old_hunk_start},{old_hunk_count} +{new_hunk_start},{new_hunk_count} @@");
    for line in before {
        output.push('\n');
        output.push(' ');
        output.push_str(&line.text);
    }
    for line in &base_lines[old_start - 1..old_end] {
        output.push('\n');
        output.push('-');
        output.push_str(&line.text);
    }
    for line in &replacement_lines {
        output.push('\n');
        output.push('+');
        output.push_str(line);
    }
    for line in hunk_after {
        output.push('\n');
        output.push(' ');
        output.push_str(line);
    }
    output
}

/// ±4 行片段：行号 + 内容，变更行用 `>` 标记。
fn build_snippet(new_content: &str, new_start_line: u64, new_text: &str) -> String {
    let lines = content_lines(new_content);
    let new_count = if new_text.is_empty() {
        0
    } else {
        new_text
            .strip_suffix('\n')
            .unwrap_or(new_text)
            .split('\n')
            .count()
    };
    let start_line = new_start_line.saturating_sub(4).max(1) as usize;
    let end_line = ((new_start_line as usize + new_count + 3).min(lines.len())).max(start_line);
    let width = end_line.to_string().len();
    let mut output = Vec::new();
    for line_number in start_line..=end_line {
        let Some(line) = lines.get(line_number - 1) else {
            break;
        };
        let changed = line_number as u64 >= new_start_line
            && (line_number as u64) < new_start_line + new_count as u64;
        output.push(format!(
            "{}{:>width$} | {}",
            if changed { '>' } else { ' ' },
            line_number,
            line.text,
            width = width
        ));
    }
    output.join("\n")
}

/// 最多 `limit` 个最近似行窗口（按相似度降序、起点升序）。
pub fn nearest_matches(content: &str, old_text: &str, limit: usize) -> Vec<NearestMatch> {
    let lines = content_lines(content);
    if lines.is_empty() {
        return Vec::new();
    }
    let search_lines: Vec<String> = old_text
        .split('\n')
        .map(normalize_for_fuzzy_match)
        .collect();
    let window_size = search_lines.len().max(1).min(lines.len());
    let search_block = search_lines.join("\n");
    let search_chars = search_block.chars().count();
    let normalized_lines: Vec<String> = lines
        .iter()
        .map(|line| normalize_for_fuzzy_match(&line.text))
        .collect();

    let windows: usize = lines.len().saturating_sub(window_size) + 1;
    // 超大文件只抽样，避免错误路径卡住；仍然覆盖文件开头。
    let stride = (windows / 20_000).max(1);
    let mut scored: Vec<(usize, f64)> = Vec::new();
    let mut start = 0usize;
    while start + window_size <= lines.len() {
        let window_block = normalized_lines[start..start + window_size].join("\n");
        let similarity = text_similarity(&window_block, &search_block, search_chars);
        scored.push((start, similarity));
        start += stride;
    }
    scored.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(start, similarity)| {
            let snippet = lines[start..start + window_size]
                .iter()
                .map(|line| truncate_chars(&line.text, 160))
                .collect::<Vec<_>>()
                .join("\n");
            (
                LineRange::new(start as u64 + 1, (start + window_size) as u64),
                snippet,
                similarity,
            )
        })
        .collect()
}

fn text_similarity(window: &str, search: &str, search_chars: usize) -> f64 {
    let window_chars = window.chars().count();
    let max_len = window_chars.max(search_chars);
    if max_len == 0 {
        return 1.0;
    }
    // 极端长文本用前后缀公共长度近似，避免 O(n*m) 阻塞错误路径。
    if max_len > 8_000 {
        let common = window
            .chars()
            .zip(search.chars())
            .take_while(|(left, right)| left == right)
            .count();
        return common as f64 / max_len as f64;
    }
    let distance = levenshtein_chars(window, search);
    1.0 - distance as f64 / max_len as f64
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut result: String = text.chars().take(limit).collect();
    result.push('…');
    result
}

/// 字符级 Levenshtein（两行动态规划）。
fn levenshtein_chars(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0usize; right.len() + 1];
    for (left_index, left_char) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right.iter().enumerate() {
            let cost = usize::from(left_char != right_char);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str) -> ReplaceEdit {
        ReplaceEdit {
            old_text: old.to_string(),
            new_text: new.to_string(),
        }
    }

    fn apply(content: &str, edits: &[ReplaceEdit]) -> FileEditPlan {
        match plan_file_edits("f.txt", content, edits) {
            FileEditOutcome::Applied(plan) => plan,
            FileEditOutcome::Rejected(rejection) => panic!("{}", rejection.message()),
        }
    }

    #[test]
    fn exact_multi_edit_applies_atomically() {
        let plan = apply(
            "alpha\nbeta\ngamma\n",
            &[edit("alpha\n", "ALPHA\n"), edit("gamma\n", "GAMMA\n")],
        );
        assert_eq!(plan.new_content, "ALPHA\nbeta\nGAMMA\n");
        assert_eq!(plan.strategy, EditStrategy::Exact);
        assert_eq!(plan.edits.len(), 2);
        assert!(plan.edits.iter().all(|edit| !edit.hunk.is_empty()));
        assert!(plan.edits.iter().all(|edit| !edit.snippet.is_empty()));
    }

    #[test]
    fn one_bad_edit_rejects_the_whole_file() {
        let outcome = plan_file_edits(
            "f.txt",
            "alpha\nbeta\n",
            &[edit("alpha\n", "ALPHA\n"), edit("missing\n", "X\n")],
        );
        let FileEditOutcome::Rejected(rejection) = outcome else {
            panic!("should reject");
        };
        assert_eq!(rejection.error.code(), "not_found");
        assert_eq!(rejection.statuses.len(), 2);
        assert!(matches!(rejection.statuses[0], EditStatus::Matched { .. }));
        assert!(matches!(rejection.statuses[1], EditStatus::Failed { .. }));
        let message = rejection.message();
        assert!(message.contains("edits[0]: matched"), "{message}");
        assert!(message.contains("edits[1]: failed"), "{message}");
        assert!(message.contains("Do not resend"));
    }

    #[test]
    fn overlapping_occurrences_are_not_unique() {
        // "abab" 在 "ababab" 里出现两次，第二次与第一次重叠；`match_indices`
        // 只会报告一次，不能据此认为匹配唯一。
        let outcome = plan_file_edits("f.txt", "ababab\n", &[edit("abab", "x")]);
        let FileEditOutcome::Rejected(rejection) = outcome else {
            panic!("overlapping occurrences must not be treated as unique");
        };
        assert_eq!(rejection.error.code(), "not_unique");
        let EditError::NotUnique { occurrences, .. } = rejection.error else {
            panic!("expected NotUnique");
        };
        assert_eq!(occurrences, 2);
    }

    #[test]
    fn overlapping_quote_normalized_occurrences_are_not_unique() {
        // 内容里的弯引号在归一化后与 ASCII 撇号等价；两次出现互相重叠。
        let outcome = plan_file_edits("f.txt", "ab\u{2019}ab\u{2019}ab\n", &[edit("ab'ab", "x")]);
        let FileEditOutcome::Rejected(rejection) = outcome else {
            panic!("overlapping quote-normalized occurrences must not be unique");
        };
        assert_eq!(rejection.error.code(), "not_unique");
    }

    #[test]
    fn not_unique_caps_the_listed_line_ranges() {
        let content: String = (0..100).map(|_| "dup\n").collect();
        let outcome = plan_file_edits("f.txt", &content, &[edit("dup\n", "x\n")]);
        let FileEditOutcome::Rejected(rejection) = outcome else {
            panic!("should reject");
        };
        let EditError::NotUnique {
            occurrences,
            line_ranges,
            ..
        } = &rejection.error
        else {
            panic!("{:?}", rejection.error);
        };
        assert_eq!(*occurrences, 100);
        assert_eq!(line_ranges.len(), MAX_LISTED_NOT_UNIQUE_RANGES);
        let message = rejection.message();
        assert!(message.contains("first 20 of 100"), "{message}");
        assert!(message.len() < 4_000, "error message must stay bounded");
    }

    #[test]
    fn not_unique_lists_all_line_numbers() {
        let outcome = plan_file_edits("f.txt", "foo\nbar\nfoo\n", &[edit("foo\n", "x\n")]);
        let FileEditOutcome::Rejected(rejection) = outcome else {
            panic!("should reject");
        };
        let EditError::NotUnique {
            occurrences,
            line_ranges,
            ..
        } = &rejection.error
        else {
            panic!("{:?}", rejection.error);
        };
        assert_eq!(*occurrences, 2);
        assert_eq!(line_ranges, &[LineRange::new(1, 1), LineRange::new(3, 3)]);
        assert!(rejection.message().contains("at line 1, line 3"));
    }

    #[test]
    fn already_applied_is_detected() {
        let outcome = plan_file_edits("f.txt", "world\n", &[edit("hello", "world")]);
        let FileEditOutcome::Rejected(rejection) = outcome else {
            panic!("should reject");
        };
        assert_eq!(rejection.error.code(), "already_applied");
    }

    #[test]
    fn empty_old_text_is_rejected() {
        let outcome = plan_file_edits("f.txt", "abc", &[edit("", "x")]);
        let FileEditOutcome::Rejected(rejection) = outcome else {
            panic!("should reject");
        };
        assert_eq!(rejection.error.code(), "empty_replacement");
    }

    #[test]
    fn overlapping_edits_are_rejected() {
        let outcome = plan_file_edits(
            "f.txt",
            "one\ntwo\nthree\n",
            &[edit("one\ntwo\n", "1\n2\n"), edit("two\nthree\n", "2\n3\n")],
        );
        let FileEditOutcome::Rejected(rejection) = outcome else {
            panic!("should reject");
        };
        assert_eq!(rejection.error.code(), "overlap");
    }

    #[test]
    fn fuzzy_matches_trailing_whitespace_and_reports_strategy() {
        let plan = apply(
            "line one   \nline two  \nline three\n",
            &[edit("line one\nline two\n", "replaced\n")],
        );
        assert_eq!(plan.new_content, "replaced\nline three\n");
        assert_eq!(plan.strategy, EditStrategy::Fuzzy);
        assert_eq!(plan.edits[0].strategy, EditStrategy::Fuzzy);
    }

    #[test]
    fn quote_normalization_preserves_original_offsets() {
        let plan = apply(
            "console.log(\u{2018}hello\u{2019});\n",
            &[edit("console.log('hello');", "console.log('world');")],
        );
        assert_eq!(plan.strategy, EditStrategy::QuoteNormalized);
        assert_eq!(plan.new_content, "console.log('world');\n");
    }

    #[test]
    fn fuzzy_preserves_untouched_lines() {
        let content = "keep before  \nfirst target  \nfirst after\nkeep after  \n";
        let plan = apply(
            content,
            &[edit("first target\nfirst after", "FIRST\nFIRST2")],
        );
        assert_eq!(
            plan.new_content,
            "keep before  \nFIRST\nFIRST2\nkeep after  \n"
        );
    }

    #[test]
    fn nearest_matches_returns_three_windows() {
        let content = "fn main() {\n    run();\n}\n\nfn other() {\n    rest();\n}\n";
        let nearest = nearest_matches(content, "fn main() {\n    walk();\n}\n", 3);
        assert!(!nearest.is_empty());
        assert!(nearest.len() <= 3);
        assert!(nearest[0].2 > 0.5);
        assert_eq!(nearest[0].0.start, 1);
    }
}
