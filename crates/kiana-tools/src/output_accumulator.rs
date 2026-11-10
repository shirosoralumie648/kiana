//! core/tools/output-accumulator.ts。
//!
//! 增量跟踪流式输出、用有界内存保存显示快照，并在需要保留完整输出时落一个临时文件。
//!
//! # 计数语义（名字相近但互不相同，见 spec §2.4）
//!
//! - `total_raw_bytes`：`append()` 收到的原始字节数之和。
//! - `total_decoded_bytes`：解码后文本**重新按 UTF-8 编码**的字节数之和。非法输入被替换成
//!   U+FFFD（3 字节），因此它可能大于 `total_raw_bytes`。
//! - `tail_bytes`：滚动缓冲文本的 UTF-8 字节数，不是容量。
//! - `max_rolling_bytes = max(max_bytes * 2, 1)`；裁剪触发条件是 `tail_bytes > 2 * max_rolling_bytes`。
//!
//! # 流式 UTF-8 解码
//!
//! `Utf8StreamDecoder`（`pub(crate)`，不进入公开 API）按 Node `TextDecoder`（UTF-8 快路径）
//! 实现：`last = false` 时**保留**末尾 0–3 字节的不完整序列，等下一块补齐（逐块
//! `from_utf8_lossy` 会把截断的 emoji 永久损坏）；非法序列按「maximal subpart」分组替换为
//! U+FFFD；流开头的 UTF-8 BOM 被丢弃（`TextDecoder` 默认 `ignoreBOM: false`），流的其余
//! 位置保留 U+FEFF。
//!
//! # 临时文件
//!
//! 用 [`kiana_core::output_files::create_output_file_stream`] 创建（`0600`、`create_new`、随机
//! hex 名），**不**在这里重新实现路径生成。写入是同步的（TS 的 `WriteStream` 也是同步缓冲
//! 写入），`close_temp_file` 显式 flush，保证后续 `read_full_output` 能读到。
//!
//! # 在 crate 中的位置与调用方
//!
//! `bash` 工具为每次执行创建一个累加器（前缀 `kiana-bash`；powershell 用
//! `kiana-powershell`），在子进程输出回调里调用 [`OutputAccumulator::append`]；
//! `finish_output` 再依次调用 [`OutputAccumulator::finish`]、[`OutputAccumulator::snapshot`]
//! 与 [`OutputAccumulator::close_temp_file`]；`execute` 最后用
//! [`OutputAccumulator::read_full_output`] 生成 codemode 的 `structuredContent`。
//! `bash_executor` 只复用本模块的 `Utf8StreamDecoder`，不使用累加器（它在每块上先做
//! ANSI/二进制清理，语义不同）。
//!
//! # 执行顺序
//!
//! 1. `append`：计入原始字节 → 流式解码 → 更新计数与滚动尾部 → 判断是否落盘；
//! 2. 滚动尾部超过 `2 * max_rolling_bytes` 时裁剪到不超过 `max_rolling_bytes`，并记录
//!    裁剪点前一个字节是否为换行；快照据此决定是否丢弃半行；
//! 3. `finish`：把解码器残留的半个字符替换为 U+FFFD 后结束；
//! 4. `snapshot`：对当前滚动尾部再做一次 `truncate_tail`，但 `total_lines`/`total_bytes`
//!    用累加器的全量统计覆盖；
//! 5. `read_full_output`：有临时文件时读首尾两段，否则解码内存里的原始块。
//!
//! # 注意事项
//!
//! - 本类型没有内部同步；除只读的 `read_full_output` 与各诊断 getter 外，其余方法都要求
//!   `&mut self`，调用方必须串行访问（`bash` 工具用互斥锁保护）。
//! - 临时文件创建/写入失败不是「原子失败」：`append` 在落盘之前已经更新了解码文本、统计
//!   与滚动尾部，而这一块的原始字节只在成功路径才进入 `raw_chunks` 或文件。调用方（bash
//!   工具）把首个错误当作整个工具调用失败。

use std::io::{BufWriter, Write as _};
use std::path::PathBuf;

use crate::truncate::{TruncatedBy, TruncationOptions, TruncationResult, truncate_tail};

/// `DEFAULT_MAX_LINES` 的再导出（本模块的默认值来自 truncate）。
use crate::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};

/// `OutputAccumulatorOptions`。
///
/// TS: `output-accumulator.ts#OutputAccumulatorOptions`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputAccumulatorOptions {
    /// 最大行数，缺省 [`DEFAULT_MAX_LINES`]。
    pub max_lines: Option<usize>,
    /// 最大字节数，缺省 [`DEFAULT_MAX_BYTES`]。
    pub max_bytes: Option<usize>,
    /// 临时文件名前缀，缺省 `"kiana-output"`。
    pub temp_file_prefix: Option<String>,
}

/// `OutputSnapshot`：显示用快照。
///
/// TS: `output-accumulator.ts#OutputSnapshot`。`truncation` 是全量统计与快照截断的合并结果。
/// 其中 `content`/`output_lines`/`output_bytes` 描述的是**这段快照文本**（来自滚动尾部），
/// 而 `total_lines`/`total_bytes` 是自第一次 `append` 起的全量统计；两者不要混用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSnapshot {
    /// 快照内容。
    pub content: String,
    /// 截断详情。
    pub truncation: TruncationResult,
    /// 完整输出的临时文件（尚未创建时为 `None`）。
    pub full_output_path: Option<String>,
}

/// `FullOutput`：程序化调用者读取的完整输出。
///
/// TS: `output-accumulator.ts#FullOutput`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullOutput {
    /// 内容；超过 `max_bytes` 时首尾各保留一半，中间用省略标记。
    pub content: String,
    /// `content` 是否省略了部分输出。
    pub truncated: bool,
}

/// [`OutputAccumulator::append`] / [`OutputAccumulator::finish`] 的错误。
#[derive(Debug, thiserror::Error)]
pub enum OutputAccumulatorError {
    /// 对已 `finish()` 的累加器继续写入（TS 的 `throw new Error(...)`，文本逐字）。
    #[error("Cannot append to a finished output accumulator")]
    Finished,
    /// 临时文件 I/O 错误。
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// 按 Node `TextDecoder`（UTF-8 快路径）语义的增量 UTF-8 解码器。
///
/// TS 的 `new TextDecoder()`（非 fatal，`ignoreBOM: false`）：非法字节替换为 U+FFFD，
/// `{stream: true}` 保留不完整尾，流开头的 U+FEFF 被丢弃。实现逐字移植
/// `lib/internal/encoding.js#TextDecoder.prototype.decode` 的 UTF-8 快路径：
/// [`Self::pending`] 对应 `this[kChunk]`，[`Self::bom_seen`] 对应 `this[kBOMSeen]`。
pub(crate) struct Utf8StreamDecoder {
    /// 上一块末尾未完成序列的原始字节（0–3 字节；Node 的 `kChunk`）。
    pending: Vec<u8>,
    /// Node 的 "BOM seen"：一旦置位，后续缓冲区不再剥离 BOM。
    bom_seen: bool,
}

impl Utf8StreamDecoder {
    pub(crate) fn new() -> Self {
        Self {
            pending: Vec::new(),
            bom_seen: false,
        }
    }

    /// 解码一块数据。`last = false` 保留不完整尾；`last = true` 把未完成序列替换为 U+FFFD。
    ///
    /// 阶段与 Node 的 `TextDecoder.prototype.decode` 一致：
    ///
    /// 1. 非流式（`last = true`）且没有残留字节时走一次性快路径，BOM 由 `decode_utf8` 处理；
    /// 2. 流式空块直接返回，不改变状态；
    /// 3. 有残留时先用 `merge_prefix_utf8` 拼出能补完首个码点的前缀，单独解码；
    /// 4. 流式模式下把末尾不完整序列存入 `pending`，其余部分解码；
    /// 5. 流式解码走到结尾时置 `bom_seen`，后续块不再剥离 BOM（整块都成为残留时会提前返回，
    ///    `bom_seen` 保持原值）。
    ///
    /// 调用开始时会快照 `ignore_bom`：`last = true` 会先清空 `bom_seen`，但本次解码仍按传入
    /// 值决定是否丢弃开头的 BOM。
    pub(crate) fn decode(&mut self, data: &[u8], last: bool) -> String {
        use std::borrow::Cow;

        let stream = !last;
        let ignore_bom = self.bom_seen;
        let mut input: Cow<'_, [u8]> = Cow::Borrowed(data);

        if !stream {
            self.bom_seen = false;
            if self.pending.is_empty() {
                return decode_utf8(data, ignore_bom);
            }
        }

        if input.is_empty() && stream {
            // Node: `if (u.length === 0 && stream) return '';`（不改变状态）。
            return String::new();
        }

        let mut prefix: Option<Vec<u8>> = None;
        if !self.pending.is_empty() {
            let chunk = std::mem::take(&mut self.pending);
            let merged = merge_prefix_utf8(&input, &chunk);
            if input.len() < 3 {
                input = Cow::Owned(merged);
            } else {
                let add = merged.len() - chunk.len();
                if add > 0 {
                    input = Cow::Owned(input[add..].to_vec());
                }
                prefix = Some(merged);
            }
        }

        if stream {
            let trail = unfinished_bytes_utf8(&input, input.len());
            if trail > 0 {
                self.pending = input[input.len() - trail..].to_vec();
                if prefix.is_none() && trail == input.len() {
                    // Node: `if (!prefix && trail === u.length) return '';`。
                    return String::new();
                }
                input = Cow::Owned(input[..input.len() - trail].to_vec());
            }
        }

        let mut output = String::new();
        if let Some(prefix) = &prefix {
            output.push_str(&decode_utf8(prefix, ignore_bom));
        }
        // Node: `decodeUTF8(u, ignoreBom || prefix, ...)`。绑定的第二个参数按
        // `args[1]->IsTrue()` 判断：`ignoreBom || prefix` 在 ignoreBom 为 false 时得到的是
        // Uint8Array 对象（`IsTrue()` 为 false），因此效果就是 `ignore_bom` 本身；后半段
        // 若以 BOM 开头同样会被丢弃。
        output.push_str(&decode_utf8(&input, ignore_bom));
        if stream {
            self.bom_seen = true;
        }
        output
    }
}

/// Node 绑定 `DecodeUTF8`（非 fatal）。
///
/// `ignore_bom = false` 且缓冲区以 UTF-8 BOM 开头（长度 ≥ 3）时先跳过 3 字节；其余按
/// WHATWG 的 maximal-subpart 规则做有损解码。
fn decode_utf8(data: &[u8], ignore_bom: bool) -> String {
    let skip = if !ignore_bom && data.len() >= 3 && data[..3] == [0xEF, 0xBB, 0xBF] {
        3
    } else {
        0
    };
    decode_lossy(&data[skip..])
}

/// WHATWG UTF-8 有损解码（flush 语义：末尾不完整序列替换为一个 U+FFFD）。
fn decode_lossy(input: &[u8]) -> String {
    let mut output = String::with_capacity(input.len());
    let mut index = 0usize;

    while index < input.len() {
        let byte = input[index];
        if byte < 0x80 {
            output.push(char::from(byte));
            index += 1;
            continue;
        }

        // (需要的续字节数, 初始 code point, 第一个续字节的下界, 上界)
        let (needed, initial, lower, upper) = match byte {
            0xC2..=0xDF => (1usize, u32::from(byte & 0x1F), 0x80u8, 0xBFu8),
            0xE0 => (2, u32::from(byte & 0x0F), 0xA0, 0xBF),
            0xE1..=0xEC => (2, u32::from(byte & 0x0F), 0x80, 0xBF),
            0xED => (2, u32::from(byte & 0x0F), 0x80, 0x9F),
            0xEE..=0xEF => (2, u32::from(byte & 0x0F), 0x80, 0xBF),
            0xF0 => (3, u32::from(byte & 0x07), 0x90, 0xBF),
            0xF1..=0xF3 => (3, u32::from(byte & 0x07), 0x80, 0xBF),
            0xF4 => (3, u32::from(byte & 0x07), 0x80, 0x8F),
            // 非法起始字节：单个 U+FFFD（maximal subpart 就是这一字节）。
            _ => {
                output.push('\u{FFFD}');
                index += 1;
                continue;
            }
        };

        let mut code_point = initial;
        let mut seen = 0usize;
        let mut lo = lower;
        let mut hi = upper;
        let mut next = index + 1;
        let mut invalid = false;

        while seen < needed {
            if next >= input.len() {
                // 序列在输入末尾被截断。
                break;
            }
            let continuation = input[next];
            if continuation < lo || continuation > hi {
                invalid = true;
                break;
            }
            code_point = (code_point << 6) | u32::from(continuation & 0x3F);
            lo = 0x80;
            hi = 0xBF;
            seen += 1;
            next += 1;
        }

        if seen == needed {
            output.push(char::from_u32(code_point).unwrap_or('\u{FFFD}'));
            index = next;
        } else if invalid {
            // 为一个 maximal subpart 产出一个替换字符，然后**重新处理**当前字节。
            output.push('\u{FFFD}');
            index = next;
        } else {
            // 输入末尾的不完整序列整体替换为一个 U+FFFD。
            output.push('\u{FFFD}');
            index = input.len();
        }
    }

    output
}

/// Node `internal/encoding/util.js#unfinishedBytesUtf8`。
///
/// 返回 `data[..len]` 末尾 0–3 个「尚未构成完整码点、但可能是某个码点前缀」的字节数。
fn unfinished_bytes_utf8(data: &[u8], len: usize) -> usize {
    let mut pos = 0usize;
    while pos < 2 && pos < len && (data[len - pos - 1] & 0xc0) == 0x80 {
        pos += 1;
    }
    if pos == len {
        // 没有留给 lead 的位置。
        return 0;
    }
    let lead = data[len - pos - 1];
    if !(0xc2..=0xf4).contains(&lead) {
        // 不是合法的 lead。
        return 0;
    }
    if pos == 0 {
        // 只有 lead；2 字节序列必须在这里返回。
        return 1;
    }
    if lead < 0xe0 || (lead < 0xf0 && pos >= 2) {
        // 2 字节序列，或 3 字节及以下但已有 2 个续字节。
        return 0;
    }
    let lower = if lead == 0xf0 {
        0x90
    } else if lead == 0xe0 {
        0xa0
    } else {
        0x80
    };
    let upper = if lead == 0xf4 {
        0x8f
    } else if lead == 0xed {
        0x9f
    } else {
        0xbf
    };
    let next = data[len - pos];
    if next >= lower && next <= upper {
        pos + 1
    } else {
        0
    }
}

/// Node `internal/encoding/util.js#mergePrefixUtf8`。
///
/// `data` 不足 3 字节时整体拼到 `chunk` 后面；否则返回 `chunk` 加上足够补完第一个码点的
/// 前缀字节（不含不完整尾）。
fn merge_prefix_utf8(data: &[u8], chunk: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return chunk.to_vec();
    }
    if data.len() < 3 {
        let mut merged = Vec::with_capacity(chunk.len() + data.len());
        merged.extend_from_slice(chunk);
        merged.extend_from_slice(data);
        return merged;
    }

    let mut temp = Vec::with_capacity(chunk.len() + 3);
    temp.extend_from_slice(chunk);
    temp.extend_from_slice(&data[..3]);
    for i in 1..=3 {
        let unfinished = unfinished_bytes_utf8(&temp, chunk.len() + i);
        if unfinished <= i {
            // i = 3 时 unfinished 至多为 3，必定命中。
            let add = i - unfinished;
            return if add > 0 {
                temp[..chunk.len() + add].to_vec()
            } else {
                chunk.to_vec()
            };
        }
    }
    chunk.to_vec()
}

/// 增量跟踪流式输出。
///
/// TS: `output-accumulator.ts#OutputAccumulator`。
///
/// 在 bash 工具中，一个实例对应一次工具调用，生命周期是：`new` → 多次
/// [`Self::append`] → [`Self::finish`] → [`Self::snapshot`] / [`Self::read_full_output`]
/// （读文件前先 [`Self::close_temp_file`]）。
///
/// 内部状态分三组：
///
/// - 统计计数：`total_raw_bytes`、`total_decoded_bytes`、`completed_lines`、`total_lines`、
///   `current_line_bytes`、`has_open_line`；
/// - 有界显示窗口：`tail_text`/`tail_bytes` 保存最近一段解码文本，`tail_starts_at_line_boundary`
///   记录窗口头是否正好在行首；
/// - 原始字节：尚未落盘时按块存进 `raw_chunks`，一旦创建临时文件就改为直接追加到文件
///   （`temp_file` + `temp_file_path`）。
///
/// 显示窗口不是全量内容：快照只保证包含最近的尾部，并且（裁剪发生后）从完整行开始。
/// 本类型内部没有锁；除 `read_full_output` 与各 getter 只需 `&self` 外，其余方法都要求
/// `&mut self`，调用方必须串行访问。
pub struct OutputAccumulator {
    max_lines: usize,
    max_bytes: usize,
    max_rolling_bytes: usize,
    temp_file_prefix: String,
    decoder: Utf8StreamDecoder,

    raw_chunks: Vec<Vec<u8>>,
    tail_text: String,
    tail_bytes: usize,
    tail_starts_at_line_boundary: bool,
    total_raw_bytes: usize,
    total_decoded_bytes: usize,
    completed_lines: usize,
    total_lines: usize,
    current_line_bytes: usize,
    has_open_line: bool,
    finished: bool,

    temp_file_path: Option<PathBuf>,
    temp_file: Option<BufWriter<std::fs::File>>,
}

impl OutputAccumulator {
    /// 构造累加器。TS: `new OutputAccumulator(options = {})`。
    ///
    /// `max_lines`/`max_bytes` 缺省取自 `truncate` 模块；`max_bytes` 同时决定滚动窗口大小
    /// （`max(2 * max_bytes, 1)`，用饱和乘法避免极大值溢出）与临时文件的触发阈值。
    /// 初始窗口视为从行首开始，且没有临时文件。
    pub fn new(options: OutputAccumulatorOptions) -> Self {
        let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
        let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
        Self {
            max_lines,
            max_bytes,
            max_rolling_bytes: max_bytes.saturating_mul(2).max(1),
            temp_file_prefix: options
                .temp_file_prefix
                .unwrap_or_else(|| "kiana-output".to_string()),
            decoder: Utf8StreamDecoder::new(),
            raw_chunks: Vec::new(),
            tail_text: String::new(),
            tail_bytes: 0,
            tail_starts_at_line_boundary: true,
            total_raw_bytes: 0,
            total_decoded_bytes: 0,
            completed_lines: 0,
            total_lines: 0,
            current_line_bytes: 0,
            has_open_line: false,
            finished: false,
            temp_file_path: None,
            temp_file: None,
        }
    }

    /// 写入一块原始字节。TS: `append(data: Buffer): void`。
    ///
    /// 已 `finish()` 时返回 [`OutputAccumulatorError::Finished`]（TS 抛错，文本逐字）。
    ///
    /// 顺序固定：先累加 `total_raw_bytes` 并解码，把解码文本并入统计与滚动尾部；然后才判断
    /// 是否需要临时文件。需要时原始字节（不是解码文本）原样追加到文件，否则压入
    /// `raw_chunks` 等待首次落盘时一起写入。因此落盘失败时解码文本与统计已经前进，但本块
    /// 原始字节不会被缓存。
    pub fn append(&mut self, data: &[u8]) -> Result<(), OutputAccumulatorError> {
        if self.finished {
            return Err(OutputAccumulatorError::Finished);
        }

        self.total_raw_bytes += data.len();
        let text = self.decoder.decode(data, false);
        self.append_decoded_text(&text);

        if self.temp_file.is_some() || self.should_use_temp_file() {
            self.ensure_temp_file()?;
            if let Some(writer) = self.temp_file.as_mut() {
                writer.write_all(data)?;
            }
        } else if !data.is_empty() {
            self.raw_chunks.push(data.to_vec());
        }
        Ok(())
    }

    /// 结束写入（幂等）。TS: `finish(): void`。
    ///
    /// 与 TS 相同：先置 `finished`（第二次调用直接返回），再 flush 解码器里剩余的不完整序列，
    /// 最后才尝试确保临时文件存在——临时文件错误不能阻止尾部字节进入内容。
    ///
    /// flush 出的 U+FFFD 也计入 `total_decoded_bytes` 与行统计；`finished` 一旦置位，即使
    /// 本次 `ensure_temp_file` 失败，第二次 `finish` 也直接返回 `Ok`、不会重试（但之后的
    /// `snapshot(true)` 仍会尝试创建临时文件）。
    pub fn finish(&mut self) -> Result<(), OutputAccumulatorError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let rest = self.decoder.decode(&[], true);
        self.append_decoded_text(&rest);
        if self.should_use_temp_file() {
            self.ensure_temp_file()?;
        }
        Ok(())
    }

    /// 生成显示快照。TS: `snapshot({ persistIfTruncated } = {})`。
    ///
    /// `output_lines`/`output_bytes` 来自快照文本的 `truncateTail`，而 `total_lines`/
    /// `total_bytes` 是全量统计；`truncated_by` 在 `truncateTail` 返回 `None` 时用总量回填。
    ///
    /// TS 的 `snapshot({persistIfTruncated})` 在创建临时文件失败时会同步抛错；本方法的冻结
    /// 签名是 `-> OutputSnapshot`，没有错误通道，因此这里尽力而为（失败时
    /// `full_output_path` 保持 `None`）。之后的 [`Self::append`] / [`Self::finish`] 会重试
    /// 创建并像 TS 一样返回原始的 I/O 错误。
    ///
    /// 快照只读滚动尾部：裁剪后若窗口不是从行首开始，`content` 会丢掉第一个不完整行（见
    /// `get_snapshot_text`）。除可能创建临时文件外，本方法不改变累加器状态。
    pub fn snapshot(&mut self, persist_if_truncated: bool) -> OutputSnapshot {
        let snapshot_text = self.get_snapshot_text();
        let tail_truncation = truncate_tail(
            &snapshot_text,
            &TruncationOptions {
                max_lines: Some(self.max_lines),
                max_bytes: Some(self.max_bytes),
            },
        );

        let truncated =
            self.total_lines > self.max_lines || self.total_decoded_bytes > self.max_bytes;
        let truncated_by = if truncated {
            tail_truncation
                .truncated_by
                .or(Some(if self.total_decoded_bytes > self.max_bytes {
                    TruncatedBy::Bytes
                } else {
                    TruncatedBy::Lines
                }))
        } else {
            None
        };

        let truncation = TruncationResult {
            content: tail_truncation.content,
            truncated,
            truncated_by,
            total_lines: self.total_lines as u64,
            total_bytes: self.total_decoded_bytes as u64,
            output_lines: tail_truncation.output_lines,
            output_bytes: tail_truncation.output_bytes,
            last_line_partial: tail_truncation.last_line_partial,
            first_line_exceeds_limit: tail_truncation.first_line_exceeds_limit,
            max_lines: self.max_lines as u64,
            max_bytes: self.max_bytes as u64,
        };

        if persist_if_truncated && truncation.truncated {
            // 冻结签名不能从这里返回错误：失败只表现为 `full_output_path == None`，
            // 下一次 append/finish 会重试创建并报告原始 I/O 错误。
            let _ = self.ensure_temp_file();
        }

        OutputSnapshot {
            content: truncation.content.clone(),
            truncation,
            full_output_path: self
                .temp_file_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
        }
    }

    /// 关闭临时文件（flush 并释放句柄）。TS: `async closeTempFile(): Promise<void>`。
    ///
    /// 与 TS 相同：没有临时文件时直接返回（`snapshot` 期间创建失败的路径不会在这里报错）。
    /// `flush` 失败会把错误返回给调用方，但句柄在 flush 之前已被取走，本次调用后
    /// `temp_file` 为 `None`，无法再次 flush；文件本身不会被删除。
    pub async fn close_temp_file(&mut self) -> std::io::Result<()> {
        if let Some(mut writer) = self.temp_file.take() {
            writer.flush()?;
            // BufWriter 在 drop 时关闭底层文件。
        }
        Ok(())
    }

    /// 读取完整输出。TS: `readFullOutput(maxBytes): Promise<FullOutput>`。
    ///
    /// 应先 `finish()` 并 `close_temp_file()`。超过 `max_bytes` 时保留首尾各一半，中间用
    /// `[... N bytes omitted ...]` 标记；头部末尾的半个字符被丢弃（流式解码保留不完整尾），
    /// 尾部跳过前导续字节。
    ///
    /// 没有临时文件时（输出从未超过阈值）直接解码内存中的 `raw_chunks` 并返回
    /// `truncated: false`——这个分支不检查 `max_bytes`；首尾截断只发生在临时文件分支。
    /// 内存分支与文件分支都用新解码器，因此流开头的 BOM 都会被丢弃。
    pub async fn read_full_output(&self, max_bytes: usize) -> std::io::Result<FullOutput> {
        let Some(temp_file_path) = &self.temp_file_path else {
            // TS: `new TextDecoder().decode(Buffer.concat(this.rawChunks))`（新的解码器 ⇒
            // 开头的 BOM 同样被丢弃）。
            let mut decoder = Utf8StreamDecoder::new();
            return Ok(FullOutput {
                content: decoder.decode(&self.raw_chunks.concat(), true),
                truncated: false,
            });
        };

        use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

        let mut file = tokio::fs::File::open(temp_file_path).await?;
        let size = file.metadata().await?.len() as usize;
        if size <= max_bytes {
            let mut buffer = vec![0u8; size];
            file.read_exact(&mut buffer).await?;
            // TS: `new TextDecoder().decode(await file.readFile())`。
            let mut decoder = Utf8StreamDecoder::new();
            return Ok(FullOutput {
                content: decoder.decode(&buffer, true),
                truncated: false,
            });
        }

        let head_bytes = max_bytes / 2;
        let tail_bytes = max_bytes - head_bytes;

        let mut head = vec![0u8; head_bytes];
        let mut tail = vec![0u8; tail_bytes];

        file.seek(std::io::SeekFrom::Start(0)).await?;
        file.read_exact(&mut head).await?;
        file.seek(std::io::SeekFrom::Start((size - tail_bytes) as u64))
            .await?;
        file.read_exact(&mut tail).await?;

        // 头部以流式方式解码：末尾不完整序列被保留（即被丢弃）。
        let mut head_decoder = Utf8StreamDecoder::new();
        let head_text = head_decoder.decode(&head, false);

        // 尾部跳过前导续字节后再整体解码（TS 同样是新的 `TextDecoder`：尾部若恰好以
        // BOM 开头也会被丢弃）。
        let mut tail_start = 0usize;
        while tail_start < tail.len() && (tail[tail_start] & 0xc0) == 0x80 {
            tail_start += 1;
        }
        let mut tail_decoder = Utf8StreamDecoder::new();
        let tail_text = tail_decoder.decode(&tail[tail_start..], true);

        let omitted = size - head_bytes - tail_bytes;
        Ok(FullOutput {
            content: format!("{head_text}\n\n[... {omitted} bytes omitted ...]\n\n{tail_text}"),
            truncated: true,
        })
    }

    /// 当前未结束行的字节数（UTF-8 字节，不是字符数）。TS: `getLastLineBytes(): number`。
    pub fn get_last_line_bytes(&self) -> usize {
        self.current_line_bytes
    }

    /// 累计解码后的字节数（测试与诊断用；TS 的私有 `totalDecodedBytes`）。
    pub fn total_decoded_bytes(&self) -> usize {
        self.total_decoded_bytes
    }

    /// 累计原始字节数（测试与诊断用；TS 的私有 `totalRawBytes`）。
    pub fn total_raw_bytes(&self) -> usize {
        self.total_raw_bytes
    }

    /// 行数（测试与诊断用；TS 的私有 `totalLines`）。
    pub fn total_lines(&self) -> usize {
        self.total_lines
    }

    /// 临时文件路径（测试与诊断用；TS 的私有 `tempFilePath`）。
    pub fn temp_file_path(&self) -> Option<&std::path::Path> {
        self.temp_file_path.as_deref()
    }

    /// 把一块已解码文本并入统计与滚动尾部。
    ///
    /// 三组状态在这里同步更新：
    ///
    /// - `total_decoded_bytes` 累加文本的 UTF-8 字节数（U+FFFD 占 3 字节，因此可能大于原始
    ///   字节数）；
    /// - `tail_text`/`tail_bytes` 是显示用滚动窗口，超过 `2 * max_rolling_bytes` 时立即裁剪；
    /// - 行统计：`completed_lines` 数换行符；`has_open_line` 表示最后一个换行之后是否还有
    ///   内容，`total_lines = completed_lines + has_open_line`，所以以换行结尾的输入不会多算
    ///   一行；`current_line_bytes` 只保留最后一行（最后一个换行之后）的字节数。
    ///
    /// 空文本直接返回，不改变任何状态。
    fn append_decoded_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let bytes = text.len();
        self.total_decoded_bytes += bytes;
        self.tail_text.push_str(text);
        self.tail_bytes += bytes;
        if self.tail_bytes > self.max_rolling_bytes.saturating_mul(2) {
            self.trim_tail();
        }

        let newlines = text.bytes().filter(|byte| *byte == b'\n').count();
        if newlines == 0 {
            self.current_line_bytes += bytes;
            self.has_open_line = true;
        } else {
            self.completed_lines += newlines;
            let last_newline = text.rfind('\n').unwrap_or(0);
            let tail = &text[last_newline + 1..];
            self.current_line_bytes = tail.len();
            self.has_open_line = !tail.is_empty();
        }
        self.total_lines = self.completed_lines + usize::from(self.has_open_line);
    }

    /// 把滚动尾部裁剪到不超过 `max_rolling_bytes` 字节。
    ///
    /// 起点从 `len - max_rolling_bytes` 开始；若落在 UTF-8 续字节上则向后推进到码点边界，
    /// 因此保留下来的后缀仍是合法 UTF-8（`from_utf8_lossy` 在这里不会产生替换字符），
    /// 实际长度可能小于 `max_rolling_bytes`。同时记录裁剪后的起点是否恰好落在行边界
    /// （前一个字节是 `\n`），供快照决定是否丢弃半个首行。
    fn trim_tail(&mut self) {
        let buffer = self.tail_text.as_bytes();
        if buffer.len() <= self.max_rolling_bytes {
            self.tail_bytes = buffer.len();
            return;
        }

        let mut start = buffer.len() - self.max_rolling_bytes;
        while start < buffer.len() && (buffer[start] & 0xc0) == 0x80 {
            start += 1;
        }

        self.tail_starts_at_line_boundary = if start == 0 {
            self.tail_starts_at_line_boundary
        } else {
            buffer[start - 1] == b'\n'
        };
        self.tail_text = String::from_utf8_lossy(&buffer[start..]).into_owned();
        self.tail_bytes = self.tail_text.len();
    }

    /// 返回供截断与显示的快照文本。
    ///
    /// 若滚动窗口是从某个 `\n` 之后开始的，直接返回；否则（窗口头是半行）丢弃到第一个
    /// `\n` 为止，避免快照以半行开头。窗口里完全没有换行时只能原样返回这段不完整行。
    fn get_snapshot_text(&self) -> String {
        if self.tail_starts_at_line_boundary {
            return self.tail_text.clone();
        }

        let Some(first_newline) = self.tail_text.find('\n') else {
            return self.tail_text.clone();
        };
        self.tail_text[first_newline + 1..].to_string()
    }

    /// 是否需要把完整输出落到临时文件。
    ///
    /// 三个条件取或：原始字节数、解码后字节数、总行数任一超过对应上限。原始字节数必须单独
    /// 检查：解码器丢弃流开头的 BOM 时 raw 会大于 decoded（测试
    /// `bom_bytes_do_not_count_toward_the_decoded_byte_limit` 依赖这一条触发落盘），只看
    /// 解码字节会让完整输出没有临时文件可读。
    fn should_use_temp_file(&self) -> bool {
        self.total_raw_bytes > self.max_bytes
            || self.total_decoded_bytes > self.max_bytes
            || self.total_lines > self.max_lines
    }

    /// 惰性创建临时文件并接管已缓存的原始块（幂等）。
    ///
    /// 首次触发时把 `raw_chunks` 中的历史原始字节按原顺序写入后清空；之后的 `append` 直接
    /// 写文件。`temp_file_path` 只在全部历史块写成功后设置，因此失败后状态仍是「没有临时
    /// 文件」，下一次 `append`/`finish` 会重新创建并从头写一遍。注意文件在写入之前就已经
    /// 由 `create_output_file_stream` 创建：若随后写入失败，临时目录里会留下一个未被记录的
    /// 半写文件。
    fn ensure_temp_file(&mut self) -> std::io::Result<()> {
        if self.temp_file_path.is_some() {
            return Ok(());
        }
        let (path, file) =
            kiana_core::output_files::create_output_file_stream(&self.temp_file_prefix, ".log")?;
        // 本函数是同步的（TS 的 `createWriteStream` 也同步创建）；tokio 的 `File` 没有待处理
        // 操作，`try_into_std` 在本进程内总是成功。失败时退化为重新打开同一个已创建的文件。
        let std_file = match file.try_into_std() {
            Ok(file) => file,
            Err(file) => {
                drop(file);
                std::fs::OpenOptions::new().write(true).open(&path)?
            }
        };
        let mut writer = BufWriter::new(std_file);
        for chunk in &self.raw_chunks {
            writer.write_all(chunk)?;
        }
        self.raw_chunks.clear();
        self.temp_file_path = Some(path);
        self.temp_file = Some(writer);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(chunks: &[&[u8]]) -> String {
        let mut decoder = Utf8StreamDecoder::new();
        let mut output = String::new();
        for chunk in chunks {
            output.push_str(&decoder.decode(chunk, false));
        }
        output.push_str(&decoder.decode(&[], true));
        output
    }

    #[test]
    fn streaming_decoder_retains_split_emoji() {
        let emoji = "\u{1F600}".as_bytes();
        let mut decoder = Utf8StreamDecoder::new();
        assert_eq!(decoder.decode(&emoji[..3], false), "");
        assert_eq!(decoder.decode(&emoji[3..], false), "\u{1F600}");
    }

    #[test]
    fn decoder_replaces_invalid_sequences_by_maximal_subpart() {
        // 与 spec §6.3 #10 的 7 个黄金用例一致（非流式）。
        let cases: &[(&[u8], &str)] = &[
            (&[0xE0, 0x80, 0x80], "\u{FFFD}\u{FFFD}\u{FFFD}"),
            (
                &[0xF0, 0x80, 0x80, 0x80],
                "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}",
            ),
            (&[0xC0, 0x80], "\u{FFFD}\u{FFFD}"),
            (&[0xED, 0xA0, 0x80], "\u{FFFD}\u{FFFD}\u{FFFD}"),
            (
                &[0xF5, 0x80, 0x80, 0x80],
                "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}",
            ),
            (&[0xE2, 0x82], "\u{FFFD}"),
            (&[0x80, 0x80, 0x80], "\u{FFFD}\u{FFFD}\u{FFFD}"),
        ];
        for (bytes, expected) in cases {
            assert_eq!(decode_all(&[bytes]), *expected, "{bytes:?}");
        }
    }

    #[test]
    fn trailing_newline_is_not_an_extra_line() {
        let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
        accumulator.append(b"a\n").expect("append");
        accumulator.finish().expect("finish");
        assert_eq!(accumulator.total_lines(), 1);
        assert_eq!(accumulator.get_last_line_bytes(), 0);
    }

    const DECODER_GOLDEN: &str =
        include_str!("../tests/fixtures/primitives/utf8_decoder.golden.json");

    /// 黄金用例由 Node v26 的 `TextDecoder`（默认 `ignoreBOM: false`）生成：覆盖 BOM 在整块/
    /// 跨块/多次出现/非开头出现时的剥离规则，以及非法序列的 maximal-subpart 分组。
    #[test]
    fn decoder_matches_the_node_text_decoder_goldens() {
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(DECODER_GOLDEN).expect("decoder fixture");
        assert!(!cases.is_empty());
        for case in cases {
            let name = case["name"].as_str().expect("name");
            let mut decoder = Utf8StreamDecoder::new();
            let mut output = String::new();
            for chunk in case["chunks"].as_array().expect("chunks") {
                let bytes: Vec<u8> = chunk
                    .as_array()
                    .expect("bytes")
                    .iter()
                    .map(|byte| byte.as_u64().expect("byte") as u8)
                    .collect();
                output.push_str(&decoder.decode(&bytes, false));
            }
            output.push_str(&decoder.decode(&[], true));
            assert_eq!(
                output,
                case["expected"].as_str().expect("expected"),
                "{name}"
            );
        }
    }

    #[test]
    fn leading_bom_is_dropped_and_not_counted() {
        let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
        accumulator.append(b"\xEF\xBB\xBFhi").expect("append");
        accumulator.finish().expect("finish");
        assert_eq!(accumulator.snapshot(false).content, "hi");
        // BOM 不进入解码文本：5 原始字节 → 2 解码字节（TS 的 `TextDecoder` 同样丢弃）。
        assert_eq!(accumulator.total_raw_bytes(), 5);
        assert_eq!(accumulator.total_decoded_bytes(), 2);
    }

    #[test]
    fn bom_split_across_appends_is_dropped() {
        let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
        accumulator.append(b"\xEF").expect("append");
        accumulator.append(b"\xBB").expect("append");
        accumulator.append(b"\xBFhi").expect("append");
        accumulator.finish().expect("finish");
        assert_eq!(accumulator.snapshot(false).content, "hi");
        assert_eq!(accumulator.total_decoded_bytes(), 2);
    }

    #[test]
    fn bom_after_the_first_output_is_preserved() {
        let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
        accumulator.append(b"a").expect("append");
        accumulator.append(b"\xEF\xBB\xBF").expect("append");
        accumulator.finish().expect("finish");
        assert_eq!(accumulator.snapshot(false).content, "a\u{FEFF}");
        assert_eq!(accumulator.total_decoded_bytes(), 4);
    }
}
