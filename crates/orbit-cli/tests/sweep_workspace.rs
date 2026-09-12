#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! `orbit sweep --workspace <selector>` must restrict a pass to one registered
//! workspace [ORB-12108]. The flag used to be accepted and ignored, so an
//! operator asking for one workspace's due routines got the whole host.
//!
//! Each fixture host owns two registered workspaces with their own `.orbit/`
//! directories, both routine sources, each with one enabled routine. Routines
//! report their source workspace, so a pass that visited the unselected
//! workspace is directly observable — and, in the live case, so is the
//! scheduler state it would have written.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

/// Registered names of the two fixture workspaces.
const SELECTED: &str = "sweep-selected";
const OTHER: &str = "sweep-other";

/// The seeded routine each fixture workspace enables. Seeding renders one
/// definition per workspace with a workspace-unique name, all of them disabled;
/// enabling exactly one per workspace gives each side a routine that reaches
/// the scheduler.
const ENABLED_ROUTINE: &str = "worktree_gc.yaml";

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    selected_repo: PathBuf,
}

impl Fixture {
    /// Both workspaces are registered against the same HOME-resolved root, so
    /// each keeps its own repo-local `.orbit/` and its own routine set.
    fn initialized() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");

        let selected_repo = temp.path().join("selected");
        let other_repo = temp.path().join("other");
        init_git_repo(&selected_repo);
        init_git_repo(&other_repo);

        run_success(
            &selected_repo,
            &home,
            &[
                "init",
                "--non-interactive",
                "--host-name",
                "sweep-workspace-host",
                "--task-prefix",
                "SW",
            ],
        );
        for (repo, name) in [(&selected_repo, SELECTED), (&other_repo, OTHER)] {
            run_success(repo, &home, &["workspace", "init", "--name", name]);
            enable_seeded_routine(repo);
        }

        Self {
            _temp: temp,
            home,
            selected_repo,
        }
    }

    fn sweep(&self, args: &[&str]) -> Value {
        run_json(&self.selected_repo, &self.home, args)
    }
}

#[test]
fn dry_run_sweep_evaluates_only_the_selected_workspace() {
    let fixture = Fixture::initialized();

    // The unfiltered pass still visits every registered workspace.
    let every_workspace = fixture.sweep(&["sweep", "--dry-run", "--format", "json"]);
    assert_eq!(
        report_sources(&every_workspace),
        BTreeSet::from([OTHER.to_string(), SELECTED.to_string()]),
        "unfiltered dry-run must report both workspaces: {every_workspace}"
    );

    let selected_only = fixture.sweep(&[
        "--workspace",
        SELECTED,
        "sweep",
        "--dry-run",
        "--format",
        "json",
    ]);
    assert_eq!(
        report_sources(&selected_only),
        BTreeSet::from([SELECTED.to_string()]),
        "filtered dry-run must report only the selected workspace: {selected_only}"
    );
    assert!(
        report_actions(&selected_only).contains("would_baseline"),
        "the selected workspace's enabled routine must still be evaluated: {selected_only}"
    );
}

#[test]
fn live_sweep_fires_only_the_selected_workspace() {
    let fixture = Fixture::initialized();

    let selected_only = fixture.sweep(&["--workspace", SELECTED, "sweep", "--format", "json"]);
    assert_eq!(
        report_sources(&selected_only),
        BTreeSet::from([SELECTED.to_string()]),
        "filtered live pass must report only the selected workspace: {selected_only}"
    );
    assert!(
        report_actions(&selected_only).contains("baselined"),
        "the selected workspace's enabled routine must reach the scheduler: {selected_only}"
    );

    // The unselected workspace must also be untouched in the store: its
    // enabled routine takes its first-observation baseline now, on the first
    // pass that actually visits it, while the already-baselined selected one
    // does not repeat.
    let every_workspace = fixture.sweep(&["sweep", "--format", "json"]);
    assert_eq!(
        baselined_sources(&every_workspace),
        BTreeSet::from([OTHER.to_string()]),
        "the filtered pass must not have recorded scheduler state for {OTHER}: {every_workspace}"
    );
}

#[test]
fn unknown_workspace_selector_fails_instead_of_sweeping_the_host() {
    let fixture = Fixture::initialized();

    command(&fixture.selected_repo, &fixture.home)
        .args(["--workspace", "not-registered", "sweep", "--dry-run"])
        .assert()
        .failure();
}

/// Source workspace of every reported routine.
fn report_sources(outcome: &Value) -> BTreeSet<String> {
    reports(outcome)
        .iter()
        .map(|report| report["source"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Every reported action, regardless of routine.
fn report_actions(outcome: &Value) -> BTreeSet<String> {
    reports(outcome)
        .iter()
        .map(|report| report["action"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Source workspaces whose routines recorded a first-observation baseline.
fn baselined_sources(outcome: &Value) -> BTreeSet<String> {
    reports(outcome)
        .iter()
        .filter(|report| report["action"] == "baselined")
        .map(|report| report["source"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn reports(outcome: &Value) -> &Vec<Value> {
    outcome["reports"]
        .as_array()
        .unwrap_or_else(|| panic!("sweep outcome must carry reports: {outcome}"))
}

fn run_success(cwd: &Path, home: &Path, args: &[&str]) {
    command(cwd, home).args(args).assert().success();
}

fn run_json(cwd: &Path, home: &Path, args: &[&str]) -> Value {
    let output = command(cwd, home)
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap_or_else(|error| {
        panic!(
            "parse JSON from `orbit {}`: {error}\nstdout:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output)
        )
    })
}

/// Every command runs against the disposable HOME, with no inherited managed
/// authority that could outrank it.
fn command(cwd: &Path, home: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

fn enable_seeded_routine(repo: &Path) {
    let path = repo.join(".orbit").join("routines").join(ENABLED_ROUTINE);
    let definition = fs::read_to_string(&path).expect("read seeded routine");
    let enabled = definition.replace("\nenabled: false\n", "\nenabled: true\n");
    assert_ne!(
        definition,
        enabled,
        "seeded routine {} must ship disabled",
        path.display()
    );
    fs::write(&path, enabled).expect("enable seeded routine");
}

fn init_git_repo(repo: &Path) {
    fs::create_dir_all(repo).expect("create repo");
    run_git(repo, &["init", "--quiet"]);
    run_git(repo, &["config", "user.name", "Orbit Test"]);
    run_git(repo, &["config", "user.email", "orbit-test@example.com"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "# sweep workspace test\n").expect("write readme");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "--quiet", "-m", "initial"]);
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git -C {} {} failed\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
