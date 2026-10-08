//! Admitted frozen batches close to their admission deadline [ORB-14624].

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::automation::BatchState;

use crate::OrbitRuntime;

/// How close to its deadline a frozen batch must be for its task to sort
/// ahead of same-priority backlog at admission.
pub(crate) const FROZEN_BATCH_EXPIRY_WINDOW_HOURS: i64 = 2;

const CONSUMER_PAGE_LIMIT: usize = 100;

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
    let mut after: Option<String> = None;
    let mut expiring = BTreeMap::new();
    loop {
        let page = store.automation_states_page(&prefix, after.as_deref(), CONSUMER_PAGE_LIMIT)?;
        if page.is_empty() {
            return Ok(expiring);
        }
        for state in page {
            if !state.consumer.starts_with(&prefix)
                || after.as_ref().is_some_and(|key| state.consumer <= *key)
            {
                return Err(OrbitError::Store(
                    "automation state page is outside its prefix or not strictly ordered".into(),
                ));
            }
            after = Some(state.consumer.clone());
            let Some(attempt) = state.active else {
                continue;
            };
            if attempt.state != BatchState::Admitted {
                continue;
            }
            let deadline = attempt.deadline();
            if let Some(task_id) = attempt.action_id
                && deadline <= horizon
            {
                expiring.insert(task_id, deadline);
            }
        }
    }
}
