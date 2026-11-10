//! `test/suite/agent-session-prompt.test.ts` 中剩余验收用例的 Rust 移植。
//!
//! 未移植：
//! - "throws when prompting without a model"：Rust `Agent` 没有清空模型的公开 API。
//! - "keeps the same system prompt for every request of the run"：TS 依赖就地修改
//!   `event.systemPromptOptions.sections`（对象身份）；Rust 事件按值传递，无等价语义。
#![allow(clippy::arc_with_non_send_sync)]

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use common::*;
use kiana_ai::providers::faux::{FauxResponseStep, faux_assistant_message};
use kiana_ai::types::{
    ImageContent, ImageKind, ModelImageInputLimits, ModelImageResizeOptions, ModelInputLimits,
    PromptCacheTier, UserContent,
};
use kiana_ai::types::{Message, UserContentBlock};
use kiana_ext_api::events::{ExtensionEvent, ExtensionEventKind, ExtensionEventResult};
use kiana_ext_api::types::{ExtensionApi, RegisterCommandOptions, SendUserMessageOptions};
use kiana_resources::loader::{PromptsResult, SkillsResult};
use kiana_resources::prompts::PromptTemplate;
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions};
use kiana_session_core::messages::{CustomMessage, CustomRole};
use serde_json::json;

const TINY_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn source_info(path: &str) -> kiana_core::source_info::SourceInfo {
    kiana_core::source_info::create_synthetic_source_info(
        path,
        kiana_core::source_info::SyntheticSourceInfoOptions {
            source: "local".to_string(),
            scope: Some(kiana_core::source_info::SourceScope::Project),
            origin: Some(kiana_core::source_info::SourceOrigin::TopLevel),
            base_dir: None,
        },
    )
}

fn review_template() -> PromptTemplate {
    PromptTemplate {
        name: "review".to_string(),
        description: "Review template".to_string(),
        argument_hint: None,
        content: "Review this code: $1".to_string(),
        source_info: source_info("/virtual/review.md"),
        file_path: "/virtual/review.md".to_string(),
    }
}

fn create_bmp_100x100() -> Vec<u8> {
    let width = 100usize;
    let height = 100usize;
    let row_bytes = width * 3;
    let size = 54 + row_bytes * height;
    let mut buffer = vec![0u8; size];
    buffer[0] = b'B';
    buffer[1] = b'M';
    buffer[2..6].copy_from_slice(&(size as u32).to_le_bytes());
    buffer[10..14].copy_from_slice(&54u32.to_le_bytes());
    buffer[14..18].copy_from_slice(&40u32.to_le_bytes());
    buffer[18..22].copy_from_slice(&(width as i32).to_le_bytes());
    buffer[22..26].copy_from_slice(&(height as i32).to_le_bytes());
    buffer[26..28].copy_from_slice(&1u16.to_le_bytes());
    buffer[28..30].copy_from_slice(&24u16.to_le_bytes());
    buffer[34..38].copy_from_slice(&((row_bytes * height) as u32).to_le_bytes());
    for y in 0..height {
        for x in 0..width {
            let offset = 54 + y * row_bytes + x * 3;
            buffer[offset] = (x % 256) as u8;
            buffer[offset + 1] = (y % 256) as u8;
            buffer[offset + 2] = 0x40;
        }
    }
    buffer
}

fn png_dimensions(data: &str) -> (u32, u32) {
    let bytes = kiana_tui::terminal_image::base64_decode(data);
    assert!(bytes.len() >= 24, "not a PNG");
    assert_eq!(&bytes[1..4], b"PNG");
    (
        u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
        u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
    )
}

fn user_texts_and_image_count(context: &kiana_ai::types::Context) -> (String, usize) {
    let mut texts = Vec::new();
    let mut images = 0;
    for message in &context.messages {
        if let Message::User(user) = message {
            match &user.content {
                UserContent::Text(text) => texts.push(text.clone()),
                UserContent::Blocks(blocks) => {
                    for block in blocks {
                        match block {
                            UserContentBlock::Text(text) => texts.push(text.text.clone()),
                            UserContentBlock::Image(_) => images += 1,
                        }
                    }
                }
            }
        }
    }
    (texts.join("\n"), images)
}

/// TS: "preserves image attachments in the provider context"
#[tokio::test]
async fn preserves_image_attachments_in_the_provider_context() {
    run(async {
        let seen = Arc::new(Mutex::new((String::new(), 0usize)));
        let sink = Arc::clone(&seen);
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let sink = Arc::clone(&sink);
            let result = user_texts_and_image_count(input.context.as_context());
            async move {
                *sink.lock().unwrap() = result;
                faux_assistant_message("ok", Default::default())
            }
        })]);
        harness
            .session
            .prompt(
                "describe".to_string(),
                PromptOptions {
                    images: Some(vec![ImageContent {
                        kind_image: ImageKind::Image,
                        data: TINY_PNG_BASE64.to_string(),
                        mime_type: "image/png".to_string(),
                    }]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap().1, 1);
        harness.cleanup();
    })
    .await;
}

/// TS: "uses the model selected by before_agent_start for image normalization"（#9631）
#[tokio::test]
async fn uses_the_model_selected_by_before_agent_start_for_image_normalization() {
    run(async {
        let strict_model_slot: Arc<Mutex<Option<kiana_ai::types::Model>>> =
            Arc::new(Mutex::new(None));
        let slot = Arc::clone(&strict_model_slot);
        let harness = create_harness(HarnessOptions {
            models: Some(vec![
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "wide".to_string(),
                    ..Default::default()
                },
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "strict".to_string(),
                    ..Default::default()
                },
            ]),
            extension_factories: vec![factory(move |kiana| {
                let slot = Arc::clone(&slot);
                let api = Arc::clone(&kiana);
                let _ = kiana.on(
                    ExtensionEventKind::BeforeAgentStart,
                    standard(move |_event, _ctx| {
                        let kiana = Arc::clone(&api);
                        let slot = Arc::clone(&slot);
                        Box::pin(async move {
                            let model = slot.lock().unwrap().clone().expect("strict model");
                            let _ = kiana.set_model(model).await;
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        let mut strict_model = harness.get_model(Some("strict")).expect("strict model");
        strict_model.input_limits = Some(ModelInputLimits {
            max_request_bytes: None,
            images: Some(ModelImageInputLimits {
                resize: Some(ModelImageResizeOptions {
                    max_width: Some(10),
                    max_height: Some(10),
                    max_bytes: Some(500_000),
                    jpeg_quality: Some(70),
                }),
                ..Default::default()
            }),
        });
        *strict_model_slot.lock().unwrap() = Some(strict_model);
        harness.set_responses(vec![faux_text_response("done")]);

        harness
            .session
            .prompt(
                "inspect".to_string(),
                PromptOptions {
                    images: Some(vec![ImageContent {
                        kind_image: ImageKind::Image,
                        data: base64::engine::general_purpose::STANDARD
                            .encode(create_bmp_100x100()),
                        mime_type: "image/bmp".to_string(),
                    }]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(
            harness.session.model().map(|model| model.id),
            Some("strict".to_string())
        );
        let user = harness
            .session
            .messages()
            .into_iter()
            .find(|message| message.role() == "user")
            .expect("user message");
        let image = match user {
            kiana_agent_core::types::AgentMessage::Standard(Message::User(user)) => match user
                .content
            {
                UserContent::Blocks(blocks) => blocks.into_iter().find_map(|block| match block {
                    UserContentBlock::Image(image) => Some(image),
                    _ => None,
                }),
                UserContent::Text(_) => None,
            },
            _ => None,
        }
        .expect("normalized image");
        assert_eq!(png_dimensions(&image.data), (10, 10));
        harness.cleanup();
    })
    .await;
}

/// TS: "expands skill commands before sending the prompt"
#[tokio::test]
async fn expands_skill_commands_before_sending_the_prompt() {
    run(async {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let skill_path = temp_dir.path().join("test-skill.md");
        std::fs::write(&skill_path, "# Test Skill\n\nUse the skill body.").expect("skill file");
        let skill_path = skill_path.to_string_lossy().into_owned();
        let base_dir = temp_dir.path().to_string_lossy().into_owned();
        let harness = create_harness(HarnessOptions {
            skills: Some(SkillsResult {
                skills: vec![kiana_ext_api::system_prompt::Skill {
                    name: "test".to_string(),
                    description: "Test skill".to_string(),
                    file_path: skill_path.clone(),
                    base_dir: base_dir.clone(),
                    source_info: source_info(&skill_path),
                    disable_model_invocation: false,
                }],
                diagnostics: Vec::new(),
            }),
            ..Default::default()
        })
        .await;
        let expanded = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&expanded);
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let sink = Arc::clone(&sink);
            let (texts, _) = user_texts_and_image_count(input.context.as_context());
            async move {
                *sink.lock().unwrap() = texts;
                faux_assistant_message("ok", Default::default())
            }
        })]);

        harness
            .session
            .prompt(
                "/skill:test explain this".to_string(),
                PromptOptions::default(),
            )
            .await
            .unwrap();

        let expanded = expanded.lock().unwrap();
        assert!(expanded.contains("<skill name=\"test\" location=\""));
        assert!(expanded.contains("Use the skill body."));
        assert!(expanded.contains("explain this"));
        harness.cleanup();
    })
    .await;
}

/// TS: "expands prompt templates before sending the prompt"
#[tokio::test]
async fn expands_prompt_templates_before_sending_the_prompt() {
    run(async {
        let harness = create_harness(HarnessOptions {
            prompts: Some(PromptsResult {
                prompts: vec![review_template()],
                diagnostics: Vec::new(),
            }),
            ..Default::default()
        })
        .await;
        let expanded = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&expanded);
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let sink = Arc::clone(&sink);
            let (texts, _) = user_texts_and_image_count(input.context.as_context());
            async move {
                *sink.lock().unwrap() = texts;
                faux_assistant_message("ok", Default::default())
            }
        })]);

        harness
            .session
            .prompt("/review src/index.ts".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(*expanded.lock().unwrap(), "Review this code: src/index.ts");
        harness.cleanup();
    })
    .await;
}

/// TS: "sendUserMessage can opt into prompt template expansion"
#[tokio::test]
async fn send_user_message_can_opt_into_prompt_template_expansion() {
    run(async {
        let harness = create_harness(HarnessOptions {
            prompts: Some(PromptsResult {
                prompts: vec![review_template()],
                diagnostics: Vec::new(),
            }),
            ..Default::default()
        })
        .await;
        let expanded = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&expanded);
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let sink = Arc::clone(&sink);
            let (texts, _) = user_texts_and_image_count(input.context.as_context());
            async move {
                *sink.lock().unwrap() = texts;
                faux_assistant_message("ok", Default::default())
            }
        })]);

        harness
            .session
            .send_user_message(
                UserContent::Text("/review src/index.ts".to_string()),
                Some(SendUserMessageOptions {
                    deliver_as: None,
                    expand_prompt_templates: Some(true),
                }),
            )
            .await
            .unwrap();

        assert_eq!(*expanded.lock().unwrap(), "Review this code: src/index.ts");
        harness.cleanup();
    })
    .await;
}

/// TS: "extension sendUserMessage can opt into extension command dispatch"
#[tokio::test]
async fn extension_send_user_message_can_opt_into_command_dispatch() {
    run(async {
        let api_slot: Arc<Mutex<Option<Arc<dyn ExtensionApi>>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&api_slot);
        let runs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let runs_sink = Arc::clone(&runs);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                *slot.lock().unwrap() = Some(Arc::clone(&kiana));
                let runs = Arc::clone(&runs_sink);
                kiana.register_command(
                    "testcmd",
                    RegisterCommandOptions {
                        description: Some("Test command".to_string()),
                        get_argument_completions: None,
                        handler: Arc::new(move |args, _ctx| {
                            let runs = Arc::clone(&runs);
                            Box::pin(async move {
                                runs.lock().unwrap().push(args);
                            })
                        }),
                    },
                );
            })],
            ..Default::default()
        })
        .await;
        let api = api_slot.lock().unwrap().clone().expect("extension api");
        api.send_user_message(
            UserContent::Text("/testcmd hello world".to_string()),
            Some(SendUserMessageOptions {
                deliver_as: None,
                expand_prompt_templates: Some(true),
            }),
        );
        for _ in 0..2000 {
            if !runs.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert_eq!(*runs.lock().unwrap(), vec!["hello world".to_string()]);
        assert!(harness.session.messages().is_empty());
        assert_eq!(harness.get_pending_response_count(), 0);
        harness.cleanup();
    })
    .await;
}

/// TS: "throws when prompted during manual compaction"
#[tokio::test]
async fn throws_when_prompted_during_manual_compaction() {
    run(async {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1 }
            }))),
            extension_factories: vec![factory({
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                move |kiana| {
                    let started = Arc::clone(&started);
                    let release = Arc::clone(&release);
                    let _ = kiana.on(
                        ExtensionEventKind::SessionBeforeCompact,
                        standard(move |event, _ctx| {
                            let started = Arc::clone(&started);
                            let release = Arc::clone(&release);
                            Box::pin(async move {
                                let ExtensionEvent::Session(
                                    kiana_ext_api::events::SessionEvent::BeforeCompact(before),
                                ) = event
                                else {
                                    return Ok(None);
                                };
                                started.notify_one();
                                release.notified().await;
                                Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                                    kiana_ext_api::events::SessionBeforeCompactResult {
                                        cancel: None,
                                        compaction: Some(
                                            kiana_compaction::compaction::CompactionResult {
                                                summary: "manual compacted".to_string(),
                                                first_kept_entry_id: before
                                                    .preparation
                                                    .first_kept_entry_id
                                                    .clone(),
                                                tokens_before: before.preparation.tokens_before,
                                                estimated_tokens_after: None,
                                                usage: None,
                                                details: Some(json!({})),
                                            },
                                        ),
                                    },
                                )))
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("one"), faux_text_response("two")]);
        harness
            .session
            .prompt("first".to_string(), PromptOptions::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("second".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let session = harness.session.clone();
        let compact = tokio::task::spawn_local(async move { session.compact(None).await });
        started.notified().await;
        let error = harness
            .session
            .prompt("third".to_string(), PromptOptions::default())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry."
        );
        release.notify_one();
        compact.await.unwrap().unwrap();
        harness.cleanup();
    })
    .await;
}

/// TS: "queues a custom message sent while before_agent_start runs into the starting run"
#[tokio::test]
async fn queues_custom_message_sent_while_before_agent_start_runs() {
    run(async {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                move |kiana| {
                    let started = Arc::clone(&started);
                    let release = Arc::clone(&release);
                    let _ = kiana.on(
                        ExtensionEventKind::BeforeAgentStart,
                        standard(move |event, _ctx| {
                            let started = Arc::clone(&started);
                            let release = Arc::clone(&release);
                            Box::pin(async move {
                                let is_first_custom = matches!(
                                    &event,
                                    ExtensionEvent::BeforeAgentStart(
                                        kiana_ext_api::events::BeforeAgentStartEvent::Custom(custom)
                                    ) if custom.message.custom_type == "first"
                                );
                                if !is_first_custom {
                                    return Ok(None);
                                }
                                started.notify_one();
                                release.notified().await;
                                Ok(None)
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&requests);
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let sink = Arc::clone(&sink);
            let text =
                serde_json::to_string(&input.context.as_context().messages).unwrap_or_default();
            async move {
                sink.lock().unwrap().push(text);
                faux_assistant_message("handled both", Default::default())
            }
        })]);

        let session = harness.session.clone();
        let first = tokio::task::spawn_local(async move {
            session
                .send_custom_message(
                    custom_message("first", "task one finished"),
                    Some(true),
                    None,
                )
                .await
        });
        started.notified().await;
        assert!(!harness.session.is_idle());
        let session = harness.session.clone();
        let second = tokio::task::spawn_local(async move {
            session
                .send_custom_message(
                    custom_message("second", "task two finished"),
                    Some(true),
                    None,
                )
                .await
        });
        release.notify_one();
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let text = &requests[0];
        let first_index = text.find("task one finished").expect("first message");
        let second_index = text.find("task two finished").expect("second message");
        assert!(first_index < second_index);
        harness.cleanup();
    })
    .await;
}

fn custom_message(custom_type: &str, content: &str) -> CustomMessage {
    CustomMessage {
        role: CustomRole::Custom,
        custom_type: custom_type.to_string(),
        content: json!(content),
        display: true,
        details: None,
        timestamp: now_ms() as f64,
    }
}

/// TS: "aborts a run when abort is requested while before_agent_start runs"
#[tokio::test]
async fn aborts_a_run_when_abort_requested_during_before_agent_start() {
    run(async {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                move |kiana| {
                    let started = Arc::clone(&started);
                    let release = Arc::clone(&release);
                    let _ = kiana.on(
                        ExtensionEventKind::BeforeAgentStart,
                        standard(move |_event, _ctx| {
                            let started = Arc::clone(&started);
                            let release = Arc::clone(&release);
                            Box::pin(async move {
                                started.notify_one();
                                release.notified().await;
                                Ok(None)
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("should not complete")]);

        let session = harness.session.clone();
        let run = tokio::task::spawn_local(async move {
            session
                .send_custom_message(
                    custom_message("task-done", "background task finished"),
                    Some(true),
                    None,
                )
                .await
        });
        started.notified().await;
        let abort_session = harness.session.clone();
        let aborted = tokio::task::spawn_local(async move { abort_session.abort().await });
        release.notify_one();
        run.await.unwrap().unwrap();
        aborted.await.unwrap();

        let last = harness
            .session
            .messages()
            .into_iter()
            .next_back()
            .expect("last message");
        assert_eq!(last.role(), "assistant");
        match last {
            kiana_agent_core::types::AgentMessage::Standard(Message::Assistant(assistant)) => {
                assert_eq!(assistant.stop_reason, kiana_ai::types::StopReason::Aborted);
            }
            other => panic!("unexpected message: {other:?}"),
        }
        assert!(harness.session.is_idle());
        harness.cleanup();
    })
    .await;
}

/// 回归（评审 finding #4）：TS 的 `SessionManager.append*` 直接写文件，失败会 reject
/// `prompt()`；Rust 修复前监听器把写错误静默丢弃，prompt 仍成功返回。
#[cfg(unix)]
#[tokio::test]
async fn persistence_failure_rejects_the_prompt() {
    use std::os::unix::fs::PermissionsExt;

    run(async {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let cwd = temp_dir.path().to_string_lossy().into_owned();
        let session_dir = tempfile::tempdir().expect("session dir");
        let session_dir_path = session_dir.path().to_string_lossy().into_owned();
        let manager = kiana_session_core::session_manager::SessionManager::create(
            &cwd,
            Some(&session_dir_path),
            None,
        )
        .expect("session manager");
        let session_file = manager
            .get_session_file()
            .expect("persisted session file")
            .to_string();
        // 让首次落盘（create_new/append）失败。
        std::fs::set_permissions(&session_dir_path, std::fs::Permissions::from_mode(0o555))
            .expect("chmod session dir");
        let harness = create_harness(HarnessOptions {
            session_manager: Some(manager),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            "ok",
            Default::default(),
        ))]);

        let error = harness
            .session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .expect_err("persistence failure must reject the prompt");
        // Node 的 fs 异常消息使用小写描述：`EACCES: permission denied, open '<path>'`。
        assert!(
            error.to_string().contains("EACCES") && error.to_string().contains("permission denied"),
            "unexpected error: {error}"
        );
        // 没有写出会话文件，也没有把失败当成成功。
        assert!(!std::path::Path::new(&session_file).exists());
        assert!(harness.session.is_idle());

        std::fs::set_permissions(&session_dir_path, std::fs::Permissions::from_mode(0o755)).ok();
        harness.cleanup();
    })
    .await;
}

/// 回归（评审 finding #3）：技能文件 frontmatter 解析失败时，TS 把
/// `readFileSync`/`stripFrontmatter` 放在同一个 try 里 —— 上报 `skill_expansion`
/// 错误并原样返回命令文本。Rust 修复前会静默展开含 frontmatter 的原始内容。
#[tokio::test]
async fn skill_expansion_reports_invalid_frontmatter_and_keeps_command() {
    run(async {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let skill_path = temp_dir.path().join("broken-skill.md");
        std::fs::write(&skill_path, "---\ninvalid: [unclosed\n---\nbody text").expect("skill file");
        let skill_path = skill_path.to_string_lossy().into_owned();
        let base_dir = temp_dir.path().to_string_lossy().into_owned();
        let harness = create_harness(HarnessOptions {
            skills: Some(SkillsResult {
                skills: vec![kiana_ext_api::system_prompt::Skill {
                    name: "broken".to_string(),
                    description: "Broken skill".to_string(),
                    file_path: skill_path.clone(),
                    base_dir,
                    source_info: source_info(&skill_path),
                    disable_model_invocation: false,
                }],
                diagnostics: Vec::new(),
            }),
            ..Default::default()
        })
        .await;
        let errors: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&errors);
        let _unsubscribe = harness
            .session
            .extension_runner()
            .on_error(Arc::new(move |error| {
                sink.lock()
                    .unwrap()
                    .push((error.event.clone(), error.error.clone()));
            }));
        let sent = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&sent);
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let sink = Arc::clone(&sink);
            let (texts, _) = user_texts_and_image_count(input.context.as_context());
            async move {
                *sink.lock().unwrap() = texts;
                faux_assistant_message("ok", Default::default())
            }
        })]);

        harness
            .session
            .prompt(
                "/skill:broken explain this".to_string(),
                PromptOptions::default(),
            )
            .await
            .unwrap();

        // 原样发送（不展开），并上报 yaml 的 `error.message`。
        assert_eq!(*sent.lock().unwrap(), "/skill:broken explain this");
        let errors = errors.lock().unwrap();
        let (event, message) = errors
            .iter()
            .find(|(event, _)| event == "skill_expansion")
            .expect("skill_expansion error");
        assert_eq!(event, "skill_expansion");
        assert!(
            message.contains("Flow sequence in block collection must be sufficiently indented"),
            "unexpected error: {message}"
        );
        harness.cleanup();
    })
    .await;
}

/// 回归（评审 finding #3）：Node 的 `readFileSync(path, "utf-8")` 把非法字节解码成
/// U+FFFD 而不是报错；Rust 修复前用 `read_to_string` 直接失败。
#[tokio::test]
async fn skill_expansion_decodes_invalid_utf8_lossily() {
    run(async {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let skill_path = temp_dir.path().join("binary-skill.md");
        std::fs::write(&skill_path, b"# Skill\n\nbody \xff\xfe end").expect("skill file");
        let skill_path = skill_path.to_string_lossy().into_owned();
        let base_dir = temp_dir.path().to_string_lossy().into_owned();
        let harness = create_harness(HarnessOptions {
            skills: Some(SkillsResult {
                skills: vec![kiana_ext_api::system_prompt::Skill {
                    name: "binary".to_string(),
                    description: "Binary skill".to_string(),
                    file_path: skill_path.clone(),
                    base_dir,
                    source_info: source_info(&skill_path),
                    disable_model_invocation: false,
                }],
                diagnostics: Vec::new(),
            }),
            ..Default::default()
        })
        .await;
        let sent = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&sent);
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let sink = Arc::clone(&sink);
            let (texts, _) = user_texts_and_image_count(input.context.as_context());
            async move {
                *sink.lock().unwrap() = texts;
                faux_assistant_message("ok", Default::default())
            }
        })]);

        harness
            .session
            .prompt("/skill:binary".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let sent = sent.lock().unwrap();
        assert!(
            sent.contains("<skill name=\"binary\" location=\""),
            "{sent}"
        );
        assert!(
            sent.contains('\u{FFFD}'),
            "expected replacement chars: {sent}"
        );
        harness.cleanup();
    })
    .await;
}

/// 回归（评审 finding #3）：读取失败时的错误文本必须是 Node fs 的
/// `ENOENT: no such file or directory, open '<path>'`，而不是 Rust 的 io 文案。
#[tokio::test]
async fn skill_expansion_reports_missing_file_with_node_error_message() {
    run(async {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let skill_path = temp_dir
            .path()
            .join("missing-skill.md")
            .to_string_lossy()
            .into_owned();
        let base_dir = temp_dir.path().to_string_lossy().into_owned();
        let harness = create_harness(HarnessOptions {
            skills: Some(SkillsResult {
                skills: vec![kiana_ext_api::system_prompt::Skill {
                    name: "missing".to_string(),
                    description: "Missing skill".to_string(),
                    file_path: skill_path.clone(),
                    base_dir,
                    source_info: source_info(&skill_path),
                    disable_model_invocation: false,
                }],
                diagnostics: Vec::new(),
            }),
            ..Default::default()
        })
        .await;
        let errors: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&errors);
        let _unsubscribe = harness
            .session
            .extension_runner()
            .on_error(Arc::new(move |error| {
                sink.lock()
                    .unwrap()
                    .push((error.event.clone(), error.error.clone()));
            }));
        let sent = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&sent);
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let sink = Arc::clone(&sink);
            let (texts, _) = user_texts_and_image_count(input.context.as_context());
            async move {
                *sink.lock().unwrap() = texts;
                faux_assistant_message("ok", Default::default())
            }
        })]);

        harness
            .session
            .prompt("/skill:missing hi".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(*sent.lock().unwrap(), "/skill:missing hi");
        let errors = errors.lock().unwrap();
        let (_, message) = errors
            .iter()
            .find(|(event, _)| event == "skill_expansion")
            .expect("skill_expansion error");
        assert_eq!(
            *message,
            format!("ENOENT: no such file or directory, open '{skill_path}'")
        );
        harness.cleanup();
    })
    .await;
}

// 保持未使用导入不告警。
#[allow(dead_code)]
fn _unused(_: AgentSessionEvent, _: PromptCacheTier, _: AtomicBool) {
    let _ = Ordering::SeqCst;
}
