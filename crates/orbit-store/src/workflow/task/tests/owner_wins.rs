//! Owner-wins import: the id prefix decides which colliding bundles a
//! cross-host sync may overwrite. Here the importing host mints
//! [`LOCAL_PREFIX`], so every `ORB-` id is a mirror of the peer's task and
//! every `DANI-` id is locally owned.

use std::fs;
use std::path::Path;
use std::slice;

use tempfile::TempDir;

use super::*;

/// Prefix the importing host mints under.
const LOCAL_PREFIX: &str = "DANI";

/// Destination registry for a host that mints [`LOCAL_PREFIX`]. The prefix is
/// adopted before anything is seeded, because a registry that has already
/// allocated refuses to be renamed.
fn open_mirror_registry(global: &Path) -> TaskRegistryStore {
    let registry = open_registry(global);
    registry
        .set_task_prefix(LOCAL_PREFIX)
        .expect("adopt the local prefix");
    registry
}

/// Pack `bundles` into `archive` as the owning host would. Each call uses its
/// own source root, so exporting a changed version of a task is just another
/// call with different content under the same id.
fn export_owner_archive(global: &Path, ws_id: &str, archive: &Path, bundles: &[TaskBundleV2]) {
    let registry = open_registry(global);
    let binding = bind(&registry, global, ws_id);
    let store = bundle_store(&registry, &binding);
    for bundle in bundles {
        seed(&store, &registry, ws_id, bundle);
    }
    export_tasks(
        &registry,
        ws_id,
        ExportSelection::All,
        archive,
        exported_at(),
    )
    .expect("export owner archive");
}

/// A `depends_on` edge: dependencies are persisted as `blocked_by` relations.
fn blocked_by(target: &str) -> TaskRelation {
    TaskRelation {
        relation_type: TaskRelationType::BlockedBy,
        target: target.to_string(),
    }
}

/// The owning host moves a task on. Envelope status and event log advance
/// together, because a bundle whose last event disagrees with its status does
/// not validate.
fn advanced_to(bundle: &TaskBundleV2, status: TaskStatus) -> TaskBundleV2 {
    let mut moved = bundle.clone();
    moved.envelope.status = status;
    moved.events.push(TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id: "EV-0002".to_string(),
        at: moved.envelope.updated_at,
        by: "codex".to_string(),
        event_type: "status_changed".to_string(),
        note: None,
        from_status: Some(bundle.envelope.status),
        to_status: Some(status),
    });
    moved
}

fn landed_bundle(registry: &TaskRegistryStore, ws: &str, task_id: &str) -> TaskBundleV2 {
    read_bundle_at(
        &registry
            .canonical_task_bundle_path(ws, task_id)
            .expect("canonical path"),
    )
    .expect("read landed bundle")
}

fn id_map_path_for(archive: &Path) -> PathBuf {
    let mut os = archive.as_os_str().to_owned();
    os.push(".idmap.json");
    PathBuf::from(os)
}

#[test]
fn owner_wins_replaces_a_changed_foreign_mirror() {
    let first_export = TempDir::new().unwrap();
    let second_export = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let ws = "orbit-owner-aaaaaa";

    let mirrored = make_bundle("ORB-00000", "root task", Vec::new());
    let archive = first_export.path().join("tasks.tar.zst");
    export_owner_archive(
        first_export.path(),
        ws,
        &archive,
        slice::from_ref(&mirrored),
    );

    let registry = open_mirror_registry(dst.path());
    bind(&registry, dst.path(), ws);
    import_tasks(&registry, &archive, None, ImportConflictPolicy::OwnerWins).expect("first sync");

    // The owner moved the task on, so the local mirror is now stale.
    let mut owner_copy = advanced_to(&mirrored, TaskStatus::Review);
    owner_copy.execution_summary = "landed on the owning host".to_string();
    let refreshed = second_export.path().join("tasks.tar.zst");
    export_owner_archive(
        second_export.path(),
        ws,
        &refreshed,
        slice::from_ref(&owner_copy),
    );

    let outcome =
        import_tasks(&registry, &refreshed, None, ImportConflictPolicy::OwnerWins).expect("resync");

    assert_eq!(outcome.tasks.len(), 1);
    assert_eq!(outcome.tasks[0].action, ImportAction::Updated);
    assert_eq!(outcome.tasks[0].final_id, "ORB-00000");
    assert!(outcome.id_remap.is_empty(), "owner-wins never renumbers");
    assert!(outcome.id_map_path.is_none());

    assert_eq!(landed_bundle(&registry, ws, "ORB-00000"), owner_copy);
    assert_eq!(
        registry
            .global_task_status_index()
            .unwrap()
            .get("ORB-00000"),
        Some(&TaskStatus::Review),
        "the index row follows the replaced bundle"
    );
    assert_eq!(
        registry.tasks_for_workspace(ws).unwrap().len(),
        1,
        "the update reuses the mirror's id"
    );
    assert_eq!(
        registry.allocator_next_number().unwrap(),
        0,
        "a foreign mirror never consumes a local id"
    );
}

#[test]
fn owner_wins_keeps_locally_owned_tasks() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let ws = "orbit-owner-bbbbbb";

    // The peer's archive carries one of its own tasks plus a stale mirror of
    // ours — that mirror must not come back at us.
    let archive = src.path().join("tasks.tar.zst");
    export_owner_archive(
        src.path(),
        ws,
        &archive,
        &[
            make_bundle("ORB-00001", "peer-owned task", Vec::new()),
            make_bundle("DANI-00007", "stale mirror of our task", Vec::new()),
        ],
    );

    let registry = open_mirror_registry(dst.path());
    let binding = bind(&registry, dst.path(), ws);
    let store = bundle_store(&registry, &binding);
    let local = make_bundle("DANI-00007", "local truth", Vec::new());
    seed(&store, &registry, ws, &local);

    let outcome =
        import_tasks(&registry, &archive, None, ImportConflictPolicy::OwnerWins).expect("sync");

    let ours = outcome
        .tasks
        .iter()
        .find(|task| task.source_id == "DANI-00007")
        .expect("record for the locally owned task");
    assert_eq!(ours.action, ImportAction::SkippedLocalOwned);
    assert_eq!(ours.final_id, "DANI-00007", "no fresh id is minted");
    assert!(outcome.id_remap.is_empty());
    assert!(outcome.id_map_path.is_none());
    assert_eq!(landed_bundle(&registry, ws, "DANI-00007"), local);

    let theirs = outcome
        .tasks
        .iter()
        .find(|task| task.source_id == "ORB-00001")
        .expect("record for the peer-owned task");
    assert_eq!(theirs.action, ImportAction::Kept);
    assert_eq!(
        registry.tasks_for_workspace(ws).unwrap().len(),
        2,
        "one local task plus one new mirror"
    );
}

#[test]
fn owner_wins_second_run_changes_nothing() {
    let first_export = TempDir::new().unwrap();
    let second_export = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let ws = "orbit-owner-cccccc";

    let mirrored = make_bundle("ORB-00000", "root task", Vec::new());
    let archive = first_export.path().join("tasks.tar.zst");
    export_owner_archive(
        first_export.path(),
        ws,
        &archive,
        slice::from_ref(&mirrored),
    );

    let registry = open_mirror_registry(dst.path());
    bind(&registry, dst.path(), ws);
    import_tasks(&registry, &archive, None, ImportConflictPolicy::OwnerWins).expect("first sync");

    let owner_copy = advanced_to(&mirrored, TaskStatus::Done);
    let refreshed = second_export.path().join("tasks.tar.zst");
    export_owner_archive(
        second_export.path(),
        ws,
        &refreshed,
        slice::from_ref(&owner_copy),
    );

    let updating =
        import_tasks(&registry, &refreshed, None, ImportConflictPolicy::OwnerWins).expect("resync");
    assert_eq!(updating.tasks[0].action, ImportAction::Updated);

    let repeated = import_tasks(&registry, &refreshed, None, ImportConflictPolicy::OwnerWins)
        .expect("repeat the same sync");
    assert!(
        repeated
            .tasks
            .iter()
            .all(|task| task.action == ImportAction::AlreadyPresent),
        "a settled mirror is already present: {:?}",
        repeated.tasks
    );
    assert!(repeated.id_remap.is_empty());
    assert!(repeated.id_map_path.is_none());
    assert!(
        !id_map_path_for(&refreshed).exists(),
        "an idempotent sync writes no id map"
    );

    assert_eq!(landed_bundle(&registry, ws, "ORB-00000"), owner_copy);
    assert_eq!(registry.tasks_for_workspace(ws).unwrap().len(), 1);
    assert_eq!(registry.allocator_next_number().unwrap(), 0);
}

#[test]
fn owner_wins_recreates_a_missing_bound_mirror_and_is_rerunnable() {
    let first_export = TempDir::new().unwrap();
    let second_export = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let ws = "orbit-owner-fefefe";

    let mirrored = make_bundle("ORB-00000", "root task", Vec::new());
    let first_archive = first_export.path().join("tasks.tar.zst");
    export_owner_archive(
        first_export.path(),
        ws,
        &first_archive,
        slice::from_ref(&mirrored),
    );

    let registry = open_mirror_registry(dst.path());
    bind(&registry, dst.path(), ws);
    import_tasks(
        &registry,
        &first_archive,
        None,
        ImportConflictPolicy::OwnerWins,
    )
    .expect("first sync");

    let canonical = registry
        .canonical_task_bundle_path(ws, "ORB-00000")
        .expect("canonical path");
    let retired = canonical
        .parent()
        .expect("bundle parent")
        .join(".ORB-00000.9999.0.retired");
    fs::rename(&canonical, &retired).expect("simulate an interrupted replacement");
    assert!(!canonical.exists());
    assert!(retired.is_dir());

    let refreshed = advanced_to(&mirrored, TaskStatus::Review);
    let second_archive = second_export.path().join("tasks.tar.zst");
    export_owner_archive(
        second_export.path(),
        ws,
        &second_archive,
        &[
            refreshed.clone(),
            make_bundle("ORB-00003", "new peer task", Vec::new()),
        ],
    );

    let repaired = import_tasks(
        &registry,
        &second_archive,
        None,
        ImportConflictPolicy::OwnerWins,
    )
    .expect("owner-wins repairs the missing mirror and lands the rest");
    assert_eq!(repaired.tasks.len(), 2);
    assert_eq!(
        repaired
            .tasks
            .iter()
            .find(|task| task.source_id == "ORB-00000")
            .expect("record for the repaired mirror")
            .action,
        ImportAction::Updated
    );
    assert_eq!(
        repaired
            .tasks
            .iter()
            .find(|task| task.source_id == "ORB-00003")
            .expect("record for the new mirror")
            .action,
        ImportAction::Kept
    );
    assert_eq!(landed_bundle(&registry, ws, "ORB-00000"), refreshed);
    assert_eq!(
        landed_bundle(&registry, ws, "ORB-00003").envelope.id,
        "ORB-00003"
    );
    assert!(
        !retired.exists(),
        "successful replacement cleans retired siblings"
    );
    assert_eq!(
        registry
            .global_task_status_index()
            .unwrap()
            .get("ORB-00000"),
        Some(&TaskStatus::Review),
        "rebuilding the index sees the recreated canonical path"
    );

    let repeated = import_tasks(
        &registry,
        &second_archive,
        None,
        ImportConflictPolicy::OwnerWins,
    )
    .expect("the repaired owner-wins import is rerunnable");
    assert!(
        repeated
            .tasks
            .iter()
            .all(|task| task.action == ImportAction::AlreadyPresent)
    );
    assert_eq!(registry.tasks_for_workspace(ws).unwrap().len(), 2);
}

#[test]
fn owner_wins_repairs_an_unbound_existing_bundle_and_lands_the_rest() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let ws = "orbit-owner-ababab";
    let archive = src.path().join("tasks.tar.zst");
    let incoming = make_bundle("ORB-00000", "owner truth", Vec::new());
    export_owner_archive(
        src.path(),
        ws,
        &archive,
        &[
            incoming.clone(),
            make_bundle("ORB-00003", "new peer task", Vec::new()),
        ],
    );

    let registry = open_mirror_registry(dst.path());
    let binding = bind(&registry, dst.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00000", "orphaned mirror", Vec::new()),
    );
    registry
        .unregister_task_bundle("ORB-00000", ws)
        .expect("remove only the orphan's registry binding");
    assert!(
        registry
            .canonical_task_bundle_path(ws, "ORB-00000")
            .unwrap()
            .is_dir()
    );

    let outcome =
        import_tasks(&registry, &archive, None, ImportConflictPolicy::OwnerWins).expect("sync");

    assert_eq!(outcome.tasks.len(), 2);
    assert_eq!(
        outcome
            .tasks
            .iter()
            .find(|task| task.source_id == "ORB-00000")
            .unwrap()
            .action,
        ImportAction::Updated
    );
    assert_eq!(
        outcome
            .tasks
            .iter()
            .find(|task| task.source_id == "ORB-00003")
            .unwrap()
            .action,
        ImportAction::Kept
    );
    assert_eq!(landed_bundle(&registry, ws, "ORB-00000"), incoming);
    assert!(registry.find_task_binding("ORB-00000").unwrap().is_some());
    assert_eq!(registry.tasks_for_workspace(ws).unwrap().len(), 2);
}

#[test]
fn owner_wins_leaves_an_unbound_locally_owned_bundle_untouched() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let ws = "orbit-owner-acacac";

    // The peer's archive carries a stale mirror of our task. Locally that task
    // has lost its registry binding (a partial index rebuild) but its bundle
    // is still on disk — the unbound repair path must not let the mirror
    // overwrite it.
    let archive = src.path().join("tasks.tar.zst");
    export_owner_archive(
        src.path(),
        ws,
        &archive,
        &[make_bundle(
            "DANI-00042",
            "stale mirror of our task",
            Vec::new(),
        )],
    );

    let registry = open_mirror_registry(dst.path());
    let binding = bind(&registry, dst.path(), ws);
    let store = bundle_store(&registry, &binding);
    let local = make_bundle("DANI-00042", "local truth", Vec::new());
    seed(&store, &registry, ws, &local);
    registry
        .unregister_task_bundle("DANI-00042", ws)
        .expect("remove only our task's registry binding");
    assert!(
        registry
            .canonical_task_bundle_path(ws, "DANI-00042")
            .unwrap()
            .is_dir()
    );

    let outcome =
        import_tasks(&registry, &archive, None, ImportConflictPolicy::OwnerWins).expect("sync");

    assert_eq!(outcome.tasks.len(), 1);
    assert_eq!(outcome.tasks[0].source_id, "DANI-00042");
    assert_eq!(outcome.tasks[0].final_id, "DANI-00042");
    assert_eq!(outcome.tasks[0].action, ImportAction::SkippedLocalOwned);
    assert_eq!(
        landed_bundle(&registry, ws, "DANI-00042"),
        local,
        "the local bundle's contents are preserved"
    );
    assert!(
        registry.find_task_binding("DANI-00042").unwrap().is_none(),
        "a skipped local task is not rebound by the sync"
    );
}

#[test]
fn owner_wins_preserves_foreign_relation_targets() {
    let first_export = TempDir::new().unwrap();
    let second_export = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let ws = "orbit-owner-dddddd";

    let blocker = make_bundle("ORB-00000", "peer blocker", Vec::new());
    let dependent = make_bundle("ORB-00001", "peer dependent", vec![blocked_by("ORB-00000")]);
    let archive = first_export.path().join("tasks.tar.zst");
    export_owner_archive(
        first_export.path(),
        ws,
        &archive,
        &[blocker.clone(), dependent.clone()],
    );

    let registry = open_mirror_registry(dst.path());
    bind(&registry, dst.path(), ws);
    import_tasks(&registry, &archive, None, ImportConflictPolicy::OwnerWins).expect("first sync");

    // The owner advances the dependent while the dependency stands.
    let owner_copy = advanced_to(&dependent, TaskStatus::InProgress);
    let refreshed = second_export.path().join("tasks.tar.zst");
    export_owner_archive(
        second_export.path(),
        ws,
        &refreshed,
        &[blocker, owner_copy.clone()],
    );

    let outcome =
        import_tasks(&registry, &refreshed, None, ImportConflictPolicy::OwnerWins).expect("resync");
    let updated = outcome
        .tasks
        .iter()
        .find(|task| task.source_id == "ORB-00001")
        .expect("record for the dependent");
    assert_eq!(updated.action, ImportAction::Updated);

    let landed = landed_bundle(&registry, ws, "ORB-00001");
    assert_eq!(landed, owner_copy);
    assert_eq!(
        landed.envelope.relations,
        vec![blocked_by("ORB-00000")],
        "a foreign dependency target is kept verbatim"
    );
    assert_eq!(
        registry
            .indexed_relation_targets(ws, "ORB-00001", TaskRelationType::BlockedBy)
            .unwrap(),
        vec!["ORB-00000".to_string()],
        "the rebuilt index keeps pointing at the owner's id"
    );
}

#[test]
fn owner_wins_refuses_to_move_a_mirror_between_workspaces() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive_ws = "orbit-owner-eeeeee";
    let other_ws = "orbit-owner-ffffff";

    let incoming = make_bundle("ORB-00002", "peer-owned task", Vec::new());
    let archive = src.path().join("tasks.tar.zst");
    export_owner_archive(src.path(), archive_ws, &archive, slice::from_ref(&incoming));

    // The local mirror of the same id is bound to a different workspace.
    let registry = open_mirror_registry(dst.path());
    let other = bind(&registry, dst.path(), other_ws);
    let store = bundle_store(&registry, &other);
    let local = make_bundle("ORB-00002", "mirror in another workspace", Vec::new());
    seed(&store, &registry, other_ws, &local);
    bind(&registry, dst.path(), archive_ws);

    let error = import_tasks(&registry, &archive, None, ImportConflictPolicy::OwnerWins)
        .expect_err("a cross-workspace mirror needs reconciliation, not a silent move");
    assert!(
        format!("{error}").contains("resolve the workspace before syncing"),
        "unexpected error: {error}"
    );

    assert_eq!(landed_bundle(&registry, other_ws, "ORB-00002"), local);
    assert_eq!(
        registry.tasks_for_workspace(archive_ws).unwrap().len(),
        0,
        "the refused import writes nothing"
    );
}
