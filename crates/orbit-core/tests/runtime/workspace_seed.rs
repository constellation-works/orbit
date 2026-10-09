#![allow(missing_docs, clippy::expect_used)]

//! Workspace-local defaults: create-only seeding preserves existing definitions,
//! and sync serializes settings-only migrations with runtime edits.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::Duration;

use orbit_automation::auto_tasks::settings::load_settings_table;
use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_common::protocol::yaml::{parse_auto_task_yaml, parse_routine_yaml};
use orbit_common::security::release::sha256_hex;
use orbit_core::application::auto_tasks::AutoTaskBody;
use orbit_core::bootstrap::init::{InitOptions, init_workspace_at_root};
use orbit_core::{
    ManagedArtifactOutcome, ManagedArtifactScope, OrbitRuntime, RoutineSeedIdentity,
    reconcile_workspace_managed_artifacts, seed_absent_workspace_managed_artifacts,
};
use orbit_store::compose::auto_task::cursor_state_path;

use super::dispatch_admission::isolated;

const MANIFEST: &str = ".orbit-managed-assets.json";

fn identity() -> RoutineSeedIdentity {
    RoutineSeedIdentity::new("repo", "hm_test", "main").expect("routine seed identity")
}

/// A host whose global catalogs and one registered workspace are fully seeded.
fn seeded_host(dir: &Path) -> (PathBuf, PathBuf) {
    let global = dir.join("global");
    let workspace = dir.join("repo/.orbit");
    init_workspace_at_root(
        &global,
        InitOptions {
            global_only: true,
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .expect("global defaults");
    init_workspace_at_root(
        &workspace,
        InitOptions {
            refresh_defaults: true,
            global_root_override: Some(global.clone()),
            routine_seed_identity: Some(identity()),
            ..Default::default()
        },
    )
    .expect("workspace defaults");
    (global, workspace)
}

/// Make the catalog look like one a previous release wrote: the shipped
/// definition is gone, and so is the provenance recording it.
fn forget_shipped_default(catalog: &Path, name: &str) {
    std::fs::remove_file(catalog.join(format!("{name}.yaml"))).expect("remove shipped default");
    let manifest_path = catalog.join(MANIFEST);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    manifest["assets"]
        .as_object_mut()
        .expect("assets object")
        .remove(name);
    if let Some(provenance) = manifest
        .get_mut("routineProvenance")
        .and_then(serde_json::Value::as_object_mut)
    {
        provenance.remove(name);
    }
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write manifest");
}

/// Write `content` as a definition an earlier release shipped and recorded,
/// still unedited: a full sync would retire it, create-only must not.
fn record_unedited_retired_default(catalog: &Path, name: &str, content: &str) {
    std::fs::write(catalog.join(format!("{name}.yaml")), content).expect("write retired default");
    let manifest_path = catalog.join(MANIFEST);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    manifest["assets"]
        .as_object_mut()
        .expect("assets object")
        .insert(
            name.to_string(),
            serde_json::json!(sha256_hex(content.as_bytes())),
        );
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write manifest");
}

/// The bytes of every definition currently in `catalog`. The manifest is
/// excluded: recording the created defaults is expected to rewrite it.
fn snapshot(catalog: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    std::fs::read_dir(catalog)
        .expect("read catalog")
        .map(|entry| entry.expect("catalog entry").path())
        .filter(|path| path.is_file() && path.file_name() != Some(std::ffi::OsStr::new(MANIFEST)))
        .map(|path| {
            let bytes = std::fs::read(&path).expect("read definition");
            (path, bytes)
        })
        .collect()
}

#[test]
fn create_only_seed_creates_absent_defaults_and_never_rewrites_existing_definitions() {
    let dir = tempfile::tempdir().expect("isolated roots");
    let (global, workspace) = seeded_host(dir.path());
    let routines = workspace.join("routines");
    let auto_tasks = workspace.join("auto_tasks");

    // A release that predates `store_gc` and `friction-curation`.
    forget_shipped_default(&routines, "store_gc");
    forget_shipped_default(&auto_tasks, "friction-curation");
    record_unedited_retired_default(&auto_tasks, "retired-default", "name: retired-default\n");

    // Operator edits to a shipped default, plus files the operator wrote.
    let edited_routine = routines.join("task_pilot.yaml");
    let edited_auto_task = auto_tasks.join("qa-sweep.yaml");
    std::fs::write(
        &edited_routine,
        format!(
            "{}\n# operator note\n",
            std::fs::read_to_string(&edited_routine).expect("read task_pilot")
        ),
    )
    .expect("edit routine");
    std::fs::write(
        &edited_auto_task,
        format!(
            "{}\n# operator note\n",
            std::fs::read_to_string(&edited_auto_task).expect("read qa-sweep")
        ),
    )
    .expect("edit auto-task");
    std::fs::write(routines.join("my-routine.yaml"), "user-authored\n").expect("user routine");
    std::fs::write(auto_tasks.join("my-auto-task.yaml"), "user-authored\n")
        .expect("user auto-task");

    let before_routines = snapshot(&routines);
    let before_auto_tasks = snapshot(&auto_tasks);

    let report = seed_absent_workspace_managed_artifacts(&global, &workspace, &identity(), "main")
        .expect("create-only seed");

    let mut created: Vec<(String, String)> = report
        .actions
        .iter()
        .filter(|action| action.outcome == ManagedArtifactOutcome::Created)
        .map(|action| (action.kind.clone(), action.name.clone()))
        .collect();
    created.sort();
    assert_eq!(
        created,
        vec![
            ("auto_task".to_string(), "friction-curation".to_string()),
            ("routine".to_string(), "store_gc".to_string()),
        ],
        "only the absent defaults are created: {report:?}"
    );
    assert!(
        report
            .actions
            .iter()
            .filter(|action| action.outcome == ManagedArtifactOutcome::Created)
            .all(|action| action.scope == ManagedArtifactScope::WorkspaceLocal),
        "created defaults are workspace-local: {report:?}"
    );
    assert!(
        report
            .actions
            .iter()
            .all(|action| action.outcome != ManagedArtifactOutcome::Refreshed
                && action.outcome != ManagedArtifactOutcome::Retired),
        "create-only never refreshes or retires: {report:?}"
    );

    // Every definition that existed keeps its exact bytes.
    for before in [before_routines, before_auto_tasks] {
        for (path, bytes) in before {
            assert_eq!(
                std::fs::read(&path).expect("definition survives"),
                bytes,
                "create-only rewrote existing '{}'",
                path.display()
            );
        }
    }

    // A created routine keeps the `enabled` value it ships with.
    let shipped = std::fs::read_to_string(routines.join("store_gc.yaml")).expect("store_gc");
    assert!(
        !parse_routine_yaml(&shipped)
            .expect("store_gc parses")
            .enabled,
        "store_gc ships disabled and must stay that way"
    );

    // The created defaults are recorded with their shipped content, so a
    // convergent check sees them as already in place.
    let check =
        reconcile_workspace_managed_artifacts(&global, &workspace, Some(&identity()), "main", true)
            .expect("convergence check");
    assert!(
        check
            .actions
            .iter()
            .filter(|action| action.name == "store_gc" || action.name == "friction-curation")
            .all(|action| action.outcome == ManagedArtifactOutcome::Unchanged),
        "a created default was not recorded as shipped: {check:?}"
    );

    // A second pass has nothing to do.
    let again = seed_absent_workspace_managed_artifacts(&global, &workspace, &identity(), "main")
        .expect("repeat create-only seed");
    assert!(
        again
            .actions
            .iter()
            .all(|action| action.outcome != ManagedArtifactOutcome::Created),
        "a repeated seed creates nothing: {again:?}"
    );
}

#[test]
fn settings_only_migration_waits_for_crud_and_preserves_concurrent_settings() {
    if !isolated(
        "workspace_seed::settings_only_migration_waits_for_crud_and_preserves_concurrent_settings",
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("isolated roots");
    let (global, workspace) = seeded_host(dir.path());
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("workspace runtime");
    let auto_tasks = workspace.join("auto_tasks");
    let migrated_name = "friction-curation";
    let edited_name = "qa-sweep";
    let body_path = auto_tasks.join(format!("{migrated_name}.yaml"));
    let bundled_body = std::fs::read_to_string(&body_path).expect("bundled body");
    let mut fork = parse_auto_task_yaml(&bundled_body).expect("bundled definition");
    fork.enabled = true;
    let fork_body = serde_yaml::to_string(&fork).expect("settings-only fork");
    std::fs::write(&body_path, &fork_body).expect("persist legacy settings-only fork");
    runtime
        .auto_task_toggle(edited_name, true)
        .expect("existing settings entry for another managed default");

    // Sync can reach the same workspace through an aliased ancestor; the lock
    // must still be the one runtime CRUD holds through its canonical roots.
    #[cfg(unix)]
    let sync_workspace = {
        let alias = dir.path().join("repo-alias");
        std::os::unix::fs::symlink(workspace.parent().expect("repository"), &alias)
            .expect("workspace alias");
        alias.join(".orbit")
    };
    #[cfg(not(unix))]
    let sync_workspace = workspace.clone();

    let (check_tx, check_rx) = sync_channel(1);
    let (done_tx, done_rx) = sync_channel(1);
    let (worker, edited_settings) = with_exclusive_file_lock(
        &cursor_state_path(&runtime.paths().state_dir),
        "auto-task cursor",
        || {
            let worker = std::thread::spawn(move || {
                let check = reconcile_workspace_managed_artifacts(
                    &global,
                    &sync_workspace,
                    None,
                    "main",
                    true,
                );
                check_tx.send(check).expect("report read-only check");
                let applied = reconcile_workspace_managed_artifacts(
                    &global,
                    &sync_workspace,
                    None,
                    "main",
                    false,
                );
                done_tx.send(applied).expect("report migration");
            });
            let check = check_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("check mode must not wait for the cursor lock")
                .expect("read-only convergence check");
            assert!(check.actions.iter().any(|action| {
                action.name == migrated_name && action.outcome == ManagedArtifactOutcome::Migrated
            }));
            assert!(
                matches!(
                    done_rx.recv_timeout(Duration::from_secs(1)),
                    Err(RecvTimeoutError::Timeout)
                ),
                "sync must wait for the CRUD cursor lock before loading and replacing settings"
            );
            assert_eq!(std::fs::read_to_string(&body_path).unwrap(), fork_body);

            // The public edit re-enters this thread's lock while sync is
            // waiting. Migration must load this committed table after release,
            // preventing an edit between its table load and replacement.
            let edited = runtime
                .auto_task_toggle(edited_name, false)
                .expect("concurrent CRUD edit commits");
            assert!(!edited.enabled);
            let settings = load_settings_table(&auto_tasks).expect("committed settings");
            assert!(!settings.contains_key(migrated_name));
            Ok::<_, OrbitError>((worker, settings[edited_name].clone()))
        },
    )
    .expect("hold the cursor lock across the concurrent edit");

    let report = done_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("migration completes after lock release")
        .expect("apply sync");
    worker.join().expect("sync thread");
    assert!(report.actions.iter().any(|action| {
        action.name == migrated_name && action.outcome == ManagedArtifactOutcome::Migrated
    }));
    let settings = load_settings_table(&auto_tasks).expect("settings after migration");
    assert_eq!(settings[migrated_name].enabled, Some(true));
    assert_eq!(settings[edited_name], edited_settings);
    assert_eq!(std::fs::read_to_string(&body_path).unwrap(), bundled_body);
    assert!(
        runtime
            .auto_task_show(migrated_name)
            .unwrap()
            .unwrap()
            .enabled
    );
    assert!(
        !runtime
            .auto_task_show(edited_name)
            .unwrap()
            .unwrap()
            .enabled
    );
    assert_eq!(
        runtime.auto_task_layering(migrated_name).unwrap().body,
        AutoTaskBody::Managed
    );
}
