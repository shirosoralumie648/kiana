//! 写入门面：权限 gate、脱敏、审计、system prompt 快照、审批 pin。
//!
//! `MemoryStore` 的写 API 是 crate 内部接口；外部只能通过 [`MemoryService`] 写入。服务层
//! 保证三件事：
//!
//! 1. **不绕过权限**：每次 apply/forget/approve 先调用 [`MemoryWriteGate`]。默认没有
//!    「静默放行」——[`TrustedGate`] 是显式的测试/已审批入口，wire 层应实现基于
//!    `kiana-permissions` 的 gate（见 `docs/history/porting/handoffs/phase-10.md`）。
//! 2. **不绕过脱敏**：写入前对 name/description/body/source_key 以及 forget 原因执行
//!    [`crate::redact::redact_secrets`]，命中次数写进审计（不回显原文）。
//! 3. **每次操作一条审计**：`audit.jsonl` 每行一条 JSON；批次里每个 add/replace/remove/
//!    patch 各一条，被拒绝的批次也逐操作记录 `outcome=rejected`。
//!
//! # 快照与审批
//!
//! [`MemorySession`] 在加载时冻结 [`MemorySnapshot`]；会话中途写入只改磁盘，不改变当前
//! system prompt（前缀缓存稳定）。压缩后调用 [`MemorySession::after_compaction`] 重新
//! 加载，新快照才会生效。
//!
//! [`MemoryService::stage_replace`] 把 `old_text` 解析成完整条目并 pin 住规范字节；
//! [`MemoryService::approve`] 只按 pinned 字节应用。审批期间条目被其他写入修改时返回
//! [`MemoryError::PinnedEntryChanged`]，不会用旧 `old_text` 去命中新内容（TOCTOU-safe）。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::{MemoryConfig, MemoryScope};
use crate::entry::{MemoryEntryDraft, content_digest};
use crate::error::{MemoryError, MemoryResult, io_error};
use crate::index::IndexEntry;
use crate::lock::FileLock;
use crate::redact::redact_secrets;
use crate::store::{
    ForgetReport, MemoryStore, StoreSnapshot, WriteBatch, WriteOpKind, WriteOperation, WriteReport,
};

/// 审计格式版本。
pub const AUDIT_FORMAT_VERSION: u32 = 1;

/// 写入来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteSource {
    /// 模型/用户显式工具调用。
    Tool,
    /// 后台抽取。
    Extraction,
    /// 用户命令（`/remember`、`/forget`）。
    User,
    /// 审批后应用 pinned 字节。
    Approval,
    /// 测试。
    Test,
}

impl WriteSource {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Extraction => "extraction",
            Self::User => "user",
            Self::Approval => "approval",
            Self::Test => "test",
        }
    }
}

/// 权限层看到的写入意图（不含正文，避免把 secret 交给审计/日志）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteIntent {
    /// 操作类型。
    pub kind: WriteOpKind,
    /// 目标条目 id（add 为空）。
    pub id: Option<String>,
    /// 条目名称（remove 为空）。
    pub name: Option<String>,
}

/// 一次写入请求。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteRequest {
    /// scope。
    pub scope: MemoryScope,
    /// 来源。
    pub source: WriteSource,
    /// 操作意图。
    pub intents: Vec<WriteIntent>,
}

/// 记忆写权限 gate。
pub trait MemoryWriteGate: Send + Sync {
    /// 授权则 `Ok(())`；拒绝返回 [`MemoryError::PermissionDenied`]。
    fn authorize(&self, request: &WriteRequest) -> MemoryResult<()>;
}

/// 显式信任的 gate（测试、以及已经走完审批流程的 approve 路径）。
///
/// 生产环境不要默认使用它；wire 层应把 [`WriteRequest`] 映射成 `kiana-permissions` 的
/// `PermissionRequest` 并处理 `Ask`。
#[derive(Clone, Copy, Debug, Default)]
pub struct TrustedGate;

impl MemoryWriteGate for TrustedGate {
    fn authorize(&self, _request: &WriteRequest) -> MemoryResult<()> {
        Ok(())
    }
}

/// 拒绝所有写入（headless/未授权场景的 fail-closed 默认）。
#[derive(Clone, Debug)]
pub struct DenyAllGate {
    /// 拒绝原因。
    pub reason: String,
}

impl DenyAllGate {
    /// 构造拒绝 gate。
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl Default for DenyAllGate {
    fn default() -> Self {
        Self::new("memory writes require explicit approval")
    }
}

impl MemoryWriteGate for DenyAllGate {
    fn authorize(&self, _request: &WriteRequest) -> MemoryResult<()> {
        Err(MemoryError::PermissionDenied {
            reason: self.reason.clone(),
        })
    }
}

/// 审计记录（一行 JSON）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// 格式版本。
    pub version: u32,
    /// Unix 毫秒。
    pub ts_ms: u64,
    /// scope。
    pub scope: String,
    /// 来源。
    pub source: String,
    /// 操作（add/replace/remove/patch/forget/stage/approve）。
    pub op: String,
    /// 条目 id。
    pub entry: Option<String>,
    /// 条目名称。
    pub name: Option<String>,
    /// `applied` / `rejected`。
    pub outcome: String,
    /// 拒绝/删除原因。
    pub reason: Option<String>,
    /// 新 revision。
    pub revision: Option<u64>,
    /// 脱敏命中次数。
    pub redactions: usize,
}

/// 写入服务。
#[derive(Clone)]
pub struct MemoryService {
    store: MemoryStore,
    gate: Arc<dyn MemoryWriteGate>,
}

impl MemoryService {
    /// 使用显式 gate 构造。
    pub fn new(store: MemoryStore, gate: Arc<dyn MemoryWriteGate>) -> Self {
        Self { store, gate }
    }

    /// 使用 [`TrustedGate`]（测试/已审批入口）。
    pub fn trusted(store: MemoryStore) -> Self {
        Self::new(store, Arc::new(TrustedGate))
    }

    /// 底层存储。
    pub fn store(&self) -> &MemoryStore {
        &self.store
    }

    /// 预先计算一次写入会提交给权限 gate 的请求（已脱敏，与 [`Self::apply`] 内部一致）。
    ///
    /// 交互式审批需要在调用 `apply` 之前拿到精确 intent 集合（审批 token 按 intent 精确
    /// 匹配），同时不能让未脱敏的名称进入 UI/日志。
    pub fn authorization_request(&self, batch: &WriteBatch, source: WriteSource) -> WriteRequest {
        let (redacted, _) = redact_batch(batch.clone());
        request_for_batch(&self.store, &redacted, source)
    }

    /// 应用一批写入。
    pub fn apply(&self, batch: WriteBatch, source: WriteSource) -> MemoryResult<WriteReport> {
        // 先脱敏再交给权限层：gate 看到的是真正会落盘的内容，不会把 secret 带进
        // 权限事件/诊断。
        let (redacted_batch, redactions) = redact_batch(batch);
        let request = request_for_batch(&self.store, &redacted_batch, source);
        if let Err(denied) = self.gate.authorize(&request) {
            // 审计是写入契约的一部分：拒绝也必须留下 rejected 记录；审计失败时
            // 优先把审计错误返回给调用方（它说明治理链本身不可用）。
            self.audit_rejected(&redacted_batch, source, &denied)?;
            return Err(denied);
        }

        match self.store.apply_batch(&redacted_batch) {
            Ok(mut report) => {
                report.redactions = redactions;
                self.audit_applied(&report, source)?;
                Ok(report)
            }
            Err(error) => {
                self.audit_rejected(&redacted_batch, source, &error)?;
                Err(error)
            }
        }
    }

    /// 遗忘条目（墓碑 + 级联 + 审计）。
    pub fn forget(
        &self,
        id: &str,
        reason: &str,
        now_ms: u64,
        source: WriteSource,
    ) -> MemoryResult<ForgetReport> {
        let request = WriteRequest {
            scope: self.store.paths().scope,
            source,
            intents: vec![WriteIntent {
                kind: WriteOpKind::Remove,
                id: Some(id.to_string()),
                name: None,
            }],
        };
        if let Err(denied) = self.gate.authorize(&request) {
            self.audit_record(AuditRecord {
                version: AUDIT_FORMAT_VERSION,
                ts_ms: now_ms,
                scope: self.store.paths().scope.as_str().to_string(),
                source: source.as_str().to_string(),
                op: "forget".into(),
                entry: Some(id.to_string()),
                name: None,
                outcome: "rejected".into(),
                reason: Some(redact(&denied.to_string())),
                revision: None,
                redactions: 0,
            })?;
            return Err(denied);
        }
        let redacted_reason = redact(reason);
        let report = match self.store.forget(id, &redacted_reason, now_ms) {
            Ok(report) => report,
            Err(error) => {
                self.audit_record(AuditRecord {
                    version: AUDIT_FORMAT_VERSION,
                    ts_ms: now_ms,
                    scope: self.store.paths().scope.as_str().to_string(),
                    source: source.as_str().to_string(),
                    op: "forget".into(),
                    entry: Some(id.to_string()),
                    name: None,
                    outcome: "rejected".into(),
                    reason: Some(redact(&error.to_string())),
                    revision: None,
                    redactions: 0,
                })?;
                return Err(error);
            }
        };
        for removed in &report.removed {
            self.audit_record(AuditRecord {
                version: AUDIT_FORMAT_VERSION,
                ts_ms: now_ms,
                scope: self.store.paths().scope.as_str().to_string(),
                source: source.as_str().to_string(),
                op: "forget".into(),
                entry: Some(removed.id.clone()),
                name: Some(redact(&removed.name)),
                outcome: "applied".into(),
                reason: Some(redact(&removed.reason)),
                revision: Some(report.revision),
                redactions: 0,
            })?;
        }
        Ok(report)
    }

    /// 审批前把 `old_text` 解析成完整条目并 pin 规范字节。
    ///
    /// `old_text` 为空表示整条替换。命中 0 条返回 `NoMatch`，多条返回 `AmbiguousMatch`。
    /// 本函数不写盘；审批通过后调用 [`Self::approve`]。
    pub fn stage_replace(
        &self,
        id: &str,
        old_text: &str,
        new_entry: MemoryEntryDraft,
        now_ms: u64,
        source: WriteSource,
    ) -> MemoryResult<StagedWrite> {
        // 只读取并记录审批对象，不写盘；真正的授权在 `approve -> apply` 时执行，
        // 这样交互式 gate 可以在 stage 之后询问用户，而不是在 stage 阶段被同步拒绝。
        new_entry.validate()?;
        let entry = self.store.read_entry(id)?;
        if !old_text.is_empty() {
            let matches = entry.body.match_indices(old_text).count();
            if matches == 0 {
                return Err(MemoryError::NoMatch {
                    entry: id.to_string(),
                    old_text: old_text.to_string(),
                });
            }
            if matches > 1 {
                return Err(MemoryError::AmbiguousMatch {
                    old_text: old_text.to_string(),
                    matches,
                });
            }
        }
        let pinned_bytes = entry.to_file_bytes();
        let stage_id = content_digest(
            format!(
                "{id}\u{0}{}\u{0}{}\u{0}{now_ms}",
                content_digest(&pinned_bytes),
                content_digest(new_entry.body.as_bytes())
            )
            .as_bytes(),
        );
        let snapshot = self.store.load_snapshot()?;
        Ok(StagedWrite {
            stage_id,
            entry_id: id.to_string(),
            pinned_bytes,
            new_entry,
            expected_revision: snapshot.revision,
            source,
            created_at_ms: now_ms,
        })
    }

    /// 应用审批通过的 staged 写入。
    ///
    /// 只按 pinned 字节判断目标是否仍是审批者看到的那一条：当前文件字节必须与
    /// `pinned_bytes` 完全一致，否则返回 [`MemoryError::PinnedEntryChanged`]，不写盘。
    pub fn approve(&self, staged: &StagedWrite, _now_ms: u64) -> MemoryResult<WriteReport> {
        // pinned 字节的摘要随写入一起进入 store；比较与替换在同一把锁内完成，
        // 因此审批与写入之间没有 TOCTOU 窗口。
        let batch = WriteBatch::new().replace_if_unchanged(
            staged.entry_id.clone(),
            content_digest(&staged.pinned_bytes),
            staged.new_entry.clone(),
        );
        // `apply` 已经写审计；这里不再重复。
        self.apply(batch, WriteSource::Approval)
    }

    /// 写入抽取整合摘要（`memory_summary.md`），经过 gate/脱敏/审计。
    pub(crate) fn write_summary(
        &self,
        text: &str,
        source: WriteSource,
        now_ms: u64,
    ) -> MemoryResult<()> {
        let request = WriteRequest {
            scope: self.store.paths().scope,
            source,
            intents: vec![WriteIntent {
                kind: WriteOpKind::Add,
                id: None,
                name: Some("memory_summary.md".into()),
            }],
        };
        if let Err(denied) = self.gate.authorize(&request) {
            self.audit_rejected_request(&request, now_ms, &denied)?;
            return Err(denied);
        }
        let redaction = redact_secrets(text);
        if let Err(error) = self.store.write_summary(&redaction.text) {
            self.audit_rejected_request(&request, now_ms, &error)?;
            return Err(error);
        }
        self.audit_record(AuditRecord {
            version: AUDIT_FORMAT_VERSION,
            ts_ms: now_ms,
            scope: self.store.paths().scope.as_str().to_string(),
            source: source.as_str().to_string(),
            op: "summary".into(),
            entry: None,
            name: Some("memory_summary.md".into()),
            outcome: "applied".into(),
            reason: None,
            revision: None,
            redactions: redaction.replacements,
        })?;
        Ok(())
    }

    /// 维护入口：重建索引（同样经过权限 gate 与审计）。
    pub fn rebuild_index(&self, now_ms: u64) -> MemoryResult<StoreSnapshot> {
        let request = WriteRequest {
            scope: self.store.paths().scope,
            source: WriteSource::User,
            intents: vec![WriteIntent {
                kind: WriteOpKind::Add,
                id: None,
                name: Some("MEMORY.md".into()),
            }],
        };
        if let Err(denied) = self.gate.authorize(&request) {
            self.audit_rejected_request(&request, now_ms, &denied)?;
            return Err(denied);
        }
        let snapshot = match self.store.rebuild_index() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.audit_rejected_request(&request, now_ms, &error)?;
                return Err(error);
            }
        };
        self.audit_record(AuditRecord {
            version: AUDIT_FORMAT_VERSION,
            ts_ms: now_ms,
            scope: self.store.paths().scope.as_str().to_string(),
            source: WriteSource::User.as_str().to_string(),
            op: "rebuild_index".into(),
            entry: None,
            name: Some("MEMORY.md".into()),
            outcome: "applied".into(),
            reason: None,
            revision: Some(snapshot.revision),
            redactions: 0,
        })?;
        Ok(snapshot)
    }

    /// 加载 system prompt 快照。
    pub fn snapshot(&self) -> MemoryResult<MemorySnapshot> {
        let snapshot = self.store.load_snapshot()?;
        Ok(MemorySnapshot::from_store(snapshot, now_millis()))
    }

    /// 底层存储快照（不做额外包装）。
    pub fn store_snapshot(&self) -> MemoryResult<StoreSnapshot> {
        self.store.load_snapshot()
    }

    fn audit_applied(&self, report: &WriteReport, source: WriteSource) -> MemoryResult<()> {
        let now = now_millis();
        for operation in &report.applied {
            self.audit_record(AuditRecord {
                version: AUDIT_FORMAT_VERSION,
                ts_ms: now,
                scope: self.store.paths().scope.as_str().to_string(),
                source: source.as_str().to_string(),
                op: operation.kind.as_str().to_string(),
                entry: Some(operation.id.clone()),
                name: Some(redact(&operation.name)),
                outcome: "applied".into(),
                reason: None,
                revision: Some(report.revision),
                redactions: report.redactions,
            })?;
        }
        Ok(())
    }

    fn audit_rejected(
        &self,
        batch: &WriteBatch,
        source: WriteSource,
        error: &MemoryError,
    ) -> MemoryResult<()> {
        let now = now_millis();
        let reason = redact(&error.to_string());
        for operation in &batch.operations {
            let (kind, id, name) = match operation {
                WriteOperation::Add(draft) => (WriteOpKind::Add, None, Some(redact(&draft.name))),
                WriteOperation::Replace { id, entry }
                | WriteOperation::ReplaceIfUnchanged {
                    id,
                    expected_entry_digest: _,
                    entry,
                } => (
                    WriteOpKind::Replace,
                    Some(id.clone()),
                    Some(redact(&entry.name)),
                ),
                WriteOperation::Remove { id } => (WriteOpKind::Remove, Some(id.clone()), None),
                WriteOperation::Patch { id, .. } => (WriteOpKind::Patch, Some(id.clone()), None),
            };
            self.audit_record(AuditRecord {
                version: AUDIT_FORMAT_VERSION,
                ts_ms: now,
                scope: self.store.paths().scope.as_str().to_string(),
                source: source.as_str().to_string(),
                op: kind.as_str().to_string(),
                entry: id,
                name,
                outcome: "rejected".into(),
                reason: Some(reason.clone()),
                revision: None,
                redactions: 0,
            })?;
        }
        Ok(())
    }

    /// 写一条整请求级的 rejected 记录（summary / rebuild_index）。
    fn audit_rejected_request(
        &self,
        request: &WriteRequest,
        now_ms: u64,
        error: &MemoryError,
    ) -> MemoryResult<()> {
        for intent in &request.intents {
            self.audit_record(AuditRecord {
                version: AUDIT_FORMAT_VERSION,
                ts_ms: now_ms,
                scope: request.scope.as_str().to_string(),
                source: request.source.as_str().to_string(),
                op: intent.kind.as_str().to_string(),
                entry: intent.id.clone(),
                name: intent.name.clone().map(|name| redact(&name)),
                outcome: "rejected".into(),
                reason: Some(redact(&error.to_string())),
                revision: None,
                redactions: 0,
            })?;
        }
        Ok(())
    }

    /// 追加一条审计；同一进程/跨进程的审计写入由独立 flock 串行化，失败向上传播。
    fn audit_record(&self, record: AuditRecord) -> MemoryResult<()> {
        append_audit(
            &self.store.paths().audit_path,
            &self.store.paths().audit_lock_path,
            self.store.config().lock_timeout,
            &record,
        )
    }
}

/// 已 pin 的审批写入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedWrite {
    /// 审批凭据 id。
    pub stage_id: String,
    /// 目标条目 id。
    pub entry_id: String,
    /// 审批时看到的完整条目规范字节。
    pub pinned_bytes: Vec<u8>,
    /// 批准后要写入的新内容。
    pub new_entry: MemoryEntryDraft,
    /// 审批时索引 revision（信息用途）。
    pub expected_revision: u64,
    /// 来源。
    pub source: WriteSource,
    /// 创建时间。
    pub created_at_ms: u64,
}

/// load-time system prompt 快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemorySnapshot {
    /// scope。
    pub scope: MemoryScope,
    /// revision。
    pub revision: u64,
    /// 已截断的索引内容。
    pub index: String,
    /// 索引条目。
    pub entries: Vec<IndexEntry>,
    /// 截断警告。
    pub warnings: Vec<String>,
    /// 是否截断。
    pub truncated: bool,
    /// 加载时间（Unix 毫秒）。
    pub loaded_at_ms: u64,
}

impl MemorySnapshot {
    fn from_store(snapshot: StoreSnapshot, loaded_at_ms: u64) -> Self {
        Self {
            scope: snapshot.scope,
            revision: snapshot.revision,
            index: snapshot.index,
            entries: snapshot.entries,
            warnings: snapshot.warnings,
            truncated: snapshot.truncated,
            loaded_at_ms,
        }
    }

    /// system prompt 里应注入的索引字节。
    pub fn prompt_index(&self) -> &str {
        &self.index
    }
}

/// 压缩后重新加载的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotChange {
    /// 压缩前 revision。
    pub previous_revision: u64,
    /// 压缩后 revision。
    pub new_revision: u64,
    /// 内容是否变化。
    pub changed: bool,
}

/// 会话级记忆句柄：持有 load-time 快照。
#[derive(Clone)]
pub struct MemorySession {
    service: MemoryService,
    snapshot: MemorySnapshot,
}

impl MemorySession {
    /// 加载快照。
    pub fn load(service: MemoryService) -> MemoryResult<Self> {
        let snapshot = service.snapshot()?;
        Ok(Self { service, snapshot })
    }

    /// 当前快照。
    pub fn snapshot(&self) -> &MemorySnapshot {
        &self.snapshot
    }

    /// 当前 turn 的 system prompt 索引字节（中途写入不会改变它）。
    pub fn system_prompt_index(&self) -> &str {
        self.snapshot.prompt_index()
    }

    /// 服务引用（只读）。
    pub fn service(&self) -> &MemoryService {
        &self.service
    }

    /// 会话中途写入：磁盘会更新，但快照保持不变。
    pub fn apply(&mut self, batch: WriteBatch, source: WriteSource) -> MemoryResult<WriteReport> {
        self.service.apply(batch, source)
    }

    /// 压缩后重新加载快照；这是 mid-session 写入生效的唯一时机。
    pub fn after_compaction(&mut self) -> MemoryResult<SnapshotChange> {
        let previous_revision = self.snapshot.revision;
        let previous_index = self.snapshot.index.clone();
        let snapshot = self.service.snapshot()?;
        let changed = snapshot.revision != previous_revision || snapshot.index != previous_index;
        let new_revision = snapshot.revision;
        self.snapshot = snapshot;
        Ok(SnapshotChange {
            previous_revision,
            new_revision,
            changed,
        })
    }
}

fn request_for_batch(store: &MemoryStore, batch: &WriteBatch, source: WriteSource) -> WriteRequest {
    let intents = batch
        .operations
        .iter()
        .map(|operation| match operation {
            WriteOperation::Add(draft) => WriteIntent {
                kind: WriteOpKind::Add,
                id: None,
                name: Some(draft.name.clone()),
            },
            WriteOperation::Replace { id, entry }
            | WriteOperation::ReplaceIfUnchanged {
                id,
                expected_entry_digest: _,
                entry,
            } => WriteIntent {
                kind: WriteOpKind::Replace,
                id: Some(id.clone()),
                name: Some(entry.name.clone()),
            },
            WriteOperation::Remove { id } => WriteIntent {
                kind: WriteOpKind::Remove,
                id: Some(id.clone()),
                name: None,
            },
            WriteOperation::Patch { id, .. } => WriteIntent {
                kind: WriteOpKind::Patch,
                id: Some(id.clone()),
                name: None,
            },
        })
        .collect();
    WriteRequest {
        scope: store.paths().scope,
        source,
        intents,
    }
}

fn redact_draft(draft: MemoryEntryDraft) -> (MemoryEntryDraft, usize) {
    let name = redact_secrets(&draft.name);
    let description = redact_secrets(&draft.description);
    let body = redact_secrets(&draft.body);
    let source_key = draft.source_key.as_deref().map(redact_secrets);
    let mut replacements = name.replacements
        + description.replacements
        + body.replacements
        + source_key
            .as_ref()
            .map(|report| report.replacements)
            .unwrap_or(0);
    // 与 Phase 1 的 `redact_draft` 一致：derived_from 也是模型产物，逐项脱敏。
    let derived_from: Vec<String> = draft
        .derived_from
        .into_iter()
        .map(|value| {
            let report = redact_secrets(&value);
            replacements += report.replacements;
            if report.replacements > 0 {
                // `derived_from` 必须是合法 entry id；`[REDACTED]` 不是。
                "redacted".to_string()
            } else {
                report.text
            }
        })
        .collect();
    (
        MemoryEntryDraft {
            name: name.text,
            description: description.text,
            mtype: draft.mtype,
            body: body.text,
            derived_from,
            source_key: source_key.map(|report| report.text),
        },
        replacements,
    )
}

fn redact_batch(batch: WriteBatch) -> (WriteBatch, usize) {
    let mut replacements = 0usize;
    let operations = batch
        .operations
        .into_iter()
        .map(|operation| match operation {
            WriteOperation::Add(draft) => {
                let (draft, count) = redact_draft(draft);
                replacements += count;
                WriteOperation::Add(draft)
            }
            WriteOperation::Replace { id, entry } => {
                let (entry, count) = redact_draft(entry);
                replacements += count;
                WriteOperation::Replace { id, entry }
            }
            WriteOperation::ReplaceIfUnchanged {
                id,
                expected_entry_digest,
                entry,
            } => {
                let (entry, count) = redact_draft(entry);
                replacements += count;
                WriteOperation::ReplaceIfUnchanged {
                    id,
                    expected_entry_digest,
                    entry,
                }
            }
            WriteOperation::Remove { id } => WriteOperation::Remove { id },
            WriteOperation::Patch {
                id,
                old_text,
                new_text,
            } => {
                let new_text = redact_secrets(&new_text);
                replacements += new_text.replacements;
                WriteOperation::Patch {
                    id,
                    old_text,
                    new_text: new_text.text,
                }
            }
        })
        .collect();
    (
        WriteBatch {
            expected_revision: batch.expected_revision,
            operations,
        },
        replacements,
    )
}

fn redact(value: &str) -> String {
    redact_secrets(value).text
}

/// 读取审计文件（测试/展示用）。
pub fn read_audit(path: &Path) -> MemoryResult<Vec<AuditRecord>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error(path, error)),
    };
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str(line).map_err(|error| {
            MemoryError::InvalidArgument(format!("invalid audit line {}: {error}", index + 1))
        })?;
        records.push(record);
    }
    Ok(records)
}

/// 追加一条审计记录。
///
/// 审计与业务写入使用**独立**的跨进程锁（`audit.lock`），因此 store 的 flock 释放后，
/// 两个进程的 JSONL 追加仍然串行；整行（含换行）一次 `write_all` 写出，避免留下半个
/// JSON 行。锁/打开/写入/刷盘任何一步失败都返回错误，调用方必须处理。
fn append_audit(
    path: &Path,
    lock_path: &Path,
    lock_timeout: Duration,
    record: &AuditRecord,
) -> MemoryResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    }
    let _lock = FileLock::acquire(lock_path, lock_timeout)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| io_error(path, error))?;
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    file.write_all(&line)
        .and_then(|()| file.sync_data())
        .map_err(|error| io_error(path, error))?;
    Ok(())
}

/// 当前 Unix 毫秒。
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// 便捷构造：project scope + 默认配置 + trusted gate。
pub fn open_project_service(cwd: impl AsRef<Path>) -> MemoryResult<MemoryService> {
    Ok(MemoryService::trusted(MemoryStore::open_project(
        cwd,
        MemoryConfig::default(),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScopePaths;
    use crate::entry::MemoryType;

    fn service() -> (tempfile::TempDir, MemoryService) {
        let dir = tempfile::tempdir().unwrap();
        let paths = ScopePaths::at_root(MemoryScope::Project, dir.path().join("memory"));
        let store = MemoryStore::open(paths, MemoryConfig::default()).unwrap();
        (dir, MemoryService::trusted(store))
    }

    #[test]
    fn denied_write_creates_audit_and_no_entry() {
        let (_dir, trusted) = service();
        let service = MemoryService::new(
            trusted.store().clone(),
            Arc::new(DenyAllGate::new("no approval")),
        );
        let error = service
            .apply(
                WriteBatch::new().add(MemoryEntryDraft::new("x", "d", MemoryType::User, "b")),
                WriteSource::Tool,
            )
            .unwrap_err();
        assert!(matches!(error, MemoryError::PermissionDenied { .. }));
        assert!(service.store().read_entry("x").is_err());
        let records = read_audit(&service.store().paths().audit_path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, "rejected");
    }

    #[test]
    fn write_redacts_secret_before_disk() {
        let (_dir, service) = service();
        service
            .apply(
                WriteBatch::new().add(MemoryEntryDraft::new(
                    "x",
                    "d",
                    MemoryType::User,
                    "token=super-secret-value-1234567890",
                )),
                WriteSource::Tool,
            )
            .unwrap();
        let entry = service.store().read_entry("x").unwrap();
        assert!(!entry.body.contains("super-secret-value"));
        assert!(entry.body.contains("[REDACTED]"));
    }

    #[test]
    fn audit_failure_is_not_silently_ignored() {
        let (_dir, service) = service();
        // 让 audit.jsonl 变成目录：打开追加必然失败。业务写入不能假装成功。
        std::fs::create_dir_all(&service.store().paths().audit_path).unwrap();
        let error = service
            .apply(
                WriteBatch::new().add(MemoryEntryDraft::new("x", "d", MemoryType::User, "b")),
                WriteSource::Tool,
            )
            .expect_err("audit append failure must surface");
        assert!(matches!(error, MemoryError::Io { .. }), "{error}");
    }

    #[test]
    fn rejected_maintenance_operations_are_audited() {
        let (_dir, trusted) = service();
        let service = MemoryService::new(
            trusted.store().clone(),
            Arc::new(DenyAllGate::new("no approval")),
        );
        let error = service.rebuild_index(1).unwrap_err();
        assert!(matches!(error, MemoryError::PermissionDenied { .. }));
        service
            .write_summary("summary", WriteSource::Extraction, 2)
            .unwrap_err();
        let records = read_audit(&service.store().paths().audit_path).unwrap();
        assert_eq!(records.len(), 2, "{records:?}");
        assert!(records.iter().all(|record| record.outcome == "rejected"));
        assert!(records.iter().any(|record| record.op == "add"));
    }

    #[test]
    fn forget_store_error_is_audited() {
        let (_dir, service) = service();
        service
            .forget("missing", "cleanup", 1, WriteSource::User)
            .unwrap_err();
        let records = read_audit(&service.store().paths().audit_path).unwrap();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].op, "forget");
        assert_eq!(records[0].outcome, "rejected");
    }

    #[test]
    fn concurrent_audit_appends_stay_well_formed() {
        let (_dir, service) = service();
        std::thread::scope(|scope| {
            for index in 0..8 {
                let service = service.clone();
                scope.spawn(move || {
                    service
                        .apply(
                            WriteBatch::new().add(MemoryEntryDraft::new(
                                format!("entry-{index}"),
                                "concurrent",
                                MemoryType::User,
                                "body",
                            )),
                            WriteSource::Tool,
                        )
                        .unwrap();
                });
            }
        });
        let records = read_audit(&service.store().paths().audit_path).unwrap();
        assert_eq!(records.len(), 8, "{records:?}");
        assert!(records.iter().all(|record| record.outcome == "applied"));
    }
}
