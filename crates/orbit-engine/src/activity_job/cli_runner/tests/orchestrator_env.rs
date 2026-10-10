#![allow(missing_docs)]

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use orbit_agent::loop_engine::audit::NullSink;
#[cfg(target_os = "linux")]
use orbit_exec::probe_bwrap;
#[cfg(target_os = "linux")]
use orbit_types::policy::ResolvedFsProfile;
#[cfg(target_os = "linux")]
use orbit_types::workflow::ExecutorSandboxKind;
use orbit_types::workflow::activity_job::{ActivityToolPolicyMode, V2AuditEventKind};
use tempfile::tempdir;

use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::dispatcher::DispatchError;
#[cfg(target_os = "linux")]
use super::super::super::dispatcher::ResolvedSandbox;
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

/// [ORB-14771] A timeout whose deadline overflows `SystemTime` must not stamp
/// `ORBIT_ACTIVITY_DEADLINE_UNIX_MS`. A stamped 0 reads as an exhausted budget
/// and clamps every nested `proc.spawn` to 0 ms. The timeout sits near
/// `i64::MAX` seconds so the supervisor's `Instant + Duration` still fits.
#[test]
fn unrepresentable_deadline_omits_the_provider_deadline_stamp() {
    let timeout = Duration::from_secs(i64::MAX as u64 - 1_000_000_000);

    assert_eq!(observed_deadline_stamp(timeout), "unset");
}

/// [ORB-14771] A representable timeout still stamps the provider's deadline as
/// a future epoch-millisecond instant, so a nested `proc.spawn` keeps its scope.
#[test]
fn representable_timeout_stamps_a_future_provider_deadline() {
    let before_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis();

    let stamp = observed_deadline_stamp(Duration::from_secs(600));

    let deadline_ms: u128 = stamp.parse().expect("deadline stamp is epoch milliseconds");
    assert!(deadline_ms >= before_ms + 600_000, "{stamp}");
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

/// A claimed leaf's implementer may read its claimed task through the scoped
/// owner route, but cannot write owner task state (distributed-drain design
/// §3). The task update denial is added on top of the activity's own list.
#[test]
fn run_cli_backend_keeps_claimed_task_read_but_denies_owner_task_updates() {
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

const STDOUT_MARKER: &str = "provider-stdout-marker";
const STDERR_MARKER: &str = "provider-stderr-marker";

/// [ORB-14090] A provider that dirties its read-only inspection checkout still
/// leaves stdout/stderr blobs and `CliInvocationFinished` on the run, and the
/// step error names those blobs.
#[test]
fn inspection_mutation_keeps_provider_output_and_finish_event() {
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

/// [ORB-14090] A path the Linux post-run guard rejects is still evidenced:
/// blobs, finish event, exit code, and a permanent step error that cites them.
///
/// Hosts that refuse unprivileged user namespaces cannot exec Bubblewrap, so
/// the provider would never exit and the guard would never run. The test seam
/// keeps the real deny snapshot and execs the provider bare on those hosts.
#[cfg(target_os = "linux")]
#[test]
fn linux_post_run_guard_keeps_provider_output_and_finish_event() {
    let exercise = linux_post_run_guard_exercise;
    if probe_bwrap().available {
        exercise();
    } else {
        crate::activity_job::cli_runner::spawn::with_post_run_guard_without_user_namespace(
            exercise,
        );
    }
}

#[cfg(target_os = "linux")]
fn linux_post_run_guard_exercise() {
    let temp = tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let workspace = workspace.canonicalize().expect("canonical workspace");
    let script = workspace.join("grok");
    // The guard watches an absent deny root. The child creates it after the
    // spawn snapshot; Bubblewrap cannot mount a root that does not exist yet.
    write_guard_script(&script);
    let audit_root = temp.path().join("audit");
    let audit = persisted_writer(&audit_root, "job-linux-guard", "grok:grok-build");
    let sandbox = ResolvedSandbox {
        kind: ExecutorSandboxKind::LinuxBwrap,
        fs_profile: ResolvedFsProfile {
            name: "post-run-guard".to_string(),
            read: vec!["/**".to_string()],
            modify: vec![
                format!("{}/**", workspace.display()),
                format!("!{}/**", workspace.join("forbidden-secret").display()),
            ],
        },
        allow_fallback: false,
        managed_worktree: true,
        runtime_write_authority: Vec::new(),
        mask: None,
    };

    let error = run_cli_backend(
        &TestHost::with_command(script.display().to_string()).with_sandbox(sandbox),
        &test_agent_loop_spec_for("grok", Duration::from_secs(15)),
        "test_activity",
        "job-linux-guard",
        audit.clone(),
        &serde_json::json!({
            "prompt": "hi",
            "workspace_path": workspace,
        }),
        Some("implementer"),
    )
    .expect_err("linux post-run guard fails the step");

    let DispatchError::CliInvocationPermanent(message) = &error else {
        panic!("guard failure stays permanent: {error}");
    };
    assert!(
        message.contains("linux-bwrap child created a path forbidden by denyModify"),
        "{message}"
    );
    assert_output_survives(&error, &audit, &audit_root);
}

/// A provider that creates a denyModify match only in run scratch (a test
/// fixture's `clock.env`) completes the step: the guard removes the match and
/// records it as a denied modify, while the scratch directory itself stays.
///
/// Admitted under criterion 3: the scratch exemption is a write-policy
/// invariant, and a host that refuses user namespaces reaches it only through
/// this crate's post-run guard seam.
#[cfg(target_os = "linux")]
#[test]
fn linux_post_run_guard_removes_scratch_only_match_and_completes() {
    let exercise = linux_post_run_guard_scratch_exercise;
    if probe_bwrap().available {
        exercise();
    } else {
        crate::activity_job::cli_runner::spawn::with_post_run_guard_without_user_namespace(
            exercise,
        );
    }
}

#[cfg(target_os = "linux")]
fn linux_post_run_guard_scratch_exercise() {
    let temp = tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let workspace = workspace.canonicalize().expect("canonical workspace");
    let script = workspace.join("grok");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nmkdir -p .orbit/tmp/clock-probe/home/.orbit\nprintf secret > .orbit/tmp/clock-probe/home/.orbit/clock.env\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"marker\":\"provider-stdout-marker\"},\"error\":null}'\n",
    );
    let audit_root = temp.path().join("audit");
    let audit = persisted_writer(&audit_root, "job-linux-scratch", "grok:grok-build");
    let root = workspace.display().to_string();
    let sandbox = ResolvedSandbox {
        kind: ExecutorSandboxKind::LinuxBwrap,
        fs_profile: ResolvedFsProfile {
            name: "post-run-scratch".to_string(),
            read: vec!["/**".to_string()],
            modify: vec![
                format!("{root}/**"),
                format!("!{root}/.orbit/**"),
                format!("{root}/.orbit/tmp/**"),
                format!("!{root}/**/*.env"),
            ],
        },
        allow_fallback: false,
        managed_worktree: true,
        runtime_write_authority: Vec::new(),
        mask: None,
    };

    run_cli_backend(
        &TestHost::with_command(script.display().to_string()).with_sandbox(sandbox),
        &test_agent_loop_spec_for("grok", Duration::from_secs(15)),
        "test_activity",
        "job-linux-scratch",
        audit.clone(),
        &serde_json::json!({
            "prompt": "hi",
            "workspace_path": workspace,
        }),
        Some("implementer"),
    )
    .expect("a scratch-only denyModify match does not fail the step");

    let created = workspace.join(".orbit/tmp/clock-probe/home/.orbit/clock.env");
    assert!(!created.exists(), "the scratch match is removed");
    assert!(
        workspace
            .join(".orbit/tmp/clock-probe/home/.orbit")
            .is_dir(),
        "only the matched path is removed"
    );
    let denied: Vec<(String, String, String)> = audit
        .events_snapshot()
        .expect("audit snapshot")
        .into_iter()
        .filter_map(|event| match event.kind {
            V2AuditEventKind::FsCallDenied {
                profile,
                path,
                matched_rule,
                ..
            } => Some((profile, path, matched_rule)),
            _ => None,
        })
        .collect();
    assert_eq!(
        denied,
        vec![(
            "post-run-scratch".to_string(),
            created.display().to_string(),
            format!("!{root}/**/*.env"),
        )],
        "the removal is recorded as a denied modify"
    );
}

/// [ORB-14090] A persistence refresh failure still stores the provider output
/// and emits the finish event through the connection the refresh left in place.
#[test]
fn persistence_refresh_failure_keeps_provider_output_and_finish_event() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_provider_script(&script, false, None);
    let audit_root = temp.path().join("audit");
    let audit = persisted_writer(&audit_root, "job-refresh-failure", "grok:grok-build");
    let host = TestHost::with_command(script.display().to_string())
        .fail_persistence_refresh("audit connection refresh refused");

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec_for("grok", Duration::from_secs(10)),
        "test_activity",
        "job-refresh-failure",
        audit.clone(),
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect_err("refresh failure fails the step");

    let DispatchError::CliInvocationPermanent(message) = &error else {
        panic!("refresh failure stays permanent: {error}");
    };
    assert!(
        message.contains("refresh durable store after provider `grok` exited"),
        "{message}"
    );
    assert!(
        message.contains("audit connection refresh refused"),
        "{message}"
    );
    assert_output_survives(&error, &audit, &audit_root);
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

#[cfg(target_os = "linux")]
fn write_guard_script(path: &Path) {
    write_executable(
        path,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' 'provider-stderr-marker' >&2\nmkdir -p forbidden-secret\nprintf leak > forbidden-secret/created\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"marker\":\"provider-stdout-marker\"},\"error\":null}'\n",
    );
}

fn init_repo(path: &Path) -> String {
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
    let output = Command::new("git")
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
