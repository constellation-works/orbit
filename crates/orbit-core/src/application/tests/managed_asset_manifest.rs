use std::io;
use std::path::Path;

use orbit_common::OrbitError;
use tempfile::tempdir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::super::managed_assets::MANAGED_ASSET_MANIFEST_FILE;
use super::super::managed_assets::{
    managed_manifest_write_is_skippable, record_managed_manifest_write,
};
use crate::OrbitRuntime;
use crate::application::job::JobCatalogFilter;
use crate::bootstrap::activity::seed_default_activities;
use crate::bootstrap::init::{InitOptions, init_workspace_at_root};
use orbit_config::ConfigSeed;

fn init_global(root: &Path) {
    init_workspace_at_root(
        root,
        InitOptions {
            global_only: true,
            refresh_defaults: true,
            config_seed: Some(ConfigSeed::default()),
            ..Default::default()
        },
    )
    .expect("initialize global defaults");
}

#[cfg(unix)]
fn make_read_only(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555))
        .expect("make managed asset directory read-only");
}

#[cfg(unix)]
fn make_writable(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("restore managed asset directory permissions");
}

fn assert_invalid_input(error: OrbitError, needles: &[&str]) {
    match error {
        OrbitError::InvalidInput(message) => {
            for needle in needles {
                assert!(
                    message.contains(needle),
                    "expected `{needle}` in `{message}`"
                );
            }
        }
        other => panic!("expected InvalidInput, got {other}"),
    }
}

#[cfg(unix)]
#[test]
fn needed_manifest_write_on_readonly_dir_warns_and_continues() {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    init_global(&global_root);

    let activities_dir = global_root.join("resources/activities");
    let jobs_dir = global_root.join("resources/jobs");
    let manifest = activities_dir.join(MANAGED_ASSET_MANIFEST_FILE);
    std::fs::remove_file(&manifest).expect("drop manifest to force a write");

    make_read_only(&activities_dir);
    make_read_only(&jobs_dir);
    let seeded = seed_default_activities(&activities_dir, true);
    let workspace_root = root.path().join("repo/.orbit");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root)
        .and_then(|runtime| runtime.list_job_catalog_with_last_run(true, JobCatalogFilter::All));
    make_writable(&activities_dir);
    make_writable(&jobs_dir);

    let seeded = seeded.expect("EROFS/EACCES on a needed manifest write must not fail the command");
    assert!(
        seeded.warnings.iter().any(|warning| {
            warning.contains("could not write managed activity asset manifest")
                && warning.contains(MANAGED_ASSET_MANIFEST_FILE)
        }),
        "expected a recorded warning, got {:?}",
        seeded.warnings
    );
    assert!(
        !manifest.exists(),
        "a skipped read-only write must leave the missing manifest missing"
    );
    runtime.expect("read-only runtime bootstrap succeeds after a skipped manifest write");
}

#[test]
fn malformed_manifest_fails_closed_with_repair_message() {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    init_global(&global_root);

    let activities_dir = global_root.join("resources/activities");
    std::fs::write(
        activities_dir.join(MANAGED_ASSET_MANIFEST_FILE),
        "{not-json\n",
    )
    .expect("write malformed manifest");

    let error = seed_default_activities(&activities_dir, true)
        .expect_err("malformed manifest must fail closed");
    assert_invalid_input(
        error,
        &[
            "is invalid:",
            "repair it or move it aside only after reviewing the managed YAML files",
        ],
    );
}

#[test]
fn unexpected_manifest_asset_kind_fails_closed() {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    init_global(&global_root);

    let activities_dir = global_root.join("resources/activities");
    let manifest_path = activities_dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let raw = std::fs::read_to_string(&manifest_path).expect("read manifest");
    let mut manifest: serde_json::Value = serde_json::from_str(&raw).expect("parse manifest");
    manifest["assetKind"] = serde_json::Value::String("job".to_string());
    std::fs::write(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("serialize manifest")
        ),
    )
    .expect("write wrong-kind manifest");

    let error = seed_default_activities(&activities_dir, true)
        .expect_err("unexpected asset_kind must fail closed");
    assert_invalid_input(error, &["is for `job`", "expected `activity`"]);
}

#[cfg(unix)]
#[test]
fn stubbed_erofs_and_eacces_are_skippable() {
    for errno in [libc::EROFS, libc::EACCES] {
        let error = io::Error::from_raw_os_error(errno);
        assert!(
            managed_manifest_write_is_skippable(&error),
            "{error} should be skippable"
        );
        let mut warnings = Vec::new();
        record_managed_manifest_write(
            Path::new("/tmp/.orbit-managed-assets.json"),
            "activity",
            Err(io::Error::from_raw_os_error(errno)),
            &mut warnings,
        )
        .expect("skippable write must not fail closed");
        assert_eq!(warnings.len(), 1, "{errno}");
        assert!(
            warnings[0].contains("could not write managed activity asset manifest"),
            "{}",
            warnings[0]
        );
    }
}

#[cfg(unix)]
#[test]
fn stubbed_enospc_and_eio_fail_closed() {
    for errno in [libc::ENOSPC, libc::EIO] {
        let error = io::Error::from_raw_os_error(errno);
        assert!(
            !managed_manifest_write_is_skippable(&error),
            "{error} must stay fatal"
        );
        let mut warnings = Vec::new();
        let mapped = record_managed_manifest_write(
            Path::new("/tmp/.orbit-managed-assets.json"),
            "activity",
            Err(io::Error::from_raw_os_error(errno)),
            &mut warnings,
        )
        .expect_err("non-permission write must fail closed");
        assert!(warnings.is_empty(), "{warnings:?}");
        match mapped {
            OrbitError::Io(message) => {
                assert!(
                    message.contains("write managed activity asset manifest"),
                    "{message}"
                );
            }
            other => panic!("expected OrbitError::Io, got {other}"),
        }
    }
}

#[test]
fn constructed_storage_full_and_other_errors_fail_closed() {
    for error in [
        io::Error::new(io::ErrorKind::StorageFull, "no space left"),
        io::Error::other("input/output error"),
    ] {
        assert!(
            !managed_manifest_write_is_skippable(&error),
            "{error} must stay fatal"
        );
        let mut warnings = Vec::new();
        let mapped = record_managed_manifest_write(
            Path::new("/tmp/.orbit-managed-assets.json"),
            "activity",
            Err(error),
            &mut warnings,
        )
        .expect_err("non-permission write must fail closed");
        assert!(warnings.is_empty(), "{warnings:?}");
        match mapped {
            OrbitError::Io(message) => {
                assert!(
                    message.contains("write managed activity asset manifest"),
                    "{message}"
                );
            }
            other => panic!("expected OrbitError::Io, got {other}"),
        }
    }
}
