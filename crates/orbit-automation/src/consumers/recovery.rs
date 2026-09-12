//! Operator recovery for a delivery auto-task consumer stalled by settings or
//! a content-preserving branch rebase.
//!
//! This module resolves the same ownership, epoch and source facts the
//! evaluator uses and hands them to the shared rule, which decides what may be
//! adopted; Store commits the fenced checkpoint with its audit record. There is
//! no second scheduler, evaluator or state writer here.

use crate::automation_error_to_orbit;
use crate::delivery::recovery;
use crate::host::AutomationHost;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::automation::recovery::{RecoveryPreview, RecoveryRequest};
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule};

use super::ownership;
use crate::source::Source;

/// Preview or apply a recovery for one delivery auto-task.
///
/// A request that asks for nothing is a read-only preview; adoption or reissue
/// commits exactly one audited checkpoint. Either way the returned document
/// reports the stalled identity, the retained debt and the frozen obligations.
pub fn recover_auto_task<H: AutomationHost>(
    host: &H,
    definition: &AutoTaskDefinition,
    request: &RecoveryRequest,
    now: DateTime<Utc>,
) -> Result<RecoveryPreview, OrbitError> {
    let AutoTaskSchedule::Deliveries {
        deliveries_landed: declared,
    } = &definition.schedule
    else {
        return Err(OrbitError::InvalidInput("not a delivery definition".into()));
    };

    let ownership = ownership::resolve(host, declared.owner_machine.as_deref());
    let trigger = ownership::with_resolved_owner(declared, &ownership);
    let epoch = ownership::auto_task_epoch(definition, &trigger)?;

    // The repository the debt was observed in is a compatibility fact, so it is
    // read from the configured branch rather than the executor's HEAD.
    let source = Source::new(host.repo_root());
    let (repository, _) = source
        .head(&trigger.branch)
        .map_err(automation_error_to_orbit)?;

    let consumer = super::consumer_key(host, "auto-task", &definition.name)?;
    let by = host.write_label()?;
    let operation = recovery::Recovery {
        consumer: &consumer,
        epoch: &epoch,
        trigger: &trigger,
        repository: &repository,
        host_refusal: ownership.refusal(),
        request,
        by: &by,
        now,
        replay: None,
    };

    let store = host.automation_store()?;
    let replay = if request.replay_history {
        let state = store
            .automation_state(&consumer)?
            .ok_or_else(|| OrbitError::InvalidInput("unknown delivery consumer".into()))?;
        let receipts = store.automation_receipts(&consumer, 100)?;
        let (page, record) = source
            .replay_history(host, &trigger.branch, &state, receipts.len())
            .map_err(automation_error_to_orbit)?;
        Some(recovery::HistoryReplayInput { page, record })
    } else {
        None
    };
    let operation = recovery::Recovery {
        replay,
        ..operation
    };

    if !request.mutates() {
        return recovery::preview(store.as_ref(), &operation).map_err(automation_error_to_orbit);
    }

    host.ensure_coordination_task_write_permitted()?;

    if let Some(replay) = &operation.replay {
        ensure_replay_head(&source, &trigger.branch, &replay.record.captured_head)?;
    }

    recovery::apply(store.as_ref(), &operation).map_err(automation_error_to_orbit)
}

/// Refuse a replay whose branch head moved after the record was captured.
pub fn ensure_replay_head(
    source: &Source<'_>,
    branch: &str,
    captured_head: &orbit_types::workflow::automation::SourceRevision,
) -> Result<(), OrbitError> {
    let (_, current_head) = source.head(branch).map_err(automation_error_to_orbit)?;
    if &current_head != captured_head {
        return Err(OrbitError::InvalidInput(
            orbit_types::workflow::automation::recovery::refusal::HISTORY_HEAD_CHANGED.into(),
        ));
    }

    Ok(())
}
