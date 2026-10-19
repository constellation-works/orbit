//! The server-issued claim a pilot run carries.

use crate::OrbitRuntime;
use crate::runtime::engine::crew::BundleCrews;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::JobRun;
use orbit_types::workflow::automation::members::*;
use serde_json::Value;

/// The job a preparation attempt admits; only its runs may mix crews.
const PILOT_JOB: &str = "task_pilot_pipeline";

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

/// Whether `run`, starting with `input`, may name tasks whose crews differ.
///
/// Only the live task-pilot preparation attempt's own admitted run may:
/// prepare partitions it by crew before any pilot runs [ORB-16839]. A
/// `state_automation` marker proves nothing by itself, since any caller can
/// shape run input [ORB-16860]. The claim must still be the consumer's live,
/// unexpired preparation attempt, the run must be of the pilot job, and the
/// attempt's action key must be bound to this very run. A missing, malformed,
/// stale or expired claim, another trigger kind, or a live claim carried by a
/// different run or job keeps the one-bundle-one-crew refusal. Any failure to
/// prove the claim fails closed to [`BundleCrews::Unanimous`]; prepare
/// reports a bad claim on its own.
pub(crate) fn bundle_crews(runtime: &OrbitRuntime, run: &JobRun, input: &Value) -> BundleCrews {
    if run.job_id != PILOT_JOB {
        return BundleCrews::Unanimous;
    }
    let Ok(Some(active)) = claim(runtime, input) else {
        return BundleCrews::Unanimous;
    };
    let bound = runtime
        .stores()
        .jobs()
        .automation_job_for_key(&active.action_key)
        .ok()
        .flatten();
    if active.kind == StateTriggerKind::PreparationEligible
        && bound.as_deref() == Some(run.run_id.as_str())
    {
        BundleCrews::PreparationPartitions
    } else {
        BundleCrews::Unanimous
    }
}
