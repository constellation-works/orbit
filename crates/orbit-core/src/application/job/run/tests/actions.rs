//! Cancellation never signals a process that only reuses a recorded pid.

use std::process::{Command, Stdio};

use chrono::Utc;
use orbit_common::process::identity::{STABLE_TOKEN_PREFIX, process_start_identity_token};
use orbit_store::V2AuditEventInsertParams;
use orbit_types::workflow::JobRun;
use rusqlite::{Connection, params};

use super::super::owner::process_is_alive;
use super::{insert_pending_run, test_runtime};
use crate::OrbitRuntime;

struct ReapingChild(std::process::Child);

impl ReapingChild {
    fn id(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for ReapingChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

/// A process alone in its own process group, as Orbit spawns a provider CLI.
fn spawn_bystander() -> ReapingChild {
    use std::os::unix::process::CommandExt;

    let mut child = Command::new("sleep");
    child
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    ReapingChild(child.spawn().expect("spawn bystander"))
}

fn set_run_pid_start_time(runtime: &OrbitRuntime, run: &JobRun, token: &str) {
    let conn = Connection::open(runtime.global_root().join("orbit.db")).expect("open orbit db");
    conn.execute(
        "UPDATE job_runs SET pid_start_time = ?3 \
         WHERE workspace_id = ?1 AND run_id = ?2",
        params![
            runtime.workspace_id().expect("workspace id"),
            run.run_id,
            token,
        ],
    )
    .expect("set pid_start_time");
}

/// Record a provider process the run spawned, as the agent step's audit
/// envelope does.
fn write_provider_spawn(runtime: &OrbitRuntime, run_id: &str, pid: u32, pid_start_time: &str) {
    let event_id = "provider-recycled";
    let parent_event_id = format!("{event_id}-invocation");
    let event = serde_json::json!({
        "event_id": event_id,
        "ts": Utc::now().to_rfc3339(),
        "run_id": run_id,
        "parent_event_id": parent_event_id,
        "step_id": "agent_implement",
        "provider": "codex",
        "body_kind": "cli_invocation_process",
        "pid": pid,
        "pid_start_time": pid_start_time,
    });
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().expect("workspace id"),
            event_id: event_id.to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: "activity.progress".to_string(),
            ts: Utc::now(),
            run_id: run_id.to_string(),
            agent_identity: "test".to_string(),
            parent_event_id: Some(parent_event_id),
            workspace_path: None,
            payload_json: event.to_string(),
        })
        .expect("write provider audit event");
}

/// A recorded owner pid whose start token no longer matches names some other
/// process now. Cancellation must never signal it.
#[test]
fn cancel_job_run_does_not_signal_reused_pid_identity_mismatch() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_reused_pid");
    let sentinel = spawn_bystander();
    let sentinel_pid = sentinel.id();
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(
            &run.run_id,
            Utc::now() - chrono::Duration::seconds(1),
            sentinel_pid,
        )
        .expect("mark running");
    // A versioned token takes the strict `Mismatch` classification path.
    set_run_pid_start_time(
        &runtime,
        &run,
        &format!("{STABLE_TOKEN_PREFIX}definitely-not-the-sentinel-start-token"),
    );

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
}

/// The same guard for the provider processes a run recorded.
#[test]
fn cancel_job_run_leaves_a_process_that_only_reuses_the_provider_pid() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_provider_reused_pid");
    let bystander = spawn_bystander();
    let bystander_pid = bystander.id();
    if process_start_identity_token(bystander_pid).is_none() {
        // `ps` cannot run here (the macOS agent-executor sandbox), so no
        // identity is derivable and the comparison under test never happens.
        return;
    }
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), 999_999)
        .expect("mark running with a dead owner");
    write_provider_spawn(
        &runtime,
        &run.run_id,
        bystander_pid,
        &format!("{STABLE_TOKEN_PREFIX}the-original-provider-started-earlier"),
    );

    let result = runtime.cancel_job_run(&run.run_id).expect("cancel run");

    assert_eq!(result.provider_processes_stopped, 0);
    assert!(
        process_is_alive(bystander_pid),
        "a pid the provider record no longer identifies must not be signalled"
    );
}
