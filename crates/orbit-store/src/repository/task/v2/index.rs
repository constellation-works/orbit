use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::contracts::{IndexedTaskRow, TaskCompletionByComplexity};

impl TaskV2Store {
    pub(crate) fn task_status_index(
        &self,
    ) -> Result<std::collections::BTreeMap<String, TaskStatus>, OrbitError> {
        self.registry.global_task_status_index()
    }

    pub(crate) fn task_status_index_for(
        &self,
        workspace_id: &str,
        targets: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        self.registry.task_status_index_for(workspace_id, targets)
    }

    pub(crate) fn task_completion_by_complexity(
        &self,
    ) -> Result<Vec<TaskCompletionByComplexity>, OrbitError> {
        self.ensure_complexity_indexed()?;
        self.registry.completion_by_complexity(&self.workspace_id)
    }

    pub(crate) fn task_complexity_by_id(&self) -> Result<BTreeMap<String, String>, OrbitError> {
        self.ensure_complexity_indexed()?;
        self.registry.complexity_by_task_id(&self.workspace_id)
    }

    /// One-time rebuild after `complexity` was added as a nullable column.
    /// Indexed unset is `''`; leftover `NULL` means the row has not been
    /// rewritten from its bundle yet.
    fn ensure_complexity_indexed(&self) -> Result<(), OrbitError> {
        if !self
            .registry
            .workspace_index_has_null_complexity(&self.workspace_id)?
        {
            return Ok(());
        }
        let _ = self.rebuild_index_best_effort("complexity column unpopulated");
        Ok(())
    }

    pub(super) fn indexed_tasks(
        &self,
        filter: TaskIndexFilter,
    ) -> Result<Option<Vec<Task>>, OrbitError> {
        let Some(bundles) = self.indexed_bundles(filter)? else {
            return Ok(None);
        };
        bundles
            .into_iter()
            .map(|bundle| self.task_from_bundle(bundle))
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// The bundles behind an index query, in index order. `None` when the
    /// index is not usable and the caller must scan bundles instead.
    pub(super) fn indexed_bundles(
        &self,
        filter: TaskIndexFilter,
    ) -> Result<Option<Vec<TaskBundleV2>>, OrbitError> {
        if !self.index_is_usable()? {
            return Ok(None);
        }
        let ids = self
            .registry
            .indexed_task_ids_filtered(&self.workspace_id, &filter)?;
        self.bundles_from_ids(ids).map(Some)
    }

    /// Decide whether the generated index still matches the bundles on disk.
    ///
    /// Two properties matter under concurrency (ORB-10988 / F2026-07-119).
    /// First, this compares envelopes, not whole bundles: the index only
    /// projects envelope fields, so assembling every task's seven-file bundle
    /// on every list was pure cost. Second, a task whose bundle a concurrent
    /// writer currently holds is *skipped* rather than propagated as an error —
    /// validating the index for task B must not fail because task A is being
    /// created or deleted at that instant.
    fn index_is_usable(&self) -> Result<bool, OrbitError> {
        if self.validate_index()?.is_some() {
            Ok(true)
        } else {
            self.rebuild_index_best_effort("missing or stale index")
        }
    }

    /// The freshness scan: compare every registered task's index row with its
    /// envelope on disk, leaving the [`EnvelopeCache`] warm for each settled
    /// task so a selection can serve its rows without reading them again.
    ///
    /// `Some(unsettled)` means the index is usable; `unsettled` names the
    /// registered tasks whose bundle a concurrent writer holds, which a
    /// selection must leave out. `None` means a row is missing or disagrees
    /// with its envelope — on `updated_at` or on any field listing filters or
    /// orders by — and the caller must rebuild or scan bundles instead.
    ///
    /// Each registered task costs one metadata probe; its envelope is parsed
    /// again only when the cache's stamp policy cannot prove the file is the
    /// one already parsed. Reuse never replaces the index comparison — a
    /// cached envelope that disagrees with its index row still sends the
    /// caller to a rebuild.
    pub(super) fn validate_index(&self) -> Result<Option<Vec<String>>, OrbitError> {
        let registered = self.registry.tasks_for_workspace(&self.workspace_id)?;
        let indexed = self
            .registry
            .indexed_task_rows_for_workspace(&self.workspace_id)?;
        if registered.len() != indexed.len() {
            return Ok(None);
        }
        self.envelope_cache.retain_registered(&registered);

        let mut unsettled = Vec::new();
        for binding in &registered {
            let Some(row) = indexed.get(&binding.task_id) else {
                return Ok(None);
            };
            match self.settled_envelope_matches(&binding.task_id, row)? {
                Some(true) => {}
                Some(false) => return Ok(None),
                None => unsettled.push(binding.task_id.clone()),
            }
        }
        Ok(Some(unsettled))
    }

    /// Whether one registered task's envelope matches its index row, reusing
    /// the previous parse while the envelope file is unchanged. `None` carries
    /// the same meaning as [`TaskBundleStoreV2::read_envelope_if_settled`]: a
    /// concurrent writer holds this bundle, so the scan skips it rather than
    /// failing.
    fn settled_envelope_matches(
        &self,
        task_id: &str,
        row: &IndexedTaskRow,
    ) -> Result<Option<bool>, OrbitError> {
        // Stamped before the parse it labels, so a write that races this read
        // costs one extra parse next scan instead of pinning stale content.
        let stamp = self
            .envelope_cache
            .stamp(&self.bundle_store.envelope_path(task_id)?);
        if let Some(stamp) = stamp
            && let Some(matches) = self
                .envelope_cache
                .inspect_fresh(task_id, &stamp, |envelope| row.matches(envelope))
        {
            return Ok(Some(matches));
        }

        let Some(envelope) = self.bundle_store.read_envelope_if_settled(task_id)? else {
            self.envelope_cache.forget(task_id);
            return Ok(None);
        };
        if let Some(stamp) = stamp {
            self.envelope_cache.remember(task_id, stamp, &envelope);
        }
        Ok(Some(row.matches(&envelope)))
    }

    /// Rebuild the generated index from the bundles, degrading to `false` (use
    /// the bundle scan instead) on any failure. Listing-triggered rebuild uses
    /// the lightweight bundle read (task fields only); explicit
    /// `reindex_workspace` still hashes artifact payloads. Every caller
    /// reaches this from a *read*, so a rebuild that cannot run must not fail
    /// that read.
    fn rebuild_index_best_effort(&self, reason: &str) -> Result<bool, OrbitError> {
        let rebuilt = self.bundle_store.list_bundles().and_then(|bundles| {
            let envelopes = bundles
                .into_iter()
                .map(|bundle| bundle.envelope)
                .collect::<Vec<_>>();
            self.registry
                .replace_workspace_task_indexes(&self.workspace_id, &envelopes)
        });
        match rebuilt {
            Ok(()) => Ok(true),
            Err(err) => {
                orbit_common::tracing::warn!(
                    target: "orbit.store.task_v2",
                    workspace_id = %self.workspace_id,
                    reason,
                    error = %err,
                    "generated task index rebuild failed; falling back to bundle scan",
                );
                Ok(false)
            }
        }
    }

    /// Materialize indexed ids into tasks, dropping any whose bundle a
    /// concurrent writer is publishing or removing. An id that disappears
    /// between the index query and the bundle read is a task that was deleted,
    /// not a listing failure.
    fn bundles_from_ids(&self, ids: Vec<String>) -> Result<Vec<TaskBundleV2>, OrbitError> {
        let mut bundles = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(bundle) = self.bundle_store.read_bundle_if_settled(&id)? else {
                continue;
            };
            bundles.push(bundle);
        }
        Ok(bundles)
    }

    pub(super) fn replace_index_best_effort(&self, envelope: &TaskEnvelopeV2, operation: &str) {
        if let Err(err) = self
            .registry
            .replace_task_index(&self.workspace_id, envelope)
        {
            orbit_common::tracing::warn!(
                target: "orbit.store.task_v2",
                task_id = %envelope.id,
                workspace_id = %self.workspace_id,
                operation,
                error = %err,
                "task bundle was updated but generated task index update failed",
            );
        }
    }

    pub(crate) fn task_from_bundle(&self, bundle: TaskBundleV2) -> Result<Task, OrbitError> {
        let status = bundle.envelope.status;
        Ok(Task {
            job_run_host: bundle.envelope.job_run_host,
            id: bundle.envelope.id,
            title: bundle.envelope.title,
            description: bundle.description,
            acceptance_criteria: parse_acceptance(&bundle.acceptance),
            tags: normalize_task_tags(bundle.envelope.tags),
            required_tools: orbit_types::task::normalize_required_tools(
                bundle.envelope.required_tools,
            ),
            plan: bundle.plan,
            execution_summary: bundle.execution_summary,
            context_files: bundle.envelope.context_files,
            created_by: bundle.envelope.created_by,
            planned_by: bundle.envelope.planned_by,
            implemented_by: bundle.envelope.implemented_by,
            status,
            priority: bundle.envelope.priority,
            complexity: bundle.envelope.complexity,
            task_type: bundle.envelope.task_type,
            pr_status: bundle.envelope.pr_status,
            external_refs: bundle.envelope.external_refs,
            relations: bundle.envelope.relations,
            job_run_id: bundle.envelope.job_run_id,
            crew: bundle.envelope.crew,
            orchestrator: bundle.envelope.orchestrator,
            created_at: bundle.envelope.created_at,
            updated_at: bundle.envelope.updated_at,
        })
    }

    pub(super) fn read_existing_bundle(&self, id: &str) -> Result<TaskBundleV2, OrbitError> {
        self.bundle_store.read_bundle(id).map_err(|err| match err {
            OrbitError::NotFound {
                kind: NotFoundKind::Task,
                ..
            } => OrbitError::not_found(NotFoundKind::Task, id.to_string()),
            other => other,
        })
    }

    /// Run `op` under this task's exclusive bundle lock.
    ///
    /// The lock target belongs to the bundle store, which hands the same file
    /// to the shared lock its full-bundle reads take, so a write and a
    /// concurrent read cannot disagree about what coordinates them.
    pub(crate) fn with_task_lock<T, F>(&self, id: &str, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        // Boundary first, bundle lock second, on every path. An admission
        // section holds the boundary exclusively and then takes bundle locks
        // inside it, so a caller that acquired them the other way round could
        // deadlock against it (ORB-12528).
        self.in_boundary(|| {
            if let Some(boundary) = &self.coordination {
                boundary.refuse_unscoped_claim_write(id)?;
            }
            self.bundle_store.with_bundle_write_lock(id, op)
        })
    }
}
