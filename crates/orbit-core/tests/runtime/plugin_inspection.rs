//! Plugin inspection uses the same workspace pins as sync and runtime loading.

use std::path::Path;
use std::process::Command;

use orbit_core::application::plugin::{plugin_doctor, sync_plugins};
use orbit_core::bootstrap::init::{InitOptions, init_workspace_at_root};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::plugin::PluginStatus;

use super::dispatch_admission::isolated;

fn git(repo: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command.current_dir(repo).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn linked_worktree_doctor_validates_shared_pins_and_ignores_local_pins() {
    if !isolated(
        "plugin_inspection::linked_worktree_doctor_validates_shared_pins_and_ignores_local_pins",
    ) {
        return;
    }

    let original_cwd = std::env::current_dir().unwrap();
    let fixture = tempfile::tempdir().unwrap();
    let fixture_root = fixture.path().canonicalize().unwrap();
    let main = fixture_root.join("main");
    let linked = fixture_root.join("linked");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init"]);
    git(
        &main,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    // Bootstrap this exact root before ordinary lookup can see an ancestor's
    // .orbit when TMPDIR is inside a managed checkout.
    init_workspace_at_root(&main.join(".orbit"), InitOptions::default())
        .expect("initialize the fixture's main workspace");
    git(
        &main,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );

    let shared_root = main.join(".orbit");
    let local_root = linked.join(".orbit");
    let malformed_pins = "schemaVersion: 1\nplugins: [\n";
    std::fs::write(shared_root.join("plugins.yaml"), malformed_pins).unwrap();
    std::env::set_current_dir(&linked).unwrap();
    let runtime = OrbitRuntime::initialize().expect("initialize from a linked worktree");
    assert_eq!(runtime.shared_root(), shared_root);
    assert_eq!(runtime.paths().orbit_dir, shared_root);
    assert_eq!(runtime.paths().local_dir, local_root);

    let sync_error = sync_plugins(&runtime, true, &[]).expect_err("shared pins do not parse");
    assert!(matches!(sync_error, OrbitError::InvalidInput(_)));
    let rows = plugin_doctor(&runtime).expect("doctor returns a finding for invalid pins");
    assert_eq!(
        rows.len(),
        1,
        "invalid shared pins must produce one finding"
    );
    assert_eq!(rows[0].plugin, "pin file");
    assert_eq!(rows[0].status, PluginStatus::Inactive);
    assert!(!rows[0].intentional);
    assert_eq!(rows[0].message, sync_error.to_string());
    drop(runtime);

    // A stale worktree-local file is unrelated to the shared pins in use.
    std::fs::write(
        shared_root.join("plugins.yaml"),
        "schemaVersion: 1\nplugins: []\n",
    )
    .unwrap();
    std::fs::create_dir_all(&local_root).unwrap();
    std::fs::write(local_root.join("plugins.yaml"), malformed_pins).unwrap();
    let runtime = OrbitRuntime::initialize().expect("reload with valid shared pins");
    assert!(plugin_doctor(&runtime).unwrap().is_empty());
    assert!(sync_plugins(&runtime, true, &[]).unwrap().is_empty());
    drop(runtime);
    std::env::set_current_dir(original_cwd).unwrap();
}
