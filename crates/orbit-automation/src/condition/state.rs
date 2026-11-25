//! Condition-state and fire identity rules applied in the checkpoint transaction.

use orbit_common::OrbitError;
use orbit_store::Tx;
use orbit_types::workflow::automation::{ConditionAudit, ConditionState};

/// Check the first condition checkpoint and insert it if absent.
pub fn initialize_state(tx: &Tx<'_>, state: &ConditionState) -> Result<bool, OrbitError> {
    if state.generation != 0 {
        return Err(OrbitError::InvalidInput(
            "a condition consumer starts at generation 0".into(),
        ));
    }
    state
        .validate()
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;

    tx.condition().insert_state(state)
}

fn validate_step(previous: &ConditionState, next: &ConditionState) -> Result<(), OrbitError> {
    if previous.consumer != next.consumer {
        return Err(OrbitError::InvalidInput(
            "a condition commit cannot change its consumer".into(),
        ));
    }
    if previous.generation.checked_add(1) != Some(next.generation) {
        return Err(OrbitError::InvalidInput(
            "a condition commit advances exactly one generation".into(),
        ));
    }
    next.validate()
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    // A fire's identity, captured subject and action key never change once
    // recorded; only its acknowledgement may move.
    for before in &previous.fires {
        if let Some(after) = next.fire(&before.fire_id)
            && (after.subject != before.subject
                || after.revision != before.revision
                || after.arm_sequence != before.arm_sequence
                || after.epoch != before.epoch
                || after.depth != before.depth
                || after.captured != before.captured
                || after.action_key != before.action_key
                || (before.action_id.is_some() && after.action_id != before.action_id))
        {
            return Err(OrbitError::InvalidInput(format!(
                "condition fire {} cannot change its recorded identity or action",
                before.fire_id
            )));
        }
    }
    Ok(())
}

/// Advance one condition generation, preserving fire identities.
pub fn commit_state(
    tx: &Tx<'_>,
    previous: &ConditionState,
    next: &ConditionState,
) -> Result<bool, OrbitError> {
    validate_step(previous, next)?;
    tx.condition().replace_state(previous, next)
}

/// Advance the condition checkpoint together with its operator audit.
pub fn commit_audited_state(
    tx: &Tx<'_>,
    previous: &ConditionState,
    next: &ConditionState,
    audit: &ConditionAudit,
) -> Result<bool, OrbitError> {
    validate_step(previous, next)?;
    if audit.consumer != previous.consumer {
        return Err(OrbitError::InvalidInput(
            "a condition audit names the consumer it changes".into(),
        ));
    }

    let condition = tx.condition();
    if !condition.replace_state(previous, next)? {
        return Ok(false);
    }
    condition.insert_audit(audit)?;
    Ok(true)
}
