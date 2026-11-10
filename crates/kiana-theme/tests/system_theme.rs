//! `system-theme.test.ts` 的逐条移植（两个 describe）。
//!
//! 另外用 `tests/fixtures/theme_golden.json`（由 TS 参照实现生成）逐字节校验 56 个 token：
//! system 主题是**数值**拟合，测试里的阈值断言看不出 ulp 级偏差，黄金文件可以。

mod common;

use common::{lock, reset_theme_state};
use kiana_theme::{
    SystemThemeInput, ThemeColorValue, ThemeToken, generate_system_theme_colors,
    get_available_themes, get_resolved_theme_colors, get_theme_by_name, get_theme_export_colors,
    set_terminal_colors, wcag_contrast,
};
use kiana_tui::{
    RgbColor, TerminalColorScheme, TerminalColors, color_to_oklch, color_to_rgb, parse_color,
    rgb_color,
};
use serde_json::Value;

fn rgb(hex: &str) -> RgbColor {
    color_to_rgb(parse_color(hex).expect("valid hex color"))
}

fn to_color(value: RgbColor) -> kiana_tui::Color {
    rgb_color(f64::from(value.r), f64::from(value.g), f64::from(value.b))
}

fn lightness(color: RgbColor) -> f64 {
    color_to_oklch(to_color(color)).l
}

fn hue(color: RgbColor) -> f64 {
    color_to_oklch(to_color(color)).h
}

fn chroma(color: RgbColor) -> f64 {
    color_to_oklch(to_color(color)).c
}

fn dracula() -> SystemThemeInput {
    let palette = [
        "#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2",
        "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df", "#a4ffff", "#ffffff",
    ]
    .iter()
    .map(|hex| rgb(hex))
    .collect();
    SystemThemeInput {
        background: Some(rgb("#282a36")),
        foreground: Some(rgb("#f8f8f2")),
        palette: Some(palette),
        ..SystemThemeInput::default()
    }
}

fn frappe() -> SystemThemeInput {
    let palette = [
        "#51576d", "#e78284", "#a6d189", "#e5c890", "#8caaee", "#f4b8e4", "#81c8be", "#b5bfe2",
        "#626880", "#e67172", "#8ec772", "#d9ba73", "#7b9ef0", "#f2a4db", "#5abfb5", "#a5adce",
    ]
    .iter()
    .map(|hex| rgb(hex))
    .collect();
    SystemThemeInput {
        background: Some(rgb("#303446")),
        foreground: Some(rgb("#c6d0f5")),
        palette: Some(palette),
        ..SystemThemeInput::default()
    }
}

/// Dark with a palette, light with an unreadable foreground, background only, and mid-gray.
fn terminals() -> Vec<(&'static str, SystemThemeInput)> {
    vec![
        ("dracula", dracula()),
        (
            "solarizedLight",
            SystemThemeInput {
                background: Some(rgb("#fdf6e3")),
                foreground: Some(rgb("#657b83")),
                ..SystemThemeInput::default()
            },
        ),
        (
            "backgroundOnly",
            SystemThemeInput {
                background: Some(rgb("#1e1e1e")),
                ..SystemThemeInput::default()
            },
        ),
        (
            "midGray",
            SystemThemeInput {
                background: Some(rgb("#808080")),
                foreground: Some(rgb("#ffffff")),
                ..SystemThemeInput::default()
            },
        ),
    ]
}

const PANELS: [ThemeToken; 5] = [
    ThemeToken::UserMessageBg,
    ThemeToken::ToolPendingBg,
    ThemeToken::ToolSuccessBg,
    ThemeToken::ToolErrorBg,
    ThemeToken::SelectedBg,
];

fn resolved(input: &SystemThemeInput, token: ThemeToken) -> RgbColor {
    let generated = generate_system_theme_colors(input);
    match generated.colors.get(&token).expect("token present") {
        ThemeColorValue::Str(value) if value.is_empty() => {
            if PANELS.contains(&token) {
                input.background.expect("background")
            } else {
                input.foreground.expect("foreground")
            }
        }
        ThemeColorValue::Str(value) => rgb(value),
        ThemeColorValue::Index(index) => {
            panic!("unexpected palette index {index} for {token}")
        }
    }
}

fn to_terminal_colors(input: &SystemThemeInput) -> TerminalColors {
    TerminalColors {
        foreground: input.foreground,
        background: input.background,
        palette: input.palette.clone(),
    }
}

#[test]
fn keeps_body_text_readable_wcag_4_5_1_on_the_background_and_its_panels() {
    let _guard = lock();
    reset_theme_state();
    for (name, input) in terminals() {
        let text = resolved(&input, ThemeToken::Text);
        for surface in [
            input.background.expect("background"),
            resolved(&input, ThemeToken::SelectedBg),
        ] {
            let contrast = wcag_contrast(text, surface);
            assert!(contrast >= 4.5, "{name}: {contrast}");
        }
        let contrast = wcag_contrast(
            resolved(&input, ThemeToken::ToolTitle),
            resolved(&input, ThemeToken::ToolErrorBg),
        );
        assert!(contrast >= 4.5, "{name}: {contrast}");
    }
}

#[test]
fn orders_foreground_roles_by_contrast_and_keeps_panels_close_to_the_background() {
    let _guard = lock();
    reset_theme_state();
    for (name, input) in terminals() {
        let background = lightness(input.background.expect("background"));
        let offset = |token: ThemeToken| lightness(resolved(&input, token)) - background;
        // On mid-gray the levels collapse to the strongest reachable color.
        if name != "midGray" {
            assert!(
                offset(ThemeToken::Text).abs() > offset(ThemeToken::Muted).abs(),
                "{name}"
            );
            assert!(
                offset(ThemeToken::Muted).abs() > offset(ThemeToken::Dim).abs(),
                "{name}"
            );
        }
        let lighter =
            generate_system_theme_colors(&input).appearance == Some(TerminalColorScheme::Dark);
        for panel in PANELS {
            let contrast = wcag_contrast(
                resolved(&input, panel),
                input.background.expect("background"),
            );
            assert!(contrast < 2.0, "{name} {panel}: {contrast}");
            assert_eq!(offset(panel) > 0.0, lighter, "{name} {panel}");
        }
    }
}

#[test]
fn uses_the_terminal_foreground_and_palette_hues() {
    let _guard = lock();
    reset_theme_state();
    let dracula = dracula();
    assert_eq!(
        generate_system_theme_colors(&dracula)
            .colors
            .get(&ThemeToken::Text),
        Some(&ThemeColorValue::Str(String::new()))
    );
    // Solarized's foreground is below 4.5:1 on its own background, so text is darkened.
    let solarized = &terminals()[1].1;
    assert_ne!(
        generate_system_theme_colors(solarized)
            .colors
            .get(&ThemeToken::Text),
        Some(&ThemeColorValue::Str(String::new()))
    );
    let error_hue = hue(resolved(&dracula, ThemeToken::Error));
    let palette_hue = hue(dracula.palette.as_ref().expect("palette")[1]);
    assert!((error_hue - palette_hue).abs() < 8.0);
}

// https://github.com/earendil-works/kiana/issues/10255
#[test]
fn keeps_pastel_palette_colors_pastel_at_other_lightnesses() {
    let _guard = lock();
    reset_theme_state();
    let frappe = frappe();
    let pink = frappe.palette.as_ref().expect("palette")[5];
    let accent = resolved(&frappe, ThemeToken::Accent);
    // The accent is darker than the pink, but must not gain chroma (it was 2x before the cap).
    assert!(lightness(accent) < lightness(pink) - 0.05);
    assert!(chroma(accent) <= chroma(pink) * 1.03);
    for panel in [ThemeToken::UserMessageBg, ThemeToken::CustomMessageBg] {
        assert!(chroma(resolved(&frappe, panel)) <= 0.1, "{panel}");
    }
}

#[test]
fn renders_grayscale_at_zero_saturation() {
    let _guard = lock();
    reset_theme_state();
    let input = SystemThemeInput {
        saturation: Some(0.0),
        ..dracula()
    };
    let colors = generate_system_theme_colors(&input).colors;
    let error = match colors.get(&ThemeToken::Error).expect("error") {
        ThemeColorValue::Str(value) => parse_color(value).expect("hex color"),
        ThemeColorValue::Index(index) => parse_color(&format!("#{index}")).expect("hex"),
    };
    assert!(color_to_oklch(error).c < 0.005);
}

#[test]
fn falls_back_to_palette_indices_and_faint_text_without_a_background() {
    let _guard = lock();
    reset_theme_state();
    let generated = generate_system_theme_colors(&SystemThemeInput {
        appearance_hint: Some(TerminalColorScheme::Light),
        ..SystemThemeInput::default()
    });
    assert_eq!(generated.appearance, Some(TerminalColorScheme::Light));
    assert_eq!(
        generated.colors.get(&ThemeToken::Error),
        Some(&ThemeColorValue::Index(1))
    );
    assert_eq!(
        generated.colors.get(&ThemeToken::Text),
        Some(&ThemeColorValue::Str(String::new()))
    );
    assert_eq!(
        generated.colors.get(&ThemeToken::UserMessageBg),
        Some(&ThemeColorValue::Str(String::new()))
    );
    assert!(generated.dim.contains(&kiana_theme::ThemeColor::Muted));
    assert_eq!(
        generate_system_theme_colors(&SystemThemeInput {
            saturation: Some(0.0),
            ..SystemThemeInput::default()
        })
        .colors
        .get(&ThemeToken::Error),
        Some(&ThemeColorValue::Str(String::new()))
    );
}

#[test]
fn is_listed_first_has_no_export_colors_and_is_generated_from_the_terminal_colors() {
    let _guard = lock();
    reset_theme_state();
    assert_eq!(get_available_themes()[0], "system");
    assert_eq!(
        get_theme_export_colors(Some("system")),
        kiana_theme::ThemeExportColors::default()
    );

    set_terminal_colors(&to_terminal_colors(&dracula()));
    let theme = get_theme_by_name("system").expect("system theme");
    assert_eq!(theme.appearance(), TerminalColorScheme::Dark);
    assert_eq!(theme.get_fg_ansi(kiana_theme::ThemeColor::Text), "\x1b[39m");
    assert!(
        theme
            .get_fg_ansi(kiana_theme::ThemeColor::Error)
            .starts_with("\x1b[38;")
    );
}

#[test]
fn renders_faint_tokens_with_sgr_2_and_closes_it() {
    let _guard = lock();
    reset_theme_state();
    let theme = get_theme_by_name("system").expect("system theme");
    assert_eq!(
        theme.fg(kiana_theme::ThemeColor::Muted, "x"),
        "\x1b[39m\x1b[2mx\x1b[22;39m"
    );
    let style = kiana_theme::ThemeStyle {
        fg: Some(kiana_theme::ThemeFg::Token(kiana_theme::ThemeColor::Muted)),
        ..kiana_theme::ThemeStyle::default()
    };
    assert_eq!(
        theme.style("x", &style).expect("style"),
        "\x1b[39m\x1b[2mx\x1b[22m\x1b[39m"
    );
}

/// 与 TS 参照实现（`tests/fixtures/theme_golden.json`）逐字节比较。
#[test]
fn matches_the_reference_implementation() {
    let _guard = lock();
    reset_theme_state();
    let golden: Value = serde_json::from_str(include_str!("fixtures/theme_golden.json"))
        .expect("golden fixture parses");

    let inputs = [
        ("dracula", dracula()),
        (
            "solarizedLight",
            SystemThemeInput {
                background: Some(rgb("#fdf6e3")),
                foreground: Some(rgb("#657b83")),
                ..SystemThemeInput::default()
            },
        ),
        (
            "backgroundOnly",
            SystemThemeInput {
                background: Some(rgb("#1e1e1e")),
                ..SystemThemeInput::default()
            },
        ),
        (
            "midGray",
            SystemThemeInput {
                background: Some(rgb("#808080")),
                foreground: Some(rgb("#ffffff")),
                ..SystemThemeInput::default()
            },
        ),
        ("frappe", frappe()),
        (
            "grayscale",
            SystemThemeInput {
                saturation: Some(0.0),
                ..dracula()
            },
        ),
        (
            "noColorsLight",
            SystemThemeInput {
                appearance_hint: Some(TerminalColorScheme::Light),
                ..SystemThemeInput::default()
            },
        ),
        (
            "noColors",
            SystemThemeInput {
                saturation: Some(0.0),
                ..SystemThemeInput::default()
            },
        ),
    ];

    for (name, input) in inputs {
        let generated = generate_system_theme_colors(&input);
        let expected = &golden[name];
        let expected_colors = expected["colors"].as_object().expect("colors object");
        assert_eq!(expected_colors.len(), 56, "{name}");
        for (token_name, expected_value) in expected_colors {
            let token = ThemeToken::from_str(token_name).expect("known token");
            let actual = generated.colors.get(&token).expect("token present");
            match (expected_value, actual) {
                (Value::String(expected), ThemeColorValue::Str(actual)) => {
                    assert_eq!(actual, expected, "{name} {token_name}");
                }
                (Value::Number(expected), ThemeColorValue::Index(actual)) => {
                    assert_eq!(
                        u64::from(*actual),
                        expected.as_u64().expect("palette index"),
                        "{name} {token_name}"
                    );
                }
                _ => {
                    panic!("{name} {token_name}: kind mismatch ({expected_value:?} vs {actual:?})")
                }
            }
        }
        let expected_dim: Vec<String> = expected["dim"]
            .as_array()
            .expect("dim array")
            .iter()
            .map(|value| value.as_str().expect("dim token").to_string())
            .collect();
        let actual_dim: Vec<String> = generated
            .dim
            .iter()
            .map(|color| color.as_str().to_string())
            .collect();
        assert_eq!(actual_dim, expected_dim, "{name} dim");
        let expected_appearance = match expected["appearance"].as_str() {
            Some("dark") => Some(TerminalColorScheme::Dark),
            Some("light") => Some(TerminalColorScheme::Light),
            _ => None,
        };
        assert_eq!(
            generated.appearance, expected_appearance,
            "{name} appearance"
        );
    }

    // 内置主题的解析颜色（`getResolvedThemeColors`）与导出颜色。
    set_terminal_colors(&TerminalColors::default());
    common::reset_theme_state();
    for theme_name in ["dark", "light"] {
        let actual = get_resolved_theme_colors(Some(theme_name)).expect("resolved colors");
        let expected = golden[&format!("{theme_name}Resolved")]
            .as_object()
            .expect("resolved object");
        assert_eq!(actual.len(), expected.len(), "{theme_name}");
        let actual_json = Value::Object(
            actual
                .iter()
                .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                .collect(),
        );
        assert_eq!(
            &actual_json,
            &Value::Object(expected.clone()),
            "{theme_name}"
        );
    }
    for theme_name in ["dark", "light"] {
        let actual = get_theme_export_colors(Some(theme_name));
        let expected = golden[&format!("{theme_name}Export")]
            .as_object()
            .expect("export object");
        for key in ["pageBg", "cardBg", "infoBg"] {
            let expected_value = expected[key].as_str().map(str::to_string);
            let actual_value = match key {
                "pageBg" => actual.page_bg.clone(),
                "cardBg" => actual.card_bg.clone(),
                _ => actual.info_bg.clone(),
            };
            assert_eq!(actual_value, expected_value, "{theme_name} {key}");
        }
    }
    assert_eq!(
        get_theme_export_colors(Some("system")),
        kiana_theme::ThemeExportColors::default()
    );
}

/// 评审 finding（minor）：JS 的 `clamp(NaN, 0, 1)` 仍是 `NaN`（`Math.min`/`Math.max` 传播
/// NaN），有 background 时第一次 `okhslColor(h, s, l)` 会触发 kiana-tui 的 `requireFinite` →
/// `Error: s must be finite`。Rust 用 panic 承载同一条 `error.message`；没有 background 时
/// TS 的 `indexedColors()` 只做 `saturation > 0` 的比较（NaN 为 false），不抛。
/// `±Infinity` 与 JS 一样夹到 1 / 0。
#[test]
fn nan_saturation_throws_like_okhsl_color_validation() {
    let _guard = lock();
    reset_theme_state();

    let mut input = dracula();
    input.saturation = Some(f64::NAN);
    let payload = std::panic::catch_unwind(|| generate_system_theme_colors(&input))
        .expect_err("NaN saturation must fail like TS");
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied());
    assert_eq!(message, Some("s must be finite"));

    // 没有 background：`indexedColors(NaN, hint)` 不调用 okhslColor，TS 不抛。
    let indexed = generate_system_theme_colors(&SystemThemeInput {
        saturation: Some(f64::NAN),
        appearance_hint: Some(TerminalColorScheme::Light),
        ..SystemThemeInput::default()
    });
    assert_eq!(indexed.appearance, Some(TerminalColorScheme::Light));
    assert_eq!(
        indexed.colors.get(&ThemeToken::Error),
        Some(&ThemeColorValue::Str(String::new()))
    );

    // ±Infinity：JS 的 clamp 得到 1 / 0。
    let colored = |saturation: f64| {
        let mut input = dracula();
        input.saturation = Some(saturation);
        generate_system_theme_colors(&input).colors
    };
    assert_eq!(colored(f64::INFINITY), colored(1.0));
    assert_eq!(colored(f64::NEG_INFINITY), colored(0.0));
}
