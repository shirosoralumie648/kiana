//! `packages/chord/test/sqlite/{migrations,node}.test.ts` 的 Rust 端口。
//!
//! 需要真实 SQLite：`cargo test -p kiana-chord --features sqlite-node --test sqlite_node`。
//! TS 用 `node:sqlite`，Rust 用 `rusqlite`（bundled）。JS 的微任务时序在 Rust 里用
//! `tokio::spawn` + `futures::poll!` 表达：`futures::poll!` 把 future 推到第一个
//! await（与 TS 里调用即开始执行同步前缀一致），排队的 future 在调用时就已登记轮次。

#![cfg(feature = "sqlite-node")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kiana_chord::sqlite::node::{
    NodeSqliteDatabase, NodeSqliteDatabaseOptions, open_node_sqlite_database,
};
use kiana_chord::sqlite::{
    SqliteBoxFuture, SqliteDatabase, SqliteDatabaseExt, SqliteError, SqliteExecutor,
    SqliteMigration, SqliteRow, SqliteValue, apply_sqlite_migrations,
    validate_sqlite_schema_prefix,
};
use tokio::sync::oneshot;

fn row(pairs: &[(&str, SqliteValue)]) -> SqliteRow {
    SqliteRow::from_pairs(pairs.iter().cloned())
}

fn int(value: i64) -> SqliteValue {
    SqliteValue::Integer(value)
}

async fn open() -> NodeSqliteDatabase {
    open_node_sqlite_database(":memory:", NodeSqliteDatabaseOptions::default())
        .await
        .expect("open sqlite")
}

/// 让出调度器，给其他任务执行机会；`yield_now` 单次只让出当前任务，循环若干次以确保推进。
async fn yield_tasks() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn tracks_independent_histories_and_skips_migrations_already_applied() {
    let database = open().await;
    let first = vec![SqliteMigration::new(
        1,
        [
            "CREATE TABLE notes (body TEXT)",
            "INSERT INTO notes VALUES ('kept')",
        ],
    )];
    apply_sqlite_migrations(&database, &first, "notes_schema")
        .await
        .expect("first");
    apply_sqlite_migrations(&database, &first, "notes_schema")
        .await
        .expect("skips applied");
    apply_sqlite_migrations(&database, &[], "other_schema")
        .await
        .expect("empty");
    let mut extended = first.clone();
    extended.push(SqliteMigration::new(
        2,
        ["ALTER TABLE notes ADD COLUMN title TEXT"],
    ));
    apply_sqlite_migrations(&database, &extended, "notes_schema")
        .await
        .expect("second");
    assert_eq!(
        database
            .all("SELECT body, title FROM notes", &[])
            .await
            .expect("notes"),
        vec![row(&[
            ("body", SqliteValue::Text("kept".into())),
            ("title", SqliteValue::Null)
        ])]
    );
    assert_eq!(
        database
            .get("SELECT version FROM notes_schema", &[])
            .await
            .expect("version"),
        Some(row(&[("version", int(2))]))
    );
    assert_eq!(
        database
            .get("SELECT version FROM other_schema", &[])
            .await
            .expect("version"),
        Some(row(&[("version", int(0))]))
    );
    let error = apply_sqlite_migrations(&database, &first, "notes_schema")
        .await
        .expect_err("newer than supported");
    assert_eq!(
        error.message,
        "SQLite schema version 2 is newer than supported version 1"
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn rejects_invalid_migration_versions_before_touching_the_database() {
    let database = open().await;
    for versions in [vec![0], vec![2], vec![1, 3], vec![1, 1]] {
        let migrations: Vec<SqliteMigration> = versions
            .into_iter()
            .map(|version| SqliteMigration::new(version, Vec::<String>::new()))
            .collect();
        let error = apply_sqlite_migrations(&database, &migrations, "app_schema")
            .await
            .expect_err("invalid versions");
        assert_eq!(
            error.message,
            "SQLite migrations must have contiguous versions starting at 1"
        );
    }
    assert_eq!(
        database
            .all("SELECT name FROM sqlite_schema", &[])
            .await
            .expect("schema"),
        Vec::<SqliteRow>::new()
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn validates_prefixes_and_migration_table_identifiers_before_executing_sql() {
    let database = open().await;
    for prefix in [
        "",
        "1bad",
        "bad-prefix",
        "bad; DROP TABLE notes",
        "sqlite_app",
        "SQLITE_app",
    ] {
        assert!(
            validate_sqlite_schema_prefix(prefix).is_err(),
            "prefix {prefix:?}"
        );
        assert!(
            apply_sqlite_migrations(&database, &[], prefix)
                .await
                .is_err(),
            "table {prefix:?}"
        );
    }
    for prefix in ["kiana_", "_private", "app2"] {
        assert!(
            validate_sqlite_schema_prefix(prefix).is_ok(),
            "prefix {prefix:?}"
        );
    }
    assert_eq!(
        database
            .all("SELECT name FROM sqlite_schema", &[])
            .await
            .expect("schema"),
        Vec::<SqliteRow>::new()
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn rolls_initial_bootstrap_and_every_pending_migration_back_together() {
    let database = open().await;
    let failed = vec![
        SqliteMigration::new(
            1,
            [
                "CREATE TABLE migration_first (value TEXT) STRICT",
                "INSERT INTO migration_first (value) VALUES ('retained')",
            ],
        ),
        SqliteMigration::new(
            2,
            [
                "CREATE TABLE migration_second (value TEXT) STRICT",
                "THIS IS NOT SQL",
            ],
        ),
    ];
    apply_sqlite_migrations(&database, &failed, "app_schema")
        .await
        .expect_err("invalid SQL");
    assert_eq!(
        database
            .get(
                "SELECT count(*) AS count FROM sqlite_schema WHERE name IN ('app_schema', 'migration_first', 'migration_second')",
                &[],
            )
            .await
            .expect("count"),
        Some(row(&[("count", int(0))]))
    );

    let repaired = vec![
        failed[0].clone(),
        SqliteMigration::new(2, ["CREATE TABLE migration_second (value TEXT) STRICT"]),
    ];
    apply_sqlite_migrations(&database, &repaired, "app_schema")
        .await
        .expect("repaired");
    assert_eq!(
        database
            .get("SELECT version FROM app_schema WHERE singleton = 1", &[])
            .await
            .expect("version"),
        Some(row(&[("version", int(2))]))
    );
    assert_eq!(
        database
            .get("SELECT value FROM migration_first", &[])
            .await
            .expect("value"),
        Some(row(&[("value", SqliteValue::Text("retained".into()))]))
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn configures_connection_defaults_and_accepts_explicit_settings() {
    for options in [
        NodeSqliteDatabaseOptions::default(),
        NodeSqliteDatabaseOptions {
            busy_timeout_ms: Some(123),
            wal_auto_checkpoint_pages: Some(0),
        },
    ] {
        let database = open_node_sqlite_database(":memory:", options.clone())
            .await
            .expect("open");
        assert_eq!(
            database
                .get("PRAGMA busy_timeout", &[])
                .await
                .expect("busy_timeout"),
            Some(row(&[(
                "timeout",
                int(options.busy_timeout_ms.unwrap_or(5_000) as i64)
            )]))
        );
        assert_eq!(
            database
                .get("PRAGMA wal_autocheckpoint", &[])
                .await
                .expect("wal_autocheckpoint"),
            Some(row(&[(
                "wal_autocheckpoint",
                int(options.wal_auto_checkpoint_pages.unwrap_or(1_000))
            )]))
        );
        assert_eq!(
            database
                .get("PRAGMA synchronous", &[])
                .await
                .expect("synchronous"),
            Some(row(&[("synchronous", int(1))]))
        );
        database.close().await.expect("close");
    }
}

#[tokio::test]
async fn commits_work_done_through_the_transaction_handle_and_closes_idempotently() {
    let database = open().await;
    database
        .transaction_typed::<(), _>(|transaction| {
            Box::pin(async move {
                transaction
                    .exec("CREATE TABLE async_probe (value INTEGER)")
                    .await?;
                transaction
                    .run("INSERT INTO async_probe (value) VALUES (?)", &[int(1)])
                    .await?;
                Ok(())
            })
        })
        .await
        .expect("transaction");
    assert_eq!(
        database
            .get("SELECT value FROM async_probe", &[])
            .await
            .expect("value"),
        Some(row(&[("value", int(1))]))
    );
    database.close().await.expect("first close");
    database.close().await.expect("second close");
}

#[tokio::test]
async fn serializes_concurrent_transactions() {
    let database = Arc::new(open().await);
    database
        .exec("CREATE TABLE transaction_queue (value INTEGER)")
        .await
        .expect("create");
    let (first_started, first_started_rx) = oneshot::channel();
    let (release_first, first_gate) = oneshot::channel::<()>();
    let first = tokio::spawn({
        let database = Arc::clone(&database);
        async move {
            database
                .transaction_typed::<(), _>(move |transaction| {
                    Box::pin(async move {
                        transaction
                            .exec("INSERT INTO transaction_queue (value) VALUES (1)")
                            .await?;
                        let _ = first_started.send(());
                        let _ = first_gate.await;
                        Ok(())
                    })
                })
                .await
        }
    });
    first_started_rx.await.expect("first started");

    let second_started = Arc::new(AtomicBool::new(false));
    let second = tokio::spawn({
        let database = Arc::clone(&database);
        let second_started = Arc::clone(&second_started);
        async move {
            database
                .transaction_typed::<(), _>(move |transaction| {
                    Box::pin(async move {
                        second_started.store(true, Ordering::SeqCst);
                        transaction
                            .exec("INSERT INTO transaction_queue (value) VALUES (2)")
                            .await?;
                        Ok(())
                    })
                })
                .await
        }
    });
    yield_tasks().await;
    assert!(!second_started.load(Ordering::SeqCst));

    release_first.send(()).expect("release");
    first.await.expect("join first").expect("first");
    second.await.expect("join second").expect("second");
    assert_eq!(
        database
            .all("SELECT value FROM transaction_queue ORDER BY value", &[])
            .await
            .expect("rows"),
        vec![row(&[("value", int(1))]), row(&[("value", int(2))])]
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn runs_operations_in_call_order_whether_they_start_immediately_or_wait() {
    let database = Arc::new(open().await);
    database
        .exec("CREATE TABLE call_order (value INTEGER)")
        .await
        .expect("create");
    let (started, started_rx) = oneshot::channel();
    let transaction = tokio::spawn({
        let database = Arc::clone(&database);
        async move {
            database
                .transaction_typed::<(), _>(move |handle| {
                    Box::pin(async move {
                        handle
                            .run("INSERT INTO call_order (value) VALUES (?)", &[int(1)])
                            .await?;
                        let _ = started.send(());
                        tokio::task::yield_now().await;
                        Err(SqliteError::new("roll back"))
                    })
                })
                .await
        }
    });
    started_rx.await.expect("started");

    // 事务期间发起的操作既不能看到未提交的行，也不能加入它的回滚。
    let before_write = database.all("SELECT value FROM call_order ORDER BY value", &[]);
    let write_params = [int(2)];
    let write = database.run("INSERT INTO call_order (value) VALUES (?)", &write_params);
    let after_write = database.all("SELECT value FROM call_order ORDER BY value", &[]);

    let error = transaction.await.expect("join").expect_err("rollback");
    assert_eq!(error.message, "roll back");
    write.await.expect("write");
    assert_eq!(before_write.await.expect("before"), Vec::<SqliteRow>::new());
    assert_eq!(
        after_write.await.expect("after"),
        vec![row(&[("value", int(2))])]
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn queues_ordinary_operations_behind_an_active_transaction() {
    let database = Arc::new(open().await);
    database
        .exec("CREATE TABLE operation_queue (value INTEGER)")
        .await
        .expect("create");
    let (started, started_rx) = oneshot::channel();
    let (release, gate) = oneshot::channel::<()>();
    let pending = tokio::spawn({
        let database = Arc::clone(&database);
        async move {
            database
                .transaction_typed::<(), _>(move |transaction| {
                    Box::pin(async move {
                        transaction
                            .exec("INSERT INTO operation_queue (value) VALUES (1)")
                            .await?;
                        let _ = started.send(());
                        let _ = gate.await;
                        Ok(())
                    })
                })
                .await
        }
    });
    started_rx.await.expect("started");

    let mut write = Box::pin(database.exec("INSERT INTO operation_queue (value) VALUES (2)"));
    assert!(futures::poll!(write.as_mut()).is_pending());
    let mut read = Box::pin(database.all("SELECT value FROM operation_queue ORDER BY value", &[]));
    assert!(futures::poll!(read.as_mut()).is_pending());

    release.send(()).expect("release");
    pending.await.expect("join").expect("transaction");
    write.await.expect("write");
    assert_eq!(
        read.await.expect("read"),
        vec![row(&[("value", int(1))]), row(&[("value", int(2))])]
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn queues_database_calls_made_synchronously_by_a_transaction_that_started_immediately() {
    let database = Arc::new(open().await);
    database
        .exec("CREATE TABLE barrier_probe (value INTEGER)")
        .await
        .expect("create");
    let (outside_sender, outside_receiver) =
        oneshot::channel::<SqliteBoxFuture<'static, Result<(), SqliteError>>>();
    let outside_database = Arc::clone(&database);
    let transaction = tokio::spawn({
        let database = Arc::clone(&database);
        async move {
            database
                .transaction_typed::<(), _>(move |handle| {
                    Box::pin(async move {
                        // 误用：这个调用必须等事务结束，而不是加入事务。
                        let mut outside: SqliteBoxFuture<'static, Result<(), SqliteError>> =
                            Box::pin(async move {
                                let params = [int(2)];
                                outside_database
                                    .run("INSERT INTO barrier_probe (value) VALUES (?)", &params)
                                    .await
                            });
                        assert!(
                            futures::poll!(outside.as_mut()).is_pending(),
                            "outside call must queue behind the transaction"
                        );
                        let _ = outside_sender.send(outside);
                        handle
                            .run("INSERT INTO barrier_probe (value) VALUES (?)", &[int(1)])
                            .await?;
                        tokio::task::yield_now().await;
                        Err(SqliteError::new("roll back"))
                    })
                })
                .await
        }
    });
    let outside = outside_receiver.await.expect("outside future");
    let error = transaction.await.expect("join").expect_err("rollback");
    assert_eq!(error.message, "roll back");
    outside.await.expect("outside write");
    assert_eq!(
        database
            .all("SELECT value FROM barrier_probe", &[])
            .await
            .expect("rows"),
        vec![row(&[("value", int(2))])]
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn rejects_a_transaction_handle_used_after_its_transaction_settles() {
    let database = open().await;
    database
        .exec("CREATE TABLE stale_probe (value INTEGER)")
        .await
        .expect("create");
    let (handle_sender, handle_receiver) = oneshot::channel::<Box<dyn SqliteExecutor>>();
    database
        .transaction_typed::<(), _>(move |transaction| {
            Box::pin(async move {
                transaction
                    .run("INSERT INTO stale_probe (value) VALUES (?)", &[int(1)])
                    .await?;
                let _ = handle_sender.send(transaction);
                Ok(())
            })
        })
        .await
        .expect("transaction");
    let handle = handle_receiver.await.expect("handle");
    let stale = "SQLite transaction handle is no longer active";
    assert_eq!(
        handle
            .exec("INSERT INTO stale_probe (value) VALUES (2)")
            .await
            .expect_err("stale exec")
            .message,
        stale
    );
    assert_eq!(
        handle
            .run("INSERT INTO stale_probe (value) VALUES (?)", &[int(3)])
            .await
            .expect_err("stale run")
            .message,
        stale
    );
    assert_eq!(
        database
            .all("SELECT value FROM stale_probe", &[])
            .await
            .expect("rows"),
        vec![row(&[("value", int(1))])]
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn does_not_preserve_the_callback_error_when_rollback_itself_fails() {
    let database = open().await;
    database
        .exec("CREATE TABLE rollback_probe (value INTEGER)")
        .await
        .expect("create");
    let error: SqliteError = database
        .transaction_typed::<(), _>(|transaction| {
            Box::pin(async move {
                transaction
                    .exec("INSERT INTO rollback_probe (value) VALUES (1)")
                    .await?;
                transaction.exec("COMMIT").await?;
                Err(SqliteError::new("rejected after an escaped commit"))
            })
        })
        .await
        .expect_err("aggregate");
    assert!(error.is_aggregate());
    assert_eq!(
        error.message,
        "SQLite transaction failed and rollback failed"
    );
    assert_eq!(error.errors.len(), 2);
    assert_eq!(error.errors[0].message, "rejected after an escaped commit");
    assert_eq!(
        database
            .get("SELECT value FROM rollback_probe", &[])
            .await
            .expect("value"),
        Some(row(&[("value", int(1))]))
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn binds_blobs_and_text_and_reads_rows() {
    let database = open().await;
    database
        .exec("CREATE TABLE blobs (data BLOB, label TEXT, note TEXT)")
        .await
        .expect("create");
    database
        .run(
            "INSERT INTO blobs (data, label, note) VALUES (?, ?, ?)",
            &[
                SqliteValue::Blob(vec![1, 2, 3]),
                SqliteValue::Text("x".into()),
                SqliteValue::Null,
            ],
        )
        .await
        .expect("insert");
    let row = database
        .get("SELECT data, label, note FROM blobs", &[])
        .await
        .expect("select")
        .expect("row");
    assert_eq!(row.get_blob("data"), Some(&[1u8, 2, 3][..]));
    assert_eq!(row.get_text("label"), Some("x"));
    assert_eq!(row.get("note"), Some(&SqliteValue::Null));
    database.close().await.expect("close");
}

// ── 回归：node:sqlite 的执行、绑定、错误与取消语义 ──────────────────────────

#[tokio::test]
async fn run_executes_row_returning_statements_and_binds_missing_parameters_to_null() {
    let database = open().await;
    // node 的 StatementSync.run 会执行任何语句（包括返回行的）并正常结算。
    database
        .run("PRAGMA journal_mode = WAL", &[])
        .await
        .expect("journal pragma");
    database
        .run("PRAGMA user_version", &[])
        .await
        .expect("user version pragma");
    database.run("SELECT 1", &[]).await.expect("select");
    database
        .exec("CREATE TABLE returning_rows (value INTEGER)")
        .await
        .expect("create");
    database
        .run(
            "INSERT INTO returning_rows (value) VALUES (1) RETURNING value",
            &[],
        )
        .await
        .expect("returning");
    // 缺少的位置参数按 NULL 绑定。
    database
        .run("INSERT INTO returning_rows (value) VALUES (?)", &[])
        .await
        .expect("missing parameter");
    let rows = database
        .all("SELECT value FROM returning_rows", &[])
        .await
        .expect("rows");
    assert_eq!(
        rows,
        vec![
            row(&[("value", int(1))]),
            row(&[("value", SqliteValue::Null)])
        ]
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn run_rejects_extra_parameters_like_node_sqlite() {
    let database = open().await;
    assert_eq!(
        database
            .run("SELECT ? AS value", &[int(1), int(2)])
            .await
            .expect_err("extra run parameter")
            .message,
        "column index out of range"
    );
    assert_eq!(
        database
            .get("SELECT ? AS value", &[int(1), int(2)])
            .await
            .expect_err("extra get parameter")
            .message,
        "column index out of range"
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn prepares_only_the_first_statement_of_a_multi_statement_string() {
    let database = open().await;
    database
        .exec("CREATE TABLE multi (value INTEGER)")
        .await
        .expect("create");
    database
        .run(
            "INSERT INTO multi (value) VALUES (1); INSERT INTO multi (value) VALUES (2)",
            &[],
        )
        .await
        .expect("insert");
    let first = database
        .get("SELECT value FROM multi; SELECT 99 AS other", &[])
        .await
        .expect("get")
        .expect("row");
    assert_eq!(first.get_i64("value"), Some(1));
    assert!(!first.contains("other"));
    let count = database
        .get("SELECT count(*) AS c FROM multi", &[])
        .await
        .expect("count")
        .expect("row");
    assert_eq!(count.get_i64("c"), Some(1));
    database.close().await.expect("close");
}

#[tokio::test]
async fn rejects_double_quoted_string_literals_like_node_sqlite() {
    let database = open().await;
    database
        .exec("CREATE TABLE names (name TEXT)")
        .await
        .expect("create");
    database
        .run("INSERT INTO names (name) VALUES ('alice')", &[])
        .await
        .expect("insert");
    assert_eq!(
        database
            .all("SELECT * FROM names WHERE name = \"alice\"", &[])
            .await
            .expect_err("dqs dml")
            .message,
        "no such column: \"alice\" - should this be a string literal in single-quotes?"
    );
    // DQS_DDL 同样关闭：表达式索引里的双引号串不是字面量。
    assert_eq!(
        database
            .exec("CREATE INDEX names_idx ON names (name) WHERE name = \"alice\"")
            .await
            .expect_err("dqs ddl")
            .message,
        "no such column: \"alice\" - should this be a string literal in single-quotes?"
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn reports_sqlite_messages_without_rusqlite_statement_context() {
    let database = open().await;
    assert_eq!(
        database.exec("SELEC 1").await.expect_err("syntax").message,
        "near \"SELEC\": syntax error"
    );
    database
        .exec("CREATE TABLE columns (value INTEGER)")
        .await
        .expect("create");
    assert_eq!(
        database
            .all("SELECT missing FROM columns", &[])
            .await
            .expect_err("column")
            .message,
        "no such column: missing"
    );
    assert_eq!(
        database
            .run("INSERT INTO missing_table VALUES (1)", &[])
            .await
            .expect_err("table")
            .message,
        "no such table: missing_table"
    );
    database
        .exec("CREATE TABLE unique_values (value INTEGER UNIQUE)")
        .await
        .expect("create unique");
    database
        .run("INSERT INTO unique_values (value) VALUES (1)", &[])
        .await
        .expect("first insert");
    assert_eq!(
        database
            .run("INSERT INTO unique_values (value) VALUES (1)", &[])
            .await
            .expect_err("unique")
            .message,
        "UNIQUE constraint failed: unique_values.value"
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn rejects_integers_outside_the_javascript_safe_range() {
    let database = open().await;
    assert_eq!(
        database
            .get("SELECT 9007199254740993 AS value", &[])
            .await
            .expect_err("too large")
            .message,
        "Value is too large to be represented as a JavaScript number: 9007199254740993"
    );
    assert_eq!(
        database
            .get("SELECT -9007199254740992 AS value", &[])
            .await
            .expect_err("too small")
            .message,
        "Value is too large to be represented as a JavaScript number: -9007199254740992"
    );
    let row = database
        .get("SELECT 9007199254740991 AS value", &[])
        .await
        .expect("max safe")
        .expect("row");
    assert_eq!(row.get_i64("value"), Some(9_007_199_254_740_991));
    database.close().await.expect("close");
}

#[tokio::test]
async fn cancelling_a_running_transaction_rolls_back_and_keeps_the_queue_usable() {
    let database = open().await;
    database
        .exec("CREATE TABLE cancel (value INTEGER)")
        .await
        .expect("create");
    let transaction = database.transaction_typed::<(), _>(|transaction| {
        Box::pin(async move {
            transaction
                .run("INSERT INTO cancel (value) VALUES (1)", &[])
                .await?;
            std::future::pending::<()>().await;
            Ok(())
        })
    });
    let timed_out = tokio::time::timeout(std::time::Duration::from_millis(100), transaction).await;
    assert!(timed_out.is_err(), "the transaction is still running");
    // 取消回滚了打开的写事务：后续写入不会加入一个被遗弃的事务。
    database
        .exec("INSERT INTO cancel (value) VALUES (2)")
        .await
        .expect("write after cancellation");
    assert_eq!(
        database
            .all("SELECT value FROM cancel", &[])
            .await
            .expect("rows"),
        vec![row(&[("value", int(2))])]
    );
    database
        .transaction_typed::<(), _>(|transaction| {
            Box::pin(async move {
                transaction
                    .run("INSERT INTO cancel (value) VALUES (3)", &[])
                    .await?;
                Ok(())
            })
        })
        .await
        .expect("next transaction");
    database.close().await.expect("close");
}

#[tokio::test]
async fn dropping_a_queued_transaction_releases_its_turn() {
    use std::future::Future;

    let database = open().await;
    let (release_sender, release_receiver) = oneshot::channel::<()>();
    let first = database.transaction_typed::<(), _>(move |transaction| {
        Box::pin(async move {
            transaction
                .exec("CREATE TABLE queued (value INTEGER)")
                .await?;
            let _ = release_receiver.await;
            Ok(())
        })
    });
    tokio::pin!(first);
    std::future::poll_fn(|context| {
        let _ = first.as_mut().poll(context);
        std::task::Poll::Ready(())
    })
    .await;
    // 第二个事务入队，并 poll 一次以登记等待。
    let mut second = database.transaction_typed::<(), _>(|transaction| {
        Box::pin(async move {
            transaction
                .run("INSERT INTO queued (value) VALUES (1)", &[])
                .await?;
            Ok(())
        })
    });
    std::future::poll_fn(|context| {
        let _ = second.as_mut().poll(context);
        std::task::Poll::Ready(())
    })
    .await;
    // 第一个事务结算时 leave() 已经把轮次信号发给第二个事务。
    let _ = release_sender.send(());
    first.await.expect("first transaction");
    // 第二个事务从未再次被 poll 就被丢弃：队列必须继续，而不是永远 busy。
    drop(second);
    let executed = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        database.exec("INSERT INTO queued (value) VALUES (2)"),
    )
    .await;
    executed
        .expect("queue continues after dropping a queued transaction")
        .expect("insert");
    assert_eq!(
        database
            .all("SELECT value FROM queued", &[])
            .await
            .expect("rows"),
        vec![row(&[("value", int(2))])]
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn ignores_invalid_trailing_statements_like_node_sqlite() {
    let database = open().await;
    let row = database
        .get("SELECT 1 AS value; SELEC 2", &[])
        .await
        .expect("first statement compiles")
        .expect("row");
    assert_eq!(row.get_i64("value"), Some(1));
    database
        .run("SELECT 1; SELEC 2", &[])
        .await
        .expect("run ignores the tail");
    let row = database
        .get("SELECT ';' AS value", &[])
        .await
        .expect("semicolon inside a literal")
        .expect("row");
    assert_eq!(row.get_text("value"), Some(";"));
    database.close().await.expect("close");
}

#[tokio::test]
async fn dropping_an_unpolled_transaction_releases_the_queue() {
    let database = open().await;
    let transaction = database.transaction_typed::<(), _>(|_| Box::pin(async { Ok(()) }));
    drop(transaction);
    let executed =
        tokio::time::timeout(std::time::Duration::from_secs(2), database.exec("SELECT 1")).await;
    executed.expect("queue continues").expect("exec");
    database.close().await.expect("close");
}

#[tokio::test]
async fn dropping_an_unpolled_queued_transaction_does_not_block_the_queue() {
    use std::future::Future;

    let database = open().await;
    let (release_sender, release_receiver) = oneshot::channel::<()>();
    let first = database.transaction_typed::<(), _>(move |_| {
        Box::pin(async move {
            let _ = release_receiver.await;
            Ok(())
        })
    });
    tokio::pin!(first);
    std::future::poll_fn(|context| {
        let _ = first.as_mut().poll(context);
        std::task::Poll::Ready(())
    })
    .await;
    // 入队后立即丢弃，从未 poll 过（守卫仍会在 drop 时标记这一轮已放弃）。
    drop(database.transaction_typed::<(), _>(|_| Box::pin(async { Ok(()) })));
    let _ = release_sender.send(());
    first.await.expect("first transaction");
    let executed =
        tokio::time::timeout(std::time::Duration::from_secs(2), database.exec("SELECT 1")).await;
    executed.expect("queue continues").expect("exec");
    database.close().await.expect("close");
}

#[tokio::test]
async fn binds_positional_parameters_to_anonymous_parameters_only() {
    let database = open().await;
    // 命名参数之后的匿名参数按匿名顺序接收位置参数。
    let row = database
        .get("SELECT :v AS named, ? AS anonymous", &[int(5)])
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.get("named"), Some(&SqliteValue::Null));
    assert_eq!(row.get_i64("anonymous"), Some(5));
    // 只有命名参数时，位置参数与 node 一样报错。
    assert_eq!(
        database
            .get("SELECT :v AS value", &[int(5)])
            .await
            .expect_err("named positional")
            .message,
        "column index out of range"
    );
    // `?NNN` 属于匿名参数。
    let row = database
        .get("SELECT ?1 AS value", &[int(5)])
        .await
        .expect("numbered")
        .expect("row");
    assert_eq!(row.get_i64("value"), Some(5));
    let row = database
        .get("SELECT ?2 AS value", &[int(1), int(2)])
        .await
        .expect("numbered second")
        .expect("row");
    assert_eq!(row.get_i64("value"), Some(2));
    let row = database
        .get("SELECT ?2 AS value", &[int(1)])
        .await
        .expect("numbered gap")
        .expect("row");
    assert_eq!(row.get("value"), Some(&SqliteValue::Null));
    database.close().await.expect("close");
}

#[tokio::test]
async fn rejects_sql_without_statements_like_node_sqlite() {
    let database = open().await;
    for sql in ["", "   ", "-- hi", "/* block */", ";", ";;", "; -- hi"] {
        assert_eq!(
            database
                .get(sql, &[])
                .await
                .expect_err("no statement")
                .message,
            "The SQL query contains no statements.",
            "get {sql:?}"
        );
        assert_eq!(
            database
                .run(sql, &[])
                .await
                .expect_err("no statement run")
                .message,
            "The SQL query contains no statements.",
            "run {sql:?}"
        );
    }
    let row = database
        .get("-- hi\nSELECT 1 AS v", &[])
        .await
        .expect("statement after a comment")
        .expect("row");
    assert_eq!(row.get_i64("v"), Some(1));
    database.close().await.expect("close");
}

#[tokio::test]
async fn classifies_odd_sql_like_node_sqlite() {
    let database = open().await;
    // SQLite 的词法器把**恰好**以 "/*" 结束的输入当作 `TK_SLASH` 记号：node:sqlite
    // 报 `near "/": syntax error`，而不是"没有语句"（"/* "、"/*x" 仍是注释）。
    // 前导空语句与注释都会先被跳过，所以组合输入同样命中这个规则。
    for sql in [
        "/*", " /*", "\t/*", ";\t/*", "; /*", ";;/*", "/*;*/ /*", "--\n/*",
    ] {
        assert_eq!(
            database
                .get(sql, &[])
                .await
                .expect_err("lone comment opener")
                .message,
            "near \"/\": syntax error",
            "get {sql:?}"
        );
    }
    for sql in ["/* ", "/*x", "/**"] {
        assert_eq!(
            database.get(sql, &[]).await.expect_err("comment").message,
            "The SQL query contains no statements.",
            "get {sql:?}"
        );
    }
    // node:sqlite 把 JS 字符串作为 NUL 结尾的 C 字符串交给 SQLite：第一个 U+0000
    // 之后的内容不可见。
    assert_eq!(
        database
            .get("\u{0}", &[])
            .await
            .expect_err("nul only")
            .message,
        "The SQL query contains no statements."
    );
    assert_eq!(
        database
            .get("x\u{0}y", &[])
            .await
            .expect_err("nul inside")
            .message,
        "near \"x\": syntax error"
    );
    let row = database
        .get("SELECT 1 AS v;\u{0}garbage", &[])
        .await
        .expect("text after nul is ignored")
        .expect("row");
    assert_eq!(row.get_i64("v"), Some(1));
    // exec 同样按 NUL 截断；"/*" 在 exec 里也报语法错误。
    database.exec("\u{0}").await.expect("empty exec");
    assert_eq!(
        database
            .exec("/*")
            .await
            .expect_err("exec lone comment opener")
            .message,
        "near \"/\": syntax error"
    );
    assert_eq!(
        database
            .exec("x\u{0}y")
            .await
            .expect_err("exec nul inside")
            .message,
        "near \"x\": syntax error"
    );
    database.close().await.expect("close");
}

#[tokio::test]
async fn rejects_operations_after_close_with_node_messages() {
    let database = open().await;
    database.close().await.expect("close");
    for (name, error) in [
        ("exec", database.exec("SELECT 1").await.expect_err("exec")),
        ("run", database.run("SELECT 1", &[]).await.expect_err("run")),
        ("get", database.get("SELECT 1", &[]).await.expect_err("get")),
        ("all", database.all("SELECT 1", &[]).await.expect_err("all")),
    ] {
        assert_eq!(error.message, "database is not open", "{name}");
    }
    let error = database
        .transaction_typed::<(), _>(|_| Box::pin(async { Ok(()) }))
        .await
        .expect_err("transaction after close");
    assert_eq!(error.message, "database is not open");
}

#[cfg(unix)]
#[tokio::test]
async fn reports_mkdir_failures_with_node_fs_text() {
    let path = std::env::temp_dir().join(format!(
        "chord-sqlite-open-{}-{}",
        std::process::id(),
        line!()
    ));
    std::fs::write(&path, b"x").expect("temp file");
    let child = path.join("nested.db");
    let error = open_node_sqlite_database(
        &child.to_string_lossy(),
        NodeSqliteDatabaseOptions::default(),
    )
    .await
    .expect_err("mkdir over a file");
    assert_eq!(
        error.message,
        format!(
            "EEXIST: file already exists, mkdir '{}'",
            path.to_string_lossy()
        )
    );
    let _ = std::fs::remove_file(&path);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn reports_sqlite_open_failures_with_node_text() {
    if !std::path::Path::new("/proc/1").is_dir() {
        return;
    }
    // mkdir 父目录成功（/proc/1 已存在），`DatabaseSync` 打开失败。node:sqlite 只暴露
    // `sqlite3_errmsg` 的文本；rusqlite 会在 CannotOpen 时追加 `": <path>"`。
    let error = open_node_sqlite_database(
        "/proc/1/chord-node-sqlite-test.db",
        NodeSqliteDatabaseOptions::default(),
    )
    .await
    .expect_err("procfs is not writable");
    assert_eq!(error.message, "unable to open database file");
}
