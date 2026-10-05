use std::collections::BTreeSet;

use super::*;
use crate::contracts::{TaskCandidates, TaskListFilter, TaskPage, TaskResidualFilter, TaskRow};
use crate::driver::sqlite::task_registry::is_terminal_status;
use orbit_types::task::satisfy_completed_archived_dependencies;

impl TaskV2Store {
    pub(crate) fn task_candidates(
        &self,
        filter: &TaskListFilter,
        limit: usize,
    ) -> Result<TaskCandidates, OrbitError> {
        self.ensure_recovered()?;
        let filter = filter.normalized();
        if let Some(unsettled) = self.validate_index()? {
            return self.indexed_candidates(&filter, limit, unsettled);
        }
        // The scan validates task fields on every encountered bundle
        // (envelope, bodies, events, event/envelope status) and fails on any
        // error; only the index repair it feeds is gated and best effort.
        // Artifact payload hashing is deferred.
        let envelopes = self
            .scan_and_repair_index("missing or stale index")?
            .into_iter()
            .map(|bundle| bundle.envelope)
            .collect::<Vec<_>>();
        Ok(select_candidates(envelopes, &filter, limit))
    }

    /// Selection over a validated index. SQL answers every predicate it
    /// projects, the ordering, and — when nothing is left for `matches` —
    /// the limit and the total, so only the selected envelopes leave the
    /// cache. Otherwise the index narrows the candidates and the remaining
    /// predicates run over those envelopes before the limit.
    fn indexed_candidates(
        &self,
        filter: &TaskListFilter,
        limit: usize,
        unsettled: Vec<String>,
    ) -> Result<TaskCandidates, OrbitError> {
        if filter.statuses.as_ref().is_some_and(Vec::is_empty) {
            return Ok(TaskCandidates::default());
        }
        let bounded = filter.is_fully_indexed();
        // Non-indexed predicates need the pre-cursor match set so
        // `total_without_cursor` is the untruncated count. Fully indexed
        // pages keep SQL `LIMIT` and COUNT the unbounded total separately
        // when a cursor is present.
        let unbounded = filter.without_cursor();
        let index_source = if bounded { filter } else { &unbounded };
        let selection = self.registry.indexed_task_selection(
            &self.workspace_id,
            &index_source.index_filter(unsettled.clone()),
            filter.terminal_last,
            (bounded && limit < usize::MAX).then_some(limit),
        )?;
        let total_without_cursor = if !bounded {
            0
        } else if filter.scan_before.is_none() {
            selection.total
        } else {
            self.registry
                .indexed_task_selection(
                    &self.workspace_id,
                    &unbounded.index_filter(unsettled),
                    filter.terminal_last,
                    Some(0),
                )?
                .total
        };
        // A row that no longer matches was rewritten after the scan; leaving
        // it out here keeps every returned candidate true to the filter, and
        // hydration re-checks the selected page against the bundle anyway.
        let envelopes = selection
            .ids
            .iter()
            .filter_map(|id| self.envelope_cache.cached(id))
            .filter(|envelope| index_source.matches(envelope))
            .collect::<Vec<_>>();
        if bounded {
            return Ok(TaskCandidates {
                items: envelopes,
                total: selection.total,
                total_without_cursor,
            });
        }
        Ok(select_candidates(envelopes, filter, limit))
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
        let status_by_id = self.listing_status_index(&candidates.items)?;
        let mut selected = candidates.items;
        if let Some(matches) = residual {
            // The residual is decidable from envelope metadata (see
            // [`TaskResidualFilter`]), so selection applies it to every
            // candidate here and only the survivors that fit the page pay for
            // a bundle: a lock, seven file reads and their parses. Each
            // hydrated task is checked again below, which is the authority.
            selected.retain(|candidate| matches(&Self::metadata_task(candidate), &status_by_id));
        }
        let selected_total = selected.len();
        let mut items = Vec::with_capacity(selected_total.min(limit));
        for candidate in selected {
            if items.len() >= limit {
                break;
            }
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
            selected_total
        } else {
            candidates.total
        };
        let total_without_cursor = if residual.is_some() {
            total
        } else {
            candidates.total_without_cursor
        };
        items.truncate(limit);
        Ok(TaskPage {
            items,
            total,
            total_without_cursor,
            status_by_id,
        })
    }

    fn scan_task_page(
        &self,
        filter: &TaskListFilter,
        limit: usize,
        residual: TaskResidualFilter<'_>,
    ) -> Result<TaskPage, OrbitError> {
        let unbounded = filter.without_cursor();
        let mut bundles = self.bundle_store.list_bundles()?;
        bundles.retain(|bundle| unbounded.matches(&bundle.envelope));
        let status_by_id =
            self.listing_status_index(bundles.iter().map(|bundle| &bundle.envelope))?;
        let mut items = Vec::with_capacity(bundles.len());
        for bundle in bundles {
            let row = self.row_from_bundle(bundle)?;
            if residual.is_none_or(|matches| matches(&row.task, &status_by_id)) {
                items.push(row);
            }
        }
        sort_listing(
            &mut items,
            filter.terminal_last,
            |row| &row.task.created_at,
            |row| &row.task.id,
            |row| row.task.status,
        );
        let total_without_cursor = items.len();
        if let Some((at, id)) = &filter.scan_before {
            items.retain(|row| {
                row.task.created_at < *at || (row.task.created_at == *at && row.task.id > *id)
            });
        }
        let total = items.len();
        items.truncate(limit);
        Ok(TaskPage {
            items,
            total,
            total_without_cursor,
            status_by_id,
        })
    }

    /// The dependency-status projection one listing needs: this workspace
    /// plus every relation target the selected envelopes name, resolved
    /// wherever it is registered (see `TaskRegistryStore::task_status_index_for`).
    /// Archived `blocked_by` targets that reached `done` first project as
    /// `done` (see `satisfy_completed_archived_dependencies`).
    /// Taken before hydration so the residual predicate can run row by row.
    fn listing_status_index<'a>(
        &self,
        selected: impl IntoIterator<Item = &'a TaskEnvelopeV2>,
    ) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        let relations = selected
            .into_iter()
            .flat_map(|envelope| envelope.relations.iter())
            .collect::<Vec<_>>();
        let targets = relations
            .iter()
            .map(|relation| relation.target.clone())
            .collect::<BTreeSet<_>>();
        let mut status_by_id = self
            .registry
            .task_status_index_for(&self.workspace_id, &targets)?;
        let dependencies = relations
            .iter()
            .filter(|relation| relation.relation_type == TaskRelationType::BlockedBy)
            .map(|relation| relation.target.clone());
        // A listing must not fail on one unreadable dependency owner; that
        // edge keeps its archived dead end, as it did before this rule.
        let Ok(()) = satisfy_completed_archived_dependencies::<std::convert::Infallible>(
            &mut status_by_id,
            dependencies,
            |id| {
                Ok(self.registered_task_history(id).unwrap_or_else(|error| {
                    orbit_common::tracing::warn!(
                        task_id = id,
                        %error,
                        "archived dependency history unreadable; keeping it a dead end"
                    );
                    None
                }))
            },
        );
        Ok(status_by_id)
    }

    pub(crate) fn get_task_row(
        &self,
        id: &str,
        list_read: bool,
    ) -> Result<Option<TaskRow>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        self.ensure_recovered()?;
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
        let history = super::sidecars::task_history_from_events(std::mem::take(&mut bundle.events));
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

/// Filter, order, count, and bound envelopes in memory: the path for a
/// rebuilt index and for predicates the index does not project.
fn select_candidates(
    envelopes: Vec<TaskEnvelopeV2>,
    filter: &TaskListFilter,
    limit: usize,
) -> TaskCandidates {
    let unbounded = filter.without_cursor();
    let mut items = envelopes
        .into_iter()
        .filter(|task| unbounded.matches(task))
        .collect::<Vec<_>>();
    sort_listing(
        &mut items,
        filter.terminal_last,
        |task| &task.created_at,
        |task| &task.id,
        |task| task.status,
    );
    let total_without_cursor = items.len();
    if filter.scan_before.is_some() {
        items.retain(|task| filter.matches_cursor(task));
    }
    let total = items.len();
    items.truncate(limit);
    TaskCandidates {
        items,
        total,
        total_without_cursor,
    }
}

/// Canonical listing order — newest first, task ID ascending for ties — and,
/// with `terminal_last`, the status-aware partition: tasks in a terminal
/// status move behind the rest while each partition keeps that order.
fn sort_listing<T>(
    items: &mut [T],
    terminal_last: bool,
    created_at: impl Fn(&T) -> &chrono::DateTime<Utc>,
    id: impl Fn(&T) -> &str,
    status: impl Fn(&T) -> TaskStatus,
) {
    sort_by_created_desc_id_asc(items, created_at, id);
    if terminal_last {
        items.sort_by_key(|item| is_terminal_status(status(item)));
    }
}
