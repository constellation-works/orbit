//! What a pass records in its drain's run state, and the refusals it reads
//! back: the failure breaker, the host load forecast, protocol skew and the
//! settlements the owner refused.

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_store::contracts::PullDestination;
use orbit_types::workflow::{
    DrainPassObservation, DrainPassOutcome, PullAdmissionFit, PullBreaker, ResourceThrottle,
};

use crate::OrbitRuntime;
use crate::application::distributed::{OwnerAnswer, RefusedPullSettlement};
use crate::application::pull_drain::drain::{BreakerGate, PullDrain};

/// Preserve the cause before the engine terminalizes the drain. Its outbox
/// remains available to leaf workers and later settlement-only passes.
pub(super) fn protocol_skew_failure(
    runtime: &OrbitRuntime,
    action: &str,
    run_id: &str,
    throttle: Option<orbit_types::workflow::ResourceThrottle>,
    message: String,
) -> DispatchError {
    match runtime.record_pull_pass(
        run_id,
        throttle,
        None,
        Some(&format!("protocol_skew: {message}")),
        Some("protocol_skew"),
        OwnerAnswer::None,
        None,
    ) {
        Ok(_) => DispatchError::ProtocolSkew(message),
        Err(error) => DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("pull drain could not record pass health: {error}"),
        },
    }
}

/// What decided a pull pass's outcome, for its drain's pass counters. Only a
/// pass whose window was open is counted: a settle-only pass admits nothing
/// by design.
pub(super) struct PullPass<'a> {
    pub(super) admitted: bool,
    /// The pass recorded an error.
    pub(super) failed: bool,
    /// The drain was already degraded when the pass began.
    pub(super) degraded: bool,
    /// Why the pass requested no new claim, as it records it.
    pub(super) refusal: Option<&'a str>,
    pub(super) throttle: Option<&'a ResourceThrottle>,
    /// The refill stopped because every slot under the ceiling was taken.
    pub(super) ceiling_full: bool,
    /// The owner answered the pass's last request idle.
    pub(super) owner_idle: bool,
}

/// The refusal code of a pass the load forecast held.
const FORECAST_NO_HEADROOM: &str = "forecast_no_headroom";

impl PullPass<'_> {
    /// The pass's one outcome, in [`DrainPassOutcome`]'s precedence. The
    /// owner's idle answer adds its waiting reasons when the pass is recorded.
    ///
    /// The forecast is judged before a slot is allocated, so a pass with a
    /// full ceiling and no forecast room counts as `gap_fit_held`, and one
    /// whose ceiling is full never asks the owner, so it cannot tell whether
    /// the backlog had a candidate.
    pub(super) fn observe(&self) -> DrainPassObservation {
        let mut observation = DrainPassObservation::default();
        let refusal = self.refusal.map(refusal_code);
        observation.outcome = if self.admitted {
            DrainPassOutcome::Admitted
        } else if self.failed {
            DrainPassOutcome::Failed
        } else if self.degraded {
            observation.held_reason = Some("pass_failures".to_string());
            DrainPassOutcome::Held
        } else if let Some(code) = refusal.clone().filter(|code| code != FORECAST_NO_HEADROOM) {
            observation.held_reason = Some(code);
            DrainPassOutcome::Held
        } else if let Some(throttle) = self.throttle {
            observation.throttled = throttle.kinds();
            DrainPassOutcome::Throttle
        } else if refusal.is_some() {
            DrainPassOutcome::GapFitHeld
        } else if self.ceiling_full {
            DrainPassOutcome::CeilingFull
        } else if self.owner_idle {
            DrainPassOutcome::NoCandidate
        } else {
            // Nothing else ends a refill early: an operator stop or cancel
            // recorded while the pass was requesting did.
            observation.held_reason = Some("admissions_stopped".to_string());
            DrainPassOutcome::Held
        };
        observation
    }
}

/// The code a refusal leads with (`circuit_open: ...`), or `owner_refused`
/// for an owner's free-form answer.
fn refusal_code(refusal: &str) -> String {
    refusal
        .split_once(':')
        .map(|(code, _)| code.trim())
        .filter(|code| {
            code.starts_with(|c: char| c.is_ascii_lowercase())
                && code
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
        .unwrap_or("owner_refused")
        .to_string()
}

/// Record the drain's failure breaker as this pass left it; `None` closes it
/// [ORB-15639].
pub(super) fn record_breaker(
    runtime: &OrbitRuntime,
    run_id: &str,
    breaker: Option<&PullBreaker>,
) -> Result<(), OrbitError> {
    runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            state.pull_drain.pull_breaker = breaker.cloned();
            Ok(())
        })
        .map(|_| ())
}

/// Record what the drain's host load forecast last had room for; `None`
/// clears it once nothing is forecast.
pub(super) fn record_admission_fit(
    runtime: &OrbitRuntime,
    run_id: &str,
    fit: Option<&PullAdmissionFit>,
) -> Result<(), OrbitError> {
    runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            state.pull_drain.pull_admission_fit = fit.cloned();
            Ok(())
        })
        .map(|_| ())
}

/// Why the failure breaker holds new requests, if it does: open until it
/// half-opens, or half-open while `waits_on` claims this drain carries have
/// not settled.
pub(super) fn breaker_refusal(
    breaker: Option<&PullBreaker>,
    gate: BreakerGate,
    waits_on: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    let breaker = breaker?;
    match gate {
        BreakerGate::Closed => None,
        BreakerGate::Open => Some(format!(
            "circuit_open: the failure breaker is {}; inspect the failed claims and fix the \
             cause meanwhile, or start a new drain to reset it",
            breaker.describe(now)
        )),
        BreakerGate::HalfOpen => (waits_on > 0).then(|| {
            format!(
                "circuit_half_open: the failure breaker is {}; {waits_on} claim(s) this drain \
                 carries have not settled yet",
                breaker.describe(now)
            )
        }),
    }
}

/// The settlements for `destination` its owner refused while still holding
/// their claims [ORB-13979].
pub(super) fn refused_settlements(
    drain: &PullDrain<'_>,
    destination: &PullDestination,
) -> Result<Vec<RefusedPullSettlement>, OrbitError> {
    Ok(drain
        .jobs
        .unsettled_local_pull_admissions()?
        .iter()
        .filter(|record| record.destination == *destination)
        .filter_map(RefusedPullSettlement::of)
        .collect())
}
