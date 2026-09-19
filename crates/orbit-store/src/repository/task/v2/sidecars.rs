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
                    .map(|comment| TaskComment {
                        at: comment.at,
                        by: comment.by,
                        message: comment.body,
                    })
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
        match self.bundle_store.read_bundle(id) {
            Ok(bundle) => Ok(Some(
                bundle
                    .events
                    .into_iter()
                    .map(|event| TaskHistoryEntry {
                        at: event.at,
                        by: event.by,
                        event: event.event_type,
                        note: event.note,
                        from_status: event.from_status,
                        to_status: event.to_status,
                    })
                    .collect(),
            )),
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Task,
                ..
            }) => Ok(None),
            Err(err) => Err(err),
        }
    }
}
