//! latex 扩展：`components/markdown.ts` 里的 `TokenizerExtension`（block + inline）与
//! `trimPartialClosingFences`。
//!
//! 对应 TS 源 `packages/tui/src/components/markdown.ts` 第 33–197 行。扩展在 marked 的
//! tokenizer 之前的扩展位运行：块级扩展在每个块的开头尝试，行内扩展在每个行内扫描位置尝试；
//! `start` 函数只用于把 `paragraph`/`text` 的扫描裁剪到定界符之前，避免扩展符号被当成普通文本。
//!
//! 流式（未闭合）的 `$$`/`\[` 返回 `pending` token：渲染层原样输出源码，等闭合后切换为渲染
//! （issue #5825 的同类处理）。`containsTableStart` 保证块级数学不会吞掉紧随其后的 GFM 表格
//! （issue #11159）。
//!
//! # 在解析链中的位置
//! - 块级：`lexer::block_tokens` 在每轮块扫描的最开头调用 `block::tokenize_block_extension`，
//!   后者转发到 `tokenize_block_latex`；paragraph 的扫描范围由 `block_extension_start`
//!   裁剪，保证 `$$` / `\[` 之前的内容先成为 paragraph。
//! - 行内：`inline` 模块的主循环在 escape 之前调用 `tokenize_inline_latex`；text token
//!   的扫描范围由 `inline_extension_start` 裁剪，避免 `$...$` 被当成普通文本吞掉。
//! - 流式输出：`crate::components::markdown` 在 `Lexer::lex` 之后调用
//!   `trim_partial_closing_fences`，修剪围栏代码块末尾“还没写完”的闭围栏行。
//!
//! # 一个定界符的生命周期
//! 1. 块级扩展只在块开头尝试（正则允许最多 3 个前导空格）；行内扩展在每个行内扫描位置
//!    尝试，由 `inline_extension_start` 保证 text 不会跨过定界符。
//! 2. 找到未转义的闭合定界符并通过后续检查后才产出 `pending = false` 的 token：块级要求
//!    内容非空且不含表格起始（`text` 再经 `trim_js` 去首尾空白）；行内要求内容非空、不
//!    跨行，单 `$` 还要先通过歧义消解。
//! 3. 找不到闭合时进入流式分支：行内分支中 `\[`/`\(` 无闭合时一律产出 `pending = true`；
//!    块级没有 `\(`，只有 `\[`，且捕获内容非空才产出（空内容交回普通文本）；`$`/`$$` 还
//!    要求内容通过 `looks_like_pending_dollar_math`，否则宁可按普通文本处理。渲染层对
//!    pending token 输出源码（块级先 `trim_js`），闭合后的下一帧才交给 LaTeX 渲染器。
//! 4. 块级匹配前先检查 `contains_table_start`：惰性 `[\s\S]*?` 会跨越空行吞掉后面的 GFM
//!    表格，宁可放弃数学匹配，让表格 tokenizer 接管。
//!
//! # 重要约束与失败模式
//! - 正则在 `OnceLock` 首次初始化时编译（见 `regexes`）；模式写错会在首次调用 `regexes()`
//!   时 panic。按当前调用链，第一次非空 markdown 的块级词法分析
//!   （`Lexer::lex` → `block_tokens` → `tokenize_block_latex`）就会触发；这属于编程错误，
//!   不是进程启动期错误，也不是可恢复的解析错误。
//! - `fancy_regex` 的运行时错误（例如回溯限制触发）在调用点被 `unwrap_or(false)` /
//!   `ok().flatten()` 吞掉，行为等同于“没有匹配”：极端输入下扩展会静默失效而不是报错。
//! - `block_extension_start` / `inline_extension_start` 返回的是 **UTF-16 码元下标**，
//!   调用方按 JS 的 `slice`/`substring` 语义使用（换算工具在 `lexer` 模块）。
//! - `trim_partial_closing_fences` 只改 `Code::text`，不碰 `raw`；其余 token 一律不动。

use std::sync::OnceLock;

use fancy_regex::Regex;

use super::Token;
use super::lexer::translate_pattern;

/// 把一条 JS 方言的正则源翻译后编译。
///
/// 模式常量按 TS 源的语义书写；方言差异（`\s`/`\d`/`.`/`[\s\S]`）统一由
/// `translate_pattern` 换算。编译失败直接 panic：模式是编译期常量，写错属于程序错误，
/// 不应在运行时静默降级。panic 信息里附带译文，便于区分是原模式写错还是翻译引入的问题。
fn re(pattern: &str) -> Regex {
    let translated = translate_pattern(pattern);
    Regex::new(&translated)
        .unwrap_or_else(|err| panic!("latex regex compile failed: {err}\n{translated}"))
}

/// 本扩展用到的全部正则，由 [`regexes`] 惰性编译后进程内共享。
///
/// 分组：`table_delimiter_row` 用于表格起始探测；`looks_like_pending_dollar_math`
/// 用于 `$` 的流式判定；`dollar_block`/`bracket_block` 匹配闭合块，
/// `pending_*` 匹配未闭合块；`block_start` 用于块级扩展起点裁剪；
/// `constant_name`/`identifier_start` 用于区分 shell 变量式的 `$NAME`。
struct LatexRegexes {
    table_delimiter_row: Regex,
    looks_like_pending_dollar_math: Regex,
    dollar_block: Regex,
    bracket_block: Regex,
    pending_bracket_block: Regex,
    pending_dollar_block: Regex,
    block_start: Regex,
    constant_name: Regex,
    identifier_start: Regex,
}

/// 惰性编译并返回共享的正则表。
///
/// 第一次调用完成 9 条模式的编译，之后直接复用；模式源按 TS 语义书写，编译前先经
/// `translate_pattern` 换成 `fancy_regex` 支持的方言。
fn regexes() -> &'static LatexRegexes {
    static REGEXES: OnceLock<LatexRegexes> = OnceLock::new();
    REGEXES.get_or_init(|| LatexRegexes {
        table_delimiter_row: re(r"^ {0,3}\|?[ \t]*:?-+:?[ \t]*(?:\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$"),
        looks_like_pending_dollar_math: re(r"\\[A-Za-z]+|[_^=+*/<>()\[\]|±≤≥≠≈∈→⇒∞∫∑√-]"),
        dollar_block: re(r"^ {0,3}\$\$[ \t]*\n?([\s\S]*?)\$\$[ \t]*(?:\n|$)"),
        bracket_block: re(r"^ {0,3}\\\[[ \t]*\n?([\s\S]*?)\\\][ \t]*(?:\n|$)"),
        pending_bracket_block: re(r"^ {0,3}\\\[[ \t]*\n?([\s\S]*)$"),
        pending_dollar_block: re(r"^ {0,3}\$\$[ \t]*\n?([\s\S]*)$"),
        block_start: re(r"(?:^|\n) {0,3}(?:\$\$|\\\[)"),
        constant_name: re(r"^[A-Z_][A-Z0-9_]*(?:[^A-Za-z0-9_\s])?$"),
        identifier_start: re(r"^[A-Za-z_][A-Za-z0-9_]*"),
    })
}

/// `isEscaped(source, index)`：向前数连续反斜杠，奇数表示被转义。
///
/// `index` 是从 0 开始的**字节**下标，且必须落在字符边界上（调用方传入的是
/// `str::find` 的结果）。例如 `\$` 中 `$` 被转义，而 `\\$` 中没有。
fn is_escaped(source: &str, index: usize) -> bool {
    let bytes = source.as_bytes();
    let mut backslashes = 0usize;
    let mut position = index;
    while position > 0 && bytes[position - 1] == b'\\' {
        backslashes += 1;
        position -= 1;
    }
    backslashes % 2 == 1
}

/// `findClosingDelimiter(source, closing, start)`：`indexOf` 后跳过被转义的命中。
///
/// 返回第一处未转义命中的字节下标；`start` 也必须是字符边界。每跳过一次转义命中，
/// 就从命中的下一字节继续找，因此不会在同一位置反复命中。找不到时返回 `None`。
fn find_closing_delimiter(source: &str, closing: &str, start: usize) -> Option<usize> {
    let mut search = start;
    loop {
        let index = source[search..].find(closing)? + search;
        if !is_escaped(source, index) {
            return Some(index);
        }
        search = index + closing.len();
    }
}

/// `looksLikePendingDollarMath(source)`。
///
/// `source` 可能来自三处：行内 `$` 或 `$$` 之后的剩余源码（`tokenize_inline_latex` 的
/// pending 分支），以及块级未闭合 `$$` 的捕获组内容（`pending_dollar_block`，已去掉开
/// 定界符及其后的空格/制表符与一个可选换行）。只有出现 LaTeX 命令（`\name`）或常见数学
/// 符号/运算符才认为这是值得等待闭合的流式数学，避免把正文里落单的 `$` 也变成 pending token。
/// 正则运行错误按“不匹配”处理（返回 `false`）。
fn looks_like_pending_dollar_math(source: &str) -> bool {
    regexes()
        .looks_like_pending_dollar_math
        .is_match(source)
        .unwrap_or(false)
}

/// `tokenizeInlineLatex(source)`。
///
/// 输入是行内扫描当前位置开始的剩余源码；只有它以 `$$`、`\(`、`\[` 或单个 `$` 开头时才
/// 可能产出 token，否则返回 `None` 交给其它 tokenizer。
///
/// 返回值：
/// - `Some(Latex { pending: false })`：找到未转义的闭合定界符，且内容非空、不含换行；
///   `raw` 只包含到闭合定界符为止的前缀，调用方按 `raw` 推进源码。
/// - `Some(Latex { pending: true })`：没有闭合定界符。`\(`/`\[` 的内容任意；`$`/`$$`
///   还要求 `looks_like_pending_dollar_math` 通过。
/// - `None`：不是数学。包括单 `$` 的歧义消解失败、内容为空、内容跨行等情况。
///
/// 单 `$` 的歧义消解只在“已经找到闭合定界符”后执行，具体规则见函数内注释。
pub(crate) fn tokenize_inline_latex(source: &str) -> Option<Token> {
    // 分支顺序：`$$` → `\(` → `\[` → 单个 `$`；单 `$` 后紧跟 JS 空白（如 "$ 5"）直接拒绝。
    let (opening, closing): (&str, &str) = if source.starts_with("$$") {
        ("$$", "$$")
    } else if source.starts_with("\\(") {
        ("\\(", "\\)")
    } else if source.starts_with("\\[") {
        ("\\[", "\\]")
    } else if source.starts_with('$') && !starts_with_dollar_space(source) {
        ("$", "$")
    } else {
        return None;
    };

    let closing_index = find_closing_delimiter(source, closing, opening.len());
    if let Some(closing_index) = closing_index
        && opening == "$"
    {
        // 找到闭合 `$` 后才做歧义消解：右定界符前的尾随空白、右定界符后的数字、
        // `$CONST_NAME` 紧邻标识符、含反引号的片段，都更像文本/代码而不是数学。
        let inner = &source[opening.len()..closing_index];
        let after = &source[closing_index + closing.len()..];
        if ends_with_js_whitespace(inner)
            || starts_with_digit(after)
            || (is_constant_name(inner) && starts_with_identifier(after))
            || inner.contains('`')
        {
            return None;
        }
    }

    let Some(closing_index) = closing_index else {
        // 没有闭合定界符：反斜杠定界符（\( / \[）总可以安全地等待；单个 `$` 需要内容
        // “像数学”，否则正文里的落单美元符号会一直被当成 pending 吞掉后续文本。
        let pending_source = &source[opening.len()..];
        if opening.starts_with('\\') || looks_like_pending_dollar_math(pending_source) {
            return Some(Token::Latex {
                raw: source.to_string(),
                text: pending_source.to_string(),
                pending: true,
            });
        }
        return None;
    };

    let text = &source[opening.len()..closing_index];
    // 行内数学必须非空且不跨行；否则交回普通 tokenizer。
    if text.is_empty() || text.contains('\n') {
        return None;
    }

    let raw = &source[..closing_index + closing.len()];
    Some(Token::Latex {
        raw: raw.to_string(),
        text: text.to_string(),
        pending: false,
    })
}

/// `/^\$\s/`（`\s` 用 JS 空白集合）。
///
/// 只看 `$` 后的第一个字符；空串（只有 `$`）返回 false，交给后续的 pending 分支。
fn starts_with_dollar_space(source: &str) -> bool {
    let mut chars = source.chars();
    if chars.next() != Some('$') {
        return false;
    }
    matches!(chars.next(), Some(ch) if crate::text::is_js_whitespace(ch))
}

/// `/^\d/`。
///
/// 只用 ASCII 数字，对应 JS `\d` 的语义（不是 Unicode 的 Nd 类）。
fn starts_with_digit(source: &str) -> bool {
    matches!(source.chars().next(), Some(ch) if ch.is_ascii_digit())
}

/// `/^[A-Z_][A-Z0-9_]*(?:[^A-Za-z0-9_\s])?$/`（`\s` 用 JS 空白集合）。
///
/// 末尾允许一个非字母数字、非空白的字符，是为了让 `$HOME/` 这类 shell 变量路径也算
/// “常量名”。用于 `$NAME` 的歧义消解，不用于数学内容解析。
fn is_constant_name(text: &str) -> bool {
    regexes().constant_name.is_match(text).unwrap_or(false)
}

/// `/^[A-Za-z_][A-Za-z0-9_]*/`。
///
/// 锚定在开头但只匹配前缀（没有 `$`）；用来判断右定界符之后是否紧跟一个标识符，
/// 从而把 `$CONST_NAME` 与 `$USER` 这类 shell 变量排除在数学之外。
fn starts_with_identifier(source: &str) -> bool {
    regexes().identifier_start.is_match(source).unwrap_or(false)
}

/// `/\s$/`（JS 空白集合）；空串返回 false。
fn ends_with_js_whitespace(text: &str) -> bool {
    text.chars()
        .next_back()
        .is_some_and(crate::text::is_js_whitespace)
}

/// `containsTableStart(source)`：相邻两行中第二行是 GFM 分隔行且（分隔行含 `|` 或首行含 `|`）。
///
/// 块级数学的闭合与 pending 匹配都要做“止损”判断：闭合模式用惰性 `[\s\S]*?`，
/// pending 模式用贪婪 `[\s\S]*` 一直吃到输入末尾；两者都会跨越空行，若不检查，
/// `$$`/`\[` 会把后面的表格吞进数学块（issue #11159）。
///
/// 要求两行中至少一行含 `|`，是为了避免把 `标题` + `---` 这种 setext 标题的下划线误判
/// 成表格起始（GFM 表格的分隔行本身允许不带 `|`，单列表格就是这种情况）。
fn contains_table_start(source: &str) -> bool {
    let lines: Vec<&str> = source.split('\n').collect();
    if lines.len() < 2 {
        return false;
    }
    let regex = &regexes().table_delimiter_row;
    for index in 0..lines.len() - 1 {
        let delimiter_row = lines[index + 1];
        if (delimiter_row.contains('|') || lines[index].contains('|'))
            && regex.is_match(delimiter_row).unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// `tokenizeBlockLatex(source)`。
///
/// 只在块的开头尝试，按 TS 的顺序匹配四种形式（先闭合、后未闭合）：
/// 1. `$$...$$`：内容非空，且不成 GFM 表格起始；
/// 2. `\[...\]`：同样要求内容非空、不成表格；
/// 3. `\[...` 未闭合 → `pending = true`（内容非空即可；对应 TS 的 `pendingBracket?.[1]`
///    真值判断）；
/// 4. `$$...` 未闭合 → `pending = true`，且内容必须通过 `looks_like_pending_dollar_math`。
///
/// 闭合分支的 `text` 经 `trim_js` 去首尾空白；pending 分支保留原内容（渲染层原样输出）。
/// `raw` 是正则整体匹配，可能带上结尾换行或定界符后的空格。
///
/// 四个分支都检查 `contains_table_start`：宁可放弃数学匹配，也不能吞掉后面的表格。
/// 返回 `None` 表示“这里不是块级数学”，块扫描继续尝试内建 tokenizer。
pub(crate) fn tokenize_block_latex(source: &str) -> Option<Token> {
    let regexes = regexes();
    // 闭合形式优先，避免把已经闭合的块误判成 pending。
    if let Ok(Some(cap)) = regexes.dollar_block.captures(source)
        && let Some(content) = cap.get(1)
        && !content.as_str().is_empty()
        && !contains_table_start(content.as_str())
    {
        return Some(Token::LatexBlock {
            raw: cap.get(0).expect("group 0").as_str().to_string(),
            text: crate::text::trim_js(content.as_str()).to_string(),
            pending: false,
        });
    }

    if let Ok(Some(cap)) = regexes.bracket_block.captures(source)
        && let Some(content) = cap.get(1)
        && !content.as_str().is_empty()
        && !contains_table_start(content.as_str())
    {
        return Some(Token::LatexBlock {
            raw: cap.get(0).expect("group 0").as_str().to_string(),
            text: crate::text::trim_js(content.as_str()).to_string(),
            pending: false,
        });
    }

    // 未闭合的 \[：内容非空即视为流式数学。
    if let Ok(Some(cap)) = regexes.pending_bracket_block.captures(source)
        && let Some(content) = cap.get(1)
        // TS `pendingBracket?.[1] &&`：捕获为空串时是假值，落到普通 paragraph/text 路径。
        && !content.as_str().is_empty()
        && !contains_table_start(content.as_str())
    {
        return Some(Token::LatexBlock {
            raw: cap.get(0).expect("group 0").as_str().to_string(),
            text: content.as_str().to_string(),
            pending: true,
        });
    }
    // 未闭合的 $$：匹配到输入末尾，内容还必须像数学，否则交回普通文本。
    if let Ok(Some(cap)) = regexes.pending_dollar_block.captures(source)
        && let Some(content) = cap.get(1)
        && looks_like_pending_dollar_math(content.as_str())
        && !contains_table_start(content.as_str())
    {
        return Some(Token::LatexBlock {
            raw: cap.get(0).expect("group 0").as_str().to_string(),
            text: content.as_str().to_string(),
            pending: true,
        });
    }
    None
}

/// 块级扩展的 `start(source)`：返回 `(?:^|\n) {0,3}(?:\$\$|\\\[)` 的匹配起点
/// （若匹配文本以 `\n` 开头则 +1）。返回 UTF-16 下标。
///
/// 结果指向定界符所在行的行首（换行符之后；若该行有前导空格，指向第一个空格），
/// 换行符本身不计入。单位是 UTF-16 码元，因为调用方（`lexer` 的 paragraph 裁剪）按
/// JS `slice`/`substring` 的语义使用它。
///
/// 无匹配或正则运行错误时返回 `None`，调用方不做裁剪。
pub(crate) fn block_extension_start(source: &str) -> Option<usize> {
    let matched = regexes().block_start.find(source).ok().flatten()?;
    let text = matched.as_str();
    let offset = if text.starts_with('\n') { 1 } else { 0 };
    Some(super::lexer::utf16_len(&source[..matched.start()]) + offset)
}

/// 行内扩展的 `start(source)`：`$`、`\(`、`\[` 三处 `indexOf` 的最小值（UTF-16 下标）。
///
/// 这里用的是字面量 `find`，不跳过被转义的定界符：即使起点落在 `\$` 上，行内循环在
/// 该位置调用 `tokenize_inline_latex` 会返回 `None`，随后 escape tokenizer 消费转义，
/// 因此结果不受影响。三者都不存在时返回 `None`。
pub(crate) fn inline_extension_start(source: &str) -> Option<usize> {
    let mut indices: Vec<usize> = Vec::new();
    if let Some(index) = source.find('$') {
        indices.push(super::lexer::utf16_len(&source[..index]));
    }
    if let Some(index) = source.find("\\(") {
        indices.push(super::lexer::utf16_len(&source[..index]));
    }
    if let Some(index) = source.find("\\[") {
        indices.push(super::lexer::utf16_len(&source[..index]));
    }
    indices.into_iter().min()
}

/// `trimPartialClosingFences(tokens)`：只处理最后一个 token（递归进 list/blockquote）。
///
/// 语义是“隐藏看起来还没写完的闭围栏”：流式输入的最后一行可能只有 1–2 个反引号或
/// 波浪号，词法阶段它属于代码内容；直接渲染会先显示这些字符、闭合后突然消失。这里把
/// 该行从 `text` 中删掉（连同它前面的一个换行，若存在），使未闭合与闭合两种状态输出
/// 行数一致（测试 `stabilizes_partial_closing_fence_rendering` 锁定该行为）。
///
/// 只处理最后一个 token：流式输入的新字符总是追加在末尾，只有末尾的代码块可能还没有
/// 收到闭合围栏；前面的代码块一旦遇上后续内容，就已经被某个闭围栏消费掉了。
///
/// 判定一条行是“部分闭围栏”需要同时满足：
/// - `raw` 以至少 3 个同类围栏字符开头（`fence_marker`；要求位于字节 0，带缩进的围栏
///   不处理）；
/// - `raw` 的最后一行非空、UTF-16 长度小于开围栏长度，且整行都是同一围栏字符。
///
/// 注意：长度不足开围栏的行按 CommonMark 本可作为代码内容，这里为了流式稳定仍然隐藏
/// 它。函数只修改 `text`，`raw` 保持原样。
pub(crate) fn trim_partial_closing_fences(tokens: &mut [Token]) {
    let Some(token) = tokens.last_mut() else {
        return;
    };
    match token {
        Token::List(list) => {
            // 只有最后一个条目还可能处于流式接收状态。
            if let Some(item) = list.items.last_mut() {
                trim_partial_closing_fences(&mut item.tokens);
            }
        }
        Token::Blockquote { tokens, .. } => trim_partial_closing_fences(tokens),
        Token::Code { raw, text, .. } => {
            let Some(marker) = fence_marker(raw) else {
                return;
            };
            let Some(last_line) = raw.split('\n').next_back() else {
                return;
            };
            let last_line_len = super::lexer::utf16_len(last_line);
            let marker_len = super::lexer::utf16_len(marker);
            // `last_line_len >= marker_len` 时直接返回、不再修剪：这一行可能是真正的闭围栏
            // （词法阶段已把它排除在 `text` 之外），也可能只是普通代码内容（例如开围栏为
            // 4 个反引号、最后一行是 ````x）。只有整行都是同一围栏字符、且短于开围栏的行，
            // 才视为“部分闭围栏”并从 `text` 中隐藏。
            if last_line.is_empty()
                || last_line_len >= marker_len
                || last_line != repeat_char(marker.chars().next().expect("marker"), last_line_len)
            {
                return;
            }
            // 按字节截掉最后一行（ASCII 围栏字符，字节长度等于字符数）；若剩余文本仍以
            // 换行结尾，再删掉这个换行，避免代码块尾部多出一个空行。
            let new_len = text.len().saturating_sub(last_line.len());
            let mut new_text = text[..new_len].to_string();
            if new_text.ends_with('\n') {
                new_text.pop();
            }
            *text = new_text;
        }
        _ => {}
    }
}

/// `/^(`{3,}|~{3,})/`。
///
/// 只认字符串开头的连续反引号或波浪号（要求至少 3 个，且同一字符），不跳过前导空格。
/// 返回整段 run（可能长于 3 个），供调用方比较“部分闭围栏”的长度。
fn fence_marker(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let first = *bytes.first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let mut end = 0usize;
    while end < bytes.len() && bytes[end] == first {
        end += 1;
    }
    if end >= 3 { Some(&raw[..end]) } else { None }
}

fn repeat_char(ch: char, count: usize) -> String {
    ch.to_string().repeat(count)
}
