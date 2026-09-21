use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_core::OrbitRuntime;
use orbit_registry::workspace_registry::{registry_path_for, save_registry_to};
use orbit_store::maintenance::task_registry::{
    WorkspaceConfig, workspace_config_path, write_workspace_config,
};
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};
use serde_json::json;
use tempfile::TempDir;

use crate::registry_runtime::RegisteredRuntimeFactory;
use crate::task_owner::{bound_workspace_identity, initialize_for_task_show, resolve_task_owner};

/// Two registered workspaces holding one task, with the checkout identity the
/// task registry partitions by deliberately different from the logical registry
/// ID (L-0098) — the join `resolve_task_owner` has to get right.
struct OwnerFixture {
    _root: TempDir,
    global: PathBuf,
    alpha: OrbitRuntime,
    beta_repo: PathBuf,
    beta_orbit_dir: PathBuf,
    beta_task_id: String,
}

fn owner_fixture() -> OwnerFixture {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::write(
        global.join("config.toml"),
        "[machine]\nid = \"hm_owner_test\"\nname = \"owner-test\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("machine identity");

    let (ws_alpha, checkout_alpha) =
        registered_workspace(root.path(), "ws_alpha", "alpha", "alpha-a1b2c3");
    let (ws_beta, checkout_beta) =
        registered_workspace(root.path(), "ws_beta", "beta", "beta-d4e5f6");
    save_registry_to(
        &WorkspaceRegistry {
            workspaces: vec![ws_alpha.clone(), ws_beta.clone()],
            checkouts: vec![checkout_alpha.clone(), checkout_beta.clone()],
            ..Default::default()
        },
        &registry_path_for(&global),
    )
    .expect("workspace registry");

    let alpha =
        RegisteredRuntimeFactory::open_registered_checkout(&global, &ws_alpha, &checkout_alpha)
            .expect("alpha runtime");
    let beta =
        RegisteredRuntimeFactory::open_registered_checkout(&global, &ws_beta, &checkout_beta)
            .expect("beta runtime");
    let created = beta
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Beta-only task",
                "description": "Filed in beta; addressable from anywhere by ID.",
                "complexity": "low",
                "workspace": checkout_beta.repo_root
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("seed beta task");

    OwnerFixture {
        _root: root,
        global,
        alpha,
        beta_repo: checkout_beta.repo_root,
        beta_orbit_dir: checkout_beta.orbit_dir,
        beta_task_id: created["id"].as_str().expect("created task id").to_string(),
    }
}

fn registered_workspace(
    root: &Path,
    logical_id: &str,
    name: &str,
    checkout_identity: &str,
) -> (Workspace, WorkspaceCheckout) {
    let repo = root.join(name);
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("orbit dir");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: checkout_identity.to_string(),
        },
    )
    .expect("workspace config");
    let workspace = Workspace {
        id: logical_id.to_string(),
        name: name.to_string(),
        owner_machine_id: Some("hm_owner_test".to_string()),
        git_remote: None,
        ship_mode: Some("local".to_string()),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let checkout = WorkspaceCheckout::owner(logical_id.to_string(), repo, orbit_dir);
    (workspace, checkout)
}

#[test]
fn global_lookup_resolves_the_owning_workspace_by_task_id() {
    let fixture = owner_fixture();
    let selection =
        resolve_task_owner(&fixture.global, &fixture.beta_task_id).expect("registry owns the id");
    assert_eq!(selection.workspace.id, "ws_beta");
    assert_eq!(selection.workspace.name, "beta");
    assert_eq!(selection.checkout.repo_root, fixture.beta_repo);
}

#[test]
fn implicit_task_show_bootstrap_binds_the_owner_and_explicit_workspace_still_filters() {
    let fixture = owner_fixture();

    let owner = initialize_for_task_show(Some(&fixture.global), None, &fixture.beta_task_id)
        .expect("implicit bootstrap follows the id");
    assert_eq!(owner.paths().repo_root, fixture.beta_repo);
    assert_eq!(
        owner
            .get_task(&fixture.beta_task_id)
            .expect("owner runtime reads the task")
            .title,
        "Beta-only task"
    );

    // `--workspace alpha` is a filter, not a hint: the bootstrap binds alpha and
    // beta's task is simply not there.
    let filtered =
        initialize_for_task_show(Some(&fixture.global), Some("alpha"), &fixture.beta_task_id)
            .expect("explicit selector binds the named workspace");
    assert_eq!(filtered.paths().repo_root, fixture.alpha.paths().repo_root);
    assert!(matches!(
        filtered.get_task(&fixture.beta_task_id),
        Err(OrbitError::NotFound {
            kind: NotFoundKind::Task,
            ..
        })
    ));
}

#[test]
fn an_unregistered_task_id_is_a_plain_not_found() {
    let fixture = owner_fixture();
    assert!(matches!(
        resolve_task_owner(&fixture.global, "ORB-99999"),
        Err(OrbitError::NotFound {
            kind: NotFoundKind::Task,
            ..
        })
    ));
}

#[test]
fn a_stale_owning_checkout_names_the_workspace_instead_of_the_task_id() {
    let fixture = owner_fixture();
    // The registry still knows the ID; the checkout it points at no longer
    // identifies itself.
    std::fs::remove_file(workspace_config_path(&fixture.beta_orbit_dir))
        .expect("drop the owning checkout identity");

    let error = resolve_task_owner(&fixture.global, &fixture.beta_task_id)
        .expect_err("a stale owner must fail rather than resolve");
    let message = error.to_string();
    assert!(
        message.contains("beta") && message.contains(&fixture.beta_task_id),
        "stale owner must name the workspace and the task: {message}"
    );
    assert!(
        !matches!(error, OrbitError::NotFound { .. }),
        "a stale checkout must not be reported as an unknown task id: {message}"
    );
}

#[test]
fn bound_workspace_identity_reports_the_registry_name_and_logical_id() {
    let fixture = owner_fixture();
    let identity = bound_workspace_identity(&fixture.alpha).expect("alpha is registered");
    assert_eq!(identity.id, "ws_alpha");
    assert_eq!(identity.name, "alpha");
    assert_eq!(identity.label(), "alpha (ws_alpha)");
}
