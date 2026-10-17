#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! [ORB-16356] A terminal failed run parked in `someday` past
//! `worktree.parked_reclaim_after_hours` loses its whole worktree. A held
//! host-validation lease, a claim that still holds a drain slot, and a
//! backlog task keep theirs, and the dry run names why.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use assert_cmd::cargo::cargo_bin_cmd;
use chrono::{Duration as ChronoDuration, Utc};
use orbit_common::fs::file_lock::{FileLockGuard, FileLockOptions, acquire_exclusive_file_lock};
use orbit_common::security::release::sha256_hex;
use orbit_common::test_env;
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::fixture_crew;

struct Workspace {
    repo: PathBuf,
    orbit_dir: PathBuf,
}

#[test]
fn parked_age_reclaims_someday_and_keeps_lease_claim_and_backlog() {
    if crate::isolated_test::run_in_child(
        "parked_worktree_gc::parked_age_reclaims_someday_and_keeps_lease_claim_and_backlog",
    ) {
        return;
    }

    let temp = tempdir().unwrap();
    // GC reports canonical paths; macOS temp dirs sit behind the /var symlink.
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&repo).unwrap();
    crate::git_repo::SeededRepo::new(&repo)
        .readme("# parked worktree gc\n")
        .build();
    run_success(
        &repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--machine-name",
            "parked-gc-fixture",
            "--task-prefix",
            "PG",
        ],
    );
    fixture_crew::configure_sol(&home.join(".orbit"));
    let workspace = register_workspace(&repo, &home);
    run_success(
        &repo,
        &home,
        &[
            "config",
            "set",
            "--seed-from-global",
            "worktree.parked_reclaim_after_hours",
            "1",
        ],
    );

    let finished_at = Utc::now() - ChronoDuration::hours(2);
    let someday = seed_run(
        &workspace,
        &home,
        "someday",
        TaskStatus::Someday,
        false,
        finished_at,
    );
    let leased = seed_run(
        &workspace,
        &home,
        "leased",
        TaskStatus::Someday,
        false,
        finished_at,
    );
    let claimed = seed_run(
        &workspace,
        &home,
        "claimed",
        TaskStatus::Someday,
        true,
        finished_at,
    );
    let backlog = seed_run(
        &workspace,
        &home,
        "backlog",
        TaskStatus::Backlog,
        false,
        finished_at,
    );
    let _lease = hold_checkout_lease(&leased.worktree);

    let dry = run_json(&repo, &home, &["gc", "worktrees", "--json"]);
    assert_eq!(dry["dry_run"], true);
    assert_eq!(action_for(&dry, &someday.worktree), "would_remove", "{dry}");
    assert_eq!(
        action_for(&dry, &leased.worktree),
        "skipped:lease_held",
        "{dry}"
    );
    assert!(
        detail_for(&dry, &leased.worktree).contains("lease held by pid"),
        "{dry}"
    );
    assert_eq!(
        action_for(&dry, &claimed.worktree),
        "skipped:live_claim",
        "{dry}"
    );
    assert!(
        detail_for(&dry, &claimed.worktree).contains("holds a drain slot"),
        "{dry}"
    );
    assert_eq!(
        action_for(&dry, &backlog.worktree),
        "skipped:task_status_ineligible",
        "{dry}"
    );
    assert!(someday.worktree.exists());

    let removed = run_json(
        &repo,
        &home,
        &[
            "gc",
            "worktrees",
            "--run",
            &someday.run_id,
            "--confirm",
            "--json",
        ],
    );
    assert_eq!(removed["dry_run"], false);
    assert_eq!(
        action_for(&removed, &someday.worktree),
        "removed",
        "{removed}"
    );
    assert!(
        !someday.worktree.exists(),
        "the someday checkout must be gone"
    );
    assert!(leased.worktree.exists());
    assert!(claimed.worktree.exists());
    assert!(backlog.worktree.exists());
}

struct Seeded {
    worktree: PathBuf,
    run_id: String,
}

fn seed_run(
    workspace: &Workspace,
    home: &Path,
    label: &str,
    status: TaskStatus,
    live_claim: bool,
    finished_at: chrono::DateTime<Utc>,
) -> Seeded {
    use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
    use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
    use orbit_core::runtime::OrbitRuntimeRoots;
    use orbit_engine::RuntimeHost;
    use orbit_types::workflow::JobRunState;

    let runtime = RegisteredRuntimeFactory::open_resolved_roots(OrbitRuntimeRoots {
        global_root: home.join(".orbit"),
        shared_root: workspace.orbit_dir.clone(),
        local_root: workspace.orbit_dir.clone(),
    })
    .expect("registered GC fixture runtime");
    let task = runtime
        .add_task(TaskAddParams {
            title: format!("parked GC fixture {label}"),
            complexity: orbit_types::task::TaskComplexity::Low,
            ..Default::default()
        })
        .expect("GC fixture task");
    runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                status: Some(status),
                ..Default::default()
            },
            "parked-gc-fixture".to_string(),
        )
        .expect("park the fixture task");
    let task_id = task.id.to_string();
    let run = runtime
        .insert_job_run(
            "gc_fixture_pipeline",
            1,
            finished_at,
            Some(json!({"task_id": task_id, "crew": "sol"})),
            None,
        )
        .expect("GC fixture run");
    let mut owner_command = std::process::Command::new("/bin/sh");
    test_env::clear_inherited_authority(|name| {
        owner_command.env_remove(name);
    });
    let mut owner = crate::child_guard::ChildGuard::new(
        owner_command
            .args(["-c", "read -r release; exit 0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("GC fixture owner"),
    );
    assert!(
        runtime
            .mark_job_run_running(&run.run_id, finished_at, owner.id())
            .expect("start GC fixture run")
            .owns_execution()
    );
    assert!(
        runtime
            .finalize_job_run(&run.run_id, JobRunState::Failed, finished_at, Some(0))
            .expect("finish GC fixture run")
    );
    if live_claim {
        insert_live_claim(&runtime, &run.run_id);
    }
    drop(owner.stdin.take());
    let mut status = None;
    test_env::wait_until("GC fixture owner to exit", || {
        status = owner.try_wait().expect("poll GC fixture owner");
        status.is_some()
    });
    assert!(status.expect("GC fixture owner exited").success());
    let run_id = run.run_id;
    drop(runtime);

    let worktree = workspace
        .repo
        .join(".orbit/state/worktrees")
        .join(format!("orbit-{run_id}"));
    let branch = format!("orbit/parked-gc-{label}");
    crate::git_repo::run(
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
    fs::write(worktree.join("fixture.txt"), label).unwrap();
    crate::git_repo::run(&worktree, &["add", "fixture.txt"]);
    crate::git_repo::run(&worktree, &["commit", "-m", "parked gc fixture"]);
    Seeded { worktree, run_id }
}

fn insert_live_claim(runtime: &orbit_core::OrbitRuntime, run_id: &str) {
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let record = json!({
        "destination": {
            "owner_machine_id": "fixture-owner",
            "owner_workspace_id": "owner-workspace",
            "selector": "fixture-owner/owner-workspace",
            "execution_machine_id": "fixture-host"
        },
        "request": {
            "request_id": format!("parked-{run_id}"),
            "caller_version": "fixture",
            "caller_schema": 1,
            "caller_before_pr": false,
            "run_context": {
                "run_id": "parent-run",
                "job_name": "workspace_auto_pipeline",
                "machine_name": null
            },
            "ship": {
                "mode": "local",
                "base_branch": "main",
                "landing_branch": "main",
                "before_pr": false,
                "completion": "review",
                "authorization_reference": null
            }
        },
        "receipt": null,
        "leaf_run_id": run_id,
        "phase": "claimed",
        "settlement": null,
        "refusal": null
    });
    let store = runtime.sqlite_store().expect("sqlite store");
    let connection = store.connection();
    let connection = connection.lock().expect("sqlite connection");
    connection
        .execute(
            "INSERT INTO local_pull_admissions (
                workspace_id, owner_machine, owner_workspace, execution_machine,
                request_id, claim_id, leaf_run_id, record_json
             ) VALUES (?1, 'fixture-owner', 'owner-workspace', 'fixture-host', ?2, NULL, ?3, ?4)",
            rusqlite::params![
                workspace_id,
                format!("parked-{run_id}"),
                run_id,
                record.to_string()
            ],
        )
        .expect("record a live claim");
}

fn hold_checkout_lease(worktree: &Path) -> FileLockGuard {
    let source = worktree.canonicalize().expect("canonical worktree");
    let common = orbit_common::fs::git::git_common_dir(&source).expect("git common dir");
    let primary = common
        .parent()
        .filter(|_| common.file_name().is_some_and(|name| name == ".git"))
        .expect("primary checkout")
        .to_path_buf();
    let key = sha256_hex(source.as_os_str().as_encoded_bytes());
    let directory = primary.join(".orbit/state/host-validation-leases");
    fs::create_dir_all(&directory).unwrap();
    let record = json!({
        "schema_version": 1,
        "kind": "host_evidence",
        "source": source,
    });
    fs::write(
        directory.join(format!("{key}.json")),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    acquire_exclusive_file_lock(
        &directory.join(format!("{key}.lock")),
        "host validation target lease",
        FileLockOptions {
            timeout: Duration::from_secs(5),
            ..FileLockOptions::default()
        },
    )
    .expect("hold the checkout lease")
}

fn action_for(gc: &Value, path: &Path) -> String {
    report_for(gc, path)["action"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn detail_for(gc: &Value, path: &Path) -> String {
    report_for(gc, path)["detail"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn report_for<'a>(gc: &'a Value, path: &Path) -> &'a Value {
    let wanted = path.to_str().expect("utf-8 path");
    gc["reports"]
        .as_array()
        .expect("reports")
        .iter()
        .find(|report| report["path"].as_str() == Some(wanted))
        .unwrap_or_else(|| panic!("no report for {wanted} in {gc}"))
}

fn register_workspace(repo: &Path, home: &Path) -> Workspace {
    let output = command(repo, home)
        .args(["workspace", "init", "--name", "parked-gc"])
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

fn run_success(cwd: &Path, home: &Path, args: &[&str]) {
    command(cwd, home).args(args).assert().success();
}

fn run_json(cwd: &Path, home: &Path, args: &[&str]) -> Value {
    let output = command(cwd, home)
        .env("ORBIT_OPERATOR", "1")
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
