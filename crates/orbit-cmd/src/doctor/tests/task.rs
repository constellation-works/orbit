use super::*;

#[test]
fn retired_graph_cleanup_removes_only_the_two_resolved_locations() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = split_root_runtime(&temp);
    let local_graph = runtime.local_root().join("graph");
    let shared_graph = runtime.shared_root().join("knowledge/graph");
    let unrelated = runtime.shared_root().join("knowledge/keep.txt");
    fs::create_dir_all(&local_graph).expect("create local graph");
    fs::create_dir_all(&shared_graph).expect("create shared graph");
    fs::write(local_graph.join("local.db"), b"retired").expect("write local graph");
    fs::write(shared_graph.join("shared.db"), b"retired").expect("write shared graph");
    fs::write(&unrelated, b"keep").expect("write unrelated state");

    assert_eq!(
        runtime
            .remove_retired_graph_state()
            .expect("remove retired graph state"),
        2
    );
    assert!(!local_graph.exists());
    assert!(!shared_graph.exists());
    assert!(unrelated.exists(), "cleanup must preserve sibling state");
    assert_eq!(
        runtime
            .remove_retired_graph_state()
            .expect("repeat cleanup"),
        0,
        "cleanup is idempotent when both locations are absent"
    );
}

#[test]
fn ordinary_doctor_leaves_retired_graph_locations_untouched() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = split_root_runtime(&temp);
    let local_marker = runtime.local_root().join("graph/local.db");
    let shared_marker = runtime.shared_root().join("knowledge/graph/shared.db");
    for marker in [&local_marker, &shared_marker] {
        fs::create_dir_all(marker.parent().expect("graph parent")).expect("create graph parent");
        fs::write(marker, b"retired").expect("write graph marker");
    }

    let results = runtime.doctor_workspace().expect("doctor");

    assert!(local_marker.exists());
    assert!(shared_marker.exists());
    assert!(results.iter().all(|row| row.check_name != "graph-index"));
}

#[cfg(unix)]
#[test]
fn retired_graph_cleanup_unlinks_boundaries_without_following_them() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = split_root_runtime(&temp);
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).expect("create outside");
    let outside_marker = outside.join("keep.db");
    fs::write(&outside_marker, b"keep").expect("write outside marker");
    let local_graph = runtime.local_root().join("graph");
    let shared_graph = runtime.shared_root().join("knowledge/graph");
    fs::create_dir_all(shared_graph.parent().expect("knowledge parent"))
        .expect("create knowledge parent");
    std::os::unix::fs::symlink(&outside, &local_graph).expect("link local graph");
    std::os::unix::fs::symlink(&outside, &shared_graph).expect("link shared graph");

    assert_eq!(
        runtime
            .remove_retired_graph_state()
            .expect("remove graph links"),
        2
    );
    assert!(
        outside_marker.exists(),
        "cleanup must not follow graph symlinks"
    );
    assert!(fs::symlink_metadata(local_graph).is_err());
    assert!(fs::symlink_metadata(shared_graph).is_err());
}

#[test]
fn workspace_retired_backend_warns_artifacts_activities_with_repair_command() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let activities = temp
        .path()
        .join("repo")
        .join(".orbit")
        .join("resources")
        .join("activities");
    fs::create_dir_all(&activities).expect("create workspace activities");
    let path = activities.join("workspace_finisher.yaml");
    fs::write(
        &path,
        "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: workspace_finisher\nspec:\n  type: agent_loop\n  description: fixture\n  instruction: do the work\n  backend: http\n",
    )
    .expect("write retired backend activity");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "artifacts-activities");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(
        row.message.contains("workspace_finisher.yaml"),
        "{}",
        row.message
    );
    assert!(
        row.message.contains("spec.backend: http"),
        "{}",
        row.message
    );
    assert!(
        row.message.contains("schemaVersion 2 parse failed"),
        "{}",
        row.message
    );
    assert_eq!(
        row.remediation.as_deref(),
        Some("Run `orbit doctor --fix-retired-activity-backends`.")
    );

    let repaired = runtime
        .repair_retired_activity_backends()
        .expect("repair retired backends");
    assert_eq!(repaired.repaired, vec![path.clone()]);
    assert!(repaired.skipped.is_empty(), "{repaired:?}");
    assert!(
        !fs::read_to_string(&path)
            .expect("read repaired activity")
            .contains("backend:"),
        "repair must remove only the backend key"
    );

    let after = runtime.doctor_workspace().expect("doctor after repair");
    assert_eq!(
        status_of(&after, "artifacts-activities").status,
        WorkspaceDoctorStatus::Ok,
        "{after:?}"
    );
}

#[test]
fn stale_shipped_activity_default_names_the_refresh_remediation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("repo/.orbit");
    let runtime = OrbitRuntime::initialize_from_resolved_roots(
        OrbitRuntimeRoots {
            global_root: global_root.clone(),
            shared_root: workspace_root.clone(),
            local_root: workspace_root,
        },
        None,
    )
    .expect("initialize runtime with defaults");
    let activities_dir = global_root.join("resources/activities");
    let path = activities_dir.join("agent_implement.yaml");
    let current = fs::read_to_string(&path).expect("read current activity");
    let stale = current.replacen("  tools:\n", "  tools:\n    - fs.read\n", 1);
    assert_ne!(stale, current, "fixture must contain the retired tool");
    fs::write(&path, &stale).expect("write stale activity");

    let manifest_path = activities_dir.join(".orbit-managed-assets.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).expect("read managed manifest"))
            .expect("parse managed manifest");
    manifest["assets"]["agent_implement"] =
        serde_json::Value::String(format!("{:x}", Sha256::digest(stale.as_bytes())));
    fs::write(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("serialize managed manifest")
        ),
    )
    .expect("write stale managed manifest");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "artifacts-activities");
    assert_eq!(row.status, WorkspaceDoctorStatus::Error, "{row:?}");
    assert!(row.message.contains("stale"), "{}", row.message);
    assert!(row.message.contains("older release"), "{}", row.message);
    assert_eq!(row.remediation.as_deref(), Some("Run `orbit init`."));
}

#[test]
fn missing_shipped_activity_default_is_an_error_not_healthy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("repo/.orbit");
    let runtime = OrbitRuntime::initialize_from_resolved_roots(
        OrbitRuntimeRoots {
            global_root: global_root.clone(),
            shared_root: workspace_root.clone(),
            local_root: workspace_root,
        },
        None,
    )
    .expect("initialize runtime with defaults");
    let path = global_root.join("resources/activities/git_merge.yaml");
    std::fs::remove_file(&path).expect("delete shipped default");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "artifacts-activities");
    assert_eq!(row.status, WorkspaceDoctorStatus::Error, "{row:?}");
    assert!(row.message.contains("missing"), "{}", row.message);
    assert!(row.message.contains("git_merge"), "{}", row.message);
    assert_eq!(row.remediation.as_deref(), Some("Run `orbit init`."));
    assert!(
        results
            .iter()
            .any(|row| row.status == WorkspaceDoctorStatus::Error),
        "a missing shipped default must not leave the workspace looking healthy: {results:?}"
    );
}

/// [ORB-12668] An aborted create (ORB-* directory, no task.yaml) is named
/// with its path and a reindex remediation instead of staying silent.
#[test]
fn unpublished_task_stub_is_reported_with_path_and_reindex_remediation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    write_registered_workspace(&global_root, "ws_registered", "registered");
    write_task_bundle(&global_root, "ws_registered", "ORB-00000");
    let stub = write_unpublished_stub(&global_root, "ws_registered", "ORB-00001");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "empty-task-stubs");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(
        row.message.contains(&stub.to_string_lossy().into_owned()),
        "message names the stub path: {}",
        row.message
    );
    assert!(
        !row.message.contains("ORB-00000"),
        "healthy bundles must not be reported as stubs: {}",
        row.message
    );
    assert_eq!(
        row.remediation.as_deref(),
        Some(
            "Run `orbit task reindex` from the owning checkout to skip or remove empty stub directories."
        )
    );
    assert_eq!(
        status_of(&results, "unresolved-task-bundles").status,
        WorkspaceDoctorStatus::Ok,
        "lock-only residue is not unresolved data: {results:?}"
    );
}

/// [ORB-12688] Residue and data-bearing missing-`task.yaml` dirs in the same
/// partition are classified and worded as distinct doctor rows.
#[test]
fn unpublished_stub_and_unresolved_bundle_are_classified_distinctly() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    write_registered_workspace(&global_root, "ws_registered", "registered");
    write_task_bundle(&global_root, "ws_registered", "ORB-00000");
    let stub = write_unpublished_stub(&global_root, "ws_registered", "ORB-00001");
    let unresolved = write_unresolved_bundle(&global_root, "ws_registered", "ORB-00002");
    let stub_path = stub.to_string_lossy().into_owned();
    let unresolved_path = unresolved.to_string_lossy().into_owned();

    let results = runtime.doctor_workspace().expect("doctor");

    let stub_row = status_of(&results, "empty-task-stubs");
    assert_eq!(
        stub_row.status,
        WorkspaceDoctorStatus::Warning,
        "{stub_row:?}"
    );
    assert!(
        stub_row.message.contains("unpublished task-bundle stub"),
        "stub row names residue: {}",
        stub_row.message
    );
    assert!(
        stub_row.message.contains(&stub_path),
        "stub row names the stub path: {}",
        stub_row.message
    );
    assert!(
        !stub_row.message.contains(&unresolved_path),
        "stub row must not name retained data: {}",
        stub_row.message
    );
    assert_eq!(
        stub_row.remediation.as_deref(),
        Some(
            "Run `orbit task reindex` from the owning checkout to skip or remove empty stub directories."
        )
    );

    let unresolved_row = status_of(&results, "unresolved-task-bundles");
    assert_eq!(
        unresolved_row.status,
        WorkspaceDoctorStatus::Warning,
        "{unresolved_row:?}"
    );
    assert!(
        unresolved_row.message.contains("retained task data"),
        "unresolved row names retained data: {}",
        unresolved_row.message
    );
    assert!(
        !unresolved_row
            .message
            .contains("unpublished task-bundle stub"),
        "unresolved row must not call retained data a stub: {}",
        unresolved_row.message
    );
    assert!(
        unresolved_row.message.contains(&unresolved_path),
        "unresolved row names the data-bearing path: {}",
        unresolved_row.message
    );
    assert!(
        !unresolved_row.message.contains(&stub_path),
        "unresolved row must not name residue: {}",
        unresolved_row.message
    );
    let remediation = unresolved_row
        .remediation
        .as_deref()
        .expect("unresolved row has a remedy");
    assert!(
        !remediation.contains("orbit task reindex"),
        "unresolved remedy must not advertise reindex: {remediation}"
    );
    assert!(
        remediation.contains("task.yaml"),
        "unresolved remedy names restoring the envelope: {remediation}"
    );
}

/// [ORB-12109] A task-store partition whose workspace is still registered on
/// this host is healthy, not an orphan. Coordinated composition also activates
/// a claimed partition for the fixture checkout, so the host scans two
/// partitions and both must stay claimed.

#[test]
fn registered_task_store_partition_is_not_an_orphan() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    write_registered_workspace(&global_root, "ws_registered", "registered");
    write_task_bundle(&global_root, "ws_registered", "ORB-1");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(row.status, WorkspaceDoctorStatus::Ok, "{row:?}");
    assert!(
        row.message.contains("2 task-store partition"),
        "{}",
        row.message
    );
    assert!(
        row.message.contains("all claimed by a workspace binding"),
        "{}",
        row.message
    );
}

/// [ORB-12109] A task-store partition whose workspace id no longer resolves
/// in the registry — left behind by `workspace teardown` on an older binary,
/// or by deleting a checkout without running teardown — is named with its
/// path and an exact repair command. Emptied of bundles, it carries nothing to
/// recover, so the repair is the right next step [ORB-12131].
#[test]
fn orphan_task_store_partition_is_reported_with_path_and_remediation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    write_registered_workspace(&global_root, "ws_registered", "registered");
    write_task_bundle(&global_root, "ws_registered", "ORB-1");
    write_empty_partition(&global_root, "ws_orphan");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(row.message.contains("ws_orphan"), "{}", row.message);
    assert!(
        row.message.contains(
            &task_workspaces_dir(&global_root)
                .join("ws_orphan")
                .to_string_lossy()
                .into_owned()
        ),
        "message names the orphaned partition path: {}",
        row.message
    );
    assert!(row.message.contains("0 task bundle(s)"), "{}", row.message);
    assert!(
        !row.message.contains("ws_registered"),
        "registered partition must not be reported: {}",
        row.message
    );
    assert_eq!(
        row.remediation.as_deref(),
        Some("Run `orbit doctor --fix-orphan-task-stores --confirm`.")
    );
}

/// [ORB-12119] A partition bound in the task registry under a derived
/// `<slug>-<hash>` id is live task state, even though that id is absent from
/// the workspace catalog, which knows the same checkout as `ws_*`. Comparing
/// partition names against catalog ids alone flagged every such partition —
/// the host's real task stores — as orphaned.
#[test]
fn task_registry_bound_partition_is_not_an_orphan() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    let drifted_root = temp.path().join("drifted");
    fs::create_dir_all(drifted_root.join(".orbit")).expect("create drifted checkout");

    write_registered_workspace(&global_root, "ws_drifted", "drifted");
    bind_task_partition(&global_root, "drifted-a1b2c3", "drifted", &drifted_root);
    write_task_bundle(&global_root, "drifted-a1b2c3", "ORB-1");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(
        row.status,
        WorkspaceDoctorStatus::Ok,
        "a bound partition is live task state: {row:?}"
    );
}

/// A task-registry binding to a deleted checkout is stale rather than a live
/// claim. Doctor reports it, and the confirmed repair removes its partition
/// and the binding's task data.
#[test]
fn stale_task_registry_binding_is_reported_and_removed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    let deleted_root = temp.path().join("deleted");
    fs::create_dir_all(deleted_root.join(".orbit")).expect("create deleted checkout");

    bind_task_partition_at(
        &global_root,
        "deleted-a1b2c3",
        "deleted",
        &deleted_root,
        &global_root,
    );
    write_task_bundle(&global_root, "deleted-a1b2c3", "ORB-2");
    fs::remove_dir_all(&deleted_root).expect("delete checkout");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(row.message.contains("deleted-a1b2c3"), "{}", row.message);
    assert!(row.message.contains("1 task bundle(s)"), "{}", row.message);
    assert_eq!(
        row.remediation.as_deref(),
        Some(
            "Run `orbit doctor --fix-orphan-task-stores --confirm`. Populated stale partitions \
             are deleted along with their task bundles."
        )
    );

    let removed = runtime
        .remove_orphan_task_stores()
        .expect("remove stale orphan task store");
    assert_eq!(removed.empty_partitions, 0, "{removed:?}");
    assert_eq!(
        removed.populated_partitions, 1,
        "a populated stale partition was removed: {removed:?}"
    );
    assert_eq!(
        removed.task_bundles, 1,
        "the removed partition's bundle is counted: {removed:?}"
    );
    assert!(
        !task_workspaces_dir(&global_root)
            .join("deleted-a1b2c3")
            .exists()
    );
    assert!(!partition_is_bound(&global_root, "deleted-a1b2c3").expect("read binding"));
}

/// `orbit workspace init` uses the catalog `ws_*` id as the task partition in
/// this layout. A deleted checkout must therefore be stale even while the
/// catalog entry remains present.
#[test]
fn deleted_catalog_checkout_partition_is_reported_and_removed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    let deleted_root = temp.path().join("deleted");
    fs::create_dir_all(deleted_root.join(".orbit")).expect("create deleted checkout");

    write_registered_workspace(&global_root, "ws_deleted", "deleted");
    write_registered_checkout(&global_root, "ws_deleted", &deleted_root);
    bind_task_partition(&global_root, "ws_deleted", "deleted", &deleted_root);
    write_task_bundle(&global_root, "ws_deleted", "ORB-3");
    fs::remove_dir_all(&deleted_root).expect("delete checkout");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(row.message.contains("ws_deleted"), "{}", row.message);
    assert!(row.message.contains("1 task bundle(s)"), "{}", row.message);
    assert!(
        row.message.contains("missing checkout directories"),
        "{}",
        row.message
    );

    let removed = runtime
        .remove_orphan_task_stores()
        .expect("remove stale catalog partition");
    assert_eq!(removed.populated_partitions, 1, "{removed:?}");
    assert_eq!(removed.task_bundles, 1, "{removed:?}");
    assert!(
        !task_workspaces_dir(&global_root)
            .join("ws_deleted")
            .exists()
    );
    assert!(!partition_is_bound(&global_root, "ws_deleted").expect("read binding"));
}

/// A shared external root is not per-checkout evidence: the catalog's
/// repository root must be used when `workspace init` supplied only a
/// path-free task-registry registration.
#[test]
fn deleted_shared_root_catalog_checkout_partition_is_reported_and_removed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    let deleted_root = temp.path().join("deleted");
    fs::create_dir_all(&deleted_root).expect("create deleted checkout");

    write_registered_workspace(&global_root, "ws_shared_deleted", "shared-deleted");
    write_registered_shared_root_checkout(&global_root, "ws_shared_deleted", &deleted_root);
    register_task_workspace(&global_root, "ws_shared_deleted", "shared-deleted");
    write_task_bundle(&global_root, "ws_shared_deleted", "ORB-5");
    fs::remove_dir_all(&deleted_root).expect("delete checkout");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(row.message.contains("ws_shared_deleted"), "{}", row.message);
    assert!(row.message.contains("1 task bundle(s)"), "{}", row.message);
    assert!(
        row.message.contains("missing checkout directories"),
        "{}",
        row.message
    );

    let removed = runtime
        .remove_orphan_task_stores()
        .expect("remove stale shared-root partition");
    assert_eq!(removed.populated_partitions, 1, "{removed:?}");
    assert_eq!(removed.task_bundles, 1, "{removed:?}");
    assert!(
        !task_workspaces_dir(&global_root)
            .join("ws_shared_deleted")
            .exists()
    );
    assert!(!partition_is_bound(&global_root, "ws_shared_deleted").expect("read binding"));
}

/// [ORB-12223] Shared external-root layout: deleted checkout → doctor warns →
/// catalog `workspace remove` → doctor still reports the partition stale →
/// the confirmed repair reclaims it. A path-free task-registry binding plus a
/// survivor occupying the shared `orbit_dir` is the production shape.
#[test]
fn catalog_remove_keeps_a_deleted_shared_root_partition_stale_and_reclaimable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    let survivor_root = temp.path().join("survivor");
    let deleted_root = temp.path().join("deleted");
    fs::create_dir_all(&survivor_root).expect("create survivor checkout");
    fs::create_dir_all(&deleted_root).expect("create deleted checkout");

    write_registered_workspace(&global_root, "ws_survivor", "survivor");
    write_registered_shared_root_checkout(&global_root, "ws_survivor", &survivor_root);
    bind_task_partition_at(
        &global_root,
        "ws_survivor",
        "survivor",
        &survivor_root,
        &global_root,
    );
    write_task_bundle(&global_root, "ws_survivor", "ORB-1");

    write_registered_workspace(&global_root, "ws_shared_deleted", "shared-deleted");
    write_registered_shared_root_checkout(&global_root, "ws_shared_deleted", &deleted_root);
    register_task_workspace(&global_root, "ws_shared_deleted", "shared-deleted");
    write_task_bundle(&global_root, "ws_shared_deleted", "ORB-5");
    fs::remove_dir_all(&deleted_root).expect("delete checkout");

    let results = runtime.doctor_workspace().expect("doctor before remove");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(row.message.contains("ws_shared_deleted"), "{}", row.message);
    assert!(
        row.message.contains("missing checkout directories"),
        "{}",
        row.message
    );

    let registry_path = workspace_registry::registry_path_for(&global_root);
    let mut registry =
        workspace_registry::load_registry_from(&registry_path).expect("load catalog");
    let checkout = registry
        .checkouts
        .iter()
        .find(|checkout| checkout.workspace_id == "ws_shared_deleted")
        .cloned()
        .expect("catalog checkout");
    let leftover = retain_task_store_on_catalog_remove(
        &global_root,
        "ws_shared_deleted",
        "shared-deleted",
        Some(&checkout),
    )
    .expect("retain leftover");
    assert_eq!(leftover.expect("leftover partition").task_bundles, 1);
    workspace_registry::remove_workspace(&mut registry, "ws_shared_deleted")
        .expect("drop catalog workspace");
    workspace_registry::save_registry_to(&registry, &registry_path).expect("save catalog");

    let results = runtime.doctor_workspace().expect("doctor after remove");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(
        row.status,
        WorkspaceDoctorStatus::Warning,
        "catalog removal must not re-hide the partition as claimed: {row:?}"
    );
    assert!(row.message.contains("ws_shared_deleted"), "{}", row.message);
    assert!(
        row.message.contains("missing checkout directories"),
        "{}",
        row.message
    );

    let removed = runtime
        .remove_orphan_task_stores()
        .expect("remove stale shared-root partition after catalog drop");
    assert_eq!(removed.populated_partitions, 1, "{removed:?}");
    assert_eq!(removed.task_bundles, 1, "{removed:?}");
    assert!(
        !task_workspaces_dir(&global_root)
            .join("ws_shared_deleted")
            .exists()
    );
    assert!(
        task_workspaces_dir(&global_root)
            .join("ws_survivor")
            .join("ORB-1")
            .is_dir(),
        "the live shared-root checkout's bundles must survive"
    );
    assert!(!partition_is_bound(&global_root, "ws_shared_deleted").expect("read binding"));
    assert!(partition_is_bound(&global_root, "ws_survivor").expect("read survivor binding"));
}

/// [ORB-12223] Repo-local layout: dropping the catalog entry after a deleted
/// checkout must not hide the already-bound stale partition.
#[test]
fn catalog_remove_keeps_a_deleted_repo_local_partition_stale_and_reclaimable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    let deleted_root = temp.path().join("deleted");
    fs::create_dir_all(deleted_root.join(".orbit")).expect("create deleted checkout");

    write_registered_workspace(&global_root, "ws_deleted", "deleted");
    write_registered_checkout(&global_root, "ws_deleted", &deleted_root);
    bind_task_partition(&global_root, "ws_deleted", "deleted", &deleted_root);
    write_task_bundle(&global_root, "ws_deleted", "ORB-3");
    fs::remove_dir_all(&deleted_root).expect("delete checkout");

    let results = runtime.doctor_workspace().expect("doctor before remove");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");

    let registry_path = workspace_registry::registry_path_for(&global_root);
    let mut registry =
        workspace_registry::load_registry_from(&registry_path).expect("load catalog");
    let checkout = registry
        .checkouts
        .iter()
        .find(|checkout| checkout.workspace_id == "ws_deleted")
        .cloned()
        .expect("catalog checkout");
    retain_task_store_on_catalog_remove(&global_root, "ws_deleted", "deleted", Some(&checkout))
        .expect("retain leftover");
    workspace_registry::remove_workspace(&mut registry, "ws_deleted").expect("drop catalog");
    workspace_registry::save_registry_to(&registry, &registry_path).expect("save catalog");

    let results = runtime.doctor_workspace().expect("doctor after remove");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(row.message.contains("ws_deleted"), "{}", row.message);
    assert!(
        row.message.contains("missing checkout directories"),
        "{}",
        row.message
    );

    let removed = runtime
        .remove_orphan_task_stores()
        .expect("remove stale repo-local partition after catalog drop");
    assert_eq!(removed.populated_partitions, 1, "{removed:?}");
    assert_eq!(removed.task_bundles, 1, "{removed:?}");
    assert!(
        !task_workspaces_dir(&global_root)
            .join("ws_deleted")
            .exists()
    );
    assert!(!partition_is_bound(&global_root, "ws_deleted").expect("read binding"));
}

/// A catalog checkout that cannot be stat-ed is not evidence of deletion. Its
/// populated task partition remains recoverable until the path is resolved.
#[test]
fn unreachable_catalog_checkout_partition_is_not_deleted() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    let unreachable_root = temp.path().join("unreachable");
    fs::create_dir_all(unreachable_root.join(".orbit")).expect("create checkout");

    write_registered_workspace(&global_root, "ws_unreachable", "unreachable");
    write_registered_checkout(&global_root, "ws_unreachable", &unreachable_root);
    bind_task_partition(
        &global_root,
        "ws_unreachable",
        "unreachable",
        &unreachable_root,
    );
    write_task_bundle(&global_root, "ws_unreachable", "ORB-4");
    fs::remove_dir_all(&unreachable_root).expect("remove checkout directory");
    fs::write(&unreachable_root, b"not a directory").expect("write path obstruction");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(
        row.message.contains("could not be reached"),
        "{}",
        row.message
    );
    assert!(row.message.contains("ws_unreachable"), "{}", row.message);
    assert!(row.message.contains("1 task bundle(s)"), "{}", row.message);

    assert_eq!(
        runtime.remove_orphan_task_stores().expect("run repair"),
        OrphanTaskStoreRemoval::default()
    );
    assert!(
        task_workspaces_dir(&global_root)
            .join("ws_unreachable")
            .join("ORB-4")
            .is_dir(),
        "the repair must not delete a catalog partition without absence evidence"
    );
}

/// [ORB-12119] The fix deletes only partitions no registry claims: a
/// task-registry binding, a workspace-catalog entry, and the synthetic
/// `--root` data-dir partition each keep their bundles.
#[test]
fn fix_orphan_task_stores_keeps_every_claimed_partition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    let drifted_root = temp.path().join("drifted");
    fs::create_dir_all(drifted_root.join(".orbit")).expect("create drifted checkout");

    write_registered_workspace(&global_root, "ws_registered", "registered");
    write_task_bundle(&global_root, "ws_registered", "ORB-1");
    bind_task_partition(&global_root, "drifted-a1b2c3", "drifted", &drifted_root);
    write_task_bundle(&global_root, "drifted-a1b2c3", "ORB-2");
    // Every `--root <data-dir>` write lands here, and no registry ever records it.
    write_task_bundle(&global_root, "ws_unbound-data-dir", "ORB-3");
    write_empty_partition(&global_root, "ws_orphan");

    let removed = runtime
        .remove_orphan_task_stores()
        .expect("remove orphan task stores");
    assert_eq!(
        removed.empty_partitions, 1,
        "only the unclaimed partition is removed: {removed:?}"
    );
    assert_eq!(removed.populated_partitions, 0, "{removed:?}");
    assert_eq!(removed.task_bundles, 0, "{removed:?}");

    let partitions = task_workspaces_dir(&global_root);
    assert!(!partitions.join("ws_orphan").exists());
    for claimed in ["ws_registered", "drifted-a1b2c3", "ws_unbound-data-dir"] {
        assert!(
            partitions.join(claimed).is_dir(),
            "claimed partition '{claimed}' must survive the fix"
        );
    }

    let results = runtime.doctor_workspace().expect("doctor after fix");
    assert_eq!(
        status_of(&results, "orphan-task-stores").status,
        WorkspaceDoctorStatus::Ok,
        "{results:?}"
    );
}

/// [ORB-12109] `--fix-orphan-task-stores` deletes only the unregistered
/// partition, leaving the registered one untouched, and doctor is healthy
/// again afterward — the teardown-then-doctor regression path.
#[test]
fn fix_orphan_task_stores_removes_only_the_unregistered_partition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    write_registered_workspace(&global_root, "ws_registered", "registered");
    write_task_bundle(&global_root, "ws_registered", "ORB-1");
    write_empty_partition(&global_root, "ws_orphan");

    let removed = runtime
        .remove_orphan_task_stores()
        .expect("remove orphan task stores");
    assert_eq!(removed.empty_partitions, 1, "{removed:?}");
    assert_eq!(removed.populated_partitions, 0, "{removed:?}");
    assert!(!task_workspaces_dir(&global_root).join("ws_orphan").exists());
    assert!(
        task_workspaces_dir(&global_root)
            .join("ws_registered")
            .exists()
    );

    let results = runtime.doctor_workspace().expect("doctor after fix");
    assert_eq!(
        status_of(&results, "orphan-task-stores").status,
        WorkspaceDoctorStatus::Ok,
        "{results:?}"
    );
}

/// [ORB-12131] A partition that still holds task bundles is reported without
/// pointing the operator at the deletion repair: the same picture is what a
/// lost `tasks/index.sqlite` paints for every live checkout, and `orbit task
/// reindex` restores those bundles.
#[test]
fn populated_unclaimed_partition_warns_toward_reindex_not_deletion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let populated = task_workspaces_dir(&runtime.global_root())
        .join("elsewhere-d4e5f6")
        .join("ORB-77");
    fs::create_dir_all(&populated).expect("create task bundle in an unclaimed partition");

    let row = status_of(
        &runtime.doctor_workspace().expect("doctor"),
        "orphan-task-stores",
    )
    .clone();
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(
        row.message.contains("elsewhere-d4e5f6") && row.message.contains("1 task bundle(s)"),
        "message names the partition and its bundles: {}",
        row.message
    );
    let remediation = row.remediation.expect("actionable row has remediation");
    assert!(
        remediation.contains("orbit task reindex"),
        "remediation points at recovery: {remediation}"
    );
    assert!(
        !remediation.contains("--fix-orphan-task-stores --confirm"),
        "remediation must not advertise the deletion repair: {remediation}"
    );

    assert_eq!(
        runtime.remove_orphan_task_stores().expect("run the repair"),
        OrphanTaskStoreRemoval::default()
    );
    assert!(
        populated.is_dir(),
        "the repair must not delete task bundles"
    );
}

/// [ORB-12143] A populated partition whose bound checkout cannot be stat-ed —
/// here because the checkout path resolves through a non-directory, as an
/// unmounted volume or an unsearchable parent does — is reported with the
/// filesystem failure and is never pointed at the deletion repair.
#[test]
fn unreachable_checkout_partition_is_reported_without_the_deletion_repair() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    let unreachable_root = temp.path().join("unreachable");
    fs::create_dir_all(unreachable_root.join(".orbit")).expect("create checkout");

    bind_task_partition(
        &global_root,
        "unreachable-a1b2c3",
        "unreachable",
        &unreachable_root,
    );
    write_task_bundle(&global_root, "unreachable-a1b2c3", "ORB-7");
    fs::remove_dir_all(&unreachable_root).expect("remove checkout directory");
    fs::write(&unreachable_root, b"not a directory").expect("write a file where the checkout was");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(
        row.message.contains("could not be reached")
            && row.message.contains("unreachable-a1b2c3")
            && row.message.contains("1 task bundle(s)"),
        "message names the partition, its bundles, and why it was kept: {}",
        row.message
    );
    let remediation = row
        .remediation
        .as_deref()
        .expect("actionable row has remediation");
    assert!(
        !remediation.contains("--fix-orphan-task-stores --confirm"),
        "remediation must not advertise the deletion repair: {remediation}"
    );

    assert_eq!(
        runtime.remove_orphan_task_stores().expect("run the repair"),
        OrphanTaskStoreRemoval::default()
    );
    assert!(
        task_workspaces_dir(&global_root)
            .join("unreachable-a1b2c3")
            .join("ORB-7")
            .is_dir(),
        "the repair must not delete task bundles it cannot prove are abandoned"
    );
}
