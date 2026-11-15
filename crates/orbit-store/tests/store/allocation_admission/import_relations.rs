//! Public import of a known-prefix relation whose target is neither in the
//! accepted archive set nor already registered. Rejection has to name the
//! source, the relation type and the missing target, and it has to happen
//! before any canonical bundle write or workspace/binding insert. Rollback
//! deletes a bundle directory, so a directory check alone cannot show the
//! ordering; inserts into `workspace_bindings` and `task_bundle_bindings`
//! are copied into an audit table the importer does not roll back.

use orbit_store::maintenance::task_registry::task_workspaces_dir;
use orbit_store::workflow::task::{ImportAction, ImportOutcome};
use orbit_types::task::TaskRelationType;
use rusqlite::Connection;

use super::*;

#[test]
fn unresolved_known_prefix_relation_fails_before_import_mutation() {
    if !allocation_admission_isolated(
        "import_relations::unresolved_known_prefix_relation_fails_before_import_mutation",
    ) {
        return;
    }

    let source_root = TempDir::new().unwrap();
    let (source, parent, child) = linked_pair(source_root.path());
    assert_eq!(parent, "ORB-00000");
    assert_eq!(child, "ORB-00001");
    let dangling = source_root.path().join("dangling.tar.zst");
    export_selected(&source.registry, &[&child], &dangling);

    for policy in [
        ImportConflictPolicy::Fail,
        ImportConflictPolicy::Skip,
        ImportConflictPolicy::Renumber,
        ImportConflictPolicy::OwnerWins,
    ] {
        let target_root = TempDir::new().unwrap();
        let target = registry_at(target_root.path());
        let audit = install_import_write_audit(target_root.path());
        let next = target.allocator_next_number().unwrap();
        let error = import_tasks(&target, &dangling, None, policy)
            .expect_err("an out-of-set known-prefix parent must be rejected");
        match &error {
            OrbitError::NotFound { id, guidance, .. } => {
                assert_eq!(id, &parent, "{policy:?}: {error}");
                let guidance = guidance.as_deref().unwrap_or("");
                assert!(
                    guidance.contains(&format!("{child} child_of -> '{parent}'")),
                    "{policy:?}: diagnostic must name the source, type and target, got {guidance}"
                );
            }
            other => panic!("{policy:?}: expected a task not-found diagnostic, got {other}"),
        }
        assert_import_did_not_mutate(target_root.path(), &target, &audit, next, policy);
        assert!(
            !id_map_path(&dangling).exists(),
            "{policy:?}: a rejected import must not write an id map"
        );
    }

    let linked = source_root.path().join("linked.tar.zst");
    export_selected(&source.registry, &[&parent, &child], &linked);
    let fresh_root = TempDir::new().unwrap();
    let fresh = registry_at(fresh_root.path());
    let imported = import_tasks(&fresh, &linked, None, ImportConflictPolicy::Fail)
        .expect("in-archive parent resolves before either bundle is registered");
    assert!(imported.registered_workspace);
    assert_eq!(imported_action(&imported, &parent), ImportAction::Kept);
    assert_eq!(imported_action(&imported, &child), ImportAction::Kept);
    assert_eq!(
        child_of_targets(&fresh, &child),
        vec![parent.clone()],
        "the child keeps its in-archive parent"
    );

    let renumber_root = TempDir::new().unwrap();
    let renumber_dest = Coordinated::open(renumber_root.path());
    let local_parent = renumber_dest.create_task("local parent");
    let local_child = renumber_dest.create_task("local child");
    assert_eq!(local_parent.id, parent);
    assert_eq!(local_child.id, child);
    let renumbered = import_tasks(
        &renumber_dest.registry,
        &linked,
        Some(PARTITION_ID),
        ImportConflictPolicy::Renumber,
    )
    .expect("remapped in-archive targets resolve");
    assert_eq!(
        renumbered.id_remap.get(&parent).map(String::as_str),
        Some("ORB-00002")
    );
    assert_eq!(
        renumbered.id_remap.get(&child).map(String::as_str),
        Some("ORB-00003")
    );
    assert_eq!(
        imported_action(&renumbered, &parent),
        ImportAction::Renumbered
    );
    assert_eq!(
        imported_action(&renumbered, &child),
        ImportAction::Renumbered
    );
    assert_eq!(
        child_of_targets(&renumber_dest.registry, "ORB-00003"),
        vec!["ORB-00002".to_string()]
    );
    assert_eq!(renumber_dest.task(&parent).title, "local parent");
    assert_eq!(renumber_dest.task(&child).title, "local child");

    let skip_root = TempDir::new().unwrap();
    let skip_dest = Coordinated::open(skip_root.path());
    let existing_parent = skip_dest.create_task("existing parent");
    assert_eq!(existing_parent.id, parent);
    let skipped = import_tasks(
        &skip_dest.registry,
        &linked,
        Some(PARTITION_ID),
        ImportConflictPolicy::Skip,
    )
    .expect("a registered parent resolves a kept child");
    assert_eq!(
        imported_action(&skipped, &parent),
        ImportAction::SkippedConflict
    );
    assert_eq!(imported_action(&skipped, &child), ImportAction::Kept);
    assert_eq!(
        child_of_targets(&skip_dest.registry, &child),
        vec![parent.clone()]
    );
    assert_eq!(skip_dest.task(&parent).title, "existing parent");

    let foreign_root = TempDir::new().unwrap();
    {
        let registry = registry_at(foreign_root.path());
        registry.set_task_prefix("DANI").unwrap();
    }
    let foreign = Coordinated::open(foreign_root.path());
    let foreign_parent = foreign.create_task("foreign parent");
    let foreign_child = foreign.create_task_with_status(
        "foreign child",
        &["src/lib.rs"],
        TaskStatus::Backlog,
        Some(foreign_parent.id.clone()),
    );
    assert_eq!(foreign_parent.id, "DANI-00000");
    assert_eq!(foreign_child.id, "DANI-00001");
    let foreign_child_only = foreign_root.path().join("dani-child.tar.zst");
    export_selected(&foreign.registry, &[&foreign_child.id], &foreign_child_only);
    let foreign_pair = foreign_root.path().join("dani-pair.tar.zst");
    export_selected(
        &foreign.registry,
        &[&foreign_parent.id, &foreign_child.id],
        &foreign_pair,
    );

    // Landing DANI-00001 makes DANI known to the same check the index rebuild
    // runs after the binding exists, so a missing DANI parent is rejected
    // before any insert. A target under a prefix this import does not register
    // stays foreign and is admitted.
    let mirror_root = TempDir::new().unwrap();
    let mirror = registry_at(mirror_root.path());
    let audit = install_import_write_audit(mirror_root.path());
    let next = mirror.allocator_next_number().unwrap();
    let error = import_tasks(
        &mirror,
        &foreign_child_only,
        None,
        ImportConflictPolicy::OwnerWins,
    )
    .expect_err("a missing parent under a prefix this import registers is unresolved");
    match &error {
        OrbitError::NotFound { id, guidance, .. } => {
            assert_eq!(id, &foreign_parent.id, "{error}");
            let guidance = guidance.as_deref().unwrap_or("");
            assert!(
                guidance.contains(&format!(
                    "{} child_of -> '{}'",
                    foreign_child.id, foreign_parent.id
                )),
                "diagnostic must name the source, type and target, got {guidance}"
            );
        }
        other => panic!("expected a task not-found diagnostic, got {other}"),
    }
    assert_import_did_not_mutate(
        mirror_root.path(),
        &mirror,
        &audit,
        next,
        ImportConflictPolicy::OwnerWins,
    );

    let fred_child = foreign.create_task_with_status(
        "mentions an unissued prefix",
        &["src/lib.rs"],
        TaskStatus::Backlog,
        Some("FRED-00001".into()),
    );
    assert_eq!(fred_child.id, "DANI-00002");
    let fred_archive = foreign_root.path().join("dani-fred.tar.zst");
    export_selected(&foreign.registry, &[&fred_child.id], &fred_archive);
    let fred_root = TempDir::new().unwrap();
    let fred_dest = registry_at(fred_root.path());
    let fred_imported = import_tasks(
        &fred_dest,
        &fred_archive,
        None,
        ImportConflictPolicy::OwnerWins,
    )
    .expect("a prefix this import does not register may dangle");
    assert_eq!(
        imported_action(&fred_imported, &fred_child.id),
        ImportAction::Kept
    );
    assert_eq!(fred_dest.allocator_next_number().unwrap(), 0);
    assert_eq!(
        child_of_targets(&fred_dest, &fred_child.id),
        vec!["FRED-00001".to_string()]
    );

    let parent_only = foreign_root.path().join("dani-parent.tar.zst");
    export_selected(&foreign.registry, &[&foreign_parent.id], &parent_only);
    let registered_root = TempDir::new().unwrap();
    let registered = registry_at(registered_root.path());
    let parent_imported = import_tasks(
        &registered,
        &parent_only,
        None,
        ImportConflictPolicy::OwnerWins,
    )
    .expect("a foreign parent with no dangling target lands");
    assert_eq!(
        imported_action(&parent_imported, &foreign_parent.id),
        ImportAction::Kept
    );
    assert_eq!(registered.allocator_next_number().unwrap(), 0);
    let child_imported = import_tasks(
        &registered,
        &foreign_child_only,
        None,
        ImportConflictPolicy::OwnerWins,
    )
    .expect("an already registered foreign parent resolves the child");
    assert_eq!(
        imported_action(&child_imported, &foreign_child.id),
        ImportAction::Kept
    );
    assert_eq!(
        child_of_targets(&registered, &foreign_child.id),
        vec![foreign_parent.id.clone()]
    );
    assert_eq!(registered.allocator_next_number().unwrap(), 0);

    let mirror_root = TempDir::new().unwrap();
    let mirror = registry_at(mirror_root.path());
    let first = import_tasks(
        &mirror,
        &foreign_pair,
        None,
        ImportConflictPolicy::OwnerWins,
    )
    .expect("owner-wins keeps a foreign pair");
    assert_eq!(
        imported_action(&first, &foreign_parent.id),
        ImportAction::Kept
    );
    assert_eq!(
        imported_action(&first, &foreign_child.id),
        ImportAction::Kept
    );
    assert!(first.id_remap.is_empty());
    assert_eq!(
        child_of_targets(&mirror, &foreign_child.id),
        vec![foreign_parent.id.clone()]
    );
    foreign
        .backends
        .task
        .document
        .update_task_document(
            &foreign_child.id,
            orbit_store::contracts::TaskDocumentUpdateParams {
                actor: "codex".into(),
                title: Some("foreign child renamed".into()),
                ..Default::default()
            },
        )
        .unwrap();
    export_selected(
        &foreign.registry,
        &[&foreign_parent.id, &foreign_child.id],
        &foreign_pair,
    );
    let second = import_tasks(
        &mirror,
        &foreign_pair,
        None,
        ImportConflictPolicy::OwnerWins,
    )
    .expect("owner-wins refreshes the changed mirror");
    assert_eq!(
        imported_action(&second, &foreign_parent.id),
        ImportAction::AlreadyPresent
    );
    assert_eq!(
        imported_action(&second, &foreign_child.id),
        ImportAction::Updated
    );
    assert_eq!(second.id_remap.len(), 0);
    assert_eq!(
        child_of_targets(&mirror, &foreign_child.id),
        vec![foreign_parent.id.clone()]
    );
}

fn linked_pair(root: &Path) -> (Coordinated, String, String) {
    let source = Coordinated::open(root);
    let parent = source.create_task("archive parent");
    let child = source.create_task_with_status(
        "archive child",
        &["src/lib.rs"],
        TaskStatus::Backlog,
        Some(parent.id.clone()),
    );
    (source, parent.id, child.id)
}

fn export_selected(registry: &TaskRegistryStore, ids: &[&str], dest: &Path) {
    export_tasks(
        registry,
        PARTITION_ID,
        ExportSelection::Ids(ids.iter().copied().map(str::to_string).collect()),
        dest,
        Utc.with_ymd_and_hms(2026, 11, 1, 8, 0, 0).unwrap(),
    )
    .unwrap_or_else(|error| panic!("export {ids:?}: {error}"));
}

fn imported_action(outcome: &ImportOutcome, source_id: &str) -> ImportAction {
    outcome
        .tasks
        .iter()
        .find(|task| task.source_id == source_id)
        .unwrap_or_else(|| panic!("no import record for {source_id}"))
        .action
}

fn child_of_targets(registry: &TaskRegistryStore, id: &str) -> Vec<String> {
    registry
        .indexed_relation_targets(PARTITION_ID, id, TaskRelationType::ChildOf)
        .unwrap_or_else(|error| panic!("child_of targets for {id}: {error}"))
}

fn id_map_path(archive: &Path) -> PathBuf {
    let mut name = archive.as_os_str().to_owned();
    name.push(".idmap.json");
    PathBuf::from(name)
}

fn install_import_write_audit(root: &Path) -> Connection {
    let conn = Connection::open(task_registry_path(root)).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.execute_batch(
        "CREATE TABLE import_write_audit (
             table_name TEXT NOT NULL,
             row_id TEXT NOT NULL
         );
         CREATE TRIGGER import_write_audit_workspace
         AFTER INSERT ON workspace_bindings
         BEGIN
             INSERT INTO import_write_audit(table_name, row_id)
             VALUES ('workspace_bindings', NEW.workspace_id);
         END;
         CREATE TRIGGER import_write_audit_binding
         AFTER INSERT ON task_bundle_bindings
         BEGIN
             INSERT INTO import_write_audit(table_name, row_id)
             VALUES ('task_bundle_bindings', NEW.task_id);
         END;",
    )
    .unwrap();
    conn
}

fn assert_import_did_not_mutate(
    root: &Path,
    registry: &TaskRegistryStore,
    audit: &Connection,
    next_before: u32,
    policy: ImportConflictPolicy,
) {
    let mut statement = audit
        .prepare("SELECT table_name, row_id FROM import_write_audit ORDER BY rowid")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        rows.is_empty(),
        "{policy:?}: relation rejection must happen before workspace or binding inserts; a later rollback still leaves these audit rows: {rows:?}"
    );
    assert!(
        !registry.partition_ids().unwrap().contains(PARTITION_ID),
        "{policy:?}: source workspace must stay unregistered"
    );
    assert!(registry.find_task_binding("ORB-00000").unwrap().is_none());
    assert!(registry.find_task_binding("ORB-00001").unwrap().is_none());
    assert_eq!(registry.allocator_next_number().unwrap(), next_before);
    let partition = task_workspaces_dir(root).join(PARTITION_ID);
    assert!(
        !partition.exists(),
        "{policy:?}: canonical partition must not be created, found {}",
        partition.display()
    );
}
