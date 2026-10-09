#![allow(missing_docs, clippy::expect_used)]

//! Create-only seeding of workspace-local defaults, as host-level `orbit init`
//! runs it: an absent routine or auto-task is created with the `enabled` value
//! it ships with, and nothing that already exists is rewritten or retired.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_common::security::release::sha256_hex;
use orbit_core::bootstrap::init::{InitOptions, init_workspace_at_root};
use orbit_core::{
    ManagedArtifactOutcome, ManagedArtifactScope, RoutineSeedIdentity,
    reconcile_workspace_managed_artifacts, seed_absent_workspace_managed_artifacts,
};

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
