//! `test/session-support.ts` 的 Rust 等价物：内存 Storage（`storage/memory.ts` 的逐语义移植）、
//! 可控 Storage（提交/查找闸门、注入提交失败）与提交辅助。
//!
//! 生产代码的 Storage 后端由 storage 单元实现；这里是测试替身，供 session 单元自测使用。

#![allow(dead_code)]

// vitest 的 `setupFiles` 等价物（`test/setup-document-retention.ts`）：进程启动时读取
// `KIANA_DURABLE_DOCUMENT_RETENTION_MS` 并改默认文档保留期。
#[path = "document_retention.rs"]
mod document_retention;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_chord::context::Context;
use kiana_chord::delta::Op;
use kiana_chord::delta::index::apply_immutable_batches;
use kiana_durable::harness::types::{AnyTask, AnyTaskDefinition};
use kiana_durable::session::{ErasedTaskInitial, SessionImpl, SessionOptions};
use kiana_durable::storage::scan::{ScanStart, next_cursor, scan_start};
use kiana_durable::{
    BoxError, ConversationId, ConversationQuery, ConversationRecord, Cursor, DocDefinition,
    DocFamilyToken, DocTarget, DocToken, DocumentAddress, DocumentBaseContent,
    DocumentCommitChange, DocumentContent, DocumentContentKind, DocumentCreate, DocumentFork,
    DocumentHistory, DocumentId, DocumentPoint, DocumentQuery, DocumentRecord, DocumentScope,
    DocumentSemantics, EntryDraft, EntryId, EntryQuery, EntryRecord, IdKind, JsonObject, JsonValue,
    Page, ScanOrder, ScopeKind, Seq, Session, SessionEnd, Storage, StorageRequestError,
    StorageWrite, StoredDocument, StoredEntry, SubmissionId, SubmissionQuery, SubmissionRecord,
    TaskId, TaskOptions, TaskOutcome, TaskOwnership, TaskQuery, TaskRecord, TaskState,
    seq_from_number,
};
use serde_json::json;

pub type ControlledSession = (Arc<ControlledStorage>, SessionImpl);

/// `close()` 传给 Storage 的上下文探针键（断言 cleanup 保留了调用方上下文值）。
pub static CLOSE_PROBE_KEY: std::sync::LazyLock<kiana_chord::context::ContextKey<String>> =
    std::sync::LazyLock::new(|| {
        kiana_chord::context::create_context_key("durable.test.close_probe")
    });

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn js(value: &JsonValue) -> String {
    kiana_json::js_object::stringify(value)
}

fn scope_key(scope: &DocumentScope) -> String {
    match scope {
        DocumentScope::Session => js(&json!(["session"])),
        DocumentScope::Conversation { conversation_id } => {
            js(&json!(["conversation", conversation_id.get()]))
        }
        DocumentScope::Task { task_id } => js(&json!(["task", task_id.get()])),
    }
}

fn address_key(address: &DocumentAddress) -> String {
    js(&json!([
        address.kind,
        scope_key(&address.scope),
        match &address.key {
            None => json!(["singleton"]),
            Some(key) => json!(["family", key]),
        }
    ]))
}

fn record_address_key(record: &DocumentCreate) -> String {
    address_key(&DocumentAddress {
        kind: record.kind.clone(),
        scope: record.scope,
        key: record.key.clone(),
    })
}

fn record_record_key(record: &DocumentRecord) -> String {
    address_key(&DocumentAddress {
        kind: record.kind.clone(),
        scope: record.scope,
        key: record.key.clone(),
    })
}

fn is_alive_at(record: &DocumentRecord, at: DocumentPoint) -> bool {
    match at {
        DocumentPoint::Current => record.retired_at.is_none(),
        DocumentPoint::At(seq) => {
            record.created_at <= seq && record.retired_at.is_none_or(|retired| seq < retired)
        }
    }
}

fn is_current_only(record: &DocumentRecord) -> bool {
    !matches!(record.scope, DocumentScope::Conversation { .. })
        || record.history == Some(DocumentHistory::Latest)
}

fn lower_bound(ids: &[i64], target: i64) -> usize {
    let mut low = 0;
    let mut high = ids.len();
    while low < high {
        let middle = (low + high) / 2;
        if ids[middle] < target {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

fn upper_bound(ids: &[i64], target: i64) -> usize {
    let mut low = 0;
    let mut high = ids.len();
    while low < high {
        let middle = (low + high) / 2;
        if ids[middle] <= target {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

fn insert_sorted(ids: &mut Vec<i64>, id: i64) {
    if ids.last().is_none_or(|last| *last < id) {
        ids.push(id);
    } else {
        let at = lower_bound(ids, id);
        ids.insert(at, id);
    }
}

fn remove_sorted(ids: &mut Vec<i64>, id: i64) {
    let at = lower_bound(ids, id);
    if ids.get(at) == Some(&id) {
        ids.remove(at);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TableName {
    Conversation,
    Entry,
    Task,
    Submission,
    Document,
}

#[derive(Debug, Clone)]
struct DocumentRevision {
    seq: Seq,
    content: DocumentContent,
}

#[derive(Debug, Clone)]
struct StoredDocumentState {
    record: DocumentRecord,
    revisions: Vec<DocumentRevision>,
}

#[derive(Debug, Default)]
struct DocumentAddressIndex {
    ids: Vec<i64>,
    current_id: Option<i64>,
}

#[derive(Debug, Default)]
struct State {
    record_types: HashMap<i64, TableName>,
    conversations: HashMap<i64, ConversationRecord>,
    conversation_ids: Vec<i64>,
    conversation_ids_by_owner_conversation: HashMap<i64, Vec<i64>>,
    conversation_ids_by_owner_task: HashMap<i64, Vec<i64>>,
    entries: HashMap<i64, EntryRecord>,
    entry_commit_seqs: HashMap<i64, Seq>,
    entry_ids: HashMap<i64, Vec<i64>>,
    head_entry_ids: HashMap<i64, Vec<i64>>,
    tasks: HashMap<i64, TaskRecord>,
    task_ids: Vec<i64>,
    submissions: HashMap<i64, SubmissionRecord>,
    submission_ids: Vec<i64>,
    submission_ids_by_request: HashMap<(i64, String), i64>,
    documents: HashMap<i64, StoredDocumentState>,
    document_addresses: HashMap<String, DocumentAddressIndex>,
    document_ids_by_scope: HashMap<String, Vec<i64>>,
    next_id: i64,
    next_seq: i64,
}

impl State {
    fn new() -> Self {
        State {
            next_id: 2,
            next_seq: 1,
            ..Default::default()
        }
    }

    fn materialize(
        &self,
        id: DocumentId,
        at: DocumentPoint,
    ) -> Result<Option<StoredDocument>, BoxError> {
        let Some(stored) = self.documents.get(&id.get()) else {
            return Ok(None);
        };
        if at != DocumentPoint::Current && is_current_only(&stored.record) {
            return Err(StorageRequestError::new(format!(
                "Document {id} does not retain historical content"
            ))
            .into());
        }
        if !is_alive_at(&stored.record, at) {
            return Ok(None);
        }
        let revisions: Vec<&DocumentRevision> = match at {
            DocumentPoint::Current => stored.revisions.iter().collect(),
            DocumentPoint::At(seq) => stored
                .revisions
                .iter()
                .filter(|revision| revision.seq <= seq)
                .collect(),
        };
        let mut base_index = revisions.len().saturating_sub(1);
        let mut base = None;
        while base_index < revisions.len() {
            if revisions[base_index].content.kind == DocumentContentKind::Base {
                base = Some(revisions[base_index]);
                break;
            }
            base_index = base_index.wrapping_sub(1);
            if base_index >= revisions.len() {
                break;
            }
        }
        let Some(base) = base else {
            return Err(
                std::io::Error::other(format!("Document {id} is missing a required base")).into(),
            );
        };
        let version = base.content.version;
        let mut batches: Vec<Vec<Op>> = Vec::new();
        for revision in revisions.iter().skip(base_index + 1) {
            if revision.content.kind != DocumentContentKind::Delta
                || revision.content.version != version
            {
                return Err(std::io::Error::other(format!(
                    "Document {id} crosses a stored version boundary without a base"
                ))
                .into());
            }
            batches.push(
                revision
                    .content
                    .ops
                    .as_ref()
                    .map(|ops| ops.0.clone())
                    .unwrap_or_default(),
            );
        }
        let root = JsonValue::Object(
            base.content
                .value
                .clone()
                .unwrap_or_default()
                .into_iter()
                .collect(),
        );
        let value =
            apply_immutable_batches(root, batches).map_err(|error| Box::new(error) as BoxError)?;
        let JsonValue::Object(value) = value else {
            return Err(std::io::Error::other("document value is not an object").into());
        };
        Ok(Some(StoredDocument {
            record: stored.record.clone(),
            version,
            value: value.into_iter().collect(),
            deltas_since_base: (revisions.len() - base_index - 1) as u64,
        }))
    }

    fn visible_entries(
        &self,
        conversation_id: ConversationId,
        min_entry_id: i64,
        max_entry_id: i64,
    ) -> Result<Vec<EntryRecord>, BoxError> {
        if !self.conversations.contains_key(&conversation_id.get()) {
            return Err(StorageRequestError::new(format!(
                "Unknown conversation: {conversation_id}"
            ))
            .into());
        }
        let mut entries = Vec::new();
        let mut current_id = conversation_id.get();
        let mut upper = max_entry_id;
        loop {
            if let Some(ids) = self.entry_ids.get(&current_id) {
                let mut index = upper_bound(ids, upper) as i64 - 1;
                while index >= 0 {
                    let id = ids[index as usize];
                    if id < min_entry_id {
                        break;
                    }
                    entries.push(
                        self.entries
                            .get(&id)
                            .expect("entry index is complete")
                            .clone(),
                    );
                    index -= 1;
                }
            }
            let Some(conversation) = self.conversations.get(&current_id) else {
                break;
            };
            let Some(parent) = &conversation.parent else {
                break;
            };
            upper = upper.min(parent.at.get());
            current_id = parent.conversation_id.get();
        }
        Ok(entries)
    }

    fn visible_entries_ascending(
        &self,
        conversation_id: ConversationId,
        min_entry_id: i64,
        max_entry_id: i64,
    ) -> Result<Vec<EntryRecord>, BoxError> {
        if !self.conversations.contains_key(&conversation_id.get()) {
            return Err(StorageRequestError::new(format!(
                "Unknown conversation: {conversation_id}"
            ))
            .into());
        }
        let mut segments: Vec<(i64, i64)> = Vec::new();
        let mut current_id = conversation_id.get();
        let mut upper = max_entry_id;
        loop {
            segments.push((current_id, upper));
            let Some(conversation) = self.conversations.get(&current_id) else {
                break;
            };
            let Some(parent) = &conversation.parent else {
                break;
            };
            upper = upper.min(parent.at.get());
            if upper < min_entry_id {
                break;
            }
            current_id = parent.conversation_id.get();
        }
        let mut entries = Vec::new();
        for (current, upper) in segments.into_iter().rev() {
            if let Some(ids) = self.entry_ids.get(&current) {
                for id in ids.iter().skip(lower_bound(ids, min_entry_id)) {
                    if *id > upper {
                        break;
                    }
                    entries.push(
                        self.entries
                            .get(id)
                            .expect("entry index is complete")
                            .clone(),
                    );
                }
            }
        }
        Ok(entries)
    }
}

fn document_actions(
    state: &State,
    writes: &[StorageWrite],
) -> Result<IndexMap<i64, DocumentAction>, BoxError> {
    let mut actions: IndexMap<i64, DocumentAction> = IndexMap::new();
    for write in writes {
        let id = match write {
            StorageWrite::DocumentCreate { record, .. } => record.id.get(),
            StorageWrite::DocumentCopy { record, .. } => record.id.get(),
            StorageWrite::DocumentChange { id, .. } | StorageWrite::DocumentRetire { id } => {
                id.get()
            }
            _ => continue,
        };
        let action = actions.entry(id).or_default();
        match write {
            StorageWrite::DocumentCreate { record, content } => {
                if action.create.is_some() || action.content.is_some() {
                    return Err(std::io::Error::other(format!(
                        "Document {id} has more than one content command"
                    ))
                    .into());
                }
                action.create = Some(record.clone());
                action.content = Some(DocumentContent::base(
                    content.version,
                    content.value.clone(),
                ));
            }
            StorageWrite::DocumentCopy { .. } => unreachable!("copies are resolved first"),
            StorageWrite::DocumentChange { content, .. } => {
                if action.content.is_some() {
                    return Err(std::io::Error::other(format!(
                        "Document {id} has more than one content command"
                    ))
                    .into());
                }
                action.content = Some(content.clone());
            }
            StorageWrite::DocumentRetire { .. } => {
                if action.retire {
                    return Err(std::io::Error::other(format!(
                        "Document {id} is retired more than once"
                    ))
                    .into());
                }
                action.retire = true;
            }
            _ => {}
        }
    }
    let mut live_counts: HashMap<String, i64> = HashMap::new();
    for (id, action) in &actions {
        let existing = state.documents.get(id);
        if action.create.is_none() && existing.is_none() {
            return Err(std::io::Error::other(format!("Unknown document: {id}")).into());
        }
        if action.create.is_some() && existing.is_some() {
            return Err(std::io::Error::other(format!("Document {id} already exists")).into());
        }
        if existing.is_some_and(|stored| stored.record.retired_at.is_some()) {
            return Err(std::io::Error::other(format!("Document {id} is retired")).into());
        }
        if let Some(content) = &action.content
            && content.kind == DocumentContentKind::Delta
        {
            let Some(previous) = existing.and_then(|stored| stored.revisions.last()) else {
                return Err(
                    std::io::Error::other(format!("Document {id} delta has no base")).into(),
                );
            };
            if previous.content.version != content.version {
                return Err(std::io::Error::other(format!(
                    "Document {id} version transition requires a base"
                ))
                .into());
            }
        }
        let key = match &action.create {
            Some(create) => record_address_key(create),
            None => record_record_key(&existing.expect("checked above").record),
        };
        let current_id = state
            .document_addresses
            .get(&key)
            .and_then(|index| index.current_id);
        let mut live = match live_counts.get(&key) {
            Some(live) => *live,
            None => i64::from(current_id.is_some()),
        };
        if action.retire && current_id == Some(*id) {
            live -= 1;
        }
        if action.create.is_some() && !action.retire {
            live += 1;
        }
        live_counts.insert(key, live);
    }
    for live in live_counts.values() {
        if *live > 1 {
            return Err(std::io::Error::other(
                "Document address already has a current incarnation",
            )
            .into());
        }
    }
    Ok(actions)
}

#[derive(Default)]
struct DocumentAction {
    create: Option<DocumentCreate>,
    content: Option<DocumentContent>,
    retire: bool,
}

fn apply_document_actions(state: &mut State, actions: &IndexMap<i64, DocumentAction>, seq: Seq) {
    for (id, action) in actions {
        let id = *id;
        if let Some(create) = &action.create {
            let record = DocumentRecord {
                id: create.id,
                kind: create.kind.clone(),
                key: create.key.clone(),
                scope: create.scope,
                history: create.history,
                fork: create.fork,
                created_at: seq,
                retired_at: action.retire.then_some(seq),
            };
            let revision = DocumentRevision {
                seq,
                content: action.content.clone().expect("creation has content"),
            };
            state.documents.insert(
                id,
                StoredDocumentState {
                    record: record.clone(),
                    revisions: vec![revision],
                },
            );
            state.record_types.insert(id, TableName::Document);
            let key = record_address_key(create);
            let address = state.document_addresses.entry(key).or_default();
            insert_sorted(&mut address.ids, id);
            let scope = scope_key(&record.scope);
            insert_sorted(state.document_ids_by_scope.entry(scope).or_default(), id);
            state.next_id = state.next_id.max(id + 1);
        } else if let Some(content) = &action.content {
            let revision = DocumentRevision {
                seq,
                content: content.clone(),
            };
            let stored = state.documents.get_mut(&id).expect("validated");
            if content.kind == DocumentContentKind::Base && is_current_only(&stored.record) {
                stored.revisions = vec![revision];
            } else {
                stored.revisions.push(revision);
            }
        }
        if action.retire && action.create.is_none() {
            let stored = state.documents.get_mut(&id).expect("validated");
            stored.record.retired_at = Some(seq);
        }
        if action.retire || action.create.is_some() {
            let stored = state.documents.get(&id).expect("validated");
            let key = record_record_key(&stored.record);
            if action.retire && is_current_only(&stored.record) {
                state
                    .documents
                    .get_mut(&id)
                    .expect("validated")
                    .revisions
                    .clear();
            }
            let address = state
                .document_addresses
                .get_mut(&key)
                .expect("address indexed");
            if action.retire && address.current_id == Some(id) {
                address.current_id = None;
            }
            if action.create.is_some() && !action.retire {
                address.current_id = Some(id);
            }
        }
    }
}

fn resolve_document_copies(
    state: &State,
    writes: &[StorageWrite],
) -> Result<Vec<StorageWrite>, BoxError> {
    if !writes
        .iter()
        .any(|write| matches!(write, StorageWrite::DocumentCopy { .. }))
    {
        return Ok(writes.to_vec());
    }
    let mut changed = std::collections::HashSet::new();
    for write in writes {
        match write {
            StorageWrite::DocumentCreate { record, .. }
            | StorageWrite::DocumentCopy { record, .. } => {
                changed.insert(record.id.get());
            }
            StorageWrite::DocumentChange { id, .. } | StorageWrite::DocumentRetire { id } => {
                changed.insert(id.get());
            }
            _ => {}
        }
    }
    let mut resolved = Vec::with_capacity(writes.len());
    for write in writes {
        let StorageWrite::DocumentCopy { record, source } = write else {
            resolved.push(write.clone());
            continue;
        };
        let fail = |cause: BoxError| -> BoxError {
            std::io::Error::other(format!("Document copy {} was rejected: {cause}", record.id))
                .into()
        };
        if changed.contains(&source.id.get()) {
            return Err(fail(
                std::io::Error::other(format!(
                    "Fork source document {} is changed in the copy batch",
                    source.id
                ))
                .into(),
            ));
        }
        let Some(stored) = state.materialize(source.id, source.at).map_err(fail)? else {
            return Err(fail(
                std::io::Error::other(format!("Fork source document {} cannot be read", source.id))
                    .into(),
            ));
        };
        let matches = matches!(stored.record.scope, DocumentScope::Conversation { .. })
            && matches!(record.scope, DocumentScope::Conversation { .. })
            && stored.record.kind == record.kind
            && stored.record.key == record.key
            && stored.record.history == record.history
            && stored.record.fork == record.fork;
        if !matches {
            return Err(fail(
                std::io::Error::other(format!(
                    "Fork source document {} does not match the copied record",
                    source.id
                ))
                .into(),
            ));
        }
        resolved.push(StorageWrite::DocumentCreate {
            record: record.clone(),
            content: DocumentBaseContent::new(stored.version, stored.value),
        });
    }
    Ok(resolved)
}

fn apply_prepared(state: &mut State, writes: &[StorageWrite], seq: Seq) -> Result<(), BoxError> {
    for write in writes {
        match write {
            StorageWrite::Conversation { value } => {
                state
                    .record_types
                    .insert(value.id.get(), TableName::Conversation);
                state.conversations.insert(value.id.get(), value.clone());
                insert_sorted(&mut state.conversation_ids, value.id.get());
                if let Some(owner) = &value.owner {
                    insert_sorted(
                        state
                            .conversation_ids_by_owner_conversation
                            .entry(owner.conversation_id.get())
                            .or_default(),
                        value.id.get(),
                    );
                    insert_sorted(
                        state
                            .conversation_ids_by_owner_task
                            .entry(owner.task_id.get())
                            .or_default(),
                        value.id.get(),
                    );
                }
                state.next_id = state.next_id.max(value.id.get() + 1);
            }
            StorageWrite::Entry { value } => {
                state.record_types.insert(value.id.get(), TableName::Entry);
                state.entries.insert(value.id.get(), value.clone());
                state.entry_commit_seqs.insert(value.id.get(), seq);
                insert_sorted(
                    state
                        .entry_ids
                        .entry(value.conversation_id.get())
                        .or_default(),
                    value.id.get(),
                );
                if value.head.is_some() {
                    insert_sorted(
                        state
                            .head_entry_ids
                            .entry(value.conversation_id.get())
                            .or_default(),
                        value.id.get(),
                    );
                }
                state.next_id = state.next_id.max(value.id.get() + 1);
            }
            StorageWrite::Task { value } => {
                state.record_types.insert(value.id.get(), TableName::Task);
                if !state.tasks.contains_key(&value.id.get()) {
                    insert_sorted(&mut state.task_ids, value.id.get());
                }
                state.tasks.insert(value.id.get(), value.clone());
                state.next_id = state.next_id.max(value.id.get() + 1);
            }
            StorageWrite::Submission { value } => {
                state
                    .record_types
                    .insert(value.id.get(), TableName::Submission);
                if let Some(previous) = state.submissions.get(&value.id.get())
                    && let Some(request_id) = &previous.request_id
                    && state
                        .submission_ids_by_request
                        .get(&(previous.conversation_id.get(), request_id.clone()))
                        == Some(&value.id.get())
                {
                    state
                        .submission_ids_by_request
                        .remove(&(previous.conversation_id.get(), request_id.clone()));
                }
                if !state.submissions.contains_key(&value.id.get()) {
                    insert_sorted(&mut state.submission_ids, value.id.get());
                }
                state.submissions.insert(value.id.get(), value.clone());
                if let Some(request_id) = &value.request_id {
                    state.submission_ids_by_request.insert(
                        (value.conversation_id.get(), request_id.clone()),
                        value.id.get(),
                    );
                }
                state.next_id = state.next_id.max(value.id.get() + 1);
            }
            StorageWrite::DocumentCreate { .. }
            | StorageWrite::DocumentCopy { .. }
            | StorageWrite::DocumentChange { .. }
            | StorageWrite::DocumentRetire { .. } => {}
        }
    }
    Ok(())
}

/// `storage/memory.ts` 的内存等价物（测试替身）。
pub struct MemoryStorage {
    state: Mutex<State>,
    closed: AtomicBool,
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStorage {
    pub fn new() -> Self {
        MemoryStorage {
            state: Mutex::new(State::new()),
            closed: AtomicBool::new(false),
        }
    }

    pub fn reopen(&self) {
        self.closed.store(false, Ordering::SeqCst);
    }

    fn assert_open(&self) -> Result<(), BoxError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("MemoryStorage is closed").into());
        }
        Ok(())
    }

    fn materialize(
        &self,
        id: DocumentId,
        at: DocumentPoint,
    ) -> Result<Option<StoredDocument>, BoxError> {
        lock(&self.state).materialize(id, at)
    }

    pub fn commit_inner(
        &self,
        writes: &[StorageWrite],
        adopt: &(dyn Fn(Seq) + Send + Sync),
    ) -> Result<Seq, BoxError> {
        self.assert_open()?;
        let mut state = lock(&self.state);
        let seq = seq_from_number(state.next_seq);
        let resolved = resolve_document_copies(&state, writes)?;
        let actions = document_actions(&state, &resolved)?;
        apply_prepared(&mut state, &resolved, seq)?;
        apply_document_actions(&mut state, &actions, seq);
        state.next_seq += 1;
        drop(state);
        adopt(seq);
        Ok(seq)
    }
}

#[async_trait]
impl Storage for MemoryStorage {
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: kiana_durable::AdoptFn,
        _context: &dyn Context,
    ) -> Result<Seq, BoxError> {
        self.commit_inner(writes, &|seq| adopt(seq))
    }

    async fn mint_id(&self, _kind: IdKind) -> Result<i64, BoxError> {
        self.assert_open()?;
        let mut state = lock(&self.state);
        let id = state.next_id;
        state.next_id += 1;
        Ok(id)
    }

    async fn conversation(
        &self,
        id: ConversationId,
        _context: &dyn Context,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        self.assert_open()?;
        Ok(lock(&self.state).conversations.get(&id.get()).cloned())
    }

    async fn scan_conversations(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        self.assert_open()?;
        let state = lock(&self.state);
        let start: ScanStart = scan_start(query.order, cursor, ScanOrder::Ascending)?;
        let ids: Vec<i64> = match (query.owner_task_id, query.owner_conversation_id) {
            (Some(task_id), _) => state
                .conversation_ids_by_owner_task
                .get(&task_id.get())
                .cloned()
                .unwrap_or_default(),
            (None, Some(conversation_id)) => state
                .conversation_ids_by_owner_conversation
                .get(&conversation_id.get())
                .cloned()
                .unwrap_or_default(),
            (None, None) => state.conversation_ids.clone(),
        };
        let mut values = Vec::new();
        for index in scan_indexes(&ids, start) {
            if values.len() > limit {
                break;
            }
            let value = state
                .conversations
                .get(&ids[index])
                .expect("indexed")
                .clone();
            if let Some(owner_conversation_id) = query.owner_conversation_id
                && value.owner.map(|owner| owner.conversation_id) != Some(owner_conversation_id)
            {
                continue;
            }
            values.push(value);
        }
        Ok(page(values, limit, start.order))
    }

    async fn entry(
        &self,
        id: EntryId,
        _context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.assert_open()?;
        let state = lock(&self.state);
        let Some(entry) = state.entries.get(&id.get()) else {
            return Ok(None);
        };
        Ok(Some(StoredEntry {
            entry: entry.clone(),
            commit_seq: *state.entry_commit_seqs.get(&id.get()).expect("indexed"),
        }))
    }

    async fn visible_entry(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        _context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.assert_open()?;
        let state = lock(&self.state);
        let entries = state.visible_entries(conversation_id, id.get(), id.get())?;
        let Some(entry) = entries.into_iter().next() else {
            return Ok(None);
        };
        Ok(Some(StoredEntry {
            entry,
            commit_seq: *state
                .entry_commit_seqs
                .get(&id.get())
                .expect("visible entry is indexed"),
        }))
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        _context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError> {
        self.assert_open()?;
        let state = lock(&self.state);
        if !state.conversations.contains_key(&conversation_id.get()) {
            return Err(StorageRequestError::new(format!(
                "Unknown conversation: {conversation_id}"
            ))
            .into());
        }
        let mut current_id = conversation_id.get();
        let mut upper = at_or_before_entry_id.map(|id| id.get()).unwrap_or(i64::MAX);
        loop {
            let ids = state
                .head_entry_ids
                .get(&current_id)
                .cloned()
                .unwrap_or_default();
            let index = upper_bound(&ids, upper) as i64 - 1;
            if index >= 0 {
                let entry = state
                    .entries
                    .get(&ids[index as usize])
                    .expect("head index is complete")
                    .clone();
                return Ok(Some(entry));
            }
            let conversation = state.conversations.get(&current_id).expect("parent exists");
            let Some(parent) = &conversation.parent else {
                return Ok(None);
            };
            upper = upper.min(parent.at.get());
            current_id = parent.conversation_id.get();
        }
    }

    async fn scan_entries(
        &self,
        query: &EntryQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<EntryRecord>, BoxError> {
        self.assert_open()?;
        let state = lock(&self.state);
        let start: ScanStart = scan_start(query.order, cursor, ScanOrder::Descending)?;
        let mut min_entry_id = query.min_entry_id.map(|id| id.get()).unwrap_or(i64::MIN);
        let mut max_entry_id = query.max_entry_id.map(|id| id.get()).unwrap_or(i64::MAX);
        if let Some(after) = start.after {
            if start.order == ScanOrder::Descending {
                max_entry_id = max_entry_id.min(after - 1);
            } else {
                min_entry_id = min_entry_id.max(after + 1);
            }
        }
        let entries = if start.order == ScanOrder::Descending {
            state.visible_entries(query.conversation_id, min_entry_id, max_entry_id)?
        } else {
            state.visible_entries_ascending(query.conversation_id, min_entry_id, max_entry_id)?
        };
        let mut values = Vec::new();
        for entry in entries {
            values.push(entry);
            if values.len() > limit {
                break;
            }
        }
        Ok(page(values, limit, start.order))
    }

    async fn task(
        &self,
        id: TaskId,
        _context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        self.assert_open()?;
        Ok(lock(&self.state).tasks.get(&id.get()).cloned())
    }

    async fn scan_tasks(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<TaskRecord>, BoxError> {
        self.assert_open()?;
        let state = lock(&self.state);
        let start: ScanStart = scan_start(query.order, cursor, ScanOrder::Ascending)?;
        let mut values = Vec::new();
        for index in scan_indexes(&state.task_ids, start) {
            if values.len() > limit {
                break;
            }
            let value = state
                .tasks
                .get(&state.task_ids[index])
                .expect("indexed")
                .clone();
            if let Some(conversation_id) = query.conversation_id
                && value.conversation_id != conversation_id
            {
                continue;
            }
            if let Some(kind) = &query.kind
                && &value.kind != kind
            {
                continue;
            }
            if let Some(status) = query.status
                && value.state.status() != status
            {
                continue;
            }
            if let Some(abort_requested) = query.abort_requested
                && value.abort_requested != abort_requested
            {
                continue;
            }
            if let Some(background) = query.background
                && value.background != background
            {
                continue;
            }
            values.push(value);
        }
        Ok(page(values, limit, start.order))
    }

    async fn submission(
        &self,
        id: SubmissionId,
        _context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.assert_open()?;
        Ok(lock(&self.state).submissions.get(&id.get()).cloned())
    }

    async fn scan_submissions(
        &self,
        query: &SubmissionQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<SubmissionRecord>, BoxError> {
        self.assert_open()?;
        let state = lock(&self.state);
        let start: ScanStart = scan_start(query.order, cursor, ScanOrder::Ascending)?;
        let mut values = Vec::new();
        for index in scan_indexes(&state.submission_ids, start) {
            if values.len() > limit {
                break;
            }
            let value = state
                .submissions
                .get(&state.submission_ids[index])
                .expect("indexed")
                .clone();
            if let Some(conversation_id) = query.conversation_id
                && value.conversation_id != conversation_id
            {
                continue;
            }
            if let Some(status) = query.status
                && value.status != status
            {
                continue;
            }
            values.push(value);
        }
        Ok(page(values, limit, start.order))
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        _context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.assert_open()?;
        let state = lock(&self.state);
        let id = state
            .submission_ids_by_request
            .get(&(conversation_id.get(), request_id.to_owned()));
        Ok(id.and_then(|id| state.submissions.get(id).cloned()))
    }

    async fn find_document(
        &self,
        address: &DocumentAddress,
        at: DocumentPoint,
        _context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError> {
        self.assert_open()?;
        let state = lock(&self.state);
        let index = state.document_addresses.get(&address_key(address));
        match at {
            DocumentPoint::Current => Ok(index.and_then(|index| {
                index
                    .current_id
                    .and_then(|id| state.documents.get(&id).map(|stored| stored.record.clone()))
            })),
            DocumentPoint::At(_) => {
                let Some(index) = index else {
                    return Ok(None);
                };
                for id in &index.ids {
                    let record = &state.documents.get(id).expect("indexed").record;
                    if is_alive_at(record, at) {
                        return Ok(Some(record.clone()));
                    }
                }
                Ok(None)
            }
        }
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        _context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError> {
        self.assert_open()?;
        let state = lock(&self.state);
        state.materialize(id, at)
    }

    async fn scan_documents(
        &self,
        query: &DocumentQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<DocumentRecord>, BoxError> {
        self.assert_open()?;
        let state = lock(&self.state);
        let ids = state
            .document_ids_by_scope
            .get(&scope_key(&query.scope))
            .cloned()
            .unwrap_or_default();
        let after = cursor
            .and_then(|cursor| cursor.get("after"))
            .and_then(JsonValue::as_i64);
        let start = match after {
            Some(after) => upper_bound(&ids, after),
            None => 0,
        };
        let mut values = Vec::new();
        for id in ids.iter().skip(start) {
            if values.len() > limit {
                break;
            }
            let record = state.documents.get(id).expect("indexed");
            if let Some(kind) = &query.kind
                && &record.record.kind != kind
            {
                continue;
            }
            if is_alive_at(&record.record, query.at) {
                values.push(record.record.clone());
            }
        }
        Ok(page(values, limit, ScanOrder::Ascending))
    }

    async fn close(&self, _context: &dyn Context) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

fn scan_indexes(ids: &[i64], start: ScanStart) -> Vec<usize> {
    match start.order {
        ScanOrder::Ascending => {
            let first = start
                .after
                .map(|after| upper_bound(ids, after))
                .unwrap_or(0);
            (first..ids.len()).collect()
        }
        ScanOrder::Descending => {
            let end = start
                .after
                .map(|after| lower_bound(ids, after))
                .unwrap_or(ids.len());
            (0..end).rev().collect()
        }
    }
}

fn page<T: Clone + HasId>(values: Vec<T>, limit: usize, order: ScanOrder) -> Page<T> {
    if values.len() <= limit {
        return Page {
            items: values,
            next: None,
        };
    }
    let items: Vec<T> = values[..limit].to_vec();
    let last = items.last().expect("more than the limit").id();
    Page {
        items,
        next: Some(next_cursor(last, order)),
    }
}

pub trait HasId {
    fn id(&self) -> i64;
}

impl HasId for ConversationRecord {
    fn id(&self) -> i64 {
        self.id.get()
    }
}
impl HasId for EntryRecord {
    fn id(&self) -> i64 {
        self.id.get()
    }
}
impl HasId for TaskRecord {
    fn id(&self) -> i64 {
        self.id.get()
    }
}
impl HasId for SubmissionRecord {
    fn id(&self) -> i64 {
        self.id.get()
    }
}
impl HasId for DocumentRecord {
    fn id(&self) -> i64 {
        self.id.get()
    }
}

// ---------------------------------------------------------------------------
// ControlledStorage
// ---------------------------------------------------------------------------

/// 一个可等待、可释放的闸门（TS: `Gate`）。
#[derive(Clone)]
pub struct Gate {
    entered: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Notify>,
    released: Arc<AtomicBool>,
}

impl Gate {
    fn new() -> Self {
        Gate {
            entered: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new(tokio::sync::Notify::new()),
            released: Arc::new(AtomicBool::new(false)),
        }
    }

    fn signal_entered(&self) {
        self.entered.add_permits(1);
    }

    /// 等待第一个被扣住的调用到达。
    pub async fn entered(&self) {
        let _ = self.entered.acquire().await;
    }

    /// 释放被扣住的调用。
    pub fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.release.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            if self.released.load(Ordering::SeqCst) {
                return;
            }
            let notified = self.release.notified();
            if self.released.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

struct HeldGate {
    gate: Gate,
}

/// 一次提交在持久化之后的故障注入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitFault {
    /// Storage 在采纳之后拒绝（模拟回收失败）。
    RejectAfterAdopt,
    /// Storage 提交但不调用采纳。
    SkipAdopt,
}

/// MemoryStorage 加可观察提交、扣住的提交/查找与注入失败（TS: `ControlledStorage`）。
pub struct ControlledStorage {
    inner: MemoryStorage,
    /// Session 收到的原始批次。
    pub admitted_commits: Mutex<Vec<Vec<StorageWrite>>>,
    /// 为值断言分离的批次。
    pub commits: Mutex<Vec<Vec<StorageWrite>>>,
    pub mint_count: AtomicUsize,
    pub document_read_count: AtomicUsize,
    /// `task()` 的后端读取次数（重复读取的 memoization 断言用）。
    pub task_read_count: AtomicUsize,
    /// `close()` 看到的上下文探针（值 + 是否还有 abort signal）。
    pub close_probe: Mutex<Option<(String, bool)>>,
    commit_gate: Mutex<Option<HeldGate>>,
    find_gate: Mutex<Option<HeldGate>>,
    commit_failure: Mutex<Option<BoxError>>,
    /// 下一次提交的故障注入。
    pub fault: Mutex<Option<CommitFault>>,
    /// 采纳/落定顺序（`adopt`、`adopted`、`resolve`）。
    pub order: Mutex<Vec<&'static str>>,
}

impl Default for ControlledStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlledStorage {
    pub fn new() -> Self {
        ControlledStorage {
            inner: MemoryStorage::new(),
            admitted_commits: Mutex::new(Vec::new()),
            commits: Mutex::new(Vec::new()),
            mint_count: AtomicUsize::new(0),
            document_read_count: AtomicUsize::new(0),
            task_read_count: AtomicUsize::new(0),
            close_probe: Mutex::new(None),
            commit_gate: Mutex::new(None),
            find_gate: Mutex::new(None),
            commit_failure: Mutex::new(None),
            fault: Mutex::new(None),
            order: Mutex::new(Vec::new()),
        }
    }

    pub fn hold_commits(&self) -> Gate {
        let gate = Gate::new();
        *lock(&self.commit_gate) = Some(HeldGate { gate: gate.clone() });
        gate
    }

    pub fn hold_find_document(&self) -> Gate {
        let gate = Gate::new();
        *lock(&self.find_gate) = Some(HeldGate { gate: gate.clone() });
        gate
    }

    /// 模拟扣住的提交期间崩溃：它永远不到达 Storage，之后的提交继续。
    pub fn crash(&self) {
        *lock(&self.commit_gate) = None;
    }

    pub fn fail_next_commit(&self, error: BoxError) {
        *lock(&self.commit_failure) = Some(error);
    }

    pub fn reopen(&self) {
        self.inner.reopen();
    }

    pub fn inner(&self) -> &MemoryStorage {
        &self.inner
    }

    /// 已到达 Storage 的提交批次数。
    pub fn commits_len(&self) -> usize {
        lock(&self.commits).len()
    }
}

#[async_trait]
impl Storage for ControlledStorage {
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: kiana_durable::AdoptFn,
        _context: &dyn Context,
    ) -> Result<Seq, BoxError> {
        lock(&self.admitted_commits).push(writes.to_vec());
        lock(&self.commits).push(writes.to_vec());
        let gate = lock(&self.commit_gate)
            .as_ref()
            .map(|held| held.gate.clone());
        if let Some(gate) = gate {
            gate.signal_entered();
            gate.wait().await;
        }
        if let Some(failure) = lock(&self.commit_failure).take() {
            return Err(failure);
        }
        let fault = lock(&self.fault).take();
        let order = &self.order;
        let record = |seq| {
            lock(order).push("adopt");
            adopt(seq);
            lock(order).push("adopted");
        };
        let seq = match fault {
            Some(CommitFault::SkipAdopt) => self.inner.commit_inner(writes, &|_seq| {})?,
            Some(CommitFault::RejectAfterAdopt) => {
                self.inner.commit_inner(writes, &record)?;
                lock(order).push("resolve");
                return Err(std::io::Error::other("reclaim failed").into());
            }
            None => self.inner.commit_inner(writes, &record)?,
        };
        lock(order).push("resolve");
        Ok(seq)
    }

    async fn mint_id(&self, kind: IdKind) -> Result<i64, BoxError> {
        self.mint_count.fetch_add(1, Ordering::SeqCst);
        self.inner.mint_id(kind).await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        self.inner.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        self.inner
            .scan_conversations(query, limit, cursor, context)
            .await
    }

    async fn entry(
        &self,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.inner.entry(id, context).await
    }

    async fn visible_entry(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.inner.visible_entry(conversation_id, id, context).await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError> {
        self.inner
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
        self.inner.scan_entries(query, limit, cursor, context).await
    }

    async fn task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        self.task_read_count.fetch_add(1, Ordering::SeqCst);
        self.inner.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<TaskRecord>, BoxError> {
        self.inner.scan_tasks(query, limit, cursor, context).await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.inner.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: &SubmissionQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<SubmissionRecord>, BoxError> {
        self.inner
            .scan_submissions(query, limit, cursor, context)
            .await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.inner
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: &DocumentAddress,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError> {
        let gate = lock(&self.find_gate).as_ref().map(|held| held.gate.clone());
        if let Some(gate) = gate {
            gate.signal_entered();
            gate.wait().await;
        }
        self.inner.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError> {
        self.document_read_count.fetch_add(1, Ordering::SeqCst);
        self.inner.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: &DocumentQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<DocumentRecord>, BoxError> {
        self.inner
            .scan_documents(query, limit, cursor, context)
            .await
    }

    async fn close(&self, context: &dyn Context) {
        if let Some(value) = CLOSE_PROBE_KEY.get(context) {
            *lock(&self.close_probe) = Some((value, context.abort_signal().is_some()));
        }
        self.inner.close(context).await;
    }
}

// ---------------------------------------------------------------------------
// Session helpers
// ---------------------------------------------------------------------------

/// `openTestSession()`。
pub fn open_test_session() -> (
    Arc<ControlledStorage>,
    SessionImpl,
    Arc<Mutex<Vec<kiana_durable::CommitPublication>>>,
) {
    let storage = Arc::new(ControlledStorage::new());
    let session = SessionImpl::new(
        storage.clone(),
        SessionOptions {
            document_retention_ms: Some(600_000.0),
            ..Default::default()
        },
    );
    let publications = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&publications);
    let _keep = session
        .subscribe_commits(Arc::new(move |publication, _context| {
            lock(&sink).push(publication.clone());
        }))
        .expect("a fresh Session accepts listeners");
    (storage, session, publications)
}

/// TS `documentChanges()`：发布里的文档变化。
pub fn document_changes(
    publication: &kiana_durable::CommitPublication,
) -> Vec<DocumentCommitChange> {
    publication
        .changes
        .iter()
        .filter_map(|change| match change {
            kiana_durable::CommitChange::Document(document) => Some(document.clone()),
            kiana_durable::CommitChange::Table(_) => None,
        })
        .collect()
}

/// TS `documentCopyChanges()`。
pub fn document_copy_changes(
    publication: &kiana_durable::CommitPublication,
) -> Vec<DocumentCommitChange> {
    document_changes(publication)
        .into_iter()
        .filter(|change| matches!(change, DocumentCommitChange::DocumentCopy { .. }))
        .collect()
}

/// 在 Session 上运行一个使用具体事务的提交。
pub async fn commit<T, F>(session: &SessionImpl, change: F) -> Result<T, BoxError>
where
    T: Send + 'static,
    F: for<'a> FnOnce(
            &'a kiana_durable::session::Transaction,
        ) -> futures::future::BoxFuture<'a, Result<T, BoxError>>
        + Send
        + 'static,
{
    let context: &dyn Context = &**kiana_chord::context::BACKGROUND_CONTEXT;
    session
        .commit_with(change, context, Default::default())
        .await
}

/// 创建一个 ownerless 会话。
pub async fn create_conversation(session: &SessionImpl) -> ConversationId {
    commit(session, |tx| {
        Box::pin(async move {
            Ok(tx
                .create_conversation(&kiana_durable::CreateConversationOptions {
                    ownership: kiana_durable::ConversationOwnership::Ownerless,
                })
                .await?
                .id)
        })
    })
    .await
    .expect("conversation creation succeeds")
}

/// 测试定义：会话作用域、最新历史、initial fork，初始值 `{"value": 0}`。
pub fn counter_definition() -> DocToken {
    test_definition("test.counter", 1)
}

pub fn session_definition(kind: &str, version: i64) -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new(
            kind,
            version,
            kiana_durable::DocumentSemantics {
                scope: kiana_durable::ScopeKind::Session,
                history: None,
                fork: None,
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("count".to_owned(), json!(0));
            value
        }),
    )
}

pub fn test_definition(kind: &str, version: i64) -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new(
            kind,
            version,
            kiana_durable::DocumentSemantics {
                scope: kiana_durable::ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Initial),
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("value".to_owned(), json!(0));
            value
        }),
    )
}

pub fn target(token: &DocToken, conversation_id: ConversationId) -> DocTarget {
    DocTarget::conversation(token, conversation_id)
}

/// 一次文档提交：读改写计数。
pub async fn bump(
    session: &SessionImpl,
    token: &DocToken,
    conversation_id: ConversationId,
    by: i64,
) -> Result<(), BoxError> {
    let target = target(token, conversation_id);
    session
        .commit_with(
            move |tx| {
                Box::pin(async move {
                    let mut draft = tx.doc(&target).await?;
                    let current = draft.get("value").and_then(JsonValue::as_i64).unwrap_or(0);
                    draft.insert("value".to_owned(), json!(current + by));
                    Ok(())
                })
            },
            &**kiana_chord::context::BACKGROUND_CONTEXT,
            Default::default(),
        )
        .await
}

pub fn value_of(document: &JsonObject) -> JsonValue {
    document.get("value").cloned().unwrap_or(JsonValue::Null)
}

/// 一个只读快照。
pub async fn snapshot(
    session: &SessionImpl,
    token: &DocToken,
    conversation_id: ConversationId,
) -> Result<Option<JsonObject>, BoxError> {
    session
        .snapshot(
            &target(token, conversation_id),
            &**kiana_chord::context::BACKGROUND_CONTEXT,
        )
        .await
}

/// 直接对一个目标做快照。
pub async fn snapshot_target(
    session: &SessionImpl,
    target: &DocTarget,
) -> Result<Option<JsonObject>, BoxError> {
    session
        .snapshot(target, &**kiana_chord::context::BACKGROUND_CONTEXT)
        .await
}

// ---------------------------------------------------------------------------
// Task helpers
// ---------------------------------------------------------------------------

/// `defineTask` 的擦除形态（`initial` 见 `workTask`）。
pub fn work_task(name: &str, version: i64) -> AnyTask {
    work_task_with_initial(name, version, |_| json!({ "phase": "start" }))
}

pub fn work_task_with_initial(
    name: &str,
    version: i64,
    initial: impl Fn(JsonValue) -> JsonValue + Send + Sync + 'static,
) -> AnyTask {
    AnyTask {
        definition: Arc::new(AnyTaskDefinition {
            name: name.to_owned(),
            version,
            initial: Arc::new(ErasedTaskInitial(Arc::new(initial))),
            phases: IndexMap::new(),
            abort: Arc::new(()),
            migrate: None,
            hooks: None,
        }),
    }
}

/// `{ ownership: { kind: "conversation" }, conversationId, background }`。
pub fn conversation_task_options(conversation_id: ConversationId, background: bool) -> TaskOptions {
    let mut options = TaskOptions::new(TaskOwnership::Conversation);
    options.conversation_id = Some(conversation_id);
    options.background = background.then_some(true);
    options
}

/// `test/session-tables.test.ts` 的本地 `createTask()`：创建任务，可选地写一条进度行。
pub async fn create_task(
    session: &SessionImpl,
    task: &AnyTask,
    conversation_id: ConversationId,
    with_document: Option<(&DocToken, &str)>,
) -> Result<TaskId, BoxError> {
    let task = task.clone();
    let with_document = with_document.map(|(token, line)| (token.clone(), line.to_owned()));
    commit(session, move |tx| {
        let task = task.clone();
        let with_document = with_document.clone();
        Box::pin(async move {
            let task_id = tx
                .create_task(
                    &task,
                    json!({ "path": "a" }),
                    conversation_task_options(conversation_id, false),
                )
                .await?;
            if let Some((token, line)) = with_document {
                let target = DocTarget::task(&token, task_id);
                let mut draft = tx.doc(&target).await?;
                let lines = draft
                    .entry("lines".to_owned())
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .expect("lines is an array");
                lines.push(json!(line));
            }
            Ok(task_id)
        })
    })
    .await
}

/// TS 测试里的 `terminal(task)`：把记录改成完成终态。
pub fn terminal_record(task: &TaskRecord) -> TaskRecord {
    let mut record = task.clone();
    record.state = TaskState::Terminal {
        outcome: TaskOutcome::Completed {
            result: json!({ "ok": true }),
        },
    };
    record
}

/// `EntryDraft` 便捷构造。
pub fn entry_draft(kind: &str, data: Option<JsonValue>) -> EntryDraft {
    EntryDraft {
        key_order: None,
        kind: kind.to_owned(),
        model: None,
        data,
        head: None,
        edits: None,
    }
}

// ---------------------------------------------------------------------------
// 文档定义（session-tables / session-checkpoints-migrations 的本地定义）
// ---------------------------------------------------------------------------

fn lines_initial() -> JsonObject {
    let mut value = JsonObject::new();
    value.insert("lines".to_owned(), json!([]));
    value
}

/// `ProgressDoc`：task 作用域、最新历史。
pub fn progress_definition(kind: &str) -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new(kind, 1, DocumentSemantics::TASK).with_initial(|_| lines_initial()),
    )
}

/// `StepDoc`：task 作用域、keyed 家族。
pub fn step_definition(kind: &str) -> DocFamilyToken {
    kiana_durable::define_doc_family(
        DocDefinition::family(kind, 1, DocumentSemantics::TASK).with_initial(|_| lines_initial()),
    )
}

/// `NotesDoc`：conversation 作用域、可回退、fork asOf。
pub fn notes_definition(kind: &str) -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new(
            kind,
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Rewindable),
                fork: Some(DocumentFork::AsOf),
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("text".to_owned(), json!(""));
            value
        }),
    )
}

/// 让 Session 失败并等待关闭（测试收尾）。
pub async fn wait_closed(session: &SessionImpl) -> SessionEnd {
    session.closed().await
}
