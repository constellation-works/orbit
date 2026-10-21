use std::collections::BTreeMap;

use chrono::Utc;
use orbit_engine::DispatchError;
use orbit_types::workflow::{
    DrainAdmissionPass, DrainPassObservation, DrainPassOutcome, RunStateUpdate,
};
use serde_json::{Value, json};

use super::super::drains::drain_run_id;
use super::pool::{DEFAULT_POLL_SLEEP_SECONDS, templated_u64};
use crate::OrbitRuntime;
use crate::application::admission::pass_error::PassError;

/// Consecutive admission passes a drain defers on a busy store lock before it
/// fails with that cause. A deferred pass has already waited out one lock
/// acquisition deadline (30 s) and then waits the drain's poll interval
/// (`poll_sleep_seconds`, 30 s by default) before the next one, so ten in a
/// row are five to ten minutes in which no pass could read or write the task
/// store: long enough to outlast a burst of admission sections, short enough
/// that a store that stays locked still ends the drain visibly.
const TRANSIENT_PASS_FAILURE_LIMIT: u32 = 10;

/// The stable code a deferred pass records as its `last_pass_error_code`.
const LOCK_BUSY_PASS_CODE: &str = orbit_common::LOCK_BUSY_ERROR_CODE;

/// Defer a pass that lost only to a busy lock instead of failing the drain
/// [ORB-16376].
///
/// A drain is a long-lived supervisor: one admission section held past a
/// waiter's 30 s deadline used to fail this step, which failed the drain's
/// loop and ended its window while its children kept running, so nothing
/// was admitted until an operator restarted it. Nothing was decided under
/// the lock, so the pass is skipped instead: it admits nothing, records the
/// lock error (which names the holder) as the drain's last pass with
/// `last_pass_error_code` `lock_busy`, and returns an idle answer that waits
/// the drain's poll interval before the next pass retries. The waiting
/// backlog the previous pass recorded is kept, with the time it was read.
///
/// The count of consecutive deferred passes lives on that record, and any
/// pass that completes resets it. At [`TRANSIENT_PASS_FAILURE_LIMIT`] the
/// pass fails as before, naming the lock and the count, so a store that
/// stays locked still ends the drain. A pass outside a drain, or one whose
/// deferral cannot be recorded, has no count to bound it and fails at once.
pub(super) fn defer_lock_busy_pass(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    error: PassError,
) -> Result<Value, DispatchError> {
    let Some(run_id) = drain_run_id(input) else {
        return Err(error.into());
    };
    let backoff_seconds = templated_u64(
        action,
        input,
        "poll_sleep_seconds",
        DEFAULT_POLL_SLEEP_SECONDS,
    )?;
    let reason = error.reason();
    let mut failures = None;
    let recorded = runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            let pass = deferred_pass(state.pull_drain.drain_last_pass.as_ref(), &reason);
            failures = Some(pass.consecutive_pass_failures);
            state.pull_drain.count_admission_pass(
                &DrainPassObservation {
                    outcome: DrainPassOutcome::Failed,
                    ..DrainPassObservation::default()
                },
                pass.recorded_at,
            );
            state.pull_drain.record_admission_pass(pass);
            Ok(())
        });
    let failures = match (recorded, failures) {
        (Ok(RunStateUpdate::Updated), Some(failures)) => failures,
        (Ok(_), _) => return Err(error.into()),
        (Err(record_error), _) => {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id,
                error = %record_error,
                "drain could not record a deferred admission pass; the pass fails"
            );
            return Err(error.into());
        }
    };
    if failures >= TRANSIENT_PASS_FAILURE_LIMIT {
        tracing::error!(
            target: "orbit.core.job_run",
            run_id,
            consecutive_pass_failures = failures,
            error = %reason,
            "drain admission failed on a busy task-store lock for too many consecutive passes"
        );
        return Err(DispatchError::action_failed(
            action,
            format!("{reason} ({failures} consecutive admission passes failed on a busy lock)"),
        ));
    }
    tracing::warn!(
        target: "orbit.core.job_run",
        run_id,
        consecutive_pass_failures = failures,
        limit = TRANSIENT_PASS_FAILURE_LIMIT,
        backoff_seconds,
        error = %reason,
        "drain admission pass deferred on a busy task-store lock; the next pass retries"
    );
    Ok(json!({
        "loose_task_ids": [],
        "loose_task_dispatches": [],
        "has_leaves": false,
        "idle": true,
        "sleep_seconds": backoff_seconds,
        "pass_deferred": {
            "code": LOCK_BUSY_PASS_CODE,
            "reason": reason,
            "consecutive_pass_failures": failures,
            "limit": TRANSIENT_PASS_FAILURE_LIMIT,
        },
    }))
}

/// The record of a deferred pass: the previous pass's view of the backlog,
/// read when that pass ran, with this pass's failure and its count.
fn deferred_pass(previous: Option<&DrainAdmissionPass>, reason: &str) -> DrainAdmissionPass {
    let now = Utc::now();
    let consecutive_pass_failures = previous
        .map_or(0, |pass| pass.consecutive_pass_failures)
        .saturating_add(1);
    let mut pass = previous.cloned().unwrap_or_else(|| DrainAdmissionPass {
        recorded_at: now,
        capacity: None,
        queued: 0,
        deferred: Vec::new(),
        deferred_total: 0,
        excluded: Vec::new(),
        excluded_total: 0,
        waiting_recorded_at: None,
        waiting_by_reason: BTreeMap::new(),
        consecutive_idle_passes: 0,
        resource_throttle: None,
        task_pilot_warning: None,
        admission_refusal: None,
        last_pass_error_code: None,
        last_pass_error: None,
        consecutive_pass_failures: 0,
        degraded: false,
        admission_forecast: None,
    });
    if previous.is_some() {
        pass.waiting_recorded_at = pass.waiting_recorded_at.or(Some(pass.recorded_at));
    }
    pass.recorded_at = now;
    pass.last_pass_error_code = Some(LOCK_BUSY_PASS_CODE.to_string());
    pass.last_pass_error = Some(reason.to_string());
    pass.consecutive_pass_failures = consecutive_pass_failures;
    pass
}
