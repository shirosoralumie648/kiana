//! system-theme.ts（由终端颜色生成）。
//!
//! TS: `packages/coding-agent/src/modes/interactive/theme/system-theme.ts`（655 行）。
//!
//! `system` 主题把终端自己上报的颜色（前景/背景/16 色调色板）映射成全部 56 个 token：
//!
//! - 每个 token 属于一个色族（决定色相与饱和度的来源，ANSI slot）；
//! - 每个 token 有一组对比度规则（在背景与它所在的 panel 上必须达到某个 level）；
//! - level 是一条“目标 OKLab 亮度”曲线（对 surface 亮度的 6 次多项式），色相/饱和度来自
//!   调色板，亮度只由规则决定；
//! - 终端什么都没上报时退化成 ANSI 索引 0-15 + 终端默认色（中性 token 用 SGR 2 变暗）。
//!
//! 数值必须逐位复刻（测试用 WCAG 对比度与 OKLab 数值断言）：FAMILIES、TOKEN_FAMILIES、
//! TOKEN_SLOTS、LEVELS、RULES 的顺序、SOLVE_ORDER 的 DFS 后序都不能改。
//!
//! 与 TS 的实现差异（由 `tests/system_theme.rs` 的黄金文件测试逐 token 覆盖）：
//!
//! - 多项式用逐项乘积累加求幂（JS 的 `**` 走 V8 的 pow；两者在 ulp 级别可能不同，只有
//!   恰好跨过 u8 取整边界时才可能让十六进制输出不同）；
//! - `Math.min()`/`Math.max()` 对空数组分别返回 `+Infinity`/`-Infinity`：这里用
//!   `fold(+/-Infinity)` 保持同样的退化行为（实际 token 都有规则，不会出现空集合）。
//!
//! # 为什么需要这个模块
//!
//! 内置的 `dark`/`light` 主题是固定 JSON，可能与用户终端的背景、前景、调色板冲突。
//! `system` 主题在运行时读取终端上报的颜色并即时生成全部 56 个 token，让界面融入终端
//! 自己的配色，同时保证文字在背景和各种面板上仍然可读。没有它，用户只能手工写一份
//! 主题去匹配自己的终端。
//!
//! # 在 crate 中的位置
//!
//! 上游：`kiana_tui` 提供终端颜色类型与 OKLab/OKHSL/sRGB 转换。下游：`crate::theme`
//! 的 `create_system_theme()` 填好 [`SystemThemeInput`] 并消费
//! [`generate_system_theme_colors`] 的 [`SystemThemeColors`]；`crate::theme_controller`
//! 用主题名 [`SYSTEM_THEME_NAME`] 判断是否自动跟随终端颜色。本模块不依赖 crate 内其他
//! 模块的逻辑，只借用 token 类型与颜色值类型。
//!
//! # 执行流程
//!
//! 有背景色时的主路径：
//!
//! 1. [`terminal_appearance`] 从背景/前景判断终端是深色还是浅色；深色意味着 token 应当
//!    比它所在的 surface 更亮；
//! 2. 按 `solve_order()`（规则依赖的 DFS 后序）逐个 token 求解：对 token 的每条规则、
//!    每个 surface，用该 surface 的 OKLab 亮度查 `curve()` 的 level 曲线得到目标亮度，
//!    再把多个目标合并成一个（深色取最大、浅色取最小），最后按色族色相与 ANSI slot
//!    上色；
//! 3. 七个面板背景额外受 `limit_panel` 限制：生成色与白（深色）/黑（浅色）文字的对比度
//!    必须达到 4.5:1，必要时在“背景亮度 → 目标亮度”之间二分找最贴近背景的可行值；
//! 4. 初次求解（t = 0）中任一目标落在 `[0,1]` 之外或曲线不可达都会整体失败；此时在
//!    t ∈ `[0,2]` 上二分“放松参数”重新求解，t 会把目标距离压向最低可读 level；
//! 5. 三个正文 token（`text`/`userMessageText`/`toolTitle`）若终端自己的前景色已经够
//!    强，直接输出空串（交给终端默认色），否则用前景色的色相重新上色；
//! 6. 输出按 `TOKEN_FAMILIES` 顺序写入 `IndexMap`；空串表示“终端默认色”，由
//!    `crate::theme` 的 `add_token()` 翻译成 SGR 39/49。
//!
//! 没有背景色时走 `indexed_colors()`：不做任何色彩计算，非中性 token 在饱和度 > 0 时
//! 引用 ANSI 0-15 的索引，其余一律用终端默认色；中性前景 token 记入 `dim`（渲染时加
//! SGR 2 变暗）。
//!
//! # 注意事项
//!
//! - 纯函数：不读环境变量、不写全局状态、不做 IO；相同输入得到相同输出。唯一的失败是
//!   `saturation` 为 NaN 时的 panic（见 [`generate_system_theme_colors`]）。
//! - 所有表（`family()` 的常量、`TOKEN_FAMILIES`、`TOKEN_SLOTS`、`curve()`、`RULES`）
//!   的数值与顺序都来自 TS 参照实现。`tests/system_theme.rs` 的
//!   `matches_the_reference_implementation` 用 `tests/fixtures/theme_golden.json`
//!   逐 token 比较颜色文本/调色板索引；改表必须同步重新生成黄金文件。
//! - `TOKEN_FAMILIES` 必须覆盖全部 56 个 token（每个一次）：输出只按它插入，漏项会让
//!   主题缺少该 token。
//! - `palette` 必须正好 16 项，否则整份被忽略（见 `SystemThemeInput` 的 `palette` 字段）。
//! - 面板 token 也会被当成 surface 使用（`selectedBg`、`tool*Bg` 等），`scrollbarThumb`
//!   又画在 `scrollbarTrack` 上；被依赖的颜色必须先解出——这就是 `solve_order()` 存在
//!   的理由。

use std::collections::HashMap;
use std::sync::LazyLock;

use indexmap::IndexMap;
use kiana_tui::{
    RgbColor, TerminalColorScheme, color_to_okhsl, color_to_oklch, color_to_rgb, okhsl_color,
    oklab_to_okhsl_lightness, oklch_color, rgb_color,
};

use crate::theme_schema::ThemeColorValue;
use crate::theme_tokens::{ThemeColor, ThemeToken};

/// `SYSTEM_THEME_NAME = "system"`。
///
/// 这个主题名是保留字：`loadTheme("system")` 总是即时生成系统主题，用户无法用同名主题
/// 文件覆盖它；主题控制器也用该名字决定是否自动跟随终端颜色（见 `crate::theme` 与
/// `crate::theme_controller`）。
pub const SYSTEM_THEME_NAME: &str = "system";

// =============================================================================
// Recipe: color families and their tokens
// =============================================================================

/// 一个色族的 OKHSL 色相与饱和度曲线（`max` 在中亮度，向黑/白回落到 `min`）。
///
/// 生成颜色时色相取 `hue`（有调色板时改用调色板 slot 里的实测色相，见 `anchored`）；
/// 饱和度由 `saturation_curve` 按目标亮度在 `saturation_min`（亮度 0/1）与
/// `saturation_max`（亮度 0.5）之间插值。`slot` 是该色族默认使用的 ANSI 0-15 槽位，
/// 单个 token 可被 `TOKEN_SLOTS` 覆盖。
#[derive(Debug, Clone, Copy)]
struct Family {
    /// OKHSL 色相，单位为度。
    hue: f64,
    /// 亮度 0/1（黑/白）处的 OKHSL 饱和度。
    saturation_min: f64,
    /// 中亮度（0.5）处的 OKHSL 饱和度。
    saturation_max: f64,
    /// 取色相/饱和度所用的 ANSI 调色板 slot。
    slot: u8,
}

/// TS: `typeof FAMILIES` 的键。
///
/// 每个 token 恰好属于一个色族；`family()` 是 `FAMILIES[name]` 的对应物。用枚举而不是
/// 字符串键是为了让表在编译期固定、查找不分配内存，也不会拼错名字。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum FamilyName {
    Neutral,
    Blue,
    Green,
    Red,
    Yellow,
    Orange,
    Violet,
    Calamine,
    ThinkingSlate,
    ThinkingBlue,
    ThinkingPeriwinkle,
    ThinkingViolet,
    ThinkingMagenta,
    ThinkingRed,
}

impl FamilyName {
    /// TS: `FAMILIES[name]`。
    const fn family(self) -> Family {
        use FamilyName::*;
        match self {
            Neutral => Family {
                hue: 231.49,
                saturation_min: 0.02,
                saturation_max: 0.08,
                slot: 8,
            },
            Blue => Family {
                hue: 231.49,
                saturation_min: 0.1,
                saturation_max: 0.68,
                slot: 4,
            },
            Green => Family {
                hue: 158.68,
                saturation_min: 0.1,
                saturation_max: 0.76,
                slot: 2,
            },
            Red => Family {
                hue: 20.0,
                saturation_min: 0.1,
                saturation_max: 0.92,
                slot: 1,
            },
            Yellow => Family {
                hue: 82.36,
                saturation_min: 0.5,
                saturation_max: 1.0,
                slot: 3,
            },
            Orange => Family {
                hue: 52.0,
                saturation_min: 0.12,
                saturation_max: 0.85,
                slot: 3,
            },
            Violet => Family {
                hue: 295.0,
                saturation_min: 0.2,
                saturation_max: 0.6,
                slot: 5,
            },
            Calamine => Family {
                hue: 202.43,
                saturation_min: 0.1,
                saturation_max: 0.74,
                slot: 6,
            },
            ThinkingSlate => Family {
                hue: 231.49,
                saturation_min: 0.08,
                saturation_max: 0.2,
                slot: 4,
            },
            ThinkingBlue => Family {
                hue: 231.49,
                saturation_min: 0.2,
                saturation_max: 0.45,
                slot: 4,
            },
            ThinkingPeriwinkle => Family {
                hue: 263.25,
                saturation_min: 0.3,
                saturation_max: 0.6,
                slot: 6,
            },
            ThinkingViolet => Family {
                hue: 295.0,
                saturation_min: 0.4,
                saturation_max: 0.75,
                slot: 5,
            },
            ThinkingMagenta => Family {
                hue: 337.5,
                saturation_min: 0.5,
                saturation_max: 0.85,
                slot: 13,
            },
            ThinkingRed => Family {
                hue: 20.0,
                saturation_min: 0.95,
                saturation_max: 1.0,
                slot: 1,
            },
        }
    }
}

/// TS: `TOKEN_FAMILIES`（**声明顺序决定输出 key 顺序**）。
///
/// 全部 56 个 token 各出现一次；`generate_system_theme_colors` 只按这张表往结果里插入，
/// 因此漏项会直接让输出少一个 key（黄金测试断言 56 项，见 `tests/system_theme.rs`）。
const TOKEN_FAMILIES: &[(ThemeToken, FamilyName)] = &[
    (ThemeToken::SelectedBg, FamilyName::Blue),
    (ThemeToken::SearchMatchBg, FamilyName::Orange),
    (ThemeToken::UserMessageBg, FamilyName::Blue),
    (ThemeToken::CustomMessageBg, FamilyName::Violet),
    (ThemeToken::ToolPendingBg, FamilyName::Neutral),
    (ThemeToken::ToolSuccessBg, FamilyName::Green),
    (ThemeToken::ToolErrorBg, FamilyName::Red),
    (ThemeToken::Text, FamilyName::Neutral),
    (ThemeToken::UserMessageText, FamilyName::Neutral),
    (ThemeToken::CustomMessageText, FamilyName::Neutral),
    (ThemeToken::ToolTitle, FamilyName::Neutral),
    (ThemeToken::SyntaxOperator, FamilyName::Neutral),
    (ThemeToken::SyntaxPunctuation, FamilyName::Neutral),
    (ThemeToken::Muted, FamilyName::Neutral),
    (ThemeToken::Dim, FamilyName::Neutral),
    (ThemeToken::ThinkingText, FamilyName::Neutral),
    (ThemeToken::ToolOutput, FamilyName::Neutral),
    (ThemeToken::MdLinkUrl, FamilyName::Neutral),
    (ThemeToken::MdQuote, FamilyName::Neutral),
    (ThemeToken::MdQuoteBorder, FamilyName::Neutral),
    (ThemeToken::MdHr, FamilyName::Neutral),
    (ThemeToken::MdCodeBlockBorder, FamilyName::Neutral),
    (ThemeToken::ToolDiffContext, FamilyName::Neutral),
    (ThemeToken::SyntaxComment, FamilyName::Neutral),
    (ThemeToken::ScrollbarTrack, FamilyName::Neutral),
    (ThemeToken::ScrollbarThumb, FamilyName::Neutral),
    (ThemeToken::SearchMatchText, FamilyName::Neutral),
    (ThemeToken::BorderMuted, FamilyName::Neutral),
    (ThemeToken::Accent, FamilyName::Violet),
    (ThemeToken::BorderAccent, FamilyName::Violet),
    (ThemeToken::CustomMessageLabel, FamilyName::Violet),
    (ThemeToken::MdCode, FamilyName::Violet),
    (ThemeToken::MdListBullet, FamilyName::Violet),
    (ThemeToken::SyntaxType, FamilyName::Violet),
    (ThemeToken::Border, FamilyName::Blue),
    (ThemeToken::MdLink, FamilyName::Blue),
    (ThemeToken::SyntaxKeyword, FamilyName::Blue),
    (ThemeToken::SyntaxVariable, FamilyName::Calamine),
    (ThemeToken::Success, FamilyName::Green),
    (ThemeToken::MdCodeBlock, FamilyName::Green),
    (ThemeToken::ToolDiffAdded, FamilyName::Green),
    (ThemeToken::BashMode, FamilyName::Green),
    (ThemeToken::SyntaxNumber, FamilyName::Green),
    (ThemeToken::Error, FamilyName::Red),
    (ThemeToken::ToolDiffRemoved, FamilyName::Red),
    (ThemeToken::Warning, FamilyName::Yellow),
    (ThemeToken::MdHeading, FamilyName::Yellow),
    (ThemeToken::SyntaxFunction, FamilyName::Yellow),
    (ThemeToken::SyntaxString, FamilyName::Orange),
    (ThemeToken::ThinkingOff, FamilyName::Neutral),
    (ThemeToken::ThinkingMinimal, FamilyName::ThinkingSlate),
    (ThemeToken::ThinkingLow, FamilyName::ThinkingBlue),
    (ThemeToken::ThinkingMedium, FamilyName::ThinkingPeriwinkle),
    (ThemeToken::ThinkingHigh, FamilyName::ThinkingViolet),
    (ThemeToken::ThinkingXhigh, FamilyName::ThinkingMagenta),
    (ThemeToken::ThinkingMax, FamilyName::ThinkingRed),
];

/// TS: `TOKEN_SLOTS` —— 覆盖色族默认 slot 的 token。
///
/// 同一色族里个别 token 需要借用别的 ANSI 槽位：`syntaxString` 用 2（绿，而它属于
/// Orange 色族）、`syntaxNumber` 用 5（紫）。`searchMatchBg` 的 3 与其色族默认值相同，
/// 保留这一项是为了与 TS 表逐项对应。查不到的 token 回落到色族的 `slot` 字段。
const TOKEN_SLOTS: &[(ThemeToken, u8)] = &[
    (ThemeToken::SyntaxString, 2),
    (ThemeToken::SyntaxNumber, 5),
    (ThemeToken::SearchMatchBg, 3),
];

/// token → 色族。线性扫描 56 项；表未覆盖的 token 回落到 `Neutral`（当前表覆盖全部
/// token，这个分支只是防御）。
fn token_family(token: ThemeToken) -> FamilyName {
    TOKEN_FAMILIES
        .iter()
        .find(|(candidate, _)| *candidate == token)
        .map(|(_, family)| *family)
        .unwrap_or(FamilyName::Neutral)
}

/// token 的显式 slot 覆盖；`None` 表示使用色族的默认 slot。
fn token_slot(token: ThemeToken) -> Option<u8> {
    TOKEN_SLOTS
        .iter()
        .find(|(candidate, _)| *candidate == token)
        .map(|(_, slot)| *slot)
}

// =============================================================================
// Contrast levels and rules
// =============================================================================

/// 目标亮度曲线：surface 的 OKLab 亮度 → token 需要的 OKLab 亮度。
///
/// `reachable` 是该 level 有效的 surface 亮度区间：区间外的 surface 对这个 level 来说
/// 太暗或太亮，TS 的 `levelTarget()` 返回 `undefined`，这里用 `None` 表达。深色主题的
/// 曲线只覆盖偏暗的 surface，浅色主题的曲线只覆盖偏亮的 surface（见 `curve()`）。
#[derive(Debug, Clone, Copy)]
struct Curve {
    /// 6 个多项式系数，按升幂排列（`coefficients[0]` 是常数项）。
    coefficients: [f64; 6],
    /// 允许的 surface OKLab 亮度范围 `[min, max]`。
    reachable: [f64; 2],
}

impl Curve {
    /// TS: `levelTarget()` 的多项式部分（`Σ c[i] * surfaceL ** i`，i = 0..5）。
    ///
    /// 逐项累加并同步维护 `power`，保持 TS 的求值顺序；不要改成 Horner 法或 `powi`，
    /// 那会改变浮点舍入路径，可能让黄金文件的十六进制输出出现差异。
    fn evaluate(self, surface_l: f64) -> f64 {
        let mut sum = 0.0;
        let mut power = 1.0;
        for coefficient in self.coefficients {
            sum += coefficient * power;
            power *= surface_l;
        }
        sum
    }

    /// 该曲线在 `surface_l` 上的目标亮度；`None` 表示 `surface_l` 落在 `reachable` 外，
    /// 即当前背景/面板亮度下这个 level 无法定义。
    fn target(self, surface_l: f64) -> Option<f64> {
        if surface_l < self.reachable[0] || surface_l > self.reachable[1] {
            return None;
        }
        Some(self.evaluate(surface_l))
    }
}

/// TS: `type Level = keyof typeof LEVELS`。
///
/// 每个 level 用深色/浅色两条曲线描述“目标亮度离 surface 多远”。`RULES` 把 token 映射
/// 到 level；同一 token 有多条规则时，求解阶段会取所有目标的极值。
///
/// 命名含义：`Panel`/`Track`/`Thumb` 是背景与滚动条表面，`Thinking0..6` 是 thinking
/// 边框的七个等级，`Subtle`/`Readable`/`Emphasis`/`TextOnPanel`/`Text` 是命名上由弱到
/// 强的文字可读性目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Level {
    Panel,
    Track,
    Thinking0,
    Thinking1,
    Thinking2,
    Thinking3,
    Thinking4,
    Thinking5,
    Thinking6,
    Subtle,
    Thumb,
    Readable,
    Emphasis,
    TextOnPanel,
    Text,
}

/// TS: `LEVELS[level][appearance]`。
///
/// `appearance == Dark` 表示深色终端：token 要比 surface 更亮（`generate` 里的 `lighter`
/// 即为真），因此深色曲线只定义在偏暗的 surface 上；浅色终端相反。30 条曲线的系数逐字
/// 来自 TS，不能重算、截断或调换顺序。
fn curve(level: Level, appearance: TerminalColorScheme) -> Curve {
    use Level::*;
    let dark = appearance == TerminalColorScheme::Dark;
    match (level, dark) {
        (Panel, true) => Curve {
            coefficients: [0.29131, -0.39746, 2.33185, -0.85524, -1.2076, 0.86276],
            reachable: [0.0, 0.979],
        },
        (Panel, false) => Curve {
            coefficients: [-3.74073, 27.94549, -78.44258, 112.6798, -79.60015, 22.11277],
            reachable: [0.348, 1.0],
        },
        (Track, true) => Curve {
            coefficients: [0.39028, -0.23015, 0.83573, 2.43829, -4.38292, 2.01582],
            reachable: [0.0, 0.946],
        },
        (Track, false) => Curve {
            coefficients: [
                -5.24921, 38.37322, -107.28833, 152.10005, -106.17127, 29.18061,
            ],
            reachable: [0.368, 1.0],
        },
        (Thinking0, true) => Curve {
            coefficients: [0.52988, -0.05809, -0.30924, 4.63567, -6.52933, 2.89108],
            reachable: [0.0, 0.873],
        },
        (Thinking0, false) => Curve {
            coefficients: [
                -28.27749, 182.85284, -469.62416, 603.15916, -384.59976, 97.35147,
            ],
            reachable: [0.51, 1.0],
        },
        (Thinking1, true) => Curve {
            coefficients: [0.55278, -0.03667, -0.45659, 4.95347, -6.90265, 3.0706],
            reachable: [0.0, 0.858],
        },
        (Thinking1, false) => Curve {
            coefficients: [
                -37.10484, 235.86282, -596.62344, 754.3633, -474.00763, 118.3551,
            ],
            reachable: [0.535, 1.0],
        },
        (Thinking2, true) => Curve {
            coefficients: [0.57486, -0.01765, -0.58987, 5.25227, -7.27175, 3.25532],
            reachable: [0.0, 0.842],
        },
        (Thinking2, false) => Curve {
            coefficients: [
                -59.89653, 377.05024, -945.07843, 1182.03145, -734.96375, 181.68658,
            ],
            reachable: [0.556, 1.0],
        },
        (Thinking3, true) => Curve {
            coefficients: [0.59621, -0.00062, -0.71148, 5.53588, -7.6392, 3.44606],
            reachable: [0.0, 0.827],
        },
        (Thinking3, false) => Curve {
            coefficients: [
                -72.07122,
                445.84082,
                -1099.57352,
                1353.88793,
                -829.53392,
                202.26164,
            ],
            reachable: [0.58, 1.0],
        },
        (Thinking4, true) => Curve {
            coefficients: [0.61691, 0.01462, -0.82288, 5.80651, -8.00641, 3.64333],
            reachable: [0.0, 0.811],
        },
        (Thinking4, false) => Curve {
            coefficients: [
                -110.14338,
                674.21488,
                -1645.75941,
                2004.32367,
                -1215.15899,
                293.3183,
            ],
            reachable: [0.6, 1.0],
        },
        (Thinking5, true) => Curve {
            coefficients: [0.63702, 0.02826, -0.92498, 6.06465, -8.37246, 3.84651],
            reachable: [0.0, 0.795],
        },
        (Thinking5, false) => Curve {
            coefficients: [
                -175.47701,
                1063.54495,
                -2570.70594,
                3098.80776,
                -1860.15527,
                444.76392,
            ],
            reachable: [0.62, 1.0],
        },
        (Thinking6, true) => Curve {
            coefficients: [0.65658, 0.04044, -1.01835, 6.30989, -8.73529, 4.05439],
            reachable: [0.0, 0.779],
        },
        (Thinking6, false) => Curve {
            coefficients: [
                -183.81712,
                1094.70055,
                -2602.68539,
                3088.71276,
                -1826.91131,
                430.75931,
            ],
            reachable: [0.643, 1.0],
        },
        (Subtle, true) => Curve {
            coefficients: [0.56762, -0.02475, -0.5383, 5.12628, -7.10931, 3.17324],
            reachable: [0.0, 0.848],
        },
        (Subtle, false) => Curve {
            coefficients: [
                -232.85459,
                1376.54473,
                -3249.11801,
                3827.91186,
                -2248.29472,
                526.55751,
            ],
            reachable: [0.657, 1.0],
        },
        (Thumb, true) => Curve {
            coefficients: [0.60323, 0.00278, -0.73328, 5.57157, -7.68067, 3.46933],
            reachable: [0.0, 0.823],
        },
        (Thumb, false) => Curve {
            coefficients: [
                -82.89897,
                511.01355,
                -1255.98095,
                1540.76821,
                -940.68087,
                228.58523,
            ],
            reachable: [0.586, 1.0],
        },
        (Readable, true) => Curve {
            coefficients: [0.66937, 0.04704, -1.06871, 6.43941, -8.9332, 4.17229],
            reachable: [0.0, 0.77],
        },
        (Readable, false) => Curve {
            coefficients: [
                -1554.52576,
                8733.56817,
                -19604.93507,
                21977.72696,
                -12300.99599,
                2749.81288,
            ],
            reachable: [0.751, 1.0],
        },
        (Emphasis, true) => Curve {
            coefficients: [0.7303, 0.07695, -1.31626, 7.1681, -10.14436, 4.92846],
            reachable: [0.0, 0.712],
        },
        (Emphasis, false) => Curve {
            coefficients: [
                -4948.31942,
                26870.91986,
                -58334.48399,
                63280.17197,
                -34298.01053,
                7430.30146,
            ],
            reachable: [0.811, 1.0],
        },
        (TextOnPanel, true) => Curve {
            coefficients: [0.86713, 0.05232, -0.89428, 4.79014, -5.5432, 1.75023],
            reachable: [0.0, 0.542],
        },
        (TextOnPanel, false) => Curve {
            coefficients: [
                -8570.89457,
                43954.60805,
                -90084.00702,
                92220.6791,
                -47152.15802,
                9632.27113,
            ],
            reachable: [0.867, 1.0],
        },
        (Text, true) => Curve {
            coefficients: [0.89242, 0.02311, -0.44862, 2.34417, -0.06084, -2.63844],
            reachable: [0.0, 0.5],
        },
        (Text, false) => Curve {
            coefficients: [
                -2004.67048,
                6664.47299,
                -6060.70202,
                -1792.61209,
                5133.82359,
                -1939.85583,
            ],
            reachable: [0.894, 1.0],
        },
    }
}

/// TS: `levelTarget(level, appearance, surfaceL)`。
fn level_target(level: Level, appearance: TerminalColorScheme, surface_l: f64) -> Option<f64> {
    curve(level, appearance).target(surface_l)
}

/// TS: `type Surface = ThemeBg | "background" | "scrollbarTrack"`。
///
/// `Background` 是终端背景（输入里的 `background`，不是 token）；`Token` 则是已经或
/// 将要被求解出来的主题 token（`scrollbarTrack` 也走这一支）。`key()` 把它们统一成
/// `solve()` 颜色表的键：`None` 表示背景。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SurfaceRef {
    Background,
    Token(ThemeToken),
}

impl SurfaceRef {
    /// `solve()` 里的 Map 键：`None` 表示背景。
    fn key(self) -> Option<ThemeToken> {
        match self {
            SurfaceRef::Background => None,
            SurfaceRef::Token(token) => Some(token),
        }
    }
}

/// 一条对比度规则。
///
/// `token` 必须绘制在 `on` 列出的每个 surface 上，并达到 `level`。同一 token 可以有多条
/// 规则（例如 `text` 在背景上用 `Text`、在 `selectedBg` 上用 `TextOnPanel`）；求解时
/// 逐条计算目标再合并，因此每条规则都是硬约束。
#[derive(Debug, Clone)]
struct Rule {
    /// 受约束的 token。
    token: ThemeToken,
    /// 该 token 可能出现的全部 surface。
    on: Vec<SurfaceRef>,
    /// 在每个 surface 上要求达到的对比度等级。
    level: Level,
}

/// 三个工具面板背景 token：`ToolTitle` 必须在这三种面板上都可读。
const TOOL_PANELS: &[SurfaceRef] = &[
    SurfaceRef::Token(ThemeToken::ToolPendingBg),
    SurfaceRef::Token(ThemeToken::ToolSuccessBg),
    SurfaceRef::Token(ThemeToken::ToolErrorBg),
];
/// TS: `PANELS`。
///
/// 全部 7 个背景 token：求解时它们走 `limit_panel` 的 4.5:1 限制；无背景的索引模式下
/// 一律输出空串（终端默认背景）。
const PANELS: [ThemeToken; 7] = [
    ThemeToken::UserMessageBg,
    ThemeToken::ToolPendingBg,
    ThemeToken::ToolSuccessBg,
    ThemeToken::ToolErrorBg,
    ThemeToken::SelectedBg,
    ThemeToken::SearchMatchBg,
    ThemeToken::CustomMessageBg,
];

/// TS: `THINKING`。
///
/// thinking 边框的 7 个前景 token，按等级从低到高排列；与 `THINKING_LEVELS` 同下标配对。
const THINKING: [ThemeColor; 7] = [
    ThemeColor::ThinkingOff,
    ThemeColor::ThinkingMinimal,
    ThemeColor::ThinkingLow,
    ThemeColor::ThinkingMedium,
    ThemeColor::ThinkingHigh,
    ThemeColor::ThinkingXhigh,
    ThemeColor::ThinkingMax,
];

/// TS: `THINKING_LEVELS`。
///
/// 与 `THINKING` 一一对应的 level 数组：下标 i 是 `THINKING[i]` 的目标等级。
const THINKING_LEVELS: [Level; 7] = [
    Level::Thinking0,
    Level::Thinking1,
    Level::Thinking2,
    Level::Thinking3,
    Level::Thinking4,
    Level::Thinking5,
    Level::Thinking6,
];

/// TS: `READABLE_FLOOR`。
///
/// 放松求解时的最低可读等级：深色终端用 `Readable`，浅色用 `Subtle`。`generate` 的
/// `target` 闭包以它对应的距离作为压缩下限。
fn readable_floor(appearance: TerminalColorScheme) -> Level {
    match appearance {
        TerminalColorScheme::Dark => Level::Readable,
        TerminalColorScheme::Light => Level::Subtle,
    }
}

/// TS: `FOREGROUND_LEVEL`。
///
/// 三个正文 token 的目标等级：用于判断终端自带前景色是否够强，以及重上色时的目标亮度。
const FOREGROUND_LEVEL: Level = Level::Emphasis;

/// TS: `FOREGROUND_TOKENS`。
///
/// 允许直接复用终端前景色的三个正文 token（输出空串即“用终端默认色”）；索引模式下它们
/// 不会被加入 `dim`。
const FOREGROUND_TOKENS: [ThemeColor; 3] = [
    ThemeColor::Text,
    ThemeColor::UserMessageText,
    ThemeColor::ToolTitle,
];

/// TS: `TEXT_MINIMUM_WCAG_CONTRAST`。
///
/// 普通文本的 WCAG 2 AA 门槛 4.5:1；`limit_panel`、`with_text_contrast` 与
/// `terminal_appearance` 都用它。
const TEXT_MINIMUM_WCAG_CONTRAST: f64 = 4.5;

fn background_surface() -> SurfaceRef {
    SurfaceRef::Background
}

/// 为一组 token 生成共享同一 `on`/`level` 的规则；返回顺序与 `tokens` 一致，会决定
/// `build_rules` 结果中这些规则的先后。
fn each(tokens: &[ThemeToken], on: &[SurfaceRef], level: Level) -> Vec<Rule> {
    tokens
        .iter()
        .map(|token| Rule {
            token: *token,
            on: on.to_vec(),
            level,
        })
        .collect()
}

/// TS: `RULES`（顺序敏感：`SOLVE_ORDER` 由它推导）。
///
/// 按依赖关系书写：面板背景排在最前，然后是正文/标题、强调与状态色、markdown、diff、
/// 语法高亮、边框与滚动条，最后是 thinking 等级。`solve_order()` 按规则顺序做 DFS
/// 后序，从而保证 surface 先于画在它上面的 token 求解。同一 token 的目标最终取极值，
/// 因此这里的组间顺序不改变颜色语义，但会改变访问顺序；测试
/// `solve_order_is_a_depth_first_post_order` 锁定了关键先后关系。
fn build_rules() -> Vec<Rule> {
    let mut rules = Vec::new();
    let panels: Vec<ThemeToken> = PANELS.to_vec();
    // 7 个面板背景只依赖终端背景，必须最先解出。
    rules.extend(each(&panels, &[background_surface()], Level::Panel));

    let background = [background_surface()];
    let selected = [SurfaceRef::Token(ThemeToken::SelectedBg)];
    let user_message = [SurfaceRef::Token(ThemeToken::UserMessageBg)];
    let search_match_bg = [SurfaceRef::Token(ThemeToken::SearchMatchBg)];
    let scrollbar_track = [SurfaceRef::Token(ThemeToken::ScrollbarTrack)];

    // 正文与 tool 标题：text 在背景/选中项上、userMessageText 在用户消息面板上、
    // toolTitle 在三种工具面板上。
    rules.push(Rule {
        token: ThemeToken::Text,
        on: background.to_vec(),
        level: Level::Text,
    });
    rules.push(Rule {
        token: ThemeToken::Text,
        on: selected.to_vec(),
        level: Level::TextOnPanel,
    });
    rules.push(Rule {
        token: ThemeToken::UserMessageText,
        on: user_message.to_vec(),
        level: Level::TextOnPanel,
    });
    rules.push(Rule {
        token: ThemeToken::ToolTitle,
        on: TOOL_PANELS.to_vec(),
        level: Level::TextOnPanel,
    });

    let readable_on_background_panels = [
        background_surface(),
        SurfaceRef::Token(ThemeToken::SelectedBg),
        SurfaceRef::Token(ThemeToken::ToolPendingBg),
        SurfaceRef::Token(ThemeToken::ToolSuccessBg),
        SurfaceRef::Token(ThemeToken::ToolErrorBg),
    ];
    // 强调色与状态色：在背景、选中项和三种工具面板上都保持 readable。
    rules.extend(each(
        &[
            ThemeToken::Accent,
            ThemeToken::Success,
            ThemeToken::Error,
            ThemeToken::Warning,
        ],
        &readable_on_background_panels,
        Level::Readable,
    ));

    let muted_surfaces = [
        background_surface(),
        SurfaceRef::Token(ThemeToken::SelectedBg),
        SurfaceRef::Token(ThemeToken::CustomMessageBg),
        SurfaceRef::Token(ThemeToken::ToolPendingBg),
        SurfaceRef::Token(ThemeToken::ToolSuccessBg),
        SurfaceRef::Token(ThemeToken::ToolErrorBg),
    ];
    // muted 比 dim 更清晰（两者用不同的 level）；thinkingText 只出现在背景上。
    rules.push(Rule {
        token: ThemeToken::Muted,
        on: muted_surfaces.to_vec(),
        level: Level::Readable,
    });
    rules.push(Rule {
        token: ThemeToken::Dim,
        on: muted_surfaces.to_vec(),
        level: Level::Subtle,
    });
    rules.push(Rule {
        token: ThemeToken::ThinkingText,
        on: background.to_vec(),
        level: Level::Readable,
    });

    let custom_message_text_surfaces = [
        SurfaceRef::Token(ThemeToken::CustomMessageBg),
        SurfaceRef::Token(ThemeToken::ToolPendingBg),
        SurfaceRef::Token(ThemeToken::ToolSuccessBg),
        SurfaceRef::Token(ThemeToken::ToolErrorBg),
    ];
    // 自定义消息的正文与标签：画在自定义消息面板（以及工具面板）上。
    rules.push(Rule {
        token: ThemeToken::CustomMessageText,
        on: custom_message_text_surfaces.to_vec(),
        level: Level::Readable,
    });

    let custom_message_label_surfaces = [
        background_surface(),
        SurfaceRef::Token(ThemeToken::CustomMessageBg),
        SurfaceRef::Token(ThemeToken::SelectedBg),
        SurfaceRef::Token(ThemeToken::ToolPendingBg),
        SurfaceRef::Token(ThemeToken::ToolSuccessBg),
        SurfaceRef::Token(ThemeToken::ToolErrorBg),
    ];
    rules.push(Rule {
        token: ThemeToken::CustomMessageLabel,
        on: custom_message_label_surfaces.to_vec(),
        level: Level::Readable,
    });

    let tool_output_surfaces = [
        background_surface(),
        SurfaceRef::Token(ThemeToken::ToolPendingBg),
        SurfaceRef::Token(ThemeToken::ToolSuccessBg),
        SurfaceRef::Token(ThemeToken::ToolErrorBg),
    ];
    // 工具输出：出现在背景与三种工具面板上。
    rules.push(Rule {
        token: ThemeToken::ToolOutput,
        on: tool_output_surfaces.to_vec(),
        level: Level::Readable,
    });

    let message_surfaces = [
        background_surface(),
        SurfaceRef::Token(ThemeToken::UserMessageBg),
        SurfaceRef::Token(ThemeToken::CustomMessageBg),
    ];
    // Markdown 的标题/链接/行内代码/引用等：出现在背景、用户消息与自定义消息面板上。
    rules.extend(each(
        &[
            ThemeToken::MdHeading,
            ThemeToken::MdLink,
            ThemeToken::MdLinkUrl,
            ThemeToken::MdCode,
            ThemeToken::MdQuote,
            ThemeToken::MdCodeBlockBorder,
            ThemeToken::MdListBullet,
        ],
        &message_surfaces,
        Level::Readable,
    ));

    let code_block_surfaces = [
        background_surface(),
        SurfaceRef::Token(ThemeToken::UserMessageBg),
        SurfaceRef::Token(ThemeToken::CustomMessageBg),
        SurfaceRef::Token(ThemeToken::ToolPendingBg),
        SurfaceRef::Token(ThemeToken::ToolSuccessBg),
        SurfaceRef::Token(ThemeToken::ToolErrorBg),
    ];
    // 代码块内容需要覆盖所有可能作为容器的 surface（含工具面板）。
    rules.push(Rule {
        token: ThemeToken::MdCodeBlock,
        on: code_block_surfaces.to_vec(),
        level: Level::Readable,
    });

    // diff 的三类行：背景与三种工具面板。
    rules.extend(each(
        &[
            ThemeToken::ToolDiffAdded,
            ThemeToken::ToolDiffRemoved,
            ThemeToken::ToolDiffContext,
        ],
        &[
            background_surface(),
            SurfaceRef::Token(ThemeToken::ToolPendingBg),
            SurfaceRef::Token(ThemeToken::ToolSuccessBg),
            SurfaceRef::Token(ThemeToken::ToolErrorBg),
        ],
        Level::Readable,
    ));

    // 语法高亮 token：代码可能出现在任何消息/工具容器里，统一要求在全部容器上 readable。
    rules.extend(each(
        &[
            ThemeToken::SyntaxComment,
            ThemeToken::SyntaxKeyword,
            ThemeToken::SyntaxFunction,
            ThemeToken::SyntaxVariable,
            ThemeToken::SyntaxString,
            ThemeToken::SyntaxNumber,
            ThemeToken::SyntaxType,
            ThemeToken::SyntaxOperator,
            ThemeToken::SyntaxPunctuation,
        ],
        &[
            background_surface(),
            SurfaceRef::Token(ThemeToken::UserMessageBg),
            SurfaceRef::Token(ThemeToken::CustomMessageBg),
            SurfaceRef::Token(ThemeToken::ToolPendingBg),
            SurfaceRef::Token(ThemeToken::ToolSuccessBg),
            SurfaceRef::Token(ThemeToken::ToolErrorBg),
        ],
        Level::Readable,
    ));

    // 搜索命中文字只画在命中背景上。
    rules.push(Rule {
        token: ThemeToken::SearchMatchText,
        on: search_match_bg.to_vec(),
        level: Level::Readable,
    });
    // 边框与 bash 模式边框只画在背景上；borderMuted 更弱（Subtle）。
    rules.extend(each(
        &[
            ThemeToken::BashMode,
            ThemeToken::Border,
            ThemeToken::BorderAccent,
        ],
        &background,
        Level::Readable,
    ));
    rules.push(Rule {
        token: ThemeToken::BorderMuted,
        on: background.to_vec(),
        level: Level::Subtle,
    });
    // 引用边框与分隔线：出现在背景和各类消息/工具容器上。
    rules.extend(each(
        &[ThemeToken::MdQuoteBorder, ThemeToken::MdHr],
        &[
            background_surface(),
            SurfaceRef::Token(ThemeToken::UserMessageBg),
            SurfaceRef::Token(ThemeToken::CustomMessageBg),
            SurfaceRef::Token(ThemeToken::ToolPendingBg),
            SurfaceRef::Token(ThemeToken::ToolSuccessBg),
            SurfaceRef::Token(ThemeToken::ToolErrorBg),
        ],
        Level::Readable,
    ));
    // 滚动条：track 画在背景上，thumb 又画在 track 上——这是唯一“surface 是另一个
    // 前景 token”的依赖，也是 solve_order 必须后序的原因。
    rules.push(Rule {
        token: ThemeToken::ScrollbarTrack,
        on: background.to_vec(),
        level: Level::Track,
    });
    rules.push(Rule {
        token: ThemeToken::ScrollbarThumb,
        on: scrollbar_track.to_vec(),
        level: Level::Thumb,
    });
    // thinking 七个等级都只画在背景上，按下标与 THINKING_LEVELS 配对。
    for (index, token) in THINKING.iter().enumerate() {
        rules.push(Rule {
            token: token.token(),
            on: background.to_vec(),
            level: THINKING_LEVELS[index],
        });
    }

    rules
}

/// TS: `RULES`。
///
/// `build_rules()` 会分配多个 `Vec`，只在首次访问时执行一次；之后所有求解共享同一份
/// 只读规则表（`LazyLock` 提供一次性初始化）。
fn rules() -> &'static [Rule] {
    static RULES: LazyLock<Vec<Rule>> = LazyLock::new(build_rules);
    &RULES
}

/// TS: `SOLVE_ORDER` —— DFS 后序：每个 surface 先于画在它上面的 token。
///
/// 从每条规则出发递归访问它的 surface，回来后才把当前 token 追加到 `order`；于是
/// `solve()` 顺序填色时，任何 token 用到的 surface 颜色都已经是最终值。背景不是 token，
/// 不会进入 `order`；重复访问由 `order.contains` 去重。
///
/// 规则图当前无环（面板只依赖背景，`scrollbarThumb` 依赖 `scrollbarTrack`，其余 token
/// 只依赖面板/背景）。实现没有“访问中”标记，若未来把规则改成互相依赖，这里会无限递归
/// 而不是报错。
fn solve_order() -> &'static [ThemeToken] {
    static ORDER: LazyLock<Vec<ThemeToken>> = LazyLock::new(|| {
        fn visit(token: ThemeToken, order: &mut Vec<ThemeToken>) {
            if order.contains(&token) {
                return;
            }
            for rule in rules() {
                if rule.token != token {
                    continue;
                }
                for surface in &rule.on {
                    if let SurfaceRef::Token(surface_token) = surface {
                        visit(*surface_token, order);
                    }
                }
            }
            order.push(token);
        }
        let mut order = Vec::new();
        for rule in rules() {
            visit(rule.token, &mut order);
        }
        order
    });
    &ORDER
}

/// 是否是背景面板 token（`PANELS`）。决定求解时走 `limit_panel` 而不是普通上色，以及
/// 无背景的索引模式下一律使用终端默认背景。
fn is_panel(token: ThemeToken) -> bool {
    PANELS.contains(&token)
}

// =============================================================================
// Public API
// =============================================================================

/// TS: `SystemThemeInput`。
///
/// 全部字段可选，缺省表示“终端没有告诉我们”。`background` 决定走完整求解还是索引退化
/// 路径；`palette` 必须正好 16 项；`saturation` 在终端颜色尚未到达时用 0，让首帧先按
/// 灰阶渲染。
#[derive(Debug, Clone, Default)]
pub struct SystemThemeInput {
    /// 终端上报的前景色。
    ///
    /// 用于判断终端明暗，以及在三个正文 token 上直接复用（输出空串表示用终端默认色）；
    /// 不参与其余 token 的上色（那些用调色板或色族色相）。
    pub foreground: Option<RgbColor>,
    /// 终端上报的背景色。
    ///
    /// `None` 时无法做亮度计算，`generate_system_theme_colors` 退化成 ANSI 索引主题。
    pub background: Option<RgbColor>,
    /// ANSI 0-15 的调色板（只有正好 16 个时才使用）。
    ///
    /// 顺序即槽位下标 0-15；长度不是 16 时整份被忽略（不截断、不补齐），生成时只用
    /// 色族色相。
    pub palette: Option<Vec<RgbColor>>,
    /// 饱和度倍数 0（灰阶）..1；首帧在颜色到达前沿用灰阶。
    ///
    /// `None` 视为 1.0，越界值按 JS 的 clamp 夹取。`NaN` 且给了 `background` 时会
    /// panic，见 [`generate_system_theme_colors`] 的 Panics 小节。
    pub saturation: Option<f64>,
    /// 终端没有上报背景时使用的外观（来自明暗报告或 COLORFGBG）。
    ///
    /// 只在无背景的退化路径生效；有背景时外观由 [`terminal_appearance`] 从颜色计算。
    pub appearance_hint: Option<TerminalColorScheme>,
}

/// TS: `SystemThemeColors`。
///
/// `colors` 恰好 56 项，顺序与 `TOKEN_FAMILIES` 一致；`dim` 只由无背景的退化路径填充。
#[derive(Debug, Clone)]
pub struct SystemThemeColors {
    /// Hex 颜色、ANSI 调色板索引，或 `""`（终端默认）。
    ///
    /// 十六进制文本是小写 `#rrggbb`；索引只出现在无背景的退化路径。空串由
    /// `crate::theme` 的 `add_token()` 翻译成终端默认前景/背景的 SGR 序列。
    pub colors: IndexMap<ThemeToken, ThemeColorValue>,
    /// 渲染为 SGR 2 的前景 token（终端未上报颜色时使用）。
    ///
    /// 只在无背景的退化路径非空：这些 token 用终端默认前景色，并被 `Theme` 记录为
    /// dim token（渲染时套 SGR 2）。完整求解路径返回空 `Vec`，颜色本身已是具体值。
    pub dim: Vec<ThemeColor>,
    /// 生成的主题面向的外观。
    ///
    /// 有背景时是 `terminal_appearance` 的计算结果；无背景时等于输入的
    /// `appearance_hint`（可能为 `None`）。
    pub appearance: Option<TerminalColorScheme>,
}

/// OKLab 亮度（0-1）。
fn oklab_lightness(color: RgbColor) -> f64 {
    kiana_tui::color_to_oklch(rgb_color(
        f64::from(color.r),
        f64::from(color.g),
        f64::from(color.b),
    ))
    .l
}

/// TS: `relativeLuminance({r, g, b})` —— WCAG 2 相对亮度。
///
/// 输入 8 位 sRGB 通道：先按 WCAG 阈值 0.04045 做 sRGB 线性化，再按
/// 0.2126/0.7152/0.0722 加权。输出 0（黑）到 1（白），是 [`wcag_contrast`] 的中间量。
pub fn relative_luminance(color: RgbColor) -> f64 {
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

/// TS: `wcagContrast(first, second)` —— WCAG 2 对比度，1-21。
///
/// 公式是 `(较亮亮度 + 0.05) / (较暗亮度 + 0.05)`，与参数顺序无关；同色为 1，黑白为 21。
/// 主题里的 4.5 下限（`TEXT_MINIMUM_WCAG_CONTRAST`）即普通文本的 WCAG AA 门槛。
pub fn wcag_contrast(first: RgbColor, second: RgbColor) -> f64 {
    let a = relative_luminance(first);
    let b = relative_luminance(second);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// TS: `terminalAppearance(background, foreground)`。
///
/// 终端是深色还是浅色：优先跟随它自己的前景色方向（当文字能保持可读时），否则比较白字/黑字
/// 在背景上的对比度。
///
/// 判定分三步：
///
/// 1. 有前景色且两者 OKLab 亮度差 > 0.05 时，按“前景比背景亮 = 深色终端”给出候选；
/// 2. 用该候选下的极端文字色（深色用白、浅色用黑）与背景算对比度，达到 4.5:1 才采用；
/// 3. 否则退回白/黑对比度的大小比较，相等时选深色。
///
/// 中灰背景就是第 3 步的典型例子：白字达不到 4.5:1，于是判为浅色（见
/// `tests/system_theme.rs`）。
pub fn terminal_appearance(
    background: RgbColor,
    foreground: Option<RgbColor>,
) -> TerminalColorScheme {
    let white = RgbColor {
        r: 255,
        g: 255,
        b: 255,
    };
    let black = RgbColor { r: 0, g: 0, b: 0 };
    let white_contrast = wcag_contrast(white, background);
    let black_contrast = wcag_contrast(black, background);
    if let Some(foreground) = foreground {
        let foreground_l = oklab_lightness(foreground);
        let background_l = oklab_lightness(background);
        if (foreground_l - background_l).abs() > 0.05 {
            let appearance = if foreground_l > background_l {
                TerminalColorScheme::Dark
            } else {
                TerminalColorScheme::Light
            };
            let best = match appearance {
                TerminalColorScheme::Dark => white_contrast,
                TerminalColorScheme::Light => black_contrast,
            };
            if best >= TEXT_MINIMUM_WCAG_CONTRAST {
                return appearance;
            }
        }
    }
    if white_contrast >= black_contrast {
        TerminalColorScheme::Dark
    } else {
        TerminalColorScheme::Light
    }
}

/// JS `Math.min(max, Math.max(min, value))`。
///
/// JS 的 `Math.min`/`Math.max` 会传播 `NaN`（任一侧是 `NaN` 结果就是 `NaN`），而 Rust 的
/// `f64::min`/`f64::max` 会忽略 `NaN` 返回另一侧；不能用它们实现这个 clamp，否则
/// `clamp(NaN, 0, 1)` 会悄悄变成 `0`。`±Infinity` 与 JS 一样夹到边界。
///
/// `generate_system_theme_colors` 用它夹 `saturation`，因此 `NaN` 会原样传到后面的
/// 有限性检查（而不是被悄悄修正）。
fn clamp(value: f64, min: f64, max: f64) -> f64 {
    js_min(max, js_max(min, value))
}

/// JS `Math.max`：任一侧是 `NaN` 即得 `NaN`。
fn js_max(first: f64, second: f64) -> f64 {
    if first.is_nan() || second.is_nan() {
        f64::NAN
    } else if first > second {
        first
    } else {
        second
    }
}

/// JS `Math.min`：任一侧是 `NaN` 即得 `NaN`。
fn js_min(first: f64, second: f64) -> f64 {
    if first.is_nan() || second.is_nan() {
        f64::NAN
    } else if first < second {
        first
    } else {
        second
    }
}

/// 生成主题里 `ThemeColorValue::Str` 使用的颜色文本：小写 `#rrggbb`。
fn hex_of(color: RgbColor) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}

/// 亮度处的高斯权重（中心 0.5，σ 0.25），在黑白处归零、中亮度为 1。
///
/// 归一化时减去了两端的值，使 `lightness == 0.0/1.0` 恰好得到 0（而不是高斯尾部的
/// 0.135…）。`saturation_curve` 用它插值，`paint` 的无调色板分支也直接用它。
fn bell_weight(lightness: f64) -> f64 {
    let gaussian = |x: f64| (-((x - 0.5) * (x - 0.5)) / (2.0 * 0.25 * 0.25)).exp();
    (gaussian(lightness) - gaussian(0.0)) / (1.0 - gaussian(0.0))
}

/// 色族饱和度曲线（相对最大值）：中亮度为 1，黑白处为 `min / max`。
///
/// 返回的是“相对最大值”的因子（0-1），不是绝对饱和度：调用方乘上 `saturation_max`，
/// 或与源色的锚点比值比较。`saturation_max == 0` 是防御分支（当前表没有这种色族）。
fn saturation_curve(family: Family, lightness: f64) -> f64 {
    let floor = if family.saturation_max > 0.0 {
        family.saturation_min / family.saturation_max
    } else {
        1.0
    };
    floor + (1.0 - floor) * bell_weight(lightness)
}

/// `RgbColor` → OKHSL 通道（`h` 度，`s`/`l` 0-1；`s` 相对该色相/明度的色域最大值）。
fn okhsl_of(color: RgbColor) -> kiana_tui::OkhslChannels {
    color_to_okhsl(rgb_color(
        f64::from(color.r),
        f64::from(color.g),
        f64::from(color.b),
    ))
}

/// 一个终端颜色的 OKHSL 通道与 OKLCH chroma。
///
/// OKHSL 的 `s` 是相对色域最大值的比例，适合做曲线缩放；OKLCH 的 chroma 是绝对值，
/// 用来给缩放后的结果设上限（避免淡色被推成高饱和）。
#[derive(Debug, Clone, Copy)]
struct SourceColor {
    /// OKHSL 色相（度）。
    h: f64,
    /// OKHSL 饱和度（0-1）。
    s: f64,
    /// OKHSL 亮度（0-1）。
    l: f64,
    /// 源色自身的 OKLCH chroma（绝对色度）。
    chroma: f64,
}

/// 把调色板项一次性转换成后续要用的两种表示；调色板最多 16 项，转换只在生成开始时做一次。
fn source_of(color: RgbColor) -> SourceColor {
    let okhsl = okhsl_of(color);
    let oklch = color_to_oklch(rgb_color(
        f64::from(color.r),
        f64::from(color.g),
        f64::from(color.b),
    ));
    SourceColor {
        h: okhsl.h,
        s: okhsl.s,
        l: okhsl.l,
        chroma: oklch.c,
    }
}

/// 源色在另一个 OKHSL 亮度上的颜色：饱和度按色族曲线回落，chroma 也以源色为上限。
///
/// 步骤（`lightness` 是目标 OKHSL 亮度，`saturation` 是全局饱和度倍数）：
///
/// 1. `anchor` 是色族曲线在**源色亮度**处的值，表示源色本身相对色族最大值有多饱和；
/// 2. `falloff = 目标处曲线值 / anchor` 并封顶 1，使目标亮度上的饱和度不超过源色
///    （源色亮度处曲线为 0 时无法比较，直接取 1，不做衰减）；
/// 3. 保持源色色相，饱和度取 `源色s × falloff × saturation`，生成目标亮度上的颜色；
/// 4. 若结果的 OKLCH chroma 超过 `源色chroma × falloff × saturation`，就用同一亮度与
///    色相、把 chroma 压回上限。这一步保证淡色系的颜色在别的亮度上不会变得更艳
///    （见 `keeps_pastel_palette_colors_pastel_at_other_lightnesses`）。
fn anchored(source: SourceColor, family: Family, lightness: f64, saturation: f64) -> RgbColor {
    let anchor = saturation_curve(family, source.l);
    let falloff = if anchor > 0.0 {
        (saturation_curve(family, lightness) / anchor).min(1.0)
    } else {
        1.0
    };
    let color = okhsl_color(source.h, source.s * falloff * saturation, lightness);
    let cap = source.chroma * falloff * saturation;
    let oklch = color_to_oklch(color);
    if oklch.c <= cap {
        color_to_rgb(color)
    } else {
        color_to_rgb(oklch_color(oklch.l, cap, source.h))
    }
}

/// 把文字颜色推向白/黑，直到在每个 surface 上都达到 WCAG 最低对比度。
///
/// 三种出口：
///
/// - 原色已满足所有 surface → 原样返回；
/// - 同色相/饱和度的极端色（深色推白、浅色推黑）满足 → 在“当前亮度 → 极端亮度”之间
///   二分 20 次，返回刚好满足阈值的一侧（区间约缩到 1e-6）；
/// - 连极端色都不满足 → 直接返回极端色（不再尝试其他色相）。
///
/// `lighter` 必须与生成该颜色的终端外观一致（`appearance == Dark` 时为真），否则二分
/// 方向相反。二分假定区间内对比度随亮度单调改善；代码没有额外校验单调性，与 TS 相同。
fn with_text_contrast(color: RgbColor, surfaces: &[RgbColor], lighter: bool) -> RgbColor {
    let meets = |candidate: RgbColor| {
        surfaces
            .iter()
            .all(|surface| wcag_contrast(candidate, *surface) >= TEXT_MINIMUM_WCAG_CONTRAST)
    };
    if meets(color) {
        return color;
    }
    let okhsl = okhsl_of(color);
    let at = |lightness: f64| okhsl_color(okhsl.h, okhsl.s, lightness);
    let extreme = if lighter { 1.0 } else { 0.0 };
    let extreme_color = color_to_rgb(at(extreme));
    if !meets(extreme_color) {
        return extreme_color;
    }
    let (mut low, mut high) = (okhsl.l, extreme);
    for _ in 0..20 {
        let middle = (low + high) / 2.0;
        if meets(color_to_rgb(at(middle))) {
            high = middle;
        } else {
            low = middle;
        }
    }
    color_to_rgb(at(high))
}

/// TS: `generateSystemThemeColors(input)` —— 从终端颜色生成 56 个 token。
///
/// # 输入与输出
///
/// 借用 [`SystemThemeInput`]（不改动调用方数据），返回新的 [`SystemThemeColors`]。
/// 纯函数：不读环境、不写全局状态、不做 IO，相同输入总得到相同输出。
///
/// # 执行流程
///
/// 1. `saturation` 缺省 1.0，按 JS 语义夹到 `[0,1]`；
/// 2. 没有背景色 → 走 `indexed_colors()`：只输出 ANSI 索引与终端默认色，直接返回；
/// 3. 有背景色 → 校验 clamp 后的 `saturation` 不是 NaN（见 Panics），把调色板预转换成
///    `SourceColor`（长度不是 16 就整份忽略）；
/// 4. 用 [`terminal_appearance`] 判定深色/浅色，得到“远离背景”的方向与极端文字色；
/// 5. 按 `solve_order()` 逐 token 求解：规则 → 目标亮度 → 合并极值 → 上色，面板额外做
///    4.5:1 限制；首次求解失败时二分放松参数 t；
/// 6. 按 `TOKEN_FAMILIES` 顺序输出十六进制文本，三个正文 token 可改用终端默认色（空串）；
/// 7. 正文最后统一走 `with_text_contrast` 兜底 4.5:1。
///
/// # 失败与退化
///
/// - 没有背景：不做色彩计算，`appearance` 原样返回 `appearance_hint`。
/// - 中灰背景等“放不下所有 level”的情况：`solve(0)` 返回 `None`，随后在 t ∈ `[0,2]` 上
///   二分最小放松量。放松把目标距离压向最低可读 level；t > 1 时继续向 surface 收缩，
///   t = 2 时每条规则的目标都等于其 surface 亮度，求解必然完成。
/// - 某个目标亮度落在 `[0,1]` 之外：整次求解返回 `None`，不钳制，交给放松阶段处理。
/// - 调色板长度不是 16：视为没有调色板，只用色族色相与饱和度曲线。
///
/// # Panics
///
/// `input.saturation` 是 `NaN` 且给了 `background` 时。TS 的 `clamp(NaN, 0, 1)` 仍是
/// `NaN`，随后 `okhslColor()` 的 `requireFinite` 抛 `s must be finite`；本函数返回
/// `SystemThemeColors`、无法返回错误，用同一条消息的 panic 复刻该抛出。
pub fn generate_system_theme_colors(input: &SystemThemeInput) -> SystemThemeColors {
    let saturation = clamp(input.saturation.unwrap_or(1.0), 0.0, 1.0);
    let Some(background) = input.background else {
        // 没有背景：无法计算亮度关系，只能生成 ANSI 索引/默认色主题。
        return indexed_colors(saturation, input.appearance_hint);
    };
    // TS: 有 background 时第一处 `okhslColor(h, s, l)` 会先 `requireFinite`：
    // `clamp(NaN, 0, 1)` 仍是 `NaN` → 抛 `Error: s must be finite`。（没有 background 的
    // `indexedColors()` 只做 `saturation > 0` 的比较，JS 对 NaN 是 false，不抛。）
    // Rust 的 `kiana_tui::okhsl_color` 不做这项校验（NaN 的 s 按灰阶处理），所以这里显式
    // 复刻 TS 的抛出：本函数的公开签名（spec §2.3.1 的 Rust 形态）返回
    // `SystemThemeColors`、不能返回错误，panic 消息逐字等于 TS 的 `error.message`。
    if saturation.is_nan() {
        panic!("s must be finite");
    }
    // 只有正好 16 项才可用：槽位按 ANSI 0-15 下标直接索引；长度不符时整份丢弃。
    let palette: Option<Vec<SourceColor>> = match &input.palette {
        Some(palette) if palette.len() == 16 => {
            Some(palette.iter().copied().map(source_of).collect())
        }
        _ => None,
    };

    // 深色终端（appearance == Dark）要把 token 往亮处推，浅色往暗处推。
    let appearance = terminal_appearance(background, input.foreground);
    let lighter = appearance == TerminalColorScheme::Dark;
    let extreme = if lighter { 1.0 } else { 0.0 };
    let background_l = oklab_lightness(background);

    // paint：把目标 OKLab 亮度落成一个具体颜色。没有调色板时只用色族色相与
    // saturation_curve 给的饱和度；有调色板时用该 token 的 slot 颜色做锚点（`anchored`）。
    let paint = |token: ThemeToken, oklab_l: f64| -> RgbColor {
        let lightness = oklab_to_okhsl_lightness(oklab_l);
        let family = token_family(token).family();
        let Some(palette) = palette.as_deref() else {
            let saturation_value = (family.saturation_min
                + (family.saturation_max - family.saturation_min) * bell_weight(lightness))
                * saturation;
            return color_to_rgb(okhsl_color(family.hue, saturation_value, lightness));
        };
        let slot = token_slot(token).unwrap_or(family.slot);
        let source = palette[usize::from(slot)];
        anchored(source, family, lightness, saturation)
    };

    // target：规则 level + surface 亮度 → 目标 OKLab 亮度。曲线在当前 surface 上不可达
    // 时以极端色（深色为白、浅色为黑）作为方向；t 把“离 surface 的距离”压向最低可读
    // level 对应的距离，t > 1 时再整体向 surface 收缩。只有 t == 0 且不可达才返回 None。
    let target = |level: Level, surface_l: f64, t: f64| -> Option<f64> {
        let reached = level_target(level, appearance, surface_l);
        if reached.is_none() && t == 0.0 {
            return None;
        }
        let distance = reached.unwrap_or(extreme) - surface_l;
        let floor = level_target(readable_floor(appearance), appearance, surface_l)
            .unwrap_or(extreme)
            - surface_l;
        let compressed = if distance.abs() > floor.abs() {
            distance - (distance - floor) * t.min(1.0)
        } else {
            distance
        };
        Some(surface_l + compressed * (1.0 - (t - 1.0).max(0.0)))
    };

    // 极端文字色：深色终端用白、浅色用黑；`readable` 判断某个面板/背景色能否承载它。
    let extreme_text = if lighter {
        RgbColor {
            r: 255,
            g: 255,
            b: 255,
        }
    } else {
        RgbColor { r: 0, g: 0, b: 0 }
    };
    let readable =
        |color: RgbColor| wcag_contrast(extreme_text, color) >= TEXT_MINIMUM_WCAG_CONTRAST;
    // 面板上色：目标色若能让白（深色）/黑（浅色）文字达到 4.5:1 就直接用；否则在
    // “背景亮度 → 目标亮度”之间二分，取最贴近背景、仍然可读的亮度。
    let limit_panel = |token: ThemeToken, l: f64| -> RgbColor {
        let color = paint(token, l);
        if readable(color) {
            return color;
        }
        let (mut low, mut high) = (background_l, l);
        for _ in 0..20 {
            let middle = (low + high) / 2.0;
            if readable(paint(token, middle)) {
                low = middle;
            } else {
                high = middle;
            }
        }
        paint(token, low)
    };

    // solve：按 solve_order（DFS 后序）给每个 token 选亮度。对同一 token 的所有规则与
    // surface 收集目标，深色取最亮、浅色取最暗（都等于“离背景最远”）；t = 0 时任一条
    // 不可达、或任一条目标越界，都让整次求解失败（返回 None），由调用方决定是否放松。
    let solve = |t: f64| -> Option<HashMap<Option<ThemeToken>, RgbColor>> {
        let mut colors: HashMap<Option<ThemeToken>, RgbColor> = HashMap::new();
        colors.insert(None, background);
        for token in solve_order().iter().copied() {
            let mut targets: Vec<f64> = Vec::new();
            for rule in rules().iter().filter(|rule| rule.token == token) {
                for surface in &rule.on {
                    let surface_color = *colors.get(&surface.key()).unwrap_or(&background);
                    let surface_l = oklab_lightness(surface_color);
                    let value = target(rule.level, surface_l, t)?;
                    if !(0.0..=1.0).contains(&value) {
                        return None;
                    }
                    targets.push(value);
                }
            }
            let l = if lighter {
                targets.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            } else {
                targets.iter().copied().fold(f64::INFINITY, f64::min)
            };
            // 面板是画在别的 token 底下的 surface：选好亮度后还要保证白/黑文字在它
            // 上面可读，必要时用 limit_panel 往背景方向回退。
            let color = if is_panel(token) {
                limit_panel(token, l)
            } else {
                paint(token, l)
            };
            colors.insert(Some(token), color);
        }
        Some(colors)
    };

    let mut relaxation = 0.0;
    let mut colors = solve(0.0);
    if colors.is_none() {
        // 中灰背景放不下所有 level：二分 t ∈ [0,2] 找最小放松量。t = 2 时目标恒等于
        // surface 亮度，求解必然完成；取最小的成功值以尽量保留对比层次。
        let (mut low, mut high) = (0.0, 2.0);
        colors = solve(high);
        for _ in 0..20 {
            let middle = (low + high) / 2.0;
            let attempt = solve(middle);
            if let Some(attempt) = attempt {
                high = middle;
                colors = Some(attempt);
            } else {
                low = middle;
            }
        }
        relaxation = high;
    }
    // 理论上放松后一定能得到结果；若仍失败则得到空表，所有 token 退化成终端默认色。
    let solved = colors.unwrap_or_default();
    // 收集每个 token 实际用到的全部 surface 颜色：正文兜底对比度时要在这些 surface 上
    // 都达标（而不是只看背景）。
    let surfaces_of = |token: ThemeToken| -> Vec<RgbColor> {
        rules()
            .iter()
            .filter(|rule| rule.token == token)
            .flat_map(|rule| {
                rule.on
                    .iter()
                    .map(|surface| *solved.get(&surface.key()).unwrap_or(&background))
            })
            .collect()
    };

    // 输出顺序 = TOKEN_FAMILIES 顺序；缺席的 token 用空串（终端默认色）表示。
    let mut result: IndexMap<ThemeToken, ThemeColorValue> = IndexMap::new();
    for (token, _) in TOKEN_FAMILIES {
        let color = solved.get(&Some(*token));
        result.insert(
            *token,
            match color {
                Some(color) => ThemeColorValue::Str(hex_of(*color)),
                None => ThemeColorValue::Str(String::new()),
            },
        );
    }

    // 三个正文 token 特殊处理：终端自己的前景色若已经足够显眼，就直接用默认色；
    // 否则用它的色相在需要的亮度上重新上色。
    for token_color in FOREGROUND_TOKENS {
        let token = token_color.token();
        let surfaces = surfaces_of(token);
        // 正文优先用终端自己的前景色（当它的亮度已达到 Emphasis 目标时），否则用前景色的
        // 色相调到刚好够亮。
        let mut text = solved.get(&Some(token)).copied();
        if let Some(foreground) = input.foreground {
            let targets: Vec<Option<f64>> = surfaces
                .iter()
                .map(|surface| target(FOREGROUND_LEVEL, oklab_lightness(*surface), relaxation))
                .collect();
            if targets
                .iter()
                .all(|value| matches!(value, Some(value) if (0.0..=1.0).contains(value)))
            {
                let values: Vec<f64> = targets.iter().filter_map(|value| *value).collect();
                let needed = if lighter {
                    values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                } else {
                    values.iter().copied().fold(f64::INFINITY, f64::min)
                };
                let foreground_l = oklab_lightness(foreground);
                // 终端前景已经达到（或超过）所需亮度：输出空串，使用终端自身的前景色。
                if if lighter {
                    foreground_l >= needed
                } else {
                    foreground_l <= needed
                } {
                    result.insert(token, ThemeColorValue::Str(String::new()));
                    continue;
                }
                // 否则保留前景色的色相/饱和度，在 needed 对应的 OKHSL 亮度上重新上色。
                text = Some(anchored(
                    source_of(foreground),
                    FamilyName::Neutral.family(),
                    oklab_to_okhsl_lightness(needed),
                    saturation,
                ));
            }
        }
        // 无论来自求解还是前景色，正文都要在所有出现的 surface 上兜底 4.5:1
        // （中灰背景放松后尤其重要）。
        if let Some(text) = text {
            result.insert(
                token,
                ThemeColorValue::Str(hex_of(with_text_contrast(text, &surfaces, lighter))),
            );
        }
    }
    SystemThemeColors {
        colors: result,
        dim: Vec::new(),
        appearance: Some(appearance),
    }
}

/// 终端什么都没上报时的颜色：ANSI 索引 0-15 + 终端默认色。
///
/// 没有背景就无法做亮度计算，只能信终端：面板与中性 token 输出空串（终端默认前景/
/// 背景），非中性 token 在 `saturation > 0` 时输出其 slot 的索引（`TOKEN_SLOTS` 覆盖，
/// 缺省为色族 slot），否则同样回退默认色；具体颜色由终端决定。`dim` 收集需要用 SGR 2 变暗的中性前景 token，三个正文 token
/// （`FOREGROUND_TOKENS`）除外。
///
/// `appearance` 原样透传 `appearance_hint`（这一路径无法从颜色推断外观）。
fn indexed_colors(saturation: f64, appearance: Option<TerminalColorScheme>) -> SystemThemeColors {
    let mut colors: IndexMap<ThemeToken, ThemeColorValue> = IndexMap::new();
    let mut dim: Vec<ThemeColor> = Vec::new();
    for (token, family_name) in TOKEN_FAMILIES {
        if is_panel(*token) {
            // 面板：用终端默认背景。
            colors.insert(*token, ThemeColorValue::Str(String::new()));
            continue;
        }
        let neutral = *family_name == FamilyName::Neutral;
        // 中性 token 用终端默认前景；只有非中性才引用调色板索引，且 saturation 为 0
        // 或 NaN（`NaN > 0.0` 为 false）时同样回退默认色。
        let value = if !neutral && saturation > 0.0 {
            ThemeColorValue::Index(token_slot(*token).unwrap_or(family_name.family().slot))
        } else {
            ThemeColorValue::Str(String::new())
        };
        colors.insert(*token, value);
        // dim 只对可映射为 ThemeColor 的中性 token 生效，并排除三个正文 token。
        if neutral
            && !FOREGROUND_TOKENS
                .iter()
                .any(|color| color.token() == *token)
            && let Some(color) = ThemeColor::from_token(*token)
        {
            dim.push(color);
        }
    }
    SystemThemeColors {
        colors,
        dim,
        appearance,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(hex: &str) -> RgbColor {
        color_to_rgb(kiana_tui::parse_color(hex).expect("valid hex"))
    }

    #[test]
    fn wcag_contrast_matches_the_reference_values() {
        assert!((wcag_contrast(rgb("#000000"), rgb("#ffffff")) - 21.0).abs() < 1e-9);
        assert!((wcag_contrast(rgb("#ffffff"), rgb("#ffffff")) - 1.0).abs() < 1e-9);
        // #777777 与白色约 4.48:1（TS 参照实现给出 4.4783…）。
        let contrast = wcag_contrast(rgb("#777777"), rgb("#ffffff"));
        assert!((contrast - 4.478_089).abs() < 1e-3, "{contrast}");
    }

    #[test]
    fn terminal_appearance_prefers_the_reported_foreground_direction() {
        let background = RgbColor {
            r: 118,
            g: 118,
            b: 118,
        };
        assert_eq!(
            terminal_appearance(background, None),
            TerminalColorScheme::Light
        );
        assert_eq!(
            terminal_appearance(
                background,
                Some(RgbColor {
                    r: 255,
                    g: 255,
                    b: 255
                })
            ),
            TerminalColorScheme::Dark
        );
        // 中灰上白字达不到 4.5:1，退回对比度比较。
        let mid_gray = RgbColor {
            r: 128,
            g: 128,
            b: 128,
        };
        assert_eq!(
            terminal_appearance(
                mid_gray,
                Some(RgbColor {
                    r: 255,
                    g: 255,
                    b: 255
                })
            ),
            TerminalColorScheme::Light
        );
    }

    #[test]
    fn solve_order_is_a_depth_first_post_order() {
        let order = solve_order();
        assert_eq!(order.len(), 56);
        // 每个 surface 都在它自己的规则之前被 push；`scrollbarTrack` 在 `scrollbarThumb` 前。
        let track = order.iter().position(|t| *t == ThemeToken::ScrollbarTrack);
        let thumb = order.iter().position(|t| *t == ThemeToken::ScrollbarThumb);
        assert!(track < thumb);
        let background_panels: Vec<usize> = PANELS
            .iter()
            .map(|token| order.iter().position(|t| t == token).expect("panel solved"))
            .collect();
        let text = order
            .iter()
            .position(|t| *t == ThemeToken::Text)
            .expect("text");
        assert!(background_panels.iter().all(|index| *index < text));
        // 所有 56 个 token 都出现在 SOLVE_ORDER 中。
        for token in ThemeToken::ALL {
            assert!(order.contains(&token), "{token} missing from SOLVE_ORDER");
        }
    }

    #[test]
    fn indexed_colors_cover_every_token() {
        let generated = generate_system_theme_colors(&SystemThemeInput {
            appearance_hint: Some(TerminalColorScheme::Light),
            ..Default::default()
        });
        assert_eq!(generated.colors.len(), 56);
        assert_eq!(generated.appearance, Some(TerminalColorScheme::Light));
        assert_eq!(
            generated.colors.get(&ThemeToken::Error),
            Some(&ThemeColorValue::Index(1))
        );
        assert_eq!(
            generated.colors.get(&ThemeToken::UserMessageBg),
            Some(&ThemeColorValue::Str(String::new()))
        );
        assert!(generated.dim.contains(&ThemeColor::Muted));
        assert!(!generated.dim.contains(&ThemeColor::Text));
        // 零饱和度：非中性 token 也退回终端默认色。
        let grayscale = generate_system_theme_colors(&SystemThemeInput {
            saturation: Some(0.0),
            ..Default::default()
        });
        assert_eq!(
            grayscale.colors.get(&ThemeToken::Error),
            Some(&ThemeColorValue::Str(String::new()))
        );
        // 调色板长度不是 16 时按“没有调色板”处理。
        let short_palette = generate_system_theme_colors(&SystemThemeInput {
            background: Some(rgb("#1e1e1e")),
            palette: Some(vec![rgb("#000000")]),
            ..Default::default()
        });
        assert!(short_palette.colors.contains_key(&ThemeToken::Accent));
    }
}
