//! Who may admit an operator-only agent invocation, and why no other
//! submission path can manufacture one [ORB-11354].

use orbit_common::OrbitError;
use orbit_types::tool::{McpCapability, ToolSessionContext};

use crate::OrbitRuntime;
use crate::application::job::{AGENT_INVOKE_JOB_ID, AgentInvokeRequest};

fn request<'a>(cwd: &'a str, session: &'a ToolSessionContext) -> AgentInvokeRequest<'a> {
    AgentInvokeRequest {
        prompt: "why is the sweep clock restarting",
        cwd,
        crew: None,
        timeout_seconds: None,
        idempotency_key: None,
        provider_sandbox: None,
        actor: Some("human"),
        session_context: session,
    }
}

// ---------------------------------------------------------------- admission

fn assert_no_run_created(runtime: &OrbitRuntime) {
    let runs = runtime
        .list_job_runs(crate::application::job::JobRunListParams::default())
        .expect("list runs");
    assert!(
        runs.is_empty(),
        "a refused caller must not leave a durable run behind"
    );
}

// --------------------------------------------------------------- validation
//
// Validation runs *after* admission, so these use an operator session. They
// assert the invocation's own contract: an explicit, containable cwd and a
// bounded timeout.

// -------------------------------------------------------------- forgery

// ------------------------------------------------------------ result reading

// ---------------------------------------------------- restart and retry

// ------------------------------------------------------ provider sandbox

/// Uses the deterministic audit-insert fault and detached-worker seam so no
/// provider executes (unit-test admission criterion 2).
#[cfg(unix)]
#[test]
fn invocation_audit_failure_preserves_admission_retry_and_original_error() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &invocation_audit_failure_preserves_admission_retry_and_original_error,
    )) {
        return;
    }
    use crate::application::job::tests::SubmissionAuditFault;

    let session = ToolSessionContext {
        effective_capabilities: [McpCapability::Operator].into_iter().collect(),
        ..ToolSessionContext::default()
    };
    for enabled in [true, false] {
        let fixture = SubmissionAuditFault::new(AGENT_INVOKE_JOB_ID, enabled);
        let cwd = fixture.runtime.paths().repo_root.display().to_string();
        let submit = || {
            fixture.runtime.submit_agent_invoke_run(AgentInvokeRequest {
                idempotency_key: Some("audit-retry"),
                ..request(&cwd, &session)
            })
        };
        let result = fixture.capture(submit);
        if enabled {
            let result = result.expect("the audit failure must not hide an admitted invocation");
            assert!(!result.deduplicated);
            let run = fixture
                .runtime
                .stores()
                .jobs()
                .get_job_run(&result.run_id)
                .unwrap()
                .unwrap();
            assert_eq!(run.job_id, AGENT_INVOKE_JOB_ID);
            fixture.assert_warning(Some(&result.run_id));
            let retry = fixture
                .capture(submit)
                .expect("retry must resolve the admitted run");
            assert!(retry.deduplicated);
            assert_eq!(retry.run_id, result.run_id);
            fixture.assert_warning(Some(&retry.run_id));
            assert_eq!(
                fixture
                    .runtime
                    .stores()
                    .jobs()
                    .list_job_runs(AGENT_INVOKE_JOB_ID,)
                    .unwrap()
                    .len(),
                1,
                "a retry must not admit a second invocation"
            );
        } else {
            assert!(
                matches!(result, Err(OrbitError::InvalidInput(_))),
                "preserve the original invocation error: {result:?}"
            );
            fixture.assert_warning(None);
            assert_no_run_created(&fixture.runtime);
        }
    }
}
