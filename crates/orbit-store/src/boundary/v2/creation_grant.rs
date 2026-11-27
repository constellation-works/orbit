//! Carrying a task's context creation grant through task bundle revisions.
//!
//! The grant row is appended before the envelope publish, so the pending-write
//! journal commits or aborts it together with the scope it describes.

use chrono::{DateTime, Utc};
use orbit_types::task::{CONTEXT_CREATION_AUTHORIZED_EVENT, ContextCreationState};

use super::*;

/// The creation grant `bundle`'s history holds for its current scope.
pub(super) fn creation_state(bundle: &TaskBundleV2) -> ContextCreationState {
    ContextCreationState::resolve(
        &bundle.envelope.id,
        &bundle.envelope.context_files,
        bundle.envelope.updated_at,
        bundle.envelope.context_creation_sealed_at,
        bundle
            .events
            .iter()
            .map(|event| (event.event_type.as_str(), event.note.as_deref())),
    )
}

/// Maintain the task's context creation grant across an envelope revision at
/// `updated_at`, returning the `context_creation_sealed_at` value to publish
/// on the envelope.
///
/// Only emits a [`CONTEXT_CREATION_AUTHORIZED_EVENT`] row into
/// `pending_events` (and `bundle.events`) when the grant itself changes
/// (initial authorization, scope or selector change, or revocation of a void
/// grant). An unchanged current grant appends no history row and carries its
/// revision binding forward via `Some(updated_at)`.
pub(super) fn append_creation_grant(
    bundle: &mut TaskBundleV2,
    next_context_files: &[String],
    authorize: &[String],
    actor: &str,
    updated_at: DateTime<Utc>,
    pending_events: &mut Vec<TaskEventRowV2>,
) -> Result<Option<DateTime<Utc>>, OrbitError> {
    let state = creation_state(bundle);
    let Some(mut grant) = state.next_grant(
        &bundle.envelope.id,
        next_context_files,
        authorize,
        updated_at,
    )?
    else {
        return Ok(match state {
            ContextCreationState::Current(_) => Some(updated_at),
            ContextCreationState::Absent | ContextCreationState::Void => None,
        });
    };
    let event_id = next_event_id(&bundle.events);
    if grant.generation.is_none() {
        grant.generation = Some(event_id.clone());
    }
    let event = TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id,
        at: Utc::now(),
        by: actor.to_string(),
        event_type: CONTEXT_CREATION_AUTHORIZED_EVENT.to_string(),
        note: Some(grant.to_note()),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
        surface: None,
        caller: None,
    };
    pending_events.push(event.clone());
    bundle.events.push(event);
    Ok(Some(updated_at))
}

/// Refuse a caller-supplied history entry that would forge a grant: only the
/// scope writes above record one.
pub(super) fn reject_forged_grant(entries: &[TaskHistoryEntry]) -> Result<(), OrbitError> {
    if entries
        .iter()
        .any(|entry| entry.event == CONTEXT_CREATION_AUTHORIZED_EVENT)
    {
        return Err(OrbitError::InvalidInput(format!(
            "history event `{CONTEXT_CREATION_AUTHORIZED_EVENT}` is recorded only by a context_files write"
        )));
    }
    Ok(())
}
