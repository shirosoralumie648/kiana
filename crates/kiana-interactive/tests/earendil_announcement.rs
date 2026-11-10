//! `components/earendil-announcement.ts` 的黄金渲染测试。
//!
//! 黄金数据由 `/home/ctyun/kiana-work/scratch/anim/gen.ts` 导入真实 TS 组件生成
//! （无图片能力时的文本兜底，固定 truecolor）。

use std::sync::OnceLock;

use kiana_interactive::assets::{CLANKOLAS_PNG, clankolas_png_base64, load_clankolas_base64};
use kiana_interactive::components::earendil_announcement::{
    BLOG_URL, EarendilAnnouncementComponent, IMAGE_FILENAME,
};
use kiana_theme::init_theme;
use kiana_tui::terminal_image::{TerminalCapabilities, set_capabilities};
use kiana_tui::tui::Component;
use serde_json::Value;

fn theme() {
    static INIT: OnceLock<()> = OnceLock::new();
    set_capabilities(TerminalCapabilities {
        images: None,
        true_color: true,
        hyperlinks: false,
    });
    INIT.get_or_init(|| init_theme(Some("dark"), false));
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/earendil.json")).expect("earendil fixture parses")
}

#[test]
fn constants_are_verbatim() {
    assert_eq!(
        BLOG_URL,
        "https://mariozechner.at/posts/2026-04-08-ive-sold-out/"
    );
    assert_eq!(IMAGE_FILENAME, "clankolas.png");
    assert_eq!(BLOG_URL, fixture()["blogUrl"].as_str().expect("url"));
    assert_eq!(
        IMAGE_FILENAME,
        fixture()["imageFilename"].as_str().expect("file")
    );
}

#[test]
fn bundled_image_is_available() {
    assert_eq!(
        CLANKOLAS_PNG.len(),
        539_053,
        "clankolas.png must be byte-identical to the TS repository asset"
    );
    assert!(load_clankolas_base64().is_some());
    assert_eq!(
        clankolas_png_base64().len(),
        718_740,
        "standard base64 of a 539053-byte file"
    );
    assert!(clankolas_png_base64().starts_with("iVBORw0KGgo"));
}

#[test]
fn renders_the_announcement_exactly() {
    theme();
    let fixture = fixture();
    let mut component = EarendilAnnouncementComponent::new();
    assert!(component.has_image());
    // DynamicBorder + Text + Spacer + Text + Text + Spacer + Image + Spacer + DynamicBorder
    assert_eq!(component.children_len(), 9);
    for entry in fixture["widths"].as_array().expect("widths") {
        let width = entry["width"].as_u64().expect("width") as usize;
        let expected: Vec<String> = entry["lines"]
            .as_array()
            .expect("lines")
            .iter()
            .map(|line| line.as_str().expect("line").to_string())
            .collect();
        assert_eq!(component.render(width), expected, "render({width})");
    }
    assert_eq!(component.children_len(), 9, "render is side-effect free");
}

#[test]
fn component_order_and_text_are_verbatim() {
    theme();
    let mut component = EarendilAnnouncementComponent::new();
    let lines = component.render(80);
    let plain: Vec<String> = lines
        .iter()
        .map(|line| kiana_tui::text::strip_terminal_sequences(line))
        .collect();
    assert_eq!(plain.len(), 9);
    assert_eq!(plain[0], "─".repeat(80));
    assert_eq!(plain[1].trim_end(), " kiana has joined Earendil");
    assert_eq!(plain[2], "");
    assert_eq!(plain[3].trim_end(), " Read the blog post:");
    assert_eq!(plain[4].trim_end(), format!(" {BLOG_URL}"));
    assert_eq!(plain[5], "");
    assert_eq!(plain[6], "[Image: clankolas.png [image/png] 640x537]");
    assert_eq!(plain[7], "");
    assert_eq!(plain[8], "─".repeat(80));
    // 标题是 bold(accent)：外层 \x1b[1m ... \x1b[22m。
    assert!(lines[1].contains("\x1b[1m"));
    assert!(lines[1].contains("\x1b[22m"));
}
