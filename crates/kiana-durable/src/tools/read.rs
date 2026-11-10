//! `tools/read.ts`：读取文本（按行/字节截断）与图片（可交给 [`ImageProcessor`]）。
//!
//! 与 TS 一样，一次读取只做一遍行扫描加一段头部读取：`readHead` 读取的字节足以让
//! [`truncate_head_of`] 得出与读取全文再截断相同的结论。图片附件与文本共用同一个结果结构，
//! 说明性备注都走诊断通道。
//!
//! # 这个模块负责什么
//! 注册并执行 `read` 工具：解析 `path`/`offset`/`limit`，通过执行环境打开二进制读取器，
//! 先按文件内容探测图片格式。文本走“行扫描 + 有界头部读取”的截断路径；图片要么交给
//! [`ImageProcessor`] 预处理（缩放/转码），要么在格式与限额都满足时按原字节内联。
//!
//! # 执行流程
//! 1. `execute_read` 反序列化 [`ReadToolInput`]，用 `require_env` 取执行环境（没有环境时
//!    返回普通错误），再用 `resolve_read_tool_path` 把路径解析成绝对路径（会尝试窄空格、
//!    NFD、弯引号等变体，见 `tools/path_utils.rs`）。
//! 2. `open_binary_reader` 打开文件后进入重试循环；每轮：
//!    - `reader.info()` 记录打开文件的 `size`/`mtime_ms`；
//!    - `detect_supported_image_mime_type_of` 只读头部判断 MIME（PNG 会按 64 KiB 块遍历
//!      chunk，直到 `acTL` 或 `IDAT`）；
//!    - 非图片调用 `read_text`；图片先由 `image_model` 查会话模型的能力与限额，再调用
//!      `read_image`；
//!    - 再取一次 `info`：`size` 与 `mtime_ms` 都没变，或者“文本且只是变大（追加）”，就接受
//!      结果；否则重读一次，第二次仍变化则返回 `{path} changed while it was read`。
//! 3. 循环结束（成功或失败）都会调用 `reader.close(context)`。
//!
//! # 文本读取的两个阶段
//! `scan_lines` 是一次顺序扫描：它只保留偏移与解码字节计数，返回选区的原始字节区间
//! `[scan.start, scan.end)` 和整份文件的行数，不保留文件内容（细节见 `env/line_scan.rs`）。
//! 然后 `read_head` 从选区起点读取，直到“看到 `DEFAULT_MAX_LINES` 个换行”或“解码文本超过
//! `DEFAULT_MAX_BYTES + 1` 字节”，这正是 [`truncate_head_of`] 要求的前缀契约；因此文本
//! 路径的内存占用与文件大小无关，而截断结论与“读完整选区再截断”（整份解码 → 按换行切分
//! → `slice(start, end)` → `truncateHead`）相同。选区可能远小于整份文件：例如 3000 行
//! 文件只选 10 行时，按选区统计不会命中 2000 行上限，按整份文件统计则会。
//!
//! # 边界与失败模式
//! - `offset`/`limit` 用 `f64` 承载，复刻 TS 的 `Array.prototype.slice` 语义：小数索引向零
//!   截断、负 `limit` 先与起始行相加、结果仍为负时才从文件末尾倒数、`0` 与 `NaN` 的
//!   `offset` 按未提供处理。JSON 无法表示 `NaN`，这条分支只用于与参考实现保持同构（见
//!   `tests/tools_read_differential.rs` 的模块说明）。
//! - `offset` 越过文件行数时返回 `Err`，消息形如
//!   `Offset 100 is beyond end of file (3 lines total)`；空选区（`limit` 为 0 或负到越过
//!   起点）返回空输出而不是错误。
//! - 文本截断、续读提示与图片处理说明都放进 `diagnostics`；`details` 只在发生截断时出现，
//!   形状是 `{ "truncation": { ... } }`，键序由 `truncation_details_json` 固定。
//! - 图片的“不可用”是工具级错误结果（`is_error = true` + `unsupported_image` 诊断），不是
//!   `Err`：这类调用仍以“完成”结算，模型与 UI 都能看到原因（见 `harness/tool.rs` 对
//!   `Ending::Completed` 与错误结果的说明）。
//! - `scan_lines`/`read`/`info` 的 IO 失败与取消都通过 `BoxError` 向上抛；`harness/tool.rs`
//!   对 `execute` 返回的 `Err` 先检查取消，已取消时返回 abort 错误并以 `aborted` 结算，
//!   只有非取消错误才折算成 `tool_error` 结果并以 `failed` 结算（见 `harness/tool.rs` 的
//!   execute 错误分支）。两条路径都不会让进程失败。
//!
//! # 注意事项
//! - 文本按 UTF-8 解码且**不会失败**：非法序列按 WHATWG 规则变成 U+FFFD，行计数与
//!   `selected_bytes` 都按替换后的文本计算；文件开头的 BOM 不计入（见 `env/decode.rs`）。
//! - 图片探测发生在重试循环内部，每次重读都会重新探测；`before.size` 同时是图片
//!   `ByteSource` 的文件大小。
//! - `read_image` 会把整张图片读入内存；没有处理器时先做 base64 长度检查，超限就不读整份
//!   文件内容（`tests/tools_images.rs` 的
//!   `refuses_an_image_too_large_to_send_without_reading_it` 同时断言单次读取不超过
//!   64 KiB）。
//! - `image_model` 在会话模型无法从目录解析时按“接受图片 + `DEFAULT_IMAGE_LIMITS`”处理
//!   （见 `tests/support/tools_env.rs` 的 `FakeApi::agent` 注释；默认限额来自
//!   `image_model` 里的 `unwrap_or` 分支，而不是那条测试注释）。

use std::sync::Arc;

use async_trait::async_trait;
use kiana_ai::types::ModelInputModality;
use kiana_ai::{ImageContent, ImageKind, TextContent, TextKind, UserContentBlock};
use kiana_chord::context::Context;
use kiana_chord::output::{character_end, utf8_byte_length};
use kiana_json::to_radix_string;
use serde_json::{Map as JsonMap, Value as JsonValue, json};

use crate::env::{BinaryReader, FileInfo, LineRange, LineScan, range_decoder, starts_with_bom};
use crate::harness::define::define_tool;
use crate::harness::types::{
    DiagnosticSeverity, ToolDiagnostic, ToolExecuteFn, ToolExecutionApi, ToolExecutionResult,
    ToolRegistration,
};
use crate::tools::env::require_env;
use crate::tools::image::{
    ByteSource, DEFAULT_IMAGE_LIMITS, INLINE_IMAGE_TYPES, ImageLimits, ImageProcessor,
    base64_length, detect_supported_image_mime_type_of, image_dimensions, to_base64,
};
use crate::tools::path_utils::resolve_read_tool_path;
use crate::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationOptions, TruncationResult,
    TruncationTotals, format_size, truncate_head_of,
};
use crate::types::BoxError;

/// 每次读取请求的字节数。
const READ_CHUNK: usize = 64 * 1024;

/// `read` 的参数（TS: `readSchema`）。
///
/// `offset`/`limit` 用 `f64` 而不是整数：参数来自 JSON，但后续要套用与 TS
/// `Array.prototype.slice` 相同的索引规则（小数向零截断、负数从末尾倒数），所以保留
/// 浮点表示。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolInput {
    /// 文件路径（相对或绝对）。
    ///
    /// `resolve_read_tool_path` 会依次尝试原路径、窄不换行空格的 `AM/PM` 变体、NFD 与
    /// 弯引号变体，返回第一个存在的路径；都不存在时返回原路径（见 `tools/path_utils.rs`）。
    pub path: String,
    /// 起始行（1 起）。
    ///
    /// 省略、`0` 或负数都从第 1 行开始；小数会保留给后面的 `slice` 语义（例如 `1.5`
    /// 的起始行是 `0.5`，诊断里显示为 `Line 1.5`，此时不指向任何整行）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<f64>,
    /// 最多读取多少行。
    ///
    /// 省略表示读到文件末尾；负值先与起始行相加，结果仍为负时才按 `Array.prototype.slice`
    /// 的负索引从文件末尾倒数（例如 `offset` 为 1 或省略时，`-3` 表示排除最后 3 行）；
    /// 相加结果不为负则落在起始行之前，选区为空——10 行文件、`offset = 5`（起始行 4）、
    /// `limit = -3` 相加得 1 就是这种情况。`limit = 0` 或负到越过文件起点时选区同样为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

/// `ReadToolDetails`：文本是怎么被截断的。
///
/// TS 里 `truncation` 是可选字段；Rust 只在确实发生截断时才构造本结构，所以这里不是
/// `Option`。图片结果不带 details。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolDetails {
    /// 截断信息。
    pub truncation: ReadTruncationDetails,
}

/// `Omit<TruncationResult, "content">`。
///
/// 字段语义与 [`crate::truncate::TruncationResult`] 相同，只是不携带正文。它不会直接按
/// serde 的形状进入工具结果：`truncation_details_json` 会把它展开成固定键序的 JSON，作为
/// `details.truncation` 交给 UI。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTruncationDetails {
    /// 是否发生截断。
    pub truncated: bool,
    /// 命中的上限；未截断时是 `null`。
    pub truncated_by: Option<String>,
    /// 原始内容的总行数。
    pub total_lines: usize,
    /// 原始内容的总字节数。
    pub total_bytes: usize,
    /// 截断输出里的完整行数。
    pub output_lines: usize,
    /// 截断输出的字节数。
    pub output_bytes: usize,
    /// 最后一行是否被部分截断（仅尾部截断的边界情况）。
    pub last_line_partial: bool,
    /// 第一行是否超过字节上限（头部截断）。
    pub first_line_exceeds_limit: bool,
    /// 实际生效的行数上限。
    pub max_lines: usize,
    /// 实际生效的字节上限。
    pub max_bytes: usize,
}

impl ReadTruncationDetails {
    /// 从截断器的结果复制字段；`truncated_by` 的枚举在这里变成字符串。
    fn from_result(result: &TruncationResult) -> Self {
        ReadTruncationDetails {
            truncated: result.truncated,
            truncated_by: result.truncated_by.map(|by| match by {
                TruncatedBy::Lines => "lines".to_owned(),
                TruncatedBy::Bytes => "bytes".to_owned(),
            }),
            total_lines: result.total_lines,
            total_bytes: result.total_bytes,
            output_lines: result.output_lines,
            output_bytes: result.output_bytes,
            last_line_partial: result.last_line_partial,
            first_line_exceeds_limit: result.first_line_exceeds_limit,
            max_lines: result.max_lines,
            max_bytes: result.max_bytes,
        }
    }
}

/// `Array.prototype.slice` 对索引的转换：NaN 是 0，其余向零截断。
///
/// 注意这里是**向零**而不是向下取整：`-0.5` 变成 `-0.0`，而 `-0.0 < 0.0` 为 `false`，
/// 所以它和 JS 一样不会被当成从末尾倒数的负索引。
fn slice_index(value: f64) -> f64 {
    if value.is_nan() { 0.0 } else { value.trunc() }
}

/// `Number.isSafeInteger(value)`：有限、无小数部分、绝对值不超过 2^53 - 1。
///
/// 只有安全整数才能无损转成 `u64` 交给 `scan_lines`；`NaN`、`±Infinity`、小数与超出该
/// 范围的值都返回 `false`。调用方据此把扫描起点回落到 0、终点回落到文件末尾
/// （`LineScanner::new` 会拒绝非安全整数）。
fn is_safe_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0
}

/// JS 模板字符串里的数字（`String(value)`）。
///
/// `to_radix_string` 在 radix 10 时委托 `kiana-json` 的 `format_js_number`，复刻
/// `Number::toString(10)`（包括 `NaN`/`±Infinity`/`-0` 的特殊写法）。诊断消息要逐字对齐
/// TS，所以不能直接用 Rust 的 `f64::to_string`：`kiana-json` 的模块文档指出 Rust 的
/// `Display` 会把大数写全、把 `1e-7` 写成 `0.0000001`，与 JS 的文本不同。
fn number_text(value: f64) -> String {
    to_radix_string(value, 10)
}

/// `readHead`：解码 `[start, end)` 的一部分，足以让 `truncateHeadOf` 下结论。
///
/// 以 `READ_CHUNK` 字节为块增量解码；[`range_decoder`] 会保留跨块的不完整 UTF-8 序列，
/// 所以分块方式不影响结果。两种情况下提前返回：
/// - 已解出 `DEFAULT_MAX_LINES` 个换行；
/// - 解码文本超过 `DEFAULT_MAX_BYTES + 1` 个 UTF-8 字节。
///
/// 这两条正对应 [`truncate_head_of`] 的“前缀可以短于全文”契约（见 [`TruncationTotals`]
/// 的文档），因此不必读完整份文件。`skip_bom` 为真且 `start == 0` 时从偏移 3 开始，
/// 丢弃文件开头的 UTF-8 BOM；这个标志必须由调用方先用 `starts_with_bom` 判断（本函数
/// 不复查前三个字节）。读到 `end` 或提前遇到 EOF 时，用 `decode(None)` 冲刷解码器里残留
/// 的字节。
async fn read_head(
    reader: &dyn BinaryReader,
    start: usize,
    end: usize,
    skip_bom: bool,
    context: &dyn Context,
) -> Result<String, BoxError> {
    let mut decoder = range_decoder();
    let mut text = String::new();
    let mut newlines: usize = 0;
    let mut position = if skip_bom && start == 0 { 3 } else { start };
    while position < end {
        let length = READ_CHUNK.min(end - position);
        let bytes = reader
            .read(position as u64, length, context)
            .await
            .map_err(BoxError::from)?;
        if bytes.is_empty() {
            break;
        }
        position += bytes.len();
        let decoded = decoder.decode(Some(&bytes));
        text.push_str(&decoded);
        newlines += decoded.matches('\n').count();
        if newlines >= DEFAULT_MAX_LINES || utf8_byte_length(&text) > DEFAULT_MAX_BYTES + 1 {
            return Ok(text);
        }
    }
    let rest = decoder.decode(None);
    text.push_str(&rest);
    Ok(text)
}

/// 扫描 `[start_line, end_line)` 行。
///
/// 把 [`LineRange`] 与上下文原样转交给 [`BinaryReader::scan_lines`]，并把
/// [`crate::env::FileError`] 装箱成 [`BoxError`]。`LineRange` 的行号是 0 起、左闭右开；
/// 非法区间由实现返回 `invalid` 错误（`Invalid line range`）。
async fn scan_lines_of(
    reader: &dyn BinaryReader,
    start_line: u64,
    end_line: Option<u64>,
    context: &dyn Context,
) -> Result<LineScan, BoxError> {
    reader
        .scan_lines(
            LineRange {
                start_line,
                end_line,
            },
            context,
        )
        .await
        .map_err(BoxError::from)
}

/// 一个可读取的图片字节源：`reader` 与调用上下文。
///
/// `detect_supported_image_mime_type_of` 只需要“按偏移读取 + 总大小”，这里把已打开的
/// [`BinaryReader`] 适配成 [`ByteSource`]，避免为探测重新打开文件。
struct ReaderSource<'a> {
    /// 已打开的文件句柄。
    reader: &'a dyn BinaryReader,
    /// 打开时的文件大小；PNG chunk 遍历用它作上界。
    size: u64,
    /// 传给每次读取的取消上下文。
    context: &'a dyn Context,
}

#[async_trait]
impl ByteSource for ReaderSource<'_> {
    fn size(&self) -> u64 {
        self.size
    }

    async fn read(&self, offset: u64, length: usize) -> Result<Vec<u8>, crate::env::FileError> {
        self.reader.read(offset, length, self.context).await
    }
}

/// `ImageModel`：会话模型是否接受图片，以及在什么限额内。
///
/// 对应 TS 的 `ImageModel`。`limits` 已经合并过默认值，`read_image` 直接使用。
struct ImageModel {
    /// 模型是否声明了图片输入（`input` 里含 `image`）。
    vision: bool,
    /// 预处理与内联检查使用的限额。
    limits: ImageLimits,
}

/// 会话模型的图片输入；模型无法解析时按“接受图片、默认限额”处理。
///
/// 解析链：`api.agent(context).model` 给出 provider/model id，再交给
/// `api.models().get_model(provider, model_id)` 做运行时查找。以下情况都得到
/// `vision = true` 与 [`DEFAULT_IMAGE_LIMITS`]：
/// - agent 没有配置模型（没有 `ModelRef`）；
/// - 模型目录里查不到该 provider/model（`get_model` 返回 `None`）。
///
/// 限额只取 `input_limits.images.resize` 的 `max_width`/`max_height`/`max_bytes`；
/// `jpeg_quality` 不属于 [`ImageLimits`]，这里不读取。任一字段缺省时单独回落到默认值。
async fn image_model(
    api: &dyn ToolExecutionApi,
    context: &dyn Context,
) -> Result<ImageModel, BoxError> {
    let agent = api.agent(context).await?;
    let model = match &agent.model {
        Some(reference) => api
            .models()
            .get_model(&reference.provider, &reference.model_id),
        None => None,
    };
    let vision = model
        .as_ref()
        .is_none_or(|model| model.input.contains(&ModelInputModality::Image));
    let resize = model
        .as_ref()
        .and_then(|model| model.input_limits.as_ref())
        .and_then(|limits| limits.images.as_ref())
        .and_then(|images| images.resize.as_ref());
    Ok(ImageModel {
        vision,
        limits: ImageLimits {
            max_width: resize
                .and_then(|resize| resize.max_width)
                .unwrap_or(u64::from(DEFAULT_IMAGE_LIMITS.max_width)) as u32,
            max_height: resize
                .and_then(|resize| resize.max_height)
                .unwrap_or(u64::from(DEFAULT_IMAGE_LIMITS.max_height))
                as u32,
            max_bytes: resize
                .and_then(|resize| resize.max_bytes)
                .unwrap_or(DEFAULT_IMAGE_LIMITS.max_bytes as u64) as usize,
        },
    })
}

/// 图片相关的工具级错误结果。
///
/// 与返回 `Err` 不同：`is_error = true` 的结果仍以“完成”结算（见 `harness/tool.rs` 的
/// `Ending::Completed` 说明），原因放在 `unsupported_image` 诊断里，所以模型与 UI 都能
/// 看到；非取消的 `Err` 会让这次工具任务失败并改成 `tool_error` 诊断，取消则走 abort
/// 路径（以 `aborted` 结算，不生成 `tool_error`）。`output` 是空列表，表示给模型的
/// 内容为空。
fn image_error(message: String) -> ToolExecutionResult {
    ToolExecutionResult {
        output: Some(Vec::new()),
        is_error: Some(true),
        diagnostics: Some(vec![ToolDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: Some("unsupported_image".to_owned()),
            message,
        }]),
        ..ToolExecutionResult::default()
    }
}

/// `prepare` 输出里真正进入图片块的两个字段。
///
/// 其余信息（`converted_from`/`resized`）不进入图片块，只转成给用户看 notes。
struct PreparedImageParts {
    /// 标准 base64 文本（无 data URI 前缀）。
    data: String,
    /// 输出 MIME，可能已被处理器转换。
    mime_type: String,
}

/// `readImage`：一张图片块，附一条说明诊断。
///
/// 两条互斥路径：
/// - 有 [`ImageProcessor`]：把整张图片读入内存交给 `prepare`，由它解码、扶正、缩放并在
///   需要时转码（能否压进 `limits` 也由 `prepare` 判断）；`None` 表示“无法为模型准备这张
///   图”，直接返回 `unsupported_image` 错误。
/// - 没有处理器：只接受 [`INLINE_IMAGE_TYPES`] 里的格式（PNG/JPEG/GIF/WebP）。先按文件
///   大小算 base64 长度并与 `max_bytes` 比较（`max_bytes` 的语义是 base64 字符串长度，
///   见 `ImageLimits`），超限就不读整份内容；再读整图、从头部读宽高并检查限额；全部通过
///   才编码成 base64。
///
/// 无论哪条路径，处理结果只通过 `notes` 变成**一条** info 诊断（`code = "image"`）；
/// 图片块本身只含 base64 与 MIME。模型不支持图片时仍返回图片块，只追加一条说明；把图片
/// 替换成占位文本发生在后续消息转换（`kiana_ai::types::ImageContent` 的文档说明了这一点）。
async fn read_image(
    reader: &dyn BinaryReader,
    info: &FileInfo,
    path: &str,
    mime_type: &str,
    processor: Option<&Arc<dyn ImageProcessor>>,
    model: &ImageModel,
    context: &dyn Context,
) -> Result<ToolExecutionResult, BoxError> {
    // 两条路径的产物相同：一个可直接放进图片块的 base64 + MIME；差异只体现在 notes。
    let image: PreparedImageParts;
    let mut notes: Vec<String> = Vec::new();
    if let Some(processor) = processor {
        let bytes = reader
            .read(0, info.size as usize, context)
            .await
            .map_err(BoxError::from)?;
        let Some(prepared) = processor.prepare(&bytes, mime_type, model.limits).await else {
            return Ok(image_error(format!(
                "{path} is an image ({mime_type}) that cannot be prepared for the model"
            )));
        };
        image = PreparedImageParts {
            data: prepared.data,
            mime_type: prepared.mime_type.clone(),
        };
        if let Some(converted_from) = &prepared.converted_from {
            notes.push(format!(
                "Converted from {converted_from} to {}.",
                prepared.mime_type
            ));
        }
        if let Some(resized) = &prepared.resized {
            let (from_width, from_height) = resized.from;
            let (to_width, to_height) = resized.to;
            let scale = kiana_ai::model_catalog::js_to_fixed(
                f64::from(from_width) / f64::from(to_width),
                2,
            );
            notes.push(format!(
                "Resized from {from_width}x{from_height} to {to_width}x{to_height}. Multiply coordinates by {scale} to map them to the original."
            ));
        }
    } else {
        if !INLINE_IMAGE_TYPES.contains(mime_type) {
            return Ok(image_error(format!(
                "{path} is an image ({mime_type}) that needs converting, and no image processor is configured"
            )));
        }
        let unshrinkable = "and no image processor is configured to shrink it";
        // 先算 base64 长度再决定是否读文件：`base64_length(size) = ceil(size / 3) * 4`，
        // `max_bytes` 限制的就是这个字符串长度。
        if base64_length(info.size as usize) > model.limits.max_bytes {
            return Ok(image_error(format!(
                "{path} is an image ({mime_type}) of {}, too large to send, {unshrinkable}",
                format_size(info.size)
            )));
        }
        let bytes = reader
            .read(0, info.size as usize, context)
            .await
            .map_err(BoxError::from)?;
        let Some(size) = image_dimensions(&bytes, mime_type) else {
            return Ok(image_error(format!(
                "{path} is an image ({mime_type}) whose size cannot be read"
            )));
        };
        if size.width > model.limits.max_width || size.height > model.limits.max_height {
            return Ok(image_error(format!(
                "{path} is an image ({mime_type}) of {}x{}, larger than {}x{}, {unshrinkable}",
                size.width, size.height, model.limits.max_width, model.limits.max_height
            )));
        }
        image = PreparedImageParts {
            data: to_base64(&bytes),
            mime_type: mime_type.to_owned(),
        };
    }
    // 图片块照常返回；这条 note 只说明模型侧最终看到的是占位内容。
    if !model.vision {
        notes.push(
            "The current model does not support images; it sees a placeholder instead.".to_owned(),
        );
    }
    let mut message = format!("Read image file [{}].", image.mime_type);
    if !notes.is_empty() {
        message.push(' ');
        message.push_str(&notes.join(" "));
    }
    Ok(ToolExecutionResult {
        output: Some(vec![UserContentBlock::Image(ImageContent {
            kind_image: ImageKind::Image,
            data: image.data,
            mime_type: image.mime_type,
        })]),
        diagnostics: Some(vec![ToolDiagnostic {
            severity: DiagnosticSeverity::Info,
            code: Some("image".to_owned()),
            message,
        }]),
        ..ToolExecutionResult::default()
    })
}

/// 把 `TruncationResult`（去掉 content）变成 details 的 JSON，键序与 TS 相同。
///
/// 手写 `JsonMap` 是为了固定插入顺序：details 会原样进入工具结果 JSON，TS 的
/// `JSON.stringify` 按对象字面量的书写顺序输出（`truncated`、`truncatedBy`、…、
/// `maxBytes`）。未截断时 `truncatedBy` 显式写成 `null`（TS 的 `null`，不是缺键）。
fn truncation_details_json(details: &ReadTruncationDetails) -> JsonValue {
    let mut map = JsonMap::new();
    map.insert("truncated".to_owned(), json!(details.truncated));
    map.insert(
        "truncatedBy".to_owned(),
        match &details.truncated_by {
            Some(by) => json!(by),
            None => JsonValue::Null,
        },
    );
    map.insert("totalLines".to_owned(), json!(details.total_lines));
    map.insert("totalBytes".to_owned(), json!(details.total_bytes));
    map.insert("outputLines".to_owned(), json!(details.output_lines));
    map.insert("outputBytes".to_owned(), json!(details.output_bytes));
    map.insert(
        "lastLinePartial".to_owned(),
        json!(details.last_line_partial),
    );
    map.insert(
        "firstLineExceedsLimit".to_owned(),
        json!(details.first_line_exceeds_limit),
    );
    map.insert("maxLines".to_owned(), json!(details.max_lines));
    map.insert("maxBytes".to_owned(), json!(details.max_bytes));
    JsonValue::Object(map)
}

/// 已打开文件的文本读取结果。
///
/// 输入：已打开的 [`BinaryReader`]、原始 `path`（只用于诊断文本）、TS `number` 语义的
/// `offset`/`limit`，以及取消上下文。输出：文本块或空输出、可选的截断 `details`、以及
/// **总是存在**（可能为空数组）的 `diagnostics`。
///
/// 为了不把整份文件读进内存，这里通常只做一次 `scan_lines` 加一次有界的 `read_head`；负
/// `limit` 需要从文件末尾反算且选区非空时会再扫一次（最多两次 `scan_lines`），空选区则
/// 跳过 `read_head`。结果与“整份解码 → 按 `\n` 切分 → `slice(start, end)` → `truncateHead`”的
/// 输出、details 与诊断逐字段相同（按 JSON 比较），`tests/tools_read_differential.rs` 用
/// 400 个种子 × 4 组参数与参考实现对比。
///
/// 失败：
/// - `scan_lines`/`read` 的 IO 错误与取消直接以 [`BoxError`] 向上抛；
/// - `offset` 超过文件行数时返回 `Err`，消息形如
///   `Offset {offset} is beyond end of file ({lines} lines total)`。消息里用的是调用方
///   传入的原始 `offset`（可能是小数），而判断用的是换算后的 0 起 `start_line`。
///
/// 调用方 `execute_read` 负责在返回后比较文件元数据，判断读取期间文件是否变化。
async fn read_text(
    reader: &dyn BinaryReader,
    path: &str,
    offset: Option<f64>,
    limit: Option<f64>,
    context: &dyn Context,
) -> Result<ToolExecutionResult, BoxError> {
    // 阶段 1：把 offset/limit 换算成 0 起的行号与扫描区间（索引规则与 JS 的 slice 一致）。
    let start_line = match offset {
        Some(offset) if offset != 0.0 && !offset.is_nan() => (offset - 1.0).max(0.0),
        _ => 0.0,
    };
    let start_line_display = start_line + 1.0;
    // 行选择像 `allLines.slice(startLine, endLine)`：小数索引向零截断。
    let slice_start = slice_index(start_line);
    // 一遍扫描同时得到行数与选区；越过最后一行的选区以最后一行为结尾（与 slice 相同），
    // 空选区（零或负 limit）只扫一行然后被忽略。
    // 起点越过整个文件时从 0 开始扫只为数行数；下面的 offset 检查仍会像往常一样失败。
    let scan_start_value = if is_safe_integer(slice_start) {
        slice_start
    } else {
        0.0
    };
    let scan_start = scan_start_value as u64;
    let requested_end: Option<f64> =
        limit.map(|limit| (scan_start_value + 1.0).max(slice_index(start_line + limit)));
    let scan_end = requested_end
        .filter(|value| is_safe_integer(*value))
        .map(|value| value as u64);
    let mut scan: LineScan = scan_lines_of(reader, scan_start, scan_end, context).await?;
    let total_file_lines = (scan.newlines as f64) + 1.0;
    if start_line >= total_file_lines {
        return Err(std::io::Error::other(format!(
            "Offset {} is beyond end of file ({} lines total)",
            number_text(offset.unwrap_or(f64::NAN)),
            number_text(total_file_lines)
        ))
        .into());
    }

    // 阶段 2：按 limit 收窄选区（负 limit 的 slice 语义见下）。
    let mut user_limited_lines: Option<f64> = None;
    let mut selected_line_count = total_file_lines - slice_start;
    if let Some(limit) = limit {
        let end_line = (start_line + limit).min(total_file_lines);
        user_limited_lines = Some(end_line - start_line);
        // `slice` 把负的结束位置从行尾倒数；只有行数能算出来，需要再扫一次。
        let relative_end = slice_index(end_line);
        let slice_end = if relative_end < 0.0 {
            (total_file_lines + relative_end).max(0.0)
        } else {
            relative_end
        };
        selected_line_count = (slice_end - slice_start).max(0.0);
        if selected_line_count > 0.0 && relative_end < 0.0 {
            scan = scan_lines_of(reader, scan_start, Some(slice_end as u64), context).await?;
        }
    }
    // 阶段 3：把选区换算成全文统计。`total_lines` / `total_bytes` 会原样进入 details 与
    // 续读诊断，计行规则必须与 `truncate` 保持一致。
    let empty = selected_line_count == 0.0;
    // 与 `truncateHead` 一样计行：结尾换行不额外算一行，空文本没有行。
    let ends_with_newline =
        !empty && scan.last_line_start == scan.end && scan.last_line_start > scan.start;
    let total_lines = if empty || scan.selected_bytes == 0 {
        0usize
    } else {
        (selected_line_count - if ends_with_newline { 1.0 } else { 0.0 }) as usize
    };
    let total_bytes = if empty { 0 } else { scan.selected_bytes };
    let first_bytes = reader.read(0, 3, context).await.map_err(BoxError::from)?;
    let head = if empty {
        String::new()
    } else {
        read_head(
            reader,
            scan.start,
            scan.end,
            starts_with_bom(&first_bytes),
            context,
        )
        .await?
    };

    // 阶段 4：截断。`head` 满足 `truncate_head_of` 的前缀契约，`totals` 是选区统计（这里的
    // “全文”就是选区）；因此结论与“读完整选区再截断”（整份解码 → 按换行切分 →
    // `slice(start, end)` → `truncateHead`）一致，而不是与整份文件截断一致。
    // `TruncationOptions::new()` 使用默认的 2000 行 / 50 KiB 上限。
    let truncation = truncate_head_of(
        &head,
        TruncationTotals {
            lines: total_lines,
            bytes: total_bytes,
        },
        &TruncationOptions::new(),
    );
    let mut diagnostics: Vec<ToolDiagnostic> = Vec::new();
    let mut output_text = truncation.content.clone();
    let mut details: Option<ReadToolDetails> = None;
    if truncation.first_line_exceeds_limit {
        // 显示行首，在字节上限处按字符边界切开。和 `allLines[startLine]` 一样，小数起始行不指向任何行。
        let integral = start_line.is_finite() && start_line.fract() == 0.0;
        let line_bytes: Vec<u8> = if integral {
            head.split('\n').next().unwrap_or("").as_bytes().to_vec()
        } else {
            Vec::new()
        };
        let line_size = if integral { scan.first_line_bytes } else { 0 };
        let end = character_end(&line_bytes, DEFAULT_MAX_BYTES);
        // `lineBytes.subarray(0, end)` 在 `end` 超过长度时截到末尾（小数起始行的 `line_bytes` 为空）。
        output_text =
            String::from_utf8_lossy(&line_bytes[..end.min(line_bytes.len())]).into_owned();
        diagnostics.push(ToolDiagnostic {
            severity: DiagnosticSeverity::Warn,
            code: Some("truncated".to_owned()),
            message: format!(
                "Line {} is {}, exceeds the {} limit; showing its first {}. Use bash: sed -n '{}p' {} | tail -c +{}",
                number_text(start_line_display),
                format_size(line_size as u64),
                format_size(DEFAULT_MAX_BYTES as u64),
                format_size(end as u64),
                number_text(start_line_display),
                path,
                end + 1
            ),
        });
        // details 的统计改成实际展示的部分：字节数是字符边界处的位置，行数固定为 1
        // （即使小数起始行展示的是空内容，TS 也报告 1）。
        let mut truncated = ReadTruncationDetails::from_result(&truncation);
        truncated.output_bytes = end;
        truncated.output_lines = 1;
        details = Some(ReadToolDetails {
            truncation: truncated,
        });
    } else if truncation.truncated {
        // 其余截断：告诉模型实际显示的是哪几行、下一步 offset 是多少；字节截断时额外
        // 注明命中的是 50 KiB 上限（行数截断不写，保持 TS 的消息格式）。
        let end_line_display = start_line_display + truncation.output_lines as f64 - 1.0;
        let next_offset = end_line_display + 1.0;
        let limit_text = match truncation.truncated_by {
            Some(TruncatedBy::Lines) => String::new(),
            _ => format!(" ({} limit)", format_size(DEFAULT_MAX_BYTES as u64)),
        };
        diagnostics.push(ToolDiagnostic {
            severity: DiagnosticSeverity::Info,
            code: Some("truncated".to_owned()),
            message: format!(
                "Showing lines {}-{} of {}{}. Use offset={} to continue.",
                number_text(start_line_display),
                number_text(end_line_display),
                number_text(total_file_lines),
                limit_text,
                number_text(next_offset)
            ),
        });
        details = Some(ReadToolDetails {
            truncation: ReadTruncationDetails::from_result(&truncation),
        });
    } else if let Some(user_limited_lines) = user_limited_lines
        && start_line + user_limited_lines < total_file_lines
    {
        // 没有截断、但用户的 limit 提前结束了读取：提示剩余行数与续读 offset。
        // 这条诊断没有 code（TS 的对象字面量里就没有 `code` 键）。
        let remaining = total_file_lines - (start_line + user_limited_lines);
        let next_offset = start_line + user_limited_lines + 1.0;
        diagnostics.push(ToolDiagnostic {
            severity: DiagnosticSeverity::Info,
            code: None,
            message: format!(
                "{} more lines in file. Use offset={} to continue.",
                number_text(remaining),
                number_text(next_offset)
            ),
        });
    }

    // 阶段 5：输出。空文本给空列表而不是一个空文本块；details 只在两条截断分支里设置。
    Ok(ToolExecutionResult {
        output: Some(if output_text.is_empty() {
            Vec::new()
        } else {
            vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: output_text,
                text_signature: None,
            })]
        }),
        details: details
            .map(|details| json!({ "truncation": truncation_details_json(&details.truncation) })),
        diagnostics: Some(diagnostics),
        ..ToolExecutionResult::default()
    })
}

/// `createReadTool(options)`：注册名为 `read` 的工具。
///
/// - `description` 与 `parameters` 是模型看到的内容。支持格式列表取决于是否配置了
///   [`ImageProcessor`]：有处理器时多出 `bmp`，因为 BMP 不在 [`INLINE_IMAGE_TYPES`] 里，
///   需要先转换才能内联。文本上限直接取自 [`DEFAULT_MAX_LINES`] 与 [`DEFAULT_MAX_BYTES`]。
/// - `parameters` 的键序与字段描述被
///   `tests/tools.rs::tool_definitions_match_the_reference` 逐字比对；这是模型可见的
///   稳定契约，改动需同步该测试。
/// - 返回的 [`ToolRegistration`] 只设置了 `tool` 与 `execute`（其余字段为 `None`/`false`）：
///   `prepare_arguments`、`structured_output_schema`、`output_limits` 都缺省，所以参数修复、
///   结构化输出校验与输出截断走 harness 的默认行为。
/// - `options.images` 被克隆进执行闭包，同一注册上的每次调用共享这个处理器。
pub fn create_read_tool(options: ReadToolOptions) -> ToolRegistration {
    let formats = if options.images.is_none() {
        "jpg, png, gif, webp"
    } else {
        "jpg, png, gif, webp, bmp"
    };
    let images = options.images;
    let description = format!(
        "Read the contents of a file. Supports text files and images ({formats}). Images are sent as attachments. For text files, output is truncated to {} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
        DEFAULT_MAX_LINES,
        DEFAULT_MAX_BYTES / 1024
    );
    let parameters = json!({
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the file to read (relative or absolute)"
            },
            "offset": {
                "type": "number",
                "description": "Line number to start reading from (1-indexed)"
            },
            "limit": {
                "type": "number",
                "description": "Maximum number of lines to read"
            }
        }
    });
    let execute: ToolExecuteFn = Arc::new(move |args, api, context| {
        let images = images.clone();
        Box::pin(async move { execute_read(args, api, context, images).await })
    });
    define_tool(ToolRegistration {
        tool: kiana_ai::Tool {
            name: "read".to_owned(),
            description,
            parameters,
            constrained_sampling: None,
        },
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: None,
        callers: None,
        deferred: false,
        output_limits: None,
        execute: Some(execute),
    })
}

/// `read` 的执行体：打开文件、探测类型、读取，并在文件于读取期间变化时重试一次。
///
/// 重试规则（对应 TS 的同名循环）：
/// - `size` 与 `mtime_ms` 都没变：接受；
/// - 文本且 `size` 变大：接受。代码注释说明这是“追加（增长的日志）保持已扫到的文本
///   不变”的既有取舍；
/// - 其他变化（缩短、同大小但 mtime 变化、图片被改动）：丢弃结果重读；
/// - 第二次仍有变化：返回 `Err({path} changed while it was read)`。
///
/// `images` 是注册时捕获的图片处理器。`before`/`after` 都来自 `BinaryReader::info`，即
/// **已打开文件**的元数据，路径被改名不影响比较。`reader.close` 在循环之后无条件执行，
/// 出错路径也会关闭读取器；循环内的 `?` 只把错误存进 `result`，不会跳过 `close`。
async fn execute_read(
    args: JsonValue,
    api: Arc<dyn ToolExecutionApi>,
    context: Arc<dyn Context>,
    images: Option<Arc<dyn ImageProcessor>>,
) -> Result<ToolExecutionResult, BoxError> {
    let input: ReadToolInput = serde_json::from_value(args)?;
    let env = require_env(api.as_ref())?;
    let absolute_path = resolve_read_tool_path(env.as_ref(), &input.path, context.as_ref()).await?;
    let reader = env
        .open_binary_reader(&absolute_path, None, context.as_ref())
        .await
        .map_err(BoxError::from)?;
    let result = async {
        // 并发写入者可能在扫描与读取之间改动文件。追加（增长的日志）保持已扫到的文本不变；
        // 图片必须是完整的。否则只重读一次。
        let mut attempt = 0usize;
        loop {
            let before = reader
                .info(context.as_ref())
                .await
                .map_err(BoxError::from)?;
            let mime_type = detect_supported_image_mime_type_of(&ReaderSource {
                reader: reader.as_ref(),
                size: before.size,
                context: context.as_ref(),
            })
            .await?;
            let result = match &mime_type {
                None => {
                    read_text(
                        reader.as_ref(),
                        &input.path,
                        input.offset,
                        input.limit,
                        context.as_ref(),
                    )
                    .await?
                }
                Some(mime_type) => {
                    let model = image_model(api.as_ref(), context.as_ref()).await?;
                    read_image(
                        reader.as_ref(),
                        &before,
                        &input.path,
                        mime_type,
                        images.as_ref(),
                        &model,
                        context.as_ref(),
                    )
                    .await?
                }
            };
            let after = reader
                .info(context.as_ref())
                .await
                .map_err(BoxError::from)?;
            let unchanged = after.size == before.size && after.mtime_ms == before.mtime_ms;
            let text_growing = mime_type.is_none() && after.size > before.size;
            if unchanged || text_growing {
                return Ok(result);
            }
            if attempt == 1 {
                return Err(std::io::Error::other(format!(
                    "{} changed while it was read",
                    input.path
                ))
                .into());
            }
            attempt += 1;
        }
    }
    .await;
    reader.close(context.as_ref()).await;
    result
}

/// `createReadTool` 的选项。
///
/// `Default` 等于“不做图片预处理”：此时只有 PNG/JPEG/GIF/WebP 且宽高与 base64 长度都在
/// 模型限额内的图片会原样内联；BMP 等需要转码的格式会得到 `unsupported_image` 错误。
#[derive(Clone, Default)]
pub struct ReadToolOptions {
    /// 缩放并转换图片；没有它时，限额内的图片按原样传递。
    ///
    /// 处理器在 `read_image` 里收到整份文件字节、输入 MIME 与 `ImageLimits`，返回 `None`
    /// 表示“无法为模型准备这张图”；它可以转换格式（如 BMP → PNG）或按限额缩小。
    pub images: Option<Arc<dyn ImageProcessor>>,
}
