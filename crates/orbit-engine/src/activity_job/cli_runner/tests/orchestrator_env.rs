#![allow(missing_docs)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use orbit_agent::loop_engine::audit::AuditSink;
use orbit_types::workflow::activity_job::V2AuditEventKind;
use tempfile::tempdir;

use crate::context::{ProvenanceEnv, provenance_env};

use super::super::super::audit_writer::V2AuditWriter;
use super::super::run_cli_backend;
use super::test_support::{RecordingSink, TestHost, test_agent_loop_spec_for, write_executable};

#[test]
fn run_cli_backend_exports_runtime_identity_for_subprocess_tools() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
if [ "$ORBIT_AGENT_NAME" = "grok" ] && [ "$ORBIT_AGENT_MODEL" = "grok-build" ]; then
  printf '%s\n' '{"schemaVersion":1,"status":"success","result":{"identity":"ok"},"error":null}'
else
  printf '%s\n' '{"schemaVersion":1,"status":"failed","error":{"code":"identity_env_missing","message":"runtime identity env was not propagated","details":null}}'
  exit 1
fi
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-identity-env",
        "grok:grok-build",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-identity-env",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success);
    assert_eq!(outcome.output["provider"], "grok");
}

/// ORB-10342: pipeline-gate provider spawns must carry the same
/// AGENT_RUN_ID/AGENT_MODEL/AGENT_TASK trio the worker path sets
/// (ORB-10340), so the shared prepare-commit-msg injector can stamp commit
/// trailers regardless of which spawner produced the commit.
#[test]
fn run_cli_backend_sets_agent_telemetry_env_vars_for_commit_trailers() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
if [ "$AGENT_RUN_ID" = "job-grok-telemetry" ] && [ "$AGENT_MODEL" = "grok-build" ] && [ "$AGENT_TASK" = "ORB-10342" ]; then
  printf '%s\n' '{"schemaVersion":1,"status":"success","result":{"identity":"ok"},"error":null}'
else
  printf '%s\n' '{"schemaVersion":1,"status":"failed","error":{"code":"telemetry_env_missing","message":"agent telemetry env was not propagated","details":null}}'
  exit 1
fi
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-telemetry",
        "grok:grok-build",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-telemetry",
        audit,
        &serde_json::json!({"prompt": "hi", "task_id": "ORB-10342"}),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success);
    assert_eq!(outcome.output["provider"], "grok");
}

/// [ORB-10917] End-to-end guard for the composed dispatch environment: a
/// benignly named ambient credential must not survive into the provider child.
/// The ambient value is set by this test rather than inherited from the
/// developer's shell, and the child reports what it actually saw so a
/// regression fails loudly instead of silently forwarding.
#[test]
fn run_cli_backend_does_not_forward_benignly_named_ambient_credentials() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
if [ -z "$DATABASE_URL" ] && [ -z "$BILLING_ENDPOINT" ] && [ -n "$PATH" ]; then
  printf '%s\n' '{"schemaVersion":1,"status":"success","result":{"identity":"ok"},"error":null}'
else
  printf '{"schemaVersion":1,"status":"failed","error":{"code":"ambient_env_leaked","message":"DATABASE_URL=%s BILLING_ENDPOINT=%s","details":null}}\n' "$DATABASE_URL" "$BILLING_ENDPOINT"
  exit 1
fi
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-env-allowlist",
        "grok:grok-build",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());

    let _ambient = orbit_common::test_env::scoped([
        ("DATABASE_URL", Some("postgres://svc:hunter2@db.internal")),
        ("BILLING_ENDPOINT", Some("https://billing.internal.example")),
    ]);
    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-env-allowlist",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");

    assert!(
        outcome.success,
        "ambient credentials leaked into the provider child: {:?}",
        outcome.output
    );
}

/// [ORB-11607] An operator override in the dispatching process must not reach
/// the untrusted provider child. The `ORBIT_` envelope is an explicit name
/// set, not a prefix wildcard, so `ORBIT_OPERATOR` stays with the parent.
#[test]
fn run_cli_backend_does_not_forward_ambient_operator_override() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
if [ -z "${ORBIT_OPERATOR+x}" ]; then
  printf '%s\n' '{"schemaVersion":1,"status":"success","result":{"identity":"ok"},"error":null}'
else
  printf '{"schemaVersion":1,"status":"failed","error":{"code":"operator_env_leaked","message":"ORBIT_OPERATOR=%s","details":null}}\n' "$ORBIT_OPERATOR"
  exit 1
fi
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-operator-env-deny",
        "grok:grok-build",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());

    let _ambient = orbit_common::test_env::scoped([("ORBIT_OPERATOR", Some("1"))]);
    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-operator-env-deny",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");

    assert!(
        outcome.success,
        "ORBIT_OPERATOR leaked into the provider child: {:?}",
        outcome.output
    );
}

/// [ORB-10909] CLI-runner dispatch must inject the registry locator from the host so a
/// spawned agent whose HOME does not contain the Orbit registry can still
/// resolve `orbit tool run` against the dispatching run's root.
#[test]
fn run_cli_backend_injects_managed_registry_root_from_host() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
if [ "$ORBIT_REGISTRY_ROOT" = "/resolved/orbit/root" ] && [ -z "$ORBIT_ROOT" ] && [ "$ORBIT_WORKSPACE" = "ws_orbit" ]; then
  printf '%s\n' '{"schemaVersion":1,"status":"success","result":{"identity":"ok"},"error":null}'
else
  printf '%s\n' '{"schemaVersion":1,"status":"failed","error":{"code":"registry_root_missing","message":"managed registry routing was not isolated","details":null}}'
  exit 1
fi
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-orbit-root",
        "grok:grok-build",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: None,
        workspace_root: None,
        orbit_registry_root: Some("/resolved/orbit/root".to_string()),
        orbit_workspace_selector: Some("ws_orbit".to_string()),
    };
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-orbit-root",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success);
    assert_eq!(outcome.output["provider"], "grok");
}

#[test]
fn run_cli_backend_injects_orbit_scratch_dir_under_workspace() {
    let temp = tempdir().expect("tempdir");
    let workspace = temp.path().join("worktree");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let expected_scratch = workspace
        .canonicalize()
        .expect("canonical workspace")
        .join(".orbit")
        .join("tmp");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        &format!(
            r#"#!/bin/sh
cat > /dev/null
if [ "$ORBIT_SCRATCH_DIR" = "{scratch}" ] && [ -d "$ORBIT_SCRATCH_DIR" ]; then
  printf '%s\n' '{{"schemaVersion":1,"status":"success","result":{{"scratch":"ok"}},"error":null}}'
else
  printf '%s\n' "{{\"schemaVersion\":1,\"status\":\"failed\",\"error\":{{\"code\":\"scratch_dir_missing\",\"message\":\"ORBIT_SCRATCH_DIR=$ORBIT_SCRATCH_DIR\",\"details\":null}}}}"
  exit 1
fi
"#,
            scratch = expected_scratch.display(),
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-scratch-dir",
        "grok:grok-build",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: None,
        workspace_root: Some(workspace.clone()),
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-scratch-dir",
        audit,
        &serde_json::json!({
            "prompt": "hi",
            "workspace_path": workspace
        }),
        None,
    )
    .expect("run succeeds");

    assert!(
        outcome.success,
        "provider did not receive ORBIT_SCRATCH_DIR: {:?}",
        outcome.output
    );
    assert!(
        expected_scratch.is_dir(),
        "dispatch must create {}",
        expected_scratch.display()
    );
}

/// [ORB-10980] A managed run executes in a linked worktree whose workspace and
/// worktree-local `.orbit` state roots are mounted read-only. The child must be
/// routed to the authoritative registry root the host reports without turning
/// that locator into an explicit workspace/data-root pin. The existing
/// binary/PATH pinning plus managed provenance bindings must survive alongside
/// it — those are what let the documented `orbit tool run` fallback work.
#[test]
fn run_cli_backend_injects_registry_root_not_worktree_state_root() {
    let temp = tempdir().expect("tempdir");
    let registry_root = temp.path().join("registry");
    let workspace_state_root = temp.path().join("repo").join(".orbit");
    let worktree_state_root = temp
        .path()
        .join("repo/.orbit/state/worktrees/jrun-fixture")
        .join(".orbit");
    for directory in [&registry_root, &workspace_state_root, &worktree_state_root] {
        std::fs::create_dir_all(directory).expect("state root");
    }
    // Both `.orbit` state roots are read-only in a managed run; a child pinned
    // to either cannot bootstrap, so make that concrete in the fixture.
    for directory in [&workspace_state_root, &worktree_state_root] {
        let mut permissions = std::fs::metadata(directory)
            .expect("state root metadata")
            .permissions();
        permissions.set_mode(0o555);
        std::fs::set_permissions(directory, permissions).expect("read-only state root");
    }

    let script = temp.path().join("grok");
    write_executable(
        &script,
        &format!(
            r#"#!/bin/sh
cat > /dev/null
fail() {{
  printf '%s\n' "{{\"schemaVersion\":1,\"status\":\"failed\",\"error\":{{\"code\":\"$1\",\"message\":\"$1\",\"details\":null}}}}"
  exit 1
}}
[ "$ORBIT_REGISTRY_ROOT" = "{registry}" ] || fail registry_root_not_authoritative
[ -z "$ORBIT_ROOT" ] || fail operator_root_leaked_into_managed_child
[ "$ORBIT_WORKSPACE" = "ws_orbit" ] || fail workspace_selector_missing
[ "$ORBIT_WORKSPACE" = "daniel-e9c542" ] && fail workspace_selector_is_worktree_identity
[ "$ORBIT_REGISTRY_ROOT" = "{workspace_state}" ] && fail registry_root_is_workspace_state_root
[ "$ORBIT_REGISTRY_ROOT" = "{worktree_state}" ] && fail registry_root_is_worktree_state_root
[ -n "$ORBIT_BIN" ] || fail orbit_bin_missing
[ -n "$PATH" ] || fail path_missing
[ "$ORBIT_RUN_ID" = "job-grok-registry-root" ] || fail run_id_missing
[ "$ORBIT_MANAGED_RUN_CONTEXT" = "1" ] || fail managed_run_context_missing
[ "$ORBIT_TASK_ACTOR_KIND" = "agent" ] || fail actor_kind_missing
[ "$ORBIT_ACTIVITY_TOOLS" = "orbit.task.show,proc.spawn,github.run.list" ] || fail activity_tools_missing
[ "$ORBIT_PROC_ALLOWED_PROGRAMS" = "git,rg" ] || fail proc_programs_missing
[ "$ORBIT_ACTIVITY_FS_PROFILE" = "unrestricted" ] || fail fs_profile_missing
[ "$ORBIT_ACTIVE_TASK_ID" = "ORB-10980" ] || fail active_task_missing
printf '%s\n' '{{"schemaVersion":1,"status":"success","result":{{"identity":"ok"}},"error":null}}'
"#,
            registry = registry_root.display(),
            workspace_state = workspace_state_root.display(),
            worktree_state = worktree_state_root.display(),
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-registry-root",
        "grok:grok-build",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: Some(serde_json::json!({
            "id": "ORB-10980",
            "required_tools": ["github.run.list"]
        })),
        workspace_root: None,
        orbit_registry_root: Some(registry_root.display().to_string()),
        orbit_workspace_selector: Some("ws_orbit".to_string()),
    };
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());
    spec.tools = vec!["orbit.task.show".to_string(), "proc.spawn".to_string()];
    spec.proc_allowed_programs = Some(vec!["git".to_string(), "rg".to_string()]);
    let ambient_root = registry_root.to_string_lossy().into_owned();
    let _ambient = orbit_common::test_env::scoped([("ORBIT_ROOT", Some(ambient_root.as_str()))]);

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-registry-root",
        audit.clone(),
        &serde_json::json!({"prompt": "hi", "task_id": "ORB-10980"}),
        None,
    )
    .expect("run succeeds");

    // Restore write permission so the fixture's temp tree can be reclaimed.
    for directory in [&workspace_state_root, &worktree_state_root] {
        let mut permissions = std::fs::metadata(directory)
            .expect("state root metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(directory, permissions).expect("restore state root");
    }

    assert!(
        outcome.success,
        "managed child rejected its Orbit environment: {:?}",
        outcome.output
    );
    let events = audit.events_snapshot().expect("audit snapshot");
    let delegated = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::ToolAllowlistHarnessDelegated {
                task_id,
                task_ids,
                requested_tools,
                effective_tools,
                tools,
                ..
            } => Some((task_id, task_ids, requested_tools, effective_tools, tools)),
            _ => None,
        })
        .expect("tool allowlist audit event");
    assert_eq!(delegated.0.as_deref(), Some("ORB-10980"));
    assert_eq!(delegated.1, &["ORB-10980"]);
    assert_eq!(delegated.2, &["github.run.list"]);
    assert_eq!(
        delegated.3,
        &["orbit.task.show", "proc.spawn", "github.run.list"]
    );
    assert_eq!(delegated.4, delegated.3);
}

/// AGENT_MODEL/AGENT_TASK must be omitted (unset), not set to an empty
/// string, when the model or task id is unknown — mirrors ORB-10340's
/// worker-side semantics. AGENT_RUN_ID is always known for a dispatched run.
// Asserted at the builder boundary, not through a spawned child: the composed child env
// forwards the named `ORBIT_*` envelope, so an Orbit-dispatched test run's own run/task
// identity can reach the child and mask a correct omission. The positive propagation case
// above still covers the wiring end-to-end.
#[test]
fn run_cli_backend_omits_agent_model_and_task_env_vars_when_unknown() {
    let vars = provenance_env(ProvenanceEnv {
        orbit_run_id: Some("job-grok-telemetry-unknown"),
        orbit_managed_run_context: true,
        orbit_agent_name: Some("grok"),
        agent_run_id: Some("job-grok-telemetry-unknown"),
        ..ProvenanceEnv::default()
    });

    assert!(vars.contains(&(
        "AGENT_RUN_ID".to_string(),
        "job-grok-telemetry-unknown".to_string()
    )));
    assert!(!vars.iter().any(|(key, _)| key == "AGENT_MODEL"));
    assert!(!vars.iter().any(|(key, _)| key == "AGENT_TASK"));
}
