#![allow(missing_docs, clippy::expect_used)]

//! Workspace-local defaults: create-only seeding preserves existing definitions,
//! and reconciliation serializes migrations and manifests with runtime edits.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::Duration;

use orbit_automation::auto_tasks::settings::load_settings_table;
use orbit_common::OrbitError;
use orbit_common::fs::io::{
    FileLockOptions, with_exclusive_file_lock, with_exclusive_file_lock_options,
};
use orbit_common::protocol::yaml::{parse_auto_task_yaml, parse_routine_yaml};
use orbit_common::security::release::sha256_hex;
use orbit_core::application::auto_tasks::AutoTaskBody;
use orbit_core::bootstrap::init::{InitOptions, init_workspace_at_root};
use orbit_core::test_support::with_auto_task_manifest_read_hook;
use orbit_core::{
    AutoTaskDeleteParams, ManagedArtifactOutcome, ManagedArtifactScope, OrbitRuntime,
    RoutineSeedIdentity, reconcile_workspace_managed_artifacts,
    seed_absent_workspace_managed_artifacts,
};
use orbit_store::compose::auto_task::cursor_state_path;
use orbit_types::workflow::AutoTaskSchedule;

const MANIFEST: &str = ".orbit-managed-assets.json";

fn routine_adoption_requires_the_seeded_name_and_preserves_collisions_across_refresh() {
    let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("isolated roots");
    let (global, workspace) = seeded_host(dir.path());
    let catalog = workspace.join("routines");
    let matching_path = catalog.join("task_pilot.yaml");
    let collision_path = catalog.join("store_gc.yaml");
    let matching_shipped = std::fs::read_to_string(&matching_path).expect("shipped task pilot");
    let mut matching: serde_yaml::Value =
        serde_yaml::from_str(&matching_shipped).expect("task pilot document");
    matching["description"] = "operator-customized pilot".into();
    matching["enabled"] = true.into();
    matching["trigger"]["state"]["owner_machine"] = "hm_operator".into();
    matching["trigger"]["state"]["branch"] = "operator-branch".into();
    let matching_body = serde_yaml::to_string(&matching).expect("customized pilot");
    std::fs::write(&matching_path, &matching_body).expect("write matching-name routine");

    let mut collision: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&collision_path).expect("shipped store gc"))
            .expect("store gc document");
    collision["name"] = "operator-retention".into();
    collision["description"] = "operator-authored retention".into();
    collision["enabled"] = true.into();
    let collision_body = serde_yaml::to_string(&collision).expect("operator routine");
    std::fs::write(&collision_path, &collision_body).expect("write different-name routine");
    let manifest_path = catalog.join(MANIFEST);
    std::fs::remove_file(&manifest_path).expect("simulate a pre-provenance catalog");

    let sync = || {
        reconcile_workspace_managed_artifacts(&global, &workspace, Some(&identity()), "main", false)
            .expect("sync routines")
    };
    let report = sync();
    assert!(report.actions.iter().any(|action| {
        action.kind == "routine"
            && action.name == "task_pilot"
            && action.outcome == ManagedArtifactOutcome::Migrated
    }));
    assert!(
        report.actions.iter().any(|action| {
            action.kind == "routine"
                && action.name == "store_gc"
                && action.outcome == ManagedArtifactOutcome::Preserved
                && action.detail.is_some()
        }),
        "a differently named routine must be reported as a collision [ORB-15752]: {report:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&matching_path).expect("adopted bytes"),
        matching_body
    );
    assert_eq!(
        std::fs::read_to_string(&collision_path).expect("collision bytes"),
        collision_body
    );

    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("adoption manifest"))
            .expect("parse adoption manifest");
    assert!(manifest["assets"].get("store_gc").is_none());
    assert!(manifest["routineProvenance"].get("store_gc").is_none());
    let provenance = &mut manifest["routineProvenance"]["task_pilot"];
    assert_eq!(
        provenance["renderedDigest"],
        sha256_hex(matching_body.as_bytes())
    );
    assert_eq!(
        provenance["binding"]["name"],
        matching["name"].as_str().expect("routine name")
    );
    assert_eq!(provenance["binding"]["ownerMachine"], "hm_operator");
    assert_eq!(provenance["binding"]["branch"], "operator-branch");
    // Simulate an adopted instance from an older release: the next sync sees
    // a changed shipped template while the recorded on-disk digest is intact.
    provenance["templateDigest"] = sha256_hex(b"previous shipped template").into();
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("old template provenance"),
    )
    .expect("record previous release");

    let refreshed = sync();
    assert!(refreshed.actions.iter().any(|action| {
        action.kind == "routine"
            && action.name == "task_pilot"
            && action.outcome == ManagedArtifactOutcome::Refreshed
    }));
    assert!(refreshed.actions.iter().any(|action| {
        action.kind == "routine"
            && action.name == "store_gc"
            && action.outcome == ManagedArtifactOutcome::Preserved
    }));
    let mut expected: serde_yaml::Value =
        serde_yaml::from_str(&matching_shipped).expect("current template");
    expected["enabled"] = matching["enabled"].clone();
    expected["trigger"]["state"]["owner_machine"] =
        matching["trigger"]["state"]["owner_machine"].clone();
    expected["trigger"]["state"]["branch"] = matching["trigger"]["state"]["branch"].clone();
    let observed: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&matching_path).expect("refreshed pilot"))
            .expect("refreshed definition");
    assert_eq!(
        observed, expected,
        "refresh must use the adopted on-disk binding"
    );
    assert_eq!(
        std::fs::read_to_string(&collision_path).expect("preserved operator routine"),
        collision_body,
        "a later template change must not replace the differently named file [ORB-15752]"
    );
}

fn reconciliation_preserves_concurrent_delete_opt_out() {
    for create_only in [false, true] {
        reconcile_during_lifecycle_change(create_only, false);
    }
}

fn reconciliation_preserves_concurrent_restore_provenance() {
    for create_only in [false, true] {
        reconcile_during_lifecycle_change(create_only, true);
    }
}

/// Pause after reconciliation has read its manifest, then admit a competing
/// lifecycle writer. Without exclusion, let that writer commit before sync
/// resumes, reproducing the stale snapshot deterministically [ORB-15609].
fn reconcile_during_lifecycle_change(create_only: bool, restoring: bool) {
    let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("isolated roots");
    let (global, workspace) = seeded_host(dir.path());
    let runtime =
        Arc::new(OrbitRuntime::from_roots(&global, &workspace).expect("workspace runtime"));
    let catalog = workspace.join("auto_tasks");
    let name = "qa-sweep";
    let path = catalog.join(format!("{name}.yaml"));
    let shipped = std::fs::read(&path).expect("shipped definition");
    let delete_params = || AutoTaskDeleteParams {
        name: name.to_string(),
        ..Default::default()
    };
    if restoring {
        runtime
            .auto_task_delete(delete_params())
            .expect("initial opt-out");
    }
    // Force a later manifest replacement; an otherwise unchanged catalog
    // could conceal the stale-snapshot bug in either mode.
    forget_shipped_default(&catalog, "friction-curation");

    #[cfg(unix)]
    let sync_workspace = {
        let alias = dir.path().join("repo-alias");
        std::os::unix::fs::symlink(workspace.parent().expect("repository"), &alias)
            .expect("workspace alias");
        alias.join(".orbit")
    };
    #[cfg(not(unix))]
    let sync_workspace = workspace.clone();

    let (read_tx, read_rx) = sync_channel(1);
    let (resume_tx, resume_rx) = sync_channel(1);
    let (started_tx, started_rx) = sync_channel(1);
    let (done_tx, done_rx) = sync_channel(1);
    std::thread::scope(|scope| {
        let sync = scope.spawn(|| {
            with_auto_task_manifest_read_hook(
                move || {
                    read_tx.send(()).expect("manifest read rendezvous");
                    resume_rx
                        .recv_timeout(Duration::from_secs(30))
                        .expect("release paused reconciliation");
                },
                || {
                    if create_only {
                        seed_absent_workspace_managed_artifacts(
                            &global,
                            &sync_workspace,
                            &identity(),
                            "main",
                        )
                    } else {
                        reconcile_workspace_managed_artifacts(
                            &global,
                            &sync_workspace,
                            None,
                            "main",
                            false,
                        )
                    }
                },
            )
        });
        read_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("sync loaded its manifest");
        // A nonblocking probe proves exclusion at the precise read/write
        // boundary without relying on how quickly the writer is scheduled.
        let lock = with_exclusive_file_lock_options(
            &cursor_state_path(&runtime.paths().state_dir),
            "auto-task cursor",
            FileLockOptions {
                timeout: Duration::ZERO,
                ..Default::default()
            },
            || Ok::<_, std::io::Error>(()),
        );
        let locked = match lock {
            Ok(()) => false,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => true,
            Err(error) => panic!("cursor lock probe failed: {error}"),
        };
        let writer = scope.spawn(|| {
            started_tx.send(()).expect("writer admission rendezvous");
            let outcome = if restoring {
                runtime
                    .auto_task_restore(name)
                    .map(|definition| assert!(!definition.enabled))
            } else {
                runtime
                    .auto_task_delete(delete_params())
                    .map(|report| assert!(report.opted_out))
            };
            done_tx.send(outcome).expect("lifecycle writer result");
        });
        started_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("writer started after manifest read");
        let early = if locked {
            match done_rx.recv_timeout(Duration::from_secs(1)) {
                Err(RecvTimeoutError::Timeout) => None,
                outcome => Some(outcome.expect("writer channel")),
            }
        } else {
            Some(
                done_rx
                    .recv_timeout(Duration::from_secs(30))
                    .expect("unlocked writer commits"),
            )
        };
        resume_tx.send(()).expect("resume sync");
        let report = sync
            .join()
            .expect("sync thread")
            .expect("reconcile defaults");
        let wrote_early = early.is_some();
        early
            .unwrap_or_else(|| {
                done_rx
                    .recv_timeout(Duration::from_secs(30))
                    .expect("writer completes after sync")
            })
            .expect("lifecycle operation succeeds");
        writer.join().expect("writer thread");
        assert!(
            report.actions.iter().any(|action| {
                action.name == "friction-curation"
                    && action.outcome == ManagedArtifactOutcome::Created
            }),
            "sync must exercise a manifest replacement"
        );
        assert_lifecycle_manifest(&catalog, name, restoring, &shipped);
        assert!(
            locked,
            "reconciliation must hold CRUD's cursor sidecar through manifest replacement [ORB-15609]"
        );
        assert!(
            !wrote_early,
            "delete and restore must wait for reconciliation's manifest transaction [ORB-15609]"
        );
    });

    // A stale opt-out can stay hidden until the next convergence pass. Check
    // all entry points that could otherwise recreate a deleted default.
    reconcile_workspace_managed_artifacts(&global, &workspace, None, "main", false)
        .expect("later sync");
    seed_absent_workspace_managed_artifacts(&global, &workspace, &identity(), "main")
        .expect("later seed");
    init_workspace_at_root(
        &workspace,
        InitOptions {
            refresh_defaults: true,
            global_root_override: Some(global),
            routine_seed_identity: Some(identity()),
            ..Default::default()
        },
    )
    .expect("forced workspace initialization");
    assert_lifecycle_manifest(&catalog, name, restoring, &shipped);
    assert_eq!(
        runtime
            .auto_task_show(name)
            .expect("definition lookup")
            .is_some(),
        restoring
    );
}

fn assert_lifecycle_manifest(catalog: &Path, name: &str, restored: bool, shipped: &[u8]) {
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(catalog.join(MANIFEST)).expect("read lifecycle manifest"),
    )
    .expect("parse lifecycle manifest");
    let opted_out = manifest["optedOut"]
        .as_array()
        .is_some_and(|names| names.iter().any(|entry| entry == name));
    assert_eq!(
        opted_out, !restored,
        "concurrent reconciliation must preserve the operator's opt-out state [ORB-15609]"
    );
    let path = catalog.join(format!("{name}.yaml"));
    if restored {
        assert_eq!(std::fs::read(path).expect("restored definition"), shipped);
        assert_eq!(
            manifest["assets"][name],
            sha256_hex(shipped),
            "restored provenance must survive reconciliation"
        );
    } else {
        assert!(
            !path.exists(),
            "a concurrently deleted default must remain absent [ORB-15609]"
        );
        assert!(
            manifest["assets"].get(name).is_none(),
            "deleted default must have no managed provenance"
        );
    }
}

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
    let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("isolated roots");
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

fn implicit_runtime_open_and_workspace_init_preserve_legacy_skill_trees() {
    let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("isolated roots");
    let (global, workspace) = seeded_host(dir.path());
    let mut expected_files = Vec::new();

    for (layout, skills_dir) in [
        ("resources/skills", workspace.join("resources/skills")),
        ("skills", workspace.join("skills")),
    ] {
        for skill_id in ["orbit-pr", "orbit-approve-task"] {
            let tree = skills_dir.join(skill_id);
            std::fs::create_dir_all(&tree).expect("legacy skill tree");

            let skill = tree.join("SKILL.md");
            let skill_bytes = format!("# Operator-edited {skill_id}\n").into_bytes();
            std::fs::write(&skill, &skill_bytes).expect("operator skill edits");
            expected_files.push((layout, skill, skill_bytes));

            let note = tree.join("notes.txt");
            let note_bytes = format!("unknown operator file in {layout}/{skill_id}\n").into_bytes();
            std::fs::write(&note, &note_bytes).expect("unknown operator file");
            expected_files.push((layout, note, note_bytes));
        }
    }

    let roots = orbit_core::runtime::OrbitRuntimeRoots {
        global_root: global.clone(),
        shared_root: workspace.clone(),
        local_root: workspace.clone(),
    };
    OrbitRuntime::initialize_from_resolved_roots(roots, None).expect("implicit runtime open");

    for (layout, path, expected) in &expected_files {
        assert_eq!(
            std::fs::read(path).expect("legacy skill file survives runtime open"),
            *expected,
            "runtime open changed {layout}/{}",
            path.strip_prefix(workspace.join(layout))
                .expect("file belongs to legacy layout")
                .display()
        );
    }

    init_workspace_at_root(
        &workspace,
        InitOptions {
            global_root_override: Some(global),
            ..Default::default()
        },
    )
    .expect("explicit workspace initialization");

    for (layout, path, expected) in expected_files {
        assert_eq!(
            std::fs::read(&path).expect("legacy skill file survives workspace init"),
            expected,
            "workspace init changed {layout}/{}",
            path.strip_prefix(workspace.join(layout))
                .expect("file belongs to legacy layout")
                .display()
        );
    }
}

fn settings_only_migration_waits_for_crud_and_preserves_concurrent_settings() {
    let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("isolated roots");
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

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    routine_adoption_requires_the_seeded_name_and_preserves_collisions_across_refresh_scenarios,
    routine_adoption_requires_the_seeded_name_and_preserves_collisions_across_refresh,
    reconciliation_preserves_concurrent_delete_opt_out
);

crate::scenario_table!(
    reconciliation_preserves_concurrent_restore_provenance_scenarios,
    reconciliation_preserves_concurrent_restore_provenance,
    implicit_runtime_open_and_workspace_init_preserve_legacy_skill_trees,
    settings_only_migration_waits_for_crud_and_preserves_concurrent_settings
);

/// Blocked-task triage is retired: a fresh workspace seeds no
/// `blocked_task_triage` routine, nor its sibling pilot-warning routine since
/// [ORB-22844], and a full sync retires an unedited copy an earlier release wrote
/// instead of keeping a routine on the retired `block_escalated` trigger.
#[test]
fn blocked_task_triage_is_not_seeded_and_an_unedited_earlier_copy_is_retired_by_sync() {
    let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("isolated roots");
    let (global, workspace) = seeded_host(dir.path());
    let catalog = workspace.join("routines");
    assert!(
        !catalog.join("blocked_task_triage.yaml").exists(),
        "a fresh workspace seeds no blocked-task triage routine"
    );
    assert!(
        !catalog.join("pilot_warning_triage.yaml").exists(),
        "a fresh workspace seeds no pilot-warning triage routine"
    );

    let earlier = include_str!("../../assets/routines/retired/blocked_task_triage.yaml");
    record_unedited_retired_default(&catalog, "blocked_task_triage", earlier);
    let report = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&identity()),
        "main",
        false,
    )
    .expect("full sync");
    assert!(
        report.actions.iter().any(|action| {
            action.kind == "routine"
                && action.name == "blocked_task_triage"
                && action.outcome == ManagedArtifactOutcome::Retired
        }),
        "an unedited earlier copy is retired: {report:?}"
    );
    assert!(!catalog.join("blocked_task_triage.yaml").exists());
}

/// An unmanifested pilot-warning copy retires by content [ORB-22889]: the
/// old shipped `pilot_warning_triage` template targets the still-shipped
/// `escalation_triage_pipeline` job on the retired `pilot_warnings_held`
/// trigger, so the untracked guard must accept the retired trigger, not only
/// a retired job. Sync check reports the pending retirement, apply removes
/// the file with a recovery copy, and a second check converges.
#[test]
fn unmanifested_pilot_warning_triage_is_retired_by_sync_with_recovery_copy() {
    use orbit_automation::routines::loader::retired_routine_job_reason;

    assert!(
        retired_routine_job_reason("escalation_triage_pipeline").is_none(),
        "manual triage still ships, so the retired pilot-warning template retires by trigger [ORB-22889]"
    );

    for enabled in [false, true] {
        let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
            .expect("isolated roots");
        let (global, workspace) = seeded_host(dir.path());
        let catalog = workspace.join("routines");
        assert!(
            !catalog.join("pilot_warning_triage.yaml").exists(),
            "a fresh workspace seeds no pilot-warning triage routine"
        );

        let template = include_str!("../../assets/routines/retired/pilot_warning_triage.yaml");
        let mut body = template
            .replace("__ORBIT_ROUTINE_NAME__", "pilot-warning-triage-repo")
            .replace("__ORBIT_OWNER_MACHINE__", "hm_test")
            .replace("__ORBIT_BASE_BRANCH__", "main");
        if enabled {
            body = body.replacen("enabled: false", "enabled: true", 1);
        }
        let definition = parse_routine_yaml(&body).expect("unmanifested pilot-warning parses");
        assert!(
            definition.enabled == enabled,
            "fixture keeps enabled={enabled} [ORB-22889]"
        );
        // No manifest entry: an earlier release wrote the file without
        // recording it.
        std::fs::write(catalog.join("pilot_warning_triage.yaml"), &body)
            .expect("write unmanifested retired default");

        let check = reconcile_workspace_managed_artifacts(
            &global,
            &workspace,
            Some(&identity()),
            "main",
            true,
        )
        .expect("sync check");
        assert!(
            check.actions.iter().any(|action| {
                action.kind == "routine"
                    && action.name == "pilot_warning_triage"
                    && action.outcome == ManagedArtifactOutcome::Retired
            }),
            "sync --check reports the pending retirement (enabled={enabled}): {check:?}"
        );

        let applied = reconcile_workspace_managed_artifacts(
            &global,
            &workspace,
            Some(&identity()),
            "main",
            false,
        )
        .expect("apply sync");
        assert!(
            applied.actions.iter().any(|action| {
                action.kind == "routine"
                    && action.name == "pilot_warning_triage"
                    && action.outcome == ManagedArtifactOutcome::Retired
            }),
            "apply retires the unmanifested copy (enabled={enabled}): {applied:?}"
        );
        assert!(
            !catalog.join("pilot_warning_triage.yaml").exists(),
            "apply removes the retired definition from the active catalog (enabled={enabled})"
        );
        let preserved = workspace
            .join(".retired-managed")
            .join("routines")
            .join("pilot_warning_triage.yaml");
        assert!(
            preserved.is_file(),
            "apply preserves a recovery copy outside the active catalog (enabled={enabled})"
        );
        assert_eq!(
            std::fs::read_to_string(&preserved).expect("read recovery copy"),
            body,
            "the recovery copy keeps the exact bytes (enabled={enabled})"
        );

        let converged = reconcile_workspace_managed_artifacts(
            &global,
            &workspace,
            Some(&identity()),
            "main",
            true,
        )
        .expect("second sync check");
        assert!(
            !converged.actions.iter().any(|action| {
                action.kind == "routine"
                    && action.name == "pilot_warning_triage"
                    && action.outcome == ManagedArtifactOutcome::Retired
            }),
            "a second sync --check converges (enabled={enabled}): {converged:?}"
        );
    }

    // A genuinely customized copy on the retired trigger stays in place with
    // actionable advice instead of retiring.
    let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("isolated roots");
    let (global, workspace) = seeded_host(dir.path());
    let catalog = workspace.join("routines");
    let template = include_str!("../../assets/routines/retired/pilot_warning_triage.yaml");
    let customized = template
        .replace("__ORBIT_ROUTINE_NAME__", "pilot-warning-triage-repo")
        .replace("__ORBIT_OWNER_MACHINE__", "hm_test")
        .replace("__ORBIT_BASE_BRANCH__", "main")
        .replacen(
            "Triage proposed tasks",
            "Operator-customized pilot-warning triage",
            1,
        );
    assert_ne!(
        customized, template,
        "the customized copy differs in a template-owned field"
    );
    std::fs::write(catalog.join("pilot_warning_triage.yaml"), &customized)
        .expect("write customized retired-trigger copy");
    let report = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&identity()),
        "main",
        false,
    )
    .expect("sync customized copy");
    let preserved = report
        .actions
        .iter()
        .find(|action| {
            action.kind == "routine"
                && action.name == "pilot_warning_triage"
                && action.outcome == ManagedArtifactOutcome::Preserved
        })
        .expect("a customized copy is preserved, not retired: {report:?}");
    let detail = preserved.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("delete") && detail.contains("retarget"),
        "the preserved copy names the step that clears it: {report:?}"
    );
    assert!(
        catalog.join("pilot_warning_triage.yaml").is_file(),
        "the customized definition stays in the active catalog"
    );
}

/// Re-run `orbit workspace init` over an existing workspace.
fn reinit_workspace(global: &Path, workspace: &Path) {
    init_workspace_at_root(
        workspace,
        InitOptions {
            refresh_defaults: true,
            global_root_override: Some(global.to_path_buf()),
            routine_seed_identity: Some(identity()),
            ..Default::default()
        },
    )
    .expect("workspace re-init");
}

fn listed_enabled(runtime: &OrbitRuntime, name: &str) -> bool {
    runtime
        .auto_task_list()
        .expect("list auto-tasks")
        .into_iter()
        .find(|definition| definition.name == name)
        .unwrap_or_else(|| panic!("`{name}` is listed"))
        .enabled
}

#[test]
fn fresh_workspace_seeds_only_host_health_enabled() {
    let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("isolated roots");
    let (global, workspace) = seeded_host(dir.path());
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("workspace runtime");
    let listed: BTreeMap<String, _> = runtime
        .auto_task_list()
        .expect("list auto-tasks")
        .into_iter()
        .map(|definition| (definition.name.clone(), definition))
        .collect();

    for name in orbit_config::SHIPPED_DEFAULT_AUTO_TASKS {
        let definition = listed
            .get(*name)
            .unwrap_or_else(|| panic!("shipped default `{name}` is seeded: {listed:?}"));
        if *name == "host-health" {
            assert!(definition.enabled, "host-health seeds enabled");
            assert!(
                matches!(
                    &definition.schedule,
                    AutoTaskSchedule::Cron { cron, .. } if cron == "10 7 * * *"
                ),
                "host-health keeps its shipped schedule: {:?}",
                definition.schedule
            );
        } else {
            assert!(
                !definition.enabled,
                "shipped default `{name}` seeds disabled"
            );
        }
    }
}

#[test]
fn reinit_and_sync_preserve_an_operator_edited_host_health_definition() {
    let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("isolated roots");
    let (global, workspace) = seeded_host(dir.path());
    let path = workspace.join("auto_tasks/host-health.yaml");
    let edited = format!(
        "{}\n# operator note: keep this off on this host\n",
        std::fs::read_to_string(&path)
            .expect("read host-health")
            .replacen("enabled: true", "enabled: false", 1)
    );
    std::fs::write(&path, &edited).expect("operator edit");

    reinit_workspace(&global, &workspace);
    seed_absent_workspace_managed_artifacts(&global, &workspace, &identity(), "main")
        .expect("create-only seed");
    reconcile_workspace_managed_artifacts(&global, &workspace, Some(&identity()), "main", false)
        .expect("full sync");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read host-health"),
        edited,
        "an operator-edited host-health definition keeps its exact bytes"
    );
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("workspace runtime");
    assert!(!listed_enabled(&runtime, "host-health"));
}

#[test]
fn sync_enables_an_unconfigured_earlier_host_health_copy_and_a_toggle_decides_after() {
    let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("isolated roots");
    let (global, workspace) = seeded_host(dir.path());
    let catalog = workspace.join("auto_tasks");
    let path = catalog.join("host-health.yaml");
    // The copy an earlier release wrote and recorded: the same body, disabled.
    let earlier = std::fs::read_to_string(&path)
        .expect("read host-health")
        .replacen("enabled: true", "enabled: false", 1);
    record_unedited_retired_default(&catalog, "host-health", &earlier);

    // `orbit init` only creates absent defaults, so it leaves the copy alone.
    seed_absent_workspace_managed_artifacts(&global, &workspace, &identity(), "main")
        .expect("create-only seed");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), earlier);

    // No operator configured it, so convergence turns it on.
    reconcile_workspace_managed_artifacts(&global, &workspace, Some(&identity()), "main", false)
        .expect("full sync");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("workspace runtime");
    assert!(listed_enabled(&runtime, "host-health"));

    // Once an operator sets the flag, that flag decides across sync and re-init.
    runtime
        .auto_task_toggle("host-health", false)
        .expect("toggle host-health off");
    reconcile_workspace_managed_artifacts(&global, &workspace, Some(&identity()), "main", false)
        .expect("full sync");
    reinit_workspace(&global, &workspace);
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("workspace runtime");
    assert!(!listed_enabled(&runtime, "host-health"));
}

/// A fresh managed skill install writes every file under `assets/skills`, and
/// every relative link in the installed skill trees resolves [ORB-22531].
/// The install manifest silently dropped five shipped references that other
/// shipped pages link to, so this guards the install boundary rather than
/// pinning prose.
#[test]
fn fresh_skill_install_writes_every_shipped_reference_and_resolves_links() {
    let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("isolated roots");
    let (global, _workspace) = seeded_host(dir.path());
    let skills_root = global.join("skills");

    let assets_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/skills");
    let mut shipped = Vec::new();
    collect_tree_files(&assets_root, &assets_root, &mut shipped);
    assert!(
        !shipped.is_empty(),
        "skill assets must exist under {}",
        assets_root.display()
    );
    let missing: Vec<String> = shipped
        .iter()
        .filter(|relative| !skills_root.join(relative).is_file())
        .map(|relative| relative.display().to_string())
        .collect();
    assert!(
        missing.is_empty(),
        "a fresh skill install dropped shipped references [ORB-22531]: {missing:?}"
    );
    for required in [
        "orbit/references/attention.md",
        "orbit-setup/references/applications.md",
        "orbit-setup/references/browser-checks.md",
        "orbit-setup/references/macos.md",
        "orbit-setup/references/providers.md",
    ] {
        assert!(
            skills_root.join(required).is_file(),
            "a fresh skill install must write {required} [ORB-22531]"
        );
    }

    let mut dangling = Vec::new();
    let mut checked = 0;
    for skill_id in ["orbit", "orbit-orchestrate", "orbit-setup"] {
        let tree = skills_root.join(skill_id);
        let mut pages = Vec::new();
        collect_tree_files(&tree, &tree, &mut pages);
        pages.sort();
        for page in pages {
            if page.extension().is_none_or(|extension| extension != "md") {
                continue;
            }
            let absolute = tree.join(&page);
            let body = std::fs::read_to_string(&absolute).expect("installed skill page");
            for target in markdown_link_targets(&body) {
                let Some(resolved) = resolve_skill_link(&absolute, target) else {
                    continue;
                };
                checked += 1;
                if !resolved.is_file() {
                    dangling.push(format!("{} -> {target}", absolute.display()));
                }
            }
        }
    }
    assert!(
        checked > 0,
        "installed skill trees must contain relative links to check"
    );
    assert!(
        dangling.is_empty(),
        "installed skill trees carry dangling relative links [ORB-22531]: {dangling:?}"
    );
}

/// Every regular file under `dir`, relative to `base`.
fn collect_tree_files(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read skill tree") {
        let path = entry.expect("skill tree entry").path();
        if path.is_dir() {
            collect_tree_files(&path, base, out);
        } else if path.is_file() {
            out.push(
                path.strip_prefix(base)
                    .expect("skill tree under base")
                    .to_path_buf(),
            );
        }
    }
}

/// The destinations of inline Markdown links in `body`, ignoring fenced code
/// blocks and inline code spans that may quote link syntax.
fn markdown_link_targets(body: &str) -> Vec<&str> {
    let mut targets = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        // Keep the prose between backticks; a quoted `[text](target)`
        // example is not a link the reader can follow.
        for (index, span) in line.split('`').enumerate() {
            if index % 2 == 1 {
                continue;
            }
            let mut rest = span;
            while let Some(open) = rest.find("](") {
                let after = &rest[open + 2..];
                let Some(close) = after.find(')') else {
                    break;
                };
                targets.push(after[..close].trim());
                rest = &after[close + 1..];
            }
        }
    }
    targets
}

/// Resolve a Markdown link against the installed page that carries it.
/// Returns `None` for same-page anchors, external URLs and templates.
fn resolve_skill_link(page: &Path, target: &str) -> Option<PathBuf> {
    let target = target
        .strip_prefix('<')
        .and_then(|inner| inner.strip_suffix('>'))
        .unwrap_or(target)
        .trim();
    if target.is_empty() || target.starts_with('#') {
        return None;
    }
    if target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with("mailto:")
    {
        return None;
    }
    if target.contains('{') || target.contains('$') {
        return None;
    }
    let file = target.split('#').next().expect("anchor split").trim();
    if file.is_empty() {
        return None;
    }
    let mut resolved = PathBuf::new();
    for component in page
        .parent()
        .expect("skill page directory")
        .join(file)
        .components()
    {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other.as_os_str()),
        }
    }
    Some(resolved)
}
