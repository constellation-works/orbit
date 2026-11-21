use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{RoutineFireRecord, RoutineFireState, RoutineStoreBackend};

use super::super::loader::LoadedRoutine;
use super::report::{duration_from_minutes, parse_rfc3339};

/// A failed fire that still has retry budget, ignoring backoff.
///
/// Retryable means `Failed` — a run-level failure *or* a synchronous dispatch
/// failure: `fire` records a `submit_pipeline_run` that returns `Err` as
/// `Failed` (not `Error`) precisely because nothing dispatched, so it is
/// unambiguously safe to re-dispatch under the same slot. `Error` is reserved
/// for the ambiguous case — a crashed sweep's stale intent reclaimed by the
/// outcome sync, where a worker may have partially started — and stays
/// terminal so a make-up fire never races an orphaned run.
///
/// Skipped rows are not failures: a newer skip must not hide this row, and
/// must not be written while it still exists. A skipped row on this slot
/// occupies an attempt number but does not spend `retries.max`.
pub(super) fn retryable_failure(
    store: &dyn RoutineStoreBackend,
    routine: &LoadedRoutine,
) -> Result<Option<RoutineFireRecord>, OrbitError> {
    let retries = routine.definition.policy.retries;
    if retries.max == 0 {
        return Ok(None);
    }
    let Some(latest) = store.routine_latest_non_skip_fire(&routine.definition.name)? else {
        return Ok(None);
    };
    if latest.state != RoutineFireState::Failed {
        return Ok(None);
    }
    // Real tries on this slot. Without skipped gaps this equals
    // `latest.attempt`: max=2 allows the initial fire plus two re-dispatches.
    let non_skipped = store
        .routine_slot_attempts(&latest.routine_name, &latest.slot)?
        .map(|attempts| attempts.non_skipped)
        .unwrap_or(latest.attempt);
    if non_skipped > retries.max {
        return Ok(None);
    }
    Ok(Some(latest))
}

/// Attempt number for the next re-dispatch of `failed`.
///
/// One past the highest row already stored on that slot, so a drain-skipped
/// retry is not claimed again. The skipped row keeps its number; the
/// re-dispatch uses the next free one.
pub(super) fn next_retry_attempt(
    store: &dyn RoutineStoreBackend,
    failed: &RoutineFireRecord,
) -> Result<u32, OrbitError> {
    let highest = store
        .routine_slot_attempts(&failed.routine_name, &failed.slot)?
        .map(|attempts| attempts.highest)
        .unwrap_or(failed.attempt);
    highest.max(failed.attempt).checked_add(1).ok_or_else(|| {
        OrbitError::Store(format!(
            "routine '{}' slot '{}' has no free retry attempt",
            failed.routine_name, failed.slot
        ))
    })
}

/// The most recent failed fire whose retry backoff has elapsed.
pub(super) fn retry_candidate(
    store: &dyn RoutineStoreBackend,
    routine: &LoadedRoutine,
    now_utc: DateTime<Utc>,
) -> Result<Option<RoutineFireRecord>, OrbitError> {
    let Some(latest) = retryable_failure(store, routine)? else {
        return Ok(None);
    };
    let failed_at = parse_rfc3339(&latest.updated_at)?;
    if now_utc.signed_duration_since(failed_at)
        < duration_from_minutes(
            routine.definition.policy.retries.backoff_minutes,
            "policy.retries.backoff_minutes",
        )?
    {
        return Ok(None);
    }
    Ok(Some(latest))
}
