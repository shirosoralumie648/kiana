//! `read.ts` 的移植测试。
//!
//! 主体是 `tests/fixtures/file-tools/read_ls.golden.json` 里的黄金用例（由 TS 参考实现
//! 生成）：文本截断、offset/limit 的 JS 数值语义、WHATWG 无效 UTF-8 替换、图片魔数识别、
//! BMP→PNG、逐字的截断脚注与 `details`/`structuredContent`。另覆盖 `tools.test.ts` 的
//! cwd 解析、`block-images`、`image-resize-callers` 的语义与中止行为。

mod file_tools_test_support;

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use file_tools_test_support::{
    TestContext, execute, execute_with_signal, image_blocks, test_model, text_output,
};
use kiana_ai::types::JsonValue;
use kiana_tools::read::{
    LocalReadOperations, ReadOperations, ReadToolOptions, create_read_tool,
    create_read_tool_definition, to_read_output,
};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// 1x1 红色 PNG（最小合法 PNG）。
const TINY_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==";

fn golden() -> JsonValue {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/file-tools/read_ls.golden.json"
    );
    serde_json::from_str(&fs::read_to_string(path).expect("read golden")).expect("parse golden")
}

/// base64 解码（测试用；`kiana_tui` 暴露了 Node 兼容解码器）。
fn base64_decode(value: &str) -> Vec<u8> {
    kiana_tui::terminal_image::base64_decode(value)
}

#[tokio::test]
async fn read_matches_the_reference_goldens() {
    let golden = golden();
    let cases = golden["read"].as_array().expect("read cases");
    assert!(!cases.is_empty());
    let temp = TempDir::new().expect("temp");
    let definition = create_read_tool_definition(temp.path().to_string_lossy(), None);
    let ctx = TestContext::new(temp.path());

    for (index, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().expect("name");
        let original_path = case["path"].as_str().expect("path");
        let bytes = base64_decode(case["contentB64"].as_str().expect("content"));
        let file_path = temp.path().join(format!("read-{index}-{name}.bin"));
        fs::write(&file_path, &bytes).expect("write fixture");

        let mut params = case["args"].clone();
        params["path"] = json!(file_path.to_string_lossy());
        let result = execute(&definition, params, &ctx).await;

        if let Some(expected_error) = case["error"].as_str() {
            let error = result.expect_err(name);
            assert_eq!(error.message, expected_error, "error mismatch for {name}");
            continue;
        }

        let result =
            result.unwrap_or_else(|error| panic!("{name}: unexpected error {}", error.message));
        let expected_text = case["text"]
            .as_str()
            .expect("text")
            .replace(original_path, &file_path.to_string_lossy());
        assert_eq!(
            text_output(&result),
            expected_text,
            "text mismatch for {name}"
        );

        assert_eq!(
            result.details,
            case["details"].clone(),
            "details mismatch for {name}"
        );

        // 文本 structuredContent 就是文本；图片对象必须逐字段一致（data 是处理后的产物）。
        let expected_structured = &case["structuredContent"];
        if let Some(expected) = expected_structured.as_object() {
            let actual = result
                .structured_content
                .as_ref()
                .expect("structured content")
                .as_object()
                .expect("image structured content");
            assert_eq!(actual.get("type"), expected.get("type"), "{name}");
            assert_eq!(actual.get("mimeType"), expected.get("mimeType"), "{name}");
            assert_eq!(actual.get("note"), expected.get("note"), "{name}");
            assert_eq!(actual.get("data"), expected.get("data"), "{name}");
        } else {
            assert_eq!(
                result.structured_content,
                Some(json!(expected_text)),
                "structured content mismatch for {name}"
            );
        }

        let expected_blocks = case["blocks"].as_array().expect("blocks");
        let actual_images = image_blocks(&result);
        let expected_images: Vec<&JsonValue> = expected_blocks
            .iter()
            .filter(|block| block["type"] == "image")
            .collect();
        assert_eq!(
            actual_images.len(),
            expected_images.len(),
            "image block count mismatch for {name}"
        );
        for (actual, expected) in actual_images.iter().zip(expected_images) {
            assert_eq!(
                actual.mime_type,
                expected["mimeType"].as_str().expect("mimeType"),
                "{name}"
            );
            assert_eq!(
                actual.data.len(),
                expected["dataLength"].as_u64().expect("dataLength") as usize,
                "image data length mismatch for {name}"
            );
            assert!(
                actual
                    .data
                    .starts_with(expected["dataPrefix"].as_str().expect("dataPrefix")),
                "image data prefix mismatch for {name}"
            );
        }
    }
}

#[tokio::test]
async fn missing_file_reports_a_node_style_enoent_error() {
    let temp = TempDir::new().expect("temp");
    let definition = create_read_tool_definition(temp.path().to_string_lossy(), None);
    let ctx = TestContext::new(temp.path());
    let missing = temp.path().join("nope.txt");
    let error = execute(&definition, json!({ "path": missing }), &ctx)
        .await
        .expect_err("missing file");
    // Node 26: fs.access(path, R_OK) → `ENOENT: no such file or directory, access '<path>'`。
    assert_eq!(
        error.message,
        format!(
            "ENOENT: no such file or directory, access '{}'",
            missing.display()
        )
    );
}

#[tokio::test]
async fn reading_a_directory_reports_the_file_handle_read_error_without_a_path() {
    let temp = TempDir::new().expect("temp");
    let directory = temp.path().join("dir");
    fs::create_dir(&directory).expect("mkdir");
    let definition = create_read_tool_definition(temp.path().to_string_lossy(), None);
    let error = execute(
        &definition,
        json!({ "path": directory }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("directory");
    // `detectSupportedImageMimeTypeFromFile` 用 FileHandle.read：syscall 是 read 且不带 path。
    assert_eq!(
        error.message,
        "EISDIR: illegal operation on a directory, read"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn long_names_and_symlink_loops_use_libuv_codes_and_descriptions() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("temp");
    let definition = create_read_tool_definition(temp.path().to_string_lossy(), None);
    let ctx = TestContext::new(temp.path());

    let long = temp.path().join("a".repeat(300));
    let error = execute(&definition, json!({ "path": long }), &ctx)
        .await
        .expect_err("name too long");
    assert_eq!(
        error.message,
        format!("ENAMETOOLONG: name too long, access '{}'", long.display())
    );

    let loop_a = temp.path().join("loop-a");
    let loop_b = temp.path().join("loop-b");
    symlink(&loop_b, &loop_a).expect("symlink a");
    symlink(&loop_a, &loop_b).expect("symlink b");
    let error = execute(&definition, json!({ "path": loop_a }), &ctx)
        .await
        .expect_err("symlink loop");
    assert_eq!(
        error.message,
        format!(
            "ELOOP: too many symbolic links encountered, access '{}'",
            loop_a.display()
        )
    );
}

#[tokio::test]
async fn fractional_offsets_that_hit_the_first_line_limit_throw_a_type_error() {
    let temp = TempDir::new().expect("temp");
    let file = temp.path().join("huge-first-line.txt");
    fs::write(&file, format!("{}\nsecond\n", "x".repeat(60_000))).expect("write");
    let definition = create_read_tool_definition(temp.path().to_string_lossy(), None);
    let error = execute(
        &definition,
        json!({ "path": file, "offset": 1.5 }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("fractional offset");
    // TS: `allLines[startLine]` 是 undefined，`Buffer.byteLength(undefined)` 抛 TypeError。
    assert_eq!(
        error.message,
        "The \"string\" argument must be of type string or an instance of Buffer or ArrayBuffer. Received undefined"
    );
    assert_eq!(error.name.as_deref(), Some("TypeError"));
}

#[tokio::test]
async fn read_uses_the_context_cwd() {
    let temp = TempDir::new().expect("temp");
    fs::write(temp.path().join("ctx-cwd-read.txt"), "hello from ctx.cwd").expect("write");
    let definition = create_read_tool_definition("/", None);
    let ctx = TestContext::new(temp.path());
    let result = execute(&definition, json!({ "path": "ctx-cwd-read.txt" }), &ctx)
        .await
        .expect("read");
    assert_eq!(text_output(&result), "hello from ctx.cwd");
}

#[tokio::test]
async fn non_vision_models_get_the_verbatim_note() {
    let temp = TempDir::new().expect("temp");
    let png = base64_decode(TINY_PNG_BASE64);
    let image_path = temp.path().join("image.png");
    fs::write(&image_path, png).expect("write png");
    let definition = create_read_tool_definition(temp.path().to_string_lossy(), None);
    let ctx = TestContext::with_model(
        temp.path(),
        test_model(&[kiana_ai::types::ModelInputModality::Text]),
    );
    let result = execute(&definition, json!({ "path": image_path }), &ctx)
        .await
        .expect("read image");
    assert!(text_output(&result).contains(
        "[Current model does not support images. The image will be omitted from this request.]"
    ));
}

#[tokio::test]
async fn model_resize_profile_is_forwarded_to_image_processing() {
    let temp = TempDir::new().expect("temp");
    let png = base64_decode(TINY_PNG_BASE64);
    let image_path = temp.path().join("image.png");
    fs::write(&image_path, png).expect("write png");
    let definition = create_read_tool_definition(temp.path().to_string_lossy(), None);

    // 默认（无模型元数据）能内联。
    let default_result = execute(
        &definition,
        json!({ "path": image_path }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("read");
    assert_eq!(image_blocks(&default_result).len(), 1);

    // 模型要求 maxBytes=10：无法缩到限额内 → 单文本块 + 逐字提示。
    let mut model = test_model(&[
        kiana_ai::types::ModelInputModality::Text,
        kiana_ai::types::ModelInputModality::Image,
    ]);
    model.input_limits = Some(kiana_ai::types::ModelInputLimits {
        max_request_bytes: None,
        images: Some(kiana_ai::types::ModelImageInputLimits {
            resize: Some(kiana_ai::types::ModelImageResizeOptions {
                max_width: Some(64),
                max_height: Some(64),
                max_bytes: Some(10),
                jpeg_quality: None,
            }),
            max_per_message: None,
            max_per_request: None,
        }),
    });
    let result = execute(
        &definition,
        json!({ "path": image_path }),
        &TestContext::with_model(temp.path(), model),
    )
    .await
    .expect("read with tiny limit");
    assert_eq!(image_blocks(&result).len(), 0);
    assert!(
        text_output(&result)
            .contains("[Image omitted: could not be resized below the inline image size limit.]")
    );
}

#[tokio::test]
async fn auto_resize_false_keeps_the_original_image_bytes() {
    let temp = TempDir::new().expect("temp");
    let png = base64_decode(TINY_PNG_BASE64);
    let image_path = temp.path().join("image.png");
    fs::write(&image_path, &png).expect("write png");
    let definition = create_read_tool_definition(
        temp.path().to_string_lossy(),
        Some(ReadToolOptions {
            auto_resize_images: false,
            ..ReadToolOptions::default()
        }),
    );
    let result = execute(
        &definition,
        json!({ "path": image_path }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("read");
    let images = image_blocks(&result);
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].data, TINY_PNG_BASE64);
}

#[tokio::test]
async fn pre_aborted_read_rejects_with_operation_aborted() {
    let temp = TempDir::new().expect("temp");
    let definition = create_read_tool_definition(temp.path().to_string_lossy(), None);
    let ctx = TestContext::new(temp.path());
    let signal = CancellationToken::new();
    signal.cancel();
    let error = execute_with_signal(&definition, json!({ "path": "x" }), &signal, &ctx)
        .await
        .expect_err("aborted");
    assert_eq!(error.message, "Operation aborted");
}

/// 在 `access` 里挂起的操作。
struct HangingOperations {
    started: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ReadOperations for HangingOperations {
    async fn read_file(
        &self,
        _absolute_path: &std::path::Path,
    ) -> Result<Vec<u8>, kiana_tools::read::ToolOperationError> {
        Ok(Vec::new())
    }

    async fn access(
        &self,
        _absolute_path: &std::path::Path,
    ) -> Result<(), kiana_tools::read::ToolOperationError> {
        self.started.notify_waiters();
        std::future::pending::<()>().await;
        Ok(())
    }
}

#[tokio::test]
async fn abort_during_an_in_flight_read_rejects_immediately() {
    let temp = TempDir::new().expect("temp");
    let started = Arc::new(tokio::sync::Notify::new());
    let definition = create_read_tool_definition(
        temp.path().to_string_lossy(),
        Some(ReadToolOptions {
            operations: Some(Arc::new(HangingOperations {
                started: Arc::clone(&started),
            })),
            ..ReadToolOptions::default()
        }),
    );
    let ctx = TestContext::new(temp.path());
    let signal = CancellationToken::new();
    let waiting = started.notified();
    let task = tokio::spawn({
        let definition = definition.clone();
        let ctx = Arc::clone(&ctx);
        let signal = signal.clone();
        async move { execute_with_signal(&definition, json!({ "path": "x" }), &signal, &ctx).await }
    });
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("access started");
    signal.cancel();
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("abort resolves")
        .expect("join")
        .expect_err("aborted");
    assert_eq!(error.message, "Operation aborted");
}

#[tokio::test]
async fn custom_operations_and_detected_mime_are_used() {
    struct FancyOperations;

    #[async_trait]
    impl ReadOperations for FancyOperations {
        async fn read_file(
            &self,
            _absolute_path: &std::path::Path,
        ) -> Result<Vec<u8>, kiana_tools::read::ToolOperationError> {
            Ok(b"remote image".to_vec())
        }

        async fn access(
            &self,
            _absolute_path: &std::path::Path,
        ) -> Result<(), kiana_tools::read::ToolOperationError> {
            Ok(())
        }

        async fn detect_image_mime_type(
            &self,
            _absolute_path: &std::path::Path,
        ) -> Result<Option<String>, kiana_tools::read::ToolOperationError> {
            Ok(Some("image/png".to_string()))
        }
    }

    let temp = TempDir::new().expect("temp");
    let definition = create_read_tool_definition(
        temp.path().to_string_lossy(),
        Some(ReadToolOptions {
            operations: Some(Arc::new(FancyOperations)),
            ..ReadToolOptions::default()
        }),
    );
    let result = execute(
        &definition,
        json!({ "path": "remote.png" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("read");
    assert!(text_output(&result).contains("Read image file [image/png]"));
    assert!(text_output(&result).contains("[Image omitted:"));
}

#[test]
fn to_read_output_prefers_the_first_text_and_image_blocks() {
    use kiana_ai::types::{ImageContent, ImageKind, TextContent, TextKind, UserContentBlock};
    let content = vec![
        UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "note".to_string(),
            text_signature: None,
        }),
        UserContentBlock::Image(ImageContent {
            kind_image: ImageKind::Image,
            data: "data".to_string(),
            mime_type: "image/png".to_string(),
        }),
    ];
    let output = to_read_output(&content);
    let value = output.to_json_value();
    assert_eq!(
        value,
        json!({ "type": "image", "data": "data", "mimeType": "image/png", "note": "note" })
    );
    let text_only = vec![UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: "plain".to_string(),
        text_signature: None,
    })];
    assert_eq!(to_read_output(&text_only).to_json_value(), json!("plain"));
    assert_eq!(to_read_output(&[]).to_json_value(), json!(""));
}

#[tokio::test]
async fn agent_tool_wrapper_matches_the_definition() {
    let tool = create_read_tool("/", None);
    assert_eq!(tool.name, "read");
    assert_eq!(tool.label, "read");
}

#[test]
fn local_operations_is_the_default() {
    let _ = LocalReadOperations;
}
