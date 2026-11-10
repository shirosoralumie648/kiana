//! components/session-selector-search.ts。
//!
//! `/resume` 会话选择器的纯搜索/排序函数：token/短语/`re:` 正则解析、分层评分、
//! name filter 与 stable 排序。没有 UI 依赖，便于单独测试。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/session-selector-search.ts`。
//!
//! 与 TS 的差异（只影响性能/表示，不改变可观察结果）：
//! * TS 用 `WeakMap<SessionInfo, LowercaseCache>` 跨调用缓存消息小写文本。Rust 的
//!   `SessionInfo` 是按值传递的普通结构，没有稳定对象标识；这里改为**每次
//!   `filter_and_sort_sessions` 调用内**按会话惰性计算一次（`MatchContext`），
//!   同一会话在一次过滤中不会重复 `to_lowercase()`。
//! * TS 返回 `SessionInfo[]`（引用数组）；Rust 返回 `Vec<&SessionInfo>`，避免克隆可能
//!   数 MB 的 `all_messages_text`。
//!
//! `re:` 模式由 `translate_js_regex` 翻译成 `regex` crate 语法：`$`/`i` 大小写折叠
//! （JS `Canonicalize`）、字符类区间、Annex B 八进制/identity escape、量词合法性都按
//! JS 非 `u` 模式对齐。`regex` crate 无法表达的 JS 语义仍会不同（按「无匹配」处理）：
//! lookaround、反向引用、孤立代理项，以及依赖 UTF-16 code unit 的匹配
//! （`.`/量词对星平面字符、`[😀]` 这类含星平面字符的字符类）。`m` 标志下 Rust 只把
//! `\n` 当作行终止符（JS 还认 `\r`/U+2028/U+2029）。
//!
//! # 已知偏差（当前实现）
//! 修饰符组（`(?i:...)` / `(?-i:...)` / `(?m:...)` / `(?s:...)`）在翻译期直接改写
//! i/m/s 标志；`)` 分支只做括号配对检查、不把标志写回，因此这些效果会一直延续到模式
//! 结尾，而不是像 JS 那样只作用于组内。现有测试只覆盖“组位于模式末尾”的用法，没有
//! 覆盖组后还有字符的情况。
//!
//! # 调用关系
//! 生产代码里的调用方是 `components::session_selector` 的 `filter_sessions`：它先按
//! [`NameFilter`] 预筛会话，再调用 [`filter_and_sort_session_refs`]，最后用裸指针把返回
//! 的引用映射回原索引。`SortMode::Threaded` + 空查询的树形展示由调用方用
//! `build_session_tree` 完成，不经过本模块。集成测试 `tests/session_selector_search.rs`
//! 则直接调用公开封装 [`filter_and_sort_sessions`]。
//!
//! 公开面里 [`SortMode`]、[`NameFilter`]、[`ParsedSearchQuery`]、[`MatchResult`] 与
//! [`parse_search_query`]、[`match_session`]、[`has_session_name`]
//! 以及 [`filter_and_sort_sessions`] 经 `components::index` 再导出到 crate 根
//! （`tests/public_api.rs` 用 `type_name_of_val` 断言两处再导出的是同一项）；
//! `SearchToken`、`SearchTokenKind`、`ParsedSearchMode`、
//! [`filter_and_sort_session_refs`] 与 [`normalize_whitespace_lower`] 只能经模块路径访问。
//!
//! # 执行流程
//! 1. [`parse_search_query`]：先按 JS 空白 `trim`；`re:` 前缀走
//!    `translate_js_regex` + `Regex::new`，其余按双引号切成短语/模糊 token；
//! 2. [`filter_and_sort_session_refs`]：空查询直接返回输入序，解析出错返回空；
//! 3. 每个会话对每个 token 求分（见 [`MatchResult`]），token 之间是 AND；
//! 4. `Recent` 保持输入序；`Relevance`（以及带查询的 `Threaded`）按分数升序、同分按
//!    `modified` 降序做稳定排序。
//!
//! # 分数模型
//! 每个命中字段先归一化到层级（name < 其他文本 < fuzzy），层级乘以 `TIER_WEIGHT` 后
//! 加上该字段的 fuzzy 分数（id / 消息 / cwd 的命中恒加 0）。比较时先看层级、再比同层
//! 分数；这依赖单次 `fuzzy_match` 分数小于 `TIER_WEIGHT`（模糊匹配只作用于 name 与
//! cwd 两个短字段）。一个 token 只在**单个字段**内匹配，跨字段拼接不算命中。
//!
//! # 修改影响
//! 返回的是 `&SessionInfo` 引用，调用方依赖它们指向输入切片中的元素来反查索引
//! （`session_selector` 用 `*const SessionInfo` 建映射）；改成返回克隆会让指针反查
//! 全部落到兜底索引 0。

use kiana_session_core::SessionInfo;
use kiana_tui::fuzzy::fuzzy_match;
use kiana_tui::text::is_js_whitespace;

use regex::Regex;
use std::sync::OnceLock;

/// 一个码点闭区间 `[lo, hi]`（含端点）。
type CharInterval = (u32, u32);

/// Unicode 码点上限。
const MAX_CODE_POINT: u32 = 0x10FFFF;

/// 匹配任意字符的 Rust 片段（JS 的 `[^]`，包含换行）。
const ANY_CHAR: &str = r"[\s\S]";

/// 永远不匹配任何字符的 Rust 片段（JS 的 `[]`）。
const NEVER_MATCH: &str = r"[^\s\S]";

/// JS 非 `u` 模式下 `.` 不匹配的行终止符集合。
const DOT: &str = r"[^\n\r\u{2028}\u{2029}]";

/// JS `\s`（**非 unicode** 模式，Annex B）的码点区间。
///
/// ECMAScript 的 `WhiteSpace` + `LineTerminator`：`\t \n \v \f \r 空格 NBSP U+1680
/// U+2000–U+200A U+2028 U+2029 U+202F U+205F U+3000 U+FEFF`。注意它**包含** BOM（U+FEFF）
/// 而**不含** U+0085（NEL）；Rust `regex` 的 Unicode `\s` 恰好相反，所以必须显式展开。
const JS_SPACE_INTERVALS: &[CharInterval] = &[
    (0x09, 0x0D),
    (0x20, 0x20),
    (0xA0, 0xA0),
    (0x1680, 0x1680),
    (0x2000, 0x200A),
    (0x2028, 0x2029),
    (0x202F, 0x202F),
    (0x205F, 0x205F),
    (0x3000, 0x3000),
    (0xFEFF, 0xFEFF),
];

/// ASCII 数字集合。
const DIGIT_INTERVALS: &[CharInterval] = &[(0x30, 0x39)];

/// JS `\w`（非 unicode 模式）的 ASCII 集合。
const WORD_INTERVALS: &[CharInterval] = &[(0x30, 0x39), (0x41, 0x5A), (0x5F, 0x5F), (0x61, 0x7A)];

/// 排序并合并相邻/重叠的码点区间。
fn normalize_intervals(mut intervals: Vec<CharInterval>) -> Vec<CharInterval> {
    intervals.sort_unstable();
    let mut merged: Vec<CharInterval> = Vec::with_capacity(intervals.len());
    for (lo, hi) in intervals {
        if let Some(last) = merged.last_mut()
            && lo <= last.1.saturating_add(1)
        {
            last.1 = last.1.max(hi);
            continue;
        }
        merged.push((lo, hi));
    }
    merged
}

/// 区间集合是否包含某个码点。
fn interval_contains(intervals: &[CharInterval], ch: char) -> bool {
    let code = ch as u32;
    intervals.iter().any(|(lo, hi)| *lo <= code && code <= *hi)
}

/// 区间集合在 `[0, U+10FFFF]` 上的补集（输入必须已排序且不相交）。
fn complement_intervals(intervals: &[CharInterval]) -> Vec<CharInterval> {
    let mut result = Vec::new();
    let mut next = 0u32;
    for (lo, hi) in intervals {
        if *lo > next {
            result.push((next, lo - 1));
        }
        if *hi >= MAX_CODE_POINT {
            return result;
        }
        next = hi + 1;
    }
    if next <= MAX_CODE_POINT {
        result.push((next, MAX_CODE_POINT));
    }
    result
}

/// JS 非 `u` 模式的 `Canonicalize`：单码点大写映射（U+0131 / U+017F 例外）。
///
/// 非 `u` 模式按 UTF-16 code unit 匹配，星平面字符不参与大小写折叠。
fn js_canonicalize(ch: char) -> char {
    if ch as u32 > 0xFFFF || ch == '\u{0131}' || ch == '\u{017F}' {
        return ch;
    }
    let mut upper = ch.to_uppercase();
    let Some(first) = upper.next() else {
        return ch;
    };
    if upper.next().is_some() { ch } else { first }
}

/// 所有 `(c, Canonicalize(c))` 对（仅 BMP；非 `u` 模式不折叠星平面字符）。
fn js_case_pairs() -> &'static [(char, char)] {
    static PAIRS: OnceLock<Vec<(char, char)>> = OnceLock::new();
    PAIRS.get_or_init(|| {
        let mut pairs = Vec::new();
        for code in 0..=0xFFFFu32 {
            let Some(ch) = char::from_u32(code) else {
                continue;
            };
            let canonical = js_canonicalize(ch);
            if canonical != ch {
                pairs.push((ch, canonical));
            }
        }
        pairs
    })
}

/// 把区间集合对 `i` 标志做大小写闭包。
///
/// JS 的等价关系是 `Canonicalize(a) == Canonicalize(b)`；用 `(c, canon(c))` 对做闭包
/// 即可精确复刻（例如 `µ`/`μ`/`Μ` 同属一类，而 `ſ`/`K`（开尔文符号 U+212A）不参与）。
fn close_case(mut intervals: Vec<CharInterval>) -> Vec<CharInterval> {
    intervals = normalize_intervals(intervals);
    loop {
        let mut added = false;
        for (ch, canonical) in js_case_pairs() {
            let in_ch = interval_contains(&intervals, *ch);
            let in_canonical = interval_contains(&intervals, *canonical);
            if in_ch != in_canonical {
                let missing = if in_ch { *canonical } else { *ch };
                intervals.push((missing as u32, missing as u32));
                added = true;
            }
        }
        if !added {
            return intervals;
        }
        intervals = normalize_intervals(intervals);
    }
}

/// 把码点区间集合渲染成 Rust 正则字符类。
fn emit_intervals(intervals: &[CharInterval], negated: bool) -> String {
    if intervals.is_empty() {
        return if negated {
            ANY_CHAR.to_string()
        } else {
            NEVER_MATCH.to_string()
        };
    }
    if intervals.len() == 1 && intervals[0] == (0, MAX_CODE_POINT) {
        return if negated {
            NEVER_MATCH.to_string()
        } else {
            ANY_CHAR.to_string()
        };
    }
    let mut out = String::from("[");
    if negated {
        out.push('^');
    }
    for (lo, hi) in intervals {
        if lo == hi {
            out.push_str(&format!("\\x{{{lo:X}}}"));
        } else {
            out.push_str(&format!("\\x{{{lo:X}}}-\\x{{{hi:X}}}"));
        }
    }
    out.push(']');
    out
}

/// 转义成 Rust 正则里的字面量字符（字符类之外）。
fn escape_literal(ch: char) -> String {
    if matches!(
        ch,
        '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
    ) {
        format!("\\{ch}")
    } else if (ch as u32) < 0x20 || ch == '\u{7F}' {
        format!("\\x{{{:X}}}", ch as u32)
    } else {
        ch.to_string()
    }
}

/// 字面量原子；`i` 开启时展开成大小写等价类（JS 的 Canonicalize 语义）。
fn emit_char_atom(ch: char, case_insensitive: bool) -> String {
    if !case_insensitive {
        return escape_literal(ch);
    }
    let closed = close_case(vec![(ch as u32, ch as u32)]);
    if closed.len() == 1 && closed[0].0 == closed[0].1 {
        return escape_literal(ch);
    }
    emit_intervals(&closed, false)
}

/// 解析 Annex B 的 LegacyOctalEscapeSequence；`backslash` 指向 `\`。
///
/// 返回（字符，从 `\` 起消耗的字符数）。首位 0–3 最多三位八进制数字，4–7 最多两位。
fn legacy_octal(chars: &[char], backslash: usize) -> (char, usize) {
    let first = chars[backslash + 1] as u32 - '0' as u32;
    let max_digits = if first <= 3 { 3 } else { 2 };
    let mut value = first;
    let mut consumed = 2;
    // `consumed` 从「反斜杠 + 首位数字」开始，因此还可再读 `max_digits - 1` 位。
    while consumed <= max_digits {
        let Some(digit) = chars
            .get(backslash + consumed)
            .and_then(|ch| ch.to_digit(8))
        else {
            break;
        };
        value = value * 8 + digit;
        consumed += 1;
    }
    (char::from_u32(value).unwrap_or('\0'), consumed)
}

/// 读取定长十六进制数字；不足返回 `None`。
fn hex_value(chars: &[char], start: usize, count: usize) -> Option<u32> {
    let mut value = 0u32;
    for offset in 0..count {
        value = value * 16 + chars.get(start + offset)?.to_digit(16)?;
    }
    Some(value)
}

/// `\c` 之后的字符：字母、数字或 `_`（Annex B 的 ClassControlLetter）。
fn class_control_letter_value(ch: char) -> Option<u32> {
    if ch.is_ascii_alphabetic() || ch.is_ascii_digit() || ch == '_' {
        Some((ch.to_ascii_uppercase() as u32) % 32)
    } else {
        None
    }
}

/// 字符类里的一个原子。
enum ClassAtom {
    /// 单个字符（可作为区间端点）。
    Char(char),
    /// 多字符集合（`\d` 等类转义）。
    Set(Vec<CharInterval>),
}

/// 解析字符类里的一个原子；`index` 指向原子起点。
fn parse_class_atom(chars: &[char], index: usize) -> Result<(ClassAtom, usize), String> {
    let Some(&ch) = chars.get(index) else {
        return Err("Unterminated character class".to_string());
    };
    if ch != '\\' {
        return Ok((ClassAtom::Char(ch), index + 1));
    }
    let Some(&escaped) = chars.get(index + 1) else {
        return Err("\\ at end of pattern".to_string());
    };
    let atom = match escaped {
        'b' => ClassAtom::Char('\u{8}'),
        'B' => ClassAtom::Char('B'),
        'd' => ClassAtom::Set(DIGIT_INTERVALS.to_vec()),
        'D' => ClassAtom::Set(complement_intervals(DIGIT_INTERVALS)),
        'w' => ClassAtom::Set(WORD_INTERVALS.to_vec()),
        'W' => ClassAtom::Set(complement_intervals(WORD_INTERVALS)),
        's' => ClassAtom::Set(JS_SPACE_INTERVALS.to_vec()),
        'S' => ClassAtom::Set(complement_intervals(JS_SPACE_INTERVALS)),
        // 无 u 标志时 `\p`/`\P` 与 `\k` 都是 identity escape；`\8`/`\9` 是字面数字。
        'p' | 'P' | 'k' | '8' | '9' => ClassAtom::Char(escaped),
        'n' => ClassAtom::Char('\n'),
        'r' => ClassAtom::Char('\r'),
        't' => ClassAtom::Char('\t'),
        'f' => ClassAtom::Char('\u{c}'),
        'v' => ClassAtom::Char('\u{b}'),
        '0'..='7' => {
            let (value, consumed) = legacy_octal(chars, index);
            return Ok((ClassAtom::Char(value), index + consumed));
        }
        'c' => match chars
            .get(index + 2)
            .and_then(|ch| class_control_letter_value(*ch))
        {
            Some(value) => {
                return Ok((
                    ClassAtom::Char(char::from_u32(value).unwrap_or('\0')),
                    index + 3,
                ));
            }
            // `\c` 后不是控制字母：字面反斜杠 + `c`。
            None => ClassAtom::Set(vec![('\\' as u32, '\\' as u32), ('c' as u32, 'c' as u32)]),
        },
        'x' => match hex_value(chars, index + 2, 2) {
            Some(value) => {
                return Ok((
                    ClassAtom::Char(char::from_u32(value).unwrap_or('\0')),
                    index + 4,
                ));
            }
            None => ClassAtom::Char('x'),
        },
        'u' => match hex_value(chars, index + 2, 4) {
            Some(value) => {
                let Some(value) = char::from_u32(value) else {
                    return Err("Invalid Unicode escape".to_string());
                };
                return Ok((ClassAtom::Char(value), index + 6));
            }
            None => ClassAtom::Char('u'),
        },
        _ => ClassAtom::Char(escaped),
    };
    Ok((atom, index + 2))
}

/// 解析一个 JS（非 `u`）字符类；`start` 指向 `[`。
///
/// 返回（Rust 正则片段，`]` 之后的下标）。区间顺序按 UTF-16 code unit 校验，
/// `[a--b]` 这类 JS SyntaxError 在这里报错；`-` 只有在两侧都是单字符时才是区间。
///
/// 注意：实现实际比较的是 `char as u32`（码点）。端点都在 BMP 内时两种顺序一致；
/// 端点含星平面字符时顺序判定可能与 JS 不同（星平面字符类的匹配差异已在模块文档列出）。
fn parse_class(
    chars: &[char],
    start: usize,
    case_insensitive: bool,
) -> Result<(String, usize), String> {
    let mut index = start + 1;
    let negated = chars.get(index) == Some(&'^');
    if negated {
        index += 1;
    }
    // `[]` / `[^]`：紧跟的 `]` 关闭空类。
    if chars.get(index) == Some(&']') {
        return Ok((emit_intervals(&[], negated), index + 1));
    }
    // 阶段 1：逐个解析原子。类转义（`\d` 等）直接展开成区间集合；单字符先留着，
    // 看后面是否跟着 `-` 构成区间。
    let mut intervals: Vec<CharInterval> = Vec::new();
    loop {
        match chars.get(index) {
            None => return Err("Unterminated character class".to_string()),
            Some(']') => {
                index += 1;
                break;
            }
            Some(_) => {}
        }
        let (atom, next) = parse_class_atom(chars, index)?;
        index = next;
        let low = match atom {
            ClassAtom::Set(set) => {
                intervals.extend(set);
                continue;
            }
            ClassAtom::Char(ch) => ch,
        };
        // `A - B`：只有 `-` 后面是单字符时才构成区间；类转义（`\d` 等）让 `-` 成为字面量。
        if chars.get(index) == Some(&'-') && chars.get(index + 1).is_some_and(|ch| *ch != ']') {
            let (second, next) = parse_class_atom(chars, index + 1)?;
            match second {
                ClassAtom::Char(high) => {
                    if (low as u32) > (high as u32) {
                        return Err("Range out of order in character class".to_string());
                    }
                    intervals.push((low as u32, high as u32));
                    index = next;
                    continue;
                }
                ClassAtom::Set(set) => {
                    intervals.push((low as u32, low as u32));
                    intervals.push(('-' as u32, '-' as u32));
                    intervals.extend(set);
                    index = next;
                    continue;
                }
            }
        }
        intervals.push((low as u32, low as u32));
    }
    // 阶段 2：合并重叠区间 →（`i` 时）大小写闭包 →（`[^...]` 时）取补集。
    // 顺序不能反：先取补再闭包时，`[^a]` 的补集包含 `A`，闭包会把 `a` 也加回来。
    let mut result = normalize_intervals(intervals);
    if case_insensitive {
        result = close_case(result);
    }
    if negated {
        result = complement_intervals(&result);
    }
    Ok((emit_intervals(&result, false), index))
}

/// 统计捕获组与具名捕获组数量（JS 规则；用于区分反向引用与八进制转义）。
///
/// 扫描时跳过 `\` 转义的目标字符与字符类内部（`[(]` 不算组）；`(?:`、`(?=`、`(?!`、
/// `(?<=`、`(?<!` 都不计数，`(?<name>` 同时计入 `named`。这两个数字只用于
/// `\1`–`\9` 与 `\k` 的“反向引用还是 identity escape”判定。
fn count_groups(chars: &[char]) -> (usize, usize) {
    let mut captures = 0usize;
    let mut named = 0usize;
    let mut index = 0;
    let mut in_class = false;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            index += 2;
            continue;
        }
        if in_class {
            if ch == ']' {
                in_class = false;
            }
            index += 1;
            continue;
        }
        if ch == '[' {
            if chars.get(index + 1) == Some(&'^') && chars.get(index + 2) == Some(&']') {
                index += 3;
                continue;
            }
            if chars.get(index + 1) == Some(&']') {
                index += 2;
                continue;
            }
            in_class = true;
            index += 1;
            continue;
        }
        if ch == '(' {
            if chars.get(index + 1) == Some(&'?') {
                if chars.get(index + 2) == Some(&'<') {
                    match chars.get(index + 3) {
                        Some('=') | Some('!') => {}
                        _ => {
                            captures += 1;
                            named += 1;
                        }
                    }
                }
            } else {
                captures += 1;
            }
        }
        index += 1;
    }
    (captures, named)
}

/// 解析 `(?ims-ims:` 修饰符组；返回（`:` 之后的下标，添加/移除的 i、m、s）。
#[allow(clippy::type_complexity)]
fn parse_modifier_group(
    chars: &[char],
    open: usize,
) -> Result<(usize, [bool; 3], [bool; 3]), String> {
    let mut index = open + 2;
    let mut add = [false; 3];
    let mut remove = [false; 3];
    let mut in_remove = false;
    let mut saw_flag = false;
    loop {
        match chars.get(index) {
            None => return Err("Invalid group".to_string()),
            Some(':') => {
                // `(?-:a)` 在 JS 里是 SyntaxError；`(?i-:a)` 合法（移除侧为空）。
                if !saw_flag {
                    return Err("Invalid group".to_string());
                }
                return Ok((index + 1, add, remove));
            }
            Some('-') => {
                if in_remove {
                    return Err("Invalid group".to_string());
                }
                in_remove = true;
                index += 1;
            }
            Some(flag @ ('i' | 'm' | 's')) => {
                let slot = match flag {
                    'i' => 0,
                    'm' => 1,
                    _ => 2,
                };
                // 同一 flag 在添加/移除两侧重复（`(?i-i:a)`）在 JS 里也是 SyntaxError。
                if add[slot] || remove[slot] {
                    return Err("Repeated flag in flag group".to_string());
                }
                if in_remove {
                    remove[slot] = true;
                } else {
                    add[slot] = true;
                }
                saw_flag = true;
                index += 1;
            }
            Some(_) => return Err("Invalid group".to_string()),
        }
    }
}

/// 上一个 token 在量词合法性检查里的类别。
#[derive(Clone, Copy, PartialEq, Eq)]
enum LastToken {
    /// 模式开头或 `|` 之后：量词非法。
    None,
    /// 可量化原子。
    Atom,
    /// 断言（`^` `$` `\b` `\B`）：不可量化。
    Assertion,
    /// 量词；紧随的 `?` 是惰性修饰符。
    Quantifier,
    /// 惰性量词；不能再接量词。
    LazyQuantifier,
}

/// `check_quantifier`：JS 的 "Nothing to repeat" 规则。
fn check_quantifier(last: LastToken, question_mark: bool) -> Result<LastToken, String> {
    match (last, question_mark) {
        (LastToken::Atom, _) => Ok(LastToken::Quantifier),
        (LastToken::Quantifier, true) => Ok(LastToken::LazyQuantifier),
        _ => Err("Nothing to repeat".to_string()),
    }
}

/// 转义一个字符类之外的 `\` 序列；`index` 指向 `\`。
///
/// 返回（Rust 正则片段，消耗的字符数，是否是断言）。
fn translate_escape_outside(
    chars: &[char],
    index: usize,
    capture_count: usize,
    named_count: usize,
    case_insensitive: bool,
) -> Result<(String, usize, bool), String> {
    let Some(&escaped) = chars.get(index + 1) else {
        return Err("\\ at end of pattern".to_string());
    };
    let atom = |ch: char| Ok((emit_char_atom(ch, case_insensitive), 2, false));
    match escaped {
        // ASCII 词边界：JS 无 u 标志时把 CJK/重音字母视为非单词字符。
        'b' => Ok(("(?-u:\\b)".to_string(), 2, true)),
        'B' => Ok(("(?-u:\\B)".to_string(), 2, true)),
        'd' => Ok((emit_intervals(DIGIT_INTERVALS, false), 2, false)),
        'D' => Ok((
            emit_intervals(&complement_intervals(DIGIT_INTERVALS), false),
            2,
            false,
        )),
        'w' => Ok((emit_intervals(WORD_INTERVALS, false), 2, false)),
        'W' => Ok((
            emit_intervals(&complement_intervals(WORD_INTERVALS), false),
            2,
            false,
        )),
        's' => Ok((emit_intervals(JS_SPACE_INTERVALS, false), 2, false)),
        'S' => Ok((
            emit_intervals(&complement_intervals(JS_SPACE_INTERVALS), false),
            2,
            false,
        )),
        // 无 u 标志时 `\p`/`\P` 是 identity escape（字面 p/P）。
        'p' | 'P' => atom(escaped),
        'n' => atom('\n'),
        'r' => atom('\r'),
        't' => atom('\t'),
        'f' => atom('\u{c}'),
        'v' => atom('\u{b}'),
        '0' => {
            let (value, consumed) = legacy_octal(chars, index);
            Ok((emit_char_atom(value, case_insensitive), consumed, false))
        }
        '1'..='9' => {
            // Annex B：十进制数不超过捕获组数量时是反向引用；否则是 legacy octal。
            let mut end = index + 1;
            let mut number = 0usize;
            while let Some(digit) = chars.get(end).and_then(|ch| ch.to_digit(10)) {
                number = number.saturating_mul(10).saturating_add(digit as usize);
                end += 1;
            }
            if number >= 1 && number <= capture_count {
                // Rust regex 不支持反向引用：整个查询按「无匹配」处理。
                return Err("Invalid backreference".to_string());
            }
            // `\8`/`\9` 没有八进制含义，是 identity escape（字面数字）。
            let first = chars[index + 1];
            if first == '8' || first == '9' {
                return Ok((emit_char_atom(first, case_insensitive), 2, false));
            }
            let (value, consumed) = legacy_octal(chars, index);
            Ok((emit_char_atom(value, case_insensitive), consumed, false))
        }
        'k' => {
            // 有具名捕获组时 `\k<name>` 是（Rust 不支持的）反向引用；否则是 identity escape。
            if named_count > 0 {
                return Err("Invalid named capture referenced".to_string());
            }
            atom('k')
        }
        'c' => match chars.get(index + 2) {
            Some(letter) if letter.is_ascii_alphabetic() => {
                let value = (*letter as u32) % 32;
                Ok((
                    emit_char_atom(char::from_u32(value).unwrap_or('\0'), case_insensitive),
                    3,
                    false,
                ))
            }
            // `\c` 后面不是字母：整个序列按字面反斜杠 + c 处理。
            _ => Ok(("\\\\c".to_string(), 2, false)),
        },
        'x' => match hex_value(chars, index + 2, 2) {
            Some(value) => Ok((
                emit_char_atom(char::from_u32(value).unwrap_or('\0'), case_insensitive),
                4,
                false,
            )),
            None => atom('x'),
        },
        'u' => match hex_value(chars, index + 2, 4) {
            Some(value) => {
                if (0xD800..=0xDBFF).contains(&value) {
                    // 高代理项：与紧邻的 `\uXXXX` 低代理项合并成星平面字符。
                    if chars.get(index + 6) == Some(&'\\')
                        && chars.get(index + 7) == Some(&'u')
                        && let Some(low) = hex_value(chars, index + 8, 4)
                        && (0xDC00..=0xDFFF).contains(&low)
                    {
                        let code = 0x10000 + ((value - 0xD800) << 10) + (low - 0xDC00);
                        if let Some(combined) = char::from_u32(code) {
                            return Ok((emit_char_atom(combined, case_insensitive), 12, false));
                        }
                    }
                    // 孤立代理项无法在 Rust 字符串里表示：按无匹配处理。
                    return Err("Invalid Unicode escape".to_string());
                }
                let Some(value) = char::from_u32(value) else {
                    return Err("Invalid Unicode escape".to_string());
                };
                Ok((emit_char_atom(value, case_insensitive), 6, false))
            }
            None => atom('u'),
        },
        // 其余全是 identity escape（Annex B）：`\A`/`\z`/`\-` 等都是字面字符。
        _ => atom(escaped),
    }
}

/// `chars[start] == '{'` 时，若构成合法 JS 量词 `{n}` / `{n,}` / `{n,m}` 返回
/// （字符数，下界，上界）。
fn quantifier_at(chars: &[char], start: usize) -> Option<(usize, u64, Option<u64>)> {
    let mut index = start + 1;
    let digits_start = index;
    let mut min = 0u64;
    while let Some(digit) = chars.get(index).and_then(|ch| ch.to_digit(10)) {
        min = min.saturating_mul(10).saturating_add(digit as u64);
        index += 1;
    }
    if index == digits_start {
        return None;
    }
    if chars.get(index) == Some(&',') {
        index += 1;
        let max_start = index;
        let mut max = 0u64;
        while let Some(digit) = chars.get(index).and_then(|ch| ch.to_digit(10)) {
            max = max.saturating_mul(10).saturating_add(digit as u64);
            index += 1;
        }
        if chars.get(index) != Some(&'}') {
            return None;
        }
        if index == max_start {
            return Some((index + 1 - start, min, None));
        }
        return Some((index + 1 - start, min, Some(max)));
    }
    if chars.get(index) != Some(&'}') {
        return None;
    }
    Some((index + 1 - start, min, Some(min)))
}

/// 把 JS（无 `u` 标志）的正则源翻译成 `regex` crate 的等价源。
///
/// 与 JS 的差异只剩 Rust 正则引擎本身不支持、且无法在翻译期消除的构造
/// （lookaround / 反向引用 / 孤立代理项）：这些按「无匹配」（编译失败）处理。
fn translate_js_regex(pattern: &str) -> Result<String, String> {
    let chars: Vec<char> = pattern.chars().collect();
    let (capture_count, named_count) = count_groups(&chars);
    let mut out = String::with_capacity(pattern.len() * 2);
    let mut index = 0;
    // 顶层是 `new RegExp(pattern, "i")`；`(?-i:...)` 等修饰符组会改写这些标志
    // （改写的作用范围见 `)` 分支的说明）。
    let mut case_insensitive = true;
    let mut multiline = false;
    let mut dot_all = false;
    let mut flag_stack: Vec<(bool, bool, bool)> = Vec::new();
    let mut last = LastToken::None;

    // 单遍扫描：每个分支把当前构造展开成 Rust 片段，同时维护两件事：
    // * `flag_stack`：进入 `(` 时压入当前 i/m/s，`)` 时弹出做配对检查
    //   （弹出值当前没有写回，见 `)` 分支）；
    // * `last`：上一个产出的 token 类别，供 `*`/`+`/`?`/`{n}` 的
    //   “Nothing to repeat”检查。
    while index < chars.len() {
        let ch = chars[index];
        match ch {
            '\\' => {
                let (fragment, consumed, is_assertion) = translate_escape_outside(
                    &chars,
                    index,
                    capture_count,
                    named_count,
                    case_insensitive,
                )?;
                out.push_str(&fragment);
                index += consumed;
                last = if is_assertion {
                    LastToken::Assertion
                } else {
                    LastToken::Atom
                };
            }
            '[' => {
                let (fragment, next) = parse_class(&chars, index, case_insensitive)?;
                out.push_str(&fragment);
                index = next;
                last = LastToken::Atom;
            }
            '(' => {
                flag_stack.push((case_insensitive, multiline, dot_all));
                let mut next = index + 1;
                if chars.get(index + 1) == Some(&'?') {
                    match chars.get(index + 2) {
                        Some(':') => next = index + 3,
                        // lookaround 无法用 Rust regex 表达：编译失败 -> 无匹配。
                        Some('=') | Some('!') => {
                            return Err("Invalid group".to_string());
                        }
                        Some('<') => match chars.get(index + 3) {
                            Some('=') | Some('!') => {
                                return Err("Invalid group".to_string());
                            }
                            _ => {
                                // 具名捕获组：扫描到名字后的 `>` 直接跳过，名字内容不校验；
                                // 该组与普通捕获组一样最终输出成 `(?:`。
                                let mut close = index + 3;
                                while close < chars.len() && chars[close] != '>' {
                                    close += 1;
                                }
                                if close >= chars.len() {
                                    return Err("Invalid group".to_string());
                                }
                                next = close + 1;
                            }
                        },
                        Some('i') | Some('m') | Some('s') | Some('-') => {
                            let (after, add, remove) = parse_modifier_group(&chars, index)?;
                            if add[0] {
                                case_insensitive = true;
                            } else if remove[0] {
                                case_insensitive = false;
                            }
                            if add[1] {
                                multiline = true;
                            } else if remove[1] {
                                multiline = false;
                            }
                            if add[2] {
                                dot_all = true;
                            } else if remove[2] {
                                dot_all = false;
                            }
                            next = after;
                        }
                        _ => return Err("Invalid group".to_string()),
                    }
                }
                // 捕获组一律降级成非捕获组：我们不需要捕获，反向引用也已按无匹配处理。
                out.push_str("(?:");
                index = next;
                last = LastToken::None;
            }
            ')' => {
                // 这里只检查括号配对：弹出的 (i, m, s) 没有写回上面的变量，所以修饰符
                // 组的效果会一直保留到模式结尾，而不是像 JS 那样只作用于组内。
                if flag_stack.pop().is_none() {
                    return Err("Unmatched ')'".to_string());
                }
                out.push(')');
                index += 1;
                last = LastToken::Atom;
            }
            '|' => {
                out.push('|');
                index += 1;
                last = LastToken::None;
            }
            '.' => {
                out.push_str(if dot_all { ANY_CHAR } else { DOT });
                index += 1;
                last = LastToken::Atom;
            }
            '^' => {
                out.push_str(if multiline { "(?m:^)" } else { "^" });
                index += 1;
                last = LastToken::Assertion;
            }
            // JS 无 m 标志时 `$` 只匹配输入末尾（Rust 同）；m 标志下 Rust 只认 `\n`，
            // 与 JS 的 `\r`/U+2028/U+2029 仍有细微差异。
            '$' => {
                out.push_str(if multiline { "(?m:$)" } else { "$" });
                index += 1;
                last = LastToken::Assertion;
            }
            '*' | '+' | '?' => {
                last = check_quantifier(last, ch == '?')?;
                out.push(ch);
                index += 1;
            }
            '{' => match quantifier_at(&chars, index) {
                Some((length, min, max)) => {
                    if let Some(max) = max
                        && min > max
                    {
                        return Err("numbers out of order in {} quantifier".to_string());
                    }
                    last = check_quantifier(last, false)?;
                    out.extend(chars[index..index + length].iter());
                    index += length;
                }
                None => {
                    out.push_str(&escape_literal('{'));
                    index += 1;
                    last = LastToken::Atom;
                }
            },
            '}' => {
                out.push_str(&escape_literal('}'));
                index += 1;
                last = LastToken::Atom;
            }
            ']' => {
                out.push_str(&escape_literal(']'));
                index += 1;
                last = LastToken::Atom;
            }
            _ => {
                out.push_str(&emit_char_atom(ch, case_insensitive));
                index += 1;
                last = LastToken::Atom;
            }
        }
    }
    Ok(out)
}

/// 排序模式（TS `SortMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    /// 无搜索时按父子关系展示为树；带搜索时等同 [`SortMode::Relevance`]。
    ///
    /// 树本身不在这里构建：调用方（`session_selector::filter_sessions`）在空查询时走
    /// `build_session_tree`；本模块只把带非空查询的 `Threaded` 当作 `Relevance` 排序。
    Threaded,
    /// 只过滤、保持输入顺序。
    Recent,
    /// 按分数升序，同分按 `modified` 降序。
    Relevance,
}

/// 名称过滤（TS `NameFilter`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameFilter {
    /// 所有会话。
    All,
    /// 仅 `name.trim()` 非空的会话。
    Named,
}

/// token 类型：普通 token 走 fuzzy，引号内 token 走短语匹配。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTokenKind {
    /// 模糊 token（未加引号）。
    Fuzzy,
    /// 短语 token（双引号包围）。
    Phrase,
}

/// 一个搜索 token（保留原大小写；匹配时再小写化）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchToken {
    /// token 类型。
    pub kind: SearchTokenKind,
    /// token 文本（已 `trim`，非空）。
    pub value: String,
}

/// 查询模式（TS `ParsedSearchQuery.mode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedSearchMode {
    /// 空白/引号分词模式。
    Tokens,
    /// `re:` 正则模式。
    Regex,
}

/// 解析后的查询（TS `ParsedSearchQuery`）。
///
/// `regex` 在 `mode == Regex` 且编译成功时才为 `Some`；`error` 非空表示解析失败
/// （`filter_and_sort_sessions` 会直接返回空列表）。
///
/// 不变量：`error` 只可能出现在 Regex 模式；Regex 模式下 `tokens` 恒为空，Tokens 模式下
/// `regex` 恒为 `None`。Tokens 模式的 `tokens` 也可能为空（空查询或只有空引号），此时
/// 所有会话都算命中。
#[derive(Debug, Clone)]
pub struct ParsedSearchQuery {
    /// 模式。
    pub mode: ParsedSearchMode,
    /// token 列表（仅 tokens 模式）。
    pub tokens: Vec<SearchToken>,
    /// 编译后的正则（仅 regex 模式且编译成功）。
    pub regex: Option<Regex>,
    /// 解析失败信息（`"Empty regex"` 或正则编译错误文本）。
    pub error: Option<String>,
}

/// 匹配结果（TS `MatchResult`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchResult {
    /// 是否匹配。
    pub matches: bool,
    /// 越小越好；仅在 `matches == true` 时有意义。
    pub score: f64,
}

// 匹配分层：分数越小越好。每个 token 取它在最佳字段上的层级得分；层级压过 fuzzy 分数。
const TIER_NAME: f64 = 0.0; // 查询文本出现在会话名里
const TIER_TEXT: f64 = 1.0; // 出现在 id / 消息文本 / cwd
const TIER_FUZZY: f64 = 2.0; // 字符散落在会话名或 cwd 里
const TIER_WEIGHT: f64 = 1_000_000.0;

/// `tierScore(tier, score = 0) = tier * TIER_WEIGHT + score`。
fn tier_score(tier: f64, score: f64) -> f64 {
    tier * TIER_WEIGHT + score
}

/// `normalizeWhitespaceLower(text)`：小写化、把 `\s+` 折叠为单个空格、再 trim。
///
/// 用于短语 token 与短语匹配：查询和待匹配字段都先经过同一归一化，空白差异（换行、
/// 连续空格、U+3000 等 JS 空白）不会影响命中。返回新 `String`；空输入或纯空白输入
/// 返回空串，不会留下首尾空格。
pub fn normalize_whitespace_lower(text: &str) -> String {
    let lowered = text.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut pending_space = false;
    for ch in lowered.chars() {
        if is_js_whitespace(ch) {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out
}

/// 一次过滤内的消息文本小写缓存（TS `getMessageText` 的按会话版本）。
///
/// 不变量：一个实例只对应**一个**会话。缓存字段第一次填充后不再校验会话身份，
/// 因此跨会话复用会静默返回上一个会话的文本。现有调用点都在每个会话的循环体里新建
/// 实例（`match_session` 每次调用也新建），不要把它提升到外层循环复用。
struct MatchContext {
    /// 会话 `all_messages_text` 的小写副本；`None` 表示尚未计算。
    lower: Option<String>,
    /// `lower` 经 `normalize_whitespace_lower` 归一化后的结果；`None` 表示尚未计算。
    normalized: Option<String>,
}

impl MatchContext {
    fn new() -> Self {
        MatchContext {
            lower: None,
            normalized: None,
        }
    }

    /// 惰性计算并缓存消息文本的小写副本。
    fn lower(&mut self, session: &SessionInfo) -> &str {
        self.lower
            .get_or_insert_with(|| session.all_messages_text.to_lowercase())
            .as_str()
    }

    /// 惰性计算并缓存消息文本的 JS 空白归一化版本（内部会先复用 `lower`）。
    fn normalized(&mut self, session: &SessionInfo) -> &str {
        if self.normalized.is_none() {
            let normalized = normalize_whitespace_lower(self.lower(session));
            self.normalized = Some(normalized);
        }
        self.normalized.as_deref().unwrap_or_default()
    }
}

/// `hasSessionName(session) = Boolean(session.name?.trim())`（空白名不算有名字）。
///
/// `None`、`Some("")`、以及只含 JS 空白的名字都返回 `false`。判定使用本文件导入的
/// `is_js_whitespace`（JS `trim()` 的字符集），而不是 Rust 的 `char::is_whitespace`。
/// [`NameFilter::Named`] 与调用方 `session_selector` 的预筛都以此为准。
pub fn has_session_name(session: &SessionInfo) -> bool {
    session
        .name
        .as_deref()
        .is_some_and(|name| !name.trim_matches(is_js_whitespace).is_empty())
}

/// `matchesNameFilter(session, filter)`。
fn matches_name_filter(session: &SessionInfo, filter: NameFilter) -> bool {
    match filter {
        NameFilter::All => true,
        NameFilter::Named => has_session_name(session),
    }
}

/// 按 JS 空白切分（`trimmed.split(/\s+/)` 的等价物，去掉空段）。
fn split_js_whitespace(text: &str) -> Vec<&str> {
    text.split(is_js_whitespace)
        .filter(|part| !part.is_empty())
        .collect()
}

/// `parseSearchQuery(query)`。
///
/// `re:` 前缀区分大小写；空 pattern → `error: "Empty regex"`；正则编译失败 →
/// `regex: None` + `error`。未闭合引号回退为普通空白分词。
///
/// 输入先按 JS 空白 `trim`。只有出现在**开头**的 `re:` 才进入正则模式；pattern 两端
/// 的空白也会被 `trim`（内部空白保留）。其余情况按双引号切分：引号本身不进入 token，
/// 引号内的空白不作为分隔符（token flush 时仍会 trim 两端），未加引号的片段按 JS 空白
/// 切分，空片段丢弃。token 文本保留原大小写，匹配阶段才按需小写化。
///
/// `error` 只可能来自正则模式，且此时 `regex` 必为 `None`：
/// * pattern 为空 → `Some("Empty regex")`；
/// * JS 翻译失败（lookaround、反向引用、孤立代理项等）或 `Regex::new` 失败 →
///   `Some(错误文本)`。
///
/// Tokens 模式下 `regex` 与 `error` 恒为 `None`。未闭合引号不算错误：整串按空白重新
/// 切分，引号字符留在 token 里，且所有 token 都是 `Fuzzy`（见
/// `parses_quotes_and_unclosed_quotes`）。
pub fn parse_search_query(query: &str) -> ParsedSearchQuery {
    let trimmed = query.trim_matches(is_js_whitespace);
    if trimmed.is_empty() {
        return ParsedSearchQuery {
            mode: ParsedSearchMode::Tokens,
            tokens: Vec::new(),
            regex: None,
            error: None,
        };
    }

    if let Some(rest) = trimmed.strip_prefix("re:") {
        let pattern = rest.trim_matches(is_js_whitespace);
        if pattern.is_empty() {
            return ParsedSearchQuery {
                mode: ParsedSearchMode::Regex,
                tokens: Vec::new(),
                regex: None,
                error: Some("Empty regex".to_string()),
            };
        }
        // `i` 标志已在翻译期按 JS 的 Canonicalize 语义展开，这里不再开启 Unicode 折叠。
        let compiled = translate_js_regex(pattern)
            .and_then(|translated| Regex::new(&translated).map_err(|error| error.to_string()));
        return match compiled {
            Ok(regex) => ParsedSearchQuery {
                mode: ParsedSearchMode::Regex,
                tokens: Vec::new(),
                regex: Some(regex),
                error: None,
            },
            Err(error) => ParsedSearchQuery {
                mode: ParsedSearchMode::Regex,
                tokens: Vec::new(),
                regex: None,
                error: Some(error),
            },
        };
    }

    // 分词状态：`buf` 累积未提交的字符；`in_quote` 表示当前在双引号内；扫描结束时
    // `in_quote` 仍为 true 就是未闭合引号。
    let mut tokens: Vec<SearchToken> = Vec::new();
    let mut buf = String::new();
    let mut in_quote = false;
    let mut had_unclosed_quote = false;

    fn flush(buf: &mut String, kind: SearchTokenKind, tokens: &mut Vec<SearchToken>) {
        let value = buf.trim_matches(is_js_whitespace).to_string();
        buf.clear();
        if value.is_empty() {
            return;
        }
        tokens.push(SearchToken { kind, value });
    }

    // 逐字符扫描：`"` 切换引号状态并在切换点 flush；引号外的 JS 空白作为分隔符。
    // flush 会 trim 两端并丢弃空片段，因此连续空白和空引号都不会产生空 token。
    for ch in trimmed.chars() {
        if ch == '"' {
            if in_quote {
                flush(&mut buf, SearchTokenKind::Phrase, &mut tokens);
                in_quote = false;
            } else {
                flush(&mut buf, SearchTokenKind::Fuzzy, &mut tokens);
                in_quote = true;
            }
            continue;
        }
        if !in_quote && is_js_whitespace(ch) {
            flush(&mut buf, SearchTokenKind::Fuzzy, &mut tokens);
            continue;
        }
        buf.push(ch);
    }

    if in_quote {
        had_unclosed_quote = true;
    }

    // 未闭合引号：丢弃已解析的 token，按 JS 空白重新切分整串。引号不是空白，
    // 因此 `"foo bar` 会切成 `"foo` 与 `bar` 两个 `Fuzzy` token（引号留在文本里）。
    if had_unclosed_quote {
        return ParsedSearchQuery {
            mode: ParsedSearchMode::Tokens,
            tokens: split_js_whitespace(trimmed)
                .into_iter()
                .map(|value| SearchToken {
                    kind: SearchTokenKind::Fuzzy,
                    value: value.to_string(),
                })
                .collect(),
            regex: None,
            error: None,
        };
    }

    // `inQuote` 此时必为 false（未闭合已提前返回），等价 `flush("fuzzy")`。
    flush(&mut buf, SearchTokenKind::Fuzzy, &mut tokens);

    ParsedSearchQuery {
        mode: ParsedSearchMode::Tokens,
        tokens,
        regex: None,
        error: None,
    }
}

/// 名称字段的小写值（`session.name?.toLowerCase() ?? ""`）。
fn lower_name(session: &SessionInfo) -> String {
    session
        .name
        .as_deref()
        .map(str::to_lowercase)
        .unwrap_or_default()
}

/// `matchLiteralToken(session, value)`：value 由调用方小写。
///
/// 按 name → id → 消息文本 → cwd 的顺序找子串，命中即返回：name 命中记 `TIER_NAME`
/// 并附带 `fuzzy_match` 分数（供同层排序），其余字段命中记 `TIER_TEXT` 且分数为 0。
/// 消息文本经 `MatchContext` 惰性小写化；id 与 cwd 在每次调用时现算小写。
fn match_literal_token(
    session: &SessionInfo,
    value: &str,
    context: &mut MatchContext,
) -> Option<f64> {
    let name = lower_name(session);
    if name.contains(value) {
        return Some(tier_score(TIER_NAME, fuzzy_match(value, &name).score));
    }
    if session.id.to_lowercase().contains(value) {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    if context.lower(session).contains(value) {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    if session.cwd.to_lowercase().contains(value) {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    None
}

/// `matchFuzzyToken(session, value)`。
///
/// 先走 [`match_literal_token`] 的字面量子串路径（覆盖 name/id/消息/cwd）；失败后**只**
/// 对 name 与 cwd 做 `fuzzy_match`，取两者中较小的分数并记 `TIER_FUZZY`。id 与消息文本
/// 不参与模糊匹配——字符必须按顺序散落在 name 或 cwd 里（不必连续）才算这一档命中，
/// 见 `tests/session_selector_search.rs` 的
/// `ignores_the_position_of_a_match_in_message_text` 与
/// `does_not_match_scattered_characters_in_session_ids`。
fn match_fuzzy_token(
    session: &SessionInfo,
    value: &str,
    context: &mut MatchContext,
) -> Option<f64> {
    if let Some(literal) = match_literal_token(session, &value.to_lowercase(), context) {
        return Some(literal);
    }

    let mut best: Option<f64> = None;
    for field in [session.name.as_deref().unwrap_or(""), session.cwd.as_str()] {
        let matched = fuzzy_match(value, field);
        if matched.matches && best.is_none_or(|current| matched.score < current) {
            best = Some(matched.score);
        }
    }
    best.map(|score| tier_score(TIER_FUZZY, score))
}

/// `matchPhraseToken(session, phrase)`：phrase 已 `normalizeWhitespaceLower`。
///
/// name、消息文本、cwd 都要先做同样的空白归一化再判子串包含；id 只做 `to_lowercase`，
/// 不折叠空白。name 命中时用 `fuzzy_match` 给同层排序一个分数，其余字段命中记 0。
fn match_phrase_token(
    session: &SessionInfo,
    phrase: &str,
    context: &mut MatchContext,
) -> Option<f64> {
    let name = normalize_whitespace_lower(session.name.as_deref().unwrap_or(""));
    if name.contains(phrase) {
        return Some(tier_score(TIER_NAME, fuzzy_match(phrase, &name).score));
    }
    if session.id.to_lowercase().contains(phrase) {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    if context.normalized(session).contains(phrase) {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    if normalize_whitespace_lower(&session.cwd).contains(phrase) {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    None
}

/// `matchRegex(session, regex)`：名字命中优先，其次 id / 消息 / cwd。
///
/// `name` 为 `None` 或空串时跳过名字分支；id / 消息 / cwd 任一命中即返回
/// `TIER_TEXT`（三者不再区分先后，分数都是 0）。
fn match_regex(session: &SessionInfo, regex: &Regex) -> Option<f64> {
    if let Some(name) = session.name.as_deref()
        && !name.is_empty()
        && regex.is_match(name)
    {
        return Some(tier_score(TIER_NAME, 0.0));
    }
    if regex.is_match(&session.id)
        || regex.is_match(&session.all_messages_text)
        || regex.is_match(&session.cwd)
    {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    None
}

/// 按 [`ParsedSearchQuery::mode`] 分派并计算分数。
///
/// * 正则模式：`regex` 为 `None`（解析出错）时直接不命中；否则 name 优先单独判断，
///   再依次对 id / 消息 / cwd 调用 `is_match`（`||` 短路）。
/// * tokens 模式：逐个 token 调对应匹配函数并累加分数；任一 token 不命中立即短路返回
///   `matches: false, score: 0.0`。短语 token 归一化后为空时被**跳过**（不参与 AND）。
///
/// 共享的 `MatchContext` 只用于缓存当前会话的消息文本，必须是该会话自己的实例。
fn match_session_with(
    session: &SessionInfo,
    parsed: &ParsedSearchQuery,
    context: &mut MatchContext,
) -> MatchResult {
    if parsed.mode == ParsedSearchMode::Regex {
        let Some(regex) = parsed.regex.as_ref() else {
            return MatchResult {
                matches: false,
                score: 0.0,
            };
        };
        return match match_regex(session, regex) {
            Some(score) => MatchResult {
                matches: true,
                score,
            },
            None => MatchResult {
                matches: false,
                score: 0.0,
            },
        };
    }

    let mut total_score = 0.0;
    for token in &parsed.tokens {
        let score = match token.kind {
            SearchTokenKind::Phrase => {
                let phrase = normalize_whitespace_lower(&token.value);
                if phrase.is_empty() {
                    continue;
                }
                match_phrase_token(session, &phrase, context)
            }
            SearchTokenKind::Fuzzy => match_fuzzy_token(session, &token.value, context),
        };
        let Some(score) = score else {
            return MatchResult {
                matches: false,
                score: 0.0,
            };
        };
        total_score += score;
    }
    MatchResult {
        matches: true,
        score: total_score,
    }
}

/// `matchSession(session, parsed)`。
///
/// token 之间是 AND 语义；每个 token 只在其**单个字段**内匹配（不跨字段拼接）。
///
/// 返回的 `score` 是各 token 分数之和；任一 token 不命中即整体不命中、`score = 0.0`。
/// Tokens 模式下 token 列表为空时（例如查询只有一对空引号）恒为
/// `matches: true, score: 0.0`；正则模式下 `parsed.regex` 为 `None`（解析出错）时恒不命中。
///
/// 每次调用都新建 `MatchContext`，消息文本的小写化不会跨调用复用。
pub fn match_session(session: &SessionInfo, parsed: &ParsedSearchQuery) -> MatchResult {
    let mut context = MatchContext::new();
    match_session_with(session, parsed, &mut context)
}

/// 按 `modified` 降序的比较器（实现为 `b.modified - a.modified`）。
///
/// `modified` 可能为 `NaN`：差值为 `NaN` 或 0 时返回 `Equal`，配合稳定排序保持输入顺序。
fn compare_modified_desc(a: &SessionInfo, b: &SessionInfo) -> std::cmp::Ordering {
    let delta = b.modified - a.modified;
    if delta.is_nan() || delta == 0.0 {
        std::cmp::Ordering::Equal
    } else if delta < 0.0 {
        std::cmp::Ordering::Less
    } else {
        std::cmp::Ordering::Greater
    }
}

/// `filterAndSortSessions(sessions, query, sortMode, nameFilter = "all")`。
///
/// 空查询返回已应用 name filter 的输入序；解析失败返回空；`recent` 只过滤保序；
/// `relevance`（以及带查询的 `threaded`）按分数升序、同分 `modified` 降序。
///
/// 处理顺序：先按 `name_filter` 过滤（[`NameFilter::Named`] 用 [`has_session_name`]），
/// 再把剩下的引用交给 [`filter_and_sort_session_refs`]。返回值借用 `sessions` 的元素，
/// 不克隆 `SessionInfo`。
pub fn filter_and_sort_sessions<'a>(
    sessions: &'a [SessionInfo],
    query: &str,
    sort_mode: SortMode,
    name_filter: NameFilter,
) -> Vec<&'a SessionInfo> {
    let refs: Vec<&SessionInfo> = sessions
        .iter()
        .filter(|session| matches_name_filter(session, name_filter))
        .collect();
    filter_and_sort_session_refs(&refs, query, sort_mode)
}

/// [`filter_and_sort_sessions`] 的引用版本（已是 name-filtered 的会话集合）。
///
/// 返回的引用指向 `sessions` 里的元素，不复制会话。
///
/// 边界行为：
/// * 查询只含 JS 空白 → 原样返回输入顺序，**忽略 `sort_mode`**（调用方的 `Threaded`
///   空查询分支不调用本函数；`Relevance` 空查询也退化为输入序）；
/// * 解析出错（只有正则模式可能）→ 返回空 `Vec`；
/// * `Recent` → 只过滤，保持输入顺序；
/// * `Relevance` 或带非空查询的 `Threaded` → 稳定排序：分数升序 → `modified` 降序 →
///   输入顺序（`compare_modified_desc` 把相等与 NaN 都视为 `Equal`）。
///
/// 每个会话在循环里新建自己的 `MatchContext`，消息文本只在该会话的 token 之间复用。
pub fn filter_and_sort_session_refs<'a>(
    sessions: &[&'a SessionInfo],
    query: &str,
    sort_mode: SortMode,
) -> Vec<&'a SessionInfo> {
    let trimmed = query.trim_matches(is_js_whitespace);
    if trimmed.is_empty() {
        return sessions.to_vec();
    }

    let parsed = parse_search_query(query);
    if parsed.error.is_some() {
        return Vec::new();
    }

    let mut matched: Vec<(&SessionInfo, f64)> = Vec::new();
    for session in sessions {
        let mut context = MatchContext::new();
        let result = match_session_with(session, &parsed, &mut context);
        if result.matches {
            matched.push((session, result.score));
        }
    }

    // recent 只过滤，保持输入顺序。
    if sort_mode == SortMode::Recent {
        return matched.into_iter().map(|(session, _)| session).collect();
    }

    // stable sort：分数相同再按 modified 降序；modified 也相同时保持输入序。
    let mut scored = matched;
    scored.sort_by(|(session_a, score_a), (session_b, score_b)| {
        if score_a != score_b {
            score_a
                .partial_cmp(score_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        } else {
            compare_modified_desc(session_a, session_b)
        }
    });
    scored.into_iter().map(|(session, _)| session).collect()
}

// 测试覆盖：引号分词与未闭合引号回退、`re:` 模式与空 pattern 错误、短语的空白归一化、
// `Recent` 保序、空查询配合 `NameFilter::Named`。fixture 是下面 `session()` 构造的
// 最小 `SessionInfo`（cwd 为空、name 为 None，个别用例再补字段）。
#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, modified: f64, text: &str) -> SessionInfo {
        SessionInfo {
            path: format!("/tmp/{id}.jsonl"),
            id: id.to_string(),
            cwd: String::new(),
            name: None,
            parent_session_path: None,
            created: 0.0,
            modified,
            message_count: 1,
            first_message: "(no messages)".to_string(),
            all_messages_text: text.to_string(),
        }
    }

    #[test]
    fn parses_quotes_and_unclosed_quotes() {
        let parsed = parse_search_query("foo \"node cve\" bar");
        assert_eq!(parsed.tokens.len(), 3);
        assert_eq!(parsed.tokens[0].kind, SearchTokenKind::Fuzzy);
        assert_eq!(parsed.tokens[1].kind, SearchTokenKind::Phrase);
        assert_eq!(parsed.tokens[1].value, "node cve");

        let unclosed = parse_search_query("\"foo bar");
        assert!(
            unclosed
                .tokens
                .iter()
                .all(|t| t.kind == SearchTokenKind::Fuzzy)
        );
        assert_eq!(unclosed.tokens.len(), 2);
    }

    #[test]
    fn parses_regex_mode() {
        let parsed = parse_search_query("re:brave");
        assert_eq!(parsed.mode, ParsedSearchMode::Regex);
        assert!(parsed.regex.is_some());
        assert!(parsed.error.is_none());
        let empty = parse_search_query("re:   ");
        assert_eq!(empty.error.as_deref(), Some("Empty regex"));
    }

    #[test]
    fn quoted_phrase_normalizes_whitespace() {
        let sessions = [
            session("a", 0.0, "node\n\n   cve was discussed"),
            session("b", 1.0, "node something else"),
        ];
        let parsed = parse_search_query("\"node cve\"");
        assert!(match_session(&sessions[0], &parsed).matches);
        assert!(!match_session(&sessions[1], &parsed).matches);
    }

    #[test]
    fn recent_filter_preserves_order() {
        let sessions = [
            session("newer", 3.0, "brave"),
            session("older", 1.0, "brave"),
        ];
        let result =
            filter_and_sort_sessions(&sessions, "\"brave\"", SortMode::Recent, NameFilter::All);
        assert_eq!(
            result.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["newer", "older"]
        );
    }

    #[test]
    fn empty_query_returns_name_filtered_input_order() {
        let mut named = session("named", 2.0, "x");
        named.name = Some("Name".to_string());
        let sessions = [session("plain", 3.0, "x"), named];
        let result = filter_and_sort_sessions(&sessions, "", SortMode::Recent, NameFilter::Named);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "named");
    }
}
