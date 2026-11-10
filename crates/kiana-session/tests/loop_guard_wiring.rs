//! 第 8 周共享接线：`CreateAgentSessionOptions` 的 loop guard / budget 必须落到
//! `AgentOptions` 并可从 `AgentSession::agent()` 读回。
//!
//! 完整的检测/硬停/预算行为在 `kiana-agent-core` 的 `loop_guard_budget` 测试里覆盖；
//! 这里只验证会话工厂的透传，不重复实现 agent loop。

mod session_support;

use std::sync::{Arc, Mutex};

use kiana_agent_core::budget::BudgetConfig;
use kiana_agent_core::loop_guard::{LoopGuard, LoopGuardConfig};
use kiana_ai::providers::mock::{MockLlmAdmin, MockResponse, MockStep, MockTrajectory};
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_session::agent_session::PromptOptions;
use kiana_session::sdk::{CreateAgentSessionOptions, create_agent_session};
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;

use session_support::{empty_loader, run, sdk_fixture, settings};

#[tokio::test]
async fn loop_guard_and_budget_are_attached_to_the_agent() {
    run(async {
        let fixture = sdk_fixture(None).await;
        let loader =
            empty_loader(&fixture.cwd, &fixture.agent_dir, &fixture.settings_manager).await;
        let session_manager =
            SessionManager::in_memory(&fixture.cwd, None, None).expect("in-memory session manager");
        let loop_guard = Arc::new(Mutex::new(LoopGuard::new(LoopGuardConfig {
            stop_on_detection: true,
            ..Default::default()
        })));
        let budget_config = BudgetConfig::default()
            .with_max_turns(3)
            .with_max_tokens(1_000);

        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(fixture.cwd.clone()),
            agent_dir: Some(fixture.agent_dir.clone()),
            model: fixture.model(Some("faux-1")),
            model_runtime: Some(fixture.model_runtime.clone()),
            settings_manager: Some(fixture.settings_manager.clone()),
            session_manager: Some(session_manager),
            resource_loader: Some(loader),
            loop_guard: Some(Arc::clone(&loop_guard)),
            budget_config: Some(budget_config),
            ..Default::default()
        })
        .await
        .expect("create session");

        let agent = created.session.agent();
        assert!(
            agent.loop_guard().is_some(),
            "loop guard must be passed into AgentOptions"
        );
        assert!(
            agent.budget().is_some(),
            "budget config must produce a BudgetHandle"
        );
        created.session.dispose();
        fixture.cleanup();
    })
    .await;
}

/// 端到端：连续 5 次相同的 bash 调用触发 reminder（第 3 次后的请求里）与硬停（第 5 次）。
#[tokio::test]
async fn repeated_tool_calls_trigger_reminder_then_hard_stop() {
    run(async {
        let fixture = sdk_fixture(None).await;
        let admin = MockLlmAdmin::new();
        let mut trajectory = MockTrajectory::new();
        for _ in 0..5 {
            let arguments = [("command".to_string(), json!("true"))]
                .into_iter()
                .collect::<serde_json::Map<_, _>>();
            trajectory = trajectory.push(MockStep::Respond(MockResponse::tool_call(
                "bash", arguments,
            )));
        }
        trajectory = trajectory.then_text("done");
        admin.register("loop", trajectory);
        let handle = admin
            .provider("loop", Default::default())
            .expect("mock provider");

        let auth_storage = AuthStorage::in_memory(auth_data_from_value(json!({})));
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::new(auth_storage)),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await
        .expect("model runtime");
        model_runtime
            .register_native_provider(Arc::clone(&handle.provider))
            .expect("register provider");
        let _ = model_runtime
            .refresh(kiana_ai::models::ModelsRefreshOptions::default())
            .await;

        let loader =
            empty_loader(&fixture.cwd, &fixture.agent_dir, &fixture.settings_manager).await;
        let session_manager =
            SessionManager::in_memory(&fixture.cwd, None, None).expect("in-memory session manager");
        // 默认策略：第 3 次先 reminder，第 5 次硬停（`stop_on_detection` 保持 false）。
        let loop_guard = Arc::new(Mutex::new(LoopGuard::new(LoopGuardConfig::default())));
        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(fixture.cwd.clone()),
            agent_dir: Some(fixture.agent_dir.clone()),
            model: Some(handle.model.clone()),
            model_runtime: Some(model_runtime),
            settings_manager: Some(fixture.settings_manager.clone()),
            session_manager: Some(session_manager),
            resource_loader: Some(loader),
            loop_guard: Some(Arc::clone(&loop_guard)),
            ..Default::default()
        })
        .await
        .expect("create session");

        // `Agent::execute_run` 会把 `LoopDetected` 合成为一条 assistant 失败消息（run
        // 可能同步返回 Ok），因此错误断言读 transcript，而不是只看 `prompt` 的返回值。
        let _ = created
            .session
            .prompt("repeat".to_string(), PromptOptions::default())
            .await;
        let stopped = loop_guard
            .lock()
            .unwrap()
            .stopped()
            .cloned()
            .expect("loop guard should hard-stop at the fifth identical call");
        // 具体检测器取决于输出是否变化（相同输出会先触发 shell stagnation）；
        // 这里只要求“重复动作被检测并硬停”。
        assert!(
            stopped.count >= 3 && !stopped.message.is_empty(),
            "unexpected detection: {stopped:?}"
        );
        let error_message =
            created
                .session
                .messages()
                .into_iter()
                .find_map(|message| match message {
                    kiana_agent_core::types::AgentMessage::Standard(
                        kiana_ai::types::Message::Assistant(assistant),
                    ) if assistant.stop_reason == kiana_ai::types::StopReason::Error => {
                        assistant.error_message
                    }
                    _ => None,
                });
        assert!(
            error_message
                .as_deref()
                .is_some_and(|message| message.to_lowercase().contains("loop detected")),
            "the hard stop should surface as an assistant error: {error_message:?}"
        );
        let requests = admin.requests("loop").expect("request records");
        assert!(
            requests
                .iter()
                .any(|request| request.prompt_contains("<loop-guard")),
            "the reminder must be injected before a later request: {:?}",
            requests
                .iter()
                .map(|request| request.prompt_text.clone())
                .collect::<Vec<_>>()
        );
        created.session.dispose();
        fixture.cleanup();
    })
    .await;
}

/// SDK 路径（`CreateAgentSessionOptions` 默认带 `initial_active_tool_names`）也必须把
/// 配置里的 `edit` 映射到模型路由后的 `apply_patch`，否则工具会在激活阶段被丢掉。
#[tokio::test]
async fn sdk_apply_patch_routing_activates_the_routed_editor() {
    run(async {
        let fixture = sdk_fixture(Some(settings(json!({
            "tools": {"applyPatch": {"models": ["*"]}}
        }))))
        .await;
        let loader =
            empty_loader(&fixture.cwd, &fixture.agent_dir, &fixture.settings_manager).await;
        let session_manager =
            SessionManager::in_memory(&fixture.cwd, None, None).expect("in-memory session manager");
        let created = create_agent_session(CreateAgentSessionOptions {
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

        let active = created.session.get_active_tool_names();
        assert!(
            active.iter().any(|name| name == "apply_patch"),
            "apply_patch should be active through the SDK path: {active:?}"
        );
        assert!(
            !active.iter().any(|name| name == "edit"),
            "edit must not be active at the same time: {active:?}"
        );
        created.session.dispose();
        fixture.cleanup();
    })
    .await;
}

#[tokio::test]
async fn defaults_keep_loop_guard_and_budget_disabled() {
    run(async {
        let fixture = sdk_fixture(None).await;
        let loader =
            empty_loader(&fixture.cwd, &fixture.agent_dir, &fixture.settings_manager).await;
        let session_manager =
            SessionManager::in_memory(&fixture.cwd, None, None).expect("in-memory session manager");
        let created = create_agent_session(CreateAgentSessionOptions {
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

        let agent = created.session.agent();
        assert!(agent.loop_guard().is_none());
        assert!(agent.budget().is_none());
        created.session.dispose();
        fixture.cleanup();
    })
    .await;
}

// ---------------------------------------------------------------------------
// 深度修复：shipped CLI 的 services 入口必须能从 settings 启用 loop guard / 预算
// ---------------------------------------------------------------------------

use kiana_agent_core::loop_guard::LoopDetectorKind;
use kiana_session::agent_session_services::{
    AgentSessionServices, CreateAgentSessionFromServicesOptions, create_agent_session_from_services,
};
use session_support::SdkFixture;

async fn services_fixture(
    settings_value: serde_json::Value,
) -> (SdkFixture, kiana_session::sdk::CreateAgentSessionResult) {
    let fixture = sdk_fixture(Some(settings(settings_value))).await;
    let loader = empty_loader(&fixture.cwd, &fixture.agent_dir, &fixture.settings_manager).await;
    let session_manager =
        SessionManager::in_memory(&fixture.cwd, None, None).expect("in-memory session manager");
    let services = AgentSessionServices {
        cwd: fixture.cwd.clone(),
        agent_dir: fixture.agent_dir.clone(),
        model_runtime: fixture.model_runtime.clone(),
        settings_manager: fixture.settings_manager.clone(),
        resource_loader: loader,
        diagnostics: Vec::new(),
    };
    let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
        services,
        session_manager,
        session_start_event: None,
        model: fixture.model(Some("faux-1")),
        thinking_level: None,
        scoped_models: Vec::new(),
        tools: None,
        exclude_tools: None,
        no_tools: None,
        custom_tools: Vec::new(),
        loop_guard: None,
        budget_config: None,
    })
    .await
    .expect("create session");
    (fixture, created)
}

#[tokio::test]
async fn services_path_reads_loop_guard_and_budget_from_settings() {
    run(async {
        let (fixture, created) = services_fixture(json!({
            "loopGuard": {
                "enabled": true,
                "stopOnDetection": true,
                "thresholds": {
                    "consecutiveToolCalls": { "reminderAt": 1, "stopAt": 2 }
                }
            },
            "budget": { "maxTurns": 4, "maxTokens": 5000 }
        }))
        .await;

        let agent = created.session.agent();
        let guard = agent.loop_guard().expect("loop guard from settings");
        let guard = guard.lock().unwrap();
        assert!(guard.config().enabled);
        assert!(guard.config().stop_on_detection);
        assert_eq!(
            guard.config().thresholds[&LoopDetectorKind::ConsecutiveToolCalls].stop_at,
            2
        );
        drop(guard);

        let budget = agent.budget().expect("budget from settings");
        let budget = budget.lock().unwrap();
        assert_eq!(budget.config().max_turns, Some(4));
        assert_eq!(budget.config().max_tokens, Some(5000));
        drop(budget);
        created.session.dispose();
        drop(fixture);
    })
    .await;
}

#[tokio::test]
async fn services_path_enables_a_conservative_loop_guard_by_default() {
    run(async {
        let (fixture, created) = services_fixture(json!({})).await;
        let agent = created.session.agent();
        let guard = agent
            .loop_guard()
            .expect("loop guard must be enabled by default through the services path");
        let guard = guard.lock().unwrap();
        assert!(guard.config().enabled);
        assert!(
            !guard.config().stop_on_detection,
            "default must warn before stopping"
        );
        drop(guard);
        assert!(
            agent.budget().is_none(),
            "budget stays disabled until caps are configured"
        );
        created.session.dispose();
        drop(fixture);
    })
    .await;
}

#[tokio::test]
async fn services_path_honors_an_explicit_loop_guard_override() {
    run(async {
        let fixture = sdk_fixture(Some(settings(json!({
            "loopGuard": { "enabled": true, "stopOnDetection": false }
        }))))
        .await;
        let loader =
            empty_loader(&fixture.cwd, &fixture.agent_dir, &fixture.settings_manager).await;
        let session_manager =
            SessionManager::in_memory(&fixture.cwd, None, None).expect("in-memory session manager");
        let services = AgentSessionServices {
            cwd: fixture.cwd.clone(),
            agent_dir: fixture.agent_dir.clone(),
            model_runtime: fixture.model_runtime.clone(),
            settings_manager: fixture.settings_manager.clone(),
            resource_loader: loader,
            diagnostics: Vec::new(),
        };
        let explicit = Arc::new(Mutex::new(LoopGuard::new(LoopGuardConfig {
            stop_on_detection: true,
            ..Default::default()
        })));
        let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
            services,
            session_manager,
            session_start_event: None,
            model: fixture.model(Some("faux-1")),
            thinking_level: None,
            scoped_models: Vec::new(),
            tools: None,
            exclude_tools: None,
            no_tools: None,
            custom_tools: Vec::new(),
            loop_guard: Some(Arc::clone(&explicit)),
            budget_config: None,
        })
        .await
        .expect("create session");
        let guard = created
            .session
            .agent()
            .loop_guard()
            .expect("explicit guard");
        assert!(Arc::ptr_eq(&guard, &explicit));
        assert!(guard.lock().unwrap().config().stop_on_detection);
        created.session.dispose();
        drop(fixture);
    })
    .await;
}
