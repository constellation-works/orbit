use std::time::Duration;

use orbit_common::process::build_budget::BuildBudgetWaits;
use orbit_common::security::redaction::PatternRedactor;
use orbit_types::workflow::activity_job::AgentLoopSpec;
use serde_json::json;

use super::super::super::super::supervisor::CapturedOutput;
use super::super::super::completion::{ProviderExit, print_budget_exhausted, project_completion};
use crate::context::NullRuntimeHost;

struct IdleHost;

impl NullRuntimeHost for IdleHost {}

pub(super) fn project(stalled: bool, timed_out: bool) -> crate::DispatchOutcome {
    project_stdout("fixture", b"", false, stalled, timed_out)
}

pub(super) fn project_stdout(
    provider: &str,
    stdout: &[u8],
    exit_success: bool,
    stalled: bool,
    timed_out: bool,
) -> crate::DispatchOutcome {
    let spec: AgentLoopSpec = serde_json::from_value(json!({})).expect("empty agent spec");
    let host = IdleHost;
    let input = json!({});
    let redaction = PatternRedactor::http_default();
    let audit = crate::activity_job::V2AuditWriter::new(
        "run",
        "fixture",
        std::sync::Arc::new(orbit_agent::loop_engine::audit::NullSink),
    );
    project_completion(ProviderExit {
        audit: &audit,
        started_at: chrono::Utc::now(),
        finished_at: chrono::Utc::now(),
        agent_execution_id: None,
        host: &host,
        spec: &spec,
        input: &input,
        run_id: "run",
        provider: provider.to_string(),
        model: None,
        task_ids: &[],
        worktree_boundary: None,
        sandbox: None,
        subprocess_cwd: None,
        redaction: &redaction,
        timeout_seconds: 300,
        argv_redacted: Vec::new(),
        stdin_blob_ref: "stdin".to_string(),
        stdout_blob_ref: "stdout".to_string(),
        stderr_blob_ref: "stderr".to_string(),
        stdout: CapturedOutput::from_bytes(stdout),
        stderr: CapturedOutput::empty(),
        exit_code: exit_success.then_some(0),
        duration: Duration::from_millis(700),
        timed_out,
        stalled,
        stall_window: Duration::from_secs(30),
        print_timeout: None,
        usage_context: Default::default(),
        build_budget_waits: BuildBudgetWaits::default(),
    })
    .expect("project a provider exit")
}

// Combinatorial budget/exit/envelope logic: exact hour-long boundaries cannot
// be exercised economically by a real provider process. [ORB-15206]
#[test]
fn only_a_clean_exit_without_an_envelope_at_the_injected_budget_is_a_timeout() {
    let mut argv = Vec::new();
    let budget = orbit_agent::apply_antigravity_print_timeout(
        "antigravity",
        &mut argv,
        Duration::from_secs(3600),
    );
    assert_eq!(budget, Some(Duration::from_secs(3570)));
    for (exit_success, missing, budget, elapsed_ms, expected) in [
        (true, true, budget, 3_569_999, false),
        (true, true, budget, 3_570_000, true),
        (true, true, budget, 3_570_001, true),
        (true, false, budget, 3_570_000, false),
        (false, true, budget, 3_570_000, false),
        (true, true, None, 3_570_000, false),
    ] {
        assert_eq!(
            print_budget_exhausted(
                exit_success,
                missing,
                budget,
                Duration::from_millis(elapsed_ms),
            ),
            expected,
            "ORB-15206: exit_success={exit_success}, missing={missing}, budget={budget:?}, elapsed={elapsed_ms}ms",
        );
    }
}

// An accepted envelope used to keep the agent's `provider` and `exit_code`
// and list both as the agent's answer.
#[test]
fn an_agent_result_cannot_replace_orbit_owned_provider_or_exit_code() {
    let envelope = concat!(
        r#"{"schemaVersion":1,"status":"success","result":{"#,
        r#""provider":"spoofed","exit_code":7,"marker":"kept"},"error":null}"#
    );
    let outcome = project_stdout("fixture", envelope.as_bytes(), true, false, false);
    assert!(
        outcome.success,
        "accepted envelope should succeed: {}",
        outcome.message.unwrap_or_default()
    );
    assert_eq!(outcome.output["provider"], json!("fixture"));
    assert_eq!(outcome.output["exit_code"], json!(0));
    assert_eq!(outcome.output["marker"], json!("kept"));
    assert_eq!(outcome.output["response_result_fields"], json!(["marker"]));
}
