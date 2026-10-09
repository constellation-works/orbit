//! The server-issued claim a pilot run carries.

use crate::OrbitRuntime;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::automation::members::*;
use serde_json::Value;

/// Recheck the server-issued claim at the deterministic prepare/apply boundary.
///
/// This proves only that the claim is still the consumer's live attempt. Whether
/// the branch moved under a member's material since the claim froze its source
/// is [`stale_tasks`]' per-task answer, which supersedes those members instead
/// of failing the run [ORB-14476].
pub(crate) fn claim(
    runtime: &OrbitRuntime,
    value: &Value,
) -> Result<Option<MemberAttempt>, OrbitError> {
    let Some(claim) = value
        .get("state_automation")
        .filter(|claim| !claim.is_null())
    else {
        return Ok(None);
    };

    let submitted: MemberAttempt = serde_json::from_value(claim.clone())
        .map_err(|e| OrbitError::InvalidInput(e.to_string()))?;

    let active = runtime
        .automation_store()?
        .automation_state(&submitted.consumer)?
        .and_then(|state| state.members)
        .and_then(|members| members.active)
        .ok_or_else(|| OrbitError::InvalidInput("state claim missing".into()))?;

    if active.kind != submitted.kind
        || active.id != submitted.id
        || active.member != submitted.member
        || active.members() != submitted.members()
        || active.action_key != submitted.action_key
        || active.attempt != submitted.attempt
        || active.exhausted
        || Utc::now() >= active.deadline
    {
        return Err(OrbitError::InvalidInput(
            "state claim stale or expired".into(),
        ));
    }

    Ok(Some(active))
}
