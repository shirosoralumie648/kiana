//! `components/text.ts` 的空文本判定用 JS `String.prototype.trim`
//! （含 U+FEFF、不含 U+0085），与 Rust `str::trim` 的集合不同。

use kiana_tui::Component;
use kiana_tui::components::text_component::Text;

/// `\u{0085}`（NEL）不在 JS 空白集合里：仍渲染一行并补齐宽度。
#[test]
fn next_line_character_is_not_js_whitespace() {
    let mut text = Text::with_padding("\u{0085}", 0, 0);
    assert_eq!(text.render(5), vec![format!("\u{0085}{}", " ".repeat(5))]);
}

/// U+FEFF 在 JS 空白集合里：整段被 trim 成空串，不渲染任何行。
#[test]
fn bom_is_js_whitespace() {
    let mut text = Text::with_padding("\u{FEFF}", 0, 0);
    assert!(text.render(5).is_empty());
}
