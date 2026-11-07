//! The server-issued claim a pilot run carries.

use crate::OrbitRuntime;
use crate::runtime::engine::crew::BundleCrews;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::workflow::JobRun;
use orbit_types::workflow::automation::members::*;
use serde_json::Value;

/// The job a preparation attempt admits; only its runs may mix crews.
const PILOT_JOB: &str = "task_pilot_pipeline";
/// The ship dispatcher turns selected task ids into singleton gate runs.
const AUTO_DISPATCH_JOB: &str = "task_auto_pipeline";
/// The drain that may dispatch a pilot for repartitioning [ORB-21839].
const DRAIN_JOB: &str = "workspace_auto_pipeline";

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

/// Refuse a deterministic pilot or escalation-triage
/// step whose state claim belongs to another run.
///
/// The step's input carries the claim at its root or under `prepared`. A step
/// without a claim passes; one with a live claim passes only when
/// `owner_run_id`, the run whose reservation the step acts for, is the run the
/// claim's action is bound to.
pub(crate) fn require_claim_owner(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    owner_run_id: Option<&str>,
) -> Result<(), DispatchError> {
    let claim_input = input.get("prepared").unwrap_or(input);
    let active = claim(runtime, claim_input)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    if let Some(active) = active
        && (owner_run_id.is_none() || owner_run_id != active.action_id.as_deref())
    {
        return Err(DispatchError::action_failed(
            action,
            "state claim belongs to another run",
        ));
    }
    Ok(())
}

/// Whether `run`, starting with `input`, may name tasks whose crews differ.
///
/// The task-auto pipeline is a trusted singleton dispatcher: its backlog
/// action emits one task per gate bundle. A task-pilot preparation run may mix
/// only when its live server-issued claim is bound to that exact run; prepare
/// then partitions its tasks by crew before any pilot runs [ORB-16839]. A
/// `state_automation` marker proves nothing by itself, since any caller can
/// shape run input [ORB-16860]. A missing, malformed, stale or expired claim,
/// another trigger kind, or a live claim carried by a different run or job
/// keeps the one-bundle-one-crew refusal. Any failure to prove the claim fails
/// closed to [`BundleCrews::Unanimous`]; prepare reports a bad claim on its
/// own. A pilot a live `--approve-proposed` drain dispatched may also mix,
/// under [`verified_drain_pilot`], when crews changed after selection
/// [ORB-21839].
pub(crate) fn bundle_crews(runtime: &OrbitRuntime, run: &JobRun, input: &Value) -> BundleCrews {
    if run.job_id == AUTO_DISPATCH_JOB {
        return BundleCrews::AutoDispatchSingletons;
    }
    if run.job_id != PILOT_JOB {
        return BundleCrews::Unanimous;
    }
    if verified_drain_pilot(runtime, run, input) {
        return BundleCrews::DrainPartitions;
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

/// Whether `run` is a pilot a live `--approve-proposed` drain dispatched.
///
/// The `drain_promotion` value in run input is caller-shaped and proves
/// nothing by itself [ORB-21839]: the named run must be a live auto drain
/// started with `approve_proposed`, and its recorded child dispatches must
/// name this exact pilot run. A missing, forged, stale (ended drain) or
/// undispatched record fails closed to `Unanimous`. When this passes and
/// the tasks' live crews have diverged since selection, prepare
/// re-partitions by live crew before any pilot worker runs, so no task is
/// assessed under a crew an operator has since replaced and no stale
/// assessment is promoted: the pilot assessment is made fresh in this run.
fn verified_drain_pilot(runtime: &OrbitRuntime, run: &JobRun, input: &Value) -> bool {
    if input.get("promotion_authorized").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    let Some(drain_id) = input
        .get("drain_promotion")
        .and_then(|record| record.get("run_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return false;
    };
    let Ok(Some(drain)) = runtime.stores().jobs().get_job_run(drain_id) else {
        return false;
    };
    if drain.job_id != DRAIN_JOB || drain.state.is_terminal() {
        return false;
    }
    if drain
        .input
        .as_ref()
        .and_then(|input| input.get("approve_proposed"))
        .and_then(Value::as_bool)
        != Some(true)
    {
        return false;
    }
    runtime
        .stores()
        .jobs()
        .read_run_state(drain_id)
        .ok()
        .flatten()
        .is_some_and(|state| {
            state
                .child_dispatches
                .iter()
                .any(|child| child.child_run_id == run.run_id && child.job_name == PILOT_JOB)
        })
}
