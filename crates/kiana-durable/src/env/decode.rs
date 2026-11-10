//! `env/decode.ts`：分块解码，结果与一次性解码整段字节相同。
//!
//! Node 的流式解码器在开启 BOM 处理时可能把**块边界之后**的 U+FEFF 也当作字节序标记丢弃，
//! 而不只是流首的那一个。因此这里的解码器关闭 BOM 处理，并由调用方自己丢弃流首标记：
//! - [`range_decoder`] 对应 `new TextDecoder("utf-8", { ignoreBOM: true })`；
//! - [`starts_with_bom`] 判断前三个字节是否是 UTF-8 BOM；
//! - [`StreamDecoder`] 逐块解码，只在流首文本非空时丢弃一个前导 U+FEFF。
//!
//! # 为什么需要
//! 文件内容与命令输出都是字节流，而且按块到达（`scanLines` 的 64 KiB 块、管道读取、远程
//! 实现的分块读取）；一个多字节 UTF-8 字符可能恰好跨块。`encoding_rs::Decoder` 在两次调用
//! 之间保存未完成序列，把各块按顺序交给同一个解码器，得到的文本就与一次性解码整段字节相同。
//! 如果每块各自解码，跨块字符会被替换成 U+FFFD，解码后的字节数也会随之改变。
//!
//! # 三个入口的分工
//! - [`range_decoder`] / [`RangeDecoder`]：不做 BOM 处理（`ignoreBOM: true`），用于任意字节
//!   区间。区间可能不从文件开头开始，因此不能丢弃其中出现的 `EF BB BF`；文件开头的 BOM 由
//!   调用方按绝对偏移跳过（`LineScanner` 的 `feed`、`tools/read.rs` 的 `read_head`）。
//! - [`decode_whole`]：一次性解码整份输入，再丢弃开头的一个 U+FEFF，对应
//!   `new TextDecoder().decode(bytes)` 的默认 BOM 处理。
//! - [`StreamDecoder`]：从流开头分块解码，只丢弃第一段非空输出开头的一个 U+FEFF。
//!
//! # 调用方
//! - `env/node.rs`：文本行读取器的 `decoder` 字段，以及 `exec` 的 stdout/stderr 两条流；
//! - `tools/read.rs`：`read_head` 用 [`range_decoder`] 增量解码一个字节区间；
//! - `testing/env_conformance.rs`：用 [`decode_whole`] / [`range_decoder`] 复现 TS 测试里
//!   `TextDecoder` 的行为；
//! - `kiana-env-client` 的远程实现复用 [`RangeDecoder`] / [`StreamDecoder`]。
//!
//! # 注意事项
//! - 一个解码器实例只能服务一条按顺序到达的字节流。方法需要 `&mut self`，同一个实例不会被
//!   并发调用；不同流必须各用一个实例（同一实例交错喂两条流会得到错误文本）。
//! - 解码不会失败：非法序列按 WHATWG 规则变成 U+FFFD，调用方不需要处理错误。
//! - 流结束要调用一次 `decode(None)` 冲刷；直接丢弃解码器时，最后一段不完整序列不会出现在
//!   任何输出里。

use encoding_rs::{Decoder, UTF_8};

/// `rangeDecoder()`：`new TextDecoder("utf-8", { ignoreBOM: true })`。
pub fn range_decoder() -> RangeDecoder {
    RangeDecoder::new()
}

/// 一个流的 UTF-8 解码器：`ignoreBOM: true`，即不丢弃任何 BOM。
///
/// [`RangeDecoder::decode`] 的 `None` 表示流结束（对应 `decoder.decode()`），`Some(bytes)`
/// 表示继续流式解码（对应 `decoder.decode(bytes, { stream: true })`）。
///
/// “不做 BOM 处理”意味着输入里任何位置（包括最开头）的 `EF BB BF` 都会解码成 U+FEFF，并计入
/// 返回文本的 UTF-8 长度。需要“整份文件视图”（开头 BOM 不计）时，调用方要么用
/// [`starts_with_bom`] 判断后跳过那三个字节，要么改用 [`decode_whole`]。
pub struct RangeDecoder {
    decoder: Decoder,
}

impl std::fmt::Debug for RangeDecoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RangeDecoder")
            .finish_non_exhaustive()
    }
}

impl Default for RangeDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl RangeDecoder {
    /// `new TextDecoder("utf-8", { ignoreBOM: true })`。
    ///
    /// 只按 UTF-8 解码，且不做 BOM 处理：开头的 `EF BB BF` 也保留为 U+FEFF。
    pub fn new() -> Self {
        RangeDecoder {
            decoder: UTF_8.new_decoder_without_bom_handling(),
        }
    }

    /// 解码一块；`None` 冲刷流结束。
    ///
    /// - `Some(bytes)`：按流顺序解码下一块。跨块不完整的多字节字符留在解码器内部，本次只返回
    ///   已经完整的文本，因此返回值可能为空串；
    /// - `None`：流结束，把残留的不完整序列替换成 U+FFFD 后返回。
    ///
    /// 解码不返回错误。同一个实例可被多次调用，但必须保持块的顺序。
    pub fn decode(&mut self, bytes: Option<&[u8]>) -> String {
        let mut output = String::new();
        let last = bytes.is_none();
        let input = bytes.unwrap_or(&[]);
        let mut offset = 0usize;
        loop {
            // `decode_to_string` 只写入 `String` 的**剩余容量**；空串没有容量，必须先预留。
            // 按剩余输入最多膨胀 3 倍（一个坏字节变成 3 字节的 U+FFFD）再加 8 字节余量。
            output.reserve(input.len().saturating_sub(offset) * 3 + 8);
            let (result, read, _replaced) =
                self.decoder
                    .decode_to_string(&input[offset..], &mut output, last);
            // `read` 是本次已消费的输入字节数；按它推进偏移，避免重复解码同一段。
            offset += read;
            // `OutputFull` 表示预留容量写满了、输入可能还没消费完：扩大容量后从剩余输入继续。
            // 另一个取值 `InputEmpty` 表示输入已消费完（`last` 时转换也已完成），可以退出。
            if result != encoding_rs::CoderResult::OutputFull {
                break;
            }
        }
        output
    }

    /// `decoder.decode(bytes, { stream: true })` 的显式形式。
    pub fn decode_stream(&mut self, bytes: &[u8]) -> String {
        self.decode(Some(bytes))
    }

    /// `decoder.decode()`（流结束）的显式形式。
    pub fn decode_end(&mut self) -> String {
        self.decode(None)
    }
}

/// 一次性解码整个字节数组，等价于 `new TextDecoder().decode(bytes)`（丢弃流首 BOM）。
///
/// 实现上先喂入全部字节，再用 `decode(None)` 冲刷残留，最后丢掉解码文本开头的一个 U+FEFF。
/// U+FEFF 在 UTF-8 中只能由 `EF BB BF` 产生，所以“丢掉文本开头的 U+FEFF”与“丢掉输入开头的
/// BOM”等价；第二个连续的 BOM 会被保留（同 `TextDecoder` 的默认行为）。
pub fn decode_whole(bytes: &[u8]) -> String {
    let mut decoder = RangeDecoder::new();
    let mut text = decoder.decode(Some(bytes));
    text.push_str(&decoder.decode(None));
    text.strip_prefix('\u{FEFF}')
        .map(str::to_owned)
        .unwrap_or(text)
}

/// 解码整个输入时是否会丢弃它的前三个字节（UTF-8 字节序标记）。
///
/// 只做前缀比较，不解码也不扫描；输入不足 3 字节时返回 `false`。返回值只对“从这 3 个字节
/// 开始的整份输入”有意义：`RangeDecoder` 解码区间中段出现的同样三字节时不会丢弃它们。
pub fn starts_with_bom(first_bytes: &[u8]) -> bool {
    first_bytes.len() >= 3
        && first_bytes[0] == 0xef
        && first_bytes[1] == 0xbb
        && first_bytes[2] == 0xbf
}

/// 逐块解码一个流，结果与一次性解码全部字节完全相同。
///
/// 与 [`RangeDecoder`] 的唯一区别是它假定输入从流开头开始，并丢弃流首的 BOM。判断是**在解码
/// 文本上**做的：只有第一段非空输出开头的一个 U+FEFF 被去掉，之后（包括第二个连续的 BOM）
/// 都保留。如果前面的块只包含半个字符，输出为空，`started` 不会置位，BOM 会等到能完整解码时
/// 再被丢弃——所以分块方式不影响结果（见测试 `stream_decoder_keeps_non_leading_bom`）。
///
/// 不要把本类型用于从文件中途开始的区间：那里开头的 U+FEFF 是普通字符，不应被丢弃，应改用
/// [`RangeDecoder`]。
#[derive(Debug, Default)]
pub struct StreamDecoder {
    decoder: RangeDecoder,
    started: bool,
}

impl StreamDecoder {
    /// 新解码器。
    pub fn new() -> Self {
        StreamDecoder {
            decoder: RangeDecoder::new(),
            started: false,
        }
    }

    /// `bytes` 对应的文本（暂存不完整字符）；`None` 表示流结束。
    ///
    /// 除了一次性的流首 BOM 丢弃外，语义与 [`RangeDecoder::decode`] 相同。按 TS 的用法，
    /// `None` 是终止操作；之后不要再用同一实例解码新数据。
    pub fn decode(&mut self, bytes: Option<&[u8]>) -> String {
        let text = self.decoder.decode(bytes);
        if self.started || text.is_empty() {
            return text;
        }
        self.started = true;
        // U+FEFF 只编码为 EF BB BF，所以前导 U+FEFF 恰好就是流首的字节序标记。
        text.strip_prefix('\u{FEFF}')
            .map(str::to_owned)
            .unwrap_or(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_bom_checks_three_bytes() {
        assert!(starts_with_bom(&[0xef, 0xbb, 0xbf, 0x61]));
        assert!(!starts_with_bom(&[0xef, 0xbb]));
        assert!(!starts_with_bom(&[0x61, 0xef, 0xbb, 0xbf]));
    }

    #[test]
    fn stream_decoder_keeps_non_leading_bom() {
        let bytes = [0xe2, 0x82, 0xef, 0xbb, 0xbf, 0x61];
        let mut decoder = StreamDecoder::new();
        let mut text = String::new();
        for byte in bytes {
            text.push_str(&decoder.decode(Some(&[byte])));
        }
        text.push_str(&decoder.decode(None));
        assert_eq!(text, "\u{fffd}\u{feff}a");
        assert_eq!(text, decode_whole(&bytes));
    }

    #[test]
    fn drops_a_leading_bom_once() {
        let mut decoder = StreamDecoder::new();
        let mut text = decoder.decode(Some(&[0xef, 0xbb, 0xbf, 0x61]));
        text.push_str(&decoder.decode(Some(&[0xef, 0xbb, 0xbf])));
        text.push_str(&decoder.decode(None));
        assert_eq!(text, "a\u{feff}");
    }
}
