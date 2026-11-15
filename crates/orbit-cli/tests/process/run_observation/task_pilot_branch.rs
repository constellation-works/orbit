//! A commit passed as a pilot branch is refused before a run or task write.

use std::fs;
use std::time::Duration;

use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
use orbit_common::test_env;
use orbit_core::application::task::TaskAddParams;
use serde_json::{Value, json};

use super::{Fixture, isolated_run_observation};
use crate::git_repo;

#[test]
fn task_pilot_branch_admission_preserves_source_identity() {
    if !isolated_run_observation(
        "run_observation::task_pilot_branch::task_pilot_branch_admission_preserves_source_identity",
    ) {
        return;
    }
    let fixture = Fixture::terminal();
    git_repo::SeededRepo::new(&fixture.work).build();
    let pinned = git_repo::run(&fixture.work, &["rev-parse", "HEAD"]);
    fs::write(fixture.work.join("README.md"), "new branch source\n").unwrap();
    git_repo::run(&fixture.work, &["commit", "-am", "advance branch"]);
    let tip = git_repo::run(&fixture.work, &["rev-parse", "HEAD"]);
    let origin = fixture.home.join("origin.git");
    git_repo::run(
        &fixture.work,
        &["clone", "--bare", ".", origin.to_str().unwrap()],
    );
    git_repo::run(
        &fixture.work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    let runtime = fixture.registered_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Pilot branch admission".into(),
            acceptance_criteria: vec!["Inspect the requested source".into()],
            ..Default::default()
        })
        .unwrap();
    let before_task = serde_json::to_value(runtime.get_task(&task.id).unwrap()).unwrap();
    let before_history = serde_json::to_value(runtime.get_task_history(&task.id).unwrap()).unwrap();
    let before_runs = fixture.snapshot();
    // Limit successful controls to the real prepare boundary, without providers
    // or apply writes. Preparation still resolves the requested source.
    fixture.write_job(
        "task_pilot_pipeline.yaml",
        &json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pilot_pipeline"},
        "spec": {"state": "enabled", "kind": "workflow", "steps": [{"id": "prepare",
            "spec": {"type": "deterministic", "action": "prepare_task_pilot", "config": {}}
        }]}})
        .to_string(),
    );
    let cli = |args: &[&str]| {
        let output = fixture
            .orbit()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .args(args)
            .timeout(Duration::from_secs(120))
            .output()
            .unwrap();
        if let Ok(result) = serde_json::from_slice::<Value>(&output.stdout)
            && let Some(id) = result["run_id"].as_str()
        {
            let run = runtime.show_job_run(id).unwrap();
            test_env::wait_until("disposable pilot worker exit", || {
                run.pid.is_none_or(|pid| {
                    probe_process_liveness(pid, run.pid_start_time.as_deref())
                        == ProcessLiveness::Exited
                })
            });
        }
        output
    };
    for branch in [pinned.clone(), format!(" origin/{pinned} ")] {
        let output = cli(&[
            "run",
            "task-pilot",
            &task.id,
            "--base-branch",
            &branch,
            "--wait",
            "--timeout-seconds",
            "60",
            "--json",
        ]);
        assert!(!output.status.success(), "{output:?}");
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostic.contains("--base-branch requires a branch name"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("--input source_revision=<FULL_COMMIT_ID>"),
            "{diagnostic}"
        );
        assert_eq!(
            fixture.snapshot(),
            before_runs,
            "a mistaken revision must create no pilot run"
        );
        assert_eq!(
            serde_json::to_value(runtime.get_task(&task.id).unwrap()).unwrap(),
            before_task
        );
        assert_eq!(
            serde_json::to_value(runtime.get_task_history(&task.id).unwrap()).unwrap(),
            before_history
        );
    }
    // Give the commit's exact hex spelling a real remote branch at a *different*
    // commit. Stale tracking and local branches must not decide the pin.
    git_repo::run(
        &fixture.work,
        &["push", "origin", &format!("HEAD:refs/heads/{pinned}")],
    );
    git_repo::run(
        &fixture.work,
        &[
            "update-ref",
            &format!("refs/remotes/origin/{pinned}"),
            &pinned,
        ],
    );
    git_repo::run(&fixture.work, &["update-ref", "refs/heads/main", &pinned]);
    for branch in [
        "main".to_string(),
        pinned.clone(),
        format!("origin/{pinned}"),
    ] {
        let output = cli(&[
            "run",
            "task-pilot",
            "--base-branch",
            &branch,
            "--max-tasks",
            "1",
            "--wait",
            "--timeout-seconds",
            "60",
            "--json",
        ]);
        assert!(output.status.success(), "{output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        let state = runtime
            .read_run_state(result["run_id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        let source = &state.pipeline["prepare"]["source"];
        let normalized = branch.strip_prefix("origin/").unwrap_or(&branch);
        assert_eq!(source["source_ref"], format!("origin/{normalized}"));
        assert_eq!(source["base_branch"], normalized);
        assert_eq!(
            source["source_revision"], tip,
            "a real branch must pin its fetched head"
        );
    }
    let output = cli(&[
        "run",
        "job",
        "task_pilot_pipeline",
        "--input",
        "base_branch=main",
        "--input",
        &format!("source_revision={pinned}"),
        "--input",
        "max_tasks=1",
        "--wait",
        "--timeout-seconds",
        "60",
        "--json",
    ]);
    assert!(output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let state = runtime
        .read_run_state(result["run_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(state.pipeline["prepare"]["source"]["base_branch"], "main");
    assert_eq!(state.pipeline["prepare"]["source"]["source_ref"], pinned);
    assert_eq!(
        state.pipeline["prepare"]["source"]["source_revision"],
        pinned
    );
    assert_eq!(git_repo::run(&fixture.work, &["rev-parse", "HEAD"]), pinned);
}
