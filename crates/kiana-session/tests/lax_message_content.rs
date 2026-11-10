//! `test/suite/lax-message-content.test.ts`（AgentSession 级三条用例）的 Rust 移植。
//!
//! 未类型化扩展/工具可能给出 `content: null` 或缺省 `content`；`AgentSession` 在摄入边界
//! 归一为 `[]`，保证渲染、压缩与 provider 转换永远不会看到 `null`（issues #6259、#6276）。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
mod session_support;

use std::sync::Arc;

use common::*;
use indexmap::IndexMap;
use kiana_agent_core::types::AgentMessage;
use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventKind, ExtensionEventResult, MessageEndEventResult,
};
use kiana_ext_api::types::{ExtensionApi, ExtensionFactory};
use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use kiana_session::agent_session::PromptOptions;
use kiana_session::sdk::{CreateAgentSessionOptions, create_agent_session};
use kiana_session_core::messages::{CustomMessage, CustomRole};
use kiana_session_core::session_manager::SessionEntry;
use serde_json::Value;

/// `message_end` 扩展：把 assistant 消息替换为 `{...message, content: null}`（未类型化扩展）。
fn null_content_replacement() -> ExtensionFactory {
    factory(|kiana: Arc<dyn ExtensionApi>| {
        let _ = kiana.on(
            ExtensionEventKind::MessageEnd,
            standard(|event, _ctx| {
                Box::pin(async move {
                    let ExtensionEvent::MessageEnd(message_end) = event else {
                        return Ok(None);
                    };
                    if message_end.message.role() != "assistant" {
                        return Ok(None);
                    }
                    let Value::Object(mut object) =
                        serde_json::to_value(&message_end.message).unwrap_or(Value::Null)
                    else {
                        return Ok(None);
                    };
                    object.insert("content".to_string(), Value::Null);
                    let custom: IndexMap<String, Value> = object.into_iter().collect();
                    Ok(Some(ExtensionEventResult::MessageEnd(
                        MessageEndEventResult {
                            message: Some(AgentMessage::Custom(Box::new(custom))),
                        },
                    )))
                })
            }),
        );
    })
}

/// TS: "normalizes null content in message_end extension replacements"
#[tokio::test]
async fn normalizes_null_content_in_message_end_extension_replacements() {
    run(async {
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![null_content_replacement()],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("hello")]);

        harness
            .session
            .prompt("hi".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let messages = harness.session.messages();
        let assistants: Vec<_> = messages
            .iter()
            .filter(|message| message.role() == "assistant")
            .collect();
        assert_eq!(assistants.len(), 1, "one assistant message");
        // 仍必须是标准 assistant 消息（不是未类型的 Custom 兜底）。
        let assistant = assistants[0].as_assistant().expect("standard assistant");
        assert!(assistant.content.is_empty());

        // 会话历史里也必须是归一后的标准消息。
        let stored = harness
            .session_entries()
            .into_iter()
            .filter_map(|entry| match entry {
                SessionEntry::Message(entry) => Some(entry.message),
                _ => None,
            })
            .find(|message| message.role() == "assistant")
            .expect("persisted assistant message");
        assert!(
            stored.as_assistant().is_some(),
            "persisted message stays a standard assistant message"
        );
        assert!(stored.as_assistant().expect("assistant").content.is_empty());
    })
    .await;
}

/// TS: "normalizes null content in custom messages from extensions"
#[tokio::test]
async fn normalizes_null_content_in_custom_messages_from_extensions() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;

        harness
            .session
            .send_custom_message(
                CustomMessage {
                    role: CustomRole::Custom,
                    custom_type: "test".to_string(),
                    content: Value::Null,
                    display: false,
                    details: None,
                    timestamp: 0.0,
                },
                None,
                None,
            )
            .await
            .expect("send custom message");

        let messages = harness.session.messages();
        let custom: Vec<_> = messages
            .iter()
            .filter(|message| message.role() == "custom")
            .collect();
        assert_eq!(custom.len(), 1, "one custom message");
        let AgentMessage::Custom(object) = custom[0] else {
            panic!("custom message must stay a custom AgentMessage");
        };
        assert_eq!(object.get("content"), Some(&Value::Array(Vec::new())));

        // 持久化的 custom_message 条目同样归一。
        let stored = harness
            .session_entries()
            .into_iter()
            .find_map(|entry| match entry {
                SessionEntry::CustomMessage(entry) => Some(entry),
                _ => None,
            })
            .expect("persisted custom message entry");
        assert_eq!(stored.content, Value::Array(Vec::new()));
    })
    .await;
}

const NO_CONTENT_TOOL_EXTENSION: &str = r#"
export default function(kiana) {
    kiana.registerTool({
        name: "web_search",
        label: "Web Search",
        description: "Custom tool that returns a result without content",
        parameters: { type: "object", properties: {}, additionalProperties: false },
        execute: async () => ({ details: {} }),
    });
}
"#;

/// TS: "normalizes tool results from untyped tools that omit content"
///
/// 这条用例在 kiana-extensions 的 `JsToolExecute` 层验证：JS 工具返回 `{ details: {} }`
/// （没有 `content`）时必须被补成 `content: []`，而不是 `invalid tool result: missing
/// field content` 的 isError 结果。TS 的会话级版本还会走一遍 `prompt()`；Rust 的测试
/// harness 用 tokio runtime 驱动 LocalSet 时，`Agent::begin_run` 的 `runtime.spawn` 任务
/// 不带 LocalSet 上下文（`spawn_local` 会 panic），这是 harness 的执行环境差异，与本次
/// 归一化无关，因此这里直接执行已注册的工具定义。
#[tokio::test]
async fn normalizes_tool_results_from_untyped_tools_that_omit_content() {
    run(async {
        let fixture = session_support::sdk_fixture(None).await;
        // 文件扩展加载需要把引擎登记为 loader 的“当前引擎”。
        kiana_extensions::loader::install(fixture.engine.rc()).expect("loader install");
        let extension_dir = fixture.temp_dir.path().join("extensions");
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        let extension_path = extension_dir.join("no-content-tool.ts");
        std::fs::write(&extension_path, NO_CONTENT_TOOL_EXTENSION).expect("extension file");

        #[allow(clippy::arc_with_non_send_sync)]
        let loader: Arc<dyn ResourceLoader> =
            Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
                cwd: fixture.cwd.clone(),
                agent_dir: fixture.agent_dir.clone(),
                settings_manager: Some(fixture.settings_manager.clone()),
                additional_extension_paths: vec![extension_path.to_string_lossy().into_owned()],
                no_skills: true,
                no_prompt_templates: true,
                no_themes: true,
                no_context_files: true,
                ..Default::default()
            }));
        loader.reload(None).await.expect("loader reload");
        {
            let result = loader.get_extensions();
            assert_eq!(result.errors, Vec::new(), "extension load errors");
            assert_eq!(result.extensions.len(), 1, "one loaded extension");
        }

        // sdk_fixture 的原生 provider 不会标记 configured auth；这里用扩展 provider
        // （与 `test/sdk-client-identity` 相同的方式）注册 faux API，授权立即生效。
        let model = fixture.model(Some("faux-1")).expect("faux model");
        let runtime = kiana_config::models::runtime::ModelRuntime::create(
            kiana_config::models::runtime::CreateModelRuntimeOptions {
                credentials: Some(Arc::new(fixture.auth_storage.clone())),
                models_path: kiana_config::models::runtime::ModelsPathOption::Disabled,
                allow_model_network: false,
                refresh_on_create: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("model runtime");
        let faux = fixture.faux.clone();
        runtime
            .register_provider(
                "faux",
                kiana_config::models::composer::ProviderConfigInput {
                    api: Some(model.api.clone()),
                    base_url: Some(model.base_url.clone()),
                    api_key: Some("faux-key".to_string()),
                    stream_simple: Some(Arc::new(move |model, context, options| {
                        kiana_ai::models::ProviderStreams::stream_simple(
                            &faux, model, context, options,
                        )
                    })),
                    ..Default::default()
                },
            )
            .expect("register provider");

        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(fixture.cwd.clone()),
            agent_dir: Some(fixture.agent_dir.clone()),
            model: Some(model),
            model_runtime: Some(runtime),
            settings_manager: Some(fixture.settings_manager.clone()),
            resource_loader: Some(loader),
            session_manager: Some(
                kiana_session_core::session_manager::SessionManager::in_memory(
                    &fixture.cwd,
                    None,
                    None,
                )
                .expect("session manager"),
            ),
            ..Default::default()
        })
        .await
        .expect("create session");
        let session = created.session;

        let runner = session.extension_runner();
        let definition = runner
            .get_tool_definition("web_search")
            .expect("registered JS tool");
        let execute = Arc::clone(&definition.execute);
        let ctx = runner.create_tool_context("call-1", None);
        let result = execute
            .execute("call-1", serde_json::json!({}), None, None, ctx)
            .await
            .expect("tool executes");
        assert!(result.content.is_empty(), "content: {:?}", result.content);
        assert_eq!(result.details, serde_json::json!({}));
        session.dispose();
    })
    .await;
}
