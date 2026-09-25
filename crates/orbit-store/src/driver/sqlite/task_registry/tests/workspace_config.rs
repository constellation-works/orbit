//! Workspace config files in the orbit directory and the workspace-id grammar
//! shared by their consumers.

use std::fs;

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use tempfile::TempDir;

use super::super::RegisterWorkspaceParams;
use super::store;
use crate::contracts::WorkspaceConfig;
use crate::{
    read_workspace_config, read_workspace_config_optional, workspace_config_path,
    workspace_id_for_orbit_dir, write_workspace_config,
};

#[test]
fn workspace_config_round_trips_and_validates() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "orbit-test-abcdef".into(),
        },
    )
    .expect("write config");

    let read = read_workspace_config(&orbit_dir).expect("read config");
    assert_eq!(read.workspace_id, "orbit-test-abcdef");

    atomic_write_text(
        &workspace_config_path(&orbit_dir),
        "schema_version: 2\nworkspace_id: orbit-test-abcdef\n",
    )
    .expect("write wrong schema");
    assert!(matches!(
        read_workspace_config(&orbit_dir),
        Err(OrbitError::InvalidInput(_))
    ));

    atomic_write_text(
        &workspace_config_path(&orbit_dir),
        "schema_version: 1\nworkspace_id: ''\n",
    )
    .expect("write empty id");
    assert!(matches!(
        read_workspace_config(&orbit_dir),
        Err(OrbitError::InvalidInput(_))
    ));

    atomic_write_text(
        &workspace_config_path(&orbit_dir),
        "schema_version: 1\nworkspace_id: orbit-test-abcdef\nextra: nope\n",
    )
    .expect("write unknown field");
    assert!(matches!(
        read_workspace_config(&orbit_dir),
        Err(OrbitError::InvalidInput(_))
    ));
}

#[test]
fn persistence_consumers_share_workspace_id_grammar() {
    let cases = [
        ("ws_orbit", Some("ws_orbit")),
        ("ws_orbit-main_2", Some("ws_orbit-main_2")),
        ("orbit-test-abcdef", Some("orbit-test-abcdef")),
        ("  orbit-test-abcdef  ", Some("orbit-test-abcdef")),
        ("ws_", None),
        ("ws_Orbit", None),
        ("-orbit-abcdef", None),
        ("orbit--test-abcdef", None),
        ("orbit-ABCDEF", None),
        ("orbit-abcde", None),
        ("orbit-abcdef0", None),
        ("orbit/test-abcdef", None),
    ];

    for (raw, expected) in cases {
        let temp = TempDir::new().expect("tempdir");
        let orbit_dir = temp.path().join(".orbit");
        let file_result = write_workspace_config(
            &orbit_dir,
            &WorkspaceConfig {
                schema_version: 1,
                workspace_id: raw.into(),
            },
        );
        let store = store(&temp);
        let registry_result = store.register_workspace(RegisterWorkspaceParams {
            partition_id: raw.into(),
            slug: "Workspace".into(),
            repo_fingerprint: None,
        });

        match expected {
            Some(expected) => {
                file_result.expect("file workspace binding accepts id");
                assert_eq!(
                    read_workspace_config(&orbit_dir)
                        .expect("read file workspace binding")
                        .workspace_id,
                    expected
                );
                assert_eq!(
                    registry_result.expect("registry accepts id").partition_id,
                    expected
                );
            }
            None => {
                assert!(file_result.is_err(), "file binding accepted {raw:?}");
                assert!(registry_result.is_err(), "registry accepted {raw:?}");
            }
        }
    }
}

#[test]
fn workspace_config_optional_distinguishes_missing_file() {
    let temp = TempDir::new().expect("tempdir");

    assert_eq!(
        read_workspace_config_optional(&temp.path().join(".orbit")).expect("read optional config"),
        None
    );
}

#[test]
fn workspace_config_reads_from_a_normalized_orbit_directory() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws-test-abcdef".into(),
        },
    )
    .expect("write config");

    let normalized_alias = orbit_dir.join("..").join(".orbit");
    let config = read_workspace_config(&normalized_alias).expect("read config through alias");

    assert_eq!(config.workspace_id, "ws-test-abcdef");
}

#[cfg(unix)]
#[test]
fn workspace_config_rejects_a_symlink_outside_the_orbit_directory() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");
    fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    let outside_config = temp.path().join("outside.yaml");
    atomic_write_text(
        &outside_config,
        "schema_version: 1\nworkspace_id: ws-test-abcdef\n",
    )
    .expect("write outside config");
    std::os::unix::fs::symlink(&outside_config, workspace_config_path(&orbit_dir))
        .expect("link outside config");

    assert!(matches!(
        read_workspace_config_optional(&orbit_dir),
        Err(OrbitError::InvalidInput(_))
    ));
}

#[test]
fn workspace_id_for_orbit_dir_returns_id_from_config() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws-test-abcdef".into(),
        },
    )
    .expect("write config");

    assert_eq!(
        workspace_id_for_orbit_dir(&orbit_dir).expect("workspace id"),
        "ws-test-abcdef"
    );
}

#[test]
fn workspace_id_for_orbit_dir_accepts_canonical_logical_registry_id() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_orbit-main".into(),
        },
    )
    .expect("write config");

    assert_eq!(
        workspace_id_for_orbit_dir(&orbit_dir).expect("workspace id"),
        "ws_orbit-main"
    );
}

#[test]
fn workspace_id_for_orbit_dir_missing_file_names_path_and_key() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");

    let err = workspace_id_for_orbit_dir(&orbit_dir).expect_err("missing config");
    let message = err.to_string();
    let config_path = workspace_config_path(&orbit_dir).display().to_string();
    assert!(message.contains("config.yaml"));
    assert!(message.contains("workspace_id"));
    assert!(message.contains(&config_path));
    assert_eq!(message.matches(&config_path).count(), 1);
}

#[test]
fn workspace_id_for_orbit_dir_malformed_yaml_returns_invalid_input() {
    let temp = TempDir::new().expect("tempdir");
    let orbit_dir = temp.path().join(".orbit");
    atomic_write_text(&workspace_config_path(&orbit_dir), "not: [valid")
        .expect("write malformed config");

    assert!(matches!(
        workspace_id_for_orbit_dir(&orbit_dir),
        Err(OrbitError::InvalidInput(_))
    ));
}
