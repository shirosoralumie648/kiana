//! 移植自 `packages/durable/test/memory-storage.test.ts` 与共享的
//! `packages/durable/src/testing/storage-conformance.ts`（26 个 conformance 用例）。
//!
//! TS 的 `memory-storage.test.ts` 只是 `registerStorageConformance(...)` 的薄调用者，另外加
//! 一条 prepared-commit 用例。共享注册器 `create_storage_conformance` 由 `testing` 单元移植；
//! 本单元的工作副本里它还是空 stub（编译期无法调用），因此这里把 26 个用例**逐条落地**成独立
//! `#[tokio::test]`，用例标题同时写在函数名与 `///` 注释里。合并后若 `testing` 单元提供
//! `create_storage_conformance`，这些用例可以替换为循环（行为一致，不会重复失败）。
//!
//! 无法在 Rust 表达的部分（`Object.freeze` 的抛错、原型、孤立代理项字符串）在用例内注明。

use std::sync::{Arc, Mutex};

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_chord::delta::{Op, Seg};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::{
    BoxError, ConversationId, ConversationOwner, ConversationParent, ConversationQuery,
    ConversationRecord, Cursor, DocumentAddress, DocumentBaseContent, DocumentContent,
    DocumentContentKind, DocumentCopySource, DocumentCreate, DocumentFork, DocumentHistory,
    DocumentId, DocumentPoint, DocumentQuery, DocumentScope, EntryId, EntryQuery, EntryRecord, Id,
    IdBrand, IdKind, JoinPolicy, JsonObject, JsonValue, Page, ROOT_CONVERSATION_ID, ScanOrder,
    Storage, StorageWrite, SubmissionId, SubmissionKind, SubmissionQuery, SubmissionRecord,
    SubmissionStatus, TaskId, TaskOutcome, TaskOutcomeError, TaskQuery, TaskRecord, TaskState,
    TaskStatus, id_from_number, seq_from_number,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// 共享辅助（对应 storage-conformance.ts 的常量与工厂函数）
// ---------------------------------------------------------------------------

fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

/// 存储层提交没有内存 owner 需要 adopt。
fn adopt_nothing() -> kiana_durable::AdoptFn {
    Box::new(|_| {})
}

fn message_of<T: std::fmt::Debug>(result: Result<T, BoxError>) -> String {
    match result {
        Ok(value) => panic!("expected an error, got {value:?}"),
        Err(error) => error.to_string(),
    }
}

fn assert_error_contains<T: std::fmt::Debug>(result: Result<T, BoxError>, needle: &str) {
    let message = message_of(result);
    assert!(
        message.contains(needle),
        "expected error {message:?} to contain {needle:?}"
    );
}

fn object(value: JsonValue) -> JsonObject {
    match value {
        JsonValue::Object(map) => map.into_iter().collect(),
        other => panic!("expected a JSON object, got {other}"),
    }
}

async fn create_root(storage: &MemoryStorage) -> ConversationId {
    storage
        .commit(
            &[StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("create the root conversation");
    ROOT_CONVERSATION_ID
}

async fn mint<K: IdBrand>(storage: &MemoryStorage, kind: IdKind) -> Id<K> {
    id_from_number(storage.mint_id(kind).await.expect("mint an ID"))
}

fn conversation(id: ConversationId) -> StorageWrite {
    StorageWrite::Conversation {
        value: ConversationRecord {
            id,
            parent: None,
            owner: None,
        },
    }
}

fn conversation_with(
    id: ConversationId,
    parent: Option<ConversationParent>,
    owner: Option<ConversationOwner>,
) -> StorageWrite {
    StorageWrite::Conversation {
        value: ConversationRecord { id, parent, owner },
    }
}

fn entry_record(id: EntryId, conversation_id: ConversationId, kind: &str) -> EntryRecord {
    EntryRecord {
        key_order: None,
        model: None,
        data: None,
        edits: None,
        kind: kind.to_owned(),
        id,
        conversation_id,
        head: None,
        by_task_id: None,
    }
}

fn entry_record_with(
    id: EntryId,
    conversation_id: ConversationId,
    kind: &str,
    data: Option<JsonValue>,
    head: Option<EntryId>,
) -> EntryRecord {
    EntryRecord {
        head,
        data,
        ..entry_record(id, conversation_id, kind)
    }
}

fn entry_write(record: EntryRecord) -> StorageWrite {
    StorageWrite::Entry { value: record }
}

fn entry(id: EntryId, conversation_id: ConversationId, kind: &str) -> StorageWrite {
    entry_write(entry_record(id, conversation_id, kind))
}

fn pending_task(id: TaskId, conversation_id: ConversationId) -> TaskRecord {
    pending_task_with(id, conversation_id, "ready")
}

fn pending_task_with(id: TaskId, conversation_id: ConversationId, phase: &str) -> TaskRecord {
    TaskRecord {
        id,
        conversation_id,
        kind: "test.task".to_owned(),
        version: 1,
        input: json!({ "value": id.get() }),
        trace: None,
        links: None,
        owner: None,
        background: false,
        abort_requested: false,
        abandon_on_restart: None,
        state: TaskState::Pending {
            checkpoint: json!({ "phase": phase }),
        },
        started_at: None,
        ended_at: None,
        restarts: None,
        abort_reason: None,
        memos: None,
    }
}

fn task_write(value: TaskRecord) -> StorageWrite {
    StorageWrite::Task { value }
}

fn submission(
    id: SubmissionId,
    conversation_id: ConversationId,
    request_id: Option<&str>,
    kind: SubmissionKind,
    status: SubmissionStatus,
) -> SubmissionRecord {
    SubmissionRecord {
        key_order: None,
        conversation_id,
        request_id: request_id.map(str::to_owned),
        trace: None,
        kind,
        status,
        entry: None,
        id,
        answer: None,
        reason: None,
        detail: None,
    }
}

fn submission_write(value: SubmissionRecord) -> StorageWrite {
    StorageWrite::Submission { value }
}

fn doc_record(id: DocumentId, kind: &str, scope: DocumentScope) -> DocumentCreate {
    DocumentCreate {
        id,
        kind: kind.to_owned(),
        key: None,
        scope,
        history: None,
        fork: None,
    }
}

fn doc_record_full(
    id: DocumentId,
    kind: &str,
    scope: DocumentScope,
    key: Option<JsonValue>,
    history: Option<DocumentHistory>,
    fork: Option<DocumentFork>,
) -> DocumentCreate {
    DocumentCreate {
        key,
        history,
        fork,
        ..doc_record(id, kind, scope)
    }
}

fn doc_create(record: DocumentCreate, version: i64, value: JsonObject) -> StorageWrite {
    StorageWrite::DocumentCreate {
        record,
        content: DocumentBaseContent::new(version, value),
    }
}

fn doc_change(id: DocumentId, content: DocumentContent) -> StorageWrite {
    StorageWrite::DocumentChange { id, content }
}

fn doc_delta(id: DocumentId, version: i64, ops: Vec<Op>) -> StorageWrite {
    doc_change(id, DocumentContent::delta(version, ops))
}

fn doc_base_change(id: DocumentId, version: i64, value: JsonObject) -> StorageWrite {
    doc_change(id, DocumentContent::base(version, value))
}

fn doc_retire(id: DocumentId) -> StorageWrite {
    StorageWrite::DocumentRetire { id }
}

fn conversation_scope(conversation_id: ConversationId) -> DocumentScope {
    DocumentScope::Conversation { conversation_id }
}

fn session_scope() -> DocumentScope {
    DocumentScope::Session
}

fn round_trip(cursor: &Cursor) -> Cursor {
    serde_json::from_str(&serde_json::to_string(cursor).expect("cursor serializes"))
        .expect("cursor round-trips")
}

fn ids_of_conversations(page: &Page<ConversationRecord>) -> Vec<i64> {
    page.items.iter().map(|item| item.id.get()).collect()
}

fn ids_of_entries(page: &Page<EntryRecord>) -> Vec<i64> {
    page.items.iter().map(|item| item.id.get()).collect()
}

fn ids_of_tasks(page: &Page<TaskRecord>) -> Vec<i64> {
    page.items.iter().map(|item| item.id.get()).collect()
}

fn ids_of_submissions(page: &Page<SubmissionRecord>) -> Vec<i64> {
    page.items.iter().map(|item| item.id.get()).collect()
}

fn ids_of_documents(page: &Page<kiana_durable::DocumentRecord>) -> Vec<i64> {
    page.items.iter().map(|item| item.id.get()).collect()
}

/// 扫描器种类（用例 11 对三种表做同样的翻页检查）。
#[derive(Clone, Copy)]
enum Scanner {
    Conversations,
    Tasks,
    Submissions,
}

async fn scan_ids(
    storage: &MemoryStorage,
    scanner: Scanner,
    order: Option<ScanOrder>,
    cursor: Option<&Cursor>,
) -> Result<(Vec<i64>, Option<Cursor>), BoxError> {
    match scanner {
        Scanner::Conversations => {
            let query = ConversationQuery {
                owner_conversation_id: None,
                owner_task_id: None,
                order,
            };
            let page = storage
                .scan_conversations(&query, 2, cursor, context())
                .await?;
            Ok((ids_of_conversations(&page), page.next))
        }
        Scanner::Tasks => {
            let query = TaskQuery {
                order,
                ..TaskQuery::default()
            };
            let page = storage.scan_tasks(&query, 2, cursor, context()).await?;
            Ok((ids_of_tasks(&page), page.next))
        }
        Scanner::Submissions => {
            let query = SubmissionQuery {
                order,
                ..SubmissionQuery::default()
            };
            let page = storage
                .scan_submissions(&query, 2, cursor, context())
                .await?;
            Ok((ids_of_submissions(&page), page.next))
        }
    }
}

async fn collect_all(
    storage: &MemoryStorage,
    scanner: Scanner,
    order: Option<ScanOrder>,
) -> Vec<i64> {
    let mut found = Vec::new();
    let (mut page_ids, mut next) = scan_ids(storage, scanner, order, None)
        .await
        .expect("scan page");
    found.append(&mut page_ids);
    while let Some(cursor) = next {
        let cursor = round_trip(&cursor);
        let (mut page_ids, following) = scan_ids(storage, scanner, None, Some(&cursor))
            .await
            .expect("scan continuation");
        found.append(&mut page_ids);
        next = following;
    }
    found
}

async fn scan_entry_ids(
    storage: &MemoryStorage,
    query: &EntryQuery,
    limit: usize,
    cursor: Option<&Cursor>,
) -> Result<(Vec<i64>, Option<Cursor>), BoxError> {
    let page = storage
        .scan_entries(query, limit, cursor, context())
        .await?;
    Ok((ids_of_entries(&page), page.next))
}

// ---------------------------------------------------------------------------
// conformance 用例
// ---------------------------------------------------------------------------

/// "reserves ID 1 for the immutable root conversation"
#[tokio::test]
async fn reserves_id_1_for_the_immutable_root_conversation() {
    let storage = MemoryStorage::new();
    assert_eq!(
        mint::<kiana_durable::ConversationBrand>(&storage, IdKind::Conversation)
            .await
            .get(),
        2
    );
    assert_eq!(create_root(&storage).await, ROOT_CONVERSATION_ID);
    assert_eq!(
        storage
            .conversation(ROOT_CONVERSATION_ID, context())
            .await
            .expect("read the root"),
        Some(ConversationRecord {
            id: ROOT_CONVERSATION_ID,
            parent: None,
            owner: None,
        })
    );
    assert_error_contains(
        storage
            .commit(
                &[conversation(ROOT_CONVERSATION_ID)],
                adopt_nothing(),
                context(),
            )
            .await,
        "ID 1 already belongs to conversation",
    );
}

/// "adopts a durable commit once before resolving, and never a failed one"
#[tokio::test]
async fn adopts_a_durable_commit_once_before_resolving_and_never_a_failed_one() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let read: Arc<Mutex<Option<kiana_durable::StoredEntry>>> = Arc::new(Mutex::new(None));
    let entry_id: EntryId = mint(&storage, IdKind::Entry).await;

    // adopt 在 commit 解析之前同步运行；此刻读到批次必须可见（读走同一个存储句柄）。
    let handle = storage.clone();
    let recorded = Arc::clone(&events);
    let read_slot = Arc::clone(&read);
    let adopt: kiana_durable::AdoptFn = Box::new(move |adopted| {
        recorded
            .lock()
            .expect("events")
            .push(format!("adopt {adopted}"));
        let value = futures::executor::block_on(handle.entry(entry_id, context()))
            .expect("read inside adopt");
        *read_slot.lock().expect("read slot") = value;
    });
    let seq = storage
        .commit(&[entry(entry_id, root_id, "message")], adopt, context())
        .await
        .expect("commit");
    events
        .lock()
        .expect("events")
        .push(format!("resolve {seq}"));
    assert_eq!(
        *events.lock().expect("events"),
        vec![format!("adopt {seq}"), format!("resolve {seq}")]
    );
    assert_eq!(
        read.lock()
            .expect("read slot")
            .as_ref()
            .map(|stored| stored.commit_seq),
        Some(seq)
    );

    events.lock().expect("events").clear();
    let recorded = Arc::clone(&events);
    let failing: kiana_durable::AdoptFn = Box::new(move |adopted| {
        recorded
            .lock()
            .expect("events")
            .push(format!("adopt {adopted}"))
    });
    assert_error_contains(
        storage
            .commit(&[conversation(root_id)], failing, context())
            .await,
        "ID 1 already belongs to conversation",
    );
    assert_eq!(*events.lock().expect("events"), Vec::<String>::new());

    let recorded = Arc::clone(&events);
    let next_entry: EntryId = mint(&storage, IdKind::Entry).await;
    let adopt: kiana_durable::AdoptFn = Box::new(move |adopted| {
        recorded
            .lock()
            .expect("events")
            .push(format!("adopt {adopted}"))
    });
    let next = storage
        .commit(&[entry(next_entry, root_id, "message")], adopt, context())
        .await
        .expect("commit");
    assert_eq!(
        *events.lock().expect("events"),
        vec![format!("adopt {next}")]
    );
    assert!(next > seq);
}

/// "commits mixed table writes atomically and rolls all of them back on failure"
#[tokio::test]
async fn commits_mixed_table_writes_atomically_and_rolls_all_of_them_back_on_failure() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let entry_id: EntryId = mint(&storage, IdKind::Entry).await;
    let task_id: TaskId = mint(&storage, IdKind::Task).await;
    let submission_id: SubmissionId = mint(&storage, IdKind::Submission).await;
    let task = pending_task(task_id, root_id);
    let input = SubmissionRecord {
        entry: Some(entry_id),
        status: SubmissionStatus::Placed,
        ..submission(
            submission_id,
            root_id,
            Some("request-1"),
            SubmissionKind::Input,
            SubmissionStatus::Queued,
        )
    };
    let initial_seq = storage
        .commit(
            &[
                entry_write(entry_record_with(
                    entry_id,
                    root_id,
                    "user",
                    Some(json!({ "text": "hello" })),
                    None,
                )),
                task_write(task.clone()),
                submission_write(input.clone()),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("mixed commit");

    assert_eq!(
        storage
            .entry(entry_id, context())
            .await
            .expect("read entry"),
        Some(kiana_durable::StoredEntry {
            entry: entry_record_with(
                entry_id,
                root_id,
                "user",
                Some(json!({ "text": "hello" })),
                None
            ),
            commit_seq: initial_seq,
        })
    );
    assert_eq!(
        storage.task(task_id, context()).await.expect("read task"),
        Some(task.clone())
    );
    assert_eq!(
        storage
            .submission(submission_id, context())
            .await
            .expect("read submission"),
        Some(input.clone())
    );

    let transient_entry_id: EntryId = mint(&storage, IdKind::Entry).await;
    let running_task = TaskRecord {
        state: TaskState::Running {
            checkpoint: json!({ "phase": "effect" }),
        },
        ..task.clone()
    };
    let done_input = SubmissionRecord {
        status: SubmissionStatus::Done,
        answer: Some(transient_entry_id),
        ..input.clone()
    };
    assert_error_contains(
        storage
            .commit(
                &[
                    task_write(running_task),
                    submission_write(done_input),
                    entry(transient_entry_id, root_id, "assistant"),
                    conversation(root_id),
                ],
                adopt_nothing(),
                context(),
            )
            .await,
        "ID 1 already belongs to conversation",
    );

    assert_eq!(
        storage.task(task_id, context()).await.expect("read task"),
        Some(task)
    );
    assert_eq!(
        storage
            .submission(submission_id, context())
            .await
            .expect("read submission"),
        Some(input)
    );
    assert_eq!(
        storage
            .entry(transient_entry_id, context())
            .await
            .expect("read entry"),
        None
    );
    let after_entry: EntryId = mint(&storage, IdKind::Entry).await;
    let after_rollback_seq = storage
        .commit(
            &[entry(after_entry, root_id, "after-rollback")],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit after rollback");
    assert!(after_rollback_seq > initial_seq);
}

/// "detaches retained writes and every returned record"
#[tokio::test]
async fn detaches_retained_writes_and_every_returned_record() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let entry_id: EntryId = mint(&storage, IdKind::Entry).await;
    let task_id: TaskId = mint(&storage, IdKind::Task).await;
    let submission_id: SubmissionId = mint(&storage, IdKind::Submission).await;
    let mut entry_data = json!({ "nested": [1, 2] });
    let mut checkpoint = json!({ "phase": "ready", "nested": { "count": 1 } });
    let mut detail = json!({ "codes": ["initial"] });
    let stored_entry = entry_record_with(entry_id, root_id, "note", Some(entry_data.clone()), None);
    let stored_task = TaskRecord {
        state: TaskState::Pending {
            checkpoint: checkpoint.clone(),
        },
        ..pending_task(task_id, root_id)
    };
    let stored_input = SubmissionRecord {
        reason: Some("failed".to_owned()),
        detail: Some(detail.clone()),
        ..submission(
            submission_id,
            root_id,
            None,
            SubmissionKind::Input,
            SubmissionStatus::Unanswered,
        )
    };
    storage
        .commit(
            &[
                entry_write(stored_entry),
                task_write(stored_task.clone()),
                submission_write(stored_input),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit");

    entry_data["nested"]
        .as_array_mut()
        .expect("array")
        .push(json!(3));
    checkpoint["nested"]["count"] = json!(2);
    detail["codes"]
        .as_array_mut()
        .expect("array")
        .push(json!("mutated"));
    assert_eq!(
        storage
            .entry(entry_id, context())
            .await
            .expect("read")
            .and_then(|stored| stored.entry.data),
        Some(json!({ "nested": [1, 2] }))
    );
    assert_eq!(
        storage
            .task(task_id, context())
            .await
            .expect("read")
            .map(|task| task.state),
        Some(TaskState::Pending {
            checkpoint: json!({ "phase": "ready", "nested": { "count": 1 } })
        })
    );
    assert_eq!(
        storage
            .submission(submission_id, context())
            .await
            .expect("read")
            .and_then(|record| record.detail),
        Some(json!({ "codes": ["initial"] }))
    );

    let mut read_entry = storage
        .entry(entry_id, context())
        .await
        .expect("read")
        .expect("stored entry")
        .entry;
    read_entry.data.as_mut().expect("data")["nested"]
        .as_array_mut()
        .expect("array")
        .push(json!(9));
    let mut read_task = storage
        .task(task_id, context())
        .await
        .expect("read")
        .expect("task");
    if let TaskState::Pending { checkpoint } = &mut read_task.state {
        checkpoint["nested"]["count"] = json!(9);
    }
    let mut read_input = storage
        .submission(submission_id, context())
        .await
        .expect("read")
        .expect("submission");
    read_input.detail.as_mut().expect("detail")["codes"]
        .as_array_mut()
        .expect("array")
        .push(json!("read mutation"));

    assert_eq!(
        storage
            .entry(entry_id, context())
            .await
            .expect("read")
            .and_then(|stored| stored.entry.data),
        Some(json!({ "nested": [1, 2] }))
    );
    assert_eq!(
        storage
            .task(task_id, context())
            .await
            .expect("read")
            .map(|task| task.state),
        Some(TaskState::Pending {
            checkpoint: json!({ "phase": "ready", "nested": { "count": 1 } })
        })
    );
    assert_eq!(
        storage
            .submission(submission_id, context())
            .await
            .expect("read")
            .and_then(|record| record.detail),
        Some(json!({ "codes": ["initial"] }))
    );
}

/// "detaches prototype-like JSON keys without changing object prototypes"
///
/// Rust 的 `serde_json::Map`（preserve_order 下是 `IndexMap`）把 `__proto__` 当普通键，
/// 不存在 JS 的原型陷阱；这里仍逐项验证键被原样保留、深拷贝后互不影响。
#[tokio::test]
async fn detaches_prototype_like_json_keys_without_changing_object_prototypes() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let entry_id: EntryId = mint(&storage, IdKind::Entry).await;
    let mut data: JsonValue = serde_json::from_str(
        r#"{"__proto__":{"polluted":false},"constructor":{"label":"stored"},"toString":"value"}"#,
    )
    .expect("parse");
    storage
        .commit(
            &[entry_write(entry_record_with(
                entry_id,
                root_id,
                "note",
                Some(data.clone()),
                None,
            ))],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit");

    data["__proto__"]["polluted"] = json!(true);
    data["constructor"]["label"] = json!("mutated");
    let first_read = storage
        .entry(entry_id, context())
        .await
        .expect("read")
        .expect("stored entry")
        .entry
        .data
        .expect("data");
    assert_eq!(first_read["__proto__"], json!({ "polluted": false }));
    assert_eq!(first_read["constructor"], json!({ "label": "stored" }));
    assert_eq!(first_read["toString"], json!("value"));

    let mut first_read = first_read;
    first_read["__proto__"]["polluted"] = json!(true);
    let second_read = storage
        .entry(entry_id, context())
        .await
        .expect("read")
        .expect("stored entry")
        .entry
        .data
        .expect("data");
    assert_eq!(second_read["__proto__"], json!({ "polluted": false }));
    assert_eq!(second_read["constructor"], json!({ "label": "stored" }));
    assert_eq!(second_read["toString"], json!("value"));
}

/// "indexes entries committed out of ID order"
#[tokio::test]
async fn indexes_entries_committed_out_of_id_order() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    storage
        .commit(
            &[
                entry(id_from_number(30), root_id, "message"),
                entry(id_from_number(10), root_id, "message"),
                entry_write(entry_record_with(
                    id_from_number(20),
                    root_id,
                    "marker",
                    None,
                    Some(id_from_number(10)),
                )),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit");

    let query = EntryQuery {
        conversation_id: root_id,
        min_entry_id: None,
        max_entry_id: None,
        order: None,
    };
    let page = storage
        .scan_entries(&query, 10, None, context())
        .await
        .expect("scan");
    assert_eq!(ids_of_entries(&page), vec![30, 20, 10]);
    assert_eq!(
        storage
            .find_latest_head_marker(root_id, None, context())
            .await
            .expect("marker")
            .map(|marker| marker.id.get()),
        Some(20)
    );
}

/// "continues an entry cursor below its last item after a newer commit"
#[tokio::test]
async fn continues_an_entry_cursor_below_its_last_item_after_a_newer_commit() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let oldest_id: EntryId = mint(&storage, IdKind::Entry).await;
    let middle_id: EntryId = mint(&storage, IdKind::Entry).await;
    let newest_id: EntryId = mint(&storage, IdKind::Entry).await;
    storage
        .commit(
            &[
                entry(oldest_id, root_id, "message"),
                entry(middle_id, root_id, "message"),
                entry(newest_id, root_id, "message"),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit");

    let query = EntryQuery {
        conversation_id: root_id,
        min_entry_id: None,
        max_entry_id: None,
        order: None,
    };
    let (first_ids, first_next) = scan_entry_ids(&storage, &query, 2, None)
        .await
        .expect("first page");
    assert_eq!(first_ids, vec![newest_id.get(), middle_id.get()]);
    let appended_id: EntryId = mint(&storage, IdKind::Entry).await;
    storage
        .commit(
            &[entry(appended_id, root_id, "message")],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("append");
    let (second_ids, second_next) = scan_entry_ids(&storage, &query, 2, first_next.as_ref())
        .await
        .expect("second page");
    assert_eq!(second_ids, vec![oldest_id.get()]);
    assert_eq!(second_next, None);
}

/// "paginates conversations by opaque cursor in ascending ID order"
#[tokio::test]
async fn paginates_conversations_by_opaque_cursor_in_ascending_id_order() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let second_id: ConversationId = mint(&storage, IdKind::Conversation).await;
    let third_id: ConversationId = mint(&storage, IdKind::Conversation).await;
    storage
        .commit(
            &[conversation(third_id), conversation(second_id)],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit");

    let query = ConversationQuery::default();
    let first = storage
        .scan_conversations(&query, 2, None, context())
        .await
        .expect("first page");
    assert_eq!(
        ids_of_conversations(&first),
        vec![root_id.get(), second_id.get()]
    );
    let next = first.next.clone().expect("first page has a cursor");
    let round_tripped = round_trip(&next);
    let second = storage
        .scan_conversations(&query, 2, Some(&round_tripped), context())
        .await
        .expect("second page");
    assert_eq!(ids_of_conversations(&second), vec![third_id.get()]);
    assert_eq!(second.next, None);
}

/// "filters and pages conversations by durable owner edges"
#[tokio::test]
async fn filters_and_pages_conversations_by_durable_owner_edges() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let other_owner_id: ConversationId = mint(&storage, IdKind::Conversation).await;
    let first_task_id: TaskId = mint(&storage, IdKind::Task).await;
    let second_task_id: TaskId = mint(&storage, IdKind::Task).await;
    let first_id: ConversationId = mint(&storage, IdKind::Conversation).await;
    let second_id: ConversationId = mint(&storage, IdKind::Conversation).await;
    let third_id: ConversationId = mint(&storage, IdKind::Conversation).await;
    storage
        .commit(
            &[
                conversation(other_owner_id),
                conversation_with(
                    first_id,
                    None,
                    Some(ConversationOwner {
                        conversation_id: root_id,
                        task_id: first_task_id,
                    }),
                ),
                conversation_with(
                    second_id,
                    None,
                    Some(ConversationOwner {
                        conversation_id: root_id,
                        task_id: second_task_id,
                    }),
                ),
                conversation_with(
                    third_id,
                    None,
                    Some(ConversationOwner {
                        conversation_id: other_owner_id,
                        task_id: first_task_id,
                    }),
                ),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit");

    let owned_by_root = ConversationQuery {
        owner_conversation_id: Some(root_id),
        owner_task_id: None,
        order: None,
    };
    let first = storage
        .scan_conversations(&owned_by_root, 1, None, context())
        .await
        .expect("first page");
    assert_eq!(ids_of_conversations(&first), vec![first_id.get()]);
    let next = first.next.clone().expect("first page has a cursor");
    let second = storage
        .scan_conversations(&owned_by_root, 1, Some(&next), context())
        .await
        .expect("second page");
    assert_eq!(ids_of_conversations(&second), vec![second_id.get()]);
    assert_eq!(second.next, None);

    let owned_by_first_task = ConversationQuery {
        owner_conversation_id: None,
        owner_task_id: Some(first_task_id),
        order: None,
    };
    let by_task = storage
        .scan_conversations(&owned_by_first_task, 10, None, context())
        .await
        .expect("owner task scan");
    assert_eq!(
        ids_of_conversations(&by_task),
        vec![first_id.get(), third_id.get()]
    );

    let both = ConversationQuery {
        owner_conversation_id: Some(root_id),
        owner_task_id: Some(first_task_id),
        order: None,
    };
    let by_both = storage
        .scan_conversations(&both, 10, None, context())
        .await
        .expect("both filters");
    assert_eq!(ids_of_conversations(&by_both), vec![first_id.get()]);
}

/// "scans deep fork history newest-first through every ancestor cap"
#[tokio::test]
async fn scans_deep_fork_history_newest_first_through_every_ancestor_cap() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let root_first: EntryId = mint(&storage, IdKind::Entry).await;
    let root_fork_point: EntryId = mint(&storage, IdKind::Entry).await;
    let root_excluded_same_commit: EntryId = mint(&storage, IdKind::Entry).await;
    let root_entries_seq = storage
        .commit(
            &[
                entry(root_first, root_id, "message"),
                entry_write(entry_record_with(
                    root_fork_point,
                    root_id,
                    "marker",
                    None,
                    Some(root_first),
                )),
                entry(root_excluded_same_commit, root_id, "message"),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("root entries");
    let child_id: ConversationId = mint(&storage, IdKind::Conversation).await;
    storage
        .commit(
            &[conversation_with(
                child_id,
                Some(ConversationParent {
                    conversation_id: root_id,
                    at: root_fork_point,
                }),
                None,
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("child conversation");
    let child_fork_point: EntryId = mint(&storage, IdKind::Entry).await;
    let child_excluded: EntryId = mint(&storage, IdKind::Entry).await;
    storage
        .commit(
            &[
                entry(child_fork_point, child_id, "note"),
                entry(child_excluded, child_id, "message"),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("child entries");
    let root_excluded_later: EntryId = mint(&storage, IdKind::Entry).await;
    storage
        .commit(
            &[entry(root_excluded_later, root_id, "message")],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("root later");
    let grandchild_id: ConversationId = mint(&storage, IdKind::Conversation).await;
    storage
        .commit(
            &[conversation_with(
                grandchild_id,
                Some(ConversationParent {
                    conversation_id: child_id,
                    at: child_fork_point,
                }),
                None,
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("grandchild conversation");
    let grandchild_head: EntryId = mint(&storage, IdKind::Entry).await;
    let grandchild_tail: EntryId = mint(&storage, IdKind::Entry).await;
    let grandchild_entries_seq = storage
        .commit(
            &[
                entry_write(entry_record_with(
                    grandchild_head,
                    grandchild_id,
                    "marker",
                    None,
                    Some(grandchild_head),
                )),
                entry(grandchild_tail, grandchild_id, "message"),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("grandchild entries");
    let child_excluded_later: EntryId = mint(&storage, IdKind::Entry).await;
    storage
        .commit(
            &[entry(child_excluded_later, child_id, "message")],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("child later");

    let base_query = EntryQuery {
        conversation_id: grandchild_id,
        min_entry_id: None,
        max_entry_id: None,
        order: None,
    };
    let (first_ids, first_next) = scan_entry_ids(&storage, &base_query, 2, None)
        .await
        .expect("page 1");
    assert_eq!(
        first_ids,
        vec![grandchild_tail.get(), grandchild_head.get()]
    );
    let (second_ids, second_next) = scan_entry_ids(&storage, &base_query, 2, first_next.as_ref())
        .await
        .expect("page 2");
    assert_eq!(
        second_ids,
        vec![child_fork_point.get(), root_fork_point.get()]
    );
    let (third_ids, third_next) = scan_entry_ids(&storage, &base_query, 2, second_next.as_ref())
        .await
        .expect("page 3");
    assert_eq!(third_ids, vec![root_first.get()]);
    assert_eq!(third_next, None);

    // 由旧到新：root 段，然后每个 fork 段，各自被下一个 fork 点封顶。
    let ascending = EntryQuery {
        order: Some(ScanOrder::Ascending),
        ..base_query.clone()
    };
    let (first_up_ids, first_up_next) = scan_entry_ids(&storage, &ascending, 2, None)
        .await
        .expect("ascending page 1");
    assert_eq!(first_up_ids, vec![root_first.get(), root_fork_point.get()]);
    let (second_up_ids, second_up_next) =
        scan_entry_ids(&storage, &ascending, 2, first_up_next.as_ref())
            .await
            .expect("ascending page 2");
    assert_eq!(
        second_up_ids,
        vec![child_fork_point.get(), grandchild_head.get()]
    );
    let (third_up_ids, third_up_next) =
        scan_entry_ids(&storage, &base_query, 2, second_up_next.as_ref())
            .await
            .expect("ascending page 3");
    assert_eq!(third_up_ids, vec![grandchild_tail.get()]);
    assert_eq!(third_up_next, None);
    let ranged = EntryQuery {
        min_entry_id: Some(root_fork_point),
        max_entry_id: Some(child_fork_point),
        ..ascending.clone()
    };
    let (ranged_ids, _) = scan_entry_ids(&storage, &ranged, 10, None)
        .await
        .expect("ranged");
    assert_eq!(
        ranged_ids,
        vec![root_fork_point.get(), child_fork_point.get()]
    );
    assert_error_contains(
        scan_entry_ids(
            &storage,
            &EntryQuery {
                order: Some(ScanOrder::Descending),
                ..base_query.clone()
            },
            2,
            first_up_next.as_ref(),
        )
        .await,
        "cursor",
    );

    let current_marker = storage
        .find_latest_head_marker(grandchild_id, None, context())
        .await
        .expect("marker")
        .expect("current marker");
    assert_eq!(current_marker.id, grandchild_head);
    assert_eq!(current_marker.head, Some(grandchild_head));
    let historical_marker = storage
        .find_latest_head_marker(grandchild_id, Some(child_fork_point), context())
        .await
        .expect("marker")
        .expect("historical marker");
    assert_eq!(historical_marker.id, root_fork_point);
    assert_eq!(historical_marker.head, Some(root_first));
    assert_eq!(
        storage
            .find_latest_head_marker(grandchild_id, Some(root_first), context())
            .await
            .expect("marker"),
        None
    );

    let active_query = EntryQuery {
        min_entry_id: current_marker.head,
        ..base_query.clone()
    };
    let (active_first_ids, active_first_next) = scan_entry_ids(&storage, &active_query, 1, None)
        .await
        .expect("active page 1");
    assert_eq!(active_first_ids, vec![grandchild_tail.get()]);
    let (active_second_ids, active_second_next) =
        scan_entry_ids(&storage, &active_query, 1, active_first_next.as_ref())
            .await
            .expect("active page 2");
    assert_eq!(active_second_ids, vec![grandchild_head.get()]);
    assert_eq!(active_second_next, None);

    let historical_query = EntryQuery {
        min_entry_id: historical_marker.head,
        max_entry_id: Some(child_fork_point),
        ..base_query.clone()
    };
    let (historical_ids, _) = scan_entry_ids(&storage, &historical_query, 10, None)
        .await
        .expect("historical scan");
    assert_eq!(
        historical_ids,
        vec![
            child_fork_point.get(),
            root_fork_point.get(),
            root_first.get()
        ]
    );

    assert_eq!(
        storage.entry(root_first, context()).await.expect("entry"),
        Some(kiana_durable::StoredEntry {
            entry: entry_record(root_first, root_id, "message"),
            commit_seq: root_entries_seq,
        })
    );
    assert_eq!(
        storage
            .entry(root_fork_point, context())
            .await
            .expect("entry")
            .map(|stored| stored.commit_seq),
        Some(root_entries_seq)
    );
    assert_eq!(
        storage
            .entry(grandchild_head, context())
            .await
            .expect("entry")
            .map(|stored| stored.commit_seq),
        Some(grandchild_entries_seq)
    );
    assert_eq!(
        storage
            .entry(grandchild_tail, context())
            .await
            .expect("entry")
            .map(|stored| stored.commit_seq),
        Some(grandchild_entries_seq)
    );
    assert_eq!(
        storage
            .entry(id_from_number(999_999), context())
            .await
            .expect("entry"),
        None
    );

    assert_eq!(
        storage
            .visible_entry(grandchild_id, root_first, context())
            .await
            .expect("visible"),
        Some(kiana_durable::StoredEntry {
            entry: entry_record(root_first, root_id, "message"),
            commit_seq: root_entries_seq,
        })
    );
    assert_eq!(
        storage
            .visible_entry(grandchild_id, child_fork_point, context())
            .await
            .expect("visible")
            .map(|stored| stored.entry.conversation_id),
        Some(child_id)
    );
    assert_eq!(
        storage
            .visible_entry(grandchild_id, grandchild_tail, context())
            .await
            .expect("visible")
            .map(|stored| stored.commit_seq),
        Some(grandchild_entries_seq)
    );
    for excluded in [
        root_excluded_same_commit,
        root_excluded_later,
        child_excluded,
        child_excluded_later,
    ] {
        assert_eq!(
            storage
                .visible_entry(grandchild_id, excluded, context())
                .await
                .expect("visible"),
            None
        );
    }
    assert_eq!(
        storage
            .visible_entry(root_id, grandchild_head, context())
            .await
            .expect("visible"),
        None
    );
    assert_eq!(
        storage
            .visible_entry(grandchild_id, id_from_number(999_999), context())
            .await
            .expect("visible"),
        None
    );
    assert_error_contains(
        storage
            .visible_entry(id_from_number(999_999), root_first, context())
            .await,
        "Unknown conversation",
    );
    assert_error_contains(
        storage
            .scan_entries(
                &EntryQuery {
                    conversation_id: id_from_number(999_999),
                    min_entry_id: None,
                    max_entry_id: None,
                    order: None,
                },
                10,
                None,
                context(),
            )
            .await,
        "Unknown conversation",
    );
}

/// "scans tables in either ID order and continues a cursor in its order"
#[tokio::test]
async fn scans_tables_in_either_id_order_and_continues_a_cursor_in_its_order() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let mut conversation_ids = vec![root_id.get()];
    let mut task_ids = Vec::new();
    let mut submission_ids = Vec::new();
    for _ in 0..3 {
        let conversation_id: ConversationId = mint(&storage, IdKind::Conversation).await;
        let task_id: TaskId = mint(&storage, IdKind::Task).await;
        let submission_id: SubmissionId = mint(&storage, IdKind::Submission).await;
        storage
            .commit(
                &[
                    conversation(conversation_id),
                    task_write(pending_task(task_id, root_id)),
                    submission_write(submission(
                        submission_id,
                        root_id,
                        None,
                        SubmissionKind::Input,
                        SubmissionStatus::Queued,
                    )),
                ],
                adopt_nothing(),
                context(),
            )
            .await
            .expect("round");
        conversation_ids.push(conversation_id.get());
        task_ids.push(task_id.get());
        submission_ids.push(submission_id.get());
    }

    for (scanner, ids) in [
        (Scanner::Conversations, conversation_ids),
        (Scanner::Tasks, task_ids),
        (Scanner::Submissions, submission_ids),
    ] {
        let mut reversed = ids.clone();
        reversed.reverse();
        assert_eq!(collect_all(&storage, scanner, None).await, ids);
        assert_eq!(
            collect_all(&storage, scanner, Some(ScanOrder::Ascending)).await,
            ids
        );
        assert_eq!(
            collect_all(&storage, scanner, Some(ScanOrder::Descending)).await,
            reversed
        );
        let (_, descending_next) = scan_ids(&storage, scanner, Some(ScanOrder::Descending), None)
            .await
            .expect("descending page");
        let descending_next = descending_next.expect("descending page has a cursor");
        let (continued, _) = scan_ids(
            &storage,
            scanner,
            Some(ScanOrder::Descending),
            Some(&descending_next),
        )
        .await
        .expect("continued page");
        // TS 的 `reversed.slice(2, 4)`：不足两个时只取剩下的。
        assert_eq!(
            continued,
            reversed.iter().skip(2).take(2).copied().collect::<Vec<_>>()
        );
        assert_error_contains(
            scan_ids(
                &storage,
                scanner,
                Some(ScanOrder::Ascending),
                Some(&descending_next),
            )
            .await,
            "cursor",
        );
    }
}

/// "replaces complete task records and pages filtered task scans"
#[tokio::test]
async fn replaces_complete_task_records_and_pages_filtered_task_scans() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let first_id: TaskId = mint(&storage, IdKind::Task).await;
    let second_id: TaskId = mint(&storage, IdKind::Task).await;
    let third_id: TaskId = mint(&storage, IdKind::Task).await;
    let first = TaskRecord {
        memos: Some(object(json!({ "winner": "first" }))),
        ..pending_task(first_id, root_id)
    };
    let second = TaskRecord {
        background: true,
        ..pending_task(second_id, root_id)
    };
    let third = TaskRecord {
        abort_requested: true,
        ..pending_task(third_id, root_id)
    };
    storage
        .commit(
            &[
                task_write(first.clone()),
                task_write(second.clone()),
                task_write(third.clone()),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit tasks");

    let running = TaskRecord {
        state: TaskState::Running {
            checkpoint: json!({ "phase": "effect", "attempt": 1 }),
        },
        abort_requested: true,
        ..first.clone()
    };
    storage
        .commit(&[task_write(running.clone())], adopt_nothing(), context())
        .await
        .expect("replace as running");
    assert_eq!(
        storage.task(first_id, context()).await.expect("task"),
        Some(running)
    );
    let terminal = TaskRecord {
        id: first_id,
        conversation_id: root_id,
        kind: first.kind.clone(),
        version: first.version,
        input: first.input.clone(),
        state: TaskState::Terminal {
            outcome: TaskOutcome::Completed {
                result: json!({ "entryId": 99 }),
            },
        },
        background: false,
        abort_requested: true,
        trace: None,
        links: None,
        owner: None,
        abandon_on_restart: None,
        started_at: None,
        ended_at: None,
        restarts: None,
        abort_reason: None,
        memos: None,
    };
    storage
        .commit(&[task_write(terminal.clone())], adopt_nothing(), context())
        .await
        .expect("replace as terminal");
    assert_eq!(
        storage.task(first_id, context()).await.expect("task"),
        Some(terminal.clone())
    );

    let pending_query = TaskQuery {
        status: Some(TaskStatus::Pending),
        ..TaskQuery::default()
    };
    let pending_page = storage
        .scan_tasks(&pending_query, 1, None, context())
        .await
        .expect("pending");
    assert_eq!(ids_of_tasks(&pending_page), vec![second_id.get()]);
    let pending_next = pending_page.next.clone().expect("cursor");
    let pending_second = storage
        .scan_tasks(&pending_query, 1, Some(&pending_next), context())
        .await
        .expect("pending continuation");
    assert_eq!(ids_of_tasks(&pending_second), vec![third_id.get()]);
    let terminal_query = TaskQuery {
        status: Some(TaskStatus::Terminal),
        abort_requested: Some(true),
        ..TaskQuery::default()
    };
    assert_eq!(
        storage
            .scan_tasks(&terminal_query, 10, None, context())
            .await
            .expect("terminal")
            .items,
        vec![terminal]
    );
    let background_query = TaskQuery {
        background: Some(true),
        ..TaskQuery::default()
    };
    assert_eq!(
        ids_of_tasks(
            &storage
                .scan_tasks(&background_query, 10, None, context())
                .await
                .expect("background")
        ),
        vec![second_id.get()]
    );
}

/// "stores owners and scans waiting and completing tasks by status"
#[tokio::test]
async fn stores_owners_and_scans_waiting_and_completing_tasks_by_status() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let owner_id: TaskId = mint(&storage, IdKind::Task).await;
    let waiting_id: TaskId = mint(&storage, IdKind::Task).await;
    let completing_id: TaskId = mint(&storage, IdKind::Task).await;
    let owner = pending_task(owner_id, root_id);
    let waiting = TaskRecord {
        owner: Some(owner_id),
        state: TaskState::Waiting {
            checkpoint: json!({ "phase": "next" }),
            on: vec![owner_id],
            policy: JoinPolicy::AllSettled,
        },
        memos: Some(object(json!({ "kept": true }))),
        ..pending_task(waiting_id, root_id)
    };
    let completing = TaskRecord {
        owner: Some(owner_id),
        state: TaskState::Completing {
            outcome: TaskOutcome::Failed {
                error: TaskOutcomeError {
                    message: "held".to_owned(),
                    detail: None,
                },
                result: None,
            },
        },
        ..pending_task(completing_id, root_id)
    };
    storage
        .commit(
            &[
                task_write(owner.clone()),
                task_write(waiting.clone()),
                task_write(completing.clone()),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit tasks");
    assert_eq!(
        storage.task(waiting_id, context()).await.expect("task"),
        Some(waiting.clone())
    );
    assert_eq!(
        storage.task(completing_id, context()).await.expect("task"),
        Some(completing.clone())
    );
    let scan = |status: TaskStatus| {
        let storage = storage.clone();
        async move {
            storage
                .scan_tasks(
                    &TaskQuery {
                        status: Some(status),
                        ..TaskQuery::default()
                    },
                    10,
                    None,
                    context(),
                )
                .await
                .expect("scan")
                .items
        }
    };
    assert_eq!(scan(TaskStatus::Waiting).await, vec![waiting]);
    assert_eq!(scan(TaskStatus::Completing).await, vec![completing.clone()]);
    assert_eq!(
        ids_of_tasks(&Page {
            items: scan(TaskStatus::Pending).await,
            next: None,
        }),
        vec![owner_id.get()]
    );
    let outcome = match &completing.state {
        TaskState::Completing { outcome } => outcome.clone(),
        other => panic!("expected a completing task, got {other:?}"),
    };
    let terminal = TaskRecord {
        state: TaskState::Terminal { outcome },
        ..completing
    };
    storage
        .commit(&[task_write(terminal.clone())], adopt_nothing(), context())
        .await
        .expect("settle");
    assert_eq!(scan(TaskStatus::Completing).await, Vec::<TaskRecord>::new());
    assert_eq!(scan(TaskStatus::Terminal).await, vec![terminal]);
}

/// "stores and scans sleeping tasks by status"
#[tokio::test]
async fn stores_and_scans_sleeping_tasks_by_status() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let sleeping_id: TaskId = mint(&storage, IdKind::Task).await;
    let sleeping = TaskRecord {
        state: TaskState::Sleeping {
            checkpoint: json!({ "phase": "next" }),
            until: 61_000,
        },
        memos: Some(object(json!({ "kept": true }))),
        ..pending_task(sleeping_id, root_id)
    };
    storage
        .commit(&[task_write(sleeping.clone())], adopt_nothing(), context())
        .await
        .expect("commit");
    assert_eq!(
        storage.task(sleeping_id, context()).await.expect("task"),
        Some(sleeping.clone())
    );
    let scan = |status: TaskStatus| {
        let storage = storage.clone();
        async move {
            storage
                .scan_tasks(
                    &TaskQuery {
                        status: Some(status),
                        ..TaskQuery::default()
                    },
                    10,
                    None,
                    context(),
                )
                .await
                .expect("scan")
                .items
        }
    };
    assert_eq!(scan(TaskStatus::Sleeping).await, vec![sleeping.clone()]);
    let running = TaskRecord {
        state: TaskState::Running {
            checkpoint: json!({ "phase": "next" }),
        },
        ..sleeping
    };
    storage
        .commit(&[task_write(running.clone())], adopt_nothing(), context())
        .await
        .expect("wake");
    assert_eq!(scan(TaskStatus::Sleeping).await, Vec::<TaskRecord>::new());
    assert_eq!(scan(TaskStatus::Running).await, vec![running]);
}

async fn collect_submission_ids(storage: &MemoryStorage, query: &SubmissionQuery) -> Vec<i64> {
    let mut found = Vec::new();
    let mut cursor: Option<Cursor> = None;
    loop {
        let page = storage
            .scan_submissions(query, 1, cursor.as_ref(), context())
            .await
            .expect("submission scan");
        found.extend(page.items.iter().map(|item| item.id.get()));
        cursor = page.next.clone();
        if cursor.is_none() {
            break;
        }
    }
    found
}

async fn scan_document_ids(
    storage: &MemoryStorage,
    query: &DocumentQuery,
    limit: usize,
    cursor: Option<&Cursor>,
) -> Result<(Vec<i64>, Option<Cursor>), BoxError> {
    let page = storage
        .scan_documents(query, limit, cursor, context())
        .await?;
    Ok((ids_of_documents(&page), page.next))
}

/// "indexes request IDs per conversation and replaces complete submission records"
#[tokio::test]
async fn indexes_request_ids_per_conversation_and_replaces_complete_submission_records() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let second_conversation_id: ConversationId = mint(&storage, IdKind::Conversation).await;
    storage
        .commit(
            &[conversation(second_conversation_id)],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("second conversation");
    let first_id: SubmissionId = mint(&storage, IdKind::Submission).await;
    let second_id: SubmissionId = mint(&storage, IdKind::Submission).await;
    let other_conversation_id: SubmissionId = mint(&storage, IdKind::Submission).await;
    let first = submission(
        first_id,
        root_id,
        Some("same"),
        SubmissionKind::Input,
        SubmissionStatus::Queued,
    );
    let second = submission(
        second_id,
        root_id,
        Some("other"),
        SubmissionKind::Input,
        SubmissionStatus::Queued,
    );
    let other_conversation = submission(
        other_conversation_id,
        second_conversation_id,
        Some("same"),
        SubmissionKind::Input,
        SubmissionStatus::Queued,
    );
    storage
        .commit(
            &[
                submission_write(first.clone()),
                submission_write(second.clone()),
                submission_write(other_conversation.clone()),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit submissions");
    assert_eq!(
        storage
            .submission_by_request(root_id, "same", context())
            .await
            .expect("by request"),
        Some(first.clone())
    );
    assert_eq!(
        storage
            .submission_by_request(second_conversation_id, "same", context())
            .await
            .expect("by request"),
        Some(other_conversation.clone())
    );

    let placed_second = SubmissionRecord {
        status: SubmissionStatus::Placed,
        entry: Some(mint(&storage, IdKind::Entry).await),
        ..second.clone()
    };
    storage
        .commit(
            &[submission_write(placed_second.clone())],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("place");
    assert_eq!(
        storage
            .submission(second_id, context())
            .await
            .expect("submission"),
        Some(placed_second.clone())
    );
    assert_eq!(
        storage
            .submission_by_request(root_id, "other", context())
            .await
            .expect("by request"),
        Some(placed_second.clone())
    );

    let all = SubmissionQuery::default();
    assert_eq!(
        collect_submission_ids(&storage, &all).await,
        vec![first_id.get(), second_id.get(), other_conversation_id.get()]
    );
    assert_eq!(
        collect_submission_ids(
            &storage,
            &SubmissionQuery {
                conversation_id: Some(root_id),
                ..SubmissionQuery::default()
            }
        )
        .await,
        vec![first_id.get(), second_id.get()]
    );
    assert_eq!(
        collect_submission_ids(
            &storage,
            &SubmissionQuery {
                status: Some(SubmissionStatus::Queued),
                ..SubmissionQuery::default()
            }
        )
        .await,
        vec![first_id.get(), other_conversation_id.get()]
    );
    assert_eq!(
        collect_submission_ids(
            &storage,
            &SubmissionQuery {
                status: Some(SubmissionStatus::Placed),
                ..SubmissionQuery::default()
            }
        )
        .await,
        vec![second_id.get()]
    );
    assert_eq!(
        collect_submission_ids(
            &storage,
            &SubmissionQuery {
                conversation_id: Some(second_conversation_id),
                status: Some(SubmissionStatus::Queued),
                ..SubmissionQuery::default()
            }
        )
        .await,
        vec![other_conversation_id.get()]
    );
    assert_eq!(
        collect_submission_ids(
            &storage,
            &SubmissionQuery {
                conversation_id: Some(second_conversation_id),
                status: Some(SubmissionStatus::Placed),
                ..SubmissionQuery::default()
            }
        )
        .await,
        Vec::<i64>::new()
    );
    assert_eq!(
        storage
            .scan_submissions(
                &SubmissionQuery {
                    status: Some(SubmissionStatus::Placed),
                    ..SubmissionQuery::default()
                },
                10,
                None,
                context(),
            )
            .await
            .expect("placed scan")
            .items,
        vec![placed_second]
    );
}

/// "stores passive write submissions without input-only lifecycle states"
#[tokio::test]
async fn stores_passive_write_submissions_without_input_only_lifecycle_states() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let done_id: SubmissionId = mint(&storage, IdKind::Submission).await;
    let failed_id: SubmissionId = mint(&storage, IdKind::Submission).await;
    let queued_done = submission(
        done_id,
        root_id,
        Some("passive-done"),
        SubmissionKind::Write,
        SubmissionStatus::Queued,
    );
    let queued_failed = submission(
        failed_id,
        root_id,
        Some("passive-failed"),
        SubmissionKind::Write,
        SubmissionStatus::Queued,
    );
    storage
        .commit(
            &[
                submission_write(queued_done.clone()),
                submission_write(queued_failed.clone()),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit");

    let done = SubmissionRecord {
        status: SubmissionStatus::Done,
        entry: Some(mint(&storage, IdKind::Entry).await),
        ..queued_done
    };
    let unanswered = SubmissionRecord {
        status: SubmissionStatus::Unanswered,
        reason: Some("closed".to_owned()),
        detail: Some(json!({ "retryable": false })),
        ..queued_failed
    };
    storage
        .commit(
            &[
                submission_write(done.clone()),
                submission_write(unanswered.clone()),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("settle");
    assert_eq!(
        storage
            .submission(done_id, context())
            .await
            .expect("submission"),
        Some(done.clone())
    );
    assert_eq!(
        storage
            .submission_by_request(root_id, "passive-done", context())
            .await
            .expect("by request"),
        Some(done)
    );
    assert_eq!(
        storage
            .submission(failed_id, context())
            .await
            .expect("submission"),
        Some(unanswered.clone())
    );
    assert_eq!(
        storage
            .submission_by_request(root_id, "passive-failed", context())
            .await
            .expect("by request"),
        Some(unanswered)
    );
}

/// "reconstructs rewindable documents and preserves half-open incarnations"
#[tokio::test]
async fn reconstructs_rewindable_documents_and_preserves_half_open_incarnations() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let first_id: DocumentId = mint(&storage, IdKind::Document).await;
    let first_record = doc_record_full(
        first_id,
        "conversation.notes",
        conversation_scope(root_id),
        None,
        Some(DocumentHistory::Rewindable),
        Some(DocumentFork::AsOf),
    );
    let initial = object(json!({ "items": ["a"], "nested": { "count": 1 } }));
    let created_at = storage
        .commit(
            &[doc_create(first_record.clone(), 1, initial.clone())],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("create document");
    let appended = vec![json!("b")];
    let changed_at = storage
        .commit(
            &[doc_delta(
                first_id,
                1,
                vec![
                    Op::Splice {
                        path: vec![Seg::Key("items".to_owned())],
                        index: 1,
                        remove: 0,
                        items: appended.clone(),
                    },
                    Op::Set(
                        vec![Seg::Key("nested".to_owned()), Seg::Key("count".to_owned())],
                        json!(2),
                    ),
                ],
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("append delta");

    // 调用方后续修改自己保留的数组不影响已提交的历史（深拷贝）。
    let mut mutated_initial = initial.clone();
    mutated_initial["items"]
        .as_array_mut()
        .expect("array")
        .push(json!("caller mutation"));
    mutated_initial["nested"]["count"] = json!(-1);

    let at_created = storage
        .document(first_id, DocumentPoint::At(created_at), context())
        .await
        .expect("read")
        .expect("document");
    assert_eq!(at_created.version, 1);
    assert_eq!(
        at_created.value,
        object(json!({ "items": ["a"], "nested": { "count": 1 } }))
    );
    assert_eq!(at_created.deltas_since_base, 0);
    let changed = storage
        .document(first_id, DocumentPoint::At(changed_at), context())
        .await
        .expect("read")
        .expect("document");
    assert_eq!(
        changed.value,
        object(json!({ "items": ["a", "b"], "nested": { "count": 2 } }))
    );
    assert_eq!(changed.deltas_since_base, 1);
    let mut changed_value = changed.value.clone();
    changed_value["items"]
        .as_array_mut()
        .expect("array")
        .push(json!("read mutation"));
    assert_eq!(
        storage
            .document(first_id, DocumentPoint::Current, context())
            .await
            .expect("read")
            .expect("document")
            .value,
        object(json!({ "items": ["a", "b"], "nested": { "count": 2 } }))
    );

    let checkpoint_at = storage
        .commit(
            &[doc_base_change(
                first_id,
                2,
                object(json!({ "items": ["checkpoint"], "nested": { "count": 3 } })),
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("checkpoint base");
    let replaced_at = storage
        .commit(
            &[doc_delta(
                first_id,
                2,
                vec![Op::Replace(
                    json!({ "items": ["replacement"], "nested": { "count": 4 } }),
                )],
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("replacement delta");
    let at_changed_again = storage
        .document(first_id, DocumentPoint::At(changed_at), context())
        .await
        .expect("read")
        .expect("document");
    assert_eq!(at_changed_again.version, 1);
    assert_eq!(
        at_changed_again.value,
        object(json!({ "items": ["a", "b"], "nested": { "count": 2 } }))
    );
    let at_checkpoint = storage
        .document(first_id, DocumentPoint::At(checkpoint_at), context())
        .await
        .expect("read")
        .expect("document");
    assert_eq!(at_checkpoint.version, 2);
    assert_eq!(
        at_checkpoint.value,
        object(json!({ "items": ["checkpoint"], "nested": { "count": 3 } }))
    );
    assert_eq!(at_checkpoint.deltas_since_base, 0);
    let at_replaced = storage
        .document(first_id, DocumentPoint::At(replaced_at), context())
        .await
        .expect("read")
        .expect("document");
    assert_eq!(
        at_replaced.value,
        object(json!({ "items": ["replacement"], "nested": { "count": 4 } }))
    );
    assert_eq!(at_replaced.deltas_since_base, 1);
    assert_eq!(
        storage
            .document(first_id, DocumentPoint::Current, context())
            .await
            .expect("read")
            .expect("document")
            .deltas_since_base,
        1
    );

    let second_id: DocumentId = mint(&storage, IdKind::Document).await;
    let retired_at = storage
        .commit(
            &[
                doc_create(
                    DocumentCreate {
                        id: second_id,
                        ..first_record.clone()
                    },
                    1,
                    object(json!({ "items": ["new"] })),
                ),
                doc_retire(first_id),
                doc_delta(
                    first_id,
                    2,
                    vec![Op::Set(vec![Seg::Key("retiring".to_owned())], json!(true))],
                ),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("retire and replace");
    let address = DocumentAddress {
        kind: first_record.kind.clone(),
        scope: first_record.scope,
        key: None,
    };
    assert_eq!(
        storage
            .find_document(&address, DocumentPoint::At(changed_at), context())
            .await
            .expect("find")
            .map(|record| record.id),
        Some(first_id)
    );
    let successor = storage
        .find_document(&address, DocumentPoint::At(retired_at), context())
        .await
        .expect("find")
        .expect("second incarnation");
    assert_eq!(successor.id, second_id);
    assert_eq!(successor.created_at, retired_at);
    let (at_changed_ids, _) = scan_document_ids(
        &storage,
        &DocumentQuery {
            scope: first_record.scope,
            at: DocumentPoint::At(changed_at),
            kind: None,
        },
        10,
        None,
    )
    .await
    .expect("scan");
    assert_eq!(at_changed_ids, vec![first_id.get()]);
    let (at_retired_ids, _) = scan_document_ids(
        &storage,
        &DocumentQuery {
            scope: first_record.scope,
            at: DocumentPoint::At(retired_at),
            kind: None,
        },
        10,
        None,
    )
    .await
    .expect("scan");
    assert_eq!(at_retired_ids, vec![second_id.get()]);
    assert_eq!(
        storage
            .document(first_id, DocumentPoint::At(retired_at), context())
            .await
            .expect("read"),
        None
    );
    assert_eq!(
        storage
            .document(second_id, DocumentPoint::Current, context())
            .await
            .expect("read")
            .expect("document")
            .value,
        object(json!({ "items": ["new"] }))
    );
}

/// "streams long document tails across root replacement deltas"
#[tokio::test]
async fn streams_long_document_tails_across_root_replacement_deltas() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let id: DocumentId = mint(&storage, IdKind::Document).await;
    let record = doc_record_full(
        id,
        "conversation.long-tail",
        conversation_scope(root_id),
        None,
        Some(DocumentHistory::Rewindable),
        Some(DocumentFork::AsOf),
    );
    let initial_rows: Vec<JsonValue> = (0..512)
        .map(|value| json!({ "value": value, "stable": format!("row-{value}") }))
        .collect();
    let initial = object(json!({ "revision": 0, "rows": initial_rows }));
    let created_at = storage
        .commit(
            &[doc_create(record, 1, initial.clone())],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("create");

    let mut before_replacement = initial.clone();
    let mut before_replacement_at = created_at;
    for revision in 1..=24i64 {
        let index = ((revision * 17) % 512) as usize;
        before_replacement["rows"][index]["value"] = json!(-revision);
        before_replacement["revision"] = json!(revision);
        before_replacement_at = storage
            .commit(
                &[doc_delta(
                    id,
                    1,
                    vec![
                        Op::Set(
                            vec![
                                Seg::Key("rows".to_owned()),
                                Seg::Index(index as i64),
                                Seg::Key("value".to_owned()),
                            ],
                            json!(-revision),
                        ),
                        Op::Set(vec![Seg::Key("revision".to_owned())], json!(revision)),
                    ],
                )],
                adopt_nothing(),
                context(),
            )
            .await
            .expect("delta");
    }

    let replacement_rows: Vec<JsonValue> = (0..512)
        .map(|value| json!({ "value": 10_000 + value, "stable": format!("new-{value}") }))
        .collect();
    let replacement = json!({ "revision": 100, "rows": replacement_rows });
    let replacement_snapshot = replacement.clone();
    let replacement_at = storage
        .commit(
            &[doc_delta(id, 1, vec![Op::Replace(replacement.clone())])],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("replace");
    let mut mutated_replacement = replacement;
    mutated_replacement["rows"][0]["value"] = json!(-999);

    let mut current = replacement_snapshot.clone();
    for revision in 101..=124i64 {
        let index = ((revision * 19) % 512) as usize;
        current["rows"][index]["value"] = json!(-revision);
        current["revision"] = json!(revision);
        storage
            .commit(
                &[doc_delta(
                    id,
                    1,
                    vec![
                        Op::Set(
                            vec![
                                Seg::Key("rows".to_owned()),
                                Seg::Index(index as i64),
                                Seg::Key("value".to_owned()),
                            ],
                            json!(-revision),
                        ),
                        Op::Set(vec![Seg::Key("revision".to_owned())], json!(revision)),
                    ],
                )],
                adopt_nothing(),
                context(),
            )
            .await
            .expect("tail delta");
    }

    assert_eq!(
        storage
            .document(id, DocumentPoint::At(created_at), context())
            .await
            .expect("read")
            .expect("document")
            .value,
        initial
    );
    assert_eq!(
        storage
            .document(id, DocumentPoint::At(before_replacement_at), context())
            .await
            .expect("read")
            .expect("document")
            .value,
        before_replacement
    );
    assert_eq!(
        storage
            .document(id, DocumentPoint::At(replacement_at), context())
            .await
            .expect("read")
            .expect("document")
            .value,
        object(replacement_snapshot.clone())
    );
    let read = storage
        .document(id, DocumentPoint::Current, context())
        .await
        .expect("read")
        .expect("document");
    assert_eq!(read.value, object(current.clone()));
    let mut mutated_read = read.value.clone();
    mutated_read["rows"][0]["value"] = json!(-1_000);
    assert_eq!(
        storage
            .document(id, DocumentPoint::Current, context())
            .await
            .expect("read")
            .expect("document")
            .value,
        object(current)
    );
}

/// "copies stored document bases independently and rejects ambiguous sources"
#[tokio::test]
async fn copies_stored_document_bases_independently_and_rejects_ambiguous_sources() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let child_id: ConversationId = mint(&storage, IdKind::Conversation).await;
    let second_child_id: ConversationId = mint(&storage, IdKind::Conversation).await;
    storage
        .commit(
            &[conversation(child_id), conversation(second_child_id)],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("child conversations");
    let source_id: DocumentId = mint(&storage, IdKind::Document).await;
    let source_record = doc_record_full(
        source_id,
        "copy.source",
        conversation_scope(root_id),
        None,
        Some(DocumentHistory::Rewindable),
        Some(DocumentFork::AsOf),
    );
    let created_at = storage
        .commit(
            &[doc_create(
                source_record.clone(),
                2,
                object(json!({ "count": 1, "rows": [{ "value": "base" }] })),
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("create source");
    storage
        .commit(
            &[doc_delta(
                source_id,
                2,
                vec![
                    Op::Set(vec![Seg::Key("count".to_owned())], json!(2)),
                    Op::Splice {
                        path: vec![Seg::Key("rows".to_owned())],
                        index: 1,
                        remove: 0,
                        items: vec![json!({ "value": "current" })],
                    },
                ],
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("source delta");
    let historical_copy_id: DocumentId = mint(&storage, IdKind::Document).await;
    let current_copy_id: DocumentId = mint(&storage, IdKind::Document).await;
    let retired_copy_id: DocumentId = mint(&storage, IdKind::Document).await;
    let child_record = |id: DocumentId, conversation_id: ConversationId| {
        doc_record_full(
            id,
            "copy.source",
            conversation_scope(conversation_id),
            None,
            Some(DocumentHistory::Rewindable),
            Some(DocumentFork::AsOf),
        )
    };
    storage
        .commit(
            &[
                StorageWrite::DocumentCopy {
                    record: child_record(historical_copy_id, child_id),
                    source: DocumentCopySource {
                        id: source_id,
                        at: DocumentPoint::At(created_at),
                    },
                },
                StorageWrite::DocumentCopy {
                    record: child_record(current_copy_id, second_child_id),
                    source: DocumentCopySource {
                        id: source_id,
                        at: DocumentPoint::Current,
                    },
                },
                StorageWrite::DocumentCopy {
                    record: child_record(retired_copy_id, root_id),
                    source: DocumentCopySource {
                        id: source_id,
                        at: DocumentPoint::Current,
                    },
                },
                doc_retire(retired_copy_id),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("copies");
    let historical = storage
        .document(historical_copy_id, DocumentPoint::Current, context())
        .await
        .expect("read")
        .expect("historical copy");
    assert_eq!(historical.version, 2);
    assert_eq!(
        historical.value,
        object(json!({ "count": 1, "rows": [{ "value": "base" }] }))
    );
    let current = storage
        .document(current_copy_id, DocumentPoint::Current, context())
        .await
        .expect("read")
        .expect("current copy");
    assert_eq!(current.version, 2);
    assert_eq!(
        current.value,
        object(json!({ "count": 2, "rows": [{ "value": "base" }, { "value": "current" }] }))
    );
    assert_eq!(
        storage
            .document(retired_copy_id, DocumentPoint::Current, context())
            .await
            .expect("read"),
        None
    );

    storage
        .commit(
            &[
                doc_base_change(source_id, 2, object(json!({ "count": 99, "rows": [] }))),
                doc_retire(source_id),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("retire source");
    assert_eq!(
        storage
            .document(current_copy_id, DocumentPoint::Current, context())
            .await
            .expect("read")
            .expect("current copy")
            .value,
        object(json!({ "count": 2, "rows": [{ "value": "base" }, { "value": "current" }] }))
    );

    let latest_source_id: DocumentId = mint(&storage, IdKind::Document).await;
    let latest_copy_id: DocumentId = mint(&storage, IdKind::Document).await;
    let latest_source = doc_record_full(
        latest_source_id,
        "copy.latest",
        conversation_scope(root_id),
        None,
        Some(DocumentHistory::Latest),
        Some(DocumentFork::Current),
    );
    storage
        .commit(
            &[doc_create(
                latest_source.clone(),
                4,
                object(json!({ "retained": "copy" })),
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("latest source");
    storage
        .commit(
            &[StorageWrite::DocumentCopy {
                record: DocumentCreate {
                    id: latest_copy_id,
                    scope: conversation_scope(child_id),
                    ..latest_source.clone()
                },
                source: DocumentCopySource {
                    id: latest_source_id,
                    at: DocumentPoint::Current,
                },
            }],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("latest copy");
    storage
        .commit(
            &[
                doc_base_change(
                    latest_source_id,
                    4,
                    object(json!({ "retained": "source-only" })),
                ),
                doc_retire(latest_source_id),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("retire latest source");
    let latest_copy = storage
        .document(latest_copy_id, DocumentPoint::Current, context())
        .await
        .expect("read")
        .expect("latest copy");
    assert_eq!(latest_copy.version, 4);
    assert_eq!(latest_copy.value, object(json!({ "retained": "copy" })));

    let conflict_id: DocumentId = mint(&storage, IdKind::Document).await;
    assert_error_contains(
        storage
            .commit(
                &[
                    StorageWrite::DocumentCopy {
                        record: child_record(conflict_id, child_id),
                        source: DocumentCopySource {
                            id: current_copy_id,
                            at: DocumentPoint::Current,
                        },
                    },
                    doc_retire(current_copy_id),
                ],
                adopt_nothing(),
                context(),
            )
            .await,
        "was rejected",
    );
    assert_eq!(
        storage
            .document(conflict_id, DocumentPoint::Current, context())
            .await
            .expect("read"),
        None
    );
    assert_eq!(
        storage
            .document(current_copy_id, DocumentPoint::Current, context())
            .await
            .expect("read")
            .expect("current copy")
            .value,
        object(json!({ "count": 2, "rows": [{ "value": "base" }, { "value": "current" }] }))
    );

    let mismatch_id: DocumentId = mint(&storage, IdKind::Document).await;
    assert_error_contains(
        storage
            .commit(
                &[StorageWrite::DocumentCopy {
                    record: DocumentCreate {
                        kind: "copy.mismatch".to_owned(),
                        ..child_record(mismatch_id, child_id)
                    },
                    source: DocumentCopySource {
                        id: current_copy_id,
                        at: DocumentPoint::Current,
                    },
                }],
                adopt_nothing(),
                context(),
            )
            .await,
        "was rejected",
    );
    assert_eq!(
        storage
            .document(mismatch_id, DocumentPoint::Current, context())
            .await
            .expect("read"),
        None
    );
}

/// "uses bases for version transitions and rejects historical reads of current-only documents"
#[tokio::test]
async fn uses_bases_for_version_transitions_and_rejects_historical_reads_of_current_only_documents()
{
    let storage = MemoryStorage::new();
    let _root_id = create_root(&storage).await;
    let id: DocumentId = mint(&storage, IdKind::Document).await;
    let record = doc_record(id, "session.settings", session_scope());
    storage
        .commit(
            &[doc_create(record, 1, object(json!({ "count": 1 })))],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("create");
    storage
        .commit(
            &[doc_delta(
                id,
                1,
                vec![Op::Set(vec![Seg::Key("count".to_owned())], json!(2))],
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("delta");
    let migrated_at = storage
        .commit(
            &[doc_base_change(id, 2, object(json!({ "count": 3 })))],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("migrate");
    let current = storage
        .document(id, DocumentPoint::Current, context())
        .await
        .expect("read")
        .expect("document");
    assert_eq!(current.version, 2);
    assert_eq!(current.value, object(json!({ "count": 3 })));
    assert_error_contains(
        storage
            .document(id, DocumentPoint::At(migrated_at), context())
            .await,
        "does not retain historical content",
    );
    assert_error_contains(
        storage
            .commit(
                &[doc_delta(
                    id,
                    1,
                    vec![Op::Set(vec![Seg::Key("count".to_owned())], json!(4))],
                )],
                adopt_nothing(),
                context(),
            )
            .await,
        "version transition requires a base",
    );
    assert_eq!(
        storage
            .document(id, DocumentPoint::Current, context())
            .await
            .expect("read")
            .expect("document")
            .value,
        object(json!({ "count": 3 }))
    );
    storage
        .commit(&[doc_retire(id)], adopt_nothing(), context())
        .await
        .expect("retire");
    assert_eq!(
        storage
            .document(id, DocumentPoint::Current, context())
            .await
            .expect("read"),
        None
    );
}

/// "indexes logical addresses and exact-scope scans independently"
#[tokio::test]
async fn indexes_logical_addresses_and_exact_scope_scans_independently() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let first_id: DocumentId = mint(&storage, IdKind::Document).await;
    let second_id: DocumentId = mint(&storage, IdKind::Document).await;
    let conversation_document_id: DocumentId = mint(&storage, IdKind::Document).await;
    let task_id: TaskId = mint(&storage, IdKind::Task).await;
    let task_singleton_id: DocumentId = mint(&storage, IdKind::Document).await;
    let task_family_id: DocumentId = mint(&storage, IdKind::Document).await;
    let task_other_kind_id: DocumentId = mint(&storage, IdKind::Document).await;
    let created_at = storage
        .commit(
            &[
                task_write(pending_task(task_id, root_id)),
                doc_create(
                    doc_record_full(
                        first_id,
                        "cache",
                        session_scope(),
                        Some(json!("__proto__")),
                        None,
                        None,
                    ),
                    1,
                    object(json!({ "owner": "first" })),
                ),
                doc_create(
                    doc_record_full(
                        second_id,
                        "cache",
                        session_scope(),
                        Some(json!("constructor")),
                        None,
                        None,
                    ),
                    1,
                    object(json!({ "owner": "second" })),
                ),
                doc_create(
                    doc_record_full(
                        conversation_document_id,
                        "cache",
                        conversation_scope(root_id),
                        Some(json!("__proto__")),
                        Some(DocumentHistory::Latest),
                        Some(DocumentFork::Current),
                    ),
                    1,
                    object(json!({ "owner": "conversation" })),
                ),
                doc_create(
                    doc_record_full(
                        task_singleton_id,
                        "task.cache",
                        DocumentScope::Task { task_id },
                        None,
                        None,
                        None,
                    ),
                    1,
                    object(json!({ "owner": "singleton" })),
                ),
                doc_create(
                    doc_record_full(
                        task_family_id,
                        "task.cache",
                        DocumentScope::Task { task_id },
                        Some(json!("member")),
                        None,
                        None,
                    ),
                    1,
                    object(json!({ "owner": "family" })),
                ),
                doc_create(
                    doc_record_full(
                        task_other_kind_id,
                        "task.other",
                        DocumentScope::Task { task_id },
                        None,
                        None,
                        None,
                    ),
                    1,
                    object(json!({ "owner": "other" })),
                ),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit documents");

    let first_address = DocumentAddress {
        kind: "cache".to_owned(),
        scope: session_scope(),
        key: Some(json!("__proto__")),
    };
    assert_eq!(
        storage
            .find_document(&first_address, DocumentPoint::Current, context())
            .await
            .expect("find")
            .map(|record| record.id),
        Some(first_id)
    );
    let session_query = DocumentQuery {
        scope: session_scope(),
        at: DocumentPoint::Current,
        kind: None,
    };
    let first_page = storage
        .scan_documents(&session_query, 1, None, context())
        .await
        .expect("session page");
    assert_eq!(first_page.items.len(), 1);
    let (first_page_ids, first_next) = scan_document_ids(&storage, &session_query, 1, None)
        .await
        .expect("session page");
    let (second_page_ids, second_next) =
        scan_document_ids(&storage, &session_query, 1, first_next.as_ref())
            .await
            .expect("session continuation");
    assert_eq!(
        [first_page_ids, second_page_ids].concat(),
        vec![first_id.get(), second_id.get()]
    );
    assert_eq!(second_next, None);
    let (conversation_ids, _) = scan_document_ids(
        &storage,
        &DocumentQuery {
            scope: conversation_scope(root_id),
            at: DocumentPoint::Current,
            kind: None,
        },
        10,
        None,
    )
    .await
    .expect("conversation scope");
    assert_eq!(conversation_ids, vec![conversation_document_id.get()]);
    assert_eq!(
        storage
            .find_document(
                &DocumentAddress {
                    kind: "task.cache".to_owned(),
                    scope: DocumentScope::Task { task_id },
                    key: None,
                },
                DocumentPoint::Current,
                context(),
            )
            .await
            .expect("find")
            .map(|record| record.id),
        Some(task_singleton_id)
    );
    assert_eq!(
        storage
            .find_document(
                &DocumentAddress {
                    kind: "task.cache".to_owned(),
                    scope: DocumentScope::Task { task_id },
                    key: Some(json!("member")),
                },
                DocumentPoint::Current,
                context(),
            )
            .await
            .expect("find")
            .map(|record| record.id),
        Some(task_family_id)
    );
    let (task_cache_ids, _) = scan_document_ids(
        &storage,
        &DocumentQuery {
            scope: DocumentScope::Task { task_id },
            at: DocumentPoint::Current,
            kind: Some("task.cache".to_owned()),
        },
        10,
        None,
    )
    .await
    .expect("task scope");
    assert_eq!(
        task_cache_ids,
        vec![task_singleton_id.get(), task_family_id.get()]
    );
    assert_error_contains(
        storage
            .document(task_singleton_id, DocumentPoint::At(created_at), context())
            .await,
        "does not retain historical content",
    );
}

/// "keeps document lifecycle failures atomic and gives create-plus-retire an empty lifetime"
#[tokio::test]
async fn keeps_document_lifecycle_failures_atomic_and_gives_create_plus_retire_an_empty_lifetime() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let first_id: DocumentId = mint(&storage, IdKind::Document).await;
    let second_id: DocumentId = mint(&storage, IdKind::Document).await;
    let record = doc_record(first_id, "singleton", session_scope());
    storage
        .commit(
            &[doc_create(record.clone(), 1, object(json!({ "value": 1 })))],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("create singleton");
    assert_error_contains(
        storage
            .commit(
                &[
                    doc_create(
                        DocumentCreate {
                            id: second_id,
                            ..record.clone()
                        },
                        1,
                        object(json!({ "value": 2 })),
                    ),
                    doc_delta(first_id, 1, Vec::new()),
                ],
                adopt_nothing(),
                context(),
            )
            .await,
        "already has a current incarnation",
    );
    assert_eq!(
        storage
            .document(first_id, DocumentPoint::Current, context())
            .await
            .expect("read")
            .expect("document")
            .value,
        object(json!({ "value": 1 }))
    );
    assert_eq!(
        storage
            .document(second_id, DocumentPoint::Current, context())
            .await
            .expect("read"),
        None
    );

    let empty_id: DocumentId = mint(&storage, IdKind::Document).await;
    let empty_at = storage
        .commit(
            &[
                doc_create(
                    doc_record_full(
                        empty_id,
                        "singleton",
                        conversation_scope(root_id),
                        Some(json!("empty")),
                        Some(DocumentHistory::Rewindable),
                        Some(DocumentFork::Initial),
                    ),
                    1,
                    object(json!({})),
                ),
                doc_retire(empty_id),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("create and retire");
    assert_eq!(
        storage
            .document(empty_id, DocumentPoint::Current, context())
            .await
            .expect("read"),
        None
    );
    assert_eq!(
        storage
            .document(empty_id, DocumentPoint::At(empty_at), context())
            .await
            .expect("read"),
        None
    );
    assert_eq!(
        storage
            .find_document(
                &DocumentAddress {
                    kind: "singleton".to_owned(),
                    scope: conversation_scope(root_id),
                    key: Some(json!("empty")),
                },
                DocumentPoint::At(empty_at),
                context(),
            )
            .await
            .expect("find"),
        None
    );
}

/// "rolls back record tables and secondary indexes when a document command fails"
#[tokio::test]
async fn rolls_back_record_tables_and_secondary_indexes_when_a_document_command_fails() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let task_id: TaskId = mint(&storage, IdKind::Task).await;
    let submission_id: SubmissionId = mint(&storage, IdKind::Submission).await;
    let document_id: DocumentId = mint(&storage, IdKind::Document).await;
    let task = pending_task(task_id, root_id);
    let stored_submission = submission(
        submission_id,
        root_id,
        Some("atomic"),
        SubmissionKind::Input,
        SubmissionStatus::Queued,
    );
    let record = doc_record(document_id, "atomic", session_scope());
    let baseline_seq = storage
        .commit(
            &[
                task_write(task.clone()),
                submission_write(stored_submission.clone()),
                doc_create(record.clone(), 1, object(json!({ "count": 1 }))),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("baseline");

    let entry_id: EntryId = mint(&storage, IdKind::Entry).await;
    let conflicting_document_id: DocumentId = mint(&storage, IdKind::Document).await;
    assert_error_contains(
        storage
            .commit(
                &[
                    task_write(TaskRecord {
                        state: TaskState::Running {
                            checkpoint: json!({ "phase": "effect" }),
                        },
                        ..task.clone()
                    }),
                    submission_write(SubmissionRecord {
                        status: SubmissionStatus::Unanswered,
                        reason: Some("failed".to_owned()),
                        ..stored_submission.clone()
                    }),
                    entry(entry_id, root_id, "transient"),
                    doc_create(
                        DocumentCreate {
                            id: conflicting_document_id,
                            ..record.clone()
                        },
                        1,
                        object(json!({ "count": 2 })),
                    ),
                ],
                adopt_nothing(),
                context(),
            )
            .await,
        "already has a current incarnation",
    );

    assert_eq!(
        storage.task(task_id, context()).await.expect("task"),
        Some(task.clone())
    );
    assert_eq!(
        storage
            .scan_tasks(
                &TaskQuery {
                    status: Some(TaskStatus::Pending),
                    ..TaskQuery::default()
                },
                10,
                None,
                context(),
            )
            .await
            .expect("scan")
            .items,
        vec![task]
    );
    assert_eq!(
        storage
            .submission_by_request(root_id, "atomic", context())
            .await
            .expect("by request"),
        Some(stored_submission)
    );
    assert_eq!(
        storage.entry(entry_id, context()).await.expect("entry"),
        None
    );
    assert_eq!(
        storage
            .document(conflicting_document_id, DocumentPoint::Current, context())
            .await
            .expect("read"),
        None
    );
    assert_eq!(
        storage
            .find_document(
                &DocumentAddress {
                    kind: record.kind.clone(),
                    scope: record.scope,
                    key: None,
                },
                DocumentPoint::Current,
                context(),
            )
            .await
            .expect("find")
            .map(|found| found.id),
        Some(document_id)
    );
    let after_rollback_seq = storage
        .commit(
            &[doc_delta(
                document_id,
                1,
                vec![Op::Set(vec![Seg::Key("count".to_owned())], json!(3))],
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit after rollback");
    assert!(after_rollback_seq > baseline_seq);
}

/// "keeps indexed string identities lossless"
///
/// TS 用孤立代理项（`"\ud800"` / `"\ud801"`）验证索引键不做 Unicode 规范化。Rust 的 `String`
/// 无法表示未配对的代理项（见 ARCHITECTURE §4.2 的 UTF-16 说明），因此这里改用“同一字形的
/// 组合式 / 分解式拼写”——同样验证键按码点逐字比较、不做 NFC/NFD 归一化。
#[tokio::test]
async fn keeps_indexed_string_identities_lossless() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let first = "\u{e9}";
    let second = "e\u{301}";
    assert_ne!(first, second);
    let first_task_id: TaskId = mint(&storage, IdKind::Task).await;
    let second_task_id: TaskId = mint(&storage, IdKind::Task).await;
    let first_submission_id: SubmissionId = mint(&storage, IdKind::Submission).await;
    let second_submission_id: SubmissionId = mint(&storage, IdKind::Submission).await;
    let first_kind_document_id: DocumentId = mint(&storage, IdKind::Document).await;
    let second_kind_document_id: DocumentId = mint(&storage, IdKind::Document).await;
    let first_key_document_id: DocumentId = mint(&storage, IdKind::Document).await;
    let second_key_document_id: DocumentId = mint(&storage, IdKind::Document).await;
    storage
        .commit(
            &[
                task_write(TaskRecord {
                    kind: first.to_owned(),
                    ..pending_task(first_task_id, root_id)
                }),
                task_write(TaskRecord {
                    kind: second.to_owned(),
                    ..pending_task(second_task_id, root_id)
                }),
                submission_write(submission(
                    first_submission_id,
                    root_id,
                    Some(first),
                    SubmissionKind::Input,
                    SubmissionStatus::Queued,
                )),
                submission_write(submission(
                    second_submission_id,
                    root_id,
                    Some(second),
                    SubmissionKind::Input,
                    SubmissionStatus::Queued,
                )),
                doc_create(
                    doc_record(first_kind_document_id, first, session_scope()),
                    1,
                    object(json!({ "identity": "first kind" })),
                ),
                doc_create(
                    doc_record(second_kind_document_id, second, session_scope()),
                    1,
                    object(json!({ "identity": "second kind" })),
                ),
                doc_create(
                    doc_record_full(
                        first_key_document_id,
                        "family",
                        session_scope(),
                        Some(json!(first)),
                        None,
                        None,
                    ),
                    1,
                    object(json!({ "identity": "first key" })),
                ),
                doc_create(
                    doc_record_full(
                        second_key_document_id,
                        "family",
                        session_scope(),
                        Some(json!(second)),
                        None,
                        None,
                    ),
                    1,
                    object(json!({ "identity": "second key" })),
                ),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit identities");

    let scan_kind = |kind: &'static str| {
        let storage = storage.clone();
        async move {
            storage
                .scan_tasks(
                    &TaskQuery {
                        kind: Some(kind.to_owned()),
                        ..TaskQuery::default()
                    },
                    10,
                    None,
                    context(),
                )
                .await
                .expect("scan")
                .items
        }
    };
    assert_eq!(
        ids_of_tasks(&Page {
            items: scan_kind(first).await,
            next: None
        }),
        vec![first_task_id.get()]
    );
    assert_eq!(
        ids_of_tasks(&Page {
            items: scan_kind(second).await,
            next: None
        }),
        vec![second_task_id.get()]
    );
    assert_eq!(
        storage
            .task(first_task_id, context())
            .await
            .expect("task")
            .map(|task| task.kind),
        Some(first.to_owned())
    );
    assert_eq!(
        storage
            .task(second_task_id, context())
            .await
            .expect("task")
            .map(|task| task.kind),
        Some(second.to_owned())
    );
    assert_eq!(
        storage
            .submission_by_request(root_id, first, context())
            .await
            .expect("by request")
            .and_then(|record| record.request_id),
        Some(first.to_owned())
    );
    assert_eq!(
        storage
            .submission_by_request(root_id, first, context())
            .await
            .expect("by request")
            .map(|record| record.id),
        Some(first_submission_id)
    );
    assert_eq!(
        storage
            .submission_by_request(root_id, second, context())
            .await
            .expect("by request")
            .map(|record| record.id),
        Some(second_submission_id)
    );
    assert_eq!(
        storage
            .find_document(
                &DocumentAddress {
                    kind: first.to_owned(),
                    scope: session_scope(),
                    key: None,
                },
                DocumentPoint::Current,
                context(),
            )
            .await
            .expect("find")
            .map(|record| record.id),
        Some(first_kind_document_id)
    );
    assert_eq!(
        storage
            .find_document(
                &DocumentAddress {
                    kind: second.to_owned(),
                    scope: session_scope(),
                    key: None,
                },
                DocumentPoint::Current,
                context(),
            )
            .await
            .expect("find")
            .map(|record| record.id),
        Some(second_kind_document_id)
    );
    assert_eq!(
        storage
            .find_document(
                &DocumentAddress {
                    kind: "family".to_owned(),
                    scope: session_scope(),
                    key: Some(json!(first)),
                },
                DocumentPoint::Current,
                context(),
            )
            .await
            .expect("find")
            .map(|record| record.id),
        Some(first_key_document_id)
    );
    assert_eq!(
        storage
            .find_document(
                &DocumentAddress {
                    kind: "family".to_owned(),
                    scope: session_scope(),
                    key: Some(json!(second)),
                },
                DocumentPoint::Current,
                context(),
            )
            .await
            .expect("find")
            .map(|record| record.id),
        Some(second_key_document_id)
    );
    let (kind_scan_ids, _) = scan_document_ids(
        &storage,
        &DocumentQuery {
            scope: session_scope(),
            at: DocumentPoint::Current,
            kind: Some(first.to_owned()),
        },
        10,
        None,
    )
    .await
    .expect("scan");
    assert_eq!(kind_scan_ids, vec![first_kind_document_id.get()]);
}

/// "keeps one global record ID namespace and rejects exhausted ID minting"
#[tokio::test]
async fn keeps_one_global_record_id_namespace_and_rejects_exhausted_id_minting() {
    let storage = MemoryStorage::new();
    let root_id = create_root(&storage).await;
    let explicit_entry_id: EntryId = id_from_number(100);
    storage
        .commit(
            &[entry(explicit_entry_id, root_id, "message")],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("explicit ID");
    assert_eq!(
        mint::<kiana_durable::EntryBrand>(&storage, IdKind::Entry)
            .await
            .get(),
        101
    );
    assert_error_contains(
        storage
            .commit(
                &[task_write(pending_task(id_from_number(100), root_id))],
                adopt_nothing(),
                context(),
            )
            .await,
        "ID 100 already belongs to entry",
    );

    storage
        .commit(
            &[entry_write(entry_record(
                id_from_number(9_007_199_254_740_991),
                root_id,
                "last-id",
            ))],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("last safe ID");
    assert_error_contains(
        storage.mint_id(IdKind::Entry).await,
        "ID space is exhausted",
    );
    assert_error_contains(
        storage.mint_id(IdKind::Entry).await,
        "ID space is exhausted",
    );
}

/// "rejects every operation after close"
#[tokio::test]
async fn rejects_every_operation_after_close() {
    let storage = MemoryStorage::new();
    let _root_id = create_root(&storage).await;
    storage.close(context()).await;
    assert_error_contains(
        storage.conversation(ROOT_CONVERSATION_ID, context()).await,
        "closed",
    );
    assert_error_contains(
        storage.commit(&[], adopt_nothing(), context()).await,
        "closed",
    );
    assert_error_contains(storage.mint_id(IdKind::Conversation).await, "closed");
}

/// `memory-storage.test.ts` 的独立用例："does not expose retained state through a prepared commit"。
///
/// TS 断言 `prepared.writes` 深度冻结（`push` 抛错）。Rust 的 `writes()` 只给出
/// `&[StorageWrite]`，外部无法改写；这里验证可读性、`apply()` 的幂等以及读回值不被泄漏。
#[tokio::test]
async fn does_not_expose_retained_state_through_a_prepared_commit() {
    let storage = MemoryStorage::new();
    storage
        .commit(
            &[conversation(ROOT_CONVERSATION_ID)],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("root conversation");
    let entry_id: EntryId = id_from_number(2);
    let prepared = storage
        .prepare_commit(
            &[entry_write(entry_record_with(
                entry_id,
                ROOT_CONVERSATION_ID,
                "test",
                Some(json!({ "nested": [1] })),
                None,
            ))],
            None,
        )
        .expect("prepare");
    match &prepared.writes()[0] {
        StorageWrite::Entry { value } => assert_eq!(value.data, Some(json!({ "nested": [1] }))),
        other => panic!("expected an entry write, got {other:?}"),
    }
    assert_eq!(prepared.apply().expect("apply"), seq_from_number(2));
    assert_eq!(prepared.apply().expect("apply"), seq_from_number(2));
    assert_eq!(
        storage
            .entry(entry_id, context())
            .await
            .expect("read")
            .expect("entry")
            .entry
            .data,
        Some(json!({ "nested": [1] }))
    );
}

/// 回归：`page()` 在 `limit == 0` 且存在匹配行时，TS 会在 `items.at(-1)!.id` 处抛
/// `TypeError: Cannot read properties of undefined (reading 'id')`（`slice(0, 0)` 得到空数组，
/// 但 `values.length <= limit` 为假）。Rust 用同文案的错误复现，不能静默返回空页。
#[tokio::test]
async fn scans_with_limit_zero_and_matching_rows_fail_like_typescript() {
    let storage = MemoryStorage::new();
    let root = create_root(&storage).await;
    let entry_id: EntryId = id_from_number(2);
    let task_id: TaskId = id_from_number(3);
    let submission_id: SubmissionId = id_from_number(4);
    let document_id: DocumentId = id_from_number(5);
    storage
        .commit(
            &[
                entry(entry_id, root, "test"),
                task_write(pending_task(task_id, root)),
                submission_write(submission(
                    submission_id,
                    root,
                    None,
                    SubmissionKind::Input,
                    SubmissionStatus::Queued,
                )),
                doc_create(
                    doc_record(document_id, "test.doc", DocumentScope::Session),
                    1,
                    object(json!({ "a": 1 })),
                ),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("seed one row in every table");

    let entry_query = EntryQuery {
        conversation_id: root,
        min_entry_id: None,
        max_entry_id: None,
        order: None,
    };
    let document_query = DocumentQuery {
        scope: DocumentScope::Session,
        at: DocumentPoint::Current,
        kind: None,
    };
    let failures = [
        (
            "scanConversations",
            message_of(
                storage
                    .scan_conversations(&ConversationQuery::default(), 0, None, context())
                    .await,
            ),
        ),
        (
            "scanEntries",
            message_of(storage.scan_entries(&entry_query, 0, None, context()).await),
        ),
        (
            "scanTasks",
            message_of(
                storage
                    .scan_tasks(&TaskQuery::default(), 0, None, context())
                    .await,
            ),
        ),
        (
            "scanSubmissions",
            message_of(
                storage
                    .scan_submissions(&SubmissionQuery::default(), 0, None, context())
                    .await,
            ),
        ),
        (
            "scanDocuments",
            message_of(
                storage
                    .scan_documents(&document_query, 0, None, context())
                    .await,
            ),
        ),
    ];
    for (name, message) in failures {
        assert_eq!(
            message, "Cannot read properties of undefined (reading 'id')",
            "{name} must throw on limit 0 with matching rows"
        );
    }

    // 没有匹配行时 TS 的 `values.length <= limit` 成立，返回空页而不抛错。
    let empty = MemoryStorage::new();
    let page = empty
        .scan_conversations(&ConversationQuery::default(), 0, None, context())
        .await
        .expect("an empty scan with limit 0 succeeds");
    assert!(page.items.is_empty());
    assert_eq!(page.next, None);
}

/// 类型边界（reviewer finding 1）：TS 的 `applyImmutableBatches(...) as JsonObject` 只是编译期
/// 断言——根被 `["r", [1, 2]]` 替换后 `document()` 会原样返回数组。Rust 的
/// `StoredDocument.value` 是冻结契约 `JsonObject`（`IndexMap`），无法表示非对象根，因此读取点
/// 显式报错，而不是静默丢弃或伪造值。提交本身成功。
#[tokio::test]
async fn document_materializing_a_non_object_root_reports_the_type_boundary() {
    let storage = MemoryStorage::new();
    let document_id: DocumentId = id_from_number(2);
    storage
        .commit(
            &[doc_create(
                doc_record(document_id, "test.doc", DocumentScope::Session),
                1,
                object(json!({ "a": 1 })),
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("create the document");
    storage
        .commit(
            &[doc_delta(document_id, 1, vec![Op::Replace(json!([1, 2]))])],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("a root replace commits like in TypeScript");
    assert_eq!(
        message_of(
            storage
                .document(document_id, DocumentPoint::Current, context())
                .await
        ),
        "Document 2 materialized a non-object value"
    );
}

// ---------------------------------------------------------------------------
// 审查回归（review findings）
// ---------------------------------------------------------------------------

/// 不可提交的父会话（reviewer finding：critical）。
///
/// TS 的 `visibleEntries` / `visibleEntriesAscending` / `findLatestHeadMarker` 都是生成器 /
/// 惰性遍历：`entry()` 只取第一条、`scanEntries` 至多取 `limit + 1` 条，因此链上缺失的父会话
/// 只有在遍历真正走到它时才抛 `TypeError`；`findLatestHeadMarker` 只要当前会话里有游标内的
/// head 条目就直接返回。Rust 之前会急切走完整条链并 `expect`，导致读取 panic（毒化互斥锁）。
#[tokio::test]
async fn reads_a_fork_chain_whose_parent_was_never_committed_like_typescript() {
    const PARENT_ERROR: &str = "Cannot read properties of undefined (reading 'parent')";

    let storage = MemoryStorage::new();
    create_root(&storage).await;
    let child: ConversationId = id_from_number(2);
    storage
        .commit(
            &[conversation_with(
                child,
                Some(ConversationParent {
                    conversation_id: id_from_number(999),
                    at: id_from_number(1),
                }),
                None,
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit a child whose parent is not in storage (TS accepts this too)");
    let first: EntryId = id_from_number(0);
    let second: EntryId = id_from_number(5);
    storage
        .commit(
            &[
                entry_write(entry_record_with(first, child, "test", None, Some(first))),
                entry_write(entry_record_with(second, child, "test", None, Some(second))),
            ],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit entries in the child conversation");

    // entry() 只拉取生成器的第一条：命中就返回，绝不再看父会话。
    let stored = storage
        .visible_entry(child, first, context())
        .await
        .expect("a first-segment hit does not walk the chain")
        .expect("the entry is visible");
    assert_eq!(stored.entry.id, first);
    assert_eq!(stored.commit_seq.get(), 3);
    assert_eq!(
        storage
            .visible_entry(child, second, context())
            .await
            .expect("first yield")
            .map(|stored| stored.entry.id),
        Some(second)
    );
    // 边界内没有条目，但 min 上界让链的遍历在缺失父会话之前停止。
    assert_eq!(
        storage
            .visible_entry(child, id_from_number(3), context())
            .await
            .expect("the min bound stops the walk before the missing parent"),
        None
    );

    // limit 1 时只拉 2 条（limit + 1），同样不触碰父会话。
    let page = storage
        .scan_entries(
            &EntryQuery {
                conversation_id: child,
                min_entry_id: None,
                max_entry_id: None,
                order: None,
            },
            1,
            None,
            context(),
        )
        .await
        .expect("a lazily satisfied limit never walks to the parent");
    assert_eq!(ids_of_entries(&page), vec![5]);
    let cursor = page.next.expect("one more entry is visible");
    assert_eq!(cursor.get("after"), Some(&JsonValue::from(5)));
    assert_eq!(
        cursor.get("order"),
        Some(&JsonValue::String("descending".to_owned()))
    );

    // 真正走完整条链才是 TS 的 TypeError（可捕获，不 panic）。
    let ascending = EntryQuery {
        conversation_id: child,
        min_entry_id: None,
        max_entry_id: None,
        order: Some(ScanOrder::Ascending),
    };
    let descending = EntryQuery {
        order: None,
        ..ascending.clone()
    };
    assert_eq!(
        message_of(storage.scan_entries(&descending, 10, None, context()).await),
        PARENT_ERROR
    );
    assert_eq!(
        message_of(storage.scan_entries(&ascending, 10, None, context()).await),
        PARENT_ERROR
    );
    // ascending 在第一次 yield 之前就建好整条链，所以 limit 0 也报错。
    assert_eq!(
        message_of(storage.scan_entries(&ascending, 0, None, context()).await),
        PARENT_ERROR
    );
    // 错误穿过互斥锁后状态仍可读（Rust 端没有 panic/毒化）。
    assert!(
        storage
            .visible_entry(child, first, context())
            .await
            .expect("reads still work after a chain error")
            .is_some()
    );

    // findLatestHeadMarker 在游标内找到 head 条目时直接返回。
    assert_eq!(
        storage
            .find_latest_head_marker(child, None, context())
            .await
            .expect("head marker in the first segment")
            .map(|marker| (marker.id.get(), marker.head.map(|head| head.get()))),
        Some((5, Some(5)))
    );
    assert_eq!(
        storage
            .find_latest_head_marker(child, Some(id_from_number(4)), context())
            .await
            .expect("head marker below the bound")
            .map(|marker| marker.id.get()),
        Some(0)
    );

    // 只有「游标内没有 head 条目」的查询才会走到缺失的父会话。
    let only_above = MemoryStorage::new();
    create_root(&only_above).await;
    only_above
        .commit(
            &[conversation_with(
                child,
                Some(ConversationParent {
                    conversation_id: id_from_number(999),
                    at: id_from_number(1),
                }),
                None,
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit the child");
    let marker_id: EntryId = id_from_number(7);
    only_above
        .commit(
            &[entry_write(entry_record_with(
                marker_id,
                child,
                "test",
                None,
                Some(marker_id),
            ))],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("commit a head entry");
    assert_eq!(
        message_of(
            only_above
                .find_latest_head_marker(child, Some(id_from_number(3)), context())
                .await
        ),
        PARENT_ERROR
    );
    assert!(
        only_above
            .find_latest_head_marker(child, None, context())
            .await
            .expect("the marker is still readable after the error")
            .is_some()
    );
}

/// 缺失 `ops` 的 delta 修订（reviewer finding：major）。
///
/// TS 的 `materializeDocument` 把 `revision.ops` 交给 chord 的 `applyImmutableBatches`，后者对
/// `undefined` / `null` 执行 `for...of`，抛 `TypeError: ops is not iterable`。提交本身是合法的
/// （`checkDocumentActions` 只看 `kind`/`version`），只有读取时按需失败。
#[tokio::test]
async fn rejects_a_document_delta_without_ops_like_typescript() {
    let storage = MemoryStorage::new();
    create_root(&storage).await;
    let document_id: DocumentId = id_from_number(2);
    storage
        .commit(
            &[doc_create(
                doc_record(
                    document_id,
                    "k",
                    DocumentScope::Conversation {
                        conversation_id: ROOT_CONVERSATION_ID,
                    },
                ),
                1,
                object(json!({ "a": 1 })),
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("create the base document");
    storage
        .commit(
            &[doc_change(
                document_id,
                DocumentContent {
                    version: 1,
                    kind: DocumentContentKind::Delta,
                    value: None,
                    ops: None,
                },
            )],
            adopt_nothing(),
            context(),
        )
        .await
        .expect("TS accepts a delta without ops at commit time");

    // 读取点如果落在坏 delta 之前，回放不会碰到它。
    let before = storage
        .document(
            document_id,
            DocumentPoint::At(seq_from_number(2)),
            context(),
        )
        .await
        .expect("the base alone materializes")
        .expect("the document exists at seq 2");
    assert_eq!(before.value, object(json!({ "a": 1 })));
    assert_eq!(before.deltas_since_base, 0);

    assert_eq!(
        message_of(
            storage
                .document(document_id, DocumentPoint::Current, context())
                .await
        ),
        "ops is not iterable"
    );
    // 后续读取不受影响（错误不是 panic）。
    assert!(
        storage
            .document(
                document_id,
                DocumentPoint::At(seq_from_number(2)),
                context()
            )
            .await
            .expect("later reads still work")
            .is_some()
    );
}

/// 显式 JSON `key: null` 的文档地址（reviewer finding：minor）。
///
/// TS 的 `addressKey` 用 `key === undefined` 区分：缺省 key 是 `["singleton"]`，显式 `null`
/// 是 `["family", null]`。两者可以并存，`findDocument({..., key: null})` 命中的是后者。
#[tokio::test]
async fn keeps_an_explicit_json_null_document_key_distinct_from_an_absent_one() {
    let storage = MemoryStorage::new();
    create_root(&storage).await;
    let singleton: StorageWrite = serde_json::from_value(json!({
        "type": "document.create",
        "record": { "id": 2, "kind": "k", "scope": { "kind": "session" } },
        "content": { "version": 1, "kind": "base", "value": { "which": "absent" } }
    }))
    .expect("decode the singleton create");
    let null_key: StorageWrite = serde_json::from_value(json!({
        "type": "document.create",
        "record": { "id": 3, "kind": "k", "key": null, "scope": { "kind": "session" } },
        "content": { "version": 1, "kind": "base", "value": { "which": "null" } }
    }))
    .expect("decode the null-key create");
    storage
        .commit(&[singleton], adopt_nothing(), context())
        .await
        .expect("the singleton address is free");
    storage
        .commit(&[null_key], adopt_nothing(), context())
        .await
        .expect("an explicit null key is a family member, not the singleton");

    let found = storage
        .find_document(
            &DocumentAddress {
                kind: "k".to_owned(),
                scope: DocumentScope::Session,
                key: Some(JsonValue::Null),
            },
            DocumentPoint::Current,
            context(),
        )
        .await
        .expect("find the null-key lookup")
        .expect("the null-key incarnation exists");
    assert_eq!(found.id.get(), 3);
    assert_eq!(found.key, Some(JsonValue::Null));

    let singleton_record = storage
        .find_document(
            &DocumentAddress {
                kind: "k".to_owned(),
                scope: DocumentScope::Session,
                key: None,
            },
            DocumentPoint::Current,
            context(),
        )
        .await
        .expect("find the singleton lookup")
        .expect("the singleton incarnation exists");
    assert_eq!(singleton_record.id.get(), 2);
    assert_eq!(singleton_record.key, None);

    let page = storage
        .scan_documents(
            &DocumentQuery {
                scope: DocumentScope::Session,
                at: DocumentPoint::Current,
                kind: None,
            },
            10,
            None,
            context(),
        )
        .await
        .expect("scan the session scope");
    assert_eq!(ids_of_documents(&page), vec![2, 3]);
}
