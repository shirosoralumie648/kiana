//! `packages/durable/test/sqlite-facade.test.ts` 的移植。
//!
//! 需要真实 SQLite：`cargo test -p kiana-durable --features sqlite --test sqlite_facade`。
//!
//! TS 用例针对的是“可移植 SQLite 门面的结算/排序语义”，它们既是 store 的契约也是
//! `SqliteStorage` 的契约。Rust 侧用同样形状的包装器复现：
//! - `GatedDatabase`：在指定查询上、或在下一个事务回调开始前暂停（TS 用未结算的 Promise
//!   达到同一目的）；
//! - `ControlledSettlementDatabase`：事务的结算可被测试显式放行/拒绝。
//!
//! # 已知差异（测试手段，不是行为）
//! - TS 的 “prepares each storage statement once per connection” 直接覆写
//!   `DatabaseSync.prepare` 并统计次数。Rust 的 `kiana_chord::sqlite::node::NodeSqliteDatabase`
//!   不暴露 prepare 计数（内部用 `prepare_cached`），无法从外部观测；这里改为执行同一批
//!   提交/读取并断言结果，语句缓存的复用由 chord 驱动的缓存实现保证。
//! - TS 的类再导出身份检查（`toBe`）在 Rust 里是类型同一性：把再导出的驱动器/打开器
//!   赋给按 chord 类型书写的函数指针即可在编译期验证。

#![cfg(feature = "sqlite")]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_chord::sqlite::database::{
    ErasedSqliteValue, SqliteBoxFuture, SqliteDatabase, SqliteError, SqliteExecutor, SqliteRow,
    SqliteValue, TransactionCallback,
};
use kiana_chord::sqlite::node::NodeSqliteDatabase;
use serde_json::json;
use tokio::sync::oneshot;

use kiana_durable::storage::sqlite::node::open_node_sqlite_database;
use kiana_durable::storage::sqlite::{SqliteStorage, SqliteStorageOptions};
use kiana_durable::{
    AdoptFn, ConversationRecord, DocumentContent, DocumentId, DocumentPoint, DocumentScope,
    EntryId, EntryQuery, EntryRecord, IdKind, ROOT_CONVERSATION_ID, Seq, Storage, StorageWrite,
    id_from_number,
};

fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

fn adopt() -> AdoptFn {
    Box::new(|_seq: Seq| {})
}

fn ok<T, E: std::fmt::Display>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{error}"),
    }
}

async fn open_memory() -> SqliteStorage {
    let database = ok(open_node_sqlite_database(":memory:", Default::default()).await);
    ok(SqliteStorage::open(Arc::new(database), SqliteStorageOptions::default()).await)
}

fn entry_write(id: EntryId, kind: &str) -> StorageWrite {
    StorageWrite::Entry {
        value: EntryRecord {
            model: None,
            data: None,
            edits: None,
            kind: kind.to_owned(),
            id,
            conversation_id: ROOT_CONVERSATION_ID,
            head: None,
            by_task_id: None,
        },
    }
}

// ---------------------------------------------------------------------------
// 再导出身份
// ---------------------------------------------------------------------------

#[test]
fn re_exports_the_chord_driver_and_node_opener_without_wrapping() {
    // 同一类型：把再导出的类型传回 chord 的类型即可编译。
    fn same_driver(
        database: kiana_chord::sqlite::node::NodeSqliteDatabase,
    ) -> kiana_durable::storage::sqlite::node::NodeSqliteDatabase {
        database
    }
    let _ = same_driver as fn(kiana_chord::sqlite::node::NodeSqliteDatabase) -> NodeSqliteDatabase;
    // 同一个函数项：按 chord 的签名书写的函数指针可以直接接收再导出的打开器。
    let opener: fn(
        &str,
        kiana_chord::sqlite::node::NodeSqliteDatabaseOptions,
    ) -> SqliteBoxFuture<
        'static,
        Result<kiana_chord::sqlite::node::NodeSqliteDatabase, SqliteError>,
    > = open_node_sqlite_database;
    let _ = opener;
}

// ---------------------------------------------------------------------------
// 语句复用（行为等价；见文件头的已知差异）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reuses_storage_statements_across_transactions() {
    let storage = open_memory().await;
    storage
        .commit(
            &[StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            adopt(),
            context(),
        )
        .await
        .expect("root commit");
    let writes: Vec<StorageWrite> = (0..100)
        .map(|index| entry_write(id_from_number(index + 2), "cached"))
        .collect();
    storage
        .commit(&writes, adopt(), context())
        .await
        .expect("bulk entries");
    storage
        .commit(
            &[entry_write(id_from_number(102), "cached-again")],
            adopt(),
            context(),
        )
        .await
        .expect("later entry");
    assert_eq!(
        ok(storage.entry(id_from_number(2), context()).await)
            .expect("entry")
            .entry
            .kind,
        "cached"
    );
    assert_eq!(
        ok(storage.entry(id_from_number(102), context()).await)
            .expect("entry")
            .entry
            .kind,
        "cached-again"
    );
    let failure = storage
        .commit(
            &[StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            adopt(),
            context(),
        )
        .await
        .expect_err("duplicate root is rejected");
    assert_eq!(failure.to_string(), "ID 1 already belongs to conversation");
    assert_eq!(
        ok(storage.entry(id_from_number(2), context()).await)
            .expect("entry")
            .entry
            .kind,
        "cached"
    );
    storage.close(context()).await;
}

// ---------------------------------------------------------------------------
// 调用顺序
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reads_storage_commits_in_call_order() {
    // TS 里 `storage.commit(...)` 在调用时就排队；Rust 的 future 是惰性的，因此先让提交进入
    // 事务（第一次 poll），再发起读。受控结算把提交**结果**的交付推迟到测试放行之后：读必须
    // 观察已经落盘的写入，即使提交的 promise 还没有结算。
    let database = Arc::new(ControlledSettlementDatabase::new(ok(
        open_node_sqlite_database(":memory:", Default::default()).await,
    )));
    let storage = ok(SqliteStorage::open(
        Arc::clone(&database) as Arc<dyn SqliteDatabase>,
        SqliteStorageOptions::default(),
    )
    .await);
    database.control_next_settlement(SettlementMode::Delay);
    let conversation = [StorageWrite::Conversation {
        value: ConversationRecord {
            id: ROOT_CONVERSATION_ID,
            parent: None,
            owner: None,
        },
    }];
    let mut commit = Box::pin(storage.commit(&conversation, adopt(), context()));
    assert!(
        futures::poll!(commit.as_mut()).is_pending(),
        "the commit settles only when the test releases it"
    );
    // 提交调用于读之前：读必须观察它的写入，即使提交的结果还没有交付。
    let read = storage.conversation(ROOT_CONVERSATION_ID, context()).await;
    assert_eq!(
        ok(read),
        Some(ConversationRecord {
            id: ROOT_CONVERSATION_ID,
            parent: None,
            owner: None,
        })
    );
    assert!(
        futures::poll!(commit.as_mut()).is_pending(),
        "the commit result is still pending"
    );
    database.settle();
    ok(commit.await);
    storage.close(context()).await;
}

// ---------------------------------------------------------------------------
// 接纳读与关闭
// ---------------------------------------------------------------------------

/// 在包含 `gate_sql` 的 `get` 上暂停，直到测试放行（TS 的未结算 Promise）。
struct GatedDatabase {
    inner: NodeSqliteDatabase,
    gate_sql: &'static str,
    gate: Mutex<Option<oneshot::Receiver<()>>>,
}

impl GatedDatabase {
    fn new(inner: NodeSqliteDatabase, gate_sql: &'static str) -> Self {
        GatedDatabase {
            inner,
            gate_sql,
            gate: Mutex::new(None),
        }
    }

    fn arm(&self, receiver: oneshot::Receiver<()>) {
        *self.gate.lock().expect("gate") = Some(receiver);
    }
}

impl SqliteExecutor for GatedDatabase {
    fn exec<'a>(&'a self, sql: &'a str) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
        self.inner.exec(sql)
    }

    fn run<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
        self.inner.run(sql, params)
    }

    fn get<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<Option<SqliteRow>, SqliteError>> {
        let receiver = if sql.contains(self.gate_sql) {
            self.gate.lock().expect("gate").take()
        } else {
            None
        };
        Box::pin(async move {
            if let Some(receiver) = receiver {
                let _ = receiver.await;
            }
            self.inner.get(sql, params).await
        })
    }

    fn all<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<Vec<SqliteRow>, SqliteError>> {
        let receiver = if sql.contains(self.gate_sql) {
            self.gate.lock().expect("gate").take()
        } else {
            None
        };
        Box::pin(async move {
            if let Some(receiver) = receiver {
                let _ = receiver.await;
            }
            self.inner.all(sql, params).await
        })
    }
}

impl SqliteDatabase for GatedDatabase {
    fn transaction<'a>(
        &'a self,
        callback: TransactionCallback<'a>,
    ) -> SqliteBoxFuture<'a, Result<ErasedSqliteValue, SqliteError>> {
        // 事务内的门控：在回调开始之前暂停（事务已 BEGIN，队列轮次仍由它持有）。
        let receiver = self.gate.lock().expect("gate").take();
        let Some(receiver) = receiver else {
            return self.inner.transaction(callback);
        };
        let gated: TransactionCallback<'a> = Box::new(move |transaction| {
            Box::pin(async move {
                let _ = receiver.await;
                callback(transaction).await
            })
        });
        self.inner.transaction(gated)
    }

    fn close(&self) -> SqliteBoxFuture<'_, Result<(), SqliteError>> {
        self.inner.close()
    }
}

#[tokio::test]
async fn lets_admitted_multi_query_reads_finish_before_storage_closes() {
    let database = Arc::new(GatedDatabase::new(
        ok(open_node_sqlite_database(":memory:", Default::default()).await),
        "SELECT record FROM entries WHERE conversation_id = ?",
    ));
    let storage = ok(SqliteStorage::open(
        Arc::clone(&database) as Arc<dyn SqliteDatabase>,
        SqliteStorageOptions::default(),
    )
    .await);
    let entry_id: EntryId = id_from_number(2);
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
                entry_write(entry_id, "probe"),
            ],
            adopt(),
            context(),
        )
        .await
        .expect("seed");
    let (sender, receiver) = oneshot::channel();
    database.arm(receiver);
    let mut scan = Box::pin(storage.scan_entries(
        &EntryQuery {
            conversation_id: ROOT_CONVERSATION_ID,
            min_entry_id: None,
            max_entry_id: None,
            order: None,
        },
        10,
        None,
        context(),
    ));
    // 驱动到被门控的查询：每次轮询后让出调度器，让被门控的查询有机会推进到下一个 await 点。
    let mut polls = 0;
    let mut scan_poll = std::task::Poll::Pending;
    while polls < 64 && !database.gate.lock().expect("gate").is_none() {
        scan_poll = futures::poll!(scan.as_mut());
        if scan_poll.is_ready() {
            break;
        }
        tokio::task::yield_now().await;
        polls += 1;
    }
    assert!(
        database.gate.lock().expect("gate").is_none(),
        "the scan reached the gated query"
    );
    assert!(
        scan_poll.is_pending(),
        "the gated read is admitted but not settled"
    );
    let mut closed = Box::pin(storage.close(context()));
    assert!(
        futures::poll!(closed.as_mut()).is_pending(),
        "close waits for admitted reads"
    );
    sender.send(()).expect("release the gate");
    let page = ok(scan.await);
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.id.get())
            .collect::<Vec<_>>(),
        vec![entry_id.get()]
    );
    closed.await;
    let failure = storage
        .scan_entries(
            &EntryQuery {
                conversation_id: ROOT_CONVERSATION_ID,
                min_entry_id: None,
                max_entry_id: None,
                order: None,
            },
            10,
            None,
            context(),
        )
        .await
        .expect_err("reads after close fail");
    assert_eq!(failure.to_string(), "SqliteStorage is closed");
}

// ---------------------------------------------------------------------------
// 关闭失败
// ---------------------------------------------------------------------------

/// 关闭时失败的数据库：TS 的 `close()` 以驱动错误 rejection，且 `this.closing` 缓存同一个
/// 已 rejection 的 promise。
struct FailingCloseDatabase {
    inner: NodeSqliteDatabase,
    closes: AtomicU32,
}

impl FailingCloseDatabase {
    fn new(inner: NodeSqliteDatabase) -> Self {
        FailingCloseDatabase {
            inner,
            closes: AtomicU32::new(0),
        }
    }

    fn close_count(&self) -> u32 {
        self.closes.load(Ordering::SeqCst)
    }
}

impl SqliteExecutor for FailingCloseDatabase {
    fn exec<'a>(&'a self, sql: &'a str) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
        self.inner.exec(sql)
    }

    fn run<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
        self.inner.run(sql, params)
    }

    fn get<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<Option<SqliteRow>, SqliteError>> {
        self.inner.get(sql, params)
    }

    fn all<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<Vec<SqliteRow>, SqliteError>> {
        self.inner.all(sql, params)
    }
}

impl SqliteDatabase for FailingCloseDatabase {
    fn transaction<'a>(
        &'a self,
        callback: TransactionCallback<'a>,
    ) -> SqliteBoxFuture<'a, Result<ErasedSqliteValue, SqliteError>> {
        self.inner.transaction(callback)
    }

    fn close(&self) -> SqliteBoxFuture<'_, Result<(), SqliteError>> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(SqliteError::new("close failed")) })
    }
}

#[tokio::test]
async fn reports_a_driver_close_failure_and_caches_it_like_the_reference() {
    let database = Arc::new(FailingCloseDatabase::new(ok(open_node_sqlite_database(
        ":memory:",
        Default::default(),
    )
    .await)));
    let storage = ok(SqliteStorage::open(
        Arc::clone(&database) as Arc<dyn SqliteDatabase>,
        SqliteStorageOptions::default(),
    )
    .await);

    let first = storage
        .close_result(context())
        .await
        .expect_err("the first close rejects with the driver error");
    assert_eq!(first.to_string(), "close failed");
    // 冻结的 `Storage::close` 没有 rejection 的位置：它完成关闭并吞掉错误。
    storage.close(context()).await;
    // 后续关闭返回同一个缓存的 rejection（TS 的 `this.closing`）。
    let repeated = storage
        .close_result(context())
        .await
        .expect_err("later closes reject the same cached failure");
    assert_eq!(repeated.to_string(), "close failed");
    assert_eq!(database.close_count(), 1, "the driver is closed once");
    let failure = storage
        .conversation(ROOT_CONVERSATION_ID, context())
        .await
        .expect_err("a closed storage rejects every read");
    assert_eq!(failure.to_string(), "SqliteStorage is closed");
}

// ---------------------------------------------------------------------------
// 受控结算（TS: ControlledSettlementDatabase）
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettlementMode {
    Immediate,
    Delay,
    Reject,
}

struct SettlementState {
    mode: SettlementMode,
    pending: Option<oneshot::Sender<()>>,
}

/// 把事务的结算交给测试显式放行；`reject` 在回调成功之后抛错（因此事务回滚）。
struct ControlledSettlementDatabase {
    inner: NodeSqliteDatabase,
    state: Mutex<SettlementState>,
}

impl ControlledSettlementDatabase {
    fn new(inner: NodeSqliteDatabase) -> Self {
        ControlledSettlementDatabase {
            inner,
            state: Mutex::new(SettlementState {
                mode: SettlementMode::Immediate,
                pending: None,
            }),
        }
    }

    fn control_next_settlement(&self, mode: SettlementMode) {
        let mut state = self.state.lock().expect("settlement state");
        assert!(state.pending.is_none(), "A settlement is already pending");
        state.mode = mode;
    }

    fn settle(&self) {
        let sender = self
            .state
            .lock()
            .expect("settlement state")
            .pending
            .take()
            .expect("No settlement is pending");
        let _ = sender.send(());
    }
}

impl SqliteExecutor for ControlledSettlementDatabase {
    fn exec<'a>(&'a self, sql: &'a str) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
        self.inner.exec(sql)
    }

    fn run<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
        self.inner.run(sql, params)
    }

    fn get<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<Option<SqliteRow>, SqliteError>> {
        self.inner.get(sql, params)
    }

    fn all<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<Vec<SqliteRow>, SqliteError>> {
        self.inner.all(sql, params)
    }
}

impl SqliteDatabase for ControlledSettlementDatabase {
    fn transaction<'a>(
        &'a self,
        callback: TransactionCallback<'a>,
    ) -> SqliteBoxFuture<'a, Result<ErasedSqliteValue, SqliteError>> {
        let mode = {
            let mut state = self.state.lock().expect("settlement state");
            let mode = state.mode;
            state.mode = SettlementMode::Immediate;
            mode
        };
        if mode == SettlementMode::Immediate {
            return self.inner.transaction(callback);
        }
        let (sender, receiver) = oneshot::channel();
        self.state.lock().expect("settlement state").pending = Some(sender);
        let callback: TransactionCallback<'a> = if mode == SettlementMode::Reject {
            Box::new(move |transaction| {
                Box::pin(async move {
                    callback(transaction).await?;
                    Err(SqliteError::new("controlled settlement rejection"))
                })
            })
        } else {
            callback
        };
        let settlement = self.inner.transaction(callback);
        Box::pin(async move {
            let result = settlement.await;
            let _ = receiver.await;
            result
        })
    }

    fn close(&self) -> SqliteBoxFuture<'_, Result<(), SqliteError>> {
        self.inner.close()
    }
}

#[tokio::test]
async fn awaits_async_transaction_settlement_and_adopts_ids_only_after_success() {
    let database = Arc::new(ControlledSettlementDatabase::new(ok(
        open_node_sqlite_database(":memory:", Default::default()).await,
    )));
    database.control_next_settlement(SettlementMode::Delay);
    let mut opening = Box::pin(SqliteStorage::open(
        Arc::clone(&database) as Arc<dyn SqliteDatabase>,
        SqliteStorageOptions::default(),
    ));
    assert!(
        futures::poll!(opening.as_mut()).is_pending(),
        "initialization waits for the delayed settlement"
    );
    database.settle();
    let storage = ok(opening.await);

    database.control_next_settlement(SettlementMode::Delay);
    let settled_writes = [entry_write(id_from_number(100), "settled")];
    let mut committing = Box::pin(storage.commit(&settled_writes, adopt(), context()));
    assert!(
        futures::poll!(committing.as_mut()).is_pending(),
        "commit waits for settlement"
    );
    assert_eq!(ok(storage.mint_id(IdKind::Entry).await), 2);
    database.settle();
    assert_eq!(ok(committing.await).get(), 1);
    assert_eq!(ok(storage.mint_id(IdKind::Entry).await), 101);

    database.control_next_settlement(SettlementMode::Reject);
    let rejected_writes = [entry_write(id_from_number(200), "rejected")];
    let mut rejected = Box::pin(storage.commit(&rejected_writes, adopt(), context()));
    assert!(futures::poll!(rejected.as_mut()).is_pending());
    assert_eq!(ok(storage.mint_id(IdKind::Entry).await), 102);
    database.settle();
    let failure = rejected.await.expect_err("the settlement rejects");
    assert_eq!(failure.to_string(), "controlled settlement rejection");
    assert_eq!(ok(storage.mint_id(IdKind::Entry).await), 103);
    assert!(ok(storage.entry(id_from_number(200), context()).await).is_none());
    storage.close(context()).await;
}

// ---------------------------------------------------------------------------
// 文档读的事务快照
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reads_a_document_from_one_committed_state_while_a_commit_replaces_its_base() {
    let id: DocumentId = id_from_number::<kiana_durable::types::DocumentBrand>(5);
    let database = Arc::new(GatedDatabase::new(
        ok(open_node_sqlite_database(":memory:", Default::default()).await),
        // 该用例的门控在事务回调开始处（而不是某条查询）：读取事务必须在替换提交之前结算。
        "SELECT record FROM documents WHERE id = ?",
    ));
    let storage = ok(SqliteStorage::open(
        Arc::clone(&database) as Arc<dyn SqliteDatabase>,
        SqliteStorageOptions::default(),
    )
    .await);
    storage
        .commit(
            &[StorageWrite::DocumentCreate {
                record: kiana_durable::types::DocumentCreate {
                    id,
                    kind: "replaced".to_owned(),
                    key: None,
                    scope: DocumentScope::Session,
                    history: None,
                    fork: None,
                },
                content: kiana_durable::types::DocumentBaseContent::new(
                    1,
                    match json!({ "value": 1 }) {
                        serde_json::Value::Object(map) => map.into_iter().collect(),
                        _ => unreachable!(),
                    },
                ),
            }],
            adopt(),
            context(),
        )
        .await
        .expect("create document");

    // 让读取事务在回调开始处暂停：此时它已持有队列轮次（TS 里 `document()` 同步入队的等价物）。
    let (sender, receiver) = oneshot::channel();
    database.arm(receiver);
    let mut read = Box::pin(storage.document(id, DocumentPoint::Current, context()));
    let mut polls = 0;
    let mut read_poll = std::task::Poll::Pending;
    while polls < 64 && !database.gate.lock().expect("gate").is_none() {
        read_poll = futures::poll!(read.as_mut());
        if read_poll.is_ready() {
            break;
        }
        tokio::task::yield_now().await;
        polls += 1;
    }
    assert!(
        database.gate.lock().expect("gate").is_none(),
        "the read entered its transaction"
    );
    assert!(
        read_poll.is_pending(),
        "the read is held inside its transaction"
    );

    // 读取还在事务中时发起替换：它必须排在读取事务之后，不能插入读取的记录/修订查询之间。
    let replacement = [StorageWrite::DocumentChange {
        id,
        content: DocumentContent::base(
            1,
            match json!({ "value": 2 }) {
                serde_json::Value::Object(map) => map.into_iter().collect(),
                _ => unreachable!(),
            },
        ),
    }];
    let mut replace = Box::pin(storage.commit(&replacement, adopt(), context()));
    assert!(
        futures::poll!(replace.as_mut()).is_pending(),
        "the replacement waits for the read's transaction"
    );

    sender.send(()).expect("release the gate");
    let stored = ok(read.await).expect("document");
    let value = stored
        .value
        .get("value")
        .and_then(serde_json::Value::as_i64)
        .expect("value");
    assert_eq!(
        value, 1,
        "a document read must see the state committed before the replacement"
    );
    let replaced = ok(replace.await);
    assert_eq!(replaced.get(), 2);
    let after = ok(storage
        .document(id, DocumentPoint::Current, context())
        .await)
    .expect("document after replacement");
    assert_eq!(
        after.value.get("value").and_then(serde_json::Value::as_i64),
        Some(2)
    );
    storage.close(context()).await;
}
