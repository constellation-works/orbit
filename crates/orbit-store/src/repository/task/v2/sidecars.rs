use super::*;

/// Small scans avoid helper startup; large scans bound simultaneous probes
/// just as envelope freshness does. Bundle reads never leave the caller.
const PARALLEL_BUNDLE_STAMP_MIN_TASKS: usize = 256;
const MAX_BUNDLE_STAMP_THREADS: usize = 16;

impl TaskV2Store {
    pub(crate) fn task_decision_snapshots(
        &self,
    ) -> Result<BTreeMap<String, crate::contracts::TaskDecisionSnapshot>, OrbitError> {
        self.registry.task_decision_snapshots(&self.workspace_id)
    }

    pub(crate) fn record_task_decision_snapshots(
        &self,
        records: &[crate::contracts::TaskDecisionSnapshot],
    ) -> Result<bool, OrbitError> {
        self.registry
            .record_task_decision_snapshots(&self.workspace_id, records)
    }

    pub(crate) fn decision_request_task_id(
        &self,
        request_id: &str,
    ) -> Result<Option<String>, OrbitError> {
        self.registry
            .decision_request_task_id(&self.workspace_id, request_id)
    }

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
        self.settled_history_stamp(id)
    }

    /// [`Self::task_history_stamp`] for each of `ids`, checking the
    /// partition for a commit to recover once rather than per task. A task
    /// is absent when nothing can vouch for its logs, and every task is when
    /// there is no commit boundary or the partition has a commit to recover.
    pub(crate) fn task_history_stamps(
        &self,
        ids: &[String],
    ) -> Result<BTreeMap<String, String>, OrbitError> {
        let mut stamps = BTreeMap::new();
        let Some(boundary) = &self.coordination else {
            return Ok(stamps);
        };
        if !boundary.has_nothing_to_recover()? {
            return Ok(stamps);
        }
        for id in ids {
            orbit_types::task::validate_orb_task_id(id)?;
            if let Some(stamp) = self.settled_history_stamp(id)? {
                stamps.insert(id.clone(), stamp);
            }
        }
        Ok(stamps)
    }

    /// One task's log stamp once the partition is known to be settled.
    fn settled_history_stamp(&self, id: &str) -> Result<Option<String>, OrbitError> {
        let Some([events, comments]) = self.bundle_store.settled_log_paths(id)? else {
            return Ok(None);
        };
        Ok(persisted_file_stamp(&events)
            .zip(persisted_file_stamp(&comments))
            .map(|(events, comments)| format!("{events}/{comments}")))
    }

    /// Filesystem evidence of the bundle a whole-bundle read opens, for each
    /// of `ids`, taken without reading it: whole-bundle stamps over its
    /// directories and logs.
    ///
    /// A store write that changes what a whole-bundle read returns, or
    /// whether it succeeds, changes the stamp: it appends to a log, truncates
    /// one, or renames a new file into a directory. It carries the limits
    /// [`Self::task_history_stamp`] states: taken before a read it labels
    /// that read or something older, and it is evidence that the store
    /// wrote nothing, not proof the bytes are identical: an out-of-band
    /// in-place edit of a renamed-into-place document, manifest or blob,
    /// or a change inside a legacy nested artifact
    /// directory, goes unseen until the bundle's next store write.
    ///
    /// The partition is checked once for a commit to recover; a task is
    /// absent when nothing can vouch for its bundle (a write to settle, a
    /// missing task, a platform without file identity), and every task is
    /// when there is no commit boundary or the partition has a commit to
    /// recover.
    pub(crate) fn task_bundle_stamps(
        &self,
        ids: &[String],
    ) -> Result<BTreeMap<String, String>, OrbitError> {
        let Some(boundary) = &self.coordination else {
            return Ok(BTreeMap::new());
        };
        if !boundary.has_nothing_to_recover()? {
            return Ok(BTreeMap::new());
        }
        let threads = std::thread::available_parallelism()
            .map_or(1, |parallelism| parallelism.get())
            .min(MAX_BUNDLE_STAMP_THREADS);
        if ids.len() < PARALLEL_BUNDLE_STAMP_MIN_TASKS || threads < 2 {
            return self.stamp_settled_task_bundles(ids);
        }
        // Only metadata and non-blocking commit-lease probes run on helpers:
        // no boundary entry, recovery or bundle read with thread-local locks.
        std::thread::scope(|scope| {
            let chunks = ids
                .chunks(ids.len().div_ceil(threads))
                .map(|chunk| {
                    let handle = std::thread::Builder::new()
                        .spawn_scoped(scope, move || self.stamp_settled_task_bundles(chunk))
                        .ok();
                    (chunk, handle)
                })
                .collect::<Vec<_>>();
            // Join every helper before propagating errors. A lost helper
            // supplies no proof, so those tasks get strict bundle reads.
            let batches = chunks
                .into_iter()
                .map(|(chunk, handle)| match handle {
                    Some(handle) => handle.join().unwrap_or_else(|_| Ok(BTreeMap::new())),
                    None => self.stamp_settled_task_bundles(chunk),
                })
                .collect::<Vec<_>>();
            let mut stamps = BTreeMap::new();
            for batch in batches {
                stamps.extend(batch?);
            }
            Ok(stamps)
        })
    }

    /// Stamp a slice after the caller checked partition settledness once.
    fn stamp_settled_task_bundles(
        &self,
        ids: &[String],
    ) -> Result<BTreeMap<String, String>, OrbitError> {
        let mut stamps = BTreeMap::new();
        for id in ids {
            let Some(bundle_dir) = self.bundle_store.settled_bundle_dir(id)? else {
                continue;
            };
            if let Some(stamp) = bundle_stamp::task_bundle_freshness_stamp(&bundle_dir) {
                stamps.insert(id.clone(), stamp);
            }
        }
        Ok(stamps)
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
        .map(|event| {
            TaskHistoryEntry::new(event.at, event.by, event.event_type)
                .with_note(event.note)
                .with_status_change(event.from_status, event.to_status)
                .with_operation_id(event.operation_id)
                .with_decision(event.decision)
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
