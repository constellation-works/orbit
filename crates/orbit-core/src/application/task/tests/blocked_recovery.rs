//! The blocked-task recovery backstop: which blocks it recovers, the one
//! decision per block episode, human suppression, the concurrency bound, and
//! the escalation of a recovery run that ended without a decision.

use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::git::run_git;
use orbit_engine::{
    RuntimeHost, TaskAutomationUpdate, blocked_workflow_failure_update,
    blocked_workflow_interruption_update,
};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_store::{JobRunStepParams, TaskReservationReleaseReason};
use orbit_types::task::{
    TASK_ENVELOPE_FILE_NAME, TASK_PLAN_FILE_NAME, Task, TaskComment, TaskEnvelopeV2, TaskStatus,
};
use orbit_types::workflow::{JobRunState, JobTargetType};
use serde_json::{Value, json};
use tempfile::tempdir;

use super::{assert_isolated_child, enter_isolated_child};
use crate::OrbitRuntime;
use crate::application::task::blocked_recovery::ATTRIBUTION_SLACK_MS;
use crate::application::task::{
    BLOCKED_TASK_RECOVERY_JOB, BlockedRecoveryInput, BlockedRecoveryPreparation,
    BlockedRecoveryTick, EpisodeDisposition, FinalRecoveryOutcome, FinalRecoveryRecord,
    MAX_ACTIVE_BLOCKED_RECOVERIES, TaskAddParams, TaskUpdateParams,
};

/// A runtime whose repository has one commit on `main` and whose
/// `workflow.final_recovery_crews` is `pool`.
fn recovery_runtime(pool: &str) -> (tempfile::TempDir, OrbitRuntime) {
    assert_isolated_child();
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    std::fs::write(
        workspace_root.join("config.toml"),
        format!(
            r#"
[workflow]
default_crew = "implementer"
final_recovery_crews = {pool}

[crews.implementer]
model = "implementer-model"
provider = "codex"
backend = "cli"
"#
        ),
    )
    .expect("write crew config");
    commit_on_main(&repo_root);
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}

fn commit_on_main(repo: &Path) {
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@orbit.invalid",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "base",
        ][..],
    ] {
        let output = run_git(repo, args).expect("run git");
        assert!(output.success, "git {args:?}: {}", output.stderr);
    }
}

fn in_progress_task(runtime: &OrbitRuntime, title: &str) -> Task {
    let task = runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: "Exercise the blocked-task recovery backstop.".to_string(),
            acceptance_criteria: vec!["One decision per block episode.".to_string()],
            plan: "1) implement 2) validate".to_string(),
            ..Default::default()
        })
        .expect("add task");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .expect("start task")
}

/// Block `task` the way run finalization does: a coupled `task_pr_pipeline`
/// run records a failing step and terminalizes as failed.
fn block_by_failed_run(runtime: &OrbitRuntime, task: &Task) -> String {
    let jobs = runtime.stores().jobs();
    let run = jobs
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_ids": [task.id]})),
            None,
        )
        .expect("insert delivery run");
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark run running");
    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                job_run_id: Some(run.run_id.clone()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("couple task to run");
    let now = Utc::now();
    jobs.complete_job_run_step(
        &run.run_id,
        &JobRunStepParams {
            step_index: 0,
            target_type: JobTargetType::Activity,
            target_id: "agent_implement".to_string(),
            started_at: now,
            finished_at: now,
            duration_ms: Some(1),
            exit_code: Some(1),
            agent_response_json: None,
            state: JobRunState::Failed,
            error_code: Some("STEP_FAILED".to_string()),
            error_message: Some("implementation failed its tests".to_string()),
        },
    )
    .expect("record failing step");
    finish_run(runtime, &run.run_id, JobRunState::Failed);
    run.run_id
}

fn block_with(runtime: &OrbitRuntime, task: &Task, update: TaskAutomationUpdate) {
    runtime
        .apply_task_automation_update(&task.id, update)
        .expect("block task");
}

fn claim_failed_update() -> TaskAutomationUpdate {
    TaskAutomationUpdate {
        status: Some(TaskStatus::Blocked),
        status_event: Some("claim_failed".to_string()),
        status_note: Some("claim settlement failed: run_id=jrun-claimed-leaf".to_string()),
        ..TaskAutomationUpdate::default()
    }
}

fn finish_run(runtime: &OrbitRuntime, run_id: &str, state: JobRunState) {
    let jobs = runtime.stores().jobs();
    if jobs
        .get_job_run(run_id)
        .expect("read run")
        .is_some_and(|run| run.state == JobRunState::Pending)
    {
        jobs.mark_job_run_running(run_id, Utc::now(), std::process::id())
            .expect("mark run running");
    }
    runtime
        .finalize_job_run_with_reservation_cleanup(
            run_id,
            state,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("finalize run");
}

/// One tick whose submissions are recorded as queued recovery runs instead of
/// spawning workers. Returns the submitted run inputs by run id.
fn tick(runtime: &OrbitRuntime) -> (BlockedRecoveryTick, Vec<Value>) {
    let mut inputs = Vec::new();
    let tick = runtime
        .run_blocked_task_recovery_tick_with(Utc::now(), &mut |input| {
            inputs.push(input.clone());
            runtime
                .stores()
                .jobs()
                .insert_job_run(BLOCKED_TASK_RECOVERY_JOB, 1, Utc::now(), Some(input), None)
                .map(|run| run.run_id)
        })
        .expect("run backstop tick");
    (tick, inputs)
}

fn source_of(input: &Value) -> &str {
    input["block_source"].as_str().expect("block source")
}

fn recovery_comments(runtime: &OrbitRuntime, task_id: &str) -> Vec<TaskComment> {
    runtime
        .get_task_comments(task_id)
        .expect("read comments")
        .into_iter()
        .filter(|comment| FinalRecoveryRecord::last(std::slice::from_ref(comment)).is_some())
        .collect()
}

#[test]
fn every_owned_block_source_gets_exactly_one_recovery_within_the_concurrency_bound() {
    if !enter_isolated_child(
        module_path!(),
        "every_owned_block_source_gets_exactly_one_recovery_within_the_concurrency_bound",
    ) {
        return;
    }
    let (_root, runtime) = recovery_runtime(r#"["implementer"]"#);

    let run_failed = in_progress_task(&runtime, "Leaf run failed");
    let failed_run_id = block_by_failed_run(&runtime, &run_failed);
    let gate = in_progress_task(&runtime, "Gate failed before its leaf");
    block_with(
        &runtime,
        &gate,
        blocked_workflow_failure_update(
            "task_gate_pipeline",
            "jrun-gate-1",
            Some("GATE"),
            Some("gate refused"),
        ),
    );
    let interrupted = in_progress_task(&runtime, "Worker died");
    block_with(
        &runtime,
        &interrupted,
        blocked_workflow_interruption_update("task_pr_pipeline", "jrun-lost-1", None, None),
    );
    let claim = in_progress_task(&runtime, "Claim settlement failed");
    block_with(&runtime, &claim, claim_failed_update());
    let claimed_blocker = in_progress_task(&runtime, "Claimed implementer blocker");
    block_with(
        &runtime,
        &claimed_blocker,
        TaskAutomationUpdate {
            execution_summary: Some(format!(
                "Outcome: failed\nError: {} kind=environment toolchain unavailable",
                orbit_types::workflow::TASK_BLOCKED_BY_AGENT_MARKER,
            )),
            status: Some(TaskStatus::Blocked),
            status_event: Some("claim_failed".to_string()),
            // Claimed failure settlements put their diagnostic in the task
            // summary; the history event itself has no status note.
            ..TaskAutomationUpdate::default()
        },
    );
    // A block a human set by hand is not the backstop's.
    let manual = in_progress_task(&runtime, "Blocked by hand");
    runtime
        .update_task(
            &manual.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Blocked),
                ..Default::default()
            },
        )
        .expect("block by hand");

    let (first, first_inputs) = tick(&runtime);
    assert_eq!(first.skipped, None);
    assert_eq!(
        first.dispatched.len(),
        MAX_ACTIVE_BLOCKED_RECOVERIES,
        "the first tick fills the concurrency bound and no more"
    );
    let (held, _) = tick(&runtime);
    assert!(
        held.dispatched.is_empty(),
        "no dispatch while the bound is full: {held:?}"
    );

    for (_, run_id) in &first.dispatched {
        finish_run(&runtime, run_id, JobRunState::Success);
    }
    let (second, second_inputs) = tick(&runtime);
    assert_eq!(second.dispatched.len(), 2, "{second:?}");
    for (_, run_id) in &second.dispatched {
        finish_run(&runtime, run_id, JobRunState::Success);
    }
    let (third, _) = tick(&runtime);
    assert!(
        third.dispatched.is_empty() && third.settled.is_empty(),
        "every episode was recovered once already: {third:?}"
    );

    let inputs: Vec<&Value> = first_inputs.iter().chain(&second_inputs).collect();
    let by_task = |task: &Task| {
        let matching: Vec<&&Value> = inputs
            .iter()
            .filter(|input| input["task_id"] == json!(task.id))
            .collect();
        assert_eq!(matching.len(), 1, "one recovery for {}", task.title);
        *matching[0]
    };
    assert_eq!(source_of(by_task(&run_failed)), "run_failed");
    assert_eq!(by_task(&run_failed)["failed_run_id"], json!(failed_run_id));
    assert_eq!(source_of(by_task(&gate)), "gate_failed");
    assert_eq!(source_of(by_task(&interrupted)), "run_interrupted");
    assert_eq!(source_of(by_task(&claim)), "claim_failed");
    assert!(
        inputs
            .iter()
            .all(|input| input["task_id"] != json!(claimed_blocker.id)),
        "a claimed implementer blocker does not dispatch another recovery agent"
    );
    assert!(
        inputs
            .iter()
            .all(|input| input["task_id"] != json!(manual.id)),
        "a hand-set block is never recovered"
    );
}

#[test]
fn activity_by_a_human_after_the_block_suppresses_recovery() {
    if !enter_isolated_child(
        module_path!(),
        "activity_by_a_human_after_the_block_suppresses_recovery",
    ) {
        return;
    }
    let (_root, runtime) = recovery_runtime(r#"["implementer"]"#);
    let touched = in_progress_task(&runtime, "Operator commented after the block");
    let automated = in_progress_task(&runtime, "Only automation wrote after the block");
    for task in [&touched, &automated] {
        runtime
            .update_task(
                &task.id,
                TaskUpdateParams {
                    comment: Some("before the block".to_string()),
                    ..Default::default()
                },
            )
            .expect("comment before the block");
        block_with(&runtime, task, claim_failed_update());
    }
    block_with(
        &runtime,
        &automated,
        TaskAutomationUpdate {
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "claude".to_string(),
                message: "agent note".to_string(),
            }],
            ..TaskAutomationUpdate::default()
        },
    );
    runtime
        .update_task(
            &touched.id,
            TaskUpdateParams {
                comment: Some("I'll take this one".to_string()),
                ..Default::default()
            },
        )
        .expect("human comment after the block");

    let views = runtime.blocked_recovery_view(Utc::now()).expect("view");
    let disposition = |task: &Task| {
        views
            .iter()
            .find(|view| view.episode.task_id == task.id)
            .map(|view| view.disposition.clone())
            .expect("blocked task in view")
    };
    assert!(
        matches!(
            disposition(&touched),
            EpisodeDisposition::HumanIntervened { .. }
        ),
        "{:?}",
        disposition(&touched)
    );
    assert_eq!(disposition(&automated), EpisodeDisposition::Eligible);

    let (tick, _) = tick(&runtime);
    assert_eq!(
        tick.dispatched
            .iter()
            .map(|(task_id, _)| task_id.as_str())
            .collect::<Vec<_>>(),
        vec![automated.id.as_str()]
    );
}

const EDITED_PLAN: &str = "1) fix the fixture by hand 2) requeue";

/// ORB-14228: modern field edits have attributed semantic history. Exercise
/// the dashboard's human surface rather than relying on the process actor.
#[test]
fn a_human_field_edit_after_the_block_is_never_overridden() {
    if !enter_isolated_child(
        module_path!(),
        "a_human_field_edit_after_the_block_is_never_overridden",
    ) {
        return;
    }
    field_edit_after_the_block_is_never_overridden(|runtime, task| {
        let history = runtime.get_task_history(&task.id).expect("history");
        let comments = runtime.get_task_comments(&task.id).expect("comments");
        runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    plan: Some(EDITED_PLAN.to_string()),
                    ..Default::default()
                },
                "human:recovery-operator".to_string(),
            )
            .expect("human edits the plan");
        let updated_history = runtime.get_task_history(&task.id).expect("durable history");
        assert_eq!(updated_history.len(), history.len() + 1);
        assert_eq!(&updated_history[..history.len()], history.as_slice());
        let event = updated_history.last().expect("semantic update event");
        assert_eq!(event.event, "updated");
        assert_eq!(event.by, "human:recovery-operator");
        assert!(event.at > history.last().expect("block history").at);
        assert_eq!(
            runtime.get_task_comments(&task.id).expect("comments"),
            comments
        );
        EpisodeDisposition::HumanIntervened {
            by: event.by.clone(),
        }
    });
}

/// Fault injection in an isolated bundle models a legacy client that wrote
/// the plan and timestamp without history. The modern API cannot model this.
#[test]
fn a_legacy_unattributed_field_edit_after_the_block_is_never_overridden() {
    if !enter_isolated_child(
        module_path!(),
        "a_legacy_unattributed_field_edit_after_the_block_is_never_overridden",
    ) {
        return;
    }
    field_edit_after_the_block_is_never_overridden(|runtime, task| {
        assert_isolated_child();
        let history = runtime.get_task_history(&task.id).expect("history");
        let comments = runtime.get_task_comments(&task.id).expect("comments");
        let registry = TaskRegistryStore::open(&task_registry_path(&runtime.global_root()))
            .expect("isolated task registry");
        let bundle = registry
            .canonical_task_bundle_path(&runtime.workspace_id().expect("workspace"), &task.id)
            .expect("isolated bundle");
        let envelope_path = bundle.join(TASK_ENVELOPE_FILE_NAME);
        let mut envelope: TaskEnvelopeV2 =
            serde_yaml::from_slice(&std::fs::read(&envelope_path).expect("read envelope"))
                .expect("parse envelope");
        // Pass the timestamp slack since every attributed write; record no
        // event or comment, just as the old field-only writer did.
        let slack = u64::try_from(ATTRIBUTION_SLACK_MS).expect("non-negative slack");
        std::thread::sleep(std::time::Duration::from_millis(slack + 100));
        envelope.updated_at = Utc::now();
        std::fs::write(bundle.join(TASK_PLAN_FILE_NAME), EDITED_PLAN).expect("legacy plan write");
        std::fs::write(
            envelope_path,
            serde_yaml::to_string(&envelope).expect("serialize envelope"),
        )
        .expect("legacy timestamp write");
        assert_eq!(
            runtime.get_task_history(&task.id).expect("history"),
            history
        );
        assert_eq!(
            runtime.get_task_comments(&task.id).expect("comments"),
            comments
        );
        EpisodeDisposition::UnexplainedChange {
            changed_at: envelope.updated_at,
        }
    });
}

/// Deterministically interleave a field edit on each side of dispatch and
/// exercise both recovery steps against the changed durable task revision.
fn field_edit_after_the_block_is_never_overridden(
    edit_plan: impl Fn(&OrbitRuntime, &Task) -> EpisodeDisposition,
) {
    let (_root, runtime) = recovery_runtime(r#"["implementer"]"#);
    let edited = in_progress_task(&runtime, "Operator rewrote the plan before dispatch");
    let dispatched = in_progress_task(&runtime, "Operator rewrote the plan after dispatch");
    block_by_failed_run(&runtime, &edited);
    block_with(&runtime, &dispatched, claim_failed_update());
    let disposition = |task: &Task| {
        runtime
            .blocked_recovery_view(Utc::now())
            .expect("view")
            .into_iter()
            .find(|view| view.episode.task_id == task.id)
            .map(|view| view.disposition)
            .expect("blocked task in view")
    };
    assert_eq!(disposition(&edited), EpisodeDisposition::Eligible);
    assert_eq!(disposition(&dispatched), EpisodeDisposition::Eligible);
    let expected = edit_plan(&runtime, &edited);
    assert_eq!(disposition(&edited), expected);

    let (first, inputs) = tick(&runtime);
    assert_eq!(
        first
            .dispatched
            .iter()
            .map(|(task_id, _)| task_id.as_str())
            .collect::<Vec<_>>(),
        vec![dispatched.id.as_str()],
        "the edited task is held for the human"
    );

    // Once dispatched, the same edit makes both steps of the run stand down.
    let input = BlockedRecoveryInput::from_json(&inputs[0]).expect("run input");
    let run_id = first.dispatched[0].1.clone();
    let expected = edit_plan(&runtime, &dispatched);
    assert_eq!(disposition(&dispatched), expected);
    assert!(matches!(
        runtime
            .prepare_blocked_task_recovery(&input, &run_id)
            .expect("prepare"),
        BlockedRecoveryPreparation::Skip { .. }
    ));
    let outcome = runtime
        .apply_blocked_task_recovery(
            &input,
            &run_id,
            "main",
            Some(&json!({"decision": "requeue", "reason": "retry"})),
        )
        .expect("apply");
    assert!(
        matches!(outcome, FinalRecoveryOutcome::Refused { .. }),
        "{outcome:?}"
    );
    for task in [&edited, &dispatched] {
        let task = runtime.get_task(&task.id).expect("task");
        assert_eq!(task.status, TaskStatus::Blocked);
        assert_eq!(task.plan, EDITED_PLAN);
    }
    let (held, inputs) = tick(&runtime);
    assert!(held.dispatched.is_empty() && inputs.is_empty());
}

#[test]
#[allow(clippy::print_stdout)] // Report unavailable native sandbox checks visibly.
fn a_recovery_applies_one_decision_recorded_with_its_run_id() {
    if !enter_isolated_child(
        module_path!(),
        "a_recovery_applies_one_decision_recorded_with_its_run_id",
    ) {
        return;
    }
    let (_root, runtime) = recovery_runtime(r#"["implementer"]"#);
    let requeued = in_progress_task(&runtime, "Recovered by requeue");
    block_by_failed_run(&runtime, &requeued);
    let resumed = in_progress_task(&runtime, "Agent proposed resume");
    block_with(&runtime, &resumed, claim_failed_update());

    let (tick_result, inputs) = tick(&runtime);
    assert_eq!(tick_result.dispatched.len(), 2);
    let run_of = |task: &Task| {
        let index = inputs
            .iter()
            .position(|input| input["task_id"] == json!(task.id))
            .expect("dispatched");
        (
            BlockedRecoveryInput::from_json(&inputs[index]).expect("run input"),
            tick_result.dispatched[index].1.clone(),
        )
    };

    let (input, run_id) = run_of(&requeued);
    let BlockedRecoveryPreparation::Ready(prepared) = runtime
        .prepare_blocked_task_recovery(&input, &run_id)
        .expect("prepare")
    else {
        panic!("a current episode is prepared");
    };
    assert_eq!(prepared.failed_step_id, "agent_implement");
    assert_eq!(prepared.error_message, "implementation failed its tests");
    assert_eq!(prepared.base_ref, "main");
    assert!(
        prepared.checkout.join(".git").exists(),
        "the agent gets a checkout of the base"
    );
    let denied_root = prepared.checkout.join(".orbit");
    assert!(
        denied_root.is_dir(),
        "recovery preparation must supply the sandbox's deny root before the agent step"
    );
    assert_eq!(
        std::fs::read_dir(&denied_root).expect("deny root").count(),
        0,
        "the recovery checkout must not contain copied runtime stores"
    );
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::process::Stdio;

        use orbit_exec::linux_bwrap_write_grant_diagnostic;
        use orbit_types::workflow::ExecutorSandboxKind;

        let kind = if cfg!(target_os = "linux") {
            ExecutorSandboxKind::LinuxBwrap
        } else {
            ExecutorSandboxKind::MacosSandboxExec
        };
        crate::adapter::engine_host::v2_host::test_support::seed_executor(
            &runtime,
            "claude",
            Some(kind),
        );
        let sandbox = runtime
            .resolve_executor_sandbox("claude", None, Some(&prepared.checkout))
            .expect("resolve recovery sandbox")
            .expect("native sandbox");
        assert!(
            linux_bwrap_write_grant_diagnostic(
                &sandbox.fs_profile,
                &prepared.checkout.join("source.rs")
            )
            .expect("source grant")
            .is_none(),
            "recovery must retain source writes on both backends"
        );
        for relative in [
            "state/new.json",
            "config.toml",
            "resources/new.json",
            "tmp/new.env",
        ] {
            assert!(
                linux_bwrap_write_grant_diagnostic(
                    &sandbox.fs_profile,
                    &denied_root.join(relative)
                )
                .expect("deny rule")
                .is_some(),
                "recovery's own .orbit tree must stay denied after runtime grants and policy exceptions"
            );
        }
        assert!(
            linux_bwrap_write_grant_diagnostic(
                &sandbox.fs_profile,
                &denied_root.join("tmp/log.txt")
            )
            .expect("scratch grant")
            .is_none(),
            "the required artifact scratch directory must stay writable"
        );
        orbit_common::fs::path::ensure_orbit_scratch_dir(&prepared.checkout)
            .expect("prepare scratch as the agent launcher does");
        let args = [
            "-c".to_string(),
            "set -eu; printf reached > agent-step.txt; printf scratch > .orbit/tmp/agent.log"
                .to_string(),
        ];
        #[cfg(target_os = "linux")]
        {
            orbit_exec::prepare_linux_bwrap_write_grants(&sandbox.fs_profile, &prepared.checkout)
                .expect("prepare the same write anchors as the agent launcher");
            let mut plan = orbit_exec::compile_linux_bwrap_argv(
                &sandbox.fs_profile,
                "/bin/sh",
                &args,
                Some(&prepared.checkout),
                sandbox.managed_worktree,
            )
            .expect(
                "the effective recovery profile must reach the agent step without a deny refusal",
            );
            let guard = plan
                .take_post_run_guard()
                .expect("future filename denies need a post-run guard");
            let probe = orbit_exec::probe_bwrap();
            if probe.available {
                let child =
                    orbit_exec::spawn_under_linux_bwrap(orbit_exec::LinuxBwrapSpawnRequest {
                        plan: &plan,
                        env: &[],
                        cwd: Some(&prepared.checkout),
                        stdin: Stdio::null(),
                        stdout: Stdio::piped(),
                        stderr: Stdio::piped(),
                    })
                    .expect("spawn the recovery agent step");
                let outcome = orbit_exec::supervise_child(child, Some(30_000), None)
                    .expect("bounded recovery agent step");
                assert!(outcome.result.success, "agent step: {:?}", outcome.result);
                guard.verify().expect("allowed source and scratch writes");
            } else {
                orbit_exec::report_bwrap_deferral("recovery agent kernel launch", &probe.detail);
            }
            std::fs::write(prepared.checkout.join("new.env"), "forbidden")
                .expect("simulate an agent write");
            assert!(
                matches!(guard.verify(), Err(OrbitError::PolicyDenied(_))),
                "recovery must reject newly created files covered by the effective policy's deny globs"
            );
        }
        #[cfg(target_os = "macos")]
        {
            let profile_text =
                orbit_exec::compile_macos_sandbox_profile(&sandbox.fs_profile, "claude")
                    .expect("compile the effective recovery sandbox");
            if orbit_exec::sandbox_exec_available() {
                let (child, _profile_file) =
                    orbit_exec::spawn_under_macos_sandbox(orbit_exec::MacosSandboxSpawnRequest {
                        profile_text: &profile_text,
                        program: "/bin/sh",
                        args: &args,
                        env: &[],
                        cwd: Some(&prepared.checkout),
                        stdin: Stdio::null(),
                        stdout: Stdio::piped(),
                        stderr: Stdio::piped(),
                        inherited_fds: &[],
                    })
                    .expect("spawn the recovery agent step");
                let outcome = orbit_exec::supervise_child(child, Some(30_000), None)
                    .expect("bounded recovery agent step");
                assert!(outcome.result.success, "agent step: {:?}", outcome.result);
            } else {
                assert_ne!(
                    std::env::var("ORBIT_REQUIRE_SANDBOX_EXEC").as_deref(),
                    Ok("1"),
                    "native recovery launch must run on the admitted macOS CI host"
                );
                println!("SKIP: recovery agent kernel launch: sandbox-exec is unavailable");
            }
        }
    }
    let outcome = runtime
        .apply_blocked_task_recovery(
            &input,
            &run_id,
            &prepared.base_ref,
            Some(&json!({"decision": "requeue", "reason": "the flaky dependency was fixed"})),
        )
        .expect("apply");
    assert_eq!(outcome, FinalRecoveryOutcome::Requeued);
    runtime
        .remove_recovery_checkout(&run_id)
        .expect("remove checkout");
    assert!(!prepared.checkout.exists());
    assert_eq!(
        runtime.get_task(&requeued.id).expect("task").status,
        TaskStatus::Backlog
    );
    let record = FinalRecoveryRecord::last(&recovery_comments(&runtime, &requeued.id))
        .expect("decision comment");
    assert_eq!(
        (record.run_id.as_str(), record.decision.as_str()),
        (run_id.as_str(), "requeue")
    );

    // The backstop never resumes: a resume proposal is escalated, and the
    // escalated episode is not recovered again.
    let (input, run_id) = run_of(&resumed);
    let outcome = runtime
        .apply_blocked_task_recovery(
            &input,
            &run_id,
            "main",
            Some(&json!({"decision": "resume", "step_id": "implement", "rationale": "retry"})),
        )
        .expect("apply resume");
    assert!(
        matches!(outcome, FinalRecoveryOutcome::Escalated { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        runtime.get_task(&resumed.id).expect("task").status,
        TaskStatus::Blocked
    );
    for (_, run_id) in &tick_result.dispatched {
        finish_run(&runtime, run_id, JobRunState::Success);
    }
    let (again, _) = tick(&runtime);
    assert!(again.dispatched.is_empty(), "{again:?}");
    assert_eq!(recovery_comments(&runtime, &resumed.id).len(), 1);

    // A task that moved since dispatch is not prepared.
    let moved = in_progress_task(&runtime, "Moved after dispatch");
    block_with(&runtime, &moved, claim_failed_update());
    let (moved_tick, moved_inputs) = tick(&runtime);
    let input = BlockedRecoveryInput::from_json(&moved_inputs[0]).expect("run input");
    runtime
        .update_task(
            &moved.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
        )
        .expect("operator requeues");
    assert!(matches!(
        runtime
            .prepare_blocked_task_recovery(&input, &moved_tick.dispatched[0].1)
            .expect("prepare"),
        BlockedRecoveryPreparation::Skip { .. }
    ));
}

#[test]
fn a_recovery_run_that_ends_without_a_decision_is_escalated_once() {
    if !enter_isolated_child(
        module_path!(),
        "a_recovery_run_that_ends_without_a_decision_is_escalated_once",
    ) {
        return;
    }
    let (_root, runtime) = recovery_runtime(r#"["implementer"]"#);
    let task = in_progress_task(&runtime, "Recovery agent crashed");
    block_by_failed_run(&runtime, &task);

    let (first, inputs) = tick(&runtime);
    let run_id = first.dispatched[0].1.clone();
    finish_run(&runtime, &run_id, JobRunState::Failed);
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Blocked,
        "a failed recovery run does not open a new block episode"
    );

    // A resumed attempt of the same episode owns the decision while it runs.
    let resumed = runtime
        .stores()
        .jobs()
        .insert_job_run(
            BLOCKED_TASK_RECOVERY_JOB,
            2,
            Utc::now(),
            Some(inputs[0].clone()),
            Some(run_id.clone()),
        )
        .expect("insert resumed attempt");
    let (held, _) = tick(&runtime);
    assert!(
        held.settled.is_empty() && held.dispatched.is_empty(),
        "{held:?}"
    );
    finish_run(&runtime, &resumed.run_id, JobRunState::Failed);

    let (settle, _) = tick(&runtime);
    assert_eq!(settle.settled, vec![task.id.clone()]);
    assert!(settle.dispatched.is_empty());
    let (after, _) = tick(&runtime);
    assert!(after.settled.is_empty() && after.dispatched.is_empty());

    let comments = recovery_comments(&runtime, &task.id);
    assert_eq!(comments.len(), 1, "one decision for the episode");
    let record = FinalRecoveryRecord::last(&comments).expect("decision");
    assert_eq!(record.decision, "escalate");
    assert!(
        [run_id.as_str(), resumed.run_id.as_str()].contains(&record.run_id.as_str()),
        "the decision names one of the episode's runs: {record:?}"
    );
}

#[test]
fn requeues_share_the_final_recovery_bound_across_episodes() {
    if !enter_isolated_child(
        module_path!(),
        "requeues_share_the_final_recovery_bound_across_episodes",
    ) {
        return;
    }
    let (_root, runtime) = recovery_runtime(r#"["implementer"]"#);
    let task = in_progress_task(&runtime, "Keeps failing");
    let mut outcomes = Vec::new();
    for _ in 0..3 {
        if runtime.get_task(&task.id).expect("task").status != TaskStatus::InProgress {
            runtime
                .update_task(
                    &task.id,
                    TaskUpdateParams {
                        status: Some(TaskStatus::InProgress),
                        ..Default::default()
                    },
                )
                .expect("restart task");
        }
        block_with(&runtime, &task, claim_failed_update());
        let (tick, inputs) = tick(&runtime);
        assert_eq!(tick.dispatched.len(), 1, "each new episode is recovered");
        let input = BlockedRecoveryInput::from_json(&inputs[0]).expect("run input");
        outcomes.push(
            runtime
                .apply_blocked_task_recovery(
                    &input,
                    &tick.dispatched[0].1,
                    "main",
                    Some(&json!({"decision": "requeue", "reason": "try again"})),
                )
                .expect("apply"),
        );
        finish_run(&runtime, &tick.dispatched[0].1, JobRunState::Success);
    }
    assert_eq!(
        outcomes[..2],
        [
            FinalRecoveryOutcome::Requeued,
            FinalRecoveryOutcome::Requeued
        ]
    );
    assert!(
        matches!(outcomes[2], FinalRecoveryOutcome::Escalated { .. }),
        "the third requeue in a day exceeds the shared bound: {:?}",
        outcomes[2]
    );
}

#[test]
fn an_empty_crew_pool_turns_the_backstop_off() {
    if !enter_isolated_child(module_path!(), "an_empty_crew_pool_turns_the_backstop_off") {
        return;
    }
    let (_root, runtime) = recovery_runtime("[]");
    let task = in_progress_task(&runtime, "Blocked with recovery off");
    block_by_failed_run(&runtime, &task);
    let tick = runtime
        .run_blocked_task_recovery_tick_with(Utc::now(), &mut |_| -> Result<String, OrbitError> {
            panic!("nothing is submitted with an empty pool")
        })
        .expect("tick");
    assert!(tick.skipped.is_some());
    assert!(tick.dispatched.is_empty());
    assert!(runtime.blocked_task_recovery_disabled_reason().is_some());
}

#[test]
fn a_recovery_checkout_never_resolves_outside_its_directory() {
    if !enter_isolated_child(
        module_path!(),
        "a_recovery_checkout_never_resolves_outside_its_directory",
    ) {
        return;
    }
    let (_root, runtime) = recovery_runtime(r#"["implementer"]"#);
    let state_dir = runtime.paths().state_dir.clone();
    let outside = state_dir.join("outside");
    std::fs::create_dir_all(outside.join("keep")).expect("create outside dir");
    let head = run_git(&runtime.paths().repo_root, &["rev-parse", "HEAD"]).expect("rev-parse");
    let head = head.stdout.trim().to_string();

    for hostile in ["../outside", "..", "a/../../outside", "/tmp", ""] {
        assert!(
            matches!(
                runtime.remove_recovery_checkout(hostile),
                Err(OrbitError::InvalidInput(_))
            ),
            "removal must refuse run id {hostile:?}"
        );
        assert!(
            matches!(
                runtime.create_recovery_checkout(hostile, &head),
                Err(OrbitError::InvalidInput(_))
            ),
            "creation must refuse run id {hostile:?}"
        );
    }
    assert!(
        outside.join("keep").is_dir(),
        "a refused run id leaves directories outside the recovery checkouts alone"
    );

    let checkout = runtime
        .create_recovery_checkout("jrun-legit_1", &head)
        .expect("create checkout");
    let checkouts = state_dir
        .join("recovery-checkouts")
        .canonicalize()
        .expect("checkouts dir");
    assert_eq!(checkout.parent(), Some(checkouts.as_path()));
    runtime
        .remove_recovery_checkout("jrun-legit_1")
        .expect("remove checkout");
    assert!(!checkout.exists());
}

/// [F2026-10-159] Recovering a task whose run lost its worker, the backstop's
/// agent was offered `resume` and in-checkout implementation it cannot apply,
/// and nothing named the failed run's worktree that still held the work. The
/// shipped pipeline runs here end to end against a substitute provider that
/// records the envelope it was given and still proposes `resume`.
#[test]
#[cfg(unix)]
fn the_backstop_offers_only_its_decisions_and_names_the_retained_candidate() {
    use std::os::unix::fs::PermissionsExt;

    use orbit_types::workflow::ExecutorSandboxKind;

    use crate::adapter::engine_host::v2_host::test_support::seed_executor;
    use crate::application::task::BACKSTOP_DECISIONS;
    use crate::bootstrap::init::{InitOptions, init_workspace_at_root};

    if !enter_isolated_child(
        module_path!(),
        "the_backstop_offers_only_its_decisions_and_names_the_retained_candidate",
    ) {
        return;
    }
    let (root, runtime) = recovery_runtime(r#"["implementer"]"#);
    init_workspace_at_root(
        &runtime.global_root(),
        InitOptions {
            global_only: true,
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .expect("seed the shipped recovery job and activities");
    let runtime = OrbitRuntime::from_roots(
        &runtime.global_root(),
        &runtime.paths().repo_root.join(".orbit"),
    )
    .expect("reopen with the shipped catalog");
    let repo = runtime.paths().repo_root.clone();

    // The failed run's worktree, where it was created, holding two
    // uncommitted files.
    let task = in_progress_task(&runtime, "Worker lost mid-implementation");
    let failed_run_id = block_by_failed_run(&runtime, &task);
    let failed_run = runtime
        .get_job_run_backend(&failed_run_id)
        .expect("read failed run")
        .expect("failed run");
    let worktree =
        orbit_engine::run_worktree_paths(&repo, &failed_run).expect("worktree")[0].clone();
    let target = worktree.to_string_lossy().to_string();
    let added = run_git(
        &repo,
        &["worktree", "add", "--detach", "--quiet", &target, "HEAD"],
    )
    .expect("git worktree add");
    assert!(added.success, "{}", added.stderr);
    std::fs::write(worktree.join("candidate.rs"), "pub fn done() {}\n").expect("write file");
    std::fs::write(worktree.join("candidate_test.rs"), "#[test]\nfn t() {}\n").expect("write");
    let git_dir = run_git(&worktree, &["rev-parse", "--absolute-git-dir"]).expect("git dir");
    let index = Path::new(git_dir.stdout.trim()).join("index");
    let index_before = std::fs::read(&index).expect("read index");
    let index_mtime = std::fs::metadata(&index)
        .and_then(|m| m.modified())
        .expect("mtime");

    let envelope = root.path().join("envelope.json");
    let provider = root.path().join("codex");
    std::fs::write(
        &provider,
        format!(
            "#!/bin/sh\ncat > '{}'\nprintf '%s\\n' '{}'\n",
            envelope.display(),
            r#"{"schemaVersion":1,"status":"success","result":{"decision":"resume","step_id":"agent_implement","rationale":"finished the implementation"},"error":null}"#
        ),
    )
    .expect("write provider");
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755))
        .expect("make provider executable");
    seed_executor(&runtime, "codex", Some(ExecutorSandboxKind::Off));
    let mut executor = runtime
        .get_executor_def("codex")
        .expect("read executor")
        .expect("executor");
    executor.command = Some(provider.to_string_lossy().to_string());
    executor.args = Vec::new();
    runtime
        .upsert_executor_def(&executor)
        .expect("point executor at the provider");

    let (_, inputs) = tick(&runtime);
    assert_eq!(inputs.len(), 1);
    let job = runtime
        .show_job_catalog_entry(BLOCKED_TASK_RECOVERY_JOB)
        .expect("shipped recovery job");
    let recovery = runtime
        .run_job_v2_from_yaml(&job.path, inputs[0].clone())
        .expect("run the recovery pipeline");
    assert!(recovery.success, "{recovery:#?}");

    let candidate = &recovery.pipeline["prepare"]["recovery"]["retained_candidate"];
    assert_eq!(candidate["status"], "present", "{candidate}");
    assert_eq!(candidate["path"], json!(target));
    assert_eq!(
        candidate["changed_paths"],
        json!(["candidate.rs", "candidate_test.rs"]),
        "{candidate}"
    );

    // What the agent was given: only the backstop's decisions, no step to
    // resume from, the lane's limits, and the candidate.
    let given = std::fs::read_to_string(&envelope).expect("provider saw an envelope");
    let (_, envelope_text) = given
        .split_once("Execution envelope:\n")
        .expect("the prompt embeds the envelope");
    let given: Value = serde_json::from_str(envelope_text.trim()).expect("envelope parses");
    let input = &given["input"];
    assert_eq!(input["decisions"], json!(BACKSTOP_DECISIONS));
    assert!(
        !input["decisions"]
            .as_array()
            .expect("decisions")
            .contains(&json!("resume")),
        "the backstop has no live run to resume: {input}"
    );
    assert!(input.get("step_ids").is_none(), "{input}");
    assert!(
        input["lane_contract"]
            .as_str()
            .is_some_and(|text| !text.trim().is_empty()),
        "{input}"
    );
    assert_eq!(&input["retained_candidate"], candidate);
    assert_ne!(input["workspace_path"], json!(target));

    // `resume` is applied as an escalation naming the failed run and its
    // retained candidate, which is left exactly as it was.
    assert_eq!(recovery.pipeline["apply"]["outcome"], "escalated");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Blocked
    );
    let comments = recovery_comments(&runtime, &task.id);
    let comment = &comments.last().expect("decision comment").message;
    for named in [
        failed_run_id.as_str(),
        target.as_str(),
        "candidate.rs",
        "candidate_test.rs",
    ] {
        assert!(comment.contains(named), "comment names {named}: {comment}");
    }
    assert!(worktree.join("candidate.rs").is_file());
    assert!(worktree.join("candidate_test.rs").is_file());
    assert_eq!(std::fs::read(&index).expect("read index"), index_before);
    assert_eq!(
        std::fs::metadata(&index)
            .and_then(|m| m.modified())
            .expect("mtime"),
        index_mtime,
        "identifying the candidate must not write its index"
    );
    assert!(
        !Path::new(input["workspace_path"].as_str().expect("checkout")).exists(),
        "the recovery checkout is removed"
    );
}

/// A failed run with no worktree left still gets a candidate entry saying
/// so, and `complete_no_diff` completes only on a commit reachable from the
/// base.
#[test]
fn complete_no_diff_needs_a_covering_commit_on_the_base() {
    if !enter_isolated_child(
        module_path!(),
        "complete_no_diff_needs_a_covering_commit_on_the_base",
    ) {
        return;
    }
    let (_root, runtime) = recovery_runtime(r#"["implementer"]"#);
    let repo = runtime.paths().repo_root.clone();
    let unreachable = in_progress_task(&runtime, "Claims an unlanded commit");
    let covered = in_progress_task(&runtime, "Already landed");
    block_by_failed_run(&runtime, &unreachable);
    block_by_failed_run(&runtime, &covered);
    for args in [
        &["checkout", "-q", "-b", "side"][..],
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@orbit.invalid",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "not on main",
        ][..],
    ] {
        let output = run_git(&repo, args).expect("run git");
        assert!(output.success, "git {args:?}: {}", output.stderr);
    }
    let side = run_git(&repo, &["rev-parse", "HEAD"]).expect("rev-parse");
    let main = run_git(&repo, &["rev-parse", "main"]).expect("rev-parse");

    let (tick_result, inputs) = tick(&runtime);
    assert_eq!(tick_result.dispatched.len(), 2);
    let apply = |task: &Task, commit: &str| {
        let index = inputs
            .iter()
            .position(|input| input["task_id"] == json!(task.id))
            .expect("dispatched");
        let input = BlockedRecoveryInput::from_json(&inputs[index]).expect("run input");
        let run_id = &tick_result.dispatched[index].1;
        let BlockedRecoveryPreparation::Ready(prepared) = runtime
            .prepare_blocked_task_recovery(&input, run_id)
            .expect("prepare")
        else {
            panic!("a current episode is prepared");
        };
        let outcome = runtime
            .apply_blocked_task_recovery(
                &input,
                run_id,
                &prepared.base_ref,
                Some(&json!({
                    "decision": "complete_no_diff",
                    "evidence_commit": commit,
                    "rationale": "the outcome is on the base",
                })),
            )
            .expect("apply");
        runtime
            .remove_recovery_checkout(run_id)
            .expect("remove checkout");
        (prepared, outcome)
    };

    let (prepared, outcome) = apply(&unreachable, side.stdout.trim());
    assert!(
        matches!(
            &prepared.retained_candidate,
            crate::application::task::retained_candidate::RetainedCandidate::Absent { .. }
        ),
        "{:?}",
        prepared.retained_candidate
    );
    assert!(
        matches!(outcome, FinalRecoveryOutcome::Escalated { .. }),
        "a commit off the base never completes the task: {outcome:?}"
    );
    assert_eq!(
        runtime.get_task(&unreachable.id).expect("task").status,
        TaskStatus::Blocked
    );

    let (_, outcome) = apply(&covered, main.stdout.trim());
    assert!(
        matches!(
            outcome,
            FinalRecoveryOutcome::Completed {
                status: TaskStatus::Review,
                ..
            }
        ),
        "{outcome:?}"
    );
}
