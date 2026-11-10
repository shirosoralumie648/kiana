//! `decode.rs`：WHATWG UTF-8 流式解码，与 Node `new TextDecoder().decode(bytes)` 对整段输入的结果一致。
//! 非法序列按最大子段规则变成 U+FFFD，只有流最开头的字节序标记被丢弃（对应 Durable `env/decode.ts`）。
//!
//! TS 源：packages/env/daemon/src/decode.rs（原 daemon 本身就是 Rust；逐字移植）。
//! WHATWG UTF-8 decoding that matches Node's `new TextDecoder().decode(bytes)` of a whole input when the input arrives
//! in pieces (Durable `env/decode.ts`): invalid sequences become U+FFFD by the maximal-subpart rule, and only a
//! byte-order mark at the very start is dropped.
//!
//! # 为什么需要这个文件
//! 命令输出和文件内容都是字节流，可能分块到达（管道读取、`pread`、`scanLines` 的 64 KiB 分块），
//! 而一个多字节 UTF-8 字符可能恰好跨块。如果每块单独解码，跨块字符会被错误替换成 U+FFFD。
//! `encoding_rs::Decoder` 在两次调用之间保存未完成序列的状态，本文件把这一能力包装成两个入口：
//! 从流开头解码的 `StreamDecoder`（丢弃开头 BOM，供 `exec` 的 stdout/stderr 使用），
//! 以及用于任意字节区间的 `range_decoder`（不做 BOM 处理，供 `scanLines` 统计选中行的解码字节数；
//! 区间不一定从文件开头开始，文件开头的 BOM 由 `LineScanner` 自己跳过）。
//!
//! # 一次解码的流程
//! 1. 调用方创建解码器：`StreamDecoder::new` 或 `range_decoder`；
//! 2. 每收到一块字节就调用 `decode` / `decode_into`，得到本块中已经完整的文本；
//! 3. 最后一块之后再调用一次并传 `last = true`（字节可以为空），冲刷残留的不完整序列。
//!
//! # 注意事项
//! - 同一条流必须按到达顺序喂给同一个解码器，且不能并发调用（`Decoder` 的方法需要 `&mut self`）。
//! - 解码不会失败：非法序列按 WHATWG 的“最大子段”规则变成 U+FFFD，调用方不需要处理错误。
//! - 对 `StreamDecoder`，只有流最开头的 `EF BB BF` 会被丢弃；之后出现的同样字节序列是普通字符 U+FEFF。

use encoding_rs::{Decoder, UTF_8};

/// Decodes one stream chunk by chunk, dropping a leading byte-order mark.
///
/// 这是 `TextDecoder` 增量模式的等价物：不完整的字符留在解码器内部，等后续字节到齐后再输出；
/// 若到 `last = true` 时仍不完整，则按 WHATWG 规则替换成 U+FFFD。
///
/// 不变量：一条字节流自始至终只能使用一个实例，调用顺序必须与字节到达顺序一致；
/// 实例被丢弃而流尚未收尾时，最后一个不完整字符不会出现在任何输出里。
pub struct StreamDecoder {
    decoder: Decoder,
}

impl StreamDecoder {
    /// 创建一个流式解码器：只丢弃开头的 UTF-8 BOM，不做编码嗅探。
    ///
    /// 这里特意用 `new_decoder_with_bom_removal` 而不是 `new_decoder`：后者会嗅探 UTF-16 的
    /// `FF FE` / `FE FF` 开头并切换解码器，而 Node 的 `TextDecoder("utf-8")` 不会；
    /// 测试 `drops_only_a_leading_mark_and_never_sniffs_utf16` 锁定了这个差异。
    pub fn new() -> StreamDecoder {
        // Removes a UTF-8 BOM only; plain `new_decoder()` would also sniff UTF-16 BOMs and switch encodings.
        StreamDecoder {
            decoder: UTF_8.new_decoder_with_bom_removal(),
        }
    }

    /// Text for `bytes`, holding back an incomplete character; with `last`, the end of the stream.
    ///
    /// - `bytes`：按流顺序到达的下一段字节，可以为空；
    /// - `last`：`true` 表示这是流的最后一段，此时未完成的序列会立即变成 U+FFFD；
    /// - 返回：本段中已经完整的字符组成的文本；只有半个字符到达时可能为空串。
    ///
    /// 分多次调用的结果与一次性解码全部字节相同；非法序列只会被替换，不会返回错误。
    pub fn decode(&mut self, bytes: &[u8], last: bool) -> String {
        decode_into(&mut self.decoder, bytes, last)
    }
}

/// A decoder for a byte range that may not start at the beginning of its input: no BOM handling at all.
///
/// `scanLines` 只解码文件中被选中的行区间，该区间不一定从文件开头开始：`startLine` 为 0 时，
/// 文件开头的 BOM 由 `LineScanner` 的 `head`/`feed` 显式跳过，`range_decoder` 负责保留区间中段
/// 出现的 `EF BB BF`。若这里使用带 BOM 移除的解码器，区间中间恰好出现 `EF BB BF` 时会被误当作
/// 标记丢掉，导致解码后的字节数偏小；这里保留它们（解码为 U+FEFF，按 3 字节计数），与整文件
/// `TextDecoder` 视图的字节数一致。
pub fn range_decoder() -> Decoder {
    UTF_8.new_decoder_without_bom_handling()
}

/// 用调用方提供的解码器解码一段字节，返回本段可确定的文本。
///
/// 接收外部 `Decoder` 而不是把状态藏在自己的类型里，是因为 `scanLines` 需要为两个相互独立的
/// 区间（选中行与首行）各自维护一套解码状态，并在区间结束时分别计算解码后的 UTF-8 长度。
///
/// `capacity` 是 `decode_to_string` 实际可用的输出缓冲区大小，必须按最坏情况预留：
/// `max_utf8_buffer_length` 给出正常上限，异常（返回 `None`）时退回 `3 * len + 16`
/// （每个坏字节最多替换成 3 字节的 U+FFFD）。`decode_to_string` 只使用 `String` 已有的备用容量，
/// 不会自动扩容：容量不足时它返回 `OutputFull` 并只消费部分输入。因此这里的
/// `debug_assert_eq!(read, bytes.len())` 是在校验“容量足够、全部输入已被消费”这一前提；
/// release 构建下断言不生效，若容量估算有误，未消费的字节会被静默丢弃。
/// `last` 的含义与 `StreamDecoder::decode` 相同。
pub fn decode_into(decoder: &mut Decoder, bytes: &[u8], last: bool) -> String {
    let capacity = decoder
        .max_utf8_buffer_length(bytes.len())
        .unwrap_or(bytes.len() * 3 + 16);
    let mut text = String::with_capacity(capacity);
    let (_result, read, _replaced) = decoder.decode_to_string(bytes, &mut text, last);
    debug_assert_eq!(read, bytes.len());
    text
}

/// Whether decoding the whole input drops its first three bytes as a byte-order mark.
///
/// 只检查前缀，不扫描也不解码。调用方 `scanLines` 先缓存文件的前 3 个字节，用本函数确认整文件
/// 解码时确实会丢掉这 3 字节，再在统计解码字节数时把它们扣除。
pub fn starts_with_bom(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xef, 0xbb, 0xbf])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_feff_that_does_not_start_the_stream() {
        let mut decoder = StreamDecoder::new();
        let bytes = [0xe2, 0x82, 0xef, 0xbb, 0xbf, 0x61];
        let text: String = bytes
            .iter()
            .map(|byte| decoder.decode(&[*byte], false))
            .collect::<String>()
            + &decoder.decode(&[], true);
        assert_eq!(text, "\u{fffd}\u{feff}a");
    }

    #[test]
    fn drops_only_a_leading_mark_and_never_sniffs_utf16() {
        let mut decoder = StreamDecoder::new();
        assert_eq!(
            decoder.decode(&[0xef, 0xbb], false) + &decoder.decode(&[0xbf, 0x61], true),
            "a"
        );
        let mut decoder = StreamDecoder::new();
        assert_eq!(
            decoder.decode(&[0xff, 0xfe, 0x41, 0x00], true),
            "\u{fffd}\u{fffd}A\u{0}"
        );
    }

    #[test]
    fn replaces_an_incomplete_sequence_once() {
        let mut decoder = StreamDecoder::new();
        assert_eq!(decoder.decode(&[0xe2, 0x82, 0x41], true), "\u{fffd}A");
        let mut decoder = StreamDecoder::new();
        assert_eq!(decoder.decode(&[0xe2, 0x82], true), "\u{fffd}");
    }
}
