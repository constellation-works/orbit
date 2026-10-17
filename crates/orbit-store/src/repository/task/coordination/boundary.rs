//! Entering the boundary: construction and journal binding, the ordinary
//! (shared) and admission (exclusive) sections, and per-thread boundary depth.

use super::{
    COORDINATION_LOCK_FILE, COORDINATION_LOCK_LABEL, REQUIRED_MARKER_FILE, TaskCommitBoundary,
};
use crate::Store;
use crate::driver::sqlite::task_registry::TaskRegistryStore;
use crate::repository::task::v2_bundle::TaskBundleStoreV2;
use orbit_common::OrbitError;
use orbit_common::fs::io::{
    FileLockOptions, atomic_write_text, create_private_dir_all, hold_step,
    with_exclusive_file_lock_options, with_shared_file_lock_options,
};
use orbit_common::fs::lock_retry::retry_busy_acquisition;
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A boundary section held this long logs its label and duration on release:
/// long enough to stay quiet for a healthy section, short enough to name one
/// that keeps waiters queued toward the 3 s contention warning and the 30 s
/// acquisition deadline. The report also names the section's slowest step —
/// the pending-recovery check, a lock wait, or a step its operation timed
/// with [`hold_step`] — so a long hold says where it went.
const SECTION_HOLD_WARN_AFTER: Duration = Duration::from_secs(2);

/// The step a section's operation is timed as, less whatever steps it timed
/// inside itself.
const OPERATION_STEP: &str = "operation";
/// The step that settles an interrupted commit before a section runs.
const PENDING_RECOVERY_STEP: &str = "pending recovery";

/// Lock policy for the host and partition locks. Shared holders record
/// themselves, so a waiter blocked by ordinary sections can name them, and a
/// queued admission or recovery holds back ordinary sections that arrive
/// after it. Without that priority an exclusive waiter got the partition only
/// at an instant when no ordinary section held it, and under drain load
/// admissions waited past the 3 s warning behind a stream of overlapping
/// readers (ORB-15107).
fn boundary_lock_options() -> FileLockOptions {
    FileLockOptions {
        record_shared_holders: true,
        warn_held_after: Some(SECTION_HOLD_WARN_AFTER),
        prefer_exclusive_waiters: true,
        ..FileLockOptions::default()
    }
}

/// The boundary section taking a lock: its kind and the call site that
/// entered it. Its label is what the lock's holder record, a waiter's
/// contention warning and a long hold's release report name.
#[derive(Debug, Clone, Copy)]
pub(super) struct Section {
    kind: &'static str,
    caller: &'static Location<'static>,
}

impl Section {
    #[track_caller]
    pub(super) fn here(kind: &'static str) -> Self {
        Self {
            kind,
            caller: Location::caller(),
        }
    }

    fn label(self) -> String {
        format!(
            "{COORDINATION_LOCK_LABEL}: {} at {}:{}",
            self.kind,
            self.caller.file(),
            self.caller.line()
        )
    }
}

fn shared_section<T>(
    target: &Path,
    section: Section,
    mut options: FileLockOptions,
    op: impl FnOnce() -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    // A section nested inside another partition's section already holds that
    // partition, which a writer queued here may be waiting behind indirectly:
    // queueing this read behind that writer could close a cycle that plain
    // shared locks never had. Only an outermost section yields to a writer.
    // The depth counts partition sections only, so the host lock every
    // section takes first does not exempt its partition read. A status-only
    // observation never yields ([`TaskCommitBoundary::observe_for_status`]).
    if BoundaryDepth::any() || StatusObservation::active() {
        options.prefer_exclusive_waiters = false;
    }
    with_shared_file_lock_options(target, &section.label(), options, op)
}

/// Run `op` once `enter` has taken its section's locks. An entry that lost
/// to a held lock before it reached `op` is made again while this thread's
/// lock-busy retry scope has time left ([`retry_busy_acquisition`]), so a
/// late delivery step outlasts a long hold instead of failing at the first
/// deadline. Nothing ran under the lock, so the repeat decides nothing twice.
/// A failure `op` itself returns is never retried here.
fn entered<T>(
    op: impl FnOnce() -> Result<T, OrbitError>,
    mut enter: impl FnMut(&mut dyn FnMut() -> Result<T, OrbitError>) -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    let mut op = Some(op);
    retry_busy_acquisition(|| {
        let result = enter(&mut || {
            let op = op.take().ok_or_else(|| {
                OrbitError::Store("boundary operation was already consumed".into())
            })?;
            op()
        });
        match result {
            Err(error) if op.is_some() => Err(error),
            result => Ok(result),
        }
    })
}

fn exclusive_section<T>(
    target: &Path,
    section: Section,
    options: FileLockOptions,
    op: impl FnOnce() -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    with_exclusive_file_lock_options(target, &section.label(), options, op)
}

thread_local! {
    /// Depth of boundary sections this thread is inside. Recovery must never
    /// run *inside* a commit this thread is performing: that commit's own
    /// journal row is legitimately unsettled.
    static BOUNDARY_DEPTH: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub(super) struct BoundaryDepth;

impl BoundaryDepth {
    pub(super) fn enter(partition: &Path) -> Self {
        BOUNDARY_DEPTH.with(|depth| depth.borrow_mut().push(partition.to_path_buf()));
        Self
    }

    pub(super) fn active(partition: &Path) -> bool {
        BOUNDARY_DEPTH.with(|depth| depth.borrow().iter().any(|held| held == partition))
    }

    /// Whether this thread is inside any partition's boundary section.
    fn any() -> bool {
        BOUNDARY_DEPTH.with(|depth| !depth.borrow().is_empty())
    }
}

impl Drop for BoundaryDepth {
    fn drop(&mut self) {
        BOUNDARY_DEPTH.with(|depth| {
            depth.borrow_mut().pop();
        });
    }
}

thread_local! {
    /// Whether this thread is inside
    /// [`TaskCommitBoundary::observe_for_status`].
    static STATUS_OBSERVATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct StatusObservation {
    outer: bool,
}

impl StatusObservation {
    fn enter() -> Self {
        Self {
            outer: STATUS_OBSERVATION.with(|active| active.replace(true)),
        }
    }

    fn active() -> bool {
        STATUS_OBSERVATION.with(std::cell::Cell::get)
    }
}

impl Drop for StatusObservation {
    fn drop(&mut self) {
        STATUS_OBSERVATION.with(|active| active.set(self.outer));
    }
}

fn host_lock_for_partition(partition: &Path) -> PathBuf {
    // Partitions are tasks/workspaces/<id>. Keep host metadata beside the
    // registry database, outside the directory enumerated as workspaces.
    partition
        .ancestors()
        .nth(2)
        .unwrap_or(partition)
        .join("host-task-commit")
}

impl TaskCommitBoundary {
    pub fn new(
        store: Store,
        registry: TaskRegistryStore,
        workspace_id: String,
    ) -> Result<Self, OrbitError> {
        let partition_dir = registry.workspace_partition_dir(&workspace_id)?;
        create_private_dir_all(&partition_dir)
            .map_err(|error| OrbitError::from_write_io(&partition_dir, error))?;
        let boundary = Self {
            bundle_store: TaskBundleStoreV2::new(registry.clone(), workspace_id.clone()),
            store,
            registry,
            workspace_id,
            partition_dir,
            lock_options: boundary_lock_options(),
        };
        // Activation writes the marker once and nothing removes it, so a
        // bound partition is verified without the lock. Every runtime open
        // used to take the partition lock exclusively here, queueing behind
        // every ordinary section in the partition until a sweep or CLI open
        // timed out under drain load (ORB-15088).
        if boundary
            .partition_dir
            .join(REQUIRED_MARKER_FILE)
            .try_exists()?
        {
            boundary.verify_journal_binding()?;
            return Ok(boundary);
        }
        boundary.exclusive(
            &boundary.lock_target(),
            Section::here("activation"),
            || -> Result<(), OrbitError> {
                let marker = boundary.partition_dir.join(REQUIRED_MARKER_FILE);
                if marker.try_exists()? {
                    boundary.verify_journal_binding()?;
                } else {
                    let path = serde_json::to_string(&boundary.store.task_commit_database_path()?)
                        .map_err(|error| OrbitError::Store(error.to_string()))?;
                    atomic_write_text(&marker, &path)
                        .map_err(|error| OrbitError::from_write_io(&marker, error))?;
                }
                Ok(())
            },
        )?;
        Ok(boundary)
    }

    /// Observation-only handle: no directory creation, lock files, or marker
    /// writes. It still refuses a partition bound to a different coordination
    /// journal, which only reads the marker.
    pub fn for_observation(
        store: Store,
        registry: TaskRegistryStore,
        workspace_id: String,
    ) -> Result<Self, OrbitError> {
        let partition_dir = registry.workspace_partition_dir(&workspace_id)?;
        let boundary = Self {
            bundle_store: TaskBundleStoreV2::new(registry.clone(), workspace_id.clone()),
            store,
            registry,
            workspace_id,
            partition_dir,
            lock_options: boundary_lock_options(),
        };
        boundary.verify_journal_binding()?;
        Ok(boundary)
    }

    pub(super) fn verify_journal_binding(&self) -> Result<(), OrbitError> {
        let marker = self.partition_dir.join(REQUIRED_MARKER_FILE);
        if marker.try_exists()? {
            let path: PathBuf = serde_json::from_str(&std::fs::read_to_string(marker)?)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if path != self.store.task_commit_database_path()? {
                return Err(OrbitError::Store(
                    "task partition is bound to a different coordination journal".into(),
                ));
            }
        }
        Ok(())
    }

    /// Legacy compositions may serve an uncoordinated partition, but cannot
    /// race or overwrite one that has opted into durable coordination.
    #[track_caller]
    pub(crate) fn enter_uncoordinated<T>(
        registry: &TaskRegistryStore,
        workspace_id: &str,
        op: impl FnOnce() -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        let section = Section::here("uncoordinated");
        let options = boundary_lock_options();
        let partition = registry.workspace_partition_dir(workspace_id)?;
        let host_lock = host_lock_for_partition(&partition);
        shared_section(&host_lock, section, options, || {
            shared_section(
                &partition.join(COORDINATION_LOCK_FILE),
                section,
                options,
                || {
                    if partition.join(REQUIRED_MARKER_FILE).try_exists()? {
                        return Err(OrbitError::Store(
                            "this task partition requires coordinated backends".into(),
                        ));
                    }
                    op()
                },
            )
        })
    }

    /// The task-store partition this boundary serializes.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Run an ordinary task or reservation operation inside the boundary.
    ///
    /// Shared with every other ordinary participant and excluded by an
    /// admission section. Reads take it too, so a caller cannot observe a
    /// reservation whose task transition is still being applied.
    #[track_caller]
    pub fn enter_ordinary<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        let section = Section::here("ordinary");
        entered(op, |op| {
            self.shared(&self.host_lock_target(), section, || {
                self.enter_ordinary_locked(section, op)
            })
        })
    }

    fn enter_ordinary_locked<T>(
        &self,
        section: Section,
        op: impl FnOnce() -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        let mut op = Some(op);
        loop {
            let result = self.shared(&self.lock_target(), section, || {
                // A live commit holds the partition exclusively until it has
                // cleared its marker, so a marker this read still sees once it
                // gets in belongs to an interrupted commit. Drop the shared
                // acquisition before taking recovery exclusive. Checking first
                // and entering after made a reader that met a live commit wait
                // for the partition twice, the second time behind any writer
                // that had queued meanwhile (ORB-15797).
                if !BoundaryDepth::active(&self.partition_dir)
                    && self.pending_marker_path().try_exists()?
                {
                    return Ok(None);
                }
                let _depth = BoundaryDepth::enter(&self.partition_dir);
                let operation = op.take().ok_or_else(|| {
                    OrbitError::Store("ordinary boundary operation was already consumed".into())
                })?;
                hold_step(OPERATION_STEP, operation).map(Some)
            })?;
            if let Some(result) = result {
                return Ok(result);
            }
            hold_step(PENDING_RECOVERY_STEP, || self.recover_pending_in(section))?;
        }
    }

    /// Run a status-only observation whose ordinary sections on this thread
    /// pass a queued admission, recovery or commit instead of waiting behind
    /// it at the turnstile.
    ///
    /// For a diagnostic that decides and writes nothing, such as routine
    /// status. Its sections still take the shared locks and settle a pending
    /// commit first, so it never observes a half-applied transition and still
    /// waits for a section that holds a lock exclusively. Only the queued
    /// writers' hold times leave its wait: one status pass entered dozens of
    /// sections, each queued behind every admission and commit of a drain,
    /// and took seconds. A queued writer may in turn wait for the sections
    /// this observation enters while it is queued, which a status pass bounds
    /// to a short sequence of reads; every other ordinary section keeps
    /// yielding to it (ORB-15107).
    pub fn observe_for_status<T>(op: impl FnOnce() -> T) -> T {
        let _observation = StatusObservation::enter();
        op()
    }

    /// Hold this partition exclusively for one admission decision.
    ///
    /// Readiness, dependencies within the partition, conflicts, and the
    /// commit itself run inside `op`, so nothing an ordinary write could
    /// change moves underneath the decision. Calling
    /// [`Self::commit_task_transition`] inside `op` re-enters the same
    /// acquisition.
    ///
    /// The host lock is held shared, as an ordinary section holds it, so
    /// ordinary sections in every other partition keep running while this
    /// one decides. Excluding them all for every claim mutation and commit
    /// stalled every workspace's task reads and writes on the host behind
    /// one drain's admissions (ORB-15106). `op` must therefore read only
    /// this partition; an admission whose decision reads another
    /// partition's dependency takes a host-wide section instead.
    #[track_caller]
    pub fn with_admission<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        let section = Section::here("admission");
        entered(op, |op| {
            self.shared(&self.host_lock_target(), section, || {
                self.admission_locked(section, op)
            })
        })
    }

    /// [`Self::with_admission`] that also excludes every other partition's
    /// ordinary sections, for a decision that reads a dependency another
    /// workspace partition holds: nothing an ordinary write anywhere on the
    /// host could change moves underneath that read.
    ///
    /// The host lock is never upgraded in place, so this is refused inside
    /// any boundary section on this thread: a nested request would run under
    /// the outer section's shared hold.
    #[track_caller]
    pub(super) fn with_host_admission<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        let section = Section::here("host admission");
        if BoundaryDepth::any() {
            return Err(OrbitError::Store(
                "a host-wide admission cannot nest inside another boundary section".into(),
            ));
        }
        entered(op, |op| {
            self.exclusive(&self.host_lock_target(), section, || {
                self.admission_locked(section, op)
            })
        })
    }

    fn admission_locked<T>(
        &self,
        section: Section,
        op: impl FnOnce() -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        self.exclusive(&self.lock_target(), section, || {
            #[cfg(test)]
            let _probe = section_probe::SectionProbe::enter(section.kind);
            hold_step(PENDING_RECOVERY_STEP, || self.recover_pending_in(section))?;
            let _depth = BoundaryDepth::enter(&self.partition_dir);
            hold_step(OPERATION_STEP, op)
        })
    }

    /// Dependent coordination rows published for this partition, by kind.
    ///
    /// Settles an interrupted commit first, so a caller reading back its own
    /// receipts cannot miss one that a crashed commit had already decided.
    pub fn coordination_rows(
        &self,
        kind: &str,
    ) -> Result<Vec<crate::contracts::TaskCoordinationRow>, OrbitError> {
        self.enter_ordinary(|| self.store.task_coordination_rows(&self.workspace_id, kind))
    }

    /// Dependent coordination rows of `kind` whose payload carries `value` as
    /// a JSON string, settling an interrupted commit first. Every row with a
    /// string field equal to `value` is among them, so a caller decodes only
    /// these and keeps its exact matches instead of decoding every row of the
    /// kind inside its section.
    pub(super) fn coordination_rows_mentioning(
        &self,
        kind: &str,
        value: &str,
    ) -> Result<Vec<crate::contracts::TaskCoordinationRow>, OrbitError> {
        // Payloads are compact `serde_json` encodings, which escape a string
        // the same way wherever it sits.
        let needle =
            serde_json::to_string(value).map_err(|error| OrbitError::Store(error.to_string()))?;
        self.enter_ordinary(|| {
            self.store
                .task_coordination_rows_containing(&self.workspace_id, kind, &needle)
        })
    }

    /// One dependent coordination row by identity, settling an interrupted
    /// commit first like [`Self::coordination_rows`].
    pub fn coordination_row(
        &self,
        kind: &str,
        row_id: &str,
    ) -> Result<Option<crate::contracts::TaskCoordinationRow>, OrbitError> {
        self.enter_ordinary(|| {
            self.store
                .task_coordination_row(&self.workspace_id, kind, row_id)
        })
    }

    /// Hold `target` shared for `section`, under this boundary's lock policy.
    pub(super) fn shared<T>(
        &self,
        target: &Path,
        section: Section,
        op: impl FnOnce() -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        shared_section(target, section, self.lock_options, op)
    }

    /// Hold `target` exclusively for `section`, under this boundary's lock
    /// policy.
    pub(super) fn exclusive<T>(
        &self,
        target: &Path,
        section: Section,
        op: impl FnOnce() -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        exclusive_section(target, section, self.lock_options, op)
    }

    /// The same boundary with another lock policy, so a test can contend
    /// within milliseconds instead of the production deadlines.
    #[cfg(test)]
    pub(crate) fn with_lock_options(mut self, options: FileLockOptions) -> Self {
        self.lock_options = options;
        self
    }

    pub(super) fn host_lock_target(&self) -> PathBuf {
        host_lock_for_partition(&self.partition_dir)
    }

    pub(super) fn lock_target(&self) -> PathBuf {
        self.partition_dir.join(COORDINATION_LOCK_FILE)
    }
}

/// What each outermost admission section on this thread cost, so tests can
/// bound the work done while other task writers wait, and tell a
/// partition-scoped section from a host-wide one.
#[cfg(test)]
pub(crate) mod section_probe {
    use std::cell::{Cell, RefCell};
    use std::time::{Duration, Instant};

    use orbit_common::fs::io::durability_syncs_requested;

    use super::super::lifecycle::STATE_DECODES;
    use crate::repository::task::v2_bundle::{CANONICAL_BUNDLE_READS, LIGHTWEIGHT_BUNDLE_READS};

    /// One exclusive section: its kind (`admission` holds the host lock
    /// shared, `host admission` exclusively), how long it was held, the
    /// bundles read in it, the durability syncs it requested and the claim
    /// lifecycle rows it decoded.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct SectionCost {
        pub(crate) kind: &'static str,
        pub(crate) held: Duration,
        pub(crate) lightweight_reads: u64,
        pub(crate) canonical_reads: u64,
        pub(crate) durability_syncs: u64,
        pub(crate) state_decodes: u64,
    }

    thread_local! {
        static DEPTH: Cell<u32> = const { Cell::new(0) };
        pub(crate) static SECTIONS: RefCell<Vec<SectionCost>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) struct SectionProbe {
        kind: &'static str,
        started: Instant,
        lightweight: u64,
        canonical: u64,
        syncs: u64,
        state_decodes: u64,
    }

    impl SectionProbe {
        pub(super) fn enter(kind: &'static str) -> Self {
            DEPTH.with(|depth| depth.set(depth.get() + 1));
            Self {
                kind,
                started: Instant::now(),
                lightweight: LIGHTWEIGHT_BUNDLE_READS.with(Cell::get),
                canonical: CANONICAL_BUNDLE_READS.with(Cell::get),
                syncs: durability_syncs_requested(),
                state_decodes: STATE_DECODES.with(Cell::get),
            }
        }
    }

    impl Drop for SectionProbe {
        fn drop(&mut self) {
            let outermost = DEPTH.with(|depth| {
                depth.set(depth.get() - 1);
                depth.get() == 0
            });
            if outermost {
                let cost = SectionCost {
                    kind: self.kind,
                    held: self.started.elapsed(),
                    lightweight_reads: LIGHTWEIGHT_BUNDLE_READS.with(Cell::get) - self.lightweight,
                    canonical_reads: CANONICAL_BUNDLE_READS.with(Cell::get) - self.canonical,
                    durability_syncs: durability_syncs_requested() - self.syncs,
                    state_decodes: STATE_DECODES.with(Cell::get) - self.state_decodes,
                };
                SECTIONS.with(|sections| sections.borrow_mut().push(cost));
            }
        }
    }
}
