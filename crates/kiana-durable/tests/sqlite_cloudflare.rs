//! `packages/durable/test/sqlite-cloudflare.test.ts` 的移植（占位实现）。
//!
//! Cloudflare Durable Object 后端**不移植**（ARCHITECTURE.md §8 第 3 条）：TS 把 chord 的
//! `DurableObjectSqliteDatabase`/`DurableObjectSqliteStorage` 再导出并加一个打开器；Rust 没有
//! Workers 注入的 SQL 执行器等价物，因此 `storage/sqlite/cloudflare.rs` 是冻结的 not supported
//! 占位。
//!
//! 因此本文件只覆盖占位能观察到的契约：
//! - 打开器总是返回 `StorageBackendUnsupported::Cloudflare`，`Display` 逐字；
//! - 占位类型可构造（与 TS 的再导出同形）；
//! - TS 用例里与后端无关的 schema 命名规则：默认不带前缀、`kiana_` 前缀只作用于 kiana 的表/索引、
//!   空串/非法/保留前缀被拒绝。这些规则由真实后端共用的 `createSqliteSchema` 决定，直接用
//!   它验证。
//!
//! 占位与 schema 都不依赖 `rusqlite`，因此不带任何 feature 也会运行：
//! `cargo test -p kiana-durable --test sqlite_cloudflare`。

use kiana_durable::storage::indexeddb::StorageBackendUnsupported;
use kiana_durable::storage::sqlite::cloudflare::{
    DurableObjectSqliteDatabase, DurableObjectSqliteStorage, open_durable_object_sqlite_storage,
};
use kiana_durable::storage::sqlite::schema::{DEFAULT_SQLITE_SCHEMA, create_sqlite_schema};

#[test]
fn the_durable_object_opener_is_an_explicit_not_supported_placeholder() {
    let failure = open_durable_object_sqlite_storage(DurableObjectSqliteStorage)
        .expect_err("Cloudflare storage is not supported");
    assert!(matches!(failure, StorageBackendUnsupported::Cloudflare));
    assert_eq!(
        failure.to_string(),
        "Cloudflare Durable Object storage is not supported in the Rust build"
    );
}

#[test]
fn the_placeholders_are_constructible_like_the_typescript_re_exports() {
    let storage = DurableObjectSqliteStorage;
    let database = DurableObjectSqliteDatabase;
    assert_eq!(format!("{storage:?}"), "DurableObjectSqliteStorage");
    assert_eq!(format!("{database:?}"), "DurableObjectSqliteDatabase");
    let _ = DurableObjectSqliteDatabase;
    let _ = DurableObjectSqliteStorage;
}

#[test]
fn keeps_schema_names_unprefixed_by_default() {
    assert_eq!(
        DEFAULT_SQLITE_SCHEMA.tables.durable_schema,
        "durable_schema"
    );
    let schema = create_sqlite_schema(None).expect("default schema");
    assert_eq!(schema.tables.tasks, "tasks");
    assert!(!schema.tables.durable_schema.starts_with("kiana_"));
}

#[test]
fn prefixes_only_kiana_tables_and_indexes_without_touching_host_tables() {
    let schema = create_sqlite_schema(Some("kiana_")).expect("prefixed schema");
    let names: Vec<&str> = vec![
        &schema.tables.durable_schema,
        &schema.tables.durable_metadata,
        &schema.tables.record_ids,
        &schema.tables.conversations,
        &schema.tables.entries,
        &schema.tables.tasks,
        &schema.tables.submissions,
        &schema.tables.documents,
        &schema.tables.document_revisions,
    ]
    .into_iter()
    .map(String::as_str)
    .collect();
    assert_eq!(names.len(), 9);
    assert!(names.iter().all(|name| name.starts_with("kiana_")));
    assert!(names.contains(&"kiana_tasks"));
    assert!(names.contains(&"kiana_durable_schema"));
    // 宿主自己的 `tasks`/`host_notes` 不受影响（TS 用同名表的对比断言）。
    assert!(!names.contains(&"tasks"));
    assert!(!names.contains(&"host_notes"));
}

#[test]
fn rejects_invalid_and_reserved_prefixes() {
    let invalid = create_sqlite_schema(Some("")).expect_err("empty prefix");
    assert!(invalid.message.contains("Invalid"), "{}", invalid.message);
    let invalid = create_sqlite_schema(Some("bad-prefix")).expect_err("invalid prefix");
    assert!(invalid.message.contains("Invalid"), "{}", invalid.message);
    let reserved = create_sqlite_schema(Some("sqlite_kiana_")).expect_err("reserved prefix");
    assert!(
        reserved.message.contains("reserved"),
        "{}",
        reserved.message
    );
}
