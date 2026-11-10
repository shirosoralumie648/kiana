//! alt-screen-search.ts：备用屏全文搜索（语料构建、匹配、搜索框组件）。
//!
//! ## 与 TS 的表示差异
//!
//! 1. **索引单位**：TS 的 `textStart`/`textEnd`/`match.index` 都是 UTF-16 码元偏移（`string.length`
//!    语义）。Rust 的 `regex` 给出字节偏移，因此匹配结果先按"从上一个匹配继续数"的方式换算成
//!    UTF-16 偏移，再与 span 的 `text_start`/`text_end` 比较——数值与预期偏移一致。
//! 2. **正则**：TS `new RegExp(escapeRegExp(q), "giu")`；Rust 用
//!    [`regex::RegexBuilder`] 的 `case_insensitive(true)`（Unicode simple case folding，与 JS `i`
//!    的规范一致；土耳其语 İ/ı 等边角见 `spec/tui-core.md` §2.12.2）。
//! 3. **数组身份**：[`AltScreenSearchIndex::search`] 在未变化时返回**同一个**
//!    `Rc<Vec<…>>`（TS 返回同一数组引用），测试据此断言 "reuses indexed matches"。
//!
//! ## 组成与调用关系
//!
//! - 纯函数层：[`find_alt_screen_search_matches`] 一次性构建语料并返回全部匹配；
//!   [`AltScreenSearchIndex`] 是带缓存的版本，[`crate::alt_screen::TuiAltScreen`] 在每次
//!   渲染前用它重算当前转录行上的匹配。
//! - 组件层：[`AltScreenSearchComponent`] 是搜索 overlay 的内容（3 行方框：输入行、
//!   右对齐的结果计数、带导航按钮的底边）。`TuiAltScreen` 的 `toggle_search` 创建它，
//!   以右上角锚点、宽度 40%、最小 32 列、四周 1 格边距显示。组件本身**不做搜索**：
//!   文本变化时通过构造参数里的回调把 query 交给宿主。
//!
//! ## 语料构建（`build_search_corpus`）
//!
//! 1. 每行先 `strip_terminal_sequences` 去掉 ANSI/OSC 序列，再按"非空白 run"切块；
//!    相邻块（包括跨行相邻的块）在语料里只放**一个**空格，因此查询里的连续空白被折叠后
//!    照样能命中跨列/跨行的短语。
//! 2. 每个块记录一个 `SearchSourceSpan`：`text_start/text_end` 是它在语料字符串中的
//!    UTF-16 偏移，`row` 是原行号，`start_col/end_col` 是渲染列的半开区间。
//!    `linear_columns` 区分两种块：可打印 ASCII 块里"UTF-16 偏移 = 显示列"，命中可以按
//!    偏移线性换算；含 Unicode/宽字符的块只能整块命中（列区间取整块宽度）。
//! 3. 空行/纯空白行不产生块，但行号仍按输入 `lines` 计（调用方传的是完整转录行，
//!    中间的空行不会让后续行的行号前移）。
//!
//! ## 匹配流程（`find_search_corpus_matches`）
//!
//! 查询先转义成正则字面量，再以 `case_insensitive(true)` 在语料上迭代匹配。`regex` 给出
//! 字节偏移，`Utf16Cursor` 按升序增量换算成 UTF-16 偏移（与 span 同一坐标系）。每个命中
//! 按 span 切成若干段：[`AltScreenSearchSegment`] 的 `start_col` 含、`end_col` 不含。
//! 同一行上列方向相接/重叠的段会合并；被源文本空白隔开的块保留为多段（各段分别高亮，
//! 中间空白列不点亮）。
//!
//! ## 缓存语义（[`AltScreenSearchIndex::search`]）
//!
//! - 每次调用都逐行比较 `lines`（先比长度再比内容）；行变化时重建语料。
//! - 行未变且规范化 query 与上次相同时不跑正则，`changed == false`，返回上次的同一个
//!   `Rc`；调用方据此跳过选中项重算。
//! - 空 query 也按普通 query 处理：结果为空数组，但行内容仍会被缓存。
//!
//! ## 组件约束
//!
//! [`AltScreenSearchComponent::new`] 的 `on_query_change` 在 [`Component::handle_input`]
//! 期间被同步调用；`TuiAltScreen` 的回调通过 `Weak<RefCell<AltState>>` 只排队 `pending_query`，
//! 不持有 `TuiAltScreen`；查询在 `handle_terminal_input` 的输入分发结束后由
//! `update_search_query` 应用。

use std::rc::Rc;

use regex::RegexBuilder;

use crate::components::input::{Input, InputOptions};
use crate::keybindings::get_keybindings;
use crate::text::{
    get_grapheme_segmenter, is_js_whitespace, strip_terminal_sequences, trim_js, truncate_to_width,
    visible_width,
};
use crate::tui::{Component, Focusable};

/// 搜索结果的一段（TS `SearchSourceSpan`）。
///
/// `text_start..text_end` 是语料 `text` 上的 UTF-16 半开区间；`row` 是调用方传入的原始
/// 行号；`start_col..end_col` 是渲染列（显示格）的半开区间。`linear_columns == true`
/// 表示块内全是可打印 ASCII（0x20-0x7e），此时 UTF-16 偏移与显示列一一对应，命中可以按
/// 偏移线性截取；否则（含宽字符/组合字符）整块使用 `start_col..end_col`。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchSourceSpan {
    text_start: usize,
    text_end: usize,
    row: usize,
    start_col: usize,
    end_col: usize,
    linear_columns: bool,
}

/// 搜索语料（TS `SearchCorpus`）。
///
/// `text` 是扁平语料：空白分隔的块之间恰有一个 U+0020；同一非 ASCII run 内的相邻字素块
/// 之间没有分隔符（文本直接相连）。`spans` 按 `text_start` 升序排列且互不重叠。
/// 映射阶段只取与命中相交的 span，因此分隔空格本身不会被映射成高亮列。
#[derive(Debug, Clone, Default)]
struct SearchCorpus {
    text: String,
    spans: Vec<SearchSourceSpan>,
}

/// 匹配的一段（TS `AltScreenSearchSegment`）。
///
/// 一次匹配可能跨行，因此按屏幕片段拆开；每段的列坐标都是**显示列**（不是 UTF-16 下标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AltScreenSearchSegment {
    /// 原始输入中的 0 基行号（与传给搜索的行数组下标一致）。
    pub row: usize,
    /// 起始显示列（含）。
    pub start_col: usize,
    /// 结束显示列（不含）；与 `start_col` 的差是该段在屏幕上的宽度。
    pub end_col: usize,
}

/// 一次匹配（TS `AltScreenSearchMatch`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AltScreenSearchMatch {
    /// 按屏幕从上到下、从左到右排列的命中片段；同一行内相邻或重叠的片段已合并。
    /// 匹配器的生产路径保证非空（空的 `segments` 不会被加入结果）。
    pub segments: Vec<AltScreenSearchSegment>,
}

/// `PRINTABLE_ASCII = /^[\x20-\x7e]*$/`。
///
/// 这个区间的字符显示宽度都是 1，所以命中这条快路径时可以把字节偏移直接当显示列使用。
fn is_printable_ascii(text: &str) -> bool {
    text.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

/// `/^\s+$/u.test(text)`（JS 空白集合，见 [`is_js_whitespace`]）。
///
/// 空串不算"纯空白"；逐码点判断，不按字素簇切分。
fn is_whitespace_only(text: &str) -> bool {
    !text.is_empty() && text.chars().all(is_js_whitespace)
}

/// `buildSearchCorpus(lines)`——逐字移植（见 `spec/tui-core.md` §2.12.1）。
///
/// 把渲染后的转录行压成一个可正则搜索的扁平字符串，同时保留"每个非空白块对应回哪一行、
/// 哪些列"的映射。输入是**已渲染**的行（可能带 ANSI），函数内部会先剥离终端序列。
/// 输出语料里，空白分隔的块之间（包括跨行）恰有一个 U+0020；同一非 ASCII run 内的相邻
/// 字素块之间没有分隔符，文本直接相连。`spans` 与 `text` 的 UTF-16 坐标对齐。
fn build_search_corpus(lines: &[String]) -> SearchCorpus {
    let mut chunks: Vec<String> = Vec::new();
    let mut spans: Vec<SearchSourceSpan> = Vec::new();
    // `text_length` 是语料已累计的 UTF-16 长度（不是字节数），分隔空格也计入。
    let mut text_length: usize = 0;
    // 上一个块之后是否需要补一个分隔空格；语料还没有内容（text_length == 0）时的空白
    // 不会置位，因此语料不会以空格开头。
    let mut pending_separator = false;

    // 只在真正要写下个块时补分隔符，避免在语料末尾留下多余空格。
    fn append_separator(chunks: &mut Vec<String>, text_length: &mut usize, pending: &mut bool) {
        if !*pending {
            return;
        }
        chunks.push(" ".to_string());
        *text_length += 1;
        *pending = false;
    }

    for (row, raw_line) in lines.iter().enumerate() {
        let line = strip_terminal_sequences(raw_line);
        let mut column: usize = 0;

        // 渲染出来的转录行绝大多数是 ASCII：整段非空格 run 一次性索引，避免逐字素分配。
        // 这条路径里字节偏移、UTF-16 偏移与显示列三者相等（可打印 ASCII 宽度都是 1）。
        if is_printable_ascii(&line) {
            let bytes = line.as_bytes();
            let mut index = 0usize;
            while index < bytes.len() {
                if bytes[index] == 0x20 {
                    // 空格只推进显示列并标记"待补分隔符"：连续空格在语料里仍只占一个空格。
                    if text_length > 0 {
                        pending_separator = true;
                    }
                    column += 1;
                    index += 1;
                    continue;
                }
                let mut end = index + 1;
                while end < bytes.len() && bytes[end] != 0x20 {
                    end += 1;
                }
                // 把 `index..end` 作为一个块；块前若有待补的分隔符先写进语料。
                append_separator(&mut chunks, &mut text_length, &mut pending_separator);
                let text = &line[index..end];
                chunks.push(text.to_string());
                // ASCII 块：UTF-16 偏移与显示列都等于字节长度。
                spans.push(SearchSourceSpan {
                    text_start: text_length,
                    text_end: text_length + text.len(),
                    row,
                    start_col: column,
                    end_col: column + text.len(),
                    linear_columns: true,
                });
                text_length += text.len();
                column += text.len();
                index = end;
            }
        } else {
            // 非 ASCII 行走字素簇切分：空白字素只推进显示列，其余字素各自成为一个块
            // （组合字符不会被拆开）。语料按 UTF-16 计数、列按可见宽度累加，两者不再相等。
            for grapheme in get_grapheme_segmenter().segment(&line) {
                let text = grapheme.segment;
                let width = visible_width(text);
                if is_whitespace_only(text) {
                    if text_length > 0 {
                        pending_separator = true;
                    }
                    column += width;
                    continue;
                }
                append_separator(&mut chunks, &mut text_length, &mut pending_separator);
                chunks.push(text.to_string());
                let utf16_length = text.encode_utf16().count();
                // 标记 `linear_columns = false`：命中只能整块映射，不能按偏移线性截取。
                spans.push(SearchSourceSpan {
                    text_start: text_length,
                    text_end: text_length + utf16_length,
                    row,
                    start_col: column,
                    end_col: column + width,
                    linear_columns: false,
                });
                text_length += utf16_length;
                column += width;
            }
        }
        // 行结束：语料已有内容时，下一行的第一个块前补一个空格；空行不会产生额外分隔。
        if text_length > 0 {
            pending_separator = true;
        }
    }

    SearchCorpus {
        text: chunks.concat(),
        spans,
    }
}

/// `normalizeQuery(query)`：`query.replace(/\s+/gu, " ").trim()`。
///
/// 连续 JS 空白折叠成一个 U+0020，再按 JS 语义 trim；结果为空串表示"无查询"。
fn normalize_query(query: &str) -> String {
    let mut result = String::with_capacity(query.len());
    let mut in_whitespace = false;
    for ch in query.chars() {
        if is_js_whitespace(ch) {
            if !in_whitespace {
                result.push(' ');
                in_whitespace = true;
            }
        } else {
            result.push(ch);
            in_whitespace = false;
        }
    }
    trim_js(&result).to_string()
}

/// `escapeRegExp(text)`：转义 `[.*+?^${}()|[\]\\]`。
///
/// 逐字符处理：需要转义的字符先补一个 `\`，其余原样；反斜杠因此变成 `\\`。转义后的串
/// 只含字面量，`RegexBuilder` 不会把它解释成元字符。
fn escape_reg_exp(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(
            ch,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            result.push('\\');
        }
        result.push(ch);
    }
    result
}

/// 字节偏移 → UTF-16 偏移的增量换算器（匹配按字节升序到达）。
///
/// `regex::find_iter` 从左到右产出命中，因此这里只需向前累计：每次把 `self.byte..byte`
/// 的切片编码成 UTF-16 并加到计数上，整体是 O(语料长度)，不会对每个命中重新从头扫描。
struct Utf16Cursor<'a> {
    text: &'a str,
    byte: usize,
    utf16: usize,
}

impl<'a> Utf16Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Utf16Cursor {
            text,
            byte: 0,
            utf16: 0,
        }
    }

    /// 返回到 `byte` 为止的 UTF-16 偏移（`byte` 之前的字节只统计一次）。
    ///
    /// 前置条件：`byte` 单调不减且落在 `text` 的字符边界上（`regex` 的命中边界满足）。
    /// debug 构建的断言只检查单调不减与上界；字符边界由切片本身保证，越界或落在非字符
    /// 边界上的切片都会 panic。
    fn offset_at(&mut self, byte: usize) -> usize {
        debug_assert!(byte >= self.byte && byte <= self.text.len());
        let slice = &self.text[self.byte..byte];
        self.utf16 += slice.encode_utf16().count();
        self.byte = byte;
        self.utf16
    }
}

/// `findSearchCorpusMatches(corpus, normalizedQuery)`——逐字移植。
///
/// 输入必须是已规范化的非空 query 与 `build_search_corpus` 产出的语料；返回按语料顺序
/// （即屏幕从上到下、从左到右）排列的匹配。每个匹配至少含一个段。
fn find_search_corpus_matches(
    corpus: &SearchCorpus,
    normalized_query: &str,
) -> Vec<AltScreenSearchMatch> {
    // 阶段 1：空 query 直接返回（正则空串会匹配每个位置，没有意义）。
    if normalized_query.is_empty() {
        return Vec::new();
    }
    // 阶段 2：转义后构建大小写不敏感的正则。查询已是字面量，构建失败只可能是资源限制，
    // 保守地当作"无匹配"（不 panic）。
    let escaped = escape_reg_exp(normalized_query);
    let Ok(expression) = RegexBuilder::new(&escaped)
        .case_insensitive(true)
        .unicode(true)
        .build()
    else {
        // 全部字符都已转义的普通字面量，不会构建失败；保守地返回无匹配。
        return Vec::new();
    };

    let mut matches = Vec::new();
    // 两个游标都只向前推进：span_index 指向第一个可能与本命中相交的块。
    let mut span_index = 0usize;
    let mut cursor = Utf16Cursor::new(&corpus.text);

    for found in expression.find_iter(&corpus.text) {
        let start = cursor.offset_at(found.start());
        let end = start
            + corpus.text[found.start()..found.end()]
                .encode_utf16()
                .count();
        // 跳过在命中开始前就结束的块。
        while span_index < corpus.spans.len() && corpus.spans[span_index].text_end <= start {
            span_index += 1;
        }

        // 阶段 3：收集与 [start, end) 相交的块，换算成屏幕列区间。
        let mut segments: Vec<AltScreenSearchSegment> = Vec::new();
        for span in &corpus.spans[span_index..] {
            // 块按 text_start 升序：一旦越过命中结尾，后面的块都不可能相交。
            if span.text_start >= end {
                break;
            }
            if span.text_end <= start {
                continue;
            }
            // ASCII 块按相交部分线性换算（UTF-16 偏移差 = 列差）；非 ASCII 块整块高亮。
            let start_col = if span.linear_columns {
                span.start_col + start.max(span.text_start) - span.text_start
            } else {
                span.start_col
            };
            let end_col = if span.linear_columns {
                span.start_col + end.min(span.text_end) - span.text_start
            } else {
                span.end_col
            };
            // 同一行且与上一段相接/重叠时合并，避免跨块命中在屏幕上裂成两个高亮段。
            if let Some(previous) = segments.last_mut()
                && previous.row == span.row
                && start_col <= previous.end_col
            {
                previous.end_col = previous.end_col.max(end_col);
            } else {
                segments.push(AltScreenSearchSegment {
                    row: span.row,
                    start_col,
                    end_col,
                });
            }
        }
        // 阶段 4：为下一个命中推进 span 游标（本命中覆盖到的块不再回看）。
        while span_index < corpus.spans.len() && corpus.spans[span_index].text_end <= end {
            span_index += 1;
        }
        // 只含分隔空格的命中不会与任何块相交，此时丢弃。
        if !segments.is_empty() {
            matches.push(AltScreenSearchMatch { segments });
        }
    }

    matches
}

/// `AltScreenSearchResult`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AltScreenSearchResult {
    /// 匹配（TS 的数组引用语义用 `Rc` 表达）。
    pub matches: Rc<Vec<AltScreenSearchMatch>>,
    /// 本次是否重新计算了匹配：行内容或规范化 query 变化时为 true；false 表示
    /// `matches` 与上一次调用是同一个 `Rc`。
    pub changed: bool,
}

/// 缓存语料与匹配，直到渲染出的转录行不变（TS `AltScreenSearchIndex`）。
///
/// 缓存键是"完整行文本 + 规范化 query"。每次 [`AltScreenSearchIndex::search`] 都会逐行
/// 比较，因此调用方可以每帧无条件调用；命中缓存时不建语料、不跑正则，返回同一个
/// `Rc<Vec<AltScreenSearchMatch>>`（对应 TS 返回同一数组引用）。行比较是 `Vec<String>`
/// 的逐项相等，最坏仍是 O(总字符数)。
#[derive(Debug, Default)]
pub struct AltScreenSearchIndex {
    source_lines: Option<Vec<String>>,
    corpus: Option<SearchCorpus>,
    normalized_query: Option<String>,
    matches: Rc<Vec<AltScreenSearchMatch>>,
}

impl AltScreenSearchIndex {
    /// 新建（空缓存）。
    pub fn new() -> Self {
        AltScreenSearchIndex::default()
    }

    /// `search(lines, query)`。
    ///
    /// `lines` 是当前转录的**完整**渲染行（跨行短语依赖这一点）；`query` 是原始输入，
    /// 内部会规范化。返回的 `matches`/`changed` 见 [`AltScreenSearchIndex`] 的缓存语义。
    ///
    /// 副作用：首次调用或行变化时把 `lines` 复制进缓存并重建语料；query 变化时重跑正则。
    /// 两者都未变时是纯查询（只做行比较），不会修改缓存。
    pub fn search(&mut self, lines: &[String], query: &str) -> AltScreenSearchResult {
        // 行身份 = 长度 + 逐行内容；没有增量优化，必须逐行比较才能发现中间行的变化。
        let mut source_changed = self.source_lines.as_ref().map(Vec::len) != Some(lines.len());
        if !source_changed && let Some(source) = self.source_lines.as_ref() {
            for (index, line) in lines.iter().enumerate() {
                if source[index] == *line {
                    continue;
                }
                source_changed = true;
                break;
            }
        }
        if source_changed || self.corpus.is_none() {
            self.source_lines = Some(lines.to_vec());
            self.corpus = Some(build_search_corpus(lines));
        }

        let normalized_query = normalize_query(query);
        // 行或规范化 query 任一变化都要重算；空 query 首次出现也走这里（结果为空数组）。
        let changed = source_changed || self.normalized_query.as_deref() != Some(&normalized_query);
        if changed {
            self.normalized_query = Some(normalized_query.clone());
            let corpus = self.corpus.as_ref().expect("corpus built above");
            self.matches = Rc::new(find_search_corpus_matches(corpus, &normalized_query));
        }
        AltScreenSearchResult {
            matches: Rc::clone(&self.matches),
            changed,
        }
    }
}

/// `findAltScreenSearchMatches(lines, query)`。
///
/// 无缓存的便捷入口：空/纯空白 query 直接返回 `[]`（不构建语料）；否则**每次调用**都重建
/// 语料并跑一遍正则。每帧路径应改用 [`AltScreenSearchIndex`]。
pub fn find_alt_screen_search_matches(lines: &[String], query: &str) -> Vec<AltScreenSearchMatch> {
    let normalized_query = normalize_query(query);
    if normalized_query.is_empty() {
        return Vec::new();
    }
    let corpus = build_search_corpus(lines);
    find_search_corpus_matches(&corpus, &normalized_query)
}

/// `getAltScreenSearchMatchKey(match)`：`row:startCol:row:endCol`。
///
/// 取匹配的**首段起点 + 末段终点**作为身份键：匹配数组重算后（行内容变化），
/// [`crate::alt_screen::TuiAltScreen`] 用它在新区里找回"同一处"命中并保持选中。
/// `segments` 为空时返回空串（匹配器不会产出这种值）。
pub fn get_alt_screen_search_match_key(match_: &AltScreenSearchMatch) -> String {
    let (Some(first), Some(last)) = (match_.segments.first(), match_.segments.last()) else {
        return String::new();
    };
    format!(
        "{}:{}:{}:{}",
        first.row, first.start_col, last.row, last.end_col
    )
}

/// 导航按钮样式（TS `(text, hovered) => string`）。
///
/// `text` 是**未着色**的按钮文本（如 `↑ Shift+Enter`），`hovered` 表示指针是否在该按钮上。
/// 返回值原样拼进底边。实现必须保持按钮的**可见宽度**不变，否则按钮命中区间（按未着色
/// 文本的宽度计算）会与实际绘制位置错位。
pub type NavigationButtonStyle = Rc<dyn Fn(&str, bool) -> String>;

/// 备用屏搜索框（TS `AltScreenSearchComponent`）。
///
/// 固定 3 行：顶边框、输入行 + 右对齐的 `当前/总数`、底边框 + 导航按钮。
/// 组件自己持有 [`Input`]（提示符一个空格、占位符 `Find in transcript`、占位符样式为
/// 暗色），不直接做搜索：文本变化时通过 `on_query_change` 把 query 交给宿主。
/// 导航按钮的命中框在每次 `render` 时重算：行号固定为 2（底边框），列号相对组件左上角，
/// 未渲染时保持 `-1`。
pub struct AltScreenSearchComponent {
    input: Input,
    on_query_change: Box<dyn FnMut(&str)>,
    navigation_button_style: NavigationButtonStyle,
    result_count: usize,
    result_index: isize,
    previous_button_start: isize,
    previous_button_end: isize,
    next_button_start: isize,
    next_button_end: isize,
    hovered_navigation_direction: Option<i8>,
    focused: bool,
}

impl AltScreenSearchComponent {
    /// `constructor(onQueryChange, navigationButtonStyle = (text) => text)`。
    ///
    /// `on_query_change` 在输入内容实际变化时同步调用（见 [`Component::handle_input`]）；
    /// `navigation_button_style` 缺省时按钮不着色（恒等函数）。初始状态：无结果
    /// （`result_index = -1`、`result_count = 0`）、未聚焦。
    pub fn new(
        on_query_change: Box<dyn FnMut(&str)>,
        navigation_button_style: Option<NavigationButtonStyle>,
    ) -> Self {
        AltScreenSearchComponent {
            input: Input::new(InputOptions {
                prompt: Some(" ".to_string()),
                placeholder: Some("Find in transcript".to_string()),
                placeholder_style: Some(std::sync::Arc::new(|text: &str| {
                    format!("\x1b[2m{text}\x1b[22m")
                })),
            }),
            on_query_change,
            navigation_button_style: navigation_button_style
                .unwrap_or_else(|| Rc::new(|text: &str, _hovered: bool| text.to_string())),
            result_count: 0,
            result_index: -1,
            previous_button_start: -1,
            previous_button_end: -1,
            next_button_start: -1,
            next_button_end: -1,
            hovered_navigation_direction: None,
            focused: false,
        }
    }

    /// 当前查询（`input.getValue()`）。
    pub fn query(&self) -> &str {
        self.input.get_value()
    }

    /// `setResult(index, count)`。
    ///
    /// `index` 是 0 基选中下标，`-1` 表示没有选中项；渲染时显示 `index + 1 / count`。
    /// `count == 0` 且输入非空时显示 `No matches`。只写状态，不触发重绘。
    pub fn set_result(&mut self, index: isize, count: usize) {
        self.result_index = index;
        self.result_count = count;
    }

    /// `getNavigationDirectionAt(row, column)`：`-1` 上一个、`1` 下一个。
    ///
    /// `row`/`column` 是相对组件左上角的 0 基坐标。只有底边框（`row == 2`）上的两个按钮
    /// 命中：区间为 `[start, end)`，按未着色按钮文本的可见宽度计算。尚未渲染过（命中框
    /// 仍为 `-1`）或落在分隔符/边框上时返回 `None`。
    pub fn get_navigation_direction_at(&self, row: usize, column: usize) -> Option<i8> {
        if row != 2 {
            return None;
        }
        let column = column as isize;
        if column >= self.previous_button_start && column < self.previous_button_end {
            return Some(-1);
        }
        if column >= self.next_button_start && column < self.next_button_end {
            return Some(1);
        }
        None
    }

    /// `setHoveredNavigationDirection(direction)`：值变化返回 true。
    ///
    /// 返回 true 表示调用方需要请求一帧重绘（hover 样式挂在按钮文本上）；值未变时不产生渲染。
    pub fn set_hovered_navigation_direction(&mut self, direction: Option<i8>) -> bool {
        if direction == self.hovered_navigation_direction {
            return false;
        }
        self.hovered_navigation_direction = direction;
        true
    }
}

/// `formatKey(key)`：`key.split("+")` 逐段首字母大写；darwin 上 `alt` → `Option`。
///
/// 只接收一个按键串（渲染时取每个动作的第一个绑定）；`None` 显示 `Unbound`。
/// 每段只把**首字符**大写、其余字符原样保留（`shift+enter` → `Shift+Enter`）。
fn format_key(key: Option<&str>) -> String {
    let Some(key) = key else {
        return "Unbound".to_string();
    };
    key.split('+')
        .map(|part| {
            if cfg!(target_os = "macos") && part.eq_ignore_ascii_case("alt") {
                return "Option".to_string();
            }
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut result = String::new();
                    result.extend(first.to_uppercase());
                    result.push_str(chars.as_str());
                    result
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

impl Component for AltScreenSearchComponent {
    /// 把输入交给内部的 [`Input`]；只有文本真的变化时才回调宿主。
    ///
    /// 方向键、Home/End 等按键只移动光标（不改文本），因此不会触发搜索。
    fn handle_input(&mut self, data: &str) {
        let previous = self.input.get_value().to_string();
        self.input.handle_input(data);
        let query = self.input.get_value().to_string();
        if query != previous {
            (self.on_query_change)(&query);
        }
    }

    /// 转发给内部输入框；其 `invalidate` 目前是空操作（`Input` 没有可失效的渲染缓存）。
    fn invalidate(&mut self) {
        self.input.invalidate();
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }

    /// 渲染 3 行方框；宽度不足时导航按钮先退化为纯箭头，再整组隐藏。
    ///
    /// 布局同时更新导航按钮的命中框（相对组件左上角的 0 基行列），供
    /// [`AltScreenSearchComponent::get_navigation_direction_at`] 使用。
    fn render(&mut self, width: usize) -> Vec<String> {
        // 宽度至少按 1 处理；左右边框各占 1 列。
        let safe_width = width.max(1);
        let inner_width = safe_width.saturating_sub(2);

        // 按钮只显示每个动作的**第一个**绑定键，再按平台把 `alt` 显示成 `Option`。
        let keybindings = get_keybindings();
        let previous_keys = keybindings.get_keys("tui.altScreen.searchPrevious");
        let next_keys = keybindings.get_keys("tui.altScreen.searchNext");
        let previous_key = format_key(previous_keys.first().map(String::as_str));
        let next_key = format_key(next_keys.first().map(String::as_str));

        // 结果计数：空 query 不显示；有 query 但无命中显示 `No matches`；否则 `序号/总数`。
        let query = self.input.get_value();
        let result = if query.is_empty() {
            String::new()
        } else if self.result_count == 0 {
            "No matches".to_string()
        } else {
            format!("{}/{}", self.result_index + 1, self.result_count)
        };
        // 结果文本最多占 inner_width - 3：两侧各 1 个空格之外，至少给输入框留 1 列。
        let result_space = inner_width.saturating_sub(3);
        let visible_result = truncate_to_width(&result, result_space, "", false);
        let result_text = if visible_result.is_empty() {
            String::new()
        } else {
            format!("\x1b[2m {visible_result} \x1b[22m")
        };
        // 输入框渲染到剩余宽度；不足部分补空格，把结果计数顶到右侧。
        let input_width = inner_width.saturating_sub(visible_width(&result_text));
        let input_line = truncate_to_width(
            self.input
                .render(input_width.max(1))
                .first()
                .cloned()
                .unwrap_or_default()
                .as_str(),
            input_width,
            "",
            false,
        );
        let input_padding = " ".repeat(input_width.saturating_sub(visible_width(&input_line)));
        let content = format!("{input_line}{input_padding}{result_text}");

        let mut previous_button = format!("↑ {previous_key}");
        let mut next_button = format!("↓ {next_key}");
        let mut separator = " · ".to_string();
        let outer_gap_width = 1usize;
        // 底边可用宽度：inner_width 预留两端各 1 格间距，以及右侧 1 格 rule。
        let available_controls_width = inner_width.saturating_sub(outer_gap_width * 2 + 1);
        let mut controls_width = visible_width(&previous_button)
            + visible_width(&separator)
            + visible_width(&next_button);
        // 窄屏先退化成纯箭头（去掉快捷键文字）；仍然放不下就整组隐藏。
        if controls_width > available_controls_width {
            previous_button = "↑".to_string();
            next_button = "↓".to_string();
            separator = " ".to_string();
            controls_width = visible_width(&previous_button)
                + visible_width(&separator)
                + visible_width(&next_button);
        }
        let show_buttons = controls_width <= available_controls_width;
        // 命中框用未着色文本测量；样式回调只应追加 ANSI（见 NavigationButtonStyle 的宽度约束）。
        let rendered_buttons = if show_buttons {
            format!(
                "{}{}{}",
                (self.navigation_button_style)(
                    &previous_button,
                    self.hovered_navigation_direction == Some(-1)
                ),
                separator,
                (self.navigation_button_style)(
                    &next_button,
                    self.hovered_navigation_direction == Some(1)
                )
            )
        } else {
            String::new()
        };
        let outer_gaps_width = if show_buttons { outer_gap_width * 2 } else { 0 };
        // 右侧 rule 只在按钮绘制出来、且剩余宽度还分得出 1 列时保留。
        let right_rule_width =
            if !rendered_buttons.is_empty() && inner_width > controls_width + outer_gaps_width {
                1
            } else {
                0
            };
        let left_rule_width = inner_width
            .saturating_sub(if show_buttons { controls_width } else { 0 })
            .saturating_sub(outer_gaps_width)
            .saturating_sub(right_rule_width);
        // 命中框：列 0 是左边框 `└`，之后是 left rule，再隔一个 gap 才是第一个按钮。
        // 每次渲染都重算；按钮隐藏时写 -1 哨兵，get_navigation_direction_at 因此不会命中。
        let previous_start = 1 + left_rule_width + outer_gap_width;
        self.previous_button_start = if show_buttons {
            previous_start as isize
        } else {
            -1
        };
        self.previous_button_end = if show_buttons {
            (previous_start + visible_width(&previous_button)) as isize
        } else {
            -1
        };
        self.next_button_start = if show_buttons {
            self.previous_button_end + visible_width(&separator) as isize
        } else {
            -1
        };
        self.next_button_end = if show_buttons {
            self.next_button_start + visible_width(&next_button) as isize
        } else {
            -1
        };

        // 1 列宽的极端情况只画三个边框字符：正常路径的 `┌…┐` 至少 2 列，会超出给定宽度。
        if safe_width == 1 {
            return vec!["┌".to_string(), "│".to_string(), "└".to_string()];
        }
        let space_if_buttons = if rendered_buttons.is_empty() { "" } else { " " };
        // 底边布局：└ + 左 rule + 间距 + 按钮组 + 间距 + 右 rule + ┘。
        vec![
            format!("┌{}┐", "─".repeat(inner_width)),
            format!("│{content}│"),
            format!(
                "└{}{space_if_buttons}{rendered_buttons}{space_if_buttons}{}┘",
                "─".repeat(left_rule_width),
                "─".repeat(right_rule_width)
            ),
        ]
    }
}

impl Focusable for AltScreenSearchComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    /// 焦点同时镜像给内部输入框：它据此决定是否输出光标标记。
    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.input.set_focused(focused);
    }
}
