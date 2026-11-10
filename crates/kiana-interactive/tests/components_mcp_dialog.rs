//! `McpDialog` 的渲染/按键/启停回调测试。
//!
//! 组件文件通过 `#[path]` 直接编进测试 crate：`components/mod.rs` 由 Integrate agent
//! 统一声明，本任务不修改共享文件。组件内部不引用 `crate::components::…` 的其它组件，
//! 因此这里不需要 `components` 命名空间垫片（与 `tests/permissions_dialog.rs` 的差别）。
//!
//! 锁定行为：
//! - 配置读取复用 `load_mcp_config`：全局 + 项目 `mcp.json`、项目覆盖的 `override` 路径、
//!   未受信时不读项目文件；
//! - 列表行的状态标记、`状态 · 工具数 · 来源` 文案与“需要处理优先”的排序；
//! - 详情页的 transport/source/state/exposure/tools/prompts/最近错误与动作菜单；
//! - `↑↓`/Enter/Esc 的列表→详情→工具页导航，只有列表页 Esc 关闭面板；
//! - `e` 启停回调（成功更新本地 `enabled`、失败保留原状态并显示错误）；
//! - `l`/`o` 登录/登出入口与可用性提示；
//! - `r` 刷新回调与快照替换时的选中保持；
//! - `write_mcp_server_enabled` 使用既有 setter 写 `enabled`，并选中
//!   `override ?? source` 目标文件。

#[path = "../src/components/mcp_dialog.rs"]
pub mod mcp_dialog;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};

use indexmap::IndexMap;
use kiana_builtin_extensions::mcp::config::{
    LoadMcpConfigOptions, McpExposure, McpServerScope, load_mcp_config,
};
use kiana_builtin_extensions::mcp::runtime::{McpServerStatus, McpServerStatusState};
use kiana_tui::text::visible_width;
use kiana_tui::tui::Component;
use mcp_dialog::{
    McpDialog, McpDialogCallbacks, McpDialogOptions, McpDialogSnapshot, McpDialogView,
    McpNoticeKind, McpServerAction, McpServerRow, server_actions, write_mcp_server_enabled,
};

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 主题是进程级全局状态；测试并行运行，用一把锁串行化初始化与渲染。
fn setup() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    kiana_theme::init_theme(Some("dark"), false);
    guard
}

/// 去掉 CSI/OSC/APC 转义序列（测试只需比较可见文本）。
fn strip_ansi(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\x1b' {
            output.push(character);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') | Some('_') => {
                chars.next();
                for next in chars.by_ref() {
                    if next == '\x07' {
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    output
}

fn render_plain(dialog: &mut McpDialog, width: usize) -> Vec<String> {
    dialog
        .render(width)
        .iter()
        .map(|line| strip_ansi(line))
        .collect()
}

fn joined(dialog: &mut McpDialog, width: usize) -> String {
    render_plain(dialog, width).join("\n")
}

fn server(
    name: &str,
    scope: McpServerScope,
    enabled: bool,
    state: Option<McpServerStatusState>,
    tools: &[&str],
) -> McpServerRow {
    let source = match scope {
        McpServerScope::Global => "/home/user/.kiana/agent/mcp.json".to_string(),
        McpServerScope::Project => "/repo/.kiana/mcp.json".to_string(),
        McpServerScope::Extension => "test-extension".to_string(),
    };
    McpServerRow {
        name: name.to_string(),
        scope,
        source,
        override_path: None,
        enabled,
        exposure: McpExposure::Codemode,
        transport: format!("npx {name}"),
        status: state.map(|state| McpServerStatus {
            state,
            last_error: None,
            tool_count: tools.len(),
            prompt_count: 0,
        }),
        tools: tools.iter().map(|tool| (*tool).to_string()).collect(),
        tool_exposure: IndexMap::new(),
        resources: false,
    }
}

fn snapshot(servers: Vec<McpServerRow>) -> McpDialogSnapshot {
    McpDialogSnapshot {
        servers,
        errors: Vec::new(),
        global_config: None,
        project_config: None,
    }
}

fn dialog_with(servers: Vec<McpServerRow>, callbacks: McpDialogCallbacks) -> McpDialog {
    McpDialog::new(McpDialogOptions {
        snapshot: snapshot(servers),
        callbacks,
    })
}

fn down(dialog: &mut McpDialog) {
    dialog.handle_input("\x1b[B");
}

fn up(dialog: &mut McpDialog) {
    dialog.handle_input("\x1b[A");
}

fn enter(dialog: &mut McpDialog) {
    dialog.handle_input("\r");
}

fn escape(dialog: &mut McpDialog) {
    dialog.handle_input("\x1b");
}

// ---------------------------------------------------------------------------
// 配置读取
// ---------------------------------------------------------------------------

#[test]
fn loads_global_and_project_config_through_existing_api() {
    let _guard = setup();
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(repo.join(".kiana")).unwrap();
    std::fs::write(
        agent.join("mcp.json"),
        r#"{"mcpServers":{"global-a":{"command":"npx","args":["-y","a"]},"off":{"url":"https://example.test/mcp","enabled":false}}}"#,
    )
    .unwrap();
    std::fs::write(
        repo.join(".kiana/mcp.json"),
        r#"{"mcpServers":{"global-a":{"enabled":false},"project-b":{"command":"uvx","args":["b"]}}}"#,
    )
    .unwrap();

    let expected_global = agent.join("mcp.json").to_string_lossy().into_owned();
    let expected_project = repo.join(".kiana/mcp.json").to_string_lossy().into_owned();

    let loaded = McpDialogSnapshot::load(LoadMcpConfigOptions {
        agent_dir: agent.clone(),
        cwd: repo.clone(),
        project_trusted: true,
    });
    assert_eq!(
        loaded.global_config.as_deref(),
        Some(expected_global.as_str())
    );
    assert_eq!(
        loaded.project_config.as_deref(),
        Some(expected_project.as_str())
    );
    assert!(loaded.errors.is_empty());

    // 需要处理优先：project-b（无运行期状态）在禁用的两台之前；同为禁用按 locale 排序。
    let names: Vec<&str> = loaded.servers.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, vec!["project-b", "global-a", "off"]);

    let project_b = &loaded.servers[0];
    assert_eq!(project_b.scope, McpServerScope::Project);
    assert!(project_b.enabled);
    assert_eq!(project_b.transport, "uvx b");

    let global_a = &loaded.servers[1];
    assert_eq!(global_a.scope, McpServerScope::Global);
    assert!(!global_a.enabled);
    assert_eq!(
        global_a.override_path.as_deref(),
        Some(expected_project.as_str())
    );
    assert_eq!(global_a.transport, "npx -y a");

    let off = &loaded.servers[2];
    assert!(!off.enabled);
    assert_eq!(off.transport, "https://example.test/mcp");

    // 项目未受信：不读项目文件，覆盖也不生效。
    let untrusted = McpDialogSnapshot::load(LoadMcpConfigOptions {
        agent_dir: agent,
        cwd: repo,
        project_trusted: false,
    });
    assert_eq!(untrusted.project_config, None);
    let names: Vec<&str> = untrusted
        .servers
        .iter()
        .map(|row| row.name.as_str())
        .collect();
    assert_eq!(names, vec!["global-a", "off"]);
    let global_a = untrusted
        .servers
        .iter()
        .find(|row| row.name == "global-a")
        .unwrap();
    assert!(global_a.enabled);
    assert!(global_a.override_path.is_none());
}

#[test]
fn invalid_config_errors_are_shown_not_fatal() {
    let _guard = setup();
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(agent.join("mcp.json"), "{not json").unwrap();

    let loaded = McpDialogSnapshot::load(LoadMcpConfigOptions {
        agent_dir: agent,
        cwd: repo,
        project_trusted: true,
    });
    assert!(loaded.servers.is_empty());
    assert_eq!(loaded.errors.len(), 1);
    assert!(loaded.errors[0].contains("mcp.json"));

    let mut dialog = McpDialog::new(McpDialogOptions {
        snapshot: loaded,
        callbacks: McpDialogCallbacks::default(),
    });
    let text = joined(&mut dialog, 120);
    assert!(text.contains("Config errors:"));
    assert!(text.contains("mcp.json"));
    assert!(text.contains("No MCP servers configured."));
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

#[test]
fn list_renders_state_tool_count_scope_and_recent_error() {
    let _guard = setup();
    let mut connected = server(
        "github",
        McpServerScope::Project,
        true,
        Some(McpServerStatusState::Connected),
        &["search", "issues", "prs"],
    );
    connected.status.as_mut().unwrap().prompt_count = 2;
    connected.resources = true;

    let mut failed = server(
        "broken",
        McpServerScope::Global,
        true,
        Some(McpServerStatusState::Failed),
        &[],
    );
    failed.status.as_mut().unwrap().last_error = Some("spawn npx ENOENT\nsecond line".to_string());

    let off = server(
        "off",
        McpServerScope::Global,
        false,
        Some(McpServerStatusState::Disabled),
        &[],
    );

    let mut dialog = dialog_with(vec![connected, failed, off], McpDialogCallbacks::default());
    let lines = render_plain(&mut dialog, 120);
    let text = lines.join("\n");

    assert!(text.contains("MCP servers (3)"));
    assert!(text.contains("→ ● github"));
    assert!(text.contains("connected · 3 tools · project"));
    assert!(text.contains("✗ broken"));
    assert!(text.contains("failed · 0 tools · global"));
    assert!(text.contains("spawn npx ENOENT"));
    assert!(!text.contains("second line"));
    assert!(text.contains("○ off"));
    assert!(text.contains("disabled · 0 tools · global"));
    assert!(text.contains("↑↓ choose · Enter details"));
    assert!(text.contains("l sign in"));
}

#[test]
fn empty_state_and_narrow_width_do_not_panic() {
    let _guard = setup();
    let mut dialog = dialog_with(vec![], McpDialogCallbacks::default());
    let text = joined(&mut dialog, 120);
    assert!(text.contains("No MCP servers configured."));

    // 窄宽度只保证不 panic、不越界（空态文案会被截断）。
    let lines = render_plain(&mut dialog, 24);
    assert!(lines.iter().all(|line| visible_width(line) <= 24));

    // 极窄宽度同样保证不越界。
    let lines = render_plain(&mut dialog, 1);
    assert!(lines.len() >= 2);
    assert!(lines.iter().all(|line| visible_width(line) <= 1));
}

#[test]
fn detail_renders_transport_source_error_and_actions() {
    let _guard = setup();
    let mut row = server(
        "github",
        McpServerScope::Global,
        true,
        Some(McpServerStatusState::Failed),
        &["search"],
    );
    row.override_path = Some("/repo/.kiana/mcp.json".to_string());
    row.status.as_mut().unwrap().last_error = Some("401 Unauthorized".to_string());
    row.exposure = McpExposure::Direct;
    row.resources = true;

    let mut dialog = dialog_with(vec![row], McpDialogCallbacks::default());
    enter(&mut dialog);
    assert!(matches!(
        dialog.view(),
        McpDialogView::ServerDetail { name, selected: 0 } if name == "github"
    ));
    let text = joined(&mut dialog, 120);
    assert!(text.contains("MCP server github"));
    assert!(text.contains("transport: npx github"));
    assert!(text.contains("source: global (/home/user/.kiana/agent/mcp.json)"));
    assert!(text.contains("project override: /repo/.kiana/mcp.json"));
    assert!(text.contains("state: failed"));
    assert!(text.contains("exposure: direct"));
    assert!(text.contains("tools: 1"));
    assert!(text.contains("prompts: 0"));
    assert!(text.contains("resources: yes"));
    assert!(text.contains("recent error:"));
    assert!(text.contains("401 Unauthorized"));
    assert!(text.contains("Actions"));
    assert!(text.contains("→ Disable"));
    assert!(text.contains("Back"));
}

// ---------------------------------------------------------------------------
// 导航
// ---------------------------------------------------------------------------

#[test]
fn list_keys_move_open_detail_and_only_list_escape_closes() {
    let _guard = setup();
    let closed = Rc::new(Cell::new(0usize));
    let closed_cb = Rc::clone(&closed);
    let callbacks = McpDialogCallbacks {
        on_close: Box::new(move || closed_cb.set(closed_cb.get() + 1)),
        ..McpDialogCallbacks::default()
    };
    let mut dialog = dialog_with(
        vec![
            server(
                "alpha",
                McpServerScope::Global,
                true,
                Some(McpServerStatusState::Connected),
                &["a", "b"],
            ),
            server(
                "beta",
                McpServerScope::Project,
                true,
                Some(McpServerStatusState::Connecting),
                &[],
            ),
        ],
        callbacks,
    );

    assert_eq!(dialog.selected_name(), Some("alpha"));
    down(&mut dialog);
    assert_eq!(dialog.selected_name(), Some("beta"));
    up(&mut dialog);
    assert_eq!(dialog.selected_name(), Some("alpha"));

    enter(&mut dialog);
    assert!(matches!(dialog.view(), McpDialogView::ServerDetail { .. }));
    assert_eq!(closed.get(), 0);

    escape(&mut dialog);
    assert_eq!(dialog.view(), &McpDialogView::ServerList);
    assert_eq!(closed.get(), 0);

    escape(&mut dialog);
    assert_eq!(closed.get(), 1);
}

#[test]
fn tool_list_navigation_and_exposure_override_annotation() {
    let _guard = setup();
    let mut row = server(
        "tools",
        McpServerScope::Global,
        true,
        Some(McpServerStatusState::Connected),
        &["alpha", "beta"],
    );
    row.tool_exposure
        .insert("beta".to_string(), McpExposure::Direct);
    let mut dialog = dialog_with(vec![row], McpDialogCallbacks::default());

    enter(&mut dialog);
    // 动作顺序：Disable(0) / Sign out(1) / Tools(2) / Back(3)。
    down(&mut dialog);
    down(&mut dialog);
    enter(&mut dialog);
    assert!(matches!(
        dialog.view(),
        McpDialogView::ToolList { name, selected: 0 } if name == "tools"
    ));
    let text = joined(&mut dialog, 120);
    assert!(text.contains("Tools of tools (2)"));
    assert!(text.contains("default exposure: codemode"));
    assert!(text.contains("→ alpha"));
    assert!(text.contains("beta [direct]"));

    down(&mut dialog);
    assert!(matches!(
        dialog.view(),
        McpDialogView::ToolList { selected: 1, .. }
    ));
    escape(&mut dialog);
    assert!(matches!(dialog.view(), McpDialogView::ServerDetail { .. }));
}

// ---------------------------------------------------------------------------
// 启用/禁用
// ---------------------------------------------------------------------------

#[test]
fn toggle_callback_receives_target_state_and_updates_row() {
    let _guard = setup();
    let calls = Rc::new(RefCell::new(Vec::<(String, bool)>::new()));
    let calls_cb = Rc::clone(&calls);
    let callbacks = McpDialogCallbacks {
        on_toggle: Box::new(move |row, enabled| {
            calls_cb.borrow_mut().push((row.name.clone(), enabled));
            Ok(())
        }),
        ..McpDialogCallbacks::default()
    };
    let mut dialog = dialog_with(
        vec![server(
            "off",
            McpServerScope::Global,
            false,
            Some(McpServerStatusState::Disabled),
            &[],
        )],
        callbacks,
    );

    dialog.handle_input("e");
    assert_eq!(calls.borrow().as_slice(), &[("off".to_string(), true)]);
    assert!(dialog.snapshot().servers[0].enabled);
    // 旧快照里的 `disabled` 被丢弃，等宿主刷新真实状态。
    assert_eq!(dialog.snapshot().servers[0].effective_state(), None);
    assert_eq!(dialog.notice().unwrap().kind, McpNoticeKind::Info);
    assert!(dialog.notice().unwrap().message.contains("Enabled off"));

    dialog.handle_input("e");
    assert_eq!(
        calls.borrow().as_slice(),
        &[("off".to_string(), true), ("off".to_string(), false)]
    );
    assert!(!dialog.snapshot().servers[0].enabled);
    assert!(dialog.notice().unwrap().message.contains("Disabled off"));
}

#[test]
fn toggle_error_keeps_row_state_and_shows_error_notice() {
    let _guard = setup();
    let callbacks = McpDialogCallbacks {
        on_toggle: Box::new(|_, _| Err("permission denied".to_string())),
        ..McpDialogCallbacks::default()
    };
    let mut dialog = dialog_with(
        vec![server(
            "off",
            McpServerScope::Global,
            false,
            Some(McpServerStatusState::Disabled),
            &[],
        )],
        callbacks,
    );

    dialog.handle_input("e");
    assert!(!dialog.snapshot().servers[0].enabled);
    let notice = dialog.notice().unwrap();
    assert_eq!(notice.kind, McpNoticeKind::Error);
    assert_eq!(notice.message, "permission denied");
}

#[test]
fn write_mcp_server_enabled_uses_existing_setter() {
    let _guard = setup();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(
        &path,
        "{\n  \"mcpServers\": {\n    \"srv\": {\n      \"command\": \"npx\"\n    }\n  }\n}\n",
    )
    .unwrap();

    let mut row = server("srv", McpServerScope::Global, true, None, &[]);
    row.source = path.to_string_lossy().into_owned();

    write_mcp_server_enabled(&row, false).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"enabled\": false"));
    let loaded = load_mcp_config(LoadMcpConfigOptions {
        agent_dir: dir.path().to_path_buf(),
        cwd: dir.path().to_path_buf(),
        project_trusted: false,
    });
    let entry = loaded
        .servers
        .iter()
        .find(|entry| entry.name == "srv")
        .unwrap();
    assert_eq!(entry.config.base().enabled, Some(false));

    // 重新启用普通条目会删除 `enabled` 键（缺省即启用），这是既有 setter 的行为。
    write_mcp_server_enabled(&row, true).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("\"enabled\""));
    let loaded = load_mcp_config(LoadMcpConfigOptions {
        agent_dir: dir.path().to_path_buf(),
        cwd: dir.path().to_path_buf(),
        project_trusted: false,
    });
    let entry = loaded
        .servers
        .iter()
        .find(|entry| entry.name == "srv")
        .unwrap();
    assert_ne!(entry.config.base().enabled, Some(false));

    // 扩展注册的条目没有可写文件。
    let extension = server("ext", McpServerScope::Extension, true, None, &[]);
    let error = write_mcp_server_enabled(&extension, false).unwrap_err();
    assert!(error.contains("registered by an extension"));
}

#[test]
fn write_mcp_server_enabled_targets_project_override_file() {
    let _guard = setup();
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(repo.join(".kiana")).unwrap();
    let global_path = agent.join("mcp.json");
    let project_path = repo.join(".kiana/mcp.json");
    std::fs::write(
        &global_path,
        "{\n  \"mcpServers\": {\n    \"srv\": {\n      \"command\": \"npx\"\n    }\n  }\n}\n",
    )
    .unwrap();
    std::fs::write(
        &project_path,
        "{\n  \"mcpServers\": {\n    \"srv\": {\n      \"enabled\": false\n    }\n  }\n}\n",
    )
    .unwrap();

    let loaded = McpDialogSnapshot::load(LoadMcpConfigOptions {
        agent_dir: agent,
        cwd: repo,
        project_trusted: true,
    });
    let row = loaded.servers.iter().find(|row| row.name == "srv").unwrap();
    assert!(!row.enabled);
    assert!(row.override_path.is_some());

    write_mcp_server_enabled(row, true).unwrap();
    let project_text = std::fs::read_to_string(&project_path).unwrap();
    assert!(project_text.contains("\"enabled\": true"));
    let global_text = std::fs::read_to_string(&global_path).unwrap();
    assert!(!global_text.contains("\"enabled\""));
}

// ---------------------------------------------------------------------------
// 登录/登出与刷新
// ---------------------------------------------------------------------------

#[test]
fn login_logout_entries_and_callbacks() {
    let _guard = setup();
    let login_calls = Rc::new(RefCell::new(Vec::<String>::new()));
    let logout_calls = Rc::new(RefCell::new(Vec::<String>::new()));
    let login_cb = Rc::clone(&login_calls);
    let logout_cb = Rc::clone(&logout_calls);
    let callbacks = McpDialogCallbacks {
        on_login: Box::new(move |row| {
            login_cb.borrow_mut().push(row.name.clone());
            Ok(())
        }),
        on_logout: Box::new(move |row| {
            logout_cb.borrow_mut().push(row.name.clone());
            Ok(())
        }),
        ..McpDialogCallbacks::default()
    };
    let mut dialog = dialog_with(
        vec![
            server(
                "auth",
                McpServerScope::Global,
                true,
                Some(McpServerStatusState::NeedsAuth),
                &[],
            ),
            server(
                "live",
                McpServerScope::Global,
                true,
                Some(McpServerStatusState::Connected),
                &["x"],
            ),
            server(
                "off",
                McpServerScope::Global,
                false,
                Some(McpServerStatusState::Disabled),
                &[],
            ),
        ],
        callbacks,
    );

    // needs-auth 行上 l 生效，o 给出提示。
    dialog.handle_input("l");
    assert_eq!(login_calls.borrow().as_slice(), &["auth".to_string()]);
    assert!(
        dialog
            .notice()
            .unwrap()
            .message
            .contains("Sign-in requested")
    );
    dialog.handle_input("o");
    assert!(logout_calls.borrow().is_empty());
    assert!(
        dialog
            .notice()
            .unwrap()
            .message
            .contains("Sign out is only available")
    );

    // 已连接行上 o 生效。
    down(&mut dialog);
    dialog.handle_input("o");
    assert_eq!(logout_calls.borrow().as_slice(), &["live".to_string()]);

    // 禁用的服务器先要求启用。
    down(&mut dialog);
    dialog.handle_input("l");
    assert_eq!(login_calls.borrow().len(), 1);
    assert!(
        dialog
            .notice()
            .unwrap()
            .message
            .contains("Enable the server before signing in.")
    );

    // 详情动作按状态生成：needs-auth 有 Sign in 而没有 Sign out。
    let auth_actions = server_actions(&server(
        "auth",
        McpServerScope::Global,
        true,
        Some(McpServerStatusState::NeedsAuth),
        &[],
    ));
    assert!(auth_actions.contains(&McpServerAction::SignIn));
    assert!(!auth_actions.contains(&McpServerAction::SignOut));
    assert!(
        auth_actions.contains(&McpServerAction::Disable)
            && auth_actions.contains(&McpServerAction::Back)
    );

    let live_actions = server_actions(&server(
        "live",
        McpServerScope::Global,
        true,
        Some(McpServerStatusState::Connected),
        &["x"],
    ));
    assert!(live_actions.contains(&McpServerAction::SignOut));
    assert!(live_actions.contains(&McpServerAction::Tools));
    assert!(!live_actions.contains(&McpServerAction::SignIn));
}

#[test]
fn refresh_callback_and_snapshot_replacement_preserve_selection() {
    let _guard = setup();
    let refreshes = Rc::new(Cell::new(0usize));
    let refreshes_cb = Rc::clone(&refreshes);
    let callbacks = McpDialogCallbacks {
        on_refresh: Box::new(move || refreshes_cb.set(refreshes_cb.get() + 1)),
        ..McpDialogCallbacks::default()
    };
    let mut dialog = dialog_with(
        vec![
            server(
                "a",
                McpServerScope::Global,
                true,
                Some(McpServerStatusState::Connected),
                &[],
            ),
            server(
                "b",
                McpServerScope::Global,
                true,
                Some(McpServerStatusState::Connected),
                &[],
            ),
            server(
                "c",
                McpServerScope::Global,
                true,
                Some(McpServerStatusState::Connected),
                &[],
            ),
        ],
        callbacks,
    );
    down(&mut dialog);
    down(&mut dialog);
    assert_eq!(dialog.selected_name(), Some("c"));

    dialog.handle_input("r");
    assert_eq!(refreshes.get(), 1);

    // 宿主刷新后顺序变化：按名字保留选中项。
    dialog.set_snapshot(snapshot(vec![
        server(
            "c",
            McpServerScope::Project,
            true,
            Some(McpServerStatusState::Connected),
            &[],
        ),
        server(
            "a",
            McpServerScope::Global,
            true,
            Some(McpServerStatusState::Connected),
            &[],
        ),
    ]));
    assert_eq!(dialog.selected_name(), Some("c"));

    // 详情页正在看的服务器消失时退回列表。
    enter(&mut dialog);
    assert!(matches!(dialog.view(), McpDialogView::ServerDetail { .. }));
    dialog.set_snapshot(snapshot(vec![server(
        "other",
        McpServerScope::Global,
        true,
        Some(McpServerStatusState::Connected),
        &[],
    )]));
    assert_eq!(dialog.view(), &McpDialogView::ServerList);
}
