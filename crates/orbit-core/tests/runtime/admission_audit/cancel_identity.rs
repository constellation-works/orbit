//! Cancellation preserves concurrent worker progress and process identity.

use std::process::{Command, Stdio};

use chrono::Utc;
use orbit_common::process::identity::{
    ProbeOutcome, STABLE_TOKEN_PREFIX, probe_process_start_identity,
};
use orbit_store::V2AuditEventInsertParams;
use orbit_types::workflow::JobRun;
use rusqlite::{Connection, params};

use super::run_support::{insert_pending_run, test_runtime};
use orbit_common::process::identity::process_is_alive;
use orbit_core::OrbitRuntime;

use crate::scenarios::Outcome;

const PROBE_CONTROL: &str = "ORBIT_TEST_CANCEL_IDENTITY_PROBE";
const PROVIDER_ROW: &str = "cancel_job_run_leaves_a_process_that_only_reuses_the_provider_pid";

/// Observe the actual libtest command, including the table child's forwarding,
/// rather than calling the scenario or inspecting source text.
#[test]
fn provider_identity_probe_coverage_reaches_test_command() {
    let table = "admission_audit::cancel_identity::cancel_job_run_does_not_signal_reused_pid_identity_mismatch_scenarios";
    for unavailable in [true, false] {
        for selected_row in [None, Some(PROVIDER_ROW)] {
            let home = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
            let logs = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            orbit_common::test_env::clear_inherited_authority(|key| {
                command.env_remove(key);
            });
            // Keep libtest capture on: notices must survive it, including the
            // outer table's normal child-output forwarding when no row is set.
            command
                .args(["--exact", table, "--test-threads=1"])
                .env("HOME", home.path())
                .env("USERPROFILE", home.path())
                .env("ORBIT_TEST_TASK_FIXTURE_CHILD", table)
                .env_remove("ORBIT_RUNTIME_SCENARIO_ROW")
                .env_remove(PROBE_CONTROL)
                .current_dir(home.path());
            if unavailable {
                command.env(PROBE_CONTROL, "unavailable");
            }
            if let Some(row) = selected_row {
                command.env("ORBIT_RUNTIME_SCENARIO_ROW", row);
            }
            let output = orbit_common::test_env::run_child_test(&mut command, table, logs.path());
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let context = format!(
                "unavailable={unavailable}, selected={selected_row:?}:\n{stdout}\n{stderr}"
            );
            assert!(output.status.success(), "{context}");
            assert!(
                stdout
                    .lines()
                    .rev()
                    .find(|line| line.starts_with("test result: "))
                    .is_some_and(
                        |line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;")
                    ),
                "table entry point must run exactly once: {context}"
            );
            let pass = format!("qa-scenario-pass: {PROVIDER_ROW}");
            let skip = format!("SKIP: {PROVIDER_ROW}: ");
            let passed = stdout
                .lines()
                .chain(stderr.lines())
                .any(|line| line == pass);
            let notice = stdout
                .lines()
                .chain(stderr.lines())
                .find_map(|line| line.strip_prefix(&skip));
            if unavailable {
                assert_eq!(
                    notice,
                    Some("identity probe unavailable (deterministic control)"),
                    "unavailable cancellation coverage must reach the test command: {context}"
                );
                assert!(
                    !passed,
                    "unexecuted cancellation must earn no pass marker: {context}"
                );
                use std::io::Write;
                writeln!(std::io::stderr(), "test-note: unavailable provider PID-reuse control reported coverage without a pass marker (selected={selected_row:?})").unwrap();
            } else if let Some(reason) = notice {
                assert!(!reason.is_empty() && !passed, "{context}");
                // Forward only genuine host unavailability, never the injected
                // negative control's notice as missing native coverage.
                use std::io::Write;
                writeln!(std::io::stderr(), "{skip}{reason}").unwrap();
            } else {
                assert!(
                    passed,
                    "executed safety assertions must earn a pass marker: {context}"
                );
                use std::io::Write;
                writeln!(std::io::stderr(), "test-note: provider PID-reuse cancellation executed (selected={selected_row:?})").unwrap();
            }
        }
    }
}

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
fn cancel_job_run_does_not_signal_reused_pid_identity_mismatch() -> Outcome {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_reused_pid");
    let sentinel = spawn_bystander();
    let sentinel_pid = sentinel.id();
    crate::admission_audit::store_support::jobs(&runtime)
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
    Outcome::Passed
}

/// The same guard for the provider processes a run recorded.
fn cancel_job_run_leaves_a_process_that_only_reuses_the_provider_pid() -> Outcome {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_cancel_provider_reused_pid");
    let bystander = spawn_bystander();
    let bystander_pid = bystander.id();
    // Substitute only this fixture prerequisite. The available path and the
    // runtime's own cancellation probes always use the real native boundary.
    let forced_unavailable = std::env::var(PROBE_CONTROL).as_deref() == Ok("unavailable");
    let probe = if forced_unavailable {
        ProbeOutcome::Unavailable
    } else {
        probe_process_start_identity(bystander_pid)
    };
    match probe {
        ProbeOutcome::Token(_) => {}
        ProbeOutcome::NoProcess => {
            return Outcome::Unavailable("identity probe found no live bystander process".into());
        }
        ProbeOutcome::Unavailable => {
            let reason = if forced_unavailable {
                "identity probe unavailable (deterministic control)".into()
            } else {
                orbit_common::test_env::start_identity_probe_blocker().unwrap_or_else(|| {
                    "live bystander's process-start identity is inaccessible".into()
                })
            };
            return Outcome::Unavailable(reason);
        }
    }
    crate::admission_audit::store_support::jobs(&runtime)
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
    Outcome::Passed
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    cancel_job_run_does_not_signal_reused_pid_identity_mismatch_scenarios,
    cancel_job_run_does_not_signal_reused_pid_identity_mismatch,
    cancel_job_run_leaves_a_process_that_only_reuses_the_provider_pid
);
