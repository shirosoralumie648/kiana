//! wheel-scroll.ts。
//!
//! 把鼠标滚轮事件转换成行数。`"auto"` 模式在**不会**自行加速滚轮的终端上按事件速度放大：
//! 孤立的单格滚轮移动一行，快速旋转最多每次 6 行。
//!
//! TS: `packages/tui/src/wheel-scroll.ts`。
//!
//! ## 为什么需要单独一层
//!
//! 终端只上报“发生了一次滚轮事件”，不告诉应用这次手势该移动多少行。物理滚轮、触控板
//! 与不同终端的事件密度差别很大，因此把“事件 → 行数”的换算集中在本模块。
//! [`WheelScrollAccelerator`] 带可变状态（上一次事件的时间、方向、平均间隔与小数余量），
//! 调用方必须在整场会话里复用同一个实例；重建实例等同于开始一次新手势。
//!
//! ## 调用方
//!
//! `crate::alt_screen` 的鼠标处理把 SGR 序列解析成 `direction`（滚轮向上 `-1`、向下 `1`），
//! 用 [`crate::tui::monotonic_clock`] 的毫秒时间戳调用 [`WheelScrollAccelerator::next`]，
//! 再把返回的行数乘进滚轮事件的 delta。备用屏选项 `wheel_scroll_lines` 未设置时用
//! `WheelScrollLines::Fixed(1.0)` 构造。
//!
//! ## 时间与数值约定
//!
//! - `now` 是**毫秒**，应来自单调时钟。
//! - [`WheelScrollAccelerator::next`] 只返回非负整数行数，方向由调用方自己保留；
//!   `direction` 在这里只用于判断“是否仍是同一手势”。
//! - 在非 macOS 构建里 `terminal_accelerates_wheel()` 恒为 `false`（`cfg!` 在编译期展开），
//!   因此 `Default` 与 `with_platform_default` 总得到 `accelerate = true`。

/// 每个滚轮事件移动的行数，或 `"auto"` 表示按速度加速（TS `WheelScrollLines`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WheelScrollLines {
    /// 固定行数；非有限值或小于 1 时按 1 处理。
    ///
    /// 实际使用 `floor` 后的整数部分，所以 `Fixed(2.9)` 每个事件移动 2 行。
    Fixed(f64),
    /// 按事件间隔自动加速；`accelerate` 为假时退化为每个事件 1 行。
    Auto,
}

/// 同一物理滚轮格内的多个事件间隔小于此时（毫秒）。
///
/// Ghostty 约 4 ms 发一个事件；高分辨率来源同样密集。这些事件各移动一行且不加速。
const BURST_GAP_MS: f64 = 5.0;
/// 超过这个间隔的停顿结束一次滚动手势。
const GESTURE_GAP_MS: f64 = 200.0;
/// 映射到"每次事件一行"的平均间隔；更快的事件按比例放大。
const REFERENCE_GAP_MS: f64 = 100.0;
/// 自动模式单次事件可返回的行数上限（不含 `carry` 带来的小数累加）。
const MAX_AUTO_LINES: f64 = 6.0;

/// 本地 macOS 终端收到的滚轮/触控板增量已经由系统加速过，且每个事件就是一行；
/// 其它平台以及客户端平台未知的 SSH 会话通常是一个物理格发一个事件。
///
/// `cfg!` 是编译期常量：非 macOS 构建不会求值后面的 `env::var_os`，直接返回 `false`。
/// 三个 `SSH_*` 变量任一存在即视为 SSH 会话。
fn terminal_accelerates_wheel() -> bool {
    cfg!(target_os = "macos")
        && std::env::var_os("SSH_CONNECTION").is_none()
        && std::env::var_os("SSH_CLIENT").is_none()
        && std::env::var_os("SSH_TTY").is_none()
}

/// 把滚轮事件转换成行数（TS `WheelScrollAccelerator`）。
///
/// 在不会加速滚轮输入的终端上，`"auto"` 模式按事件速度决定行数：
/// 间隔 100 ms 每个事件 1 行，50 ms 2 行，20 ms 5 行。
///
/// 各字段的写入者不同：`lines` 由 [`WheelScrollAccelerator::new`] 与
/// [`WheelScrollAccelerator::set_lines`] 设置；`accelerate` 只由 `new` 决定，且只在
/// [`WheelScrollLines::Auto`] 下生效（为假时每个 Auto 事件返回 1 行；`Fixed` 分支先返回，
/// 不受它影响）。`last_time` / `last_direction` 记录上一事件用于判定手势边界，
/// `average_gap` 是同一手势内的间隔滑动平均，`carry` 保存上一事件被取整截掉的小数行数、
/// 累加到下一次计算——这四项由 [`WheelScrollAccelerator::next`] 与私有 `reset` 维护。
#[derive(Debug, Clone)]
pub struct WheelScrollAccelerator {
    lines: WheelScrollLines,
    accelerate: bool,
    /// 上一事件的时间（毫秒）。初始与 `reset` 后是 `-∞`，使首个事件的间隔为 `+∞`，
    /// 必定被判定为“不是同一手势”。
    last_time: f64,
    /// 上一事件的方向。`0` 是“还没有上一事件”的哨兵，真实事件只会传 `-1` 或 `1`。
    last_direction: i32,
    /// 同一手势内非微事件的平均间隔（毫秒）；`None` 表示尚未采到第一个样本。
    average_gap: Option<f64>,
    /// 取整后剩下的小数行数，下一次计算时加回，使慢速滚动的累计位移不被丢弃。
    carry: f64,
}

impl Default for WheelScrollAccelerator {
    /// TS 默认参数：`lines = "auto"`、`accelerate = !terminalAcceleratesWheel()`。
    ///
    /// `terminal_accelerates_wheel()` 为真（macOS 本机、非 SSH）时 `accelerate = false`；
    /// 其余情况为 `true`。
    fn default() -> Self {
        Self::new(WheelScrollLines::Auto, !terminal_accelerates_wheel())
    }
}

impl WheelScrollAccelerator {
    /// `new WheelScrollAccelerator(lines = "auto", accelerate = !terminalAcceleratesWheel())`。
    ///
    /// 初始状态等价于刚执行过 `reset`：没有“上一事件”，也没有平均间隔与小数余量。
    pub fn new(lines: WheelScrollLines, accelerate: bool) -> Self {
        WheelScrollAccelerator {
            lines,
            accelerate,
            last_time: f64::NEG_INFINITY,
            last_direction: 0,
            average_gap: None,
            carry: 0.0,
        }
    }

    /// 用默认参数构造（`"auto"` + 平台探测）。等价于 `Self::default()`。
    pub fn with_platform_default() -> Self {
        Self::default()
    }

    /// `setLines(lines)`：设置并 `reset()`。
    ///
    /// `reset()` 无条件执行：即使新旧规则相同，调用后手势状态（上一事件、平均间隔、
    /// `carry`）也会被清空。Auto 模式下下一个事件按新手势从 1 行开始；Fixed 规则不读
    /// 这些状态，直接返回固定行数 `max(1, floor(n))`（见 [`WheelScrollAccelerator::next`]）。
    pub fn set_lines(&mut self, lines: WheelScrollLines) {
        self.lines = lines;
        self.reset();
    }

    /// 当前行数设置。
    pub fn lines(&self) -> WheelScrollLines {
        self.lines
    }

    /// `next(direction, now)`：返回该事件应移动的**正**行数。
    ///
    /// `direction` 为 `-1` 或 `1`，`now` 是毫秒时间戳。
    ///
    /// 决策顺序与代码分支一一对应：
    ///
    /// 1. `Fixed(n)`：返回 `max(1, floor(n))`；`NaN` / `±Inf` 也返回 1。此分支不读取、
    ///    也不更新任何手势状态。
    /// 2. `accelerate == false`：恒返回 1，同样不更新状态。
    /// 3. 只有“方向与上一事件相同、且间隔不超过 200 ms”才算同一手势；否则清空平均间隔
    ///    与 `carry` 并返回 1（手势的第一个事件永远只移动 1 行）。
    /// 4. 间隔小于 5 ms：视为同一物理格被拆成的高分辨率事件，返回 1 且不计入平均。
    /// 5. 其余情况把间隔并入平均（首个样本直接采用，之后取 `(旧平均 + 间隔) / 2`），
    ///    计算 `scaled = max(1, 100 / 平均间隔)`，先取 `min(scaled, MAX_AUTO_LINES)`，
    ///    再加上 `carry`，最后取和的整数部分返回，小数部分写回 `carry`。
    ///
    /// 数值边界：进入第 5 步时平均间隔至少为 5 ms，除法结果有限；`now` 若不是单调递增，
    /// 间隔可能为负，此时间隔小于 5，会落到返回 1 的分支（第 3 步或第 4 步）。
    pub fn next(&mut self, direction: i32, now: f64) -> usize {
        if let WheelScrollLines::Fixed(lines) = self.lines {
            return if lines.is_finite() {
                // `Math.max(1, Math.floor(lines))`；负数与 0 都夹到 1。
                let floored = lines.floor();
                let clamped = if floored > 1.0 { floored } else { 1.0 };
                // 非负且有限；`as usize` 对超大值饱和（JS 侧是 f64，不会出现该差异）。
                clamped as usize
            } else {
                1
            };
        }
        if !self.accelerate {
            return 1;
        }

        let gap = now - self.last_time;
        let same_gesture = direction == self.last_direction && gap <= GESTURE_GAP_MS;
        self.last_time = now;
        self.last_direction = direction;
        if !same_gesture {
            self.average_gap = None;
            self.carry = 0.0;
            return 1;
        }
        if gap < BURST_GAP_MS {
            return 1;
        }

        self.average_gap = match self.average_gap {
            // 手势内第一个非微事件：直接作为初始平均。
            None => Some(gap),
            // 之后的每个样本把平均值向当前间隔拉近一半。
            Some(average) => Some((average + gap) / 2.0),
        };
        let average_gap = self.average_gap.expect("assigned above");
        // 平均间隔为 0 时不可能是有限值（`gap >= BURST_GAP_MS`），因此除法安全。
        let scaled = (REFERENCE_GAP_MS / average_gap).max(1.0);
        let lines = scaled.min(MAX_AUTO_LINES) + self.carry;
        // 整数行本次发出（>= 1，因为 scaled 已夹到 1），小数行进入 carry。
        let whole = lines.floor();
        self.carry = lines - whole;
        whole as usize
    }

    /// 清空全部手势状态；构造与 [`WheelScrollAccelerator::set_lines`] 使用。
    fn reset(&mut self) {
        self.last_time = f64::NEG_INFINITY;
        self.last_direction = 0;
        self.average_gap = None;
        self.carry = 0.0;
    }
}
