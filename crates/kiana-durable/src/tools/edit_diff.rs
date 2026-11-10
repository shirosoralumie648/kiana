//! `tools/edit-diff.ts`：模糊匹配、替换应用与 **jsdiff 8.0.4 的逐字移植**。
//!
//! TS 依赖 `diff` 包的 `createTwoFilesPatch` 与 `diffLines`；`similar` 的分组与 hunk 范围
//! 不兼容，因此这里移植 jsdiff 的 Myers 实现（`libesm/diff/base.js`）与
//! `structuredPatch`/`formatPatch`（`libesm/patch/create.js`），并保持
//! `\ No newline at end of file`、`@@ -a,b +c,d @@` 的字节级输出。
//!
//! # 这个模块解决什么问题
//!
//! `edit` 工具让模型只提供 `oldText`/`newText`，由工具负责在文件里定位、替换并回报
//! diff。本模块承担这条链路里的全部纯字符串运算：
//!
//! - 行尾探测与归一化：[`detect_line_ending`]、[`normalize_to_lf`]、
//!   [`restore_line_endings`]、[`strip_bom`]；
//! - 模糊查找与替换应用：[`fuzzy_find_text`]、
//!   [`apply_edits_to_normalized_content`]；
//! - 两种输出：[`generate_diff_string`] 生成给人看的带行号 diff，
//!   [`generate_unified_patch`] 生成 unified patch，二者都建立在 [`diff_lines`] 上。
//!
//! 本模块不读文件、不写文件、不接触 `ExecutionEnv`，也不持有任何全局状态；副作用
//! （读、写、取消检查、变更队列）全部在调用方 `edit.rs` 里完成。因此这里的函数可以
//! 直接用字符串做单元测试。
//!
//! # 在系统中的位置
//!
//! 主要上游是 [`crate::tools::edit`] 的 `execute_edit`：它先 `strip_bom` /
//! `detect_line_ending` / `normalize_to_lf`，把 LF 归一化后的内容交给
//! [`apply_edits_to_normalized_content`]，再把返回的 `new_content` 还原行尾与 BOM 后
//! 写回文件；两个 diff 函数只用于填充工具结果的 `details`。`examples/16_coding_tools.rs`
//! 与 `tests/tools.rs` 也直接调用这里的函数，并有 TS 参考实现生成的夹具做逐字比对。
//!
//! # 一次 edit 的执行流程
//!
//! 1. 每个 edit 的 `oldText`/`newText` 各自做一次 `normalize_to_lf`（调用方可能传入带
//!    CRLF 的文本）；`oldText` 为空立即报错。
//! 2. 先做一轮探测：只要任意一个 `oldText` 需要模糊匹配，整个替换基底就切换为
//!    `normalize_for_fuzzy_match(content)`。这是全局开关，不是逐 edit 决定；一旦触发，
//!    其它 edit 也会在这个归一化基底上重新定位。
//! 3. 在选定基底上逐个定位、检查唯一性，再按位置排序并检查重叠。
//! 4. 应用替换：精确路径直接逆序拼接；模糊路径只重建被替换触及的整行，未触碰的行
//!    逐字节取自 LF 归一化后的原文。
//! 5. 返回 LF 形态的 `base_content`/`new_content`；调用方负责还原行尾与 BOM，并生成
//!    展示用 diff 与 patch。
//!
//! # 重要约束
//!
//! - **偏移单位**：本文件的 `match_index`、`match_length` 与行 span 都是 UTF-8 字节
//!   偏移，而 JS 字符串索引是 UTF-16 码元。它们只在内部使用、不进入任何输出；唯一
//!   需要按 UTF-16 计数的是 `count_occurrences` 里空 `oldText` 的分支，那里显式用
//!   `encode_utf16` 复刻 JS 的 `split`。
//! - **错误文案是模型可见输出**：`get_*_error` 的字符串是当前稳定契约（改动需同步测试），
//!   单个 edit 与多个 edit 的措辞不同；`tests/tools.rs` 与 `tests/fixtures/tools_diff_cases.json` 会
//!   断言原文。
//! - **输出字节被夹具锁定**：patch/diff 的换行、`@@` 头、`\ No newline at end of file`
//!   都由 TS 参考实现生成的夹具逐字节比对，不能按“更标准的 unified diff”去改。
//! - **纯函数**：函数之间不共享可变状态，输入相同则输出相同。
//!
//! # 阅读顺序
//!
//! 先读行尾与归一化部分，再看 [`apply_edits_to_normalized_content`] 掌握编辑主流程；
//! [`diff_lines`] 的 Myers 实现与 [`generate_diff_string`] 可以放最后——它们只消费
//! 文本、不参与文件修改。

use std::collections::HashMap;

use unicode_normalization::UnicodeNormalization;

use crate::types::BoxError;

// ---------------------------------------------------------------------------
// 行尾与归一化
// ---------------------------------------------------------------------------

/// `"\r\n" | "\n"`。
///
/// 表示整份文件的“主导”行尾风格：[`detect_line_ending`] 只返回这两种之一，
/// [`restore_line_endings`] 用它在写回前还原。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineEnding {
    /// CRLF。
    Crlf,
    /// LF。
    Lf,
}

/// `detectLineEnding(content)`。
///
/// 比较第一个 `\n` 与第一个 `\r\n` 的起点：`\r\n` 更靠前就返回
/// [`LineEnding::Crlf`]，否则返回 [`LineEnding::Lf`]；完全没有换行时按 LF 处理。
///
/// 因此混合行尾的文件由**第一处换行**决定整体风格，后面的行不参与判断。
pub fn detect_line_ending(content: &str) -> LineEnding {
    let crlf_index = content.find("\r\n");
    let lf_index = content.find('\n');
    match (lf_index, crlf_index) {
        (None, _) | (Some(_), None) => LineEnding::Lf,
        (Some(lf), Some(crlf)) => {
            if crlf < lf {
                LineEnding::Crlf
            } else {
                LineEnding::Lf
            }
        }
    }
}

/// `normalizeToLF(text)`：`\r\n` → `\n`，再 `\r` → `\n`。
///
/// 两趟替换意味着单独的 `\r`（旧 Mac 风格）也会变成 `\n`。匹配、应用替换与比较内容
/// 之前都应先经过这一步，保证所有偏移都在同一套 LF 文本上计算。
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// `restoreLineEndings(text, ending)`。
///
/// 只应在**已经归一化为 LF** 的文本上调用：CRLF 模式会把每个 `\n` 换成 `\r\n`，
/// 若传入的文本还残留 `\r\n`，会得到 `\r\r\n`。
pub fn restore_line_endings(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Crlf => text.replace('\n', "\r\n"),
        LineEnding::Lf => text.to_owned(),
    }
}

/// `normalizeForFuzzyMatch(text)`：NFKC、逐行去尾空白、智能引号/破折号/特殊空格转 ASCII。
///
/// 顺序固定：先 NFKC（把全角/兼容字符折叠成常规形式），再逐行去掉行尾空白，
/// 最后做标点与空格替换。NFKC 会改变字符序列，换序后 trim 的输入就不同了，因此
/// 这三步的顺序不能调整。
///
/// 这个函数不增删换行，因此归一化前后的行数相同；但同一行的字节长度可能变化。
/// 模糊匹配得到的偏移只能用于归一化后的文本，不能拿去索引原文；需要在原文上保留
/// 未触及行时用 [`apply_replacements_preserving_unchanged_lines`]。
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let normalized: String = text.nfkc().collect();
    let trimmed = normalized
        .split('\n')
        .map(trim_end_js)
        .collect::<Vec<_>>()
        .join("\n");
    let mut output = String::with_capacity(trimmed.len());
    for character in trimmed.chars() {
        match character {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => output.push('\''),
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => output.push('"'),
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => output.push('-'),
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => {
                output.push(' ')
            }
            _ => output.push(character),
        }
    }
    output
}

/// JS `trimEnd()` 去掉的字符集：WhiteSpace + LineTerminator（含 U+FEFF，不含 U+0085）。
fn is_js_trim_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

fn trim_end_js(text: &str) -> &str {
    text.trim_end_matches(is_js_trim_whitespace)
}

/// `stripBom(content)`：返回 `(bom, text)`。
///
/// `bom` 是 `"\u{FEFF}"` 或空串，`text` 是去掉 BOM 后的其余内容。调用方在写回时把
/// `bom` 原样拼回，避免编辑过程把文件开头的 BOM 丢掉。
pub fn strip_bom(content: &str) -> (String, String) {
    match content.strip_prefix('\u{FEFF}') {
        Some(rest) => ("\u{FEFF}".to_owned(), rest.to_owned()),
        None => (String::new(), content.to_owned()),
    }
}

// ---------------------------------------------------------------------------
// 行区间与替换
// ---------------------------------------------------------------------------

/// 一行的字节范围 `[start, end)`；`end` 包含该行自己的换行符（如果有）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineSpan {
    start: usize,
    end: usize,
}

/// `splitLinesWithEndings(content)`：`/[^\n]*\n|[^\n]+/g`。
///
/// 每行保留自己的 `\n`；最后一行没有换行时也原样收下；空输入得到空 `Vec`。
/// 切分按字节找 `\n`，而 `\n` 是单个 ASCII 字节，所以每个切点都是合法的 UTF-8 边界。
fn split_lines_with_endings(content: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            lines.push(content[start..=index].to_owned());
            start = index + 1;
        }
    }
    if start < content.len() {
        lines.push(content[start..].to_owned());
    }
    lines
}

// 把 `split_lines_with_endings` 的行拼成连续区间：除最后一行外每行都带自己的换行，
// 行与行之间没有空隙，因此 `base_lines[i].end == base_lines[i + 1].start`。
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

/// 一次被应用的替换（TS: `Pick<MatchedEdit, "matchIndex" | "matchLength" | "newText">`）。
///
/// `match_index` 与 `match_length` 都以 `base_content` 的字节为单位；`new_text` 是
/// 替换文本，不参与坐标计算。
/// [`apply_replacements_preserving_unchanged_lines`] 在计算行范围时会拒绝越界区间；
/// 但同一批替换在字节层面是否互相重叠不会被检查——上层
/// [`apply_edits_to_normalized_content`] 在调用前已经保证它们互不重叠，直接调用
/// [`apply_replacements_preserving_unchanged_lines`] 时需要自行保证。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextReplacement {
    /// 在 `baseContent` 里的起始字节。
    pub match_index: usize,
    /// 匹配长度（字节）。
    ///
    /// 精确匹配时是 `old_text` 的字节数；模糊匹配时是归一化后 `old_text` 的字节数。
    pub match_length: usize,
    /// 替换文本。
    pub new_text: String,
}

// 把替换区间映射成它覆盖的整行范围 `[start_line, end_line)`。
//
// 起点必须落在某一行的 `[start, end)` 内；起点恰好在内容末尾（或越过末尾）时找不到
// 所在行，返回 "Replacement range is outside the base content."。终点则逐行扩展，直到
// 某行的 `end` 覆盖 `replacement_end`；终点越界同样报这个错。
fn get_replacement_line_range(
    lines: &[LineSpan],
    replacement: &TextReplacement,
) -> Result<(usize, usize), BoxError> {
    let replacement_start = replacement.match_index;
    let replacement_end = replacement.match_index + replacement.match_length;

    let mut start_line: Option<usize> = None;
    for (index, line) in lines.iter().enumerate() {
        if replacement_start >= line.start && replacement_start < line.end {
            start_line = Some(index);
            break;
        }
    }
    let Some(start_line) = start_line else {
        return Err(std::io::Error::other("Replacement range is outside the base content.").into());
    };

    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return Err(std::io::Error::other("Replacement range is outside the base content.").into());
    }

    Ok((start_line, end_line + 1))
}

// 在 `content` 上应用一组替换，返回新字符串。
//
// 调用前提：`replacements` 按 `match_index` 升序排列，否则逆序应用时后面的偏移会失效。
// `offset` 把替换记录里的绝对偏移换算成 `content` 内的偏移（分组应用时传入组的起始
// 字节）。每次替换都用 `format!` 重新拼接整个字符串，未做原地修改。
fn apply_replacements(content: &str, replacements: &[TextReplacement], offset: usize) -> String {
    let mut result = content.to_owned();
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

/// 把针对 `baseContent` 的替换应用到 `originalContent`，同时保留原始内容里未被触碰的整行。
///
/// **为什么需要**：模糊匹配时 `base_content` 是归一化后的文本（NFKC、行尾去空白、
/// 引号/空格替换）。如果直接替换整份基底再返回，所有未被编辑触及的行也会被改写成
/// 归一化形态。这个函数按**整行**分组，只重建与替换区间重叠的行组，其余行逐字节取自
/// `original_content`。
///
/// **输入**：`original_content` 是归一化之前的原文，行数必须与 `base_content` 相同；
/// `base_content` 是替换的坐标系；`replacements` 的偏移相对 `base_content`。
///
/// **分组规则**：按 `match_index` 升序遍历；每个替换映射到它覆盖的整行范围
/// `[start_line, end_line)`。若新替换的起始行仍落在上一组的行范围内，就并入上一组并
/// 扩张结束行（同一行或跨行重叠的替换合并处理）；否则另起一组。相邻但不相交的行会
/// 分到不同组，两组的行范围正好首尾相接、没有重叠。
///
/// **输出**：未触发行保持原始字节，被触行的整行来自 `base_content`（因此那一行的
/// 归一化形态会进入结果）。输出行数由未触行的原始行与被触行的替换结果决定：`new_text`
/// 含换行、或替换跨越原有行边界时，行数可能与 `original_content` 不同。输入两侧行数
/// 不一致时报
/// "Cannot preserve unchanged lines because the base content has a different line count."；
/// 替换区间越界时报 "Replacement range is outside the base content."。
pub fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[TextReplacement],
) -> Result<String, BoxError> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return Err(std::io::Error::other(
            "Cannot preserve unchanged lines because the base content has a different line count.",
        )
        .into());
    }

    struct Group {
        start_line: usize,
        end_line: usize,
        replacements: Vec<TextReplacement>,
    }
    let mut groups: Vec<Group> = Vec::new();
    // 先按起点排序：分组扫描需要有序，且同一组内的替换也必须升序
    // （`apply_replacements` 的逆序应用依赖这个前提）。
    let mut sorted: Vec<&TextReplacement> = replacements.iter().collect();
    sorted.sort_by_key(|replacement| replacement.match_index);
    for replacement in sorted {
        let (start_line, end_line) = get_replacement_line_range(&base_lines, replacement)?;
        if let Some(current) = groups.last_mut()
            && start_line < current.end_line
        {
            current.end_line = current.end_line.max(end_line);
            current.replacements.push(replacement.clone());
            continue;
        }
        groups.push(Group {
            start_line,
            end_line,
            replacements: vec![replacement.clone()],
        });
    }

    let mut original_line_index = 0usize;
    let mut result = String::new();
    // 逐组重建：组之前的整行原样取自 original_content，组覆盖的行用 base_content
    // 的行切片加替换结果，组之后的行继续原样复制。
    for group in &groups {
        for line in &original_lines[original_line_index..group.start_line] {
            result.push_str(line);
        }
        let group_start_offset = base_lines[group.start_line].start;
        let group_end_offset = base_lines[group.end_line - 1].end;
        result.push_str(&apply_replacements(
            &base_content[group_start_offset..group_end_offset],
            &group.replacements,
            group_start_offset,
        ));
        original_line_index = group.end_line;
    }
    for line in &original_lines[original_line_index..] {
        result.push_str(line);
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// 模糊查找
// ---------------------------------------------------------------------------

/// `FuzzyMatchResult`。
///
/// `index` 与 `match_length` 的坐标系是 `content_for_replacement`，不是调用时传入的
/// `content`：精确命中时两者相同；模糊命中时 `content_for_replacement` 是
/// [`normalize_for_fuzzy_match`] 的结果。调用方必须把替换应用到
/// `content_for_replacement`，否则偏移会错位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatchResult {
    /// 是否找到。
    pub found: bool,
    /// 匹配起点（在 `content_for_replacement` 里的字节偏移）；未找到为 -1。
    pub index: isize,
    /// 匹配长度。
    ///
    /// 精确命中是 `old_text` 的字节数；模糊命中是归一化后 `old_text` 的字节数。
    pub match_length: usize,
    /// 是否用了模糊匹配（false = 精确匹配）。
    pub used_fuzzy_match: bool,
    /// 替换操作应使用的内容：精确匹配时是原文，模糊匹配时是归一化后的文本。
    ///
    /// 未找到时回退为传入的原文；此时 `found` 为 `false`、`index` 为 `-1`，调用方
    /// 不应使用这两个字段。
    pub content_for_replacement: String,
}

/// `fuzzyFindText(content, oldText)`：先精确匹配，再在模糊归一化空间里匹配。
///
/// 两个阶段：
/// 1. 先在 `content` 里按字节查找精确子串 `old_text`；命中则把 `used_fuzzy_match`
///    记为 `false`，`content_for_replacement` 就是原文。
/// 2. 否则把两边都做 [`normalize_for_fuzzy_match`] 再查找；命中则
///    `used_fuzzy_match = true`，`match_length` 取归一化后 `old_text` 的字节长度。
///    两阶段都不命中时返回 `found = false`、`index = -1`、`match_length = 0`，并把
///    原文放回 `content_for_replacement`。
///
/// 注意精确阶段是普通子串查找：空 `old_text` 会立即在位置 0 命中
/// （`match_length = 0`）。这个函数本身不做非空检查，拒绝空 `oldText` 是上层
/// [`apply_edits_to_normalized_content`] 的职责。
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    if let Some(exact_index) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index: exact_index as isize,
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_owned(),
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
            content_for_replacement: content.to_owned(),
        },
    }
}

/// 一次文本替换（TS: `Edit`）。
///
/// JSON 字段名是工具 schema 的 `oldText` / `newText`（由 `serde` 的 `camelCase` 重命名
/// 属性保证，属性保持原样）。两个字段都按调用方传入的原始文本保存，行尾在
/// [`apply_edits_to_normalized_content`] 内部才归一化。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    /// 原文（必须唯一匹配）。
    ///
    /// 匹配分两轮：第一轮在传入的原文 `normalized_content` 上探测是否需要模糊匹配；
    /// 第二轮在选定基底（任一 edit 触发模糊匹配时已做 [`normalize_for_fuzzy_match`]）
    /// 上重新查找。两轮都针对整份基底，不是在上一次替换后的内容上递增匹配。
    pub old_text: String,
    /// 替换文本。
    ///
    /// 可以为空（用于删除）；行尾会先被归一化为 LF。
    pub new_text: String,
}

/// `applyEditsToNormalizedContent` 的结果。
///
/// 函数假定调用方已完成 `strip_bom` / `normalize_to_lf`（`edit.rs` 确实如此）。直接
/// 调用时 `base_content` 原样透传输入的 `normalized_content`：不做归一化、不剥 BOM，
/// 因此可能保留 CRLF 或 BOM；`new_content` 由该基底与 edit 文本产生，可能混合行尾，
/// 二者都不保证不含 BOM，也没有还原调用方原来的行尾。还原是 `edit.rs` 用
/// [`restore_line_endings`] 和保存下来的 BOM 完成的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEditsResult {
    /// 用于生成 diff 的基准内容（LF 归一化后的原文）。
    pub base_content: String,
    /// 替换后的内容。
    ///
    /// 已应用全部 edit；使用模糊匹配时，未被触及的行逐字节取自原文（LF 形态）。
    pub new_content: String,
}

// 统计 `old_text` 在归一化空间里的出现次数（非重叠、从左到右，与 JS `split` 的计数
// 方式一致）。上层对**每个** edit 都调用它，即使该 edit 在探测阶段是精确命中——
// 唯一性必须在即将应用替换的基底上成立。
fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    if fuzzy_old_text.is_empty() {
        // JS 是 `fuzzyContent.split(fuzzyOldText).length - 1`：空分隔符按 UTF-16 码元拆分，
        // 非空内容得到“码元数 - 1”，空内容拆成 `[]`（即 -1）。调用方只判断 `> 1`，
        // 所以空内容饱和成 0 与 -1 行为相同。
        return fuzzy_content.encode_utf16().count().saturating_sub(1);
    }
    fuzzy_content.matches(&fuzzy_old_text).count()
}

// 下面四个函数生成错误消息；这些字符串是模型在工具结果里看到的文本（当前稳定契约）：
// 单个 edit 与多个 edit 的措辞不同，`tests/tools.rs` 和
// `tests/fixtures/tools_diff_cases.json` 会断言原文，改文案等于改对外行为。
fn get_not_found_error(path: &str, edit_index: usize, total_edits: usize) -> BoxError {
    let message = if total_edits == 1 {
        format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        )
    } else {
        format!(
            "Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines."
        )
    };
    std::io::Error::other(message).into()
}

fn get_duplicate_error(
    path: &str,
    edit_index: usize,
    total_edits: usize,
    occurrences: usize,
) -> BoxError {
    let message = if total_edits == 1 {
        format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        )
    } else {
        format!(
            "Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        )
    };
    std::io::Error::other(message).into()
}

fn get_empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> BoxError {
    let message = if total_edits == 1 {
        format!("oldText must not be empty in {path}.")
    } else {
        format!("edits[{edit_index}].oldText must not be empty in {path}.")
    };
    std::io::Error::other(message).into()
}

fn get_no_change_error(path: &str, total_edits: usize) -> BoxError {
    let message = if total_edits == 1 {
        format!(
            "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        )
    } else {
        format!("No changes made to {path}. The replacements produced identical content.")
    };
    std::io::Error::other(message).into()
}

// 内部中间态：`edit_index` 保留该 edit 在原始 `edits` 数组里的下标。`matched_edits`
// 会按 `match_index` 排序，排序后的 overlap 错误仍靠它指回原始下标；not-found 与
// duplicate 在 push 之前就用循环下标 `index` 生成文案，不读取该字段。其余字段是选定
// 基底上的匹配结果。
struct MatchedEdit {
    edit_index: usize,
    match_index: usize,
    match_length: usize,
    new_text: String,
}

/// `applyEditsToNormalizedContent(normalizedContent, edits, path)`。
///
/// edit 工具的核心：输入已经 LF 归一化的文件内容与若干 `edit`，输出可直接用于 diff
/// 与写回的两种文本。函数不碰文件系统，`path` 只用于拼错误消息。
///
/// 执行顺序：
/// 1. 把每个 `edit.old_text` / `new_text` 再做一次 `normalize_to_lf`（调用方可能传入
///    带 CRLF 的 oldText），然后拒绝空的 `old_text`。
/// 2. 做一轮探测：只要任意 edit 在 `normalized_content` 上需要模糊匹配，
///    `replacement_base_content` 就切换为 [`normalize_for_fuzzy_match`] 的结果，
///    否则就是原文。这是一个全局开关：一旦触发，其它 edit 也会在归一化基底上
///    重新定位。
/// 3. 在选定基底上逐个重新定位：找不到报 not-found；出现次数大于 1 报 duplicate
///    （即使该 edit 在探测阶段是精确命中）。
/// 4. 按 `match_index` 排序并检查相邻替换是否重叠；只有严格大于才算重叠，首尾相接
///    是允许的。
/// 5. 应用替换：未使用模糊匹配时直接在基底上逆序拼接；使用模糊匹配时调用
///    [`apply_replacements_preserving_unchanged_lines`]，保住未触行的原始字节。
/// 6. 若结果与 `base_content` 完全相同，返回 “No changes made” 错误。
///
/// 返回值：`base_content` 原样取自传入的 `normalized_content`（diff 的“前”侧），
/// `new_content` 是在该基底上应用 edit 后的文本。函数自身只对 edit 文本做
/// `normalize_to_lf`；`base_content` 与 `new_content` 都不保证无 BOM，`new_content`
/// 还可能混合行尾（直接调用夹具里就有保留 CRLF 与 BOM 的用例）。
///
/// 错误：全部用 `std::io::Error::other` 包装进 `BoxError`，文案是模型可见输出的一部分。
/// 检查顺序会影响同一输入报哪条错误：空 oldText 优先于 not-found，逐个 edit 的
/// not-found/duplicate 优先于全部 edit 的 overlap 检查，no-change 检查在最后。
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEditsResult, BoxError> {
    // 阶段 1：把 edit 文本统一到 LF。空 oldText 在这里就拒绝，早于任何查找。
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

    // 阶段 2：探测是否有 edit 需要模糊匹配。只要有一个需要，整份基底就换成模糊
    // 归一化后的文本；这一步决定后面所有替换的坐标系。
    let initial_matches: Vec<FuzzyMatchResult> = normalized_edits
        .iter()
        .map(|edit| fuzzy_find_text(normalized_content, &edit.old_text))
        .collect();
    let used_fuzzy_match = initial_matches.iter().any(|result| result.used_fuzzy_match);
    let replacement_base_content = if used_fuzzy_match {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_owned()
    };

    // 阶段 3：在选定基底上逐个重新定位，并做存在性/唯一性检查。
    let mut matched_edits: Vec<MatchedEdit> = Vec::new();
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
            match_index: match_result.index as usize,
            match_length: match_result.match_length,
            new_text: edit.new_text.clone(),
        });
    }

    // 阶段 4：按位置排序后检查重叠。首尾相接（前一个的结束 == 后一个的开始）允许。
    matched_edits.sort_by_key(|edit| edit.match_index);
    for index in 1..matched_edits.len() {
        let previous = &matched_edits[index - 1];
        let current = &matched_edits[index];
        if previous.match_index + previous.match_length > current.match_index {
            return Err(std::io::Error::other(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index
            ))
            .into());
        }
    }

    // 阶段 5：应用替换。模糊路径必须走 preserving 版本，否则未触行的原始字节会被
    // 整份归一化文本覆盖。
    let base_content = normalized_content.to_owned();
    let new_content = if used_fuzzy_match {
        let replacements: Vec<TextReplacement> = matched_edits
            .iter()
            .map(|edit| TextReplacement {
                match_index: edit.match_index,
                match_length: edit.match_length,
                new_text: edit.new_text.clone(),
            })
            .collect();
        apply_replacements_preserving_unchanged_lines(
            normalized_content,
            &replacement_base_content,
            &replacements,
        )?
    } else {
        let replacements: Vec<TextReplacement> = matched_edits
            .iter()
            .map(|edit| TextReplacement {
                match_index: edit.match_index,
                match_length: edit.match_length,
                new_text: edit.new_text.clone(),
            })
            .collect();
        apply_replacements(&replacement_base_content, &replacements, 0)
    };

    if base_content == new_content {
        return Err(get_no_change_error(path, normalized_edits.len()));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
    })
}

// ---------------------------------------------------------------------------
// jsdiff 8.0.4：diffLines
// ---------------------------------------------------------------------------

/// `diffLines` 的一块（TS: `ChangeObject`）。
///
/// 一次 [`diff_lines`] 的结果按编辑脚本顺序排列。`added` 与 `removed` 同时为 false
/// 表示两文件共有的一段；同一处变更可能拆成相邻的删除块与新增块，谁先谁后由 Myers
/// 的取舍决定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// 该块的文本。
    ///
    /// 是这一块在对应文件里的原始拼接，保留行尾换行；最后一行的换行可能不存在。
    pub value: String,
    /// 是否新增。
    pub added: bool,
    /// 是否删除。
    pub removed: bool,
    /// 行数。
    ///
    /// 一个 token 对应一行（换行符附在前一行上），末行即使没有换行也算一行。
    pub count: usize,
}

/// `tokenize(value)`（`newlineIsToken: false`、`stripTrailingCr: false`）。
///
/// 目标 token 形状是“一行内容 + 它自己的换行符”。实现先把输入拆成交替的
/// `[文本片段, 换行片段]`（`\n` 或 `\r\n`），再把换行片段并回前一个文本片段，最后丢弃
/// 空 token。单独的 `\r` 不是换行片段，会留在文本内容里。
///
/// 输出顺序就是行顺序；除最后一行外每个 token 都以换行结尾，空输入返回空 `Vec`。
fn tokenize_lines(value: &str) -> Vec<String> {
    let bytes = value.as_bytes();
    let mut pieces: Vec<String> = Vec::new();
    let mut last = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\n' {
            pieces.push(value[last..index].to_owned());
            pieces.push("\n".to_owned());
            index += 1;
            last = index;
        } else if bytes[index] == b'\r' && index + 1 < bytes.len() && bytes[index + 1] == b'\n' {
            pieces.push(value[last..index].to_owned());
            pieces.push("\r\n".to_owned());
            index += 2;
            last = index;
        } else {
            index += 1;
        }
    }
    pieces.push(value[last..].to_owned());

    // 字符串以换行结尾时，split 的最后一个空 token 被丢弃。
    if pieces.last().is_some_and(String::is_empty) {
        pieces.pop();
    }

    let mut tokens: Vec<String> = Vec::new();
    for (index, piece) in pieces.into_iter().enumerate() {
        if index % 2 == 1 {
            if let Some(token) = tokens.last_mut() {
                token.push_str(&piece);
            }
        } else {
            tokens.push(piece);
        }
    }
    // removeEmpty
    tokens.retain(|token| !token.is_empty());
    tokens
}

/// Myers 搜索中的一条路径状态。
///
/// `old_pos` 是已经消费到的 old token 下标（-1 表示尚未消费任何 old token）；`last`
/// 是 components 链的链尾。对角线 `old_pos - new_pos` 由 `best_path` 的 key 隐含，
/// 不单独存储。
#[derive(Debug, Clone, Copy)]
struct PathState {
    old_pos: isize,
    last: Option<usize>,
}

/// 编辑脚本中的一个块：连续 `count` 个同类 token（共同行 / 新增行 / 删除行）。
///
/// `previous` 指向前一个块，形成反向链。`value` 在搜索阶段保持空串，直到
/// `build_values` 才按 token 游标填入；被淘汰的搜索分支仍留在 arena 里，只有沿
/// `previous` 能走到的链会被输出。
#[derive(Debug, Clone)]
struct Component {
    count: usize,
    added: bool,
    removed: bool,
    previous: Option<usize>,
    value: String,
}

/// Myers 差分的工作区：借用两边的 token 序列，`components` 是只增不减的 arena。
///
/// 每条搜索路径用索引指向自己的链尾，多条路径共享同一个链前缀；因此复制
/// `PathState` 很便宜，但 arena 会保留所有曾经创建过的块（包括被放弃的分支）。
struct Myers<'a> {
    old_tokens: &'a [String],
    new_tokens: &'a [String],
    components: Vec<Component>,
}

impl Myers<'_> {
    /// `addToPath(path, added, removed, oldPosInc)`。
    ///
    /// 把“新增一行”或“删除一行”接到路径尾部：若链尾与本操作同类，新块 `count + 1`
    /// 且 `previous` 指向链尾的 `previous`（等价于把链尾换成一个计数加一的块，旧块
    /// 留在 arena 里但不在链上）；否则新块 `count = 1`、`previous` 指向链尾。
    /// 每次调用都把 `old_pos` 前移 `old_pos_increment`（新增为 0，删除为 1）。
    fn add_to_path(
        &mut self,
        path: PathState,
        added: bool,
        removed: bool,
        old_pos_increment: isize,
    ) -> PathState {
        if let Some(last_index) = path.last {
            let last = &self.components[last_index];
            if last.added == added && last.removed == removed {
                let component = Component {
                    count: last.count + 1,
                    added,
                    removed,
                    previous: last.previous,
                    value: String::new(),
                };
                self.components.push(component);
                return PathState {
                    old_pos: path.old_pos + old_pos_increment,
                    last: Some(self.components.len() - 1),
                };
            }
        }
        self.components.push(Component {
            count: 1,
            added,
            removed,
            previous: path.last,
            value: String::new(),
        });
        PathState {
            old_pos: path.old_pos + old_pos_increment,
            last: Some(self.components.len() - 1),
        }
    }

    /// `extractCommon(basePath, newTokens, oldTokens, diagonalPath)`。
    ///
    /// 从 `base_path.old_pos` 与 `new_pos = old_pos - diagonal_path` 出发，沿对角线
    /// 吃掉相等的 token；吃到 `common_count` 个就压入一个共同块并更新链尾。返回值是
    /// 消耗后的 `new_pos`，`old_pos` 写回 `base_path`。
    fn extract_common(&mut self, base_path: &mut PathState, diagonal_path: isize) -> isize {
        let new_len = self.new_tokens.len() as isize;
        let old_len = self.old_tokens.len() as isize;
        let mut old_pos = base_path.old_pos;
        let mut new_pos = old_pos - diagonal_path;
        let mut common_count = 0usize;
        while new_pos + 1 < new_len
            && old_pos + 1 < old_len
            && self.old_tokens[(old_pos + 1) as usize] == self.new_tokens[(new_pos + 1) as usize]
        {
            new_pos += 1;
            old_pos += 1;
            common_count += 1;
        }
        if common_count > 0 {
            self.components.push(Component {
                count: common_count,
                added: false,
                removed: false,
                previous: base_path.last,
                value: String::new(),
            });
            base_path.last = Some(self.components.len() - 1);
        }
        base_path.old_pos = old_pos;
        new_pos
    }

    /// `buildValues(lastComponent, newTokens, oldTokens)`。
    ///
    /// 先沿 `previous` 从链尾回溯到起点再反转，得到时间顺序；然后同时维护 old/new
    /// 两个 token 游标取子切片：removed 块只消费 old，added 块只消费 new，共同块
    /// 两边都消费。调用前提是这条链已经覆盖到两边的末尾（调用点都在“两边都耗尽”
    /// 的分支上）。
    fn build_values(mut self, last_component: Option<usize>) -> Vec<Change> {
        let mut order: Vec<usize> = Vec::new();
        let mut current = last_component;
        while let Some(index) = current {
            order.push(index);
            current = self.components[index].previous;
        }
        order.reverse();

        let mut new_pos = 0usize;
        let mut old_pos = 0usize;
        let mut changes = Vec::with_capacity(order.len());
        for index in order {
            let component = &mut self.components[index];
            if !component.removed {
                let end = new_pos + component.count;
                component.value = self.new_tokens[new_pos..end].concat();
                new_pos = end;
                if !component.added {
                    old_pos += component.count;
                }
            } else {
                let end = old_pos + component.count;
                component.value = self.old_tokens[old_pos..end].concat();
                old_pos = end;
            }
            changes.push(Change {
                value: component.value.clone(),
                added: component.added,
                removed: component.removed,
                count: component.count,
            });
        }
        changes
    }
}

// 对角线搜索的初始上下界，用作 ±∞ 哨兵：分别赋给 `min_diagonal` 与 `max_diagonal`，
// 循环中会被 `min`/`max` 夹到 ±edit_length。常量本身只参与取负，没有与 edit_length
// 做加减；`/ 4` 这个取值的具体理由在源码与 spec 中都没有依据，待核实。
const DIAGONAL_INFINITY: isize = isize::MAX / 4;

/// jsdiff Myers 的核心：输入两份文本，按顺序输出 [`Change`] 块。
///
/// `best_path` 保存“当前编辑距离下仍可继续扩展的路径”，key 是 `old_pos - new_pos`
/// （对角线）。每一轮 `edit_length` 只沿同奇偶性的对角线推进：一次新增或删除都会让
/// 对角线变化 ±1，因此编辑距离相同的路径落在同奇偶的对角线上。
///
/// 阶段：
/// 1. 两边分别 tokenize；
/// 2. 先在 diagonal 0 上吃掉公共前缀，两边都耗尽就直接物化返回；
/// 3. 从 `edit_length = 1` 起扩展前沿：对每条对角线，取 `diagonal - 1` 的删除候选
///    与 `diagonal + 1` 的新增候选，判断能否继续，按 jsdiff 的取舍规则选一条，再吃掉
///    新的公共部分；
/// 4. 任一路径让两边都耗尽时立即物化并返回；
/// 5. 循环耗尽则返回空 `Vec`（兜底；`structured_patch` 会把它当成“无差异”）。
///
/// 取舍规则（`remove_path.old_pos < add_path.old_pos` 时优先新增）决定同一位置上
/// `-` 块与 `+` 块的先后顺序，因此会直接改变 patch 的字节输出，不能随意改动。
fn diff_line_changes(old_string: &str, new_string: &str) -> Vec<Change> {
    let old_tokens = tokenize_lines(old_string);
    let new_tokens = tokenize_lines(new_string);
    let new_len = new_tokens.len() as isize;
    let old_len = old_tokens.len() as isize;

    let mut state = Myers {
        old_tokens: &old_tokens,
        new_tokens: &new_tokens,
        components: Vec::new(),
    };
    let mut best_path: HashMap<isize, PathState> = HashMap::new();
    // 初始状态：old_pos = -1 表示两边都还没消费任何 token；先在 diagonal 0 上
    // 吃掉公共前缀，若两边同时耗尽就不需要进入 Myers 循环。
    let mut initial = PathState {
        old_pos: -1,
        last: None,
    };
    let mut new_pos = state.extract_common(&mut initial, 0);
    if initial.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
        return state.build_values(initial.last);
    }
    best_path.insert(0, initial);

    let mut edit_length = 1isize;
    let max_edit_length = new_len + old_len;
    let mut min_diagonal = -DIAGONAL_INFINITY;
    let mut max_diagonal = DIAGONAL_INFINITY;

    // 逐轮扩大编辑距离。每轮只扫描仍可能通向终点的对角线区间，步长为 2 保持奇偶性。
    while edit_length <= max_edit_length {
        let start = min_diagonal.max(-edit_length);
        let end = max_diagonal.min(edit_length);
        let mut diagonal_path = start;
        while diagonal_path <= end {
            // 删除候选来自 diagonal - 1（删除让 diagonal 加 1），新增候选来自
            // diagonal + 1（新增让 diagonal 减 1）。
            let remove_path = best_path.get(&(diagonal_path - 1)).copied();
            let add_path = best_path.get(&(diagonal_path + 1)).copied();
            // 与 jsdiff 一致：读取后立即丢弃这个 predecessor，它在本轮已被消费，
            // 后续轮次不会再需要。
            if remove_path.is_some() {
                best_path.remove(&(diagonal_path - 1));
            }
            let mut can_add = false;
            if let Some(add_path) = add_path {
                // 新增候选要消费的 new token 下标是 old_pos - diagonal；越界说明没有
                // 可新增的行。
                let add_path_new_pos = add_path.old_pos - diagonal_path;
                can_add = 0 <= add_path_new_pos && add_path_new_pos < new_len;
            }
            // 删除候选必须还有下一个 old token 可消费。
            let can_remove = remove_path.is_some_and(|path| path.old_pos + 1 < old_len);
            // 两个方向都走不通：这条对角线被淘汰。
            if !can_add && !can_remove {
                best_path.remove(&diagonal_path);
                diagonal_path += 2;
                continue;
            }
            // 取舍：两个方向都可用时比较 old_pos，仅当 `remove_path.old_pos <
            // add_path.old_pos` 时选新增；否则（删除候选更靠后，或两者相等）选删除。
            // 这个 tie-break 决定同一位置上 -/+ 块的先后顺序。
            let mut base_path = if !can_remove
                || (can_add
                    && remove_path.as_ref().unwrap().old_pos < add_path.as_ref().unwrap().old_pos)
            {
                state.add_to_path(add_path.unwrap(), true, false, 0)
            } else {
                state.add_to_path(remove_path.unwrap(), false, true, 1)
            };
            // 接上新增/删除这一步之后，继续沿当前对角线吃掉公共行。
            new_pos = state.extract_common(&mut base_path, diagonal_path);
            // 两边都到末尾：这条路径就是最终编辑脚本，立即物化。
            if base_path.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                return state.build_values(base_path.last);
            }
            best_path.insert(diagonal_path, base_path);
            // 已经到某一边末尾的路径只能朝一个方向继续，用 min/max_diagonal 把反向的
            // 对角线排除出后续扫描范围。
            if base_path.old_pos + 1 >= old_len {
                max_diagonal = max_diagonal.min(diagonal_path - 1);
            }
            if new_pos + 1 >= new_len {
                min_diagonal = min_diagonal.max(diagonal_path + 1);
            }
            diagonal_path += 2;
        }
        edit_length += 1;
    }
    // 兜底：正常输入会从上面的提前返回点结束。若循环耗尽，返回空块列表
    // （`structured_patch` 会把它当成“无差异”）。
    Vec::new()
}

/// `diffLines(oldStr, newStr)`。
///
/// 公开入口，输出顺序与 jsdiff 8.0.4 的 `diffLines` 一致：共同行、删除行、新增行按
/// 编辑脚本顺序排列，同一位置的 `-` 块可能排在 `+` 块之前。
/// `tests/fixtures/tools_diff_cases.json` 的 `diff_cases` 用 TS 参考实现生成的
/// `jsdiff` 字段逐块比对 `value`/`added`/`removed`/`count`。
pub fn diff_lines(old_string: &str, new_string: &str) -> Vec<Change> {
    diff_line_changes(old_string, new_string)
}

// ---------------------------------------------------------------------------
// jsdiff 8.0.4：structuredPatch / formatPatch / createTwoFilesPatch
// ---------------------------------------------------------------------------

/// 一个 unified diff hunk。
///
/// `old_start`/`new_start` 是 1 起的起始行号，`old_lines`/`new_lines` 是两侧行数；
/// `lines` 已经带 ` `/`+`/`-` 前缀，并在需要时插入了 `\ No newline at end of file`
/// 标记行。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hunk {
    old_start: i64,
    old_lines: i64,
    new_start: i64,
    new_lines: i64,
    lines: Vec<String>,
}

/// `splitLines(text)`：每行含其换行（最后一行可无）。
///
/// 与 `split_lines_with_endings` 不同，空串会被拆成一个空字符串元素；非空输入下
/// 两者输出相同（都保留行尾换行，最后一行没有换行时也原样保留）。`structured_patch`
/// 给哨兵块预置空缓存，正是为了避开空串这一点。
fn split_lines(text: &str) -> Vec<String> {
    let has_trailing_newline = text.ends_with('\n');
    let mut result: Vec<String> = text.split('\n').map(|line| format!("{line}\n")).collect();
    if has_trailing_newline {
        result.pop();
    } else {
        let last = result.pop().unwrap_or_default();
        result.push(last[..last.len().saturating_sub(1)].to_owned());
    }
    result
}

// 给未变行加 unified diff 的上下文前缀（一个空格）。
fn context_lines(lines: &[String]) -> Vec<String> {
    lines.iter().map(|line| format!(" {line}")).collect()
}

/// `structuredPatch(oldFileName, newFileName, oldStr, newStr, undefined, undefined, { context })`。
///
/// 把 [`diff_lines`] 的块组装成 unified diff 的 hunk：
///
/// - `old_range_start == 0` 表示“当前没有打开的 hunk”。行号从 1 开始，且回填的上下文
///   不会超过已消费的行数，所以 0 不会与真实行号冲突。
/// - 遇到变更块而当前没有 hunk 时开启一个：起始行号取当前 `old_line`/`new_line`，
///   再把**前一个未变块**末尾的 `context` 行作为前导上下文加进来，起始行号相应前移。
/// - 遇到未变块且 hunk 打开时：若这段未变行不超过 `2 * context` 行、并且后面还有真实
///   变更块，就整段作为上下文并入当前 hunk（让两处变更共享同一个 hunk）；否则关闭
///   当前 hunk，只取这段未变块开头的 `context` 行作为尾随上下文。
/// - `old_line`/`new_line` 始终表示“下一个未消费的行号”：变更行只推进对应一侧，
///   未变行两侧都推进。
///
/// `lines_cache` 缓存每个块拆分后的行：开启新 hunk 时要回读前一个未变块的尾部行，
/// 缓存避免重复拆分。最后一项（哨兵块）的缓存预置为空 `Vec`，因为 `split_lines("")`
/// 会返回一个空字符串元素、凭空多出一行。
///
/// 扫描结束后逐个 hunk 处理行尾：能去掉 `\n` 的只去换行；某行没有 `\n` 说明它是对应
/// 文件的最后一行，在其后插入 `\ No newline at end of file` 标记。
///
/// 文件名参数不参与计算（函数末尾显式忽略），保留只为与 TS 签名一致；真正写进
/// `---`/`+++` 的是 `format_patch`。
fn structured_patch(
    old_file_name: &str,
    new_file_name: &str,
    old_string: &str,
    new_string: &str,
    context: i64,
) -> Vec<Hunk> {
    let mut diff = diff_lines(old_string, new_string);
    // 追加一个 value 为空、行数为 0 的哨兵块：如果文件以变更块结尾，循环需要一个
    // 未变块来关闭最后一个 hunk。
    diff.push(Change {
        value: String::new(),
        added: false,
        removed: false,
        count: 0,
    });
    // 哨兵没有行。这里预置空 Vec 而不是让下面的 split_lines 处理 ""，否则会多出一个
    // 幽灵空行。
    let mut lines_cache: Vec<Option<Vec<String>>> = vec![None; diff.len()];
    lines_cache[diff.len() - 1] = Some(Vec::new());

    let mut hunks: Vec<Hunk> = Vec::new();
    let mut old_range_start: i64 = 0;
    let mut new_range_start: i64 = 0;
    let mut current_range: Vec<String> = Vec::new();
    let mut old_line: i64 = 1;
    let mut new_line: i64 = 1;

    for index in 0..diff.len() {
        let lines = match lines_cache[index].take() {
            Some(lines) => lines,
            None => split_lines(&diff[index].value),
        };
        let added = diff[index].added;
        let removed = diff[index].removed;
        if added || removed {
            // 0 是“没有打开的 hunk”的空标记；第一次遇到变更块时在这里开启。
            if old_range_start == 0 {
                let previous = if index > 0 { index - 1 } else { 0 };
                old_range_start = old_line;
                new_range_start = new_line;
                if index > 0 {
                    // 前导上下文：回读前一个未变块的最后 context 行，并把 hunk 的
                    // 起始行号往前移同样多行。
                    let previous_lines = lines_cache[previous]
                        .as_ref()
                        .expect("the previous component's lines were computed");
                    current_range = if context > 0 {
                        let start = previous_lines.len().saturating_sub(context as usize);
                        context_lines(&previous_lines[start..])
                    } else {
                        Vec::new()
                    };
                    old_range_start -= current_range.len() as i64;
                    new_range_start -= current_range.len() as i64;
                }
            }
            // 变更行带 +/- 前缀，只推进对应文件一侧的行号。
            for line in &lines {
                current_range.push(format!("{}{}", if added { '+' } else { '-' }, line));
            }
            if added {
                new_line += lines.len() as i64;
            } else {
                old_line += lines.len() as i64;
            }
        } else {
            if old_range_start != 0 {
                // 间隔足够短（<= 2 * context）且后面还有真实变更块：整段并入当前
                // hunk，避免产生两个本可合并的相邻 hunk。
                if lines.len() as i64 <= context * 2 && index < diff.len() - 2 {
                    for line in context_lines(&lines) {
                        current_range.push(line);
                    }
                } else {
                    // 关闭 hunk：只取这段未变块开头的 context 行作为尾随上下文。
                    let context_size = lines.len().min(context as usize);
                    for line in context_lines(&lines[..context_size]) {
                        current_range.push(line);
                    }
                    // old_line 是下一个未消费的行号，减去 hunk 起始行号就是 hunk 内
                    // 已写入的旧行数；再加上尾随上下文行数。
                    hunks.push(Hunk {
                        old_start: old_range_start,
                        old_lines: old_line - old_range_start + context_size as i64,
                        new_start: new_range_start,
                        new_lines: new_line - new_range_start + context_size as i64,
                        lines: std::mem::take(&mut current_range),
                    });
                    old_range_start = 0;
                    new_range_start = 0;
                }
            }
            old_line += lines.len() as i64;
            new_line += lines.len() as i64;
        }
        lines_cache[index] = Some(lines);
    }

    // 行尾处理：能去掉换行的只去掉换行；没有换行的行只可能是文件最后一行，在它后面
    // 插入标记并跳过，避免对标记本身再做一次检查。
    for hunk in &mut hunks {
        let mut index = 0usize;
        while index < hunk.lines.len() {
            if let Some(stripped) = hunk.lines[index].strip_suffix('\n') {
                let stripped = stripped.to_owned();
                hunk.lines[index] = stripped;
                index += 1;
            } else {
                hunk.lines
                    .insert(index + 1, "\\ No newline at end of file".to_owned());
                index += 2;
            }
        }
    }

    // 文件名不参与 structuredPatch 的计算，只为与 TS 签名一致；---/+++ 由
    // format_patch 生成。
    let _ = (old_file_name, new_file_name);
    hunks
}

/// `formatPatch(patch, FILE_HEADERS_ONLY)`。
///
/// 输出固定两行文件头（`--- path`、`+++ path`，无时间戳、无 `=` 分隔线），随后每个
/// hunk 一行 `@@ -old_start,old_lines +new_start,new_lines @@` 和正文。计数**总是**带
/// `,count`（即使为 1 也不省略），这是 jsdiff 的输出格式。
///
/// 某一侧行数为 0（纯新增或纯删除的 hunk）时，该侧起始行号减 1，符合 unified diff
/// “空范围指向插入点前一行”的约定；夹具中的 `@@ -0,0 +1,1 @@` 就是这种情形。
///
/// 输出总是以 `\n` 结尾；没有 hunk 时只剩两行文件头（不是空串）。
fn format_patch(old_file_name: &str, new_file_name: &str, hunks: &[Hunk]) -> String {
    let mut output: Vec<String> = Vec::new();
    output.push(format!("--- {old_file_name}"));
    output.push(format!("+++ {new_file_name}"));
    for hunk in hunks {
        let mut old_start = hunk.old_start;
        let mut new_start = hunk.new_start;
        if hunk.old_lines == 0 {
            old_start -= 1;
        }
        if hunk.new_lines == 0 {
            new_start -= 1;
        }
        output.push(format!(
            "@@ -{},{} +{},{} @@",
            old_start, hunk.old_lines, new_start, hunk.new_lines
        ));
        for line in &hunk.lines {
            output.push(line.clone());
        }
    }
    let mut text = output.join("\n");
    text.push('\n');
    text
}

/// `createTwoFilesPatch(path, path, old, new, undefined, undefined, { context, headerOptions: FILE_HEADERS_ONLY })`。
///
/// `path` 同时作为 `---` 与 `+++` 的文件名；`context_lines` 是每个 hunk 的前后上下文
/// 行数（`edit` 工具固定传 4）。这是工具结果 `details.patch` 的来源，与
/// [`generate_diff_string`] 消费同一对 `base_content`/`new_content`。
pub fn generate_unified_patch(
    path: &str,
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> String {
    let hunks = structured_patch(path, path, old_content, new_content, context_lines as i64);
    format_patch(path, path, &hunks)
}

/// `generateDiffString` 的结果。
///
/// `diff` 是给人看的、带行号列的多行文本（不是 unified diff），行间用 `\n` 连接、
/// 末尾没有换行；`first_changed_line` 只在存在变更时是 `Some`，无变化时 `diff` 为空串。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffStringResult {
    /// 带行号的展示 diff。
    pub diff: String,
    /// 新文件里第一处变化的行号（1 起）。
    pub first_changed_line: Option<usize>,
}

// 左补空格到固定宽度。调用方只传十进制行号（纯 ASCII），所以用字节长度计算宽度。
fn pad_start(value: usize, width: usize) -> String {
    let text = value.to_string();
    let padding = width.saturating_sub(text.len());
    format!("{}{}", " ".repeat(padding), text)
}

/// `generateDiffString(oldContent, newContent, contextLines = 4)`。
///
/// 输出格式：变更行是 `+新文件行号 文本` / `-旧文件行号 文本`，未变行是
/// ` 行号 文本`；行号列按两侧 `split('\n')` 元素个数的最大值的十进制位数左补空格
/// 对齐（文本以换行结尾时 split 会多一个空元素，TS 用同样的计数方式）。被省略的
/// 中间行用一行 `...` 表示，行号列留空、`...` 与文本列对齐。
///
/// 上下文规则由相邻块决定：`last_was_change` 表示上一块是不是变更，下一块是不是
/// 变更决定这段未变行属于前导上下文、尾随上下文还是两者之间。两处变更之间的未变行
/// 不超过 `2 * context_lines` 行时全部显示，否则首尾各留 `context_lines` 行、中间用
/// 省略行代替；文件开头/结尾的未变行只保留靠近变更的一侧。
///
/// `first_changed_line`：遇到第一个 added/removed 块时、写入任何变更行之前捕获当时的
/// `new_line_number`（1 起）。因此纯删除得到的也是“该删除发生处的新文件行号”。
///
/// 注意：这是展示文本，不处理 BOM 或行尾（调用方传的是 LF 归一化后的文本），输出
/// 末尾没有换行。
pub fn generate_diff_string(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> DiffStringResult {
    let parts = diff_lines(old_content, new_content);
    let mut output: Vec<String> = Vec::new();

    let old_lines: Vec<&str> = old_content.split('\n').collect();
    let new_lines: Vec<&str> = new_content.split('\n').collect();
    let max_line_number = old_lines.len().max(new_lines.len());
    let line_number_width = max_line_number.to_string().len();

    let mut old_line_number = 1usize;
    let mut new_line_number = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for (index, part) in parts.iter().enumerate() {
        // 块值以 \n 结尾时 split 会产生一个尾部空串，先去掉；最后一行没有换行时保留。
        let mut raw: Vec<&str> = part.value.split('\n').collect();
        if raw.last() == Some(&"") {
            raw.pop();
        }

        if part.added || part.removed {
            // 第一处变更在写入任何变更行之前记录：此刻 new_line_number 就是变化在
            // 新文件中的位置（删除块不会让 new_line_number 前进）。
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_number);
            }
            for line in raw {
                if part.added {
                    output.push(format!(
                        "+{} {}",
                        pad_start(new_line_number, line_number_width),
                        line
                    ));
                    new_line_number += 1;
                } else {
                    output.push(format!(
                        "-{} {}",
                        pad_start(old_line_number, line_number_width),
                        line
                    ));
                    old_line_number += 1;
                }
            }
            last_was_change = true;
        } else {
            let next_part_is_change =
                index < parts.len() - 1 && (parts[index + 1].added || parts[index + 1].removed);
            let has_leading_change = last_was_change;
            let has_trailing_change = next_part_is_change;

            // 两侧都紧邻变更：短间隔全部显示；长间隔首尾各留 context_lines 行，
            // 中间用一行省略号代替。
            if has_leading_change && has_trailing_change {
                if raw.len() <= context_lines * 2 {
                    for line in raw {
                        output.push(format!(
                            " {} {}",
                            pad_start(old_line_number, line_number_width),
                            line
                        ));
                        old_line_number += 1;
                        new_line_number += 1;
                    }
                } else {
                    let leading_lines = &raw[..context_lines];
                    let trailing_lines = &raw[raw.len() - context_lines..];
                    let skipped_lines = raw.len() - leading_lines.len() - trailing_lines.len();

                    for line in leading_lines {
                        output.push(format!(
                            " {} {}",
                            pad_start(old_line_number, line_number_width),
                            line
                        ));
                        old_line_number += 1;
                        new_line_number += 1;
                    }

                    output.push(format!(" {} ...", " ".repeat(line_number_width)));
                    old_line_number += skipped_lines;
                    new_line_number += skipped_lines;

                    for line in trailing_lines {
                        output.push(format!(
                            " {} {}",
                            pad_start(old_line_number, line_number_width),
                            line
                        ));
                        old_line_number += 1;
                        new_line_number += 1;
                    }
                }
            } else if has_leading_change {
                // 只有上方是变更：显示开头至多 context_lines 行，其余省略。
                let shown_lines = &raw[..context_lines.min(raw.len())];
                let skipped_lines = raw.len() - shown_lines.len();

                for line in shown_lines {
                    output.push(format!(
                        " {} {}",
                        pad_start(old_line_number, line_number_width),
                        line
                    ));
                    old_line_number += 1;
                    new_line_number += 1;
                }

                if skipped_lines > 0 {
                    output.push(format!(" {} ...", " ".repeat(line_number_width)));
                    old_line_number += skipped_lines;
                    new_line_number += skipped_lines;
                }
            } else if has_trailing_change {
                // 只有下方是变更：前面的行省略，只显示末尾至多 context_lines 行。
                let skipped_lines = raw.len().saturating_sub(context_lines);
                if skipped_lines > 0 {
                    output.push(format!(" {} ...", " ".repeat(line_number_width)));
                    old_line_number += skipped_lines;
                    new_line_number += skipped_lines;
                }

                for line in &raw[skipped_lines..] {
                    output.push(format!(
                        " {} {}",
                        pad_start(old_line_number, line_number_width),
                        line
                    ));
                    old_line_number += 1;
                    new_line_number += 1;
                }
            } else {
                // 两侧都没有变更相邻（长未变段的中间）：整段不输出，只推进行号。
                old_line_number += raw.len();
                new_line_number += raw.len();
            }

            last_was_change = false;
        }
    }

    DiffStringResult {
        diff: output.join("\n"),
        first_changed_line,
    }
}
