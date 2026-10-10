//! Retention for the host store: audit rows and unreferenced audit blobs
//! (`orbit gc audit`), and the pipeline state of old terminal runs
//! (`orbit gc runs`).
//!
//! Both plan by default and delete only when asked. Deletes run in batches of
//! [`RETENTION_BATCH_ROWS`], each its own short write transaction, with a
//! pause between them so other writers are not starved while a long history
//! is pruned. Neither runs `VACUUM`: freed pages stay on the freelist, which
//! each report shows.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use orbit_common::storage::blob_sweep::{self, AuditBlobRoot, SweepEntry, SweepOutcome};
use orbit_store::contracts::{
    AuditRetentionTable, RetentionSelection, StoreRetentionBackend, StoreSpace,
};
use serde::Serialize;

use crate::{OrbitError, OrbitRuntime};

/// Rows one retention write transaction deletes or archives at most.
const RETENTION_BATCH_ROWS: usize = 1_000;
/// Pause between batches, so a queued writer gets the lock.
const BATCH_PAUSE: Duration = Duration::from_millis(20);
/// A blob written or reused this recently is never swept, whatever the
/// reference scan saw: it covers a write published while the scan ran.
const BLOB_GRACE: Duration = Duration::from_secs(24 * 60 * 60);

/// Rows a cutoff selects in one table, and how many an apply removed.
#[derive(Debug, Clone, Serialize)]
pub struct RetentionTableReport {
    pub table: &'static str,
    /// `host` for the command audit, `workspace` for workspace-keyed tables.
    pub scope: &'static str,
    pub rows: u64,
    pub bytes: u64,
    pub rows_removed: u64,
}

/// One audit blob root: what is stored, what nothing names, and what an apply
/// removed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BlobSweepReport {
    pub root: String,
    pub blobs: u64,
    pub bytes: u64,
    /// Blobs no remaining row and no pending marker names.
    pub unreferenced: u64,
    pub unreferenced_bytes: u64,
    /// Blobs a pending marker protects: written, not yet named by a row.
    pub pending: u64,
    /// Blobs written or reused inside the grace window.
    pub recent: u64,
    /// Markers older than the audit cutoff, left by writes never published.
    pub stale_markers: u64,
    pub removed: u64,
    pub removed_bytes: u64,
    /// Candidates a writer claimed between the scan and their removal.
    pub retained_at_removal: u64,
    pub markers_removed: u64,
}

/// Page accounting of the store file after the command.
#[derive(Debug, Clone, Serialize)]
pub struct StoreSpaceReport {
    pub file_bytes: u64,
    pub freelist_pages: u64,
    /// Bytes deleted rows freed inside the file. Only `VACUUM` returns them
    /// to the filesystem.
    pub freelist_bytes: u64,
}

impl From<StoreSpace> for StoreSpaceReport {
    fn from(space: StoreSpace) -> Self {
        Self {
            file_bytes: space.page_size.saturating_mul(space.page_count),
            freelist_pages: space.freelist_pages,
            freelist_bytes: space.page_size.saturating_mul(space.freelist_pages),
        }
    }
}

/// Write-batch accounting for an apply.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct BatchReport {
    pub batches: u64,
    /// The longest single write transaction.
    pub max_batch_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditGcReport {
    pub apply: bool,
    pub retention_days: u32,
    pub cutoff: DateTime<Utc>,
    pub tables: Vec<RetentionTableReport>,
    pub blobs: BlobSweepReport,
    pub writes: BatchReport,
    pub store: StoreSpaceReport,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunGcReport {
    pub apply: bool,
    pub retention_days: u32,
    pub cutoff: DateTime<Utc>,
    /// Terminal runs past the cutoff that still keep pipeline state.
    pub runs: u64,
    pub state_bytes: u64,
    pub runs_archived: u64,
    pub writes: BatchReport,
    pub store: StoreSpaceReport,
}

/// What retention could reclaim now, measured without the blob reference scan
/// so `orbit doctor` stays cheap.
#[derive(Debug, Clone, Serialize)]
pub struct StoreRetentionOverview {
    pub audit_days: u32,
    pub runs_days: u32,
    pub audit_rows: u64,
    pub audit_bytes: u64,
    pub run_states: u64,
    pub run_state_bytes: u64,
    pub blob_bytes: u64,
    /// Blobs older than the audit cutoff that no pending marker protects: an
    /// upper bound on what the sweep removes, since a newer row may still
    /// name one.
    pub blob_bytes_past_cutoff: u64,
    pub store: StoreSpaceReport,
}

/// The configured retention windows.
struct RetentionDays {
    audit: u32,
    runs: u32,
}

impl OrbitRuntime {
    fn retention_days(&self) -> Result<RetentionDays, OrbitError> {
        let config = orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::new(
            self.global_root(),
            self.shared_root(),
        ))?;
        Ok(RetentionDays {
            audit: config.snapshot.retention_audit_days,
            runs: config.snapshot.retention_runs_days,
        })
    }

    /// Plan, or with `apply` perform, audit retention: rows of the command
    /// audit and of this workspace's run audit older than the cutoff, then
    /// the blobs under this workspace's audit root that nothing names.
    /// `retention_days` overrides `retention.audit_days`.
    pub fn gc_audit(
        &self,
        apply: bool,
        retention_days: Option<u32>,
    ) -> Result<AuditGcReport, OrbitError> {
        let days = resolve_override(retention_days, self.retention_days()?.audit)?;
        let cutoff = cutoff_for(days)?;
        let store = self.sqlite_store()?;
        let workspace_id = self.workspace_id()?;
        let mut writes = BatchReport::default();
        let mut tables = Vec::new();
        for (table, scope) in [
            (AuditRetentionTable::Command, "host"),
            (AuditRetentionTable::Run, "workspace"),
        ] {
            let RetentionSelection { rows, bytes } =
                store.audit_retention_selection(table, &workspace_id, cutoff)?;
            let rows_removed = if apply && rows > 0 {
                run_batches(&mut writes, || {
                    store.prune_audit_retention_batch(
                        table,
                        &workspace_id,
                        cutoff,
                        RETENTION_BATCH_ROWS,
                    )
                })?
            } else {
                0
            };
            tables.push(RetentionTableReport {
                table: table.table_name(),
                scope,
                rows,
                bytes,
                rows_removed,
            });
        }
        let blobs = self.sweep_audit_blobs(&store, apply, &workspace_id, cutoff)?;
        Ok(AuditGcReport {
            apply,
            retention_days: days,
            cutoff,
            tables,
            blobs,
            writes,
            store: store.store_space()?.into(),
        })
    }

    /// Plan, or with `apply` perform, run retention: drop the pipeline state
    /// of this workspace's terminal runs that finished before the cutoff and
    /// stamp their `archived_at`. Run rows and steps stay, so `run show`, run
    /// history and the scoreboard keep working. `retention_days` overrides
    /// `retention.runs_days`.
    pub fn gc_runs(
        &self,
        apply: bool,
        retention_days: Option<u32>,
    ) -> Result<RunGcReport, OrbitError> {
        let days = resolve_override(retention_days, self.retention_days()?.runs)?;
        let cutoff = cutoff_for(days)?;
        let store = self.sqlite_store()?;
        let workspace_id = self.workspace_id()?;
        let RetentionSelection { rows, bytes } =
            store.run_state_retention_selection(&workspace_id, cutoff)?;
        let mut writes = BatchReport::default();
        let runs_archived = if apply && rows > 0 {
            let archived_at = Utc::now();
            run_batches(&mut writes, || {
                store.archive_run_states_batch(
                    &workspace_id,
                    cutoff,
                    archived_at,
                    RETENTION_BATCH_ROWS,
                )
            })?
        } else {
            0
        };
        Ok(RunGcReport {
            apply,
            retention_days: days,
            cutoff,
            runs: rows,
            state_bytes: bytes,
            runs_archived,
            writes,
            store: store.store_space()?.into(),
        })
    }

    /// Reclaimable audit, run-state and blob bytes under the configured
    /// windows, without scanning blob references.
    pub fn store_retention_overview(&self) -> Result<StoreRetentionOverview, OrbitError> {
        let days = self.retention_days()?;
        let audit_cutoff = cutoff_for(days.audit)?;
        let store = self.sqlite_store()?;
        let workspace_id = self.workspace_id()?;
        let mut audit_rows = 0;
        let mut audit_bytes = 0;
        for table in [AuditRetentionTable::Command, AuditRetentionTable::Run] {
            let selection = store.audit_retention_selection(table, &workspace_id, audit_cutoff)?;
            audit_rows += selection.rows;
            audit_bytes += selection.bytes;
        }
        let runs = store.run_state_retention_selection(&workspace_id, cutoff_for(days.runs)?)?;
        let root = AuditBlobRoot::new(&self.paths().audit_dir);
        let past_cutoff = SystemTime::from(audit_cutoff);
        let marked: HashSet<String> = root
            .pending()?
            .into_iter()
            .filter(|marker| marker.modified >= past_cutoff)
            .map(|marker| marker.hash)
            .collect();
        let mut blob_bytes = 0;
        let mut blob_bytes_past_cutoff = 0;
        for blob in root.blobs()? {
            blob_bytes += blob.bytes;
            if blob.modified < past_cutoff && !marked.contains(&blob.hash) {
                blob_bytes_past_cutoff += blob.bytes;
            }
        }
        Ok(StoreRetentionOverview {
            audit_days: days.audit,
            runs_days: days.runs,
            audit_rows,
            audit_bytes,
            run_states: runs.rows,
            run_state_bytes: runs.bytes,
            blob_bytes,
            blob_bytes_past_cutoff,
            store: store.store_space()?.into(),
        })
    }

    /// Find, and with `apply` remove, the blobs under this workspace's audit
    /// root that no remaining row names. A plan skips the rows the prune
    /// would delete, so both modes count the same blobs.
    fn sweep_audit_blobs(
        &self,
        store: &dyn StoreRetentionBackend,
        apply: bool,
        workspace_id: &str,
        cutoff: DateTime<Utc>,
    ) -> Result<BlobSweepReport, OrbitError> {
        let root = AuditBlobRoot::new(&self.paths().audit_dir);
        let _lock = if apply {
            let lock = root.try_lock()?.ok_or_else(|| {
                OrbitError::Execution(format!(
                    "another audit blob sweep is running under {}",
                    self.paths().audit_dir.display()
                ))
            })?;
            root.recover_quarantine()?;
            Some(lock)
        } else {
            None
        };
        let fresh_after = SystemTime::now()
            .checked_sub(BLOB_GRACE)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let marker_stale_before = SystemTime::from(cutoff);
        let mut report = BlobSweepReport {
            root: root.blobs_dir().display().to_string(),
            ..BlobSweepReport::default()
        };
        // Markers are read before references: a write that publishes after
        // this read refreshed its blob's mtime, which keeps it out of
        // `candidates` or is seen by the recheck at removal.
        let (fresh_markers, stale_markers): (Vec<_>, Vec<_>) = root
            .pending()?
            .into_iter()
            .partition(|marker| marker.modified >= marker_stale_before);
        let marked: HashSet<&str> = fresh_markers.iter().map(|m| m.hash.as_str()).collect();
        let mut candidates: HashMap<String, SweepEntry> = HashMap::new();
        for blob in root.blobs()? {
            report.blobs += 1;
            report.bytes += blob.bytes;
            if marked.contains(blob.hash.as_str()) {
                report.pending += 1;
            } else if blob.modified >= fresh_after {
                report.recent += 1;
            } else {
                candidates.insert(blob.hash.clone(), blob);
            }
        }
        report.stale_markers = stale_markers.len() as u64;
        if !candidates.is_empty() {
            store.visit_blob_reference_text(Some((workspace_id, cutoff)), &mut |text| {
                blob_sweep::for_each_blob_hash(text, |hash| {
                    candidates.remove(hash);
                });
            })?;
        }
        report.unreferenced = candidates.len() as u64;
        report.unreferenced_bytes = candidates.values().map(|blob| blob.bytes).sum();
        if !apply {
            return Ok(report);
        }
        for blob in candidates.values() {
            match root.remove_unless_protected(blob, fresh_after, marker_stale_before)? {
                SweepOutcome::Removed => {
                    report.removed += 1;
                    report.removed_bytes += blob.bytes;
                }
                SweepOutcome::Retained => report.retained_at_removal += 1,
                SweepOutcome::Missing => {}
            }
        }
        for marker in &stale_markers {
            if root.remove_stale_marker(marker, marker_stale_before)? {
                report.markers_removed += 1;
            }
        }
        Ok(report)
    }
}

fn resolve_override(requested: Option<u32>, configured: u32) -> Result<u32, OrbitError> {
    match requested {
        Some(0) => Err(OrbitError::InvalidInput(
            "--older-than-days must be at least 1".to_string(),
        )),
        Some(days) => Ok(days),
        None => Ok(configured),
    }
}

fn cutoff_for(days: u32) -> Result<DateTime<Utc>, OrbitError> {
    chrono::Duration::try_days(i64::from(days))
        .and_then(|window| Utc::now().checked_sub_signed(window))
        .ok_or_else(|| OrbitError::InvalidInput(format!("a {days}-day window is too large")))
}

/// Run `batch` until it removes fewer than a full batch, recording each write
/// transaction's duration. Returns the total removed.
fn run_batches(
    writes: &mut BatchReport,
    mut batch: impl FnMut() -> Result<usize, OrbitError>,
) -> Result<u64, OrbitError> {
    let mut total = 0u64;
    loop {
        let started = Instant::now();
        let removed = batch()?;
        let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        writes.batches += 1;
        writes.max_batch_ms = writes.max_batch_ms.max(elapsed);
        total += removed as u64;
        if removed < RETENTION_BATCH_ROWS {
            return Ok(total);
        }
        std::thread::sleep(BATCH_PAUSE);
    }
}
