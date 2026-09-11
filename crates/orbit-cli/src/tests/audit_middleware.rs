use clap::Parser;
use orbit_types::telemetry::AuditEvent;
use serde_json::{Value, json};

use crate::command::{Cli, Payload};

use super::super::audit_middleware::*;
use orbit_common::test_env::{self, AGENT_IDENTITY_ENV};
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_core::context::ActorKind;
use orbit_core::{ActorIdentity, OrbitError, OrbitRuntime};
use orbit_types::telemetry::AuditEventStatus;

fn meta_for(args: &[&str]) -> CommandMeta {
    let cli = Cli::parse_from(args);
    extract_command_meta(&cli.command)
}

fn audit_event_for_meta_without_orbit_run_id(meta: CommandMeta) -> AuditEvent {
    let _env = test_env::unset(["ORBIT_RUN_ID"]);
    let runtime = OrbitRuntime::in_memory().expect("build in-memory runtime");
    {
        let mut guard = AuditGuard::new(&runtime, meta);
        guard.mark_success();
    }

    let events = runtime
        .list_audit_events(None, None, Some(AuditEventStatus::Success), None, 8)
        .expect("list audit events");
    assert_eq!(events.len(), 1);
    events.into_iter().next().expect("single audit event")
}

fn audit_event_for_actor(actor: ActorIdentity) -> AuditEvent {
    let runtime = OrbitRuntime::in_memory()
        .expect("build in-memory runtime")
        .with_actor(actor.clone());
    let meta = Cli::parse_from(["orbit", "search", "actor"])
        .command
        .operation()
        .attribute_to(&actor)
        .audit_meta
        .expect("search is audited");
    {
        let mut guard = AuditGuard::new(&runtime, meta);
        guard.mark_success();
    }

    runtime
        .list_audit_events(None, None, Some(AuditEventStatus::Success), None, 8)
        .expect("list audit events")
        .into_iter()
        .next()
        .expect("single audit event")
}

#[test]
fn nonzero_payload_exit_is_audited_as_failure() {
    let runtime = OrbitRuntime::in_memory().expect("build in-memory runtime");
    {
        let mut guard = AuditGuard::new(&runtime, meta_for(&["orbit", "doctor"]));
        let result = Ok(Payload::document(json!({"status": "error"}))
            .with_exit_code(1)
            .into());
        guard.mark_result(&result);
    }

    let events = runtime
        .list_audit_events(None, None, Some(AuditEventStatus::Failure), None, 8)
        .expect("list audit events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].command, "doctor");
    assert_eq!(events[0].exit_code, 1);
}

#[test]
fn cli_agent_envelope_records_canonical_actor_family() {
    let _env = test_env::unset(AGENT_IDENTITY_ENV.iter().copied());
    // SAFETY: the shared test-env guard serializes and restores this mutation.
    unsafe {
        std::env::set_var("ORBIT_AGENT_MODEL", "gpt-5.6-sol");
    }

    let actor = ActorIdentity::from_env();
    assert_eq!(actor.kind, ActorKind::Agent);
    assert_eq!(actor.label, "codex");
    assert_eq!(audit_event_for_actor(actor).role, "codex");
}

#[test]
fn cli_without_agent_envelope_records_unknown_actor() {
    let _env = test_env::unset(AGENT_IDENTITY_ENV.iter().copied().chain(["ORBIT_OPERATOR"]));

    let actor = ActorIdentity::from_env();
    assert_eq!(actor.kind, ActorKind::Unknown);
    assert_eq!(actor.label, "unknown");
    assert_eq!(audit_event_for_actor(actor).role, "unknown");
}

#[test]
fn cli_operator_override_without_agent_envelope_records_operator_actor() {
    // ORB-12115: a grant enabled under `ORBIT_OPERATOR=1` previously recorded
    // `actor: "unknown"` even though the override is itself an audited,
    // deliberate act — it should name who enabled it.
    let _env = test_env::scoped(
        AGENT_IDENTITY_ENV
            .iter()
            .map(|name| (*name, None))
            .chain([("ORBIT_OPERATOR", Some("1"))]),
    );

    let actor = ActorIdentity::from_env();
    assert_eq!(actor.kind, ActorKind::Human);
    assert_eq!(actor.label, "operator");
    assert_eq!(audit_event_for_actor(actor).role, "operator");
}

#[test]
fn run_ship_audit_meta_uses_unified_workflow_alias() {
    let pr = meta_for(&["orbit", "run", "ship", "T1"]);
    assert_eq!(pr.command, "run");
    assert_eq!(pr.subcommand.as_deref(), Some("ship"));
    assert_eq!(pr.target_type.as_deref(), Some("workflow"));
    assert_eq!(pr.target_id.as_deref(), Some("ship"));

    let local = meta_for(&["orbit", "run", "ship", "-m", "local", "T1"]);
    assert_eq!(local.subcommand.as_deref(), Some("ship"));
    assert_eq!(local.target_type.as_deref(), Some("workflow"));
    assert_eq!(local.target_id.as_deref(), Some("ship"));
}

#[test]
fn run_ship_local_audit_meta_uses_deprecated_top_level_command() {
    let meta = meta_for(&["orbit", "run", "ship-local", "T1"]);
    assert_eq!(meta.command, "run");
    assert_eq!(meta.subcommand.as_deref(), Some("ship-local"));
    assert_eq!(meta.target_type.as_deref(), Some("workflow"));
    assert_eq!(meta.target_id.as_deref(), Some("ship-local"));
}

#[test]
fn tool_run_audit_meta_uses_agent_flags_for_role() {
    let meta = meta_for(&[
        "orbit",
        "tool",
        "run",
        "orbit.search",
        "--agent",
        "codex",
        "--model",
        TEST_CODEX_MODEL,
    ]);

    assert_eq!(meta.command, "tool");
    assert_eq!(meta.subcommand.as_deref(), Some("run"));
    assert_eq!(meta.tool_name.as_deref(), Some("orbit.search"));
    assert_eq!(meta.role, TEST_CODEX_MODEL);
}

#[test]
fn tool_run_audit_meta_uses_input_identity_for_role() {
    let meta = meta_for(&[
        "orbit",
        "tool",
        "run",
        "orbit.search",
        "--input",
        r#"{"query":"actor","agent":"codex","model":"gpt-5.5"}"#,
    ]);

    assert_eq!(meta.role, TEST_CODEX_MODEL);
}

#[test]
fn tool_run_audit_meta_uses_model_only_input_for_role() {
    let meta = meta_for(&[
        "orbit",
        "tool",
        "run",
        "orbit.search",
        "--input",
        r#"{"query":"actor","model":"gpt-5.5"}"#,
    ]);

    assert_eq!(meta.role, TEST_CODEX_MODEL);
}

#[test]
fn tool_run_audit_meta_prefers_input_identity_over_flags() {
    let meta = meta_for(&[
        "orbit",
        "tool",
        "run",
        "orbit.search",
        "--agent",
        "codex",
        "--model",
        TEST_CODEX_MODEL,
        "--input",
        r#"{"query":"actor","agent":"claude","model":"opus-4.6"}"#,
    ]);

    assert_eq!(meta.role, "opus-4.6");
}

#[test]
fn tool_run_audit_meta_uses_agent_role_without_identity() {
    // Without flag/input identity the role falls back to the process env, so
    // the assertion is only meaningful with that env cleared (ORB-10350).
    let _env = test_env::unset(AGENT_IDENTITY_ENV.iter().copied());
    let meta = meta_for(&["orbit", "tool", "run", "orbit.search"]);

    assert_eq!(meta.role, "agent");
}

#[test]
fn search_audit_meta_preserves_kind_discriminator() {
    // ORB-00202: `orbit search --kind X` collapsed three per-domain
    // `<command> search` rows into one. ORB-00205 then prefixed the
    // subcommand with the search mode (`query` / `similar` / `path`),
    // so a query-mode search of kind `task` emits `query:task`. The
    // `--kind` value is still preserved on the right side of the colon
    // so downstream audit queries can distinguish task and doc searches.
    let task = meta_for(&["orbit", "search", "foo", "--kind", "task"]);
    assert_eq!(task.command, "search");
    assert_eq!(task.subcommand.as_deref(), Some("query:task"));
    assert_eq!(task.target_type.as_deref(), Some("search"));

    let doc = meta_for(&["orbit", "search", "foo", "--kind", "doc"]);
    assert_eq!(doc.subcommand.as_deref(), Some("query:doc"));

    // Default `--kind all` is captured explicitly rather than left blank.
    let all = meta_for(&["orbit", "search", "foo"]);
    assert_eq!(all.subcommand.as_deref(), Some("query:all"));
}

#[test]
fn job_run_pipeline_worker_audit_uses_static_run_id_without_env() {
    let meta = meta_for(&["orbit", "job", "run-pipeline-worker", "jrun-worker"]);
    assert_eq!(meta.command, "job");
    assert_eq!(meta.subcommand.as_deref(), Some("run-pipeline-worker"));
    assert_eq!(meta.target_type.as_deref(), Some("job_run"));
    assert_eq!(meta.target_id.as_deref(), Some("jrun-worker"));
    assert_eq!(meta.job_run_id.as_deref(), Some("jrun-worker"));

    let row = audit_event_for_meta_without_orbit_run_id(meta);
    assert_eq!(row.command, "job");
    assert_eq!(row.subcommand.as_deref(), Some("run-pipeline-worker"));
    assert_eq!(row.target_id.as_deref(), Some("jrun-worker"));
    assert_eq!(row.job_run_id.as_deref(), Some("jrun-worker"));
}

#[test]
fn job_replay_audit_uses_static_run_id_without_env() {
    let meta = meta_for(&["orbit", "job", "replay", "jrun-source"]);
    assert_eq!(meta.command, "job");
    assert_eq!(meta.subcommand.as_deref(), Some("replay"));
    assert_eq!(meta.target_type.as_deref(), Some("job_run"));
    assert_eq!(meta.target_id.as_deref(), Some("jrun-source"));
    assert_eq!(meta.job_run_id.as_deref(), Some("jrun-source"));

    let row = audit_event_for_meta_without_orbit_run_id(meta);
    assert_eq!(row.command, "job");
    assert_eq!(row.subcommand.as_deref(), Some("replay"));
    assert_eq!(row.target_id.as_deref(), Some("jrun-source"));
    assert_eq!(row.job_run_id.as_deref(), Some("jrun-source"));
}

#[test]
fn audit_guard_event_json_shapes_are_snapshotted() {
    let events = vec![
        audit_guard_event_json(AuditEventStatus::Success),
        audit_guard_event_json(AuditEventStatus::Failure),
        audit_guard_event_json(AuditEventStatus::Denied),
    ];

    let actual = serde_json::to_string_pretty(&events).expect("serialize audit snapshot");
    assert_eq!(
        actual,
        include_str!("../snapshots/audit_guard_event_json_shapes.json").trim_end()
    );
}

fn audit_guard_event_json(status: AuditEventStatus) -> Value {
    let _ = orbit_core::adapter::command::take_tool_audit_recorded();
    let runtime = OrbitRuntime::in_memory().expect("build in-memory runtime");
    {
        let mut guard = AuditGuard::new(&runtime, snapshot_meta());
        match status {
            AuditEventStatus::Success => guard.mark_success(),
            AuditEventStatus::Failure => {
                let error = OrbitError::InvalidInput("snapshot failure".to_string());
                guard.mark_failure(&error);
            }
            AuditEventStatus::Denied => guard.mark_denied("snapshot denied"),
        }
    }

    let events = runtime
        .list_audit_events(
            None,
            Some("orbit.task.update".to_string()),
            Some(status),
            None,
            8,
        )
        .expect("list audit events");
    assert_eq!(events.len(), 1);
    let mut value = serde_json::to_value(&events[0]).expect("serialize audit event");
    normalize_audit_event_json(&mut value);
    value
}

fn snapshot_meta() -> CommandMeta {
    CommandMeta {
        command: "tool".to_string(),
        subcommand: Some("run".to_string()),
        tool_name: Some("orbit.task.update".to_string()),
        target_type: Some("tool".to_string()),
        target_id: Some("orbit.task.update".to_string()),
        role: TEST_CODEX_MODEL.to_string(),
        arguments_json: Some(r#"{"id":"ORB-00002","model":"gpt-5.5"}"#.to_string()),
        job_run_id: None,
    }
}

fn normalize_audit_event_json(value: &mut Value) {
    let object = value
        .as_object_mut()
        .expect("audit event serializes to object");
    object.insert("id".to_string(), json!(1));
    object.insert("execution_id".to_string(), json!("<execution_id>"));
    object.insert("timestamp".to_string(), json!("<timestamp>"));
    object.insert("duration_ms".to_string(), json!(0));
    object.insert(
        "working_directory".to_string(),
        json!("<working_directory>"),
    );
    object.insert("host".to_string(), json!("<host>"));
    object.insert("pid".to_string(), json!(0));
    object.insert("task_id".to_string(), Value::Null);
    object.insert("job_run_id".to_string(), Value::Null);
    object.insert("activity_id".to_string(), Value::Null);
    object.insert("step_index".to_string(), Value::Null);
}

/// Integration tests that exercise the real `AuditGuard::Drop` against an
/// in-memory runtime, covering the four CLI `tool run` paths the
/// dedup mechanism must handle: success-via-runtime (suppress guard
/// emission), failure-via-runtime (suppress guard emission), invalid
/// JSON / missing input (guard records its own row), and `--dry-run`
/// (guard records its own row). All four must produce exactly one
/// audit row.
mod cli_dedup_invariant {
    use super::*;
    use orbit_core::adapter::command::take_tool_audit_recorded;
    use serde_json::json;

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
    fn dispatch_failure_via_runtime_yields_exactly_one_row() {
        let runtime = fresh_runtime();
        {
            let mut guard = AuditGuard::new(&runtime, tool_run_meta("orbit.task.show"));
            let result = runtime.execute_tool_command("orbit.task.show", json!({}), None, None);
            match &result {
                Ok(_) => panic!("expected dispatch failure"),
                Err(err) => guard.mark_failure(err),
            }
        }
        assert_eq!(
            count_rows(&runtime, "orbit.task.show"),
            1,
            "runtime owns the row even on dispatch failure"
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

    #[test]
    fn dry_run_bail_before_runtime_yields_exactly_one_row() {
        let runtime = fresh_runtime();
        {
            let mut guard = AuditGuard::new(&runtime, tool_run_meta("orbit.search"));
            // `--dry-run` returns Ok without invoking the runtime.
            guard.mark_success();
        }
        assert_eq!(
            count_rows(&runtime, "orbit.search"),
            1,
            "guard records its own row for the dry-run short-circuit"
        );
    }
}
