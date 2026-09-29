#![allow(missing_docs)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use orbit_agent::loop_engine::audit::AuditSink;
use orbit_common::OrbitError;
use orbit_tools::plugin::BrokeredCaller;
use orbit_tools::{ToolContext, ToolRegistry};
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::workflow::activity_job::{
    ActivityToolPolicyMode, AgentLoopSpec, V2AuditEventKind,
};
use serde_json::json;
use tempfile::tempdir;

use crate::context::{ProvenanceEnv, provenance_env};

use super::super::super::audit_writer::V2AuditWriter;
use super::super::orchestrator::activity_tool_policy_env;
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

/// [ORB-13664] The claude provider's pinned env must reach the spawned child:
/// background tasks let a headless leaf hand off while its gates still run.
#[test]
fn run_cli_backend_disables_background_tasks_for_the_claude_child() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("claude");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
if [ "$CLAUDE_CODE_DISABLE_BACKGROUND_TASKS" = "1" ]; then
  printf '%s\n' '{"schemaVersion":1,"status":"success","result":{},"error":null}'
else
  printf '%s\n' '{"schemaVersion":1,"status":"failed","error":{"code":"background_tasks_enabled","message":"background task switch was not propagated","details":null}}'
  exit 1
fi
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-claude-background-env",
        "claude:opus",
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
    let spec = test_agent_loop_spec_for("claude", Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-claude-background-env",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success, "{}", outcome.output);
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

pub(super) fn policy_test_host(script: &std::path::Path, required_tools: &[&str]) -> TestHost {
    TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: Some(serde_json::json!({
            "id": "ORB-13315",
            "required_tools": required_tools,
        })),
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    }
}

/// A grok stand-in that fails with `$code` unless every shell `checks` holds.
pub(super) fn policy_checking_script(dir: &std::path::Path, checks: &str) -> std::path::PathBuf {
    let script = dir.join("grok");
    write_executable(
        &script,
        &format!(
            r#"#!/bin/sh
cat > /dev/null
fail() {{
  printf '%s\n' "{{\"schemaVersion\":1,\"status\":\"failed\",\"error\":{{\"code\":\"$1\",\"message\":\"$1\",\"details\":null}}}}"
  exit 1
}}
{checks}
printf '%s\n' '{{"schemaVersion":1,"status":"success","result":{{"policy":"ok"}},"error":null}}'
"#
        ),
    );
    script
}

pub(super) fn delegated_policy(
    audit: &V2AuditWriter,
) -> (
    Vec<String>,
    Option<ActivityToolPolicyMode>,
    Option<Vec<String>>,
) {
    audit
        .events_snapshot()
        .expect("audit snapshot")
        .into_iter()
        .find_map(|event| match event.kind {
            V2AuditEventKind::ToolAllowlistHarnessDelegated {
                effective_tools,
                tool_policy,
                tool_disallow_list,
                ..
            } => Some((effective_tools, tool_policy, tool_disallow_list)),
            _ => None,
        })
        .expect("tool allowlist audit event")
}

/// [ORB-13315] A deny-mode activity stamps its policy marker, disallow list,
/// and name, plus the concrete callable set as the legacy allowlist an older
/// MCP server would enforce, and the harness event records mode and list.
#[test]
fn run_cli_backend_stamps_deny_mode_policy_for_the_managed_child() {
    let temp = tempdir().expect("tempdir");
    let script = policy_checking_script(
        temp.path(),
        r#"[ "$ORBIT_TASK_ACTOR_KIND" = "agent" ] || fail actor_kind_missing
[ "$ORBIT_ACTIVITY_TOOL_POLICY" = "deny" ] || fail policy_marker_missing
[ "$ORBIT_ACTIVITY_TOOLS_DENY" = "orbit.workflow.ship,proc.*" ] || fail disallow_list_missing
[ "$ORBIT_ACTIVITY_NAME" = "custom_agent" ] || fail activity_name_missing
[ "$ORBIT_ACTIVITY_TOOLS" = "orbit.task.show,orbit.search,github.run.list" ] || fail concrete_allowlist_missing"#,
    );
    let audit = Arc::new(V2AuditWriter::new(
        "job-deny-mode",
        "grok:grok-build",
        Arc::new(RecordingSink::default()) as Arc<dyn AuditSink>,
    ));
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.tool_disallow_list = Some(vec![
        "orbit.workflow.ship".to_string(),
        "proc.*".to_string(),
    ]);

    let outcome = run_cli_backend(
        &policy_test_host(&script, &["github.run.list"]),
        &spec,
        "custom_agent",
        "job-deny-mode",
        audit.clone(),
        &serde_json::json!({"prompt": "hi", "task_id": "ORB-13315"}),
        None,
    )
    .expect("run succeeds");

    assert!(
        outcome.success,
        "child rejected its policy env: {:?}",
        outcome.output
    );
    let (effective_tools, tool_policy, tool_disallow_list) = delegated_policy(&audit);
    assert_eq!(
        effective_tools,
        ["orbit.task.show", "orbit.search", "github.run.list"]
    );
    assert_eq!(tool_policy, Some(ActivityToolPolicyMode::Deny));
    assert_eq!(
        tool_disallow_list.as_deref(),
        Some(["orbit.workflow.ship".to_string(), "proc.*".to_string()].as_slice())
    );
}

/// A claimed leaf's implementer writes no owner task state and re-reads
/// nothing (distributed-drain design §3): in claimed mode the owner task
/// tools are denied on top of the activity's own list, so the child can
/// neither see nor call them, and the harness event records the widened list.
#[test]
fn run_cli_backend_denies_owner_task_tools_to_a_claimed_implementer() {
    let temp = tempdir().expect("tempdir");
    let script = policy_checking_script(
        temp.path(),
        r#"[ "$ORBIT_ACTIVITY_TOOL_POLICY" = "deny" ] || fail policy_marker_missing
[ "$ORBIT_ACTIVITY_TOOLS_DENY" = "orbit.workflow.ship,proc.*,orbit.task.show,orbit.task.update" ] || fail claimed_disallow_list_missing
[ "$ORBIT_ACTIVITY_TOOLS" = "orbit.search,github.run.list" ] || fail owner_task_tools_still_callable"#,
    );
    let audit = Arc::new(V2AuditWriter::new(
        "job-claimed-deny",
        "grok:grok-build",
        Arc::new(RecordingSink::default()) as Arc<dyn AuditSink>,
    ));
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.tool_disallow_list = Some(vec![
        "orbit.workflow.ship".to_string(),
        "proc.*".to_string(),
    ]);

    let outcome = run_cli_backend(
        &policy_test_host(&script, &[]),
        &spec,
        "agent_implement",
        "job-claimed-deny",
        audit.clone(),
        &serde_json::json!({"prompt": "hi", "task_id": "ORB-13315", "claimed": true}),
        None,
    )
    .expect("run succeeds");

    assert!(
        outcome.success,
        "child still had owner task tools: {:?}",
        outcome.output
    );
    let (effective_tools, tool_policy, tool_disallow_list) = delegated_policy(&audit);
    assert_eq!(effective_tools, ["orbit.search", "github.run.list"]);
    assert_eq!(tool_policy, Some(ActivityToolPolicyMode::Deny));
    assert_eq!(
        tool_disallow_list.as_deref(),
        Some(
            [
                "orbit.workflow.ship".to_string(),
                "proc.*".to_string(),
                "orbit.task.show".to_string(),
                "orbit.task.update".to_string(),
            ]
            .as_slice()
        )
    );
}

/// The same guard holds for an allowlisted activity: claimed mode removes the
/// owner task tools from what the activity would otherwise grant.
#[test]
fn run_cli_backend_removes_owner_task_tools_from_a_claimed_allowlist() {
    let temp = tempdir().expect("tempdir");
    let script = policy_checking_script(
        temp.path(),
        r#"[ "$ORBIT_ACTIVITY_TOOLS" = "orbit.search" ] || fail owner_task_tools_still_allowed"#,
    );
    let audit = Arc::new(V2AuditWriter::new(
        "job-claimed-allow",
        "grok:grok-build",
        Arc::new(RecordingSink::default()) as Arc<dyn AuditSink>,
    ));
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.tools = vec![
        "orbit.task.show".to_string(),
        "orbit.task.update".to_string(),
        "orbit.search".to_string(),
    ];

    let outcome = run_cli_backend(
        &policy_test_host(&script, &[]),
        &spec,
        "agent_implement",
        "job-claimed-allow",
        audit.clone(),
        &serde_json::json!({"prompt": "hi", "task_id": "ORB-13315", "claimed": true}),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success, "{:?}", outcome.output);
    let (effective_tools, tool_policy, _) = delegated_policy(&audit);
    assert_eq!(effective_tools, ["orbit.search"]);
    assert_eq!(tool_policy, Some(ActivityToolPolicyMode::Allow));
}

#[test]
fn run_cli_backend_forwards_program_deny_mode_with_legacy_mcp_fallback() {
    let temp = tempdir().expect("tempdir");
    let script = policy_checking_script(
        temp.path(),
        r#"[ "$ORBIT_PROC_PROGRAM_POLICY" = "deny" ] || fail program_policy_marker
[ "$ORBIT_PROC_DISALLOWED_PROGRAMS" = "sudo,su,doas,pkexec,ssh,scp,sftp,rsync,nc,ncat,netcat,socat,systemctl,loginctl,shutdown,reboot,mount,umount,chroot,nsenter,unshare,docker,podman" ] || fail program_disallow_list
[ "$ORBIT_PROC_ALLOWED_PROGRAMS" = "awk,bash,cargo,cat,find,git,grep,jq,ls,make,ps,python3,rg,sed,sh" ] || fail legacy_mcp_fallback"#,
    );
    let audit = Arc::new(V2AuditWriter::new(
        "job-program-deny",
        "grok:grok-build",
        Arc::new(RecordingSink::default()) as Arc<dyn AuditSink>,
    ));
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.proc_disallowed_programs = Some("sudo,su,doas,pkexec,ssh,scp,sftp,rsync,nc,ncat,netcat,socat,systemctl,loginctl,shutdown,reboot,mount,umount,chroot,nsenter,unshare,docker,podman".split(',').map(str::to_string).collect());
    let outcome = run_cli_backend(
        &policy_test_host(&script, &[]),
        &spec,
        "agent_invoke",
        "job-program-deny",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");
    assert!(outcome.success, "{:?}", outcome.output);
}

/// [ORB-13315] A task requirement never overrides a disallow entry: the run
/// is refused before any provider launch.
#[test]
fn run_cli_backend_refuses_a_disallowed_task_requirement_before_launch() {
    let temp = tempdir().expect("tempdir");
    let script = policy_checking_script(temp.path(), "fail must_not_launch");
    let audit = Arc::new(V2AuditWriter::new(
        "job-deny-required",
        "grok:grok-build",
        Arc::new(RecordingSink::default()) as Arc<dyn AuditSink>,
    ));
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.tool_disallow_list = Some(vec!["github.run.list".to_string()]);

    let error = run_cli_backend(
        &policy_test_host(&script, &["github.run.list"]),
        &spec,
        "custom_agent",
        "job-deny-required",
        audit,
        &serde_json::json!({"prompt": "hi", "task_id": "ORB-13315"}),
        None,
    )
    .expect_err("a disallowed requirement must refuse dispatch");

    let message = error.to_string();
    assert!(message.contains("`github.run.list`"), "{message}");
    assert!(message.contains("(custom_agent)"), "{message}");
}

/// [ORB-13315] An allowlist-mode run keeps today's envelope exactly, even
/// when its dispatching process itself runs under a deny-mode envelope: the
/// inherited deny names must not replace this run's allowlist.
#[test]
fn run_cli_backend_allowlist_mode_drops_an_inherited_deny_envelope() {
    let temp = tempdir().expect("tempdir");
    let script = policy_checking_script(
        temp.path(),
        r#"[ -z "${ORBIT_ACTIVITY_TOOL_POLICY+x}" ] || fail inherited_policy_marker
[ -z "${ORBIT_ACTIVITY_TOOLS_DENY+x}" ] || fail inherited_disallow_list
[ -z "${ORBIT_ACTIVITY_NAME+x}" ] || fail inherited_activity_name
[ "$ORBIT_ACTIVITY_TOOLS" = "orbit.task.show,github.run.list" ] || fail allowlist_changed"#,
    );
    let audit = Arc::new(V2AuditWriter::new(
        "job-allow-nested",
        "grok:grok-build",
        Arc::new(RecordingSink::default()) as Arc<dyn AuditSink>,
    ));
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.tools = vec!["orbit.task.show".to_string()];
    let _outer = orbit_common::test_env::scoped([
        ("ORBIT_ACTIVITY_TOOL_POLICY", Some("deny")),
        ("ORBIT_ACTIVITY_TOOLS_DENY", Some("")),
        ("ORBIT_ACTIVITY_NAME", Some("outer_activity")),
    ]);

    let outcome = run_cli_backend(
        &policy_test_host(&script, &["github.run.list"]),
        &spec,
        "custom_agent",
        "job-allow-nested",
        audit.clone(),
        &serde_json::json!({"prompt": "hi", "task_id": "ORB-13315"}),
        None,
    )
    .expect("run succeeds");

    assert!(
        outcome.success,
        "child saw a deny envelope: {:?}",
        outcome.output
    );
    let (effective_tools, tool_policy, tool_disallow_list) = delegated_policy(&audit);
    assert_eq!(effective_tools, ["orbit.task.show", "github.run.list"]);
    assert_eq!(tool_policy, Some(ActivityToolPolicyMode::Allow));
    assert_eq!(tool_disallow_list, None);
}

/// A deny list covering every registered tool must not stamp an empty
/// legacy allowlist, which an older MCP server reads as unrestricted.
#[test]
fn deny_mode_with_no_callable_tool_stamps_a_non_empty_legacy_allowlist() {
    let disallow = vec!["orbit.task.*".to_string()];
    let env = activity_tool_policy_env("custom_agent", Some(&disallow), &[]);
    let allowlist = env
        .iter()
        .find(|(name, _)| name == "ORBIT_ACTIVITY_TOOLS")
        .map(|(_, value)| value.as_str())
        .expect("legacy allowlist stamped");
    assert!(!allowlist.is_empty());
    assert!(!orbit_types::workflow::tool_allowed(
        "orbit.task.show",
        &[allowlist.to_string()]
    ));

    let tools = vec!["orbit.task.show".to_string()];
    assert_eq!(
        activity_tool_policy_env("custom_agent", None, &tools),
        [(
            "ORBIT_ACTIVITY_TOOLS".to_string(),
            "orbit.task.show".to_string()
        )],
        "allowlist mode stamps exactly the legacy allowlist"
    );
}

/// Shipped `agent_invoke` program deny list and the pre-migration allowlist an
/// older MCP server still enforces. Kept beside the child assertion so the
/// stamped envelope and the spec cannot drift apart inside this test.
const AGENT_INVOKE_DISALLOW: &str = "sudo,su,doas,pkexec,ssh,scp,sftp,rsync,nc,ncat,netcat,socat,systemctl,loginctl,shutdown,reboot,mount,umount,chroot,nsenter,unshare,docker,podman";
const AGENT_INVOKE_LEGACY_ALLOWLIST: &str =
    "awk,bash,cargo,cat,find,git,grep,jq,ls,make,ps,python3,rg,sed,sh";

/// Outer deny-mode process policy the current activity must replace.
fn outer_program_deny_env() -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped([
        ("ORBIT_PROC_PROGRAM_POLICY", Some("deny")),
        ("ORBIT_PROC_DISALLOWED_PROGRAMS", Some("sudo")),
        ("ORBIT_PROC_ALLOWED_PROGRAMS", Some("python3")),
    ])
}

struct StampedProgramPolicy {
    policy: Option<String>,
    disallowed: Option<String>,
    allowed: Option<String>,
}

/// A grok stand-in that records the process-policy envelope, then fails with
/// `$code` unless every shell `checks` holds.
fn program_policy_script(dir: &Path, checks: &str) -> (PathBuf, PathBuf) {
    let report = dir.join("report");
    std::fs::create_dir_all(&report).expect("report dir");
    let script = dir.join("grok");
    write_executable(
        &script,
        &format!(
            r#"#!/bin/sh
cat > /dev/null
fail() {{
  printf '%s\n' "{{\"schemaVersion\":1,\"status\":\"failed\",\"error\":{{\"code\":\"$1\",\"message\":\"$1\",\"details\":null}}}}"
  exit 1
}}
stamp() {{
  name="$1"
  value="$2"
  present="$3"
  if [ "$present" = present ]; then
    printf 'set:%s\n' "$value" > "{report}/$name" || fail stamp_failed
  else
    printf 'unset\n' > "{report}/$name" || fail stamp_failed
  fi
}}
if [ -z "${{ORBIT_PROC_PROGRAM_POLICY+x}}" ]; then
  stamp policy "" absent
else
  stamp policy "$ORBIT_PROC_PROGRAM_POLICY" present
fi
if [ -z "${{ORBIT_PROC_DISALLOWED_PROGRAMS+x}}" ]; then
  stamp disallowed "" absent
else
  stamp disallowed "$ORBIT_PROC_DISALLOWED_PROGRAMS" present
fi
if [ -z "${{ORBIT_PROC_ALLOWED_PROGRAMS+x}}" ]; then
  stamp allowed "" absent
else
  stamp allowed "$ORBIT_PROC_ALLOWED_PROGRAMS" present
fi
{checks}
printf '%s\n' '{{"schemaVersion":1,"status":"success","result":{{"policy":"ok"}},"error":null}}'
"#,
            report = report.display(),
        ),
    );
    (script, report)
}

fn read_stamp(report: &Path, name: &str) -> Option<String> {
    let raw = std::fs::read_to_string(report.join(name)).expect("program-policy stamp");
    let raw = raw.trim_end_matches('\n');
    match raw.strip_prefix("set:") {
        Some(value) => Some(value.to_string()),
        None => {
            assert_eq!(raw, "unset", "program-policy stamp {name} was {raw}");
            None
        }
    }
}

fn read_stamped_program_policy(report: &Path) -> StampedProgramPolicy {
    StampedProgramPolicy {
        policy: read_stamp(report, "policy"),
        disallowed: read_stamp(report, "disallowed"),
        allowed: read_stamp(report, "allowed"),
    }
}

/// Comma split used by the managed CLI/MCP callback for program lists.
fn split_env_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

/// Tool context a nested managed CLI/MCP `proc.spawn` builds from the child
/// envelope. A deny marker counts only when its list is present, including
/// when that list is empty; otherwise the legacy allowlist applies.
fn nested_cli_context(stamped: &StampedProgramPolicy) -> ToolContext {
    let proc_disallowed_programs = match (&stamped.policy, &stamped.disallowed) {
        (Some(policy), Some(list)) if policy == "deny" => Some(split_env_list(list)),
        _ => None,
    };
    ToolContext {
        proc_allowed_programs: stamped
            .allowed
            .as_deref()
            .map(split_env_list)
            .unwrap_or_default(),
        proc_disallowed_programs,
        proc_spawn_activity_scoped: true,
        ..Default::default()
    }
}

/// Plugin-broker caller the dispatcher builds from the current activity spec.
fn broker_context(spec: &AgentLoopSpec, worktree: &Path) -> ToolContext {
    let caller = BrokeredCaller {
        worktree: worktree.to_path_buf(),
        fs_profile: ResolvedFsProfile {
            name: "unrestricted".to_string(),
            read: vec!["./**".to_string()],
            modify: Vec::new(),
        },
        proc_allowed_programs: spec.proc_allowed_programs.clone().unwrap_or_default(),
        proc_disallowed_programs: spec.proc_disallowed_programs.clone(),
    };
    let mut ctx = ToolContext::default();
    caller.restrict(&mut ctx);
    ctx
}

fn spawn_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    registry
}

fn program_policy_denied(registry: &ToolRegistry, ctx: &ToolContext, program: &str) -> bool {
    match registry.execute(
        "proc.spawn",
        ctx,
        json!({ "program": program, "timeout_ms": 1000 }),
    ) {
        Ok(_) => false,
        Err(OrbitError::PolicyDenied(message)) => {
            message.contains("not in the allowed list")
                || message.contains("activity disallow list")
        }
        Err(_) => false,
    }
}

fn assert_paths_agree(
    registry: &ToolRegistry,
    nested: &ToolContext,
    broker: &ToolContext,
    program: &str,
    denied: bool,
) {
    assert_eq!(
        nested.proc_disallowed_programs, broker.proc_disallowed_programs,
        "nested CLI/MCP and the plugin broker disagree on the disallow list"
    );
    if broker.proc_disallowed_programs.is_none() {
        assert_eq!(
            nested.proc_allowed_programs, broker.proc_allowed_programs,
            "nested CLI/MCP and the plugin broker disagree on the allowlist"
        );
    }
    let nested_denied = program_policy_denied(registry, nested, program);
    let broker_denied = program_policy_denied(registry, broker, program);
    assert_eq!(
        nested_denied, denied,
        "nested proc.spawn decision for {program}"
    );
    assert_eq!(
        broker_denied, nested_denied,
        "plugin broker and nested proc.spawn disagree on {program}"
    );
}

fn require_available(program: &str) {
    let status = std::process::Command::new(program)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap_or_else(|error| panic!("{program} is not an available executable: {error}"));
    assert!(status.success(), "{program} is not an available executable");
}

/// [ORB-13427] An allowlist activity launched under an outer deny-mode
/// process policy exports only its own allowlist. Nested `proc.spawn` then
/// refuses an available program outside that list, including when the list
/// is empty.
#[test]
fn run_cli_backend_allowlist_replaces_an_inherited_program_deny_policy() {
    require_available("true");
    let registry = spawn_registry();
    for (label, programs, checks) in [
        (
            "git-only",
            vec!["git".to_string()],
            r#"[ -z "${ORBIT_PROC_PROGRAM_POLICY+x}" ] || fail inherited_program_policy
[ -z "${ORBIT_PROC_DISALLOWED_PROGRAMS+x}" ] || fail inherited_program_disallow
[ "$ORBIT_PROC_ALLOWED_PROGRAMS" = "git" ] || fail current_allowlist"#,
        ),
        (
            "empty",
            Vec::new(),
            r#"[ -z "${ORBIT_PROC_PROGRAM_POLICY+x}" ] || fail inherited_program_policy
[ -z "${ORBIT_PROC_DISALLOWED_PROGRAMS+x}" ] || fail inherited_program_disallow
[ -n "${ORBIT_PROC_ALLOWED_PROGRAMS+x}" ] || fail empty_allowlist_missing
[ -z "$ORBIT_PROC_ALLOWED_PROGRAMS" ] || fail empty_allowlist_not_empty"#,
        ),
    ] {
        let temp = tempdir().expect("tempdir");
        let (script, report) = program_policy_script(temp.path(), checks);
        let audit = Arc::new(V2AuditWriter::new(
            "job-program-allow-nested",
            "grok:grok-build",
            Arc::new(RecordingSink::default()) as Arc<dyn AuditSink>,
        ));
        let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
        spec.tools = vec!["proc.spawn".to_string()];
        spec.proc_allowed_programs = Some(programs);
        let outcome = {
            let _outer = outer_program_deny_env();
            run_cli_backend(
                &policy_test_host(&script, &[]),
                &spec,
                "custom_agent",
                "job-program-allow-nested",
                audit,
                &serde_json::json!({"prompt": "hi", "task_id": "ORB-13427"}),
                None,
            )
            .expect("run succeeds")
        };
        assert!(
            outcome.success,
            "{label} child kept an inherited program policy: {:?}",
            outcome.output
        );

        let stamped = read_stamped_program_policy(&report);
        let nested = nested_cli_context(&stamped);
        let broker = broker_context(&spec, temp.path());
        assert_paths_agree(&registry, &nested, &broker, "true", true);
        if label == "git-only" {
            assert_paths_agree(&registry, &nested, &broker, "git", false);
        }
    }
}

/// [ORB-13427] A deny-mode activity replaces an outer program policy. An
/// explicit empty disallow list stays empty, and a shipped activity still
/// stamps its legacy MCP allowlist.
#[test]
fn run_cli_backend_deny_mode_replaces_an_inherited_program_policy() {
    require_available("true");
    let registry = spawn_registry();

    let temp = tempdir().expect("tempdir");
    let (script, report) = program_policy_script(
        temp.path(),
        &format!(
            r#"[ "$ORBIT_PROC_PROGRAM_POLICY" = "deny" ] || fail program_policy_marker
[ "$ORBIT_PROC_DISALLOWED_PROGRAMS" = "{AGENT_INVOKE_DISALLOW}" ] || fail program_disallow_list
[ "$ORBIT_PROC_ALLOWED_PROGRAMS" = "{AGENT_INVOKE_LEGACY_ALLOWLIST}" ] || fail legacy_mcp_fallback"#
        ),
    );
    let audit = Arc::new(V2AuditWriter::new(
        "job-program-deny-nested",
        "grok:grok-build",
        Arc::new(RecordingSink::default()) as Arc<dyn AuditSink>,
    ));
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.tools = vec!["proc.spawn".to_string()];
    spec.proc_disallowed_programs = Some(
        AGENT_INVOKE_DISALLOW
            .split(',')
            .map(str::to_string)
            .collect(),
    );
    let outcome = {
        let _outer = outer_program_deny_env();
        run_cli_backend(
            &policy_test_host(&script, &[]),
            &spec,
            "agent_invoke",
            "job-program-deny-nested",
            audit,
            &serde_json::json!({"prompt": "hi"}),
            None,
        )
        .expect("run succeeds")
    };
    assert!(
        outcome.success,
        "shipped deny-mode child kept the outer program policy: {:?}",
        outcome.output
    );
    let nested = nested_cli_context(&read_stamped_program_policy(&report));
    let broker = broker_context(&spec, temp.path());
    assert_paths_agree(&registry, &nested, &broker, "sudo", true);
    assert_paths_agree(&registry, &nested, &broker, "true", false);

    let temp = tempdir().expect("tempdir");
    let (script, report) = program_policy_script(
        temp.path(),
        r#"[ "$ORBIT_PROC_PROGRAM_POLICY" = "deny" ] || fail program_policy_marker
[ -n "${ORBIT_PROC_DISALLOWED_PROGRAMS+x}" ] || fail empty_disallow_unset
[ -z "$ORBIT_PROC_DISALLOWED_PROGRAMS" ] || fail empty_disallow_replaced
[ -n "${ORBIT_PROC_ALLOWED_PROGRAMS+x}" ] || fail legacy_allowlist_unset
[ -z "$ORBIT_PROC_ALLOWED_PROGRAMS" ] || fail legacy_allowlist_not_empty"#,
    );
    let audit = Arc::new(V2AuditWriter::new(
        "job-program-deny-empty",
        "grok:grok-build",
        Arc::new(RecordingSink::default()) as Arc<dyn AuditSink>,
    ));
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.tools = vec!["proc.spawn".to_string()];
    spec.proc_disallowed_programs = Some(Vec::new());
    let outcome = {
        let _outer = outer_program_deny_env();
        run_cli_backend(
            &policy_test_host(&script, &[]),
            &spec,
            "custom_agent",
            "job-program-deny-empty",
            audit,
            &serde_json::json!({"prompt": "hi"}),
            None,
        )
        .expect("run succeeds")
    };
    assert!(
        outcome.success,
        "empty deny-mode child kept the outer program policy: {:?}",
        outcome.output
    );
    let nested = nested_cli_context(&read_stamped_program_policy(&report));
    let broker = broker_context(&spec, temp.path());
    assert_eq!(nested.proc_disallowed_programs, Some(Vec::new()));
    assert_paths_agree(&registry, &nested, &broker, "sudo", false);
    assert_paths_agree(&registry, &nested, &broker, "true", false);
}
