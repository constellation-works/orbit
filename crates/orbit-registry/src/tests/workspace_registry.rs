use std::fs;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_types::workspace::{
    WORKSPACE_REGISTRY_SCHEMA_VERSION, Workspace, WorkspaceCheckout, WorkspaceCheckoutRole,
    WorkspaceRegistry, WorkspaceStatus,
};
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::workspace_registry::{
    assign_checkout_role, find_checkout_by_path, find_workspace, find_workspace_by_id,
    find_workspace_by_path, load_registry_from, load_registry_from_with_writer, register_checkout,
    remove_workspace, rename_local_owner_host_id, resolve_logical_workspace, save_registry_to,
    set_path_override, validate_workspaces,
};

fn timestamp() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 18, 1, 2, 3)
        .single()
        .expect("fixed timestamp")
}

fn logical_workspace(id: &str, owner_machine_id: Option<&str>) -> Workspace {
    Workspace {
        id: id.to_string(),
        name: id.trim_start_matches("ws_").to_string(),
        owner_machine_id: owner_machine_id.map(str::to_string),
        git_remote: Some("git@example.test:orbit/repo.git".to_string()),
        ship_mode: Some("pr".to_string()),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: timestamp(),
        updated_at: timestamp(),
    }
}

fn write_host_identity(root: &Path, _legacy_mode: &str, machine_id: &str) {
    fs::write(
        root.join("host.toml"),
        format!(
            "schema_version = 2\nmachine_id = \"{machine_id}\"\nhost_id = \"test-host\"\ntask_prefix = \"ORB\"\n"
        ),
    )
    .expect("write host identity");
}

fn write_current_host_identity(root: &Path, machine_id: &str) {
    fs::write(
        root.join("host.toml"),
        format!(
            "schema_version = 2\nmachine_id = \"{machine_id}\"\nhost_id = \"test-host\"\ntask_prefix = \"ORB\"\n"
        ),
    )
    .expect("write current host identity");
}

fn write_json(path: &Path, value: &Value) -> Vec<u8> {
    let bytes = serde_json::to_vec_pretty(value).expect("serialize fixture");
    fs::write(path, &bytes).expect("write fixture");
    bytes
}

#[test]
fn legacy_registry_migrates_to_path_free_catalog_and_is_byte_stable() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("workspaces.json");
    let repo_root = root.path().join("repo");
    let orbit_dir = repo_root.join(".orbit");
    let override_path = root.path().join("linked-worktree");
    write_json(
        &path,
        &json!({
            "workspaces": [{
                "id": "ws_orbit",
                "name": "orbit",
                "root": repo_root,
                "orbit_dir": orbit_dir,
                "git_remote": "git@example.test:orbit/orbit.git",
                "ship_mode": "pr",
                "base_branch": "agent-main",
                "status": "active",
                "created_at": "2026-07-18T01:02:03Z",
                "updated_at": "2026-07-18T01:02:03Z"
            }],
            "path_overrides": {
                (override_path.to_string_lossy().to_string()): "ws_orbit",
                (root.path().join("dangling").to_string_lossy().to_string()): "ws_missing"
            }
        }),
    );

    let migrated = load_registry_from(&path).expect("migrate registry");
    assert_eq!(migrated.schema_version, WORKSPACE_REGISTRY_SCHEMA_VERSION);
    assert_eq!(migrated.workspaces.len(), 1);
    assert_eq!(migrated.checkouts.len(), 1);
    let workspace = &migrated.workspaces[0];
    assert_eq!(workspace.id, "ws_orbit");
    assert_eq!(workspace.name, "orbit");
    assert_eq!(
        workspace.git_remote.as_deref(),
        Some("git@example.test:orbit/orbit.git")
    );
    assert_eq!(workspace.ship_mode.as_deref(), Some("pr"));
    assert_eq!(workspace.base_branch, "agent-main");
    assert_eq!(workspace.created_at, timestamp());
    assert_eq!(workspace.updated_at, timestamp());
    let checkout = &migrated.checkouts[0];
    assert_eq!(checkout.workspace_id, "ws_orbit");
    assert_eq!(checkout.role, Some(WorkspaceCheckoutRole::Owner));
    assert_eq!(checkout.path_overrides, vec![override_path]);

    let persisted: Value =
        serde_json::from_slice(&fs::read(&path).expect("read migrated registry"))
            .expect("parse migrated registry");
    assert!(persisted["workspaces"][0].get("root").is_none());
    assert!(persisted["workspaces"][0].get("orbit_dir").is_none());
    assert!(persisted.get("path_overrides").is_none());
    assert_eq!(persisted["checkouts"][0]["role"], "owner");

    let first_bytes = fs::read(&path).expect("read first migration");
    let second = load_registry_from(&path).expect("load migrated registry again");
    assert_eq!(second, migrated);
    assert_eq!(fs::read(&path).expect("read second migration"), first_bytes);
}

#[test]
fn standalone_missing_role_canonicalizes_to_local_owner() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("workspaces.json");
    write_json(
        &path,
        &json!({
            "schema_version": 1,
            "workspaces": [logical_workspace("ws_orbit", None)],
            "checkouts": [{
                "workspace_id": "ws_orbit",
                "repo_root": "/repos/orbit",
                "orbit_dir": "/repos/orbit/.orbit"
            }]
        }),
    );

    let registry = load_registry_from(&path).expect("load standalone registry");
    assert_eq!(
        registry.checkouts[0].role,
        Some(WorkspaceCheckoutRole::Owner)
    );
    let persisted: Value =
        serde_json::from_slice(&fs::read(&path).expect("read registry")).expect("parse registry");
    assert_eq!(persisted["checkouts"][0]["role"], "owner");
}

#[test]
fn multi_host_legacy_registry_rejects_missing_role_without_rewriting() {
    for mode in ["hub", "spoke"] {
        let root = tempdir().expect("tempdir");
        write_host_identity(root.path(), mode, "hm_local");
        let path = root.path().join("workspaces.json");
        let original = write_json(
            &path,
            &json!({
                "workspaces": [{
                    "id": "ws_legacy",
                    "name": "legacy",
                    "root": "/repos/legacy",
                    "orbit_dir": "/repos/legacy/.orbit",
                    "git_remote": null,
                    "base_branch": "main",
                    "status": "active",
                    "created_at": "2026-07-18T01:02:03Z",
                    "updated_at": "2026-07-18T01:02:03Z"
                }],
                "path_overrides": {}
            }),
        );

        let error = load_registry_from(&path).expect_err("legacy role must not be inferred");
        let message = error.to_string();
        assert!(message.contains("ws_legacy"), "{message}");
        assert!(
            message.contains("missing a local checkout role"),
            "{message}"
        );
        assert_eq!(fs::read(&path).expect("read unchanged registry"), original);
    }
}

#[test]
fn multi_host_owner_role_requires_declared_owner_without_rewriting() {
    for mode in ["hub", "spoke"] {
        let root = tempdir().expect("tempdir");
        write_host_identity(root.path(), mode, "hm_local");
        let path = root.path().join("workspaces.json");
        let original = write_json(
            &path,
            &json!({
                "schema_version": 1,
                "workspaces": [logical_workspace("ws_missing_owner", None)],
                "checkouts": [{
                    "workspace_id": "ws_missing_owner",
                    "repo_root": "/repos/missing-owner",
                    "orbit_dir": "/repos/missing-owner/.orbit",
                    "role": "owner"
                }]
            }),
        );

        let error = load_registry_from(&path)
            .expect_err("multi-host owner must already name its stable owner")
            .to_string();
        assert!(error.contains("ws_missing_owner"), "unexpected: {error}");
        assert!(
            error.contains("no declared owner_machine_id"),
            "unexpected: {error}"
        );
        assert_eq!(fs::read(&path).expect("read unchanged registry"), original);
    }
}

#[test]
fn multi_host_modes_reject_missing_unknown_and_contradictory_roles_by_workspace_id() {
    let cases = [
        (
            "hub",
            json!({
                "workspace_id": "ws_missing_role",
                "repo_root": "/repos/missing",
                "orbit_dir": "/repos/missing/.orbit"
            }),
            "missing a local checkout role",
        ),
        (
            "spoke",
            json!({
                "workspace_id": "ws_unknown_role",
                "repo_root": "/repos/unknown",
                "orbit_dir": "/repos/unknown/.orbit",
                "role": "secondary"
            }),
            "unknown checkout role 'secondary'",
        ),
        (
            "hub",
            json!({
                "workspace_id": "ws_contradictory",
                "repo_root": "/repos/contradictory",
                "orbit_dir": "/repos/contradictory/.orbit",
                "role": "owner"
            }),
            "logical owner is machine 'hm_remote'",
        ),
        (
            "spoke",
            json!({
                "workspace_id": "ws_replica_without_owner",
                "repo_root": "/repos/replica",
                "orbit_dir": "/repos/replica/.orbit",
                "role": "replica"
            }),
            "replica role without owner_machine_id",
        ),
    ];

    for (mode, checkout, expected) in cases {
        let root = tempdir().expect("tempdir");
        write_host_identity(root.path(), mode, "hm_local");
        let path = root.path().join("workspaces.json");
        let workspace_id = checkout["workspace_id"].as_str().expect("workspace id");
        let owner = match workspace_id {
            "ws_contradictory" | "ws_replica_without_owner" => Some("hm_remote"),
            _ => Some("hm_local"),
        };
        let original = write_json(
            &path,
            &json!({
                "schema_version": 1,
                "workspaces": [logical_workspace(workspace_id, owner)],
                "checkouts": [checkout]
            }),
        );

        let error = load_registry_from(&path).expect_err("invalid role must fail closed");
        let message = error.to_string();
        assert!(message.contains(workspace_id), "{message}");
        assert!(message.contains(expected), "{message}");
        assert_eq!(fs::read(&path).expect("read unchanged registry"), original);
    }
}

#[test]
fn valid_spoke_replica_requires_and_preserves_owner_machine() {
    let root = tempdir().expect("tempdir");
    write_host_identity(root.path(), "spoke", "hm_local");
    let path = root.path().join("workspaces.json");
    write_json(
        &path,
        &json!({
            "schema_version": 1,
            "workspaces": [logical_workspace("ws_orbit", Some("hm_owner"))],
            "checkouts": [{
                "workspace_id": "ws_orbit",
                "repo_root": "/repos/orbit",
                "orbit_dir": "/repos/orbit/.orbit",
                "role": "replica",
                "owner_machine_id": "hm_owner"
            }]
        }),
    );

    let registry = load_registry_from(&path).expect("valid replica registry");
    assert_eq!(
        registry.checkouts[0].role,
        Some(WorkspaceCheckoutRole::Replica)
    );
    assert_eq!(
        registry.checkouts[0].owner_machine_id.as_deref(),
        Some("hm_owner")
    );
}

#[test]
fn identity_lookup_is_path_independent_and_path_lookup_is_checkout_only() {
    let mut registry = WorkspaceRegistry {
        workspaces: vec![
            logical_workspace("ws_outer", None),
            logical_workspace("ws_inner", None),
            logical_workspace("ws_remote", Some("hm_remote")),
        ],
        checkouts: vec![
            WorkspaceCheckout::owner(
                "ws_outer".to_string(),
                PathBuf::from("/repos"),
                PathBuf::from("/repos/.orbit"),
            ),
            WorkspaceCheckout::owner(
                "ws_inner".to_string(),
                PathBuf::from("/different/inner"),
                PathBuf::from("/different/inner/.orbit"),
            ),
        ],
        ..Default::default()
    };
    registry.checkouts[1]
        .path_overrides
        .push(PathBuf::from("/repos/inner"));

    assert_eq!(
        find_workspace(&registry, "ws_remote")
            .expect("lookup by id")
            .map(|workspace| workspace.id.as_str()),
        Some("ws_remote")
    );
    assert_eq!(
        find_workspace(&registry, "remote")
            .expect("lookup by name")
            .map(|workspace| workspace.id.as_str()),
        Some("ws_remote")
    );
    assert_eq!(
        find_workspace_by_path(&registry, Path::new("/repos/inner/src"))
            .map(|workspace| workspace.id.as_str()),
        Some("ws_inner")
    );
    assert_eq!(
        find_checkout_by_path(&registry, Path::new("/repos/inner/src"))
            .map(|checkout| checkout.workspace_id.as_str()),
        Some("ws_inner")
    );
    assert!(find_workspace_by_path(&registry, Path::new("/remote/ws_remote")).is_none());
}

#[test]
fn checkout_registration_allows_distinct_repos_to_share_an_orbit_root() {
    let shared_root = PathBuf::from("/srv/orbit");
    let mut registry = WorkspaceRegistry {
        workspaces: vec![
            logical_workspace("ws_alpha", None),
            logical_workspace("ws_beta", None),
        ],
        checkouts: vec![WorkspaceCheckout::owner(
            "ws_alpha".to_string(),
            PathBuf::from("/repos/alpha"),
            shared_root.clone(),
        )],
        ..Default::default()
    };

    register_checkout(
        &mut registry,
        WorkspaceCheckout::owner(
            "ws_beta".to_string(),
            PathBuf::from("/repos/beta"),
            shared_root,
        ),
    )
    .expect("shared Orbit root is not checkout identity");

    assert_eq!(registry.checkouts.len(), 2);
}

#[test]
fn save_rejects_two_checkouts_sharing_a_repo_root() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("workspaces.json");
    let shared = PathBuf::from("/repos/shared");
    let registry = WorkspaceRegistry {
        workspaces: vec![
            logical_workspace("ws_alpha", None),
            logical_workspace("ws_beta", None),
        ],
        checkouts: vec![
            WorkspaceCheckout::owner(
                "ws_alpha".to_string(),
                shared.clone(),
                PathBuf::from("/repos/shared/.orbit-alpha"),
            ),
            WorkspaceCheckout::owner(
                "ws_beta".to_string(),
                shared,
                PathBuf::from("/repos/shared/.orbit-beta"),
            ),
        ],
        ..Default::default()
    };

    let error = save_registry_to(&registry, &path)
        .expect_err("duplicate repo_root must fail at the persistence boundary")
        .to_string();
    assert!(error.contains("invalid registry:"), "{error}");
    assert!(error.contains("ws_alpha"), "{error}");
    assert!(error.contains("ws_beta"), "{error}");
    assert!(!path.exists(), "rejected save must not write the registry");
}

#[test]
fn set_path_override_rejects_a_path_claimed_by_another_checkout() {
    let mut registry = WorkspaceRegistry {
        workspaces: vec![
            logical_workspace("ws_alpha", None),
            logical_workspace("ws_beta", None),
        ],
        checkouts: vec![
            WorkspaceCheckout::owner(
                "ws_alpha".to_string(),
                PathBuf::from("/repos/alpha"),
                PathBuf::from("/repos/alpha/.orbit"),
            ),
            {
                let mut beta = WorkspaceCheckout::owner(
                    "ws_beta".to_string(),
                    PathBuf::from("/repos/beta"),
                    PathBuf::from("/repos/beta/.orbit"),
                );
                beta.path_overrides = vec![PathBuf::from("/overrides/beta")];
                beta
            },
        ],
        ..Default::default()
    };
    let before = registry.clone();

    let claimed_root = set_path_override(&mut registry, PathBuf::from("/repos/alpha"), "ws_beta")
        .expect_err("override equal to another repo_root must fail")
        .to_string();
    assert!(
        claimed_root.contains("already registered to workspace 'ws_alpha'"),
        "{claimed_root}"
    );
    assert_eq!(registry, before);

    let claimed_override =
        set_path_override(&mut registry, PathBuf::from("/overrides/beta"), "ws_alpha")
            .expect_err("override equal to another override must fail")
            .to_string();
    assert!(
        claimed_override.contains("already registered to workspace 'ws_beta'"),
        "{claimed_override}"
    );
    assert_eq!(registry, before);
}

#[test]
fn resolve_logical_workspace_accepts_name_or_id_and_fails_closed() {
    let mut registry = WorkspaceRegistry {
        workspaces: vec![
            logical_workspace("ws_orbit", Some("hm_owner")),
            logical_workspace("ws_other", Some("hm_owner")),
        ],
        ..Default::default()
    };
    registry.workspaces[0].name = "orbit".to_string();
    registry.workspaces[1].name = "other".to_string();

    assert_eq!(
        resolve_logical_workspace(&registry, "orbit")
            .expect("name")
            .id,
        "ws_orbit"
    );
    assert_eq!(
        resolve_logical_workspace(&registry, "ws_orbit")
            .expect("id")
            .id,
        "ws_orbit"
    );

    let unknown = resolve_logical_workspace(&registry, "missing").expect_err("unknown");
    match unknown {
        OrbitError::InvalidInput(message) => {
            assert!(message.contains("missing"), "{message}");
            assert!(message.contains("unknown workspace selector"), "{message}");
        }
        other => panic!("expected InvalidInput, got {other}"),
    }

    registry.workspaces[1].name = "orbit".to_string();
    let ambiguous = resolve_logical_workspace(&registry, "orbit").expect_err("ambiguous");
    match ambiguous {
        OrbitError::InvalidInput(message) => {
            assert!(message.contains("orbit"), "{message}");
            assert!(
                message.contains("ambiguous workspace selector"),
                "{message}"
            );
        }
        other => panic!("expected InvalidInput, got {other}"),
    }
}

#[test]
fn mutation_helpers_reject_a_name_that_matches_another_workspace_id() {
    let mut registry = WorkspaceRegistry {
        workspaces: vec![
            logical_workspace("ws_1", None),
            logical_workspace("ws_2", None),
        ],
        checkouts: vec![WorkspaceCheckout::owner(
            "ws_1".to_string(),
            PathBuf::from("/repos/one"),
            PathBuf::from("/repos/one/.orbit"),
        )],
        ..Default::default()
    };
    registry.workspaces[0].name = "x".to_string();
    registry.workspaces[1].name = "ws_1".to_string();
    let before = registry.clone();

    let resolved = resolve_logical_workspace(&registry, "ws_1")
        .expect_err("selector must be ambiguous")
        .to_string();
    assert!(
        resolved.contains("ambiguous workspace selector"),
        "{resolved}"
    );

    let removed = remove_workspace(&mut registry, "ws_1")
        .expect_err("removal must reject an ambiguous selector")
        .to_string();
    assert!(
        removed.contains("ambiguous workspace selector"),
        "{removed}"
    );
    assert_eq!(registry, before);

    let assigned = assign_checkout_role(
        &mut registry,
        "ws_1",
        WorkspaceCheckoutRole::Owner,
        None,
        None,
    )
    .expect_err("role assignment must reject an ambiguous selector")
    .to_string();
    assert!(
        assigned.contains("ambiguous workspace selector"),
        "{assigned}"
    );
    assert_eq!(registry, before);
}

#[test]
fn exact_id_lookup_survives_a_name_that_matches_another_workspace_id() {
    let mut registry = WorkspaceRegistry {
        workspaces: vec![
            logical_workspace("ws_alpha", None),
            logical_workspace("ws_ws_alpha", None),
        ],
        checkouts: vec![
            WorkspaceCheckout::owner(
                "ws_alpha".to_string(),
                PathBuf::from("/repos/alpha"),
                PathBuf::from("/repos/alpha/.orbit"),
            ),
            WorkspaceCheckout::owner(
                "ws_ws_alpha".to_string(),
                PathBuf::from("/repos/ws_alpha"),
                PathBuf::from("/repos/ws_alpha/.orbit"),
            ),
        ],
        ..Default::default()
    };
    registry.workspaces[0].name = "alpha".to_string();
    registry.workspaces[1].name = "ws_alpha".to_string();
    let before = registry.clone();

    assert_eq!(
        find_workspace_by_id(&registry, "ws_alpha").map(|workspace| workspace.name.as_str()),
        Some("alpha")
    );
    assert_eq!(
        find_workspace_by_id(&registry, "ws_ws_alpha").map(|workspace| workspace.name.as_str()),
        Some("ws_alpha")
    );
    assert_eq!(
        find_workspace_by_path(&registry, Path::new("/repos/alpha/src"))
            .map(|workspace| workspace.id.as_str()),
        Some("ws_alpha")
    );

    let ambiguous = find_workspace(&registry, "ws_alpha")
        .expect_err("id-or-name selector must stay fail-closed")
        .to_string();
    assert!(
        ambiguous.contains("ambiguous workspace selector"),
        "{ambiguous}"
    );

    let removed = remove_workspace(&mut registry, "ws_alpha")
        .expect_err("removal must reject an ambiguous selector")
        .to_string();
    assert!(
        removed.contains("ambiguous workspace selector"),
        "{removed}"
    );
    assert_eq!(registry, before);

    let assigned = assign_checkout_role(
        &mut registry,
        "ws_alpha",
        WorkspaceCheckoutRole::Owner,
        None,
        None,
    )
    .expect_err("role assignment must reject an ambiguous selector")
    .to_string();
    assert!(
        assigned.contains("ambiguous workspace selector"),
        "{assigned}"
    );
    assert_eq!(registry, before);
}

#[test]
fn malformed_and_future_registries_fail_without_rewriting() {
    for (bytes, expected) in [
        (b"{ not json".to_vec(), "malformed JSON"),
        (
            serde_json::to_vec_pretty(&json!({
                "schema_version": WORKSPACE_REGISTRY_SCHEMA_VERSION + 1,
                "workspaces": [],
                "checkouts": []
            }))
            .expect("serialize future fixture"),
            "unsupported schema_version",
        ),
    ] {
        let root = tempdir().expect("tempdir");
        let path = root.path().join("workspaces.json");
        fs::write(&path, &bytes).expect("write invalid fixture");
        let error = load_registry_from(&path).expect_err("invalid registry must fail");
        assert!(error.to_string().contains(expected), "{error}");
        assert_eq!(fs::read(&path).expect("read unchanged fixture"), bytes);
    }
}

#[test]
fn registry_io_rejects_a_non_registry_file_name() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("other.json");
    fs::write(&path, b"not a registry").expect("write fixture");

    let error = load_registry_from(&path).expect_err("alternate registry file must fail");

    assert!(
        error.to_string().contains("must name 'workspaces.json'"),
        "unexpected: {error}"
    );
}

#[cfg(unix)]
#[test]
fn registry_io_rejects_a_symlinked_registry_file() {
    let root = tempdir().expect("tempdir");
    let outside = tempdir().expect("tempdir");
    let target = outside.path().join("workspaces.json");
    fs::write(&target, b"not a registry").expect("write target");
    let path = root.path().join("workspaces.json");
    std::os::unix::fs::symlink(&target, &path).expect("create registry symlink");

    let error = load_registry_from(&path).expect_err("symlinked registry must fail");

    assert!(
        error.to_string().contains("must not be a symlink"),
        "unexpected: {error}"
    );
}

#[test]
fn persisted_replica_owner_ids_must_be_logical_and_remain_byte_stable_on_rejection() {
    for (logical_owner, checkout_owner) in [
        ("/tmp/hub", "/tmp/hub"),
        ("hm_owner", "ssh\\hub"),
        ("hm_owner", "hm_owner\ntransport"),
    ] {
        let root = tempdir().expect("tempdir");
        write_host_identity(root.path(), "spoke", "hm_local");
        let path = root.path().join("workspaces.json");
        let original = write_json(
            &path,
            &json!({
                "schema_version": 1,
                "workspaces": [logical_workspace("ws_orbit", Some(logical_owner))],
                "checkouts": [{
                    "workspace_id": "ws_orbit",
                    "repo_root": "/repos/orbit",
                    "orbit_dir": "/repos/orbit/.orbit",
                    "role": "replica",
                    "owner_machine_id": checkout_owner
                }]
            }),
        );

        let error = load_registry_from(&path)
            .expect_err("transport-shaped persisted owner must fail")
            .to_string();
        assert!(
            error.contains("owner_machine_id") || error.contains("machine_id"),
            "unexpected: {error}"
        );
        assert_eq!(fs::read(&path).expect("read unchanged registry"), original);
    }
}

#[test]
fn invalid_local_machine_id_cannot_be_copied_into_owner_role() {
    let root = tempdir().expect("tempdir");
    write_host_identity(root.path(), "hub", "/tmp/hub");
    let path = root.path().join("workspaces.json");
    let original = write_json(
        &path,
        &json!({
            "schema_version": 1,
            "workspaces": [logical_workspace("ws_orbit", None)],
            "checkouts": [{
                "workspace_id": "ws_orbit",
                "repo_root": "/repos/orbit",
                "orbit_dir": "/repos/orbit/.orbit",
                "role": "owner"
            }]
        }),
    );

    let error = load_registry_from(&path)
        .expect_err("invalid local machine_id must fail before owner canonicalization")
        .to_string();
    assert!(error.contains("machine_id"), "unexpected: {error}");
    assert_eq!(fs::read(&path).expect("read unchanged registry"), original);
}

#[test]
fn injected_migration_write_failure_preserves_readable_legacy_registry() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join("workspaces.json");
    let original = write_json(
        &path,
        &json!({
            "workspaces": [{
                "id": "ws_orbit",
                "name": "orbit",
                "root": "/repos/orbit",
                "orbit_dir": "/repos/orbit/.orbit",
                "git_remote": null,
                "base_branch": "main",
                "status": "active",
                "created_at": "2026-07-18T01:02:03Z",
                "updated_at": "2026-07-18T01:02:03Z"
            }],
            "path_overrides": {}
        }),
    );

    let error = load_registry_from_with_writer(&path, |_, _| {
        Err(OrbitError::Io("injected write failure".to_string()))
    })
    .expect_err("write failure must surface");
    assert!(error.to_string().contains("injected write failure"));
    assert_eq!(
        fs::read(&path).expect("read preserved legacy file"),
        original
    );

    let recovered = load_registry_from(&path).expect("legacy source remains migratable");
    assert_eq!(recovered.workspaces[0].id, "ws_orbit");
    assert_eq!(recovered.checkouts[0].workspace_id, "ws_orbit");
}

#[test]
fn assign_checkout_role_is_idempotent_and_rejects_owner_of_another_machine_byte_valid() {
    let root = tempdir().expect("tempdir");
    write_host_identity(root.path(), "spoke", "hm_local");
    let path = root.path().join("workspaces.json");
    write_json(
        &path,
        &json!({
            "schema_version": 1,
            "workspaces": [logical_workspace("ws_orbit", Some("hm_owner"))],
            "checkouts": [{
                "workspace_id": "ws_orbit",
                "repo_root": "/repos/orbit",
                "orbit_dir": "/repos/orbit/.orbit",
                "role": "replica",
                "owner_machine_id": "hm_owner"
            }]
        }),
    );

    // Re-declaring the same replica role is idempotent and persists cleanly.
    let mut registry = load_registry_from(&path).expect("load replica");
    assign_checkout_role(
        &mut registry,
        "ws_orbit",
        WorkspaceCheckoutRole::Replica,
        Some("hm_owner"),
        Some("hm_local"),
    )
    .expect("replica role");
    save_registry_to(&registry, &path).expect("save replica");

    // A replica declaration cannot silently replace the hub-declared owner in
    // either the logical workspace or its mirrored checkout binding.
    let before_rebind = fs::read(&path).expect("read before rebind");
    let mut rebind = load_registry_from(&path).expect("reload before rebind");
    let in_memory_before = rebind.clone();
    let error = assign_checkout_role(
        &mut rebind,
        "ws_orbit",
        WorkspaceCheckoutRole::Replica,
        Some("hm_other"),
        Some("hm_local"),
    )
    .expect_err("replica owner rebind must fail before mutation")
    .to_string();
    assert!(error.contains("already owned"), "unexpected: {error}");
    assert_eq!(rebind, in_memory_before);
    assert_eq!(fs::read(&path).expect("read after rebind"), before_rebind);

    // Declaring owner role on this non-owner machine fails before mutating the
    // in-memory registry and leaves the previous file byte-valid.
    let before = fs::read(&path).expect("read before");
    let mut contradictory = load_registry_from(&path).expect("reload");
    let in_memory_before = contradictory.clone();
    let error = assign_checkout_role(
        &mut contradictory,
        "ws_orbit",
        WorkspaceCheckoutRole::Owner,
        None,
        Some("hm_local"),
    )
    .expect_err("owner role on a non-owner machine must fail before mutation")
    .to_string();
    assert!(error.contains("owner"), "unexpected: {error}");
    assert_eq!(contradictory, in_memory_before);
    assert_eq!(
        fs::read(&path).expect("read after"),
        before,
        "rejected save must leave the registry file byte-identical"
    );
}

#[test]
fn explicit_owner_role_stamps_the_validated_local_machine_before_save() {
    let root = tempdir().expect("tempdir");
    write_current_host_identity(root.path(), "hm_local");
    let path = root.path().join("workspaces.json");
    let mut registry = WorkspaceRegistry {
        workspaces: vec![logical_workspace("ws_orbit", None)],
        checkouts: vec![WorkspaceCheckout {
            workspace_id: "ws_orbit".to_string(),
            repo_root: "/repos/orbit".into(),
            orbit_dir: "/repos/orbit/.orbit".into(),
            path_overrides: Vec::new(),
            role: None,
            owner_machine_id: None,
        }],
        ..Default::default()
    };

    assign_checkout_role(
        &mut registry,
        "ws_orbit",
        WorkspaceCheckoutRole::Owner,
        None,
        Some("hm_local"),
    )
    .expect("explicit local owner declaration");
    assert_eq!(
        registry.workspaces[0].owner_machine_id.as_deref(),
        Some("hm_local")
    );
    save_registry_to(&registry, &path).expect("persist explicit owner declaration");

    let loaded = load_registry_from(&path).expect("reload explicit owner declaration");
    assert_eq!(
        loaded.workspaces[0].owner_machine_id.as_deref(),
        Some("hm_local")
    );
    assert_eq!(loaded.checkouts[0].role, Some(WorkspaceCheckoutRole::Owner));
    assert_eq!(
        loaded.owner_host_ids.get("hm_local").map(String::as_str),
        Some("test-host")
    );
}

#[test]
fn local_owner_rename_changes_only_the_display_name_projection() {
    let mut registry = WorkspaceRegistry {
        owner_host_ids: [
            ("hm_local".to_string(), "old".to_string()),
            ("hm_remote".to_string(), "remote".to_string()),
        ]
        .into_iter()
        .collect(),
        workspaces: vec![
            logical_workspace("ws_one", Some("hm_local")),
            logical_workspace("ws_two", Some("hm_local")),
            logical_workspace("ws_remote", Some("hm_remote")),
        ],
        ..WorkspaceRegistry::default()
    };
    let stable_owners = registry
        .workspaces
        .iter()
        .map(|workspace| workspace.owner_machine_id.clone())
        .collect::<Vec<_>>();

    assert_eq!(
        rename_local_owner_host_id(&mut registry, "hm_local", "new").expect("rename"),
        2
    );
    assert_eq!(registry.owner_host_ids["hm_local"], "new");
    assert_eq!(registry.owner_host_ids["hm_remote"], "remote");
    assert_eq!(
        registry
            .workspaces
            .iter()
            .map(|workspace| workspace.owner_machine_id.clone())
            .collect::<Vec<_>>(),
        stable_owners
    );
}

#[test]
fn replica_role_rejects_transport_shaped_owner_before_any_mutation() {
    let root = tempdir().expect("tempdir");
    write_host_identity(root.path(), "spoke", "hm_local");
    let path = root.path().join("workspaces.json");
    write_json(
        &path,
        &json!({
            "schema_version": 1,
            "workspaces": [logical_workspace("ws_orbit", Some("hm_local"))],
            "checkouts": [{
                "workspace_id": "ws_orbit",
                "repo_root": "/repos/orbit",
                "orbit_dir": "/repos/orbit/.orbit",
                "role": "owner"
            }]
        }),
    );
    load_registry_from(&path).expect("canonicalize local owner display name");
    let original_bytes = fs::read(&path).expect("read original registry");

    for rejected in [
        "",
        "dk1",
        "user@dk1",
        "ssh:dk1",
        "hm_ssh:dk1",
        " /tmp/hub",
        "/tmp/hub",
        "ssh\\hub",
        "hm_owner\nother",
    ] {
        let mut registry = load_registry_from(&path).expect("load registry");
        let before = registry.clone();
        let error = assign_checkout_role(
            &mut registry,
            "ws_orbit",
            WorkspaceCheckoutRole::Replica,
            Some(rejected),
            Some("hm_local"),
        )
        .expect_err("transport-shaped owner must fail")
        .to_string();
        assert!(
            error.contains("machine_id") || error.contains("logical registry identifier"),
            "unexpected error for {rejected:?}: {error}"
        );
        assert_eq!(
            registry, before,
            "rejected owner {rejected:?} mutated the in-memory registry"
        );
        assert_eq!(
            fs::read(&path).expect("read registry after rejection"),
            original_bytes,
            "rejected owner {rejected:?} changed persisted bytes"
        );
    }
}

#[test]
fn validate_workspaces_reports_whether_any_status_changed() {
    let repo_root = tempdir().expect("repo tempdir");
    let mut registry = WorkspaceRegistry {
        workspaces: vec![logical_workspace("ws_orbit", None)],
        checkouts: vec![WorkspaceCheckout::owner(
            "ws_orbit".to_string(),
            repo_root.path().to_path_buf(),
            repo_root.path().join(".orbit"),
        )],
        ..Default::default()
    };

    // The checkout's repo_root exists and the workspace is already Active, so
    // nothing should change — callers like `orbit workspace list` rely on
    // this to skip an unnecessary registry write [ORB-10928].
    assert!(!validate_workspaces(&mut registry));
    assert_eq!(registry.workspaces[0].status, WorkspaceStatus::Active);

    drop(repo_root);
    assert!(validate_workspaces(&mut registry));
    assert_eq!(registry.workspaces[0].status, WorkspaceStatus::Invalid);

    // Once flagged invalid, re-validating with the same missing root is a
    // no-op.
    assert!(!validate_workspaces(&mut registry));
}
