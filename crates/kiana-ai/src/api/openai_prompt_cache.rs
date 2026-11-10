//! api/openai-prompt-cache.ts。
//!
//! TS: `packages/ai/src/api/openai-prompt-cache.ts`。`prompt_cache_key` 的最大长度按
//! **Unicode 码点**（`Array.from`）而不是 UTF-16 code unit 或字节数计算，因此这里用
//! `chars()` 而**不是** `&s[..64]`（后者会按字节截断并在非边界 panic）。
//!
//! # 这个文件解决什么问题
//! OpenAI 的 completions / responses / Azure responses / Codex 四条适配路径都要把会话 ID
//! 写进 `prompt_cache_key`，服务端对该字段有 64 码点的长度限制。把截断规则集中在这一个
//! 纯函数里，避免四处各写一份而导致字段形状或缓存键不一致。
//!
//! # 返回值的三态
//! 返回值是 `Option<String>`，与 TS 的三态一一对应，调用方必须用 `if let Some(key)` 决定
//! **是否写字段**：
//! - `None`（TS 的 `undefined`）：字段**不出现**；
//! - `Some("")`：字段**出现且为空串**（这两者在 JSON 里不等价）；
//! - `Some(s)`：字段出现，内容为原串或截断后的前缀。
//!
//! 是否调用本函数（以及是否写字段）属于各适配器的业务判断：`openai_completions` 与
//! `openai_responses` 在 `cacheRetention == none` 时不写，`azure_openai_responses` 不看
//! `cacheRetention`——只要 `session_id` 为 `Some`（含空串）就写入，Codex 另有自己的会话
//! 亲和逻辑。本文件只负责截断，不解释这些开关。
//!
//! # 截断语义
//! 只保留前 64 个码点，不补齐、不哈希。因此两个会话 ID 可能共享同一个前缀键，这是 TS
//! 参考实现的既定行为，而不是本移植引入的缺陷；截断发生在字符边界上，任意 Unicode 输入
//! 都不会产生非法 UTF-8。
//!
//! 测试锁定四个边界：`None`、空串、恰好 64 码点、65 个 emoji（4 字节 UTF-8 / 2 个
//! UTF-16 code unit，只算 1 个码点）。

/// `OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH`。
///
/// 即 `prompt_cache_key` 的长度上限，单位是 Unicode 码点，不是字节数，也不是 UTF-16
/// code unit。单独导出是为了让调用方与测试引用同一个常量；修改它会同时改变所有 OpenAI
/// 适配器发送的键长度。
pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

/// `clampOpenAIPromptCacheKey(key)`。
///
/// - `None`（TS 的 `undefined`）→ `None`（字段**不出现**）。
/// - 空串 → `Some("")`（字段**出现**且为空串）。
/// - ≤ 64 码点 → 原样返回。
/// - > 64 码点 → 前 64 个码点（emoji 等代理对按 1 个码点计）。
///
/// # 为什么用 `chars()`
/// Rust 的 `String` 是 UTF-8 字节序列，`&s[..64]` 在 64 不是字符边界时会 panic（安全的
/// `str` 按字节下标切片要么落在字符边界上、结果仍是合法 UTF-8，要么直接 panic；只有绕过
/// 边界检查、用字节直接构造字符串才会产出非法 UTF-8）。`chars().count()` 与
/// `chars().take(64)` 按码点计数，正好对应 TS `Array.from` 的语义，且对任意输入都不会
/// panic。
///
/// # 调用方约定
/// 返回 `Some` 只表示「字段应写入」，不表示「应该写这个字段」：调用方
/// （`openai_completions`、`openai_responses`、`azure_openai_responses`、
/// `openai_codex_responses`）仍要先判断 `cacheRetention` 等开关，再决定是否调用与写入。
///
/// TS: `src/api/openai-prompt-cache.ts#clampOpenAIPromptCacheKey`
pub fn clamp_openai_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let key = key?;
    if key.chars().count() <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH {
        return Some(key.to_string());
    }
    Some(
        key.chars()
            .take(OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undefined_stays_undefined() {
        assert_eq!(clamp_openai_prompt_cache_key(None), None);
    }

    #[test]
    fn empty_string_is_kept() {
        assert_eq!(clamp_openai_prompt_cache_key(Some("")), Some(String::new()));
    }

    #[test]
    fn exactly_max_length_is_unchanged() {
        let key = "a".repeat(64);
        assert_eq!(clamp_openai_prompt_cache_key(Some(&key)), Some(key));
    }

    #[test]
    fn longer_keys_are_truncated_to_64_code_points() {
        let key = "a".repeat(65);
        assert_eq!(
            clamp_openai_prompt_cache_key(Some(&key)),
            Some("a".repeat(64))
        );
    }

    #[test]
    fn truncation_counts_code_points_not_utf16_units() {
        // "😀" 是 4 字节 UTF-8 / 2 个 UTF-16 code unit，但只算 1 个码点。
        let key: String = "😀".repeat(64);
        assert_eq!(clamp_openai_prompt_cache_key(Some(&key)), Some(key.clone()));
        let longer: String = "😀".repeat(65);
        assert_eq!(clamp_openai_prompt_cache_key(Some(&longer)), Some(key));
    }
}
