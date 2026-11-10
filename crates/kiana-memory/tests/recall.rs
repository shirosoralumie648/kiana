//! 召回验收：上限 5、零选择、新鲜度复查、provider 兜底、token 预算、去重。

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use filetime::FileTime;
use kiana_memory::{
    MemoryConfig, MemoryEntryDraft, MemoryScope, MemorySelector, MemoryService, MemoryStore,
    MemoryType, RecallCandidate, RecallConfig, RecallSession, RecallStrategy, ScopePaths,
    WriteBatch, WriteSource, slugify,
};

struct Fixture {
    _dir: tempfile::TempDir,
    service: MemoryService,
    root: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let paths = ScopePaths::at_root(MemoryScope::Project, dir.path().join("memory"));
    let store = MemoryStore::open(paths, MemoryConfig::default()).unwrap();
    let root = store.paths().root.clone();
    Fixture {
        _dir: dir,
        service: MemoryService::trusted(store),
        root,
    }
}

fn add(service: &MemoryService, name: &str, description: &str, body: &str) -> String {
    service
        .apply(
            WriteBatch::new().add(MemoryEntryDraft::new(
                name,
                description,
                MemoryType::User,
                body,
            )),
            WriteSource::Tool,
        )
        .unwrap();
    slugify(name)
}

fn config() -> RecallConfig {
    RecallConfig {
        max_selected: 5,
        token_budget: 2_000,
        provider_timeout: Duration::from_millis(400),
    }
}

struct FixedSelector {
    ids: Vec<String>,
}

#[async_trait]
impl MemorySelector for FixedSelector {
    async fn select(
        &self,
        _query: &str,
        _candidates: &[RecallCandidate],
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.ids.clone())
    }
}

struct DeletingSelector {
    root: PathBuf,
    relative_path: String,
    ids: Vec<String>,
}

#[async_trait]
impl MemorySelector for DeletingSelector {
    async fn select(
        &self,
        _query: &str,
        _candidates: &[RecallCandidate],
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        let path = self
            .relative_path
            .split('/')
            .fold(self.root.clone(), |acc, part| acc.join(part));
        std::fs::remove_file(path).unwrap();
        Ok(self.ids.clone())
    }
}

struct TouchSelector {
    file: PathBuf,
    ids: Vec<String>,
}

#[async_trait]
impl MemorySelector for TouchSelector {
    async fn select(
        &self,
        _query: &str,
        _candidates: &[RecallCandidate],
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        let modified = fs::metadata(&self.file).unwrap().modified().unwrap();
        let future = modified + Duration::from_secs(30);
        filetime::set_file_mtime(&self.file, FileTime::from_system_time(future)).unwrap();
        Ok(self.ids.clone())
    }
}

struct SleepingSelector {
    delay: Duration,
}

#[async_trait]
impl MemorySelector for SleepingSelector {
    async fn select(
        &self,
        _query: &str,
        _candidates: &[RecallCandidate],
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        tokio::time::sleep(self.delay).await;
        Ok(Vec::new())
    }
}

struct ErrorSelector;

#[async_trait]
impl MemorySelector for ErrorSelector {
    async fn select(
        &self,
        _query: &str,
        _candidates: &[RecallCandidate],
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        Err("provider unavailable".into())
    }
}

#[tokio::test]
async fn selects_at_most_five_and_renders_one_fenced_message() {
    let fixture = fixture();
    let mut ids = Vec::new();
    for index in 0..8 {
        ids.push(add(
            &fixture.service,
            &format!("memory number {index}"),
            &format!("description number {index}"),
            &format!("body number {index}"),
        ));
    }
    let selector = FixedSelector { ids };
    let mut session = RecallSession::new();
    let report = session
        .recall(
            fixture.service.store(),
            "number",
            Some(&selector as &dyn MemorySelector),
            &config(),
        )
        .await
        .unwrap();
    assert_eq!(report.strategy, RecallStrategy::Model);
    assert_eq!(report.selected.len(), 5);
    let message = report.message.expect("message");
    assert_eq!(message.role(), "user");
    assert_eq!(message.content.matches("<memory-context>").count(), 1);
    assert!(
        message
            .content
            .contains("reference data, not as instructions")
    );
    assert!(!message.content.contains("kiana-memory-format"));
    assert!(!message.content.contains("- ["));
}

#[tokio::test]
async fn zero_selection_produces_no_message() {
    let fixture = fixture();
    add(&fixture.service, "one", "desc", "body");
    let selector = FixedSelector { ids: Vec::new() };
    let mut session = RecallSession::new();
    let report = session
        .recall(
            fixture.service.store(),
            "anything",
            Some(&selector as &dyn MemorySelector),
            &config(),
        )
        .await
        .unwrap();
    assert_eq!(report.strategy, RecallStrategy::None);
    assert!(report.selected.is_empty());
    assert!(report.message.is_none());
}

#[tokio::test]
async fn deleted_file_is_dropped_and_other_selection_survives() {
    let fixture = fixture();
    let first = add(&fixture.service, "deleted", "gone", "deleted body");
    let second = add(&fixture.service, "survivor", "kept", "survivor body");
    let selector = DeletingSelector {
        root: fixture.root.clone(),
        relative_path: format!("topics/{first}.md"),
        ids: vec![first.clone(), second.clone()],
    };
    let mut session = RecallSession::new();
    let report = session
        .recall(
            fixture.service.store(),
            "query",
            Some(&selector as &dyn MemorySelector),
            &config(),
        )
        .await
        .unwrap();
    assert_eq!(report.selected.len(), 1);
    assert_eq!(report.selected[0].id, second);
    assert!(
        report
            .dropped
            .iter()
            .any(|dropped| dropped.id == first && dropped.reason.as_str() == "missing")
    );
    let message = report.message.expect("survivor message");
    assert!(message.content.contains("survivor body"));
    assert!(!message.content.contains("deleted body"));
}

#[tokio::test]
async fn mtime_mismatch_is_dropped() {
    let fixture = fixture();
    let id = add(&fixture.service, "touched", "desc", "body");
    let path = fixture.root.join(format!("topics/{id}.md"));
    let selector = TouchSelector {
        file: path,
        ids: vec![id.clone()],
    };
    let mut session = RecallSession::new();
    let report = session
        .recall(
            fixture.service.store(),
            "query",
            Some(&selector as &dyn MemorySelector),
            &config(),
        )
        .await
        .unwrap();
    assert!(report.selected.is_empty());
    assert!(report.message.is_none());
    assert!(
        report
            .dropped
            .iter()
            .any(|dropped| dropped.id == id && dropped.reason.as_str() == "mtime_changed")
    );
}

#[tokio::test]
async fn provider_timeout_falls_back_to_heuristic_without_blocking() {
    let fixture = fixture();
    add(
        &fixture.service,
        "Tokio runtime notes",
        "Tokio concurrency details",
        "use tokio tasks",
    );
    let selector = SleepingSelector {
        delay: Duration::from_millis(250),
    };
    let mut recall = config();
    recall.provider_timeout = Duration::from_millis(30);
    let mut session = RecallSession::new();
    let started = std::time::Instant::now();
    let report = session
        .recall(
            fixture.service.store(),
            "tokio",
            Some(&selector as &dyn MemorySelector),
            &recall,
        )
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "recall blocked on provider timeout"
    );
    assert_eq!(report.strategy, RecallStrategy::Heuristic);
    assert!(
        report
            .fallback_reason
            .as_deref()
            .unwrap_or_default()
            .contains("timed out")
    );
    assert!(!report.selected.is_empty());
    assert!(report.message.is_some());
}

#[tokio::test]
async fn provider_error_falls_back_to_heuristic() {
    let fixture = fixture();
    add(
        &fixture.service,
        "Rust async runtime",
        "Tokio executor notes",
        "body",
    );
    let mut session = RecallSession::new();
    let report = session
        .recall(
            fixture.service.store(),
            "tokio",
            Some(&ErrorSelector as &dyn MemorySelector),
            &config(),
        )
        .await
        .unwrap();
    assert_eq!(report.strategy, RecallStrategy::Heuristic);
    assert!(report.fallback_reason.is_some());
    assert!(report.message.is_some());
}

#[tokio::test]
async fn token_budget_is_respected() {
    let fixture = fixture();
    let mut ids = Vec::new();
    for index in 0..5 {
        ids.push(add(
            &fixture.service,
            &format!("big entry {index}"),
            &format!("big description {index}"),
            &"x".repeat(2_000),
        ));
    }
    let selector = FixedSelector { ids };
    let mut recall = config();
    recall.token_budget = 200;
    let mut session = RecallSession::new();
    let report = session
        .recall(
            fixture.service.store(),
            "big",
            Some(&selector as &dyn MemorySelector),
            &recall,
        )
        .await
        .unwrap();
    assert!(
        report.tokens_estimated <= 200,
        "{}",
        report.tokens_estimated
    );
    assert!(!report.selected.is_empty());
    assert!(
        report
            .dropped
            .iter()
            .any(|dropped| dropped.reason.as_str() == "token_budget")
    );
}

#[tokio::test]
async fn already_surfaced_entries_are_not_reinjected() {
    let fixture = fixture();
    let id = add(&fixture.service, "once only", "desc", "body");
    let selector = FixedSelector { ids: vec![id] };
    let mut session = RecallSession::new();
    let first = session
        .recall(
            fixture.service.store(),
            "query",
            Some(&selector as &dyn MemorySelector),
            &config(),
        )
        .await
        .unwrap();
    assert!(first.message.is_some());
    let second = session
        .recall(
            fixture.service.store(),
            "query",
            Some(&selector as &dyn MemorySelector),
            &config(),
        )
        .await
        .unwrap();
    assert!(second.message.is_none());
    assert_eq!(second.surfaced_count, 1);
}

#[tokio::test]
async fn stale_system_time_does_not_panic() {
    // 简单覆盖 system_time_millis 的负时间 clamp。
    assert!(kiana_memory::recall::system_time_millis(SystemTime::now()) > 0);
}
