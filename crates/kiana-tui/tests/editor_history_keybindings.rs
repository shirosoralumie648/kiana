//! `packages/tui/test/editor-history-keybindings.test.ts` 的 Rust 移植。

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::TestTerminal;
use kiana_tui::Component;
use kiana_tui::components::editor::{Editor, EditorOptions, EditorTheme, EditorTui};
use kiana_tui::keybindings::{
    KeyIdList, KeybindingsConfig, KeybindingsManager, set_keybindings, tui_keybindings_static,
};
use kiana_tui::terminal_colors::Terminal;

/// TS `afterEach(() => setKeybindings(new KeybindingsManager(TUI_KEYBINDINGS)))`。
fn reset_keybindings() {
    set_keybindings(KeybindingsManager::with_defaults());
}

fn new_editor() -> Editor {
    let terminal: Rc<RefCell<Box<dyn Terminal>>> =
        Rc::new(RefCell::new(Box::new(TestTerminal::new())));
    Editor::new(
        EditorTui::from_terminal(terminal),
        EditorTheme::plain(),
        EditorOptions::default(),
    )
}

#[test]
fn browses_history_directly_without_first_moving_the_cursor() {
    let mut config = KeybindingsConfig::new();
    config.insert(
        "tui.editor.historyPrevious".to_string(),
        KeyIdList::One("ctrl+p".to_string()),
    );
    config.insert(
        "tui.editor.historyNext".to_string(),
        KeyIdList::One("ctrl+n".to_string()),
    );
    set_keybindings(KeybindingsManager::new(
        tui_keybindings_static().clone(),
        config,
    ));

    let mut editor = new_editor();
    editor.add_to_history("older prompt");
    editor.add_to_history("newer\nmultiline prompt");
    editor.set_text("draft");
    editor.handle_input("\x1b[D");
    editor.handle_input("\x1b[D");

    editor.handle_input("\x10"); // Ctrl+P
    assert_eq!(editor.get_text(), "newer\nmultiline prompt");
    assert_eq!(editor.get_cursor(), (0, 0));

    editor.handle_input("\x10"); // Ctrl+P
    assert_eq!(editor.get_text(), "older prompt");

    editor.handle_input("\x0e"); // Ctrl+N
    assert_eq!(editor.get_text(), "newer\nmultiline prompt");
    assert_eq!(editor.get_cursor(), (1, 16));

    editor.handle_input("\x0e"); // Ctrl+N
    assert_eq!(editor.get_text(), "draft");
    assert_eq!(editor.get_cursor(), (0, 3));

    reset_keybindings();
}
