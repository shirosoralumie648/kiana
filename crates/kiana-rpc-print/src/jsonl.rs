//! modes/rpc/jsonl.ts：严格 JSONL 框架（LF-only）与增量 UTF-8 解码。
//!
//! TS: `packages/coding-agent/src/modes/rpc/jsonl.ts`
//! （`serializeJsonLine` / `attachJsonlLineReader`；spec §2.4、§4.1、§4.1.1）。
//!
//! ## 框架规则（spec §4.1）
//!
//! - 记录分隔符只有 `\n`；`\r`、U+2028、U+2029、U+0085、VT、FF 都不是边界。
//! - 每行剥离**一个**尾随 `\r`（`a\r\r\n` -> `a\r`；`a\r\n` -> `a`）。
//! - 空行以 `""` 回调。
//! - 输入结束（`finish`）时若非空，最后一段作为一条记录（无尾随 LF 也输出）。
//! - UTF-8 增量解码：跨 chunk 的不完整序列保留到下一 chunk；`finish` 对被截断的序列补 U+FFFD
//!   （合法前缀整体一个；range 无效的 lead 及其续字节各自一个）。见 §4.1.1 的 15 行表。
//!
//! ## 为什么自己实现而不是用 `encoding_rs`
//!
//! `encoding_rs` 的替换计数与 Node `StringDecoder("utf8")` 在 overlong / 代理区 / 截断 4 字节
//! 序列上不同（spec §4.1.1）。Rust 标准库 `str::from_utf8` 的 `Utf8Error::error_len` 恰好是
//! V8/WHATWG 的 “maximal subpart” 语义：`Some(n)` 是本次无效子段的字节数，`None` 表示输入以
//! 一个未完成的前缀结尾。逐段解码与 V8 的替换结果逐字节一致（由随机分块对拍保证）。
//!
//! ## 缓冲规则必须逐字复刻 `StringDecoder`（不只是替换语义）
//!
//! Node 的 decoder 在**形状**层面决定是否把尾部字节留到下一 chunk（`src/string_decoder.cc`
//! `DecodeData`）：从末尾向前扫最多 3 个 `10xxxxxx` 续字节，若它们前面的 lead 字节
//! （`110xxxxx` / `1110xxxx` / `11110xxx`，**不检查范围**）声明的长度大于已看到的字节数，
//! 就把这段整体留到下一 `write`/`end`。这与“用 `str::from_utf8` 的 `error_len == None`
//! 决定是否保留”不同：`e0 80` 形状上是 3 字节序列的前 2 字节（range 无效），Node 会保留，
//! 而 `from_utf8` 会立刻报错。纯字节输入下最终文本相同，但只要中间插入字符串 chunk
//! （`push_str` 不经过 decoder），替换字符出现的位置就会不同，进而改变行内容与行数。
//! 因此 [`JsonlLineReader`] 精确实现 Node 的两个状态（`pending` + `missing`）。
//!
//! 混合调用 `push_bytes` / `push_str` 时，未完成的字节缓冲独立存在：字符串 chunk 直接并入行
//! 缓冲（TS 的 `typeof chunk === "string"` 分支不经过 decoder）；被保留的字节在下次
//! `push_bytes` 或 `finish` 时产出（位置在该字符串之后）。
//!
//! ## 状态与数据流
//!
//! [`JsonlLineReader`] 只有三个字段，构成一个两段式流水线：
//!
//! ```text
//! bytes --(decoder: pending/missing)--> UTF-8 text --(buffer + '\n' 切分)--> lines
//! ```
//!
//! - `pending` + `missing` 是 decoder 的未完成序列（对应 Node 的 `incomplete` 与
//!   `lastNeed`）；只有 `missing == 0` 时才会去解码新字节。
//! - `buffer` 保存“已解码但没有换行”的文本。`push_*` 只返回**本次**因遇到 `\n` 而完成的
//!   行；余下文本留在 `buffer`，直到后续 chunk 或 [`JsonlLineReader::finish`]。
//! - `\n` 本身与它前面的一个 `\r` 被消费掉，不再出现在结果里；其余空白与其它
//!   Unicode 换行符（U+2028 等）按普通字符处理。
//!
//! ## 调用方与错误边界
//!
//! 生产路径是 `rpc_mode` 的输入循环（`crates/kiana-rpc-print/src/rpc_mode.rs` 直接
//! `JsonlLineReader::new()` 后逐块 `push_bytes`），因为它需要和 stdin 取消/关停的
//! `select!` 交织。[`pump_jsonl`] 由 `rpc_client` 读取子进程的 stdout 时使用
//! （`crates/kiana-rpc-print/src/rpc_client.rs`）；[`attach_jsonl_line_reader`] 是
//! `pump_jsonl` 的 trait-object 适配，当前只有测试与导出面审计在引用
//! （`crates/kiana-rpc-print/tests/jsonl_parity.rs`、`tests/export_surface.rs`）。
//!
//! 本模块**只做分帧，不解析 JSON**：任何人都不会因为内容不是 JSON 而报错，行内容原样交给
//! 回调。唯一的错误来源是 [`pump_jsonl`] 底层的 `AsyncRead::read`（返回
//! [`std::io::Result`]）；`JsonlLineReader` 自身的方法不会失败。

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};

/// 一次读取的块大小；与语义无关（Node 的流块大小由 `highWaterMark` 决定）。
const READ_CHUNK: usize = 64 * 1024;

/// `serializeJsonLine(value)`：`JSON.stringify(value)` + 单个 `\n`。
///
/// TS: `jsonl.ts#serializeJsonLine`。数字格式/字符串转义/键序全部走
/// [`kiana_json::js_object::stringify`]（`ARCHITECTURE.md` §4.3）。
///
/// TS 的顶层 `undefined`/函数/symbol 会得到 `"undefined\n"`；[`Value`] 无法表示这些值，
/// 因此 Rust 侧不存在该分支。
pub fn serialize_json_line(value: &Value) -> String {
    let mut line = kiana_json::js_object::stringify(value);
    line.push('\n');
    line
}

/// LF-only 的增量 JSONL 读取器（TS: `attachJsonlLineReader` 的 `buffer` + `StringDecoder`）。
///
/// 每次 `push_*` 返回本次新产生的完整行；`finish` 返回剩余的最后一行（若有）。
#[derive(Debug, Default)]
pub struct JsonlLineReader {
    /// 已解码、尚未遇到 `\n` 的文本。
    buffer: String,
    /// decoder 的 incomplete-character buffer：形状上跨 chunk 序列已收到的前缀。
    pending: Vec<u8>,
    /// decoder 还需要多少个续字节才能完成 `pending` 里的序列（Node `StringDecoder.lastNeed`）。
    missing: usize,
}

/// Node `StringDecoder` 在单个 chunk 末尾保留未完成序列的起点与缺失字节数。
///
/// 逐行对应 `src/string_decoder.cc` 的向后扫描：只按位掩码判断形状，不做范围校验；
/// 遇到 4 个以上续字节、扫到下标 0、或 lead 字节形状无效时保留 0 字节。
///
/// 返回 `(tail_start, missing)`：`tail_start` 之前可以安全解码，`tail_start..` 是应留到
/// 下一 chunk 的前缀，`missing` 是完成该前缀还差的续字节数。`missing == 0` 时
/// `tail_start == data.len()`（即没有任何字节需要保留）。末字节是 ASCII 时走快路径直接
/// 返回，避免常见情况下进入扫描循环。
fn scan_incomplete_tail(data: &[u8]) -> (usize, usize) {
    let len = data.len();
    if len == 0 || data[len - 1] & 0x80 == 0 {
        return (len, 0);
    }
    let mut buffered = 0usize;
    let mut index = len - 1;
    loop {
        buffered += 1;
        if data[index] & 0xC0 == 0x80 {
            if buffered >= 4 || index == 0 {
                return (len, 0);
            }
            index -= 1;
            continue;
        }
        let needed = if data[index] & 0xE0 == 0xC0 {
            2
        } else if data[index] & 0xF0 == 0xE0 {
            3
        } else if data[index] & 0xF8 == 0xF0 {
            4
        } else {
            return (len, 0);
        };
        if buffered >= needed {
            return (len, 0);
        }
        return (index, needed - buffered);
    }
}

impl JsonlLineReader {
    /// 新建空读取器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一段字节（TS: `decoder.write(chunk)` 分支），返回本次产生的完整行。
    ///
    /// 复刻 `StringDecoder::DecodeData` 的两段逻辑：先用新字节补完上一 chunk 留下的
    /// `pending`（遇到非续字节就提前结束该序列），再对剩余数据做一次“尾部是否可能不完整”
    /// 的向后扫描。
    ///
    /// 顺序不可交换：必须先补完旧序列，否则旧 `pending` 会被新字节的形状判断覆盖。第一阶段
    /// 若在新字节里遇到非续字节（`broken`），说明旧序列已确定无效，此时旧前缀立即按替换规则
    /// 落地，而**这个非续字节本身**必须留给第二阶段重新当作候选 lead。
    pub fn push_bytes(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut data = bytes;

        // 阶段 1：补完上一 chunk 留下的未完成序列。
        if self.missing > 0 {
            let limit = data.len().min(self.missing);
            let mut consumed = 0;
            let mut broken = false;
            while consumed < limit {
                if data[consumed] & 0xC0 != 0x80 {
                    // 期望续字节却遇到新字符：结束旧序列，新字节留给后面的扫描。
                    self.missing = 0;
                    self.pending.extend_from_slice(&data[..consumed]);
                    data = &data[consumed..];
                    broken = true;
                    break;
                }
                consumed += 1;
            }
            if !broken {
                let found = data.len().min(self.missing);
                self.pending.extend_from_slice(&data[..found]);
                data = &data[found..];
                self.missing -= found;
            }
            if self.missing == 0 {
                self.flush_pending();
            }
        }

        // 阶段 2：新字节（或阶段 1 剩下的部分）里可能又有“形状上未完成”的尾部。
        // `tail_start` 之前立即解码，之后最多 3 字节留到下一 chunk。
        if !data.is_empty() && self.missing == 0 {
            let (tail_start, missing) = scan_incomplete_tail(data);
            self.decode_all(&data[..tail_start]);
            if tail_start < data.len() {
                self.pending.extend_from_slice(&data[tail_start..]);
                self.missing = missing;
            }
        }

        self.take_lines()
    }

    /// 喂入一段字符串（TS: `typeof chunk === "string"` 分支，直接拼接、不经 decoder）。
    ///
    /// 字符串已经是合法 UTF-8，因此不触碰 `pending`/`missing`：若此前有被保留的字节，它们
    /// 会在这段字符串**之后**才产出（顺序由 Node 的同一分支决定，见模块文档）。
    pub fn push_str(&mut self, text: &str) -> Vec<String> {
        self.buffer.push_str(text);
        self.take_lines()
    }

    /// 输入结束（TS: `onEnd` 的 `decoder.end()`）：按替换规则解码残留字节，返回最后一行。
    ///
    /// 空缓冲不产生行，与 TS 的 `if (buffer.length > 0)` 一致。
    ///
    /// 返回值最多一个元素：每次 `push_*` 都已在返回前取走所有完整行，而 `pending` 里保留的
    /// 只可能是续字节/lead 字节（其中不会有 `\n`），所以 `flush_pending` 不会产生新行。
    /// 调用方可以按“是否为空”判断有没有残余行。
    pub fn finish(&mut self) -> Vec<String> {
        self.flush_pending();
        let mut lines = self.take_lines();
        if !self.buffer.is_empty() {
            let mut line = std::mem::take(&mut self.buffer);
            if line.ends_with('\r') {
                line.pop();
            }
            lines.push(line);
        }
        lines
    }

    /// 解码并清空 `pending`（Node：`MakeString(incomplete, buffered)` / `FlushData`）。
    ///
    /// `push_bytes` 只在 `missing == 0`（序列已完整或已确定无效）时调用；`finish()` 则
    /// 无条件调用——此时残留前缀是最终形态（EOF，不会再有后续字节），交给
    /// [`Self::decode_all`] 按替换规则收尾。
    fn flush_pending(&mut self) {
        if self.pending.is_empty() {
            self.missing = 0;
            return;
        }
        let pending = std::mem::take(&mut self.pending);
        self.missing = 0;
        self.decode_all(&pending);
    }

    /// 按 V8/WHATWG maximal-subpart 规则把一段字节解码进 `buffer`。
    ///
    /// 末尾未完成的前缀在这种情况下是**最终**的（不会再有后续字节），因此只补一个 U+FFFD；
    /// 无效子段每个补一个。
    ///
    /// 循环语义：`str::from_utf8` 一次只报告第一个错误，处理完就继续下一轮；`error_len()`
    /// 返回 `None` 表示“输入以不完整前缀结尾”，此时补一个 U+FFFD 后直接返回（合法的前缀
    /// 已写完）。`valid_up_to` 之前的字节由标准库保证合法，`expect` 因此不会触发。
    fn decode_all(&mut self, mut bytes: &[u8]) {
        loop {
            match std::str::from_utf8(bytes) {
                Ok(text) => {
                    self.buffer.push_str(text);
                    return;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    // `valid_up_to` 之前保证是合法 UTF-8。
                    self.buffer.push_str(
                        std::str::from_utf8(&bytes[..valid]).expect("valid_up_to is valid UTF-8"),
                    );
                    self.buffer.push('\u{FFFD}');
                    match error.error_len() {
                        Some(len) => bytes = &bytes[valid + len..],
                        None => return,
                    }
                }
            }
        }
    }

    /// 取出 `buffer` 里所有以 `\n` 结尾的行（每行剥离一个尾随 `\r`）。
    ///
    /// 这里用 `drain(..=index)` 移走已消费部分，剩余文本留在 `buffer` 等待后续 chunk。
    /// `find('\n')` 找到的是字节位置，但 `\n` 是 ASCII，因此该位置必定是字符边界，切片安全。
    fn take_lines(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        while let Some(index) = self.buffer.find('\n') {
            let mut line = self.buffer[..index].to_string();
            if line.ends_with('\r') {
                line.pop();
            }
            lines.push(line);
            self.buffer.drain(..=index);
        }
        lines
    }
}

/// 从异步 reader 泵出 JSONL 行直到 EOF（TS: `attachJsonlLineReader` 的 data/end 装配）。
///
/// EOF 时调用 [`JsonlLineReader::finish`]，因此无尾随 LF 的最后一行也会回调。
///
/// 每读满一块（`READ_CHUNK`，64 KiB；不足时按实际读取量）就按块内顺序回调。回调是同步的
/// `FnMut`，因此 `on_line` 中的耗时操作会阻塞后续读取——需要背压时由调用方在回调里处理。
/// 读取错误直接以 `std::io::Result` 返回，`finish` 的残余行**不会**再被送出（错误已经
/// 截断了流）。[`JsonlLineReader`] 的状态随函数结束丢弃，调用方若需分块/取消应直接用
/// 读取器。
pub async fn pump_jsonl<R, F>(reader: &mut R, on_line: &mut F) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    F: FnMut(String),
{
    let mut line_reader = JsonlLineReader::new();
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        for line in line_reader.push_bytes(&chunk[..read]) {
            on_line(line);
        }
    }
    for line in line_reader.finish() {
        on_line(line);
    }
    Ok(())
}

/// [`pump_jsonl`] 的 trait-object 适配（spec §2.9 要求暴露的
/// `attach_jsonl_line_reader` 名字）。
///
/// 与 TS 的 `attachJsonlLineReader` 不同：TS 返回 detach 闭包并让流继续运行；Rust 侧把
/// “持续读取”表达为一个 pump 到 EOF 的 future，取消由调用方丢弃/取消 task 实现。
/// 参数是 `&mut dyn FnMut(String)`，因此可以和泛型版本 [`pump_jsonl`] 共享实现；这里的
/// 适配闭包只做一次转发，不持有额外状态。
pub async fn attach_jsonl_line_reader<R>(
    reader: &mut R,
    on_line: &mut dyn FnMut(String),
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut adapter = |line: String| on_line(line);
    pump_jsonl(reader, &mut adapter).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialize_json_line_appends_one_lf() {
        let line = serialize_json_line(&json!({"a": 1}));
        assert_eq!(line, "{\"a\":1}\n");
    }

    #[test]
    fn reader_strips_exactly_one_cr() {
        let mut reader = JsonlLineReader::new();
        assert_eq!(reader.push_str("a\r\r\n"), vec!["a\r".to_string()]);
    }
}
