use super::*;

impl TaskV2Store {
    /// A task's comments for a reader outside any boundary section.
    ///
    /// When the partition has nothing to recover, only the comment log is
    /// read, under the task's shared bundle lock: no commit lock, no lock
    /// holder record, no other bundle file. A reader that finds an
    /// interrupted commit or an incomplete write falls back to
    /// [`Self::get_task_comments`] inside the boundary, which settles the
    /// partition first. A codebase scan reads every task's history and the
    /// comments of many, so this is the read it pays per task (ORB-16938).
    pub(crate) fn read_task_comments(
        &self,
        id: &str,
    ) -> Result<Option<Vec<TaskComment>>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        let _span = comments_read_span(id);
        match self.read_sidecar_outside_boundary(id, TaskBundleStoreV2::read_comments_if_settled)? {
            Some(rows) => {
                Ok(rows.map(|rows| rows.into_iter().map(task_comment_from_row).collect()))
            }
            None => self.in_boundary(|| self.get_task_comments(id)),
        }
    }

    /// [`Self::read_task_comments`] for the task's history.
    pub(crate) fn read_task_history(
        &self,
        id: &str,
    ) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        let _span = history_read_span(id);
        match self.read_sidecar_outside_boundary(id, TaskBundleStoreV2::read_events_if_settled)? {
            Some(rows) => Ok(rows.map(task_history_from_events)),
            None => self.in_boundary(|| self.settled_task_history(id)),
        }
    }

    /// One sidecar read without entering the boundary: `Some` with the rows,
    /// or `Some(None)` for a missing task, when the partition and the bundle
    /// are settled; `None` when the caller must settle them first. A legacy
    /// composition has no journal to consult and keeps its locked read.
    fn read_sidecar_outside_boundary<T>(
        &self,
        id: &str,
        read: SettledSidecarRead<T>,
    ) -> Result<Option<Option<Vec<T>>>, OrbitError> {
        let Some(boundary) = &self.coordination else {
            return Ok(None);
        };
        if !boundary.has_nothing_to_recover()? {
            return Ok(None);
        }
        match read(&self.bundle_store, id) {
            Ok(Some(rows)) => Ok(Some(Some(rows))),
            Ok(None) => Ok(None),
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Task,
                ..
            }) => Ok(Some(None)),
            Err(err) => Err(err),
        }
    }

    pub(crate) fn get_task_comments(
        &self,
        id: &str,
    ) -> Result<Option<Vec<TaskComment>>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        self.ensure_recovered()?;
        match self.bundle_store.read_bundle_lightweight(id) {
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
        let _span = history_read_span(id);
        self.settled_task_history(id)
    }

    /// Filesystem evidence of a task's event and comment logs, taken without
    /// reading them: each log's byte length, modification time, device and
    /// inode, as the registry's envelope stamps record a `task.yaml`.
    ///
    /// The logs only grow by appends, a rollback truncates them and a bundle
    /// rewrite renames new files over them, so any write that changes what
    /// [`Self::read_task_history`] or [`Self::read_task_comments`] would
    /// return changes the stamp. Taken before a read, it labels what that
    /// read saw or something older, never newer: a racing write costs the
    /// caller one more read later. `None` when no stamp can vouch for the
    /// logs: the partition or the bundle has a write to settle, the task is
    /// missing, there is no commit boundary, or the platform reports no file
    /// identity.
    pub(crate) fn task_history_stamp(&self, id: &str) -> Result<Option<String>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        let Some(boundary) = &self.coordination else {
            return Ok(None);
        };
        if !boundary.has_nothing_to_recover()? {
            return Ok(None);
        }
        let Some([events, comments]) = self.bundle_store.settled_log_paths(id)? else {
            return Ok(None);
        };
        Ok(persisted_file_stamp(&events)
            .zip(persisted_file_stamp(&comments))
            .map(|(events, comments)| format!("{events}/{comments}")))
    }

    /// The history from the whole bundle, once the partition has settled.
    fn settled_task_history(&self, id: &str) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError> {
        self.ensure_recovered()?;
        match self.bundle_store.read_bundle_lightweight(id) {
            Ok(bundle) => Ok(Some(task_history_from_events(bundle.events))),
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Task,
                ..
            }) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// One sidecar log read on its own, `None` when it must be read with the
/// bundle instead.
type SettledSidecarRead<T> = fn(&TaskBundleStoreV2, &str) -> Result<Option<Vec<T>>, OrbitError>;

/// One span per history read, whichever path serves it: what a selection's
/// history cost is counted in.
fn history_read_span(id: &str) -> orbit_common::tracing::span::EnteredSpan {
    orbit_common::tracing::trace_span!(
        target: "orbit.store.task_query",
        "task_history_read",
        task_id = %id,
    )
    .entered()
}

/// [`history_read_span`] for a comment read.
fn comments_read_span(id: &str) -> orbit_common::tracing::span::EnteredSpan {
    orbit_common::tracing::trace_span!(
        target: "orbit.store.task_query",
        "task_comments_read",
        task_id = %id,
    )
    .entered()
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
