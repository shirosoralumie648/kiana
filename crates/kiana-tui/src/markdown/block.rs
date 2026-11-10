//! block tokens：`marked@18` 的 `Tokenizer.ts` 块级部分（GFM 默认选项）。
//!
//! 逐函数对应 `space` / `code` / `fences` / `heading` / `hr` / `blockquote` / `list` / `html` /
//! `def` / `table` / `lheading` / `paragraph` / `text`，以及 `helpers.ts` 的
//! `rtrim`/`splitCells`/`findClosingBracket`/`expandTabs`/`trimTrailingBlankLines`。
//!
//! # 在 crate 中的位置
//!
//! [`Lexer`] 的块级 tokenizer 集合。调用方是 `crate::markdown::lexer::block_tokens` 的块级
//! 主循环：它把剩余源码按**固定顺序**交给这些方法逐个尝试（LaTeX 块扩展 → space → code →
//! fences → heading → hr → blockquote → list → html → def → table → lheading → paragraph →
//! text），第一个返回 `Some` 的方法胜出；主循环再按返回 token 的 `raw` 长度推进源码
//! （`substring_after_raw`，按 UTF-16 码元计算并夹到末尾）。因此每个方法只要无法从 `src`
//! 开头匹配就返回 `None`——`None` 表示“不是这种块”，不是错误。
//!
//! 行内 token 不在本文件生成：heading / lheading / paragraph / text / table 只调用
//! `Lexer::inline` 登记 `inline_id`，真正的行内扫描推迟到全部块级 token 完成之后
//! （引用式链接允许先使用、后定义）。
//!
//! # 主要流程与不变量
//!
//! - 正则来自 `crate::markdown::lexer` 的 `Rules`（进程内只编译一次）。`fancy_regex` 的运行时
//!   错误（返回 `Err`）在这里统一被 `.ok().flatten()` / `.unwrap_or(false)` 当作“不匹配”，
//!   主循环会继续尝试下一个 tokenizer；代价是正则运行期错误会被静默降级为普通文本
//!   （少数 `expect` 续行路径除外，见下）。
//! - `raw` 必须等于 `src` 的前缀。要把行尾换行留给 space tokenizer 的方法
//!   （heading / hr / lheading / table / def / blockquote）会先 `rtrim(..., '\n')`；
//!   `code` / `html` 用 `trim_trailing_blank_lines` 去掉多余空行；`fences` 原样保留；
//!   `paragraph` 在行间保留 `\n`，但不在结尾吞掉换行；`text` 只匹配单行（`^[^\n]+`）；
//!   两者的 `raw` 都不带结尾换行。
//! - blockquote / list 的 `raw` 是重建出来的，可能比剩余源码长；推进源码时必须走
//!   `substring_after_raw` 的夹取语义，否则会切到字符串之外。
//! - 递归解析（blockquote / list 内部）会临时修改 `Lexer::state.top`：只有 top=true 时主循环
//!   才尝试 `paragraph`。blockquote 每段解析后立即恢复原值；列表项解析结束时主循环
//!   （`block_tokens`）会把 top 置回 true。
//!
//! # 失败模式
//!
//! - `list` 的条目正则由 bullet 动态拼出，编译失败直接 panic；bullet 的取值受
//!   `Rules::list` 约束，用户输入无法让它变成非法正则，因此这类 panic 只可能来自代码改动。
//! - 续行重解析（blockquote 的 blockquote/list 两个分支）用 `expect` 断言成功：
//!   输入以已经匹配过的 `raw` 开头，正常情况下必然再次匹配；若正则运行期报错导致 `None`，
//!   这里会 panic。
//! - 其余辅助函数假定输入来自上方的正则捕获，不对手工构造的畸形输入做越界保护。
//!
//! # 阅读顺序
//!
//! 先读 `crate::markdown::lexer::block_tokens` 主循环，再按本文件顺序看简单的 tokenizer；
//! 重点是 `blockquote`（引用 + 懒续行合并）与 `list`（缩进、松散列表、任务项）。

use fancy_regex::Regex;

use super::lexer::{Lexer, byte_at_utf16, slice_utf16, substring_after_raw, utf16_len};
use super::{ListItem, ListToken, TableAlign, TableCell, TableToken, Token};
use crate::text::{trim_end_js, trim_js};

/// `rtrim(str, c)`：删掉结尾连续的 `c` 字符。
///
/// 返回原字符串的借用切片（不分配新字符串）；全部字符都是 `c` 时返回空串。只按字符相等
/// 判断，不做大小写或全半角归一化。块级 tokenizer 用它去掉 `raw` 末尾的换行，让换行留给
/// 下一个 space token（`inline.rs` 也用它数结尾反斜杠的个数）。
pub(crate) fn rtrim(text: &str, c: char) -> &str {
    let mut end = text.len();
    for (index, ch) in text.char_indices().rev() {
        if ch == c {
            end = index;
        } else {
            break;
        }
    }
    &text[..end]
}

/// `trimTrailingBlankLines(str)`：保留一个结尾空行，多于一个时截掉。
///
/// 用于 `code` / `html` 的 `raw`。按 `\n` 分行后从尾部数连续空行（`blank_line` 只认空格和
/// 制表符）：结尾空行不超过 1 个时原样返回 `text`（即最多保留一个 `\n`）；达到 2 个及以上
/// 时丢掉**全部**尾随空行再 `join("\n")`，因此结果末尾没有换行。全是空行时，多行输入
/// 返回空串，单行输入原样返回。
fn trim_trailing_blank_lines(text: &str, rules: &super::lexer::Rules) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut end = lines.len() as isize - 1;
    while end >= 0
        && rules
            .blank_line
            .is_match(lines[end as usize])
            .unwrap_or(false)
    {
        end -= 1;
    }
    if lines.len() as isize - end <= 2 {
        // 保留单个结尾空行
        return text.to_string();
    }
    lines[..(end + 1) as usize].join("\n")
}

/// `expandTabs(line, indent = 0)`：按 4 列制表位展开。
///
/// `indent` 是行首已经占用的列数（列表里传 bullet 的 UTF-16 长度），决定第一个 tab 要补
/// 几个空格：`4 - (col % 4)`。**列数按码点递增 1**（marked 的实现如此），不按显示宽度计算，
/// 因此宽字符、组合字符和 astral 字符都只前进 1 列；`tests/markdown.rs` 的
/// `expand_tabs_counts_code_points` 锁定了这一行为。除 `\t` 外的字符原样保留。
fn expand_tabs(line: &str, indent: usize) -> String {
    let mut col = indent;
    let mut expanded = String::with_capacity(line.len());
    for ch in line.chars() {
        if ch == '\t' {
            let added = 4 - (col % 4);
            expanded.push_str(&" ".repeat(added));
            col += added;
        } else {
            expanded.push(ch);
            // marked 的 `expandTabs` 用 `for (const char of line)`（按码点迭代）并 `col++`。
            col += 1;
        }
    }
    expanded
}

/// `line.search(/[^ ]/)` 的字节下标（前导只可能是 ASCII 空格，故与 UTF-16 下标一致）；无匹配返回 -1。
///
/// 注意它只把 ASCII 空格当作“空白”：tab、全角空格、NBSP 等都会被当成第一个非空格字符。
/// 列表缩进判断依赖这一点（只把 ASCII 空格当空白）。返回 `-1` 表示整行只有空格；
/// 调用方通常用 `>= indent` 比较，需要自己处理 `-1` 不满足条件的情况。
fn search_non_space(line: &str) -> i64 {
    match line.bytes().position(|byte| byte != b' ') {
        Some(index) => index as i64,
        None => -1,
    }
}

/// `line.slice(index)`（JS `slice` 语义，支持负下标）。
///
/// 下标单位是 UTF-16 码元：负值从尾部算，正数越界时夹到字符串末尾，负值越界时夹到 0；
/// 最后通过 `byte_at_utf16` 换成字节下标，因此不会切在多字节字符中间。返回借用切片。
fn js_slice_from(line: &str, index: i64) -> &str {
    let units = utf16_len(line) as i64;
    let start = if index < 0 {
        (units + index).max(0)
    } else {
        index.min(units)
    };
    &line[byte_at_utf16(line, start as usize)..]
}

/// `findClosingBracket(str, "()")`；返回 UTF-16 下标，`-1` 表示未闭合，`-2` 表示开括号过多。
///
/// 扫描规则：反斜杠转义下一个字符（`\(`、`\)` 不计数）；`(` 使层级 +1，`)` 使层级 -1。
/// 某个 `)` 让层级首次降到 -1 时立即返回**它的下标**（即多出来的右括号位置，调用方把它当作
/// 可用的收尾位置）；扫完仍有未闭合的 `(` 返回 -2；否则返回 -1（没有可用的收尾右括号）。
/// 与 JS 版一样按 UTF-16 码元计数，调用方可以直接拿它做 JS 风格切片。
/// `inline.rs` 的 link tokenizer 用 -2 判定“开括号多于闭括号”并放弃该 link。
pub(crate) fn find_closing_bracket(text: &str) -> i64 {
    if !text.contains(')') {
        return -1;
    }
    let mut level = 0i64;
    let mut chars = text.char_indices().peekable();
    while let Some((byte_index, ch)) = chars.next() {
        if ch == '\\' {
            // JS `i++` 跳过下一个码元
            chars.next();
            continue;
        }
        if ch == '(' {
            level += 1;
        } else if ch == ')' {
            level -= 1;
            if level < 0 {
                return utf16_len(&text[..byte_index]) as i64;
            }
        }
    }
    if level > 0 { -2 } else { -1 }
}

/// `splitCells(tableRow, count?)`。
///
/// 步骤与 marked 的 `helpers.splitCells` 一致：
/// 1. 给**未转义**的 `|` 前补一个空格（连续反斜杠为奇数才算转义；每遇到 `|` 重置计数）。
/// 2. 按 `" |"` 切分；这样 `\|` 留在单元格内部，而普通 `|` 成为分隔符。
/// 3. 若第一个/最后一个单元格 trim 后为空则各去掉一个（只各去一个，不是全部）。
/// 4. 给了 `count` 时截断或补空串到恰好 `count` 个（表格数据行用它对齐表头列数）。
/// 5. 每个单元格 `trim_js`，再把 `\|` 还原成 `|`。
///
/// 返回的是已去两端空白、已去转义的单元格文本；表头调用时 `count` 为 `None`。
fn split_cells(table_row: &str, count: Option<usize>) -> Vec<String> {
    // 未转义的 `|` 前补一个空格后再按 ` |` 切分。
    let mut spaced = String::with_capacity(table_row.len() + 8);
    let mut backslashes = 0usize;
    for ch in table_row.chars() {
        if ch == '|' {
            if backslashes % 2 == 1 {
                spaced.push('|');
            } else {
                spaced.push_str(" |");
            }
            backslashes = 0;
            continue;
        }
        if ch == '\\' {
            backslashes += 1;
        } else {
            backslashes = 0;
        }
        spaced.push(ch);
    }

    let mut cells: Vec<String> = spaced.split(" |").map(|cell| cell.to_string()).collect();
    if cells.first().is_some_and(|cell| trim_js(cell).is_empty()) {
        cells.remove(0);
    }
    if cells.last().is_some_and(|cell| trim_js(cell).is_empty()) {
        cells.pop();
    }
    if let Some(count) = count {
        if cells.len() > count {
            cells.truncate(count);
        } else {
            while cells.len() < count {
                cells.push(String::new());
            }
        }
    }
    for cell in &mut cells {
        *cell = trim_js(cell).replace("\\|", "|");
    }
    cells
}

/// `indentCodeCompensation(raw, text, rules)`。
///
/// 围栏代码块的内容缩进补偿：当开围栏本身带缩进时，内容中前导空白不少于该缩进的行也要
/// 去掉同样多的 UTF-16 码元，否则渲染结果会多出一段缩进。
///
/// 约束：`rules.indent_code_compensation` 是 `^(\s+)(?:```)`——**只对反引号围栏生效**，
/// `~~~` 围栏即使带缩进也不会补偿。捕获组 1 是开围栏前的空白，`\s+` 会把它整体吃掉；
/// 正常输入下只有空格。空白不足的行原样保留；用 UTF-16 长度比较和切片，与 TS 的
/// `indentInNode.length` / `slice` 对齐。开围栏无缩进（正则不匹配）时原样返回 `text`。
fn indent_code_compensation(raw: &str, text: &str, rules: &super::lexer::Rules) -> String {
    let Some(cap) = rules.indent_code_compensation.captures(raw).ok().flatten() else {
        return text.to_string();
    };
    let indent_to_code = cap.get(1).map(|matched| matched.as_str()).unwrap_or("");
    let indent_len = utf16_len(indent_to_code);
    let mut out: Vec<String> = Vec::new();
    for node in text.split('\n') {
        let Some(match_indent) = rules.beginning_space.find(node).ok().flatten() else {
            out.push(node.to_string());
            continue;
        };
        if utf16_len(match_indent.as_str()) >= indent_len {
            out.push(slice_utf16(node, indent_len).to_string());
        } else {
            out.push(node.to_string());
        }
    }
    out.join("\n")
}

/// `anyPunctuation` 替换（`\<punct>` → `<punct>`）。
///
/// `regex` 应当是 `rules.any_punctuation`（`\\([\p{P}\p{S}])`），替换串 `$1` 引用第一个捕获组；
/// 传入别的正则会得到不同语义。返回新字符串，不修改入参。fences 的 info string、def 的
/// href/title 以及行内 link 都靠它把反斜杠转义还原。
pub(crate) fn replace_any_punctuation(regex: &Regex, text: &str) -> String {
    regex.replace_all(text, "$1").into_owned()
}

/// `cachedIndentRegex` 的缓存下标：`max(0, min(3, indent - 1))`。
///
/// `Rules` 里 `next_bullet` / `hr_indent` / `fences_begin` / `heading_begin` / `html_begin` /
/// `blockquote_begin` 各预编译了 4 档缩进（0..3 个前导空格）。这里把列表的列缩进映射到档位：
/// 1 → 0、2 → 1、3 → 2、4 及以上 → 3。调用方保证 `indent >= 1`。
fn cached_indent_index(indent: i64) -> usize {
    (indent - 1).clamp(0, 3) as usize
}

impl Lexer {
    // -----------------------------------------------------------------------
    // space
    // -----------------------------------------------------------------------
    /// `space`：一段只含空格/制表符和换行的空白（`rules.newline`）。
    ///
    /// 正则末尾的 `(?:...|$)` 在串尾可以匹配空串，这里显式返回 `None`，避免产生零长度 token。
    /// 调用方（`block_tokens`）在 `raw` 只有一个 UTF-16 码元且前面已有 token 时，会把它折进
    /// 上一个 token 的 `raw`（追加一个 `\n`），而不是单独保留 Space token。
    pub(crate) fn space(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.newline.captures(src).ok().flatten()?;
        let raw = cap.get(0)?.as_str();
        if raw.is_empty() {
            return None;
        }
        Some(Token::Space {
            raw: raw.to_string(),
        })
    }

    // -----------------------------------------------------------------------
    // code（缩进代码块）
    // -----------------------------------------------------------------------
    /// `code`：缩进代码块：每个非空内容行以 4 个空格或 `{0,3}\t` 开头（`rules.block_code`）。
    ///
    /// 与 `fences` 的分工：这里负责缩进 ≥4 列或 tab 缩进的内容，`fences` 只接受 ≤3 个空格
    /// 缩进的围栏。`raw` 先用 `trim_trailing_blank_lines` 去掉多余结尾空行；`text` 再按
    /// 多行模式删掉每行开头的 1–4 个空格或 `{0,3}\t`（`rules.code_remove_indent`）。
    /// `lang` 为 `None`，`indented` 为 `true`。
    pub(crate) fn code(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.block_code.captures(src).ok().flatten()?;
        let raw = trim_trailing_blank_lines(cap.get(0)?.as_str(), rules);
        let text = rules.code_remove_indent.replace_all(&raw, "").into_owned();
        Some(Token::Code {
            raw,
            lang: None,
            text,
            indented: true,
        })
    }

    // -----------------------------------------------------------------------
    // fences
    // -----------------------------------------------------------------------
    /// `fences`：围栏代码块（``` 或 ~~~，至少 3 个）。
    ///
    /// 正则分组：1 = 开围栏（闭围栏以 `\1` 回引用开围栏，至少同样长，其后可再跟若干
    /// 反引号或波浪号），2 = info string，3 = 代码内容（未闭合时一直取到输入末尾）。
    /// `raw` 原样取自正则、不做 `rtrim`；闭围栏后的换行不属于 `raw`，留给 space tokenizer
    /// （若 space 只吃掉一个换行，`block_tokens` 会把它折进本 token 的 `raw`）；只有未闭合
    /// 围栏才可能包含输入末尾的换行。`text` 经 `indent_code_compensation` 去掉与开围栏
    /// 对齐的缩进。`lang` 是 info string 去空白并反转义标点后的结果：没有 info string 时是
    /// `Some("")`，与缩进代码块的 `None` 区分；`indented` 为 `false`。
    pub(crate) fn fences(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.fences.captures(src).ok().flatten()?;
        let raw = cap.get(0)?.as_str().to_string();
        let content = cap.get(3).map(|matched| matched.as_str()).unwrap_or("");
        let text = indent_code_compensation(&raw, content, rules);
        let lang_group = cap.get(2).map(|matched| matched.as_str()).unwrap_or("");
        let lang = if !lang_group.is_empty() {
            replace_any_punctuation(&rules.any_punctuation, trim_js(lang_group))
        } else {
            String::new()
        };
        Some(Token::Code {
            raw,
            lang: Some(lang),
            text,
            indented: false,
        })
    }

    // -----------------------------------------------------------------------
    // heading
    // -----------------------------------------------------------------------
    /// `heading`：ATX 标题（1–6 个 `#`，后接空白或行尾）。
    ///
    /// 处理流程：`text` 先 `trim_js`；若结尾是 `#`，去掉连续的 `#` 后，只有“剩下为空”或
    /// “剩下的部分以空格结尾”时才把 `#` 当作可选的闭合标记并再次 trim（`# foo #` → `foo`，
    /// `# foo#` 保留 `foo#`）。`raw` 去掉行尾换行，`depth` 是 `#` 的 UTF-16 长度，
    /// 行内 token 通过 `inline_id` 延迟生成。
    pub(crate) fn heading(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.heading.captures(src).ok().flatten()?;
        let mut text = trim_js(cap.get(2)?.as_str()).to_string();
        if rules.ending_hash.is_match(&text).unwrap_or(false) {
            let trimmed = rtrim(&text, '#');
            if trimmed.is_empty() || rules.ending_space_char.is_match(trimmed).unwrap_or(false) {
                text = trim_js(trimmed).to_string();
            }
        }
        let raw = rtrim(cap.get(0)?.as_str(), '\n').to_string();
        let depth = utf16_len(cap.get(1)?.as_str());
        let inline_id = self.inline(&text);
        Some(Token::Heading {
            raw,
            depth,
            text,
            inline_id: Some(inline_id),
            tokens: Vec::new(),
        })
    }

    // -----------------------------------------------------------------------
    // hr
    // -----------------------------------------------------------------------
    /// `hr`：水平线（≥3 个 `-`/`_`/`*`，字符间可夹空格/制表符，缩进 ≤3）。
    ///
    /// 只去掉 `raw` 的行尾换行；没有其他字段。列表的条目循环会先用 `hr` 检查，避免把
    /// `- - -` 当成列表项而不是水平线。
    pub(crate) fn hr(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.hr.captures(src).ok().flatten()?;
        let raw = rtrim(cap.get(0)?.as_str(), '\n').to_string();
        Some(Token::Hr { raw })
    }

    // -----------------------------------------------------------------------
    // blockquote
    // -----------------------------------------------------------------------
    /// `blockquote`：引用块（`>` 行及其懒续行）。
    ///
    /// 对应 marked 的 `Tokenizer.blockquote`。单个正则只能圈出“可能属于引用”的源码，其中还
    /// 混有懒续行（不带 `>` 的行），因此这里做多轮解析：
    /// 1. 把匹配到的 `raw` 按 `\n` 拆行，每轮取出一段：段可以以非引用行开头，之后收下引用
    ///    行；一旦已经进入引用段又遇到非引用行就停下，把该行留给下一轮。去掉引用行的
    ///    `> ` 前缀后得到 `current_text`。
    /// 2. 把 `Lexer::state.top` 临时置为 true，用块级 tokenizer 递归解析该段，随后恢复。
    /// 3. 查看最后一个子 token：`code` → 停止；嵌套 `blockquote` → 把剩余行合并回旧 raw
    ///    后整体重解析并替换；`list` → 把剩余行当作列表懒续行重解析、替换并继续；其他
    ///    类型 → 继续处理下一段。
    ///
    /// 不变量与边界：`raw`/`text` 是逐段拼接出来的，长度可能超过原始 `src`（调用方按
    /// `substring_after_raw` 夹取）；子 token 的 `raw` 与段文本长度可能不同，截断一律用
    /// `saturating_sub` 防止下溢。续行重解析用 `expect` 断言必须成功；若正则运行期报错
    /// 导致返回 `None` 会 panic。`unreachable!` 分支只依赖 `type_name()` 与枚举变体一致
    /// 这一不变量。
    pub(crate) fn blockquote(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.blockquote.captures(src).ok().flatten()?;
        let raw_text = rtrim(cap.get(0)?.as_str(), '\n').to_string();
        let mut lines: Vec<String> = raw_text.split('\n').map(|line| line.to_string()).collect();
        let mut raw = String::new();
        let mut text = String::new();
        let mut tokens: Vec<Token> = Vec::new();

        // 每轮从 lines 开头取一段：非引用行只有在还没遇到引用行时才被收下；已经进入引用段
        // 后再遇到非引用行就停下，把它留给下一轮（续行合并时会产生这种剩余行）。
        while !lines.is_empty() {
            let mut in_blockquote = false;
            let mut current_lines: Vec<String> = Vec::new();
            let mut i = 0usize;
            while i < lines.len() {
                if rules.blockquote_start.is_match(&lines[i]).unwrap_or(false) {
                    current_lines.push(lines[i].clone());
                    in_blockquote = true;
                } else if !in_blockquote {
                    current_lines.push(lines[i].clone());
                } else {
                    break;
                }
                i += 1;
            }
            lines = lines.split_off(i);

            let current_raw = current_lines.join("\n");
            // 去引用前缀分两步：先把未被引用的懒续行里形如 `---`/`===` 的下划线缩进 4 格，
            // 避免递归解析时被当成 setext 标题；再删掉引用行开头的 `> `。
            let current_text = rules
                .blockquote_setext_replace2
                .replace_all(
                    &rules
                        .blockquote_setext_replace
                        .replace_all(&current_raw, "\n    $1"),
                    "",
                )
                .into_owned();
            raw = if raw.is_empty() {
                current_raw
            } else {
                format!("{raw}\n{current_raw}")
            };
            text = if text.is_empty() {
                current_text.clone()
            } else {
                format!("{text}\n{current_text}")
            };

            // 引用行按顶层 token 解析（续行时合并段落）。
            // state.top 临时置 true：只有 top=true 时 block_tokens 才尝试 paragraph，
            // 其余 tokenizer 不读这个状态；解析完立即恢复，避免影响外层调用。
            let top = self.state.top;
            self.state.top = true;
            self.block_tokens(&current_text, &mut tokens, true);
            self.state.top = top;

            if lines.is_empty() {
                break;
            }

            // 最后一个子 token 决定剩余行怎么处理：嵌套引用/列表可以继续吸收懒续行，
            // 代码块则必须停止。
            let last_type = tokens.last().map(Token::type_name);
            match last_type {
                // 代码块之后的剩余行不并入本 blockquote 的 raw/text，直接结束，
                // 由调用方（block_tokens）从剩余源码继续解析。
                Some("code") => break,
                Some("blockquote") => {
                    // 剩余行是嵌套引用的懒续行：把旧 raw 与剩余行拼起来，重新解析整个嵌套
                    // 引用，再用新 token 替换旧的。
                    let (old_raw, old_text) = match tokens.last() {
                        Some(Token::Blockquote { raw, text, .. }) => (raw.clone(), text.clone()),
                        _ => unreachable!(),
                    };
                    let continuation = lines.join("\n");
                    let new_text = format!(
                        "{old_raw}\n{}",
                        rules
                            .blockquote_setext_replace2
                            .replace_all(&continuation, "")
                    );
                    let new_token = self
                        .blockquote(&new_text)
                        .expect("blockquote continuation parses");
                    let new_text_value = match &new_token {
                        Token::Blockquote { text, .. } => text.clone(),
                        _ => unreachable!(),
                    };
                    // 用新 token 替换旧 token；raw 直接追加 "\n" + continuation（不截断、不用
                    // 新 token 的 raw）；text 按旧 text 长度截掉尾部后接上新 token 的 text。
                    *tokens.last_mut().expect("non-empty") = new_token;
                    raw = format!("{raw}\n{continuation}");
                    let keep = text.len().saturating_sub(old_text.len());
                    text.truncate(keep);
                    text.push_str(&new_text_value);
                    break;
                }
                Some("list") => {
                    // 列表懒续行：把剩余行接到旧 list.raw 之后重新解析列表。list() 返回前会
                    // 对 raw 做 trim_end_js，新 raw 不一定覆盖整个重解析输入，所以剩余行要
                    // 从“重解析输入 − 新 raw”重新计算，而不是简单丢弃 lines。
                    let old_raw = match tokens.last() {
                        Some(Token::List(list)) => list.raw.clone(),
                        _ => unreachable!(),
                    };
                    let new_text = format!("{old_raw}\n{}", lines.join("\n"));
                    let new_token = self.list(&new_text).expect("list continuation parses");
                    let new_raw = new_token.raw().to_string();
                    *tokens.last_mut().expect("non-empty") = new_token;
                    let keep_raw = raw.len().saturating_sub(old_raw.len());
                    raw.truncate(keep_raw);
                    raw.push_str(&new_raw);
                    // text 的截断用的是旧 list.raw 的长度，不是旧 list.text 的长度。
                    let keep_text = text.len().saturating_sub(old_raw.len());
                    text.truncate(keep_text);
                    text.push_str(&new_raw);
                    lines = substring_after_raw(&new_text, &new_raw)
                        .split('\n')
                        .map(|line| line.to_string())
                        .collect();
                    continue;
                }
                _ => {}
            }
        }

        Some(Token::Blockquote { raw, text, tokens })
    }

    // -----------------------------------------------------------------------
    // list
    // -----------------------------------------------------------------------
    /// `list`：列表（有序/无序，含懒续行、松散列表与 GFM 任务项）。
    ///
    /// 对应 marked 的 `Tokenizer.list`。分四个阶段，顺序不可交换：
    /// 1. **切条目**：用按 bullet 动态拼出的 `item_regex` 反复从 `source` 开头取 item；
    ///    `hr` 命中时结束（`- - -` 是水平线而不是条目）。有序列表的 bullet 正则只固定
    ///    “1 到 9 位数字 + 分隔符”，同一列表内数字可以不同。
    /// 2. **收集内容**：用首行内容的缩进算出 `indent`（列数），续行缩进够就剥掉 `indent`
    ///    列并入当前 item；遇到更浅的 bullet、围栏、标题、HTML、引用或水平线则结束 item。
    /// 3. **行内延迟与松散判定**：每个 item 的 `text` 用块级 tokenizer 递归解析（调用前把
    ///    `state.top` 置 false，因此普通行通常成为 `text` token；block_tokens 只在 top=true
    ///    时才尝试 paragraph）；item 之间出现空行，或某个 item 内出现含空行的 Space
    ///    token，就判为 loose。
    /// 4. **任务项**：把 `[ ]`/`[x]` 前缀从 text/raw 与对应的行内队列源中删除，插入
    ///    [`Token::Checkbox`]；loose 列表随后把块级 `text` 改名为 `paragraph` 并给所有
    ///    item 标 loose。
    ///
    /// 约束与边界：
    /// - `indent` 以“列”计：首行 marker（含最多 3 个前导空格）的 UTF-16 长度 + 内容缩进；
    ///   内容起始列 >4 时按 1 处理。续行比较前把 tab 全换成 4 个空格（不是 tab stop），
    ///   首行则用 `expand_tabs` 按 tab stop 展开。
    /// - 只有最后一个 item 的 `raw`/`text` 与整个 `list.raw` 做 `trim_end_js`。
    /// - 一个条目都没匹配到时返回 `None`（主循环继续尝试 html/def/…）。
    /// - 任务前缀用 `replacen(..., 1)` 删除，每处只删一次。
    /// - 第一遍已逐个 item 做过块级解析（非空 text 的行内源已登记）；任务阶段是“从队尾
    ///   向前找第一个仍带任务标记的 src”，并不与当前 item 严格对应；它依赖每个任务 item
    ///   恰好登记一次带标记的 src（回归测试见 `tests/markdown.rs` 的
    ///   `loose_task_item_checkbox_survives_inline_queue_repro`）。
    /// - 条目正则编译失败会 panic；bullet 的两处 `expect` 依赖 `rules.list` 保证分组 1
    ///   非空。
    pub(crate) fn list(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.list.captures(src).ok().flatten()?;
        let mut bull = trim_js(cap.get(1)?.as_str()).to_string();
        let is_ordered = utf16_len(&bull) > 1;
        let start_value: Option<u64> = if is_ordered {
            let last_len = bull.chars().next_back().map(char::len_utf8).unwrap_or(1);
            bull.get(..bull.len() - last_len)
                .and_then(|digits| digits.parse::<u64>().ok())
        } else {
            None
        };
        let mut list = ListToken {
            raw: String::new(),
            ordered: is_ordered,
            start: start_value,
            loose: false,
            items: Vec::new(),
        };

        // marked：有序列表为 `\d{1,9}\` + 最后一个字符，无序为 `\` + 首字符（不再做正则转义）
        // 拼出本列表专用的条目正则：group 1 = 前导 ≤3 空格 + bullet，group 2 = 行余下部分
        // 加行尾换行。分隔符/符号直接塞进正则，取值只有 * + - . )，无需额外转义。
        bull = if is_ordered {
            format!(r"\d{{1,9}}\{}", bull.chars().next_back().expect("bullet"))
        } else {
            format!("\\{}", bull.chars().next().expect("bullet"))
        };
        let item_pattern = format!(r"^( {{0,3}}{bull})((?:[\t ][^\n]*)?(?:\n|$))");
        let item_regex = match Regex::new(&super::lexer::translate_pattern(&item_pattern)) {
            Ok(regex) => regex,
            Err(err) => panic!("list item regex failed: {err}\n{item_pattern}"),
        };

        let mut source = src.to_string();
        let mut ends_with_blank_line = false;

        // 逐条切 item：source 是尚未消费的源码，每次匹配都必须从它的开头开始。
        while !source.is_empty() {
            let mut end_early = false;
            let Some(cap) = item_regex.captures(&source).ok().flatten() else {
                break;
            };
            // 水平线优先于条目：`- - -` 之类即使能被 bullet 正则匹配，也应结束整个列表，
            // 交给外层的 hr tokenizer。
            if rules.hr.is_match(&source).unwrap_or(false) {
                break;
            }

            let cap0 = cap.get(0)?.as_str().to_string();
            let cap1 = cap.get(1)?.as_str().to_string();
            let cap2 = cap.get(2)?.as_str().to_string();
            let mut raw = cap0.clone();
            source = source[cap0.len()..].to_string();

            let bullet_units = utf16_len(&cap1);
            let first_line = cap2.split('\n').next().unwrap_or("");
            let mut line = expand_tabs(first_line, bullet_units);
            let next_line = source.split('\n').next().unwrap_or("").to_string();
            let mut blank_line = trim_js(&line).is_empty();

            let mut indent: i64;
            let mut item_contents = String::new();
            // indent 是续行要剥掉的列数：
            // - 空条目：marker 长度 + 1（内容视为从下一列开始）；
            // - 非空条目：首个非空格的位置（起始列 >4 时按 1），再加 marker 长度。
            if blank_line {
                indent = bullet_units as i64 + 1;
            } else {
                indent = search_non_space(&line);
                indent = if indent > 4 { 1 } else { indent };
                item_contents = js_slice_from(&line, indent).to_string();
                indent += bullet_units as i64;
            }

            if blank_line && rules.blank_line.is_match(&next_line).unwrap_or(false) {
                // 每个 item 开头最多吸收一个空行
                // end_early=true 表示这一条 item 到此为止，下面的续行收集整个跳过。
                raw.push_str(&next_line);
                raw.push('\n');
                let consume = next_line.len() + 1;
                source = source.get(consume..).unwrap_or("").to_string();
                end_early = true;
            }

            if !end_early {
                // 按缩进档取预编译的“行首检查”正则（0..3 档）。这些正则用原始行（tab 未
                // 展开）匹配，缩进超过档位上限的 bullet/围栏等不会被当成新块的开始。
                let indent_index = cached_indent_index(indent);
                let next_bullet_regex = &rules.next_bullet[indent_index];
                let hr_regex = &rules.hr_indent[indent_index];
                let fences_begin_regex = &rules.fences_begin[indent_index];
                let heading_begin_regex = &rules.heading_begin[indent_index];
                let html_begin_regex = &rules.html_begin[indent_index];
                let blockquote_begin_regex = &rules.blockquote_begin[indent_index];

                // 收集当前 item 的续行：每轮先看行首是否开启新的块（围栏/标题/HTML/引用/
                // 更浅的 bullet/水平线），命中就结束 item 且不消费该行；否则按缩进决定是
                // 继续并入本 item，还是作为懒续行原样接上。
                while !source.is_empty() {
                    let raw_line = source.split('\n').next().unwrap_or("").to_string();
                    let next_line = raw_line.clone();
                    let next_line_without_tabs = rules
                        .tab_char_global
                        .replace_all(&next_line, "    ")
                        .into_owned();

                    // 六个“下一行是不是新块”的检查。命中任一就结束当前 item，该行留给
                    // 下一轮条目匹配（如果是更浅的 bullet）或外层 tokenizer。
                    if fences_begin_regex.is_match(&next_line).unwrap_or(false) {
                        break;
                    }
                    if heading_begin_regex.is_match(&next_line).unwrap_or(false) {
                        break;
                    }
                    if html_begin_regex.is_match(&next_line).unwrap_or(false) {
                        break;
                    }
                    if blockquote_begin_regex.is_match(&next_line).unwrap_or(false) {
                        break;
                    }
                    if next_bullet_regex.is_match(&next_line).unwrap_or(false) {
                        break;
                    }
                    if hr_regex.is_match(&next_line).unwrap_or(false) {
                        break;
                    }

                    // tab 展开后计算缩进；缩进足够或整行为空时并入本 item。
                    let search = search_non_space(&next_line_without_tabs);
                    if search >= indent || trim_js(&next_line).is_empty() {
                        // 缩进足够，归入当前 item
                        item_contents.push('\n');
                        item_contents.push_str(js_slice_from(&next_line_without_tabs, indent));
                    } else {
                        // 缩进不足：上一行是空行、缩进代码块、围栏、标题或水平线时结束
                        // item；否则这一行是懒续行，原样（保留 tab）接到内容后面。
                        if blank_line {
                            break;
                        }
                        // 上一行是缩进代码块时结束 item
                        if search_non_space(&rules.tab_char_global.replace_all(&line, "    ")) >= 4
                        {
                            break;
                        }
                        if fences_begin_regex.is_match(&line).unwrap_or(false) {
                            break;
                        }
                        if heading_begin_regex.is_match(&line).unwrap_or(false) {
                            break;
                        }
                        if hr_regex.is_match(&line).unwrap_or(false) {
                            break;
                        }
                        item_contents.push('\n');
                        item_contents.push_str(&next_line);
                    }

                    // 该行已被消费：推进 source，并把当前行记作下一轮判断“上一行形态”的
                    // line（已按缩进剥过）。
                    blank_line = trim_js(&next_line).is_empty();
                    raw.push_str(&raw_line);
                    raw.push('\n');
                    let consume = raw_line.len() + 1;
                    source = source.get(consume..).unwrap_or("").to_string();
                    line = js_slice_from(&next_line_without_tabs, indent).to_string();
                }
            }

            // 松散列表判定（第一处）：item 的 raw 以空行结尾时先记下，等下一个 item
            // 出现再置 loose；double_blank_line 匹配 raw 末尾的“空行”（两个换行之间
            // 只有空格/制表符）。
            if !list.loose {
                if ends_with_blank_line {
                    // 上一个 item 以空行结尾 → 松散列表
                    list.loose = true;
                } else if rules.double_blank_line.is_match(&raw).unwrap_or(false) {
                    ends_with_blank_line = true;
                }
            }

            list.items.push(ListItem {
                raw: raw.clone(),
                task: rules.list_is_task.is_match(&item_contents).unwrap_or(false),
                checked: None,
                loose: false,
                text: item_contents,
                tokens: Vec::new(),
            });
            list.raw.push_str(&raw);
        }

        // 防御性返回：hr 在首个条目之前命中（正常从 block_tokens 调用时不会到达，因为
        // hr 已先被尝试）时这里拿不到任何条目，返回 None；若发生，block_tokens 会继续尝试
        // html/def/…。
        let last = list.items.last_mut()?;
        // 只修剪最后一个 item 与整个 list 的尾部空白（marked 的行为；其余 item 原样保留）。
        last.raw = trim_end_js(&last.raw).to_string();
        last.text = trim_end_js(&last.text).to_string();
        list.raw = trim_end_js(&list.raw).to_string();

        // 第一遍：token 化每个 item；spacer token 决定 loose。
        // 调用开始时 state.top=false（block_tokens 只在 top=true 时尝试 paragraph），
        // 普通行通常成为 text token；嵌套调用可能把 top 改回 true（见 block_tokens 末尾）。
        // 这次调用同时把每个 item 的行内源登记进 inline_queue。
        for item in &mut list.items {
            self.state.top = false;
            let mut item_tokens: Vec<Token> = Vec::new();
            self.block_tokens(&item.text, &mut item_tokens, false);
            item.tokens = item_tokens;
            if !list.loose {
                // 松散判定（第二处）：item 内出现 Space token 且其 raw 至少含两个换行
                // （any_line）→ 列表松散。已经判定 loose 后不再检查。
                let spacers: Vec<&Token> = item
                    .tokens
                    .iter()
                    .filter(|token| matches!(token, Token::Space { .. }))
                    .collect();
                let has_multiple_line_breaks = !spacers.is_empty()
                    && spacers
                        .iter()
                        .any(|token| rules.any_line.is_match(token.raw()).unwrap_or(false));
                list.loose = has_multiple_line_breaks;
            }
        }

        // 第二遍：放入任务复选框。
        // item.task 在收集阶段由 list_is_task（^\[[ xX]\] +\S）标记；只有首个 token 是
        // Text/Paragraph（且稍后能在 raw 中找到复选框原文）才插入复选框，否则下面的
        // else if 会取消 task 标记。
        for item in &mut list.items {
            let first_is_text_like = matches!(
                item.tokens.first(),
                Some(Token::Text { .. }) | Some(Token::Paragraph { .. })
            );
            if item.task && first_is_text_like {
                // 从 item.text 与首个 text 类 token 的 raw/text 中各删一次 "[ ] "/"[x] "
                // 前缀。
                item.text = rules
                    .list_replace_task
                    .replacen(&item.text, 1, "")
                    .into_owned();
                if let Some(Token::Text { raw, text, .. } | Token::Paragraph { raw, text, .. }) =
                    item.tokens.first_mut()
                {
                    *raw = rules.list_replace_task.replacen(raw, 1, "").into_owned();
                    *text = rules.list_replace_task.replacen(text, 1, "").into_owned();
                }
                // 行内 token 延迟生成：第一遍登记队列时 src 还带着任务前缀，这里必须同步
                // 删掉，否则渲染时前缀会重复。注意第一遍已逐个 item 做过块级解析并登记
                // 行内源，所以这是“从队尾找第一个仍带任务标记的 src”，不是按当前 item
                // 精确对应；它依赖每个任务 item 恰好登记一次带标记的 src。
                for index in (0..self.inline_queue.len()).rev() {
                    if rules
                        .list_is_task
                        .is_match(&self.inline_queue[index].src)
                        .unwrap_or(false)
                    {
                        let replaced = rules
                            .list_replace_task
                            .replacen(&self.inline_queue[index].src, 1, "")
                            .into_owned();
                        self.inline_queue[index].src = replaced;
                        break;
                    }
                }

                // 复选框原文在 item.raw 中查找（不锚定，取第一处）；不是 "[ ]" 即视为
                // 已勾选（[x]/[X]）。
                let task_raw = rules
                    .list_task_checkbox
                    .find(&item.raw)
                    .ok()
                    .flatten()
                    .map(|matched| matched.as_str().to_string());
                if let Some(task_raw) = task_raw {
                    let checked = task_raw != "[ ]";
                    let checkbox_raw = format!("{task_raw} ");
                    item.checked = Some(checked);
                    // 松散列表：复选框要成为首个 Paragraph/Text 的 raw/text 前缀，并插进它
                    // 的 tokens 头部——这样行内回填（append）后仍然是 checkbox 在前。
                    if list.loose {
                        let has_tokens_field = matches!(
                            item.tokens.first(),
                            Some(Token::Paragraph { .. }) | Some(Token::Text { .. })
                        );
                        if has_tokens_field {
                            if let Some(
                                Token::Text {
                                    raw, text, tokens, ..
                                }
                                | Token::Paragraph {
                                    raw, text, tokens, ..
                                },
                            ) = item.tokens.first_mut()
                            {
                                *raw = format!("{checkbox_raw}{raw}");
                                *text = format!("{checkbox_raw}{text}");
                                tokens.insert(
                                    0,
                                    Token::Checkbox {
                                        raw: checkbox_raw.clone(),
                                        checked,
                                    },
                                );
                            }
                        } else {
                            item.tokens.insert(
                                0,
                                Token::Paragraph {
                                    raw: checkbox_raw.clone(),
                                    text: checkbox_raw.clone(),
                                    inline_id: None,
                                    tokens: vec![Token::Checkbox {
                                        raw: checkbox_raw,
                                        checked,
                                    }],
                                },
                            );
                        }
                    } else {
                        item.tokens.insert(
                            0,
                            Token::Checkbox {
                                raw: checkbox_raw,
                                checked,
                            },
                        );
                    }
                }
            } else if item.task {
                // 首个 token 不是 Text/Paragraph：取消 task 标记，不插入 Checkbox token。
                item.task = false;
            }
        }

        // 松散列表的所有 item 都标记 loose，并把块级 text 改名为 paragraph。
        if list.loose {
            for item in &mut list.items {
                item.loose = true;
                for token in &mut item.tokens {
                    if let Token::Text {
                        raw,
                        text,
                        inline_id,
                        tokens,
                        ..
                    } = token
                    {
                        *token = Token::Paragraph {
                            raw: std::mem::take(raw),
                            text: std::mem::take(text),
                            inline_id: inline_id.take(),
                            tokens: std::mem::take(tokens),
                        };
                    }
                }
            }
        }

        Some(Token::List(list))
    }

    // -----------------------------------------------------------------------
    // html（块级）
    // -----------------------------------------------------------------------
    /// `html`：块级 HTML（标签、注释、CDATA、`<?...?>`、`<!...>` 等，规则见 `rules.html`）。
    ///
    /// `raw` 用 `trim_trailing_blank_lines` 去掉多余结尾空行，并原样作为 `text`；
    /// `block` 恒为 true。`pre` 只在正则的分组 1 捕获到 `pre` / `script` / `style` 时为 true
    /// （`textarea` 虽然也参与同一分支的匹配，但这里不算 `pre`）。`in_link` / `in_raw_block`
    /// 对块级 HTML 恒为 false，它们是行内 HTML tokenizer 才使用的状态。
    pub(crate) fn html(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.html.captures(src).ok().flatten()?;
        let pre_tag = cap.get(1).map(|matched| matched.as_str()).unwrap_or("");
        let raw = trim_trailing_blank_lines(cap.get(0)?.as_str(), rules);
        let pre = matches!(pre_tag, "pre" | "script" | "style");
        Some(Token::Html {
            raw: raw.clone(),
            block: true,
            pre,
            text: raw,
            in_link: false,
            in_raw_block: false,
        })
    }

    // -----------------------------------------------------------------------
    // def（链接定义）
    // -----------------------------------------------------------------------
    /// `definition`：链接定义 `[label]: href "title"`（对应 TS 的 `def` tokenizer）。
    ///
    /// 字段处理：
    /// - `tag`：label 转小写，再把连续的 JS 空白折叠成一个空格，作为后续引用链接的查找键；
    /// - `href`：先去掉可选的尖括号包裹（`rules.href_brackets`），再还原 `\<标点>` 转义；
    /// - `title`：去掉最外层的引号或圆括号，再还原转义；没有 title 时为 `None`；
    /// - `raw`：去掉行尾换行。
    ///
    /// 注意：是否把 [`Token::Def`] 放进 token 树由调用方决定——同名 `tag` 已定义时 token
    /// 会被丢弃（源码仍然照常消费），见 `crate::markdown::lexer::block_tokens` 的 def 分支。
    pub(crate) fn definition(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.def.captures(src).ok().flatten()?;
        let label = cap.get(1)?.as_str();
        // tag 是查找键：label 先转小写，再把连续的 JS 空白折叠成单个空格。
        let tag = rules
            .multiple_space_global
            .replace_all(&label.to_lowercase(), " ")
            .into_owned();
        let href_group = cap.get(2).map(|matched| matched.as_str()).unwrap_or("");
        // href 可能被尖括号包住：先剥掉一层 <...>，再还原反斜杠标点转义。
        let href = if !href_group.is_empty() {
            let stripped = rules.href_brackets.replace(href_group, "$1").into_owned();
            replace_any_punctuation(&rules.any_punctuation, &stripped)
        } else {
            String::new()
        };
        // title 去掉最外层引号/括号（TITLE 正则保证至少两个字符）再还原转义。
        let title = cap.get(3).map(|matched| {
            let value = matched.as_str();
            let inner = &value[1..value.len().saturating_sub(1)];
            replace_any_punctuation(&rules.any_punctuation, inner)
        });
        let raw = rtrim(cap.get(0)?.as_str(), '\n').to_string();
        Some(Token::Def {
            raw,
            tag,
            href,
            title,
        })
    }

    // -----------------------------------------------------------------------
    // table
    // -----------------------------------------------------------------------
    /// `table`：GFM 表格（表头行 + 分隔行 + 可选数据行）。
    ///
    /// 正则分组：1 = 表头行，2 = 分隔行，3 = 数据行。以下两种情况返回 `None`：
    /// - 分隔行既不含 `|` 也不含 `:`（`rules.table_delimiter`）——这种“标题 + `---`”其实
    ///   是 setext 标题，主循环随后会尝试 `lheading`；
    /// - 表头与分隔行的列数不一致。数据行允许列数不同：之后按表头列数截断或补空。
    ///
    /// `align` 由分隔行每列的 `-:` / `:-:` / `:-` 判定；表头/单元格的 `text` 保留
    /// `split_cells` 处理后的文本，行内 token 通过 `inline_id` 延迟回填——回填时表格单元格
    /// 是**覆盖** `tokens`，与 heading/paragraph 的追加不同（见 `lexer_fill_inline_tokens`）。
    pub(crate) fn table(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.table.captures(src).ok().flatten()?;
        let cap1 = cap.get(1)?.as_str();
        let cap2 = cap.get(2)?.as_str();
        let cap3 = cap.get(3).map(|matched| matched.as_str());

        if !rules.table_delimiter.is_match(cap2).unwrap_or(false) {
            // 分隔行必须含 `|` 或 `:`，否则是 setext 标题
            return None;
        }

        let headers = split_cells(cap1, None);
        // 去掉分隔行首尾的 "|"，再按 "|" 取每列的对齐标记（如 "-:"、":-:"、"---"）。
        let aligns: Vec<String> = rules
            .table_align_chars
            .replace_all(cap2, "")
            .split('|')
            .map(|value| value.to_string())
            .collect();
        // 数据行：去掉末尾空行后按行切分；每行的单元格稍后按表头列数补齐/截断。
        let rows_raw: Vec<String> = match cap3 {
            Some(value) if !trim_js(value).is_empty() => rules
                .table_row_blank_line
                .replace_all(value, "")
                .split('\n')
                .map(|row| row.to_string())
                .collect(),
            _ => Vec::new(),
        };

        let mut item = TableToken::empty(rtrim(cap.get(0)?.as_str(), '\n').to_string());

        if headers.len() != aligns.len() {
            // 表头与对齐列数必须一致；行可以不同。
            return None;
        }

        for align in &aligns {
            item.align.push(
                if rules.table_align_right.is_match(align).unwrap_or(false) {
                    TableAlign::Right
                } else if rules.table_align_center.is_match(align).unwrap_or(false) {
                    TableAlign::Center
                } else if rules.table_align_left.is_match(align).unwrap_or(false) {
                    TableAlign::Left
                } else {
                    TableAlign::None
                },
            );
        }

        // 表头单元格登记行内队列 id；真正的行内 token 在全部块级 token 完成后回填。
        for (index, header) in headers.iter().enumerate() {
            let inline_id = self.inline(header);
            item.header.push(TableCell {
                text: header.clone(),
                inline_id: Some(inline_id),
                tokens: Vec::new(),
                header: true,
                align: item.align[index],
            });
        }

        let header_len = item.header.len();
        // 数据行按表头列数补齐/截断，因此每行的单元格数都等于 header_len；
        // 对齐方式按下标取。
        for row in rows_raw {
            let mut cells: Vec<TableCell> = Vec::new();
            for (index, cell) in split_cells(&row, Some(header_len)).iter().enumerate() {
                let inline_id = self.inline(cell);
                cells.push(TableCell {
                    text: cell.clone(),
                    inline_id: Some(inline_id),
                    tokens: Vec::new(),
                    header: false,
                    align: item.align.get(index).copied().unwrap_or(TableAlign::None),
                });
            }
            item.rows.push(cells);
        }

        Some(Token::Table(item))
    }

    // -----------------------------------------------------------------------
    // lheading（setext 标题）
    // -----------------------------------------------------------------------
    /// `lheading`：setext 标题（正文的下一行是 `===` 或 `---`）。
    ///
    /// 分组 1 = 标题正文，2 = 下划线。`text` 去两端 JS 空白；`depth` 只由下划线首字符决定：
    /// `=` → 1，`-` → 2（长度不限，下划线末尾允许空格）。`raw` 去掉行尾换行，下划线保留在
    /// `raw` 内。主循环在 `table` 之后才尝试它，因此“标题行 + `---`”这类输入会落到这里
    /// 成为 h2：分隔行不含 `|`/`:` 时，`table` 在 `rules.table_delimiter` 检查处就返回
    /// None（早于表头与分隔行的列数比较）。
    pub(crate) fn lheading(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.lheading.captures(src).ok().flatten()?;
        let text = trim_js(cap.get(1)?.as_str()).to_string();
        let underline = cap.get(2)?.as_str();
        let depth = if underline.starts_with('=') { 1 } else { 2 };
        let raw = rtrim(cap.get(0)?.as_str(), '\n').to_string();
        let inline_id = self.inline(&text);
        Some(Token::Heading {
            raw,
            depth,
            text,
            inline_id: Some(inline_id),
            tokens: Vec::new(),
        })
    }

    // -----------------------------------------------------------------------
    // paragraph
    // -----------------------------------------------------------------------
    /// `paragraph`：顶层段落（只有 `Lexer::state.top` 为真时 `block_tokens` 才会调用它）。
    ///
    /// 分组 1 是段落正文，行与行之间保留单个 `\n`；`text` 在末尾是换行时去掉一个——本实现的
    /// 段落规则以 `[^\n]+` 结尾，正常不会带入换行，这一判断是为了与 marked 的写法保持一致。
    /// `raw` 原样取自匹配结果，行内 token 通过 `inline_id` 延迟生成。
    pub(crate) fn paragraph(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.paragraph.captures(src).ok().flatten()?;
        let group = cap.get(1)?.as_str();
        let text = group.strip_suffix('\n').unwrap_or(group);
        let raw = cap.get(0)?.as_str().to_string();
        let inline_id = self.inline(text);
        Some(Token::Paragraph {
            raw,
            text: text.to_string(),
            inline_id: Some(inline_id),
            tokens: Vec::new(),
        })
    }

    // -----------------------------------------------------------------------
    // text（块级）
    // -----------------------------------------------------------------------
    /// `text`：块级 text token（非顶层上下文的兜底，例如列表 item 内部）。
    ///
    /// 规则只匹配一行（`^[^\n]+`），`raw` 与 `text` 相同，`escaped=false`，行内 token 通过
    /// `inline_id` 延迟生成。主循环会把连续产生的 Text token 合并（补一个 `\n` 并更新行内
    /// 队列源），所以最终 token 树里的一个 Text token 往往包含多行。
    pub(crate) fn text(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.block_text.captures(src).ok().flatten()?;
        let raw = cap.get(0)?.as_str().to_string();
        let inline_id = self.inline(&raw);
        Some(Token::Text {
            raw: raw.clone(),
            text: raw,
            inline_id: Some(inline_id),
            tokens: Vec::new(),
            escaped: false,
        })
    }

    // -----------------------------------------------------------------------
    // 块级扩展
    // -----------------------------------------------------------------------
    /// 块级扩展入口：当前只注册了 LaTeX 的 `$$...$$` 与 `\[...\]`。
    ///
    /// `block_tokens` 在每个块的最开头先尝试它（早于 `space`），成功时返回
    /// [`Token::LatexBlock`]；未闭合且内容非空、不含表格起点（`$$` 还需通过
    /// `looks_like_pending_dollar_math`）的定界符才返回 `pending=true` 的 token，其余未闭合
    /// 情况返回 `None`，交给普通 paragraph/text。流式渲染阶段 pending token 按 `trim_js` 后的
    /// 源码输出。具体匹配逻辑在 `crate::markdown::latex_ext::tokenize_block_latex`。
    pub(crate) fn tokenize_block_extension(&mut self, src: &str) -> Option<Token> {
        super::latex_ext::tokenize_block_latex(src)
    }
}
