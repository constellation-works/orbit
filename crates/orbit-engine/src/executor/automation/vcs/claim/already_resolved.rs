//! The native replay behind a claimed implementer's already-resolved outcome
//! [ORB-15747].
//!
//! An implementer that finds its clean base already resolves the task hands
//! off `NoDiff`, but the tests that show it may run only outside the agent
//! sandbox, and an affected-test gate over an empty diff runs none of them.
//! The implementer names those tests as
//! [`AlreadyResolved`] selectors. Each is admitted through the closed
//! `cargo test` grammar, then run here on the NoDiff base outside any agent
//! sandbox ([`run_host_sandbox_test`]), and must report a passed test. A
//! passing run joins the handoff's validation logs, which the owner rechecks
//! by digest; anything else fails the step with the run's output, and its
//! log is attached to the owner's task.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::text::ceil_char_boundary;
use orbit_types::workflow::handoff::{AlreadyResolved, HandoffCandidate};
use orbit_types::workflow::{
    HostEvidenceReason, HostSandboxCommand, VALIDATION_ENVIRONMENT_MARKER,
};
use serde_json::{Value, json};

use crate::context::{ClaimExecutionContext, RuntimeHost};

use super::super::host_evidence::run_host_sandbox_test;
use super::input::refused;
use super::validation::ClaimedResult;

/// The most output a failure message quotes, from the end of the run.
const MAX_QUOTED_OUTPUT_BYTES: usize = 4 * 1024;

/// The admitted selectors of this attempt's implementer output, handed in as
/// `implementation`, or `None` when it declared no already-resolved outcome.
pub(super) fn declared(input: &Value) -> Result<Option<Vec<HostSandboxCommand>>, OrbitError> {
    let Some(output) = input.get("implementation") else {
        return Ok(None);
    };
    AlreadyResolved::from_output(output)
        .and_then(|declared| declared.map(|declared| declared.admit()).transpose())
        .map_err(refused)
}

/// Run each admitted selector on `candidate`'s base, in order, and return
/// their passing results; the first that is not passing evidence fails.
pub(super) fn replay<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &ClaimExecutionContext,
    workspace: &Path,
    candidate: &HandoffCandidate,
    selectors: &[HostSandboxCommand],
) -> Result<Vec<ClaimedResult>, OrbitError> {
    let mut results = Vec::new();
    for (index, selector) in selectors.iter().enumerate() {
        let run = run_host_sandbox_test(
            host,
            workspace,
            &candidate.base,
            selector,
            &context.run_id,
            &workspace.join("target"),
        )?;
        let host_command = selector.host_command();
        let refusal = match run.judgement {
            Ok(_) => {
                results.push(ClaimedResult::passed(host_command, run.output));
                continue;
            }
            Err(refusal) => refusal,
        };
        let log_path = format!(
            "validation/{}/already-resolved-{index}.failed.json",
            context.claim_id
        );
        let log = json!({
            "schema_version": 1,
            "role": "already_resolved",
            "workspace_id": context.workspace_id,
            "task_id": context.task_id,
            "claim_id": context.claim_id,
            "machine_id": context.machine_id,
            "run_id": context.run_id,
            "tested_head": candidate.base.commit,
            "command": host_command,
            "exit_code": run.exit_code,
            "timed_out": run.timed_out,
            "reason": refusal.reason.as_str(),
            "detail": refusal.detail,
            "output": run.output,
            "validation_env": run.environment,
        });
        // The log is evidence for the owner's reader; failing to deliver it
        // must not hide the failure itself.
        if let Err(error) = serde_json::to_vec(&log)
            .map_err(|error| OrbitError::Execution(error.to_string()))
            .and_then(|bytes| host.attach_claim_validation_log(&log_path, bytes))
        {
            tracing::warn!(
                path = log_path,
                "could not attach an already-resolved replay log: {error}"
            );
        }
        let message = format!(
            "already-resolved replay `{host_command}` on base {} is not passing native \
             evidence ({}): {}. Log: `{log_path}`. Output tail:\n{}",
            candidate.base.commit,
            refusal.reason.as_str(),
            refusal.detail,
            output_tail(&run.output)
        );
        return Err(match refusal.reason {
            // The host's condition, not the base's: nothing was judged.
            HostEvidenceReason::SandboxUnavailable | HostEvidenceReason::ToolMissing => {
                OrbitError::Execution(format!("{VALIDATION_ENVIRONMENT_MARKER} {message}"))
            }
            // A run that proves nothing is a refused declaration.
            HostEvidenceReason::SelfSkipped | HostEvidenceReason::NoTestsRan => refused(message),
            _ => OrbitError::Execution(message),
        });
    }
    Ok(results)
}

/// The end of `output`, where a failing test run reports what failed.
fn output_tail(output: &str) -> &str {
    let output = output.trim_end();
    &output[ceil_char_boundary(output, output.len().saturating_sub(MAX_QUOTED_OUTPUT_BYTES))..]
}
