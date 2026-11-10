//! `utils/syntax-highlight.ts` 的 TS 对拍回归测试。
//!
//! Fixture 由 TypeScript 参考实现（highlight.js 10.7.3 + `renderHighlightedHtml`）生成。
//! `languages` 是 20 种急切语言在真实代码片段上的逐字节输出（theme 覆盖全部 hljs
//! scope 名，scope 用 `[scope]text[/scope]` 标记）；`render` 是
//! `renderHighlightedHtml` 的 HTML→文本用例。
//!
//! 生成方式（在 TS 参考仓库执行）：
//!
//! ```text
//! node gen_syntax_fixture.mjs syntax_diff.json   # 见交付说明
//! ```
//!
//! 已知偏差（不在本 fixture 里，见 `src/syntax_highlight.rs` 的模块文档）：swift 的
//! `func` 参数列表在 hljs 的 `FUNCTION` 模式内，本实现按普通文本输出；`highlightAuto`
//! 的 relevance 评分是简化版，自动检测不在对拍范围内（`kiana-theme` 始终显式传语言）。

use std::sync::Arc;

use kiana_core::syntax_highlight::{
    HighlightFormatter, HighlightOptions, HighlightTheme, highlight, render_highlighted_html,
};

/// highlight.js 10.7.3 的全部 scope 名（CSS class 去掉 `hljs-` 前缀）。
const GATE_SCOPES: &[&str] = &[
    "keyword",
    "built_in",
    "type",
    "literal",
    "number",
    "regexp",
    "string",
    "subst",
    "symbol",
    "class",
    "function",
    "title",
    "params",
    "comment",
    "doctag",
    "meta",
    "meta-keyword",
    "meta-string",
    "section",
    "tag",
    "name",
    "attr",
    "attribute",
    "variable",
    "bullet",
    "code",
    "emphasis",
    "strong",
    "formula",
    "link",
    "quote",
    "selector-tag",
    "selector-id",
    "selector-class",
    "selector-attr",
    "selector-pseudo",
    "template-tag",
    "template-variable",
    "addition",
    "deletion",
    "default",
];

fn gate_theme() -> HighlightTheme {
    let mk = |scope: &'static str| -> HighlightFormatter {
        Arc::new(move |t: &str| format!("[{scope}]{t}[/{scope}]"))
    };
    GATE_SCOPES
        .iter()
        .map(|s| ((*s).to_string(), mk(s)))
        .collect()
}

fn read_fixture() -> serde_json::Value {
    let text = include_str!("fixtures/syntax_port_corpus.json");
    serde_json::from_str(text).expect("valid fixture json")
}

fn highlight_with_theme(code: &str, language: &str) -> String {
    highlight(
        code,
        &HighlightOptions {
            language: Some(language.to_string()),
            ignore_illegals: true,
            language_subset: None,
            theme: gate_theme(),
        },
    )
}

fn compact_diff(expected: &str, actual: &str) -> String {
    let e: Vec<char> = expected.chars().collect();
    let a: Vec<char> = actual.chars().collect();
    let mut i = 0;
    while i < e.len() && i < a.len() && e[i] == a[i] {
        i += 1;
    }
    let start = i.saturating_sub(40);
    let es: String = e[start..(i + 60).min(e.len())].iter().collect();
    let as_: String = a[start..(i + 60).min(a.len())].iter().collect();
    format!("first diff at char {i}\n  exp[..]: {es:?}\n  act[..]: {as_:?}")
}

#[test]
fn language_outputs_match_the_typescript_reference() {
    let fixture = read_fixture();
    let mut mismatches = Vec::new();
    for (language, entries) in fixture["languages"].as_object().unwrap() {
        for entry in entries.as_array().unwrap() {
            let code = entry["code"].as_str().unwrap();
            let expected = entry["output"].as_str().unwrap();
            let actual = highlight_with_theme(code, language);
            if actual != expected {
                mismatches.push(format!(
                    "language={language} code={code:?}\n{}",
                    compact_diff(expected, &actual)
                ));
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} mismatches:\n{}",
        mismatches.len(),
        mismatches.join("\n---\n")
    );
}

#[test]
fn render_output_matches_the_typescript_reference() {
    let fixture = read_fixture();
    let theme = gate_theme();
    for entry in fixture["render"].as_array().unwrap() {
        let html = entry["html"].as_str().unwrap();
        let expected = entry["output"].as_str().unwrap();
        let actual = render_highlighted_html(html, &theme);
        assert_eq!(actual, expected, "html={html:?}");
    }
}

/// 回归：gate 评审列出的 hljs span 结构差异（普通代码里的 function/class/method/
/// constructor/getter-setter、python class 冒号、kotlin 泛型与 supertype、c `_t`
/// 类型、csharp 泛型返回类型、ruby kwarg symbol、php `{$x}` subst、perl regexp、
/// groovy attr、dart `${}` subst、go 下划线数字）。
/// Fixture 由 TypeScript 参考实现（highlight.js 10.7.3）生成。
#[test]
fn gate_regressions_match_the_typescript_reference() {
    let text = include_str!("fixtures/syntax_regressions.json");
    let fixture: serde_json::Value = serde_json::from_str(text).expect("valid regression fixture");
    for entry in fixture["cases"].as_array().unwrap() {
        let language = entry["language"].as_str().unwrap();
        let code = entry["code"].as_str().unwrap();
        let expected = entry["output"].as_str().unwrap();
        let actual = highlight_with_theme(code, language);
        assert_eq!(
            actual,
            expected,
            "language={language} code={code:?}\n{}",
            compact_diff(expected, &actual)
        );
    }
}
