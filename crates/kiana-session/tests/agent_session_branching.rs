//! `test/agent-session-branching.test.ts`：AgentSession forking（TS 因缺 API_KEY 而 skip；
//! 这里用 faux provider 覆盖同样的行为）。
#![allow(clippy::arc_with_non_send_sync)]

mod session_support;

use kiana_session::agent_session::PromptOptions;
use kiana_session::agent_session_runtime::ForkSessionOptions;
use kiana_session_core::session_manager::SessionManager;

use session_support::{RuntimeHostOptions, run, runtime_host};

fn msg_text(message: &kiana_agent_core::types::AgentMessage) -> String {
    session_support::message_text(message)
}

#[tokio::test]
async fn allows_forking_from_single_message() {
    run(async {
        let host = runtime_host(RuntimeHostOptions::default()).await;
        let session = host.runtime.session();
        session
            .prompt("Say hello".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        session.wait_for_idle().await;
        let user_messages = session.get_user_messages_for_forking();
        assert_eq!(user_messages.len(), 1);
        assert_eq!(user_messages[0].1, "Say hello");
        let result = host
            .runtime
            .fork(&user_messages[0].0, None)
            .await
            .expect("fork");
        assert!(!result.cancelled);
        assert_eq!(result.selected_text.as_deref(), Some("Say hello"));
        let new_session = host.runtime.session();
        assert!(
            new_session
                .messages()
                .iter()
                .filter(|message| message.role() != "system")
                .count()
                == 0
        );
        let file = new_session.session_file().expect("session file");
        assert!(!std::path::Path::new(&file).exists());
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn supports_in_memory_forking_in_no_session_mode() {
    run(async {
        let dir = tempfile::tempdir().expect("dir");
        let host = runtime_host(RuntimeHostOptions {
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            session_manager: Some(
                SessionManager::in_memory(dir.path().to_str().unwrap(), None, None)
                    .expect("in-memory"),
            ),
            ..Default::default()
        })
        .await;
        let session = host.runtime.session();
        assert!(session.session_file().is_none());
        session
            .prompt("Say hi".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        session.wait_for_idle().await;
        let user_messages = session.get_user_messages_for_forking();
        assert_eq!(user_messages.len(), 1);
        assert!(!session.messages().is_empty());
        let result = host
            .runtime
            .fork(&user_messages[0].0, None)
            .await
            .expect("fork");
        assert!(!result.cancelled);
        assert_eq!(result.selected_text.as_deref(), Some("Say hi"));
        let new_session = host.runtime.session();
        assert!(
            new_session
                .messages()
                .iter()
                .filter(|message| message.role() != "system")
                .count()
                == 0
        );
        assert!(new_session.session_file().is_none());
        host.dispose().await;
    })
    .await;
}

#[tokio::test]
async fn forks_from_middle_of_conversation() {
    run(async {
        let host = runtime_host(RuntimeHostOptions::default()).await;
        let session = host.runtime.session();
        session
            .prompt("Say one".to_string(), PromptOptions::default())
            .await
            .expect("p1");
        session.wait_for_idle().await;
        session
            .prompt("Say two".to_string(), PromptOptions::default())
            .await
            .expect("p2");
        session.wait_for_idle().await;
        session
            .prompt("Say three".to_string(), PromptOptions::default())
            .await
            .expect("p3");
        session.wait_for_idle().await;
        let user_messages = session.get_user_messages_for_forking();
        assert_eq!(user_messages.len(), 3);
        let result = host
            .runtime
            .fork(
                &user_messages[1].0,
                Some(ForkSessionOptions {
                    position: Some(kiana_ext_api::events::ForkPosition::Before),
                    ..Default::default()
                }),
            )
            .await
            .expect("fork");
        assert!(!result.cancelled);
        assert_eq!(result.selected_text.as_deref(), Some("Say two"));
        let new_session = host.runtime.session();
        let messages: Vec<_> = new_session
            .messages()
            .into_iter()
            .filter(|message| message.role() != "system")
            .collect();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role(), "user");
        assert_eq!(messages[1].role(), "assistant");
        assert_eq!(msg_text(&messages[0]), "Say one");
        host.dispose().await;
    })
    .await;
}
