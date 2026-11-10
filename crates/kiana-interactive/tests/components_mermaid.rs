//! Mermaid transformer 的 TS 对拍测试。
//!
//! 移植自 `packages/coding-agent/test/mermaid.test.ts`。

mod common;

use std::sync::Arc;

use kiana_config::settings::MermaidRenderingMode;
use kiana_ext_api::types::{MarkdownMessageType, MarkdownTransformContext};
use kiana_theme::theme_tokens::ThemeColor;

use kiana_interactive::components::mermaid::{
    DiagramKind, GlobalMermaidTheme, MermaidTheme, MermaidTransformerOptions, diagram_kind,
    render_mermaid_plain,
};

struct MockTheme;

impl MermaidTheme for MockTheme {
    fn fg(&self, color: ThemeColor, text: &str) -> String {
        format!("<{color}>{text}</{color}>")
    }

    fn bold(&self, text: &str) -> String {
        format!("<bold>{text}</bold>")
    }
}

struct Options {
    max_width: usize,
    is_streaming: bool,
    message_type: MarkdownMessageType,
    mode: MermaidRenderingMode,
    themed: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            max_width: 100,
            is_streaming: false,
            message_type: MarkdownMessageType::Assistant,
            mode: MermaidRenderingMode::Streaming,
            themed: false,
        }
    }
}

fn transform_mermaid(markdown: &str, options: Options) -> String {
    let transformer = create_transformer(options.mode, options.themed);
    transformer(
        markdown,
        &MarkdownTransformContext {
            message_type: options.message_type,
            is_streaming: options.is_streaming,
            available_width: options.max_width,
            message_id: None,
            timestamp: None,
        },
    )
}

fn create_transformer(
    mode: MermaidRenderingMode,
    themed: bool,
) -> kiana_ext_api::types::MarkdownTransformer {
    let theme: Option<Arc<dyn MermaidTheme>> = if themed {
        Some(Arc::new(MockTheme))
    } else {
        None
    };
    kiana_interactive::components::mermaid::create_mermaid_markdown_transformer(
        MermaidTransformerOptions {
            get_mode: Box::new(move || mode),
            theme,
        },
    )
}

fn golden(key: &str) -> String {
    let raw = include_str!("fixtures/render_goldens_dark.json");
    let value: serde_json::Value = serde_json::from_str(raw).expect("golden json");
    value["mermaid"][key]
        .as_str()
        .unwrap_or_else(|| panic!("missing mermaid golden {key}"))
        .to_string()
}

#[test]
fn replaces_mermaid_code_blocks_with_unicode_diagrams() {
    let markdown = "Before\n\n```mermaid\nflowchart LR\n  A[Start] --> B[Done]\n```\nAfter";
    let rendered = transform_mermaid(markdown, Options::default());
    assert!(rendered.contains("Before"));
    assert!(rendered.contains("┌───────┐"));
    assert!(rendered.contains("│ Start ├───▶│ Done │"));
    assert!(rendered.contains("└───────┘    └──────┘`\nAfter"));
    assert!(!rendered.contains("```mermaid"));
    assert!(rendered.contains("After"));
    assert_eq!(rendered, golden("simple"));
}

#[test]
fn leaves_unsupported_and_oversized_diagrams_unchanged() {
    let unsupported = "```mermaid\npie\n  title Pets\n  \"Dogs\" : 4\n```";
    let oversized = "```mermaid\nflowchart LR\n  A[Start] --> B[Done]\n```";
    assert_eq!(
        transform_mermaid(unsupported, Options::default()),
        unsupported
    );
    assert_eq!(
        transform_mermaid(
            oversized,
            Options {
                max_width: 10,
                ..Options::default()
            }
        ),
        oversized
    );
    assert_eq!(
        transform_mermaid(unsupported, Options::default()),
        golden("unsupported")
    );
    assert_eq!(
        transform_mermaid(
            oversized,
            Options {
                max_width: 10,
                ..Options::default()
            }
        ),
        golden("oversized")
    );
}

#[test]
fn maps_semantic_spans_through_the_kiana_theme() {
    let rendered = transform_mermaid(
        "```mermaid\nflowchart LR\n  A --> B\n```",
        Options {
            themed: true,
            ..Options::default()
        },
    );
    assert!(rendered.contains("<borderMuted>"));
    assert!(rendered.contains("<accent>"));
    assert_eq!(rendered, golden("themed"));
}

#[test]
fn renders_incomplete_mermaid_blocks_during_streaming() {
    let partial_markdown = "```mermaid\nflowchart LR\n  A --> B";
    let rendered = transform_mermaid(
        partial_markdown,
        Options {
            is_streaming: true,
            ..Options::default()
        },
    );
    assert!(rendered.contains("───▶"));
    assert_eq!(rendered, golden("streaming"));
}

#[test]
fn renders_diagrams_with_class_assignments() {
    let markdown = "```mermaid\nflowchart LR\n  A[Foo]:::highlight --> B[Bar]\n```";
    let rendered = transform_mermaid(markdown, Options::default());
    assert!(!rendered.contains("```mermaid"));
    assert!(!rendered.contains("Mermaid diagram not rendered"));
    assert!(rendered.contains("│ Foo ├───▶│ Bar │"));
    assert_eq!(rendered, golden("classed"));
}

#[test]
fn falls_back_to_the_code_block_with_a_warning_after_streaming() {
    let markdown = "```mermaid\nflowchart LR\n  A[Foo] invalid\n```";
    let final_render = transform_mermaid(markdown, Options::default());
    let followed_by_text =
        transform_mermaid(&format!("{markdown}\nFollowing text"), Options::default());
    let streaming = transform_mermaid(
        markdown,
        Options {
            is_streaming: true,
            ..Options::default()
        },
    );

    assert!(final_render.contains(markdown));
    assert!(final_render.contains("```\n`Mermaid diagram not rendered"));
    assert!(final_render.contains("dropped, expected a link: \"invalid\""));
    assert!(!final_render.contains("more)"));
    assert!(followed_by_text.contains("  \nFollowing text"));
    assert!(!streaming.contains("Mermaid diagram not rendered"));
    assert!(!streaming.contains("```mermaid"));
    assert!(streaming.contains("│ Foo │"));
    assert_eq!(final_render, golden("invalid"));
    assert_eq!(followed_by_text, golden("invalid_followed"));
    assert_eq!(streaming, golden("invalid_streaming"));
}

#[test]
fn summarizes_additional_partial_render_warnings() {
    let markdown = "```mermaid\nflowchart LR\n  A[Foo] invalid\n  B[Bar] also-invalid\n```";
    let rendered = transform_mermaid(markdown, Options::default());
    assert!(rendered.contains(markdown));
    assert!(rendered.contains("dropped, expected a link: \"invalid\""));
    assert!(rendered.contains("(+1 more)"));
    assert!(!rendered.contains("dropped, expected a link: \"also-invalid\""));
    assert_eq!(rendered, golden("two_invalid"));
}

#[test]
fn respects_rendering_modes_and_skips_thinking_blocks() {
    let markdown = "```mermaid\nflowchart LR\n  A --> B\n```";
    assert_eq!(
        transform_mermaid(
            markdown,
            Options {
                mode: MermaidRenderingMode::Off,
                ..Options::default()
            }
        ),
        markdown
    );
    assert_eq!(
        transform_mermaid(
            markdown,
            Options {
                mode: MermaidRenderingMode::Final,
                is_streaming: true,
                ..Options::default()
            }
        ),
        markdown
    );
    assert!(
        !transform_mermaid(
            markdown,
            Options {
                mode: MermaidRenderingMode::Final,
                ..Options::default()
            }
        )
        .contains("```mermaid")
    );
    assert_eq!(
        transform_mermaid(
            markdown,
            Options {
                message_type: MarkdownMessageType::AssistantThinking,
                ..Options::default()
            }
        ),
        markdown
    );
    assert_eq!(
        transform_mermaid(
            markdown,
            Options {
                mode: MermaidRenderingMode::Off,
                ..Options::default()
            }
        ),
        golden("mode_off")
    );
    assert_eq!(
        transform_mermaid(
            markdown,
            Options {
                mode: MermaidRenderingMode::Final,
                is_streaming: true,
                ..Options::default()
            }
        ),
        golden("mode_final_streaming")
    );
    assert_eq!(
        transform_mermaid(
            markdown,
            Options {
                mode: MermaidRenderingMode::Final,
                ..Options::default()
            }
        ),
        golden("mode_final")
    );
    assert_eq!(
        transform_mermaid(
            markdown,
            Options {
                message_type: MarkdownMessageType::AssistantThinking,
                ..Options::default()
            }
        ),
        golden("thinking")
    );
}

#[test]
fn diagram_kind_reports_all_supported_headers() {
    assert_eq!(
        diagram_kind("flowchart LR\n A --> B"),
        Some(DiagramKind::Flowchart)
    );
    assert_eq!(
        diagram_kind("graph TD\n A --> B"),
        Some(DiagramKind::Flowchart)
    );
    assert_eq!(
        diagram_kind("stateDiagram-v2\n A --> B"),
        Some(DiagramKind::State)
    );
    assert_eq!(
        diagram_kind("classDiagram\n A <|-- B"),
        Some(DiagramKind::Class)
    );
    assert_eq!(
        diagram_kind("erDiagram\n A ||--o{ B : has"),
        Some(DiagramKind::Er)
    );
    assert_eq!(
        diagram_kind("sequenceDiagram\n A->>B: hi"),
        Some(DiagramKind::Sequence)
    );
    assert_eq!(diagram_kind("pie\n title Pets"), None);
}

#[test]
fn verbatim_flowchart_renders_match_ts_output() {
    // 直接对比 plain 行：边框、箭头与宽度都要与下方期望字面量完全一致。
    let (plain, width, warnings) =
        render_mermaid_plain("flowchart LR\n  A[Start] --> B[Done]").expect("diagram");
    assert_eq!(
        plain,
        vec![
            "┌───────┐    ┌──────┐",
            "│ Start ├───▶│ Done │",
            "└───────┘    └──────┘"
        ]
    );
    assert_eq!(width, 21);
    assert!(warnings.is_empty());

    let (plain, _, warnings) =
        render_mermaid_plain("flowchart LR\n  A[Foo] invalid").expect("diagram");
    assert_eq!(plain, vec!["┌─────┐", "│ Foo │", "└─────┘"]);
    assert_eq!(warnings, vec!["dropped, expected a link: \"invalid\""]);
}

#[test]
fn global_theme_provider_uses_kiana_theme_tokens() {
    kiana_theme::init_theme(Some("dark"), false);
    let theme = GlobalMermaidTheme;
    assert_eq!(
        theme.fg(ThemeColor::Accent, "x"),
        kiana_theme::THEME
            .fg(ThemeColor::Accent, "x")
            .expect("theme")
    );
    assert_eq!(
        theme.bold("x"),
        kiana_theme::THEME.bold("x").expect("theme")
    );
}

#[test]
fn flowchart_layout_variants_match_grok_mermaid() {
    let raw = include_str!("fixtures/render_goldens_dark.json");
    let fixture: serde_json::Value = serde_json::from_str(raw).expect("golden json");
    let cases: [(&str, &str); 15] = [
        (
            "td_diamond",
            "flowchart TD\n  A --> B\n  A --> C\n  B --> D\n  C --> D",
        ),
        ("lr_label", "flowchart LR\n  A -- yes --> B"),
        ("pipe_label", "flowchart LR\n  A -->|go| B"),
        ("round", "flowchart LR\n  A(Round) --> B(Round)"),
        ("diamond", "flowchart LR\n  A{Decide} --> B"),
        ("self", "flowchart LR\n  A --> A"),
        ("reverse", "flowchart LR\n  A <-- B"),
        ("dotted", "flowchart LR\n  A -.-> B"),
        ("thick", "flowchart LR\n  A ==> B"),
        ("rl", "flowchart RL\n  A[Start] --> B[Done]"),
        ("bt", "flowchart BT\n  A[Start] --> B[Done]"),
        ("chain", "flowchart LR\n  A --> B --> C --> D"),
        ("skip", "flowchart TD\n  A --> B\n  B --> C\n  A --> C"),
        (
            "long_label",
            "flowchart LR\n  A[This is a rather long label that wraps] --> B",
        ),
        ("many", "flowchart LR\n  A --> B\n  B --> C\n  A --> C"),
    ];
    for (key, source) in cases {
        let golden = &fixture["layout"][key];
        let (plain, width, warnings) =
            render_mermaid_plain(source).unwrap_or_else(|| panic!("{key} did not render"));
        let expected_plain: Vec<String> = golden["plain"]
            .as_array()
            .expect("plain")
            .iter()
            .map(|line| line.as_str().unwrap().to_string())
            .collect();
        assert_eq!(plain, expected_plain, "plain mismatch for {key}\n{plain:?}");
        assert_eq!(
            width,
            golden["width"].as_u64().unwrap() as usize,
            "width mismatch for {key}"
        );
        let expected_warnings: Vec<String> = golden["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .map(|line| line.as_str().unwrap().to_string())
            .collect();
        assert_eq!(warnings, expected_warnings, "warnings mismatch for {key}");
    }
}

fn plain_lines(source: &str) -> (Vec<String>, usize) {
    let (plain, width, _) = render_mermaid_plain(source).expect("diagram");
    (plain, width)
}

#[test]
fn arrowless_links_keep_their_single_direction_glyph() {
    // grok-mermaid `maskChar`：掩码只有 U/D 时为 '│'，只有 L/R 时为 '─'；
    // 早期移植把单方向位落到 default 的 '┼'/'╋'。
    let cases: [(&str, &[&str]); 4] = [
        (
            "flowchart TD\n  A --- B",
            &[
                " ┌───┐",
                " │ A │",
                " └─┬─┘",
                "   │",
                "   │",
                " ┌───┐",
                " │ B │",
                " └───┘",
            ],
        ),
        (
            "flowchart TD\n  A === B",
            &[
                " ┌───┐",
                " │ A │",
                " └─┬─┘",
                "   ┃",
                "   ┃",
                " ┌───┐",
                " │ B │",
                " └───┘",
            ],
        ),
        (
            "flowchart LR\n  A --- B\n  B --- A",
            &[
                "┌───┐    ┌───┐",
                "│ A ├────│ B │",
                "└───┘    └─┬─┘",
                "  │        │",
                "  └────────┘",
            ],
        ),
        (
            "flowchart TD\n  A --> B\n  A --> C\n  B --- D\n  C --- D",
            &[
                "     ┌───┐",
                "     │ A │",
                "     └─┬─┘",
                "   ┌───┴───┐",
                "   ▼       ▼",
                " ┌───┐   ┌───┐",
                " │ B │   │ C │",
                " └─┬─┘   └─┬─┘",
                "   └───┬───┘",
                "       │",
                "     ┌───┐",
                "     │ D │",
                "     └───┘",
            ],
        ),
    ];
    for (source, expected) in cases {
        let (plain, _) = plain_lines(source);
        assert_eq!(plain, expected, "plain mismatch for {source}");
    }
}

#[test]
fn labels_are_measured_and_painted_per_grapheme_cluster() {
    // goldens 由 TS 的 grok-mermaid@0.2.3 render() 生成。
    let cases: [(&str, &[&str], usize); 5] = [
        (
            "flowchart LR\n  A[e\u{301}] --> B",
            &["┌───┐    ┌───┐", "│ e\u{301} ├───▶│ B │", "└───┘    └───┘"],
            14,
        ),
        (
            "flowchart LR\n  A[👨‍👩‍👧] --> B",
            &["┌────┐    ┌───┐", "│ 👨‍👩‍👧 ├───▶│ B │", "└────┘    └───┘"],
            15,
        ),
        (
            "flowchart LR\n  A[🇯🇵] --> B",
            &["┌────┐    ┌───┐", "│ 🇯🇵 ├───▶│ B │", "└────┘    └───┘"],
            15,
        ),
        (
            "flowchart LR\n  A[हिन्दी] --> B",
            &["┌────┐    ┌───┐", "│ हिन्दी ├───▶│ B │", "└────┘    └───┘"],
            15,
        ),
        (
            "flowchart LR\n  A[é] --> B",
            &["┌───┐    ┌───┐", "│ é ├───▶│ B │", "└───┘    └───┘"],
            14,
        ),
    ];
    for (source, expected, expected_width) in cases {
        let (plain, width) = plain_lines(source);
        assert_eq!(plain, expected, "plain mismatch for {source}");
        assert_eq!(width, expected_width, "width mismatch for {source}");
    }
}

#[test]
fn retries_without_the_final_line_when_the_flowchart_exceeds_the_node_cap() {
    // grok-mermaid 的 `render()` 失败时去掉最后一行重试一次（`attempt()`）：超过
    // MAX_NODES(128) 的图因此退回到能画的前缀，并在最终渲染给出 dropped 警告。
    let mut source = String::from("flowchart TD\n");
    for index in 0..128 {
        source.push_str(&format!("  N{index} --> N{}\n", index + 1));
    }

    let (plain, _width, warnings) =
        render_mermaid_plain(&source).expect("the salvaged prefix renders");
    assert_eq!(
        warnings,
        vec![r#"dropped, unreadable final line: "N127 --> N128""#.to_string()]
    );
    assert!(plain.iter().any(|line| line.contains("N0")));

    let markdown = format!("```mermaid\n{source}```");
    let final_render = transform_mermaid(&markdown, Options::default());
    assert!(final_render.starts_with(&markdown), "{final_render}");
    assert!(
        final_render.contains(
            "Mermaid diagram not rendered: dropped, unreadable final line: \"N127 --> N128\""
        ),
        "{final_render}"
    );

    let streaming = transform_mermaid(
        &markdown,
        Options {
            is_streaming: true,
            ..Options::default()
        },
    );
    assert!(
        !streaming.contains("Mermaid diagram not rendered"),
        "{streaming}"
    );
    assert!(streaming.contains("N0"), "{streaming}");
}
