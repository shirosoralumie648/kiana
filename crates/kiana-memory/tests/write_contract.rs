//! 写入契约：批次原子性、revision、预算、损坏备份、patch/remove。

use std::fs;
use std::path::Path;

use kiana_memory::{
    MemoryConfig, MemoryEntryDraft, MemoryError, MemoryScope, MemoryService, MemoryStore,
    MemoryType, ScopePaths, WriteBatch, WriteSource,
};

fn service_with_config(dir: &Path, config: MemoryConfig) -> MemoryService {
    let paths = ScopePaths::at_root(MemoryScope::Project, dir.join("memory"));
    let store = MemoryStore::open(paths, config).unwrap();
    MemoryService::trusted(store)
}

fn service(dir: &Path) -> MemoryService {
    service_with_config(dir, MemoryConfig::default())
}

fn user(name: &str, body: &str) -> MemoryEntryDraft {
    MemoryEntryDraft::new(
        name,
        format!("description for {name}"),
        MemoryType::User,
        body,
    )
}

#[test]
fn batch_is_atomic_and_reports_budget_usage() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path());
    let report = service
        .apply(
            WriteBatch::new()
                .add(user("one", "body one"))
                .add(user("two", "body two")),
            WriteSource::Tool,
        )
        .unwrap();
    assert_eq!(report.applied.len(), 2);
    assert_eq!(report.revision, 1);
    assert!(report.usage_ratio > 0.0 && report.usage_ratio < 1.0);
    assert_eq!(
        report.limit_bytes,
        MemoryConfig::default().limits.max_total_bytes
    );
    assert!(report.index_bytes > 0);

    let snapshot = service.store().load_snapshot().unwrap();
    let index_before = fs::read(&service.store().paths().index_path).unwrap();
    let error = service
        .apply(
            WriteBatch::new()
                .add(user("three", "body three"))
                .add(MemoryEntryDraft::new(
                    "bad",
                    "desc",
                    MemoryType::Feedback,
                    "no why",
                )),
            WriteSource::Tool,
        )
        .unwrap_err();
    assert!(matches!(error, MemoryError::MissingField { .. }));
    let after = service.store().load_snapshot().unwrap();
    assert_eq!(snapshot.revision, after.revision);
    assert_eq!(
        index_before,
        fs::read(&service.store().paths().index_path).unwrap()
    );
    assert!(service.store().read_entry("three").is_err());
}

#[test]
fn stale_revision_is_rejected_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path());
    let revision = service.store().load_snapshot().unwrap().revision;
    service
        .apply(
            WriteBatch::new()
                .expected(revision)
                .add(user("first", "body")),
            WriteSource::Tool,
        )
        .unwrap();
    let error = service
        .apply(
            WriteBatch::new()
                .expected(revision)
                .add(user("second", "body")),
            WriteSource::Tool,
        )
        .unwrap_err();
    match error {
        MemoryError::RevisionConflict { expected, actual } => {
            assert_eq!(expected, revision);
            assert_eq!(actual, revision + 1);
        }
        other => panic!("unexpected error: {other}"),
    }
    assert!(service.store().read_entry("second").is_err());
}

#[test]
fn corrupt_index_is_backed_up_and_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path());
    let paths = service.store().paths().clone();
    fs::write(&paths.index_path, b"manual notes that cannot round-trip").unwrap();
    let error = service
        .apply(WriteBatch::new().add(user("x", "body")), WriteSource::Tool)
        .unwrap_err();
    match error {
        MemoryError::CorruptIndex { backup, .. } => {
            let backup = backup.expect("backup path");
            assert_eq!(
                fs::read(backup).unwrap(),
                b"manual notes that cannot round-trip"
            );
        }
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(
        fs::read(&paths.index_path).unwrap(),
        b"manual notes that cannot round-trip"
    );
    assert!(service.store().read_entry("x").is_err());
}

#[test]
fn corrupt_topic_file_is_backed_up_and_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path());
    let paths = service.store().paths().clone();
    fs::write(paths.topics_dir.join("broken.md"), b"no frontmatter here").unwrap();
    let error = service
        .apply(WriteBatch::new().add(user("x", "body")), WriteSource::Tool)
        .unwrap_err();
    match error {
        MemoryError::CorruptEntry { backup, .. } => {
            let backup = backup.expect("backup path");
            assert_eq!(fs::read(backup).unwrap(), b"no frontmatter here");
        }
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(
        fs::read(paths.topics_dir.join("broken.md")).unwrap(),
        b"no frontmatter here"
    );
}

#[test]
fn byte_budget_exceeded_leaves_store_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = MemoryConfig::default();
    config.limits.max_total_bytes = 500;
    let service = service_with_config(dir.path(), config);
    let error = service
        .apply(
            WriteBatch::new().add(user("big", &"x".repeat(2_000))),
            WriteSource::Tool,
        )
        .unwrap_err();
    assert!(matches!(error, MemoryError::BudgetExceeded { .. }));
    assert!(service.store().list_entries().unwrap().is_empty());
}

#[test]
fn patch_requires_unique_span_and_remove_works() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path());
    service
        .apply(
            WriteBatch::new().add(user("entry", "alpha beta gamma")),
            WriteSource::Tool,
        )
        .unwrap();
    let error = service
        .apply(
            WriteBatch::new().patch("entry", "missing", "x"),
            WriteSource::Tool,
        )
        .unwrap_err();
    assert!(matches!(error, MemoryError::NoMatch { .. }));

    let error = service
        .apply(
            WriteBatch::new().patch("entry", "a", "x"),
            WriteSource::Tool,
        )
        .unwrap_err();
    assert!(matches!(error, MemoryError::RepeatedPatch { .. }));

    service
        .apply(
            WriteBatch::new().patch("entry", "beta", "BETA"),
            WriteSource::Tool,
        )
        .unwrap();
    let entry = service.store().read_entry("entry").unwrap();
    assert!(entry.body.contains("alpha BETA gamma"));
    assert_eq!(entry.revision, 2);

    service
        .apply(WriteBatch::new().remove("entry"), WriteSource::Tool)
        .unwrap();
    assert!(service.store().read_entry("entry").is_err());
    assert!(service.store().list_entries().unwrap().is_empty());
}

#[test]
fn remove_then_add_same_slug_in_one_batch_keeps_the_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path());
    service
        .apply(
            WriteBatch::new().add(user("entry", "old body")),
            WriteSource::Tool,
        )
        .unwrap();
    service
        .apply(
            WriteBatch::new()
                .remove("entry")
                .add(user("entry", "new body")),
            WriteSource::Tool,
        )
        .unwrap();
    let entries = service.store().list_entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].body, "new body");
}

#[test]
fn successful_writes_leave_no_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path());
    service
        .apply(
            WriteBatch::new()
                .add(user("one", "body"))
                .add(user("two", "body")),
            WriteSource::Tool,
        )
        .unwrap();
    let mut stack = vec![service.store().paths().root.clone()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(!name.contains(".tmp."), "leftover temp file {name}");
            if entry.path().is_dir() {
                stack.push(entry.path());
            }
        }
    }
}

#[test]
fn manifest_scan_is_capped_at_two_hundred_files() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path());
    let paths = service.store().paths().clone();
    for index in 0..250u32 {
        let entry = kiana_memory::MemoryEntry {
            id: format!("entry-{index}"),
            name: format!("Entry {index}"),
            description: format!("description {index}"),
            mtype: MemoryType::User,
            body: "body".into(),
            revision: 1,
            derived_from: Vec::new(),
            source_key: None,
        };
        fs::write(paths.entry_path(&entry.id), entry.to_file_bytes()).unwrap();
    }
    let manifest = service.store().scan_manifest().unwrap();
    assert_eq!(manifest.scanned_files, 250);
    assert_eq!(manifest.entries.len(), 200);
    assert!(manifest.truncated);
    assert!(
        manifest
            .entries
            .iter()
            .all(|entry| entry.description.starts_with("description"))
    );
}
