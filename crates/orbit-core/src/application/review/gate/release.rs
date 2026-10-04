//! Close review attempts that ended without a verdict, charging only the
//! reviewer runtime they actually spent.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::ReviewReleaseRequest;
use orbit_store::contracts::ReviewRelease;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{ReviewAttempt, ReviewAttemptState, seconds_between};
use serde_json::json;

use super::super::REVIEW_AUDIT;
use crate::OrbitRuntime;

/// Release the attempt a failing or terminating run admitted. An attempt
/// already settled with a verdict, or unknown to its lineage, is left alone.
pub(crate) fn release_review_attempt(
    runtime: &OrbitRuntime,
    request: &ReviewReleaseRequest,
) -> Result<(), OrbitError> {
    let store = runtime.review_store()?;
    let workspace_id = runtime.workspace_id()?;
    let Some(ledger) = store.review_ledger(&workspace_id, &request.lineage_key)? else {
        return Ok(());
    };
    let Some(attempt) = ledger
        .attempts
        .iter()
        .find(|attempt| attempt.attempt_id == request.attempt_id)
    else {
        return Ok(());
    };
    if attempt.released_at.is_none() && attempt.state != ReviewAttemptState::Open {
        return Ok(());
    }
    let now = Utc::now();
    let charge = reviewer_runtime(runtime, attempt, &request.run_id, Segment::Closing, now)?;
    release(
        runtime,
        &workspace_id,
        &request.lineage_key,
        attempt,
        charge,
        now,
    )
}

/// Release an attempt still open under a different run of the lineage, so
/// a new admission never inherits it open. The owner's runtime is charged
/// only up to the moment that run ended.
pub(super) fn release_abandoned(
    runtime: &OrbitRuntime,
    workspace_id: &str,
    lineage_key: &str,
    admitting_run_id: &str,
) -> Result<(), OrbitError> {
    let store = runtime.review_store()?;
    let Some(ledger) = store.review_ledger(workspace_id, lineage_key)? else {
        return Ok(());
    };
    let Some(open) = ledger
        .open_attempt()
        .filter(|attempt| attempt.run_id != admitting_run_id)
    else {
        return Ok(());
    };
    let now = Utc::now();
    let charge = reviewer_runtime(runtime, open, admitting_run_id, Segment::Bound, now)?;
    release(runtime, workspace_id, lineage_key, open, charge, now)
}

fn release(
    runtime: &OrbitRuntime,
    workspace_id: &str,
    lineage_key: &str,
    attempt: &ReviewAttempt,
    charge: u64,
    now: DateTime<Utc>,
) -> Result<(), OrbitError> {
    runtime.review_store()?.review_release(
        workspace_id,
        &ReviewRelease {
            lineage_key,
            attempt_id: &attempt.attempt_id,
            elapsed_seconds: charge,
            now,
        },
    )?;
    runtime.record_pipeline_audit(
        REVIEW_AUDIT,
        Some(&attempt.run_id),
        Some("system"),
        AuditEventStatus::Success,
        json!({
            "phase": "release",
            "attempt_id": attempt.attempt_id,
            "lineage_key": lineage_key,
            "charged_seconds": charge,
            "recorded_at": now.to_rfc3339(),
        }),
        None,
    )?;
    Ok(())
}

/// What `run_id` is to the attempt being charged.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Segment {
    /// The run closing the attempt: its own span is charged too.
    Closing,
    /// A run that never worked the attempt: its start only bounds the span
    /// charged to an owner that recorded no end.
    Bound,
}

/// Reviewer runtime to charge for `attempt` at `now`.
///
/// - Open under `run_id`: wall time since the attempt started.
/// - Open under another run: that run's span up to when it finished (or,
///   when it never recorded an end, up to `run_id`'s start), plus `run_id`'s
///   own span when it is the [`Segment::Closing`] run.
/// - Released earlier: the charge already recorded, plus the closing run's
///   span after the release.
///
/// Time the lineage spent with no run working the attempt is never charged.
pub(super) fn reviewer_runtime(
    runtime: &OrbitRuntime,
    attempt: &ReviewAttempt,
    run_id: &str,
    segment: Segment,
    now: DateTime<Utc>,
) -> Result<u64, OrbitError> {
    let run_started = runtime
        .get_job_run_backend(run_id)?
        .and_then(|run| run.started_at);
    let (charged, resumed_from) = match (&attempt.state, attempt.released_at) {
        (_, Some(released_at)) => (attempt.elapsed_seconds.unwrap_or(0), released_at),
        (ReviewAttemptState::Settled { .. }, None) => return Ok(attempt.elapsed_at(now)),
        (ReviewAttemptState::Open, None) if attempt.run_id == run_id => {
            return Ok(attempt.elapsed_at(now));
        }
        (ReviewAttemptState::Open, None) => {
            let owner_end = runtime
                .get_job_run_backend(&attempt.run_id)?
                .and_then(|run| run.finished_at)
                .or(run_started)
                .unwrap_or(now)
                .min(now);
            (seconds_between(attempt.started_at, owner_end), owner_end)
        }
    };
    if segment == Segment::Bound {
        return Ok(charged);
    }
    let segment_start = run_started
        .map_or(resumed_from, |started| started.max(resumed_from))
        .max(attempt.started_at);
    Ok(charged.saturating_add(seconds_between(segment_start, now)))
}
