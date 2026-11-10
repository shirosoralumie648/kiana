//! `test/bedrock-custom-headers.test.ts` 的移植。
//!
//! TS 断言 build-step 中间件的注册与注入；Rust 侧没有 Smithy 中间件，
//! 对应逻辑是 [`merge_custom_headers`]（构建请求时调用）。

use indexmap::IndexMap;

use kiana_ai::api::bedrock_converse_stream::{is_reserved_header, merge_custom_headers};

fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

fn overrides(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

#[test]
fn injects_the_caller_header() {
    let mut request = headers(&[("content-type", "application/json")]);
    merge_custom_headers(&mut request, &overrides(&[("x-custom", "v")]));
    assert_eq!(
        request
            .iter()
            .find(|(name, _)| name == "x-custom")
            .map(|(_, value)| value.as_str()),
        Some("v")
    );
}

#[test]
fn skips_reserved_headers_case_insensitively_while_applying_allowed_ones() {
    let mut request = headers(&[("authorization", "real"), ("host", "real-host")]);
    let overrides: IndexMap<String, String> = [
        ("authorization", "evil"),
        ("x-amz-date", "evil"),
        ("x-allowed", "ok"),
        ("Authorization", "evil2"),
        ("X-Amz-Date", "evil2"),
        ("HOST", "evil3"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect();

    assert!(is_reserved_header("authorization"));
    assert!(is_reserved_header("x-amz-date"));
    assert!(is_reserved_header("HOST"));
    assert!(!is_reserved_header("x-allowed"));

    merge_custom_headers(&mut request, &overrides);
    let map: IndexMap<String, String> = request
        .iter()
        .map(|(name, value)| (name.to_lowercase(), value.clone()))
        .collect();
    assert_eq!(map.get("authorization").map(String::as_str), Some("real"));
    assert_eq!(map.get("host").map(String::as_str), Some("real-host"));
    assert!(!map.contains_key("x-amz-date"));
    assert_eq!(map.get("x-allowed").map(String::as_str), Some("ok"));
}

#[test]
fn overrides_an_existing_same_name_header() {
    let mut request = headers(&[("X-Custom", "old")]);
    merge_custom_headers(&mut request, &overrides(&[("x-custom", "new")]));
    let values: Vec<&str> = request
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("x-custom"))
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(values, vec!["new"]);
}

#[test]
fn does_not_register_anything_for_an_empty_override_set() {
    let mut request = headers(&[("accept", "x")]);
    merge_custom_headers(&mut request, &IndexMap::new());
    assert_eq!(request, headers(&[("accept", "x")]));
}
