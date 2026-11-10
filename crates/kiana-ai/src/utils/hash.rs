//! `utils/hash.ts`——短哈希。
//!
//! TS 的 `shortHash` 是日志/调试用的短标识（不是加密哈希）：两个 32 位种子逐 UTF-16 码元
//! 混入，最后用 `Number.prototype.toString(36)` 输出。复刻要点：
//! - `Math.imul(a, b)` 是 **int32 截断**的乘法，Rust 用 `u32::wrapping_mul` 得到同样的位；
//! - `>>>` 是**逻辑**右移（`u32 >>`），`^` 在位层面等价；
//! - `(x >>> 0).toString(36)` 需要把 `u32` 当 f64 走 [`kiana_json::to_radix_string`]。
//!
//! TS: `utils/hash.ts#shortHash`
//!
//! # 设计原因
//!
//! 这不是通用哈希：没有抗碰撞、随机化或分布均匀的要求，硬性要求是**同一输入在任何平台、
//! 任何进程、任何构建下得到同一字符串**。因此实现刻意使用固定的 32 位 wrapping 运算并复用
//! `kiana_json::to_radix_string`（JS `Number.prototype.toString(36)` 的等价物），而不引入
//! `DefaultHasher` 一类标准库哈希——它们不保证跨版本稳定。
//!
//! 另一个容易忽略的点：TS 遍历的是 UTF-16 **码元**（`charCodeAt`），不是码点。Rust 若用
//! `chars()` 遍历，非 BMP 字符（emoji 等）只会贡献一个标量，哈希值就与 TS 不同。
//!
//! # 使用方与跨版本约束
//!
//! - `api/openai_responses_shared.rs`：把外部 tool call id 归一化成 `fc_<hash>` 等稳定 id；
//! - `api/openai_completions.rs`：id 过长时用哈希前 8 位压缩；
//! - `api/mistral_conversations.rs`：由种子生成 tool call id。
//!
//! 这些 id 会作为消息/tool call 的一部分被持久化；算法一旦改动，跨版本 id 就不再匹配
//! （`spec/ai-api-anthropic-openai.md` §9.1 明确要求逐位复刻）。
//!
//! # 输出形状
//!
//! 结果是两段 base36 **直接拼接**：`h2` 段在前、`h1` 段在后。每段是 1–7 位 base36（无前导零）；
//! `spec/ai-api-anthropic-openai.md` 的规范样例总长为 13/14 个字符，20 万随机样本的最大值为 14，
//! 但长度**没有保证**——本文件测试中的空串与 `"abc"` 实测都是 12 位。调用方按需截取（例如取
//! 前 8 位），但不要把它当成一个数值解析，也不要补零。

/// 快速确定性哈希，把长字符串缩短。
///
/// 输入任意 UTF-8 字符串，输出两段 base36 直接拼接的文本（小写字母 + 数字；实测通常
/// 13–14 个字符，但不保证长度恒定）。纯函数、无副作用；同一输入跨进程、跨平台、跨构建
/// 结果相同。它不是加密哈希，碰撞概率没有设计保证，不要用于 token、校验和或任何安全场景。
///
/// 调用方通常只把结果当作不透明 id 片段：需要短后缀时按 TS 的规则截取（例如前 8 位），
/// 需要全长时直接使用。
///
/// ```
/// # use kiana_ai::utils::hash::short_hash;
/// assert_eq!(short_hash(""), "k4n83c7h0j2b");
/// ```
pub fn short_hash(str: &str) -> String {
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;
    // 第一轮：每个 UTF-16 码元依次混入两个独立状态。`wrapping_mul` 与 `Math.imul` 的
    // 位模式相同（都是 mod 2^32 的乘积）；`^` 在 u32 上与 TS 的 32 位按位异或一致。
    for ch in str.encode_utf16() {
        let ch = u32::from(ch);
        h1 = (h1 ^ ch).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ ch).wrapping_mul(1_597_334_677);
    }
    // 第二轮：交叉混合。第二条 h2 的表达式必须使用**刚更新过的** h1——TS 的赋值顺序就是
    // 这样，换成旧 h1 会得到另一组哈希值（跨版本 id 将不再匹配）。
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    // 输出顺序是 h2 在前、h1 在后；u32 → f64 是精确转换，再按 JS 的 base36 规则格式化。
    format!(
        "{}{}",
        kiana_json::to_radix_string(f64::from(h2), 36),
        kiana_json::to_radix_string(f64::from(h1), 36)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_js_for_empty_and_ascii() {
        // 期望值由 TS 参照实现（node）逐字生成。
        assert_eq!(short_hash(""), "k4n83c7h0j2b");
        assert_eq!(short_hash("a"), "m8735310ae7sx");
        assert_eq!(short_hash("hello world"), "n7rb4n1m39uz8");
        assert_eq!(short_hash("abc"), "y0biex7f9bbh");
        assert_eq!(short_hash("test-string"), "lelc231bivufj");
    }

    #[test]
    fn hashes_utf16_code_units_not_bytes() {
        // 非 BMP 字符按代理对的两个码元参与（与 TS 的 charCodeAt 一致）。
        assert_eq!(short_hash("😀"), "13wj7r7usi372");
        assert_ne!(short_hash("😀"), short_hash("a"));
    }
}
