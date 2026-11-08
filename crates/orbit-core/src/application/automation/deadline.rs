//! Admitted frozen batches close to their admission deadline [ORB-14624].

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::automation::BatchState;

use super::consumer_states;
use crate::OrbitRuntime;
use std::ops::ControlFlow;

/// How close to its deadline a frozen batch must be for its task to sort
/// ahead of same-priority backlog at admission.
pub(crate) const FROZEN_BATCH_EXPIRY_WINDOW_HOURS: i64 = 2;

/// Task ID → admission deadline for every task this workspace's delivery
/// automation minted over a frozen batch that reaches its deadline within
/// [`FROZEN_BATCH_EXPIRY_WINDOW_HOURS`] of `now`, or already passed it.
///
/// The deadline is [`orbit_types::workflow::automation::BatchAttempt::deadline`]:
/// the batch's `retry_until`, or an operator reissue's. A host without an
/// automation identity has no consumers and returns an empty map. Pages are
/// read to exhaustion, like the stall scan.
pub(crate) fn expiring_frozen_batch_tasks(
    runtime: &OrbitRuntime,
    now: DateTime<Utc>,
) -> Result<BTreeMap<String, DateTime<Utc>>, OrbitError> {
    let Some(machine) = runtime.automation_machine_identity() else {
        return Ok(BTreeMap::new());
    };
    let prefix = format!("{machine}/{}/", runtime.workspace_id()?);
    let horizon = now + Duration::hours(FROZEN_BATCH_EXPIRY_WINDOW_HOURS);

    let store = runtime.automation_store()?;
    let mut expiring = BTreeMap::new();
    let _: ControlFlow<std::convert::Infallible> =
        consumer_states::for_each_workspace_consumer_state(
            &prefix,
            |after, limit| store.automation_states_page(&prefix, after, limit),
            |state| &state.consumer,
            |state| {
                if let Some(attempt) = state.active
                    && attempt.state == BatchState::Admitted
                {
                    let deadline = attempt.deadline();
                    if let Some(task_id) = attempt.action_id
                        && deadline <= horizon
                    {
                        expiring.insert(task_id, deadline);
                    }
                }
                Ok(ControlFlow::Continue(()))
            },
        )?;
    Ok(expiring)
}
