//! Bounded continuation of an implementer that declared its authorized work
//! unfinished [ORB-21837].
//!
//! An activity that declares `incomplete_continuation` may get a fresh
//! provider invocation in the same worktree when its invocation ended with
//! the typed `implementation_incomplete` signal (see
//! `orbit_agent::IncompleteContinuationRequest`) and nothing else failed.
//! Each continuation re-runs the ordinary per-invocation dispatch: the same
//! crew, tool grant, sandbox, worktree boundary and task authority. Its wall
//! clock is the unspent part of the activity's budget, never more. Orbit
//! commits nothing here, and the step succeeds only when a continuation
//! itself completes; a refused or exhausted continuation returns the latest
//! declared failure with the original one attached, and the worktree keeps
//! the candidate.

use std::sync::Arc;
use std::time::{Duration, Instant};

use orbit_types::workflow::activity_job::{AgentLoopSpec, IncompleteContinuationPolicy};
use serde_json::{Value, json};

use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::dispatcher::{DispatchError, DispatchOutcome};
use super::dispatch::dispatch_cli_invocation;
use super::prepare::{invocation_timeout_seconds, reject_removed_trusted_host_execution};
use super::response_correction;
use crate::context::RuntimeHost;

/// Orbit-owned step output key carrying the typed request the projection
/// read from the invocation's envelope, or null. Written over any
/// same-named agent result field.
pub(super) const REQUEST_OUTPUT_KEY: &str = "incomplete_continuation_request";
/// Step output key recording what continuation did for this dispatch.
const OUTCOME_OUTPUT_KEY: &str = "incomplete_continuation";
/// Input key a continuation invocation receives.
const CONTINUATION_INPUT_KEY: &str = "implementation_continuation";

/// Why a requested continuation did not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContinuationRefusal {
    RoundsExhausted,
    WallClockExhausted,
    TaskTerminal,
    TaskUnavailable,
}

impl ContinuationRefusal {
    fn as_str(self) -> &'static str {
        match self {
            Self::RoundsExhausted => "rounds_exhausted",
            Self::WallClockExhausted => "wall_clock_exhausted",
            Self::TaskTerminal => "task_terminal",
            Self::TaskUnavailable => "task_unavailable",
        }
    }
}

/// Run a CLI agent-loop step, continuing a typed unfinished implementation
/// within the activity's own budget when its spec admits that.
#[allow(
    clippy::too_many_arguments,
    reason = "policy authorizes the activity name while audit and the worktree boundary report the step id"
)]
pub(crate) fn run_cli_backend_for_step(
    host: &dyn RuntimeHost,
    spec: &AgentLoopSpec,
    step_id: &str,
    activity_name: &str,
    run_id: &str,
    audit: Arc<V2AuditWriter>,
    input: &Value,
    fs_profile: Option<&str>,
    wall_clock_bound: Option<u64>,
) -> Result<DispatchOutcome, DispatchError> {
    // Resolve workspace policy once. Per-invocation dispatch must keep the
    // reduced spec a continuation/correction supplies instead of refilling it.
    let resolved_spec = wall_clock_bound
        .or_else(|| host.activity_wall_clock_timeout_seconds(activity_name))
        .map(|seconds| {
            let mut resolved = spec.clone();
            resolved.wall_clock_timeout_seconds = seconds;
            resolved
        });
    let spec = resolved_spec.as_ref().unwrap_or(spec);
    let started = Instant::now();
    let invoke = |spec: &AgentLoopSpec, input: &Value| {
        dispatch_cli_invocation(
            host,
            spec,
            step_id,
            activity_name,
            run_id,
            Arc::clone(&audit),
            input,
            fs_profile,
        )
    };
    // The superseded invocation's usage is still spent by this step; the
    // executor persists only the trace of the outcome it receives.
    let persist_superseded_trace = |outcome: &DispatchOutcome| {
        if let Some(invocation) = outcome.invocation.as_ref()
            && let Err(error) = host.persist_invocation_trace(run_id, step_id, input, invocation)
        {
            audit.note_telemetry_failure("invocation_trace", Some(step_id), &error);
        }
    };
    let mut outcome = invoke(spec, input)?;
    if spec.incomplete_continuation.is_none() && spec.response_correction.is_none() {
        return Ok(outcome);
    }
    // The budget the first invocation was admitted with. Later invocations
    // spend what it left; build-admission waits never refill it.
    reject_removed_trusted_host_execution(spec, activity_name)?;
    let budget = Duration::from_secs(invocation_timeout_seconds(spec));
    if let Some(policy) = spec.response_correction {
        // Asset load keeps this apart from `incomplete_continuation`: a
        // correction's activity is read-only, so it has no work to continue.
        let correction = response_correction::ResponseCorrectionRun {
            spec,
            input,
            run_id,
            step_id,
            policy,
            started,
            budget,
        };
        return correction.correct_rejected_replies(outcome, invoke, persist_superseded_trace);
    }
    let Some(policy) = spec.incomplete_continuation else {
        return Ok(outcome);
    };

    let mut rounds = 0u32;
    let mut original_message: Option<String> = None;
    let mut refusal = None;
    while let Some(remaining_work) = continuation_request(&outcome) {
        let remaining = budget.saturating_sub(started.elapsed());
        if let Some(refused) =
            refuse_continuation(host, run_id, step_id, input, policy, rounds, remaining)
        {
            refusal = Some(refused);
            break;
        }
        rounds += 1;
        if original_message.is_none() {
            original_message = Some(
                outcome
                    .message
                    .clone()
                    .unwrap_or_else(|| "implementation declared incomplete".to_string()),
            );
        }
        persist_superseded_trace(&outcome);
        tracing::info!(
            target: "orbit.engine.cli_runner",
            run_id,
            step_id,
            round = rounds,
            max_rounds = policy.max_rounds,
            remaining_seconds = remaining.as_secs(),
            "continuing an implementation declared incomplete"
        );
        let mut continuation_spec = spec.clone();
        continuation_spec.wall_clock_timeout_seconds = remaining.as_secs().max(1);
        let continuation_input = continuation_input(
            input,
            rounds,
            policy.max_rounds,
            remaining,
            &remaining_work,
            original_message.as_deref(),
        );
        outcome = invoke(&continuation_spec, &continuation_input)?;
    }

    let requested = rounds > 0 || refusal.is_some();
    if requested && let Some(output) = outcome.output.as_object_mut() {
        output.insert(
            OUTCOME_OUTPUT_KEY.to_string(),
            json!({
                "rounds": rounds,
                "max_rounds": policy.max_rounds,
                "refusal": refusal.map(ContinuationRefusal::as_str),
            }),
        );
    }
    if !outcome.success
        && rounds > 0
        && let Some(original) = original_message
    {
        let latest = outcome
            .message
            .take()
            .unwrap_or_else(|| "step completed with success=false".to_string());
        let stop = refusal.map_or_else(String::new, |refusal| {
            format!(" (continuation refused: {})", refusal.as_str())
        });
        outcome.message = Some(format!(
            "{latest}; after {rounds} implementation continuation(s){stop}; original error before continuation: {original}"
        ));
    }
    Ok(outcome)
}

/// The remaining work a failed outcome's typed request names, if any.
fn continuation_request(outcome: &DispatchOutcome) -> Option<Vec<String>> {
    if outcome.success {
        return None;
    }
    outcome
        .output
        .get(REQUEST_OUTPUT_KEY)?
        .get("remaining_work")?
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()
        .filter(|items| !items.is_empty())
}

/// The first bound a requested continuation would cross, if any.
fn refuse_continuation(
    host: &dyn RuntimeHost,
    run_id: &str,
    step_id: &str,
    input: &Value,
    policy: IncompleteContinuationPolicy,
    rounds: u32,
    remaining: Duration,
) -> Option<ContinuationRefusal> {
    if rounds >= policy.max_rounds {
        return Some(ContinuationRefusal::RoundsExhausted);
    }
    if remaining < Duration::from_secs(policy.min_remaining_seconds.max(1)) {
        return Some(ContinuationRefusal::WallClockExhausted);
    }
    // The task's live record, not the prompt's snapshot: an operator may
    // have closed or cancelled it while the first invocation ran.
    match host.task_context_for_agent_input(run_id, step_id, input) {
        Ok(Some(task)) if task.get("terminal").and_then(Value::as_bool) == Some(true) => {
            Some(ContinuationRefusal::TaskTerminal)
        }
        Ok(Some(_)) => None,
        Ok(None) | Err(_) => Some(ContinuationRefusal::TaskUnavailable),
    }
}

fn continuation_input(
    input: &Value,
    round: u32,
    max_rounds: u32,
    remaining: Duration,
    remaining_work: &[String],
    original_message: Option<&str>,
) -> Value {
    let mut input = input.clone();
    if let Some(object) = input.as_object_mut() {
        object.insert(
            CONTINUATION_INPUT_KEY.to_string(),
            json!({
                "round": round,
                "max_rounds": max_rounds,
                "remaining_budget_seconds": remaining.as_secs(),
                "remaining_work": remaining_work,
                "previous_error": original_message,
            }),
        );
    }
    input
}
