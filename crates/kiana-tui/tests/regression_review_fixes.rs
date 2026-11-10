//! 评审回归：TS 与 Rust 在正则/浮点/分段语义上的差异。
//!
//! 每一条都先在 Node 26.12.1 / ICU 78.3 上跑过 `packages/tui/src/**` 得到期望值，
//! 再在这里逐值断言（注释里给出对应的 TS 表达式）。
//!
//! | 节 | 主题 |
//! |---|---|
//! | 1 | `parseColor`：JS 的 `\d` / `i` 是 ASCII-only，非法十六进制不得 panic |
//! | 2 | `parseColor`：`oklch`/`okhsl` 的 JS `\s` 集合与 `requireFinite` |
//! | 3 | `fuzzyMatch`：数字开头的 query 也要做 alphaNumeric 交换 |
//! | 4 | `parseOscColorResponse`：多字节输入不 panic、JS `trim()` 空白集合 |
//! | 5 | `parseOscHexChannel`：任意位宽的通道归一化 |
//! | 6 | `getOsc8LinkAtColumn`：URL 里出现 ESC/BEL 的 OSC 8 序列不算链接 |
//! | 7 | `cjkBreakRegex`：Unicode 17 新增的 CJK 码点 |
//! | 8 | Oklab / OKHSL 数值稳定（逐位断言） |

use kiana_tui::colors::{
    Color, color_to_okhsl, color_to_oklch, color_to_rgb, okhsl_color, parse_color,
};
use kiana_tui::fuzzy::fuzzy_match;
use kiana_tui::terminal_colors::{OscColorTarget, RgbColor, parse_osc_color_response};
use kiana_tui::text::{cjk_break_matches, get_osc8_link_at_column, wrap_text_with_ansi};

// ---------------------------------------------------------------------------
// 1. `parseColor` 的十六进制分支
// ---------------------------------------------------------------------------

/// TS：`/^#([\da-f]{3}|[\da-f]{6})$/i`。JS 的 `\d` 与不带 `u` 的 `i` 都是 **ASCII-only**，
/// `String.prototype` 的 `parseInt` 也不会把 `٠`（U+0660）当数字，因此这些输入全部落到
/// `throw new Error("Invalid color value: ...")`——本仓库的冻结签名对应 `None`。
#[test]
fn non_ascii_hex_digits_are_rejected_without_panicking() {
    for value in [
        "#a\u{0660}b",
        "#\u{0660}\u{0660}\u{0660}",
        "#a\u{FF10}b",
        "#\u{FF10}\u{FF10}\u{FF10}",
    ] {
        assert_eq!(parse_color(value), None, "{value:?} must not parse");
    }
}

/// 正常路径不受影响（`#abc` 展开、大小写混写）。
#[test]
fn ascii_hex_still_parses() {
    assert_eq!(
        parse_color("#abc"),
        Some(Color::Rgb {
            r: 170.0,
            g: 187.0,
            b: 204.0
        })
    );
    assert_eq!(parse_color("#aBc"), parse_color("#abc"));
    assert_eq!(parse_color("#abcd"), None);
}

// ---------------------------------------------------------------------------
// 2. `oklch(...)` / `okhsl(...)`
// ---------------------------------------------------------------------------

/// TS 的 `\s` 含 U+FEFF、不含 U+0085；`\d` 是 ASCII-only。
#[test]
fn oklch_whitespace_and_digits_follow_js_regex_semantics() {
    // BOM 算空白
    assert_eq!(
        parse_color("oklch(\u{FEFF}0.5 0.1 200)"),
        Some(Color::Oklch {
            l: 0.5,
            c: 0.1,
            h: 200.0
        })
    );
    // NEL（U+0085）不是 JS 空白
    assert_eq!(parse_color("oklch(\u{85} 0.5 0.1 200)"), None);
    // 阿拉伯数字不是 JS 的 `\d`
    assert_eq!(parse_color("okhsl(5\u{0660} 0.5 0.5)"), None);
    assert_eq!(parse_color("oklch(0.5 0.1 2\u{0660}\u{0660})"), None);
    // NBSP 是 JS 空白
    assert_eq!(
        parse_color("oklch(\u{A0}0.5 0.1 200)"),
        Some(Color::Oklch {
            l: 0.5,
            c: 0.1,
            h: 200.0
        })
    );
    // 尾部多余字符仍被锚定
    assert_eq!(parse_color("oklch(0.5 0.1 200)X"), None);
}

/// TS 的 `oklchColor`/`okhslColor` 先 `requireFinite(...)`：
/// `oklch(0.5 1e999 200)` 抛 `c must be finite`，本仓库返回 `None`。
#[test]
fn non_finite_oklch_and_okhsl_channels_are_rejected() {
    for value in [
        "oklch(0.5 1e999 200)",
        "oklch(0.5 0.1 1e999)",
        "okhsl(1e999 60% 55%)",
        "okhsl(10 60% 1e999)",
        "oklch(1e999 0.1 200)",
    ] {
        assert_eq!(parse_color(value), None, "{value:?} must be rejected");
    }
}

// ---------------------------------------------------------------------------
// 3. `fuzzyMatch` 的 alphaNumeric 交换
// ---------------------------------------------------------------------------

/// TS 先试 `^[a-z]+[0-9]+$`，失败再试 `^[0-9]+[a-z]+$`；两处都要能交换。
/// 期望值来自 `fuzzyMatch` 的实测输出。
#[test]
fn fuzzy_swaps_digits_then_letters() {
    for (query, text, score) in [
        ("3v", "deepseek-v3", -8.1),
        ("4phi", "phi-4", -42.3),
        ("3o", "o3-mini", -19.9),
        ("2gemma", "gemma-2", -86.4),
        ("31llama", "llama-3.1", -93.600_000_000_000_01),
        ("46glm", "glm-4.6", -49.699_999_999_999_996),
        ("codex52", "codex-5.2", -98.600_000_000_000_01),
        ("v3", "deepseek-v3", -13.1),
        ("3x", "x-3", -17.8),
    ] {
        let result = fuzzy_match(query, text);
        assert!(result.matches, "{query:?} should match {text:?}");
        assert_eq!(result.score, score, "{query:?} vs {text:?}");
    }
}

/// 两边都能整段命中时，交换不该把分数弄坏（TS 里 `+5` 之后仍是最优）。
#[test]
fn fuzzy_exact_matches_are_unaffected_by_swapping() {
    for query in ["ab12", "12ab"] {
        let result = fuzzy_match(query, query);
        assert!(result.matches);
        assert_eq!(result.score, -159.4, "{query:?}");
    }
}

// ---------------------------------------------------------------------------
// 4. `parseOscColorResponse`：多字节值与 JS `trim()`
// ---------------------------------------------------------------------------

/// 值里含多字节字符时 TS 只是解析失败（`rgb: undefined`），Rust 不能 panic。
#[test]
fn osc_color_values_with_multibyte_characters_do_not_panic() {
    for value in ["\u{20ac}\u{e9}", "\u{e9}\u{e9}", "\u{4e2d}\u{6587}"] {
        let data = format!("\x1b]10;{value}\x07");
        assert_eq!(
            parse_osc_color_response(&data),
            Some((OscColorTarget::Foreground, None)),
            "{value:?}"
        );
    }
}

/// TS 的 `trim()` 用 JS `\s`：U+FEFF 去掉、U+0085 保留（保留后不是合法颜色 → `None`）。
#[test]
fn osc_color_values_trim_like_js() {
    assert_eq!(
        parse_osc_color_response("\x1b]10;\u{FEFF}#ffffff\x07"),
        Some((
            OscColorTarget::Foreground,
            Some(RgbColor {
                r: 255,
                g: 255,
                b: 255
            })
        ))
    );
    assert_eq!(
        parse_osc_color_response("\x1b]10;\u{85}#ffffff\x07"),
        Some((OscColorTarget::Foreground, None))
    );
}

// ---------------------------------------------------------------------------
// 5. `parseOscHexChannel`
// ---------------------------------------------------------------------------

/// `value / (16 ** len - 1) * 255`，`len >= 256` 时 `16 ** len` 溢出成 `Infinity`。
/// TS 里 `Math.round(...)` 后仍是 255（被 `Math.min`/`u8` 边界吃掉），绝不会回绕成 16 或 0。
#[test]
fn long_osc_hex_channels_saturate_instead_of_wrapping() {
    for length in [4usize, 16, 20, 33, 40, 255] {
        let channel = "f".repeat(length);
        let data = format!("\x1b]10;rgb:{channel}/{channel}/{channel}\x07");
        assert_eq!(
            parse_osc_color_response(&data),
            Some((
                OscColorTarget::Foreground,
                Some(RgbColor {
                    r: 255,
                    g: 255,
                    b: 255
                })
            )),
            "channel length {length}"
        );
    }
}

/// `parseInt(channel, 16)` 必须按**一次** IEEE 就近舍入（不是逐位乘加）。
/// 期望值来自 Node：`Math.round(Number.parseInt(c, 16) / (16 ** c.length - 1) * 255)`。
#[test]
fn osc_hex_channels_round_once_like_js() {
    let long = |s: String| format!("\x1b]10;rgb:{s}/{s}/{s}\x07");
    let cases: &[(String, Option<u8>)] = &[
        ("f".repeat(255), Some(255)),
        (format!("8{}", "0".repeat(254)), Some(128)),
        (format!("1{}", "0".repeat(254)), Some(16)),
        (format!("7{}", "f".repeat(20)), Some(128)),
        (format!("{}1", "0".repeat(300)), Some(0)),
        ("1234567890abcdef1234567890abcdef".to_string(), Some(18)),
        ("ffffffffffffffff".to_string(), Some(255)),
        ("ffffffffffffff".to_string(), Some(255)),
        ("1000000000000000".to_string(), Some(16)),
        ("8000000000000000".to_string(), Some(128)),
        ("1".to_string(), Some(17)),
        ("0".to_string(), Some(0)),
        ("00".to_string(), Some(0)),
        ("7".to_string(), Some(119)),
        ("80".to_string(), Some(128)),
        ("abc".to_string(), Some(171)),
        ("00ff".to_string(), Some(1)),
    ];
    for (channel, expected) in cases {
        let data = long(channel.clone());
        let parsed = parse_osc_color_response(&data);
        let got = match parsed {
            Some((OscColorTarget::Foreground, Some(RgbColor { r, g, b }))) => {
                assert_eq!((r, g), (b, r), "channels must be equal for {channel:?}");
                Some(r)
            }
            Some((OscColorTarget::Foreground, None)) => None,
            other => panic!("unexpected {other:?} for {channel:?}"),
        };
        assert_eq!(got, *expected, "channel {channel:?}");
    }
}

/// 短通道的归一化（`rgb:1/2/3` → 17/34/51）。
#[test]
fn short_osc_hex_channels_normalize_by_bit_width() {
    assert_eq!(
        parse_osc_color_response("\x1b]10;rgb:1/2/3\x07"),
        Some((
            OscColorTarget::Foreground,
            Some(RgbColor {
                r: 17,
                g: 34,
                b: 51
            })
        ))
    );
    assert_eq!(
        parse_osc_color_response("\x1b]10;rgb:00/80/ff\x07"),
        Some((
            OscColorTarget::Foreground,
            Some(RgbColor {
                r: 0,
                g: 128,
                b: 255
            })
        ))
    );
}

// ---------------------------------------------------------------------------
// 6. `getOsc8LinkAtColumn`
// ---------------------------------------------------------------------------

/// TS 的 `getOsc8LinkAtColumn` 用 `^\x1b]8;(?:[^;]*);([^\x07\x1b]*)(?:\x07|\x1b\\)$`：
/// URL 里不允许出现 BEL/ESC，因此 `\x1b]8;;a\x1bb\x07` 不是 OSC 8 序列，
/// 不能把此前激活的链接清掉（tracker 用的 `indexOf` 版本则认得它——两者不同）。
#[test]
fn malformed_osc8_with_esc_in_url_does_not_clobber_the_active_link() {
    // 序列本身非法 → 没有链接
    assert_eq!(get_osc8_link_at_column("\x1b]8;;a\x1bb\x07X", 0), None);
    // `good` 生效后遇到非法序列 → 仍是 `good`（不是 None）
    assert_eq!(
        get_osc8_link_at_column("\x1b]8;;good\x07A\x1b]8;;a\x1bb\x07B", 1),
        Some("good".to_string())
    );
}

/// 合法序列的三种形态：关闭、带参数、ST 终止符。
#[test]
fn well_formed_osc8_sequences_are_still_recognized() {
    assert_eq!(
        get_osc8_link_at_column("\x1b]8;;good\x07A\x1b]8;;\x07B", 1),
        None
    );
    assert_eq!(
        get_osc8_link_at_column("\x1b]8;id=1;http://x\x07A", 0),
        Some("http://x".to_string())
    );
    assert_eq!(
        get_osc8_link_at_column("\x1b]8;;http://x\x1b\\A", 0),
        Some("http://x".to_string())
    );
}

// ---------------------------------------------------------------------------
// 7. `cjkBreakRegex`
// ---------------------------------------------------------------------------

/// Unicode 17 新增的 CJK 码点（`regex` crate 的 `\p{scx=...}` 表里还没有它们）。
#[test]
fn unicode_17_cjk_code_points_break_like_the_rest() {
    for cp in [0x16FF2u32, 0x2B73A, 0x2CEA2, 0x323B0] {
        let ch = char::from_u32(cp).expect("scalar value");
        assert!(cjk_break_matches(&ch.to_string()), "U+{cp:X}");
    }
}

/// 换行结果：`ABC ` 后跟 10 个 U+323B0 在宽度 10 下按 3/5/2 折行。
#[test]
fn wrap_counts_unicode_17_cjk_code_points_as_full_width() {
    let text = format!("ABC {}", "\u{323B0}".repeat(10));
    assert_eq!(
        wrap_text_with_ansi(&text, 10),
        vec![
            "ABC \u{323B0}\u{323B0}\u{323B0}".to_string(),
            "\u{323B0}".repeat(5),
            "\u{323B0}".repeat(2),
        ]
    );
}

// ---------------------------------------------------------------------------
// 8. Oklab / OKHSL 数值
// ---------------------------------------------------------------------------

fn bits(value: f64) -> u64 {
    value.to_bits()
}

/// `colorToOklch` / `colorToOkhsl` 与 TS **逐位**一致（`Math.cbrt`、`x ** 3`、`Math.hypot`
/// 都有各自的算法，换成 Rust 的 `f64::cbrt` / `x*x*x` / `(a*a+b*b).sqrt()` 会差 1 ulp）。
#[test]
fn oklch_and_okhsl_are_bit_exact() {
    // [hex, oklch(l, c, h), okhsl(h, s, l)]，每个字段是 f64 的十六进制位模式。
    let cases: &[(&str, [u64; 3], [u64; 3])] = &[
        (
            "#123456",
            [
                0x3fd4_6d41_6403_b3d0,
                0x3fb2_8c61_50a7_af5d,
                0x406f_6563_f5ff_93ce,
            ],
            [
                0x406f_6563_f5ff_93ce,
                0x3fe6_20ac_b845_406e,
                0x3fcc_057f_4e7c_fcba,
            ],
        ),
        (
            "#a1b2c3",
            [
                0x3fe8_3075_326b_c81f,
                0x3f9f_b141_b1c8_1e29,
                0x406f_0791_6769_1880,
            ],
            [
                0x406f_0791_6769_1880,
                0x3fc9_2148_71b7_f4fe,
                0x3fe6_eac3_5f82_cb26,
            ],
        ),
        (
            "#c0ffee",
            [
                0x3fee_7f1b_0f24_d517,
                0x3fb1_2e33_52cc_82c5,
                0x4066_0b6c_bb96_aa64,
            ],
            [
                0x4066_0b6c_bb96_aa64,
                0x3fef_ffff_c6a2_2d2f,
                0x3fee_3fb1_982b_c179,
            ],
        ),
        (
            "#deadbe",
            [
                0x3fe9_80ae_a32e_87e0,
                0x3faf_4de0_8645_7583,
                0x4076_3df4_2999_e4ee,
            ],
            [
                0x4076_3df4_2999_e4ee,
                0x3fdb_554c_fd12_c7ff,
                0x3fe8_70f6_07d3_11f6,
            ],
        ),
        (
            "#ff0000",
            [
                0x3fe4_1835_d8ec_7d89,
                0x3fd0_7de2_1ca4_6eb1,
                0x403d_3bdf_93f8_2e50,
            ],
            [
                0x403d_3bdf_93f8_2e50,
                0x3ff0_0000_0000_0000,
                0x3fe2_2dbf_df88_14af,
            ],
        ),
    ];
    for (hex, oklch, okhsl) in cases {
        let color = parse_color(hex).unwrap_or_else(|| panic!("{hex} parses"));
        let o = color_to_oklch(color);
        assert_eq!(
            [bits(o.l), bits(o.c), bits(o.h)],
            *oklch,
            "{hex} colorToOklch"
        );
        let k = color_to_okhsl(color);
        assert_eq!(
            [bits(k.h), bits(k.s), bits(k.l)],
            *okhsl,
            "{hex} colorToOkhsl"
        );
    }
}

/// `okhsl(h, s, l)` 三元组与它的 sRGB 结果。
type OkhslCase = (f64, f64, f64, (f64, f64, f64));

/// `okhslToRgb` 的整条链条（立方根 → `chromaStops` → 8 位裁剪）的输出由下列精确值固定。
/// 期望值是 `okhslToRgb(h, s, l)` 在 9 个色相 × 5 个饱和度 × 5 个明度上的输出。
#[test]
fn okhsl_to_rgb_matches_ts_across_the_grid() {
    let cases: &[OkhslCase] = &[
        (0.0, 0.25, 0.5, (145.0, 107.0, 118.0)),
        (0.0, 0.8, 0.5, (201.0, 51.0, 114.0)),
        (0.0, 1.0, 0.5, (215.0, 0.0, 113.0)),
        (60.0, 0.5, 0.5, (154.0, 107.0, 68.0)),
        (120.0, 0.5, 0.5, (113.0, 126.0, 71.0)),
        (200.0, 0.5, 0.5, (68.0, 131.0, 134.0)),
        (300.0, 0.5, 0.5, (130.0, 102.0, 176.0)),
        (359.0, 1.0, 0.5, (214.0, 0.0, 116.0)),
        (273.0, 0.8, 0.9, (218.0, 226.0, 251.0)),
        (17.0, 1.0, 0.1, (50.0, 0.0, 8.0)),
        (0.0, 0.0, 0.5, (119.0, 119.0, 119.0)),
        (0.0, 0.0, 0.0, (0.0, 0.0, 0.0)),
        (0.0, 0.0, 1.0, (255.0, 255.0, 255.0)),
        (200.0, 1.0, 1.0, (255.0, 255.0, 255.0)),
    ];
    for &(h, s, l, expected) in cases {
        let Color::Rgb { r, g, b } = okhsl_color(h, s, l) else {
            panic!("okhsl_color returns rgb")
        };
        assert_eq!((r, g, b), expected, "okhsl({h} {s} {l})");
    }
}

/// `colorToRgb` 的 `Math.round` + 裁剪。
#[test]
fn color_to_rgb_matches_ts_for_fractional_channels() {
    assert_eq!(
        color_to_rgb(Color::Rgb {
            r: 127.5,
            g: 0.4,
            b: 254.6
        }),
        RgbColor {
            r: 128,
            g: 0,
            b: 255
        }
    );
}

// ---------------------------------------------------------------------------
// 9. `cjk_punctuation_regex` 由区间表生成（评审 minor #10）
// ---------------------------------------------------------------------------

/// 旧实现手写了一个字符类，与 `CJK_PUNCTUATION_RANGES` 是两份数据；现在模式串由表生成。
/// 断言两者逐码点一致（表的覆盖 = 正则的覆盖），避免以后再漂移。
#[test]
fn cjk_punctuation_regex_agrees_with_the_range_table() {
    use kiana_tui::text::{cjk_punctuation_regex, is_cjk_punctuation};
    let regex = cjk_punctuation_regex();
    // 表里出现过的每个区间端点与内部抽样点都必须匹配，且与 `is_cjk_punctuation` 一致。
    for cp in [
        0x00B7u32, 0x2014, 0x2018, 0x2019, 0x201C, 0x201D, 0x2026, 0x3001, 0x3002, 0x3003, 0x3008,
        0x3011, 0x3014, 0x301F, 0x3030, 0x303D, 0x30A0, 0x30FB, 0xFE45, 0xFE46, 0xFF01, 0xFF08,
        0xFF09, 0xFF0C, 0xFF0E, 0xFF1A, 0xFF1B, 0xFF1F, 0xFF3B, 0xFF3D, 0xFF5B, 0xFF5D, 0xFF61,
        0xFF63, 0xFF65, 0x16FE2,
    ] {
        let ch = char::from_u32(cp).expect("scalar value");
        let text = ch.to_string();
        assert!(is_cjk_punctuation(ch), "U+{cp:X} must be CJK punctuation");
        assert!(
            regex.is_match(&text),
            "U+{cp:X} must match cjkPunctuationRegex"
        );
    }
    // 表外的字符两边都不认。
    for cp in [0x41u32, 0x4E2D, 0x3042, 0xD55C, 0xFF66, 0x30FA, 0x2010] {
        let ch = char::from_u32(cp).expect("scalar value");
        let text = ch.to_string();
        if is_cjk_punctuation(ch) {
            assert!(regex.is_match(&text), "U+{cp:X} table/table mismatch");
        } else {
            assert!(!regex.is_match(&text), "U+{cp:X} must not match");
        }
    }
    // 锚定：多字符输入（含前半是非标点）不算匹配。
    assert!(!regex.is_match("a，"));
}
