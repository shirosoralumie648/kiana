//! diff.ts（行级 diff 着色）。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/diff.ts`。
//!
//! 依赖 npm `diff@8.0.4` 的 `diffWords`。本文件把该库的 `word.js` + `base.js`（Myers 核心）
//! 与 `util/string.js` 的空白整理逻辑**逐行移植**为私有函数（不能换成 `similar`：分词与
//! 合并规则不同，输出也不同）。`diffWords` 的公共结果只在本文件内部消费。
//!
//! 与 TS 的差异：
//! * JS 字符串索引是 UTF-16 code unit；Rust 的 `String` 不能表示孤立代理项。最长公共
//!   前后缀/最长重叠在这里按 `char` 比较——对合法 UTF-8 输入，token 边界、trim、空白
//!   处理与 JS 完全一致；只有「两个不同星形平面字符共享高代理项」这类病态输入会在
//!   公共前缀的长度上差一个 code unit（JS 会切出孤立代理项，输出本身也是损坏的）。
//! * `process` 无关。
//! * 主题 API 使用类型化 token（[`ThemeColor`]）。
//!
//! `render_diff` 的解析规则逐字保留：`parseDiffLine` 的正则
//! `^([+-\s])(\s*\d*)\s(.*)$` 里 JS 的 `\s`（含 U+FEFF、不含 U+0085）与 ASCII 的 `\d`
//! 都用显式字符类表达；`.` 不匹配 `\n`/`\r`/U+2028/U+2029；因此含 `\r`（CRLF 输入）的行
//! 解析失败、整行按 context 原样输出——这与 JS 一致。
//!
//! # 这个文件解决什么问题
//!
//! 输入一段 diff 文本（典型来源是编辑工具结果的 `result.diff`，行格式为
//! `-12 old` / `+12 new` / ` 12 context`，生成逻辑见 `crates/kiana-tools/src/edit_diff.rs`
//! 的 `generate_diff_string`），输出带 ANSI 颜色的字符串：删除行用 `ToolDiffRemoved`、
//! 新增行用 `ToolDiffAdded`、context 行与无法解析的行用 `ToolDiffContext`。若某个删除块
//! 恰好是「一行删除 + 一行新增」，还会对这两行做词级比对，把改动到的词用 `inverse`
//! 反显标出。没有它，上层只能原样打印 diff，读者需要自己分辨一行内改了哪几个词。
//!
//! # 在系统中的位置
//!
//! 调用方：
//! * `kiana-tools` 的编辑结果渲染器（`crates/kiana-tools/src/renderers/edit.rs`）：结果 diff
//!   构造 `RenderDiffOptions { file_path }` 后传入，预览 diff 则传 `None`；
//! * `kiana-interactive` 的 components barrel 原样再导出；`kiana-coding-agent` 再以
//!   `renderDiff` 的 camelCase 别名导出。
//!
//! 被调用方：只有 `kiana-theme` 的全局主题（`THEME.fg` 取前景色、`THEME.inverse` 反显）。
//! 渲染路径不读写文件、不接触终端、不持有任何跨调用状态；输入 `&str`，输出 `String`，
//! 唯一的隐式输入是当前全局主题。
//!
//! # 执行流程
//!
//! 1. `render_diff` 按 `'\n'` 切行，逐行用 `parse_diff_line` 解析出前缀、行号串与内容。
//! 2. 前缀为 `-` 时，先连续收集删除行，再连续收集紧随其后的新增行；只有「1 删 + 1 增」
//!    才走 `render_intra_line_diff`，否则整块按「先全部删除、后全部新增」输出。前缀为 `+`
//!    的行单独输出；其余（含解析失败）都按 context 输出。
//! 3. `render_intra_line_diff` 调 `diff_words`（本文件下半部分的 jsdiff 移植）得到词级
//!    `Change` 列表，把 added/removed 片段包 `inverse`，公共片段同时写入两行。
//! 4. `diff_words` 三步走：分词（`tokenize` + `remove_empty`）→ token 级 Myers
//!    （`diff_tokens`，token 相等性是比较 trim 后的值）→ 空白归属后处理（`post_process`）。
//!
//! # 注意事项
//!
//! * 颜色取自**调用时**的全局主题；主题未初始化时 `render_diff` 会 panic（见 [`render_diff`]
//!   的 Panics 节）。
//! * 本文件是行为契约，不是可随意替换的实现：jsdiff 的分词、trim 相等、空白归一任何一步
//!   换掉（例如改用 `similar` crate），都会破坏黄金样张锁定的逐字节输出。`tests/fixtures/
//!   diff_words.json` 与 `tests/fixtures/diff_render.json` 是 TS 侧生成的黄金样张，专用于
//!   锁定这些细节。
//! * 输出行数等于 `diff_text.split('\n')` 的元素个数：每消费一个输入行恰好产生一个输出行，
//!   空输入与结尾换行因此也会各多出一个空行。

use std::collections::HashMap;
use std::sync::LazyLock;

use kiana_theme::ThemeColor;
use regex::Regex;

/// 按全局主题着色（TS `theme.fg(token, str)`）。
///
/// 每次调用都读当前全局主题，不做缓存。失败路径：`THEME.fg` 返回 `Err` 时这里 `expect`
/// panic——可能是还没调用 `kiana_theme::init_theme`，也可能是当前主题缺少该 token
/// （后者见 `Theme::try_fg` 的文档）。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// invert（TS `theme.inverse(text)`）。
///
/// 主题层用 `\x1b[7m` / `\x1b[27m` 包住文本（单测里的 `mark_inverse` 依赖这两个序列）。
/// 与 [`fg`] 一样每次读全局主题；未初始化时 panic。
fn inverse(text: &str) -> String {
    kiana_theme::THEME
        .inverse(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

// ============================================================================
// diff.ts
// ============================================================================

/// `RenderDiffOptions`（TS 接口；`file_path` 保留但未使用）。
///
/// 这是一个**兼容壳**：TS 的 `renderDiff(diffText, _options = {})` 接受该对象，但函数体
/// 从未读取它。Rust 侧保留同名字段，使调用点与 TS 一一对应（`kiana-tools` 的编辑结果渲染器
/// 会传入 `file_path`），`render_diff` 的 `_options` 参数因此不参与任何计算。
///
/// `Default` 得到 `file_path: None`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderDiffOptions {
    /// 文件路径（unused, kept for API compatibility）。
    ///
    /// `render_diff` 不读取它；保留只为与 TS 的 `filePath?: string` 字段对齐。
    pub file_path: Option<String>,
}

/// `parseDiffLine(line)`：提取 prefix / lineNum / content。
///
/// 正则逐字：`^([+-\s])(\s*\d*)\s(.*)$`（JS `\s`/`\d`/`.` 语义见模块文档）。三个捕获组：
/// 1. 单个前缀字符（`+`、`-` 或任意 JS 空白，例如 context 行的前导空格/Tab）；
/// 2. 行号串，**原样**返回（`\s*\d*` 允许前导空白，甚至允许为空）；
/// 3. 行内容，允许为空串。
///
/// 返回 `None` 表示该行不是 diff 行（`@@ ... @@`、`--- a/file`、`+1` 这种数字后缺空格的
/// 行、CRLF 行等），调用方会把它当 context 原样输出。
///
/// 正则用显式字符类而不依赖 Rust `regex` 的 `\s`：后者是 Unicode `White_Space`，
/// **不含** U+FEFF，与 JS 的 `\s` 不同（这也是常量 `JS_WS` 存在的原因）。正则只编译一次
/// （`LazyLock`），之后每行只做匹配。
fn parse_diff_line(line: &str) -> Option<ParsedLine> {
    static PARSE_DIFF_LINE: LazyLock<Regex> = LazyLock::new(|| {
        const JS_WS: &str = "\\u{0009}\\u{000A}\\u{000B}\\u{000C}\\u{000D}\\u{0020}\
\\u{00A0}\\u{1680}\\u{2000}-\\u{200A}\\u{2028}\\u{2029}\\u{202F}\\u{205F}\\u{3000}\\u{FEFF}";
        let pattern = format!(
            "^([+\\-{JS_WS}])([{JS_WS}]*[0-9]*)[{JS_WS}]([^\\n\\r\\u{{2028}}\\u{{2029}}]*)$"
        );
        Regex::new(&pattern).expect("valid parseDiffLine regex")
    });

    let captures = PARSE_DIFF_LINE.captures(line)?;
    Some(ParsedLine {
        prefix: captures.get(1)?.as_str().chars().next()?,
        line_num: captures.get(2)?.as_str().to_string(),
        content: captures.get(3)?.as_str().to_string(),
    })
}

/// `parseDiffLine` 的结果（字段名与上游实现逐字对应）。
///
/// 只由 [`parse_diff_line`] 构造；字段全部按捕获组原样保存，不做数字解析或 trim，
/// 因为输出行要逐字拼回 `line_num`（TS 同样如此）。
struct ParsedLine {
    /// `match[1]`：`'+'`、`'-'` 或空白字符。
    prefix: char,
    /// `match[2]`：**原样**保留的捕获串（可能为空、可能带前导空白）。
    line_num: String,
    /// `match[3]`：行内容（允许为空）。
    content: String,
}

/// `replaceTabs(text)`：`\t` → 三个空格。
///
/// 只在**能被正则解析**的行上调用（context / 单增 / 删除块）；解析失败的行按 TS 原样输出，
/// 不替换 tab（见 `render_diff` 的分支）。
fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

/// `renderIntraLineDiff(oldContent, newContent)`：词级 diff + inverse。
///
/// 只有**第一个** removed / 第一个 added 片段剥一次前导空白（剥出的空白不着色）；
/// 后续片段整体（含前导空白）都包 `inverse`。公共片段同时追加到两行。
///
/// 参数是调用方**已经 `replace_tabs` 过**的行内容；返回的两行不含行首的 `-lineNum ` /
/// `+lineNum ` 前缀（由 `render_diff` 负责拼接）。
///
/// 设计原因：当第一个 changed 片段带前导空白时（黄金表 `tests/fixtures/diff_words.json`
/// 中约占 4% 的用例），整段反显会把这块空白也涂色，因此只对首个片段剥一次；这是 TS 的
/// 原始规则（spec §2.57.2），不能“顺手”改成对每个片段都剥。
///
/// 注意：公共片段的值取自**新**文本的 token 拼接（见 `build_values`），所以当两行只差空白
/// 时，删除行显示的空白可能已被“归一”为新文本的写法；`diff_words` 的单测
/// `diff_words_absorbs_pure_whitespace_differences` 锁定了这一点。
fn render_intra_line_diff(old_content: &str, new_content: &str) -> (String, String) {
    let word_diff = diff_words(old_content, new_content);

    let mut removed_line = String::new();
    let mut added_line = String::new();
    let mut is_first_removed = true;
    let mut is_first_added = true;

    for part in &word_diff {
        if part.removed {
            let mut value = part.value.as_str();
            if is_first_removed {
                let leading_ws = leading_ws(value);
                value = &value[leading_ws.len()..];
                removed_line.push_str(leading_ws);
                is_first_removed = false;
            }
            if !value.is_empty() {
                removed_line.push_str(&inverse(value));
            }
        } else if part.added {
            let mut value = part.value.as_str();
            if is_first_added {
                let leading_ws = leading_ws(value);
                value = &value[leading_ws.len()..];
                added_line.push_str(leading_ws);
                is_first_added = false;
            }
            if !value.is_empty() {
                added_line.push_str(&inverse(value));
            }
        } else {
            removed_line.push_str(&part.value);
            added_line.push_str(&part.value);
        }
    }

    (removed_line, added_line)
}

/// `renderDiff(diffText, options)`：给 diff 文本着色并做行内变更高亮。
///
/// * 非匹配行：`theme.fg("toolDiffContext", line)`，**不做** `replaceTabs`；
/// * context 行：`" {lineNum} {replaceTabs(content)}"`；
/// * 单个 `-`/`+`：`replaceTabs` 后做行内词 diff；
/// * 多行块：先全部 removed、再全部 added。
///
/// 补充：行内词 diff 只在同一个删除块内恰好收集到 1 行 `-` 与 1 行 `+` 时才发生
/// （相邻的删除行、相邻的新增行会一起收集）；单独的 `-` 行或 `+` 行不与任何行配对。
///
/// # 参数
///
/// * `diff_text`：diff 文本。只按 `'\n'` 切分（不用 `str::lines`），因此 `\r` 会留在行尾，
///   并可能导致该行解析失败。
/// * `_options`：仅为对齐 TS 签名而保留，函数体不读取（见 [`RenderDiffOptions`]）。
///
/// # 返回
///
/// 已着色的字符串：每个输入行对应一个输出行，用 `'\n'` 连接。着色来自全局主题的
/// `ToolDiffContext` / `ToolDiffRemoved` / `ToolDiffAdded` 前景色；行内变化的片段再包
/// `inverse`。行号不做数字解析，正则捕获到什么就原样拼接（可能带前导空白）。
///
/// # 边界
///
/// * 空输入 `""` 也按一行处理，产出一个被 context 色包裹的空行；
/// * 以 `\n` 结尾的输入会多切出一个尾部空串，故输出多一个空 context 行（即保留尾部换行）；
/// * 含 `\r` 的行（CRLF 输入）、`--- a/file` / `+++ b/file`、`@@ ... @@`、`+1`（数字后无
///   空格）等不匹配行首正则的文本整行走 context 分支，且**不替换 tab**；
/// * 删除块后面的连续新增行会被一并消费；若中间夹了 context 行，则两段分别处理。
///
/// # Panics
///
/// 没有任何全局主题被设置时 panic（例如尚未调用 `kiana_theme::init_theme`）——内部对
/// `THEME.fg(...)` / `THEME.inverse(...)` 使用 `expect`。若当前主题缺少某个 token，
/// `fg` 也会 panic。
pub fn render_diff(diff_text: &str, _options: Option<&RenderDiffOptions>) -> String {
    // 注意：`split('\n')` 保留 `\r` 与尾部空元素（不能用 `lines()`）。
    let lines: Vec<&str> = diff_text.split('\n').collect();
    let mut result: Vec<String> = Vec::new();

    // 行级状态机：每次循环至少消费一行 `lines[i]`，并把**同样数量**的着色行推进
    // `result`（删除块一次消费多行时也一样）。因此 `result.len()` 最终恒等于 `lines.len()`，
    // 这也解释了空输入/结尾换行为什么会多出空行。
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        // 解析失败不是错误：文件头、`@@`、CRLF 行等都属于合法的“不可着色”文本。
        let Some(parsed) = parse_diff_line(line) else {
            result.push(fg(ThemeColor::ToolDiffContext, line));
            i += 1;
            continue;
        };

        if parsed.prefix == '-' {
            // 收集连续的 removed 行。
            let mut removed_lines: Vec<(String, String)> = Vec::new();
            while i < lines.len() {
                let Some(p) = parse_diff_line(lines[i]) else {
                    break;
                };
                if p.prefix != '-' {
                    break;
                }
                removed_lines.push((p.line_num, p.content));
                i += 1;
            }

            // 收集连续的 added 行。
            let mut added_lines: Vec<(String, String)> = Vec::new();
            while i < lines.len() {
                let Some(p) = parse_diff_line(lines[i]) else {
                    break;
                };
                if p.prefix != '+' {
                    break;
                }
                added_lines.push((p.line_num, p.content));
                i += 1;
            }

            // 只有单删单增才做行内 diff（表示单行修改）。
            if removed_lines.len() == 1 && added_lines.len() == 1 {
                let (removed_num, removed_content) = &removed_lines[0];
                let (added_num, added_content) = &added_lines[0];
                let (removed_line, added_line) = render_intra_line_diff(
                    &replace_tabs(removed_content),
                    &replace_tabs(added_content),
                );
                result.push(fg(
                    ThemeColor::ToolDiffRemoved,
                    &format!("-{removed_num} {removed_line}"),
                ));
                result.push(fg(
                    ThemeColor::ToolDiffAdded,
                    &format!("+{added_num} {added_line}"),
                ));
            } else {
                // 多行块不做词级 diff（TS 只在“1 删 + 1 增”时按单行修改处理）；输出顺序固定为
                // 先全部 removed、再全部 added。输入中交错排列的 `-`/`+` 会被切成多个块
                // （每个 `-` 块连带消费紧随其后的 `+` 块），块与块之间仍按输入顺序输出。
                for (line_num, content) in &removed_lines {
                    result.push(fg(
                        ThemeColor::ToolDiffRemoved,
                        &format!("-{line_num} {}", replace_tabs(content)),
                    ));
                }
                for (line_num, content) in &added_lines {
                    result.push(fg(
                        ThemeColor::ToolDiffAdded,
                        &format!("+{line_num} {}", replace_tabs(content)),
                    ));
                }
            }
        } else if parsed.prefix == '+' {
            // 孤立的 added 行（前面没有紧邻的删除块）：整行着色，不做词级 diff。
            result.push(fg(
                ThemeColor::ToolDiffAdded,
                &format!("+{} {}", parsed.line_num, replace_tabs(&parsed.content)),
            ));
            i += 1;
        } else {
            // 其余前缀（通常是一个空白）都是 context 行；输出统一用一个前导空格 + lineNum，
            // 因此输入行首的 Tab/NBSP 等会被归一（line_num 捕获串本身是什么就保留什么）。
            result.push(fg(
                ThemeColor::ToolDiffContext,
                &format!(" {} {}", parsed.line_num, replace_tabs(&parsed.content)),
            ));
            i += 1;
        }
    }

    // `join` 不做任何 trim：尾部空行会变成末尾的 `\n`，与 TS 的 `result.join("\n")` 一致。
    result.join("\n")
}

// ============================================================================
// jsdiff `diffWords` 移植（diff@8.0.4 的 word.js / base.js / util/string.js）
// ============================================================================
//
// 以下函数逐行对应 jsdiff 的内部实现，全部私有：外部只能通过 `render_diff` 观察其行为。
// 为了保持与 JS 相同的分支与顺序，这里保留了 JS 的控制流（HashMap 表示 Myers 前沿、
// 索引竞技场代替可变链表对象），看起来比“地道”的 Rust 写法更绕。改动任何一步（分词规则、
// 相等性、空白归一）都会破坏黄金样张锁定的逐字节输出；请用 `tests/fixtures/diff_words.json`
// 的黄金样张验证，不要用 `similar` 之类的通用 diff crate 替换。

/// jsdiff 的 `Change` 对象。
///
/// 不变量：`added` 与 `removed` 不会同时为 true；两者都为 false 表示公共片段。
/// `diff_tokens`（经 `build_values`）只产生「added」「removed」「公共」三种组合，
/// `post_process` 只改 `value`，不改标记。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    value: String,
    added: bool,
    removed: bool,
}

/// JS 的 `\s` 字符集。
///
/// ECMAScript WhiteSpace + LineTerminator：`\t\n\v\f\r`、空格、NBSP、U+1680、U+2000–U+200A、
/// U+2028/2029、U+202F、U+205F、U+3000、U+FEFF。**包含** U+FEFF，**不包含** U+0085。
///
/// 不能换成 `char::is_whitespace`：后者按 Unicode `White_Space` 属性判断，**包含** U+0085
/// 而**不包含** U+FEFF，与本函数在边界字符上正好相反，会让 trim/分词结果与 TS 分叉。
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
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

/// `extendedWordChars` 字符集（word.js 的逐字区间）。
///
/// 只有 ASCII 字母数字/下划线与上述拉丁扩展区段算“单词字符”。其他字母（CJK、希腊、西里尔）
/// 不在任何区段里，在分词时会被当作普通非空白字符、**每个 `char` 单独成 token**，所以
/// `你好世界` 会逐字比较（见单测 `diff_words_splits_cjk_per_character`）。这是 jsdiff 的
/// 既有行为，不是本移植的选择。
fn is_extended_word_char(c: char) -> bool {
    matches!(
        c,
        'a'..='z'
            | 'A'..='Z'
            | '0'..='9'
            | '_'
            | '\u{00AD}'
            | '\u{00C0}'..='\u{00D6}'
            | '\u{00D8}'..='\u{00F6}'
            | '\u{00F8}'..='\u{02C6}'
            | '\u{02C8}'..='\u{02D7}'
            | '\u{02DE}'..='\u{02FF}'
            | '\u{1E00}'..='\u{1EFF}'
    )
}

/// `leadingWs(string)`。
fn leading_ws(text: &str) -> &str {
    let end = text
        .char_indices()
        .find(|(_, c)| !is_js_whitespace(*c))
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    &text[..end]
}

/// `trailingWs(string)`。
fn trailing_ws(text: &str) -> &str {
    let mut start = text.len();
    for (index, c) in text.char_indices().rev() {
        if is_js_whitespace(c) {
            start = index;
        } else {
            break;
        }
    }
    &text[start..]
}

/// `leadingAndTrailingWs(string)`（无 segmenter 分支）。
fn leading_and_trailing_ws(text: &str) -> (&str, &str) {
    (leading_ws(text), trailing_ws(text))
}

/// JS `String.prototype.trim()`：去掉两端 `\s`（含 U+FEFF）。
///
/// 不用 `str::trim`：名字相同但字符集不同（见 `is_js_whitespace`），U+FEFF 和 U+0085
/// 的处理正好与 JS 相反。返回的是原串的子切片；全为空白时返回空串。
fn js_trim(text: &str) -> &str {
    let start = text
        .char_indices()
        .find(|(_, c)| !is_js_whitespace(*c))
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    let end = text
        .char_indices()
        .rev()
        .find(|(_, c)| !is_js_whitespace(*c))
        .map(|(index, c)| index + c.len_utf8())
        .unwrap_or(0);
    if start >= end { "" } else { &text[start..end] }
}

/// `longestCommonPrefix(str1, str2)`（JS `str1.slice(0, i)`）。
fn longest_common_prefix(first: &str, second: &str) -> String {
    let mut prefix = String::new();
    for (a, b) in first.chars().zip(second.chars()) {
        if a != b {
            break;
        }
        prefix.push(a);
    }
    prefix
}

/// `longestCommonSuffix(str1, str2)`。
fn longest_common_suffix(first: &str, second: &str) -> String {
    if first.is_empty() || second.is_empty() {
        return String::new();
    }
    let mut count = 0usize;
    let mut left = first.chars().rev();
    let mut right = second.chars().rev();
    loop {
        match (left.next(), right.next()) {
            (Some(a), Some(b)) if a == b => count += 1,
            _ => break,
        }
    }
    if count == 0 {
        return String::new();
    }
    let chars: Vec<char> = first.chars().collect();
    chars[chars.len() - count..].iter().collect()
}

/// `replacePrefix(string, oldPrefix, newPrefix)`。
///
/// 前置条件：`text` 必须以 `old_prefix` 开头；不满足时 panic（`new_prefix` 为空即删除前缀）。
/// panic 表示调用方的空白归属假设被破坏，属于内部不变量错误，而不是可恢复的输入错误。
/// 直接调用者是 `dedupe_whitespace_in_change_objects`，以及只做转发的 `remove_prefix` /
/// `remove_suffix`（新前缀为空串）；都位于空白归并路径。
fn replace_prefix(text: &str, old_prefix: &str, new_prefix: &str) -> String {
    if !text.starts_with(old_prefix) {
        panic!("string {text:?} doesn't start with prefix {old_prefix:?}; this is a bug");
    }
    format!("{new_prefix}{}", &text[old_prefix.len()..])
}

/// `replaceSuffix(string, oldSuffix, newSuffix)`（空旧后缀直接追加）。
///
/// 非空 `old_suffix` 不是 `text` 的后缀时 panic；与 [`replace_prefix`] 一样，这是内部
/// 不变量检查（不满足即移植假设已被破坏），不是可恢复的用户输入错误。
fn replace_suffix(text: &str, old_suffix: &str, new_suffix: &str) -> String {
    if old_suffix.is_empty() {
        return format!("{text}{new_suffix}");
    }
    if !text.ends_with(old_suffix) {
        panic!("string {text:?} doesn't end with suffix {old_suffix:?}; this is a bug");
    }
    format!("{}{new_suffix}", &text[..text.len() - old_suffix.len()])
}

/// `removePrefix(string, oldPrefix)`。
fn remove_prefix(text: &str, old_prefix: &str) -> String {
    replace_prefix(text, old_prefix, "")
}

/// `removeSuffix(string, oldSuffix)`。
fn remove_suffix(text: &str, old_suffix: &str) -> String {
    replace_suffix(text, old_suffix, "")
}

/// `maximumOverlap(string1, string2)`：`string2.slice(0, overlapCount(string1, string2))`。
///
/// 语义：取 `second` 的前 `k` 个字符，`k` 是「`first` 的后缀同时是 `second` 的前缀」的
/// 最长长度。`dedupe_whitespace_in_change_objects` 用它找出删除片段一端与相邻保留片段
/// 另一端重叠的空白，再从删除片段中剥掉，避免同一段空白在两侧都被当成变更。
fn maximum_overlap(first: &str, second: &str) -> String {
    second.chars().take(overlap_count(first, second)).collect()
}

/// `overlapCount(a, b)`（word.js 注释里的 KMP 实现）。
///
/// 返回最大的 `k`（`k <= min(|a|, |b|)`），使得 `a` 的末尾 `k` 个字符等于 `b` 的前 `k` 个
/// 字符。因为只可能是 `a` 的后缀，扫描从 `a.len() - min(|a|, |b|)` 开始；`map` 是 jsdiff
/// 自建的回退表（上游注释称 back-reference / KMP failure links 的变体）：失配时用
/// `map[k]` 跳转，取值不等于标准前缀函数数组，不可当作 π 在别处复用。`k` 始终表示
/// 当前已匹配的 `b` 前缀长度。
///
/// 当前只在空白归并里调用，两个参数都是纯空白串（`leading_ws`/`trailing_ws` 的切片）。
fn overlap_count(first: &str, second: &str) -> usize {
    let a: Vec<char> = first.chars().collect();
    let b: Vec<char> = second.chars().collect();

    let mut start_a = 0usize;
    if a.len() > b.len() {
        start_a = a.len() - b.len();
    }
    let mut end_b = b.len();
    if a.len() < b.len() {
        end_b = a.len();
    }

    let mut map = vec![0usize; end_b];
    let mut k = 0usize;
    if end_b > 0 {
        map[0] = 0;
    }
    for j in 1..end_b {
        if b[j] == b[k] {
            map[j] = map[k];
        } else {
            map[j] = k;
        }
        while k > 0 && b[j] != b[k] {
            k = map[k];
        }
        if b[j] == b[k] {
            k += 1;
        }
    }

    k = 0;
    for &character in &a[start_a..] {
        while k > 0 && character != b[k] {
            k = map[k];
        }
        if character == b[k] {
            k += 1;
        }
    }
    k
}

/// `tokenizeIncludingWhitespace` 正则：`[word]+|\s+|[^word]`（`u` 模式，按码点）。
///
/// 三段式切分：连续单词字符一块、连续 JS 空白一块、其余每个字符单独一块。返回的每块都非空，
/// 且所有块拼接后等于原串（后面 `tokenize` 会在此基础上调整空白归属）。
fn tokenize_including_whitespace(value: &str) -> Vec<String> {
    let chars: Vec<char> = value.chars().collect();
    let mut parts = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        let start = index;
        if is_extended_word_char(chars[index]) {
            index += 1;
            while index < chars.len() && is_extended_word_char(chars[index]) {
                index += 1;
            }
        } else if is_js_whitespace(chars[index]) {
            index += 1;
            while index < chars.len() && is_js_whitespace(chars[index]) {
                index += 1;
            }
        } else {
            index += 1;
        }
        parts.push(chars[start..index].iter().collect());
    }
    parts
}

/// `WordDiff.tokenize` 的空白拼接循环。
///
/// 在 `tokenize_including_whitespace` 的三段块上再做一次空白归属调整，规则与 jsdiff 相同：
///
/// * part 是空白：并入**前一个** token 的尾部；若它是行首空白，则先自成一个 token；
/// * part 的前一块是空白（说明刚处理过空白）：若那个空白 token 仍是最后一个（只可能是
///   行首情形），把它与本 part 合并；否则把「前一空白 + 本 part」作为新 token 压入。
///
/// 结果是：除首尾两端的空白外，每段内部空白会在相邻两个 token 的边界上**各出现一次**
/// （前一个 token 的尾部、后一个 token 的头部）。`word_equals` 用 trim 比较忽略这一点，
/// `join_tokens` 拼接时剥掉后一个 token 的前导空白，所以最终文本不会重复。这是 jsdiff
/// 的既定结构，不要把它“优化”成只保留一份。
fn tokenize(value: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut prev_part: Option<String> = None;

    for part in tokenize_including_whitespace(value) {
        let part_is_ws = part.chars().next().is_some_and(is_js_whitespace);
        match &prev_part {
            None if part_is_ws => tokens.push(part.clone()),
            Some(_) if part_is_ws => {
                let last = tokens.pop().expect("token before whitespace");
                tokens.push(format!("{last}{part}"));
            }
            Some(prev) if prev.chars().next().is_some_and(is_js_whitespace) => {
                if tokens.last().is_some_and(|last| last == prev) {
                    let last = tokens.pop().expect("whitespace token");
                    tokens.push(format!("{last}{part}"));
                } else {
                    tokens.push(format!("{prev}{part}"));
                }
            }
            _ => tokens.push(part.clone()),
        }
        prev_part = Some(part);
    }

    tokens
}

/// `Diff.removeEmpty`。
///
/// `diff_words` 在进入 Myers 前丢弃空 token。当前 `tokenize` 的输出不含空串，所以这一步
/// 实际是防御性的；保留它是为了让移植步骤与上游一一对应。
fn remove_empty(tokens: Vec<String>) -> Vec<String> {
    tokens
        .into_iter()
        .filter(|token| !token.is_empty())
        .collect()
}

/// `WordDiff.equals`：`left.trim() === right.trim()`（`ignoreCase` 为 false）。
///
/// 这是 jsdiff `diffWords` 忽略纯空白差异的关键：两个 token 只要去掉首尾 JS 空白后相同，
/// Myers 就认为它们相等，于是空白变化被吸收进公共片段。不要改成逐字节相等，否则
/// `"foo  bar"` 与 `"foo bar"` 会产生额外的 added/removed。
fn word_equals(left: &str, right: &str) -> bool {
    js_trim(left) == js_trim(right)
}

/// Myers 路径状态（`bestPath[k]`）。
///
/// `old_pos` 是当前路径在旧 token 序列上已消费到的**下标**（起点为 `-1`，所以比较时都用
/// `old_pos + 1` 看“下一个”位置）；`last` 是组件链表头结点的竞技场下标，链路从新到旧。
#[derive(Debug, Clone, Copy)]
struct PathState {
    old_pos: isize,
    last: Option<usize>,
}

/// Myers 组件链表节点（`lastComponent`）。
///
/// 一个节点代表一段连续的同类编辑：`count` 个 token、`added`/`removed` 标记（两者都为
/// false 表示公共段）、`previous` 指向前一个节点。链表按时间倒序，`build_values` 会先反转。
#[derive(Debug, Clone, Copy)]
struct Component {
    count: usize,
    added: bool,
    removed: bool,
    previous: Option<usize>,
}

/// 组件节点竞技场（替代 JS 的可变链表对象）。
///
/// JS 版本用对象引用共享 `lastComponent` 链；Rust 用 `Vec` 下标表示引用，避免 `Rc/RefCell`。
/// 节点一旦 `push` 就**不再修改**：`add_to_path` 需要“在末尾追加”时会复制旧节点并压入新节点。
/// 因此已发放的下标始终有效，`Arena` 也不会删除节点。
#[derive(Default)]
struct Arena {
    nodes: Vec<Component>,
}

impl Arena {
    fn push(&mut self, component: Component) -> usize {
        self.nodes.push(component);
        self.nodes.len() - 1
    }
}

/// `Diff.addToPath`。
///
/// 在 `path` 末尾追加一个编辑步：若新步与链表头结点同类（`added`/`removed` 都相同），
/// 就把头结点计数 +1 后压入新节点（旧节点保持不变）；否则压入一个 `count = 1` 的新节点。
/// `old_pos_increment` 对删除为 `1`、对插入为 `0`，因为删除会消耗一个旧 token。
///
/// 返回新的 `PathState`，不会修改传入的 `path`（`Copy` 语义），便于前沿里保留多个候选。
fn add_to_path(
    arena: &mut Arena,
    path: PathState,
    added: bool,
    removed: bool,
    old_pos_increment: isize,
) -> PathState {
    if let Some(last_index) = path.last {
        let last = arena.nodes[last_index];
        if last.added == added && last.removed == removed {
            let previous = last.previous;
            let node = arena.push(Component {
                count: last.count + 1,
                added,
                removed,
                previous,
            });
            return PathState {
                old_pos: path.old_pos + old_pos_increment,
                last: Some(node),
            };
        }
    }
    let node = arena.push(Component {
        count: 1,
        added,
        removed,
        previous: path.last,
    });
    PathState {
        old_pos: path.old_pos + old_pos_increment,
        last: Some(node),
    }
}

/// `Diff.extractCommon`：返回新的 `newPos`，就地更新 `path`。
///
/// Myers 的“沿对角线滑行”：`new_pos = old_pos - diagonal_path`（`diagonal_path` 就是
/// `old_pos - new_pos`），从两条路径的交点开始，只要下一个旧/新 token 被 `word_equals`
/// 判为相等就同时前进。滑行的步数会作为一个公共组件追加到链表；返回新的 `new_pos`，
/// `path.old_pos` 与 `path.last` 被更新。
fn extract_common(
    arena: &mut Arena,
    path: &mut PathState,
    new_tokens: &[String],
    old_tokens: &[String],
    diagonal_path: isize,
) -> isize {
    let new_len = new_tokens.len() as isize;
    let old_len = old_tokens.len() as isize;
    let mut old_pos = path.old_pos;
    let mut new_pos = old_pos - diagonal_path;
    let mut common_count = 0usize;

    while new_pos + 1 < new_len
        && old_pos + 1 < old_len
        && word_equals(
            &old_tokens[(old_pos + 1) as usize],
            &new_tokens[(new_pos + 1) as usize],
        )
    {
        new_pos += 1;
        old_pos += 1;
        common_count += 1;
    }

    if common_count > 0 {
        let node = arena.push(Component {
            count: common_count,
            added: false,
            removed: false,
            previous: path.last,
        });
        path.last = Some(node);
    }
    path.old_pos = old_pos;
    new_pos
}

/// `WordDiff.join`：第一个 token 原样，其余去掉**前导**空白后拼接。
///
/// 为什么只剥前导：`tokenize` 会让同一段内部空白出现在相邻两个 token 的边界上（见其文档），
/// 后一个 token 的前导空白是重复的一份，剥掉后才得到原文；第一个 token 的前导空白是行首
/// 缩进，必须保留。
fn join_tokens(tokens: &[String]) -> String {
    let mut joined = String::new();
    for (index, token) in tokens.iter().enumerate() {
        if index == 0 {
            joined.push_str(token);
        } else {
            let ws_len = leading_ws(token).len();
            joined.push_str(&token[ws_len..]);
        }
    }
    joined
}

/// `Diff.buildValues`：把组件链表转成 `Change` 数组（removed 在 added 之前、同类合并计数）。
///
/// 链表按时间倒序，先反向遍历再 `reverse`。遍历时用两个游标消费 token 切片：
/// * 非删除组件（公共或新增）从 `new_tokens` 取 `count` 个；公共组件同时推进 `old_pos`，
///   新增组件不推进（它没有消耗旧 token）；
/// * 删除组件从 `old_tokens` 取，并推进 `old_pos`。
///
/// 因此**公共片段的文本取自新 token 序列**：两行只差空白时，输出采用新文本的空白写法
/// （单测 `diff_words_absorbs_pure_whitespace_differences` 锁定了该行为）。
/// 组件记录的 `count` 正是 Myers 路径实际消费的 token 数，所以切片不会越界。
fn build_values(
    arena: &Arena,
    last_component: Option<usize>,
    new_tokens: &[String],
    old_tokens: &[String],
) -> Vec<Change> {
    let mut components = Vec::new();
    let mut current = last_component;
    while let Some(index) = current {
        components.push(arena.nodes[index]);
        current = arena.nodes[index].previous;
    }
    components.reverse();

    let mut values = Vec::with_capacity(components.len());
    let mut new_pos = 0usize;
    let mut old_pos = 0usize;
    for component in components {
        if !component.removed {
            let value = join_tokens(&new_tokens[new_pos..new_pos + component.count]);
            new_pos += component.count;
            if !component.added {
                old_pos += component.count;
            }
            values.push(Change {
                value,
                added: component.added,
                removed: component.removed,
            });
        } else {
            let value = join_tokens(&old_tokens[old_pos..old_pos + component.count]);
            old_pos += component.count;
            values.push(Change {
                value,
                added: component.added,
                removed: component.removed,
            });
        }
    }
    values
}

/// `Diff.diffWithOptionsObj` 的 Myers 核心（无 callback / maxEditLength / timeout）。
///
/// # 输入输出
///
/// `old_tokens` / `new_tokens` 是已分词的 token 序列。返回按路径顺序排列的 `Change` 列表，
/// 公共片段的值取自新序列（见 `build_values`）。token 相等性用 `word_equals`（trim 后比较），
/// 因此纯空白差异不算编辑。
///
/// # 算法
///
/// 标准 Myers“最远到达”前沿：`best_path[k]` 保存对角线 `k = old_pos - new_pos` 上走得最远
/// 的路径末端。编辑距离 `d` 从 1 递增，每轮只访问 `|k| <= d` 的斜线（并受两端剪枝约束）；
/// 斜线 `k` 的两条前驱是 `k-1`（再删一个旧 token）与 `k+1`（再插一个新 token）。
/// 选择 `old_pos` 更大的候选（持平选删除，由下面的 `<` 决定），再调用 `extract_common`
/// 沿对角线吃掉相等 token。两个序列都耗尽时用 `build_values` 收尾。
///
/// # 边界
///
/// `max_edit_length = old_len + new_len` 覆盖最坏情况（全删全增），因此循环必然在某个 `d`
/// 上到达终点；末尾的空 `Vec` 只是防御性返回。两侧都为空（或完全相同）时走 seed 的提前返回；
/// 只有一侧为空时会正常进入主循环（例如全增/全删）。
fn diff_tokens(old_tokens: &[String], new_tokens: &[String]) -> Vec<Change> {
    let new_len = new_tokens.len() as isize;
    let old_len = old_tokens.len() as isize;
    let mut arena = Arena::default();

    // 前沿表：对角线 k -> 该斜线上已到达的最远路径；k = old_pos - new_pos。
    let mut best_path: HashMap<isize, PathState> = HashMap::new();
    // 起点在 (-1, -1)：先把两个序列的公共前缀整段吃掉；若这就是全部输入，直接收尾。
    let mut seed = PathState {
        old_pos: -1,
        last: None,
    };
    let seed_new_pos = extract_common(&mut arena, &mut seed, new_tokens, old_tokens, 0);
    if seed.old_pos + 1 >= old_len && seed_new_pos + 1 >= new_len {
        return build_values(&arena, seed.last, new_tokens, old_tokens);
    }
    best_path.insert(0, seed);

    let mut min_diagonal_to_consider = isize::MIN;
    let mut max_diagonal_to_consider = isize::MAX;
    let max_edit_length = new_len.saturating_add(old_len);

    let mut edit_length: isize = 1;
    while edit_length <= max_edit_length {
        // 第 d 轮：前沿从 -d 扫到 +d（再用两端剪枝收紧范围）。
        let mut diagonal_path = min_diagonal_to_consider.max(-edit_length);
        let upper = max_diagonal_to_consider.min(edit_length);
        while diagonal_path <= upper {
            let remove_path = best_path.remove(&(diagonal_path - 1));
            let add_path = best_path.get(&(diagonal_path + 1)).copied();

            // 两条前驱：k-1 经一次删除到达，k+1 经一次插入到达；还要确认目标下标没有越过
            // 旧/新序列末尾（`old_pos` 是已消费下标，故用 +1 判断）。
            let mut can_add = false;
            if let Some(add) = add_path {
                let add_path_new_pos = add.old_pos - diagonal_path;
                can_add = add_path_new_pos >= 0 && add_path_new_pos < new_len;
            }
            let can_remove = remove_path.is_some_and(|path| path.old_pos + 1 < old_len);

            if !can_add && !can_remove {
                // 两条前驱都不可用（这条斜线已在编辑图外）：清掉可能残留的旧值。
                best_path.remove(&diagonal_path);
                diagonal_path += 2;
                continue;
            }

            // 选优：`!can_remove` 时只能插入；否则取 `old_pos` 更大的候选；两者持平选删除
            // （`<` 而不是 `<=`），与 jsdiff 的分支顺序一致。
            let mut base_path = if !can_remove
                || (can_add
                    && remove_path.expect("canRemove").old_pos < add_path.expect("canAdd").old_pos)
            {
                add_to_path(&mut arena, add_path.expect("canAdd"), true, false, 0)
            } else {
                add_to_path(&mut arena, remove_path.expect("canRemove"), false, true, 1)
            };

            // 沿当前对角线吃掉相等的 token；若两端都到底，这条路径即为最优解。
            let new_pos = extract_common(
                &mut arena,
                &mut base_path,
                new_tokens,
                old_tokens,
                diagonal_path,
            );
            if base_path.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                return build_values(&arena, base_path.last, new_tokens, old_tokens);
            }

            best_path.insert(diagonal_path, base_path);
            // 剪枝（对应 jsdiff 的 min/maxDiagonalToConsider）：路径已耗尽旧 token 时把
            // 下一轮的斜线上界收到 `k - 1`；已耗尽新 token 时把下界抬到 `k + 1`。
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

    // maxEditLength 覆盖了整张编辑图，Myers 必然找到路径；保底返回空数组。
    Vec::new()
}

/// `dedupeWhitespaceInChangeObjects`。
///
/// jsdiff 词级 diff 的空白后处理：消除删除/插入片段与相邻保留片段之间重复的空白
/// （jsdiff word.js 注释：trailing whitespace in one object is repeated as leading
/// whitespace in the next），避免拼接回文本时空白重复；因此 `render_intra_line_diff`
/// 也更少把重复空白反显（这是效果，而非上游声明的动机）。
/// 把变更片段（deletion/insertion）两端与相邻保留片段共享的空白挪到保留片段一侧。
/// 由 `post_process` 在扫描到保留片段时调用。
///
/// 参数：
/// * `changes`：整个 `Change` 列表，函数会原地改写多个元素的值（**包括保留片段**）；
/// * `start_keep`：变更串**前面**最近的公共片段下标，没有则为 `None`；
/// * `deletion` / `insertion`：变更串中最后一个删除/新增片段的下标；
/// * `end_keep`：变更串**后面**最近的公共片段下标，直到列表末尾则为 `None`。
///
/// 分支按「有删除/有插入 × 有无前后保留」划分，逐条对应 jsdiff 的同名分支：
/// 1. 两者都有：分别处理前导空白（共同前缀）与尾随空白（共同后缀）；
/// 2. 只有插入：剥掉插入片段或后保留片段的重复前导空白；
/// 3. 只有删除且两侧都有保留：用后保留片段的前导空白与删除片段的两端空白求公共前后缀并重分配；
/// 4. 只有删除且只有后保留：用 `maximum_overlap` 求删除尾部空白与后保留前导空白的重叠并剥掉；
/// 5. 只有删除且只有前保留：对称地处理删除前导空白与前保留尾部空白。
///
/// 这些步骤依赖 `tokenize` 造成的「内部空白在相邻 token 边界重复」这一结构；前置条件不成立时
/// `replace_prefix` / `replace_suffix` 会 panic。各分支里的 `deletion.expect(...)` 是调用约定
/// 断言：`post_process` 只在存在插入或删除时调用，而走到 3–5 分支时插入已在前面的分支被排除，
/// 所以删除必然存在。
fn dedupe_whitespace_in_change_objects(
    changes: &mut [Change],
    start_keep: Option<usize>,
    deletion: Option<usize>,
    insertion: Option<usize>,
    end_keep: Option<usize>,
) {
    if let (Some(deletion), Some(insertion)) = (deletion, insertion) {
        let deletion_value = changes[deletion].value.clone();
        let insertion_value = changes[insertion].value.clone();
        let (old_ws_prefix, old_ws_suffix) = leading_and_trailing_ws(&deletion_value);
        let (new_ws_prefix, new_ws_suffix) = leading_and_trailing_ws(&insertion_value);
        let old_ws_prefix = old_ws_prefix.to_string();
        let old_ws_suffix = old_ws_suffix.to_string();
        let new_ws_prefix = new_ws_prefix.to_string();
        let new_ws_suffix = new_ws_suffix.to_string();

        if let Some(start_keep) = start_keep {
            let common_ws_prefix = longest_common_prefix(&old_ws_prefix, &new_ws_prefix);
            changes[start_keep].value = replace_suffix(
                &changes[start_keep].value.clone(),
                &new_ws_prefix,
                &common_ws_prefix,
            );
            changes[deletion].value =
                remove_prefix(&changes[deletion].value.clone(), &common_ws_prefix);
            changes[insertion].value =
                remove_prefix(&changes[insertion].value.clone(), &common_ws_prefix);
        }
        if let Some(end_keep) = end_keep {
            let common_ws_suffix = longest_common_suffix(&old_ws_suffix, &new_ws_suffix);
            changes[end_keep].value = replace_prefix(
                &changes[end_keep].value.clone(),
                &new_ws_suffix,
                &common_ws_suffix,
            );
            changes[deletion].value =
                remove_suffix(&changes[deletion].value.clone(), &common_ws_suffix);
            changes[insertion].value =
                remove_suffix(&changes[insertion].value.clone(), &common_ws_suffix);
        }
    } else if let Some(insertion) = insertion {
        if start_keep.is_some() {
            let ws = leading_ws(&changes[insertion].value).to_string();
            changes[insertion].value = changes[insertion].value[ws.len()..].to_string();
        }
        if let Some(end_keep) = end_keep {
            let ws = leading_ws(&changes[end_keep].value).to_string();
            changes[end_keep].value = changes[end_keep].value[ws.len()..].to_string();
        }
    } else if let (Some(start_keep), Some(end_keep)) = (start_keep, end_keep) {
        let deletion = deletion.expect("caller guarantees an insertion or deletion");
        let new_ws_full = leading_ws(&changes[end_keep].value).to_string();
        let deletion_value = changes[deletion].value.clone();
        let (del_ws_start, del_ws_end) = leading_and_trailing_ws(&deletion_value);
        let del_ws_start = del_ws_start.to_string();
        let del_ws_end = del_ws_end.to_string();

        let new_ws_start = longest_common_prefix(&new_ws_full, &del_ws_start);
        changes[deletion].value = remove_prefix(&changes[deletion].value.clone(), &new_ws_start);

        let remaining_new_ws = remove_prefix(&new_ws_full, &new_ws_start);
        let new_ws_end = longest_common_suffix(&remaining_new_ws, &del_ws_end);
        changes[deletion].value = remove_suffix(&changes[deletion].value.clone(), &new_ws_end);
        changes[end_keep].value =
            replace_prefix(&changes[end_keep].value.clone(), &new_ws_full, &new_ws_end);
        let start_prefix = &new_ws_full[..new_ws_full.len() - new_ws_end.len()];
        changes[start_keep].value = replace_suffix(
            &changes[start_keep].value.clone(),
            &new_ws_full,
            start_prefix,
        );
    } else if let Some(end_keep) = end_keep {
        let end_keep_ws_prefix = leading_ws(&changes[end_keep].value).to_string();
        let deletion_ws_suffix =
            trailing_ws(&changes[deletion.expect("deletion")].value).to_string();
        let overlap = maximum_overlap(&deletion_ws_suffix, &end_keep_ws_prefix);
        changes[deletion.expect("deletion")].value = remove_suffix(
            &changes[deletion.expect("deletion")].value.clone(),
            &overlap,
        );
    } else if let Some(start_keep) = start_keep {
        let start_keep_ws_suffix = trailing_ws(&changes[start_keep].value).to_string();
        let deletion_ws_prefix =
            leading_ws(&changes[deletion.expect("deletion")].value).to_string();
        let overlap = maximum_overlap(&start_keep_ws_suffix, &deletion_ws_prefix);
        changes[deletion.expect("deletion")].value = remove_prefix(
            &changes[deletion.expect("deletion")].value.clone(),
            &overlap,
        );
    }
}

/// `WordDiff.postProcess`。
///
/// 线性扫描 `Change` 列表，记录「最近一个公共片段」以及本轮变更串里最后一个插入/删除：
/// * 遇到新增/删除：只更新对应下标；
/// * 遇到公共片段且前面有变更：以最近公共片段为 `start_keep`、当前片段为 `end_keep` 调
///   `dedupe_whitespace_in_change_objects`，然后重置本轮变更记录；
/// * 扫描结束时仍有未处理的变更：以 `end_keep = None` 再调一次（处理列表/行尾）。
///
/// `start_keep` 是**全局最近**的公共片段：若列表以变更开头，它就是 `None`。
fn post_process(changes: &mut [Change]) {
    let mut last_keep: Option<usize> = None;
    let mut insertion: Option<usize> = None;
    let mut deletion: Option<usize> = None;

    for index in 0..changes.len() {
        let added = changes[index].added;
        let removed = changes[index].removed;
        if added {
            insertion = Some(index);
        } else if removed {
            deletion = Some(index);
        } else {
            if insertion.is_some() || deletion.is_some() {
                dedupe_whitespace_in_change_objects(
                    changes,
                    last_keep,
                    deletion,
                    insertion,
                    Some(index),
                );
            }
            last_keep = Some(index);
            insertion = None;
            deletion = None;
        }
    }

    if insertion.is_some() || deletion.is_some() {
        dedupe_whitespace_in_change_objects(changes, last_keep, deletion, insertion, None);
    }
}

/// `Diff.diff` + `WordDiff.postProcess` 的等价物（`diffWords` 的默认选项）。
///
/// 流水线：两侧分别 `tokenize` → `remove_empty` → `diff_tokens`（trim 相等的 Myers）→
/// `post_process`（空白归属）。输入是任意 UTF-8 字符串，输出有序 `Change` 列表。
///
/// 文本还原性：公共片段 + added 片段精确拼回**新**文本；公共片段的空白取自新文本
/// （见 `build_values`），所以在纯空白差异处，removed 侧不保证逐字节还原旧文本。
/// 这是 jsdiff 的既有行为，`render_diff` 的字级高亮直接依赖它。
///
/// 本函数是所有词级结果的唯一入口，`render_intra_line_diff` 只消费它的输出。
fn diff_words(old_text: &str, new_text: &str) -> Vec<Change> {
    let old_tokens = remove_empty(tokenize(old_text));
    let new_tokens = remove_empty(tokenize(new_text));
    let mut changes = diff_tokens(&old_tokens, &new_tokens);
    post_process(&mut changes);
    changes
}

// 单测分两组：
// * `diff_words_*`：直接锁定 jsdiff 移植结果；其中 `diff_words_golden_fixtures` 读取
//   `tests/fixtures/diff_words.json`（TS 侧生成的黄金样张）；
// * `parse_diff_line_*` / `render_diff_*`：锁定行级解析与着色；
//   `render_diff_golden_fixtures_match_marked_text` 读取 `tests/fixtures/diff_render.json`，
//   用 `mark_inverse` 把反显区间标成 `<<INV>>`/`<</INV>>` 后再与 TS 输出比对。
// 依赖颜色的用例需要先 `kiana_theme::init_theme`，因为 `fg`/`inverse` 读全局主题。
#[cfg(test)]
mod tests {
    use super::*;

    fn added(value: &str) -> Change {
        Change {
            value: value.to_string(),
            added: true,
            removed: false,
        }
    }

    fn removed(value: &str) -> Change {
        Change {
            value: value.to_string(),
            added: false,
            removed: true,
        }
    }

    fn kept(value: &str) -> Change {
        Change {
            value: value.to_string(),
            added: false,
            removed: false,
        }
    }

    #[test]
    fn diff_words_replaces_a_word() {
        assert_eq!(
            diff_words("foo bar", "foo baz"),
            vec![kept("foo "), removed("bar"), added("baz")]
        );
    }

    #[test]
    fn diff_words_deletes_a_word_and_keeps_whitespace() {
        assert_eq!(
            diff_words("foo bar baz", "foo baz"),
            vec![kept("foo "), removed("bar "), kept("baz")]
        );
    }

    #[test]
    fn diff_words_absorbs_pure_whitespace_differences() {
        // 公共片段取**新**文本的 token 拼接结果（jsdiff 的 buildValues 用 newTokens）。
        assert_eq!(diff_words("foo  bar", "foo bar"), vec![kept("foo bar")]);
        assert_eq!(diff_words("a\tb", "a b"), vec![kept("a b")]);
    }

    #[test]
    fn diff_words_handles_identity_and_empty_inputs() {
        assert_eq!(diff_words("", ""), Vec::<Change>::new());
        assert_eq!(
            diff_words("same text", "same text"),
            vec![kept("same text")]
        );
        assert_eq!(diff_words("", "new"), vec![added("new")]);
        assert_eq!(diff_words("old", ""), vec![removed("old")]);
    }

    #[test]
    fn diff_words_splits_cjk_per_character() {
        assert_eq!(
            diff_words("你好世界", "你好 Earth"),
            vec![kept("你好"), removed("世界"), added(" Earth")]
        );
    }

    #[test]
    fn diff_words_multi_word_change() {
        assert_eq!(
            diff_words("the quick brown fox", "the slow brown fox"),
            vec![
                kept("the "),
                removed("quick"),
                added("slow"),
                kept(" brown fox")
            ]
        );
    }

    #[test]
    fn diff_words_golden_fixtures() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/diff_words.json"
        ))
        .expect("read diff_words.json fixture");
        let cases: serde_json::Value = serde_json::from_str(&raw).expect("parse fixture");
        let cases = cases.as_array().expect("fixture is an array");
        assert!(!cases.is_empty(), "fixture must not be empty");

        for case in cases {
            let old = case["old"].as_str().expect("old");
            let new = case["new"].as_str().expect("new");
            let expected: Vec<Change> = case["changes"]
                .as_array()
                .expect("changes")
                .iter()
                .map(|change| Change {
                    value: change["value"].as_str().expect("value").to_string(),
                    added: change["added"].as_bool().unwrap_or(false),
                    removed: change["removed"].as_bool().unwrap_or(false),
                })
                .collect();
            assert_eq!(
                diff_words(old, new),
                expected,
                "diffWords({old:?}, {new:?}) mismatch"
            );
        }
    }

    #[test]
    fn render_diff_golden_fixtures_match_marked_text() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/diff_render.json"
        ))
        .expect("read diff_render.json fixture");
        let cases: serde_json::Value = serde_json::from_str(&raw).expect("parse fixture");
        let cases = cases.as_array().expect("fixture is an array");
        assert!(!cases.is_empty(), "fixture must not be empty");

        kiana_theme::init_theme(Some("dark"), false);
        for case in cases {
            let diff_text = case["diffText"].as_str().expect("diffText");
            let expected = case["markedText"].as_str().expect("markedText");
            let rendered = render_diff(diff_text, None);
            let marked = mark_inverse(&rendered);
            assert_eq!(marked, expected, "renderDiff({diff_text:?}) mismatch");
        }
    }

    /// 把 inverse 区间标成 `<<INV>>`/`<</INV>>`，并去掉其余 SGR。
    fn mark_inverse(text: &str) -> String {
        static SGR: LazyLock<Regex> =
            LazyLock::new(|| Regex::new("\x1b\\[[0-9;]*m").expect("valid SGR regex"));
        let marked = text
            .replace("\x1b[7m", "<<INV>>")
            .replace("\x1b[27m", "<</INV>>");
        SGR.replace_all(&marked, "").into_owned()
    }

    /// 只去掉 SGR（主题的 fg/inverse 只产生 SGR 序列）。
    fn strip_sgr(text: &str) -> String {
        static SGR: LazyLock<Regex> =
            LazyLock::new(|| Regex::new("\x1b\\[[0-9;]*m").expect("valid SGR regex"));
        SGR.replace_all(text, "").into_owned()
    }

    #[test]
    fn parse_diff_line_preserves_captures_verbatim() {
        let parsed = parse_diff_line("+123 hello").expect("matches");
        assert_eq!(parsed.prefix, '+');
        assert_eq!(parsed.line_num, "123");
        assert_eq!(parsed.content, "hello");

        let parsed = parse_diff_line("  abc").expect("matches");
        assert_eq!(parsed.prefix, ' ');
        assert_eq!(parsed.line_num, "");
        assert_eq!(parsed.content, "abc");

        let parsed = parse_diff_line("  12 abc").expect("matches");
        assert_eq!(parsed.prefix, ' ');
        assert_eq!(parsed.line_num, " 12");
        assert_eq!(parsed.content, "abc");

        assert!(parse_diff_line("--- a/file").is_none());
        assert!(parse_diff_line("+++ b/file").is_none());
        assert!(parse_diff_line("+1").is_none());
        assert!(parse_diff_line("\\ No newline at end of file").is_none());
        assert!(parse_diff_line("").is_none());
        // JS 的 `.` 不匹配 `\r`，所以 CRLF 行解析失败。
        assert!(parse_diff_line("+123 hello\r").is_none());
    }

    #[test]
    fn replace_tabs_uses_three_spaces() {
        assert_eq!(replace_tabs("a\tb\tc"), "a   b   c");
    }

    #[test]
    fn render_intra_line_diff_strips_only_the_first_leading_whitespace() {
        kiana_theme::init_theme(Some("dark"), false);
        let (removed_line, added_line) = render_intra_line_diff("  foo bar", "  foo baz");
        let inverse_bar = inverse("bar");
        assert_eq!(
            removed_line,
            format!("  foo {}", inverse_bar),
            "leading whitespace of the first removed part stays uncolored"
        );
        assert!(added_line.contains(&inverse("baz")));
    }

    #[test]
    fn render_diff_single_line_change_uses_intra_line_diff() {
        kiana_theme::init_theme(Some("dark"), false);
        let output = render_diff("@@ -1 +1 @@\n-1 foo bar\n+1 foo baz", None);
        let lines: Vec<&str> = output.split('\n').collect();
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0].starts_with(
                kiana_theme::THEME
                    .get_fg_ansi(ThemeColor::ToolDiffContext)
                    .unwrap()
                    .as_str()
            )
        );
        assert!(lines[1].contains(&inverse("bar")));
        assert!(lines[2].contains(&inverse("baz")));
        assert_eq!(strip_sgr(&output), "@@ -1 +1 @@\n-1 foo bar\n+1 foo baz");
    }

    #[test]
    fn render_diff_multi_line_block_shows_removed_before_added() {
        kiana_theme::init_theme(Some("dark"), false);
        let output = render_diff("-1 a\n-2 b\n+1 c", None);
        assert_eq!(strip_sgr(&output), "-1 a\n-2 b\n+1 c");
        let lines: Vec<&str> = output.split('\n').collect();
        let removed_ansi = kiana_theme::THEME
            .get_fg_ansi(ThemeColor::ToolDiffRemoved)
            .unwrap();
        let added_ansi = kiana_theme::THEME
            .get_fg_ansi(ThemeColor::ToolDiffAdded)
            .unwrap();
        assert!(lines[0].starts_with(&removed_ansi));
        assert!(lines[1].starts_with(&removed_ansi));
        assert!(lines[2].starts_with(&added_ansi));
    }

    #[test]
    fn render_diff_trailing_newline_keeps_an_empty_context_line() {
        kiana_theme::init_theme(Some("dark"), false);
        let output = render_diff("abc\n", None);
        assert_eq!(strip_sgr(&output), "abc\n");
        assert_eq!(output.split('\n').count(), 2);
    }

    #[test]
    fn render_diff_empty_input_renders_one_empty_context_line() {
        kiana_theme::init_theme(Some("dark"), false);
        let output = render_diff("", None);
        assert_eq!(strip_sgr(&output), "");
        assert!(
            !output.is_empty(),
            "theme context still wraps the empty line"
        );
    }
}
