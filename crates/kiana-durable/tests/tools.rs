//! `packages/durable/test/tools.test.ts` 的移植：编码工具（read/write/edit/bash/powershell）
//! 与 `tools/edit-diff.ts`、`tools/image.ts` 的行为。
//!
//! 与 TS 的差异只在测试手段：
//! - TS 用 `vi.spyOn` / 子类覆盖 `NodeExecutionEnv` 的方法；Rust 用 [`TestEnv`] 包装
//!   [`NodeExecutionEnv`] 并注入钩子（读、写、exec、id）。
//! - TS 的 `fakeApi` 是对象字面量；Rust 用 [`FakeApi`] 实现 `ToolExecutionApi`。
//! - 依赖 harness registry/会话（多 agent、tool override）的用例移植到 examples 或标记 `#[ignore]`。
//!
//! 生成自 TS 参考实现的逐字输出（jsdiff 的 patch/diff 字符串、编辑错误文案、工具 schema）
//! 放在 `tests/fixtures/tools_*.json`。

use std::sync::Arc;

use kiana_durable::BoxError;
use kiana_durable::harness::types::ToolRegistration;
use kiana_durable::tools::bash::{create_bash_tool, create_powershell_tool};
use kiana_durable::tools::edit::{EditToolInput, create_edit_tool};
use kiana_durable::tools::edit_diff::{
    Edit, TextReplacement, apply_edits_to_normalized_content,
    apply_replacements_preserving_unchanged_lines, detect_line_ending, diff_lines, fuzzy_find_text,
    generate_diff_string, generate_unified_patch, normalize_for_fuzzy_match, normalize_to_lf,
    restore_line_endings, strip_bom,
};
use kiana_durable::tools::read::{ReadToolOptions, create_read_tool};
use kiana_durable::tools::write::create_write_tool;
use serde_json::{Value as JsonValue, json};

/// 读取一份 golden fixture。
fn fixture(name: &str) -> JsonValue {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

// ---------------------------------------------------------------------------
// tools/edit-diff.ts（jsdiff 逐字移植）
// ---------------------------------------------------------------------------

#[test]
fn jsdiff_matches_the_reference_for_every_pair() {
    let fixture = fixture("tools_diff_cases.json");
    let cases = fixture["diff_cases"].as_array().expect("diff_cases");
    assert!(!cases.is_empty());
    for (index, case) in cases.iter().enumerate() {
        let old = case["old"].as_str().unwrap();
        let new = case["new"].as_str().unwrap();

        let expected: Vec<JsonValue> =
            serde_json::from_str(case["jsdiff"].as_str().unwrap()).expect("jsdiff json");
        let actual = diff_lines(old, new);
        assert_eq!(
            actual.len(),
            expected.len(),
            "case {index}: component count for {old:?} -> {new:?}"
        );
        for (part, expected) in actual.iter().zip(&expected) {
            assert_eq!(
                part.value,
                expected["value"].as_str().unwrap(),
                "case {index}"
            );
            assert_eq!(
                part.added,
                expected["added"].as_bool().unwrap(),
                "case {index}"
            );
            assert_eq!(
                part.removed,
                expected["removed"].as_bool().unwrap(),
                "case {index}"
            );
            assert_eq!(
                part.count as u64,
                expected["count"].as_u64().unwrap(),
                "case {index}"
            );
        }

        let patch = generate_unified_patch("f.txt", old, new, 4);
        assert_eq!(patch, case["patch"].as_str().unwrap(), "case {index}");
        assert_eq!(
            patch,
            case["rawPatch"].as_str().unwrap(),
            "case {index}: createTwoFilesPatch"
        );

        let displayed = generate_diff_string(old, new, 4);
        assert_eq!(
            displayed.diff,
            case["diff"].as_str().unwrap(),
            "case {index}"
        );
        assert_eq!(
            json!(displayed.first_changed_line),
            case["firstChangedLine"],
            "case {index}"
        );
    }
}

#[test]
fn apply_edits_matches_the_reference() {
    let fixture = fixture("tools_diff_cases.json");
    let cases = fixture["edit_results"].as_array().expect("edit_results");
    assert!(!cases.is_empty());
    for (index, case) in cases.iter().enumerate() {
        let content = case["content"].as_str().unwrap();
        let edits: Vec<Edit> = case["edits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|edit| Edit {
                old_text: edit["oldText"].as_str().unwrap().to_owned(),
                new_text: edit["newText"].as_str().unwrap().to_owned(),
            })
            .collect();
        let result = apply_edits_to_normalized_content(content, &edits, "f.txt");
        match case.get("error") {
            Some(expected) => {
                let error = result.expect_err(&format!("case {index} should fail"));
                assert_eq!(
                    error.to_string(),
                    expected.as_str().unwrap(),
                    "case {index}"
                );
            }
            None => {
                let result = result.unwrap_or_else(|error| panic!("case {index}: {error}"));
                let expected = &case["result"];
                assert_eq!(
                    result.base_content,
                    expected["baseContent"].as_str().unwrap(),
                    "case {index}"
                );
                assert_eq!(
                    result.new_content,
                    expected["newContent"].as_str().unwrap(),
                    "case {index}"
                );
            }
        }
    }
}

/// 回归：`countOccurrences` 是 JS 的 `fuzzyContent.split(fuzzyOldText).length - 1`。归一化后为空
/// 的 oldText（纯空白）在非空内容里计数是“UTF-16 码元数 - 1”；旧实现按码元数返回，普遍多 1，
/// 既改错误文案里的数字，也会把“唯一匹配后继续检查重叠”的调用误判成重复。
#[test]
fn whitespace_only_old_text_counts_occurrences_like_string_split() {
    fn edit(old_text: &str, new_text: &str) -> Edit {
        Edit {
            old_text: old_text.to_owned(),
            new_text: new_text.to_owned(),
        }
    }

    // 期望值由 TS 的 `applyEditsToNormalizedContent` 生成。
    let error = apply_edits_to_normalized_content("a b", &[edit(" ", "X")], "f.txt").unwrap_err();
    assert_eq!(
        error.to_string(),
        "Found 2 occurrences of the text in f.txt. The text must be unique. Please provide more context to make it unique."
    );
    let error =
        apply_edits_to_normalized_content("a\tb\tc", &[edit("\t", "X")], "f.txt").unwrap_err();
    assert_eq!(
        error.to_string(),
        "Found 4 occurrences of the text in f.txt. The text must be unique. Please provide more context to make it unique."
    );
    let error = apply_edits_to_normalized_content("a  b", &[edit("  ", "X")], "f.txt").unwrap_err();
    assert_eq!(
        error.to_string(),
        "Found 3 occurrences of the text in f.txt. The text must be unique. Please provide more context to make it unique."
    );
    let error =
        apply_edits_to_normalized_content("x \ny ", &[edit(" ", "X")], "f.txt").unwrap_err();
    assert_eq!(
        error.to_string(),
        "Found 2 occurrences of the text in f.txt. The text must be unique. Please provide more context to make it unique."
    );

    // 空白 oldText 计数为 1 时，继续走重叠检查而不是报重复。
    let error =
        apply_edits_to_normalized_content("\ta", &[edit("\ta", "X"), edit("\t", "Y")], "f.txt")
            .unwrap_err();
    assert_eq!(
        error.to_string(),
        "edits[0] and edits[1] overlap in f.txt. Merge them into one edit or target disjoint regions."
    );

    // 空内容按 JS 拆成 `[]`（-1），行为上等价于 0：替换照常进行。
    let result = apply_edits_to_normalized_content("\t\t", &[edit("\t", "X")], "f.txt").unwrap();
    assert_eq!(result.new_content, "X\t");
}

/// `Edit` 的 serde 形状必须与工具的 JSON schema（`oldText`/`newText`）一致。
#[test]
fn edit_json_uses_the_tool_schema_field_names() {
    let input: EditToolInput = serde_json::from_value(json!({
        "path": "f",
        "edits": [{ "oldText": "a", "newText": "b" }],
    }))
    .expect("camelCase edit input");
    assert_eq!(
        input.edits,
        vec![Edit {
            old_text: "a".to_owned(),
            new_text: "b".to_owned(),
        }]
    );
    assert_eq!(
        serde_json::to_value(&input.edits[0]).expect("serialize edit"),
        json!({ "oldText": "a", "newText": "b" })
    );
}

#[test]
fn fuzzy_matches_and_helpers_match_the_reference() {
    let fixture = fixture("tools_diff_cases.json");
    for (index, case) in fixture["fuzzy_cases"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let content = case["content"].as_str().unwrap();
        let old_text = case["oldText"].as_str().unwrap();
        let result = fuzzy_find_text(content, old_text);
        let expected = &case["result"];
        assert_eq!(
            result.found,
            expected["found"].as_bool().unwrap(),
            "case {index}"
        );
        assert_eq!(
            result.index,
            expected["index"].as_i64().unwrap() as isize,
            "case {index}"
        );
        assert_eq!(
            result.match_length as u64,
            expected["matchLength"].as_u64().unwrap(),
            "case {index}"
        );
        assert_eq!(
            result.used_fuzzy_match,
            expected["usedFuzzyMatch"].as_bool().unwrap(),
            "case {index}"
        );
        assert_eq!(
            result.content_for_replacement,
            expected["contentForReplacement"].as_str().unwrap(),
            "case {index}"
        );
    }

    let helpers = &fixture["helper_cases"];
    for case in helpers["line_endings"].as_array().unwrap() {
        let content = case["c"].as_str().unwrap();
        let expected = case["e"].as_str().unwrap();
        let actual = match detect_line_ending(content) {
            kiana_durable::tools::edit_diff::LineEnding::Lf => "\n",
            kiana_durable::tools::edit_diff::LineEnding::Crlf => "\r\n",
        };
        assert_eq!(actual, expected);
    }
    for case in helpers["normalize"].as_array().unwrap() {
        let content = case["c"].as_str().unwrap();
        let normalized = normalize_to_lf(content);
        assert_eq!(normalized, case["n"].as_str().unwrap());
        assert_eq!(
            restore_line_endings(
                &normalized,
                kiana_durable::tools::edit_diff::LineEnding::Crlf
            ),
            case["r"].as_str().unwrap()
        );
    }
    for case in helpers["stripBom"].as_array().unwrap() {
        let (bom, text) = strip_bom(case["c"].as_str().unwrap());
        assert_eq!(bom, case["r"]["bom"].as_str().unwrap());
        assert_eq!(text, case["r"]["text"].as_str().unwrap());
    }
    for case in helpers["fuzzyNorm"].as_array().unwrap() {
        assert_eq!(
            normalize_for_fuzzy_match(case["v"].as_str().unwrap()),
            case["r"].as_str().unwrap()
        );
    }
    for case in helpers["preserving"].as_array().unwrap() {
        let replacements: Vec<TextReplacement> = case["replacements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|replacement| TextReplacement {
                match_index: replacement["matchIndex"].as_u64().unwrap() as usize,
                match_length: replacement["matchLength"].as_u64().unwrap() as usize,
                new_text: replacement["newText"].as_str().unwrap().to_owned(),
            })
            .collect();
        let result = apply_replacements_preserving_unchanged_lines(
            case["original"].as_str().unwrap(),
            case["base"].as_str().unwrap(),
            &replacements,
        )
        .unwrap();
        assert_eq!(result, case["result"].as_str().unwrap());
    }
}

// ---------------------------------------------------------------------------
// 工具定义（name/description/parameters/structuredOutputSchema/outputLimits）
// ---------------------------------------------------------------------------

#[test]
fn tool_definitions_match_the_reference() {
    let fixture = fixture("tools_schemas.json");
    let tools: [(&str, ToolRegistration); 5] = [
        ("read", create_read_tool(ReadToolOptions::default())),
        ("write", create_write_tool()),
        ("edit", create_edit_tool()),
        ("bash", create_bash_tool(None)),
        ("powershell", create_powershell_tool(None)),
    ];
    for (name, tool) in tools {
        let expected = &fixture[name];
        assert_eq!(tool.tool.name, expected["name"].as_str().unwrap(), "{name}");
        assert_eq!(
            tool.tool.description,
            expected["description"].as_str().unwrap(),
            "{name}"
        );
        assert_eq!(
            serde_json::to_string(&tool.tool.parameters).unwrap(),
            serde_json::to_string(&expected["parameters"]).unwrap(),
            "{name}: parameters (including key order)"
        );
        let schema = tool
            .structured_output_schema
            .clone()
            .unwrap_or(JsonValue::Null);
        assert_eq!(
            serde_json::to_string(&schema).unwrap(),
            serde_json::to_string(&expected["structuredOutputSchema"]).unwrap(),
            "{name}: structuredOutputSchema"
        );
        let limits = tool
            .output_limits
            .as_ref()
            .map(|limits| serde_json::to_value(limits).unwrap())
            .unwrap_or(JsonValue::Null);
        assert_eq!(
            serde_json::to_string(&limits).unwrap(),
            serde_json::to_string(&expected["outputLimits"]).unwrap(),
            "{name}: outputLimits"
        );
    }
    let coding = kiana_durable::tools::CodingTools.clone();
    assert_eq!(
        coding.name(),
        fixture["codingTools"]["name"].as_str().unwrap()
    );
    let names: Vec<String> = coding
        .tools()
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect();
    let expected: Vec<String> = fixture["codingTools"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, expected);
}

// ---------------------------------------------------------------------------
// 行为测试（tools.test.ts）用的替身
// ---------------------------------------------------------------------------

#[path = "support/tools_env.rs"]
mod tools_support;

use kiana_chord::context::BACKGROUND_CONTEXT;
use kiana_durable::env::{ExecutionEnv, FileContent, FileSystem};
use kiana_durable::tools::edit::prepare_edit_arguments;
use tools_support::{
    BlockingEditState, BlockingWriteState, FakeApi, GrowingReader, TempDir, TestEnv,
    abort_controller, delay, diagnostic_text, run as run_tool, text_output, with_signal,
};

/// `createEnv()`：临时目录 + 本地执行环境。
fn create_env() -> (TempDir, Arc<TestEnv>, Arc<dyn ExecutionEnv>) {
    let dir = TempDir::new();
    let env = Arc::new(TestEnv::new(dir.path()));
    let dyn_env: Arc<dyn ExecutionEnv> = env.clone();
    (dir, env, dyn_env)
}

fn background() -> kiana_chord::context::ContextRef {
    BACKGROUND_CONTEXT.clone()
}

/// `run(tool, args, env)`。
async fn run(
    tool: &ToolRegistration,
    args: JsonValue,
    env: Arc<dyn ExecutionEnv>,
) -> (
    kiana_durable::harness::types::ToolExecutionResult,
    Arc<FakeApi>,
) {
    let (result, api) = run_tool(tool, args, Some(env), background()).await;
    (result.expect("the tool returns a result"), api)
}

/// `runFailing(tool, args, env)`：返回错误与流式/诊断通道。
async fn run_failing(
    tool: &ToolRegistration,
    args: JsonValue,
    env: Arc<dyn ExecutionEnv>,
) -> (BoxError, Arc<FakeApi>) {
    let (result, api) = run_tool(tool, args, Some(env), background()).await;
    match result {
        Ok(_) => panic!("Expected the tool to throw"),
        Err(error) => (error, api),
    }
}

async fn write_text(env: &dyn ExecutionEnv, path: &str, text: &str) {
    env.write_file(
        path,
        FileContent::Text(text.to_owned()),
        background().as_ref(),
    )
    .await
    .expect("write file");
}

#[tokio::test]
async fn fails_with_an_ordinary_error_when_no_environment_is_configured() {
    let tool = create_read_tool(ReadToolOptions::default());
    let (result, _api) = run_tool(&tool, json!({ "path": "x" }), None, background()).await;
    let error = result.expect_err("no environment");
    assert!(
        error.to_string().contains("No execution environment"),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

#[test]
fn detects_the_complete_gif_signature() {
    for signature in ["GIF87a", "GIF89a"] {
        assert_eq!(
            kiana_durable::tools::image::detect_supported_image_mime_type(signature.as_bytes()),
            Some("image/gif".to_owned()),
            "{signature}"
        );
    }
}

/// `isBmp` 的 `14 + dibHeaderSize` 在 TS 里是浮点相加，不会溢出。声明 0xFFFFFFFF 的
/// DIB 头（损坏/截断的 BMP）必须当作非图片，而不是在 debug 构建里 panic。
#[test]
fn rejects_a_bmp_whose_dib_header_size_overflows_a_u32() {
    let mut bytes = vec![0u8; 26];
    bytes[0] = b'B';
    bytes[1] = b'M';
    bytes[10..14].copy_from_slice(&0u32.to_le_bytes());
    bytes[14..18].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        kiana_durable::tools::image::detect_supported_image_mime_type(&bytes),
        None
    );
    assert_eq!(
        kiana_durable::tools::image::image_dimensions(&bytes, "image/bmp"),
        None
    );
}

/// 同一个文件经由 `read`：探测不到图片就按文本/二进制返回，而不是让工具调用崩溃。
#[tokio::test]
async fn reads_a_bmp_with_an_overflowing_dib_header_size_instead_of_panicking() {
    let (_dir, _env, env) = create_env();
    let mut bytes = vec![0u8; 26];
    bytes[0] = b'B';
    bytes[1] = b'M';
    bytes[14..18].copy_from_slice(&u32::MAX.to_le_bytes());
    env.write_file(
        "broken.bmp",
        FileContent::Bytes(bytes),
        background().as_ref(),
    )
    .await
    .expect("write file");
    let (result, _api) = run(
        &create_read_tool(ReadToolOptions::default()),
        json!({ "path": "broken.bmp" }),
        env,
    )
    .await;
    assert!(!diagnostic_text(&result).contains("image/"));
}

#[tokio::test]
async fn reads_text_with_offsets_and_limits_and_reports_continuation() {
    let (_dir, _env, env) = create_env();
    let content = (1..=100)
        .map(|index| format!("Line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    write_text(env.as_ref(), "test.txt", &content).await;
    let (result, _api) = run(
        &create_read_tool(ReadToolOptions::default()),
        json!({ "path": "test.txt", "offset": 41, "limit": 20 }),
        env,
    )
    .await;
    let output = text_output(&result);
    assert!(!output.contains("Line 40"));
    assert!(output.contains("Line 41"));
    assert!(output.contains("Line 60"));
    assert!(!output.contains("Line 61"));
    assert!(!output.contains("more lines"));
    assert_eq!(
        diagnostic_text(&result),
        "40 more lines in file. Use offset=61 to continue."
    );
}

#[tokio::test]
async fn truncates_large_text_by_line_count() {
    let (_dir, _env, env) = create_env();
    let content = (1..=2500)
        .map(|index| format!("Line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    write_text(env.as_ref(), "large.txt", &content).await;
    let (result, _api) = run(
        &create_read_tool(ReadToolOptions::default()),
        json!({ "path": "large.txt" }),
        env,
    )
    .await;
    assert_eq!(
        diagnostic_text(&result),
        "Showing lines 1-2000 of 2500. Use offset=2001 to continue."
    );
    assert_eq!(
        result.diagnostics.as_ref().unwrap()[0].code.as_deref(),
        Some("truncated")
    );
    let truncation = &result.details.as_ref().unwrap()["truncation"];
    assert_eq!(truncation["truncated"], json!(true));
    assert_eq!(truncation["truncatedBy"], json!("lines"));
    assert_eq!(truncation["totalLines"], json!(2500));
    assert_eq!(truncation["outputLines"], json!(2000));
}

#[tokio::test]
async fn does_not_count_a_trailing_newline_as_an_extra_line_at_the_limit() {
    let (_dir, _env, env) = create_env();
    let content = format!("{}\n", vec!["x"; 2000].join("\n"));
    write_text(env.as_ref(), "exact.txt", &content).await;
    let (result, _api) = run(
        &create_read_tool(ReadToolOptions::default()),
        json!({ "path": "exact.txt" }),
        env,
    )
    .await;
    assert!(result.details.is_none());
    assert_eq!(
        result.diagnostics.as_deref(),
        Some(&[][..]),
        "no diagnostics"
    );
}

#[tokio::test]
async fn shows_the_start_of_a_line_longer_than_the_byte_limit() {
    let (_dir, _env, env) = create_env();
    let content = format!("{}\nnext\n", "é".repeat(40_000));
    write_text(env.as_ref(), "long.txt", &content).await;
    let (result, _api) = run(
        &create_read_tool(ReadToolOptions::default()),
        json!({ "path": "long.txt" }),
        env,
    )
    .await;
    assert_eq!(text_output(&result), "é".repeat(25_600));
    assert_eq!(
        diagnostic_text(&result),
        "Line 1 is 78.1KB, exceeds the 50.0KB limit; showing its first 50.0KB. Use bash: sed -n '1p' long.txt | tail -c +51201"
    );
    let truncation = &result.details.as_ref().unwrap()["truncation"];
    assert_eq!(truncation["truncated"], json!(true));
    assert_eq!(truncation["firstLineExceedsLimit"], json!(true));
    assert_eq!(truncation["outputBytes"], json!(51_200));
    assert_eq!(truncation["outputLines"], json!(1));
    assert!(truncation.get("content").is_none());
}

#[tokio::test]
async fn rejects_offsets_beyond_the_file() {
    let (_dir, _env, env) = create_env();
    write_text(env.as_ref(), "short.txt", "one\ntwo\nthree").await;
    let (error, _api) = run_failing(
        &create_read_tool(ReadToolOptions::default()),
        json!({ "path": "short.txt", "offset": 100 }),
        env,
    )
    .await;
    assert_eq!(
        error.to_string(),
        "Offset 100 is beyond end of file (3 lines total)"
    );
}

#[tokio::test]
async fn reads_a_log_that_grows_while_it_is_read() {
    let (_dir, env, dyn_env) = create_env();
    write_text(dyn_env.as_ref(), "app.log", "one\ntwo\n").await;
    let log_path = format!("{}/app.log", env.cwd());
    let hook: tools_support::ReaderHook = Arc::new(move |inner, path, options, context| {
        let log_path = log_path.clone();
        Box::pin(async move {
            let reader = inner
                .open_binary_reader(&path, options.as_ref(), context)
                .await?;
            Ok(Arc::new(GrowingReader::new(reader, log_path))
                as Arc<dyn kiana_durable::env::BinaryReader>)
        })
    });
    let growing = Arc::new(TestEnv::new(env.cwd()).with_reader_hook(hook));
    let (result, _api) = run(
        &create_read_tool(ReadToolOptions::default()),
        json!({ "path": "app.log", "limit": 2 }),
        growing,
    )
    .await;
    assert_eq!(text_output(&result), "one\ntwo");
}

#[tokio::test]
async fn returns_images_by_content_as_one_image_block() {
    let (_dir, _env, env) = create_env();
    let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==";
    let bytes = base64_decode(data);
    env.write_file(
        "image.txt",
        FileContent::Bytes(bytes),
        background().as_ref(),
    )
    .await
    .expect("write image");
    let (result, _api) = run(
        &create_read_tool(ReadToolOptions::default()),
        json!({ "path": "image.txt" }),
        env,
    )
    .await;
    match result.output.as_deref() {
        Some([kiana_ai::UserContentBlock::Image(image)]) => {
            assert_eq!(image.data, data);
            assert_eq!(image.mime_type, "image/png");
        }
        other => panic!("unexpected output: {other:?}"),
    }
    assert!(result.is_error.is_none());
    assert_eq!(diagnostic_text(&result), "Read image file [image/png].");
}

/// 测试用的最小 base64 解码（只处理标准字母表与填充）。
fn base64_decode(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' {
            break;
        }
        let Some(value) = ALPHABET.iter().position(|candidate| *candidate == byte) else {
            continue;
        };
        buffer = (buffer << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    output
}

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

use std::sync::atomic::Ordering;

use tools_support::{Deferred, ReadHook, WriteHook};

/// 首次写 `"first\n"` 时阻塞，写 `"second\n"` 时记录，其它内容直接写。
fn blocking_write_env(cwd: impl Into<String>, state: Arc<BlockingWriteState>) -> TestEnv {
    let hook: WriteHook = Arc::new(move |inner, path, content, context| {
        let state = Arc::clone(&state);
        Box::pin(async move {
            match &content {
                FileContent::Text(text) if text == "first\n" => {
                    state.first_write_started.resolve();
                    state.finish_first_write.wait().await;
                }
                FileContent::Text(text) if text == "second\n" => {
                    state.second_write_started.store(true, Ordering::SeqCst);
                }
                _ => {}
            }
            inner.write_file(&path, content, context).await
        })
    });
    TestEnv::new(cwd).with_write_hook(hook)
}

/// 读文本延迟 20ms，用来放大并发窗口。
fn slow_read_env(cwd: impl Into<String>) -> TestEnv {
    let hook: ReadHook = Arc::new(move |inner, path, context| {
        Box::pin(async move {
            delay(20).await;
            inner.read_text_file(&path, context).await
        })
    });
    TestEnv::new(cwd).with_read_hook(hook)
}

async fn read_text(env: &dyn ExecutionEnv, path: &str) -> String {
    env.read_text_file(path, background().as_ref())
        .await
        .expect("read file")
}

#[tokio::test]
async fn writes_files_and_creates_parent_directories() {
    let (_dir, _env, env) = create_env();
    let (result, _api) = run(
        &create_write_tool(),
        json!({ "path": "nested/dir/file.txt", "content": "hello" }),
        env.clone(),
    )
    .await;
    assert_eq!(
        text_output(&result),
        "Successfully wrote to nested/dir/file.txt"
    );
    assert_eq!(
        read_text(env.as_ref(), "nested/dir/file.txt").await,
        "hello"
    );
}

#[tokio::test]
async fn keeps_the_mutation_queue_locked_until_an_aborted_write_settles() {
    let dir = TempDir::new();
    let state = Arc::new(BlockingWriteState::default());
    let env = Arc::new(blocking_write_env(dir.path(), Arc::clone(&state)));
    let tool = create_write_tool();
    let (signal, abort) = abort_controller();
    let first = {
        let tool = tool.clone();
        let env = env.clone();
        tokio::spawn(async move {
            let (result, _api) = run_tool(
                &tool,
                json!({ "path": "file.txt", "content": "first\n" }),
                Some(env as Arc<dyn ExecutionEnv>),
                with_signal(signal),
            )
            .await;
            result
        })
    };
    state.first_write_started.wait().await;
    abort();
    let second = {
        let tool = tool.clone();
        let env = env.clone();
        tokio::spawn(async move {
            let (result, _api) = run_tool(
                &tool,
                json!({ "path": "file.txt", "content": "second\n" }),
                Some(env as Arc<dyn ExecutionEnv>),
                background(),
            )
            .await;
            result
        })
    };
    delay(20).await;
    assert!(!state.second_write_started.load(Ordering::SeqCst));
    state.finish_first_write.resolve();
    assert!(first.await.unwrap().is_err());
    second.await.unwrap().expect("second write");
    assert_eq!(read_text(env.as_ref(), "file.txt").await, "second\n");
}

// ---------------------------------------------------------------------------
// edit
// ---------------------------------------------------------------------------

fn edit_args(path: &str, edits: &[(&str, &str)]) -> JsonValue {
    json!({
        "path": path,
        "edits": edits
            .iter()
            .map(|(old_text, new_text)| json!({ "oldText": old_text, "newText": new_text }))
            .collect::<Vec<_>>(),
    })
}

#[tokio::test]
async fn applies_disjoint_edits_and_returns_both_diff_formats() {
    let (_dir, _env, env) = create_env();
    let original = "alpha\nbeta\ngamma\ndelta\n";
    write_text(env.as_ref(), "edit.txt", original).await;
    let (result, _api) = run(
        &create_edit_tool(),
        edit_args(
            "edit.txt",
            &[("alpha\n", "ALPHA\n"), ("gamma\n", "GAMMA\n")],
        ),
        env.clone(),
    )
    .await;
    let details = result.details.as_ref().expect("details");
    assert_eq!(
        text_output(&result),
        "Successfully replaced 2 block(s) in edit.txt."
    );
    let diff = details["diff"].as_str().unwrap();
    assert!(diff.contains("ALPHA"));
    assert!(diff.contains("GAMMA"));
    let expected_new = "ALPHA\nbeta\nGAMMA\ndelta\n";
    assert_eq!(
        details["patch"].as_str().unwrap(),
        generate_unified_patch("edit.txt", original, expected_new, 4)
    );
    assert_eq!(read_text(env.as_ref(), "edit.txt").await, expected_new);
}

#[test]
fn repairs_edits_sent_as_a_json_string_a_single_object_or_legacy_fields() {
    let edit = json!({ "oldText": "a", "newText": "b" });
    let as_string =
        json!({ "path": "f", "edits": serde_json::to_string(&vec![edit.clone()]).unwrap() });
    assert_eq!(
        prepare_edit_arguments(&as_string).unwrap(),
        json!({ "path": "f", "edits": [edit.clone()] })
    );
    assert_eq!(
        as_string["edits"],
        json!(serde_json::to_string(&vec![edit.clone()]).unwrap())
    );
    assert_eq!(
        prepare_edit_arguments(
            &json!({ "path": "f", "edits": serde_json::to_string(&edit).unwrap() })
        )
        .unwrap(),
        json!({ "path": "f", "edits": [edit.clone()] })
    );
    assert_eq!(
        prepare_edit_arguments(&json!({ "path": "f", "edits": edit.clone() })).unwrap(),
        json!({ "path": "f", "edits": [edit.clone()] })
    );
    assert_eq!(
        prepare_edit_arguments(&json!({
            "path": "f", "edits": [edit.clone()], "oldText": "c", "newText": "d"
        }))
        .unwrap(),
        json!({ "path": "f", "edits": [edit.clone(), { "oldText": "c", "newText": "d" }] })
    );
    assert_eq!(
        prepare_edit_arguments(&json!({ "path": "f", "edits": "not json" })).unwrap(),
        json!({ "path": "f", "edits": "not json" })
    );
}

#[tokio::test]
async fn matches_all_edits_against_the_original_and_rejects_overlaps() {
    let (_dir, _env, env) = create_env();
    write_text(env.as_ref(), "edit.txt", "one\ntwo\nthree\n").await;
    let (error, _api) = run_failing(
        &create_edit_tool(),
        edit_args(
            "edit.txt",
            &[
                ("one\ntwo\n", "ONE\nTWO\n"),
                ("two\nthree\n", "TWO\nTHREE\n"),
            ],
        ),
        env.clone(),
    )
    .await;
    assert!(error.to_string().contains("overlap"), "{error}");
    assert_eq!(
        read_text(env.as_ref(), "edit.txt").await,
        "one\ntwo\nthree\n"
    );
}

#[tokio::test]
async fn rejects_missing_and_duplicate_target_text() {
    let (_dir, _env, env) = create_env();
    write_text(env.as_ref(), "edit.txt", "foo foo foo").await;
    let tool = create_edit_tool();
    let (error, _api) =
        run_failing(&tool, edit_args("edit.txt", &[("bar", "baz")]), env.clone()).await;
    assert!(
        error.to_string().contains("Could not find the exact text"),
        "{error}"
    );
    let (error, _api) =
        run_failing(&tool, edit_args("edit.txt", &[("foo", "bar")]), env.clone()).await;
    assert!(error.to_string().contains("Found 3 occurrences"), "{error}");
}

/// 编辑写入门控：首次 `"ALPHA\nbeta\n"` 阻塞，其余按内容记录。
fn blocking_edit_env(cwd: impl Into<String>, state: Arc<BlockingEditState>) -> TestEnv {
    let hook: WriteHook = Arc::new(move |inner, path, content, context| {
        let state = Arc::clone(&state);
        Box::pin(async move {
            match &content {
                FileContent::Text(text) if text == "ALPHA\nbeta\n" => {
                    state.first_edit_write_started.resolve();
                    state.finish_first_edit_write.wait().await;
                    let result = inner
                        .write_file(&path, content, BACKGROUND_CONTEXT.as_ref())
                        .await;
                    state.first_edit_write_settled.store(true, Ordering::SeqCst);
                    return result;
                }
                FileContent::Text(text) if text == "ALPHA\nBETA\n" || text == "alpha\nBETA\n" => {
                    state
                        .second_edit_write_started
                        .store(true, Ordering::SeqCst);
                }
                _ => {}
            }
            inner.write_file(&path, content, context).await
        })
    });
    TestEnv::new(cwd).with_write_hook(hook)
}

#[tokio::test]
async fn keeps_the_mutation_queue_locked_until_an_aborted_edit_write_settles() {
    let dir = TempDir::new();
    let state = Arc::new(BlockingEditState::default());
    let env = Arc::new(blocking_edit_env(dir.path(), Arc::clone(&state)));
    write_text(env.as_ref(), "file.txt", "alpha\nbeta\n").await;
    let tool = create_edit_tool();
    let (signal, abort) = abort_controller();
    let first = {
        let tool = tool.clone();
        let env = env.clone();
        tokio::spawn(async move {
            let (result, _api) = run_tool(
                &tool,
                edit_args("file.txt", &[("alpha", "ALPHA")]),
                Some(env as Arc<dyn ExecutionEnv>),
                with_signal(signal),
            )
            .await;
            result
        })
    };
    state.first_edit_write_started.wait().await;
    abort();
    let second = {
        let tool = tool.clone();
        let env = env.clone();
        tokio::spawn(async move {
            let (result, _api) = run_tool(
                &tool,
                edit_args("file.txt", &[("beta", "BETA")]),
                Some(env as Arc<dyn ExecutionEnv>),
                background(),
            )
            .await;
            result
        })
    };
    delay(20).await;
    assert!(!state.second_edit_write_started.load(Ordering::SeqCst));
    state.finish_first_edit_write.resolve();
    let first_error = first.await.unwrap().expect_err("first edit aborts");
    assert_eq!(first_error.to_string(), "Operation aborted");
    second.await.unwrap().expect("second edit");
    assert!(state.first_edit_write_settled.load(Ordering::SeqCst));
    assert_eq!(read_text(env.as_ref(), "file.txt").await, "ALPHA\nBETA\n");
}

#[cfg(unix)]
#[tokio::test]
async fn serializes_concurrent_edits_through_canonical_and_symlink_paths() {
    let dir = TempDir::new();
    let env = Arc::new(slow_read_env(dir.path()));
    write_text(env.as_ref(), "target.txt", "alpha\nbeta\ngamma\n").await;
    std::os::unix::fs::symlink("target.txt", format!("{}/link.txt", env.cwd())).unwrap();
    let tool = create_edit_tool();
    let (first, second) = tokio::join!(
        run_tool(
            &tool,
            edit_args("target.txt", &[("alpha", "ALPHA")]),
            Some(env.clone() as Arc<dyn ExecutionEnv>),
            background(),
        ),
        run_tool(
            &tool,
            edit_args("link.txt", &[("beta", "BETA")]),
            Some(env.clone() as Arc<dyn ExecutionEnv>),
            background(),
        ),
    );
    first.0.expect("first edit");
    second.0.expect("second edit");
    assert_eq!(
        read_text(env.as_ref(), "target.txt").await,
        "ALPHA\nBETA\ngamma\n"
    );
}

#[tokio::test]
async fn serializes_edits_of_one_file_across_environment_objects_of_one_file_system() {
    let dir = TempDir::new();
    let first_env = Arc::new(slow_read_env(dir.path()));
    let second_env = Arc::new(slow_read_env(dir.path()));
    write_text(first_env.as_ref(), "file.txt", "alpha\nbeta\n").await;
    let tool = create_edit_tool();
    let (first, second) = tokio::join!(
        run_tool(
            &tool,
            edit_args("file.txt", &[("alpha", "ALPHA")]),
            Some(first_env.clone() as Arc<dyn ExecutionEnv>),
            background(),
        ),
        run_tool(
            &tool,
            edit_args("file.txt", &[("beta", "BETA")]),
            Some(second_env.clone() as Arc<dyn ExecutionEnv>),
            background(),
        ),
    );
    first.0.expect("first edit");
    second.0.expect("second edit");
    assert_eq!(
        read_text(first_env.as_ref(), "file.txt").await,
        "ALPHA\nBETA\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn serializes_a_new_file_created_through_a_symlinked_directory_with_its_canonical_path() {
    let dir = TempDir::new();
    let state = Arc::new(BlockingWriteState::default());
    let env = Arc::new(blocking_write_env(dir.path(), Arc::clone(&state)));
    std::fs::create_dir_all(format!("{}/real", env.cwd())).unwrap();
    std::os::unix::fs::symlink(format!("{}/real", env.cwd()), format!("{}/link", env.cwd()))
        .unwrap();
    let tool = create_write_tool();
    let first = {
        let tool = tool.clone();
        let env = env.clone();
        tokio::spawn(async move {
            let (result, _api) = run_tool(
                &tool,
                json!({ "path": "link/new.txt", "content": "first\n" }),
                Some(env as Arc<dyn ExecutionEnv>),
                background(),
            )
            .await;
            result
        })
    };
    state.first_write_started.wait().await;
    let second = {
        let tool = tool.clone();
        let env = env.clone();
        tokio::spawn(async move {
            let (result, _api) = run_tool(
                &tool,
                json!({ "path": "real/new.txt", "content": "second\n" }),
                Some(env as Arc<dyn ExecutionEnv>),
                background(),
            )
            .await;
            result
        })
    };
    delay(20).await;
    assert!(!state.second_write_started.load(Ordering::SeqCst));
    state.finish_first_write.resolve();
    first.await.unwrap().expect("first write");
    second.await.unwrap().expect("second write");
    assert_eq!(read_text(env.as_ref(), "real/new.txt").await, "second\n");
}

#[cfg(unix)]
#[tokio::test]
async fn keys_a_missing_file_whose_name_contains_a_backslash_like_the_created_file() {
    let dir = TempDir::new();
    let env = Arc::new(TestEnv::new(dir.path()));
    let created = Deferred::new();
    let release = Deferred::new();
    let first = {
        let env = Arc::clone(&env);
        let created = created.clone();
        let release = release.clone();
        tokio::spawn(async move {
            kiana_durable::tools::file_mutation_queue::with_file_mutation_queue(
                env.as_ref(),
                "a\\b.txt",
                || {
                    let env = Arc::clone(&env);
                    let created = created.clone();
                    let release = release.clone();
                    async move {
                        env.write_file(
                            "a\\b.txt",
                            FileContent::Text("first\n".to_owned()),
                            BACKGROUND_CONTEXT.as_ref(),
                        )
                        .await
                        .map_err(BoxError::from)?;
                        created.resolve();
                        release.wait().await;
                        Ok(())
                    }
                },
                BACKGROUND_CONTEXT.as_ref(),
            )
            .await
            .expect("first queue entry");
        })
    };
    created.wait().await;
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let second = {
        let env = Arc::clone(&env);
        let entered = Arc::clone(&entered);
        tokio::spawn(async move {
            kiana_durable::tools::file_mutation_queue::with_file_mutation_queue(
                env.as_ref(),
                "a\\b.txt",
                || {
                    let entered = Arc::clone(&entered);
                    async move {
                        entered.store(true, Ordering::SeqCst);
                        Ok(())
                    }
                },
                BACKGROUND_CONTEXT.as_ref(),
            )
            .await
            .expect("second queue entry");
        })
    };
    delay(20).await;
    assert!(!entered.load(Ordering::SeqCst));
    release.resolve();
    first.await.unwrap();
    second.await.unwrap();
    assert!(entered.load(Ordering::SeqCst));
}

#[tokio::test]
async fn does_not_serialize_the_same_path_on_different_file_systems() {
    let dir = TempDir::new();
    let state = Arc::new(BlockingWriteState::default());
    let local = Arc::new(blocking_write_env(dir.path(), Arc::clone(&state)));
    let other = Arc::new(blocking_write_env(dir.path(), Arc::clone(&state)).with_id("other"));
    let tool = create_write_tool();
    let blocked = {
        let tool = tool.clone();
        let local = local.clone();
        tokio::spawn(async move {
            let (result, _api) = run_tool(
                &tool,
                json!({ "path": "file.txt", "content": "first\n" }),
                Some(local as Arc<dyn ExecutionEnv>),
                background(),
            )
            .await;
            result
        })
    };
    state.first_write_started.wait().await;
    let (result, _api) = run_tool(
        &tool,
        json!({ "path": "file.txt", "content": "second\n" }),
        Some(other as Arc<dyn ExecutionEnv>),
        background(),
    )
    .await;
    result.expect("other file system write");
    assert!(state.second_write_started.load(Ordering::SeqCst));
    state.finish_first_write.resolve();
    blocked.await.unwrap().expect("blocked write");
}

#[cfg(unix)]
#[tokio::test]
async fn edits_regular_files_through_symlinks() {
    let (_dir, _env, env) = create_env();
    write_text(env.as_ref(), "target.txt", "before\n").await;
    std::os::unix::fs::symlink("target.txt", format!("{}/link.txt", env.cwd())).unwrap();
    let (result, _api) = run(
        &create_edit_tool(),
        edit_args("link.txt", &[("before", "after")]),
        env.clone(),
    )
    .await;
    result.details.expect("details");
    assert_eq!(read_text(env.as_ref(), "target.txt").await, "after\n");
}

#[tokio::test]
async fn preserves_bom_and_crlf_line_endings() {
    let (_dir, _env, env) = create_env();
    write_text(env.as_ref(), "edit.txt", "\u{FEFF}one\r\ntwo\r\n").await;
    run(
        &create_edit_tool(),
        edit_args("edit.txt", &[("two", "TWO")]),
        env.clone(),
    )
    .await;
    assert_eq!(
        read_text(env.as_ref(), "edit.txt").await,
        "\u{FEFF}one\r\nTWO\r\n"
    );
}

// ---------------------------------------------------------------------------
// powershell / bash
// ---------------------------------------------------------------------------

use std::sync::Mutex;

use indexmap::IndexMap;
use kiana_durable::env::node::NodeEnvOptions;
use kiana_durable::env::{
    CreateTempFileOptions, ExecutionErrorCode, ShellCommand, ShellExecResult, ShellOutputInfo,
    ShellOutputSkip, ShellOutputStream, ShellOutputWindow,
};
use kiana_durable::tools::bash::{BashPrepare, BashToolOptions, PowerShellToolOptions};
use kiana_durable::truncate::DEFAULT_MAX_LINES;
use tools_support::{ExecHook, execution_error};

/// `programsEnv(installed, output, exitCode)`：只有列出的程序能启动，记录每条命令。
fn programs_env(
    installed: &[&str],
    output: &str,
    exit_code: i32,
) -> (TempDir, Arc<TestEnv>, Arc<Mutex<Vec<ShellCommand>>>) {
    let dir = TempDir::new();
    let commands = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&commands);
    let installed: Vec<String> = installed
        .iter()
        .map(|program| (*program).to_owned())
        .collect();
    let output = output.to_owned();
    let hook: ExecHook = Arc::new(move |_inner, command, options, context| {
        let recorded = Arc::clone(&recorded);
        let installed = installed.clone();
        let output = output.clone();
        Box::pin(async move {
            recorded.lock().unwrap().push(command.clone());
            let program = match &command {
                ShellCommand::Shell(_) => {
                    return Err(execution_error(
                        ExecutionErrorCode::SpawnError,
                        "spawn <shell> ENOENT",
                    ));
                }
                ShellCommand::Argv(argv) => argv.first().cloned().unwrap_or_default(),
            };
            if !installed.contains(&program) {
                return Err(execution_error(
                    ExecutionErrorCode::SpawnError,
                    format!("spawn {program} ENOENT"),
                ));
            }
            if let Some(on_output) = options.and_then(|options| options.on_output) {
                on_output(
                    &output,
                    context,
                    &ShellOutputInfo {
                        stream: ShellOutputStream::Stdout,
                        skipped: None,
                    },
                );
            }
            Ok(ShellExecResult {
                exit_code,
                spill_path: None,
            })
        })
    });
    let env = Arc::new(TestEnv::new(dir.path()).with_exec_hook(hook));
    (dir, env, commands)
}

fn argv_of(command: &ShellCommand) -> Vec<String> {
    match command {
        ShellCommand::Argv(argv) => argv.clone(),
        ShellCommand::Shell(command) => vec![command.clone()],
    }
}

#[tokio::test]
async fn powershell_runs_the_command_as_one_argument_forcing_utf8_output() {
    let (_dir, env, commands) = programs_env(&["pwsh"], "héllo\n", 0);
    let (result, api) = run(
        &create_powershell_tool(None),
        json!({ "command": "Write-Output 'héllo'" }),
        env,
    )
    .await;
    assert_eq!(api.streamed(), "héllo\n");
    let commands = commands.lock().unwrap().clone();
    assert_eq!(
        commands.iter().map(argv_of).collect::<Vec<_>>(),
        vec![vec![
            "pwsh".to_owned(),
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-ExecutionPolicy".to_owned(),
            "Bypass".to_owned(),
            "-Command".to_owned(),
            "try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}\nWrite-Output 'héllo'"
                .to_owned(),
        ]]
    );
    result.structured_output.expect("structured output");
}

#[tokio::test]
async fn powershell_falls_back_to_windows_powershell_and_reports_the_last_start_failure() {
    let (_dir, env, commands) = programs_env(&["powershell"], "ok", 0);
    let (result, api) = run(
        &create_powershell_tool(Some(PowerShellToolOptions {
            command_prefix: Some("$x = 1".to_owned()),
            ..PowerShellToolOptions::default()
        })),
        json!({ "command": "$x" }),
        env,
    )
    .await;
    assert_eq!(api.streamed(), "ok");
    let commands = commands.lock().unwrap().clone();
    let programs = commands
        .iter()
        .map(|command| match command {
            ShellCommand::Argv(argv) => argv[0].clone(),
            ShellCommand::Shell(_) => String::new(),
        })
        .collect::<Vec<_>>();
    assert_eq!(programs, vec!["pwsh".to_owned(), "powershell".to_owned()]);
    let script = argv_of(&commands[1]).pop().unwrap();
    assert!(script.ends_with("\n$x = 1\n$x"), "{script:?}");
    result.structured_output.expect("structured output");

    let (_dir, env, _commands) = programs_env(&[], "", 0);
    let (error, _api) = run_failing(
        &create_powershell_tool(None),
        json!({ "command": "1" }),
        env,
    )
    .await;
    assert_eq!(error.to_string(), "spawn powershell ENOENT");
}

#[tokio::test]
async fn powershell_answers_a_nonzero_exit_with_an_error_result_carrying_the_exit_code() {
    let (_dir, env, _commands) = programs_env(&["pwsh"], "partial", 3);
    let (result, api) = run(
        &create_powershell_tool(None),
        json!({ "command": "exit 3" }),
        env,
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_output,
        Some(json!({ "output": "partial", "truncated": false, "exitCode": 3 }))
    );
    let diagnostics = result.diagnostics.expect("diagnostics");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].severity,
        kiana_durable::harness::types::DiagnosticSeverity::Error
    );
    assert_eq!(diagnostics[0].code.as_deref(), Some("exit_code"));
    assert_eq!(diagnostics[0].message, "Command exited with code 3");
    assert_eq!(api.streamed(), "partial");
}

#[tokio::test]
async fn passes_the_retained_window_to_the_environment_and_forwards_what_it_skipped() {
    let window = ShellOutputWindow {
        max_bytes: 4,
        max_lines: 1,
        min_interval_ms: 100.0,
        bytes_per_second: 1024.0,
    };
    let skipped = ShellOutputSkip {
        bytes: 6,
        newlines: 2,
        ends_with_newline: true,
    };
    let received: Arc<Mutex<Option<ShellOutputWindow>>> = Arc::new(Mutex::new(None));
    let recorded = Arc::clone(&received);
    let hook: ExecHook = Arc::new(move |_inner, _command, options, context| {
        let recorded = Arc::clone(&recorded);
        Box::pin(async move {
            let options = options.expect("options");
            *recorded.lock().unwrap() = options.window;
            if let Some(on_output) = options.on_output {
                on_output(
                    "tail\n",
                    context,
                    &ShellOutputInfo {
                        stream: ShellOutputStream::Stdout,
                        skipped: Some(skipped),
                    },
                );
            }
            Ok(ShellExecResult {
                exit_code: 0,
                spill_path: None,
            })
        })
    });
    let _dir = TempDir::new();
    let env: Arc<dyn ExecutionEnv> = Arc::new(TestEnv::new(_dir.path()).with_exec_hook(hook));
    let api = FakeApi::with_window(Some(env), window);
    let tool = create_bash_tool(None);
    let execute = tool.execute.as_ref().expect("execute");
    let result = execute(json!({ "command": "anything" }), api.clone(), background())
        .await
        .expect("result");
    assert_eq!(*received.lock().unwrap(), Some(window));
    assert_eq!(api.chunks(), vec![("tail\n".to_owned(), Some(skipped))]);
    result.structured_output.expect("structured output");
}

#[tokio::test]
async fn streams_combined_stdout_and_stderr_and_repeats_it_with_the_exit_code() {
    let (_dir, _env, env) = create_env();
    let (result, api) = run(
        &create_bash_tool(None),
        json!({ "command": "printf out; printf err >&2" }),
        env,
    )
    .await;
    let streamed = api.streamed();
    assert!(streamed.contains("out"), "{streamed:?}");
    assert!(streamed.contains("err"), "{streamed:?}");
    assert!(result.output.is_none());
    assert_eq!(
        result.structured_output,
        Some(json!({ "output": streamed, "truncated": false, "exitCode": 0 }))
    );
}

#[tokio::test]
async fn answers_a_nonzero_exit_with_an_error_and_throws_on_a_timeout_after_streaming() {
    let (_dir, _env, env) = create_env();
    let tool = create_bash_tool(None);
    let (result, api) = run(
        &tool,
        json!({ "command": "printf failed; exit 7" }),
        env.clone(),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_output,
        Some(json!({ "output": "failed", "truncated": false, "exitCode": 7 }))
    );
    let diagnostics = result.diagnostics.expect("diagnostics");
    assert_eq!(diagnostics[0].code.as_deref(), Some("exit_code"));
    assert_eq!(diagnostics[0].message, "Command exited with code 7");
    assert_eq!(api.streamed(), "failed");

    let (error, _api) =
        run_failing(&tool, json!({ "command": "sleep 2", "timeout": 0.01 }), env).await;
    assert_eq!(error.to_string(), "Command timed out after 0.01 seconds");
}

#[tokio::test]
async fn reports_the_spill_of_a_command_that_times_out() {
    let dir = TempDir::new();
    let hook: ExecHook = Arc::new(move |inner, _command, options, context| {
        let inner = Arc::clone(&inner);
        Box::pin(async move {
            let mut output = String::new();
            for index in 1..=DEFAULT_MAX_LINES + 1 {
                output.push_str(&format!("line-{index}\n"));
            }
            let spill_path = inner
                .create_temp_file(
                    Some(&CreateTempFileOptions {
                        prefix: Some("timeout-".to_owned()),
                        suffix: Some(".log".to_owned()),
                    }),
                    context,
                )
                .await
                .map_err(|error| execution_error(ExecutionErrorCode::Unknown, error.to_string()))?;
            inner
                .write_file(&spill_path, FileContent::Text(output.clone()), context)
                .await
                .map_err(|error| execution_error(ExecutionErrorCode::Unknown, error.to_string()))?;
            if let Some(on_output) = options.and_then(|options| options.on_output) {
                on_output(
                    &output,
                    context,
                    &ShellOutputInfo {
                        stream: ShellOutputStream::Stdout,
                        skipped: None,
                    },
                );
            }
            Err(execution_error(ExecutionErrorCode::Timeout, "timeout").with_spill_path(spill_path))
        })
    });
    let env = Arc::new(TestEnv::new(dir.path()).with_exec_hook(hook));
    let (error, api) = run_failing(
        &create_bash_tool(None),
        json!({ "command": "emit-output-then-time-out", "timeout": 0.05 }),
        env.clone(),
    )
    .await;
    assert_eq!(error.to_string(), "Command timed out after 0.05 seconds");
    let reported = api.reported();
    let full_output_path = reported[0]
        .message
        .strip_prefix("Full output: ")
        .unwrap_or_else(|| panic!("unexpected diagnostic: {}", reported[0].message));
    let full_output = env
        .read_text_file(full_output_path, background().as_ref())
        .await
        .expect("read spill");
    assert!(full_output.contains("line-1\nline-2"));
    assert!(full_output.contains(&format!(
        "line-{DEFAULT_MAX_LINES}\nline-{}",
        DEFAULT_MAX_LINES + 1
    )));
}

#[tokio::test]
async fn prepares_command_cwd_and_an_explicit_environment_with_the_calls_api() {
    let dir = TempDir::new();
    let shell_env = IndexMap::from([(
        "KIANA_BASH_PREPARE_INHERITED".to_owned(),
        "inherited".to_owned(),
    )]);
    let env = Arc::new(TestEnv::with_options(NodeEnvOptions {
        cwd: dir.path(),
        shell_env: Some(shell_env),
        ..NodeEnvOptions::default()
    }));
    let dyn_env: Arc<dyn ExecutionEnv> = env.clone();
    env.create_dir("workspace", None, background().as_ref())
        .await
        .expect("create workspace");
    let workspace = format!("{}/workspace", env.cwd());
    let received_env: Arc<Mutex<Option<Arc<dyn ExecutionEnv>>>> = Arc::new(Mutex::new(None));
    let received_signal: Arc<Mutex<Option<kiana_chord::context::AbortSignal>>> =
        Arc::new(Mutex::new(None));
    let (signal, _abort) = abort_controller();
    let expected_signal = signal.clone();
    let prepare: BashPrepare = {
        let workspace = workspace.clone();
        let received_env = Arc::clone(&received_env);
        let received_signal = Arc::clone(&received_signal);
        Arc::new(move |execution, api, context| {
            let workspace = workspace.clone();
            let received_env = Arc::clone(&received_env);
            let received_signal = Arc::clone(&received_signal);
            Box::pin(async move {
                *received_env.lock().unwrap() = api.env();
                *received_signal.lock().unwrap() = context.abort_signal();
                execution.cwd = workspace;
                execution.env = IndexMap::from([(
                    "KIANA_BASH_PREPARE_EXPLICIT".to_owned(),
                    "explicit".to_owned(),
                )]);
                execution.inherit_env = false;
                execution.command += concat!(
                    "\n: > prepared-cwd\n",
                    "printf '%s:%s:%s' \"$prefix\" \"${KIANA_BASH_PREPARE_INHERITED-}\" \"$KIANA_BASH_PREPARE_EXPLICIT\"",
                    "\nprintf ':%s' \"$PWD\""
                );
                Ok(())
            })
        })
    };
    let tool = create_bash_tool(Some(BashToolOptions {
        command_prefix: Some("prefix=ready".to_owned()),
        prepare: Some(prepare),
    }));
    let (result, api) = run_tool(
        &tool,
        json!({ "command": ":" }),
        Some(dyn_env.clone()),
        with_signal(signal),
    )
    .await;
    let result = result.expect("bash result");
    let received = received_env.lock().unwrap().clone().expect("received env");
    assert!(Arc::ptr_eq(&received, &dyn_env));
    let received_signal = received_signal
        .lock()
        .unwrap()
        .clone()
        .expect("received signal");
    assert!(received_signal.same_signal(&expected_signal));
    let pwd = env
        .canonical_path(&workspace, background().as_ref())
        .await
        .expect("canonical workspace");
    assert_eq!(api.streamed(), format!("ready::explicit:{pwd}"));
    assert!(
        env.exists(&format!("{workspace}/prepared-cwd"), background().as_ref())
            .await
            .expect("exists")
    );
    result.structured_output.expect("structured output");
}

#[tokio::test]
async fn supports_command_prefixes() {
    let (_dir, _env, env) = create_env();
    let (result, api) = run(
        &create_bash_tool(Some(BashToolOptions {
            command_prefix: Some("value=hello".to_owned()),
            prepare: None,
        })),
        json!({ "command": "printf $value" }),
        env,
    )
    .await;
    assert_eq!(api.streamed(), "hello");
    result.structured_output.expect("structured output");
}

#[tokio::test]
async fn streams_every_byte_and_spills_complete_output_beyond_the_default_limits() {
    let (_dir, env, dyn_env) = create_env();
    let (result, api) = run(
        &create_bash_tool(None),
        json!({ "command": "i=1; while [ $i -le 3000 ]; do echo line-$i; i=$((i + 1)); done" }),
        dyn_env,
    )
    .await;
    let expected = (1..=3000)
        .map(|index| format!("line-{index}\n"))
        .collect::<String>();
    assert_eq!(api.streamed(), expected);
    let reported = api.reported();
    let full_output_path = reported[0]
        .message
        .strip_prefix("Full output: ")
        .unwrap_or_else(|| panic!("unexpected diagnostic: {}", reported[0].message));
    assert_eq!(read_text(env.as_ref(), full_output_path).await, expected);
    result.structured_output.expect("structured output");
}

#[tokio::test]
async fn does_not_spill_output_within_the_limits() {
    let (_dir, _env, env) = create_env();
    let (result, api) = run(
        &create_bash_tool(None),
        json!({ "command": "printf small" }),
        env,
    )
    .await;
    assert!(api.reported().is_empty());
    result.structured_output.expect("structured output");
}
