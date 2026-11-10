//! `test/session-checkpoints-migrations.test.ts` 的端口（节选）。

mod support;

use kiana_durable::{
    BoxError, DocDefinition, DocTarget, DocToken, DocumentBaseKind, DocumentContentKind,
    DocumentSemantics, DocumentSemanticsError, JsonObject, StorageWrite,
};
use serde_json::json;
use support::*;

fn session_doc(
    kind: &str,
    version: i64,
    initial: impl Fn() -> JsonObject + Send + Sync + 'static,
) -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new(kind, version, DocumentSemantics::SESSION)
            .with_initial(move |_| initial()),
    )
}

fn object(value: serde_json::Value) -> JsonObject {
    serde_json::from_value(value).expect("a JSON object")
}

fn document_changes_of(
    writes: &[StorageWrite],
) -> Vec<(DocumentContentKind, i64, Option<JsonObject>)> {
    writes
        .iter()
        .filter_map(|write| match write {
            StorageWrite::DocumentChange { content, .. } => {
                Some((content.kind, content.version, content.value.clone()))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn calls_checkpoint_when_with_its_definition_value() {
    let doc = kiana_durable::define_doc(
        DocDefinition::new("checkpoint.receiver", 1, DocumentSemantics::SESSION)
            .with_initial(|_| object(json!({ "count": 0 })))
            .with_checkpoint_when(|value, _ops, _info| {
                value.get("count").and_then(|count| count.as_i64()) == Some(2)
            }),
    );
    let (storage, session, _publications) = open_test_session();
    for count in 0..=2 {
        let target = DocTarget::session(&doc);
        commit(&session, move |tx| {
            let target = target.clone();
            Box::pin(async move {
                tx.doc(&target)
                    .await?
                    .insert("count".to_owned(), json!(count));
                Ok(())
            })
        })
        .await
        .expect("the commit succeeds");
    }
    let kinds: Vec<DocumentContentKind> = support::lock(&storage.commits)
        .iter()
        .flat_map(|writes| {
            writes.iter().filter_map(|write| match write {
                StorageWrite::DocumentChange { content, .. } => Some(content.kind),
                _ => None,
            })
        })
        .collect();
    assert_eq!(
        kinds,
        [DocumentContentKind::Delta, DocumentContentKind::Base]
    );
}

#[tokio::test]
async fn selects_bases_only_for_nonempty_ordinary_batches_and_passes_the_prepared_revision() {
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<JsonObject>::new()));
    let false_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<JsonObject>::new()));
    let calls_for_predicate = std::sync::Arc::clone(&calls);
    let base_doc = kiana_durable::define_doc(
        DocDefinition::new("checkpoint.base", 1, DocumentSemantics::SESSION)
            .with_initial(|_| object(json!({ "items": [] })))
            .with_checkpoint_when(move |value, _ops, _info| {
                support::lock(&calls_for_predicate).push(value.clone());
                true
            }),
    );
    let false_for_predicate = std::sync::Arc::clone(&false_calls);
    let delta_doc = kiana_durable::define_doc(
        DocDefinition::new("checkpoint.delta", 1, DocumentSemantics::SESSION)
            .with_initial(|_| object(json!({ "count": 0 })))
            .with_checkpoint_when(move |value, _ops, _info| {
                support::lock(&false_for_predicate).push(value.clone());
                false
            }),
    );
    let default_doc = session_doc("checkpoint.default", 1, || object(json!({ "count": 0 })));

    let (storage, session, publications) = open_test_session();
    let (base_target, delta_target, default_target) = (
        DocTarget::session(&base_doc),
        DocTarget::session(&delta_doc),
        DocTarget::session(&default_doc),
    );
    commit(&session, move |tx| {
        let (base_target, delta_target, default_target) = (
            base_target.clone(),
            delta_target.clone(),
            default_target.clone(),
        );
        Box::pin(async move {
            tx.doc(&base_target).await?;
            tx.doc(&delta_target).await?;
            tx.doc(&default_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the creation commits");
    assert!(support::lock(&calls).is_empty());
    assert!(support::lock(&false_calls).is_empty());
    let created = support::lock(&storage.commits)
        .last()
        .cloned()
        .expect("the creation reached storage");
    for write in &created {
        if let StorageWrite::DocumentCreate { content, .. } = write {
            assert_eq!(content.kind, DocumentBaseKind::Base);
        }
    }

    let (base_target, delta_target, default_target) = (
        DocTarget::session(&base_doc),
        DocTarget::session(&delta_doc),
        DocTarget::session(&default_doc),
    );
    commit(&session, move |tx| {
        let (base_target, delta_target, default_target) = (
            base_target.clone(),
            delta_target.clone(),
            default_target.clone(),
        );
        Box::pin(async move {
            let mut base = tx.doc(&base_target).await?;
            base.entry("items".to_owned())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .expect("items is an array")
                .push(json!("x"));
            let mut delta = tx.doc(&delta_target).await?;
            let count = delta
                .get("count")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            delta.insert("count".to_owned(), json!(count + 1));
            let mut default = tx.doc(&default_target).await?;
            let count = default
                .get("count")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            default.insert("count".to_owned(), json!(count + 1));
            Ok(())
        })
    })
    .await
    .expect("the changes commit");

    let admitted = support::lock(&storage.admitted_commits)
        .last()
        .cloned()
        .expect("the changes reached storage");
    let changes = document_changes_of(&admitted);
    assert_eq!(
        changes.iter().map(|(kind, _, _)| *kind).collect::<Vec<_>>(),
        [
            DocumentContentKind::Base,
            DocumentContentKind::Delta,
            DocumentContentKind::Delta,
        ]
    );
    assert_eq!(support::lock(&calls).len(), 1);
    assert_eq!(support::lock(&false_calls).len(), 1);
    let base_snapshot = snapshot_target(&session, &DocTarget::session(&base_doc))
        .await
        .unwrap()
        .expect("the base document exists");
    assert_eq!(support::lock(&calls)[0], base_snapshot);
    assert_eq!(changes[0].2, Some(base_snapshot));

    let publication = support::lock(&publications)
        .last()
        .cloned()
        .expect("the changes published");
    let documents = document_changes(&publication);
    assert_eq!(documents.len(), 3);
    let published_base = match &documents[0] {
        kiana_durable::DocumentCommitChange::Document { value, .. } => value.clone(),
        _ => panic!("expected a document"),
    };
    assert_eq!(published_base, changes[0].2);
}

#[tokio::test]
async fn passes_the_stored_delta_count_since_the_newest_base_including_after_unload() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
    let seen_for_v1 = std::sync::Arc::clone(&seen);
    let v1 = kiana_durable::define_doc(
        DocDefinition::new(
            "checkpoint.deltas-since-base",
            1,
            DocumentSemantics::SESSION,
        )
        .with_initial(|_| object(json!({ "count": 0 })))
        .with_checkpoint_when(move |_value, _ops, info| {
            support::lock(&seen_for_v1).push(info.deltas_since_base);
            info.deltas_since_base >= 2
        }),
    );
    let seen_for_v2 = std::sync::Arc::clone(&seen);
    let v2 = kiana_durable::define_doc(
        DocDefinition::new(
            "checkpoint.deltas-since-base",
            2,
            DocumentSemantics::SESSION,
        )
        .with_initial(|_| object(json!({ "count": 0 })))
        .with_migrate(|value, _from| value)
        .with_checkpoint_when(move |_value, _ops, info| {
            support::lock(&seen_for_v2).push(info.deltas_since_base);
            false
        }),
    );
    let (storage, session, _publications) = open_test_session();
    let create_target = DocTarget::session(&v1);
    commit(&session, move |tx| {
        Box::pin(async move {
            tx.doc(&create_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the document is created");
    for _ in 0..3 {
        let target = DocTarget::session(&v1);
        commit(&session, move |tx| {
            Box::pin(async move {
                let mut draft = tx.doc(&target).await?;
                let count = draft
                    .get("count")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0);
                draft.insert("count".to_owned(), json!(count + 1));
                Ok(())
            })
        })
        .await
        .expect("the increment commits");
    }
    session.unload_documents().await.expect("unload succeeds");
    let post_target = DocTarget::session(&v1);
    commit(&session, move |tx| {
        Box::pin(async move {
            let mut draft = tx.doc(&post_target).await?;
            let count = draft
                .get("count")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            draft.insert("count".to_owned(), json!(count + 1));
            Ok(())
        })
    })
    .await
    .expect("the post-unload increment commits");
    assert_eq!(*support::lock(&seen), [0, 1, 2, 0]);
    let kinds: Vec<DocumentContentKind> = {
        let commits = support::lock(&storage.commits);
        commits[commits.len() - 4..]
            .iter()
            .map(|writes| match &writes[0] {
                StorageWrite::DocumentChange { content, .. } => content.kind,
                _ => panic!("expected a document change"),
            })
            .collect()
    };
    assert_eq!(
        kinds,
        [
            DocumentContentKind::Delta,
            DocumentContentKind::Delta,
            DocumentContentKind::Base,
            DocumentContentKind::Delta,
        ]
    );

    // 需要的版本 base 重置计数，且不调用谓词。
    for _ in 0..2 {
        let target = DocTarget::session(&v2);
        commit(&session, move |tx| {
            Box::pin(async move {
                let mut draft = tx.doc(&target).await?;
                let count = draft
                    .get("count")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0);
                draft.insert("count".to_owned(), json!(count + 1));
                Ok(())
            })
        })
        .await
        .expect("the version-2 increment commits");
    }
    assert_eq!(*support::lock(&seen), [0, 1, 2, 0, 0]);
    {
        let commits = support::lock(&storage.commits);
        let second_last = &commits[commits.len() - 2][0];
        match second_last {
            StorageWrite::DocumentChange { content, .. } => {
                assert_eq!(content.kind, DocumentContentKind::Base);
                assert_eq!(content.version, 2);
            }
            _ => panic!("expected a document change"),
        }
    }
}

#[tokio::test]
async fn rolls_back_every_prepared_document_when_a_checkpoint_predicate_throws() {
    let exploding = kiana_durable::define_doc(
        DocDefinition::new("checkpoint.explode", 1, DocumentSemantics::SESSION)
            .with_initial(|_| object(json!({ "count": 0 })))
            .with_checkpoint_when(|_value, _ops, _info| panic!("checkpoint failed")),
    );
    let stable = session_doc("checkpoint.stable", 1, || object(json!({ "count": 0 })));
    let (storage, session, _publications) = open_test_session();
    let (exploding_target, stable_target) =
        (DocTarget::session(&exploding), DocTarget::session(&stable));
    commit(&session, move |tx| {
        let (exploding_target, stable_target) = (exploding_target.clone(), stable_target.clone());
        Box::pin(async move {
            tx.doc(&exploding_target).await?;
            tx.doc(&stable_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the creation commits");
    let commits = support::lock(&storage.commits).len();
    let (exploding_target, stable_target) =
        (DocTarget::session(&exploding), DocTarget::session(&stable));
    let error = commit(&session, move |tx| {
        let (exploding_target, stable_target) = (exploding_target.clone(), stable_target.clone());
        Box::pin(async move {
            let mut exploding = tx.doc(&exploding_target).await?;
            exploding.insert("count".to_owned(), json!(1));
            let mut stable = tx.doc(&stable_target).await?;
            stable.insert("count".to_owned(), json!(1));
            Ok(())
        })
    })
    .await
    .expect_err("the checkpoint failure aborts the commit");
    assert!(error.to_string().contains("checkpoint failed"));
    assert_eq!(support::lock(&storage.commits).len(), commits);
    assert_eq!(
        snapshot_target(&session, &DocTarget::session(&exploding))
            .await
            .unwrap(),
        Some(object(json!({ "count": 0 })))
    );
    // 之后仍可用。
    let stable_target = DocTarget::session(&stable);
    commit(&session, move |tx| {
        Box::pin(async move {
            let mut draft = tx.doc(&stable_target).await?;
            draft.insert("count".to_owned(), json!(2));
            Ok(())
        })
    })
    .await
    .expect("the Session remains usable");
    assert_eq!(
        snapshot_target(&session, &DocTarget::session(&stable))
            .await
            .unwrap(),
        Some(object(json!({ "count": 2 })))
    );
}

#[tokio::test]
async fn migrates_read_only_once_per_cold_load_and_writes_nothing() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let old = session_doc("migration.read-only", 1, || object(json!({ "count": 2 })));
    let calls_for_migrate = std::sync::Arc::clone(&calls);
    let current = kiana_durable::define_doc(
        DocDefinition::new("migration.read-only", 3, DocumentSemantics::SESSION)
            .with_initial(|_| object(json!({ "count": 0, "labels": [] })))
            .with_migrate(move |value, from_version| {
                assert_eq!(from_version, 1);
                calls_for_migrate.fetch_add(1, Ordering::SeqCst);
                let mut migrated = JsonObject::new();
                migrated.insert(
                    "count".to_owned(),
                    value.get("count").cloned().unwrap_or(json!(0)),
                );
                migrated.insert("labels".to_owned(), json!(["migrated"]));
                migrated
            }),
    );
    let (storage, session, _publications) = open_test_session();
    let old_target = DocTarget::session(&old);
    commit(&session, move |tx| {
        Box::pin(async move {
            tx.doc(&old_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the old document is created");
    session.unload_documents().await.expect("unload succeeds");
    let commits = support::lock(&storage.commits).len();

    let first = snapshot_target(&session, &DocTarget::session(&current))
        .await
        .unwrap()
        .expect("the migrated document exists");
    assert_eq!(first, object(json!({ "count": 2, "labels": ["migrated"] })));
    let second = snapshot_target(&session, &DocTarget::session(&current))
        .await
        .unwrap()
        .expect("the migrated document exists");
    assert_eq!(second, first);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(support::lock(&storage.commits).len(), commits);

    session.unload_documents().await.expect("unload succeeds");
    let third = snapshot_target(&session, &DocTarget::session(&current))
        .await
        .unwrap()
        .expect("the migrated document exists");
    assert_eq!(third, first);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(support::lock(&storage.commits).len(), commits);
}

#[tokio::test]
async fn writes_the_required_base_on_the_first_successful_transaction_then_deltas() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let old = session_doc("migration.transition", 1, || object(json!({ "count": 4 })));
    let checkpoints = std::sync::Arc::new(AtomicUsize::new(0));
    let checkpoints_for_predicate = std::sync::Arc::clone(&checkpoints);
    let current = kiana_durable::define_doc(
        DocDefinition::new("migration.transition", 3, DocumentSemantics::SESSION)
            .with_initial(|_| object(json!({ "count": 0 })))
            .with_migrate(|value, from_version| {
                let count = value
                    .get("count")
                    .and_then(|count| count.as_i64())
                    .unwrap_or(0);
                let mut migrated = JsonObject::new();
                migrated.insert("count".to_owned(), json!(count + from_version - 1));
                migrated
            })
            .with_checkpoint_when(move |_value, _ops, _info| {
                checkpoints_for_predicate.fetch_add(1, Ordering::SeqCst);
                false
            }),
    );
    let (storage, session, _publications) = open_test_session();
    let old_target = DocTarget::session(&old);
    commit(&session, move |tx| {
        Box::pin(async move {
            tx.doc(&old_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the old document is created");
    session.unload_documents().await.expect("unload succeeds");
    assert_eq!(
        snapshot_target(&session, &DocTarget::session(&current))
            .await
            .unwrap(),
        Some(object(json!({ "count": 4 })))
    );
    let current_target = DocTarget::session(&current);
    commit(&session, move |tx| {
        Box::pin(async move {
            tx.doc(&current_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the migration-only transaction commits");
    let writes = support::lock(&storage.commits)
        .last()
        .cloned()
        .expect("the migration base reached storage");
    match &writes[0] {
        StorageWrite::DocumentChange { content, .. } => {
            assert_eq!(content.kind, DocumentContentKind::Base);
            assert_eq!(content.version, 3);
            assert_eq!(content.value, Some(object(json!({ "count": 4 }))));
        }
        _ => panic!("expected a document change"),
    }
    assert_eq!(checkpoints.load(Ordering::SeqCst), 0);

    let current_target = DocTarget::session(&current);
    commit(&session, move |tx| {
        Box::pin(async move {
            let mut draft = tx.doc(&current_target).await?;
            draft.insert("count".to_owned(), json!(7));
            Ok(())
        })
    })
    .await
    .expect("the edit commits");
    let writes = support::lock(&storage.commits)
        .last()
        .cloned()
        .expect("the delta reached storage");
    match &writes[0] {
        StorageWrite::DocumentChange { content, .. } => {
            assert_eq!(content.kind, DocumentContentKind::Delta);
            assert_eq!(content.version, 3);
        }
        _ => panic!("expected a document change"),
    }
    assert_eq!(checkpoints.load(Ordering::SeqCst), 1);
    session.unload_documents().await.expect("unload succeeds");
    assert_eq!(
        snapshot_target(&session, &DocTarget::session(&current))
            .await
            .unwrap(),
        Some(object(json!({ "count": 7 })))
    );
}

#[tokio::test]
async fn rejects_newer_stored_versions_and_older_versions_without_migration() {
    let v2 = session_doc("migration.compatibility", 2, || {
        object(json!({ "count": 2 }))
    });
    let v1 = session_doc("migration.compatibility", 1, || {
        object(json!({ "count": 1 }))
    });
    let v3_without_migration = session_doc("migration.compatibility", 3, || {
        object(json!({ "count": 3 }))
    });
    let (storage, session, _publications) = open_test_session();
    let v2_target = DocTarget::session(&v2);
    commit(&session, move |tx| {
        Box::pin(async move {
            tx.doc(&v2_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the version-2 document is created");
    session.unload_documents().await.expect("unload succeeds");
    let commits = support::lock(&storage.commits).len();

    let error = snapshot_target(&session, &DocTarget::session(&v1))
        .await
        .expect_err("a newer stored version is rejected");
    assert!(error.to_string().contains("newer version 2 than 1"));
    let v1_target = DocTarget::session(&v1);
    let error = commit(&session, move |tx| {
        Box::pin(async move {
            tx.doc(&v1_target).await?;
            Ok(())
        })
    })
    .await
    .expect_err("a newer stored version is rejected in a transaction");
    assert!(error.to_string().contains("newer version 2 than 1"));
    let error = snapshot_target(&session, &DocTarget::session(&v3_without_migration))
        .await
        .expect_err("a missing migration is rejected");
    assert!(
        error
            .to_string()
            .contains("requires migration from version 2")
    );
    let v3_target = DocTarget::session(&v3_without_migration);
    let error = commit(&session, move |tx| {
        Box::pin(async move {
            tx.doc(&v3_target).await?;
            Ok(())
        })
    })
    .await
    .expect_err("a missing migration is rejected in a transaction");
    assert!(
        error
            .to_string()
            .contains("requires migration from version 2")
    );
    assert_eq!(support::lock(&storage.commits).len(), commits);
}

#[allow(dead_code)]
fn _error_types(error: DocumentSemanticsError) -> BoxError {
    Box::new(error)
}

// ---------------------------------------------------------------------------
// 跨定义版本的 tracker 缓存
// ---------------------------------------------------------------------------

fn versioned_v1() -> DocToken {
    session_doc("cache.versioned", 1, || object(json!({ "name": "first" })))
}

fn versioned_v2() -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new("cache.versioned", 2, DocumentSemantics::SESSION)
            .with_initial(|_| object(json!({ "names": [] })))
            .with_migrate(|value, from_version| {
                assert_eq!(from_version, 1);
                let mut migrated = JsonObject::new();
                migrated.insert(
                    "names".to_owned(),
                    json!([value.get("name").cloned().unwrap_or(json!(""))]),
                );
                migrated
            }),
    )
}

#[tokio::test]
async fn migrates_a_document_cached_by_an_older_token_without_unloading() {
    let (storage, session, _publications) = open_test_session();
    let v1_target = DocTarget::session(&versioned_v1());
    commit(&session, move |tx| {
        Box::pin(async move {
            tx.doc(&v1_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the version-1 document is created");
    assert_eq!(
        snapshot_target(&session, &DocTarget::session(&versioned_v1()))
            .await
            .unwrap(),
        Some(object(json!({ "name": "first" })))
    );
    // 仍然缓存着，却被更新版本的 token 访问。
    assert_eq!(
        snapshot_target(&session, &DocTarget::session(&versioned_v2()))
            .await
            .unwrap(),
        Some(object(json!({ "names": ["first"] })))
    );
    let v2_target = DocTarget::session(&versioned_v2());
    commit(&session, move |tx| {
        Box::pin(async move {
            let mut draft = tx.doc(&v2_target).await?;
            draft
                .entry("names".to_owned())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .expect("names is an array")
                .push(json!("second"));
            Ok(())
        })
    })
    .await
    .expect("the version-2 edit commits");
    let writes = support::lock(&storage.commits)
        .last()
        .cloned()
        .expect("the edit reached storage");
    match &writes[0] {
        StorageWrite::DocumentChange { content, .. } => {
            assert_eq!(content.version, 2);
            assert_eq!(content.kind, DocumentContentKind::Base);
            assert_eq!(
                content.value,
                Some(object(json!({ "names": ["first", "second"] })))
            );
        }
        _ => panic!("expected a document change"),
    }
    let error = snapshot_target(&session, &DocTarget::session(&versioned_v1()))
        .await
        .expect_err("the older token sees the newer stored version");
    assert!(
        error.to_string().contains("newer version 2"),
        "unexpected: {error}"
    );
}

#[tokio::test]
async fn serves_an_older_token_from_storage_after_a_newer_token_migrated_in_memory() {
    let (storage, session, _publications) = open_test_session();
    let v1_target = DocTarget::session(&versioned_v1());
    commit(&session, move |tx| {
        Box::pin(async move {
            tx.doc(&v1_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the version-1 document is created");
    session.unload_documents().await.expect("unload succeeds");
    let commits = support::lock(&storage.commits).len();
    assert_eq!(
        snapshot_target(&session, &DocTarget::session(&versioned_v2()))
            .await
            .unwrap(),
        Some(object(json!({ "names": ["first"] })))
    );
    assert_eq!(support::lock(&storage.commits).len(), commits);
    assert_eq!(
        snapshot_target(&session, &DocTarget::session(&versioned_v1()))
            .await
            .unwrap(),
        Some(object(json!({ "name": "first" }))),
        "the older token is served from Storage"
    );
    let v1_target = DocTarget::session(&versioned_v1());
    commit(&session, move |tx| {
        Box::pin(async move {
            let mut draft = tx.doc(&v1_target).await?;
            draft.insert("name".to_owned(), json!("renamed"));
            Ok(())
        })
    })
    .await
    .expect("the version-1 edit commits");
    assert_eq!(
        snapshot_target(&session, &DocTarget::session(&versioned_v2()))
            .await
            .unwrap(),
        Some(object(json!({ "names": ["renamed"] })))
    );
}
