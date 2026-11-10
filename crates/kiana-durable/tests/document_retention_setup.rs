//! `test/setup-document-retention.ts` 的等价开关（spec/durable-harness.md §8.5 第 2 条）。
//!
//! 本二进制只包含 setup 夹具本身：它的初始化段在 `main` 之前运行，把环境变量
//! `KIANA_DURABLE_DOCUMENT_RETENTION_MS` 应用到默认文档保留期。断言在没有变量时默认值不变、
//! 有变量时按 `Number(value)` 生效（`0` → 每次提交后逐出）。
//!
//! 跑第二遍：
//! `KIANA_DURABLE_DOCUMENT_RETENTION_MS=0 cargo test -p kiana-durable`

#[path = "support/document_retention.rs"]
mod document_retention;

use kiana_durable::session::{DEFAULT_DOCUMENT_RETENTION_MS, document_retention};

#[test]
fn the_setup_fixture_applies_the_document_retention_environment() {
    let expected = std::env::var("KIANA_DURABLE_DOCUMENT_RETENTION_MS")
        .ok()
        .and_then(|raw| document_retention::parse_document_retention(&raw))
        .map_or(DEFAULT_DOCUMENT_RETENTION_MS, |ms| ms as f64);
    assert_eq!(document_retention(None), expected);
    // 显式给出的保留期仍然优先（`documentRetention(ms)` 的规则与夹具无关）。
    assert_eq!(document_retention(Some(1234.0)), 1234.0);
}
