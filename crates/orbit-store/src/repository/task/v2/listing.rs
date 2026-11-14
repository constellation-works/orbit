use std::collections::BTreeSet;

use super::index::IndexUsable;
use super::*;
use crate::contracts::{
    TaskCandidateKey, TaskCandidateKeys, TaskCandidates, TaskListFilter, TaskListHydration,
    TaskPage, TaskResidualFilter, TaskRow,
};
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

    /// Informational read: never enter coordination, settle pending writes,
    /// repair the index or persist freshness proofs. Index rows that disagree
    /// with an envelope no writer holds are read from their bundles and merged
    /// with the rest of the index ([`Self::indexed_candidates_with_stale`]);
    /// an incomplete index falls back to the same strict bundle scan, without
    /// publishing a repair.
    pub(crate) fn observe_task_candidates(
        &self,
        filter: &TaskListFilter,
        limit: usize,
    ) -> Result<TaskCandidates, OrbitError> {
        let filter = filter.normalized();
        if let Some(usable) = self.observe_index_tolerating_stale()? {
            if usable.stale.is_empty() {
                return self.indexed_candidates(&filter, limit, usable.unsettled);
            }
            return self.indexed_candidates_with_stale(&filter, limit, usable);
        }
        let envelopes = self
            .bundle_store
            .list_bundles()?
            .into_iter()
            .map(|bundle| bundle.envelope)
            .collect();
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
        let mut envelopes = Vec::with_capacity(selection.rows.len());
        for row in &selection.rows {
            if let Some(envelope) = self.selected_envelope(&row.task_id)?
                && index_source.matches(&envelope)
            {
                envelopes.push(envelope);
            }
        }
        if bounded {
            return Ok(TaskCandidates {
                items: envelopes,
                total: selection.total,
                total_without_cursor,
            });
        }
        Ok(select_candidates(envelopes, filter, limit))
    }

    /// Selection over an index that a freshness scan accepted apart from
    /// `usable.stale` rows. The index answers every other task (in-flight
    /// ones left out, as in [`Self::indexed_candidates`]); each stale task is
    /// read from its bundle with the bundle scan's checks, so a stale row can
    /// neither drop nor add a task. The merged envelopes then take the bundle
    /// scan's in-memory filter, order, count and bound, giving the answer
    /// that scan would give without reading every other bundle.
    fn indexed_candidates_with_stale(
        &self,
        filter: &TaskListFilter,
        limit: usize,
        usable: IndexUsable,
    ) -> Result<TaskCandidates, OrbitError> {
        if filter.statuses.as_ref().is_some_and(Vec::is_empty) {
            return Ok(TaskCandidates::default());
        }
        let unbounded = filter.without_cursor();
        let mut excluded = usable.unsettled;
        excluded.extend(usable.stale.iter().cloned());
        let selection = self.registry.indexed_task_selection(
            &self.workspace_id,
            &unbounded.index_filter(excluded),
            filter.terminal_last,
            None,
        )?;
        let mut envelopes = Vec::with_capacity(selection.rows.len() + usable.stale.len());
        for row in &selection.rows {
            if let Some(envelope) = self.selected_envelope(&row.task_id)? {
                envelopes.push(envelope);
            }
        }
        for task_id in &usable.stale {
            if let Some(bundle) = self.bundle_store.read_bundle_if_settled(task_id)? {
                envelopes.push(bundle.envelope);
            }
        }
        Ok(select_candidates(envelopes, filter, limit))
    }

    /// The page a fully indexed filter selects, answered by the generated
    /// index alone: ids and creation times, with no envelope read. `None` when
    /// the filter needs envelope predicates or the index cannot serve, and the
    /// caller selects through [`Self::task_candidates`] instead.
    pub(crate) fn task_candidate_keys(
        &self,
        filter: &TaskListFilter,
        limit: usize,
    ) -> Result<Option<TaskCandidateKeys>, OrbitError> {
        self.candidate_keys(filter, limit, false)
    }

    /// [`Self::task_candidate_keys`] under the observational terms of
    /// [`Self::observe_task_candidates`]: freshness is validated without
    /// persisting proofs and recovery is never entered.
    pub(crate) fn observe_task_candidate_keys(
        &self,
        filter: &TaskListFilter,
        limit: usize,
    ) -> Result<Option<TaskCandidateKeys>, OrbitError> {
        self.candidate_keys(filter, limit, true)
    }

    fn candidate_keys(
        &self,
        filter: &TaskListFilter,
        limit: usize,
        observed: bool,
    ) -> Result<Option<TaskCandidateKeys>, OrbitError> {
        if !observed {
            self.ensure_recovered()?;
        }
        let filter = filter.normalized();
        if !filter.is_fully_indexed() {
            return Ok(None);
        }
        let Some(unsettled) = (if observed {
            self.validate_index_with_proofs(false)?
        } else {
            self.validate_index()?
        }) else {
            return Ok(None);
        };
        let selection = self.registry.indexed_task_selection(
            &self.workspace_id,
            &filter.index_filter(unsettled.clone()),
            filter.terminal_last,
            (limit < usize::MAX).then_some(limit),
        )?;
        let total_without_cursor = if filter.scan_before.is_none() {
            selection.total
        } else {
            self.registry
                .indexed_task_selection(
                    &self.workspace_id,
                    &filter.without_cursor().index_filter(unsettled),
                    filter.terminal_last,
                    Some(0),
                )?
                .total
        };
        let items = selection
            .rows
            .into_iter()
            .map(|row| {
                let created_at = chrono::DateTime::parse_from_rfc3339(&row.created_at)
                    .map_err(|error| {
                        OrbitError::Store(format!(
                            "invalid indexed created_at '{}' for task '{}': {error}",
                            row.created_at, row.task_id
                        ))
                    })?
                    .with_timezone(&Utc);
                Ok(TaskCandidateKey {
                    id: row.task_id,
                    created_at,
                })
            })
            .collect::<Result<Vec<_>, OrbitError>>()?;
        Ok(Some(TaskCandidateKeys {
            items,
            total: selection.total,
            total_without_cursor,
        }))
    }

    pub(crate) fn query_task_rows(
        &self,
        filter: &TaskListFilter,
        limit: usize,
        residual: TaskResidualFilter<'_>,
        hydration: TaskListHydration,
    ) -> Result<TaskPage, OrbitError> {
        let filter = filter.normalized();
        let selection_filter = if residual.is_some() {
            filter.without_cursor()
        } else {
            filter.clone()
        };
        let candidates = self.task_candidates(
            &selection_filter,
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
        let residual_total_without_cursor = selected.len();
        if residual.is_some() && filter.scan_before.is_some() {
            selected.retain(|candidate| filter.matches_cursor(candidate));
        }
        let selected_total = selected.len();
        let mut items = Vec::with_capacity(selected_total.min(limit));
        for candidate in selected {
            if items.len() >= limit {
                break;
            }
            if hydration == TaskListHydration::EnvelopeOnly {
                // Compact page: the candidate envelope is the whole row.
                // No bundle read, so no body documents or sidecars, and a
                // task a concurrent writer holds still lists from the
                // envelope selection already validated.
                let task = Self::metadata_task(&candidate);
                if residual.is_none_or(|matches| matches(&task, &status_by_id)) {
                    items.push(TaskRow {
                        task,
                        comments: Vec::new(),
                        history: Vec::new(),
                        artifacts: Vec::new(),
                    });
                }
                continue;
            }
            // A settled bundle with a provable freshness stamp may serve
            // its remembered row; anything else is read exactly as before.
            // The stamp is taken before any read below, so it labels that
            // read or something older, never something newer.
            let stamp = match self.bundle_store.settled_bundle_dir(&candidate.id) {
                Ok(Some(bundle_dir)) => {
                    super::bundle_stamp::task_bundle_freshness_stamp(&bundle_dir)
                }
                _ => None,
            };
            if let Some(stamp) = &stamp
                && let Some((envelope, row)) = self.row_cache.cached_row(&candidate.id, stamp)
            {
                if envelope != candidate {
                    // An update raced selection. One strict scan re-evaluates
                    // filters before the limit; no stale row escapes.
                    return self.scan_task_page(&filter, limit, residual);
                }
                if residual.is_none_or(|matches| matches(&row.task, &status_by_id)) {
                    items.push(row);
                }
                continue;
            }
            let Some(bundle) = self.bundle_store.read_bundle_if_settled(&candidate.id)? else {
                self.row_cache.forget(&candidate.id);
                continue;
            };
            if bundle.envelope != candidate {
                // An update raced selection. One strict scan re-evaluates filters
                // before the limit; no retry loop or stale selected row escapes.
                return self.scan_task_page(&filter, limit, residual);
            }
            let envelope = bundle.envelope.clone();
            let row = self.row_from_bundle(bundle)?;
            if let Some(stamp) = stamp {
                self.row_cache
                    .remember(&candidate.id, stamp, envelope, row.clone());
            }
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
            residual_total_without_cursor
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
        filter.sort_task_listing(
            &mut items,
            |row| row.task.not_before,
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

    /// The settled envelopes of `ids`: one keyed envelope read per id, so the
    /// cost follows the ids asked for rather than the workspace. The envelope
    /// file is read directly, not through the envelope cache, which is only
    /// proven current inside a freshness scan.
    pub(crate) fn task_envelopes_for_ids(
        &self,
        ids: &BTreeSet<String>,
    ) -> Result<Vec<TaskEnvelopeV2>, OrbitError> {
        self.ensure_recovered()?;
        let mut envelopes = Vec::with_capacity(ids.len());
        for id in ids {
            let _span = orbit_common::tracing::trace_span!(
                target: "orbit.store.task_query",
                "task_metadata_read",
                task_id = %id,
            )
            .entered();
            if let Some(envelope) = self.bundle_store.read_envelope_if_settled(id)? {
                envelopes.push(envelope);
            }
        }
        Ok(envelopes)
    }

    fn row_from_bundle(&self, mut bundle: TaskBundleV2) -> Result<TaskRow, OrbitError> {
        let comments = std::mem::take(&mut bundle.comments)
            .into_iter()
            .map(super::sidecars::task_comment_from_row)
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
    filter.select_owned_snapshot(envelopes, limit)
}
