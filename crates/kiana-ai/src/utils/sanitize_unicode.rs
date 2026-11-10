//! `utils/sanitize-unicode.ts`——删除未配对的 UTF-16 代理项。
//!
//! TS 的正则（含 lookbehind）：
//! `/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/g`。
//!
//! **Rust 侧被迫偏差**：`String` 不允许孤立代理项，因此对合法 `&str` 输入本函数是恒等
//! 变换（`ARCHITECTURE.md` §8 第 31 条已声明：孤立代理项在解码处按 U+FFFD 处理）。
//! 为让需要逐码元处理的调用方仍能复刻 TS 的行为，这里同时提供工作在 `&[u16]` 上的
//! [`sanitize_surrogate_units`]。
//!
//! TS: `utils/sanitize-unicode.ts#sanitizeSurrogates`
//!
//! # 为什么需要它
//! 未配对的 UTF-16 代理项是非法文本，部分 provider 会因此拒绝请求
//! （`tests/unicode_surrogate.rs` 记录了 Anthropic 的 `no low surrogate in string`）。
//! TS 侧在把消息文本写进请求 JSON 的路径上会先调用 `sanitizeSurrogates`，所以 Rust 侧
//! 保留同名步骤，让调用点与 TS 一一对应。
//!
//! # 调用方
//! 适配器在把文本块写进请求 JSON 之前调用 [`sanitize_surrogates`]，包括
//! `api/anthropic_messages.rs`（上文「Anthropic 拒绝请求」对应的主要调用方）、
//! `api/bedrock_converse_stream.rs`、`api/google_shared.rs`、`api/mistral_conversations.rs`、
//! `api/openai_completions.rs`、`api/openai_responses_shared.rs` 与 `api/openrouter_images.rs`。
//! 目前生产代码没有调用 [`sanitize_surrogate_units`]，它由本文件的单元测试覆盖，
//! 供需要处理 `&[u16]` 的调用方使用。
//!
//! # 逐码元版本的规则
//! 与 TS 正则等价：
//! - 高代理项（U+D800..=U+DBFF）只有在**紧后**一个码元是低代理项时保留；
//! - 低代理项（U+DC00..=U+DFFF）只有在**紧前**一个码元是高代理项时保留；
//! - 其余一律删除。
//!
//! 每个码元只与原输入中的相邻码元比较，删除不会改变后续判定，因此一次线性扫描即可。

/// 删除未配对的代理项（`&str` 版本）。
///
/// 合法 UTF-8 的 `&str` 中代理项总是成对出现，因此返回原字符串的一份拷贝。
///
/// # 为什么必然恒等
/// UTF-8 无法编码孤立代理项；用 `&str` 能构造出来的代理对只对应 U+10000 以上的字符，
/// 它们本来就是配对的。所以这里没有任何可删除的码元，返回值与输入内容相同。
///
/// # 代价与语义
/// 每次调用都分配一份新的 `String`（即使内容不变），返回类型是拥有所有权的字符串，
/// 以满足调用方把它直接塞进请求 JSON 的需要。函数不做 NFC/NFD 等任何规范化。
pub fn sanitize_surrogates(text: &str) -> String {
    text.to_string()
}

/// 删除未配对的代理项（按 UTF-16 码元版本，逐字复刻 TS 的正则）。
///
/// # 参数与返回
/// - `units`：任意 UTF-16 码元序列，**可以**包含孤立代理项——这正是本函数要清理的输入。
/// - 返回新分配的 `Vec<u16>`：合法配对原样保留且顺序不变，被删除的码元不占位。
///
/// # 为什么不用 `&str`
/// `&str` 表示不了一个孤立代理项，因而无法承载需要清洗的中间状态（例如从 `Vec<u16>` 或
/// WTF-8 解码器拿到的数据）。本函数只做码元级过滤，不负责把结果转回字符串；调用方需要
/// 自己处理编码。
pub fn sanitize_surrogate_units(units: &[u16]) -> Vec<u16> {
    let is_high = |unit: u16| (0xd800..=0xdbff).contains(&unit);
    let is_low = |unit: u16| (0xdc00..=0xdfff).contains(&unit);
    let mut out = Vec::with_capacity(units.len());
    // 邻接判断只看原始输入（`units`），不看已写出的 `out`：先删掉的码元不会改变后续
    // 码元的配对判断，因此一次顺序扫描与 TS 的全局正则替换等价。
    for (index, &unit) in units.iter().enumerate() {
        if is_high(unit) {
            // 高代理项后面必须紧跟低代理项，否则删除。
            if !units.get(index + 1).copied().is_some_and(is_low) {
                continue;
            }
        } else if is_low(unit) {
            // 低代理项前面必须是高代理项，否则删除。
            if index == 0 || !units.get(index - 1).copied().is_some_and(is_high) {
                continue;
            }
        }
        out.push(unit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_paired_surrogates_and_drops_lone_ones() {
        // "Hello 🙈 World" 的码元：🙈 = D83D DE48。
        let units: Vec<u16> = "Hello 🙈 World".encode_utf16().collect();
        assert_eq!(sanitize_surrogate_units(&units), units);
        // 孤立高代理项被删除。
        let mut lone = vec![b'T' as u16, 0xd83d, b'!' as u16];
        assert_eq!(
            sanitize_surrogate_units(&lone),
            vec![b'T' as u16, b'!' as u16]
        );
        // 孤立低代理项被删除。
        lone = vec![0xde48, b'x' as u16];
        assert_eq!(sanitize_surrogate_units(&lone), vec![b'x' as u16]);
        // 两个高代理项相邻：第一个被删，第二个留下（后无低代理项 -> 也删）。
        assert_eq!(
            sanitize_surrogate_units(&[0xd83d, 0xd83d, 0xde48]),
            vec![0xd83d, 0xde48]
        );
    }

    #[test]
    fn str_version_is_identity() {
        assert_eq!(sanitize_surrogates("Hello 🙈 World"), "Hello 🙈 World");
        assert_eq!(sanitize_surrogates(""), "");
    }
}
