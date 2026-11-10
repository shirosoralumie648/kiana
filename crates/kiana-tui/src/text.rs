//! utils.ts / word-navigation.ts / kill-ring.ts / undo-stack.ts。
//!
//! 宽度、ANSI/OSC/APC 识别与跨行状态、换行、截断、按列切片、overlay 分段、按词移动、
//! kill ring 与撤销栈。本 crate 内不得出现第二份实现。
//!
//! ## 与 TS 的表示差异（均不改变可观察输出）
//!
//! - **索引单位**。TS 的 `length`/`slice`/`indexOf` 是 UTF-16 码元；本文件对外暴露的
//!   `pos`/`length` 若是 **ANSI 序列扫描用的字节偏移**，那是因为可识别的序列全部是 ASCII，
//!   两者数值相同（见 [`extract_ansi_code`]）。而涉及文本切片的偏移（[`find_word_backward`]、
//!   [`find_word_forward`] 的 `cursor`、[`GraphemeSegment::index`]、[`WordSegment::index`]）
//!   一律是 **UTF-16 码元**（编辑器的光标偏移与测试断言都按此单位）。光标落在代理对**中间**
//!   时，按码元切片会切出孤立代理；Rust 的 `str` 不能表示孤立代理，因此本文件按 UTF-16
//!   码元逐位切片，把落单的半个代理写成 `U+FFFD`——ICU 对孤立代理的分段行为与 `U+FFFD`
//!   完全相同（见 `slice_utf16_prefix` / `utf16_units`），所以结果与逐码元切片的预期逐值一致。
//! - **分段器**。字素簇用 `unicode-segmentation`（UAX #29，与 ICU 78.3 在本仓库测试覆盖的
//!   字符集上逐例一致，含 emoji ZWJ、区域指示符对、`\r\n`、Indic 连字）。
//!
//!   词边界按 ICU 的 `word.txt` 规则引擎重写了一遍（`default_word_segments`）：
//!   `Intl.Segmenter` 走的是 ICU 的规则引擎 + 词典引擎。逐段对齐需要规则引擎的
//!   **边界**与**规则状态标签**（`{100}`/`{200}`/`{400}`，`isWordLike` 就是"标签 > 0"），
//!   所以本文件内嵌了 ICU `word.txt` 的规则表与 Unicode 17.0 的
//!   `Word_Break`/`Script`/`Extended_Pictographic`/`LineBreak=SA`/`GCB=Control` 区间表
//!   （`text_tables::WORD_BREAK_RANGES` 等）。
//!
//!   词典引擎那一半在 `word_dict`：从 Node 26.12.1 内置的 ICU 78.3 数据包里取出
//!   `cjdict`/`thaidict`/`laodict`/`khmerdict`/`burmesedict` 五份词典（逐字节提交在
//!   `src/dicts/*.bin`），并按 ICU4C `common/dictbe.cpp` 逐函数移植了
//!   `CjkBreakEngine`/`ThaiBreakEngine`/`LaoBreakEngine`/`KhmerBreakEngine`/
//!   `BurmeseBreakEngine`。词典数据与算法都与 Node 相同，因此词典段内部的边界
//!   （`ひらがな` → `ひ`/`ら`/`が`/`な`，`จอห์น` 不切，半角片假名先做 NFKC）与 Node 一致。
//!
//!   规则引擎那一半同样已对齐：`__`/`_`、`한́`、`가〮나`、`ก́x` 这类 `isWordLike`
//!   与边界问题不依赖词典（见 `tests/regression_word_segmentation.rs`）。
//! - **宽度表**。East Asian Width 用 `get-east-asian-width@1.6.0`（Unicode 17.0）的区间表逐段
//!   内嵌（`text_tables::FULLWIDTH_RANGES` / `WIDE_RANGES`），**不用** `unicode-width`（算法不同）。
//! - **宽度缓存**。TS 是模块级 `Map`（FIFO 逐出 512 项）；Rust 用 `thread_local!`，语义等价且
//!   无锁（整个 TUI 单线程）。
//! - **[`flatten_lines`] 是 no-op**。TS 用它让 V8 把拼接出来的字符串压平；Rust 无对应物，
//!   但保留签名以对齐调用方。
//!
//! ## 这个文件为什么存在
//!
//! TUI 的排版、截断、overlay 合成使用同一种单位——“终端显示列”；编辑器的按词移动是
//! 例外，使用 UTF-16 码元偏移（见上文“与 TS 的表示差异”和下文“贯穿全文件的约定”）。
//! 如果各模块各自按字节数、UTF-16 码元数或 `chars().count()` 近似，宽字符、组合字符、
//! emoji、TAB 与 ANSI 序列就会让光标与渲染错位。本 crate 只在本文档描述的位置定义宽度与
//! 切片规则；布局（`layout.rs`）、各组件（`components/*`）、overlay（`tui.rs`）、
//! markdown 与编辑器都调用这里。
//!
//! ## 阅读顺序与执行流程
//!
//! 1. **宽度**：[`grapheme_width`] 把**一个字素簇**换算成列数；[`visible_width`] 在它
//!    之上处理 TAB（记 3 列）与可识别转义序列（记 0 列），并带 512 项 FIFO 缓存。
//! 2. **转义序列识别**：[`ansi_code_length`] 是后续所有扫描的公共基础，决定“哪一段是
//!    控制序列、哪一段是可见文本”。识别范围是**有限**的：CSI 只认 `m`/`G`/`K`/`H`/`J`
//!    作终止符，OSC/APC 只认 BEL 或 `ESC \`；其余以 ESC 开头的字节按普通文本处理
//!    （ESC 本身属于 Control，宽度为 0）。
//! 3. **跨行样式状态**：[`AnsiCodeTracker`] 扫描序列并记录 SGR 属性与 OSC 8 链接，
//!    供换行时在行首重开样式、行末只关下划线与链接。
//! 4. **截断 / 换行 / 切片 / overlay**：[`truncate_to_width`]、[`wrap_text_with_ansi`]、
//!    [`slice_with_width`]、[`extract_segments`] 全部按字素簇与显示列工作，并保证
//!    切点处不会把样式泄漏到本不该有样式的文本上。
//! 5. **字符分类与词分段**：[`is_js_whitespace`] 等函数提供 JS 语义的空白/标点集合；
//!    [`get_word_segments`] 按 ICU 规则引擎 + 词典引擎给出词边界。
//! 6. **数据结构**：[`KillRing`] 与 [`UndoStack`] 是纯内存容器，不做 IO。
//!
//! ## 贯穿全文件的约定
//!
//! - 所有“列”都是**终端显示列**（从 0 起），既不是字节偏移也不是 UTF-16 偏移。唯一的
//!   例外是模块开头列出的、对外承诺 UTF-16 单位的 API（[`WordSegment::index`]、
//!   [`find_word_backward`]/[`find_word_forward`] 的 `cursor` 与返回值）。
//! - “可见文本”指 [`ansi_code_length`] 识别出的序列之外的字节。未识别的 ESC 序列
//!   **不会**被剥离：`visible_width("\x1b[31")` 等于 3，因为 ESC 是 Control（0 列），
//!   后面的 `[`/`3`/`1` 各占一列（`tests/visible_width.rs` 锁定该行为）。
//! - 位置参数分两类：字节型（`ansi_code_length`/`extract_ansi_code` 的 `pos`，要求落在
//!   UTF-8 字符边界上）与列号型（截断/切片/overlay 的 `start_col`/`length`，单位是显示列）。
//!   本文件的内部扫描一律按完整字符前进，不会切出半个字符。
//!
//! ## 注意事项
//!
//! - [`visible_width`] 的缓存键是**原始串**（含 ANSI），FIFO 逐出 512 项；缓存是
//!   `thread_local!`，每线程一份，调用方无需加锁。
//! - 修改 [`grapheme_width`] 的判定顺序会同时改变 `visible_width`、全部截断/换行结果、
//!   overlay 合成与编辑器列宽；`tests/visible_width.rs`、`tests/truncate_to_width.rs`、
//!   `tests/wrap_ansi.rs` 用具体数值锁定了这些行为。
//! - 私有 helper 的影响面往往大于函数名所示：`truncate_fragment_to_width` 只被
//!   [`truncate_to_width`] 的退化路径调用，`split_into_tokens_with_ansi` 只被
//!   `wrap_single_line` 调用，但改动会波及该入口的全部输出；`update_tracker_from_text`、
//!   `class_matches` 这类 helper 则被多条扫描/分段路径复用。

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::LazyLock;

use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

#[path = "text_tables.rs"]
mod text_tables;

#[path = "word_dict.rs"]
mod word_dict;

// ============================================================================
// 宽度
// ============================================================================

/// 在有序区间表里做二分查找。
fn range_contains(ranges: &[(u32, u32)], cp: u32) -> bool {
    let mut low = 0usize;
    let mut high = ranges.len();
    while low < high {
        let mid = (low + high) / 2;
        let (start, end) = ranges[mid];
        if cp < start {
            high = mid;
        } else if cp > end {
            low = mid + 1;
        } else {
            return true;
        }
    }
    false
}

/// `eastAsianWidth(cp)`：`get-east-asian-width@1.6.0`（F/W → 2，其余 → 1）。
fn east_asian_width(cp: u32) -> usize {
    // 快路径：低于两张表的最小值、或高于两张表的最大值
    let lowest = text_tables::FULLWIDTH_MIN.min(text_tables::WIDE_MIN);
    let highest = text_tables::FULLWIDTH_MAX.max(text_tables::WIDE_MAX);
    if cp < lowest || cp > highest {
        return 1;
    }
    if range_contains(text_tables::FULLWIDTH_RANGES, cp)
        || range_contains(text_tables::WIDE_RANGES, cp)
    {
        2
    } else {
        1
    }
}

/// `\p{Default_Ignorable_Code_Point}`。
fn is_default_ignorable(cp: u32) -> bool {
    range_contains(text_tables::DI_RANGES, cp)
}

/// `\p{Control}`（General_Category=Cc）。
fn is_control(cp: u32) -> bool {
    range_contains(text_tables::CONTROL_RANGES, cp)
}

/// `\p{Format}`（General_Category=Cf）。
fn is_format(cp: u32) -> bool {
    range_contains(text_tables::FORMAT_RANGES, cp)
}

/// `\p{Mark}`（General_Category=M*）。
fn is_mark(cp: u32) -> bool {
    range_contains(text_tables::MARK_RANGES, cp)
}

/// `\p{Spacing_Mark}`（General_Category=Mc）。
fn is_spacing_mark(cp: u32) -> bool {
    range_contains(text_tables::MC_RANGES, cp)
}

/// `zeroWidthRegex` 的单字符判定：`Default_Ignorable | Control | Mark | Surrogate`。
fn is_zero_width_char(cp: u32) -> bool {
    is_default_ignorable(cp) || is_control(cp) || is_mark(cp)
}

/// `nonPrintingCharRegex` 的单字符判定（比零宽多一个 `Format`）。
fn is_non_printing_char(cp: u32) -> bool {
    is_default_ignorable(cp) || is_control(cp) || is_format(cp) || is_mark(cp)
}

/// `terminalSpacingMarkRegex` 的单字符判定。
///
/// `[\p{Spacing_Mark}--[U+1734 U+302E U+302F]]` 并上显式例外列表（终端确实为其分配单元格）。
fn is_terminal_spacing_mark_char(cp: u32) -> bool {
    if is_spacing_mark(cp) {
        return !matches!(cp, 0x1734 | 0x302E | 0x302F);
    }
    matches!(
        cp,
        0x065F | 0x0F7F | 0x102B | 0x102C | 0x1031 | 0x1033..=0x1035 | 0x1038 | 0x103A..=0x103E
    )
}

/// `terminalSpacingMarkRegex.test(segment)`：整个簇都由间距标记组成且非空。
fn is_terminal_spacing_mark(segment: &str) -> bool {
    let mut any = false;
    for ch in segment.chars() {
        if !is_terminal_spacing_mark_char(ch as u32) {
            return false;
        }
        any = true;
    }
    any
}

/// `zeroWidthRegex.test(segment)`：整个簇都是零宽字符且非空。
fn is_zero_width(segment: &str) -> bool {
    let mut any = false;
    for ch in segment.chars() {
        if !is_zero_width_char(ch as u32) {
            return false;
        }
        any = true;
    }
    any
}

/// `couldBeEmoji(segment)`：RGI emoji 的廉价前置过滤。
fn could_be_emoji(segment: &str) -> bool {
    let Some(cp) = segment.chars().next().map(|c| c as u32) else {
        return false;
    };
    (0x1F000..=0x1FBFF).contains(&cp)
        || (0x2300..=0x23FF).contains(&cp)
        || (0x2600..=0x27BF).contains(&cp)
        || (0x2B50..=0x2B55).contains(&cp)
        || segment.contains('\u{FE0F}')
        || segment.encode_utf16().count() > 2
}

/// `rgiEmojiRegex.test(segment)`。
fn is_rgi_emoji(segment: &str) -> bool {
    text_tables::RGI_EMOJI.binary_search(&segment).is_ok()
}

/// `graphemeWidth(segment)`。
///
/// 依据 string-width 的算法，并加入 emoji 前置过滤、区域指示符宽 2、间距标记与
/// Thai/Lao AM 的例外；**顺序即语义**，不可重排。
///
/// 输入约定：`segment` 必须是**单个字素簇**（本文件的调用方都用 `graphemes(true)`
/// 切好）；传入多簇字符串得到的结果不具列宽语义。
/// 返回终端列数，可能为 0（纯控制/格式/标记簇）；TAB 返回 3（本库对 TAB 的统一约定，
/// 不是终端的 8 列制表位）。纯间距标记簇按 `chars().count()` 计列，因此结果可大于 2。
pub fn grapheme_width(segment: &str) -> usize {
    // 单码元（BMP 单字符）快路径
    let mut units = segment.encode_utf16();
    if let (Some(unit), None) = (units.next(), units.next()) {
        if (0x20..=0x7E).contains(&unit) {
            return 1;
        }
        if unit == 0x09 {
            return 3;
        }
    }

    // 有些标记即使没有基字也占单元格
    if is_terminal_spacing_mark(segment) {
        return segment.chars().count();
    }

    // 零宽簇
    if is_zero_width(segment) {
        return 0;
    }

    // emoji（先做廉价过滤，再查 RGI 集合）
    if could_be_emoji(segment) && is_rgi_emoji(segment) {
        return 2;
    }

    // 去掉前导的不可打印字符，取基字
    let base = segment.trim_start_matches(|ch: char| is_non_printing_char(ch as u32));
    let Some(base_cp) = base.chars().next().map(|c| c as u32) else {
        return 0;
    };

    // 区域指示符即使单独出现也按终端实际渲染宽度 2 计，避免流式渲染漂移
    if (0x1F1E6..=0x1F1FF).contains(&base_cp) {
        return 2;
    }

    let mut width = east_asian_width(base_cp);

    // `Intl.Segmenter` 可能把多个占格码点并进一个簇：Indic 辅音后的标记、半角/全角形式、
    // Thai/Lao AM 元音。
    let mut follows_mark = false;
    for ch in base.chars().skip(1) {
        let c = ch as u32;
        if is_terminal_spacing_mark_char(c) {
            width += 1;
            follows_mark = false;
        } else if is_mark(c) {
            follows_mark = true;
        } else if !is_non_printing_char(c) {
            if follows_mark || (0xFF00..=0xFFEF).contains(&c) {
                width += east_asian_width(c);
            } else if c == 0x0E33 || c == 0x0EB3 {
                width += 1;
            }
            follows_mark = false;
        }
    }

    width
}

/// `visibleWidth` 的 512 项 FIFO 缓存（键是原始串，含 ANSI）。
struct WidthCache {
    map: HashMap<String, usize>,
    order: VecDeque<String>,
}

impl WidthCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, key: &str) -> Option<usize> {
        self.map.get(key).copied()
    }

    fn insert(&mut self, key: &str, width: usize) {
        if self.map.contains_key(key) {
            self.map.insert(key.to_string(), width);
            return;
        }
        if self.map.len() >= WIDTH_CACHE_SIZE
            && let Some(first) = self.order.pop_front()
        {
            self.map.remove(&first);
        }
        self.order.push_back(key.to_string());
        self.map.insert(key.to_string(), width);
    }
}

const WIDTH_CACHE_SIZE: usize = 512;

thread_local! {
    static WIDTH_CACHE: RefCell<WidthCache> = RefCell::new(WidthCache::new());
}

/// `asciiVisibleWidth(str)`：只由可打印 ASCII、TAB 与可识别转义序列组成时返回宽度，否则 `None`。
fn ascii_visible_width(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut width = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if (0x20..=0x7E).contains(&b) {
            width += 1;
            i += 1;
        } else if b == 0x09 {
            width += 3;
            i += 1;
        } else if b == 0x1B {
            let length = ansi_code_length(s, i);
            if length == 0 {
                return None;
            }
            i += length;
        } else {
            return None;
        }
    }
    Some(width)
}

/// `visibleWidth(str)`：终端列宽（TAB=3，ANSI/OSC/APC 零宽）。
///
/// 三级路径：空串直接 0；整串只含可打印 ASCII、TAB 与**可识别**转义序列时按字节累加，
/// 既不查缓存也不做字素分段（见 `ascii_visible_width`）；其余情况先查 512 项 FIFO
/// 缓存，未命中再“展开 TAB 为 3 个空格 → 剥掉可识别序列 → 按字素簇求和”。
///
/// 未识别的 ESC 序列不会被剥离：ESC 属于 Control，[`grapheme_width`] 给它 0 列，
/// 但序列中的可打印字节照常计宽（`tests/visible_width.rs` 的
/// `treats_unterminated_escape_sequences_as_zero_width_control_characters` 锁定）。
pub fn visible_width(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }
    if let Some(width) = ascii_visible_width(s) {
        return width;
    }
    if let Some(cached) = WIDTH_CACHE.with(|cache| cache.borrow().get(s)) {
        return cached;
    }

    // 先展开 TAB，再剥掉可识别的转义序列
    let mut clean: std::borrow::Cow<'_, str> = if s.contains('\t') {
        std::borrow::Cow::Owned(s.replace('\t', "   "))
    } else {
        std::borrow::Cow::Borrowed(s)
    };
    if clean.contains('\x1b') {
        let source = clean.clone();
        let mut stripped = String::with_capacity(source.len());
        let mut copy_from = 0usize;
        let bytes = source.as_bytes();
        let mut escape_index = source.find('\x1b');
        while let Some(mut idx) = escape_index {
            let length = ansi_code_length(&source, idx);
            if length > 0 {
                stripped.push_str(&source[copy_from..idx]);
                idx += length;
                copy_from = idx;
            } else {
                idx += 1;
            }
            escape_index = if idx >= bytes.len() {
                None
            } else {
                source[idx..].find('\x1b').map(|offset| idx + offset)
            };
        }
        stripped.push_str(&source[copy_from..]);
        clean = std::borrow::Cow::Owned(stripped);
    }

    let mut width = 0usize;
    for segment in clean.graphemes(true) {
        width += grapheme_width(segment);
    }

    WIDTH_CACHE.with(|cache| cache.borrow_mut().insert(s, width));
    width
}

// ============================================================================
// ANSI / OSC / APC
// ============================================================================

/// 一个可识别的转义序列。
///
/// TS: `extractAnsiCode` 的 `{ code, length }`。`pos`/`length` 是字节偏移；对正常的
/// 全 ASCII 终端序列，数值与 TS 的 UTF-16 偏移相同（畸形 CSI 的例外见 `length` 字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsiCode {
    /// 序列原文（含 ESC 与终止符，不含后面的可见文本）。
    pub code: String,
    /// 序列的**字节**长度，等于 `code.len()`。正常的全 ASCII 终端序列，其字节长度与
    /// TS 以 UTF-16 计的长度相同；但 [`ansi_code_length`] 不校验 CSI 的参数区，
    /// 参数区含非 ASCII 字节的畸形 CSI（如 `ESC [ é m`，é = U+00E9）会被贪婪识别，
    /// 此时字节长度**不等于** UTF-16 长度。
    pub length: usize,
}

/// `ansiCodeLength(str, pos)`：从 `pos` 起的转义序列长度，无法识别时为 0。
///
/// CSI 只认 `m`/`G`/`K`/`H`/`J` 为终止符；OSC(`ESC ]`) 与 APC(`ESC _`) 只认 BEL 或 `ESC \`。
///
/// `pos` 是字节偏移，且必须落在字符边界（函数直接检查 `bytes[pos]`）；越界或该字节
/// 不是 ESC 时返回 0。CSI 的参数区**不做合法性校验**，只是顺序向后找终止字节，因此
/// `ESC [ ?25l … ESC [4m` 会被整体认成一条序列——[`AnsiCodeTracker::process`] 的注释
/// 说明了它为什么依赖这个行为。OSC/APC 找不到 BEL 或 `ESC \` 时返回 0（该序列按
/// 未识别文本处理，不参与剥离）。
pub fn ansi_code_length(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    if pos >= bytes.len() || bytes[pos] != 0x1B {
        return 0;
    }
    let next = bytes.get(pos + 1).copied();
    match next {
        Some(b'[') => {
            for (j, &byte) in bytes.iter().enumerate().skip(pos + 2) {
                if matches!(byte, 0x6D | 0x47 | 0x4B | 0x48 | 0x4A) {
                    return j + 1 - pos;
                }
            }
            0
        }
        Some(b']') | Some(b'_') => {
            for j in pos + 2..bytes.len() {
                if bytes[j] == 0x07 {
                    return j + 1 - pos;
                }
                if bytes[j] == 0x1B && bytes.get(j + 1) == Some(&b'\\') {
                    return j + 2 - pos;
                }
            }
            0
        }
        _ => 0,
    }
}

/// `extractAnsiCode(str, pos)`。
pub fn extract_ansi_code(s: &str, pos: usize) -> Option<AnsiCode> {
    let length = ansi_code_length(s, pos);
    if length == 0 {
        return None;
    }
    Some(AnsiCode {
        code: s[pos..pos + length].to_string(),
        length,
    })
}

/// `stripTerminalSequences(str)`：去掉可识别的 ANSI/OSC/APC，保留其余字节。
pub fn strip_terminal_sequences(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }
    let mut result = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        let length = ansi_code_length(s, i);
        if length > 0 {
            i += length;
            continue;
        }
        let ch = s[i..].chars().next().expect("i is a char boundary");
        result.push(ch);
        i += ch.len_utf8();
    }
    result
}

/// 一个打开的 OSC 8 超链接（`ActiveHyperlink`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveHyperlink {
    /// OSC 8 的参数段（`ESC ] 8 ; <params> ; <url> ST` 中的 `<params>`），原样保存。
    pub params: String,
    /// 链接 URL，原样保存。空 URL 表示“关闭链接”，由解析函数映射为 `None`，
    /// 因此本结构里不会出现空 `url`。
    pub url: String,
    /// 原始终止符：`"\x07"`（BEL）或 `"\x1b\\"`（ST）。**必须原样保留**。
    pub terminator: &'static str,
}

/// `parseOsc8Hyperlink(ansiCode)` 的三态：
/// `None` = 不是 OSC 8 序列；`Some(None)` = 关闭链接；`Some(Some(_))` = 打开链接。
fn parse_osc8_hyperlink(ansi_code: &str) -> Option<Option<ActiveHyperlink>> {
    let body = ansi_code.strip_prefix("\x1b]8;")?;
    let (body, terminator) = if let Some(body) = body.strip_suffix('\x07') {
        (body, "\x07")
    } else if let Some(body) = body.strip_suffix("\x1b\\") {
        (body, "\x1b\\")
    } else {
        (body, "\x1b\\")
    };
    let separator = body.find(';')?;
    let params = &body[..separator];
    let url = &body[separator + 1..];
    if url.is_empty() {
        return Some(None);
    }
    Some(Some(ActiveHyperlink {
        params: params.to_string(),
        url: url.to_string(),
        terminator,
    }))
}

/// `formatOsc8Hyperlink(hyperlink)`。
fn format_osc8_hyperlink(hyperlink: &ActiveHyperlink) -> String {
    format!(
        "\x1b]8;{};{}{}",
        hyperlink.params, hyperlink.url, hyperlink.terminator
    )
}

/// `formatOsc8Close(terminator)`。
fn format_osc8_close(terminator: &str) -> String {
    format!("\x1b]8;;{terminator}")
}

/// `getActiveOsc8Close(prefix)`：`prefix` 末尾仍打开的 OSC 8 的关闭序列（沿用原终止符）。
fn get_active_osc8_close(prefix: &str) -> String {
    if !prefix.contains("\x1b]8;") {
        return String::new();
    }
    let mut active: Option<ActiveHyperlink> = None;
    let mut i = 0usize;
    while i < prefix.len() {
        let length = ansi_code_length(prefix, i);
        if length > 0 {
            if let Some(hyperlink) = parse_osc8_hyperlink(&prefix[i..i + length]) {
                active = hyperlink;
            }
            i += length;
        } else {
            i += prefix[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    match active {
        Some(hyperlink) => format_osc8_close(hyperlink.terminator),
        None => String::new(),
    }
}

/// `THAI_LAO_AM` 的兼容分解：U+0E33 → U+0E4D U+0E32；U+0EB3 → U+0ECD U+0EB2。
fn normalize_thai_lao_am(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\u{0E33}' => out.push_str("\u{0E4D}\u{0E32}"),
            '\u{0EB3}' => out.push_str("\u{0ECD}\u{0EB2}"),
            other => out.push(other),
        }
    }
    out
}

/// `normalizeTerminalOutput(str)`：AM 兼容分解 + 展开可识别序列之外的 TAB。
///
/// 顺序：先（仅在串确实含 U+0E33/U+0EB3 时）把泰/老挝的 AM 元音做兼容分解，再把
/// **可识别转义序列之外**的 TAB 展开为 3 个空格；序列内部字节原样保留。不会修改入参。
///
/// 调用方是 `TuiBase::apply_line_resets`（`tui.rs`）：渲染前对非图片行归一化，随后追加
/// `SEGMENT_RESET`，因此这里的结果会直接进入写往终端的字节。
pub fn normalize_terminal_output(s: &str) -> String {
    let mut normalized = s.to_string();
    if normalized.contains('\u{0E33}') || normalized.contains('\u{0EB3}') {
        normalized = normalize_thai_lao_am(&normalized);
    }
    if !normalized.contains('\t') {
        return normalized;
    }
    let mut result = String::with_capacity(normalized.len());
    let mut i = 0usize;
    while i < normalized.len() {
        let length = ansi_code_length(&normalized, i);
        if length > 0 {
            result.push_str(&normalized[i..i + length]);
            i += length;
            continue;
        }
        let ch = normalized[i..]
            .chars()
            .next()
            .expect("i is a char boundary");
        if ch == '\t' {
            result.push_str("   ");
        } else {
            result.push(ch);
        }
        i += ch.len_utf8();
    }
    result
}

/// 一个显式对应 `Intl.SegmentData` 的字素段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphemeSegment<'a> {
    /// 该字素簇的原始切片（借自输入串）。
    pub segment: &'a str,
    /// UTF-16 偏移（与 `Intl.Segmenter` 的 `index` 相同）。
    pub index: usize,
}

/// `Intl.Segmenter({ granularity: "grapheme" })` 的等价物。
#[derive(Debug, Clone, Copy, Default)]
pub struct GraphemeSegmenter;

impl GraphemeSegmenter {
    /// 按字素簇切分；`index` 是 UTF-16 偏移。
    pub fn segment<'a>(&self, text: &'a str) -> impl Iterator<Item = GraphemeSegment<'a>> {
        let mut utf16_index = 0usize;
        UnicodeSegmentation::grapheme_indices(text, true).map(move |(byte_index, segment)| {
            let _ = byte_index;
            let index = utf16_index;
            utf16_index += segment.encode_utf16().count();
            GraphemeSegment { segment, index }
        })
    }
}

/// `getGraphemeSegmenter()`：共享的字素分段器（无状态，可直接取静态引用）。
pub fn get_grapheme_segmenter() -> &'static GraphemeSegmenter {
    static SEGMENTER: GraphemeSegmenter = GraphemeSegmenter;
    &SEGMENTER
}

/// `getGraphemeCellRange` 的返回值：该字素占用的终端列区间 `[start, end)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphemeCellRange {
    /// 起始显示列（含）。
    pub start: usize,
    /// 结束显示列（不含）；宽度为 2 的字素占 `[start, start + 2)`。
    pub end: usize,
}

/// `getGraphemeCellRange(line, column)`：某一可见列所属字素的列区间。
///
/// 逐段跳过可识别转义序列，再按字素簇累加列宽；返回第一个满足
/// `start <= column < end` 的可见字素。零宽字素不占列、永远不会被返回；`column`
/// 超出整行宽度时返回 `None`。TAB 在这里同样按 3 列计。
pub fn get_grapheme_cell_range(line: &str, column: usize) -> Option<GraphemeCellRange> {
    let mut current_col = 0usize;
    let mut i = 0usize;
    while i < line.len() {
        let length = ansi_code_length(line, i);
        if length > 0 {
            i += length;
            continue;
        }
        let mut text_end = i;
        while text_end < line.len() && ansi_code_length(line, text_end) == 0 {
            text_end += line[text_end..].chars().next().map_or(1, char::len_utf8);
        }
        for segment in line[i..text_end].graphemes(true) {
            let width = grapheme_width(segment);
            if width > 0 && column >= current_col && column < current_col + width {
                return Some(GraphemeCellRange {
                    start: current_col,
                    end: current_col + width,
                });
            }
            current_col += width;
        }
        i = text_end;
    }
    None
}

/// `getOsc8LinkAtColumn` 内联用的 `^\x1b\]8;[^;]*;([^\x07\x1b]*)(?:\x07|\x1b\\)$`。
///
/// 与 [`parse_osc8_hyperlink`]（tracker 用）**不同**：这条正则的 URL 段是 `[^\x07\x1b]*`，
/// 因此 URL 中含 BEL/ESC 的畸形 OSC 8 不匹配、**不改动**当前链接（TS 就是这么写的；
/// 若改用 `indexOf` 式解析，畸形序列会覆盖掉前一个有效链接）。
fn parse_osc8_hyperlink_anchored(ansi_code: &str) -> Option<String> {
    let body = ansi_code.strip_prefix("\x1b]8;")?;
    let body = if let Some(body) = body.strip_suffix('\x07') {
        body
    } else {
        body.strip_suffix("\x1b\\")?
    };
    let separator = body.find(';')?;
    let url = &body[separator + 1..];
    // `[^\x07\x1b]*`：URL 里出现 BEL 或 ESC 就整体不匹配。
    if url.contains(['\x07', '\x1b']) {
        return None;
    }
    // `activeUrl = hyperlink[1] || undefined`：空 URL 关闭链接。
    Some(url.to_string())
}

/// `getOsc8LinkAtColumn(line, column)`：覆盖该可见列的 OSC 8 URL。
///
/// 只认 `ESC ] 8;…` 形式；空 URL 关闭链接，TAB 按 3 列计。与 tracker 的解析不同，
/// 这里用一条更严格的“锚定”解析（见 `parse_osc8_hyperlink_anchored`）：URL 段含
/// BEL/ESC 的畸形序列会被忽略，**不会**覆盖此前仍有效的链接。
pub fn get_osc8_link_at_column(line: &str, column: usize) -> Option<String> {
    let mut active_url: Option<String> = None;
    let mut current_col = 0usize;
    let mut i = 0usize;
    while i < line.len() {
        let length = ansi_code_length(line, i);
        if length > 0 {
            if let Some(url) = parse_osc8_hyperlink_anchored(&line[i..i + length]) {
                active_url = if url.is_empty() { None } else { Some(url) };
            }
            i += length;
            continue;
        }
        let mut text_end = i;
        while text_end < line.len() && ansi_code_length(line, text_end) == 0 {
            text_end += line[text_end..].chars().next().map_or(1, char::len_utf8);
        }
        for segment in line[i..text_end].graphemes(true) {
            let width = if segment == "\t" {
                3
            } else {
                grapheme_width(segment)
            };
            if column >= current_col && column < current_col + width {
                return active_url;
            }
            current_col += width;
        }
        i = text_end;
    }
    None
}

// ============================================================================
// AnsiCodeTracker：跨行样式状态
// ============================================================================

/// 跨行保持 SGR 与 OSC 8 状态的追踪器。
///
/// TS: `utils.ts#AnsiCodeTracker`。属性逐个跟踪，以便行末只重置需要重置的部分。
#[derive(Debug, Clone, Default)]
pub struct AnsiCodeTracker {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
    /// 完整代码，如 `"31"` 或 `"38;5;240"`。
    fg_color: Option<String>,
    /// 完整代码，如 `"41"` 或 `"48;5;240"`。
    bg_color: Option<String>,
    active_hyperlink: Option<ActiveHyperlink>,
}

impl AnsiCodeTracker {
    /// 空状态：无任何 SGR 属性、无活动链接。
    pub fn new() -> Self {
        Self::default()
    }

    /// `process(ansiCode)`：吞入一个转义序列并更新状态。
    ///
    /// 只处理两类输入：以 `ESC ] 8;` 开头的 OSC 8（打开或关闭链接），以及末尾为 `m`
    /// 的 CSI SGR。其他序列直接忽略、状态不变。SGR 参数按 TS 的**未锚定**正则解析：
    /// 即使传入的“码”是 [`ansi_code_length`] 把多条控制序列吞成的一整段，也会取其中
    /// 第一个 `ESC [ <数字;> m`。参数为空或为 `0` 时只清 SGR，**不动** OSC 8 链接。
    pub fn process(&mut self, ansi_code: &str) {
        // OSC 8 超链接：打开或关闭。保留原终止符（部分终端只让 BEL 终止的链接可点击）。
        if let Some(hyperlink) = parse_osc8_hyperlink(ansi_code) {
            self.active_hyperlink = hyperlink;
            return;
        }

        if !ansi_code.ends_with('m') {
            return;
        }

        // TS: `ansiCode.match(/\x1b\[([\d;]*)m/)` —— **未锚定**，且会在失败后继续尝试后续的
        // `\x1b[`。传给 `process` 的"码"有时会吞掉后面的可见文本再遇到某个 `m`
        // （如 `\x1b[?25l…\x1b[4m` 整体被 `ansiCodeLength` 认成一个 CSI），
        // 此时仍应命中其中第一个 `ESC [ <数字;> m`，不能按"整串必须是 SGR"处理。
        let params = {
            let mut search_from = 0usize;
            loop {
                let start = match ansi_code[search_from..].find("\x1b[") {
                    None => return,
                    Some(offset) => search_from + offset,
                };
                let rest = &ansi_code[start + 2..];
                let end = rest
                    .bytes()
                    .position(|b| !(b.is_ascii_digit() || b == b';'))
                    .unwrap_or(rest.len());
                if rest.as_bytes().get(end) == Some(&b'm') {
                    break &rest[..end];
                }
                search_from = start + 2;
            }
        };

        if params.is_empty() || params == "0" {
            self.reset();
            return;
        }

        let parts: Vec<&str> = params.split(';').collect();
        let mut i = 0usize;
        while i < parts.len() {
            let code = parse_js_int(parts[i]);

            // 256 色与 RGB 代码会消耗多个参数
            if code == Some(38) || code == Some(48) {
                if parts.get(i + 1) == Some(&"5") && parts.get(i + 2).is_some() {
                    let color_code = format!("{};{};{}", parts[i], parts[i + 1], parts[i + 2]);
                    if code == Some(38) {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    i += 3;
                    continue;
                } else if parts.get(i + 1) == Some(&"2") && parts.get(i + 4).is_some() {
                    let color_code = format!(
                        "{};{};{};{};{}",
                        parts[i],
                        parts[i + 1],
                        parts[i + 2],
                        parts[i + 3],
                        parts[i + 4]
                    );
                    if code == Some(38) {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    i += 5;
                    continue;
                }
            }

            match code {
                Some(0) => self.reset(),
                Some(1) => self.bold = true,
                Some(2) => self.dim = true,
                Some(3) => self.italic = true,
                Some(4) => self.underline = true,
                Some(5) => self.blink = true,
                Some(7) => self.inverse = true,
                Some(8) => self.hidden = true,
                Some(9) => self.strikethrough = true,
                Some(21) => self.bold = false,
                Some(22) => {
                    self.bold = false;
                    self.dim = false;
                }
                Some(23) => self.italic = false,
                Some(24) => self.underline = false,
                Some(25) => self.blink = false,
                Some(27) => self.inverse = false,
                Some(28) => self.hidden = false,
                Some(29) => self.strikethrough = false,
                Some(39) => self.fg_color = None,
                Some(49) => self.bg_color = None,
                Some(c) => {
                    if (30..=37).contains(&c) || (90..=97).contains(&c) {
                        self.fg_color = Some(c.to_string());
                    } else if (40..=47).contains(&c) || (100..=107).contains(&c) {
                        self.bg_color = Some(c.to_string());
                    }
                }
                None => {}
            }
            i += 1;
        }
    }

    /// `reset()`（私有）：清空 SGR 状态，**不动** OSC 8 链接。
    fn reset(&mut self) {
        self.bold = false;
        self.dim = false;
        self.italic = false;
        self.underline = false;
        self.blink = false;
        self.inverse = false;
        self.hidden = false;
        self.strikethrough = false;
        self.fg_color = None;
        self.bg_color = None;
    }

    /// 清空全部状态以便复用。
    pub fn clear(&mut self) {
        self.reset();
        self.active_hyperlink = None;
    }

    /// `getActiveCodes()`：行首需要重新打开的序列（SGR 在前，OSC 8 在后）。
    ///
    /// SGR 参数按固定顺序拼接：粗体、暗色、斜体、下划线、闪烁、反显、隐藏、删除线、
    /// 前景色、背景色；颜色使用保存时的完整代码（如 `38;5;240`）。没有 SGR 时只输出
    /// 链接的打开序列，两者都没有则返回空串。
    pub fn get_active_codes(&self) -> String {
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".to_string());
        }
        if self.dim {
            codes.push("2".to_string());
        }
        if self.italic {
            codes.push("3".to_string());
        }
        if self.underline {
            codes.push("4".to_string());
        }
        if self.blink {
            codes.push("5".to_string());
        }
        if self.inverse {
            codes.push("7".to_string());
        }
        if self.hidden {
            codes.push("8".to_string());
        }
        if self.strikethrough {
            codes.push("9".to_string());
        }
        if let Some(fg) = &self.fg_color {
            codes.push(fg.clone());
        }
        if let Some(bg) = &self.bg_color {
            codes.push(bg.clone());
        }

        let mut result = if codes.is_empty() {
            String::new()
        } else {
            format!("\x1b[{}m", codes.join(";"))
        };
        if let Some(hyperlink) = &self.active_hyperlink {
            result.push_str(&format_osc8_hyperlink(hyperlink));
        }
        result
    }

    /// `getActiveBackgroundCode()`。
    pub fn get_active_background_code(&self) -> String {
        match &self.bg_color {
            Some(bg) => format!("\x1b[{bg}m"),
            None => String::new(),
        }
    }

    /// `hasActiveCodes()`。
    pub fn has_active_codes(&self) -> bool {
        self.bold
            || self.dim
            || self.italic
            || self.underline
            || self.blink
            || self.inverse
            || self.hidden
            || self.strikethrough
            || self.fg_color.is_some()
            || self.bg_color.is_some()
            || self.active_hyperlink.is_some()
    }

    /// `getLineEndReset()`：行末关闭下划线（防止渗到 padding）并关闭 OSC 8（下一行会重开）。
    pub fn get_line_end_reset(&self) -> String {
        let mut result = String::new();
        if self.underline {
            result.push_str("\x1b[24m");
        }
        if let Some(hyperlink) = &self.active_hyperlink {
            result.push_str(&format_osc8_close(hyperlink.terminator));
        }
        result
    }
}

/// JS `Number.parseInt(text, 10)` 在 SGR 参数上的行为：非数字（含空串）→ `None`（NaN）。
fn parse_js_int(text: &str) -> Option<i64> {
    let trimmed = text.trim_start_matches([' ', '\t', '\n', '\r', '\u{0B}', '\u{0C}']);
    let mut value: i64 = 0;
    let mut any = false;
    for ch in trimmed.chars() {
        let Some(digit) = ch.to_digit(10) else { break };
        value = value.saturating_mul(10).saturating_add(i64::from(digit));
        any = true;
    }
    if any { Some(value) } else { None }
}

/// `updateTrackerFromText(text, tracker)`：扫描文本中的可识别转义序列并送入追踪器。
fn update_tracker_from_text(text: &str, tracker: &mut AnsiCodeTracker) {
    let mut i = text.find('\x1b');
    while let Some(index) = i {
        let length = ansi_code_length(text, index);
        if length > 0 {
            tracker.process(&text[index..index + length]);
            i = text[index + length..]
                .find('\x1b')
                .map(|offset| index + length + offset);
        } else {
            i = text[index + 1..]
                .find('\x1b')
                .map(|offset| index + 1 + offset);
        }
    }
}

/// `getActiveBackgroundAnsi(text)`：文本末尾仍生效的背景色 ANSI（无则空串）。
///
/// 扫描 `text` 中所有可识别序列并交给 [`AnsiCodeTracker::process`]，返回其
/// `get_active_background_code()`：有背景色时是重开该颜色的完整 CSI（如 `\x1b[44m`），
/// 否则空串。只保留最后的状态，不修改文本。
pub fn get_active_background_ansi(text: &str) -> String {
    let mut tracker = AnsiCodeTracker::new();
    update_tracker_from_text(text, &mut tracker);
    tracker.get_active_background_code()
}

// ============================================================================
// 截断
// ============================================================================

/// `isPrintableAscii(str)`。
fn is_printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

/// `truncateFragmentToWidth` 的返回值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncatedFragment {
    /// 截断后的文本（保留被保留部分的 ANSI 序列）。
    pub text: String,
    /// 上述文本的可见列宽（不含 ANSI）。
    pub width: usize,
}

/// `truncateFragmentToWidth(text, maxWidth)`：按字素截断，保留 ANSI/TAB。
///
/// 只用于“省略号本身不比 `maxWidth` 窄”的退化路径（[`truncate_to_width`] 的
/// `ellipsis_width >= max_width` 分支）：返回**不带**省略号、也不追加 reset 的连续前缀。
///
/// 语义：从左往右整字素地收，遇到第一个放不下（`width + w > max_width`）的字素就停，
/// 不会跳过它再去收后面更窄的字素。TAB 记 3 列。ANSI 序列先进入 `pending_ansi`，
/// 只在下一个可见字素确定被保留时才一并输出；因此切点之后的控制序列不会进入结果。
fn truncate_fragment_to_width(text: &str, max_width: usize) -> TruncatedFragment {
    if max_width == 0 || text.is_empty() {
        return TruncatedFragment {
            text: String::new(),
            width: 0,
        };
    }
    if is_printable_ascii(text) {
        let clipped: String = text.chars().take(max_width).collect();
        let width = clipped.len();
        return TruncatedFragment {
            text: clipped,
            width,
        };
    }

    let has_ansi = text.contains('\x1b');
    let has_tabs = text.contains('\t');
    if !has_ansi && !has_tabs {
        let mut result = String::new();
        let mut width = 0usize;
        for segment in text.graphemes(true) {
            let w = grapheme_width(segment);
            if width + w > max_width {
                break;
            }
            result.push_str(segment);
            width += w;
        }
        return TruncatedFragment {
            text: result,
            width,
        };
    }

    let mut result = String::new();
    let mut width = 0usize;
    let mut i = 0usize;
    let mut pending_ansi = String::new();
    while i < text.len() {
        let length = ansi_code_length(text, i);
        if length > 0 {
            pending_ansi.push_str(&text[i..i + length]);
            i += length;
            continue;
        }
        if text[i..].starts_with('\t') {
            if width + 3 > max_width {
                break;
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push('\t');
            width += 3;
            i += 1;
            continue;
        }

        let mut end = i;
        while end < text.len() && !text[end..].starts_with('\t') {
            if ansi_code_length(text, end) > 0 {
                break;
            }
            end += text[end..].chars().next().map_or(1, char::len_utf8);
        }

        for segment in text[i..end].graphemes(true) {
            let w = grapheme_width(segment);
            if width + w > max_width {
                return TruncatedFragment {
                    text: result,
                    width,
                };
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push_str(segment);
            width += w;
        }
        i = end;
    }
    TruncatedFragment {
        text: result,
        width,
    }
}

/// `finalizeTruncatedResult(prefix, prefixWidth, ellipsis, ellipsisWidth, maxWidth, pad)`。
///
/// 输出形状固定为 `prefix + OSC8 关闭 + "\x1b[0m" + ellipsis + "\x1b[0m"`，再按需补空格；
/// `ellipsis` 为空时只输出一次 reset。OSC8 关闭序列取自 `prefix`
/// 末尾仍打开的链接，并沿用其原始终止符。`pad` 的补空量按可见宽
/// `prefix_width + ellipsis_width` 计算（转义序列不计入），超过 `max_width` 时补 0 个。
fn finalize_truncated_result(
    prefix: &str,
    prefix_width: usize,
    ellipsis: &str,
    ellipsis_width: usize,
    max_width: usize,
    pad: bool,
) -> String {
    const RESET: &str = "\x1b[0m";
    let hyperlink_close = get_active_osc8_close(prefix);
    let visible = prefix_width + ellipsis_width;
    let mut result = if !ellipsis.is_empty() {
        format!("{prefix}{hyperlink_close}{RESET}{ellipsis}{RESET}")
    } else {
        format!("{prefix}{hyperlink_close}{RESET}")
    };
    if pad {
        result.push_str(&" ".repeat(max_width.saturating_sub(visible)));
    }
    result
}

/// `truncateToWidth(text, maxWidth, ellipsis = "...", pad = false)`。
///
/// 截断时先关闭 OSC 8 与全部 SGR，再输出省略号，最后再补一次 reset。
///
/// - `max_width`：目标显示列宽；为 0 时返回空串。
/// - `ellipsis`：省略号文本，宽度按 [`visible_width`] 计算，允许为空串。
/// - `pad`：为 `true` 时用空格把结果补齐到 `max_width` 列。
///
/// 只有 `visible_width(text) > max_width` 时才真正截断；否则原样返回（`pad` 时补空格），
/// **不会**追加 reset 或省略号。真正截断的输出形状见 `finalize_truncated_result`；
/// 例外是省略号被裁到 0 宽的情形：不输出任何 reset 或省略号，`pad` 时返回 `max_width`
/// 个空格，否则返回空串（见 `truncate_to_width_impl` 第 3 步）。
pub fn truncate_to_width(text: &str, max_width: usize, ellipsis: &str, pad: bool) -> String {
    truncate_to_width_impl(text, max_width as i64, ellipsis, pad)
}

/// `maxWidth <= 0` 的判断需要 `isize`（TS 可传负数）。
///
/// 分支顺序不可随意合并（每个分支处理一种退化输入）：
/// 1. `max_width <= 0` → 空串（即使 `pad` 也不补）。
/// 2. 空文本 → 仅在 `pad` 时返回 `max_width` 个空格。
/// 3. 省略号不比 `max_width` 窄：文本本身放得下就原样返回/补空；否则把省略号自身用
///    `truncate_fragment_to_width` 裁到 `max_width`。若裁剪后宽度为 0（例如省略号是宽 2
///    的 `界` 而 `max_width` 为 1），则不经过 `finalize_truncated_result`，而是 `pad`
///    时返回 `max_width` 个空格、否则返回空串；只有裁剪后仍有宽度才交给
///    `finalize_truncated_result`。
/// 4. 纯可打印 ASCII：每个字符一列，直接按字符数取 `max_width - ellipsis_width` 个。
/// 5. 一般情况：边扫描边累计两个量——`kept_width`（连续前缀的宽度）与
///    `visible_so_far`（扫过的总宽度）。只有 `kept_width + w <= target_width` 的字素
///    才进入结果；一旦某个字素放不下就停止收取（`keep_contiguous_prefix` 置 false），
///    不会跳过它再拼后面的窄字素。扫描仍继续到总宽度超过 `max_width`，用于识别
///    “整串其实放得下”的情况——那时返回原文而不是截断结果。
fn truncate_to_width_impl(text: &str, max_width: i64, ellipsis: &str, pad: bool) -> String {
    if max_width <= 0 {
        return String::new();
    }
    let max_width = usize::try_from(max_width).expect("checked positive");

    if text.is_empty() {
        return if pad {
            " ".repeat(max_width)
        } else {
            String::new()
        };
    }

    let ellipsis_width = visible_width(ellipsis);
    if ellipsis_width >= max_width {
        let text_width = visible_width(text);
        if text_width <= max_width {
            return if pad {
                let mut out = text.to_string();
                out.push_str(&" ".repeat(max_width - text_width));
                out
            } else {
                text.to_string()
            };
        }
        let clipped = truncate_fragment_to_width(ellipsis, max_width);
        if clipped.width == 0 {
            return if pad {
                " ".repeat(max_width)
            } else {
                String::new()
            };
        }
        return finalize_truncated_result("", 0, &clipped.text, clipped.width, max_width, pad);
    }

    if is_printable_ascii(text) {
        if text.len() <= max_width {
            return if pad {
                let mut out = text.to_string();
                out.push_str(&" ".repeat(max_width - text.len()));
                out
            } else {
                text.to_string()
            };
        }
        let target_width = max_width - ellipsis_width;
        let prefix: String = text.chars().take(target_width).collect();
        return finalize_truncated_result(
            &prefix,
            target_width,
            ellipsis,
            ellipsis_width,
            max_width,
            pad,
        );
    }

    let target_width = max_width - ellipsis_width;
    let mut result = String::new();
    let mut pending_ansi = String::new();
    let mut visible_so_far = 0usize;
    let mut kept_width = 0usize;
    let mut keep_contiguous_prefix = true;
    let mut overflowed = false;
    let has_ansi = text.contains('\x1b');
    let has_tabs = text.contains('\t');

    // 两个分支共享同一套收字规则；没有 ANSI/TAB 时省掉逐字节扫描，直接按字素走。
    let exhausted_input = if !has_ansi && !has_tabs {
        for segment in text.graphemes(true) {
            let width = grapheme_width(segment);
            if keep_contiguous_prefix && kept_width + width <= target_width {
                result.push_str(segment);
                kept_width += width;
            } else {
                keep_contiguous_prefix = false;
            }
            visible_so_far += width;
            if visible_so_far > max_width {
                overflowed = true;
                break;
            }
        }
        !overflowed
    } else {
        let mut i = 0usize;
        while i < text.len() {
            let length = ansi_code_length(text, i);
            if length > 0 {
                pending_ansi.push_str(&text[i..i + length]);
                i += length;
                continue;
            }

            if text[i..].starts_with('\t') {
                if keep_contiguous_prefix && kept_width + 3 <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push('\t');
                    kept_width += 3;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += 3;
                if visible_so_far > max_width {
                    overflowed = true;
                    break;
                }
                i += 1;
                continue;
            }

            let mut end = i;
            while end < text.len() && !text[end..].starts_with('\t') {
                if ansi_code_length(text, end) > 0 {
                    break;
                }
                end += text[end..].chars().next().map_or(1, char::len_utf8);
            }

            for segment in text[i..end].graphemes(true) {
                let width = grapheme_width(segment);
                if keep_contiguous_prefix && kept_width + width <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push_str(segment);
                    kept_width += width;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += width;
                if visible_so_far > max_width {
                    overflowed = true;
                    break;
                }
            }
            if overflowed {
                break;
            }
            i = end;
        }
        i >= text.len()
    };

    // 扫描完仍没超过 max_width：整串其实放得下，原样返回（不产生省略号）。
    if !overflowed && exhausted_input {
        return if pad {
            let mut out = text.to_string();
            out.push_str(&" ".repeat(max_width.saturating_sub(visible_so_far)));
            out
        } else {
            text.to_string()
        };
    }

    // 放不下：用连续前缀 + 省略号收尾（前缀可能短于 target_width，见上面的收字规则）。
    finalize_truncated_result(
        &result,
        kept_width,
        ellipsis,
        ellipsis_width,
        max_width,
        pad,
    )
}

/// `truncateToWidth` 的默认省略号 `"..."` 与 `pad = false`。
pub fn truncate_to_width_default(text: &str, max_width: usize) -> String {
    truncate_to_width(text, max_width, "...", false)
}

// ============================================================================
// 换行
// ============================================================================

// ============================================================================
// 换行
// ============================================================================

/// `splitIntoTokensWithAnsi(text)` 的 token 类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Space,
    Word,
}

/// `splitIntoTokensWithAnsi(text)`：按"空白 / 非空白"切 token。
///
/// 只有 ASCII 空格是 `space`；TAB 属于 `word`（TS 逐字符比较 `segment === " "`）。
/// 非 ASCII 文本里，落在 CJK 脚本的字素各自成独立 token（因此换行可在任意汉字/假名/谚文边界发生）。
/// ANSI 序列缓存在 `pending_ansi`，附着到下一个可见字符上。
///
/// 不变量：把所有 token 按顺序拼接会逐字节还原输入；ANSI 只是被移到其后的第一个可见
/// 字符之前，不丢弃、不重排。文本末尾残留的 ANSI 附着到最后一个 token；若整段文本
/// 没有任何可见字符，它自成唯一 token。
fn split_into_tokens_with_ansi(text: &str) -> Vec<String> {
    fn flush(current: &mut String, current_kind: &mut Option<TokenKind>, tokens: &mut Vec<String>) {
        if current.is_empty() {
            return;
        }
        tokens.push(std::mem::take(current));
        *current_kind = None;
    }

    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut pending_ansi = String::new();
    let mut current_kind: Option<TokenKind> = None;
    let mut i = 0usize;

    while i < text.len() {
        let length = ansi_code_length(text, i);
        if length > 0 {
            pending_ansi.push_str(&text[i..i + length]);
            i += length;
            continue;
        }

        // 可见文本一直延伸到下一个可识别转义序列
        let mut end = i;
        loop {
            match text[end..].find('\x1b').map(|offset| end + offset) {
                Some(index) if ansi_code_length(text, index) == 0 => {
                    end = index + text[index..].chars().next().map_or(1, char::len_utf8);
                }
                Some(index) => {
                    end = index;
                    break;
                }
                None => {
                    end = text.len();
                    break;
                }
            }
        }

        let chunk = &text[i..end];
        // 可打印 ASCII 每个字符都是单字素，跳过分段器
        let ascii = is_printable_ascii(chunk);
        if ascii {
            for ch in chunk.chars() {
                let segment_is_space = ch == ' ';
                let segment_kind = if segment_is_space {
                    TokenKind::Space
                } else {
                    TokenKind::Word
                };
                if !current.is_empty() && current_kind != Some(segment_kind) {
                    flush(&mut current, &mut current_kind, &mut tokens);
                }
                if !pending_ansi.is_empty() {
                    current.push_str(&pending_ansi);
                    pending_ansi.clear();
                }
                current_kind = Some(segment_kind);
                current.push(ch);
            }
        } else {
            for segment in chunk.graphemes(true) {
                let segment_is_space = segment == " ";
                if !segment_is_space && cjk_break_matches(segment) {
                    flush(&mut current, &mut current_kind, &mut tokens);
                    let mut token = std::mem::take(&mut pending_ansi);
                    token.push_str(segment);
                    tokens.push(token);
                    continue;
                }

                let segment_kind = if segment_is_space {
                    TokenKind::Space
                } else {
                    TokenKind::Word
                };
                if !current.is_empty() && current_kind != Some(segment_kind) {
                    flush(&mut current, &mut current_kind, &mut tokens);
                }
                if !pending_ansi.is_empty() {
                    current.push_str(&pending_ansi);
                    pending_ansi.clear();
                }
                current_kind = Some(segment_kind);
                current.push_str(segment);
            }
        }

        i = end;
    }

    // 残余 ANSI 附着到最后一个 token（没有可见字符时作为独立 token）
    if !pending_ansi.is_empty() {
        if !current.is_empty() {
            current.push_str(&pending_ansi);
        } else if let Some(last) = tokens.last_mut() {
            last.push_str(&pending_ansi);
        } else {
            current = std::mem::take(&mut pending_ansi);
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

/// `token.trim() === ""`（JS `trim` 的空白集合）。
fn is_js_blank(text: &str) -> bool {
    !text.chars().any(|ch| !is_js_whitespace(ch))
}

/// `wrapSingleLine(line, width)`。
///
/// 输入是**一条逻辑行**（不含换行符），输出是若干条显示行。`width` 是显示列数。
///
/// 流程：
/// 1. 空行返回一个空串；整行已放得下则原样返回（这条快路径不做行尾裁剪）。
/// 2. 用 `split_into_tokens_with_ansi` 切成空白/单词 token，同时用 `AnsiCodeTracker`
///    重放 token 里的样式。
/// 3. 单个非空白 token 比 `width` 还宽：先结束当前行（补 `get_line_end_reset`），
///    再交给 `break_long_word` 按字素硬断，除最后一段外都直接成为整行。
/// 4. 常规 token：加上去会超宽且当前行非空时，先 `trim_end_js` 再补行末 reset 后收行；
///    新行以 `get_active_codes()` 开头。若当前 token 是空白，则不放到新行行首
///    （避免行首空白），否则把 token 接到样式前缀之后。
/// 5. 最后一行不补 reset（由调用方负责），全部行再做一次 `trim_end_js`。
fn wrap_single_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    if visible_width(line) <= width {
        return vec![line.to_string()];
    }

    let mut wrapped: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();
    let tokens = split_into_tokens_with_ansi(line);

    let mut current_line = String::new();
    let mut current_visible_length = 0usize;

    for token in &tokens {
        let token_visible_length = visible_width(token);
        let is_whitespace = is_js_blank(token);

        // token 自身超宽：逐字素断开
        if token_visible_length > width && !is_whitespace {
            if !current_line.is_empty() {
                let line_end_reset = tracker.get_line_end_reset();
                if !line_end_reset.is_empty() {
                    current_line.push_str(&line_end_reset);
                }
                wrapped.push(std::mem::take(&mut current_line));
            }
            let broken = break_long_word(token, width, &mut tracker);
            let last = broken.len().saturating_sub(1);
            for line in &broken[..last] {
                wrapped.push(line.clone());
            }
            current_line = broken[last].clone();
            current_visible_length = visible_width(&current_line);
            continue;
        }

        let total_needed = current_visible_length + token_visible_length;
        if total_needed > width && current_visible_length > 0 {
            // 去掉行尾空白，再加"只关下划线"的 reset（保留背景色）
            let mut line_to_wrap = trim_end_js(&current_line).to_string();
            let line_end_reset = tracker.get_line_end_reset();
            if !line_end_reset.is_empty() {
                line_to_wrap.push_str(&line_end_reset);
            }
            wrapped.push(line_to_wrap);
            if is_whitespace {
                // 新行不以空白开头
                current_line = tracker.get_active_codes();
                current_visible_length = 0;
            } else {
                current_line = tracker.get_active_codes();
                current_line.push_str(token);
                current_visible_length = token_visible_length;
            }
        } else {
            current_line.push_str(token);
            current_visible_length += token_visible_length;
        }

        update_tracker_from_text(token, &mut tracker);
    }

    if !current_line.is_empty() {
        // 最后一行不加 reset，交给调用方
        wrapped.push(current_line);
    }

    if wrapped.is_empty() {
        return vec![String::new()];
    }
    // 行尾空白可能让行超出请求宽度
    wrapped
        .into_iter()
        .map(|line| trim_end_js(&line).to_string())
        .collect()
}

/// `breakLongWord(word, width, tracker)`。
///
/// 把超宽 token 按字素硬断成多行：每个字素用 [`visible_width`] 计宽，放不下就收行。
/// 由于字素不可再拆，单个字素本身比 `width` 宽时会独占一行并**超出** `width`。
///
/// `tracker` 是进入本 token 时的样式状态；函数会就地处理 token 内的 ANSI 并推进它，
/// 返回后调用方继续用它处理后续 token。每次断行先输出 `get_line_end_reset()`（只关
/// 下划线与 OSC 8），新行行首再用 `get_active_codes()` 重开全部样式；最后一行不补 reset。
fn break_long_word(word: &str, width: usize, tracker: &mut AnsiCodeTracker) -> Vec<String> {
    #[derive(Debug)]
    enum Piece<'a> {
        Ansi(&'a str),
        Grapheme(&'a str),
    }

    let mut lines: Vec<String> = Vec::new();
    let mut current_line = tracker.get_active_codes();
    let mut current_width = 0usize;

    // 先把 ANSI 与可见内容分开
    let mut pieces: Vec<Piece<'_>> = Vec::new();
    let mut i = 0usize;
    while i < word.len() {
        let length = ansi_code_length(word, i);
        if length > 0 {
            pieces.push(Piece::Ansi(&word[i..i + length]));
            i += length;
        } else {
            let mut end = i;
            while end < word.len() && ansi_code_length(word, end) == 0 {
                end += word[end..].chars().next().map_or(1, char::len_utf8);
            }
            for segment in word[i..end].graphemes(true) {
                pieces.push(Piece::Grapheme(segment));
            }
            i = end;
        }
    }

    for piece in pieces {
        match piece {
            Piece::Ansi(code) => {
                current_line.push_str(code);
                tracker.process(code);
            }
            Piece::Grapheme(grapheme) => {
                if grapheme.is_empty() {
                    continue;
                }
                let grapheme_w = visible_width(grapheme);
                if current_width + grapheme_w > width {
                    let line_end_reset = tracker.get_line_end_reset();
                    if !line_end_reset.is_empty() {
                        current_line.push_str(&line_end_reset);
                    }
                    lines.push(std::mem::take(&mut current_line));
                    current_line = tracker.get_active_codes();
                    current_width = 0;
                }
                current_line.push_str(grapheme);
                current_width += grapheme_w;
            }
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

/// `wrapTextWithAnsi(text, width)`：按词换行，保留 ANSI 与 OSC 8 状态（不做 padding）。
///
/// 先把输入按 `\r\n|\r|\n` 切成逻辑行（见 `split_lines_js`，尾随换行会留下一个空行），
/// 再逐行交给 `wrap_single_line`。逻辑行之间的 SGR/OSC 8 状态由 `AnsiCodeTracker`
/// 携带：第 n 行的样式前缀来自前面所有逻辑行扫描后的活动状态。返回至少一行
/// （空输入返回 `vec![String::new()]`）；不追加尾部 reset，也不做 padding。
pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    // 先按换行符切行，再逐行换行；样式跨行携带
    let input_lines = split_lines_js(text);
    let mut result: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();

    for input_line in input_lines {
        let prefix = if result.is_empty() {
            String::new()
        } else {
            tracker.get_active_codes()
        };
        let mut prefixed = prefix;
        prefixed.push_str(input_line);
        for wrapped_line in wrap_single_line(&prefixed, width) {
            result.push(wrapped_line);
        }
        update_tracker_from_text(input_line, &mut tracker);
    }

    if result.is_empty() {
        vec![String::new()]
    } else {
        result
    }
}

/// `text.split(/\r\n|\r|\n/)`。
///
/// 返回借自输入的切片，至少一个元素：空串得到 `[""]`；输入以换行结尾时最后一个元素
/// 是空串；`\r\n` 视为一个分隔符。`\r` 与 `\n` 之外没有其他行分隔符。
fn split_lines_js(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                lines.push(&text[start..i]);
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 2;
                } else {
                    i += 1;
                }
                start = i;
            }
            b'\n' => {
                lines.push(&text[start..i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    lines.push(&text[start..]);
    lines
}

/// `applyBackgroundToLine(line, width, bgFn)`：补齐到 `width` 后交给 `bg_fn` 包背景色。
///
/// 补空量是 `width - visible_width(line)`（用显示列，不用字节数；已超宽时为 0）。
/// `bg_fn` 收到的是**补好空格之后**的整行，其返回值被原样采用——是否加背景色序列与
/// reset 完全由 `bg_fn` 决定。`Text` 组件（`components/text_component.rs`，TS `class Text`）与
/// markdown 组件都用它给内容行和上下留白统一上背景。
pub fn apply_background_to_line(
    line: &str,
    width: usize,
    bg_fn: &dyn Fn(&str) -> String,
) -> String {
    let visible_len = visible_width(line);
    let padding_needed = width.saturating_sub(visible_len);
    let mut with_padding = line.to_string();
    with_padding.push_str(&" ".repeat(padding_needed));
    bg_fn(&with_padding)
}

// ============================================================================
// 切片
// ============================================================================

/// `sliceWithWidth` 的返回值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceResult {
    /// 切片文本：包含落在列区间内的可见字素及其 ANSI；若结果非空，还包含起点之前
    /// 缓存下来的 ANSI（用于恢复切片开始处的样式）。
    pub text: String,
    /// 结果实际包含的可见列宽，可能小于请求的 `length`（行不够长或 strict 排除了
    /// 边界字素）；非 strict 模式下，跨右边界的字素会让它比 `length` 最多多出
    /// “该字素宽度 - 1”列——[`grapheme_width`] 对纯间距标记簇按字符数计列，字素宽度
    /// 可大于 2，因此可能不止多 1 列。
    pub width: usize,
}

/// `sliceByColumn(line, startCol, length, strict = false)`。
///
/// 参数与 [`slice_with_width`] 相同，只返回文本、丢弃宽度（需要列宽时用后者）。
pub fn slice_by_column(line: &str, start_col: usize, length: usize, strict: bool) -> String {
    slice_with_width(line, start_col, length, strict).text
}

/// `sliceWithWidth(line, startCol, length, strict = false)`：取一段可见列。
///
/// 起始列之前的 ANSI 会按原顺序前置到结果里（issue #10169：不能把 reset 排到开始样式之后）。
///
/// 列区间是 `[start_col, start_col + length)`，列号从 0 起、按显示列计：
/// - `strict == false`：只要字素的**起始列**落在区间内就整个收下（宽字素可跨过右边界，
///   因此结果宽度可能比 `length` 大）；
/// - `strict == true`：还要求整个字素落在右边界内，边界上的宽字素被排除。
///
/// ANSI 处理：位于起点前的序列按原顺序暂存，随第一个被收下的内容一起输出；区间内的
/// 序列原样输出；右边界之后的序列丢弃。`length == 0` 直接返回空结果。
pub fn slice_with_width(line: &str, start_col: usize, length: usize, strict: bool) -> SliceResult {
    slice_with_width_i64(line, start_col as i64, length as i64, strict)
}

/// [`slice_with_width`] 的 i64 实现：公开入口只接受非负列号，内部用 i64 计算右边界，
/// 避免 `start_col + length` 在 `usize` 上回绕。
fn slice_with_width_i64(line: &str, start_col: i64, length: i64, strict: bool) -> SliceResult {
    if length <= 0 {
        return SliceResult {
            text: String::new(),
            width: 0,
        };
    }
    let end_col = start_col + length;
    let mut result = String::new();
    let mut result_width = 0usize;
    let mut current_col: i64 = 0;
    let mut i = 0usize;
    // 起点之前遇到的 ANSI 先攒在这里，等第一个被收下的内容出现时按原顺序输出。
    let mut pending_ansi = String::new();

    while i < line.len() {
        let ansi_length = ansi_code_length(line, i);
        if ansi_length > 0 {
            // 区间内的序列立即输出；起点前的先缓存；右边界后的直接丢弃。
            if current_col >= start_col && current_col < end_col {
                result.push_str(&pending_ansi);
                result.push_str(&line[i..i + ansi_length]);
                pending_ansi.clear();
            } else if current_col < start_col {
                pending_ansi.push_str(&line[i..i + ansi_length]);
            }
            i += ansi_length;
            continue;
        }

        let mut text_end = i;
        while text_end < line.len() && ansi_code_length(line, text_end) == 0 {
            text_end += line[text_end..].chars().next().map_or(1, char::len_utf8);
        }

        for segment in line[i..text_end].graphemes(true) {
            let w = grapheme_width(segment) as i64;
            let in_range = current_col >= start_col && current_col < end_col;
            let fits = !strict || current_col + w <= end_col;
            if in_range && fits {
                if !pending_ansi.is_empty() {
                    result.push_str(&pending_ansi);
                    pending_ansi.clear();
                }
                result.push_str(segment);
                result_width += w as usize;
            }
            current_col += w;
            if current_col >= end_col {
                break;
            }
        }
        i = text_end;
        if current_col >= end_col {
            break;
        }
    }

    SliceResult {
        text: result,
        width: result_width,
    }
}

/// `extractSegments` 的返回值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segments {
    /// overlay 左侧要保留的文本（含其 ANSI）。
    pub before: String,
    /// `before` 的可见列宽。
    pub before_width: usize,
    /// overlay 右侧要保留的文本；开头带有扫描到此处时仍生效的样式重开序列。
    pub after: String,
    /// `after` 的可见列宽。
    pub after_width: usize,
}

/// `extractSegments(line, beforeEnd, afterStart, afterLen, strictAfter = false)`。
///
/// overlay 合成用：一次扫描同时取出 overlay 之前与之后的片段，"after" 继承 overlay 之前的样式。
/// TS 用模块级 `pooledStyleTracker`（每次调用先 `clear()`）；这里用局部 tracker，
/// 语义等价（见 `spec/tui-text.md` §8.2 第 10 条 (a)）。
///
/// 列参数（均按显示列）：
/// - `before_end`：overlay 左边界。`before` 只收**完整**落在 `[0, before_end)` 内的
///   字素；跨边界的宽字素不计入 `before`，也不计入 `before_width`。
/// - `after_start`：overlay 右边界，也是 `after` 的左边界（含）。`after_len` 是希望取的
///   列数，区间为 `[after_start, after_start + after_len)`。
/// - `strict_after`：`after` 是否要求字素完整落在区间内；`before` 不受此参数影响。
///
/// `after` 的第一个可见字素前会插入 `tracker.get_active_codes()`，让脱离原行的片段能
/// 独立渲染。ANSI 细节：`before_end` 之前的序列进入 `pending_ansi_before`，随 `before`
/// 的内容一起输出；`after` 区间内的普通 SGR 在第一个可见字素之前被丢弃（样式已由
/// `get_active_codes` 表达），零宽 APC（`ESC _`，例如 CURSOR_MARKER）例外；第一个
/// 可见字素之后，区间内的序列原样复制。
///
/// 扫描停止条件：`after_len <= 0` 时到 `before_end` 即停（调用方只关心 before），
/// 否则扫到 `after_end`。`before`/`after` 都可能为空串，此时对应宽度为 0。
pub fn extract_segments(
    line: &str,
    before_end: usize,
    after_start: usize,
    after_len: usize,
    strict_after: bool,
) -> Segments {
    let before_end = before_end as i64;
    let after_start = after_start as i64;
    let after_len = after_len as i64;
    let after_end = after_start + after_len;

    let mut before = String::new();
    let mut before_width = 0usize;
    let mut after = String::new();
    let mut after_width = 0usize;
    let mut current_col: i64 = 0;
    let mut i = 0usize;
    let mut pending_ansi_before = String::new();
    let mut after_started = false;
    let mut tracker = AnsiCodeTracker::new();

    while i < line.len() {
        let ansi_length = ansi_code_length(line, i);
        if ansi_length > 0 {
            let code = &line[i..i + ansi_length];
            tracker.process(code);
            if current_col < before_end {
                pending_ansi_before.push_str(code);
            } else if current_col >= after_start
                && current_col < after_end
                && (after_started || code.starts_with("\x1b_"))
            {
                // 普通 SGR 在 after 起点被丢弃（样式已由 getActiveCodes 表达）；
                // 零宽的 APC 标记（CURSOR_MARKER）不受此限。
                after.push_str(code);
            }
            i += ansi_length;
            continue;
        }

        let mut text_end = i;
        while text_end < line.len() && ansi_code_length(line, text_end) == 0 {
            text_end += line[text_end..].chars().next().map_or(1, char::len_utf8);
        }

        for segment in line[i..text_end].graphemes(true) {
            let w = grapheme_width(segment) as i64;

            // before 只收完整落入左区间的字素；跨 before_end 的字素跳过，但列号照常推进。
            if current_col < before_end && current_col + w <= before_end {
                if !pending_ansi_before.is_empty() {
                    before.push_str(&pending_ansi_before);
                    pending_ansi_before.clear();
                }
                before.push_str(segment);
                before_width += w as usize;
            } else if current_col >= after_start && current_col < after_end {
                let fits = !strict_after || current_col + w <= after_end;
                if fits {
                    if !after_started {
                        after.push_str(&tracker.get_active_codes());
                        after_started = true;
                    }
                    after.push_str(segment);
                    after_width += w as usize;
                }
            }

            current_col += w;
            // after_len <= 0 表示调用方只需要 before，扫到左边界即可停止；否则要扫到右边界。
            let done = if after_len <= 0 {
                current_col >= before_end
            } else {
                current_col >= after_end
            };
            if done {
                break;
            }
        }
        i = text_end;
        let done = if after_len <= 0 {
            current_col >= before_end
        } else {
            current_col >= after_end
        };
        if done {
            break;
        }
    }

    Segments {
        before,
        before_width,
        after,
        after_width,
    }
}

// ============================================================================
// 字符分类
// ============================================================================

/// JS `\s` 的 25 个码点（**不含** U+0085、U+180E、U+200B；**含** U+FEFF）。
///
/// Rust 的 `char::is_whitespace`（`White_Space`）多 U+0085、少 U+FEFF，不能直接替换。
pub fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `String.prototype.trimEnd()`（JS 空白集合）。
pub fn trim_end_js(text: &str) -> &str {
    text.trim_end_matches(is_js_whitespace)
}

/// `String.prototype.trim()`（JS 空白集合）。
pub fn trim_js(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// `isWhitespaceChar(char)`：**未锚定**——`char` 内任一字符匹配即为 true。
pub fn is_whitespace_char(text: &str) -> bool {
    text.chars().any(is_js_whitespace)
}

/// `PUNCTUATION_REGEX` 的字符集合（`[(){}[\]<>.,;:'"!?+\-=*/\\|&%^$#@~`]`）。
///
/// 是否包含某个字符由 [`is_punctuation_char`] 判定；需要正则形态的场景（分段器边界）由
/// [`punctuation_regex`] 提供。
pub fn is_punctuation_char_in_set(ch: char) -> bool {
    matches!(
        ch,
        '(' | ')'
            | '{'
            | '}'
            | '['
            | ']'
            | '<'
            | '>'
            | '.'
            | ','
            | ';'
            | ':'
            | '\''
            | '"'
            | '!'
            | '?'
            | '+'
            | '-'
            | '='
            | '*'
            | '/'
            | '\\'
            | '|'
            | '&'
            | '%'
            | '^'
            | '$'
            | '#'
            | '@'
            | '~'
            | '`'
    )
}

/// `PUNCTUATION_REGEX`（`/g` 版本派生用）。
pub fn punctuation_regex() -> &'static Regex {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"[(){}\[\]<>.,;:'"!?+\-=*/\\|&%^$#@~`]"#).expect("PUNCTUATION_REGEX compiles")
    });
    &REGEX
}

/// `isPunctuationChar(char)`：**未锚定**——`char` 内任一字符匹配即为 true。
pub fn is_punctuation_char(text: &str) -> bool {
    text.chars().any(is_punctuation_char_in_set)
}

/// `cjkBreakRegex` 的单字符判定（`Script_Extensions` 含 Han/Hiragana/Katakana/Hangul/Bopomofo）。
pub fn is_cjk_break_char(ch: char) -> bool {
    range_contains(text_tables::CJK_BREAK_RANGES, ch as u32)
}

/// `cjkBreakRegex`（无锚点）。
///
/// 直接从 [`is_cjk_break_char`] 的区间表生成字符类：`regex` 自带的 `\p{scx=Han}` 等属性表
/// 是**旧的 Unicode 版本**，漏掉 Node 26（Unicode 17.0）里的
/// U+16FF2..U+16FF6、U+2B73A..U+2B73F、U+2CEA2..U+2CEAD、U+323B0..U+33479 等 4,321 个码点，
/// 会让 `wrapTextWithAnsi` 在这些字符旁切出不同的 token 边界。
pub fn cjk_break_regex() -> &'static Regex {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        let mut source = String::with_capacity(1024);
        source.push('[');
        for (start, end) in text_tables::CJK_BREAK_RANGES {
            if start == end {
                source.push_str(&format!("\\x{{{start:X}}}"));
            } else {
                source.push_str(&format!("\\x{{{start:X}}}-\\x{{{end:X}}}"));
            }
        }
        source.push(']');
        Regex::new(&source).expect("cjkBreakRegex compiles")
    });
    &REGEX
}

/// `cjkBreakRegex.test(text)`。
pub fn cjk_break_matches(text: &str) -> bool {
    cjk_break_regex().is_match(text)
}

/// `cjkPunctuationRegex` 的单字符判定（由 Node 枚举全部码点生成的表）。
pub fn is_cjk_punctuation(ch: char) -> bool {
    range_contains(text_tables::CJK_PUNCTUATION_RANGES, ch as u32)
}

/// `cjkPunctuationRegex`。第一分支（"既是 `\p{P}` 又是 CJK 脚本"）无法用 `regex` 表达
/// （需要同一位置的零宽前瞻 + 字符类），因此用区间表另实现 [`is_cjk_punctuation`]；
/// 这个正则只覆盖显式标点列表，供必须拿到 `Regex` 的调用方使用。
///
/// 模式串由 `text_tables::CJK_PUNCTUATION_RANGES` 生成，不再手写字符类：区间表是唯一
/// 事实来源（`is_cjk_punctuation` 与这个正则因此不会互相漂移）。
pub fn cjk_punctuation_regex() -> &'static Regex {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        let mut pattern = String::from("^[");
        for &(start, end) in text_tables::CJK_PUNCTUATION_RANGES {
            if start == end {
                pattern.push_str(&format!("\\x{{{start:X}}}"));
            } else {
                pattern.push_str(&format!("\\x{{{start:X}}}-\\x{{{end:X}}}"));
            }
        }
        pattern.push_str("]$");
        Regex::new(&pattern).expect("cjkPunctuationRegex compiles")
    });
    &REGEX
}

/// `autocompleteSeparatorRegex` 的单字符判定：JS 空白 **或** CJK 标点。
pub fn is_autocomplete_separator(ch: char) -> bool {
    is_js_whitespace(ch) || is_cjk_punctuation(ch)
}

/// `flattenLines(lines)`：V8 的字符串扁平化提示；Rust 无对应物，保留签名（no-op）。
pub fn flatten_lines(_lines: &mut [String]) {}

// ============================================================================
// 词分段与按词移动（word-navigation.ts）
// ============================================================================

/// `Intl.SegmentData`（word 粒度）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordSegment {
    /// 该段的文本。
    pub segment: String,
    /// 该段在**原始输入**中的起始 UTF-16 偏移。
    pub index: usize,
    /// 产生本段时传给分段器的完整输入（对应 `Intl.SegmentData.input`，逐段各存一份）。
    pub input: String,
    /// 段是否“像词”：对应 ICU 规则状态标签 > 0（`{100}`/`{200}`/`{400}`）。
    pub is_word_like: bool,
}

/// 自定义分段器：把文本切成按 `index` 升序的段列表。
///
/// 契约：各段必须首尾相接、完整覆盖输入。[`find_word_backward`]/[`find_word_forward`]
/// 只按顺序累加 `segment` 的 UTF-16 长度，不读取 `index`，也不检测缺口。
pub type SegmentFn<'a> = &'a dyn Fn(&str) -> Vec<WordSegment>;
/// 原子段判定：整段视为一个单位（如粘贴标记）。
pub type AtomicPredicate<'a> = &'a dyn Fn(&str) -> bool;

/// TS `WordNavigationOptions`：
/// - `segment`：自定义分段器（返回按 index 升序的段列表），缺省用 `Intl.Segmenter`。
/// - `is_atomic_segment`：识别"原子段"（如粘贴标记），整段视为一个单位。
#[derive(Clone, Copy, Default)]
pub struct WordNavigationOptions<'a> {
    /// 自定义分段器；`None` 时用默认的 ICU 规则 + 词典分段。
    pub segment: Option<SegmentFn<'a>>,
    /// 原子段判定：返回 `true` 的段（例如粘贴标记）在移动时整体跨越，不再按内部标点细分。
    pub is_atomic_segment: Option<AtomicPredicate<'a>>,
}

// ---------------------------------------------------------------------------
// ICU 词规则引擎（word.txt）
// ---------------------------------------------------------------------------
//
// `Intl.Segmenter("word")` 由 ICU 的规则引擎产生边界，规则状态（`{100}`/`{200}`/`{400}`）
// 决定 `isWordLike`。这里按 ICU 78.3 的 `word.txt` 逐条移植：字符类 → 规则项 → NFA →
// 带 `!!chain` 的 DFA。字符类 id 见 `text_tables::WORD_BREAK_RANGES`。

/// `Word_Break` 类 id（与 `text_tables::WORD_BREAK_RANGES` 的第三列一致；0 = Other）。
mod wb {
    pub(super) const OTHER: u8 = 0;
    pub(super) const CR: u8 = 1;
    pub(super) const LF: u8 = 2;
    pub(super) const NEWLINE: u8 = 3;
    pub(super) const EXTEND: u8 = 4;
    pub(super) const ZWJ: u8 = 5;
    pub(super) const FORMAT: u8 = 6;
    pub(super) const RI: u8 = 7;
    pub(super) const EXTEND_NUM_LET: u8 = 8;
    pub(super) const KATAKANA: u8 = 9;
    pub(super) const HEBREW: u8 = 10;
    pub(super) const ALETTER: u8 = 11;
    pub(super) const MID_NUM_LET: u8 = 12;
    pub(super) const MID_LETTER: u8 = 13;
    pub(super) const MID_NUM: u8 = 14;
    pub(super) const SINGLE_QUOTE: u8 = 15;
    pub(super) const DOUBLE_QUOTE: u8 = 16;
    pub(super) const NUMERIC: u8 = 17;
    pub(super) const WSEG_SPACE: u8 = 18;
}

/// `Word_Break` 属性（`text_tables::WORD_BREAK_RANGES`，Unicode 17.0）。
fn word_break_class(cp: u32) -> u8 {
    use text_tables::WORD_BREAK_RANGES;
    let mut low = 0usize;
    let mut high = WORD_BREAK_RANGES.len();
    while low < high {
        let mid = (low + high) / 2;
        let (start, end, class) = WORD_BREAK_RANGES[mid];
        if cp < start {
            high = mid;
        } else if cp > end {
            low = mid + 1;
        } else {
            return class;
        }
    }
    wb::OTHER
}

/// `\p{Script=Han}` / `\p{Script=Hiragana}` / `\p{Script=Katakana}`（ICU 的
/// `$Han` / `$Hiragana` / `$Katakana`，**不含** Script_Extensions）。
fn is_script_han(cp: u32) -> bool {
    range_contains(text_tables::HAN_SCRIPT_RANGES, cp)
}
fn is_script_hiragana(cp: u32) -> bool {
    range_contains(text_tables::HIRAGANA_SCRIPT_RANGES, cp)
}
fn is_script_katakana(cp: u32) -> bool {
    range_contains(text_tables::KATAKANA_SCRIPT_RANGES, cp)
}

/// `\p{Script=Thai}` / `Lao` / `Khmer` / `Myanmar`（ICU 四个 SA 词典引擎的脚本集合）。
fn is_script_thai(cp: u32) -> bool {
    range_contains(text_tables::THAI_SCRIPT_RANGES, cp)
}
fn is_script_lao(cp: u32) -> bool {
    range_contains(text_tables::LAO_SCRIPT_RANGES, cp)
}
fn is_script_khmer(cp: u32) -> bool {
    range_contains(text_tables::KHMER_SCRIPT_RANGES, cp)
}
fn is_script_myanmar(cp: u32) -> bool {
    range_contains(text_tables::MYANMAR_SCRIPT_RANGES, cp)
}

/// SA 词典引擎的脚本 id（ICU 每个脚本一个引擎，`fSet` 只含本脚本的 `LineBreak=SA` 字符）。
const SEA_THAI: u8 = 1;
const SEA_LAO: u8 = 2;
const SEA_KHMER: u8 = 3;
const SEA_MYANMAR: u8 = 4;

/// 该码点属于哪个 SA 词典引擎；`0` 表示不属于任何一个。
fn sea_dictionary_script(cp: u32) -> u8 {
    if !is_complex_context(cp) {
        return 0;
    }
    if is_script_thai(cp) {
        SEA_THAI
    } else if is_script_lao(cp) {
        SEA_LAO
    } else if is_script_khmer(cp) {
        SEA_KHMER
    } else if is_script_myanmar(cp) {
        SEA_MYANMAR
    } else {
        0
    }
}

/// ICU `$HangulSyllable` = `[\uac00-\ud7a3]`。
fn is_hangul_syllable(cp: u32) -> bool {
    (0xAC00..=0xD7A3).contains(&cp)
}

/// ICU `$KanaKanji` = `[$Han $Hiragana $Katakana]`。
///
/// 注意集合来源各不相同：`$Han` 是 `[:Han:]`、`$Hiragana` 是 `[:Hiragana:]`（都是
/// **Script**），而 `$Katakana` 是 `[\p{Word_Break = Katakana}]`（**Word_Break 属性**）。
/// 后者比 `[:Katakana:]` 多出 `ー`/`゛`/`゜`、带圈/带框片假名等 Script=Common 的字符
/// （见 `text_tables::WORD_BREAK_RANGES` 的 class 9）；这些字符在规则里与汉字/假名连写，
/// 但不属于词典引擎的字符集（见 [`is_cjk_dictionary_char`]）。
fn is_kana_kanji(cp: u32) -> bool {
    is_script_han(cp) || is_script_hiragana(cp) || word_break_class(cp) == wb::KATAKANA
}

/// 词典引擎的 `[:Katakana:]`（Script），用于 [`is_cjk_dictionary_char`]。
fn is_script_katakana_run(cp: u32) -> bool {
    is_script_katakana(cp)
}

/// ICU `$dictionaryCJK` = `[$KanaKanji $HangulSyllable]`。
fn is_dictionary_cjk(cp: u32) -> bool {
    is_kana_kanji(cp) || is_hangul_syllable(cp)
}

/// ICU `$ComplexContext` = `[:LineBreak=Complex_Context:]`，减去 Extend 与 GCB=Control
/// 后并入 `$ALetterPlus`。
fn is_complex_context(cp: u32) -> bool {
    range_contains(text_tables::COMPLEX_CONTEXT_RANGES, cp)
}

/// ICU `$Extended_Pict` = `\p{Extended_Pictographic}`。
fn is_extended_pictographic(cp: u32) -> bool {
    range_contains(text_tables::EXTENDED_PICTOGRAPHIC_RANGES, cp)
}

/// ICU `$Ideographic` = `\p{Ideographic}`。
fn is_ideographic(cp: u32) -> bool {
    range_contains(text_tables::IDEOGRAPHIC_RANGES, cp)
}

/// ICU `$Control` = `[\p{Grapheme_Cluster_Break = Control}]`（用于 `$ALetterPlus` 的排除）。
fn is_gcb_control(cp: u32) -> bool {
    range_contains(text_tables::GCB_CONTROL_RANGES, cp)
}

/// ICU 规则里的字符类 id。**名字即身份**：`!!chain` 的 followPos 合并要求两侧叶子
/// 属于同一个 `UnicodeSet`（`calcChainedFollowPos` 比较 `fVal`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cls {
    Cr,
    Lf,
    Newline,
    ExtendNoHan,
    Zwj,
    Format,
    RegionalIndicator,
    ExtendNumLet,
    Katakana,
    HebrewLetter,
    ALetter,
    SingleQuote,
    DoubleQuote,
    MidNumLet,
    MidLetter,
    MidNum,
    Numeric,
    WsegSpace,
    ExtendedPict,
    Hiragana,
    Ideographic,
    HangulSyllable,
    ComplexContext,
    GcbControl,
    KanaKanji,
    /// `[$Extend $Format $ZWJ]`
    ExFm,
    /// `[^$CR $LF $Newline $ExFm]`
    NotCrLfNlExFm,
    /// `[$ALetter-$dictionaryCJK [$ComplexContext-$Extend-$Control]]`
    ALetterPlus,
    /// 规则 999 的 `.`
    Any,
}

/// 类 id → 是否匹配该码点。`ExFm` / `ALetterPlus` / `NotCrLfNlExFm` / `Any` 是 ICU 规则里
/// 用集合运算拼出来的，逐个照抄。
fn class_matches(class: Cls, cp: u32) -> bool {
    let wb_class = || word_break_class(cp);
    match class {
        Cls::Cr => wb_class() == wb::CR,
        Cls::Lf => wb_class() == wb::LF,
        Cls::Newline => wb_class() == wb::NEWLINE,
        // `$Extend = [\p{Word_Break = Extend}-$Han]`（ICU 从 $Extend 里挖掉汉字）
        Cls::ExtendNoHan => wb_class() == wb::EXTEND && !is_script_han(cp),
        Cls::Zwj => wb_class() == wb::ZWJ,
        Cls::Format => wb_class() == wb::FORMAT,
        Cls::RegionalIndicator => wb_class() == wb::RI,
        Cls::ExtendNumLet => wb_class() == wb::EXTEND_NUM_LET,
        Cls::Katakana => wb_class() == wb::KATAKANA,
        Cls::HebrewLetter => wb_class() == wb::HEBREW,
        Cls::ALetter => wb_class() == wb::ALETTER,
        Cls::SingleQuote => wb_class() == wb::SINGLE_QUOTE,
        Cls::DoubleQuote => wb_class() == wb::DOUBLE_QUOTE,
        Cls::MidNumLet => wb_class() == wb::MID_NUM_LET,
        Cls::MidLetter => wb_class() == wb::MID_LETTER,
        Cls::MidNum => wb_class() == wb::MID_NUM,
        Cls::Numeric => wb_class() == wb::NUMERIC,
        Cls::WsegSpace => wb_class() == wb::WSEG_SPACE,
        Cls::ExtendedPict => is_extended_pictographic(cp),
        Cls::Hiragana => is_script_hiragana(cp),
        Cls::Ideographic => is_ideographic(cp),
        Cls::HangulSyllable => is_hangul_syllable(cp),
        Cls::ComplexContext => is_complex_context(cp),
        Cls::GcbControl => is_gcb_control(cp),
        Cls::KanaKanji => is_kana_kanji(cp),
        // `$ExFm = [$Extend $Format $ZWJ]`
        Cls::ExFm => {
            class_matches(Cls::ExtendNoHan, cp)
                || class_matches(Cls::Format, cp)
                || class_matches(Cls::Zwj, cp)
        }
        // `[^$CR $LF $Newline $ExFm]`
        Cls::NotCrLfNlExFm => {
            !(class_matches(Cls::Cr, cp)
                || class_matches(Cls::Lf, cp)
                || class_matches(Cls::Newline, cp)
                || class_matches(Cls::ExFm, cp))
        }
        // `[$ALetter-$dictionaryCJK [$ComplexContext-$Extend-$Control]]`
        Cls::ALetterPlus => {
            (class_matches(Cls::ALetter, cp) && !is_dictionary_cjk(cp))
                || (class_matches(Cls::ComplexContext, cp)
                    && !class_matches(Cls::ExtendNoHan, cp)
                    && !class_matches(Cls::GcbControl, cp))
        }
        Cls::Any => true,
    }
}

/// 规则项：一个字符类 + 是否 `*`。
#[derive(Debug, Clone, Copy)]
struct RuleElem {
    class: Cls,
    star: bool,
}

const fn e(class: Cls) -> RuleElem {
    RuleElem { class, star: false }
}
const fn s(class: Cls) -> RuleElem {
    RuleElem { class, star: true }
}

/// 一条规则：项序列 + 规则状态标签 + 是否允许 `!!chain` 链入（ICU 的 `^` 禁止链入）。
struct Rule {
    elems: &'static [RuleElem],
    tag: u32,
    chain_in: bool,
}

/// ICU 78.3 `word.txt` 的规则，**顺序即优先级**（同一位置接受时取最后匹配的那条）。
const RULES: &[Rule] = &[
    // Rule 3 - CR x LF
    Rule {
        elems: &[e(Cls::Cr), e(Cls::Lf)],
        tag: 0,
        chain_in: true,
    },
    // Rule 3c - ZWJ x Extended_Pictographic
    Rule {
        elems: &[e(Cls::Zwj), e(Cls::ExtendedPict)],
        tag: 0,
        chain_in: true,
    },
    // Rule 3d - WSegSpace x WSegSpace
    Rule {
        elems: &[e(Cls::WsegSpace), e(Cls::WsegSpace)],
        tag: 0,
        chain_in: true,
    },
    // Rule 4 (a) - ^$ExFm+（只出现在文本开头/边界之后）
    Rule {
        elems: &[e(Cls::ExFm), s(Cls::ExFm)],
        tag: 0,
        chain_in: false,
    },
    // Rule 4 (b) - [^$CR $LF $Newline $ExFm] $ExFm*
    Rule {
        elems: &[e(Cls::NotCrLfNlExFm), s(Cls::ExFm)],
        tag: 0,
        chain_in: true,
    },
    // $Numeric $ExFm* {100}
    Rule {
        elems: &[e(Cls::Numeric), s(Cls::ExFm)],
        tag: 100,
        chain_in: true,
    },
    // $ALetterPlus $ExFm* {200}
    Rule {
        elems: &[e(Cls::ALetterPlus), s(Cls::ExFm)],
        tag: 200,
        chain_in: true,
    },
    // $HangulSyllable {200}
    Rule {
        elems: &[e(Cls::HangulSyllable)],
        tag: 200,
        chain_in: true,
    },
    // $Hebrew_Letter $ExFm* {200}
    Rule {
        elems: &[e(Cls::HebrewLetter), s(Cls::ExFm)],
        tag: 200,
        chain_in: true,
    },
    // $Katakana $ExFm* {400}
    Rule {
        elems: &[e(Cls::Katakana), s(Cls::ExFm)],
        tag: 400,
        chain_in: true,
    },
    // $Hiragana $ExFm* {400}
    Rule {
        elems: &[e(Cls::Hiragana), s(Cls::ExFm)],
        tag: 400,
        chain_in: true,
    },
    // $Ideographic $ExFm* {400}
    Rule {
        elems: &[e(Cls::Ideographic), s(Cls::ExFm)],
        tag: 400,
        chain_in: true,
    },
    // rule 5 - ($ALetterPlus | $Hebrew_Letter) $ExFm* ($ALetterPlus | $Hebrew_Letter)
    Rule {
        elems: &[e(Cls::ALetterPlus), s(Cls::ExFm), e(Cls::ALetterPlus)],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[e(Cls::HebrewLetter), s(Cls::ExFm), e(Cls::HebrewLetter)],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[e(Cls::ALetterPlus), s(Cls::ExFm), e(Cls::HebrewLetter)],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[e(Cls::HebrewLetter), s(Cls::ExFm), e(Cls::ALetterPlus)],
        tag: 200,
        chain_in: true,
    },
    // rule 6 - (… ) $ExFm* (MidLetter|MidNumLet|SingleQuote) $ExFm* (… ) {200}
    Rule {
        elems: &[
            e(Cls::ALetterPlus),
            s(Cls::ExFm),
            e(Cls::MidLetter),
            s(Cls::ExFm),
            e(Cls::ALetterPlus),
        ],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[
            e(Cls::ALetterPlus),
            s(Cls::ExFm),
            e(Cls::MidNumLet),
            s(Cls::ExFm),
            e(Cls::ALetterPlus),
        ],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[
            e(Cls::ALetterPlus),
            s(Cls::ExFm),
            e(Cls::SingleQuote),
            s(Cls::ExFm),
            e(Cls::ALetterPlus),
        ],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[
            e(Cls::HebrewLetter),
            s(Cls::ExFm),
            e(Cls::MidLetter),
            s(Cls::ExFm),
            e(Cls::HebrewLetter),
        ],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[
            e(Cls::HebrewLetter),
            s(Cls::ExFm),
            e(Cls::MidNumLet),
            s(Cls::ExFm),
            e(Cls::HebrewLetter),
        ],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[
            e(Cls::HebrewLetter),
            s(Cls::ExFm),
            e(Cls::SingleQuote),
            s(Cls::ExFm),
            e(Cls::HebrewLetter),
        ],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[
            e(Cls::ALetterPlus),
            s(Cls::ExFm),
            e(Cls::MidLetter),
            s(Cls::ExFm),
            e(Cls::HebrewLetter),
        ],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[
            e(Cls::ALetterPlus),
            s(Cls::ExFm),
            e(Cls::MidNumLet),
            s(Cls::ExFm),
            e(Cls::HebrewLetter),
        ],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[
            e(Cls::ALetterPlus),
            s(Cls::ExFm),
            e(Cls::SingleQuote),
            s(Cls::ExFm),
            e(Cls::HebrewLetter),
        ],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[
            e(Cls::HebrewLetter),
            s(Cls::ExFm),
            e(Cls::MidLetter),
            s(Cls::ExFm),
            e(Cls::ALetterPlus),
        ],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[
            e(Cls::HebrewLetter),
            s(Cls::ExFm),
            e(Cls::MidNumLet),
            s(Cls::ExFm),
            e(Cls::ALetterPlus),
        ],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[
            e(Cls::HebrewLetter),
            s(Cls::ExFm),
            e(Cls::SingleQuote),
            s(Cls::ExFm),
            e(Cls::ALetterPlus),
        ],
        tag: 200,
        chain_in: true,
    },
    // rule 7a - $Hebrew_Letter $ExFm* $Single_Quote {200}
    Rule {
        elems: &[e(Cls::HebrewLetter), s(Cls::ExFm), e(Cls::SingleQuote)],
        tag: 200,
        chain_in: true,
    },
    // rule 7b/7c - $Hebrew_Letter $ExFm* $Double_Quote $ExFm* $Hebrew_Letter
    Rule {
        elems: &[
            e(Cls::HebrewLetter),
            s(Cls::ExFm),
            e(Cls::DoubleQuote),
            s(Cls::ExFm),
            e(Cls::HebrewLetter),
        ],
        tag: 0,
        chain_in: true,
    },
    // rule 8 - $Numeric $ExFm* $Numeric
    Rule {
        elems: &[e(Cls::Numeric), s(Cls::ExFm), e(Cls::Numeric)],
        tag: 0,
        chain_in: true,
    },
    // rule 9 - ($ALetterPlus | $Hebrew_Letter) $ExFm* $Numeric
    Rule {
        elems: &[e(Cls::ALetterPlus), s(Cls::ExFm), e(Cls::Numeric)],
        tag: 0,
        chain_in: true,
    },
    Rule {
        elems: &[e(Cls::HebrewLetter), s(Cls::ExFm), e(Cls::Numeric)],
        tag: 0,
        chain_in: true,
    },
    // rule 10 - $Numeric $ExFm* ($ALetterPlus | $Hebrew_Letter)
    Rule {
        elems: &[e(Cls::Numeric), s(Cls::ExFm), e(Cls::ALetterPlus)],
        tag: 0,
        chain_in: true,
    },
    Rule {
        elems: &[e(Cls::Numeric), s(Cls::ExFm), e(Cls::HebrewLetter)],
        tag: 0,
        chain_in: true,
    },
    // rule 11/12 - $Numeric $ExFm* (MidNum|MidNumLet|SingleQuote) $ExFm* $Numeric
    Rule {
        elems: &[
            e(Cls::Numeric),
            s(Cls::ExFm),
            e(Cls::MidNum),
            s(Cls::ExFm),
            e(Cls::Numeric),
        ],
        tag: 0,
        chain_in: true,
    },
    Rule {
        elems: &[
            e(Cls::Numeric),
            s(Cls::ExFm),
            e(Cls::MidNumLet),
            s(Cls::ExFm),
            e(Cls::Numeric),
        ],
        tag: 0,
        chain_in: true,
    },
    Rule {
        elems: &[
            e(Cls::Numeric),
            s(Cls::ExFm),
            e(Cls::SingleQuote),
            s(Cls::ExFm),
            e(Cls::Numeric),
        ],
        tag: 0,
        chain_in: true,
    },
    // rule 13 - $Katakana $ExFm* $Katakana {400}
    Rule {
        elems: &[e(Cls::Katakana), s(Cls::ExFm), e(Cls::Katakana)],
        tag: 400,
        chain_in: true,
    },
    // rule 13a - X $ExFm* $ExtendNumLet
    Rule {
        elems: &[e(Cls::ALetterPlus), s(Cls::ExFm), e(Cls::ExtendNumLet)],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[e(Cls::HebrewLetter), s(Cls::ExFm), e(Cls::ExtendNumLet)],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[e(Cls::Numeric), s(Cls::ExFm), e(Cls::ExtendNumLet)],
        tag: 100,
        chain_in: true,
    },
    Rule {
        elems: &[e(Cls::Katakana), s(Cls::ExFm), e(Cls::ExtendNumLet)],
        tag: 400,
        chain_in: true,
    },
    Rule {
        elems: &[e(Cls::ExtendNumLet), s(Cls::ExFm), e(Cls::ExtendNumLet)],
        tag: 200,
        chain_in: true,
    },
    // rule 13b - $ExtendNumLet $ExFm* X
    Rule {
        elems: &[e(Cls::ExtendNumLet), s(Cls::ExFm), e(Cls::ALetterPlus)],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[e(Cls::ExtendNumLet), s(Cls::ExFm), e(Cls::HebrewLetter)],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[e(Cls::ExtendNumLet), s(Cls::ExFm), e(Cls::Numeric)],
        tag: 100,
        chain_in: true,
    },
    Rule {
        elems: &[e(Cls::ExtendNumLet), s(Cls::ExFm), e(Cls::Katakana)],
        tag: 400,
        chain_in: true,
    },
    // rules 15-17 - ^$Regional_Indicator $ExFm* $Regional_Indicator
    Rule {
        elems: &[
            e(Cls::RegionalIndicator),
            s(Cls::ExFm),
            e(Cls::RegionalIndicator),
        ],
        tag: 0,
        chain_in: false,
    },
    // special handling for CJK: chain for later dictionary segmentation
    Rule {
        elems: &[e(Cls::HangulSyllable), e(Cls::HangulSyllable)],
        tag: 200,
        chain_in: true,
    },
    Rule {
        elems: &[e(Cls::KanaKanji), e(Cls::KanaKanji)],
        tag: 400,
        chain_in: true,
    },
    // Rule 999 - .;
    Rule {
        elems: &[e(Cls::Any)],
        tag: 0,
        chain_in: true,
    },
];

/// NFA 配置：`rule * RULE_STRIDE + k`，表示规则 `rule` 已吃掉前 `k` 项、下一步期待第 `k` 项。
const RULE_STRIDE: usize = 8;

/// `closure`：`*` 项可以匹配零次，所以处于 `*` 项时可以跳到下一项。
fn closure(cfg: usize, out: &mut Vec<usize>) {
    let rule = &RULES[cfg / RULE_STRIDE];
    let k = cfg % RULE_STRIDE;
    // `k == len` 是接受状态（`k == RULE_STRIDE` 理论上不会出现，规则最多 5 项）。
    if k < rule.elems.len() && rule.elems[k].star {
        let next = cfg / RULE_STRIDE * RULE_STRIDE + k + 1;
        if !out.contains(&next) {
            out.push(next);
            closure(next, out);
        }
    }
}

/// 吃掉一个码点后配置的走向；`None` 表示该配置死掉。
fn step(cfg: usize, cp: u32) -> Option<usize> {
    let rule_index = cfg / RULE_STRIDE;
    let rule = &RULES[rule_index];
    let k = cfg % RULE_STRIDE;
    if k >= rule.elems.len() {
        return None;
    }
    let elem = rule.elems[k];
    if !class_matches(elem.class, cp) {
        return None;
    }
    Some(if elem.star {
        cfg
    } else {
        rule_index * RULE_STRIDE + k + 1
    })
}

/// 加入配置并展开它的 `*` 闭包；已存在则不动。去重是为了避免同一配置重复入集合，
/// 导致 `active` 膨胀以及 `closure`/`step` 对同一配置重复展开。
fn add_config(set: &mut Vec<usize>, cfg: usize) {
    if !set.contains(&cfg) {
        set.push(cfg);
        closure(cfg, set);
    }
}

/// 该配置是否是一个"接受态"（整条规则匹配完），返回其规则状态标签。
fn accept_tag(cfg: usize) -> Option<u32> {
    let rule = &RULES[cfg / RULE_STRIDE];
    (cfg % RULE_STRIDE >= rule.elems.len()).then_some(rule.tag)
}

/// 元素的字符类能否匹配 `cp`。
fn elem_matches(cfg: usize, cp: u32) -> bool {
    let rule = &RULES[cfg / RULE_STRIDE];
    let k = cfg % RULE_STRIDE;
    k < rule.elems.len() && class_matches(rule.elems[k].class, cp)
}

/// 元素是否可能是规则的最后一个消费项（ICU 的 lastPos，只有 `*` 项可空）。
fn is_last_position(cfg: usize) -> bool {
    let rule = &RULES[cfg / RULE_STRIDE];
    let k = cfg % RULE_STRIDE;
    if k >= rule.elems.len() {
        return false;
    }
    rule.elems[k + 1..].iter().all(|elem| elem.star)
}

/// 从 `start` 起匹配一段，返回 `(结束下标, 规则状态标签)`。
///
/// 移植自 ICU 的 `RuleBasedBreakIterator::handleNext`（`rbbi.cpp`）+ 状态表的
/// `calcChainedFollowPos`（`rbbitblb.cpp`）：
///
/// - DFA 状态是 NFA 配置的集合；
/// - 每次接受只**记录**位置与标签，继续往后找最长匹配（ICU 是"最长匹配 + 最后接受态"）；
/// - `!!chain` 让"能结束一条规则的叶子"把"首项同类字符的链入规则"的 followPos 并进来，
///   这正是 `snake_case_name`、`x.y.z` 这类整段词能连起来的原因。
///
/// 返回 `(end, tag)`：`end` 是匹配结束的码点下标（至少 `start + 1`），`tag` 是最后
/// 一次接受时的规则状态标签（0 表示非 word-like）。没有任何配置接受时返回
/// `(start + 1, 0)`；正常情况下规则 999 的 `.` 能兜住任意一个码点，该分支仅作防御。
fn match_rule_segment(cps: &[u32], start: usize) -> (usize, u32) {
    let mut active: Vec<usize> = Vec::with_capacity(32);
    for (index, rule) in RULES.iter().enumerate() {
        if !rule.elems.is_empty() && class_matches(rule.elems[0].class, cps[start]) {
            add_config(&mut active, index * RULE_STRIDE);
        }
    }

    let mut best_end = start;
    let mut best_tag = 0u32;
    let mut accepted = false;
    for &cfg in &active {
        if let Some(tag) = accept_tag(cfg) {
            accepted = true;
            best_tag = best_tag.max(tag);
        }
    }
    if !accepted {
        best_end = start;
    }

    let mut pos = start;
    while pos < cps.len() {
        let cp = cps[pos];
        let mut next: Vec<usize> = Vec::with_capacity(32);
        let mut chainable = false;
        for &cfg in &active {
            if let Some(advanced) = step(cfg, cp) {
                if is_last_position(cfg) {
                    chainable = true;
                }
                add_config(&mut next, advanced);
            }
        }
        if next.is_empty() {
            break;
        }
        if chainable {
            // ICU 的 calcChainedFollowPos：结束叶子与链入规则的首叶子类别相同时，
            // 把链入规则首项之后的 followPos 并入。等效于"该字符已被链入规则吃掉"。
            for (index, rule) in RULES.iter().enumerate() {
                if rule.chain_in && !rule.elems.is_empty() && class_matches(rule.elems[0].class, cp)
                {
                    add_config(&mut next, index * RULE_STRIDE + 1);
                }
            }
        }

        let mut tag = None;
        for &cfg in &next {
            if let Some(value) = accept_tag(cfg) {
                tag = Some(tag.map_or(value, |current: u32| current.max(value)));
            }
        }
        if let Some(value) = tag {
            best_end = pos + 1;
            best_tag = value;
            accepted = true;
        }
        active = next;
        pos += 1;
    }

    if !accepted {
        return (start + 1, 0);
    }
    let _ = elem_matches;
    (best_end, best_tag)
}

/// 规则引擎给出的段：`[start, end)` + 规则状态标签。
#[derive(Debug, Clone, Copy)]
struct RuleSegment {
    start: usize,
    end: usize,
    tag: u32,
}

/// 对整串跑规则引擎。
///
/// 从下标 0 起反复调用 `match_rule_segment`，直到覆盖全部码点。返回的 `end` 会被夹到
/// `[pos + 1, len]`，保证每轮至少前进一个码点，不会死循环。
fn rule_segments(cps: &[u32]) -> Vec<RuleSegment> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < cps.len() {
        let (end, tag) = match_rule_segment(cps, pos);
        let end = end.max(pos + 1).min(cps.len());
        out.push(RuleSegment {
            start: pos,
            end,
            tag,
        });
        pos = end;
    }
    out
}

/// 词典引擎的覆盖范围（ICU 的 `CjkBreakEngine` 集合：`[[:Han:][:Hiragana:][:Katakana:]ーｰﾞﾟ]`）。
fn is_cjk_dictionary_char(cp: u32) -> bool {
    is_script_han(cp)
        || is_script_hiragana(cp)
        || is_script_katakana_run(cp)
        || cp == 0x30FC
        || cp == 0xFF70
        || cp == 0xFF9E
        || cp == 0xFF9F
}

/// `\p{Script=Common}`。
///
/// ICU 的 `UnhandledEngine` 按**整个 Script** 记忆已处理的字符
/// （`UnhandledEngine::handleCharacter` 里 `applyIntPropertyValue(UCHAR_SCRIPT, …)`），
/// 所以一旦它接管过 `゛`/`゜`（`Script=Common` 的词典字符），后续同属 Common 的
/// `ー`/`ｰ`/`ﾞ`/`ﾟ` 都会被它整段吞掉，不再交给 `CjkBreakEngine`。
fn is_common_script(cp: u32) -> bool {
    range_contains(text_tables::COMMON_SCRIPT_RANGES, cp)
}

/// 与 Node 26.12.1 / ICU 78.3 的 `Intl.Segmenter("word")` 对齐的词分段。
///
/// 两条腿：
///
/// 1. **边界与 `isWordLike`** 由本文件的 ICU 规则引擎（[`rule_segments`]）给出，
///    与 ICU 78.3 的 `word.txt` 逐条对应，规则状态标签 > 0 即 `isWordLike`
///    （`SNOW`：`{100}`/`{200}`/`{400}`）。这一步解释了为什么 `__` 是 word-like、
///    `_` 不是，`한́` 不是（谚文音节 + 组合标记落到无标签分支），`ก́x` 是一段。
/// 2. **词典细分**。ICU 把 Han/Hiragana/Katakana（以及泰/老/高棉/缅）的连续段交给
///    词典引擎。这里用 [`word_dict`]——从 Node 内置的 ICU 78.3 数据包取出的**同一份
///    词典**（`cjdict`/`thaidict`/`laodict`/`khmerdict`/`burmesedict`）加上逐函数移植
///    的引擎（`CjkBreakEngine`/`ThaiBreakEngine`/`LaoBreakEngine`/`KhmerBreakEngine`/
///    `BurmeseBreakEngine`）——给出段内边界。因此 `ひらがな` 是 `ひ`/`ら`/`が`/`な`、
///    `จอห์น` 不切、`าษาไทย` 是 `าษา`/`ไทย`，与 Node 逐段一致。
///
///    引擎调度按 ICU `rbbi_cache.cpp` 的 `populateDictionary` 实现：扫描**词典类别**
///    （`$dictionary` = `$ComplexContext ∪ $KanaKanji ∪ $HangulSyllable`）字符，选中
///    `getLanguageBreakEngine` 会给出的引擎并让它吃掉自己 `fSet` 的连续段。两个容易
///    漏掉的细节：
///    * `゛`/`゜`（以及 `ー`/`ｰ` 在冷启动时）的 `Script=Common`，`loadEngineFor` 造不出
///      引擎，于是由 `UnhandledEngine` 接手；它按**整个 Script** 记忆，会把紧随其后的
///      `ー`/`ﾞ`/`ﾟ` 一并吞掉（`よ゛゜ーこ゛ぎ` 不切、`ざこ゜ーひだ` 不切）。若 Cjk
///      引擎已在本次迭代器的引擎列表里，则 Cjk 排在 Unhandled 之前优先处理。
///    * `CjkBreakEngine` 的工厂缓存是**进程级**的：第一次处理 Han/平假名/片假名之后，
///      单独出现的 `ー` 也会交给 Cjk（`ーこ` → `ー`/`こ`）。本移植按"始终已创建"建模，
///      与长驻进程（以及黄金数据）一致；唯一偏差是冷进程里**第一次**分段、且第一个词典
///      字符就是 `ー`/`ｰ`（此前没有任何 Han/平假名/片假名 词典段）：Node 此时会落到
///      `UnhandledEngine`（不切），本移植按 Cjk 处理（切）。
fn default_word_segments(text: &str) -> Vec<WordSegment> {
    // 阶段 1：转成码点数组，先跑 ICU 规则引擎，得到带 `isWordLike` 标签的规则段。
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let cps: Vec<u32> = chars.iter().map(|&(_, ch)| ch as u32).collect();
    let rules = rule_segments(&cps);

    let mut result: Vec<WordSegment> = Vec::new();
    let mut utf16 = 0usize;
    // `populateDictionary` 用的是 UText 的原生（UTF-16 码元）区间：
    // `if ((endPos - startPos) <= 1) return;` —— 一个增补平面字符也算 2 个码元。
    let mut utf16_offsets = Vec::with_capacity(chars.len() + 1);
    let mut offset = 0usize;
    for &(_, ch) in &chars {
        utf16_offsets.push(offset);
        offset += ch.len_utf16();
    }
    utf16_offsets.push(offset);

    // ICU `BreakIterator` 的词典引擎状态（每次 `segment()` 调用一个迭代器）：
    // * `unhandled_common` —— `UnhandledEngine` 是否已把 `Script=Common` 加入 `fHandled`；
    // * `cjk_in_list` —— `CjkBreakEngine` 是否已经进入本迭代器的引擎列表。
    // `getLanguageBreakEngine` 的查找顺序是：迭代器引擎列表（后进先出）→ 工厂（全局）→
    // `UnhandledEngine`。Unhandled 排在列表最底（`insertElementAt(…, 0)`），所以列表里
    // 已有 Cjk 时 Cjk 优先；只有 Cjk 不在列表里时，被吞掉过的 Common 字符才会继续走
    // Unhandled。工厂里的 Cjk 引擎是**进程级**的（第一次处理 Han/平假名/片假名后创建），
    // 这里按"始终已创建"建模——见模块文档。
    let mut unhandled_common = false;
    let mut cjk_in_list = false;

    for rule in &rules {
        let mut piece_starts: Vec<usize> = vec![rule.start];
        // `populateDictionary` 对长度 <= 1（UTF-16 码元）的规则段直接返回：不建引擎、不切分。
        if utf16_offsets[rule.end] - utf16_offsets[rule.start] > 1 {
            let mut index = rule.start;
            while index < rule.end {
                let cp = cps[index];
                // 触发引擎查找的只有词典类别字符：
                // `$dictionary` = `$ComplexContext ∪ $KanaKanji ∪ $HangulSyllable`。
                if !is_dictionary_cjk(cp) && !is_complex_context(cp) {
                    index += 1;
                    continue;
                }
                // 标准 ICU 数据里没有韩语词典（`loadDictionaryMatcherFor(Hangul)` 失败），
                // 谚文音节退化为 `UnhandledEngine`，不产生切分。
                if is_hangul_syllable(cp) {
                    index += 1;
                    continue;
                }
                let in_fset = is_cjk_dictionary_char(cp);
                let scripted_cjk =
                    is_script_han(cp) || is_script_hiragana(cp) || is_script_katakana(cp);
                // `ー`/`ｰ` 的 Script 是 Common（不在 `[:Katakana:]` 里，靠 fSet 显式加入），
                // 所以工厂无法从它们创建 Cjk 引擎；由 Cjk 接手需要它已经存在。
                if in_fset && (scripted_cjk || cjk_in_list || !unhandled_common) {
                    cjk_in_list = true;
                    let run_start = index;
                    let mut run_end = index;
                    while run_end + 1 < rule.end && is_cjk_dictionary_char(cps[run_end + 1]) {
                        run_end += 1;
                    }
                    let run: Vec<char> = chars[run_start..=run_end]
                        .iter()
                        .map(|&(_, ch)| ch)
                        .collect();
                    for boundary in word_dict::segment_run(&run, word_dict::DictKind::Cjk) {
                        piece_starts.push(run_start + boundary);
                    }
                    index = run_end + 1;
                    continue;
                }
                if is_common_script(cp) {
                    // `UnhandledEngine`：`handleCharacter(cp)` 把 `Script=Common` 加入
                    // `fHandled`，`findBreaks` 随即吞掉 Common 连续段（含 `ー`/`ﾞ`/`゛`/`゜`），
                    // 不产生切分。
                    unhandled_common = true;
                    let mut run_end = index + 1;
                    while run_end < rule.end && is_common_script(cps[run_end]) {
                        run_end += 1;
                    }
                    index = run_end;
                    continue;
                }
                let sea = sea_dictionary_script(cp);
                if sea == 0 {
                    // 其他 `LineBreak=SA` 脚本（例如 Tai Tham）：没有词典引擎，不产生切分。
                    index += 1;
                    continue;
                }
                // SA 词典引擎：`fSet` 是 `[:Script:] & [:LineBreak=SA:]` 的连续段。
                let run_start = index;
                let mut run_end = index;
                while run_end + 1 < rule.end && sea_dictionary_script(cps[run_end + 1]) == sea {
                    run_end += 1;
                }
                let kind = match sea {
                    SEA_THAI => word_dict::DictKind::Thai,
                    SEA_LAO => word_dict::DictKind::Lao,
                    SEA_KHMER => word_dict::DictKind::Khmer,
                    _ => word_dict::DictKind::Burmese,
                };
                let run: Vec<char> = chars[run_start..=run_end]
                    .iter()
                    .map(|&(_, ch)| ch)
                    .collect();
                for boundary in word_dict::segment_run(&run, kind) {
                    piece_starts.push(run_start + boundary);
                }
                index = run_end + 1;
            }
        }
        // 规则段自身的起止点与词典给出的段内边界合并、排序、去重。
        piece_starts.sort_unstable();
        piece_starts.dedup();
        piece_starts.push(rule.end);
        piece_starts.dedup();

        // 相邻边界两两构成输出段；`isWordLike` 沿用整条规则段的标签（词典细分不改变它）。
        for window in piece_starts.windows(2) {
            let (start, end) = (window[0], window[1]);
            if start >= end {
                continue;
            }
            let byte_start = chars[start].0;
            let byte_end = if end < chars.len() {
                chars[end].0
            } else {
                text.len()
            };
            let segment = &text[byte_start..byte_end];
            result.push(WordSegment {
                segment: segment.to_string(),
                index: utf16,
                input: text.to_string(),
                is_word_like: rule.tag > 0,
            });
            utf16 += segment.encode_utf16().count();
        }
    }

    // 空串时不产生段；ICU 对空输入也返回空。
    result
}

/// `getWordSegmenter()` 的等价物：返回共享的 `Intl.Segmenter` 分段函数。
///
/// TS 返回的是一个可 `segment()` 的实例；Rust 侧用返回段列表的函数表达，语义相同
/// （调用方都只做 `[...segmenter.segment(text)]`）。
pub fn get_word_segments(text: &str) -> Vec<WordSegment> {
    default_word_segments(text)
}

/// 一个 UTF-16 码元。辅助平面字符占两个码元；TS 的 `slice` 会把代理对切开，
/// 而 `str` 不能存孤立代理，所以把半个代理写成 `U+FFFD`
/// （ICU 对孤立代理的分段行为与 `U+FFFD` 完全相同，见 [`default_word_segments`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Utf16Unit {
    Scalar(char),
    HighHalf(char),
    LowHalf(char),
}

/// 把 `text` 拆成 UTF-16 码元序列。
fn utf16_units(text: &str) -> Vec<Utf16Unit> {
    let mut units = Vec::with_capacity(text.len());
    for ch in text.chars() {
        if ch.len_utf16() == 1 {
            units.push(Utf16Unit::Scalar(ch));
        } else {
            units.push(Utf16Unit::HighHalf(ch));
            units.push(Utf16Unit::LowHalf(ch));
        }
    }
    units
}

/// 码元序列 → 字符串：成对的代理还原成原字符，落单的代理写成 `U+FFFD`。
fn units_to_string(units: &[Utf16Unit]) -> String {
    let mut out = String::with_capacity(units.len());
    let mut index = 0usize;
    while index < units.len() {
        match units[index] {
            Utf16Unit::Scalar(ch) => {
                out.push(ch);
                index += 1;
            }
            Utf16Unit::HighHalf(ch) => {
                if index + 1 < units.len() && matches!(units[index + 1], Utf16Unit::LowHalf(_)) {
                    out.push(ch);
                    index += 2;
                } else {
                    out.push('\u{FFFD}');
                    index += 1;
                }
            }
            Utf16Unit::LowHalf(_) => {
                out.push('\u{FFFD}');
                index += 1;
            }
        }
    }
    out
}

/// `text.slice(0, cursor)`（UTF-16 语义；落在代理对中间时把代理替换成 `U+FFFD`）。
fn slice_utf16_prefix(text: &str, utf16_end: usize) -> String {
    let units = utf16_units(text);
    units_to_string(&units[..utf16_end.min(units.len())])
}

/// `text.slice(cursor)`（UTF-16 语义；落在代理对中间时把代理替换成 `U+FFFD`）。
fn slice_utf16_suffix(text: &str, utf16_start: usize) -> String {
    let units = utf16_units(text);
    let start = utf16_start.min(units.len());
    units_to_string(&units[start..])
}

/// `segment.length`（UTF-16）。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `findWordBackward(text, cursor, options?)`：向一个词后退后的光标位置（UTF-16 偏移）。
///
/// `cursor` 与返回值都是 UTF-16 偏移（与 TS 的 `string.length`/`slice` 同单位）。
/// 光标可以落在代理对中间：`text[..cursor]` 的半个代理按模块文档的约定换成 `U+FFFD`
/// 后再分段。`cursor == 0` 直接返回 0。
///
/// 过程：
/// 1. 只对光标**前**的文本分段（默认 ICU 规则 + 词典，或 `options.segment`）。
/// 2. 从尾部跳过空白段；空白判定是 `is_whitespace_char`（段内任一字符是 JS 空白即算），
///    但 `is_atomic_segment` 为 true 的段既不算空白、也不按内部标点拆。
/// 3. 最后一个段：原子段整段回退；word-like 段回退到段内**最后一个 ASCII 标点之后**
///    （段内没有标点则整段回退）；其余情况把连续的“非 word-like、非空白”段整体回退。
///
/// 返回值可能等于 `cursor`（例如最后一个标点恰好是段尾）。调用方不应假设每次都会前进。
pub fn find_word_backward(
    text: &str,
    cursor: usize,
    options: Option<&WordNavigationOptions<'_>>,
) -> usize {
    if cursor == 0 {
        return 0;
    }

    let text_before_cursor = slice_utf16_prefix(text, cursor);
    let is_atomic = options.and_then(|opts| opts.is_atomic_segment);
    let mut segments: Vec<WordSegment> = match options.and_then(|opts| opts.segment) {
        Some(segment_fn) => segment_fn(&text_before_cursor),
        None => default_word_segments(&text_before_cursor),
    };
    let mut new_cursor = cursor;

    let atomic = |segment: &str| is_atomic.is_some_and(|f| f(segment));

    // 跳过尾部空白（原子段不算空白）
    while let Some(top) = segments.last() {
        if atomic(&top.segment) || !is_whitespace_char(&top.segment) {
            break;
        }
        new_cursor -= segments
            .pop()
            .map_or(0, |segment| utf16_len(&segment.segment));
    }

    if segments.is_empty() {
        return new_cursor;
    }

    let last = segments.last().expect("checked non-empty");
    if atomic(&last.segment) {
        // 整段原子标记作为单位
        new_cursor -= utf16_len(&last.segment);
    } else if last.is_word_like {
        // 在一个 word-like 段内跳过，保留 ASCII 标点边界
        let segment = last.segment.clone();
        let matches: Vec<usize> = punctuation_regex()
            .find_iter(&segment)
            .map(|m| m.start())
            .collect();
        if matches.is_empty() {
            new_cursor -= utf16_len(&segment);
        } else {
            let last_match = *matches.last().expect("checked non-empty");
            let tail = &segment[last_match..];
            let last_punct_len = punctuation_regex().find(tail).map_or(0, |m| m.end());
            let after = &tail[last_punct_len..];
            new_cursor -= utf16_len(after);
        }
    } else {
        // 连续的非单词、非空白段（标点）整体回退
        while let Some(top) = segments.last() {
            if atomic(&top.segment) || top.is_word_like || is_whitespace_char(&top.segment) {
                break;
            }
            new_cursor -= segments
                .pop()
                .map_or(0, |segment| utf16_len(&segment.segment));
        }
    }

    new_cursor
}

/// `findWordForward(text, cursor, options?)`：向一个词前进后的光标位置（UTF-16 偏移）。
///
/// `cursor` 与返回值都是 UTF-16 偏移。`cursor >= utf16_len(text)` 时直接返回文本长度。
/// 只对光标**后**的文本分段（`slice_utf16_suffix` 同样把落单代理换成 `U+FFFD`）。
///
/// 过程：
/// 1. 跳过前导空白段（`is_whitespace_char` 判定），原子段例外。
/// 2. 原子段整段跨过。
/// 3. word-like 段：停在段内第一个 ASCII 标点**之前**；若该标点就在段首，本次不再前进，
///    返回值等于“跳过前导空白后的位置”（未跳过空白时才等于 `cursor`）。
/// 4. 其余情况：把连续的非 word-like、非空白段整体跨过，停在下一个 word-like、
///    空白或原子段之前。
pub fn find_word_forward(
    text: &str,
    cursor: usize,
    options: Option<&WordNavigationOptions<'_>>,
) -> usize {
    if cursor >= utf16_len(text) {
        return utf16_len(text);
    }

    let text_after_cursor = slice_utf16_suffix(text, cursor);
    let is_atomic = options.and_then(|opts| opts.is_atomic_segment);
    let segments: Vec<WordSegment> = match options.and_then(|opts| opts.segment) {
        Some(segment_fn) => segment_fn(&text_after_cursor),
        None => default_word_segments(&text_after_cursor),
    };
    let atomic = |segment: &str| is_atomic.is_some_and(|f| f(segment));
    let mut iter = segments.into_iter();
    let mut next = iter.next();
    let mut new_cursor = cursor;

    // 跳过前导空白
    while let Some(segment) = &next {
        if atomic(&segment.segment) || !is_whitespace_char(&segment.segment) {
            break;
        }
        new_cursor += utf16_len(&segment.segment);
        next = iter.next();
    }

    let Some(segment) = next else {
        return new_cursor;
    };

    if atomic(&segment.segment) {
        // 整段原子标记作为单位
        new_cursor += utf16_len(&segment.segment);
    } else if segment.is_word_like {
        // 段内第一个 ASCII 标点前停下（保留标点边界）
        match punctuation_regex().find(&segment.segment) {
            Some(m) => new_cursor += utf16_len(&segment.segment[..m.start()]),
            None => new_cursor += utf16_len(&segment.segment),
        }
    } else {
        // 连续的非单词、非空白段（标点）整体跳过
        let mut current = Some(segment);
        while let Some(segment) = current {
            if atomic(&segment.segment)
                || segment.is_word_like
                || is_whitespace_char(&segment.segment)
            {
                break;
            }
            new_cursor += utf16_len(&segment.segment);
            current = iter.next();
        }
    }

    new_cursor
}

// ============================================================================
// KillRing（kill-ring.ts）
// ============================================================================

/// `KillRing.push` 的选项（TS `{ prepend: boolean; accumulate?: boolean }`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PushOptions {
    /// 累积时把新文本放在前面（向后删除）还是后面（向前删除）。
    pub prepend: bool,
    /// 与栈顶合并，而不是新增条目。
    pub accumulate: bool,
}

/// Emacs 风格的 kill ring。
///
/// 记录被 kill 的文本；连续的 kill 可以合并成一条；`rotate()` 支持 yank-pop 循环。
/// 无容量上限；yank / yank-pop 的"粘贴哪一个 + 循环替换"逻辑在消费者（editor/input）里，
/// 本类型只有栈语义。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KillRing {
    ring: Vec<String>,
}

impl KillRing {
    /// 空环。
    pub fn new() -> Self {
        Self::default()
    }

    /// `push(text, opts)`：空串直接返回（不产生条目）。
    ///
    /// `accumulate` 为 true 且环非空时与最新条目合并、不新增条目：`prepend` 决定新文本
    /// 放在旧文本之前（向后删除）还是之后（向前删除）。其他情况把 `text` 追加为新条目。
    pub fn push(&mut self, text: &str, options: PushOptions) {
        if text.is_empty() {
            return;
        }
        if options.accumulate && !self.ring.is_empty() {
            let last = self.ring.pop().expect("checked non-empty");
            if options.prepend {
                self.ring.push(format!("{text}{last}"));
            } else {
                self.ring.push(format!("{last}{text}"));
            }
        } else {
            self.ring.push(text.to_string());
        }
    }

    /// `peek()`：最新一条，不修改环形缓冲。
    pub fn peek(&self) -> Option<&str> {
        self.ring.last().map(String::as_str)
    }

    /// `rotate()`：把最后一条移到最前（yank-pop 循环）；长度 ≤ 1 时是 no-op。
    pub fn rotate(&mut self) {
        if self.ring.len() > 1
            && let Some(last) = self.ring.pop()
        {
            self.ring.insert(0, last);
        }
    }

    /// `length` getter。
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// `length === 0`。
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

// ============================================================================
// UndoStack（undo-stack.ts）
// ============================================================================

/// 克隆式撤销栈：`push` 存快照的副本，`pop` 直接返回已脱离的快照。
///
/// TS 用 `structuredClone`（深拷贝）；Rust 用 `S: Clone`，对本库用到的普通结构等价。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoStack<S> {
    stack: Vec<S>,
}

impl<S: Clone> UndoStack<S> {
    /// 空栈。
    pub fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// `push(state)`：压入 `state` 的深拷贝。
    ///
    /// 保存的是 `state.clone()` 而非对原对象的引用，因此后续修改原对象不会影响已入栈
    /// 的快照——除非 `S` 的 `Clone` 本身共享内部状态（例如含 `Rc`/`Arc`）。
    pub fn push(&mut self, state: &S) {
        self.stack.push(state.clone());
    }

    /// `pop()`：弹出最近一次快照。
    pub fn pop(&mut self) -> Option<S> {
        self.stack.pop()
    }

    /// `clear()`：清空快照。
    pub fn clear(&mut self) {
        self.stack.clear();
    }

    /// `length` getter。
    pub fn len(&self) -> usize {
        self.stack.len()
    }

    /// `length === 0`。
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
}
