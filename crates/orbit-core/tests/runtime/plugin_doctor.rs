//! Plugin diagnostics use the main checkout's pins from a linked worktree.

use std::path::Path;
use std::process::Command;

use orbit_core::OrbitRuntime;
use orbit_core::application::plugin::{plugin_doctor, sync_plugins};
use orbit_types::plugin::PluginStatus;

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("fixture git command");
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

#[test]
fn linked_worktree_doctor_validates_shared_pins_and_ignores_local_pins() {
    if !super::dispatch_admission::isolated(
        "plugin_doctor::linked_worktree_doctor_validates_shared_pins_and_ignores_local_pins",
    ) {
        return;
    }
    let fixture = tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir())
        .expect("isolated linked worktree fixture");
    let repo = fixture.path().join("repo");
    let worktree = fixture.path().join("worktree");
    let shared = repo.join(".orbit");
    let local = worktree.join(".orbit");
    std::fs::create_dir_all(&shared).expect("shared Orbit root");
    git(&repo, &["init", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Orbit Test",
            "-c",
            "user.email=orbit-test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            worktree.to_str().expect("fixture worktree path"),
        ],
    );
    // This child owns its process, so cwd changes cannot affect other tests.
    std::env::set_current_dir(&worktree).expect("run from the linked checkout");
    let malformed = "schemaVersion: 1\nplugins: [\n";
    std::fs::write(shared.join("plugins.yaml"), malformed).expect("malformed shared pins");
    let runtime = OrbitRuntime::initialize().expect("linked-worktree runtime");
    assert_eq!(runtime.shared_root(), shared.canonicalize().unwrap());
    assert_eq!(runtime.paths().local_dir, local);
    assert_ne!(runtime.shared_root(), runtime.paths().local_dir);

    let rows = plugin_doctor(&runtime).expect("doctor with malformed shared pins");
    let findings = rows
        .iter()
        .filter(|row| row.plugin == "pin file")
        .collect::<Vec<_>>();
    assert_eq!(
        findings.len(),
        1,
        "invalid shared pins must be a finding: {rows:?}"
    );
    assert_eq!(findings[0].status, PluginStatus::Inactive);
    assert!(!findings[0].intentional);
    let sync_error = sync_plugins(&runtime, true, &[]).expect_err("sync rejects the same pins");
    assert_eq!(findings[0].message, sync_error.to_string());

    std::fs::write(
        shared.join("plugins.yaml"),
        "schemaVersion: 1\nplugins: []\n",
    )
    .expect("repair shared pins");
    std::fs::create_dir_all(&local).expect("worktree-local Orbit root");
    std::fs::write(local.join("plugins.yaml"), malformed).expect("stale local pins");
    let runtime = OrbitRuntime::initialize().expect("reopen with valid shared pins");
    assert!(
        plugin_doctor(&runtime)
            .expect("doctor ignores local pins")
            .is_empty()
    );
    assert!(
        sync_plugins(&runtime, true, &[])
            .expect("sync ignores local pins")
            .is_empty()
    );
}
