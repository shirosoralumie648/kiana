//! 移植自 `packages/durable/test/types.test.ts`。
//!
//! 原文件绝大部分是 TS 的编译期断言（`expectTypeOf` / `@ts-expect-error`）。Rust 的品牌与
//! 判别联合在类型层强制，无法在运行期“断言不能编译”；这里保留 **运行期可表达** 的部分：
//! ID 的裸数字形状、`JSON.stringify` 键序、以及各记录的 JSON 形状。
//!
//! 偏差：TS 的 `@ts-expect-error` 用例（会话 ID 不能赋给任务 ID、`base` 不能带 `ops` 等）
//! 在 Rust 里由类型系统保证，但没有运行期断言（见交付说明）。

use serde_json::json;

use kiana_durable::{
    ContextEdit, ContextEditAction, ConversationId, DocumentBaseContent, DocumentContent,
    DocumentCreate, DocumentFork, DocumentHistory, DocumentId, DocumentRecord, DocumentScope,
    EntryId, ROOT_CONVERSATION_ID, ScanOrder, Seq, StorageWrite, SubmissionId, SubmissionKind,
    SubmissionRecord, SubmissionStatus, TaskId, TaskOutcome, TaskState, id_from_number,
    seq_from_number,
};

fn value_of<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).expect("serializes")
}

/// `serde_json::Value` 的对象是 `serde_json::Map`；durable 的 `JsonObject` 是 `IndexMap`。
fn object(value: serde_json::Value) -> kiana_durable::JsonObject {
    value
        .as_object()
        .expect("object")
        .iter()
        .map(|(key, item)| (key.clone(), item.clone()))
        .collect()
}

#[test]
fn brands_numeric_ids_by_record_kind_and_carries_task_result_types() {
    let conversation_id: ConversationId = id_from_number(1);
    let entry_id: EntryId = id_from_number(2);
    let task_id: TaskId<u32> = id_from_number(4);
    let document_id: DocumentId = id_from_number(6);
    let _submission_id: SubmissionId = id_from_number(5);
    let seq: Seq = seq_from_number(1);

    assert_eq!(value_of(&conversation_id), json!(1));
    assert_eq!(serde_json::to_string(&task_id).unwrap(), "4");
    assert_eq!(ROOT_CONVERSATION_ID.get(), 1);
    assert_eq!(value_of(&seq), json!(1));

    // 品牌在类型层：不同 kind 的 Id 不能互相赋值；这里只能验证裸数字的同一性。
    assert_eq!(document_id.get(), 6);
    assert_eq!(entry_id.get(), 2);
}

#[test]
fn encodes_discriminator_dependent_fields() {
    let conversation_id: ConversationId = id_from_number(1);
    let entry_id: EntryId = id_from_number(2);
    let answer_id: EntryId = id_from_number(3);
    let submission_id: SubmissionId = id_from_number(5);
    let task_id: TaskId = id_from_number(4);
    let document_id: DocumentId = id_from_number(6);
    let seq = seq_from_number(1);

    let omit = ContextEdit {
        target: entry_id,
        action: ContextEditAction::Omit,
        messages: None,
    };
    let replace = ContextEdit {
        target: entry_id,
        action: ContextEditAction::Replace,
        messages: Some(Vec::new()),
    };
    assert_eq!(
        serde_json::to_string(&omit).unwrap(),
        r#"{"target":2,"action":"omit"}"#
    );
    assert_eq!(
        serde_json::to_string(&replace).unwrap(),
        r#"{"target":2,"action":"replace","messages":[]}"#
    );

    let pending: TaskState<serde_json::Value, serde_json::Value> = TaskState::Pending {
        checkpoint: json!({ "phase": "ready" }),
    };
    assert_eq!(
        serde_json::to_string(&pending).unwrap(),
        r#"{"status":"pending","checkpoint":{"phase":"ready"}}"#
    );
    let terminal: TaskState<serde_json::Value, serde_json::Value> = TaskState::Terminal {
        outcome: TaskOutcome::Completed {
            result: json!({ "value": 1 }),
        },
    };
    assert_eq!(
        serde_json::to_string(&terminal).unwrap(),
        r#"{"status":"terminal","outcome":{"status":"completed","result":{"value":1}}}"#
    );

    let completed_input = SubmissionRecord {
        key_order: None,
        trace: None,
        id: submission_id,
        conversation_id,
        request_id: None,
        kind: SubmissionKind::Input,
        status: SubmissionStatus::Done,
        entry: Some(entry_id),
        answer: Some(answer_id),
        reason: None,
        detail: None,
    };
    assert_eq!(
        serde_json::to_string(&completed_input).unwrap(),
        r#"{"conversationId":1,"type":"input","status":"done","entry":2,"id":5,"answer":3}"#
    );

    let completed_write = SubmissionRecord {
        kind: SubmissionKind::Write,
        answer: None,
        ..completed_input.clone()
    };
    assert_eq!(
        serde_json::to_string(&completed_write).unwrap(),
        r#"{"conversationId":1,"type":"write","status":"done","entry":2,"id":5}"#
    );

    let queued_write = SubmissionRecord::write_queued(submission_id, conversation_id, None);
    assert_eq!(
        serde_json::to_string(&queued_write).unwrap(),
        r#"{"conversationId":1,"type":"write","status":"queued","id":5}"#
    );

    let base = DocumentContent::base(1, object(json!({ "count": 1 })));
    assert_eq!(
        serde_json::to_string(&base).unwrap(),
        r#"{"version":1,"kind":"base","value":{"count":1}}"#
    );
    // delta 的 Op wire 形式由 chord 的 `encode`/`decode` 桥接；往返必须逐字不变。
    let delta_json = json!({ "version": 1, "kind": "delta", "ops": [["s", ["count"], 2]] });
    let delta: DocumentContent = serde_json::from_value(delta_json.clone()).expect("valid delta");
    assert_eq!(serde_json::to_value(&delta).unwrap(), delta_json);

    let conversation_document = DocumentCreate {
        id: document_id,
        kind: "test".to_owned(),
        key: None,
        scope: DocumentScope::Conversation { conversation_id },
        history: Some(DocumentHistory::Rewindable),
        fork: Some(DocumentFork::AsOf),
    };
    assert_eq!(
        serde_json::to_string(&conversation_document).unwrap(),
        r#"{"id":6,"kind":"test","scope":{"kind":"conversation","conversationId":1},"history":"rewindable","fork":"asOf"}"#
    );

    let session_record = DocumentRecord {
        id: document_id,
        kind: "test".to_owned(),
        key: None,
        created_at: seq,
        retired_at: None,
        scope: DocumentScope::Session,
        history: None,
        fork: None,
    };
    assert_eq!(
        serde_json::to_string(&session_record).unwrap(),
        r#"{"id":6,"kind":"test","scope":{"kind":"session"},"createdAt":1}"#
    );

    let create_write = StorageWrite::DocumentCreate {
        record: DocumentCreate {
            id: document_id,
            kind: "test".to_owned(),
            key: None,
            scope: DocumentScope::Session,
            history: None,
            fork: None,
        },
        content: DocumentBaseContent::new(1, object(json!({}))),
    };
    assert_eq!(
        serde_json::to_string(&create_write).unwrap(),
        r#"{"type":"document.create","record":{"id":6,"kind":"test","scope":{"kind":"session"}},"content":{"version":1,"kind":"base","value":{}}}"#
    );

    let completed = TaskOutcome::<serde_json::Value>::Completed { result: json!(1) };
    assert_eq!(
        serde_json::to_string(&completed).unwrap(),
        r#"{"status":"completed","result":1}"#
    );

    let task_record: kiana_durable::TaskRecord<
        serde_json::Value,
        serde_json::Value,
        serde_json::Value,
    > = kiana_durable::TaskRecord {
        trace: None,
        links: None,
        id: task_id,
        conversation_id,
        kind: "test".to_owned(),
        version: 1,
        input: json!(null),
        owner: None,
        background: false,
        abort_requested: false,
        abort_reason: None,
        abandon_on_restart: None,
        started_at: None,
        ended_at: None,
        restarts: None,
        state: TaskState::Terminal {
            outcome: TaskOutcome::Completed { result: json!(1) },
        },
        memos: None,
    };
    let encoded = serde_json::to_string(&task_record).unwrap();
    assert!(encoded.contains(r#""id":4"#));
    assert!(encoded.contains(r#""state":{"status":"terminal""#));
}

#[test]
fn scan_orders_serialize_to_their_ts_strings() {
    assert_eq!(
        serde_json::to_string(&ScanOrder::Ascending).unwrap(),
        "\"ascending\""
    );
    assert_eq!(
        serde_json::to_string(&ScanOrder::Descending).unwrap(),
        "\"descending\""
    );
    assert_eq!(ScanOrder::Ascending.as_str(), "ascending");
    assert_eq!(ScanOrder::Descending.as_str(), "descending");
}

// ---------------------------------------------------------------------------
// 类型契约回归（TS 源里被审阅发现丢失的形状/语义）。
// ---------------------------------------------------------------------------

use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;

use kiana_durable::harness::types::{
    AfterFailedRun, AnyTask, ConversationCreateOptions, ConversationRetryPolicy,
    ConversationStreamOptions, ProgressPolicy, QueueMode, ResolvedCompactionSettings,
    SchedulerOutcome, Settings, SettledSubmissionRecord, SettledTask, ToolExecutionMode,
    TypedEntryRuntime,
};
use kiana_durable::{
    AssistantEntry, BoxError, ConversationQuery, ConversationRecord, CreateConversationOptions,
    Cursor, DocDraft, DocTarget, EntryData, EntryDraft, EntryHead, EntryQuery, EntryRecord, NoData,
    Page, SubmissionCreate, SubmissionSettlement, TaskOptions, TaskOutcomeError, TaskQuery,
    TaskRecord, Tx, TypedEntryDraft, TypedEntryTx, UserEntry, define_entry,
};

/// 只实现条目读写的内存 `Tx`，其余方法只证明签名存在。
struct MemoryTx {
    entries: Mutex<Vec<EntryRecord>>,
    next_id: AtomicI64,
}

impl MemoryTx {
    fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            next_id: AtomicI64::new(1),
        }
    }
}

#[async_trait]
impl Tx for MemoryTx {
    async fn conversation(
        &self,
        _id: ConversationId,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        Err("conversation is not used by this test".into())
    }

    async fn entry(&self, id: EntryId) -> Result<Option<EntryRecord>, BoxError> {
        Ok(self
            .entries
            .lock()
            .expect("entries lock")
            .iter()
            .find(|entry| entry.id == id)
            .cloned())
    }

    async fn task(&self, _id: TaskId) -> Result<Option<TaskRecord>, BoxError> {
        Err("task is not used by this test".into())
    }

    async fn scan_conversations(
        &self,
        _query: &ConversationQuery,
        _limit: usize,
        _cursor: Option<&Cursor>,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        Err("scanConversations is not used by this test".into())
    }

    async fn scan_entries(
        &self,
        _query: &EntryQuery,
        _limit: usize,
        _cursor: Option<&Cursor>,
    ) -> Result<Page<EntryRecord>, BoxError> {
        Err("scanEntries is not used by this test".into())
    }

    async fn latest_head_marker(
        &self,
        _conversation_id: ConversationId,
    ) -> Result<Option<EntryRecord>, BoxError> {
        Err("latestHeadMarker is not used by this test".into())
    }

    async fn scan_tasks(
        &self,
        _query: &TaskQuery,
        _limit: usize,
        _cursor: Option<&Cursor>,
    ) -> Result<Page<TaskRecord>, BoxError> {
        Err("scanTasks is not used by this test".into())
    }

    async fn submission_by_request(
        &self,
        _conversation_id: ConversationId,
        _request_id: &str,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        Err("submissionByRequest is not used by this test".into())
    }

    async fn create_conversation(
        &self,
        _options: &CreateConversationOptions,
    ) -> Result<ConversationRecord, BoxError> {
        Err("createConversation is not used by this test".into())
    }

    async fn fork_conversation(
        &self,
        _parent_conversation_id: ConversationId,
        _at: EntryId,
        _options: &CreateConversationOptions,
    ) -> Result<ConversationRecord, BoxError> {
        Err("forkConversation is not used by this test".into())
    }

    async fn append_entry(
        &self,
        conversation_id: ConversationId,
        value: EntryDraft,
    ) -> Result<EntryRecord, BoxError> {
        let id: EntryId = id_from_number(self.next_id.fetch_add(1, Ordering::SeqCst));
        let head = value.head.map(|head| match head {
            EntryHead::SelfHead => id,
            EntryHead::Id(head) => head,
        });
        let record = EntryRecord {
            key_order: None,
            id,
            conversation_id,
            kind: value.kind,
            model: value.model,
            data: value.data,
            head,
            edits: value.edits,
            by_task_id: None,
        };
        self.entries
            .lock()
            .expect("entries lock")
            .push(record.clone());
        Ok(record)
    }

    async fn create_task(
        &self,
        _task: &AnyTask,
        _input: kiana_durable::JsonValue,
        _options: TaskOptions,
    ) -> Result<TaskId, BoxError> {
        Err("createTask is not used by this test".into())
    }

    async fn create_submission(
        &self,
        _create: SubmissionCreate,
    ) -> Result<SubmissionRecord, BoxError> {
        Err("createSubmission is not used by this test".into())
    }

    fn settle_submission(&self, _id: SubmissionId, _settlement: SubmissionSettlement) {}

    fn place_submission(&self, _id: SubmissionId, _entry: EntryId) {}

    async fn doc(&self, _target: &DocTarget) -> Result<DocDraft, BoxError> {
        Err("doc is not used by this test".into())
    }

    async fn retire_doc(&self, _target: &DocTarget) -> Result<(), BoxError> {
        Err("retireDoc is not used by this test".into())
    }
}

/// 自定义条目 `data` 的 Rust 侧编解码（TS 里 `D extends JsonValue` 无需转换）。
#[derive(Debug, Clone, PartialEq)]
struct Counter {
    n: i64,
}

impl EntryData for Counter {
    fn from_entry_data(data: Option<&kiana_durable::JsonValue>) -> Result<Self, BoxError> {
        let value = data.cloned().unwrap_or(kiana_durable::JsonValue::Null);
        let n = value
            .get("n")
            .and_then(kiana_durable::JsonValue::as_i64)
            .ok_or("counter data needs n")?;
        Ok(Counter { n })
    }

    fn into_entry_data(self) -> Result<Option<kiana_durable::JsonValue>, BoxError> {
        Ok(Some(json!({ "n": self.n })))
    }
}

#[test]
fn appends_and_reads_typed_entries_through_tokens() {
    // 移植 `harness-submissions.test.ts` 的 "appends and reads typed entries through tokens"。
    let counter_token = define_entry::<Counter>("app.counter");
    let tx = MemoryTx::new();
    let conversation_id: ConversationId = id_from_number(1);

    let counter = futures::executor::block_on(tx.append_entry_typed(
        &counter_token,
        conversation_id,
        TypedEntryDraft {
            model: None,
            data: Counter { n: 1 },
            head: None,
            edits: None,
        },
    ))
    .expect("append a typed entry");
    assert_eq!(counter.data, Counter { n: 1 });
    assert_eq!(counter.record.kind, "app.counter");
    assert_eq!(counter.record.data, Some(json!({ "n": 1 })));

    let marker = futures::executor::block_on(tx.append_entry_typed(
        &UserEntry,
        conversation_id,
        TypedEntryDraft {
            model: None,
            data: NoData,
            head: None,
            edits: None,
        },
    ))
    .expect("append a data-less typed entry");
    // `TypedEntryDraft<never>` 不写 `data` 键。
    assert_eq!(marker.record.data, None);

    let found = futures::executor::block_on(tx.entry_typed(&counter_token, counter.record.id))
        .expect("read a typed entry");
    assert_eq!(found.map(|entry| entry.data), Some(Counter { n: 1 }));

    // kind 不匹配 → undefined；不存在 → undefined。
    assert!(
        futures::executor::block_on(tx.entry_typed(&AssistantEntry, counter.record.id))
            .expect("read")
            .is_none()
    );
    assert!(
        futures::executor::block_on(tx.entry_typed(&counter_token, id_from_number(999_999)))
            .expect("read")
            .is_none()
    );

    // 扩展 trait 对 `&dyn Tx` 也可用（Session 内部就是这样传递事务的）。
    let object: &dyn Tx = &tx;
    let via_object =
        futures::executor::block_on(object.entry_typed(&counter_token, counter.record.id))
            .expect("read through a trait object");
    assert_eq!(via_object.map(|entry| entry.data), Some(Counter { n: 1 }));

    // `is()` 收窄守卫。
    assert!(counter_token.is(&counter.record));
    assert!(!AssistantEntry.is(&counter.record));
}

#[test]
fn typed_runtime_entry_is_available_on_task_runtime_objects() {
    // `TaskRuntime` 以 trait object 使用；类型化重载必须能落在 `dyn TaskRuntime` 上。
    fn assert_implements<T: TypedEntryRuntime + ?Sized>() {}
    assert_implements::<dyn kiana_durable::harness::types::TaskRuntime>();
}

fn terminal_task() -> TaskRecord {
    TaskRecord {
        trace: None,
        links: None,
        id: id_from_number(4),
        conversation_id: id_from_number(1),
        kind: "test".to_owned(),
        version: 1,
        input: json!(null),
        owner: None,
        background: false,
        abort_requested: false,
        abort_reason: None,
        abandon_on_restart: None,
        started_at: Some(1_700_000_000_000),
        ended_at: Some(1_700_000_001_000),
        restarts: None,
        state: TaskState::Terminal {
            outcome: TaskOutcome::Completed { result: json!(7) },
        },
        memos: None,
    }
}

#[test]
fn task_lifecycle_times_serialize_as_integer_epoch_milliseconds() {
    // ARCHITECTURE §4.3(7)：`Date.now()` 毫秒 → i64；普通 serde_json 也不能写出 `1700000000000.0`。
    let mut record = terminal_task();
    record.ended_at = None;
    record.state = TaskState::Sleeping {
        checkpoint: json!(null),
        until: 1_700_000_002_000,
    };
    let encoded = serde_json::to_string(&record).expect("serialize the task record");
    assert!(
        encoded.contains(r#""startedAt":1700000000000"#),
        "{encoded}"
    );
    assert!(encoded.contains(r#""until":1700000002000}"#), "{encoded}");
    assert!(!encoded.contains(".0"), "{encoded}");
}

#[test]
fn accepts_the_largest_safe_document_version() {
    let definition = kiana_durable::documents::DocDefinition::new(
        "test.max-version",
        9_007_199_254_740_991,
        kiana_durable::DocumentSemantics::SESSION,
    );
    let token = kiana_durable::documents::define_doc(definition);
    assert_eq!(token.definition.version, 9_007_199_254_740_991);
}

#[test]
#[should_panic(expected = "Document test.too-large version must be a positive integer")]
fn rejects_document_versions_beyond_the_largest_safe_integer() {
    let definition = kiana_durable::documents::DocDefinition::new(
        "test.too-large",
        9_007_199_254_740_992,
        kiana_durable::DocumentSemantics::SESSION,
    );
    let _token = kiana_durable::documents::define_doc(definition);
}

#[test]
fn resolved_settings_compaction_fields_are_concrete() {
    // TS: `Settings.compaction: CompactionPolicy & Pick<CompactionSettings, "modelOverrides">`
    // —— 四个策略字段必须是具体值，只有 modelOverrides 可选。
    let compaction = ResolvedCompactionSettings {
        enabled: true,
        reserve_tokens: 32_000.0,
        keep_recent_tokens: 8_000.0,
        background_tokens: 0.0,
        model_overrides: None,
    };
    let settings = Settings {
        extensions: None,
        stream: ConversationStreamOptions::default(),
        retry: ConversationRetryPolicy {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 500.0,
            max_agent_delay_ms: None,
        },
        compaction,
        progress: ProgressPolicy {
            partial_interval_ms: 250.0,
            output_interval_ms: 250.0,
            persist_within_ms: 1_000.0,
        },
        tool_execution: ToolExecutionMode::Parallel,
        steering_mode: QueueMode::OneAtATime,
        follow_up_mode: QueueMode::OneAtATime,
        context_retention_ms: 60_000.0,
        document_retention_ms: 600_000.0,
        after_failed_run: AfterFailedRun::Keep,
        restart_limit: 5.0,
    };
    assert!(settings.compaction.enabled);
    assert_eq!(settings.compaction.keep_recent_tokens, 8_000.0);
}

#[test]
fn conversation_create_options_require_ownership() {
    let options = ConversationCreateOptions {
        ownership: kiana_durable::ConversationOwnership::Ownerless,
        agent: None,
        init: None,
    };
    assert_eq!(
        options.ownership,
        kiana_durable::ConversationOwnership::Ownerless
    );
}

#[test]
fn scheduler_outcomes_are_limited_to_faulted_and_orphaned() {
    let faulted = SchedulerOutcome::Faulted {
        error: TaskOutcomeError {
            message: "boom".to_owned(),
            detail: None,
        },
    };
    assert_eq!(
        serde_json::to_string(&faulted).unwrap(),
        r#"{"status":"faulted","error":{"message":"boom"}}"#
    );

    let orphaned = SchedulerOutcome::Orphaned {
        reason: "missing task".to_owned(),
    };
    assert_eq!(
        serde_json::to_string(&orphaned).unwrap(),
        r#"{"status":"orphaned","reason":"missing task"}"#
    );

    // 写进 `TaskState` 前转成普通的 `TaskOutcome`，形状与调度器直接写入的一致。
    let outcome: TaskOutcome = orphaned.into();
    assert_eq!(
        serde_json::to_string(&outcome).unwrap(),
        r#"{"status":"orphaned","reason":"missing task"}"#
    );
}

#[test]
fn settled_task_and_submission_expose_their_terminal_invariants() {
    let settled = SettledTask::new(terminal_task());
    assert_eq!(settled.id, id_from_number(4));
    assert_eq!(
        settled.outcome(),
        &TaskOutcome::Completed { result: json!(7) }
    );

    let submission = SubmissionRecord {
        key_order: None,
        trace: None,
        id: id_from_number(5),
        conversation_id: id_from_number(1),
        request_id: None,
        kind: SubmissionKind::Input,
        status: SubmissionStatus::Done,
        entry: None,
        answer: None,
        reason: None,
        detail: None,
    };
    let settled_submission = SettledSubmissionRecord::new(submission);
    assert_eq!(settled_submission.id, id_from_number(5));
    assert_eq!(settled_submission.status, SubmissionStatus::Done);
}

#[test]
#[should_panic(expected = "SettledTask requires a terminal task")]
fn settled_task_rejects_a_non_terminal_record() {
    let mut record = terminal_task();
    record.state = TaskState::Pending {
        checkpoint: json!(null),
    };
    let _settled = SettledTask::new(record);
}

#[test]
#[should_panic(expected = "SettledSubmissionRecord requires a settled submission")]
fn settled_submission_rejects_a_queued_record() {
    let _settled = SettledSubmissionRecord::new(SubmissionRecord::write_queued(
        id_from_number(5),
        id_from_number(1),
        None,
    ));
}

#[test]
fn execution_env_cleanup_receives_the_context() {
    // 编译期契约（TS: `FileSystem.cleanup(context)` / `Shell.cleanup(context)`，两者都接收 Context）。
    async fn file_system_cleanup(
        fs: &dyn kiana_durable::env::FileSystem,
        context: &dyn kiana_chord::context::Context,
    ) {
        fs.cleanup(context).await;
    }
    async fn shell_cleanup(
        shell: &dyn kiana_durable::env::Shell,
        context: &dyn kiana_chord::context::Context,
    ) {
        shell.cleanup(context).await;
    }
    let _ = file_system_cleanup;
    let _ = shell_cleanup;
}

// ---------------------------------------------------------------------------
// JSON 键序对拍（golden 由 TS 参考实现 `JSON.stringify` 逐字生成）
// ---------------------------------------------------------------------------

fn user_message(text: &str, timestamp: i64) -> kiana_ai::Message {
    kiana_ai::Message::User(kiana_ai::UserMessage {
        role: kiana_ai::UserRole::User,
        content: kiana_ai::UserContent::Text(text.to_owned()),
        timestamp,
    })
}

#[test]
fn entry_record_json_matches_ts_object_literal_insertion_order() {
    let conversation_id: ConversationId = id_from_number(2);
    let entry_id: EntryId = id_from_number(3);

    // TS: `tx.appendEntry(UserEntry, conv, { model: [msg] })` →
    // {"model":[...],"kind":"kiana.user","id":3,"conversationId":2}
    let typed = EntryRecord {
        key_order: None,
        model: Some(vec![user_message("hi", 0)]),
        data: None,
        edits: None,
        kind: "kiana.user".to_owned(),
        id: entry_id,
        conversation_id,
        head: None,
        by_task_id: None,
    };
    assert_eq!(
        serde_json::to_string(&typed).unwrap(),
        r#"{"model":[{"role":"user","content":"hi","timestamp":0}],"kind":"kiana.user","id":3,"conversationId":2}"#
    );

    // TS: `tx.appendEntry(ResetEntry, conv, { head: "self", model })`：`head` 被解构后重排在
    // `conversationId` 之后。
    let reset = EntryRecord {
        model: Some(vec![user_message("h", 0)]),
        kind: "kiana.reset".to_owned(),
        head: Some(entry_id),
        ..typed.clone()
    };
    assert_eq!(
        serde_json::to_string(&reset).unwrap(),
        r#"{"model":[{"role":"user","content":"h","timestamp":0}],"kind":"kiana.reset","id":3,"conversationId":2,"head":3}"#
    );

    // TS: `tx.appendEntry(ToolResultEntry, conv, { model, data })`：`data` 在 `kind` 之前。
    let tool = EntryRecord {
        model: Some(vec![user_message("x", 0)]),
        data: Some(
            json!({ "diagnostics": [{ "severity": "error", "code": "boom", "message": "m" }] }),
        ),
        kind: "kiana.tool-result".to_owned(),
        head: None,
        ..typed.clone()
    };
    assert_eq!(
        serde_json::to_string(&tool).unwrap(),
        r#"{"model":[{"role":"user","content":"x","timestamp":0}],"data":{"diagnostics":[{"severity":"error","code":"boom","message":"m"}]},"kind":"kiana.tool-result","id":3,"conversationId":2}"#
    );

    // TS: `tx.appendEntry(SystemEntry, conv, { model, edits })`：`edits` 在 `kind` 之前。
    let edited = EntryRecord {
        model: Some(vec![user_message("s", 0)]),
        data: None,
        edits: Some(vec![ContextEdit {
            target: entry_id,
            action: ContextEditAction::Omit,
            messages: None,
        }]),
        kind: "kiana.system".to_owned(),
        ..typed
    };
    assert_eq!(
        serde_json::to_string(&edited).unwrap(),
        r#"{"model":[{"role":"user","content":"s","timestamp":0}],"edits":[{"target":3,"action":"omit"}],"kind":"kiana.system","id":3,"conversationId":2}"#
    );
}

#[test]
fn persisted_tool_result_message_keeps_the_ts_object_literal_order() {
    // golden 由 TS 参考实现生成（`appendToolResult` 的字面量 + `JSON.stringify`）：
    //   {"role":"toolResult","toolCallId":"c1","toolName":"read","content":[...],
    //    "details":{"a":1},"usage":{...},"isError":false,"durationMs":5,"timestamp":7}
    // kiana-ai 的 `ToolResultMessage` 把 `timestamp` 声明在 `durationMs` 之前；持久化 JSON 必须
    // 按 durable 的构造序（`durationMs` 在 `timestamp` 之前）写出。
    let conversation_id: ConversationId = id_from_number(2);
    let entry_id: EntryId = id_from_number(3);
    let text = kiana_ai::UserContentBlock::Text(kiana_ai::TextContent {
        kind_text: kiana_ai::TextKind::Text,
        text: "x".to_owned(),
        text_signature: None,
    });
    let usage = kiana_ai::Usage {
        input: 1,
        output: 2,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: 3,
        cost: kiana_ai::UsageCost::default(),
    };
    let message = kiana_ai::Message::ToolResult(kiana_ai::ToolResultMessage {
        role: kiana_ai::ToolResultRole::ToolResult,
        tool_call_id: "c1".to_owned(),
        tool_name: "read".to_owned(),
        content: vec![text],
        details: Some(json!({ "a": 1 })),
        usage: Some(usage),
        nested_calls: None,
        is_error: false,
        timestamp: 7,
        duration_ms: Some(5.0),
    });
    let full = EntryRecord {
        key_order: None,
        model: Some(vec![message.clone()]),
        data: Some(json!({ "diagnostics": [] })),
        edits: None,
        kind: "kiana.tool-result".to_owned(),
        id: entry_id,
        conversation_id,
        head: None,
        by_task_id: None,
    };
    assert_eq!(
        kiana_json::js_object::stringify(&value_of(&full)),
        r#"{"model":[{"role":"toolResult","toolCallId":"c1","toolName":"read","content":[{"type":"text","text":"x"}],"details":{"a":1},"usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":3,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"isError":false,"durationMs":5,"timestamp":7}],"data":{"diagnostics":[]},"kind":"kiana.tool-result","id":3,"conversationId":2}"#
    );

    // 省略可选字段时也没有 `details`/`usage` 占位。
    let kiana_ai::Message::ToolResult(mut bare) = message else {
        unreachable!()
    };
    bare.details = None;
    bare.usage = None;
    let sparse = EntryRecord {
        model: Some(vec![kiana_ai::Message::ToolResult(bare)]),
        ..full
    };
    assert_eq!(
        kiana_json::js_object::stringify(&value_of(&sparse)),
        r#"{"model":[{"role":"toolResult","toolCallId":"c1","toolName":"read","content":[{"type":"text","text":"x"}],"isError":false,"durationMs":5,"timestamp":7}],"data":{"diagnostics":[]},"kind":"kiana.tool-result","id":3,"conversationId":2}"#
    );
}

#[test]
fn created_record_json_matches_ts_creation_order() {
    let conversation_id: ConversationId = id_from_number(2);

    // TS: `tx.createSubmission({ conversationId, type: "write", status: "queued" })`
    //     → copyJson({ ...create, id }) = {"conversationId":2,"type":"write","status":"queued","id":6}
    let queued_write = SubmissionRecord::write_queued(id_from_number(6), conversation_id, None);
    assert_eq!(
        serde_json::to_string(&queued_write).unwrap(),
        r#"{"conversationId":2,"type":"write","status":"queued","id":6}"#
    );

    // TS: `tx.createTask(task, 7, { conversationId })`（#createTask 的对象字面量顺序）。
    let task: TaskRecord = TaskRecord {
        id: id_from_number(7),
        conversation_id,
        kind: "probe.task".to_owned(),
        version: 1,
        input: json!(7),
        trace: None,
        links: None,
        owner: None,
        background: false,
        abort_requested: false,
        abandon_on_restart: None,
        state: TaskState::Pending {
            checkpoint: json!({ "phase": "ready" }),
        },
        started_at: None,
        ended_at: None,
        restarts: None,
        abort_reason: None,
        memos: None,
    };
    assert_eq!(
        serde_json::to_string(&task).unwrap(),
        r#"{"id":7,"conversationId":2,"kind":"probe.task","version":1,"input":7,"background":false,"abortRequested":false,"state":{"status":"pending","checkpoint":{"phase":"ready"}}}"#
    );

    // TS: `defineDoc({ scope: "session" })` 首次提交 → storage 打戳
    //     `record = { ...create, createdAt }` = {"id":8,"kind":"test","scope":{"kind":"session"},"createdAt":7}
    let session_record = DocumentRecord {
        id: id_from_number(8),
        kind: "test".to_owned(),
        key: None,
        scope: DocumentScope::Session,
        history: None,
        fork: None,
        created_at: seq_from_number(7),
        retired_at: None,
    };
    assert_eq!(
        serde_json::to_string(&session_record).unwrap(),
        r#"{"id":8,"kind":"test","scope":{"kind":"session"},"createdAt":7}"#
    );

    // TS: conversation 家族成员 → {"id":5,"kind":"fam","key":"k1",...,"createdAt":4}
    let family_record = DocumentRecord {
        id: id_from_number(5),
        kind: "fam".to_owned(),
        key: Some(json!("k1")),
        scope: DocumentScope::Conversation { conversation_id },
        history: Some(DocumentHistory::Latest),
        fork: Some(DocumentFork::Current),
        created_at: seq_from_number(4),
        retired_at: None,
    };
    assert_eq!(
        serde_json::to_string(&family_record).unwrap(),
        r#"{"id":5,"kind":"fam","key":"k1","scope":{"kind":"conversation","conversationId":2},"history":"latest","fork":"current","createdAt":4}"#
    );

    // retire 在 TS 里是 `{ ...record, retiredAt: seq }` → 追加在最后。
    let retired = DocumentRecord {
        retired_at: Some(seq_from_number(9)),
        ..family_record
    };
    assert_eq!(
        serde_json::to_string(&retired).unwrap(),
        r#"{"id":5,"kind":"fam","key":"k1","scope":{"kind":"conversation","conversationId":2},"history":"latest","fork":"current","createdAt":4,"retiredAt":9}"#
    );
}

#[test]
fn tool_diagnostic_json_matches_ts_object_literal_order() {
    use kiana_durable::harness::types::{DiagnosticSeverity, ToolDiagnostic};

    let with_code = ToolDiagnostic {
        severity: DiagnosticSeverity::Error,
        code: Some("boom".to_owned()),
        message: "m".to_owned(),
    };
    assert_eq!(
        serde_json::to_string(&with_code).unwrap(),
        r#"{"severity":"error","code":"boom","message":"m"}"#
    );
    let without_code = ToolDiagnostic {
        severity: DiagnosticSeverity::Warn,
        code: None,
        message: "m".to_owned(),
    };
    assert_eq!(
        serde_json::to_string(&without_code).unwrap(),
        r#"{"severity":"warn","message":"m"}"#
    );
}

#[test]
fn ts_type_only_export_aliases_are_importable_and_erase_to_canonical_types() {
    use kiana_durable::{
        CommonDocDefinition, ConversationDocFamilyToken, ConversationDocToken, DocFamilyDefinition,
        DocToken, LatestConversationSemantics, PhaseHandler, RewindableConversationDocFamilyToken,
        RewindableConversationDocToken, RewindableConversationSemantics, SessionDocFamilyToken,
        SessionDocToken, TaskDocFamilyToken, TaskDocToken,
    };

    fn as_definition<T>(definition: CommonDocDefinition<T>) -> kiana_durable::DocDefinition {
        definition
    }
    fn as_family_definition<T, I>(
        definition: DocFamilyDefinition<T, I>,
    ) -> kiana_durable::DocDefinition {
        definition
    }
    fn as_token<T>(token: SessionDocToken<T>) -> DocToken {
        token
    }
    fn as_conversation_token<T>(token: ConversationDocToken<T>) -> DocToken {
        token
    }
    fn as_rewindable_token<T>(token: RewindableConversationDocToken<T>) -> DocToken {
        token
    }
    fn as_task_token<T>(token: TaskDocToken<T>) -> DocToken {
        token
    }
    fn as_family_token<T, I>(token: SessionDocFamilyToken<T, I>) -> kiana_durable::DocFamilyToken {
        token
    }
    fn as_conversation_family_token<T, I>(
        token: ConversationDocFamilyToken<T, I>,
    ) -> kiana_durable::DocFamilyToken {
        token
    }
    fn as_rewindable_family_token<T, I>(
        token: RewindableConversationDocFamilyToken<T, I>,
    ) -> kiana_durable::DocFamilyToken {
        token
    }
    fn as_task_family_token<T, I>(
        token: TaskDocFamilyToken<T, I>,
    ) -> kiana_durable::DocFamilyToken {
        token
    }

    let definition = kiana_durable::documents::DocDefinition::new(
        "alias.test",
        1,
        kiana_durable::DocumentSemantics::SESSION,
    );
    let _ = as_definition::<kiana_durable::JsonObject>(definition);
    let family = kiana_durable::documents::DocDefinition::family(
        "alias.family",
        1,
        kiana_durable::DocumentSemantics::SESSION,
    );
    let _ = as_family_definition::<kiana_durable::JsonObject, kiana_durable::JsonValue>(family);

    let token = kiana_durable::documents::define_doc(kiana_durable::documents::DocDefinition::new(
        "alias.singleton",
        1,
        kiana_durable::DocumentSemantics::SESSION,
    ));
    let _ = as_token::<kiana_durable::JsonObject>(token.clone());
    let _ = as_conversation_token::<kiana_durable::JsonObject>(token.clone());
    let _ = as_rewindable_token::<kiana_durable::JsonObject>(token.clone());
    let _ = as_task_token::<kiana_durable::JsonObject>(token);

    let family_token = kiana_durable::documents::define_doc_family(
        kiana_durable::documents::DocDefinition::family(
            "alias.members",
            1,
            kiana_durable::DocumentSemantics::SESSION,
        ),
    );
    let _ = as_family_token::<kiana_durable::JsonObject, kiana_durable::JsonValue>(
        family_token.clone(),
    );
    let _ = as_conversation_family_token::<kiana_durable::JsonObject, kiana_durable::JsonValue>(
        family_token.clone(),
    );
    let _ = as_rewindable_family_token::<kiana_durable::JsonObject, kiana_durable::JsonValue>(
        family_token.clone(),
    );
    let _ =
        as_task_family_token::<kiana_durable::JsonObject, kiana_durable::JsonValue>(family_token);

    let latest: LatestConversationSemantics =
        kiana_durable::DocumentSemantics::latest(kiana_durable::DocumentFork::Current);
    assert_eq!(latest.history, Some(DocumentHistory::Latest));
    let rewindable: RewindableConversationSemantics =
        kiana_durable::DocumentSemantics::rewindable(kiana_durable::DocumentFork::AsOf);
    assert_eq!(rewindable.history, Some(DocumentHistory::Rewindable));

    // TS 的 `PhaseHandler` 函数类型在 Rust 侧是 `TaskPhase` trait 对象。
    let _: Option<&PhaseHandler> = None;
    let _: Option<&dyn kiana_durable::TaskPhase> = None;
}

#[test]
fn define_entry_accepts_a_runtime_kind_string() {
    let kind = format!("ext.{}.entry", 7);
    let token = kiana_durable::define_entry::<kiana_durable::NoData>(kind.clone());
    assert_eq!(token.kind.as_ref(), kind.as_str());
    let record = EntryRecord {
        key_order: None,
        kind: kind.clone(),
        model: None,
        data: None,
        edits: None,
        id: id_from_number(1),
        conversation_id: id_from_number(1),
        head: None,
        by_task_id: None,
    };
    assert!(token.is(&record));
}

#[test]
#[should_panic(expected = "Entry kind must be a non-empty string")]
fn define_entry_rejects_an_empty_runtime_kind() {
    let _ = kiana_durable::define_entry::<kiana_durable::NoData>(String::new());
}

#[test]
fn resolve_address_keeps_non_string_family_keys() {
    // TS 的 `resolveAddress` 只做 `args[index++] as string` 断言：非字符串 key 不校验，
    // 原样进入地址并由 `addressId` 的 JSON 数组带出。
    use kiana_durable::documents::resolve_address;
    use kiana_durable::{DocumentAddress, JsonValue};

    let definition = kiana_durable::documents::DocDefinition::family(
        "test",
        1,
        kiana_durable::DocumentSemantics::SESSION,
    );
    let resolved = resolve_address(&definition, &[json!(5)]);
    assert_eq!(resolved.id, r#"["test","session",null,5]"#);
    assert_eq!(resolved.address.key, Some(json!(5)));
    assert_eq!(resolved.next_argument, 1);

    // 显式 null 是“有 key”（TS 里不是 undefined）。
    let resolved = resolve_address(&definition, &[json!(null)]);
    assert_eq!(resolved.address.key, Some(JsonValue::Null));

    // 缺参数才是 undefined → 单例地址。
    let resolved = resolve_address(&definition, &[]);
    assert_eq!(resolved.address.key, None);
    assert_eq!(resolved.next_argument, 1);
    assert_eq!(resolved.id, r#"["test","session",null,null]"#);

    // documentCreate 保留同一个 key 值（序列化为 `"key":5`）。
    let address = DocumentAddress {
        kind: "test".to_owned(),
        scope: kiana_durable::DocumentScope::Session,
        key: Some(json!(5)),
    };
    let create =
        kiana_durable::documents::document_create(&definition, &address, id_from_number(6));
    assert_eq!(
        serde_json::to_string(&create).unwrap(),
        r#"{"id":6,"kind":"test","key":5,"scope":{"kind":"session"}}"#
    );

    // 带类型的家族入口把 key 固定为字符串（`impl Into<String>`），字符串 key 也构成地址身份的一部分；非字符串 key 只能走通用入口。
    let token = kiana_durable::documents::define_doc_family(
        kiana_durable::documents::DocDefinition::family(
            "test",
            1,
            kiana_durable::DocumentSemantics::SESSION,
        ),
    );
    let target = DocTarget::session_family(&token, "github", json!(null));
    assert_eq!(target.address_id(), r#"["test","session",null,"github"]"#);
}

/// spec §2.1 的 84 个 type-only 导出：每个名字都必须能从 crate 根导入。
#[test]
#[allow(unused_imports)]
fn ts_type_only_exports_are_all_importable_from_the_crate_root() {
    use kiana_durable::{
        CheckpointInfo, CommitChange, CommitOptions, CommitPersistence, CommitPublication,
        CommonDocDefinition, ContextEdit, ConversationDocFamilyToken, ConversationDocToken,
        ConversationId, ConversationOwnership, ConversationQuery, ConversationRecord, Cursor,
        DocDefinition, DocFamilyDefinition, DocFamilyToken, DocToken, DocumentAddress,
        DocumentCommitChange, DocumentContent, DocumentCopySource, DocumentCreate, DocumentId,
        DocumentObserver, DocumentPoint, DocumentQuery, DocumentReader, DocumentRecord,
        DocumentSemantics, DocumentWatch, Entry, EntryDraft, EntryId, EntryQuery, EntryRecord,
        HookRunner, HostCallback, Id, JoinPolicy, JsonObject, LatestConversationSemantics,
        NextTaskState, Page, PhaseHandler, ReportFrom, ReportInfo, ReportTask,
        RewindableConversationDocFamilyToken, RewindableConversationDocToken,
        RewindableConversationSemantics, RunningTask, ScanOrder, Seq, Session,
        SessionDocFamilyToken, SessionDocToken, SessionEnd, SetTimer, Storage, StorageWrite,
        StoredDocument, SubmissionCreate, SubmissionId, SubmissionQuery, SubmissionRecord,
        SubmissionSettlement, TableCommitChange, Task, TaskDefinition, TaskDocFamilyToken,
        TaskDocToken, TaskId, TaskOptions, TaskOutcome, TaskOutcomeError, TaskOwnership, TaskQuery,
        TaskRecord, TaskRuntime, TaskState, Tx, TypedEntry, TypedEntryDraft,
    };
}

#[test]
fn stringify_json_matches_json_stringify_number_formatting() {
    // 磁盘/线格式必须用 `stringify_json`：serde_json 的数字格式与 JS 不同。
    assert_eq!(serde_json::to_string(&json!(1e20)).unwrap(), "1e+20");
    assert_eq!(
        kiana_durable::stringify_json(&json!(1e20)).unwrap(),
        "100000000000000000000"
    );
    assert_eq!(serde_json::to_string(&json!(1e-6)).unwrap(), "1e-6");
    assert_eq!(
        kiana_durable::stringify_json(&json!(1e-6)).unwrap(),
        "0.000001"
    );
    assert_eq!(serde_json::to_string(&json!(-0.0)).unwrap(), "-0.0");
    assert_eq!(kiana_durable::stringify_json(&json!(-0.0)).unwrap(), "0");
    assert_eq!(serde_json::to_string(&json!(1.0)).unwrap(), "1.0");
    assert_eq!(kiana_durable::stringify_json(&json!(1.0)).unwrap(), "1");
    assert_eq!(
        kiana_durable::stringify_json(&json!(1e21)).unwrap(),
        "1e+21"
    );
    assert_eq!(kiana_durable::stringify_json(&json!(1.5)).unwrap(), "1.5");
    assert_eq!(
        kiana_durable::stringify_json(&json!({ "a": 1, "b": [true, null] })).unwrap(),
        r#"{"a":1,"b":[true,null]}"#
    );
}
