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
//! scheduler state and the worktrees its workers collect.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use chrono::{Duration as ChronoDuration, Utc};
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

use crate::routine_runs::RoutineRuns;

/// Registered names of the two fixture workspaces.
const SELECTED: &str = "sweep-selected";
const OTHER: &str = "sweep-other";

/// The seeded routine each fixture workspace enables. Seeding renders one
/// definition per workspace with a workspace-unique name, all of them disabled;
/// enabling exactly one per workspace gives each side a routine that reaches
/// the scheduler.
const ENABLED_ROUTINE: &str = "worktree_gc.yaml";

struct Fixture {
    temp: Option<TempDir>,
    home: PathBuf,
    selected_repo: PathBuf,
    other_repo: PathBuf,
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
                "--machine-name",
                "sweep-workspace-host",
                "--task-prefix",
                "SW",
            ],
        );
        for (repo, name) in [(&selected_repo, SELECTED), (&other_repo, OTHER)] {
            run_success(repo, &home, &["workspace", "init", "--name", name]);
            let shown = run_json(repo, &home, &["workspace", "show", "--json"]);
            let canonical = repo.canonicalize().expect("fixture checkout");
            assert_eq!(shown["registered"], true, "{shown}");
            assert_eq!(
                shown["checkout"]["repo_root"],
                canonical.to_string_lossy().as_ref(),
                "{shown}"
            );
            assert_eq!(
                shown["checkout"]["orbit_dir"],
                canonical.join(".orbit").to_string_lossy().as_ref(),
                "{shown}"
            );
            enable_seeded_routine(repo);
        }

        Self {
            temp: Some(temp),
            home,
            selected_repo,
            other_repo,
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
    if !isolated_live_sweep() {
        return;
    }
    let mut fixture = Fixture::initialized();
    // Discover and settle workers even if a sweep fails before reporting its
    // submitted IDs, or an assertion unwinds. This guard owns the roots.
    let mut runs = RoutineRuns::new(
        fixture.temp.take().expect("fixture roots"),
        &fixture.home,
        &[&fixture.selected_repo, &fixture.other_repo],
    );
    crate::fixture_crew::configure_sol(&fixture.home.join(".orbit"));
    run_success(
        &fixture.selected_repo,
        &fixture.home,
        &[
            "config",
            "set",
            "workflow.resource_throttle.enabled",
            "false",
            "--global",
        ],
    );

    let selected_only = fixture.sweep(&["--workspace", SELECTED, "sweep", "--format", "json"]);
    runs.retain_sweep(&selected_only);
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
    let selected_routine = format!("worktree-gc-{SELECTED}");
    let other_routine = format!("worktree-gc-{OTHER}");
    // Prevent a natural hourly slot between these baseline passes from
    // dispatching the selected routine before the due-state setup below.
    run_success(
        &fixture.selected_repo,
        &fixture.home,
        &["routine", "pause", &selected_routine],
    );
    let every_workspace = fixture.sweep(&["sweep", "--format", "json"]);
    runs.retain_sweep(&every_workspace);
    assert_eq!(
        baselined_sources(&every_workspace),
        BTreeSet::from([OTHER.to_string()]),
        "the filtered pass must not have recorded scheduler state for {OTHER}: {every_workspace}"
    );
    run_success(
        &fixture.selected_repo,
        &fixture.home,
        &["routine", "resume", &selected_routine],
    );

    // Both workspaces own a real, clean worktree eligible for the GC job.
    // Backdating cursors under a minutely fixture schedule makes both due
    // without waiting for a wall-clock slot or relying on host load.
    let (selected_tree, selected_task) =
        seed_eligible_worktree(&fixture.selected_repo, &fixture.home);
    let (other_tree, other_task) = seed_eligible_worktree(&fixture.other_repo, &fixture.home);
    runs.settle()
        .expect("seed workers exit before the due pass");
    zero_worktree_gc_age_floor(&fixture.home);
    for repo in [&fixture.selected_repo, &fixture.other_repo] {
        make_routine_due(repo, &fixture.home);
    }
    let selected_before = run_ids(&fixture.selected_repo, &fixture.home);
    let other_before = run_ids(&fixture.other_repo, &fixture.home);
    let other_cursor = routine_cursor(&fixture.home, &other_routine);

    let selected_only = fixture.sweep(&["--workspace", SELECTED, "sweep", "--format", "json"]);
    runs.retain_sweep(&selected_only);
    runs.settle().expect("all selected-pass workers exit");
    assert_eq!(
        report_sources(&selected_only),
        BTreeSet::from([SELECTED.to_string()]),
        "due selected pass must evaluate only the selected workspace: {selected_only}"
    );
    let selected_run = fired_run(&selected_only, &selected_routine);
    assert_eq!(
        run_ids(&fixture.selected_repo, &fixture.home),
        selected_before
            .into_iter()
            .chain([selected_run.to_string()])
            .collect(),
        "selected pass must record its dispatched run in the selected workspace"
    );
    assert_removed_worktree(
        &fixture.selected_repo,
        &fixture.home,
        selected_run,
        &selected_tree,
        &selected_task,
    );
    assert_eq!(
        run_ids(&fixture.other_repo, &fixture.home),
        other_before,
        "selected pass must admit no run in the unselected workspace"
    );
    assert_eq!(
        routine_cursor(&fixture.home, &other_routine),
        other_cursor,
        "selected pass must preserve the unselected routine's entire cursor"
    );
    assert!(other_tree.exists(), "unselected worktree must survive");

    // The negative control is eligible too: an unfiltered pass fires it and
    // removes its worktree. Settle any selected re-fire as well if the clock
    // crossed a minute between passes.
    let every_workspace = fixture.sweep(&["sweep", "--format", "json"]);
    runs.retain_sweep(&every_workspace);
    runs.settle().expect("all unfiltered-pass workers exit");
    assert_removed_worktree(
        &fixture.other_repo,
        &fixture.home,
        fired_run(&every_workspace, &other_routine),
        &other_tree,
        &other_task,
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

fn fired_run<'a>(outcome: &'a Value, routine: &str) -> &'a str {
    let report = reports(outcome)
        .iter()
        .find(|report| report["routine"] == routine)
        .unwrap_or_else(|| panic!("missing routine {routine}: {outcome}"));
    assert_eq!(report["action"], "fired", "due routine must fire: {report}");
    report["run_id"].as_str().expect("fired run id")
}

fn run_ids(repo: &Path, home: &Path) -> BTreeSet<String> {
    let history = run_json(repo, home, &["run", "history", "--no-reconcile", "--json"]);
    history["runs"]
        .as_array()
        .expect("run history")
        .iter()
        .map(|run| run["run_id"].as_str().expect("run id").to_string())
        .collect()
}

fn routine_cursor(home: &Path, routine: &str) -> (String, Option<String>, String) {
    let connection = rusqlite::Connection::open(home.join(".orbit/orbit.db"))
        .expect("open fixture scheduler store");
    connection
        .query_row(
            "SELECT baseline_at, last_slot, updated_at FROM routine_cursors WHERE routine_name = ?1",
            [routine],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("recorded routine cursor")
}

fn make_routine_due(repo: &Path, home: &Path) {
    let path = repo.join(".orbit/routines").join(ENABLED_ROUTINE);
    let mut definition: serde_yaml::Value =
        serde_yaml::from_slice(&fs::read(&path).expect("read routine")).expect("parse routine");
    let name = definition["name"]
        .as_str()
        .expect("routine name")
        .to_string();
    definition["trigger"]["cron"] = "* * * * *".into();
    definition["trigger"]["missed_run"] = "catch_up_once".into();
    fs::write(
        path,
        serde_yaml::to_string(&definition).expect("encode routine"),
    )
    .expect("write minutely fixture schedule");

    let connection = rusqlite::Connection::open(home.join(".orbit/orbit.db"))
        .expect("open fixture scheduler store");
    let baseline_at = (Utc::now() - ChronoDuration::minutes(2)).to_rfc3339();
    let updated = connection
        .execute(
            "UPDATE routine_cursors SET baseline_at = ?1, last_slot = NULL WHERE routine_name = ?2",
            rusqlite::params![baseline_at, name],
        )
        .expect("backdate fixture cursor");
    assert_eq!(updated, 1, "routine must have been baselined: {name}");
}

fn zero_worktree_gc_age_floor(home: &Path) {
    let path = home.join(".orbit/resources/jobs/worktree_gc_pipeline.yaml");
    let mut job: serde_yaml::Value =
        serde_yaml::from_slice(&fs::read(&path).expect("read GC job")).expect("parse GC job");
    job["spec"]["default_input"]["older_than_hours"] = 0.into();
    fs::write(path, serde_yaml::to_string(&job).expect("encode GC job"))
        .expect("write fixture age floor");
}

/// A terminal run naming an archived task and a clean worktree at the path
/// scheduled GC resolves for it, matching the real worktree setup contract.
fn seed_eligible_worktree(repo: &Path, home: &Path) -> (PathBuf, String) {
    let task = run_json(
        repo,
        home,
        &[
            "task",
            "add",
            "--title",
            "Sweep GC fixture",
            "--complexity",
            "low",
            "--json",
        ],
    );
    let task_id = task["id"].as_str().expect("fixture task id").to_string();
    run_success(repo, home, &["task", "archive", &task_id]);
    let job = repo.join("gc-fixture.yaml");
    fs::write(
        &job,
        r#"schemaVersion: 2
kind: Job
metadata:
  name: gc_fixture_pipeline
spec:
  state: enabled
  kind: workflow
  steps:
    - id: finish
      default_input:
        seconds: 0
      spec:
        type: deterministic
        action: sleep
        config: {}
"#,
    )
    .expect("write fixture job");
    let run = run_json(
        repo,
        home,
        &[
            "run",
            "job",
            job.to_str().expect("fixture job path"),
            "--input",
            &format!("task_id={task_id}"),
            "--input",
            "crew=sol",
            "--wait",
            "--json",
        ],
    );
    assert_eq!(run["state"], "success", "seed run must succeed: {run}");
    let run_id = run["run_id"].as_str().expect("seed run id");
    let tree = repo
        .join(".orbit/state/worktrees")
        .join(format!("orbit-{run_id}"));
    run_git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            "orbit/gc-fixture",
            tree.to_str().expect("worktree path"),
            "HEAD",
        ],
    );
    (tree.canonicalize().expect("eligible worktree"), task_id)
}

fn assert_removed_worktree(repo: &Path, home: &Path, run_id: &str, tree: &Path, task: &str) {
    let run = run_json(repo, home, &["run", "show", run_id, "--json"]);
    assert_eq!(
        run["run"]["state"], "success",
        "GC worker must succeed: {run}"
    );
    let reap = &run["pipeline_state"]["pipeline"]["reap"];
    assert!(
        reap["reports"]
            .as_array()
            .expect("GC reports")
            .iter()
            .any(|report| {
                report["action"] == "removed"
                    && report["task_id"] == task
                    && report["path"] == tree.to_string_lossy().as_ref()
            }),
        "worker must report removing its workspace's eligible tree: {reap}"
    );
    assert!(!tree.exists(), "worker must remove {}", tree.display());
}

fn isolated_live_sweep() -> bool {
    const NAME: &str = "sweep_workspace::live_sweep_fires_only_the_selected_workspace";
    const MARKER: &str = "ORBIT_LIVE_SWEEP_CHILD";
    if std::env::var_os(MARKER).is_some() {
        return true;
    }
    let temp = tempdir().expect("child output");
    let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"));
    test_env::clear_inherited_authority(|key| {
        child.env_remove(key);
    });
    child
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env(MARKER, "1");
    let output = test_env::run_child_test(&mut child, NAME, temp.path());
    test_env::assert_child_test_passed(NAME, output.status, &output.stdout, &output.stderr);
    false
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
        .env("USERPROFILE", home)
        .timeout(test_env::FIXTURE_STEP_DEADLINE);
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
    crate::git_repo::init(repo);
    run_git(repo, &["config", "user.name", "Orbit Test"]);
    run_git(repo, &["config", "user.email", "orbit-test@example.com"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "# sweep workspace test\n").expect("write readme");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "--quiet", "-m", "initial"]);
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = crate::git_repo::command()
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
