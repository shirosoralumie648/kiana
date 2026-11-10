//! extensions/codemode/renderer.ts：`codemode` 工具的呈现。
//!
//! 调用显示脚本；结果列出嵌套工具调用的状态与 `models.*` 调用的成本，随后是去掉
//! "Script completed" 头部的脚本输出，以及脚本运行中的用时（`Elapsed`）或总用时（`Took`），
//! 与 bash 行一致。嵌套调用不是单独的工具行，因为它们从不作为工具调用到达模型。
//!
//! # Rust 形态
//!
//! `ToolRenderContext.last_component` 在 Rust 里是 `Box<dyn Component>`（没有向下转型），
//! 因此每次都新建 `Container`（与 `mcp/tools.rs` 渲染器相同的处理）；复用只是性能优化，输出不受影响。
//!
//! # 渲染流程
//!
//! [`codemode_renderers`] 返回的闭包由 `codemode/tool.rs` 的 `create_codemode_tool_definition`
//! 装进 `ToolDefinition`；TUI 用 `render_call` 画调用、用 `render_result` 画结果。
//! 两个闭包都要求 `Send + Sync`，不能借用外部可变状态：需要跨帧保留的计时信息写
//! `ToolRenderContext.state`，其余组件每次重建。
//!
//! `render_call`：
//!
//! 1. `start_tool_timer` 在 `context.execution_started` 为真后的首次渲染记录 `startedAt`
//!    （同一调用只记一次）；执行开始前的渲染（排队/尚未开始）不写状态；
//! 2. 从参数取 `code`；缺失或 `null` 当空串，类型不对显示 `[invalid arg]`；
//! 3. 删 `\r`、去掉末尾空白、把制表符换成空格，再按 JavaScript 语法高亮；
//! 4. 展开时显示全部高亮行，否则保留前 `CODE_PREVIEW_LINES` 个可视行并附展开提示。
//!
//! `render_result`：
//!
//! 1. 从 `result.details` 反序列化 `CodemodeToolDetails`；失败按空 details 处理；
//! 2. 渲染嵌套调用：折叠时只保留最后 `CALL_PREVIEW_COUNT` 个，隐藏部分用一行
//!    "N earlier calls" 摘要代替；带成本（`cost` 为 `Some`、非 0、非 NaN）的调用超过
//!    一个时，按**全部**调用求和并追加 "Model calls" 行；
//! 3. 若第一块内容整块匹配 "Script completed|failed / Wall time … / Output:" 头部就丢掉它；
//!    流式结果（`is_partial`）不显示正文；
//! 4. 正文按 `context.is_error` 着色；展开显示全文，折叠裁到 `OUTPUT_PREVIEW_LINES`
//!    个可视行，并在正文非空时追加上 `fullOutputPath`（完整输出文件）；
//! 5. 最后追加计时行（`Elapsed` 或 `Took`），它不依赖正文是否为空。
//!
//! # 数值与字符串的 JS 保真
//!
//! 渲染文本是当前稳定契约（快照与 golden 值均按 ECMAScript 语义生成），因此数字与截断都按 ECMAScript 语义实现：
//! `js_round`（`Math.round`）、`js_to_precision`（`Number#toPrecision`）、`js_trim_end` /
//! `is_js_whitespace`（`trimEnd` 的 `\s` 集合）、`utf16_len` / `utf16_slice`（JS 字符串按
//! UTF-16 code unit 计长与截断）。这些不是性能优化；改动前对照 `tests/codemode_renderer.rs`
//! 的渲染快照（成本、截断、耗时等渲染输出）与本文件 `to_precision_matches_javascript_golden_values`
//! （Node 26 生成）的数值 golden——后者才锁定 `js_round` / `js_to_precision` / `trimEnd` 的逐例结果。

use std::sync::LazyLock;

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::{JsonValue, UserContentBlock};
use kiana_ext_api::tool_types::{
    RenderCallFn, RenderResultFn, ToolRenderContext, ToolRenderResultOptions, ToolRenderers,
};
use kiana_theme::theme::Theme;
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::{Component, Container};
use kiana_ui_kit::keybinding_hints::key_hint;
use kiana_ui_kit::visual_truncate::{Keep, VisualLinePreview, VisualLinePreviewOptions};
use regex::Regex;

use crate::codemode::tool::{CodemodeNestedCall, CodemodeNestedCallStatus, CodemodeToolDetails};

/// 脚本预览的胶囊行数。
const CODE_PREVIEW_LINES: usize = 10;
/// 折叠时显示的调用数（取最后 N 个）。
const CALL_PREVIEW_COUNT: usize = 8;
/// 折叠输出的可视行数。
const OUTPUT_PREVIEW_LINES: usize = 5;
/// 折叠时参数的最大字符数（按 UTF-16 code unit 计，见 [`utf16_len`]）。
const COLLAPSED_ARGS_CHARS: usize = 80;
/// 结果头部的正则。
///
/// 整块匹配 `Script completed|failed`、`Wall time <数字> seconds`、`Output:` 三行（最后
/// 带换行）。wall time 的位数不固定，所以用 `[\d.]+`；`^`/`$` 要求整块文本就是头部，
/// 而不是普通输出中包含这几行，避免误删。
static SCRIPT_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Script (completed|failed)\nWall time [\d.]+ seconds\nOutput:\n$")
        .expect("valid script header regex")
});

/// JS `String.prototype.trimEnd` 的空白集合。
///
/// 从尾部逐字符检查，返回去掉尾随空白的切片；全空白输入返回空串。调用方在语法高亮前
/// 使用它，避免脚本末尾的换行/空白多出一行预览。
fn js_trim_end(text: &str) -> &str {
    let mut end = text.len();
    for (index, character) in text.char_indices().rev() {
        if is_js_whitespace(character) {
            end = index;
        } else {
            break;
        }
    }
    &text[..end]
}

/// JS 正则 `\s` 的字符集合（ECMA-262）。
///
/// 逐项列出（含 BOM U+FEFF 与行分隔符 U+2028/U+2029），必须与 ECMAScript 的 `\s` 集合一致：
/// 换成其它空白判定会改变 `trimEnd` 的截断位置，进而改变预览输出。
fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000a}'
            | '\u{000b}'
            | '\u{000c}'
            | '\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `theme.fg(color, text)` 的 ANSI 前缀/后缀（用于把 `formatHint` 变成 `'static` 闭包）。
///
/// `VisualLinePreviewOptions::format_hint` 是 `Box<dyn Fn(usize) -> String>`（隐含
/// `'static`），不能借用 `&Theme`。这里用一个不会出现在正常文本里的探测字符 U+0001，
/// 把一次 `fg` 调用拆成前缀与后缀，之后在闭包里拼出同样的着色。探测失败（`fg` 的输出
/// 不含该字符，正常实现不会发生）时返回两个空串，提示行退化为无色文本。
fn ansi_parts(theme: &Theme, color: ThemeColor) -> (String, String) {
    const MARKER: char = '\u{0001}';
    let probe = theme.fg(color, "\u{0001}");
    match probe.split_once(MARKER) {
        Some((prefix, suffix)) => (prefix.to_string(), suffix.to_string()),
        None => (String::new(), String::new()),
    }
}

/// `formatDuration(ms)`。
///
/// `None` 返回空串（调用方据此省略耗时）。小于 1000ms 用毫秒并四舍五入；否则换算成秒、
/// 保留一位小数（`toFixed(1)`）。这里不校验 `NaN`/负值。
fn format_duration(ms: Option<f64>) -> String {
    let Some(ms) = ms else {
        return String::new();
    };
    if ms < 1000.0 {
        format!("{}ms", js_round(ms))
    } else {
        format!("{}s", kiana_ai::model_catalog::js_to_fixed(ms / 1000.0, 1))
    }
}

/// JS `Math.round`：半数向 +∞ 取整。
///
/// 结果再经 `format_js_number` 转成 JS 数字文本（整数不带 `.0`），返回 `String` 而不是
/// 数值，避免调用方再做一次格式化。`-0` 归一成 `0`：时长非负，且 JS 模板字符串也会把
/// `-0` 写成 `"0"`。
fn js_round(value: f64) -> String {
    let rounded = (value + 0.5).floor();
    // `Math.round(0.5) === 1`、`Math.round(-0.5) === -0`；时长非负，-0 写成 "0"。
    kiana_json::js_num::format_js_number(if rounded == 0.0 { 0.0 } else { rounded })
}

/// 较大的金额用美分，分类器调用的不足一美分部分用两位有效数字。
///
/// `>= 0.01` 走 `toFixed(2)`（两位小数，即分）；小于一分钱走 `toPrecision(2)`，因为按两位
/// 小数会把小额成本显示成 `$0.00`。负值也走有效数字分支（`$` 后带负号）。
fn format_cost(cost: f64) -> String {
    if cost >= 0.01 {
        format!("${}", kiana_ai::model_catalog::js_to_fixed(cost, 2))
    } else {
        format!("${}", js_to_precision(cost, 2))
    }
}

/// 十进制文本里第一个非零数字的指数（`"0.000013"` → -5，`"10.0"` → 1）。
///
/// 只在 `js_to_precision` 内部（经由 `decimals_after_rounding`）使用：从 `toFixed` 的结果
/// 反推舍入后的数量级，绕开 `log10` 在 10 的幂附近的浮点误差。全零文本返回 0。
fn decimal_exponent(text: &str) -> i32 {
    let (integer_part, fraction_part) = text.split_once('.').unwrap_or((text, ""));
    if let Some(position) = integer_part.find(|character| character != '0') {
        return integer_part.len() as i32 - 1 - position as i32;
    }
    if let Some(position) = fraction_part.find(|character| character != '0') {
        return -(position as i32) - 1;
    }
    0
}

/// JS `Number.prototype.toPrecision(2)`（只用于 `formatCost`）。
///
/// ECMA-262：先把有效数字四舍五入到 `precision` 位，再按**舍入后**的指数选择定点/指数形式
/// （`e < -6` 或 `e >= precision` 用指数形式）。因此 `9.99e-7.toPrecision(2)` 是
/// `"0.0000010"` 而不是 `"1.0e-6"`。
///
/// 生产代码里只有 `format_cost` 调用它，且固定 `precision = 2`。非有限值、0、次正规数与
/// 正常范围各有单独分支；正常范围在舍入后用十进制文本重算指数，避免浮点误差改变输出
/// 形态。
fn js_to_precision(value: f64, precision: u32) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value < 0.0 {
            "-Infinity".to_string()
        } else {
            "Infinity".to_string()
        };
    }
    if value == 0.0 {
        let zeros = "0".repeat(precision.saturating_sub(1) as usize);
        return format!("0.{zeros}");
    }
    let negative = value < 0.0;
    let magnitude = value.abs();
    let digits = precision as i32;
    // 次正规数（|x| < 2.2250738585072014e-308）：`10f64.powi(exponent)` 在这里下溢为 0，
    // 尾数计算会得到 Infinity。次正规数的指数恒 < -6，ECMA 一定选指数形式，而 Rust 的
    // `{:.*e}` 对 f64 做正确舍入的十进制展开；这个量级上两位有效数字的十进制中点需要分母
    // 含 5 的高次幂，double 不可能恰好命中，所以“半数向偶数”与 ECMA 的“半数远离零”一致
    // （`formatCost` 只用 precision=2；已用 604 个随机次正规数与 Node 对照验证，下方测试
    // 保留有代表性的样例）。
    if magnitude < f64::MIN_POSITIVE {
        let formatted = format!("{:.*e}", precision.saturating_sub(1) as usize, magnitude);
        let (mantissa, exponent) = formatted
            .split_once('e')
            .expect("Rust scientific formatting always has an exponent");
        let exponent: i32 = exponent.parse().expect("decimal exponent");
        let sign = if exponent >= 0 { '+' } else { '-' };
        let body = format!("{mantissa}e{sign}{}", exponent.abs());
        return if negative { format!("-{body}") } else { body };
    }
    // 初始指数：10^(p-1) <= mantissa < 10^p。
    let mut exponent = magnitude.log10().floor() as i32;
    if magnitude / 10f64.powi(exponent) >= 10.0 {
        exponent += 1;
    } else if magnitude / 10f64.powi(exponent) < 1.0 {
        exponent -= 1;
    }
    // 舍入可能把数值推到下一个数量级（9.99 → 10.0），指数按舍入后的文本重算。
    let initial_fraction = (digits - 1 - exponent).max(0) as u32;
    let rounded = kiana_ai::model_catalog::js_to_fixed(magnitude, initial_fraction);
    exponent = decimals_after_rounding(&rounded, exponent);
    let body = if exponent < -6 || exponent >= digits {
        let mut exponent = exponent;
        let mut mantissa = magnitude / 10f64.powi(exponent);
        let mut rounded = kiana_ai::model_catalog::js_to_fixed(mantissa, precision - 1);
        // 尾数本身舍入到 10.0 时再进一位。
        if rounded
            .parse::<f64>()
            .is_ok_and(|value| value >= 10f64.powi(digits - 1) * 10.0)
        {
            exponent += 1;
            mantissa /= 10.0;
            rounded = kiana_ai::model_catalog::js_to_fixed(mantissa, precision - 1);
        }
        let sign = if exponent >= 0 { '+' } else { '-' };
        format!("{rounded}e{sign}{}", exponent.abs())
    } else {
        let fraction = (digits - 1 - exponent).max(0) as u32;
        kiana_ai::model_catalog::js_to_fixed(magnitude, fraction)
    };
    if negative { format!("-{body}") } else { body }
}

/// 舍入后的指数：优先用十进制文本判断（`log10` 在 10 的幂上可能有浮点误差）。
///
/// 文本解析失败或值为 0 时退回调用方给的 `fallback` 指数。
fn decimals_after_rounding(rounded: &str, fallback: i32) -> i32 {
    if rounded.parse::<f64>().is_ok_and(|value| value > 0.0) {
        decimal_exponent(rounded)
    } else {
        fallback
    }
}

/// `statusIcon(call, theme)`。
fn status_icon(call: &CodemodeNestedCall, theme: &Theme) -> String {
    match call.status {
        CodemodeNestedCallStatus::Running => theme.fg(ThemeColor::Warning, "…"),
        CodemodeNestedCallStatus::Ok => theme.fg(ThemeColor::Success, "✓"),
        CodemodeNestedCallStatus::Error => theme.fg(ThemeColor::Error, "✗"),
        CodemodeNestedCallStatus::Cancelled => theme.fg(ThemeColor::Muted, "⊘"),
    }
}

/// `formatCall(call, theme, expanded)`。
///
/// 一行显示「状态图标 + 工具名 + 参数 + 耗时 + 成本」：
///
/// - 折叠时参数超过 [`COLLAPSED_ARGS_CHARS`] 个 UTF-16 code unit 就截断：保留前
///   `COLLAPSED_ARGS_CHARS - 3` 个 code unit，再补 `...`；
/// - 成本只在 `Some`、非 0、非 NaN 时显示（未报告 usage 的调用不显示 `$0.00`）；
/// - 展开时若有错误，错误文本另起一行并缩进 4 个空格，内部换行也同样缩进。
///
/// 输入来自 `result.details.calls`，这里只读不写，不改变调用状态。
fn format_call(call: &CodemodeNestedCall, theme: &Theme, expanded: bool) -> String {
    let args = if !expanded && utf16_len(&call.args) > COLLAPSED_ARGS_CHARS {
        format!(
            "{}...",
            utf16_slice(&call.args, 0, COLLAPSED_ARGS_CHARS - 3)
        )
    } else {
        call.args.clone()
    };
    let duration = format_duration(call.duration_ms);
    let mut line = format!(
        "{} {}",
        status_icon(call, theme),
        theme.fg(ThemeColor::ToolTitle, &call.name)
    );
    if !args.is_empty() {
        line.push(' ');
        line.push_str(&theme.fg(ThemeColor::Muted, &args));
    }
    if !duration.is_empty() {
        line.push(' ');
        line.push_str(&theme.fg(ThemeColor::Dim, &duration));
    }
    if let Some(cost) = call.cost
        && cost != 0.0
        && !cost.is_nan()
    {
        line.push(' ');
        line.push_str(&theme.fg(ThemeColor::Dim, &format_cost(cost)));
    }
    if expanded && let Some(error) = &call.error {
        line.push_str("\n    ");
        line.push_str(&theme.fg(
            ThemeColor::Error,
            &error.split('\n').collect::<Vec<_>>().join("\n    "),
        ));
    }
    line
}

/// UTF-16 code unit 数。
///
/// JS 字符串的 `length` 按 UTF-16 计；参数截断阈值也用同一单位，不能换成 `char` 数，
/// 否则含非 BMP 字符的参数显示长度会与 TS 不同。
fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// 按 UTF-16 code unit 截取。
///
/// 边界按 JS `String#slice` 的方式钳制：`start`/`end` 超过长度时取长度，`end < start`
/// 时返回空串。截断可能切开代理对，`from_utf16_lossy` 会把孤立代理替换成 U+FFFD。
fn utf16_slice(value: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    let start = start.min(units.len());
    let end = end.min(units.len()).max(start);
    String::from_utf16_lossy(&units[start..end])
}

/// 从 `result.details` 读 [`CodemodeToolDetails`]。
///
/// 解析失败（`details` 为 `null`、字段类型不符等）时返回 `Default`：渲染路径没有错误
/// 返回，不能因为 details 形状异常让整个工具行渲染失败。
fn details_of(result: &AgentToolResult<JsonValue>) -> CodemodeToolDetails {
    serde_json::from_value(result.details.clone()).unwrap_or_default()
}

/// `codemode` 的渲染器。
///
/// 返回 `render_call` + `render_result`，`render_shell` 为 `None`（不请求工具自绘外壳，
/// 沿用默认外壳）。两个闭包不读取设置、不访问文件系统；但语法高亮、键位提示与图片占位
/// 会读取进程级/线程级全局状态（当前主题、键位表、终端能力缓存），并非全部信息都来自
/// 调用参数、工具结果与 [`ToolRenderContext`]。
///
/// 调用方是 `codemode/tool.rs` 的 `create_codemode_tool_definition`，它把返回值装进
/// `ToolDefinition`；因此每次创建工具定义都会新建一对闭包，闭包之间的状态只通过
/// `ToolRenderContext.state` 共享。
pub fn codemode_renderers() -> ToolRenderers {
    let render_call: RenderCallFn = std::sync::Arc::new(
        |args: &JsonValue, theme: &Theme, context: &ToolRenderContext| -> Box<dyn Component> {
            kiana_tools::render_utils::start_tool_timer(context);
            // 脚本包含 `// @options:` 行，因此选项随脚本显示。
            let code = kiana_tools::render_utils::str(args.get("code"));
            let title = theme.fg(ThemeColor::ToolTitle, &theme.bold("codemode"));
            let mut component = Container::new();
            match code {
                None => {
                    component.add_child(Box::new(Text::with_padding(
                        format!("{title} {}", theme.fg(ThemeColor::Error, "[invalid arg]")),
                        0,
                        0,
                    )));
                    return Box::new(component);
                }
                Some(code) => {
                    component.add_child(Box::new(Text::with_padding(title, 0, 0)));
                    if code.is_empty() {
                        return Box::new(component);
                    }
                    // 先去掉 CR（源码可能来自 Windows 换行），再去尾部空白；否则末尾换行
                    // 会在折叠预览里多占一行。制表符先替换成三个空格，让缩进宽度固定。
                    let without_cr = code.replace('\r', "");
                    let cleaned = js_trim_end(&without_cr);
                    let highlighted = kiana_theme::highlight_code(
                        &kiana_tools::render_utils::replace_tabs(cleaned),
                        Some("javascript"),
                    )
                    .join("\n");
                    // 展开显示全部高亮行；折叠保留开头 N 个可视行（Keep::Start 把提示行放在
                    // 保留段之后），行数按渲染宽度换行后的可视行计算，而不是源码逻辑行。
                    // 这里读 context.expanded；render_result 对应的是 options.expanded。
                    if context.expanded {
                        component.add_child(Box::new(Text::with_padding(highlighted, 0, 0)));
                    } else {
                        let (muted_open, muted_close) = ansi_parts(theme, ThemeColor::Muted);
                        let hint = key_hint("app.tools.expand", "to expand");
                        component.add_child(Box::new(VisualLinePreview::new(
                            VisualLinePreviewOptions {
                                text: highlighted,
                                max_visual_lines: CODE_PREVIEW_LINES,
                                keep: Keep::Start,
                                format_hint: Box::new(move |hidden| {
                                    format!(
                                        "{muted_open}... ({hidden} more lines,{muted_close} {hint}{muted_open}){muted_close}"
                                    )
                                }),
                            },
                        )));
                    }
                }
            }
            Box::new(component)
        },
    );
    let render_result: RenderResultFn = std::sync::Arc::new(
        |result: &AgentToolResult<JsonValue>,
         options: &ToolRenderResultOptions,
         theme: &Theme,
         context: &ToolRenderContext|
         -> Box<dyn Component> {
            let timer_text =
                kiana_tools::render_utils::tool_timer_text(context, options.is_partial);
            let mut component = Container::new();
            let details = details_of(result);
            let calls = details.calls;
            if !calls.is_empty() {
                // 折叠时只显示最后 CALL_PREVIEW_COUNT 个；saturating_sub 处理调用数不足的
                // 情况（此时 shown 就是全部 calls）。
                let shown: &[CodemodeNestedCall] = if options.expanded {
                    &calls
                } else {
                    &calls[calls.len().saturating_sub(CALL_PREVIEW_COUNT)..]
                };
                let mut lines: Vec<String> = shown
                    .iter()
                    .map(|call| format_call(call, theme, options.expanded))
                    .collect();
                if shown.len() < calls.len() {
                    lines.insert(
                        0,
                        format!(
                            "{} {}{}",
                            theme.fg(
                                ThemeColor::Muted,
                                &format!("... ({} earlier calls,", calls.len() - shown.len())
                            ),
                            key_hint("app.tools.expand", "to expand"),
                            theme.fg(ThemeColor::Muted, ")")
                        ),
                    );
                }
                // 合计按全部 calls（含折叠隐藏的调用）计算，而不是只算 shown；与逐行显示的
                // 判断一致，只统计非 0、非 NaN 的成本。
                let priced: Vec<&CodemodeNestedCall> = calls
                    .iter()
                    .filter(|call| call.cost.is_some_and(|cost| cost != 0.0 && !cost.is_nan()))
                    .collect();
                if priced.len() > 1 {
                    let total: f64 = priced.iter().map(|call| call.cost.unwrap_or(0.0)).sum();
                    lines.push(theme.fg(
                        ThemeColor::Muted,
                        &format!("Model calls: {}", format_cost(total)),
                    ));
                }
                component.add_child(Box::new(Spacer::new()));
                component.add_child(Box::new(Text::with_padding(lines.join("\n"), 0, 0)));
            }

            // 丢弃 "Script completed\nWall time ...\nOutput:\n" 头部；被拒绝的输入（非法
            // options）没有头部。
            let has_header = result.content.first().is_some_and(|first| match first {
                UserContentBlock::Text(text) => SCRIPT_HEADER.is_match(&text.text),
                UserContentBlock::Image(_) => false,
            });
            // 流式阶段正文固定为空，只显示调用列表与计时；最终结果才拼接正文。
            let output = if options.is_partial {
                String::new()
            } else {
                let mut without_header = result.clone();
                if has_header {
                    without_header.content.remove(0);
                }
                // js_trim 按 JS 空白集合去掉首尾空白；空输出不占用屏幕空间。
                kiana_json::js_string::js_trim(&kiana_tools::render_utils::get_text_output(
                    Some(&without_header),
                    context.show_images,
                ))
                .to_string()
            };
            if !output.is_empty() {
                let color = if context.is_error {
                    ThemeColor::Error
                } else {
                    ThemeColor::ToolOutput
                };
                let styled = kiana_tools::render_utils::replace_tabs(&output)
                    .split('\n')
                    .map(|line| theme.fg(color, line))
                    .collect::<Vec<_>>()
                    .join("\n");
                component.add_child(Box::new(Spacer::new()));
                if options.expanded {
                    component.add_child(Box::new(Text::with_padding(styled, 0, 0)));
                } else {
                    // 限制换行后的可视行，而不是逻辑行：脚本输出常常是一整行 JSON。
                    let (muted_open, muted_close) = ansi_parts(theme, ThemeColor::Muted);
                    let hint = key_hint("app.tools.expand", "to expand");
                    component.add_child(Box::new(VisualLinePreview::new(
                        VisualLinePreviewOptions {
                            text: styled,
                            max_visual_lines: OUTPUT_PREVIEW_LINES,
                            keep: Keep::Start,
                            format_hint: Box::new(move |hidden| {
                                format!(
                                    "{muted_open}... ({hidden} more lines,{muted_close} {hint}{muted_open}){muted_close}"
                                )
                            }),
                        },
                    )));
                    // 折叠预览会隐藏末尾的截断提示，因此在这里给出文件名（只在折叠且正文
                    // 非空时显示）。
                    if let Some(full_output_path) = &details.full_output_path {
                        component.add_child(Box::new(Text::with_padding(
                            theme.fg(
                                ThemeColor::Muted,
                                &format!("Full output: {full_output_path}"),
                            ),
                            0,
                            0,
                        )));
                    }
                }
            }
            // 计时行独立于正文：没有输出也会显示 `Elapsed`（运行中）或 `Took`（已结束）。
            if let Some(timer_text) = timer_text {
                component.add_child(Box::new(Spacer::new()));
                component.add_child(Box::new(Text::with_padding(
                    theme.fg(ThemeColor::Muted, &timer_text),
                    0,
                    0,
                )));
            }
            Box::new(component)
        },
    );
    ToolRenderers {
        render_shell: None,
        render_call: Some(render_call),
        render_result: Some(render_result),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_precision_matches_javascript_golden_values() {
        // 由 TS 的 `Number.prototype.toPrecision(2)`（Node 26）生成。
        let cases = [
            (0.000012936, "0.000013"),
            (0.009999999, "0.010"),
            (0.00001, "0.000010"),
            (0.000001, "0.0000010"),
            (9.99e-7, "0.0000010"),
            (1e-7, "1.0e-7"),
            (1.2936e-7, "1.3e-7"),
            (0.001, "0.0010"),
            (0.00999, "0.010"),
            (0.0000009999999, "0.0000010"),
            (0.000123456, "0.00012"),
            (0.000099999, "0.00010"),
            (1.234e-10, "1.2e-10"),
            (99.9, "1.0e+2"),
            (9.99, "10"),
            // 次正规数：10^exponent 下溢，不能走正常的尾数计算。以下由 Node 26 生成
            // （含最小次正规数、其整数倍、最大次正规数与 MIN_POSITIVE 邻域）。
            (5e-324, "4.9e-324"),
            (1e-323, "9.9e-324"),
            (1e-320, "1.0e-320"),
            (3.5e-310, "3.5e-310"),
            (1.5e-323, "1.5e-323"),
            (5e-323, "4.9e-323"),
            (1.26e-321, "1.3e-321"),
            (5.180_65e-318, "5.2e-318"),
            (1.112_536_929_253_6e-308, "1.1e-308"),
            (2.225_073_858_507_201e-308, "2.2e-308"),
            (2.225073858507201e-308, "2.2e-308"),
            (1.582_274_743_827_34e-309, "1.6e-309"),
            (1.935_455_243_578_089e-308, "1.9e-308"),
            (4.450_147_717_014_402_3e-308, "4.5e-308"),
        ];
        for (value, expected) in cases {
            assert_eq!(js_to_precision(value, 2), expected, "value {value}");
        }
        assert_eq!(js_to_precision(0.0, 2), "0.0");
        assert_eq!(js_to_precision(-5e-324, 2), "-4.9e-324");
        assert_eq!(format_cost(5e-324), "$4.9e-324");
        assert_eq!(format_cost(-5e-324), "$-4.9e-324");
    }
}
