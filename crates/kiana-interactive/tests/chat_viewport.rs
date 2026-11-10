//! `chat-viewport.test.ts` 的移植。

use kiana_interactive::chat_viewport::{ChatViewportOptions, create_chat_viewport};
use kiana_tui::components::scroll_view::ScrollViewScrollbar;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::{Component, Container};

use crate::common::shared;

mod common;

fn options(scrollbar: Option<ScrollViewScrollbar>) -> ChatViewportOptions {
    ChatViewportOptions {
        document: shared(Container::new()),
        pending_messages: shared(Container::new()),
        status: shared(Container::new()),
        editor: shared(Container::new()),
        footer: shared(Container::new()),
        widgets_above: None,
        widgets_below: None,
        scrollbar,
        scrollbar_track_style: None,
        scrollbar_thumb_style: None,
    }
}

#[test]
fn defaults_the_transcript_scrollbar_to_auto_and_accepts_overrides() {
    let automatic = create_chat_viewport(options(None));
    let hidden = create_chat_viewport(options(Some(ScrollViewScrollbar::Hidden)));

    assert_eq!(automatic.scrollbar(), ScrollViewScrollbar::Auto);
    assert_eq!(hidden.scrollbar(), ScrollViewScrollbar::Hidden);
}

#[test]
fn lays_out_transcript_above_the_input_dock() {
    let document = shared(Text::new("transcript line"));
    let editor = shared(Text::new("editor"));
    let footer = shared(Text::new("footer"));
    let mut viewport = create_chat_viewport(ChatViewportOptions {
        document,
        pending_messages: shared(Container::new()),
        status: shared(Container::new()),
        editor,
        footer,
        widgets_above: None,
        widgets_below: None,
        scrollbar: None,
        scrollbar_track_style: None,
        scrollbar_thumb_style: None,
    });

    // dock（editor/footer）在 root 里渲染。
    let lines = viewport.root.render(60);
    let rendered = lines.join("\n");
    assert!(rendered.contains("editor"), "rendered: {rendered}");
    assert!(rendered.contains("footer"), "rendered: {rendered}");
    // transcript 挂载的是传入的 document（同一实例，内容可渲染）。
    let transcript = viewport.transcript.borrow_mut().render(60).join("\n");
    assert!(
        transcript.contains("transcript line"),
        "transcript: {transcript}"
    );
}
