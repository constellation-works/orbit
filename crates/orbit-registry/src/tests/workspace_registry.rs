use crate::workspace_registry::{
    WorkspaceRegistryMachineContext, find_workspace_by_id, load_registry_from,
    load_registry_from_with_context_and_writer, load_registry_from_with_writer, save_registry_to,
    with_registry_lock,
};
use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
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

fn write_json(path: &Path, value: &Value) -> Vec<u8> {
    let bytes = serde_json::to_vec_pretty(value).expect("serialize fixture");
    fs::write(path, &bytes).expect("write fixture");
    bytes
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
