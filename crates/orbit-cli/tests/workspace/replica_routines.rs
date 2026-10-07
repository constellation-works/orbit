//! [ORB-14173] A replica checkout schedules its host-local worktree GC on the
//! host clock, and nothing else.
//!
//! The fixture host owns one owner checkout and one replica checkout of a
//! workspace owned by another machine. Both carry the seeded routines. The
//! replica's worktree GC, ship sweep and an auto-task are all enabled; only the
//! GC may fire there, and the GC run must retain a task whose prefix it cannot
//! route. The owner checkout's
//! scheduling is the control: its routines are evaluated exactly as before.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin_cmd;
use chrono::{Duration as ChronoDuration, Utc};
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

use crate::{fixture_crew, git_repo};

const OWNER: &str = "gc-owner";
const REPLICA: &str = "gc-replica";
const REMOTE_OWNER: &str = "hm_fixture_remote";

#[test]
fn replica_fires_only_its_worktree_gc_routine_and_owner_scheduling_is_unchanged() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let owner_repo = temp.path().join("owner");
    let replica_repo = temp.path().join("replica");
    fs::create_dir_all(&home).expect("create home");
    init_git_repo(&owner_repo);
    init_git_repo(&replica_repo);

    run_success(
        &owner_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--machine-name",
            "replica-routines-host",
            "--task-prefix",
            "RR",
        ],
    );
    fixture_crew::configure_sol(&home.join(".orbit"));
    run_success(&owner_repo, &home, &["workspace", "init", "--name", OWNER]);
    run_success(
        &replica_repo,
        &home,
        &[
            "workspace",
            "init",
            "--name",
            REPLICA,
            "--role",
            "replica",
            "--owner",
            REMOTE_OWNER,
        ],
    );

    // Every replica-side candidate is switched on, so only the replica rule
    // can keep the owner work from firing.
    enable_routine(&replica_repo, "worktree_gc.yaml");
    enable_routine(&replica_repo, "ship_sweep.yaml");
    enable_auto_task(&replica_repo, "qa-sweep.yaml");
    enable_routine(&owner_repo, "worktree_gc.yaml");
    zero_worktree_gc_age_floor(&home);
    let replica_worktree = seed_replica_worktree(&replica_repo, &home);

    let replica_gc = format!("worktree-gc-{REPLICA}");
    let replica_ship = format!("ship-sweep-{REPLICA}");
    let owner_gc = format!("worktree-gc-{OWNER}");
    let owner_ship = format!("ship-sweep-{OWNER}");

    // Discovery: the replica GC is listed beside the owner's routines; the
    // replica's ship sweep is listed as owner work with the owner named.
    let listed = run_json(&owner_repo, &home, &["routine", "list", "--json"]);
    let scheduled = names(&listed["routines"]);
    for name in [&replica_gc, &owner_gc, &owner_ship] {
        assert!(scheduled.contains(name), "{name} not listed: {listed}");
    }
    assert!(!scheduled.contains(&replica_ship), "{listed}");
    let owner_only = listed["owner_only"]
        .as_array()
        .expect("owner_only array")
        .iter()
        .find(|row| row["name"] == replica_ship.as_str())
        .unwrap_or_else(|| panic!("replica ship sweep not listed as owner-only: {listed}"));
    assert_eq!(owner_only["owner_machine"], REMOTE_OWNER, "{owner_only}");
    assert!(
        owner_only["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains(REMOTE_OWNER)),
        "the refusal names the owner: {owner_only}"
    );

    // An owner-only routine never fires here, so a pause is refused rather
    // than recorded as if it changed something.
    let paused = command(&owner_repo, &home)
        .args(["routine", "pause", &replica_ship])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    assert!(
        String::from_utf8_lossy(&paused).contains(REMOTE_OWNER),
        "pause refusal names the owner: {}",
        String::from_utf8_lossy(&paused)
    );
    run_success(&owner_repo, &home, &["routine", "pause", &replica_gc]);
    run_success(&owner_repo, &home, &["routine", "resume", &replica_gc]);

    // First observation baselines the cursors; then both GC routines are due.
    let baseline = sweep(&owner_repo, &home);
    assert_owner_work_refused(&baseline, &replica_ship);
    make_routine_due(&home, &replica_gc);
    make_routine_due(&home, &owner_gc);

    let fired = sweep(&owner_repo, &home);
    assert_owner_work_refused(&fired, &replica_ship);
    for name in [&replica_gc, &owner_gc] {
        let report = routine_report(&fired, name)
            .unwrap_or_else(|| panic!("{name} was not evaluated: {fired}"));
        assert!(
            matches!(report["action"].as_str(), Some("fired" | "retry_fired")),
            "{name} did not dispatch on its due pass: {report}"
        );
    }
    // Owner evaluation is unchanged: its disabled ship sweep is evaluated as
    // a disabled routine, not deferred to anyone.
    let owner_ship_row = routine_report(&fired, &owner_ship).expect("owner ship sweep row");
    assert_eq!(owner_ship_row["action"], "skipped", "{owner_ship_row}");
    assert_eq!(
        owner_ship_row["reason"], "disabled_in_definition",
        "{owner_ship_row}"
    );
    assert!(
        fired["auto_task_reports"]
            .as_array()
            .is_none_or(|rows| rows.iter().all(|row| row["source"] != REPLICA)),
        "a replica never evaluates or mints auto-tasks: {fired}"
    );

    // The replica GC run executes in the replica checkout and settles there.
    let gc_run_id = routine_report(&fired, &replica_gc).expect("replica GC row")["run_id"]
        .as_str()
        .expect("fired replica GC carries a run id")
        .to_string();
    let replica_orbit_dir = replica_repo
        .canonicalize()
        .expect("resolve replica checkout")
        .join(".orbit");
    let run = wait_for_terminal_run(&replica_repo, &home, &gc_run_id);
    assert_eq!(run["run"]["state"], "success", "{run}");
    assert_eq!(
        run["pipeline_state"]["initial_input"]["__routine_dispatch_orbit_dir"],
        replica_orbit_dir.to_string_lossy().as_ref(),
        "{run}"
    );
    // This task's foreign prefix has no owner admission or mirror here, so
    // GC retains it without querying an owner whose namespace it cannot verify.
    let reports = run["pipeline_state"]["pipeline"]["reap"]["reports"]
        .as_array()
        .unwrap_or_else(|| panic!("reap reports array: {run}"));
    let retained = reports
        .iter()
        .find(|report| report["task_id"] == "RM-00001")
        .unwrap_or_else(|| panic!("seeded replica worktree was not classified: {run}"));
    assert_eq!(
        retained["action"], "skipped:task_prefix_unroutable",
        "{retained}"
    );
    assert!(replica_worktree.exists(), "retained worktree was removed");
}

#[test]
fn replica_auto_task_definition_mutations_refuse_without_changing_local_state() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let owner_repo = temp.path().join("owner");
    let replica_repo = temp.path().join("replica");
    fs::create_dir_all(&home).expect("create home");
    init_git_repo(&owner_repo);
    init_git_repo(&replica_repo);

    run_success(
        &owner_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--machine-name",
            "replica-auto-task-host",
            "--task-prefix",
            "RA",
        ],
    );
    fixture_crew::configure_sol(&home.join(".orbit"));
    run_success(&owner_repo, &home, &["workspace", "init", "--name", OWNER]);
    run_success(
        &replica_repo,
        &home,
        &[
            "workspace",
            "init",
            "--name",
            REPLICA,
            "--role",
            "replica",
            "--owner",
            REMOTE_OWNER,
        ],
    );

    // Model a deleted shipped default so restore would write its definition
    // and managed-asset record if the Core authority check were missing.
    fs::remove_file(replica_repo.join(".orbit/auto_tasks/backlog-hygiene.yaml"))
        .expect("remove restore target in isolated fixture");
    let cursor_path = replica_repo.join(".orbit/state/auto-tasks.json");
    fs::create_dir_all(cursor_path.parent().expect("cursor parent"))
        .expect("create cursor directory");
    fs::write(
        &cursor_path,
        r#"{"definitions":{"doc-duties":{"baseline_at":"2026-10-05T00:00:00Z"}}}"#,
    )
    .expect("seed auto-task cursor");

    let listed = run_json(
        &replica_repo,
        &home,
        &["auto-task", "list", "--format", "json"],
    );
    assert!(listed.to_string().contains("qa-sweep"), "{listed}");
    let shown = run_json(
        &replica_repo,
        &home,
        &["auto-task", "show", "qa-sweep", "--json"],
    );
    assert_eq!(shown["name"], "qa-sweep");

    for args in [
        vec![
            "auto-task",
            "add",
            "--name",
            "replica-add-check",
            "--every-minutes",
            "60",
            "--title",
            "Must stay absent",
            "--json",
        ],
        vec![
            "auto-task",
            "update",
            "qa-sweep",
            "--description",
            "Must stay unchanged",
            "--json",
        ],
        vec!["auto-task", "toggle", "qa-sweep", "on", "--json"],
        vec!["auto-task", "delete", "doc-duties", "--json"],
        vec!["auto-task", "restore", "backlog-hygiene", "--json"],
        vec!["auto-task", "mint", "qa-sweep", "--json"],
    ] {
        let before = auto_task_state_snapshot(&replica_repo, &home);
        let output = command(&replica_repo, &home)
            .args(&args)
            .output()
            .expect("run replica auto-task mutation");
        assert!(
            !output.status.success(),
            "orbit {args:?} unexpectedly succeeded"
        );
        let diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            diagnostic.contains(REMOTE_OWNER),
            "refusal must name the owner for {args:?}: {diagnostic}"
        );
        assert_eq!(
            auto_task_state_snapshot(&replica_repo, &home),
            before,
            "refused orbit {args:?} must preserve definition bytes, cursor, managed assets, and task state"
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
struct AutoTaskStateSnapshot {
    definitions_and_assets: BTreeMap<String, Vec<u8>>,
    cursor: Option<Vec<u8>>,
    tasks: Value,
}

fn auto_task_state_snapshot(repo: &Path, home: &Path) -> AutoTaskStateSnapshot {
    let definitions_dir = repo.join(".orbit/auto_tasks");
    let definitions_and_assets = fs::read_dir(&definitions_dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", definitions_dir.display()))
        .map(|entry| {
            let entry = entry.expect("read auto-task entry");
            let bytes = fs::read(entry.path())
                .unwrap_or_else(|error| panic!("read {}: {error}", entry.path().display()));
            (entry.file_name().to_string_lossy().into_owned(), bytes)
        })
        .collect();
    let cursor = fs::read(repo.join(".orbit/state/auto-tasks.json")).ok();
    let tasks = run_json(repo, home, &["task", "list", "--json"]);
    AutoTaskStateSnapshot {
        definitions_and_assets,
        cursor,
        tasks,
    }
}

fn assert_owner_work_refused(outcome: &Value, routine: &str) {
    let report = routine_report(outcome, routine)
        .unwrap_or_else(|| panic!("{routine} has no sweep row: {outcome}"));
    assert_eq!(report["action"], "skipped", "{report}");
    assert!(
        report["reason"]
            .as_str()
            .is_some_and(|reason| reason.starts_with("owner_only_in_replica:")
                && reason.contains(REMOTE_OWNER)),
        "{report}"
    );
    assert!(report["run_id"].is_null(), "{report}");
}

fn names(rows: &Value) -> Vec<String> {
    rows.as_array()
        .expect("routine rows")
        .iter()
        .filter_map(|row| row["name"].as_str().map(str::to_owned))
        .collect()
}

/// A finished run in the replica checkout naming a task, and a real, clean
/// worktree at the path GC resolves for that run.
fn seed_replica_worktree(repo: &Path, home: &Path) -> PathBuf {
    let fixture_job = repo.join("gc-fixture.yaml");
    fs::write(
        &fixture_job,
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: gc_fixture_pipeline\nspec:\n  \
         state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        \
         seconds: 0\n      spec:\n        type: deterministic\n        action: sleep\n        \
         config: {}\n",
    )
    .expect("write fixture job");
    let run = run_json(
        repo,
        home,
        &[
            "run",
            "job",
            fixture_job.to_str().expect("utf-8 fixture path"),
            "--input",
            "task_id=RM-00001",
            "--input",
            "crew=sol",
            "--wait",
            "--json",
        ],
    );
    assert_eq!(run["state"], "success", "fixture run: {run}");
    let run_id = run["run_id"].as_str().expect("fixture run id");
    let worktree = repo
        .join(".orbit/state/worktrees")
        .join(format!("orbit-{run_id}"));
    run_git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            "orbit/gc-fixture",
            worktree.to_str().expect("utf-8 worktree path"),
            "HEAD",
        ],
    );
    worktree
}

fn enable_routine(repo: &Path, file: &str) {
    let path = repo.join(".orbit/routines").join(file);
    let content = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let mut updated = content.replace("enabled: false", "enabled: true");
    updated = updated
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("cron:") {
                "  cron: \"* * * * *\"".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&path, updated + "\n")
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn enable_auto_task(repo: &Path, file: &str) {
    let path = repo.join(".orbit/auto_tasks").join(file);
    let content = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    fs::write(
        &path,
        content.replacen("enabled: false", "enabled: true", 1),
    )
    .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

/// Drop the seeded job's age floor so a just-finished fixture run is old
/// enough to classify.
fn zero_worktree_gc_age_floor(home: &Path) {
    let path = home.join(".orbit/resources/jobs/worktree_gc_pipeline.yaml");
    let content = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let updated = content
        .lines()
        .map(|line| match line.split_once("older_than_hours:") {
            Some((indent, value)) if value.trim().parse::<u64>().is_ok() => {
                format!("{indent}older_than_hours: 0")
            }
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(updated.contains("older_than_hours: 0"), "{updated}");
    fs::write(&path, updated + "\n")
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn sweep(cwd: &Path, home: &Path) -> Value {
    run_json(cwd, home, &["sweep", "--format", "json"])
}

fn routine_report<'a>(outcome: &'a Value, routine: &str) -> Option<&'a Value> {
    outcome["reports"]
        .as_array()?
        .iter()
        .find(|report| report["routine"].as_str() == Some(routine))
}

fn make_routine_due(home: &Path, routine: &str) {
    let database = home.join(".orbit/orbit.db");
    let connection = rusqlite::Connection::open(&database)
        .unwrap_or_else(|error| panic!("open {}: {error}", database.display()));
    let baseline_at = (Utc::now() - ChronoDuration::minutes(1)).to_rfc3339();
    let updated = connection
        .execute(
            "UPDATE routine_cursors SET baseline_at = ?1, last_slot = NULL WHERE routine_name = ?2",
            rusqlite::params![baseline_at, routine],
        )
        .unwrap_or_else(|error| panic!("backdate cursor {routine}: {error}"));
    assert_eq!(updated, 1, "expected a recorded cursor for {routine}");
}

fn wait_for_terminal_run(repo: &Path, home: &Path, run_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let run = run_json(repo, home, &["run", "show", run_id, "--json"]);
        let terminal = matches!(
            run["run"]["state"].as_str(),
            Some("success" | "failed" | "cancelled")
        );
        if terminal || Instant::now() >= deadline {
            return run;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
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

fn init_git_repo(repo: &Path) {
    git_repo::init(repo);
    run_git(repo, &["config", "user.name", "Orbit Test"]);
    run_git(repo, &["config", "user.email", "orbit-test@example.com"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "# replica routines\n").expect("write readme");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "--quiet", "-m", "initial"]);
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
