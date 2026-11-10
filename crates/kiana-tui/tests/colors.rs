//! `test/colors.test.ts`：颜色解析、色域映射与 ANSI 拼接。
//!
//! TS 源：`packages/tui/test/colors.test.ts`（4 个用例，标题保留在测试函数文档中以便对照）。

use kiana_tui::colors::{
    color_to_hex, color_to_okhsl, color_to_rgb, indexed_color, okhsl_color, oklch_color,
    parse_color, rgb_color, style_text,
};
use kiana_tui::terminal_colors::{Color, TerminalColorMode, TextAttributes, TextStyle};

/// 断言两个 `f64` 逐位相等（含 `-0.0` 与 `NaN` 的区分按值比较）。
fn assert_same(actual: Color, expected: Color, context: &str) {
    match (actual, expected) {
        (
            Color::Rgb {
                r: ar,
                g: ag,
                b: ab,
            },
            Color::Rgb {
                r: er,
                g: eg,
                b: eb,
            },
        ) => {
            assert_eq!(ar, er, "{context}: r");
            assert_eq!(ag, eg, "{context}: g");
            assert_eq!(ab, eb, "{context}: b");
        }
        _ => assert_eq!(actual, expected, "{context}"),
    }
}

#[test]
fn parses_hex_and_oklch_colors_and_rejects_everything_else() {
    assert_eq!(
        parse_color("#abc"),
        Some(Color::Rgb {
            r: 170.0,
            g: 187.0,
            b: 204.0
        })
    );
    assert_same(
        parse_color("oklch(62% 0.1 200)").unwrap(),
        oklch_color(0.62, 0.1, 200.0),
        "oklch(62% 0.1 200)",
    );
    // TS 抛 `/Invalid color value/`；冻结签名改为返回 None。
    assert_eq!(parse_color(""), None);
    assert_eq!(parse_color("red"), None);
}

#[test]
fn gamut_maps_oklch_to_srgb_including_the_lightness_limits() {
    assert_eq!(
        color_to_rgb(oklch_color(0.627955, 0.257683, 29.2339)),
        kiana_tui::terminal_colors::RgbColor { r: 255, g: 0, b: 0 }
    );
    assert_eq!(
        color_to_rgb(oklch_color(1.0, 0.3, 150.0)),
        kiana_tui::terminal_colors::RgbColor {
            r: 255,
            g: 255,
            b: 255
        }
    );
    assert_eq!(
        color_to_rgb(oklch_color(0.0, 0.3, 150.0)),
        kiana_tui::terminal_colors::RgbColor { r: 0, g: 0, b: 0 }
    );
}

#[test]
fn parses_okhsl_colors_and_round_trips_them() {
    // 红色 cusp 处的全饱和即纯 sRGB 红。
    assert_same(
        parse_color("okhsl(29.23 100% 56.8%)").unwrap(),
        rgb_color(255.0, 0.0, 0.0),
        "okhsl(29.23 100% 56.8%)",
    );
    assert_same(
        parse_color("OKHSL(250deg 60% 55%)").unwrap(),
        okhsl_color(250.0, 0.6, 0.55),
        "OKHSL(250deg 60% 55%)",
    );
    // TS 抛 `/s must be between 0 and 1/`；冻结签名改为返回 None。
    assert_eq!(parse_color("okhsl(250 160% 55%)"), None);
    for hex in ["#4f8eb3", "#20242a", "#f8f9fa"] {
        let channels = color_to_okhsl(parse_color(hex).unwrap());
        assert_eq!(
            color_to_hex(okhsl_color(channels.h, channels.s, channels.l)),
            hex,
            "round trip {hex}"
        );
    }
}

#[test]
fn styles_text_and_closes_sequences_in_reverse_order() {
    let attributes = TextAttributes {
        bold: Some(true),
        italic: Some(true),
        ..Default::default()
    };
    assert_eq!(
        style_text(
            "Ready",
            &TextStyle {
                attributes,
                fg: Some(rgb_color(18.0, 52.0, 86.0)),
                bg: Some(indexed_color(9)),
            },
            TerminalColorMode::Truecolor,
        ),
        "\x1b[38;2;18;52;86m\x1b[48;5;9m\x1b[1m\x1b[3mReady\x1b[23m\x1b[22m\x1b[49m\x1b[39m"
    );

    let styled = style_text(
        "Ready",
        &TextStyle {
            attributes: TextAttributes::default(),
            fg: Some(rgb_color(18.0, 52.0, 86.0)),
            bg: None,
        },
        TerminalColorMode::Palette256,
    );
    let rest = styled
        .strip_prefix("\x1b[38;5;")
        .expect("以 256 色前景序列开头")
        .strip_suffix("mReady\x1b[39m")
        .expect("以 mReady\\x1b[39m 结尾");
    assert!(
        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()),
        "styled = {styled:?}"
    );
}

// ---------------------------------------------------------------------------
// 回归：`colorToRgb` 的通道是 number，`colorToHex` 才做 `Math.round`
// ---------------------------------------------------------------------------

/// `mixColors(..., "srgb")` 会产出 `127.5` 这样的分数通道。TS 的 `colorToHex` 先
/// `Math.round`（`127.5 -> 128`），若中途按"截断"落成 `u8` 就会得到 `0x7f`。
#[test]
fn rounds_fractional_channels_instead_of_truncating() {
    let half = kiana_tui::colors::rgb_color(127.5, 60.0, 127.5);
    assert_eq!(color_to_hex(half), "#803c80");

    let mixed = kiana_tui::colors::mix_colors(
        rgb_color(255.0, 0.0, 0.0),
        rgb_color(0.0, 120.0, 255.0),
        0.5,
        kiana_tui::terminal_colors::ColorMixSpace::Srgb,
    );
    assert_eq!(color_to_hex(mixed), "#803c80");
    // truecolor 的 ANSI 同样按 Math.round 取整
    assert_eq!(
        kiana_tui::colors::foreground_ansi(half, TerminalColorMode::Truecolor),
        "\x1b[38;2;128;60;128m"
    );
}
