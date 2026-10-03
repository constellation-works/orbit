//! Exercises the real `AuditGuard::Drop` against an in-memory runtime: a CLI
//! `tool run` produces exactly one audit row whether the runtime records it
//! (the guard suppresses its own) or the CLI bails before the runtime is
//! reached (the guard records it).

use orbit_core::adapter::command::take_tool_audit_recorded;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::json;

use super::super::audit_middleware::*;

fn fresh_runtime() -> OrbitRuntime {
    // Reset the dedup signal so cross-test thread-local leakage
    // cannot mask a real bug in the per-call set/clear cycle.
    let _ = take_tool_audit_recorded();
    OrbitRuntime::in_memory().expect("build in-memory runtime")
}

fn tool_run_meta(tool_name: &str) -> CommandMeta {
    CommandMeta {
        command: "tool".to_string(),
        subcommand: Some("run".to_string()),
        tool_name: Some(tool_name.to_string()),
        target_type: Some("tool".to_string()),
        target_id: Some(tool_name.to_string()),
        role: "agent".to_string(),
        arguments_json: None,
        job_run_id: None,
    }
}

fn count_rows(runtime: &OrbitRuntime, tool_name: &str) -> usize {
    runtime
        .list_audit_events(None, Some(tool_name.to_string()), None, None, 16)
        .expect("list audit events")
        .len()
}

#[test]
fn success_via_runtime_yields_exactly_one_row() {
    let runtime = fresh_runtime();
    {
        let mut guard = AuditGuard::new(&runtime, tool_run_meta("orbit.search"));
        let result = runtime.execute_tool_command(
            "orbit.search",
            json!({ "query": "anything" }),
            None,
            None,
        );
        assert!(result.is_ok());
        guard.mark_success();
    }
    assert_eq!(
        count_rows(&runtime, "orbit.search"),
        1,
        "runtime owns the row, guard suppressed"
    );
}

#[test]
fn invalid_json_bail_before_runtime_yields_exactly_one_row() {
    let runtime = fresh_runtime();
    {
        let mut guard = AuditGuard::new(&runtime, tool_run_meta("orbit.search"));
        // Simulate a CLI invalid-JSON parse failure that happens
        // before `execute_tool_command` is reached.
        let parse_err = OrbitError::InvalidInput("invalid JSON input: ...".to_string());
        guard.mark_failure(&parse_err);
        // Guard drops here without the runtime ever recording an
        // audit row.
    }
    assert_eq!(
        count_rows(&runtime, "orbit.search"),
        1,
        "guard records its own row when runtime is never reached"
    );
}
