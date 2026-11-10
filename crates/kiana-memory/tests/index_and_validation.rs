//! 索引上限与字段校验的验收测试。

use std::fs;

use kiana_memory::{
    IndexEntry, MemoryConfig, MemoryEntry, MemoryEntryDraft, MemoryError, MemoryScope,
    MemoryService, MemoryStore, MemoryType, ScopePaths, WriteBatch, WriteSource, render_entry_line,
    render_index,
};

fn service_in(dir: &std::path::Path) -> MemoryService {
    let paths = ScopePaths::at_root(MemoryScope::Project, dir.join("memory"));
    let store = MemoryStore::open(paths, MemoryConfig::default()).unwrap();
    MemoryService::trusted(store)
}

#[test]
fn five_hundred_entries_index_stays_within_caps_with_one_warning() {
    let entries: Vec<IndexEntry> = (0..500)
        .map(|index| {
            IndexEntry::new(
                format!("entry-{index}"),
                format!("Entry {index}"),
                "a fairly long description that is still a single line and gets truncated as needed"
                    .to_string(),
            )
        })
        .collect();
    let limits = kiana_memory::MemoryLimits::default();
    let rendered = render_index(&entries, 7, &limits);
    let line_count = rendered.content.lines().count();
    assert!(line_count <= 200, "line count {line_count}");
    assert!(
        rendered.content.len() <= 25_000,
        "byte count {}",
        rendered.content.len()
    );
    let warnings = rendered
        .content
        .lines()
        .filter(|line| line.starts_with("> WARNING:"))
        .count();
    assert_eq!(warnings, 1, "exactly one truncation warning");
    assert!(rendered.dropped > 0);
    for line in rendered.content.lines().skip(2) {
        assert!(
            line.chars().count() <= 200,
            "index line exceeds 200 chars: {}",
            line.chars().count()
        );
    }
    // 截断后的索引仍然可以解析。
    kiana_memory::parse_index(&rendered.content).unwrap();
}

#[test]
fn every_rendered_entry_line_is_bounded_even_for_huge_fields() {
    let line = render_entry_line(
        &IndexEntry::new("id", "n".repeat(400), "d".repeat(400)),
        200,
    );
    assert!(line.chars().count() <= 200);
    assert!(!line.contains('\n'));
}

#[test]
fn feedback_missing_why_returns_field_error_and_leaves_files_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_in(dir.path());
    let paths = service.store().paths().clone();
    let before_index = fs::read(&paths.index_path).ok();
    let before_files: Vec<_> = fs::read_dir(&paths.topics_dir).unwrap().collect();

    let error = service
        .apply(
            WriteBatch::new().add(MemoryEntryDraft::new(
                "no-why",
                "missing why",
                MemoryType::Feedback,
                "Rule: always test",
            )),
            WriteSource::Tool,
        )
        .unwrap_err();
    match error {
        MemoryError::MissingField { field, .. } => assert_eq!(field, "body.why"),
        other => panic!("unexpected error: {other}"),
    }

    assert_eq!(before_index, fs::read(&paths.index_path).ok());
    let after_files: Vec<_> = fs::read_dir(&paths.topics_dir).unwrap().collect();
    assert_eq!(before_files.len(), after_files.len());
    assert!(service.store().list_entries().unwrap().is_empty());
}

#[test]
fn feedback_missing_how_to_apply_returns_field_error_and_leaves_files_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_in(dir.path());
    let paths = service.store().paths().clone();
    let before_index = fs::read(&paths.index_path).ok();

    let error = service
        .apply(
            WriteBatch::new().add(MemoryEntryDraft::new(
                "no-how",
                "missing how",
                MemoryType::Project,
                "Fact: deploy freeze\nWhy: release branch",
            )),
            WriteSource::Tool,
        )
        .unwrap_err();
    match error {
        MemoryError::MissingField { field, .. } => assert_eq!(field, "body.how_to_apply"),
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(before_index, fs::read(&paths.index_path).ok());
    assert!(service.store().list_entries().unwrap().is_empty());
}

#[test]
fn valid_feedback_with_markdown_labels_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_in(dir.path());
    service
        .apply(
            WriteBatch::new().add(MemoryEntryDraft::new(
                "integration-tests",
                "hit a real database",
                MemoryType::Feedback,
                "Integration tests must hit a real database\n\n**Why:** a mocked migration passed but prod failed\n\n**How to apply:** whenever a test touches the migration path",
            )),
            WriteSource::Tool,
        )
        .unwrap();
    let entries = service.store().list_entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].mtype, MemoryType::Feedback);
}

#[test]
fn rebuilding_an_index_from_five_hundred_topic_files_stays_within_caps() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_in(dir.path());
    let paths = service.store().paths().clone();
    for index in 0..500u32 {
        let entry = MemoryEntry {
            id: format!("entry-{index}"),
            name: format!("Entry {index}"),
            description: "a long but single-line description used to exercise the byte budget"
                .into(),
            mtype: MemoryType::User,
            body: "body".into(),
            revision: 1,
            derived_from: Vec::new(),
            source_key: None,
        };
        fs::write(paths.entry_path(&entry.id), entry.to_file_bytes()).unwrap();
    }
    let snapshot = service.rebuild_index(1).unwrap();
    assert!(snapshot.index.lines().count() <= 200);
    assert!(snapshot.index.len() <= 25_000);
    assert_eq!(
        snapshot
            .index
            .lines()
            .filter(|line| line.starts_with("> WARNING:"))
            .count(),
        1
    );
    let on_disk = fs::read_to_string(&paths.index_path).unwrap();
    assert_eq!(on_disk, snapshot.index);
    assert_eq!(
        kiana_memory::parse_index(&on_disk).unwrap().warnings.len(),
        1
    );
}
