//! 移植 `packages/coding-agent/test/tree-selector.test.ts`（18 项）。

use std::collections::HashMap;
use std::sync::Once;

use chrono::TimeZone;
use chrono::Timelike;

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, JsonObject, Message, StopReason, TextContent,
    TextKind, ToolCall, ToolCallKind, UserContent, UserMessage, UserRole,
};
use kiana_core::ansi::strip_ansi;
use kiana_interactive::components::tree_selector::{FilterMode, TreeSelectorComponent};
use kiana_session_core::{
    ContextEditEntry, ContextEditType, CustomMessageEntry, CustomMessageType, LabelType,
    ModelChangeEntry, ModelChangeType, SessionEntry, SessionMessageEntry, SessionMessageType,
    SessionTreeNode, ThinkingLevelChangeEntry, ThinkingLevelChangeType,
};
use kiana_tui::text::visible_width;

const UP: &str = "\x1b[A";
const DOWN: &str = "\x1b[B";
const CTRL_LEFT: &str = "\x1b[1;5D";
const CTRL_RIGHT: &str = "\x1b[1;5C";
const ALT_LEFT: &str = "\x1b[1;3D";
const ALT_RIGHT: &str = "\x1b[1;3C";

static THEME_INIT: Once = Once::new();

fn setup() {
    THEME_INIT.call_once(|| kiana_theme::init_theme(Some("dark"), false));
    // 测试隔离：keybindings 是 thread-local 单例。
    kiana_tui::set_keybindings(kiana_tui::keybindings::KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        kiana_config::keybindings::KeybindingsConfig::new(),
    ));
}

fn timestamp() -> String {
    "2026-01-01T00:00:00.000Z".to_string()
}

fn user_message(id: &str, parent_id: Option<&str>, content: &str) -> SessionEntry {
    SessionEntry::Message(SessionMessageEntry {
        kind: SessionMessageType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: timestamp(),
        message: AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(content.to_string()),
            timestamp: 0,
        })),
        raw_message: None,
    })
}

fn assistant_message(id: &str, parent_id: Option<&str>, text: &str) -> SessionEntry {
    SessionEntry::Message(SessionMessageEntry {
        kind: SessionMessageType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: timestamp(),
        message: AgentMessage::Standard(Message::Assistant(AssistantMessage {
            content: vec![AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: text.to_string(),
                text_signature: None,
            })],
            stop_reason: StopReason::Stop,
            ..Default::default()
        })),
        raw_message: None,
    })
}

fn tool_call_only_assistant(id: &str, parent_id: Option<&str>) -> SessionEntry {
    let mut arguments = JsonObject::new();
    arguments.insert(
        "path".to_string(),
        serde_json::Value::String("test.ts".to_string()),
    );
    SessionEntry::Message(SessionMessageEntry {
        kind: SessionMessageType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: timestamp(),
        message: AgentMessage::Standard(Message::Assistant(AssistantMessage {
            content: vec![AssistantContentBlock::ToolCall(ToolCall {
                kind_tool_call: ToolCallKind::ToolCall,
                id: format!("tc-{id}"),
                name: "read".to_string(),
                arguments,
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            })],
            stop_reason: StopReason::ToolUse,
            ..Default::default()
        })),
        raw_message: None,
    })
}

fn model_change(id: &str, parent_id: Option<&str>) -> SessionEntry {
    SessionEntry::ModelChange(ModelChangeEntry {
        kind: ModelChangeType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: timestamp(),
        provider: "anthropic".to_string(),
        model_id: "claude-sonnet-4".to_string(),
    })
}

fn thinking_level_change(id: &str, parent_id: Option<&str>) -> SessionEntry {
    SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
        kind: ThinkingLevelChangeType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: timestamp(),
        thinking_level: "high".to_string(),
    })
}

fn context_edit(id: &str, parent_id: Option<&str>, target_id: &str) -> SessionEntry {
    SessionEntry::ContextEdit(ContextEditEntry {
        kind: ContextEditType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: timestamp(),
        target_id: target_id.to_string(),
        replacement: None,
        raw_entry: None,
    })
}

/// `buildTree(entries)`：按 `parentId` 关系建树；缺失父节点的条目无法挂到树上，因此被丢弃。
fn build_tree(entries: Vec<SessionEntry>) -> Vec<SessionTreeNode> {
    if entries.is_empty() {
        return Vec::new();
    }
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        by_id.insert(entry.id().to_string(), index);
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); entries.len()];
    let mut roots: Vec<usize> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        match entry.parent_id() {
            None => roots.push(index),
            Some(parent_id) => {
                if let Some(parent) = by_id.get(parent_id) {
                    children[*parent].push(index);
                }
            }
        }
    }
    fn build(
        index: usize,
        entries: &[SessionEntry],
        children: &[Vec<usize>],
        labels: &mut HashMap<usize, (Option<String>, Option<String>)>,
    ) -> SessionTreeNode {
        let (label, label_timestamp) = labels.remove(&index).unwrap_or((None, None));
        SessionTreeNode {
            entry: entries[index].clone(),
            children: children[index]
                .iter()
                .map(|child| build(*child, entries, children, labels))
                .collect(),
            label,
            label_timestamp,
        }
    }
    let mut labels: HashMap<usize, (Option<String>, Option<String>)> = HashMap::new();
    roots
        .iter()
        .map(|root| build(*root, &entries, &children, &mut labels))
        .collect()
}

fn selected_id(selector: &mut TreeSelectorComponent) -> Option<String> {
    selector
        .get_tree_list()
        .get_selected_node()
        .map(|node| node.entry.id().to_string())
}

fn noop_select() -> Box<dyn FnMut(&str)> {
    Box::new(|_| {})
}

fn noop_cancel() -> Box<dyn FnMut()> {
    Box::new(|| {})
}

#[test]
fn focuses_nearest_visible_ancestor_for_model_change_with_sibling_branch() {
    setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "active branch"),
        model_change("model-1", Some("user-2")),
        user_message("user-3", Some("asst-1"), "sibling branch"),
    ]);
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("model-1".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-2"));
}

#[test]
fn hides_context_edits_by_default_and_labels_them_in_all_mode() {
    setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        context_edit("edit-1", Some("asst-1"), "asst-1"),
    ]);
    let mut default_selector = TreeSelectorComponent::new(
        tree.clone(),
        Some("edit-1".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    assert_eq!(
        selected_id(&mut default_selector).as_deref(),
        Some("asst-1")
    );

    let mut all_selector = TreeSelectorComponent::new(
        tree,
        Some("edit-1".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        Some(FilterMode::All),
    );
    let rendered = all_selector.get_tree_list().render(200).join("\n");
    assert!(strip_ansi(&rendered).contains("[context omit: asst-1]"));
}

#[test]
fn focuses_nearest_visible_ancestor_for_thinking_level_change() {
    setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "active branch"),
        thinking_level_change("thinking-1", Some("user-2")),
        user_message("user-3", Some("asst-1"), "sibling branch"),
    ]);
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("thinking-1".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-2"));
}

#[test]
fn switches_to_nearest_visible_user_message_in_user_only_filter() {
    setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "active branch"),
        assistant_message("asst-2", Some("user-2"), "response"),
        user_message("user-3", Some("asst-1"), "sibling branch"),
    ]);
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("asst-2".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-2"));
    selector.handle_input("\x15"); // Ctrl+U：user-only
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-2"));
}

#[test]
fn returns_to_nearest_visible_ancestor_when_switching_back_to_default() {
    setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "active branch"),
        assistant_message("asst-2", Some("user-2"), "response"),
        user_message("user-3", Some("asst-1"), "sibling branch"),
    ]);
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("asst-2".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-2"));
    selector.handle_input("\x15");
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-2"));
    selector.handle_input("\x04"); // Ctrl+D：default
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-2"));
}

#[test]
fn renders_semantic_help_rows_without_truncating_narrow_terminal_controls() {
    setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
    ]);
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("asst-1".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    let plain_lines: Vec<String> = selector
        .render(30)
        .into_iter()
        .map(|line| strip_ansi(&line))
        .collect();
    let plain = plain_lines.join("\n");
    assert!(plain.contains("branch"));
    assert!(plain.contains("copy"));
    assert!(plain.contains("filters"));
    assert!(plain.contains("cycle"));
    assert!(plain.contains("label time"));
    assert!(!plain.contains("..."));
    assert!(plain_lines.iter().all(|line| visible_width(line) <= 30));
}

#[test]
fn copies_the_full_selected_message_with_ctrl_x() {
    setup();
    let message = format!("{}\nsecond line", "long message ".repeat(30));
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), &message),
    ]);
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("asst-1".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    let copied: std::rc::Rc<std::cell::RefCell<Option<String>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let copied_clone = std::rc::Rc::clone(&copied);
    selector.on_copy = Some(Box::new(move |text: Option<&str>| {
        *copied_clone.borrow_mut() = text.map(str::to_string);
    }));
    selector.handle_input("\x18");
    assert_eq!(copied.borrow().as_deref(), Some(message.as_str()));
}

#[test]
fn toggles_label_timestamps_for_labeled_nodes() {
    setup();
    let entries = vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
    ];
    let mut tree = build_tree(entries);
    let label_date = chrono::Local
        .with_ymd_and_hms(2026, 3, 28, 14, 32, 0)
        .single()
        .expect("valid label date");
    tree[0].label = Some("checkpoint".to_string());
    tree[0].label_timestamp = Some(label_date.to_rfc3339());
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("asst-1".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    let render = selector.get_tree_list().render(200).join("\n");
    assert!(render.contains("[checkpoint]"));
    assert!(!render.contains("3/28 14:32"));
    assert!(!render.contains("[+label time]"));

    selector.handle_input("T");
    let render = selector.get_tree_list().render(200).join("\n");
    assert!(render.contains("3/28 14:32"));
    assert!(render.contains("[+label time]"));
}

#[test]
fn preserves_selection_when_switching_to_empty_labeled_filter_and_back() {
    setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "bye"),
        assistant_message("asst-2", Some("user-2"), "goodbye"),
    ]);
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("asst-2".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-2"));
    selector.handle_input("\x0c"); // Ctrl+L：labeled-only
    assert_eq!(selected_id(&mut selector), None);
    selector.handle_input("\x04"); // Ctrl+D：default
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-2"));
}

#[test]
fn preserves_selection_through_multiple_empty_filter_switches() {
    setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
    ]);
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("asst-1".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-1"));
    selector.handle_input("\x0c");
    assert_eq!(selected_id(&mut selector), None);
    selector.handle_input("\x0c");
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-1"));
    selector.handle_input("\x0c");
    assert_eq!(selected_id(&mut selector), None);
    selector.handle_input("\x04");
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-1"));
}

fn build_branching_tree() -> Vec<SessionTreeNode> {
    build_tree(vec![
        user_message("user-1", None, "first message"),
        assistant_message("asst-1", Some("user-1"), "response 1"),
        user_message("user-2", Some("asst-1"), "second message"),
        assistant_message("asst-2", Some("user-2"), "response 2"),
        user_message("user-3a", Some("asst-2"), "branch A start"),
        assistant_message("asst-3a", Some("user-3a"), "branch A response"),
        user_message("user-4a", Some("asst-3a"), "branch A deep"),
        assistant_message(
            "asst-4a",
            Some("user-4a"),
            "branch A leaf message that is long enough to truncate perhaps",
        ),
        user_message("user-3b", Some("asst-2"), "branch B start"),
        assistant_message("asst-3b", Some("user-3b"), "branch B response"),
        user_message("user-4b", Some("asst-3b"), "branch B deep"),
    ])
}

fn branching_selector() -> TreeSelectorComponent {
    TreeSelectorComponent::new(
        build_branching_tree(),
        Some("asst-4a".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    )
}

#[test]
fn ctrl_right_unfolds_folded_node_then_segment_jumps() {
    setup();
    let mut selector = branching_selector();
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3a"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3a"));
    selector.handle_input(DOWN);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3b"));
    selector.handle_input(UP);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3a"));
    selector.handle_input(CTRL_RIGHT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3a"));
    selector.handle_input(DOWN);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-3a"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3a"));
    selector.handle_input(CTRL_RIGHT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-4a"));
}

#[test]
fn alt_left_right_are_aliases_for_fold_and_unfold_navigation() {
    setup();
    let mut selector = branching_selector();
    selector.handle_input(ALT_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3a"));
    selector.handle_input(ALT_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3a"));
    selector.handle_input(ALT_RIGHT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3a"));
    selector.handle_input(ALT_RIGHT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-4a"));
}

#[test]
fn folding_root_hides_subtree_and_preserves_nested_fold() {
    setup();
    let mut selector = branching_selector();
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3a"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3a"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-1"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-1"));
    selector.handle_input(DOWN);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-1"));
    selector.handle_input(CTRL_RIGHT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-1"));
    selector.handle_input(CTRL_RIGHT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3a"));
    selector.handle_input(DOWN);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3b"));
}

#[test]
fn folds_and_navigates_on_non_active_branch() {
    setup();
    let mut selector = branching_selector();
    let mut found = false;
    for _ in 0..20 {
        selector.handle_input(DOWN);
        if selected_id(&mut selector).as_deref() == Some("user-3b") {
            found = true;
            break;
        }
    }
    assert!(found);
    selector.handle_input(CTRL_RIGHT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-4b"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3b"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3b"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-1"));
}

#[test]
fn folds_and_navigates_with_multiple_roots() {
    setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "first root"),
        assistant_message("asst-1", Some("user-1"), "response 1"),
        user_message("user-2", None, "second root"),
        assistant_message("asst-2", Some("user-2"), "response 2"),
    ]);
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("asst-1".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-1"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-1"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-1"));
    selector.handle_input(DOWN);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-2"));
    selector.handle_input(CTRL_RIGHT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-2"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-2"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-2"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-2"));
}

#[test]
fn folding_root_hides_descendants_even_when_intermediate_nodes_are_filtered() {
    setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        tool_call_only_assistant("tool-asst-1", Some("user-1")),
        user_message("user-2", Some("tool-asst-1"), "follow up"),
        assistant_message("asst-2", Some("user-2"), "response"),
    ]);
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("asst-2".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-1"));
    selector.handle_input(CTRL_LEFT);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-1"));
    selector.handle_input(DOWN);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-1"));
}

#[test]
fn search_resets_fold_state() {
    setup();
    let mut selector = branching_selector();
    selector.handle_input(CTRL_LEFT);
    selector.handle_input(CTRL_LEFT);
    selector.handle_input(DOWN);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("user-3b"));

    selector.handle_input("b");
    selector.handle_input("\x1b"); // 清空搜索

    let mut current_id = String::new();
    for _ in 0..20 {
        selector.handle_input(DOWN);
        current_id = selected_id(&mut selector).unwrap_or_default();
        if current_id == "user-3a" {
            break;
        }
    }
    assert_eq!(current_id, "user-3a");

    selector.handle_input(DOWN);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-3a"));
}

#[test]
fn filter_mode_change_resets_fold_state() {
    setup();
    let mut selector = branching_selector();
    selector.handle_input(CTRL_LEFT);
    selector.handle_input(CTRL_LEFT);

    selector.handle_input("\x15"); // ctrl+u：user-only 清折叠
    selector.handle_input("\x04"); // ctrl+d：回到 default

    let mut current_id = String::new();
    for _ in 0..20 {
        selector.handle_input(DOWN);
        current_id = selected_id(&mut selector).unwrap_or_default();
        if current_id == "user-3a" {
            break;
        }
    }
    assert_eq!(current_id, "user-3a");

    selector.handle_input(DOWN);
    assert_eq!(selected_id(&mut selector).as_deref(), Some("asst-3a"));
}

/// 保证 `LabelType` 被引用（测试文件里没有 label 条目，但保持导入与源文件一一对应）。
#[allow(dead_code)]
fn _label_type_marker() -> LabelType {
    LabelType::Marker
}

#[test]
fn renders_ts_golden_tree_layout() {
    setup();
    let mut tree = build_branching_tree();
    let label_date = chrono::Local
        .with_ymd_and_hms(2026, 3, 28, 14, 32, 0)
        .single()
        .expect("valid label date");
    tree[0].label = Some("checkpoint".to_string());
    tree[0].label_timestamp = Some(label_date.to_rfc3339());
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("asst-4a".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );

    let expected = r#"== width 40 initial ==

────────────────────────────────────────
   Session Tree                         
  ↑/↓ move · ←/→ page · ctrl+←/→ branch
  ctrl+x copy · shift+l label
  shift+t label time
  filters ctrl+d/t/u/l/a
  cycle ctrl+o/shift+ctrl+o
  Type to search:
────────────────────────────────────────

  • [checkpoint] user: first message
  • assistant: response 1
  • user: second message
  • assistant: response 2
  ├⊟ • user: branch A start
  │     • assistant: branch A response
  │     • user: branch A deep
› │     • assistant: branch A leaf messa
  └⊟ user: branch B start
        assistant: branch B response
        user: branch B deep
  (8/11)

────────────────────────────────────────
== width 80 initial ==

────────────────────────────────────────────────────────────────────────────────
   Session Tree                                                                 
  ↑/↓ move · ←/→ page · ctrl+←/→ branch · ctrl+x copy · shift+l label
  shift+t label time · filters ctrl+d/t/u/l/a · cycle ctrl+o/shift+ctrl+o
  Type to search:
────────────────────────────────────────────────────────────────────────────────

  • [checkpoint] user: first message
  • assistant: response 1
  • user: second message
  • assistant: response 2
  ├⊟ • user: branch A start
  │     • assistant: branch A response
  │     • user: branch A deep
› │     • assistant: branch A leaf message that is long enough to truncate perha
  └⊟ user: branch B start
        assistant: branch B response
        user: branch B deep
  (8/11)

────────────────────────────────────────────────────────────────────────────────
== width 40 timestamps ==

────────────────────────────────────────
   Session Tree                         
  ↑/↓ move · ←/→ page · ctrl+←/→ branch
  ctrl+x copy · shift+l label
  shift+t label time
  filters ctrl+d/t/u/l/a
  cycle ctrl+o/shift+ctrl+o
  Type to search:
────────────────────────────────────────

  • [checkpoint] 3/28 14:32 user: first 
  • assistant: response 1
  • user: second message
  • assistant: response 2
  ├⊟ • user: branch A start
  │     • assistant: branch A response
  │     • user: branch A deep
› │     • assistant: branch A leaf messa
  └⊟ user: branch B start
        assistant: branch B response
        user: branch B deep
  (8/11) [+label time]

────────────────────────────────────────
== folded selected: user-3a

────────────────────────────────────────────────────────────────────────────────
   Session Tree                                                                 
  ↑/↓ move · ←/→ page · ctrl+←/→ branch · ctrl+x copy · shift+l label
  shift+t label time · filters ctrl+d/t/u/l/a · cycle ctrl+o/shift+ctrl+o
  Type to search:
────────────────────────────────────────────────────────────────────────────────

  • [checkpoint] 3/28 14:32 user: first message
  • assistant: response 1
  • user: second message
  • assistant: response 2
› ├⊞ • user: branch A start
  └⊟ user: branch B start
        assistant: branch B response
        user: branch B deep
  (5/8) [+label time]

────────────────────────────────────────────────────────────────────────────────
"#;

    let mut out = String::new();
    for width in [40, 80] {
        out.push_str(&format!("== width {width} initial ==\n"));
        out.push_str(
            &selector
                .render(width)
                .into_iter()
                .map(|line| strip_ansi(&line))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        out.push('\n');
    }
    selector.handle_input("T");
    out.push_str("== width 40 timestamps ==\n");
    out.push_str(
        &selector
            .render(40)
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    out.push('\n');
    selector.handle_input(CTRL_LEFT);
    selector.handle_input(CTRL_LEFT);
    out.push_str(&format!(
        "== folded selected: {}\n",
        selected_id(&mut selector).unwrap_or_default()
    ));
    out.push_str(
        &selector
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    out.push('\n');

    assert_eq!(out, expected);
}

// ---------------------------------------------------------------------------
// 回归测试：reviewer 发现的行为差异
// ---------------------------------------------------------------------------

fn selector_with_label_timestamp(label_timestamp: Option<&str>) -> TreeSelectorComponent {
    let mut tree = build_tree(vec![user_message("u1", None, "hello")]);
    tree[0].label = Some("checkpoint".to_string());
    tree[0].label_timestamp = label_timestamp.map(str::to_string);
    TreeSelectorComponent::new(
        tree,
        Some("u1".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    )
}

/// 搜索文本按 `extractContent(...).slice(0, 200)` 截断（UTF-16 code unit）；只在 200 之后
/// 出现的匹配必须被过滤掉（TS `getSearchableText` 对 message 条目如此）。
#[test]
fn search_truncates_message_content_to_200_utf16_units() {
    setup();
    let long_prefix = "x".repeat(300);
    let tree = build_tree(vec![
        user_message("u-early", None, "needle at start"),
        user_message(
            "u-deep",
            Some("u-early"),
            &format!("{long_prefix} needle after 200"),
        ),
    ]);
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("u-deep".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    for ch in "needle".chars() {
        selector.handle_input(&ch.to_string());
    }
    assert_eq!(selected_id(&mut selector).as_deref(), Some("u-early"));
    let rendered = strip_ansi(&selector.get_tree_list().render(200).join("\n"));
    assert!(rendered.contains("(1/1)"), "{rendered}");
}

/// `custom_message` 的数组内容同样截断到 200 个 UTF-16 code unit。
#[test]
fn search_truncates_custom_message_array_content_to_200_utf16_units() {
    setup();
    let long_prefix = "y".repeat(300);
    let custom = SessionEntry::CustomMessage(CustomMessageEntry {
        kind: CustomMessageType::Marker,
        custom_type: "note".to_string(),
        content: serde_json::json!([
            {"type": "text", "text": format!("{long_prefix} needle after 200")}
        ]),
        display: true,
        details: None,
        id: "custom-1".to_string(),
        parent_id: None,
        timestamp: timestamp(),
        raw_entry: None,
    });
    let tree = vec![SessionTreeNode {
        entry: custom,
        children: Vec::new(),
        label: None,
        label_timestamp: None,
    }];
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("custom-1".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    for ch in "needle".chars() {
        selector.handle_input(&ch.to_string());
    }
    let rendered = strip_ansi(&selector.get_tree_list().render(200).join("\n"));
    assert!(rendered.contains("(0/0)"), "{rendered}");
}

/// `labelTimestamp` 的真值判断与 `new Date(...)` 解析语义。
#[test]
fn formats_label_timestamps_like_js_date_parsing() {
    setup();

    // 空串是 falsy：不显示时间戳，也不留下多余空格。
    let mut selector = selector_with_label_timestamp(Some(""));
    selector.handle_input("T");
    let rendered = strip_ansi(&selector.get_tree_list().render(100).join("\n"));
    assert!(rendered.contains("[checkpoint] user: hello"), "{rendered}");
    assert!(!rendered.contains("[checkpoint]  "), "{rendered}");

    // date-only 在 JS 里按 UTC 午夜解析，再按本地时区显示。
    let mut selector = selector_with_label_timestamp(Some("2026-03-28"));
    selector.handle_input("T");
    let rendered = strip_ansi(&selector.get_tree_list().render(100).join("\n"));
    let local = chrono::Utc
        .with_ymd_and_hms(2026, 3, 28, 0, 0, 0)
        .single()
        .expect("valid date")
        .with_timezone(&chrono::Local);
    let expected = format!("3/28 {:02}:{:02}", local.hour(), local.minute());
    assert!(
        rendered.contains(&format!("[checkpoint] {expected} user: hello")),
        "{rendered}"
    );

    // Invalid Date：所有 getter 都是 NaN，TS 输出 "aN/NaN/NaN NaN:NaN"。
    let mut selector = selector_with_label_timestamp(Some("not-a-date"));
    selector.handle_input("T");
    let rendered = strip_ansi(&selector.get_tree_list().render(100).join("\n"));
    assert!(
        rendered.contains("[checkpoint] aN/NaN/NaN NaN:NaN user: hello"),
        "{rendered}"
    );
}

/// 已知类型但字段畸形、退化为 `Unknown` 的条目仍按 `entry.type` 分派：
/// 默认过滤隐藏设置类条目；`all` 模式按原类型渲染（缺失字段 -> `undefined`）。
#[test]
fn dispatches_unknown_entries_by_original_type() {
    setup();
    let mut object = JsonObject::new();
    object.insert(
        "type".to_string(),
        serde_json::Value::String("model_change".to_string()),
    );
    object.insert(
        "id".to_string(),
        serde_json::Value::String("bad-model".to_string()),
    );
    object.insert(
        "parentId".to_string(),
        serde_json::Value::String("u1".to_string()),
    );
    object.insert(
        "timestamp".to_string(),
        serde_json::Value::String(timestamp()),
    );
    let bad = SessionEntry::Unknown(object);
    let tree = vec![SessionTreeNode {
        entry: user_message("u1", None, "hello"),
        children: vec![SessionTreeNode {
            entry: bad,
            children: Vec::new(),
            label: None,
            label_timestamp: None,
        }],
        label: None,
        label_timestamp: None,
    }];
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("u1".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    let rendered = strip_ansi(&selector.get_tree_list().render(60).join("\n"));
    assert!(rendered.contains("(1/1)"), "{rendered}");
    assert!(!rendered.contains("[model:"), "{rendered}");

    selector.handle_input("\x01"); // ctrl+a: all
    let rendered = strip_ansi(&selector.get_tree_list().render(60).join("\n"));
    assert!(rendered.contains("[model: undefined]"), "{rendered}");
}

/// TS 把组件回调装在 `TreeList` 的公开字段上；通过 `getTreeList()` 覆盖回调必须改变行为。
/// Rust 侧组件不能绕过这些字段自行分发。
#[test]
fn tree_list_callback_overrides_redirect_component_input() {
    use std::cell::RefCell;
    use std::rc::Rc;

    setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "assistant text"),
    ]);
    let constructor_select: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let constructor_cancel = Rc::new(RefCell::new(0usize));
    let select_calls = Rc::clone(&constructor_select);
    let cancel_calls = Rc::clone(&constructor_cancel);
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("asst-1".to_string()),
        24,
        Box::new(move |id: &str| select_calls.borrow_mut().push(id.to_string())),
        Box::new(move || *cancel_calls.borrow_mut() += 1),
        None,
        None,
        None,
    );

    // 覆盖 TreeList.onSelect：组件的构造回调不应再被调用。
    let tree_select: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let calls = Rc::clone(&tree_select);
        selector.get_tree_list().on_select = Some(Box::new(move |id: &str| {
            calls.borrow_mut().push(id.to_string())
        }));
    }
    selector.handle_input("\r");
    assert_eq!(tree_select.borrow().as_slice(), ["asst-1"]);
    assert!(constructor_select.borrow().is_empty());

    // 覆盖 TreeList.onCancel。
    let tree_cancel = Rc::new(RefCell::new(0usize));
    {
        let calls = Rc::clone(&tree_cancel);
        selector.get_tree_list().on_cancel = Some(Box::new(move || *calls.borrow_mut() += 1));
    }
    selector.handle_input("\x1b");
    assert_eq!(*tree_cancel.borrow(), 1);
    assert_eq!(*constructor_cancel.borrow(), 0);

    // 覆盖 TreeList.onCopy 优先于组件公开的 onCopy 字段。
    let component_copy = Rc::new(RefCell::new(Vec::new()));
    {
        let calls = Rc::clone(&component_copy);
        selector.on_copy = Some(Box::new(move |text: Option<&str>| {
            calls.borrow_mut().push(format!("component:{text:?}"));
        }));
    }
    let tree_copy = Rc::new(RefCell::new(Vec::new()));
    {
        let calls = Rc::clone(&tree_copy);
        selector.get_tree_list().on_copy = Some(Box::new(move |text: Option<&str>| {
            calls.borrow_mut().push(format!("tree:{text:?}"));
        }));
    }
    selector.handle_input("\x18"); // ctrl+x: app.message.copy
    assert_eq!(
        tree_copy.borrow().as_slice(),
        ["tree:Some(\"assistant text\")"]
    );
    assert!(component_copy.borrow().is_empty());

    // 覆盖 TreeList.onLabelEdit：不应再打开内置的 label 输入框。
    let tree_label = Rc::new(RefCell::new(Vec::new()));
    {
        let calls = Rc::clone(&tree_label);
        selector.get_tree_list().on_label_edit =
            Some(Box::new(move |id: &str, label: Option<&str>| {
                calls.borrow_mut().push(format!("{id}:{label:?}"));
            }));
    }
    selector.handle_input("L"); // shift+l: app.tree.editLabel
    assert_eq!(tree_label.borrow().as_slice(), ["asst-1:None"]);
    let rendered = strip_ansi(&selector.render(100).join("\n"));
    assert!(!rendered.contains("Label (empty to remove)"), "{rendered}");
}

/// TS 直接读原始 JSON：缺失 `replacement` 键（undefined）按 "replace"，显式 `null` 才是
/// "omit"；缺失 `tokensBefore` 时 `Math.round(undefined / 1000)` 是 NaN。
#[test]
fn missing_optional_fields_use_js_undefined_semantics() {
    use kiana_session_core::{CompactionEntry, CompactionType};

    setup();
    let mut edit_raw = JsonObject::new();
    edit_raw.insert("type".to_string(), serde_json::json!("context_edit"));
    edit_raw.insert("id".to_string(), serde_json::json!("edit-1"));
    edit_raw.insert("parentId".to_string(), serde_json::json!("user-1"));
    edit_raw.insert("targetId".to_string(), serde_json::json!("tgt"));
    let edit = SessionEntry::ContextEdit(ContextEditEntry {
        kind: ContextEditType::Marker,
        id: "edit-1".to_string(),
        parent_id: Some("user-1".to_string()),
        timestamp: timestamp(),
        target_id: "tgt".to_string(),
        replacement: None,
        raw_entry: Some(edit_raw),
    });

    let mut compaction_raw = JsonObject::new();
    compaction_raw.insert("type".to_string(), serde_json::json!("compaction"));
    compaction_raw.insert("id".to_string(), serde_json::json!("comp-1"));
    compaction_raw.insert("parentId".to_string(), serde_json::json!("edit-1"));
    let compaction = SessionEntry::Compaction(CompactionEntry {
        kind: CompactionType::Marker,
        id: "comp-1".to_string(),
        parent_id: Some("edit-1".to_string()),
        timestamp: timestamp(),
        summary: String::new(),
        first_kept_entry_id: String::new(),
        tokens_before: 0.0,
        details: None,
        usage: None,
        from_hook: None,
        system_message: None,
        first_kept_entry_index: None,
        raw_entry: Some(compaction_raw),
    });

    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        edit,
        compaction,
    ]);
    let mut selector = TreeSelectorComponent::new(
        tree,
        Some("comp-1".to_string()),
        24,
        noop_select(),
        noop_cancel(),
        None,
        None,
        None,
    );
    let rendered = strip_ansi(&selector.render(100).join("\n"));
    assert!(rendered.contains("[compaction: NaNk tokens]"), "{rendered}");

    selector.handle_input("\x01"); // ctrl+a: all
    let rendered = strip_ansi(&selector.render(100).join("\n"));
    assert!(rendered.contains("[context replace: tgt]"), "{rendered}");
    assert!(!rendered.contains("[context omit: tgt]"), "{rendered}");
}
