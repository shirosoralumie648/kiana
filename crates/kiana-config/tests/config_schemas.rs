//! `packages/coding-agent/test/config-schemas.test.ts` 的 settings 子集。
//!
//! 逐条对应关系：
//!
//! | Rust 测试 | TS 用例 |
//! |---|---|
//! | [`validates_representative_settings_json_documents`] | `it.each` 的 settings.json 行（`validates representative settings.json documents`） |
//! | [`rejects_settings_values_that_runtime_accessors_reject`] | `rejects settings values that runtime accessors reject` |
//! | [`accepts_schema_in_settings_json_and_preserves_unknown_settings`] | `accepts $schema in settings.json and preserves unknown settings`（只测数据模型，SettingsManager 部分属于 settings-manager 单元） |
//! | [`settings_defaults_match_the_typescript_source`] | 无同名 TS 用例（§2.7 的逐字段默认值） |
//! | [`rejects_non_positive_settings_token_limits`] | 无同名 TS 用例（settings 侧的非正整数限制） |
//!
//! 不在此文件的 TS 用例：4 个生成产物的逐字节比较、`describes legacy settings migrations
//! accurately`（都依赖 `generate-schemas.ts` 产物，阶段 8，`ARCHITECTURE.md` §8.25）；
//! models.json / keybindings.json / theme 的用例分别在 models-config、keybindings、kiana-theme 单元。

use kiana_ai::utils::retry::DEFAULT_MAX_AGENT_RETRY_DELAY_MS;
use kiana_config::settings::defaults::{SETTINGS_DEFAULTS, SettingsDefaults};
use kiana_config::settings::schema::{
    self, CodemodeMode, CompactionSettings, DefaultProjectTrust, DoubleEscapeAction,
    FullscreenExitOutput, FullscreenScrollbar, FullscreenWheelScrollLines, MermaidRenderingMode,
    ModelThinkingLevel, QuietStartup, SessionShareDestination, Settings, SettingsInput,
    SteeringMode, TerminalImagesSetting, TimeoutSetting, TransportSetting, TreeFilterMode, TuiMode,
    validate_settings_schema,
};
use serde_json::{Value, json};

fn accepts(value: Value) -> bool {
    schema::settings_schema_accepts(&value)
}

// ---------------------------------------------------------------------------
// TS: it.each([...]) "validates representative $name documents"（settings.json 行）
// ---------------------------------------------------------------------------

#[test]
fn validates_representative_settings_json_documents() {
    let valid = json!({
        "theme": "dark",
        "cacheWarming": "idle",
        "extensionSetting": { "enabled": true },
        "queueMode": "all",
        "websockets": true,
        "skills": { "enableSkillCommands": true, "customDirectories": ["./skills"] },
        "retry": { "maxDelayMs": 1000 },
    });
    assert!(
        accepts(valid),
        "the representative settings.json document must validate"
    );
    assert!(!accepts(json!({ "cacheWarming": "always" })));
}

// ---------------------------------------------------------------------------
// TS: "rejects settings values that runtime accessors reject"
// ---------------------------------------------------------------------------

#[test]
fn rejects_settings_values_that_runtime_accessors_reject() {
    for invalid in [
        json!({ "compaction": { "reserveTokens": -1 } }),
        json!({ "compaction": { "keepRecentTokens": 1.5 } }),
        json!({
            "compaction": {
                "modelOverrides": { "provider/model": { "reserveTokens": 9_007_199_254_740_992.0 } }
            }
        }),
        json!({ "httpIdleTimeoutMs": -1 }),
        json!({ "websocketConnectTimeoutMs": -1 }),
        json!({ "httpIdleTimeoutMs": "bogus" }),
        json!({ "websocketConnectTimeoutMs": "30000" }),
        json!({ "codemode": { "inlineBudget": -1 } }),
    ] {
        assert!(!accepts(invalid.clone()), "should reject {invalid}");
    }

    let valid = json!({
        "compaction": {
            "reserveTokens": 0,
            "keepRecentTokens": 9_007_199_254_740_991.0,
            "modelOverrides": { "provider/model": { "reserveTokens": 0 } },
        },
        "httpIdleTimeoutMs": 0,
        "websocketConnectTimeoutMs": "disabled",
        "codemode": { "inlineBudget": 0 },
    });
    assert!(accepts(valid.clone()), "should accept {valid}");
}

// ---------------------------------------------------------------------------
// TS: Compile(SettingsSchema).Check —— `Type.Record` 的 `^.*$` patternProperties
// ---------------------------------------------------------------------------

#[test]
fn record_keys_with_line_terminators_bypass_value_validation() {
    // TypeBox 1.3.27 把 `Type.Record(Type.String(), T)` 编译成
    // `patternProperties: { "^.*$": T }`，且不写 `additionalProperties`（缺省放行）。
    // ECMA-262 的 `.` 不匹配行终止符，所以含 \n、\r、U+2028、U+2029 的键对应的值
    // 不参与校验（TS 侧实测 `Check` 返回 true）。
    for line_terminator in ["\n", "\r", "\u{2028}", "\u{2029}"] {
        for key in [
            format!("provider{line_terminator}model"),
            // 只有行终止符的键同样不匹配 `^.*$`。
            line_terminator.to_string(),
        ] {
            let mut levels = serde_json::Map::new();
            levels.insert(key.clone(), json!("bogus"));
            assert!(
                accepts(json!({ "modelThinkingLevels": Value::Object(levels) })),
                "modelThinkingLevels key {key:?} must bypass validation"
            );

            let mut overrides = serde_json::Map::new();
            overrides.insert(key.clone(), json!(5));
            assert!(
                accepts(json!({ "compaction": { "modelOverrides": Value::Object(overrides) } })),
                "compaction.modelOverrides key {key:?} must bypass validation"
            );

            // 非法条目对象（负数 token 限制）同样不被校验。
            let mut bad_entries = serde_json::Map::new();
            bad_entries.insert(key.clone(), json!({ "reserveTokens": -1 }));
            assert!(
                accepts(json!({ "compaction": { "modelOverrides": Value::Object(bad_entries) } })),
                "compaction.modelOverrides key {key:?} must skip entry validation"
            );
        }
    }

    // 不含行终止符的键仍然逐条校验。
    assert!(!accepts(
        json!({ "modelThinkingLevels": { "provider/model": "bogus" } })
    ));
    assert!(!accepts(
        json!({ "compaction": { "modelOverrides": { "provider/model": 5 } } })
    ));
    assert!(!accepts(json!({
        "compaction": { "modelOverrides": { "provider/model": { "reserveTokens": -1 } } }
    })));
}

// ---------------------------------------------------------------------------
// TS: settings-manager.ts `loadFromStorage`（`JSON.parse` + `migrateSettings`）
// ---------------------------------------------------------------------------

#[test]
fn top_level_json_arrays_deserialize_as_empty_settings() {
    // TS 读取 settings.json 时只做 `JSON.parse` + `migrateSettings`：顶层数组不会
    // 抛错（`"queueMode" in []` 为 false），getter 全部回落到默认值。
    for raw in [json!([]), json!([1, 2, 3]), json!(["theme"])] {
        let settings: Settings = serde_json::from_value(raw.clone())
            .unwrap_or_else(|error| panic!("top-level array {raw} must parse: {error}"));
        assert_eq!(settings, Settings::default());
        assert_eq!(serde_json::to_value(&settings).unwrap(), json!({}));
    }

    let input: SettingsInput = serde_json::from_value(json!([{ "theme": "dark" }]))
        .expect("SettingsInput accepts a top-level array");
    assert_eq!(input, SettingsInput::default());

    // 其余非对象顶层值仍然失败（TS 在 `in` 运算符处抛 TypeError）。
    for scalar in [json!(null), json!("str"), json!(42), json!(true)] {
        assert!(
            serde_json::from_value::<Settings>(scalar.clone()).is_err(),
            "top-level {scalar} must be rejected"
        );
    }
}

// ---------------------------------------------------------------------------
// TS: "accepts $schema in settings.json and preserves unknown settings"
// ---------------------------------------------------------------------------

#[test]
fn accepts_schema_in_settings_json_and_preserves_unknown_settings() {
    let raw = json!({
        "$schema": "https://kiana.dev/schemas/settings.schema.json",
        "theme": "light",
        "extensionSetting": { "enabled": true },
    });

    let settings: Settings = serde_json::from_value(raw.clone()).expect("settings parse");
    assert_eq!(
        settings.schema.as_deref(),
        Some("https://kiana.dev/schemas/settings.schema.json")
    );
    assert_eq!(settings.theme.as_deref(), Some("light"));
    assert_eq!(
        settings.extra.get("extensionSetting"),
        Some(&json!({ "enabled": true }))
    );

    // 未知键原样写回（键序除外）。
    let round_tripped = serde_json::to_value(&settings).expect("settings serialize");
    assert_eq!(round_tripped, raw);
}

// ---------------------------------------------------------------------------
// §2.7 SETTINGS_DEFAULTS
// ---------------------------------------------------------------------------

#[test]
fn settings_defaults_match_the_typescript_source() {
    let defaults = SETTINGS_DEFAULTS;

    // 顶层标量默认值，顺序与 settings-defaults.ts 一致。
    assert_eq!(defaults.transport, TransportSetting::Auto);
    assert_eq!(
        defaults.session_share_destination,
        SessionShareDestination::Auto
    );
    assert_eq!(defaults.steering_mode, SteeringMode::OneAtATime);
    assert_eq!(defaults.follow_up_mode, SteeringMode::OneAtATime);
    assert!(!defaults.hide_thinking_block);
    assert!(!defaults.show_cache_miss_notices);
    assert_eq!(defaults.quiet_startup, QuietStartup::Boolean(false));
    assert_eq!(defaults.default_project_trust, DefaultProjectTrust::Ask);
    assert!(!defaults.collapse_changelog);
    assert!(defaults.enable_install_telemetry);
    assert!(!defaults.enable_analytics);
    assert!(defaults.enable_skill_commands);
    assert_eq!(defaults.double_escape_action, DoubleEscapeAction::Tree);
    assert_eq!(defaults.tree_filter_mode, TreeFilterMode::Default);
    assert_eq!(defaults.editor_padding_x, 0.0);
    assert_eq!(defaults.output_pad, 1.0);
    assert_eq!(defaults.autocomplete_max_visible, 5.0);
    assert_eq!(defaults.cache_warming, schema::CacheWarmingMode::Streaming);
    assert_eq!(defaults.tui_mode, TuiMode::Fullscreen);
    assert_eq!(
        defaults.fullscreen_exit_output,
        FullscreenExitOutput::Transcript
    );
    assert_eq!(defaults.fullscreen_scrollbar, FullscreenScrollbar::Auto);
    assert!(defaults.fullscreen_copy_on_select);
    assert_eq!(
        defaults.fullscreen_wheel_scroll_lines,
        FullscreenWheelScrollLines::Auto
    );

    // 嵌套默认值。
    assert!(defaults.compaction.enabled);
    assert_eq!(defaults.compaction.reserve_tokens, 16_384.0);
    assert_eq!(defaults.compaction.keep_recent_tokens, 20_000.0);

    assert_eq!(defaults.branch_summary.reserve_tokens, 16_384.0);
    assert!(!defaults.branch_summary.skip_prompt);

    assert!(defaults.retry.enabled);
    assert_eq!(defaults.retry.max_retries, 3.0);
    assert_eq!(defaults.retry.base_delay_ms, 2_000.0);
    assert_eq!(
        defaults.retry.max_agent_delay_ms,
        DEFAULT_MAX_AGENT_RETRY_DELAY_MS as f64
    );
    assert_eq!(defaults.retry.max_agent_delay_ms, 60_000.0);
    assert_eq!(defaults.retry.provider.max_retry_delay_ms, 60_000.0);

    assert!(defaults.terminal.show_images);
    assert_eq!(defaults.terminal.image_width_cells, 60.0);
    assert!(!defaults.terminal.clear_on_shrink);
    assert!(!defaults.terminal.show_terminal_progress);

    assert!(defaults.images.auto_resize);
    assert!(!defaults.images.block_images);

    // `markdown.codeBlockIndent` 恰好两个空格。
    assert_eq!(defaults.markdown.code_block_indent, "  ");
    assert_eq!(defaults.markdown.code_block_indent.len(), 2);
    assert_eq!(defaults.markdown.mermaid, MermaidRenderingMode::Streaming);

    assert!(defaults.warnings.anthropic_extra_usage);

    assert_eq!(defaults.codemode.mode, CodemodeMode::On);
    assert_eq!(defaults.codemode.inline_budget, 3_000.0);
}

#[test]
fn settings_defaults_expand_to_a_settings_value() {
    assert_eq!(SettingsDefaults::default(), SETTINGS_DEFAULTS);

    let settings = SETTINGS_DEFAULTS.to_settings();
    let compaction = settings.compaction.as_ref().expect("compaction defaults");
    assert_eq!(compaction.enabled, Some(true));
    assert_eq!(compaction.reserve_tokens, Some(16_384.0));
    assert_eq!(compaction.keep_recent_tokens, Some(20_000.0));
    assert!(compaction.model_overrides.is_none());

    let retry = settings.retry.as_ref().expect("retry defaults");
    assert_eq!(retry.max_agent_delay_ms, Some(60_000.0));
    assert_eq!(
        retry
            .provider
            .as_ref()
            .expect("retry provider")
            .max_retry_delay_ms,
        Some(60_000.0)
    );

    let markdown = settings.markdown.as_ref().expect("markdown defaults");
    assert_eq!(markdown.code_block_indent.as_deref(), Some("  "));

    // 没有默认值的字段保持缺省。
    assert!(settings.theme.is_none());
    assert!(settings.default_provider.is_none());
    assert!(settings.packages.is_none());

    let value = serde_json::to_value(&settings).expect("defaults serialize");
    let object = value.as_object().expect("defaults are an object");
    // §2.7 的全部 31 个顶层键都存在，且没有多余键。
    let expected_keys = [
        "transport",
        "sessionShareDestination",
        "steeringMode",
        "followUpMode",
        "compaction",
        "branchSummary",
        "retry",
        "hideThinkingBlock",
        "showCacheMissNotices",
        "quietStartup",
        "defaultProjectTrust",
        "collapseChangelog",
        "enableInstallTelemetry",
        "enableAnalytics",
        "enableSkillCommands",
        "terminal",
        "images",
        "doubleEscapeAction",
        "treeFilterMode",
        "editorPaddingX",
        "outputPad",
        "autocompleteMaxVisible",
        "markdown",
        "warnings",
        "codemode",
        "cacheWarming",
        "tuiMode",
        "fullscreenExitOutput",
        "fullscreenScrollbar",
        "fullscreenCopyOnSelect",
        "fullscreenWheelScrollLines",
    ];
    assert_eq!(object.len(), expected_keys.len());
    for key in expected_keys {
        assert!(object.contains_key(key), "missing default key {key}");
    }
}

// ---------------------------------------------------------------------------
// 非负安全整数限制
// ---------------------------------------------------------------------------

#[test]
fn rejects_non_positive_settings_token_limits() {
    for invalid in [
        json!({ "compaction": { "reserveTokens": -1 } }),
        json!({ "compaction": { "reserveTokens": 1.5 } }),
        json!({ "compaction": { "keepRecentTokens": -1 } }),
        json!({ "compaction": { "modelOverrides": { "provider/model": { "keepRecentTokens": -1 } } } }),
        json!({ "compaction": { "modelOverrides": { "provider/model": { "reserveTokens": 9_007_199_254_740_992.0 } } } }),
        json!({ "compaction": { "reserveTokens": "16384" } }),
        json!({ "compaction": { "reserveTokens": true } }),
        json!({ "codemode": { "inlineBudget": -1 } }),
        json!({ "httpIdleTimeoutMs": -1 }),
        json!({ "websocketConnectTimeoutMs": -1 }),
        json!({ "httpIdleTimeoutMs": "bogus" }),
        json!({ "websocketConnectTimeoutMs": "30000" }),
    ] {
        assert!(!accepts(invalid.clone()), "should reject {invalid}");
    }

    assert!(accepts(json!({
        "compaction": {
            "reserveTokens": 0,
            "keepRecentTokens": 9_007_199_254_740_991.0,
            "modelOverrides": { "provider/model": { "reserveTokens": 0 } },
        },
        "codemode": { "inlineBudget": 0 },
        "httpIdleTimeoutMs": 0,
        "websocketConnectTimeoutMs": "disabled",
    })));
}

// ---------------------------------------------------------------------------
// 已知字段的畸形值
// ---------------------------------------------------------------------------

#[test]
fn rejects_malformed_known_settings_fields() {
    for invalid in [
        json!([]),
        json!(null),
        json!("settings"),
        json!({ "transport": "ws" }),
        json!({ "transport": "websocket_cached" }),
        json!({ "sessionShareDestination": "email" }),
        json!({ "steeringMode": 1 }),
        json!({ "followUpMode": "sometimes" }),
        json!({ "quietStartup": 5 }),
        json!({ "quietStartup": "sometimes" }),
        json!({ "defaultProjectTrust": "sometimes" }),
        json!({ "defaultThinkingLevel": "bogus" }),
        json!({ "modelThinkingLevels": { "provider/model": "bogus" } }),
        json!({ "cacheWarming": "always" }),
        json!({ "tuiMode": "other" }),
        json!({ "fullscreenExitOutput": "nothing" }),
        json!({ "fullscreenScrollbar": "sometimes" }),
        json!({ "outputPad": 2 }),
        json!({ "outputPad": 1.5 }),
        json!({ "editorPaddingX": "1" }),
        json!({ "autocompleteMaxVisible": false }),
        json!({ "npmCommand": "npm" }),
        json!({ "extensions": ["a", 1] }),
        json!({ "$schema": 42 }),
        json!({ "terminal": { "showImages": "yes" } }),
        json!({ "terminal": { "hyperlinks": 1 } }),
        json!({ "terminal": { "trueColor": 1 } }),
        json!({ "terminal": { "images": true } }),
        json!({ "terminal": { "images": "wezterm" } }),
        json!({ "terminal": { "imageWidthCells": "60" } }),
        json!({ "markdown": { "mermaid": "sometimes" } }),
        json!({ "markdown": { "codeBlockIndent": 2 } }),
        json!({ "codemode": { "mode": "off" } }),
        json!({ "packages": [{ "autoload": true }] }),
        json!({ "packages": [42] }),
        json!({ "skills": 5 }),
        json!({ "skills": [1] }),
        json!({ "thinkingBudgets": { "low": "low" } }),
        json!({ "retry": { "provider": { "timeoutMs": "5" } } }),
        json!({ "compaction": { "modelOverrides": { "provider/model": 5 } } }),
    ] {
        assert!(!accepts(invalid.clone()), "should reject {invalid}");
    }

    // 同名字段的合法值（含 union 的每个分支）。
    for valid in [
        json!({ "transport": "auto" }),
        json!({ "transport": "sse" }),
        json!({ "transport": "websocket" }),
        json!({ "transport": "websocket-cached" }),
        json!({ "quietStartup": true }),
        json!({ "quietStartup": "header" }),
        json!({ "outputPad": 0 }),
        json!({ "outputPad": 1.0 }),
        json!({ "terminal": { "hyperlinks": true } }),
        json!({ "terminal": { "hyperlinks": "auto" } }),
        json!({ "terminal": { "trueColor": false } }),
        json!({ "terminal": { "images": false } }),
        json!({ "terminal": { "images": "kitty" } }),
        json!({ "terminal": { "images": "iterm2" } }),
        json!({ "terminal": { "images": "auto" } }),
        json!({ "fullscreenWheelScrollLines": 3 }),
        json!({ "fullscreenWheelScrollLines": "auto" }),
        json!({ "skills": ["./skills"] }),
        json!({ "skills": { "enableSkillCommands": true, "customDirectories": ["./skills"] } }),
        json!({ "packages": ["npm:kiana-extension", { "source": "git:repo", "autoload": false }] }),
        json!({ "thinkingBudgets": { "minimal": 1, "low": 2, "medium": 3, "high": 4 } }),
    ] {
        assert!(accepts(valid.clone()), "should accept {valid}");
    }
}

// ---------------------------------------------------------------------------
// 7 值 ModelThinkingLevel
// ---------------------------------------------------------------------------

#[test]
fn thinking_levels_accept_all_seven_values_and_reject_unknown() {
    let levels = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
    for level in levels {
        assert!(
            accepts(json!({ "defaultThinkingLevel": level })),
            "should accept {level}"
        );
        let settings: Settings =
            serde_json::from_value(json!({ "defaultThinkingLevel": level })).expect("parse");
        assert!(settings.default_thinking_level.is_some(), "parse {level}");
    }

    // 7 个值都能从 JSON 反序列化成枚举。
    let parsed: Vec<ModelThinkingLevel> = levels
        .iter()
        .map(|level| serde_json::from_value(json!(level)).expect("thinking level"))
        .collect();
    assert_eq!(
        parsed,
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Minimal,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Xhigh,
            ModelThinkingLevel::Max,
        ]
    );

    assert!(!accepts(json!({ "defaultThinkingLevel": "bogus" })));
    assert!(serde_json::from_value::<ModelThinkingLevel>(json!("bogus")).is_err());

    assert!(accepts(
        json!({ "modelThinkingLevels": { "provider/model": "high", "other/model": "off" } })
    ));
    assert!(!accepts(
        json!({ "modelThinkingLevels": { "provider/model": "bogus" } })
    ));

    // 宽松的 Settings 解析把非法值留在 `extra`，不会让整份设置解析失败。
    let settings: Settings =
        serde_json::from_value(json!({ "defaultThinkingLevel": "bogus" })).expect("parse");
    assert!(settings.default_thinking_level.is_none());
    assert_eq!(
        settings.extra.get("defaultThinkingLevel"),
        Some(&json!("bogus"))
    );
}

// ---------------------------------------------------------------------------
// 数据模型：宽松解析与原样保留
// ---------------------------------------------------------------------------

#[test]
fn settings_preserve_invalid_and_unknown_values() {
    let raw = json!({
        "$schema": 42,
        "theme": 12,
        "tuiMode": "other",
        "unknownSetting": { "nested": [1, 2, 3] },
        "terminal": { "showImages": "yes", "futureOption": true },
        "markdown": { "mermaid": "sometimes" },
        "compaction": { "modelOverrides": { "provider/model": null } },
        "httpIdleTimeoutMs": -1.0,
    });

    let settings: Settings = serde_json::from_value(raw.clone()).expect("settings parse");
    assert!(settings.schema.is_none());
    assert!(settings.theme.is_none());
    assert!(settings.tui_mode.is_none());
    assert_eq!(
        settings.extra.get("unknownSetting"),
        Some(&json!({ "nested": [1, 2, 3] }))
    );
    assert_eq!(settings.extra.get("$schema"), Some(&json!(42)));

    let terminal = settings.terminal.as_ref().expect("terminal object");
    assert!(terminal.show_images.is_none());
    assert_eq!(terminal.extra.get("showImages"), Some(&json!("yes")));
    assert_eq!(terminal.extra.get("futureOption"), Some(&json!(true)));

    let markdown = settings.markdown.as_ref().expect("markdown object");
    assert!(markdown.mermaid.is_none());
    assert_eq!(markdown.extra.get("mermaid"), Some(&json!("sometimes")));

    // 非法 `modelOverrides` 值整段保留在 compaction 的 extra 里。
    let compaction = settings.compaction.as_ref().expect("compaction object");
    assert!(compaction.model_overrides.is_none());
    assert_eq!(
        compaction.extra.get("modelOverrides"),
        Some(&json!({ "provider/model": null }))
    );

    // 负数超时被类型化保留，getter 才能用 `String(value)` 报错。
    assert_eq!(
        settings.http_idle_timeout_ms,
        Some(TimeoutSetting::Millis(-1.0))
    );

    // 原样写回：未知键、非法值与嵌套非法值都不丢。
    let round_tripped = serde_json::to_value(&settings).expect("settings serialize");
    assert_eq!(round_tripped, raw);
}

// ---------------------------------------------------------------------------
// ui.locale（TUI 界面语言）
// ---------------------------------------------------------------------------

#[test]
fn ui_settings_parse_serialize_and_validate() {
    let raw = json!({
        "ui": { "locale": "zh-CN", "futureUiOption": true },
    });
    let settings: Settings = serde_json::from_value(raw.clone()).expect("settings parse");
    let ui = settings.ui.as_ref().expect("ui object");
    assert_eq!(ui.locale.as_deref(), Some("zh-CN"));
    assert_eq!(ui.extra.get("futureUiOption"), Some(&json!(true)));
    assert_eq!(serde_json::to_value(&settings).expect("serialize"), raw);

    // `SettingsInput` 与运行期视图共享同一形状。
    let input: SettingsInput = serde_json::from_value(raw.clone()).expect("input parse");
    assert_eq!(
        input.ui.as_ref().and_then(|ui| ui.locale.as_deref()),
        Some("zh-CN")
    );

    assert!(accepts(json!({ "ui": {} })));
    assert!(accepts(raw));
    assert!(!accepts(json!({ "ui": "en" })));
    assert!(!accepts(json!({ "ui": [] })));
    assert!(!accepts(json!({ "ui": { "locale": 5 } })));

    // 非对象的 `ui` 原样保留在顶层 `extra`，不影响整份设置解析。
    let broken: Settings =
        serde_json::from_value(json!({ "ui": "zh-CN" })).expect("settings parse");
    assert!(broken.ui.is_none());
    assert_eq!(broken.extra.get("ui"), Some(&json!("zh-CN")));
}

#[test]
fn typed_field_assignment_overrides_stale_invalid_value() {
    let settings: Settings =
        serde_json::from_value(json!({ "tuiMode": "other" })).expect("settings parse");
    assert!(settings.tui_mode.is_none());
    assert_eq!(settings.extra.get("tuiMode"), Some(&json!("other")));

    let mut settings = settings;
    settings.tui_mode = Some(TuiMode::Regular);
    let value = serde_json::to_value(&settings).expect("settings serialize");
    assert_eq!(value, json!({ "tuiMode": "regular" }));
}

#[test]
fn settings_accept_every_literal_and_union_branch() {
    // 枚举的 serde 表示是当前稳定契约，改动需同步测试。
    assert_eq!(
        serde_json::to_value(TransportSetting::WebsocketCached).unwrap(),
        json!("websocket-cached")
    );
    assert_eq!(
        serde_json::to_value(SessionShareDestination::Gist).unwrap(),
        json!("gist")
    );
    assert_eq!(
        serde_json::to_value(SteeringMode::OneAtATime).unwrap(),
        json!("one-at-a-time")
    );
    assert_eq!(
        serde_json::to_value(DefaultProjectTrust::Never).unwrap(),
        json!("never")
    );
    assert_eq!(
        serde_json::to_value(DoubleEscapeAction::None).unwrap(),
        json!("none")
    );
    assert_eq!(
        serde_json::to_value(TreeFilterMode::NoTools).unwrap(),
        json!("no-tools")
    );
    assert_eq!(
        serde_json::to_value(TreeFilterMode::UserOnly).unwrap(),
        json!("user-only")
    );
    assert_eq!(
        serde_json::to_value(TreeFilterMode::LabeledOnly).unwrap(),
        json!("labeled-only")
    );
    assert_eq!(
        serde_json::to_value(MermaidRenderingMode::Final).unwrap(),
        json!("final")
    );
    assert_eq!(
        serde_json::to_value(CodemodeMode::Only).unwrap(),
        json!("only")
    );
    assert_eq!(
        serde_json::to_value(TuiMode::Regular).unwrap(),
        json!("regular")
    );
    assert_eq!(
        serde_json::to_value(FullscreenExitOutput::ResumeHint).unwrap(),
        json!("resume-hint")
    );
    assert_eq!(
        serde_json::to_value(FullscreenScrollbar::Hidden).unwrap(),
        json!("hidden")
    );
    assert_eq!(
        serde_json::to_value(FullscreenWheelScrollLines::Auto).unwrap(),
        json!("auto")
    );
    assert_eq!(
        serde_json::to_value(FullscreenWheelScrollLines::Lines(3.0)).unwrap(),
        json!(3.0)
    );
    assert_eq!(
        serde_json::to_value(TimeoutSetting::Disabled).unwrap(),
        json!("disabled")
    );
    assert_eq!(
        serde_json::to_value(TimeoutSetting::Millis(0.0)).unwrap(),
        json!(0.0)
    );
    assert_eq!(
        serde_json::to_value(QuietStartup::Header).unwrap(),
        json!("header")
    );
    assert_eq!(
        serde_json::to_value(QuietStartup::Boolean(true)).unwrap(),
        json!(true)
    );
    assert_eq!(
        serde_json::to_value(TerminalImagesSetting::Disabled).unwrap(),
        json!(false)
    );
}

#[test]
fn settings_input_mirrors_the_60_top_level_properties() {
    // 顺序即 `SettingsSchema.properties` 的键序（TS 侧 `Object.keys(SettingsSchema.properties)`）。
    let expected = [
        "$schema",
        "lastChangelogVersion",
        "defaultProvider",
        "defaultModel",
        "defaultThinkingLevel",
        "modelThinkingLevels",
        "transport",
        "sessionShareDestination",
        "steeringMode",
        "followUpMode",
        "theme",
        "compaction",
        "branchSummary",
        "retry",
        "hideThinkingBlock",
        "showCacheMissNotices",
        "externalEditor",
        "shellPath",
        "quietStartup",
        "defaultProjectTrust",
        "shellCommandPrefix",
        "npmCommand",
        "collapseChangelog",
        "enableInstallTelemetry",
        "enableAnalytics",
        "trackingId",
        "packages",
        "extensions",
        "deviceId",
        "skills",
        "prompts",
        "themes",
        "enableSkillCommands",
        "terminal",
        "images",
        "ui",
        "enabledModels",
        "defaultTools",
        "doubleEscapeAction",
        "treeFilterMode",
        "thinkingBudgets",
        "editorPaddingX",
        "outputPad",
        "autocompleteMaxVisible",
        "showHardwareCursor",
        "markdown",
        "warnings",
        "codemode",
        "sessionDir",
        "httpProxy",
        "httpIdleTimeoutMs",
        "cacheWarming",
        "websocketConnectTimeoutMs",
        "tuiMode",
        "fullscreenExitOutput",
        "fullscreenScrollbar",
        "fullscreenCopyOnSelect",
        "fullscreenWheelScrollLines",
        "queueMode",
        "websockets",
    ];
    assert_eq!(SettingsInput::known_keys(), expected);
    assert_eq!(SettingsInput::known_keys().len(), 60);

    // 运行期视图去掉两个旧字段，其余顺序不变。
    let expected_runtime: Vec<&str> = expected
        .iter()
        .copied()
        .filter(|key| *key != "queueMode" && *key != "websockets")
        .collect();
    assert_eq!(Settings::known_keys(), expected_runtime);
    assert_eq!(Settings::known_keys().len(), 58);

    // `SettingsInput` 接受 input-only 的字段形状。
    let input: SettingsInput = serde_json::from_value(json!({
        "queueMode": "all",
        "websockets": true,
        "skills": { "enableSkillCommands": true, "customDirectories": ["./skills"] },
        "retry": { "maxDelayMs": 1000 },
    }))
    .expect("settings input parse");
    assert_eq!(input.queue_mode, Some(SteeringMode::All));
    assert_eq!(input.websockets, Some(true));
    assert!(input.skills.is_some());
    assert_eq!(
        input.retry.as_ref().and_then(|retry| retry.max_delay_ms),
        Some(1000.0)
    );
}

#[test]
fn compaction_settings_expose_typed_overrides() {
    let compaction: CompactionSettings = serde_json::from_value(json!({
        "enabled": false,
        "reserveTokens": 8192,
        "keepRecentTokens": 10000,
        "modelOverrides": {
            "provider/family/model": { "reserveTokens": 400000 },
            "provider/other": { "keepRecentTokens": 4096 },
        },
    }))
    .expect("compaction parse");

    assert_eq!(compaction.enabled, Some(false));
    assert_eq!(compaction.reserve_tokens, Some(8192.0));
    let overrides = compaction.model_overrides.expect("overrides");
    assert_eq!(
        overrides
            .get("provider/family/model")
            .and_then(|entry| entry.reserve_tokens),
        Some(400_000.0)
    );
    assert_eq!(
        overrides
            .get("provider/other")
            .and_then(|entry| entry.keep_recent_tokens),
        Some(4096.0)
    );
}

#[test]
fn compaction_raw_accessors_expose_invalid_values_for_errors() {
    let compaction: CompactionSettings = serde_json::from_value(json!({
        "reserveTokens": 1.5,
        "modelOverrides": {
            "provider/model": null,
            "provider/valid": { "reserveTokens": 400000, "keepRecentTokens": "x" },
        },
    }))
    .expect("compaction parse");

    assert_eq!(
        compaction.raw_token_setting("reserveTokens"),
        Some(json!(1.5))
    );
    assert_eq!(compaction.raw_token_setting("keepRecentTokens"), None);

    // 非对象条目保留原值，对象条目保留每个字段的原值。
    assert_eq!(
        compaction.raw_model_override("provider/model"),
        Some(Value::Null)
    );
    let valid = compaction
        .raw_model_override("provider/valid")
        .expect("object entry");
    assert_eq!(
        valid.get("reserveTokens").and_then(Value::as_f64),
        Some(400_000.0)
    );
    assert_eq!(valid.get("keepRecentTokens"), Some(&json!("x")));
    assert_eq!(compaction.raw_model_override("missing/model"), None);

    // 整个 modelOverrides 解析失败时仍可从 raw 读取。
    let broken: CompactionSettings = serde_json::from_value(json!({
        "modelOverrides": { "provider/model": null },
    }))
    .expect("compaction parse");
    assert!(broken.model_overrides.is_none());
    assert_eq!(
        broken.raw_model_override("provider/model"),
        Some(Value::Null)
    );
}

#[test]
fn validator_reports_the_failing_path() {
    let error = validate_settings_schema(&json!({
        "compaction": { "modelOverrides": { "provider/model": { "reserveTokens": -1 } } }
    }))
    .expect_err("must fail");
    assert_eq!(
        error.path,
        "compaction.modelOverrides[\"provider/model\"].reserveTokens"
    );
    assert!(
        error
            .to_string()
            .starts_with("compaction.modelOverrides[\"provider/model\"]")
    );
}

#[test]
fn raw_accessor_returns_typed_or_preserved_values() {
    let settings: Settings = serde_json::from_value(json!({
        "theme": "light",
        "editorPaddingX": 2.5,
        "tuiMode": "other",
        "extensions": ["a", 1],
    }))
    .expect("settings parse");

    assert_eq!(settings.raw("theme"), Some(json!("light")));
    assert_eq!(settings.raw("editorPaddingX"), Some(json!(2.5)));
    assert_eq!(settings.raw("tuiMode"), Some(json!("other")));
    assert_eq!(settings.raw("extensions"), Some(json!(["a", 1])));
    assert_eq!(settings.raw("missing"), None);
}
