//! `terminal-colors.ts` + `colors.ts` 的类型契约。
//!
//! `colors.ts` 的色值模型（[`Color`]）是 `kiana-theme`（主题 token）、`kiana-tools`（diff 着色）、
//! `kiana-interactive`（组件）与 `kiana-tui`（渲染）之间的公共语言，必须在骨架阶段固定。
//!
//! 实现落在本文件；`colors.rs` 只放函数（解析、OKLAB/OKHSL 变换、ANSI 序列拼接），
//! 避免两处各写一份类型。
//!
//! ## 本文件包含什么
//!
//! 1. **跨 crate 的数据契约**：`Color` / `TextStyle` / `TerminalColors` 等类型派生了
//!    `Serialize` / `Deserialize`，字段名与 `serde` 属性就是 JSON 形状，修改它们会改变
//!    下游看到的线格式。
//! 2. **终端抽象**：[`Terminal`] trait。渲染层只依赖它；真实实现是
//!    `crate::terminal::ProcessTerminal`，测试替身见 `crates/kiana-tui/tests/common/mod.rs`。
//! 3. **终端回复解析**：OSC 10 / 11 / 4 颜色回复与 `ESC[?997;n` 明暗方案上报。颜色数学与
//!    ANSI 编码不在这里，见 `crate::colors` 与 `crate::oklab`。
//!
//! ## 序列化约定（改动前先读）
//!
//! - [`Color`] 用内部标签 `kind`，JSON 形如 `{"kind":"indexed","index":3}` /
//!   `{"kind":"rgb","r":…,"g":…,"b":…}` / `{"kind":"oklch","l":…,"c":…,"h":…}`；
//!   单元测试 `color_serializes_with_kind_tag` 锁定这一形状。
//! - [`TerminalColorMode::Palette256`] 的线上拼写是 `"256color"`（显式 `#[serde(rename)]`，
//!   测试 `color_mode_matches_ts_spelling` 锁定），不能按枚举名推断成 `palette256`。
//! - 带 `#[serde(skip_serializing_if = "Option::is_none")]` 的可选字段缺席即 `None`。
//! - [`TextStyle`] 用 `#[serde(flatten)]` 把 [`TextAttributes`] 展开到同一层，因此 JSON 里
//!   `bold` / `italic` 等与 `fg` / `bg` 平级。
//! - [`OscColorTarget`] 标注了 `#[serde(untagged)]`：没有判别字段，`Palette(u16)` 直接表示
//!   为数字。
//!
//! ## 解析入口的返回约定
//!
//! - [`parse_osc_color_response`]：不是 OSC 颜色回复返回 `None`；是回复但色值解析失败返回
//!   `Some((target, None))`。调用方必须区分两者——前者继续当普通输入处理，后者说明终端
//!   答了但答案不可用。
//! - [`parse_terminal_color_scheme_report`]：要求整串恰好由一到多个 `ESC[?997;1n` /
//!   `ESC[?997;2n` 组成，返回**最后**一个；任何多余字节都返回 `None`。
//!
//! ## 注意事项
//!
//! - 解析函数都不做 I/O，输入是完整的回复字符串；生产路径上半截回复的拼接由
//!   `crate::stdin::StdinBuffer` 负责，调用链见 `crate::tui` 的颜色查询。
//! - `hex_to_rgb` 等辅助函数按 V8 的语义实现（`parseInt` 遇非法字符即停、按位宽归一化、
//!   双精度舍入），细节见各自文档，修改前先读。

use serde::{Deserialize, Serialize};

/// sRGB 颜色，通道 0-255（TS `RgbColor`）。
///
/// 这是“已经落到 8 位”的颜色：终端回复解析与需要 `u8` 通道的接口用它；带分数的色彩运算
/// 用 [`Color::Rgb`]。默认值（`Default`）是黑色 `(0, 0, 0)`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// 终端明暗方案（TS `TerminalColorScheme`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalColorScheme {
    /// 深色背景（终端上报 `ESC[?997;1n`）。
    Dark,
    /// 浅色背景（终端上报 `ESC[?997;2n`）。
    Light,
}

/// 终端上报的当前配色（TS `TerminalColors`）。
///
/// 三个字段互相独立：终端可能只回答其中一部分，未回答的保持 `None`。`Default`（全
/// `None`）表示“什么都不知道”，而不是“黑色”。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalColors {
    /// 前景色（OSC 10 回复）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground: Option<RgbColor>,
    /// 背景色（OSC 11 回复）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<RgbColor>,
    /// ANSI 0-15；终端只报告全部 16 个时才写入。
    ///
    /// 组装点在 `crate::tui` 的颜色查询结果：16 个槽位全部收到才产出 `Some`，否则为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palette: Option<Vec<RgbColor>>,
}

/// OSC 颜色回复的目标（OSC 10 / OSC 11 / OSC 4 的索引）。
///
/// `#[serde(untagged)]`：没有判别字段，`Palette(u16)` 直接表示为数字。本仓库内它只作为
/// [`parse_osc_color_response`] 的返回值使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OscColorTarget {
    /// OSC 10：前景色。
    Foreground,
    /// OSC 11：背景色。
    Background,
    /// OSC 4：调色板索引。解析只接受 1-3 位十进制数，因此这里的取值是 0-999。
    Palette(u16),
}

/// 256 色索引（TS 的 `IndexedColor` 对象）。
///
/// 表示同一个 256 色索引；`kind` 字段固定为 `"indexed"`，因此序列化后与
/// [`Color::Indexed`] 的形状相同（`{"kind":"indexed","index":N}`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedColor {
    pub kind: IndexedColorKind,
    pub index: u8,
}

/// `IndexedColor.kind` 的字面量。
///
/// 只有一个取值，`#[default]` 让 `IndexedColorKind::default()` 可用；
/// `#[serde(rename_all = "lowercase")]` 把它写成 `"indexed"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexedColorKind {
    #[default]
    Indexed,
}

/// RGB 色值（`kind: "rgb"`）。
///
/// 通道是 `f64`，与 [`Color::Rgb`] 一样允许分数与越界值；本类型只描述 JSON 形状，
/// 不在这里做取整或夹取。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RgbColorValue {
    pub kind: RgbKind,
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

/// `RgbColorValue.kind` 的字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RgbKind {
    #[default]
    Rgb,
}

/// OKLCH 色值（`kind: "oklch"`）。
///
/// 与 [`Color::Oklch`] 的语义相同；`h` 是角度，`c` 的常见范围约 0-0.4，`l` 0-1。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OklchColorValue {
    pub kind: OklchKind,
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

/// `OklchColorValue.kind` 的字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OklchKind {
    #[default]
    Oklch,
}

/// 具体颜色（TS `Color`）。三种形态以 `kind` 判别；任何颜色都能转成 sRGB，色彩运算不会失败。
///
/// - `Indexed`：256 色表索引，ANSI 编码为 `38;5;n` / `48;5;n`（不经过颜色模式）；
/// - `Rgb`：sRGB 通道，`f64` 允许分数与越界值（取整/夹取的时机由调用方决定）；
/// - `Oklch`：OKLCH，`h` 是角度。
///
/// 数值语义与取整规则见 `crate::colors` 的模块文档；本类型只负责携带数据与 JSON 形状。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Color {
    /// 调色板索引（0-255）。
    Indexed { index: u8 },
    /// sRGB 通道（`f64`，可带分数）。
    Rgb { r: f64, g: f64, b: f64 },
    /// OKLCH 三元组。
    Oklch { l: f64, c: f64, h: f64 },
}

/// 终端颜色能力（TS `TerminalColorMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalColorMode {
    /// 256 色模式；线上拼写是 `"256color"`。
    #[serde(rename = "256color")]
    Palette256,
    /// 24 位真彩色。
    Truecolor,
}

/// 混色空间（TS `ColorMixSpace`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorMixSpace {
    /// 在 OKLCH 空间按 L/C/H 插值（色相走最短路径，见 `crate::colors::mix_colors`）。
    Oklch,
    /// 在 sRGB 空间按通道线性插值（中间结果保留分数）。
    Srgb,
}

/// OKLCH 通道（h 为角度，c 0-~0.4，l 0-1）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct OklchChannels {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

/// OKHSL 通道（h 角度，s/l 0-1；s 相对该色相与明度下 sRGB 色域能给出的最大值）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct OkhslChannels {
    pub h: f64,
    pub s: f64,
    pub l: f64,
}

/// 文本属性（TS `TextAttributes`）。
///
/// 每个字段都是 `Option<bool>`：`None` 表示“未指定”（是否继承或默认由调用方决定），
/// 只有 `Some(true)` 才表示开启。文本属性判定只认 `Some(true)`；颜色不属于本结构，
/// 是否编码由 [`TextStyle`] 的 `fg` / `bg` 是否为 `Some(Color)` 决定（`style_text_with_ansi`
/// 先用 `map` 生成前景/背景片段，`style_text_with_parts` 再按片段是否为 `Some` 拼接，
/// 见 `crate::colors`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextAttributes {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dim: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inverse: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strikethrough: Option<bool>,
}

/// 文本样式（TS `TextStyle`）。
///
/// 注意：`Color` 在本文件里是普通枚举（不是 trait 对象），因此本结构可以 `Copy`。
///
/// `#[serde(flatten)]` 把 [`TextAttributes`] 的六个可选字段展开到同一层，所以 JSON 中没有
/// 嵌套的 `attributes` 对象：`bold` / `italic` 等与 `fg` / `bg` 平级。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    #[serde(flatten)]
    pub attributes: TextAttributes,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<Color>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<Color>,
}

/// 程序状态（OSC 7501；TS `ProgramStatus`）。
///
/// 编码由 `crate::program_status::format_program_status` 负责：`state` 必填，其它字段
/// 缺省就不写进序列。不变量：`kind` 只在 `state == Blocked` 时有意义（编码时也只在该
/// 状态写出）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramStatus {
    /// 当前状态；`Clear` 用于移除终端上的状态显示。
    pub state: ProgramStatusState,
    /// `[A-Za-z0-9_.+-]{1,32}`；其它值被忽略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// 阻塞原因；非 `Blocked` 状态时即使有值也不会编码。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProgramStatusKind>,
    /// 一行人类可读文本；控制字符变空格，超长截断。
    ///
    /// 编码前会把连续控制字符替换为一个空格，并按 UTF-8 字节截断到
    /// `crate::program_status` 内部的 2048 字节上限，再做 base64。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `ProgramStatus.state`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgramStatusState {
    /// 空闲。
    Idle,
    /// 正在工作。
    Working,
    /// 阻塞在用户输入上（权限确认、问题、认证）；具体原因看 `kind`。
    Blocked,
    /// 任务完成。
    Done,
    /// 出错。
    Error,
    /// 清除状态显示；`ProcessTerminal::stop` 用它收尾。
    Clear,
}

/// `ProgramStatus.kind`（仅 `blocked` 时有意义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgramStatusKind {
    /// 等待权限确认。
    Permission,
    /// 等待用户回答问题。
    Question,
    /// 等待认证（例如登录）。
    Auth,
}

/// 终端抽象（TS `interface Terminal`）。
///
/// TS 的 `get columns()` / `get rows()` 在 Rust 里是普通方法（`&self`）。
/// `restore_on_exit` 是可选成员，默认空实现。
///
/// TUI 渲染层通过 `Box<dyn Terminal>` 持有终端（见 `crate::alt_screen` /
/// `crate::main_screen`），测试替身则实现本 trait 并捕获写入的字节流。
/// 方法都是同步的；唯一返回 future 的是 `drain_input`，它借用 `self`。
/// `start` 注册的回调由实现决定触发时机——`ProcessTerminal` 在 `poll_input` /
/// `poll_resize` 的调用线程上同步触发，测试替身则在测试主动注入时触发。
pub trait Terminal {
    /// 开始会话：保存 `on_input` / `on_resize` 回调，并让终端进入可接收按键的状态。
    ///
    /// `ProcessTerminal` 的实现还会进入 raw mode、开启括号粘贴、记录初始尺寸并启动
    /// 键盘协议协商；回调在之后由 `poll_input` / `poll_resize` 同步调用。
    fn start(&mut self, on_input: Box<dyn FnMut(&str)>, on_resize: Box<dyn FnMut()>);
    /// 结束会话：撤销本终端开启的模式并丢弃回调。
    ///
    /// 调用后实现不应再触发已注册的回调。`ProcessTerminal` 会关闭括号粘贴、Kitty 协议、
    /// 进度指示并暂停 stdin，最后恢复进入前的 raw mode。
    fn stop(&mut self);
    /// 进程 `exit` 处理器里的同步恢复：先写 `before`，再写本终端的模式重置，最后写 `after`。
    fn restore_on_exit(&mut self, _before: &str, _after: &str) {}
    /// 退出前排空 stdin（Kitty 释放事件在慢速 SSH 上会漏到父 shell）。默认 1000 ms / 空闲 50 ms。
    ///
    /// `max_ms` 是最长等待时间，`idle_ms` 是连续无新数据的空闲窗口；`ProcessTerminal`
    /// 在两者任一满足时结束。TS 的默认参数是 1000 / 50（`spec/tui-core.md` 的
    /// `drainInput` 条目），Rust 签名没有默认值，由调用方显式传入（`kiana-interactive`
    /// 的退出路径传 1000 / 50）。
    fn drain_input(
        &mut self,
        max_ms: u64,
        idle_ms: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>>;
    /// 原样写出文本；换行、光标与样式都由调用方拼好。
    fn write(&mut self, data: &str);
    /// 当前终端宽度（列，字符单元）。
    fn columns(&self) -> usize;
    /// 当前终端高度（行）。
    fn rows(&self) -> usize;
    /// Kitty 键盘协议当前是否生效（由协商结果或测试显式设置）。
    fn kitty_protocol_active(&self) -> bool;
    /// 相对移动光标：正数向下、负数向上、`0` 不动。
    fn move_by(&mut self, lines: isize);
    /// 隐藏光标。
    fn hide_cursor(&mut self);
    /// 显示光标。
    fn show_cursor(&mut self);
    /// 清除当前行（`ESC[K` 语义：从光标到行尾）。
    fn clear_line(&mut self);
    /// 清除从光标到屏幕末尾（`ESC[J` 语义）。
    fn clear_from_cursor(&mut self);
    /// 清屏并把光标移到左上角。
    fn clear_screen(&mut self);
    /// 设置终端窗口标题。
    fn set_title(&mut self, title: &str);
    /// 打开/关闭进度指示（`ProcessTerminal` 用 OSC 9;4 写出，并周期重发活动态）。
    fn set_progress(&mut self, active: bool);
    /// 上报程序状态（OSC 7501；编码见 `crate::program_status`）。
    fn set_program_status(&mut self, status: &ProgramStatus);

    /// 把读取线程已收到的 stdin 字节喂给解码/分发路径（宿主循环每轮调用）。
    ///
    /// TS 没有这个方法：Node 在 `stdin.on("data")` 时直接回调 `onInput`。Rust 的读取
    /// 线程把字节放进通道，需要一个宿主轮询点；不支持真实 stdin 的终端返回 `false`。
    fn poll_input(&mut self) -> bool {
        false
    }

    /// 推进终端自身的定时器（转义序列超时、Kitty 协商缓冲、进度保活）。
    ///
    /// TS 由事件循环里的 `setTimeout`/`setInterval` 驱动；Rust 宿主每轮传入当前时刻。
    fn poll_timers(&mut self, _now: std::time::Instant) {}

    /// 轮询终端尺寸变化（TS 的 `stdout.on("resize")`）。
    fn poll_resize(&mut self) {}
}

// ============================================================================
// 解析（terminal-colors.ts 的函数部分）
// ============================================================================

/// `hexToRgb(hex)`：可带或不带 `#`；取前 6 位，`parseInt(..., 16)` 三段。
///
/// **不做长度校验**（调用方保证）。V8 的 `parseInt` 遇到非十六进制字符即停，
/// 这里用同样的"读到非十六进制为止"的语义。
///
/// 每段最多读 2 个字符：位数不足时按实际位数解析（`#f` 的红通道是 15），一个有效字符都
/// 没有时该通道为 0；超过 6 位的部分被忽略。没有 `#` 前缀时同样从第 0/2/4 位开始切。
fn hex_to_rgb(hex: &str) -> RgbColor {
    let normalized = hex.strip_prefix('#').unwrap_or(hex);
    let bytes = normalized.as_bytes();
    let channel = |start: usize| -> u8 {
        let mut value: u32 = 0;
        let mut any = false;
        for b in bytes.iter().skip(start).take(2) {
            let Some(digit) = (*b as char).to_digit(16) else {
                break;
            };
            value = value.wrapping_mul(16).wrapping_add(digit);
            any = true;
        }
        if any { value as u8 } else { 0 }
    };
    RgbColor {
        r: channel(0),
        g: channel(2),
        b: channel(4),
    }
}

/// `parseOscHexChannel(channel)`：按位宽归一化到 0-255，非十六进制或空 → `None`。
///
/// 每个十六进制位代表 4 bit，位数决定量程：1 位满量程 15、2 位 255、3 位 4095、
/// 4 位 65535，换算后都归一到 0-255——所以 `f`、`ff`、`ffff` 都表示满值。公式是
/// `parseInt(channel, 16) / (16^len - 1) * 255`，之后按 JS `Math.round` 取整并夹到
/// 0-255。`len >= 256` 时按 JS 的 `16 ** len === Infinity` 处理（见函数内注释）。
fn parse_osc_hex_channel(channel: &str) -> Option<u8> {
    if channel.is_empty() || !channel.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // `max = 16 ** len - 1`：JS 的 `**` 是**双精度**运算。`4 * len <= 1020` 时 16**len 是精确的
    // 2 的幂，减 1 按 IEEE 舍入；`len >= 256` 时 `16 ** len` 溢出成 `Infinity`（`max > 0` 仍成立）。
    let length = channel.len();
    let max = if length >= 256 {
        f64::INFINITY
    } else {
        // 2^(4*len) 精确可表示，减 1 由硬件按 IEEE 舍入。
        (2.0f64).powi(4 * i32::try_from(length).expect("length < 256")) - 1.0
    };
    if max <= 0.0 {
        return None;
    }
    let value = parse_hex_double(channel);
    let ratio = value / max * 255.0;
    let rounded = js_round_half_up(ratio);
    if !rounded.is_finite() {
        return None;
    }
    Some(rounded.clamp(0.0, 255.0) as u8)
}

/// `parseInt(channel, 16)` 的**双精度**语义：任意长度的十六进制串都按 IEEE 就近偶数舍入。
///
/// 不能直接用 `u128::from_str_radix`（超过 32 位就失败）或循环乘加（每步舍入，与一次舍入不同：
/// 16 位以上时 34/420 个样本不一致）。这里取前 15 个十六进制位（60 bit）作尾数，其余位只提供
/// sticky 位，再按 round-to-nearest-even 收缩到 53 bit，最后乘 2 的幂（精确）。
///
/// 前导 `0` 先被去掉，所以长度判断只看有效位；尾数右移 7 位后恰好是 53 bit，低 7 位与
/// sticky 一起用于平局判定。例：`"ffff"` → `65535.0`。
fn parse_hex_double(text: &str) -> f64 {
    let digit = |b: u8| u64::from((b as char).to_digit(16).expect("caller checked hex"));
    let digits = text.trim_start_matches('0').as_bytes();
    if digits.is_empty() {
        return 0.0;
    }
    let length = digits.len();
    if length <= 15 {
        let mut value: u64 = 0;
        for &b in digits {
            value = value * 16 + digit(b);
        }
        return value as f64;
    }
    let mut high: u64 = 0;
    for &b in &digits[..15] {
        high = high * 16 + digit(b);
    }
    let sticky = digits[15..].iter().any(|&b| b != b'0');
    // 60 bit → 53 bit：低 7 位参与就近偶数舍入。
    let mut significand = high >> 7;
    let remainder = high & 0x7F;
    if remainder > 0x40 || (remainder == 0x40 && (sticky || significand & 1 == 1)) {
        significand += 1;
    }
    // 乘 2 的幂在 IEEE 下是精确的（只有溢出会变成 Inf，与 JS 一致）。
    let exponent = 4 * (length as i32 - 15) + 7;
    significand as f64 * (2.0f64).powi(exponent)
}

/// JS `Math.round`（半数向 +∞）：`NaN` / `±Inf` / `±0` 原样返回。
///
/// 实现是 `(x + 0.5).floor()`。对负的半数（如 `-0.5`）得到 `+0.0`，而 JS 得到 `-0`；
/// 两者数值相等、只差零的符号，且本文件的调用点只传入非负比值（`value / max * 255`）。
fn js_round_half_up(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    (x + 0.5).floor()
}

/// `parseOscColorValue(rawValue)`。
///
/// 先做 JS 语义的 `trim`（`crate::text::trim_js`，与 Rust 的 `str::trim` 在 U+FEFF /
/// U+0085 上不同），再按两种语法解析：
///
/// - `#` 开头：恰好 6 位十六进制按每通道 2 位（8 bit）解释；恰好 12 位按每通道 4 位
///   （16 bit）交给 `parse_osc_hex_channel` 归一化；其它长度一律 `None`（所以 `#fff`
///   **不**被接受）。
/// - 其它：剥掉可选的 `rgb:` / `rgba:` 前缀（大小写不敏感），按 `/` 切成至少 3 段，
///   每段是任意长度的十六进制串，按位宽归一化；多余的段忽略。缺段或任一段非法 → `None`。
///
/// 两条路径都不接受命名色、`rgb()` 函数形式或百分比。
fn parse_osc_color_value(raw_value: &str) -> Option<RgbColor> {
    // TS 的 `String.prototype.trim()` 用 JS `\s`（含 U+FEFF、不含 U+0085），
    // 与 Rust 的 `str::trim`（Unicode White_Space）正好在这两点相反。
    let value = crate::text::trim_js(raw_value);
    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(hex_to_rgb(value));
        }
        if hex.len() == 12 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let r = parse_osc_hex_channel(&hex[0..4]);
            let g = parse_osc_hex_channel(&hex[4..8]);
            let b = parse_osc_hex_channel(&hex[8..12]);
            return match (r, g, b) {
                (Some(r), Some(g), Some(b)) => Some(RgbColor { r, g, b }),
                _ => None,
            };
        }
        return None;
    }

    // `value.replace(/^rgba?:/i, "")`
    let rgb_value = strip_rgb_prefix(value);
    let parts: Vec<&str> = rgb_value.split('/').collect();
    if parts.len() < 3 {
        return None;
    }
    let r = parse_osc_hex_channel(parts[0]);
    let g = parse_osc_hex_channel(parts[1]);
    let b = parse_osc_hex_channel(parts[2]);
    match (r, g, b) {
        (Some(r), Some(g), Some(b)) => Some(RgbColor { r, g, b }),
        _ => None,
    }
}

/// `value.replace(/^rgba?:/i, "")`。
///
/// 只认 ASCII 前缀；用 `strip_prefix` 的字节切片会落在多字节字符中间并 panic
/// （`value[..4]`），因此逐字节比较后按长度切。没有匹配的前缀时原样返回。
fn strip_rgb_prefix(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 5 && bytes[..5].eq_ignore_ascii_case(b"rgba:") {
        return &value[5..];
    }
    if bytes.len() >= 4 && bytes[..4].eq_ignore_ascii_case(b"rgb:") {
        return &value[4..];
    }
    value
}

/// `parseOscColorResponse(data)`：解析 OSC 10 / 11 / 4 的颜色回复。
///
/// 不是这类回复时返回 `None`；是回复但颜色无法解析时 `rgb == None`。
///
/// 识别步骤：
///
/// 1. 必须以 `ESC ]` 开头，且**整串**以 BEL 或 `ESC \`（ST）结尾；结尾之后还有别的字符
///    会直接返回 `None`。
/// 2. 目标字段：`10;` → 前景、`11;` → 背景、`4;<1-3 位十进制>;<值>` → 调色板索引；
///    其它前缀（例如 `12;`）返回 `None`。
/// 3. 值里不能出现 BEL 或 ESC（对应 TS 正则的 `[^\x07\x1b]*`）。
/// 4. 交给 `parse_osc_color_value`；失败时保留目标、`rgb` 为 `None`。
///
/// 索引为 4 位（如 `1000`）、空索引、或缺少 `;值` 时都返回 `None`；这些边界由
/// `crates/kiana-tui/tests/terminal_colors.rs` 锁定。
pub fn parse_osc_color_response(data: &str) -> Option<(OscColorTarget, Option<RgbColor>)> {
    const ESC: char = '\u{1b}';
    const BEL: char = '\u{7}';
    if !data.starts_with("\u{1b}]") {
        return None;
    }
    // `^\x1b\](?:(1[01])|4;(\d{1,3}));([^\x07\x1b]*)(?:\x07|\x1b\\)$`
    // `ESC ]` 是 2 个 ASCII 字节，按字节切安全。
    let body = &data[2..];
    // 尾部终止符（BEL 或 ST），必须是最后一个字符
    let body = if let Some(body) = body.strip_suffix(BEL) {
        body
    } else {
        let body = body.strip_suffix('\\')?;
        body.strip_suffix(ESC)?
    };

    // 注意两条分支的 `;` 数量不同：`ESC ]10;<value>` 只有 1 个，而 `ESC ]4;<index>;<value>`
    // 有 2 个（索引本身是 `4;` 之后直到下一个 `;` 的一段）。
    let target = if let Some(rest) = body.strip_prefix("10;") {
        (OscColorTarget::Foreground, rest)
    } else if let Some(rest) = body.strip_prefix("11;") {
        (OscColorTarget::Background, rest)
    } else {
        let rest = body.strip_prefix("4;")?;
        let separator = rest.find(';')?;
        let digits = &rest[..separator];
        if digits.is_empty() || digits.len() > 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        (
            OscColorTarget::Palette(digits.parse().ok()?),
            &rest[separator + 1..],
        )
    };
    let (target, value) = target;

    // `([^\x07\x1b]*)`：值里不能出现 BEL 或 ESC
    if value.chars().any(|ch| ch == BEL || ch == ESC) {
        return None;
    }

    Some((target, parse_osc_color_value(value)))
}

/// `parseTerminalColorSchemeReport(data)`：`ESC [ ?997;1n`（dark）/ `2`（light）。
///
/// 正则要求同一序列**一次或多次**重复，因此 `\x1b[?997;1n\x1b[?997;2n` 也算匹配，
/// 取**最后一次**捕获（JS `match` 对重复组保留最后一次捕获）。
///
/// 实现是循环剥离：每轮必须正好剥掉一个完整块，否则返回 `None`（空串在循环前就返回
/// `None`）；全部剥完后返回最后记录的值。
pub fn parse_terminal_color_scheme_report(data: &str) -> Option<TerminalColorScheme> {
    const CHUNK_DARK: &str = "\x1b[?997;1n";
    const CHUNK_LIGHT: &str = "\x1b[?997;2n";

    if data.is_empty() {
        return None;
    }
    let mut rest = data;
    let mut last: Option<TerminalColorScheme> = None;
    // 每轮只接受两种固定块之一；两个都不匹配时 `?` 直接返回 `None`。
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix(CHUNK_DARK) {
            last = Some(TerminalColorScheme::Dark);
            rest = tail;
        } else {
            let tail = rest.strip_prefix(CHUNK_LIGHT)?;
            last = Some(TerminalColorScheme::Light);
            rest = tail;
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_serializes_with_kind_tag() {
        let c = Color::Indexed { index: 3 };
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            r#"{"kind":"indexed","index":3}"#
        );
        let c = Color::Oklch {
            l: 0.5,
            c: 0.1,
            h: 120.0,
        };
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            r#"{"kind":"oklch","l":0.5,"c":0.1,"h":120.0}"#
        );
    }

    #[test]
    fn color_mode_matches_ts_spelling() {
        assert_eq!(
            serde_json::to_string(&TerminalColorMode::Palette256).unwrap(),
            "\"256color\""
        );
    }
}
