//! `DynamicBorder` 的测试。

mod common;

use kiana_theme::ThemeColor;
use kiana_tui::Component;
use kiana_ui_kit::DynamicBorder;

/// 所有断言放在一个测试里：主题是进程级全局状态，同二进制内的测试并行执行，避免与
/// `init_theme` 切换互相干扰。
#[test]
fn dynamic_border_renders_and_follows_theme() {
    common::setup();

    // 默认颜色 = 全局主题的 border token（每次 render 时惰性读取）。
    let mut border = DynamicBorder::default();
    let dark_line = border.render(10).remove(0);
    assert_eq!(dark_line, common::fg(ThemeColor::Border, "──────────"));
    assert_eq!(dark_line.chars().filter(|c| *c == '─').count(), 10);

    // 宽度 0 → 恰好 1 列（TS `Math.max(1, width)`）。
    let lines = border.render(0);
    assert_eq!(lines.len(), 1);
    assert_eq!(
        kiana_tui::strip_terminal_sequences(&lines[0]),
        "─".to_string(),
        "width 0 clamps to one column"
    );

    // 主题切换后无需重建（默认闭包读全局主题）。
    kiana_theme::init_theme(Some("light"), false);
    let light_line = border.render(10).remove(0);
    assert_eq!(light_line, common::fg(ThemeColor::Border, "──────────"));
    kiana_theme::init_theme(Some("dark"), false);

    // 显式颜色函数。
    let mut custom = DynamicBorder::from_fn(|text| format!("<{text}>"));
    assert_eq!(custom.render(4), vec!["<────>".to_string()]);
    assert_eq!(custom.render(0), vec!["<─>".to_string()]);

    // invalidate 是空实现（状态不变）。
    border.invalidate();
    assert_eq!(border.render(10).remove(0), dark_line);

    // TS `DynamicBorder` 没有 handleInput。
    assert!(!border.handles_input());

    // 宽度不变式（width 0 是唯一允许溢出的情况，见组件文档）。
    for width in common::WIDTHS {
        let mut border = DynamicBorder::default();
        let line = border.render(width).remove(0);
        assert_eq!(kiana_tui::visible_width(&line), width);
    }
}
