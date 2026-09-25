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
//! 2. **Prepare.** Write a durable pending marker in the partition directory,
//!    then insert a `prepared` journal row. Nothing about the task has changed
//!    yet.
//! 3. **Decide.** One SQLite transaction inserts the reservation and dependent
//!    coordination rows and flips the journal row to `committed`. This is the
//!    commit point and the only place the decision exists.
//! 4. **Apply.** Roll the decision forward onto the bundle: truncate
//!    `events.jsonl` to its recorded pre-apply length, append the intent's
//!    events, republish `task.yaml`, settle the journal row `applied`, and
//!    remove the marker.
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
//! partition after coordinated composition has activated it. A shared host
//! lock additionally protects dependency reads across workspace partitions;
//! admission takes it exclusively before its partition lock.
//!
//! # Recovery before exposure
//!
//! The pending marker is the cheap signal that a commit is in flight or was
//! interrupted. Any participant that sees it takes the boundary exclusively
//! and replays the journal before it reads or writes anything, so no caller
//! observes a committed reservation whose task transition has not landed. A
//! compensation or replay that fails leaves the marker in place and returns
//! the error: the partition stays closed until recovery succeeds.
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
/// Present from the first durable step of a commit until the commit has been
/// applied or abandoned.
const PENDING_MARKER_FILE: &str = ".task-commit-pending";
const REQUIRED_MARKER_FILE: &str = ".task-commit-required";
const COMMIT_INTENT_SCHEMA_VERSION: u32 = 2;

/// Injection points for the durability tests. Each names a moment the process
/// can die and a recovery obligation that follows from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CoordinationFault {
    /// After the marker is durable, before the journal row exists.
    AfterMarker,
    /// After the undecided journal row is durable, before the commit point.
    BeforeCommit,
    /// After the commit point, before any bundle file changes.
    AfterCommit,
    /// Midway through rolling a committed decision onto the bundle.
    DuringApply,
    /// After evidence documents/comments, before artifact and envelope publication.
    DuringEvidenceApply,
    /// While abandoning an undecided commit.
    DuringCompensation,
    /// While replaying the journal on the next entry.
    DuringRecovery,
}

#[cfg(test)]
thread_local! {
    static BEFORE_ORDINARY_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static INJECTED_FAULTS: std::cell::RefCell<std::collections::HashSet<CoordinationFault>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

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
}

mod admission;
mod boundary;
mod commit;
mod faults;
mod handoff;
mod landing;
mod lifecycle;

// Shared with sibling modules and the tests through `super::`.
use boundary::BoundaryDepth;
use faults::fail_if_injected;

pub use admission::admission_refusal;
#[cfg(test)]
pub(crate) use faults::inject_coordination_faults;

#[cfg(test)]
mod tests;
