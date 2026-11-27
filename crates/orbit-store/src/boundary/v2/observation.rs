//! Informational task reads for display surfaces: the dashboard board and
//! task detail, readiness, Jobs title labels and resource profiles.
//!
//! Each read here answers from the settled snapshot on disk. It never enters
//! the commit boundary, settles a pending commit, repairs the generated index
//! or persists freshness proofs, so a display poll neither queues behind a
//! held task writer nor holds the boundary against one. A bundle a writer
//! holds in flight reads as absent, exactly as it would have a moment earlier
//! or later. Admission and mutation keep the coordinated reads in
//! `operations.rs`; nothing here may feed a decision that admits or writes.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::contract::{TaskListFilter, TaskListHydration, TaskPage, TaskResidualFilter, TaskRow};

impl TaskV2Store {
    /// [`Self::task_envelopes_for_ids`] from the settled snapshot: one keyed
    /// envelope read per id, with no recovery. An id whose bundle is in
    /// flight or absent is left out.
    pub fn observe_task_envelopes_for_ids(
        &self,
        ids: &BTreeSet<String>,
    ) -> Result<Vec<TaskEnvelopeV2>, OrbitError> {
        self.settled_envelopes_for_ids(ids)
    }

    /// [`Self::task_ids_with_tag`] from the registry's generated index alone,
    /// outside the commit boundary.
    pub fn observe_task_ids_with_tag(
        &self,
        tag: &str,
        task_ids: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, OrbitError> {
        self.registry.task_ids_with_tag(tag, task_ids)
    }

    /// [`Self::task_status_index`] from the registry, outside the commit
    /// boundary.
    pub fn observe_task_status_index(&self) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        self.registry.global_task_status_index()
    }

    /// The listed row of one task (see [`Self::get_task_row`] with
    /// `list_read`) from its settled bundle, with no recovery. `None` when the
    /// task is absent or a writer has its bundle in flight.
    pub fn observe_listed_task_row(&self, id: &str) -> Result<Option<TaskRow>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        self.listed_row_settled(id)
    }

    /// [`Self::get_task_row`] (a direct, strict read: artifact payloads are
    /// checked) without the commit boundary or recovery. A bundle with an
    /// interrupted commit awaiting replay is refused, so the caller can take
    /// the coordinated read that settles it.
    pub fn observe_task_row(&self, id: &str) -> Result<Option<TaskRow>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        match self.bundle_store.read_bundle(id) {
            Ok(bundle) => self.row_from_bundle(bundle).map(Some),
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Task,
                ..
            }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// [`Self::query_task_rows`] over the observational candidate selection
    /// ([`Self::observe_task_candidates`]). Hydration and its race fallback
    /// are the coordinated page's own.
    pub fn observe_task_rows(
        &self,
        filter: &TaskListFilter,
        limit: usize,
        residual: TaskResidualFilter<'_>,
        hydration: TaskListHydration,
    ) -> Result<TaskPage, OrbitError> {
        let mut pages =
            self.observe_task_row_pages(&[(filter.clone(), limit)], residual, hydration)?;
        Ok(pages.pop().unwrap_or_default())
    }

    /// [`Self::observe_task_rows`] for several pages of one request, in
    /// request order, sharing one observational index freshness validation
    /// (or one settled bundle listing when the index cannot serve).
    pub fn observe_task_row_pages(
        &self,
        pages: &[(TaskListFilter, usize)],
        residual: TaskResidualFilter<'_>,
        hydration: TaskListHydration,
    ) -> Result<Vec<TaskPage>, OrbitError> {
        if pages.is_empty() {
            return Ok(Vec::new());
        }
        let usable = self.observe_index_tolerating_stale()?;
        let mut scanned = None;
        pages
            .iter()
            .map(|(filter, limit)| {
                let filter = filter.normalized();
                let selection_filter = if residual.is_some() {
                    filter.without_cursor()
                } else {
                    filter.clone()
                };
                let candidates = self.observed_candidates(
                    &selection_filter,
                    Self::selection_limit(*limit, residual),
                    usable.as_ref(),
                    &mut scanned,
                )?;
                self.page_from_candidates(&filter, *limit, residual, hydration, candidates)
            })
            .collect()
    }
}
