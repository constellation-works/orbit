//! Close review attempts that ended without a verdict, and record the
//! reviewer runtime they are charged.
//!
//! The lineage is charged reviewer process runtime only: the engine reports
//! each reviewer invocation's start and end, and a release or settlement
//! charges what those reports add up to. Retry backoff, recovery activities,
//! the gate's own steps and time no reviewer ran are never charged. An
//! invocation that never reported its end — its process died with the run —
//! is charged up to the run's end, never past its own wall-clock bound.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::{ReviewReleaseRequest, ReviewerInvocationRequest};
use orbit_store::contracts::{ReviewInvocationRecord, ReviewRelease};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{ReviewAttempt, ReviewAttemptState, ReviewLedger};
use serde_json::json;

use super::super::REVIEW_AUDIT;
use crate::OrbitRuntime;

/// Release the attempt a failing run admitted. An attempt already settled
/// with a verdict, or unknown to its lineage, is left alone.
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
    release(runtime, &workspace_id, &ledger, attempt, now, now)
}

impl OrbitRuntime {
    /// Release every review attempt a terminating run still holds, so no
    /// run ends with an attempt open — whether it failed, was cancelled or
    /// was found dead — and a fresh lineage never leaves the old one open.
    /// Best effort: run finalization never fails on review evidence.
    pub(crate) fn best_effort_release_run_review_attempts(
        &self,
        run_id: &str,
        finished_at: DateTime<Utc>,
    ) {
        let released = self.review_store().and_then(|store| {
            let workspace_id = self.workspace_id()?;
            store.review_release_run(&workspace_id, run_id, finished_at)
        });
        match released {
            Ok(ledgers) => {
                for ledger in ledgers {
                    audit_release(self, run_id, &ledger, finished_at);
                }
            }
            Err(error) => tracing::warn!(
                run_id = %run_id,
                error = %error,
                "terminating run could not release its review attempts; the next admission \
                 of the lineage releases them"
            ),
        }
    }
}

/// Release an attempt still held by a different run of the lineage, so a
/// new admission never inherits it open. A reviewer that run left running
/// is charged up to when the run ended.
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
    let Some(attempt) = ledger.attempts.iter().find(|attempt| {
        attempt
            .holder_run_id()
            .is_some_and(|holder| holder != admitting_run_id)
    }) else {
        return Ok(());
    };
    let now = Utc::now();
    let holder_end = match attempt.holder_run_id() {
        Some(holder) => runtime
            .get_job_run_backend(holder)?
            .and_then(|run| run.finished_at),
        None => None,
    };
    let bound = holder_end.unwrap_or(now).min(now);
    release(runtime, workspace_id, &ledger, attempt, bound, now)
}

/// Record a reviewer invocation starting or finishing for its attempt.
pub(crate) fn record_reviewer_invocation(
    runtime: &OrbitRuntime,
    request: &ReviewerInvocationRequest,
) -> Result<(), OrbitError> {
    runtime.review_store()?.review_record_invocation(
        &runtime.workspace_id()?,
        &ReviewInvocationRecord {
            lineage_key: &request.lineage_key,
            attempt_id: &request.attempt_id,
            run_id: &request.run_id,
            event: request.event,
            now: Utc::now(),
        },
    )?;
    Ok(())
}

fn release(
    runtime: &OrbitRuntime,
    workspace_id: &str,
    ledger: &ReviewLedger,
    attempt: &ReviewAttempt,
    bound: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), OrbitError> {
    let released = runtime.review_store()?.review_release(
        workspace_id,
        &ReviewRelease {
            lineage_key: &ledger.lineage_key,
            attempt_id: &attempt.attempt_id,
            bound,
            now,
        },
    )?;
    audit_release(runtime, &attempt.run_id, &released, now);
    Ok(())
}

fn audit_release(runtime: &OrbitRuntime, run_id: &str, ledger: &ReviewLedger, now: DateTime<Utc>) {
    let released = ledger
        .attempts
        .iter()
        .filter(|attempt| attempt.released_at == Some(now))
        .map(|attempt| json!({ "attempt_id": attempt.attempt_id, "charged_seconds": attempt.elapsed_seconds }))
        .collect::<Vec<_>>();
    if let Err(error) = runtime.record_pipeline_audit(
        REVIEW_AUDIT,
        Some(run_id),
        Some("system"),
        AuditEventStatus::Success,
        json!({
            "phase": "release",
            "lineage_key": ledger.lineage_key,
            "released": released,
            "recorded_at": now.to_rfc3339(),
        }),
        None,
    ) {
        tracing::warn!(run_id = %run_id, error = %error, "review release audit failed");
    }
}
