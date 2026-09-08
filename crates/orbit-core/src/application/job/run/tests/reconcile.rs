//! Stale read/list behavior and terminal timing repair tests.

use super::*;

use super::super::JobRunListParams;
use crate::application::job::TERMINAL_OUTCOME_CONFLICT_CODE;
use chrono::{DateTime, Duration, Utc};
use orbit_store::V2AuditEventInsertParams;
use orbit_types::workflow::JobRunState;

#[test]
fn show_job_run_reconciles_stale_running_owner() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_stale");
    let started_at = Utc::now() - Duration::seconds(3);
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started_at, 999_999)
        .expect("mark running with impossible pid");

    let shown = runtime.show_job_run(&run.run_id).expect("show run");

    assert_eq!(shown.state, JobRunState::Interrupted);
    assert!(shown.finished_at.is_some());
    assert!(shown.duration_ms.is_some_and(|value| value > 0));
    assert!(shown.steps.iter().any(|step| {
        step.state == JobRunState::Interrupted
            && step.error_code.as_deref() == Some("process_not_found")
            && step.error_message.as_deref().is_some_and(|message| {
                message.contains("recorded worker process is no longer alive")
            })
    }));
}

#[cfg(unix)]
#[test]
fn show_job_run_keeps_live_owner_running() {
    use orbit_common::process::identity::process_start_identity_token;

    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_live");
    let pid = std::process::id();
    if process_start_identity_token(pid).is_none() {
        return;
    }
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), pid)
        .expect("mark current process running");

    let shown = runtime.show_job_run(&run.run_id).expect("show run");

    assert_eq!(shown.state, JobRunState::Running);
    assert!(shown.finished_at.is_none());
    assert!(shown.duration_ms.is_none());
}

#[test]
fn show_job_run_keeps_pending_runs_pending() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_pending");

    let shown = runtime.show_job_run(&run.run_id).expect("show pending run");

    assert_eq!(shown.state, JobRunState::Pending);
    assert!(shown.finished_at.is_none());
    assert!(shown.duration_ms.is_none());
}

#[test]
fn show_job_run_repairs_terminal_run_missing_timing() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_terminal");
    let started_at = Utc::now() - Duration::seconds(8);
    let finished_at = started_at + Duration::seconds(5);
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started_at, std::process::id())
        .expect("mark running");
    runtime
        .stores()
        .jobs()
        .finalize_job_run(&run.run_id, JobRunState::Success, finished_at, Some(5_000))
        .expect("finalize success");
    let finalized = runtime.show_job_run(&run.run_id).expect("show finalized");
    strip_run_timing(&runtime, &finalized);
    write_run_finished_audit(&runtime, &run.run_id, finished_at);

    let repaired = runtime.show_job_run(&run.run_id).expect("show repaired");

    assert_eq!(repaired.state, JobRunState::Success);
    assert_eq!(repaired.finished_at, Some(finished_at));
    assert_eq!(repaired.duration_ms, Some(5_000));
}

/// [ORB-10070] The workspace-open orphan scan terminalizes `pending` children
/// left behind by an interrupted parent run: never claimed by any worker and
/// far older than the claim grace window, they finalize as `interrupted`
/// instead of deferring consumers forever.
#[test]
fn workspace_open_reconciles_orphaned_pending_children_of_interrupted_parent() {
    let _env = orbit_common::test_env::unset(["ORBIT_MANAGED_RUN_CONTEXT", "ORBIT_RUN_ID"]);
    let (root, runtime) = test_runtime();
    let parent = insert_pending_run(&runtime, "task_auto_pipeline");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&parent.run_id, Utc::now() - Duration::days(4), 999_999)
        .expect("mark parent running");
    runtime
        .stores()
        .jobs()
        .finalize_job_run(
            &parent.run_id,
            JobRunState::Interrupted,
            Utc::now() - Duration::days(4),
            Some(1_000),
        )
        .expect("finalize parent interrupted");
    let children = [
        insert_pending_run(&runtime, "task_gate_pipeline"),
        insert_pending_run(&runtime, "task_gate_pipeline"),
    ];
    for child in &children {
        backdate_run_created_at(&runtime, child, Utc::now() - Duration::days(4));
    }
    drop(runtime);

    let reopened = OrbitRuntime::from_roots(
        &root.path().join("global"),
        &root.path().join("repo").join(".orbit"),
    )
    .expect("reopen workspace");

    for child in &children {
        let reconciled = reopened
            .get_job_run_backend(&child.run_id)
            .expect("read child run")
            .expect("child run exists");
        assert_eq!(reconciled.state, JobRunState::Interrupted);
        assert!(reconciled.finished_at.is_some());
        let shown = reopened.show_job_run(&child.run_id).expect("show child");
        assert!(shown.steps.iter().any(|step| {
            step.state == JobRunState::Interrupted
                && step.error_code.as_deref() == Some("never_claimed")
                && step
                    .error_message
                    .as_deref()
                    .is_some_and(|message| message.contains("no live worker process owns it"))
        }));
    }
    let parent = reopened
        .get_job_run_backend(&parent.run_id)
        .expect("read parent run")
        .expect("parent run exists");
    assert_eq!(parent.state, JobRunState::Interrupted);
}

/// [ORB-10070] A freshly queued run that no worker has claimed yet is inside
/// the grace window and must never be terminalized by a racing reconcile.
#[test]
fn reconcile_keeps_fresh_unclaimed_pending_run_pending() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_fresh_pending");

    runtime
        .reconcile_stale_job_runs(None)
        .expect("reconcile scan");

    let stored = runtime
        .get_job_run_backend(&run.run_id)
        .expect("read run")
        .expect("run exists");
    assert_eq!(stored.state, JobRunState::Pending);
}

/// [ORB-10070] A pending run claimed by a live worker stays pending even far
/// past the unclaimed grace window: the claim, not the run's age, decides.
#[cfg(unix)]
#[test]
fn pending_run_with_live_claimed_owner_stays_pending() {
    use orbit_common::process::identity::process_start_identity_token;

    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_claimed_live");
    let pid = std::process::id();
    if process_start_identity_token(pid).is_none() {
        return;
    }
    assert!(
        runtime
            .stores()
            .jobs()
            .claim_pending_job_run_owner(&run.run_id, pid)
            .expect("claim pending run")
    );
    backdate_run_created_at(&runtime, &run, Utc::now() - Duration::days(4));

    runtime
        .reconcile_stale_job_runs(None)
        .expect("reconcile scan");

    let stored = runtime
        .get_job_run_backend(&run.run_id)
        .expect("read run")
        .expect("run exists");
    assert_eq!(stored.state, JobRunState::Pending);
    assert_eq!(stored.pid, Some(pid));
}

/// [ORB-10070] A pending run whose claimed worker died is conclusively
/// orphaned and reconciles to `interrupted` immediately — no grace window.
#[cfg(unix)]
#[test]
fn pending_run_with_dead_claimed_owner_is_reconciled() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_claimed_dead");
    assert!(
        runtime
            .stores()
            .jobs()
            .claim_pending_job_run_owner(&run.run_id, 999_999)
            .expect("claim with impossible pid")
    );

    runtime
        .reconcile_stale_job_runs(None)
        .expect("reconcile scan");

    let shown = runtime.show_job_run(&run.run_id).expect("show run");
    assert_eq!(shown.state, JobRunState::Interrupted);
    assert!(shown.finished_at.is_some());
    assert!(shown.steps.iter().any(|step| {
        step.state == JobRunState::Interrupted
            && step
                .error_message
                .as_deref()
                .is_some_and(|message| message.contains("no live worker process owns it"))
    }));
}

/// [ORB-11116] A worker may terminalize after the sweep reads/classifies its
/// stale snapshot. Each terminal outcome wins before the interruption boundary
/// is revalidated, so reconciliation must neither interrupt nor record a
/// first-terminal-wins conflict afterward.
#[test]
fn concurrent_terminal_outcomes_win_before_stale_interruption_revalidation() {
    for terminal_state in [
        JobRunState::Success,
        JobRunState::Failed,
        JobRunState::Cancelled,
    ] {
        let (_root, runtime) = test_runtime();
        let run = insert_pending_run(&runtime, "qa_reconcile_terminal_race");
        runtime
            .stores()
            .jobs()
            .mark_job_run_running(&run.run_id, Utc::now() - Duration::seconds(3), 999_999)
            .expect("mark stale running");
        let stale_snapshot = runtime
            .get_job_run_backend(&run.run_id)
            .expect("read stale snapshot")
            .expect("stale run exists");
        reserve_for_run(&runtime, &run.run_id, "file:src/reconcile-race.rs");
        reserve_for_run(&runtime, "jrun-unrelated", "file:src/unrelated.rs");

        let reconciled = runtime
            .reconcile_stale_job_run_after_classification(&stale_snapshot, || {
                runtime
                    .stores()
                    .jobs()
                    .finalize_job_run(&run.run_id, terminal_state, Utc::now(), Some(3_000))
                    .expect("concurrent terminal writer wins");
            })
            .expect("reconcile stale snapshot");

        assert!(!reconciled, "{terminal_state} must prevent interruption");
        let stored = runtime.show_job_run(&run.run_id).expect("show winner");
        assert_eq!(stored.state, terminal_state);
        assert!(stored.finished_at.is_some());
        assert!(stored.steps.iter().all(|step| {
            step.state != JobRunState::Interrupted
                && step.error_code.as_deref() != Some(TERMINAL_OUTCOME_CONFLICT_CODE)
        }));
        let owners = active_reservation_owners(&runtime);
        assert!(
            owners.iter().any(|owner| owner == &run.run_id),
            "{terminal_state} winner must keep its reservation"
        );
        assert!(
            owners.iter().any(|owner| owner == "jrun-unrelated"),
            "unrelated reservations must stay"
        );
    }
}

/// [ORB-10594] The incident shape, end to end through the sweep: a run whose
/// owner was recorded in another PID namespace survives a full
/// `reconcile_stale_job_runs` pass. Inside the sandbox that condemned three
/// healthy runs on 2026-08-02 the owner PID is invisible — 999_999 stands in
/// for that, since no probe available here can find it either — but the
/// namespace recorded on the token says the verdict is not this observer's to
/// make.
#[cfg(unix)]
#[test]
fn sweep_spares_a_run_whose_owner_lives_in_another_pid_namespace() {
    use orbit_common::process::identity::{STABLE_TOKEN_PREFIX, current_pid_namespace};

    let (_root, runtime) = test_runtime();
    let Some(current) = current_pid_namespace() else {
        return; // non-Linux: namespaces are not observable.
    };
    let run = insert_pending_run(&runtime, "qa_foreign_ns");
    let started_at = Utc::now() - Duration::minutes(30);
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started_at, 999_999)
        .expect("mark running");
    set_run_pid_start_time(
        &runtime,
        &run,
        &format!("{STABLE_TOKEN_PREFIX}pidns={current}0000:Sun Aug  2 20:13:45 2026"),
    );

    runtime
        .reconcile_stale_job_runs(None)
        .expect("reconcile scan");

    let stored = runtime
        .get_job_run_backend(&run.run_id)
        .expect("read run")
        .expect("run exists");
    assert_eq!(
        stored.state,
        JobRunState::Running,
        "a run owned in another PID namespace must survive the sweep"
    );
    assert!(stored.finished_at.is_none());
    assert!(stored.duration_ms.is_none());
}

/// [ORB-10594] The counterpart the fix must not break: same sweep, same
/// unreachable PID, but the token names *this* namespace, so the run is
/// genuinely orphaned and is still marked interrupted.
#[cfg(unix)]
#[test]
fn sweep_still_condemns_a_run_whose_owner_died_in_this_pid_namespace() {
    use orbit_common::process::identity::{STABLE_TOKEN_PREFIX, current_pid_namespace};

    let (_root, runtime) = test_runtime();
    let Some(current) = current_pid_namespace() else {
        return;
    };
    let run = insert_pending_run(&runtime, "qa_local_ns_dead");
    let started_at = Utc::now() - Duration::minutes(30);
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started_at, 999_999)
        .expect("mark running");
    set_run_pid_start_time(
        &runtime,
        &run,
        &format!("{STABLE_TOKEN_PREFIX}pidns={current}:Sun Aug  2 20:13:45 2026"),
    );

    runtime
        .reconcile_stale_job_runs(None)
        .expect("reconcile scan");

    let shown = runtime.show_job_run(&run.run_id).expect("show run");
    assert_eq!(shown.state, JobRunState::Interrupted);
    assert!(shown.steps.iter().any(|step| {
        step.error_code.as_deref() == Some("process_not_found")
            && step.error_message.as_deref().is_some_and(|message| {
                message.contains("recorded worker process is no longer alive")
            })
    }));
}

/// [ORB-10594] An orphaned run's recorded end of work is when its trail went
/// quiet, not when a sweep happened to notice. Detection lag used to be booked
/// as run duration.
#[test]
fn orphaned_run_finished_at_tracks_last_audit_activity_not_detection_time() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_orphan_timing");
    let started_at = Utc::now() - Duration::minutes(40);
    let last_activity = started_at + Duration::minutes(5);
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started_at, 999_999)
        .expect("mark running with impossible pid");
    write_run_finished_audit(&runtime, &run.run_id, last_activity);

    let shown = runtime.show_job_run(&run.run_id).expect("show run");

    assert_eq!(shown.state, JobRunState::Interrupted);
    assert_eq!(shown.finished_at, Some(last_activity));
    // ~5 minutes of work, not the ~40 minutes back to `started_at`.
    let duration_ms = shown.duration_ms.expect("duration recorded");
    assert!(
        (295_000..=305_000).contains(&duration_ms),
        "duration must span started_at..last activity, got {duration_ms}ms"
    );
}

#[test]
fn orphaned_run_finished_at_falls_back_to_now_for_out_of_range_audit_timestamps() {
    let (_root, runtime) = test_runtime();

    for (suffix, activity_at) in [
        ("before-start", Utc::now() - Duration::hours(2)),
        ("after-now", Utc::now() + Duration::hours(2)),
    ] {
        let run = insert_pending_run(&runtime, "qa_orphan_out_of_range_timing");
        let started_at = Utc::now() - Duration::minutes(30);
        runtime
            .stores()
            .jobs()
            .mark_job_run_running(&run.run_id, started_at, 999_999)
            .expect("mark running with impossible pid");
        write_audit_activity(&runtime, &run.run_id, suffix, activity_at);

        let before_fallback = Utc::now();
        let shown = runtime
            .show_job_run(&run.run_id)
            .expect("show reconciled run");
        let after_fallback = Utc::now();
        let finished_at = shown.finished_at.expect("reconciled finished time");

        assert!(
            finished_at >= before_fallback && finished_at <= after_fallback,
            "{suffix} activity must fall back to detection time"
        );
    }
}

fn write_audit_activity(
    runtime: &OrbitRuntime,
    run_id: &str,
    suffix: &str,
    activity_at: DateTime<Utc>,
) {
    let event = serde_json::json!({
        "schemaVersion": 1,
        "event_type": "test.activity",
        "event_id": format!("evt-{run_id}-{suffix}"),
        "ts": activity_at.to_rfc3339(),
        "run_id": run_id,
        "agent_identity": "system",
    });
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().expect("workspace id"),
            event_id: event["event_id"].as_str().expect("event id").to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: "test.activity".to_string(),
            ts: activity_at,
            run_id: run_id.to_string(),
            agent_identity: "system".to_string(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: event.to_string(),
        })
        .expect("insert audit activity");
}

#[cfg(unix)]
#[test]
fn list_job_runs_reconciles_before_state_filtering() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_filter");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now() - Duration::seconds(3), 999_999)
        .expect("mark stale running");

    let running = runtime
        .list_job_runs(JobRunListParams {
            state: Some(JobRunState::Running),
            ..JobRunListParams::default()
        })
        .expect("list running");
    let interrupted = runtime
        .list_job_runs(JobRunListParams {
            state: Some(JobRunState::Interrupted),
            ..JobRunListParams::default()
        })
        .expect("list interrupted");

    assert!(
        !running
            .iter()
            .any(|candidate| candidate.run_id == run.run_id)
    );
    assert!(
        interrupted
            .iter()
            .any(|candidate| candidate.run_id == run.run_id)
    );
}

/// [ORB-11603] A worker may replace the recorded owner after the sweep
/// classified a stale snapshot. The freshness reread must observe the new
/// owner and must not interrupt or release reservations.
#[cfg(unix)]
#[test]
fn concurrent_owner_change_after_stale_classification_is_not_interrupted() {
    use orbit_common::process::identity::process_start_identity_token;

    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_reconcile_owner_race");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now() - Duration::seconds(3), 999_999)
        .expect("mark stale running");
    let stale_snapshot = runtime
        .get_job_run_backend(&run.run_id)
        .expect("read stale snapshot")
        .expect("stale run exists");
    reserve_for_run(&runtime, &run.run_id, "file:src/owner-race.rs");
    reserve_for_run(&runtime, "jrun-unrelated", "file:src/unrelated.rs");

    let pid = std::process::id();
    let Some(token) = process_start_identity_token(pid) else {
        return;
    };

    let reconciled = runtime
        .reconcile_stale_job_run_after_classification(&stale_snapshot, || {
            set_run_owner(&runtime, &run, pid, Some(token.as_str()));
        })
        .expect("reconcile after owner change");

    assert!(
        !reconciled,
        "a live replacement owner must prevent interruption"
    );
    let stored = runtime
        .get_job_run_backend(&run.run_id)
        .expect("read winner")
        .expect("run exists");
    assert_eq!(stored.state, JobRunState::Running);
    assert!(stored.finished_at.is_none());
    let owners = active_reservation_owners(&runtime);
    assert!(
        owners.iter().any(|owner| owner == &run.run_id),
        "live replacement owner must keep its reservation"
    );
    assert!(
        owners.iter().any(|owner| owner == "jrun-unrelated"),
        "unrelated reservations must stay"
    );
}

/// [ORB-11603] List and history repair incomplete terminal timing on the
/// second pass without requiring a stale-owner classification.
#[test]
fn list_and_history_repair_terminal_run_missing_timing() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_list_terminal");
    let started_at = Utc::now() - Duration::seconds(8);
    let finished_at = started_at + Duration::seconds(5);
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started_at, std::process::id())
        .expect("mark running");
    runtime
        .stores()
        .jobs()
        .finalize_job_run(&run.run_id, JobRunState::Success, finished_at, Some(5_000))
        .expect("finalize success");
    let finalized = runtime
        .get_job_run_backend(&run.run_id)
        .expect("read finalized")
        .expect("run exists");
    strip_run_timing(&runtime, &finalized);
    write_run_finished_audit(&runtime, &run.run_id, finished_at);

    let listed = runtime
        .list_job_runs(JobRunListParams {
            job_id: Some("qa_list_terminal".to_string()),
            ..JobRunListParams::default()
        })
        .expect("list repaired");
    let listed_run = listed
        .iter()
        .find(|candidate| candidate.run_id == run.run_id)
        .expect("listed repaired run");
    assert_eq!(listed_run.state, JobRunState::Success);
    assert_eq!(listed_run.finished_at, Some(finished_at));
    assert_eq!(listed_run.duration_ms, Some(5_000));

    strip_run_timing(&runtime, listed_run);
    let history = runtime
        .job_history("qa_list_terminal")
        .expect("history repaired");
    let history_run = history
        .iter()
        .find(|candidate| candidate.run_id == run.run_id)
        .expect("history repaired run");
    assert_eq!(history_run.state, JobRunState::Success);
    assert_eq!(history_run.finished_at, Some(finished_at));
    assert_eq!(history_run.duration_ms, Some(5_000));
}

/// [ORB-11603] Unchanged healthy pending/running owners are classified once
/// per list/history call. A later call classifies again (pass-local reuse
/// does not cross calls). Stale finalization may probe again after rereading
/// and is covered separately by the race fixtures.
#[cfg(unix)]
#[test]
fn list_and_history_classify_unchanged_healthy_owners_once() {
    use super::super::owner::reset_classify_owner_snapshots;
    use orbit_common::process::identity::process_start_identity_token;

    let pid = std::process::id();
    if process_start_identity_token(pid).is_none() {
        return;
    }

    let (_root, runtime) = test_runtime();
    let pending = insert_pending_run(&runtime, "qa_probe_pending");
    assert!(
        runtime
            .stores()
            .jobs()
            .claim_pending_job_run_owner(&pending.run_id, pid)
            .expect("claim pending")
    );
    let running = insert_pending_run(&runtime, "qa_probe_running");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&running.run_id, Utc::now(), pid)
        .expect("mark running");

    reset_classify_owner_snapshots();
    let listed = runtime
        .list_job_runs(JobRunListParams::default())
        .expect("list healthy owners");
    assert!(listed.iter().any(|candidate| {
        candidate.run_id == pending.run_id && candidate.state == JobRunState::Pending
    }));
    assert!(listed.iter().any(|candidate| {
        candidate.run_id == running.run_id && candidate.state == JobRunState::Running
    }));
    assert_one_classification_per_run(&[pending.run_id.as_str(), running.run_id.as_str()]);

    reset_classify_owner_snapshots();
    let history = runtime
        .job_history("qa_probe_pending")
        .expect("history healthy pending");
    assert!(
        history
            .iter()
            .any(|candidate| candidate.run_id == pending.run_id)
    );
    assert_one_classification_per_run(&[pending.run_id.as_str()]);

    reset_classify_owner_snapshots();
    runtime
        .list_job_runs(JobRunListParams::default())
        .expect("list healthy owners again");
    assert_one_classification_per_run(&[pending.run_id.as_str(), running.run_id.as_str()]);
}

#[cfg(unix)]
fn assert_one_classification_per_run(run_ids: &[&str]) {
    use super::super::owner::classify_owner_snapshots;

    let snapshots = classify_owner_snapshots();
    for run_id in run_ids {
        let matches: Vec<_> = snapshots
            .iter()
            .filter(|snapshot| snapshot.run_id == *run_id)
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "run {run_id} classified {} times in one list/history operation: {matches:?}; all={snapshots:?}",
            matches.len()
        );
    }
}
