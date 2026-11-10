//! marked 与 marked-cjk-friendly 的等价 lexer/renderer。
//!
//! 对应 npm 包 `marked@18.0.11` 的 `src/{rules,helpers,Tokenizer,Lexer}.ts` 与本包
//! `marked-cjk-friendly@0.1.2` 的 `emStrong` 重写，以及 `packages/tui/src/components/markdown.ts`
//! 里的 LaTeX tokenizer 扩展与严格删除线 tokenizer。实现按 TS 源逐个函数移植：
//!
//! | 文件 | 对应 TS |
//! |---|---|
//! | [`lexer`] | `Lexer.ts` + `rules.ts`（正则构造） |
//! | [`block`] | `Tokenizer.ts` 的块级 tokenizer |
//! | [`inline`] | `Tokenizer.ts` 的行内 tokenizer + marked-cjk-friendly 的 `emStrong` |
//! | [`latex_ext`] | `components/markdown.ts` 的 LaTeX `TokenizerExtension` 与 `trimPartialClosingFences` |
//! | [`list`] | `Markdown#renderList`（列表排版） |
//! | [`table`] | `Markdown#renderTable`（表格排版） |
//!
//! 解析器选项固定为 marked 默认值（`gfm: true`、`breaks: false`、`pedantic: false`），并注册
//! 与 TS 相同的三处扩展：严格删除线 `del`、CJK 友好的 `emStrong`、LaTeX block/inline 扩展。
//!
//! ## 与 TS 的表示差异
//!
//! - **索引单位**。TS 的字符串是 UTF-16；本实现内部以 UTF-8 字节/`char` 处理，但在需要
//!   与 TS 逐单位对齐的地方（`emStrong`/`del` 的 `raw` 切分、CJK 邻接判定、mask 长度）显式
//!   换算成 UTF-16 码元（见 `lexer::utf16_len` 等）。ASCII 输入的字节偏移与 UTF-16 偏移一一对应。
//! - **token 缓存**。TS 用 `WeakRef` 让 token 树在渲染突增后能被 GC；Rust 用 `Rc` 持有，
//!   生命周期到下一次 source 变化为止。可观察输出不变。
//! - **正则方言**。JS 的 `\s`/`\d`/`\w`/`.` 按 JS 语义展开（见 `lexer::translate_pattern`）；
//!   `\p{L}`/`\p{N}`/`\p{P}`/`\p{S}` 使用 `regex` crate 的 Unicode 表（版本略旧于 Node 26 的
//!   Unicode 17），仅影响 Unicode 14–17 新增的极少数码点。
//!
//! # 模块职责与调用关系
//! 本文件定义 token 数据结构及其访问器（含可变的 [`Token::tokens_mut`]），并重导出
//! [`Lexer`]；不做词法分析、也不做渲染。词法分析在 [`lexer`] 中实现：[`Lexer`] 把源
//! 文本变成 [`Token`] 树；`crate::components::markdown`
//! 的 `Markdown` 组件负责把树渲染成带 ANSI 的行，其中列表与表格分别委托 [`list`] 与
//! [`table`]，LaTeX 定界符的解析规则在 [`latex_ext`]。
//!
//! # 一次完整流程
//! 1. 调用方（渲染组件、测试、mermaid 扩展）执行 [`Lexer::new`] 后调用 [`Lexer::lex`]；
//! 2. 块级阶段生成顶层 token；heading/paragraph/text 与表格单元格只登记行内源，把队列
//!    id 写进 `inline_id`，`tokens` 暂时为空；
//! 3. `lex` 末尾逐项处理队列并把结果回填到 `tokens`，因此拿到的树里行内 token 已就位；
//! 4. 渲染组件在 `lex` 之后先调用 `latex_ext::trim_partial_closing_fences` 处理流式围栏，
//!    再按 token 类型渲染。
//!
//! # 数据结构不变量
//! - `raw` 通常是词法阶段消费的源码片段（可能做尾部 trim）；例外是 blockquote，它的
//!   `raw` 是重建的，可能比源串长（见 `tests/markdown.rs` 的
//!   `blockquote_reconstructed_raw_may_exceed_source`）。
//! - `text` 是处理过的文本（trim、去缩进、去任务前缀等），不保证等于 `raw`。
//! - `inline_id` 在 `lex` 返回后仍保留 `Some(id)`；它只在词法内部用于回填，渲染层不读取。
//! - 只有 heading/paragraph/text/link/strong/em/del/blockquote 这些容器变体带 `tokens`；
//!   [`Token::tokens`] 对其余变体返回 `None`。

// --- 子模块 ---
pub mod block;
pub mod inline;
pub mod latex_ext;
pub mod lexer;
pub mod list;
pub mod table;

pub use lexer::Lexer;

/// `Token`（marked 的 token 联合类型）。
///
/// 字段命名与 TS 对齐；`inline_id` 是本实现的内部机制：块级 tokenizer 把行内 token 化推迟到
/// 全部块级 token 生成之后（引用式链接可以定义在使用之后），因此先记录队列 id，最后由
/// [`Lexer::lex`] 在块级扫描结束后按队列统一回填（实现见 `lexer` 模块的
/// `lexer_fill_inline_tokens`）。回填后 `inline_id` 仍保留 `Some(id)`，渲染层不读取它。
///
/// token 树只由词法分析器生成；各字段是 `pub`，渲染层和测试直接读取，调用方不应手工
/// 改写树（`raw`/`text` 之间的一致性没有强制检查）。
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// `space`：markdown 中的空行。
    Space { raw: String },
    /// `code`：缩进代码块或围栏代码块。
    Code {
        /// 原始源码片段（围栏代码块含围栏行；缩进代码块含行首缩进）。
        raw: String,
        /// 围栏的 info string：缩进代码块为 `None`，围栏代码块没有 info string 时为
        /// `Some("")`（与缩进代码块的 `None` 区分）。
        lang: Option<String>,
        /// 代码内容。缩进代码块删掉行首缩进；围栏代码块经 `indent_code_compensation` 处理，
        /// 但该补偿的正则只匹配带缩进的反引号围栏，`~~~` 围栏即使带缩进也原样保留。该函数
        /// 当前取的是行首 `indent_len` 个 UTF-16 码元组成的前缀而不是去掉这段前缀，所以前导
        /// 空白足够长的行可能丢失缩进后的正文（这是当前实现的既有行为，不能据此认为缩进
        /// 已被正确去除）。
        text: String,
        /// `codeBlockStyle: "indented"`（TS 字段，渲染不用）。
        indented: bool,
    },
    /// `heading`：ATX 或 setext 标题。
    Heading {
        raw: String,
        /// `#` 的 UTF-16 码元数，即标题级别（1–6；setext 下划线 `=` → 1、`-` → 2）。
        depth: usize,
        text: String,
        inline_id: Option<usize>,
        tokens: Vec<Token>,
    },
    /// `hr`：水平线。
    Hr { raw: String },
    /// `blockquote`：引用（子 token 是块级 token）。
    Blockquote {
        raw: String,
        text: String,
        tokens: Vec<Token>,
    },
    /// `list`：列表。
    List(ListToken),
    /// `html`：HTML 块或行内 HTML。
    Html {
        raw: String,
        /// 块级 HTML 为 `true`，行内标签为 `false`。
        block: bool,
        /// 块级 HTML 捕获到 `pre`/`script`/`style` 时为 `true`；行内标签恒为 `false`。
        pre: bool,
        text: String,
        /// 处理完该行内标签后是否位于 `<a>` 内（块级 HTML 恒为 `false`）。
        in_link: bool,
        /// 处理完该行内标签后是否位于 `pre`/`code`/`kbd`/`script` 原始块内
        /// （块级 HTML 恒为 `false`）。
        in_raw_block: bool,
    },
    /// `def`：链接定义。
    Def {
        raw: String,
        /// label 归一化后的查找键（小写、连续空白折叠成一个空格）。
        tag: String,
        /// 链接目标（已去掉可选的尖括号包裹并还原转义）。
        href: String,
        /// 标题；`None` 表示定义里没有标题。
        title: Option<String>,
    },
    /// `table`：GFM 表格。
    Table(TableToken),
    /// `paragraph`：段落。
    Paragraph {
        raw: String,
        text: String,
        inline_id: Option<usize>,
        tokens: Vec<Token>,
    },
    /// `text`：块级 text token 或行内 text token。
    Text {
        raw: String,
        text: String,
        inline_id: Option<usize>,
        tokens: Vec<Token>,
        /// 位于 `pre`/`code`/`kbd`/`script` 原始块内时为 `true`（行内 tokenizer 从
        /// `in_raw_block` 状态复制；块级 text 恒为 `false`）。
        escaped: bool,
    },
    /// `escape`：反斜杠转义。
    Escape { raw: String, text: String },
    /// `link` / `image`：链接或图片（`image` 在渲染时按纯文本输出 `text`）。
    Link {
        raw: String,
        href: String,
        title: Option<String>,
        text: String,
        tokens: Vec<Token>,
        image: bool,
    },
    /// `strong`。
    Strong {
        raw: String,
        text: String,
        tokens: Vec<Token>,
    },
    /// `em`。
    Em {
        raw: String,
        text: String,
        tokens: Vec<Token>,
    },
    /// `codespan`。
    Codespan { raw: String, text: String },
    /// `br`。
    Br { raw: String },
    /// `del`。
    Del {
        raw: String,
        text: String,
        tokens: Vec<Token>,
    },
    /// `latex`：行内数学（`pending` 表示流式未闭合）。
    Latex {
        raw: String,
        text: String,
        pending: bool,
    },
    /// `latexBlock`：块级数学（`pending` 表示流式未闭合）。
    LatexBlock {
        raw: String,
        text: String,
        pending: bool,
    },
    /// `checkbox`：任务列表复选框。
    ///
    /// `raw` 是复选框原文加一个尾随空格（如 `"[ ] "` / `"[x] "`）；`checked` 在原文不是
    /// `[ ]` 时为 `true`。渲染层不输出该 token（`text()` 返回 `None`），复选框由
    /// `markdown::list` 根据 `ListItem::task` / `checked` 重画。
    Checkbox { raw: String, checked: bool },
}

impl Token {
    /// token 的 `type` 字符串（对应 TS 的 `token.type`）。
    ///
    /// `Link { image: true }` 返回 `"image"`，其余与变体名一致。渲染层用这些字符串判断
    /// 下一个兄弟 token（如 `"space"`）来决定是否补空行，因此不能随意改名。
    pub fn type_name(&self) -> &'static str {
        match self {
            Token::Space { .. } => "space",
            Token::Code { .. } => "code",
            Token::Heading { .. } => "heading",
            Token::Hr { .. } => "hr",
            Token::Blockquote { .. } => "blockquote",
            Token::List(_) => "list",
            Token::Html { .. } => "html",
            Token::Def { .. } => "def",
            Token::Table(_) => "table",
            Token::Paragraph { .. } => "paragraph",
            Token::Text { .. } => "text",
            Token::Escape { .. } => "escape",
            Token::Link { image: false, .. } => "link",
            Token::Link { image: true, .. } => "image",
            Token::Strong { .. } => "strong",
            Token::Em { .. } => "em",
            Token::Codespan { .. } => "codespan",
            Token::Br { .. } => "br",
            Token::Del { .. } => "del",
            Token::Latex { .. } => "latex",
            Token::LatexBlock { .. } => "latexBlock",
            Token::Checkbox { .. } => "checkbox",
        }
    }

    /// `"raw" in token`：所有 token 都带 `raw`。
    ///
    /// `Token::List` / `Token::Table` 变体本身没有内联的 `raw` 字段；源码分别放在
    /// [`ListToken::raw`] / [`TableToken::raw`] 中，本方法直接返回这两个字段。
    pub fn raw(&self) -> &str {
        match self {
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
            | Token::Checkbox { raw, .. } => raw,
            Token::List(list) => &list.raw,
            Token::Table(table) => &table.raw,
        }
    }

    /// `"text" in token && typeof token.text === "string"`。
    ///
    /// `Space`/`Hr`/`List`/`Def`/`Table`/`Br`/`Checkbox` 没有字符串类型的 `text` 字段，
    /// 返回 `None`；其余变体返回 `Some`（可能为空串）。
    pub fn text(&self) -> Option<&str> {
        match self {
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
            Token::Space { .. }
            | Token::Hr { .. }
            | Token::List(_)
            | Token::Def { .. }
            | Token::Table(_)
            | Token::Br { .. }
            | Token::Checkbox { .. } => None,
        }
    }

    /// `token.tokens`（不存在时为 `None`）。
    ///
    /// 只有 heading/blockquote/paragraph/text/link/strong/em/del 这 8 种容器变体返回
    /// `Some`；返回的切片可能为空（行内 token 尚未回填时）。
    pub fn tokens(&self) -> Option<&[Token]> {
        match self {
            Token::Heading { tokens, .. }
            | Token::Blockquote { tokens, .. }
            | Token::Paragraph { tokens, .. }
            | Token::Text { tokens, .. }
            | Token::Link { tokens, .. }
            | Token::Strong { tokens, .. }
            | Token::Em { tokens, .. }
            | Token::Del { tokens, .. } => Some(tokens),
            _ => None,
        }
    }

    /// `token.tokens`（可变）。
    ///
    /// 返回 `Some` 的变体集合与 [`Token::tokens`] 相同（当前仓库内没有调用方）。
    pub fn tokens_mut(&mut self) -> Option<&mut Vec<Token>> {
        match self {
            Token::Heading { tokens, .. }
            | Token::Blockquote { tokens, .. }
            | Token::Paragraph { tokens, .. }
            | Token::Text { tokens, .. }
            | Token::Link { tokens, .. }
            | Token::Strong { tokens, .. }
            | Token::Em { tokens, .. }
            | Token::Del { tokens, .. } => Some(tokens),
            _ => None,
        }
    }
}

/// `Tokens.List`。
///
/// 由 `block` 的 list tokenizer 生成；`items` 至少一个（没有任何条目时 tokenizer 返回
/// `None` 而不是空列表）。
#[derive(Debug, Clone, PartialEq)]
pub struct ListToken {
    /// 整个列表的源码：覆盖第一个条目到列表末尾，末尾的空白已去掉。
    pub raw: String,
    /// 有序（`1.`/`1)`）还是无序（`-`/`+`/`*`）。
    pub ordered: bool,
    /// TS 的 `token.start`：有序列表是数字，无序列表是 `''`。
    ///
    /// Rust 用 `Option<u64>`：有序列表为 `Some(n)`，无序列表为 `None`；渲染时 `None`
    /// 按 1 处理。
    pub start: Option<u64>,
    /// 松散列表：条目之间有空行（渲染时会在条目间补空行）。
    pub loose: bool,
    /// 条目列表；tokenizer 保证非空。
    pub items: Vec<ListItem>,
}

/// `Tokens.ListItem`。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ListItem {
    /// 条目源码（含 marker 与行尾换行）；最后一个条目已去掉尾部空白。
    pub raw: String,
    /// 是否有任务复选框前缀 `[ ]`/`[x]`（且首个块级 token 是 text/paragraph）。
    pub task: bool,
    /// 复选框状态；`None` 表示非任务项，或任务标记未能与 `item.raw` 里的复选框原文
    /// 对应上。
    pub checked: Option<bool>,
    /// TS 的 `item.loose`；渲染层读的是 `ListToken::loose`，此字段当前未被消费。
    pub loose: bool,
    /// 去掉 marker（以及任务前缀）后的条目内容，用于块级递归解析。
    pub text: String,
    /// 条目的块级 token；`lex` 返回后内部行内 token 已填充。
    pub tokens: Vec<Token>,
}

/// GFM 表格对齐方式（`Tokens.Table['align']` 的元素，`null` → [`TableAlign::None`]）。
///
/// 值由分隔行每列的形状决定：以 `:` 结尾且开头不是 `:` → 右对齐；两端都是 `:` → 居中；
/// 只以 `:` 开头 → 左对齐；没有 `:` → `None`。判定顺序是 right → center → left（见
/// `block` 的 table tokenizer）。当前表格渲染器不读取该字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableAlign {
    /// 左对齐（分隔行形如 `:---`）。
    Left,
    /// 居中（`:---:`）。
    Center,
    /// 右对齐（`---:`）。
    Right,
    /// 没有显式对齐（`---`）。
    None,
}

/// `Tokens.TableCell`。
#[derive(Debug, Clone, PartialEq)]
pub struct TableCell {
    /// 单元格文本（`split_cells` 处理后的原文，尚未做行内 token 化）。
    pub text: String,
    /// 行内 token 的队列 id；`lex` 回填后仍保留。
    pub inline_id: Option<usize>,
    /// 行内 token；`lex` 返回后已填充（表格单元格是覆盖而非追加）。
    pub tokens: Vec<Token>,
    /// 表头单元格为 `true`，数据行为 `false`；渲染器用 `TableToken::header` 区分，
    /// 不读取本字段。
    pub header: bool,
    /// 该列的对齐方式；渲染器不读取。
    pub align: TableAlign,
}

/// `Tokens.Table`。
#[derive(Debug, Clone, PartialEq)]
pub struct TableToken {
    /// 表格源码（去掉末尾换行）。
    pub raw: String,
    /// 表头单元格；长度即列数。
    pub header: Vec<TableCell>,
    /// 每列对齐方式，长度与 `header` 相同（tokenizer 校验）。
    pub align: Vec<TableAlign>,
    /// 数据行；每行的单元格数等于 `header.len()`（不足补空、超出截断）。
    pub rows: Vec<Vec<TableCell>>,
}

impl TableToken {
    /// 新建一个空表格 token（各列初始化为空数组），由 `block` 的表格
    /// tokenizer 逐项填充。
    pub(crate) fn empty(raw: String) -> Self {
        TableToken {
            raw,
            header: Vec::new(),
            align: Vec::new(),
            rows: Vec::new(),
        }
    }
}
