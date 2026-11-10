//! 抽取/整合验收：两阶段、脱敏、watermark、lease、幂等、预算与跳过规则。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use kiana_memory::extract::LeaseRecord;
use kiana_memory::{
    ClaimOutcome, ConsolidationOperation, ConsolidationPlan, ExtractionDraft, ExtractionHooks,
    ExtractionJob, ExtractionStateStore, ExtractionStatus, FileExtractionStore, MemoryConfig,
    MemoryEntryDraft, MemoryError, MemoryScope, MemoryService, MemoryStore, MemoryType, NoHooks,
    Phase1Extractor, Phase2Consolidator, RawMemory, ScopePaths, SessionCandidate, SessionSource,
    SessionTranscript, StateCommit, TranscriptMessage,
};

struct StaticSource {
    sessions: Vec<(SessionCandidate, SessionTranscript)>,
}

#[async_trait]
impl SessionSource for StaticSource {
    async fn scan_sessions(&self) -> kiana_memory::MemoryResult<Vec<SessionCandidate>> {
        Ok(self
            .sessions
            .iter()
            .map(|(candidate, _)| candidate.clone())
            .collect())
    }

    async fn load_session(
        &self,
        session_id: &str,
    ) -> kiana_memory::MemoryResult<SessionTranscript> {
        self.sessions
            .iter()
            .find(|(candidate, _)| candidate.session_id == session_id)
            .map(|(_, transcript)| transcript.clone())
            .ok_or_else(|| MemoryError::Source(format!("unknown session {session_id}")))
    }
}

fn session(id: &str, modified_ms: u64, is_subagent: bool) -> (SessionCandidate, SessionTranscript) {
    (
        SessionCandidate {
            session_id: id.to_string(),
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            modified_at: UNIX_EPOCH + Duration::from_millis(modified_ms),
            modified_ms,
            is_subagent,
        },
        SessionTranscript {
            session_id: id.to_string(),
            messages: vec![TranscriptMessage {
                role: "user".into(),
                text: "remember this".into(),
            }],
            is_subagent,
        },
    )
}

struct FailingSource;

#[async_trait]
impl SessionSource for FailingSource {
    async fn scan_sessions(&self) -> kiana_memory::MemoryResult<Vec<SessionCandidate>> {
        Err(MemoryError::Source(
            "session directory is unreadable".into(),
        ))
    }

    async fn load_session(
        &self,
        session_id: &str,
    ) -> kiana_memory::MemoryResult<SessionTranscript> {
        Err(MemoryError::Source(format!(
            "unexpected load of {session_id}"
        )))
    }
}

struct FixedExtractor {
    draft: ExtractionDraft,
}

#[async_trait]
impl Phase1Extractor for FixedExtractor {
    async fn extract(
        &self,
        _transcript: &SessionTranscript,
    ) -> kiana_memory::MemoryResult<ExtractionDraft> {
        Ok(self.draft.clone())
    }
}

struct FixedConsolidator {
    plan: ConsolidationPlan,
}

#[async_trait]
impl Phase2Consolidator for FixedConsolidator {
    async fn consolidate(
        &self,
        _existing: &[kiana_memory::ManifestEntry],
        _draft: &ExtractionDraft,
    ) -> kiana_memory::MemoryResult<ConsolidationPlan> {
        Ok(self.plan.clone())
    }
}

struct FailingExtractor;

#[async_trait]
impl Phase1Extractor for FailingExtractor {
    async fn extract(
        &self,
        _transcript: &SessionTranscript,
    ) -> kiana_memory::MemoryResult<ExtractionDraft> {
        Err(MemoryError::Provider(
            "model unavailable for sk-ant-abcdefghijklmnopqrstuvwxyz".into(),
        ))
    }
}

struct CrashOnce {
    calls: AtomicUsize,
}

impl ExtractionHooks for CrashOnce {
    fn before_commit(&self, _session_id: &str) -> kiana_memory::MemoryResult<()> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(MemoryError::Provider("simulated crash".into()))
        } else {
            Ok(())
        }
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    service: MemoryService,
    paths: ScopePaths,
    config: MemoryConfig,
}

fn fixture(auto_extract: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let paths = ScopePaths::at_root(MemoryScope::Project, dir.path().join("memory"));
    let mut config = MemoryConfig {
        auto_extract,
        ..MemoryConfig::default()
    };
    config.extraction.min_session_age_ms = 0;
    config.extraction.lease_ttl_ms = 1_000;
    config.extraction.max_runs_per_day = 12;
    let store = MemoryStore::open(paths.clone(), config.clone()).unwrap();
    Fixture {
        _dir: dir,
        service: MemoryService::trusted(store),
        paths,
        config,
    }
}

fn state_store(fixture: &Fixture) -> FileExtractionStore {
    FileExtractionStore::for_scope(&fixture.paths, Duration::from_millis(500))
}

fn draft_with_secret() -> ExtractionDraft {
    ExtractionDraft {
        memories: vec![RawMemory {
            name: "Deploy freeze".into(),
            description: "Merge freeze for the release branch".into(),
            mtype: MemoryType::User,
            body: "Freeze starts 2026-03-05\napi_key=sk-ant-abcdefghijklmnopqrstuvwxyz".into(),
            derived_from: vec!["s1:0-12;token=sk-ant-abcdefghijklmnopqrstuvwxyz".into()],
            source_key: Some("session-1:0-12".into()),
        }],
        synopsis: "Released branch status\npassword=hunter2".into(),
    }
}

fn plan_from_draft(draft: &ExtractionDraft) -> ConsolidationPlan {
    ConsolidationPlan {
        operations: draft
            .memories
            .iter()
            .map(|memory| {
                ConsolidationOperation::Upsert(MemoryEntryDraft {
                    name: memory.name.clone(),
                    description: memory.description.clone(),
                    mtype: memory.mtype,
                    body: memory.body.clone(),
                    derived_from: memory.derived_from.clone(),
                    source_key: memory.source_key.clone(),
                })
            })
            .collect(),
        synopsis: draft.synopsis.clone(),
        carried_forward: 0,
    }
}

fn all_files(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap();
            if entry.path().is_dir() {
                stack.push(entry.path());
            } else {
                out.push(entry.path());
            }
        }
    }
    out
}

fn walk_text(root: &std::path::Path) -> String {
    let mut text = String::new();
    for path in all_files(root) {
        if let Ok(value) = fs::read_to_string(&path) {
            text.push_str(&value);
        }
    }
    text
}

#[tokio::test]
async fn two_phase_extraction_redacts_everything_and_advances_watermark() {
    let fixture = fixture(true);
    let source = StaticSource {
        sessions: vec![session("s1", 500_000, false)],
    };
    let draft = draft_with_secret();
    let extractor = FixedExtractor {
        draft: draft.clone(),
    };
    let consolidator = FixedConsolidator {
        plan: plan_from_draft(&draft),
    };
    let job = ExtractionJob::new(
        &fixture.service,
        Arc::new(state_store(&fixture)),
        &source,
        &extractor,
        &consolidator,
        &fixture.config,
    );
    let outcome = job.run_at(1_000_000).await.unwrap();
    assert_eq!(outcome.status, ExtractionStatus::Extracted);
    assert_eq!(outcome.sessions_processed, 1);
    assert_eq!(outcome.entries_written, 1);
    assert_eq!(outcome.watermark_ms, 500_000);

    let state = state_store(&fixture).load().unwrap();
    assert_eq!(state.watermark_ms, 500_000);
    assert!(state.lease.is_none());
    assert_eq!(state.daily.runs, 1);
    assert!(state.daily.bytes > 0);

    let text = walk_text(&fixture.paths.root);
    assert!(!text.contains("hunter2"), "secret leaked: {text}");
    assert!(
        !text.contains("sk-ant-abcdefghijklmnopqrstuvwxyz"),
        "secret leaked: {text}"
    );
    let summary = fs::read_to_string(&fixture.paths.summary_path).unwrap();
    assert!(summary.contains("Released branch status"));
    assert!(summary.contains("[REDACTED]"));
}

#[tokio::test]
async fn auto_extract_is_disabled_by_default() {
    let fixture = fixture(false);
    assert!(!MemoryConfig::default().auto_extract);
    let source = StaticSource {
        sessions: vec![session("s1", 500_000, false)],
    };
    let draft = draft_with_secret();
    let extractor = FixedExtractor {
        draft: draft.clone(),
    };
    let consolidator = FixedConsolidator {
        plan: plan_from_draft(&draft),
    };
    let job = ExtractionJob::new(
        &fixture.service,
        Arc::new(state_store(&fixture)),
        &source,
        &extractor,
        &consolidator,
        &fixture.config,
    );
    let outcome = job.run_at(1_000_000).await.unwrap();
    assert_eq!(outcome.status, ExtractionStatus::SkippedDisabled);
    assert!(fixture.service.store().list_entries().unwrap().is_empty());
}

#[tokio::test]
async fn daily_budget_stops_further_runs_on_the_same_day() {
    let mut fixture = fixture(true);
    fixture.config.extraction.max_runs_per_day = 1;
    let draft = draft_with_secret();
    let extractor = FixedExtractor {
        draft: draft.clone(),
    };
    let consolidator = FixedConsolidator {
        plan: plan_from_draft(&draft),
    };
    let source_a = StaticSource {
        sessions: vec![session("s1", 500_000, false)],
    };
    let job = ExtractionJob::new(
        &fixture.service,
        Arc::new(state_store(&fixture)),
        &source_a,
        &extractor,
        &consolidator,
        &fixture.config,
    );
    assert_eq!(
        job.run_at(1_000_000).await.unwrap().status,
        ExtractionStatus::Extracted
    );

    let source_b = StaticSource {
        sessions: vec![session("s2", 700_000, false)],
    };
    let job = ExtractionJob::new(
        &fixture.service,
        Arc::new(state_store(&fixture)),
        &source_b,
        &extractor,
        &consolidator,
        &fixture.config,
    );
    let outcome = job.run_at(1_100_000).await.unwrap();
    assert_eq!(outcome.status, ExtractionStatus::SkippedBudget);
    assert_eq!(fixture.service.store().list_entries().unwrap().len(), 1);
}

#[tokio::test]
async fn current_subagent_and_too_new_sessions_are_skipped() {
    let mut fixture = fixture(true);
    fixture.config.extraction.min_session_age_ms = 300_000;
    let source = StaticSource {
        sessions: vec![
            session("current", 900_000, false),
            session("sub", 900_000, true),
            session("fresh", 900_000, false),
        ],
    };
    let draft = draft_with_secret();
    let extractor = FixedExtractor {
        draft: draft.clone(),
    };
    let consolidator = FixedConsolidator {
        plan: plan_from_draft(&draft),
    };
    let mut job = ExtractionJob::new(
        &fixture.service,
        Arc::new(state_store(&fixture)),
        &source,
        &extractor,
        &consolidator,
        &fixture.config,
    );
    job.current_session_id = Some("current".into());
    let outcome = job.run_at(1_000_000).await.unwrap();
    assert_eq!(outcome.status, ExtractionStatus::NoSessions);
    assert_eq!(outcome.sessions_processed, 0);
    assert_eq!(fixture.service.store().list_entries().unwrap().len(), 0);
    let state = state_store(&fixture).load().unwrap();
    assert!(state.lease.is_none());
    assert_eq!(state.watermark_ms, 0);
}

#[tokio::test]
async fn crashed_consolidation_is_retried_after_stale_lease_without_duplicates() {
    let fixture = fixture(true);
    let source = StaticSource {
        sessions: vec![session("s1", 500_000, false)],
    };
    let draft = draft_with_secret();
    let extractor = FixedExtractor {
        draft: draft.clone(),
    };
    let consolidator = FixedConsolidator {
        plan: plan_from_draft(&draft),
    };
    let crash = CrashOnce {
        calls: AtomicUsize::new(0),
    };
    let mut job = ExtractionJob::new(
        &fixture.service,
        Arc::new(state_store(&fixture)),
        &source,
        &extractor,
        &consolidator,
        &fixture.config,
    );
    job.hooks = &crash;
    let error = job.run_at(1_000_000).await.unwrap_err();
    assert!(error.to_string().contains("simulated crash"));

    // 应用已完成但 watermark 未提交、lease 未释放。
    assert_eq!(fixture.service.store().list_entries().unwrap().len(), 1);
    let state = state_store(&fixture).load().unwrap();
    assert!(state.lease.is_some());
    assert_eq!(state.watermark_ms, 0);

    // 过期后允许接管，attempt 递增。
    let store = state_store(&fixture);
    match store
        .claim_at(1_000_000 + 10_000, "recover", 1_000)
        .unwrap()
    {
        ClaimOutcome::Claimed {
            stale_takeover,
            lease,
            ..
        } => {
            assert!(stale_takeover);
            assert_eq!(lease.attempt, 2);
        }
        other => panic!("expected stale takeover, got {other:?}"),
    }
    store.release("recover").unwrap();

    // 重跑：source_key 已存在，不重复写；watermark 前进。
    let mut retry = ExtractionJob::new(
        &fixture.service,
        Arc::new(state_store(&fixture)),
        &source,
        &extractor,
        &consolidator,
        &fixture.config,
    );
    retry.hooks = &NoHooks;
    let outcome = retry.run_at(1_000_000 + 20_000).await.unwrap();
    assert_eq!(outcome.status, ExtractionStatus::Extracted);
    assert_eq!(outcome.sessions_processed, 1);
    assert_eq!(outcome.entries_written, 1);
    assert_eq!(fixture.service.store().list_entries().unwrap().len(), 1);
    let state = state_store(&fixture).load().unwrap();
    assert_eq!(state.watermark_ms, 500_000);
    assert!(state.lease.is_none());
}

#[tokio::test]
async fn provider_failure_releases_lease_and_keeps_watermark() {
    let fixture = fixture(true);
    let source = StaticSource {
        sessions: vec![session("s1", 500_000, false)],
    };
    let extractor = FailingExtractor;
    let draft = draft_with_secret();
    let consolidator = FixedConsolidator {
        plan: plan_from_draft(&draft),
    };
    let job = ExtractionJob::new(
        &fixture.service,
        Arc::new(state_store(&fixture)),
        &source,
        &extractor,
        &consolidator,
        &fixture.config,
    );
    let outcome = job.run_at(1_000_000).await.unwrap();
    assert_eq!(outcome.status, ExtractionStatus::Failed);
    assert_eq!(outcome.watermark_ms, 0);
    let state = state_store(&fixture).load().unwrap();
    assert!(state.lease.is_none());
    assert_eq!(state.consecutive_failures, 1);
    assert!(state.last_error.is_some());
    let text = walk_text(&fixture.paths.root);
    assert!(
        !text.contains("sk-ant-abcdefghijklmnopqrstuvwxyz"),
        "provider error leaked a secret into state/audit: {text}"
    );
}

#[tokio::test]
async fn commit_rejects_a_stale_revision() {
    let fixture = fixture(true);
    let store = state_store(&fixture);
    let claim = store.claim_at(1_000, "a", 5_000).unwrap();
    let revision = match claim {
        ClaimOutcome::Claimed { revision, .. } => revision,
        other => panic!("{other:?}"),
    };
    let state = store.load().unwrap();
    // 另一个 writer 让 revision 前进。
    store.release("a").unwrap();
    let stale = store
        .commit(
            "a",
            revision,
            2_000,
            StateCommit {
                watermark_ms: 10,
                watermark_session: Some("s".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(matches!(stale, MemoryError::RevisionConflict { .. }));
    assert_eq!(state.version, kiana_memory::EXTRACTION_STATE_FORMAT_VERSION);
    // 新的 claim 可以正常提交。
    let claim = store.claim_at(3_000, "b", 5_000).unwrap();
    let revision = match claim {
        ClaimOutcome::Claimed { revision, .. } => revision,
        other => panic!("{other:?}"),
    };
    let committed = store
        .commit(
            "b",
            revision,
            3_000,
            StateCommit {
                watermark_ms: 10,
                watermark_session: Some("s".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(committed.watermark_ms, 10);
    assert!(committed.lease.is_none());
}

/// 持久化的扫描错误也必须累计 `consecutive_failures`，达到上限后进入退避，
/// 而不是每次启动都无限重试。
#[tokio::test]
async fn persistent_scan_failures_enter_retry_backoff() {
    let fixture = fixture(true);
    let source = FailingSource;
    let extractor = FixedExtractor {
        draft: ExtractionDraft::default(),
    };
    let consolidator = FixedConsolidator {
        plan: plan_from_draft(&ExtractionDraft::default()),
    };
    for _ in 0..3 {
        let job = ExtractionJob::new(
            &fixture.service,
            Arc::new(state_store(&fixture)),
            &source,
            &extractor,
            &consolidator,
            &fixture.config,
        );
        assert!(
            job.run_at(1_000_000).await.is_err(),
            "scan errors are still surfaced to the caller"
        );
    }
    let state = state_store(&fixture).load().unwrap();
    assert_eq!(state.consecutive_failures, 3);
    assert!(
        state
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("unreadable"))
    );

    let job = ExtractionJob::new(
        &fixture.service,
        Arc::new(state_store(&fixture)),
        &source,
        &extractor,
        &consolidator,
        &fixture.config,
    );
    let outcome = job.run_at(1_000_000).await.unwrap();
    assert_eq!(outcome.status, ExtractionStatus::SkippedRetry);
}

/// claim 成功但随后 load 失败时，lease 必须被释放，而不是泄漏到 TTL。
struct LoadFailingStore {
    released: AtomicUsize,
}

impl ExtractionStateStore for LoadFailingStore {
    fn load(&self) -> kiana_memory::MemoryResult<kiana_memory::ExtractionState> {
        Err(MemoryError::Source("state disappeared".into()))
    }

    fn claim_at(
        &self,
        now_ms: u64,
        owner: &str,
        ttl_ms: u64,
    ) -> kiana_memory::MemoryResult<ClaimOutcome> {
        Ok(ClaimOutcome::Claimed {
            lease: LeaseRecord {
                owner: owner.into(),
                acquired_at_ms: now_ms,
                expires_at_ms: now_ms + ttl_ms,
                attempt: 1,
            },
            revision: 1,
            stale_takeover: false,
        })
    }

    fn release(&self, _owner: &str) -> kiana_memory::MemoryResult<bool> {
        self.released.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }

    fn commit(
        &self,
        _owner: &str,
        _expected_revision: u64,
        _now_ms: u64,
        _commit: StateCommit,
    ) -> kiana_memory::MemoryResult<kiana_memory::ExtractionState> {
        unreachable!("commit must not run when load failed")
    }
}

#[tokio::test]
async fn load_failure_after_claim_releases_the_lease() {
    let fixture = fixture(true);
    let source = FailingSource;
    let extractor = FixedExtractor {
        draft: ExtractionDraft::default(),
    };
    let consolidator = FixedConsolidator {
        plan: plan_from_draft(&ExtractionDraft::default()),
    };
    let store = Arc::new(LoadFailingStore {
        released: AtomicUsize::new(0),
    });
    let job = ExtractionJob::new(
        &fixture.service,
        store.clone(),
        &source,
        &extractor,
        &consolidator,
        &fixture.config,
    );
    assert!(job.run_at(1_000_000).await.is_err());
    assert_eq!(store.released.load(Ordering::SeqCst), 1);
}
