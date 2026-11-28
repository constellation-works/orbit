//! Landing attempt bookkeeping and the claim mutations that record it.

use orbit_common::OrbitError;
use orbit_store::contract::{ClaimInspection, ClaimInvocation, ClaimMutation};
use orbit_types::workflow::handoff::LandingAttempt;

use crate::OrbitRuntime;

/// The owner identity recorded for landing decisions in task history. Landing
/// is an owner-operator action; the approval that authorized it carries the
/// approver separately and immutably.
const LANDING_ACTOR: &str = "owner-landing";

/// The latest landing attempt for each handoff, for inspection.
#[cfg(feature = "test-support")]
pub fn landing_attempts(rt: &OrbitRuntime) -> Result<Vec<LandingAttempt>, OrbitError> {
    rt.ensure_coordination_task_write_permitted()?;
    rt.stores().tasks().landing_attempts()
}

/// Inspect an exact attempt, retaining stopped evidence after a retry.
#[cfg(feature = "test-support")]
pub fn landing_attempt_record(
    rt: &OrbitRuntime,
    handoff_id: &str,
    attempt: u32,
) -> Result<Option<LandingAttempt>, OrbitError> {
    rt.ensure_coordination_task_write_permitted()?;
    rt.stores()
        .tasks()
        .landing_attempt_record(handoff_id, attempt)
}

pub(super) fn landing_attempt(
    rt: &OrbitRuntime,
    handoff_id: &str,
) -> Result<Option<LandingAttempt>, OrbitError> {
    rt.stores().tasks().landing_attempt(handoff_id)
}

/// Open the next attempt for this handoff, before any job exists to carry
/// it. A crash between this and the submission leaves the request pending,
/// which the next dispatch pass resubmits under the same key.
pub(super) fn open_landing_attempt(rt: &OrbitRuntime, handoff_id: &str) -> Result<(), OrbitError> {
    let seen = landing_attempt(rt, handoff_id)?.map_or(0, |attempt| attempt.attempt);
    mutate_landing_claim(
        rt,
        handoff_id,
        &format!("landing-open:{handoff_id}:{seen}"),
        None,
    )
}

pub(super) fn attach_landing_job(
    rt: &OrbitRuntime,
    handoff_id: &str,
    run_id: &str,
) -> Result<(), OrbitError> {
    mutate_landing_claim(
        rt,
        handoff_id,
        &format!("landing-attach:{handoff_id}:{run_id}"),
        Some(run_id.to_string()),
    )
}

fn mutate_landing_claim(
    rt: &OrbitRuntime,
    handoff_id: &str,
    mutation_id: &str,
    job_run_id: Option<String>,
) -> Result<(), OrbitError> {
    let (claim, _) = rt.landing_claim(handoff_id)?;
    rt.mutate_execution_claim(
        Some(&landing_invocation(rt, &claim)),
        mutation_id,
        &ClaimMutation::DispatchLanding {
            handoff_id: handoff_id.to_string(),
            job_run_id,
        },
    )?;
    Ok(())
}

/// Landing runs as the owner operator. The capability that permits it is the
/// durable authorization recorded at approval, which the store rechecks —
/// including its revocation state — inside every landing transaction.
pub(super) fn landing_invocation(_rt: &OrbitRuntime, claim: &ClaimInspection) -> ClaimInvocation {
    ClaimInvocation::trusted_operator(
        claim.claim.task_id.clone(),
        claim.claim.claim_id.clone(),
        LANDING_ACTOR.to_string(),
    )
}

pub(super) fn missing_attempt(handoff_id: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "no landing attempt is recorded for handoff '{handoff_id}'"
    ))
}
