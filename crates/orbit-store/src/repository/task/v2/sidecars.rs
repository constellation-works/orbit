use super::*;

impl TaskV2Store {
    pub(crate) fn get_task_comments(
        &self,
        id: &str,
    ) -> Result<Option<Vec<TaskComment>>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        self.ensure_recovered()?;
        match self.bundle_store.read_bundle(id) {
            Ok(bundle) => Ok(Some(
                bundle
                    .comments
                    .into_iter()
                    .map(task_comment_from_row)
                    .collect(),
            )),
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Task,
                ..
            }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub(crate) fn get_task_history(
        &self,
        id: &str,
    ) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        self.ensure_recovered()?;
        let _span = orbit_common::tracing::trace_span!(
            target: "orbit.store.task_query",
            "task_history_read",
            task_id = %id,
        )
        .entered();
        match self.bundle_store.read_bundle(id) {
            Ok(bundle) => Ok(Some(task_history_from_events(bundle.events))),
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Task,
                ..
            }) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// Project a bundle's event rows as the task history read surface.
pub(crate) fn task_history_from_events(events: Vec<TaskEventRowV2>) -> Vec<TaskHistoryEntry> {
    events
        .into_iter()
        .map(|event| TaskHistoryEntry {
            at: event.at,
            by: event.by,
            event: event.event_type,
            note: event.note,
            from_status: event.from_status,
            to_status: event.to_status,
            operation_id: event.operation_id,
            decision: event.decision,
        })
        .collect()
}

/// Project a bundle's comment row as the task comment read surface.
pub(crate) fn task_comment_from_row(comment: TaskCommentRowV2) -> TaskComment {
    TaskComment {
        at: comment.at,
        by: comment.by,
        message: comment.body,
        operation_id: comment.operation_id,
    }
}
