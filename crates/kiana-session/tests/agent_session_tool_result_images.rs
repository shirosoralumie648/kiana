//! `test/suite/agent-session-tool-result-images.test.ts` 的 Rust 移植。
//!
//! TS 用 `vi.mock` 拦截 `normalizeToolResultImages` 断言选项；Rust 没有可注入点，改为断言
//! 可观察结果：
//! - `images.autoResize=false` 时不做尺寸缩放（仅格式转换）；
//! - 打开 autoResize 后，模型 `inputLimits.images.resize` 的 10x10 限额生效。

mod common;

use std::sync::Arc;

use base64::Engine as _;
use common::*;
use kiana_agent_core::types::{AgentMessage, AgentTool, AgentToolError, AgentToolExecute};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message,
};
use kiana_ai::types::{
    AssistantContentBlock, ImageContent, ImageKind, Message, ModelImageInputLimits,
    ModelImageResizeOptions, ModelInputLimits, StopReason, TextContent, TextKind, UserContentBlock,
};
use kiana_session::agent_session::PromptOptions;
use serde_json::json;

/// 100x100 24bpp BMP（与 `kiana-core/tests/image_process.rs` 的夹具相同）。
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

fn bmp_base64() -> String {
    base64::engine::general_purpose::STANDARD.encode(create_bmp_100x100())
}

struct ScreenshotTool {
    data: String,
}

impl AgentToolExecute for ScreenshotTool {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<kiana_agent_core::types::AgentToolResult<serde_json::Value>, AgentToolError>,
    > {
        let data = self.data.clone();
        Box::pin(async move {
            Ok(kiana_agent_core::types::AgentToolResult {
                content: vec![
                    UserContentBlock::Text(TextContent {
                        kind_text: TextKind::Text,
                        text: "captured".to_string(),
                        text_signature: None,
                    }),
                    UserContentBlock::Image(ImageContent {
                        kind_image: ImageKind::Image,
                        data,
                        mime_type: "image/bmp".to_string(),
                    }),
                ],
                details: serde_json::Value::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn screenshot_tool(data: String) -> AgentTool {
    AgentTool {
        name: "screenshot".to_string(),
        label: "Screenshot".to_string(),
        description: "Return a screenshot".to_string(),
        parameters: json!({"type": "object", "properties": {}}),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(ScreenshotTool { data }),
    }
}

fn png_dimensions(data: &str) -> (u32, u32) {
    let bytes = kiana_tui::terminal_image::base64_decode(data);
    assert!(bytes.len() >= 24, "not a PNG");
    assert_eq!(&bytes[1..4], b"PNG");
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    (width, height)
}

fn screenshot_response() -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message(
        AssistantContentBlock::ToolCall(kiana_ai::providers::faux::faux_tool_call(
            "screenshot",
            indexmap::IndexMap::new(),
        )),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
}

fn last_tool_image(harness: &Harness) -> ImageContent {
    let results: Vec<_> = harness
        .session
        .messages()
        .into_iter()
        .filter_map(|message| match message {
            AgentMessage::Standard(Message::ToolResult(result)) => Some(result),
            _ => None,
        })
        .collect();
    results
        .last()
        .expect("tool result")
        .content
        .iter()
        .find_map(|block| match block {
            UserContentBlock::Image(image) => Some(image.clone()),
            _ => None,
        })
        .expect("image content")
}

/// TS: "passes image settings and the current model profile to tool result normalization"
#[tokio::test]
async fn applies_model_image_resize_options_to_tool_results() {
    run(async {
        let bmp = bmp_base64();
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![screenshot_tool(bmp.clone())]),
            settings: Some(settings_from_json(json!({
                "images": { "autoResize": false }
            }))),
            ..Default::default()
        })
        .await;
        assert!(harness.session.model().is_some());
        let mut model = harness.session.model().expect("model");
        model.input_limits = Some(ModelInputLimits {
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
        harness.agent.set_model(model);

        // Phase 1：全局 autoResize=false → 只做格式转换，不按模型 profile 缩放。
        harness.set_responses(vec![screenshot_response(), faux_text_response("done")]);
        harness
            .session
            .prompt("take a screenshot".to_string(), PromptOptions::default())
            .await
            .unwrap();
        let image = last_tool_image(&harness);
        assert_eq!(png_dimensions(&image.data), (100, 100));

        // Phase 2：开启 autoResize 后，模型的 10x10 profile 生效。
        harness
            .settings_manager
            .apply_overrides(settings_from_json(json!({
                "images": { "autoResize": true }
            })));
        harness.set_responses(vec![screenshot_response(), faux_text_response("done")]);
        harness
            .session
            .prompt(
                "take another screenshot".to_string(),
                PromptOptions::default(),
            )
            .await
            .unwrap();
        let image = last_tool_image(&harness);
        assert_ne!(image.data, bmp, "image should be resized");
        assert_eq!(png_dimensions(&image.data), (10, 10));
        harness.cleanup();
    })
    .await;
}
