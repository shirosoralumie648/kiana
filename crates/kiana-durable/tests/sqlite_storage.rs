//! `packages/durable/test/sqlite-storage.test.ts` 的移植。
//!
//! 需要真实 SQLite：`cargo test -p kiana-durable --features sqlite --test sqlite_storage`。
//!
//! 覆盖：
//! - 共享的 `registerStorageConformance`（26 条）跑在 `SqliteStorage` 上，以及每条用例
//!   提交后都关闭/重开存储的 `ReopeningStorage` 版本；
//! - TS 特有的持久化矩阵：重开后的记录/序号/ID 分配、前缀命名空间、非法前缀、
//!   元数据损坏、缺失 base、文档 delta 回放与损坏操作、事务回滚、历史点重建、
//!   索引使用、current-only 修订回收、WAL 自动 checkpoint 与关闭截断、页复用、体积上限。
//!
//! # 与 TS 的差异（测试手段，不是行为）
//! - TS 的 “rolls SQL rows and sequence allocation back” 用循环 JSON 让 `JSON.stringify`
//!   在写入中途抛错；Rust 的 `JsonValue` 无法表示循环结构，改用同样发生在第一个表写入
//!   **之后**、元数据更新之前的失败（复制一个不存在的文档来源），断言不变：行与序号一起
//!   回滚、失败批次不可见、后续提交从原序号继续。
//! - `DatabaseSync` 的只读检查连接在 Rust 里用同一 `NodeSqliteDatabase` 门面打开（不写）。

#![cfg(feature = "sqlite")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_chord::delta::{Op, Seg};
use kiana_chord::sqlite::database::{SqliteDatabase, SqliteExecutor, SqliteValue};
use serde_json::{Value, json};

use kiana_durable::storage::sqlite::node::{
    NodeSqliteDatabase, NodeSqliteStorageOptions, open_node_sqlite_database,
    open_node_sqlite_storage,
};
use kiana_durable::testing::assertions::PanicAssertions;
use kiana_durable::testing::types::StorageUse;
use kiana_durable::testing::{StorageConformanceOptions, create_storage_conformance};
use kiana_durable::{
    AdoptFn, BoxError, ConversationId, ConversationQuery, ConversationRecord, Cursor,
    DocumentAddress, DocumentBaseContent, DocumentContent, DocumentCreate, DocumentId,
    DocumentPoint, DocumentQuery, DocumentRecord, DocumentScope, EntryId, EntryQuery, EntryRecord,
    IdKind, JsonObject, Page, ROOT_CONVERSATION_ID, Seq, Storage, StorageWrite, StoredDocument,
    StoredEntry, SubmissionId, SubmissionQuery, SubmissionRecord, TaskId, TaskQuery, TaskRecord,
    id_from_number,
};

// ---------------------------------------------------------------------------
// 通用辅助
// ---------------------------------------------------------------------------

fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

/// 存储层提交没有内存 owner 需要 adopt。
fn adopt() -> AdoptFn {
    Box::new(|_seq: Seq| {})
}

fn ok<T, E: std::fmt::Display>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{error}"),
    }
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
            "kiana-durable-sqlite-{tag}-{}-{nanos}-{counter}",
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

fn object(value: Value) -> JsonObject {
    match value {
        Value::Object(map) => map.into_iter().collect(),
        other => panic!("expected a JSON object, got {other}"),
    }
}

/// 保存 `SqliteStorage` 便于在测试里调用内部观察点（`next_id`）。
struct SqliteStorageHandle {
    storage: kiana_durable::storage::sqlite::SqliteStorage,
}

impl std::ops::Deref for SqliteStorageHandle {
    type Target = kiana_durable::storage::sqlite::SqliteStorage;
    fn deref(&self) -> &Self::Target {
        &self.storage
    }
}

async fn create_sqlite_storage(
    options: NodeSqliteStorageOptions,
) -> (SqliteStorageHandle, TempDir) {
    let directory = TempDir::new("storage");
    let storage = ok(open_node_sqlite_storage(&directory.join("storage.sqlite"), options).await);
    (SqliteStorageHandle { storage }, directory)
}

async fn commit(storage: &dyn Storage, writes: &[StorageWrite]) -> Seq {
    ok(storage.commit(writes, adopt(), context()).await)
}

// ---------------------------------------------------------------------------
// 写入构造函数（对应 TS 的对象字面量）
// ---------------------------------------------------------------------------

fn entry(id: EntryId, conversation_id: ConversationId, data: Option<Value>) -> EntryRecord {
    EntryRecord {
        model: None,
        data,
        edits: None,
        kind: "message".to_owned(),
        id,
        conversation_id,
        head: None,
        by_task_id: None,
    }
}

async fn create_root(storage: &dyn Storage) -> Seq {
    commit(
        storage,
        &[StorageWrite::Conversation {
            value: ConversationRecord {
                id: ROOT_CONVERSATION_ID,
                parent: None,
                owner: None,
            },
        }],
    )
    .await
}

fn pending_task(id: TaskId) -> TaskRecord {
    serde_json::from_value(json!({
        "id": id.get(),
        "conversationId": ROOT_CONVERSATION_ID.get(),
        "kind": "test.task",
        "version": 1,
        "input": null,
        "state": { "status": "pending", "checkpoint": { "phase": "ready" } },
        "background": false,
        "abortRequested": false,
    }))
    .expect("pending task record")
}

fn session_document(id: DocumentId, kind: &str) -> DocumentCreate {
    DocumentCreate {
        id,
        kind: kind.to_owned(),
        key: None,
        scope: DocumentScope::Session,
        history: None,
        fork: None,
    }
}

fn base_value(version: i64, value: Value) -> DocumentBaseContent {
    DocumentBaseContent::new(version, object(value))
}

fn doc_create(record: DocumentCreate, content: DocumentBaseContent) -> StorageWrite {
    StorageWrite::DocumentCreate { record, content }
}

fn doc_change(id: DocumentId, content: DocumentContent) -> StorageWrite {
    StorageWrite::DocumentChange { id, content }
}

fn doc_retire(id: DocumentId) -> StorageWrite {
    StorageWrite::DocumentRetire { id }
}

async fn mint(storage: &dyn Storage, kind: IdKind) -> i64 {
    ok(storage.mint_id(kind).await)
}

// ---------------------------------------------------------------------------
// 原始 SQLite 观察
// ---------------------------------------------------------------------------

async fn open_database(path: &str) -> NodeSqliteDatabase {
    ok(open_node_sqlite_database(path, Default::default()).await)
}

/// `scalar(db, "SELECT ... AS value ...")`。
async fn scalar(database: &dyn SqliteExecutor, sql: &str) -> i64 {
    let row = database
        .get(sql, &[])
        .await
        .expect("scalar query")
        .expect("row");
    row.get_i64("value").expect("value column")
}

async fn revision_count(path: &str, document_id: DocumentId) -> i64 {
    let database = open_database(path).await;
    let count = database
        .get(
            "SELECT count(*) AS count FROM document_revisions WHERE document_id = ?",
            &[SqliteValue::Integer(document_id.get())],
        )
        .await
        .expect("count revisions")
        .expect("row")
        .get_i64("count")
        .expect("count column");
    database.close().await.expect("close observer");
    count
}

// ---------------------------------------------------------------------------
// ReopeningStorage（TS 同名包装器）
// ---------------------------------------------------------------------------

/// 每次 `commit` 之后关闭并重开存储，其余方法转发到当前实例。
struct ReopeningStorage {
    current: tokio::sync::Mutex<kiana_durable::storage::sqlite::SqliteStorage>,
    path: String,
    options: NodeSqliteStorageOptions,
    closed: AtomicBool,
}

impl ReopeningStorage {
    fn new(
        storage: kiana_durable::storage::sqlite::SqliteStorage,
        path: String,
        options: NodeSqliteStorageOptions,
    ) -> Self {
        ReopeningStorage {
            current: tokio::sync::Mutex::new(storage),
            path,
            options,
            closed: AtomicBool::new(false),
        }
    }

    async fn current(
        &self,
    ) -> tokio::sync::MutexGuard<'_, kiana_durable::storage::sqlite::SqliteStorage> {
        self.current.lock().await
    }
}

#[async_trait]
impl Storage for ReopeningStorage {
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: AdoptFn,
        _context: &dyn Context,
    ) -> Result<Seq, BoxError> {
        let mut current = self.current().await;
        if self.closed.load(Ordering::SeqCst) {
            return current.commit(writes, adopt, context()).await;
        }
        let result = current.commit(writes, adopt, context()).await;
        current.close(context()).await;
        let reopened = open_node_sqlite_storage(&self.path, self.options.clone()).await?;
        *current = reopened;
        result
    }

    async fn mint_id(&self, kind: IdKind) -> Result<i64, BoxError> {
        self.current().await.mint_id(kind).await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        self.current().await.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        self.current()
            .await
            .scan_conversations(query, limit, cursor, context)
            .await
    }

    async fn entry(
        &self,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.current().await.entry(id, context).await
    }

    async fn visible_entry(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.current()
            .await
            .visible_entry(conversation_id, id, context)
            .await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError> {
        self.current()
            .await
            .find_latest_head_marker(conversation_id, at_or_before_entry_id, context)
            .await
    }

    async fn scan_entries(
        &self,
        query: &EntryQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<EntryRecord>, BoxError> {
        self.current()
            .await
            .scan_entries(query, limit, cursor, context)
            .await
    }

    async fn task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        self.current().await.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<TaskRecord>, BoxError> {
        self.current()
            .await
            .scan_tasks(query, limit, cursor, context)
            .await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.current().await.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: &SubmissionQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<SubmissionRecord>, BoxError> {
        self.current()
            .await
            .scan_submissions(query, limit, cursor, context)
            .await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.current()
            .await
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: &DocumentAddress,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError> {
        self.current()
            .await
            .find_document(address, at, context)
            .await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError> {
        self.current().await.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: &DocumentQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<DocumentRecord>, BoxError> {
        self.current()
            .await
            .scan_documents(query, limit, cursor, context)
            .await
    }

    async fn close(&self, context: &dyn Context) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.current().await.close(context).await;
    }
}

// ---------------------------------------------------------------------------
// 共享 conformance
// ---------------------------------------------------------------------------

fn conformance_options(across_reopen: bool) -> StorageConformanceOptions {
    StorageConformanceOptions {
        assertions: Arc::new(PanicAssertions),
        with_storage: Arc::new(move |run: StorageUse| {
            Box::pin(async move {
                let directory = TempDir::new(if across_reopen {
                    "conformance-reopen"
                } else {
                    "conformance"
                });
                let path = directory.join("storage.sqlite");
                if across_reopen {
                    let created = open_storage_at(&path).await;
                    created.close(context()).await;
                    let wrapper = Arc::new(ReopeningStorage::new(
                        open_storage_at(&path).await,
                        path,
                        NodeSqliteStorageOptions::default(),
                    ));
                    let storage: Arc<dyn Storage> = wrapper.clone();
                    run(storage).await;
                    wrapper.close(context()).await;
                } else {
                    let storage: Arc<dyn Storage> = Arc::new(open_storage_at(&path).await);
                    let closing = Arc::clone(&storage);
                    run(storage).await;
                    closing.close(context()).await;
                }
            })
        }),
    }
}

async fn open_storage_at(path: &str) -> kiana_durable::storage::sqlite::SqliteStorage {
    ok(open_node_sqlite_storage(path, NodeSqliteStorageOptions::default()).await)
}

/// `registerStorageConformance(..., "SqliteStorage", ...)`。
#[tokio::test]
async fn passes_the_shared_storage_conformance_suite() {
    let cases = create_storage_conformance(&conformance_options(false));
    assert_eq!(cases.len(), 26);
    for case in cases {
        println!("SqliteStorage conformance case: {}", case.name);
        (case.run)().await;
    }
}

/// `registerStorageConformance(..., "SqliteStorage across reopen", ...)`。
#[tokio::test]
async fn passes_the_shared_storage_conformance_suite_across_reopen() {
    let cases = create_storage_conformance(&conformance_options(true));
    assert_eq!(cases.len(), 26);
    for case in cases {
        println!(
            "SqliteStorage across reopen conformance case: {}",
            case.name
        );
        (case.run)().await;
    }
}

// ---------------------------------------------------------------------------
// TS 特有的持久化用例
// ---------------------------------------------------------------------------

#[tokio::test]
async fn persists_records_sequence_allocation_and_global_id_allocation_across_reopen() {
    let (handle, directory) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    let path = directory.join("storage.sqlite");
    let storage: &dyn Storage = &*handle;
    assert_eq!(create_root(storage).await.get(), 1);
    let entry_id: EntryId = id_from_number(mint(storage, IdKind::Entry).await);
    assert_eq!(
        commit(
            storage,
            &[StorageWrite::Entry {
                value: entry(entry_id, ROOT_CONVERSATION_ID, None),
            }],
        )
        .await
        .get(),
        2
    );
    storage.close(context()).await;

    let reopened = open_storage_at(&path).await;
    let restored = ok(reopened.entry(entry_id, context()).await).expect("entry");
    assert_eq!(
        restored,
        StoredEntry {
            entry: entry(entry_id, ROOT_CONVERSATION_ID, None),
            commit_seq: kiana_durable::types::seq_from_number(2),
        }
    );
    let next: EntryId = id_from_number(ok(reopened.mint_id(IdKind::Entry).await));
    assert_eq!(next.get(), entry_id.get() + 1);
    let task_id: TaskId = id_from_number(ok(reopened.mint_id(IdKind::Task).await));
    assert_eq!(
        ok(reopened
            .commit(
                &[StorageWrite::Task {
                    value: pending_task(task_id),
                }],
                adopt(),
                context(),
            )
            .await)
        .get(),
        3
    );
    reopened.close(context()).await;
}

#[tokio::test]
async fn namespaces_tables_and_indexes_with_a_prefix_across_reopen() {
    let directory = TempDir::new("prefix");
    let path = directory.join("storage.sqlite");
    let host = open_database(&path).await;
    host.exec("CREATE TABLE tasks (text TEXT)")
        .await
        .expect("host table");
    host.close().await.expect("close host");

    let storage = ok(open_node_sqlite_storage(
        &path,
        NodeSqliteStorageOptions {
            prefix: Some("kiana_".to_owned()),
            ..Default::default()
        },
    )
    .await);
    assert_eq!(create_root(&storage).await.get(), 1);
    storage.close(context()).await;

    let reopened = ok(open_node_sqlite_storage(
        &path,
        NodeSqliteStorageOptions {
            prefix: Some("kiana_".to_owned()),
            ..Default::default()
        },
    )
    .await);
    assert_eq!(
        ok(reopened.conversation(ROOT_CONVERSATION_ID, context()).await),
        Some(ConversationRecord {
            id: ROOT_CONVERSATION_ID,
            parent: None,
            owner: None,
        })
    );
    let database = open_database(&path).await;
    let names: Vec<String> = ok(database
        .all(
            "SELECT name FROM sqlite_master WHERE type IN ('table', 'index') AND name NOT LIKE 'sqlite_%'",
            &[],
        )
        .await)
        .iter()
        .map(|row| row.get_text("name").expect("name").to_owned())
        .collect();
    let unprefixed: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| !name.starts_with("kiana_"))
        .collect();
    assert_eq!(unprefixed, ["tasks"]);
    assert!(names.iter().any(|name| name == "kiana_tasks"));
    database.close().await.expect("close observer");
    reopened.close(context()).await;
}

#[tokio::test]
async fn rejects_an_invalid_prefix_before_creating_the_database() {
    // TS 的 `/^[A-Za-z_][A-Za-z0-9_]*$/` 没有 `m` 标志：`$` 只在输入末尾匹配，尾随换行**不是**
    // 语法例外（node 实测 `"kiana_\n"`、`"kiana_\r"`、U+2028、U+2029 都被拒绝），Rust 的逐字符
    // 校验与之一致。
    for prefix in [
        "bad-prefix",
        "kiana_\n",
        "kiana_\r",
        "kiana_\u{2028}",
        "kiana_\u{2029}",
    ] {
        let directory = TempDir::new("bad-prefix");
        let database_directory = directory.join("nested");
        let failure = open_node_sqlite_storage(
            &format!("{database_directory}/storage.sqlite"),
            NodeSqliteStorageOptions {
                prefix: Some(prefix.to_owned()),
                ..Default::default()
            },
        )
        .await
        .expect_err("invalid prefix is rejected");
        assert!(
            failure
                .to_string()
                .starts_with("Invalid SQLite schema prefix "),
            "unexpected message for {prefix:?}: {failure}"
        );
        assert!(
            std::fs::metadata(&database_directory).is_err(),
            "the database directory must not be created"
        );
    }
}

#[tokio::test]
async fn rejects_persisted_metadata_corruption_on_reopen() {
    let (handle, directory) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    let path = directory.join("storage.sqlite");
    handle.close(context()).await;
    let database = open_database(&path).await;
    database
        .exec("DELETE FROM durable_metadata")
        .await
        .expect("corrupt metadata");
    database.close().await.expect("close");

    let failure = open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
        .await
        .expect_err("corrupt metadata is rejected");
    assert_eq!(failure.to_string(), "Durable SQLite metadata is missing");
}

#[tokio::test]
async fn rejects_a_document_whose_required_base_is_missing() {
    let (handle, directory) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    let path = directory.join("storage.sqlite");
    let storage: &dyn Storage = &*handle;
    create_root(storage).await;
    let id: DocumentId = id_from_number(mint(storage, IdKind::Document).await);
    commit(
        storage,
        &[doc_create(
            session_document(id, "corrupt"),
            base_value(1, json!({ "retained": true })),
        )],
    )
    .await;
    let database = open_database(&path).await;
    database
        .run(
            "DELETE FROM document_revisions WHERE document_id = ?",
            &[SqliteValue::Integer(id.get())],
        )
        .await
        .expect("delete revisions");
    database.close().await.expect("close");

    let failure = storage
        .document(id, DocumentPoint::Current, context())
        .await
        .expect_err("missing base is rejected");
    assert_eq!(
        failure.to_string(),
        format!("Document {id} is missing a required base")
    );
    // 走事务读：错误后存储仍可用，且下一次读仍可正常执行。
    assert!(
        storage
            .task(id_from_number(999), context())
            .await
            .expect("task read")
            .is_none()
    );
    handle.close(context()).await;
}

#[tokio::test]
async fn replays_detached_root_replacements_and_follow_up_edits_while_rejecting_corrupt_operations()
{
    let (handle, directory) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    let path = directory.join("storage.sqlite");
    let storage: &dyn Storage = &*handle;
    create_root(storage).await;
    let id: DocumentId = id_from_number(mint(storage, IdKind::Document).await);
    commit(
        storage,
        &[doc_create(
            session_document(id, "replay"),
            base_value(1, json!({ "nested": { "value": 1 }, "rows": [] })),
        )],
    )
    .await;
    commit(
        storage,
        &[doc_change(
            id,
            DocumentContent::delta(
                1,
                decode_ops(&[json!(["r", { "nested": { "value": 2 }, "rows": [{ "id": 1 }] }])]),
            ),
        )],
    )
    .await;
    commit(
        storage,
        &[doc_change(
            id,
            DocumentContent::delta(
                1,
                decode_ops(&[
                    json!(["s", ["nested", "value"], 3]),
                    json!(["p", ["rows"], 1, 0, [{ "id": 2 }]]),
                    json!(["m", ["rows"], [1, 0]]),
                ]),
            ),
        )],
    )
    .await;
    commit(
        storage,
        &[doc_change(
            id,
            DocumentContent::delta(1, decode_ops(&[json!(["s", ["nested", "value"], 4])])),
        )],
    )
    .await;

    let expected = json!({ "nested": { "value": 4 }, "rows": [{ "id": 2 }, { "id": 1 }] });
    let first = ok(storage
        .document(id, DocumentPoint::Current, context())
        .await)
    .expect("document");
    assert_eq!(first.value, object(expected.clone()));
    // 返回的记录是脱离式副本（模拟 SQLite 编解码边界）：改它不影响下一次读取。
    let mut detached = first.value.clone();
    if let Some(Value::Object(nested)) = detached.get_mut("nested") {
        nested.insert("value".to_owned(), json!(99));
    }
    if let Some(Value::Array(rows)) = detached.get_mut("rows")
        && let Some(Value::Object(row)) = rows.get_mut(0)
    {
        row.insert("id".to_owned(), json!(99));
    }
    let second = ok(storage
        .document(id, DocumentPoint::Current, context())
        .await)
    .expect("document");
    assert_eq!(second.value, object(expected));

    let database = open_database(&path).await;
    database
        .run(
            "UPDATE document_revisions SET content = ? WHERE document_id = ? AND seq = (SELECT max(seq) FROM document_revisions WHERE document_id = ?)",
            &[
                SqliteValue::Text("[[\"unknown\"]]".to_owned()),
                SqliteValue::Integer(id.get()),
                SqliteValue::Integer(id.get()),
            ],
        )
        .await
        .expect("corrupt revision");
    database.close().await.expect("close");

    let failure = storage
        .document(id, DocumentPoint::Current, context())
        .await
        .expect_err("corrupt operations are rejected");
    assert!(
        failure.to_string().contains("unknown op verb"),
        "unexpected message: {failure}"
    );
    handle.close(context()).await;
}

fn decode_ops(wire: &[Value]) -> Vec<Op> {
    kiana_chord::delta::index::decode(wire).expect("valid op wire")
}

/// 覆盖最新 revision 的 `content` 列（模拟损坏或外部写入的库）。
async fn corrupt_latest_revision(path: &str, document_id: DocumentId, content: &str) {
    let database = open_database(path).await;
    database
        .run(
            "UPDATE document_revisions SET content = ? WHERE document_id = ? AND seq = (SELECT max(seq) FROM document_revisions WHERE document_id = ?)",
            &[
                SqliteValue::Text(content.to_owned()),
                SqliteValue::Integer(document_id.get()),
                SqliteValue::Integer(document_id.get()),
            ],
        )
        .await
        .expect("corrupt revision");
    database.close().await.expect("close observer");
}

/// TS 对 revision 的 `content` 是 `apply(value, JSON.parse(content) as Op[])`：
/// `JSON.parse` 之后直接 `for...of`，因此错误取决于 JSON 形状——数组逐条 `assertValidOp`
/// （`TypeError` 文案），字符串逐码点迭代（每个非空字符都不是元组），其余不可迭代。
#[tokio::test]
async fn applies_revision_content_like_the_reference_and_reports_its_errors() {
    let (handle, directory) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    let path = directory.join("storage.sqlite");
    let storage: &dyn Storage = &*handle;
    create_root(storage).await;
    let id: DocumentId = id_from_number(mint(storage, IdKind::Document).await);
    commit(
        storage,
        &[doc_create(
            session_document(id, "content"),
            base_value(1, json!({ "value": 1 })),
        )],
    )
    .await;
    commit(
        storage,
        &[doc_change(
            id,
            DocumentContent::delta(1, decode_ops(&[json!(["s", ["value"], 2])])),
        )],
    )
    .await;

    // JSON 字符串：V8 逐码点迭代，字符不是元组。
    corrupt_latest_revision(&path, id, "\"ab\"").await;
    let failure = storage
        .document(id, DocumentPoint::Current, context())
        .await
        .expect_err("a string revision is rejected");
    assert_eq!(failure.to_string(), "op is not a tuple");
    assert_eq!(
        failure
            .downcast_ref::<kiana_chord::delta::index::OpError>()
            .map(kiana_chord::delta::index::OpError::name),
        Some("TypeError"),
        "assertValidOp throws a TypeError"
    );

    // 空字符串：零次迭代，等于空 op 批次（TS 静默得到 base 的值）。
    corrupt_latest_revision(&path, id, "\"\"").await;
    let stored = ok(storage
        .document(id, DocumentPoint::Current, context())
        .await)
    .expect("document");
    assert_eq!(stored.value, object(json!({ "value": 1 })));
    assert_eq!(stored.deltas_since_base, 1);

    // 非可迭代的 JSON 值：数字/对象/布尔/null 都报 V8 的固定文案。
    for content in ["5", "{}", "true", "null"] {
        corrupt_latest_revision(&path, id, content).await;
        let failure = storage
            .document(id, DocumentPoint::Current, context())
            .await
            .expect_err("a non-iterable revision is rejected");
        assert_eq!(failure.to_string(), "ops is not iterable", "{content}");
    }

    // 畸形 op 元组：用已解码语法的 assertValidOp 文案（wire 短形式在这里是非法元组）。
    for (content, message) in [
        ("[[\"s\",\"x\"]]", "s arity"),
        ("[[]]", "op is not a tuple"),
        ("[null]", "op is not a tuple"),
        ("[[\"bogus\"]]", "unknown op verb: bogus"),
    ] {
        corrupt_latest_revision(&path, id, content).await;
        let failure = storage
            .document(id, DocumentPoint::Current, context())
            .await
            .expect_err("a malformed operation is rejected");
        assert_eq!(failure.to_string(), message, "{content}");
    }

    // 根被替换成非对象：TS 只做 `as JsonObject` 断言并原样返回；Rust 的
    // `StoredDocument.value: JsonObject` 无法表示，显式报错（冻结类型的已知偏差）。
    corrupt_latest_revision(&path, id, "[[\"r\",[1,2]]]").await;
    let failure = storage
        .document(id, DocumentPoint::Current, context())
        .await
        .expect_err("a non-object root is rejected");
    assert_eq!(
        failure.to_string(),
        format!("Document {id} materialized a non-object value")
    );

    handle.close(context()).await;
}

/// revision `content` 落盘为 `Op[]` 的 JSON 序列化：已解码的元组、内联路径，**没有** wire
/// 的短形式/`#` 引用。这是当前稳定契约（改动需同步测试），因为读取方会按 `assertValidOp`
/// 校验并拒绝压缩形式。
/// TS 源：`JSON.stringify(Op[])`。
#[tokio::test]
async fn stores_decoded_operations_like_the_reference() {
    let (handle, directory) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    let path = directory.join("storage.sqlite");
    let storage: &dyn Storage = &*handle;
    create_root(storage).await;
    let id: DocumentId = id_from_number(mint(storage, IdKind::Document).await);
    commit(
        storage,
        &[doc_create(
            session_document(id, "encoded"),
            base_value(1, json!({ "count": 0 })),
        )],
    )
    .await;
    // 同一路径连续两条 op：wire 编码会压成 `["s",2]`，TS 存的是两条完整元组。
    commit(
        storage,
        &[doc_change(
            id,
            DocumentContent::delta(
                1,
                vec![
                    Op::Set(vec![Seg::Key("count".to_owned())], json!(1)),
                    Op::Set(vec![Seg::Key("count".to_owned())], json!(2)),
                ],
            ),
        )],
    )
    .await;

    let database = open_database(&path).await;
    let content = database
        .get(
            "SELECT content FROM document_revisions WHERE document_id = ? AND kind = 'delta' ORDER BY seq DESC LIMIT 1",
            &[SqliteValue::Integer(id.get())],
        )
        .await
        .expect("read delta")
        .expect("delta row")
        .get_text("content")
        .expect("content")
        .to_owned();
    database.close().await.expect("close observer");
    assert_eq!(
        content, "[[\"s\",[\"count\"],1],[\"s\",[\"count\"],2]]",
        "revision content must be JSON.stringify(Op[])"
    );

    let stored = ok(storage
        .document(id, DocumentPoint::Current, context())
        .await)
    .expect("document");
    assert_eq!(stored.value, object(json!({ "count": 2 })));
    handle.close(context()).await;
}

/// `JSON.stringify(undefined)` 是 `undefined`，node:sqlite 把它绑成 NULL，随后 `content TEXT
/// NOT NULL` 约束失败。Rust 侧同样绑定 NULL，让驱动产生同一错误（而不是自造一条文案）。
#[tokio::test]
async fn fails_the_commit_like_the_driver_when_revision_content_is_missing() {
    let (handle, _directory) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    let storage: &dyn Storage = &*handle;
    create_root(storage).await;
    let id: DocumentId = id_from_number(mint(storage, IdKind::Document).await);
    commit(
        storage,
        &[doc_create(
            session_document(id, "missing"),
            base_value(1, json!({ "value": 1 })),
        )],
    )
    .await;

    for content in [
        DocumentContent {
            version: 1,
            kind: kiana_durable::DocumentContentKind::Delta,
            value: None,
            ops: None,
        },
        DocumentContent {
            version: 1,
            kind: kiana_durable::DocumentContentKind::Base,
            value: None,
            ops: None,
        },
    ] {
        let failure = storage
            .commit(&[doc_change(id, content)], adopt(), context())
            .await
            .expect_err("missing content is rejected by the driver");
        assert!(
            failure
                .to_string()
                .contains("NOT NULL constraint failed: document_revisions.content"),
            "unexpected message: {failure}"
        );
    }

    // 两次失败都回滚：base 仍在，值没有被改动。
    let stored = ok(storage
        .document(id, DocumentPoint::Current, context())
        .await)
    .expect("document");
    assert_eq!(stored.value, object(json!({ "value": 1 })));
    assert_eq!(stored.deltas_since_base, 0);
    handle.close(context()).await;
}

#[tokio::test]
async fn rolls_sql_rows_and_sequence_allocation_back_as_one_transaction() {
    // TS 用循环 JSON 让编码在第一个表写入之后失败；Rust 的 JSON 值不能循环，改用复制一个
    // 不存在的文档来源——同样发生在第一个表写入之后、元数据更新之前。
    let (handle, directory) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    let path = directory.join("storage.sqlite");
    let storage: &dyn Storage = &*handle;
    create_root(storage).await;
    let transient_id: EntryId = id_from_number(mint(storage, IdKind::Entry).await);
    let document_id: DocumentId = id_from_number(mint(storage, IdKind::Document).await);
    let failure = storage
        .commit(
            &[
                StorageWrite::Entry {
                    value: entry(transient_id, ROOT_CONVERSATION_ID, None),
                },
                StorageWrite::DocumentCopy {
                    record: DocumentCreate {
                        id: document_id,
                        kind: "copy".to_owned(),
                        key: None,
                        scope: DocumentScope::Conversation {
                            conversation_id: ROOT_CONVERSATION_ID,
                        },
                        history: Some(kiana_durable::types::DocumentHistory::Rewindable),
                        fork: Some(kiana_durable::types::DocumentFork::AsOf),
                    },
                    source: kiana_durable::types::DocumentCopySource {
                        id: id_from_number(document_id.get() + 100),
                        at: DocumentPoint::Current,
                    },
                },
            ],
            adopt(),
            context(),
        )
        .await
        .expect_err("the batch fails");
    assert!(
        failure.to_string().contains("was rejected"),
        "unexpected message: {failure}"
    );
    assert!(ok(storage.entry(transient_id, context()).await).is_none());
    assert!(
        ok(storage
            .document(document_id, DocumentPoint::Current, context())
            .await)
        .is_none()
    );
    let committed_id: EntryId = id_from_number(mint(storage, IdKind::Entry).await);
    assert_eq!(
        commit(
            storage,
            &[StorageWrite::Entry {
                value: entry(committed_id, ROOT_CONVERSATION_ID, None),
            }],
        )
        .await
        .get(),
        2
    );

    let database = open_database(&path).await;
    assert_eq!(
        scalar(&database, "SELECT count(*) AS value FROM entries").await,
        1
    );
    assert_eq!(
        scalar(
            &database,
            "SELECT next_seq AS value FROM durable_metadata WHERE singleton = 1",
        )
        .await,
        3
    );
    database.close().await.expect("close");
    handle.close(context()).await;
}

#[tokio::test]
async fn reconstructs_recent_and_ancient_rewindable_points_after_reopen() {
    let (handle, directory) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    let path = directory.join("storage.sqlite");
    let storage: &dyn Storage = &*handle;
    create_root(storage).await;
    let id: DocumentId = id_from_number(mint(storage, IdKind::Document).await);
    let record = DocumentCreate {
        id,
        kind: "history".to_owned(),
        key: None,
        scope: DocumentScope::Conversation {
            conversation_id: ROOT_CONVERSATION_ID,
        },
        history: Some(kiana_durable::types::DocumentHistory::Rewindable),
        fork: Some(kiana_durable::types::DocumentFork::AsOf),
    };
    let created_at = commit(
        storage,
        &[doc_create(record, base_value(1, json!({ "count": 0 })))],
    )
    .await;
    let mut ancient_at = created_at;
    let mut recent_at = created_at;
    for count in 1..=40i64 {
        let content = if count == 20 {
            DocumentContent::base(1, object(json!({ "count": count })))
        } else {
            DocumentContent::delta(
                1,
                vec![Op::Set(vec![Seg::Key("count".to_owned())], json!(count))],
            )
        };
        recent_at = commit(storage, &[doc_change(id, content)]).await;
        if count == 5 {
            ancient_at = recent_at;
        }
    }
    storage.close(context()).await;

    let reopened = open_storage_at(&path).await;
    let ancient = ok(reopened
        .document(id, DocumentPoint::At(ancient_at), context())
        .await)
    .expect("ancient point");
    assert_eq!(ancient.value, object(json!({ "count": 5 })));
    let recent = ok(reopened
        .document(id, DocumentPoint::At(recent_at), context())
        .await)
    .expect("recent point");
    assert_eq!(recent.value, object(json!({ "count": 40 })));
    reopened.close(context()).await;
}

#[tokio::test]
async fn uses_indexes_for_exact_addresses_exact_scopes_entry_history_and_document_revision_tails() {
    let (handle, directory) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    let path = directory.join("storage.sqlite");
    handle.close(context()).await;
    let database = open_database(&path).await;
    let int = SqliteValue::Integer;
    let text = |value: &str| SqliteValue::Text(value.to_owned());
    let plans: Vec<String> = vec![
        plan(
            &database,
            "EXPLAIN QUERY PLAN SELECT record FROM documents\n\
             \t\t\t\t\t\tWHERE kind = ? AND scope_kind = ? AND owner_id = ? AND family = ? AND key_value = ?\n\
             \t\t\t\t\t\tAND retired_at IS NULL ORDER BY created_at DESC LIMIT 1",
            &[text("kind"), text("session"), int(0), int(0), text("")],
        )
        .await,
        plan(
            &database,
            "EXPLAIN QUERY PLAN SELECT record FROM documents\n\
             \t\t\t\t\t\tWHERE kind = ? AND scope_kind = ? AND owner_id = ? AND family = ? AND key_value = ?\n\
             \t\t\t\t\t\tAND created_at <= ? AND (retired_at IS NULL OR retired_at > ?)\n\
             \t\t\t\t\t\tORDER BY created_at DESC LIMIT 1",
            &[
                text("kind"),
                text("conversation"),
                int(1),
                int(0),
                text(""),
                int(10),
                int(10),
            ],
        )
        .await,
        plan(
            &database,
            "EXPLAIN QUERY PLAN SELECT record FROM documents\n\
             \t\t\t\t\t\tWHERE scope_kind = ? AND owner_id = ? AND kind = ? AND id > ? ORDER BY id LIMIT ?",
            &[text("task"), int(1), text("kind"), int(0), int(10)],
        )
        .await,
        plan(
            &database,
            "EXPLAIN QUERY PLAN SELECT record FROM entries\n\
             \t\t\t\t\t\tWHERE conversation_id = ? AND id <= ? ORDER BY id DESC LIMIT ?",
            &[int(1), int(10), int(10)],
        )
        .await,
        plan(
            &database,
            "EXPLAIN QUERY PLAN SELECT record FROM entries\n\
             \t\t\t\t\t\tWHERE conversation_id = ? AND head IS NOT NULL AND id <= ? ORDER BY id DESC LIMIT 1",
            &[int(1), int(10)],
        )
        .await,
        plan(
            &database,
            "EXPLAIN QUERY PLAN SELECT record FROM tasks WHERE status = ? AND id > ? ORDER BY id LIMIT ?",
            &[text("pending"), int(0), int(10)],
        )
        .await,
        plan(
            &database,
            "EXPLAIN QUERY PLAN SELECT seq, kind, version, content FROM document_revisions\n\
             \t\t\t\t\t\tWHERE document_id = ? AND kind = 'base' AND seq <= ? ORDER BY seq DESC LIMIT 1",
            &[int(1), int(10)],
        )
        .await,
        plan(
            &database,
            "EXPLAIN QUERY PLAN SELECT seq, kind, version, content FROM document_revisions\n\
             \t\t\t\t\t\tWHERE document_id = ? AND seq > ? AND seq <= ? ORDER BY seq",
            &[int(1), int(5), int(10)],
        )
        .await,
    ];
    assert!(
        plans[0].contains("documents_by_address"),
        "plan 0: {}",
        plans[0]
    );
    assert!(
        plans[1].contains("documents_by_address"),
        "plan 1: {}",
        plans[1]
    );
    assert!(
        plans[2].contains("documents_by_scope_kind"),
        "plan 2: {}",
        plans[2]
    );
    assert!(
        plans[3].contains("entries_by_conversation"),
        "plan 3: {}",
        plans[3]
    );
    assert!(
        plans[4].contains("entry_heads_by_conversation"),
        "plan 4: {}",
        plans[4]
    );
    assert!(plans[5].contains("tasks_by_status"), "plan 5: {}", plans[5]);
    assert!(
        plans[6].contains("document_revisions_by_kind"),
        "plan 6: {}",
        plans[6]
    );
    assert!(
        plans[7].contains("sqlite_autoindex_document_revisions_1"),
        "plan 7: {}",
        plans[7]
    );
    for detail in &plans[..3] {
        assert!(
            !detail.contains("SCAN documents"),
            "unexpected scan: {detail}"
        );
    }
    assert!(
        !plans[7].contains("SCAN document_revisions"),
        "unexpected scan: {}",
        plans[7]
    );
    database.close().await.expect("close");
}

async fn plan(database: &dyn SqliteExecutor, sql: &str, params: &[SqliteValue]) -> String {
    database
        .all(sql, params)
        .await
        .expect("explain")
        .iter()
        .map(|row| row.get_text("detail").unwrap_or("").to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn reclaims_current_only_revisions_only_after_a_base_or_retirement() {
    let (handle, directory) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    let path = directory.join("storage.sqlite");
    let storage: &dyn Storage = &*handle;
    create_root(storage).await;
    let id: DocumentId = id_from_number(mint(storage, IdKind::Document).await);
    commit(
        storage,
        &[doc_create(
            session_document(id, "latest"),
            base_value(1, json!({ "count": 0 })),
        )],
    )
    .await;
    for count in 1..=10i64 {
        commit(
            storage,
            &[doc_change(
                id,
                DocumentContent::delta(
                    1,
                    vec![Op::Set(vec![Seg::Key("count".to_owned())], json!(count))],
                ),
            )],
        )
        .await;
    }
    assert_eq!(revision_count(&path, id).await, 11);
    let database = open_database(&path).await;
    let content = database
        .get(
            "SELECT content FROM document_revisions WHERE document_id = ? AND kind = 'delta' ORDER BY seq DESC LIMIT 1",
            &[SqliteValue::Integer(id.get())],
        )
        .await
        .expect("read delta")
        .expect("delta row")
        .get_text("content")
        .expect("content")
        .to_owned();
    assert_eq!(
        serde_json::from_str::<Value>(&content).expect("delta json"),
        json!([["s", ["count"], 10]])
    );
    database.close().await.expect("close");

    commit(
        storage,
        &[doc_change(
            id,
            DocumentContent::base(1, object(json!({ "count": 11 }))),
        )],
    )
    .await;
    assert_eq!(revision_count(&path, id).await, 1);
    commit(
        storage,
        &[doc_change(
            id,
            DocumentContent::delta(
                1,
                vec![Op::Set(vec![Seg::Key("count".to_owned())], json!(12))],
            ),
        )],
    )
    .await;
    assert_eq!(revision_count(&path, id).await, 2);
    commit(storage, &[doc_retire(id)]).await;
    assert_eq!(revision_count(&path, id).await, 0);
    handle.close(context()).await;
}

#[tokio::test]
async fn auto_checkpoints_wal_frames_and_truncates_the_wal_on_close() {
    let (handle, directory) = create_sqlite_storage(NodeSqliteStorageOptions {
        wal_auto_checkpoint_pages: Some(1),
        ..Default::default()
    })
    .await;
    let path = directory.join("storage.sqlite");
    let storage: &dyn Storage = &*handle;
    create_root(storage).await;
    for index in 0..20i64 {
        let entry_id: EntryId = id_from_number(mint(storage, IdKind::Entry).await);
        commit(
            storage,
            &[StorageWrite::Entry {
                value: entry(
                    entry_id,
                    ROOT_CONVERSATION_ID,
                    Some(json!({ "text": "x".repeat(32 * 1024), "index": index })),
                ),
            }],
        )
        .await;
    }
    let wal_path = format!("{path}-wal");
    let wal_size = std::fs::metadata(&wal_path).expect("wal file").len();
    assert!(wal_size < 512 * 1024, "wal too large: {wal_size}");
    let observer = open_database(&path).await;
    assert_eq!(
        scalar(&observer, "SELECT count(*) AS value FROM entries").await,
        20
    );
    handle.close(context()).await;
    assert_eq!(std::fs::metadata(&wal_path).expect("wal file").len(), 0);
    observer.close().await.expect("close observer");
}

#[tokio::test]
async fn reuses_pages_released_by_current_only_checkpoints() {
    let (handle, directory) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    let path = directory.join("storage.sqlite");
    let storage: &dyn Storage = &*handle;
    create_root(storage).await;
    let id: DocumentId = id_from_number(mint(storage, IdKind::Document).await);
    let large = "x".repeat(512 * 1024);
    commit(
        storage,
        &[doc_create(
            session_document(id, "reuse"),
            base_value(1, json!({ "text": large })),
        )],
    )
    .await;
    commit(
        storage,
        &[doc_change(
            id,
            DocumentContent::base(1, object(json!({ "text": "small" }))),
        )],
    )
    .await;
    let before = open_database(&path).await;
    let pages_after_delete = scalar(
        &before,
        "SELECT page_count AS value FROM pragma_page_count() ",
    )
    .await;
    let free_after_delete = scalar(
        &before,
        "SELECT freelist_count AS value FROM pragma_freelist_count() ",
    )
    .await;
    before.close().await.expect("close before");
    assert!(free_after_delete > 0);

    commit(
        storage,
        &[doc_change(
            id,
            DocumentContent::base(1, object(json!({ "text": large }))),
        )],
    )
    .await;
    let after = open_database(&path).await;
    let pages_after_reuse = scalar(
        &after,
        "SELECT page_count AS value FROM pragma_page_count() ",
    )
    .await;
    let free_after_reuse = scalar(
        &after,
        "SELECT freelist_count AS value FROM pragma_freelist_count() ",
    )
    .await;
    after.close().await.expect("close after");
    assert!(
        pages_after_reuse <= pages_after_delete + 2,
        "pages: {pages_after_reuse} vs {pages_after_delete}"
    );
    assert!(
        free_after_reuse < free_after_delete,
        "freelist: {free_after_reuse} vs {free_after_delete}"
    );
    handle.close(context()).await;
}

#[tokio::test]
async fn keeps_representative_row_and_document_storage_bounded() {
    let (handle, directory) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    let path = directory.join("storage.sqlite");
    let storage: &dyn Storage = &*handle;
    create_root(storage).await;
    for index in 0..100i64 {
        let entry_id: EntryId = id_from_number(mint(storage, IdKind::Entry).await);
        commit(
            storage,
            &[StorageWrite::Entry {
                value: entry(
                    entry_id,
                    ROOT_CONVERSATION_ID,
                    Some(json!({ "index": index, "text": "x".repeat(1024) })),
                ),
            }],
        )
        .await;
    }
    let document_id: DocumentId = id_from_number(mint(storage, IdKind::Document).await);
    let record = DocumentCreate {
        id: document_id,
        kind: "size.history".to_owned(),
        key: None,
        scope: DocumentScope::Conversation {
            conversation_id: ROOT_CONVERSATION_ID,
        },
        history: Some(kiana_durable::types::DocumentHistory::Rewindable),
        fork: Some(kiana_durable::types::DocumentFork::AsOf),
    };
    commit(
        storage,
        &[doc_create(record, base_value(1, json!({ "count": 0 })))],
    )
    .await;
    for count in 1..=100i64 {
        commit(
            storage,
            &[doc_change(
                document_id,
                DocumentContent::delta(
                    1,
                    vec![Op::Set(vec![Seg::Key("count".to_owned())], json!(count))],
                ),
            )],
        )
        .await;
    }
    handle.close(context()).await;
    let size = std::fs::metadata(&path).expect("database file").len();
    assert!(size < 1024 * 1024, "database too large: {size}");
}
