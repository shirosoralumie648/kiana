//! `panel_commands` 注册表、headless 文本与 `list_for_session` 的回归测试。
//!
//! 这些是 `tui-panels-design.md` §6.3/§9.1 新增的稳定面：面板命令不能进入冻结的 24 个
//! 内置命令表，headless 文本的排序/清洗/空态必须与面板一致。

use kiana_core::slash_commands::BUILTIN_SLASH_COMMANDS;
use kiana_session::background::journal::{BackgroundTaskJournal, MemoryTaskJournal};
use kiana_session::background::{BackgroundTaskRecord, BackgroundTaskStatus};
use kiana_session::panel_commands::{
    PANEL_COMMANDS, format_task_records, is_panel_command, is_panel_command_text,
    panel_command_names,
};

fn record(
    task_id: &str,
    parent: &str,
    tool: &str,
    status: BackgroundTaskStatus,
    created: u64,
) -> BackgroundTaskRecord {
    BackgroundTaskRecord {
        version: 1,
        task_id: task_id.to_string(),
        parent_session_id: parent.to_string(),
        parent_turn_id: format!("tool:{task_id}"),
        tool: tool.to_string(),
        description: Some(format!("run {task_id}\nsecond line")),
        status,
        terminal_seq: if status.is_terminal() { 1 } else { 0 },
        terminal: None,
        output_path: format!("/tmp/{task_id}.output"),
        output_bytes: 0,
        output_truncated: false,
        notified: false,
        attempts: 1,
        created_at_ms: created,
        updated_at_ms: created,
        pid: None,
        work_backed: false,
        work_id: None,
        run_id: None,
    }
}

#[test]
fn panel_commands_are_disjoint_from_builtins_and_extensions() {
    for command in PANEL_COMMANDS {
        assert!(
            BUILTIN_SLASH_COMMANDS
                .iter()
                .all(|builtin| builtin.name != command.name),
            "panel command {name} collides with a builtin",
            name = command.name
        );
        assert_ne!(command.name, "mcp");
        assert_ne!(command.name, "llama");
    }
    assert_eq!(
        panel_command_names().collect::<Vec<_>>(),
        vec!["permissions", "tasks"]
    );
    assert!(is_panel_command("permissions"));
    assert!(is_panel_command("tasks"));
    assert!(!is_panel_command("mcp"));
}

#[test]
fn command_text_matching_is_exact_and_ignores_surrounding_space() {
    assert!(is_panel_command_text("/tasks"));
    assert!(is_panel_command_text("  /tasks   "));
    assert!(is_panel_command_text("/tasks all"));
    assert!(!is_panel_command_text("/tasksfoo"));
    assert!(!is_panel_command_text("/mcp"));
    assert!(!is_panel_command_text("hello /tasks"));
}

#[test]
fn empty_task_list_reports_the_empty_state() {
    assert_eq!(format_task_records(&[]), "No background tasks.");
}

#[test]
fn task_records_are_sorted_sanitized_and_filtered_by_kind() {
    let records = vec![
        record(
            "task_b",
            "session_1",
            "bash",
            BackgroundTaskStatus::Completed,
            200,
        ),
        record(
            "task_a",
            "session_1",
            "agent",
            BackgroundTaskStatus::Running,
            100,
        ),
        record(
            "task_c",
            "session_1",
            "custom",
            BackgroundTaskStatus::Failed,
            300,
        ),
    ];
    let text = format_task_records(&records);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "Background tasks (3):");
    assert!(lines[1].starts_with("  [running] agent"), "{}", lines[1]);
    assert!(lines[1].contains("task_a"));
    assert!(lines[1].contains("output: /tmp/task_a.output"));
    assert!(lines[2].starts_with("  [completed] shell"), "{}", lines[2]);
    assert!(lines[2].contains("output: /tmp/task_b.output"));
    assert!(lines[3].starts_with("  [failed] custom"), "{}", lines[3]);
    assert!(
        !lines[3].contains("output:"),
        "unknown kinds do not expose output paths"
    );
    // 描述里的换行被折叠成单个空格。
    for line in &lines[1..] {
        assert!(!line.contains('\n'));
        assert!(!line.contains("second  line"));
    }
    assert!(text.contains("\"run task_a second line\""));
}

#[tokio::test]
async fn memory_journal_lists_only_the_requested_session_in_time_order() {
    let journal = MemoryTaskJournal::new();
    journal
        .save(record(
            "task_1",
            "session_1",
            "bash",
            BackgroundTaskStatus::Running,
            100,
        ))
        .await
        .unwrap();
    journal
        .save(record(
            "task_2",
            "session_2",
            "bash",
            BackgroundTaskStatus::Completed,
            50,
        ))
        .await
        .unwrap();
    journal
        .save(record(
            "task_3",
            "session_1",
            "agent",
            BackgroundTaskStatus::Failed,
            50,
        ))
        .await
        .unwrap();

    let listed = journal.list_for_session("session_1").await.unwrap();
    let ids: Vec<&str> = listed
        .iter()
        .map(|record| record.task_id.as_str())
        .collect();
    // 同 created_at_ms 时按 task id 稳定排序。
    assert_eq!(ids, vec!["task_3", "task_1"]);
    assert!(
        journal
            .list_for_session("session_3")
            .await
            .unwrap()
            .is_empty()
    );
}

mod session_support;

use std::sync::Arc;

use kiana_config::settings::SettingsManager;
use kiana_permissions::{PermissionRule, RuleDecision};
use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use kiana_session::agent_session::AgentSession;
use kiana_session::panel_commands::execute_headless;
use kiana_session::sdk::{CreateAgentSessionOptions, NoTools, create_agent_session};
use kiana_session_core::session_manager::SessionManager;

/// 最小会话夹具：in-memory settings/session，无扩展、无工具。
async fn session_fixture() -> AgentSession {
    let temp = tempfile::tempdir().expect("temp");
    let cwd = temp.path().to_string_lossy().into_owned();
    let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    let settings_manager = SettingsManager::in_memory(Default::default());
    // `ResourceLoader` 在单线程 LocalSet 上运行（trait 不带 Send/Sync），与 SDK 一样用 Arc。
    #[allow(clippy::arc_with_non_send_sync)]
    let loader: Arc<dyn ResourceLoader> =
        Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            settings_manager: Some(settings_manager.clone()),
            no_skills: true,
            no_prompt_templates: true,
            no_themes: true,
            no_context_files: true,
            ..Default::default()
        }));
    loader.reload(None).await.expect("reload");
    let created = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(agent_dir),
        model: kiana_ai::compat::get_model("anthropic", "claude-sonnet-4-5"),
        settings_manager: Some(settings_manager),
        session_manager: Some(SessionManager::in_memory(&cwd, None, None).expect("session")),
        resource_loader: Some(loader),
        no_tools: Some(NoTools::Builtin),
        ..Default::default()
    })
    .await
    .expect("create session");
    created.session
}

#[tokio::test(flavor = "current_thread")]
async fn execute_headless_outputs_permissions_and_tasks_text_without_a_model() {
    session_support::run(async {
        let _engine = session_support::install_engine();
        let session = session_fixture().await;
        let permissions = execute_headless(&session, "/permissions")
            .await
            .expect("panel command")
            .expect("text");
        assert!(
            permissions.starts_with("Permissions\n  mode:"),
            "{permissions}"
        );
        assert!(permissions.contains("  workspace roots: "), "{permissions}");
        assert!(permissions.contains("  allow:\n"), "{permissions}");
        assert!(permissions.contains("  ask:\n"), "{permissions}");
        assert!(permissions.contains("  deny:\n"), "{permissions}");
        assert!(
            permissions.contains("  session approvals: "),
            "{permissions}"
        );

        let tasks = execute_headless(&session, "/tasks")
            .await
            .expect("panel command")
            .expect("text");
        assert_eq!(tasks, "Background tasks are not enabled for this session.");

        match execute_headless(&session, "/tasks all").await {
            Some(Err(message)) => assert_eq!(message, "Usage: /tasks"),
            other => panic!("unexpected result: {other:?}"),
        }
        assert!(execute_headless(&session, "/settings").await.is_none());
        assert!(execute_headless(&session, "hello there").await.is_none());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn permissions_text_sanitizes_repository_controlled_rule_text() {
    session_support::run(async {
        let _engine = session_support::install_engine();
        let session = session_fixture().await;
        session.set_permission_rules(vec![PermissionRule::Tool {
            pattern: "rule\u{1b}]0;spoof\u{7}".to_string(),
            decision: RuleDecision::Deny,
            reason: None,
        }]);
        let text = execute_headless(&session, "/permissions")
            .await
            .expect("panel command")
            .expect("text");
        assert!(text.contains("rule"), "{text}");
        assert!(
            !text.contains('\u{1b}'),
            "escape sequence leaked into /permissions: {text:?}"
        );
        assert!(
            !text.contains('\u{7}'),
            "control character leaked into /permissions: {text:?}"
        );
    })
    .await;
}
