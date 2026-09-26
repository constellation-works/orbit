//! [ORB-10470] Resume lineage ownership and checkpoint reuse.
//!
//! These reproduce the `jrun-20260725-2246-3` recovery sequence recorded in
//! F2026-07-121 / F2026-07-122: a `task_pr_pipeline` whose delivery tail failed
//! at `push`, whose task was blocked by that failure and then re-stamped by an
//! intervening short-lived attempt, and whose resume has to reach the failed
//! step without re-running the agent implementation that already succeeded.

use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_store::contracts::TaskReservationReleaseReason;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::JobRunState;
use serde_json::json;

use crate::OrbitRuntime;
use crate::application::job::JobRunListParams;
use crate::application::job::pipeline::worker_command_override;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

use super::exec::{test_runtime, v2_events};

/// A delivery-tail shaped job: one step that claims the worktree (and so emits
/// the batch/ownership id), one implementation step, then the deterministic
/// tail that failed in the incident.
fn write_delivery_tail_job(path: &Path, name: &str) {
    let yaml = format!(
        r#"schemaVersion: 2
kind: Job
metadata:
  name: {name}
spec:
  state: enabled
  kind: workflow
  steps:
    - id: worktree
      spec:
        type: deterministic
        action: sleep
        config: {{}}
    - id: implement_bundle
      spec:
        type: deterministic
        action: sleep
        config: {{}}
    - id: push
      spec:
        type: deterministic
        action: sleep
        config: {{}}
    - id: pr_open
      spec:
        type: deterministic
        action: sleep
        config: {{}}
"#
    );
    std::fs::write(path, yaml).expect("write delivery-tail job yaml");
}

fn seed_task(runtime: &OrbitRuntime, title: &str) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: "Fixture task for resume lineage reconciliation.".to_string(),
            plan: "Fixture execution plan.".to_string(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed task")
        .id
}

fn couple_task_to_run(runtime: &OrbitRuntime, task_id: &str, run_id: &str, status: TaskStatus) {
    runtime
        .update_task(
            task_id,
            TaskUpdateParams {
                status: Some(status),
                job_run_id: Some(Some(run_id.to_string())),
                ..Default::default()
            },
        )
        .expect("couple task to run");
}

/// Seed the incident's source run: `worktree` and `implement_bundle`
/// checkpointed as success (the worktree checkpoint carrying the batch id),
/// then a terminal `failed` at `push`, which blocks the coupled task.
fn seed_failed_delivery_run(
    runtime: &OrbitRuntime,
    job_name: &str,
    task_id: &str,
    retry_source: Option<&str>,
) -> String {
    let run_id = seed_checkpointed_delivery_run(
        runtime,
        job_name,
        task_id,
        retry_source,
        Utc::now(),
        std::process::id(),
    );
    runtime
        .finalize_job_run_with_reservation_cleanup(
            &run_id,
            JobRunState::Failed,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("finalize source run as failed");
    run_id
}

/// A running delivery run with its `worktree` and `implement_bundle` steps
/// checkpointed and the task claimed, owned by `owner_pid`.
fn seed_checkpointed_delivery_run(
    runtime: &OrbitRuntime,
    job_name: &str,
    task_id: &str,
    retry_source: Option<&str>,
    started_at: chrono::DateTime<Utc>,
    owner_pid: u32,
) -> String {
    let input = json!({"seconds": 0, "task_ids": [task_id]});
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(
            job_name,
            1,
            Utc::now(),
            Some(input.clone()),
            retry_source.map(ToOwned::to_owned),
        )
        .expect("insert source run");
    let initial = orbit_types::workflow::PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        input.clone(),
    );
    runtime
        .stores()
        .jobs()
        .write_run_state(&run.run_id, &initial)
        .expect("write initial state");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started_at, owner_pid)
        .expect("mark source run running");

    <OrbitRuntime as RuntimeHost>::checkpoint_step(
        runtime,
        &run.run_id,
        0,
        "worktree",
        &json!({"job_run_id": run.run_id, "batch_id": run.run_id, "workspace_path": "/tmp/wt"}),
    )
    .expect("checkpoint worktree step");
    <OrbitRuntime as RuntimeHost>::checkpoint_step(
        runtime,
        &run.run_id,
        1,
        "implement_bundle",
        &json!({"implemented": true}),
    )
    .expect("checkpoint implement step");

    // `worktree_setup` claims the task.
    couple_task_to_run(runtime, task_id, &run.run_id, TaskStatus::InProgress);
    run.run_id
}

#[test]
fn resume_readmits_blocked_task_and_realigns_ownership_to_the_checkpointed_batch() {
    let (_root, runtime, _repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    write_delivery_tail_job(&jobs_dir.join("qa_resume_tail.yaml"), "qa_resume_tail");

    let task_id = seed_task(&runtime, "resume lineage fixture");
    let source_run_id = seed_failed_delivery_run(&runtime, "qa_resume_tail", &task_id, None);
    assert_eq!(
        runtime
            .get_task(&task_id)
            .expect("task after failure")
            .status,
        TaskStatus::Blocked,
        "the source run's failure blocks its coupled task",
    );

    // An intervening short-lived attempt re-stamps the task, exactly as
    // `jrun-20260725-2343` did before the real resume ran.
    let intervening_run_id =
        seed_failed_delivery_run(&runtime, "qa_resume_tail", &task_id, Some(&source_run_id));
    couple_task_to_run(&runtime, &task_id, &intervening_run_id, TaskStatus::Blocked);

    let result = runtime
        .resume_job_run(&source_run_id)
        .expect("resume the source run");

    assert!(result.success);
    let task = runtime.get_task(&task_id).expect("task after resume");
    assert_eq!(
        task.status,
        TaskStatus::InProgress,
        "a task blocked by this lineage's own failure is re-admitted by the resume",
    );
    assert_eq!(
        task.job_run_id.as_deref(),
        Some(source_run_id.as_str()),
        "ownership is realigned to the batch id the reused checkpoints carry, \
         not to the intervening attempt and not to the new run id",
    );

    // The reused steps are skipped, and only the delivery tail re-executes.
    let skipped: Vec<String> = v2_events(&runtime, &result.run_id, "step.skipped")
        .iter()
        .map(|row| {
            let payload: serde_json::Value =
                serde_json::from_str(&row.payload_json).expect("payload");
            payload["step_id"].as_str().unwrap_or_default().to_string()
        })
        .collect();
    assert!(skipped.contains(&"worktree".to_string()));
    assert!(
        skipped.contains(&"implement_bundle".to_string()),
        "an already-successful agent implementation is never re-run by resume",
    );
    let started: Vec<String> = v2_events(&runtime, &result.run_id, "step.started")
        .iter()
        .map(|row| {
            let payload: serde_json::Value =
                serde_json::from_str(&row.payload_json).expect("payload");
            payload["step_id"].as_str().unwrap_or_default().to_string()
        })
        .collect();
    assert!(!started.contains(&"implement_bundle".to_string()));
    assert!(started.contains(&"push".to_string()));
    assert!(started.contains(&"pr_open".to_string()));

    let resumed = runtime.show_job_run(&result.run_id).expect("show resumed");
    assert_eq!(
        resumed.retry_source_run_id.as_deref(),
        Some(source_run_id.as_str())
    );
}

/// [ORB-12969] A reboot kills the worker; the sweep reconciles the run
/// `interrupted` and blocks its task. Resuming that run must bring the task
/// back to `in-progress` and realign its ownership with no manual task update,
/// exactly as it does for a failed source.
#[test]
fn resume_readmits_a_task_blocked_by_an_interrupted_source() {
    let (_root, runtime, _repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    write_delivery_tail_job(
        &jobs_dir.join("qa_resume_interrupted.yaml"),
        "qa_resume_interrupted",
    );

    let task_id = seed_task(&runtime, "interrupted resume fixture");
    // An owner pid that cannot exist: the worker died with the host.
    let source_run_id = seed_checkpointed_delivery_run(
        &runtime,
        "qa_resume_interrupted",
        &task_id,
        None,
        Utc::now() - chrono::Duration::seconds(3),
        999_999,
    );
    runtime
        .reconcile_stale_job_runs(None)
        .expect("orphan sweep");
    assert_eq!(
        runtime
            .show_job_run(&source_run_id)
            .expect("source run")
            .state,
        JobRunState::Interrupted
    );
    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Blocked,
        "the interruption blocks its coupled task",
    );

    // An intervening attempt in the lineage re-stamps the task, so the resume
    // has real ownership drift to repair.
    let intervening_run_id = seed_failed_delivery_run(
        &runtime,
        "qa_resume_interrupted",
        &task_id,
        Some(&source_run_id),
    );
    couple_task_to_run(&runtime, &task_id, &intervening_run_id, TaskStatus::Blocked);

    let result = runtime
        .resume_job_run(&source_run_id)
        .expect("resume the interrupted source run");

    assert!(result.success);
    let task = runtime.get_task(&task_id).expect("task after resume");
    assert_eq!(task.status, TaskStatus::InProgress);
    assert_eq!(
        task.job_run_id.as_deref(),
        Some(source_run_id.as_str()),
        "ownership is realigned to the checkpointed batch id",
    );
}

#[test]
fn resume_reconciliation_is_idempotent_and_scoped_to_the_retry_lineage() {
    let (_root, runtime, _repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    write_delivery_tail_job(&jobs_dir.join("qa_resume_scope.yaml"), "qa_resume_scope");

    let task_id = seed_task(&runtime, "resume scope fixture");
    let source_run_id = seed_failed_delivery_run(&runtime, "qa_resume_scope", &task_id, None);
    let plan = runtime
        .plan_job_run_resume(&source_run_id)
        .expect("plan resume");

    let first = runtime
        .reconcile_resume_task_ownership(&plan, "jrun-resume-scope-new")
        .expect("first reconciliation");
    assert_eq!(first, vec![task_id.clone()]);
    let second = runtime
        .reconcile_resume_task_ownership(&plan, "jrun-resume-scope-new")
        .expect("second reconciliation");
    assert!(
        second.is_empty(),
        "reconciliation is idempotent — re-running it rewrites nothing",
    );

    // A task owned by a run outside this lineage keeps both its status and its
    // ownership: resume never weakens the coupling of an unrelated run.
    let foreign_task_id = seed_task(&runtime, "foreign fixture");
    couple_task_to_run(
        &runtime,
        &foreign_task_id,
        "jrun-unrelated-owner",
        TaskStatus::Blocked,
    );
    let reconciled = runtime
        .reconcile_resume_task_ownership(&plan, "jrun-resume-scope-new")
        .expect("reconcile with a foreign task present");
    assert!(reconciled.is_empty());
    let foreign = runtime.get_task(&foreign_task_id).expect("foreign task");
    assert_eq!(foreign.status, TaskStatus::Blocked);
    assert_eq!(foreign.job_run_id.as_deref(), Some("jrun-unrelated-owner"));
}

#[test]
fn resume_leaves_tasks_claimed_by_a_live_run_in_the_same_lineage_alone() {
    // An earlier resume of the same source may still be executing. Its claim on
    // the bundle is live, so a second resume must not steal ownership out from
    // under it — the downstream handoff check stays the arbiter instead.
    let (_root, runtime, _repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    write_delivery_tail_job(&jobs_dir.join("qa_resume_live.yaml"), "qa_resume_live");

    let task_id = seed_task(&runtime, "live sibling fixture");
    let source_run_id = seed_failed_delivery_run(&runtime, "qa_resume_live", &task_id, None);
    let live = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "qa_resume_live",
            2,
            Utc::now(),
            Some(json!({"seconds": 0, "task_ids": [task_id]})),
            Some(source_run_id.clone()),
        )
        .expect("insert live sibling resume");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&live.run_id, Utc::now(), std::process::id())
        .expect("mark sibling running");
    couple_task_to_run(&runtime, &task_id, &live.run_id, TaskStatus::InProgress);

    let plan = runtime
        .plan_job_run_resume(&source_run_id)
        .expect("plan resume");
    let reconciled = runtime
        .reconcile_resume_task_ownership(&plan, "jrun-resume-live-new")
        .expect("reconcile against a live sibling");

    assert!(reconciled.is_empty());
    let task = runtime.get_task(&task_id).expect("task after reconcile");
    assert_eq!(
        task.job_run_id.as_deref(),
        Some(live.run_id.as_str()),
        "the live sibling keeps its claim",
    );
}

#[test]
fn pipeline_worker_resumes_from_the_runs_own_checkpoints() {
    // The submission path persists the resumed run and hands it to a detached
    // worker, so the worker — not the caller — must honor the checkpoints. The
    // same path makes a worker restart idempotent: already-successful steps are
    // skipped rather than re-dispatched.
    let (_root, runtime, _repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    write_delivery_tail_job(&jobs_dir.join("qa_resume_worker.yaml"), "qa_resume_worker");

    let input = json!({"seconds": 0});
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "qa_resume_worker",
            2,
            Utc::now(),
            Some(input.clone()),
            Some("jrun-resume-worker-source".to_string()),
        )
        .expect("insert resumed run");
    let mut seeded = orbit_types::workflow::PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        input.clone(),
    );
    seeded.record_step(
        0,
        JobRunState::Success,
        Some(json!({"job_run_id": "jrun-resume-worker-source"})),
        None,
    );
    seeded.record_step(
        1,
        JobRunState::Success,
        Some(json!({"implemented": true})),
        None,
    );
    seeded.sync_pipeline(json!({
        "worktree": {"job_run_id": "jrun-resume-worker-source"},
        "implement_bundle": {"implemented": true},
    }));
    runtime
        .stores()
        .jobs()
        .write_run_state(&run.run_id, &seeded)
        .expect("seed resumed run state");

    runtime
        .execute_pipeline_run_worker(&run.run_id)
        .expect("worker executes the resumed run");

    let finished = runtime.show_job_run(&run.run_id).expect("show resumed run");
    assert_eq!(finished.state, JobRunState::Success);
    let skipped: Vec<String> = v2_events(&runtime, &run.run_id, "step.skipped")
        .iter()
        .map(|row| {
            let payload: serde_json::Value =
                serde_json::from_str(&row.payload_json).expect("payload");
            payload["step_id"].as_str().unwrap_or_default().to_string()
        })
        .collect();
    assert!(skipped.contains(&"worktree".to_string()));
    assert!(skipped.contains(&"implement_bundle".to_string()));

    let state = runtime
        .read_run_state(&run.run_id)
        .expect("read resumed state")
        .expect("state exists");
    assert_eq!(
        state.step_outputs.get(&0),
        Some(&json!({"job_run_id": "jrun-resume-worker-source"})),
        "the reused checkpoint output is preserved, not overwritten by a replay",
    );
    assert_eq!(state.step_states.len(), 4);
}

#[test]
fn resume_submission_rejects_a_non_terminal_run_before_persisting_anything() {
    let (_root, runtime, _repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    write_delivery_tail_job(&jobs_dir.join("qa_resume_guard.yaml"), "qa_resume_guard");
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("qa_resume_guard", 1, Utc::now(), None, None)
        .expect("insert pending run");

    let error = runtime
        .submit_resume_run(&run.run_id, Some("test"), None)
        .expect_err("a pending run is not resumable");
    assert!(
        error
            .to_string()
            .contains("resume requires an interrupted, failed, or timed-out run"),
        "{error}"
    );
    let runs = runtime
        .list_job_runs(JobRunListParams {
            job_id: Some("qa_resume_guard".to_string()),
            ..Default::default()
        })
        .expect("list runs");
    assert_eq!(runs.len(), 1, "a rejected resume persists no new run");
}

/// [ORB-10597] A terminal state is not proof the source stopped working.
/// `interrupted` is written by the orphan sweep without any teardown, so a run
/// condemned in error is still executing — resuming it would start a second
/// execution against the same worktree, task claims, and delivery tail.
#[cfg(unix)]
#[test]
fn resume_refuses_an_interrupted_run_whose_worker_is_still_alive() {
    use orbit_common::process::identity::process_start_identity_token;

    let (_root, runtime, _repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    write_delivery_tail_job(&jobs_dir.join("qa_resume_live.yaml"), "qa_resume_live");

    let pid = std::process::id();
    if process_start_identity_token(pid).is_none() {
        // No identity probe on this host; liveness is unknowable and resume is
        // deliberately permitted rather than blocked.
        return;
    }
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("qa_resume_live", 1, Utc::now(), None, None)
        .expect("insert run");
    // Owned by this very test process, which is unambiguously alive.
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), pid)
        .expect("mark running");
    runtime
        .stores()
        .jobs()
        .finalize_job_run(&run.run_id, JobRunState::Interrupted, Utc::now(), Some(1))
        .expect("condemn run to interrupted");

    let error = runtime
        .submit_resume_run(&run.run_id, Some("test"), None)
        .expect_err("resume must refuse a source whose worker is still alive");
    assert!(
        error.to_string().contains("is still alive"),
        "the refusal must name the live worker: {error}"
    );
    assert!(
        error.to_string().contains(&pid.to_string()),
        "the refusal must name the pid so an operator can act on it: {error}"
    );

    let runs = runtime
        .list_job_runs(JobRunListParams {
            job_id: Some("qa_resume_live".to_string()),
            ..Default::default()
        })
        .expect("list runs");
    assert_eq!(runs.len(), 1, "a refused resume persists no new run");
}

#[test]
fn claimed_leaf_refuses_generic_resume_but_same_run_evidence_retries_work() {
    use orbit_store::contracts::*;
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    let (_root, runtime, repo, global) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "claimed leaf".into(),
            plan: "execute".into(),
            status: Some(TaskStatus::Backlog),
            context_files: vec!["file:future.rs".into()],
            ..Default::default()
        })
        .expect("task");
    let backends = orbit_store::compose::workspace_coordinated_backends(
        TaskRegistryStore::open(&task_registry_path(&global)).expect("registry"),
        runtime.workspace_id().expect("workspace"),
        runtime.stores().host.sqlite.clone(),
    )
    .expect("backends");
    let identity = AdmissionIdentity::trusted_local(ExecutionLocation {
        machine_id: "machine".into(),
        machine_name: None,
    });
    let request = AdmissionRequest {
        request_id: "pull".into(),
        caller_version: "test".into(),
        caller_schema: 1,
        caller_review_policy: "none".into(),
        run_context: AdmissionRunContext {
            run_id: "drain".into(),
            job_name: "auto".into(),
            machine_name: None,
        },
        ship: AdmissionShipContract {
            mode: "pr".into(),
            base_branch: "agent-main".into(),
            landing_branch: "agent-main".into(),
            review_policy: "none".into(),
            completion: "review".into(),
            authorization_reference: None,
        },
    };
    let AdmissionLookup::Found { receipt, .. } = backends
        .commit_boundary
        .admit_task(&identity, &request, "test", &repo, &repo.join(".orbit"))
        .expect("admit")
    else {
        panic!("receipt")
    };
    let claim = receipt.claim.expect("claim");
    let leaf = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_id":task.id})),
            None,
        )
        .expect("leaf");
    let unbound = ClaimInvocation::trusted_worker(
        task.id.clone(),
        claim.claim_id.clone(),
        "machine".into(),
        None,
    );
    let run = ClaimRun {
        machine_id: "machine".into(),
        run_id: leaf.run_id.clone(),
    };
    runtime
        .mutate_execution_claim(
            Some(&unbound),
            "bind",
            &ClaimMutation::Bind {
                run: run.clone(),
                ship: request.ship,
            },
        )
        .expect("bind");
    let bound = ClaimInvocation::trusted_worker(
        task.id.clone(),
        claim.claim_id.clone(),
        "machine".into(),
        Some(run),
    );
    let evidence = ClaimMutation::Evidence(ClaimEvidence {
        comment: Some("same-run step retry".into()),
        ..Default::default()
    });
    assert!(
        runtime
            .mutate_execution_claim(None, "no-context", &evidence)
            .is_err()
    );
    let first = runtime
        .mutate_execution_claim(Some(&bound), "step", &evidence)
        .expect("step");
    assert_eq!(
        runtime
            .mutate_execution_claim(Some(&bound), "step", &evidence)
            .expect("retry"),
        first
    );
    runtime
        .stores()
        .jobs()
        .finalize_job_run(&leaf.run_id, JobRunState::Interrupted, Utc::now(), Some(1))
        .expect("interrupt");
    let error = match runtime.plan_job_run_resume(&leaf.run_id) {
        Ok(_) => panic!("resume refused"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("claimed execution"), "{error}");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::InProgress
    );
}

/// [ORB-12575] A worker killed between the pending marker and its clear leaves
/// `.task-commit-pending` in the task partition. Resume is the operator's next
/// command, so it has to replay that journal like every other runtime read —
/// not refuse with the doctor's non-repairing "claim inspection" error and
/// leave the marker for an unrelated task command to clear.
#[test]
fn resume_recovers_a_pending_commit_marker_instead_of_refusing_claim_inspection() {
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    let (_root, runtime, _repo, global) = test_runtime();
    let jobs_dir = global.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    write_delivery_tail_job(
        &jobs_dir.join("qa_resume_pending.yaml"),
        "qa_resume_pending",
    );
    let registry = TaskRegistryStore::open(&task_registry_path(&global)).expect("registry");
    let partition = registry
        .workspace_partition_dir(&runtime.workspace_id().expect("workspace"))
        .expect("partition dir");
    let marker = partition.join(".task-commit-pending");

    // The interrupted-commit signal with no journal row behind it: exactly what
    // a SIGKILL between `write_pending_marker` and the journal insert leaves.
    let leave_marker = || std::fs::write(&marker, b"commit-1-1-0").expect("write pending marker");
    leave_marker();
    let inspection = runtime
        .inspect_execution_claims()
        .expect_err("doctor inspection stays non-repairing");
    assert!(
        inspection.to_string().contains("claim inspection"),
        "{inspection}"
    );
    assert!(marker.exists(), "inspection must not clear the marker");

    // CLI surface: the guard settles the partition and resume reaches its
    // ordinary resolution of the source run.
    let error = runtime
        .plan_job_run_resume("jrun-does-not-exist")
        .err()
        .expect("unknown run is still refused");
    assert!(
        !error.to_string().contains("claim inspection"),
        "resume must not surface the inspection error: {error}"
    );
    assert!(error.to_string().contains("not found"), "{error}");
    assert!(!marker.exists(), "resume recovered the pending commit");

    // Submission surface (MCP/HTTP/dashboard) funnels through the same planner.
    leave_marker();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("qa_resume_pending", 1, Utc::now(), None, None)
        .expect("insert pending run");
    let error = runtime
        .submit_resume_run(&run.run_id, Some("test"), None)
        .expect_err("a pending run is not resumable");
    assert!(
        error
            .to_string()
            .contains("resume requires an interrupted, failed, or timed-out run"),
        "{error}"
    );
    assert!(
        !marker.exists(),
        "submit_resume_run recovered the pending commit"
    );
}

/// A pipeline worker that stays alive without claiming, so a submitted resume
/// remains `pending` for the whole test instead of being terminalized by its
/// startup observer. Thread-local: each racing thread installs its own.
struct IdleWorker;

impl IdleWorker {
    fn install() -> Self {
        worker_command_override::set(["sh", "-c", "sleep 10"]);
        Self
    }
}

impl Drop for IdleWorker {
    fn drop(&mut self) {
        worker_command_override::clear();
    }
}

fn resumes_of(runtime: &OrbitRuntime, source_run_id: &str) -> Vec<String> {
    runtime
        .stores()
        .jobs()
        .job_run_retries(source_run_id, 100)
        .expect("list resumes")
        .into_iter()
        .map(|run| run.run_id)
        .collect()
}

fn live_resume_of(error: OrbitError) -> (String, String) {
    match error {
        OrbitError::ResumeRunInFlight {
            source_run_id,
            run_id,
        } => (source_run_id, run_id),
        other => panic!("expected ResumeRunInFlight, got {other:?}"),
    }
}

/// The 2026-09-25 reboot recovery: eight dashboard resumes of one source were
/// all accepted and eight agents shared its worktree. `submit_resume_run` is
/// the seam the CLI, MCP, and HTTP surfaces share, so refusing here refuses
/// everywhere; a terminal first resume re-opens the source.
#[test]
fn a_second_resume_is_refused_while_the_first_is_live_and_allowed_once_it_is_cancelled() {
    let (_root, runtime, _repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    write_delivery_tail_job(&jobs_dir.join("qa_resume_once.yaml"), "qa_resume_once");
    let task_id = seed_task(&runtime, "resume once fixture");
    let source = seed_failed_delivery_run(&runtime, "qa_resume_once", &task_id, None);
    let _worker = IdleWorker::install();

    let first = runtime
        .submit_resume_run(&source, Some("test"), None)
        .expect("first resume is admitted");
    let refused = runtime
        .submit_resume_run(&source, Some("dashboard"), None)
        .expect_err("a second resume while the first is live is refused");
    assert!(
        refused.to_string().contains(&first.run_id),
        "the refusal names the live run: {refused}"
    );
    assert_eq!(
        live_resume_of(refused),
        (source.clone(), first.run_id.clone())
    );
    assert_eq!(resumes_of(&runtime, &source), vec![first.run_id.clone()]);

    runtime
        .stores()
        .jobs()
        .finalize_job_run(&first.run_id, JobRunState::Cancelled, Utc::now(), Some(1))
        .expect("cancel first resume");
    let second = runtime
        .submit_resume_run(&source, Some("test"), None)
        .expect("resume is allowed again once the first is terminal");
    let second_run = runtime
        .get_job_run_backend(&second.run_id)
        .expect("read second resume")
        .expect("second resume exists");
    assert_eq!(
        second_run.retry_source_run_id.as_deref(),
        Some(source.as_str()),
        "a re-resume chains from the run the caller named"
    );
    assert_eq!(second_run.attempt, 2);
}

/// Two processes resuming the same source at once — modelled as two runtimes
/// over one workspace — admit exactly one run; the loser names the winner.
#[test]
fn concurrent_resumes_of_one_source_create_exactly_one_run() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    let jobs_dir = global_root.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    write_delivery_tail_job(&jobs_dir.join("qa_resume_race.yaml"), "qa_resume_race");
    let task_id = seed_task(&runtime, "resume race fixture");
    let source = seed_failed_delivery_run(&runtime, "qa_resume_race", &task_id, None);
    let workspace_root = repo_root.join(".orbit");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

    let outcomes = std::thread::scope(|scope| {
        let racers = (0..2)
            .map(|_| {
                let barrier = std::sync::Arc::clone(&barrier);
                let (global_root, workspace_root, source) =
                    (&global_root, &workspace_root, &source);
                scope.spawn(move || {
                    let racer = OrbitRuntime::from_roots(global_root, workspace_root)
                        .expect("racer runtime");
                    let _worker = IdleWorker::install();
                    barrier.wait();
                    racer.submit_resume_run(source, Some("racer"), None)
                })
            })
            .collect::<Vec<_>>();
        racers
            .into_iter()
            .map(|racer| racer.join().expect("racer thread"))
            .collect::<Vec<_>>()
    });

    let admitted = outcomes
        .iter()
        .filter_map(|outcome| outcome.as_ref().ok())
        .map(|invoke| invoke.run_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        admitted.len(),
        1,
        "exactly one resume is admitted: {outcomes:?}"
    );
    let loser = outcomes
        .into_iter()
        .find_map(Result::err)
        .expect("one racer is refused");
    assert_eq!(live_resume_of(loser), (source.clone(), admitted[0].clone()));
    assert_eq!(resumes_of(&runtime, &source), admitted);
}
