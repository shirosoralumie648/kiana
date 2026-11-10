//! `packages/durable/test-workers/storage.test.ts` 的移植。
//!
//! 需要真实 SQLite：`cargo test -p kiana-durable --features sqlite --test workers_storage`。
//!
//! TS 的 Workers 测试跑在 workerd 的 Durable Object 上。Cloudflare 后端不移植
//! （ARCHITECTURE.md §8 第 3 条），因此：
//! - `Durable Object SQLite` 前缀 conformance 无法运行（占位打开器永远 not supported）；
//! - 剩下的 **schema 迁移用例** 与 Workers 无关，它只依赖 `DurableObjectSqliteDatabase` 的
//!   SQL 执行能力。这里用内存 `NodeSqliteDatabase` 直接复现 TS 的步骤：同一连接上先应用
//!   版本 1（`legacy_` 前缀），插入 waiting 任务、拒绝 sleeping，再用同一个前缀打开存储触发
//!   版本 2 迁移，最后验证版本、保留的行、新的 sleeping 行与五个带前缀的索引。

#![cfg(feature = "sqlite")]

use std::sync::Arc;

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_chord::sqlite::database::{SqliteExecutor, SqliteValue};
use kiana_durable::Storage;
use kiana_durable::storage::sqlite::migrations::{
    apply_sqlite_migrations_for_schema_with, create_sqlite_migrations,
};
use kiana_durable::storage::sqlite::node::open_node_sqlite_database;
use kiana_durable::storage::sqlite::schema::create_sqlite_schema;
use kiana_durable::storage::sqlite::{SqliteStorage, SqliteStorageOptions};

fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

fn ok<T, E: std::fmt::Display>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{error}"),
    }
}

#[tokio::test]
async fn migrates_a_version_1_schema_to_admit_sleeping_tasks_keeping_tasks_and_indexes() {
    let database = ok(open_node_sqlite_database(":memory:", Default::default()).await);
    let schema = ok(create_sqlite_schema(Some("legacy_")));
    let tasks = schema.tables.tasks.clone();
    let migrations = create_sqlite_migrations(&schema);

    // 只应用版本 1：一个只认识 schema 版本 1 的数据库。
    ok(apply_sqlite_migrations_for_schema_with(&database, &schema, &migrations[..1]).await);
    let insert = |id: i64, status: &str| {
        let sql = format!(
            "INSERT INTO {tasks} (id, conversation_id, kind, status, abort_requested, background, record) VALUES (?, 1, 'test', ?, 0, 0, '{{}}')"
        );
        let params = vec![
            SqliteValue::Integer(id),
            SqliteValue::Text(status.to_owned()),
        ];
        let database = database.clone();
        async move { database.run(&sql, &params).await }
    };
    ok(insert(5, "waiting").await);
    assert!(
        insert(6, "sleeping").await.is_err(),
        "a version 1 database rejects sleeping tasks"
    );

    // 用同一前缀打开 durable 存储会应用版本 2。
    let storage = ok(SqliteStorage::open(
        Arc::new(database.clone()),
        SqliteStorageOptions {
            prefix: Some("legacy_".to_owned()),
        },
    )
    .await);
    let version = database
        .get(
            &format!("SELECT version FROM {}", schema.tables.durable_schema),
            &[],
        )
        .await
        .expect("read schema version")
        .expect("schema row")
        .get_i64("version");
    assert_eq!(version, Some(2));
    let waiting = database
        .get(&format!("SELECT status FROM {tasks} WHERE id = 5"), &[])
        .await
        .expect("read waiting task")
        .expect("waiting row");
    assert_eq!(waiting.get_text("status"), Some("waiting"));
    ok(insert(6, "sleeping").await);

    let indexes = database
        .all(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = ? ORDER BY name",
            &[SqliteValue::Text(tasks.clone())],
        )
        .await
        .expect("read indexes");
    let names: Vec<&str> = indexes
        .iter()
        .filter_map(|row| row.get_text("name"))
        .collect();
    assert_eq!(
        names,
        vec![
            "legacy_tasks_by_abort_requested",
            "legacy_tasks_by_background",
            "legacy_tasks_by_conversation",
            "legacy_tasks_by_kind",
            "legacy_tasks_by_status",
        ]
    );
    let leftover = database
        .get(
            "SELECT count(*) AS count FROM sqlite_master WHERE name = 'legacy_tasks_v2'",
            &[],
        )
        .await
        .expect("count legacy_tasks_v2")
        .expect("count row");
    assert_eq!(leftover.get_i64("count"), Some(0));

    storage.close(context()).await;
}
