//! A resumed run whose deferred footprint stays held past its wait ends
//! held through the public worker and its real run finalizer: no editor
//! starts, the run does not fail, and its task returns to the backlog for a
//! later admission. `footprint_wait_seconds: 0` makes the wait expire on
//! the first poll, so nothing here sleeps or depends on load.
#![cfg(unix)]

use std::path::Path;

use chrono::Utc;
use orbit_core::TaskStatus;
use orbit_engine::{Dispatch, RunLifecycle};
use orbit_engine::{DispatchError, TaskAutomationUpdate, V2AuditWriter, execute_job_with_resume};
use orbit_types::resource::ExecutorResource;
use orbit_types::workflow::activity_job::{JobV2, Provider, V2AuditEvent, V2AuditEventKind};
use orbit_types::workflow::{
    ExecutorDef, ExecutorSandboxKind, FOOTPRINT_UNAVAILABLE_ERROR_CODE, HeldFailure, JobRunState,
    PR_CONFLICT_RECOVERY_ACTIVITY, PipelineState, STEP_FAILURE_RECOVERY_ACTIVITY,
};
use serde_json::json;

use super::provider_failure_hold::{FakeCodex, fixture, implementation_job};

#[test]
fn busy_deferred_footprint_holds_the_run_and_requeues_the_task() {
    if !super::dispatch_admission::isolated(
        "footprint_hold::busy_deferred_footprint_holds_the_run_and_requeues_the_task",
    ) {
        return;
    }
    let fx = fixture(
        "[workflow]\ndefault_crew = \"fixture\"\n\n\
         [crews.fixture]\nprovider = \"codex\"\nmodel = \"fixture-model\"\n",
    );
    // The fake editor proves it started by touching a marker; a held run
    // must never reach it.
    let marker = fx.repo.join("editor-started");
    let frame = json!({"type": "turn.failed", "error": {"message": "editor ran"}});
    let cli = FakeCodex::named(&fx.repo, "codex", &frame.to_string());
    std::fs::write(
        &cli.path,
        format!(
            "#!/bin/sh\ntouch '{}'\ncat > /dev/null\ncat <<'STDOUT'\n{frame}\nSTDOUT\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut executor: ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/codex.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(cli.path.display().to_string());
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    fx.runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();

    let jobs_dir = fx.runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    let mut job = implementation_job(Provider::Codex);
    job.steps.remove(0);
    job.steps[0].recovery_activity = None;
    job.steps[0].resolved_recovery_activity = None;
    job.failure_activity = None;
    job.resolved_failure_activity = None;
    job.final_recovery_activity = None;
    job.resolved_final_recovery_activity = None;
    std::fs::write(jobs_dir.join("task_pr_pipeline.yaml"), serde_json::to_string(&json!({
        "schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pr_pipeline"}, "spec": job,
    })).unwrap()).unwrap();

    // Both tasks claim `file:feature.txt`; the holder in review keeps it.
    let task = fx.task("fixture");
    let holder = fx.task("fixture");
    fx.runtime
        .apply_task_automation_update(
            &holder,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();

    let input = json!({
        "task_ids": [task],
        "crew": "fixture",
        "footprint_deferred": true,
        "footprint_wait_seconds": 0,
    });
    let run = fx
        .runtime
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input.clone()), None)
        .unwrap();
    fx.runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), input),
        )
        .unwrap();
    fx.runtime
        .apply_task_automation_update(
            &task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(run.run_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();

    let _ = fx.runtime.execute_pipeline_run_worker(&run.run_id);
    let stored = fx.runtime.show_job_run(&run.run_id).unwrap();
    assert_eq!(stored.state, JobRunState::Held, "{:?}", stored.steps);
    let diagnostic = stored.steps.last().expect("run diagnostic");
    assert_eq!(
        diagnostic.error_code.as_deref(),
        Some(FOOTPRINT_UNAVAILABLE_ERROR_CODE)
    );
    assert!(
        diagnostic
            .error_message
            .as_deref()
            .unwrap()
            .contains(&holder),
        "the held diagnostic names the holder: {diagnostic:?}"
    );
    assert!(!marker.exists(), "no repair editor started");
    assert_eq!(fx.status(&task), TaskStatus::Backlog);

    let deferred = fx
        .runtime
        .audit_failure_incidents(&orbit_store::contract::FailureIncidentQuery::default())
        .unwrap()
        .incidents
        .into_iter()
        .filter(|incident| {
            incident.surface == orbit_store::contract::RUN_DEFERRED_SURFACE
                && incident.run_ids.contains(&run.run_id)
        })
        .map(|incident| incident.class)
        .collect::<Vec<_>>();
    assert_eq!(deferred, [orbit_store::contract::FailureClass::Deferred]);

    let replay_count = fx.runtime.get_task_history(&task).unwrap().len();
    fx.runtime
        .finalize_job_run(&run.run_id, JobRunState::Held, Utc::now(), Some(1))
        .unwrap();
    assert_eq!(
        fx.runtime.get_task_history(&task).unwrap().len(),
        replay_count,
        "held cleanup is idempotent"
    );

    // Once the holder releases, a later admission takes the footprint.
    fx.runtime
        .apply_task_automation_update(
            &holder,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .unwrap();
    let next = fx
        .runtime
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({
                "task_ids": [task],
                "footprint_deferred": true,
                "footprint_wait_seconds": 0,
            })),
            None,
        )
        .unwrap();
    Dispatch::acquire_deferred_footprint(&fx.runtime, &next.run_id, "agent_review_repair")
        .expect("the footprint is taken once the holder releases it");
}

#[test]
fn stopped_run_waiting_for_its_footprint_is_not_held() {
    if !super::dispatch_admission::isolated(
        "footprint_hold::stopped_run_waiting_for_its_footprint_is_not_held",
    ) {
        return;
    }
    let fx = fixture(
        "[workflow]\ndefault_crew = \"fixture\"\n\n\
         [crews.fixture]\nprovider = \"codex\"\nmodel = \"fixture-model\"\n",
    );
    let task = fx.task("fixture");
    let holder = fx.task("fixture");
    fx.runtime
        .apply_task_automation_update(
            &holder,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();
    // The run is not `Running`, so the wait ends on the stopped-run check
    // before any sleep, even with a long wait bound.
    let run = fx
        .runtime
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({
                "task_ids": [task],
                "footprint_deferred": true,
                "footprint_wait_seconds": 3600,
            })),
            None,
        )
        .unwrap();
    let error =
        Dispatch::acquire_deferred_footprint(&fx.runtime, &run.run_id, "agent_review_repair")
            .unwrap_err();
    assert!(
        matches!(error, DispatchError::CliInvocationPermanent(_)),
        "a stopped run keeps the permanent failure, got: {error}"
    );
    assert!(
        error.to_string().contains("run stopped"),
        "the refusal names the stop: {error}"
    );
    assert_eq!(
        HeldFailure::of_local(Some(error.failure_code()), Some(&error.to_string())),
        None,
        "cancellation is never classified as a hold"
    );
}

/// An exhausted budget does not hide a stop: with `footprint_wait_seconds: 0`
/// a stopped run still keeps its permanent refusal, while the same wait on a
/// running run ends in the typed hold. Neither refusal takes the footprint
/// from its holder, and once the holder releases it the running run takes it.
#[test]
fn an_exhausted_footprint_wait_keeps_a_stopped_run_permanent() {
    if !super::dispatch_admission::isolated(
        "footprint_hold::an_exhausted_footprint_wait_keeps_a_stopped_run_permanent",
    ) {
        return;
    }
    let fx = fixture(
        "[workflow]\ndefault_crew = \"fixture\"\n\n\
         [crews.fixture]\nprovider = \"codex\"\nmodel = \"fixture-model\"\n",
    );
    let task = fx.task("fixture");
    let holder = fx.task("fixture");
    fx.runtime
        .apply_task_automation_update(
            &holder,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();
    let insert = || {
        fx.runtime
            .insert_job_run(
                "task_pr_pipeline",
                1,
                Utc::now(),
                Some(json!({
                    "task_ids": [task],
                    "footprint_deferred": true,
                    "footprint_wait_seconds": 0,
                })),
                None,
            )
            .unwrap()
            .run_id
    };

    let stopped = insert();
    let error = Dispatch::acquire_deferred_footprint(&fx.runtime, &stopped, "agent_review_repair")
        .unwrap_err();
    assert!(
        matches!(error, DispatchError::CliInvocationPermanent(_)),
        "a stopped run with a spent budget keeps the permanent failure, got: {error}"
    );
    assert!(
        error.to_string().contains("run stopped"),
        "the refusal names the stop: {error}"
    );
    assert_eq!(
        HeldFailure::of_local(Some(error.failure_code()), Some(&error.to_string())),
        None,
        "cancellation is never classified as a hold"
    );

    let running = insert();
    fx.runtime
        .mark_job_run_running(&running, Utc::now(), std::process::id())
        .unwrap();
    let error = Dispatch::acquire_deferred_footprint(&fx.runtime, &running, "agent_review_repair")
        .unwrap_err();
    assert!(
        matches!(error, DispatchError::FootprintUnavailable(_)),
        "a running run with a spent budget is held, got: {error}"
    );
    assert!(
        error.to_string().contains(&holder),
        "the holder still owns the footprint the stopped run was refused: {error}"
    );
    assert_eq!(
        HeldFailure::of_local(Some(error.failure_code()), Some(&error.to_string())),
        Some(HeldFailure::FootprintUnavailable),
    );

    fx.runtime
        .apply_task_automation_update(
            &holder,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .unwrap();
    Dispatch::acquire_deferred_footprint(&fx.runtime, &running, "agent_review_repair")
        .expect("the running run takes the footprint once the holder releases it");
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_candidate_repo(
    fx: &super::provider_failure_hold::Fixture,
    conflict: bool,
) -> (String, String) {
    git(&fx.repo, &["init", "-q", "-b", "main"]);
    git(&fx.repo, &["config", "user.email", "fixture@example.com"]);
    git(&fx.repo, &["config", "user.name", "Fixture"]);
    git(&fx.repo, &["config", "commit.gpgsign", "false"]);
    git(&fx.repo, &["config", "maintenance.auto", "false"]);
    std::fs::write(fx.repo.join(".gitignore"), ".orbit/\neditor-started\n").unwrap();
    std::fs::write(fx.repo.join("feature.txt"), "base\n").unwrap();
    git(&fx.repo, &["add", "."]);
    git(&fx.repo, &["commit", "-qm", "base"]);

    let origin = fx.repo.parent().unwrap().join("origin.git");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--bare", "-q"]);
    git(&origin, &["config", "maintenance.auto", "false"]);
    git(
        &fx.repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&fx.repo, &["push", "-q", "origin", "main"]);

    git(&fx.repo, &["checkout", "-qb", "task/fixture"]);
    if conflict {
        std::fs::write(fx.repo.join("feature.txt"), "candidate\n").unwrap();
        git(&fx.repo, &["add", "feature.txt"]);
    } else {
        std::fs::write(fx.repo.join("candidate-broken.txt"), "broken\n").unwrap();
        git(&fx.repo, &["add", "candidate-broken.txt"]);
    }
    git(&fx.repo, &["commit", "-qm", "candidate"]);
    let candidate_sha = git(&fx.repo, &["rev-parse", "HEAD"]);
    git(&fx.repo, &["push", "-q", "origin", "task/fixture"]);

    if conflict {
        git(&fx.repo, &["checkout", "-q", "main"]);
        std::fs::write(fx.repo.join("feature.txt"), "upstream\n").unwrap();
        git(&fx.repo, &["add", "feature.txt"]);
        git(&fx.repo, &["commit", "-qm", "upstream"]);
        git(&fx.repo, &["push", "-q", "origin", "main"]);
    }
    let base_sha = git(&fx.repo, &["rev-parse", "main"]);
    git(&fx.repo, &["checkout", "-q", "task/fixture"]);
    if conflict {
        let output = crate::git_test_support::command()
            .current_dir(&fx.repo)
            .args(["rebase", "main"])
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "expected rebase conflict on feature.txt"
        );
    }
    (base_sha, candidate_sha)
}

fn install_activities(runtime: &orbit_core::OrbitRuntime) {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/activities");
    let activities = runtime.global_root().join("resources/activities");
    std::fs::create_dir_all(&activities).unwrap();
    for name in [
        "git_rebase.yaml",
        "candidate_validate.yaml",
        "pr_conflict_recovery.yaml",
        "step_failure_recovery.yaml",
        "pr_failure_handoff.yaml",
        "final_recovery.yaml",
    ] {
        std::fs::copy(assets.join(name), activities.join(name)).unwrap();
    }
}

fn setup_fake_codex(fx: &super::provider_failure_hold::Fixture, marker: &Path, exit_code: i32) {
    install_activities(&fx.runtime);
    let frame = json!({"type": "turn.failed", "error": {"message": "editor ran"}});
    let cli = FakeCodex::named(&fx.repo, "codex", &frame.to_string());
    std::fs::write(
        &cli.path,
        format!(
            "#!/bin/sh\ntouch '{}'\ncat > /dev/null\ncat <<'STDOUT'\n{frame}\nSTDOUT\nexit {exit_code}\n",
            marker.display()
        ),
    )
    .unwrap();
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut executor: ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/codex.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(cli.path.display().to_string());
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    fx.runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();
}

#[test]
fn step_recovery_busy_deferred_footprint_holds_the_run_and_requeues_the_task() {
    if !super::dispatch_admission::isolated(
        "footprint_hold::step_recovery_busy_deferred_footprint_holds_the_run_and_requeues_the_task",
    ) {
        return;
    }
    for recovery_activity in [
        PR_CONFLICT_RECOVERY_ACTIVITY,
        STEP_FAILURE_RECOVERY_ACTIVITY,
    ] {
        let conflict = recovery_activity == PR_CONFLICT_RECOVERY_ACTIVITY;
        let validation_rule = if conflict {
            ""
        } else {
            "required_validation_commands = [\"test ! -f candidate-broken.txt\"]\n"
        };
        let fx = fixture(&format!(
            "[workflow]\n\
             default_crew = \"fixture\"\n\
             medium_complexity_crews = [\"fixture\"]\n\
             {validation_rule}\n\
             [workflow.purpose_crews]\n\
             failure_recovery = [\"fixture\"]\n\
             final_recovery = [\"fixture\"]\n\n\
             [crews.fixture]\n\
             provider = \"codex\"\n\
             model = \"fixture-model\"\n"
        ));
        let task = fx.task("fixture");
        let holder = fx.task("fixture");
        fx.runtime
            .apply_task_automation_update(
                &task,
                TaskAutomationUpdate {
                    execution_summary: Some(
                        "Outcome: success\nChanges: candidate ready.\nAssessment: ready."
                            .to_string(),
                    ),
                    ..Default::default()
                },
            )
            .unwrap();
        fx.runtime
            .apply_task_automation_update(
                &holder,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Review),
                    ..Default::default()
                },
            )
            .unwrap();

        let (base_sha, candidate_sha) = init_candidate_repo(&fx, conflict);
        let marker = fx.repo.join("editor-started");
        setup_fake_codex(&fx, &marker, 1);

        let jobs_dir = fx.runtime.global_root().join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        let step = if conflict {
            json!({
                "id": "sync_base",
                "recovery_activity": PR_CONFLICT_RECOVERY_ACTIVITY,
                "target": "activity:git_rebase",
                "default_input": {
                    "step_id": "sync_base",
                    "completed_task_ids": "{{ input.task_ids }}",
                    "workspace_path": fx.repo,
                    "head": "task/fixture",
                    "head_sha": candidate_sha,
                    "base": "main",
                    "base_ref": "refs/remotes/origin/main",
                    "base_sha": base_sha,
                    "sync_required": true,
                    "commits_behind": 1,
                }
            })
        } else {
            json!({
                "id": "validate",
                "recovery_activity": STEP_FAILURE_RECOVERY_ACTIVITY,
                "target": "activity:candidate_validate",
                "default_input": {
                    "completed_task_ids": "{{ input.task_ids }}",
                    "workspace_path": fx.repo,
                    "base_sha": base_sha,
                    "base_ref": "refs/remotes/origin/main",
                }
            })
        };
        std::fs::write(
            jobs_dir.join("task_pr_pipeline.yaml"),
            serde_json::to_string(&json!({
                "schemaVersion": 2,
                "kind": "Job",
                "metadata": {"name": "task_pr_pipeline"},
                "spec": {
                    "state": "enabled",
                    "kind": "workflow",
                    "failure_activity": "pr_failure_handoff",
                    "final_recovery_activity": "final_recovery",
                    "steps": [step],
                },
            }))
            .unwrap(),
        )
        .unwrap();

        let remote_head_before = git(&fx.repo, &["rev-parse", "origin/task/fixture"]);
        let local_head_before = git(&fx.repo, &["rev-parse", "HEAD"]);
        let status_before = git(&fx.repo, &["status", "--porcelain"]);

        let input = json!({
            "task_ids": [task],
            "crew": "fixture",
            "footprint_deferred": true,
            "footprint_wait_seconds": 0,
        });
        let run = fx
            .runtime
            .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input.clone()), None)
            .unwrap();
        fx.runtime
            .write_run_state(
                &run.run_id,
                &PipelineState::new(run.run_id.clone(), run.job_id.clone(), input),
            )
            .unwrap();
        fx.runtime
            .apply_task_automation_update(
                &task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::InProgress),
                    job_run_id: Some(run.run_id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();

        let _ = fx.runtime.execute_pipeline_run_worker(&run.run_id);

        let stored = fx.runtime.show_job_run(&run.run_id).unwrap();
        assert_eq!(
            stored.state,
            JobRunState::Held,
            "{recovery_activity}: expected Held, got {:?} steps={:?}",
            stored.state,
            stored.steps
        );
        let diagnostic = stored.steps.last().expect("run diagnostic");
        assert_eq!(
            diagnostic.error_code.as_deref(),
            Some(FOOTPRINT_UNAVAILABLE_ERROR_CODE),
            "{recovery_activity}: {diagnostic:?}"
        );
        assert!(
            diagnostic
                .error_message
                .as_deref()
                .unwrap()
                .contains(&holder),
            "{recovery_activity}: the held diagnostic names the holder: {diagnostic:?}"
        );
        assert!(
            !marker.exists(),
            "{recovery_activity}: neither recovery editor nor final_recovery may start"
        );
        assert_eq!(
            fx.status(&task),
            TaskStatus::Backlog,
            "{recovery_activity}: task must return to backlog"
        );
        let history = fx.runtime.get_task_history(&task).unwrap();
        assert!(
            history
                .iter()
                .any(|entry| entry.event == "footprint_unavailable_requeued"),
            "{recovery_activity}: expected footprint_unavailable_requeued in {history:?}"
        );

        let events: Vec<V2AuditEventKind> = fx
            .runtime
            .list_v2_audit_events(orbit_store::V2AuditEventFilter {
                workspace_id: fx.runtime.workspace_id().unwrap(),
                run_id: Some(run.run_id.clone()),
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .map(|row| {
                serde_json::from_str::<V2AuditEvent>(&row.payload_json)
                    .unwrap()
                    .kind
            })
            .collect();
        assert!(
            events.iter().any(|kind| matches!(
                kind,
                V2AuditEventKind::StepRecoveryAttempted {
                    recovery_activity: activity,
                    recovery_succeeded: false,
                    failure_phase: Some(phase),
                    error_message: Some(message),
                    ..
                } if activity == recovery_activity
                    && phase == "dispatch"
                    && message.contains(FOOTPRINT_UNAVAILABLE_ERROR_CODE)
            )),
            "{recovery_activity}: expected StepRecoveryAttempted dispatch footprint_unavailable in {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|kind| matches!(kind, V2AuditEventKind::FinalRecoveryAttempted { .. })),
            "{recovery_activity}: final_recovery must not run: {events:?}"
        );
        assert!(
            !events.iter().any(|kind| matches!(
                kind,
                V2AuditEventKind::ActivityStarted { activity_name, .. }
                    if activity_name == "final_recovery" || activity_name == "pr_failure_handoff"
            )),
            "{recovery_activity}: neither final_recovery nor pr_failure_handoff may run: {events:?}"
        );

        assert_eq!(
            git(&fx.repo, &["rev-parse", "origin/task/fixture"]),
            remote_head_before,
            "{recovery_activity}: published branch untouched"
        );
        assert_eq!(
            git(&fx.repo, &["rev-parse", "HEAD"]),
            local_head_before,
            "{recovery_activity}: local HEAD untouched"
        );
        assert_eq!(
            git(&fx.repo, &["status", "--porcelain"]),
            status_before,
            "{recovery_activity}: working tree untouched"
        );

        let deferred = fx
            .runtime
            .audit_failure_incidents(&orbit_store::contract::FailureIncidentQuery::default())
            .unwrap()
            .incidents
            .into_iter()
            .filter(|incident| {
                incident.surface == orbit_store::contract::RUN_DEFERRED_SURFACE
                    && incident.run_ids.contains(&run.run_id)
            })
            .map(|incident| incident.class)
            .collect::<Vec<_>>();
        assert_eq!(
            deferred,
            [orbit_store::contract::FailureClass::Deferred],
            "{recovery_activity}"
        );
    }
}

#[test]
fn non_footprint_and_stopped_run_recovery_dispatch_errors_preserve_existing_outcomes() {
    if !super::dispatch_admission::isolated(
        "footprint_hold::non_footprint_and_stopped_run_recovery_dispatch_errors_preserve_existing_outcomes",
    ) {
        return;
    }

    // 1. Ordinary non-FootprintUnavailable recovery dispatch error (e.g. a
    //    missing provider command) keeps the original step failure rather than
    //    holding.
    let fx = fixture(
        "[workflow]\n\
         default_crew = \"fixture\"\n\
         medium_complexity_crews = [\"fixture\"]\n\
         required_validation_commands = [\"test ! -f candidate-broken.txt\"]\n\n\
         [workflow.purpose_crews]\n\
         failure_recovery = [\"fixture\"]\n\n\
         [crews.fixture]\n\
         provider = \"codex\"\n\
         model = \"fixture-model\"\n",
    );
    let task = fx.task("fixture");
    let holder = fx.task("fixture");
    let stopped_task = fx.task("fixture");
    let (base_sha, _) = init_candidate_repo(&fx, false);
    install_activities(&fx.runtime);
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut executor: ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/codex.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(fx.repo.join("missing-codex").display().to_string());
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    fx.runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();

    let jobs_dir = fx.runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::write(
        jobs_dir.join("task_pr_pipeline.yaml"),
        serde_json::to_string(&json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "task_pr_pipeline"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [{
                    "id": "validate",
                    "recovery_activity": STEP_FAILURE_RECOVERY_ACTIVITY,
                    "target": "activity:candidate_validate",
                    "default_input": {
                        "completed_task_ids": "{{ input.task_ids }}",
                        "workspace_path": fx.repo,
                        "base_sha": base_sha,
                        "base_ref": "refs/remotes/origin/main",
                    }
                }],
            },
        }))
        .unwrap(),
    )
    .unwrap();

    let input = json!({
        "task_ids": [task],
        "crew": "fixture",
    });
    let run = fx
        .runtime
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input.clone()), None)
        .unwrap();
    fx.runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), input),
        )
        .unwrap();
    fx.runtime
        .apply_task_automation_update(
            &task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(run.run_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();

    let _ = fx.runtime.execute_pipeline_run_worker(&run.run_id);
    let stored = fx.runtime.show_job_run(&run.run_id).unwrap();
    assert_eq!(stored.state, JobRunState::Failed, "{:?}", stored.steps);
    let diagnostic = stored.steps.last().expect("run diagnostic");
    assert_ne!(
        diagnostic.error_code.as_deref(),
        Some(FOOTPRINT_UNAVAILABLE_ERROR_CODE)
    );
    assert!(
        diagnostic
            .error_message
            .as_deref()
            .unwrap()
            .contains("required validation"),
        "expected original candidate_validate error: {diagnostic:?}"
    );
    assert_eq!(fx.status(&task), TaskStatus::Blocked);

    // 2. A run stopped while step recovery waits for its deferred footprint
    //    returns the permanent run-stopped refusal, so the run keeps the
    //    original step failure rather than holding.
    fx.runtime
        .apply_task_automation_update(
            &holder,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();
    let stopped_input = json!({
        "task_ids": [stopped_task],
        "crew": "fixture",
        "footprint_deferred": true,
        "footprint_wait_seconds": 3600,
    });
    let stopped_run = fx
        .runtime
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(stopped_input.clone()),
            None,
        )
        .unwrap();
    fx.runtime
        .write_run_state(
            &stopped_run.run_id,
            &PipelineState::new(
                stopped_run.run_id.clone(),
                stopped_run.job_id.clone(),
                stopped_input.clone(),
            ),
        )
        .unwrap();
    fx.runtime
        .apply_task_automation_update(
            &stopped_task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(stopped_run.run_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    // Leave `stopped_run` in `Pending` (not `Running`) so
    // `acquire_deferred_footprint` sees a stopped run immediately without
    // sleeping.
    let mut job: JobV2 = serde_json::from_value(json!({
        "state": "enabled",
        "kind": "workflow",
        "steps": [{
            "id": "validate",
            "recovery_activity": STEP_FAILURE_RECOVERY_ACTIVITY,
            "target": "activity:candidate_validate",
            "default_input": {
                "completed_task_ids": "{{ input.task_ids }}",
                "workspace_path": fx.repo,
                "base_sha": base_sha,
                "base_ref": "refs/remotes/origin/main",
            }
        }],
    }))
    .unwrap();
    let catalog = fx.runtime.v2_activity_catalog().unwrap();
    orbit_engine::resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    let audit = V2AuditWriter::with_disk_sinks(
        &fx.repo.join("audit"),
        fx.runtime.stores().host_store().clone(),
        fx.runtime.workspace_id().unwrap(),
        &stopped_run.run_id,
        "fixture",
        Some(&fx.repo),
    )
    .unwrap();
    let err = execute_job_with_resume(
        &job,
        stopped_input,
        &stopped_run.run_id,
        audit.clone(),
        &fx.runtime,
        None,
    )
    .unwrap_err();
    assert!(
        !matches!(err, DispatchError::FootprintUnavailable(_)),
        "stopped run must not produce FootprintUnavailable: {err}"
    );
    assert!(
        err.to_string().contains("required validation"),
        "expected original step error on stopped-run recovery refusal: {err}"
    );
    let stopped_events: Vec<V2AuditEventKind> = audit
        .events_snapshot()
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect();
    assert!(
        stopped_events.iter().any(|kind| matches!(
            kind,
            V2AuditEventKind::StepRecoveryAttempted {
                recovery_activity,
                recovery_succeeded: false,
                failure_phase: Some(phase),
                error_message: Some(message),
                ..
            } if recovery_activity == STEP_FAILURE_RECOVERY_ACTIVITY
                && phase == "dispatch"
                && message.contains("run stopped")
        )),
        "expected run-stopped refusal recorded in StepRecoveryAttempted: {stopped_events:?}"
    );
}
