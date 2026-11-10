//! `packages/durable/test/sqlite-migrations.test.ts` 的移植。
//!
//! 需要真实 SQLite：`cargo test -p kiana-durable --features sqlite --test sqlite_migrations`。
//!
//! 四个用例逐条对应 TS：
//! - 建出当前 schema 且可重复应用；
//! - 版本 1 的库迁移到版本 2（`sleeping` 状态），保留任务行与五个索引；
//! - 比当前版本新的库被拒绝；
//! - 失败的迁移整体回滚，重试成功后既有数据仍在。

#![cfg(feature = "sqlite")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use kiana_chord::context::BACKGROUND_CONTEXT;
use kiana_chord::context::Context;
use kiana_chord::sqlite::database::{SqliteDatabase, SqliteExecutor, SqliteRow};
use kiana_durable::storage::sqlite::migrations::apply_sqlite_migrations_with;
use kiana_durable::storage::sqlite::node::{open_node_sqlite_database, open_node_sqlite_storage};
use kiana_durable::storage::sqlite::{
    CURRENT_SQLITE_SCHEMA_VERSION, SQLITE_MIGRATIONS, SqliteMigration, apply_sqlite_migrations,
};
use kiana_durable::types::{
    AdoptFn, ConversationRecord, EntryBrand, EntryRecord, ROOT_CONVERSATION_ID, Seq, Storage,
    StorageWrite, TaskRecord, id_from_number, stringify_json,
};
use serde_json::json;

fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

fn adopt() -> AdoptFn {
    Box::new(|_seq: Seq| {})
}

/// 独占的临时目录；`Drop` 时尽力删除。
struct TempDir {
    path: String,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "kiana-durable-sqlite-migrations-{tag}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir {
            path: path.to_string_lossy().into_owned(),
        }
    }

    fn join(&self, name: &str) -> String {
        format!("{}/{}", self.path, name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn scalar(row: Option<SqliteRow>, column: &str) -> i64 {
    row.expect("row").get_i64(column).expect("integer column")
}

async fn open(path: &str) -> kiana_durable::storage::sqlite::node::NodeSqliteDatabase {
    open_node_sqlite_database(path, Default::default())
        .await
        .expect("open sqlite database")
}

/// `SELECT version FROM durable_schema WHERE singleton = 1`。
async fn schema_version(database: &dyn SqliteExecutor) -> i64 {
    scalar(
        database
            .get(
                "SELECT version FROM durable_schema WHERE singleton = 1",
                &[],
            )
            .await
            .expect("read schema version"),
        "version",
    )
}

#[tokio::test]
async fn creates_the_current_schema_and_can_be_applied_repeatedly() {
    let directory = TempDir::new("repeat");
    let database = open(&directory.join("storage.sqlite")).await;
    apply_sqlite_migrations(&database)
        .await
        .expect("apply migrations");
    apply_sqlite_migrations(&database)
        .await
        .expect("apply migrations again");
    assert_eq!(
        schema_version(&database).await,
        CURRENT_SQLITE_SCHEMA_VERSION
    );
    let metadata = database
        .get(
            "SELECT next_id, next_seq FROM durable_metadata WHERE singleton = 1",
            &[],
        )
        .await
        .expect("read metadata")
        .expect("metadata row");
    assert_eq!(metadata.get_text("next_id"), Some("2"));
    assert_eq!(metadata.get_i64("next_seq"), Some(1));
    database.close().await.expect("close");
}

#[tokio::test]
async fn migrates_a_version_1_database_to_store_sleeping_tasks_keeping_tasks_and_indexes() {
    let directory = TempDir::new("upgrade");
    let path = directory.join("storage.sqlite");
    let database = open(&path).await;
    apply_sqlite_migrations_with(&database, &SQLITE_MIGRATIONS[..1])
        .await
        .expect("apply version 1");
    let waiting: TaskRecord = serde_json::from_value(json!({
        "id": 5,
        "conversationId": 1,
        "kind": "test.task",
        "version": 1,
        "input": null,
        "background": false,
        "abortRequested": false,
        "state": { "status": "waiting", "checkpoint": { "phase": "run" }, "on": [], "policy": "allSettled" },
    }))
    .expect("task record");
    let waiting_json = stringify_json(&waiting).expect("encode task");
    database
        .run(
            "INSERT INTO tasks (id, conversation_id, kind, status, abort_requested, background, record) VALUES (5, 1, 'test.task', 'waiting', 0, 0, ?)",
            &[kiana_chord::sqlite::SqliteValue::Text(waiting_json.clone())],
        )
        .await
        .expect("insert waiting task");
    assert!(
        database
            .run(
                "INSERT INTO tasks (id, conversation_id, kind, status, abort_requested, background, record) VALUES (6, 1, 'test.task', 'sleeping', 0, 0, '{}')",
                &[],
            )
            .await
            .is_err(),
        "version 1 rejects sleeping tasks"
    );
    database.close().await.expect("close version 1");

    let storage = open_node_sqlite_storage(&path, Default::default())
        .await
        .expect("migrate through storage");
    storage.close(context()).await;

    let migrated = open(&path).await;
    assert_eq!(schema_version(&migrated).await, 2);
    let kept = migrated
        .get("SELECT status, record FROM tasks WHERE id = 5", &[])
        .await
        .expect("read task")
        .expect("task row");
    assert_eq!(kept.get_text("status"), Some("waiting"));
    assert_eq!(kept.get_text("record"), Some(waiting_json.as_str()));
    migrated
        .run(
            "INSERT INTO tasks (id, conversation_id, kind, status, abort_requested, background, record) VALUES (6, 1, 'test.task', 'sleeping', 0, 0, '{}')",
            &[],
        )
        .await
        .expect("version 2 accepts sleeping tasks");
    let indexes = migrated
        .all(
            "SELECT name FROM sqlite_schema WHERE type = 'index' AND tbl_name = 'tasks' ORDER BY name",
            &[],
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
            "tasks_by_abort_requested",
            "tasks_by_background",
            "tasks_by_conversation",
            "tasks_by_kind",
            "tasks_by_status",
        ]
    );
    assert_eq!(
        scalar(
            migrated
                .get(
                    "SELECT count(*) AS count FROM sqlite_schema WHERE name = 'tasks_v2'",
                    &[],
                )
                .await
                .expect("count tasks_v2"),
            "count",
        ),
        0
    );
    migrated.close().await.expect("close migrated");
}

#[tokio::test]
async fn rejects_a_database_newer_than_the_portable_core() {
    let directory = TempDir::new("newer");
    let path = directory.join("storage.sqlite");
    let database = open(&path).await;
    apply_sqlite_migrations(&database)
        .await
        .expect("apply migrations");
    database
        .run(
            "UPDATE durable_schema SET version = ? WHERE singleton = 1",
            &[kiana_chord::sqlite::SqliteValue::Integer(
                CURRENT_SQLITE_SCHEMA_VERSION + 1,
            )],
        )
        .await
        .expect("bump version");
    database.close().await.expect("close");

    let failure = open_node_sqlite_storage(&path, Default::default())
        .await
        .expect_err("newer schema is rejected");
    assert!(
        failure
            .to_string()
            .contains("is newer than supported version"),
        "unexpected message: {failure}"
    );
}

#[tokio::test]
async fn rolls_a_failed_migration_back_and_preserves_stored_data_for_a_successful_retry() {
    let directory = TempDir::new("rollback");
    let path = directory.join("storage.sqlite");
    let storage = open_node_sqlite_storage(&path, Default::default())
        .await
        .expect("open storage");
    storage
        .commit(
            &[
                StorageWrite::Conversation {
                    value: ConversationRecord {
                        id: ROOT_CONVERSATION_ID,
                        parent: None,
                        owner: None,
                    },
                },
                StorageWrite::Entry {
                    value: EntryRecord {
                        model: None,
                        data: Some(json!({ "retained": true })),
                        edits: None,
                        kind: "retained".to_owned(),
                        id: id_from_number::<EntryBrand>(2),
                        conversation_id: ROOT_CONVERSATION_ID,
                        head: None,
                        by_task_id: None,
                    },
                },
            ],
            adopt(),
            context(),
        )
        .await
        .expect("seed commit");
    storage.close(context()).await;

    let database = open(&path).await;
    let next_version = CURRENT_SQLITE_SCHEMA_VERSION + 1;
    let mut failed_migrations: Vec<SqliteMigration> = SQLITE_MIGRATIONS.clone();
    failed_migrations.push(SqliteMigration {
        version: next_version,
        statements: vec![
            "CREATE TABLE migration_probe (value TEXT) STRICT".to_owned(),
            "THIS IS NOT SQL".to_owned(),
        ],
    });
    assert!(
        apply_sqlite_migrations_with(&database, &failed_migrations)
            .await
            .is_err(),
        "the broken migration fails"
    );
    assert_eq!(
        schema_version(&database).await,
        CURRENT_SQLITE_SCHEMA_VERSION
    );
    assert_eq!(
        scalar(
            database
                .get(
                    "SELECT count(*) AS count FROM sqlite_schema WHERE type = 'table' AND name = 'migration_probe'",
                    &[],
                )
                .await
                .expect("count migration_probe"),
            "count",
        ),
        0
    );

    let mut successful_migrations: Vec<SqliteMigration> = SQLITE_MIGRATIONS.clone();
    successful_migrations.push(SqliteMigration {
        version: next_version,
        statements: vec!["CREATE TABLE migration_probe (value TEXT) STRICT".to_owned()],
    });
    apply_sqlite_migrations_with(&database, &successful_migrations)
        .await
        .expect("retry succeeds");
    assert_eq!(schema_version(&database).await, next_version);

    let entry = database
        .get("SELECT record, commit_seq FROM entries WHERE id = 2", &[])
        .await
        .expect("read entry")
        .expect("entry row");
    let expected = stringify_json(&EntryRecord {
        model: None,
        data: Some(json!({ "retained": true })),
        edits: None,
        kind: "retained".to_owned(),
        id: id_from_number::<EntryBrand>(2),
        conversation_id: ROOT_CONVERSATION_ID,
        head: None,
        by_task_id: None,
    })
    .expect("encode entry");
    assert_eq!(entry.get_text("record"), Some(expected.as_str()));
    assert_eq!(entry.get_i64("commit_seq"), Some(1));
    let metadata = database
        .get(
            "SELECT next_id, next_seq FROM durable_metadata WHERE singleton = 1",
            &[],
        )
        .await
        .expect("read metadata")
        .expect("metadata row");
    assert_eq!(metadata.get_text("next_id"), Some("3"));
    assert_eq!(metadata.get_i64("next_seq"), Some(2));
    database.close().await.expect("close");
}

/// `packages/durable/src/storage/sqlite/migrations.ts` 的 `SQLITE_MIGRATIONS`，由 TS 参考实现
/// `JSON.stringify` 逐字生成（缩进是制表符；`sqlite_schema.sql` 会原样保存这些文本，因此任何
/// 空格差异都是可见的写入差异）。
const TS_SQLITE_MIGRATIONS: &str = r##"[{"version":1,"statements":["CREATE TABLE durable_metadata (\n\t\t\tsingleton INTEGER PRIMARY KEY CHECK (singleton = 1),\n\t\t\tnext_id TEXT NOT NULL,\n\t\t\tnext_seq INTEGER NOT NULL\n\t\t) STRICT","INSERT INTO durable_metadata (singleton, next_id, next_seq) VALUES (1, '2', 1)","CREATE TABLE record_ids (\n\t\t\tid INTEGER PRIMARY KEY,\n\t\t\trecord_type TEXT NOT NULL CHECK (record_type IN ('conversation', 'entry', 'task', 'submission', 'document'))\n\t\t) STRICT","CREATE TABLE conversations (\n\t\t\tid INTEGER PRIMARY KEY,\n\t\t\towner_conversation_id INTEGER,\n\t\t\towner_task_id INTEGER,\n\t\t\trecord TEXT NOT NULL CHECK (json_valid(record))\n\t\t) STRICT","CREATE INDEX conversations_by_owner_conversation ON conversations (owner_conversation_id, id)","CREATE INDEX conversations_by_owner_task ON conversations (owner_task_id, id)","CREATE TABLE entries (\n\t\t\tid INTEGER PRIMARY KEY,\n\t\t\tconversation_id INTEGER NOT NULL,\n\t\t\thead INTEGER,\n\t\t\tcommit_seq INTEGER NOT NULL,\n\t\t\trecord TEXT NOT NULL CHECK (json_valid(record))\n\t\t) STRICT","CREATE INDEX entries_by_conversation ON entries (conversation_id, id DESC)","CREATE INDEX entry_heads_by_conversation ON entries (conversation_id, id DESC) WHERE head IS NOT NULL","CREATE TABLE tasks (\n\t\t\tid INTEGER PRIMARY KEY,\n\t\t\tconversation_id INTEGER NOT NULL,\n\t\t\tkind TEXT NOT NULL,\n\t\t\tstatus TEXT NOT NULL CHECK (status IN ('pending', 'running', 'waiting', 'completing', 'terminal')),\n\t\t\tabort_requested INTEGER NOT NULL CHECK (abort_requested IN (0, 1)),\n\t\t\tbackground INTEGER NOT NULL CHECK (background IN (0, 1)),\n\t\t\trecord TEXT NOT NULL CHECK (json_valid(record))\n\t\t) STRICT","CREATE INDEX tasks_by_status ON tasks (status, id)","CREATE INDEX tasks_by_conversation ON tasks (conversation_id, id)","CREATE INDEX tasks_by_kind ON tasks (kind, id)","CREATE INDEX tasks_by_abort_requested ON tasks (abort_requested, id)","CREATE INDEX tasks_by_background ON tasks (background, id)","CREATE TABLE submissions (\n\t\t\tid INTEGER PRIMARY KEY,\n\t\t\tconversation_id INTEGER NOT NULL,\n\t\t\trequest_id TEXT,\n\t\t\tstatus TEXT NOT NULL CHECK (status IN ('queued', 'placed', 'done', 'unanswered')),\n\t\t\trecord TEXT NOT NULL CHECK (json_valid(record))\n\t\t) STRICT","CREATE INDEX submissions_by_request ON submissions (conversation_id, request_id)","CREATE INDEX submissions_by_conversation ON submissions (conversation_id, id)","CREATE INDEX submissions_by_status ON submissions (status, id)","CREATE TABLE documents (\n\t\t\tid INTEGER PRIMARY KEY,\n\t\t\tkind TEXT NOT NULL,\n\t\t\tfamily INTEGER NOT NULL CHECK (family IN (0, 1)),\n\t\t\tkey_value TEXT NOT NULL,\n\t\t\tscope_kind TEXT NOT NULL CHECK (scope_kind IN ('session', 'conversation', 'task')),\n\t\t\towner_id INTEGER NOT NULL,\n\t\t\tcreated_at INTEGER NOT NULL,\n\t\t\tretired_at INTEGER,\n\t\t\trecord TEXT NOT NULL CHECK (json_valid(record))\n\t\t) STRICT","CREATE INDEX documents_by_address\n\t\t\tON documents (kind, scope_kind, owner_id, family, key_value, created_at DESC, retired_at)","CREATE INDEX documents_by_scope ON documents (scope_kind, owner_id, id)","CREATE INDEX documents_by_scope_kind ON documents (scope_kind, owner_id, kind, id)","CREATE TABLE document_revisions (\n\t\t\tdocument_id INTEGER NOT NULL,\n\t\t\tseq INTEGER NOT NULL,\n\t\t\tkind TEXT NOT NULL CHECK (kind IN ('base', 'delta')),\n\t\t\tversion INTEGER NOT NULL,\n\t\t\tcontent TEXT NOT NULL CHECK (json_valid(content)),\n\t\t\tPRIMARY KEY (document_id, seq)\n\t\t) STRICT","CREATE INDEX document_revisions_by_kind ON document_revisions (document_id, kind, seq DESC)"]},{"version":2,"statements":["CREATE TABLE tasks_v2 (\n\t\t\tid INTEGER PRIMARY KEY,\n\t\t\tconversation_id INTEGER NOT NULL,\n\t\t\tkind TEXT NOT NULL,\n\t\t\tstatus TEXT NOT NULL CHECK (status IN ('pending', 'running', 'waiting', 'sleeping', 'completing', 'terminal')),\n\t\t\tabort_requested INTEGER NOT NULL CHECK (abort_requested IN (0, 1)),\n\t\t\tbackground INTEGER NOT NULL CHECK (background IN (0, 1)),\n\t\t\trecord TEXT NOT NULL CHECK (json_valid(record))\n\t\t) STRICT","INSERT INTO tasks_v2 (id, conversation_id, kind, status, abort_requested, background, record)\n\t\t\tSELECT id, conversation_id, kind, status, abort_requested, background, record FROM tasks","DROP TABLE tasks","ALTER TABLE tasks_v2 RENAME TO tasks","CREATE INDEX tasks_by_status ON tasks (status, id)","CREATE INDEX tasks_by_conversation ON tasks (conversation_id, id)","CREATE INDEX tasks_by_kind ON tasks (kind, id)","CREATE INDEX tasks_by_abort_requested ON tasks (abort_requested, id)","CREATE INDEX tasks_by_background ON tasks (background, id)"]}]"##;

#[test]
fn statements_are_byte_identical_to_the_reference() {
    let golden: serde_json::Value =
        serde_json::from_str(TS_SQLITE_MIGRATIONS).expect("golden migrations json");
    let golden = golden.as_array().expect("golden array");
    assert_eq!(golden.len(), SQLITE_MIGRATIONS.len(), "migration count");
    for (migration_index, migration) in SQLITE_MIGRATIONS.iter().enumerate() {
        let expected = &golden[migration_index];
        assert_eq!(
            expected["version"].as_i64(),
            Some(migration.version),
            "migration {migration_index} version"
        );
        let statements = expected["statements"].as_array().expect("statements");
        assert_eq!(
            statements.len(),
            migration.statements.len(),
            "migration {migration_index} statement count"
        );
        for (statement_index, statement) in migration.statements.iter().enumerate() {
            assert_eq!(
                statements[statement_index].as_str(),
                Some(statement.as_str()),
                "migration {migration_index} statement {statement_index}"
            );
        }
    }
}
