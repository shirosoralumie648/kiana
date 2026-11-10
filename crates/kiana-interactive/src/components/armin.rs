//! components/armin.ts。
//!
//! `Armin says hi!`：XBM 半块像素画 + 7 种入场效果的彩蛋组件。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/armin.ts`。
//!
//! ## 调用方与依赖
//!
//! 生产入口只有一个：`/arminsayshi` 命令（`interactive_mode/commands/mod.rs` 分发到
//! `commands/misc.rs` 的 `handle_armin_says_hi`）。该函数先尝试全屏 3D 彩蛋
//! [`super::easter_egg_3d::play_armin_3d`]，主屏下不播放时才把本组件追加到聊天区。
//! `easter_egg_3d` 同时复用本模块的 [`is_armin_pixel`] 与两个尺寸常量构建 3D 模型——
//! 改位图 `BITS` 或宽高常量会同时影响两条路径。[`ArminComponent`] 经
//! `components/index.rs` 再导出到 crate 根与 SDK 面
//! （`kiana-coding-agent/tests/sdk_surface.rs` 锁定该名字）。
//! 黄金数据由 `tests/armin.rs` 对照 `tests/fixtures/armin.json` 逐帧对拍。
//!
//! ## 与 TS 的表示差异（运行时适配）
//!
//! * **定时器**：TS 构造时 `setInterval(1000 / fps)`，每帧 `tickEffect()` → `updateDisplay()`
//!   → `ui.requestRender()`，`tickEffect()` 返回 `true` 后 `clearInterval`。Rust 侧同样用
//!   一个**周期任务**：宿主可注入 [`Timer`]（[`ArminComponent::set_timer`]，与
//!   `Loader`/`TuiBase::set_timer` 同一抽象）；交互模式（`LocalSet`）里构造时会自动安装
//!   [`LocalTimer`]（`spawn_local`，见 `ARCHITECTURE.md` §8.1）。**每次定时器触发只推进
//!   一帧**（按固定节拍播放，不按墙钟补帧）；动画在未被渲染时也照常推进，重绘由定时器请求。
//!   `render` **不**请求重绘（TS 的 `render` 从不 `requestRender`）。
//! * **无定时器的退化**：没有 tokio 运行时 / `spawn_local` 不可用（例如只构造组件做渲染
//!   测试）时，节拍退化为**渲染时按经过时间补齐帧**（`advance_to_now`）：每次 `render` 用
//!   注入的时钟算出到此刻为止应执行多少帧（`floor(elapsed / interval)`），并在动画未结束
//!   时请求下一帧。这是与 TS 唯一的调度差异（停滞后的追赶、未渲染时不推进），只会在没有
//!   定时器时出现。
//! * **`TUI` 参数换成 [`RequestRenderCallback`]**（与 `Loader` 的约定一致）。
//! * **随机源**：`Math.random()` 换成可注入的 [`ArminRng`]（默认 xorshift64*），测试可给定
//!   序列。`Math.floor(random() * n)` 的消费次数与顺序逐字照抄。
//! * **时钟**：`performance.now()` 换成可注入的 [`ArminClock`]（毫秒，默认
//!   [`monotonic_clock`]）。
//! * `render` 返回**克隆的行**（TS 返回缓存数组的同一引用）；调用方不应依赖引用相等。
//! * `dispose()` 幂等（TS 的 `clearInterval` + `interval = null`）。
//!
//! ## 内部结构与一次播放
//!
//! 组件的全部可变状态放在私有的 `ArminState` 里，通过 `Rc<RefCell<_>>` 共享给三个入口：
//! 定时器回调、[`ArminComponent::tick`]（测试用的手动节拍）和 `render`（只有定时器不可用时
//! 才会推进帧）。三条路径推进一帧的步骤一致：调用 `tick_effect` → `grid_version += 1`
//! → `frames_done += 1`；`tick_effect` 返回 `true` 时把 `done` 置位。定时器路径随后
//! 停掉定时器；[`ArminComponent::tick`] 与退化补帧路径（`advance_to_now`）不触碰定时器。
//!
//! 1. **构造**：用第一次随机数从 [`Effect::ALL`] 选效果（下标 `floor(r * 7)`），一次性构建
//!    `final_grid`（18 行 × 31 列半块字符），再按效果初始化 `effect_state`；`dissolve` 还会把
//!    `current_grid` 覆盖成随机噪声。
//! 2. **播放**：每个效果函数只改 `current_grid`、读不变的 `final_grid`。typewriter/scanline/
//!    fade/dissolve 是单调填充，rain/crt 每帧先清空再重画；不要假设 `current_grid` 只会增加。
//! 3. **渲染**：`render` 按 `(width, grid_version)` 缓存行。`grid_version` 每推进一帧必变，
//!    因此缓存命中的唯一情况是「宽度相同且这一帧没有推进」。
//!
//! ## 不变量与注意事项
//!
//! * `final_grid` 构造后不再修改；`current_grid` 始终是 18 行 × 31 列（由各效果自行维护）。
//! * `done` 与 `disposed` 一旦置位不再回退；两者任一为真时 [`ArminComponent::tick`] 返回
//!   `true` 且不再改状态。
//! * 所有状态是 `Rc`/`RefCell`，组件只能在 UI 线程（`LocalSet`）构造和使用；定时器回调通过
//!   `LocalTimer` 的 `spawn_local` 回到同一线程，`request_render` 在释放 `RefCell` 借用之后
//!   才调用（避免宿主在回调里重入渲染导致二次借用）。
//! * `rain` 只有在**每一列**都落定后才返回结束。本组件位图的第 0 列与第 30 列整列为空，
//!   这两列找不到 `target_row`、永远不会 `settled`，所以 `rain` 实际不会自行结束——黄金数据
//!   里 800 帧的最后一帧 `done` 仍为 false（见 `tests/armin.rs` 与 `tests/fixtures/armin.json`）。
//!   这是与 TS 相同的既有行为，不要为了「让动画停下」而改动结束判定。

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use kiana_theme::ThemeColor;
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::tui::{Component, NoopTimer, Timer, monotonic_clock};

use super::easter_egg_3d::LocalTimer;

/// `ARMIN_WIDTH`：位图与显示网格的列数。
pub const ARMIN_WIDTH: usize = 31;
/// `ARMIN_HEIGHT`：位图的行数（半块渲染后是 18 行）。
pub const ARMIN_HEIGHT: usize = 36;

// 模块内别名：效果函数只关心网格尺寸，用短名少写重复的 `ARMIN_` 前缀。
const WIDTH: usize = ARMIN_WIDTH;
const HEIGHT: usize = ARMIN_HEIGHT;

/// XBM 位图：36 行 × ceil(31/8)=4 字节 = 144 字节，LSB first，**1 = 背景、0 = 前景**。
const BITS: [u8; 144] = [
    0xff, 0xff, 0xff, 0x7f, 0xff, 0xf0, 0xff, 0x7f, 0xff, 0xed, 0xff, 0x7f, 0xff, 0xdb, 0xff, 0x7f,
    0xff, 0xb7, 0xff, 0x7f, 0xff, 0x77, 0xfe, 0x7f, 0x3f, 0xf8, 0xfe, 0x7f, 0xdf, 0xff, 0xfe, 0x7f,
    0xdf, 0x3f, 0xfc, 0x7f, 0x9f, 0xc3, 0xfb, 0x7f, 0x6f, 0xfc, 0xf4, 0x7f, 0xf7, 0x0f, 0xf7, 0x7f,
    0xf7, 0xff, 0xf7, 0x7f, 0xf7, 0xff, 0xe3, 0x7f, 0xf7, 0x07, 0xe8, 0x7f, 0xef, 0xf8, 0x67, 0x70,
    0x0f, 0xff, 0xbb, 0x6f, 0xf1, 0x00, 0xd0, 0x5b, 0xfd, 0x3f, 0xec, 0x53, 0xc1, 0xff, 0xef, 0x57,
    0x9f, 0xfd, 0xee, 0x5f, 0x9f, 0xfc, 0xae, 0x5f, 0x1f, 0x78, 0xac, 0x5f, 0x3f, 0x00, 0x50, 0x6c,
    0x7f, 0x00, 0xdc, 0x77, 0xff, 0xc0, 0x3f, 0x78, 0xff, 0x01, 0xf8, 0x7f, 0xff, 0x03, 0x9c, 0x78,
    0xff, 0x07, 0x8c, 0x7c, 0xff, 0x0f, 0xce, 0x78, 0xff, 0xff, 0xcf, 0x7f, 0xff, 0xff, 0xcf, 0x78,
    0xff, 0xff, 0xdf, 0x78, 0xff, 0xff, 0xdf, 0x7d, 0xff, 0xff, 0x3f, 0x7e, 0xff, 0xff, 0xff, 0x7f,
];

const BYTES_PER_ROW: usize = WIDTH.div_ceil(8);
/// `Math.ceil(HEIGHT / 2)`：半块渲染后的行数。
const DISPLAY_HEIGHT: usize = HEIGHT.div_ceil(2);
/// `"ARMIN SAYS HI"`（13 个字符，**不截断**，见 spec §2.60.6）。
const MESSAGE: &str = "ARMIN SAYS HI";

/// 7 种入场效果（TS `type Effect` / `EFFECTS`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// 逐格打字机。
    Typewriter,
    /// 逐行扫描线。
    Scanline,
    /// 列雨。
    Rain,
    /// 随机淡入。
    Fade,
    /// CRT 从中间向外展开。
    Crt,
    /// 故障抖动。
    Glitch,
    /// 噪声消散。
    Dissolve,
}

impl Effect {
    /// 全部效果，**声明顺序即 `EFFECTS` 顺序**（构造时按随机下标的映射依赖它）。
    pub const ALL: [Effect; 7] = [
        Effect::Typewriter,
        Effect::Scanline,
        Effect::Rain,
        Effect::Fade,
        Effect::Crt,
        Effect::Glitch,
        Effect::Dissolve,
    ];
}

/// `isArminPixel(x, y)`：`true` = 前景。
///
/// TS 只有 `y >= HEIGHT` 守卫、**没有** x 守卫：
/// * `x >= 31` 但 `byteIndex` 仍在 144 字节内时，会读到下一行（或 padding）的位；
/// * `byteIndex` 越界时 `BITS[byteIndex]` 是 `undefined`，`undefined >> n === 0`，因此
///   `((… >> bit) & 1) === 0` 为 **true**（前景）。
///
/// Rust 用安全索引复刻同一结果（`get` 越界时返回 `true`），调用方 `getChar` 只传
/// `x < 31`（见 spec §2.60.6）。
pub fn is_armin_pixel(x: usize, y: usize) -> bool {
    if y >= HEIGHT {
        return false;
    }
    let byte_index = y * BYTES_PER_ROW + x / 8;
    let bit_index = x % 8;
    match BITS.get(byte_index) {
        Some(byte) => ((byte >> bit_index) & 1) == 0,
        // JS: `(undefined >> bit) & 1 === 0` → true。
        None => true,
    }
}

/// 单元格的半块字符：`upper && lower → █`、`upper → ▀`、`lower → ▄`、否则空格。
///
/// `row` 是**显示行**（0..18），对应源位图的两行 `row * 2` 与 `row * 2 + 1`；一个字符格
/// 同时表达上下两个像素正是半块渲染的全部意义。
fn get_char(x: usize, row: usize) -> char {
    let upper = is_armin_pixel(x, row * 2);
    let lower = is_armin_pixel(x, row * 2 + 1);
    if upper && lower {
        '█'
    } else if upper {
        '▀'
    } else if lower {
        '▄'
    } else {
        ' '
    }
}

/// 由 XBM 位图一次性构建最终网格（`DISPLAY_HEIGHT` 行 × `WIDTH` 列）。
///
/// 构造时调用一次，此后只读：所有入场效果都是「从空网格逐渐逼近这个网格」。
fn build_final_grid() -> Vec<Vec<char>> {
    let mut grid = Vec::with_capacity(DISPLAY_HEIGHT);
    for row in 0..DISPLAY_HEIGHT {
        let mut line = Vec::with_capacity(WIDTH);
        for x in 0..WIDTH {
            line.push(get_char(x, row));
        }
        grid.push(line);
    }
    grid
}

/// 全空格网格；`rain` 与 `crt` 每帧从它开始重画。
fn create_empty_grid() -> Vec<Vec<char>> {
    vec![vec![' '; WIDTH]; DISPLAY_HEIGHT]
}

/// 可注入的随机源（TS `Math.random()`）。
///
/// 所有效果在**相同位置、相同顺序**消费随机数；`tests/armin.rs` 用从 TS 记录下来的序列
/// 逐帧重放，因此改动任何一处 `next_f64()` 调用的位置或次数都会破坏黄金对拍。
pub trait ArminRng {
    /// 返回 `[0, 1)` 的浮点数（与 `Math.random()` 相同）。
    fn next_f64(&mut self) -> f64;
}

/// 默认随机源：xorshift64*（种子固定时可逐帧复现）。
///
/// 只用于决定彩蛋的视觉效果，不承担任何安全用途；目的是让测试与调试可以固定种子。
#[derive(Debug, Clone)]
pub struct XorShiftRng {
    state: u64,
}

impl XorShiftRng {
    /// 用种子构造（`0` 会被替换为 golden-ratio 常量，避免退化）。
    pub fn new(seed: u64) -> Self {
        XorShiftRng {
            state: if seed == 0 {
                0x9e37_79b9_7f4a_7c15
            } else {
                seed
            },
        }
    }

    /// 用当前时间构造（生产默认）。
    pub fn from_time() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0x1234_5678_9abc_def0);
        XorShiftRng::new(nanos ^ 0x9e37_79b9_7f4a_7c15)
    }
}

impl ArminRng for XorShiftRng {
    fn next_f64(&mut self) -> f64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        let value = x.wrapping_mul(0x2545_f491_4f6c_dd1d);
        // 53 位尾数 → [0, 1)。
        ((value >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

/// 时钟（毫秒，单调递增；TS `performance.now()`）。
///
/// 只在「定时器不可用」的退化路径（`advance_to_now`）里被读取；定时器正常工作时，
/// 帧推进与墙钟无关，`start_time` 不会被使用。
pub type ArminClock = Rc<dyn Fn() -> f64>;

// 颜色在渲染时通过全局主题解析（TS 同样在 render 里调用 `theme.fg`）；主题未初始化时
// 直接 panic，而不是静默输出无色文本。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// JS `array.slice(0, end)` 的取元素个数（`end` 可为负；本组件只会遇到 `width == 0` 时的 -1）。
fn js_slice_take(len: usize, end: isize) -> usize {
    if end >= 0 {
        len.min(end as usize)
    } else {
        len.saturating_sub(end.unsigned_abs())
    }
}

/// 一列雨滴的状态。
#[derive(Debug, Clone, Copy)]
struct RainDrop {
    /// 当前下落位置（显示行号）；可以为负，表示还在屏幕上方。
    y: i64,
    /// 已经从底部固定住的行数；达到 `DISPLAY_HEIGHT` 表示该列完成。
    settled: usize,
}

/// 每种入场效果自己的步进状态；由 `init_effect` 建立，`tick_effect` 每次推进一格。
/// 各变体承载的进度值：`Typewriter.pos` 是下一个要写入的线性 cell 下标（行优先遍历
/// 18×31）；`Scanline.row` 是下一个要整行复制的显示行；`Rain.drops` 每列一个雨滴；
/// `Fade` / `Dissolve` 的 `positions` 是洗牌后的填充顺序、`idx` 是下一个要取的位置
/// （`Dissolve` 与 `Fade` 相同，但初始网格是随机噪声而不是空格）；
/// `Crt.expansion` 是从中间行向上下展开的半径；`Glitch.phase` 是已生成的故障帧数
/// （`glitch_frames` 固定为 8）。
///
/// 注意：不要给单个变体加 `///`——rustfmt 会把原本多行的短变体折叠成一行，
/// 破坏基线排版（见 docs/COMMENT_STYLE.md §7.2）。
#[derive(Debug)]
enum EffectState {
    Typewriter {
        pos: usize,
    },
    Scanline {
        row: usize,
    },
    Rain {
        drops: Vec<RainDrop>,
    },
    Fade {
        positions: Vec<(usize, usize)>,
        idx: usize,
    },
    Crt {
        expansion: usize,
    },
    Glitch {
        phase: usize,
        glitch_frames: usize,
    },
    Dissolve {
        positions: Vec<(usize, usize)>,
        idx: usize,
    },
}

/// 动画的全部可变状态（`render` 与宿主回调共享）。
///
/// 字段分三组：绘制数据（`final_grid` / `current_grid` / `effect_state`）、
/// `render` 的行缓存（`cached_*`，键为「宽度 + `grid_version`」）、以及宿主注入的
/// 副作用与时钟。`timer_running` 与定时器驱动器共享同一个 `Cell`，是「是否需要退化补帧」
/// 的唯一判据。
struct ArminState {
    effect: Effect,
    /// 目标网格，构造后只读。
    final_grid: Vec<Vec<char>>,
    /// 当前显示的网格，由各效果逐步绘制。
    current_grid: Vec<Vec<char>>,
    effect_state: EffectState,
    cached_lines: Vec<String>,
    cached_width: usize,
    cached_version: i64,
    /// 每推进一帧加一（即使这一帧的内容没有变化），用于让渲染缓存失效。
    grid_version: u64,
    request_render: Option<RequestRenderCallback>,
    rng: Box<dyn ArminRng>,
    clock: ArminClock,
    start_time: f64,
    /// 已执行的帧数（定时器、手动 `tick`、退化补帧三条路径都会累加）。
    frames_done: usize,
    done: bool,
    disposed: bool,
    /// 定时器是否真的在排程（[`LocalTimer`] 无法 `spawn_local` 时置回 false）。
    /// 为 false 时 `render` 用 [`ArminState::advance_to_now`] 退化驱动。
    timer_running: Rc<Cell<bool>>,
}

impl ArminState {
    /// 按当前效果初始化 `effect_state`（`Dissolve` 还会重写 `current_grid`）。
    ///
    /// 这里也是随机数消耗顺序的一部分：`Rain` 先为 31 列各取一次初始 `y`；`Fade`/`Dissolve`
    /// 先按行优先收集全部 cell，再 Fisher-Yates 洗牌；`Dissolve` 还要先为每个 cell 取一次
    /// 噪声字符。对拍测试按 TS 的同一 `Math.random()` 序列重放，调整顺序会立刻破坏黄金结果。
    fn init_effect(&mut self) {
        match self.effect {
            Effect::Typewriter => self.effect_state = EffectState::Typewriter { pos: 0 },
            Effect::Scanline => self.effect_state = EffectState::Scanline { row: 0 },
            Effect::Rain => {
                // 每列的起始高度独立：`-floor(r * 18 * 2)`，因此一开始可能远在屏幕上方。
                let mut drops = Vec::with_capacity(WIDTH);
                for _ in 0..WIDTH {
                    let y = -((self.rng.next_f64() * DISPLAY_HEIGHT as f64 * 2.0).floor() as i64);
                    drops.push(RainDrop { y, settled: 0 });
                }
                self.effect_state = EffectState::Rain { drops };
            }
            Effect::Fade => {
                // 先把所有 cell 按行优先收集，再打乱成随机填充顺序。
                let mut positions = Vec::with_capacity(DISPLAY_HEIGHT * WIDTH);
                for row in 0..DISPLAY_HEIGHT {
                    for x in 0..WIDTH {
                        positions.push((row, x));
                    }
                }
                fisher_yates(&mut positions, &mut *self.rng);
                self.effect_state = EffectState::Fade { positions, idx: 0 };
            }
            Effect::Crt => self.effect_state = EffectState::Crt { expansion: 0 },
            Effect::Glitch => {
                self.effect_state = EffectState::Glitch {
                    phase: 0,
                    glitch_frames: 8,
                }
            }
            Effect::Dissolve => {
                // 噪声只在初始化时生成一次（之后每帧只把已揭示的 cell 换成最终字符）。
                const CHARS: [char; 7] = [' ', '░', '▒', '▓', '█', '▀', '▄'];
                let mut grid = Vec::with_capacity(DISPLAY_HEIGHT);
                for _ in 0..DISPLAY_HEIGHT {
                    let mut line = Vec::with_capacity(WIDTH);
                    for _ in 0..WIDTH {
                        let index = (self.rng.next_f64() * CHARS.len() as f64).floor() as usize;
                        // 自定义 RNG 理论上可以返回 1.0；夹住下标避免越界。
                        line.push(CHARS[index.min(CHARS.len() - 1)]);
                    }
                    grid.push(line);
                }
                self.current_grid = grid;
                let mut positions = Vec::with_capacity(DISPLAY_HEIGHT * WIDTH);
                for row in 0..DISPLAY_HEIGHT {
                    for x in 0..WIDTH {
                        positions.push((row, x));
                    }
                }
                fisher_yates(&mut positions, &mut *self.rng);
                self.effect_state = EffectState::Dissolve { positions, idx: 0 };
            }
        }
    }

    /// 推进一帧，返回 `true` 表示动画结束（TS `tickEffect()`）。
    ///
    /// 每个分支只碰自己需要的字段。`EffectState` 与 `self.effect` 不匹配时返回 `true`：
    /// 构造后二者始终一致，这只是防御分支，不走正常路径。
    fn tick_effect(&mut self) -> bool {
        match self.effect {
            Effect::Typewriter => self.tick_typewriter(),
            Effect::Scanline => self.tick_scanline(),
            Effect::Rain => self.tick_rain(),
            Effect::Fade => self.tick_fade(),
            Effect::Crt => self.tick_crt(),
            Effect::Glitch => self.tick_glitch(),
            Effect::Dissolve => self.tick_dissolve(),
        }
    }

    /// 每帧填 3 个 cell（行优先）。越界检查在每次写入**之前**，所以最后一帧可能只填 1~2 个
    /// cell；写满 558 个 cell 后的下一次调用返回 `true`。
    fn tick_typewriter(&mut self) -> bool {
        let EffectState::Typewriter { pos } = &mut self.effect_state else {
            return true;
        };
        let pixels_per_frame = 3;
        for _ in 0..pixels_per_frame {
            let row = *pos / WIDTH;
            let x = *pos % WIDTH;
            if row >= DISPLAY_HEIGHT {
                return true;
            }
            self.current_grid[row][x] = self.final_grid[row][x];
            *pos += 1;
        }
        false
    }

    /// 每帧复制一整行；写满 18 行后的下一次调用返回 `true`（那一次不写任何行）。
    fn tick_scanline(&mut self) -> bool {
        let EffectState::Scanline { row } = &mut self.effect_state else {
            return true;
        };
        if *row >= DISPLAY_HEIGHT {
            return true;
        }
        for x in 0..WIDTH {
            self.current_grid[*row][x] = self.final_grid[*row][x];
        }
        *row += 1;
        false
    }

    /// 每帧先整屏清空，再逐列推进雨滴。
    ///
    /// 每列分两部分：底部已固定的 `settled` 行（从 `final_grid` 抄）和上方一个下落中的
    /// `▓`（位于 `drop.y` 行，可能仍在屏幕外）。当 `drop.y` 追上该列下一个非空目标行时，
    /// `settled` 抬到目标行（从底部数起），`y` 重置回屏幕上方，等待下一滴。
    ///
    /// 返回 `true` 的条件是**所有列**都 `settled >= DISPLAY_HEIGHT`。整列为空的列永远找不到
    /// 目标行，也就永不落定——本组件的第 0/30 列正是如此，详见模块文档。
    fn tick_rain(&mut self) -> bool {
        let Self {
            effect_state,
            current_grid,
            final_grid,
            rng,
            ..
        } = self;
        let EffectState::Rain { drops } = effect_state else {
            return true;
        };
        let mut all_settled = true;
        *current_grid = create_empty_grid();
        for x in 0..WIDTH {
            let drop = &mut drops[x];
            // Draw settled pixels: `for (row = H-1; row >= H - settled; row--)`.
            let mut row = DISPLAY_HEIGHT as isize - 1;
            let stop = DISPLAY_HEIGHT as isize - drop.settled as isize;
            while row >= stop && row >= 0 {
                current_grid[row as usize][x] = final_grid[row as usize][x];
                row -= 1;
            }
            if drop.settled >= DISPLAY_HEIGHT {
                continue;
            }
            all_settled = false;

            // 目标行 = 尚未固定区域里最低的非空格；`None` 表示该列剩余部分全是空格。
            // Find the target row for this column (lowest non-space pixel).
            let mut target_row: Option<usize> = None;
            let mut row = DISPLAY_HEIGHT as isize - 1 - drop.settled as isize;
            while row >= 0 {
                if final_grid[row as usize][x] != ' ' {
                    target_row = Some(row as usize);
                    break;
                }
                row -= 1;
            }

            drop.y += 1;
            // 只有落在可见行内才处理；未追上目标行时画一个下落标记。
            if drop.y >= 0 && drop.y < DISPLAY_HEIGHT as i64 {
                if let Some(target_row) = target_row
                    && drop.y >= target_row as i64
                {
                    drop.settled = DISPLAY_HEIGHT - target_row;
                    // 下一滴从屏幕上方重新开始（随机 1..=5 行的提前量）。
                    drop.y = -((rng.next_f64() * 5.0).floor() as i64) - 1;
                } else {
                    current_grid[drop.y as usize][x] = '▓';
                }
            }
        }
        all_settled
    }

    /// 每帧从洗牌序列取 15 个位置填成最终字符。
    ///
    /// 耗尽检查在每次写入**之前**，所以最后一帧可能不足 15 个；这正是 TS 的循环结构。
    fn tick_fade(&mut self) -> bool {
        let EffectState::Fade { positions, idx } = &mut self.effect_state else {
            return true;
        };
        let pixels_per_frame = 15;
        for _ in 0..pixels_per_frame {
            if *idx >= positions.len() {
                return true;
            }
            let (row, x) = positions[*idx];
            self.current_grid[row][x] = self.final_grid[row][x];
            *idx += 1;
        }
        false
    }

    /// 每帧清空后只画 `[mid-expansion, mid+expansion]` 这一段行，模拟 CRT 从中间展开。
    ///
    /// `expansion` 从 0 开始递增；第 19 次调用后 `expansion` 变成 19（> 18），返回 `true`。
    fn tick_crt(&mut self) -> bool {
        let Self {
            effect_state,
            current_grid,
            final_grid,
            ..
        } = self;
        let EffectState::Crt { expansion } = effect_state else {
            return true;
        };
        let mid_row = DISPLAY_HEIGHT / 2;
        *current_grid = create_empty_grid();
        let top = mid_row as i64 - *expansion as i64;
        let bottom = mid_row as i64 + *expansion as i64;
        let start = top.max(0) as usize;
        let end = bottom.min(DISPLAY_HEIGHT as i64 - 1);
        if end >= 0 {
            for row in start..=(end as usize) {
                for x in 0..WIDTH {
                    current_grid[row][x] = final_grid[row][x];
                }
            }
        }
        *expansion += 1;
        *expansion > DISPLAY_HEIGHT
    }

    /// 前 8 帧生成故障帧；第 9 次调用换成干净的 `final_grid` 并返回 `true`。
    ///
    /// 每行的随机数消耗顺序（黄金对拍依赖它）：先**无条件**取一次 `offset`；命中 30% 分支
    /// 就做水平循环位移（不再取其它随机数）；否则再取一次判断 20% 的整行替换，命中时还要
    /// 取一次目标行号；都不命中则保留原行（该行总共只消耗第一次随机数）。
    fn tick_glitch(&mut self) -> bool {
        let Self {
            effect_state,
            current_grid,
            final_grid,
            rng,
            ..
        } = self;
        let EffectState::Glitch {
            phase,
            glitch_frames,
        } = effect_state
        else {
            return true;
        };
        if *phase < *glitch_frames {
            let mut grid = Vec::with_capacity(final_grid.len());
            for row in final_grid.iter() {
                // JS 每行**无条件**先消耗一次随机数（offset），再决定分支。
                let offset = (rng.next_f64() * 7.0).floor() as i64 - 3;
                let mut glitch_row = row.clone();
                if rng.next_f64() < 0.3 {
                    // 两段 `slice` 拼接即循环位移：offset > 0 左移，< 0 从尾部取（右移）。
                    let mut shifted = Vec::with_capacity(row.len());
                    shifted.extend_from_slice(&js_slice_from(row, offset));
                    shifted.extend_from_slice(&js_slice_to(row, offset));
                    // 行本来就是 WIDTH 列，这里的截断与 TS 的 `slice(0, WIDTH)` 对齐，不改变内容。
                    shifted.truncate(WIDTH);
                    grid.push(shifted);
                    continue;
                }
                if rng.next_f64() < 0.2 {
                    let swap_row = (rng.next_f64() * DISPLAY_HEIGHT as f64).floor() as usize;
                    grid.push(final_grid[swap_row.min(DISPLAY_HEIGHT - 1)].clone());
                    continue;
                }
                glitch_row.truncate(WIDTH);
                grid.push(glitch_row);
            }
            *current_grid = grid;
            *phase += 1;
            return false;
        }
        *current_grid = final_grid.clone();
        true
    }

    /// 每帧从洗牌序列取 20 个位置，用 `final_grid` 的字符替换噪声。
    ///
    /// 噪声网格只在 `init_effect` 里生成一次；这里与 `tick_fade` 一样在每次写入前检查耗尽。
    fn tick_dissolve(&mut self) -> bool {
        let EffectState::Dissolve { positions, idx } = &mut self.effect_state else {
            return true;
        };
        let pixels_per_frame = 20;
        for _ in 0..pixels_per_frame {
            if *idx >= positions.len() {
                return true;
            }
            let (row, x) = positions[*idx];
            self.current_grid[row][x] = self.final_grid[row][x];
            *idx += 1;
        }
        false
    }

    /// 按经过时间补齐所有到期的帧（**只有定时器不可用时的退化路径**；TS 每个
    /// `setInterval` 回调只推进一帧，不按墙钟追赶）。
    fn advance_to_now(&mut self) {
        if self.done || self.disposed {
            return;
        }
        let interval = if self.effect == Effect::Glitch {
            1000.0 / 60.0
        } else {
            1000.0 / 30.0
        };
        let elapsed = (self.clock)() - self.start_time;
        // 注入的时钟可能返回 NaN 或回退值；非有限/非正数时保持现状，避免把 NaN 转成整数。
        if !elapsed.is_finite() || elapsed <= 0.0 {
            return;
        }
        let due = (elapsed / interval).floor();
        if !due.is_finite() || due <= 0.0 {
            return;
        }
        // 一次 render 最多补 10_000 帧，避免时钟大幅跳变时在这里长时间循环。
        let due = due.min(10_000.0) as usize;
        while self.frames_done < due {
            let done = self.tick_effect();
            self.grid_version = self.grid_version.wrapping_add(1);
            self.frames_done += 1;
            if done {
                self.done = true;
                break;
            }
        }
    }

    fn request_frame(&self) {
        if let Some(callback) = &self.request_render {
            callback();
        }
    }

    /// TS `this.interval !== null`：动画结束（`tickEffect` 返回 true）或 `dispose()` 之后，
    /// `stopAnimation()` 清掉 interval，组件从此完全静止、不再请求重绘。
    fn animating(&self) -> bool {
        !self.done && !self.disposed
    }

    /// 把 `current_grid` 排版成可见行，返回缓存行的克隆。
    ///
    /// 每行由「1 个前导空格 + accent 着色的裁剪内容 + 右侧补空格」组成，补到 `width - 1`
    /// 列（内容更宽时不补也不裁）；最后追加固定的 `ARMIN SAYS HI` 行，总行数恒为
    /// `DISPLAY_HEIGHT + 1 = 19`。
    ///
    /// 两个需要保持的边界：`width == 0` 时 `available_width == -1`，`js_slice_take` 复刻
    /// JS `slice(0, -1)`（每行保留 30 列，整行可见宽度反而变成 31）；消息行**从不裁剪**，
    /// `width < 14` 时会超出 `width`。测试 `render_width_overflow_matches_ts_message_rules`
    /// 锁定了这两个行为。
    fn render(&mut self, width: usize) -> Vec<String> {
        // 定时器在跑时严格照 TS：状态只由 interval 回调推进，`render` 只读。定时器不可用
        // 时才按经过时间补帧（退化路径，见模块文档）。
        let fallback = !self.timer_running.get();
        if fallback {
            self.advance_to_now();
        }
        if width == self.cached_width && self.cached_version == self.grid_version as i64 {
            // 缓存命中：退化路径下仍要请求下一帧，否则动画会停在原地（定时器路径由回调请求）。
            if fallback && self.animating() {
                self.request_frame();
            }
            return self.cached_lines.clone();
        }

        // 左边固定留 1 列；可用宽度刻意不做下限（TS 同样没有 `Math.max(1, ...)`）。
        const PADDING: isize = 1;
        let available_width = width as isize - PADDING;
        let mut lines = Vec::with_capacity(DISPLAY_HEIGHT + 1);
        for row in &self.current_grid {
            let take = js_slice_take(row.len(), available_width);
            let clipped: String = row[..take].iter().collect();
            let clipped_len = clipped.chars().count();
            let pad_right = (width as isize - PADDING - clipped_len as isize).max(0) as usize;
            lines.push(format!(
                " {}{}",
                fg(ThemeColor::Accent, &clipped),
                " ".repeat(pad_right)
            ));
        }

        // 消息行不裁剪，只补右侧空格：宽度不足时 TS 也是溢出的（见上面的边界说明）。
        let msg_pad_right =
            (width as isize - PADDING - MESSAGE.chars().count() as isize).max(0) as usize;
        lines.push(format!(
            " {}{}",
            fg(ThemeColor::Accent, MESSAGE),
            " ".repeat(msg_pad_right)
        ));

        self.cached_lines = lines;
        self.cached_width = width;
        self.cached_version = self.grid_version as i64;
        if fallback && self.animating() {
            self.request_frame();
        }
        self.cached_lines.clone()
    }
}

/// 标准 Fisher-Yates 洗牌：从末尾向前，每个元素消耗一次随机数并与 `[0, i]` 中的位置交换。
///
/// 消耗次数固定为 `len - 1`，这是与 TS 对拍的一部分。`j` 再夹一次 `i` 是防御性的：
/// trait 约定 `[0, 1)`，但自定义实现返回 1.0 时也不能越界。
fn fisher_yates(positions: &mut [(usize, usize)], rng: &mut dyn ArminRng) {
    let mut i = positions.len();
    while i > 1 {
        i -= 1;
        let j = (rng.next_f64() * (i + 1) as f64).floor() as usize;
        let j = j.min(i);
        positions.swap(i, j);
    }
}

/// JS `array.slice(start)`（`start` 在本组件里 ∈ [-3, 3]）。负数从尾部取。
fn js_slice_from(row: &[char], start: i64) -> Vec<char> {
    let len = row.len() as i64;
    let begin = if start < 0 {
        (len + start).max(0)
    } else {
        start.min(len)
    };
    row[begin as usize..].to_vec()
}

/// JS `array.slice(0, end)`（`end` 在本组件里 ∈ [-3, 3]）。负数表示「去掉尾部 |end| 个」。
fn js_slice_to(row: &[char], end: i64) -> Vec<char> {
    let len = row.len() as i64;
    let stop = if end < 0 {
        (len + end).max(0)
    } else {
        end.min(len)
    };
    row[..stop as usize].to_vec()
}

/// `setInterval(1000 / fps)` 的 Rust 等价物：每次触发只推进一帧，然后重新排程。
///
/// 与 [`crate::components::easter_egg_3d`] 的 `Driver` 同构；回调持有 `Weak`，组件销毁后
/// 自然停止（TS 的 interval 闭包会一直跑到动画结束，但状态已不属于任何可见组件）。
///
/// 每次触发是**一次性**定时器：回调执行一帧后再调用 `set` 排下一拍，而不是依赖平台的
/// interval 语义。取消靠 `generation`：`stop()` 递增它，已经 spawn、尚未执行的旧回调在
/// 升级后发现代数不符就丢弃。`LocalTimer::cancel` 是空实现（无法撤销已 spawn 的任务），
/// 因此代数检查是这里真正的取消机制。
struct ArminDriver {
    /// 实际排程器；测试可替换成手动触发的替身。
    timer: RefCell<Box<dyn Timer>>,
    /// 每拍延迟（毫秒），构造时按效果固定：glitch 16，其余 33；效果不会中途改变。
    interval_ms: Cell<u64>,
    /// 与 `ArminState::timer_running` 是同一个 `Cell`：驱动器置假后 `render` 会接管补帧。
    running: Rc<Cell<bool>>,
    /// 取消代数；每次 `stop()` +1。
    generation: Cell<u64>,
    /// 用 `Weak` 指向状态，避免驱动器延长组件生命周期。
    state: RefCell<Weak<RefCell<ArminState>>>,
}

impl ArminDriver {
    fn schedule(self: &Rc<Self>) {
        if !self.running.get() {
            return;
        }
        // 先把代数与 Weak 拷贝进回调：回调执行期间不借用 self。
        let generation = self.generation.get();
        let weak = self.state.borrow().clone();
        let this = Rc::downgrade(self);
        self.timer.borrow_mut().set(
            self.interval_ms.get(),
            Box::new(move || {
                let Some(this) = this.upgrade() else {
                    return;
                };
                // 被 stop() 或一次新的 start() 取代的旧排程直接丢弃。
                if !this.running.get() || this.generation.get() != generation {
                    return;
                }
                let Some(state) = weak.upgrade() else {
                    this.stop();
                    return;
                };
                let (done, request) = {
                    let mut state = state.borrow_mut();
                    if state.done || state.disposed {
                        (true, None)
                    } else {
                        let done = state.tick_effect();
                        state.grid_version = state.grid_version.wrapping_add(1);
                        state.frames_done += 1;
                        if done {
                            state.done = true;
                        }
                        // TS 在 `if (done) stopAnimation()` **之前**无条件 requestRender。
                        (done, state.request_render.clone())
                    }
                };
                // 借用已释放：requestRender 可能同步触发渲染，不能持有 RefCell 借用去调用它。
                if let Some(request) = request {
                    request();
                }
                if done {
                    this.stop();
                } else {
                    this.schedule();
                }
            }),
        );
    }

    /// 先停掉旧排程（递增代数、取消定时器），再启动新一轮。
    fn start(self: &Rc<Self>) {
        self.stop();
        self.running.set(true);
        self.schedule();
    }

    /// 幂等停止：未运行时直接返回；否则清标志、递增代数让在途回调失效，并取消定时器。
    fn stop(&self) {
        if !self.running.get() {
            return;
        }
        self.running.set(false);
        self.generation.set(self.generation.get() + 1);
        self.timer.borrow_mut().cancel();
    }
}

/// `Armin says hi!` 动画组件（TS `class ArminComponent implements Component`）。
///
/// 组合而非继承：共享状态在 `state`，节拍由 `driver` 负责，组件本身只把
/// [`Component::render`] 转给状态。动画推进通过构造时注入的 `request_render` 通知宿主重绘，
/// 组件不会自己调用渲染器。
///
/// 全部字段是 `Rc`/`RefCell`，既不是 `Send` 也不是 `Sync`：必须在 UI 线程构造、渲染与销毁。
pub struct ArminComponent {
    state: Rc<RefCell<ArminState>>,
    driver: Rc<ArminDriver>,
}

impl ArminComponent {
    /// TS `constructor(tui)`；`requestRender` 由宿主注入。
    ///
    /// `request_render == None` 时动画仍会推进（定时器照跑），但不会请求宿主重绘——
    /// 适合只查询网格的测试。随机源取当前时间种子，效果因此每次不同。
    pub fn new(request_render: Option<RequestRenderCallback>) -> Self {
        Self::with_rng(request_render, Box::new(XorShiftRng::from_time()))
    }

    /// 注入随机源（测试用）。
    ///
    /// 效果选择、rain 的初始高度、fade/dissolve 的洗牌顺序都由它决定；给定固定序列即可让
    /// 整段动画可复现。
    pub fn with_rng(request_render: Option<RequestRenderCallback>, rng: Box<dyn ArminRng>) -> Self {
        Self::with_rng_and_clock(request_render, rng, Rc::new(monotonic_clock))
    }

    /// 注入随机源与时钟（测试用；`clock` 返回毫秒）。
    ///
    /// 时钟只在「定时器不可用」的退化路径里参与推进；注入固定时钟可以让测试直接跨到动画
    /// 结束，而不用逐帧 `tick`。
    pub fn with_rng_and_clock(
        request_render: Option<RequestRenderCallback>,
        mut rng: Box<dyn ArminRng>,
        clock: ArminClock,
    ) -> Self {
        // 构造顺序固定（随机数消费顺序即对外可观测行为）：选效果 → 建最终网格 → 空网格 → initEffect()。
        // 效果下标用第一次随机数；`min` 防御返回 1.0 的自定义 RNG。
        let effect_index = (rng.next_f64() * Effect::ALL.len() as f64).floor() as usize;
        let effect = Effect::ALL[effect_index.min(Effect::ALL.len() - 1)];
        let final_grid = build_final_grid();
        let current_grid = create_empty_grid();
        let start_time = clock();
        let timer_running = Rc::new(Cell::new(false));
        let mut state = ArminState {
            effect,
            final_grid,
            current_grid,
            effect_state: EffectState::Typewriter { pos: 0 },
            cached_lines: Vec::new(),
            cached_width: 0,
            cached_version: -1,
            grid_version: 0,
            request_render,
            rng,
            clock,
            start_time,
            frames_done: 0,
            done: false,
            disposed: false,
            timer_running: Rc::clone(&timer_running),
        };
        state.init_effect();
        let state = Rc::new(RefCell::new(state));
        // driver 与 state 共享 running 标志；timer 先用 NoopTimer 占位，set_timer 时才真正排程。
        let driver = Rc::new(ArminDriver {
            timer: RefCell::new(Box::new(NoopTimer)),
            interval_ms: Cell::new(interval_ms_for(effect)),
            running: timer_running,
            generation: Cell::new(0),
            state: RefCell::new(Rc::downgrade(&state)),
        });
        let mut component = ArminComponent { state, driver };
        // 有 tokio 运行时上下文才尝试安装 LocalTimer。注意这只证明「在运行时里」，不证明
        // 「在 LocalSet 里」：后者的 spawn_local 会 panic，被 LocalTimer 捕获后把 running
        // 置回 false，render 随即改用退化补帧。
        if tokio::runtime::Handle::try_current().is_ok() {
            component.install_local_timer();
        }
        component
    }

    /// 安装/替换帧定时器并开始排程（宿主提供 `setInterval` 等价物）。
    ///
    /// 与 `Loader::set_timer` 的约定一致：测试注入手动替身；交互模式的宿主通常无需调用
    /// （`LocalSet` 里构造时已自动安装 [`LocalTimer`]）。
    ///
    /// 替换时先停掉旧排程；只有动画尚未结束且未 `dispose()` 才会立即开始新一轮。
    pub fn set_timer(&mut self, timer: Box<dyn Timer>) {
        self.driver.stop();
        *self.driver.timer.borrow_mut() = timer;
        if self.state.borrow().animating() {
            self.driver.start();
        }
    }

    /// 安装基于 tokio `LocalSet` 的定时器；`spawn_local` 不可用时 [`LocalTimer`] 会把共享的
    /// running 标志置回 false，`render` 随即接管为按经过时间补帧。
    fn install_local_timer(&mut self) {
        let running = Rc::clone(&self.driver.running);
        self.set_timer(Box::new(LocalTimer::new(running)));
    }

    /// 当前效果（测试/宿主查询）。构造时随机选定，之后不再改变。
    pub fn effect(&self) -> Effect {
        self.state.borrow().effect
    }

    /// 最终半块网格（18 行 × 31 列；测试/宿主查询）。
    ///
    /// 返回克隆，调用方修改它不会影响组件。
    pub fn final_grid(&self) -> Vec<Vec<char>> {
        self.state.borrow().final_grid.clone()
    }

    /// 当前半块网格（测试/宿主查询）。随每一帧变化；同样返回克隆。
    pub fn current_grid(&self) -> Vec<Vec<char>> {
        self.state.borrow().current_grid.clone()
    }

    /// 动画是否已结束（`tickEffect` 返回过 `true`）。
    pub fn is_done(&self) -> bool {
        self.state.borrow().done
    }

    /// 已执行的帧数（测试/宿主查询）。
    ///
    /// 定时器回调、手动 [`Self::tick`]、退化路径的补帧都计入同一个计数。
    pub fn frames_done(&self) -> usize {
        self.state.borrow().frames_done
    }

    /// 手动推进一帧（等价 TS 定时器回调的一次触发；测试用）。动画结束或 `dispose()` 后
    /// 返回 `true` 且不再改动状态。
    ///
    /// 它与定时器互不影响：不会停止/启动定时器，也不检查 `timer_running`；若两者同时运行，
    /// 帧会被推进两次。`request_render` 与 TS 一样在 `done` 的那一帧也会被调用。
    pub fn tick(&self) -> bool {
        let mut state = self.state.borrow_mut();
        if state.done || state.disposed {
            return true;
        }
        let done = state.tick_effect();
        state.grid_version = state.grid_version.wrapping_add(1);
        state.frames_done += 1;
        if done {
            state.done = true;
        }
        // TS 的 interval 回调在 `stopAnimation()` 之前**无条件** requestRender。
        state.request_frame();
        done
    }

    /// TS `dispose()`：`stopAnimation()`（幂等）。
    ///
    /// 只停止推进，不清空网格或缓存；`render` 仍可调用并显示当前帧。注意这是**固有方法**：
    /// `impl Component` 没有覆盖同名的 trait 钩子，经 `dyn Component` 调用 `dispose` 时走的是
    /// 默认空实现；组件真正被丢弃时由 [`Drop`] 兜底停止排程。
    pub fn dispose(&mut self) {
        self.state.borrow_mut().disposed = true;
        self.driver.stop();
    }
}

impl Drop for ArminComponent {
    fn drop(&mut self) {
        // TS 没有 dispose 调用时 interval 会跑完；Rust 侧组件销毁后状态已不可达，
        // 停掉排程即可（回调里的 Weak 也会让下一次触发自然结束）。
        self.driver.stop();
    }
}

/// JS 定时器延迟：`1000 / fps` 经过 ToInt32 截断（30 fps → 33 ms，glitch 60 fps → 16 ms）。
fn interval_ms_for(effect: Effect) -> u64 {
    if effect == Effect::Glitch { 16 } else { 33 }
}

impl Component for ArminComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.state.borrow_mut().render(width)
    }

    fn handles_input(&self) -> bool {
        // TS `ArminComponent` 没有 `handleInput`。
        false
    }

    fn invalidate(&mut self) {
        // TS：**只** `cachedWidth = 0`（不动 cachedVersion / cachedLines）。
        self.state.borrow_mut().cached_width = 0;
    }
}
