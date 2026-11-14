//! A transport outage before `worktree_setup` holds the run while all tasks
//! remain in backlog. For an already-in-progress task, the run fails through
//! ordinary failure handling instead of leaving it in an unresumable hold.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{JobRunState, OrbitRuntime, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::workflow::{DeliveryRefusal, PipelineState};
use serde_json::json;

use super::dispatch_admission::{PressureProbe, isolated};

const JOB: &str = "origin_fetch_fixture";
const ORIGIN_UNAVAILABLE_CODE: &str = "delivery_origin_unavailable";

#[test]
fn worktree_setup_origin_outage_holds_backlog_and_fails_in_progress() {
    if !isolated(
        "origin_fetch_hold::worktree_setup_origin_outage_holds_backlog_and_fails_in_progress",
    ) {
        return;
    }

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    prepare_repository(&repo, root.path());

    let mode = root.path().join("upload-pack-mode");
    std::fs::write(&mode, "offline").unwrap();
    let upload_pack = root.path().join("upload-pack");
    let origin = root.path().join("origin.git");
    write_upload_pack(&upload_pack, &mode, &origin);
    git(
        &repo,
        &[
            "config",
            "remote.origin.uploadpack",
            upload_pack.to_str().unwrap(),
        ],
    );

    let jobs = global.join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    write_setup_job(&jobs.join(format!("{JOB}.yaml")));
    let runtime = Arc::new(
        OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
            .unwrap()
            .with_host_resource_probe(PressureProbe::calm()),
    );

    let admitted: Vec<_> = (0..3)
        .map(|index| {
            let task = add_backlog_task(&runtime, index);
            let run = submit_setup_run(&runtime, &task, &format!("outage-{index}"), "agent-main");
            (task, run)
        })
        .collect();

    thread::scope(|scope| {
        let workers: Vec<_> = admitted
            .iter()
            .map(|(_, run_id)| {
                let runtime = Arc::clone(&runtime);
                let run_id = run_id.clone();
                scope.spawn(move || {
                    let _ = runtime.execute_pipeline_run_worker(&run_id);
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });

    for (task_id, run_id) in &admitted {
        assert_origin_hold(&runtime, task_id, run_id);
    }

    let held_history = runtime
        .list_job_runs(JobRunListParams {
            state: Some(JobRunState::Held),
            ..Default::default()
        })
        .unwrap();
    assert!(
        admitted
            .iter()
            .all(|(_, run_id)| held_history.iter().any(|row| row.run_id == *run_id))
    );

    let child = json!({"status": "held", "run_id": admitted[0].1});
    let parent = runtime
        .run_deterministic(
            "pipeline_success_guard",
            &json!({}),
            &json!({"result": child}),
            ToolContext::default(),
        )
        .expect("a held child passes its parent's success guard");
    assert_eq!(parent["succeeded"], true, "{parent}");

    std::fs::write(&mode, "available").unwrap();
    let retry = submit_setup_run(&runtime, &admitted[0].0, "recovered", "agent-main");
    let _ = runtime.execute_pipeline_run_worker(&retry);
    assert_eq!(
        runtime.show_job_run(&retry).unwrap().state,
        JobRunState::Success
    );
    assert_eq!(
        runtime.get_task(&admitted[0].0).unwrap().status,
        TaskStatus::InProgress
    );

    std::fs::write(&mode, "offline").unwrap();
    let in_progress_run =
        submit_setup_run(&runtime, &admitted[0].0, "in-progress-outage", "agent-main");
    let _ = runtime.execute_pipeline_run_worker(&in_progress_run);
    let in_progress_outage = runtime.show_job_run(&in_progress_run).unwrap();
    assert_eq!(
        in_progress_outage.state,
        JobRunState::Failed,
        "{:?}",
        in_progress_outage.steps
    );
    assert_ne!(
        in_progress_outage.steps[0].error_code.as_deref(),
        Some(ORIGIN_UNAVAILABLE_CODE)
    );
    assert_eq!(
        runtime.get_task(&admitted[0].0).unwrap().status,
        TaskStatus::Blocked
    );

    std::fs::write(&mode, "auth").unwrap();
    let auth_task = add_backlog_task(&runtime, 3);
    let auth_run = submit_setup_run(&runtime, &auth_task, "auth-refused", "agent-main");
    let _ = runtime.execute_pipeline_run_worker(&auth_run);
    let auth = runtime.show_job_run(&auth_run).unwrap();
    assert_eq!(auth.state, JobRunState::Failed, "{:?}", auth.steps);
    assert_ne!(
        auth.steps[0].error_code.as_deref(),
        Some(ORIGIN_UNAVAILABLE_CODE)
    );
    assert_eq!(
        runtime.get_task(&auth_task).unwrap().status,
        TaskStatus::Backlog
    );

    std::fs::write(&mode, "available").unwrap();
    git(&origin, &["update-ref", "-d", "refs/heads/agent-main"]);
    let missing_task = add_backlog_task(&runtime, 4);
    let missing_run = submit_setup_run(&runtime, &missing_task, "missing-base", "agent-main");
    let _ = runtime.execute_pipeline_run_worker(&missing_run);
    let missing = runtime.show_job_run(&missing_run).unwrap();
    assert_eq!(missing.state, JobRunState::Failed, "{:?}", missing.steps);
    assert_ne!(
        missing.steps[0].error_code.as_deref(),
        Some(ORIGIN_UNAVAILABLE_CODE)
    );
    assert_eq!(
        runtime.get_task(&missing_task).unwrap().status,
        TaskStatus::Backlog
    );

    // The lock-wait row stays on this path. Short test-support limits replace
    // the production hold and lock wait. The upload-pack sleeps past that
    // wait, then fails the transport, so the origin probe confirms the outage
    // without the production probe deadline.
    std::fs::write(&mode, "hang").unwrap();
    let hang: Vec<_> = [5_usize, 6]
        .into_iter()
        .map(|index| {
            let task = add_backlog_task(&runtime, index);
            let run = submit_setup_run(&runtime, &task, &format!("hang-{index}"), "agent-main");
            (task, run)
        })
        .collect();
    let (holder_task, holder_run) = hang[0].clone();
    let (waiter_task, waiter_run) = hang[1].clone();
    thread::scope(|scope| {
        let holder_runtime = Arc::clone(&runtime);
        let holder_id = holder_run.clone();
        let holder = scope.spawn(move || run_with_short_fetch_limits(&holder_runtime, &holder_id));
        let give_up = Instant::now() + Duration::from_secs(5);
        while !fetch_lock_is_contended(&repo) {
            assert!(
                Instant::now() < give_up,
                "the fetch lock was never contended"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let waiter_runtime = Arc::clone(&runtime);
        let waiter_id = waiter_run.clone();
        let waiter = scope.spawn(move || run_with_short_fetch_limits(&waiter_runtime, &waiter_id));
        holder.join().unwrap();
        waiter.join().unwrap();
    });
    let mut saw_fetch_lock_waiter = false;
    for (task_id, run_id) in [(holder_task, holder_run), (waiter_task, waiter_run)] {
        let run = assert_origin_hold(&runtime, &task_id, &run_id);
        saw_fetch_lock_waiter |= run.steps[0]
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("acquiring git fetch lock"));
    }
    assert!(
        saw_fetch_lock_waiter,
        "the lock waiter times out and the origin probe confirms the outage"
    );

    assert_eq!(
        DeliveryRefusal::from_error_code(ORIGIN_UNAVAILABLE_CODE),
        Some(DeliveryRefusal::OriginUnavailable)
    );
    assert_eq!(
        DeliveryRefusal::OriginUnavailable.error_code(),
        ORIGIN_UNAVAILABLE_CODE
    );
}

fn prepare_repository(repo: &Path, root: &Path) {
    git(repo, &["init", "-q"]);
    git(repo, &["checkout", "-q", "-b", "agent-main"]);
    git(repo, &["config", "user.name", "Orbit Test"]);
    git(
        repo,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(repo, &["add", ".gitignore", "base.txt"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    let origin = root.join("origin.git");
    git(root, &["init", "--bare", "-q", origin.to_str().unwrap()]);
    git(repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(repo, &["push", "-q", "-u", "origin", "agent-main"]);
}

fn write_upload_pack(script: &Path, mode: &Path, origin: &Path) {
    std::fs::write(
        script,
        format!(
            "#!/bin/sh\ncase \"$(cat '{}')\" in\nhang) sleep 0.8; echo 'fatal: unable to access origin: Could not resolve host' >&2; exit 1 ;;\noffline) echo 'fatal: unable to access origin: Could not resolve host' >&2; exit 1 ;;\nauth) echo 'Permission denied (publickey). Connection closed' >&2; exit 1 ;;\n*) exec git-upload-pack '{}' ;;\nesac\n",
            mode.display(),
            origin.display()
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(script, permissions).unwrap();
}

fn write_setup_job(path: &Path) {
    std::fs::write(
        path,
        json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": JOB},
            "spec": {
                "state": "enabled",
                "max_active_runs": 0,
                "steps": [{
                    "id": "worktree",
                    "spec": {
                        "type": "deterministic",
                        "action": "worktree_setup",
                        "config": {}
                    },
                    "default_input": {
                        "task_ids": "{{ input.task_ids }}",
                        "run_id": "{{ input.run_id }}",
                        "base": "{{ input.base }}",
                        "base_sync": "{{ input.base_sync }}"
                    }
                }]
            }
        })
        .to_string(),
    )
    .unwrap();
}

fn add_backlog_task(runtime: &OrbitRuntime, index: usize) -> String {
    runtime
        .add_task(TaskAddParams {
            title: format!("Origin outage fixture {index}"),
            description: "A task admitted through worktree setup.".into(),
            plan: "1. Set up the task worktree.".into(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string()
}

fn submit_setup_run(runtime: &OrbitRuntime, task_id: &str, run_id: &str, base: &str) -> String {
    let input = json!({
        "task_ids": [task_id],
        "run_id": run_id,
        "base": base,
        "base_sync": "remote"
    });
    let run = runtime
        .insert_job_run(JOB, 1, Utc::now(), Some(input.clone()), None)
        .unwrap();
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, input),
        )
        .unwrap();
    run.run_id
}

fn assert_origin_hold(
    runtime: &OrbitRuntime,
    task_id: &str,
    run_id: &str,
) -> orbit_types::workflow::JobRun {
    let run = runtime.show_job_run(run_id).unwrap();
    assert_eq!(run.state, JobRunState::Held, "{run_id}: {:?}", run.steps);
    assert_eq!(
        run.steps[0].error_code.as_deref(),
        Some(ORIGIN_UNAVAILABLE_CODE),
        "{run_id}: {:?}",
        run.steps[0]
    );
    assert_eq!(
        runtime.get_task(task_id).unwrap().status,
        TaskStatus::Backlog
    );
    run
}

/// Hold and lock-wait short enough that the 0.8 s upload-pack outlasts the
/// wait and still finishes inside the hold. Production defaults stay in place
/// wherever this is not installed.
fn run_with_short_fetch_limits(runtime: &OrbitRuntime, run_id: &str) {
    const HOLD_LIMIT: Duration = Duration::from_millis(2_500);
    const LOCK_WAIT: Duration = Duration::from_millis(200);
    orbit_engine::test_support::with_worktree_setup_fetch_limits(HOLD_LIMIT, LOCK_WAIT, || {
        let _ = runtime.execute_pipeline_run_worker(run_id);
    });
}

fn fetch_lock_is_contended(repo: &Path) -> bool {
    let common = orbit_common::fs::git::git_common_dir(repo).unwrap();
    let lock_path = orbit_common::fs::git::git_fetch_lock_target(&common)
        .with_file_name(".orbit-git-fetch.lock");
    orbit_common::fs::io::try_acquire_exclusive_file_lock(&lock_path, "fixture contention probe")
        .unwrap()
        .is_none()
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
