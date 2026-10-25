use super::cli_support::{
    TestHost, persisted_blobs, persisted_writer, test_agent_loop_spec_for, write_executable,
};
use orbit_agent::loop_engine::audit::NullSink;
use orbit_engine::activity_job::cli_runner::run_cli_backend;
use orbit_engine::{DispatchError, V2AuditWriter};
use orbit_types::workflow::activity_job::{ActivityToolPolicyMode, V2AuditEventKind};
use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tempfile::tempdir;
const STDOUT_MARKER: &str = "provider-stdout-marker";
const STDERR_MARKER: &str = "provider-stderr-marker";

/// The public CLI dispatch path advertises only tools the managed agent can
/// call, while retaining the activity's full effective-tool policy.
#[test]
fn agent_implement_envelope_omits_tools_without_agent_capability() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::agent_implement_envelope_omits_tools_without_agent_capability"
    )) {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        r##"#!/bin/sh
cat >/dev/null
printf '%s\n' '{"schemaVersion":1,"status":"success","result":{},"error":null}'
"##,
    );
    let audit_root = temp.path().join("audit");
    let audit = persisted_writer(&audit_root, "job-agent-tools", "grok:grok-build");
    let host = TestHost::with_command(script.display().to_string())
        .with_additional_tools(&["orbit.workflow.run.show", "orbit.workflow.run.list"]);
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(10));
    spec.tool_disallow_list = Some(Vec::new());

    run_cli_backend(
        &host,
        &spec,
        "agent_implement",
        "job-agent-tools",
        audit.clone(),
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("agent implementation dispatch");

    let events = audit.events_snapshot().expect("audit snapshot");
    let stdin_blob_ref = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted {
                stdin_blob_ref: Some(blob_ref),
                ..
            } => Some(blob_ref.clone()),
            _ => None,
        })
        .expect("provider stdin blob");
    let effective_tools = events
        .into_iter()
        .find_map(|event| match event.kind {
            V2AuditEventKind::ToolAllowlistHarnessDelegated {
                effective_tools, ..
            } => Some(effective_tools),
            _ => None,
        })
        .expect("effective activity tools");
    assert!(effective_tools.contains(&"orbit.workflow.run.show".to_string()));
    assert!(effective_tools.contains(&"orbit.workflow.run.list".to_string()));

    let provider_input = persisted_blobs(&audit_root)
        .read(&stdin_blob_ref)
        .expect("read provider stdin");
    let envelope_json = provider_input
        .split(|byte| *byte == b'\n')
        .rev()
        .find(|line| !line.is_empty())
        .expect("provider input ends with activity envelope JSON");
    let envelope: serde_json::Value =
        serde_json::from_slice(envelope_json).expect("activity envelope JSON");
    let tools = envelope["tools"].as_array().expect("advertised tools");

    assert!(
        !tools.contains(&serde_json::json!("orbit.workflow.run.show")),
        "{tools:?}"
    );
    assert!(
        !tools.contains(&serde_json::json!("orbit.workflow.run.list")),
        "{tools:?}"
    );
    assert!(
        tools.contains(&serde_json::json!("orbit.task.show")),
        "{tools:?}"
    );
    assert!(
        tools.contains(&serde_json::json!("orbit.search")),
        "{tools:?}"
    );
}

/// [ORB-10917] End-to-end guard for the composed dispatch environment: a
/// benignly named ambient credential must not survive into the provider child.
/// The ambient value is set by this test rather than inherited from the
/// developer's shell, and the child reports what it actually saw so a
/// regression fails loudly instead of silently forwarding.
#[test]
fn run_cli_backend_does_not_forward_benignly_named_ambient_credentials() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::run_cli_backend_does_not_forward_benignly_named_ambient_credentials"
    )) {
        return;
    }

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

/// [ORB-14771] A representable timeout still stamps the provider's deadline as
/// a future epoch-millisecond instant, so a nested `proc.spawn` keeps its scope.
#[test]
fn representable_timeout_stamps_a_future_provider_deadline() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::representable_timeout_stamps_a_future_provider_deadline"
    )) {
        return;
    }

    let before_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis();

    let stamp = observed_deadline_stamp(Duration::from_secs(600));

    let deadline_ms: u128 = stamp.parse().expect("deadline stamp is epoch milliseconds");
    assert!(deadline_ms >= before_ms + 600_000, "{stamp}");

    // ORB-14771: an unrepresentable SystemTime deadline must stay unset,
    // rather than stamping zero and exhausting every nested proc.spawn.
    let timeout = Duration::from_secs(i64::MAX as u64 - 1_000_000_000);
    assert_eq!(observed_deadline_stamp(timeout), "unset");
}

/// A claimed leaf's implementer may read its claimed task through the scoped
/// owner route, but cannot write owner task state (distributed-drain design
/// §3). The task update denial is added on top of the activity's own list.
#[test]
fn run_cli_backend_keeps_claimed_task_read_but_denies_owner_task_updates() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::run_cli_backend_keeps_claimed_task_read_but_denies_owner_task_updates"
    )) {
        return;
    }

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
[ "$ORBIT_ACTIVITY_TOOLS_DENY" = "orbit.workflow.ship,proc.*,orbit.task.update" ] || fail claimed_disallow_list_missing
[ "$ORBIT_ACTIVITY_TOOLS" = "orbit.task.show,orbit.search,github.run.list" ] || fail claimed_task_read_unavailable
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
        "child had an unexpected owner task policy: {:?}",
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
    assert_eq!(
        effective_tools,
        ["orbit.task.show", "orbit.search", "github.run.list"]
    );
    assert_eq!(tool_policy, Some(ActivityToolPolicyMode::Deny));
    assert_eq!(
        tool_disallow_list.as_deref(),
        Some(
            [
                "orbit.workflow.ship".to_string(),
                "proc.*".to_string(),
                "orbit.task.update".to_string(),
            ]
            .as_slice()
        )
    );
}

/// An allowlisted activity gets the same claimed-mode guard as a deny-listed
/// one: an allowlist the removal empties refuses every tool (an empty
/// `ORBIT_ACTIVITY_TOOLS` reads as unrestricted), and a wildcard covering
/// `orbit.task.update` keeps its other tools but not that one.
#[test]
fn run_cli_backend_claimed_allowlist_never_widens_or_keeps_owner_task_updates() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::run_cli_backend_claimed_allowlist_never_widens_or_keeps_owner_task_updates"
    )) {
        return;
    }

    for (tools, expected) in [
        (vec!["orbit.task.update"], "orbit.activity-policy.none"),
        (
            vec!["orbit.task.*", "orbit.search"],
            "orbit.task.show,orbit.search",
        ),
    ] {
        let temp = tempdir().expect("tempdir");
        let script = temp.path().join("grok");
        write_executable(
            &script,
            &format!(
                r#"#!/bin/sh
cat > /dev/null
if [ "$ORBIT_ACTIVITY_TOOLS" = "{expected}" ]; then
  printf '%s\n' '{{"schemaVersion":1,"status":"success","result":{{"policy":"ok"}},"error":null}}'
else
  printf '{{"schemaVersion":1,"status":"failed","error":{{"code":"tools_widened","message":"%s","details":null}}}}\n' "$ORBIT_ACTIVITY_TOOLS"
  exit 1
fi
"#
            ),
        );
        let audit = persisted_writer(
            &temp.path().join("audit"),
            "job-claimed-allowlist",
            "grok:grok-build",
        );
        let host = TestHost::with_command(script.display().to_string())
            .with_additional_tools(&["orbit.task.update"]);
        let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
        spec.tools = tools.iter().map(|tool| (*tool).to_string()).collect();

        let outcome = run_cli_backend(
            &host,
            &spec,
            "custom_allowlist",
            "job-claimed-allowlist",
            audit,
            &serde_json::json!({"prompt": "hi", "claimed": true}),
            None,
        )
        .expect("run succeeds");

        assert!(
            outcome.success,
            "{tools:?}: child saw unexpected tools: {:?} {:?}",
            outcome.output, outcome.message
        );
    }
}

/// A blob the audit store cannot write is counted as an audit failure and
/// recorded as no ref, never as an `error:` string standing in for one.
#[test]
fn run_cli_backend_counts_a_failed_blob_write_and_records_no_ref() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::run_cli_backend_counts_a_failed_blob_write_and_records_no_ref"
    )) {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        r##"#!/bin/sh
cat >/dev/null
printf '%s\n' '{"schemaVersion":1,"status":"success","result":{},"error":null}'
"##,
    );
    let audit_root = temp.path().join("audit");
    let audit = persisted_writer(&audit_root, "job-blob-failure", "grok:grok-build");
    // A file where the blob directory belongs makes every blob write fail.
    fs::create_dir_all(&audit_root).expect("audit root");
    fs::write(audit_root.join("blobs"), b"not a directory").expect("block blob directory");
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec_for("grok", Duration::from_secs(10));

    run_cli_backend(
        &host,
        &spec,
        "agent_implement",
        "job-blob-failure",
        audit.clone(),
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("provider output is still projected");

    assert!(audit.degraded_audit(), "lost blobs degrade the audit trail");
    assert!(audit.audit_failure_count() >= 3, "stdin, stdout and stderr");
    for event in audit.events_snapshot().expect("audit snapshot") {
        match event.kind {
            V2AuditEventKind::CliInvocationStarted { stdin_blob_ref, .. } => {
                assert_eq!(stdin_blob_ref, None);
            }
            V2AuditEventKind::CliInvocationFinished {
                stdout_blob_ref,
                stderr_blob_ref,
                ..
            } => {
                assert_eq!((stdout_blob_ref, stderr_blob_ref), (None, None));
            }
            _ => {}
        }
    }
}

/// A live env value the provider echoes back on any stream is redacted
/// before the stdin, stdout and stderr captures are persisted as blobs.
#[test]
fn run_cli_backend_redacts_live_env_values_in_stored_blobs() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::run_cli_backend_redacts_live_env_values_in_stored_blobs"
    )) {
        return;
    }

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

/// [ORB-14090] A provider that dirties its read-only inspection checkout still
/// leaves stdout/stderr blobs and `CliInvocationFinished` on the run, and the
/// step error names those blobs.
#[test]
fn inspection_mutation_keeps_provider_output_and_finish_event() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::inspection_mutation_keeps_provider_output_and_finish_event"
    )) {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let revision = init_repo(&source);
    let script = temp.path().join("grok");
    write_provider_script(&script, true, None);
    let audit_root = temp.path().join("audit");
    let audit = persisted_writer(&audit_root, "job-inspection-mutation", "grok:grok-build");

    let error = run_cli_backend(
        &TestHost::with_command(script.display().to_string()),
        &test_agent_loop_spec_for("grok", Duration::from_secs(10)),
        "test_activity",
        "job-inspection-mutation",
        audit.clone(),
        &serde_json::json!({
            "prompt": "hi",
            "workspace_path": source,
            "inspection_revision": revision,
            "source_revision": revision,
        }),
        Some("reviewer"),
    )
    .expect_err("mutated inspection checkout fails the step");

    let DispatchError::CliInvocationPermanent(message) = &error else {
        panic!("a mutated read-only inspection checkout is permanent: {error}");
    };
    assert!(
        message.contains("source inspection checkout changed during read-only execution"),
        "{message}"
    );
    assert_output_survives(&error, &audit, &audit_root);
}

/// [ORB-14090] Inspection failure must not skip the worktree boundary. A
/// concurrent primary source-path mutation is still `WorktreeIntegrity`, and
/// that step error cites the provider blobs.
#[test]
fn inspection_mutation_still_classifies_primary_checkout_drift() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::inspection_mutation_still_classifies_primary_checkout_drift"
    )) {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let primary = temp.path().join("primary");
    let assigned = temp.path().join("assigned");
    init_repo(&primary);
    // The inspection slot is a nested repository under the primary's
    // `.orbit/` tree. A real checkout ignores that tree. Without the ignore,
    // fingerprinting hashes the slot directory and `git hash-object
    // --no-filters` fails with `(null)` before the boundary can classify a
    // source-path edit.
    fs::write(primary.join(".gitignore"), ".orbit/\n").expect("gitignore");
    git(&primary, &["add", ".gitignore"]);
    git(&primary, &["commit", "-m", "ignore orbit state"]);
    let revision = git(&primary, &["rev-parse", "HEAD"]);
    git(
        &primary,
        &[
            "worktree",
            "add",
            "--detach",
            &assigned.display().to_string(),
            "HEAD",
        ],
    );
    let script = temp.path().join("grok");
    write_provider_script(&script, true, Some(&primary.join("drift.txt")));
    let audit_root = temp.path().join("audit");
    let audit = persisted_writer(&audit_root, "job-inspection-drift", "grok:grok-build");
    let host = TestHost::with_command(script.display().to_string()).with_workspace_root(primary);

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec_for("grok", Duration::from_secs(10)),
        "test_activity",
        "job-inspection-drift",
        audit.clone(),
        &serde_json::json!({
            "prompt": "hi",
            "workspace_path": assigned,
            "repo_root": assigned,
            "inspection_revision": revision,
            "source_revision": revision,
        }),
        Some("reviewer"),
    )
    .expect_err("primary drift fails the step");

    let DispatchError::WorktreeIntegrity { code, diagnostic } = &error else {
        panic!("primary mutation must stay WorktreeIntegrity, got {error}");
    };
    assert_eq!(*code, "primary_checkout_drift");
    let parsed: serde_json::Value = serde_json::from_str(diagnostic).expect("diagnostic json");
    assert_eq!(parsed["code"], "primary_checkout_drift");
    let (stdout_blob_ref, stderr_blob_ref) = assert_output_survives(&error, &audit, &audit_root);
    assert_eq!(
        parsed["stdout_blob_ref"].as_str(),
        Some(stdout_blob_ref.as_str())
    );
    assert_eq!(
        parsed["stderr_blob_ref"].as_str(),
        Some(stderr_blob_ref.as_str())
    );
}
/// Runs a grok provider that records its `ORBIT_ACTIVITY_DEADLINE_UNIX_MS`
/// value, or `unset` when the variable is absent, and returns that record.
fn observed_deadline_stamp(timeout: Duration) -> String {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    let observed = temp.path().join("deadline");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf '%s' \"${{ORBIT_ACTIVITY_DEADLINE_UNIX_MS-unset}}\" > '{}'\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            observed.display()
        ),
    );
    let audit = Arc::new(V2AuditWriter::new(
        "job-deadline-stamp",
        "grok:grok-build",
        Arc::new(NullSink),
    ));

    let outcome = run_cli_backend(
        &TestHost::with_command(script.display().to_string()),
        &test_agent_loop_spec_for("grok", timeout),
        "test_activity",
        "job-deadline-stamp",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success, "{:?}", outcome.output);
    fs::read_to_string(observed).expect("provider recorded its deadline env")
}
fn assert_output_survives(
    error: &DispatchError,
    audit: &V2AuditWriter,
    audit_root: &Path,
) -> (String, String) {
    let (stdout_blob_ref, stderr_blob_ref) = finished_output_refs(audit);
    let rendered = error.to_string();
    assert!(
        rendered.contains(&stdout_blob_ref) && rendered.contains(&stderr_blob_ref),
        "step error must cite stored blobs: {rendered}"
    );
    let blobs = persisted_blobs(audit_root);
    let stdout = String::from_utf8(blobs.read(&stdout_blob_ref).expect("read stdout blob"))
        .expect("stdout blob utf8");
    let stderr = String::from_utf8(blobs.read(&stderr_blob_ref).expect("read stderr blob"))
        .expect("stderr blob utf8");
    assert!(stdout.contains(STDOUT_MARKER), "{stdout}");
    assert!(stderr.contains(STDERR_MARKER), "{stderr}");
    (stdout_blob_ref, stderr_blob_ref)
}
fn finished_output_refs(audit: &V2AuditWriter) -> (String, String) {
    audit
        .events_snapshot()
        .expect("audit snapshot")
        .into_iter()
        .find_map(|event| match event.kind {
            V2AuditEventKind::CliInvocationFinished {
                exit_code,
                stdout_blob_ref,
                stderr_blob_ref,
                ..
            } => {
                assert_eq!(
                    exit_code,
                    Some(0),
                    "finish event keeps the provider exit code"
                );
                Some((
                    stdout_blob_ref.expect("stdout blob ref"),
                    stderr_blob_ref.expect("stderr blob ref"),
                ))
            }
            _ => None,
        })
        .expect("CliInvocationFinished")
}
fn write_provider_script(path: &Path, mutate_cwd: bool, primary_drift: Option<&Path>) {
    let mut body =
        String::from("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' 'provider-stderr-marker' >&2\n");
    if mutate_cwd {
        body.push_str("printf mutated > inspection-mutated\n");
    }
    if let Some(primary) = primary_drift {
        body.push_str(&format!("printf drift > '{}'\n", primary.display()));
    }
    body.push_str(
        "printf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"marker\":\"provider-stdout-marker\"},\"error\":null}'\n",
    );
    write_executable(path, &body);
}
pub(super) fn init_repo(path: &Path) -> String {
    fs::create_dir_all(path).expect("repo dir");
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.email", "post-run@example.test"]);
    git(path, &["config", "user.name", "post-run"]);
    fs::write(path.join("README"), "hello\n").expect("readme");
    git(path, &["add", "README"]);
    git(path, &["commit", "-m", "init"]);
    git(path, &["rev-parse", "HEAD"])
}
fn git(repo: &Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "gc.auto=0",
        ])
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git stdout")
        .trim()
        .to_string()
}
