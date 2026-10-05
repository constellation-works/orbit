//! Carrying a task's context creation grant through the bundle writes that
//! change its scope.
//!
//! The grant row is appended before the envelope publish, so the pending-write
//! journal commits or aborts it together with the scope it describes.

use orbit_types::task::{CONTEXT_CREATION_AUTHORIZED_EVENT, ContextCreationState};

use super::*;

/// The creation grant `bundle`'s history holds for its current scope.
pub(super) fn creation_state(bundle: &TaskBundleV2) -> ContextCreationState {
    ContextCreationState::resolve(
        &bundle.envelope.id,
        &bundle.envelope.context_files,
        bundle
            .events
            .iter()
            .map(|event| (event.event_type.as_str(), event.note.as_deref())),
    )
}

/// Append the grant row a scope change to `next_context_files` requires, if
/// any, to the open bundle write. Call before the envelope is republished.
pub(super) fn append_creation_grant(
    store: &TaskBundleStoreV2,
    bundle: &mut TaskBundleV2,
    next_context_files: &[String],
    authorize: &[String],
    actor: &str,
) -> Result<(), OrbitError> {
    let Some(grant) =
        creation_state(bundle).next_grant(&bundle.envelope.id, next_context_files, authorize)?
    else {
        return Ok(());
    };
    let event = TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id: next_event_id(&bundle.events),
        at: Utc::now(),
        by: actor.to_string(),
        event_type: CONTEXT_CREATION_AUTHORIZED_EVENT.to_string(),
        note: Some(grant.to_note()),
        from_status: None,
        to_status: None,
    };
    store.append_event(&bundle.envelope.id, &event)?;
    bundle.events.push(event);
    Ok(())
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
