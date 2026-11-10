//! `components/user-message-selector.ts` 的单元测试（TS 侧无独立测试文件）。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Once;

use kiana_interactive::components::user_message_selector::{
    UserMessageItem, UserMessageList, UserMessageSelectorComponent,
};
use kiana_tui::text::visible_width;
use kiana_tui::tui::Component;

static THEME_INIT: Once = Once::new();

fn setup() {
    THEME_INIT.call_once(|| kiana_theme::init_theme(Some("dark"), false));
    kiana_tui::set_keybindings(kiana_tui::keybindings::KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        kiana_config::keybindings::KeybindingsConfig::new(),
    ));
}

fn message(id: &str, text: &str) -> UserMessageItem {
    UserMessageItem {
        id: id.to_string(),
        text: text.to_string(),
        timestamp: None,
    }
}

#[test]
fn defaults_to_the_most_recent_message() {
    setup();
    let list = UserMessageList::new(vec![message("a", "first"), message("b", "second")], None);
    assert_eq!(
        list.selected_message().map(|item| item.id.as_str()),
        Some("b")
    );
}

#[test]
fn selects_explicit_initial_message() {
    setup();
    let list = UserMessageList::new(
        vec![message("a", "first"), message("b", "second")],
        Some("a"),
    );
    assert_eq!(
        list.selected_message().map(|item| item.id.as_str()),
        Some("a")
    );
}

#[test]
fn up_and_down_wrap_around() {
    setup();
    let mut list = UserMessageList::new(
        vec![message("a", "first"), message("b", "second")],
        Some("a"),
    );
    assert_eq!(list.selected_index(), 0);
    list.handle_input("\x1b[A"); // up 环绕到最后一个
    assert_eq!(list.selected_index(), 1);
    list.handle_input("\x1b[B"); // down 环绕回第一个
    assert_eq!(list.selected_index(), 0);
}

#[test]
fn confirm_reports_selected_entry_id() {
    setup();
    let selected: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let selected_clone = Rc::clone(&selected);
    let mut list = UserMessageList::new(
        vec![message("a", "first"), message("b", "second")],
        Some("a"),
    );
    list.on_select = Some(Box::new(move |id: &str| {
        *selected_clone.borrow_mut() = Some(id.to_string());
    }));
    list.handle_input("\r");
    assert_eq!(selected.borrow().as_deref(), Some("a"));
}

#[test]
fn renders_messages_metadata_and_scroll_indicator() {
    setup();
    let messages: Vec<UserMessageItem> = (0..12)
        .map(|index| message(&index.to_string(), &format!("message {index}\nline two")))
        .collect();
    let mut list = UserMessageList::new(messages, None);
    let lines = list.render(40);
    let joined = lines.join("\n");
    assert!(joined.contains("message 11 line two"));
    assert!(joined.contains("Message 12 of 12"));
    assert!(joined.contains("(12/12)"));
    for line in &lines {
        assert!(visible_width(line) <= 40, "line too wide: {line:?}");
    }
}

#[test]
fn renders_empty_state() {
    setup();
    let mut list = UserMessageList::new(Vec::new(), None);
    let lines = list.render(40);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("No user messages found"));
}

#[test]
fn component_renders_layout_and_forwards_input() {
    setup();
    let selected: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let selected_clone = Rc::clone(&selected);
    let cancelled = Rc::new(RefCell::new(false));
    let cancelled_clone = Rc::clone(&cancelled);
    let mut component = UserMessageSelectorComponent::new(
        vec![message("a", "first"), message("b", "second")],
        Box::new(move |id: &str| {
            *selected_clone.borrow_mut() = Some(id.to_string());
        }),
        Box::new(move || {
            *cancelled_clone.borrow_mut() = true;
        }),
        None,
    );
    let lines = component.render(60);
    let joined = lines.join("\n");
    assert!(joined.contains("Fork from Message"));
    assert!(joined.contains("Select a user message to copy the active path"));
    assert!(joined.contains("Message 2 of 2"));

    component.handle_input("\r");
    assert_eq!(selected.borrow().as_deref(), Some("b"));

    component.handle_input("\x1b");
    assert!(*cancelled.borrow());

    assert_eq!(
        component
            .get_message_list()
            .selected_message()
            .map(|item| item.id.as_str()),
        Some("b")
    );
}

#[test]
fn renders_ts_golden_layout() {
    setup();
    let messages = vec![
        message("a", "first message"),
        message("b", "second\nmessage with more text"),
        message("c", "third message"),
    ];
    let mut selector =
        UserMessageSelectorComponent::new(messages, Box::new(|_| {}), Box::new(|| {}), None);
    let expected = r#"== w40 ==

 Fork from Message                      
 Select a user message to copy the      
 active path up to that point into a    
 new session                            

────────────────────────────────────────

  first message
  Message 1 of 3

  second message with more text
  Message 2 of 3

› third message
  Message 3 of 3


────────────────────────────────────────
== w20 ==

 Fork from Message  
 Select a user      
 message to copy    
 the active path up 
 to that point into 
 a new session      

────────────────────

  first message
  Message 1 of 3

  second message ...
  Message 2 of 3

› third message
  Message 3 of 3


────────────────────
== w40 after up2 ==

 Fork from Message                      
 Select a user message to copy the      
 active path up to that point into a    
 new session                            

────────────────────────────────────────

› first message
  Message 1 of 3

  second message with more text
  Message 2 of 3

  third message
  Message 3 of 3


────────────────────────────────────────
== empty w40 ==

 Fork from Message                      
 Select a user message to copy the      
 active path up to that point into a    
 new session                            

────────────────────────────────────────

  No user messages found

────────────────────────────────────────
"#;

    let mut out = String::new();
    out.push_str("== w40 ==\n");
    out.push_str(&render_plain(&mut selector, 40));
    out.push_str("== w20 ==\n");
    out.push_str(&render_plain(&mut selector, 20));
    {
        let list = selector.get_message_list();
        list.handle_input("\x1b[A");
        list.handle_input("\x1b[A");
    }
    out.push_str("== w40 after up2 ==\n");
    out.push_str(&render_plain(&mut selector, 40));
    let mut empty =
        UserMessageSelectorComponent::new(Vec::new(), Box::new(|_| {}), Box::new(|| {}), None);
    out.push_str("== empty w40 ==\n");
    out.push_str(&render_plain(&mut empty, 40));

    assert_eq!(out, expected);
}

fn render_plain(component: &mut UserMessageSelectorComponent, width: usize) -> String {
    let mut out = component
        .render(width)
        .into_iter()
        .map(|line| kiana_core::ansi::strip_ansi(&line))
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    out
}
