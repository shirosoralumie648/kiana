//! 移植 `packages/coding-agent/test/export-html-whitespace.test.ts`。
//!
//! 对应 `core/export-html/template.css`、`ansi-to-html.ts` 与 `tool-renderer.ts`。
//! describe/it 标题保持逐字（见测试函数上的注释）。

use std::sync::Arc;

use kiana_ai::types::JsonValue;
use kiana_export::ansi_to_html::ansi_lines_to_html;
use kiana_export::assets::TEMPLATE_CSS;
use kiana_export::tool_renderer::{
    ToolHtmlRenderer, ToolHtmlRendererDeps, create_tool_html_renderer,
};
use kiana_ext_api::AgentToolResult;
use kiana_ext_api::tool_types::{ToolRenderContext, ToolRenderResultOptions, ToolRenderers};
use kiana_theme::theme::Theme;
use kiana_tui::tui::Component;
use regex::Regex;

struct FixedComponent(Vec<String>);

impl Component for FixedComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.0.clone()
    }
}

// it("preserves whitespace for plain-text tool output lines without preserving template
//     whitespace")
#[test]
fn preserves_whitespace_for_plain_text_tool_output_lines_without_preserving_template_whitespace() {
    let css = TEMPLATE_CSS;

    assert!(
        Regex::new(
            r"\.output-preview > div:not\(\.expand-hint\),\s*\.output-full > div:not\(\.expand-hint\) \{[\s\S]*?white-space:\s*pre-wrap;"
        )
        .unwrap()
        .is_match(css),
        "expected per-line pre-wrap rule"
    );
    assert!(
        Regex::new(r"\.ansi-line\s*\{[\s\S]*?white-space:\s*pre;")
            .unwrap()
            .is_match(css),
        "expected .ansi-line pre"
    );
    assert!(
        !Regex::new(r"\.output-preview,\s*\.output-full\s*\{[\s\S]*?white-space:\s*pre-wrap;")
            .unwrap()
            .is_match(css),
        "must not apply pre-wrap to the whole output block"
    );
}

// it("does not insert source whitespace between ANSI-rendered lines")
#[test]
fn does_not_insert_source_whitespace_between_ansi_rendered_lines() {
    assert_eq!(
        ansi_lines_to_html(&["one".to_string(), "two".to_string()]),
        "<div class=\"ansi-line\">one</div><div class=\"ansi-line\">two</div>"
    );
}

// it("trims TUI spacing lines from custom tool result HTML")
#[test]
fn trims_tui_spacing_lines_from_custom_tool_result_html() {
    let render_result = Arc::new(
        |_result: &AgentToolResult<JsonValue>,
         _options: &ToolRenderResultOptions,
         _theme: &Theme,
         _context: &ToolRenderContext| {
            Box::new(FixedComponent(vec![
                String::new(),
                "\u{1b}[31mone\u{1b}[0m".to_string(),
                "two".to_string(),
                String::new(),
            ])) as Box<dyn Component>
        },
    );
    let mut renderer = create_tool_html_renderer(ToolHtmlRendererDeps {
        get_tool_renderers: Arc::new(move |name: &str| {
            if name != "custom" {
                return None;
            }
            Some(ToolRenderers {
                render_shell: None,
                render_call: None,
                render_result: Some(render_result.clone()),
            })
        }),
        theme: kiana_theme::get_theme_by_name("dark").expect("dark theme"),
        cwd: "/tmp".to_string(),
        width: None,
    });

    let rendered = renderer
        .render_result("id", "custom", &[], &JsonValue::Null, false)
        .expect("rendered");
    assert_eq!(
        rendered.expanded,
        "<div class=\"ansi-line\"><span style=\"color:#800000\">one</span></div><div class=\"ansi-line\">two</div>"
    );
}
