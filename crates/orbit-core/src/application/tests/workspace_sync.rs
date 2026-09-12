use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

use crate::application::MANAGED_ASSET_MANIFEST_FILE;
use crate::application::routine::RoutineSeedIdentity;
use crate::application::workspace_sync::{
    ManagedArtifactOutcome, reconcile_workspace_managed_artifacts,
};
use crate::bootstrap::init::{InitOptions, init_workspace_at_root};

fn seed_identity(workspace_name: &str) -> RoutineSeedIdentity {
    RoutineSeedIdentity::new(workspace_name).expect("routine seed identity")
}

fn initialized_roots(base: &Path) -> (PathBuf, PathBuf) {
    let global = base.join("global");
    let workspace = base.join("repo/.orbit");
    init_workspace_at_root(
        &global,
        InitOptions {
            global_only: true,
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .expect("initialize global root");
    init_workspace_at_root(
        &workspace,
        InitOptions {
            global_root_override: Some(global.clone()),
            routine_seed_identity: Some(
                RoutineSeedIdentity::new("alpha").expect("routine seed identity"),
            ),
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .expect("initialize workspace root");
    (global, workspace)
}

fn routine_manifest(workspace: &Path) -> PathBuf {
    workspace.join("routines").join(MANAGED_ASSET_MANIFEST_FILE)
}

fn sha256(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

/// [ORB-12236] A workspace seeded before host pins were retired carries a
/// `hosts:` line and a manifest binding that names a host. Sync adopts the new
/// host-free bytes as an ordinary managed refresh — not a collision, and not a
/// preserved local modification.
#[test]
fn a_workspace_seeded_with_host_pins_refreshes_onto_the_host_free_bytes() {
    let root = tempdir().expect("create tempdir");
    let (global, workspace) = initialized_roots(root.path());
    let routine = workspace.join("routines/task_triage.yaml");

    // Reconstruct what the previous release wrote: the same document plus the
    // host pin it rendered, recorded in the manifest under a hosts binding.
    let current = std::fs::read_to_string(&routine).expect("read seeded routine");
    let pinned = current.replace("trigger:", "hosts:\n  - host-a\ntrigger:");
    assert_ne!(pinned, current, "fixture must add the retired pin");
    std::fs::write(&routine, &pinned).expect("write the previously seeded routine");

    let manifest_path = routine_manifest(&workspace);
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    manifest["assets"]["task_triage"] = Value::from(sha256(&pinned));
    let provenance = &mut manifest["routineProvenance"]["task_triage"];
    provenance["templateDigest"] = Value::from("digest-of-the-host-pinned-template");
    provenance["renderedDigest"] = Value::from(sha256(&pinned));
    provenance["binding"]["hosts"] = Value::from(vec!["host-a"]);
    std::fs::write(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("serialize pre-upgrade manifest")
        ),
    )
    .expect("write pre-upgrade manifest");

    let applied = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&seed_identity("alpha")),
        false,
    )
    .expect("sync the upgraded workspace");

    assert!(applied.actions.iter().any(|action| {
        action.name == "task_triage" && action.outcome == ManagedArtifactOutcome::Refreshed
    }));
    assert!(
        !applied
            .actions
            .iter()
            .any(|action| action.name == "task_triage"
                && action.outcome == ManagedArtifactOutcome::Preserved),
    );
    let refreshed = std::fs::read_to_string(&routine).expect("read refreshed routine");
    assert!(!refreshed.contains("hosts:"), "{refreshed}");
    let definition = orbit_common::protocol::yaml::parse_routine_yaml(&refreshed)
        .expect("refreshed routine parses");
    assert_eq!(definition.name, "task-triage-alpha");
    assert!(!definition.has_legacy_host_pin());

    // Converged: a second sync is a no-op.
    let second = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&seed_identity("alpha")),
        false,
    )
    .expect("second sync");
    assert!(!second.has_pending_changes());
}

/// A workspace seeded under an earlier binding — including one whose routine
/// names came from the checkout directory before [ORB-12107] — keeps the
/// recorded name and bytes. Convergence reports the drift and renames nothing,
/// because a routine's run history and state key off its name.
#[test]
fn binding_drift_does_not_claim_template_drift_or_rewrite_routines() {
    let root = tempdir().expect("create tempdir");
    let (global, workspace) = initialized_roots(root.path());
    let routine = workspace.join("routines/task_triage.yaml");
    let before_routine = std::fs::read(&routine).expect("read routine");
    let before_manifest = std::fs::read(routine_manifest(&workspace)).expect("read manifest");

    let report = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&seed_identity("different-workspace")),
        false,
    )
    .expect("sync with drifted current binding");

    assert!(report.actions.iter().any(|action| {
        action.kind == "routine" && action.outcome == ManagedArtifactOutcome::BindingDrift
    }));
    assert!(!report.has_pending_changes());
    assert_eq!(
        std::fs::read(&routine).expect("reread routine"),
        before_routine
    );
    assert_eq!(
        std::fs::read(routine_manifest(&workspace)).expect("reread manifest"),
        before_manifest
    );
}

#[test]
fn legacy_routine_manifest_check_is_read_only_and_apply_migrates_only_exact_instances() {
    let root = tempdir().expect("create tempdir");
    let (global, workspace) = initialized_roots(root.path());
    let manifest_path = routine_manifest(&workspace);
    let raw = std::fs::read_to_string(&manifest_path).expect("read manifest");
    let mut manifest: Value = serde_json::from_str(&raw).expect("parse manifest");
    manifest
        .as_object_mut()
        .expect("manifest object")
        .remove("routineProvenance");
    manifest["schemaVersion"] = Value::from(1);
    let legacy = format!(
        "{}\n",
        serde_json::to_string_pretty(&manifest).expect("serialize legacy manifest")
    );
    std::fs::write(&manifest_path, &legacy).expect("write legacy manifest");
    let modified = workspace.join("routines/task_triage.yaml");
    let edited = format!(
        "{}# operator edit\n",
        std::fs::read_to_string(&modified).expect("read routine")
    );
    std::fs::write(&modified, &edited).expect("edit one legacy routine");

    let checked = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&seed_identity("new-suffix")),
        true,
    )
    .expect("check legacy migration");
    assert!(checked.has_pending_changes());
    assert!(checked.actions.iter().any(|action| {
        action.path == modified && action.outcome == ManagedArtifactOutcome::Preserved
    }));
    assert_eq!(
        std::fs::read_to_string(&manifest_path).expect("manifest after check"),
        legacy,
        "--check must not migrate the manifest"
    );
    assert_eq!(
        std::fs::read_to_string(&modified).expect("routine after check"),
        edited
    );

    let applied = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&seed_identity("new-suffix")),
        false,
    )
    .expect("apply legacy migration");
    assert!(applied.actions.iter().any(|action| {
        action.kind == "routine" && action.outcome == ManagedArtifactOutcome::Migrated
    }));
    let migrated: Value = serde_json::from_str(
        &std::fs::read_to_string(&manifest_path).expect("read migrated manifest"),
    )
    .expect("parse migrated manifest");
    let provenance = migrated["routineProvenance"]
        .as_object()
        .expect("routine provenance");
    assert!(!provenance.contains_key("task_triage"));
    let entry = &provenance["worktree_gc"];
    assert!(entry["templateDigest"].as_str().is_some());
    assert!(entry["renderedDigest"].as_str().is_some());
    assert_eq!(entry["binding"]["name"], "worktree-gc-alpha");
    assert_eq!(
        std::fs::read_to_string(&modified).expect("modified routine survives"),
        edited
    );
}

#[test]
fn real_template_refresh_uses_recorded_binding_and_second_run_is_a_no_op() {
    let root = tempdir().expect("create tempdir");
    let (global, workspace) = initialized_roots(root.path());
    let manifest_path = routine_manifest(&workspace);
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    manifest["routineProvenance"]["task_triage"]["templateDigest"] =
        Value::String("digest-from-previous-shipped-template".to_string());
    std::fs::write(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("serialize stale provenance")
        ),
    )
    .expect("write stale template identity");

    let applied = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&seed_identity("different-current-suffix")),
        false,
    )
    .expect("refresh true template drift");
    assert!(applied.actions.iter().any(|action| {
        action.name == "task_triage" && action.outcome == ManagedArtifactOutcome::Refreshed
    }));
    let routine = orbit_common::protocol::yaml::parse_routine_yaml(
        &std::fs::read_to_string(workspace.join("routines/task_triage.yaml"))
            .expect("read refreshed routine"),
    )
    .expect("parse refreshed routine");
    assert_eq!(routine.name, "task-triage-alpha");

    let before = std::fs::read(&manifest_path).expect("snapshot converged manifest");
    let second = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&seed_identity("different-current-suffix")),
        false,
    )
    .expect("second sync");
    assert!(!second.has_pending_changes());
    assert_eq!(
        std::fs::read(&manifest_path).expect("manifest no-op"),
        before
    );
}

/// A workspace seeded before routines recorded provenance has no routine
/// manifest, and its routines are customized by design. Sync must adopt them
/// instead of reporting a collision on every run, and the adoption must leave
/// the family reconcilable when a shipped template later changes [ORB-11154].
#[test]
fn manifestless_customized_routines_are_adopted_and_stay_reconcilable() {
    let root = tempdir().expect("create tempdir");
    let (global, workspace) = initialized_roots(root.path());
    let manifest_path = routine_manifest(&workspace);
    std::fs::remove_file(&manifest_path).expect("drop the pre-provenance routine manifest");
    let customized = workspace.join("routines/task_triage.yaml");
    let edited = std::fs::read_to_string(&customized)
        .expect("read seeded routine")
        .replace("enabled: false", "enabled: true");
    std::fs::write(&customized, &edited).expect("customize the seeded routine");

    let checked = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&seed_identity("alpha")),
        true,
    )
    .expect("check a pre-provenance workspace");
    assert!(
        !checked.actions.iter().any(|action| {
            action.kind == "routine" && action.outcome == ManagedArtifactOutcome::Preserved
        }),
        "customized routines must not be reported as collisions: {:?}",
        checked.actions
    );
    assert!(
        !manifest_path.exists(),
        "--check must not write the manifest"
    );

    let applied = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&seed_identity("alpha")),
        false,
    )
    .expect("adopt a pre-provenance workspace");
    assert!(!applied.actions.iter().any(|action| {
        action.kind == "routine" && action.outcome == ManagedArtifactOutcome::Preserved
    }));
    assert!(applied.actions.iter().any(|action| {
        action.name == "task_triage" && action.outcome == ManagedArtifactOutcome::Migrated
    }));
    assert_eq!(
        std::fs::read_to_string(&customized).expect("reread routine"),
        edited,
        "adoption must not rewrite the operator's routine"
    );
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).expect("read manifest"))
            .expect("parse adopted manifest");
    let entry = &manifest["routineProvenance"]["task_triage"];
    assert_eq!(entry["renderedDigest"], Value::from(sha256(&edited)));
    assert_eq!(entry["binding"]["name"], "task-triage-alpha");

    // Provenance now exists, so a later shipped-template change reconciles
    // rather than repeating a collision report forever.
    manifest["routineProvenance"]["task_triage"]["templateDigest"] =
        Value::String("digest-from-previous-shipped-template".to_string());
    std::fs::write(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("serialize stale provenance")
        ),
    )
    .expect("write stale template identity");
    let refreshed = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&seed_identity("alpha")),
        false,
    )
    .expect("refresh the adopted routine");
    assert!(refreshed.actions.iter().any(|action| {
        action.name == "task_triage" && action.outcome == ManagedArtifactOutcome::Refreshed
    }));
}

#[test]
fn sync_refreshes_only_provenance_clean_non_routine_assets_and_retires_safely() {
    let root = tempdir().expect("create tempdir");
    let (global, workspace) = initialized_roots(root.path());
    let activities = global.join("resources/activities");
    let current_path = activities.join("sleep.yaml");
    let current = std::fs::read_to_string(&current_path).expect("read current activity");
    let previous_shipped = format!("{current}# previous shipped template\n");
    std::fs::write(&current_path, &previous_shipped).expect("write previous shipped activity");

    let retired_path = activities.join("retired_sync_fixture.yaml");
    let retired = "previous Orbit-written retired activity\n";
    std::fs::write(&retired_path, retired).expect("write retired activity");
    let manifest_path = activities.join(MANAGED_ASSET_MANIFEST_FILE);
    let mut manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(&manifest_path).expect("read activity manifest"),
    )
    .expect("parse activity manifest");
    manifest["assets"]["sleep"] = Value::String(sha256(&previous_shipped));
    manifest["assets"]["retired_sync_fixture"] = Value::String(sha256(retired));
    std::fs::write(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("serialize activity manifest")
        ),
    )
    .expect("write previous activity provenance");

    let checked = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&seed_identity("alpha")),
        true,
    )
    .expect("check non-routine convergence");
    assert!(checked.actions.iter().any(|action| {
        action.path == current_path && action.outcome == ManagedArtifactOutcome::Refreshed
    }));
    assert!(checked.actions.iter().any(|action| {
        action.path == retired_path && action.outcome == ManagedArtifactOutcome::Retired
    }));
    assert_eq!(
        std::fs::read_to_string(&current_path).expect("activity after check"),
        previous_shipped
    );
    assert!(retired_path.exists());

    reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&seed_identity("alpha")),
        false,
    )
    .expect("apply non-routine convergence");
    assert_eq!(
        std::fs::read_to_string(&current_path).expect("refreshed activity"),
        current
    );
    assert!(!retired_path.exists());

    let before_manifest = std::fs::read(&manifest_path).expect("snapshot converged manifest");
    let second = reconcile_workspace_managed_artifacts(
        &global,
        &workspace,
        Some(&seed_identity("alpha")),
        false,
    )
    .expect("second convergence");
    assert!(!second.has_pending_changes());
    assert_eq!(
        std::fs::read(&manifest_path).expect("manifest after no-op"),
        before_manifest
    );
}
