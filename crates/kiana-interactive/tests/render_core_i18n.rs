//! i18n 测试：JSON/TOML 数据包、用户目录 > 内置 > fallback、缺失 key 回退不 panic、
//! strict parity + MUST_TRANSLATE_KEYS CI、多行数组、UI locale 与 output language 分离。

use std::fs;

use kiana_interactive::render_core::i18n::{
    LanguageSettings, LocaleLoader, LocalePack, LocaleSource, MUST_TRANSLATE_KEYS,
    TranslationValue, Translator, builtin_pack, check_parity, locale_ci, parse_locale_json,
    parse_locale_toml, resolve_supported_locale,
};
use tempfile::tempdir;

fn english() -> LocalePack {
    builtin_pack("en").expect("builtin en")
}

fn chinese() -> LocalePack {
    builtin_pack("zh").expect("builtin zh")
}

#[test]
fn builtin_packs_are_valid_and_strict_zh_passes_parity() {
    let en = english();
    let zh = chinese();
    assert!(zh.strict);
    assert!(locale_ci(&[zh.clone()], &en).is_ok());
    for key in MUST_TRANSLATE_KEYS {
        assert!(en.strings.contains_key(*key), "en missing {key}");
        assert!(zh.strings.contains_key(*key), "zh missing {key}");
    }
}

#[test]
fn missing_key_falls_back_to_english_then_key_without_panicking() {
    let mut zh = chinese();
    zh.strings
        .remove("pending.retry")
        .expect("key exists in builtin");
    let translator = Translator::new(zh, english());
    assert_eq!(translator.t("pending.retry", &[]), "Retry");
    assert_eq!(translator.t("does.not.exist", &[]), "does.not.exist");
    assert!(translator.ta("does.not.exist").is_empty());
}

#[test]
fn interpolation_and_multi_line_arrays() {
    let translator = Translator::new(chinese(), english());
    assert_eq!(
        translator.t("modal.approval_position", &[("index", "2"), ("total", "3")]),
        "审批 2/3"
    );
    let lines = translator.ta("i18n.hint.lines");
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[1], "按 Enter 确认。");
    // 未翻译时多行数组回退英文。
    let mut zh = chinese();
    zh.strings.remove("i18n.hint.lines");
    let translator = Translator::new(zh, english());
    assert_eq!(
        translator.ta("i18n.hint.lines")[0],
        "Use arrow keys to move."
    );
}

#[test]
fn strict_parity_failure_lists_missing_and_untranslated_keys() {
    let mut zh = chinese();
    zh.strings.remove("approval.reject");
    let report = check_parity(&zh, &english());
    assert_eq!(report.missing, vec!["approval.reject"]);
    let error = locale_ci(&[zh], &english()).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("approval.reject"), "{text}");

    let mut zh = chinese();
    zh.strings.insert(
        "approval.allow_once".to_string(),
        TranslationValue::Text("Allow once".to_string()),
    );
    let report = check_parity(&zh, &english());
    assert_eq!(report.untranslated_must, vec!["approval.allow_once"]);
    let error = locale_ci(&[zh], &english()).unwrap_err();
    assert!(error.to_string().contains("approval.allow_once"));
}

#[test]
fn non_strict_locale_is_not_blocked_by_parity() {
    let mut ja = LocalePack::empty("ja");
    ja.strings.insert(
        "approval.title".to_string(),
        TranslationValue::Text("許可".to_string()),
    );
    ja.strict = false;
    assert!(locale_ci(&[ja], &english()).is_ok());
}

#[test]
fn user_locale_overrides_builtin_and_broken_user_file_is_skipped() {
    let root = tempdir().unwrap();
    let builtin_dir = root.path().join("builtin");
    let user_dir = root.path().join("user");
    fs::create_dir_all(&builtin_dir).unwrap();
    fs::create_dir_all(&user_dir).unwrap();
    fs::write(
        builtin_dir.join("zh.json"),
        r#"{"locale":"zh","strings":{"approval.title":"内置标题","custom.builtin":"内置值"}}"#,
    )
    .unwrap();
    fs::write(
        user_dir.join("zh.json"),
        r#"{"locale":"zh","strings":{"approval.title":"用户标题","custom.user":"用户值"}}"#,
    )
    .unwrap();
    let loader = LocaleLoader::new(&user_dir, &builtin_dir);
    let loaded = loader.load("zh");
    assert_eq!(loaded.source, LocaleSource::User);
    assert_eq!(
        loaded.pack.get("approval.title"),
        Some(&TranslationValue::Text("用户标题".to_string()))
    );
    assert_eq!(
        loaded.pack.get("custom.builtin"),
        Some(&TranslationValue::Text("内置值".to_string()))
    );
    // 用户包未覆盖的内置资源 key 仍然可用。
    assert_eq!(
        loaded.pack.get("pending.retry"),
        Some(&TranslationValue::Text("重试".to_string()))
    );

    fs::write(user_dir.join("zh.json"), "{not json").unwrap();
    let loaded = loader.load("zh");
    assert_eq!(loaded.source, LocaleSource::Builtin);
    assert!(!loaded.diagnostics.is_empty());
    assert_eq!(
        loaded.pack.get("approval.title"),
        Some(&TranslationValue::Text("内置标题".to_string()))
    );
}

#[test]
fn toml_locale_pack_is_data_only_and_supports_arrays() {
    let toml = r#"
# comment
locale = "zh"
strict = true
[strings]
approval.title = "TOML 标题"
"multi.line" = ["第一行", "第二行"]
danger = "require('fs')"
"#;
    let pack = parse_locale_toml(toml).unwrap();
    assert_eq!(pack.locale, "zh");
    assert!(pack.strict);
    assert_eq!(
        pack.get("approval.title"),
        Some(&TranslationValue::Text("TOML 标题".to_string()))
    );
    assert_eq!(
        pack.get("multi.line"),
        Some(&TranslationValue::Lines(vec![
            "第一行".to_string(),
            "第二行".to_string()
        ]))
    );
    // 只是数据：不执行、不报错。
    assert_eq!(
        pack.get("danger"),
        Some(&TranslationValue::Text("require('fs')".to_string()))
    );
}

#[test]
fn loader_prefers_toml_when_json_is_absent() {
    let root = tempdir().unwrap();
    let user_dir = root.path().join("user");
    let builtin_dir = root.path().join("builtin");
    fs::create_dir_all(&user_dir).unwrap();
    fs::create_dir_all(&builtin_dir).unwrap();
    fs::write(
        user_dir.join("ja.toml"),
        "locale = \"ja\"\n[strings]\napproval.title = \"許可\"\n",
    )
    .unwrap();
    let loader = LocaleLoader::new(&user_dir, &builtin_dir);
    let loaded = loader.load("ja");
    assert_eq!(loaded.source, LocaleSource::User);
    assert_eq!(
        loaded.pack.get("approval.title"),
        Some(&TranslationValue::Text("許可".to_string()))
    );
}

#[test]
fn json_flat_format_is_accepted() {
    let pack = parse_locale_json(r#"{"locale":"de","hello":"Hallo"}"#).unwrap();
    assert_eq!(pack.locale, "de");
    assert_eq!(
        pack.get("hello"),
        Some(&TranslationValue::Text("Hallo".to_string()))
    );
}

#[test]
fn locale_detection_and_ui_output_language_are_separate() {
    assert_eq!(resolve_supported_locale("zh_CN.UTF-8"), Some("zh"));
    assert_eq!(resolve_supported_locale("zh-TW"), Some("zh-TW"));
    assert_eq!(resolve_supported_locale("en_US"), Some("en"));
    assert_eq!(resolve_supported_locale("xx_YY"), None);

    let settings = LanguageSettings::new("zh", Some("en".to_string()));
    assert_eq!(settings.ui_locale, "zh");
    assert!(settings.output_rule().is_none(), "英文回答不需要额外规则");

    let settings = LanguageSettings::new("en", Some("zh".to_string()));
    let rule = settings.output_rule().expect("zh rule");
    assert!(rule.contains("Chinese"));
    assert!(rule.contains("中文"));
    assert_eq!(settings.ui_locale, "en", "output language 不改变 UI locale");

    let settings = LanguageSettings::new("zh", Some("ja".to_string()));
    assert!(settings.output_rule().unwrap().contains("Japanese"));
}

#[test]
fn loader_never_panics_for_unknown_locale() {
    let root = tempdir().unwrap();
    let loader = LocaleLoader::new(root.path().join("user"), root.path().join("builtin"));
    let loaded = loader.load("xx");
    assert_eq!(loaded.pack.locale, "xx");
    let translator = loader.load_translator("xx");
    // 未知 locale：回退英文内置包。
    assert_eq!(translator.t("approval.reject", &[]), "Reject");
}
