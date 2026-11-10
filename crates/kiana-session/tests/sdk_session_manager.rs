//! `test/sdk-session-manager.test.ts`：createAgentSession 的 session manager 默认值与 cwd。
#![allow(clippy::arc_with_non_send_sync)]

mod session_support;

use kiana_ai::types::{UserContent, UserContentBlock};
use kiana_session::sdk::{CreateAgentSessionOptions, create_agent_session};
use kiana_session_core::session_manager::{SessionManager, get_default_session_dir_path};
use serde_json::json;

use session_support::{empty_loader, run, sdk_fixture};

fn tool_output_text(
    result: &kiana_agent_core::types::AgentToolResult<serde_json::Value>,
) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

#[tokio::test]
async fn uses_agent_dir_for_the_default_persisted_session_path() {
    run(async {
        let fixture = sdk_fixture(None).await;
        let loader =
            empty_loader(&fixture.cwd, &fixture.agent_dir, &fixture.settings_manager).await;
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(fixture.cwd.clone()),
            agent_dir: Some(fixture.agent_dir.clone()),
            model: fixture.model(Some("faux-1")),
            model_runtime: Some(fixture.model_runtime.clone()),
            settings_manager: Some(fixture.settings_manager.clone()),
            resource_loader: Some(loader),
            ..Default::default()
        })
        .await
        .expect("create session");
        let session = result.session;
        let expected_dir = get_default_session_dir_path(&fixture.cwd, &fixture.agent_dir);
        assert_eq!(
            session.session_manager().get_session_dir(),
            expected_dir.as_str()
        );
        let session_file = session.session_file().expect("session file");
        assert!(session_file.starts_with(&format!("{expected_dir}/")));
        session.dispose();
        fixture.cleanup();
    })
    .await;
}

#[tokio::test]
async fn keeps_an_explicit_session_manager_override() {
    run(async {
        let fixture = sdk_fixture(None).await;
        let loader =
            empty_loader(&fixture.cwd, &fixture.agent_dir, &fixture.settings_manager).await;
        let session_manager = SessionManager::in_memory(&fixture.cwd, None, None).expect("session");
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(fixture.cwd.clone()),
            agent_dir: Some(fixture.agent_dir.clone()),
            model: fixture.model(Some("faux-1")),
            model_runtime: Some(fixture.model_runtime.clone()),
            settings_manager: Some(fixture.settings_manager.clone()),
            session_manager: Some(session_manager),
            resource_loader: Some(loader),
            ..Default::default()
        })
        .await
        .expect("create session");
        let session = result.session;
        assert!(!session.with_session_manager_mut(|manager| manager.is_persisted()));
        session.dispose();
        fixture.cleanup();
    })
    .await;
}

#[tokio::test]
async fn derives_cwd_from_an_explicit_session_manager_when_cwd_is_omitted() {
    run(async {
        let fixture = sdk_fixture(None).await;
        let session_cwd = fixture.temp_dir.path().join("session-project");
        std::fs::create_dir_all(&session_cwd).expect("session cwd");
        let session_cwd = session_cwd.to_string_lossy().into_owned();
        let session_manager = SessionManager::in_memory(&session_cwd, None, None).expect("session");
        let loader =
            empty_loader(&fixture.cwd, &fixture.agent_dir, &fixture.settings_manager).await;
        let result = create_agent_session(CreateAgentSessionOptions {
            agent_dir: Some(fixture.agent_dir.clone()),
            model: fixture.model(Some("faux-1")),
            model_runtime: Some(fixture.model_runtime.clone()),
            settings_manager: Some(fixture.settings_manager.clone()),
            session_manager: Some(session_manager),
            resource_loader: Some(loader),
            ..Default::default()
        })
        .await
        .expect("create session");
        let session = result.session;
        assert!(
            session
                .system_prompt()
                .contains(&format!("<cwd>\n{session_cwd}\n</cwd>"))
        );
        let bash = session
            .state()
            .tools
            .into_iter()
            .find(|tool| tool.name == "bash")
            .expect("bash tool");
        let result = (bash.execute)
            .execute("test", json!({ "command": "pwd" }), None, None)
            .await
            .expect("bash");
        let output = tool_output_text(&result);
        let expected = std::fs::canonicalize(&session_cwd).expect("canonical session cwd");
        let actual = std::fs::canonicalize(output.trim()).expect("canonical pwd");
        assert_eq!(actual, expected);
        session.dispose();
        fixture.cleanup();
    })
    .await;
}

#[tokio::test]
async fn exposes_current_session_state_to_the_builtin_bash_tool() {
    run(async {
        let fixture = sdk_fixture(None).await;
        let loader = empty_loader(&fixture.cwd, &fixture.agent_dir, &fixture.settings_manager).await;
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(fixture.cwd.clone()),
            agent_dir: Some(fixture.agent_dir.clone()),
            model: fixture.model(Some("faux-1")),
            thinking_level: Some(kiana_ai::types::ModelThinkingLevel::High),
            model_runtime: Some(fixture.model_runtime.clone()),
            settings_manager: Some(fixture.settings_manager.clone()),
            resource_loader: Some(loader),
            ..Default::default()
        })
        .await
        .expect("create session");
        let session = result.session;
        assert!(session.session_file().is_some());
        assert!(session.system_prompt().contains(
            "You can inspect KIANA_* environment variables for current model and session details."
        ));
        let bash = session
            .state()
            .tools
            .into_iter()
            .find(|tool| tool.name == "bash")
            .expect("bash tool");
        let result = (bash.execute)
            .execute(
                "test",
                json!({
                    "command": "printf '%s\\n' \"$KIANA_SESSION_ID\" \"$KIANA_SESSION_FILE\" \"$KIANA_PROVIDER\" \"$KIANA_MODEL\" \"$KIANA_REASONING_LEVEL\""
                }),
                None,
                None,
            )
            .await
            .expect("bash");
        let output = tool_output_text(&result);
        let model = fixture.model(Some("faux-1")).expect("model");
        let thinking = serde_json::to_value(session.thinking_level())
            .expect("thinking")
            .as_str()
            .expect("string")
            .to_string();
        assert_eq!(
            output.trim().split('\n').collect::<Vec<_>>(),
            vec![
                session.session_id(),
                session.session_file().expect("file"),
                model.provider,
                model.id,
                thinking,
            ]
        );
        session.dispose();
        fixture.cleanup();
    })
    .await;
}

// `UserContent` 在本测试里仅用于类型对齐（未直接构造）。
#[allow(dead_code)]
fn _unused_content(_: UserContent) {}
