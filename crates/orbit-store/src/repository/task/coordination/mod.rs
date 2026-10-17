//! The recoverable task/reservation commit boundary.
//!
//! # Why this exists
//!
//! A task lives in three places that cannot commit together: its bundle files,
//! the registry index, and the reservation database. Admission has to publish a
//! status transition, its history, and a reservation as *one* fact, and it has
//! to read readiness under the same serialization that ordinary task and
//! reservation writes take — otherwise a task can be admitted from state that
//! changed between the check and the write.
//!
//! # The protocol
//!
//! 1. **Serialize.** Every participant enters one advisory lock per task-store
//!    partition. Ordinary task reads/writes and ordinary reservation writes
//!    take it *shared*, so they keep running concurrently with each other; an
//!    admission section takes it *exclusive*. The lock is re-entrant per
//!    thread ([`orbit_common::fs::io`]), so a nested task lock inside an
//!    admission section runs under the outer acquisition instead of
//!    self-deadlocking. Every participant takes the boundary before any bundle
//!    lock, so the two locks are always acquired in the same order.
//! 2. **Prepare.** Under the task's bundle lock, which the committer keeps
//!    until step 4 ends, name the task's commit lease: a file in the
//!    partition's `.task-commit-applying/` that the committer keeps locked as
//!    long. Then insert a `prepared` journal row. Nothing about the task has
//!    changed yet.
//! 3. **Decide.** One SQLite transaction inserts the reservation and dependent
//!    coordination rows and flips the journal row to `committed`. This is the
//!    commit point and the only place the decision exists. `task.yaml` is then
//!    published without syncs, so lock-free envelope readers see the decision.
//! 4. **Apply.** Once the outermost admission section has released the
//!    partition, roll the decision forward onto the bundle: truncate
//!    `events.jsonl` to its recorded pre-apply length, append the intent's
//!    events, republish `task.yaml` durably, settle the journal row
//!    `applied`, update the registry index, unname the lease and release the
//!    bundle lock. Only steps 2 and 3 exclude the partition, and they request
//!    no file syncs.
//!
//! A failure before step 3 leaves no reservation, no coordination row, and an
//! untouched bundle: compensation only has to abandon an undecided row. A
//! failure after step 3 leaves a durable decision that step 4 replays
//! verbatim, as many times as it takes. Nothing consults the bundle to infer
//! whether a commit happened, and nothing tries to un-publish a published
//! envelope.
//!
//! # One composition per partition
//!
//! Every runtime composition participates in the same filesystem locks. A
//! durable required marker prevents legacy task stores from accessing a
//! partition after coordinated composition has activated it. A host lock,
//! always taken before the partition lock, additionally protects dependency
//! reads across workspace partitions. Ordinary sections, claim mutations,
//! commits and an admission whose decision reads only its own partition hold
//! it shared, so they never stall another partition's work. Only an admission
//! that reads a dependency another partition holds takes it exclusively, and
//! only for that decision.
//!
//! # Recovery before exposure
//!
//! A lease nobody holds is the cheap signal that a commit was interrupted: a
//! live committer holds its lease's lock until it has applied, and the kernel
//! drops a dead process's locks but not its files. A participant that finds a
//! dead lease before it reads, or once it holds the partition, takes the
//! boundary exclusively and replays the journal before it reads or writes
//! anything, leaving rows whose leases are still held to their committers.
//! A bundle reader or writer that meets a dead lease under the bundle lock is
//! refused until then, and one that meets a live apply waits for it on the
//! bundle lock, so no caller sees a committed transition half-applied. A
//! lease's name is not synced, so a host crash may lose it while its decision
//! survives; opening the boundary replays any unsettled row whose lease
//! nobody holds. A compensation or replay that fails leaves the lease dead
//! and returns the error: the partition stays closed until recovery succeeds. A
//! pending marker an older version left is still honored: a participant that
//! sees it waits for the partition as a reader first, as that version's
//! commit held the partition until it cleared the marker.
//!
//! # Diagnosing contention
//!
//! Every section names itself to the locks it takes: its kind (ordinary,
//! admission, recovery, ...) and the call site that entered the boundary.
//! Exclusive and shared holders both record that label with their pid and
//! acquisition time, so a waiter's contention warning and a lock timeout name
//! whoever holds the lock, and a section that holds it past a threshold logs
//! its label and held duration on release.
//!
//! # Who takes the boundary
//!
//! Coordinated task mutations enter through `TaskV2Store::in_boundary` or
//! `TaskV2Store::with_task_lock` (boundary first, then the bundle lock).
//! Coordinated reservation mutations enter through
//! `SqliteTaskReservationStoreBackend::in_boundary`, including methods that
//! look like getters but lazily mark expired rows released
//! (`list_active_task_reservations`, `show_workspace_claim`). The one
//! deliberate exception is `inspect_active_task_reservations`: it is a true
//! read (it does not expire rows) so `orbit doctor` stays strictly
//! read-only, and it therefore stays outside the boundary.

use std::path::PathBuf;

use orbit_types::task::{TaskEnvelopeV2, TaskEventRowV2};
use serde::{Deserialize, Serialize};

use crate::Store;
use crate::driver::sqlite::task_registry::TaskRegistryStore;
use crate::repository::task::v2_bundle::TaskBundleStoreV2;

/// Lock target for one task-store partition. The advisory lock is its
/// dot-prefixed sibling, so this name is never itself created or removed.
const COORDINATION_LOCK_FILE: &str = "task-commit";
const COORDINATION_LOCK_LABEL: &str = "task commit boundary";
/// Written by older versions from the first durable step of a commit until
/// it had been applied or abandoned, all inside its admission section.
/// Recovery still honors one such a version left behind, and clears it.
const PENDING_MARKER_FILE: &str = ".task-commit-pending";
const REQUIRED_MARKER_FILE: &str = ".task-commit-required";
const COMMIT_INTENT_SCHEMA_VERSION: u32 = 2;

/// The bundle-side half of a commit, replayed verbatim by recovery.
///
/// `events_len` is the length of `events.jsonl` before the apply, so a replay
/// after a partial append is deterministic: truncate, then append exactly
/// these rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskCommitIntent {
    schema_version: u32,
    task_id: String,
    events_len: u64,
    events: Vec<TaskEventRowV2>,
    envelope: TaskEnvelopeV2,
    #[serde(default)]
    evidence: lifecycle::EvidenceIntent,
}

/// One task-store partition's durable commit and recovery authority.
///
/// Compositions for the same partition share filesystem locks and the durable
/// journal. Legacy compositions refuse activated partitions. See the module docs for
/// the protocol and [`TaskCommitBoundary::commit_task_transition`] for the
/// integration entry point.
pub struct TaskCommitBoundary {
    store: Store,
    registry: TaskRegistryStore,
    bundle_store: TaskBundleStoreV2,
    workspace_id: String,
    partition_dir: PathBuf,
    /// Deadlines and holder diagnostics for the host and partition locks.
    lock_options: orbit_common::fs::io::FileLockOptions,
}

mod admission;
mod apply;
mod boundary;
mod branch_observation;
mod commit;
mod handoff;
mod landing;
mod lifecycle;
mod owner_wave;
mod repair;
mod selection;

#[cfg(test)]
mod tests;

// Shared with sibling modules through `super::`.
use boundary::{BoundaryDepth, Section};

pub use admission::admission_refusal;
#[cfg(feature = "test-support")]
pub(crate) use boundary::lock_timeout_hook;
