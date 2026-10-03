//! Trusted-host execution admission at the CLI-runner boundary [ORB-11354].
//!
//! The mode has two halves — the activity declares it, the operator's
//! submission admits it — and only both halves together, with a well-formed
//! admission, may run an unsandboxed process.

use std::sync::Arc;
use std::time::Duration;

use orbit_agent::loop_engine::audit::NullSink;
use orbit_types::workflow::activity_job::{
    TRUSTED_HOST_ADMISSION_KEY, TrustedHostAdmission, V2AuditEventKind,
};
use tempfile::tempdir;

use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::dispatcher::DispatchError;
use super::super::run_cli_backend;
use super::test_support::{TestHost, persisted_writer, test_agent_loop_spec_for, write_executable};

fn admitted_input() -> serde_json::Value {
    let admission = TrustedHostAdmission {
        authorized_by: "hm_mac".to_string(),
        authorizer_provenance: "session".to_string(),
        caller_machine_id: Some("hm_mac".to_string()),
        authorized_at: "2026-09-06T00:00:00Z".to_string(),
        workspace_path: "/checkout".to_string(),
        cwd: "/checkout".to_string(),
    };
    serde_json::json!({
        "prompt": "why is this host slow",
        TRUSTED_HOST_ADMISSION_KEY: serde_json::to_value(admission).expect("encode admission"),
    })
}

fn echoing_provider(dir: &std::path::Path) -> String {
    let script = dir.join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' \
         '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"summary\":\"ok\"},\"error\":null}'\n",
    );
    script.display().to_string()
}

/// A malformed admission is not an admission.
#[test]
fn a_forged_admission_value_does_not_admit_the_mode() {
    let temp = tempdir().expect("tempdir");
    let host = TestHost::with_command(echoing_provider(temp.path()));
    let mut spec = test_agent_loop_spec_for("codex", Duration::from_secs(10));
    spec.trusted_host_execution = true;
    let audit = Arc::new(V2AuditWriter::new(
        "job-trusted-forged",
        "codex:gpt-5.5",
        Arc::new(NullSink),
    ));

    let error = run_cli_backend(
        &host,
        &spec,
        "agent_invoke",
        "job-trusted-forged",
        audit,
        &serde_json::json!({ TRUSTED_HOST_ADMISSION_KEY: true }),
        None,
    )
    .expect_err("a non-admission value must not admit the mode");
    assert!(matches!(error, DispatchError::CliInvocationPermanent(_)));
}

/// The inverse: an admission riding on an activity that never declared the mode
/// changes nothing. Ordinary managed activities keep their sandbox resolution.
#[test]
fn an_admission_alone_does_not_remove_an_ordinary_activitys_sandbox() {
    let temp = tempdir().expect("tempdir");
    let host = TestHost::with_command(echoing_provider(temp.path()));
    let spec = test_agent_loop_spec_for("codex", Duration::from_secs(10));
    assert!(!spec.trusted_host_execution);
    let audit = persisted_writer(
        &temp.path().join("audit"),
        "job-untrusted-with-admission",
        "codex:gpt-5.5",
    );

    let outcome = run_cli_backend(
        &host,
        &spec,
        "agent_implement",
        "job-untrusted-with-admission",
        audit.clone(),
        &admitted_input(),
        None,
    )
    .expect("an ordinary activity still runs");
    assert!(outcome.success);

    let events = audit.events_snapshot().expect("events snapshot");
    assert!(
        events.iter().all(|event| !matches!(
            event.kind,
            V2AuditEventKind::TrustedHostExecutionAdmitted { .. }
        )),
        "an activity that did not declare the mode must not enter it"
    );
    // `TestHost` declares no sandbox, so the metadata here is the ordinary
    // "this executor has none" shape rather than the trusted-host one.
    let backend = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted {
                sandbox_backend, ..
            } => Some(sandbox_backend.clone()),
            _ => None,
        })
        .expect("started event");
    assert_eq!(backend, None);
}
