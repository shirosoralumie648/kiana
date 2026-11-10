//! `packages/durable/test/jsonl-storage.test.ts` 的移植。
//!
//! 覆盖：
//! - 共享的 `registerStorageConformance`（26 条）跑在 `JsonlStorage` 上，以及每条用例前后都
//!   关闭/重开存储的 `ReopeningStorage` 版本；
//! - TS 特有的发布/恢复矩阵：append/flush 失败注入 → poison；reclaim 的 write/rename/remove
//!   失败 → 静默、下次 open 重试；尾部截断的字节偏移；确认集合与回收规则。
//!
//! # 与 TS 的差异（测试手段，不是行为）
//! - TS 用 `InstrumentedEnv extends NodeExecutionEnv` 覆写方法；Rust 用 `FileSystem` 包装器
//!   转发给 `NodeExecutionEnv`。
//! - TS 的 “serializes the complete candidate before I/O” 用 `1n as unknown as JsonValue`
//!   让 `JSON.stringify` 抛错；Rust 的 `JsonValue` 无法表示 BigInt，改用同样发生在 I/O 之前的
//!   准备期失败（重复的会话 ID），断言不变：没有文件操作、准备失败的写入不可见、序号不被消耗。
//!   注意这是替代而非等价：TS 的失败点在 `prepareCommit` 成功之后、`encodeCommit` 之内；Rust 的
//!   `encode_commit` 是纯函数且不会失败（所有记录类型都实现 `Serialize`），"编码期失败" 在 Rust
//!   里不可表示，因此 "先完整编码再 I/O" 只由代码结构保证（`commit`：prepare → encode → resolve
//!   → append），测试只覆盖了准备期失败的无副作用性。
//! - `toHaveLength`/`toEqual` 等断言逐条改写为 `assert_eq!`。

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_chord::delta::{Op, Seg};
use kiana_durable::env::node::NodeExecutionEnv;
use kiana_durable::env::{
    BinaryReader, CreateDirOptions, CreateTempFileOptions, DirReader, FileContent, FileError,
    FileErrorCode, FileInfo, FileSystem, FileWatcher, ListFilesOptions, OpenBinaryReaderOptions,
    ReadTextLinesOptions, RemoveOptions, TextLineReader, WatchCallback, WatchTarget,
};
use kiana_durable::storage::jsonl::{JsonlStorage, JsonlStorageOptions, open_node_jsonl_storage};
use kiana_durable::testing::assertions::PanicAssertions;
use kiana_durable::testing::types::StorageUse;
use kiana_durable::testing::{StorageConformanceOptions, create_storage_conformance};
use kiana_durable::{
    AdoptFn, BoxError, ConversationId, ConversationQuery, ConversationRecord, Cursor,
    DocumentAddress, DocumentBaseContent, DocumentContent, DocumentCreate, DocumentId,
    DocumentPoint, DocumentQuery, DocumentRecord, DocumentScope, EntryId, EntryQuery, EntryRecord,
    IdKind, Page, ROOT_CONVERSATION_ID, Seq, Storage, StorageWrite, StoredDocument, StoredEntry,
    SubmissionId, SubmissionQuery, SubmissionRecord, TaskId, TaskQuery, TaskRecord, id_from_number,
};
use serde_json::{Value, json};

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
            "kiana-durable-jsonl-{tag}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir {
            path: path.to_string_lossy().into_owned(),
        }
    }

    fn path(&self) -> &str {
        &self.path
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

async fn open_storage(directory: &TempDir) -> JsonlStorage {
    ok(JsonlStorage::open(
        directory.path(),
        Arc::new(NodeExecutionEnv::new(directory.path())),
        context(),
        JsonlStorageOptions::default(),
    )
    .await)
}

async fn open_storage_with(
    directory: &TempDir,
    env: Arc<InstrumentedEnv>,
    options: JsonlStorageOptions,
) -> JsonlStorage {
    ok(JsonlStorage::open(directory.path(), env, context(), options).await)
}

async fn commit(storage: &dyn Storage, writes: &[StorageWrite]) -> Seq {
    ok(storage.commit(writes, adopt(), context()).await)
}

/// `readLines(path)`：`text === "" ? [] : text.trimEnd().split("\n")`。
fn read_lines(path: &str) -> Vec<String> {
    let text = std::fs::read_to_string(path).expect("read file");
    if text.is_empty() {
        return Vec::new();
    }
    text.trim_end().split('\n').map(str::to_owned).collect()
}

fn file_exists(path: &str) -> bool {
    Path::new(path).exists()
}

fn reclaim_files(directory: &str) -> Vec<String> {
    std::fs::read_dir(directory)
        .expect("read directory")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".reclaim"))
        .collect()
}

fn append_bytes(path: &str, content: &[u8]) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .expect("open for append");
    file.write_all(content).expect("append");
}

fn object(value: Value) -> kiana_durable::JsonObject {
    match value {
        Value::Object(map) => map.into_iter().collect(),
        other => panic!("expected a JSON object, got {other}"),
    }
}

// ---------------------------------------------------------------------------
// 写入构造函数（对应 TS 的对象字面量）
// ---------------------------------------------------------------------------

fn pending_task(id: TaskId, phase: &str) -> TaskRecord {
    serde_json::from_value(json!({
        "id": id.get(),
        "conversationId": ROOT_CONVERSATION_ID.get(),
        "kind": "test.task",
        "version": 1,
        "input": null,
        "state": { "status": "pending", "checkpoint": { "phase": phase } },
        "background": false,
        "abortRequested": false,
    }))
    .expect("pending task record")
}

fn terminal_task(id: TaskId) -> TaskRecord {
    serde_json::from_value(json!({
        "id": id.get(),
        "conversationId": ROOT_CONVERSATION_ID.get(),
        "kind": "test.task",
        "version": 1,
        "input": null,
        "state": { "status": "terminal", "outcome": { "status": "completed", "result": null } },
        "background": false,
        "abortRequested": false,
    }))
    .expect("terminal task record")
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

fn conversation_write(id: ConversationId) -> StorageWrite {
    StorageWrite::Conversation {
        value: ConversationRecord {
            id,
            parent: None,
            owner: None,
        },
    }
}

fn entry_write(id: EntryId, kind: &str) -> StorageWrite {
    StorageWrite::Entry {
        value: EntryRecord {
            key_order: None,
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

fn task_write(value: TaskRecord) -> StorageWrite {
    StorageWrite::Task { value }
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

async fn create_root(storage: &dyn Storage) {
    commit(storage, &[conversation_write(ROOT_CONVERSATION_ID)]).await;
}

async fn mint(storage: &dyn Storage, kind: IdKind) -> i64 {
    ok(storage.mint_id(kind).await)
}

async fn mint_task(storage: &dyn Storage) -> TaskId {
    id_from_number(mint(storage, IdKind::Task).await)
}

async fn mint_document(storage: &dyn Storage) -> DocumentId {
    id_from_number(mint(storage, IdKind::Document).await)
}

async fn mint_entry(storage: &dyn Storage) -> EntryId {
    id_from_number(mint(storage, IdKind::Entry).await)
}

fn delta_count(count: i64) -> DocumentContent {
    DocumentContent::delta(
        1,
        vec![Op::Set(vec![Seg::Key("count".to_owned())], json!(count))],
    )
}

fn base_value(value: Value) -> DocumentBaseContent {
    DocumentBaseContent::new(1, object(value))
}

/// `readdir(directory).filter(name => name.endsWith(".reclaim"))`。
fn assert_no_reclaim_files(directory: &str) {
    assert_eq!(reclaim_files(directory), Vec::<String>::new());
}

// ---------------------------------------------------------------------------
// 共享 conformance
// ---------------------------------------------------------------------------

/// 每条用例前后都关闭/重开存储的包装器（TS: `ReopeningStorage`）。
struct ReopeningStorage {
    current: Mutex<Arc<JsonlStorage>>,
    directory: String,
    closed: AtomicBool,
}

impl ReopeningStorage {
    fn new(storage: JsonlStorage, directory: String) -> Self {
        ReopeningStorage {
            current: Mutex::new(Arc::new(storage)),
            directory,
            closed: AtomicBool::new(false),
        }
    }

    fn current(&self) -> Arc<JsonlStorage> {
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    async fn reopen(&self) -> Result<(), BoxError> {
        let reopened = JsonlStorage::open(
            &self.directory,
            Arc::new(NodeExecutionEnv::new(self.directory.clone())),
            context(),
            JsonlStorageOptions::default(),
        )
        .await?;
        *self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(reopened);
        Ok(())
    }
}

#[async_trait]
impl Storage for ReopeningStorage {
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: AdoptFn,
        context: &dyn Context,
    ) -> Result<Seq, BoxError> {
        if self.closed.load(Ordering::SeqCst) {
            return self.current().commit(writes, adopt, context).await;
        }
        let current = self.current();
        let result = current.commit(writes, adopt, context).await;
        current.close(context).await;
        self.reopen().await?;
        result
    }

    async fn mint_id(&self, kind: IdKind) -> Result<i64, BoxError> {
        self.current().mint_id(kind).await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        self.current().conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        self.current()
            .scan_conversations(query, limit, cursor, context)
            .await
    }

    async fn entry(
        &self,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.current().entry(id, context).await
    }

    async fn visible_entry(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.current()
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
            .scan_entries(query, limit, cursor, context)
            .await
    }

    async fn task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        self.current().task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<TaskRecord>, BoxError> {
        self.current()
            .scan_tasks(query, limit, cursor, context)
            .await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.current().submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: &SubmissionQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<SubmissionRecord>, BoxError> {
        self.current()
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
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: &DocumentAddress,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError> {
        self.current().find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError> {
        self.current().document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: &DocumentQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<DocumentRecord>, BoxError> {
        self.current()
            .scan_documents(query, limit, cursor, context)
            .await
    }

    async fn close(&self, context: &dyn Context) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.current().close(context).await;
    }
}

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
                let storage = JsonlStorage::open(
                    directory.path(),
                    Arc::new(NodeExecutionEnv::new(directory.path())),
                    context(),
                    JsonlStorageOptions::default(),
                )
                .await
                .expect("open JSONL storage");
                if across_reopen {
                    let wrapper =
                        Arc::new(ReopeningStorage::new(storage, directory.path().to_owned()));
                    let storage: Arc<dyn Storage> = wrapper.clone();
                    run(storage).await;
                    wrapper.close(context()).await;
                } else {
                    let storage: Arc<dyn Storage> = Arc::new(storage);
                    let closing = Arc::clone(&storage);
                    run(storage).await;
                    closing.close(context()).await;
                }
            })
        }),
    }
}

/// `registerStorageConformance(..., "JsonlStorage", ...)`。
#[tokio::test]
async fn passes_the_shared_storage_conformance_suite() {
    let cases = create_storage_conformance(&conformance_options(false));
    assert_eq!(cases.len(), 26);
    for case in cases {
        println!("JsonlStorage conformance case: {}", case.name);
        (case.run)().await;
    }
}

/// `registerStorageConformance(..., "JsonlStorage across reopen", ...)`。
#[tokio::test]
async fn passes_the_shared_storage_conformance_suite_across_reopen() {
    let cases = create_storage_conformance(&conformance_options(true));
    assert_eq!(cases.len(), 26);
    for case in cases {
        println!("JsonlStorage across reopen conformance case: {}", case.name);
        (case.run)().await;
    }
}

// ---------------------------------------------------------------------------
// 失败注入包装器（TS: `InstrumentedEnv`）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureOperation {
    Append,
    Flush,
    Write,
    Rename,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureMode {
    Before,
    After,
    Short,
}

#[derive(Debug, Clone, Copy)]
struct Failure {
    operation: FailureOperation,
    call: u32,
    mode: FailureMode,
}

#[derive(Debug, Default)]
struct Observations {
    append: u32,
    flush: u32,
    write: u32,
    rename: u32,
    remove: u32,
    operations: Vec<String>,
}

struct InstrumentedEnv {
    inner: NodeExecutionEnv,
    failure: Mutex<Option<Failure>>,
    observed: Mutex<Observations>,
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn content_bytes(content: &FileContent) -> Vec<u8> {
    match content {
        FileContent::Text(text) => text.as_bytes().to_vec(),
        FileContent::Bytes(bytes) => bytes.clone(),
    }
}

fn injected(message: &str, path: &str) -> FileError {
    FileError::new(FileErrorCode::Unknown, message).with_path(path)
}

impl InstrumentedEnv {
    fn new(cwd: impl Into<kiana_durable::env::node::NodeEnvOptions>) -> Self {
        InstrumentedEnv {
            inner: NodeExecutionEnv::new(cwd),
            failure: Mutex::new(None),
            observed: Mutex::new(Observations::default()),
        }
    }

    fn fail(&self, failure: Failure) {
        *self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(failure);
        self.reset();
    }

    fn clear(&self) {
        *self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        self.reset();
    }

    fn reset(&self) {
        *self
            .observed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Observations::default();
    }

    fn operations(&self) -> Vec<String> {
        self.observed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .operations
            .clone()
    }

    /// 记录一次调用，返回命中的失败（如果这次调用的序号匹配）。
    fn begin(&self, operation: FailureOperation, entry: String) -> Option<Failure> {
        let mut observed = self
            .observed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let call = match operation {
            FailureOperation::Append => {
                observed.append += 1;
                observed.append
            }
            FailureOperation::Flush => {
                observed.flush += 1;
                observed.flush
            }
            FailureOperation::Write => {
                observed.write += 1;
                observed.write
            }
            FailureOperation::Rename => {
                observed.rename += 1;
                observed.rename
            }
            FailureOperation::Remove => {
                observed.remove += 1;
                observed.remove
            }
        };
        observed.operations.push(entry);
        let failure = *self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        failure.filter(|failure| failure.operation == operation && failure.call == call)
    }
}

#[async_trait]
impl FileSystem for InstrumentedEnv {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn cwd(&self) -> &str {
        self.inner.cwd()
    }

    fn set_cwd(&self, cwd: String) {
        self.inner.set_cwd(cwd);
    }

    async fn absolute_path(&self, path: &str, context: &dyn Context) -> Result<String, FileError> {
        self.inner.absolute_path(path, context).await
    }

    async fn join_path(
        &self,
        parts: &[String],
        context: &dyn Context,
    ) -> Result<String, FileError> {
        self.inner.join_path(parts, context).await
    }

    async fn read_text_file(&self, path: &str, context: &dyn Context) -> Result<String, FileError> {
        self.inner.read_text_file(path, context).await
    }

    async fn open_text_line_reader(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Arc<dyn TextLineReader>, FileError> {
        self.inner.open_text_line_reader(path, context).await
    }

    async fn read_text_lines(
        &self,
        path: &str,
        options: Option<&ReadTextLinesOptions>,
        context: &dyn Context,
    ) -> Result<Vec<String>, FileError> {
        self.inner.read_text_lines(path, options, context).await
    }

    async fn read_binary_file(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Vec<u8>, FileError> {
        self.inner.read_binary_file(path, context).await
    }

    async fn open_binary_reader(
        &self,
        path: &str,
        options: Option<&OpenBinaryReaderOptions>,
        context: &dyn Context,
    ) -> Result<Arc<dyn BinaryReader>, FileError> {
        self.inner.open_binary_reader(path, options, context).await
    }

    async fn write_file(
        &self,
        path: &str,
        content: FileContent,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        let failure = self.begin(FailureOperation::Write, format!("write:{}", basename(path)));
        match failure {
            None => self.inner.write_file(path, content, context).await,
            Some(Failure {
                mode: FailureMode::Before,
                ..
            }) => Err(injected("injected write failure", path)),
            Some(Failure {
                mode: FailureMode::Short,
                ..
            }) => {
                let bytes = content_bytes(&content);
                let end = std::cmp::max(1, bytes.len() / 2).min(bytes.len());
                self.inner
                    .write_file(path, FileContent::Bytes(bytes[..end].to_vec()), context)
                    .await?;
                Err(injected("injected short write", path))
            }
            Some(Failure {
                mode: FailureMode::After,
                ..
            }) => {
                self.inner.write_file(path, content, context).await?;
                Err(injected("injected post-write failure", path))
            }
        }
    }

    async fn append_file(
        &self,
        path: &str,
        content: FileContent,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        let failure = self.begin(
            FailureOperation::Append,
            format!("append:{}", basename(path)),
        );
        match failure {
            None => self.inner.append_file(path, content, context).await,
            Some(Failure {
                mode: FailureMode::Before,
                ..
            }) => Err(injected("injected append failure", path)),
            Some(Failure {
                mode: FailureMode::Short,
                ..
            }) => {
                let bytes = content_bytes(&content);
                let end = std::cmp::max(1, bytes.len() / 2).min(bytes.len());
                self.inner
                    .append_file(path, FileContent::Bytes(bytes[..end].to_vec()), context)
                    .await?;
                Err(injected("injected short append", path))
            }
            Some(Failure {
                mode: FailureMode::After,
                ..
            }) => {
                self.inner.append_file(path, content, context).await?;
                Err(injected("injected post-append failure", path))
            }
        }
    }

    async fn truncate_file(
        &self,
        path: &str,
        size: u64,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        self.inner.truncate_file(path, size, context).await
    }

    async fn flush_file(&self, path: &str, context: &dyn Context) -> Result<(), FileError> {
        let failure = self.begin(FailureOperation::Flush, format!("flush:{}", basename(path)));
        match failure {
            None => self.inner.flush_file(path, context).await,
            Some(Failure {
                mode: FailureMode::After,
                ..
            }) => {
                self.inner.flush_file(path, context).await?;
                Err(injected("injected flush failure", path))
            }
            Some(_) => Err(injected("injected flush failure", path)),
        }
    }

    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        let failure = self.begin(
            FailureOperation::Rename,
            format!(
                "rename:{}->{}",
                basename(source_path),
                basename(destination_path)
            ),
        );
        match failure {
            None => {
                self.inner
                    .rename_file(source_path, destination_path, context)
                    .await
            }
            Some(Failure {
                mode: FailureMode::After,
                ..
            }) => {
                self.inner
                    .rename_file(source_path, destination_path, context)
                    .await?;
                Err(injected("injected rename failure", source_path))
            }
            Some(_) => Err(injected("injected rename failure", source_path)),
        }
    }

    async fn file_info(&self, path: &str, context: &dyn Context) -> Result<FileInfo, FileError> {
        self.inner.file_info(path, context).await
    }

    async fn list_dir(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Vec<FileInfo>, FileError> {
        self.inner.list_dir(path, context).await
    }

    async fn list_files(
        &self,
        path: &str,
        options: Option<&ListFilesOptions>,
        context: &dyn Context,
    ) -> Result<kiana_durable::env::FileListing, FileError> {
        self.inner.list_files(path, options, context).await
    }

    async fn open_dir_reader(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Arc<dyn DirReader>, FileError> {
        self.inner.open_dir_reader(path, context).await
    }

    async fn watch(
        &self,
        targets: &[WatchTarget],
        on_change: WatchCallback,
        context: &dyn Context,
    ) -> Result<Arc<dyn FileWatcher>, FileError> {
        self.inner.watch(targets, on_change, context).await
    }

    async fn canonical_path(&self, path: &str, context: &dyn Context) -> Result<String, FileError> {
        self.inner.canonical_path(path, context).await
    }

    async fn exists(&self, path: &str, context: &dyn Context) -> Result<bool, FileError> {
        self.inner.exists(path, context).await
    }

    async fn create_dir(
        &self,
        path: &str,
        options: Option<&CreateDirOptions>,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        self.inner.create_dir(path, options, context).await
    }

    async fn remove(
        &self,
        path: &str,
        options: Option<&RemoveOptions>,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        let failure = self.begin(
            FailureOperation::Remove,
            format!("remove:{}", basename(path)),
        );
        match failure {
            None => self.inner.remove(path, options, context).await,
            Some(Failure {
                mode: FailureMode::After,
                ..
            }) => {
                self.inner.remove(path, options, context).await?;
                Err(injected("injected remove failure", path))
            }
            Some(_) => Err(injected("injected remove failure", path)),
        }
    }

    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        context: &dyn Context,
    ) -> Result<String, FileError> {
        self.inner.create_temp_dir(prefix, context).await
    }

    async fn create_temp_file(
        &self,
        options: Option<&CreateTempFileOptions>,
        context: &dyn Context,
    ) -> Result<String, FileError> {
        self.inner.create_temp_file(options, context).await
    }

    async fn cleanup(&self, context: &dyn Context) {
        self.inner.cleanup(context).await;
    }
}

// ---------------------------------------------------------------------------
// Pico JsonlStorage publication and recovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn opens_through_the_node_adapter() {
    let directory = TempDir::new("node-adapter");
    let storage =
        ok(
            open_node_jsonl_storage(directory.path(), context(), JsonlStorageOptions::default())
                .await,
        );
    create_root(&storage).await;
    assert_eq!(
        ok(storage.conversation(ROOT_CONVERSATION_ID, context()).await),
        Some(ConversationRecord {
            id: ROOT_CONVERSATION_ID,
            parent: None,
            owner: None,
        })
    );
    storage.close(context()).await;
}

#[tokio::test]
async fn publishes_every_commit_before_reclaiming_current_only_document_and_terminal_task_sidecars()
{
    let directory = TempDir::new("publish");
    let storage = open_storage(&directory).await;
    create_root(&storage).await;
    let task_id = mint_task(&storage).await;
    let document_id = mint_document(&storage).await;
    commit(&storage, &[task_write(pending_task(task_id, "ready"))]).await;
    commit(
        &storage,
        &[doc_create(
            session_document(document_id, "test.document"),
            base_value(json!({ "count": 0 })),
        )],
    )
    .await;
    commit(&storage, &[doc_change(document_id, delta_count(1))]).await;
    commit(
        &storage,
        &[doc_change(
            document_id,
            DocumentContent::base(1, object(json!({ "count": 1 }))),
        )],
    )
    .await;
    assert_eq!(
        read_lines(&directory.join(&format!("task-{task_id}.jsonl"))).len(),
        1
    );
    assert_eq!(
        read_lines(&directory.join(&format!("doc-{document_id}.jsonl"))).len(),
        1
    );

    commit(&storage, &[doc_retire(document_id)]).await;
    commit(&storage, &[task_write(terminal_task(task_id))]).await;

    assert_eq!(read_lines(&directory.join("main.jsonl")).len(), 7);
    assert!(!file_exists(
        &directory.join(&format!("task-{task_id}.jsonl"))
    ));
    assert!(!file_exists(
        &directory.join(&format!("doc-{document_id}.jsonl"))
    ));
    let marker_types: Vec<String> = read_lines(&directory.join("main.jsonl"))
        .iter()
        .map(|line| {
            serde_json::from_str::<Value>(line).expect("marker json")["type"]
                .as_str()
                .expect("marker type")
                .to_owned()
        })
        .collect();
    assert_eq!(marker_types, vec!["commit"; 7]);
}

#[tokio::test]
async fn orders_multiple_live_task_replacements_in_one_sidecar_by_commit_ordinal() {
    let directory = TempDir::new("task-ordinal");
    let storage = open_storage(&directory).await;
    create_root(&storage).await;
    let task_id = mint_task(&storage).await;
    commit(
        &storage,
        &[
            task_write(pending_task(task_id, "first")),
            task_write(pending_task(task_id, "second")),
        ],
    )
    .await;
    assert_eq!(
        read_lines(&directory.join(&format!("task-{task_id}.jsonl"))).len(),
        2
    );

    let reopened = open_storage(&directory).await;
    assert_eq!(
        ok(reopened.task(task_id, context()).await),
        Some(pending_task(task_id, "second"))
    );
}

#[tokio::test]
async fn removes_a_complete_plus_torn_unconfirmed_multi_record_sidecar_append() {
    let directory = TempDir::new("torn-append");
    let env = Arc::new(InstrumentedEnv::new(directory.path()));
    let storage = open_storage_with(&directory, env.clone(), JsonlStorageOptions::default()).await;
    create_root(&storage).await;
    let task_id = mint_task(&storage).await;
    env.fail(Failure {
        operation: FailureOperation::Append,
        call: 1,
        mode: FailureMode::Short,
    });
    let failure = storage
        .commit(
            &[
                task_write(pending_task(task_id, "first")),
                task_write(pending_task(
                    task_id,
                    &format!("second-{}", "x".repeat(512)),
                )),
            ],
            adopt(),
            context(),
        )
        .await
        .expect_err("the short append poisons the storage");
    assert!(failure.to_string().contains("poisoned"));

    let partial = std::fs::read(directory.join(&format!("task-{task_id}.jsonl")))
        .expect("read partial sidecar");
    assert_ne!(partial.last(), Some(&0x0a));
    assert_eq!(partial.iter().filter(|byte| **byte == 0x0a).count(), 1);

    let reopened = open_storage(&directory).await;
    assert_eq!(ok(reopened.task(task_id, context()).await), None);
    assert_eq!(
        std::fs::metadata(directory.join(&format!("task-{task_id}.jsonl")))
            .expect("sidecar metadata")
            .len(),
        0
    );
}

#[tokio::test]
async fn serializes_the_complete_candidate_before_io_and_leaves_preparation_failures_usable() {
    // TS 用 BigInt（`1n`）让 `JSON.stringify` 抛错；Rust 的 `JsonValue` 表示不了 BigInt，
    // 这里用同样发生在任何 I/O 之前的准备期失败：重复的会话 ID。差异见文件头的说明：
    // Rust 的 `encode_commit` 不会失败，所以失败点只能落在 prepareCommit。
    let directory = TempDir::new("prepare-failure");
    let env = Arc::new(InstrumentedEnv::new(directory.path()));
    let storage = open_storage_with(&directory, env.clone(), JsonlStorageOptions::default()).await;
    create_root(&storage).await;
    env.clear();
    let id = mint_entry(&storage).await;
    storage
        .commit(
            &[conversation_write(ROOT_CONVERSATION_ID)],
            adopt(),
            context(),
        )
        .await
        .expect_err("duplicate conversation id fails during preparation");
    assert_eq!(env.operations(), Vec::<String>::new());
    assert_eq!(ok(storage.entry(id, context()).await), None);
    assert_eq!(commit(&storage, &[entry_write(id, "good")]).await.get(), 2);
}

#[tokio::test]
async fn poisons_after_append_failure_and_recovers_only_confirmed_state() {
    let failures = [
        Failure {
            operation: FailureOperation::Append,
            call: 1,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOperation::Append,
            call: 1,
            mode: FailureMode::After,
        },
        Failure {
            operation: FailureOperation::Append,
            call: 1,
            mode: FailureMode::Short,
        },
        Failure {
            operation: FailureOperation::Append,
            call: 2,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOperation::Append,
            call: 2,
            mode: FailureMode::After,
        },
        Failure {
            operation: FailureOperation::Append,
            call: 3,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOperation::Append,
            call: 3,
            mode: FailureMode::Short,
        },
        Failure {
            operation: FailureOperation::Append,
            call: 3,
            mode: FailureMode::After,
        },
    ];
    for failure in failures {
        let directory = TempDir::new("append-failure");
        let env = Arc::new(InstrumentedEnv::new(directory.path()));
        let storage =
            open_storage_with(&directory, env.clone(), JsonlStorageOptions::default()).await;
        create_root(&storage).await;
        let first_id = mint_document(&storage).await;
        let second_id = mint_document(&storage).await;
        env.fail(failure);
        let error = storage
            .commit(
                &[
                    doc_create(
                        session_document(first_id, "first"),
                        base_value(json!({ "text": "α" })),
                    ),
                    doc_create(
                        session_document(second_id, "second"),
                        base_value(json!({ "text": "β" })),
                    ),
                ],
                adopt(),
                context(),
            )
            .await
            .expect_err("the injected append failure poisons the storage");
        assert!(error.to_string().contains("poisoned"));
        let read_failure = storage
            .document(first_id, DocumentPoint::Current, context())
            .await
            .expect_err("reads fail on a poisoned storage");
        assert!(read_failure.to_string().contains("poisoned"));

        let reopened = open_storage(&directory).await;
        let marker_survived = failure.call == 3 && failure.mode == FailureMode::After;
        for (id, text) in [(first_id, "α"), (second_id, "β")] {
            let document = ok(reopened
                .document(id, DocumentPoint::Current, context())
                .await);
            if marker_survived {
                let document = document.expect("the published marker confirms both sidecars");
                assert_eq!(document.value.get("text"), Some(&json!(text)));
            } else {
                assert_eq!(document, None);
            }
        }
    }
}

#[tokio::test]
async fn poisons_after_flush_failure_and_never_writes_a_marker() {
    let failures = [
        Failure {
            operation: FailureOperation::Flush,
            call: 1,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOperation::Flush,
            call: 1,
            mode: FailureMode::After,
        },
        Failure {
            operation: FailureOperation::Flush,
            call: 2,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOperation::Flush,
            call: 2,
            mode: FailureMode::After,
        },
    ];
    for failure in failures {
        let directory = TempDir::new("flush-failure");
        let env = Arc::new(InstrumentedEnv::new(directory.path()));
        let storage =
            open_storage_with(&directory, env.clone(), JsonlStorageOptions { fsync: true }).await;
        create_root(&storage).await;
        let first_id = mint_document(&storage).await;
        let second_id = mint_document(&storage).await;
        env.fail(failure);
        let error = storage
            .commit(
                &[
                    doc_create(
                        session_document(first_id, "flush.first"),
                        base_value(json!({})),
                    ),
                    doc_create(
                        session_document(second_id, "flush.second"),
                        base_value(json!({})),
                    ),
                ],
                adopt(),
                context(),
            )
            .await
            .expect_err("the injected flush failure poisons the storage");
        assert!(error.to_string().contains("poisoned"));
        let reopened = open_storage(&directory).await;
        assert_eq!(
            ok(reopened
                .document(first_id, DocumentPoint::Current, context())
                .await),
            None
        );
        assert_eq!(
            ok(reopened
                .document(second_id, DocumentPoint::Current, context())
                .await),
            None
        );
    }
}

#[tokio::test]
async fn orders_publication_flushes_exactly_and_flushes_main_only_to_authorize_reclamation() {
    for fsync in [false, true] {
        let directory = TempDir::new("ordered-flushes");
        let env = Arc::new(InstrumentedEnv::new(directory.path()));
        let storage =
            open_storage_with(&directory, env.clone(), JsonlStorageOptions { fsync }).await;
        create_root(&storage).await;
        let first_id = mint_document(&storage).await;
        let second_id = mint_document(&storage).await;
        env.clear();
        commit(
            &storage,
            &[
                doc_create(session_document(second_id, "second"), base_value(json!({}))),
                doc_create(session_document(first_id, "first"), base_value(json!({}))),
            ],
        )
        .await;
        let mut expected = vec![
            format!("append:doc-{second_id}.jsonl"),
            format!("append:doc-{first_id}.jsonl"),
        ];
        if fsync {
            expected.push(format!("flush:doc-{second_id}.jsonl"));
            expected.push(format!("flush:doc-{first_id}.jsonl"));
        }
        expected.push("append:main.jsonl".to_owned());
        assert_eq!(env.operations(), expected);

        env.clear();
        commit(
            &storage,
            &[doc_change(
                first_id,
                DocumentContent::base(1, object(json!({ "checkpoint": true }))),
            )],
        )
        .await;
        let mut expected = vec![format!("append:doc-{first_id}.jsonl")];
        if fsync {
            expected.push(format!("flush:doc-{first_id}.jsonl"));
        }
        expected.push("append:main.jsonl".to_owned());
        if fsync {
            expected.push("flush:main.jsonl".to_owned());
        }
        expected.push(format!("write:doc-{first_id}.jsonl.reclaim"));
        if fsync {
            expected.push(format!("flush:doc-{first_id}.jsonl.reclaim"));
        }
        expected.push(format!(
            "rename:doc-{first_id}.jsonl.reclaim->doc-{first_id}.jsonl"
        ));
        assert_eq!(env.operations(), expected);

        env.clear();
        let entry_id = mint_entry(&storage).await;
        commit(&storage, &[entry_write(entry_id, "main-only")]).await;
        assert_eq!(env.operations(), vec!["append:main.jsonl".to_owned()]);

        let task_id = mint_task(&storage).await;
        commit(&storage, &[task_write(pending_task(task_id, "ready"))]).await;
        env.clear();
        commit(&storage, &[task_write(terminal_task(task_id))]).await;
        let mut expected = vec!["append:main.jsonl".to_owned()];
        if fsync {
            expected.push("flush:main.jsonl".to_owned());
        }
        expected.push(format!("remove:task-{task_id}.jsonl"));
        assert_eq!(env.operations(), expected);
    }
}

#[tokio::test]
async fn recovers_a_committed_base_across_reclaim_write_and_rename_failures() {
    let failures = [
        Failure {
            operation: FailureOperation::Write,
            call: 1,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOperation::Write,
            call: 1,
            mode: FailureMode::Short,
        },
        Failure {
            operation: FailureOperation::Write,
            call: 1,
            mode: FailureMode::After,
        },
        Failure {
            operation: FailureOperation::Rename,
            call: 1,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOperation::Rename,
            call: 1,
            mode: FailureMode::After,
        },
    ];
    for failure in failures {
        let directory = TempDir::new("reclaim-failure");
        let env = Arc::new(InstrumentedEnv::new(directory.path()));
        let storage =
            open_storage_with(&directory, env.clone(), JsonlStorageOptions::default()).await;
        create_root(&storage).await;
        let id = mint_document(&storage).await;
        commit(
            &storage,
            &[doc_create(
                session_document(id, "test.document"),
                base_value(json!({ "count": 0 })),
            )],
        )
        .await;
        commit(&storage, &[doc_change(id, delta_count(1))]).await;

        env.fail(failure);
        let seq = commit(
            &storage,
            &[doc_change(
                id,
                DocumentContent::base(1, object(json!({ "count": 2 }))),
            )],
        )
        .await;
        assert_eq!(seq.get(), 4);
        let current = ok(storage
            .document(id, DocumentPoint::Current, context())
            .await)
        .expect("current document");
        assert_eq!(current.value.get("count"), Some(&json!(2)));
        storage.close(context()).await;

        let reopened = open_storage(&directory).await;
        let current = ok(reopened
            .document(id, DocumentPoint::Current, context())
            .await)
        .expect("current document after reopen");
        assert_eq!(current.value.get("count"), Some(&json!(2)));
        assert_eq!(
            read_lines(&directory.join(&format!("doc-{id}.jsonl"))).len(),
            1
        );
        assert_no_reclaim_files(directory.path());
    }
}

#[tokio::test]
async fn recovers_document_retirement_reclamation_across_remove_failures() {
    for mode in [FailureMode::Before, FailureMode::After] {
        let directory = TempDir::new("retire-remove");
        let env = Arc::new(InstrumentedEnv::new(directory.path()));
        let storage =
            open_storage_with(&directory, env.clone(), JsonlStorageOptions::default()).await;
        create_root(&storage).await;
        let task_id = mint_task(&storage).await;
        let id = mint_document(&storage).await;
        commit(
            &storage,
            &[
                task_write(pending_task(task_id, "ready")),
                doc_create(
                    DocumentCreate {
                        id,
                        kind: "task.document".to_owned(),
                        key: None,
                        scope: DocumentScope::Task { task_id },
                        history: None,
                        fork: None,
                    },
                    base_value(json!({ "count": 1 })),
                ),
            ],
        )
        .await;
        env.fail(Failure {
            operation: FailureOperation::Remove,
            call: 1,
            mode,
        });
        assert_eq!(commit(&storage, &[doc_retire(id)]).await.get(), 3);
        assert_eq!(
            ok(storage
                .document(id, DocumentPoint::Current, context())
                .await),
            None
        );
        storage.close(context()).await;

        let reopened = open_storage(&directory).await;
        assert_eq!(
            ok(reopened
                .document(id, DocumentPoint::Current, context())
                .await),
            None
        );
        assert_eq!(
            ok(reopened.task(task_id, context()).await),
            Some(pending_task(task_id, "ready"))
        );
        assert!(!file_exists(&directory.join(&format!("doc-{id}.jsonl"))));
        assert_no_reclaim_files(directory.path());
    }
}

#[tokio::test]
async fn defers_reclamation_after_authorizing_main_flush_failure() {
    for mode in [FailureMode::Before, FailureMode::After] {
        let directory = TempDir::new("deferred-reclaim");
        let env = Arc::new(InstrumentedEnv::new(directory.path()));
        let storage =
            open_storage_with(&directory, env.clone(), JsonlStorageOptions { fsync: true }).await;
        create_root(&storage).await;
        let id = mint_document(&storage).await;
        commit(
            &storage,
            &[doc_create(
                session_document(id, "test.document"),
                base_value(json!({ "count": 0 })),
            )],
        )
        .await;
        env.fail(Failure {
            operation: FailureOperation::Flush,
            call: 2,
            mode,
        });
        assert_eq!(
            commit(
                &storage,
                &[doc_change(
                    id,
                    DocumentContent::base(1, object(json!({ "count": 2 }))),
                )],
            )
            .await
            .get(),
            3
        );
        assert_eq!(
            env.operations(),
            vec![
                format!("append:doc-{id}.jsonl"),
                format!("flush:doc-{id}.jsonl"),
                "append:main.jsonl".to_owned(),
                "flush:main.jsonl".to_owned(),
            ]
        );
        let current = ok(storage
            .document(id, DocumentPoint::Current, context())
            .await)
        .expect("current document");
        assert_eq!(current.value.get("count"), Some(&json!(2)));
        assert_eq!(
            read_lines(&directory.join(&format!("doc-{id}.jsonl"))).len(),
            2
        );
        storage.close(context()).await;

        let recovery_env = Arc::new(InstrumentedEnv::new(directory.path()));
        recovery_env.fail(Failure {
            operation: FailureOperation::Flush,
            call: 1,
            mode,
        });
        let deferred = open_storage_with(
            &directory,
            recovery_env.clone(),
            JsonlStorageOptions { fsync: true },
        )
        .await;
        let current = ok(deferred
            .document(id, DocumentPoint::Current, context())
            .await)
        .expect("current document");
        assert_eq!(current.value.get("count"), Some(&json!(2)));
        assert_eq!(
            recovery_env.operations(),
            vec!["flush:main.jsonl".to_owned()]
        );
        assert_eq!(
            read_lines(&directory.join(&format!("doc-{id}.jsonl"))).len(),
            2
        );
        deferred.close(context()).await;

        let reclaimed = open_storage_with(
            &directory,
            Arc::new(InstrumentedEnv::new(directory.path())),
            JsonlStorageOptions { fsync: true },
        )
        .await;
        let current = ok(reclaimed
            .document(id, DocumentPoint::Current, context())
            .await)
        .expect("current document");
        assert_eq!(current.value.get("count"), Some(&json!(2)));
        assert_eq!(
            read_lines(&directory.join(&format!("doc-{id}.jsonl"))).len(),
            1
        );
        reclaimed.close(context()).await;
    }
}

#[tokio::test]
async fn keeps_a_committed_base_usable_after_reclaim_temp_flush_failure() {
    for mode in [FailureMode::Before, FailureMode::After] {
        let directory = TempDir::new("reclaim-flush");
        let env = Arc::new(InstrumentedEnv::new(directory.path()));
        let storage =
            open_storage_with(&directory, env.clone(), JsonlStorageOptions { fsync: true }).await;
        create_root(&storage).await;
        let id = mint_document(&storage).await;
        commit(
            &storage,
            &[doc_create(
                session_document(id, "test.document"),
                base_value(json!({ "count": 0 })),
            )],
        )
        .await;
        env.fail(Failure {
            operation: FailureOperation::Flush,
            call: 3,
            mode,
        });
        assert_eq!(
            commit(
                &storage,
                &[doc_change(
                    id,
                    DocumentContent::base(1, object(json!({ "count": 2 }))),
                )],
            )
            .await
            .get(),
            3
        );
        env.clear();
        commit(&storage, &[doc_change(id, delta_count(3))]).await;
        storage.close(context()).await;

        let reopened = open_storage_with(
            &directory,
            Arc::new(InstrumentedEnv::new(directory.path())),
            JsonlStorageOptions { fsync: true },
        )
        .await;
        let current = ok(reopened
            .document(id, DocumentPoint::Current, context())
            .await)
        .expect("current document");
        assert_eq!(current.value.get("count"), Some(&json!(3)));
        assert_eq!(
            read_lines(&directory.join(&format!("doc-{id}.jsonl"))).len(),
            2
        );
    }
}

#[tokio::test]
async fn recovers_terminal_task_reclamation_across_remove_failures() {
    for mode in [FailureMode::Before, FailureMode::After] {
        let directory = TempDir::new("terminal-remove");
        let env = Arc::new(InstrumentedEnv::new(directory.path()));
        let storage =
            open_storage_with(&directory, env.clone(), JsonlStorageOptions::default()).await;
        create_root(&storage).await;
        let id = mint_task(&storage).await;
        commit(&storage, &[task_write(pending_task(id, "ready"))]).await;
        env.fail(Failure {
            operation: FailureOperation::Remove,
            call: 1,
            mode,
        });
        assert_eq!(
            commit(&storage, &[task_write(terminal_task(id))])
                .await
                .get(),
            3
        );
        assert_eq!(
            ok(storage.task(id, context()).await),
            Some(terminal_task(id))
        );
        storage.close(context()).await;

        let reopened = open_storage(&directory).await;
        assert_eq!(
            ok(reopened.task(id, context()).await),
            Some(terminal_task(id))
        );
        assert!(!file_exists(&directory.join(&format!("task-{id}.jsonl"))));
        assert_no_reclaim_files(directory.path());
    }
}

#[tokio::test]
async fn appends_later_deltas_to_the_replacement_sidecar_after_a_current_only_base() {
    let directory = TempDir::new("deltas-after-base");
    let storage = open_storage(&directory).await;
    create_root(&storage).await;
    let id = mint_document(&storage).await;
    commit(
        &storage,
        &[doc_create(
            session_document(id, "test.document"),
            base_value(json!({ "count": 0 })),
        )],
    )
    .await;
    commit(
        &storage,
        &[doc_change(
            id,
            DocumentContent::base(1, object(json!({ "count": 10 }))),
        )],
    )
    .await;
    commit(&storage, &[doc_change(id, delta_count(11))]).await;
    assert_eq!(
        read_lines(&directory.join(&format!("doc-{id}.jsonl"))).len(),
        2
    );
    let reopened = open_storage(&directory).await;
    let current = ok(reopened
        .document(id, DocumentPoint::Current, context())
        .await)
    .expect("current document");
    assert_eq!(current.value.get("count"), Some(&json!(11)));
}

#[tokio::test]
async fn never_reclaims_rewindable_document_history_including_after_a_base_and_retirement() {
    let directory = TempDir::new("rewindable");
    let storage = open_storage(&directory).await;
    create_root(&storage).await;
    let id = mint_document(&storage).await;
    let record = DocumentCreate {
        id,
        kind: "rewindable".to_owned(),
        key: None,
        scope: DocumentScope::Conversation {
            conversation_id: ROOT_CONVERSATION_ID,
        },
        history: Some(kiana_durable::DocumentHistory::Rewindable),
        fork: Some(kiana_durable::DocumentFork::AsOf),
    };
    let created_at = commit(
        &storage,
        &[doc_create(record, base_value(json!({ "count": 0 })))],
    )
    .await;
    let changed_at = commit(&storage, &[doc_change(id, delta_count(1))]).await;
    commit(
        &storage,
        &[doc_change(
            id,
            DocumentContent::base(1, object(json!({ "count": 2 }))),
        )],
    )
    .await;
    commit(&storage, &[doc_retire(id)]).await;

    assert_eq!(
        read_lines(&directory.join(&format!("doc-{id}.jsonl"))).len(),
        3
    );
    let reopened = open_storage(&directory).await;
    let at_created = ok(reopened
        .document(id, DocumentPoint::At(created_at), context())
        .await)
    .expect("as-of creation");
    assert_eq!(at_created.value.get("count"), Some(&json!(0)));
    let at_changed = ok(reopened
        .document(id, DocumentPoint::At(changed_at), context())
        .await)
    .expect("as-of change");
    assert_eq!(at_changed.value.get("count"), Some(&json!(1)));
    assert_eq!(
        ok(reopened
            .document(id, DocumentPoint::Current, context())
            .await),
        None
    );
    assert_eq!(
        read_lines(&directory.join(&format!("doc-{id}.jsonl"))).len(),
        3
    );
}

#[tokio::test]
async fn reclaims_retired_task_session_and_latest_conversation_document_sidecars() {
    let directory = TempDir::new("retired-sidecars");
    let storage = open_storage(&directory).await;
    create_root(&storage).await;
    let task_id = mint_task(&storage).await;
    let session_id = mint_document(&storage).await;
    let latest_id = mint_document(&storage).await;
    let task_document_id = mint_document(&storage).await;
    let created_at = commit(
        &storage,
        &[
            task_write(pending_task(task_id, "ready")),
            doc_create(
                session_document(session_id, "session"),
                base_value(json!({})),
            ),
            doc_create(
                DocumentCreate {
                    id: latest_id,
                    kind: "latest".to_owned(),
                    key: None,
                    scope: DocumentScope::Conversation {
                        conversation_id: ROOT_CONVERSATION_ID,
                    },
                    history: Some(kiana_durable::DocumentHistory::Latest),
                    fork: Some(kiana_durable::DocumentFork::Current),
                },
                base_value(json!({})),
            ),
            doc_create(
                DocumentCreate {
                    id: task_document_id,
                    kind: "task".to_owned(),
                    key: None,
                    scope: DocumentScope::Task { task_id },
                    history: None,
                    fork: None,
                },
                base_value(json!({})),
            ),
        ],
    )
    .await;
    let retired_at = commit(
        &storage,
        &[
            doc_retire(session_id),
            doc_retire(latest_id),
            doc_retire(task_document_id),
            task_write(terminal_task(task_id)),
        ],
    )
    .await;
    for file in [
        format!("doc-{session_id}.jsonl"),
        format!("doc-{latest_id}.jsonl"),
        format!("doc-{task_document_id}.jsonl"),
        format!("task-{task_id}.jsonl"),
    ] {
        assert!(!file_exists(&directory.join(&file)), "{file} was reclaimed");
    }

    let reopened = open_storage(&directory).await;
    assert_eq!(
        ok(reopened.task(task_id, context()).await),
        Some(terminal_task(task_id))
    );
    assert_eq!(
        ok(reopened
            .document(session_id, DocumentPoint::Current, context())
            .await),
        None
    );
    assert_eq!(
        ok(reopened
            .document(latest_id, DocumentPoint::Current, context())
            .await),
        None
    );
    assert_eq!(
        ok(reopened
            .document(task_document_id, DocumentPoint::Current, context())
            .await),
        None
    );
    let address = DocumentAddress {
        kind: "session".to_owned(),
        scope: DocumentScope::Session,
        key: None,
    };
    let found = ok(reopened
        .find_document(&address, DocumentPoint::At(created_at), context())
        .await)
    .expect("the created incarnation is addressable");
    assert_eq!(
        found,
        DocumentRecord {
            id: session_id,
            kind: "session".to_owned(),
            key: None,
            scope: DocumentScope::Session,
            history: None,
            fork: None,
            created_at,
            retired_at: Some(retired_at),
        }
    );
    assert_eq!(
        ok(reopened
            .find_document(&address, DocumentPoint::At(retired_at), context())
            .await),
        None
    );
}

#[tokio::test]
async fn truncates_torn_utf8_tails_at_exact_byte_offsets_and_reuses_the_unconfirmed_sequence() {
    let directory = TempDir::new("torn-tail");
    let storage = open_storage(&directory).await;
    create_root(&storage).await;
    let document_id = mint_document(&storage).await;
    commit(
        &storage,
        &[doc_create(
            session_document(document_id, "test.document"),
            base_value(json!({ "text": "kept" })),
        )],
    )
    .await;
    let sidecar_path = directory.join(&format!("doc-{document_id}.jsonl"));
    let main_path = directory.join("main.jsonl");
    let sidecar_size = std::fs::metadata(&sidecar_path).expect("sidecar").len();
    let main_size = std::fs::metadata(&main_path).expect("main").len();
    let torn = "{\"text\":\"€".as_bytes();
    append_bytes(&sidecar_path, &torn[..torn.len() - 1]);
    append_bytes(&main_path, &torn[..torn.len() - 1]);

    let reopened = open_storage(&directory).await;
    assert_eq!(
        std::fs::metadata(&sidecar_path).expect("sidecar").len(),
        sidecar_size
    );
    assert_eq!(
        std::fs::metadata(&main_path).expect("main").len(),
        main_size
    );
    assert_eq!(
        commit(
            &reopened,
            &[doc_change(
                document_id,
                DocumentContent::delta(1, Vec::new())
            )]
        )
        .await
        .get(),
        3
    );
}

#[tokio::test]
async fn removes_complete_unconfirmed_sidecar_tails_without_resurrecting_terminal_tasks() {
    let directory = TempDir::new("unconfirmed-tail");
    let storage = open_storage(&directory).await;
    create_root(&storage).await;
    let task_id = mint_task(&storage).await;
    commit(&storage, &[task_write(pending_task(task_id, "ready"))]).await;
    commit(&storage, &[task_write(terminal_task(task_id))]).await;
    let sidecar_path = directory.join(&format!("task-{task_id}.jsonl"));
    assert!(!file_exists(&sidecar_path));
    let stale = serde_json::to_value(pending_task(task_id, "stale")).expect("stale task json");
    let line = json!({
        "format": 1,
        "type": "record",
        "seq": 4,
        "ordinal": 0,
        "payload": { "type": "task", "value": stale },
    });
    append_bytes(
        &sidecar_path,
        format!("{}\n", kiana_json::js_object::stringify(&line)).as_bytes(),
    );

    let reopened = open_storage(&directory).await;
    assert!(!file_exists(&sidecar_path));
    assert_eq!(
        ok(reopened.task(task_id, context()).await),
        Some(terminal_task(task_id))
    );
    assert_eq!(commit(&reopened, &[]).await.get(), 4);
}

#[tokio::test]
async fn fails_open_when_confirmed_sidecar_data_is_missing() {
    let directory = TempDir::new("missing-sidecar");
    let storage = open_storage(&directory).await;
    create_root(&storage).await;
    let document_id = mint_document(&storage).await;
    commit(
        &storage,
        &[doc_create(
            session_document(document_id, "test.document"),
            base_value(json!({})),
        )],
    )
    .await;
    std::fs::write(directory.join(&format!("doc-{document_id}.jsonl")), "").expect("truncate");

    let error = JsonlStorage::open(
        directory.path(),
        Arc::new(NodeExecutionEnv::new(directory.path())),
        context(),
        JsonlStorageOptions::default(),
    )
    .await
    .expect_err("missing confirmed data fails open");
    assert!(
        error
            .to_string()
            .contains("Missing confirmed sidecar record"),
        "{error}"
    );
}

#[tokio::test]
async fn rejects_a_confirmed_record_after_an_unconfirmed_sidecar_record() {
    let directory = TempDir::new("unconfirmed-between");
    let storage = open_storage(&directory).await;
    create_root(&storage).await;
    let document_id = mint_document(&storage).await;
    commit(
        &storage,
        &[doc_create(
            session_document(document_id, "test.document"),
            base_value(json!({ "count": 0 })),
        )],
    )
    .await;
    commit(&storage, &[doc_change(document_id, delta_count(1))]).await;
    let path = directory.join(&format!("doc-{document_id}.jsonl"));
    let lines = read_lines(&path);
    let unconfirmed = json!({
        "format": 1,
        "type": "record",
        "seq": 2,
        "ordinal": 999,
        "payload": {
            "type": "document",
            "id": document_id.get(),
            "content": { "kind": "delta", "version": 1, "ops": [] },
        },
    });
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n{}\n",
            lines[0],
            kiana_json::js_object::stringify(&unconfirmed),
            lines[1]
        ),
    )
    .expect("rewrite sidecar");

    let error = JsonlStorage::open(
        directory.path(),
        Arc::new(NodeExecutionEnv::new(directory.path())),
        context(),
        JsonlStorageOptions::default(),
    )
    .await
    .expect_err("an unconfirmed record before a confirmed one fails open");
    assert!(
        error
            .to_string()
            .contains("Confirmed record follows an unconfirmed tail"),
        "{error}"
    );
}

#[tokio::test]
async fn rejects_non_increasing_main_commit_sequences() {
    let directory = TempDir::new("non-increasing");
    let storage = open_storage(&directory).await;
    create_root(&storage).await;
    let path = directory.join("main.jsonl");
    let content = std::fs::read(&path).expect("read main");
    append_bytes(&path, &content);

    let error = JsonlStorage::open(
        directory.path(),
        Arc::new(NodeExecutionEnv::new(directory.path())),
        context(),
        JsonlStorageOptions::default(),
    )
    .await
    .expect_err("duplicate sequences fail open");
    assert!(
        error
            .to_string()
            .contains("Commit sequence does not strictly increase"),
        "{error}"
    );
}

#[tokio::test]
async fn rejects_structurally_invalid_confirmed_document_content() {
    let directory = TempDir::new("invalid-content");
    let storage = open_storage(&directory).await;
    create_root(&storage).await;
    let document_id = mint_document(&storage).await;
    commit(
        &storage,
        &[doc_create(
            session_document(document_id, "test.document"),
            base_value(json!({})),
        )],
    )
    .await;
    let path = directory.join(&format!("doc-{document_id}.jsonl"));
    let text = std::fs::read_to_string(&path).expect("read sidecar");
    let mut record: Value = serde_json::from_str(text.trim()).expect("record json");
    record["payload"]["content"]
        .as_object_mut()
        .expect("content object")
        .shift_remove("value");
    std::fs::write(
        &path,
        format!("{}\n", kiana_json::js_object::stringify(&record)),
    )
    .expect("rewrite sidecar");

    let error = JsonlStorage::open(
        directory.path(),
        Arc::new(NodeExecutionEnv::new(directory.path())),
        context(),
        JsonlStorageOptions::default(),
    )
    .await
    .expect_err("invalid document content fails open");
    assert!(
        error.to_string().contains("Invalid document content"),
        "{error}"
    );
}

#[tokio::test]
async fn rejects_malformed_complete_main_and_sidecar_lines() {
    let main_directory = TempDir::new("malformed-main");
    let main_storage = open_storage(&main_directory).await;
    create_root(&main_storage).await;
    append_bytes(&main_directory.join("main.jsonl"), b"{bad}\n");
    let error = JsonlStorage::open(
        main_directory.path(),
        Arc::new(NodeExecutionEnv::new(main_directory.path())),
        context(),
        JsonlStorageOptions::default(),
    )
    .await
    .expect_err("malformed main line fails open");
    assert!(
        error.to_string().contains("Malformed complete main.jsonl"),
        "{error}"
    );

    let sidecar_directory = TempDir::new("malformed-sidecar");
    let sidecar_storage = open_storage(&sidecar_directory).await;
    create_root(&sidecar_storage).await;
    std::fs::write(sidecar_directory.join("doc-99.jsonl"), "{bad}\n").expect("write sidecar");
    let error = JsonlStorage::open(
        sidecar_directory.path(),
        Arc::new(NodeExecutionEnv::new(sidecar_directory.path())),
        context(),
        JsonlStorageOptions::default(),
    )
    .await
    .expect_err("malformed sidecar line fails open");
    assert!(
        error
            .to_string()
            .contains("Malformed complete doc-99.jsonl"),
        "{error}"
    );
}

/// 逐字的外部格式（spec/durable-rest.md §4.1）：行形状、键名、ordinal、seq。
///
/// 期望值来自 TS 参考实现的探针输出（同一逻辑写入序列），键序取**生产写入路径**
/// （`session/transaction.ts` 构造 `DocumentContent` 时 `version` 在前、构造任务记录时
/// `state` 在 `background`/`abortRequested` 之后），与 Rust 共享类型的字段顺序一致；
/// TS 测试夹具里的对象字面量把同样的键排成别的顺序，所以文档 payload 用结构相等断言。
#[tokio::test]
async fn writes_the_documented_main_and_sidecar_line_format() {
    let directory = TempDir::new("golden-format");
    let storage = open_storage(&directory).await;
    // 1. 根会话。
    commit(&storage, &[conversation_write(ROOT_CONVERSATION_ID)]).await;
    let task_id = mint_task(&storage).await;
    assert_eq!(task_id.get(), 2);
    let document_id = mint_document(&storage).await;
    assert_eq!(document_id.get(), 3);

    // 2. 非终态任务只写 sidecar，主日志记 `task.sidecar`。
    commit(&storage, &[task_write(pending_task(task_id, "ready"))]).await;
    assert_eq!(
        read_lines(&directory.join(&format!("task-{task_id}.jsonl"))),
        vec![
            "{\"format\":1,\"type\":\"record\",\"seq\":2,\"ordinal\":0,\"payload\":{\"type\":\"task\",\"value\":{\"id\":2,\"conversationId\":1,\"kind\":\"test.task\",\"version\":1,\"input\":null,\"background\":false,\"abortRequested\":false,\"state\":{\"status\":\"pending\",\"checkpoint\":{\"phase\":\"ready\"}}}}}",
        ]
    );

    // 3. 文档创建与增量。
    commit(
        &storage,
        &[doc_create(
            session_document(document_id, "test.document"),
            base_value(json!({ "count": 0 })),
        )],
    )
    .await;
    commit(&storage, &[doc_change(document_id, delta_count(1))]).await;
    let document_lines = read_lines(&directory.join(&format!("doc-{document_id}.jsonl")));
    assert_eq!(document_lines.len(), 2);
    let expected: Vec<Value> = vec![
        json!({
            "format": 1, "type": "record", "seq": 3, "ordinal": 0,
            "payload": {
                "type": "document",
                "id": 3,
                "content": { "kind": "base", "version": 1, "value": { "count": 0 } },
            },
        }),
        json!({
            "format": 1, "type": "record", "seq": 4, "ordinal": 0,
            "payload": {
                "type": "document",
                "id": 3,
                "content": { "kind": "delta", "version": 1, "ops": [["s", ["count"], 1]] },
            },
        }),
    ];
    for (line, expected) in document_lines.iter().zip(expected) {
        assert_eq!(
            serde_json::from_str::<Value>(line).expect("sidecar line"),
            expected
        );
    }

    // 4. 退役（回收 sidecar）与终态任务（写主日志）。
    commit(&storage, &[doc_retire(document_id)]).await;
    commit(&storage, &[task_write(terminal_task(task_id))]).await;
    assert!(!file_exists(
        &directory.join(&format!("doc-{document_id}.jsonl"))
    ));
    assert!(!file_exists(
        &directory.join(&format!("task-{task_id}.jsonl"))
    ));
    assert_eq!(
        read_lines(&directory.join("main.jsonl")),
        vec![
            "{\"format\":1,\"type\":\"commit\",\"seq\":1,\"writes\":[{\"type\":\"conversation\",\"value\":{\"id\":1}}]}",
            "{\"format\":1,\"type\":\"commit\",\"seq\":2,\"writes\":[{\"type\":\"task.sidecar\",\"id\":2,\"ordinal\":0}]}",
            "{\"format\":1,\"type\":\"commit\",\"seq\":3,\"writes\":[{\"type\":\"document.create\",\"record\":{\"id\":3,\"kind\":\"test.document\",\"scope\":{\"kind\":\"session\"}},\"ordinal\":0}]}",
            "{\"format\":1,\"type\":\"commit\",\"seq\":4,\"writes\":[{\"type\":\"document.change\",\"id\":3,\"ordinal\":0}]}",
            "{\"format\":1,\"type\":\"commit\",\"seq\":5,\"writes\":[{\"type\":\"document.retire\",\"id\":3}]}",
            "{\"format\":1,\"type\":\"commit\",\"seq\":6,\"writes\":[{\"type\":\"task\",\"value\":{\"id\":2,\"conversationId\":1,\"kind\":\"test.task\",\"version\":1,\"input\":null,\"background\":false,\"abortRequested\":false,\"state\":{\"status\":\"terminal\",\"outcome\":{\"status\":\"completed\",\"result\":null}}}}]}",
        ]
    );
}

/// `document.copy` 展开后的 sidecar 内容字节。
///
/// TS 的 `memory.resolveDocumentCopies`（`storage/memory.ts:324`）用字面量
/// `{ kind: "base", version, value }` 构造复制内容，`JSON.stringify` 保留该字面量键序；生产路径
/// `session/transaction.ts` 的普通创建则是 `{ version, kind, value }`。两种来源的磁盘字节不同，
/// 所以这里分别逐字节断言。期望值来自 TS 参考实现（runnable copy）的 copy-probe 探针输出。
#[tokio::test]
async fn writes_fork_copied_document_content_in_the_ts_copy_literal_key_order() {
    let directory = TempDir::new("copy-key-order");
    let storage = open_storage(&directory).await;
    // 1. 根会话（seq 1）。
    commit(&storage, &[conversation_write(ROOT_CONVERSATION_ID)]).await;
    let source_id = mint_document(&storage).await;
    assert_eq!(source_id.get(), 2);
    let child_id = mint_document(&storage).await;
    assert_eq!(child_id.get(), 3);
    let child_conversation = id_from_number(mint(&storage, IdKind::Conversation).await);
    // 2. 子会话（seq 2）。
    commit(&storage, &[conversation_write(child_conversation)]).await;

    // 3. 源文档：普通创建，内容键序是生产路径的 version, kind, value（seq 3）。
    let source = DocumentCreate {
        id: source_id,
        kind: "src".to_owned(),
        key: None,
        scope: DocumentScope::Conversation {
            conversation_id: ROOT_CONVERSATION_ID,
        },
        history: Some(kiana_durable::DocumentHistory::Rewindable),
        fork: Some(kiana_durable::DocumentFork::AsOf),
    };
    commit(
        &storage,
        &[doc_create(source, base_value(json!({ "n": 0 })))],
    )
    .await;

    // 4. 复制到子会话（seq 4）：内容键序是 TS 复制字面量的 kind, version, value。
    let child = DocumentCreate {
        id: child_id,
        kind: "src".to_owned(),
        key: None,
        scope: DocumentScope::Conversation {
            conversation_id: child_conversation,
        },
        history: Some(kiana_durable::DocumentHistory::Rewindable),
        fork: Some(kiana_durable::DocumentFork::AsOf),
    };
    commit(
        &storage,
        &[StorageWrite::DocumentCopy {
            record: child,
            source: kiana_durable::DocumentCopySource {
                id: source_id,
                at: DocumentPoint::Current,
            },
        }],
    )
    .await;

    assert_eq!(
        read_lines(&directory.join(&format!("doc-{child_id}.jsonl"))),
        vec![
            "{\"format\":1,\"type\":\"record\",\"seq\":4,\"ordinal\":0,\"payload\":{\"type\":\"document\",\"id\":3,\"content\":{\"kind\":\"base\",\"version\":1,\"value\":{\"n\":0}}}}",
        ]
    );
    assert_eq!(
        read_lines(&directory.join(&format!("doc-{source_id}.jsonl"))),
        vec![
            "{\"format\":1,\"type\":\"record\",\"seq\":3,\"ordinal\":0,\"payload\":{\"type\":\"document\",\"id\":2,\"content\":{\"version\":1,\"kind\":\"base\",\"value\":{\"n\":0}}}}",
        ]
    );
    assert_eq!(
        read_lines(&directory.join("main.jsonl")),
        vec![
            "{\"format\":1,\"type\":\"commit\",\"seq\":1,\"writes\":[{\"type\":\"conversation\",\"value\":{\"id\":1}}]}",
            "{\"format\":1,\"type\":\"commit\",\"seq\":2,\"writes\":[{\"type\":\"conversation\",\"value\":{\"id\":4}}]}",
            "{\"format\":1,\"type\":\"commit\",\"seq\":3,\"writes\":[{\"type\":\"document.create\",\"record\":{\"id\":2,\"kind\":\"src\",\"scope\":{\"kind\":\"conversation\",\"conversationId\":1},\"history\":\"rewindable\",\"fork\":\"asOf\"},\"ordinal\":0}]}",
            "{\"format\":1,\"type\":\"commit\",\"seq\":4,\"writes\":[{\"type\":\"document.create\",\"record\":{\"id\":3,\"kind\":\"src\",\"scope\":{\"kind\":\"conversation\",\"conversationId\":4},\"history\":\"rewindable\",\"fork\":\"asOf\"},\"ordinal\":0}]}",
        ]
    );

    // 复制出来的文档可读，且重开后值不变。
    let reopened = open_storage(&directory).await;
    let copied = ok(reopened
        .document(child_id, DocumentPoint::Current, context())
        .await)
    .expect("copied document");
    assert_eq!(copied.value.get("n"), Some(&json!(0)));
}
