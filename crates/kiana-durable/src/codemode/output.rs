//! codemode/output.ts：一次脚本结果在模型眼里的样子。
//!
//! "Script completed/failed" 头、墙钟时间、脚本的文本项（多个时带 `==> text N/M <==`）、
//! `<console_output>` 块里的 console 行、返回值，以及失败时的错误与已做的调用。文本超出
//! token 预算时保留首尾，并把完整文本写进临时文件；图片也写进临时文件，路径放在图片项前面。
//!
//! 与 TS 的逐字对齐点：`…`（U+2026）、`Math.ceil`、UTF-16 码元的 `slice`/`length`、
//! `atob` 的宽容解码、`toFixed(1)`。UTF-16 切片用 [`kiana_chord::delta::index`] 的等价物。
//!
//! # 为什么需要这个模块
//! 脚本结果（[`CodemodeResult`]）只是数据：文本项、图片项、返回值与错误。模型看到的是一条
//! 工具结果消息，需要固定头、把 console 行收拢、给多段文本编号、按 token 预算裁剪，并把
//! 图片落到临时文件。本文件负责这层“展示投影”：它不执行工具、不改写持久化结果，唯一的
//! 副作用是调用调用方给的 `write` 回调写文件。
//!
//! # 调用方
//! 生产代码里只有 `codemode.rs::complete()` 调用 [`format_result`]（测试也直接驱动它）：
//! `wall_ms` 是 `now - start.startedAt`，`max_output_tokens` 来自脚本首行的
//! `// @options:`（缺省 [`DEFAULT_MAX_OUTPUT_TOKENS`]），`limits` 与工具注册项共用同一份
//! `OUTPUT_LIMITS`，`write` 是 `output_files()`。返回的 `full_output_path` 被写进工具
//! 结果的 `details.fullOutputPath`。
//!
//! # 执行顺序
//! 1. 成功结果把返回值追加成一个文本项（字符串原样，其它值紧凑 JSON）；失败结果不追加
//!    返回值。
//! 2. `format_items()`：给非 console 文本项编号（多于一个时），console 行收集到末尾的
//!    `<console_output>` 块，图片按原位置插入。
//! 3. 失败结果这时才追加 `Script error:` 段与失败前的调用摘要；若 `generated_images > 0`
//!    而结果里没有图片块，再追加“生成的图片未展示”提醒。这两类文本项都在 `format_items()`
//!    之后加入，因此不参与 `==> text N/M` 编号，也不会被 console 收集。
//! 4. `join_adjacent_text()`：合并相邻文本项；图片会打断合并。
//! 5. `truncate()`：超预算时把文本合成一项并保留首尾，完整文本写入 `output.txt`；图片被
//!    移到文本之后。
//! 6. `save_images()`：每张图写一个文件，在图片块前插入一行路径说明；同一份 base64 只写
//!    一次。
//! 7. 再一次 `join_adjacent_text()`，然后在最前面加 `Script completed`/`Script failed`
//!    头（含 `toFixed(1)` 的墙钟秒数）。
//!
//! # 约束与失败模式
//! - 长度与切片都按 UTF-16 码元（JS 的 `string.length`/`slice`），不是字节或 Unicode
//!   标量；因此预算与 TS 的字符数口径一致。
//! - 文本逐字对齐 TS：省略号是 U+2026，token 数按 `Math.ceil` 的口径，时间用
//!   `toFixed(1)`。
//! - `write` 的失败不是 [`format_result`] 的错误：`output.txt` 写失败变成提示文本，图片
//!   写失败变成图片标签的一部分。目前唯一会让格式化返回 `Err` 的是图片 base64 解码失败
//!   （`atob`），此时 `complete()` 以该错误结束，不写结果。
//! - 文本预算取 `min(max_output_tokens * 4, max_bytes / 4)`；`Limits` 由调用方同时写进
//!   工具注册项的 `output_limits`，Harness 的裁剪只是最后兜底。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use kiana_ai::types::{ImageContent, ImageKind, TextContent, TextKind, UserContentBlock};
use kiana_chord::delta::index::{utf16_len, utf16_slice};
use kiana_codemode::types::{
    CodemodeErrorKind, CodemodeOutputItem, CodemodeResult, CodemodeResultErr, CodemodeResultOk,
};
use serde_json::Value as JsonValue;

use crate::types::BoxError;

/// 一块输出：文本或图片（TS 的 `TextContent | ImageContent`）。
///
/// `UserContentBlock` 只有 `Text`/`Image` 两个变体，因此本文件不需要处理其它分支。
pub type Item = UserContentBlock;

/// 写一个结果文件的异步回调。
///
/// 回调返回一个 `Send` future；本文件在异步函数里 await 它，再把 `OutputFileResult`
/// 转成路径字符串或提示文本。
pub type OutputFuture = Pin<Box<dyn Future<Output = OutputFileResult> + Send>>;

/// 交给 [`OutputFile`] 的内容：`string | Uint8Array`。
///
/// `Text` 的字节视图是 UTF-8 编码（[`OutputData::into_bytes`]）；本文件只构造它，不直接
/// 写盘。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputData {
    /// 文本。
    Text(String),
    /// 原始字节。
    Bytes(Vec<u8>),
}

impl OutputData {
    /// 字节视图（文本按 UTF-8 编码）。
    pub fn into_bytes(self) -> Vec<u8> {
        match self {
            OutputData::Text(text) => text.into_bytes(),
            OutputData::Bytes(bytes) => bytes,
        }
    }
}

/// 写文件的结果：路径，或写不出的原因。
///
/// `Error` 是给人看的消息，本文件会把它嵌进结果文本（`[Could not save ...]`）或图片标签；
/// 不重试，也不向上传播。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputFileResult {
    /// 已写出的路径。
    Path(String),
    /// 失败消息。
    Error(String),
}

/// 写一个结果文件（TS 的 `OutputFile`）。
///
/// 参数：固定文件名与内容；返回路径或原因。TS 里是 `(name, data) => Promise<...>`。
///
/// 本文件只用它写两种名字：`output.txt`（完整文本）与 `image-N.ext`（图片）。目录选择与
/// 实际写入由调用方实现（`codemode.rs::output_files()`），本文件只消费返回的路径字符串。
pub type OutputFile = Arc<dyn Fn(String, OutputData) -> OutputFuture + Send + Sync>;

/// 工具的输出上限（Harness 按它裁剪，且不带溢出文件）。
///
/// 与 `codemode.rs` 的 `OUTPUT_LIMITS` 是同一组值：`truncate()` 用它把文本预算压到
/// `max_bytes/4` 以内，并按 `max_lines - LINE_MARGIN` 限制首尾行数；Harness 的裁剪是
/// 最后兜底。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// 最大字节数。
    pub max_bytes: usize,
    /// 最大行数。
    pub max_lines: usize,
}

/// 结果里除脚本文本以外的行数：头、标记、备注。
const LINE_MARGIN: usize = 100;

/// 脚本输出的默认 token 预算。
///
/// 脚本没有在首行写 `// @options: {"max_output_tokens": ...}` 时使用（`codemode.rs` 把
/// 值存进 start 数据，重跑沿用首次运行的值）。单位是估算 token：每 4 个码元算 1 个。
pub const DEFAULT_MAX_OUTPUT_TOKENS: f64 = 10_000.0;
/// 估算 token 时每个 token 的字符数。
const CHARS_PER_TOKEN: f64 = 4.0;

/// `image()` 能接受的图片类型的扩展名。
///
/// 只映射四种常见 MIME 类型；未知类型写 `.bin`，内容仍是原始字节。
fn image_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        _ => ".bin",
    }
}

/// 同脚本的 `text()`：字符串原样，其他值用紧凑 JSON。
fn value_text(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        other => kiana_json::js_object::stringify(other),
    }
}

/// 文本项。
fn text_item(text: impl Into<String>) -> Item {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.into(),
        text_signature: None,
    })
}

/// 图片项。
fn image_item(data: impl Into<String>, mime_type: impl Into<String>) -> Item {
    UserContentBlock::Image(ImageContent {
        kind_image: ImageKind::Image,
        data: data.into(),
        mime_type: mime_type.into(),
    })
}

/// 把脚本输出排成模型能分辨的样子：文本项多于一个时每项以 `==> text N/M <==` 开头；
/// `console` 行放在所有其它输出之后，合成一个 `<console_output>` 块。
///
/// 编号只数非 console 的文本项（图片不参与）；`console: Some(false)` 按普通文本处理，
/// 构造器不会产生这种值。图片保持它们在输出里的相对顺序，console 块总在最后。
fn format_items(output: &[CodemodeOutputItem]) -> Vec<Item> {
    // 编号只数非 console 的文本项；图片不参与。
    let total = output
        .iter()
        .filter(|item| match item {
            CodemodeOutputItem::Text { console, .. } => *console != Some(true),
            CodemodeOutputItem::Image { .. } => false,
        })
        .count();
    let mut items: Vec<Item> = Vec::new();
    let mut console_lines: Vec<&str> = Vec::new();
    let mut index = 0usize;
    for item in output {
        match item {
            CodemodeOutputItem::Image { data, mime_type } => {
                items.push(image_item(data.clone(), mime_type.clone()));
            }
            CodemodeOutputItem::Text { text, console } if *console == Some(true) => {
                console_lines.push(text);
            }
            CodemodeOutputItem::Text { text, .. } => {
                index += 1;
                let rendered = if total > 1 {
                    format!("==> text {index}/{total} <==\n{text}")
                } else {
                    text.clone()
                };
                items.push(text_item(rendered));
            }
        }
    }
    if !console_lines.is_empty() {
        items.push(text_item(format!(
            "<console_output>\n{}\n</console_output>",
            console_lines.join("\n")
        )));
    }
    items
}

/// 合并相邻的文本项，每段从新行开始。分隔符检查看最后一个非空片段，而不是拼好的整串
/// （与 TS 相同的线性复杂度）。
///
/// 规则：文本片段依次拼接；若上一段的最后一个非空片段不以换行结尾，先补一个 `\n`；图片
/// 原样保留并结束当前文本段。`last_part` 只记录最后一个非空片段——尾部是空串的文本项不会
/// 清空它，因此“上一段是否以换行结尾”始终针对真实内容判断；每个片段只检查一次，避免
/// 反复扫描已拼接的整串。
fn join_adjacent_text(items: Vec<Item>) -> Vec<Item> {
    let mut joined: Vec<Item> = Vec::with_capacity(items.len());
    let mut parts: Vec<String> = Vec::new();
    let mut last_part = String::new();
    let flush = |parts: &mut Vec<String>, joined: &mut Vec<Item>| {
        if !parts.is_empty() {
            joined.push(text_item(parts.join("")));
        }
        parts.clear();
    };
    for item in items {
        let text = match item {
            UserContentBlock::Text(text) => text,
            other => {
                flush(&mut parts, &mut joined);
                joined.push(other);
                last_part.clear();
                continue;
            }
        };
        if !last_part.is_empty() && !last_part.ends_with('\n') {
            parts.push("\n".to_owned());
            last_part = "\n".to_owned();
        }
        parts.push(text.text.clone());
        if !text.text.is_empty() {
            last_part = text.text;
        }
    }
    flush(&mut parts, &mut joined);
    joined
}

/// [`truncate`] 的结果。
///
/// `full_output_path` 只在文本被裁剪且 `output.txt` 写成功时存在；写失败时为 `None`，
/// 原因已经写进展示文本。
struct Truncated {
    items: Vec<Item>,
    full_output_path: Option<String>,
}

/// 应用 token 预算：合并后的文本超预算时，文本项合成一项，保留首尾；图片跟在后面。
/// 完整文本写进临时文件。
///
/// 预算与门槛：
/// - `budget = min(max_tokens * 4, max_bytes / 4)`：同时受工具输出字节上限约束；
/// - `line_budget = (max_lines - LINE_MARGIN) / 2`：`LINE_MARGIN` 留给头与标记，首尾各
///   一半；
/// - 不裁剪的条件：没有文本，或文本 UTF-16 长度不超过 `budget` 且行数不超过
///   `2 * line_budget`。
///
/// 裁剪时：
/// - 头部取前 `floor(budget/2)` 个 UTF-16 码元、最多 `line_budget` 行；
/// - 尾部取最后 `floor(tail_chars)` 个码元（`tail_chars = budget - head_chars`）、最多
///   `line_budget` 行；
/// - 中间被去掉的码元数换算成 token 写进 `…N tokens truncated…`，原始 token 数用
///   `ceil(总长度/4)`；
/// - 所有文本项先按换行合并，因此裁剪后的单块文本里会保留（可能被切断的）
///   `==> text N/M` 标记；
/// - 写 `output.txt` 成功时在文本末尾附 `[Full output: <path> ...]`，失败则附原因。
///
/// 字节上限的余量：`max_bytes / 4` 的假设是一个 UTF-16 码元最多 3 个 UTF-8 字节，加上
/// 头与标记的少量字节，文本部分不超过 `max_bytes`（测试
/// `keeps_a_result_inside_the_tool_output_limits_whatever_max_output_tokens_says` 锁定）。
async fn truncate(
    items: &[Item],
    max_tokens: f64,
    limits: Limits,
    write: &OutputFile,
) -> Result<Truncated, BoxError> {
    // 所有文本项（含编号标记）按换行合并成一份；图片不参与文本预算。
    let texts: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    let combined = texts.join("\n");
    // 也在上限之内：一个 UTF-16 码元最多 3 个 UTF-8 字节，结果的其余部分只有几行。
    let budget = (max_tokens * CHARS_PER_TOKEN).min((limits.max_bytes / 4) as f64);
    let line_budget = limits.max_lines.saturating_sub(LINE_MARGIN) / 2;
    let lines = combined.matches('\n').count() + 1;
    let combined_length = utf16_len(&combined);
    // 不裁剪时原样返回，保留每个文本项与图片的相对位置及 `==> text N/M` 编号。
    if texts.is_empty() || ((combined_length as f64) <= budget && lines <= 2 * line_budget) {
        return Ok(Truncated {
            items: items.to_vec(),
            full_output_path: None,
        });
    }
    // 首尾各分一半预算；尾部用剩余量，避免小数误差。
    let head_chars = (budget / 2.0).floor().max(0.0) as usize;
    let tail_chars = budget - head_chars as f64;
    let head_slice = utf16_slice(&combined, 0, Some(head_chars));
    let head = head_slice
        .split('\n')
        .take(line_budget)
        .collect::<Vec<_>>()
        .join("\n");
    let tail = if tail_chars > 0.0 {
        let count = tail_chars.trunc().max(0.0) as usize;
        let length = utf16_len(&combined);
        let start = length.saturating_sub(count);
        let tail_slice = utf16_slice(&combined, start, None);
        let mut lines: Vec<&str> = tail_slice.split('\n').collect();
        // 行数超出时丢掉更靠前的行，只保留最后 line_budget 行。
        if lines.len() > line_budget {
            lines.drain(..lines.len() - line_budget);
        }
        lines.join("\n")
    } else {
        String::new()
    };
    // 实际去掉的码元数（含被行预算丢掉的行）；token 数按 4 码元一个向上取整。
    let removed = combined_length - utf16_len(&head) - utf16_len(&tail);
    let original_tokens = (combined_length as f64 / CHARS_PER_TOKEN).ceil() as i64;
    let removed_tokens = (removed as f64 / CHARS_PER_TOKEN).ceil() as i64;
    let mut text = format!(
        "Warning: truncated output (original token count: {original_tokens})\nTotal output lines: {lines}\n\n{head}\u{2026}{removed_tokens} tokens truncated\u{2026}{tail}"
    );
    // 完整文本落盘；写失败只改变提示文本，不改变裁剪结果。
    let spilled = write("output.txt".to_owned(), OutputData::Text(combined)).await;
    match &spilled {
        OutputFileResult::Path(path) => {
            text.push_str(&format!(
                "\n\n[Full output: {path} (read with offset/limit)]"
            ));
        }
        OutputFileResult::Error(error) => {
            text.push_str(&format!("\n\n[Could not save the full output: {error}]"));
        }
    }
    // 裁剪后的文本合成一项，图片全部移到它后面（裁剪丢失的只是文本边界）。
    let mut result: Vec<Item> = vec![text_item(text)];
    result.extend(
        items
            .iter()
            .filter(|item| matches!(item, UserContentBlock::Image(_)))
            .cloned(),
    );
    Ok(Truncated {
        items: result,
        full_output_path: match spilled {
            OutputFileResult::Path(path) => Some(path),
            OutputFileResult::Error(_) => None,
        },
    })
}

/// `atob`：宽容 base64 解码（标准字母表，忽略 ASCII 空白）。
///
/// TS 的 `Uint8Array.from(atob(data), (char) => char.charCodeAt(0))`。错误文案与 Node 的
/// `atob` 一致：长度模 4 为 1 → "The string to be decoded is not correctly encoded."；
/// 其余非法字符 → "Invalid character"。
///
/// 规则（WHATWG forgiving-base64）：先移除 `\t`/`\n`/`\f`/`\r`/空格；长度是 4 的倍数时
/// 最多去掉末尾两个 `=`；之后长度模 4 为 1 是错误；每个字符贡献 6 位，满 8 位输出一个
/// 字节，末尾不足 8 位的余数被丢弃。非 ASCII 字节也会落到 "Invalid character"。
pub(crate) fn atob(data: &str) -> Result<Vec<u8>, BoxError> {
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut cleaned: Vec<u8> = Vec::with_capacity(data.len());
    for byte in data.bytes() {
        // `forgiving-base64 decode` 第 1 步：移除 ASCII 空白。TS 的 atob 只认这 5 个。
        if matches!(byte, b'\t' | b'\n' | 0x0c | b'\r' | b' ') {
            continue;
        }
        cleaned.push(byte);
    }
    if cleaned.len().is_multiple_of(4) {
        // 移除末尾的一个或两个 `=`。
        if cleaned.last() == Some(&b'=') {
            cleaned.pop();
            if cleaned.last() == Some(&b'=') {
                cleaned.pop();
            }
        }
    }
    if cleaned.len() % 4 == 1 {
        return Err("The string to be decoded is not correctly encoded.".into());
    }
    let mut out: Vec<u8> = Vec::with_capacity(cleaned.len() / 4 * 3 + 2);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for byte in &cleaned {
        let Some(value) = digit(*byte) else {
            return Err("Invalid character".into());
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Ok(out)
}

/// 把每张图片写进文件，并在图片项前面放一项带路径的文本；同一张图片只写一次。
///
/// 去重键是 base64 数据本身（不看 `mimeType`）：同一份数据出现多次时复用第一个标签，
/// 但每个图片项前仍各放一份该标签。文件按首次出现顺序命名 `image-1.ext`、`image-2.ext`…
/// 扩展名由 MIME 决定（未知类型用 `.bin`）。写失败只影响标签文本，图片块仍然保留。
///
/// 错误：`atob` 失败会返回 `Err`，整个格式化中止；这是本函数唯一的错误来源。
async fn save_images(items: Vec<Item>, write: &OutputFile) -> Result<Vec<Item>, BoxError> {
    let mut labels: Vec<(String, String)> = Vec::new();
    let mut result: Vec<Item> = Vec::with_capacity(items.len());
    for item in items {
        let image = match item {
            UserContentBlock::Image(image) => image,
            other => {
                result.push(other);
                continue;
            }
        };
        // 去重看 base64 数据：同一张图只写一次文件，但每次出现都重新插入标签。
        let label = match labels
            .iter()
            .find(|(data, _)| data == &image.data)
            .map(|(_, label)| label.clone())
        {
            Some(label) => label,
            None => {
                let index = labels.len() + 1;
                let bytes = atob(&image.data)?;
                let kind = format!("{}, {} bytes", image.mime_type, bytes.len());
                // 写失败不能丢掉结果：它成为标签的一部分。
                let saved = write(
                    format!("image-{index}{}", image_extension(&image.mime_type)),
                    OutputData::Bytes(bytes),
                )
                .await;
                let label = match saved {
                    OutputFileResult::Path(path) => {
                        format!("[Image saved to {path} ({kind})]")
                    }
                    OutputFileResult::Error(error) => {
                        format!("[Image ({kind}) could not be saved: {error}]")
                    }
                };
                labels.push((image.data.clone(), label.clone()));
                label
            }
        };
        result.push(text_item(label));
        result.push(UserContentBlock::Image(image));
    }
    Ok(result)
}

/// `formatError` 用到的调用摘要（TS 的 `{ name, status }`）。
///
/// `codemode.rs::complete()` 从 `details.calls` 构造它；`details()` 已把仍在运行的行改成
/// `cancelled`，所以 `status` 实际是 `"ok"`/`"error"`/`"cancelled"`，这里原样拼进文本。
#[derive(Debug, Clone, Copy)]
pub struct CallSummary<'a> {
    /// 工具名。
    pub name: &'a str,
    /// 状态字面量。
    pub status: &'a str,
}

/// 失败结果的头：脚本错误的 stack 优先，其它类别加固定前缀；再附失败前的调用摘要。
///
/// `kind == Script` 时用 `stack`（没有则 `name: message`，`name` 缺省 `Error`）；
/// `Timeout`/`Aborted`/`Sandbox` 分别加 `Script timed out:`/`Script aborted:`/
/// `Script sandbox failed:`。`calls` 为空时写“没有调用”，否则列出 `名字 (状态)`，并声明
/// 这些调用不会被撤销。
fn format_error(result: &CodemodeResultErr, calls: &[CallSummary<'_>]) -> String {
    let error = &result.error;
    let head = match error.kind {
        CodemodeErrorKind::Script => match &error.stack {
            Some(stack) => stack.clone(),
            None => format!(
                "{}: {}",
                error.name.as_deref().unwrap_or("Error"),
                error.message
            ),
        },
        CodemodeErrorKind::Timeout => format!("Script timed out: {}", error.message),
        CodemodeErrorKind::Aborted => format!("Script aborted: {}", error.message),
        CodemodeErrorKind::Sandbox => format!("Script sandbox failed: {}", error.message),
    };
    let summary = if calls.is_empty() {
        "No tool calls were made.".to_owned()
    } else {
        let calls = calls
            .iter()
            .map(|call| format!("{} ({})", call.name, call.status))
            .collect::<Vec<_>>()
            .join(", ");
        format!("Tool calls made before the failure (they are not undone): {calls}")
    };
    format!("{head}\n\n{summary}")
}

/// [`format_result`] 的选项。
///
/// 由 `codemode.rs::complete()` 构造：`wall_ms` 是 `now - startedAt`，
/// `max_output_tokens` 来自 start 数据，`limits` 与工具注册项的 `output_limits` 同源，
/// `calls` 只用于失败结果的摘要，`write` 负责实际落盘。
pub struct FormatResultOptions<'a> {
    /// `models.generateImages()` 返回的图片数；脚本一张都没展示时结果会说明。
    pub generated_images: usize,
    /// 实际耗时。
    ///
    /// 单位毫秒；头部按秒显示，保留一位小数。
    pub wall_ms: f64,
    /// 输出的 token 预算。
    ///
    /// 估算 token：每 4 个码元算 1 个。
    pub max_output_tokens: f64,
    /// 脚本已做的调用。
    pub calls: &'a [CallSummary<'a>],
    /// 工具的输出上限。
    pub limits: Limits,
    /// 写结果文件。
    pub write: OutputFile,
}

/// [`format_result`] 的结果：工具结果的内容，以及被裁剪时完整文本的路径。
///
/// `content` 已含头与所有文本/图片项；`full_output_path` 只在文本被裁剪且 `output.txt`
/// 写成功时是 `Some`（写失败时提示文本里有原因）。
pub struct FormatResult {
    /// 内容。
    pub content: Vec<Item>,
    /// 完整文本文件。
    pub full_output_path: Option<String>,
}

/// 一个脚本结果对应的工具结果内容；被裁剪时附带完整文本的路径。
///
/// 输入是沙箱返回的 [`CodemodeResult`] 与调用方提供的展示选项；输出是工具结果的
/// `content` 与溢出文件路径。函数是异步的：`write` 回调在裁剪与保存图片时都可能被调用。
///
/// 成功与失败共用同一条管线：
/// - `Ok`：`value`（若存在）作为额外文本项；`Err`：追加 `Script error:` 段。
/// - 若 `generated_images > 0` 但结果里一张图片都没有，追加一条提示（脚本没展示生成的图）。
/// - 之后经历 合并文本 → 裁剪 → 保存图片 → 再合并 → 加固定头。
///
/// 错误：目前只有图片 base64 解码失败会返回 `Err`；文件写入失败被降级为结果文本。
pub async fn format_result(
    result: &CodemodeResult,
    options: FormatResultOptions<'_>,
) -> Result<FormatResult, BoxError> {
    // 阶段 1：拆出两路。成功结果把返回值当作最后一段文本输出；失败结果不输出返回值。
    let (ok, output, value) = match result {
        CodemodeResult::Ok(CodemodeResultOk { output, value, .. }) => {
            (true, output.as_slice(), value.as_ref())
        }
        CodemodeResult::Err(CodemodeResultErr { output, .. }) => (false, output.as_slice(), None),
    };
    let mut output_items = output.to_vec();
    if ok && let Some(value) = value {
        output_items.push(CodemodeOutputItem::text(value_text(value)));
    }
    // 阶段 2：排版（多文本编号、console 收拢、图片按原位置插入）。
    let mut items = format_items(&output_items);
    if let CodemodeResult::Err(error) = result {
        items.push(text_item(format!(
            "Script error:\n{}",
            format_error(error, options.calls)
        )));
    }
    // 阶段 3：脚本没展示的生成图片提醒；只看图片块，不看文本。
    let generated = options.generated_images;
    if generated > 0
        && !items
            .iter()
            .any(|item| matches!(item, UserContentBlock::Image(_)))
    {
        let plural = if generated == 1 { "" } else { "s" };
        items.push(text_item(format!(
            "Note: models.generateImages() returned {generated} image{plural} that the script did not show. Show each image block of result.output with image(block)."
        )));
    }
    // 阶段 4：先合并相邻文本，再按预算裁剪。
    let truncated = truncate(
        &join_adjacent_text(items),
        options.max_output_tokens,
        options.limits,
        &options.write,
    )
    .await?;
    // 阶段 5：保存图片。
    // 在裁剪之后：它合并文本项并把图片挪到后面，这样每个路径都紧挨着它的图片。
    let content = join_adjacent_text(save_images(truncated.items, &options.write).await?);
    // 阶段 6：加固定头；头单独成项，不参与文本合并。
    let header = format!(
        "{}\nWall time {} seconds\nOutput:\n",
        if ok {
            "Script completed"
        } else {
            "Script failed"
        },
        kiana_ai::model_catalog::js_to_fixed(options.wall_ms / 1000.0, 1)
    );
    let mut with_header: Vec<Item> = Vec::with_capacity(content.len() + 1);
    with_header.push(text_item(header));
    with_header.extend(content);
    Ok(FormatResult {
        content: with_header,
        full_output_path: truncated.full_output_path,
    })
}
