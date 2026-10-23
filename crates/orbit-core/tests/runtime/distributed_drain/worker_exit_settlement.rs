//! Reconciliation freezes worker-exit evidence in the claim outbox before
//! the owner receives it. Late supervisor observations amend only the run.

use super::*;
use orbit_core::test_support::{
    WorkerReapPhase, install_substitute_pipeline_worker, install_worker_reap_hook,
    pipeline_worker_observer_completed,
};
use orbit_types::workflow::ClaimFailureClass;
use std::sync::mpsc;
use std::time::Instant;

const DEADLINE: Duration = Duration::from_secs(30);

fn wait_for_observer(pair: &Pair, leaf: &str, pid: u32) {
    let deadline = Instant::now() + DEADLINE;
    while !pipeline_worker_observer_completed(&pair.follower_repo, leaf, pid) {
        assert!(Instant::now() < deadline, "supervisor did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn mark_old(pair: &Pair, leaf: &str, pid: u32, long_identity: bool) {
    pair.follower_jobs
        .mark_job_run_running(leaf, Utc::now() - chrono::Duration::minutes(5), pid)
        .unwrap();
    let mut run = pair.follower_jobs.get_job_run(leaf).unwrap().unwrap();
    // An already launched fixture keeps its original start on a repeated
    // claim; age that start explicitly past the owner startup grace.
    run.started_at = Some(Utc::now() - chrono::Duration::minutes(5));
    if long_identity {
        // A long legacy identity makes the liveness prefix exceed the reason
        // bound. The cause must survive, including at a UTF-8 boundary.
        run.pid_start_time = Some("é".repeat(1024));
    }
    pair.follower
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&pair.follower.workspace_id().unwrap(), &run, None)
        .unwrap();
}

fn owner_reason(pair: &Pair, leaf: &str) -> String {
    let settlements = pair.wire.owner.leaf_settlements(None, false).unwrap();
    let settlement = settlements
        .iter()
        .find(|settlement| settlement.leaf_run_id.as_deref() == Some(leaf))
        .expect("owner durably recorded the leaf release");
    assert_eq!(
        settlement.kind,
        Some(orbit_store::contracts::ClaimSettlementKind::Release)
    );
    assert_eq!(settlement.failure_class, Some(ClaimFailureClass::Transient));
    assert_eq!(pair.owner_status(&pair.claimed_task(leaf)), "backlog");
    let reason = settlement.reason.clone().expect("release reason");
    assert!(reason.len() <= 1024, "reason exceeds byte bound: {reason}");
    reason
}

/// Natural exit plus a supervisor rendezvous reproduces both sides of the
/// observation/reconciliation race without killing any worker.
#[test]
fn reconciliation_carries_observed_exit_and_freezes_unavailable_late_exit() {
    if !isolated(
        module_path!(),
        "reconciliation_carries_observed_exit_and_freezes_unavailable_late_exit",
    ) {
        return;
    }
    for (phase, available, long_identity, deliver_first) in [
        (WorkerReapPhase::AfterExitRecorded, true, false, true),
        (WorkerReapPhase::AfterExitRecorded, true, true, true),
        (WorkerReapPhase::BeforeExitRecorded, false, false, true),
        (WorkerReapPhase::BeforeExitRecorded, false, true, false),
    ] {
        let pair = Pair::new(1);
        let drain = pair.run_drain();
        let pid_file = pair._root.path().join("exited-worker.pid");
        install_substitute_pipeline_worker([
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("echo $$ > '{}'; exit 23", pid_file.display()),
        ]);
        let (reaped, at_phase) = mpsc::sync_channel(1);
        let (settled, resume) = mpsc::sync_channel(1);
        install_worker_reap_hook(phase, move |run_id| {
            reaped.send(run_id.to_string()).unwrap();
            resume.recv_timeout(DEADLINE).unwrap();
        });
        pair.pass(&drain);
        let leaf = at_phase.recv_timeout(DEADLINE).unwrap();
        let pid = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        mark_old(&pair, &leaf, pid, long_identity);

        let run = pair.follower.show_job_run(&leaf).unwrap();
        assert_eq!(run.state, JobRunState::Interrupted);
        assert!(
            run.steps.iter().any(|step| step
                .error_message
                .as_deref()
                .is_some_and(|message| message.contains("reason=process_not_found"))),
            "{run:?}"
        );
        let recorded = pair.admission(&leaf).settlement.expect("frozen outbox");
        if deliver_first {
            pair.pass_over(&drain, json!({"window_expired": true}));
        }
        // Release the observer even if the assertion below fails on the old
        // ordering, and wait for every process/thread this fixture started.
        settled.send(()).unwrap();
        wait_for_observer(&pair, &leaf, pid);
        // Late evidence cannot rewrite the outbox even before its first
        // delivery, and cannot trigger another delivery after it settled.
        pair.pass_over(&drain, json!({"window_expired": true}));
        let reason = owner_reason(&pair, &leaf);
        if available {
            assert!(
                reason.contains("exited with code 23"),
                "owner lost the recorded worker exit: {reason}"
            );
        } else {
            assert!(
                reason.contains("no supervisor observed or recorded its exit status")
                    && reason.contains("no worker scope evidence is available")
                    && !reason.contains("exited with code 23"),
                "owner must distinguish unavailable settlement-time evidence: {reason}"
            );
            let amended = pair.follower.show_job_run(&leaf).unwrap();
            assert_eq!(amended.state, JobRunState::Interrupted);
            assert!(
                amended
                    .steps
                    .iter()
                    .any(
                        |step| step.error_message.as_deref().is_some_and(|message| message
                            .contains("afterwards its supervisor reaped")
                            && message.contains("exited with code 23"))
                    ),
                "{amended:?}"
            );
        }
        let owner_before = pair.owner_task(&pair.claimed_task(&leaf));
        for _ in 0..2 {
            pair.pass_over(&drain, json!({"window_expired": true}));
            pair.follower.deliver_recorded_pull_settlements();
        }
        assert_eq!(pair.admission(&leaf).settlement, Some(recorded));
        assert_eq!(owner_reason(&pair, &leaf), reason);
        assert_eq!(pair.owner_task(&pair.claimed_task(&leaf)), owner_before);
        assert_eq!(pair.wire.calls("orbit.drain.claim.settle").len(), 1);
    }
}

/// A worker nobody observed reaping cannot acquire an invented exit cause.
/// The no-scope case also runs on hosts without systemd.
#[test]
fn an_unobserved_worker_release_carries_available_scope_evidence() {
    if !isolated(
        module_path!(),
        "an_unobserved_worker_release_carries_available_scope_evidence",
    ) {
        return;
    }
    for scope in [false, true] {
        if scope && !cfg!(target_os = "linux") {
            continue;
        }
        let pair = Pair::new(1);
        let drain = pair.run_drain();
        let mut worker = std::process::Command::new("true").spawn().unwrap();
        let pid = worker.id();
        assert!(worker.wait().unwrap().success());
        let leaf = pair.launched_leaf(&drain, 1, pid);
        // Even a dead worker keeps its startup grace. Reading a fresh leaf
        // must neither interrupt it nor freeze a release prematurely.
        assert_eq!(
            pair.follower.show_job_run(&leaf).unwrap().state,
            JobRunState::Running
        );
        assert!(pair.admission(&leaf).settlement.is_none());
        mark_old(&pair, &leaf, pid, false);
        if scope {
            let unit = format!("orbit-worker-{leaf}-fixture.scope");
            pair.follower
                .record_audit_event(&orbit_store::contracts::AuditEventInsertParams {
                    execution_id: format!("claim-{leaf}"),
                    command: "pipeline".into(),
                    subcommand: None,
                    tool_name: Some("pipeline.worker.claimed".into()),
                    target_type: Some("job_run".into()),
                    target_id: Some(leaf.clone()),
                    role: "operator".into(),
                    status: orbit_types::telemetry::AuditEventStatus::Success,
                    exit_code: 0,
                    duration_ms: 0,
                    working_directory: pair.follower_repo.to_string_lossy().into_owned(),
                    arguments_json: Some(
                        json!({
                            "worker_pid": pid, "scope_unit": unit,
                            "scope_cgroup": format!("/sys/fs/cgroup/{unit}")
                        })
                        .to_string(),
                    ),
                    stdout_truncated: None,
                    stderr_truncated: None,
                    error_message: None,
                    host: None,
                    pid: std::process::id(),
                    session_id: None,
                    workspace_id: Some(pair.follower.workspace_id().unwrap()),
                    caller_machine_id: None,
                    caller_machine_name: None,
                    process_machine_id: None,
                    process_machine_name: None,
                    transport: None,
                    effective_capabilities: Default::default(),
                    origin_session_id: None,
                    mcp_call_id: None,
                    lease_id: None,
                    task_id: None,
                    job_run_id: Some(leaf.clone()),
                    activity_id: None,
                    step_index: None,
                })
                .unwrap();
        }
        let run = pair.follower.show_job_run(&leaf).unwrap();
        assert_eq!(run.state, JobRunState::Interrupted);
        pair.pass_over(&drain, json!({"window_expired": true}));
        let reason = owner_reason(&pair, &leaf);
        assert!(
            reason.contains("no supervisor observed or recorded its exit status"),
            "{reason}"
        );
        assert!(
            !reason.contains("killed by signal") && !reason.contains("exited with code"),
            "{reason}"
        );
        if scope {
            assert!(
                reason.contains(&format!("worker scope 'orbit-worker-{leaf}-fixture.scope'")),
                "{reason}"
            );
        } else {
            assert!(
                reason.contains("no worker scope evidence is available"),
                "{reason}"
            );
        }
    }
}
