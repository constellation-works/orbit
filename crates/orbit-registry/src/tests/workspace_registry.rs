use crate::workspace_registry::{
    WorkspaceRegistryMachineContext, assign_checkout_role, find_checkout_by_path, find_workspace,
    find_workspace_by_id, find_workspace_by_path, load_registry_from,
    load_registry_from_with_context_and_writer, load_registry_from_with_writer, remove_workspace,
    save_registry_to, with_registry_lock,
};
use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_types::workspace::{
    WORKSPACE_REGISTRY_SCHEMA_VERSION, Workspace, WorkspaceCheckout, WorkspaceCheckoutRole,
    WorkspaceRegistry, WorkspaceStatus,
};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

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

fn write_machine_identity(root: &Path, _legacy_mode: &str, machine_id: &str) {
    write_current_machine_identity(root, machine_id);
}

fn write_current_machine_identity(root: &Path, machine_id: &str) {
    fs::write(
        root.join("config.toml"),
        format!(
            "[machine]\nid = \"{machine_id}\"\nname = \"test-machine\"\ntask_prefix = \"ORB\"\n"
        ),
    )
    .expect("write machine identity");
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
        write_machine_identity(root.path(), mode, "hm_local");
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

/// A checkout is recorded at its resolved path. Once its directory is deleted
/// the path can no longer be canonicalized whole, so a spelling through a
/// symlinked ancestor has to be resolved up to the first directory that still
/// exists, or `workspace remove <path>` cannot name the checkout it recorded.
#[cfg(unix)]
#[test]
fn a_deleted_checkout_is_found_by_a_path_spelled_through_a_symlinked_ancestor() {
    let temp = tempfile::tempdir().expect("tempdir");
    let real = temp.path().join("real");
    std::fs::create_dir_all(real.join("repo")).expect("checkout directory");
    let linked = temp.path().join("linked");
    std::os::unix::fs::symlink(&real, &linked).expect("link the checkout parent");
    let recorded = linked
        .join("repo")
        .canonicalize()
        .expect("resolve the checkout");
    let registry = WorkspaceRegistry {
        workspaces: vec![logical_workspace("ws_gone", None)],
        checkouts: vec![WorkspaceCheckout::owner(
            "ws_gone".to_string(),
            recorded.clone(),
            recorded.join(".orbit"),
        )],
        ..Default::default()
    };
    std::fs::remove_dir_all(real.join("repo")).expect("delete the checkout");

    for spelled in [linked.join("repo"), linked.join("repo/src")] {
        assert_eq!(
            find_checkout_by_path(&registry, &spelled)
                .map(|checkout| checkout.workspace_id.as_str()),
            Some("ws_gone"),
            "{} names the deleted checkout recorded at {}",
            spelled.display(),
            recorded.display()
        );
    }
    assert!(find_checkout_by_path(&registry, &linked.join("elsewhere")).is_none());
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
fn migrating_read_rechecks_under_lock_and_preserves_interleaved_registration() {
    const TEST: &str = "tests::workspace_registry::migrating_read_rechecks_under_lock_and_preserves_interleaved_registration";
    const CHILD: &str = "ORBIT_TEST_REGISTRY_MIGRATION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output_dir = tempdir().expect("child output directory");
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        command
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1");
        orbit_common::test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let output = orbit_common::test_env::run_child_test(&mut command, TEST, output_dir.path());
        orbit_common::test_env::assert_child_test_passed(
            TEST,
            output.status,
            &output.stdout,
            &output.stderr,
        );
        return;
    }
    let root = tempdir().expect("tempdir");
    let path = root.path().join("workspaces.json");
    write_json(
        &path,
        &json!({
            "workspaces": [],
            "path_overrides": {}
        }),
    );
    let reads = std::cell::Cell::new(0);

    let loaded = load_registry_from_with_context_and_writer(
        &path,
        |_| {
            reads.set(reads.get() + 1);
            if reads.get() == 1 {
                // The reader already holds legacy bytes. Complete a second writer's
                // locked registration before the reader can acquire the migration lock.
                with_registry_lock(&path, || {
                    let mut registered = load_registry_from(&path)?;
                    registered
                        .workspaces
                        .push(logical_workspace("ws_new", None));
                    save_registry_to(&registered, &path)
                })
                .expect("interleaved locked registration");
            }
            Ok(WorkspaceRegistryMachineContext { machine_id: None })
        },
        save_registry_to,
    )
    .expect("load and migrate under lock");

    let persisted = load_registry_from(&path).expect("read persisted registry");
    assert!(
        find_workspace_by_id(&persisted, "ws_new").is_some(),
        "migration must not overwrite the interleaved locked registration"
    );
    assert!(
        find_workspace_by_id(&loaded, "ws_new").is_some(),
        "the returned snapshot must include the interleaved registration"
    );
}
