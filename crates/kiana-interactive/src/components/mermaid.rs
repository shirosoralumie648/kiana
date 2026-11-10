//! components/mermaid.ts。
//!
//! 把顶层 Mermaid 代码块替换成 Unicode 终端图（grok-mermaid 的 flowchart 子集移植）。
//! 非 flowchart / 超宽 / 语法不支持时保留原始代码块；未流式时把警告附在代码块后。
//!
//! 与 grok-mermaid 的 `render()` 一致，解析失败时会去掉最后一行重试一次
//! （`attempt()`）：这既是流式（最后一行未输完）的正常路径，也让超过节点/边上限的
//! flowchart 退回到能画的前缀，并在最终渲染显示
//! `Mermaid diagram not rendered: dropped, unreadable final line: ...`。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/mermaid.ts`；
//! 渲染算法移植自 `grok-mermaid@0.2.3`（Apache-2.0）的 `parse.ts`/`layout.ts`/`canvas.ts`/`labels.ts`。
//!
//! # 与 grok-mermaid 的差异
//!
//! - 只支持 flowchart（`graph`/`flowchart`）；state/class/ER/sequence 与含 `subgraph` 的图
//!   返回 `None`（调用方保留原始代码块）。
//! - 宽度与绘制都按字素簇（`unicode-segmentation`，等价于 `Intl.Segmenter`），簇宽取
//!   各码点按 `unicode-width` 计量的最大值，并在含 VS16 或 ≥2 个区域指示符时为 2
//!   （`width.ts` 的 `clusterWidth`）。
//!
//! ## Public API for InteractiveMode
//!
//! - [`create_mermaid_markdown_transformer`]（[`MermaidTransformerOptions`]：`get_mode` + 可选主题）
//! - [`MermaidTheme`] / [`GlobalMermaidTheme`]（生产主题；测试可注入假主题）
//! - [`diagram_kind`] / [`render_mermaid_plain`]（诊断）
//!
//! ## 为什么需要它
//!
//! 终端只能输出字符网格，显示不了 Mermaid 的 SVG。本模块在进程内把 flowchart 解析成
//! 节点和边、做分层布局、再画到字符画布上，让消息里的 Mermaid 代码块直接变成可读的图；
//! 画不出来时原样保留代码块，信息不会丢。
//!
//! ## 一次转换的执行流程
//!
//! 1. [`create_mermaid_markdown_transformer`] 返回一个闭包；每次调用先读 `get_mode()`，
//!    按模式、消息类型与流式状态决定是否直接返回原文。
//! 2. 用 `kiana_tui::markdown::Lexer` 切出顶层 token；只有 info string 首词是 `mermaid`
//!    的 `Code` token 参与替换，其余 token 用 `raw()` 原样拷贝。
//! 3. `render_mermaid`：去掉控制字符 → 解析成 `Graph` → 布局并绘制成 `Canvas` →
//!    `to_lines` 产出 plain/styled 行；整图失败时去掉最后一行再试一次。
//! 4. 图宽超过 `context.available_width` 或渲染返回 `None` 时保留原始代码块；非流式且
//!    解析期有警告时也保留原块，并在后面追加一行警告；其余情况逐行包成行内代码输出。
//!
//! ## 关键数据结构
//!
//! - `Graph`：解析结果（节点、边、`id → 下标` 映射、方向、警告）。节点数或边数超过上限
//!   时置 `over_cap`，整张图放弃渲染。
//! - `Canvas`：固定宽高的字符网格。每格记录字素簇、`SpanClass`、方向位（`U`/`D`/`L`/`R`）、
//!   线型（实线/点线/粗线）与 `occupied`（节点框占位）。框线与连线先写方向位，最后由
//!   `finalize_mask` 合成盒线字符。
//! - `MermaidArt`：最终产物；`plain` 与 `styled` 行一一对应，`width` 是终端列宽，
//!   `warnings` 是解析期被丢弃语句的记录。
//!
//! ## 注意事项
//!
//! - 只替换**顶层**代码块：`Lexer::lex` 返回的顶层 token 才参与，块引用/列表内部的
//!   code token 不会替换。
//! - 宽度一律按终端列（字素簇，见 `string_width`）计算；`available_width` 是 `Markdown`
//!   组件扣掉左右 padding 后的内容宽度。
//! - 变换结果会作为 markdown 重新解析（`kiana_tui::components::markdown` 在 token 化
//!   之前调用变换器），所以图的每一行都包成行内代码，按字面文本渲染；行间用 `"  \n"`
//!   （两个空格 + 换行，CommonMark 的硬换行写法）连接。
//! - 单个代码块失败只影响它自己：其余 token 照常输出，函数本身不返回错误。
//!
//! ## 阅读顺序
//!
//! 1. [`create_mermaid_markdown_transformer`]：对外入口、跳过条件与 token 级替换策略；
//! 2. `parse_graph` → `parse_statement` → `parse_link`：把语句解析成 `Graph`；
//! 3. `compute_ranks`、`place_td` / `place_lr`：分层与布局；
//! 4. `Canvas`、`draw_box` 与各 `route_*`：把布局画成字符网格；
//! 5. `render_mermaid`：失败时去掉最后一行重试，并汇总 `warnings`。
//!
//! 建议按这个顺序读：先看入口能弄清“什么时候画”，再看解析/布局/绘制三段如何交接。
//!
//! ## 修改影响
//!
//! - 输出文本被 `crates/kiana-interactive/tests/components_mermaid.rs` 的 golden 锁定
//!   （简单用例、主题着色、流式/最终模式、15 个布局变体、字素簇宽度等；数据来自
//!   `crates/kiana-interactive/tests/fixtures/render_goldens_dark.json`）。改动布局常量、
//!   字形表或路由方式都会让这些断言失败，需要同时核对 grok-mermaid/TS 的对应输出。
//! - 公开面只有入口及其选项、主题接口与诊断函数（含 [`DiagramKind`]）；`Graph`、`Canvas`、
//!   `MermaidArt` 等都是私有实现，可以重组而不影响下游。
//! - 变换器在 `interactive_mode` 组装变换器链时排在第一位，因此后续的扩展变换器看到的是
//!   已经替换好的图文本，而不是原始 Mermaid 代码块。

use std::collections::HashMap;
use std::sync::Arc;

use kiana_config::settings::MermaidRenderingMode;
use kiana_ext_api::types::{MarkdownMessageType, MarkdownTransformContext, MarkdownTransformer};
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::markdown::{Lexer, Token};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

// ============================================================================
// 主题
// ============================================================================

/// Mermaid 需要的主题面（TS 的 `Theme` 只用 `fg` 与 `bold`）。
///
/// 用 trait 而不是具体 `Theme`，是为了让测试可以注入 `<token>text</token>` 形式的假主题
/// （TS 测试同样伪造 `fg`/`bold`）。
///
/// trait 要求 `Send + Sync`：`Arc<dyn MermaidTheme>` 被变换器闭包持有，而
/// `MarkdownTransformer` 的别名本身就是 `Arc<dyn Fn(..) -> String + Send + Sync>`
/// （`kiana_ext_api::types`），主题必须能随闭包跨线程共享。
///
/// 渲染时每个非 `None` 类别的 `Span` 调一次 `fg`（按 `SpanClass` 选 token），警告行再调
/// 一次 `fg`；`bold` 在当前实现里没有调用点（trait 仍照 TS 的 `Theme` 保留这两个方法）。
pub trait MermaidTheme: Send + Sync {
    /// `theme.fg(color, text)`。
    fn fg(&self, color: ThemeColor, text: &str) -> String;
    /// `theme.bold(text)`。
    fn bold(&self, text: &str) -> String;
}

/// 生产主题：每次调用读全局主题代理（主题切换后旧 transformer 立刻换色）。
///
/// `fg`/`bold` 直接转发到 `kiana_theme::THEME`；主题未初始化时 `.expect` 会 panic（对应
/// TS 在未初始化主题上取色抛错）。渲染链上这个 panic 会被
/// `components/markdown_transform.rs` 的静默 `catch_unwind` 捕获，结果是这次变换的输出
/// 被丢弃、markdown 保持进入变换器前的样子，而不是进程崩溃。
pub struct GlobalMermaidTheme;

impl MermaidTheme for GlobalMermaidTheme {
    fn fg(&self, color: ThemeColor, text: &str) -> String {
        kiana_theme::THEME
            .fg(color, text)
            .expect("theme must be initialized before rendering (TS throws here)")
    }

    fn bold(&self, text: &str) -> String {
        kiana_theme::THEME
            .bold(text)
            .expect("theme must be initialized before rendering (TS throws here)")
    }
}

/// `interface MermaidTransformerOptions`（TS 不导出）。
///
/// 生产构造点在 `interactive_mode.rs`：`get_mode` 读取当前会话 `SettingsManager` 的
/// `mermaid_rendering_mode`（会话替换后跟着换），`theme` 是 `Arc<GlobalMermaidTheme>`。
/// 测试用假主题替换 `theme`、用固定值替换 `get_mode`。
pub struct MermaidTransformerOptions {
    /// 当前渲染模式（每次转换时读取）。
    pub get_mode: Box<dyn Fn() -> MermaidRenderingMode + Send + Sync>,
    /// 主题；`None` 时输出无 ANSI 的 `art.plain`。
    pub theme: Option<Arc<dyn MermaidTheme>>,
}

/// 判断 token 是否是 Mermaid 代码块；是则返回代码内容。
///
/// 只匹配 `Code` token，且 info string 的**第一个空白分隔词**（转小写后）等于 `mermaid`，
/// 因此 `Mermaid`、`mermaid title` 都算；缩进代码块（`lang == None`）与其它语言不算。
fn is_mermaid(token: &Token) -> Option<&str> {
    match token {
        Token::Code { lang, text, .. } => {
            let lang = lang.as_deref()?;
            let first = lang.split_whitespace().next().unwrap_or("");
            (first.to_lowercase() == "mermaid").then_some(text.as_str())
        }
        _ => None,
    }
}

/// CommonMark 内联代码：围栏长度 = 最长反引号 run + 1；空行用 NBSP。
///
/// 为什么逐行包裹：变换结果会被 markdown 渲染器重新解析，图的每一行必须按字面文本显示，
/// 行首空格和连续的盒线字符不能被当成块级/行内语法。内容以反引号开头或结尾时两侧各补
/// 一个空格，是 CommonMark 对“围栏与内容粘连”的要求。
fn code_span(line: &str) -> String {
    let content = if line.is_empty() { "\u{00a0}" } else { line };
    let mut longest = 0usize;
    let mut run = 0usize;
    for ch in content.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest + 1);
    let padding = if content.starts_with('`') || content.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{padding}{content}{padding}{fence}")
}

// `SpanClass` → 主题 token 的固定映射：边框 BorderMuted、节点文字 Text、连线 Accent、
// 连线标签 Muted；`None` 表示该格没有语义样式，原样透传。
fn style_span(span: &Span, theme: &dyn MermaidTheme) -> String {
    match span.cls {
        SpanClass::Border => theme.fg(ThemeColor::BorderMuted, &span.text),
        SpanClass::Text => theme.fg(ThemeColor::Text, &span.text),
        SpanClass::Edge => theme.fg(ThemeColor::Accent, &span.text),
        SpanClass::EdgeLabel => theme.fg(ThemeColor::Muted, &span.text),
        SpanClass::None => span.text.clone(),
    }
}

// 把画布行转成带 ANSI 的字符串行。同一行内类别相同的相邻格已在 `to_lines` 合并成
// 一个 `Span`，这里每个 Span 只调一次 `fg`。
fn themed_lines(art: &MermaidArt, theme: &dyn MermaidTheme) -> Vec<String> {
    art.styled
        .iter()
        .map(|row| {
            row.iter()
                .map(|span| style_span(span, theme))
                .collect::<Vec<String>>()
                .join("")
        })
        .collect()
}

/// `createMermaidMarkdownTransformer(options)`。
///
/// 返回 `MarkdownTransformer`（`Arc<dyn Fn(&str, &MarkdownTransformContext) -> String>`），
/// 由 `InteractiveMode` 的 markdown 变换器链调用；`Markdown` 组件在渲染缓存未命中时执行
/// 一次（缓存键是原文与终端宽度），同一原文与宽度不会重复变换。
///
/// # 跳过条件（原样返回输入 markdown，不做任何改写）
///
/// - `(options.get_mode)()` 是 `MermaidRenderingMode::Off`；
/// - `context.message_type` 是 `AssistantThinking`（思考块里的 Mermaid 不画）；
/// - `context.is_streaming` 且模式不是 `MermaidRenderingMode::Streaming`（Final 模式只在
///   流式结束后才画）。
///
/// # 单个代码块的处理
///
/// - 只有 info string 首词为 `mermaid`（大小写不敏感）的**顶层** `Code` token 会被替换。
/// - `render_mermaid` 返回 `None`（空输入、非 flowchart、含 `subgraph`、超节点/边上限、
///   画布超限）→ 原样输出该 token 的 `raw()`。
/// - 图宽 `art.width`（终端列）超过 `context.available_width` → 原样输出。
/// - 非流式且 `warnings` 非空 → 原样输出代码块，随后追加一行行内代码形式的警告：只显示
///   第一条，其余聚合成 `" (+N more)"`。
/// - 其余情况输出图：每行包成行内 code span，行间用 `"  \n"`（硬换行）连接，末尾补 `\n`。
///
/// # 其他
///
/// - 返回类型是 `String`，函数本身不返回错误；单个块失败不影响同一 markdown 里的其它块。
/// - `get_mode` 在**每次调用**（每次变换）时读一次，而不是每个代码块读一次。
/// - 返回的闭包持有 `Arc<MermaidTransformerOptions>`，可以克隆并跨线程传递（类型要求
///   `Send + Sync`）。
/// - 若 theme 实现 panic（例如全局主题未初始化），在 `InteractiveMode` 的渲染链上会被
///   静默捕获并保留变换前的文本；直接调用该闭包时 panic 会向上传播。
pub fn create_mermaid_markdown_transformer(
    options: MermaidTransformerOptions,
) -> MarkdownTransformer {
    let options = Arc::new(options);
    Arc::new(move |markdown: &str, context: &MarkdownTransformContext| {
        // 每次变换读一次模式：设置面板切换后，已构造的 transformer 立即按新模式工作。
        let mode = (options.get_mode)();
        // 三种情况完全不动 markdown：关闭渲染、思考块、以及“只在最终渲染出图”时仍在流式。
        if mode == MermaidRenderingMode::Off
            || context.message_type == MarkdownMessageType::AssistantThinking
            || (context.is_streaming && mode != MermaidRenderingMode::Streaming)
        {
            return markdown.to_string();
        }

        let mut lexer = Lexer::new();
        let tokens = lexer.lex(markdown);
        let mut out = String::new();
        // 逐个顶层 token 重建输出：非 Mermaid token 用 `raw()` 原样拼回，保证块外内容
        // 逐字不变（包括原有围栏、缩进与空行）。
        for token in &tokens {
            let Some(code) = is_mermaid(token) else {
                out.push_str(token.raw());
                continue;
            };
            let raw = token.raw();
            let Some(art) = render_mermaid(code) else {
                // 画不出来（非 flowchart / subgraph / 超上限 / 画布过大）：保留原始块。
                out.push_str(raw);
                continue;
            };
            if art.width > context.available_width {
                // 图比可用宽度还宽：宁可显示源码，也不输出会被折行的图。
                out.push_str(raw);
                continue;
            }
            if !context.is_streaming && !art.warnings.is_empty() {
                // 最终渲染且解析期丢弃过语句：保留原始块并附一行警告。只展示第一条，
                // 其余聚合成 " (+N more)"；警告行同样包成行内代码。
                let suffix = if art.warnings.len() > 1 {
                    format!(" (+{} more)", art.warnings.len() - 1)
                } else {
                    String::new()
                };
                let warning = format!("Mermaid diagram not rendered: {}{suffix}", art.warnings[0]);
                let styled_warning = match &options.theme {
                    Some(theme) => theme.fg(ThemeColor::Warning, &warning),
                    None => warning,
                };
                out.push_str(raw);
                out.push('\n');
                out.push_str(&code_span(&styled_warning));
                out.push_str("  \n");
                continue;
            }
            let lines = match &options.theme {
                Some(theme) => themed_lines(&art, theme.as_ref()),
                None => art.plain.clone(),
            };
            // 正常出图：每行单独包成行内代码，行间用 "  \n"（硬换行）连接，末尾补一个
            // 换行把图与后面的 markdown 分开。
            out.push_str(
                &lines
                    .iter()
                    .map(|line| code_span(line))
                    .collect::<Vec<String>>()
                    .join("  \n"),
            );
            out.push('\n');
        }
        out
    })
}

// ============================================================================
// MermaidArt
// ============================================================================

/// `Span.cls`：一个格子的着色语义，决定它最终用哪个主题 token。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpanClass {
    Border,
    Text,
    Edge,
    EdgeLabel,
    None,
}

/// 一段同类的相邻单元（`to_lines` 合并连续同类格子得到的最小着色单位）。
#[derive(Debug, Clone)]
struct Span {
    text: String,
    cls: SpanClass,
}

/// 渲染结果（`MermaidArt`）。
///
/// `plain` 与 `styled` 行一一对应（`styled` 是 `plain` 按 `SpanClass` 切段的结果）；
/// `width` 是全部行中最右侧非空格的右边一列（即渲染所需列数，终端列），调用方用它和
/// `available_width` 比较；有任何解析期警告时，非流式的最终渲染会退回原始代码块。
#[derive(Debug, Clone, Default)]
struct MermaidArt {
    plain: Vec<String>,
    styled: Vec<Vec<Span>>,
    width: usize,
    warnings: Vec<String>,
}

// ============================================================================
// 宽度与标签
// ============================================================================

/// 单个码点的列宽：`unicode-width` 对单字符串的宽度（grok-mermaid `WIDTHS` 的来源）。
fn code_point_width(ch: char) -> usize {
    let mut buffer = [0u8; 4];
    UnicodeWidthStr::width(ch.encode_utf8(&mut buffer))
}

/// `clusterWidth(cluster)`：簇内最宽的码点胜出；VS16 或区域指示符对强制 2 列。
///
/// TS 用 `Intl.Segmenter` 分簇后逐簇测量；这里用 `unicode-segmentation` 的扩展字素簇
/// 复刻同一分簇。典型结果：`e` + 组合重音仍算 1 列；由宽 2 的 emoji 组成的 ZWJ 序列取
/// 最大值仍是 2；带 VS16 的字符（emoji 呈现选择符）与两个区域指示符（国旗）强制算 2。
fn cluster_width(cluster: &str) -> usize {
    const VS16: u32 = 0xFE0F;
    let mut width = 0usize;
    let mut vs16 = false;
    let mut regional = 0usize;
    for ch in cluster.chars() {
        let cp = ch as u32;
        if cp == VS16 {
            vs16 = true;
        }
        if (0x1F1E6..=0x1F1FF).contains(&cp) {
            regional += 1;
        }
        width = width.max(code_point_width(ch));
    }
    if vs16 || regional >= 2 { 2 } else { width }
}

// `string_width(s)`：按扩展字素簇逐簇累加 `cluster_width`，得到终端列宽。
// 所有宽度比较（换行、截断、available_width）都走这里，保证与绘制时用的是同一套度量。
fn string_width(text: &str) -> usize {
    UnicodeSegmentation::graphemes(text, true)
        .map(cluster_width)
        .sum()
}

/// `measured(s)`：`(字素簇, 列宽)` 序列。
fn measured(text: &str) -> Vec<(&str, usize)> {
    UnicodeSegmentation::graphemes(text, true)
        .map(|cluster| (cluster, cluster_width(cluster)))
        .collect()
}

// 只折叠 ASCII 大小写：调用点（Mermaid 关键字、方向、语句类型）全部是 ASCII，
// 用 `to_lowercase` 的多字符展开反而可能改变长度。
fn ascii_lower(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_uppercase() {
                ch.to_ascii_lowercase()
            } else {
                ch
            }
        })
        .collect()
}

// 与 `ascii_lower` 对称；只用于方向关键字（LR/RL/BT）的比较。
fn ascii_upper(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_lowercase() {
                ch.to_ascii_uppercase()
            } else {
                ch
            }
        })
        .collect()
}

// 标签排版常量：超过 WRAP_WIDTH 列换行、最多 MAX_LINES 行（多出的行用 "…" 截断）；
// 连线标签最多 MAX_LABEL 列；LABEL_BREAK_CHARS 是超长单词的优先断行位置（标识符里常见
// 的分隔符），见 `wrap_label`。
const WRAP_WIDTH: usize = 24;
const MAX_LINES: usize = 4;
const MAX_LABEL: usize = 28;
const LABEL_BREAK_CHARS: [char; 4] = ['_', '-', '.', '/'];

/// C0/C1 控制字符（`\t\n\r` 除外）。
///
/// 保留制表、换行、回车，是因为它们分别由 `skip_spaces`、`src_lines` 与
/// `strip_suffix('\r')` 处理；其余控制字符会被原样写进字符网格，只会破坏终端显示。
fn strip_controls(src: &str) -> String {
    src.chars()
        .filter(|ch| {
            let code = *ch as u32;
            !(code <= 0x08
                || code == 0x0b
                || code == 0x0c
                || (0x0e..=0x1f).contains(&code)
                || (0x7f..=0x9f).contains(&code))
        })
        .collect()
}

/// Rust `str::lines()` 语义：按 `\n` 切、去掉行尾 `\r`、结尾换行不产生空行。
fn src_lines(src: &str) -> Vec<String> {
    let mut out: Vec<String> = src
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect();
    if out.last().is_some_and(|line| line.is_empty()) {
        out.pop();
    }
    out
}

// 节点 id 的字符集：Unicode 字母/数字与 '_'。`parse_node` 用它扫描 id 以及 `:::`
// 后面的样式类名。
fn is_id_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

// 解析实体名主体（不含 '&' 与 ';'）：只认 5 个命名实体和十进制/十六进制数字实体。
// 数字实体必须是合法码点、不是代理区、也不在 C0/C1 控制区，否则返回 None，调用方保留
// 原字符。
fn decode_entity_body(body: &str) -> Option<char> {
    match body {
        "lt" => return Some('<'),
        "gt" => return Some('>'),
        "amp" => return Some('&'),
        "quot" => return Some('"'),
        "apos" => return Some('\''),
        _ => {}
    }
    let digits = body.strip_prefix('#')?;
    let (hex, digits) = match digits.strip_prefix(['x', 'X']) {
        Some(rest) => (true, rest),
        None => (false, digits),
    };
    if digits.is_empty()
        || !digits.chars().all(|ch| {
            if hex {
                ch.is_ascii_hexdigit()
            } else {
                ch.is_ascii_digit()
            }
        })
    {
        return None;
    }
    let code = u32::from_str_radix(digits, if hex { 16 } else { 10 }).ok()?;
    if code > 0x10ffff || (0xd800..=0xdfff).contains(&code) {
        return None;
    }
    if code < 0x20 || (0x7f..=0x9f).contains(&code) {
        return None;
    }
    char::from_u32(code)
}

// 还原 `&...;` 形式的 HTML 实体。只在一个 '&' 之后最多 10 个字符内找 ';'
// （`i + 1 + 10`），找不到或识别失败时原样输出 '&' 再继续，因此不完整/未知的实体会逐字
// 保留，不会吞掉后面的文本。
fn decode_html_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '&' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let hi = (i + 1 + 10).min(chars.len());
        let mut semi = None;
        for (j, ch) in chars.iter().enumerate().take(hi).skip(i + 1) {
            if *ch == ';' {
                semi = Some(j);
                break;
            }
        }
        let decoded = semi.and_then(|semi| {
            let body: String = chars[i + 1..semi].iter().collect();
            decode_entity_body(&body)
        });
        match (semi, decoded) {
            (Some(semi), Some(decoded)) => {
                out.push(decoded);
                i = semi + 1;
            }
            _ => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

// 去掉标注文字里的轻量 markdown：所有反引号、`**`/`__`，以及不在单词内部的 `*`/`_`。
// `in_word` 检查（前后都是字母数字）让 `snake_case`、`a*b` 这类标识符保持原样；
// 最后 trim 首尾空白。
fn strip_markdown(text: &str) -> String {
    let no_code: String = text.chars().filter(|ch| *ch != '`').collect();
    let no_strong = no_code.replace("**", "").replace("__", "");
    let chars: Vec<char> = no_strong.chars().collect();
    let mut out = String::new();
    for (i, ch) in chars.iter().enumerate() {
        let in_word = i > 0
            && chars[i - 1].is_alphanumeric()
            && chars.get(i + 1).is_some_and(|next| next.is_alphanumeric());
        if (*ch == '*' || *ch == '_') && !in_word {
            continue;
        }
        out.push(*ch);
    }
    out.trim().to_string()
}

// 允许被剥掉的 HTML 格式标签白名单（按小写比较）。不在此表的标签（如 `<div>`）连同尖括号
// 原样保留；`<br>` 单独处理成空格（见 `strip_html_tags`）。
const HTML_FORMAT_TAGS: [&str; 26] = [
    "b", "strong", "i", "em", "u", "s", "strike", "del", "ins", "mark", "small", "big", "sub",
    "sup", "code", "kbd", "samp", "var", "tt", "span", "font", "q", "abbr", "cite", "pre", "br",
];

// 从 `<` 起尝试识别一个标签，返回 (标签名, 标签结束后的下标)。支持 `</name>`；名字只取
// ASCII 字母数字；遇到另一个 `<` 或输入结束就放弃（返回 None），避免把 `a < b` 之类的
// 普通文本误判成标签。
fn html_tag_at(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut i = start + 1;
    if chars.get(i) == Some(&'/') {
        i += 1;
    }
    let name_start = i;
    while chars.get(i).is_some_and(|ch| ch.is_ascii_alphanumeric()) {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name: String = chars[name_start..i].iter().collect();
    while let Some(ch) = chars.get(i) {
        if *ch == '>' {
            return Some((name, i + 1));
        }
        if *ch == '<' {
            return None;
        }
        i += 1;
    }
    None
}

// 剥掉白名单里的格式标签；`<br>` 换成空格，未知标签（含只有半个 `<` 的文本）按普通
// 字符保留。
fn strip_html_tags(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '<'
            && let Some((name, end)) = html_tag_at(&chars, i)
        {
            let lower = name.to_lowercase();
            if lower == "br" {
                out.push(' ');
                i = end;
                continue;
            }
            if HTML_FORMAT_TAGS.contains(&lower.as_str()) {
                i = end;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

// 去掉两端成对的引号或反引号：长度足够且同时以 `open` 开头、`close` 结尾时取中间部分，
// 返回 None 表示“不是成对包裹”。
fn unwrap_pair(text: &str, open: &str, close: &str) -> Option<String> {
    if text.len() >= open.len() + close.len() && text.starts_with(open) && text.ends_with(close) {
        Some(text[open.len()..text.len() - close.len()].to_string())
    } else {
        None
    }
}

// 标签清洗的固定顺序：去 HTML 标签 → 去两端成对引号 → 去两端成对反引号（有反引号时
// 该段按 markdown 处理）→ 解 HTML 实体。
// 顺序不能随意交换：先解实体的话，`&lt;b&gt;` 会变成 `<b>` 并被当成真实标签剥掉。
fn clean_label(raw: &str) -> String {
    let trimmed = strip_html_tags(raw.trim()).trim().to_string();
    let unquoted = unwrap_pair(&trimmed, "\"", "\"")
        .or_else(|| unwrap_pair(&trimmed, "'", "'"))
        .unwrap_or(trimmed)
        .trim()
        .to_string();
    match unwrap_pair(&unquoted, "`", "`") {
        Some(md) => decode_html_entities(&strip_markdown(md.trim())),
        None => decode_html_entities(&unquoted),
    }
}

// 在字符串里找最后一个 `_`、`-`、`.` 或 `/`（取最靠右者），返回字符下标；用于超长单词
// 的优先断行位置。
fn last_break(text: &str) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();
    let mut best: Option<usize> = None;
    for target in LABEL_BREAK_CHARS {
        if let Some(index) = chars.iter().rposition(|ch| *ch == target) {
            best = Some(best.map_or(index, |current| current.max(index)));
        }
    }
    best
}

/// `wrapLabel(label, width, maxLines)`。
///
/// 三个阶段：
/// 1. 按空白分词，能放下就追加到当前行（词间一个空格），放不下就换行；
/// 2. 单个词比 `width` 还宽时按字素簇逐列切块，切块点优先选块内最后一个
///    `_`、`-`、`.`、`/`（`last_break`），没有就硬切；
/// 3. 行数超过 `max_lines` 时截断，并在最后一行留一列放 `…`。
///
/// `width` 会被抬到至少 1，所以零宽与 1 列等价。返回值至少一行（可能是空串）。
fn wrap_label(label: &str, width: usize, max_lines: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;

    for word in label.split_whitespace() {
        let ww = string_width(word);
        if ww > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let mut chunk = String::new();
            let mut chunk_w = 0usize;
            for (ch, cw) in measured(word) {
                if chunk_w + cw > width && !chunk.is_empty() {
                    let chars: Vec<char> = chunk.chars().collect();
                    match last_break(&chunk) {
                        Some(position) => {
                            let head: String = chars[..=position].iter().collect();
                            let carry: String = chars[position + 1..].iter().collect();
                            lines.push(head);
                            chunk_w = string_width(&carry);
                            chunk = carry;
                        }
                        None => {
                            lines.push(std::mem::take(&mut chunk));
                            chunk_w = 0;
                        }
                    }
                }
                chunk.push_str(ch);
                chunk_w += cw;
            }
            cur = chunk;
            cur_w = chunk_w;
        } else if cur.is_empty() {
            cur = word.to_string();
            cur_w = ww;
        } else if cur_w + 1 + ww <= width {
            cur.push(' ');
            cur.push_str(word);
            cur_w += 1 + ww;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
            cur_w = ww;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let target = width.saturating_sub(1).max(1);
        let last = lines.last().cloned().unwrap_or_default();
        let mut kept = String::new();
        let mut used = 0usize;
        for (ch, cw) in measured(&last) {
            if used + cw > target {
                break;
            }
            kept.push_str(ch);
            used += cw;
        }
        if let Some(slot) = lines.last_mut() {
            *slot = format!("{kept}…");
        }
    }
    lines
}

/// `fitLabel(label, inner)`：超宽时截到 `inner` 列（含 `…`）。
///
/// 循环条件保留一列给 `…`，因此 `inner >= 1` 时结果宽度不超过 `inner`。
fn fit_label(label: &str, inner: usize) -> String {
    if string_width(label) <= inner {
        return label.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for (ch, cw) in measured(label) {
        if used + cw + 1 > inner {
            break;
        }
        out.push_str(ch);
        used += cw;
    }
    format!("{out}…")
}

// ============================================================================
// 图模型
// ============================================================================

/// 节点形状。Mermaid 的 `[[...]]`、`[...]`、`>...]` 都归矩形，`(...)`、`((...))`、
/// `([...])`、`[(...)]` 归圆角，`{...}`、`{{...}}` 归菱形。
/// 绘制时 `Round` 与 `Diamond` 都用圆角框（见 `draw_box`），菱形没有真正的斜边。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Rect,
    Round,
    Diamond,
}

/// 连线端点的箭头样式；`None` 表示该端没有箭头（只画一条接入边界的线）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Head {
    None,
    Arrow,
    Circle,
    Cross,
}

/// 连线线型；绘制时决定 `Canvas.cur_style`，最终字形在 `finalize_mask` 里替换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Solid,
    Dotted,
    Thick,
}

/// 图方向。`parse_dir` 只认 `LR`/`RL`/`BT`，其余（`TD`/`TB`/未知）都是 `Down`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Down,
    Up,
    Right,
    Left,
}

/// 解析出的节点：标签已清洗；`shape` 只影响框角。
#[derive(Debug, Clone)]
struct Node {
    label: String,
    shape: Shape,
}

/// 解析出的边：`from`/`to` 是节点下标；`head_from` 是源端的箭头（`<--` 一类写法）。
#[derive(Debug, Clone)]
struct Edge {
    from: usize,
    to: usize,
    label: Option<String>,
    head_to: Head,
    head_from: Head,
    line: LineKind,
}

// 图规模上限。超过时置 `over_cap` 并整图放弃渲染；`render_mermaid` 随后会去掉最后一行
// 重试，从而画出能画的前缀（集成测试
// `retries_without_the_final_line_when_the_flowchart_exceeds_the_node_cap` 锁定该行为）。
const MAX_NODES: usize = 128;
const MAX_EDGES: usize = 512;

/// 解析结果。`index` 是 id → `nodes` 下标，用来把重复出现的节点合并成同一个框；
/// `over_cap` 一旦置位就表示这张图超出规模上限、不能渲染；`warnings` 记录被丢弃的语句
/// （最终渲染据此决定是否退回代码块）。
#[derive(Default)]
struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    index: HashMap<String, usize>,
    over_cap: bool,
    warnings: Vec<String>,
    dir: Option<Dir>,
}

impl Graph {
    fn new(dir: Dir) -> Self {
        Graph {
            dir: Some(dir),
            ..Graph::default()
        }
    }

    /// 未显式给出方向时按 `Down`（TD）处理；`Graph::default()` 里 `dir` 是 None。
    fn dir(&self) -> Dir {
        self.dir.unwrap_or(Dir::Down)
    }

    /// 按 id 取节点下标；不存在则新建并登记到 `index`。
    ///
    /// 同一个 id 再次出现时：带标签的引用会覆盖已有节点的标签与形状（最后一次定义生效），
    /// 不带标签的引用保持原样。节点数达到 `MAX_NODES` 时置 `over_cap` 并返回 None。
    fn node_index(&mut self, id: &str, label: Option<&str>, shape: Shape) -> Option<usize> {
        if let Some(existing) = self.index.get(id) {
            if let Some(label) = label {
                self.nodes[*existing].label = label.to_string();
                self.nodes[*existing].shape = shape;
            }
            return Some(*existing);
        }
        if self.nodes.len() >= MAX_NODES {
            self.over_cap = true;
            return None;
        }
        self.index.insert(id.to_string(), self.nodes.len());
        self.nodes.push(Node {
            label: label.unwrap_or(id).to_string(),
            shape,
        });
        Some(self.nodes.len() - 1)
    }

    /// 追加一条边；边数达到 `MAX_EDGES` 时置 `over_cap` 并返回 false，调用方立即停止解析。
    fn push_edge(&mut self, edge: Edge) -> bool {
        if self.edges.len() >= MAX_EDGES {
            self.over_cap = true;
            return false;
        }
        self.edges.push(edge);
        true
    }
}

// 方向关键字大小写不敏感；`LR` 向右、`RL` 向左、`BT` 向上，其它（含 `TD`/`TB`）都向下。
fn parse_dir(token: &str) -> Dir {
    match ascii_upper(token).as_str() {
        "LR" => Dir::Right,
        "RL" => Dir::Left,
        "BT" => Dir::Up,
        _ => Dir::Down,
    }
}

// --- 语句切分 ---------------------------------------------------------------

// 把累积的语句去首尾空白后放入 out；空语句（连续 `;`、行尾 `;`）直接丢弃。
fn flush_statement(current: &mut String, out: &mut Vec<String>) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
    current.clear();
}

// 按 `;` 切一条逻辑行；双引号内的 `;` 与 `%` 不参与切分（引号内容整体保留）。
// 不在引号内遇到 `%%` 就停止：Mermaid 里 `%%` 到行尾是注释。
fn split_statements(line: &str, out: &mut Vec<String>) {
    let chars: Vec<char> = line.chars().collect();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if in_quotes {
            if ch == '"' {
                in_quotes = false;
            }
            current.push(ch);
        } else if ch == '"' {
            in_quotes = true;
            current.push(ch);
        } else if ch == '%' && chars.get(i + 1) == Some(&'%') {
            break;
        } else if ch == ';' {
            flush_statement(&mut current, out);
        } else {
            current.push(ch);
        }
        i += 1;
    }
    flush_statement(&mut current, out);
}

// 逐行切语句；行边界由 `src_lines` 决定（与 Rust `str::lines` 相同），每条语句再去空白。
fn statements_of(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in src_lines(src) {
        split_statements(&line, &mut out);
    }
    out
}

fn first_word(statement: &str) -> String {
    statement
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

fn words(statement: &str) -> Vec<&str> {
    statement.split_whitespace().collect()
}

// 首条语句的首词（小写）；没有语句或首词为空时返回 None。图类型由它判定，方向则由
// `parse_graph` 另外读首条语句的第二个词。
fn header_kind(statements: &[String]) -> Option<String> {
    let header = statements.first()?;
    let kind = first_word(header);
    if kind.is_empty() {
        None
    } else {
        Some(ascii_lower(&kind))
    }
}

/// 只识别 flowchart / graph；其它类型返回 `None`（调用方保留原始块）。
///
/// 返回两层 `Option`：
/// - 外层 `None`：首条语句不是 `graph`/`flowchart`，不是本模块能画的图；
/// - 内层 `None`：首条语句是 flowchart，但含 `subgraph`（布局未移植）、节点/边超过上限，
///   或最终没有任何节点——同样保留原始块。
///
/// 首条语句的第二个词是方向（`LR`/`RL`/`BT`，缺省 `TD`）。`classdef`/`class`/`style`/
/// `linkstyle`/`click`/`direction`/`end` 不产生图形，直接跳过；其它每条语句交给
/// `parse_statement`，任何一条超出上限就立刻放弃整张图。
fn parse_graph(src: &str) -> Option<Option<Graph>> {
    let statements = statements_of(src);
    let kind = header_kind(&statements)?;
    if kind != "graph" && kind != "flowchart" {
        return None;
    }
    let dir_token = words(&statements[0])
        .get(1)
        .map(|token| parse_dir(token))
        .unwrap_or(Dir::Down);
    let mut graph = Graph::new(dir_token);

    for statement in statements.iter().skip(1) {
        match ascii_lower(&first_word(statement)).as_str() {
            "subgraph" => return Some(None), // subgraph 布局未移植：保留原始代码块
            "end" | "classdef" | "class" | "style" | "linkstyle" | "click" | "direction" => {
                continue;
            }
            _ => {}
        }
        parse_statement(statement, &mut graph);
        if graph.over_cap {
            return Some(None);
        }
    }

    if graph.nodes.is_empty() {
        Some(None)
    } else {
        Some(Some(graph))
    }
}

/// `read_shape_at`/`read_shape` 的中间结果：形状、清洗后的标签、标签结束后的下标，以及
/// 标签未闭合时缺失的结束符（由 `parse_node` 转成警告）。
struct Shaped {
    shape: Shape,
    label: Option<String>,
    after: usize,
    unclosed: Option<String>,
}

// 只跳过空格与制表符；换行在 `statements_of` 之前就切掉了，不会出现在这里。
fn skip_spaces(chars: &[char], mut i: usize) -> usize {
    while chars.get(i).is_some_and(|ch| *ch == ' ' || *ch == '\t') {
        i += 1;
    }
    i
}

// 解析 `id` 加可选的形状/标签，必要时在图中建立节点，返回 (节点下标, 下一个待解析位置)。
// id 由 `is_id_char` 组成；标签未闭合时记一条 warning 但继续（标签取到语句末尾）。
// 返回 None 只有两种原因：这里没有 id，或节点数已超过上限。
fn parse_node(chars: &[char], start: usize, graph: &mut Graph) -> Option<(usize, usize)> {
    let mut i = skip_spaces(chars, start);
    let id_start = i;
    while chars.get(i).is_some_and(|ch| is_id_char(*ch)) {
        i += 1;
    }
    if i == id_start {
        return None;
    }
    let id: String = chars[id_start..i].iter().collect();
    let shaped = read_shape_at(chars, i);
    if let Some(unclosed) = &shaped.unclosed {
        graph.warnings.push(format!(
            "node \"{id}\": label is missing its closing `{unclosed}`"
        ));
    }
    let index = graph.node_index(&id, shaped.label.as_deref(), shaped.shape)?;

    // `id:::name`：吞掉样式类，保持语句可继续解析。类名允许 '-'，但末尾连续的 '-' 不计入
    // （回退到最后一个非 '-' 位置）。
    let mut next = shaped.after;
    if chars.get(next) == Some(&':')
        && chars.get(next + 1) == Some(&':')
        && chars.get(next + 2) == Some(&':')
    {
        let mut k = next + 3;
        while chars.get(k).is_some_and(|ch| is_id_char(*ch) || *ch == '-') {
            k += 1;
        }
        while k > next + 3 && chars.get(k - 1) == Some(&'-') {
            k -= 1;
        }
        if k > next + 3 {
            next = k;
        }
    }
    Some((index, next))
}

// 识别形状前缀并返回 `Shaped`。两字符前缀优先：`[[ ]]`、`[( )]`、`(( ))`、`([ ])`、
// `{{ }}`；单字符 `[ ]`、`( )`、`{ }` 与 `> ]` 也能识别。
// 只归并成三种 Shape：`[[..]]` 与 `>..]` 是矩形，`((..))`/`([..])`/`(..)`/`[(..)]` 是
// 圆角，`{{..}}`/`{..}` 是菱形。不以形状字符开头时 label=None、after=i（标签缺省用 id）。
fn read_shape_at(chars: &[char], i: usize) -> Shaped {
    let c = chars.get(i).copied();
    let n = chars.get(i + 1).copied();
    match c {
        Some('[') => match n {
            Some('[') => read_shape(chars, i + 2, "]]", Shape::Rect),
            Some('(') => read_shape(chars, i + 2, ")]", Shape::Round),
            _ => read_shape(chars, i + 1, "]", Shape::Rect),
        },
        Some('(') => match n {
            Some('(') => read_shape(chars, i + 2, "))", Shape::Round),
            Some('[') => read_shape(chars, i + 2, "])", Shape::Round),
            _ => read_shape(chars, i + 1, ")", Shape::Round),
        },
        Some('{') => match n {
            Some('{') => read_shape(chars, i + 2, "}}", Shape::Diamond),
            _ => read_shape(chars, i + 1, "}", Shape::Diamond),
        },
        Some('>') => read_shape(chars, i + 1, "]", Shape::Rect),
        _ => Shaped {
            shape: Shape::Rect,
            label: None,
            after: i,
            unclosed: None,
        },
    }
}

// 从 `start` 扫到 `closer`，得到标签原文（尚未 `clean_label`）。
// 跳过空白后首字符是 '"' 时按引号规则找结束符：引号内的 closer 不算数。
// 扫到末尾仍未遇到 closer：label 取剩余全文、after 指向末尾、unclosed 记下缺的结束符，
// 由 `parse_node` 生成警告（此时语句的剩余部分已被这个标签吃掉）。
fn read_shape(chars: &[char], start: usize, closer: &str, shape: Shape) -> Shaped {
    let mut j = start;
    while chars.get(j).is_some_and(|ch| *ch == ' ' || *ch == '\t') {
        j += 1;
    }
    let quoted = chars.get(j) == Some(&'"');

    let closer_chars: Vec<char> = closer.chars().collect();
    let mut i = start;
    let mut text = String::new();
    let mut in_quotes = false;
    while i < chars.len() {
        let c = chars[i];
        if quoted && c == '"' {
            in_quotes = !in_quotes;
            text.push(c);
            i += 1;
            continue;
        }
        if !in_quotes
            && i + closer_chars.len() <= chars.len()
            && chars[i..i + closer_chars.len()] == closer_chars[..]
        {
            return Shaped {
                shape,
                label: Some(clean_label(&text)),
                after: i + closer_chars.len(),
                unclosed: None,
            };
        }
        text.push(c);
        i += 1;
    }
    Shaped {
        shape,
        label: Some(clean_label(&text)),
        after: chars.len(),
        unclosed: Some(closer.to_string()),
    }
}

// 组成连线操作符的字符集；`<`/`>` 同时表示箭头方向。
fn is_link_char(ch: char) -> bool {
    matches!(ch, '-' | '.' | '=' | '<' | '>')
}

/// `parse_link` 的结果：两端箭头、线型、可选标签，以及操作符/标签之后的下标。
struct Link {
    left: Head,
    right: Head,
    line: LineKind,
    label: Option<String>,
    next: usize,
}

// 线型由操作符里是否含 '=' 或 '.' 决定：'=' 优先（粗线），其次 '.'（点线），否则实线。
fn line_kind(op: &str) -> LineKind {
    if op.contains('=') {
        LineKind::Thick
    } else if op.contains('.') {
        LineKind::Dotted
    } else {
        LineKind::Solid
    }
}

// 操作符后紧跟的 `o`/`x` 端点（`--o`、`--x`）。只有后面是空白、`|`、`&`、`;` 或输入结束
// 时才承认，避免把节点 id 开头的 o/x 吞掉。
fn trailing_head(chars: &[char], i: usize) -> Option<(Head, usize)> {
    let head = match chars.get(i) {
        Some('o') => Head::Circle,
        Some('x') => Head::Cross,
        _ => return None,
    };
    let boundary = match chars.get(i + 1) {
        None => true,
        Some(ch) => matches!(*ch, ' ' | '\t' | '|' | '&' | ';'),
    };
    boundary.then_some((head, i + 1))
}

// 解析一段连线操作符（含可选标签），返回 `Link`；`next` 指向操作符/标签之后。
//
// 支持的形式（与下面的分支一一对应）：
// - 可选前置端点 `o--`/`x--`（圆点/叉），此时左端箭头由这个字母给出；
// - 操作符是 `-`/`.`/`=`/`<`/`>` 的连续串：以 `<` 开头 → 左箭头，含 `>` → 右箭头；
//   含 `=` → 粗线，含 `.` → 点线，否则实线；
// - 右端没有箭头时再试尾部端点 `o`/`x`；
// - `|label|` 形式：读到下一个 `|` 为止；
// - `-- label -->` 形式：两个操作符之间的文本作为标签；文本后紧接 `<` 时该分支不成立
//   （回退成无标签连线），第二个操作符的线型只在第一个操作符是实线时生效。
//
// 两种标签都经过 `clean_label`，空标签归为 None。
fn parse_link(chars: &[char], start: usize) -> Option<Link> {
    let mut i = skip_spaces(chars, start);
    let mut left = Head::None;
    if let (Some('o' | 'x'), Some(next)) = (chars.get(i), chars.get(i + 1))
        && matches!(*next, '-' | '.' | '=')
    {
        left = if chars[i] == 'o' {
            Head::Circle
        } else {
            Head::Cross
        };
        i += 1;
    }

    let op_start = i;
    while chars.get(i).is_some_and(|ch| is_link_char(*ch)) {
        i += 1;
    }
    if i == op_start {
        return None;
    }
    let op1: String = chars[op_start..i].iter().collect();
    if left == Head::None && op1.starts_with('<') {
        left = Head::Arrow;
    }
    let mut line = line_kind(&op1);
    let mut right = if op1.contains('>') {
        Head::Arrow
    } else {
        Head::None
    };
    if right == Head::None
        && let Some((head, next)) = trailing_head(chars, i)
    {
        right = head;
        i = next;
    }

    if chars.get(i) == Some(&'|') {
        i += 1;
        let label_start = i;
        while chars.get(i).is_some_and(|ch| *ch != '|') {
            i += 1;
        }
        let label = clean_label(&chars[label_start..i].iter().collect::<String>());
        if chars.get(i) == Some(&'|') {
            i += 1;
        }
        let label = if label.is_empty() { None } else { Some(label) };
        return Some(Link {
            left,
            right,
            line,
            label,
            next: i,
        });
    }

    if right == Head::None {
        let text_start = skip_spaces(chars, i);
        let mut j = text_start;
        while chars.get(j).is_some_and(|ch| !is_link_char(*ch)) {
            j += 1;
        }
        if j < chars.len() && j > text_start && chars.get(j) != Some(&'<') {
            let text: String = chars[text_start..j].iter().collect();
            let op2_start = j;
            while chars.get(j).is_some_and(|ch| is_link_char(*ch)) {
                j += 1;
            }
            let op2: String = chars[op2_start..j].iter().collect();
            if op2.contains('>') {
                right = Head::Arrow;
            } else if let Some((head, next)) = trailing_head(chars, j) {
                right = head;
                j = next;
            }
            if line == LineKind::Solid {
                line = line_kind(&op2);
            }
            let label = clean_label(&text);
            return Some(Link {
                left,
                right,
                line,
                label: if label.is_empty() { None } else { Some(label) },
                next: j,
            });
        }
    }

    Some(Link {
        left,
        right,
        line,
        label: None,
        next: i,
    })
}

// 解析一个节点组：`A & B & C` 里用 `&` 连接的节点共享同一条边，返回
// (节点下标列表, 组之后的下标)。任一节点解析失败则整个组失败。
fn parse_node_group(
    chars: &[char],
    start: usize,
    graph: &mut Graph,
) -> Option<(Vec<usize>, usize)> {
    let (first, mut i) = parse_node(chars, start, graph)?;
    let mut group = vec![first];
    loop {
        let j = skip_spaces(chars, i);
        if chars.get(j) != Some(&'&') {
            break;
        }
        let (next, after) = parse_node(chars, j + 1, graph)?;
        group.push(next);
        i = after;
    }
    Some((group, i))
}

// 解析一条语句（可含链式连线 `A --> B --> C`）。
//
// 流程：先解析起点的节点组；然后循环「连线 → 目标节点组」，对当前组与目标组做笛卡尔积
// 连边，再把目标组变成下一轮的起点。语句不以节点开头、或连线上没有目标时记 warning 并
// 结束本条语句（不影响其它语句）；边数超限时直接返回，由调用方检查 `over_cap` 后放弃
// 整张图。
//
// `<--` 这类反向写法在这里归一：左端是箭头而右端不是时交换 from/to，并把箭头放到
// `head_to`，让边的绘制方向与箭头指向一致（分层从 from 排到 to）。
fn parse_statement(statement: &str, graph: &mut Graph) {
    let chars: Vec<char> = statement.chars().collect();
    let mut i = 0usize;

    let Some((mut prev, next)) = parse_node_group(&chars, i, graph) else {
        graph.warnings.push(format!(
            "dropped, does not start with a node: \"{statement}\""
        ));
        return;
    };
    i = next;

    loop {
        i = skip_spaces(&chars, i);
        if i >= chars.len() {
            break;
        }
        let Some(link) = parse_link(&chars, i) else {
            graph.warnings.push(format!(
                "dropped, expected a link: \"{}\"",
                chars[i..].iter().collect::<String>()
            ));
            break;
        };
        i = skip_spaces(&chars, link.next);
        let Some((target, after)) = parse_node_group(&chars, i, graph) else {
            graph
                .warnings
                .push(format!("dropped, link has no target: \"{statement}\""));
            break;
        };
        i = after;
        for from in &prev {
            for to in &target {
                let reversed = link.left == Head::Arrow && link.right != Head::Arrow;
                let pushed = graph.push_edge(Edge {
                    from: if reversed { *to } else { *from },
                    to: if reversed { *from } else { *to },
                    label: link.label.clone(),
                    head_to: if reversed { Head::Arrow } else { link.right },
                    head_from: if reversed { link.right } else { link.left },
                    line: link.line,
                });
                if !pushed {
                    return;
                }
            }
        }
        prev = target;
    }
}

// ============================================================================
// Canvas
// ============================================================================

/// 宽字形尾部占位（TS `CONT = String.fromCharCode(0)`）：构建行时跳过。
const CONT: &str = "\0";
// 方向位：某格在上/下/左/右方向上有连线。多段线经过同一格时按位 OR 叠加，
// `finalize_mask` 再把组合翻译成盒线字符。
const U: u8 = 1;
const D: u8 = 2;
const L: u8 = 4;
const R: u8 = 8;
// 线型位：绘制时写入 `style`，`finalize_mask` 只在 style 恰好等于单一类型时换成
// 点线/粗线字形；同一格被不同线型经过时（位已叠加）保持实线字形。
const STY_DOT: u8 = 1;
const STY_THICK: u8 = 2;
const STY_SOLID: u8 = 4;

/// 字符画布：`w × h` 的扁平网格，下标是 `y * w + x`。
///
/// 几个平行数组按同一组下标对齐：`ch`（字素簇或 `CONT`）、`cls`（着色语义）、
/// `mask`（方向位）、`style`（线型位）、`occupied`（节点框占位，连线不得覆盖文字）。
/// 所有绘制入口都做越界检查，边界外的写入被静默丢弃。
struct Canvas {
    w: i64,
    h: i64,
    /// 每个单元是一个字素簇（TS `ch: string[]`），`CONT` 表示宽字形的第二格。
    ch: Vec<String>,
    cls: Vec<SpanClass>,
    mask: Vec<u8>,
    style: Vec<u8>,
    occupied: Vec<bool>,
    /// 当前绘制使用的线型位；`layout_canvas` 在每条边开始前按 `Edge.line` 设置，
    /// `add_bits*` 每次经过格子时把它 OR 进 `style`。箭头与标签用 `set`/`set_str`
    /// 直接写字符，不受它影响。
    cur_style: u8,
}

impl Canvas {
    fn new(w: i64, h: i64) -> Self {
        let n = (w.max(1) * h.max(1)) as usize;
        Canvas {
            w,
            h,
            ch: vec![" ".to_string(); n],
            cls: vec![SpanClass::None; n],
            mask: vec![0; n],
            style: vec![0; n],
            occupied: vec![false; n],
            cur_style: STY_SOLID,
        }
    }

    fn idx(&self, x: i64, y: i64) -> usize {
        (y * self.w + x) as usize
    }

    fn set(&mut self, x: i64, y: i64, c: char, cls: SpanClass) {
        let mut buffer = [0u8; 4];
        self.set_str(x, y, c.encode_utf8(&mut buffer), cls);
    }

    // 越界写入被忽略：宽字形落在右边界时，它的 `CONT` 可能已经在画布之外。
    fn set_str(&mut self, x: i64, y: i64, c: &str, cls: SpanClass) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let i = self.idx(x, y);
        self.ch[i] = c.to_string();
        self.cls[i] = cls;
    }

    // 连线写方向位（不直接写字符），最终字符交给 `finalize_mask` 合成；
    // `occupied` 的格子直接跳过——节点框内部与边框由 `draw_box` 负责，连线不能覆盖。
    // 边框要“开口”接出连线时走 `junction`（它不做 occupied 检查）。
    fn add_bits(&mut self, x: i64, y: i64, bits: u8) {
        self.add_bits_cls(x, y, bits, SpanClass::Edge);
    }

    // 只有本来不是 `Border` 的格子才更新 `cls`，保证边框颜色不会被连线类别覆盖；
    // 线型位同样在每次经过时 OR 进来。
    fn add_bits_cls(&mut self, x: i64, y: i64, bits: u8, cls: SpanClass) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let i = self.idx(x, y);
        if self.occupied[i] {
            return;
        }
        self.mask[i] |= bits;
        self.style[i] |= self.cur_style;
        if self.cls[i] != SpanClass::Border {
            self.cls[i] = cls;
        }
    }

    // 在节点框边界上打一个 T/十字连接点。与 `add_bits_cls` 不同，这里**不做** `occupied`
    // 检查，因此可以把框的边线变成 `┬`/`┼` 这类带连线的形状。
    fn junction(&mut self, x: i64, y: i64, bits: u8) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let i = self.idx(x, y);
        self.mask[i] |= bits;
        if self.cls[i] != SpanClass::Border {
            self.cls[i] = SpanClass::Edge;
        }
    }

    // 竖直线段：中间格子同时带上 U/D 两个方向位，两个端点只带朝内的那一位。
    fn seg_v(&mut self, x: i64, y0: i64, y1: i64) {
        let a = y0.min(y1);
        let b = y0.max(y1);
        let mut y = a;
        while y <= b {
            let mut bits = 0;
            if y > a {
                bits |= U;
            }
            if y < b {
                bits |= D;
            }
            self.add_bits(x, y, bits);
            y += 1;
        }
    }

    // 水平线段：与 `seg_v` 对称，方向位换成 L/R。
    fn seg_h(&mut self, y: i64, x0: i64, x1: i64) {
        let a = x0.min(x1);
        let b = x0.max(x1);
        let mut x = a;
        while x <= b {
            let mut bits = 0;
            if x > a {
                bits |= L;
            }
            if x < b {
                bits |= R;
            }
            self.add_bits(x, y, bits);
            x += 1;
        }
    }

    // 把方向位翻译成盒线字符：只填仍是空格的格子（文字与框角优先），
    // style 恰好是 STY_DOT / STY_THICK 时再换成点线/粗线字形。
    fn finalize_mask(&mut self) {
        for i in 0..self.ch.len() {
            if self.mask[i] != 0 && self.ch[i] == " " {
                let c = mask_char(self.mask[i]);
                let c = if self.style[i] == STY_DOT {
                    dotted_char(c)
                } else if self.style[i] == STY_THICK {
                    thick_char(c)
                } else {
                    c
                };
                self.ch[i] = c.to_string();
            }
        }
    }

    // 上下翻转画布（`BT` 方向）：先交换行，再用 `FLIP_V` 把每个字形换成镜像。
    // 它在 `finalize_mask` 之后调用，`mask`/`style`/`occupied` 已不再被读取，所以只交换
    // `ch` 与 `cls`。
    fn flip_vertical(&mut self) {
        let mut y = 0;
        while y < self.h / 2 {
            let y2 = self.h - 1 - y;
            let mut x = 0;
            while x < self.w {
                let i = self.idx(x, y);
                let j = self.idx(x, y2);
                self.ch.swap(i, j);
                self.cls.swap(i, j);
                x += 1;
            }
            y += 1;
        }
        for ch in &mut self.ch {
            *ch = flip_glyph(ch, FLIP_V);
        }
    }

    // 左右翻转画布（`RL` 方向）：交换列 → 字形镜像 → 把 `Text`/`EdgeLabel` 的连续段再
    // 反转一次，让文字保持正常方向（只有框线与箭头被镜像）。
    // 段内反转按“格”进行，不重新按字素簇分组。
    fn flip_horizontal(&mut self) {
        let mut y = 0;
        while y < self.h {
            let mut x = 0;
            while x < self.w / 2 {
                let x2 = self.w - 1 - x;
                let i = self.idx(x, y);
                let j = self.idx(x2, y);
                self.ch.swap(i, j);
                self.cls.swap(i, j);
                x += 1;
            }
            y += 1;
        }
        for ch in &mut self.ch {
            *ch = flip_glyph(ch, FLIP_H);
        }
        let mut y = 0;
        while y < self.h {
            let mut x = 0;
            while x < self.w {
                let cls = self.cls[self.idx(x, y)];
                if cls == SpanClass::Text || cls == SpanClass::EdgeLabel {
                    let start = self.idx(x, y);
                    while x < self.w && self.cls[self.idx(x, y)] == cls {
                        x += 1;
                    }
                    let end = self.idx(x, y);
                    self.ch[start..end].reverse();
                } else {
                    x += 1;
                }
            }
            y += 1;
        }
    }

    // 把网格裁剪成输出行，同时产出 plain 与 styled（两者行一一对应）：
    // - 每行取最右侧非空格 + 1 作为该行宽度，全局 `width` 取所有行的最大值；
    // - `plain` 去掉行尾空格；`styled` 在相同的列区间内按 `cls` 把相邻同类格子合并成
    //   `Span`；`CONT` 占位不计入任何输出；
    // - 最后去掉上下连续的空白行（中间的空行保留），plain/styled 用同一区间切片。
    fn to_lines(&self) -> (Vec<String>, Vec<Vec<Span>>, usize) {
        let mut plain: Vec<String> = Vec::new();
        let mut styled: Vec<Vec<Span>> = Vec::new();
        let mut width = 0usize;
        let mut y = 0;
        while y < self.h {
            let mut last = 0i64;
            let mut x = self.w - 1;
            while x >= 0 {
                if self.ch[self.idx(x, y)] != " " {
                    last = x + 1;
                    break;
                }
                x -= 1;
            }
            width = width.max(last as usize);
            let mut spans: Vec<Span> = Vec::new();
            let mut plain_row = String::new();
            let mut run = String::new();
            let mut run_cls = SpanClass::None;
            let mut x = 0;
            while x < last {
                let i = self.idx(x, y);
                let c = self.ch[i].as_str();
                if c != CONT {
                    let cls = self.cls[i];
                    plain_row.push_str(c);
                    if cls != run_cls && !run.is_empty() {
                        spans.push(Span {
                            text: std::mem::take(&mut run),
                            cls: run_cls,
                        });
                    }
                    run_cls = cls;
                    run.push_str(c);
                }
                x += 1;
            }
            if !run.is_empty() {
                spans.push(Span {
                    text: run,
                    cls: run_cls,
                });
            }
            styled.push(spans);
            plain.push(plain_row.trim_end_matches(' ').to_string());
            y += 1;
        }
        let mut first = 0usize;
        while first < plain.len() && plain[first].is_empty() {
            first += 1;
        }
        let mut end = plain.len();
        while end > first && plain[end - 1].is_empty() {
            end -= 1;
        }
        (
            plain[first..end].to_vec(),
            styled[first..end].to_vec(),
            width,
        )
    }
}

// 逐字素簇画文本：宽度为 0 的簇跳过；占 2 列及以上的簇在自己的格子之后补写 `CONT`，
// 避免后续绘制把宽字形的后半格当成空白。
fn draw_text(canvas: &mut Canvas, text: &str, x: i64, y: i64, cls: SpanClass) {
    let mut cur = x;
    for (cluster, cw) in measured(text) {
        if cw == 0 {
            continue;
        }
        canvas.set_str(cur, y, cluster, cls);
        let mut k = 1;
        while k < cw as i64 {
            canvas.set_str(cur + k, y, CONT, cls);
            k += 1;
        }
        cur += cw as i64;
    }
}

// 方向位组合 → 盒线字符：只有上/下（或只有左/右）时分别是 '│'/'─'，未列出的组合落到
// 四通的 '┼'。
fn mask_char(mask: u8) -> char {
    match mask {
        0 => ' ',
        m if m == U || m == D || m == U | D => '│',
        m if m == L || m == R || m == L | R => '─',
        m if m == D | R => '┌',
        m if m == D | L => '┐',
        m if m == U | R => '└',
        m if m == U | L => '┘',
        m if m == U | D | R => '├',
        m if m == U | D | L => '┤',
        m if m == D | L | R => '┬',
        m if m == U | L | R => '┴',
        _ => '┼',
    }
}

// 实线字形到点线字形的替换表；表里没有的字形原样返回（目前只有横/竖两种）。
fn dotted_char(c: char) -> char {
    match c {
        '─' => '╌',
        '│' => '╎',
        other => other,
    }
}

// 实线字形到粗线字形的替换表；覆盖全部盒线线段/拐角/交叉，其它字符原样返回。
fn thick_char(c: char) -> char {
    match c {
        '─' => '━',
        '│' => '┃',
        '┌' => '┏',
        '┐' => '┓',
        '└' => '┗',
        '┘' => '┛',
        '├' => '┣',
        '┤' => '┫',
        '┬' => '┳',
        '┴' => '┻',
        '┼' => '╋',
        other => other,
    }
}

/// TS `FLIP_V`：单字符字形查表；多字符簇（组合字符等）原样保留。
const FLIP_V: &[(char, char)] = &[
    ('┌', '└'),
    ('└', '┌'),
    ('┐', '┘'),
    ('┘', '┐'),
    ('┏', '┗'),
    ('┗', '┏'),
    ('┓', '┛'),
    ('┛', '┓'),
    ('╭', '╰'),
    ('╰', '╭'),
    ('╮', '╯'),
    ('╯', '╮'),
    ('┬', '┴'),
    ('┴', '┬'),
    ('┳', '┻'),
    ('┻', '┳'),
    ('▼', '▲'),
    ('▲', '▼'),
    ('▽', '△'),
    ('△', '▽'),
];

/// TS `FLIP_H`。
const FLIP_H: &[(char, char)] = &[
    ('┌', '┐'),
    ('┐', '┌'),
    ('└', '┘'),
    ('┘', '└'),
    ('┏', '┓'),
    ('┓', '┏'),
    ('┗', '┛'),
    ('┛', '┗'),
    ('╭', '╮'),
    ('╮', '╭'),
    ('╰', '╯'),
    ('╯', '╰'),
    ('├', '┤'),
    ('┤', '├'),
    ('┣', '┫'),
    ('┫', '┣'),
    ('▶', '◄'),
    ('◄', '▶'),
    ('▷', '◁'),
    ('◁', '▷'),
];

// 查表替换单字符字形；不是恰好一个 `char` 的簇（组合序列、宽字形等）原样返回。
fn flip_glyph(c: &str, table: &[(char, char)]) -> String {
    let mut chars = c.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => table
            .iter()
            .find(|(from, _)| *from == ch)
            .map_or_else(|| c.to_string(), |(_, to)| to.to_string()),
        _ => c.to_string(),
    }
}

// ============================================================================
// 布局
// ============================================================================

// 布局间距：`PAD` 是框内文字左右留白；`GAP_X`/`GAP_Y` 是相邻节点框之间与层与层之间的
// 最小间距（层间还要容纳连线总线，实际间距会更大，见 `place_td`）。
// `MAX_CANVAS_CELLS` 是画布总格数上限；超过时 `layout_canvas` 返回 None，整图不渲染，
// 避免按输入规模分配超大画布。
const PAD: i64 = 1;
const GAP_X: i64 = 3;
const GAP_Y: i64 = 2;
const MAX_CANVAS_CELLS: i64 = 1 << 21;

// `a - b` 的下界 0：坐标计算里用来保证框不越过左/上边界。
fn sat(a: i64, b: i64) -> i64 {
    (a - b).max(0)
}

// 整数除 2（向零取整）。调用点都传非负数。
fn half(n: i64) -> i64 {
    n / 2
}

/// 布局后的节点矩形：`x`/`y` 是左上角，`w`/`h` 是外框尺寸，`cx`/`cy` 是中心，`rank` 是
/// 层级下标。路由只依赖 `cx`/`cy` 与四条边界的坐标。
#[derive(Debug, Clone, Copy, Default)]
struct Placed {
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    cx: i64,
    cy: i64,
    rank: usize,
}

/// 每个节点的尺寸预计算：`box_w`/`box_h` 是最终外框；`lay_w`/`lay_h` 是参与布局的尺寸
/// （`lay_w` 含自环标签预留的宽度，`lay_h` 含自环的额外高度）；`extra_h` 与
/// `self_label_w` 记录自环占用的额外空间。
struct NodeSizes {
    box_w: Vec<i64>,
    box_h: Vec<i64>,
    lay_w: Vec<i64>,
    lay_h: Vec<i64>,
    extra_h: Vec<i64>,
    self_label_w: Vec<i64>,
}

/// 布局产生的路由信息：画布尺寸、每层结束坐标（总线起点）、每条边的总线槽位、回边车道
/// 起点与槽位。`edge_bus`/`edge_lane` 的下标与 `graph.edges` 一一对应。
///
/// 绘制时第 `i` 条边的总线行/车道列由两处相加得到：相邻层连线用
/// `band_end[from.rank] + edge_bus[i]`，跨层连线用 `lane_base + edge_lane[i]`
/// （调用点在 `layout_canvas`）。
struct RoutePlan {
    canvas_w: i64,
    canvas_h: i64,
    band_end: Vec<i64>,
    edge_bus: Vec<usize>,
    lane_base: i64,
    edge_lane: Vec<usize>,
}

/// 区间占用：`(起点, 终点, from 节点, to 节点, 边下标)`；`assign_tracks` 用它做车道分配。
type Span5 = (i64, i64, i64, i64, usize);

// 计算每个节点的层级（rank）：从所有入度为 0 的节点开始 DFS，丢弃回边得到 DAG，
// 再按拓扑序（DFS 后序的逆序）做最长路径松弛，所以 rank 是“从某个根出发的最长路径
// 长度”，不是最短路径。先遍历入度 0 的节点、再遍历全部节点，保证有环或孤立的分量也能
// 分到层级。
//
// 自环（`from == to`）不计入入度与 `children`，因此不会把节点推到下一层；它们由
// `route_self` 单独绕开。
fn compute_ranks(graph: &Graph) -> Vec<usize> {
    let n = graph.nodes.len();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for edge in &graph.edges {
        if edge.from != edge.to {
            children[edge.from].push(edge.to);
            indeg[edge.to] += 1;
        }
    }
    let mut color = vec![0u8; n];
    let mut dag: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut order: Vec<usize> = Vec::new();

    let roots: Vec<usize> = (0..n).filter(|i| indeg[*i] == 0).collect();
    let starts: Vec<usize> = roots.into_iter().chain(0..n).collect();
    for start in starts {
        if color[start] == 0 {
            dfs_dag(start, &children, &mut color, &mut dag, &mut order);
        }
    }

    let mut rank = vec![0usize; n];
    for u in order.iter().rev() {
        for v in &dag[*u] {
            rank[*v] = rank[*v].max(rank[*u] + 1);
        }
    }
    rank
}

// 迭代版 DFS（显式栈，无递归）。color：0=未访问、1=在栈上、2=已完成。
// 指向“在栈上”节点的边是回边，丢弃；指向已完成节点的边保留（跨分量的横向边）。
fn dfs_dag(
    start: usize,
    children: &[Vec<usize>],
    color: &mut [u8],
    dag: &mut [Vec<usize>],
    order: &mut Vec<usize>,
) {
    let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
    color[start] = 1;
    while let Some((u, index)) = stack.last().copied() {
        if index < children[u].len() {
            let v = children[u][index];
            if let Some(frame) = stack.last_mut() {
                frame.1 += 1;
            }
            if color[v] == 1 {
                continue;
            }
            dag[u].push(v);
            if color[v] == 0 {
                color[v] = 1;
                stack.push((v, 0));
            }
        } else {
            color[u] = 2;
            order.push(u);
            stack.pop();
        }
    }
}

// 统计相邻两层之间（rank 差恰为 1）的边在给定层内顺序下的交叉数；自环与跨层边不计。
// 复杂度 O(E²)，只用于比较不同排序的优劣，边数受 `MAX_EDGES` 限制。
fn count_crossings(edges: &[Edge], ranks: &[usize], pos: &[usize]) -> usize {
    let adjacent: Vec<(usize, usize, usize)> = edges
        .iter()
        .filter(|edge| edge.from != edge.to && ranks[edge.to] == ranks[edge.from] + 1)
        .map(|edge| (ranks[edge.from], pos[edge.from], pos[edge.to]))
        .collect();
    let mut crossings = 0usize;
    for i in 0..adjacent.len() {
        for j in i + 1..adjacent.len() {
            let a = adjacent[i];
            let b = adjacent[j];
            if a.0 == b.0 && ((a.1 < b.1 && a.2 > b.2) || (a.1 > b.1 && a.2 < b.2)) {
                crossings += 1;
            }
        }
    }
    crossings
}

// 用重心（barycenter）启发式减少相邻层交叉：最多 8 轮，偶数轮自上而下按父节点平均位置
// 排序，奇数轮自下而上按子节点平均位置排序；每轮后统计交叉数，保留最好的一次（初始顺序
// 也算一个候选）。交叉数降到 0 就提前结束；没有邻居的节点用自己当前的位置作 key。结果
// 直接写回 `by_rank`。
fn order_ranks(by_rank: &mut [Vec<usize>], edges: &[Edge], ranks: &[usize]) {
    let n = ranks.len();
    if by_rank.len() < 2 || n < 3 {
        return;
    }
    let mut parents: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for edge in edges {
        if edge.from != edge.to && ranks[edge.to] > ranks[edge.from] {
            parents[edge.to].push(edge.from);
            children[edge.from].push(edge.to);
        }
    }
    let mut pos = vec![0usize; n];
    let reindex = |row: &[usize], pos: &mut [usize]| {
        for (i, node) in row.iter().enumerate() {
            pos[*node] = i;
        }
    };
    for row in by_rank.iter() {
        reindex(row, &mut pos);
    }

    let mut best: Vec<Vec<usize>> = by_rank.to_vec();
    let mut best_crossings = count_crossings(edges, ranks, &pos);
    if best_crossings == 0 {
        return;
    }

    for iteration in 0..8 {
        let rows: Vec<usize> = if iteration % 2 == 0 {
            (1..by_rank.len()).collect()
        } else {
            (0..by_rank.len().saturating_sub(1)).rev().collect()
        };
        for row_index in rows {
            let neighbors = if iteration % 2 == 0 {
                &parents
            } else {
                &children
            };
            let mut row = std::mem::take(&mut by_rank[row_index]);
            row.sort_by(|a, b| {
                let key = |v: usize| -> f64 {
                    if neighbors[v].is_empty() {
                        pos[v] as f64
                    } else {
                        neighbors[v].iter().map(|u| pos[*u] as f64).sum::<f64>()
                            / neighbors[v].len() as f64
                    }
                };
                key(*a)
                    .partial_cmp(&key(*b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            reindex(&row, &mut pos);
            by_rank[row_index] = row;
        }
        let crossings = count_crossings(edges, ranks, &pos);
        if crossings < best_crossings {
            best_crossings = crossings;
            best = by_rank.to_vec();
        }
        if best_crossings == 0 {
            break;
        }
    }
    for (i, row) in by_rank.iter_mut().enumerate() {
        *row = best[i].clone();
    }
}

// 计算同层节点的中心坐标（TD 是 x，LR 是 y），按节点下标返回。
// 1. 先按顺序把每层节点紧挨着排开（含 `sep` 间距）；
// 2. 再做 10 轮松弛，交替参考父/子节点的平均位置，把节点拉向邻居；
// 3. 最后整体平移到最小左边界为 0，并四舍五入成整数。
fn assign_positions(
    by_rank: &[Vec<usize>],
    size: &[i64],
    sep: i64,
    edges: &[Edge],
    ranks: &[usize],
) -> Vec<i64> {
    let n = size.len();
    let mut parents: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for edge in edges {
        if edge.from != edge.to && ranks[edge.to] > ranks[edge.from] {
            parents[edge.to].push(edge.from);
            children[edge.from].push(edge.to);
        }
    }

    let mut pos = vec![0.0f64; n];
    for row in by_rank {
        let mut x = 0.0f64;
        for v in row {
            let h = size[*v] as f64 / 2.0;
            x += h;
            pos[*v] = x;
            x += h + sep as f64;
        }
    }

    for iteration in 0..10 {
        let rows: Vec<Vec<usize>> = if iteration % 2 == 0 {
            by_rank.to_vec()
        } else {
            by_rank.iter().rev().cloned().collect()
        };
        let neighbors = if iteration % 2 == 0 {
            &parents
        } else {
            &children
        };
        for row in &rows {
            relax_rank(row, neighbors, &mut pos, size, sep);
        }
    }

    let mut min_left = f64::INFINITY;
    for v in 0..n {
        min_left = min_left.min(pos[v] - size[v] as f64 / 2.0);
    }
    if !min_left.is_finite() {
        min_left = 0.0;
    }
    (0..n)
        .map(|v| (pos[v] - min_left).max(0.0).round() as i64)
        .collect()
}

// 一层节点的松弛：目标位置取邻居平均位置（没有邻居就保持原位）；然后从左到右、从右到左
// 各扫一遍，把目标位置压进“相邻节点不重叠且至少相隔 sep”的可行区间，取区间中点，最后
// 再从左到右强制一遍最小间距。
fn relax_rank(nodes: &[usize], neigh: &[Vec<usize>], pos: &mut [f64], size: &[i64], sep: i64) {
    let n = nodes.len();
    if n == 0 {
        return;
    }
    let desired: Vec<f64> = nodes
        .iter()
        .map(|v| {
            if neigh[*v].is_empty() {
                pos[*v]
            } else {
                neigh[*v].iter().map(|u| pos[*u]).sum::<f64>() / neigh[*v].len() as f64
            }
        })
        .collect();
    let half_of = |i: usize| size[nodes[i]] as f64 / 2.0;

    let mut left = vec![0.0f64; n];
    for i in 0..n {
        left[i] = if i == 0 {
            desired[i]
        } else {
            desired[i].max(left[i - 1] + half_of(i - 1) + sep as f64 + half_of(i))
        };
    }
    let mut right = vec![0.0f64; n];
    for i in (0..n).rev() {
        right[i] = if i == n - 1 {
            desired[i]
        } else {
            desired[i].min(right[i + 1] - half_of(i + 1) - sep as f64 - half_of(i))
        };
    }
    for i in 0..n {
        pos[nodes[i]] = (left[i] + right[i]) / 2.0;
    }
    for i in 1..n {
        let min_p = pos[nodes[i - 1]] + half_of(i - 1) + sep as f64 + half_of(i);
        if pos[nodes[i]] < min_p {
            pos[nodes[i]] = min_p;
        }
    }
}

// 把区间分到尽量少的“车道”（区间图贪心着色）：先按 (起点, 终点, from, to, 边下标) 排序，
// 再为每个区间找第一条车道，要求对车道内每个已有区间都满足：不与它重叠（两端各留 2 格
// 余量），或与它共享 from/to 节点。
// 返回 (边下标 → 车道号) 与车道总数。
//
// 排序键的最后一项是边下标，因此输入顺序不同也不会改变分配结果，golden 输出可重复。
fn assign_tracks(spans: &[Span5]) -> (Vec<(usize, usize)>, usize) {
    let mut sorted = spans.to_vec();
    sorted.sort_by_key(|span| (span.0, span.1, span.2, span.3, span.4));
    let mut tracks: Vec<Vec<(i64, i64, i64, i64)>> = Vec::new();
    let mut assigned: Vec<(usize, usize)> = Vec::new();
    for (s, e, f, t, idx) in sorted {
        let slot = tracks.iter().position(|members| {
            members
                .iter()
                .all(|(s2, e2, f2, t2)| e2 + 2 <= s || e + 2 <= *s2 || *f2 == f || *t2 == t)
        });
        let slot = match slot {
            Some(slot) => slot,
            None => {
                tracks.push(Vec::new());
                tracks.len() - 1
            }
        };
        tracks[slot].push((s, e, f, t));
        assigned.push((idx, slot));
    }
    (assigned, tracks.len())
}

// 收集第 `rank` 层到下一层的边（要求 rank 差恰为 1）里需要横向移动的那些，作为总线的
// 区间。`exact` 为真时“中心不同”就算需要总线；否则只认相差 > 1 的边——相差 1 列的边由
// `route_forward` 直接落在目标列上，不需要总线。
fn bus_spans(
    graph: &Graph,
    ranks: &[usize],
    centers: &[i64],
    rank: usize,
    exact: bool,
) -> Vec<Span5> {
    let mut out = Vec::new();
    for (i, edge) in graph.edges.iter().enumerate() {
        let jogs = if exact {
            centers[edge.from] != centers[edge.to]
        } else {
            (centers[edge.from] - centers[edge.to]).abs() > 1
        };
        if edge.from != edge.to && ranks[edge.from] == rank && ranks[edge.to] == rank + 1 && jogs {
            out.push((
                centers[edge.from].min(centers[edge.to]),
                centers[edge.from].max(centers[edge.to]),
                edge.from as i64,
                edge.to as i64,
                i,
            ));
        }
    }
    out
}

// 收集跨层边的区间，用来在画布边缘分配“车道”。这里包含所有 rank 差不为 1 的边（回边、
// 同层边、跨多层的边）；自环由 `route_self` 单独处理，跳过。`vertical` 为真时用 y 区间
// （TD），否则用 x 区间（LR）。
fn lane_spans(graph: &Graph, ranks: &[usize], placed: &[Placed], vertical: bool) -> Vec<Span5> {
    let mut out = Vec::new();
    for (i, edge) in graph.edges.iter().enumerate() {
        if edge.from == edge.to || ranks[edge.to] == ranks[edge.from] + 1 {
            continue;
        }
        let from = placed[edge.from];
        let to = placed[edge.to];
        let a = if vertical {
            from.cy.min(to.cy)
        } else {
            from.cx.min(to.cx)
        };
        let b = if vertical {
            from.cy.max(to.cy)
        } else {
            from.cx.max(to.cx)
        };
        out.push((a, b, edge.from as i64, edge.to as i64, i));
    }
    out
}

// TD/TB 布局：层沿 y 轴排列，层内沿 x 轴排开。
//
// 阶段：
// 1. `assign_positions` 得到每层节点的中心 x（用 `lay_w`，自环节点已预留标签宽度）；
// 2. 为每一层计算需要横向移动的相邻边（总线），`assign_tracks` 得到每条边的槽位；
// 3. 每层高度取该层最高的框（含自环额外高度）；层间距取 `GAP_Y` 与“总线槽数 + 1”的
//    较大者，得到 `rank_y`、`band_end` 与画布高度；
// 4. 按中心摆放节点框，`diagram_w` 记录右侧最远点；自环节点的标签可能更靠右；
// 5. 带标签的边可能伸出画布：相邻层按“目标中心 + 2 + 标签宽”，跨层按
//    “diagram_w + 标签宽 + 1”抬高 `content_w`；
// 6. 跨层边在画布右侧分车道，`canvas_w = content_w + 1 + 车道数`。
//
// 返回的 `band_end[rank]` 是第 rank 层的下边界，总线画在它下方第 `edge_bus[i]` 行。
fn place_td(
    ranks: &[usize],
    max_rank: usize,
    by_rank: &[Vec<usize>],
    sizes: &NodeSizes,
    graph: &Graph,
    placed: &mut [Placed],
) -> RoutePlan {
    let centers = assign_positions(by_rank, &sizes.lay_w, GAP_X, &graph.edges, ranks);

    let mut edge_bus = vec![0usize; graph.edges.len()];
    let mut bus_tracks = vec![0usize; max_rank + 1];
    for (rank, track) in bus_tracks.iter_mut().enumerate().take(max_rank) {
        let spans = bus_spans(graph, ranks, &centers, rank, false);
        if spans.is_empty() {
            continue;
        }
        let (assigned, count) = assign_tracks(&spans);
        for (idx, slot) in assigned {
            edge_bus[idx] = slot;
        }
        *track = count;
    }

    let rank_h: Vec<i64> = by_rank
        .iter()
        .map(|row| {
            if row.is_empty() {
                3
            } else {
                row.iter()
                    .map(|i| sizes.box_h[*i] + sizes.extra_h[*i])
                    .max()
                    .unwrap_or(0)
            }
        })
        .collect();
    let mut rank_y = vec![0i64; max_rank + 1];
    for rank in 1..=max_rank {
        rank_y[rank] =
            rank_y[rank - 1] + rank_h[rank - 1] + GAP_Y.max(bus_tracks[rank - 1] as i64 + 1);
    }
    let canvas_h = rank_y[max_rank] + rank_h[max_rank];
    let band_end: Vec<i64> = (0..=max_rank)
        .map(|rank| rank_y[rank] + rank_h[rank])
        .collect();

    let mut diagram_w = 1i64;
    for (rank, row) in by_rank.iter().enumerate() {
        for idx in row {
            let w = sizes.box_w[*idx];
            let h = sizes.box_h[*idx];
            let cx = centers[*idx];
            let x = sat(cx, half(w));
            let y = rank_y[rank] + half(rank_h[rank] - h - sizes.extra_h[*idx]);
            placed[*idx] = Placed {
                x,
                y,
                w,
                h,
                cx,
                cy: y + half(h),
                rank,
            };
            diagram_w = diagram_w.max(x + w);
            if sizes.extra_h[*idx] > 0 && sizes.self_label_w[*idx] > 0 {
                diagram_w = diagram_w.max(x + w + 2 + sizes.self_label_w[*idx]);
            }
        }
    }

    let mut content_w = diagram_w;
    for edge in &graph.edges {
        if edge.from == edge.to {
            continue;
        }
        let Some(label) = &edge.label else {
            continue;
        };
        let lw = string_width(label).min(MAX_LABEL) as i64;
        content_w = if ranks[edge.to] == ranks[edge.from] + 1 {
            content_w.max(placed[edge.to].cx + 2 + lw)
        } else {
            content_w.max(diagram_w + lw + 1)
        };
    }

    let mut edge_lane = vec![0usize; graph.edges.len()];
    let lanes = lane_spans(graph, ranks, placed, true);
    let mut canvas_w = content_w;
    let mut lane_base = 0i64;
    if !lanes.is_empty() {
        let (assigned, count) = assign_tracks(&lanes);
        for (idx, slot) in assigned {
            edge_lane[idx] = slot;
        }
        canvas_w = content_w + 1 + count as i64;
        lane_base = content_w + 1;
    }

    RoutePlan {
        canvas_w,
        canvas_h,
        band_end,
        edge_bus,
        lane_base,
        edge_lane,
    }
}

// LR/RL 布局：层沿 x 轴排列，层内沿 y 轴排开。与 `place_td` 一一对应：
// - 列宽取该层最宽的框；
// - 层间距取 `GAP_X + 1` 与“最宽的自环/相邻层连线标签 + 3”的较大者，给标签预留宽度；
//   总线槽数会进一步加宽间距；
// - 跨层边在画布底部按车道绘制；
// - 最后一层里的自环节点会在右侧额外占用标签宽度（`self_tails`）。
fn place_lr(
    ranks: &[usize],
    max_rank: usize,
    by_rank: &[Vec<usize>],
    sizes: &NodeSizes,
    graph: &Graph,
    placed: &mut [Placed],
) -> RoutePlan {
    let col_w: Vec<i64> = by_rank
        .iter()
        .map(|row| {
            if row.is_empty() {
                0
            } else {
                row.iter().map(|i| sizes.box_w[*i]).max().unwrap_or(0)
            }
        })
        .collect();

    let label_widths: Vec<usize> = graph
        .edges
        .iter()
        .filter(|edge| edge.from == edge.to || ranks[edge.to] == ranks[edge.from] + 1)
        .filter_map(|edge| edge.label.as_ref())
        .map(|label| string_width(label).min(MAX_LABEL))
        .collect();
    let max_label = label_widths.iter().copied().max().unwrap_or(0) as i64;
    let base_gap = (GAP_X + 1).max(max_label + 3);

    let centers = assign_positions(by_rank, &sizes.lay_h, 1, &graph.edges, ranks);

    let mut edge_bus = vec![0usize; graph.edges.len()];
    let mut bus_tracks = vec![0usize; max_rank + 1];
    for (rank, track) in bus_tracks.iter_mut().enumerate().take(max_rank) {
        let spans = bus_spans(graph, ranks, &centers, rank, true);
        if spans.is_empty() {
            continue;
        }
        let (assigned, count) = assign_tracks(&spans);
        for (idx, slot) in assigned {
            edge_bus[idx] = slot;
        }
        *track = count;
    }

    let mut rank_x = vec![0i64; max_rank + 1];
    for rank in 1..=max_rank {
        rank_x[rank] =
            rank_x[rank - 1] + col_w[rank - 1] + base_gap.max(bus_tracks[rank - 1] as i64 + 1);
    }
    let self_tails: Vec<i64> = by_rank
        .get(max_rank)
        .map(|row| {
            row.iter()
                .filter(|i| sizes.extra_h[**i] > 0 && sizes.self_label_w[**i] > 0)
                .map(|i| 2 + sizes.self_label_w[*i])
                .collect()
        })
        .unwrap_or_default();
    let canvas_w =
        rank_x[max_rank] + col_w[max_rank] + self_tails.iter().copied().max().unwrap_or(0);
    let band_end: Vec<i64> = (0..=max_rank)
        .map(|rank| rank_x[rank] + col_w[rank])
        .collect();

    let mut diagram_h = 1i64;
    for (rank, row) in by_rank.iter().enumerate() {
        let x = rank_x[rank];
        for idx in row {
            let w = sizes.box_w[*idx];
            let h = sizes.box_h[*idx];
            let cy = centers[*idx];
            let y = sat(cy, half(h + sizes.extra_h[*idx]));
            placed[*idx] = Placed {
                x,
                y,
                w,
                h,
                cx: x + half(w),
                cy: y + half(h),
                rank,
            };
            diagram_h = diagram_h.max(y + h + sizes.extra_h[*idx]);
        }
    }

    let mut edge_lane = vec![0usize; graph.edges.len()];
    let lanes = lane_spans(graph, ranks, placed, false);
    let mut canvas_h = diagram_h;
    let mut lane_base = 0i64;
    if !lanes.is_empty() {
        let (assigned, count) = assign_tracks(&lanes);
        for (idx, slot) in assigned {
            edge_lane[idx] = slot;
        }
        canvas_h = diagram_h + 1 + count as i64;
        lane_base = diagram_h + 1;
    }

    RoutePlan {
        canvas_w,
        canvas_h,
        band_end,
        edge_bus,
        lane_base,
        edge_lane,
    }
}

// 一组行里最宽的列数；空集合返回 1，避免后续算出 0 宽度的框。
fn widest(lines: &[String]) -> i64 {
    lines
        .iter()
        .map(|line| string_width(line) as i64)
        .max()
        .unwrap_or(1)
        .max(1)
}

// 把解析后的图布局并绘制成画布；空图或画布超过 MAX_CANVAS_CELLS 时返回 None。
//
// 流程：
// 1. compute_ranks 分层，按层分桶后 order_ranks 减少交叉；
// 2. 每个节点标签 wrap_label 成最多 MAX_LINES 行，据此算出框尺寸（宽 = 最长行 +
//    2*PAD + 2，高 = 行数 + 2，额外的 2 列/行留给左右与上下边框）；自环额外占 2 行
//    高度，并把框宽抬到至少 7；
// 3. Down/Up 走 place_td，Right/Left 走 place_lr；Up/Left 先按 Down/Right 画完再整体
//    翻转；
// 4. 依次画节点框，再按每条边的线型设置 cur_style 并路由连线；
// 5. finalize_mask 把方向位合成盒线字符，最后按需翻转。
//
// 画布格数按 canvas_w * canvas_h 检查，超限就放弃整图（调用方保留代码块）。
fn layout_canvas(graph: &Graph) -> Option<Canvas> {
    let n = graph.nodes.len();
    if n == 0 {
        return None;
    }
    let ranks = compute_ranks(graph);
    let max_rank = ranks.iter().copied().max().unwrap_or(0);
    let mut by_rank: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
    for (idx, rank) in ranks.iter().enumerate() {
        by_rank[*rank].push(idx);
    }
    order_ranks(&mut by_rank, &graph.edges, &ranks);

    let wrapped: Vec<Vec<String>> = graph
        .nodes
        .iter()
        .map(|node| wrap_label(&node.label, WRAP_WIDTH, MAX_LINES))
        .collect();
    let box_w: Vec<i64> = wrapped
        .iter()
        .map(|lines| widest(lines) + 2 * PAD + 2)
        .collect();
    let box_h: Vec<i64> = wrapped.iter().map(|lines| lines.len() as i64 + 2).collect();

    let mut extra_h = vec![0i64; n];
    let mut self_label_w = vec![0i64; n];
    for edge in &graph.edges {
        if edge.from != edge.to {
            continue;
        }
        extra_h[edge.from] = 2;
        if let Some(label) = &edge.label {
            self_label_w[edge.from] =
                self_label_w[edge.from].max(string_width(label).min(MAX_LABEL) as i64);
        }
    }
    let mut box_w = box_w;
    for i in 0..n {
        if extra_h[i] > 0 {
            box_w[i] = box_w[i].max(7);
        }
    }
    let sizes = NodeSizes {
        box_w: box_w.clone(),
        box_h: box_h.clone(),
        lay_w: box_w
            .iter()
            .enumerate()
            .map(|(i, w)| {
                w + if self_label_w[i] > 0 {
                    2 * (self_label_w[i] + 3)
                } else {
                    0
                }
            })
            .collect(),
        lay_h: box_h
            .iter()
            .enumerate()
            .map(|(i, h)| h + extra_h[i])
            .collect(),
        extra_h,
        self_label_w,
    };

    let mut placed = vec![Placed::default(); n];
    let dir = graph.dir();
    let vertical = matches!(dir, Dir::Down | Dir::Up);
    let plan = if vertical {
        place_td(&ranks, max_rank, &by_rank, &sizes, graph, &mut placed)
    } else {
        place_lr(&ranks, max_rank, &by_rank, &sizes, graph, &mut placed)
    };

    if plan.canvas_w * plan.canvas_h > MAX_CANVAS_CELLS {
        return None;
    }

    let mut canvas = Canvas::new(plan.canvas_w, plan.canvas_h);
    for idx in 0..n {
        draw_box(
            &mut canvas,
            placed[idx],
            &wrapped[idx],
            graph.nodes[idx].shape,
        );
    }

    for (i, edge) in graph.edges.iter().enumerate() {
        canvas.cur_style = match edge.line {
            LineKind::Dotted => STY_DOT,
            LineKind::Thick => STY_THICK,
            LineKind::Solid => STY_SOLID,
        };
        if edge.from == edge.to {
            route_self(&mut canvas, placed[edge.from], edge);
            continue;
        }
        let from = placed[edge.from];
        let to = placed[edge.to];
        let adjacent = to.rank == from.rank + 1;
        let bus = plan.band_end[from.rank] + plan.edge_bus[i] as i64;
        let lane = plan.lane_base + plan.edge_lane[i] as i64;
        if vertical {
            if adjacent {
                route_forward(&mut canvas, from, to, edge, bus);
            } else {
                route_back(&mut canvas, from, to, edge, lane);
            }
        } else if adjacent {
            route_forward_lr(&mut canvas, from, to, edge, bus);
        } else {
            route_back_lr(&mut canvas, from, to, edge, lane);
        }
    }

    canvas.finalize_mask();
    match dir {
        Dir::Up => canvas.flip_vertical(),
        Dir::Left => canvas.flip_horizontal(),
        _ => {}
    }
    Some(canvas)
}

// 画一个节点框：四角直接 set，四条边用 add_bits_cls 写方向位（这样连线可以在边上用
// junction 开口接出）；最后把整块矩形标记 occupied，防止连线覆盖框内文字。
// 标签逐行居中绘制，超出内宽的由 fit_label 截断。
fn draw_box(canvas: &mut Canvas, p: Placed, lines: &[String], shape: Shape) {
    let (x, y, w, h) = (p.x, p.y, p.w, p.h);
    let right = x + w - 1;
    let bottom = y + h - 1;
    let rounded = shape == Shape::Round || shape == Shape::Diamond;
    canvas.set(x, y, if rounded { '╭' } else { '┌' }, SpanClass::Border);
    canvas.set(right, y, if rounded { '╮' } else { '┐' }, SpanClass::Border);
    canvas.set(
        x,
        bottom,
        if rounded { '╰' } else { '└' },
        SpanClass::Border,
    );
    canvas.set(
        right,
        bottom,
        if rounded { '╯' } else { '┘' },
        SpanClass::Border,
    );

    let mut cx = x + 1;
    while cx < right {
        canvas.add_bits_cls(cx, y, L | R, SpanClass::Border);
        canvas.add_bits_cls(cx, bottom, L | R, SpanClass::Border);
        cx += 1;
    }
    let mut cy = y + 1;
    while cy < bottom {
        canvas.add_bits_cls(x, cy, U | D, SpanClass::Border);
        canvas.add_bits_cls(right, cy, U | D, SpanClass::Border);
        cy += 1;
    }
    let mut cy = y;
    while cy <= bottom {
        let mut cx = x;
        while cx <= right {
            let index = canvas.idx(cx, cy);
            canvas.occupied[index] = true;
            cx += 1;
        }
        cy += 1;
    }

    let inner = sat(w, 2 * PAD + 2).max(1);
    for (li, line) in lines.iter().enumerate() {
        let text = fit_label(line, inner as usize);
        let text_x = x + 1 + PAD + half(sat(inner, string_width(&text) as i64));
        draw_text(canvas, &text, text_x, y + 1 + li as i64, SpanClass::Text);
    }
}

// 端点箭头字形：圆点/叉用 'o'/'×'，普通箭头用调用方给的默认字符（▼/▲/◄/▶）。
fn head_glyph(head: Head, arrow: char) -> char {
    match head {
        Head::Circle => 'o',
        Head::Cross => '×',
        _ => arrow,
    }
}

// 相邻层（TD 向下）的连线：从源框底边出来，竖直到该层的总线行，需要时横移到目标列，
// 再竖直到目标框上方一行。`bx` 的选择：两中心相差不超过 1 时直接落在目标列，省掉横移。
// 有箭头就写箭头字形；没有箭头时只补一个方向位，让目标框边自然延续。
// 标签放在箭头右侧同一行。
fn route_forward(canvas: &mut Canvas, from: Placed, to: Placed, edge: &Edge, bus: i64) {
    let tx = to.cx;
    let bx = if (from.cx - tx).abs() <= 1 {
        tx
    } else {
        from.cx
    };
    let by = from.y + from.h - 1;
    let head_row = to.y - 1;

    canvas.junction(bx, by, D);
    canvas.seg_v(bx, by, bus);
    if bx == tx {
        canvas.seg_v(bx, bus, head_row);
    } else {
        canvas.seg_h(bus, bx, tx);
        canvas.seg_v(tx, bus, head_row);
    }

    if edge.head_to == Head::None {
        canvas.add_bits(tx, head_row, U);
    } else {
        canvas.set(tx, head_row, head_glyph(edge.head_to, '▼'), SpanClass::Edge);
    }
    if edge.head_from != Head::None {
        canvas.set(bx, by, head_glyph(edge.head_from, '▲'), SpanClass::Edge);
    }
    if let Some(label) = &edge.label {
        place_label(canvas, label, head_row, tx + 1);
    }
}

// 自环：从框底偏右处向下绕两行、向右、再向上接回框底另一处，画成一个小环，箭头画在
// 回接处。空间不够（`ret_x <= exit_x`，或画布底部不够两行）时直接放弃，什么都不画。
// 标签放在框右侧同一行；`place_label` 会跳过被占用的格子。
fn route_self(canvas: &mut Canvas, p: Placed, edge: &Edge) {
    let bottom = p.y + p.h - 1;
    let exit_x = p.cx + 1;
    let ret_x = p.x + p.w - 2;
    if ret_x <= exit_x || bottom + 2 >= canvas.h {
        return;
    }
    let (v, h, bl, br) = match edge.line {
        LineKind::Dotted => ('╎', '╌', '╰', '╯'),
        LineKind::Thick => ('┃', '━', '┗', '┛'),
        LineKind::Solid => ('│', '─', '╰', '╯'),
    };
    canvas.junction(exit_x, bottom, D);
    canvas.set(exit_x, bottom + 1, v, SpanClass::Edge);
    canvas.set(exit_x, bottom + 2, bl, SpanClass::Edge);
    let mut x = exit_x + 1;
    while x < ret_x {
        canvas.set(x, bottom + 2, h, SpanClass::Edge);
        x += 1;
    }
    canvas.set(ret_x, bottom + 2, br, SpanClass::Edge);
    canvas.set(
        ret_x,
        bottom + 1,
        head_glyph(edge.head_to, '▲'),
        SpanClass::Edge,
    );
    if let Some(label) = &edge.label {
        place_label(canvas, label, bottom + 1, p.x + p.w + 1);
    }
}

// 跨层 TD 连线：从源框右边出来，横到右侧车道 x，竖直到目标中心行，再向左接到目标框
// 右边一列；箭头朝左（◄）。标签放在目标中心行的上一行、车道的左侧
// （起点 = 车道 x 减标签宽再减 1，且不小于 0）。
fn route_back(canvas: &mut Canvas, from: Placed, to: Placed, edge: &Edge, lane_x: i64) {
    let sx = from.x + from.w - 1;
    let sy = from.cy;
    let tx = to.x + to.w - 1;
    let tyc = to.cy;

    canvas.junction(sx, sy, R);
    canvas.seg_h(sy, sx, lane_x);
    canvas.seg_v(lane_x, sy, tyc);
    canvas.seg_h(tyc, tx + 1, lane_x);

    if edge.head_to == Head::None {
        canvas.add_bits(tx + 1, tyc, R);
    } else {
        canvas.set(tx + 1, tyc, head_glyph(edge.head_to, '◄'), SpanClass::Edge);
    }
    if edge.head_from != Head::None {
        canvas.set(sx, sy, head_glyph(edge.head_from, '◄'), SpanClass::Edge);
    }
    if let Some(label) = &edge.label {
        place_label(
            canvas,
            label,
            sat(tyc, 1),
            sat(lane_x, string_width(label) as i64 + 1),
        );
    }
}

// 相邻层 LR 连线：从源框右边出来，横到该层总线列，需要时竖移到目标中心行，再横到目标
// 框左边一列；箭头朝右（▶）。标签放在目标中心行的上一行、总线列的右侧一格。
fn route_forward_lr(canvas: &mut Canvas, from: Placed, to: Placed, edge: &Edge, bus: i64) {
    let rx = from.x + from.w - 1;
    let ry = from.cy;
    let ly = to.cy;
    let head_col = to.x - 1;

    canvas.junction(rx, ry, R);
    canvas.seg_h(ry, rx, bus);
    if ry == ly {
        canvas.seg_h(ry, bus, head_col);
    } else {
        canvas.seg_v(bus, ry, ly);
        canvas.seg_h(ly, bus, head_col);
    }

    if edge.head_to == Head::None {
        canvas.add_bits(head_col, ly, R);
    } else {
        canvas.set(head_col, ly, head_glyph(edge.head_to, '▶'), SpanClass::Edge);
    }
    if edge.head_from != Head::None {
        canvas.set(rx, ry, head_glyph(edge.head_from, '◄'), SpanClass::Edge);
    }
    if let Some(label) = &edge.label {
        place_label(canvas, label, sat(ly, 1), bus + 1);
    }
}

// 跨层 LR 回边：从源框底边出来，竖直到下方车道 y，横移到目标列，再向上接到目标框底边
// 下一行；箭头朝上（▲）。标签放在车道的上一行、两框中心之间。
fn route_back_lr(canvas: &mut Canvas, from: Placed, to: Placed, edge: &Edge, lane_y: i64) {
    let sx = from.cx;
    let sy = from.y + from.h - 1;
    let tx = to.cx;
    let ty = to.y + to.h - 1;

    canvas.junction(sx, sy, D);
    canvas.seg_v(sx, sy, lane_y);
    canvas.seg_h(lane_y, sx, tx);
    canvas.seg_v(tx, lane_y, ty + 1);

    if edge.head_to == Head::None {
        canvas.add_bits(tx, ty + 1, D);
    } else {
        canvas.set(tx, ty + 1, head_glyph(edge.head_to, '▲'), SpanClass::Edge);
    }
    if edge.head_from != Head::None {
        canvas.set(sx, sy, head_glyph(edge.head_from, '▲'), SpanClass::Edge);
    }
    if let Some(label) = &edge.label {
        place_label(canvas, label, sat(lane_y, 1), half(sx + tx));
    }
}

// 在 (row, start_x) 放一段连线标签：
// 1. 先按 MAX_LABEL 截断；
// 2. 逐簇检查目标格：越界、已有字符、方向位非 0 或 occupied 都算阻挡，遇到第一个阻挡
//    就停止（标签宁可短，也不覆盖节点框或其它连线）；
// 3. 宽字形的后续格子写 CONT。
fn place_label(canvas: &mut Canvas, label: &str, row: i64, start_x: i64) {
    if row >= canvas.h {
        return;
    }
    let text = fit_label(label, MAX_LABEL);
    let mut x = start_x;
    for (cluster, cw) in measured(&text) {
        if cw == 0 {
            continue;
        }
        if x + cw as i64 > canvas.w {
            break;
        }
        let mut blocked = false;
        let mut k = 0;
        while k < cw as i64 {
            if x + k < 0 || x + k >= canvas.w || row < 0 {
                blocked = true;
                break;
            }
            let i = canvas.idx(x + k, row);
            if canvas.ch[i] != " " || canvas.mask[i] != 0 || canvas.occupied[i] {
                blocked = true;
                break;
            }
            k += 1;
        }
        if blocked {
            break;
        }
        canvas.set_str(x, row, cluster, SpanClass::EdgeLabel);
        let mut k = 1;
        while k < cw as i64 {
            canvas.set_str(x + k, row, CONT, SpanClass::EdgeLabel);
            k += 1;
        }
        x += cw as i64;
    }
}

// ============================================================================
// 入口
// ============================================================================

/// `render(src)` 的一轮绘制：flowchart 的 `MermaidArt`；空输入/不支持的图/超限 → `None`。
///
/// `parse_graph` 的外层与内层 `None` 在这里折叠成同一个 `None`；成功时把
/// `Graph::warnings` 原样带进 `MermaidArt`——警告本身不影响出图，只影响调用方的最终渲染
/// 回退策略。
fn draw_mermaid(src: &str) -> Option<MermaidArt> {
    let graph = parse_graph(src)??;
    let canvas = layout_canvas(&graph)?;
    let (plain, styled, width) = canvas.to_lines();
    Some(MermaidArt {
        plain,
        styled,
        width,
        warnings: graph.warnings,
    })
}

/// `render(src)`：flowchart 的 `MermaidArt`；空输入/不支持的图/超限 → `None`。
///
/// 与 grok-mermaid 的 `render()` 一致：第一次失败了就**去掉最后一行**再试一次
/// （`attempt()`）。流式输出时最后一行通常只打了一半；对 flowchart 而言这还让超过
/// `MAX_NODES`/`MAX_EDGES` 的图退回到能画的前缀，并追加
/// `dropped, unreadable final line: "<line>"` 警告（最终渲染显示警告，流式中照常出图）。
///
/// 重试的实现细节：先 `trim_end`，再找最后一个 `\n` 并对前缀重新绘制；单行输入没有换行
/// 可去，直接返回 `None`。重试成功时把去掉的那一行（`trim` 后）追加到 warning 末尾。
/// 重试只做一次，且不区分失败原因：语法错误、节点/边超限、画布过大都走这条路径。
fn render_mermaid(src: &str) -> Option<MermaidArt> {
    let src = strip_controls(src);
    if src.trim().is_empty() {
        return None;
    }
    let art = match draw_mermaid(&src) {
        Some(art) => art,
        None => {
            let body = src.trim_end();
            let cut = body.rfind('\n')?;
            let mut salvaged = draw_mermaid(&body[..cut])?;
            let dropped = body[cut + 1..].trim();
            salvaged
                .warnings
                .push(format!("dropped, unreadable final line: \"{dropped}\""));
            salvaged
        }
    };
    Some(art)
}

/// `diagramKind(src)`：仅用于测试与宿主诊断（flowchart 之外的 kind 也返回名字）。
///
/// 只按首条语句的首词分类，与能否真正渲染无关：`Flowchart` 也可能因为 `subgraph`、节点
/// 上限或画布上限而画不出来；其余变体一律保留原始代码块。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramKind {
    /// `graph` / `flowchart`。
    Flowchart,
    /// `stateDiagram*`。
    State,
    /// `classDiagram*`。
    Class,
    /// `erDiagram`。
    Er,
    /// `sequenceDiagram`。
    Sequence,
}

/// `diagramKind(src)`。
///
/// 先去掉 C0/C1 控制字符，再按行与 `;` 切成语句，取第一条非空语句的首词转小写后比较：
/// `graph`/`flowchart` → [`DiagramKind::Flowchart`]；`statediagram*`、`classdiagram*`、
/// `erdiagram`、`sequencediagram` → 对应变体；其它（含空输入、`pie`）→ `None`。
///
/// 只看第一条语句：文件头前的空行与 `%%` 注释行不产生语句，因此不影响结果。
pub fn diagram_kind(src: &str) -> Option<DiagramKind> {
    let statements = statements_of(&strip_controls(src));
    let kind = header_kind(&statements)?;
    if kind == "graph" || kind == "flowchart" {
        return Some(DiagramKind::Flowchart);
    }
    if kind.starts_with("statediagram") {
        return Some(DiagramKind::State);
    }
    if kind.starts_with("classdiagram") {
        return Some(DiagramKind::Class);
    }
    if kind == "erdiagram" {
        return Some(DiagramKind::Er);
    }
    if kind == "sequencediagram" {
        return Some(DiagramKind::Sequence);
    }
    None
}

/// 公开的 `render`：返回 `(plain 行, 宽度, 警告)`（测试与诊断用）。
///
/// 直接走 `render_mermaid` 的完整流程（含去尾行重试），不带主题、不看终端宽度。
/// 返回 `None` 的条件：空输入、非 flowchart、含 `subgraph`、节点/边超过上限，或画布超过
/// `MAX_CANVAS_CELLS` 格。宽度单位是终端列。
pub fn render_mermaid_plain(src: &str) -> Option<(Vec<String>, usize, Vec<String>)> {
    render_mermaid(src).map(|art| (art.plain, art.width, art.warnings))
}
