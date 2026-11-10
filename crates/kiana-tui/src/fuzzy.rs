//! fuzzy.ts：模糊匹配与过滤。
//!
//! 只要 query 的每个字符按顺序出现在 text 里就算匹配（不必连续）。**分数越低越好**；
//! 不匹配时 `score == 0`。
//!
//! ## 索引单位
//!
//! TS 的 `indexOf`/`length` 是 UTF-16 码元，`positions` 与 `scorePositions` 的 `i * 0.1`、
//! `consecutiveMatches * 5` 权重全部建立在这个单位上。Rust 侧因此把待匹配文本展开成
//! `Vec<u16>` 的 **UTF-16 码元序列**（BMP 之外的字符占两个码元），
//! 保证分数逐位稳定。
//!
//! `toLowerCase()` 用 `str::to_lowercase()`（与 JS 一样可扩展为多个码点）。
//!
//! ## 为什么需要这个模块
//!
//! 设置列表的搜索框（`crate::components::settings_list` 的 `apply_filter`）与斜杠命令补全
//! （`crate::autocomplete`）都用 [`fuzzy_filter`] 做“输入几个字母就能筛出目标”。分数直接
//! 决定列表顺序，因此分数必须逐位稳定：`tests/fuzzy.rs` 与
//! `tests/regression_review_fixes.rs` 用 `assert_eq!` 锁定精确浮点值；改权重前先看这些断言。
//!
//! ## 主要流程
//!
//! 1. [`fuzzy_match`] 先把 query 与 text 都 `to_lowercase()`，再展开成 UTF-16 码元；
//! 2. `match_query` 做一次贪心对齐（每个 query 码元取“上次命中之后第一次出现”），
//!    交给 `score_positions` 打分；
//! 3. 再枚举 text 中每一处**连续**出现的 query，用同一套权重重算并取最小值（修复贪心
//!    对齐把 `opus` 拆进 `openrouter` 的问题，见 `tests/fuzzy.rs` 的 #11220 用例）；
//! 4. 整串相等额外 `-100`；
//! 5. 主查询完全不命中时，才尝试 ASCII“字母 + 数字”交换（`codex52` ↔ `52codex`），命中则
//!    在交换后的分数上再加 `5.0`。
//!
//! ## 注意事项
//!
//! - 分数**可以为正数**（测试里 `fuzzy_match("opus", "…claude-sonnet…")` 是 41.6）；
//!   “越低越好”不等于“命中就是负数”。
//! - 空 query 恒命中、分数 `0.0`；[`fuzzy_filter`] 不会把空 token 传给它。
//! - 位置、索引与权重全部按 UTF-16 码元计算。改成 `Vec<char>` 会让非 BMP 字符的位置惩罚
//!   与 TS 不同（代理码元各占一个位置）。

use crate::text::{is_js_whitespace, trim_js};

/// 一次模糊匹配的结果。分数越低越好；不匹配时 `score == 0`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FuzzyMatch {
    /// 是否按顺序命中；空 query 视为命中。
    pub matches: bool,
    /// 匹配质量：**越低越好**。不命中时恒为 `0.0`，命中时也可能为正数。
    pub score: f64,
}

/// 把字符串展开成 UTF-16 码元序列。
///
/// 与 TS 的 `string[i]` / `length` / `indexOf` 完全同构：BMP 之外的字符占两个码元，
/// 高位与低位代理各自可区分（`"🙂"` 与 `"😀"` 的高位代理相同、低位不同，不会互相误配）。
fn to_utf16_units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// 码元是否落在 `/[[\s\-_./:]]/`（`\s` 用 JS 空白集合）。代理码元永远不命中。
fn is_boundary_unit(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(is_boundary_char)
}

/// `scorePositions(textLower, positions)`。
///
/// `last_match_index` 与 TS 一样从 `-1` 起步：因此 `i == 0` 时 `last_match_index == i - 1`
/// 成立，首个命中也拿到"连续命中"加分（TS 的实际行为，见 `fuzzyMatch("test","test") == -159.4`）。
///
/// 要求 `positions` 的每个元素都小于 `text_lower.len()`（调用方的位置都来自扫描结果，满足
/// 该条件）：循环会读 `text_lower[position - 1]`，越界会 panic。`position == 0` 由短路条件
/// 保证不会做这次索引。
fn score_positions(text_lower: &[u16], positions: &[usize]) -> f64 {
    let mut score = 0.0f64;
    let mut last_match_index: i64 = -1;
    let mut consecutive_matches = 0i64;

    for &position in positions {
        let i = position as i64;
        // 连续命中加分
        if last_match_index == i - 1 {
            consecutive_matches += 1;
            score -= (consecutive_matches * 5) as f64;
        } else {
            consecutive_matches = 0;
            // 间隔扣分
            if last_match_index >= 0 {
                score += ((i - last_match_index - 1) * 2) as f64;
            }
        }

        // 词边界奖励
        if position == 0 || is_boundary_unit(text_lower[position - 1]) {
            score -= 10.0;
        }

        // 越靠后越差
        score += i as f64 * 0.1;

        last_match_index = i;
    }

    score
}

/// `/[\s\-_./:]/` 的单字符测试（`\s` 用 JS 空白集合）。
fn is_boundary_char(ch: char) -> bool {
    is_js_whitespace(ch) || matches!(ch, '-' | '_' | '.' | '/' | ':')
}

/// `matchQuery(normalizedQuery, textLower)`；两者都已经是 UTF-16 码元序列。
///
/// 大小写归一由调用方完成。空 query 返回命中、分数 0；query 比 text 长，或贪心对齐时
/// 找不到某个码元，返回不命中、分数 0。
fn match_query(normalized_query: &[u16], text_lower: &[u16]) -> FuzzyMatch {
    if normalized_query.is_empty() {
        return FuzzyMatch {
            matches: true,
            score: 0.0,
        };
    }
    if normalized_query.len() > text_lower.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    // 贪心对齐：每个 query 码元取上一次命中之后的第一次出现
    let mut positions: Vec<usize> = Vec::with_capacity(normalized_query.len());
    let mut last_match_index: i64 = -1;
    for &query_char in normalized_query {
        let found = ((last_match_index + 1) as usize..text_lower.len())
            .find(|&index| text_lower[index] == query_char);
        let Some(index) = found else {
            return FuzzyMatch {
                matches: false,
                score: 0.0,
            };
        };
        positions.push(index);
        last_match_index = index as i64;
    }

    let mut score = score_positions(text_lower, &positions);

    // 贪心对齐可能把 query 摊到前面不相关的位置（"opus" 在
    // "openrouter openrouter/anthropic/claude-opus" 里从 "openrouter" 取到 o、p、u）。
    // 因此还要给每一处连续出现打分，取最优。
    // 循环会把 positions 原地改写成当前子串的下标；分数只取 min，循环结束后 positions 的
    // 最终值不再使用。
    let mut start = find_subslice(text_lower, normalized_query, 0);
    while let Some(start_index) = start {
        for (k, position) in positions.iter_mut().enumerate() {
            *position = start_index + k;
        }
        score = score.min(score_positions(text_lower, &positions));
        start = find_subslice(text_lower, normalized_query, start_index + 1);
    }

    // 整串相等（两侧都已小写）再减 100，保证完全相等的项排在最前；
    // 这个分支只可能出现在 text 与 query 码元序列完全一致时。
    if normalized_query == text_lower {
        score -= 100.0;
    }

    FuzzyMatch {
        matches: true,
        score,
    }
}

/// `textLower.indexOf(normalizedQuery, from)`（码元序列版）。
///
/// 空 needle 的分支与 JS `indexOf` 有一处细微差异：`from > len` 时这里返回 `None`，而 JS
/// 会把位置夹到 `len` 再返回。该分支只为完备性保留——`match_query` 已对空 query 提前返回，
/// 实际不会走到。
fn find_subslice(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return if from <= haystack.len() {
            Some(from)
        } else {
            None
        };
    }
    if needle.len() > haystack.len() {
        return None;
    }
    let last_start = haystack.len() - needle.len();
    let mut index = from;
    while index <= last_start {
        if haystack[index..index + needle.len()] == *needle {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// `fuzzyMatch(query, text)`。
///
/// 大小写不敏感（两侧先 `to_lowercase`）。主查询不命中时再尝试 ASCII 字母/数字交换后的
/// query；交换形式命中后在它自己的分数上加 `5.0` 惩罚。两者都不命中时返回
/// `matches = false`、`score = 0.0`。空 query 恒命中、分数 `0.0`。
pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch {
    let query_lower = query.to_lowercase();
    let text_lower_string = text.to_lowercase();
    let query_units = to_utf16_units(&query_lower);
    let text_units = to_utf16_units(&text_lower_string);

    let primary_match = match_query(&query_units, &text_units);
    if primary_match.matches {
        return primary_match;
    }

    // 交换是主查询失败后的兜底，而不是在两种结果里择优：只要主查询命中就直接返回。
    let swapped_query = swap_alpha_numeric(&query_lower);
    let Some(swapped) = swapped_query else {
        return primary_match;
    };

    let swapped_units = to_utf16_units(&swapped);
    let swapped_match = match_query(&swapped_units, &text_units);
    if !swapped_match.matches {
        return primary_match;
    }

    FuzzyMatch {
        matches: true,
        score: swapped_match.score + 5.0,
    }
}

/// `^(?<letters>[a-z]+)(?<digits>[0-9]+)$` / `^(?<digits>[0-9]+)(?<letters>[a-z]+)$` 的交换。
///
/// TS 先试 `letters+digits`，失败再试 `digits+letters`（两个独立的正则）；两条都可能命中时
/// **前者优先**（`alphaNumericMatch ? … : numericAlphaMatch ? … : ""`）。这里保持同样的顺序。
///
/// 只接受纯 ASCII 的 `[a-z]+[0-9]+` / `[0-9]+[a-z]+`：含非 ASCII 字节，或字母与数字交错
/// （`a1b`）都返回 `None`。扫描按字节进行，但两个分支的切分点都落在“前缀全为 ASCII”的位置，
/// 不会拆开多字节字符。
fn swap_alpha_numeric(query_lower: &str) -> Option<String> {
    let is_ascii_lower = |b: u8| b.is_ascii_lowercase();
    let is_ascii_digit = |b: u8| b.is_ascii_digit();
    let bytes = query_lower.as_bytes();

    // letters + digits
    let letters_end = bytes
        .iter()
        .position(|&b| !is_ascii_lower(b))
        .unwrap_or(bytes.len());
    if letters_end > 0
        && letters_end < bytes.len()
        && bytes[letters_end..].iter().all(|&b| is_ascii_digit(b))
    {
        return Some(format!(
            "{}{}",
            &query_lower[letters_end..],
            &query_lower[..letters_end]
        ));
    }

    // digits + letters
    let digits_end = bytes
        .iter()
        .position(|&b| !is_ascii_digit(b))
        .unwrap_or(bytes.len());
    if digits_end > 0
        && digits_end < bytes.len()
        && bytes[digits_end..].iter().all(|&b| is_ascii_lower(b))
    {
        return Some(format!(
            "{}{}",
            &query_lower[digits_end..],
            &query_lower[..digits_end]
        ));
    }

    None
}

/// `fuzzyFilter(items, query, getText)`：空白或 `/` 分隔的 token 必须全部命中，按总分升序稳定排序。
///
/// query 先 `trim()`；空（或全空白）时**原样返回**（顺序不变、不过滤不排序）。
///
/// 非空 query 按 JS 空白与 `/` 切成 token；任一 token 不命中即淘汰该 item。所有 token 的分数
/// 相加，结果按总分**升序稳定排序**（同分保持输入顺序，与 V8 的 `Array.prototype.sort` 一致）。
///
/// `get_text` 对每个 item 至多调用一次（同一 item 的多个 token 复用同一份文本），返回值不需要
/// 做大小写归一——[`fuzzy_match`] 会在内部处理。只含分隔符的 query（例如 `"/"`）分词后没有
/// token，同样原样返回。
pub fn fuzzy_filter<T>(items: Vec<T>, query: &str, get_text: impl Fn(&T) -> String) -> Vec<T> {
    if trim_js(query).is_empty() {
        return items;
    }

    let trimmed = trim_js(query);
    // 连续分隔符会产生空串，必须过滤；否则空 token 会被 fuzzy_match 当作“恒命中”。
    let tokens: Vec<&str> = trimmed
        .split(|ch: char| is_js_whitespace(ch) || ch == '/')
        .filter(|token| !token.is_empty())
        .collect();
    if tokens.is_empty() {
        return items;
    }

    let mut results: Vec<(T, f64)> = Vec::new();
    for item in items {
        // 每个 item 只取一次文本：get_text 可能较贵（例如克隆 label），多个 token 复用它。
        let text = get_text(&item);
        let mut total_score = 0.0f64;
        let mut all_match = true;
        for token in &tokens {
            let m = fuzzy_match(token, &text);
            if m.matches {
                total_score += m.score;
            } else {
                all_match = false;
                break;
            }
        }
        if all_match {
            results.push((item, total_score));
        }
    }

    // V8 的 `Array.prototype.sort` 是稳定排序；`sort_by` 同样是稳定排序，且只比总分。
    results.sort_by(|a, b| a.1.total_cmp(&b.1));
    results.into_iter().map(|(item, _)| item).collect()
}
