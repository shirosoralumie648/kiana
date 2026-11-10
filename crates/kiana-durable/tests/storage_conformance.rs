//! `packages/durable/src/testing/*` 的契约测试。
//!
//! Rust 没有动态测试注册，conformance 用例由 `register_*` 交给调用方的
//! [`StorageConformanceRunner`]；本文件验证：
//!
//! - 26 个 storage 用例与 29 个 env 用例的标题、顺序逐字来自 TS（ARCHITECTURE §4.8 第 1 条）；
//! - watch 用例的 30s 超时与 `listFiles`/`symlinks` 开关的裁剪规则；
//! - 基准规模、`storageBenchmarkPrimaryRecordCount` 与种子数据的形状；
//! - `createExpectAssertions` 对 `ExpectLike` 的转发，以及 `PanicAssertions` 的失败行为。

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::FutureExt;
use kiana_chord::context::Context;
use serde_json::{Value, json};

use kiana_durable::env::{
    BinaryReader, CreateDirOptions, CreateTempFileOptions, DirPage, DirReader, ExecutionEnv,
    ExecutionError, ExecutionErrorCode, FileContent, FileError, FileErrorCode, FileInfo, FileKind,
    FileSystem, FileWatcher, LineRange, LineScan, OpenBinaryReaderOptions, ReadTextLinesOptions,
    RemoveOptions, Shell, ShellCommand, ShellExecOptions, ShellExecResult, TextLineReader,
    WatchCallback, WatchTarget,
};

use kiana_durable::testing::assertions::{
    ExpectResult, PanicAssertions, PanicExpect, RejectsFuture, matches_subset,
};
use kiana_durable::testing::types::{CaseFuture, CaseRun, ConformanceFuture, EnvUse, StorageUse};
use kiana_durable::testing::{
    EnvConformanceCase, EnvConformanceOptions, EnvConformanceProvider, EnvConformanceRegistration,
    ExpectLike, STORAGE_MEMORY_SCALES, STORAGE_READ_BENCHMARKS, STORAGE_WRITE_BENCHMARKS,
    StorageBenchmarkDataset, StorageBenchmarkScale, StorageConformanceAssertions,
    StorageConformanceCase, StorageConformanceOptions, StorageConformanceProvider,
    StorageConformanceRunner, TIMING_SCALE, create_env_conformance, create_expect_assertions,
    create_storage_conformance, register_env_conformance, register_storage_conformance,
    seed_storage_benchmark, seed_storage_benchmark_with_scale, seed_storage_write_benchmark,
    storage_benchmark_primary_record_count,
};
use kiana_durable::types::{
    AdoptFn, BoxError, ConversationId, ConversationQuery, ConversationRecord, Cursor,
    DocumentAddress, DocumentContentKind, DocumentId, DocumentPoint, DocumentQuery, DocumentRecord,
    EntryId, EntryQuery, EntryRecord, IdKind, Page, Seq, Storage, StorageWrite, StoredDocument,
    StoredEntry, SubmissionId, SubmissionQuery, SubmissionRecord, TaskId, TaskQuery, TaskRecord,
    seq_from_number,
};

// ---------------------------------------------------------------------------
// 逐字来自 TS 的用例标题
// ---------------------------------------------------------------------------

const STORAGE_CASE_NAMES: [&str; 26] = [
    "reserves ID 1 for the immutable root conversation",
    "adopts a durable commit once before resolving, and never a failed one",
    "commits mixed table writes atomically and rolls all of them back on failure",
    "detaches retained writes and every returned record",
    "detaches prototype-like JSON keys without changing object prototypes",
    "indexes entries committed out of ID order",
    "continues an entry cursor below its last item after a newer commit",
    "paginates conversations by opaque cursor in ascending ID order",
    "filters and pages conversations by durable owner edges",
    "scans deep fork history newest-first through every ancestor cap",
    "scans tables in either ID order and continues a cursor in its order",
    "replaces complete task records and pages filtered task scans",
    "stores owners and scans waiting and completing tasks by status",
    "stores and scans sleeping tasks by status",
    "indexes request IDs per conversation and replaces complete submission records",
    "stores passive write submissions without input-only lifecycle states",
    "reconstructs rewindable documents and preserves half-open incarnations",
    "streams long document tails across root replacement deltas",
    "copies stored document bases independently and rejects ambiguous sources",
    "uses bases for version transitions and rejects historical reads of current-only documents",
    "indexes logical addresses and exact-scope scans independently",
    "keeps document lifecycle failures atomic and gives create-plus-retire an empty lifetime",
    "rolls back record tables and secondary indexes when a document command fails",
    "keeps indexed string identities lossless",
    "keeps one global record ID namespace and rejects exhausted ID minting",
    "rejects every operation after close",
];

const ENV_CASE_NAMES: [&str; 29] = [
    "binary reader reads byte ranges of the opened file",
    "binary reader scans lines like decoding the whole file",
    "binary reader keeps reading the file it opened after a rename",
    "binary reader refuses directories, missing files and aborted opens",
    "directory reader pages every entry exactly once",
    "directory reader reports the end and refuses use after close",
    "directory reader refuses missing paths and files",
    "directory reader skips entries removed during enumeration",
    "watch reports a missing file's creation, changes, replacement and removal",
    "watch reports a missing target whose ancestors are created",
    "watch follows directories created together with their contents",
    "watch keeps watching a path whose parent is renamed and recreated",
    "watch skips excluded entries and reports a rename out of them",
    "watch keeps recursive coverage where a non-recursive target overlaps",
    "watch follows a directory replaced at the same path",
    "watch stops reporting once closed",
    "argv exec passes arguments to the program without shell parsing",
    "exec reports the stream of every chunk in both forms",
    "argv exec honors cwd and exit codes",
    "argv exec reports missing programs and empty argv as spawn errors",
    "windowed exec keeps the exact tail and counts what it skips",
    "argv exec distinguishes timeout from abort",
    "listFiles lists files and directories depth first in name order",
    "listFiles skips what git ignores in work trees, .git, and excluded names",
    "listFiles stops at its bounds and reports truncated",
    "listFiles fails on missing directories, files, and aborts",
    "listFiles lists symbolic links without following them",
    "binary reader follows symlinks unless noFollow refuses the final one",
    "watch reports changes to the file a watched symbolic link points to",
];

/// `watchCase`（TS）加 30s 超时的用例下标。
const ENV_WATCH_INDICES: [usize; 9] = [8, 9, 10, 11, 12, 13, 14, 15, 28];

const LIST_FILES_CASE_NAMES: [&str; 5] = [
    "listFiles lists files and directories depth first in name order",
    "listFiles skips what git ignores in work trees, .git, and excluded names",
    "listFiles stops at its bounds and reports truncated",
    "listFiles fails on missing directories, files, and aborts",
    "listFiles lists symbolic links without following them",
];

const SYMLINK_CASE_NAMES: [&str; 3] = [
    "listFiles lists symbolic links without following them",
    "binary reader follows symlinks unless noFollow refuses the final one",
    "watch reports changes to the file a watched symbolic link points to",
];

// ---------------------------------------------------------------------------
// 测试夹具
// ---------------------------------------------------------------------------

fn empty_case() -> CaseFuture {
    Box::pin(async {})
}

fn storage_provider() -> StorageConformanceProvider {
    Arc::new(|_run: StorageUse| empty_case())
}

fn env_provider() -> EnvConformanceProvider {
    Arc::new(|_run: EnvUse| empty_case())
}

fn storage_options() -> StorageConformanceOptions {
    StorageConformanceOptions {
        assertions: Arc::new(PanicAssertions),
        with_storage: storage_provider(),
    }
}

fn env_options(list_files: bool, symlinks: bool) -> EnvConformanceOptions {
    EnvConformanceOptions {
        assertions: Arc::new(PanicAssertions),
        with_env: env_provider(),
        shell: None,
        symlinks: Some(symlinks),
        list_files: Some(list_files),
    }
}

fn names_without<'a>(remove: &[&str], options: &[&'a str]) -> Vec<&'a str> {
    options
        .iter()
        .copied()
        .filter(|name| !remove.contains(name))
        .collect()
}

/// 只记录写入与 mint 的 Storage；读取方法一律报错（种子与基准只写不读）。
struct RecordingStorage {
    next_id: Mutex<i64>,
    next_seq: Mutex<i64>,
    mint_count: AtomicUsize,
    commits: Mutex<Vec<Vec<StorageWrite>>>,
}

impl RecordingStorage {
    fn new() -> Self {
        RecordingStorage {
            next_id: Mutex::new(1),
            next_seq: Mutex::new(0),
            mint_count: AtomicUsize::new(0),
            commits: Mutex::new(Vec::new()),
        }
    }

    fn commits(&self) -> Vec<Vec<StorageWrite>> {
        self.commits.lock().expect("commits lock").clone()
    }

    fn mint_count(&self) -> usize {
        self.mint_count.load(Ordering::SeqCst)
    }
}

fn unsupported() -> BoxError {
    "recording storage only supports commit and mint_id".into()
}

#[async_trait]
impl Storage for RecordingStorage {
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: AdoptFn,
        _context: &dyn Context,
    ) -> Result<Seq, BoxError> {
        let seq = {
            let mut next = self.next_seq.lock().expect("seq lock");
            *next += 1;
            seq_from_number(*next)
        };
        self.commits
            .lock()
            .expect("commits lock")
            .push(writes.to_vec());
        adopt(seq);
        Ok(seq)
    }

    async fn mint_id(&self, _kind: IdKind) -> Result<i64, BoxError> {
        self.mint_count.fetch_add(1, Ordering::SeqCst);
        let mut next = self.next_id.lock().expect("id lock");
        *next += 1;
        Ok(*next)
    }

    async fn conversation(
        &self,
        _id: ConversationId,
        _context: &dyn Context,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        Err(unsupported())
    }

    async fn scan_conversations(
        &self,
        _query: &ConversationQuery,
        _limit: usize,
        _cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        Err(unsupported())
    }

    async fn entry(
        &self,
        _id: EntryId,
        _context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        Err(unsupported())
    }

    async fn visible_entry(
        &self,
        _conversation_id: ConversationId,
        _id: EntryId,
        _context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        Err(unsupported())
    }

    async fn find_latest_head_marker(
        &self,
        _conversation_id: ConversationId,
        _at_or_before_entry_id: Option<EntryId>,
        _context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError> {
        Err(unsupported())
    }

    async fn scan_entries(
        &self,
        _query: &EntryQuery,
        _limit: usize,
        _cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<EntryRecord>, BoxError> {
        Err(unsupported())
    }

    async fn task(
        &self,
        _id: TaskId,
        _context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        Err(unsupported())
    }

    async fn scan_tasks(
        &self,
        _query: &TaskQuery,
        _limit: usize,
        _cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<TaskRecord>, BoxError> {
        Err(unsupported())
    }

    async fn submission(
        &self,
        _id: SubmissionId,
        _context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        Err(unsupported())
    }

    async fn scan_submissions(
        &self,
        _query: &SubmissionQuery,
        _limit: usize,
        _cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<SubmissionRecord>, BoxError> {
        Err(unsupported())
    }

    async fn submission_by_request(
        &self,
        _conversation_id: ConversationId,
        _request_id: &str,
        _context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        Err(unsupported())
    }

    async fn find_document(
        &self,
        _address: &DocumentAddress,
        _at: DocumentPoint,
        _context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError> {
        Err(unsupported())
    }

    async fn document(
        &self,
        _id: DocumentId,
        _at: DocumentPoint,
        _context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError> {
        Err(unsupported())
    }

    async fn scan_documents(
        &self,
        _query: &DocumentQuery,
        _limit: usize,
        _cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<DocumentRecord>, BoxError> {
        Err(unsupported())
    }

    async fn close(&self, _context: &dyn Context) {}
}

fn write_kind(write: &StorageWrite) -> &'static str {
    match write {
        StorageWrite::Conversation { .. } => "conversation",
        StorageWrite::Entry { .. } => "entry",
        StorageWrite::Task { .. } => "task",
        StorageWrite::Submission { .. } => "submission",
        StorageWrite::DocumentCreate { .. } => "document.create",
        StorageWrite::DocumentCopy { .. } => "document.copy",
        StorageWrite::DocumentChange { .. } => "document.change",
        StorageWrite::DocumentRetire { .. } => "document.retire",
    }
}

// ---------------------------------------------------------------------------
// 用例标题、顺序与超时
// ---------------------------------------------------------------------------

#[test]
fn storage_conformance_exposes_typescript_titles_in_order() {
    let cases = create_storage_conformance(&storage_options());
    let names: Vec<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    assert_eq!(names, STORAGE_CASE_NAMES.to_vec());
}

#[test]
fn env_conformance_exposes_typescript_titles_and_watch_timeouts() {
    let cases = create_env_conformance(&env_options(true, true));
    let names: Vec<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    assert_eq!(names, ENV_CASE_NAMES.to_vec());
    for (index, case) in cases.iter().enumerate() {
        let expected = if ENV_WATCH_INDICES.contains(&index) {
            Some(30_000)
        } else {
            None
        };
        assert_eq!(case.timeout_ms, expected, "timeout for {}", case.name);
    }
}

#[test]
fn env_conformance_gates_list_files_and_symlink_cases() {
    let without_list_files = create_env_conformance(&env_options(false, true));
    let names: Vec<&str> = without_list_files
        .iter()
        .map(|case| case.name.as_str())
        .collect();
    assert_eq!(
        names,
        names_without(&LIST_FILES_CASE_NAMES, &ENV_CASE_NAMES)
    );

    let without_symlinks = create_env_conformance(&env_options(true, false));
    let names: Vec<&str> = without_symlinks
        .iter()
        .map(|case| case.name.as_str())
        .collect();
    assert_eq!(names, names_without(&SYMLINK_CASE_NAMES, &ENV_CASE_NAMES));

    let minimal = create_env_conformance(&env_options(false, false));
    let names: Vec<&str> = minimal.iter().map(|case| case.name.as_str()).collect();
    let mut removed = LIST_FILES_CASE_NAMES.to_vec();
    removed.extend(SYMLINK_CASE_NAMES.iter().copied());
    assert_eq!(names, names_without(&removed, &ENV_CASE_NAMES));
    assert_eq!(minimal.len(), 22);
}

#[derive(Default)]
struct CollectingRunner {
    suites: Mutex<Vec<String>>,
    items: Mutex<Vec<(String, Option<u64>)>>,
}

impl StorageConformanceRunner for CollectingRunner {
    fn describe(&self, name: &str, suite: &mut dyn FnMut()) {
        self.suites
            .lock()
            .expect("suites lock")
            .push(name.to_owned());
        suite();
    }

    fn expect(&self) -> Arc<dyn ExpectLike> {
        Arc::new(PanicExpect)
    }

    fn it(&self, name: &str, _test: CaseRun, timeout_ms: Option<u64>) {
        self.items
            .lock()
            .expect("items lock")
            .push((name.to_owned(), timeout_ms));
    }
}

#[test]
fn runner_registration_forwards_titles_and_timeouts() {
    let runner = CollectingRunner::default();
    let cases = register_storage_conformance(&runner, "storage", storage_provider());
    assert_eq!(cases.len(), STORAGE_CASE_NAMES.len());
    assert_eq!(
        *runner.suites.lock().expect("suites lock"),
        vec!["storage".to_owned()]
    );
    let items = runner.items.lock().expect("items lock").clone();
    let names: Vec<&str> = items.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, STORAGE_CASE_NAMES.to_vec());
    assert!(items.iter().all(|(_, timeout)| timeout.is_none()));

    let runner = CollectingRunner::default();
    let cases = register_env_conformance(
        &runner,
        "env",
        env_provider(),
        EnvConformanceRegistration {
            list_files: Some(true),
            symlinks: Some(true),
            ..EnvConformanceRegistration::default()
        },
    );
    assert_eq!(cases.len(), ENV_CASE_NAMES.len());
    let items = runner.items.lock().expect("items lock").clone();
    let names: Vec<&str> = items.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ENV_CASE_NAMES.to_vec());
    for (index, (name, timeout)) in items.iter().enumerate() {
        let expected = if ENV_WATCH_INDICES.contains(&index) {
            Some(30_000)
        } else {
            None
        };
        assert_eq!(*timeout, expected, "timeout for {name}");
    }
}

// ---------------------------------------------------------------------------
// 基准表与种子
// ---------------------------------------------------------------------------

#[test]
fn benchmark_scales_and_record_counts_match_typescript() {
    assert_eq!(TIMING_SCALE.name, "timing");
    assert_eq!(storage_benchmark_primary_record_count(&TIMING_SCALE), 1_870);
    assert_eq!(STORAGE_MEMORY_SCALES.len(), 2);
    assert_eq!(STORAGE_MEMORY_SCALES[0].name, "1k");
    assert_eq!(STORAGE_MEMORY_SCALES[0].entry_count, 1_000);
    assert_eq!(STORAGE_MEMORY_SCALES[0].task_count, 200);
    assert_eq!(STORAGE_MEMORY_SCALES[0].document_count, 200);
    assert_eq!(
        storage_benchmark_primary_record_count(&STORAGE_MEMORY_SCALES[0]),
        1_670
    );
    assert_eq!(STORAGE_MEMORY_SCALES[1].name, "10k");
    assert_eq!(STORAGE_MEMORY_SCALES[1].entry_count, 10_000);
    assert_eq!(STORAGE_MEMORY_SCALES[1].task_count, 2_000);
    assert_eq!(STORAGE_MEMORY_SCALES[1].document_count, 2_000);
    assert_eq!(
        storage_benchmark_primary_record_count(&STORAGE_MEMORY_SCALES[1]),
        14_270
    );
}

#[test]
fn benchmark_tables_match_typescript() {
    let read_names: Vec<&str> = STORAGE_READ_BENCHMARKS
        .iter()
        .map(|benchmark| benchmark.name)
        .collect();
    assert_eq!(
        read_names,
        vec![
            "exact entry lookup",
            "entry page scan (100)",
            "filtered task scan (50)",
            "exact document address among many",
            "document replay tail (0)",
            "document replay tail (16)",
            "document replay tail (128)",
            "document replay tail (1024)",
            "ancient historical read before newer base",
            "recent historical read after newer base",
            "fork-depth history scan (100)",
            "fork-depth head lookup",
        ]
    );
    let write_benchmarks: Vec<(&str, f64)> = STORAGE_WRITE_BENCHMARKS
        .iter()
        .map(|benchmark| (benchmark.name, benchmark.expected))
        .collect();
    assert_eq!(
        write_benchmarks,
        vec![
            ("commit one entry", 1.0),
            ("commit 100 entries", 100.0),
            ("commit mixed entry/task/submission/document", 4.0),
        ]
    );
}

#[tokio::test]
async fn seeding_storage_benchmark_writes_the_documented_dataset() {
    let scale = StorageBenchmarkScale {
        name: "test",
        entry_count: 5,
        task_count: 3,
        document_count: 2,
    };
    let storage = RecordingStorage::new();
    let dataset = seed_storage_benchmark_with_scale(&storage, &scale)
        .await
        .expect("seed benchmark");

    assert_eq!(dataset.first_entry_id.get(), 2);
    assert_eq!(dataset.filtered_task_count, 1);
    assert_eq!(dataset.exact_document_id.get(), 11);
    assert_eq!(dataset.exact_document_key, "key-1");
    assert_eq!(dataset.replay_document_ids.len(), 4);
    assert_eq!(
        dataset.replay_document_ids.get(&0).map(|id| id.get()),
        Some(12)
    );
    assert_eq!(
        dataset.replay_document_ids.get(&16).map(|id| id.get()),
        Some(13)
    );
    assert_eq!(
        dataset.replay_document_ids.get(&128).map(|id| id.get()),
        Some(14)
    );
    assert_eq!(
        dataset.replay_document_ids.get(&1024).map(|id| id.get()),
        Some(15)
    );
    assert_eq!(dataset.historical_document_id.get(), 16);
    assert_eq!(dataset.deepest_conversation_id.get(), 248);
    assert_eq!(dataset.ancestor_head_entry_id.get(), 2);
    // 提交序号：1 root、2 entries、3 tasks、4 documents、5 replay create、
    // 6..=1029 replay deltas、1030 historical create、1031..=1158 ancient deltas、
    // 1159 base checkpoint、1160..=1287 recent deltas、1288..=1303 forks。
    assert_eq!(dataset.ancient_at.get(), 1_158);
    assert_eq!(dataset.recent_at.get(), 1_287);

    let commits = storage.commits();
    assert_eq!(commits.len(), 1_303);
    let total: usize = commits.iter().map(Vec::len).sum();
    assert_eq!(total, 1_705);
    let count_kind = |kind: &str| {
        commits
            .iter()
            .flatten()
            .filter(|write| write_kind(write) == kind)
            .count()
    };
    assert_eq!(count_kind("conversation"), 9);
    assert_eq!(count_kind("entry"), 261);
    assert_eq!(count_kind("task"), 3);
    assert_eq!(count_kind("document.create"), 7);
    assert_eq!(count_kind("document.change"), 1_425);
    assert_eq!(count_kind("document.copy"), 0);
    assert_eq!(count_kind("document.retire"), 0);
    assert_eq!(count_kind("submission"), 0);
    assert_eq!(storage.mint_count(), 279);

    assert_eq!(commits[0].len(), 1);
    assert!(matches!(commits[0][0], StorageWrite::Conversation { .. }));
    assert_eq!(commits[1].len(), 5);
    match &commits[1][0] {
        StorageWrite::Entry { value } => {
            assert_eq!(value.id.get(), 2);
            assert_eq!(value.head.map(|head| head.get()), Some(2));
        }
        other => panic!("unexpected write: {other:?}"),
    }
    assert_eq!(commits[2].len(), 3);
    assert_eq!(commits[3].len(), 2);
    assert_eq!(commits[4].len(), 4);
    assert!(
        commits[4]
            .iter()
            .all(|write| matches!(write, StorageWrite::DocumentCreate { .. }))
    );
    // replay tail 的变化批次：1..=16 写 3 条，17..=128 写 2 条，129..=1024 写 1 条。
    assert_eq!(commits[5].len(), 3);
    assert_eq!(commits[5 + 15].len(), 3);
    assert_eq!(commits[5 + 16].len(), 2);
    assert_eq!(commits[5 + 127].len(), 2);
    assert_eq!(commits[5 + 128].len(), 1);
    assert_eq!(commits[5 + 1023].len(), 1);
    assert!(matches!(
        commits[1029][0],
        StorageWrite::DocumentCreate { .. }
    ));
    // 历史文档在新 base 之前的读点是最后一条 ancient delta。
    match &commits[1158][0] {
        StorageWrite::DocumentChange { content, .. } => {
            assert!(matches!(content.kind, DocumentContentKind::Base));
        }
        other => panic!("unexpected write: {other:?}"),
    }
    // 8 层 fork，每层一条会话 + 32 条条目。
    assert_eq!(commits[1287].len(), 1);
    assert!(matches!(
        commits[1287][0],
        StorageWrite::Conversation { .. }
    ));
    assert_eq!(commits[1288].len(), 32);
    assert_eq!(commits[1302].len(), 32);
    match &commits[1302][0] {
        StorageWrite::Entry { value } => {
            assert_eq!(value.conversation_id.get(), 248);
        }
        other => panic!("unexpected write: {other:?}"),
    }
}

#[tokio::test]
async fn seeding_write_benchmark_writes_baseline_entries() {
    let storage = RecordingStorage::new();
    seed_storage_write_benchmark(&storage)
        .await
        .expect("seed write benchmark");
    let commits = storage.commits();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].len(), 1);
    assert!(matches!(commits[0][0], StorageWrite::Conversation { .. }));
    assert_eq!(commits[1].len(), 100);
    for (index, write) in commits[1].iter().enumerate() {
        match write {
            StorageWrite::Entry { value } => {
                assert_eq!(value.id.get(), 2 + index as i64);
                assert_eq!(value.kind, "benchmark.baseline");
                assert_eq!(
                    value
                        .data
                        .as_ref()
                        .and_then(|data| data.get("index"))
                        .and_then(Value::as_i64),
                    Some(index as i64)
                );
            }
            other => panic!("unexpected write: {other:?}"),
        }
    }
    assert_eq!(storage.mint_count(), 100);
}

#[tokio::test]
async fn seeding_storage_benchmark_without_a_scale_uses_the_timing_scale() {
    // TS: `seedStorageBenchmark(storage, scale: StorageBenchmarkScale = TIMING_SCALE)` —— 省略
    // scale 的调用（packages/durable/test/storage.bench.ts）必须与显式传 TIMING_SCALE 等价。
    let storage = RecordingStorage::new();
    let dataset = seed_storage_benchmark(&storage)
        .await
        .expect("seed benchmark");

    assert_eq!(dataset.exact_document_key, "key-299");
    assert_eq!(dataset.filtered_task_count, 5); // min(50, ceil(300 / 60))
    assert_eq!(dataset.ancestor_head_entry_id.get(), 2);
    assert_eq!(dataset.replay_document_ids.len(), 4);

    let commits = storage.commits();
    // 1 root + 10 个 entry 批 + 3 个 task 批 + 3 个 document 批 + 1 replay create
    // + 1024 replay delta + 1 historical create + 128 ancient delta + 1 base
    // + 128 recent delta + 8 conversation + 8 fork-entry 批。
    assert_eq!(commits.len(), 1_316);
    assert_eq!(commits[1].len(), 100);
    assert_eq!(commits[10].len(), 100);
    assert_eq!(commits[11].len(), 100);
    assert_eq!(commits[13].len(), 100);
    assert_eq!(commits[14].len(), 100);
    assert_eq!(commits[16].len(), 100);
    // 除不受 mint 的 root 会话（ID 1）外，每个主记录恰好 mint 一次。
    assert_eq!(
        storage.mint_count(),
        storage_benchmark_primary_record_count(&TIMING_SCALE) - 1
    );
}

/// 基准读取只需要字段存在；这里用任意合法 ID 构造，便于喂给会报错的 storage。
fn benchmark_dataset_stub() -> StorageBenchmarkDataset {
    use kiana_durable::types::{DocumentBrand, EntryBrand, id_from_number};

    let mut replay_document_ids = std::collections::BTreeMap::new();
    for tail in [0i64, 16, 128, 1_024] {
        replay_document_ids.insert(tail, id_from_number::<DocumentBrand>(20 + tail));
    }
    StorageBenchmarkDataset {
        first_entry_id: id_from_number::<EntryBrand>(2),
        filtered_task_count: 0,
        exact_document_id: id_from_number::<DocumentBrand>(3),
        exact_document_key: "key-0".to_owned(),
        replay_document_ids,
        historical_document_id: id_from_number::<DocumentBrand>(4),
        ancient_at: seq_from_number(1),
        recent_at: seq_from_number(2),
        deepest_conversation_id: id_from_number::<kiana_durable::types::ConversationBrand>(1),
        ancestor_head_entry_id: id_from_number::<EntryBrand>(2),
    }
}

#[tokio::test]
async fn read_benchmarks_propagate_storage_errors_instead_of_sentinel_results() {
    // TS 的 `run` 是 promise：存储报错时 reject。Rust 的 `run` 输出 `f64`，故用 panic 让基准
    // 失败；过去会把 Err 变成 -1 或 NaN，导致“错误的测量值”而不是失败。
    let storage: Arc<dyn Storage> = Arc::new(RecordingStorage::new());
    let dataset = benchmark_dataset_stub();
    for benchmark in STORAGE_READ_BENCHMARKS.iter() {
        let run = Arc::clone(&benchmark.run);
        let outcome = AssertUnwindSafe(run(Arc::clone(&storage), dataset.clone()))
            .catch_unwind()
            .await;
        assert!(
            outcome.is_err(),
            "{} must fail when storage errors",
            benchmark.name
        );
    }
}

#[tokio::test]
async fn read_benchmarks_keep_the_absent_record_sentinels() {
    // 记录缺失（Ok(None)）时 TS 产生 `?? -1` 与 `Number(undefined)`（NaN）；这个只读的
    // 伪造 storage 用来验证哨兵值仍然保留，而不是把“缺失”也当成错误。
    struct MissingStorage;
    #[async_trait]
    impl Storage for MissingStorage {
        async fn commit(
            &self,
            _writes: &[StorageWrite],
            _adopt: AdoptFn,
            _context: &dyn Context,
        ) -> Result<Seq, BoxError> {
            Err("missing storage only reads".into())
        }
        async fn mint_id(&self, _kind: IdKind) -> Result<i64, BoxError> {
            Err("missing storage only reads".into())
        }
        async fn conversation(
            &self,
            _id: ConversationId,
            _context: &dyn Context,
        ) -> Result<Option<ConversationRecord>, BoxError> {
            Ok(None)
        }
        async fn scan_conversations(
            &self,
            _query: &ConversationQuery,
            _limit: usize,
            _cursor: Option<&Cursor>,
            _context: &dyn Context,
        ) -> Result<Page<ConversationRecord>, BoxError> {
            Err("missing storage only reads".into())
        }
        async fn entry(
            &self,
            _id: EntryId,
            _context: &dyn Context,
        ) -> Result<Option<StoredEntry>, BoxError> {
            Ok(None)
        }
        async fn visible_entry(
            &self,
            _conversation_id: ConversationId,
            _id: EntryId,
            _context: &dyn Context,
        ) -> Result<Option<StoredEntry>, BoxError> {
            Ok(None)
        }
        async fn find_latest_head_marker(
            &self,
            _conversation_id: ConversationId,
            _at_or_before_entry_id: Option<EntryId>,
            _context: &dyn Context,
        ) -> Result<Option<EntryRecord>, BoxError> {
            Ok(None)
        }
        async fn scan_entries(
            &self,
            _query: &EntryQuery,
            _limit: usize,
            _cursor: Option<&Cursor>,
            _context: &dyn Context,
        ) -> Result<Page<EntryRecord>, BoxError> {
            Err("missing storage only reads".into())
        }
        async fn task(
            &self,
            _id: TaskId,
            _context: &dyn Context,
        ) -> Result<Option<TaskRecord>, BoxError> {
            Ok(None)
        }
        async fn scan_tasks(
            &self,
            _query: &TaskQuery,
            _limit: usize,
            _cursor: Option<&Cursor>,
            _context: &dyn Context,
        ) -> Result<Page<TaskRecord>, BoxError> {
            Err("missing storage only reads".into())
        }
        async fn submission(
            &self,
            _id: SubmissionId,
            _context: &dyn Context,
        ) -> Result<Option<SubmissionRecord>, BoxError> {
            Ok(None)
        }
        async fn scan_submissions(
            &self,
            _query: &SubmissionQuery,
            _limit: usize,
            _cursor: Option<&Cursor>,
            _context: &dyn Context,
        ) -> Result<Page<SubmissionRecord>, BoxError> {
            Err("missing storage only reads".into())
        }
        async fn submission_by_request(
            &self,
            _conversation_id: ConversationId,
            _request_id: &str,
            _context: &dyn Context,
        ) -> Result<Option<SubmissionRecord>, BoxError> {
            Ok(None)
        }
        async fn find_document(
            &self,
            _address: &DocumentAddress,
            _at: DocumentPoint,
            _context: &dyn Context,
        ) -> Result<Option<DocumentRecord>, BoxError> {
            Ok(None)
        }
        async fn document(
            &self,
            _id: DocumentId,
            _at: DocumentPoint,
            _context: &dyn Context,
        ) -> Result<Option<StoredDocument>, BoxError> {
            Ok(None)
        }
        async fn scan_documents(
            &self,
            _query: &DocumentQuery,
            _limit: usize,
            _cursor: Option<&Cursor>,
            _context: &dyn Context,
        ) -> Result<Page<DocumentRecord>, BoxError> {
            Err("missing storage only reads".into())
        }
        async fn close(&self, _context: &dyn Context) {}
    }

    let storage: Arc<dyn Storage> = Arc::new(MissingStorage);
    let dataset = benchmark_dataset_stub();
    let run = |name: &str| {
        STORAGE_READ_BENCHMARKS
            .iter()
            .find(|benchmark| benchmark.name == name)
            .unwrap_or_else(|| panic!("missing benchmark {name}"))
            .run
            .clone()
    };
    // `?? -1`：记录缺失的三个基准返回 -1，不是 NaN。
    for name in [
        "exact entry lookup",
        "exact document address among many",
        "fork-depth head lookup",
    ] {
        let value = run(name)(Arc::clone(&storage), dataset.clone()).await;
        assert_eq!(value, -1.0, "{name}");
    }
    // `Number(undefined)`：文档缺失的读取是 NaN。
    for name in [
        "document replay tail (0)",
        "document replay tail (16)",
        "document replay tail (128)",
        "document replay tail (1024)",
        "ancient historical read before newer base",
        "recent historical read after newer base",
    ] {
        let value = run(name)(Arc::clone(&storage), dataset.clone()).await;
        assert!(value.is_nan(), "{name}");
    }
}

// ---------------------------------------------------------------------------
// 断言适配器
// ---------------------------------------------------------------------------

#[test]
fn panic_assertions_accept_matching_values_and_panic_on_mismatch() {
    let assertions = PanicAssertions;
    assertions.ok(true, None);
    assertions.strict_equal(&json!(1), &json!(1));
    assertions.deep_equal(&json!({ "a": [1, 2] }), &json!({ "a": [1, 2] }));
    assertions.partial_deep_equal(&json!({ "a": 1, "b": 2 }), &json!({ "a": 1 }));
    assertions.greater_than(2.0, 1.0);

    assert!(
        std::panic::catch_unwind(AssertUnwindSafe(|| {
            assertions.deep_equal(&json!(1), &json!(2));
        }))
        .is_err()
    );
    assert!(
        std::panic::catch_unwind(AssertUnwindSafe(|| {
            assertions.ok(false, Some("boom"));
        }))
        .is_err()
    );
    assert!(
        std::panic::catch_unwind(AssertUnwindSafe(|| {
            assertions.partial_deep_equal(&json!({ "a": 1 }), &json!({ "a": 2 }));
        }))
        .is_err()
    );
    assert!(matches_subset(
        &json!({ "a": { "b": 1 }, "c": 2 }),
        &json!({ "a": { "b": 1 } })
    ));
    assert!(!matches_subset(&json!({ "a": 1 }), &json!({ "a": 2 })));
    // 数组是等长子集匹配，不是前缀匹配（Vitest `toMatchObject` 语义）。
    assert!(!matches_subset(&json!([1, 2]), &json!([1])));
}

#[tokio::test]
async fn panic_expect_rejects_checks_the_error_message() {
    let matched = AssertUnwindSafe(PanicExpect.rejects(
        Box::pin(async { Err::<Value, BoxError>("boom: bad".into()) }),
        "bad",
    ))
    .catch_unwind()
    .await;
    assert!(matched.is_ok());

    let unmatched = AssertUnwindSafe(PanicExpect.rejects(
        Box::pin(async { Err::<Value, BoxError>("boom: bad".into()) }),
        "missing",
    ))
    .catch_unwind()
    .await;
    assert!(unmatched.is_err());

    let resolved = AssertUnwindSafe(PanicExpect.rejects(
        Box::pin(async { Ok::<Value, BoxError>(json!(1)) }),
        "anything",
    ))
    .catch_unwind()
    .await;
    assert!(resolved.is_err());
}

#[derive(Debug, PartialEq)]
enum RecordedOp {
    ToBe(Value),
    Truthy,
    Equal(Value),
    MatchObject(Value),
    GreaterThan(f64),
}

#[derive(Debug)]
struct RecordedExpect {
    actual: Value,
    op: RecordedOp,
    message: Option<String>,
}

#[derive(Default)]
struct RecordingExpect {
    seen: Mutex<Vec<RecordedExpect>>,
    rejects: Mutex<Vec<String>>,
}

struct RecordingResult<'a> {
    owner: &'a RecordingExpect,
    actual: Value,
    message: Option<String>,
}

impl RecordingResult<'_> {
    fn record(&self, op: RecordedOp) {
        self.owner
            .seen
            .lock()
            .expect("seen lock")
            .push(RecordedExpect {
                actual: self.actual.clone(),
                op,
                message: self.message.clone(),
            });
    }
}

impl ExpectResult for RecordingResult<'_> {
    fn to_be(&self, expected: &Value) {
        self.record(RecordedOp::ToBe(expected.clone()));
    }

    fn to_be_greater_than(&self, expected: f64) {
        self.record(RecordedOp::GreaterThan(expected));
    }

    fn to_equal(&self, expected: &Value) {
        self.record(RecordedOp::Equal(expected.clone()));
    }

    fn to_match_object(&self, expected: &Value) {
        self.record(RecordedOp::MatchObject(expected.clone()));
    }

    fn to_be_truthy(&self) {
        self.record(RecordedOp::Truthy);
    }
}

impl ExpectLike for RecordingExpect {
    fn expect<'a>(
        &'a self,
        actual: &Value,
        message: Option<&'a str>,
    ) -> Box<dyn ExpectResult + 'a> {
        Box::new(RecordingResult {
            owner: self,
            actual: actual.clone(),
            message: message.map(str::to_owned),
        })
    }

    fn rejects<'a>(
        &'a self,
        operation: ConformanceFuture<'a>,
        message_includes: &'a str,
    ) -> RejectsFuture<'a> {
        self.rejects
            .lock()
            .expect("rejects lock")
            .push(message_includes.to_owned());
        Box::pin(async move {
            let error = operation
                .await
                .expect_err("expected the operation to reject");
            assert!(error.to_string().contains(message_includes));
        })
    }
}

#[tokio::test]
async fn expect_adapter_forwards_to_expect_like() {
    let expect = Arc::new(RecordingExpect::default());
    let assertions = create_expect_assertions(expect.clone());
    assertions.ok(true, Some("must hold"));
    assertions.strict_equal(&json!(1), &json!(1));
    assertions.deep_equal(&json!({ "a": 1 }), &json!({ "a": 1 }));
    assertions.partial_deep_equal(&json!({ "a": 1, "b": 2 }), &json!({ "a": 1 }));
    assertions.greater_than(2.0, 1.0);
    assertions
        .rejects(
            Box::pin(async { Err::<Value, BoxError>("boom happened".into()) }),
            "boom",
        )
        .await;

    let seen = expect.seen.lock().expect("seen lock");
    assert_eq!(seen.len(), 5);
    assert_eq!(seen[0].actual, json!(true));
    assert_eq!(seen[0].op, RecordedOp::Truthy);
    // 只有 `ok` 透传 message（TS `expect(value, message)`）。
    assert_eq!(seen[0].message.as_deref(), Some("must hold"));
    assert_eq!(seen[1].actual, json!(1));
    assert_eq!(seen[1].op, RecordedOp::ToBe(json!(1)));
    assert_eq!(seen[1].message, None);
    assert_eq!(seen[2].op, RecordedOp::Equal(json!({ "a": 1 })));
    assert_eq!(seen[2].message, None);
    assert_eq!(seen[3].op, RecordedOp::MatchObject(json!({ "a": 1 })));
    assert_eq!(seen[3].message, None);
    assert_eq!(seen[4].actual, json!(2.0));
    assert_eq!(seen[4].op, RecordedOp::GreaterThan(1.0));
    assert_eq!(seen[4].message, None);
    drop(seen);
    assert_eq!(
        *expect.rejects.lock().expect("rejects lock"),
        vec!["boom".to_owned()]
    );
}

// ---------------------------------------------------------------------------
// 回归测试：一致性用例必须能辨别后端行为（对照 TS 原文的断言）
// ---------------------------------------------------------------------------

const BINARY_RANGES_CASE: &str = "binary reader reads byte ranges of the opened file";
const DIRECTORY_PAGES_CASE: &str = "directory reader pages every entry exactly once";
const ADOPT_CASE: &str = "adopts a durable commit once before resolving, and never a failed one";

fn env_unsupported(path: &str) -> FileError {
    FileError::new(
        FileErrorCode::NotSupported,
        "not supported by the fake environment",
    )
    .with_path(path)
}

fn env_missing(path: &str) -> FileError {
    FileError::new(
        FileErrorCode::NotFound,
        format!("ENOENT: no such file or directory, open '{path}'"),
    )
    .with_path(path)
}

fn context_aborted(context: &dyn Context) -> bool {
    context
        .abort_signal()
        .is_some_and(|signal| signal.is_aborted())
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// 只实现两个 env 一致性用例所需子集的 `ExecutionEnv`。
///
/// `aborted_code` 让回归测试可以模拟一个对中止上下文返回 `invalid` 的错误 reader。
struct FakeEnv {
    files: Mutex<Vec<(String, Vec<u8>)>>,
    dirs: Mutex<Vec<String>>,
    aborted_code: FileErrorCode,
}

impl FakeEnv {
    fn new(aborted_code: FileErrorCode) -> Self {
        FakeEnv {
            files: Mutex::new(Vec::new()),
            dirs: Mutex::new(Vec::new()),
            aborted_code,
        }
    }
}

#[async_trait]
impl FileSystem for FakeEnv {
    fn id(&self) -> &str {
        "fake-env"
    }

    fn cwd(&self) -> &str {
        "."
    }

    fn set_cwd(&self, _cwd: String) {}

    async fn absolute_path(&self, path: &str, _context: &dyn Context) -> Result<String, FileError> {
        Ok(path.to_owned())
    }

    async fn join_path(
        &self,
        parts: &[String],
        _context: &dyn Context,
    ) -> Result<String, FileError> {
        Ok(parts.join("/"))
    }

    async fn read_text_file(
        &self,
        path: &str,
        _context: &dyn Context,
    ) -> Result<String, FileError> {
        Err(env_unsupported(path))
    }

    async fn open_text_line_reader(
        &self,
        path: &str,
        _context: &dyn Context,
    ) -> Result<Arc<dyn TextLineReader>, FileError> {
        Err(env_unsupported(path))
    }

    async fn read_text_lines(
        &self,
        path: &str,
        _options: Option<&ReadTextLinesOptions>,
        _context: &dyn Context,
    ) -> Result<Vec<String>, FileError> {
        Err(env_unsupported(path))
    }

    async fn read_binary_file(
        &self,
        path: &str,
        _context: &dyn Context,
    ) -> Result<Vec<u8>, FileError> {
        Err(env_unsupported(path))
    }

    async fn open_binary_reader(
        &self,
        path: &str,
        _options: Option<&OpenBinaryReaderOptions>,
        _context: &dyn Context,
    ) -> Result<Arc<dyn BinaryReader>, FileError> {
        let files = self.files.lock().expect("files lock");
        let bytes = files
            .iter()
            .find(|(name, _)| name == path)
            .map(|(_, bytes)| bytes.clone())
            .ok_or_else(|| env_missing(path))?;
        drop(files);
        Ok(Arc::new(FakeBinaryReader {
            name: basename(path).to_owned(),
            path: path.to_owned(),
            bytes,
            closed: AtomicBool::new(false),
            aborted_code: self.aborted_code,
        }))
    }

    async fn write_file(
        &self,
        path: &str,
        content: FileContent,
        _context: &dyn Context,
    ) -> Result<(), FileError> {
        let bytes = match content {
            FileContent::Text(text) => text.into_bytes(),
            FileContent::Bytes(bytes) => bytes,
        };
        let mut files = self.files.lock().expect("files lock");
        match files.iter_mut().find(|(name, _)| name == path) {
            Some(file) => file.1 = bytes,
            None => files.push((path.to_owned(), bytes)),
        }
        Ok(())
    }

    async fn append_file(
        &self,
        path: &str,
        _content: FileContent,
        _context: &dyn Context,
    ) -> Result<(), FileError> {
        Err(env_unsupported(path))
    }

    async fn truncate_file(
        &self,
        path: &str,
        _size: u64,
        _context: &dyn Context,
    ) -> Result<(), FileError> {
        Err(env_unsupported(path))
    }

    async fn flush_file(&self, path: &str, _context: &dyn Context) -> Result<(), FileError> {
        Err(env_unsupported(path))
    }

    async fn rename_file(
        &self,
        _source_path: &str,
        _destination_path: &str,
        _context: &dyn Context,
    ) -> Result<(), FileError> {
        Err(env_unsupported("rename"))
    }

    async fn file_info(&self, path: &str, _context: &dyn Context) -> Result<FileInfo, FileError> {
        Err(env_unsupported(path))
    }

    async fn list_dir(
        &self,
        path: &str,
        _context: &dyn Context,
    ) -> Result<Vec<FileInfo>, FileError> {
        Err(env_unsupported(path))
    }

    async fn open_dir_reader(
        &self,
        _path: &str,
        _context: &dyn Context,
    ) -> Result<Arc<dyn DirReader>, FileError> {
        let mut entries: Vec<FileInfo> = self
            .files
            .lock()
            .expect("files lock")
            .iter()
            .map(|(name, bytes)| FileInfo {
                name: basename(name).to_owned(),
                path: name.clone(),
                kind: FileKind::File,
                size: bytes.len() as u64,
                mtime_ms: 0.0,
            })
            .collect();
        entries.extend(
            self.dirs
                .lock()
                .expect("dirs lock")
                .iter()
                .map(|name| FileInfo {
                    name: basename(name).to_owned(),
                    path: name.clone(),
                    kind: FileKind::Directory,
                    size: 0,
                    mtime_ms: 0.0,
                }),
        );
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(Arc::new(FakeDirReader {
            entries,
            cursor: Mutex::new(0),
        }))
    }

    async fn watch(
        &self,
        _targets: &[WatchTarget],
        _on_change: WatchCallback,
        _context: &dyn Context,
    ) -> Result<Arc<dyn FileWatcher>, FileError> {
        Err(env_unsupported("watch"))
    }

    async fn canonical_path(
        &self,
        path: &str,
        _context: &dyn Context,
    ) -> Result<String, FileError> {
        Err(env_unsupported(path))
    }

    async fn exists(&self, path: &str, _context: &dyn Context) -> Result<bool, FileError> {
        Err(env_unsupported(path))
    }

    async fn create_dir(
        &self,
        path: &str,
        _options: Option<&CreateDirOptions>,
        _context: &dyn Context,
    ) -> Result<(), FileError> {
        let mut dirs = self.dirs.lock().expect("dirs lock");
        if !dirs.iter().any(|dir| dir == path) {
            dirs.push(path.to_owned());
        }
        Ok(())
    }

    async fn remove(
        &self,
        path: &str,
        _options: Option<&RemoveOptions>,
        _context: &dyn Context,
    ) -> Result<(), FileError> {
        Err(env_unsupported(path))
    }

    async fn create_temp_dir(
        &self,
        _prefix: Option<&str>,
        _context: &dyn Context,
    ) -> Result<String, FileError> {
        Err(env_unsupported("createTempDir"))
    }

    async fn create_temp_file(
        &self,
        _options: Option<&CreateTempFileOptions>,
        _context: &dyn Context,
    ) -> Result<String, FileError> {
        Err(env_unsupported("createTempFile"))
    }

    async fn cleanup(&self, _context: &dyn Context) {}
}

#[async_trait]
impl Shell for FakeEnv {
    async fn exec(
        &self,
        _command: ShellCommand,
        _options: Option<&ShellExecOptions>,
        _context: &dyn Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        Err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            "the fake environment has no shell",
        ))
    }

    async fn cleanup(&self, _context: &dyn Context) {}
}

struct FakeBinaryReader {
    name: String,
    path: String,
    bytes: Vec<u8>,
    closed: AtomicBool,
    aborted_code: FileErrorCode,
}

#[async_trait]
impl BinaryReader for FakeBinaryReader {
    async fn info(&self, _context: &dyn Context) -> Result<FileInfo, FileError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(
                FileError::new(FileErrorCode::Invalid, "Binary reader is closed")
                    .with_path(&self.path),
            );
        }
        Ok(FileInfo {
            name: self.name.clone(),
            path: self.path.clone(),
            kind: FileKind::File,
            size: self.bytes.len() as u64,
            mtime_ms: 0.0,
        })
    }

    async fn read(
        &self,
        offset: u64,
        length: usize,
        context: &dyn Context,
    ) -> Result<Vec<u8>, FileError> {
        if context_aborted(context) {
            return Err(
                FileError::new(self.aborted_code, "The operation was aborted")
                    .with_path(&self.path),
            );
        }
        if self.closed.load(Ordering::SeqCst) {
            return Err(
                FileError::new(FileErrorCode::Invalid, "Binary reader is closed")
                    .with_path(&self.path),
            );
        }
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(self.bytes.len());
        let end = start.saturating_add(length).min(self.bytes.len());
        Ok(self.bytes[start..end].to_vec())
    }

    async fn scan_lines(
        &self,
        _range: LineRange,
        _context: &dyn Context,
    ) -> Result<LineScan, FileError> {
        Err(env_unsupported(&self.path))
    }

    async fn close(&self, _context: &dyn Context) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

struct FakeDirReader {
    entries: Vec<FileInfo>,
    cursor: Mutex<usize>,
}

#[async_trait]
impl DirReader for FakeDirReader {
    async fn next(&self, max_entries: usize, _context: &dyn Context) -> Result<DirPage, FileError> {
        if max_entries == 0 {
            return Err(FileError::new(
                FileErrorCode::Invalid,
                "maxEntries must be a positive safe integer",
            ));
        }
        let mut cursor = self.cursor.lock().expect("cursor lock");
        let start = *cursor;
        let end = start.saturating_add(max_entries).min(self.entries.len());
        *cursor = end;
        Ok(DirPage {
            entries: self.entries[start..end].to_vec(),
            done: end >= self.entries.len(),
        })
    }

    async fn close(&self, _context: &dyn Context) {}
}

fn fake_env_options(
    invocations: Arc<AtomicUsize>,
    aborted_code: FileErrorCode,
) -> EnvConformanceOptions {
    EnvConformanceOptions {
        assertions: Arc::new(PanicAssertions),
        with_env: Arc::new(move |use_env: EnvUse| {
            invocations.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let env: Arc<dyn ExecutionEnv> = Arc::new(FakeEnv::new(aborted_code));
                use_env(env).await;
            })
        }),
        shell: None,
        symlinks: None,
        list_files: None,
    }
}

fn find_env_case(options: &EnvConformanceOptions, name: &str) -> EnvConformanceCase {
    create_env_conformance(options)
        .into_iter()
        .find(|case| case.name == name)
        .expect("env conformance case")
}

/// 回归：`read(0, 1, abortedContext())` 必须断言 `aborted`，且 `FileInfo.kind` 必须投影成
/// `file`/`directory`/`symlink`（而不是 `listFiles` 行用的 `f`/`d`/`l`）。
#[tokio::test]
async fn env_cases_assert_aborted_reads_and_file_kinds_like_typescript() {
    for name in [BINARY_RANGES_CASE, DIRECTORY_PAGES_CASE] {
        let invocations = Arc::new(AtomicUsize::new(0));
        let options = fake_env_options(invocations.clone(), FileErrorCode::Aborted);
        let case = find_env_case(&options, name);
        (case.run)().await;
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "case `{name}` ran its environment hook"
        );
    }
}

/// 负向对照：一个对中止上下文返回 `invalid` 的 reader 必须让用例失败（证明上面的用例不是空跑）。
#[tokio::test]
async fn env_case_rejects_a_reader_that_reports_invalid_for_an_aborted_context() {
    let options = fake_env_options(Arc::new(AtomicUsize::new(0)), FileErrorCode::Invalid);
    let case = find_env_case(&options, BINARY_RANGES_CASE);
    let outcome = AssertUnwindSafe((case.run)()).catch_unwind().await;
    assert!(
        outcome.is_err(),
        "the case must reject `invalid` for an aborted read"
    );
}

fn fake_storage_unsupported() -> BoxError {
    "adopt visibility storage supports only commit, mint_id and entry".into()
}

/// 只实现 adopt 用例所需子集的 `Storage`。
///
/// `visible_at_adopt` 决定批次是在 `adopt` 之前还是之后才写入记录表：TS 的契约要求
/// adopt 被调用时批次已经可见，所以后者必须被用例拒绝。
struct AdoptVisibilityStorage {
    visible_at_adopt: bool,
    next_id: Mutex<i64>,
    next_seq: Mutex<i64>,
    conversations: Mutex<HashSet<i64>>,
    records: Mutex<HashMap<i64, StoredEntry>>,
}

impl AdoptVisibilityStorage {
    fn new(visible_at_adopt: bool) -> Self {
        AdoptVisibilityStorage {
            visible_at_adopt,
            next_id: Mutex::new(1),
            next_seq: Mutex::new(0),
            conversations: Mutex::new(HashSet::new()),
            records: Mutex::new(HashMap::new()),
        }
    }

    fn publish(&self, writes: &[StorageWrite], seq: Seq) {
        let mut conversations = self.conversations.lock().expect("conversations lock");
        let mut records = self.records.lock().expect("records lock");
        for write in writes {
            match write {
                StorageWrite::Conversation { value } => {
                    conversations.insert(value.id.get());
                }
                StorageWrite::Entry { value } => {
                    records.insert(
                        value.id.get(),
                        StoredEntry {
                            entry: value.clone(),
                            commit_seq: seq,
                        },
                    );
                }
                _ => {}
            }
        }
    }
}

#[async_trait]
impl Storage for AdoptVisibilityStorage {
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: AdoptFn,
        _context: &dyn Context,
    ) -> Result<Seq, BoxError> {
        {
            let conversations = self.conversations.lock().expect("conversations lock");
            for write in writes {
                if let StorageWrite::Conversation { value } = write
                    && conversations.contains(&value.id.get())
                {
                    return Err(
                        format!("ID {} already belongs to conversation", value.id.get()).into(),
                    );
                }
            }
        }
        let seq = {
            let mut next = self.next_seq.lock().expect("seq lock");
            *next += 1;
            seq_from_number(*next)
        };
        if self.visible_at_adopt {
            self.publish(writes, seq);
            adopt(seq);
        } else {
            adopt(seq);
            self.publish(writes, seq);
        }
        Ok(seq)
    }

    async fn mint_id(&self, _kind: IdKind) -> Result<i64, BoxError> {
        let mut next = self.next_id.lock().expect("id lock");
        *next += 1;
        Ok(*next)
    }

    async fn conversation(
        &self,
        _id: ConversationId,
        _context: &dyn Context,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        Err(fake_storage_unsupported())
    }

    async fn scan_conversations(
        &self,
        _query: &ConversationQuery,
        _limit: usize,
        _cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        Err(fake_storage_unsupported())
    }

    async fn entry(
        &self,
        id: EntryId,
        _context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        Ok(self
            .records
            .lock()
            .expect("records lock")
            .get(&id.get())
            .cloned())
    }

    async fn visible_entry(
        &self,
        _conversation_id: ConversationId,
        _id: EntryId,
        _context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        Err(fake_storage_unsupported())
    }

    async fn find_latest_head_marker(
        &self,
        _conversation_id: ConversationId,
        _at_or_before_entry_id: Option<EntryId>,
        _context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError> {
        Err(fake_storage_unsupported())
    }

    async fn scan_entries(
        &self,
        _query: &EntryQuery,
        _limit: usize,
        _cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<EntryRecord>, BoxError> {
        Err(fake_storage_unsupported())
    }

    async fn task(
        &self,
        _id: TaskId,
        _context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        Err(fake_storage_unsupported())
    }

    async fn scan_tasks(
        &self,
        _query: &TaskQuery,
        _limit: usize,
        _cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<TaskRecord>, BoxError> {
        Err(fake_storage_unsupported())
    }

    async fn submission(
        &self,
        _id: SubmissionId,
        _context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        Err(fake_storage_unsupported())
    }

    async fn scan_submissions(
        &self,
        _query: &SubmissionQuery,
        _limit: usize,
        _cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<SubmissionRecord>, BoxError> {
        Err(fake_storage_unsupported())
    }

    async fn submission_by_request(
        &self,
        _conversation_id: ConversationId,
        _request_id: &str,
        _context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        Err(fake_storage_unsupported())
    }

    async fn find_document(
        &self,
        _address: &DocumentAddress,
        _at: DocumentPoint,
        _context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError> {
        Err(fake_storage_unsupported())
    }

    async fn document(
        &self,
        _id: DocumentId,
        _at: DocumentPoint,
        _context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError> {
        Err(fake_storage_unsupported())
    }

    async fn scan_documents(
        &self,
        _query: &DocumentQuery,
        _limit: usize,
        _cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<DocumentRecord>, BoxError> {
        Err(fake_storage_unsupported())
    }

    async fn close(&self, _context: &dyn Context) {}
}

fn adopt_storage_options(visible_at_adopt: bool) -> StorageConformanceOptions {
    StorageConformanceOptions {
        assertions: Arc::new(PanicAssertions),
        with_storage: Arc::new(move |run: StorageUse| {
            let storage: Arc<dyn Storage> = Arc::new(AdoptVisibilityStorage::new(visible_at_adopt));
            Box::pin(async move { run(storage).await })
        }),
    }
}

fn find_storage_case(options: &StorageConformanceOptions, name: &str) -> StorageConformanceCase {
    create_storage_conformance(options)
        .into_iter()
        .find(|case| case.name == name)
        .expect("storage conformance case")
}

/// 合规后端：adopt 时批次已经可见，用例通过。
#[tokio::test]
async fn adopt_case_passes_when_the_batch_is_visible_at_adopt() {
    let options = adopt_storage_options(true);
    let case = find_storage_case(&options, ADOPT_CASE);
    (case.run)().await;
}

/// 回归：只在 commit resolve 之后才发布写入的后端必须被用例拒绝（读取在 adopt 回调里发起）。
#[tokio::test]
async fn adopt_case_rejects_a_batch_published_only_after_resolving() {
    let options = adopt_storage_options(false);
    let case = find_storage_case(&options, ADOPT_CASE);
    let outcome = AssertUnwindSafe((case.run)()).catch_unwind().await;
    assert!(
        outcome.is_err(),
        "the case must reject a storage that publishes the batch only after resolving"
    );
}
