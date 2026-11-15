#![allow(missing_docs)]

#[cfg(target_os = "linux")]
use std::fs;
use std::path::Path;
use std::time::Duration;

#[cfg(target_os = "linux")]
use orbit_exec::probe_bwrap;
#[cfg(target_os = "linux")]
use orbit_types::policy::ResolvedFsProfile;
#[cfg(target_os = "linux")]
use orbit_types::workflow::ExecutorSandboxKind;
use orbit_types::workflow::activity_job::V2AuditEventKind;
use tempfile::tempdir;

use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::dispatcher::DispatchError;
#[cfg(target_os = "linux")]
use super::super::super::dispatcher::ResolvedSandbox;
use super::super::run_cli_backend;
use super::test_support::{
    TestHost, persisted_blobs, persisted_writer, test_agent_loop_spec_for, write_executable,
};

const STDOUT_MARKER: &str = "provider-stdout-marker";
const STDERR_MARKER: &str = "provider-stderr-marker";

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

/// [ORB-21352] A write-granting sandboxed dispatch gives the child a
/// `CARGO_DENY_DB_PATH` under the run's scratch dir, created by dispatch,
/// and sets no disable-fetch variable.
#[cfg(target_os = "linux")]
#[test]
fn write_granting_dispatch_injects_cargo_deny_db_path_under_scratch() {
    let exercise = write_granting_cargo_deny_exercise;
    if probe_bwrap().available {
        exercise();
    } else {
        crate::activity_job::cli_runner::spawn::with_post_run_guard_without_user_namespace(
            exercise,
        );
    }
}

#[cfg(target_os = "linux")]
fn write_granting_cargo_deny_exercise() {
    let temp = tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let workspace = workspace.canonicalize().expect("canonical workspace");
    let script = workspace.join("grok");
    write_env_probe_script(&script);
    let audit_root = temp.path().join("audit");
    let audit = persisted_writer(&audit_root, "job-cargo-deny-writer", "grok:grok-build");
    let root = workspace.display().to_string();
    let sandbox = ResolvedSandbox {
        kind: ExecutorSandboxKind::LinuxBwrap,
        fs_profile: ResolvedFsProfile {
            name: "implementer".to_string(),
            read: vec!["/**".to_string()],
            modify: vec![
                format!("{root}/**"),
                format!("!{root}/.orbit/**"),
                format!("{root}/.orbit/tmp/**"),
            ],
        },
        allow_fallback: false,
        managed_worktree: true,
        runtime_write_authority: Vec::new(),
        mask: None,
    };

    let outcome = run_cli_backend(
        &TestHost::with_command(script.display().to_string()).with_sandbox(sandbox),
        &test_agent_loop_spec_for("grok", Duration::from_secs(15)),
        "test_activity",
        "job-cargo-deny-writer",
        audit,
        &serde_json::json!({
            "prompt": "hi",
            "workspace_path": workspace,
        }),
        Some("implementer"),
    )
    .expect("write-granting dispatch completes");

    let expected = workspace.join(".orbit/tmp/cargo-deny-advisory-db");
    assert!(
        expected.is_dir(),
        "dispatch creates the advisory database directory"
    );
    assert_eq!(
        outcome.output["cargo_deny_db_path"],
        expected.display().to_string(),
        "the child receives the per-run advisory database path"
    );
    assert_eq!(
        outcome.output["cargo_deny_disable_fetch"], "",
        "the injection sets no CARGO_DENY_DISABLE_FETCH"
    );
    assert_eq!(
        outcome.output["orbit_cargo_deny_disable_fetch"], "",
        "the injection sets no ORBIT_CARGO_DENY_DISABLE_FETCH"
    );
}

/// [ORB-21352] A profile that grants no scratch writes gains no advisory
/// path and no created directory; cargo-deny there still fails closed.
#[cfg(target_os = "linux")]
#[test]
fn read_only_dispatch_injects_no_cargo_deny_db_path() {
    let exercise = read_only_cargo_deny_exercise;
    if probe_bwrap().available {
        exercise();
    } else {
        crate::activity_job::cli_runner::spawn::with_post_run_guard_without_user_namespace(
            exercise,
        );
    }
}

#[cfg(target_os = "linux")]
fn read_only_cargo_deny_exercise() {
    // Bubblewrap mounts a private tmpfs over /tmp, and this reviewer profile
    // binds nothing writable, so a fixture under /tmp is hidden from the
    // probe and the child never runs. Root the fixture on disk outside /tmp,
    // as the sandbox tests do.
    let scratch = orbit_common::fs::path::ensure_orbit_scratch_dir(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    )
    .expect("scratch dir");
    let temp = tempfile::Builder::new()
        .prefix("read-only-cargo-deny-")
        .tempdir_in(scratch)
        .expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let workspace = workspace.canonicalize().expect("canonical workspace");
    let script = workspace.join("grok");
    write_env_probe_script(&script);
    let audit_root = temp.path().join("audit");
    let audit = persisted_writer(&audit_root, "job-cargo-deny-reviewer", "grok:grok-build");
    let sandbox = ResolvedSandbox {
        kind: ExecutorSandboxKind::LinuxBwrap,
        fs_profile: ResolvedFsProfile {
            name: "reviewer".to_string(),
            read: vec!["/**".to_string()],
            modify: Vec::new(),
        },
        allow_fallback: false,
        managed_worktree: true,
        runtime_write_authority: Vec::new(),
        mask: None,
    };

    let outcome = run_cli_backend(
        &TestHost::with_command(script.display().to_string()).with_sandbox(sandbox),
        &test_agent_loop_spec_for("grok", Duration::from_secs(15)),
        "test_activity",
        "job-cargo-deny-reviewer",
        audit,
        &serde_json::json!({
            "prompt": "hi",
            "workspace_path": workspace,
        }),
        Some("reviewer"),
    )
    .expect("read-only dispatch completes");

    assert_eq!(
        outcome.output["cargo_deny_db_path"], "",
        "a read-only profile receives no advisory database path"
    );
    assert!(
        !workspace.join(".orbit/tmp/cargo-deny-advisory-db").exists(),
        "dispatch creates no advisory database directory for a read-only profile"
    );
}

/// [ORB-21352] When the per-run advisory directory cannot be created,
/// dispatch fails instead of skipping the advisory check.
#[cfg(target_os = "linux")]
#[test]
fn uncreatable_cargo_deny_db_path_fails_dispatch() {
    let exercise = uncreatable_cargo_deny_exercise;
    if probe_bwrap().available {
        exercise();
    } else {
        crate::activity_job::cli_runner::spawn::with_post_run_guard_without_user_namespace(
            exercise,
        );
    }
}

#[cfg(target_os = "linux")]
fn uncreatable_cargo_deny_exercise() {
    let temp = tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(workspace.join(".orbit/tmp")).expect("scratch");
    let workspace = workspace.canonicalize().expect("canonical workspace");
    fs::write(
        workspace.join(".orbit/tmp/cargo-deny-advisory-db"),
        "not a directory",
    )
    .expect("block the advisory database path");
    let script = workspace.join("grok");
    write_env_probe_script(&script);
    let audit_root = temp.path().join("audit");
    let audit = persisted_writer(&audit_root, "job-cargo-deny-blocked", "grok:grok-build");
    let root = workspace.display().to_string();
    let sandbox = ResolvedSandbox {
        kind: ExecutorSandboxKind::LinuxBwrap,
        fs_profile: ResolvedFsProfile {
            name: "implementer".to_string(),
            read: vec!["/**".to_string()],
            modify: vec![
                format!("{root}/**"),
                format!("!{root}/.orbit/**"),
                format!("{root}/.orbit/tmp/**"),
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
        "job-cargo-deny-blocked",
        audit,
        &serde_json::json!({
            "prompt": "hi",
            "workspace_path": workspace,
        }),
        Some("implementer"),
    )
    .expect_err("an uncreatable advisory directory fails dispatch");

    let DispatchError::CliInvocationPermanent(message) = &error else {
        panic!("creation failure stays permanent: {error}");
    };
    assert!(
        message.contains("create cargo-deny advisory database dir"),
        "{message}"
    );
}

#[cfg(target_os = "linux")]
fn write_env_probe_script(path: &Path) {
    write_executable(
        path,
        "#!/bin/sh\ncat > /dev/null\nprintf '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"cargo_deny_db_path\":\"%s\",\"cargo_deny_disable_fetch\":\"%s\",\"orbit_cargo_deny_disable_fetch\":\"%s\"},\"error\":null}\\n' \"$CARGO_DENY_DB_PATH\" \"${CARGO_DENY_DISABLE_FETCH:-}\" \"${ORBIT_CARGO_DENY_DISABLE_FETCH:-}\"\n",
    );
}

#[cfg(target_os = "linux")]
fn write_guard_script(path: &Path) {
    write_executable(
        path,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' 'provider-stderr-marker' >&2\nmkdir -p forbidden-secret\nprintf leak > forbidden-secret/created\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"marker\":\"provider-stdout-marker\"},\"error\":null}'\n",
    );
}
