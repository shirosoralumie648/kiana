//! modes/interactive/model-search.ts。
//!
//! `/model` 与 scoped-models 选择器的模糊搜索文本，以及「模型名是否只是 ID 的另一种写法」
//! 的判定。公式是稳定契约（空白与 Unicode 分词敏感），改动需同步测试。
//!
//! TS: `packages/coding-agent/src/modes/interactive/model-search.ts`
//!
//! # 为什么需要这个模块
//! `/model` 与 scoped-models 选择器都用模糊匹配过滤模型列表，但两者「什么算命中」略有
//! 不同：前者希望以 provider 开头的查询优先，后者把裸 id 放在最前。把搜索文本的拼装与
//! 「名称是否只是 ID 的另一种写法」的判定集中在这里，避免两个选择器各写一份而漂移。
//! 模糊打分本身不在这里，由调用方的 `fuzzy_filter` 完成。
//!
//! # 约束
//! 所有字符串公式是稳定契约（包括重复出现的 provider、空 `name` 不追加空格、JS 空白
//! 集合）。改动会直接改变用户可见的匹配与排序，现有测试锁定这些字符串。分词使用 Unicode
//! 字母/数字通用类别，不能用 ASCII 判断或 `char::is_alphabetic` 代替。
//!
//! # 调用方
//! - `crate::components::model_selector`：列表搜索文本与名称去重显示；
//! - `crate::components::scoped_models_selector`：同上；
//! - `crate::interactive_mode`：`/model <prefix>` 参数补全。

use std::collections::HashSet;
use std::sync::LazyLock;

use kiana_tui::text::trim_js;
use regex::Regex;

/// `splitModelLabel` 的分隔正则：`/[^\p{L}\p{N}]+/u`。
///
/// 用 regex crate 而不是 `char::is_alphabetic`，因为 TS 的 `\p{L}` 是 Unicode General
/// Category Letter，而 `char::is_alphabetic` 是 Derived Core Property Alphabetic
/// （还会包含 Other_Alphabetic 组合记号）。
static MODEL_LABEL_SEPARATOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]+").expect("model label separator regex"));

/// `splitModelLabel(value)`：小写后按非字母/数字字符切分并丢弃空串。
///
/// TS 里是模块私有函数（不导出）；Rust 侧 `pub(crate)` 以便单元测试。
pub(crate) fn split_model_label(value: &str) -> Vec<String> {
    MODEL_LABEL_SEPARATOR
        .split(&value.to_lowercase())
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// `getModelSearchText(item)`。
///
/// ```text
/// `${id} ${provider} ${provider}/${id} ${provider} ${id}${name}`
/// ```
///
/// `name` 非空时前缀一个空格（TS `item.name ? ` ${item.name}` : ""`，空串是 falsy）。
///
/// `provider` 作为独立 token 出现两次，另一次嵌在 `provider/id` 中；这是 TS 公式的一部分，
/// 未去重。改变 token 顺序会改变模糊匹配的分数与排序。供 `/model <prefix>` 补全与
/// scoped-models 过滤使用。
pub fn get_model_search_text(id: &str, provider: &str, name: Option<&str>) -> String {
    let name = match name {
        Some(name) if !name.is_empty() => format!(" {name}"),
        _ => String::new(),
    };
    format!("{id} {provider} {provider}/{id} {provider} {id}{name}")
}

/// `getDistinctModelName(item)`：名字含 ID 没有的词时返回**原 name**（已 trim），否则 `None`。
///
/// ID 与 name 都按 `/[^\p{L}\p{N}]+/u` 小写分词。name 的每个词都被 ID 覆盖（整词命中 ID
/// token，或长度 > 1 且是压缩 ID 的子串）→ `None`；单字符词必须整词命中（`"4"` 不被
/// `"mistral-small-2603"` 覆盖）。长度判断按 JS 的 UTF-16 码元数。
pub fn get_distinct_model_name(id: &str, name: Option<&str>) -> Option<String> {
    let name = trim_js(name?);
    if name.is_empty() {
        return None;
    }
    let id_words = split_model_label(id);
    let id_tokens: HashSet<&str> = id_words.iter().map(String::as_str).collect();
    // 压缩 ID：所有 token 直接拼接，用来吸收名字与 ID 在分隔符上的差异
    // （例如 name 写 "45"、ID 写 "4-5"）。它只对多字符词生效，见下。
    let compact_id: String = id_words.concat();
    let is_covered = |word: &str| {
        // 单字符词必须整词命中 ID 的 token，否则 "4" 会误命中 "2603" 的子串。
        id_tokens.contains(word) || (word.encode_utf16().count() > 1 && compact_id.contains(word))
    };
    if split_model_label(name).iter().all(|word| is_covered(word)) {
        None
    } else {
        Some(name.to_string())
    }
}

/// `getModelSelectorSearchText(item)`。
///
/// 与 [`get_model_search_text`] 的区别：**以 provider 开头**（不把裸 id 放在最前），让
/// `/model` 里的精确 provider 前缀查询排在 `openrouter/openai/gpt-5` 这类代理 ID 之前。
pub fn get_model_selector_search_text(id: &str, provider: &str, name: Option<&str>) -> String {
    let name = match name {
        Some(name) if !name.is_empty() => format!(" {name}"),
        _ => String::new(),
    };
    format!("{provider} {provider}/{id} {provider} {id}{name}")
}

#[cfg(test)]
mod tests {
    // 测试目标：锁定三条公开公式的 TS 逐字输出（搜索文本字符串、分词结果、
    // 名称去重判定），含 Unicode 分词、空 name、单字符词整词命中这些边界。
    // 期望值来自 TS 源公式，改动任何 format! 前先对照 model-search.ts。
    use super::*;

    #[test]
    fn splits_on_non_letters_and_numbers() {
        assert_eq!(
            split_model_label("MiniMax M3 [rasmus@openrouter, parasail]"),
            vec!["minimax", "m3", "rasmus", "openrouter", "parasail"]
        );
        assert_eq!(
            split_model_label("eu.anthropic.claude-sonnet-4-5-20250929-v1:0"),
            vec![
                "eu",
                "anthropic",
                "claude",
                "sonnet",
                "4",
                "5",
                "20250929",
                "v1",
                "0"
            ]
        );
        assert_eq!(split_model_label("---"), Vec::<String>::new());
    }

    #[test]
    fn search_text_matches_ts_formula() {
        assert_eq!(
            get_model_search_text("gpt-5", "openai", Some("GPT-5")),
            "gpt-5 openai openai/gpt-5 openai gpt-5 GPT-5"
        );
        assert_eq!(
            get_model_search_text("gpt-5", "openai", None),
            "gpt-5 openai openai/gpt-5 openai gpt-5"
        );
        assert_eq!(
            get_model_search_text("gpt-5", "openai", Some("")),
            "gpt-5 openai openai/gpt-5 openai gpt-5"
        );
        assert_eq!(
            get_model_selector_search_text("gpt-5", "openai", Some("GPT-5")),
            "openai openai/gpt-5 openai gpt-5 GPT-5"
        );
        assert_eq!(
            get_model_selector_search_text("gpt-5", "openai", None),
            "openai openai/gpt-5 openai gpt-5"
        );
    }

    #[test]
    fn hides_names_that_restate_the_id() {
        assert_eq!(
            get_distinct_model_name("claude-sonnet-4-5", Some("Claude Sonnet 4.5")),
            None
        );
        assert_eq!(
            get_distinct_model_name("openai/gpt-oss-120b", Some("OpenAI: gpt-oss-120b")),
            None
        );
        assert_eq!(
            get_distinct_model_name(
                "eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
                Some("Claude Sonnet 4.5 (EU)")
            ),
            None
        );
        assert_eq!(get_distinct_model_name("gpt-5", Some("")), None);
        assert_eq!(get_distinct_model_name("gpt-5", None), None);
        assert_eq!(get_distinct_model_name("gpt-5", Some("   ")), None);
    }

    #[test]
    fn shows_names_with_words_the_id_lacks() {
        let name = "MiniMax M3 [rasmus@openrouter, parasail]";
        assert_eq!(
            get_distinct_model_name("minimax/minimax-m3", Some(name)),
            Some(name.to_string())
        );
        assert_eq!(
            get_distinct_model_name("gemini-3.1-flash-lite-image", Some("Nano Banana 2 Lite")),
            Some("Nano Banana 2 Lite".to_string())
        );
    }

    #[test]
    fn requires_single_character_words_to_match_a_whole_token() {
        assert_eq!(
            get_distinct_model_name("mistral-small-2603", Some("Mistral Small 4")),
            Some("Mistral Small 4".to_string())
        );
    }
}
