//! Trusted-host execution admission at the CLI-runner boundary [ORB-11354].
//!
//! The mode has two halves — the activity declares it, the operator's
//! submission admits it — and only both halves together, with a well-formed
//! admission, may run an unsandboxed process.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use orbit_agent::loop_engine::audit::NullSink;
use orbit_common::OrbitError;
use orbit_types::workflow::activity_job::{
    TRUSTED_HOST_ADMISSION_KEY, TrustedHostAdmission, V2AuditEvent, V2AuditEventKind,
};
use tempfile::tempdir;

use orbit_engine::activity_job::audit_writer::{EnvelopeSink, V2AuditWriter};

use super::cli_support::{TestHost, persisted_writer, test_agent_loop_spec_for, write_executable};
use orbit_engine::DispatchError;
use orbit_engine::activity_job::cli_runner::run_cli_backend;

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
fn a_forged_admission_value_does_not_admit_the_mode() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::a_forged_admission_value_does_not_admit_the_mode"
    )) {
        return;
    }

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
fn an_admission_alone_does_not_remove_an_ordinary_activitys_sandbox() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::an_admission_alone_does_not_remove_an_ordinary_activitys_sandbox"
    )) {
        return;
    }

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

/// The target activity controls authorization, while the audit event keeps
/// the pipeline step id for operators reading the run trace.
fn distinct_target_keeps_the_pipeline_step_id_in_trusted_host_audit() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::distinct_target_keeps_the_pipeline_step_id_in_trusted_host_audit"
    )) {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let host = TestHost::with_command(echoing_provider(temp.path()));
    let mut spec = test_agent_loop_spec_for("codex", Duration::from_secs(10));
    spec.trusted_host_execution = true;
    let captured = Arc::new(CapturedEnvelopes::default());
    let audit = Arc::new(
        V2AuditWriter::new(
            "job-trusted-distinct-target",
            "codex:gpt-5.5",
            Arc::new(NullSink),
        )
        .with_envelope_sink(captured.clone()),
    );

    let mut target = super::job_support::target_step("review", "unused");
    let orbit_types::workflow::activity_job::JobV2StepBody::Target(body) = &mut target.body else {
        panic!("target");
    };
    body.spec = orbit_types::workflow::activity_job::ActivityV2Spec::AgentLoop(spec);
    body.activity_name = Some("agent_review_repair".into());
    let outcome = orbit_engine::execute_job_with_resume(
        &super::job_support::job_with_steps(vec![target]),
        admitted_input(),
        "job-trusted-distinct-target",
        audit.clone(),
        &host,
        None,
    )
    .expect("admitted trusted-host activity runs");
    assert!(outcome.success);

    let events = captured.0.lock().expect("captured events");
    let activity = events.iter().find_map(|event| match &event.kind {
        V2AuditEventKind::TrustedHostExecutionAdmitted { activity_name, .. } => {
            Some(activity_name.as_str())
        }
        _ => None,
    });
    assert_eq!(
        activity,
        Some("review"),
        "trusted-host audit labels retain the step id while policy uses the target activity"
    );
}

#[derive(Default)]
struct CapturedEnvelopes(Mutex<Vec<V2AuditEvent>>);

impl EnvelopeSink for CapturedEnvelopes {
    fn write_envelope(&self, event: &V2AuditEvent) -> Result<(), OrbitError> {
        self.0.lock().expect("captured events").push(event.clone());
        Ok(())
    }
}

#[test]
fn trusted_host_authority_contracts() {
    super::run_family(
        concat!(module_path!(), "::trusted_host_authority_contracts"),
        module_path!(),
        &[
            (
                "a_forged_admission_value_does_not_admit_the_mode",
                a_forged_admission_value_does_not_admit_the_mode,
            ),
            (
                "an_admission_alone_does_not_remove_an_ordinary_activitys_sandbox",
                an_admission_alone_does_not_remove_an_ordinary_activitys_sandbox,
            ),
            (
                "distinct_target_keeps_the_pipeline_step_id_in_trusted_host_audit",
                distinct_target_keeps_the_pipeline_step_id_in_trusted_host_audit,
            ),
        ],
    );
}
