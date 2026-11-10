//! 移植自 `packages/durable/test/indexeddb-storage.test.ts`。
//!
//! **IndexedDB 后端不移植**（ARCHITECTURE.md §8 第 2 条）：浏览器 API 在 Rust 二进制里没有
//! 对应运行时，M1 也没有 `wasm32-unknown-unknown` 目标。TS 的该测试文件由
//! `registerStorageConformance(..., "IndexedDbStorage", ...)` 与 4 条 IndexedDB 专属用例
//! （跨 reopen 的 conformance、mint 持久化、事务回滚、fork 祖先扫描、单事务混合写入）组成；
//! 这些行为在 `storage-conformance` 里记为“不适用”，不是失败。
//!
//! Rust 侧能验证的契约只有占位入口本身：`open()` 恒失败，文案逐字，且保留调用方给的名字
//! 供诊断日志使用。真正的存储语义由 `MemoryStorage`（参考语义）、JSONL 与 SQLite 后端覆盖。

use kiana_durable::storage::indexeddb::{
    IndexedDbStorage, IndexedDbStorageOptions, StorageBackendUnsupported,
};

/// TS 的 `globalThis.indexedDB` 由 `fake-indexeddb` 提供；Rust 没有等价物，`indexedDB`
/// 选项在占位实现里被忽略（见 `indexeddb.rs` 的模块文档）。
#[test]
fn open_always_reports_the_backend_as_unsupported() {
    let options = IndexedDbStorageOptions {
        database_name: Some("conformance".to_owned()),
        database_version: Some(1),
    };
    let error = IndexedDbStorage::open(options.clone())
        .expect_err("the Rust build has no IndexedDB backend");
    assert_eq!(
        error,
        StorageBackendUnsupported::IndexedDb {
            database_name: Some("conformance".to_owned()),
        }
    );
    assert_eq!(
        error.to_string(),
        "IndexedDB storage is not supported in the Rust build"
    );
    // 选项本身仍可按值构造与比较（TS 的 `IndexedDbStorageOptions` 形状）。
    assert_eq!(options.database_version, Some(1));
}

/// `open()` 不读全局变量：不给 `databaseName` 也走同一条失败路径。
#[test]
fn open_without_a_database_name_still_fails() {
    let error = IndexedDbStorage::open(IndexedDbStorageOptions::default())
        .expect_err("the Rust build has no IndexedDB backend");
    assert_eq!(
        error.to_string(),
        "IndexedDB storage is not supported in the Rust build"
    );
    assert_eq!(
        error,
        StorageBackendUnsupported::IndexedDb {
            database_name: None
        }
    );
}

/// 同一枚举还承载 Cloudflare Durable Object 存储的占位错误（§8 第 3 条），文案各自逐字。
#[test]
fn distinct_browser_backend_errors_keep_their_own_messages() {
    assert_eq!(
        StorageBackendUnsupported::Cloudflare.to_string(),
        "Cloudflare Durable Object storage is not supported in the Rust build"
    );
    // `StorageBackendUnsupported` 是 `std::error::Error`，可以进入 `BoxError` 链。
    let boxed: kiana_durable::BoxError = Box::new(StorageBackendUnsupported::indexeddb(
        IndexedDbStorageOptions::default(),
    ));
    assert_eq!(
        boxed.to_string(),
        "IndexedDB storage is not supported in the Rust build"
    );
}
