//! components/easter-egg-3d.ts + components/easter-egg-3d.lazy.ts。
//!
//! 全屏 3D 彩蛋：kiana logo（点击 header logo）与 Armin（`/arminsayshi`）。屏幕先溶解成盲文尘埃，
//! 模型在中心旋转，块状像素做滑块拼图；退出时按相反的时间线重组。
//!
//! TS 源：
//! * `packages/coding-agent/src/modes/interactive/components/easter-egg-3d.ts`
//! * `packages/coding-agent/src/modes/interactive/components/easter-egg-3d.lazy.ts`
//!
//! ## 与 TS 的表示差异（运行时适配）
//!
//! * **`TUI` 换成注入的回调**：`tui.terminal.rows` → `rows` 回调，`tui.requestRender()` →
//!   [`RequestRenderCallback`]，`performance.now()` → [`EasterEggClock`]，`setInterval` →
//!   [`kiana_tui::tui::Timer`]（[`EasterEgg3dAnimation::set_timer`] 安装；默认 [`NoopTimer`]）。
//!   与 TS 一样，`render` **不**请求重绘（唯一来源是 30 fps 定时器）；只有定时器没有真正
//!   跑起来时才由 `render` 逐帧驱动（见下）。
//! * **lazy 入口的渲染器参数**：spec 写的是 `&mut Tui`，但 `kiana_tui::Tui` 没有
//!   `getScreenLines`，只有 [`InteractiveRenderer`]（`tui_renderer.rs`）能区分主屏/全屏。
//!   因此 [`play_kiana_logo_3d`] / [`play_armin_3d`] 接收 `&mut InteractiveRenderer`。
//! * **同步世界里的 async**：TS 的 lazy 包装 `import(...).then(...)` 不阻塞；Rust 的
//!   [`play_kiana_logo_3d`] / [`play_armin_3d`] 是同步函数（签名冻结），内部直接调用同步变体
//!   `play_easter_egg_3d_now`，立即建立 overlay（先原样显示抓屏，作为动画就绪前的过渡界面），
//!   同时在 `LocalSet` 上发出带 100 ms 超时的终端颜色查询；
//!   查询结束（回复或超时）后**才**用终端上报的颜色构造动画并把 `start_time` 重置到此刻，
//!   因此每一帧都用上报色、时间线从 0 开始（与 TS「先 await 再 new」一致）。
//!   [`play_easter_egg_3d`]（async，与 TS 同名同义）保留完整的「先查终端颜色再播放」序列，
//!   查询期间替持有借用的调用方泵入终端输入（否则回复无人消费、必然超时）。模块级 `playing`
//!   标志对两条路径都生效。
//! * **调用约束**：`play_*` 会 `show_overlay()`，`EasterEgg3dAnimation` 收尾时会
//!   `overlay.hide()`。二者都操作 `TuiCore`，因此**不能在持有渲染器 `RefCell` 借用的输入
//!   分发内部调用**（宿主应先排队、事件返回后再调用；im-core 的 logo 点击与 im-commands 的
//!   `/arminsayshi` 都按此约定）。async 的 [`play_easter_egg_3d`] 还会跨 `await` 持有借用。
//! * **帧驱动**：`play_*` 在 `tokio` 运行时可用时安装 [`LocalTimer`]（`spawn_local`，
//!   与交互模式单线程 `LocalSet` 的架构一致，见 `ARCHITECTURE.md` §8.1），复刻 TS 的 30fps
//!   `setInterval` 及其「退出完成 / 1 秒未渲染 → finish()」判定。定时器不可用时（`NoopTimer`、
//!   无 tokio 运行时、或不在 `LocalSet` 里）由 `render` 按**同一时钟与条件**自行收尾并逐帧
//!   驱动：`render` 在 `TuiCore` 可变借用内被调用，不能 `hide()`，改为把 overlay 标记为
//!   不可见并请求一帧重绘（用户可见结果与 `hide()` 相同；overlay 条目留在栈里但不再渲染/聚焦）。
//! * 颜色仍以 f64 参与运算；只有光栅缓冲按 TS 的 `Float32Array` 在**每次写入**时舍入为 f32。
//! * `hash` 用 `u32::wrapping_mul` 复刻 `Math.imul`；`Math.hypot` 用 V8 的「除以最大值」算法。
//! * 随机性：3D 动画**不使用** `Math.random()`（全部随机来自 `hash`），因此可逐字符对拍。
//!
//! ## 这个文件解决什么问题
//!
//! 它把「当前屏幕」当成一张带 ANSI 颜色的字符画，先让每个字素按距离变成盲文尘埃溶解掉，
//! 再把模型（4x4 的 kiana logo 或 31x36 的 Armin 像素图）用**光线投射**渲染成盲文点阵
//! （每个终端格是 2x4 个点），随后模型按确定性的滑块拼图打散/重组，退出时沿相反的时间线
//! 让尘埃重新聚成原屏幕。没有它，header logo 点击不会产生动画；`/arminsayshi` 会退化成
//! 内联的 `ArminComponent`（`armin.rs` 的 XBM 半块像素 + 入场效果动画）——当 [`play_armin_3d`] 返回
//! false（主屏）时 `interactive_mode/commands/misc.rs` 正是这样回退。
//!
//! 输入是「切换全屏前抓下来的行」（`Vec<String>`，可能含 SGR 转义）、模型参数与终端前景/
//! 背景色；输出是 `Component::render` 返回的整屏 ANSI 行。动画本身不写终端，只产出字符串，
//! 由 `kiana-tui` 的 overlay 合成为全屏画面。
//!
//! ## 主要数据结构与执行流程
//!
//! * [`Model`] / [`Block`]：模型是一次性构建的只读像素集合（每个前景像素一个 1x1x`DEPTH`
//!   的方块），[`BlockRaster`] 每帧把它们变换到屏幕并投射光线。
//! * [`parse_screen`] 把抓屏行解析成 [`ScreenCell`] 网格；`Inner::prepare_cells` 再给每格补上
//!   溶解延迟、盲文点数与抖动种子；`Inner::prepare_stars` 生成确定性的稀疏星空。
//! * 每帧 `Inner::render` 的顺序：由时钟算 `dissolve_time` / [`Pose`] / 拼图位移 → 构建
//!   [`Box3`] 列表 → [`BlockRaster::render`] 光栅化（含光晕）→ 逐格按优先级
//!   「提示文字 > logo 盲文点 > 原字符/尘埃/星空」拼成 ANSI 行。
//! * 播放入口分两条：async 的 [`play_easter_egg_3d`]（先查颜色再建动画）与同步的
//!   [`play_kiana_logo_3d`] / [`play_armin_3d`]（先建 overlay 显示抓屏，颜色查询完成后再
//!   通过 `Inner::resolve_colors` 重置时间线）。两者最终都走 `start_animation`。
//!
//! ## 状态机与生命周期
//!
//! * 模块级 `PLAYING`（[`is_playing`]）让全进程同一时刻只接受一个彩蛋；`swap(true)` 失败
//!   即直接返回。它在 `on_done`、`on_render_finish`（无定时器收尾）、`on_dispose` 与 eager
//!   路径的 `has_overlay()` 早退分支复位。
//! * `Inner.colors_pending`：仅 lazy 路径为 true，此时 `render` 原样返回抓屏且忽略关闭输入；
//!   `resolve_colors` 只生效一次，迟到的颜色回复不会改动已启动的时间线。
//! * `Inner.timer_active` 是 TS `timer !== undefined` 的等价物：`finish_now` 把它置 false 并
//!   取走 `on_done`；[`EasterEgg3dAnimation::is_playing`] 直接返回它。
//! * `Inner.exit`：第一次 `close()` 记录退出起点/相位/块偏移；进度 ≥ 1 或第二次 `close()`
//!   结束动画。没有定时器时由 `Inner::finish_on_render` 在 `render` 里完成同样的判定。
//!
//! ## 注意事项与失败模式
//!
//! * 本模块假定主题已初始化：`theme_color` 与 `ansi_for` 在 `THEME` 未初始化时 `expect` panic
//!   （对应 TS 在这里抛错）；非法的索引色（如越界的 `38;5;n`）经 `palette` 以 TS 相同
//!   文案 panic。
//! * `render` 运行在 `TuiCore` 的可变借用内，不能隐藏 overlay；所有「立即消失」的路径都只
//!   翻转 `Rc<Cell<bool>>` 可见性标志并请求重绘（见 `overlay_options_with_visibility`）。
//! * 抓屏行可能比 `width` 短或长：解析时按 `width` 截断，缺失的格子在合成阶段显示星空/空格。
//! * 逐字符行为由 `crates/kiana-interactive/tests/easter_egg_3d.rs` 的黄金快照锁定（fixture
//!   由 TS 生成）；修改常量、渲染公式或数学函数前应先对照这些测试。
//!
//! ## 阅读顺序
//!
//! 常量与 [`Model`] 构建 → [`parse_screen`] → [`shuffle_steps`] / [`BlockRaster`] → `Inner`
//! 的渲染与收尾 → 文件末尾的播放入口。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};

use kiana_theme::ThemeColor;
use kiana_tui::colors::{background_ansi, color_to_rgb, foreground_ansi, indexed_color, rgb_color};
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::terminal_colors::TerminalColors;
use kiana_tui::text::{get_grapheme_segmenter, visible_width};
use kiana_tui::tui::{
    Component, NoopTimer, OverlayAnchor, OverlayHandle, OverlayOptions, SizeValue, Timer,
    TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType, monotonic_clock,
};
use kiana_ui_kit::keybinding_hints::{KeyTextFormatOptions, format_key_text};

use crate::tui_renderer::InteractiveRenderer;

use super::armin::{ARMIN_HEIGHT, ARMIN_WIDTH, is_armin_pixel};

/// sRGB 三元组（TS `type Rgb = readonly [number, number, number]`，分量是 f64 0..255）。
pub type Rgb = [f64; 3];

// ---------------------------------------------------------------------------
// 常量（逐字，见 spec §2.45 / §2.60.7）
// ---------------------------------------------------------------------------

const DEPTH: f64 = 0.7;
const FLY_START: f64 = 0.1;
const FLY_DURATION: f64 = 1.3;
/// 无 `origin` 的模型从中心的斑点长大时的 braille dots / 像素。
const START_SCALE: f64 = 0.1;

const FRAME_MS: f64 = 1000.0 / 30.0;
const DUST_DURATION: f64 = 0.35;
const WAVE_SPREAD: f64 = 0.55;
const WAVE_JITTER: f64 = 0.12;
const DISSOLVE_END: f64 = FLY_START + WAVE_SPREAD + WAVE_JITTER + DUST_DURATION;
const BACKGROUND_FADE: f64 = 0.5;
const EXIT_DURATION: f64 = 1.1;
/// 屏幕完全溶解后，暗淡的星空才淡入。
const STARS_START: f64 = 2.5;
const STARS_FADE: f64 = 2.0;
const STAR_DENSITY: f64 = 0.018;
/// 星点色调：中性、淡蓝、淡金、淡玫瑰；每种都与终端前景色混合一半。
const STAR_TINTS: [Rgb; 4] = [
    [255.0, 255.0, 255.0],
    [170.0, 195.0, 255.0],
    [255.0, 225.0, 170.0],
    [255.0, 190.0, 205.0],
];
const LOGO_COLOR_STEP: f64 = 5.0;
/// 浅色背景上 logo 的光晕。
const LIGHT_HALO_STRENGTH: f64 = 0.3;
const HALO_RADIUS_X: i64 = 6;
const HALO_RADIUS_Y: i64 = 3;
const HALO_LEVELS: f64 = 32.0;
const STAR_ALPHA_LEVELS: f64 = 8.0;
const STAR_FLICKER_LEVELS: f64 = 3.0;
const HINT_FADE: f64 = 0.5;

// 滑块拼图的时间线。
const PUZZLE_START: f64 = FLY_START + FLY_DURATION + 3.0;
const PUZZLE_STEP: f64 = 0.3;
const PUZZLE_STEPS: usize = 12;
const PUZZLE_RETURN: f64 = 1.0;
const PUZZLE_HOLD: f64 = 0.6;
/// 一个完整拼图周期：12 步 × 0.3s + 归位 1s + 停留 0.6s = 5.2s。
const PUZZLE_CYCLE: f64 = PUZZLE_STEP * PUZZLE_STEPS as f64 + PUZZLE_RETURN + PUZZLE_HOLD;
/// 每步允许的 10 个方向（`+x -x +y -y +x -x +y -y +z -z`）。
const PUZZLE_MOVES: [Cell3; 10] = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
];

// 自转锁定到拼图周期。
/// 起飞后的加速时长（秒）。
const SPIN_RAMP: f64 = 1.4;
/// 匀速阶段的角速度：一个拼图周期转一整圈（弧度/秒）。
const SPIN_SPEED: f64 = (std::f64::consts::PI * 2.0) / PUZZLE_CYCLE;
/// `spin_angle` 在整圈附近用正弦减速，让正面停留更久；这是相位上的停留幅度。
const SPIN_LINGER: f64 = 0.6;
/// 第一次出现「正面」的时间点（拼图最后一步结束后的 hold 中点），用于计算相位对齐。
const FIRST_FRONT_VIEW: f64 =
    PUZZLE_START + PUZZLE_STEP * PUZZLE_STEPS as f64 + PUZZLE_RETURN + PUZZLE_HOLD / 2.0;

/// 盲文点位（2x4 单元内 `row * 2 + column`）。
pub const DOT_BITS: [u8; 8] = [0x01, 0x08, 0x02, 0x10, 0x04, 0x20, 0x40, 0x80];

/// `BRAILLE[bits]`：`String.fromCharCode(0x2800 + bits)`。
pub fn braille(bits: u8) -> char {
    char::from_u32(0x2800 + u32::from(bits)).expect("braille range is valid")
}

/// `Intl.Segmenter` 等价物来自 `kiana-tui`；模型网格坐标。
type Cell3 = [i64; 3];

/// 模型（TS `interface Model`）：动画期间只读的体素集合。
///
/// 由 [`create_model`] 系列函数一次性构建，坐标单位是「模型像素」：一个块占据
/// `x..x+1`、`y..y+1`、`z±DEPTH/2` 的盒子。`radius` 与 `width_share` 只被
/// `Inner::pose` 用来决定模型在屏幕上的最终缩放，渲染阶段不再修改任何字段。
pub struct Model {
    /// 位图宽（像素）。
    pub columns: usize,
    /// 位图高（像素）。
    pub rows: usize,
    /// 每个前景像素一个块。
    pub blocks: Vec<Block>,
    /// 相机到模型中心的距离（像素）。
    pub camera_distance: f64,
    /// 任意块角到中心的最远距离（含外层深度）。
    pub radius: f64,
    /// 旋转时模型覆盖的最大屏幕宽度比例。
    pub width_share: f64,
    /// 拼图每步移动的块数（输入 0..1 的随机数）。
    puzzle_kind: PuzzleKind,
    /// 模型半块渲染的左上角（`None` = 从中心长出）。
    pub origin: Option<(usize, usize)>,
}

/// 一个块（TS `Model["blocks"]` 的元素）。
pub struct Block {
    /// 位图列、行与深度层（-1/0/1）。
    pub home: Cell3,
    /// 颜色。
    pub color: Rgb,
}

/// `puzzleMoves` 的策略（TS 里是每个模型各自的闭包；Rust 用枚举区分）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PuzzleKind {
    /// kiana logo：`random < 0.4 ? 2 : 1`。
    Logo,
    /// Armin：`round(blockCount * (0.03 + random * 0.04))`。
    Armin {
        /// 块数（构建模型时确定）。
        block_count: usize,
    },
}

impl Model {
    /// TS `model.puzzleMoves(random)`：给定一个 0..1 的随机数，返回本步移动的块数。
    ///
    /// 具体策略由 [`PuzzleKind`] 决定：logo 在 1/2 块之间切换；Armin 按块数的 3%–7%
    /// 取整（[`js_round`] 后 clamp 到 ≥ 0）。随机数本身来自 [`hash`]，不是系统随机源。
    pub fn puzzle_moves(&self, random: f64) -> usize {
        match self.puzzle_kind {
            PuzzleKind::Logo => {
                if random < 0.4 {
                    2
                } else {
                    1
                }
            }
            PuzzleKind::Armin { block_count } => {
                js_round(block_count as f64 * (0.03 + random * 0.04)).max(0.0) as usize
            }
        }
    }
}

/// `CORAL`。
pub const CORAL: Rgb = [228.0, 138.0, 122.0];
/// `BLUE`。
pub const BLUE: Rgb = [79.0, 142.0, 179.0];
/// `YELLOW`。
pub const YELLOW: Rgb = [234.0, 182.0, 93.0];
const KIANA_LOGO_PIXELS: [&str; 4] = ["ccc.", "b.c.", "bb.y", "b..y"];

/// `createModel(...)`：按行优先扫描位图，把 `pixel` 返回 `Some(color)` 的格变成块。
///
/// * `pixel(column, row)` 对 `0..columns` × `0..rows` 的每个格调用一次；返回 `None` 的格
///   直接跳过（透明像素），因此 `blocks` 的顺序是「行优先、行内列优先」，后续拼图步骤与
///   退出偏移都依赖这个顺序。
/// * `radius` 是模型中心到块角距离的保守上界（用半宽、半高与 `1 + DEPTH/2` 的深度余量
///   经 [`js_hypot3`] 计算），供 `pose` 估算旋转后的投影范围。
/// * `camera_distance` 是透视投影的相机距离（模型像素单位）；`width_share` 是模型最多占
///   屏幕宽度的比例。两者都只在 `pose`/光栅化中使用，不参与块的生成。
/// * `origin = Some((column, row))` 表示模型应当从 header 里的半块渲染处长出（坐标是终端
///   格），`None` 表示从屏幕中心开始。
pub fn create_model(
    columns: usize,
    rows: usize,
    pixel: impl Fn(usize, usize) -> Option<Rgb>,
    camera_distance: f64,
    width_share: f64,
    origin: Option<(usize, usize)>,
    puzzle_kind: PuzzleKind,
) -> Model {
    let mut blocks = Vec::new();
    for row in 0..rows {
        for column in 0..columns {
            if let Some(color) = pixel(column, row) {
                blocks.push(Block {
                    home: [column as i64, row as i64, 0],
                    color,
                });
            }
        }
    }
    let radius = js_hypot3(columns as f64 / 2.0, rows as f64 / 2.0, 1.0 + DEPTH / 2.0);
    Model {
        columns,
        rows,
        blocks,
        camera_distance,
        radius,
        width_share,
        puzzle_kind,
        origin,
    }
}

/// `kianaLogoModel(origin)`：4x4 的 kiana logo 体素模型。
///
/// 位图见 `KIANA_LOGO_PIXELS`：`c`/`b`/`y` 分别是 [`CORAL`]/[`BLUE`]/[`YELLOW`]，`.` 透明。
/// 相机距离固定 10、宽度份额 0.35；`PuzzleKind::Logo` 让每步只移动 1–2 块。
/// `origin` 是 header logo 左上角的终端格坐标，`None` 语义见 [`create_model`]。
pub fn kiana_logo_model(origin: (usize, usize)) -> Model {
    create_model(
        4,
        4,
        |column, row| {
            let symbol = KIANA_LOGO_PIXELS[row].as_bytes().get(column).copied();
            match symbol {
                Some(b'c') => Some(CORAL),
                Some(b'b') => Some(BLUE),
                Some(b'y') => Some(YELLOW),
                _ => None,
            }
        },
        10.0,
        0.35,
        Some(origin),
        PuzzleKind::Logo,
    )
}

/// `arminModel(color)`：按 `super::armin::is_armin_pixel` 的位图构建的 3D Armin。
///
/// 宽高取自 [`ARMIN_WIDTH`]/[`ARMIN_HEIGHT`]，所有前景像素使用同一个 `color`（调用方传
/// 主题的 `Accent` 色）。块数在这里先数一遍，用于 `PuzzleKind::Armin` 计算每步移动块数；
/// 相机距离 80、宽度份额 0.45，`origin = None` 表示从屏幕中心长出。
pub fn armin_model(color: Rgb) -> Model {
    let columns = ARMIN_WIDTH;
    let rows = ARMIN_HEIGHT;
    let block_count = (0..rows)
        .flat_map(|row| (0..columns).map(move |column| (column, row)))
        .filter(|(column, row)| is_armin_pixel(*column, *row))
        .count();
    create_model(
        columns,
        rows,
        |column, row| {
            if is_armin_pixel(column, row) {
                Some(color)
            } else {
                None
            }
        },
        80.0,
        0.45,
        None,
        PuzzleKind::Armin { block_count },
    )
}

// ---------------------------------------------------------------------------
// 数学工具（运算顺序固定，保证逐帧结果可复现）
// ---------------------------------------------------------------------------

/// JS `Math.imul` 链（`hash`）。`value` 是整数值的 f64，按 ToInt32 截断。
pub fn hash(value: f64) -> f64 {
    let v = js_to_int32(value) as u32;
    let mut x = v ^ 0x9e37_79b9u32;
    x = x.wrapping_mul(0x85eb_ca6bu32);
    x ^= x >> 13;
    x = x.wrapping_mul(0xc2b2_ae35u32);
    x ^= x >> 16;
    f64::from(x) / 4_294_967_296.0
}

/// JS `ToInt32`（整数值输入；超出范围按模 2^32）。
fn js_to_int32(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    // `as i64` 向零截断；再 `as i32` 取低 32 位（模 2^32）。
    value.trunc() as i64 as i32
}

/// JS `Math.round`：半数向 +∞。
pub fn js_round(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    (value + 0.5).floor()
}

// ---------------------------------------------------------------------------
// Math.sin / Math.cos（V8 的 fdlibm 端口；逐位复刻以保证旋转结果可复现）
// ---------------------------------------------------------------------------

/// V8 `Math.sin`：fdlibm 的 `__ieee754_rem_pio2` + `__kernel_sin` / `__kernel_cos`。
///
/// `libm`/系统 `libm` 的优化版 `rem_pio2`（Bruce Evans）在约 0.9% 的输入上与 V8 差
/// 1 ulp（旋转矩阵/姿态因此不是逐位一致，会破坏按 Node 输出生成的金帧对拍）。
/// 这里逐行移植 V8 `src/base/ieee754.cc` 的原始 fdlibm 实现
/// （SunPro 版 `e_rem_pio2.c` + `k_sin.c` / `k_cos.c`），
/// 与 Node 26 的 `Math.sin` 在 25 万个样本（含 ±1e300）上逐位相同。
pub fn js_sin(x: f64) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;
    if ix <= 0x3fe9_21fb {
        return kernel_sin(x, 0.0, 0);
    }
    if ix >= 0x7ff0_0000 {
        return nan_from(x);
    }
    let (n, y0, y1) = rem_pio2(x);
    match n & 3 {
        0 => kernel_sin(y0, y1, 1),
        1 => kernel_cos(y0, y1),
        2 => -kernel_sin(y0, y1, 1),
        _ => -kernel_cos(y0, y1),
    }
}

/// V8 `Math.cos`（与 [`js_sin`] 同一 fdlibm 端口）。
pub fn js_cos(x: f64) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;
    if ix <= 0x3fe9_21fb {
        return kernel_cos(x, 0.0);
    }
    if ix >= 0x7ff0_0000 {
        return nan_from(x);
    }
    let (n, y0, y1) = rem_pio2(x);
    match n & 3 {
        0 => kernel_cos(y0, y1),
        1 => -kernel_sin(y0, y1, 1),
        2 => -kernel_cos(y0, y1),
        _ => kernel_sin(y0, y1, 1),
    }
}

/// TS 的 `x - x`（Inf/NaN 都变成 NaN）。fdlibm 用这个表达式抛信号/返回 NaN。
#[allow(clippy::eq_op, reason = "逐字复刻 JS `x - x`；两个操作数必然相等")]
fn nan_from(x: f64) -> f64 {
    x - x
}

fn high_word(x: f64) -> u32 {
    (x.to_bits() >> 32) as u32
}

fn low_word(x: f64) -> u32 {
    x.to_bits() as u32
}

fn set_high_word(x: f64, high: u32) -> f64 {
    f64::from_bits((x.to_bits() & 0xffff_ffff) | (u64::from(high) << 32))
}

fn set_low_word(x: f64, low: u32) -> f64 {
    f64::from_bits((x.to_bits() & !0xffff_ffff) | u64::from(low))
}

fn insert_words(high: u32, low: u32) -> f64 {
    f64::from_bits((u64::from(high) << 32) | u64::from(low))
}

/// fdlibm `two_over_pi`：2/π 的 24-bit 分块（396 个十六进制位）。
const TWO_OVER_PI: [i32; 66] = [
    0xA2F983, 0x6E4E44, 0x1529FC, 0x2757D1, 0xF534DD, 0xC0DB62, 0x95993C, 0x439041, 0xFE5163,
    0xABDEBB, 0xC561B7, 0x246E3A, 0x424DD2, 0xE00649, 0x2EEA09, 0xD1921C, 0xFE1DEB, 0x1CB129,
    0xA73EE8, 0x8235F5, 0x2EBB44, 0x84E99C, 0x7026B4, 0x5F7E41, 0x3991D6, 0x398353, 0x39F49C,
    0x845F8B, 0xBDF928, 0x3B1FF8, 0x97FFDE, 0x05980F, 0xEF2F11, 0x8B5A0A, 0x6D1F6D, 0x367ECF,
    0x27CB09, 0xB74F46, 0x3F669E, 0x5FEA2D, 0x7527BA, 0xC7EBE5, 0xF17B3D, 0x0739F7, 0x8A5292,
    0xEA6BFB, 0x5FB11F, 0x8D5D08, 0x560330, 0x46FC7B, 0x6BABF0, 0xCFBC20, 0x9AF436, 0x1DA9E3,
    0x91615E, 0xE61B08, 0x659985, 0x5F14A0, 0x68408D, 0xFFD880, 0x4D7327, 0x310606, 0x1556CA,
    0x73A8C9, 0x60E27B, 0xC08C6B,
];

/// fdlibm `npio2_hw`：n * kiana/2 的高位字（n = 1..32）。
const NPIO2_HW: [u32; 32] = [
    0x3FF921FB, 0x400921FB, 0x4012D97C, 0x401921FB, 0x401F6A7A, 0x4022D97C, 0x4025FDBB, 0x402921FB,
    0x402C463A, 0x402F6A7A, 0x4031475C, 0x4032D97C, 0x40346B9C, 0x4035FDBB, 0x40378FDB, 0x403921FB,
    0x403AB41B, 0x403C463A, 0x403DD85A, 0x403F6A7A, 0x40407E4C, 0x4041475C, 0x4042106C, 0x4042D97C,
    0x4043A28C, 0x40446B9C, 0x404534AC, 0x4045FDBB, 0x4046C6CB, 0x40478FDB, 0x404858EB, 0x404921FB,
];

/// fdlibm `__ieee754_rem_pio2`：返回 `x rem kiana/2`（`y0 + y1`）与商 `n`。
// `INVPIO2` 是 fdlibm 截断到 53 位的 2/π；std 的 `FRAC_2_PI` 是另一个值，不能替换。
#[allow(
    clippy::approx_constant,
    reason = "fdlibm's truncated 2/π constant must keep its exact bits"
)]
fn rem_pio2(x: f64) -> (i32, f64, f64) {
    const HALF: f64 = 0.5;
    const TWO24: f64 = 16777216.0;
    const INVPIO2: f64 = 0.6366197723675814;
    const PIO2_1: f64 = 1.5707963267341256;
    const PIO2_1T: f64 = 6.077100506506192e-11;
    const PIO2_2: f64 = 6.077100506303966e-11;
    const PIO2_2T: f64 = 2.0222662487959506e-21;
    const PIO2_3: f64 = 2.0222662487111665e-21;
    const PIO2_3T: f64 = 8.4784276603689e-32;

    let hx = high_word(x) as i32;
    let ix = hx & 0x7fff_ffff;
    if ix <= 0x3fe9_21fb {
        return (0, x, 0.0);
    }
    if ix < 0x4002_d97c {
        // |x| < 3pi/4：n = ±1 的特例。
        if hx > 0 {
            let mut z = x - PIO2_1;
            let (y0, y1) = if ix != 0x3ff9_21fb {
                let y0 = z - PIO2_1T;
                (y0, (z - y0) - PIO2_1T)
            } else {
                z -= PIO2_2;
                let y0 = z - PIO2_2T;
                (y0, (z - y0) - PIO2_2T)
            };
            return (1, y0, y1);
        }
        let mut z = x + PIO2_1;
        let (y0, y1) = if ix != 0x3ff9_21fb {
            let y0 = z + PIO2_1T;
            (y0, (z - y0) + PIO2_1T)
        } else {
            z += PIO2_2;
            let y0 = z + PIO2_2T;
            (y0, (z - y0) + PIO2_2T)
        };
        return (-1, y0, y1);
    }
    if ix <= 0x4139_21fb {
        // |x| ~<= 2^19*(kiana/2)：中等规模。
        let t = x.abs();
        let n = (t * INVPIO2 + HALF) as i32;
        let f_n = f64::from(n);
        let mut r = t - f_n * PIO2_1;
        let mut w = f_n * PIO2_1T;
        let mut y0;
        if n < 32 && ix as u32 != NPIO2_HW[(n - 1) as usize] {
            y0 = r - w;
        } else {
            let j = ix >> 20;
            y0 = r - w;
            let high = high_word(y0);
            let mut i = j - ((high >> 20) & 0x7ff) as i32;
            if i > 16 {
                let t = r;
                w = f_n * PIO2_2;
                r = t - w;
                w = f_n * PIO2_2T - ((t - r) - w);
                y0 = r - w;
                let high = high_word(y0);
                i = j - ((high >> 20) & 0x7ff) as i32;
                if i > 49 {
                    let t = r;
                    w = f_n * PIO2_3;
                    r = t - w;
                    w = f_n * PIO2_3T - ((t - r) - w);
                    y0 = r - w;
                }
            }
        }
        let y1 = (r - y0) - w;
        return if hx < 0 { (-n, -y0, -y1) } else { (n, y0, y1) };
    }
    if ix >= 0x7ff0_0000 {
        // Inf / NaN。
        let nan = nan_from(x);
        return (0, nan, nan);
    }
    // 其余（大规模）参数：先切成 24-bit 分块。
    let low = low_word(x);
    let e0 = (ix >> 20) - 1046;
    let z = set_high_word(
        set_low_word(x, low),
        (ix as u32).wrapping_sub((e0 as u32) << 20),
    );
    let mut tx = [0.0f64; 3];
    tx[0] = f64::from(z as i32);
    let scaled = (z - tx[0]) * TWO24;
    tx[1] = f64::from(scaled as i32);
    tx[2] = (scaled - tx[1]) * TWO24;
    let mut nx = 3;
    while tx[nx - 1] == 0.0 {
        nx -= 1;
    }
    let (n, y0, y1) = kernel_rem_pio2(&tx, e0, nx, 2);
    if hx < 0 { (-n, -y0, -y1) } else { (n, y0, y1) }
}

/// fdlibm `__kernel_rem_pio2`（`prec == 2`，即双精度）。
fn kernel_rem_pio2(x: &[f64; 3], e0: i32, nx: usize, prec: usize) -> (i32, f64, f64) {
    const INIT_JK: [i32; 4] = [2, 3, 4, 6];
    const PIO2: [f64; 8] = [
        1.570796251296997,
        7.549789415861596e-08,
        5.390302529957765e-15,
        3.282003415807913e-22,
        1.270655753080676e-29,
        1.2293330898111133e-36,
        2.7337005381646456e-44,
        2.1674168387780482e-51,
    ];
    const ZERO: f64 = 0.0;
    const ONE: f64 = 1.0;
    const TWO24: f64 = 16777216.0;
    const TWON24: f64 = 5.960464477539063e-08;

    let mut iq = [0i32; 20];
    let mut f = [0.0f64; 20];
    let mut fq = [0.0f64; 20];
    let mut q = [0.0f64; 20];
    let jk = INIT_JK[prec];
    let jp = jk;
    let jx = (nx - 1) as i32;
    let mut jv = (e0 - 3) / 24;
    if jv < 0 {
        jv = 0;
    }
    let mut q0 = e0 - 24 * (jv + 1);
    let m = jx + jk;
    let first = jv - jx;
    for i in 0..=m {
        let index = first + i;
        f[i as usize] = if index < 0 {
            ZERO
        } else {
            f64::from(TWO_OVER_PI[index as usize])
        };
    }
    for i in 0..=jk {
        let mut fw = 0.0;
        for j in 0..=jx {
            fw += x[j as usize] * f[jx.wrapping_add(i).wrapping_sub(j) as usize];
        }
        q[i as usize] = fw;
    }
    let mut jz = jk;
    // `goto recompute`：重算时重新蒸馏 q[]。
    'recompute: loop {
        let mut i = 0i32;
        let mut j = jz;
        let mut z = q[jz as usize];
        while j > 0 {
            let fw = (TWON24 * z) as i32;
            iq[i as usize] = (z - TWO24 * f64::from(fw)) as i32;
            z = q[(j - 1) as usize] + f64::from(fw);
            i += 1;
            j -= 1;
        }
        z = libm::scalbn(z, q0);
        z -= 8.0 * (z * 0.125).floor();
        let mut n = z as i32;
        z -= f64::from(n);
        let mut ih = 0i32;
        if q0 > 0 {
            let i = iq[(jz - 1) as usize] >> (24 - q0);
            n += i;
            iq[(jz - 1) as usize] -= i << (24 - q0);
            ih = iq[(jz - 1) as usize] >> (23 - q0);
        } else if q0 == 0 {
            ih = iq[(jz - 1) as usize] >> 23;
        } else if z >= 0.5 {
            ih = 2;
        }
        if ih > 0 {
            n += 1;
            let mut carry = 0;
            for i in 0..jz {
                let value = iq[i as usize];
                if carry == 0 {
                    if value != 0 {
                        carry = 1;
                        iq[i as usize] = 0x0100_0000 - value;
                    }
                } else {
                    iq[i as usize] = 0x00ff_ffff - value;
                }
            }
            if q0 > 0 {
                match q0 {
                    1 => iq[(jz - 1) as usize] &= 0x7f_ffff,
                    2 => iq[(jz - 1) as usize] &= 0x3f_ffff,
                    _ => {}
                }
            }
            if ih == 2 {
                z = ONE - z;
                if carry != 0 {
                    z -= libm::scalbn(ONE, q0);
                }
            }
        }
        if z == ZERO {
            let mut any = 0i32;
            let mut i = jz - 1;
            while i >= jk {
                any |= iq[i as usize];
                i -= 1;
            }
            if any == 0 {
                let mut k = 1;
                while jk >= k && iq[(jk - k) as usize] == 0 {
                    k += 1;
                }
                let mut i = jz + 1;
                while i <= jz + k {
                    f[(jx + i) as usize] = f64::from(TWO_OVER_PI[(jv + i) as usize]);
                    let mut fw = 0.0;
                    for j in 0..=jx {
                        fw += x[j as usize] * f[(jx + i - j) as usize];
                    }
                    q[i as usize] = fw;
                    i += 1;
                }
                jz += k;
                continue 'recompute;
            }
        }
        // 去掉零项。
        if z == 0.0 {
            jz -= 1;
            q0 -= 24;
            while iq[jz as usize] == 0 {
                jz -= 1;
                q0 -= 24;
            }
        } else {
            z = libm::scalbn(z, -q0);
            if z >= TWO24 {
                let fw = (TWON24 * z) as i32;
                iq[jz as usize] = (z - TWO24 * f64::from(fw)) as i32;
                jz += 1;
                q0 += 24;
                iq[jz as usize] = fw;
            } else {
                iq[jz as usize] = z as i32;
            }
        }
        let mut fw = libm::scalbn(ONE, q0);
        let mut i = jz;
        while i >= 0 {
            q[i as usize] = fw * f64::from(iq[i as usize]);
            fw *= TWON24;
            i -= 1;
        }
        i = jz;
        while i >= 0 {
            fw = 0.0;
            let mut k = 0;
            while k <= jp && k <= jz - i {
                fw += PIO2[k as usize] * q[(i + k) as usize];
                k += 1;
            }
            fq[(jz - i) as usize] = fw;
            i -= 1;
        }
        // `prec == 2`：拆成高/低两个双精度数。
        fw = 0.0;
        let mut i = jz;
        while i >= 0 {
            fw += fq[i as usize];
            i -= 1;
        }
        let y0 = if ih == 0 { fw } else { -fw };
        fw = fq[0] - fw;
        let mut i = 1;
        while i <= jz {
            fw += fq[i as usize];
            i += 1;
        }
        let y1 = if ih == 0 { fw } else { -fw };
        return (n & 7, y0, y1);
    }
}

/// fdlibm `__kernel_sin`。
fn kernel_sin(x: f64, y: f64, iy: i32) -> f64 {
    const HALF: f64 = 0.5;
    const S1: f64 = -0.16666666666666632;
    const S2: f64 = 0.00833333333332249;
    const S3: f64 = -0.0001984126982985795;
    const S4: f64 = 2.7557313707070068e-06;
    const S5: f64 = -2.5050760253406863e-08;
    const S6: f64 = 1.58969099521155e-10;
    let ix = high_word(x) & 0x7fff_ffff;
    if ix < 0x3e40_0000 && (x as i32) == 0 {
        return x;
    }
    let z = x * x;
    let v = z * x;
    let r = S2 + z * (S3 + z * (S4 + z * (S5 + z * S6)));
    if iy == 0 {
        x + v * (S1 + z * r)
    } else {
        x - ((z * (HALF * y - v * r) - y) - v * S1)
    }
}

/// fdlibm `__kernel_cos`。
fn kernel_cos(x: f64, y: f64) -> f64 {
    const ONE: f64 = 1.0;
    const C1: f64 = 0.0416666666666666;
    const C2: f64 = -0.001388888888887411;
    const C3: f64 = 2.480158728947673e-05;
    const C4: f64 = -2.7557314351390663e-07;
    const C5: f64 = 2.087572321298175e-09;
    const C6: f64 = -1.1359647557788195e-11;
    let ix = high_word(x) & 0x7fff_ffff;
    if ix < 0x3e40_0000 && (x as i32) == 0 {
        return ONE;
    }
    let z = x * x;
    let r = z * (C1 + z * (C2 + z * (C3 + z * (C4 + z * (C5 + z * C6)))));
    if ix < 0x3fd3_3333 {
        ONE - (0.5 * z - (z * r - x * y))
    } else {
        let qx = if ix > 0x3fe9_0000 {
            0.28125
        } else {
            insert_words(ix - 0x0020_0000, 0)
        };
        let iz = 0.5 * z - qx;
        let a = ONE - qx;
        a - (iz - (z * r - x * y))
    }
}

fn clamp01(value: f64) -> f64 {
    // TS `value < 0 ? 0 : value > 1 ? 1 : value`；`f64::clamp` 的语义完全相同
    // （-0.0 与 NaN 都原样返回）。
    value.clamp(0.0, 1.0)
}

/// 五次平滑（quintic smoothstep）。
pub fn smooth(value: f64) -> f64 {
    let t = clamp01(value);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// `easeOut`。
pub fn ease_out(value: f64) -> f64 {
    let t = 1.0 - clamp01(value);
    1.0 - t * t * t
}

/// 逐分量线性插值。
pub fn mix(a: Rgb, b: Rgb, amount: f64) -> Rgb {
    [
        a[0] + (b[0] - a[0]) * amount,
        a[1] + (b[1] - a[1]) * amount,
        a[2] + (b[2] - a[2]) * amount,
    ]
}

/// `Math.hypot`（V8：除以最大值后求平方和，再乘回最大值）。
pub fn js_hypot2(a: f64, b: f64) -> f64 {
    js_hypot(&[a, b])
}

/// `Math.hypot(a, b, c)`。
pub fn js_hypot3(a: f64, b: f64, c: f64) -> f64 {
    js_hypot(&[a, b, c])
}

fn js_hypot(values: &[f64]) -> f64 {
    let mut max = 0.0f64;
    let mut nan = false;
    for &value in values {
        let abs = value.abs();
        if abs.is_nan() {
            nan = true;
        }
        if abs > max {
            max = abs;
        }
    }
    if nan {
        return f64::NAN;
    }
    if max == 0.0 {
        return 0.0;
    }
    if !max.is_finite() {
        return f64::INFINITY;
    }
    let mut sum = 0.0;
    for &value in values {
        let scaled = value / max;
        sum += scaled * scaled;
    }
    max * sum.sqrt()
}

/// 按相对亮度判断背景是否浅色。
pub fn is_light([r, g, b]: Rgb) -> bool {
    0.2126 * r + 0.7152 * g + 0.0722 * b > 128.0
}

/// `toRgb(color)`：`Color` → f64 三元组（`colorToRgb` 的通道是 0..255）。
pub fn to_rgb(color: kiana_tui::colors::Color) -> Rgb {
    let rgb = color_to_rgb(color);
    [f64::from(rgb.r), f64::from(rgb.g), f64::from(rgb.b)]
}

/// braille dots a glyph turns into（粗略的「墨水」量）。
///
/// 空白字素返回 0；单个标点（点、逗号、冒号、分号、引号、反引号、连字符、下划线、中点）
/// 返回 2；单个制表/框线字符（U+2500–U+257F）返回 3；其余返回 `4 + floor(seed * 3)`，
/// 即 4–6 个点（`seed` 预期在 0..1）。只检查
/// `text` 的第一个字符且要求它独占整段文本，多字素文本按普通字符处理。该值决定该格在
/// 消散时最多闪出几个盲文点。
pub fn glyph_ink(text: &str, seed: f64) -> usize {
    if text.trim().is_empty() {
        return 0;
    }
    let mut chars = text.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        if matches!(ch, '.' | ',' | ':' | ';' | '\'' | '`' | '-' | '_' | '·') {
            return 2;
        }
        if ('\u{2500}'..='\u{257f}').contains(&ch) {
            return 3;
        }
    }
    4 + js_floor_to_usize(seed * 3.0)
}

fn js_floor_to_usize(value: f64) -> usize {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        value.floor() as usize
    }
}

// ---------------------------------------------------------------------------
// 屏幕解析
// ---------------------------------------------------------------------------

/// 解析后的一格（TS `interface ScreenCell`）。
#[derive(Debug, Clone)]
pub struct ScreenCell {
    /// 字素；宽字符的伴随单元是 `""`。
    pub text: String,
    /// 1 或 2；宽字符第二列是 0。
    pub width: usize,
    /// 前景色（已解析；默认前景在解析时填入）。
    pub fg: Rgb,
    /// 背景色；未设置表示终端默认背景。
    pub bg: Option<Rgb>,
    /// 变成盲文尘埃前的秒数。
    pub delay: f64,
    /// 变成的盲文点数。
    pub ink: usize,
    /// 抖动种子。
    pub seed: i64,
}

/// `parseScreen(lines, width, foreground, background)`：把带 SGR 的抓屏行解析成格网格。
///
/// 输入 `lines` 是切换全屏前抓下来的终端行，可能包含 `\x1b[...m` 颜色序列；`width` 是当前
/// 终端列数，超出的字素被丢弃。输出与 `lines` 等长，每个元素是该行的 [`ScreenCell`]：
///
/// * 字素按 `kiana-tui` 的 segmenter 切分；宽字符占两列，第二列是 `text == ""`、
///   `width == 0` 的伴随单元（`prepare_cells` 靠它把两列当成同一个字素处理）。
/// * 没有任何颜色设置时 `fg` 填入 `foreground`；`bg == None` 表示终端默认背景。`dim`（SGR 2）
///   把前景向背景混合 40%，`inverse`（SGR 7）交换前景/背景。
/// * 这里只填 `text/width/fg/bg`，`delay/ink/seed` 保持 0，由 `Inner::prepare_cells` 计算。
///
/// 只认识 `parse_screen` 实际需要的 SGR 子集（`apply_sgr` 列出的代码）；其它 CSI 被跳过。
/// OSC/APC/DCS/PM 字符串序列一直跳到 BEL 或 ST，未闭合时该行剩余内容被忽略。索引色越界
/// 会在 `palette` 中 panic（与 TS 的 `indexedColor` 抛错一致，见该函数说明）。
pub fn parse_screen(
    lines: &[String],
    width: usize,
    foreground: Rgb,
    background: Rgb,
) -> Vec<Vec<ScreenCell>> {
    lines
        .iter()
        .map(|line| {
            let mut cells: Vec<ScreenCell> = Vec::new();
            let mut fg: Option<Rgb> = None;
            let mut bg: Option<Rgb> = None;
            let mut dim = false;
            let mut inverse = false;
            let bytes = line.as_bytes();
            let mut i = 0usize;
            while i < bytes.len() {
                let sequence_start = match line[i..].find('\x1b') {
                    Some(offset) => i + offset,
                    None => {
                        push_text(
                            &mut cells,
                            &line[i..],
                            width,
                            fg,
                            bg,
                            dim,
                            inverse,
                            foreground,
                            background,
                        );
                        break;
                    }
                };
                if sequence_start > i {
                    push_text(
                        &mut cells,
                        &line[i..sequence_start],
                        width,
                        fg,
                        bg,
                        dim,
                        inverse,
                        foreground,
                        background,
                    );
                }
                let kind = bytes.get(sequence_start + 1).copied();
                if kind == Some(b'[') {
                    let mut end = sequence_start + 2;
                    while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                        end += 1;
                    }
                    if bytes.get(end) == Some(&b'm') {
                        let params = line[sequence_start + 2..end].to_string();
                        apply_sgr(&params, &mut fg, &mut bg, &mut dim, &mut inverse);
                    }
                    i = end + 1;
                } else if matches!(kind, Some(b']') | Some(b'_') | Some(b'P') | Some(b'^')) {
                    // 字符串序列以 BEL 或 ST（ESC \）结束。
                    let bell = line[sequence_start + 2..]
                        .find('\x07')
                        .map(|offset| sequence_start + 2 + offset);
                    let st = line[sequence_start + 2..]
                        .find("\x1b\\")
                        .map(|offset| sequence_start + 2 + offset);
                    match (bell, st) {
                        (None, None) => break,
                        (Some(bell), Some(st)) => {
                            i = if bell < st { bell + 1 } else { st + 2 };
                        }
                        (Some(bell), None) => i = bell + 1,
                        (None, Some(st)) => i = st + 2,
                    }
                } else {
                    // TS `i = sequenceStart + 2` 按 UTF-16 单元前进：跳过 ESC 与紧随其后的
                    // 一个单元。Rust 的 `i` 是字节下标，ESC 之后可能是 2–4 字节的字符，
                    // 直接 `+2` 会落在字符中间，下一轮 `&line[i..]` 直接 panic。这里跳到
                    // 下一个 char 边界（跳过 ESC 后的**一个字符**）：
                    // * BMP 字符（é、你）恰好是一个 char，跳过它即正确；
                    // * 4 字节 emoji 在 TS 里只跳过高代理项，剩下的低代理项是不可见字素
                    //   （`visibleWidth === 0`），会被 `pushText` 丢弃，因此结果同样是
                    //   「跳过 ESC + 整个 emoji」（见
                    //   `parse_screen_skips_esc_before_multibyte_characters` 测试）。
                    let next = line[sequence_start + 1..].chars().next();
                    i = sequence_start + 1 + next.map_or(1, char::len_utf8);
                }
            }
            cells
        })
        .collect()
}

/// 把一段无转义文本按字素写入 `cells`，并应用当前的 SGR 状态。
///
/// 零宽字素（组合符、变体选择符等）直接丢弃；放不下（`cells.len() + 字素宽度 > width`）的
/// 字素也丢弃，因此一行末尾可能少于 `width` 格。宽字符会额外 push 一个 `width == 0`
/// 的伴随单元，其 `text` 为空——渲染循环遇到它时按「已由前一列消费」跳过。
#[allow(clippy::too_many_arguments)]
fn push_text(
    cells: &mut Vec<ScreenCell>,
    text: &str,
    width: usize,
    fg: Option<Rgb>,
    bg: Option<Rgb>,
    dim: bool,
    inverse: bool,
    foreground: Rgb,
    background: Rgb,
) {
    for segment in get_grapheme_segmenter().segment(text) {
        let glyph_width = visible_width(segment.segment);
        if glyph_width == 0 || cells.len() + glyph_width > width {
            continue;
        }
        let mut cell_fg = fg.unwrap_or(foreground);
        let mut cell_bg = bg;
        if inverse {
            cell_fg = bg.unwrap_or(background);
            cell_bg = Some(fg.unwrap_or(foreground));
        }
        if dim {
            cell_fg = mix(cell_fg, cell_bg.unwrap_or(background), 0.4);
        }
        let cell = ScreenCell {
            text: segment.segment.to_string(),
            width: glyph_width,
            fg: cell_fg,
            bg: cell_bg,
            delay: 0.0,
            ink: 0,
            seed: 0,
        };
        cells.push(cell.clone());
        if glyph_width == 2 {
            cells.push(ScreenCell {
                text: String::new(),
                width: 0,
                ..cell
            });
        }
    }
}

/// TS `palette(index) = toRgb(indexedColor(index))`。
///
/// `indexedColor` 对非整数/越界索引**抛出** `ANSI color index must be an integer from 0 to
/// 255: <index>`（`parseScreen` 里没有捕获），因此 `parseScreen`/`render` 会以同样文案失败。
/// Rust 的 [`indexed_color`] 只接受 `u8`，在这里复刻抛出行为（panic 的消息与 TS 的
/// `Error.message` 相同）。
fn palette(index: f64) -> Option<Rgb> {
    if !index.is_finite() || index < 0.0 || index > 255.0 || index.fract() != 0.0 {
        panic!(
            "ANSI color index must be an integer from 0 to 255: {}",
            kiana_json::to_radix_string(index, 10)
        );
    }
    Some(to_rgb(indexed_color(index as u8)))
}

fn js_parse_int(part: &str) -> Option<f64> {
    let trimmed = part.trim_start();
    let bytes = trimmed.as_bytes();
    let mut index = 0usize;
    let mut sign = 1.0;
    if let Some(&byte) = bytes.first() {
        if byte == b'+' {
            index += 1;
        } else if byte == b'-' {
            sign = -1.0;
            index += 1;
        }
    }
    let start = index;
    let mut value = 0.0f64;
    while let Some(&byte) = bytes.get(index) {
        if !byte.is_ascii_digit() {
            break;
        }
        value = value * 10.0 + f64::from(byte - b'0');
        index += 1;
    }
    if index == start {
        return None;
    }
    Some(sign * value)
}

/// 应用一条 SGR 的参数列表（`\x1b[` 与 `m` 之间的部分），就地更新颜色与修饰状态。
///
/// 支持 `38/48` 扩展颜色（`;` 与 `:` 两种分隔）、基本/亮色 30–37、90–97、40–47、100–107，
/// 以及 `0/2/22/7/27/39/49`。空参数等价于 `0`（重置）。不认识的代码被忽略。
/// 这里刻意逐条模拟 TS 的数组切片行为：分号形态的参数个数由「类型」决定且**不补齐**，
/// 这样截断序列（如 `38;2;1;2`）会像 TS 一样被忽略，而不是得到含 NaN 的颜色。
fn apply_sgr(
    params: &str,
    fg: &mut Option<Rgb>,
    bg: &mut Option<Rgb>,
    dim: &mut bool,
    inverse: &mut bool,
) {
    let owned;
    let codes: Vec<&str> = if params.is_empty() {
        owned = vec!["0"];
        owned
    } else {
        params.split(';').collect()
    };
    let mut i = 0usize;
    while i < codes.len() {
        let parts: Vec<Option<f64>> = codes[i].split(':').map(js_parse_int).collect();
        let code = parts.first().copied().flatten().unwrap_or(f64::NAN);
        if code == 38.0 || code == 48.0 {
            // 扩展颜色：`38;5;n` / `38;2;r;g;b`，或冒号子参数形态。
            let args: Vec<Option<f64>>;
            if parts.len() > 1 {
                args = parts[1..]
                    .iter()
                    .copied()
                    .filter(|part| part.is_some())
                    .collect();
                // `38:2::r:g:b` 的空色彩空间 id 已被过滤。
            } else {
                let kind = js_parse_int(codes.get(i + 1).copied().unwrap_or(""));
                let count = if kind == Some(5.0) {
                    2
                } else if kind == Some(2.0) {
                    4
                } else {
                    1
                };
                // TS `codes.slice(i + 1, i + 1 + count)`：越界时返回**较短的数组**。
                // 数组长度参与后面的 `args.length >= 4` 判定，所以这里不能补齐到 count 个
                // （否则 `38;2;1;2` 这种截断序列会得到一个含 NaN 的颜色，而 TS 忽略它）。
                args = (i + 1..i + 1 + count)
                    .filter(|j| *j < codes.len())
                    .map(|j| js_parse_int(codes[j]))
                    .collect();
                i += count;
            }
            let mut color: Option<Rgb> = None;
            let first = args.first().copied().flatten();
            // TS 判断的是 `args[1] !== undefined`（数组长度），不是「是数字」：
            // `38;5;` 的第二个参数是 NaN，`palette(NaN)` 会抛出（不是忽略）。
            if first == Some(5.0) && args.len() > 1 {
                color = palette(args[1].unwrap_or(f64::NAN));
            } else if first == Some(2.0) && args.len() >= 4 {
                // TS 在分号形态里不检查 NaN：`[38;2;x;5;5m` 会得到 NaN 通道并一路传播。
                color = Some([
                    args[1].unwrap_or(f64::NAN),
                    args[2].unwrap_or(f64::NAN),
                    args[3].unwrap_or(f64::NAN),
                ]);
            }
            if let Some(color) = color {
                if code == 38.0 {
                    *fg = Some(color);
                } else {
                    *bg = Some(color);
                }
            }
        } else if code == 0.0 {
            *fg = None;
            *bg = None;
            *dim = false;
            *inverse = false;
        } else if code == 2.0 {
            *dim = true;
        } else if code == 22.0 {
            *dim = false;
        } else if code == 7.0 {
            *inverse = true;
        } else if code == 27.0 {
            *inverse = false;
        } else if (30.0..=37.0).contains(&code) {
            *fg = palette(code - 30.0);
        } else if (90.0..=97.0).contains(&code) {
            *fg = palette(code - 90.0 + 8.0);
        } else if code == 39.0 {
            *fg = None;
        } else if (40.0..=47.0).contains(&code) {
            *bg = palette(code - 40.0);
        } else if (100.0..=107.0).contains(&code) {
            *bg = palette(code - 100.0 + 8.0);
        } else if code == 49.0 {
            *bg = None;
        }
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// 旋转与自转
// ---------------------------------------------------------------------------

/// 行主序 3x3：yaw（y）→ pitch（x）→ roll（z）的 `Rz * Rx * Ry`。
///
/// 三个角都是弧度；返回数组按 `m[row * 3 + column]` 索引，既用于把模型变换到相机空间
/// （光栅化的射线方向），也用于取相机空间法线（矩阵的列）。乘法顺序固定，改顺序会
/// 改变旋转姿态，破坏逐帧对拍。
pub fn rotation(yaw: f64, pitch: f64, roll: f64) -> [f64; 9] {
    // V8 的 Math.sin/cos 是 fdlibm 端口；std 的实现在部分输入上差 1 ulp，用 libm 逐位对齐。
    let sy = js_sin(yaw);
    let cy = js_cos(yaw);
    let sx = js_sin(pitch);
    let cx = js_cos(pitch);
    let sz = js_sin(roll);
    let cz = js_cos(roll);
    let ry = [cy, 0.0, sy, 0.0, 1.0, 0.0, -sy, 0.0, cy];
    let rx = [1.0, 0.0, 0.0, 0.0, cx, -sx, 0.0, sx, cx];
    let rz = [cz, -sz, 0.0, sz, cz, 0.0, 0.0, 0.0, 1.0];
    multiply3(&rz, &multiply3(&rx, &ry))
}

/// 3x3 乘法（行主序）。
pub fn multiply3(a: &[f64; 9], b: &[f64; 9]) -> [f64; 9] {
    let mut result = [0.0f64; 9];
    for row in 0..3 {
        for column in 0..3 {
            result[row * 3 + column] = a[row * 3] * b[column]
                + a[row * 3 + 1] * b[3 + column]
                + a[row * 3 + 2] * b[6 + column];
        }
    }
    result
}

/// `baseSpinPhase(time)`：从静止加速 `SPIN_RAMP` 秒后匀速，返回累积相位（弧度）。
///
/// `time` 与其它时间参数一样是**动画内秒数**（从 `start_time` 起算），不是墙钟毫秒。
/// 前 `SPIN_RAMP` 秒用 `u^6 - 3u^5 + 2.5u^4` 平滑起步（两端位置与速度连续），之后按
/// `SPIN_SPEED` 线性增长。`spin_alignment` 用它把第一个正面时刻对齐到整圈。
pub fn base_spin_phase(time: f64) -> f64 {
    let elapsed = time - FLY_START;
    if elapsed <= 0.0 {
        return 0.0;
    }
    let u = elapsed / SPIN_RAMP;
    if u < 1.0 {
        SPIN_SPEED
            * SPIN_RAMP
            * (libm::pow(u, 6.0) - 3.0 * libm::pow(u, 5.0) + 2.5 * libm::pow(u, 4.0))
    } else {
        SPIN_SPEED * (SPIN_RAMP * 0.5 + elapsed - SPIN_RAMP)
    }
}

/// `SPIN_ALIGNMENT`：每次重组正面时相位都是整数圈。
fn spin_alignment() -> f64 {
    std::f64::consts::PI * 2.0 - (base_spin_phase(FIRST_FRONT_VIEW) % (std::f64::consts::PI * 2.0))
}

/// `spinPhase(time)`：基础相位加上对齐修正，让第一次「正面」落在整圈上。
///
/// 修正项按 `smooth((time - FLY_START) / FLY_DURATION)` 淡入，避免飞入阶段突然跳相位；
/// 返回值仍是弧度，供 [`spin_angle`] 与 `pose` 的 pitch/roll 使用。
pub fn spin_phase(time: f64) -> f64 {
    base_spin_phase(time) + spin_alignment() * smooth((time - FLY_START) / FLY_DURATION)
}

/// `spinAngle(time)`：相位在整圈处停留，让 logo 从正面被看清。
pub fn spin_angle(time: f64) -> f64 {
    let phase = spin_phase(time);
    phase - SPIN_LINGER * js_sin(phase)
}

/// 光照方向（模块加载时算一次，f64）。
pub fn light_vector() -> [f64; 3] {
    static LIGHT: std::sync::OnceLock<[f64; 3]> = std::sync::OnceLock::new();
    *LIGHT.get_or_init(|| normalize([-0.45, -0.6, 0.75]))
}

/// 视线与光线的半向量。
pub fn half_vector() -> [f64; 3] {
    static HALF: std::sync::OnceLock<[f64; 3]> = std::sync::OnceLock::new();
    *HALF.get_or_init(|| {
        let light = light_vector();
        normalize([light[0], light[1], light[2] + 1.0])
    })
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let length = js_hypot3(v[0], v[1], v[2]);
    [v[0] / length, v[1] / length, v[2] / length]
}

// ---------------------------------------------------------------------------
// 拼图
// ---------------------------------------------------------------------------

/// 一次洗牌周期内每步之后的块位置（含起始位置；`steps.len() == PUZZLE_STEPS + 1`）。
/// 每个周期是确定性的（随机数来自 `hash`）。
///
/// 伪随机序列从 `cycle * 7919 + 1` 开始，每次调用 `next()` 返回 `hash(random)` 后把
/// `random` 加 1（不是上一轮的哈希值）。每一步：
///
/// 1. 用 `model.puzzle_moves(next())` 决定本步移动几块（见 [`PuzzleKind`]）。
/// 2. 对每个尚未在本步移动过的块，尝试 `PUZZLE_MOVES` 的 10 个方向；目标必须落在
///    `0..columns` × `0..rows`、z ∈ {-1, 0, 1}，且不得占据已被占用的格。
/// 3. 优先从「上一步没动过」的候选块中选，全部动过时退回全部候选；空候选则提前结束本轮
///    洗牌（后续步不再生成，`steps` 可短于 `PUZZLE_STEPS + 1`，`block_offsets` 取快照时会
///    钳制下标）。
///
/// 返回的每一帧都是**位置快照**（`Vec<Cell3>` 与 `model.blocks` 同序），渲染时再减去
/// `home` 得到块偏移。`cycle` 由 `Inner::block_offsets` 根据动画时间推导，同一周期会命中
/// 缓存，只有进入下一周期才重新计算。
pub fn shuffle_steps(model: &Model, cycle: i64) -> Vec<Vec<Cell3>> {
    let columns = model.columns as i64;
    let rows = model.rows as i64;
    let mut positions: Vec<Cell3> = model.blocks.iter().map(|block| block.home).collect();
    let mut steps = vec![positions.clone()];
    let mut last_moved: HashSet<usize> = HashSet::new();
    let mut random = (cycle * 7_919 + 1) as f64;
    let mut next = || {
        let value = hash(random);
        random += 1.0;
        value
    };
    let key = |position: &Cell3| ((position[2] + 1) * rows + position[1]) * columns + position[0];
    for _ in 0..PUZZLE_STEPS {
        let mut occupied: HashSet<i64> = positions.iter().map(key).collect();
        let mut next_positions = positions.clone();
        let mut moved: HashSet<usize> = HashSet::new();
        let move_count = model.puzzle_moves(next());
        for _ in 0..move_count {
            let mut candidates: Vec<(usize, Cell3)> = Vec::new();
            for (block, position) in positions.iter().enumerate() {
                if moved.contains(&block) {
                    continue;
                }
                for delta in PUZZLE_MOVES {
                    let target = [
                        position[0] + delta[0],
                        position[1] + delta[1],
                        position[2] + delta[2],
                    ];
                    if target[0] < 0 || target[0] >= columns || target[1] < 0 || target[1] >= rows {
                        continue;
                    }
                    if target[2] < -1 || target[2] > 1 || occupied.contains(&key(&target)) {
                        continue;
                    }
                    candidates.push((block, target));
                }
            }
            // 优先选刚没动过的块，避免看起来只有一个块在抖。
            let fresh: Vec<(usize, Cell3)> = candidates
                .iter()
                .copied()
                .filter(|(block, _)| !last_moved.contains(block))
                .collect();
            let pool = if fresh.is_empty() { candidates } else { fresh };
            if pool.is_empty() {
                break;
            }
            let choice = (next() * pool.len() as f64).floor() as usize;
            let (block, target) = pool[choice.min(pool.len() - 1)];
            occupied.insert(key(&target));
            next_positions[block] = target;
            moved.insert(block);
        }
        last_moved.clear();
        last_moved.extend(moved);
        positions = next_positions;
        steps.push(positions.clone());
    }
    steps
}

// ---------------------------------------------------------------------------
// BlockRaster
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Face {
    axis: usize,
    plane: f64,
    u_min: f64,
    u_max: f64,
    v_min: f64,
    v_max: f64,
    min_x: i64,
    min_y: i64,
    max_x: i64,
    max_y: i64,
    red: f64,
    green: f64,
    blue: f64,
}

#[derive(Debug, Clone, Copy)]
struct Rect {
    min_x: i64,
    min_y: i64,
    max_x: i64,
    max_y: i64,
}

/// 一个方块（TS `interface Box`）：模型空间里的轴对齐盒子。
///
/// 坐标单位是模型像素：`min`/`max` 由块的 `home` 减去模型半宽/半高并加上拼图偏移得到，
/// z 方向厚度为 `DEPTH`。`color` 是未打光的原始颜色，光照在 `visible_faces` 中按面计算。
#[derive(Debug, Clone, Copy)]
pub struct Box3 {
    /// 最小角（模型空间）。
    pub min: [f64; 3],
    /// 最大角（模型空间）。
    pub max: [f64; 3],
    /// 未打光的原始颜色。
    pub color: Rgb,
}

/// 姿态（TS `interface Pose`）：模型 → 屏幕的相机参数。
///
/// 坐标是 dot 空间（1 终端格 = 2x4 个 dot），所以最终 `center_x` 约等于终端列数、
/// `center_y` 约等于行数的两倍（`pose` 里 `end_x = width`、`end_y = height * 2 - 2`）。
/// 三个旋转角都是**弧度**，按 yaw→pitch→roll 的顺序合成（见 [`rotation`]）。
#[derive(Debug, Clone, Copy)]
pub struct Pose {
    /// 中心 x（dot 空间）。
    pub center_x: f64,
    /// 中心 y（dot 空间）。
    pub center_y: f64,
    /// 深度 0 处每个模型像素的盲文点数。
    pub scale: f64,
    /// 偏航（弧度）。
    pub yaw: f64,
    /// 俯仰（弧度）。
    pub pitch: f64,
    /// 翻滚（弧度）。
    pub roll: f64,
}

/// 把模型的块光栅化进盲文单元。只画朝向相机、且没有被相邻块完全遮住的面；
/// 每个面在投影包围盒内逐点做射线-平面求交，深度缓冲解决遮挡。缓冲跨帧复用。
pub struct BlockRaster {
    /// 每格的盲文点位（OR 累积）。
    pub bits: Vec<u8>,
    /// 每格点亮的点数。
    pub counts: Vec<u8>,
    /// 每格已点亮点的 RGB **和**（f32，逐次写入舍入）。
    pub rgb: Vec<f32>,
    /// 每格的光晕强度 0..1（只有请求光晕时才写）。
    pub halo_amount: Vec<f32>,
    /// 每格的光晕颜色。
    pub halo_rgb: Vec<f32>,
    width: usize,
    height: usize,
    /// 每个 dot 的最近命中射线参数；越小越近。
    depth: Vec<f32>,
    /// 每个 dot 的最近命中面下标 + 1（0 = 无）。
    face_ids: Vec<u16>,
    /// 上一帧写过的格包围盒（下一帧先清它）。
    dirty: Option<Rect>,
    /// 上一帧有光晕的格包围盒。
    halo_dirty: Option<Rect>,
    camera_distance: f64,
}

impl BlockRaster {
    /// `new BlockRaster(cameraDistance)`：创建空光栅；缓冲在第一次 [`Self::render`] 时按尺寸分配。
    ///
    /// `camera_distance` 应与所渲染 [`Model`] 的 `camera_distance` 一致，否则透视与深度
    /// 雾化都和 `pose` 的缩放假设不匹配（`EasterEgg3dAnimation::new` 直接取模型的该字段）。
    pub fn new(camera_distance: f64) -> Self {
        BlockRaster {
            bits: Vec::new(),
            counts: Vec::new(),
            rgb: Vec::new(),
            halo_amount: Vec::new(),
            halo_rgb: Vec::new(),
            width: 0,
            height: 0,
            depth: Vec::new(),
            face_ids: Vec::new(),
            dirty: None,
            halo_dirty: None,
            camera_distance,
        }
    }

    /// `render(width, height, pose, boxes, background, haloStrength)`：把 `boxes` 光栅化成本帧。
    ///
    /// * `width`/`height` 是终端**格**数；内部按 2x4 换算成 dot 缓冲。
    /// * `pose` 提供中心、缩放与旋转；`background` 参与远处块的雾化，并决定光照采用亮色还是
    ///   暗色背景分支（`visible_faces` 的 `light_background`）。
    /// * `halo_strength > 0` 时额外生成光晕；调用方只在浅色背景传非零值。
    ///
    /// 缓冲跨帧复用：尺寸变化时重新分配并清空全部缓冲；尺寸不变时只清上一帧记录的
    /// `dirty`/`halo_dirty` 矩形。函数结束时不重置 `dirty`，它记录的是**本帧**画过的区域，
    /// 留给下一帧清理。若没有任何可见面，函数提前返回，`bits` 等保持已清空状态。
    pub fn render(
        &mut self,
        width: usize,
        height: usize,
        pose: &Pose,
        boxes: &[Box3],
        background: Rgb,
        halo_strength: f64,
    ) {
        // 阶段 1：尺寸变化时重建所有缓冲；否则只清上一帧写过的矩形。`halo_dirty` 与
        // `dirty` 分开清，因为光晕覆盖的格子可能比模型本身更大。
        let dot_width = width * 2;
        let dot_height = height * 4;
        if width != self.width || height != self.height {
            self.width = width;
            self.height = height;
            self.bits = vec![0; width * height];
            self.counts = vec![0; width * height];
            self.rgb = vec![0.0; width * height * 3];
            self.halo_amount = vec![0.0; width * height];
            self.halo_rgb = vec![0.0; width * height * 3];
            self.halo_dirty = None;
            self.depth = vec![f32::INFINITY; dot_width * dot_height];
            self.face_ids = vec![0; dot_width * dot_height];
            self.dirty = None;
        } else if let Some(dirty) = self.dirty {
            for row in dirty.min_y..=dirty.max_y {
                let start = (row * width as i64 + dirty.min_x) as usize;
                let end = (row * width as i64 + dirty.max_x + 1) as usize;
                self.bits[start..end].fill(0);
                self.counts[start..end].fill(0);
                self.rgb[start * 3..end * 3].fill(0.0);
            }
            self.dirty = None;
        }
        if let Some(dirty) = self.halo_dirty {
            for row in dirty.min_y..=dirty.max_y {
                let start = (row * width as i64 + dirty.min_x) as usize;
                let end = (row * width as i64 + dirty.max_x + 1) as usize;
                self.halo_amount[start..end].fill(0.0);
                self.halo_rgb[start * 3..end * 3].fill(0.0);
            }
            self.halo_dirty = None;
        }

        // 阶段 2：收集可见面。矩阵 `m` 是「模型 → 相机」的旋转；相机在相机空间的
        // (0, 0, cameraDistance)，乘上转置旋转（即取 m 的第三行，也就是转置矩阵 m^T 的
        // 第三列）得到物体空间的原点。
        let m = rotation(pose.yaw, pose.pitch, pose.roll);
        let center_x = pose.center_x;
        let center_y = pose.center_y;
        let scale = pose.scale;
        let camera_distance = self.camera_distance;
        // 相机在相机空间的 (0, 0, cameraDistance)。`rotation()` 按行主序存 m[row*3+column]，
        // R^T·(0,0,d) 等于 m 的第三行（即转置矩阵 m^T 的第三列），所以物体空间原点
        // 就是第三行乘 camera_distance。
        let origin = [
            m[6] * camera_distance,
            m[7] * camera_distance,
            m[8] * camera_distance,
        ];
        let light = is_light(background);
        let faces = self.visible_faces(&m, &origin, pose, boxes, dot_width, dot_height, light);
        if faces.is_empty() {
            return;
        }
        let mut min_x = dot_width as i64;
        let mut min_y = dot_height as i64;
        let mut max_x: i64 = -1;
        let mut max_y: i64 = -1;
        for face in &faces {
            min_x = min_x.min(face.min_x);
            min_y = min_y.min(face.min_y);
            max_x = max_x.max(face.max_x);
            max_y = max_y.max(face.max_y);
        }
        if max_x < min_x || max_y < min_y {
            return;
        }

        // 阶段 3：逐面逐 dot 做射线-平面求交，深度测试后把「最近面」记到 face_ids。
        // 每个 face 只在自己的投影包围盒内测试；射线方向对相邻 dot 是线性的，所以内层
        // 循环用加法步进而非每次重新算投影。
        let depth = &mut self.depth;
        let face_ids = &mut self.face_ids;
        for (face_index, face) in faces.iter().enumerate() {
            let a = face.axis;
            let u = (a + 1) % 3;
            let v = (a + 2) % 3;
            let origin_a = origin[a];
            let origin_u = origin[u];
            let origin_v = origin[v];
            // 物体空间的射线方向对 dot 位置是线性的，所以逐点加法步进。
            let step_a = m[a] / scale;
            let step_u = m[u] / scale;
            let step_v = m[v] / scale;
            let sx = (face.min_x as f64 + 0.5 - center_x) / scale;
            for dot_y in face.min_y..=face.max_y {
                let sy = (dot_y as f64 + 0.5 - center_y) / scale;
                let mut direction_a = m[a] * sx + m[3 + a] * sy - m[6 + a] * camera_distance;
                let mut direction_u = m[u] * sx + m[3 + u] * sy - m[6 + u] * camera_distance;
                let mut direction_v = m[v] * sx + m[3 + v] * sy - m[6 + v] * camera_distance;
                let first_index = dot_y as usize * dot_width + face.min_x as usize;
                let dot_count = (face.max_x - face.min_x + 1) as usize;
                for index in first_index..first_index + dot_count {
                    let t = (face.plane - origin_a) / direction_a;
                    if t > 0.0 && t < depth[index] as f64 {
                        let hit_u = origin_u + t * direction_u;
                        let hit_v = origin_v + t * direction_v;
                        if hit_u >= face.u_min
                            && hit_u <= face.u_max
                            && hit_v >= face.v_min
                            && hit_v <= face.v_max
                        {
                            depth[index] = t as f32;
                            face_ids[index] = (face_index + 1) as u16;
                        }
                    }
                    direction_a += step_a;
                    direction_u += step_u;
                    direction_v += step_v;
                }
            }
        }

        // 阶段 4：着色、打包进盲文格，并重置 dot 缓冲供下一帧使用。
        // 每个被选中的 dot 贡献一次颜色样本：`rgb` 累加，`counts` 计数，`bits` 按 2x4 位图
        // OR 进所属终端格。远处样本按 `fog` 向背景色混合。这里同时记录本帧的格包围盒。
        let mut cell_min_x = width as i64;
        let mut cell_min_y = height as i64;
        let mut cell_max_x: i64 = -1;
        let mut cell_max_y: i64 = -1;
        for dot_y in min_y..=max_y {
            let mut index = dot_y as usize * dot_width + min_x as usize;
            for dot_x in min_x..=max_x {
                let id = face_ids[index];
                if id == 0 {
                    index += 1;
                    continue;
                }
                let face = &faces[id as usize - 1];
                // 离相机越远越向背景色雾化，制造深度感。
                let fog = clamp01(0.15 - camera_distance * (1.0 - depth[index] as f64) * 0.12)
                    * if light { 0.5 } else { 1.0 };
                face_ids[index] = 0;
                depth[index] = f32::INFINITY;
                let cell_x = dot_x >> 1;
                let cell_y = dot_y >> 2;
                let cell = (cell_y * width as i64 + cell_x) as usize;
                self.bits[cell] |= DOT_BITS[((dot_y & 3) * 2 + (dot_x & 1)) as usize];
                self.counts[cell] = self.counts[cell].wrapping_add(1);
                self.rgb[cell * 3] = (f64::from(self.rgb[cell * 3])
                    + face.red
                    + (background[0] - face.red) * fog) as f32;
                self.rgb[cell * 3 + 1] = (f64::from(self.rgb[cell * 3 + 1])
                    + face.green
                    + (background[1] - face.green) * fog)
                    as f32;
                self.rgb[cell * 3 + 2] = (f64::from(self.rgb[cell * 3 + 2])
                    + face.blue
                    + (background[2] - face.blue) * fog)
                    as f32;
                if cell_x < cell_min_x {
                    cell_min_x = cell_x;
                }
                if cell_x > cell_max_x {
                    cell_max_x = cell_x;
                }
                if cell_y < cell_min_y {
                    cell_min_y = cell_y;
                }
                if cell_y > cell_max_y {
                    cell_max_y = cell_y;
                }
                index += 1;
            }
        }
        if cell_max_x >= 0 {
            self.dirty = Some(Rect {
                min_x: cell_min_x,
                min_y: cell_min_y,
                max_x: cell_max_x,
                max_y: cell_max_y,
            });
        }
        // 阶段 5：可选光晕。必须在 `dirty` 仍是本帧包围盒时调用（`render_halo` 只扩散
        // 这个区域）；光晕会写入独立的 `halo_*` 缓冲，不改变 `bits/counts/rgb`。
        if halo_strength > 0.0 {
            self.render_halo(halo_strength);
        }
    }

    /// 用可分离的 tent 滤波器把覆盖率与颜色扩散到邻格（浅色背景的 halo）。
    ///
    /// 先水平、再垂直各做一次一维卷积（半径 `HALO_RADIUS_X/Y`），权重和为 1；因此复杂度是
    /// `O(区域 × (2rx+1 + 2ry+1))` 而不是二维卷积的乘积。覆盖率按 `strength * coverage^0.7`
    /// 量化到 `HALO_LEVELS` 级、颜色量化到 `LOGO_COLOR_STEP`，减少相邻格的 ANSI 差异。
    /// 结果只写 `halo_amount/halo_rgb`，并记录 `halo_dirty` 供下一帧清理。
    fn render_halo(&mut self, strength: f64) {
        let Some(cells) = self.dirty else {
            return;
        };
        let width = self.width as i64;
        let height = self.height as i64;
        let min_x = (cells.min_x - HALO_RADIUS_X).max(0);
        let max_x = (cells.max_x + HALO_RADIUS_X).min(width - 1);
        let min_y = (cells.min_y - HALO_RADIUS_Y).max(0);
        let max_y = (cells.max_y + HALO_RADIUS_Y).min(height - 1);
        let region_width = (max_x - min_x + 1) as usize;
        let tent = |radius: i64| -> Vec<f64> {
            let mut weights: Vec<f64> = (0..=radius * 2)
                .map(|i| (radius + 1 - (i - radius).abs()) as f64)
                .collect();
            let total: f64 = weights.iter().sum();
            for weight in &mut weights {
                *weight /= total;
            }
            weights
        };
        let weights_x = tent(HALO_RADIUS_X);
        let weights_y = tent(HALO_RADIUS_Y);

        // 水平 pass：只遍历包含 logo 的行。
        let rows = (cells.max_y - cells.min_y + 1) as usize;
        let mut horizontal = vec![0.0f32; rows * region_width * 4];
        for y in cells.min_y..=cells.max_y {
            for x in min_x..=max_x {
                let mut coverage = 0.0f64;
                let mut red = 0.0f64;
                let mut green = 0.0f64;
                let mut blue = 0.0f64;
                for dx in -HALO_RADIUS_X..=HALO_RADIUS_X {
                    let source_x = x + dx;
                    if source_x < cells.min_x || source_x > cells.max_x {
                        continue;
                    }
                    let source = (y * width + source_x) as usize;
                    let count = self.counts[source];
                    if count == 0 {
                        continue;
                    }
                    let weight = weights_x[(dx + HALO_RADIUS_X) as usize] / 8.0;
                    coverage += f64::from(count) * weight;
                    red += f64::from(self.rgb[source * 3]) * weight;
                    green += f64::from(self.rgb[source * 3 + 1]) * weight;
                    blue += f64::from(self.rgb[source * 3 + 2]) * weight;
                }
                let target = ((y - cells.min_y) as usize * region_width + (x - min_x) as usize) * 4;
                horizontal[target] = coverage as f32;
                horizontal[target + 1] = red as f32;
                horizontal[target + 2] = green as f32;
                horizontal[target + 3] = blue as f32;
            }
        }

        // 垂直 pass 写入 halo 缓冲。
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let mut coverage = 0.0f64;
                let mut red = 0.0f64;
                let mut green = 0.0f64;
                let mut blue = 0.0f64;
                for dy in -HALO_RADIUS_Y..=HALO_RADIUS_Y {
                    let source_y = y + dy;
                    if source_y < cells.min_y || source_y > cells.max_y {
                        continue;
                    }
                    let weight = weights_y[(dy + HALO_RADIUS_Y) as usize];
                    let source = ((source_y - cells.min_y) as usize * region_width
                        + (x - min_x) as usize)
                        * 4;
                    coverage += f64::from(horizontal[source]) * weight;
                    red += f64::from(horizontal[source + 1]) * weight;
                    green += f64::from(horizontal[source + 2]) * weight;
                    blue += f64::from(horizontal[source + 3]) * weight;
                }
                let target = (y * width + x) as usize;
                // logo 下方的格子至少保留自身覆盖率。
                let count = self.counts[target];
                if count > 0 && f64::from(count) / 8.0 > coverage {
                    coverage = f64::from(count) / 8.0;
                    red = (f64::from(self.rgb[target * 3]) / f64::from(count)) * coverage;
                    green = (f64::from(self.rgb[target * 3 + 1]) / f64::from(count)) * coverage;
                    blue = (f64::from(self.rgb[target * 3 + 2]) / f64::from(count)) * coverage;
                }
                if coverage <= 0.0 {
                    continue;
                }
                // 量化，让相邻格通常共享同一个背景与转义序列。
                let amount = js_round(strength * libm::pow(coverage.min(1.0), 0.7) * HALO_LEVELS)
                    / HALO_LEVELS;
                if amount <= 0.0 {
                    continue;
                }
                self.halo_amount[target] = amount as f32;
                self.halo_rgb[target * 3] =
                    (js_round(red / coverage / LOGO_COLOR_STEP) * LOGO_COLOR_STEP) as f32;
                self.halo_rgb[target * 3 + 1] =
                    (js_round(green / coverage / LOGO_COLOR_STEP) * LOGO_COLOR_STEP) as f32;
                self.halo_rgb[target * 3 + 2] =
                    (js_round(blue / coverage / LOGO_COLOR_STEP) * LOGO_COLOR_STEP) as f32;
            }
        }
        self.halo_dirty = Some(Rect {
            min_x,
            min_y,
            max_x,
            max_y,
        });
    }

    /// 过滤出朝向相机、且没有被同一平面上的相邻块完全盖住的面，并预先算好投影包围盒与颜色。
    ///
    /// 隐藏面剔除只做两级近似：背面（用相机位置与平面符号判断）和「同轴同平面、且在另两个
    /// 轴上完全覆盖本面」的相邻块。没有做通用的遮挡查询，所以内部块仍可能被光栅化；正确性
    /// 由逐 dot 的深度测试保证（多画的面不会覆盖更近的面）。
    ///
    /// 光照每个面只算一次：环境项 + 漫反射 + 边缘光；暗色背景下再叠加 Blinn-Phong 高光
    /// （指数 24，系数 0.6），亮色背景把高光关掉。每个通道的结果上限截到 255。
    #[allow(clippy::too_many_arguments)]
    fn visible_faces(
        &self,
        m: &[f64; 9],
        origin: &[f64; 3],
        pose: &Pose,
        boxes: &[Box3],
        dot_width: usize,
        dot_height: usize,
        light_background: bool,
    ) -> Vec<Face> {
        let mut faces = Vec::new();
        let camera_distance = self.camera_distance;
        let light = light_vector();
        let half = half_vector();
        for (box_index, box3) in boxes.iter().enumerate() {
            for a in 0..3 {
                let u = (a + 1) % 3;
                let v = (a + 2) % 3;
                for side in [-1.0f64, 1.0] {
                    let plane = if side > 0.0 { box3.max[a] } else { box3.min[a] };
                    // 背面朝向相机。
                    if (origin[a] - plane) * side <= 0.0 {
                        continue;
                    }
                    let covered = boxes.iter().enumerate().any(|(other_index, other)| {
                        other_index != box_index
                            && (if side > 0.0 {
                                other.min[a]
                            } else {
                                other.max[a]
                            }) == plane
                            && other.min[u] <= box3.min[u]
                            && other.max[u] >= box3.max[u]
                            && other.min[v] <= box3.min[v]
                            && other.max[v] >= box3.max[v]
                    });
                    if covered {
                        continue;
                    }

                    let mut min_x = f64::INFINITY;
                    let mut min_y = f64::INFINITY;
                    let mut max_x = f64::NEG_INFINITY;
                    let mut max_y = f64::NEG_INFINITY;
                    let mut corner = [0.0f64; 3];
                    for corner_u in [box3.min[u], box3.max[u]] {
                        for corner_v in [box3.min[v], box3.max[v]] {
                            corner[a] = plane;
                            corner[u] = corner_u;
                            corner[v] = corner_v;
                            let x = m[0] * corner[0] + m[1] * corner[1] + m[2] * corner[2];
                            let y = m[3] * corner[0] + m[4] * corner[1] + m[5] * corner[2];
                            let z = m[6] * corner[0] + m[7] * corner[1] + m[8] * corner[2];
                            let perspective =
                                (pose.scale * camera_distance) / (camera_distance - z);
                            let screen_x = pose.center_x + x * perspective;
                            let screen_y = pose.center_y + y * perspective;
                            min_x = min_x.min(screen_x);
                            min_y = min_y.min(screen_y);
                            max_x = max_x.max(screen_x);
                            max_y = max_y.max(screen_y);
                        }
                    }
                    let face_min_x = (min_x.floor() as i64).max(0);
                    let face_min_y = (min_y.floor() as i64).max(0);
                    let face_max_x = (max_x.ceil() as i64).min(dot_width as i64 - 1);
                    let face_max_y = (max_y.ceil() as i64).min(dot_height as i64 - 1);
                    if face_max_x < face_min_x || face_max_y < face_min_y {
                        continue;
                    }

                    // 面是平的，光照每面算一次；相机空间法线是旋转矩阵的一列。
                    let nx = m[a] * side;
                    let ny = m[3 + a] * side;
                    let nz = m[6 + a] * side;
                    let diffuse = (nx * light[0] + ny * light[1] + nz * light[2]).max(0.0);
                    let rim = (nx * 0.8 - nz * 0.3).max(0.0);
                    let specular = if light_background {
                        0.0
                    } else {
                        libm::pow((nx * half[0] + ny * half[1] + nz * half[2]).max(0.0), 24.0)
                            * 0.6
                            * 255.0
                    };
                    let lighting = if light_background {
                        (0.55 + diffuse * 0.45 + rim * 0.1).min(1.0)
                    } else {
                        0.45 + diffuse * 0.78 + rim * 0.25
                    };
                    faces.push(Face {
                        axis: a,
                        plane,
                        u_min: box3.min[u],
                        u_max: box3.max[u],
                        v_min: box3.min[v],
                        v_max: box3.max[v],
                        min_x: face_min_x,
                        min_y: face_min_y,
                        max_x: face_max_x,
                        max_y: face_max_y,
                        red: (box3.color[0] * lighting + specular).min(255.0),
                        green: (box3.color[1] * lighting + specular).min(255.0),
                        blue: (box3.color[2] * lighting + specular).min(255.0),
                    });
                }
            }
        }
        faces
    }
}

// ---------------------------------------------------------------------------
// 动画
// ---------------------------------------------------------------------------

/// 前景/背景（终端实际颜色，或主题兜底色）。
///
/// 两个通道都是 f64、0..255；来自 `queryTerminalColors` 的上报值，缺失通道由
/// `colors_from_reported` 回退到主题（前景 `text`，背景按明暗取黑/白）。
/// 背景同时参与雾化、星空混合与 halo 的亮度判定，因此查询完成后要重建屏幕解析缓存
/// （`resolve_colors` 会把尺寸标记为 -1）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EasterEggColors {
    /// 前景。
    pub foreground: Rgb,
    /// 背景。
    pub background: Rgb,
}

/// 时钟（毫秒；TS `performance.now()`）。
pub type EasterEggClock = Rc<dyn Fn() -> f64>;

/// 终端行数回调（TS `this.tui.terminal.rows`）。
pub type TerminalRows = Rc<dyn Fn() -> usize>;

/// 一个星点：字形、色调与闪烁参数；由 `prepare_stars` 按格索引生成。
#[derive(Debug, Clone)]
struct Star {
    glyph: char,
    tint: Rgb,
    strength: f64,
    phase: f64,
    speed: f64,
}

/// `close()` 在第一次调用时冻结的退出时间线；后续每帧据此倒放。
///
/// `time` 是退出开始时的动画秒数，`yaw` 是当时的旋转角，`target_yaw` 是向上取整到整圈的
/// 目标角，`offsets` 冻结了当时的拼图位移（退出期间不再重新洗牌）。
#[derive(Debug, Clone)]
struct ExitState {
    start: f64,
    time: f64,
    yaw: f64,
    target_yaw: f64,
    offsets: Vec<[f64; 3]>,
}

/// 当前拼图周期的缓存：`cycle` 是周期序号，`steps` 是 `shuffle_steps` 的快照列表。
#[derive(Debug, Clone)]
struct ShuffleCache {
    cycle: i64,
    steps: Vec<Vec<Cell3>>,
}

/// 底部提示「<key> to return」的几何与颜色；只在实际渲染时由 `Inner::hint` 生成。
#[derive(Debug, Clone)]
struct Hint {
    row: isize,
    start: usize,
    text: String,
    key_length: usize,
    key_color: Rgb,
    color: Rgb,
}

/// 动画的全部可变状态，包在 `Rc<RefCell<..>>` 里由组件与定时器/颜色任务共享。
///
/// 所有权：overlay 条目持有 [`EasterEgg3dAnimation`]，后者持有本结构的 `Rc`；`Driver` 和
/// 颜色查询任务只持 `Weak`，因此 overlay 被移除后它们下一次回调会自然失效。所有可变访问
/// 都通过 `borrow_mut()` 串行化——这是单线程 `LocalSet` 模型，没有跨线程共享。
///
/// 四个收尾回调（`on_done`/`on_render_finish`/`on_dispose`/`on_close_request`）的区别见各自
/// 字段注释；`timer_active` 决定当前走哪条收尾路径，`finish_now` 负责保证 `on_done` 只执行
/// 一次。
struct Inner {
    screen: Vec<String>,
    model: Model,
    foreground: Rgb,
    background: Rgb,
    on_done: Option<Box<dyn FnMut()>>,
    /// 无定时器时在 `render` 里收尾用的安全回调（不能隐藏 overlay，只能做 `Rc<Cell>` 级别的
    /// 状态切换；播放入口用它把 overlay 标记为不可见）。`None` 时直接调用 `on_done`
    /// （直接构造组件的测试/宿主在 render 上下文中调用是安全的）。
    on_render_finish: Option<Box<dyn FnMut()>>,
    /// 动画未收尾就被销毁（宿主把 overlay 移除了）时调用：只做安全操作（复位模块级
    /// `playing`）。TS 的 `setInterval` 闭包让动画活到定时器发现「1 秒未渲染」并 `finish()`；
    /// Rust 里 overlay 条目是唯一的所有者，销毁即视为收尾，但此时可能正持有
    /// `TuiCore` 的可变借用，所以不能 `hide()`。
    on_dispose: Option<Box<dyn FnMut()>>,
    /// 输入分发内部的第二次 `close()`：TS 会同步 `finish()` + `overlay.hide()`；Rust 在
    /// 输入分发里正持有 `TuiCore` 借用，不能隐藏。播放入口提供本回调：立即把 overlay 标记
    /// 为不可见（下一帧合成就消失），并在事件循环拿回控制权后（`spawn_local` 的任务）真正
    /// `finish()` → `hide()`。直接构造的组件没有该回调，退回 `finish_requested` + 定时器。
    on_close_request: Option<Box<dyn FnMut()>>,
    clock: EasterEggClock,
    rows: TerminalRows,
    request_render: Option<RequestRenderCallback>,
    start_time: f64,
    last_render: f64,
    /// lazy 入口先显示抓屏、等终端颜色回复（或 100 ms 超时）后才构造时间线：
    /// 为 true 时 `render` 原样返回抓屏，`resolve_colors` 把 `start_time` 重置到此刻
    /// （TS 在 `await queryTerminalColors` 之后才 `new EasterEgg3dAnimation`）。
    colors_pending: bool,
    /// TS 的 `timer` 是否存在（`finish()` 的幂等依据）。
    timer_active: bool,
    /// 帧定时器是否真的在跑（`set_timer` 成功后为 true；[`LocalTimer`] 在无法
    /// `spawn_local` 时把它置回 false）。为 false 时由 `render` 按同一规则自行收尾。
    timer_running: Rc<Cell<bool>>,
    /// 在输入回调里请求「立即 finish」（不能在那里隐藏 overlay）。
    finish_requested: bool,
    exit: Option<ExitState>,
    shuffle: Option<ShuffleCache>,
    screen_width: isize,
    screen_height: isize,
    stars: Vec<Option<Star>>,
    cells: Vec<Vec<ScreenCell>>,
    ansi_cache: HashMap<i64, String>,
    raster: BlockRaster,
}

impl Inner {
    fn elapsed(&self) -> f64 {
        ((self.clock)() - self.start_time) / 1000.0
    }

    fn exit_progress(&self) -> f64 {
        match &self.exit {
            Some(exit) => clamp01(((self.clock)() - exit.start) / 1000.0 / EXIT_DURATION),
            None => 0.0,
        }
    }

    /// `should_finish`：退出进度 ≥ 1，或超过 1 秒没有渲染。
    fn should_finish(&self, now_ms: f64) -> bool {
        (self.exit.is_some() && self.exit_progress() >= 1.0) || now_ms - self.last_render > 1000.0
    }

    /// `finish()`：幂等；返回 `onDone` 回调（由调用方在合适的上下文里执行）。
    fn finish_now(&mut self) -> Option<Box<dyn FnMut()>> {
        if !self.timer_active {
            return None;
        }
        self.timer_active = false;
        // 定时器路径优先：不要再让 render 走无定时器的收尾分支。
        self.on_render_finish = None;
        self.on_close_request = None;
        self.on_done.take()
    }

    /// 没有可用定时器时的 `finish()`：在 `render` 里按与 TS 定时器相同的条件（退出完成 /
    /// 超过 1 秒未渲染）收尾。`render` 在 `TuiCore` 的可变借用内被调用，不能 `hide()`，
    /// 所以播放入口提供 [`Self::on_render_finish`]（标记 overlay 不可见 + 请求重绘）；
    /// 直接构造的组件没有该回调时退回 `on_done`（测试场景没有 `TuiCore` 借用）。
    fn finish_on_render(&mut self) {
        if !self.timer_active {
            return;
        }
        self.timer_active = false;
        if let Some(mut callback) = self.on_render_finish.take() {
            self.on_done = None;
            callback();
        } else if let Some(mut callback) = self.on_done.take() {
            callback();
        }
    }

    /// 终端颜色查询返回（或超时）时**只**解析一次颜色，并把时间线重置到此刻。
    ///
    /// TS 在 `await tui.queryTerminalColors({ timeoutMs: 100 })` 之后才创建动画：
    /// 所有帧都用终端上报的颜色，`startTime` 从查询结束开始。超时后到达的迟到回复
    /// **不会**再改动已构造的动画（与 TS 相同）。
    fn resolve_colors(&mut self, colors: EasterEggColors) {
        if !self.colors_pending {
            return;
        }
        self.colors_pending = false;
        self.foreground = colors.foreground;
        self.background = colors.background;
        let now = (self.clock)();
        self.start_time = now;
        self.last_render = now;
        // 屏幕解析把默认前景烘进了 cell，背景参与 halo/星空，全部按最终颜色重建。
        self.screen_width = -1;
        self.screen_height = -1;
        self.ansi_cache.clear();
        if self.timer_active
            && let Some(request) = &self.request_render
        {
            request();
        }
    }

    /// TS `close()`：第一次进入退出动画，第二次立即结束。
    fn close(&mut self) -> Option<Box<dyn FnMut()>> {
        if self.exit.is_some() {
            return self.finish_now();
        }
        let time = self.elapsed();
        let yaw = spin_angle(time);
        let turn = std::f64::consts::PI * 2.0;
        self.exit = Some(ExitState {
            start: (self.clock)(),
            time,
            yaw,
            target_yaw: (yaw / turn).ceil() * turn,
            offsets: self.block_offsets(time),
        });
        None
    }

    /// `close()` 在输入分发内部调用：不能在那时隐藏 overlay（`TuiCore` 正被可变借用）。
    ///
    /// TS 的第二次 `close()` → `finish()` → `onDone()` → `overlay.hide()` 在
    /// `handleInput`/`handleMouse` 里**同步**完成。Rust 只能用等价的两步：
    /// [`Self::on_close_request`] 立即把 overlay 标记为不可见（本帧合并时就消失），并安排
    /// 一个事件循环任务在输入分发返回后执行真正的 `finish()` + `hide()`；没有任务可用时由
    /// 下一次定时器 tick / render 兜底（`finish_requested`）。
    fn close_in_input(&mut self) {
        if self.exit.is_some() {
            self.finish_requested = true;
            if let Some(mut callback) = self.on_close_request.take() {
                callback();
            }
        } else {
            let _ = self.close();
        }
    }

    /// 颜色查询期间原样显示抓屏：TS 此时还没有创建 overlay，用户看到的仍是原界面。
    fn pending_frame(&self) -> Vec<String> {
        let height = (self.rows)().max(1);
        (0..height)
            .map(|row| self.screen.get(row).cloned().unwrap_or_default())
            .collect()
    }

    fn fly_progress(&self, time: f64) -> f64 {
        smooth((time - FLY_START) / FLY_DURATION)
    }

    fn star_alpha(&self, time: f64) -> f64 {
        smooth((time - STARS_START) / STARS_FADE)
    }

    fn hint_alpha(&self, time: f64) -> f64 {
        smooth((time - FLY_START - FLY_DURATION) / HINT_FADE)
    }

    /// 每个块相对模型位置的位移（grid 单位）。
    ///
    /// 时间线分四段：`PUZZLE_START` 之前全为 0；`PUZZLE_STEP * PUZZLE_STEPS` 之内按每步
    /// 0.3 秒在 `shuffle_steps` 的快照之间用 `smooth` 插值；随后 `PUZZLE_RETURN` 秒内所有块
    /// 同时飞回原位（z 方向按块序奇偶交替 ±`arc`，避免互相穿过）；最后 `PUZZLE_HOLD` 秒
    /// 保持归位。每次进入新周期会重算并缓存 `shuffle_steps`。
    fn block_offsets(&mut self, time: f64) -> Vec<[f64; 3]> {
        let block_count = self.model.blocks.len();
        if time < PUZZLE_START {
            return vec![[0.0, 0.0, 0.0]; block_count];
        }
        let cycle = ((time - PUZZLE_START) / PUZZLE_CYCLE).floor();
        let local = time - PUZZLE_START - cycle * PUZZLE_CYCLE;
        if self.shuffle.as_ref().map(|shuffle| shuffle.cycle) != Some(cycle as i64) {
            let steps = shuffle_steps(&self.model, cycle as i64);
            self.shuffle = Some(ShuffleCache {
                cycle: cycle as i64,
                steps,
            });
        }
        let steps = &self
            .shuffle
            .as_ref()
            .expect("shuffle cache was just filled")
            .steps;
        let offset = |position: &Cell3, index: usize| -> [f64; 3] {
            let home = self.model.blocks[index].home;
            [
                position[0] as f64 - home[0] as f64,
                position[1] as f64 - home[1] as f64,
                position[2] as f64 - home[2] as f64,
            ]
        };
        let shuffle_end = PUZZLE_STEP * PUZZLE_STEPS as f64;
        if local < shuffle_end {
            let step = (local / PUZZLE_STEP).floor() as usize;
            let progress = smooth((local - step as f64 * PUZZLE_STEP) / PUZZLE_STEP);
            let from = &steps[step.min(PUZZLE_STEPS)];
            let to = &steps[(step + 1).min(PUZZLE_STEPS)];
            return (0..block_count)
                .map(|index| {
                    let a = offset(&from[index], index);
                    let b = offset(&to[index], index);
                    [
                        a[0] + (b[0] - a[0]) * progress,
                        a[1] + (b[1] - a[1]) * progress,
                        a[2] + (b[2] - a[2]) * progress,
                    ]
                })
                .collect();
        }
        if local < shuffle_end + PUZZLE_RETURN {
            // 所有块同时飞回；深度方向交替弧线避免互相穿过。
            let u = (local - shuffle_end) / PUZZLE_RETURN;
            let remaining = 1.0 - smooth(u);
            let arc = js_sin(std::f64::consts::PI * clamp01(u)) * 0.8;
            return (0..block_count)
                .map(|index| {
                    let [x, y, z] = offset(&steps[PUZZLE_STEPS][index], index);
                    [
                        x * remaining,
                        y * remaining,
                        z * remaining + if index % 2 == 0 { arc } else { -arc },
                    ]
                })
                .collect();
        }
        vec![[0.0, 0.0, 0.0]; block_count]
    }

    /// `pose(width, height, progress, time)`：在 dot 空间里算出模型中心、缩放与旋转。
    ///
    /// `progress` 是飞入进度（0 = 起始位置，1 = 屏幕中心）；缩放按几何插值
    /// `start * (end/start)^progress`，`end_scale` 同时受屏幕宽度份额、可用高度与模型半径
    /// 约束（`reach` 是模型旋转后最远点的投影距离）。有 `origin` 时起点在 header 半块渲染
    /// 的位置，否则从屏幕中心以 `START_SCALE` 长出。pitch/roll 都乘 `progress`，所以起始
    /// 姿态是正视。
    fn pose(&self, width: usize, height: usize, progress: f64, time: f64) -> Pose {
        let model = &self.model;
        let camera_distance = model.camera_distance;
        let radius = model.radius;
        let reach = radius * (camera_distance / (camera_distance - radius));
        let start_scale = match model.origin {
            Some(_) => (2.0 * (camera_distance - DEPTH / 2.0)) / camera_distance,
            None => START_SCALE,
        };
        let end_scale = start_scale.max(
            (width as f64 * 2.0 * model.width_share).min((height as f64 * 4.0 - 8.0) * 0.48)
                / reach,
        );
        let end_x = width as f64;
        let end_y = height as f64 * 2.0 - 2.0;
        let start_x = match model.origin {
            Some((column, _)) => column as f64 * 2.0 + model.columns as f64,
            None => end_x,
        };
        let start_y = match model.origin {
            Some((_, row)) => row as f64 * 4.0 + model.rows as f64,
            None => end_y,
        };
        Pose {
            center_x: start_x + (end_x - start_x) * progress,
            center_y: start_y + (end_y - start_y) * progress,
            // 缩放按几何插值，感觉更均匀。
            scale: start_scale * libm::pow(end_scale / start_scale, progress),
            yaw: spin_angle(time),
            pitch: 0.3 * js_sin(spin_phase(time)) * progress,
            roll: 0.06 * js_sin(2.0 * spin_phase(time)) * progress,
        }
    }

    /// `hint(width, height, alpha, background)`：生成底部「<key> to return」提示。
    ///
    /// 键名来自 `tui.select.cancel` 的第一个绑定（缺失时用 "escape"），文本用
    /// `format_key_text` 格式化。提示固定在第 `height - 2` 行（高度不足 2 时为负数，永远
    /// 不会匹配），水平居中；文本比 `width` 宽时返回 `None`。键与说明文字分别用
    /// `Muted`/`Dim` 主题色按 `alpha` 与背景混合。
    fn hint(&self, width: usize, height: usize, alpha: f64, background: Rgb) -> Option<Hint> {
        if alpha <= 0.0 {
            return None;
        }
        let keys = get_keybindings().get_keys("tui.select.cancel");
        let key = format_key_text(
            keys.first().map(String::as_str).unwrap_or("escape"),
            KeyTextFormatOptions::default(),
        );
        let text = format!("{key} to return");
        let text_length = text.chars().count();
        if text_length > width {
            return None;
        }
        Some(Hint {
            // TS `height - 2`：高度不足 2 时是负数，永远不会等于任何行。
            row: height as isize - 2,
            start: (width as isize - text_length as isize).max(0) as usize / 2,
            text,
            key_length: key.chars().count(),
            key_color: mix(background, theme_color(ThemeColor::Muted), alpha),
            color: mix(background, theme_color(ThemeColor::Dim), alpha),
        })
    }

    /// 稀疏、确定性的星空：约 `STAR_DENSITY` 的格子里有一个盲文点。
    ///
    /// 每个格子的取舍与参数全部来自 `hash(index * 3 + 0x51ed)` 等固定表达式，没有墙钟或
    /// 随机源，因此同一尺寸下每次重建结果一致；闪烁由 `Star.phase/speed` 与当前时间决定。
    /// 索引是 `row * width + column`，尺寸变化时调用方会整体重建。
    fn prepare_stars(&self, width: usize, height: usize) -> Vec<Option<Star>> {
        let mut stars = vec![None; width * height];
        for (index, slot) in stars.iter_mut().enumerate() {
            if hash(index as f64 * 3.0 + 0x51ed as f64) >= STAR_DENSITY {
                continue;
            }
            let random = |salt: f64| hash(index as f64 * 7.0 + salt * 0x9e37 as f64);
            *slot = Some(Star {
                glyph: braille(DOT_BITS[(random(1.0) * 8.0).floor().min(7.0) as usize]),
                tint: STAR_TINTS[(random(2.0) * STAR_TINTS.len() as f64).floor().min(3.0) as usize],
                strength: 0.1 + random(3.0) * 0.12,
                phase: random(4.0) * std::f64::consts::PI * 2.0,
                speed: 0.4 + random(5.0) * 0.8,
            });
        }
        stars
    }

    /// `prepareCells(width, foreground)`：解析屏幕并为每格算好消散时序。
    ///
    /// 延迟 = `FLY_START + 到模型中心的归一化距离 * WAVE_SPREAD + seed * WAVE_JITTER`，
    /// 因此尘埃从模型出现的位置向外扩散；宽字符的两个单元共享同一时间线与点数（取前一列
    /// 的文本算 `glyph_ink`）。若模型有 `origin`，header 中模型覆盖的矩形会被清成空格，
    /// 让 3D 模型替代原来的半块渲染（保留前景色，背景清空）。
    fn prepare_cells(&self, width: usize, foreground: Rgb) -> Vec<Vec<ScreenCell>> {
        let mut cells = parse_screen(&self.screen, width, foreground, self.background);
        let height = (self.rows)().max(1);
        let columns = self.model.columns;
        let rows = self.model.rows;
        // 尘埃从模型出现的位置散开。
        let center_x = match self.model.origin {
            Some((column, _)) => column as f64 + columns as f64 / 2.0,
            None => width as f64 / 2.0,
        };
        let center_y = match self.model.origin {
            Some((_, row)) => row as f64 + rows as f64 / 4.0,
            None => height as f64 / 2.0,
        };
        let farthest = js_hypot2(
            (center_x).max(width as f64 - center_x),
            (center_y).max(height as f64 - center_y) * 2.0,
        );
        for (row, line) in cells.iter_mut().enumerate() {
            for column in 0..line.len() {
                let seed = hash(row as f64 * 65_537.0 + column as f64);
                // 宽字符的第二列沿用第一列的时间线。
                let owner_index = if line[column].width == 0 {
                    column.saturating_sub(1)
                } else {
                    column
                };
                if line[column].width != 0 {
                    let distance =
                        js_hypot2(column as f64 - center_x, (row as f64 - center_y) * 2.0)
                            / farthest;
                    line[column].delay = FLY_START + distance * WAVE_SPREAD + seed * WAVE_JITTER;
                } else {
                    line[column].delay = line[owner_index].delay;
                }
                line[column].seed = (row * 65_537 + column) as i64;
                let owner_text = line[owner_index].text.clone();
                line[column].ink = glyph_ink(&owner_text, seed);
            }
        }
        // 3D 模型替换它原来的半块渲染。
        if let Some((origin_column, origin_row)) = self.model.origin {
            let header_rows = rows.div_ceil(2);
            for row in origin_row..origin_row + header_rows {
                let Some(line) = cells.get_mut(row) else {
                    continue;
                };
                for column in origin_column..origin_column + columns {
                    if let Some(cell) = line.get_mut(column) {
                        cell.text = " ".to_string();
                        cell.width = 1;
                        cell.ink = 0;
                        cell.bg = None;
                    }
                }
            }
        }
        cells
    }

    /// 整帧渲染（TS `EasterEgg3dAnimation.render(width)`）。
    ///
    /// 每帧的顺序：无定时器时的收尾判定 → 颜色未就绪则原样返回抓屏 → 尺寸变化时重建
    /// `cells`/`stars` → 计算 `dissolve_time`、[`Pose`]、hint/星空透明度与拼图位移 →
    /// [`BlockRaster::render`] → 逐格合成 ANSI 行。`width` 来自宿主的当前终端列数，
    /// 高度每次都从 `rows` 回调现取（`max(1, ...)`）。
    ///
    /// 合成优先级（高到低）：hint 文字 → logo 盲文点 → 尚未消散的原字符 → 盲文尘埃 →
    /// 星空/空格。宽字符在「原字符」阶段一次输出两列并跳过伴随列（仅当伴随列上没有 logo
    /// 点时；否则整格进入尘埃逻辑）。所有行以 `\x1b[0m` 结尾，保证下一行不继承当前样式。
    fn render(&mut self, width: usize) -> Vec<String> {
        let height = (self.rows)().max(1);
        let foreground = self.foreground;
        let background = self.background;
        let now_ms = (self.clock)();
        // TS 的 `setInterval` 在这些时刻调用 `finish()`：退出进度 ≥ 1，或超过 1 秒没有
        // 渲染（overlay 已被外部隐藏）。没有可用的定时器时（`NoopTimer`，或
        // `spawn_local` 不可用），由本次渲染补做同样的判定——否则 `timer_active` 会永远
        // 为 true：overlay 停在最后一帧，且每帧都重新请求重绘。
        if self.timer_active && !self.timer_running.get() {
            let finish_requested = std::mem::take(&mut self.finish_requested);
            if finish_requested || self.should_finish(now_ms) {
                self.finish_on_render();
            }
        }
        self.last_render = now_ms;
        // 终端颜色还没回来：TS 里动画尚未构造（`await queryTerminalColors` 之后才 new），
        // 屏幕保持原样。这里原样返回切换全屏前抓下来的行。
        if self.colors_pending {
            return self.pending_frame();
        }
        if width as isize != self.screen_width || height as isize != self.screen_height {
            self.screen_width = width as isize;
            self.screen_height = height as isize;
            self.cells = self.prepare_cells(width, foreground);
            self.stars = self.prepare_stars(width, height);
        }

        // 溶解时间：进入时正放，退出时倒放，所以屏幕按相反顺序重组。
        let dissolve_time;
        let mut pose;
        let hint_alpha;
        let star_alpha;
        let offsets;
        if let Some(exit) = self.exit.clone() {
            let progress = self.exit_progress();
            let landing = clamp01(progress / 0.8);
            let settle = 1.0 - smooth(landing);
            dissolve_time = exit.time.min(DISSOLVE_END) * (1.0 - progress);
            pose = self.pose(
                width,
                height,
                self.fly_progress(exit.time) * settle,
                exit.time,
            );
            pose.yaw = exit.yaw + (exit.target_yaw - exit.yaw) * ease_out(landing);
            pose.pitch *= settle;
            pose.roll *= settle;
            hint_alpha = self.hint_alpha(exit.time) * (1.0 - smooth(progress / 0.2));
            star_alpha = self.star_alpha(exit.time) * (1.0 - smooth(progress / 0.3));
            // 块在 logo 落地前就归位。
            let gather = 1.0 - smooth(progress / 0.5);
            offsets = exit
                .offsets
                .iter()
                .map(|offset| [offset[0] * gather, offset[1] * gather, offset[2] * gather])
                .collect();
        } else {
            let time = self.elapsed();
            dissolve_time = time;
            pose = self.pose(width, height, self.fly_progress(time), time);
            hint_alpha = self.hint_alpha(time);
            star_alpha = self.star_alpha(time);
            offsets = self.block_offsets(time);
        }

        let columns = self.model.columns;
        let rows = self.model.rows;
        let boxes: Vec<Box3> = self
            .model
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                let [dx, dy, dz] = offsets[index];
                let x = block.home[0] as f64 - columns as f64 / 2.0 + dx;
                let y = block.home[1] as f64 - rows as f64 / 2.0 + dy;
                Box3 {
                    min: [x, y, dz - DEPTH / 2.0],
                    max: [x + 1.0, y + 1.0, dz + DEPTH / 2.0],
                    color: block.color,
                }
            })
            .collect();
        self.raster.render(
            width,
            height,
            &pose,
            &boxes,
            background,
            if is_light(background) {
                LIGHT_HALO_STRENGTH
            } else {
                0.0
            },
        );

        let background_fade = smooth(dissolve_time / BACKGROUND_FADE);
        let text_fade = smooth(dissolve_time / 0.6) * 0.3;
        // 屏幕完全溶解后文字层为空，可以跳过。
        let text_active = dissolve_time < DISSOLVE_END || background_fade < 1.0;
        let hint = self.hint(width, height, hint_alpha, background);
        let frame = (dissolve_time * 14.0).floor();
        let now = self.elapsed();
        // 星星亮度量化，只有某行的星点跨档时该行才变化。
        let star_level = js_round(star_alpha * STAR_ALPHA_LEVELS) / STAR_ALPHA_LEVELS;

        // TS 的 `render()` **从不** `requestRender()`：唯一的重绘来源是 30 fps 的
        // `setInterval`。只有定时器没有真正跑起来（`NoopTimer`/不在 `LocalSet`）时才由
        // 渲染驱动下一帧；否则会让宿主以 16 ms 节流（~60 fps）空转。
        let request_render = if self.timer_active && !self.timer_running.get() {
            self.request_render.clone()
        } else {
            None
        };
        let background = self.background;
        let foreground = self.foreground;
        let Self {
            cells,
            stars,
            raster,
            ansi_cache,
            ..
        } = self;
        let halo = |index: usize, base: Option<Rgb>| -> Option<Rgb> {
            let amount = f64::from(raster.halo_amount[index]);
            if amount <= 0.0 {
                return base;
            }
            let color = &raster.halo_rgb;
            Some(mix(
                base.unwrap_or(background),
                [
                    f64::from(color[index * 3]),
                    f64::from(color[index * 3 + 1]),
                    f64::from(color[index * 3 + 2]),
                ],
                amount,
            ))
        };
        let starfield = |index: usize| -> Option<(char, Rgb)> {
            if star_level <= 0.0 {
                return None;
            }
            let star = stars.get(index)?.as_ref()?;
            let flicker =
                js_round((0.5 + 0.5 * js_sin(now * star.speed + star.phase)) * STAR_FLICKER_LEVELS)
                    / STAR_FLICKER_LEVELS;
            Some((
                star.glyph,
                mix(
                    background,
                    mix(foreground, star.tint, 0.5),
                    star.strength * (0.7 + 0.3 * flicker) * star_level,
                ),
            ))
        };

        let mut lines = Vec::with_capacity(height);
        for row in 0..height {
            let row_cells: &[ScreenCell] = cells.get(row).map(Vec::as_slice).unwrap_or(&[]);
            let mut line = String::new();
            let mut current_fg: i64 = -1;
            let mut current_bg: i64 = -1;
            let mut column = 0usize;
            while column < width {
                let index = row * width + column;
                let cell = if text_active {
                    row_cells.get(column)
                } else {
                    None
                };
                let cell_bg = halo(
                    index,
                    cell.and_then(|cell| cell.bg)
                        .filter(|_| background_fade < 1.0)
                        .map(|bg| mix(bg, background, background_fade)),
                );
                // 1) hint 覆盖一切。
                if let Some(hint) = &hint
                    && row as isize == hint.row
                    && column >= hint.start
                    && column < hint.start + hint.text.chars().count()
                {
                    let offset = column - hint.start;
                    let character = hint
                        .text
                        .chars()
                        .nth(offset)
                        .expect("offset is within text");
                    let color = if offset < hint.key_length {
                        hint.key_color
                    } else {
                        hint.color
                    };
                    emit(
                        &mut line,
                        &mut current_fg,
                        &mut current_bg,
                        ansi_cache,
                        &character.to_string(),
                        Some(color),
                        cell_bg,
                    );
                    column += 1;
                    continue;
                }
                // 2) logo 点。
                let dots = raster.bits[index];
                if dots != 0 {
                    let step = LOGO_COLOR_STEP * f64::from(raster.counts[index]);
                    let rgb = &raster.rgb;
                    let color: Rgb = [
                        js_round(f64::from(rgb[index * 3]) / step) * LOGO_COLOR_STEP,
                        js_round(f64::from(rgb[index * 3 + 1]) / step) * LOGO_COLOR_STEP,
                        js_round(f64::from(rgb[index * 3 + 2]) / step) * LOGO_COLOR_STEP,
                    ];
                    emit(
                        &mut line,
                        &mut current_fg,
                        &mut current_bg,
                        ansi_cache,
                        &braille(dots).to_string(),
                        Some(color),
                        cell_bg,
                    );
                    column += 1;
                    continue;
                }
                // 3) 屏外/已淡出的格 → 星空或空格。
                let Some(cell) = cell else {
                    let star = starfield(index);
                    emit(
                        &mut line,
                        &mut current_fg,
                        &mut current_bg,
                        ansi_cache,
                        &star
                            .as_ref()
                            .map(|(glyph, _)| glyph.to_string())
                            .unwrap_or_else(|| " ".to_string()),
                        star.map(|(_, fg)| fg),
                        cell_bg,
                    );
                    column += 1;
                    continue;
                };
                // 4) 还没到消散时刻：原字符（宽字符整格保留并跳过伴随列）。
                let dust = (dissolve_time - cell.delay) / DUST_DURATION;
                if dust < 0.0
                    && cell.width == 2
                    && raster.bits.get(index + 1).copied().unwrap_or(0) == 0
                {
                    emit(
                        &mut line,
                        &mut current_fg,
                        &mut current_bg,
                        ansi_cache,
                        &cell.text,
                        Some(mix(cell.fg, background, text_fade)),
                        cell_bg,
                    );
                    // TS 的 `for` 循环还会 `column++`：宽字符连同伴随列一起跳过。
                    column += 2;
                    continue;
                }
                if dust < 0.0 && cell.width == 1 {
                    emit(
                        &mut line,
                        &mut current_fg,
                        &mut current_bg,
                        ansi_cache,
                        &cell.text,
                        Some(mix(cell.fg, background, text_fade)),
                        cell_bg,
                    );
                    column += 1;
                    continue;
                }
                // 5) 变成盲文尘：点数从 ink 线性减到 0。
                let dot_count = if dust < 0.0 {
                    cell.ink as i64
                } else {
                    js_round(cell.ink as f64 * (1.0 - clamp01(dust))) as i64
                };
                if dot_count <= 0 {
                    let star = starfield(index);
                    emit(
                        &mut line,
                        &mut current_fg,
                        &mut current_bg,
                        ansi_cache,
                        &star
                            .as_ref()
                            .map(|(glyph, _)| glyph.to_string())
                            .unwrap_or_else(|| " ".to_string()),
                        star.map(|(_, fg)| fg),
                        cell_bg,
                    );
                    column += 1;
                    continue;
                }
                // 选 `dotCount` 个不同的点；选择每秒变几次，尘埃会闪烁。
                let mut bits = 0u8;
                let mut placed = 0i64;
                let mut attempt = 0i64;
                while placed < dot_count && attempt < 32 {
                    let bit =
                        DOT_BITS[(hash(cell.seed as f64 * 7919.0 + frame * 131.0 + attempt as f64)
                            * 8.0)
                            .floor()
                            .min(7.0) as usize];
                    attempt += 1;
                    if bits & bit != 0 {
                        continue;
                    }
                    bits |= bit;
                    placed += 1;
                }
                let fade = text_fade + (1.0 - text_fade) * libm::pow(clamp01(dust), 0.8);
                emit(
                    &mut line,
                    &mut current_fg,
                    &mut current_bg,
                    ansi_cache,
                    &braille(bits).to_string(),
                    Some(mix(cell.fg, background, fade)),
                    cell_bg,
                );
                column += 1;
            }
            lines.push(format!("{line}\x1b[0m"));
        }
        if let Some(request) = &request_render {
            request();
        }
        lines
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // TS 的 `setInterval` 闭包持有动画：即使宿主把 overlay 移除（例如退出时隐藏全部
        // overlay），定时器仍会在「1 秒未渲染」后调用 `finish()` → `onDone()` → 复位
        // `playing`。Rust 里 overlay 条目是动画的唯一所有者，条目被移除时本结构体同步销毁；
        // 此时可能正持有 `TuiCore` 的可变借用（`hideOverlay` 里），所以只调用安全的
        // `on_dispose`（复位模块级 `playing`），不碰 overlay。
        if self.timer_active
            && let Some(mut callback) = self.on_dispose.take()
        {
            callback();
        }
    }
}

/// 把颜色打包成 24-bit 整数（RGB 各 8 位），用作 ANSI 缓存键。
///
/// 打包前用 [`js_round`] 舍入；`-1` 由 `emit` 用作「当前没有颜色」的哨兵，正常颜色
/// （0..255）不会与它冲突。
fn pack(color: Rgb) -> i64 {
    ((js_round(color[0]) as i64) << 16)
        | ((js_round(color[1]) as i64) << 8)
        | js_round(color[2]) as i64
}

/// `ansi(key, isBackground)`：把打包颜色转成前台/后台 SGR 序列，并跨帧缓存。
///
/// 缓存键是 `key * 2 + is_background`；命中时直接克隆字符串。颜色模式取自全局主题
/// （`THEME.get_color_mode()`），主题未初始化时 `expect` panic——与 TS 在这里抛错一致。
fn ansi_for(key: i64, is_background: bool, cache: &mut HashMap<i64, String>) -> String {
    let cache_key = key * 2 + i64::from(is_background);
    if let Some(value) = cache.get(&cache_key) {
        return value.clone();
    }
    let color = rgb_color(
        ((key >> 16) & 255) as f64,
        ((key >> 8) & 255) as f64,
        (key & 255) as f64,
    );
    let mode = kiana_theme::THEME
        .get_color_mode()
        .expect("theme must be initialized before rendering (TS throws here)");
    let value = if is_background {
        background_ansi(color, mode)
    } else {
        foreground_ansi(color, mode)
    };
    cache.insert(cache_key, value.clone());
    value
}

/// 向行缓冲追加一段文本，并在颜色变化时写入 SGR 序列。
///
/// `current_fg`/`current_bg` 是跨调用保持的「已设置颜色」哨兵，初值为 -1（无）。两条
/// 稳定输出规则：
///
/// * 前景序列只在 `text != " "` 时写——空格不改变后续输出的前景色，避免每行末尾无意义的
///   颜色切换；背景则在 key 变化时总是写。
/// * `fg == None` 写 `\x1b[39m`（默认前景），`bg == None` 写 `\x1b[49m`（默认背景）。
///
/// 序列本身由 `ansi_for` 按 `(颜色, 前景/背景)` 缓存在 `HashMap` 中；每行结束时调用方额外
/// 追加 `\x1b[0m` 重置状态。
fn emit(
    line: &mut String,
    current_fg: &mut i64,
    current_bg: &mut i64,
    ansi_cache: &mut HashMap<i64, String>,
    text: &str,
    fg: Option<Rgb>,
    bg: Option<Rgb>,
) {
    let fg_key = fg.map(pack).unwrap_or(-1);
    let bg_key = bg.map(pack).unwrap_or(-1);
    if fg_key != *current_fg && text != " " {
        line.push_str(&if let Some(_fg) = fg {
            ansi_for(fg_key, false, ansi_cache)
        } else {
            "\x1b[39m".to_string()
        });
        *current_fg = fg_key;
    }
    if bg_key != *current_bg {
        line.push_str(&if bg.is_some() {
            ansi_for(bg_key, true, ansi_cache)
        } else {
            "\x1b[49m".to_string()
        });
        *current_bg = bg_key;
    }
    line.push_str(text);
}

/// 主题 token 的具体颜色（TS `toRgb(theme.colors.<token>)`）。
fn theme_color(color: ThemeColor) -> Rgb {
    let value = kiana_theme::THEME
        .color(color.token())
        .expect("theme must be initialized before rendering (TS throws here)")
        .unwrap_or(kiana_tui::colors::Color::Rgb {
            r: 255.0,
            g: 255.0,
            b: 255.0,
        });
    to_rgb(value)
}

/// 帧驱动（`setInterval` 的 Rust 等价物）。
///
/// 每次 `schedule` 把回调交给 [`Timer`]；回调触发后先检查 `running` 与 `generation` 是否
/// 仍是排程时的那一代——`stop()`/重新 `start()` 都会递增 generation，使旧定时器已投递的
/// 回调变成 no-op（[`LocalTimer`] 的 `cancel` 无法真正撤销已 spawn 的任务）。回调只持
/// `Weak` 指向 `Driver` 与 `Inner`，组件销毁后自动停止。
///
/// 单次 tick 的顺序是：取 `finish_requested`/`should_finish` → 需要收尾就 `stop()` 并执行
/// `finish_now()` 返回的回调 → 否则请求重绘并重新排程。`RefCell` 的借用都在各自的作用域
/// 内释放，不会在调用外部回调时持有 `Inner` 的借用。
struct Driver {
    timer: RefCell<Box<dyn Timer>>,
    interval_ms: Cell<u64>,
    /// 定时器是否**真的**在排程。与 [`Inner`] 共享：`LocalTimer` 无法 `spawn_local` 时
    /// 置回 false，`render` 据此补做无定时器收尾（见 [`Inner::finish_on_render`]）。
    running: Rc<Cell<bool>>,
    generation: Cell<u64>,
    inner: RefCell<Weak<RefCell<Inner>>>,
}

impl Driver {
    /// 排下一帧：把回调交给当前 [`Timer`]，回调里做收尾判定或请求重绘后再次 `schedule`。
    ///
    /// `running == false` 时直接返回（`stop()` 之后的幂等保护）。回调捕获的是 `Weak`，
    /// 升级失败说明 `Driver`/`Inner` 已销毁，什么都不做。
    fn schedule(self: &Rc<Self>) {
        if !self.running.get() {
            return;
        }
        let generation = self.generation.get();
        let weak = self.inner.borrow().clone();
        let this = Rc::downgrade(self);
        self.timer.borrow_mut().set(
            self.interval_ms.get(),
            Box::new(move || {
                let Some(this) = this.upgrade() else {
                    return;
                };
                if !this.running.get() || this.generation.get() != generation {
                    return;
                }
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let now = {
                    let inner = inner.borrow();
                    (inner.clock)()
                };
                let should_finish = {
                    let mut inner = inner.borrow_mut();
                    let finish_requested = std::mem::take(&mut inner.finish_requested);
                    finish_requested || inner.should_finish(now)
                };
                if should_finish {
                    this.stop();
                    let callback = inner.borrow_mut().finish_now();
                    if let Some(mut callback) = callback {
                        callback();
                    }
                    return;
                }
                let request = {
                    let inner = inner.borrow();
                    inner.request_render.clone()
                };
                if let Some(request) = request {
                    request();
                }
                this.schedule();
            }),
        );
    }

    /// 标记运行并递增 generation，使此前已投递的回调全部失效，然后开始排程。
    ///
    /// 重复调用是安全的：每次都会得到新的一代，旧定时器的回调只会在检查时直接返回。
    fn start(self: &Rc<Self>) {
        self.running.set(true);
        self.generation.set(self.generation.get() + 1);
        self.schedule();
    }

    /// 停止排程：清 running、递增 generation，并调用 [`Timer::cancel`]。
    ///
    /// `LocalTimer::cancel` 不会撤销已经 spawn 的任务，真正的取消依靠 generation；
    /// `running == false` 时提前返回，避免重复 cancel。
    fn stop(&self) {
        if !self.running.get() {
            return;
        }
        self.running.set(false);
        self.generation.set(self.generation.get() + 1);
        self.timer.borrow_mut().cancel();
    }
}

/// 3D 彩蛋动画组件（TS `class EasterEgg3dAnimation implements Component`）。
///
/// 构造后即可作为 overlay 内容交给 `kiana-tui`：`render(width)` 返回整屏 ANSI 行，
/// `handle_input`/`handle_mouse` 处理关闭，`invalidate()` 丢弃尺寸与 ANSI 缓存。
/// 组件本身不拥有 overlay 句柄；真正的 `show_overlay`/`hide` 由 `start_animation` 的回调
/// 完成。内部的 `Inner` 与 `Driver` 都是 `Rc`，`Driver` 的回调只持 `Weak`，因此
/// 组件被丢弃后定时器不会让动画复活。
pub struct EasterEgg3dAnimation {
    inner: Rc<RefCell<Inner>>,
    driver: Rc<Driver>,
}

impl EasterEgg3dAnimation {
    /// TS `constructor(tui, screen, model, colors, onDone)`，参数按 Rust 的可注入形态展开。
    ///
    /// * `screen`：切换全屏前抓下的行，交给 `prepare_cells` 解析。
    /// * `model`：由 [`kiana_logo_model`] / [`armin_model`] 构建，光栅的相机距离取自它。
    /// * `colors`：本帧使用的前景/背景；lazy 路径先传兜底色，随后由 `resolve_colors` 覆盖。
    /// * `on_done`：收尾回调，由 `finish_now` 或 `finish_on_render` 取出并执行**至多一次**。
    ///   组件在 `timer_active` 仍为 true 时被丢弃不会自动调用它（只有设置了 `on_dispose`
    ///   才会收到销毁通知），直接构造的宿主需要自行保证生命周期。
    /// * `rows`：`tui.terminal.rows`。
    /// * `request_render`：`tui.requestRender()`；`None` 表示无人接收重绘请求。
    /// * `clock`：`performance.now()`（毫秒）；`start_time`/`last_render` 都在构造时取一次。
    ///
    /// 定时器默认未安装（[`NoopTimer`] 语义）；[`EasterEgg3dAnimation::set_timer`] 安装后
    /// 立即按 `FRAME_MS` 开始排程（对应 TS 构造里的 `setInterval`）。未安装时
    /// `timer_running` 保持 false，`render` 会按无定时器路径自行收尾并逐帧请求重绘。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        screen: Vec<String>,
        model: Model,
        colors: EasterEggColors,
        on_done: impl FnMut() + 'static,
        rows: TerminalRows,
        request_render: Option<RequestRenderCallback>,
        clock: EasterEggClock,
    ) -> Self {
        let start_time = clock();
        let raster = BlockRaster::new(model.camera_distance);
        let timer_running = Rc::new(Cell::new(false));
        let inner = Rc::new(RefCell::new(Inner {
            screen,
            model,
            foreground: colors.foreground,
            background: colors.background,
            on_done: Some(Box::new(on_done)),
            on_render_finish: None,
            on_dispose: None,
            on_close_request: None,
            clock: Rc::clone(&clock),
            rows,
            request_render,
            start_time,
            last_render: start_time,
            colors_pending: false,
            timer_active: true,
            timer_running: Rc::clone(&timer_running),
            finish_requested: false,
            exit: None,
            shuffle: None,
            screen_width: -1,
            screen_height: -1,
            stars: Vec::new(),
            cells: Vec::new(),
            ansi_cache: HashMap::new(),
            raster,
        }));
        let driver = Rc::new(Driver {
            timer: RefCell::new(Box::new(NoopTimer)),
            interval_ms: Cell::new(FRAME_MS as u64),
            running: timer_running,
            generation: Cell::new(0),
            inner: RefCell::new(Rc::downgrade(&inner)),
        });
        EasterEgg3dAnimation { inner, driver }
    }

    /// 安装/替换帧定时器并开始排程（宿主提供 `setInterval` 等价物）。
    ///
    /// 若动画已经 `finish`（`timer_active == false`），这里只替换定时器对象、不会重新启动，
    /// 避免收尾后的组件被宿主重新激活。
    pub fn set_timer(&mut self, timer: Box<dyn Timer>) {
        let active = self.inner.borrow().timer_active;
        *self.driver.timer.borrow_mut() = timer;
        if active {
            self.driver.start();
        }
    }

    /// 安装基于 tokio `LocalSet` 的定时器（交互模式的运行环境，见 ARCHITECTURE §8.1）。
    /// `spawn_local` 不可用时 [`LocalTimer`] 会把共享的 running 标志置回 false，
    /// 动画改为在 `render` 里按同一时钟自行收尾。
    fn install_local_timer(&mut self) {
        let running = Rc::clone(&self.driver.running);
        self.set_timer(Box::new(LocalTimer::new(running)));
    }

    /// 播放入口注入的安全回调（只操作 `Rc<Cell>` / 共享内部状态，不在输入分发里借 `TuiCore`）：
    /// * `on_render_finish`：无定时器时在 `render` 中收尾——把 overlay 标记为不可见并请求重绘。
    /// * `on_dispose`：动画被销毁但没走过 `finish()`（宿主移除 overlay）——复位模块级 `playing`。
    /// * `on_close_request`：输入分发里的第二次 `close()`——先隐藏帧、再安排任务真正 `hide()`。
    fn set_finish_fallbacks(
        &mut self,
        on_render_finish: impl FnMut() + 'static,
        on_dispose: impl FnMut() + 'static,
        on_close_request: impl FnMut() + 'static,
    ) {
        let mut inner = self.inner.borrow_mut();
        inner.on_render_finish = Some(Box::new(on_render_finish));
        inner.on_dispose = Some(Box::new(on_dispose));
        inner.on_close_request = Some(Box::new(on_close_request));
    }

    /// 延迟时间线到终端颜色查询结束（lazy 入口；见 [`Inner::resolve_colors`]）。
    fn defer_colors(&mut self) {
        self.inner.borrow_mut().colors_pending = true;
    }

    /// 与动画共享的内部状态（供播放入口挂终端颜色更新任务）。
    fn shared_inner(&self) -> Rc<RefCell<Inner>> {
        Rc::clone(&self.inner)
    }

    /// TS `close()`：播放退出动画；第二次调用立即结束。
    ///
    /// 第一次调用冻结当前时间与旋转角、计算目标整圈角与拼图偏移；之后的 `render` 在
    /// 1.1 秒（`EXIT_DURATION`）内完成退出：模型在进度 0.8 时先落位，屏幕按相反时间线
    /// 重组到 1.0。第二次调用走 `finish_now`，直接执行 `on_done`（复位 `PLAYING`、
    /// 隐藏 overlay），不再播放动画。
    pub fn close(&mut self) {
        let callback = self.inner.borrow_mut().close();
        if let Some(mut callback) = callback {
            callback();
        }
    }

    /// 动画是否还会推进（TS `this.timer !== undefined`）。
    ///
    /// 直接返回 `timer_active`：收尾（`finish_now`/`finish_on_render`）后为 false，即使
    /// overlay 条目还在栈里没被移除。
    pub fn is_playing(&self) -> bool {
        self.inner.borrow().timer_active
    }

    /// 当前退出进度（0..1；未退出时 0）。
    ///
    /// 基于 `clock()` 与第一次 `close()` 记录的 `exit.start` 计算，并 clamp 到 0..1；
    /// 未开始退出时返回 0。它决定退出动画的倒放速度，不改变 `is_playing`。
    pub fn exit_progress(&self) -> f64 {
        self.inner.borrow().exit_progress()
    }

    /// 底层光栅（测试/宿主查询）。
    ///
    /// 返回的 `Ref` 借用着 `Inner`；在它存活期间 `render` 无法借用同一 `RefCell`，因此
    /// 调用方应只做读取并尽快释放。内容反映最近一次 `render` 的结果。
    pub fn raster(&self) -> std::cell::Ref<'_, BlockRaster> {
        std::cell::Ref::map(self.inner.borrow(), |inner| &inner.raster)
    }

    /// 测试接缝：暴露 TS 的私有 `prepareCells(width, foreground)` 供黄金对拍。
    #[doc(hidden)]
    pub fn prepare_cells_for_test(&self, width: usize) -> Vec<Vec<ScreenCell>> {
        let inner = self.inner.borrow();
        inner.prepare_cells(width, inner.foreground)
    }
}

impl Component for EasterEgg3dAnimation {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.inner.borrow_mut().render(width)
    }

    /// TS `handleInput(data)`：`tui.select.cancel` **或** `app.clear` → `close()`。
    fn handle_input(&mut self, data: &str) {
        // 终端颜色查询期间动画在 TS 里还不存在（`await` 尚未返回），此时不应由它响应
        // 关闭键（否则 `exit.time` 会基于被重置前的时间线）。
        if self.inner.borrow().colors_pending {
            return;
        }
        let keybindings = get_keybindings();
        if keybindings.matches(data, "tui.select.cancel") || keybindings.matches(data, "app.clear")
        {
            self.inner.borrow_mut().close_in_input();
        }
    }

    /// TS `handleMouse(event)`：任意类型都返回 `{handled: true, render: false}`；
    /// 只有 `click` 才 `close()`。与 `handle_input` 一样，终端颜色查询期间（`colors_pending`）
    /// 的点击被忽略。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.event_type == TuiMouseEventType::Click && !self.inner.borrow().colors_pending {
            self.inner.borrow_mut().close_in_input();
        }
        Some(TuiMouseEventResult {
            handled: Some(true),
            render: Some(false),
            ..Default::default()
        })
    }

    /// 丢弃屏幕尺寸与 ANSI 缓存，下一次 `render` 会重新解析抓屏并重建星空。
    ///
    /// 只清尺寸标记与 `ansi_cache`，不直接清 `cells`/`stars`（它们由尺寸变化分支重建，
    /// 提前清空会让一次未重算的合成读到空缓冲）。它由 `kiana-tui` 的 `invalidate_all`
    /// 转发到 overlay 组件。
    fn invalidate(&mut self) {
        let mut inner = self.inner.borrow_mut();
        inner.screen_width = -1;
        inner.screen_height = -1;
        inner.ansi_cache.clear();
    }
}

// ---------------------------------------------------------------------------
// 入口（eager + lazy）
// ---------------------------------------------------------------------------

/// 要播放哪个彩蛋（TS `type EasterEgg3d`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EasterEgg3d {
    /// kiana logo 从 header logo（`column`, `row` 是左上格）起飞。
    KianaLogo {
        /// header logo 左上角列（终端格）。
        column: usize,
        /// header logo 左上角行（终端格）。
        row: usize,
    },
    /// `/arminsayshi` 的 3D Armin。
    Armin,
}

/// 模块级单例（TS `let playing = false`）。
static PLAYING: AtomicBool = AtomicBool::new(false);

/// 当前是否有 3D 彩蛋在播放（测试/宿主查询）。
///
/// 这是全进程单例（`static AtomicBool`，`SeqCst`），不区分 kiana logo 与 Armin，也不区分
/// 渲染器；`PLAYING` 为 true 时两条播放路径都会拒绝新请求。它只在动画收尾（`on_done` /
/// `on_render_finish` / `on_dispose`）或 eager 路径发现已有 overlay 时复位。
pub fn is_playing() -> bool {
    PLAYING.load(Ordering::SeqCst)
}

/// 测试接缝：清空模块级 `playing`（TS 里只有 `onDone` 会复位它）。
#[doc(hidden)]
pub fn reset_playing_state_for_test() {
    PLAYING.store(false, Ordering::SeqCst);
    LAST_ANIMATION.with(|slot| *slot.borrow_mut() = None);
    LAST_ANIMATION_FINISHED.with(|slot| *slot.borrow_mut() = None);
}

thread_local! {
    /// 测试接缝：最近一次播放入口创建的动画（`Weak`；overlay 销毁后自动失效）。
    static LAST_ANIMATION: RefCell<Option<Weak<RefCell<Inner>>>> = const { RefCell::new(None) };
    /// 测试接缝：overlay 的可见性标志（`visible()` 回调读它）。
    static LAST_ANIMATION_FINISHED: RefCell<Option<Rc<Cell<bool>>>> = const { RefCell::new(None) };
}

/// 测试接缝：最近一次播放动画当前使用的前景/背景（含终端颜色异步更新后的值）。
#[doc(hidden)]
pub fn last_animation_colors_for_test() -> Option<EasterEggColors> {
    LAST_ANIMATION.with(|slot| {
        let inner = slot.borrow().as_ref()?.upgrade()?;
        let inner = inner.borrow();
        Some(EasterEggColors {
            foreground: inner.foreground,
            background: inner.background,
        })
    })
}

/// 测试接缝：动画的时间线是否仍在等终端颜色（lazy 入口）。
#[doc(hidden)]
pub fn last_animation_colors_pending_for_test() -> Option<bool> {
    LAST_ANIMATION.with(|slot| {
        let inner = slot.borrow().as_ref()?.upgrade()?;
        Some(inner.borrow().colors_pending)
    })
}

/// 测试接缝：最近一次播放动画的时间线已推进多少毫秒（`resolve_colors` 会把它归零）。
#[doc(hidden)]
pub fn last_animation_elapsed_ms_for_test() -> Option<f64> {
    LAST_ANIMATION.with(|slot| {
        let inner = slot.borrow().as_ref()?.upgrade()?;
        let inner = inner.borrow();
        Some(((inner.clock)() - inner.start_time).max(0.0))
    })
}

/// 测试接缝：overlay 的可见性标志（`close_in_input` 会立即把它置为 false）。
#[doc(hidden)]
pub fn last_animation_hidden_for_test() -> Option<bool> {
    LAST_ANIMATION_FINISHED.with(|slot| slot.borrow().as_ref().map(|finished| finished.get()))
}

/// 用 tokio 本地任务复刻 `setInterval`（交互模式跑在单线程 `LocalSet` 上）。
///
/// 没有 tokio 运行时（或不在 `LocalSet` 里）时不排程，并把共享的 running 标志置回 false，
/// 让 `Inner::finish_on_render` 接管收尾。[`Timer`] 的回调是 `Box<dyn FnOnce()>`
/// （非 `Send`），无法跨线程投递。
pub struct LocalTimer {
    running: Rc<Cell<bool>>,
}

impl LocalTimer {
    /// 用 `Driver` 共享的「定时器在跑」标志构造。
    pub fn new(running: Rc<Cell<bool>>) -> Self {
        LocalTimer { running }
    }
}

impl Timer for LocalTimer {
    fn set(&mut self, delay_ms: u64, callback: Box<dyn FnOnce()>) {
        if tokio::runtime::Handle::try_current().is_err() {
            self.running.set(false);
            return;
        }
        // `spawn_local` 在非 `LocalSet` 上下文会 panic；这里吞掉该 panic，让动画退化为
        // 「由宿主渲染轮询驱动 + render 自行收尾」，而不是让应用崩溃。
        let spawned = crate::components::markdown_transform::catch_unwind_guarded(
            std::panic::AssertUnwindSafe(|| {
                tokio::task::spawn_local(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms.max(1))).await;
                    callback();
                });
            }),
        )
        .is_ok();
        if !spawned {
            self.running.set(false);
        }
    }

    fn cancel(&mut self) {
        // 已排程的任务由 Driver 的 generation 检查丢弃。
    }
}

/// 在两种渲染器上分别发起 `queryTerminalColors({ timeoutMs })`。
///
/// 返回的 future 是 `'static` 的（不借用 renderer）：查询在调用时已写入终端，回复稍后由
/// 宿主的输入泵消费。`timeout_ms` 由调用方固定为 TS 的 100 ms。
fn query_terminal_colors(
    renderer: &mut InteractiveRenderer,
    timeout_ms: u64,
) -> Pin<Box<dyn Future<Output = TerminalColors> + 'static>> {
    match renderer {
        InteractiveRenderer::Regular(renderer) => renderer
            .base_mut()
            .query_terminal_colors_with_late_reply(timeout_ms, None),
        InteractiveRenderer::Fullscreen(renderer) => renderer
            .base_mut()
            .query_terminal_colors_with_late_reply(timeout_ms, None),
    }
}

/// 把终端读线程排队的输入喂给 TUI（宿主事件循环的 `pumpPendingInput`）。
fn pump_pending_input(renderer: &mut InteractiveRenderer) {
    match renderer {
        InteractiveRenderer::Regular(renderer) => renderer.base_mut().pump_pending_input(),
        InteractiveRenderer::Fullscreen(renderer) => renderer.base_mut().pump_pending_input(),
    }
}

/// `queryTerminalColors` 的 await 等价物：查询期间由本函数泵入终端输入。
///
/// 调用方（`play_easter_egg_3d`）在整个 await 期间持有 `&mut InteractiveRenderer`，
/// 宿主的 `handleTerminalInput` 无法运行，OSC 10/11 回复就永远没人消费（future 必然
/// 在 100 ms 截止时刻拿到空快照）。TS 的 promise 不阻塞事件循环，回复由事件循环消费；
/// 这里用 2 ms 的轮询把同样的输入泵进来。
async fn query_terminal_colors_pumping(
    renderer: &mut InteractiveRenderer,
    timeout_ms: u64,
) -> TerminalColors {
    let mut query = query_terminal_colors(renderer, timeout_ms);
    loop {
        tokio::select! {
            colors = &mut query => return colors,
            _ = tokio::time::sleep(std::time::Duration::from_millis(2)) => {
                pump_pending_input(renderer);
            }
        }
    }
}

/// TS 的兜底颜色：`foreground = toRgb(theme.colors.text)`，`background = dark ? 黑 : 白`。
///
/// 主题尚未报告 appearance 时按暗色处理（`unwrap_or(true)`，即黑底）。`theme_color` 在
/// 主题未初始化时会 panic。
fn fallback_colors() -> EasterEggColors {
    let dark = kiana_theme::THEME
        .appearance()
        .map(|appearance| appearance == kiana_theme::ThemeAppearance::Dark)
        .unwrap_or(true);
    EasterEggColors {
        foreground: theme_color(ThemeColor::Text),
        background: if dark {
            [0.0, 0.0, 0.0]
        } else {
            [255.0, 255.0, 255.0]
        },
    }
}

/// 把终端上报的颜色逐通道转成 [`EasterEggColors`]；缺失通道用 [`fallback_colors`] 补齐。
///
/// `TerminalColors` 的通道是 `u8`，这里提升为 f64 以保持与其余颜色运算一致。
fn colors_from_reported(reported: &TerminalColors) -> EasterEggColors {
    let fallback = fallback_colors();
    EasterEggColors {
        foreground: reported
            .foreground
            .map(|rgb| [f64::from(rgb.r), f64::from(rgb.g), f64::from(rgb.b)])
            .unwrap_or(fallback.foreground),
        background: reported
            .background
            .map(|rgb| [f64::from(rgb.r), f64::from(rgb.g), f64::from(rgb.b)])
            .unwrap_or(fallback.background),
    }
}

/// TS `showOverlay(animation, { anchor: "top-left", width: "100%", maxHeight: "100%" })`。
///
/// 覆盖整个屏幕的左上角锚点、100% 宽与 100% 最大高；动画自己按当前宽度/行数生成整屏内容，
/// 所以不需要 x/y 偏移。两条播放入口最终都经 `overlay_options_with_visibility` 使用它，
/// 额外挂上「收尾后不可见」的回调。
pub fn overlay_options() -> OverlayOptions {
    OverlayOptions {
        anchor: Some(OverlayAnchor::TopLeft),
        width: Some(SizeValue::Percent(100.0)),
        max_height: Some(SizeValue::Percent(100.0)),
        ..Default::default()
    }
}

/// 在 [`overlay_options`] 上加可见性回调：无定时器时动画在 `render` 里收尾后无法 `hide()`
/// （正持有 `TuiCore` 借用），改为把 overlay 标记为不可见——下一帧合成时跳过它，焦点在
/// 下一次输入时交还，`hasOverlay()` 立刻变回 false（与 TS `hide()` 的用户可见行为一致）。
fn overlay_options_with_visibility(finished: Option<Rc<Cell<bool>>>) -> OverlayOptions {
    let mut options = overlay_options();
    options.visible = finished.map(|finished| {
        Box::new(move |_columns: usize, _rows: usize| !finished.get())
            as Box<dyn Fn(usize, usize) -> bool>
    });
    options
}

/// 真正的播放（TS eager `playEasterEgg3d(tui, screen, egg)`）：先查终端颜色，再创建动画与
/// overlay。重复调用（模块级 `playing`）直接返回。
///
/// 调用方不应在持有渲染器 `RefCell` 借用的输入分发内部 await 本函数；宿主应先排空事件
/// （interactive-mode 的约定）。查询期间本函数会替调用方泵入终端输入（否则 OSC 10/11
/// 回复无人消费）；被泵入的输入会正常分发给聚焦组件/监听器，因此这些回调不能反过来借用
/// 同一个渲染器 `RefCell`（交互模式的输入监听器通常只操作共享标志，满足该约束）。
///
/// 查询结束（回复或 100 ms 超时）后才检查 `has_overlay()`：若已有其他 overlay，则复位
/// `PLAYING` 并放弃；否则用上报色（缺失通道回退主题色）装配动画。本函数没有返回值，
/// 播放是否真的开始对调用方不可直接区分，只能事后用 [`is_playing`] 观察。
pub async fn play_easter_egg_3d(
    renderer: &mut InteractiveRenderer,
    screen: Vec<String>,
    egg: EasterEgg3d,
) {
    if PLAYING.swap(true, Ordering::SeqCst) {
        return;
    }
    // 只有这一处 await：终端颜色查询最多等待 100 ms。
    // 调用方（宿主）在 await 期间持有渲染器借用，无法再 `handleTerminalInput`；
    // 这里替它泵入终端输入，让 OSC 10/11 回复被消费（TS 的事件循环在这段时间照常运行）。
    let reported = query_terminal_colors_pumping(renderer, 100).await;
    let colors = colors_from_reported(&reported);
    if renderer.has_overlay() {
        PLAYING.store(false, Ordering::SeqCst);
        return;
    }
    start_animation(renderer, screen, egg, colors, false);
}

/// 同步播放变体（lazy 入口使用）：立即建立 overlay 并原样显示抓屏，等终端颜色查询
/// 结束（回复或 100 ms 超时）后才构造时间线（见 [`Inner::resolve_colors`]）。
fn play_easter_egg_3d_now(
    renderer: &mut InteractiveRenderer,
    screen: Vec<String>,
    egg: EasterEgg3d,
) -> Option<Rc<RefCell<Inner>>> {
    if PLAYING.swap(true, Ordering::SeqCst) {
        return None;
    }
    Some(start_animation(
        renderer,
        screen,
        egg,
        fallback_colors(),
        true,
    ))
}

/// 两条播放入口共用的装配函数：建模型、注入宿主回调、安装定时器并 `show_overlay`。
///
/// `colors` 是动画初始使用的前景/背景；`defer_colors = true`（lazy 路径）时时间线先挂起，
/// 等 `spawn_terminal_color_update` 调 `resolve_colors` 才真正开始。函数返回共享的 `Inner`，
/// 调用方用来挂颜色任务或测试。
///
/// 装配的四个回调都只操作 `Rc<Cell>`/模块级标志，不借用渲染器：`on_done` 复位 `PLAYING`
/// 并 `hide()` overlay；`on_render_finish` 在无定时器收尾时把 overlay 标记为不可见并请求
/// 重绘；`on_dispose` 复位 `PLAYING` 并置 `finished`；`on_close_request` 先翻转可见性，
/// 再在 tokio 可用时 `spawn_local` 一个任务（先 `yield_now` 让输入分发返回）执行真正的
/// `finish_now`。
///
/// `rows` 回调通过 `renderer.terminal().try_borrow()` 取行数，借用失败（说明终端正被可变
/// 借用）时回退 24。
fn start_animation(
    renderer: &mut InteractiveRenderer,
    screen: Vec<String>,
    egg: EasterEgg3d,
    colors: EasterEggColors,
    defer_colors: bool,
) -> Rc<RefCell<Inner>> {
    let model = match egg {
        EasterEgg3d::Armin => armin_model(theme_color(ThemeColor::Accent)),
        EasterEgg3d::KianaLogo { column, row } => kiana_logo_model((column, row)),
    };
    let rows: TerminalRows = {
        let terminal = renderer.terminal();
        Rc::new(move || {
            terminal
                .try_borrow()
                .map(|terminal| terminal.rows())
                .unwrap_or(24)
        })
    };
    let request_render: RequestRenderCallback = {
        let flag = renderer.render_requested_handle();
        Rc::new(move || flag.set(true))
    };
    let overlay_slot: Rc<RefCell<Option<Box<dyn OverlayHandle>>>> = Rc::new(RefCell::new(None));
    let on_done = {
        let slot = Rc::clone(&overlay_slot);
        move || {
            PLAYING.store(false, Ordering::SeqCst);
            if let Some(mut handle) = slot.borrow_mut().take() {
                handle.hide();
            }
        }
    };
    // 无定时器收尾与「overlay 被外部移除」都只能做 Rc<Cell> 级别的操作。
    let finished = Rc::new(Cell::new(false));
    let soft_finished = Rc::clone(&finished);
    let soft_request = Rc::clone(&request_render);
    let dispose_finished = Rc::clone(&finished);
    let mut animation = EasterEgg3dAnimation::new(
        screen,
        model,
        colors,
        on_done,
        rows,
        Some(Rc::clone(&request_render)),
        Rc::new(monotonic_clock),
    );
    let inner = animation.shared_inner();
    LAST_ANIMATION.with(|slot| *slot.borrow_mut() = Some(Rc::downgrade(&inner)));
    LAST_ANIMATION_FINISHED.with(|slot| *slot.borrow_mut() = Some(Rc::clone(&finished)));
    let close_inner = Rc::downgrade(&inner);
    let close_finished = Rc::clone(&finished);
    let close_request = Rc::clone(&request_render);
    animation.set_finish_fallbacks(
        move || {
            PLAYING.store(false, Ordering::SeqCst);
            soft_finished.set(true);
            // `hide()` 会请求重绘；不可见化后要自己请求一次，让底层界面重画。
            soft_request();
        },
        move || {
            PLAYING.store(false, Ordering::SeqCst);
            dispose_finished.set(true);
        },
        move || {
            // 输入分发内部：先让本帧合并时跳过 overlay，再安排任务执行真正的 finish()。
            close_finished.set(true);
            close_request();
            if tokio::runtime::Handle::try_current().is_ok() {
                let weak = close_inner.clone();
                let _ = crate::components::markdown_transform::catch_unwind_guarded(
                    std::panic::AssertUnwindSafe(|| {
                        tokio::task::spawn_local(async move {
                            // 让当前输入分发先返回（`TuiCore` 在分发期间被可变借用）。
                            tokio::task::yield_now().await;
                            if let Some(inner) = weak.upgrade() {
                                let callback = inner.borrow_mut().finish_now();
                                if let Some(mut callback) = callback {
                                    callback();
                                }
                            }
                        });
                    }),
                );
            }
        },
    );
    if defer_colors {
        animation.defer_colors();
    }
    if tokio::runtime::Handle::try_current().is_ok() {
        animation.install_local_timer();
    }
    let handle = renderer.show_overlay(
        Box::new(animation),
        overlay_options_with_visibility(Some(finished)),
    );
    *overlay_slot.borrow_mut() = Some(handle);
    inner
}

/// 懒加载路径的终端颜色查询（对应 TS 动态 import 之后的 `await queryTerminalColors`）。
///
/// TS 的 lazy 包装会启动 async 的 eager 路径，先 `await tui.queryTerminalColors({ timeoutMs:
/// 100 })` 再创建动画。Rust 的 lazy 入口是同步签名（`playKianaLogo3d`/`playArmin3d` 冻结为
/// 同步、且接收 `&mut InteractiveRenderer`），既不能 await，也不能在等待期间让宿主借用
/// 渲染器（回复由宿主的 `handleTerminalInput` 消费）。因此 overlay 立即建立但**时间线
/// 挂起**：`render` 原样显示抓屏；查询结束（回复或超时）后 [`Inner::resolve_colors`]
/// 用终端上报的颜色构造动画并把 `start_time` 重置到那一刻——与 TS「先 await 再 new」
/// 的可见结果一致（不会出现兜底色闪一下、halo/灯光中途翻转或时间线偏移）。
/// 返回 false 表示没有可用的任务执行环境，调用方必须立即用兜底色完成解析。
fn spawn_terminal_color_update(
    renderer: &mut InteractiveRenderer,
    inner: Weak<RefCell<Inner>>,
) -> bool {
    if tokio::runtime::Handle::try_current().is_err() {
        return false;
    }
    // future 是 'static 的：查询在调用时写入终端，回复由宿主输入泵消费。
    let reported = query_terminal_colors(renderer, 100);
    let spawned = crate::components::markdown_transform::catch_unwind_guarded(
        std::panic::AssertUnwindSafe(|| {
            tokio::task::spawn_local(async move {
                let reported = reported.await;
                if let Some(inner) = inner.upgrade() {
                    inner
                        .borrow_mut()
                        .resolve_colors(colors_from_reported(&reported));
                }
            });
        }),
    );
    spawned.is_ok()
}

/// lazy 入口的私有实现（TS `playEasterEgg3d(tui, egg): boolean`）：
/// 非全屏 → `false`；已有 overlay → `true`（什么都不播）；否则抓屏并在**切换前**启动。
fn play_easter_egg_3d_lazy(renderer: &mut InteractiveRenderer, egg: EasterEgg3d) -> bool {
    if !matches!(renderer, InteractiveRenderer::Fullscreen(_)) {
        return false;
    }
    if renderer.has_overlay() {
        return true;
    }
    let screen = renderer.get_screen_lines();
    if let Some(inner) = play_easter_egg_3d_now(renderer, screen, egg)
        && !spawn_terminal_color_update(renderer, Rc::downgrade(&inner))
    {
        // 没有事件循环可以消费回复：直接视为 100 ms 超时（用兜底色构造）。
        inner.borrow_mut().resolve_colors(fallback_colors());
    }
    true
}

/// TS `playKianaLogo3d(tui, column, row): void`：从 header logo 起飞的 3D kiana logo。
///
/// `column`/`row` 是 header logo 左上角的终端格坐标，传给 [`kiana_logo_model`] 作为起始位置。
/// 返回 `()`（与 TS 相同：调用方不关心是否能播）。只在全屏模式下播放；主屏、已有 overlay、
/// 或已有一个彩蛋在播放时静默不做任何事。
pub fn play_kiana_logo_3d(renderer: &mut InteractiveRenderer, column: usize, row: usize) {
    let _ = play_easter_egg_3d_lazy(renderer, EasterEgg3d::KianaLogo { column, row });
}

/// TS `playArmin3d(tui): boolean`：播放 3D Armin；不能播放（主屏）时返回 `false`，
/// 调用方据此回退到内联的 [`super::armin::ArminComponent`]。
///
/// 返回 `true` 不一定表示本次真的开始了动画：已有 overlay（或已有彩蛋在播放）时同样返回
/// `true` 但什么都不做（与 TS 的 `hasOverlay()` 分支一致）。`/arminsayshi` 因此不会在
/// overlay 之上再叠加内联 Armin。
pub fn play_armin_3d(renderer: &mut InteractiveRenderer) -> bool {
    play_easter_egg_3d_lazy(renderer, EasterEgg3d::Armin)
}
