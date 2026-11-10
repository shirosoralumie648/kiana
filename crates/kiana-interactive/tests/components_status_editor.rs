//! 状态指示器与自定义编辑器的 TS 对拍测试。
//!
//! 移植自：
//! - `packages/coding-agent/test/status-indicator.test.ts`
//! - `packages/coding-agent/test/custom-editor-history-keybindings.test.ts`

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_config::keybindings::{KeyIdList, KeybindingsConfig, KeybindingsManager};
use kiana_theme::theme::get_editor_theme;
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::editor::EditorTui;
use kiana_tui::components::loader::LoaderIndicatorOptions;
use kiana_tui::components::text_component::StyleFn;
use kiana_tui::text::visible_width;
use kiana_tui::tui::{Component, Timer};

use kiana_interactive::components::custom_editor::{CustomEditor, CustomEditorOptions};
use kiana_interactive::components::status_indicator::{
    BranchSummaryStatusIndicator, CompactionStatusIndicator, CompactionStatusReason, IdleStatus,
    RetryStatusIndicator, StatusIndicator, WorkingStatusIndicator,
};

use crate::common::TestTerminal;

/// 待触发的定时器回调（测试用）。
type PendingCallback = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;

/// 可手动推进的定时器（TS 的 fake timers）。
struct FakeTimer {
    pending: PendingCallback,
}

impl Timer for FakeTimer {
    fn set(&mut self, _delay_ms: u64, callback: Box<dyn FnOnce()>) {
        *self.pending.borrow_mut() = Some(callback);
    }

    fn cancel(&mut self) {
        *self.pending.borrow_mut() = None;
    }
}

fn advance(pending: &PendingCallback) {
    let callback = pending.borrow_mut().take();
    if let Some(callback) = callback {
        callback();
    }
}

fn golden(key: &str) -> Vec<String> {
    let raw = include_str!("fixtures/render_goldens_dark.json");
    let value: serde_json::Value = serde_json::from_str(raw).expect("golden json");
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("missing golden {key}"))
        .iter()
        .map(|line| line.as_str().unwrap().to_string())
        .collect()
}

fn golden_string(key: &str) -> String {
    let raw = include_str!("fixtures/render_goldens_dark.json");
    let value: serde_json::Value = serde_json::from_str(raw).expect("golden json");
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("missing golden {key}"))
        .to_string()
}

fn golden_map(key: &str) -> serde_json::Map<String, serde_json::Value> {
    let raw = include_str!("fixtures/render_goldens_dark.json");
    let value: serde_json::Value = serde_json::from_str(raw).expect("golden json");
    value[key].as_object().expect("golden object").clone()
}

fn new_editor(embed_working_status: bool) -> CustomEditor {
    new_editor_sized(embed_working_status, 80, 24)
}

fn new_editor_sized(embed_working_status: bool, columns: usize, rows: usize) -> CustomEditor {
    let terminal = TestTerminal::with_size(columns, rows);
    let editor_tui = EditorTui::from_terminal(Rc::new(RefCell::new(
        Box::new(terminal) as Box<dyn kiana_tui::Terminal>
    )));
    let keybindings = Arc::new(KeybindingsManager::new(KeybindingsConfig::new(), None));
    CustomEditor::new(
        editor_tui,
        get_editor_theme(),
        keybindings,
        CustomEditorOptions {
            padding_x: None,
            autocomplete_max_visible: None,
            embed_working_status,
        },
    )
}

#[test]
fn keeps_idle_status_at_the_same_height_as_standalone_status_indicators() {
    let mut idle = IdleStatus::new();
    let lines = idle.render(20);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines, vec![" ".repeat(20), " ".repeat(20)]);
}

#[test]
fn keeps_the_top_border_unchanged_unless_the_editor_opts_in() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut editor = new_editor(false);
    let mut indicator = WorkingStatusIndicator::new(None, "Working", None, None);
    let standalone_line = indicator.render(20)[1].clone();
    assert!(
        standalone_line.contains(
            &kiana_theme::THEME
                .get_fg_ansi(ThemeColor::Accent)
                .expect("theme")
        )
    );
    assert!(
        standalone_line.contains(
            &kiana_theme::THEME
                .get_fg_ansi(ThemeColor::Muted)
                .expect("theme")
        )
    );
    editor.set_working_status_indicator(Some(Box::new(indicator)));
    assert_eq!(
        kiana_core::ansi::strip_ansi(&editor.render(20)[0]),
        "─".repeat(20)
    );
    assert_eq!(editor.render(20)[0], golden_string("editor_no_embed"));
}

#[test]
fn embeds_the_working_indicator_when_the_editor_opts_in() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut editor = new_editor(true);
    assert!(editor.embed_working_status());
    let border: Arc<dyn Fn(&str) -> String + Send + Sync> = Arc::from(
        kiana_theme::THEME
            .current()
            .expect("theme")
            .get_thinking_border_color("high"),
    );
    editor.set_border_color_fn(Arc::clone(&border));
    let border_for_indicator = Arc::clone(&border);
    let color_fn: StyleFn = Arc::new(move |text: &str| border_for_indicator(text));
    editor.set_working_status_indicator(Some(Box::new(WorkingStatusIndicator::new(
        None,
        "Working",
        None,
        Some(color_fn),
    ))));

    let top_border = editor.render(20)[0].clone();
    assert_eq!(
        kiana_core::ansi::strip_ansi(&top_border),
        "── ⠋ Working ───────"
    );
    assert_eq!(visible_width(&top_border), 20);
    assert_eq!(
        top_border
            .split(
                &kiana_theme::THEME
                    .get_fg_ansi(ThemeColor::ThinkingHigh)
                    .expect("theme")
            )
            .count(),
        5
    );

    let goldens = golden_map("editor_borders");
    for (width, key) in [
        (20, "w20"),
        (80, "w80"),
        (120, "w120"),
        (4, "w4"),
        (10, "w10"),
        (1, "w1"),
    ] {
        assert_eq!(
            editor.render(width)[0],
            goldens[key].as_str().expect("golden border").to_string(),
            "border width {width}"
        );
    }
}

#[test]
fn embeds_compaction_summary_and_retry_labels_within_the_border_width() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut editor = new_editor(true);
    let labels = golden_map("status_labels");

    let working = WorkingStatusIndicator::new(None, "Working", None, None);
    drop(working);

    let indicators: Vec<(Box<dyn StatusIndicator>, &str)> = vec![
        (
            Box::new(CompactionStatusIndicator::new(
                None,
                CompactionStatusReason::Manual,
                None,
            )),
            "compaction_manual",
        ),
        (
            Box::new(CompactionStatusIndicator::new(
                None,
                CompactionStatusReason::Threshold,
                None,
            )),
            "compaction_threshold",
        ),
        (
            Box::new(CompactionStatusIndicator::new(
                None,
                CompactionStatusReason::Overflow,
                None,
            )),
            "compaction_overflow",
        ),
        (
            Box::new(BranchSummaryStatusIndicator::new(None)),
            "branch_summary",
        ),
    ];
    for (indicator, key) in indicators {
        let mut indicator = indicator;
        let label = kiana_core::ansi::strip_ansi(&indicator.render(120)[1])
            .trim()
            .to_string();
        editor.set_working_status_indicator(Some(indicator));
        assert!(
            kiana_core::ansi::strip_ansi(&editor.render(120)[0]).contains(&format!("── {label} ")),
            "{key}"
        );
        for width in [1, 4, 10, 20, 80, 120] {
            assert_eq!(
                visible_width(&editor.render(width)[0]),
                width,
                "{key} width {width}"
            );
        }
    }

    // Retry：假定时器推进 1 秒后文本变成 `in 2s`。
    let pending: PendingCallback = Rc::new(RefCell::new(None));
    let mut retry = RetryStatusIndicator::new(None, 1, 3, 3000.0);
    retry.set_countdown_timer(Box::new(FakeTimer {
        pending: Rc::clone(&pending),
    }));
    assert_eq!(
        kiana_core::ansi::strip_ansi(&retry.render(120)[1]).trim(),
        kiana_core::ansi::strip_ansi(labels["retry"].as_str().expect("retry golden")).trim()
    );
    editor.set_working_status_indicator(Some(Box::new(retry)));
    advance(&pending);
    assert!(kiana_core::ansi::strip_ansi(&editor.render(120)[0]).contains("Retrying (1/3) in 2s"));

    editor.set_working_status_indicator(None);
    assert_eq!(
        kiana_core::ansi::strip_ansi(&editor.render(120)[0]),
        "─".repeat(120)
    );
}

#[test]
fn disposes_retry_countdown_updates() {
    kiana_theme::init_theme(Some("dark"), false);
    let requests = Rc::new(AtomicUsize::new(0));
    let counter = Rc::clone(&requests);
    let mut retry = RetryStatusIndicator::new(
        Some(Rc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })),
        1,
        3,
        1000.0,
    );
    let pending: PendingCallback = Rc::new(RefCell::new(None));
    retry.set_countdown_timer(Box::new(FakeTimer {
        pending: Rc::clone(&pending),
    }));
    let calls_before_dispose = requests.load(Ordering::SeqCst);
    StatusIndicator::dispose(&mut retry);
    advance(&pending);
    assert_eq!(requests.load(Ordering::SeqCst), calls_before_dispose);
}

#[test]
fn retry_status_indicator_keeps_an_external_message_until_the_next_countdown_tick() {
    kiana_theme::init_theme(Some("dark"), false);
    let pending: PendingCallback = Rc::new(RefCell::new(None));
    let mut retry = RetryStatusIndicator::new(None, 1, 3, 3000.0);
    retry.set_countdown_timer(Box::new(FakeTimer {
        pending: Rc::clone(&pending),
    }));

    // TS 只在 CountdownTimer 的 tick 回调用 setMessage；外部设置的消息保持到下一次 tick。
    retry.set_message("custom message");
    assert!(
        kiana_core::ansi::strip_ansi(&retry.render(120)[1]).contains("custom message"),
        "render() must not overwrite a caller-supplied message"
    );
    assert!(
        kiana_core::ansi::strip_ansi(&retry.render_in_border(40)).contains("custom message"),
        "render_in_border() must not overwrite a caller-supplied message"
    );

    advance(&pending);
    assert!(
        kiana_core::ansi::strip_ansi(&retry.render(120)[1]).contains("Retrying (1/3) in 2s"),
        "the next tick applies the countdown text"
    );
}

#[test]
fn status_indicator_trait_exposes_indicator_and_timer_controls() {
    kiana_theme::init_theme(Some("dark"), false);
    let calls = Rc::new(AtomicUsize::new(0));
    let counter = Rc::clone(&calls);
    let request: kiana_tui::components::loader::RequestRenderCallback = Rc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let mut indicator: Box<dyn StatusIndicator> = Box::new(WorkingStatusIndicator::new(
        Some(request),
        "Working",
        None,
        None,
    ));

    // `activeStatusIndicator.setIndicator(options)`（扩展的 setWorkingIndicator 路径）。
    indicator.set_indicator(Some(LoaderIndicatorOptions {
        frames: Some(vec!["*".to_string(), "+".to_string()]),
        interval_ms: Some(50.0),
    }));
    let first = kiana_core::ansi::strip_ansi(&indicator.render(40)[1]);
    assert!(first.contains("* Working"), "custom frame used: {first:?}");

    // 动画节拍器：到期后 requestRender 并重新排程（TS 的 setInterval）。
    let pending: PendingCallback = Rc::new(RefCell::new(None));
    indicator.set_timer(Box::new(FakeTimer {
        pending: Rc::clone(&pending),
    }));
    let before = calls.load(Ordering::SeqCst);
    advance(&pending);
    assert!(
        calls.load(Ordering::SeqCst) > before,
        "timer requests a render"
    );
    assert!(pending.borrow().is_some(), "timer re-arms after firing");
}

#[test]
fn retry_status_indicator_exposes_its_countdown_through_the_trait() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut indicator: Box<dyn StatusIndicator> =
        Box::new(RetryStatusIndicator::new(None, 1, 3, 3000.0));
    let pending: PendingCallback = Rc::new(RefCell::new(None));
    indicator.set_countdown_timer(Box::new(FakeTimer {
        pending: Rc::clone(&pending),
    }));
    advance(&pending);
    assert!(
        kiana_core::ansi::strip_ansi(&indicator.render(120)[1]).contains("Retrying (1/3) in 2s")
    );
}

#[test]
fn status_indicator_kinds_and_standalone_rendering_match_the_ts_component() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut working = WorkingStatusIndicator::new(None, "Working", None, None);
    assert_eq!(
        working.kind(),
        kiana_interactive::components::status_indicator::StatusIndicatorKind::Working
    );
    assert_eq!(working.render(120), golden("status_working"));
    assert_eq!(
        working.render_in_border(20),
        golden("status_working_border")[0]
    );
    assert_eq!(
        working.render_spinner_in_border(20),
        golden("status_working_border")[1]
    );

    let mut idle = IdleStatus::new();
    assert_eq!(idle.render(20), golden("status_idle"));
}

#[test]
fn gives_an_explicit_history_binding_precedence_over_model_cycling() {
    let mut config = KeybindingsConfig::new();
    config.insert(
        "tui.editor.historyPrevious".to_string(),
        KeyIdList::from("ctrl+p"),
    );
    config.insert(
        "tui.editor.historyNext".to_string(),
        KeyIdList::from("ctrl+n"),
    );
    let bindings = config.clone();
    kiana_tui::set_keybindings(kiana_tui::keybindings::KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        bindings,
    ));
    let keybindings = Arc::new(KeybindingsManager::new(config, None));
    let terminal = TestTerminal::new();
    let editor_tui = EditorTui::from_terminal(Rc::new(RefCell::new(
        Box::new(terminal) as Box<dyn kiana_tui::Terminal>
    )));
    let mut editor = CustomEditor::new(
        editor_tui,
        get_editor_theme(),
        keybindings,
        CustomEditorOptions::default(),
    );
    let model_cycles = Rc::new(AtomicUsize::new(0));
    let counter = Rc::clone(&model_cycles);
    editor.set_action_handler(
        "app.model.cycleForward",
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    );
    editor.add_to_history("previous prompt");
    editor.set_text("draft");

    editor.handle_input("\x10"); // Ctrl+P
    assert_eq!(editor.get_text(), "previous prompt");
    assert_eq!(model_cycles.load(Ordering::SeqCst), 0);

    editor.handle_input("\x0e"); // Ctrl+N
    assert_eq!(editor.get_text(), "draft");
    kiana_tui::keybindings::reset_global_keybindings();
}

#[test]
fn custom_editor_action_handlers_keep_insertion_order_and_replace_in_place() {
    let mut config = KeybindingsConfig::new();
    config.insert(
        "app.model.cycleForward".to_string(),
        KeyIdList::from("ctrl+x"),
    );
    config.insert(
        "app.model.cycleBackward".to_string(),
        KeyIdList::from("ctrl+x"),
    );
    let keybindings = Arc::new(KeybindingsManager::new(config, None));
    let terminal = TestTerminal::new();
    let editor_tui = EditorTui::from_terminal(Rc::new(RefCell::new(
        Box::new(terminal) as Box<dyn kiana_tui::Terminal>
    )));
    let mut editor = CustomEditor::new(
        editor_tui,
        get_editor_theme(),
        keybindings,
        CustomEditorOptions::default(),
    );
    let calls = Rc::new(RefCell::new(Vec::<&'static str>::new()));
    let first = Rc::clone(&calls);
    editor.set_action_handler(
        "app.model.cycleForward",
        Box::new(move || first.borrow_mut().push("first")),
    );
    let second = Rc::clone(&calls);
    editor.set_action_handler(
        "app.model.cycleBackward",
        Box::new(move || second.borrow_mut().push("second")),
    );
    // 同一键位命中两个动作：按插入顺序先到 cycleForward。
    editor.handle_input("\x18");
    assert_eq!(*calls.borrow(), vec!["first"]);

    // 同键替换保留插入位置：仍然第一个命中。
    let replacement = Rc::clone(&calls);
    editor.set_action_handler(
        "app.model.cycleForward",
        Box::new(move || replacement.borrow_mut().push("replacement")),
    );
    editor.handle_input("\x18");
    assert_eq!(*calls.borrow(), vec!["first", "replacement"]);
}

#[test]
fn editor_escape_and_exit_handlers_follow_the_ts_order() {
    let mut config = KeybindingsConfig::new();
    config.insert("app.interrupt".to_string(), KeyIdList::from("esc"));
    config.insert("app.exit".to_string(), KeyIdList::from("ctrl+d"));
    let keybindings = Arc::new(KeybindingsManager::new(config, None));
    let terminal = TestTerminal::new();
    let editor_tui = EditorTui::from_terminal(Rc::new(RefCell::new(
        Box::new(terminal) as Box<dyn kiana_tui::Terminal>
    )));
    let mut editor = CustomEditor::new(
        editor_tui,
        get_editor_theme(),
        keybindings,
        CustomEditorOptions::default(),
    );
    let escapes = Rc::new(RefCell::new(0usize));
    let slot = Rc::clone(&escapes);
    editor.set_on_escape(Some(Box::new(move || *slot.borrow_mut() += 1)));
    editor.handle_input("\x1b");
    assert_eq!(*escapes.borrow(), 1);

    let exits = Rc::new(RefCell::new(0usize));
    let slot = Rc::clone(&exits);
    editor.set_on_ctrl_d(Some(Box::new(move || *slot.borrow_mut() += 1)));
    editor.handle_input("\x04");
    assert_eq!(*exits.borrow(), 1);
    // 文本非空时 Ctrl+D 不触发退出。
    editor.set_text("abc");
    editor.handle_input("\x04");
    assert_eq!(*exits.borrow(), 1);
}

#[test]
fn embeds_status_with_scroll_overflow_labels_and_spinner_fallback() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut editor = new_editor_sized(true, 80, 10);
    let border: Arc<dyn Fn(&str) -> String + Send + Sync> = Arc::from(
        kiana_theme::THEME
            .current()
            .expect("theme")
            .get_thinking_border_color("high"),
    );
    editor.set_border_color_fn(Arc::clone(&border));
    let border_for_indicator = Arc::clone(&border);
    let color_fn: StyleFn = Arc::new(move |text: &str| border_for_indicator(text));
    editor.set_working_status_indicator(Some(Box::new(WorkingStatusIndicator::new(
        None,
        "Working",
        None,
        Some(color_fn),
    ))));
    editor.set_text(
        &(0..100)
            .map(|index| format!("line {index}"))
            .collect::<Vec<String>>()
            .join("\n"),
    );
    let goldens = golden_map("hidden_borders");
    for width in [120, 80, 40, 20, 14, 13, 12, 10, 8, 4, 1] {
        assert_eq!(
            editor.render(width)[0],
            goldens[&format!("w{width}")].as_str().expect("golden"),
            "hidden border width {width}"
        );
    }

    let border_for_indicator = Arc::clone(&border);
    let color_fn: StyleFn = Arc::new(move |text: &str| border_for_indicator(text));
    editor.set_working_status_indicator(Some(Box::new(WorkingStatusIndicator::new(
        None,
        "A very long working message that overflows",
        None,
        Some(color_fn),
    ))));
    for width in [120, 80, 40, 20, 10, 4, 1] {
        assert_eq!(
            editor.render(width)[0],
            goldens[&format!("long_w{width}")].as_str().expect("golden"),
            "long hidden border width {width}"
        );
    }
}
