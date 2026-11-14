//! Reconciliation and an in-flight cancellation over the same dead owner
//! (ORB-21876).
//!
//! A cancelled worker is reaped before its canceller reads the run again. Any
//! `orbit` process reconciling in that gap saw a dead owner and wrote
//! `interrupted`, so an operator's cancel blocked the task for recovery
//! instead of returning it to the backlog. The signal seam places that read
//! exactly in the gap, against real processes and with no timing involved.

use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use chrono::Utc;
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRun, JobRunState, OPERATOR_STOPPED_ERROR_CODE};
use serde_json::json;

use super::super::WORKER_TERMINATED_ERROR_CODE;
use super::super::actions::{CANCELLATION_REQUEST_AUDIT, CancellationRequest};
use super::super::owner::signal_run_owner_process;
use super::super::root_cause::{RunFailureContextProfile, RunFailureRead};
use super::super::types::JobRunCancelOutcome;
use super::drain_cancel::in_progress_task;
use super::{insert_pending_run, test_runtime};
use crate::OrbitRuntime;
use crate::application::job::pipeline::WORKER_EXIT_AUDIT;
use crate::application::tests::run_isolated_test;

/// A process alone in its own group, as a pipeline worker runs.
fn spawn_process() -> Child {
    let mut command = Command::new("sleep");
    command
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command.spawn().expect("spawn process")
}

/// `run` owned by `owner`, past the startup grace that shields a new owner.
fn mark_running(runtime: &OrbitRuntime, run: &JobRun, owner: &Child) -> JobRun {
    let jobs = runtime.stores().jobs();
    jobs.mark_job_run_running(
        &run.run_id,
        Utc::now() - chrono::Duration::seconds(30),
        owner.id(),
    )
    .expect("mark run running");
    jobs.get_job_run(&run.run_id)
        .expect("read run")
        .expect("run exists")
}

#[test]
fn reconciling_a_reaped_worker_mid_cancel_leaves_the_outcome_to_the_canceller() {
    if run_isolated_test(std::any::type_name_of_val(
        &reconciling_a_reaped_worker_mid_cancel_leaves_the_outcome_to_the_canceller,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "task_pr_pipeline");
    let owner = spawn_process();
    let owner_pid = owner.id();
    let running = mark_running(&runtime, &run, &owner);
    let task_id = in_progress_task(&runtime, &running);
    let mut bystander = spawn_process();
    // The worker's supervisor: reaping is what makes the owner read as gone.
    let supervisor = std::thread::spawn(move || {
        let mut owner = owner;
        owner.wait().expect("reap owner")
    });

    let result = runtime
        .cancel_job_run_with_options_and_signal(
            &run.run_id,
            CancellationRequest {
                actor: "dashboard",
                source: "web",
                reason: None,
                block_task: false,
                keep_candidate: false,
            },
            false,
            |target| {
                let outcome = signal_run_owner_process(target)?;
                supervisor.join().expect("supervisor thread");
                let observed = runtime.show_job_run(&target.run_id)?;
                assert_eq!(
                    observed.state,
                    JobRunState::Running,
                    "a reconciling reader must not finalize a run being cancelled"
                );
                Ok(outcome)
            },
        )
        .expect("cancel run");

    assert_eq!(result.outcome, JobRunCancelOutcome::Cancelled);
    assert_eq!(result.final_state, "cancelled");
    assert_eq!(
        result.signal_outcome.as_deref(),
        Some("terminated_process_group")
    );
    assert_eq!(
        runtime.show_job_run(&run.run_id).expect("show run").state,
        JobRunState::Cancelled
    );

    // The operator's outcome reaches the task: backlog, not a recovery block.
    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Backlog
    );
    let history = runtime.get_task_history(&task_id).expect("task history");
    assert_eq!(
        history.last().expect("cancel history").event,
        "workflow_run_cancelled"
    );

    // Only the verified target was stopped.
    assert!(
        bystander.try_wait().expect("probe bystander").is_none(),
        "cancellation must not reach a process other than pid {owner_pid}"
    );
    bystander.kill().expect("end bystander");
    bystander.wait().expect("reap bystander");
}

/// A cancellation that can no longer finish must not hold the run: without
/// these exits a crashed canceller would strand a dead run as `running`.
#[test]
fn reconciliation_interrupts_a_dead_owner_once_its_cancellation_cannot_finish() {
    if run_isolated_test(std::any::type_name_of_val(
        &reconciliation_interrupts_a_dead_owner_once_its_cancellation_cannot_finish,
    )) {
        return;
    }

    enum Request {
        /// The canceller reported it could not stop the run.
        Failed,
        /// The canceller vanished long ago without reporting anything.
        Abandoned,
    }
    for (case, request) in [
        ("failed", Request::Failed),
        ("abandoned", Request::Abandoned),
    ] {
        let (_root, runtime) = test_runtime();
        let run = insert_pending_run(&runtime, "task_pr_pipeline");
        let mut owner = spawn_process();
        let running = mark_running(&runtime, &run, &owner);
        owner.kill().expect("end owner");
        owner.wait().expect("reap owner");

        let request_id = format!("cancel-{case}");
        let requested_at = match request {
            Request::Failed => Utc::now(),
            Request::Abandoned => Utc::now() - chrono::Duration::days(1),
        };
        runtime
            .record_pipeline_audit(
                CANCELLATION_REQUEST_AUDIT,
                Some(&running.run_id),
                Some("cli"),
                AuditEventStatus::Success,
                json!({
                    "request_id": request_id,
                    "run_id": running.run_id,
                    "requested_at": requested_at.to_rfc3339(),
                }),
                None,
            )
            .expect("record request");
        if matches!(request, Request::Failed) {
            runtime
                .record_cancellation_completion(
                    &running,
                    &request_id,
                    "failed",
                    None,
                    Some("stop unconfirmed"),
                )
                .expect("record failure");
        }

        let reconciled = runtime.show_job_run(&run.run_id).expect("show run");
        assert_eq!(reconciled.state, JobRunState::Interrupted, "{case}");
        assert_eq!(
            reconciled
                .steps
                .last()
                .and_then(|step| step.error_code.as_deref()),
            Some(WORKER_TERMINATED_ERROR_CODE),
            "{case}"
        );
    }
}

/// A dead owner whose recorded exit names a systemd-sent signal is a hold.
/// An unknown sender, a crash, a signal from `orbit`, and a systemd signal
/// after a memory-limit breach stay interrupted failures.
#[test]
fn reconciliation_holds_a_worker_the_host_manager_stopped() {
    if run_isolated_test(std::any::type_name_of_val(
        &reconciliation_holds_a_worker_the_host_manager_stopped,
    )) {
        return;
    }

    struct ExitCase {
        label: &'static str,
        signal: Option<i32>,
        exit_code: Option<i32>,
        command: Option<&'static str>,
        oom_kills: Option<u64>,
        held: bool,
    }
    let cases = [
        ExitCase {
            label: "systemd SIGTERM",
            signal: Some(libc::SIGTERM),
            exit_code: None,
            command: Some("systemd"),
            oom_kills: None,
            held: true,
        },
        ExitCase {
            label: "unknown sender",
            signal: Some(libc::SIGTERM),
            exit_code: None,
            command: None,
            oom_kills: None,
            held: false,
        },
        ExitCase {
            label: "crash exit",
            signal: None,
            exit_code: Some(1),
            command: None,
            oom_kills: None,
            held: false,
        },
        ExitCase {
            label: "orbit SIGTERM",
            signal: Some(libc::SIGTERM),
            exit_code: None,
            command: Some("orbit"),
            oom_kills: None,
            held: false,
        },
        ExitCase {
            label: "systemd SIGTERM after oom",
            signal: Some(libc::SIGTERM),
            exit_code: None,
            command: Some("systemd"),
            oom_kills: Some(1),
            held: false,
        },
    ];

    for case in cases {
        let (_root, runtime) = test_runtime();
        let run = insert_pending_run(&runtime, "task_pr_pipeline");
        let mut owner = spawn_process();
        let running = mark_running(&runtime, &run, &owner);
        let pid = running.pid.expect("owner pid");
        owner.kill().expect("end owner");
        owner.wait().expect("reap owner");

        let signal_sender = case.command.map(|command| {
            json!({
                "pid": 1,
                "uid": 0,
                "command": command,
            })
        });
        let scope = case.oom_kills.map(|oom_kills| {
            json!({
                "unit": "orbit-worker-test.scope",
                "oom_kills": oom_kills,
                "tasks_refused": 0,
            })
        });
        runtime
            .record_pipeline_audit(
                WORKER_EXIT_AUDIT,
                Some(&running.run_id),
                Some("supervisor"),
                AuditEventStatus::Success,
                json!({
                    "worker_pid": pid,
                    "exit_status": case.signal.map(|signal| format!("signal: {signal}"))
                        .unwrap_or_else(|| "exit status: 1".to_string()),
                    "exit_code": case.exit_code,
                    "signal": case.signal,
                    "signal_name": case.signal.map(|_| "SIGTERM"),
                    "core_dumped": false,
                    "scope": scope,
                    "signal_sender": signal_sender,
                    "observed_at": Utc::now().to_rfc3339(),
                }),
                None,
            )
            .expect("record worker exit");

        let reconciled = runtime.show_job_run(&run.run_id).expect("show run");
        let code = reconciled
            .steps
            .last()
            .and_then(|step| step.error_code.as_deref());
        let message = reconciled
            .steps
            .last()
            .and_then(|step| step.error_message.as_deref())
            .unwrap_or("");
        let context = runtime
            .run_failure_context(
                &reconciled,
                None,
                RunFailureContextProfile::AllFailedLeaves(RunFailureRead::Observe),
                &mut |_| {},
            )
            .expect("failure query");
        let counted = context
            .causes
            .iter()
            .any(|cause| cause.run_id == reconciled.run_id);
        if case.held {
            assert_eq!(reconciled.state, JobRunState::Held, "{}", case.label);
            assert_eq!(code, Some(OPERATOR_STOPPED_ERROR_CODE), "{}", case.label);
            assert!(
                message.contains("marked held because the host manager stopped its worker"),
                "{}: {message}",
                case.label
            );
            assert!(
                !counted,
                "{} must be absent from the failure query",
                case.label
            );
        } else {
            assert_eq!(reconciled.state, JobRunState::Interrupted, "{}", case.label);
            assert_eq!(code, Some(WORKER_TERMINATED_ERROR_CODE), "{}", case.label);
            assert!(
                message.contains("marked interrupted"),
                "{}: {message}",
                case.label
            );
            assert!(counted, "{} must count as a failure", case.label);
        }
    }
}
