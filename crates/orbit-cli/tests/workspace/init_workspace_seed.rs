#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! `orbit init` on a host with a registered workspace whose catalog predates a
//! routine the release ships. Init creates that routine without a manual
//! `orbit workspace sync`, leaves the operator's own definitions alone, names
//! what it created, and clears the doctor error the gap caused.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

use crate::git_repo;

fn orbit(work: &Path, home: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ORBIT_SKIP_HOST_PREREQUISITES", "1")
        .env("PATH", work.join("empty-path"))
        .env_remove("RUST_LOG")
        .env_remove("ORBIT_HOME");
    command.timeout(std::time::Duration::from_secs(60));
    command
}

fn run_ok(work: &Path, home: &Path, args: &[&str]) -> Output {
    let output = orbit(work, home).args(args).output().expect("run orbit");
    assert!(
        output.status.success(),
        "orbit {args:?} failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// The status doctor reports for one definition-artifact check.
fn doctor_status(work: &Path, home: &Path, check: &str) -> String {
    let output = orbit(work, home)
        .args(["doctor", "--json"])
        .output()
        .expect("run orbit doctor");
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "doctor JSON parse failed: {error}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    rows.as_array()
        .and_then(|rows| rows.iter().find(|row| row["check"] == check))
        .and_then(|row| row["status"].as_str())
        .unwrap_or_else(|| panic!("no `{check}` row in doctor output: {rows}"))
        .to_string()
}

/// A host whose registered workspace was seeded by an earlier binary that did
/// not ship `store_gc`: no file and no provenance for it.
fn upgraded_workspace(temp: &Path) -> (PathBuf, PathBuf) {
    let home = temp.join("home");
    let work = temp.join("upgraded");
    fs::create_dir_all(&home).expect("fixture home");
    git_repo::init(&work);

    run_ok(
        &work,
        &home,
        &[
            "init",
            "--non-interactive",
            "--machine-name",
            "box",
            "--task-prefix",
            "WSD",
        ],
    );
    run_ok(&work, &home, &["workspace", "init", "--name", "upgraded"]);

    let catalog = work.join(".orbit/routines");
    fs::remove_file(catalog.join("store_gc.yaml")).expect("remove shipped routine");
    let manifest_path = catalog.join(".orbit-managed-assets.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    manifest["assets"]
        .as_object_mut()
        .expect("assets object")
        .remove("store_gc");
    manifest["routineProvenance"]
        .as_object_mut()
        .expect("routine provenance object")
        .remove("store_gc");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write manifest");
    (work, home)
}

#[test]
fn init_creates_a_routine_the_release_ships_without_a_workspace_sync() {
    let temp = tempdir().expect("fixture tempdir");
    let (work, home) = upgraded_workspace(temp.path());
    let catalog = work.join(".orbit/routines");

    assert_ne!(
        doctor_status(&work, &home, "artifacts-routines"),
        "ok",
        "the gap must be visible to doctor before init runs"
    );

    let init = run_ok(&work, &home, &["init", "--non-interactive"]);
    let stdout = String::from_utf8_lossy(&init.stdout);
    assert!(
        stdout.contains("workspace upgraded: created=1"),
        "init names the workspace and its created count: {stdout}"
    );
    assert!(
        stdout.contains("store_gc.yaml"),
        "init names the created file: {stdout}"
    );

    let created = fs::read_to_string(catalog.join("store_gc.yaml")).expect("created routine");
    assert!(
        created.contains("enabled: false"),
        "a created routine keeps the enabled value it ships with"
    );
    assert_eq!(
        doctor_status(&work, &home, "artifacts-routines"),
        "ok",
        "doctor is clean after init, with no `orbit workspace sync`"
    );
}

#[test]
fn init_leaves_operator_edited_and_user_authored_routines_untouched() {
    let temp = tempdir().expect("fixture tempdir");
    let (work, home) = upgraded_workspace(temp.path());
    let catalog = work.join(".orbit/routines");

    let edited = catalog.join("task_pilot.yaml");
    let edited_before = fs::read(&edited).expect("read task_pilot");
    let edited_after = [edited_before.as_slice(), b"# operator note\n"].concat();
    fs::write(&edited, &edited_after).expect("edit shipped routine");
    fs::write(catalog.join("mine.yaml"), "user-authored\n").expect("user routine");

    let init = run_ok(&work, &home, &["init", "--non-interactive"]);
    let stdout = String::from_utf8_lossy(&init.stdout);
    assert!(
        stdout.contains("workspace upgraded: created=1"),
        "the absent default is still created beside the edits: {stdout}"
    );
    assert_eq!(
        fs::read(&edited).expect("edited routine"),
        edited_after,
        "init never rewrites an operator-edited routine"
    );
    assert_eq!(
        fs::read_to_string(catalog.join("mine.yaml")).expect("user routine"),
        "user-authored\n",
        "init never rewrites a user-authored routine"
    );
}
