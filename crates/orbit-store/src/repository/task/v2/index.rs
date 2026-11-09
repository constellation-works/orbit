use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use super::envelope_cache;
use super::repair_gate::{RepairEvidence, RepairGate, RepairTicket};
use super::*;
use crate::contracts::{
    EnvelopeStampRecord, TaskCompletionByComplexity, TaskDefectFiling, TaskTerminalTransition,
};
use crate::driver::file::task_bundle::bundle_lock_target;
use crate::driver::sqlite::task_registry::TaskIndexProofRow;
use orbit_common::StorageLayer;
use orbit_common::fs::io::{FileLockProbe, hold_step, probe_file_lock};

/// Steps a boundary section's hold report names for index work
/// ([`hold_step`]). The freshness step is the scan's own time — metadata
/// probes, cache lookups and comparisons — less the snapshot reads, envelope
/// reads and stamp write it times separately.
const INDEX_FRESHNESS_STEP: &str = "index freshness";
const INDEX_SNAPSHOT_STEP: &str = "index snapshot";
const ENVELOPE_STAMP_WRITE_STEP: &str = "envelope stamp write";
const INDEX_REPAIR_STEP: &str = "index repair";
const INDEX_WRITER_WAIT_STEP: &str = "index writer wait";

/// Most freshness scans one read makes before it falls back to a bundle scan:
/// the first, its confirmation, and one more when that finds another task
/// mismatched, since a drain's commits overlap.
const INDEX_FRESHNESS_ATTEMPTS: usize = 3;
/// Longest one scan waits for a writer to publish a mismatching task's index
/// row. A deferred commit apply takes milliseconds, and a scan of the workspace
/// takes seconds.
#[cfg(not(test))]
const IN_FLIGHT_WRITER_WAIT: Duration = Duration::from_secs(2);
/// A test holds the writer on a channel, so the wait must outlast a loaded host.
#[cfg(test)]
const IN_FLIGHT_WRITER_WAIT: Duration = Duration::from_secs(60);

/// What an accepted freshness scan leaves the selection to handle.
pub(super) struct IndexUsable {
    /// Registered tasks whose bundle a concurrent writer holds; a selection
    /// leaves them out.
    pub(super) unsettled: Vec<String>,
    /// Registered tasks whose index row disagrees with a settled envelope.
    /// Empty unless the scan tolerates stale rows.
    pub(super) stale: Vec<String>,
}

/// What one freshness scan found.
enum IndexFreshness {
    /// The index serves; these registered tasks' bundles are in flight.
    Usable(Vec<String>),
    /// Only from a scan that tolerates stale rows: the index serves except
    /// for the `stale` tasks, whose index row disagrees with an envelope no
    /// writer holds. The caller reads those tasks from their bundles.
    UsableExceptStale {
        unsettled: Vec<String>,
        stale: Vec<String>,
    },
    /// The index holds a different set of tasks from the registry.
    Incomplete,
    /// This task's index row disagrees with its envelope.
    Mismatch(String),
}

/// Partition size from which a freshness scan stamps envelopes on helper
/// threads. Smaller partitions finish faster than the threads would start.
const PARALLEL_STAMP_MIN_ROWS: usize = 256;
/// Most threads one freshness scan stamps envelopes on. Aggregate reads scan
/// workspaces concurrently, so this stays well below the host's core count.
const MAX_STAMP_THREADS: usize = 4;

impl TaskV2Store {
    /// Enumerate the authoritative registrations, retaining listing order
    /// from the index only when it covers every registration. Bundle scans
    /// supply their own freshness and validation, so no envelope probe is
    /// needed here. An incomplete index keeps the normal repair fallback.
    pub(crate) fn registered_task_bundle_ids(&self) -> Result<Option<Vec<String>>, OrbitError> {
        self.ensure_recovered()?;
        let snapshot = self.registry.task_index_snapshot(&self.workspace_id)?;
        let Some(rows) = &snapshot.rows else {
            return Ok(None);
        };
        let mut ordered = rows.iter().collect::<Vec<_>>();
        ordered.sort_by(|a, b| {
            b.indexed
                .created_at
                .cmp(&a.indexed.created_at)
                .then(a.task_id.cmp(&b.task_id))
        });
        Ok(Some(
            ordered.into_iter().map(|row| row.task_id.clone()).collect(),
        ))
    }

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

    pub(crate) fn task_ids_with_tag(
        &self,
        tag: &str,
        task_ids: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, OrbitError> {
        self.registry.task_ids_with_tag(tag, task_ids)
    }

    pub(crate) fn task_change_stamps(
        &self,
    ) -> Result<Vec<crate::contracts::TaskChangeStamp>, OrbitError> {
        self.registry.task_change_stamps(&self.workspace_id)
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

    pub(crate) fn task_terminal_transitions(
        &self,
        since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<TaskTerminalTransition>, OrbitError> {
        self.ensure_complexity_indexed()?;
        self.registry
            .terminal_transitions(&self.workspace_id, since)
    }

    pub(crate) fn task_defect_filings(
        &self,
        since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<TaskDefectFiling>, OrbitError> {
        self.ensure_task_type_indexed()?;
        self.registry.defect_filings(&self.workspace_id, since)
    }

    /// Older writers leave new rows' type NULL and preserve existing rows'
    /// type even when the canonical type changes. Check every row, including
    /// named causes, against its envelope before using the defect projection.
    /// Repair stays best effort: unreadable bundles or a refused repair leave
    /// the projection serving the index it already holds.
    fn ensure_task_type_indexed(&self) -> Result<(), OrbitError> {
        // A failed freshness scan is a read failure. Swallowing it and serving
        // the projection would hide a store error behind a stale count.
        if self.validate_index()?.is_some() {
            return Ok(());
        }
        if let Some(ticket) = self.admit_index_repair()?
            && let Ok(bundles) = self.bundle_store.list_bundles()
        {
            self.attempt_index_repair(ticket, &bundles, "missing or stale defect projection");
        }
        Ok(())
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
    /// with its envelope — on `updated_at` or on a listing or defect projection
    /// field — and the caller must rebuild or scan bundles instead.
    ///
    /// Each registered task costs one metadata probe. Its envelope is parsed
    /// again only when neither the in-process [`EnvelopeCache`] nor a stamp
    /// recorded in the registry proves the file is one already compared with
    /// this index row (see the cache's freshness policy); writable scans record
    /// each new proof for later scans in any process. Reuse never replaces the index
    /// comparison — a reused envelope that disagrees with its index row still
    /// sends the caller to a rebuild.
    /// Registry rows and their fingerprints are shared between reads until
    /// SQLite's connection-local data version changes; filesystem probes are
    /// never skipped. Fingerprints are computed once per registry snapshot.
    pub(super) fn validate_index(&self) -> Result<Option<Vec<String>>, OrbitError> {
        self.validate_index_with_proofs(true)
    }

    pub(super) fn validate_index_with_proofs(
        &self,
        persist_proofs: bool,
    ) -> Result<Option<Vec<String>>, OrbitError> {
        Ok(self
            .validate_index_scan(persist_proofs, false)?
            .map(|usable| usable.unsettled))
    }

    /// The observational freshness scan for a reader that can merge stale
    /// rows itself: a row whose envelope disagrees with the index while no
    /// writer holds its bundle is reported in [`IndexUsable::stale`] instead
    /// of rejecting the whole index. The caller must read each stale task
    /// from its bundle rather than trust its index row. Without this, one
    /// such row sends every observational read to a workspace-wide bundle
    /// scan — seconds on a large partition — until an operational read
    /// repairs the index; observational reads never repair it.
    pub(super) fn observe_index_tolerating_stale(&self) -> Result<Option<IndexUsable>, OrbitError> {
        self.validate_index_scan(false, true)
    }

    fn validate_index_scan(
        &self,
        persist_proofs: bool,
        tolerate_stale: bool,
    ) -> Result<Option<IndexUsable>, OrbitError> {
        let tolerate_stale = tolerate_stale && !persist_proofs;
        let _span = orbit_common::tracing::trace_span!(
            target: "orbit.store.task_query",
            "task_index_freshness",
            workspace_id = %self.workspace_id,
        )
        .entered();
        hold_step(INDEX_FRESHNESS_STEP, || {
            // The index snapshot precedes the envelope probes. A writer can
            // finish publishing between them, so confirm a mismatch against a
            // new snapshot before paying for a workspace-wide bundle scan.
            let mut awaited = None;
            for _ in 0..INDEX_FRESHNESS_ATTEMPTS {
                match self.validate_index_once(persist_proofs, tolerate_stale)? {
                    IndexFreshness::Usable(unsettled) => {
                        orbit_common::tracing::debug!(
                            target: "orbit.store.task_query",
                            workspace_id = %self.workspace_id,
                            path = "index",
                            "task index freshness accepted",
                        );
                        return Ok(Some(IndexUsable {
                            unsettled,
                            stale: Vec::new(),
                        }));
                    }
                    IndexFreshness::UsableExceptStale { unsettled, stale } => {
                        orbit_common::tracing::debug!(
                            target: "orbit.store.task_query",
                            workspace_id = %self.workspace_id,
                            path = "index",
                            stale_rows = stale.len(),
                            "task index freshness accepted apart from stale rows",
                        );
                        return Ok(Some(IndexUsable { unsettled, stale }));
                    }
                    IndexFreshness::Incomplete => {}
                    IndexFreshness::Mismatch(task_id) => {
                        // The same task again means no writer was publishing
                        // it: the row really is stale. Observation reads create
                        // no lock files, so they only re-probe.
                        if awaited.as_ref() == Some(&task_id) {
                            return Ok(None);
                        }
                        if persist_proofs {
                            self.await_in_flight_bundle_writer(&task_id);
                        }
                        awaited = Some(task_id);
                    }
                }
            }
            Ok(None)
        })
    }

    /// Whether this task's bundle writer currently holds its exclusive lock.
    ///
    /// The probe opens the sibling `.{file}.lock` of the bundle lock target
    /// and does not acquire that lock. `false` on any probe error, so a real
    /// mismatch is never hidden.
    fn bundle_writer_holds(&self, task_id: &str) -> bool {
        let Ok(bundle_dir) = self.bundle_store.bundle_path(task_id) else {
            return false;
        };
        let target = bundle_lock_target(&bundle_dir);
        let Some(file_name) = target.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        let lock_path = target.with_file_name(format!(".{file_name}.lock"));
        let lock_path = match lock_path.parent().map(std::fs::canonicalize) {
            Some(Ok(parent)) => parent.join(format!(".{file_name}.lock")),
            _ => lock_path,
        };
        matches!(probe_file_lock(&lock_path), Ok(FileLockProbe::Held))
    }

    /// Wait for the writer publishing `task_id`, if there is one.
    ///
    /// Envelope reads take no lock, and every writer shows its envelope before
    /// it updates the index row, all under the bundle's exclusive lock: a
    /// coordinated commit does so across its whole deferred apply, with the
    /// partition already released. A read in that window would otherwise
    /// conclude the index is stale, and scan and rebuild the whole workspace
    /// under its shared partition section, with every admission queued behind
    /// it. The scan would wait for this same lock too, so this wait is never
    /// the longer one. It is bounded because the caller holds its section, and
    /// a writer that outlasts it, or a lock that cannot be taken, leaves the
    /// decision to the next probe.
    fn await_in_flight_bundle_writer(&self, task_id: &str) {
        orbit_common::tracing::debug!(
            target: "orbit.store.task_query",
            workspace_id = %self.workspace_id,
            task_id,
            "task index freshness waiting for a bundle writer",
        );
        if let Err(error) = hold_step(INDEX_WRITER_WAIT_STEP, || {
            self.bundle_store
                .await_bundle_writer(task_id, IN_FLIGHT_WRITER_WAIT)
        }) {
            orbit_common::tracing::debug!(
                target: "orbit.store.task_query",
                workspace_id = %self.workspace_id,
                task_id,
                %error,
                "task index freshness did not wait out a bundle writer",
            );
        }
    }

    fn validate_index_once(
        &self,
        persist_proofs: bool,
        tolerate_stale: bool,
    ) -> Result<IndexFreshness, OrbitError> {
        let snapshot = hold_step(INDEX_SNAPSHOT_STEP, || {
            self.registry.task_index_snapshot(&self.workspace_id)
        })?;
        let registered = &snapshot.registered;
        orbit_common::tracing::debug!(
            target: "orbit.store.task_query",
            workspace_id = %self.workspace_id,
            indexed_tasks = snapshot.indexed_count,
            "task index snapshot loaded",
        );
        let Some(rows) = &snapshot.rows else {
            return Ok(IndexFreshness::Incomplete);
        };
        self.envelope_cache.retain_registered(registered);

        let mut unsettled = Vec::new();
        let mut stale = Vec::new();
        let mut proofs = Vec::new();
        let stamps = self.stamp_index_rows(rows);
        for (row, stamp) in rows.iter().zip(stamps) {
            match self.settled_envelope_matches(row, stamp, &mut proofs)? {
                Some(true) => {}
                Some(false) if !persist_proofs && self.bundle_writer_holds(&row.task_id) => {
                    // A commit shows the decided envelope before deferred apply
                    // updates this index row, and it holds the bundle lock for
                    // that whole apply. The parse above is already that
                    // envelope, cached for selection. Waiting the writer out
                    // (or rebuilding the workspace when the wait expires) is
                    // what stalls an observational read for seconds. A mismatch
                    // with nobody holding the lock is still stale and falls
                    // through to the caller's rebuild.
                    orbit_common::tracing::debug!(
                        target: "orbit.store.task_query",
                        workspace_id = %self.workspace_id,
                        task_id = %row.task_id,
                        "task index freshness kept an in-flight commit envelope",
                    );
                }
                // Only observational scans tolerate stale rows, and the arm
                // above kept any row a writer holds: nobody is publishing this
                // one, so the caller reads its bundle instead of the row.
                Some(false) if tolerate_stale => stale.push(row.task_id.clone()),
                Some(false) => return Ok(IndexFreshness::Mismatch(row.task_id.clone())),
                None => unsettled.push(row.task_id.clone()),
            }
        }
        // Proofs only spare later scans a parse. Observation-only reads must
        // not even attempt to persist them through a read-only connection,
        // and a busy registry writer skips them rather than holding this
        // read's boundary section while it waits.
        if persist_proofs && !self.registry.is_read_only()? {
            let recorded = hold_step(ENVELOPE_STAMP_WRITE_STEP, || {
                self.registry
                    .record_envelope_stamps(&self.workspace_id, &proofs)
            });
            match recorded {
                Ok(true) => {}
                Ok(false) => orbit_common::tracing::debug!(
                    target: "orbit.store.task_bundle_v2",
                    workspace_id = %self.workspace_id,
                    "registry writer busy; skipped recording envelope stamps, so the next cold scan parses those envelopes",
                ),
                Err(error) => orbit_common::tracing::debug!(
                    target: "orbit.store.task_bundle_v2",
                    workspace_id = %self.workspace_id,
                    %error,
                    "could not record envelope stamps; the next cold scan parses those envelopes",
                ),
            }
        }
        if stale.is_empty() {
            Ok(IndexFreshness::Usable(unsettled))
        } else {
            Ok(IndexFreshness::UsableExceptStale { unsettled, stale })
        }
    }

    /// Stamp every row's envelope, in row order. This is one metadata call per
    /// registered task, the bulk of a scan over a large partition, and nothing
    /// is skipped. Only these lock-free calls leave the caller's thread: bundle
    /// reads stay on it, because file-lock re-entrancy is tracked per thread. A
    /// row with no stamp falls through to the strict envelope read.
    fn stamp_index_rows(&self, rows: &[Arc<TaskIndexProofRow>]) -> Vec<Option<EnvelopeStamp>> {
        let stamp_rows = |rows: &[Arc<TaskIndexProofRow>]| {
            rows.iter()
                .map(|row| {
                    let path = self.bundle_store.envelope_path(&row.task_id).ok()?;
                    self.envelope_cache.stamp(&path)
                })
                .collect::<Vec<_>>()
        };
        let threads = std::thread::available_parallelism()
            .map_or(1, |parallelism| parallelism.get())
            .min(MAX_STAMP_THREADS);
        if rows.len() < PARALLEL_STAMP_MIN_ROWS || threads < 2 {
            return stamp_rows(rows);
        }
        let stamp_rows = &stamp_rows;
        std::thread::scope(|scope| {
            let chunks = rows
                .chunks(rows.len().div_ceil(threads))
                .map(|chunk| {
                    let handle = std::thread::Builder::new()
                        .spawn_scoped(scope, move || stamp_rows(chunk))
                        .ok();
                    (chunk, handle)
                })
                .collect::<Vec<_>>();
            chunks
                .into_iter()
                .flat_map(|(chunk, handle)| match handle {
                    // A stamp is only an optimization, so a lost helper leaves
                    // its rows to the strict envelope read.
                    Some(handle) => handle.join().unwrap_or_else(|_| vec![None; chunk.len()]),
                    None => stamp_rows(chunk),
                })
                .collect()
        })
    }

    /// Whether one registered task's envelope matches its index row, reusing
    /// a previous parse or a recorded proof while the envelope file is
    /// unchanged. `None` carries the same meaning as
    /// [`TaskBundleStoreV2::read_envelope_if_settled`]: a concurrent writer
    /// holds this bundle, so the scan skips it rather than failing. A match
    /// the registry does not hold yet is added to `proofs`.
    fn settled_envelope_matches(
        &self,
        row: &Arc<TaskIndexProofRow>,
        // Taken before the parse it labels, so a write that races this read
        // costs one extra parse next scan instead of pinning stale content.
        stamp: Option<EnvelopeStamp>,
        proofs: &mut Vec<EnvelopeStampRecord>,
    ) -> Result<Option<bool>, OrbitError> {
        let task_id = &row.task_id;
        if stamp.is_none() {
            // Stamping drops the reason, so report an unusable id here.
            self.bundle_store.envelope_path(task_id)?;
        }
        if let Some(stamp) = &stamp {
            if let Some((matches, pending)) =
                self.envelope_cache.cached_index_match(task_id, stamp, row)
            {
                proofs.extend(pending);
                return Ok(Some(matches));
            }
            if let (Some(persisted), Some(recorded)) = (stamp.persisted(), &row.recorded)
                && recorded.stamp == persisted
                && recorded.fingerprint == row.fingerprint
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
        let matches = row.indexed.matches(&envelope);
        if matches && let Some(stamp) = &stamp {
            proofs.extend(envelope_cache::pending_envelope_proof(stamp, row));
        }
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
    /// rebuild the index from them when the registry is writable and the
    /// [`RepairGate`] admits it. Read-only registries never consult the gate.
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
        orbit_common::tracing::debug!(
            target: "orbit.store.task_query",
            workspace_id = %self.workspace_id,
            path = "bundle_scan",
            repair_admitted = ticket.is_some(),
            reason,
            "task index fallback",
        );
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
        if self.registry.is_read_only()? {
            return Ok(None);
        }
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
        let Err(error) = hold_step(INDEX_REPAIR_STEP, || {
            self.registry
                .replace_workspace_task_indexes(&self.workspace_id, &envelopes)
        }) else {
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

    pub(super) fn publish_task_index_and_lease_notices(
        &self,
        envelope: &TaskEnvelopeV2,
        operation: &str,
    ) -> Result<(), OrbitError> {
        if let Some(boundary) = &self.coordination {
            boundary.record_task_lease_overlaps(envelope)?;
        }
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
        Ok(())
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

    /// Run `op` under this task's shared bundle lock.
    ///
    /// The lock target belongs to the bundle store, which hands the same file
    /// to the exclusive lock its writes take, so a write and a concurrent read
    /// cannot disagree about what coordinates them. Concurrent readers of one
    /// task are admitted together.
    pub(crate) fn with_task_read_lock<T, F>(&self, id: &str, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        // Same lock order as writers, without requesting mutation authority.
        self.in_boundary(|| self.bundle_store.with_bundle_read_lock(id, op))
    }

    pub(crate) fn with_task_lock<T, F>(&self, id: &str, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        // Boundary first, bundle lock second, on every path. An admission
        // section holds the boundary exclusively and then takes bundle locks
        // inside it, so a caller that acquired them the other way round could
        // deadlock against it (ORB-12528).
        self.in_boundary(|| {
            #[cfg(feature = "test-support")]
            crate::repository::task::coordination::task_write_hold_hook::after_boundary_acquired();
            if let Some(boundary) = &self.coordination {
                boundary.refuse_unscoped_claim_write(id)?;
            }
            self.bundle_store.with_bundle_write_lock(id, op)
        })
    }

    /// [`Self::with_task_lock`] for an operator's comment-only write, the one
    /// unscoped write an execution claim admits [ORB-16723]. The lock itself
    /// does not refuse a protecting claim; `op` learns whether one protects
    /// the task. Every write inside still takes [`Self::with_task_lock`] or
    /// classifies itself as `update_task_history` does, so nothing but the
    /// comment lands under the claim.
    pub(crate) fn with_task_comment_lock<T, F>(&self, id: &str, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce(bool) -> Result<T, OrbitError>,
    {
        // Same boundary-then-bundle order as `with_task_lock` (ORB-12528).
        self.in_boundary(|| {
            let claimed = match &self.coordination {
                Some(boundary) => boundary.protecting_claim_for_task(id)?.is_some(),
                None => false,
            };
            self.bundle_store.with_bundle_write_lock(id, || op(claimed))
        })
    }
}
