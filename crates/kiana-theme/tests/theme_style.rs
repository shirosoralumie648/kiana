//! `theme-style.test.ts` 的逐条移植。

mod common;

use std::sync::Arc;

use common::{EnvGuard, builtin_json, lock, reset_theme_state};
use kiana_theme::{
    Theme, ThemeBg, ThemeBgValue, ThemeColor, ThemeFg, ThemeStyle, ThemeToken,
    load_theme_from_path, set_terminal_colors,
};
use kiana_tui::{
    Color, RgbColor, TerminalColorMode, TerminalColorScheme, TerminalColors, TextAttributes,
    TextStyle, color_to_hex, indexed_color, okhsl_color, style_text,
};
use tempfile::TempDir;

/// 载入内置主题的副本，交给 `edit` 修改后从临时文件加载（`truecolor` 模式）。
fn load_theme(base: &str, edit: impl FnOnce(&mut serde_json::Value)) -> (TempDir, Arc<Theme>) {
    let mut json = builtin_json(base);
    edit(&mut json);
    let dir = tempfile::tempdir().expect("temp dir");
    let name = json["name"].as_str().expect("theme name").to_string();
    let path = dir.path().join(format!("{name}.json"));
    std::fs::write(
        &path,
        serde_json::to_string(&json).expect("serialize theme"),
    )
    .expect("write theme");
    let theme =
        load_theme_from_path(&path, Some(TerminalColorMode::Truecolor)).expect("theme loads");
    (dir, theme)
}

#[test]
fn renders_theme_tokens_the_same_as_the_generic_text_styler() {
    let _guard = lock();
    reset_theme_state();
    let (_dir, theme) = load_theme("dark", |_| {});

    let style = ThemeStyle {
        fg: Some(ThemeFg::Token(ThemeColor::Success)),
        bg: Some(ThemeBgValue::Token(ThemeBg::ToolSuccessBg)),
        attributes: TextAttributes {
            bold: Some(true),
            ..TextAttributes::default()
        },
    };
    let expected = style_text(
        "Ready",
        &TextStyle {
            fg: theme.color(kiana_theme::ThemeToken::Success),
            bg: theme.color(kiana_theme::ThemeToken::ToolSuccessBg),
            attributes: style.attributes,
        },
        TerminalColorMode::Truecolor,
    );
    assert_eq!(theme.style("Ready", &style).expect("style"), expected);
}

#[test]
fn rejects_unknown_tokens_and_tokens_in_the_wrong_slot() {
    let _guard = lock();
    reset_theme_state();
    let (_dir, theme) = load_theme("dark", |_| {});

    let style = ThemeStyle {
        fg: Some(ThemeFg::Named("notAToken")),
        ..ThemeStyle::default()
    };
    assert_eq!(
        theme
            .style("x", &style)
            .expect_err("unknown token")
            .to_string(),
        "Unknown theme color: notAToken"
    );

    // 背景 token 不是前景颜色；用 theme.color(token) 拿它的颜色。
    let style = ThemeStyle {
        fg: Some(ThemeFg::Named("userMessageBg")),
        ..ThemeStyle::default()
    };
    assert_eq!(
        theme
            .style("x", &style)
            .expect_err("wrong slot")
            .to_string(),
        "Unknown theme color: userMessageBg"
    );
    assert_eq!(
        theme
            .fg_named("userMessageBg", "x")
            .expect_err("wrong slot")
            .to_string(),
        "Unknown theme color: userMessageBg"
    );
    assert_eq!(
        theme
            .bg_named("accent", "x")
            .expect_err("wrong slot")
            .to_string(),
        "Unknown theme color: accent"
    );
}

#[test]
fn loads_oklch_theme_values() {
    let _guard = lock();
    reset_theme_state();
    let (_dir, theme) = load_theme("dark", |json| {
        json["colors"]["accent"] = serde_json::json!("oklch(62% 0.1 200)");
    });
    assert_eq!(
        theme.color(kiana_theme::ThemeToken::Accent),
        Some(Color::Oklch {
            l: 0.62,
            c: 0.1,
            h: 200.0
        })
    );
}

#[test]
fn loads_okhsl_theme_values_including_through_variables() {
    let _guard = lock();
    reset_theme_state();
    let (_dir, theme) = load_theme("dark", |json| {
        json["vars"]["brand"] = serde_json::json!("okhsl(250 60% 55%)");
        json["colors"]["accent"] = serde_json::json!("brand");
        json["colors"]["error"] = serde_json::json!("okhsl(20 90% 60%)");
    });
    assert_eq!(
        color_to_hex(
            theme
                .color(kiana_theme::ThemeToken::Accent)
                .expect("accent")
        ),
        color_to_hex(okhsl_color(250.0, 0.6, 0.55))
    );
    assert_eq!(
        color_to_hex(theme.color(kiana_theme::ThemeToken::Error).expect("error")),
        color_to_hex(okhsl_color(20.0, 0.9, 0.6))
    );
}

#[test]
fn detects_the_appearance_unless_it_is_declared() {
    let _guard = lock();
    reset_theme_state();
    let _env = EnvGuard::unset("COLORFGBG");

    assert_eq!(
        load_theme("dark", |_| {}).1.appearance(),
        TerminalColorScheme::Dark
    );
    assert_eq!(
        load_theme("light", |_| {}).1.appearance(),
        TerminalColorScheme::Light
    );
    // 没有声明时，从主题自己的颜色推导。
    for base in ["dark", "light"] {
        let (_dir, theme) = load_theme(base, |json| {
            if let Some(object) = json.as_object_mut() {
                object.remove("appearance");
            }
        });
        assert_eq!(
            theme.appearance(),
            if base == "dark" {
                TerminalColorScheme::Dark
            } else {
                TerminalColorScheme::Light
            }
        );
    }
    assert_eq!(
        load_theme("dark", |json| {
            json["appearance"] = serde_json::json!("light");
        })
        .1
        .appearance(),
        TerminalColorScheme::Light
    );

    // ANSI 0-15 跟随终端调色板，因此这类主题跟随终端背景。
    let (_dir, palette_only) = load_theme("dark", |json| {
        if let Some(object) = json.as_object_mut() {
            object.remove("appearance");
        }
        if let Some(colors) = json
            .get_mut("colors")
            .and_then(|value| value.as_object_mut())
        {
            for (key, value) in colors.iter_mut() {
                *value = serde_json::json!(if key.ends_with("Bg") { 0 } else { 7 });
            }
        }
    });
    assert_eq!(palette_only.appearance(), TerminalColorScheme::Dark);
    set_terminal_colors(&TerminalColors {
        background: Some(RgbColor {
            r: 250,
            g: 250,
            b: 250,
        }),
        ..TerminalColors::default()
    });
    assert_eq!(palette_only.appearance(), TerminalColorScheme::Light);
}

#[test]
fn renders_empty_tokens_as_terminal_defaults_and_reports_concrete_colors_for_them() {
    let _guard = lock();
    reset_theme_state();
    let _env = EnvGuard::unset("COLORFGBG");
    let (_dir, theme) = load_theme("dark", |json| {
        json["colors"]["text"] = serde_json::json!("");
        json["colors"]["userMessageBg"] = serde_json::json!("");
    });
    assert_eq!(theme.fg(ThemeColor::Text, "x"), "\x1b[39mx\x1b[39m");
    assert_eq!(theme.bg(ThemeBg::UserMessageBg, "x"), "\x1b[49mx\x1b[49m");
    assert_eq!(
        color_to_hex(theme.color(kiana_theme::ThemeToken::Text).expect("text")),
        "#e5e5e7"
    );
    assert_eq!(
        color_to_hex(
            theme
                .color(kiana_theme::ThemeToken::UserMessageBg)
                .expect("bg")
        ),
        "#000000"
    );

    set_terminal_colors(&TerminalColors {
        foreground: Some(RgbColor {
            r: 200,
            g: 210,
            b: 220,
        }),
        background: Some(RgbColor {
            r: 10,
            g: 20,
            b: 30,
        }),
        ..TerminalColors::default()
    });
    assert_eq!(
        color_to_hex(theme.color(kiana_theme::ThemeToken::Text).expect("text")),
        "#c8d2dc"
    );
    assert_eq!(
        color_to_hex(
            theme
                .color(kiana_theme::ThemeToken::UserMessageBg)
                .expect("bg")
        ),
        "#0a141e"
    );
}

#[test]
fn wraps_chalk_styles_like_chalk_on_line_breaks_and_embedded_closes() {
    let _guard = lock();
    reset_theme_state();
    let (_dir, theme) = load_theme("dark", |_| {});
    // chalk（level >= 1）在每个换行前关闭、换行后重新开启；\r\n 保持为一个整体。
    assert_eq!(theme.bold("a\nb"), "\x1b[1ma\x1b[22m\n\x1b[1mb\x1b[22m");
    assert_eq!(theme.bold("a\r\nb"), "\x1b[1ma\x1b[22m\r\n\x1b[1mb\x1b[22m");
    // 内嵌的“本样式关闭序列”会被替换成“关闭 + 重新开启”（chalk 的 replaceClose）。
    assert_eq!(theme.bold("a\x1b[22mb"), "\x1b[1ma\x1b[22m\x1b[1mb\x1b[22m");
    // 其他样式的关闭序列不受影响。
    assert_eq!(theme.bold("a\x1b[39mb"), "\x1b[1ma\x1b[39mb\x1b[22m");
    assert_eq!(theme.bold(""), "");
    // 连续换行：每个换行各自关闭/开启（chalk 的 encase 语义）。
    assert_eq!(
        theme.bold("a\n\nb"),
        "\x1b[1ma\x1b[22m\n\x1b[1m\x1b[22m\n\x1b[1mb\x1b[22m"
    );
    assert_eq!(theme.bold("\n"), "\x1b[1m\x1b[22m\n\x1b[1m\x1b[22m");
    // 只有 CR 没有 LF：不拆行。
    assert_eq!(theme.bold("a\rb"), "\x1b[1ma\rb\x1b[22m");
    assert_eq!(theme.italic("a\nb"), "\x1b[3ma\x1b[23m\n\x1b[3mb\x1b[23m");
    assert_eq!(
        theme.underline("a\nb"),
        "\x1b[4ma\x1b[24m\n\x1b[4mb\x1b[24m"
    );
    assert_eq!(theme.inverse("a\nb"), "\x1b[7ma\x1b[27m\n\x1b[7mb\x1b[27m");
    assert_eq!(
        theme.strikethrough("a\nb"),
        "\x1b[9ma\x1b[29m\n\x1b[9mb\x1b[29m"
    );
    // markdown 的 bold/italic/underline 走同一套 chalk 语义。
    let markdown = kiana_theme::get_markdown_theme();
    assert_eq!((markdown.bold)("a\nb"), theme.bold("a\nb"));
    assert_eq!((markdown.italic)("a\r\nb"), theme.italic("a\r\nb"));
    assert_eq!((markdown.underline)("a\nb"), theme.underline("a\nb"));
}

#[test]
fn reports_specific_oklch_and_okhsl_failure_reasons_like_the_reference() {
    let _guard = lock();
    reset_theme_state();
    let cases = [
        ("okhsl(250 150% 55%)", "s must be between 0 and 1: 1.5"),
        ("okhsl(250 50% 150%)", "l must be between 0 and 1: 1.5"),
        ("okhsl(1e999 50% 55%)", "h must be finite"),
        ("okhsl(250 1e999 55%)", "s must be finite"),
        ("okhsl(250 50% 1e999)", "l must be finite"),
        ("oklch(150% 0.1 250)", "l must be between 0 and 1: 1.5"),
        ("oklch(50% -0.1 250)", "c must not be negative: -0.1"),
        ("oklch(1e999 0.1 250)", "l must be finite"),
        ("oklch(50% 1e999 250)", "c must be finite"),
        ("oklch(50% 0.1 1e999)", "h must be finite"),
        ("#ggg", "Invalid color value: #ggg"),
    ];
    for (value, expected) in cases {
        let mut json = builtin_json("dark");
        json["colors"]["accent"] = serde_json::json!(value);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("theme.json");
        std::fs::write(&path, serde_json::to_string(&json).expect("serialize"))
            .expect("write theme");
        let error = load_theme_from_path(&path, Some(TerminalColorMode::Truecolor))
            .expect_err("invalid color");
        assert_eq!(error.to_string(), expected, "value: {value}");
    }
}

#[test]
fn try_fg_reports_missing_tokens_where_fg_falls_back() {
    use indexmap::IndexMap;
    use kiana_theme::{ThemeColorValue, ThemeError, ThemeOptions, ThemeToken};

    let mut fg = IndexMap::new();
    for token in [
        ThemeToken::Muted,
        ThemeToken::Text,
        ThemeToken::SelectedBg,
        ThemeToken::ThinkingXhigh,
        ThemeToken::Accent,
    ] {
        fg.insert(token, ThemeColorValue::Str("#ffffff".to_string()));
    }
    let theme = Theme::new(
        fg,
        IndexMap::new(),
        TerminalColorMode::Truecolor,
        ThemeOptions::default(),
    )
    .expect("incomplete but constructible theme");

    // 不完整的主题（绕过校验器）：TS 的 fg() 抛错，Rust 的 fg() 静默退化为终端默认色，
    // try_fg()/THEME 代理给出逐字错误。
    assert_eq!(theme.fg(ThemeColor::Error, "x"), "\x1b[39mx\x1b[39m");
    assert_eq!(
        theme.try_fg(ThemeColor::Error, "x").expect_err("missing"),
        ThemeError::new("Unknown theme color: error")
    );
    assert_eq!(
        theme
            .try_get_fg_ansi(ThemeColor::Error)
            .expect_err("missing"),
        ThemeError::new("Unknown theme color: error")
    );
    assert_eq!(
        theme
            .try_get_bg_ansi(ThemeBg::UserMessageBg)
            .expect_err("missing"),
        ThemeError::new("Unknown theme color: userMessageBg")
    );
}

#[test]
fn unvalidated_theme_documents_follow_the_reference_cast_semantics() {
    let _guard = lock();
    reset_theme_state();
    let dir = tempfile::tempdir().expect("temp dir");

    // 未知的 colors 键：TS 在 splitThemeColors() 里抛 TypeError。
    let mut json = builtin_json("dark");
    json["colors"]["bogus"] = serde_json::json!("#fff");
    let path = dir.path().join("unknown.json");
    std::fs::write(&path, serde_json::to_string(&json).expect("serialize")).expect("write");
    let error =
        load_theme_from_path(&path, Some(TerminalColorMode::Truecolor)).expect_err("unknown token");
    assert_eq!(
        error.to_string(),
        "Cannot read properties of undefined (reading 'slot')"
    );

    // 未被引用的非法变量不影响加载。
    let mut json = builtin_json("dark");
    json["vars"]["unused"] = serde_json::Value::Null;
    let path = dir.path().join("lazy-var.json");
    std::fs::write(&path, serde_json::to_string(&json).expect("serialize")).expect("write");
    assert!(load_theme_from_path(&path, Some(TerminalColorMode::Truecolor)).is_ok());

    // 被引用时才抛 TS 的 TypeError。
    let mut json = builtin_json("dark");
    json["vars"]["broken"] = serde_json::Value::Null;
    json["colors"]["accent"] = serde_json::json!("broken");
    let path = dir.path().join("bad-var.json");
    std::fs::write(&path, serde_json::to_string(&json).expect("serialize")).expect("write");
    let error = load_theme_from_path(&path, Some(TerminalColorMode::Truecolor))
        .expect_err("referenced invalid var");
    assert_eq!(
        error.to_string(),
        "Cannot read properties of null (reading 'startsWith')"
    );

    // 目录作为主题路径：Node 的 EISDIR 文案（readFileSync 是 read 动作、不带路径）。
    let error = load_theme_from_path(dir.path(), Some(TerminalColorMode::Truecolor))
        .expect_err("directory");
    assert_eq!(
        error.to_string(),
        "EISDIR: illegal operation on a directory, read"
    );

    // 不存在的路径：Node 的 ENOENT 文案（含路径）。
    let missing = dir.path().join("missing.json");
    let error = load_theme_from_path(&missing, Some(TerminalColorMode::Truecolor))
        .expect_err("missing file");
    assert_eq!(
        error.to_string(),
        format!(
            "ENOENT: no such file or directory, open '{}'",
            missing.display()
        )
    );

    // 语法错误：V8 的 JSON.parse 文案（position/line/column）。
    let path = dir.path().join("broken.json");
    std::fs::write(&path, "{\"name\":\"x\",\"colors\":{},}").expect("write");
    let error =
        load_theme_from_path(&path, Some(TerminalColorMode::Truecolor)).expect_err("broken json");
    assert_eq!(
        error.to_string(),
        format!(
            "Failed to parse theme {}: SyntaxError: Expected double-quoted property name in JSON at position 24 (line 1 column 25)",
            path.display()
        )
    );
}

/// 回归：未校验文档里的 JS 对象语义（`Object.prototype` 命名的颜色键、数组 `vars` 的
/// `length`）。对应 `theme.ts` 的 `splitThemeColors()` / `resolveVarRefs()`。
#[test]
fn loads_unvalidated_prototype_color_keys_and_array_vars() {
    let _guard = lock();
    reset_theme_state();
    let dir = tempfile::tempdir().expect("temp dir");

    // `THEME_TOKENS["toString"]` 命中的是继承成员（`.slot` 为 undefined → 前景 token），
    // TS 照常加载；`__proto__` 则在赋值时走原型 setter 被丢弃。
    let mut json = builtin_json("dark");
    json["colors"]["toString"] = serde_json::json!("#112233");
    json["colors"]["constructor"] = serde_json::json!("#445566");
    if let serde_json::Value::Object(colors) = &mut json["colors"] {
        colors.insert("__proto__".to_string(), serde_json::json!("#778899"));
    }
    let path = dir.path().join("prototype-keys.json");
    std::fs::write(&path, serde_json::to_string(&json).expect("serialize")).expect("write");
    let theme = load_theme_from_path(&path, Some(TerminalColorMode::Truecolor))
        .expect("prototype-named color keys are accepted");
    // 真实 token 的颜色不受影响（accent 在 dark.json 里是变量引用）。
    let builtin = kiana_theme::get_theme_by_name("dark").expect("dark theme");
    assert_eq!(
        theme.color(ThemeToken::Accent),
        builtin.color(ThemeToken::Accent)
    );

    // 数组 `vars`：`"length"` 是数字（数组长度），作为颜色索引使用。
    let mut json = builtin_json("dark");
    {
        let colors = json["colors"].as_object_mut().expect("colors");
        for value in colors.values_mut() {
            *value = serde_json::json!("#eeeeee");
        }
        colors.insert("accent".to_string(), serde_json::json!("length"));
    }
    json["vars"] = serde_json::json!(["#112233", "#445566"]);
    let path = dir.path().join("array-vars.json");
    std::fs::write(&path, serde_json::to_string(&json).expect("serialize")).expect("write");
    let theme = load_theme_from_path(&path, Some(TerminalColorMode::Truecolor))
        .expect("array vars length resolves");
    assert_eq!(theme.color(ThemeToken::Accent), Some(indexed_color(2)));
}

/// 覆盖（ARCHITECTURE §4.8 item 9）：其余公开辅助函数 / 类型 / 常量的最小触达。
#[test]
fn public_surface_helpers_are_callable() {
    let _guard = lock();
    reset_theme_state();

    // 全局终端颜色、明暗与 pending 标志。
    kiana_theme::mark_terminal_colors_pending();
    assert!(kiana_theme::terminal_colors_pending());
    set_terminal_colors(&TerminalColors::default());
    assert!(!kiana_theme::terminal_colors_pending());
    kiana_theme::set_terminal_color_scheme(Some(TerminalColorScheme::Light));
    assert_eq!(
        kiana_theme::terminal_color_scheme(),
        Some(TerminalColorScheme::Light)
    );
    assert_eq!(kiana_theme::terminal_colors().foreground, None);

    // 读取进程环境变量的包装器。
    {
        let _env = EnvGuard::set("COLORFGBG", "0;15");
        assert_eq!(
            kiana_theme::detect_color_fg_bg_theme(),
            Some(TerminalColorScheme::Light)
        );
        assert_eq!(
            kiana_theme::detect_terminal_theme(&TerminalColors::default(), None),
            TerminalColorScheme::Light
        );
    }

    // 类型别名、常量、token 描述符与颜色值枚举。
    let _appearance: kiana_theme::ThemeAppearance = TerminalColorScheme::Dark;
    let _terminal_theme: kiana_theme::TerminalTheme = TerminalColorScheme::Light;
    assert_eq!(kiana_theme::SYSTEM_THEME_NAME, "system");
    assert_eq!(kiana_theme::TERMINAL_QUERY_TIMEOUT_MS, 100);
    let descriptor: &kiana_theme::ThemeTokenDescriptor = &kiana_theme::THEME_TOKENS[0];
    assert_eq!(descriptor.slot, kiana_theme::Slot::Foreground);
    assert_eq!(kiana_theme::ThemeToken::NAMES.len(), 56);
    assert_eq!(kiana_theme::THEME_COLOR_NAMES.len(), 56);
    assert_eq!(kiana_theme::ThemeColorValue::Index(3).as_index(), Some(3));
    assert_eq!(
        kiana_theme::ThemeColorValue::Str("#fff".to_string()).as_str(),
        Some("#fff")
    );
    let export = kiana_theme::ThemeExport {
        page_bg: None,
        card_bg: None,
        info_bg: None,
    };
    assert_eq!(export, kiana_theme::ThemeExport::default());
    let json: kiana_theme::ThemeJson =
        kiana_theme::validate_theme_json("dark", &builtin_json("dark")).expect("valid theme");
    assert_eq!(json.name.as_deref(), Some("dark"));
    let validator: kiana_theme::ThemeJsonValidator = Box::new(kiana_theme::validate_theme_json);
    kiana_theme::set_theme_json_validator(validator);
    kiana_theme::clear_theme_json_validator();
    let _proxy: kiana_theme::ThemeProxy = kiana_theme::theme();

    // system 主题生成结果与相对亮度。
    let generated: kiana_theme::SystemThemeColors =
        kiana_theme::generate_system_theme_colors(&kiana_theme::SystemThemeInput {
            foreground: None,
            background: None,
            palette: None,
            saturation: None,
            appearance_hint: None,
        });
    assert!(!generated.colors.is_empty());
    assert!(
        kiana_theme::relative_luminance(RgbColor {
            r: 255,
            g: 255,
            b: 255
        }) > 0.9
    );

    // 失败可见的 `try_bg()`。
    let theme = kiana_theme::get_theme_by_name("dark").expect("dark theme");
    assert!(theme.try_bg(ThemeBg::UserMessageBg, "x").is_ok());
}
