#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! [ORB-11998] Regression coverage for scheduled worktree GC losing its
//! workspace context.
//!
//! Two independent bugs combined to produce the incident's
//! `success, dry_run:false, bytes_reclaimed:0, reports:[]` signature:
//!
//! 1. **Workspace routing.** The routine sweep correctly identifies which
//!    registered workspace owns a due routine and submits its job against
//!    that workspace's own runtime — but the *detached worker subprocess*
//!    that actually executes the job re-resolves its own workspace from
//!    scratch (cwd walk-up), and an `ORBIT_ROOT` value inherited from the
//!    sweep process's own environment outranks that cwd. A stale/ambient
//!    `ORBIT_ROOT` therefore silently redirects every routine-dispatched
//!    worker to one fixed (and usually irrelevant) root, regardless of which
//!    workspace's routine fired.
//! 2. **A field-name collision**, independent of (1) and sufficient on its
//!    own to reproduce the empty-report symptom: the engine's step
//!    dispatcher auto-injects `run_id` (the *dispatching* run's own id) into
//!    every step input that doesn't already declare one
//!    (`activity_job::dispatcher::inject_run_id`). The `worktree_gc`
//!    activity's own input schema also named its "restrict to one historical
//!    run" scoping field `run_id`, so every real job/routine-dispatched run
//!    got that scope silently clobbered with its own id — a run that never
//!    has a worktree of its own — and reaped nothing, every time, regardless
//!    of workspace correctness. Renamed to `target_run_id`.
//!
//! This test builds two independently registered workspaces sharing one
//! global root, each with a real, eligible, terminal-task worktree, then
//! fires workspace B's `worktree_gc_pipeline` routine through `orbit sweep`
//! while `ORBIT_ROOT` in the sweep process's own environment points at a
//! third, unrelated (but validly initialized) root — exactly reproducing the
//! incident's ambient-environment shape. It asserts B's eligible worktree is
//! actually reclaimed and A's is left untouched, proving the dispatch
//! reaches worktree collection with an explicit, correct workspace identity
//! rather than falling back to cwd-losing environment state or another
//! registered workspace.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

struct Workspace {
    repo: PathBuf,
    orbit_dir: PathBuf,
}

#[test]
fn routine_dispatch_ignores_ambient_orbit_root_and_reaps_only_the_owning_workspaces_worktree() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let repo_a = temp.path().join("workspace-a");
    let repo_b = temp.path().join("workspace-b");
    let decoy_root = temp.path().join("decoy-root");
    fs::create_dir_all(&home).expect("create isolated home");
    fs::create_dir_all(&repo_a).expect("create repo a");
    fs::create_dir_all(&repo_b).expect("create repo b");
    init_git_repo(&repo_a);
    init_git_repo(&repo_b);

    // Host identity lives in the default global root ($HOME/.orbit) — no
    // --root/ORBIT_ROOT override, matching the real split-root production
    // layout the incident happened in.
    run_success(
        &repo_a,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "gc-routing-host",
            "--task-prefix",
            "WG",
        ],
    );
    configure_fixture_crew(&home.join(".orbit"));
    let workspace_a = register_workspace(&repo_a, &home, "workspace-a");
    let workspace_b = register_workspace(&repo_b, &home, "workspace-b");

    // A third, independently initialized (but otherwise irrelevant) pinned
    // root: self-consistent enough that a worker mistakenly pinned to it
    // opens successfully — it just owns none of this test's workspaces or
    // job runs. This is what an ambient ORBIT_ROOT looks like in practice: a
    // valid root, just not the one the firing routine belongs to.
    run_success(
        &repo_a,
        &home,
        &[
            "--root",
            &decoy_root.to_string_lossy(),
            "init",
            "--non-interactive",
            "--host-name",
            "decoy-host",
            "--task-prefix",
            "DC",
        ],
    );

    let (worktree_a, _task_a) = seed_eligible_worktree(&workspace_a, &home, "A");
    let (worktree_b, task_b) = seed_eligible_worktree(&workspace_b, &home, "B");
    assert!(worktree_a.exists(), "workspace A fixture worktree missing");
    assert!(worktree_b.exists(), "workspace B fixture worktree missing");

    // The routine's own job asset defaults to a 24h `older_than_hours`
    // floor; drop it so a freshly finished fixture run is eligible without
    // needing to fabricate a backdated run record.
    zero_worktree_gc_age_floor(&home);
    enable_worktree_gc_routine(&workspace_b);

    // Fire the sweep from workspace A's checkout — the "non-current"
    // workspace relative to B, whose routine is the one that must dispatch.
    //
    // `--root` pins sweep's *own* discovery to the real registry (it takes
    // precedence over `ORBIT_ROOT`, so sweep still correctly finds and fires
    // workspace B's routine) — but the detached worker sweep spawns for that
    // routine receives no `--root` of its own (its parent runtime is not
    // pinned: global root != workspace B's `.orbit`), so it falls through to
    // whatever `ORBIT_ROOT` it inherited. This is exactly how the incident
    // reproduced in practice: an operator/service passes `--root` (or relies
    // on `$HOME`) for the sweep invocation itself while an unrelated,
    // ambient `ORBIT_ROOT` sits in the same process environment.
    let global_root = home.join(".orbit");
    let fire_sweep = || -> Value {
        let mut sweep = command(&repo_a, &home);
        sweep.env("ORBIT_ROOT", &decoy_root);
        let output = sweep
            .args([
                "sweep",
                "--root",
                global_root.to_str().expect("global root is utf-8"),
                "--format",
                "json",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap_or_else(|error| {
            panic!(
                "parse sweep JSON: {error}\nstdout:\n{}",
                String::from_utf8_lossy(&output)
            )
        })
    };

    // The routine has never been observed before, so this first pass only
    // records a baseline cursor (avoiding a catch-up burst of fires) rather
    // than dispatching. Cross into the next whole minute — `* * * * *`'s
    // finest granularity — before sweeping again, which is when the cursor
    // makes it due.
    let baseline = fire_sweep();
    assert!(
        routine_report(&baseline, "worktree-gc-workspace-b").is_some(),
        "sweep never evaluated workspace B's worktree_gc routine: {baseline}"
    );

    sleep_past_next_minute_boundary();

    let fired = fire_sweep();
    let report = routine_report(&fired, "worktree-gc-workspace-b").unwrap_or_else(|| {
        panic!(
            "sweep never evaluated workspace B's worktree_gc routine on the second pass: {fired}"
        )
    });
    assert!(
        matches!(
            report["action"].as_str(),
            Some("fired") | Some("retry_fired")
        ),
        "workspace B's worktree_gc routine did not dispatch on its due pass: {report}"
    );

    let gc_run_id = report["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("fired report carried no run_id: {report}"))
        .to_string();

    wait_until(Duration::from_secs(20), || !worktree_b.exists());

    assert!(
        !worktree_b.exists(),
        "workspace B's eligible worktree was not reclaimed by its own routine dispatch \
         (ambient ORBIT_ROOT silently redirected the worker to a different workspace)"
    );
    assert!(
        worktree_a.exists(),
        "workspace A's worktree was removed by workspace B's routine dispatch — \
         cross-workspace deletion"
    );

    // Confirm the reclamation is attributed correctly, not just that the
    // directory happened to disappear: the dispatched run's own workspace
    // identity must match workspace B, and its activity output must name
    // exactly the reclaimed worktree with a real `bytes_reclaimed` total.
    let run_show = run_json(
        &workspace_b.repo,
        &home,
        &["run", "show", &gc_run_id, "--json"],
    );
    assert_eq!(run_show["run"]["state"], "success");
    assert_eq!(
        run_show["pipeline_state"]["initial_input"]["__routine_dispatch_orbit_dir"],
        workspace_b.orbit_dir.to_string_lossy().as_ref(),
    );
    let reap = &run_show["pipeline_state"]["pipeline"]["reap"];
    assert!(reap["bytes_reclaimed"].as_u64().unwrap_or(0) > 0, "{reap}");
    let reports = reap["reports"].as_array().expect("reap reports array");
    assert!(
        reports.iter().any(|report| {
            report["action"] == "removed"
                && report["task_id"] == task_b.as_str()
                && report["path"].as_str()
                    == Some(worktree_b.to_str().expect("worktree path is utf-8"))
        }),
        "reap output did not attribute the removal to workspace B's fixture worktree: {reap}"
    );
}

fn register_workspace(repo: &Path, home: &Path, name: &str) -> Workspace {
    // `workspace init` has no `--json` flag; read its `orbit_dir:` summary line.
    let output = command(repo, home)
        .args(["workspace", "init", "--name", name])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&output);
    let orbit_dir = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("orbit_dir:"))
        .unwrap_or_else(|| panic!("workspace init reported no orbit_dir line:\n{text}"))
        .trim();
    Workspace {
        repo: repo.to_path_buf(),
        orbit_dir: PathBuf::from(orbit_dir),
    }
}

/// Create a task settled to `archived` (one of the three GC-eligible terminal
/// statuses), a job run naming it via the legacy singular `task_id` input
/// field, and a real, clean git worktree at the exact path GC resolves for
/// that run — the same shape `setup_worktree` produces for a live task.
fn seed_eligible_worktree(workspace: &Workspace, home: &Path, label: &str) -> (PathBuf, String) {
    let task = run_json(
        &workspace.repo,
        home,
        &[
            "task",
            "add",
            "--title",
            &format!("GC fixture task {label}"),
            "--complexity",
            "low",
            "--json",
        ],
    );
    let task_id = task["id"]
        .as_str()
        .unwrap_or_else(|| panic!("task add reported no id: {task}"))
        .to_string();
    run_success(&workspace.repo, home, &["task", "archive", &task_id]);

    let fixture_job = workspace.repo.join(format!("gc-fixture-{label}.yaml"));
    fs::write(
        &fixture_job,
        r#"schemaVersion: 2
kind: Job
metadata:
  name: gc_fixture_pipeline
spec:
  state: enabled
  kind: workflow
  steps:
    - id: nap
      default_input:
        seconds: 0
      spec:
        type: deterministic
        action: sleep
        config: {}
"#,
    )
    .expect("write fixture job asset");
    let run = run_json(
        &workspace.repo,
        home,
        &[
            "run",
            "job",
            fixture_job.to_str().expect("fixture job path is utf-8"),
            "--input",
            &format!("task_id={task_id}"),
            // The isolated CI environment has no detected agent CLI, so its
            // workspace config intentionally has no default crew. Select a
            // built-in configured crew explicitly for this deterministic job.
            "--input",
            "crew=sol",
            "--wait",
            "--json",
        ],
    );
    assert_eq!(
        run["state"], "succeeded",
        "fixture job run did not settle: {run}"
    );
    let run_id = run["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("run job reported no run_id: {run}"))
        .to_string();

    let worktree = workspace
        .repo
        .join(".orbit/state/worktrees")
        .join(format!("orbit-{run_id}"));
    let branch = format!("orbit/gc-fixture-{label}");
    run_git(
        &workspace.repo,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            worktree.to_str().expect("worktree path is utf-8"),
            "HEAD",
        ],
    );
    fs::write(worktree.join("fixture.txt"), label).expect("write worktree fixture file");
    run_git(&worktree, &["add", "fixture.txt"]);
    run_git(&worktree, &["commit", "-m", "gc fixture content"]);

    (worktree, task_id)
}

fn zero_worktree_gc_age_floor(home: &Path) {
    let path = home.join(".orbit/resources/jobs/worktree_gc_pipeline.yaml");
    let content = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let updated = content.replace("older_than_hours: 24", "older_than_hours: 0");
    assert_ne!(
        content,
        updated,
        "expected an `older_than_hours: 24` default in {}",
        path.display()
    );
    fs::write(&path, updated).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn configure_fixture_crew(root: &Path) {
    let path = root.join("config.toml");
    let mut config = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));

    if !config
        .lines()
        .any(|line| line.trim_start().starts_with("default_crew ="))
    {
        if let Some(workflow) = config.find("[workflow]\n") {
            config.insert_str(workflow + "[workflow]\n".len(), "default_crew = \"sol\"\n");
        } else {
            config.push_str("\n[workflow]\ndefault_crew = \"sol\"\n");
        }
    }

    if !config.contains("[crews.sol]") {
        config.push_str(
            "\n[crews.sol]\nprovider = \"codex\"\nmodel = \"gpt-5.6-sol\"\nbackend = \"cli\"\n",
        );
    }

    fs::write(&path, config).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn enable_worktree_gc_routine(workspace: &Workspace) {
    let path = workspace.orbit_dir.join("routines/worktree_gc.yaml");
    let content = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let updated = content
        .replace("enabled: false", "enabled: true")
        .replace("cron: \"35 * * * *\"", "cron: \"* * * * *\"");
    fs::write(&path, updated).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));

    let config_path = workspace.orbit_dir.join("config.toml");
    let mut config = fs::read_to_string(&config_path).unwrap_or_default();
    config.push_str("\n[routines]\nrole = \"source\"\n");
    fs::write(&config_path, config).expect("enable routine source role");
}

fn routine_report<'a>(sweep_report: &'a Value, routine_name: &str) -> Option<&'a Value> {
    sweep_report["reports"]
        .as_array()?
        .iter()
        .find(|report| report["routine"].as_str() == Some(routine_name))
}

fn sleep_past_next_minute_boundary() {
    use chrono::{Timelike, Utc};
    let now = Utc::now();
    let remaining = 60 - now.second();
    std::thread::sleep(Duration::from_secs(u64::from(remaining) + 2));
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return;
        }
        if Instant::now() >= deadline {
            return;
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
    run_git(repo, &["init", "--quiet"]);
    run_git(repo, &["config", "user.name", "Orbit Test"]);
    run_git(repo, &["config", "user.email", "orbit-test@example.com"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "# worktree gc routing test\n").expect("write readme");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "--quiet", "-m", "initial"]);
}

fn run_git(cwd: &Path, args: &[&str]) -> String {
    let output = StdCommand::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed in {}:\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
