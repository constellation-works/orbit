use super::*;
use crate::contracts::{TaskCandidates, TaskListFilter, TaskPage, TaskResidualFilter, TaskRow};

impl TaskV2Store {
    pub(crate) fn task_candidates(
        &self,
        filter: &TaskListFilter,
        limit: usize,
    ) -> Result<TaskCandidates, OrbitError> {
        let envelopes = match self.validated_envelopes()? {
            Some(envelopes) => envelopes,
            None => {
                // Rebuild/fallback deliberately validates every encountered bundle.
                // Never swallow a content error while repairing a generated index.
                let envelopes = self
                    .bundle_store
                    .list_bundles()?
                    .into_iter()
                    .map(|bundle| bundle.envelope)
                    .collect::<Vec<_>>();
                if let Err(error) = self
                    .registry
                    .replace_workspace_task_indexes(&self.workspace_id, &envelopes)
                {
                    orbit_common::tracing::warn!(%error, "task index repair failed; using bundle scan metadata");
                }
                envelopes
            }
        };
        let filter = filter.normalized();
        let mut items = envelopes
            .into_iter()
            .filter(|task| filter.matches(task))
            .collect::<Vec<_>>();
        sort_by_created_desc_id_asc(&mut items, |task| &task.created_at, |task| &task.id);
        let total = items.len();
        items.truncate(limit);
        Ok(TaskCandidates { items, total })
    }

    pub(crate) fn query_task_rows(
        &self,
        filter: &TaskListFilter,
        limit: usize,
        residual: TaskResidualFilter<'_>,
    ) -> Result<TaskPage, OrbitError> {
        let filter = filter.normalized();
        let candidates = self.task_candidates(
            &filter,
            if residual.is_some() {
                usize::MAX
            } else {
                limit
            },
        )?;
        let status_by_id = self.task_status_index()?;
        let mut items = Vec::with_capacity(candidates.items.len());
        for candidate in candidates.items {
            let Some(bundle) = self.bundle_store.read_bundle_if_settled(&candidate.id)? else {
                continue;
            };
            if bundle.envelope != candidate {
                // An update raced selection. One strict scan re-evaluates filters
                // before the limit; no retry loop or stale selected row escapes.
                return self.scan_task_page(&filter, limit, residual);
            }
            let row = self.row_from_bundle(bundle)?;
            if residual.is_none_or(|matches| matches(&row.task, &status_by_id)) {
                items.push(row);
            }
        }
        let total = if residual.is_some() {
            items.len()
        } else {
            candidates.total
        };
        items.truncate(limit);
        Ok(TaskPage {
            items,
            total,
            status_by_id,
        })
    }

    fn scan_task_page(
        &self,
        filter: &TaskListFilter,
        limit: usize,
        residual: TaskResidualFilter<'_>,
    ) -> Result<TaskPage, OrbitError> {
        let bundles = self.bundle_store.list_bundles()?;
        let status_by_id = self.task_status_index()?;
        let mut items = Vec::new();
        for bundle in bundles {
            if filter.matches(&bundle.envelope) {
                let row = self.row_from_bundle(bundle)?;
                if residual.is_none_or(|matches| matches(&row.task, &status_by_id)) {
                    items.push(row);
                }
            }
        }
        sort_by_created_desc_id_asc(&mut items, |row| &row.task.created_at, |row| &row.task.id);
        let total = items.len();
        items.truncate(limit);
        Ok(TaskPage {
            items,
            total,
            status_by_id,
        })
    }

    pub(crate) fn get_task_row(
        &self,
        id: &str,
        list_read: bool,
    ) -> Result<Option<TaskRow>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        let bundle = if list_read {
            self.bundle_store.read_bundle_if_settled(id)?
        } else {
            match self.bundle_store.read_bundle(id) {
                Ok(bundle) => Some(bundle),
                Err(OrbitError::NotFound {
                    kind: NotFoundKind::Task,
                    ..
                }) => None,
                Err(error) => return Err(error),
            }
        };
        bundle
            .map(|bundle| self.row_from_bundle(bundle))
            .transpose()
    }

    fn row_from_bundle(&self, mut bundle: TaskBundleV2) -> Result<TaskRow, OrbitError> {
        let comments = std::mem::take(&mut bundle.comments)
            .into_iter()
            .map(|comment| TaskComment {
                at: comment.at,
                by: comment.by,
                message: comment.body,
            })
            .collect();
        let history = std::mem::take(&mut bundle.events)
            .into_iter()
            .map(|event| TaskHistoryEntry {
                at: event.at,
                by: event.by,
                event: event.event_type,
                note: event.note,
                from_status: event.from_status,
                to_status: event.to_status,
            })
            .collect();
        let mut artifacts = bundle
            .artifact_manifest
            .take()
            .map(|manifest| manifest.files)
            .unwrap_or_default();
        artifacts.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(TaskRow {
            task: self.task_from_bundle(bundle)?,
            comments,
            history,
            artifacts,
        })
    }
}
