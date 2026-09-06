//! Cancellation behavior and process-group cancellation tests.

use super::*;

#[cfg(unix)]
use super::super::owner::process_is_alive;
use chrono::{Duration, Utc};
#[cfg(unix)]
use orbit_common::OrbitError;
use std::path::Path;
#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::{Duration as StdDuration, Instant as StdInstant};
use tempfile::tempdir;

#[cfg(unix)]
struct ReapingChild(std::process::Child);

#[cfg(unix)]
impl ReapingChild {
    fn id(&self) -> u32 {
        self.0.id()
    }

    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.wait()
    }

    fn kill(&mut self) -> std::io::Result<()> {
        self.0.kill()
    }
}

#[cfg(unix)]
impl Drop for ReapingChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[test]
fn cancel_job_run_marks_pending_cancelled_without_signal() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_pending");

    let result = runtime
        .cancel_job_run_with_context(&run.run_id, "tester", "unit")
        .expect("cancel pending");

    assert_eq!(result.previous_state, "pending");
    assert_eq!(result.outcome, "cancelled");
    assert_eq!(result.final_state, "cancelled");
    assert!(!result.signal_attempted);
    assert_eq!(result.signal_outcome, None);
    let stored = runtime.show_job_run(&run.run_id).expect("show run");
    assert_eq!(stored.state, JobRunState::Cancelled);
    assert!(stored.finished_at.is_some());
    assert_eq!(stored.duration_ms, None);

    let audits = runtime.list_session_events(10).expect("events");
    let payload = audits
        .iter()
        .find(|event| event.event_type == "JobRunCancelled")
        .map(|event| &event.payload["data"])
        .expect("cancel event");
    assert_eq!(payload["run_id"], run.run_id);
    assert_eq!(payload["previous_state"], "pending");
    assert_eq!(payload["final_state"], "cancelled");
    assert_eq!(payload["actor"], "tester");
    assert_eq!(payload["source"], "unit");
    assert_eq!(payload["signal_attempted"], false);
}

#[test]
fn cancelled_pending_run_is_not_claimed_by_pipeline_worker() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_worker_skip");
    runtime
        .cancel_job_run(&run.run_id)
        .expect("cancel pending run");

    runtime
        .execute_pipeline_run_worker(&run.run_id)
        .expect("worker exits without executing cancelled run");

    let stored = runtime.show_job_run(&run.run_id).expect("show run");
    assert_eq!(stored.state, JobRunState::Cancelled);
    assert!(stored.started_at.is_none());
    assert!(stored.steps.is_empty());
}

#[test]
fn failed_run_wait_entry_carries_the_terminal_step_diagnostic() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "task_pilot_pipeline");
    let started_at = Utc::now() - Duration::seconds(2);
    let finished_at = Utc::now();
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started_at, std::process::id())
        .expect("mark child run running");
    runtime
        .stores()
        .jobs()
        .complete_job_run_step(
            &run.run_id,
            &orbit_store::JobRunStepParams {
                step_index: 0,
                target_type: orbit_types::workflow::JobTargetType::Activity,
                target_id: "task_pilot".to_string(),
                started_at,
                finished_at,
                duration_ms: Some(2_000),
                exit_code: None,
                agent_response_json: None,
                state: JobRunState::Failed,
                error_code: Some("worktree_mismatch".to_string()),
                error_message: Some("declared path pair is incomplete".to_string()),
            },
        )
        .expect("record child startup failure");
    runtime
        .stores()
        .jobs()
        .finalize_job_run(&run.run_id, JobRunState::Failed, finished_at, Some(2_000))
        .expect("finalize failed child run");

    let waited = runtime
        .wait_pipeline_runs(std::slice::from_ref(&run.run_id), 1, 1, Some("test"))
        .expect("wait failed review run");

    assert_eq!(waited.results[0].status, "failed");
    assert_eq!(
        waited.results[0].error.as_deref(),
        Some("worktree_mismatch: declared path pair is incomplete")
    );
}

#[test]
fn cancelled_run_wait_status_reports_cancelled() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_wait");
    runtime.cancel_job_run(&run.run_id).expect("cancel run");

    let result = runtime
        .wait_pipeline_runs(std::slice::from_ref(&run.run_id), 1, 1, Some("test"))
        .expect("wait cancelled");

    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0].run_id, run.run_id);
    assert_eq!(result.results[0].status, "cancelled");
}

#[test]
fn cancel_job_run_reports_terminal_run_idempotently_without_mutating_bundle() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_terminal");
    let started_at = Utc::now() - Duration::seconds(2);
    let finished_at = Utc::now();
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started_at, std::process::id())
        .expect("mark running");
    runtime
        .stores()
        .jobs()
        .finalize_job_run(&run.run_id, JobRunState::Success, finished_at, Some(2_000))
        .expect("finalize success");
    let before = runtime.show_job_run(&run.run_id).expect("show before");

    let result = runtime
        .cancel_job_run(&run.run_id)
        .expect("terminal cancellation is an idempotent observation");

    assert_eq!(result.outcome, "already_terminal");
    assert_eq!(result.previous_state, "success");
    assert_eq!(result.final_state, "success");
    assert!(!result.signal_attempted);
    let after = runtime.show_job_run(&run.run_id).expect("show after");
    assert_eq!(after, before);
    let events = runtime.list_session_events(20).expect("events");
    assert!(
        events
            .iter()
            .all(|event| event.event_type != "JobRunCancelled")
    );
}

#[cfg(unix)]
fn wait_until<F>(timeout: StdDuration, mut condition: F) -> bool
where
    F: FnMut() -> bool,
{
    let started = StdInstant::now();
    while started.elapsed() < timeout {
        if condition() {
            return true;
        }
        std::thread::sleep(StdDuration::from_millis(50));
    }
    condition()
}

#[cfg(unix)]
fn read_pid_pair(path: &Path) -> Option<(u32, u32)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut parts = raw.split_whitespace();
    let owner = parts.next()?.parse().ok()?;
    let child = parts.next()?.parse().ok()?;
    Some((owner, child))
}

#[cfg(unix)]
#[test]
fn cancel_job_run_does_not_signal_reused_pid_identity_mismatch() {
    use orbit_common::process::identity::STABLE_TOKEN_PREFIX;

    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_reused_pid");
    let mut sentinel = ReapingChild(
        Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sentinel"),
    );
    let sentinel_pid = sentinel.id();
    let started_at = Utc::now() - Duration::seconds(1);
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started_at, sentinel_pid)
        .expect("mark running");
    // Versioned token guarantees we exercise the strict `Mismatch`
    // classification path; legacy unversioned tokens may flow through the
    // softer LegacyLiveUnverified branch but must still produce
    // owner_identity_mismatch from `signal_run_owner_process`.
    let mismatched_versioned =
        format!("{STABLE_TOKEN_PREFIX}definitely-not-the-sentinel-start-token");
    set_run_pid_start_time(&runtime, &run, &mismatched_versioned);

    let result = runtime.cancel_job_run(&run.run_id).expect("cancel run");

    assert!(result.signal_attempted);
    assert_eq!(
        result.signal_outcome.as_deref(),
        Some("owner_identity_mismatch")
    );
    assert!(
        process_is_alive(sentinel_pid),
        "sentinel process must not be killed by mismatched owner identity"
    );
    let _ = sentinel.kill();
    let _ = sentinel.wait();
}

#[cfg(unix)]
#[test]
fn cancel_job_run_kills_term_resistant_process_group() {
    use std::os::unix::process::CommandExt;

    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_process_group");
    let pid_dir = tempdir().expect("pid tempdir");
    let pid_file = pid_dir.path().join("pids");
    let script = format!(
        "trap 'exit 0' TERM; (trap '' TERM; sleep 30) & child=$!; printf '%s %s\\n' $$ \"$child\" > {}; wait",
        shell_quote(pid_file.to_string_lossy().as_ref())
    );
    let mut owner = Command::new("/bin/sh");
    owner
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        owner.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut owner = ReapingChild(owner.spawn().expect("spawn owner"));
    let mut pid_pair = None;
    assert!(
        wait_until(StdDuration::from_secs(2), || {
            pid_pair = read_pid_pair(&pid_file);
            pid_pair.is_some()
        }),
        "owner did not write pid file"
    );
    let Some((owner_pid, child_pid)) = pid_pair else {
        panic!("owner did not write pid file");
    };
    assert_eq!(owner.id(), owner_pid);
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), owner_pid)
        .expect("mark running");

    let result = runtime.cancel_job_run(&run.run_id).expect("cancel run");
    let _ = owner.wait();

    assert!(result.signal_attempted);
    assert_eq!(
        result.signal_outcome.as_deref(),
        Some("killed_process_group")
    );
    assert!(
        wait_until(StdDuration::from_secs(3), || !process_is_alive(child_pid)),
        "child process {child_pid} should be gone after process-group cancellation"
    );
}

#[cfg(unix)]
#[test]
fn cancel_job_run_terminates_cooperative_process_group() {
    use std::os::unix::process::CommandExt;

    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_term_process_group");
    let mut owner = Command::new("sleep");
    owner
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        owner.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut owner = ReapingChild(owner.spawn().expect("spawn cooperative owner"));
    let owner_pid = owner.id();
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), owner_pid)
        .expect("mark running");

    let result = runtime.cancel_job_run(&run.run_id).expect("cancel run");
    owner.wait().expect("reap cooperative owner");

    assert_eq!(
        result.signal_outcome.as_deref(),
        Some("terminated_process_group")
    );
}

#[cfg(unix)]
#[test]
fn cancellation_of_an_already_exited_process_group_is_successful() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_exited_group");
    let mut owner = ReapingChild(
        Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn exited owner"),
    );
    let owner_pid = owner.id();
    owner.wait().expect("reap exited owner");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), owner_pid)
        .expect("mark running");

    let result = runtime
        .cancel_job_run(&run.run_id)
        .expect("cancel exited owner");

    assert_eq!(result.signal_outcome.as_deref(), Some("already_exited"));
    assert_eq!(
        runtime.show_job_run(&run.run_id).expect("show run").state,
        JobRunState::Cancelled
    );
}

#[cfg(unix)]
#[test]
fn cancellation_survivor_returns_typed_evidence_without_finalizing_run() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_survivor");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark running");

    let error = runtime
        .cancel_job_run_with_signaller(&run.run_id, "tester", "unit", |_| {
            Err(OrbitError::RunCancellationIncomplete {
                pid: 4242,
                pgid: Some(4242),
                term_sent: true,
                kill_sent: true,
                leader_alive: true,
                group_alive: true,
            })
        })
        .expect_err("a survivor must fail cancellation");
    assert!(matches!(
        error,
        OrbitError::RunCancellationIncomplete {
            pid: 4242,
            pgid: Some(4242),
            term_sent: true,
            kill_sent: true,
            leader_alive: true,
            group_alive: true,
        }
    ));
    assert_eq!(
        runtime.show_job_run(&run.run_id).expect("show run").state,
        JobRunState::Running,
        "a failed liveness verification must not finalize the run"
    );
    assert!(
        runtime
            .active_job_run_cancellation_request(&run.run_id)
            .expect("read cancellation audit")
            .is_none(),
        "a failed signal attempt must not make a later worker exit look cancellation-induced"
    );
    assert!(
        runtime
            .list_session_events(20)
            .expect("list events")
            .iter()
            .all(|event| event.event_type != "JobRunCancelled"),
        "a failed cancellation must not emit a success audit event"
    );
}

#[cfg(unix)]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

// ─── child-dispatch cancellation policy [ORB-10971] ───────────────────────

use orbit_types::workflow::{ChildDispatch, ChildDispatchPhase, PipelineState};

/// A parent parked in a blocking dispatch wait, plus the child it submitted.
fn parent_waiting_on_child(
    runtime: &OrbitRuntime,
    parent_job: &str,
    child_job: &str,
    blocking: bool,
) -> (JobRun, JobRun) {
    let parent = insert_pending_run(runtime, parent_job);
    let child = insert_pending_run(runtime, child_job);

    let mut state = PipelineState::new(
        parent.run_id.clone(),
        parent_job.to_string(),
        serde_json::json!({}),
    );
    let action = if blocking {
        "invoke_and_wait"
    } else {
        "invoke_detached"
    };
    state.record_child_dispatch(
        ChildDispatch::submitted(
            child.run_id.clone(),
            child_job.to_string(),
            action.to_string(),
            blocking,
            false,
            Utc::now(),
        )
        .with_parent_step_id(Some("ship_leaves".to_string())),
    );
    state.advance_child_dispatch(&child.run_id, ChildDispatchPhase::Waiting, None, None);
    runtime
        .stores()
        .jobs()
        .write_run_state(&parent.run_id, &state)
        .expect("seed parent dispatch state");
    (parent, child)
}

fn dispatch_after_cancel(runtime: &OrbitRuntime, parent_run_id: &str) -> ChildDispatch {
    runtime
        .read_run_state(parent_run_id)
        .expect("read parent state")
        .expect("parent state")
        .child_dispatches
        .into_iter()
        .next()
        .expect("the child link must survive cancellation")
}

#[test]
fn cancelling_a_waiting_parent_cascades_to_its_blocking_child_and_keeps_the_link() {
    let (_root, runtime) = test_runtime();
    let (parent, child) = parent_waiting_on_child(
        &runtime,
        "workspace_auto_pipeline",
        "task_auto_pipeline",
        true,
    );

    runtime
        .cancel_job_run_with_context(&parent.run_id, "operator", "cli")
        .expect("cancel the waiting parent");

    let dispatch = dispatch_after_cancel(&runtime, &parent.run_id);
    assert_eq!(dispatch.child_run_id, child.run_id);
    assert_eq!(dispatch.parent_step_id.as_deref(), Some("ship_leaves"));
    assert_eq!(
        dispatch.phase,
        ChildDispatchPhase::Terminal,
        "the wait step must not keep rendering as running"
    );
    let cancellation = dispatch.cancellation.expect("cancellation is recorded");
    assert_eq!(cancellation.policy.as_str(), "cascade");
    assert_eq!(cancellation.outcome, "cancelled");

    assert_eq!(
        runtime
            .show_job_run(&child.run_id)
            .expect("show child")
            .state,
        JobRunState::Cancelled,
        "the parent's wait was the child's only consumer",
    );
}

#[test]
fn cancelling_a_parent_leaves_its_detached_child_running() {
    let (_root, runtime) = test_runtime();
    let (parent, child) =
        parent_waiting_on_child(&runtime, "workspace_auto_pipeline", "epic_pipeline", false);

    runtime
        .cancel_job_run_with_context(&parent.run_id, "operator", "cli")
        .expect("cancel the parent");

    let dispatch = dispatch_after_cancel(&runtime, &parent.run_id);
    assert_eq!(dispatch.child_run_id, child.run_id);
    assert_eq!(dispatch.phase, ChildDispatchPhase::Terminal);
    let cancellation = dispatch.cancellation.expect("cancellation is recorded");
    assert_eq!(cancellation.policy.as_str(), "detach");
    assert_eq!(cancellation.outcome, "detached");

    assert_eq!(
        runtime
            .show_job_run(&child.run_id)
            .expect("show child")
            .state,
        JobRunState::Pending,
        "a detached child was dispatched to outlive the parent's step",
    );
}

#[test]
fn a_child_that_finished_first_is_recorded_rather_than_failing_the_parent_cancel() {
    let (_root, runtime) = test_runtime();
    let (parent, child) = parent_waiting_on_child(
        &runtime,
        "workspace_auto_pipeline",
        "task_auto_pipeline",
        true,
    );
    runtime
        .cancel_job_run(&child.run_id)
        .expect("child terminalizes on its own first");

    runtime
        .cancel_job_run_with_context(&parent.run_id, "operator", "cli")
        .expect("losing the race to the child must not block the parent's cancel");

    let cancellation = dispatch_after_cancel(&runtime, &parent.run_id)
        .cancellation
        .expect("cancellation is recorded");
    assert_eq!(cancellation.outcome, "already_terminal");
    assert!(cancellation.error.is_some(), "the reason is kept verbatim");
}
