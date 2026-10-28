//! A transport outage before `worktree_setup` leaves the task in backlog and
//! holds the run; a later admission proceeds after origin recovers.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread;

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
fn worktree_setup_origin_outage_holds_concurrent_runs_and_recovers() {
    if !isolated(
        "origin_fetch_hold::worktree_setup_origin_outage_holds_concurrent_runs_and_recovers",
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
    std::fs::write(&mode, "hang").unwrap();
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

    let mut saw_fetch_lock_waiter = false;
    for (task_id, run_id) in &admitted {
        let run = runtime.show_job_run(run_id).unwrap();
        assert_eq!(run.state, JobRunState::Held, "{run_id}: {:?}", run.steps);
        assert_eq!(
            run.steps[0].error_code.as_deref(),
            Some(ORIGIN_UNAVAILABLE_CODE),
            "{run_id}: {:?}",
            run.steps[0]
        );
        saw_fetch_lock_waiter |= run.steps[0]
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("acquiring git fetch lock"));
        assert_eq!(
            runtime.get_task(task_id).unwrap().status,
            TaskStatus::Backlog
        );
    }
    assert!(
        saw_fetch_lock_waiter,
        "concurrent admissions exercise a fetch-lock timeout before held settlement"
    );

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
            "#!/bin/sh\ncase \"$(cat '{}')\" in\nhang) exec sleep 30 ;;\nauth) echo 'Permission denied (publickey). Connection closed' >&2; exit 1 ;;\n*) exec git-upload-pack '{}' ;;\nesac\n",
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
