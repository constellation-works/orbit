//! Grant changes stay frozen while a stopped attempt's worktree survives.

use chrono::Utc;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_store::contracts::{
    AdmissionRunContext, ExecutionClaim, ExecutionClaimPhase, ExecutionLocation,
};
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::json;
use std::path::{Path, PathBuf};

const ELIGIBLE_STATUSES: [TaskStatus; 3] = [
    TaskStatus::Proposed,
    TaskStatus::Backlog,
    TaskStatus::Someday,
];

fn fixture(status: TaskStatus) -> (tempfile::TempDir, OrbitRuntime, PathBuf, Task) {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let repo = root.path().join("repo");
    let global = root.path().join("global");
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::create_dir_all(&global).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Orbit Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    );
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Grant window fixture".into(),
            status: Some(status),
            granted_tools: vec!["github.run.list".into()],
            ..Default::default()
        })
        .unwrap();
    (root, runtime, repo, task)
}

fn git(repo: &Path, args: &[&str]) {
    let output = crate::git_test_support::run(repo, args).unwrap();
    assert!(output.success, "{args:?}: {}", output.stderr);
}

fn recorded_run(runtime: &OrbitRuntime, input: serde_json::Value, state: JobRunState) -> JobRun {
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input), None)
        .unwrap();
    if state != JobRunState::Pending {
        jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
    }
    if state.is_terminal() {
        jobs.finalize_job_run(&run.run_id, state, Utc::now(), None)
            .unwrap();
    }
    run
}

fn record_claim(runtime: &OrbitRuntime, task: &Task, phase: ExecutionClaimPhase) {
    let claim = ExecutionClaim {
        claim_id: "grant-window-claim".into(),
        task_id: task.id.clone(),
        request_id: "grant-window-request".into(),
        executed_on: ExecutionLocation {
            machine_id: "fixture-machine".into(),
            machine_name: None,
        },
        run_context: AdmissionRunContext {
            run_id: "fixture-run".into(),
            job_name: "task_pr_pipeline".into(),
            machine_name: None,
        },
        footprint: Vec::new(),
        reservation_id: "expired-reservation".into(),
        reservation_expires_at: (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
        phase,
        repair: None,
    };
    let connection = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    connection.execute(
        "INSERT OR REPLACE INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
         VALUES (?1, 'distributed-execution-claim-v1', ?2, ?3, 'fixture', ?4)",
        rusqlite::params![runtime.workspace_id().unwrap(), claim.claim_id, serde_json::to_string(&claim).unwrap(), Utc::now().to_rfc3339()],
    ).unwrap();
}

fn change_grant(
    runtime: &OrbitRuntime,
    task: &Task,
    tools: Vec<String>,
) -> Result<Task, OrbitError> {
    runtime.update_task_with_identity(
        &task.id,
        TaskUpdateParams {
            granted_tools: Some(tools),
            comment: Some("Grant update".into()),
            ..Default::default()
        },
        None,
        None,
    )
}

fn assert_refused_atomically(runtime: &OrbitRuntime, task: &Task, reason: &str) {
    let before = runtime.get_task(&task.id).unwrap();
    let history = runtime.get_task_history(&task.id).unwrap();
    let comments = runtime.get_task_comments(&task.id).unwrap();
    for tools in [
        vec!["github.run.list".into(), "github.run.logs".into()],
        Vec::new(),
    ] {
        let error = change_grant(runtime, task, tools).expect_err(
            "a dispatched attempt's grant must remain frozen (preserved-worktree defect)",
        );
        assert!(matches!(error, OrbitError::InvalidInput(_)), "{error}");
        assert!(error.to_string().contains(reason), "{error}");
        assert_eq!(runtime.get_task(&task.id).unwrap(), before);
        assert_eq!(runtime.get_task_history(&task.id).unwrap(), history);
        assert_eq!(runtime.get_task_comments(&task.id).unwrap(), comments);
    }
}

fn preserved_worktrees_refuse_grant_changes_until_removed() {
    for status in ELIGIBLE_STATUSES {
        for fallback in [false, true] {
            let (_root, runtime, repo, task) = fixture(status);
            let input = if fallback {
                json!({"task_id": task.id, "branch_prefix": "team/grants"})
            } else {
                json!({"task_ids": [task.id, "bundle-companion"]})
            };
            let run = recorded_run(&runtime, input, JobRunState::Failed);
            let paths = orbit_engine::run_worktree_paths(&repo, &run).unwrap();
            let worktree = if fallback {
                paths.last().unwrap()
            } else {
                &paths[0]
            };
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    "-q",
                    worktree.to_str().unwrap(),
                    "HEAD",
                ],
            );
            std::fs::write(worktree.join("candidate.txt"), "preserved work\n").unwrap();
            // A newer stopped run and a released claim must not hide the older tree.
            let latest = recorded_run(
                &runtime,
                json!({"task_ids": [task.id]}),
                JobRunState::Failed,
            );
            runtime
                .update_task_with_identity(
                    &task.id,
                    TaskUpdateParams {
                        job_run_id: Some(Some(latest.run_id)),
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .unwrap();
            record_claim(&runtime, &task, ExecutionClaimPhase::Revoked);
            assert_refused_atomically(&runtime, &task, &worktree.display().to_string());
            assert_eq!(
                std::fs::read_to_string(worktree.join("candidate.txt")).unwrap(),
                "preserved work\n"
            );
            git(
                &repo,
                &["worktree", "remove", "--force", worktree.to_str().unwrap()],
            );
            assert!(
                change_grant(&runtime, &task, Vec::new())
                    .unwrap()
                    .granted_tools
                    .is_empty()
            );
        }
    }
}

fn pre_dispatch_tasks_accept_grant_changes() {
    for status in ELIGIBLE_STATUSES {
        let (_root, runtime, _repo, task) = fixture(status);
        let widened = vec!["github.run.list".into(), "github.run.logs".into()];
        assert_eq!(
            change_grant(&runtime, &task, widened.clone())
                .unwrap()
                .granted_tools,
            widened
        );
        assert!(
            change_grant(&runtime, &task, Vec::new())
                .unwrap()
                .granted_tools
                .is_empty()
        );
    }
}

fn live_runs_and_protecting_claims_still_refuse_grant_changes() {
    for status in ELIGIBLE_STATUSES {
        for state in [JobRunState::Pending, JobRunState::Running] {
            let (_root, runtime, _repo, task) = fixture(status);
            recorded_run(&runtime, json!({"task_ids": [task.id]}), state);
            assert_refused_atomically(&runtime, &task, "local run");
        }
        let (_root, runtime, _repo, task) = fixture(status);
        for phase in [
            ExecutionClaimPhase::Claimed,
            ExecutionClaimPhase::Running,
            ExecutionClaimPhase::HandedOff,
            ExecutionClaimPhase::RepairPending,
        ] {
            record_claim(&runtime, &task, phase);
            assert_refused_atomically(&runtime, &task, "execution claim");
        }
        record_claim(&runtime, &task, ExecutionClaimPhase::Revoked);
        assert!(
            change_grant(&runtime, &task, Vec::new())
                .unwrap()
                .granted_tools
                .is_empty()
        );
    }
}

crate::scenario_table!(
    granted_tools_window_scenarios,
    pre_dispatch_tasks_accept_grant_changes,
    live_runs_and_protecting_claims_still_refuse_grant_changes,
    preserved_worktrees_refuse_grant_changes_until_removed,
);
