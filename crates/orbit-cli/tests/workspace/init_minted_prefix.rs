#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! `orbit init --task-prefix` on a machine that already minted task ids.
//!
//! Tasks created before `orbit init` mint under the historical `ORB` prefix.
//! Writing an identity naming another prefix used to succeed and then fail
//! every later command at the allocator. Init must refuse first, writing
//! nothing, and leave the machine working.

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use tempfile::tempdir;

fn orbit_at_home(work: &Path, home: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ORBIT_SKIP_HOST_PREREQUISITES", "1")
        // Nothing on PATH: init must never find, let alone launch, a real
        // agent CLI or `gh` while this fixture runs.
        .env("PATH", work.join("empty-path"))
        .env_remove("RUST_LOG");
    command
}

fn run(work: &Path, home: &Path, args: &[&str]) -> Output {
    orbit_at_home(work, home)
        .args(args)
        .output()
        .expect("run orbit")
}

#[test]
fn init_refuses_a_prefix_that_contradicts_minted_ids_and_writes_no_identity() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("fixture home");
    crate::git_repo::init(&work);

    let init = run(&work, &home, &["workspace", "init", "--name", "minted"]);
    assert!(init.status.success(), "workspace init: {init:?}");
    let add = run(
        &work,
        &home,
        &["task", "add", "--title", "first", "--complexity", "low"],
    );
    assert!(add.status.success(), "task add: {add:?}");

    let refused = run(
        &work,
        &home,
        &[
            "init",
            "--task-prefix",
            "QA",
            "--machine-name",
            "box",
            "--non-interactive",
        ],
    );
    assert!(!refused.status.success(), "init must refuse: {refused:?}");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("already minted") && stderr.contains("Nothing was written"),
        "the refusal explains the fixed prefix: {stderr}"
    );

    let config = fs::read_to_string(home.join(".orbit").join("config.toml")).unwrap_or_default();
    assert!(
        !config.contains("[machine]"),
        "no machine identity may be written by a refused init:\n{config}"
    );
    let list = run(&work, &home, &["task", "list"]);
    assert!(
        list.status.success(),
        "the machine keeps working after the refusal: {list:?}"
    );
}

#[test]
fn init_on_a_machine_with_no_minted_ids_still_creates_the_identity() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("fixture home");
    crate::git_repo::init(&work);

    let init = run(&work, &home, &["workspace", "init", "--name", "fresh"]);
    assert!(init.status.success(), "workspace init: {init:?}");
    let created = run(
        &work,
        &home,
        &[
            "init",
            "--task-prefix",
            "QA",
            "--machine-name",
            "box",
            "--non-interactive",
        ],
    );
    assert!(created.status.success(), "init: {created:?}");
    let add = run(
        &work,
        &home,
        &["task", "add", "--title", "first", "--complexity", "low"],
    );
    assert!(add.status.success(), "task add: {add:?}");
    assert!(
        String::from_utf8_lossy(&add.stdout).contains("QA-"),
        "ids mint under the chosen prefix: {add:?}"
    );
}
