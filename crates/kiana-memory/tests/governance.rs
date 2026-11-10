//! 治理验收：快照冻结、审批 pin、forget 墓碑/级联、审计与权限 gate。

use std::sync::Arc;

use kiana_memory::{
    DenyAllGate, MemoryConfig, MemoryEntryDraft, MemoryError, MemoryScope, MemoryService,
    MemorySession, MemoryStore, MemoryType, ScopePaths, WriteBatch, WriteSource, read_audit,
    read_tombstones, slugify,
};

struct Fixture {
    _dir: tempfile::TempDir,
    service: MemoryService,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let paths = ScopePaths::at_root(MemoryScope::Project, dir.path().join("memory"));
    let store = MemoryStore::open(paths, MemoryConfig::default()).unwrap();
    Fixture {
        _dir: dir,
        service: MemoryService::trusted(store),
    }
}

fn user(name: &str, description: &str, body: &str) -> MemoryEntryDraft {
    MemoryEntryDraft::new(name, description, MemoryType::User, body)
}

#[test]
fn snapshot_is_frozen_mid_session_and_refreshes_after_compaction() {
    let fixture = fixture();
    let mut session = MemorySession::load(fixture.service.clone()).unwrap();
    let before = session.system_prompt_index().to_string();
    assert!(!before.is_empty());

    session
        .apply(
            WriteBatch::new().add(user("fresh", "new entry", "body")),
            WriteSource::Tool,
        )
        .unwrap();

    assert_eq!(session.system_prompt_index(), before);
    let disk = fixture.service.store().load_snapshot().unwrap();
    assert_eq!(disk.revision, 1);
    assert!(disk.index.contains("fresh"));

    let change = session.after_compaction().unwrap();
    assert!(change.changed);
    assert_eq!(change.previous_revision, 0);
    assert_eq!(change.new_revision, 1);
    assert!(session.system_prompt_index().contains("fresh"));
}

#[test]
fn approval_pins_full_entry_bytes_and_rejects_toctou_changes() {
    let fixture = fixture();
    fixture
        .service
        .apply(
            WriteBatch::new().add(user("rule", "the rule", "old rule")),
            WriteSource::Tool,
        )
        .unwrap();
    let id = slugify("rule");

    let staged = fixture
        .service
        .stage_replace(
            &id,
            "old",
            user("rule", "the rule", "new rule"),
            1,
            WriteSource::Tool,
        )
        .unwrap();

    // 审批期间另一个写入改了同一条目。
    fixture
        .service
        .apply(
            WriteBatch::new().replace(id.clone(), user("rule", "the rule", "changed rule")),
            WriteSource::Tool,
        )
        .unwrap();

    let error = fixture.service.approve(&staged, 2).unwrap_err();
    assert!(matches!(error, MemoryError::PinnedEntryChanged { .. }));
    assert_eq!(
        fixture.service.store().read_entry(&id).unwrap().body,
        "changed rule"
    );

    // 未变化时按 pinned 字节应用成功。
    let staged = fixture
        .service
        .stage_replace(
            &id,
            "changed",
            user("rule", "the rule", "approved rule"),
            3,
            WriteSource::Tool,
        )
        .unwrap();
    fixture.service.approve(&staged, 4).unwrap();
    assert_eq!(
        fixture.service.store().read_entry(&id).unwrap().body,
        "approved rule"
    );
}

#[test]
fn staging_is_read_only_and_approval_is_the_gated_write() {
    let fixture = fixture();
    fixture
        .service
        .apply(
            WriteBatch::new().add(user("entry", "desc", "old body")),
            WriteSource::Tool,
        )
        .unwrap();
    let service = MemoryService::new(
        fixture.service.store().clone(),
        Arc::new(DenyAllGate::new("user has not approved")),
    );
    let staged = service
        .stage_replace(
            &slugify("entry"),
            "old",
            user("entry", "desc", "new body"),
            1,
            WriteSource::Tool,
        )
        .unwrap();
    let error = service.approve(&staged, 2).unwrap_err();
    assert!(matches!(error, MemoryError::PermissionDenied { .. }));
    assert_eq!(
        fixture
            .service
            .store()
            .read_entry(&slugify("entry"))
            .unwrap()
            .body,
        "old body"
    );
}

#[test]
fn two_approvals_based_on_the_same_pinned_bytes_cannot_both_apply() {
    let fixture = fixture();
    fixture
        .service
        .apply(
            WriteBatch::new().add(user("rule", "the rule", "pinned body")),
            WriteSource::Tool,
        )
        .unwrap();
    let id = slugify("rule");
    let first = fixture
        .service
        .stage_replace(
            &id,
            "pinned",
            user("rule", "the rule", "first approved body"),
            1,
            WriteSource::Tool,
        )
        .unwrap();
    let second = fixture
        .service
        .stage_replace(
            &id,
            "pinned",
            user("rule", "the rule", "second approved body"),
            2,
            WriteSource::Tool,
        )
        .unwrap();
    fixture.service.approve(&first, 3).unwrap();
    let error = fixture.service.approve(&second, 4).unwrap_err();
    assert!(matches!(error, MemoryError::PinnedEntryChanged { .. }));
    assert_eq!(
        fixture.service.store().read_entry(&id).unwrap().body,
        "first approved body"
    );
}

#[test]
fn forget_writes_body_scrubbed_tombstones_and_cascades_derived_entries() {
    let fixture = fixture();
    let source = "source entry";
    let derived = "derived entry";
    fixture
        .service
        .apply(
            WriteBatch::new()
                .add(user(source, "source", "source body with private detail"))
                .add(MemoryEntryDraft {
                    name: derived.into(),
                    description: "derived".into(),
                    mtype: MemoryType::User,
                    body: "derived body".into(),
                    derived_from: vec![slugify(source)],
                    source_key: None,
                })
                .add(user("unrelated", "keep", "keep body")),
            WriteSource::Tool,
        )
        .unwrap();

    let report = fixture
        .service
        .forget(
            &slugify(source),
            "user asked to forget",
            42,
            WriteSource::User,
        )
        .unwrap();
    assert_eq!(report.removed.len(), 2);
    assert!(
        report
            .removed
            .iter()
            .any(|entry| entry.id == slugify(source) && entry.reason == "user asked to forget")
    );
    assert!(
        report
            .removed
            .iter()
            .any(|entry| entry.id == slugify(derived)
                && entry.reason == format!("derived_from:{}", slugify(source)))
    );

    let remaining = fixture.service.store().list_entries().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].name, "unrelated");

    let tombstones = read_tombstones(&fixture.service.store().paths().tombstone_path).unwrap();
    assert_eq!(tombstones.len(), 2);
    let source_tombstone = tombstones
        .iter()
        .find(|record| record.entry_id == slugify(source))
        .unwrap();
    assert_eq!(source_tombstone.cascaded, vec![slugify(derived)]);
    assert!(!source_tombstone.body_digest.is_empty());
    let serialized = serde_json::to_string(&tombstones).unwrap();
    assert!(!serialized.contains("source body with private detail"));

    let audit = read_audit(&fixture.service.store().paths().audit_path).unwrap();
    let forget_records: Vec<_> = audit
        .iter()
        .filter(|record| record.op == "forget" && record.outcome == "applied")
        .collect();
    assert_eq!(forget_records.len(), 2);
}

#[test]
fn hostile_gate_blocks_write_and_audits_the_rejection() {
    let fixture = fixture();
    let service = MemoryService::new(
        fixture.service.store().clone(),
        Arc::new(DenyAllGate::new("approval missing")),
    );
    let error = service
        .apply(
            WriteBatch::new().add(user("blocked", "desc", "body")),
            WriteSource::Tool,
        )
        .unwrap_err();
    assert!(matches!(error, MemoryError::PermissionDenied { .. }));
    assert!(service.store().list_entries().unwrap().is_empty());
    let audit = read_audit(&service.store().paths().audit_path).unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].outcome, "rejected");
    assert_eq!(audit[0].op, "add");
    assert!(
        audit[0]
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("approval missing")
    );
}

#[test]
fn every_operation_gets_one_audit_record() {
    let fixture = fixture();
    fixture
        .service
        .apply(
            WriteBatch::new()
                .add(user("one", "desc one", "body one"))
                .add(user("two", "desc two", "body two")),
            WriteSource::Tool,
        )
        .unwrap();
    let audit = read_audit(&fixture.service.store().paths().audit_path).unwrap();
    assert_eq!(audit.len(), 2);
    assert!(audit.iter().all(|record| record.outcome == "applied"));
    assert!(audit.iter().all(|record| record.source == "tool"));
    assert_eq!(audit[0].revision, Some(1));
}
