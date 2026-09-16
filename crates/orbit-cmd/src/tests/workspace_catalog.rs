use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_core::{FederatedWorkspaceTarget, WorkspaceCatalog, WorkspaceScope};
use orbit_registry::workspace_registry::{registry_path_for, save_registry_to};
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};

use crate::workspace_catalog::RegistryWorkspaceCatalog;

fn workspace(id: &str, name: &str, status: WorkspaceStatus) -> Workspace {
    Workspace {
        id: id.to_string(),
        name: name.to_string(),
        owner_machine_id: Some("hm_owner".to_string()),
        git_remote: None,
        ship_mode: Some("pr".to_string()),
        base_branch: "agent-main".to_string(),
        status,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn checkout(workspace_id: &str, root: &Path, name: &str) -> WorkspaceCheckout {
    let repo = root.join(name);
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("orbit dir");
    WorkspaceCheckout::owner(workspace_id.to_string(), repo, orbit_dir)
}

/// A two-workspace registry plus one entry whose missing checkout is invalid.
fn seeded_catalog() -> (tempfile::TempDir, RegistryWorkspaceCatalog) {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    std::fs::create_dir_all(&global).expect("global root");

    let alpha = workspace("ws_alpha", "alpha", WorkspaceStatus::Active);
    let beta = workspace("ws_beta", "beta", WorkspaceStatus::Active);
    let invalid = workspace("ws_invalid", "invalid-ws", WorkspaceStatus::Invalid);
    let invalid_repo = root.path().join("invalid-ws");
    let invalid_orbit_dir = invalid_repo.join(".orbit");
    let checkouts = vec![
        checkout(&alpha.id, root.path(), "alpha"),
        checkout(&beta.id, root.path(), "beta"),
        WorkspaceCheckout::owner(invalid.id.clone(), invalid_repo, invalid_orbit_dir),
    ];
    save_registry_to(
        &WorkspaceRegistry {
            workspaces: vec![alpha, beta, invalid],
            checkouts,
            ..Default::default()
        },
        &registry_path_for(&global),
    )
    .expect("workspace registry");

    let catalog = RegistryWorkspaceCatalog::new(&global);
    (root, catalog)
}

#[test]
fn current_scope_never_asks_the_catalog_for_a_checkout() {
    let (_root, catalog) = seeded_catalog();

    let targets = catalog
        .resolve_scope(&WorkspaceScope::Current)
        .expect("resolve");

    assert!(
        targets.is_empty(),
        "Core resolves its own checkout; the catalog must not add one"
    );
}

#[test]
fn all_registered_scope_covers_active_workspaces_only() {
    let (_root, catalog) = seeded_catalog();

    let names = catalog
        .resolve_scope(&WorkspaceScope::AllRegistered)
        .expect("resolve")
        .into_iter()
        .map(|target| target.name)
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["alpha", "beta"]);
}

#[test]
fn repeated_selectors_for_one_workspace_are_opened_once() {
    let (_root, catalog) = seeded_catalog();

    // Name and logical ID name the same checkout; opening it twice would
    // double-count its hits in the fused list.
    let targets = catalog
        .resolve_scope(&WorkspaceScope::Selectors(vec![
            "alpha".to_string(),
            "ws_alpha".to_string(),
            "beta".to_string(),
        ]))
        .expect("resolve");

    assert_eq!(
        targets
            .iter()
            .map(|target| target.workspace_id.as_str())
            .collect::<Vec<_>>(),
        vec!["ws_alpha", "ws_beta"]
    );
}

#[test]
fn an_unknown_selector_fails_closed_by_name() {
    let (_root, catalog) = seeded_catalog();

    let error = catalog
        .resolve_scope(&WorkspaceScope::Selectors(vec!["nowhere".to_string()]))
        .expect_err("an unknown selector must not be silently dropped from the scope");

    assert!(error.to_string().contains("nowhere"));
}

/// The registry is read by `resolve_scope`, and by nothing the fan-out does
/// afterwards: deleting `workspaces.json` between the two must not change what
/// `open` resolves [DANI-10365].
#[test]
fn a_resolved_scope_opens_without_reading_the_registry_again() {
    let (root, catalog) = seeded_catalog();
    let registry_path = registry_path_for(&root.path().join("global"));

    let targets = catalog
        .resolve_scope(&WorkspaceScope::AllRegistered)
        .expect("resolve");
    std::fs::remove_file(&registry_path).expect("remove registry");

    for target in &targets {
        let resolved = catalog
            .resolve_target(target)
            .expect("a resolved target opens from the scope snapshot");
        assert_eq!(resolved.repo_root(), target.repo_root);
    }
}

/// The snapshot is a per-query view, not a cache: a target it never covered
/// still falls back to the registry and still fails closed by name.
#[test]
fn a_target_outside_the_snapshot_falls_back_to_the_registry() {
    let (_root, catalog) = seeded_catalog();

    let targets = catalog
        .resolve_scope(&WorkspaceScope::Selectors(vec!["alpha".to_string()]))
        .expect("resolve");
    assert_eq!(targets.len(), 1);

    let unresolved = FederatedWorkspaceTarget {
        workspace_id: "ws_beta".to_string(),
        name: "beta".to_string(),
        repo_root: PathBuf::from("/nowhere"),
    };
    assert!(
        catalog.resolve_target(&unresolved).is_ok(),
        "a registered workspace outside the snapshot still resolves"
    );

    let gone = FederatedWorkspaceTarget {
        workspace_id: "ws_gone".to_string(),
        name: "gone".to_string(),
        repo_root: PathBuf::from("/nowhere"),
    };
    let error = catalog
        .resolve_target(&gone)
        .expect_err("an unregistered workspace fails closed");
    assert!(error.to_string().contains("gone"));
    assert!(error.to_string().contains("no longer registered"));
}

#[test]
fn a_non_active_workspace_is_not_selectable_by_name() {
    let (_root, catalog) = seeded_catalog();

    assert!(
        catalog
            .resolve_scope(&WorkspaceScope::Selectors(vec!["invalid-ws".to_string()]))
            .is_err()
    );
}
