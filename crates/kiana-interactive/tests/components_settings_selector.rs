//! `components/settings-selector.ts` 的移植测试。
//!
//! `settings-selector.test.ts` 的 5 个用例逐条移植；另外附上从 TS 参考实现
//! （`/home/ctyun/kiana-work/upstream-reference`，`FORCE_COLOR=3` 探针）生成的 120 列黄金渲染。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Once;

use indexmap::IndexMap;
use kiana_ai::{Model, ModelThinkingLevel};
use kiana_config::settings::{
    DefaultProjectTrust, FullscreenExitOutput, FullscreenScrollbar, QuietStartup,
    SessionShareDestination, SteeringMode, WarningSettings,
};
use kiana_interactive::components::settings_selector::{
    SettingsCallbacks, SettingsConfig, SettingsSelectorComponent,
};
use kiana_interactive::components::settings_submenu::{
    SteppedSubmenu, SteppedSubmenuOptions, SteppedSubmenuStep,
};
use kiana_tui::keybindings::{KeybindingsConfig, KeybindingsManager};
use kiana_tui::{
    Component, SelectItem, TerminalCapabilities, TuiMouseButton, TuiMouseEvent, TuiMouseEventType,
    WheelScrollLines, set_capabilities, set_keybindings,
};

/// 初始化主题（进程级一次）、能力与键位（线程级）。
fn setup() {
    static THEME_ONCE: Once = Once::new();
    THEME_ONCE.call_once(|| kiana_theme::init_theme(Some("dark"), false));
    set_capabilities(TerminalCapabilities {
        images: None,
        true_color: true,
        hyperlinks: false,
    });
    set_keybindings(KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        KeybindingsConfig::new(),
    ));
}

fn assert_lines(actual: Vec<String>, expected: &[&str]) {
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_string()).collect();
    assert_eq!(actual, expected);
}

fn log_strings(cell: &Rc<RefCell<Vec<String>>>) -> Vec<String> {
    cell.borrow().clone()
}

fn probe_config() -> SettingsConfig {
    let model = Model {
        id: "thinking-model".to_string(),
        name: "test".to_string(),
        provider: "test-provider".to_string(),
        reasoning: Some(true),
        ..Model::default()
    };
    let model_key = "test-provider/thinking-model".to_string();
    let mut model_thinking_levels = IndexMap::new();
    model_thinking_levels.insert(model_key.clone(), ModelThinkingLevel::Medium);
    SettingsConfig {
        auto_compact: true,
        default_model: model_key,
        current_model: Some(model.clone()),
        available_default_models: vec![model],
        show_images: true,
        image_width_cells: 80.0,
        auto_resize_images: true,
        block_images: false,
        enable_skill_commands: true,
        steering_mode: SteeringMode::OneAtATime,
        follow_up_mode: SteeringMode::OneAtATime,
        transport: kiana_ai::Transport::Sse,
        session_share_destination: SessionShareDestination::Auto,
        http_idle_timeout_ms: 30_000.0,
        cache_warming_mode: kiana_config::settings::CacheWarmingMode::Streaming,
        thinking_level: ModelThinkingLevel::High,
        available_thinking_levels: vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Minimal,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
        ],
        model_thinking_levels,
        current_theme: "dark".to_string(),
        ui_locale: "en".to_string(),
        terminal_theme: kiana_tui::TerminalColorScheme::Dark,
        available_themes: vec![
            "system".to_string(),
            "dark".to_string(),
            "light".to_string(),
        ],
        hide_thinking_block: false,
        mermaid_rendering_mode: kiana_config::settings::MermaidRenderingMode::Streaming,
        show_cache_miss_notices: false,
        collapse_changelog: false,
        enable_install_telemetry: true,
        double_escape_action: kiana_config::settings::DoubleEscapeAction::Tree,
        tree_filter_mode: kiana_config::settings::TreeFilterMode::Default,
        show_hardware_cursor: false,
        editor_padding_x: 0.0,
        output_pad: 1,
        autocomplete_max_visible: 5.0,
        quiet_startup: QuietStartup::Boolean(false),
        default_project_trust: DefaultProjectTrust::Ask,
        clear_on_shrink: false,
        show_terminal_progress: false,
        tui_mode: kiana_config::settings::TuiMode::Fullscreen,
        fullscreen_exit_output: FullscreenExitOutput::Transcript,
        fullscreen_scrollbar: FullscreenScrollbar::Auto,
        fullscreen_copy_on_select: true,
        fullscreen_wheel_scroll_lines: WheelScrollLines::Fixed(7.0),
        warnings: WarningSettings::default(),
    }
}

#[test]
fn cycles_through_fullscreen_settings() {
    setup();
    let config = SettingsConfig {
        fullscreen_exit_output: FullscreenExitOutput::Transcript,
        fullscreen_scrollbar: FullscreenScrollbar::Auto,
        fullscreen_copy_on_select: true,
        fullscreen_wheel_scroll_lines: WheelScrollLines::Fixed(7.0),
        ..SettingsConfig::default()
    };

    let exit_log = Rc::new(RefCell::new(Vec::new()));
    let scrollbar_log = Rc::new(RefCell::new(Vec::new()));
    let copy_log = Rc::new(RefCell::new(Vec::new()));
    let wheel_log = Rc::new(RefCell::new(Vec::new()));

    let callbacks = SettingsCallbacks {
        on_fullscreen_exit_output_change: {
            let log = Rc::clone(&exit_log);
            Rc::new(move |value: FullscreenExitOutput| {
                log.borrow_mut().push(
                    match value {
                        FullscreenExitOutput::Transcript => "transcript",
                        FullscreenExitOutput::ResumeHint => "resume-hint",
                    }
                    .to_string(),
                );
            })
        },
        on_fullscreen_scrollbar_change: {
            let log = Rc::clone(&scrollbar_log);
            Rc::new(move |value: FullscreenScrollbar| {
                log.borrow_mut().push(
                    match value {
                        FullscreenScrollbar::Auto => "auto",
                        FullscreenScrollbar::Always => "always",
                        FullscreenScrollbar::Hidden => "hidden",
                    }
                    .to_string(),
                );
            })
        },
        on_fullscreen_copy_on_select_change: {
            let log = Rc::clone(&copy_log);
            Rc::new(move |value: bool| log.borrow_mut().push(value.to_string()))
        },
        on_fullscreen_wheel_scroll_lines_change: {
            let log = Rc::clone(&wheel_log);
            Rc::new(move |value: WheelScrollLines| {
                log.borrow_mut().push(match value {
                    WheelScrollLines::Auto => "auto".to_string(),
                    WheelScrollLines::Fixed(lines) => format!("{}", lines as i64),
                })
            })
        },
        ..SettingsCallbacks::default()
    };

    let cycle = |label: &str, count: usize| {
        let mut component = SettingsSelectorComponent::new(config.clone(), callbacks.clone());
        let list = component.get_settings_list();
        for character in label.chars() {
            list.handle_input(&character.to_string());
        }
        for _ in 0..count {
            list.handle_input("\r");
        }
    };

    cycle("Fullscreen exit output", 2);
    assert_eq!(log_strings(&exit_log), vec!["resume-hint", "transcript"]);
    cycle("Fullscreen scrollbar", 3);
    assert_eq!(
        log_strings(&scrollbar_log),
        vec!["always", "hidden", "auto"]
    );
    cycle("Fullscreen copy on select", 2);
    assert_eq!(log_strings(&copy_log), vec!["false", "true"]);
    // #9758: settings.json 里的自定义值保留在循环里。
    cycle("Fullscreen wheel scrolling", 3);
    assert_eq!(log_strings(&wheel_log), vec!["10", "auto", "1"]);
}

#[test]
fn cycles_through_share_destinations() {
    setup();
    let config = SettingsConfig {
        session_share_destination: SessionShareDestination::Auto,
        ..SettingsConfig::default()
    };
    let log = Rc::new(RefCell::new(Vec::new()));
    let callbacks = SettingsCallbacks {
        on_session_share_destination_change: {
            let log = Rc::clone(&log);
            Rc::new(move |value: SessionShareDestination| {
                log.borrow_mut().push(
                    match value {
                        SessionShareDestination::Auto => "auto",
                        SessionShareDestination::Gist => "gist",
                        SessionShareDestination::Radius => "radius",
                    }
                    .to_string(),
                );
            })
        },
        ..SettingsCallbacks::default()
    };
    let mut component = SettingsSelectorComponent::new(config, callbacks);
    let list = component.get_settings_list();
    list.select_item("share-destination");
    for _ in 0..3 {
        list.handle_input("\r");
    }
    assert_eq!(log_strings(&log), vec!["gist", "radius", "auto"]);
}

#[test]
fn language_item_displays_names_and_reports_locale_codes() {
    setup();
    let codes = Rc::new(RefCell::new(Vec::<String>::new()));
    let callbacks = SettingsCallbacks {
        on_ui_locale_change: {
            let codes = Rc::clone(&codes);
            Rc::new(move |code: &str| codes.borrow_mut().push(code.to_string()))
        },
        ..SettingsCallbacks::default()
    };
    let config = SettingsConfig {
        ui_locale: "en".to_string(),
        ..SettingsConfig::default()
    };
    let cycle = |label: &str, count: usize| {
        let mut component = SettingsSelectorComponent::new(config.clone(), callbacks.clone());
        let list = component.get_settings_list();
        for character in label.chars() {
            list.handle_input(&character.to_string());
        }
        for _ in 0..count {
            list.handle_input("\r");
        }
    };

    // 显示名是 English / 简体中文，回调只收到代码。
    cycle("Language", 1);
    assert_eq!(log_strings(&codes), vec!["zh"]);
    cycle("Language", 2);
    assert_eq!(log_strings(&codes), vec!["zh", "zh", "en"]);
}

#[test]
fn keeps_the_configured_fixed_theme_marked_while_browsing() {
    setup();
    let config = SettingsConfig {
        current_theme: "dark".to_string(),
        terminal_theme: kiana_tui::TerminalColorScheme::Dark,
        available_themes: vec![
            "system".to_string(),
            "dark".to_string(),
            "light".to_string(),
        ],
        ..SettingsConfig::default()
    };
    let mut component = SettingsSelectorComponent::new(config, SettingsCallbacks::default());
    let list = component.get_settings_list();
    list.select_item("theme");
    list.handle_input("\r");

    let output = kiana_tui::strip_terminal_sequences(&list.render(120).join("\n"));
    let system_line = output
        .lines()
        .find(|line| line.contains("system"))
        .expect("system theme visible");
    assert!(system_line.contains("Theme created from your terminal's colors"));
    assert!(output.contains("\n    system"));
    assert!(output.contains("\n    automatic"));
    assert!(output.contains("→ ✓ dark"));

    list.handle_input("\x1b[B");
    let output = kiana_tui::strip_terminal_sequences(&list.render(120).join("\n"));
    assert!(output.contains("  ✓ dark"));
    assert!(output.contains("→   light"));
}

#[test]
fn keeps_a_configured_automatic_theme_marked_while_browsing() {
    setup();
    let config = SettingsConfig {
        current_theme: "light/dark".to_string(),
        terminal_theme: kiana_tui::TerminalColorScheme::Dark,
        available_themes: vec!["dark".to_string(), "light".to_string(), "other".to_string()],
        ..SettingsConfig::default()
    };
    let mut component = SettingsSelectorComponent::new(config, SettingsCallbacks::default());
    let list = component.get_settings_list();
    list.select_item("theme");
    list.handle_input("\r");
    list.handle_input("\r");

    let output = kiana_tui::strip_terminal_sequences(&list.render(120).join("\n"));
    assert!(output.contains("→ ✓ light"));

    list.handle_input("\x1b[B");
    let output = kiana_tui::strip_terminal_sequences(&list.render(120).join("\n"));
    assert!(output.contains("  ✓ light"));
    assert!(output.contains("→   other"));
}

#[test]
fn keeps_the_configured_per_model_thinking_level_marked_while_browsing() {
    setup();
    let model = Model {
        id: "thinking-model".to_string(),
        provider: "test-provider".to_string(),
        reasoning: Some(true),
        ..Model::default()
    };
    let model_key = "test-provider/thinking-model".to_string();
    let mut model_thinking_levels = IndexMap::new();
    model_thinking_levels.insert(model_key.clone(), ModelThinkingLevel::Medium);
    let config = SettingsConfig {
        default_model: model_key,
        available_default_models: vec![model],
        thinking_level: ModelThinkingLevel::High,
        model_thinking_levels,
        ..SettingsConfig::default()
    };
    let mut component = SettingsSelectorComponent::new(config, SettingsCallbacks::default());
    let list = component.get_settings_list();
    list.select_item("model-thinking");
    list.handle_input("\r");
    list.handle_input("\r");

    let output = kiana_tui::strip_terminal_sequences(&list.render(120).join("\n"));
    assert!(output.contains("→ ✓ medium"));
    assert!(output.contains("    (clear override)"));

    list.handle_input("\x1b[B");
    let output = kiana_tui::strip_terminal_sequences(&list.render(120).join("\n"));
    assert!(output.contains("  ✓ medium"));
    assert!(output.contains("→   high"));
}

#[test]
fn theme_automatic_apply_saves_the_automatic_setting() {
    setup();
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let callbacks = SettingsCallbacks {
        on_theme_change: {
            let log = Rc::clone(&log);
            Rc::new(move |value: &str| log.borrow_mut().push(value.to_string()))
        },
        ..SettingsCallbacks::default()
    };
    let config = SettingsConfig {
        current_theme: "light/dark".to_string(),
        terminal_theme: kiana_tui::TerminalColorScheme::Dark,
        available_themes: vec!["dark".to_string(), "light".to_string(), "other".to_string()],
        ..SettingsConfig::default()
    };
    let mut component = SettingsSelectorComponent::new(config, callbacks);
    let list = component.get_settings_list();
    list.select_item("theme");
    list.handle_input("\r");
    // Light theme (0) → Dark theme (1) → Apply (2)。
    list.handle_input("\x1b[B");
    list.handle_input("\x1b[B");
    list.handle_input("\r");
    assert_eq!(*log.borrow(), vec!["light/dark".to_string()]);
}

#[test]
fn theme_automatic_change_mode_returns_to_single_menu() {
    setup();
    let previews = Rc::new(RefCell::new(Vec::<String>::new()));
    let callbacks = SettingsCallbacks {
        on_theme_preview: {
            let previews = Rc::clone(&previews);
            Some(Rc::new(move |value: &str| {
                previews.borrow_mut().push(value.to_string())
            }))
        },
        ..SettingsCallbacks::default()
    };
    let config = SettingsConfig {
        current_theme: "light/dark".to_string(),
        terminal_theme: kiana_tui::TerminalColorScheme::Dark,
        available_themes: vec!["dark".to_string(), "light".to_string(), "other".to_string()],
        ..SettingsConfig::default()
    };
    let mut component = SettingsSelectorComponent::new(config, callbacks);
    let list = component.get_settings_list();
    list.select_item("theme");
    list.handle_input("\r");
    // Change mode 是第 4 项（0..3）。
    list.handle_input("\x1b[B");
    list.handle_input("\x1b[B");
    list.handle_input("\x1b[B");
    list.handle_input("\r");
    let output = kiana_tui::strip_terminal_sequences(&list.render(120).join("\n"));
    assert!(output.contains("→ ✓ dark"));
    assert!(output.contains("automatic"));
    assert_eq!(previews.borrow().last().map(String::as_str), Some("dark"));
}

#[test]
fn theme_automatic_light_theme_selection_updates_and_previews() {
    setup();
    let previews = Rc::new(RefCell::new(Vec::<String>::new()));
    let callbacks = SettingsCallbacks {
        on_theme_preview: {
            let previews = Rc::clone(&previews);
            Some(Rc::new(move |value: &str| {
                previews.borrow_mut().push(value.to_string())
            }))
        },
        ..SettingsCallbacks::default()
    };
    let config = SettingsConfig {
        current_theme: "light/dark".to_string(),
        terminal_theme: kiana_tui::TerminalColorScheme::Dark,
        available_themes: vec!["dark".to_string(), "light".to_string(), "other".to_string()],
        ..SettingsConfig::default()
    };
    let mut component = SettingsSelectorComponent::new(config, callbacks);
    let list = component.get_settings_list();
    list.select_item("theme");
    list.handle_input("\r");
    list.handle_input("\r");
    // Light theme 子菜单：当前 light → other。
    list.handle_input("\x1b[B");
    list.handle_input("\r");
    assert_eq!(
        previews.borrow().last().map(String::as_str),
        Some("other/dark")
    );
}

#[test]
fn golden_theme_automatic_narrow_matches_ts_reference() {
    setup();
    let mut config = probe_config();
    config.current_theme = "light/dark".to_string();
    let mut component = SettingsSelectorComponent::new(config, SettingsCallbacks::default());
    let list = component.get_settings_list();
    list.select_item("theme");
    list.handle_input("\r");
    // current_theme 是自动值 → 直接进入 Automatic Theme 面板。
    assert_lines(list.render(40), GOLDEN_THEME_AUTOMATIC_40);
}

/// 构造一个左键 press 事件（`SettingsList`/`SelectList` 的鼠标测试都用 press 选中行）。
fn mouse_press(x: isize, y: isize, width: usize, height: usize) -> TuiMouseEvent {
    TuiMouseEvent {
        event_type: TuiMouseEventType::Press,
        button: TuiMouseButton::Left,
        x,
        y,
        screen_x: x,
        screen_y: y,
        width,
        height,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: None,
    }
}

#[test]
fn automatic_theme_mouse_accounts_for_wrapped_header() {
    setup();
    let previews = Rc::new(RefCell::new(Vec::<String>::new()));
    let callbacks = SettingsCallbacks {
        on_theme_preview: {
            let previews = Rc::clone(&previews);
            Some(Rc::new(move |value: &str| {
                previews.borrow_mut().push(value.to_string())
            }))
        },
        ..SettingsCallbacks::default()
    };
    let mut config = probe_config();
    config.current_theme = "light/dark".to_string();
    let mut component = SettingsSelectorComponent::new(config, callbacks);
    let list = component.get_settings_list();
    list.select_item("theme");
    list.handle_input("\r");

    let lines = list.render(40);
    // TS 参考实现（probe_auto_mouse.ts）：40 列时两条 muted 说明各换行成 2 行，
    // 头部共 7 行；「Light theme」行下标是 7。旧实现用固定 5 行偏移，点击会错位。
    let light_row = lines
        .iter()
        .position(|line| kiana_tui::strip_terminal_sequences(line).contains("Light theme"))
        .expect("Light theme row is rendered");
    assert_eq!(light_row, 7);

    // 点击第 4 行（Change mode）必须命中它，而不是被 5 行固定偏移吃掉。
    let event = mouse_press(1, light_row as isize + 3, 40, lines.len());
    let result = list.handle_mouse(&event).expect("press is handled");
    assert_eq!(result.handled, Some(true));
    let after_press = kiana_tui::strip_terminal_sequences(&list.render(40).join("\n"));
    assert!(after_press.contains("→ Change mode"));

    list.handle_input("\r");
    // 切回单主题模式，预览当前自动模式激活的主题（terminalTheme = dark → dark）。
    assert_eq!(*previews.borrow(), vec!["dark".to_string()]);
    let output = kiana_tui::strip_terminal_sequences(&list.render(40).join("\n"));
    assert!(output.contains("Select a theme, or choose automatic"));
}

#[test]
fn stepped_submenu_ignores_mouse_events() {
    setup();
    let completed: Rc<RefCell<Vec<IndexMap<String, String>>>> = Rc::new(RefCell::new(Vec::new()));
    let completed_sink = Rc::clone(&completed);
    let options = vec![
        SelectItem::with_label("a", "A"),
        SelectItem::with_label("b", "B"),
        SelectItem::with_label("c", "C"),
    ];
    let options_for_step = options.clone();
    let step = SteppedSubmenuStep::new(
        "choice",
        "Pick",
        "Description",
        Rc::new(move |_context: &IndexMap<String, String>| options_for_step.clone()),
    );
    let mut submenu = SteppedSubmenu::new(
        vec![step],
        Rc::new(move |context: &IndexMap<String, String>| {
            completed_sink.borrow_mut().push(context.clone());
        }),
        Rc::new(|| {}),
        SteppedSubmenuOptions::default(),
    );

    let lines = submenu.render(80);
    // TS 的 SteppedSubmenu 从不 addChild(activeComponent)，Container 的 children 为空：
    // 鼠标事件无效，容器遍历也看不到当前步骤。
    let mut visited = 0usize;
    submenu.for_each_container_child(&mut |_child| {
        visited += 1;
        true
    });
    assert_eq!(visited, 0);
    // 布局：title / 空行 / description / 空行 / SelectList / 空行 / hint。
    let event = mouse_press(1, 4 + 2, 80, lines.len());
    // TS 的 SteppedSubmenu 从不把 activeComponent 作为 Container 子项，鼠标事件无效。
    assert_eq!(submenu.handle_mouse(&event), None);

    // 选中项仍是第一项（鼠标没有把选择挪到 C）。
    submenu.handle_input("\r");
    assert_eq!(completed.borrow().len(), 1);
    assert_eq!(
        completed.borrow()[0].get("choice").map(String::as_str),
        Some("a")
    );
}

#[test]
fn warnings_submenu_reports_toggles() {
    setup();
    let changes = Rc::new(RefCell::new(Vec::<Option<bool>>::new()));
    let callbacks = SettingsCallbacks {
        on_warnings_change: {
            let changes = Rc::clone(&changes);
            Rc::new(move |warnings: WarningSettings| {
                changes.borrow_mut().push(warnings.anthropic_extra_usage)
            })
        },
        ..SettingsCallbacks::default()
    };
    let mut component = SettingsSelectorComponent::new(probe_config(), callbacks);
    let list = component.get_settings_list();
    list.select_item("warnings");
    list.handle_input("\r");
    // 默认值 true → false。
    list.handle_input("\r");
    assert_eq!(*changes.borrow(), vec![Some(false)]);
}

#[test]
fn component_wraps_the_settings_list_with_dynamic_borders() {
    setup();
    let mut component =
        SettingsSelectorComponent::new(probe_config(), SettingsCallbacks::default());
    let list_lines = component.get_settings_list().render(120);
    let component_lines = component.render(120);
    assert_eq!(component_lines.len(), list_lines.len() + 2);
    assert_eq!(
        &component_lines[1..1 + list_lines.len()],
        list_lines.as_slice()
    );
    assert_eq!(
        component_lines[0],
        component_lines[component_lines.len() - 1]
    );
}

/// 回归：TS 宿主用 `focus: selector.getSettingsList()` 把焦点放在**内部**列表上
/// （`interactive-mode.ts:5137`）。Rust 侧只能通过 `FocusTarget::Path` 下钻，
/// 因此组件必须同时实现只读与可变容器遍历（`kiana-tui/src/tui.rs` 的 Component 契约）；
/// 缺少可变版本时按键在下钻阶段就被丢弃。
#[test]
fn container_traversal_exposes_the_settings_list_for_path_focus() {
    setup();
    let mut component =
        SettingsSelectorComponent::new(probe_config(), SettingsCallbacks::default());

    let mut read_children = 0usize;
    component.for_each_container_child(&mut |_child| {
        read_children += 1;
        true
    });
    assert_eq!(read_children, 1);

    let mut mutable_children = 0usize;
    component.for_each_container_child_mut(&mut |child| {
        mutable_children += 1;
        // 焦点路径送达的按键必须作用到内部 SettingsList（光标下移一项）。
        child.handle_input("\x1b[B");
        false
    });
    assert_eq!(mutable_children, 1);
    assert_eq!(component.get_settings_list().selected_index(), 1);
}

// ---------------------------------------------------------------------------
// TS 探针黄金渲染（80/120 列，raw ANSI）
// ---------------------------------------------------------------------------

#[test]
fn golden_main_list_matches_ts_reference() {
    setup();
    let mut component =
        SettingsSelectorComponent::new(probe_config(), SettingsCallbacks::default());
    let lines = component.get_settings_list().render(120);
    assert_lines(lines, GOLDEN_SETTINGS_MAIN_120);
}

#[test]
fn golden_theme_single_matches_ts_reference() {
    setup();
    let mut component =
        SettingsSelectorComponent::new(probe_config(), SettingsCallbacks::default());
    let list = component.get_settings_list();
    list.select_item("theme");
    list.handle_input("\r");
    assert_lines(list.render(120), GOLDEN_THEME_SINGLE_120);
}

#[test]
fn golden_theme_automatic_matches_ts_reference() {
    setup();
    let mut component =
        SettingsSelectorComponent::new(probe_config(), SettingsCallbacks::default());
    let list = component.get_settings_list();
    list.select_item("theme");
    list.handle_input("\r");
    // dark → light → system → automatic（SelectList 环绕）。
    list.handle_input("\x1b[B");
    list.handle_input("\x1b[B");
    list.handle_input("\x1b[B");
    list.handle_input("\r");
    assert_lines(list.render(120), GOLDEN_THEME_AUTOMATIC_120);

    list.handle_input("\r");
    assert_lines(list.render(120), GOLDEN_THEME_AUTOMATIC_LIGHT_SUBMENU_120);
}

#[test]
fn golden_warnings_submenu_matches_ts_reference() {
    setup();
    let mut component =
        SettingsSelectorComponent::new(probe_config(), SettingsCallbacks::default());
    let list = component.get_settings_list();
    list.select_item("warnings");
    list.handle_input("\r");
    assert_lines(list.render(120), GOLDEN_WARNINGS_120);
}

#[test]
fn golden_model_thinking_steps_match_ts_reference() {
    setup();
    let mut component =
        SettingsSelectorComponent::new(probe_config(), SettingsCallbacks::default());
    let list = component.get_settings_list();
    list.select_item("model-thinking");
    list.handle_input("\r");
    assert_lines(list.render(120), GOLDEN_MODEL_STEP1_120);
    list.handle_input("\r");
    assert_lines(list.render(120), GOLDEN_MODEL_STEP2_120);
}

#[test]
fn golden_model_thinking_step_without_override_matches_ts_reference() {
    setup();
    let mut config = probe_config();
    config.model_thinking_levels = IndexMap::new();
    let mut component = SettingsSelectorComponent::new(config, SettingsCallbacks::default());
    let list = component.get_settings_list();
    list.select_item("model-thinking");
    list.handle_input("\r");
    list.handle_input("\r");
    assert_lines(list.render(120), GOLDEN_MODEL_STEP2_NO_OVERRIDE_120);
}

#[test]
fn step_label_and_thinking_descriptions_are_verbatim() {
    setup();
    let mut component =
        SettingsSelectorComponent::new(probe_config(), SettingsCallbacks::default());
    let list = component.get_settings_list();
    list.select_item("model-thinking");
    list.handle_input("\r");
    let step1 = kiana_tui::strip_terminal_sequences(&list.render(120).join("\n"));
    assert!(step1.contains("Per-Model Thinking Level"));
    assert!(step1.contains("Step 1/2 · Select a model to configure"));
    list.handle_input("\r");
    let step2 = kiana_tui::strip_terminal_sequences(&list.render(120).join("\n"));
    assert!(step2.contains("Thinking Level for thinking-model [test-provider]"));
    assert!(step2.contains("Step 2/2 · Select default thinking level for this model"));
    assert!(step2.contains("Moderate reasoning (~8k tokens)"));
}
const GOLDEN_SETTINGS_MAIN_120: &[&str] = &[
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                                                                                                     ",
    "",
    "\x1b[38;2;167;152;215m→ \x1b[39m\x1b[38;2;167;152;215mAuto-compact                    \x1b[39m  \x1b[38;2;167;152;215mtrue\x1b[39m",
    "  Auto-resize images                \x1b[38;2;157;165;169mtrue\x1b[39m",
    "  Block images                      \x1b[38;2;157;165;169mfalse\x1b[39m",
    "  Skill commands                    \x1b[38;2;157;165;169mtrue\x1b[39m",
    "  Show hardware cursor              \x1b[38;2;157;165;169mfalse\x1b[39m",
    "  Editor padding                    \x1b[38;2;157;165;169m0\x1b[39m",
    "  Output padding                    \x1b[38;2;157;165;169m1\x1b[39m",
    "  Autocomplete max items            \x1b[38;2;157;165;169m5\x1b[39m",
    "  Clear on shrink                   \x1b[38;2;157;165;169mfalse\x1b[39m",
    "  Terminal progress                 \x1b[38;2;157;165;169mfalse\x1b[39m",
    "\x1b[38;2;126;136;142m  (1/34)\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Automatically compact context when it gets too large\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Type to search · Enter/Space to change · Esc to cancel\x1b[39m",
];

const GOLDEN_THEME_SINGLE_120: &[&str] = &[
    "\x1b[1m\x1b[38;2;167;152;215mTheme\x1b[39m\x1b[22m                                                                                                                   ",
    "",
    "\x1b[38;2;157;165;169mSelect a theme, or choose automatic to follow terminal appearance.\x1b[39m                                                      ",
    "",
    "    system\x1b[38;2;157;165;169m     Theme created from your terminal's colors\x1b[39m",
    "    automatic\x1b[38;2;157;165;169m  Use separate themes for light and dark terminal appearance\x1b[39m",
    "\x1b[38;2;167;152;215m→ ✓ dark\x1b[39m",
    "    light",
    "",
    "\x1b[38;2;126;136;142m  Enter to select · Esc to go back\x1b[39m                                                                                      ",
];

const GOLDEN_THEME_AUTOMATIC_120: &[&str] = &[
    "\x1b[1m\x1b[38;2;167;152;215mAutomatic Theme\x1b[39m\x1b[22m                                                                                                         ",
    "",
    "\x1b[38;2;157;165;169mChoose themes for terminal light and dark appearance.\x1b[39m                                                                   ",
    "\x1b[38;2;157;165;169mLight/dark detection requires terminal support.\x1b[39m                                                                         ",
    "",
    "\x1b[38;2;167;152;215m→ \x1b[39m\x1b[38;2;167;152;215mLight theme\x1b[39m  \x1b[38;2;167;152;215mdark\x1b[39m",
    "  Dark theme   \x1b[38;2;157;165;169mdark\x1b[39m",
    "  Apply        \x1b[38;2;157;165;169msave and go back\x1b[39m",
    "  Change mode  \x1b[38;2;157;165;169mswitch to single theme\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Theme to use in automatic mode when the terminal is light\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Enter/Space to change · Esc to cancel\x1b[39m",
];

const GOLDEN_THEME_AUTOMATIC_LIGHT_SUBMENU_120: &[&str] = &[
    "\x1b[1m\x1b[38;2;167;152;215mAutomatic Theme\x1b[39m\x1b[22m                                                                                                         ",
    "",
    "\x1b[38;2;157;165;169mChoose themes for terminal light and dark appearance.\x1b[39m                                                                   ",
    "\x1b[38;2;157;165;169mLight/dark detection requires terminal support.\x1b[39m                                                                         ",
    "",
    "\x1b[1m\x1b[38;2;167;152;215mLight Theme\x1b[39m\x1b[22m                                                                                                             ",
    "",
    "\x1b[38;2;157;165;169mSelect the theme to use for light terminal appearance\x1b[39m                                                                   ",
    "",
    "    system\x1b[38;2;157;165;169m    Theme created from your terminal's colors\x1b[39m",
    "\x1b[38;2;167;152;215m→ ✓ dark\x1b[39m",
    "    light",
    "",
    "\x1b[38;2;126;136;142m  Enter to select · Esc to go back\x1b[39m                                                                                      ",
];

const GOLDEN_THEME_AUTOMATIC_40: &[&str] = &[
    "\x1b[1m\x1b[38;2;167;152;215mAutomatic Theme\x1b[39m\x1b[22m                         ",
    "",
    "\x1b[38;2;157;165;169mChoose themes for terminal light and    ",
    "\x1b[38;2;157;165;169mdark appearance.\x1b[39m                        ",
    "\x1b[38;2;157;165;169mLight/dark detection requires terminal  ",
    "\x1b[38;2;157;165;169msupport.\x1b[39m                                ",
    "",
    "\x1b[38;2;167;152;215m→ \x1b[39m\x1b[38;2;167;152;215mLight theme\x1b[39m  \x1b[38;2;167;152;215mlight\x1b[39m",
    "  Dark theme   \x1b[38;2;157;165;169mdark\x1b[39m",
    "  Apply        \x1b[38;2;157;165;169msave and go back\x1b[39m",
    "  Change mode  \x1b[38;2;157;165;169mswitch to single theme\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Theme to use in automatic mode when\x1b[39m",
    "\x1b[38;2;126;136;142m  the terminal is light\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Enter/Space to change · Esc to cancel\x1b[39m",
];

const GOLDEN_WARNINGS_120: &[&str] = &[
    "\x1b[38;2;167;152;215m→ \x1b[39m\x1b[38;2;167;152;215mAnthropic extra usage\x1b[39m  \x1b[38;2;167;152;215mtrue\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Warn when Anthropic subscription auth may use paid extra usage\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Enter/Space to change · Esc to cancel\x1b[39m",
];

const GOLDEN_MODEL_STEP1_120: &[&str] = &[
    "\x1b[1m\x1b[38;2;167;152;215mPer-Model Thinking Level\x1b[39m\x1b[22m                                                                                                ",
    "",
    "\x1b[38;2;157;165;169mStep 1/2 · Select a model to configure\x1b[39m                                                                                  ",
    "",
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                                                                                                     ",
    "",
    "\x1b[38;2;167;152;215m→ thinking-model \x1b[38;2;157;165;169m[test-provider]\x1b[39m  medium\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Type to filter · Enter to select · Esc to go back\x1b[39m                                                                     ",
];

const GOLDEN_MODEL_STEP2_120: &[&str] = &[
    "\x1b[1m\x1b[38;2;167;152;215mThinking Level for thinking-model [test-provider]\x1b[39m\x1b[22m                                                                       ",
    "",
    "\x1b[38;2;157;165;169mStep 2/2 · Select default thinking level for this model\x1b[39m                                                                 ",
    "",
    "    off\x1b[38;2;157;165;169m               No reasoning\x1b[39m",
    "    minimal\x1b[38;2;157;165;169m           Very brief reasoning (~1k tokens)\x1b[39m",
    "    low\x1b[38;2;157;165;169m               Light reasoning (~2k tokens)\x1b[39m",
    "\x1b[38;2;167;152;215m→ ✓ medium            Moderate reasoning (~8k tokens)\x1b[39m",
    "    high\x1b[38;2;157;165;169m              Deep reasoning (~16k tokens)\x1b[39m",
    "    (clear override)\x1b[38;2;157;165;169m  Revert to global default (high)\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Enter to select · Esc to go back\x1b[39m                                                                                      ",
];

const GOLDEN_MODEL_STEP2_NO_OVERRIDE_120: &[&str] = &[
    "\x1b[1m\x1b[38;2;167;152;215mThinking Level for thinking-model [test-provider]\x1b[39m\x1b[22m                                                                       ",
    "",
    "\x1b[38;2;157;165;169mStep 2/2 · Select default thinking level for this model\x1b[39m                                                                 ",
    "",
    "\x1b[38;2;167;152;215m→   off       No reasoning\x1b[39m",
    "    minimal\x1b[38;2;157;165;169m   Very brief reasoning (~1k tokens)\x1b[39m",
    "    low\x1b[38;2;157;165;169m       Light reasoning (~2k tokens)\x1b[39m",
    "    medium\x1b[38;2;157;165;169m    Moderate reasoning (~8k tokens)\x1b[39m",
    "    high\x1b[38;2;157;165;169m      Deep reasoning (~16k tokens)\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Enter to select · Esc to go back\x1b[39m                                                                                      ",
];

#[test]
fn golden_narrow_settings_list_matches_ts_reference() {
    setup();
    let mut component =
        SettingsSelectorComponent::new(probe_config(), SettingsCallbacks::default());
    let list = component.get_settings_list();
    assert_lines(list.render(40), GOLDEN_SETTINGS_NARROW_40);
    list.select_item("model-thinking");
    list.handle_input("\r");
    assert_lines(list.render(40), GOLDEN_SETTINGS_NARROW_40_SUBMENU);
}

#[test]
fn golden_scrolled_main_lists_match_ts_reference() {
    setup();
    for (count, expected) in [
        (0usize, GOLDEN_SETTINGS_AT_0_120),
        (10, GOLDEN_SETTINGS_AT_10_120),
        (20, GOLDEN_SETTINGS_AT_20_120),
        (30, GOLDEN_SETTINGS_AT_30_120),
    ] {
        let mut component =
            SettingsSelectorComponent::new(probe_config(), SettingsCallbacks::default());
        let list = component.get_settings_list();
        for _ in 0..count {
            list.handle_input("\x1b[B");
        }
        assert_lines(list.render(120), expected);
    }
}

#[test]
fn golden_every_setting_item_matches_ts_reference() {
    setup();
    assert_eq!(GOLDEN_SETTINGS_IDS.len(), GOLDEN_SETTINGS_ITEM_LINES.len());
    assert_eq!(
        GOLDEN_SETTINGS_IDS.len(),
        GOLDEN_SETTINGS_ITEM_DESCRIPTIONS.len()
    );
    for (index, id) in GOLDEN_SETTINGS_IDS.iter().enumerate() {
        let mut component =
            SettingsSelectorComponent::new(probe_config(), SettingsCallbacks::default());
        let list = component.get_settings_list();
        list.select_item(id);
        let output = kiana_tui::strip_terminal_sequences(&list.render(120).join("\n"));
        let lines: Vec<&str> = output.split('\n').collect();
        let selected = lines
            .iter()
            .find(|line| line.starts_with("→ "))
            .copied()
            .unwrap_or("");
        assert_eq!(
            selected, GOLDEN_SETTINGS_ITEM_LINES[index].1,
            "selected line for {id}"
        );
        let hint_index = lines
            .iter()
            .position(|line| line.contains("Type to search"))
            .expect("settings list always renders the search hint");
        let mut description: Vec<&str> = Vec::new();
        let mut cursor = hint_index - 1;
        while cursor > 0 {
            cursor -= 1;
            if lines[cursor].is_empty() {
                break;
            }
            description.push(lines[cursor]);
        }
        description.reverse();
        assert_eq!(
            description,
            GOLDEN_SETTINGS_ITEM_DESCRIPTIONS[index].1.to_vec(),
            "description for {id}"
        );
    }
}
const GOLDEN_SETTINGS_AT_0_120: &[&str] = &[
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                                                                                                     ",
    "",
    "\x1b[38;2;167;152;215m→ \x1b[39m\x1b[38;2;167;152;215mAuto-compact                    \x1b[39m  \x1b[38;2;167;152;215mtrue\x1b[39m",
    "  Auto-resize images                \x1b[38;2;157;165;169mtrue\x1b[39m",
    "  Block images                      \x1b[38;2;157;165;169mfalse\x1b[39m",
    "  Skill commands                    \x1b[38;2;157;165;169mtrue\x1b[39m",
    "  Show hardware cursor              \x1b[38;2;157;165;169mfalse\x1b[39m",
    "  Editor padding                    \x1b[38;2;157;165;169m0\x1b[39m",
    "  Output padding                    \x1b[38;2;157;165;169m1\x1b[39m",
    "  Autocomplete max items            \x1b[38;2;157;165;169m5\x1b[39m",
    "  Clear on shrink                   \x1b[38;2;157;165;169mfalse\x1b[39m",
    "  Terminal progress                 \x1b[38;2;157;165;169mfalse\x1b[39m",
    "\x1b[38;2;126;136;142m  (1/34)\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Automatically compact context when it gets too large\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Type to search · Enter/Space to change · Esc to cancel\x1b[39m",
];

const GOLDEN_SETTINGS_AT_10_120: &[&str] = &[
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                                                                                                     ",
    "",
    "  Editor padding                    \x1b[38;2;157;165;169m0\x1b[39m",
    "  Output padding                    \x1b[38;2;157;165;169m1\x1b[39m",
    "  Autocomplete max items            \x1b[38;2;157;165;169m5\x1b[39m",
    "  Clear on shrink                   \x1b[38;2;157;165;169mfalse\x1b[39m",
    "  Terminal progress                 \x1b[38;2;157;165;169mfalse\x1b[39m",
    "\x1b[38;2;167;152;215m→ \x1b[39m\x1b[38;2;167;152;215mSteering mode                   \x1b[39m  \x1b[38;2;167;152;215mone-at-a-time\x1b[39m",
    "  Follow-up mode                    \x1b[38;2;157;165;169mone-at-a-time\x1b[39m",
    "  Transport                         \x1b[38;2;157;165;169msse\x1b[39m",
    "  Session share destination         \x1b[38;2;157;165;169mauto\x1b[39m",
    "  HTTP idle timeout                 \x1b[38;2;157;165;169m30 sec\x1b[39m",
    "\x1b[38;2;126;136;142m  (11/34)\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Enter while streaming queues steering messages. 'one-at-a-time': deliver one, wait for response. 'all': deliver all\x1b[39m",
    "\x1b[38;2;126;136;142m  at once.\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Type to search · Enter/Space to change · Esc to cancel\x1b[39m",
];

const GOLDEN_SETTINGS_AT_20_120: &[&str] = &[
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                                                                                                     ",
    "",
    "  Cache warming                     \x1b[38;2;157;165;169mstreaming\x1b[39m",
    "  Hide thinking                     \x1b[38;2;157;165;169mfalse\x1b[39m",
    "  Mermaid diagrams                  \x1b[38;2;157;165;169mstreaming\x1b[39m",
    "  Cache miss notices                \x1b[38;2;157;165;169mfalse\x1b[39m",
    "  Collapse changelog                \x1b[38;2;157;165;169mfalse\x1b[39m",
    "\x1b[38;2;167;152;215m→ \x1b[39m\x1b[38;2;167;152;215mQuiet startup                   \x1b[39m  \x1b[38;2;167;152;215mfalse\x1b[39m",
    "  Install telemetry                 \x1b[38;2;157;165;169mtrue\x1b[39m",
    "  Default project trust             \x1b[38;2;157;165;169mAsk\x1b[39m",
    "  Double-escape action              \x1b[38;2;157;165;169mtree\x1b[39m",
    "  Tree filter mode                  \x1b[38;2;157;165;169mdefault\x1b[39m",
    "\x1b[38;2;126;136;142m  (21/34)\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Disable verbose printing at startup (header: keep only the startup header)\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Type to search · Enter/Space to change · Esc to cancel\x1b[39m",
];

const GOLDEN_SETTINGS_AT_30_120: &[&str] = &[
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                                                                                                     ",
    "",
    "  Tree filter mode                  \x1b[38;2;157;165;169mdefault\x1b[39m",
    "  Warnings                          \x1b[38;2;157;165;169mconfigure\x1b[39m",
    "  Default thinking level per model  \x1b[38;2;157;165;169m1 configured\x1b[39m",
    "  TUI mode                          \x1b[38;2;157;165;169mfullscreen\x1b[39m",
    "  Fullscreen exit output            \x1b[38;2;157;165;169mtranscript\x1b[39m",
    "  Fullscreen scrollbar              \x1b[38;2;157;165;169mauto\x1b[39m",
    "\x1b[38;2;167;152;215m→ \x1b[39m\x1b[38;2;167;152;215mFullscreen copy on select       \x1b[39m  \x1b[38;2;167;152;215mtrue\x1b[39m",
    "  Fullscreen wheel scrolling        \x1b[38;2;157;165;169m7\x1b[39m",
    "  Theme                             \x1b[38;2;157;165;169mdark\x1b[39m",
    "  Language                          \x1b[38;2;157;165;169mEnglish\x1b[39m",
    "\x1b[38;2;126;136;142m  (31/34)\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Automatically copy selected text in fullscreen mode; disable to copy selections with Ctrl+X\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Type to search · Enter/Space to change · Esc to cancel\x1b[39m",
];

const GOLDEN_SETTINGS_IDS: &[&str] = &[
    "autocompact",
    "auto-resize-images",
    "block-images",
    "skill-commands",
    "show-hardware-cursor",
    "editor-padding",
    "output-padding",
    "autocomplete-max-visible",
    "clear-on-shrink",
    "terminal-progress",
    "steering-mode",
    "follow-up-mode",
    "transport",
    "share-destination",
    "http-idle-timeout",
    "cache-warming-mode",
    "hide-thinking",
    "mermaid-rendering",
    "cache-miss-notices",
    "collapse-changelog",
    "quiet-startup",
    "install-telemetry",
    "default-project-trust",
    "double-escape-action",
    "tree-filter-mode",
    "warnings",
    "model-thinking",
    "tui-mode",
    "fullscreen-exit-output",
    "fullscreen-scrollbar",
    "fullscreen-copy-on-select",
    "fullscreen-wheel-scroll-lines",
    "theme",
    "ui-locale",
];

const GOLDEN_SETTINGS_ITEM_LINES: &[(&str, &str)] = &[
    ("autocompact", "→ Auto-compact                      true"),
    (
        "auto-resize-images",
        "→ Auto-resize images                true",
    ),
    ("block-images", "→ Block images                      false"),
    ("skill-commands", "→ Skill commands                    true"),
    (
        "show-hardware-cursor",
        "→ Show hardware cursor              false",
    ),
    ("editor-padding", "→ Editor padding                    0"),
    ("output-padding", "→ Output padding                    1"),
    (
        "autocomplete-max-visible",
        "→ Autocomplete max items            5",
    ),
    (
        "clear-on-shrink",
        "→ Clear on shrink                   false",
    ),
    (
        "terminal-progress",
        "→ Terminal progress                 false",
    ),
    (
        "steering-mode",
        "→ Steering mode                     one-at-a-time",
    ),
    (
        "follow-up-mode",
        "→ Follow-up mode                    one-at-a-time",
    ),
    ("transport", "→ Transport                         sse"),
    (
        "share-destination",
        "→ Session share destination         auto",
    ),
    (
        "http-idle-timeout",
        "→ HTTP idle timeout                 30 sec",
    ),
    (
        "cache-warming-mode",
        "→ Cache warming                     streaming",
    ),
    ("hide-thinking", "→ Hide thinking                     false"),
    (
        "mermaid-rendering",
        "→ Mermaid diagrams                  streaming",
    ),
    (
        "cache-miss-notices",
        "→ Cache miss notices                false",
    ),
    (
        "collapse-changelog",
        "→ Collapse changelog                false",
    ),
    ("quiet-startup", "→ Quiet startup                     false"),
    (
        "install-telemetry",
        "→ Install telemetry                 true",
    ),
    (
        "default-project-trust",
        "→ Default project trust             Ask",
    ),
    (
        "double-escape-action",
        "→ Double-escape action              tree",
    ),
    (
        "tree-filter-mode",
        "→ Tree filter mode                  default",
    ),
    ("warnings", "→ Warnings                          configure"),
    (
        "model-thinking",
        "→ Default thinking level per model  1 configured",
    ),
    ("tui-mode", "→ TUI mode                          fullscreen"),
    (
        "fullscreen-exit-output",
        "→ Fullscreen exit output            transcript",
    ),
    (
        "fullscreen-scrollbar",
        "→ Fullscreen scrollbar              auto",
    ),
    (
        "fullscreen-copy-on-select",
        "→ Fullscreen copy on select         true",
    ),
    (
        "fullscreen-wheel-scroll-lines",
        "→ Fullscreen wheel scrolling        7",
    ),
    ("theme", "→ Theme                             dark"),
    ("ui-locale", "→ Language                          English"),
];

const GOLDEN_SETTINGS_ITEM_DESCRIPTIONS: &[(&str, &[&str])] = &[
    (
        "autocompact",
        &["  Automatically compact context when it gets too large"],
    ),
    (
        "auto-resize-images",
        &["  Resize large images to 2000x2000 max for better model compatibility"],
    ),
    (
        "block-images",
        &["  Prevent images from being sent to LLM providers"],
    ),
    (
        "skill-commands",
        &["  Register skills as /skill:name commands"],
    ),
    (
        "show-hardware-cursor",
        &["  Use the terminal cursor instead of Kiana's drawn cursor"],
    ),
    (
        "editor-padding",
        &["  Horizontal padding for input editor (0-3)"],
    ),
    (
        "output-padding",
        &["  Horizontal padding for messages, tool output, and command output"],
    ),
    (
        "autocomplete-max-visible",
        &["  Max visible items in autocomplete dropdown (3-20)"],
    ),
    (
        "clear-on-shrink",
        &["  Clear empty rows when content shrinks (may cause flicker)"],
    ),
    (
        "terminal-progress",
        &["  Show OSC 9;4 progress indicators in the terminal tab bar"],
    ),
    (
        "steering-mode",
        &[
            "  Enter while streaming queues steering messages. 'one-at-a-time': deliver one, wait for response. 'all': deliver all",
            "  at once.",
        ],
    ),
    (
        "follow-up-mode",
        &[
            "  Alt+Enter queues follow-up messages until agent stops. 'one-at-a-time': deliver one, wait for response. 'all':",
            "  deliver all at once.",
        ],
    ),
    (
        "transport",
        &["  Preferred transport for providers that support multiple transports"],
    ),
    (
        "share-destination",
        &[
            "  Where to upload sessions with /share (auto prefers Radius when authenticated, otherwise GitHub Gist)",
        ],
    ),
    (
        "http-idle-timeout",
        &[
            "  Maximum idle gap while waiting for HTTP headers or body chunks. Disable for local models that pause longer than five",
            "  minutes.",
        ],
    ),
    (
        "cache-warming-mode",
        &[
            "  off; streaming while the agent runs; idle also between runs while continuation stays profitable",
        ],
    ),
    (
        "hide-thinking",
        &["  Hide thinking blocks in assistant responses"],
    ),
    (
        "mermaid-rendering",
        &["  Render Mermaid code blocks as Unicode diagrams"],
    ),
    (
        "cache-miss-notices",
        &["  Show transcript notices for cache costs and provider recovery diagnostics"],
    ),
    (
        "collapse-changelog",
        &["  Show condensed changelog after updates"],
    ),
    (
        "quiet-startup",
        &["  Disable verbose printing at startup (header: keep only the startup header)"],
    ),
    (
        "install-telemetry",
        &["  Send an anonymous version/update ping after changelog-detected updates"],
    ),
    (
        "default-project-trust",
        &["  Fallback behavior when no extension or saved trust decision decides project trust"],
    ),
    (
        "double-escape-action",
        &["  Action when pressing Escape twice with empty editor"],
    ),
    ("tree-filter-mode", &["  Default filter when opening /tree"]),
    ("warnings", &["  Enable or disable individual warnings"]),
    (
        "model-thinking",
        &[
            "  Override the default thinking level for specific models. Shift+Tab cycles in-session.",
        ],
    ),
    (
        "tui-mode",
        &["  Interface layout; regular mode uses the terminal's normal scrollback"],
    ),
    (
        "fullscreen-exit-output",
        &["  Print the transcript or only a session resume hint when exiting fullscreen mode"],
    ),
    (
        "fullscreen-scrollbar",
        &["  Scrollbar behavior in fullscreen mode; has no effect in regular mode"],
    ),
    (
        "fullscreen-copy-on-select",
        &[
            "  Automatically copy selected text in fullscreen mode; disable to copy selections with Ctrl+X",
        ],
    ),
    (
        "fullscreen-wheel-scroll-lines",
        &[
            "  Lines per mouse-wheel event in fullscreen mode; 'auto' speeds up fast wheel spins where the terminal does not",
        ],
    ),
    ("theme", &["  Color theme for the interface"]),
    (
        "ui-locale",
        &["  Language for the interface (UI only; model answers keep their own language)"],
    ),
];
const GOLDEN_SETTINGS_NARROW_40: &[&str] = &[
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                     ",
    "",
    "\x1b[38;2;167;152;215m→ \x1b[39m\x1b[38;2;167;152;215mAuto-compact                    \x1b[39m  \x1b[38;2;167;152;215mtr\x1b[0m\x1b[39m",
    "  Auto-resize images                \x1b[38;2;157;165;169mtr\x1b[0m\x1b[39m",
    "  Block images                      \x1b[38;2;157;165;169mfa\x1b[0m\x1b[39m",
    "  Skill commands                    \x1b[38;2;157;165;169mtr\x1b[0m\x1b[39m",
    "  Show hardware cursor              \x1b[38;2;157;165;169mfa\x1b[0m\x1b[39m",
    "  Editor padding                    \x1b[38;2;157;165;169m0\x1b[39m",
    "  Output padding                    \x1b[38;2;157;165;169m1\x1b[39m",
    "  Autocomplete max items            \x1b[38;2;157;165;169m5\x1b[39m",
    "  Clear on shrink                   \x1b[38;2;157;165;169mfa\x1b[0m\x1b[39m",
    "  Terminal progress                 \x1b[38;2;157;165;169mfa\x1b[0m\x1b[39m",
    "\x1b[38;2;126;136;142m  (1/34)\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Automatically compact context when\x1b[39m",
    "\x1b[38;2;126;136;142m  it gets too large\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Type to search · Enter/Space to cha\x1b[0m...\x1b[0m",
];

const GOLDEN_SETTINGS_NARROW_40_SUBMENU: &[&str] = &[
    "\x1b[1m\x1b[38;2;167;152;215mPer-Model Thinking Level\x1b[39m\x1b[22m                ",
    "",
    "\x1b[38;2;157;165;169mStep 1/2 · Select a model to configure\x1b[39m  ",
    "",
    "> \x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                                     ",
    "",
    "\x1b[38;2;167;152;215m→ thinking-model \x1b[38;2;157;165;169m[test-provider]\x1b[39m\x1b[39m",
    "",
    "\x1b[38;2;126;136;142m  Type to filter · Enter to select · Esc",
    "\x1b[38;2;126;136;142mto go back\x1b[39m                              ",
];
