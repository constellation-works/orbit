//! What a pass records in its drain's run state, and the refusals it reads
//! back: the failure breaker, the host load forecast, protocol skew and the
//! settlements the owner refused.

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_store::contracts::PullDestination;
use orbit_types::workflow::{PullAdmissionFit, PullBreaker};

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
    ) {
        Ok(_) => DispatchError::ProtocolSkew(message),
        Err(error) => DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("pull drain could not record pass health: {error}"),
        },
    }
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
