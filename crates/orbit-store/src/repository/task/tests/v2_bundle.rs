// Test bodies migrated from v2_bundle/tests/store.rs (and using shared helpers
// now in sibling test_support.rs under task_store/tests/) per ORB-00247.

use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use chrono::{TimeZone, Utc};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{TASK_ARTIFACT_SCHEMA_VERSION, TaskCommentRowV2, TaskEventRowV2};
use tempfile::TempDir;

use super::super::v2_bundle::*;
use super::test_support::{
    bundle_store, legacy_double_dot_lock_path, lock_entries_for_task, sample_bundle, task_lock_path,
};

#[derive(Debug, PartialEq, Eq)]
enum CreateOutcome {
    Created,
    AlreadyExists,
    Unexpected(String),
}

#[test]
fn create_bundle_retains_the_stable_lock_after_success() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");
    let tasks_dir = bundle_dir.parent().expect("bundle parent");

    let created = store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");

    assert_eq!(created.binding.task_id, "ORB-00000");
    assert!(bundle_dir.is_dir());
    assert_eq!(
        lock_entries_for_task(tasks_dir, "ORB-00000"),
        vec![".ORB-00000.bundle.lock".to_string()]
    );
    assert!(!task_lock_path(&bundle_dir).exists());
    assert!(!legacy_double_dot_lock_path(&bundle_dir, "ORB-00000").exists());
}

#[test]
fn create_bundle_serializes_concurrent_duplicate_creators() {
    let temp = TempDir::new().expect("tempdir");
    let store = Arc::new(bundle_store(&temp));
    let bundle = Arc::new(sample_bundle("ORB-00000"));
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");
    let tasks_dir = bundle_dir.parent().expect("bundle parent").to_path_buf();
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let store = Arc::clone(&store);
            let bundle = Arc::clone(&bundle);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                match store.create_bundle(bundle.as_ref()) {
                    Ok(_) => CreateOutcome::Created,
                    Err(OrbitError::Store(message)) if message.contains("already exists") => {
                        CreateOutcome::AlreadyExists
                    }
                    Err(err) => CreateOutcome::Unexpected(err.to_string()),
                }
            })
        })
        .collect::<Vec<_>>();

    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("join creator"))
        .collect::<Vec<_>>();

    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, CreateOutcome::Created))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, CreateOutcome::AlreadyExists))
            .count(),
        1
    );
    assert!(
        !outcomes
            .iter()
            .any(|outcome| matches!(outcome, CreateOutcome::Unexpected(_))),
        "unexpected outcomes: {outcomes:?}"
    );
    assert!(bundle_dir.is_dir());
    assert_eq!(
        lock_entries_for_task(&tasks_dir, "ORB-00000"),
        vec![".ORB-00000.bundle.lock".to_string()]
    );
}

#[test]
fn bundle_store_lists_registered_bundles_from_registry() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create first bundle");
    store
        .create_bundle(&sample_bundle("ORB-00001"))
        .expect("create second bundle");

    let ids: Vec<_> = store
        .list_bundles()
        .expect("list bundles")
        .into_iter()
        .map(|bundle| bundle.envelope.id)
        .collect();
    assert_eq!(ids, vec!["ORB-00000", "ORB-00001"]);
}

#[test]
fn delete_bundle_removes_canonical_projection_and_registry_rows() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");
    assert!(bundle_dir.is_dir());

    assert!(store.delete_bundle("ORB-00000").expect("delete bundle"));
    assert!(!bundle_dir.exists());
    assert!(
        !store
            .workspace_orbit_dir
            .as_ref()
            .expect("checkout fixture")
            .join("tasks/ORB-00000")
            .exists()
    );
    assert_eq!(
        store
            .registry
            .tasks_for_workspace(&store.workspace_id)
            .expect("registry tasks"),
        Vec::new()
    );
    assert!(matches!(
        store.read_bundle("ORB-00000"),
        Err(OrbitError::NotFound {
            kind: NotFoundKind::Task,
            ..
        })
    ));
    assert!(!store.delete_bundle("ORB-00000").expect("delete missing"));
}

#[test]
fn delete_bundle_unregisters_stale_binding_when_canonical_dir_is_missing() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");
    fs::remove_dir_all(&bundle_dir).expect("remove canonical bundle");

    assert!(store.delete_bundle("ORB-00000").expect("delete stale"));
    assert!(
        fs::symlink_metadata(
            store
                .workspace_orbit_dir
                .as_ref()
                .expect("checkout fixture")
                .join("tasks/ORB-00000")
        )
        .is_err()
    );
    assert_eq!(
        store
            .registry
            .tasks_for_workspace(&store.workspace_id)
            .expect("registry tasks"),
        Vec::new()
    );
}

#[test]
fn rewrite_document_and_append_logs_are_durable() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let now = Utc.with_ymd_and_hms(2026, 5, 11, 12, 30, 0).unwrap();
    store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");

    store
        .rewrite_document(
            "ORB-00000",
            TaskDocumentV2::Description,
            "New description\n",
        )
        .expect("rewrite description");
    store
        .rewrite_document("ORB-00000", TaskDocumentV2::Acceptance, "- [x] Done\n")
        .expect("rewrite acceptance");
    store
        .rewrite_document("ORB-00000", TaskDocumentV2::Plan, "1. Finish\n")
        .expect("rewrite plan");
    store
        .rewrite_document(
            "ORB-00000",
            TaskDocumentV2::ExecutionSummary,
            "Outcome: success\n",
        )
        .expect("rewrite summary");
    store
        .append_event(
            "ORB-00000",
            &TaskEventRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                event_id: "EV-0002".to_string(),
                at: now,
                by: "codex:gpt-5.5".to_string(),
                event_type: "updated".to_string(),
                note: Some("summary written".to_string()),
                from_status: None,
                to_status: None,
            },
        )
        .expect("append event");
    let historical_review_record = r#"[independent-review]
{"candidate_head_sha":"abc123","verdict":"approve","criteria":[{"index":1,"verdict":"met"}]}"#;
    store
        .append_comment(
            "ORB-00000",
            &TaskCommentRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                comment_id: "C-0002".to_string(),
                at: now,
                by: "daniel".to_string(),
                body: historical_review_record.to_string(),
            },
        )
        .expect("append comment");

    let read = store.read_bundle("ORB-00000").expect("read bundle");
    assert_eq!(read.description, "New description\n");
    assert_eq!(read.acceptance, "- [x] Done\n");
    assert_eq!(read.plan, "1. Finish\n");
    assert_eq!(read.execution_summary, "Outcome: success\n");
    assert_eq!(read.events.len(), 2);
    assert_eq!(read.comments.len(), 2);
    assert_eq!(read.comments[1].body, historical_review_record);
}

#[test]
fn create_bundle_cleans_partial_directory_but_retains_lock_on_validation_error() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let mut bundle = sample_bundle("ORB-00000");
    bundle.envelope.title = " ".to_string();
    let bundle_path = store.bundle_path("ORB-00000").expect("bundle path");
    let tasks_dir = bundle_path.parent().expect("bundle parent").to_path_buf();

    assert!(store.create_bundle(&bundle).is_err());
    assert!(!bundle_path.exists());
    assert_eq!(
        lock_entries_for_task(&tasks_dir, "ORB-00000"),
        vec![".ORB-00000.bundle.lock".to_string()]
    );
    assert!(!task_lock_path(&bundle_path).exists());
    assert!(!legacy_double_dot_lock_path(&bundle_path, "ORB-00000").exists());
}

#[test]
fn create_bundle_treats_projection_error_as_degraded_success() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let projection_dir = store
        .workspace_orbit_dir
        .as_ref()
        .expect("checkout fixture")
        .join("tasks");
    fs::create_dir_all(&projection_dir).expect("create projection dir");
    fs::write(projection_dir.join("ORB-00000"), "not a symlink").expect("write blocker");

    let created = store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");

    assert_eq!(created.binding.task_id, "ORB-00000");
    assert!(created.projection.degraded_reason.is_some());
    assert!(store.read_bundle("ORB-00000").is_ok());
    assert_eq!(
        store
            .list_bundles()
            .expect("list bundles")
            .into_iter()
            .map(|bundle| bundle.envelope.id)
            .collect::<Vec<_>>(),
        vec!["ORB-00000"]
    );
}

#[test]
fn deletion_failures_recover_without_registering_partial_tombstones() {
    use crate::workflow::task::reindex_workspace;

    for fault in [
        DeletionFault::Publication,
        DeletionFault::PublicationSync,
        DeletionFault::Registry,
        DeletionFault::Cleanup,
    ] {
        for recover_by_reindex in [false, true] {
            let temp = TempDir::new().unwrap();
            let store = bundle_store(&temp);
            store.create_bundle(&sample_bundle("ORB-00000")).unwrap();
            store.create_bundle(&sample_bundle("ORB-00001")).unwrap();
            let dir = store.bundle_path("ORB-00000").unwrap();
            let tombstone = deletion_path(&dir);
            inject_deletion_fault(fault);
            assert!(store.delete_bundle("ORB-00000").is_err());
            assert_eq!(dir.exists(), fault == DeletionFault::Publication);
            assert_eq!(tombstone.exists(), fault != DeletionFault::Publication);
            let registered = store
                .registry
                .tasks_for_workspace(&store.workspace_id)
                .unwrap();
            assert_eq!(
                registered.len(),
                if fault == DeletionFault::Cleanup {
                    1
                } else {
                    2
                }
            );
            if fault == DeletionFault::Registry {
                // No destructive operation has run before registry removal.
                assert_eq!(
                    fs::read_to_string(tombstone.join("description.md")).unwrap(),
                    "Description body"
                );
                assert!(store.create_bundle(&sample_bundle("ORB-00000")).is_err());
            }
            if fault == DeletionFault::Cleanup {
                // Model a process dying partway through recursive removal.
                fs::remove_file(tombstone.join("description.md")).unwrap();
            }
            if recover_by_reindex {
                if fault != DeletionFault::Publication {
                    // If the same boundary remains unavailable, report failure
                    // while still indexing the healthy neighbor and retaining data.
                    inject_deletion_fault(fault);
                    let error = reindex_workspace(&store.registry, &store.workspace_id)
                        .unwrap_err()
                        .to_string();
                    assert!(error.contains("indexed 1 healthy tasks"), "{error}");
                    assert!(error.contains("ORB-00000"), "{error}");
                    assert!(tombstone.exists());
                    assert_eq!(
                        store
                            .registry
                            .indexed_task_count_for_workspace(&store.workspace_id)
                            .unwrap(),
                        1
                    );
                }
                let result = reindex_workspace(&store.registry, &store.workspace_id).unwrap();
                assert_eq!(
                    result.indexed,
                    if fault == DeletionFault::Publication {
                        2
                    } else {
                        1
                    }
                );
                assert!(reindex_workspace(&store.registry, &store.workspace_id).is_ok());
            } else {
                assert!(store.delete_bundle("ORB-00000").unwrap());
                assert!(!store.delete_bundle("ORB-00000").unwrap());
                assert!(!dir.exists());
            }
            assert!(!tombstone.exists());
            assert!(store.read_bundle("ORB-00001").is_ok());
        }
    }
}

#[test]
fn reindex_retains_unresolved_data_and_repairs_healthy_neighbors() {
    use crate::workflow::task::reindex_workspace;

    let temp = TempDir::new().unwrap();
    let store = bundle_store(&temp);
    for id in ["ORB-00000", "ORB-00001", "ORB-00002"] {
        let bundle = sample_bundle(id);
        store.create_bundle(&bundle).unwrap();
        store
            .registry
            .replace_task_index(&store.workspace_id, &bundle.envelope)
            .unwrap();
    }
    let partial = store.bundle_path("ORB-00000").unwrap();
    fs::remove_file(partial.join("description.md")).unwrap();
    let conflict = store.bundle_path("ORB-00001").unwrap();
    fs::create_dir(deletion_path(&conflict)).unwrap();
    fs::write(deletion_path(&conflict).join("evidence"), "retain me").unwrap();
    let orphan = store.bundle_path("ORB-00003").unwrap();
    fs::create_dir(&orphan).unwrap();
    fs::write(orphan.join("evidence"), "partial create").unwrap();
    store
        .registry
        .unregister_task_bundle("ORB-00002", &store.workspace_id)
        .unwrap();

    for _ in 0..2 {
        let error = reindex_workspace(&store.registry, &store.workspace_id)
            .unwrap_err()
            .to_string();
        assert!(error.contains("indexed 1 healthy tasks"), "{error}");
        for id in ["ORB-00000", "ORB-00001", "ORB-00003"] {
            assert!(error.contains(id), "{error}");
        }
        assert_eq!(
            store
                .registry
                .tasks_for_workspace(&store.workspace_id)
                .unwrap()
                .len(),
            3
        );
        assert!(
            store.list_bundles().is_err(),
            "registered corruption must still fail listing"
        );
        assert_eq!(
            store
                .registry
                .indexed_task_count_for_workspace(&store.workspace_id)
                .unwrap(),
            3,
            "keep unresolved registered index rows while repairing the healthy row"
        );
        assert!(partial.join("task.yaml").exists());
        assert!(conflict.join("task.yaml").exists());
        assert_eq!(
            fs::read_to_string(deletion_path(&conflict).join("evidence")).unwrap(),
            "retain me"
        );
        assert_eq!(
            fs::read_to_string(orphan.join("evidence")).unwrap(),
            "partial create"
        );
        assert!(store.registry.allocator_next_number().unwrap() >= 4);
    }
}

#[test]
fn reindex_registers_forward_relation_targets_before_indexing() {
    use crate::workflow::task::reindex_workspace;
    use orbit_types::task::{TaskRelation, TaskRelationType};

    let temp = TempDir::new().unwrap();
    let store = bundle_store(&temp);
    let mut child = sample_bundle("ORB-00000");
    child.envelope.relations.push(TaskRelation {
        relation_type: TaskRelationType::ChildOf,
        target: "ORB-00001".into(),
    });
    store.create_bundle(&child).unwrap();
    store.create_bundle(&sample_bundle("ORB-00001")).unwrap();
    for id in ["ORB-00000", "ORB-00001"] {
        store
            .registry
            .unregister_task_bundle(id, &store.workspace_id)
            .unwrap();
    }
    assert_eq!(
        reindex_workspace(&store.registry, &store.workspace_id)
            .unwrap()
            .indexed,
        2
    );
    assert_eq!(
        store
            .registry
            .indexed_task_count_for_workspace(&store.workspace_id)
            .unwrap(),
        2
    );
}
