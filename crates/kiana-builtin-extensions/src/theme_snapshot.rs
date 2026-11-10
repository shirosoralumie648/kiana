//! `Theme` 的只读快照（`mcp/ui.rs` 与 `llama/ui.rs` 共用）。
//!
//! TS 的 `ctx.ui.custom` 把宿主主题实例直接交给工厂；Rust 的 `Theme` 不是 `Clone`，而工厂
//! 只借出 `&Theme`，组件却必须活到工厂返回之后。这里在工厂里捕获 `fg`/`bold` 的 ANSI 序列，
//! 得到可拥有的快照，从而使用**宿主传入的主题**（而不是全局当前主题）。
//!
//! 捕获到的开/关序列与 `Theme::fg`/`Theme::bold` 逐字节一致：用一个不会出现在 ANSI 序列
//! 里的探针（U+0000）渲染，切成开/关序列；`Theme::bold("")` 返回空串（chalk 语义），
//! `Theme::fg("")` 仍带序列，因此额外记录 `empty_returns_empty`。
//!
//! # 调用方与生命周期
//!
//! `mcp/ui.rs` 的 `McpManagerView` 与 `llama/ui.rs` 的 `LlamaView` 在 `CustomFactory` 的
//! 同步工厂体里调用 `ThemeSnapshot::capture`，再把快照移进返回的 `'static` future。快照只
//! 持有 ANSI 字符串、不引用 `Theme`，所以视图存活期间原 `Theme` 的修改或释放都不会影响
//! 已渲染的结果。
//!
//! # 结构与不变量
//!
//! - `fg` 表按 `ThemeColor::ALL` 逐项捕获；该常量由 `theme_tokens.rs` 的宏与枚举一同生成，
//!   当前覆盖全部 49 个前景 token。查表是线性扫描；找不到时原样返回文本，属于防御性分支。
//! - `bold` 单独捕获，因为 `Theme::bold` 的空串语义与 `fg` 不同。
//!
//! # 注意
//!
//! `AnsiWrap::wrap` 只做 `open + text + close`。这与 `Theme::fg` 的形状完全一致；但
//! `Theme::bold` 走 `chalk_wrap`，文本含 `\n` 时会在换行前后关闭/重开、含 ESC 时会
//! `replaceClose`。快照没有复制这两条规则，因此对含换行或 ESC 的文本调用
//! `ThemeSnapshot::bold` 与直接调用 `Theme::bold` 的结果不同。本 crate 内 `bold` 的调用点
//! 都是标题文本；若标题含换行或 ESC，就会遇到上述差异。
//!
//! 对空串与普通文本的字节一致性由 `mcp/ui.rs` 的
//! `theme_snapshot_matches_theme_fg_and_bold` 测试锁定；该测试的 `fg` 循环还覆盖含 NUL
//! 的文本（`"a\u{0}b"`），`bold` 循环则只覆盖空串与普通文本，未测含 NUL 的输入。

use kiana_theme::ThemeColor;
use kiana_theme::theme::Theme;

/// 一段着色包裹（开始时序、结束时序）。捕获自 `Theme::fg`/`Theme::bold`。
#[derive(Clone)]
pub(crate) struct AnsiWrap {
    /// 文本之前的开启序列（dim token 会包含额外的 `\x1b[2m`）。
    open: String,
    /// 文本之后的关闭序列。
    close: String,
    /// `Theme::bold("")` 返回空串（chalk 语义），`Theme::fg("")` 仍带序列。
    empty_returns_empty: bool,
}

impl AnsiWrap {
    /// 用一个不会出现在 ANSI 序列里的探针捕获开/关序列。
    ///
    /// `render` 必须原样保留传入文本（`Theme::fg`/`Theme::bold` 都是 `open + text + close`
    /// 的形状）；若实现改写了探针，`find` 失败会 panic。探针选 U+0000 是因为它不可能出现在
    /// ANSI 转义序列中，渲染结果里唯一的 NUL 就是探针本身。
    ///
    /// `empty_returns_empty` 用 `render("")` 单独探测：chalk 的 `bold` 对空串返回空串，而
    /// `fg` 仍会返回开/关序列，两者必须区分。
    pub(crate) fn capture(render: impl Fn(&str) -> String) -> Self {
        const PROBE: &str = "\u{0}";
        let rendered = render(PROBE);
        let split = rendered
            .find(PROBE)
            .expect("theme coloring keeps the text verbatim");
        AnsiWrap {
            open: rendered[..split].to_string(),
            close: rendered[split + PROBE.len()..].to_string(),
            empty_returns_empty: render("").is_empty(),
        }
    }

    /// 按捕获到的开/关序列包裹文本。
    ///
    /// 只有「`text` 为空且原渲染函数对空串返回空串」时返回空串（`Theme::bold` 的 chalk
    /// 语义）；其余情况一律 `open + text + close`，不处理文本内部的 ESC 或换行。
    fn wrap(&self, text: &str) -> String {
        if text.is_empty() && self.empty_returns_empty {
            return String::new();
        }
        format!("{}{text}{}", self.open, self.close)
    }
}

/// `Theme` 的只读快照。
///
/// 视图只用到前景 token 与 `bold`；捕获全部前景 token 也让调用方获得与直接持有 `Theme`
/// 相同的行为。
#[derive(Clone)]
pub(crate) struct ThemeSnapshot {
    /// 前景 token → 捕获的开/关序列；覆盖 `ThemeColor::ALL`。
    fg: Vec<(ThemeColor, AnsiWrap)>,
    /// `Theme::bold` 的捕获序列。
    bold: AnsiWrap,
}

impl ThemeSnapshot {
    /// 从借用的 `Theme` 复制全部前景 token 与 `bold` 的 ANSI 序列。
    ///
    /// 这是本模块唯一读取 `Theme` 的时机：之后视图只用快照渲染，因此这个快照的输出不会
    /// 随之后的主题切换而变化。`Theme` 不是 `Clone`、工厂又只借出 `&Theme`，组件却要
    /// `'static`，所以必须复制而不是持有引用。
    pub(crate) fn capture(theme: &Theme) -> Self {
        ThemeSnapshot {
            fg: ThemeColor::ALL
                .into_iter()
                .map(|color| (color, AnsiWrap::capture(|text| theme.fg(color, text))))
                .collect(),
            bold: AnsiWrap::capture(|text| theme.bold(text)),
        }
    }

    /// 用快照里该 token 的序列包裹 `text`。
    ///
    /// token 不在 `fg` 表中时（按当前 `ThemeColor::ALL` 不会发生）原样返回 `text`，
    /// 因此不会 panic，也不会去读全局主题。
    pub(crate) fn fg(&self, color: ThemeColor, text: &str) -> String {
        match self.fg.iter().find(|(token, _)| *token == color) {
            Some((_, wrap)) => wrap.wrap(text),
            None => text.to_string(),
        }
    }

    /// 用捕获的 `bold` 序列包裹 `text`（空串沿用 `Theme::bold` 的空串语义）。
    pub(crate) fn bold(&self, text: &str) -> String {
        self.bold.wrap(text)
    }
}
