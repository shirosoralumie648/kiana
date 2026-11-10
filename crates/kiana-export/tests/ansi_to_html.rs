//! `ansiToHtml` / `ansiLinesToHtml` 的逐字节移植测试。
//!
//! TS 源没有独立测试文件；这里的向量由 Node 26 直接运行
//! `core/export-html/ansi-to-html.ts` 生成（`tests/fixtures/ansi_vectors.json`），
//! 覆盖调色板/亮色、256 色、RGB、参数不足、空参数、相邻 span、转义与 `&nbsp;`。

use serde::Deserialize;

use kiana_export::ansi_to_html::{ansi_lines_to_html, ansi_to_html};

#[derive(Debug, Deserialize)]
struct Vectors {
    ansi: Vec<AnsiCase>,
    lines: Vec<LinesCase>,
}

#[derive(Debug, Deserialize)]
struct AnsiCase {
    name: String,
    input: String,
    html: String,
}

#[derive(Debug, Deserialize)]
struct LinesCase {
    name: String,
    input: Vec<String>,
    html: String,
}

fn load() -> Vectors {
    let raw = include_str!("fixtures/ansi_vectors.json");
    serde_json::from_str(raw).expect("ansi vectors parse")
}

#[test]
fn ansi_to_html_matches_ts_vectors() {
    let vectors = load();
    assert!(!vectors.ansi.is_empty());
    for case in &vectors.ansi {
        assert_eq!(
            ansi_to_html(&case.input),
            case.html,
            "case {} input {:?}",
            case.name,
            case.input
        );
    }
}

#[test]
fn ansi_lines_to_html_matches_ts_vectors() {
    let vectors = load();
    assert!(!vectors.lines.is_empty());
    for case in &vectors.lines {
        assert_eq!(
            ansi_lines_to_html(&case.input),
            case.html,
            "case {} input {:?}",
            case.name,
            case.input
        );
    }
}

#[test]
fn verified_individual_vectors() {
    // 规范 §8.4 明确列出的验收向量。
    assert_eq!(
        ansi_to_html("\x1b[31m\x1b[32mX\x1b[0m"),
        "<span style=\"color:#800000\"></span><span style=\"color:#008000\">X</span>"
    );
    assert_eq!(
        ansi_to_html("\x1b[38;5;0;48;5;255mX"),
        "<span style=\"color:#000000;background-color:#eeeeee\">X</span>"
    );
    assert_eq!(
        ansi_to_html("\x1b[38;5;999mX"),
        "<span style=\"color:#1dfe1dfe1dfe\">X</span>"
    );
    assert_eq!(
        ansi_lines_to_html(&["one".to_string(), String::new()]),
        "<div class=\"ansi-line\">one</div><div class=\"ansi-line\">&nbsp;</div>"
    );
    // 行间没有源码空白。
    assert_eq!(
        ansi_lines_to_html(&["one".to_string(), "two".to_string()]),
        "<div class=\"ansi-line\">one</div><div class=\"ansi-line\">two</div>"
    );
}
