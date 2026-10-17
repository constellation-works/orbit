//! [ORB-16373] Late delivery steps that outlast a long hold of the owner's
//! task-commit lock.
//!
//! `claim_validate`, `pr_complete` and the base sync's `git_rebase` run after
//! the candidate is complete. A hold of the owner's task-commit lock past one
//! acquisition deadline used to fail them outright, and the finished
//! delivery was requeued or lost to final recovery. Each now runs in a
//! lock-busy retry scope: an owner acquisition that lost to a held lock,
//! local or routed from a claimed leaf, is made again after a growing pause
//! until the step has waited [`OWNER_LOCK_RETRY`]'s cap.

use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::fs::lock_retry::{
    LockBusyRetryPolicy, LockBusyRetryRecord, with_lock_busy_retry,
};
use orbit_types::workflow::TRANSIENT_FAILURE_MARKER;
use serde_json::{Value, json};

/// Each lost attempt has already waited out the owner's own 30 s
/// acquisition deadline, so five minutes covers several holds that a
/// saturated disk stretches past it without leaving a step stalled behind a
/// hung holder for long.
const OWNER_LOCK_RETRY: LockBusyRetryPolicy = LockBusyRetryPolicy {
    cap: Duration::from_secs(5 * 60),
    first_pause: Duration::from_secs(1),
    longest_pause: Duration::from_secs(15),
};

/// The step output field that records a retried owner acquisition.
const OWNER_LOCK_WAIT_FIELD: &str = "owner_lock_wait";

/// Run a late delivery step's action with owner lock-busy retry.
///
/// A step that succeeded after retrying records the retries and the time
/// they waited in its output. A step that still lost to a held lock fails as
/// a `transient` failure, whose message keeps the lock error and so the
/// holder it names.
pub(super) fn retrying_owner_lock(
    action: impl FnOnce() -> Result<Value, OrbitError>,
) -> Result<Value, OrbitError> {
    let (result, record) = with_lock_busy_retry(OWNER_LOCK_RETRY, action);
    match result {
        Ok(mut output) => {
            if record.retries > 0
                && let Some(object) = output.as_object_mut()
            {
                object.insert(OWNER_LOCK_WAIT_FIELD.into(), wait_record(&record));
            }
            Ok(output)
        }
        // Typed outcomes the dispatcher hands on as they are.
        Err(
            error @ (OrbitError::RecoverableVcsConflict(_)
            | OrbitError::CandidateAbsorbed(_)
            | OrbitError::TaskCompletionLiveRun { .. }),
        ) => Err(error),
        Err(error) if error.is_lock_busy() || record.exhausted => {
            let cause = error.to_string();
            let cause = cause.trim_start_matches(TRANSIENT_FAILURE_MARKER).trim();
            Err(OrbitError::Execution(format!(
                "{TRANSIENT_FAILURE_MARKER} the owner's task-commit lock stayed held after {} \
                 retries waited {} ms of this step's {} ms cap: {cause}",
                record.retries,
                record.waited.as_millis(),
                record.cap.as_millis(),
            )))
        }
        Err(error) => Err(error),
    }
}

fn wait_record(record: &LockBusyRetryRecord) -> Value {
    json!({
        "retries": record.retries,
        "waited_ms": u64::try_from(record.waited.as_millis()).unwrap_or(u64::MAX),
        "cap_ms": u64::try_from(record.cap.as_millis()).unwrap_or(u64::MAX),
    })
}
