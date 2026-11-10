//! components/text.ts。
//!
//! 多行文本组件：按词换行、左右/上下留白，可选自定义背景着色函数。上层组件
//! （`components/loader.rs`、`kiana-interactive` 的消息组件与 `kiana-extensions` 的
//! JS 绑定）直接持有它。
//!
//! ## 渲染流程
//!
//! 1. 缓存命中（原文与宽度都相同）直接返回上次结果；
//! 2. 文本为空或只含 JS 空白（`trim_js`）时返回**空数组**（不是 `[""]`）；
//! 3. `\t` 展开为 3 个空格；左右留白收窄到 `(width - 1) / 2` 以内，内容宽度至少 1；
//! 4. `wrap_text_with_ansi` 按词换行（保留 ANSI/OSC 8 状态，不做 padding）；
//! 5. 每行加左右留白，再补空格到 `width`（按可见宽度，ANSI 不计宽）；有 `custom_bg_fn`
//!    时由 `apply_background_to_line` 先补齐、再把整行交给它上背景；
//! 6. 上下各加 `padding_y` 行整宽空白（同样可能上背景），拼成结果并缓存。
//!
//! ## 缓存不变量
//!
//! 缓存键只有（原文 `text`、渲染宽度 `width`）；`padding_x`、`padding_y`、
//! `custom_bg_fn` 没有独立的键。因此所有能改变这些输入的 setter 都必须调用
//! `invalidate`（现有 setter 均如此），否则会返回旧布局。`padding_y` 没有 setter，
//! 只能在构造时设定。
//!
//! `flatten_lines` 是 V8 字符串扁平化提示的占位（Rust 侧 no-op），仅为对齐 TS 结构保留。

use std::sync::Arc;

use crate::text::{
    apply_background_to_line, flatten_lines, trim_js, visible_width, wrap_text_with_ansi,
};
use crate::tui::Component;

/// 主题/样式函数（TS `(text: string) => string`）。
///
/// 按 ARCHITECTURE §8.2：`Arc<dyn Fn(&str) -> String + Send + Sync>`（可克隆、可共享）。
pub type StyleFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// 多行文本组件（TS `class Text`）。
///
/// 渲染结果按（原文、宽度）缓存；修改文本、留白或背景函数后必须让缓存失效，
/// 见模块文档的“缓存不变量”。
pub struct Text {
    /// 原始文本（可含 ANSI、制表符与换行，渲染时不做 trim）。
    text: String,
    /// 左右留白列数（渲染时按可用宽度收窄）。
    padding_x: usize,
    /// 上下各追加的整宽空行数（构造后不可改）。
    padding_y: usize,
    /// 自定义背景函数；`None` 时用空格补到整宽。
    custom_bg_fn: Option<StyleFn>,
    /// 上次渲染的原文，与 `cached_width`、`cached_lines` 一起构成缓存。
    cached_text: Option<String>,
    /// 上次渲染的宽度。
    cached_width: Option<usize>,
    /// 上次渲染的完整输出（含上下留白）。
    cached_lines: Option<Vec<String>>,
}

impl Default for Text {
    fn default() -> Self {
        Text::new("")
    }
}

impl Text {
    /// `constructor(text = "", paddingX = 1, paddingY = 1)`。
    ///
    /// 等价于 `Text::with_padding(text, 1, 1)`：四周各留 1 列/行。
    pub fn new(text: impl Into<String>) -> Self {
        Text::with_padding(text, 1, 1)
    }

    /// 指定留白的构造（TS 三参数构造）。
    ///
    /// `padding_x` 是左右各留的列数，渲染时按实际宽度收窄（最多占半屏，见模块文档）；
    /// `padding_y` 是上下各追加的整宽空行数，原样使用、不受宽度影响。
    pub fn with_padding(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        Text::with_background(text, padding_x, padding_y, None)
    }

    /// 带自定义背景着色的构造（TS 四参数构造）。
    ///
    /// `custom_bg_fn` 收到已经补空格到 `width` 的整行，返回值被原样采用：是否添加
    /// 背景色序列与 reset 完全由它决定（见 `crate::text::apply_background_to_line`）。
    pub fn with_background(
        text: impl Into<String>,
        padding_x: usize,
        padding_y: usize,
        custom_bg_fn: Option<StyleFn>,
    ) -> Self {
        Text {
            text: text.into(),
            padding_x,
            padding_y,
            custom_bg_fn,
            cached_text: None,
            cached_width: None,
            cached_lines: None,
        }
    }

    /// `setText(text)`。
    ///
    /// 替换原文并清空渲染缓存。
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.invalidate();
    }

    /// 当前文本（TS 的私有字段，供派生组件读取）。
    ///
    /// 返回未经换行/补白的原文；空文本或纯空白文本在 `render` 中会得到空数组。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// `setCustomBgFn(fn)`。
    ///
    /// 设置 `None` 恢复“空格补齐”的默认行为；同时清空渲染缓存。
    pub fn set_custom_bg_fn(&mut self, custom_bg_fn: Option<StyleFn>) {
        self.custom_bg_fn = custom_bg_fn;
        self.invalidate();
    }

    /// `setPaddingX(paddingX)`。
    ///
    /// 保存配置值并清空缓存；渲染时才按宽度收窄。
    pub fn set_padding_x(&mut self, padding_x: usize) {
        self.padding_x = padding_x;
        self.invalidate();
    }

    /// `paddingX` getter。
    ///
    /// 返回配置值，不是渲染时实际生效的收窄值。
    pub fn padding_x(&self) -> usize {
        self.padding_x
    }
}

impl Component for Text {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 缓存命中：cachedLines && cachedText === text && cachedWidth === width。
        if let (Some(lines), Some(cached_text), Some(cached_width)) =
            (&self.cached_lines, &self.cached_text, self.cached_width)
            && cached_text == &self.text
            && cached_width == width
        {
            return lines.clone();
        }

        // 没有实际文本时什么都不渲染（返回空数组，而不是 [""]）。
        if self.text.is_empty() || trim_js(&self.text).is_empty() {
            self.cached_text = Some(self.text.clone());
            self.cached_width = Some(width);
            self.cached_lines = Some(Vec::new());
            return Vec::new();
        }

        // tab → 3 个空格。
        let normalized_text = self.text.replace('\t', "   ");

        // 宽度不足时收窄左右留白，保证内容与留白放得下。
        let padding_x = self.padding_x.min(width.saturating_sub(1) / 2);
        let content_width = (width.saturating_sub(padding_x * 2)).max(1);

        // 换行宽度至少 1；wrap 保留 ANSI/OSC 8 状态并保证返回至少一行。
        let wrapped_lines = wrap_text_with_ansi(&normalized_text, content_width);

        // 每行两侧加留白；有背景函数时先由 `apply_background_to_line` 按可见宽度补齐到
        // `width`、再把整行交给它上色，否则手动补空格。
        let margin = " ".repeat(padding_x);
        let mut content_lines: Vec<String> = Vec::with_capacity(wrapped_lines.len());
        for line in &wrapped_lines {
            let line_with_margins = format!("{margin}{line}{margin}");
            match &self.custom_bg_fn {
                Some(bg_fn) => content_lines.push(apply_background_to_line(
                    &line_with_margins,
                    width,
                    bg_fn.as_ref(),
                )),
                None => {
                    // 无背景：按可见列宽补空（ANSI 序列不计宽）。
                    let visible_len = visible_width(&line_with_margins);
                    let padding_needed = width.saturating_sub(visible_len);
                    content_lines
                        .push(format!("{line_with_margins}{}", " ".repeat(padding_needed)));
                }
            }
        }

        // 上下留白是整宽空格行；背景函数同样作用于它们（与内容行一致）。
        let empty_line = " ".repeat(width);
        let mut empty_lines: Vec<String> = Vec::with_capacity(self.padding_y);
        for _ in 0..self.padding_y {
            empty_lines.push(match &self.custom_bg_fn {
                Some(bg_fn) => apply_background_to_line(&empty_line, width, bg_fn.as_ref()),
                None => empty_line.clone(),
            });
        }

        // 拼接顺序：上留白、内容、下留白（TS 用数组展开）。
        let mut result: Vec<String> = Vec::new();
        result.extend(empty_lines.iter().cloned());
        result.extend(content_lines);
        result.extend(empty_lines);
        // V8 字符串扁平化提示的占位；Rust 侧是 no-op。
        flatten_lines(&mut result);

        // 写入缓存：键是（text，width）。
        self.cached_text = Some(self.text.clone());
        self.cached_width = Some(width);
        self.cached_lines = Some(result.clone());

        // TS `result.length > 0 ? result : [""]`：兜底返回一个空行。
        if result.is_empty() {
            vec![String::new()]
        } else {
            result
        }
    }

    fn handles_input(&self) -> bool {
        // TS 的 `Text` 没有 `handleInput`，输入不应路由到这里。
        false
    }

    fn invalidate(&mut self) {
        // 命中要求三个字段都是 Some；任一项清空即可失效，这里统一清空以保持一致。
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
    }
}
