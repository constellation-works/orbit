//! Auto-task delete and restore: a deleted definition takes its cursor with
//! it, a deleted shipped default stays out of every later reseed and doctor
//! pass, and restore brings back the shipped content.

use std::path::{Path, PathBuf};

use orbit_store::compose::auto_task::{load_cursor_state, upsert_cursor};
use orbit_types::workflow::AutoTaskCursor;
use tempfile::tempdir;

use crate::OrbitRuntime;
use crate::application::auto_tasks::delete::AutoTaskDeleteParams;
use crate::application::auto_tasks::{
    DEFAULT_AUTO_TASK_FILES, auto_tasks_dir, cursor_state_path, definition_path,
    render_default_auto_task,
};
use crate::application::health::artifact::ArtifactKind;
use crate::application::managed_assets::{
    MANAGED_ASSET_MANIFEST_FILE, ManagedAssetLayout, load_managed_asset_manifest,
};
use crate::bootstrap::init::{InitOptions, init_workspace_at_root};

use super::interval_params;

fn delete(name: &str) -> AutoTaskDeleteParams {
    AutoTaskDeleteParams {
        name: name.to_string(),
        reason: Some("not used in this workspace".to_string()),
        force: false,
    }
}

fn seed_cursor(runtime: &OrbitRuntime, name: &str) {
    upsert_cursor(
        &cursor_state_path(&runtime.paths().state_dir),
        name,
        serde_json::from_value::<AutoTaskCursor>(serde_json::json!({
            "baseline_at": chrono::Utc::now().to_rfc3339(),
        }))
        .expect("cursor fixture"),
    )
    .expect("seed scheduler cursor");
}

fn cursor_names(runtime: &OrbitRuntime) -> Vec<String> {
    load_cursor_state(&cursor_state_path(&runtime.paths().state_dir))
        .expect("load cursor state")
        .definitions
        .into_keys()
        .collect()
}

/// A workspace initialized with the shipped defaults, as `orbit workspace
/// init` leaves it.
fn seeded_runtime(root: &Path) -> (OrbitRuntime, PathBuf, PathBuf) {
    let global_root = root.join("global");
    let workspace_root = root.join("repo/.orbit");
    init_workspace_at_root(
        &global_root,
        InitOptions {
            global_only: true,
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .expect("initialize global root");
    reseed(&global_root, &workspace_root);
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
    assert_eq!(runtime.paths().local_dir, workspace_root);
    (runtime, global_root, workspace_root)
}

/// The reseed `orbit workspace init --force` performs.
fn reseed(global_root: &Path, workspace_root: &Path) {
    init_workspace_at_root(
        workspace_root,
        InitOptions {
            global_root_override: Some(global_root.to_path_buf()),
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .expect("reseed workspace");
}

fn opted_out(workspace_root: &Path) -> Vec<String> {
    load_managed_asset_manifest(
        &auto_tasks_dir(workspace_root).join(MANAGED_ASSET_MANIFEST_FILE),
        "auto_task",
        ManagedAssetLayout::YamlStem,
    )
    .expect("load auto-task manifest")
    .map(|manifest| manifest.opted_out.into_iter().collect())
    .unwrap_or_default()
}

fn auto_task_findings(runtime: &OrbitRuntime) -> Vec<String> {
    runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts")
        .into_iter()
        .find(|health| health.kind == ArtifactKind::AutoTask)
        .expect("auto-task health")
        .findings
        .into_iter()
        .map(|finding| format!("{} {:?}", finding.name, finding.condition))
        .collect()
}

#[test]
fn deleting_a_user_authored_definition_removes_file_and_cursor_without_an_opt_out() {
    let root = tempdir().expect("tempdir");
    let (runtime, _, workspace_root) = seeded_runtime(root.path());
    runtime
        .auto_task_add(interval_params("weekly-digest", 10_080))
        .expect("add");
    seed_cursor(&runtime, "weekly-digest");
    seed_cursor(&runtime, "backlog-hygiene");

    let report = runtime
        .auto_task_delete(delete("weekly-digest"))
        .expect("delete");

    assert!(!report.opted_out, "a user-authored name records no opt-out");
    assert!(report.cursor_removed);
    assert!(report.consumer.is_none());
    assert_eq!(report.deleted_by, runtime.actor_label());
    assert!(!definition_path(&workspace_root, "weekly-digest").exists());
    assert!(runtime.auto_task_show("weekly-digest").unwrap().is_none());
    assert_eq!(
        cursor_names(&runtime),
        vec!["backlog-hygiene".to_string()],
        "only the deleted definition's cursor goes"
    );
    assert!(opted_out(&workspace_root).is_empty());

    let audit = runtime
        .list_audit_events_with_kind(
            None,
            Some("orbit.auto_task.delete".to_string()),
            Some("auto_task".to_string()),
            None,
            None,
            10,
        )
        .expect("list audit events");
    assert_eq!(audit.len(), 1, "delete writes one audit record");
    let arguments = audit[0].arguments_json.as_deref().expect("audit payload");
    let payload: serde_json::Value = serde_json::from_str(arguments).expect("payload is JSON");
    assert_eq!(payload["name"], "weekly-digest");
    assert_eq!(payload["reason"], "not used in this workspace");
    assert_eq!(payload["deleted_by"], runtime.actor_label());
}

#[test]
fn a_deleted_shipped_default_survives_reseed_and_doctor() {
    let root = tempdir().expect("tempdir");
    let (runtime, global_root, workspace_root) = seeded_runtime(root.path());
    let path = definition_path(&workspace_root, "code-review");
    assert!(path.is_file(), "init seeds the shipped code-review default");

    let report = runtime
        .auto_task_delete(delete("code-review"))
        .expect("delete shipped default");
    assert!(report.opted_out);
    assert_eq!(opted_out(&workspace_root), vec!["code-review".to_string()]);

    reseed(&global_root, &workspace_root);

    assert!(
        !path.exists(),
        "a reseed must not re-create an opted-out default"
    );
    assert!(
        definition_path(&workspace_root, "backlog-hygiene").is_file(),
        "other shipped defaults are still managed"
    );
    assert_eq!(opted_out(&workspace_root), vec!["code-review".to_string()]);
    let findings = auto_task_findings(&runtime);
    assert!(
        findings
            .iter()
            .all(|finding| !finding.starts_with("code-review ")),
        "doctor must not report an opted-out default: {findings:?}"
    );
}

#[test]
fn restore_reinstates_the_shipped_content_and_clears_the_opt_out() {
    let root = tempdir().expect("tempdir");
    let (runtime, global_root, workspace_root) = seeded_runtime(root.path());
    runtime
        .auto_task_delete(delete("security-review"))
        .expect("delete shipped default");

    let restored = runtime
        .auto_task_restore("security-review")
        .expect("restore");

    assert!(!restored.enabled, "a restored default is inert, as shipped");
    let (_, embedded) = DEFAULT_AUTO_TASK_FILES
        .iter()
        .find(|(name, _)| *name == "security-review")
        .expect("shipped security-review");
    let path = definition_path(&workspace_root, "security-review");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read restored definition"),
        render_default_auto_task(embedded, runtime.workspace_base_branch())
    );
    assert!(opted_out(&workspace_root).is_empty());

    // Managed again: a reseed leaves it in place and doctor sees no drift.
    reseed(&global_root, &workspace_root);
    assert!(path.is_file());
    let findings = auto_task_findings(&runtime);
    assert!(
        findings
            .iter()
            .all(|finding| !finding.starts_with("security-review ")),
        "{findings:?}"
    );

    let error = runtime
        .auto_task_restore("security-review")
        .expect_err("an existing definition is not overwritten");
    assert!(error.to_string().contains("already exists"), "{error}");
}

#[test]
fn restore_refuses_a_name_orbit_does_not_ship() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");
    let error = runtime
        .auto_task_restore("weekly-digest")
        .expect_err("only shipped defaults can be restored");
    assert!(
        error.to_string().contains("not a shipped default"),
        "{error}"
    );
}

#[test]
fn delete_refuses_while_a_minted_task_is_open_unless_forced() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");
    runtime
        .auto_task_add(interval_params("nightly-chore", 1440))
        .expect("add");
    let minted = runtime.auto_task_mint("nightly-chore").expect("mint");

    let error = runtime
        .auto_task_delete(delete("nightly-chore"))
        .expect_err("an open minted task refuses the delete");
    assert!(
        error.to_string().contains(&minted.id),
        "the refusal names the open task: {error}"
    );
    assert!(
        runtime.auto_task_show("nightly-chore").unwrap().is_some(),
        "a refused delete leaves the definition"
    );

    let report = runtime
        .auto_task_delete(AutoTaskDeleteParams {
            force: true,
            ..delete("nightly-chore")
        })
        .expect("forced delete");
    assert_eq!(report.open_tasks, vec![minted.id.clone()]);
    assert!(runtime.auto_task_show("nightly-chore").unwrap().is_none());
    assert!(
        runtime.get_task(&minted.id).is_ok(),
        "a forced delete leaves the open task itself alone"
    );
}

#[test]
fn delete_of_an_unknown_definition_errors() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");
    let error = runtime
        .auto_task_delete(delete("missing"))
        .expect_err("unknown name");
    assert!(error.to_string().contains("no such auto-task"), "{error}");
}
