#![allow(missing_docs)]

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use orbit_agent::loop_engine::audit::NullSink;
use orbit_types::workflow::activity_job::{ActivityToolPolicyMode, V2AuditEventKind};
use tempfile::tempdir;

use super::super::super::audit_writer::V2AuditWriter;
use super::super::run_cli_backend;
use super::test_support::{
    TestHost, persisted_blobs, persisted_writer, test_agent_loop_spec_for, write_executable,
};

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

    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-env-allowlist",
        "grok:grok-build",
        Arc::new(NullSink),
    ));
    let host = TestHost::with_command(script.display().to_string());
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

/// A claimed leaf's implementer writes no owner task state and re-reads
/// nothing (distributed-drain design §3): in claimed mode the owner task
/// tools are denied on top of the activity's own list, so the child can
/// neither see nor call them, and the harness event records the widened list.
#[test]
fn run_cli_backend_denies_owner_task_tools_to_a_claimed_implementer() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
fail() {
  printf '%s\n' "{\"schemaVersion\":1,\"status\":\"failed\",\"error\":{\"code\":\"$1\",\"message\":\"$1\",\"details\":null}}"
  exit 1
}
[ "$ORBIT_ACTIVITY_TOOL_POLICY" = "deny" ] || fail policy_marker_missing
[ "$ORBIT_ACTIVITY_TOOLS_DENY" = "orbit.workflow.ship,proc.*,orbit.task.show,orbit.task.update" ] || fail claimed_disallow_list_missing
[ "$ORBIT_ACTIVITY_TOOLS" = "orbit.search,github.run.list" ] || fail owner_task_tools_still_callable
printf '%s\n' '{"schemaVersion":1,"status":"success","result":{"policy":"ok"},"error":null}'
"#,
    );
    let audit = persisted_writer(
        &temp.path().join("audit"),
        "job-claimed-deny",
        "grok:grok-build",
    );
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.tool_disallow_list = Some(vec![
        "orbit.workflow.ship".to_string(),
        "proc.*".to_string(),
    ]);

    let outcome = run_cli_backend(
        &TestHost::with_command(script.display().to_string()),
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
    let (effective_tools, tool_policy, tool_disallow_list) = audit
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
        .expect("tool allowlist audit event");
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

/// A live env value the provider echoes back on any stream is redacted
/// before the stdin, stdout and stderr captures are persisted as blobs.
#[test]
fn run_cli_backend_redacts_live_env_values_in_stored_blobs() {
    let secret = "live-cli-blob-secret-value";
    let _guard = orbit_common::test_env::scoped([("ORBIT_CLI_BLOB_TEST_TOKEN", Some(secret))]);
    let temp = tempdir().expect("tempdir");
    let stdout = temp.path().join("stdout.jsonl");
    let stderr = temp.path().join("stderr.txt");
    fs::write(
        &stdout,
        format!(
            "{{\"log\":\"stdout leak {secret}\"}}\n\
             {{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}\n"
        ),
    )
    .expect("write stdout");
    fs::write(&stderr, format!("stderr leak {secret}\n")).expect("write stderr");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\ncat '{}'\ncat '{}' >&2\n",
            stdout.display(),
            stderr.display()
        ),
    );
    let audit_root = temp.path().join("audit");
    let audit = persisted_writer(&audit_root, "job-cli-blob-redaction", "codex:gpt-5.5");

    let outcome = run_cli_backend(
        &TestHost::with_command(script.display().to_string()),
        &test_agent_loop_spec_for("codex", Duration::from_secs(10)),
        "test_activity",
        "job-cli-blob-redaction",
        audit,
        &serde_json::json!({"prompt": format!("provider stdin contains {secret}")}),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success);
    let blobs = persisted_blobs(&audit_root);
    for key in ["stdin_blob_ref", "stdout_blob_ref", "stderr_blob_ref"] {
        let blob_ref = outcome.output[key].as_str().expect("blob ref");
        let text = String::from_utf8(blobs.read(blob_ref).expect("read stored blob"))
            .expect("stored blob utf8");
        assert!(
            !text.contains(secret),
            "{key} should not contain raw live env value: {text}"
        );
        assert!(
            text.contains("[REDACTED_ENV]"),
            "{key} should include env redaction marker: {text}"
        );
    }
}
