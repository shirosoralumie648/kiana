//! `oauth-selector.test.ts` / `thinking-selector.test.ts` / `trust-selector.test.ts` 的 Rust
//! 移植，加上 `theme-selector` / `show-images-selector` / `radius-login-selector` 的行为测试
//! 与 `first-time-setup.ts` 的 golden 渲染测试。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

use kiana_agent_core::ThinkingLevel;
use kiana_ai::auth::types::{AuthCheck, AuthType};
use kiana_config::trust::manager::ProjectTrustStoreEntry;
use kiana_core::ansi::strip_ansi;
use kiana_interactive::components::first_time_setup::{
    FirstTimeSetupComponent, FirstTimeSetupOptions, FirstTimeSetupResult,
};
use kiana_interactive::components::oauth_selector::{
    AuthSelectorMode, AuthSelectorProvider, OAuthSelectorComponent,
    format_auth_selector_provider_status, format_auth_selector_provider_type,
};
use kiana_interactive::components::radius_login_selector::{
    RadiusOption, create_login_menu_selector,
};
use kiana_interactive::components::show_images_selector::ShowImagesSelectorComponent;
use kiana_interactive::components::theme_selector::ThemeSelectorComponent;
use kiana_interactive::components::thinking_selector::ThinkingSelectorComponent;
use kiana_interactive::components::trust_selector::{TrustSelectorComponent, TrustSelectorOptions};
use kiana_theme::init_theme;
use kiana_tui::keybindings::{KeybindingsConfig, KeybindingsManager, set_keybindings};
use kiana_tui::tui::{Component, Timer, TuiMouseButton, TuiMouseEvent, TuiMouseEventType};

fn theme() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| init_theme(Some("dark"), false));
}

/// 与 TS `TuiMouseEvent` 同形的鼠标事件（`x`/`y` 是相对接收组件的坐标）。
fn mouse_event(
    event_type: TuiMouseEventType,
    x: isize,
    y: isize,
    width: usize,
    height: usize,
) -> TuiMouseEvent {
    TuiMouseEvent {
        event_type,
        button: if event_type == TuiMouseEventType::Wheel {
            TuiMouseButton::None
        } else {
            TuiMouseButton::Left
        },
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
        click_count: (event_type == TuiMouseEventType::Click).then_some(1),
    }
}

fn install_keybindings() {
    set_keybindings(KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        KeybindingsConfig::new(),
    ));
}

fn install_keybindings_with(user: &[(&str, &str)]) {
    let mut config = KeybindingsConfig::new();
    for (binding, key) in user {
        config.insert((*binding).to_string(), (*key).into());
    }
    set_keybindings(KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        config,
    ));
}

type TrustDecisionLog = Rc<RefCell<Vec<(bool, Vec<(String, Option<bool>)>)>>>;
type SingleTrustDecisionLog = Rc<RefCell<Option<(bool, Vec<(String, Option<bool>)>)>>>;

fn rendered(component: &mut dyn Component, width: usize) -> String {
    strip_ansi(&component.render(width).join("\n"))
}

// --- oauth-selector.test.ts -----------------------------------------------

fn provider(
    id: &str,
    name: &str,
    auth_type: AuthType,
    status: Option<AuthCheck>,
) -> AuthSelectorProvider {
    AuthSelectorProvider {
        id: id.to_string(),
        name: name.to_string(),
        auth_type,
        method: None,
        status,
        subscription: None,
    }
}

fn auth_status(source: Option<&str>, auth_type: AuthType) -> AuthCheck {
    AuthCheck {
        source: source.map(str::to_string),
        r#type: auth_type,
    }
}

#[test]
fn format_auth_selector_provider_type_is_exact() {
    assert_eq!(
        format_auth_selector_provider_type(AuthType::ApiKey, None),
        "API key"
    );
    assert_eq!(
        format_auth_selector_provider_type(AuthType::OAuth, None),
        "subscription"
    );
    assert_eq!(
        format_auth_selector_provider_type(AuthType::OAuth, Some(false)),
        "account"
    );
    assert_eq!(
        format_auth_selector_provider_type(AuthType::OAuth, Some(true)),
        "subscription"
    );
}

#[test]
fn renders_an_option_without_compiled_auth_status_as_not_configured() {
    theme();
    install_keybindings();
    let mut selector = OAuthSelectorComponent::new(
        AuthSelectorMode::Login,
        vec![provider("google", "Google", AuthType::ApiKey, None)],
        |_, _| {},
        || {},
        None,
    );
    let output = rendered(&mut selector, 120);
    assert!(output.contains("not configured"));
    assert!(!output.contains("✓ configured"));
}

#[test]
fn shows_oauth_auth_distinctly_in_the_api_key_selector() {
    theme();
    install_keybindings();
    let mut selector = OAuthSelectorComponent::new(
        AuthSelectorMode::Login,
        vec![provider(
            "anthropic",
            "Anthropic",
            AuthType::ApiKey,
            Some(auth_status(Some("OAuth"), AuthType::OAuth)),
        )],
        |_, _| {},
        || {},
        None,
    );
    assert!(rendered(&mut selector, 120).contains("subscription configured"));
}

#[test]
fn shows_environment_api_key_auth_as_configured() {
    theme();
    install_keybindings();
    let mut selector = OAuthSelectorComponent::new(
        AuthSelectorMode::Login,
        vec![provider(
            "openai",
            "OpenAI",
            AuthType::ApiKey,
            Some(auth_status(Some("OPENAI_API_KEY"), AuthType::ApiKey)),
        )],
        |_, _| {},
        || {},
        None,
    );
    let output = rendered(&mut selector, 120);
    assert!(output.contains("✓ env: OPENAI_API_KEY"));
    assert!(!output.contains("not configured"));
}

#[test]
fn shows_models_json_api_key_auth_as_configured() {
    theme();
    install_keybindings();
    let mut selector = OAuthSelectorComponent::new(
        AuthSelectorMode::Login,
        vec![provider(
            "local-proxy",
            "local-proxy",
            AuthType::ApiKey,
            Some(auth_status(Some("key in models.json"), AuthType::ApiKey)),
        )],
        |_, _| {},
        || {},
        None,
    );
    assert!(rendered(&mut selector, 120).contains("✓ key in models.json"));
}

#[test]
fn shows_models_json_command_auth_as_configured() {
    theme();
    install_keybindings();
    let mut selector = OAuthSelectorComponent::new(
        AuthSelectorMode::Login,
        vec![provider(
            "op-proxy",
            "op-proxy",
            AuthType::ApiKey,
            Some(auth_status(
                Some("command in models.json"),
                AuthType::ApiKey,
            )),
        )],
        |_, _| {},
        || {},
        None,
    );
    assert!(rendered(&mut selector, 120).contains("✓ command in models.json"));
}

#[test]
fn status_formatter_matches_the_ts_matrix() {
    theme();
    let stored = provider(
        "a",
        "A",
        AuthType::ApiKey,
        Some(auth_status(Some("stored credential"), AuthType::ApiKey)),
    );
    assert!(format_auth_selector_provider_status(&stored).contains("✓ configured"));
    let empty_source = provider(
        "a",
        "A",
        AuthType::ApiKey,
        Some(auth_status(Some(""), AuthType::ApiKey)),
    );
    assert!(format_auth_selector_provider_status(&empty_source).contains("✓ configured"));
    let env_list = provider(
        "a",
        "A",
        AuthType::ApiKey,
        Some(auth_status(Some("FIRST_KEY, SECOND_KEY"), AuthType::ApiKey)),
    );
    assert!(format_auth_selector_provider_status(&env_list).contains("env: FIRST_KEY, SECOND_KEY"));
}

#[test]
fn oauth_selector_clamps_and_filters() {
    theme();
    install_keybindings();
    let selected: Rc<RefCell<Vec<(String, AuthType)>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_log = Rc::clone(&selected);
    let mut selector = OAuthSelectorComponent::new(
        AuthSelectorMode::Login,
        vec![
            provider("a", "Alpha", AuthType::ApiKey, None),
            provider("b", "Beta", AuthType::ApiKey, None),
        ],
        move |id, auth_type| selected_log.borrow_mut().push((id.to_string(), auth_type)),
        || {},
        None,
    );
    selector.handle_input("\x1b[B");
    assert_eq!(selector.selected_index(), 1);
    // clamp：再按一次仍停在最后一项。
    selector.handle_input("\x1b[B");
    assert_eq!(selector.selected_index(), 1);
    selector.handle_input("\x1b[A");
    assert_eq!(selector.selected_index(), 0);

    selector.handle_input("be");
    assert_eq!(selector.filtered_providers().len(), 1);
    assert_eq!(selector.filtered_providers()[0].id, "b");
    selector.handle_input("\r");
    assert_eq!(
        selected.borrow().clone(),
        vec![("b".to_string(), AuthType::ApiKey)]
    );
}

/// 回归（minor）：TS `Container.handleMouse` 让搜索框收到点击并定位光标。
#[test]
fn oauth_selector_forwards_clicks_to_the_search_input() {
    theme();
    install_keybindings();
    let mut selector = OAuthSelectorComponent::new(
        AuthSelectorMode::Login,
        vec![
            provider("a", "Alpha", AuthType::ApiKey, None),
            provider("b", "Beta", AuthType::ApiKey, None),
        ],
        |_, _| {},
        || {},
        Some("abcd"),
    );
    let lines = selector.render(120);
    let height = lines.len();
    let row = lines
        .iter()
        .position(|line| kiana_tui::strip_terminal_sequences(line).contains("> abcd"))
        .expect("search input row");
    let press = selector
        .handle_mouse(&mouse_event(
            TuiMouseEventType::Press,
            4,
            row as isize,
            120,
            height,
        ))
        .expect("press must reach the search Input");
    assert_eq!(press.handled, Some(true));
    assert_eq!(press.focus, Some(true));
    selector.handle_input("X");
    assert_eq!(selector.search_value(), "abXcd");
}

// --- thinking-selector.test.ts --------------------------------------------

fn level_row(selector: &mut ThinkingSelectorComponent, level: &str) -> Option<String> {
    selector
        .get_select_list_mut()
        .render(80)
        .into_iter()
        .map(|line| strip_ansi(&line))
        .find(|line| line.contains(level))
}

#[test]
fn keeps_the_current_thinking_level_marked_while_browsing() {
    theme();
    install_keybindings();
    let mut selector = ThinkingSelectorComponent::new(
        ThinkingLevel::Medium,
        vec![ThinkingLevel::Medium, ThinkingLevel::High],
        |_| {},
        || {},
        None,
        None,
    );
    assert_eq!(
        selector
            .get_select_list()
            .get_selected_item()
            .map(|item| item.label.clone()),
        Some("✓ medium".to_string())
    );
    assert!(
        level_row(&mut selector, "medium")
            .expect("medium row")
            .starts_with("→ ✓ medium")
    );
    selector.handle_input("\x1b[B");
    assert!(
        level_row(&mut selector, "medium")
            .expect("medium row")
            .starts_with("  ✓ medium")
    );
    assert!(
        level_row(&mut selector, "high")
            .expect("high row")
            .starts_with("→   high")
    );
}

#[test]
fn uses_the_configured_save_binding() {
    theme();
    install_keybindings_with(&[("app.thinking.save", "ctrl+r")]);
    let saved: Rc<RefCell<Vec<ThinkingLevel>>> = Rc::new(RefCell::new(Vec::new()));
    let saved_log = Rc::clone(&saved);
    let mut selector = ThinkingSelectorComponent::new(
        ThinkingLevel::Medium,
        vec![ThinkingLevel::Medium, ThinkingLevel::High],
        |_| {},
        || {},
        Some(Box::new(move |level| saved_log.borrow_mut().push(level))),
        None,
    );
    assert!(
        rendered(&mut selector, 80).contains("Ctrl+R to set as default"),
        "hint must use the configured save binding"
    );
    // ctrl+s 不是保存键位（默认 ctrl+s 已改绑到 ctrl+r）。
    selector.handle_input("\x13");
    assert!(saved.borrow().is_empty());
    selector.handle_input("\x12");
    assert_eq!(saved.borrow().clone(), vec![ThinkingLevel::Medium]);
}

/// 回归（major）：TS 的 `ThinkingSelectorComponent extends Container`，`Container.handleMouse`
/// 把点击/滚轮按行交给 search Input / SelectList。Rust 之前没有覆写 `handle_mouse`，
/// 列表的 `ListProxy` 也不转发，事件被丢弃。
#[test]
fn thinking_selector_forwards_clicks_to_the_select_list() {
    theme();
    install_keybindings();
    let selected: Rc<RefCell<Vec<ThinkingLevel>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_log = Rc::clone(&selected);
    let mut selector = ThinkingSelectorComponent::new(
        ThinkingLevel::Medium,
        vec![ThinkingLevel::Medium, ThinkingLevel::High],
        move |level| selected_log.borrow_mut().push(level),
        || {},
        None,
        None,
    );

    let lines = selector.render(80);
    let height = lines.len();
    let row = lines
        .iter()
        .position(|line| strip_ansi(line).contains("✓ medium"))
        .expect("medium row");
    // 点击第二行（high）：press 先移动选中项，click 再确认并触发 onSelect。
    let row = row + 1;

    let press = selector
        .handle_mouse(&mouse_event(
            TuiMouseEventType::Press,
            0,
            row as isize,
            80,
            height,
        ))
        .expect("press must reach the SelectList");
    assert_eq!(press.handled, Some(true));
    assert_eq!(press.focus, Some(true));
    assert_eq!(
        selector
            .get_select_list()
            .get_selected_item()
            .map(|item| item.value.as_str()),
        Some("high")
    );
    assert!(
        selected.borrow().is_empty(),
        "press only moves the selection"
    );

    let click = selector
        .handle_mouse(&mouse_event(
            TuiMouseEventType::Click,
            0,
            row as isize,
            80,
            height,
        ))
        .expect("click must reach the SelectList");
    assert_eq!(click.handled, Some(true));
    assert_eq!(selected.borrow().clone(), vec![ThinkingLevel::High]);
}

#[test]
fn thinking_selector_forwards_the_mouse_wheel_to_the_select_list() {
    theme();
    install_keybindings();
    let mut selector = ThinkingSelectorComponent::new(
        ThinkingLevel::Medium,
        vec![ThinkingLevel::Medium, ThinkingLevel::High],
        |_| {},
        || {},
        None,
        None,
    );
    let lines = selector.render(80);
    let height = lines.len();
    let row = lines
        .iter()
        .position(|line| strip_ansi(line).contains("✓ medium"))
        .expect("medium row");
    let mut event = mouse_event(TuiMouseEventType::Wheel, 0, row as isize, 80, height);
    event.wheel_delta = Some(1.0);
    let result = selector
        .handle_mouse(&event)
        .expect("wheel must reach the SelectList");
    assert_eq!(result.handled, Some(true));
    assert_eq!(
        selector
            .get_select_list()
            .get_selected_item()
            .map(|item| item.value.as_str()),
        Some("high")
    );
}

/// 回归（major）：列表之外的 search Input 也要收到点击（`Container` 按行路由）。
#[test]
fn thinking_selector_forwards_clicks_to_the_search_input() {
    theme();
    install_keybindings();
    let mut selector = ThinkingSelectorComponent::new(
        ThinkingLevel::Medium,
        vec![ThinkingLevel::Medium, ThinkingLevel::High],
        |_| {},
        || {},
        None,
        None,
    );
    selector.handle_input("abcd");
    let lines = selector.render(80);
    let height = lines.len();
    let row = lines
        .iter()
        .position(|line| kiana_tui::strip_terminal_sequences(line).contains("> abcd"))
        .expect("search input row");
    let press = selector
        .handle_mouse(&mouse_event(
            TuiMouseEventType::Press,
            4,
            row as isize,
            80,
            height,
        ))
        .expect("press must reach the Input");
    assert_eq!(press.handled, Some(true));
    assert_eq!(press.focus, Some(true));
    // x=4 → 可见列 2 → 光标落在 "ab|c"（Input.handleMouse 的 `x - 2` 约定）。
    selector.handle_input("X");
    assert_eq!(selector.search_value(), "abXcd");
}

/// 回归（minor）：TS `searchInput.onSubmit = () => this.selectList.handleInput("\r")`。
/// 提交键（包括 Kitty 协议下退化成字面量 `\n` 的 enter）必须确认当前项。
#[test]
fn thinking_selector_routes_the_input_submit_to_the_select_list() {
    theme();
    install_keybindings();
    // 用户可改绑 `tui.input.submit`：ctrl+e 不是导航键，TS 仍会经 onSubmit 选中。
    install_keybindings_with(&[("tui.input.submit", "ctrl+e")]);
    let selected: Rc<RefCell<Vec<ThinkingLevel>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_log = Rc::clone(&selected);
    let mut selector = ThinkingSelectorComponent::new(
        ThinkingLevel::Medium,
        vec![ThinkingLevel::Medium, ThinkingLevel::High],
        move |level| selected_log.borrow_mut().push(level),
        || {},
        None,
        None,
    );
    selector.handle_input("\x05");
    assert_eq!(selected.borrow().clone(), vec![ThinkingLevel::Medium]);
    install_keybindings();
}

// --- trust-selector.test.ts -----------------------------------------------

fn trust(selector: &mut TrustSelectorComponent) -> String {
    rendered(selector, 120)
}

#[test]
fn keeps_the_saved_trusted_decision_marked_while_browsing() {
    theme();
    install_keybindings();
    let mut selector = TrustSelectorComponent::new(TrustSelectorOptions {
        cwd: "/project".to_string(),
        saved_decision: Some(ProjectTrustStoreEntry {
            path: "/project".to_string(),
            decision: true,
        }),
        project_trusted: true,
        on_select: Box::new(|_| {}),
        on_cancel: Box::new(|| {}),
    });

    let output = trust(&mut selector);
    assert!(output.contains("Saved decision: trusted (/project)"));
    assert!(output.contains("Current session: trusted"));
    assert!(output.contains("→ ✓ Trust"));

    selector.handle_input("\x1b[B");
    let output = trust(&mut selector);
    assert!(output.contains("✓ Trust"));
    assert!(output.contains("→   Trust parent folder (/)"));
    assert!(!output.contains("✓ Do not trust"));
}

#[test]
fn selects_a_trust_decision() {
    theme();
    install_keybindings();
    let selected: TrustDecisionLog = Rc::new(RefCell::new(Vec::new()));
    let selected_log = Rc::clone(&selected);
    let mut selector = TrustSelectorComponent::new(TrustSelectorOptions {
        cwd: "/project".to_string(),
        saved_decision: None,
        project_trusted: false,
        on_select: Box::new(move |selection| {
            selected_log.borrow_mut().push((
                selection.trusted,
                selection
                    .updates
                    .iter()
                    .map(|update| (update.path.clone(), update.decision))
                    .collect(),
            ));
        }),
        on_cancel: Box::new(|| {}),
    });

    selector.handle_input("\n");
    assert_eq!(
        selected.borrow().clone(),
        vec![(true, vec![("/project".to_string(), Some(true))])]
    );
}

#[test]
fn labels_saved_ancestor_decisions_as_inherited() {
    theme();
    install_keybindings();
    let mut selector = TrustSelectorComponent::new(TrustSelectorOptions {
        cwd: "/parent/project/nested".to_string(),
        saved_decision: Some(ProjectTrustStoreEntry {
            path: "/parent".to_string(),
            decision: true,
        }),
        project_trusted: true,
        on_select: Box::new(|_| {}),
        on_cancel: Box::new(|| {}),
    });

    assert!(trust(&mut selector).contains("Saved decision: trusted (inherited from /parent)"));
}

#[test]
fn adds_a_trust_parent_option() {
    theme();
    install_keybindings();
    let selected: SingleTrustDecisionLog = Rc::new(RefCell::new(None));
    let selected_log = Rc::clone(&selected);
    let mut selector = TrustSelectorComponent::new(TrustSelectorOptions {
        cwd: "/parent/project".to_string(),
        saved_decision: Some(ProjectTrustStoreEntry {
            path: "/parent".to_string(),
            decision: true,
        }),
        project_trusted: true,
        on_select: Box::new(move |selection| {
            *selected_log.borrow_mut() = Some((
                selection.trusted,
                selection
                    .updates
                    .iter()
                    .map(|update| (update.path.clone(), update.decision))
                    .collect(),
            ));
        }),
        on_cancel: Box::new(|| {}),
    });

    let output = trust(&mut selector);
    assert!(output.contains("Saved decision: trusted (inherited from /parent)"));
    assert!(output.contains("✓ Trust parent folder (/parent)"));

    selector.handle_input("\n");
    assert_eq!(
        selected.borrow().clone(),
        Some((
            true,
            vec![
                ("/parent".to_string(), Some(true)),
                ("/parent/project".to_string(), None),
            ]
        ))
    );
}

// --- theme-selector / show-images-selector ---------------------------------

#[test]
fn theme_selector_marks_the_current_theme_and_previews() {
    theme();
    install_keybindings();
    let previews: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let preview_log = Rc::clone(&previews);
    let selected: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_log = Rc::clone(&selected);
    let mut selector = ThemeSelectorComponent::new(
        "dark",
        move |name| selected_log.borrow_mut().push(name.to_string()),
        || {},
        move |name| preview_log.borrow_mut().push(name.to_string()),
    );

    {
        let select_list = selector.get_select_list();
        assert_eq!(
            select_list
                .get_selected_item()
                .map(|item| item.value.as_str()),
            Some("dark")
        );
        assert_eq!(
            select_list
                .get_selected_item()
                .and_then(|item| item.description.clone()),
            Some("(current)".to_string())
        );
    }

    let down = "\x1b[B";
    selector.get_select_list_mut().handle_input(down);
    assert_eq!(previews.borrow().len(), 1);
    selector.get_select_list_mut().handle_input("\r");
    assert_eq!(selected.borrow().len(), 1);
}

#[test]
fn show_images_selector_maps_yes_and_no() {
    theme();
    install_keybindings();
    let selected: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_log = Rc::clone(&selected);
    let mut selector = ShowImagesSelectorComponent::new(
        true,
        move |show| selected_log.borrow_mut().push(show),
        || {},
    );
    assert_eq!(selector.get_select_list().selected_index(), 0);
    selector.get_select_list_mut().handle_input("\x1b[B");
    assert_eq!(selector.get_select_list().selected_index(), 1);
    selector.get_select_list_mut().handle_input("\r");
    assert_eq!(selected.borrow().clone(), vec![false]);
}

/// 回归（major）：TS 把 `SelectList` 作为 `Container` 的子项挂载，因此点击行会选中并确认。
/// Rust 之前渲染时手工拼接列表、列表不在容器子项里，鼠标事件永远到不了它。
#[test]
fn theme_selector_forwards_clicks_to_the_select_list() {
    theme();
    install_keybindings();
    let previews: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let preview_log = Rc::clone(&previews);
    let selected: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_log = Rc::clone(&selected);
    let mut selector = ThemeSelectorComponent::new(
        "dark",
        move |name| selected_log.borrow_mut().push(name.to_string()),
        || {},
        move |name| preview_log.borrow_mut().push(name.to_string()),
    );

    let values: Vec<String> = selector
        .get_select_list()
        .filtered_items()
        .iter()
        .map(|item| item.value.clone())
        .collect();
    assert!(values.len() >= 3, "expected at least system/dark/light");
    let target = values[2].clone();
    let lines = selector.render(80);
    let height = lines.len();
    // 容器子项顺序：border(1) + SelectList + border；本地 row 2 即列表第 3 项。
    let row = 1 + 2;

    let press = selector
        .handle_mouse(&mouse_event(TuiMouseEventType::Press, 0, row, 80, height))
        .expect("press must reach the SelectList");
    assert_eq!(press.handled, Some(true));
    assert_eq!(
        selector.get_select_list().selected_index(),
        2,
        "press moves the selection"
    );
    assert_eq!(previews.borrow().clone(), vec![target.clone()]);

    let click = selector
        .handle_mouse(&mouse_event(TuiMouseEventType::Click, 0, row, 80, height))
        .expect("click must reach the SelectList");
    assert_eq!(click.handled, Some(true));
    assert_eq!(selected.borrow().clone(), vec![target]);
}

/// 回归（major）：同 theme selector，show-images 的列表点击必须选中并回调。
#[test]
fn show_images_selector_forwards_clicks_to_the_select_list() {
    theme();
    install_keybindings();
    let selected: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_log = Rc::clone(&selected);
    let mut selector = ShowImagesSelectorComponent::new(
        true,
        move |show| selected_log.borrow_mut().push(show),
        || {},
    );

    let lines = selector.render(80);
    let height = lines.len();
    let row = 1 + 1; // border + 第二项（No）
    let press = selector
        .handle_mouse(&mouse_event(TuiMouseEventType::Press, 0, row, 80, height))
        .expect("press must reach the SelectList");
    assert_eq!(press.handled, Some(true));
    assert_eq!(selector.get_select_list().selected_index(), 1);

    selector
        .handle_mouse(&mouse_event(TuiMouseEventType::Click, 0, row, 80, height))
        .expect("click must reach the SelectList");
    assert_eq!(selected.borrow().clone(), vec![false]);
}

// --- radius-login-selector -------------------------------------------------

#[derive(Default)]
struct FakeTimer {
    pending: Option<Box<dyn FnOnce()>>,
    delays: Vec<u64>,
}

impl Timer for FakeTimer {
    fn set(&mut self, delay_ms: u64, callback: Box<dyn FnOnce()>) {
        self.delays.push(delay_ms);
        self.pending = Some(callback);
    }

    fn cancel(&mut self) {
        self.pending = None;
    }
}

/// 与组件共享的 fake 定时器（`set_timer` 取得所有权，测试通过 Rc<RefCell> 触发）。
#[derive(Clone, Default)]
struct SharedTimer(Rc<RefCell<FakeTimer>>);

impl Timer for SharedTimer {
    fn set(&mut self, delay_ms: u64, callback: Box<dyn FnOnce()>) {
        self.0.borrow_mut().set(delay_ms, callback);
    }

    fn cancel(&mut self) {
        self.0.borrow_mut().cancel();
    }
}

impl SharedTimer {
    fn fire(&self) {
        let callback = self.0.borrow_mut().pending.take();
        if let Some(callback) = callback {
            callback();
        }
    }

    fn delays(&self) -> Vec<u64> {
        self.0.borrow().delays.clone()
    }
}

#[test]
fn radius_login_selector_shimmers_the_selected_option() {
    theme();
    install_keybindings();
    let renders = Rc::new(RefCell::new(0usize));
    let render_log = Rc::clone(&renders);
    let selected: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_log = Rc::clone(&selected);
    let mut selector = create_login_menu_selector(
        Rc::new(move || *render_log.borrow_mut() += 1),
        "Login",
        vec![
            "Sign in with Radius".to_string(),
            "Sign in with an account".to_string(),
        ],
        RadiusOption {
            label: "Sign in with Radius".to_string(),
            text: "Sign in with Radius".to_string(),
        },
        move |option| selected_log.borrow_mut().push(option.to_string()),
        || {},
    );

    let output = rendered(&mut selector, 80);
    assert!(output.contains("→ Sign in with Radius"));
    assert!(output.contains("  Sign in with an account"));

    // 动画帧：宿主定时器每 50ms 请求一次重绘（仅当 Radius 行被选中）。
    let timer = SharedTimer::default();
    selector.set_timer(Box::new(timer.clone()));
    assert_eq!(timer.delays(), vec![50]);
    timer.fire();
    assert_eq!(*renders.borrow(), 1);
    assert_eq!(timer.delays(), vec![50, 50]);

    // 选择后停止动画并回调。
    selector.handle_input("\r");
    assert_eq!(
        selected.borrow().clone(),
        vec!["Sign in with Radius".to_string()]
    );
    timer.fire();
    assert_eq!(*renders.borrow(), 1, "selection stops the animation");

    selector.dispose();
    timer.fire();
    assert_eq!(*renders.borrow(), 1, "dispose stops the animation");
}

// --- first-time-setup.ts golden renders -----------------------------------

fn golden() -> serde_json::Value {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/first-time-setup-dark-80.json"
    ))
    .expect("first-time-setup golden fixture");
    serde_json::from_str(&raw).expect("valid golden json")
}

fn golden_lines(value: &serde_json::Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .expect("golden array")
        .iter()
        .map(|line| line.as_str().expect("golden line").to_string())
        .collect()
}

fn stripped_lines(component: &mut FirstTimeSetupComponent) -> Vec<String> {
    component
        .render(80)
        .into_iter()
        .map(|line| strip_ansi(&line))
        .collect()
}

#[test]
fn first_time_setup_renders_like_the_ts_component_in_dark_mode_at_80_columns() {
    theme();
    install_keybindings();
    let golden = golden();
    let previews: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let submits: Rc<RefCell<Vec<FirstTimeSetupResult>>> = Rc::new(RefCell::new(Vec::new()));
    let cancels = Rc::new(RefCell::new(0usize));

    let preview_log = Rc::clone(&previews);
    let submit_log = Rc::clone(&submits);
    let cancel_log = Rc::clone(&cancels);
    let mut component = FirstTimeSetupComponent::new(FirstTimeSetupOptions {
        on_theme_preview: Box::new(move |name| preview_log.borrow_mut().push(name.to_string())),
        on_submit: Box::new(move |result| submit_log.borrow_mut().push(result)),
        on_cancel: Box::new(move || *cancel_log.borrow_mut() += 1),
    });

    assert_eq!(
        stripped_lines(&mut component),
        golden_lines(&golden, "themeInitial")
    );

    component.handle_input("\x1b[B");
    assert_eq!(
        previews.borrow().clone(),
        vec!["dark".to_string()],
        "theme preview fires once with the new value"
    );
    assert_eq!(
        stripped_lines(&mut component),
        golden_lines(&golden, "themeDown")
    );

    component.handle_input("\r");
    assert_eq!(
        stripped_lines(&mut component),
        golden_lines(&golden, "analyticsInitial")
    );

    component.handle_input("j");
    assert_eq!(
        stripped_lines(&mut component),
        golden_lines(&golden, "analyticsDown")
    );

    component.handle_input("\r");
    assert_eq!(
        submits.borrow().clone(),
        vec![FirstTimeSetupResult {
            theme: "dark".to_string(),
            share_analytics: false,
        }]
    );
    assert_eq!(*cancels.borrow(), 0);
}

#[test]
fn first_time_setup_cancel_skips_setup_and_invalidate_rebuilds() {
    theme();
    install_keybindings();
    let cancels = Rc::new(RefCell::new(0usize));
    let submits = Rc::new(RefCell::new(0usize));
    let cancel_log = Rc::clone(&cancels);
    let submit_log = Rc::clone(&submits);
    let mut component = FirstTimeSetupComponent::new(FirstTimeSetupOptions {
        on_theme_preview: Box::new(|_| {}),
        on_submit: Box::new(move |_| *submit_log.borrow_mut() += 1),
        on_cancel: Box::new(move || *cancel_log.borrow_mut() += 1),
    });

    let before = stripped_lines(&mut component);
    component.invalidate();
    assert_eq!(
        stripped_lines(&mut component),
        before,
        "invalidate rebuilds the same tree"
    );

    component.handle_input("\x1b");
    assert_eq!(*cancels.borrow(), 1);
    assert_eq!(*submits.borrow(), 0);
}

// ---------------------------------------------------------------------------
// 渲染 golden（由真实 TS 组件在 dark 主题下生成；80 列，trust/oauth 用 120 列）
// ---------------------------------------------------------------------------

fn component_golden(key: &str) -> Vec<String> {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/component-renders-dark.json"
    ))
    .expect("component renders golden fixture");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("valid golden json");
    value[key]
        .as_array()
        .expect("golden array")
        .iter()
        .map(|line| line.as_str().expect("golden line").to_string())
        .collect()
}

fn assert_matches_golden(key: &str, actual: Vec<String>) {
    let stripped: Vec<String> = actual.into_iter().map(|line| strip_ansi(&line)).collect();
    assert_eq!(stripped, component_golden(key), "render mismatch for {key}");
}

#[test]
fn selectors_render_like_the_ts_components() {
    theme();
    install_keybindings();

    let mut theme_selector = ThemeSelectorComponent::new("dark", |_| {}, || {}, |_| {});
    assert_matches_golden("themeSelector", theme_selector.render(80));

    let mut show_images = ShowImagesSelectorComponent::new(true, |_| {}, || {});
    assert_matches_golden("showImages", show_images.render(80));

    let mut thinking = ThinkingSelectorComponent::new(
        ThinkingLevel::Medium,
        vec![ThinkingLevel::Medium, ThinkingLevel::High],
        |_| {},
        || {},
        None,
        None,
    );
    assert_matches_golden("thinking", thinking.render(80));
    thinking.handle_input("\x1b[B");
    assert_matches_golden("thinkingDown", thinking.render(80));

    let mut trust = TrustSelectorComponent::new(TrustSelectorOptions {
        cwd: "/project".to_string(),
        saved_decision: Some(ProjectTrustStoreEntry {
            path: "/project".to_string(),
            decision: true,
        }),
        project_trusted: true,
        on_select: Box::new(|_| {}),
        on_cancel: Box::new(|| {}),
    });
    assert_matches_golden("trust", trust.render(120));
    trust.handle_input("\x1b[B");
    assert_matches_golden("trustDown", trust.render(120));

    let mut oauth = OAuthSelectorComponent::new(
        AuthSelectorMode::Login,
        vec![
            AuthSelectorProvider {
                id: "anthropic".to_string(),
                name: "Anthropic".to_string(),
                auth_type: AuthType::ApiKey,
                method: None,
                status: Some(auth_status(Some("OAuth"), AuthType::OAuth)),
                subscription: None,
            },
            AuthSelectorProvider {
                id: "openai".to_string(),
                name: "OpenAI".to_string(),
                auth_type: AuthType::ApiKey,
                method: None,
                status: Some(auth_status(Some("OPENAI_API_KEY"), AuthType::ApiKey)),
                subscription: None,
            },
            provider("google", "Google", AuthType::ApiKey, None),
            AuthSelectorProvider {
                id: "local-proxy".to_string(),
                name: "local-proxy".to_string(),
                auth_type: AuthType::ApiKey,
                method: None,
                status: Some(auth_status(Some("key in models.json"), AuthType::ApiKey)),
                subscription: None,
            },
        ],
        |_, _| {},
        || {},
        None,
    );
    assert_matches_golden("oauth", oauth.render(120));
}
