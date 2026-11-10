//! `Intl` 与 `toLocale*`（设计 §5.8、§6.4）。
//!
//! 本地化文本只在 ICU4X 与 Node 输出**已实测逐字一致**时才比较（§6.4 的清单，避免 ICU 数据差异造成假失败）；
//! ja/ko/de/da/es/pl/tr/nl 的零填充/分隔符差异记录在 §7.4（见
//! [`KNOWN_LOCALE_DIFFERENCES`]），不比字节。

mod support;

use kiana_extensions::loader::js_host;
use support::Harness;

fn intl_harness() -> Harness {
    let harness = Harness::new();
    js_host::install_intl(&harness.engine).expect("install intl");
    harness
}

// ---------------------------------------------------------------------------
// locale 解析（LC_ALL > LANG > en-US；LANGUAGE 忽略；只取 ll_CC 前缀）
// ---------------------------------------------------------------------------

#[test]
fn locale_resolution_prefers_lc_all_then_lang_and_ignores_language() {
    assert_eq!(
        kiana_extensions::loader::js_host::resolve_locale_for_test(
            Some("de_DE.UTF-8"),
            Some("ja_JP.UTF-8")
        ),
        "de-DE"
    );
    assert_eq!(
        kiana_extensions::loader::js_host::resolve_locale_for_test(None, Some("ja_JP.UTF-8")),
        "ja-JP"
    );
    assert_eq!(
        kiana_extensions::loader::js_host::resolve_locale_for_test(None, None),
        "en-US"
    );
    assert_eq!(
        kiana_extensions::loader::js_host::resolve_locale_for_test(Some(""), Some("en_GB.UTF-8")),
        "en-GB"
    );
    // 非法值回退
    assert_eq!(
        kiana_extensions::loader::js_host::resolve_locale_for_test(Some("not a locale"), None),
        "en-US"
    );
    assert_eq!(
        kiana_extensions::loader::js_host::resolve_locale_for_test(Some("C"), None),
        "en-US"
    );
    assert_eq!(
        kiana_extensions::loader::js_host::resolve_locale_for_test(Some("POSIX"), None),
        "en-US"
    );
    // ll-CC 与 ll 都接受；修饰符 / 编码被剥离
    assert_eq!(
        kiana_extensions::loader::js_host::resolve_locale_for_test(Some("pt-BR"), None),
        "pt-BR"
    );
    assert_eq!(
        kiana_extensions::loader::js_host::resolve_locale_for_test(Some("de_DE@euro"), None),
        "de-DE"
    );
    assert_eq!(
        kiana_extensions::loader::js_host::resolve_locale_for_test(Some("zh_CN.UTF-8"), None),
        "zh-CN"
    );
}

#[test]
fn language_variable_is_not_consulted() {
    // `LANGUAGE` 只影响 gettext，Node 的 Intl 不读它；Rust 的解析函数也没有这个参数。
    assert_eq!(
        kiana_extensions::loader::js_host::resolve_locale_for_test(None, Some("en_US")),
        "en-US"
    );
}

// ---------------------------------------------------------------------------
// Segmenter
// ---------------------------------------------------------------------------

#[test]
fn segmenter_grapheme_offsets_are_utf16() {
    let harness = intl_harness();
    assert_eq!(
        harness.eval::<String>(
            r#"JSON.stringify([...new Intl.Segmenter("en", {granularity:"grapheme"}).segment("a👍🏽b")])"#
        ),
        r#"[{"segment":"a","index":0,"input":"a👍🏽b"},{"segment":"👍🏽","index":1,"input":"a👍🏽b"},{"segment":"b","index":5,"input":"a👍🏽b"}]"#
    );
}

#[test]
fn segmenter_word_reports_is_word_like() {
    let harness = intl_harness();
    assert_eq!(
        harness.eval::<String>(
            r#"JSON.stringify([...new Intl.Segmenter("en", {granularity:"word"}).segment("Hello, world!")])"#
        ),
        r#"[{"segment":"Hello","index":0,"input":"Hello, world!","isWordLike":true},{"segment":",","index":5,"input":"Hello, world!","isWordLike":false},{"segment":" ","index":6,"input":"Hello, world!","isWordLike":false},{"segment":"world","index":7,"input":"Hello, world!","isWordLike":true},{"segment":"!","index":12,"input":"Hello, world!","isWordLike":false}]"#
    );
}

#[test]
fn segmenter_word_uses_the_cjk_dictionary() {
    let harness = intl_harness();
    // Node/ICU word granularity uses the CLDR dictionary for scripts without
    // spaces; per-code-point splitting would be wrong here.
    let cases = [
        (
            "zh",
            "我爱北京天安门",
            r#"[["我",0,true],["爱",1,true],["北京",2,true],["天安门",4,true]]"#,
        ),
        (
            "zh",
            "中文分词测试",
            r#"[["中文",0,true],["分词",2,true],["测试",4,true]]"#,
        ),
        (
            "ja",
            "こんにちは世界",
            r#"[["こんにちは",0,true],["世界",5,true]]"#,
        ),
        (
            "ko",
            "안녕하세요 세계",
            r#"[["안녕하세요",0,true],[" ",5,false],["세계",6,true]]"#,
        ),
    ];
    for (locale, text, expected) in cases {
        assert_eq!(
            harness.eval::<String>(&format!(
                r#"JSON.stringify([...new Intl.Segmenter({locale}, {{granularity:"word"}}).segment({text})].map(s => [s.segment, s.index, s.isWordLike]))"#,
                locale = serde_json::to_string(locale).expect("locale json"),
                text = serde_json::to_string(text).expect("text json"),
            )),
            expected,
            "word segmentation for {locale} {text}"
        );
    }

    // KNOWN DIFFERENCE (§7.4 item 1): ICU4X's Japanese dictionary is not the
    // exact ICU4C one Node ships. Node gives い|ます here, ICU4X gives いま|す;
    // both are dictionary words, and neither splits per code point. The
    // first four segments agree with Node, which is what the shim relies on.
    assert_eq!(
        harness.eval::<String>(
            r#"JSON.stringify([...new Intl.Segmenter("ja", {granularity:"word"}).segment("東京都に住んでいます")].map(s => [s.segment, s.index, s.isWordLike]))"#
        ),
        r#"[["東京",0,true],["都",2,true],["に",3,true],["住",4,true],["んで",5,true],["いま",7,true],["す",9,true]]"#
    );
}

#[test]
fn segmenter_sentence_and_resolved_options() {
    let harness = intl_harness();
    assert_eq!(
        harness.eval::<String>(
            r#"JSON.stringify([...new Intl.Segmenter("en", {granularity:"sentence"}).segment("One. Two!")].map(s => [s.segment, s.index]))"#
        ),
        r#"[["One. ",0],["Two!",5]]"#
    );
    assert_eq!(
        harness.eval::<String>(
            r#"JSON.stringify(new Intl.Segmenter("de-DE", {granularity:"word"}).resolvedOptions())"#
        ),
        r#"{"locale":"de-DE","granularity":"word"}"#
    );
}

// ---------------------------------------------------------------------------
// NumberFormat / Number#toLocaleString
// ---------------------------------------------------------------------------

#[test]
fn number_format_matches_node_for_known_locales() {
    let harness = intl_harness();
    assert_eq!(
        harness.eval::<String>("(1234567.5).toLocaleString('en-US')"),
        "1,234,567.5"
    );
    assert_eq!(
        harness.eval::<String>("(1234567.5).toLocaleString('de-DE')"),
        "1.234.567,5"
    );
    assert_eq!(
        harness.eval::<String>("(1234567.5).toLocaleString('ru-RU')"),
        "1\u{a0}234\u{a0}567,5"
    );
    assert_eq!(
        harness.eval::<String>("(1234567.5).toLocaleString('hi-IN')"),
        "12,34,567.5"
    );
    assert_eq!(
        harness.eval::<String>("(0.1 + 0.2).toLocaleString('en-US')"),
        "0.3"
    );
    assert_eq!(
        harness.eval::<String>("(1234567.5).toLocaleString('en-US', {useGrouping: false})"),
        "1234567.5"
    );
    assert_eq!(
        harness.eval::<String>("(0.5).toLocaleString('en-US', {style: 'percent'})"),
        "50%"
    );
    assert_eq!(
        harness.eval::<String>("(2.5).toLocaleString('en-US', {maximumFractionDigits: 0})"),
        "3"
    );
    assert_eq!(
        harness.eval::<String>("(1.25).toLocaleString('en-US', {minimumFractionDigits: 3})"),
        "1.250"
    );
    assert_eq!(
        harness.eval::<String>("new Intl.NumberFormat('en-US').resolvedOptions().locale"),
        "en-US"
    );
}

#[test]
fn number_format_handles_negative_and_large_values() {
    let harness = intl_harness();
    assert_eq!(
        harness.eval::<String>("(-1234.5).toLocaleString('en-US')"),
        "-1,234.5"
    );
    assert_eq!(harness.eval::<String>("(0).toLocaleString('en-US')"), "0");
    assert_eq!(
        harness.eval::<String>("(1e21).toLocaleString('en-US')"),
        "1,000,000,000,000,000,000,000"
    );
}

// ---------------------------------------------------------------------------
// DateTimeFormat / Date#toLocale*
// ---------------------------------------------------------------------------

#[test]
fn date_time_format_matches_node_for_known_locales() {
    let harness = intl_harness();
    let cases = [
        ("en-US", "1/2/2024, 3:04:05 AM", "1/2/2024", "3:04:05 AM"),
        ("en-GB", "02/01/2024, 03:04:05", "02/01/2024", "03:04:05"),
        ("zh-CN", "2024/1/2 03:04:05", "2024/1/2", "03:04:05"),
        ("fr-FR", "02/01/2024 03:04:05", "02/01/2024", "03:04:05"),
        ("ru-RU", "02.01.2024, 03:04:05", "02.01.2024", "03:04:05"),
        ("sv-SE", "2024-01-02 03:04:05", "2024-01-02", "03:04:05"),
        ("hi-IN", "2/1/2024, 3:04:05 am", "2/1/2024", "3:04:05 am"),
    ];
    for (locale, both, date, time) in cases {
        let source = format!(
            r#"(function () {{
                var d = new Date(2024, 0, 2, 3, 4, 5);
                return [d.toLocaleString("{locale}"), d.toLocaleDateString("{locale}"), d.toLocaleTimeString("{locale}")].join("|");
            }})()"#
        );
        assert_eq!(
            harness.eval::<String>(&source),
            format!("{both}|{date}|{time}"),
            "locale {locale}"
        );
    }
}

#[test]
fn to_locale_overrides_are_writable_and_configurable() {
    let harness = intl_harness();
    assert!(harness.eval::<bool>(
        r#"(function () {
            var descriptor = Object.getOwnPropertyDescriptor(Date.prototype, "toLocaleString");
            var numberDescriptor = Object.getOwnPropertyDescriptor(Number.prototype, "toLocaleString");
            return descriptor.writable === true && descriptor.configurable === true &&
              descriptor.enumerable === false &&
              numberDescriptor.writable === true && numberDescriptor.configurable === true;
        })()"#
    ));
    // 覆盖后的方法由本单元安装（ja-JP 的零填充差异见 §7.4，不比较本地化字节）。
    assert!(
        !harness
            .eval::<String>("new Date(2024, 0, 2).toLocaleDateString('ja-JP')")
            .is_empty()
    );
}

#[test]
fn invalid_dates_render_as_invalid_date() {
    let harness = intl_harness();
    assert_eq!(
        harness.eval::<String>("new Date(NaN).toLocaleString('en-US')"),
        "Invalid Date"
    );
}

// ---------------------------------------------------------------------------
// Unsupported formatters
// ---------------------------------------------------------------------------

#[test]
fn collator_plural_rules_and_relative_time_format_are_unsupported() {
    let harness = intl_harness();
    let error = harness.expect_error("new Intl.Collator('en')");
    assert_eq!(error.name, "RangeError");
    assert_eq!(error.message, "Intl.Collator is not supported");
    let error = harness.expect_error("new Intl.PluralRules('en')");
    assert_eq!(error.name, "RangeError");
    assert_eq!(error.message, "Intl.PluralRules is not supported");
    let error = harness.expect_error("new Intl.RelativeTimeFormat('en')");
    assert_eq!(error.name, "RangeError");
    assert_eq!(error.message, "Intl.RelativeTimeFormat is not supported");
    // localeCompare 回退到 JS 的码元序（§6.4）。
    assert_eq!(harness.eval::<i32>("'a'.localeCompare('b')"), -1);
}

#[test]
fn canonical_locales_and_supported_locales_of() {
    let harness = intl_harness();
    // Node rejects structural tags (`de_DE.UTF-8`) instead of normalizing them.
    assert_eq!(
        harness.eval::<String>("JSON.stringify(Intl.getCanonicalLocales(['en-us', 'de-de']))"),
        r#"["en-US","de-DE"]"#
    );
    let error = harness.expect_error("Intl.getCanonicalLocales(['de_DE.UTF-8'])");
    assert_eq!(error.name, "RangeError");
    assert_eq!(error.message, "Invalid language tag: de_DE.UTF-8");
    let error = harness.expect_error("Intl.getCanonicalLocales([42])");
    assert_eq!(error.name, "TypeError");
    assert_eq!(error.message, "Language ID should be string or object.");
    assert_eq!(
        harness.eval::<String>("JSON.stringify(Intl.NumberFormat.supportedLocalesOf(['fr-FR']))"),
        r#"["fr-FR"]"#
    );
    let error = harness.expect_error("Intl.NumberFormat.supportedLocalesOf(['de-DE.UTF-8'])");
    assert_eq!(error.name, "RangeError");
}

// ---------------------------------------------------------------------------
// 20 locale 夹具（design §6.4；Node 26.12.1 实测，tests/fixtures/intl/locales.json）
// ---------------------------------------------------------------------------

/// 这些 locale 的 ICU4X 输出与 Node 的 ICU 输出逐字相同，可用于逐字断言（§6.4 清单，20 个 locale 夹具核对）。
const VERBATIM_LOCALES: [&str; 12] = [
    "en-US", "en-GB", "fr-FR", "ru-RU", "sv-SE", "fi-FI", "hi-IN", "th-TH", "ar-EG", "zh-CN",
    "it-IT", "pt-BR",
];

/// 已知的 ICU4X 与 Node/ICU 数据差异（§7.4 诚实清单）。
///
/// 差异只出现在日期文本（数字区域零填充与分隔符），数字格式 20 个 locale 全部一致：
/// - ja-JP: `2024/1/2` vs `2024/01/02`（ICU4X 补零）
/// - ko-KR: `2024. 1. 2.` vs `2024/1/2`（分隔符）
/// - de-DE / da-DK: `2.1.2024` vs `02.01.2024`（ICU4X 补零）
/// - es-ES: `3:04:05` vs `03:04:05`（时间补零）
/// - pl-PL: `2.01.2024` vs `02.01.2024`（日补零）
/// - tr-TR: `02.01.2024` vs `2.01.2024`（Node 补零，ICU4X 不补）
/// - nl-NL: `2-1-2024` vs `02-01-2024`（ICU4X 补零）
const KNOWN_LOCALE_DIFFERENCES: [&str; 8] = [
    "ja-JP", "ko-KR", "de-DE", "da-DK", "es-ES", "pl-PL", "tr-TR", "nl-NL",
];

#[test]
fn twenty_locale_golden_fixtures() {
    let harness = intl_harness();
    let fixtures: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("fixtures/intl/locales.json")).expect("locale fixtures");
    assert_eq!(fixtures.len(), 20);
    for fixture in fixtures {
        let locale = fixture["locale"].as_str().expect("locale");
        let source = format!(
            r#"(function () {{
                var d = new Date(2024, 0, 2, 3, 4, 5);
                return JSON.stringify([
                  d.toLocaleString("{locale}"),
                  d.toLocaleDateString("{locale}"),
                  d.toLocaleTimeString("{locale}"),
                  (1234567.5).toLocaleString("{locale}"),
                ]);
            }})()"#
        );
        let actual: Vec<String> =
            serde_json::from_str(&harness.eval::<String>(&source)).expect("json");
        assert_eq!(
            actual[3],
            fixture["number"].as_str().expect("number"),
            "number formatting for {locale}"
        );
        if VERBATIM_LOCALES.contains(&locale) {
            assert_eq!(
                actual[0],
                fixture["dateTime"].as_str().expect("dateTime"),
                "DateTimeFormat for {locale}"
            );
            assert_eq!(
                actual[1],
                fixture["date"].as_str().expect("date"),
                "toLocaleDateString for {locale}"
            );
            assert_eq!(
                actual[2],
                fixture["time"].as_str().expect("time"),
                "toLocaleTimeString for {locale}"
            );
        } else {
            // ja/ko/de/da/es/pl/tr/nl 的零填充/分隔符差异见 §7.4；这些 locale 只保证
            // 非空，且必须出现在文档化的差异清单里（防止新的静默偏差）。
            assert!(
                KNOWN_LOCALE_DIFFERENCES.contains(&locale),
                "{locale} differs from Node but is not documented in §7.4"
            );
            assert!(
                !actual[0].is_empty() && !actual[1].is_empty() && !actual[2].is_empty(),
                "{locale} produced empty date output"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 评审回归：DateTimeFormat 的默认值、选项与 resolvedOptions
// ---------------------------------------------------------------------------

fn format_case(harness: &Harness, locales: &str, options: &str) -> String {
    let source = format!(
        r#"(function () {{ var d = new Date(2024, 0, 2, 3, 4, 5); return new Intl.DateTimeFormat({locales}, {options}).format(d); }})()"#
    );
    harness.eval::<String>(&source)
}

#[test]
fn date_time_format_honors_options_and_defaults_like_node() {
    let harness = intl_harness();
    let cases = [
        ("'en-US'", "undefined", "1/2/2024"),
        ("'en-US'", "{}", "1/2/2024"),
        ("'en-US'", "{hour12: false}", "1/2/2024"),
        (
            "'en-US'",
            "{year: 'numeric', month: 'long'}",
            "January 2024",
        ),
        (
            "'en-US'",
            "{year: 'numeric', month: 'long', day: '2-digit'}",
            "January 02, 2024",
        ),
        (
            "'en-US'",
            "{hour: 'numeric', minute: 'numeric', second: 'numeric'}",
            "3:04:05 AM",
        ),
        (
            "'en-US'",
            "{hour: 'numeric', minute: 'numeric', second: 'numeric', hour12: false}",
            "03:04:05",
        ),
        ("'en-US'", "{dateStyle: 'short'}", "1/2/24"),
        ("'en-US'", "{dateStyle: 'medium'}", "Jan 2, 2024"),
        ("'en-US'", "{dateStyle: 'long'}", "January 2, 2024"),
        ("'en-US'", "{dateStyle: 'full'}", "Tuesday, January 2, 2024"),
        ("'en-US'", "{timeStyle: 'short'}", "3:04 AM"),
        ("'en-US'", "{timeStyle: 'medium'}", "3:04:05 AM"),
        ("'en-US'", "{weekday: 'long'}", "Tuesday"),
        ("'en-US'", "{month: 'long'}", "January"),
        (
            "'en-US'",
            "{year: '2-digit', month: '2-digit', day: '2-digit'}",
            "01/02/24",
        ),
        (
            "'en-US'",
            "{year: '2-digit', month: 'numeric', day: 'numeric'}",
            "1/2/24",
        ),
        ("'en-US'", "{month: 'short', day: 'numeric'}", "Jan 2"),
        (
            "'de-DE'",
            "{year: 'numeric', month: 'long', day: '2-digit'}",
            "02. Januar 2024",
        ),
        ("'en-US'", "{timeZone: 'UTC'}", "1/1/2024"),
    ];
    for (locales, options, expected) in cases {
        assert_eq!(
            format_case(&harness, locales, options),
            expected,
            "DateTimeFormat({locales}, {options})"
        );
    }
}

#[test]
fn to_locale_methods_use_node_defaults_and_overrides() {
    let harness = intl_harness();
    assert_eq!(
        harness.eval::<String>("new Date(2024,0,2,3,4,5).toLocaleString('en-US')"),
        "1/2/2024, 3:04:05 AM"
    );
    assert_eq!(
        harness.eval::<String>("new Date(2024,0,2,3,4,5).toLocaleDateString('en-US')"),
        "1/2/2024"
    );
    assert_eq!(
        harness.eval::<String>("new Date(2024,0,2,3,4,5).toLocaleTimeString('en-US')"),
        "3:04:05 AM"
    );
    assert_eq!(
        harness.eval::<String>(
            "new Date(2024,0,2,3,4,5).toLocaleDateString('en-US', {year: 'numeric', month: 'long', day: '2-digit'})"
        ),
        "January 02, 2024"
    );
    assert_eq!(
        harness.eval::<String>(
            "new Date(2024,0,2,3,4,5).toLocaleTimeString('en-US', {hour12: false})"
        ),
        "03:04:05"
    );
    // A component option replaces the method default set entirely.
    assert_eq!(
        harness
            .eval::<String>("new Date(2024,0,2,3,4,5).toLocaleString('en-US', {year: '2-digit'})"),
        "24"
    );
    assert_eq!(
        harness.eval::<String>(
            "new Date(2024,0,2,3,4,5).toLocaleDateString('en-US', {month: 'long'})"
        ),
        "January"
    );
}

#[test]
fn date_time_resolved_options_reflect_fields_and_system_time_zone() {
    let harness = intl_harness();
    let value: serde_json::Value = serde_json::from_str(
        &harness
            .eval::<String>("JSON.stringify(new Intl.DateTimeFormat('en-US').resolvedOptions())"),
    )
    .expect("json");
    assert_eq!(value["locale"], "en-US");
    assert_eq!(value["calendar"], "gregory");
    assert_eq!(value["numberingSystem"], "latn");
    assert_eq!(value["year"], "numeric");
    assert_eq!(value["month"], "numeric");
    assert_eq!(value["day"], "numeric");
    // The host time zone, not a hard-coded "UTC".
    let expected_zone = std::env::var("TZ")
        .ok()
        .filter(|zone| !zone.trim().is_empty())
        .unwrap_or_else(|| {
            std::fs::read_link("/etc/localtime")
                .ok()
                .and_then(|path| {
                    let text = path.to_string_lossy().into_owned();
                    text.find("zoneinfo/")
                        .map(|index| text[index + "zoneinfo/".len()..].to_owned())
                })
                .unwrap_or_else(|| "UTC".to_owned())
        });
    assert_eq!(value["timeZone"], expected_zone);

    let hour: serde_json::Value = serde_json::from_str(
        &harness.eval::<String>(
            "JSON.stringify(new Intl.DateTimeFormat('en-US', {hour: 'numeric', minute: 'numeric', hour12: false}).resolvedOptions())",
        ),
    )
    .expect("json");
    assert_eq!(hour["hourCycle"], "h23");
    assert_eq!(hour["hour12"], false);
    assert_eq!(hour["hour"], "2-digit");
    assert_eq!(hour["minute"], "2-digit");
}

// ---------------------------------------------------------------------------
// 评审回归：NumberFormat 的 style / formatToParts / resolvedOptions
// ---------------------------------------------------------------------------

#[test]
fn number_format_styles_units_percent_and_parts() {
    let harness = intl_harness();
    let cases = [
        (
            "(1234.5).toLocaleString('en-US', {style: 'currency', currency: 'USD'})",
            "$1,234.50",
        ),
        (
            "(1234.5).toLocaleString('en-US', {style: 'currency', currency: 'EUR'})",
            "\u{20ac}1,234.50",
        ),
        (
            "(1234.5).toLocaleString('de-DE', {style: 'currency', currency: 'EUR'})",
            "1.234,50\u{a0}\u{20ac}",
        ),
        (
            "(1234.5).toLocaleString('en-US', {style: 'currency', currency: 'USD', currencyDisplay: 'code'})",
            "USD\u{a0}1,234.50",
        ),
        (
            "(1234.5).toLocaleString('en-US', {style: 'currency', currency: 'USD', currencyDisplay: 'name'})",
            "1,234.50 US dollars",
        ),
        (
            "(5).toLocaleString('en-US', {style: 'unit', unit: 'kilometer'})",
            "5 km",
        ),
        (
            "(5).toLocaleString('en-US', {style: 'unit', unit: 'kilometer', unitDisplay: 'long'})",
            "5 kilometers",
        ),
        (
            "(5).toLocaleString('en-US', {style: 'unit', unit: 'kilometer-per-hour'})",
            "5 km/h",
        ),
        ("(0.5).toLocaleString('tr-TR', {style: 'percent'})", "%50"),
        ("(0.5).toLocaleString('en-US', {style: 'percent'})", "50%"),
        (
            "(1234).toLocaleString('en-US', {notation: 'compact'})",
            "1.2K",
        ),
        (
            "(1234.5).toLocaleString('en-US', {minimumFractionDigits: 3})",
            "1,234.500",
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(harness.eval::<String>(source), expected, "{source}");
    }
    assert_eq!(
        harness.eval::<String>(
            "JSON.stringify(new Intl.NumberFormat('en-US', {style: 'currency', currency: 'USD'}).formatToParts(1234.5))"
        ),
        r#"[{"type":"currency","value":"$"},{"type":"integer","value":"1"},{"type":"group","value":","},{"type":"integer","value":"234"},{"type":"decimal","value":"."},{"type":"fraction","value":"50"}]"#
    );
    let error = harness.expect_error("new Intl.NumberFormat('en-US', {useGrouping: 'min-2'})");
    assert_eq!(error.name, "RangeError");
    assert_eq!(
        error.message,
        "Value min-2 out of range for Intl.NumberFormat options property useGrouping"
    );
}

#[test]
fn number_format_resolved_options_include_rounding_fields() {
    let harness = intl_harness();
    let value: serde_json::Value = serde_json::from_str(
        &harness.eval::<String>("JSON.stringify(new Intl.NumberFormat('en-US').resolvedOptions())"),
    )
    .expect("json");
    assert_eq!(value["style"], "decimal");
    assert_eq!(value["minimumIntegerDigits"], 1);
    assert_eq!(value["minimumFractionDigits"], 0);
    assert_eq!(value["maximumFractionDigits"], 3);
    assert_eq!(value["useGrouping"], "auto");
    assert_eq!(value["notation"], "standard");
    assert_eq!(value["signDisplay"], "auto");
    assert_eq!(value["roundingIncrement"], 1);
    assert_eq!(value["roundingMode"], "halfExpand");
    assert_eq!(value["roundingPriority"], "auto");
    assert_eq!(value["trailingZeroDisplay"], "auto");

    let currency: serde_json::Value = serde_json::from_str(
        &harness.eval::<String>(
            "JSON.stringify(new Intl.NumberFormat('en-US', {style: 'currency', currency: 'USD'}).resolvedOptions())",
        ),
    )
    .expect("json");
    assert_eq!(currency["currency"], "USD");
    assert_eq!(currency["currencyDisplay"], "symbol");
    assert_eq!(currency["currencySign"], "standard");
    assert_eq!(currency["minimumFractionDigits"], 2);
    assert_eq!(currency["maximumFractionDigits"], 2);
}
