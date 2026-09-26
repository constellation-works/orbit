//! Operator reset of one delivery auto-task consumer.
//!
//! Core resolves the same ownership, epoch and source facts the evaluator
//! uses, reads the pins the consumer holds, and hands them to the shared rule;
//! Automation decides whether the reset may run and Store destroys the state
//! together with its audit record. There is no second state writer here, and
//! no hand-edited database row.

use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_automation::automation_error_to_orbit;
use orbit_automation::delivery::reset;
use orbit_common::OrbitError;
use orbit_types::workflow::automation::recovery::{ResetPreview, ResetRequest};
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule};
use serde::Serialize;

use super::{ownership, source::Source};

/// Preview or apply a reset for one delivery auto-task.
///
/// A request with no reason is a read-only preview of exactly what would be
/// forgotten. A reason commits one audited checkpoint that drops the consumer's
/// whole position, so the next evaluation re-baselines at the branch head.
pub fn reset_auto_task(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
    request: &ResetRequest,
    now: DateTime<Utc>,
) -> Result<ResetPreview, OrbitError> {
    let AutoTaskSchedule::Deliveries {
        deliveries_landed: declared,
    } = &definition.schedule
    else {
        return Err(OrbitError::InvalidInput("not a delivery definition".into()));
    };

    let ownership = ownership::resolve(runtime, declared.owner_machine.as_deref());
    let trigger = ownership::with_resolved_owner(declared, &ownership);
    let epoch = ownership::auto_task_epoch(definition, &trigger)?;

    // The baseline is read from the configured branch, not the executor's
    // HEAD: it is the position the next evaluation will actually seed at.
    let source = Source::new(&runtime.paths().repo_root);
    let (_, baseline) = source
        .head(&trigger.branch)
        .map_err(automation_error_to_orbit)?;

    let consumer = super::consumer_key(runtime, "auto-task", &definition.name)?;
    let by = runtime.actor().resolve_write_label(None, None)?;
    let store = runtime.automation_store()?;

    if !request.mutates() {
        return reset::preview(
            store.as_ref(),
            &reset::Reset {
                consumer: &consumer,
                epoch: &epoch,
                trigger: &trigger,
                host_refusal: ownership.refusal(),
                request,
                by: &by,
                now,
                baseline,
                released_refs: vec![],
            },
        )
        .map_err(automation_error_to_orbit);
    }

    runtime.ensure_coordination_task_write_permitted()?;

    // The pins are read before the write so the audit record names them, and
    // released after it: a refused reset must leave the frozen inputs of the
    // debt it did not forget reachable.
    let retained = source
        .retained_refs(&consumer)
        .map_err(automation_error_to_orbit)?;

    let applied = reset::apply(
        store.as_ref(),
        &reset::Reset {
            consumer: &consumer,
            epoch: &epoch,
            trigger: &trigger,
            host_refusal: ownership.refusal(),
            request,
            by: &by,
            now,
            baseline,
            released_refs: retained.clone(),
        },
    )
    .map_err(automation_error_to_orbit)?;

    let kept = source.release_refs(&retained);
    if !kept.is_empty() {
        tracing::warn!(
            consumer = consumer,
            refs = kept.join(","),
            "reset could not delete every retained automation ref; they are unreferenced now"
        );
    }

    Ok(applied)
}

/// What deleting a delivery definition did to its consumer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ConsumerTeardown {
    pub consumer: String,
    /// The consumer had recorded state, and an audited reset destroyed it.
    pub reset: bool,
    /// Every pinned `refs/orbit/automation/*` ref the consumer held; all of
    /// them are deleted.
    pub released_refs: Vec<String>,
}

/// Why deleting `definition` must not tear its delivery consumer down yet.
///
/// Delete reuses the audited reset rather than a second state writer, so it
/// inherits exactly the refusals a reset preview reports. A definition with
/// no recorded consumer state has nothing to refuse.
pub(crate) fn consumer_teardown_refusals(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
    force: bool,
    now: DateTime<Utc>,
) -> Result<Vec<String>, OrbitError> {
    let Some(consumer) = delivery_consumer(runtime, definition)? else {
        return Ok(vec![]);
    };
    if runtime
        .automation_store()?
        .automation_state(&consumer)?
        .is_none()
    {
        return Ok(vec![]);
    }
    let request = ResetRequest {
        reason: String::new(),
        force,
    };
    Ok(reset_auto_task(runtime, definition, &request, now)?.refusals)
}

/// Destroy a deleted delivery definition's consumer state through the audited
/// reset, then drop every pinned `refs/orbit/automation/*` ref it still holds,
/// so nothing is left for a consumer that can never evaluate again.
///
/// Returns `None` for a definition without a delivery trigger.
pub(crate) fn tear_down_auto_task_consumer(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
    reason: &str,
    force: bool,
    now: DateTime<Utc>,
) -> Result<Option<ConsumerTeardown>, OrbitError> {
    let Some(consumer) = delivery_consumer(runtime, definition)? else {
        return Ok(None);
    };
    let source = Source::new(&runtime.paths().repo_root);
    let pinned = source
        .retained_refs(&consumer)
        .map_err(automation_error_to_orbit)?;
    let reset = runtime
        .automation_store()?
        .automation_state(&consumer)?
        .is_some();
    if reset {
        let request = ResetRequest {
            reason: reason.to_string(),
            force,
        };
        reset_auto_task(runtime, definition, &request, now)?;
    }

    // Reset releases the pins it knows about; this catches the ones a consumer
    // without state, or a reset that could not delete them, left behind.
    let retained = source
        .retained_refs(&consumer)
        .map_err(automation_error_to_orbit)?;
    let kept = source.release_refs(&retained);
    if !kept.is_empty() {
        return Err(OrbitError::Io(format!(
            "could not delete pinned automation refs for '{consumer}': {}",
            kept.join(", ")
        )));
    }

    Ok(Some(ConsumerTeardown {
        consumer,
        reset,
        released_refs: pinned,
    }))
}

/// The consumer key of a delivery definition. A host without a registered
/// machine identity can never have evaluated one, so it has none.
fn delivery_consumer(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
) -> Result<Option<String>, OrbitError> {
    if !matches!(definition.schedule, AutoTaskSchedule::Deliveries { .. })
        || runtime.automation_machine_identity().is_none()
    {
        return Ok(None);
    }
    super::consumer_key(runtime, "auto-task", &definition.name).map(Some)
}
