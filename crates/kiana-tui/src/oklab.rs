//! oklab.ts：Oklab 与 OKHSL <-> sRGB 的数值转换。
//!
//! `colors.ts` 的 OKLCH、OKHSL 与混色都建立在本模块之上。
//!
//! Oklab 与 OKHSL 是 Björn Ottosson 的色彩空间；OKHSL 的饱和度相对于该色相与明度下
//! sRGB 色域能给出的最大值。本文件是他参考实现
//! （<https://bottosson.github.io/posts/colorpicker/>，Copyright (c) 2021 Björn Ottosson,
//! MIT）的移植：
//!
//! > Permission is hereby granted, free of charge, to any person obtaining a copy of this software
//! > and associated documentation files (the "Software"), to deal in the Software without
//! > restriction, including without limitation the rights to use, copy, modify, merge, publish,
//! > distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the
//! > Software is furnished to do so, subject to the following conditions: The above copyright
//! > notice and this permission notice shall be included in all copies or substantial portions of
//! > the Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! > IMPLIED.
//!
//! TS: `oklab.ts#oklabToOkhslLightness` / `#oklabToLinearSrgb` / `#rgbToOklab` /
//! `#linearSrgbToRgb` / `#okhslToRgb` / `#rgbToOkhsl`。
//!
//! ## 与 TS 的两处表示差异
//!
//! - TS 的 `linearSrgbToRgb` 返回 `{r,g,b}`（0-255 的 number，可能是 `NaN`）。Rust 侧返回
//!   `[f64; 3]`，调用方再决定落成 `Color::Rgb`（可保留 `NaN`）还是 `RgbColor`（u8）。
//! - 所有 `Math.min` / `Math.max` 用的是 JS 语义：任一侧是 `NaN` 就得到 `NaN`。
//!   Rust 的 `f64::min` / `f64::max` 会忽略 `NaN`，不能直接用。
//!
//! ## 执行流程（数值链路）
//!
//! - 编码 sRGB → Oklab：`rgb_channels_to_oklab` 把 0-255（可带小数）通道除以 255，
//!   经 `srgb_to_linear` 解传递函数，乘 `LINEAR_SRGB_TO_LMS` 得 LMS，逐分量 `js_cbrt`，
//!   再乘 `LMS_TO_LAB`。
//! - Oklab → 编码 sRGB：`oklab_to_linear_srgb` 乘 `LAB_TO_LMS`、逐分量立方（`js_cube`）、
//!   再乘 `LMS_TO_LINEAR_SRGB`；`linear_srgb_to_rgb` 负责编码传递函数、夹到 `[0,1]`、
//!   乘 255 并 `js_round`。
//! - OKHSL：`rgb_channels_to_okhsl` 先转 Oklab，再用 `chroma_stops`（色域 cusp、逐通道
//!   Halley 迭代——用一、二阶导加速收敛的求根法——与多项式拟合）把色度映射成 0-1 的
//!   饱和度；`okhsl_to_rgb` 做反向映射。
//!
//! ## 数值一致性约束
//!
//! - 目标是**与 V8/Node 逐位一致**：`tests/oklab_parity.rs` 用 bit pattern 黄金数据断言
//!   `oklab_to_linear_srgb`；`tests/regression_review_fixes.rs` 另有 OKHSL 网格期望值。
//!   改动常量、运算顺序，或把记录过位级差异的函数（`js_cbrt`、`js_cube`、`js_hypot` 等）
//!   换成更直观的 `f64` 写法（如 `f64::cbrt`、`f64::hypot`、直接连乘），都会破坏这些断言。
//! - `js_min` / `js_max` 复刻 JS 的 NaN 传播（任一侧 NaN 即 NaN），`js_round` 复刻
//!   “半值向 +∞”；`js_cbrt` / `js_cube` 等按各函数文档记录的 V8/JS 语义实现（不是随手
//!   可替换的等价物）。普通中间运算仍用标准 `f64`。
//! - 矩阵与多项式系数按参考实现逐位保留（个别常量带 `#[allow(clippy::excessive_precision)]`），
//!   不要按十进制“化简”成看起来更短的值。
//! - 本模块无状态、无 I/O；输出可能越界（线性 sRGB 可小于 0 或大于 1），是否裁剪由调用方决定。

use crate::terminal_colors::{OkhslChannels, RgbColor};

/// JS `Math.hypot(a, b)`。
///
/// V8 的实现是"除以最大值后求平方和"：`m = max(|a|,|b|)`，`m * sqrt((a/m)^2 + (b/m)^2)`。
/// **不能**换成 `f64::hypot`（算法不同，165 万个样本里约 44% 差 1 ulp），也不能用
/// `sqrt(a*a+b*b)`（同一样本集里约 24% 差 1 ulp）。这个逐位一致的版本在 165k 个
/// `rgbToOklab` 实际输入上 0 差异。
pub(crate) fn js_hypot(a: f64, b: f64) -> f64 {
    let max = if a.abs() > b.abs() { a.abs() } else { b.abs() };
    if max == 0.0 || !max.is_finite() {
        // `Math.hypot(0, 0) === 0`；出现 Inf 时 V8 返回 Inf，NaN 传播为 NaN。
        if max.is_nan() {
            return f64::NAN;
        }
        if max == 0.0 {
            return 0.0;
        }
        return f64::INFINITY;
    }
    let first = a / max;
    let second = b / max;
    max * (first * first + second * second).sqrt()
}

/// TS `Vector`。
///
/// 长度为 3 的 `f64` 数组，具体含义由使用它的函数决定（Oklab 是 `[L, a, b]`，sRGB 是
/// `[r, g, b]`）；单位可能是 0-1（线性/编码通道）或 0-255（`linear_srgb_to_rgb` 的输出）。
pub type Vector = [f64; 3];
/// TS `Matrix`（行主序）。
type Matrix = [Vector; 3];

// Ottosson 参考实现的常量矩阵（行主序，逐位照抄）。LMS 是变换链中间的三分量空间
// （参考实现里的三锥响应量）：`LINEAR_SRGB_TO_LMS` 与 `LMS_TO_LAB` 组成正向链路，
// `LAB_TO_LMS` 与 `LMS_TO_LINEAR_SRGB` 组成反向链路。系数决定色域边界与 OKHSL 的 cusp，
// 改动任何一位都会让逐位对照测试失败。
const LINEAR_SRGB_TO_LMS: Matrix = [
    [0.4122214694707629, 0.5363325372617349, 0.0514459932675022],
    [0.2119034958178251, 0.6806995506452344, 0.1073969535369405],
    [0.0883024591900564, 0.2817188391361215, 0.6299787016738222],
];
const LMS_TO_LAB: Matrix = [
    [0.210454268309314, 0.793617774702305, -0.0040720430116193],
    [1.9779985324311684, -2.42859224204858, 0.450593709617411],
    [0.0259040424655478, 0.7827717124575296, -0.8086757549230774],
];
const LAB_TO_LMS: Matrix = [
    [1.0, 0.3963377773761749, 0.2158037573099136],
    [1.0, -0.1055613458156586, -0.0638541728258133],
    [1.0, -0.0894841775298119, -1.2914855480194092],
];
#[allow(clippy::excessive_precision)] // 参考实现的原始字面量，逐位保留
const LMS_TO_LINEAR_SRGB: Matrix = [
    [4.0767416360759583, -3.3077115392580629, 0.2309699031821043],
    [-1.2684379732850315, 2.6097573492876882, -0.341319376002657],
    [-0.0041960761386756, -0.7034186179359362, 1.7076146940746117],
];

/// 每个 sRGB 通道（红、绿、蓝）：该通道最先裁剪的 (a, b) 半平面，以及拟合最大饱和度的多项式。
///
/// 每项是 `(半平面法向量, 多项式系数 [k0..k4])`：`max_saturation` 先按 `x*a + y*b > 1`
/// 找出最先到达边界的通道（索引 2 是无条件兜底，对应 TS `findIndex` 的 `index === 2 || …`），
/// 用该通道的多项式给出初始饱和度估计，再做一步 Halley 迭代。
const SATURATION_FIT: [([f64; 2], [f64; 5]); 3] = [
    (
        [-1.8817031, -0.80936501],
        [1.19086277, 1.76576728, 0.59662641, 0.75515197, 0.56771245],
    ),
    (
        [1.8144408, -1.19445267],
        [0.73956515, -0.45954404, 0.08285427, 0.12541073, -0.14503204],
    ),
    (
        [0.13110758, 1.81333971],
        [1.35733652, -0.00915799, -1.1513021, -0.50559606, 0.00692167],
    ),
];
// OKHSL 明度曲线的拟合常数；K3 由 K1、K2 派生，只在这两个明度互转公式里使用。
const K1: f64 = 0.206;
const K2: f64 = 0.03;
const K3: f64 = (1.0 + K1) / (1.0 + K2);

/// JS `Math.round`：`floor(x + 0.5)`，并保留 `NaN` / `±Infinity` / `±0`。
pub(crate) fn js_round(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    (x + 0.5).floor()
}

/// JS `Math.min`：任一侧是 `NaN` 即得 `NaN`。
pub(crate) fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else {
        b
    }
}

/// JS `Math.max`：任一侧是 `NaN` 即得 `NaN`。
pub(crate) fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

/// JS `Math.cbrt(x)`（V8 用的是 fdlibm/FreeBSD msun 的 `s_cbrt.c`）。
///
/// 不能换成 `f64::cbrt`：两者在 30,000 个随机样本里差 2,649 次（约 1 ulp），而
/// `Math.cbrt` 的这几位误差会沿着 `rgbToOklab` → `chromaStops` → `okhslToRgb`
/// 一路传到 `chroma < 1e-7` 的判定上（无彩色色相因此发散）。本实现在 60,000 个
/// 样本（含次正规数、负数、±0、Inf、NaN）上与 V8 逐位一致。
#[allow(clippy::excessive_precision)] // FreeBSD msun 的原始字面量，逐位保留
pub(crate) fn js_cbrt(x: f64) -> f64 {
    const B1: u32 = 715_094_163;
    const B2: u32 = 696_219_795;
    const P0: f64 = 1.875_951_824_271_770_096_43;
    const P1: f64 = -1.884_979_795_433_771_698_75;
    const P2: f64 = 1.621_429_720_105_354_466_140;
    const P3: f64 = -0.758_397_934_778_766_047_437;
    const P4: f64 = 0.145_996_192_886_612_446_982;

    let bits = x.to_bits();
    let low = bits as u32;
    let mut high = (bits >> 32) as u32;
    let sign = high & 0x8000_0000;
    high ^= sign;
    // cbrt(±Inf) = ±Inf；NaN 沿 x + x 传播。
    if high >= 0x7ff0_0000 {
        return x + x;
    }
    let mut t: f64;
    if high < 0x0010_0000 {
        // 次正规数（或 ±0）：乘 2^54 规格化后再修正结果指数。
        if (high | low) == 0 {
            return x;
        }
        t = f64::from_bits(0x4350_0000_0000_0000);
        t *= x;
        let high_t = (t.to_bits() >> 32) as u32;
        // 高 32 位（指数与部分尾数）除以 3 并加偏置 B2（次正规数分支）。
        t = f64::from_bits(u64::from(sign | ((high_t & 0x7fff_ffff) / 3 + B2)) << 32);
    } else {
        // 规格化数分支：同样的指数估计，偏置为 B1。
        t = f64::from_bits(u64::from(sign | (high / 3 + B1)) << 32);
    }
    // r = t^3 / x（初值接近真值时约为 1），多项式用它对初值做修正。
    let r = (t * t) * (t / x);
    t *= (P0 + r * (P1 + r * P2)) + ((r * r) * r) * (P3 + r * P4);
    // 清掉 t 的低位尾数，得到低精度初值，交给下面的 Newton 迭代。
    t = f64::from_bits((t.to_bits().wrapping_add(0x8000_0000)) & 0xffff_ffff_c000_0000);
    // 一次 Newton 迭代把误差压到 0.5 ulp 以内。
    let s = t * t;
    let r = x / s;
    let w = t + t;
    let r = (r - t) / (w + r);
    t + t * r
}

/// JS `x ** 2`（V8 把整数指数 2 特判成 `x * x`，与 `powf(2.0)` 有 1 ulp 差异）。
pub(crate) fn js_square(x: f64) -> f64 {
    x * x
}

/// JS `x ** 3`（V8 用通用的 `pow`；`x * x * x` 在 30 万样本里差 26%）。
pub(crate) fn js_cube(x: f64) -> f64 {
    x.powf(3.0)
}

/// JS `x ** 4`（同上，不能用连乘）。
pub(crate) fn js_quartic(x: f64) -> f64 {
    x.powf(4.0)
}

/// 行向量与列向量的点积（三个 `f64` 乘加，运算顺序固定）。
fn dot(row: Vector, v: Vector) -> f64 {
    row[0] * v[0] + row[1] * v[1] + row[2] * v[2]
}

/// 行主序矩阵 × 列向量：逐行做 `dot`。
fn multiply(m: &Matrix, v: Vector) -> Vector {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}

/// Oklab 明度 → OKHSL 明度。
///
/// 输入是 Oklab 的 `L`（常规范围 0-1），输出 OKHSL 的 `l`。端点固定为 0→0、1→1，
/// `0.5` 映射到 `0.42114056260896976`（实测值见 `spec/tui-text.md`）。`NaN` 输入得到 `NaN`。
pub fn oklab_to_okhsl_lightness(x: f64) -> f64 {
    let k3x = K3 * x;
    0.5 * (k3x - K1 + ((k3x - K1) * (k3x - K1) + 4.0 * K2 * K3 * x).sqrt())
}

/// OKHSL 明度 → Oklab 明度。
fn okhsl_to_oklab_lightness(x: f64) -> f64 {
    (x * x + K1 * x) / (K3 * (x + K2))
}

/// sRGB 传递函数：线性 → 编码通道，均为 0-1。
fn linear_to_srgb(value: f64) -> f64 {
    if value > 0.0031308 {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    } else {
        12.92 * value
    }
}

/// sRGB 传递函数的逆：编码 → 线性通道，均为 0-1。
fn srgb_to_linear(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// Oklab `[L, a, b]` → 线性 sRGB `[r, g, b]`（0-1，可能越界）。
///
/// 不做裁剪：色域外的颜色会得到小于 0 或大于 1 的通道，由调用方决定 clamp 还是保留。
/// 运算顺序固定为“矩阵 → 逐分量立方 → 矩阵”，`tests/oklab_parity.rs` 用 bit pattern
/// 黄金数据逐位锁定（含随机位模式输入，不能假定输入都是有限值）。
pub fn oklab_to_linear_srgb(lab: Vector) -> Vector {
    let lms = multiply(&LAB_TO_LMS, lab);
    let cubed = [js_cube(lms[0]), js_cube(lms[1]), js_cube(lms[2])];
    multiply(&LMS_TO_LINEAR_SRGB, cubed)
}

/// 线性 sRGB `[r, g, b]`（0-1）→ Oklab `[L, a, b]`。
fn linear_srgb_to_oklab(rgb: Vector) -> Vector {
    let lms = multiply(&LINEAR_SRGB_TO_LMS, rgb);
    let roots = [js_cbrt(lms[0]), js_cbrt(lms[1]), js_cbrt(lms[2])];
    multiply(&LMS_TO_LAB, roots)
}

/// sRGB 通道（0-255）→ Oklab `[L, a, b]`。
///
/// 输入是 `u8` 通道，因此不需要取整或裁剪；实际换算（除以 255、解传递函数）在
/// [`rgb_channels_to_oklab`] 里完成。
pub fn rgb_to_oklab(rgb: RgbColor) -> Vector {
    rgb_channels_to_oklab([f64::from(rgb.r), f64::from(rgb.g), f64::from(rgb.b)])
}

/// sRGB 通道（**可以是分数**）→ Oklab `[L, a, b]`。
///
/// TS 的 `RgbColor` 通道是 number，`mixColors(..., "srgb")` 之后往往带小数；
/// `colorToOklch`/`colorToOkhsl` 直接把它们喂给 `rgbToOklab`，不做取整。
///
/// 每一维依次执行 `value / 255`、`srgb_to_linear`、矩阵变换与 `js_cbrt`；不检查范围，
/// 越界或 `NaN` 输入会按 IEEE 运算传播（不会 panic）。
pub fn rgb_channels_to_oklab(rgb: Vector) -> Vector {
    linear_srgb_to_oklab([
        srgb_to_linear(rgb[0] / 255.0),
        srgb_to_linear(rgb[1] / 255.0),
        srgb_to_linear(rgb[2] / 255.0),
    ])
}

/// 线性 sRGB `[r, g, b]` → sRGB 通道（0-255，取整并裁剪越界通道）。
///
/// `NaN` 会原样传播（JS 的 `Math.round(NaN) === NaN`）。
///
/// 运算顺序固定为：先编码传递函数，再把结果夹到 `[0, 1]`，然后乘 255、`js_round`。返回值
/// 仍是 `f64`（可以带 `NaN`），转成 `u8` 前要由调用方决定如何处理非有限值。
pub fn linear_srgb_to_rgb(linear: Vector) -> Vector {
    let channel = |value: f64| js_round(js_min(1.0, js_max(0.0, linear_to_srgb(value))) * 255.0);
    [channel(linear[0]), channel(linear[1]), channel(linear[2])]
}

/// 每个立方根 LMS 分量沿色度方向 (a, b) 的变化率。
///
/// `LAB_TO_LMS` 每行的第一列都是 1（对应 L 项），这里只取第 2、3 列的线性组合，得到
/// “固定明度、沿 (a,b) 方向移动时”三个 LMS 分量（随后会立方）的斜率，供
/// `max_saturation` / `max_chroma` 的 Halley 迭代构造一、二阶导。
fn lms_slopes(a: f64, b: f64) -> Vector {
    [
        LAB_TO_LMS[0][1] * a + LAB_TO_LMS[0][2] * b,
        LAB_TO_LMS[1][1] * a + LAB_TO_LMS[1][2] * b,
        LAB_TO_LMS[2][1] * a + LAB_TO_LMS[2][2] * b,
    ]
}

/// 色相 (a, b) 下 sRGB 色域内的最大饱和度（C/L）：多项式拟合加一步 Halley 迭代。
///
/// 步骤：
/// 1. 按半平面条件选出最先到达下界 0 的通道（索引 2 是兜底）；上界 1 的归一化由
///    `cusp` 处理；
/// 2. 用该通道的五项多项式估算初始 `saturation`；
/// 3. 在 `L = 1` 处令 `base[i] = 1 + saturation * slopes[i]`，它就是待立方的 LMS 分量；
///    据此构造该通道线性值的 `f` 以及一、二阶导 `f1`、`f2`；
/// 4. 做一步 Halley 迭代 `s - f*f1 / (f1^2 - 0.5*f*f2)`，返回修正后的饱和度。
///
/// 纯计算，无分支保护；`a`、`b` 同时为 0 时 `slopes` 全为 0，`f1`/`f2` 也为 0，Halley
/// 公式退化为 `0/0`，返回 `NaN`。实际调用方保证方向非零：`chroma_stops` 从
/// `okhsl_to_rgb` 收到单位圆上的 `(a, b)`，从 `rgb_channels_to_okhsl` 收到的方向在
/// `chroma >= 1e-9` 时才会计算。
fn max_saturation(a: f64, b: f64) -> f64 {
    // `findIndex(([[x, y]], index) => index === 2 || x * a + y * b > 1)`
    let mut channel = 2usize;
    for (index, (half, _)) in SATURATION_FIT.iter().enumerate() {
        if index == 2 || half[0] * a + half[1] * b > 1.0 {
            channel = index;
            break;
        }
    }

    let [k0, k1, k2, k3, k4] = SATURATION_FIT[channel].1;
    let weights = LMS_TO_LINEAR_SRGB[channel];
    let saturation = k0 + k1 * a + k2 * b + k3 * a * a + k4 * a * b;

    let slopes = lms_slopes(a, b);
    let base = [
        1.0 + saturation * slopes[0],
        1.0 + saturation * slopes[1],
        1.0 + saturation * slopes[2],
    ];
    let dot =
        |values: [f64; 3]| weights[0] * values[0] + weights[1] * values[1] + weights[2] * values[2];
    let f = dot([js_cube(base[0]), js_cube(base[1]), js_cube(base[2])]);
    let f1 = dot([
        3.0 * slopes[0] * js_square(base[0]),
        3.0 * slopes[1] * js_square(base[1]),
        3.0 * slopes[2] * js_square(base[2]),
    ]);
    let f2 = dot([
        6.0 * js_square(slopes[0]) * base[0],
        6.0 * js_square(slopes[1]) * base[1],
        6.0 * js_square(slopes[2]) * base[2],
    ]);
    saturation - (f * f1) / (f1 * f1 - 0.5 * f * f2)
}

/// 色相 (a, b) 下最饱和 sRGB 颜色的 Oklab 明度与色度。
///
/// 取该色相的最大饱和度，在 `L = 1` 处算出线性 sRGB 颜色；把最大通道归一化到 1 所需的
/// 缩放会同时缩放明度，`cbrt(1 / max_channel)` 即 cusp 的明度，乘饱和度得到色度。
/// 返回 `[lightness, chroma]`。
fn cusp(a: f64, b: f64) -> [f64; 2] {
    let saturation = max_saturation(a, b);
    let linear = oklab_to_linear_srgb([1.0, saturation * a, saturation * b]);
    let max = js_max(js_max(linear[0], linear[1]), linear[2]);
    let lightness = js_cbrt(1.0 / max);
    [lightness, lightness * saturation]
}

/// 明度 `lightness` 处恒定明度线离开 sRGB 色域时的色度。
///
/// - `lightness <= cusp_l`（色域三角形的下边）：色度上界与明度成正比，直接线性插值；
/// - 否则（上边）：先按 cusp 到白色 `(1, 0)` 的连线取初值 `t`，再对三个通道分别做一步
///   Halley 迭代求通道校正量 `-f*u`，取 `u >= 0` 的通道中最小的校正量（它本身可能为负，
///   `f > 0` 时会继续减小 `t`）；`u < 0` 的通道用 `f64::MAX` 排除，不参与 `js_min`。
///
/// 返回色度；纯计算，输入越界时没有额外保护。
fn max_chroma(a: f64, b: f64, lightness: f64, cusp: [f64; 2]) -> f64 {
    let [cusp_l, cusp_c] = cusp;
    if lightness <= cusp_l {
        return (cusp_c * lightness) / cusp_l;
    }
    // 上半部：三角形边，再对每个通道到达 1 做一步 Halley 迭代。
    let t = (cusp_c * (lightness - 1.0)) / (cusp_l - 1.0);
    let slopes = lms_slopes(a, b);
    let lms = [
        lightness + t * slopes[0],
        lightness + t * slopes[1],
        lightness + t * slopes[2],
    ];
    let cubes = [js_cube(lms[0]), js_cube(lms[1]), js_cube(lms[2])];
    let first = [
        3.0 * slopes[0] * js_square(lms[0]),
        3.0 * slopes[1] * js_square(lms[1]),
        3.0 * slopes[2] * js_square(lms[2]),
    ];
    let second = [
        6.0 * js_square(slopes[0]) * lms[0],
        6.0 * js_square(slopes[1]) * lms[1],
        6.0 * js_square(slopes[2]) * lms[2],
    ];
    let mut result = f64::MAX;
    for row in LMS_TO_LINEAR_SRGB {
        let f = dot(row, cubes) - 1.0;
        let f1 = dot(row, first);
        let f2 = dot(row, second);
        let u = f1 / (f1 * f1 - 0.5 * f * f2);
        let step = if u >= 0.0 { -f * u } else { f64::MAX };
        result = js_min(result, step);
    }
    t + result
}

/// OKHSL 在明度 L、色相 (a, b) 处的色度参考点：`[c0, cMid, cMax]`。
///
/// - `c_max`：该明度/色相在色域边界上的色度（`max_chroma`）；
/// - `c_mid`：s = 0.8 处的色度，由 `mid_s`/`mid_t` 两项多项式拟合，再用四次幂的并联
///   公式合成（`js_quartic` 复刻 V8 的 `** 4`）；
/// - `c0`：低饱和段（s < 0.8）曲线使用的参数，**不是** s = 0 时的色度（那里色度为 0）。
///
/// `k = c_max / min(...)` 把拟合结果缩放到当前色域。纯计算，无 I/O 与错误路径。
fn chroma_stops(l: f64, a: f64, b: f64) -> [f64; 3] {
    let peak = cusp(a, b);
    let c_max = max_chroma(a, b, l, peak);
    let k = c_max
        / js_min(
            l * (peak[1] / peak[0]),
            (1.0 - l) * (peak[1] / (1.0 - peak[0])),
        );
    let mid_s = 0.11516993
        + 1.0
            / (7.4477897
                + 4.1590124 * b
                + a * (-2.19557347
                    + 1.75198401 * b
                    + a * (-2.13704948 - 10.02301043 * b
                        + a * (-4.24894561 + 5.38770819 * b + 4.69891013 * a))));
    let mid_t = 0.11239642
        + 1.0
            / (1.6132032 - 0.68124379 * b
                + a * (0.40370612
                    + 0.90148123 * b
                    + a * (-0.27087943
                        + 0.6122399 * b
                        + a * (0.00299215 - 0.45399568 * b - 0.14661872 * a))));
    let four = js_quartic;
    let c_mid = 0.9
        * k
        * (1.0 / (1.0 / four(l * mid_s) + 1.0 / four((1.0 - l) * mid_t)))
            .sqrt()
            .sqrt();
    let c0 = (1.0 / (1.0 / js_square(l * 0.4) + 1.0 / js_square((1.0 - l) * 0.8))).sqrt();
    [c0, c_mid, c_max]
}

/// OKHSL 转 sRGB 通道（0-255，取整并裁剪越界通道）。
///
/// - `hue` 角度；`saturation`、`lightness` 0-1。
/// - `lightness` 先经 `okhsl_to_oklab_lightness` 转成 Oklab 的 L。若 L 不在 `(0, 1)` 或
///   `saturation <= 0`，直接按灰阶返回（`a = b = 0`），不做色域拟合。
/// - `saturation < 0.8` 与 `>= 0.8` 使用两段不同的插值：前者以 `c0` 为参数，后者从
///   `c_mid` 升到 `c_max`（s = 1 正好落到色域边界）。
/// - 不做范围校验：`s > 1` 会继续走第二段公式外推；调用方 `colors.rs` 的 `okhsl_color`
///   原样转发参数。
/// - `NaN` 输入会传播为 `NaN` 通道（与 JS 一致），不会 panic。
pub fn okhsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> Vector {
    let l = okhsl_to_oklab_lightness(lightness);
    let mut lab: Vector = [l, 0.0, 0.0];
    // 灰阶与极端明度短路：没有色度可算，L 直接作为 Oklab 的 L。
    if l > 0.0 && l < 1.0 && saturation > 0.0 {
        // 归一化色相：JS 的 `%` 对负数返回负值，所以先加 360 再取模。
        let angle = 2.0 * std::f64::consts::PI * (((hue % 360.0) + 360.0) % 360.0) / 360.0;
        let a = angle.cos();
        let b = angle.sin();
        let [c0, c_mid, c_max] = chroma_stops(l, a, b);
        // 色度从 0 经 cMid（s = 0.8）升到 cMax（s = 1）。
        let chroma = if saturation < 0.8 {
            let t = 1.25 * saturation;
            let k1 = 0.8 * c0;
            (t * k1) / (1.0 - (1.0 - k1 / c_mid) * t)
        } else {
            let t = 5.0 * (saturation - 0.8);
            let k1 = (0.2 * js_square(c_mid) * js_square(1.25)) / c0;
            c_mid + (t * k1) / (1.0 - (1.0 - k1 / (c_max - c_mid)) * t)
        };
        lab = [l, chroma * a, chroma * b];
    }
    // 回到编码 sRGB：Oklab → 线性矩阵链路 → 传递函数/夹取/取整。
    linear_srgb_to_rgb(oklab_to_linear_srgb(lab))
}

/// sRGB 通道（0-255）→ OKHSL。灰度的色相为 0，饱和度与明度 0-1。
///
/// 输入是 `u8`，直接转成 `f64` 后交给 [`rgb_channels_to_okhsl`]；不取整、不裁剪。
pub fn rgb_to_okhsl(rgb: RgbColor) -> OkhslChannels {
    rgb_channels_to_okhsl([f64::from(rgb.r), f64::from(rgb.g), f64::from(rgb.b)])
}

/// sRGB 通道（**可以是分数**）→ OKHSL（`rgbToOkhsl` 的原始数值入口）。
///
/// 流程：先经 [`rgb_channels_to_oklab`] 得到 Oklab `[L, a, b]`；色度用 `js_hypot(a, b)`
/// （复刻 V8 的 `Math.hypot` 算法，不能换成 `f64::hypot`）；明度用
/// [`oklab_to_okhsl_lightness`] 换算。
///
/// - 色度小于 `1e-9`（灰阶）或换算后的明度不在 `(0, 1)` 内（含端点 0 与 1）时，色相视为
///   无定义，返回 `h = 0`、`s = 0`，`l` 保留换算值。
/// - 否则色相用 `libm::atan2` 计算并归一化到 `[0, 360)`；饱和度由 `chroma_stops` 的分段
///   反函数得到，最后夹到 `[0, 1]`。
/// - 不做取整：分数通道会照常参与（`colorToOkhsl` 依赖这一点）。
pub fn rgb_channels_to_okhsl(rgb: Vector) -> OkhslChannels {
    let [l, lab_a, lab_b] = rgb_channels_to_oklab(rgb);
    let chroma = js_hypot(lab_a, lab_b);
    let lightness = oklab_to_okhsl_lightness(l);
    // 近灰阶或明度在端点之外：色相无定义，统一返回 h = 0、s = 0（l 保留换算值）。
    if chroma < 1e-9 || lightness <= 0.0 || lightness >= 1.0 {
        return OkhslChannels {
            h: 0.0,
            s: 0.0,
            l: lightness,
        };
    }

    // 色相：atan2(b, a) 转成角度再加 360 取模；使用 libm 的固定实现。
    let hue = ((libm::atan2(lab_b, lab_a) * 180.0) / std::f64::consts::PI + 360.0) % 360.0;
    let [c0, c_mid, c_max] = chroma_stops(l, lab_a / chroma, lab_b / chroma);
    // 分段反函数：低于 cMid 用 c0 段，高于 cMid 用 cMax 段，与 okhsl_to_rgb 对应。
    let saturation = if chroma < c_mid {
        let k1 = 0.8 * c0;
        0.8 * (chroma / (k1 + (1.0 - k1 / c_mid) * chroma))
    } else {
        let k1 = (0.2 * js_square(c_mid) * js_square(1.25)) / c0;
        let offset = chroma - c_mid;
        0.8 + 0.2 * (offset / (k1 + (1.0 - k1 / (c_max - c_mid)) * offset))
    };
    // 饱和度夹到 [0, 1]；色相已在 [0, 360)，明度不再夹取。
    OkhslChannels {
        h: hue,
        s: js_min(1.0, js_max(0.0, saturation)),
        l: lightness,
    }
}
