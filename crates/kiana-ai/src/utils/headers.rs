//! `utils/headers.ts`——header 合并。
//!
//! 两个函数：
//! - `headersToRecord(headers)`：按 `headers.entries()` 顺序写入普通对象（后写覆盖先写）；
//! - `providerHeadersToRecord(...sources)`：用 `Map<小写名, [原始名, 值]>` 合并，`null` 表示
//!   **删除**该头；结果为空的 Map → `None`。删除再插入会移到末尾（Map 插入序），因此输出
//!   键序与最后一次设置的顺序一致——这正是 `IndexMap` 的 `shift_remove` + `insert` 语义。
//!
//! 与 `models.ts` 的 `merge_headers` 差异：后者保留 `null`（表示抑制），前者丢弃。
//!
//! TS: `utils/headers.ts`
//!
//! # 两个函数处理的是不同阶段的数据
//!
//! - [`headers_to_record`] 用于**响应**头：把 `ProviderResponse.headers` 复制成独立的普通
//!   键值表，交给 `on_response` 回调、日志或测试断言。输入已经是 `IndexMap`，因此这里只做
//!   拷贝，不做小写化、去重或排序。
//! - [`provider_headers_to_record`] 用于**请求**头：把多个来源（provider auth、模型头、
//!   请求选项……）按优先级合并成一张最终表。它必须处理“删除”和“大小写不敏感覆盖”，
//!   输入、输出与 `None` 语义都与前者不同。
//!
//! # 被谁调用
//!
//! - `headers_to_record`：各 `api/*` 适配器在构造 `ProviderResponse` 时使用
//!   （如 `api/anthropic_messages.rs`、`api/openai_completions.rs`、`api/llama_cpp_classify.rs`）。
//! - `provider_headers_to_record`：`api/kiana_messages.rs`、`api/bedrock_converse_stream.rs`、
//!   `api/google_shared.rs` 以及 `api/classifier_shared.rs` 的 `merge_header_sources`。
//!
//! # 顺序保证
//!
//! 输出都是 `IndexMap`，键序就是最终遍历/发送顺序。合并时“删除后重新插入”会把键移到末尾，
//! 与 JS `Map.delete` + `Map.set` 的可观察行为一致；测试 `keys_keep_original_case` 与
//! `headers_to_record_copies_in_order` 锁定了大小写与顺序。
//!
//! # 注意 `None` 的两种含义
//!
//! [`ProviderHeaders`] 的值类型是 `Option<String>`。在 `models::merge_headers` 等位置，
//! `None` 表示“抑制适配器默认添加的同名头”；而 [`provider_headers_to_record`] 把它解释为
//! “从合并结果中删除该键”。两者服务于不同阶段，混用会得到少发/多发的头。

use crate::types::ProviderHeaders;

/// `headersToRecord(headers)`：`Headers` → `Record<string, string>`。
///
/// TS 的 `Headers.entries()` 名字已是小写并保持插入序；Rust 侧用
/// [`crate::types::ProviderResponse::headers`] 的 `IndexMap` 直接构造。
///
/// 参数是响应头表的借用，返回**独立副本**（逐项 `clone`，复杂度 O(n)）：调用方之后修改
/// 输入表不会影响已交给回调或日志的响应对象。`IndexMap` 不存在重复键，因此无需处理
/// “同名合并”；同名头在到达这里之前已由 HTTP 客户端按各自的规则合并（例如 reqwest 实现
/// 会把多值合并成 `"a, b"`，`set-cookie` 取最后一个）。
///
/// TS: `utils/headers.ts#headersToRecord`
pub fn headers_to_record(
    headers: &indexmap::IndexMap<String, String>,
) -> indexmap::IndexMap<String, String> {
    let mut result = indexmap::IndexMap::with_capacity(headers.len());
    for (key, value) in headers {
        result.insert(key.clone(), value.clone());
    }
    result
}

/// `providerHeadersToRecord(...sources)`：按小写名合并，`null` 删除，空结果 → `None`。
///
/// 返回的键是**原始大小写**（最后一次设置的名字），顺序为最后一次设置的顺序。
///
/// # 参数与合并规则
///
/// `sources` 按**从低到高**的优先级排列（越靠后越优先），`None` 源直接跳过。对每个源中
/// 的每个条目：
/// 1. 用 `to_lowercase()` 得到比较键（HTTP 头名大小写不敏感）；
/// 2. 先 `shift_remove` 掉之前同名的条目——即使本次值是 `None`，也要把旧值删掉；
/// 3. 值为 `Some` 时用**本次的原始大小写**重新插入，位置在 Map 末尾。
///
/// 因此“先设置、后删除”会让该键彻底消失；“删除、再设置”会让它排到最后。全部源都为空、
/// 或所有键最终都被删除时返回 `None`（TS 的 `undefined`），调用方通常用
/// `unwrap_or_default()` 得到空 map（见 `api/classifier_shared.rs` 的 `merge_header_sources`）。
///
/// # 与 `ProviderHeaders` 其它用途的区别
///
/// [`ProviderHeaders`] 的 `None` 在请求流程的其它位置常表示“抑制适配器默认添加的同名头”
/// （见 `types.rs` 的类型文档与 `models::merge_headers`）；本函数把它解释为“从合并结果中
/// 删除该键”。本函数**不**保留 `None` 条目，所以输出类型是 `IndexMap<String, String>`。
///
/// TS: `utils/headers.ts#providerHeadersToRecord`
pub fn provider_headers_to_record(
    sources: &[Option<&ProviderHeaders>],
) -> Option<indexmap::IndexMap<String, String>> {
    // 小写名 -> (原始名, 值)；IndexMap 的 shift_remove + insert 复刻 JS Map 的「删除后重插到末尾」。
    let mut merged: indexmap::IndexMap<String, (String, String)> = indexmap::IndexMap::new();
    for source in sources.iter().flatten() {
        for (name, value) in source.iter() {
            let normalized = name.to_lowercase();
            merged.shift_remove(&normalized);
            if let Some(value) = value {
                merged.insert(normalized, (name.clone(), value.clone()));
            }
        }
    }
    if merged.is_empty() {
        return None;
    }
    Some(merged.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, Option<&str>)]) -> ProviderHeaders {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.map(str::to_string)))
            .collect()
    }

    #[test]
    fn merges_and_deletes_null_headers() {
        let a = headers(&[("Authorization", Some("Bearer x")), ("X-Drop", Some("1"))]);
        let b = headers(&[("authorization", Some("Bearer y")), ("X-Drop", None)]);
        let merged = provider_headers_to_record(&[Some(&a), Some(&b)]).expect("merged");
        assert_eq!(
            merged.get("authorization").map(String::as_str),
            Some("Bearer y")
        );
        assert!(!merged.contains_key("x-drop"));
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn empty_sources_yield_none() {
        assert!(provider_headers_to_record(&[]).is_none());
        assert!(provider_headers_to_record(&[None, Some(&ProviderHeaders::new())]).is_none());
        let deleted = headers(&[("a", None)]);
        assert!(provider_headers_to_record(&[Some(&deleted)]).is_none());
    }

    #[test]
    fn keys_keep_original_case() {
        let a = headers(&[("X-Custom-Header", Some("1"))]);
        let merged = provider_headers_to_record(&[Some(&a)]).expect("merged");
        assert_eq!(
            merged.keys().next().map(String::as_str),
            Some("X-Custom-Header")
        );
    }

    #[test]
    fn headers_to_record_copies_in_order() {
        let mut source = indexmap::IndexMap::new();
        source.insert("content-type".to_string(), "text/plain".to_string());
        source.insert("x-b".to_string(), "2".to_string());
        let record = headers_to_record(&source);
        assert_eq!(
            record.keys().cloned().collect::<Vec<_>>(),
            vec!["content-type", "x-b"]
        );
    }
}
