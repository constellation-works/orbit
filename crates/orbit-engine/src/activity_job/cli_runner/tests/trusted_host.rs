//! Trusted-host execution admission at the CLI-runner boundary [ORB-11354].
//!
//! The mode has two halves — the activity declares it, the operator's
//! submission admits it — and these tests pin each combination, because only
//! one of them may run an unsandboxed process.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use orbit_agent::loop_engine::audit::AuditSink;
use orbit_types::tool::{CallerIdentityProof, RemoteAgentInvokeMode};
use orbit_types::workflow::activity_job::{
    TRUSTED_HOST_ADMISSION_KEY, TrustedHostAdmission, V2AuditEventKind,
};
use tempfile::tempdir;

use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::dispatcher::DispatchError;
use super::super::run_cli_backend;
use super::test_support::{RecordingSink, TestHost, test_agent_loop_spec, write_executable};

fn admission() -> TrustedHostAdmission {
    TrustedHostAdmission {
        authorized_by: "hm_mac".to_string(),
        authorizer_provenance: "remote-grant".to_string(),
        caller_machine_id: Some("hm_mac".to_string()),
        caller_identity: Some(CallerIdentityProof::KeyBound),
        agent_invoke_mode: Some(RemoteAgentInvokeMode::KeyBound),
        authorized_at: "2026-09-06T00:00:00Z".to_string(),
        workspace_path: "/checkout".to_string(),
        cwd: "/checkout".to_string(),
    }
}

fn admitted_input() -> serde_json::Value {
    serde_json::json!({
        "prompt": "why is this host slow",
        TRUSTED_HOST_ADMISSION_KEY: serde_json::to_value(admission()).expect("encode admission"),
    })
}

fn writer(run_id: &'static str) -> (Arc<V2AuditWriter>, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink.clone();
    (
        Arc::new(V2AuditWriter::new(run_id, "codex:gpt-5.5", sink_for_writer)),
        sink,
    )
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

/// The admitted path: flag plus admission runs, and says so in the trail.
#[test]
fn an_admitted_invocation_runs_without_a_sandbox_and_records_its_authorizer() {
    let temp = tempdir().expect("tempdir");
    let host = TestHost::with_command(echoing_provider(temp.path()));
    let mut spec = test_agent_loop_spec(Duration::from_secs(10));
    spec.trusted_host_execution = true;
    let (audit, _sink) = writer("job-trusted-ok");

    let outcome = run_cli_backend(
        &host,
        &spec,
        "agent_invoke",
        "job-trusted-ok",
        audit.clone(),
        &admitted_input(),
        None,
    )
    .expect("an admitted invocation runs");

    assert!(outcome.success, "provider completed its envelope");

    let events = audit.events_snapshot().expect("events snapshot");
    let admitted = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::TrustedHostExecutionAdmitted {
                activity_name,
                authorized_by,
                authorizer_provenance,
                caller_machine_id,
                caller_identity,
                agent_invoke_mode,
                cwd,
                ..
            } => Some((
                activity_name.clone(),
                authorized_by.clone(),
                authorizer_provenance.clone(),
                caller_machine_id.clone(),
                *caller_identity,
                *agent_invoke_mode,
                cwd.clone(),
            )),
            _ => None,
        })
        .expect("an unsandboxed invocation must announce itself in the run trail");
    assert_eq!(admitted.0, "agent_invoke");
    assert_eq!(admitted.1, "hm_mac");
    assert_eq!(admitted.2, "remote-grant");
    assert_eq!(admitted.3.as_deref(), Some("hm_mac"));
    assert_eq!(admitted.4, Some(CallerIdentityProof::KeyBound));
    assert_eq!(admitted.5, Some(RemoteAgentInvokeMode::KeyBound));
    assert_eq!(admitted.6, "/checkout");

    // The absence of a sandbox is stated, not left to be inferred from a
    // missing field, so a reader can tell it apart from an executor that never
    // declared one.
    let backend = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted {
                sandbox_backend,
                sandbox_write_enforcement,
                ..
            } => Some((sandbox_backend.clone(), sandbox_write_enforcement.clone())),
            _ => None,
        })
        .expect("started event");
    assert_eq!(backend.0.as_deref(), Some("none-trusted-host"));
    assert_eq!(
        backend.1.as_deref(),
        Some("write_unrestricted_trusted_host")
    );
}

#[test]
fn a_cooperative_admission_is_audited_as_self_asserted_not_key_bound() {
    let temp = tempdir().expect("tempdir");
    let host = TestHost::with_command(echoing_provider(temp.path()));
    let mut spec = test_agent_loop_spec(Duration::from_secs(10));
    spec.trusted_host_execution = true;
    let (audit, _sink) = writer("job-trusted-cooperative");
    let mut input = admitted_input();
    input[TRUSTED_HOST_ADMISSION_KEY]["caller_identity"] =
        serde_json::json!(CallerIdentityProof::SelfAsserted);
    input[TRUSTED_HOST_ADMISSION_KEY]["agent_invoke_mode"] =
        serde_json::json!(RemoteAgentInvokeMode::Cooperative);

    run_cli_backend(
        &host,
        &spec,
        "agent_invoke",
        "job-trusted-cooperative",
        audit.clone(),
        &input,
        None,
    )
    .expect("cooperative admission runs");

    let events = audit.events_snapshot().expect("events snapshot");
    let (identity, mode) = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::TrustedHostExecutionAdmitted {
                caller_identity,
                agent_invoke_mode,
                ..
            } => Some((*caller_identity, *agent_invoke_mode)),
            _ => None,
        })
        .expect("trusted-host admission event");

    assert_eq!(identity, Some(CallerIdentityProof::SelfAsserted));
    assert_eq!(mode, Some(RemoteAgentInvokeMode::Cooperative));
}

/// The dangerous combination: the asset claims the mode, nothing admitted it.
/// It must fail, not quietly run sandboxed — a silent downgrade would hide a
/// broken admission path behind a confusing sandbox denial much later.
#[test]
fn a_declared_mode_without_an_admission_fails_closed() {
    let temp = tempdir().expect("tempdir");
    let host = TestHost::with_command(echoing_provider(temp.path()));
    let mut spec = test_agent_loop_spec(Duration::from_secs(10));
    spec.trusted_host_execution = true;
    let (audit, _sink) = writer("job-trusted-unadmitted");

    let error = run_cli_backend(
        &host,
        &spec,
        "agent_invoke",
        "job-trusted-unadmitted",
        audit.clone(),
        &serde_json::json!({ "prompt": "why" }),
        None,
    )
    .expect_err("an unadmitted invocation must not run");

    match error {
        DispatchError::CliInvocationPermanent(message) => assert_eq!(
            message,
            "activity `agent_invoke` declares trusted host execution but this run carries \
             no operator admission; submit it through the governed `orbit.agent.invoke` \
             operation"
        ),
        other => panic!("expected a permanent refusal, got {other:?}"),
    }
    assert!(
        audit
            .events_snapshot()
            .expect("events snapshot")
            .iter()
            .all(|event| !matches!(
                event.kind,
                V2AuditEventKind::CliInvocationStarted { .. }
                    | V2AuditEventKind::TrustedHostExecutionAdmitted { .. }
            )),
        "nothing may be spawned before the admission check"
    );
}

/// A malformed admission is not an admission.
#[test]
fn a_forged_admission_value_does_not_admit_the_mode() {
    let temp = tempdir().expect("tempdir");
    let host = TestHost::with_command(echoing_provider(temp.path()));
    let mut spec = test_agent_loop_spec(Duration::from_secs(10));
    spec.trusted_host_execution = true;
    let (audit, _sink) = writer("job-trusted-forged");

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
    let spec = test_agent_loop_spec(Duration::from_secs(10));
    assert!(!spec.trusted_host_execution);
    let (audit, _sink) = writer("job-untrusted-with-admission");

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

/// A persisted Codex override reaches `--sandbox` instead of the host default.
#[test]
fn a_trusted_host_codex_override_reaches_the_provider_argv() {
    let temp = tempdir().expect("tempdir");
    let script = echoing_provider(temp.path());
    let mut provider_config = HashMap::new();
    provider_config.insert("sandbox".to_string(), "danger-full-access".to_string());
    let host = TestHost {
        command: script.clone(),
        executor_args: Vec::new(),
        provider_config,
        sandbox: None,
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let mut spec = test_agent_loop_spec(Duration::from_secs(10));
    spec.trusted_host_execution = true;
    let (audit, _sink) = writer("job-trusted-sandbox-override");
    let mut input = admitted_input();
    input["provider_sandbox"] = serde_json::json!("codex:read-only");

    run_cli_backend(
        &host,
        &spec,
        "agent_invoke",
        "job-trusted-sandbox-override",
        audit.clone(),
        &input,
        None,
    )
    .expect("admitted invocation runs");

    let argv = audit
        .events_snapshot()
        .expect("events snapshot")
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { argv_redacted, .. } => {
                Some(argv_redacted.clone())
            }
            _ => None,
        })
        .expect("started event");
    assert!(
        argv.windows(2)
            .any(|pair| pair[0] == "--sandbox" && pair[1] == "read-only"),
        "expected --sandbox read-only in {argv:?}"
    );
}

/// An operator may shorten the activity's bound, never extend it.
#[test]
fn a_requested_timeout_can_only_shorten_the_declared_bound() {
    let temp = tempdir().expect("tempdir");
    let host = TestHost::with_command(echoing_provider(temp.path()));
    let mut spec = test_agent_loop_spec(Duration::from_secs(60));
    spec.trusted_host_execution = true;

    for (requested, expected_ms) in [(30_u64, 30_000_u64), (600, 60_000)] {
        let (audit, _sink) = writer("job-trusted-timeout");
        let mut input = admitted_input();
        input["timeout_seconds"] = serde_json::json!(requested);
        run_cli_backend(
            &host,
            &spec,
            "agent_invoke",
            "job-trusted-timeout",
            audit.clone(),
            &input,
            None,
        )
        .expect("run");
        let observed = audit
            .events_snapshot()
            .expect("events snapshot")
            .iter()
            .find_map(|event| match &event.kind {
                V2AuditEventKind::CliInvocationStarted {
                    wall_clock_timeout_ms,
                    ..
                } => Some(*wall_clock_timeout_ms),
                _ => None,
            })
            .expect("started event");
        assert_eq!(
            observed, expected_ms,
            "requested {requested}s against a 60s declared bound"
        );
    }
}

/// A `timeout_seconds` in ordinary run input is inert: only the admitted mode
/// reads it, so no activity's declared bound can be moved from run input.
#[test]
fn an_ordinary_activity_ignores_a_requested_timeout() {
    let temp = tempdir().expect("tempdir");
    let host = TestHost::with_command(echoing_provider(temp.path()));
    let spec = test_agent_loop_spec(Duration::from_secs(45));
    let (audit, _sink) = writer("job-untrusted-timeout");

    run_cli_backend(
        &host,
        &spec,
        "agent_implement",
        "job-untrusted-timeout",
        audit.clone(),
        &serde_json::json!({ "prompt": "x", "timeout_seconds": 1 }),
        None,
    )
    .expect("run");

    let observed = audit
        .events_snapshot()
        .expect("events snapshot")
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted {
                wall_clock_timeout_ms,
                ..
            } => Some(*wall_clock_timeout_ms),
            _ => None,
        })
        .expect("started event");
    assert_eq!(observed, 45_000);
}

/// Captured leftover review minutes may only shorten the activity ceiling.
#[test]
fn remaining_seconds_can_only_shorten_the_declared_bound() {
    let temp = tempdir().expect("tempdir");
    let host = TestHost::with_command(echoing_provider(temp.path()));
    let spec = test_agent_loop_spec(Duration::from_secs(60));

    for (remaining, expected_ms) in [(30_u64, 30_000_u64), (600, 60_000), (0, 1_000)] {
        let (audit, _sink) = writer("job-review-remaining");
        run_cli_backend(
            &host,
            &spec,
            "agent_review_repair",
            "job-review-remaining",
            audit.clone(),
            &serde_json::json!({
                "prompt": "x",
                "remaining_seconds": remaining
            }),
            None,
        )
        .expect("run");
        let observed = audit
            .events_snapshot()
            .expect("events snapshot")
            .iter()
            .find_map(|event| match &event.kind {
                V2AuditEventKind::CliInvocationStarted {
                    wall_clock_timeout_ms,
                    ..
                } => Some(*wall_clock_timeout_ms),
                _ => None,
            })
            .expect("started event");
        assert_eq!(
            observed, expected_ms,
            "remaining {remaining}s against a 60s declared bound"
        );
    }
}
