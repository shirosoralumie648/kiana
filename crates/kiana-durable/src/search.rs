//! `search.ts`：code mode `searchTools()` 与 `tool_search` 工具使用的 BM25 排序。
//!
//! 这是 coding agent 工具搜索的独立副本（durable 只保留自己需要的那部分）。逐字对齐的细节：
//! - 分词先按 camelCase 边界插入空格（两条正则的顺序与贪心行为），再转小写，再按
//!   `[^a-z0-9]+` 切分，丢掉空串与停用词，最后做朴素词干化。
//! - Okapi BM25，`k1 = 1.2`、`b = 0.75`；`log` 是自然对数。
//! - 得分降序；同分保持文档顺序（JS 的 `Array.prototype.sort` 是稳定的）。
//! - 空查询、空文档集或 `limit <= 0` 都返回空列表。
//!
//! # 在 crate 中的位置
//!
//! 仓库内的调用方都在本 crate 内，用法相同：先构造 [`SearchDocument`]（名字 + 可搜索
//! 文本），再调用 [`rank_documents`] 拿回按分数排好的名字。
//!
//! - `crate::tool_search` 的 `tool_search` 工具：结果用于“加载”命中的延迟工具；
//! - `crate::codemode` 的 `searchTools()`：结果转成 JSON 数组返回给脚本。
//!
//! 文档只是临时视图：`name` 与 `text` 都是借用，函数返回的 `Vec<String>` 已经复制好名字，
//! 所以调用返回后调用方可以释放文档文本。
//!
//! # 一个易错点：词干化按字节计长
//!
//! [`tokenize`] 只保留 `[a-z0-9]` 词项（小写化后不属于该字符集的字符都当分隔符），因此
//! UTF-8 字节数、字符数与 JS 的 UTF-16 code unit 数三者对词项相等。私有函数 `stem` 因此
//! 可以安全地用 `term.len()` 判断长度；若把它复用到非 ASCII 输入上，长度判定会与 TS 的
//! `term.length` 不一致。
//!
//! # 与其它副本的关系
//!
//! 仓库里还有一份独立的 BM25/分词实现（`kiana-builtin-extensions` 的 `tool_search`），
//! 两边不共享代码，都源自 TS 的同一份算法。修改分词、停用词或打分公式时，需要分别确认
//! 两处是否都要改，否则 `searchTools()` 与内建 `tool_search` 的排序会分叉。

/// 一篇可搜索文档（TS: `SearchDocument`）。
///
/// 两个字段都是借用，调用方必须保证它们在本次 [`rank_documents`] 调用期间存活。
/// `text` 只参与分词与计分，不会被返回；返回的是 `name` 的副本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchDocument<'a> {
    /// 工具名，作为结果返回。
    ///
    /// 原样复制进结果，不做大小写或其它规范化；调用方之后可以用它找回工具。
    pub name: &'a str,
    /// 参与排序的文本。
    ///
    /// 内容由调用方决定（工具名、描述、名字空间等的拼接）；分词规则见 [`tokenize`]。
    pub text: &'a str,
}

const STOP_WORDS: [&str; 21] = [
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on",
    "or", "that", "the", "this", "to", "with",
];

fn is_stop_word(term: &str) -> bool {
    STOP_WORDS.contains(&term)
}

/// 朴素单数化：`issues` 匹配 `issue`、`searches` 匹配 `search`（TS: `stem`）。
///
/// 只在 [`tokenize`] 产出的 ASCII 词项上调用，所以 `term.len()`（UTF-8 字节）与 TS 的
/// `term.length`（UTF-16 code unit）等价。规则按 TS 的顺序短路，顺序本身有语义：
/// `ies` 先于 `s`；`ches/shes/sses/xes/zes` 先于通用 `s`（例如 `classes` 去掉 `es` 得
/// `class`，若先走通用规则会得到 `classe`）；最后的 `s` 规则排除 `ss` 结尾，避免把
/// `class` 这类词去掉一个字母。
fn stem(term: &str) -> String {
    let len = term.len();
    if len > 4 && term.ends_with("ies") {
        return format!("{}y", &term[..len - 3]);
    }
    if len > 4
        && (term.ends_with("ches")
            || term.ends_with("shes")
            || term.ends_with("sses")
            || term.ends_with("xes")
            || term.ends_with("zes"))
    {
        return term[..len - 2].to_owned();
    }
    if len > 3 && term.ends_with('s') && !term.ends_with("ss") {
        return term[..len - 1].to_owned();
    }
    term.to_owned()
}

/// 第一条正则 `([a-z0-9])([A-Z])` → `"$1 $2"`。
///
/// 逐字符复制，在 ASCII 小写/数字后面紧跟 ASCII 大写时插入空格。插入位置只涉及 ASCII，
/// 所以按 `char` 迭代与 JS 按 UTF-16 code unit 迭代的结果一致；非 ASCII 字符原样复制，
/// 之后是否成为词项取决于 `to_lowercase` 的结果。
fn split_lower_upper(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    for (index, character) in chars.iter().enumerate() {
        out.push(*character);
        if (character.is_ascii_lowercase() || character.is_ascii_digit())
            && chars
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_uppercase())
        {
            out.push(' ');
        }
    }
    out
}

/// 第二条正则 `([A-Z]+)([A-Z][a-z])` → `"$1 $2"`，逐字复刻贪婪回溯的结果：
/// 在连续大写串的最后一个大写字母（其后紧跟小写字母）前插空格。
fn split_upper_run(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index].is_ascii_uppercase() {
            let mut end = index;
            while end < chars.len() && chars[end].is_ascii_uppercase() {
                end += 1;
            }
            // 需要至少两个大写（组 1 一个、组 2 首字母一个），且组 2 第二个字符是小写。
            if end - index >= 2 && chars.get(end).is_some_and(|next| next.is_ascii_lowercase()) {
                for character in &chars[index..end - 1] {
                    out.push(*character);
                }
                out.push(' ');
                out.push(chars[end - 1]);
            } else {
                for character in &chars[index..end] {
                    out.push(*character);
                }
            }
            index = end;
        } else {
            out.push(chars[index]);
            index += 1;
        }
    }
    out
}

/// 小写化、按 camelCase 与非字母数字切分、去停用词、词干化（TS: `tokenize`）。
///
/// 顺序固定为：两条 camelCase 正则 → `to_lowercase` → 只保留小写化后的 ASCII
/// 字母/数字作为词项（其它字符都当分隔符）→ 丢弃空串与停用词 → 词干化。
///
/// 参数可以是任意 `&str`，不会报错；词项只可能由小写化后的 ASCII 字母/数字组成。注意
/// `to_lowercase` 可能把个别非 ASCII 字符映射成 ASCII 字母，这种字符会成为词项的一部分
/// （与 TS 一样，是否保留由小写化结果决定，而不是按“输入是否 ASCII”判断）。
pub fn tokenize(text: &str) -> Vec<String> {
    // 两条正则都做“在已有字符之间插空格”，因此可以先串起来再统一小写、切分。
    let spaced = split_upper_run(&split_lower_upper(text)).to_lowercase();
    let mut terms = Vec::new();
    let mut current = String::new();
    for character in spaced.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            current.push(character);
        } else if !current.is_empty() {
            terms.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }
    terms
        .into_iter()
        .filter(|term| !term.is_empty() && !is_stop_word(term))
        .map(|term| stem(&term))
        .collect()
}

/// Okapi BM25（k1 1.2、b 0.75）：返回最佳匹配的名字，最优在前；同分保持文档顺序。
///
/// # 参数与返回
///
/// - `query`：任意文本；经 [`tokenize`] 后保序去重，作为查询词项。
/// - `documents`：候选文档；返回的是 `name` 的副本，`text` 不被修改。
/// - `limit`：最多返回多少个名字；`0` 返回空列表。类型是 `usize`，调用方在解析 JSON 时
///   已拒绝非正数，因此负数不会到达这里。
///
/// 返回的只有名字，没有分数。查询词项为空、文档集为空或 `limit == 0` 时返回空列表。
///
/// # 不变量
///
/// 计分只累加文档中出现的查询词项，而 idf 对任何词项都严格为正，所以“命中文档”的
/// 得分一定大于 0；`score > 0` 的过滤等价于“至少命中一个查询词项”。同分顺序由
/// `slice::sort_by` 的稳定性保证。
pub fn rank_documents(query: &str, documents: &[SearchDocument<'_>], limit: usize) -> Vec<String> {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;

    // `[...new Set(tokenize(query))]`：保序去重。
    let mut query_terms: Vec<String> = Vec::new();
    for term in tokenize(query) {
        if !query_terms.contains(&term) {
            query_terms.push(term);
        }
    }
    if query_terms.is_empty() || documents.is_empty() || limit == 0 {
        return Vec::new();
    }

    // 每篇文档的词频（Map 的插入顺序不影响后续求和）。
    // Rust 侧用 Vec<(词, 次数)> 线性查找替代 JS Map；候选规模是工具数量。
    let term_counts: Vec<Vec<(String, usize)>> = documents
        .iter()
        .map(|document| {
            let mut counts: Vec<(String, usize)> = Vec::new();
            for term in tokenize(document.text) {
                if let Some(entry) = counts.iter_mut().find(|(known, _)| *known == term) {
                    entry.1 += 1;
                } else {
                    counts.push((term, 1));
                }
            }
            counts
        })
        .collect();
    let lengths: Vec<usize> = term_counts
        .iter()
        .map(|counts| counts.iter().map(|(_, count)| *count).sum())
        .collect();
    let total_length: usize = lengths.iter().sum();
    // 对应 TS 的 `sum / n || 1`：文档集非空但所有文档都是空文本时，避免除以 0。
    let average_length = if total_length == 0 {
        1.0
    } else {
        total_length as f64 / documents.len() as f64
    };
    // idf(term) = ln(1 + (N - df + 0.5) / (df + 0.5))，df 是包含该词项的文档数。
    let idf: Vec<(String, f64)> = query_terms
        .iter()
        .map(|term| {
            let frequency = term_counts
                .iter()
                .filter(|counts| counts.iter().any(|(known, _)| known == term))
                .count();
            let value = (1.0
                + (documents.len() as f64 - frequency as f64 + 0.5) / (frequency as f64 + 0.5))
                .ln();
            (term.clone(), value)
        })
        .collect();

    // 只对文档中实际出现的查询词项累加；count 为 0 的词项跳过，不改变结果。
    let mut matches: Vec<(String, f64)> = Vec::new();
    for (index, document) in documents.iter().enumerate() {
        let mut score = 0.0f64;
        for (term, term_idf) in &idf {
            let count = term_counts[index]
                .iter()
                .find(|(known, _)| known == term)
                .map_or(0, |(_, count)| *count);
            if count == 0 {
                continue;
            }
            let norm = K1 * (1.0 - B + (B * lengths[index] as f64) / average_length);
            score += term_idf * ((count as f64 * (K1 + 1.0)) / (count as f64 + norm));
        }
        // 与 TS 的 `if (score > 0)` 一致：一个词项都没命中的文档不进入结果。
        if score > 0.0 {
            matches.push((document.name.to_owned(), score));
        }
    }

    // 稳定排序：同分保持文档顺序（`slice::sort_by` 是稳定排序）。正常输入下分数不会出现
    // NaN（idf、词频与长度都是有限值），`unwrap_or(Equal)` 只是防御性写法。
    matches.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    matches
        .into_iter()
        .take(limit)
        .map(|(name, _)| name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_splits_camel_case_and_stems() {
        assert_eq!(tokenize("readFiles"), vec!["read", "file"]);
        assert_eq!(tokenize("searchTools"), vec!["search", "tool"]);
        assert_eq!(tokenize("issues"), vec!["issue"]);
        assert_eq!(tokenize("searches"), vec!["search"]);
        assert_eq!(tokenize("HTTPServer"), vec!["http", "server"]);
        assert_eq!(tokenize("the a of"), Vec::<String>::new());
    }

    #[test]
    fn ranks_matching_documents() {
        let documents = [
            SearchDocument {
                name: "read",
                text: "Read a file from the filesystem",
            },
            SearchDocument {
                name: "write",
                text: "Write a file to the filesystem",
            },
            SearchDocument {
                name: "search",
                text: "Search files by name",
            },
        ];
        assert_eq!(
            rank_documents("file", &documents, 10),
            vec!["read", "write", "search"]
        );
        assert_eq!(rank_documents("", &documents, 10), Vec::<String>::new());
        assert_eq!(rank_documents("file", &documents, 0), Vec::<String>::new());
    }
}
