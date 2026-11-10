//! lexer：`marked@18` 的 `Lexer.ts` + `rules.ts`（GFM 模式）。
//!
//! `rules.ts` 里的正则在 TypeScript 侧是**按字符串拼接/替换**构造的（`edit(...).replace(...)`），
//! 并且 `edit` 会剥掉替换源里的 `^` 锚点。这里逐条复刻该构造过程（`edit_first` /
//! `edit_all`），最后统一把 JS 方言的 `\s`/`\d`/`\w`/`.` 展开成 Rust 正则等价物
//! （`translate_pattern`）。这样规则文本可以与 TS 源逐行对照。
//!
//! 只实现 marked 默认选项下的 GFM 规则（`gfm: true`、`breaks: false`、`pedantic: false`）。
//!
//! # 这个文件在解析链中的位置
//! [`Lexer`] 是词法分析器的入口和驱动器：`crate::components::markdown` 的渲染路径、
//! `kiana-interactive` 的 mermaid 扩展以及本 crate 的 `tests/markdown.rs` 都是
//! `Lexer::new()` 之后调用 [`Lexer::lex`]。真正的块级/行内 tokenizer 分别实现于
//! [`super::block`] 与 [`super::inline`]（都是 `impl Lexer` 的方法）；本文件提供它们
//! 共用的规则表（`Rules`、`rules`）、解析状态（`LexerState`）与 UTF-16 下标工具。
//!
//! # 一次 `lex` 的执行流程
//! 1. 把 `\r\n`/`\r` 归一化成 `\n`（`normalize_carriage_returns`）；
//! 2. `block_tokens` 按块级规则生成顶层 token。遇到 heading/paragraph/text 或表格
//!    单元格时只登记 `(id, src)`（`Lexer::inline`），不做行内解析——这样文档后部
//!    才出现的链接定义也能被前部的引用式链接看到；
//! 3. 块级扫描结束后，逐个处理队列快照，把行内 token 写进 `results[id]`；
//! 4. `lexer_fill_inline_tokens` 递归把结果填回 token 树并返回顶层 token。
//!
//! # 重要约束
//! - **规则表只编译一次**：`rules()` 用 `OnceLock` 惰性构建；单条规则写错会在首次
//!   建表时 panic（`re`），属于首次初始化规则表/首次解析时的不可恢复错误，不是某段
//!   输入返回的可恢复解析错误。
//! - **索引单位是 UTF-16 码元**：TS 的所有字符串下标都按 UTF-16 计。凡是需要与 TS
//!   对齐的切分（raw 推进、扩展起点、掩码长度）必须走本文件末尾的 UTF-16 工具，
//!   不能用 `str::len()`（UTF-8 字节）或 `chars().count()`（码点）。
//! - **`Lexer` 按文档新建**：`lex` 不会清空 `links`/`results`，复用实例会把上一份
//!   文档的链接定义带进下一份；仓库内调用方都每次 [`Lexer::new`]。

use std::sync::OnceLock;

use fancy_regex::Regex;
use indexmap::IndexMap;

use super::Token;

/// 规则表的正则类型别名。
///
/// 规则源里用到了反向引用（`inline_code`/`block_skip` 的 `\1`、`\k<a>`）和 lookbehind
/// （如 `build_inline_text` 里的 `(?<![A-Za-z0-9_])_`），标准 `regex` crate 不支持
/// 这些语法，因此规则统一用 `fancy_regex::Regex`。
pub(crate) type Re = Regex;

/// JS `\s` 的码点集合（与 `text::is_js_whitespace` 一致）。
///
/// `translate_pattern` 用它替换规则源里的 `\s`：字符类外补一层 `[...]`，字符类内直接
/// 展开成范围正文。`\u{feff}`（BOM）在 JS 里算空白、在 Rust `regex` 的 `\s` 里不算，
/// 所以不能直接依赖后者的 `\s`。
pub(crate) const JS_WS_BODY: &str = r"\t\n\x0b\x0c\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";

/// JS `.`（无 `s` 标志）不匹配的换行类字符。
///
/// 即 JS 的四个行终止符；`translate_pattern` 把字符类外的 `.` 展开成它。
const JS_DOT: &str = r"[^\n\r\u{2028}\u{2029}]";

/// 把 marked 规则源里的 JS 正则方言展开成 Rust 正则方言。
///
/// 输入是 TS `rules.ts` 里可以直接对照的规则文本，输出交给 `fancy_regex` 编译。
/// 这样规则常量本身不用改写，方言差异集中在这一个函数里：
/// - 字符类外的 `[\s\S]` → `(?s:.)`（整体识别，先于 `.` 展开）；
/// - `\s` → JS 空白集合（类内展开正文，类外补 `[...]`）；
/// - `\d` → `[0-9]`/`0-9`，`\w` → `[A-Za-z0-9_]`/`A-Za-z0-9_`（JS 语义，不是 Unicode 版）；
/// - 字符类外的 `.` → `JS_DOT`；
/// - 其余转义（如 `\1`、`\k<a>`、`\p{P}`）原样保留，由 `fancy_regex` 处理。
///
/// 只用 `in_class` 跟踪是否位于 `[...]` 内；marked 的规则源不依赖字符类嵌套或集合
/// 运算，所以不需要更完整的字符类解析。输出不做合法性校验，写错会在 `re` 编译时 panic。
pub(crate) fn translate_pattern(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len() + 64);
    let mut i = 0usize;
    let mut in_class = false;
    while i < chars.len() {
        let c = chars[i];
        // `[\s\S]`（字符类外）→ `(?s:.)`。必须在 `.` 展开之前整体识别，
        // 否则插入的 `.` 会被再展开成不带 `s` 的 JS `.`，从而无法跨行匹配。
        if !in_class
            && c == '['
            && chars.get(i + 1) == Some(&'\\')
            && chars.get(i + 2) == Some(&'s')
            && chars.get(i + 3) == Some(&'\\')
            && chars.get(i + 4) == Some(&'S')
            && chars.get(i + 5) == Some(&']')
        {
            out.push_str("(?s:.)");
            i += 6;
            continue;
        }
        // 转义序列：只有 `\s`/`\d`/`\w` 需要展开，其余（反向引用、`\p{...}`、字面转义）
        // 原样输出；`in_class` 决定前三个是否再包一层字符类括号。
        if c == '\\' && i + 1 < chars.len() {
            let n = chars[i + 1];
            match n {
                's' => {
                    if in_class {
                        out.push_str(JS_WS_BODY);
                    } else {
                        out.push('[');
                        out.push_str(JS_WS_BODY);
                        out.push(']');
                    }
                }
                'd' => {
                    if in_class {
                        out.push_str("0-9");
                    } else {
                        out.push_str("[0-9]");
                    }
                }
                'w' => {
                    if in_class {
                        out.push_str("A-Za-z0-9_");
                    } else {
                        out.push_str("[A-Za-z0-9_]");
                    }
                }
                _ => {
                    out.push('\\');
                    out.push(n);
                }
            }
            i += 2;
            continue;
        }
        // 字符类边界与类外的 `.`：没有反斜杠前缀的 `]` 一律退出字符类；转义形式 `\]`
        // 已在上面的分支整体复制，不会影响 `in_class`。
        match c {
            '[' => {
                in_class = true;
                out.push(c);
            }
            ']' => {
                in_class = false;
                out.push(c);
            }
            '.' if !in_class => out.push_str(JS_DOT),
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

/// `valSource.replace(/(^|[^\[])\^/g, '$1')`：剥掉替换源里的 `^` 锚点（`[` 之后的保留）。
///
/// marked 把规则片段当作替换值拼进外层规则；片段自带的 `^` 拼接后只会在整个规则的
/// 开头生效，位置不对，所以 TS 侧先删掉。判定只回看一个字符：串首或前一个字符不是
/// `[` 的 `^` 都删除（`[^...]` 里的取反符保留）。
fn strip_carets(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '^' && (i == 0 || chars[i - 1] != '[') {
            i += 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// `edit(...).replace(name, value)`（字符串 name → 只替换第一个）。
///
/// `name` 按字面查找（不是正则），`value` 先过 `strip_carets`。各条规则靠这些字符串
/// 替换层层展开别名，替换顺序会影响后续替换能看到的文本，因此 `build_*` 里的调用
/// 顺序不能随意调整（见上）。
fn edit_first(source: &str, name: &str, value: &str) -> String {
    let value = strip_carets(value);
    source.replacen(name, &value, 1)
}

/// `edit(...).replace(/name/g, value)`（全局替换）。
///
/// 用于同一个别名在规则里出现多次的场合（如 `build_lheading_gfm` 的两条负向先行
/// 断言）；语义与 `edit_first` 相同，只是替换所有字面出现。
fn edit_all(source: &str, name: &str, value: &str) -> String {
    let value = strip_carets(value);
    source.replace(name, &value)
}

/// 编译单条规则：先做方言翻译，再交给 `fancy_regex`。
///
/// 编译失败直接 panic，panic 文本带上翻译后的正则，方便把报错位置对回规则源。
/// 所有调用点都在 `init_rules`（即 `rules()` 的惰性初始化）内，所以规则写错的表现为
/// 进程首次解析 markdown 时崩溃，而不是某段输入返回错误。
fn re(pattern: &str) -> Re {
    let translated = translate_pattern(pattern);
    Re::new(&translated)
        .unwrap_or_else(|err| panic!("regex compile failed: {err}\npattern: {translated}"))
}

// ---------------------------------------------------------------------------
// 规则源（marked 18.0.11 的 src/rules.ts）
// ---------------------------------------------------------------------------

// 这些常量是 rules.ts 中的“片段模板”（`bull`/`heading`/`tag` 等别名）和可复用的
// 子表达式。marked 的规则不是静态字面量，而是运行时用 `edit(...).replace(...)` 拼出来
// 的；下面的 `build_*` 函数复刻了同一拼接过程，常量文本保持与 TS 源逐行可对照。

/// 列表项标记：0–3 个前导空格 + 无序符号或 1–9 位有序序号（`1.`/`1)`）。
const BULLET: &str = r" {0,3}(?:[*+-]|\d{1,9}[.)])";
/// 水平线规则（`hr`），组 1 是整段标记；三种符号各自至少重复 3 次。
const HR: &str = r"^ {0,3}((?:-[\t ]*){3,}|(?:_[ \t]*){3,}|(?:\*[ \t]*){3,})(?:\n+|$)";
/// 围栏的“打断段落”判定：反引号围栏的 info string 里不能再出现反引号。
const FENCES_INTERRUPT: &str = r" {0,3}(?:`{3,}(?=[^`\n]*(?:\n|$))|~~~)[^\n]*(?:\n|$)";
/// HTML 块的“打断段落”判定；`tag` 别名最后替换成 [`TAG_LIST`]。
const HTML_INTERRUPT: &str = r"</?(?:tag)(?: +|\n|/?>)|<(?:script|pre|style|textarea|!--)";
/// HTML 块级标签名白名单（`<tag`/`</tag` 分支用）。
const TAG_LIST: &str = concat!(
    "address|article|aside|base|basefont|blockquote|body|caption",
    "|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption",
    "|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe",
    "|legend|li|link|main|menu|menuitem|meta|nav|noframes|ol|optgroup|option",
    "|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title",
    "|tr|track|ul",
);
/// 链接定义/引用标签的正文：可含转义字符对，不能含裸 `[`/`]`/`\`；拒绝空白后直接 `]`。
const BLOCK_LABEL: &str = r"(?!\s*\])(?:\\[\s\S]|[^\[\]\\])+";
/// 链接标题：双引号、单引号或圆括号三种形式。
const TITLE: &str = r#""(?:\\"?|[^"\\])*"|'[^'\n]*(?:\n[^'\n]+)*\n?'|\([^()]*\)"#;
/// 块级 HTML 注释：允许未闭合（流式输入时一直吃到结尾）。
const COMMENT: &str = r"<!--(?:-?>|[\s\S]*?(?:-->|$))";
/// 行内 HTML 注释：必须有结尾 `-->`。
const INLINE_COMMENT: &str = r"<!--(?:-?>|[\s\S]*?-->)";
/// 块级 HTML 标签属性串；属性值允许空串、双引号、单引号或裸值。
const ATTRIBUTE_BLOCK: &str =
    r#" +[a-zA-Z:_][\w.:-]*(?: *= *"[^"\n]*"| *= *'[^'\n]*'| *= *[^\s"'=<>`]+)?"#;
/// 行内 HTML 标签属性串：前缀与等号两侧用 `\s`，可以跨行。
const ATTRIBUTE_INLINE: &str =
    r#"\s+[a-zA-Z:_][\w.:-]*(?:\s*=\s*"[^"]*"|\s*=\s*'[^']*'|\s*=\s*[^\s"'=<>`]+)?"#;
/// 非 GFM 的“标点”类；只被下划线版强调规则使用。
const PUNCTUATION_CLASS: &str = r"[\p{P}\p{S}]";
/// 非 GFM 的“标点或空白”类。
const PUNCTUATION_OR_SPACE_CLASS: &str = r"[\s\p{P}\p{S}]";
/// 非 GFM 的“既非标点也非空白”类。
const NOT_PUNCTUATION_OR_SPACE_CLASS: &str = r"[^\s\p{P}\p{S}]";
/// GFM 的“标点”类：`~` 被排除在标点之外（删除线使用）。
const PUNCTUATION_GFM: &str = r"(?!~)[\p{P}\p{S}]";
/// GFM 的“标点或空白”类。
const PUNCTUATION_OR_SPACE_GFM: &str = r"(?!~)[\s\p{P}\p{S}]";
/// GFM 的“既非标点也非空白”类；`~` 被算作非标点。
const NOT_PUNCTUATION_OR_SPACE_GFM: &str = r"(?:[^\s\p{P}\p{S}]|~)";
/// 行内链接的 label：允许嵌套方括号、反斜杠转义、成对的代码 span，且非贪婪。
const INLINE_LABEL: &str =
    r"(?:\[(?:\\[\s\S]|[^\[\]\\])*\]|\\[\s\S]|`+(?!`)[^`]*?`+(?!`)|``+(?=\])|[^\[\]\\`])*?";
/// 大小写不敏感地匹配 `http`/`https`/`ftp`：显式枚举大小写，不依赖正则标志。
const CASE_INSENSITIVE_PROTOCOL: &str = r"[hH][tT][tT][pP][sS]?|[fF][tT][pP]";
/// 行内自动链接的邮箱形式。本片段内 `(@)` 是组 1；拼进 `build_autolink` 后外层
/// `(scheme:...|email)` 成为组 1、`(@)` 变成组 2，`inline.rs::autolink` 实际读
/// `cap.get(2)` 判定 `mailto:`。
const INLINE_EMAIL: &str = r"[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+(@)[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)+(?![-_])";
/// GFM 裸邮箱形式。本片段内 `(@)` 是组 1；拼进 `build_url` 的
/// `^((?:protocol):\/\/|www\.)...|^email` 后组 1 是前缀、邮箱分支的 `(@)` 变成组 2，
/// `inline.rs::url` 用 `cap.get(2)` 区分邮箱分支。
const URL_EMAIL: &str =
    r"[A-Za-z0-9._+-]+(@)[a-zA-Z0-9-_]+(?:\.[a-zA-Z0-9-_]*[a-zA-Z0-9])+(?![-_])";
/// 行内扫描的掩码规则之一：行内链接 `[text](href)`（允许括号嵌套与转义）。
const BLOCK_SKIP_LINK: &str = r"\[(?:[^\[\]`]|(?<a>`+)[^`]+\k<a>(?!`))*?\]\((?:\\[\s\S]|[^\\\(\)]|\((?:\\[\s\S]|[^\\\(\)])*\))*\)";
/// 行内扫描的掩码规则之一：代码 span；命名组 `b` 反向引用同一串反引号。
const BLOCK_SKIP_CODE: &str = r"(?<b>`+)[^`]+\k<b>(?!`)";
/// 行内扫描的掩码规则之一：HTML 标签（`<` 后不能紧跟空格）。
const BLOCK_SKIP_HTML: &str = r"<(?! )[^<>]*?>";

/// GFM 段落规则的核心片段：组 1 是从行首直到某个块级打断符之前的多行文本。
///
/// 负向先行断言里的名字（hr/heading/lheading/blockquote/fences/list/html/table）
/// 是别名，由 `build_paragraph` / `build_blockquote_paragraph` 逐项替换成真实规则；
/// 单独看这个片段并不完整。
fn paragraph_core() -> &'static str {
    r"^([^\n]+(?:\n(?!hr|heading|lheading|blockquote|fences|list|html|table|[ \t]+\n)[^\n]+)*)"
}

/// 构造 GFM 表格规则（`table`），也供 paragraph 的“表格打断”判断复用。
///
/// 组 1=表头行，组 2=分隔行，组 3=数据行。数据行的循环用负向先行断言排除
/// hr/heading/blockquote/code/fences/list/html，避免把后续块级结构吞进表格。
fn build_gfm_table() -> String {
    let mut source = concat!(
        r"^ *([^\n ].*)\n",
        r" {0,3}((?:\| *)?:?-+:? *(?:\| *:?-+:? *)*(?:\| *)?)",
        r"(?:\n((?:(?! *\n|hr|heading|blockquote|code|fences|list|html).*(?:\n|$))*)\n*|$)",
    )
    .to_string();
    source = edit_first(&source, "hr", HR);
    source = edit_first(&source, "heading", r" {0,3}#{1,6}(?:\s|$)");
    source = edit_first(&source, "blockquote", r" {0,3}>");
    source = edit_first(&source, "code", r"(?: {4}| {0,3}\t)[^\n]");
    source = edit_first(&source, "fences", FENCES_INTERRUPT);
    source = edit_first(&source, "list", r" {0,3}(?:[*+-]|1[.)])[ \t]");
    source = edit_first(&source, "html", HTML_INTERRUPT);
    source = edit_first(&source, "tag", TAG_LIST);
    source
}

/// 构造顶层段落规则（`paragraph`）：遇到块级打断符就结束。
///
/// `list_interrupt` 由调用方给出（顶层与 blockquote 内不同）；`|lheading` 被替换为
/// 空串，即把该分支从负向先行断言中删掉；`table` 换成 [`build_gfm_table`] 的结果。
/// 逐项替换的顺序与 TS 构造保持一致。
fn build_paragraph(list_interrupt: &str, gfm_table: &str) -> String {
    let mut source = paragraph_core().to_string();
    source = edit_first(&source, "hr", HR);
    source = edit_first(&source, "heading", r" {0,3}#{1,6}(?:\s|$)");
    source = edit_first(&source, "|lheading", "");
    source = edit_first(&source, "table", gfm_table);
    source = edit_first(&source, "blockquote", r" {0,3}>");
    source = edit_first(&source, "fences", FENCES_INTERRUPT);
    source = edit_first(&source, "list", list_interrupt);
    source = edit_first(&source, "html", HTML_INTERRUPT);
    source = edit_first(&source, "tag", TAG_LIST);
    source
}

/// 构造 blockquote 内部使用的段落规则（marked 的 `blockquoteParagraph`）。
///
/// 与顶层段落相比：`|table` 分支被删除（不再把表格起点当作打断符），`list` 换成
/// ` {0,3}(?:[*+-]|\d{1,9}[.)])(?:[ \t]|\n|$)`（任意有序序号都能打断）。结果会被
/// `init_rules` 剥掉 `^` 后内联进 `blockquote` 规则。
fn build_blockquote_paragraph() -> String {
    let mut source = paragraph_core().to_string();
    source = edit_first(&source, "hr", HR);
    source = edit_first(&source, "heading", r" {0,3}#{1,6}(?:\s|$)");
    source = edit_first(&source, "|lheading", "");
    source = edit_first(&source, "|table", "");
    source = edit_first(&source, "blockquote", r" {0,3}>");
    source = edit_first(&source, "fences", FENCES_INTERRUPT);
    source = edit_first(
        &source,
        "list",
        r" {0,3}(?:[*+-]|\d{1,9}[.)])(?:[ \t]|\n|$)",
    );
    source = edit_first(&source, "html", HTML_INTERRUPT);
    source = edit_first(&source, "tag", TAG_LIST);
    source
}

/// 构造 GFM setext 标题规则（`lheading`）。
///
/// 组 1 是标题文本，组 2 是 `=`/`-` 下划线。两条负向先行断言在每行开头排除列表、
/// 缩进代码、围栏、引用、ATX 标题、HTML 与表格起点，避免把普通段落误判成标题。
/// 因为同一别名在两条断言里各出现一次，这里用 `edit_all` 而不是 `edit_first`。
fn build_lheading_gfm() -> String {
    let mut source = concat!(
        r"^(?!bull |blockCode|fences|blockquote|heading|html|table)",
        r"((?:.|\n(?!\s*?\n|bull |blockCode|fences|blockquote|heading|html|table))+?)",
        r"\n {0,3}(=+|-+) *(?:\n+|$)",
    )
    .to_string();
    source = edit_all(&source, "bull", BULLET);
    source = edit_all(&source, "blockCode", r"(?: {4}| {0,3}\t)");
    source = edit_all(&source, "fences", r" {0,3}(?:`{3,}|~{3,})");
    source = edit_all(&source, "blockquote", r" {0,3}>");
    source = edit_all(&source, "heading", r" {0,3}#{1,6}(?:\s|$)");
    source = edit_all(&source, "html", r" {0,3}<[^\n>]+>\n");
    source = edit_all(&source, "table", r" {0,3}\|?(?:[:\- ]*\|)+[\:\- ]*\n");
    source
}

/// 构造块级 HTML 规则（`html`）。
///
/// 整体加 `(?i)`；第一个分支捕获 `script|pre|style|textarea` 的标签名，结尾用 `\1`
/// 反向引用要求闭合标签同名（`fancy_regex` 才支持的语法）。最后再替换
/// `comment`/`tag`/`attribute` 三个别名。组 1 会被块级 `html` tokenizer 用来设置
/// `pre` 标记。
fn build_block_html() -> String {
    let mut source = String::from("^ {0,3}(?:");
    source.push_str(r"<(script|pre|style|textarea)[\s>][\s\S]*?(?:</\1>[^\n]*\n*|$)");
    source.push_str(r"|comment[^\n]*(\n+|$)");
    source.push_str(r"|<\?[\s\S]*?(?:\?>[^\n]*\n*|$)");
    source.push_str(r"|<![A-Z][\s\S]*?(?:>[^\n]*\n*|$)");
    source.push_str(r"|<!\[CDATA\[[\s\S]*?(?:\]\]>[^\n]*\n*|$)");
    source.push_str(r"|</?(tag)(?: +|\n|/?>)[\s\S]*?(?:(?:\n[ \t]*)+\n|$)");
    source.push_str(r"|<(?!script|pre|style|textarea)([a-z][\w-]*)(?:attribute)*? */?>(?=[ \t]*(?:\n|$))[\s\S]*?(?:(?:\n[ \t]*)+\n|$)");
    source.push_str(r"|</(?!script|pre|style|textarea)[a-z][\w-]*\s*>(?=[ \t]*(?:\n|$))[\s\S]*?(?:(?:\n[ \t]*)+\n|$)");
    source.push(')');
    let mut source = format!("(?i){source}");
    source = edit_first(&source, "comment", COMMENT);
    source = edit_first(&source, "tag", TAG_LIST);
    source = edit_first(&source, "attribute", ATTRIBUTE_BLOCK);
    source
}

/// 构造行内 text 规则（`inline_text`）：在强调、链接、裸 URL 等标记前停下。
///
/// 两个别名替换：`\b_` → lookbehind 形式 `(?<![A-Za-z0-9_])_`；`protocol` →
/// [`CASE_INSENSITIVE_PROTOCOL`]。组 1 是起始字符，后续分支在 `~`/反引号、硬换行
/// （两个以上空格加换行）、邮箱地址以及强调/链接标记之前停止。
fn build_inline_text() -> String {
    let mut source = concat!(
        r"^(`+|~+|[^`~])(?:(?=[`~])|(?= {2,}\n)|(?=[a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-]+@)|",
        r"[\s\S]*?(?:(?=[\\<!\[`*~_]|\b_|protocol:\/\/|www\.|$)|[^ ](?= {2,}\n)|",
        r"[^a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-](?=[a-zA-Z0-9.!#$%&'*+\/=?_`{\|}~-]+@)))",
    )
    .to_string();
    source = edit_first(&source, r"\b_", r"(?<![A-Za-z0-9_])_");
    source = edit_first(&source, "protocol", CASE_INSENSITIVE_PROTOCOL);
    source
}

/// 构造强调起始定界符规则（`emStrongLDelim`），星号与下划线两个分支。
///
/// `punct` 别名展开成调用方给的那一套标点类（GFM 或非 GFM）。
fn build_em_strong_l_delim(punct: &str) -> String {
    let mut source =
        r"^(?:\*+(?:((?!\*)punct)|([^\s*]))?)|^_+(?:((?!_)punct)|([^\s_]))?".to_string();
    source = edit_all(&source, "punct", punct);
    source
}

/// 构造星号强调的结束定界符规则（`emStrongRDelimAst`）。
///
/// 8 个分支依次尝试：先处理 `__`/`*` 混合的已配对情形，再按“标点 / 空白 / 非空白”
/// 三类邻接关系判断一段 `*` 是否满足 CommonMark 的左右侧定界符条件。三个别名
/// （punct、punctSpace、notPunctSpace）都要展开；GFM 变体对 `~` 有特殊处理。
fn build_em_strong_r_delim_ast(punct: &str, punct_space: &str, not_punct_space: &str) -> String {
    let mut source = concat!(
        r"^[^_*]*?__[^_*]*?\*[^_*]*?(?=__)",
        r"|[^*]+(?=[^*])",
        r"|(?!\*)punct(\*+)(?=[\s]|$)",
        r"|notPunctSpace(\*+)(?!\*)(?=punctSpace|$)",
        r"|(?!\*)punctSpace(\*+)(?=notPunctSpace)",
        r"|[\s](\*+)(?!\*)(?=punct)",
        r"|(?!\*)punct(\*+)(?!\*)(?=punct)",
        r"|notPunctSpace(\*+)(?=notPunctSpace)",
    )
    .to_string();
    source = edit_all(&source, "notPunctSpace", not_punct_space);
    source = edit_all(&source, "punctSpace", punct_space);
    source = edit_all(&source, "punct", punct);
    source
}

/// 构造下划线强调的结束定界符规则（`emStrongRDelimUnd`）。
///
/// 与星号版结构对称，但使用非 GFM 的三套标点类（下划线强调不应用 GFM 的 `~` 特例），
/// 且比星号版少最后一个 `notPunctSpace(_+)(?=notPunctSpace)` 分支。
fn build_em_strong_r_delim_und() -> String {
    let mut source = concat!(
        r"^[^_*]*?\*\*[^_*]*?_[^_*]*?(?=\*\*)",
        r"|[^_]+(?=[^_])",
        r"|(?!_)punct(_+)(?=[\s]|$)",
        r"|notPunctSpace(_+)(?!_)(?=punctSpace|$)",
        r"|(?!_)punctSpace(_+)(?=notPunctSpace)",
        r"|[\s](_+)(?!_)(?=punct)",
        r"|(?!_)punct(_+)(?!_)(?=punct)",
    )
    .to_string();
    source = edit_all(&source, "notPunctSpace", NOT_PUNCTUATION_OR_SPACE_CLASS);
    source = edit_all(&source, "punctSpace", PUNCTUATION_OR_SPACE_CLASS);
    source = edit_all(&source, "punct", PUNCTUATION_CLASS);
    source
}

/// 构造行内链接/图片规则（`link`）。
///
/// 组 1=label，组 2=href，组 3=title；行内 `link` tokenizer 按这三个组读取，改规则时
/// 不能打乱分组。href 允许尖括号包裹或裸串，title 允许双引号/单引号/圆括号三种形式。
fn build_link() -> String {
    let mut source =
        r"^!?\[(label)\]\(\s*(href)(?:(?:[ \t]+(?:\n[ \t]*)?|\n[ \t]*)(title))?\s*\)".to_string();
    source = edit_first(&source, "label", INLINE_LABEL);
    source = edit_first(
        &source,
        "href",
        r"<(?:\\.|[^\n<>\\])+>|[^ \t\n\x00-\x1f]+|(?=\))",
    );
    source = edit_first(
        &source,
        "title",
        r#""(?:\\"?|[^"\\])*"|'(?:\\'?|[^'\\])*'|\((?:\\\)?|[^)\\])*\)"#,
    );
    source
}

/// 构造引用式链接规则（`reflink`）：`[label][ref]`。
///
/// 组 1=label，组 2=引用标签；行内 `reflink` 用组 2（为空时退回组 1）查链接定义表。
fn build_reflink() -> String {
    let mut source = r"^!?\[(label)\]\[(ref)\]".to_string();
    source = edit_first(&source, "label", INLINE_LABEL);
    source = edit_first(&source, "ref", BLOCK_LABEL);
    source
}

/// 构造折叠引用规则（`nolink`）：`[label]` 或 `[label][]`。
///
/// 组 1 既是 label 也是引用标签。
fn build_nolink() -> String {
    let mut source = r"^!?\[(ref)\](?:\[\])?".to_string();
    source = edit_first(&source, "ref", BLOCK_LABEL);
    source
}

/// 合成引用式链接的搜索规则（`reflinkSearch`）：`reflink|nolink(?!\()`。
///
/// 用于行内掩码（`mask_reflinks`）与 `linkInText` 探测。两个分支的先后顺序有意义：
/// 交替从左到右尝试，`reflink` 必须在 `nolink` 之前，否则 `[label][ref]` 会先被
/// `nolink` 只吃掉前半段；`nolink` 后的 `(?!\()` 排除紧随 `(` 的 `[label]`（那是行内
/// 链接的 label，不是引用）。
fn build_reflink_search() -> String {
    let reflink = build_reflink();
    let nolink = build_nolink();
    let mut source = "reflink|nolink(?!\\()".to_string();
    source = edit_first(&source, "reflink", &reflink);
    source = edit_first(&source, "nolink", &nolink);
    source
}

/// 构造行内 HTML 标签规则（`tag`）：注释、结束标签、开始标签、处理指令、声明、
/// CDATA 六个分支；注释与属性串由别名展开。
fn build_tag() -> String {
    let mut source = concat!(
        r"^comment",
        r"|^</[a-zA-Z][\w:-]*\s*>",
        r"|^<[a-zA-Z][\w-]*(?:attribute)*?\s*/?>",
        r"|^<\?[\s\S]*?\?>",
        r"|^<![a-zA-Z]+\s[\s\S]*?>",
        r"|^<!\[CDATA\[[\s\S]*?\]\]>",
    )
    .to_string();
    source = edit_first(&source, "comment", INLINE_COMMENT);
    source = edit_first(&source, "attribute", ATTRIBUTE_INLINE);
    source
}

/// 构造尖括号自动链接规则（`autolink`）：`<scheme:...>` 与 `<email>`。
///
/// 组 1 是链接文本（协议串或邮箱），组 2 是邮箱形式的 `(@)` 标记；行内 `autolink`
/// 据此决定是否加 `mailto:` 前缀。协议名长度限制为 2–32 个字符。
fn build_autolink() -> String {
    let mut source = r"^<(scheme:[^\s\x00-\x1f<>]*|email)>".to_string();
    source = edit_first(&source, "scheme", r"[a-zA-Z][a-zA-Z0-9+.-]{1,31}");
    source = edit_first(&source, "email", INLINE_EMAIL);
    source
}

/// 构造 GFM 裸 URL / 裸邮箱规则（`url`）。
///
/// 组 1 是 `protocol://` 或 `www.` 前缀（裸邮箱分支不参与匹配，读取处允许缺失），
/// 组 2 是邮箱分支的 `(@)` 标记，用来区分 `mailto:`。裸 URL 的结尾标点由
/// `backpedal` 规则回退，`inline.rs::url` 依赖这一点。
fn build_url() -> String {
    let mut source = r"^((?:protocol):\/\/|www\.)(?:[a-zA-Z0-9\-]+\.?)+[^\s<]*|^email".to_string();
    source = edit_first(&source, "protocol", CASE_INSENSITIVE_PROTOCOL);
    source = edit_first(&source, "email", URL_EMAIL);
    source
}

/// 构造行内扫描用的掩码规则（`blockSkip`）：链接、代码、HTML 三块的并集。
///
/// `precode-code` 分两步展开：先把 `precode-` 替换成 ``(?<!`)()``，再把 `code` 替换成
/// [`BLOCK_SKIP_CODE`]，于是代码 span 分支带上前置的“前一个字符不是反引号”保护。
/// 命名组 `a`/`b` 让同一段反引号串首尾配对。
fn build_block_skip() -> String {
    let mut source = "link|precode-code|html".to_string();
    source = edit_first(&source, "link", BLOCK_SKIP_LINK);
    source = edit_first(&source, "precode-", r"(?<!`)()");
    source = edit_first(&source, "code", BLOCK_SKIP_CODE);
    source = edit_first(&source, "html", BLOCK_SKIP_HTML);
    source
}

/// 编译后的全部规则。
///
/// 按用途分为 `other`（块循环之外的辅助替换）、`block`、`inline`、CJK 变体，以及
/// `cachedIndentRegex` 对应的 6 组 `[Re; 4]`。数组下标由 `block.rs` 的
/// `cached_indent_index(indent) = clamp(indent - 1, 0, 3)` 选出：下标 0 覆盖 indent ≤ 1，
/// 下标 3 覆盖 indent ≥ 4，每个下标 i 表示“最多 i 个前导空格”的那份变体。
///
/// 字段都是 `pub(crate)`，块级/行内 tokenizer 直接借用 `self.rules`；构建完成后不再
/// 修改，因此可以安全地放在 `OnceLock` 里共享。
pub(crate) struct Rules {
    // `other`
    pub code_remove_indent: Re,
    pub output_link_replace: Re,
    pub indent_code_compensation: Re,
    pub beginning_space: Re,
    pub blank_line: Re,
    pub double_blank_line: Re,
    pub blockquote_start: Re,
    pub blockquote_setext_replace: Re,
    pub blockquote_setext_replace2: Re,
    pub list_is_task: Re,
    pub list_replace_task: Re,
    pub list_task_checkbox: Re,
    pub any_line: Re,
    pub href_brackets: Re,
    pub table_delimiter: Re,
    pub table_align_chars: Re,
    pub table_row_blank_line: Re,
    pub table_align_right: Re,
    pub table_align_center: Re,
    pub table_align_left: Re,
    pub start_a_tag: Re,
    pub end_a_tag: Re,
    pub start_pre_script_tag: Re,
    pub end_pre_script_tag: Re,
    pub start_angle_bracket: Re,
    pub end_angle_bracket: Re,
    pub unicode_alpha_numeric: Re,
    pub multiple_space_global: Re,
    pub tab_char_global: Re,
    pub new_line_char_global: Re,
    pub non_space_char: Re,
    pub ending_hash: Re,
    pub ending_space_char: Re,
    pub starting_space_char: Re,
    // block
    pub newline: Re,
    pub block_code: Re,
    pub fences: Re,
    pub hr: Re,
    pub heading: Re,
    pub blockquote: Re,
    pub list: Re,
    pub html: Re,
    pub def: Re,
    pub table: Re,
    pub lheading: Re,
    pub paragraph: Re,
    pub block_text: Re,
    // inline
    pub escape: Re,
    pub inline_code: Re,
    pub br: Re,
    pub inline_text: Re,
    pub punctuation: Re,
    pub em_strong_l_delim: Re,
    pub em_strong_r_delim_ast: Re,
    pub em_strong_r_delim_und: Re,
    pub strict_del: Re,
    pub any_punctuation: Re,
    pub autolink: Re,
    pub tag: Re,
    pub link: Re,
    pub reflink: Re,
    pub nolink: Re,
    pub reflink_search: Re,
    pub url: Re,
    pub backpedal: Re,
    pub block_skip: Re,
    // CJK（marked-cjk-friendly）
    pub cjk_test: Re,
    pub punctuation_cjk: Re,
    pub em_strong_r_delim_ast_cjk: Re,
    pub em_strong_r_delim_und_cjk: Re,
    // 缩进相关（cachedIndentRegex，0..3）
    pub next_bullet: [Re; 4],
    pub hr_indent: [Re; 4],
    pub fences_begin: [Re; 4],
    pub heading_begin: [Re; 4],
    pub html_begin: [Re; 4],
    pub blockquote_begin: [Re; 4],
}

/// marked-cjk-friendly 的 CJK 码点范围（逐字）。
///
/// 这是一段**字符类正文**（不含外层方括号），由 `cjk_class` 替换模板里的 `CJK`
/// 占位符；U+FFFF 以内的码点写成 `\uXXXX`，补充平面写成 `\u{...}`，两种写法都只在
/// `[...]` 内使用。
pub(crate) const CJK: &str = "\\u1100-\\u11ff\\u20a9\\u2329-\\u232a\\u2630-\\u2637\\u268a-\\u268f\\u2e80-\\u2e99\\u2e9b-\\u2ef3\\u2f00-\\u2fd5\\u2ff0-\\u303e\\u3041-\\u3096\\u3099-\\u30ff\\u3105-\\u312f\\u3131-\\u318e\\u3190-\\u31e5\\u31ef-\\u321e\\u3220-\\u3247\\u3250-\\ua48c\\ua490-\\ua4c6\\ua960-\\ua97c\\uac00-\\ud7a3\\ud7b0-\\ud7c6\\ud7cb-\\ud7fb\\uf900-\\ufaff\\ufe10-\\ufe19\\ufe30-\\ufe52\\ufe54-\\ufe66\\ufe68-\\ufe6b\\uff01-\\uffbe\\uffc2-\\uffc7\\uffca-\\uffcf\\uffd2-\\uffd7\\uffda-\\uffdc\\uffe0-\\uffe6\\uffe8-\\uffee\\u{16fe0}-\\u{16fe4}\\u{16ff0}-\\u{16ff6}\\u{17000}-\\u{18cda}\\u{18cff}-\\u{18d20}\\u{18d80}-\\u{18df2}\\u{18e00}-\\u{19191}\\u{191a0}-\\u{191d2}\\u{1aff0}-\\u{1aff3}\\u{1aff5}-\\u{1affb}\\u{1affd}-\\u{1affe}\\u{1b000}-\\u{1b128}\\u{1b132}\\u{1b150}-\\u{1b152}\\u{1b155}\\u{1b164}-\\u{1b168}\\u{1b170}-\\u{1b2fb}\\u{1d300}-\\u{1d356}\\u{1d360}-\\u{1d376}\\u{1f1ae}\\u{1f200}\\u{1f202}\\u{1f210}-\\u{1f219}\\u{1f21b}-\\u{1f22e}\\u{1f230}-\\u{1f231}\\u{1f237}\\u{1f23b}\\u{1f240}-\\u{1f248}\\u{1f260}-\\u{1f265}\\u{1f7da}\\u{20000}-\\u{3fffd}";

// CJK 版的标点/空白分类：在普通标点/空白类里并入 CJK 码点，供 marked-cjk-friendly
// 重写的 emStrong 规则使用。`CJK` 是占位符，编译前由 `cjk_class` 替换成 `CJK` 常量。
const CJK_PUNCT: &str = "[\\p{P}\\p{S}CJK]";
const CJK_PUNCT_SPACE: &str = "[\\s\\p{P}\\p{S}CJK]";
const CJK_NOT_PUNCT_SPACE: &str = "[^\\s\\p{P}\\p{S}CJK]";
// GFM 变体（`(?!~)` 与 `|~`，对应 marked-cjk-friendly 的 `*CjkGfm` 正则）。
const CJK_PUNCT_GFM: &str = "(?!~)[\\p{P}\\p{S}CJK]";
const CJK_PUNCT_SPACE_GFM: &str = "(?!~)[\\s\\p{P}\\p{S}CJK]";
const CJK_NOT_PUNCT_SPACE_GFM: &str = "(?:[^\\s\\p{P}\\p{S}CJK]|~)";

/// 把模板里的 `CJK` 占位符替换成 [`CJK`] 常量（纯字符串替换，占位符只出现在字符类正文里）。
fn cjk_class(body: &str) -> String {
    body.replace("CJK", CJK)
}

/// 构建并编译全部规则（由 `rules()` 惰性调用一次）。
///
/// 先构造彼此依赖的组合规则（paragraph 引用 gfm_table；blockquote 引用剥掉 `^` 的
/// blockquote_paragraph），再逐条编译。任何一条规则非法都会在 `re` 里 panic，
/// 因此这里不做错误恢复。
fn init_rules() -> Rules {
    // 表格规则同时被顶层 paragraph 的“表格打断”判断使用，所以先算出来复用。
    let gfm_table = build_gfm_table();
    // 顶层有序列表只有 `1.`/`1)` 能打断段落（GFM 行为）；blockquote 内的规则不同。
    let paragraph = build_paragraph(r" {0,3}(?:[*+-]|1[.)])[ \t]+[^ \t\n]", &gfm_table);
    let blockquote_paragraph = build_blockquote_paragraph();

    let lheading = build_lheading_gfm();

    Rules {
        code_remove_indent: re(r"(?m)^(?: {1,4}| {0,3}\t)"),
        output_link_replace: re(r"\\([\[\]])"),
        indent_code_compensation: re(r"^(\s+)(?:```)"),
        beginning_space: re(r"^\s+"),
        blank_line: re(r"^[ \t]*$"),
        double_blank_line: re(r"\n[ \t]*\n[ \t]*$"),
        blockquote_start: re(r"^ {0,3}>"),
        blockquote_setext_replace: re(r"\n {0,3}((?:=+|-+) *)(?=\n|$)"),
        blockquote_setext_replace2: re(r"(?m)^ {0,3}>[ \t]?"),
        list_is_task: re(r"^\[[ xX]\] +\S"),
        list_replace_task: re(r"^\[[ xX]\] +"),
        list_task_checkbox: re(r"\[[ xX]\]"),
        any_line: re(r"\n[^\n\r\u{2028}\u{2029}]*\n"),
        href_brackets: re(r"^<(.*)>$"),
        table_delimiter: re(r"[:|]"),
        table_align_chars: re(r"^\||\| *$"),
        table_row_blank_line: re(r"\n[ \t]*$"),
        table_align_right: re(r"^ *-+: *$"),
        table_align_center: re(r"^ *:-+: *$"),
        table_align_left: re(r"^ *:-+ *$"),
        start_a_tag: re(r"(?i)^<a "),
        end_a_tag: re(r"(?i)^</a>"),
        start_pre_script_tag: re(r"(?i)^<(pre|code|kbd|script)(\s|>)"),
        end_pre_script_tag: re(r"(?i)^</(pre|code|kbd|script)(\s|>)"),
        start_angle_bracket: re(r"^<"),
        end_angle_bracket: re(r">$"),
        unicode_alpha_numeric: re(r"[\p{L}\p{N}]"),
        multiple_space_global: re(r"\s+"),
        tab_char_global: re(r"\t"),
        new_line_char_global: re(r"\n"),
        non_space_char: re(r"[^ ]"),
        ending_hash: re(r"#$"),
        ending_space_char: re(r" $"),
        starting_space_char: re(r"^ "),
        newline: re(r"^(?:[ \t]*(?:\n|$))+"),
        block_code: re(r"^((?: {4}| {0,3}\t)[^\n]+(?:\n(?:[ \t]*(?:\n|$))*)?)+"),
        fences: re(concat!(
            r"^ {0,3}(`{3,}(?=[^`\n]*(?:\n|$))|~{3,})([^\n]*)(?:\n|$)",
            r"(?:|([\s\S]*?)(?:\n|$))(?: {0,3}\1[~`]* *(?=\n|$)|$)",
        )),
        hr: re(HR),
        heading: re(r"^ {0,3}(#{1,6})(?=\s|$)(.*)(?:\n+|$)"),
        // blockquote 把 blockquote 版段落规则内联进每一行；`strip_carets` 去掉片段开头
        // 的 `^`，否则它只会在整个规则的最前面生效。
        blockquote: re(&format!(
            "^( {{0,3}}> ?({}|[^\\n]*)(?:\\n|$))+",
            strip_carets(&blockquote_paragraph)
        )),
        // `^(bull)(...)(?:\n|$)` 里的 `bull` 是别名；全局替换成 [`BULLET`]。
        list: re(&edit_all(r"^(bull)([ \t][^\n]*?)?(?:\n|$)", "bull", BULLET)),
        html: re(&build_block_html()),
        // 链接定义：组 1=label，组 2=href，组 3=title；只有标签与标题用别名展开。
        def: re(&{
            let mut source = r"^ {0,3}\[(label)\]: *(?:\n[ \t]*)?([^<\s][^\s]*|<.*?>)(?:(?: +(?:\n[ \t]*)?| *\n[ \t]*)(title))? *(?:\n+|$)".to_string();
            source = edit_first(&source, "label", BLOCK_LABEL);
            source = edit_first(&source, "title", TITLE);
            source
        }),
        table: re(&gfm_table),
        lheading: re(&lheading),
        paragraph: re(&paragraph),
        block_text: re(r"^[^\n]+"),
        escape: re(r##"^\\([!"#$%&'()*+,\-./:;<=>?@\[\]\\^_`{|}~])"##),
        inline_code: re(r"^(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)"),
        br: re(r"^( {2,}|\\)\n(?!\s*$)"),
        inline_text: re(&build_inline_text()),
        punctuation: re(r"^((?![*_])[\s\p{P}\p{S}])"),
        em_strong_l_delim: re(&build_em_strong_l_delim(PUNCTUATION_GFM)),
        em_strong_r_delim_ast: re(&build_em_strong_r_delim_ast(
            PUNCTUATION_GFM,
            PUNCTUATION_OR_SPACE_GFM,
            NOT_PUNCTUATION_OR_SPACE_GFM,
        )),
        em_strong_r_delim_und: re(&build_em_strong_r_delim_und()),
        strict_del: re(r"^(~~)(?=[^\s~])((?:\\.|[^\\])*?(?:\\.|[^\s~\\]))\1(?=[^~]|$)"),
        any_punctuation: re(r"\\([\p{P}\p{S}])"),
        autolink: re(&build_autolink()),
        tag: re(&build_tag()),
        link: re(&build_link()),
        reflink: re(&build_reflink()),
        nolink: re(&build_nolink()),
        reflink_search: re(&build_reflink_search()),
        url: re(&build_url()),
        backpedal: re(
            r#"(?:[^?!.,:;*_'"~()&]+|\([^)]*\)|&(?![a-zA-Z0-9]+;$)|[?!.,:;*_'"~)]+(?!$))+"#,
        ),
        block_skip: re(&build_block_skip()),
        cjk_test: re(&format!("[{}]", CJK)),
        punctuation_cjk: re(&cjk_class("^((?![*_])[\\s\\p{P}\\p{S}CJK])")),
        em_strong_r_delim_ast_cjk: re(&cjk_build_ast()),
        em_strong_r_delim_und_cjk: re(&cjk_build_und()),
        // cachedIndentRegex：marked 按缩进缓存 0..3 四份规则。数组下标 i 对应
        // “最多 i 个前导空格”（`^ {0,i}`）的那一份；调用方的下标换算见 `Rules` 文档。
        next_bullet: std::array::from_fn(|i| {
            re(&format!(
                r"^ {{0,{i}}}(?:[*+-]|\d{{1,9}}[.)])((?:[ \t][^\n]*)?(?:\n|$))"
            ))
        }),
        hr_indent: std::array::from_fn(|i| {
            re(&format!(
                r"^ {{0,{i}}}((?:- *){{3,}}|(?:_ *){{3,}}|(?:\* *){{3,}})(?:\n+|$)"
            ))
        }),
        fences_begin: std::array::from_fn(|i| re(&format!(r"^ {{0,{i}}}(?:```|~~~)"))),
        heading_begin: std::array::from_fn(|i| re(&format!(r"^ {{0,{i}}}#"))),
        html_begin: std::array::from_fn(|i| re(&format!(r"(?i)^ {{0,{i}}}<(?:[a-z].*>|!--)"))),
        blockquote_begin: std::array::from_fn(|i| re(&format!(r"^ {{0,{i}}}>"))),
    }
}

/// 构造 CJK 友好的星号结束定界符规则（`emStrongRDelimAstCjkGfm`）。
///
/// 结构与非 CJK 版相同，但三个占位符 `PUNCT`/`PUNCTSPACE`/`NOT` 换成并入 CJK 码点的
/// GFM 类。替换顺序必须是 `PUNCTSPACE` → `PUNCT` → `NOT`：先替换短名 `PUNCT` 会把
/// `PUNCTSPACE` 的前半截改掉，导致后续找不到占位符。
fn cjk_build_ast() -> String {
    // marked-cjk-friendly 在 GFM 下使用 `emStrongRDelimAstCjkGfm`。
    let mut source = concat!(
        r"^[^_*]*?__[^_*]*?\*[^_*]*?(?=__)",
        r"|[^*]+(?=[^*])",
        r"|(?!\*)PUNCT(\*+)(?=[\s]|$)",
        r"|NOT(\*+)(?!\*)(?=PUNCTSPACE|$)",
        r"|(?!\*)PUNCTSPACE(\*+)(?=NOT)",
        r"|[\s](\*+)(?!\*)(?=PUNCT)",
        r"|(?!\*)PUNCT(\*+)(?!\*)(?=PUNCT)",
        r"|NOT(\*+)(?=NOT)",
    )
    .to_string();
    let punct_space = cjk_class(CJK_PUNCT_SPACE_GFM);
    let punct = cjk_class(CJK_PUNCT_GFM);
    let not_punct_space = cjk_class(CJK_NOT_PUNCT_SPACE_GFM);
    source = source.replace("PUNCTSPACE", &punct_space);
    source = source.replace("PUNCT", &punct);
    source = source.replace("NOT", &not_punct_space);
    source
}

/// 构造 CJK 友好的下划线结束定界符规则（`emStrongRDelimUndCjk`）。
///
/// 与 `cjk_build_ast` 同构，使用非 GFM 的三套 CJK 类；占位符替换顺序同样必须先
/// `PUNCTSPACE` 后 `PUNCT`。
fn cjk_build_und() -> String {
    let mut source = concat!(
        r"^[^_*]*?\*\*[^_*]*?_[^_*]*?(?=\*\*)",
        r"|[^_]+(?=[^_])",
        r"|(?!_)PUNCT(_+)(?=[\s]|$)",
        r"|NOT(_+)(?!_)(?=PUNCTSPACE|$)",
        r"|(?!_)PUNCTSPACE(_+)(?=NOT)",
        r"|[\s](_+)(?!_)(?=PUNCT)",
        r"|(?!_)PUNCT(_+)(?!_)(?=PUNCT)",
    )
    .to_string();
    source = source.replace("PUNCTSPACE", &cjk_class(CJK_PUNCT_SPACE));
    source = source.replace("PUNCT", &cjk_class(CJK_PUNCT));
    source = source.replace("NOT", &cjk_class(CJK_NOT_PUNCT_SPACE));
    source
}

/// 全局规则表（进程内只编译一次）。
///
/// 首次调用执行 `init_rules`，编译全部规则；之后所有 [`Lexer`] 共享同一个
/// `&'static Rules`，不再付出编译成本。`OnceLock` 保证初始化只执行一次，初始化期间
/// 的 panic（`re` 里的编译失败）会传播给首个调用方。
pub(crate) fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(init_rules)
}

// ---------------------------------------------------------------------------
// UTF-16 工具（TS 的字符串索引单位）
// ---------------------------------------------------------------------------

/// 字符串的 UTF-16 码元数（JS `str.length`）。
///
/// BMP 字符计 1 个码元，astral（补充平面）字符计 2 个。规则匹配出的下标、`raw.length`
/// 推进和掩码长度都以这个单位为准，不能用 `str::len()`（UTF-8 字节）或
/// `chars().count()`（码点）。
pub(crate) fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// 把 UTF-16 码元下标换成 UTF-8 字节下标（越界时夹到末尾）。
///
/// 返回值一定落在 `char` 边界上：如果 `units` 落在某个 astral 字符的两个码元中间
/// （Rust `str` 无法表示这种位置），返回该字符的起始字节，相当于向下取整到码点边界。
/// 所有“JS 下标 → Rust 切片”的转换都要经过这里，调用方拿它切串不会 panic。
pub(crate) fn byte_at_utf16(text: &str, units: usize) -> usize {
    let mut remaining = units;
    for (byte_index, ch) in text.char_indices() {
        let len = ch.len_utf16();
        if remaining < len {
            return byte_index;
        }
        remaining -= len;
    }
    text.len()
}

/// `text.slice(0, unit_count)`。
///
/// 按 UTF-16 码元截取前缀；`unit_count` 越界或落在代理对中间时按 [`byte_at_utf16`]
/// 的规则夹到合法边界，不会 panic。
pub(crate) fn slice_utf16(text: &str, unit_count: usize) -> &str {
    &text[..byte_at_utf16(text, unit_count)]
}

/// `text.slice(-1 * src.length + offset)` 的等价物（按 UTF-16 码元）。
///
/// 参数是“从末尾往回数的码元数”；超过全长时 `saturating_sub` 把起点夹到 0，于是返回
/// 整个字符串（与 JS 负下标越界的表现一致）。
pub(crate) fn slice_from_utf16(text: &str, units_from_end: usize) -> &str {
    let len = utf16_len(text);
    let start = len.saturating_sub(units_from_end);
    &text[byte_at_utf16(text, start)..]
}

/// `src.substring(token.raw.length)`。
///
/// marked 在块循环里用 `token.raw` 的长度推进剩余源码；`raw` 可能是重建出来的（例如
/// blockquote/list 续行/懒续行），长度可能**超过**剩余源码。JS 的 `substring` 会把越界
/// 下界夹到字符串长度，而 Rust 直接按字节切片会 panic。这里按 UTF-16 码元推进（与 JS
/// 一致），越界时夹到末尾；[`byte_at_utf16`] 保证结果落在字符边界上。
///
/// 块级/行内主循环每消费一个 token 都调用它；不要换成 `&src[raw.len()..]`——字节长度
/// 与 UTF-16 长度不同，而且在 raw 超过剩余源码时会 panic。
pub(crate) fn substring_after_raw<'a>(src: &'a str, raw: &str) -> &'a str {
    &src[byte_at_utf16(src, utf16_len(raw))..]
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

/// 链接定义（`Links` 的值）。
///
/// 键是规范化后的标签（小写、连续空白折叠为单个空格），块级 `definition` 登记时计算，
/// 行内 `reflink` 用同样的规范化结果查表。`title` 为 `None` 表示定义里没有标题。
#[derive(Debug, Clone, PartialEq)]
pub struct LinkDef {
    /// 链接目标（已去掉尖括号并处理转义）。
    pub href: String,
    /// 链接标题；`None` 表示定义未给出标题。
    pub title: Option<String>,
}

/// 行内 token 队列的一项。
///
/// `id` 是它在 `Lexer::results` 中的下标；`src` 是稍后要行内 token 化的文本。
/// 合并块级 token 时用 `update_last_inline_src` 改写最后一项的 `src`；任务列表第二遍
/// 也会就地改写它（去掉 `[ ] ` 前缀）。
#[derive(Debug, Clone)]
pub(crate) struct InlineQueueItem {
    /// `results` 中的槽位下标。
    pub id: usize,
    /// 待行内 token 化的源文本。
    pub src: String,
}

/// 词法分析状态（`Lexer#state`）。
///
/// 四个标志中只有 `top` 由块级代码设置，其余由行内 tokenizer 读写（见
/// `markdown::inline`）：
/// - `in_link`：正在解析链接文字（`output_link` 递归期间）或位于 `<a …>` 与 `</a>`
///   之间；`url` 只在 `!in_link` 时尝试，避免把链接文字里的裸 URL 再包一层链接；
/// - `in_raw_block`：位于 `<pre>/<code>/<kbd>/<script>` 内，决定 text token 的 `escaped`；
/// - `link_emitted`：链接文字扫描期间是否已产出一个非图片链接，用于拒绝嵌套链接
///   （图片不置位，嵌套判定也豁免图片）；
/// - `top`：是否处于顶层块扫描；为 `false` 时 `block_tokens` 跳过 paragraph 分支，
///   普通行通常落成 text token（其它 tokenizer 不受影响）。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LexerState {
    pub in_link: bool,
    pub in_raw_block: bool,
    pub link_emitted: bool,
    pub top: bool,
}

/// marked 的 `_Lexer`。
///
/// 一个实例保存**一份文档**的解析中间状态：编译好的规则表引用、块/行内状态标志、
/// 链接定义表、待处理的行内队列，以及按 id 存放的行内结果。典型用法是
/// `Lexer::new().lex(src)`（见 [`lex_markdown`]）。
///
/// `lex` 不会重置 `links`/`results`，所以不要把同一实例复用于两份互不相关的文档；
/// 仓库内调用方都每份文档新建一个实例。
pub struct Lexer {
    /// 进程级共享的编译后规则表（`rules()`）。
    pub(crate) rules: &'static Rules,
    /// 跨 token 的分析状态；块级只改 `top`，行内 tokenizer 改其余三个。
    pub(crate) state: LexerState,
    /// 文档内已登记的链接定义；块级 `definition` 写入，行内 `reflink` 查询。
    pub(crate) links: IndexMap<String, LinkDef>,
    /// 待行内 token 化的队列；块级扫描期间累积，`lex` 末尾消费。
    pub(crate) inline_queue: Vec<InlineQueueItem>,
    /// 每个队列 id 对应的行内 token 结果；`None` 表示尚未填充。
    pub(crate) results: Vec<Option<Vec<Token>>>,
}

impl Lexer {
    /// 新建一个空的 lexer：`top = true`（顶层块扫描），链接表与队列为空。
    ///
    /// 规则表通过 `rules()` 共享；除首次调用触发 `rules()` 编译全部规则外，构造只分配
    /// 几个空容器。
    pub fn new() -> Self {
        Lexer {
            rules: rules(),
            state: LexerState {
                in_link: false,
                in_raw_block: false,
                link_emitted: false,
                top: true,
            },
            links: IndexMap::new(),
            inline_queue: Vec::new(),
            results: Vec::new(),
        }
    }

    /// `new Marked().lexer(src)`：预处理 + 块级 + 行内队列 + 回填。
    ///
    /// 输入任意 markdown 源文本，返回顶层块级 token；行内 token 已经填进对应 token 的
    /// `tokens` 字段。副作用：更新 `self.links`（链接定义）、`self.state` 与
    /// `self.results`。
    ///
    /// 注意：
    /// - CRLF 归一化在这里完成，`block_tokens` 及之后只看到 `\n`；
    /// - 项目 TS 侧 `components/markdown.ts` 的流式未闭合围栏修剪
    ///   （`trim_partial_closing_fences`）**不在**这里，由调用方在 lex 之后对结果再做一遍
    ///   （`crate::components::markdown` 就是这么做的）；
    /// - 开始时不清理 `links`/`results`，复用同一实例会继承上一份文档的链接定义。
    pub fn lex(&mut self, src: &str) -> Vec<Token> {
        let normalized = normalize_carriage_returns(src);
        let mut tokens: Vec<Token> = Vec::new();
        self.block_tokens(&normalized, &mut tokens, false);

        // 块级阶段只登记了 (id, src)；这里按登记顺序做行内解析。`inline_tokens` 本身
        // 不再登记新队列项，所以处理快照即可。
        let queue = std::mem::take(&mut self.inline_queue);
        for item in &queue {
            let produced = self.inline_tokens(&item.src);
            if let Some(slot) = self.results.get_mut(item.id) {
                *slot = Some(produced);
            }
        }
        lexer_fill_inline_tokens(&mut tokens, &self.results);
        tokens
    }

    /// `Lexer.inline(src)`：登记一个稍后处理的行内 token 列表。
    ///
    /// 返回值是队列 id，同时是 `results` 的下标；此刻 `results[id]` 为 `None`，等
    /// `lex` 处理队列时填充。块级 tokenizer（heading/lheading/paragraph/text、表格
    /// 单元格）只登记不解析，因此文档后部出现的链接定义也能被前部的引用式链接看到。
    pub(crate) fn inline(&mut self, src: &str) -> usize {
        let id = self.results.len();
        self.results.push(None);
        self.inline_queue.push(InlineQueueItem {
            id,
            src: src.to_string(),
        });
        id
    }

    /// 更新“最后一个行内队列项”的源文本（合并块级 text/paragraph/token 时用）。
    ///
    /// 合并后 token 的文本已不是原单项的文本，但队列里仍只有原先登记的那一项，
    /// 所以必须把 `src` 换成合并结果，否则行内解析会使用过期文本。
    pub(crate) fn update_last_inline_src(&mut self, src: &str) {
        if let Some(last) = self.inline_queue.last_mut() {
            last.src = src.to_string();
        }
    }

    /// 丢弃最后一个行内队列项（`inlineQueue.pop()`）。
    ///
    /// 用于 text/paragraph 合并：新 token 在 tokenizer 里刚登记的 `inline()` 被丢弃，
    /// 只保留合并后的一个队列项；对应 `results` 槽位保持 `None`（不再有 token 引用它）。
    pub(crate) fn pop_inline_queue(&mut self) {
        self.inline_queue.pop();
    }

    /// `blockTokens`：块级 tokenizer 主循环。
    ///
    /// `src` 是待消费的源码，生成的 token 追加到 `tokens`；`last_paragraph_clipped`
    /// 表示若本段产出的首个 paragraph 应与上一个 `Paragraph` token 合并。顶层扫描由上一轮
    /// 的 `cut_src.len() != src.len()`（是否被 latex 扩展起点裁剪）设置；blockquote
    /// 续行直接传 `true`；`list` 项解析与 `lex` 的顶层调用传 `false`。
    ///
    /// 每轮按 marked 的优先级依次尝试：扩展、space、code、fences、heading、hr、
    /// blockquote、list、html、def、table、lheading、paragraph、text；成功后用
    /// `substring_after_raw` 推进源码。循环上界保护要求剩余字节数严格减小，否则退出，
    /// 对应 marked 在同样情况下抛出的 “Infinite loop” 错误——这里选择不 panic，因为
    /// 输入来自模型/媒体的流式文本（与 marked 的 `silent` 分支一致）。
    ///
    /// 退出时把 `state.top` 置回 `true`，供后续顶层扫描使用。
    pub(crate) fn block_tokens(
        &mut self,
        src: &str,
        tokens: &mut Vec<Token>,
        mut last_paragraph_clipped: bool,
    ) {
        let mut src = src.to_string();
        let mut src_length = usize::MAX;
        while !src.is_empty() {
            // 进度保护：每次迭代都必须让剩余源码严格变短。若某个 tokenizer 返回空 raw
            // 或没有推进，下一轮长度不变，这里退出；marked 在同样位置抛错。
            let current_len = src.len();
            if current_len < src_length {
                src_length = current_len;
            } else {
                break;
            }

            // --- 扩展（latexBlock） ---
            // 扩展位优先于所有内建 tokenizer（对应 marked 先跑 `extensions.block` 的顺序）；
            // 返回 None 时继续走内建规则。
            if let Some(token) = self.tokenize_block_extension(&src) {
                src = substring_after_raw(&src, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // --- newline ---
            if let Some(token) = self.space(&src) {
                let raw = token.raw().to_string();
                src = substring_after_raw(&src, &raw).to_string();
                // marked：`token.raw.length === 1 && lastToken !== undefined`。长度是 UTF-16 码元数，
                // 所以末尾的单个空格/制表符也会被折进上一个 token 的 raw（并追加 "\n"）。
                let raw_is_single_unit = utf16_len(&raw) == 1;
                if raw_is_single_unit && !tokens.is_empty() {
                    if let Some(last) = tokens.last_mut() {
                        last_push_raw(last, "\n");
                    }
                } else {
                    tokens.push(token);
                }
                continue;
            }

            // --- code ---
            // 缩进代码块不能打断 paragraph/text：与上一个 token 合并，并把新增文本同步
            // 进它的行内队列源（行内解析尚未开始，合并后的文本才是要解析的内容）。
            if let Some(token) = self.code(&src) {
                let (raw, text) = match &token {
                    Token::Code { raw, text, .. } => (raw.clone(), text.clone()),
                    _ => unreachable!(),
                };
                src = substring_after_raw(&src, &raw).to_string();
                let merge = matches!(
                    tokens.last(),
                    Some(Token::Paragraph { .. }) | Some(Token::Text { .. })
                );
                if merge {
                    let last = tokens.last_mut().expect("checked");
                    let needs_newline = !last.raw().ends_with('\n');
                    if needs_newline {
                        last_push_raw(last, "\n");
                    }
                    last_push_raw(last, &raw);
                    if let Some(text_slot) = token_text_mut(last) {
                        text_slot.push('\n');
                        text_slot.push_str(&text);
                    }
                    let merged_text = token_text(last).unwrap_or("").to_string();
                    self.update_last_inline_src(&merged_text);
                } else {
                    tokens.push(token);
                }
                continue;
            }

            // --- fences ---
            if let Some(token) = self.fences(&src) {
                src = substring_after_raw(&src, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // --- heading ---
            if let Some(token) = self.heading(&src) {
                src = substring_after_raw(&src, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // --- hr ---
            if let Some(token) = self.hr(&src) {
                src = substring_after_raw(&src, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // --- blockquote ---
            if let Some(token) = self.blockquote(&src) {
                src = substring_after_raw(&src, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // --- list ---
            if let Some(token) = self.list(&src) {
                src = substring_after_raw(&src, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // --- html ---
            if let Some(token) = self.html(&src) {
                src = substring_after_raw(&src, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // --- def ---
            // 与 paragraph/text 相邻时并进段落文本；否则只有该标签首次出现才登记进
            // links 并产出 def token（重复定义被忽略，先出现者生效）。
            if let Some(token) = self.definition(&src) {
                let (raw, tag, href, title) = match &token {
                    Token::Def {
                        raw,
                        tag,
                        href,
                        title,
                    } => (raw.clone(), tag.clone(), href.clone(), title.clone()),
                    _ => unreachable!(),
                };
                src = substring_after_raw(&src, &raw).to_string();
                let merge = matches!(
                    tokens.last(),
                    Some(Token::Paragraph { .. }) | Some(Token::Text { .. })
                );
                if merge {
                    let last = tokens.last_mut().expect("checked");
                    if !last.raw().ends_with('\n') {
                        last_push_raw(last, "\n");
                    }
                    last_push_raw(last, &raw);
                    if let Some(text_slot) = token_text_mut(last) {
                        text_slot.push('\n');
                        text_slot.push_str(&raw);
                    }
                    let merged_text = token_text(last).unwrap_or("").to_string();
                    self.update_last_inline_src(&merged_text);
                } else if !self.links.contains_key(&tag) {
                    self.links.insert(tag, LinkDef { href, title });
                    tokens.push(token);
                }
                continue;
            }

            // --- table (gfm) ---
            if let Some(token) = self.table(&src) {
                src = substring_after_raw(&src, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // --- lheading ---
            if let Some(token) = self.lheading(&src) {
                src = substring_after_raw(&src, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // --- top-level paragraph（按扩展起点裁剪） ---
            // latex 扩展的 `start()` 给出 “$$/\\[” 之前的 UTF-16 下标；paragraph 只扫描到
            // 那里，剩余部分留给下一轮（块级扩展或 text）。`last_paragraph_clipped`
            // 记录本次是否裁剪，供随后的 paragraph 决定是否并回上一个 token。
            let mut cut_src = src.clone();
            let first_units = js_slice_from_1_units(&src);
            let sliced = js_slice_from_1(&src);
            let extension_start = if first_units == 2 {
                // JS 的 `src.slice(1)` 在 astral 首字符时留下孤立低代理，`^` 锚点因此无法在切片
                // 起点命中（`^ {0,3}(?:\$\$|\\\[)` 在代理处失败）。Rust 无法表示孤立代理，用哨兵
                // 字符前缀模拟（哨兵不是换行/空格/定界符），再把前缀的 1 个码元减掉。
                let sentinel_prefixed = format!("\u{0}{sliced}");
                super::latex_ext::block_extension_start(&sentinel_prefixed)
                    .map(|index| index.saturating_sub(1))
            } else {
                super::latex_ext::block_extension_start(sliced)
            };
            if let Some(start_index) = extension_start {
                // `e.substring(0, startIndex + 1)`：startIndex 是 `e.slice(1)` 里的 UTF-16 下标，
                // 换回原串要补上被切掉的码元数（astral 首字符为 2，见 [`js_slice_from_1_units`]）。
                let end = start_index + first_units;
                cut_src = slice_utf16(&src, end).to_string();
            }
            if self.state.top
                && let Some(token) = self.paragraph(&cut_src)
            {
                let token_raw = token.raw().to_string();
                let merge = last_paragraph_clipped
                    && matches!(tokens.last(), Some(Token::Paragraph { .. }));
                if merge {
                    // 续段并回上一个 paragraph：丢弃本次 `paragraph()` 刚登记的队列项，
                    // 改成用合并后的文本更新上一个 token 的队列源。
                    let (raw, text) = match &token {
                        Token::Paragraph { raw, text, .. } => (raw.clone(), text.clone()),
                        _ => unreachable!(),
                    };
                    let last = tokens.last_mut().expect("checked");
                    if !last.raw().ends_with('\n') {
                        last_push_raw(last, "\n");
                    }
                    last_push_raw(last, &raw);
                    if let Some(text_slot) = token_text_mut(last) {
                        text_slot.push('\n');
                        text_slot.push_str(&text);
                    }
                    self.pop_inline_queue();
                    let merged_text = token_text(last).unwrap_or("").to_string();
                    self.update_last_inline_src(&merged_text);
                } else {
                    tokens.push(token);
                }
                last_paragraph_clipped = cut_src.len() != src.len();
                src = substring_after_raw(&src, &token_raw).to_string();
                continue;
            }

            // --- text ---
            // text 是块循环的最后兜底；相邻 text 与 marked 一样合并成一个 token。
            if let Some(token) = self.text(&src) {
                src = substring_after_raw(&src, token.raw()).to_string();
                let merge = matches!(tokens.last(), Some(Token::Text { .. }));
                if merge {
                    // 同 paragraph 合并：pop 掉新登记的队列项，并改写上一个 token 的源。
                    let (raw, text) = match &token {
                        Token::Text { raw, text, .. } => (raw.clone(), text.clone()),
                        _ => unreachable!(),
                    };
                    let last = tokens.last_mut().expect("checked");
                    if !last.raw().ends_with('\n') {
                        last_push_raw(last, "\n");
                    }
                    last_push_raw(last, &raw);
                    if let Some(text_slot) = token_text_mut(last) {
                        text_slot.push('\n');
                        text_slot.push_str(&text);
                    }
                    self.pop_inline_queue();
                    let merged_text = token_text(last).unwrap_or("").to_string();
                    self.update_last_inline_src(&merged_text);
                } else {
                    tokens.push(token);
                }
                continue;
            }

            if !src.is_empty() {
                // marked 在这里抛 `Infinite loop on byte: N`；媒体可控输入不走 panic，
                // 直接停止（与 `silent: true` 的 `console.error` 分支行为一致）。
                break;
            }
        }

        self.state.top = true;
    }
}

impl Default for Lexer {
    fn default() -> Self {
        Lexer::new()
    }
}

/// `src.slice(1)`：去掉第一个字符（等价于 JS 去掉一个 UTF-16 码元；孤立代理无法表示，
/// 这里按码点切）。
///
/// JS 的 `slice(1)` 可能把 astral 字符切成“孤立低代理”，Rust 的 `str` 无法表示这种
/// 状态；这里退而切掉整个首字符。调用方必须同时用 [`js_slice_from_1_units`] 取实际
/// 切掉的码元数，才能把切片里的下标换回原串。
pub(crate) fn js_slice_from_1(src: &str) -> &str {
    match src.char_indices().nth(1) {
        Some((index, _)) => &src[index..],
        None => "",
    }
}

/// `js_slice_from_1` 切掉的 UTF-16 码元数。JS 恒为 1，但孤立代理在 Rust 里无法表示，这里按
/// 码点切，astral 首字符会切掉 2 个码元；把 `js_slice_from_1` 结果里的下标换回原串时必须加上它。
///
/// 调用方：`block_tokens` 与 `inline_tokens` 的扩展起点裁剪。
pub(crate) fn js_slice_from_1_units(src: &str) -> usize {
    src.chars().next().map_or(0, |ch| ch.len_utf16())
}

/// `src.replace(/\r\n|\r/g, "\n")`。
///
/// 逐字节扫描：`\r` 一律输出 `\n`，紧跟的 `\n` 一并跳过（CRLF 合成一个换行）；
/// 其他位置按 UTF-8 解码后原样复制。返回串不含 `\r`，后续所有规则只需处理 `\n`。
pub(crate) fn normalize_carriage_returns(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                out.push('\n');
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
            }
            _ => {
                let ch = src[i..].chars().next().expect("boundary");
                out.push(ch);
                i += ch.len_utf8();
                continue;
            }
        }
        i += 1;
    }
    out
}

/// `token.raw += suffix`。
///
/// 块级主循环的合并分支用它把续接内容追加到上一个 token 的 `raw`。`List`/`Table` 的
/// `raw` 在嵌套结构里，单独处理；所有 token 变体都带 `raw`，所以 match 是穷尽的。
pub(crate) fn last_push_raw(token: &mut Token, suffix: &str) {
    match token {
        Token::Space { raw }
        | Token::Code { raw, .. }
        | Token::Heading { raw, .. }
        | Token::Hr { raw }
        | Token::Blockquote { raw, .. }
        | Token::Html { raw, .. }
        | Token::Def { raw, .. }
        | Token::Paragraph { raw, .. }
        | Token::Text { raw, .. }
        | Token::Escape { raw, .. }
        | Token::Link { raw, .. }
        | Token::Strong { raw, .. }
        | Token::Em { raw, .. }
        | Token::Codespan { raw, .. }
        | Token::Br { raw }
        | Token::Del { raw, .. }
        | Token::Latex { raw, .. }
        | Token::LatexBlock { raw, .. }
        | Token::Checkbox { raw, .. } => raw.push_str(suffix),
        Token::List(list) => list.raw.push_str(suffix),
        Token::Table(table) => table.raw.push_str(suffix),
    }
}

/// `token.text`（可写）。
///
/// 与 TS 的 token 形状一致：space/hr/list/def/table/br/checkbox 没有 `text` 字段，
/// 返回 `None`；合并分支据此决定是否同步更新文本。
pub(crate) fn token_text_mut(token: &mut Token) -> Option<&mut String> {
    match token {
        Token::Code { text, .. }
        | Token::Heading { text, .. }
        | Token::Blockquote { text, .. }
        | Token::Html { text, .. }
        | Token::Paragraph { text, .. }
        | Token::Text { text, .. }
        | Token::Escape { text, .. }
        | Token::Link { text, .. }
        | Token::Strong { text, .. }
        | Token::Em { text, .. }
        | Token::Codespan { text, .. }
        | Token::Del { text, .. }
        | Token::Latex { text, .. }
        | Token::LatexBlock { text, .. } => Some(text),
        _ => None,
    }
}

/// `token.text`（只读）。
///
/// 直接转发 [`Token::text`]，供合并分支在追加后重新取整段文本。
pub(crate) fn token_text(token: &Token) -> Option<&str> {
    token.text()
}

/// 递归把行内队列的结果填回块级 token 树。
///
/// marked 的 `Lexer.inline()` 把 token 的 `tokens` 数组本身放进 `inlineQueue`，之后用
/// `inlineTokens(src, tokens)` **追加**填充；任务列表第二遍会先 `unshift` checkbox，所以
/// 松散任务项的 paragraph 得到 `[checkbox, ...inline]`。这里同样用 append 而不是覆盖，
/// 并且 `inline_id` 为 `None` 的 token（不在队列里，例如 checkbox 回退构建的 paragraph）
/// 保持原样。
///
/// 遍历范围：heading/paragraph/text 按 `inline_id` 取结果；blockquote、list item 递归；
/// table 的 header/rows 单元格初始 `tokens` 为空，直接用队列结果赋值。队列里没有对应
/// id（或对应槽仍为 `None`）时按空列表处理，因此被 `pop_inline_queue` 丢弃的槽位是安全的。
fn lexer_fill_inline_tokens(tokens: &mut [Token], results: &[Option<Vec<Token>>]) {
    let lookup = |id: Option<usize>| -> Vec<Token> {
        id.and_then(|id| results.get(id).cloned().flatten())
            .unwrap_or_default()
    };
    for token in tokens.iter_mut() {
        match token {
            Token::Heading {
                inline_id, tokens, ..
            }
            | Token::Paragraph {
                inline_id, tokens, ..
            }
            | Token::Text {
                inline_id, tokens, ..
            } => {
                if inline_id.is_some() {
                    tokens.extend(lookup(*inline_id));
                }
            }
            Token::Blockquote { tokens, .. } => {
                lexer_fill_inline_tokens(tokens, results);
            }
            Token::List(list) => {
                for item in &mut list.items {
                    lexer_fill_inline_tokens(&mut item.tokens, results);
                }
            }
            Token::Table(table) => {
                for cell in &mut table.header {
                    cell.tokens = lookup(cell.inline_id);
                }
                for row in &mut table.rows {
                    for cell in row.iter_mut() {
                        cell.tokens = lookup(cell.inline_id);
                    }
                }
            }
            Token::Link { tokens, .. }
            | Token::Strong { tokens, .. }
            | Token::Em { tokens, .. }
            | Token::Del { tokens, .. } => {
                // 行内建的 token 不含队列 id；但块级 merge 可能把 text 放进这些位置，
                // 递归一次是无害的。
                lexer_fill_inline_tokens(tokens, results);
            }
            _ => {}
        }
    }
}

/// 便捷入口：对一段 markdown 源做一次完整词法分析。
///
/// 内部新建 [`Lexer`]，每次调用互不共享链接定义。它只做 lex：流式输入的未闭合围栏
/// 修剪（`trim_partial_closing_fences`）需要调用方另行处理，`crate::components::markdown`
/// 的渲染路径就是这么做的。
pub fn lex_markdown(src: &str) -> Vec<Token> {
    Lexer::new().lex(src)
}
