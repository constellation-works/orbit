use std::collections::{BTreeMap, BTreeSet};

use super::envelope_cache::EnvelopeStamp;
use super::repair_gate::{RepairEvidence, RepairGate, RepairTicket};
use super::*;
use crate::contracts::{EnvelopeStampRecord, IndexedTaskRow, TaskCompletionByComplexity};
use orbit_common::StorageLayer;

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
    /// rewritten from its bundle yet. Best effort: the projection reads what
    /// the index holds either way, so a refused or failed repair never fails
    /// it.
    fn ensure_complexity_indexed(&self) -> Result<(), OrbitError> {
        if !self
            .registry
            .workspace_index_has_null_complexity(&self.workspace_id)?
        {
            return Ok(());
        }
        if let Some(ticket) = self.admit_index_repair()?
            && let Ok(bundles) = self.bundle_store.list_bundles()
        {
            self.attempt_index_repair(ticket, &bundles, "complexity column unpopulated");
        }
        Ok(())
    }

    /// The tasks an index query selects, in index order — or, when the index
    /// cannot serve, every settled task from the bundle scan, newest first.
    /// Callers re-apply their predicates, so either answer is correct.
    pub(crate) fn tasks_for_index_filter(
        &self,
        filter: TaskIndexFilter,
    ) -> Result<Vec<Task>, OrbitError> {
        if let Some(tasks) = self.indexed_tasks(&filter)? {
            return Ok(tasks);
        }
        let mut tasks = self
            .scan_and_repair_index("missing or stale index")?
            .into_iter()
            .map(|bundle| self.task_from_bundle(bundle))
            .collect::<Result<Vec<_>, _>>()?;
        sort_by_created_desc_id_asc(&mut tasks, |task| &task.created_at, |task| &task.id);
        Ok(tasks)
    }

    /// The tasks an index query selects, in index order, or `None` when the
    /// freshness scan finds that the index cannot serve.
    pub(crate) fn indexed_tasks(
        &self,
        filter: &TaskIndexFilter,
    ) -> Result<Option<Vec<Task>>, OrbitError> {
        if self.validate_index()?.is_none() {
            return Ok(None);
        }
        let ids = self
            .registry
            .indexed_task_ids_filtered(&self.workspace_id, filter)?;
        self.bundles_from_ids(ids)?
            .into_iter()
            .map(|bundle| self.task_from_bundle(bundle))
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// The freshness scan: compare every registered task's index row with its
    /// envelope on disk.
    ///
    /// `Some(unsettled)` means the index is usable; `unsettled` names the
    /// registered tasks whose bundle a concurrent writer holds, which a
    /// selection must leave out. `None` means a row is missing or disagrees
    /// with its envelope — on `updated_at` or on any field listing filters or
    /// orders by — and the caller must rebuild or scan bundles instead.
    ///
    /// Each registered task costs one metadata probe. Its envelope is parsed
    /// again only when neither the in-process [`EnvelopeCache`] nor a stamp
    /// recorded in the registry proves the file is one already compared with
    /// this index row (see the cache's freshness policy); each new proof is
    /// recorded for later scans in any process. Reuse never replaces the index
    /// comparison — a reused envelope that disagrees with its index row still
    /// sends the caller to a rebuild.
    pub(super) fn validate_index(&self) -> Result<Option<Vec<String>>, OrbitError> {
        let _span = orbit_common::tracing::trace_span!(
            target: "orbit.store.task_query",
            "task_index_freshness",
            workspace_id = %self.workspace_id,
        )
        .entered();
        let registered = self.registry.tasks_for_workspace(&self.workspace_id)?;
        let indexed = self
            .registry
            .indexed_task_rows_for_workspace(&self.workspace_id)?;
        if registered.len() != indexed.len() {
            return Ok(None);
        }
        self.envelope_cache.retain_registered(&registered);
        let recorded = self
            .registry
            .envelope_stamps_for_workspace(&self.workspace_id)?;

        let mut unsettled = Vec::new();
        let mut proofs = Vec::new();
        for binding in &registered {
            let Some(row) = indexed.get(&binding.task_id) else {
                return Ok(None);
            };
            let check = EnvelopeCheck {
                task_id: &binding.task_id,
                row,
                recorded: recorded.get(&binding.task_id),
            };
            match self.settled_envelope_matches(check, &mut proofs)? {
                Some(true) => {}
                Some(false) => return Ok(None),
                None => unsettled.push(binding.task_id.clone()),
            }
        }
        // Best effort: the proofs only spare later scans a parse, and a
        // registry on read-only media cannot take them.
        if let Err(error) = self
            .registry
            .record_envelope_stamps(&self.workspace_id, &proofs)
        {
            orbit_common::tracing::debug!(
                target: "orbit.store.task_bundle_v2",
                workspace_id = %self.workspace_id,
                %error,
                "could not record envelope stamps; the next cold scan parses those envelopes",
            );
        }
        Ok(Some(unsettled))
    }

    /// Whether one registered task's envelope matches its index row, reusing
    /// a previous parse or a recorded proof while the envelope file is
    /// unchanged. `None` carries the same meaning as
    /// [`TaskBundleStoreV2::read_envelope_if_settled`]: a concurrent writer
    /// holds this bundle, so the scan skips it rather than failing. A match
    /// the registry does not hold yet is added to `proofs`.
    fn settled_envelope_matches(
        &self,
        check: EnvelopeCheck<'_>,
        proofs: &mut Vec<EnvelopeStampRecord>,
    ) -> Result<Option<bool>, OrbitError> {
        let EnvelopeCheck {
            task_id,
            row,
            recorded,
        } = check;
        // Stamped before the parse it labels, so a write that races this read
        // costs one extra parse next scan instead of pinning stale content.
        let stamp = self
            .envelope_cache
            .stamp(&self.bundle_store.envelope_path(task_id)?);
        let proof = |matches: bool| {
            let fingerprint = row.fingerprint();
            stamp
                .as_ref()
                .and_then(EnvelopeStamp::persisted)
                .filter(|stamp| {
                    matches
                        && recorded.is_none_or(|recorded| {
                            recorded.stamp != *stamp || recorded.fingerprint != fingerprint
                        })
                })
                .map(|stamp| EnvelopeStampRecord {
                    task_id: task_id.to_string(),
                    stamp,
                    fingerprint,
                })
        };
        if let Some(stamp) = &stamp {
            if let Some(matches) = self
                .envelope_cache
                .inspect_fresh(task_id, stamp, |envelope| row.matches(envelope))
            {
                proofs.extend(proof(matches));
                return Ok(Some(matches));
            }
            if let (Some(persisted), Some(recorded)) = (stamp.persisted(), recorded)
                && recorded.stamp == persisted
                && recorded.fingerprint == row.fingerprint()
            {
                // Any parse this process still holds is of an older file.
                self.envelope_cache.forget(task_id);
                return Ok(Some(true));
            }
        }

        let Some(envelope) = self.bundle_store.read_envelope_if_settled(task_id)? else {
            self.envelope_cache.forget(task_id);
            return Ok(None);
        };
        if let Some(stamp) = stamp {
            self.envelope_cache.remember(task_id, stamp, &envelope);
        }
        let matches = row.matches(&envelope);
        proofs.extend(proof(matches));
        Ok(Some(matches))
    }

    /// The envelope of a task the freshness scan just accepted: the parse it
    /// reused or made, or — for a task accepted on a recorded stamp — a read
    /// now. `None` when a concurrent writer holds the bundle.
    pub(super) fn selected_envelope(
        &self,
        task_id: &str,
    ) -> Result<Option<TaskEnvelopeV2>, OrbitError> {
        let _span = orbit_common::tracing::trace_span!(
            target: "orbit.store.task_query",
            "task_envelope_selection",
            task_id,
        )
        .entered();
        if let Some(envelope) = self.envelope_cache.cached(task_id) {
            return Ok(Some(envelope));
        }
        let stamp = self
            .envelope_cache
            .stamp(&self.bundle_store.envelope_path(task_id)?);
        let Some(envelope) = self.bundle_store.read_envelope_if_settled(task_id)? else {
            return Ok(None);
        };
        if let Some(stamp) = stamp {
            self.envelope_cache.remember(task_id, stamp, &envelope);
        }
        Ok(Some(envelope))
    }

    /// Read every settled bundle for a read the index cannot serve, and
    /// rebuild the index from them when the [`RepairGate`] admits it.
    ///
    /// The scan is strict: a task-field error in any bundle fails the read
    /// rather than hiding behind a degraded index. Only the rebuild is
    /// best effort, since every caller reaches it from a read. Listing uses
    /// the lightweight bundle read (task fields only); explicit
    /// `reindex_workspace` still hashes artifact payloads.
    pub(super) fn scan_and_repair_index(
        &self,
        reason: &str,
    ) -> Result<Vec<TaskBundleV2>, OrbitError> {
        let ticket = self.admit_index_repair()?;
        let bundles = self.bundle_store.list_bundles()?;
        if let Some(ticket) = ticket {
            self.attempt_index_repair(ticket, &bundles, reason);
        }
        Ok(bundles)
    }

    /// Ask the gate whether this read may attempt a rebuild. The evidence is
    /// one metadata probe per registered task, taken before the bundles are
    /// read; a recorded failure's unresolved targets are re-resolved so that
    /// restoring one re-admits the repair.
    fn admit_index_repair(&self) -> Result<Option<RepairTicket>, OrbitError> {
        let gate = self.repair_gate();
        let mut evidence = RepairEvidence::default();
        for binding in self.registry.tasks_for_workspace(&self.workspace_id)? {
            let stamp = self
                .envelope_cache
                .stamp(&self.bundle_store.envelope_path(&binding.task_id)?);
            evidence.envelopes.insert(binding.task_id, stamp);
        }
        let mut target_restored = false;
        for target in gate.unresolved_targets() {
            if self.registry.find_task_binding(&target)?.is_some() {
                target_restored = true;
                break;
            }
        }
        Ok(gate.admit(evidence, target_restored))
    }

    /// Publish the index from `bundles`, recording a refusal with the gate.
    /// The validator stays strict: a dangling edge keeps the index stale and
    /// the warning names every canonical edge that blocks it.
    fn attempt_index_repair(&self, ticket: RepairTicket, bundles: &[TaskBundleV2], reason: &str) {
        let envelopes = bundles
            .iter()
            .map(|bundle| bundle.envelope.clone())
            .collect::<Vec<_>>();
        let Err(error) = self
            .registry
            .replace_workspace_task_indexes(&self.workspace_id, &envelopes)
        else {
            ticket.succeeded();
            return;
        };
        let unresolved = self
            .registry
            .unresolved_relation_targets(&self.workspace_id, &envelopes)
            .unwrap_or_default();
        let rejected = !matches!(
            error.storage_layer(),
            Some(StorageLayer::Store | StorageLayer::Io)
        );
        let suppressed_reads = ticket.failed(
            rejected,
            unresolved
                .iter()
                .map(|edge| edge.target_task_id.clone())
                .collect(),
        );
        let unresolved = unresolved
            .iter()
            .map(|edge| {
                format!(
                    "{} {} -> {}{}",
                    edge.source_task_id,
                    edge.relation_type,
                    edge.target_task_id,
                    if edge.indexed {
                        ""
                    } else {
                        " (missing from generated index)"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        orbit_common::tracing::warn!(
            target: "orbit.store.task_v2",
            workspace_id = %self.workspace_id,
            reason,
            %error,
            unresolved,
            suppressed_reads,
            remediation = "drop each unresolved edge through orbit.task.update relations or restore its target task; `orbit doctor` lists them",
            "generated task index repair failed; reads serve from a bundle scan and retry once a bundle or target changes",
        );
    }

    fn repair_gate(&self) -> RepairGate {
        RepairGate::new(self.registry.workspaces_dir(), &self.workspace_id)
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
        let _span = orbit_common::tracing::trace_span!(
            target: "orbit.store.task_query",
            "task_bundle_materialization",
            task_id = %bundle.envelope.id,
        )
        .entered();
        Ok(Task::from_envelope_parts(
            bundle.envelope,
            bundle.description,
            parse_acceptance(&bundle.acceptance),
            bundle.plan,
            bundle.execution_summary,
        ))
    }

    /// The task an envelope describes, without its body documents: `description`,
    /// `plan`, `execution_summary` and `acceptance_criteria` are empty. Selection
    /// uses it to evaluate metadata-only predicates before paying for a bundle.
    pub(super) fn metadata_task(envelope: &TaskEnvelopeV2) -> Task {
        Task::from_envelope_parts(
            envelope.clone(),
            String::new(),
            Vec::new(),
            String::new(),
            String::new(),
        )
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

/// One registered task as the freshness scan checks it.
struct EnvelopeCheck<'a> {
    task_id: &'a str,
    row: &'a IndexedTaskRow,
    /// The proof the registry holds for this task, if any.
    recorded: Option<&'a EnvelopeStampRecord>,
}
