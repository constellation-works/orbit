//! Concurrent-mutation races against `reindex_workspace`.

use std::sync::mpsc;
use std::time::Duration;

use orbit_common::fs::io::{FileLockOptions, with_exclusive_file_lock_options};
use orbit_types::task::{ORB_TASK_ID_MAX, TaskRelation, TaskRelationType, TaskStatus};
use tempfile::TempDir;

use crate::contracts::TaskHistoryUpdateParams;
use crate::driver::file::task_bundle::bundle_lock_target;
use crate::repository::task::TaskV2Store;
use crate::workflow::task::reindex::{
    clear_after_reindex_snapshot_hook, clear_before_reindex_publication_hook,
    set_after_reindex_snapshot_hook, set_before_reindex_publication_hook,
};

use super::*;

struct AfterSnapshotGuard;

impl Drop for AfterSnapshotGuard {
    fn drop(&mut self) {
        clear_after_reindex_snapshot_hook();
        clear_before_reindex_publication_hook();
    }
}

/// Attempt the same bundle lock ordinary mutations need while reindex is in
/// its final read-to-publication window. The timeout proves that the worker
/// reached that window instead of relying on a sleep or scheduler timing.
fn assert_final_window_blocks_mutation(
    lock_target: std::path::PathBuf,
    mutate: impl FnOnce() + Send + 'static,
) -> std::thread::JoinHandle<()> {
    let (start_tx, start_rx) = mpsc::channel();
    let (probe_tx, probe_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        start_rx.recv().expect("start final-window mutation");
        let probe = with_exclusive_file_lock_options(
            &lock_target,
            "reindex race probe",
            FileLockOptions {
                timeout: Duration::from_millis(100),
                warn_after: Duration::from_secs(1),
            },
            || Ok::<_, std::io::Error>(()),
        );
        probe_tx
            .send(matches!(probe, Err(ref error) if error.kind() == std::io::ErrorKind::TimedOut))
            .expect("report lock contention");
        mutate();
    });
    set_before_reindex_publication_hook(move || {
        start_tx.send(()).expect("start mutation worker");
        assert!(
            probe_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("lock contention result"),
            "final envelope read must retain the bundle lock through publication"
        );
    });
    worker
}

#[test]
fn final_read_to_publication_update_waits_then_keeps_committed_status_and_version() {
    let temp = TempDir::new().unwrap();
    let ws = "ws_final_update";
    let registry = open_registry(temp.path());
    let binding = bind(&registry, temp.path(), ws);
    let store = bundle_store(&registry, &binding);
    for id in ["ORB-00000", "ORB-00001"] {
        seed(&store, &registry, ws, &make_bundle(id, id, Vec::new()));
    }
    let _guard = AfterSnapshotGuard;
    let target = bundle_lock_target(&store.bundle_path("ORB-00000").unwrap());
    let updater = TaskV2Store::new(registry.clone(), ws.to_string());
    let worker = assert_final_window_blocks_mutation(target, move || {
        updater
            .update_task_history(
                "ORB-00000",
                &TaskHistoryUpdateParams {
                    actor: "codex".to_string(),
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
            )
            .expect("update after reindex publication");
    });

    assert_eq!(reindex_workspace(&registry, ws).unwrap().indexed, 2);
    worker.join().expect("mutation worker");
    let envelope = store.read_bundle("ORB-00000").unwrap().envelope;
    assert_eq!(envelope.status, TaskStatus::InProgress);
    let statuses = TaskV2Store::new(registry.clone(), ws.to_string())
        .task_status_index()
        .unwrap();
    assert_eq!(statuses.get("ORB-00000"), Some(&envelope.status));
    assert_eq!(statuses.get("ORB-00001"), Some(&TaskStatus::Backlog));
    let versions = registry.indexed_task_versions_for_workspace(ws).unwrap();
    assert_eq!(
        versions.get("ORB-00000").map(String::as_str),
        Some(envelope.updated_at.to_rfc3339().as_str())
    );
}

#[test]
fn final_read_to_publication_delete_cannot_resurrect_binding_or_index() {
    let temp = TempDir::new().unwrap();
    let ws = "ws_final_delete";
    let registry = open_registry(temp.path());
    let binding = bind(&registry, temp.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00000", "deleted", Vec::new()),
    );
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00002", "target", Vec::new()),
    );
    seed(
        &store,
        &registry,
        ws,
        &make_bundle(
            "ORB-00001",
            "neighbor",
            vec![TaskRelation {
                relation_type: TaskRelationType::ChildOf,
                target: "ORB-00002".into(),
            }],
        ),
    );
    // Repair must rebuild the healthy neighbor's relation, not merely leave
    // its previously indexed edge untouched.
    registry.unregister_task_bundle("ORB-00001", ws).unwrap();
    let _guard = AfterSnapshotGuard;
    let target = bundle_lock_target(&store.bundle_path("ORB-00000").unwrap());
    let deleting_store = bundle_store(&registry, &binding);
    let worker = assert_final_window_blocks_mutation(target, move || {
        deleting_store
            .delete_bundle("ORB-00000")
            .expect("delete after reindex publication");
    });

    assert_eq!(reindex_workspace(&registry, ws).unwrap().indexed, 3);
    worker.join().expect("deletion worker");
    assert!(!store.bundle_path("ORB-00000").unwrap().exists());
    let bindings = registry.tasks_for_workspace(ws).unwrap();
    assert_eq!(
        bindings
            .into_iter()
            .map(|binding| binding.task_id)
            .collect::<Vec<_>>(),
        vec!["ORB-00001", "ORB-00002"]
    );
    let statuses = TaskV2Store::new(registry.clone(), ws.to_string())
        .task_status_index()
        .unwrap();
    assert!(!statuses.contains_key("ORB-00000"));
    assert_eq!(statuses.get("ORB-00001"), Some(&TaskStatus::Backlog));
    assert!(
        !registry
            .indexed_task_versions_for_workspace(ws)
            .unwrap()
            .contains_key("ORB-00000")
    );
    assert_eq!(
        registry
            .indexed_relation_targets(ws, "ORB-00001", TaskRelationType::ChildOf)
            .unwrap(),
        vec!["ORB-00002"]
    );
}

/// A concurrent update after the first envelope read must not publish the
/// pre-change snapshot. `task_status_index` has to match the on-disk envelope.
#[test]
fn reindex_does_not_index_stale_envelope_after_concurrent_update() {
    let temp = TempDir::new().unwrap();
    let ws = "ws_reindex_update";
    let registry = open_registry(temp.path());
    let binding = bind(&registry, temp.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00000", "updated", Vec::new()),
    );
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00001", "neighbor", Vec::new()),
    );

    let _guard = AfterSnapshotGuard;
    let updater = TaskV2Store::new(registry.clone(), ws.to_string());
    set_after_reindex_snapshot_hook(move || {
        updater
            .update_task_history(
                "ORB-00000",
                &TaskHistoryUpdateParams {
                    actor: "codex".to_string(),
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
            )
            .expect("concurrent update");
    });

    reindex_workspace(&registry, ws).expect("reindex");

    let on_disk = store
        .read_bundle("ORB-00000")
        .expect("on-disk envelope after reindex")
        .envelope;
    assert_eq!(on_disk.status, TaskStatus::InProgress);
    let statuses = TaskV2Store::new(registry.clone(), ws.to_string())
        .task_status_index()
        .expect("task_status_index");
    assert_eq!(
        statuses.get("ORB-00000"),
        Some(&on_disk.status),
        "generated index must match the on-disk envelope, not the pre-change snapshot"
    );
    assert_eq!(statuses.get("ORB-00001"), Some(&TaskStatus::Backlog));
    let indexed = registry
        .indexed_task_versions_for_workspace(ws)
        .expect("indexed versions");
    assert_eq!(
        indexed.get("ORB-00000").map(String::as_str),
        Some(on_disk.updated_at.to_rfc3339().as_str())
    );
}

/// A concurrent delete after the first envelope read must not resurrect the
/// binding or the generated index row.
#[test]
fn reindex_does_not_resurrect_task_deleted_after_envelope_read() {
    let temp = TempDir::new().unwrap();
    let ws = "ws_reindex_delete";
    let registry = open_registry(temp.path());
    let binding = bind(&registry, temp.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00000", "deleted", Vec::new()),
    );
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00001", "kept", Vec::new()),
    );

    let _guard = AfterSnapshotGuard;
    let hook_store = bundle_store(&registry, &binding);
    set_after_reindex_snapshot_hook(move || {
        hook_store
            .delete_bundle("ORB-00000")
            .expect("concurrent delete");
    });

    let outcome = reindex_workspace(&registry, ws).expect("reindex");
    assert_eq!(outcome.indexed, 1, "only the surviving bundle is indexed");

    let registered: Vec<String> = registry
        .tasks_for_workspace(ws)
        .unwrap()
        .into_iter()
        .map(|binding| binding.task_id)
        .collect();
    assert_eq!(registered, vec!["ORB-00001"]);
    assert!(
        !store.bundle_path("ORB-00000").unwrap().exists(),
        "deleted bundle directory must stay gone"
    );

    let statuses = TaskV2Store::new(registry.clone(), ws.to_string())
        .task_status_index()
        .expect("task_status_index");
    assert!(
        !statuses.contains_key("ORB-00000"),
        "reindex must not resurrect a deleted task's index row"
    );
    assert_eq!(statuses.get("ORB-00001"), Some(&TaskStatus::Backlog));
    let indexed = registry
        .indexed_task_versions_for_workspace(ws)
        .expect("indexed versions");
    assert!(!indexed.contains_key("ORB-00000"));
    assert!(indexed.contains_key("ORB-00001"));
}

#[test]
fn reindex_reregisters_disk_bundles_and_drops_stale() {
    let temp = TempDir::new().unwrap();
    let ws = "orbit-idx-dddddd";
    let registry = open_registry(temp.path());
    let binding = bind(&registry, temp.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00000", "a", Vec::new()),
    );
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00003", "b", Vec::new()),
    );

    // Simulate drift: drop ORB-00003's index+binding (dir still on disk) and add
    // a stale binding for ORB-00009 whose dir does not exist.
    registry.unregister_task_bundle("ORB-00003", ws).unwrap();
    let stale_dir = registry
        .canonical_task_bundle_path(ws, "ORB-00009")
        .unwrap();
    fs::create_dir_all(&stale_dir).unwrap();
    registry
        .register_task_bundle("ORB-00009", ws, &stale_dir)
        .unwrap();
    fs::remove_dir_all(&stale_dir).unwrap();

    let outcome = reindex_workspace(&registry, ws).expect("reindex");
    assert_eq!(outcome.indexed, 2, "two on-disk bundles reindexed");
    assert_eq!(outcome.removed_stale, 1, "stale ORB-00009 dropped");

    let registered: Vec<String> = registry
        .tasks_for_workspace(ws)
        .unwrap()
        .into_iter()
        .map(|t| t.task_id)
        .collect();
    assert_eq!(registered, vec!["ORB-00000", "ORB-00003"]);
    // Allocator moved past the highest on-disk id.
    assert!(registry.allocator_next_number().unwrap() >= 4);

    let again = reindex_workspace(&registry, ws).expect("reindex again");
    assert_eq!(again.indexed, outcome.indexed);
    assert_eq!(again.removed_stale, 0);
    assert_eq!(registry.allocator_next_number().unwrap(), 4);
}

#[test]
fn reindex_indexes_foreign_mirrors_without_advancing_local_allocator() {
    let temp = TempDir::new().unwrap();
    let ws = "ws_foreign_reindex";
    let registry = open_registry(temp.path());
    registry.set_task_prefix("DE").unwrap();
    let binding = bind(&registry, temp.path(), ws);
    let store = bundle_store(&registry, &binding);
    let foreign_id = format!("ORB-{ORB_TASK_ID_MAX}");
    seed(
        &store,
        &registry,
        ws,
        &make_bundle(&foreign_id, "mirror", Vec::new()),
    );
    registry.unregister_task_bundle(&foreign_id, ws).unwrap();
    registry.seed_allocator_start(7).unwrap();

    let outcome = reindex_workspace(&registry, ws).unwrap();
    assert_eq!(outcome.indexed, 1);
    assert_eq!(registry.allocator_next_number().unwrap(), 7);
    assert!(
        registry
            .tasks_for_workspace(ws)
            .unwrap()
            .iter()
            .any(|task| task.task_id == foreign_id)
    );
    assert_eq!(registry.allocate_task_id(ws).unwrap(), "DE-00007");

    reindex_workspace(&registry, ws).unwrap();
    assert_eq!(registry.allocator_next_number().unwrap(), 8);
}

#[test]
fn reindex_reserves_healthy_and_unresolved_local_ids() {
    let temp = TempDir::new().unwrap();
    let ws = "ws_local_reindex";
    let registry = open_registry(temp.path());
    registry.set_task_prefix("DE").unwrap();
    let binding = bind(&registry, temp.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("DE-00008", "healthy", Vec::new()),
    );
    let unresolved = registry.canonical_task_bundle_path(ws, "DE-00012").unwrap();
    fs::create_dir_all(&unresolved).unwrap();
    fs::write(unresolved.join("events.jsonl"), b"retained data").unwrap();

    let error = reindex_workspace(&registry, ws).unwrap_err();
    assert!(error.to_string().contains("unresolved bundles retained"));
    assert_eq!(registry.allocator_next_number().unwrap(), 13);
    assert!(
        registry
            .tasks_for_workspace(ws)
            .unwrap()
            .iter()
            .any(|task| task.task_id == "DE-00008")
    );
    assert_eq!(registry.allocate_task_id(ws).unwrap(), "DE-00013");
}

#[test]
fn reindex_local_ceiling_exhausts_allocator_without_reusing_id() {
    let temp = TempDir::new().unwrap();
    let ws = "ws_ceiling_reindex";
    let registry = open_registry(temp.path());
    registry.set_task_prefix("DE").unwrap();
    let binding = bind(&registry, temp.path(), ws);
    let store = bundle_store(&registry, &binding);
    let ceiling_id = format!("DE-{ORB_TASK_ID_MAX}");
    seed(
        &store,
        &registry,
        ws,
        &make_bundle(&ceiling_id, "last id", Vec::new()),
    );

    assert_eq!(reindex_workspace(&registry, ws).unwrap().indexed, 1);
    assert_eq!(reindex_workspace(&registry, ws).unwrap().indexed, 1);
    assert!(
        registry
            .allocator_next_number()
            .unwrap_err()
            .to_string()
            .contains("exhausted")
    );
    let error = registry.allocate_task_id(ws).unwrap_err();
    assert!(error.to_string().contains("exhausted"), "{error}");
}

#[test]
fn reindex_unresolved_local_ceiling_also_exhausts_allocator() {
    let temp = TempDir::new().unwrap();
    let ws = "ws_unresolved_ceiling";
    let registry = open_registry(temp.path());
    registry.set_task_prefix("DE").unwrap();
    bind(&registry, temp.path(), ws);
    let ceiling_id = format!("DE-{ORB_TASK_ID_MAX}");
    let unresolved = registry
        .canonical_task_bundle_path(ws, &ceiling_id)
        .unwrap();
    fs::create_dir_all(&unresolved).unwrap();
    fs::write(unresolved.join("events.jsonl"), b"retained data").unwrap();

    assert!(
        reindex_workspace(&registry, ws)
            .unwrap_err()
            .to_string()
            .contains("unresolved")
    );
    let error = registry.allocate_task_id(ws).unwrap_err();
    assert!(error.to_string().contains("exhausted"), "{error}");
}
