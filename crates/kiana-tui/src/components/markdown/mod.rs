//! components/markdown.ts：`Markdown` 组件、主题与渲染。
//!
//! 解析用本 crate 的 marked 等价 lexer（见 [`crate::markdown`]）；本模块只负责把 token 流变成
//! 带 ANSI 样式的行，并复刻 TS 的换行/边距/背景/表格/列表排版。逐分支对应
//! `packages/tui/src/components/markdown.ts` 的 `render` / `renderToken` / `renderInlineTokens`；
//! 列表与表格算法分别在 [`crate::markdown::list`] 与 [`crate::markdown::table`]。
//!
//! # 为什么需要一个独立模块
//! 词法层 [`crate::markdown::Lexer`] 只把源码变成与 marked 对齐的 token 树
//! （[`crate::markdown::Token`]）；列表与表格的终端排版实现在 [`crate::markdown::list`] 与
//! [`crate::markdown::table`]（两者都是 `impl Markdown` 的代码，只是放在 `markdown/` 子模块里），
//! 本模块负责其余渲染并编排整棵树。token 树与宽度无关，因此可以在同一份解析结果上按不同
//! 宽度反复排版；`TokenCache` 就是按“变换后的文本”复用这棵树的缓存。
//!
//! # 调用方
//! - `kiana-interactive`：用户消息（`components/user_message.rs`，带背景色与 `transform`）、
//!   changelog 与新版本提示等普通消息。
//! - `kiana-extensions`：把 JS 侧的 `Markdown` 构造参数（主题函数、`defaultTextStyle`、
//!   `options.transform`）桥接到本类型。
//! - 测试：`crates/kiana-tui/tests/markdown.rs` 逐用例对照 TS 的 `markdown.test.ts`。
//!
//! # 一次 `render` 的流程
//! 1. 缓存检查：原始文本与渲染宽度都没变时直接返回上次的行。
//! 2. 内容宽度 = `width - 2 * padding_x`（下溢时取 1）；有 [`MarkdownOptions::transform`]
//!    时先改写源文本。
//! 3. 变换后 trim 为空 → 返回空 `Vec`（不产生 padding 行）。
//! 4. 制表符替换成 3 个空格，再用 [`Lexer`] 解析；token 树按“制表符归一化后的变换文本”
//!    缓存，最后调用 `trim_partial_closing_fences` 修掉流式输出中未闭合的末尾围栏。
//! 5. 逐个 token 调 `render_token` 生成逻辑行；块级 token 根据下一个 token 的类型决定是否补空行。
//! 6. 非图片行按内容宽度换行（[`wrap_text_with_ansi`]）；[`is_image_line`] 为真的行原样保留。
//! 7. 非图片行加左右 padding、可选背景色并补足到 `width`，再在上下各补 `padding_y` 个空行。
//!
//! # 行内样式前缀（style prefix）
//! 主题函数是任意 `(text) -> String`；典型实现（`kiana-theme` 的 chalk 包装）在文本前后各加
//! 一段 ANSI，结尾是样式 reset。渲染器在每个带样式的内联片段之后追加 `style_prefix`
//! （默认文本样式的开头 ANSI），让片段之后的普通文本仍处于外层样式中；`render_inline_tokens`
//! 返回前会把末尾多余的 `style_prefix` 循环裁掉，防止它泄漏到行尾 padding 或表格边框。
//! 前缀通过给样式函数传 NUL 哨兵、再取哨兵之前的子串得到（见 `compute_default_style_prefix`
//! 与 `get_style_prefix`）。`preserve_heading_styling_after_inline_code` 等用例用
//! `preceding_chunk` 取目标文本之前的一小段，检查其中是否重新出现打开标题样式的序列；
//! `preserve_gray_italic_styling_after_inline_code` 只是该场景的冒烟测试：它只检查整段
//! 输出里是否存在灰色/斜体/黄色序列，而输入开头的普通文本本身已带默认灰色斜体，
//! 即使片段之后的样式恢复失效这些断言也会通过，不能作为恢复逻辑的锁定证据。
//!
//! # 缓存与失效
//! - `cached_lines` 键是「原始 `text` + 渲染宽度」；`set_text` 与 trait 版 `invalidate`
//!   清掉它，使下一次渲染重新执行 `transform`。
//! - `cached_tokens` 不随之清空：它按变换后的文本匹配，宽度变化但文本相同就复用。
//! - `default_style_prefix` 只在构造与 `with_default_text_style` 时计算；构造后本模块不再修改主题。
//!
//! # 注意事项
//! - 输出行宽按终端列计，ANSI/OSC 序列不算宽度；换行与右填充依赖 `text` 模块的宽度计算。
//! - 图片行（含 kitty / iTerm2 图片协议序列）不换行、不加右填充，协议序列不能按文本切分。
//! - 链接在 [`get_capabilities`] 报告 `hyperlinks` 时输出 OSC 8；否则把 URL 以 ` (url)` 追加在
//!   文本后，但当链接文字本身等于 URL（比较时去掉 `mailto:` 前缀）时不再重复。

use std::rc::Rc;
use std::sync::Arc;

use crate::latex::{RenderLatexOptions, render_latex};
use crate::markdown::latex_ext::trim_partial_closing_fences;
use crate::markdown::{Lexer, Token};
use crate::terminal_image::{get_capabilities, hyperlink, is_image_line};
use crate::text::{
    apply_background_to_line, flatten_lines, trim_js, visible_width, wrap_text_with_ansi,
};
use crate::tui::Component;

/// 主题里的样式函数（`(text: string) => string`）。
///
/// 合约：输入一段文本，返回带 ANSI 的文本；渲染器不做转义/配平检查，但在可见宽度计算与
/// 换行时会识别 ANSI 序列（换行时 `AnsiCodeTracker` 会跨行重放活动序列），因此样式函数仍须
/// 自行保证 open/close 配对。
/// 闭包必须 `Send + Sync`（主题类型要求可共享），不能捕获 `Rc`、`RefCell` 等非线程安全状态。
/// 返回空串是合法的，但此时该样式的“前缀”为空（见 `get_style_prefix`）。
pub type StyleFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// 代码高亮函数（`(code, lang) => string[]`）。
///
/// `code` 是代码块的正文（不含围栏，行间以 `\n` 分隔）；`lang` 取围栏语言，可能为 `None`。
/// 返回的每一行被视为**已带 ANSI 的成品行**：渲染器只在其前面加代码块缩进
/// （`code_block_indent`，缺省两个空格），不会再套 `code_block` 样式；这些行仍参与后续按宽度换行。
pub type HighlightFn = Arc<dyn Fn(&str, Option<&str>) -> Vec<String> + Send + Sync>;

/// 源文本变换函数（`(markdown, availableWidth) => string`）。
///
/// 参数是（当前源文本, 内容宽度），在 token 化之前调用；内容宽度 =
/// `render` 的 `width - 2 * padding_x`（最小 1）。只在渲染缓存未命中时执行，
/// 因此宽度变化会触发再次调用（`tests/markdown.rs` 的
/// `caches_transformed_markdown_by_source_and_available_width` 锁定这一点）。
pub type TransformFn = Arc<dyn Fn(&str, usize) -> String + Send + Sync>;

/// 默认文本样式：应用到所有文本，除非被 markdown 格式覆盖。
///
/// 它既是行内普通文本的底色，也是默认行内上下文里每个带样式片段结束后的“恢复样式”。
/// `bg_color` 不参与行内嵌套：它在 `render` 的填充阶段对非图片行整行应用
/// （包括上下 padding 空行）。
/// 四个布尔项按 `color → bold → italic → strikethrough → underline` 的顺序逐层包裹，
/// 后面的项处于更外层（实现见 `apply_default_style_with`）。
#[derive(Clone, Default)]
pub struct DefaultTextStyle {
    /// 前景色函数。
    pub color: Option<StyleFn>,
    /// 背景色函数（在填充阶段整行应用）。
    pub bg_color: Option<StyleFn>,
    /// 默认文本是否叠加主题的 `bold`。
    pub bold: bool,
    /// 默认文本是否叠加主题的 `italic`。
    pub italic: bool,
    /// 默认文本是否叠加主题的 `strikethrough`。
    pub strikethrough: bool,
    /// 默认文本是否叠加主题的 `underline`。
    pub underline: bool,
}

/// markdown 元素主题：每个函数接收文本并返回带 ANSI 的文本。
///
/// 闭包在渲染缓存未命中、生成各行时调用；`with_default_text_style` 计算默认样式前缀时
/// 也可能调用（取决于该样式是否设置了对应的主题项），缓存命中时不会调用。渲染器不校验/
/// 配平 open/close 序列，但会在宽度计算与换行时识别 ANSI（换行时 `AnsiCodeTracker` 跨行
/// 重放活动序列），引用行在遇到 `\x1b[0m` 后补回引用样式前缀；主题实现仍须自行保证配对。
#[derive(Clone)]
pub struct MarkdownTheme {
    /// 标题文字：所有标题都套 `bold`，H1 还套 `underline`；depth ≥ 3 的 `#` 前缀也走这里。
    pub heading: StyleFn,
    /// 链接文字（渲染时先 `underline` 再 `link`）。
    pub link: StyleFn,
    /// 不支持 OSC 8 时附加在链接文字后的 ` (url)`。
    pub link_url: StyleFn,
    /// 行内代码（codespan）。
    pub code: StyleFn,
    /// 代码块每一行内容；`highlight_code` 为 `Some` 时不使用。
    pub code_block: StyleFn,
    /// 代码块围栏行（开头的 ```lang 与结尾的 ```）。
    pub code_block_border: StyleFn,
    /// 引用文本；每行先叠 `italic` 再套 `quote`。
    pub quote: StyleFn,
    /// 引用行前缀 `"│ "`。
    pub quote_border: StyleFn,
    /// 分隔线字符（长度取 `width` 与 80 的较小值）。
    pub hr: StyleFn,
    /// 列表 marker（`- ` / `1. `，任务列表含 `[x] `）。
    pub list_bullet: StyleFn,
    /// 粗体文本（`**x**`）。
    pub bold: StyleFn,
    /// 斜体文本（`*x*`）。
    pub italic: StyleFn,
    /// 删除线文本（`~~x~~`）。
    pub strikethrough: StyleFn,
    /// 下划线样式；被 H1 标题、链接与默认文本样式使用。
    pub underline: StyleFn,
    /// 代码高亮器；`Some` 时代码块的每一行由它生成，否则用 `code_block` 逐行着色。
    pub highlight_code: Option<HighlightFn>,
    /// 每行代码块前缀（默认 `"  "`）。
    pub code_block_indent: Option<String>,
}

/// `MarkdownOptions`。
#[derive(Clone)]
pub struct MarkdownOptions {
    /// 保留源列表 marker，而不是归一化。
    ///
    /// 为 true 时列表渲染从 `item.raw` 还原 `1)` / `*` 等原始写法；为 false 时有序列表从
    /// `start` 递增、无序列表统一输出 `- `（实现见 `crate::markdown::list`）。
    pub preserve_ordered_list_markers: bool,
    /// 保留源反斜杠转义，而不是归一化转义标点。
    ///
    /// 控制行内 `escape` token 输出 `raw`（带反斜杠）还是 `text`（已去掉反斜杠）。
    pub preserve_backslash_escapes: bool,
    /// 解析前用可用宽度变换源 markdown。
    ///
    /// 参数与调用时机见 [`TransformFn`]；变换结果不进入 `cached_lines` 的键，
    /// 外部状态变化后要让渲染重新执行变换，必须调用 `Markdown::invalidate`。
    pub transform: Option<TransformFn>,
    /// 把支持的 LaTeX 数学渲染成 Unicode 文本（默认 true）。
    ///
    /// 为 false 时不做 LaTeX 转换：行内输出 `raw`；块级先 `trim_js(raw)` 再逐行套默认
    /// 文本样式；流式未闭合（`pending`）的 token 无论该值如何都走同一回退路径。
    pub render_latex: bool,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        MarkdownOptions {
            preserve_ordered_list_markers: false,
            preserve_backslash_escapes: false,
            transform: None,
            render_latex: true,
        }
    }
}

/// 行内样式上下文（TS 的 `{ applyText, stylePrefix }`）。
///
/// `apply_text` 给普通文本着色；`style_prefix` 是需要在每个带样式片段之后重放的
/// “外层样式重新打开序列”（默认上下文里等于 `apply_text` 输出中文本之前的部分）。
/// 列表与表格把收到的上下文原样传给子 token；
/// 标题与引用会构造自己的上下文（各自的 `apply_text`/前缀不同）。
#[derive(Clone)]
pub(crate) struct InlineStyleContext {
    /// 给一段（可能含 `\n`）的文本应用样式。
    pub(crate) apply_text: StyleFn,
    /// 重放用的外层样式前缀；默认上下文里等于 `apply_text` 输出中文本之前的部分，可能为空。
    pub(crate) style_prefix: String,
}

/// 解析结果缓存（TS 的 `{ source, tokens }` + `WeakRef`）。
///
/// `source` 是制表符归一化并经过 `transform` 的文本，作为缓存键；TS 侧用 `WeakRef`
/// 允许 token 树在内存压力下被回收，Rust 侧用 `Rc` 一直持有到源文本变化为止，
/// 可观察输出不变（见 `crate::markdown` 模块文档的“与 TS 的表示差异”）。
struct TokenCache {
    source: String,
    tokens: Vec<Token>,
}

/// `Markdown implements Component`。
///
/// 一个 Markdown 块组件：持有源文本、主题、默认文本样式、选项，以及渲染缓存
/// （行、原始文本、宽度）与 token 缓存。所有排版都在 `render` 中惰性完成。
///
/// 构造器与 `with_*` 是链式调用；`with_*` 只写字段、**不清理已生成的缓存**，
/// 因此对一个已经渲染过的实例换样式或选项后，需要再调用 [`Markdown::invalidate`]，
/// 否则相同文本与宽度会直接命中旧结果。
pub struct Markdown {
    text: String,
    padding_x: usize,
    padding_y: usize,
    default_text_style: Option<DefaultTextStyle>,
    pub(crate) theme: MarkdownTheme,
    pub(crate) options: MarkdownOptions,
    /// `getDefaultStylePrefix()` 的缓存。主题不可变，因此在构造时计算与 TS 的懒计算等价。
    default_style_prefix: String,
    cached_text: Option<String>,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
    cached_tokens: Option<Rc<TokenCache>>,
}

impl Markdown {
    /// `constructor(text, paddingX, paddingY, theme, defaultTextStyle?, options?)`。
    ///
    /// - `padding_x`：左右各留的空格列数；内容宽度 = `width - 2 * padding_x`（最小 1）。
    /// - `padding_y`：输出上下各补的空行数；空行是整行空格（有背景色时也应用背景）。
    /// - 初始没有默认文本样式；选项取 `MarkdownOptions::default()`（`render_latex` 为 true）。
    ///
    /// 构造结束时就计算一次默认样式前缀（逻辑见 `compute_default_style_prefix`）；
    /// 主题按值持有、之后不再变化，只有 `with_default_text_style` 会重算前缀。
    pub fn new(
        text: impl Into<String>,
        padding_x: usize,
        padding_y: usize,
        theme: MarkdownTheme,
    ) -> Self {
        let mut markdown = Markdown {
            text: text.into(),
            padding_x,
            padding_y,
            default_text_style: None,
            theme,
            options: MarkdownOptions::default(),
            default_style_prefix: String::new(),
            cached_text: None,
            cached_width: None,
            cached_lines: None,
            cached_tokens: None,
        };
        markdown.default_style_prefix = markdown.compute_default_style_prefix();
        markdown
    }

    /// 设置默认文本样式（构造参数 `defaultTextStyle`）。
    ///
    /// 整体覆盖已有样式（不是逐字段合并），并立即重算 `default_style_prefix`。
    /// 不清理渲染缓存；对已渲染实例调用后需自行 [`Markdown::invalidate`]。
    pub fn with_default_text_style(mut self, style: DefaultTextStyle) -> Self {
        self.default_text_style = Some(style);
        self.default_style_prefix = self.compute_default_style_prefix();
        self
    }

    /// 设置选项（构造参数 `options`，TS 会浅拷贝）。
    ///
    /// 整个 [`MarkdownOptions`] 被替换（不是逐字段合并）。不清理渲染缓存；
    /// 对已渲染实例修改选项后需调用 [`Markdown::invalidate`]。
    pub fn with_options(mut self, options: MarkdownOptions) -> Self {
        self.options = options;
        self
    }

    /// `setText(text)`。
    ///
    /// 只清渲染缓存（文本 / 宽度 / 行）；token 缓存保留，若变换后的文本不同会自然重新解析。
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.invalidate();
    }

    /// 当前源文本。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// `invalidate()`：只清渲染缓存（不清 token 缓存与 `defaultStylePrefix`）。
    ///
    /// token 缓存可以保留：它的键是变换后的文本（制表符已归一化），下次文本不同就会重新解析；
    /// 而 `defaultStylePrefix` 只由主题与默认样式决定，与源文本无关。
    pub fn invalidate(&mut self) {
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
    }

    /// `applyDefaultStyle(text)`。
    ///
    /// 纯转发到 `apply_default_style_with`（样式嵌套顺序见该函数）。
    pub(crate) fn apply_default_style(&self, text: &str) -> String {
        apply_default_style_with(&self.theme, self.default_text_style.as_ref(), text)
    }

    // 用 NUL 作哨兵调用一次默认样式，取它在结果中的位置：哨兵之前的部分就是“重新打开
    // 默认样式”所需的 ANSI 前缀，哨兵之后是剩余输出（通常是关闭序列，这里不需要）。
    // 若样式函数过滤/改写了哨兵，`find` 返回 `None`，前缀退化为空串。
    fn compute_default_style_prefix(&self) -> String {
        if self.default_text_style.is_none() {
            return String::new();
        }
        let sentinel = "\u{0}";
        let styled = self.apply_default_style(sentinel);
        match styled.find('\u{0}') {
            Some(byte_index) => styled[..byte_index].to_string(),
            None => String::new(),
        }
    }

    /// `getStylePrefix(styleFn)`（每次重新计算，不缓存）。
    ///
    /// 与 `compute_default_style_prefix` 同样的哨兵技巧：把 `"\u{0}"` 交给样式函数，
    /// 返回第一个 NUL 之前的子串（即开头的 ANSI）。样式函数丢弃哨兵时返回空串。
    /// 不缓存是因为标题、引用等样式闭包是每次渲染临时构造的，没有稳定的缓存键。
    pub(crate) fn get_style_prefix(&self, style_fn: &dyn Fn(&str) -> String) -> String {
        let styled = style_fn("\u{0}");
        match styled.find('\u{0}') {
            Some(byte_index) => styled[..byte_index].to_string(),
            None => String::new(),
        }
    }

    // 把默认样式封装成可克隆的上下文：`apply_text` 捕获主题与默认样式的克隆，
    // `style_prefix` 直接复用构造时算好的前缀，避免每次重放都重新探测一遍。
    fn get_default_inline_style_context(&self) -> InlineStyleContext {
        let theme = self.theme.clone();
        let style = self.default_text_style.clone();
        InlineStyleContext {
            apply_text: Arc::new(move |text: &str| {
                apply_default_style_with(&theme, style.as_ref(), text)
            }),
            style_prefix: self.default_style_prefix.clone(),
        }
    }

    /// `renderInlineTokens(tokens, styleContext?)`。
    ///
    /// 把一个行内 token 序列拼成字符串（`Br` 会插入 `\n`；真正的按宽度换行由
    /// `Component::render` 统一处理）。`style_context` 为 `None` 时先构造默认样式上下文
    /// 再递归，保证这条路径只有最外层付一次构造成本。
    ///
    /// 关键不变量：
    /// - `Strong` / `Em` / `Codespan` / `Del` / `Link` 等带样式的片段结束后都会追加
    ///   `style_prefix`，让后续普通文本仍处于外层样式中；返回前循环裁掉末尾多余的前缀。
    /// - `Link { image: true }`（Markdown 图片）只输出 `text`，不产生链接或图片协议序列。
    /// - 链接是否用 OSC 8 由 [`get_capabilities`] 决定；不支持时按需追加 ` (href)`。
    /// - `Latex` 仅在 `render_latex` 为 true 且非 `pending` 时渲染，否则输出 `raw`。
    pub(crate) fn render_inline_tokens(
        &self,
        tokens: &[Token],
        style_context: Option<&InlineStyleContext>,
    ) -> String {
        if style_context.is_none() {
            // 顶层调用不传上下文：构造一次默认上下文后走同一条递归路径，
            // 避免每个 token 分支都重复判断“有没有上下文”。
            let context = self.get_default_inline_style_context();
            return self.render_inline_tokens(tokens, Some(&context));
        }
        let context = style_context.expect("checked");
        // `prefix` 在这里相当于“外层样式的重新打开序列”。
        let apply_text = context.apply_text.clone();
        let prefix = context.style_prefix.as_str();

        let mut result = String::new();
        for token in tokens {
            match token {
                Token::Latex {
                    raw, text, pending, ..
                } => {
                    // 流式未闭合（pending）或关闭数学渲染时输出源码；
                    // 支持但渲染失败（`None`）时同样回退到 `raw`。
                    let rendered = if !*pending && self.options.render_latex {
                        render_latex(text, &RenderLatexOptions::default())
                            .unwrap_or_else(|| raw.clone())
                    } else {
                        raw.clone()
                    };
                    result.push_str(&apply_text_with_newlines(&*apply_text, &rendered));
                }
                Token::Escape { raw, text } => {
                    // 默认去掉反斜杠（输出 `text`），保留选项时输出 `raw`。
                    let value = if self.options.preserve_backslash_escapes {
                        raw
                    } else {
                        text
                    };
                    result.push_str(&apply_text_with_newlines(&*apply_text, value));
                }
                Token::Text { text, tokens, .. } => {
                    // 已经行内 token 化的 text 递归渲染子 token；否则按普通文本着色。
                    if !tokens.is_empty() {
                        result.push_str(&self.render_inline_tokens(tokens, Some(context)));
                    } else {
                        result.push_str(&apply_text_with_newlines(&*apply_text, text));
                    }
                }
                Token::Paragraph { tokens, .. } => {
                    result.push_str(&self.render_inline_tokens(tokens, Some(context)));
                }
                // 行内格式的统一模式：递归渲染内部（沿用同一上下文），再套主题样式，
                // 最后重放前缀，使后续文本回到外层样式。
                Token::Strong { tokens, .. } => {
                    let inner = self.render_inline_tokens(tokens, Some(context));
                    result.push_str(&(self.theme.bold)(&inner));
                    result.push_str(prefix);
                }
                Token::Em { tokens, .. } => {
                    let inner = self.render_inline_tokens(tokens, Some(context));
                    result.push_str(&(self.theme.italic)(&inner));
                    result.push_str(prefix);
                }
                Token::Codespan { text, .. } => {
                    result.push_str(&(self.theme.code)(text));
                    result.push_str(prefix);
                }
                Token::Link {
                    text,
                    href,
                    tokens,
                    image,
                    ..
                } => {
                    if *image {
                        // 图片按纯文本输出（TS 的 default 分支）
                        result.push_str(&apply_text_with_newlines(&*apply_text, text));
                        continue;
                    }
                    let link_text = self.render_inline_tokens(tokens, Some(context));
                    let styled_link = (self.theme.link)(&(self.theme.underline)(&link_text));
                    // 终端支持 OSC 8 时把文本包成超链接；否则在没有“文字 == URL”时
                    // 追加 ` (href)`。比较前去掉 `mailto:`，避免自动链接的邮箱重复显示。
                    if get_capabilities().hyperlinks {
                        result.push_str(&hyperlink(&styled_link, href));
                    } else {
                        let href_for_comparison = href.strip_prefix("mailto:").unwrap_or(href);
                        if text == href || text == href_for_comparison {
                            result.push_str(&styled_link);
                        } else {
                            result.push_str(&styled_link);
                            result.push_str(&(self.theme.link_url)(&format!(" ({href})")));
                        }
                    }
                    result.push_str(prefix);
                }
                // 行内换行：先保留为 `\n`，后续换行阶段按列宽处理。
                Token::Br { .. } => result.push('\n'),
                Token::Del { tokens, .. } => {
                    let inner = self.render_inline_tokens(tokens, Some(context));
                    result.push_str(&(self.theme.strikethrough)(&inner));
                    result.push_str(prefix);
                }
                Token::Html { raw, .. } => {
                    result.push_str(&apply_text_with_newlines(&*apply_text, raw));
                }
                other => {
                    if let Some(text) = other.text() {
                        result.push_str(&apply_text_with_newlines(&*apply_text, text));
                    }
                }
            }
        }

        // 最后一个带样式片段可能以 `prefix` 结尾；循环删掉所有尾部副本，
        // 否则它会跟在本行文本后一起参与宽度计算/填色。
        while !prefix.is_empty() && result.ends_with(prefix) {
            let new_len = result.len() - prefix.len();
            result.truncate(new_len);
        }

        result
    }

    /// `renderToken(token, width, nextTokenType?, styleContext?)`。
    ///
    /// 把一个（块级）token 渲染成若干**逻辑行**；此时还没有按 `width` 做最终换行
    /// （行内 `Br` 产生的 `\n` 仍留在字符串里），换行、padding 与背景在
    /// `Component::render` 中统一处理。返回的行不保证不超过 `width`。
    ///
    /// `next_token_type` 只用于块间距：`None` 表示后面没有需要补空行的块（通常是文档末尾，
    /// 也包括列表条目内部的 token 与引用里的最后一个 token）；`Some("space")` 表示下一个
    /// token 自己会输出空行，因此本 token 不再补空行，避免出现两条连续空行。
    /// `width` 是内容可用宽度（已扣除外层 padding）；列表、表格、引用会把更窄的宽度继续下传。
    pub(crate) fn render_token(
        &self,
        token: &Token,
        width: usize,
        next_token_type: Option<&str>,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        // 块间距规则（`followed_by_block`）：后面还有非 space 的 token 时补一个空行；
        // space 会自己渲染成空行，文档末尾（None）不应有尾随空行。
        let followed_by_block = next_token_type.is_some_and(|kind| kind != "space");

        match token {
            Token::Heading { depth, tokens, .. } => {
                let heading_level = *depth;
                let heading_prefix = format!("{} ", "#".repeat(heading_level));

                // 标题样式：统一叠 bold；H1 额外叠 underline，H2 不显示 `#` 前缀，
                // depth ≥ 3 才把与层级同数量的 `#` 前缀一起染色（与 TS 输出一致）。
                let theme = self.theme.clone();
                let heading_style_fn: StyleFn = if heading_level == 1 {
                    Arc::new(move |text: &str| {
                        (theme.heading)(&(theme.bold)(&(theme.underline)(text)))
                    })
                } else {
                    Arc::new(move |text: &str| (theme.heading)(&(theme.bold)(text)))
                };
                let heading_style_context = InlineStyleContext {
                    apply_text: heading_style_fn.clone(),
                    style_prefix: self.get_style_prefix(&*heading_style_fn),
                };
                let heading_text = self.render_inline_tokens(tokens, Some(&heading_style_context));
                let styled_heading = if heading_level >= 3 {
                    format!("{}{}", heading_style_fn(&heading_prefix), heading_text)
                } else {
                    heading_text
                };
                lines.push(styled_heading);
                if followed_by_block {
                    // 标题后补一个空行（下一个 token 是 space 时不补）
                    lines.push(String::new());
                }
            }
            Token::Paragraph { tokens, .. } => {
                let paragraph_text = self.render_inline_tokens(tokens, style_context);
                lines.push(paragraph_text);
                // 下一个 token 是 list/space 时不补空行
                if next_token_type.is_some_and(|kind| kind != "list" && kind != "space") {
                    lines.push(String::new());
                }
            }
            Token::Text { .. } => {
                lines.push(self.render_inline_tokens(std::slice::from_ref(token), style_context));
            }
            Token::LatexBlock {
                raw, text, pending, ..
            } => {
                // 块级数学：开启渲染且非流式未闭合时按 display 模式渲染；失败或关闭时回退源码。
                // 渲染结果按行拆开，每行单独套默认文本样式（块级 token 不走行内上下文）。
                let rendered = if !*pending && self.options.render_latex {
                    render_latex(text, &RenderLatexOptions { display: true })
                        .unwrap_or_else(|| trim_js(raw).to_string())
                } else {
                    trim_js(raw).to_string()
                };
                for line in rendered.split('\n') {
                    lines.push(self.apply_default_style(line));
                }
                if followed_by_block {
                    lines.push(String::new());
                }
            }
            Token::Code { text, lang, .. } => {
                // 围栏行本身不加缩进；正文每行统一加 indent（默认两个空格）。
                // 有高亮器时其返回行被视为成品行，否则逐行套 `code_block` 样式。
                let indent = self
                    .theme
                    .code_block_indent
                    .clone()
                    .unwrap_or_else(|| "  ".to_string());
                lines.push((self.theme.code_block_border)(&format!(
                    "```{}",
                    lang.as_deref().unwrap_or("")
                )));
                if let Some(highlight) = &self.theme.highlight_code {
                    let highlighted = highlight(text, lang.as_deref());
                    for line in highlighted {
                        lines.push(format!("{indent}{line}"));
                    }
                } else {
                    for code_line in text.split('\n') {
                        lines.push(format!("{indent}{}", (self.theme.code_block)(code_line)));
                    }
                }
                lines.push((self.theme.code_block_border)("```"));
                if followed_by_block {
                    // 代码块后补空行（下一个 token 是 space 时不补）
                    lines.push(String::new());
                }
            }
            Token::List(list) => {
                lines.extend(self.render_list(list, 0, width, style_context));
            }
            Token::Table(table) => {
                lines.extend(self.render_table(table, width, next_token_type, style_context));
            }
            Token::Blockquote { tokens, .. } => {
                // 引用样式 = quote(italic(...))；prefix 在内层样式 reset 之后重新打开引用样式。
                let theme = self.theme.clone();
                let quote_style: StyleFn =
                    Arc::new(move |text: &str| (theme.quote)(&(theme.italic)(text)));
                let quote_style_prefix = self.get_style_prefix(&*quote_style);
                // 行内出现完整 reset `\x1b[0m` 时在其后补回引用样式前缀，
                // 否则引用颜色/斜体在该行剩余部分会丢失。
                let reset_replacement = format!("\x1b[0m{quote_style_prefix}");
                let apply_quote_style = |line: &str| -> String {
                    if quote_style_prefix.is_empty() {
                        quote_style(line)
                    } else {
                        let with_reapplied = line.replace("\x1b[0m", &reset_replacement);
                        quote_style(&with_reapplied)
                    }
                };

                // 宽度不足 3 列时，`width - 2` 会下溢或只剩 0 列内容：降级为无边框渲染，
                // 直接把内部 token 按原宽度递归，保证深层嵌套引用仍能输出文本。
                if width < 3 {
                    for (index, quote_token) in tokens.iter().enumerate() {
                        let next = tokens.get(index + 1).map(Token::type_name);
                        lines.extend(self.render_token(quote_token, width, next, style_context));
                    }
                } else {
                    // 内容宽度让出 "│ " 两列；内部统一用“identity apply_text + 引用前缀”的
                    // 上下文，让每个行内片段结束后都能恢复引用样式。
                    let quote_content_width = width - 2;
                    let quote_inline_style_context = InlineStyleContext {
                        apply_text: Arc::new(|text: &str| text.to_string()),
                        style_prefix: quote_style_prefix.clone(),
                    };
                    let mut rendered_quote_lines: Vec<String> = Vec::new();
                    for (index, quote_token) in tokens.iter().enumerate() {
                        let next = tokens.get(index + 1).map(Token::type_name);
                        rendered_quote_lines.extend(self.render_token(
                            quote_token,
                            quote_content_width,
                            next,
                            Some(&quote_inline_style_context),
                        ));
                    }
                    // 不要在外层引用的间距前多渲染一个空的引用行：
                    // 内部块级 token 补的尾随空行如果留下，会各自变成带 "│ " 边框的空行；
                    // 而外层 followed_by_block 还会再补一次空行，所以先弹掉这些尾行。
                    while rendered_quote_lines
                        .last()
                        .is_some_and(|line| line.is_empty())
                    {
                        rendered_quote_lines.pop();
                    }
                    for quote_line in &rendered_quote_lines {
                        // 整行先套引用样式再换行；每个换行片段都要补回 "│ " 边框。
                        let styled_line = apply_quote_style(quote_line);
                        for wrapped_line in wrap_text_with_ansi(&styled_line, quote_content_width) {
                            lines.push(format!(
                                "{}{}",
                                (self.theme.quote_border)("│ "),
                                wrapped_line
                            ));
                        }
                    }
                    if followed_by_block {
                        lines.push(String::new());
                    }
                }
            }
            Token::Hr { .. } => {
                // 分隔线长度取 width 与 80 的较小值。
                lines.push((self.theme.hr)(&"─".repeat(width.min(80))));
                if followed_by_block {
                    lines.push(String::new());
                }
            }
            Token::Html { raw, .. } => {
                // HTML 作为纯文本渲染（去掉首尾空白后套默认样式）
                lines.push(self.apply_default_style(trim_js(raw)));
            }
            // 空行 token：产生一个空逻辑行，后续填充阶段会把它补满到 width。
            Token::Space { .. } => lines.push(String::new()),
            other => {
                // 其余 token 若没有 `text`（如 def、checkbox）则不产生行；
                // 任务列表的复选框由列表 marker 负责输出。
                if let Some(text) = other.text() {
                    lines.push(text.to_string());
                }
            }
        }

        lines
    }
}

impl Component for Markdown {
    /// `render(width)`：按终端总宽度生成屏幕行。
    ///
    /// 缓存键是（原始 `text`, `width`）；`transform` 的输出不单独作为键，因此同一宽度下
    /// 重复调用不会再次执行 `transform`——需要重新变换时调用 [`Markdown::invalidate`]。
    /// 变换后的文本 trim 为空时返回空 `Vec`（不产生 padding 行）。
    fn render(&mut self, width: usize) -> Vec<String> {
        // 缓存命中：上次渲染保存的原始文本与宽度都与本次相同。
        if let (Some(lines), Some(text), Some(cached_width)) =
            (&self.cached_lines, &self.cached_text, self.cached_width)
            && text == &self.text
            && cached_width == width
        {
            return lines.clone();
        }

        // 内容可用宽度（扣除左右 padding）
        let content_width = width
            .saturating_sub(self.padding_x.saturating_mul(2))
            .max(1);
        // 变换函数拿到的是内容宽度（已扣除左右 padding），不是终端总宽度；
        // `caches_transformed_markdown_by_source_and_available_width` 用 80/2 → 76 锁定。
        let text = match &self.options.transform {
            Some(transform) => transform(&self.text, content_width),
            None => self.text.clone(),
        };

        // 没有实际文本时不渲染；空结果同样写入缓存，避免重复 transform。
        if trim_js(&text).is_empty() {
            self.cached_text = Some(self.text.clone());
            self.cached_width = Some(width);
            self.cached_lines = Some(Vec::new());
            return Vec::new();
        }

        // 制表符统一替换为 3 个空格（在 token 化之前完成，因此后续宽度计算看到的都是空格）。
        let normalized_text = text.replace('\t', "   ");

        // 解析（token 缓存只依赖源文本）：宽度不进键，文本相同就复用 token 树。
        let tokens_rc: Rc<TokenCache> = match &self.cached_tokens {
            Some(cache) if cache.source == normalized_text => cache.clone(),
            _ => {
                let mut lexer = Lexer::new();
                let mut tokens = lexer.lex(&normalized_text);
                // 流式输出可能停在未闭合的围栏上：只修剪最后一个 token
                // （必要时递归进列表/引用的最后一项），让半行围栏不显示为代码内容。
                trim_partial_closing_fences(&mut tokens);
                let cache = Rc::new(TokenCache {
                    source: normalized_text,
                    tokens,
                });
                self.cached_tokens = Some(cache.clone());
                cache
            }
        };

        let mut rendered_lines: Vec<String> = Vec::new();
        for (index, token) in tokens_rc.tokens.iter().enumerate() {
            // `next_token_type` 决定块级 token 是否补空行；`None` 表示文档最后一个 token。
            let next_token_type = tokens_rc.tokens.get(index + 1).map(Token::type_name);
            rendered_lines.extend(self.render_token(token, content_width, next_token_type, None));
        }

        // 换行（还没有 padding / background）
        let mut wrapped_lines: Vec<String> = Vec::new();
        for line in &rendered_lines {
            if is_image_line(line) {
                // 图片协议序列不能按可见文本切分，整行原样保留。
                wrapped_lines.push(line.clone());
            } else {
                wrapped_lines.extend(wrap_text_with_ansi(line, content_width));
            }
        }

        // 加边距与背景：两条互斥路径，最终每个非图片行的可见宽度都补到 width；
        // 图片行由本循环的 is_image_line 分支原样推入，不加边距/背景。
        // 有背景色时交给 apply_background_to_line（先补空格再整行着色）；
        // 否则手动补右侧空格。
        let left_margin = " ".repeat(self.padding_x);
        let bg_fn = self
            .default_text_style
            .as_ref()
            .and_then(|style| style.bg_color.clone());
        let mut content_lines: Vec<String> = Vec::new();
        for line in &wrapped_lines {
            if is_image_line(line) {
                content_lines.push(line.clone());
                continue;
            }
            let line_with_margins = format!("{left_margin}{line}{left_margin}");
            if let Some(background) = &bg_fn {
                content_lines.push(apply_background_to_line(
                    &line_with_margins,
                    width,
                    &**background,
                ));
            } else {
                let visible_len = visible_width(&line_with_margins);
                content_lines.push(format!(
                    "{line_with_margins}{}",
                    " ".repeat(width.saturating_sub(visible_len))
                ));
            }
        }

        // 上下 padding（空行）：每行是 width 个空格；有背景色时同样整行着色。
        let empty_line = " ".repeat(width);
        let mut empty_lines: Vec<String> = Vec::new();
        for _ in 0..self.padding_y {
            empty_lines.push(match &bg_fn {
                Some(background) => apply_background_to_line(&empty_line, width, &**background),
                None => empty_line.clone(),
            });
        }

        let mut result: Vec<String> = Vec::new();
        result.extend(empty_lines.iter().cloned());
        result.extend(content_lines);
        result.extend(empty_lines);
        // `flatten_lines` 在 Rust 侧是 no-op（对应 V8 的字符串扁平化提示），
        // 保留调用只为对齐 TS 的代码结构。
        flatten_lines(&mut result);

        self.cached_text = Some(self.text.clone());
        self.cached_width = Some(width);
        self.cached_lines = Some(result.clone());

        // 源文本非空、但所有 token 都不产生可见行（例如只包含链接定义），
        // 且 padding_y = 0 时 result 为空；此时退化为一个空行而不是空 Vec。
        if result.is_empty() {
            vec![String::new()]
        } else {
            result
        }
    }

    /// TS 的 `Markdown` 没有 `handleInput` 成员：作为聚焦的 overlay 时，
    /// `handleTerminalInput` 既不调用它也不请求立即渲染（`tui.ts:1147`）。
    /// `tests/markdown.rs::markdown_component_does_not_handle_input` 锁定该返回值。
    fn handles_input(&self) -> bool {
        false
    }

    /// TS `Markdown.invalidate()`：清渲染缓存，下一次 `render` 重新执行 `transform`。
    ///
    /// 之前只在固有方法 [`Markdown::invalidate`] 里实现，导致通过 `Container.invalidate()`
    /// 的转发（`&mut dyn Component`）命中 trait 默认空实现——`user-message.test.ts` 的
    /// “reapplies Markdown transformers when invalidated” 依赖这条转发。
    fn invalidate(&mut self) {
        Markdown::invalidate(self);
    }
}

/// `applyTextWithNewlines(text)`：按 `\n` 分段分别应用，再原样拼回。
///
/// 每个分段（包括空段，例如 `"a\n\nb"` 的中间段）都单独传给 `apply_text`，
/// 因此样式函数只需要处理单行文本，不必自己处理换行。
fn apply_text_with_newlines(apply_text: &dyn Fn(&str) -> String, text: &str) -> String {
    text.split('\n')
        .map(apply_text)
        .collect::<Vec<String>>()
        .join("\n")
}

/// `applyDefaultStyle(text)` 的纯函数版本。
///
/// 样式按 `color → bold → italic → strikethrough → underline` 的顺序逐层包裹，
/// 后应用的样式在外层；顺序变化会改变 ANSI 嵌套结果，因此顺序固定且改动需同步测试。
/// 没有默认样式时原样返回。
fn apply_default_style_with(
    theme: &MarkdownTheme,
    style: Option<&DefaultTextStyle>,
    text: &str,
) -> String {
    let Some(style) = style else {
        return text.to_string();
    };
    let mut styled = text.to_string();
    if let Some(color) = &style.color {
        styled = color(&styled);
    }
    if style.bold {
        styled = (theme.bold)(&styled);
    }
    if style.italic {
        styled = (theme.italic)(&styled);
    }
    if style.strikethrough {
        styled = (theme.strikethrough)(&styled);
    }
    if style.underline {
        styled = (theme.underline)(&styled);
    }
    styled
}
