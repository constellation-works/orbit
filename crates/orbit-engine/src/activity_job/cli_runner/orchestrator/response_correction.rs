//! Bounded correction of rejected provider output, either for
//! saying `success` and carrying an `error` object [ORB-22426] or for ending a
//! clean turn in prose with no envelope [ORB-22427], or for Claude's exact
//! structured-output retry exhaustion terminal [ORB-22663].
//!
//! The parser stays the authority: a correction hands the rejected reply back
//! to a fresh invocation, and the step succeeds only if that invocation's own
//! reply parses. Each correction re-runs the ordinary per-invocation dispatch,
//! so it has the same crew, tool grant and sandbox. Its wall clock is the
//! unspent part of the activity's budget, never more. Asset load admits the
//! policy only on a read-only activity, so a repeated invocation cannot repeat
//! a mutation.

use std::time::{Duration, Instant};

use orbit_types::workflow::activity_job::{AgentLoopSpec, ResponseCorrectionPolicy};
use serde_json::{Value, json};

use super::super::super::dispatcher::{DispatchError, DispatchOutcome};

/// Orbit-owned step output key carrying the rejected reply from the projection
/// to the loop below, or null. It never leaves the dispatch.
pub(super) const REQUEST_OUTPUT_KEY: &str = "response_correction_request";
/// Step output key recording what correction did for this dispatch.
const OUTCOME_OUTPUT_KEY: &str = "response_correction";
/// Input key a correction invocation receives.
const CORRECTION_INPUT_KEY: &str = "response_correction";

/// `rejection` of a reply that says `success` and carries `error`.
pub(super) const CONTRADICTORY_REPLY: &str = "contradictory_reply";
/// `rejection` of a clean turn that ended without an envelope.
pub(super) const MISSING_ENVELOPE: &str = "missing_envelope";
/// `rejection` of Claude's provider-owned structured-output retry exhaustion.
pub(super) const STRUCTURED_OUTPUT_EXHAUSTED: &str = "structured_output_retry_exhausted";

/// Why a requested correction did not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CorrectionRefusal {
    RoundsExhausted,
    WallClockExhausted,
}

impl CorrectionRefusal {
    fn as_str(self) -> &'static str {
        match self {
            Self::RoundsExhausted => "rounds_exhausted",
            Self::WallClockExhausted => "wall_clock_exhausted",
        }
    }
}

/// The first rejected reply's diagnostics, which the step keeps whatever
/// happens next.
struct OriginalRejection {
    message: Option<String>,
    stdout_blob_ref: Value,
}

/// One step dispatch whose activity declares `response_correction`.
pub(super) struct ResponseCorrectionRun<'a> {
    pub(super) spec: &'a AgentLoopSpec,
    pub(super) input: &'a Value,
    pub(super) run_id: &'a str,
    pub(super) step_id: &'a str,
    pub(super) policy: ResponseCorrectionPolicy,
    pub(super) started: Instant,
    pub(super) budget: Duration,
}

impl ResponseCorrectionRun<'_> {
    /// Correct `first` while the projection recognized its reply as correctable
    /// and the bounds admit another invocation; any other outcome is returned
    /// as is.
    pub(super) fn correct_rejected_replies(
        &self,
        first: DispatchOutcome,
        invoke: impl Fn(&AgentLoopSpec, &Value) -> Result<DispatchOutcome, DispatchError>,
        persist_superseded_trace: impl Fn(&DispatchOutcome),
    ) -> Result<DispatchOutcome, DispatchError> {
        let mut outcome = first;
        let mut rounds = 0u32;
        let mut original: Option<OriginalRejection> = None;
        let mut refusal = None;
        while let Some(request) = correction_request(&outcome) {
            let original = original.get_or_insert_with(|| OriginalRejection {
                message: outcome.message.clone(),
                stdout_blob_ref: outcome
                    .output
                    .get("stdout_blob_ref")
                    .cloned()
                    .unwrap_or(Value::Null),
            });
            let remaining = self.budget.saturating_sub(self.started.elapsed());
            if let Some(refused) = self.refuse_correction(rounds, remaining) {
                refusal = Some(refused);
                break;
            }
            rounds += 1;
            persist_superseded_trace(&outcome);
            tracing::info!(
                target: "orbit.engine.cli_runner",
                run_id = self.run_id,
                step_id = self.step_id,
                round = rounds,
                max_rounds = self.policy.max_rounds,
                remaining_seconds = remaining.as_secs(),
                "correcting rejected provider output"
            );
            let mut correction_spec = self.spec.clone();
            correction_spec.wall_clock_timeout_seconds = remaining.as_secs().max(1);
            let correction_input =
                self.correction_input(rounds, remaining, &request, original.message.as_deref());
            outcome = invoke(&correction_spec, &correction_input)?;
        }

        if let Some(output) = outcome.output.as_object_mut() {
            output.remove(REQUEST_OUTPUT_KEY);
        }
        let Some(original) = original else {
            return Ok(outcome);
        };
        if let Some(output) = outcome.output.as_object_mut() {
            output.insert(
                OUTCOME_OUTPUT_KEY.to_string(),
                json!({
                    "rounds": rounds,
                    "max_rounds": self.policy.max_rounds,
                    "refusal": refusal.map(CorrectionRefusal::as_str),
                    "original_error": original.message,
                    "original_stdout_blob_ref": original.stdout_blob_ref,
                }),
            );
        }
        if !outcome.success
            && rounds > 0
            && let Some(original_message) = original.message
        {
            let latest = outcome
                .message
                .take()
                .unwrap_or_else(|| "step completed with success=false".to_string());
            let stop = refusal.map_or_else(String::new, |refusal| {
                format!(" (correction refused: {})", refusal.as_str())
            });
            outcome.message = Some(format!(
                "{latest}; after {rounds} response correction(s){stop}; original error before correction: {original_message}"
            ));
        }
        Ok(outcome)
    }

    /// The first bound a requested correction would cross, if any.
    fn refuse_correction(&self, rounds: u32, remaining: Duration) -> Option<CorrectionRefusal> {
        if rounds >= self.policy.max_rounds {
            return Some(CorrectionRefusal::RoundsExhausted);
        }
        if remaining < Duration::from_secs(self.policy.min_remaining_seconds.max(1)) {
            return Some(CorrectionRefusal::WallClockExhausted);
        }
        None
    }

    fn correction_input(
        &self,
        round: u32,
        remaining: Duration,
        request: &Value,
        original_message: Option<&str>,
    ) -> Value {
        let mut input = self.input.clone();
        if let Some(object) = input.as_object_mut() {
            object.insert(
                CORRECTION_INPUT_KEY.to_string(),
                json!({
                    "round": round,
                    "max_rounds": self.policy.max_rounds,
                    "remaining_budget_seconds": remaining.as_secs(),
                    "rejection": request.get("rejection"),
                    "rejected_reply": request.get("rejected_reply"),
                    "rejected_reply_truncated": request.get("rejected_reply_truncated"),
                    "parser_error": original_message,
                }),
            );
        }
        input
    }
}

/// The rejected reply a failed outcome's typed request carries, if any.
fn correction_request(outcome: &DispatchOutcome) -> Option<Value> {
    if outcome.success {
        return None;
    }
    outcome
        .output
        .get(REQUEST_OUTPUT_KEY)
        .filter(|request| request.is_object())
        .cloned()
}
