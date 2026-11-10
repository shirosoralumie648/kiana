//! core/tools/edit-diff.ts。
//!
//! `edit` 工具的 diff/patch 与模糊匹配核心。本文件是 crate 内风险最高的移植：
//!
//! - **`diff` npm 包（8.0.4）的 Myers 实现**被逐行移植（`diff_lines`），包括
//!   diagonal 剪枝、`addToPath` 的相邻同类合并、`removeEmpty` 与行 tokenize 规则。
//!   `generateDiffString` 与 `generateUnifiedPatch` 都建立在这份 parts 上，因此
//!   hunk 边界、删除/新增的先后顺序由黄金 fixture 逐字节锁定（黄金测试见
//!   `tests/edit_diff.rs`，fixture 由 TS 参考实现生成）。
//! - **`createTwoFilesPatch` + `FILE_HEADERS_ONLY` 的实测行为**（§8.5 未决问题的答案）：
//!   首行**没有** 67 个 `=` 的分隔线（`FILE_HEADERS_ONLY.includeUnderline === false`），
//!   `--- path` / `+++ path` 无时间戳，`@@` 头**总是**带 `,count`（长度为 1 也不省略），
//!   内容相同只输出两行文件头，且输出**总是**以 `\n` 结尾。文件末尾无换行时插入
//!   `\ No newline at end of file`。规格 §8.3 的「67 个等号 / 无尾随换行 / 相同内容为空串」
//!   与 TS 源不符，以 TS 源（实测）为准。
//! - JS 的字符串索引是 UTF-16 code unit；这里**统一使用 UTF-8 字节偏移**
//!   （`matchIndex`/`matchLength`/行 span 都是内部量，不进入任何输出），对合法 UTF-8
//!   输入与 JS 等价，含 CJK/emoji 的黄金用例覆盖了这一点。
//! - [`normalize_for_fuzzy_match`] 的 `trimEnd` 使用 JS 的 `WhiteSpace +
//!   LineTerminator` 集合（Rust 的 `trim_end` 含 U+0085 但不含 U+FEFF，两者不同）。
//! - [`apply_edits_to_normalized_content`] 的错误文本逐字保留（§2.8.6；用户可见契约，改动需同步测试），并且
//!   `occurrences > 1` 的检查对**每一个** edit 都执行，即便它在初匹配里是精确命中。
//!
//! # 在 crate 中的位置
//!
//! 本模块只做纯字符串运算；文件 IO 只出现在 [`compute_edits_diff`] /
//! [`compute_edit_diff`]（可读性预检 + 读文件），用于 TUI 预览。真正执行编辑的
//! `crate::edit` 自己读文件，然后按「`split_bom` → [`detect_line_ending`] →
//! [`normalize_to_lf`] → [`apply_edits_to_normalized_content`] → 写回 →
//! [`generate_diff_string`] / [`generate_unified_patch`]」的顺序调用本模块。
//!
//! 上游调用方：
//! - `crate::edit`：执行编辑，并把 diff/patch 放进工具结果的详情；
//! - `crate::renderers::edit`：参数补全后用 [`compute_edits_diff`] 异步预演
//!   （只读，不写盘），把 [`EditDiffResult`] 或 [`EditDiffError`] 存进渲染状态；
//! - `crate::read` 与 `crate::edit` 共用 `decode_utf8`（`pub(crate)`）。
//!
//! # 一次编辑的执行流程
//!
//! 1. 调用方读文件、按 WHATWG 规则解码 UTF-8、`split_bom`、[`normalize_to_lf`]；
//! 2. [`apply_edits_to_normalized_content`] 把每个 edit 的 `oldText`/`newText` 也
//!    归一化为 LF，并拒绝空 `oldText`；
//! 3. 先探测每个 edit 能否精确命中；只要有一个需要模糊匹配，整份基底就切换为
//!    [`normalize_for_fuzzy_match`] 的结果（全局开关，不是逐 edit 决定）；
//! 4. 在选定基底上逐个重新定位，检查存在性与出现次数（> 1 报 duplicate）；
//! 5. 按 `matchIndex` 排序，检查替换区间是否重叠（首尾相接允许）；
//! 6. 逆序应用替换（先应用靠后的替换，靠前的偏移保持有效），新旧内容相同则报
//!    no-change；
//! 7. 调用方还原 BOM/行尾写回，再用本模块生成展示 diff 与 unified patch。
//!
//! # 注意事项
//!
//! - `match_index` / `match_length` / 行 span 都是 UTF-8 **字节**偏移，不进入
//!   diff/patch 文本输出；JS 的字符串下标是 UTF-16 码元，只在纯 ASCII 文本上与字节
//!   偏移数值相同。唯一按 UTF-16 计数的地方是 `count_occurrences` 的空 needle 分支。
//! - 错误消息是用户/模型可见文本（[`EditError::message`] 原样成为
//!   [`EditDiffError::error`]），并被 `tests/edit_diff.rs` 的黄金 fixture 逐字锁定；
//!   改文案等于改行为。
//! - 模糊归一化会去掉每行行尾空白；为此替换触及的行从归一化基底重建，**未触及的行
//!   逐字节取自原文**（[`apply_replacements_preserving_unchanged_lines`]），否则会把
//!   文件里其它位置的尾随空白一并抹掉。
//! - 无 edit（空数组）不会报 empty oldText，而是在最后因新旧内容相同报 no-change
//!   （单/复数措辞由 `total_edits == 1` 决定，0 走复数分支）。
//!
//! # 阅读顺序
//!
//! 行尾处理 → 模糊归一化 → [`apply_edits_to_normalized_content`]；
//! `diff_lines` 的 Myers 实现与两个输出函数可以最后读——它们只消费前面得到的文本。

use std::path::Path;

use kiana_core::text::split_bom;
/// `edit` 工具的单个替换项（`old_text` → `new_text`），对应 TS `edit.ts` 的
/// `Edit` 接口（TypeBox `replaceEditSchema` 的内联对象）；`ReplaceEdit` 是 Rust 侧 helper。
///
/// 字段来自工具参数，行尾可能是 CRLF；[`apply_edits_to_normalized_content`] 会在
/// 匹配前把它们各自归一化为 LF。
pub use kiana_ext_api::tool_types::ReplaceEdit as Edit;
use serde::{Deserialize, Serialize};

use crate::path_utils::resolve_to_cwd;
use crate::read::{R_OK, access_path, read_file_bytes};

// ===========================================================================
// 行尾处理
// ===========================================================================

/// 文件的行尾风格。TS: `detectLineEnding` 的 `"\r\n" | "\n"`。
///
/// 整份文件只取一种风格，按**首个换行**判定（见 [`detect_line_ending`]），混合行尾的
/// 文件也不会逐行保留；`edit` 写回时 [`restore_line_endings`] 会把所有换行统一成它。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    /// `"\r\n"`。
    Crlf,
    /// `"\n"`。
    Lf,
}

impl LineEnding {
    /// 该行尾风格的字符串形式。
    pub const fn as_str(self) -> &'static str {
        match self {
            LineEnding::Crlf => "\r\n",
            LineEnding::Lf => "\n",
        }
    }
}

/// `detectLineEnding(content)`：首个 `\r\n` 出现在首个 `\n` 之前才判为 CRLF。
///
/// 其余情况一律返回 [`LineEnding::Lf`]：没有换行、只有裸 `\r`、或第一个 `\n` 早于
/// 第一个 `\r\n`（例如 `"\n\r\n"`）。裸 `\r` 不参与判定，只在 [`normalize_to_lf`]
/// 里被当作换行处理。
pub fn detect_line_ending(content: &str) -> LineEnding {
    let lf_index = content.find('\n');
    if lf_index.is_none() {
        return LineEnding::Lf;
    }
    let crlf_index = content.find("\r\n");
    match crlf_index {
        Some(crlf) => {
            // `lf_index` 已在上面确认存在。
            let lf = lf_index.expect("checked above");
            if crlf < lf {
                LineEnding::Crlf
            } else {
                LineEnding::Lf
            }
        }
        None => LineEnding::Lf,
    }
}

/// `normalizeToLF(text)`：`\r\n` → `\n`，再单独的 `\r` → `\n`。
///
/// 输出只含 LF，因此匹配前对内容和 `oldText` 各做一次就能消除行尾差异；该函数是
/// 幂等的。注意它**不**处理其他 Unicode 行分隔符（如 U+2028）。
pub fn normalize_to_lf(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            output.push('\n');
        } else {
            output.push(character);
        }
    }
    output
}

/// `restoreLineEndings(text, ending)`：CRLF 模式下把每个 `\n` 换成 `\r\n`。
///
/// 输入必须是 LF 形态（通常是 [`normalize_to_lf`] / [`AppliedEditsResult`] 的输出）：
/// 它只做 `\n` → `\r\n` 的替换，若文本里本来就有 `\r\n` 会得到 `\r\r\n`。
pub fn restore_line_endings(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Crlf => text.replace('\n', "\r\n"),
        LineEnding::Lf => text.to_string(),
    }
}

// ===========================================================================
// 模糊匹配归一化
// ===========================================================================

/// JS 的 `String.prototype.trimEnd` 空白集合（`WhiteSpace + LineTerminator`）。
///
/// 与 Rust 的 [`str::trim_end`] 不同：JS 包含 U+FEFF（ZWNBSP），不包含 U+0085（NEL）。
fn is_js_trim_end_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000d}'
            | ' '
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// `normalizeForFuzzyMatch(text)`：NFKC → 逐行 `trimEnd` → 智能引号/破折号/特殊空格。
///
/// 顺序固定为 NFKC → trimEnd：NFKC 可能把空白变体折叠成普通空格，之后才会被 `trimEnd` 去掉。
///
/// 结果就是后文的“模糊匹配空间”：`content` 与 `oldText` 分别独立归一化后做子串
/// 查找，所以得到的偏移是归一化文本上的字节偏移，不能拿去直接切原文（见
/// [`fuzzy_find_text`] 与 [`apply_replacements_preserving_unchanged_lines`]）。
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;

    let normalized: String = text.nfkc().collect();
    let trimmed = normalized
        .split('\n')
        .map(|line| line.trim_end_matches(is_js_trim_end_whitespace))
        .collect::<Vec<_>>()
        .join("\n");

    trimmed
        .chars()
        .map(|character| match character {
            // Smart single quotes → '
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            // Smart double quotes → "
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            // Various dashes/hyphens → -
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            // Special spaces → regular space
            '\u{00a0}' | '\u{2002}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

// ===========================================================================
// 行切分与 span
// ===========================================================================

/// `splitLinesWithEndings(content)`：每行保留结尾的 `\n`；末尾无换行的最后一行也收。
///
/// TS 的正则是 `/[^\n]*\n|[^\n]+/g`：`"a\nb"` → `["a\n", "b"]`，`""` → `[]`。
///
/// 只按字节 `\n` 切分（UTF-8 多字节序列中不会出现 `\n`），所以 CRLF 的 `\r` 留在
/// 行尾；输入以 `\n` 结尾时不会多出一个空行。
pub fn split_lines_with_endings(content: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            lines.push(&content[start..=index]);
            start = index + 1;
        }
    }
    if start < content.len() {
        lines.push(&content[start..]);
    }
    lines
}

/// 一行的字节范围（`[start, end)`）。
///
/// 由 `get_line_spans` 生成，按行序首尾相接、互不重叠；空内容没有任何 span。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineSpan {
    start: usize,
    end: usize,
}

/// `getLineSpans(content)`。
///
/// 依次累加 [`split_lines_with_endings`] 各行的字节长度得到 `[start, end)`；行列表
/// 已含行尾 `\n`，因此 span 之间恰好首尾相接，空内容返回空 `Vec`。
fn get_line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0usize;
    split_lines_with_endings(content)
        .into_iter()
        .map(|line| {
            let span = LineSpan {
                start: offset,
                end: offset + line.len(),
            };
            offset = span.end;
            span
        })
        .collect()
}

// ===========================================================================
// 替换
// ===========================================================================

/// 一处待应用的替换（`Pick<MatchedEdit, "matchIndex" | "matchLength" | "newText">`）。
///
/// 索引以**替换基底**（通常是 [`AppliedEditsResult`] 的 `base_content`，模糊模式下
/// 是 [`normalize_for_fuzzy_match`] 的结果）为准，单位是 UTF-8 字节。构造本结构不
/// 校验范围：`apply_replacements` 直接按字节切片，越界区间会 panic。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextReplacement {
    /// 匹配起点（字节偏移，作用在 `baseContent` 上）。
    pub match_index: usize,
    /// 匹配长度（字节）。
    pub match_length: usize,
    /// 替换文本。
    pub new_text: String,
}

/// `getReplacementLineRange(lines, replacement)`。
///
/// 把一次替换映射成行区间：起始行是包含 `match_index` 的那一行；结束行是第一个
/// `end >= match_index + match_length` 的行；返回半开行区间 `[start, end + 1)`。
/// 零长度匹配归入起点所在行。若起点落在所有行之后（例如正好等于 `content.len()`）
/// 或替换越过最后一行，返回 “Replacement range is outside the base content.”。
fn get_replacement_line_range(
    lines: &[LineSpan],
    replacement: &TextReplacement,
) -> Result<(usize, usize), EditError> {
    let replacement_start = replacement.match_index;
    let replacement_end = replacement.match_index + replacement.match_length;

    let mut start_line = None;
    for (index, line) in lines.iter().enumerate() {
        if replacement_start >= line.start && replacement_start < line.end {
            start_line = Some(index);
            break;
        }
    }
    let Some(start_line) = start_line else {
        return Err(EditError::new(
            "Replacement range is outside the base content.",
        ));
    };

    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return Err(EditError::new(
            "Replacement range is outside the base content.",
        ));
    }

    Ok((start_line, end_line + 1))
}

/// `applyReplacements(content, replacements, offset = 0)`：逆序应用，偏移保持稳定。
///
/// `content` 是完整基底的一个切片，`offset` 是该切片在完整基底中的起点；每个
/// replacement 的 `match_index` 是完整基底上的绝对偏移，循环内先减去 `offset`。
/// 先应用靠后的替换，靠前替换的绝对偏移在后续切片中仍然有效。
///
/// 调用方必须保证 replacements 已按 `match_index` 升序排列、互不重叠且区间落在切片
/// 内；函数不做校验。越界一定 panic；乱序不报错，可能静默产生错误结果（较早的替换
/// 改变长度后，靠后替换的绝对偏移会失效）。
fn apply_replacements(content: &str, replacements: &[TextReplacement], offset: usize) -> String {
    let mut result = content.to_string();
    for replacement in replacements.iter().rev() {
        let match_index = replacement.match_index - offset;
        result = format!(
            "{}{}{}",
            &result[..match_index],
            replacement.new_text,
            &result[match_index + replacement.match_length..]
        );
    }
    result
}

/// `applyReplacementsPreservingUnchangedLines(originalContent, baseContent, replacements)`。
///
/// 把归一化基底上的替换按「实际触及的行」加宽，未被触及的行原样取自
/// `original_content`（保住原始尾随空白等字节）。
///
/// # 前提与失败
///
/// - `base_content` 与 `original_content` 必须行数相同（按
///   [`split_lines_with_endings`] 的口径），否则返回固定错误
///   “Cannot preserve unchanged lines because the base content has a different line
///   count.”。
/// - 每个替换的区间必须落在 `base_content` 内，否则返回
///   “Replacement range is outside the base content.”。
///
/// # 分组规则
///
/// 替换按 `match_index` 升序处理：若某替换的起始行落在当前组已覆盖的行内
/// （`start_line < current.end_line`），并入该组并扩大结束行；否则另起一组。每组只把
/// `base_content` 的对应行切片交给 `apply_replacements`，组之间与末尾的剩余行直接
/// 拼接 `original_content`。函数不检查同组替换是否重叠——该不变量由调用方
/// [`apply_edits_to_normalized_content`] 保证。
pub fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[TextReplacement],
) -> Result<String, EditError> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return Err(EditError::new(
            "Cannot preserve unchanged lines because the base content has a different line count.",
        ));
    }

    struct Group {
        start_line: usize,
        end_line: usize,
        replacements: Vec<TextReplacement>,
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut sorted_replacements = replacements.to_vec();
    sorted_replacements.sort_by_key(|replacement| replacement.match_index);
    for replacement in sorted_replacements {
        let (start_line, end_line) = get_replacement_line_range(&base_lines, &replacement)?;
        if let Some(current) = groups.last_mut()
            && start_line < current.end_line
        {
            current.end_line = current.end_line.max(end_line);
            current.replacements.push(replacement);
            continue;
        }
        groups.push(Group {
            start_line,
            end_line,
            replacements: vec![replacement],
        });
    }

    let mut original_line_index = 0usize;
    let mut result = String::new();
    for group in groups {
        result.push_str(&original_lines[original_line_index..group.start_line].concat());

        let group_start_offset = base_lines[group.start_line].start;
        let group_end_offset = base_lines[group.end_line - 1].end;
        let rewritten = apply_replacements(
            &base_content[group_start_offset..group_end_offset],
            &group.replacements,
            group_start_offset,
        );
        result.push_str(&rewritten);
        original_line_index = group.end_line;
    }
    result.push_str(&original_lines[original_line_index..].concat());

    Ok(result)
}

// ===========================================================================
// 模糊查找
// ===========================================================================

/// `FuzzyMatchResult`。
///
/// 不变量：`found == true` 时 `index`/`match_length` 的坐标系是
/// `content_for_replacement`，而不是调用方传入的 `content`（模糊命中时前者已被
/// 归一化）；`found == false` 时 `index == -1`、`match_length == 0`、
/// `used_fuzzy_match == false`，且 `content_for_replacement` 退回原始内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatchResult {
    /// 是否找到。
    pub found: bool,
    /// 匹配起点（`content_for_replacement` 上的字节偏移）；未命中时为 `-1`。
    pub index: isize,
    /// 匹配长度（字节）。
    pub match_length: usize,
    /// 是否使用了模糊匹配（false = 精确匹配）。
    pub used_fuzzy_match: bool,
    /// 替换操作应使用的内容（精确匹配时是原文，模糊匹配时是归一化文本）。
    pub content_for_replacement: String,
}

/// `fuzzyFindText(content, oldText)`：先精确，再在模糊归一化空间里查找。
///
/// 两个阶段：
/// 1. 在 `content` 上做精确子串查找（取第一次出现）；命中则 `used_fuzzy_match =
///    false`，位置与长度都是原文字节偏移。
/// 2. 否则对 `content` 与 `old_text` 分别做 [`normalize_for_fuzzy_match`] 再查找；
///    命中则 `used_fuzzy_match = true`，`match_length` 取归一化后 needle 的字节长度。
///
/// 本函数不会失败：两阶段都不命中时返回 `found = false`。注意 `find("")` 会在位置
/// 0 命中且长度为 0：若精确阶段未命中且归一化后的 `oldText` 为空（例如纯空白），
/// 模糊阶段会在位置 0 得到零长度匹配。出现次数检查只能拦住 occurrences > 1 的情况
/// （内容只有 1 个 UTF-16 码元时 occurrences = 0，会被当作插入应用），不能普遍兜底。
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    if let Some(exact_index) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index: exact_index as isize,
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        };
    }

    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    match fuzzy_content.find(&fuzzy_old_text) {
        Some(fuzzy_index) => FuzzyMatchResult {
            found: true,
            index: fuzzy_index as isize,
            match_length: fuzzy_old_text.len(),
            used_fuzzy_match: true,
            content_for_replacement: fuzzy_content,
        },
        None => FuzzyMatchResult {
            found: false,
            index: -1,
            match_length: 0,
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        },
    }
}

/// `countOccurrences(content, oldText)`：`normalized.split(normalizedOld).length - 1`。
///
/// 计数在归一化空间进行，与该 edit 是否精确命中无关：两个字节不同但归一化后相同的
/// 出现也会被算作重复。非空 needle 用非重叠子串计数，等价于 JS 的 split 计数；结果
/// 只用于“必须唯一”的判断（`> 1` 时报 duplicate）。
fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    if fuzzy_old_text.is_empty() {
        // JS `"abc".split("")` 返回 ["a","b","c"]（长度 = code unit 数），
        // 所以 `split(fuzzyOldText).length - 1` 是 `utf16_len - 1`。
        // 空 oldText 在调用方已被拦截，但纯空白 oldText 经 NFKC + trimEnd 归一化后
        // 也会变空（例如 " "），这条分支因此可达。
        return fuzzy_content.encode_utf16().count().saturating_sub(1);
    }
    fuzzy_content.matches(fuzzy_old_text.as_str()).count()
}

// ===========================================================================
// 编辑应用
// ===========================================================================

/// `AppliedEditsResult`。
///
/// 两个字段都是 **LF 形态、不含 BOM**——前提是调用方按约定先 `split_bom` +
/// [`normalize_to_lf`]（构造它的函数不处理 BOM，也不归一化内容行尾）。`base_content`
/// 是 diff 的“前”侧，`new_content` 是“后”侧；使用模糊匹配时后者只重建被触及的行，
/// 其余行仍是原文（LF 形态）的字节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEditsResult {
    /// 用于 diff 的基底（未做模糊归一化的 `normalizedContent`）。
    pub base_content: String,
    /// 应用替换后的内容。
    pub new_content: String,
}

/// 编辑应用失败（TS 里由这些函数直接 `throw new Error(message)`）。
///
/// `Display` 只输出 `message`（无前缀）。`edit.rs` 执行路径把它放进
/// `AgentToolError.message`，预览路径放进 [`EditDiffError::error`]，两者都会原样
/// 展示，因此文案是接口的一部分。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct EditError {
    /// 逐字的错误消息。
    pub message: String,
}

impl EditError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

// 下面四个构造函数生成逐字的 TS 错误消息：单个 edit（total_edits == 1）与多个 edit
// 的措辞不同，`tests/edit_diff.rs` 的黄金 fixture 会断言这些字符串，改文案等于改行为。
// 同一批 edit 的处理顺序是：先对所有 edit 检查空 oldText，再按 edit 顺序逐个检查
// not-found / duplicate，全部定位成功后才检查 overlap，最后才比较新旧内容。
fn get_not_found_error(path: &str, edit_index: usize, total_edits: usize) -> EditError {
    if total_edits == 1 {
        EditError::new(format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        ))
    } else {
        EditError::new(format!(
            "Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines."
        ))
    }
}

fn get_duplicate_error(
    path: &str,
    edit_index: usize,
    total_edits: usize,
    occurrences: usize,
) -> EditError {
    if total_edits == 1 {
        EditError::new(format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        ))
    } else {
        EditError::new(format!(
            "Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        ))
    }
}

fn get_empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> EditError {
    if total_edits == 1 {
        EditError::new(format!("oldText must not be empty in {path}."))
    } else {
        EditError::new(format!(
            "edits[{edit_index}].oldText must not be empty in {path}."
        ))
    }
}

fn get_no_change_error(path: &str, total_edits: usize) -> EditError {
    if total_edits == 1 {
        EditError::new(format!(
            "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        ))
    } else {
        EditError::new(format!(
            "No changes made to {path}. The replacements produced identical content."
        ))
    }
}

/// 一处已定位的编辑（`MatchedEdit`）。
///
/// `edit_index` 是它在调用方 `edits` 数组里的原始下标，用于错误消息 `edits[i]`；
/// `replacement` 中的偏移是替换基底上的字节偏移。
#[derive(Debug, Clone, PartialEq, Eq)]
struct MatchedEdit {
    edit_index: usize,
    replacement: TextReplacement,
}

/// `applyEditsToNormalizedContent(normalizedContent, edits, path)`。
///
/// 所有 edit 都对**同一份**原文匹配；应用时按 `matchIndex` 逆序。只要任一 edit 用到
/// 模糊匹配，整个操作都在归一化空间里定位，再把触及的行覆盖回原文。
///
/// # 前提与返回值
///
/// `normalized_content` 必须已由调用方做 LF 归一化（本函数只归一化 edit 文本，不碰
/// 内容的行尾/BOM）。`path` 只用于拼错误消息，不做 IO。成功时返回
/// [`AppliedEditsResult`]，其中 `base_content` 就是 `normalized_content` 的副本，
/// `new_content` 是替换结果。
///
/// # 检查顺序
///
/// 1. 空 `oldText`（先对所有 edit 检查）；
/// 2. 按 edit 顺序逐个检查 not-found 与 duplicate；
/// 3. 全部定位成功后才检查 overlap；
/// 4. 最后检查新旧内容是否相同。
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEditsResult, EditError> {
    // 阶段 1：把 edit 文本也归一化成 LF。调用方可能传带 CRLF 的 oldText/newText，
    // 而匹配基底是 LF 的；不先统一，带 `\r\n` 的 oldText 无法精确命中，只能退化为
    // 模糊匹配（`fuzzy_find_text` 的第二阶段会经 `normalize_for_fuzzy_match` 归一化
    // 并命中，但会触发全局模糊基底）。这里只拒绝空串：纯空白 oldText 仍会进入查找
    // （它在模糊空间里可能归一化成空）。
    let normalized_edits: Vec<Edit> = edits
        .iter()
        .map(|edit| Edit {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect();

    for (index, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(get_empty_old_text_error(
                path,
                index,
                normalized_edits.len(),
            ));
        }
    }

    // 阶段 2：探测是否需要模糊匹配。这轮结果只用于决定 used_fuzzy_match：一旦全局
    // 切换基底，各 edit 的位置/长度都必须在新基底上重新计算，因此下一轮会再查一次，
    // 而不是复用这里的 index/match_length（它们属于另一个坐标系）。
    let initial_matches: Vec<FuzzyMatchResult> = normalized_edits
        .iter()
        .map(|edit| fuzzy_find_text(normalized_content, &edit.old_text))
        .collect();
    let used_fuzzy_match = initial_matches.iter().any(|result| result.used_fuzzy_match);
    let replacement_base_content = if used_fuzzy_match {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_string()
    };

    // 阶段 3：在选定的基底上逐个定位，并检查存在性与唯一性。occurrences 在归一化
    // 空间里统计（即使该 edit 精确命中），所以两个字节不同但归一化相同的出现也会
    // 被算作重复。
    let mut matched_edits: Vec<MatchedEdit> = Vec::with_capacity(normalized_edits.len());
    for (index, edit) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&replacement_base_content, &edit.old_text);
        if !match_result.found {
            return Err(get_not_found_error(path, index, normalized_edits.len()));
        }

        let occurrences = count_occurrences(&replacement_base_content, &edit.old_text);
        if occurrences > 1 {
            return Err(get_duplicate_error(
                path,
                index,
                normalized_edits.len(),
                occurrences,
            ));
        }

        matched_edits.push(MatchedEdit {
            edit_index: index,
            replacement: TextReplacement {
                match_index: match_result.index as usize,
                match_length: match_result.match_length,
                new_text: edit.new_text.clone(),
            },
        });
    }

    // 阶段 4：按位置排序后检查重叠。用严格大于判断，首尾相接是允许的；错误消息里
    // 的 edits[i] 用原始下标（edit_index），不是排序后的下标。
    matched_edits.sort_by_key(|edit| edit.replacement.match_index);
    for window in matched_edits.windows(2) {
        let previous = &window[0];
        let current = &window[1];
        if previous.replacement.match_index + previous.replacement.match_length
            > current.replacement.match_index
        {
            return Err(EditError::new(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index
            )));
        }
    }

    // 阶段 5：应用替换。位置是基底上的绝对字节偏移，逆序应用让靠前替换的偏移保持
    // 有效；模糊路径必须走 preserving 版本，把所有未触及的行按原文拼回，否则会把
    // 归一化后的文本整份写回，文件里未被编辑的行也会丢掉行尾空白。
    let base_content = normalized_content.to_string();
    let replacements: Vec<TextReplacement> = matched_edits
        .into_iter()
        .map(|matched| matched.replacement)
        .collect();
    let new_content = if used_fuzzy_match {
        apply_replacements_preserving_unchanged_lines(
            normalized_content,
            &replacement_base_content,
            &replacements,
        )?
    } else {
        apply_replacements(&replacement_base_content, &replacements, 0)
    };

    // 阶段 6：内容完全没变（包括 edits 为空）时报错，而不是返回一份“空 diff”。
    // 0 个 edit 走 total_edits == 1 之外的复数文案。
    if base_content == new_content {
        return Err(get_no_change_error(path, normalized_edits.len()));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
    })
}

// ===========================================================================
// diff@8 的 Myers 行 diff（逐行移植）
// ===========================================================================

/// `diffLines` 的一个 part。
///
/// `added`/`removed` 同时为 false 表示两边共有的一段；一次替换通常表现为相邻的
/// 一个 removed part 和一个 added part，先后顺序由 Myers 的选择决定。`value` 是该
/// part 在对应文件中的原始拼接（行 token 已含行尾，最后一行可能没有换行）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffPart {
    added: bool,
    removed: bool,
    value: String,
}

/// `diff/line.js#tokenize` + `base.js#removeEmpty`。
///
/// `value.split(/(\n|\r\n)/)` 合并回「内容 + 行尾」；单独的 `\r` 留在内容里。
/// 空输入得到空 `Vec`，末尾的换行不会产生额外空 token；所有空 token 都被丢弃。
fn tokenize_lines(value: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut start = 0usize;
    for (index, byte) in value.bytes().enumerate() {
        if byte == b'\n' {
            tokens.push(value[start..=index].to_string());
            start = index + 1;
        }
    }
    if start < value.len() {
        tokens.push(value[start..].to_string());
    }
    tokens.retain(|token| !token.is_empty());
    tokens
}

/// Myers 路径上的一个组件（`base.js` 的 `lastComponent` 链节点）。
///
/// 组件是“同一类连续编辑”的计数：`added`/`removed` 都为 false 表示公共行。
/// `previous` 是链上前驱在 `ComponentArena` 里的下标——用下标而不是引用，是因为多条
/// 搜索路径会共享同一条组件链（见 `SearchPath`），彼此不持有所有权。
#[derive(Debug, Clone, Copy)]
struct ComponentNode {
    count: usize,
    added: bool,
    removed: bool,
    previous: Option<usize>,
}

/// 一条搜索路径：`oldPos` 与组件链尾。
///
/// `old_pos` 是旧序列上已消费到的下标（0 起，`-1` 表示尚未消费任何 token）；
/// `last` 是组件链尾在 `ComponentArena` 中的下标，`None` 表示空路径。
#[derive(Debug, Clone, Copy)]
struct SearchPath {
    old_pos: i64,
    last: Option<usize>,
}

/// 组件 arena：`addToPath` 会创建新节点（旧节点可能被遗弃）。
///
/// 节点只增不删。`add_to_path` 与 `extract_common` 每次 push 一个新节点而不是原地
/// 修改链尾，因为同一批旧路径仍可能被 `best_path` 引用——原地改会破坏它们的组件链。
struct ComponentArena {
    nodes: Vec<ComponentNode>,
}

impl ComponentArena {
    fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    fn push(&mut self, node: ComponentNode) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    /// `addToPath(path, added, removed, oldPosInc)`。
    ///
    /// 若链尾组件的 `added`/`removed` 与本次相同，则新建一个 `count + 1` 的节点并
    /// 指回同一前驱，使连续同类编辑保持为一个组件；否则新建 `count = 1` 的节点，前驱
    /// 为原链尾。两条分支都返回新的 `SearchPath`，旧路径不变。
    fn add_to_path(
        &mut self,
        path: SearchPath,
        added: bool,
        removed: bool,
        old_pos_increment: i64,
    ) -> SearchPath {
        let old_pos = path.old_pos + old_pos_increment;
        if let Some(last_index) = path.last {
            let last = self.nodes[last_index];
            if last.added == added && last.removed == removed {
                let index = self.push(ComponentNode {
                    count: last.count + 1,
                    added,
                    removed,
                    previous: last.previous,
                });
                return SearchPath {
                    old_pos,
                    last: Some(index),
                };
            }
        }
        let index = self.push(ComponentNode {
            count: 1,
            added,
            removed,
            previous: path.last,
        });
        SearchPath {
            old_pos,
            last: Some(index),
        }
    }
}

/// `extractCommon(basePath, newTokens, oldTokens, diagonalPath)`。
///
/// 沿当前对角线向前消费两序列中相同的 token，返回推进后的路径与新的 `new_pos`。
/// `new_pos = old_pos - diagonalPath` 来自“对角线 = old_pos - new_pos”的定义；只有
/// 确实消费了 token 才追加一个公共组件。
fn extract_common(
    arena: &mut ComponentArena,
    mut path: SearchPath,
    new_tokens: &[String],
    old_tokens: &[String],
    diagonal_path: i64,
) -> (SearchPath, i64) {
    let new_len = new_tokens.len() as i64;
    let old_len = old_tokens.len() as i64;
    let mut old_pos = path.old_pos;
    let mut new_pos = old_pos - diagonal_path;
    let mut common_count: usize = 0;
    while new_pos + 1 < new_len
        && old_pos + 1 < old_len
        && old_tokens[(old_pos + 1) as usize] == new_tokens[(new_pos + 1) as usize]
    {
        new_pos += 1;
        old_pos += 1;
        common_count += 1;
    }
    if common_count > 0 {
        let index = arena.push(ComponentNode {
            count: common_count,
            added: false,
            removed: false,
            previous: path.last,
        });
        path.last = Some(index);
    }
    path.old_pos = old_pos;
    (path, new_pos)
}

/// `buildValues(lastComponent, newTokens, oldTokens)`。
///
/// 从链尾回溯并反转成编辑脚本顺序，再用两个游标把组件展开成 [`DiffPart`]：非
/// removed 组件从 `new_tokens` 取文本（公共组件同时推进旧游标），removed 组件从
/// `old_tokens` 取文本。`value` 是若干 token 的直接拼接，不再插入分隔符。
fn build_values(
    arena: &ComponentArena,
    last_component: Option<usize>,
    new_tokens: &[String],
    old_tokens: &[String],
) -> Vec<DiffPart> {
    let mut components = Vec::new();
    let mut current = last_component;
    while let Some(index) = current {
        let component = arena.nodes[index];
        components.push(component);
        current = component.previous;
    }
    components.reverse();

    let mut parts = Vec::with_capacity(components.len());
    let mut new_pos = 0usize;
    let mut old_pos = 0usize;
    for component in components {
        if !component.removed {
            let end = new_pos + component.count;
            let value = new_tokens[new_pos..end].concat();
            new_pos = end;
            if !component.added {
                old_pos += component.count;
            }
            parts.push(DiffPart {
                added: component.added,
                removed: false,
                value,
            });
        } else {
            let end = old_pos + component.count;
            let value = old_tokens[old_pos..end].concat();
            old_pos = end;
            parts.push(DiffPart {
                added: false,
                removed: true,
                value,
            });
        }
    }
    parts
}

/// `diffLines(oldStr, newStr)`（`ignoreWhitespace: false`）。
///
/// 逐行 Myers 搜索，与 jsdiff `base.js` 的流程一一对应：
///
/// 1. 用 `tokenize_lines` 切分两侧；
/// 2. `best_path` 以对角线号为键，保存该对角线上的最优路径。初始只有对角线 0，
///    先 `extract_common` 吃掉公共前缀；两侧都消费完则直接结束；
/// 3. `edit_length` 从 1 起递增，每轮只遍历与它同奇偶的对角线
///    （`-edit_length..=edit_length`，步长 2）。一条对角线可由“删除一条旧 token”
///    （对角线 -1）或“新增一条新 token”（对角线 +1）到达；两条候选都可用时选
///    `oldPos` 更大的那条（`remove.old_pos < add.old_pos` 时选 add），这个取舍决定
///    同一处替换中删除/新增的先后顺序；
/// 4. 候选先过 `can_add` / `can_remove` 的边界检查，再由 `extract_common` 沿对角线
///    吃掉公共 token；
/// 5. 一旦某条路径同时消费完两条序列，`build_values` 回溯组件链生成 parts；
/// 6. `min_diagonal_to_consider` / `max_diagonal_to_consider` 在某一侧耗尽后收窄
///    扫描范围，不再遍历不可能成功的对角线。
fn diff_lines(old_content: &str, new_content: &str) -> Vec<DiffPart> {
    let old_tokens = tokenize_lines(old_content);
    let new_tokens = tokenize_lines(new_content);
    let old_len = old_tokens.len() as i64;
    let new_len = new_tokens.len() as i64;
    let mut arena = ComponentArena::new();

    // 空路径从 old_pos = -1 开始（尚未消费任何 token）；先沿对角线 0 吃掉公共前缀，
    // 两侧都已耗尽时不需要进入 Myers 循环。
    let mut best_path: std::collections::HashMap<i64, SearchPath> =
        std::collections::HashMap::new();
    let initial = SearchPath {
        old_pos: -1,
        last: None,
    };
    let (initial_path, mut new_pos) =
        extract_common(&mut arena, initial, &new_tokens, &old_tokens, 0);
    best_path.insert(0, initial_path);
    if best_path[&0].old_pos + 1 >= old_len && new_pos + 1 >= new_len {
        return build_values(&arena, best_path[&0].last, &new_tokens, &old_tokens);
    }

    // 每轮 edit_length 对应“编辑距离”。对角线范围是 [-edit_length, edit_length]
    // 的偶数步进：距离每增加 1 只能多一次新增或删除，所以可达对角线的奇偶性固定。
    let mut edit_length: i64 = 1;
    let mut min_diagonal_to_consider = i64::MIN;
    let mut max_diagonal_to_consider = i64::MAX;

    loop {
        let mut diagonal_path = min_diagonal_to_consider.max(-edit_length);
        let upper = max_diagonal_to_consider.min(edit_length);
        while diagonal_path <= upper {
            // 先取两条候选（它们可能共享组件链），再把 diagonal-1 从 map 移除；
            // SearchPath 是 Copy，取值不会与后续的 remove 冲突。
            let remove_path = best_path.get(&(diagonal_path - 1)).copied();
            let add_path = best_path.get(&(diagonal_path + 1)).copied();
            if remove_path.is_some() {
                best_path.remove(&(diagonal_path - 1));
            }

            // can_add / can_remove 把越界候选挡在索引之外：新增后的新位置必须落在
            // [0, new_len)，删除候选还必须剩有旧 token。
            let mut can_add = false;
            if let Some(add) = add_path {
                let add_path_new_pos = add.old_pos - diagonal_path;
                can_add = add_path_new_pos >= 0 && add_path_new_pos < new_len;
            }
            let can_remove = remove_path.is_some_and(|path| path.old_pos + 1 < old_len);

            if !can_add && !can_remove {
                best_path.remove(&diagonal_path);
                diagonal_path += 2;
                continue;
            }

            // 两条候选都可用时选 old_pos 更大（走得更远）的那条；这个取舍决定同一处
            // 替换里删除与新增的先后顺序，必须与 jsdiff 保持一致。
            let base_path = if !can_remove
                || (can_add
                    && remove_path
                        .expect("can_remove false implies add path")
                        .old_pos
                        < add_path.expect("can_add").old_pos)
            {
                arena.add_to_path(add_path.expect("can_add or can_remove"), true, false, 0)
            } else {
                arena.add_to_path(remove_path.expect("can_remove"), false, true, 1)
            };

            let (base_path, base_new_pos) = extract_common(
                &mut arena,
                base_path,
                &new_tokens,
                &old_tokens,
                diagonal_path,
            );
            new_pos = base_new_pos;

            // 两侧都消费完：回溯组件链得到最终 parts。
            if base_path.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                return build_values(&arena, base_path.last, &new_tokens, &old_tokens);
            }

            // 记录本对角线的路径。某一侧已耗尽时收窄后续扫描的对角线范围，
            // 对应 jsdiff 的 minDiagonalToConsider / maxDiagonalToConsider。
            best_path.insert(diagonal_path, base_path);
            if base_path.old_pos + 1 >= old_len {
                max_diagonal_to_consider = max_diagonal_to_consider.min(diagonal_path - 1);
            }
            if new_pos + 1 >= new_len {
                min_diagonal_to_consider = min_diagonal_to_consider.max(diagonal_path + 1);
            }
            diagonal_path += 2;
        }
        edit_length += 1;
    }
}

// ===========================================================================
// generateDiffString
// ===========================================================================

/// `generateDiffString` 的返回值。TS: `EditDiffResult`。
///
/// 序列化形状是 `{"diff": "...", "firstChangedLine"?: N}`（`camelCase`，缺省字段被
/// 跳过）：TUI 预览状态（`renderers/edit.rs`）与 `sdk_exports.rs` 的导出都依赖它。
/// `diff` 为空串表示两份内容没有差异。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditDiffResult {
    /// 带行号的展示 diff。
    pub diff: String,
    /// 新文件里第一处变更的行号（1 起）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<u64>,
}

impl EditDiffResult {
    /// 便捷构造。
    pub fn new(diff: String, first_changed_line: Option<u64>) -> Self {
        Self {
            diff,
            first_changed_line,
        }
    }
}

/// `EditDiffResult | EditDiffError` 的错误分支。
///
/// 序列化为 `{"error": "..."}`；`renderers/edit.rs` 用 untagged 枚举把本类型与
/// [`EditDiffResult`] 区分开，所以字段名与形状属于渲染层接口。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditDiffError {
    /// 错误消息。
    pub error: String,
}

/// JS 的 `String(number).padStart(width, " ")`（数字按 ECMAScript 规则格式化）。
///
/// 行号先转 f64 再交给 `kiana_json::js_num::format_js_number`，复刻 ECMAScript 的
/// `String(number)`；格式化结果是 ASCII，所以 `len()` 就是显示宽度。宽度不足时左侧
/// 补空格，超过时不截断。
fn pad_line_number(value: f64, width: usize) -> String {
    let text = kiana_json::js_num::format_js_number(value);
    if text.len() >= width {
        text
    } else {
        format!("{}{text}", " ".repeat(width - text.len()))
    }
}

/// `generateDiffString(oldContent, newContent, contextLines = 4)`。
///
/// 返回带行号的展示 diff，以及**新文件**里第一处变更的行号（在写第一行变更之前捕获）。
/// 跳过行是 `" " + " ".repeat(width) + " ..."`。
///
/// 行号从 1 起，宽度取旧/新两边行数最大值（按 `'\n'` 切分的段数）的十进制位数。
/// 内容完全相同时 `diff` 为空串、`first_changed_line` 为 `None`；首个变更 part 是
/// 纯删除时，`first_changed_line` 仍是该删除位置在新文件中的行号。
pub fn generate_diff_string(old_content: &str, new_content: &str) -> EditDiffResult {
    generate_diff_string_with_context(old_content, new_content, 4)
}

/// [`generate_diff_string`] 的显式上下文行数版本。
///
/// 一个公共 part 保留多少行取决于它两侧：前后都有变更时保留前后各至多
/// `context_lines` 行（总长不超过 `2 * context_lines` 时全保留，否则中间插一行
/// `...`）；只有一侧有变更时保留该侧的至多 `context_lines` 行；两侧都没有变更
/// （整份内容无差异）时不输出任何行。行号游标始终按被跳过的行数前进。
pub fn generate_diff_string_with_context(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> EditDiffResult {
    let parts = diff_lines(old_content, new_content);
    let mut output: Vec<String> = Vec::new();

    let old_lines: Vec<&str> = old_content.split('\n').collect();
    let new_lines: Vec<&str> = new_content.split('\n').collect();
    let max_line_num = old_lines.len().max(new_lines.len());
    let line_num_width = max_line_num.to_string().len();

    let mut old_line_num = 1.0f64;
    let mut new_line_num = 1.0f64;
    let mut last_was_change = false;
    let mut first_changed_line: Option<u64> = None;

    for (index, part) in parts.iter().enumerate() {
        // part.value 的每个行 token 都带行尾（文件最后一行可能没有），split 后末尾
        // 的空串来自最后的那个换行，去掉它避免把行数多算一行。
        let mut raw: Vec<&str> = part.value.split('\n').collect();
        if raw.last() == Some(&"") {
            raw.pop();
        }

        if part.added || part.removed {
            // 第一处变更：在消费该 part 的任何行之前记录新文件行号。纯删除 part 也
            // 走这里，因此记录的是删除发生处在新文件中的行号。
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_num as u64);
            }

            for line in raw {
                if part.added {
                    output.push(format!(
                        "+{} {}",
                        pad_line_number(new_line_num, line_num_width),
                        line
                    ));
                    new_line_num += 1.0;
                } else {
                    output.push(format!(
                        "-{} {}",
                        pad_line_number(old_line_num, line_num_width),
                        line
                    ));
                    old_line_num += 1.0;
                }
            }
            last_was_change = true;
        } else {
            // 公共段分四种：两侧都有变更（折叠中间）、只有前面有变更（折叠尾部）、
            // 只有后面有变更（折叠头部）、都没有（整份无差异，不输出任何行，只推进
            // 行号游标）。
            let next_part_is_change =
                index < parts.len() - 1 && (parts[index + 1].added || parts[index + 1].removed);
            let has_leading_change = last_was_change;
            let has_trailing_change = next_part_is_change;

            if has_leading_change && has_trailing_change {
                if raw.len() <= context_lines * 2 {
                    for line in raw {
                        output.push(format!(
                            " {} {}",
                            pad_line_number(old_line_num, line_num_width),
                            line
                        ));
                        old_line_num += 1.0;
                        new_line_num += 1.0;
                    }
                } else {
                    let leading_lines = &raw[..context_lines];
                    let trailing_lines = &raw[raw.len() - context_lines..];
                    let skipped_lines = raw.len() - leading_lines.len() - trailing_lines.len();

                    for line in leading_lines {
                        output.push(format!(
                            " {} {}",
                            pad_line_number(old_line_num, line_num_width),
                            line
                        ));
                        old_line_num += 1.0;
                        new_line_num += 1.0;
                    }

                    output.push(format!(" {} ...", " ".repeat(line_num_width)));
                    old_line_num += skipped_lines as f64;
                    new_line_num += skipped_lines as f64;

                    for line in trailing_lines {
                        output.push(format!(
                            " {} {}",
                            pad_line_number(old_line_num, line_num_width),
                            line
                        ));
                        old_line_num += 1.0;
                        new_line_num += 1.0;
                    }
                }
            } else if has_leading_change {
                let shown_lines = &raw[..raw.len().min(context_lines)];
                let skipped_lines = raw.len() - shown_lines.len();

                for line in shown_lines {
                    output.push(format!(
                        " {} {}",
                        pad_line_number(old_line_num, line_num_width),
                        line
                    ));
                    old_line_num += 1.0;
                    new_line_num += 1.0;
                }

                if skipped_lines > 0 {
                    output.push(format!(" {} ...", " ".repeat(line_num_width)));
                    old_line_num += skipped_lines as f64;
                    new_line_num += skipped_lines as f64;
                }
            } else if has_trailing_change {
                let skipped_lines = raw.len().saturating_sub(context_lines);
                if skipped_lines > 0 {
                    output.push(format!(" {} ...", " ".repeat(line_num_width)));
                    old_line_num += skipped_lines as f64;
                    new_line_num += skipped_lines as f64;
                }

                for line in &raw[skipped_lines..] {
                    output.push(format!(
                        " {} {}",
                        pad_line_number(old_line_num, line_num_width),
                        line
                    ));
                    old_line_num += 1.0;
                    new_line_num += 1.0;
                }
            } else {
                old_line_num += raw.len() as f64;
                new_line_num += raw.len() as f64;
            }

            last_was_change = false;
        }
    }

    EditDiffResult {
        diff: output.join("\n"),
        first_changed_line,
    }
}

// ===========================================================================
// generateUnifiedPatch（structuredPatch + formatPatch(FILE_HEADERS_ONLY)）
// ===========================================================================

/// `splitLines(text)`（`patch/create.js` 内部）：保留行尾换行；无尾随换行的最后一行例外。
///
/// 与 [`split_lines_with_endings`] 的区别：这里返回拥有所有权的 `String`，并保证
/// 最后一行在原文没有尾随换行时也不带 `\n`。输入为空串时返回一个空行（`[""]`）；
/// 本模块的调用点只会传入非空 part，因此这条边界不参与实际输出。
fn split_patch_lines(text: &str) -> Vec<String> {
    let has_trailing_newline = text.ends_with('\n');
    let mut result: Vec<String> = text.split('\n').map(|line| format!("{line}\n")).collect();
    if has_trailing_newline {
        result.pop();
    } else {
        let last = result.pop().unwrap_or_default();
        result.push(last[..last.len().saturating_sub(1)].to_string());
    }
    result
}

/// 一个 hunk（`structuredPatch` 的中间形态）。
///
/// 行号都是 1 起：`old_start`/`new_start` 已包含前导上下文（hunk 打开时回退过），
/// `old_lines`/`new_lines` 是 hunk 覆盖的总行数（含上下文），`lines` 的每项带
/// `+`/`-`/空格前缀，并且可能带行尾 `\n`（最后一步才剥离）。
struct Hunk {
    old_start: i64,
    old_lines: usize,
    new_start: i64,
    new_lines: usize,
    lines: Vec<String>,
}

/// `structuredPatch` + `formatPatch(..., FILE_HEADERS_ONLY)`。
///
/// 无 hunk 时只输出 `--- path\n+++ path\n`（不是空串）；结果**总是**以 `\n` 结尾。
///
/// # 流程
///
/// 1. 在 `diff_lines` 结果末尾追加一个空 part 作哨兵，让最后一个打开的 hunk 在循环
///    里走“关闭”分支；
/// 2. 扫描 parts 维护新旧行游标：遇到变更 part 时打开 hunk，并把前一个公共 part
///    最多 `context` 行提升为前导上下文（hunk 起始行号随之前移）；之后的公共 part
///    若不超过 `2 * context` 行且后面还有真实 part 就并入 hunk，否则截取前
///    `context` 行并关闭 hunk；
/// 3. 逐 hunk 去掉行尾 `\n`；某行没有 `\n`（文件末尾）时在其后插入
///    `\ No newline at end of file`；
/// 4. 写 `--- path` / `+++ path`，再加 `@@ -a,b +c,d @@`（b/d 为 0 时起始行号减一，
///    这是 unified diff 的约定）。
///
/// 两个文件头使用同一个 `path`，不带时间戳；即使内容相同也输出这两行。
fn format_unified_patch(
    path: &str,
    old_content: &str,
    new_content: &str,
    context: usize,
) -> String {
    let mut parts = diff_lines(old_content, new_content);
    // 追加哨兵，便于收尾（TS: `diff.push({ value: '', lines: [] })`）。
    parts.push(DiffPart {
        added: false,
        removed: false,
        value: String::new(),
    });

    let mut hunks: Vec<Hunk> = Vec::new();
    let mut old_range_start: Option<i64> = None;
    let mut new_range_start: i64 = 0;
    let mut current_range: Vec<String> = Vec::new();
    // 旧/新行号都是 1 起、指向“下一个待消费的行”。打开 hunk 时会从上一个公共 part
    // 借最多 context 行作前导上下文，因此起始行号要回退相应行数。
    let mut old_line: i64 = 1;
    let mut new_line: i64 = 1;

    for index in 0..parts.len() {
        let part = &parts[index];
        // 哨兵的 lines 是空数组；其它 part 用 `splitLines(value)`。
        let lines: Vec<String> = if part.value.is_empty() && index == parts.len() - 1 {
            Vec::new()
        } else {
            split_patch_lines(&part.value)
        };

        if part.added || part.removed {
            if old_range_start.is_none() {
                let previous = if index > 0 {
                    Some(&parts[index - 1])
                } else {
                    None
                };
                old_range_start = Some(old_line);
                new_range_start = new_line;
                if let Some(previous) = previous {
                    let previous_lines: Vec<String> =
                        if previous.value.is_empty() && index - 1 == parts.len() - 1 {
                            Vec::new()
                        } else {
                            split_patch_lines(&previous.value)
                        };
                    current_range = if context > 0 {
                        let take_start = previous_lines.len().saturating_sub(context);
                        previous_lines[take_start..]
                            .iter()
                            .map(|line| format!(" {line}"))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let start = old_range_start.expect("just set");
                    old_range_start = Some(start - current_range.len() as i64);
                    new_range_start -= current_range.len() as i64;
                }
            }

            for line in &lines {
                current_range.push(format!("{}{line}", if part.added { "+" } else { "-" }));
            }

            if part.added {
                new_line += lines.len() as i64;
            } else {
                old_line += lines.len() as i64;
            }
        } else if let Some(range_start) = old_range_start {
            // 关闭或合并 hunk。
            // 公共段很短（<= 2*context）且后面还有真实 part 时继续并入当前 hunk；
            // 位于哨兵之前的最后一段必须走关闭分支，否则会把文件尾部所有行都塞进
            // 尾随上下文（jsdiff 只保留 context 行）。
            if lines.len() <= context * 2 && index < parts.len() - 2 {
                for line in &lines {
                    current_range.push(format!(" {line}"));
                }
            } else {
                // 关闭 hunk：只保留前 context 行作尾随上下文；行数 = 已消费行数 +
                // 尾随上下文（起始行号此前已因前导上下文回退过）。
                let context_size = lines.len().min(context);
                for line in &lines[..context_size] {
                    current_range.push(format!(" {line}"));
                }
                hunks.push(Hunk {
                    old_start: range_start,
                    old_lines: (old_line - range_start + context_size as i64) as usize,
                    new_start: new_range_start,
                    new_lines: (new_line - new_range_start + context_size as i64) as usize,
                    lines: std::mem::take(&mut current_range),
                });
                old_range_start = None;
                new_range_start = 0;
            }
            old_line += lines.len() as i64;
            new_line += lines.len() as i64;
        } else {
            old_line += lines.len() as i64;
            new_line += lines.len() as i64;
        }
    }

    // Step 2：去掉行尾 `\n`，必要时插入 `\ No newline at end of file`。
    // 行 token 通常以 `\n` 结尾；没有 `\n` 的那一行就是文件最后一行，标记行插在它
    // 后面且自身不参与后续 strip。
    for hunk in &mut hunks {
        let mut index = 0usize;
        while index < hunk.lines.len() {
            if let Some(stripped) = hunk.lines[index].strip_suffix('\n') {
                hunk.lines[index] = stripped.to_string();
            } else {
                hunk.lines
                    .insert(index + 1, "\\ No newline at end of file".to_string());
                index += 1;
            }
            index += 1;
        }
    }

    // FILE_HEADERS_ONLY：两个文件头都用 path 本身，无时间戳、无 `Index:` 行、
    // 无 `=` 分隔线，hunk 之间也不插入额外空行。
    let mut output: Vec<String> = Vec::new();
    output.push(format!("--- {path}"));
    output.push(format!("+++ {path}"));
    for hunk in &mut hunks {
        let mut old_start = hunk.old_start;
        let mut new_start = hunk.new_start;
        // Unified diff quirk：chunk size 为 0 时起始行号减一，表示“在第 start 行
        // 之后”。例如向空文件插入内容得到 `@@ -0,0 +1,1 @@`（黄金测试锁定）。
        if hunk.old_lines == 0 {
            old_start -= 1;
        }
        if hunk.new_lines == 0 {
            new_start -= 1;
        }
        output.push(format!(
            "@@ -{old_start},{} +{new_start},{} @@",
            hunk.old_lines, hunk.new_lines
        ));
        output.extend(hunk.lines.iter().cloned());
    }
    let mut text = output.join("\n");
    text.push('\n');
    text
}

/// `generateUnifiedPatch(path, oldContent, newContent, contextLines = 4)`。
///
/// 输出格式以 TS 参考实现为准（见模块文档）：无 `Index:` 行、无 `=` 分隔线、
/// `@@ -a,b +c,d @@` 始终带长度、始终以 `\n` 结尾、内容相同只输出两行文件头。
///
/// 纯函数：不读文件，`path` 原样写进 `---`/`+++` 头；需要预览时由调用方读入内容再
/// 传入。上下文行数固定为 4，显式控制请用
/// [`generate_unified_patch_with_context`]。
pub fn generate_unified_patch(path: &str, old_content: &str, new_content: &str) -> String {
    format_unified_patch(path, old_content, new_content, 4)
}

/// [`generate_unified_patch`] 的显式上下文行数版本。
///
/// `context_lines = 0` 时 hunk 只含变更行（`@@` 头仍带长度）；其余格式规则与默认
/// 版本完全相同。
pub fn generate_unified_patch_with_context(
    path: &str,
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> String {
    format_unified_patch(path, old_content, new_content, context_lines)
}

// ===========================================================================
// computeEditsDiff / computeEditDiff
// ===========================================================================

/// 读取失败时构造 `Could not edit file: {path}. {Error code: X | message}.`。
///
/// 有 Node 错误码时只显示 `Error code: X`（丢弃 OS 描述）；没有 code 时显示
/// `Error: {message}`。这与 `edit.rs` 的 `format_edit_access_error` 文案一致——
/// 执行路径与预览路径必须给出相同的错误文本。
fn access_error_message(path: &str, error: &crate::read::ToolOperationError) -> String {
    let detail = match &error.code {
        Some(code) => format!("Error code: {code}"),
        None => format!("Error: {}", error.message),
    };
    format!("Could not edit file: {path}. {detail}.")
}

/// `computeEditsDiff(path, edits, cwd)`：只计算预览 diff，不写文件。
///
/// 失败时返回 [`EditDiffError`]（`{error: message}`），与 TS 的
/// `EditDiffResult | EditDiffError` 联合类型对应。
///
/// # 流程
///
/// 1. `resolve_to_cwd` 得到用于 IO 的路径；
/// 2. `access(2)` 预检可读性——失败消息使用**用户传入的 `path`**；
/// 3. 读文件；读阶段的错误（包括目录的 `EISDIR`）原样返回，消息里是解析后的路径；
/// 4. WHATWG UTF-8 解码 → `split_bom` → [`normalize_to_lf`]；
/// 5. [`apply_edits_to_normalized_content`] 定位并应用，失败文本使用传入的 `path`；
/// 6. 成功后只生成展示 diff，不生成 unified patch、不写盘。
///
/// `access` 与 `read` 是两次独立调用：两者之间文件被删除或权限变化时，以 `read`
/// 的结果为准（错误同样原样返回）。
pub async fn compute_edits_diff(
    path: &str,
    edits: &[Edit],
    cwd: &str,
) -> Result<EditDiffResult, EditDiffError> {
    // 解析后的路径只用于 IO；所有面向用户的错误消息仍使用调用方传入的 `path`，
    // 与 edit.ts 的 `computeEditsDiff` 一致（read 阶段抛出的原样错误除外）。
    let absolute_path = resolve_to_cwd(path, cwd);

    if let Err(error) = check_readable_file(&absolute_path) {
        return Err(EditDiffError {
            error: access_error_message(path, &error),
        });
    }

    // `readFile` 的错误原样返回（TS 的外层 catch）：目录是
    // `EISDIR: illegal operation on a directory, read '<abs path>'`。
    let bytes = match read_file_bytes(&absolute_path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(EditDiffError {
                error: error.message,
            });
        }
    };
    let raw_content = decode_utf8(&bytes);

    // BOM 必须在匹配前剥离：它不属于任何 oldText，也不该参与 diff；写回时由
    // `edit.rs` 单独补回。匹配基底统一为 LF，edit 文本的归一化在下一层完成。
    let split = split_bom(&raw_content);
    let normalized_content = normalize_to_lf(&split.text);
    match apply_edits_to_normalized_content(&normalized_content, edits, path) {
        Ok(applied) => Ok(generate_diff_string(
            &applied.base_content,
            &applied.new_content,
        )),
        Err(error) => Err(EditDiffError {
            error: error.message,
        }),
    }
}

/// `access(path, R_OK)`：预检可读性（真正的 access(2)，目录只检查权限位）。
///
/// 目录能通过这一步，真正的 `EISDIR` 要等到随后的 `read_file_bytes` 才出现——这与
/// Node 的 `fs.access` + `fs.readFile` 行为一致（见 `compute_edits_diff` 的注释）。
fn check_readable_file(absolute_path: &Path) -> Result<(), crate::read::ToolOperationError> {
    access_path(absolute_path, R_OK)
}

/// `buffer.toString("utf-8")` / `readFile(path, "utf-8")` 的 WHATWG 解码。
///
/// - 非法 UTF-8 字节按 WHATWG 替换算法变成 U+FFFD（第二个返回值表示是否发生过替换，
///   这里被丢弃）；
/// - **不**处理 BOM（`decode_without_bom_handling`），开头的 U+FEFF 会保留，调用方
///   必须再调用 `split_bom` 去除（`edit.rs` 与 `compute_edits_diff` 都这样做）。
pub(crate) fn decode_utf8(bytes: &[u8]) -> String {
    let (decoded, _) = encoding_rs::UTF_8.decode_without_bom_handling(bytes);
    decoded.into_owned()
}

/// `computeEditDiff(path, oldText, newText, cwd)`：单编辑便捷包装。
///
/// 把一对旧的/新文本包装成单元素 edit 列表后转调 [`compute_edits_diff`]；空
/// `old_text`、找不到、出现多次等校验都在下层完成。
pub async fn compute_edit_diff(
    path: &str,
    old_text: &str,
    new_text: &str,
    cwd: &str,
) -> Result<EditDiffResult, EditDiffError> {
    let edits = [Edit {
        old_text: old_text.to_string(),
        new_text: new_text.to_string(),
    }];
    compute_edits_diff(path, &edits, cwd).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings_follow_the_first_newline() {
        assert_eq!(detect_line_ending("a\r\nb"), LineEnding::Crlf);
        assert_eq!(detect_line_ending("a\nb\r\n"), LineEnding::Lf);
        assert_eq!(detect_line_ending("no newline"), LineEnding::Lf);
        assert_eq!(detect_line_ending(""), LineEnding::Lf);
        assert_eq!(detect_line_ending("a\rb\n"), LineEnding::Lf);
    }

    #[test]
    fn normalize_and_restore_line_endings_round_trip() {
        assert_eq!(normalize_to_lf("a\r\nb\rc\nd"), "a\nb\nc\nd");
        assert_eq!(restore_line_endings("a\nb", LineEnding::Crlf), "a\r\nb");
        assert_eq!(restore_line_endings("a\nb", LineEnding::Lf), "a\nb");
    }

    #[test]
    fn split_lines_with_endings_matches_the_regex() {
        assert_eq!(split_lines_with_endings("a\nb"), vec!["a\n", "b"]);
        assert_eq!(split_lines_with_endings("a\n"), vec!["a\n"]);
        assert_eq!(split_lines_with_endings(""), Vec::<&str>::new());
        assert_eq!(split_lines_with_endings("a\n\n"), vec!["a\n", "\n"]);
        assert_eq!(split_lines_with_endings("\n"), vec!["\n"]);
        assert_eq!(split_lines_with_endings("a\r\nb"), vec!["a\r\n", "b"]);
    }

    #[test]
    fn trim_end_uses_the_js_whitespace_set() {
        // U+FEFF 是 JS 的 WhiteSpace，Rust 的 trim_end 不会去掉它。
        assert_eq!(normalize_for_fuzzy_match("a\u{feff}"), "a");
        // U+0085 是 Rust 的 White_Space，但 JS trimEnd 不认。
        assert_eq!(normalize_for_fuzzy_match("a\u{0085}"), "a\u{0085}");
        assert_eq!(normalize_for_fuzzy_match("a \t\nb  "), "a\nb");
    }

    /// 空白 oldText 归一化后为空串：JS `split("").length - 1` 是 code unit 数 - 1。
    #[test]
    fn empty_normalized_needle_uses_split_semantics() {
        let edit = |old: &str, new: &str| Edit {
            old_text: old.to_string(),
            new_text: new.to_string(),
        };
        let applied = apply_edits_to_normalized_content("a", &[edit(" ", "X")], "f")
            .expect("one code unit means zero occurrences");
        assert_eq!(applied.new_content, "Xa");

        let error = apply_edits_to_normalized_content("a b", &[edit(" ", "X")], "f")
            .expect_err("two code units");
        assert_eq!(
            error.message,
            "Found 2 occurrences of the text in f. The text must be unique. Please provide more context to make it unique."
        );

        let error = apply_edits_to_normalized_content("😀😀", &[edit(" ", "X")], "f")
            .expect_err("four UTF-16 code units");
        assert!(
            error
                .message
                .starts_with("Found 3 occurrences of the text in f.")
        );
    }

    #[test]
    fn zero_edits_is_reported_as_no_change() {
        let error = apply_edits_to_normalized_content("abc", &[], "f").expect_err("no edits");
        assert_eq!(
            error.message,
            "No changes made to f. The replacements produced identical content."
        );
    }
}
