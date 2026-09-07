//! Resolved scheduling constraints handed to the shared state evaluator
//! [ORB-11332].
//!
//! Operation mode never runs a second timer. The operator-enabled state
//! routine remains the single cadence owner; a valid grant only tells the
//! shared evaluator which members it names and how soon they are due. With
//! no admitting grant, or with a preference that keeps preparation manual
//! and recovery existing, the constraints are empty and the routine behaves
//! exactly as before.

use chrono::Utc;
use orbit_automation::members::MemberConstraints;
use orbit_common::OrbitError;
use orbit_config::{PreparationPreference, RecoveryPreference};
use orbit_types::workflow::automation::members::{StateTrigger, StateTriggerKind};

use super::captured_policy;
use crate::OrbitRuntime;

/// The constraints for one state trigger evaluation.
pub(crate) fn member_constraints(
    runtime: &OrbitRuntime,
    trigger: &StateTrigger,
) -> Result<MemberConstraints, OrbitError> {
    let Some(grant) = runtime.active_operation_grant()? else {
        return Ok(MemberConstraints::default());
    };
    if !grant.admission(Utc::now()).admits() {
        return Ok(MemberConstraints::default());
    }
    let policy = captured_policy(&grant)?;
    let due_after_seconds = match trigger.kind {
        StateTriggerKind::PreparationEligible => (grant.rights.prepare
            && policy.preparation.value == PreparationPreference::Automatic)
            .then_some(grant.limits.preparation_due_seconds),
        // A scheduled recovery preference makes in-scope incidents due as
        // soon as the evaluator sees them; the aggregate budget bounds them.
        StateTriggerKind::ExecutionFailed => {
            (policy.recovery.value == RecoveryPreference::Scheduled).then_some(0)
        }
    };
    let Some(due_after_seconds) = due_after_seconds else {
        return Ok(MemberConstraints::default());
    };
    Ok(MemberConstraints {
        scope: grant.task_ids.iter().cloned().collect(),
        due_after_seconds: Some(due_after_seconds),
    })
}
