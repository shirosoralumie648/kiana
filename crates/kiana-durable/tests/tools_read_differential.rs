//! `packages/durable/test/tools-read-differential.test.ts` 的移植。
//!
//! 参考实现（旧版 `read`：整份解码、按行切分、再截断）在这里用 Rust 重写，与有界读取的
//! 工具逐项比较输出、details 与诊断。TS 测试里 `offset`/`limit` 会直接传 `NaN`；JSON 无法
//! 表示 `NaN`（`serde_json` 的 `Number` 只装有限值），所以这里把 `NaN` 当成“未提供”——
//! 生产路径也只能收到 JSON，`NaN` 无法从调用方到达工具。种子与随机序列与 TS 相同。

use std::sync::Arc;

use kiana_chord::output::character_end;
use kiana_durable::env::ExecutionEnv;
use kiana_durable::harness::types::ToolRegistration;
use kiana_durable::tools::image::{detect_supported_image_mime_type, to_base64};
use kiana_durable::tools::read::{ReadToolOptions, create_read_tool};
use kiana_durable::truncate::{
    DEFAULT_MAX_BYTES, TruncatedBy, TruncationOptions, TruncationResult, format_size, truncate_head,
};
use kiana_json::to_radix_string;
use serde_json::{Map as JsonMap, Value as JsonValue, json};

#[path = "support/tools_env.rs"]
mod tools_support;

use tools_support::{TempDir, TestEnv};

fn background() -> kiana_chord::context::ContextRef {
    kiana_chord::context::BACKGROUND_CONTEXT.clone()
}

/// `String(value)`。
fn number_text(value: f64) -> String {
    to_radix_string(value, 10)
}

/// JS 的 `ToIntegerOrInfinity`（`Array.prototype.slice` 对索引做的转换）。
fn to_integer(value: f64) -> f64 {
    if value.is_nan() {
        0.0
    } else if value.is_infinite() {
        value
    } else {
        value.trunc()
    }
}

/// `allLines.slice(start, end)` 的下标范围（`end` 缺省到尾部）。
fn slice_range(len: usize, start: f64, end: Option<f64>) -> (usize, usize) {
    let length = len as f64;
    let mut a = to_integer(start);
    if a < 0.0 {
        a = (length + a).max(0.0);
    } else {
        a = a.min(length);
    }
    let mut b = match end {
        None => length,
        Some(end) => {
            let mut b = to_integer(end);
            if b < 0.0 {
                b = (length + b).max(0.0);
            } else {
                b = b.min(length);
            }
            b
        }
    };
    if b < a {
        b = a;
    }
    (a as usize, b as usize)
}

/// `truncation` 对象（去掉 `content`）在 TS 里的 JSON 形状。
fn truncation_json(truncation: &TruncationResult) -> JsonValue {
    json!({
        "truncated": truncation.truncated,
        "truncatedBy": truncation.truncated_by.map(|by| match by {
            TruncatedBy::Lines => "lines",
            TruncatedBy::Bytes => "bytes",
        }),
        "totalLines": truncation.total_lines,
        "totalBytes": truncation.total_bytes,
        "outputLines": truncation.output_lines,
        "outputBytes": truncation.output_bytes,
        "lastLinePartial": truncation.last_line_partial,
        "firstLineExceedsLimit": truncation.first_line_exceeds_limit,
        "maxLines": truncation.max_lines,
        "maxBytes": truncation.max_bytes,
    })
}

/// 旧版 `read`：整份解码 → 按 `\n` 切分 → 选中行 → 截断。
fn reference_read(
    bytes: &[u8],
    path: &str,
    offset: Option<f64>,
    limit: Option<f64>,
) -> Result<JsonValue, String> {
    if let Some(mime_type) = detect_supported_image_mime_type(bytes) {
        return Ok(json!({
            "output": [{
                "type": "image",
                "data": to_base64(bytes),
                "mimeType": mime_type,
            }],
            "diagnostics": [{
                "severity": "info",
                "code": "image",
                "message": format!("Read image file [{mime_type}]."),
            }],
        }));
    }
    // `new TextDecoder().decode` 默认去掉流首的 BOM。
    let text = if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        String::from_utf8_lossy(&bytes[3..])
    } else {
        String::from_utf8_lossy(bytes)
    };
    let all_lines: Vec<&str> = text.split('\n').collect();
    let total_file_lines = all_lines.len() as f64;
    let start_line = match offset {
        Some(offset) if offset != 0.0 && !offset.is_nan() => (offset - 1.0).max(0.0),
        _ => 0.0,
    };
    let start_line_display = start_line + 1.0;
    if start_line >= total_file_lines {
        return Err(format!(
            "Offset {} is beyond end of file ({} lines total)",
            number_text(offset.unwrap_or(f64::NAN)),
            number_text(total_file_lines)
        ));
    }
    let (selected_content, user_limited_lines) = match limit {
        Some(limit) => {
            let end_line = (start_line + limit).min(total_file_lines);
            let (start, end) = slice_range(all_lines.len(), start_line, Some(end_line));
            (
                all_lines[start..end].join("\n"),
                Some(end_line - start_line),
            )
        }
        None => {
            let (start, end) = slice_range(all_lines.len(), start_line, None);
            (all_lines[start..end].join("\n"), None)
        }
    };

    let truncation = truncate_head(&selected_content, &TruncationOptions::new());
    let mut diagnostics: Vec<JsonValue> = Vec::new();
    let mut output_text = truncation.content.clone();
    let mut details: Option<JsonValue> = None;
    if truncation.first_line_exceeds_limit {
        // 与 `allLines[startLine]` 一致：小数起始行不指向任何行，`new TextEncoder().encode(undefined)`
        // 得到的是空字节（`encode` 的可选参数缺省为 `""`）。
        let integral = start_line.is_finite() && start_line.fract() == 0.0;
        let line_string = if integral {
            all_lines.get(start_line as usize).copied().unwrap_or("")
        } else {
            ""
        };
        let line_bytes = line_string.as_bytes();
        let end = character_end(line_bytes, DEFAULT_MAX_BYTES);
        // `lineBytes.subarray(0, end)` 会截到长度。
        output_text =
            String::from_utf8_lossy(&line_bytes[..end.min(line_bytes.len())]).into_owned();
        diagnostics.push(json!({
            "severity": "warn",
            "code": "truncated",
            "message": format!(
                "Line {} is {}, exceeds the {} limit; showing its first {}. Use bash: sed -n '{}p' {} | tail -c +{}",
                number_text(start_line_display),
                format_size(line_bytes.len() as u64),
                format_size(DEFAULT_MAX_BYTES as u64),
                format_size(end as u64),
                number_text(start_line_display),
                path,
                end + 1
            ),
        }));
        let mut truncated = truncation_json(&truncation);
        truncated["outputBytes"] = json!(end);
        truncated["outputLines"] = json!(1);
        details = Some(json!({ "truncation": truncated }));
    } else if truncation.truncated {
        let end_line_display = start_line_display + truncation.output_lines as f64 - 1.0;
        let next_offset = end_line_display + 1.0;
        let limit_text = match truncation.truncated_by {
            Some(TruncatedBy::Lines) => String::new(),
            _ => format!(" ({} limit)", format_size(DEFAULT_MAX_BYTES as u64)),
        };
        diagnostics.push(json!({
            "severity": "info",
            "code": "truncated",
            "message": format!(
                "Showing lines {}-{} of {}{}. Use offset={} to continue.",
                number_text(start_line_display),
                number_text(end_line_display),
                number_text(total_file_lines),
                limit_text,
                number_text(next_offset)
            ),
        }));
        details = Some(json!({ "truncation": truncation_json(&truncation) }));
    } else if let Some(user_limited_lines) = user_limited_lines
        && start_line + user_limited_lines < total_file_lines
    {
        let remaining = total_file_lines - (start_line + user_limited_lines);
        let next_offset = start_line + user_limited_lines + 1.0;
        diagnostics.push(json!({
            "severity": "info",
            "message": format!(
                "{} more lines in file. Use offset={} to continue.",
                number_text(remaining),
                number_text(next_offset)
            ),
        }));
    }

    let mut result = JsonMap::new();
    result.insert(
        "output".to_owned(),
        if output_text.is_empty() {
            json!([])
        } else {
            json!([{ "type": "text", "text": output_text }])
        },
    );
    if let Some(details) = details {
        result.insert("details".to_owned(), details);
    }
    result.insert("diagnostics".to_owned(), JsonValue::Array(diagnostics));
    Ok(JsonValue::Object(result))
}

fn random(seed: u32) -> impl FnMut() -> f64 {
    let mut state = seed;
    move || {
        state = state.wrapping_add(0x6d2b_79f5);
        let mut t = state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }
}

const PIECES: [&[u8]; 11] = [
    &[0x0a],
    &[0x0a],
    &[0x61],
    &[0x62, 0x63, 0x64, 0x65],
    &[0xef, 0xbb, 0xbf],
    &[0xc3, 0xa9],
    &[0xe2, 0x82, 0xac],
    &[0xf0, 0x9f, 0x98, 0x80],
    &[0xe2, 0x82],
    &[0xff],
    &[0x0d, 0x0a],
];

/// 大多是小文件，一些超过行数上限，一些超过字节上限，一些有一行超长。
fn random_file(next: &mut impl FnMut() -> f64) -> Vec<u8> {
    let kind = next();
    if kind < 0.1 {
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice("x\n".repeat(2100).as_bytes());
        return bytes;
    }
    if kind < 0.2 {
        let length = DEFAULT_MAX_BYTES + 200 + (next() * 400.0).floor() as usize;
        let mut line = vec![0x61u8; length];
        line[DEFAULT_MAX_BYTES - 1 + (next() * 3.0).floor() as usize] = 0xc3;
        let mut bytes = line;
        bytes.push(0x0a);
        bytes.push(0x62);
        return bytes;
    }
    if kind < 0.3 {
        let line = format!("{}\n", "0123456789".repeat(30));
        return line.repeat(200).into_bytes();
    }
    let mut bytes: Vec<u8> = Vec::new();
    let pieces = (next() * 80.0).floor() as usize;
    for _ in 0..pieces {
        let index = (next() * PIECES.len() as f64).floor() as usize;
        bytes.extend_from_slice(PIECES[index]);
    }
    bytes
}

/// `undefined`、数字，以及 `NaN`（用 `f64::NAN` 表示）。
const OFFSETS: [Option<f64>; 12] = [
    None,
    Some(0.0),
    Some(1.0),
    Some(2.0),
    Some(3.0),
    Some(2.5),
    Some(-4.0),
    Some(50.0),
    Some(2001.0),
    Some(3000.0),
    Some(1e20),
    Some(f64::NAN),
];
const LIMITS: [Option<f64>; 12] = [
    None,
    Some(0.0),
    Some(1.0),
    Some(2.0),
    Some(-3.0),
    Some(-1e20),
    Some(1.5),
    Some(7.0),
    Some(2000.0),
    Some(2500.0),
    Some(1e20),
    Some(f64::NAN),
];

/// `NaN` 无法出现在 JSON 里，按“未提供”处理。
fn effective(value: Option<f64>) -> Option<f64> {
    value.filter(|value| !value.is_nan())
}

/// 写进调用参数的值；`NaN` 用 `null`（与缺省等价）。
fn trial_json(value: Option<f64>) -> Option<JsonValue> {
    match value {
        None => None,
        Some(value) if value.is_nan() => Some(JsonValue::Null),
        Some(value) => Some(json!(value)),
    }
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Ok(JsonValue),
    Error(String),
}

async fn actual_outcome(
    tool: &ToolRegistration,
    env: &Arc<dyn ExecutionEnv>,
    args: JsonValue,
) -> Outcome {
    let (result, _api) = tools_support::run(tool, args, Some(Arc::clone(env)), background()).await;
    match result {
        Ok(result) => Outcome::Ok(serde_json::to_value(&result).expect("result is JSON")),
        Err(error) => Outcome::Error(error.to_string()),
    }
}

fn failure_context(seed: u32, trial: usize, offset: Option<f64>, limit: Option<f64>) -> String {
    format!("seed {seed} trial {trial} offset {offset:?} limit {limit:?}")
}

#[tokio::test]
async fn returns_exactly_what_reading_the_whole_file_returned() {
    let dir = TempDir::new();
    let env: Arc<dyn ExecutionEnv> = Arc::new(TestEnv::new(dir.path()));
    let tool = create_read_tool(ReadToolOptions::default());
    let file_path = format!("{}/f.txt", dir.path());
    for seed in 1..=400u32 {
        let mut next = random(seed);
        let file = random_file(&mut next);
        std::fs::write(&file_path, &file).expect("write f.txt");
        for trial in 0..4 {
            let offset = OFFSETS[(next() * OFFSETS.len() as f64).floor() as usize];
            let limit = LIMITS[(next() * LIMITS.len() as f64).floor() as usize];
            let mut args = JsonMap::new();
            args.insert("path".to_owned(), json!("f.txt"));
            if let Some(value) = trial_json(offset) {
                args.insert("offset".to_owned(), value);
            }
            if let Some(value) = trial_json(limit) {
                args.insert("limit".to_owned(), value);
            }
            let actual = actual_outcome(&tool, &env, JsonValue::Object(args)).await;
            let expected = match reference_read(&file, "f.txt", effective(offset), effective(limit))
            {
                Ok(value) => Outcome::Ok(value),
                Err(message) => Outcome::Error(message),
            };
            assert_eq!(
                actual,
                expected,
                "{}",
                failure_context(seed, trial, offset, limit)
            );
        }
    }
}

#[tokio::test]
async fn detects_animated_pngs_whose_actl_chunk_lies_far_beyond_the_header() {
    let dir = TempDir::new();
    fn chunk(kind: &str, length: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(length as u32).to_be_bytes());
        bytes.extend_from_slice(kind.as_bytes());
        bytes.extend(std::iter::repeat_n(0u8, length + 4));
        bytes
    }
    let mut png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    png.extend(chunk("IHDR", 13));
    png.extend(chunk("iCCP", 200_000));
    png.extend(chunk("acTL", 8));
    png.extend(chunk("IDAT", 10));
    std::fs::write(format!("{}/a.png", dir.path()), &png).expect("write a.png");
    let env: Arc<dyn ExecutionEnv> = Arc::new(TestEnv::new(dir.path()));
    let tool = create_read_tool(ReadToolOptions::default());
    let actual = actual_outcome(&tool, &env, json!({ "path": "a.png" })).await;
    let expected = match reference_read(&png, "a.png", None, None) {
        Ok(value) => Outcome::Ok(value),
        Err(message) => Outcome::Error(message),
    };
    assert_eq!(actual, expected);
}

/// 回归：小数 offset 选中超过 50 KiB 的首行时，TS 的 `lineBytes` 是空数组、`subarray` 截到长度，
/// 返回空内容与 `outputBytes: 51200`（旧实现会因 `&lineBytes[..51200]` 越界 panic）。
#[tokio::test]
async fn a_fractional_offset_on_a_line_over_the_byte_limit_returns_empty_content() {
    let dir = TempDir::new();
    let mut file = vec![b'a'; 51_554];
    file.extend_from_slice(b"\nb");
    assert_eq!(file.len(), 51_556);
    std::fs::write(format!("{}/f65_2.txt", dir.path()), &file).expect("write f65_2.txt");
    let env: Arc<dyn ExecutionEnv> = Arc::new(TestEnv::new(dir.path()));
    let tool = create_read_tool(ReadToolOptions::default());
    let actual = actual_outcome(&tool, &env, json!({ "path": "f65_2.txt", "offset": 1.5 })).await;
    let expected = Outcome::Ok(json!({
        "output": [],
        "details": { "truncation": {
            "truncated": true,
            "truncatedBy": "bytes",
            "totalLines": 2,
            "totalBytes": 51_556,
            "outputLines": 1,
            "outputBytes": 51_200,
            "lastLinePartial": false,
            "firstLineExceedsLimit": true,
            "maxLines": 2_000,
            "maxBytes": 51_200,
        }},
        "diagnostics": [{
            "severity": "warn",
            "code": "truncated",
            "message": "Line 1.5 is 0B, exceeds the 50.0KB limit; showing its first 50.0KB. Use bash: sed -n '1.5p' f65_2.txt | tail -c +51201",
        }],
    }));
    assert_eq!(actual, expected);
}
