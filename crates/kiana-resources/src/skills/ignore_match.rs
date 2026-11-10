//! npm `ignore` 7.0.8 语义的私有复刻（仅 `add(pattern)` + `ignores(path)`）。
//!
//! `skills.ts` 的 ignore 规则由本模块处理。Rust `ignore` crate 的 glob 解析与
//! npm `ignore` 包不同（brace 展开、单独的 `/` 行、POSIX 字符类、无效字符类等），因此这里
//! 按 `node_modules/ignore/index.js`（7.0.8）逐段移植其 pattern → RegExp 的字符串改写链，
//! 再把生成的正则交给 `regex` crate 匹配。
//!
//! 移植范围：
//! - `checkPattern` / `createRule`（`!` 否定、`\!` / `\#` 转义）；
//! - `extractBrackets` + `scanBracket`（git wildmatch 的括号表达式与 POSIX 类）；
//! - `REPLACERS` 改写链（BOM、行尾、尾随空格、转义、`?`、`*`、`**`、锚点、结尾）；
//! - `RuleManager.test` / `Ignore._t` / `ignores`（父目录递归、目录剪枝、否定规则的 skip 逻辑）；
//! - 默认大小写不敏感（npm `ignore()` 的 `ignorecase: true`）。
//!
//! 已知边界：JS 正则按 UTF-16 code unit 工作，Rust `regex` 按 Unicode scalar 工作。
//! 为保证 `?` / `*` / 字符类的计数与 npm `ignore` 一致，这里在匹配前把每个 UTF-16
//! code unit 映射成一个 Rust 字符（BMP 原样，代理项映射到 U+1D800..U+1DFFF 区间，
//! 见 [`encode_js_units`]），从而让 Rust 正则按 code unit 计数。
//!
//! # 为什么需要它
//! 上游 `skills.ts` / `package-manager.ts` 直接调用 npm `ignore()` 决定目录能否剪枝。
//! 若换用 `ignore` 或 `globset` crate，brace、POSIX 字符类、单独的 `/` 行等语义都与
//! npm 实现不同，扫描结果会偏离 TS 版本。因此这里只移植“添加规则 + 查询路径”这一对
//! 操作，不实现 `createFilter` / `test` / `checkIgnore` / `filter` 等 npm 包的其它能力
//! （`.git` 仓库根的判断属于上游 TS 调用方的 `findGitRepoRoot`，不在本模块或 npm 包内）。
//!
//! # 被谁调用
//! - `crate::skills::discover`：递归扫描技能目录时累积 `.gitignore` / `.ignore` / `.fdignore`；
//! - `crate::package::discover`：用 `IgnoreMatcher` 包装本模块，做包资源的目录剪枝。
//! 两者都会先把模式改写成相对 root 的形式，再调用 [`NpmIgnore::ignores`]；目录路径必须
//! 带尾随 `/`，这是目录规则能否命中的前提。
//!
//! # 执行流程
//! 1. [`NpmIgnore::add`] 逐行调用 `check_pattern`，过滤空行、注释行和非法尾随反斜杠；
//! 2. `IgnoreRule::create` 解析 `!` 否定与 `\!` / `\#` 字面量转义，并判断是否只匹配 basename；
//! 3. `compile_pattern`：UTF-16 code unit 编码 -> `make_regex_prefix` 改写链 ->
//!    `finish_trailing_wildcard` 收尾 -> `to_regex_source` 翻译成 `regex` crate 语法 ->
//!    以大小写不敏感编译；
//! 4. [`NpmIgnore::ignores`] -> `test`：先递归检查父目录（父目录已被忽略就直接返回），
//!    再按添加顺序用 `test_rules` 求出本条路径的结果。
//!
//! # 关键约束
//! - 规则顺序有语义：后面的规则可以覆盖前面的，`add` 不得重排或去重。
//! - 匹配单位是 UTF-16 code unit（见 [`encode_js_units`]），不是 Rust 的 Unicode scalar。
//! - `NpmIgnore` 不做内部同步：查询只读（`&self`），添加规则需要 `&mut self`，
//!   共享方式由调用方决定。
//! - 单条模式编译失败不会 panic，而是退化为永不匹配（`NEVER_MATCH`），效果是“该规则不生效”。
//!
//! # 与 JS 的差异与失败模式
//! - JS 正则支持 lookaround，`regex` crate 不支持；因此 JS 中依赖 lookahead 的改写步骤，
//!   这里都改用等价的无 lookaround 正则或手写扫描：分别见 `extract_brackets`
//!   （转义字符进入占位符，步骤 6 的 `(?!\\)\?` 不再需要）、`has_inner_slash`
//!   （步骤 10 的锚点选择）、`rewrite_globstars`、`rewrite_intermediate_wildcards`
//!   （步骤 11/12 的 lookahead）、`unescape_special`（步骤 14）；结尾断言
//!   `(?=$|\/$)` 本身保留，由 `to_regex_source` 翻译成 `regex` crate 可用的形式。
//! - 未闭合或无法解析的 `[...]` 会被替换为永不匹配，并丢弃该括号之后的模式文本，
//!   这一行为由 `extract_brackets` 决定，不是错误处理路径。
//! - JS 在正则非法时抛异常；这里返回永不匹配，调用方看不到错误。

use regex::{Captures, Regex};
use std::borrow::Cow;
use std::sync::LazyLock;

/// npm `ignore` 的 `PLACEHOLDER`（模式中的 NUL 与内部占位符）。
const PLACEHOLDER: char = '\u{0000}';

/// npm `ignore` 的 `TRAILING_WILDCARD`（尾随 `*` 的中间标记）。
const TRAILING_WILDCARD: char = '\u{E000}';

/// 永远不匹配的 Rust 正则片段（JS 侧的 `[]`）。
///
/// Rust 正则不允许空字符类，这里对“空白 + 非空白”的并集取补集：
/// `[^\s\S]` 不含任何字符，因此永远不命中。
const NEVER_MATCH: &str = r"[^\s\S]";

/// 正则元字符（npm `REGEX_LITERAL_SPECIAL`）。
fn is_regex_literal_special(c: char) -> bool {
    matches!(
        c,
        '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' | '/'
    )
}

/// 未转义 `?` 的替换结果（最终转义链会把 `/` 变成 `\/`）。
const QUESTION_MARK_REPLACEMENT: &str = "[^/]";

/// 中间 `*` 的替换结果。
const STAR_REPLACEMENT: &str = "[^\\/]*";

// 以下正则逐条对应 npm `ignore` 的 REPLACERS 项（步骤号见 `make_regex_prefix` 的注释）。
// JS 侧凡是带 `(?=...)` 的步骤都无法直接翻译（`regex` crate 不支持 lookaround），
// 对应逻辑改由下方手写扫描函数完成。
static RE_TRAILING_SPACES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"((?:\\\\)*?)(\\? +)$").unwrap());
static RE_BACKSLASH_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\\+?) ").unwrap());
static RE_ESCAPE_META: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\\$.|*+(){^]").unwrap());
static RE_TRAILING_WILDCARD_MATCH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|[^\\])((?:\\\\)*)\\\*$").unwrap());
static RE_UNESCAPE_BACKSLASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\\\").unwrap());
static RE_ESCAPED_BRACKET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\\[([^\]/]*?)(\\*)($|\])").unwrap());
static RE_RESTORE_PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("\u{0000}(\\d+)\u{0000}").unwrap());

/// POSIX 类的码点区间（git `wildmatch.c` 的 ASCII 类别）。
///
/// 返回 `None` 表示类名不受支持；调用方 `scan_bracket` 会因此把整个括号表达式判为
/// 无法解析（最终变成永不匹配）。这些区间只覆盖 ASCII，不包含 Unicode 类别。
fn posix_class_ranges(name: &str) -> Option<Vec<(u32, u32)>> {
    let ranges = match name {
        "alnum" => vec![(0x30, 0x39), (0x41, 0x5A), (0x61, 0x7A)],
        "alpha" => vec![(0x41, 0x5A), (0x61, 0x7A)],
        "blank" => vec![(0x09, 0x09), (0x20, 0x20)],
        "cntrl" => vec![(0x00, 0x1F), (0x7F, 0x7F)],
        "digit" => vec![(0x30, 0x39)],
        "graph" => vec![(0x21, 0x7E)],
        "lower" => vec![(0x61, 0x7A)],
        "print" => vec![(0x20, 0x7E)],
        "punct" => vec![(0x21, 0x2F), (0x3A, 0x40), (0x5B, 0x60), (0x7B, 0x7E)],
        "space" => vec![(0x09, 0x0A), (0x0D, 0x0D), (0x20, 0x20)],
        "upper" => vec![(0x41, 0x5A)],
        "xdigit" => vec![(0x30, 0x39), (0x41, 0x46), (0x61, 0x66)],
        _ => return None,
    };
    Some(ranges)
}

// ---------------------------------------------------------------------------
// 括号表达式（npm `scanBracket` / `classSource`）
// ---------------------------------------------------------------------------

/// `scan_bracket` 的解析结果。
struct ScannedBracket {
    /// `]` 的下标（chars）。
    end: usize,
    /// 已展开的码点闭区间；POSIX 类会展开成多个区间。
    ranges: Vec<(u32, u32)>,
    /// 首个成员字符是 `!` 或 `^` 时为真，由 `class_source` 取补集。
    negated: bool,
}

/// 把区间拆成不含代理项（U+D800..U+DFFF）的 UTF-8 可表示区间。
///
/// 区间端点本身是合法 `char`，不会落在代理项；但跨越 U+D7FF..U+E000 的区间会把代理项
/// 包含进来，而 Rust 正则的 `\x{...}` 不能表示代理项码点，所以这里按代理项空洞切开并丢弃。
fn scalar_ranges(ranges: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for &(lo, hi) in ranges {
        if lo > 0x10FFFF {
            continue;
        }
        let hi = hi.min(0x10FFFF);
        let lo_surrogate = lo.max(0xD800);
        let hi_surrogate = hi.min(0xDFFF);
        if lo_surrogate <= hi_surrogate {
            if lo < 0xD800 {
                out.push((lo, 0xD7FF));
            }
            if hi > 0xDFFF {
                out.push((0xE000, hi));
            }
        } else {
            out.push((lo, hi));
        }
    }
    out
}

/// 往字符类体内追加一个码点或闭区间。
///
/// 统一用 `\x{..}` 十六进制转义，避免端点字符正好是 `]`、`-`、`\` 时破坏类语法；
/// `lo > hi` 视为非法区间，静默跳过（与 `scan_bracket` 中“只有 `from <= to` 才入表”一致）。
fn push_range(out: &mut String, lo: u32, hi: u32) {
    if lo > hi {
        return;
    }
    if lo == hi {
        out.push_str(&format!("\\x{{{lo:x}}}"));
    } else {
        out.push_str(&format!("\\x{{{lo:x}}}-\\x{{{hi:x}}}"));
    }
}

/// 构造 Rust 正则的字符类。
///
/// npm `classSource` 对能匹配 `/` 的正类加 `(?!/)` 守卫；无论哪种情况，最终类都等价于
/// “成员集合去掉 `/`”。负类则等价于“全集中去掉成员集合与 `/`”。
///
/// `regex` crate 不支持 `(?!/)` 这类 lookaround，所以这里不照搬守卫，而是直接在成员
/// 区间上减掉 `/`（正类）或从补集里排除 `/`（负类）。正类被减空时返回 `NEVER_MATCH`。
fn class_source(negated: bool, ranges: &[(u32, u32)]) -> String {
    let scalar = scalar_ranges(ranges);
    let mut out = String::new();
    if negated {
        out.push_str("[^\\x{2f}");
        for (lo, hi) in scalar {
            push_range(&mut out, lo, hi);
        }
        out.push(']');
        return out;
    }

    // 正类：去掉 `/`。
    let mut body = String::new();
    for (lo, hi) in scalar {
        let mut pieces: Vec<(u32, u32)> = Vec::new();
        if lo <= 0x2F && 0x2F <= hi {
            if lo < 0x2F {
                pieces.push((lo, 0x2E));
            }
            if hi > 0x2F {
                pieces.push((0x30, hi));
            }
        } else {
            pieces.push((lo, hi));
        }
        for (piece_lo, piece_hi) in pieces {
            push_range(&mut body, piece_lo, piece_hi);
        }
    }
    if body.is_empty() {
        return NEVER_MATCH.to_string();
    }
    format!("[{body}]")
}

/// npm `scanBracket`。
///
/// 输入是 pattern 的 `char` 序列与 `[` 的下标；成功时返回闭合 `]` 的下标和展开后的
/// 码点区间，失败时返回 `None`。失败包括提前越界（模式在 `]` 之前结束）和 POSIX 类名
/// 未知；调用方会把它当作“永不匹配”，不会向上报错。
///
/// 解析规则：首个成员为 `!` / `^` 表示取反；`\x` 按字面量收录；`a-z` 只在 `-` 前有成员、
/// 后面还有字符且不是 `]` 时展开成区间（`from > to` 的区间被丢弃）；`[:name:]` 查
/// `posix_class_ranges` 展开。
fn scan_bracket(pattern: &[char], start: usize) -> Option<ScannedBracket> {
    let length = pattern.len();
    let mut index = start + 1;
    let mut negated = false;

    let lead = *pattern.get(index)?;
    if lead == '!' || lead == '^' {
        negated = true;
        index += 1;
    }

    let mut ranges: Vec<(u32, u32)> = Vec::new();
    let mut prev: Option<char> = None;

    loop {
        let ch = *pattern.get(index)?;

        if ch == '\\' {
            let escaped = *pattern.get(index + 1)?;
            ranges.push((escaped as u32, escaped as u32));
            prev = Some(escaped);
            index += 1;
        } else if ch == '-' && prev.is_some() && index + 1 < length && pattern[index + 1] != ']' {
            index += 1;
            let mut to = pattern[index];
            if to == '\\' {
                index += 1;
                to = *pattern.get(index)?;
            }
            let from = prev.expect("checked");
            if from <= to {
                ranges.push((from as u32, to as u32));
            }
            prev = None;
        } else if ch == '[' && pattern.get(index + 1) == Some(&':') {
            let name_start = index + 2;
            let mut end = name_start;
            while end < length && pattern[end] != ']' {
                end += 1;
            }
            if end == length {
                return None;
            }
            if end > name_start && pattern[end - 1] == ':' {
                let name: String = pattern[name_start..end - 1].iter().collect();
                let expanded = posix_class_ranges(&name)?;
                ranges.extend(expanded);
                prev = None;
                index = end;
            } else {
                ranges.push(('[' as u32, '[' as u32));
                prev = Some('[');
                index = name_start - 2;
            }
        } else {
            ranges.push((ch as u32, ch as u32));
            prev = Some(ch);
        }

        index += 1;
        if pattern.get(index) == Some(&']') {
            return Some(ScannedBracket {
                end: index,
                ranges,
                negated,
            });
        }
    }
}

/// npm `extractBrackets`：替换括号表达式为占位符，返回改写后的串与类源码表。
///
/// 之所以要“先抽出再还原”，是因为后续改写链会对元字符做统一转义；括号表达式里已经
/// 由 `class_source` 生成的 `\x{..}` 不能被再次转义。这里用 `\0<n>\0` 占位符把源码
/// 暂存到 `sources`，等整条链走完再按编号填回。
///
/// 三类内容会被抽出/特殊处理：
/// 1. 转义的元字符与 NUL 本身，记录其正则源码；
/// 2. 完整的 `[...]`，用 `scan_bracket` 解析；解析失败则记为永不匹配，并**丢弃该括号
///    之后的全部模式文本**（`index = chars.len()`）；
/// 3. 转义的 `*`、`[`、空格、`\` 保持原样，留给后续步骤处理。
fn extract_brackets(pattern: &str) -> (String, Vec<String>) {
    let chars: Vec<char> = pattern.chars().collect();
    let mut sources: Vec<String> = Vec::new();
    let mut out = String::new();
    let mut index = 0;

    let hold = |sources: &mut Vec<String>, source: String| -> String {
        sources.push(source);
        format!("{PLACEHOLDER}{}{PLACEHOLDER}", sources.len() - 1)
    };

    while index < chars.len() {
        let ch = chars[index];

        if ch == '\\' {
            let Some(&escaped) = chars.get(index + 1) else {
                // `checkPattern` 已丢弃孤立尾随反斜杠，这里按字面量兜底。
                out.push('\\');
                index += 1;
                continue;
            };
            if escaped == '*' || escaped == '[' || escaped == ' ' || escaped == '\\' {
                out.push('\\');
                out.push(escaped);
            } else {
                let source = if is_regex_literal_special(escaped) {
                    format!("\\{escaped}")
                } else {
                    escaped.to_string()
                };
                let placeholder = hold(&mut sources, source);
                out.push_str(&placeholder);
            }
            index += 2;
        } else if ch == PLACEHOLDER {
            let placeholder = hold(&mut sources, format!("[\\x{{{:x}}}]", ch as u32));
            out.push_str(&placeholder);
            index += 1;
        } else if ch == '[' {
            match scan_bracket(&chars, index) {
                None => {
                    let placeholder = hold(&mut sources, NEVER_MATCH.to_string());
                    out.push_str(&placeholder);
                    index = chars.len();
                }
                Some(scanned) => {
                    let source = class_source(scanned.negated, &scanned.ranges);
                    let placeholder = hold(&mut sources, source);
                    out.push_str(&placeholder);
                    index = scanned.end + 1;
                }
            }
        } else {
            out.push(ch);
            index += 1;
        }
    }

    (out, sources)
}

// ---------------------------------------------------------------------------
// 改写链（npm `makeRegexPrefix` + `_make`）
// ---------------------------------------------------------------------------

/// 预处理后的 pattern 是否含“内部斜杠”（决定是否锚定到路径开头）。
///
/// JS: `/\/(?!$)/.test(pattern)`。这里按字节扫描：`/` 是 ASCII，UTF-8 多字节字符的
/// 后续字节不会等于 `0x2F`，因此不会误判。
fn has_inner_slash(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    bytes
        .iter()
        .enumerate()
        .any(|(index, byte)| *byte == b'/' && index + 1 != bytes.len())
}

/// npm `checkPattern`。
///
/// 返回 `false` 的模式会被丢弃：空串、以 `#` 开头的注释行、全空格行，以及结尾恰好是
/// 一个孤立反斜杠的模式（JS 正则 `/(?:[^\\]|^)\\$/` 的等价判断）。
/// 注意判定的是**原始**模式（含 `!` / `\#` 前缀），不加前缀剥离。
pub(crate) fn check_pattern(pattern: &str) -> bool {
    if pattern.is_empty() || pattern.starts_with('#') {
        return false;
    }
    if pattern.bytes().all(|byte| byte == b' ') {
        return false;
    }
    // JS: /(?:[^\\]|^)\\$/ —— 结尾恰有一个反斜杠（其前不是反斜杠）。
    let bytes = pattern.as_bytes();
    if bytes.last() == Some(&b'\\') {
        let before = bytes.len().checked_sub(2).map(|index| bytes[index]);
        if bytes.len() == 1 || before != Some(b'\\') {
            return false;
        }
    }
    true
}

/// 应用 `REPLACERS` 中不使用 lookaround 的步骤。
fn replace_regex(src: &str, regex: &Regex, replacer: impl Fn(&Captures<'_>) -> String) -> String {
    regex
        .replace_all(src, |captures: &Captures<'_>| replacer(captures))
        .into_owned()
}

/// 去掉 BOM、行尾与尾随空格，处理内部转义空格，然后转义正则元字符。
///
/// 对应 REPLACERS 的第 1–5 步。这些步骤都能用 `regex` crate 直接表达；后续中无法用
/// 无 lookaround 正则表达的步骤，在 `make_regex_prefix` 里用手写扫描完成。
fn preprocess_source(source: &str) -> String {
    // 1. Remove BOM
    let source = source.strip_prefix('\u{FEFF}').unwrap_or(source);

    // 2. 行尾（skills.ts 已按行切分，这里兜底 `add(string)` 的语义）。
    let source = source.trim_end_matches(['\r', '\n']);

    // 3. Trailing spaces（反斜杠转义的空格保留一个）。
    let source = replace_regex(source, &RE_TRAILING_SPACES, |captures| {
        let m1 = captures.get(1).map_or("", |m| m.as_str());
        let m2 = captures.get(2).map_or("", |m| m.as_str());
        if m2.starts_with('\\') {
            format!("{m1} ")
        } else {
            m1.to_string()
        }
    });

    // 4. (\ ) -> ' '，保留偶数个反斜杠。
    //    `m1` 是空格前的反斜杠串：`\\` 表示一个字面反斜杠，成对出现有效，因此只截取
    //    偶数长度；末尾统一补一个空格（即去掉转义空格的反斜杠）。
    let source = replace_regex(&source, &RE_BACKSLASH_SPACE, |captures| {
        let m1 = captures.get(1).map_or("", |m| m.as_str());
        let length = m1.len();
        format!("{} ", &m1[..length - length % 2])
    });

    // 5. Escape metacharacters.
    replace_regex(&source, &RE_ESCAPE_META, |captures| {
        format!("\\{}", captures.get(0).map_or("", |m| m.as_str()))
    })
}

/// npm `makeRegexPrefix` 的改写链（返回占位符已还原、但尚未做尾随 `*` 替换的源）。
///
/// 输入 `pattern` 已经过 [`encode_js_units`] 编码；输出仍是“JS 风格”的正则源串：
/// 需要收尾的模式已带结尾断言 `(?=$|\/$)`，尾随 `*` 则保留为 `TRAILING_WILDCARD`
/// 标记。后续由 `finish_trailing_wildcard` 和 `to_regex_source` 转成 `regex` crate
/// 可用的语法。
///
/// 步骤顺序与 npm 一致，不能调换。注意步骤 10 判断“内部斜杠”时用的是原始 `pattern`
/// 而不是已经转义的 `source`，所以两者都必须保留。
fn make_regex_prefix(pattern: &str) -> String {
    let (extracted, sources) = extract_brackets(pattern);

    // 5 之后的所有步骤。
    let mut source = preprocess_source(&extracted);

    // 6. '?' -> '[^/]'（转义过的 '?' 已被 extractBrackets 移走）。
    source = source.replace('?', QUESTION_MARK_REPLACEMENT);

    // 7. 前导 '/' -> '^'。
    if let Some(rest) = source.strip_prefix('/') {
        source = format!("^{rest}");
    }

    // 8. '/' -> '\/'。
    source = source.replace('/', "\\/");

    // 9. 前导 '**/'（可重复）-> '^(?:.*\/)?'。
    source = rewrite_leading_globstars(&source);

    // 10. 锚点。
    if !source.is_empty() && !source.starts_with('^') {
        let anchor = if has_inner_slash(pattern) {
            "^"
        } else {
            "(?:^|\\/)"
        };
        source = format!("{anchor}{source}");
    }

    // 11. 中间/结尾的 '/**'。
    source = rewrite_globstars(&source);

    // 12. 中间通配符。
    source = rewrite_intermediate_wildcards(&source);

    // 13. 尾随通配符。
    source = replace_regex(&source, &RE_TRAILING_WILDCARD_MATCH, |captures| {
        let whole = captures.get(0).map_or("", |m| m.as_str());
        let p1 = captures.get(1).map_or("", |m| m.as_str());
        let p2 = captures.get(2).map_or("", |m| m.as_str());
        if (p2.len() / 2) % 2 == 0 {
            format!("{p1}{p2}{TRAILING_WILDCARD}")
        } else {
            whole.to_string()
        }
    });

    // 14. 反转义（保留反斜杠）。
    source = unescape_special(&source);

    // 15. '\\\\' -> '\'。
    source = replace_regex(&source, &RE_UNESCAPE_BACKSLASH, |_| "\\".to_string());

    // 16. 转义的字面 '[' 还原。
    source = replace_regex(&source, &RE_ESCAPED_BRACKET, |captures| {
        let range = captures.get(1).map_or("", |m| m.as_str());
        let end_escape = captures.get(2).map_or("", |m| m.as_str());
        let close = captures.get(3).map_or("", |m| m.as_str());
        // cleanRangeBackSlash：保留偶数个反斜杠。
        let kept = &end_escape[..end_escape.len() - end_escape.len() % 2];
        format!("\\[{range}{kept}{close}")
    });

    // 17. 结尾。
    match source.chars().last() {
        None => {}
        Some(TRAILING_WILDCARD) => {}
        Some('/') => source.push('$'),
        Some(_) => source.push_str("(?=$|\\/$)"),
    }

    // 还原括号表达式。
    if !sources.is_empty() {
        source = RE_RESTORE_PLACEHOLDER
            .replace_all(&source, |captures: &Captures<'_>| {
                let index: usize = captures
                    .get(1)
                    .map_or("", |m| m.as_str())
                    .parse()
                    .unwrap_or(usize::MAX);
                sources
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| NEVER_MATCH.to_string())
            })
            .into_owned();
    }

    source
}

/// 步骤 9：`/^\^*(?:\\\*\\\*\\\/)+/`。
///
/// 先跳过前导的 `^`（步骤 7 可能把前导 `/` 变成 `^`），再把连续的开头 `**/` 组整体
/// 替换成 `^(?:.*\/)?`；一组都没有时原样返回。
fn rewrite_leading_globstars(source: &str) -> String {
    let mut index = 0;
    let bytes = source.as_bytes();
    while index < bytes.len() && bytes[index] == b'^' {
        index += 1;
    }
    let mut groups = 0;
    while source[index..].starts_with("\\*\\*\\/") {
        index += 6;
        groups += 1;
    }
    if groups == 0 {
        return source.to_string();
    }
    format!("^(?:.*\\/)?{}", &source[index..])
}

/// 步骤 14：`/\\\\\\(?=[$.|*+(){^])/g`（两个反斜杠 + 元字符 -> 一个反斜杠）。
fn unescape_special(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && bytes.get(index + 1) == Some(&b'\\')
            && bytes.get(index + 2).is_some_and(|byte| {
                matches!(
                    byte,
                    b'$' | b'.' | b'|' | b'*' | b'+' | b'(' | b')' | b'{' | b'^'
                )
            })
        {
            out.push('\\');
            index += 2;
            continue;
        }
        let ch = source[index..].chars().next().expect("bounds");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

/// 步骤 11：`/\\\/\\\*\\\*(?=\\\/|$)/g`。
///
/// 只在 `/**` 后面紧跟 `\/` 或已到末尾时改写，所以 `a/**b` 中的 `**` 不会被当成分隔符。
/// 三种收尾：`/**` 在末尾 -> `\/.+`（至少一段）；`/**/` 正好收尾 -> 一段或多段；
/// `/**/rest` -> 零段或多段（允许 `a/**/b` 匹配 `a/b`）。
fn rewrite_globstars(source: &str) -> String {
    const MATCH: &str = "\\/\\*\\*";
    let mut out = String::with_capacity(source.len());
    let mut index = 0;
    while index < source.len() {
        if source[index..].starts_with(MATCH) {
            let after = index + MATCH.len();
            let rest = &source[after..];
            if rest.is_empty() {
                out.push_str("\\/.+");
                index = after;
                continue;
            }
            if rest.starts_with("\\/") {
                if rest == "\\/" {
                    out.push_str("(?:\\/[^\\/]+)+");
                } else {
                    out.push_str("(?:\\/[^\\/]+)*");
                }
                index = after;
                continue;
            }
        }
        let ch = source[index..].chars().next().expect("bounds");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

/// 步骤 12：`/(^|[^\\]+)(\\\*)+(?=.+)/g`。
///
/// 把“不在结尾”的连续 `\*` 改写成 `[^\/]*`；末尾的最后一个 `\*` 留给尾随通配符步骤。
/// 前缀条件是“行首或前一个字符不是反斜杠”，因此紧跟在反斜杠之后的 `\*`
/// （转义序列的一部分）不会被替换。
fn rewrite_intermediate_wildcards(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut index = 0;
    while index < source.len() {
        let at_start_or_after_non_backslash = if index == 0 {
            true
        } else {
            let previous = source[..index].chars().next_back().expect("non-empty");
            previous != '\\'
        };
        if at_start_or_after_non_backslash && source[index..].starts_with("\\*") {
            let mut end = index;
            while source[end..].starts_with("\\*") {
                end += 2;
            }
            // `(\\*)+(?=.+)`：`+` 贪婪匹配整段，但可回退到只留一个字符在匹配之后。
            // 因此末尾的连续 N 个 `\*` 只替换前 N-1 个（N=1 时不替换），最后一个交给
            // 尾随通配符步骤。
            let replace_end = if end < source.len() { end } else { end - 2 };
            if replace_end > index {
                for _ in 0..(replace_end - index) / 2 {
                    out.push_str(STAR_REPLACEMENT);
                }
                index = replace_end;
                continue;
            }
        }
        let ch = source[index..].chars().next().expect("bounds");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

/// `_make`：尾随通配符标记 -> 具体量词，并追加结尾断言。
///
/// 步骤 13 先把真正的尾随 `*` 标成 `TRAILING_WILDCARD` 而不是立刻展开，因为要到最后
/// 才能判断它前面的上下文：
/// - 前面是 `\/`：这是目录内的 `*`，展开成 `\/[^/]+`（至少一个路径分量）；
/// - 否则是普通尾随星号，展开成 `[^/]*`。
/// 两种都追加 npm 的结尾断言 `(?=$|\/$)`，随后由 `to_regex_source` 翻译。
fn finish_trailing_wildcard(source: &str) -> String {
    let Some(prefix) = source.strip_suffix(TRAILING_WILDCARD) else {
        return source.to_string();
    };
    // JS 的 `(^|\\/)?` 中第一个分支是行首锚点（p1 为空串，按假值处理），只有
    // `\/` 分支会得到 `[^/]+`。
    let head = if let Some(head) = prefix.strip_suffix("\\/") {
        format!("{head}\\/[^/]+")
    } else {
        format!("{prefix}[^/]*")
    };
    format!("{head}(?=$|\\/$)")
}

/// 把 npm 生成的正则源码翻译成 `regex` crate 语法。
fn to_regex_source(source: &str) -> String {
    // 结尾断言 `(?=$|\/$)` -> `(?:$|/$)`：断言要求匹配位置之后是字符串结尾，
    // 或者 `/$`（一个斜杠且紧接字符串结尾）。翻译时必须把这两个字符都消费掉
    // （`(?:$|/)` 只消费斜杠，会让 `a` 之类的模式额外匹配 `a/b/c` 这类后代路径，
    // 因为 `Regex::is_match` 不锚定，前缀匹配就为真）。这与 node-ignore 的
    // `RegExp.test` 语义一致：`a(?=$|\/$)` 对 `a/b/c` 为假。
    let source = source.replace("(?=$|\\/$)", "(?:$|/$)");
    // `\/`（含字符类内部）就是字面量 '/'。
    source.replace("\\/", "/")
}

/// 把 JS 字符串的 UTF-16 code unit 序列映射成 Rust 字符串。
///
/// npm `ignore` 7.0.8 生成的是**不带 `u` 标志**的 JS 正则，匹配单位是 UTF-16
/// code unit（`?` 只消费一个 code unit、`[!a]` 也只消费一个）。Rust `regex` 按
/// Unicode scalar 匹配，会把一个 astral 字符当成一个单位。这里做一个双射：
/// - BMP code unit 原样映射（保留大小写属性，`(?i)` 才能工作）；
/// - 代理 code unit `u` 映射到 `U+10000 + u`（U+1D800..U+1DFFF，与任何 code unit
///   的映射结果都不重叠）。
///
/// pattern 与 path 用同一映射，正则里的字面量、字符类、量词就都按 code unit 计数。
fn encode_js_units(value: &str) -> Cow<'_, str> {
    if value.is_ascii() {
        // ASCII 的 code unit 与字节一一对应，无需重编码，直接借用避免分配。
        return Cow::Borrowed(value);
    }
    let mut encoded = String::with_capacity(value.len());
    for unit in value.encode_utf16() {
        let code_point = if (0xD800..=0xDFFF).contains(&unit) {
            0x10000 + u32::from(unit)
        } else {
            u32::from(unit)
        };
        encoded.push(char::from_u32(code_point).expect("UTF-16 code unit maps to a scalar"));
    }
    Cow::Owned(encoded)
}

/// 把一条 pattern 编译成最终正则。编译失败时退化为永不匹配（JS 会抛错，但该路径
/// 只可能来自极其畸形的模式；skills.ts 的 try/catch 会中止整个目录扫描）。
///
/// 流程固定为：UTF-16 编码 -> `make_regex_prefix` -> `finish_trailing_wildcard` ->
/// `to_regex_source`。`case_insensitive(true)` 对应 npm `ignore()` 的默认 `ignorecase`。
fn compile_pattern(pattern: &str) -> Regex {
    let encoded = encode_js_units(pattern);
    let source = make_regex_prefix(&encoded);
    let source = finish_trailing_wildcard(&source);
    let source = to_regex_source(&source);
    regex::RegexBuilder::new(&source)
        .case_insensitive(true)
        .build()
        .unwrap_or_else(|_| Regex::new(NEVER_MATCH).expect("never-match regex"))
}

// ---------------------------------------------------------------------------
// 规则与匹配（npm `RuleManager` / `Ignore`）
// ---------------------------------------------------------------------------

/// 一条已编译的 ignore 规则。
///
/// 三个字段在 `create` 时一次确定，之后只读；匹配时不会修改规则。
struct IgnoreRule {
    /// `!` 前缀：命中表示“取消忽略”。
    negative: bool,
    /// 模式不含斜杠或只以斜杠结尾时为真；匹配时只看路径最后一段。
    basename_only: bool,
    /// 由 `compile_pattern` 生成，已大小写不敏感。
    regex: Regex,
}

impl IgnoreRule {
    /// npm `createRule` + `matchesBasename`。
    ///
    /// 解析顺序固定：先去掉一个前导 `!` 记为否定，再还原 `\!` / `\#` 开头的字面量。
    /// 因此 `!!x` 是否定模式 `!x`，而 `\!x` 是字面量 `!x`。
    ///
    /// `basename_only` 只看第一个 `/`：没有斜杠，或该斜杠正好是最后一个字节
    /// （`foo/` 这种目录模式）时为真；含中间斜杠的模式按完整路径匹配。
    fn create(pattern: &str) -> Self {
        let mut negative = false;
        let mut body = pattern;
        if let Some(rest) = body.strip_prefix('!') {
            negative = true;
            body = rest;
        }
        let mut body = body.to_string();
        if let Some(rest) = body.strip_prefix("\\!") {
            body = format!("!{rest}");
        }
        if let Some(rest) = body.strip_prefix("\\#") {
            body = format!("#{rest}");
        }

        let basename_only = match body.find('/') {
            None => true,
            Some(index) => index == body.len() - 1,
        };

        IgnoreRule {
            negative,
            basename_only,
            regex: compile_pattern(&body),
        }
    }
}

/// npm `Ignore` 的子集：只维护规则表并回答 `ignores(path)`。
///
/// 不保存 root 或 options：调用方负责把模式改写成相对 root 的形式，并给目录路径补上
/// 尾随 `/`。规则按插入顺序保存，`test_rules` 依赖这个顺序实现“后者覆盖前者”。
pub(crate) struct NpmIgnore {
    rules: Vec<IgnoreRule>,
}

impl NpmIgnore {
    /// 构造空规则表；等价于 npm `ignore()`（规则编译时统一大小写不敏感）。
    pub(crate) fn new() -> Self {
        NpmIgnore { rules: Vec::new() }
    }

    /// npm `add(patterns)`（逐个字符串元素；`skills.ts` 已按行切分）。
    ///
    /// 逐条过滤后编译并追加；顺序即优先级，`test_rules` 严格从头遍历。空行、注释行和
    /// 结尾带孤立反斜杠的模式会被 `check_pattern` 静默丢弃；其余模式即便生成的正则无法
    /// 编译，也会保留为“永不匹配”的规则（效果是该条不生效）。
    /// 调用方 `discover` 的注释说明 TS 的 `ig.add` 在 try/catch 内。
    pub(crate) fn add(&mut self, patterns: &[String]) {
        for pattern in patterns {
            if !check_pattern(pattern) {
                continue;
            }
            self.rules.push(IgnoreRule::create(pattern));
        }
    }

    /// npm `ignores(path)`。
    ///
    /// `path` 必须是相对规则 root 的 POSIX 路径；目录要带尾随 `/`，否则 `foo/` 这类
    /// 目录规则不会命中。纯查询，无副作用。
    pub(crate) fn ignores(&self, path: &str) -> bool {
        self.test(path, false).0
    }

    /// `Ignore._t(path, cache, checkUnignored)`：先看父目录，再测本条路径。
    ///
    /// 返回 `(ignored, unignored)`：前者是最终是否被忽略，后者表示最后命中的是否定规则。
    /// 递归检查父目录是 npm 的语义：父目录已被忽略时，子路径无法再用否定规则救回，
    /// 因此直接返回父结果（见 `negation_and_parent_recursion` 测试）。
    ///
    /// `parent_of` 每次至少去掉一个路径分量，递归因此必然终止。npm 的 `cache` 参数
    /// 没有移植，重复查询会重复计算。
    fn test(&self, path: &str, check_unignored: bool) -> (bool, bool) {
        if path.is_empty() {
            return (false, false);
        }
        if let Some(parent) = parent_of(path) {
            let parent_result = self.test(&parent, check_unignored);
            if parent_result.0 {
                return parent_result;
            }
        }
        self.test_rules(path, check_unignored)
    }

    /// `RuleManager.test`（MODE_IGNORE）。
    ///
    /// 按规则顺序确定最终状态。`skip` 的两个条件分别对应 npm 的短路逻辑：
    /// 1. 已有结论、且这条规则命中也不会改变结论时跳过；
    /// 2. 当前未命中、又不是在 `check_unignored` 模式下时跳过否定规则。
    ///
    /// `basename_only` 的规则只拿路径最后一段去匹配（目录段保留尾随 `/`），其余规则拿
    /// 完整路径。命中与否用 `regex` 判断，匹配串同样按 UTF-16 code unit 编码。
    fn test_rules(&self, path: &str, check_unignored: bool) -> (bool, bool) {
        let mut ignored = false;
        let mut unignored = false;
        let basename = basename_of(path);

        for rule in &self.rules {
            let negative = rule.negative;
            let skip = (unignored == negative && ignored != unignored)
                || (negative && !ignored && !unignored && !check_unignored);
            if skip {
                continue;
            }
            let target = if rule.basename_only {
                basename.as_str()
            } else {
                path
            };
            // npm 正则按 UTF-16 code unit 匹配。
            if rule.regex.is_match(&encode_js_units(target)) {
                ignored = !negative;
                unignored = negative;
            }
        }

        (ignored, unignored)
    }
}

/// npm `parentOf`。
///
/// 返回父路径；没有父目录（如 `a`、`a/`、`/a`）时返回 `None`。两条分支的结果形态不同：
/// - 绝对路径或含 `//` 的路径：按 `/` 切分、丢掉空段、再去掉最后一段，结果**带**尾随
///   `/` 但**不带**前导 `/`（例如 `/a/b` -> `a/`）；
/// - 普通相对路径：截取到最后一个 `/`（含），例如 `a/b` -> `a/`。
fn parent_of(path: &str) -> Option<String> {
    if path.starts_with('/') || path.contains("//") {
        let mut slices: Vec<&str> = path.split('/').filter(|piece| !piece.is_empty()).collect();
        slices.pop();
        if slices.is_empty() {
            return None;
        }
        return Some(format!("{}/", slices.join("/")));
    }

    let trimmed = path.strip_suffix('/').unwrap_or(path);
    trimmed.rfind('/').map(|index| path[..=index].to_string())
}

/// npm `basenameOf`（目录路径保留尾随 `/`）。
///
/// 目录路径带尾随 `/` 时，返回的最后一段也补回尾随 `/`（`a/foo/` -> `foo/`），这样
/// `foo/` 这类目录规则能命中；完全没有斜杠时原样返回。
fn basename_of(path: &str) -> String {
    if let Some(without) = path.strip_suffix('/') {
        return match without.rfind('/') {
            Some(index) => format!("{}/", &without[index + 1..]),
            None => path.to_string(),
        };
    }
    match path.rfind('/') {
        Some(index) => path[index + 1..].to_string(),
        None => path.to_string(),
    }
}

// 单元测试的目标是锁定 npm `ignore` 7.0.8 的可观察行为：括号/POSIX 类、globstar、
// 否定与父目录递归、UTF-16 code unit 匹配、结尾断言。多数期望值由 node + ignore@7.0.8
// 直接生成；`ignore_astral.json` 是外部基准 fixture，修改匹配逻辑后应重新核对它。
#[cfg(test)]
mod tests {
    use super::*;

    fn ignores(patterns: &[&str], path: &str) -> bool {
        let mut matcher = NpmIgnore::new();
        matcher.add(
            &patterns
                .iter()
                .map(|pattern| (*pattern).to_string())
                .collect::<Vec<_>>(),
        );
        matcher.ignores(path)
    }

    fn assert_cases(patterns: &[&str], cases: &[(&str, bool)]) {
        for (path, expected) in cases {
            assert_eq!(
                ignores(patterns, path),
                *expected,
                "patterns {patterns:?} path {path:?}"
            );
        }
    }

    #[test]
    fn brace_characters_are_literal() {
        assert_cases(
            &["skill-{a,b}/"],
            &[
                ("skill-a/", false),
                ("skill-b/", false),
                ("skill-{a,b}/", true),
            ],
        );
        assert_cases(
            &["sub/{a,b}.md"],
            &[
                ("sub/{a,b}.md", true),
                ("sub/a.md", false),
                ("sub/b.md", false),
            ],
        );
    }

    #[test]
    fn slash_only_patterns_match_nothing() {
        assert_cases(&["/"], &[("x", false), ("x/", false), ("a/b", false)]);
        assert_cases(&["//"], &[("x", false), ("x/", false)]);
    }

    #[test]
    fn bracket_semantics_match_git_wildmatch() {
        assert_cases(&["a[b"], &[("a[b", false), ("ab", false)]);
        assert_cases(
            &["a[[:alpha:]]"],
            &[("ab", true), ("a1", false), ("a[[:alpha:]]", false)],
        );
        assert_cases(&["[/]"], &[("x", false)]);
        assert_cases(&["[&/]"], &[("&", true), ("x", false)]);
    }

    #[test]
    fn negation_and_parent_recursion() {
        // 被忽略的父目录下的文件无法被重新包含。
        assert_cases(
            &["foo/", "!foo/bar.md"],
            &[("foo/", true), ("foo/bar.md", true)],
        );
        assert_cases(&["!foo"], &[("foo", false)]);
        assert_cases(
            &["*", "!foo.md"],
            &[("foo.md", false), ("bar.md", true), ("sub/foo.md", true)],
        );
    }

    #[test]
    fn globstar_forms() {
        assert_cases(
            &["a/**/b"],
            &[
                ("a/b", true),
                ("a/x/b", true),
                ("a/x/y/b", true),
                ("a/xb", false),
            ],
        );
        assert_cases(&["a/**"], &[("a/", false), ("a/b", true), ("a/b/c", true)]);
        assert_cases(
            &["a/**/"],
            &[
                ("a/", false),
                ("a/b", false),
                ("a/b/", true),
                ("a/b/c", true),
            ],
        );
        assert_cases(&["**/"], &[("x", false), ("x/", true), ("a/b", true)]);
    }

    #[test]
    fn trailing_star_and_slash_rules() {
        assert_cases(
            &["a/*"],
            &[
                ("a/", false),
                ("a/b", true),
                ("a/b/", true),
                ("a/b/c", true),
            ],
        );
        assert_cases(
            &["foo*/"],
            &[
                ("foo", false),
                ("foo/", true),
                ("foobar/", true),
                ("foobar", false),
            ],
        );
        assert_cases(
            &["*.md"],
            &[("a.md", true), ("a/b.md", true), ("ab", false)],
        );
        assert_cases(
            &["/foo/"],
            &[("foo", false), ("foo/", true), ("a/foo/", false)],
        );
    }

    #[test]
    fn literals_and_escapes() {
        assert_cases(&["a$b"], &[("a$b", true), ("ab", false)]);
        assert_cases(&["a+b"], &[("a+b", true), ("aab", false)]);
        assert_cases(&["a(b)"], &[("a(b)", true), ("ab", false)]);
        assert_cases(&["a^b"], &[("a^b", true), ("ab", false)]);
        assert_cases(&["\\*"], &[("*", true), ("a", false)]);
        assert_cases(&["\\!x"], &[("!x", true), ("x", false)]);
        assert_cases(&["\\#y"], &[("#y", true), ("y", false)]);
        assert_cases(&["a\\ b"], &[("a b", true), ("ab", false)]);
        assert_cases(&["a  "], &[("a", true), ("a  ", false)]);
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_cases(
            &["FOO.md"],
            &[("foo.md", true), ("FOO.md", true), ("sub/Foo.MD", true)],
        );
    }

    #[test]
    fn question_mark_matches_one_non_slash_char() {
        assert_cases(&["a?c"], &[("abc", true), ("a/c", false), ("a?c", true)]);
        assert_cases(
            &["a?*.md"],
            &[("ab.md", true), ("a.md", false), ("abc.md", true)],
        );
    }

    #[test]
    fn matches_utf16_code_units_like_the_js_regex() {
        // 期望值由 node 26 + ignore@7.0.8 生成：npm 的正则不带 `u` 标志，
        // astral 字符（emoji）占两个 code unit。
        assert_cases(
            &["?"],
            &[("😀", false), ("a", true), ("a😀", false), ("?", true)],
        );
        assert_cases(&["??"], &[("😀", true), ("😀a", false), ("ab", true)]);
        assert_cases(&["?a"], &[("😀a", false)]);
        assert_cases(
            &["[!a]"],
            &[("😀", false), ("b", true), ("😀a", false), ("a", false)],
        );
        assert_cases(&["[a-z]"], &[("😀", false), ("m", true)]);
        assert_cases(&["[😀]"], &[("😀", false)]);
        assert_cases(&["a?c"], &[("a😀c", false)]);
        assert_cases(&["*😀"], &[("a😀", true), ("a😀b", false)]);
        assert_cases(&["?*"], &[("😀", true), ("😀a", true)]);
    }

    #[test]
    fn matches_the_npm_ignore_astral_fixture() {
        // 期望值由 node 26 + ignore@7.0.8 生成（pattern、path、ignores）。
        let data = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/skills/ignore_astral.json"
        ));
        let cases: Vec<(String, String, bool)> = serde_json::from_str(data).expect("parse fixture");
        assert!(!cases.is_empty());
        let mut matcher = NpmIgnore::new();
        let mut current = String::new();
        let mut failures = Vec::new();
        for (pattern, path, expected) in &cases {
            if *pattern != current {
                matcher = NpmIgnore::new();
                matcher.add(std::slice::from_ref(pattern));
                current = pattern.clone();
            }
            let actual = matcher.ignores(path);
            if actual != *expected {
                failures.push(format!(
                    "{pattern:?} vs {path:?}: expected {expected}, got {actual}"
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} mismatches:\n{}",
            failures.len(),
            failures
                .iter()
                .take(20)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn hash_at_start_is_a_comment_but_mid_pattern_is_literal() {
        assert_cases(&["#x"], &[("#x", false), ("x", false)]);
        assert_cases(&["a#b"], &[("a#b", true)]);
    }

    #[test]
    fn blank_and_trailing_backslash_patterns_are_skipped() {
        assert!(!check_pattern(""));
        assert!(!check_pattern("   "));
        assert!(!check_pattern("a\\"));
        assert!(check_pattern("a\\\\"));
        assert!(!check_pattern("#x"));
    }

    #[test]
    fn end_assertion_does_not_let_patterns_match_descendants() {
        // npm `_make` 的结尾断言是 `(?=$|\/$)`：匹配位置之后必须是结尾，或
        // “斜杠 + 结尾”。翻译成可消费的等价物时不能只消费斜杠，否则非 basename
        // 模式会顺带匹配所有后代路径（否定规则因此错误地取消忽略子项）。
        // 期望值由 node-ignore 7.0.8 生成。
        assert_cases(
            &["a/**", "!a/b"],
            &[
                ("a/b", false),
                ("a/b/", false),
                ("a/b/c", true),
                ("a/b/c/", true),
                ("a/x", true),
                ("a/", false),
            ],
        );
        assert_cases(
            &["a/*", "!/a"],
            &[("a/x", true), ("a/", false), ("a", false)],
        );
        // 无斜杠（basename）模式仍匹配任意层级的同名文件。
        assert_cases(
            &["a", "!a/b"],
            &[("a/b", true), ("a/b/c", true), ("b/a", true), ("a", true)],
        );
    }
}
