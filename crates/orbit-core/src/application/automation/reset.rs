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
