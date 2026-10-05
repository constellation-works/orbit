//! Bounded automatic repair of the generated task index.
//!
//! A read that finds the index stale serves itself from a bundle scan and
//! rebuilds the index from the same bundles. When the rebuild is rejected —
//! a canonical relation names a task the registry no longer has, say — the
//! bundles stay the same on the next read, so the same rebuild fails the
//! same way: one full-workspace attempt and one warning per request, in
//! every store a process opens over the registry (ORB-14181).
//!
//! The gate records each failure against the evidence the attempt read —
//! every registered task and its envelope stamp, taken before the bundles —
//! and the targets it found unresolved. While that evidence holds, reads
//! scan without attempting. Any change re-admits one attempt: a supported
//! write lands a new envelope inode, a create or delete changes the
//! registered set, and registering a recorded target restores it. A retry
//! interval bounds the suppression even when nothing observable changed, so
//! a failure is never cached for good.
//!
//! State is per process and keyed by registry and workspace, so every store
//! instance that reopens the same partition shares one record, and at most
//! one read per key attempts a repair at a time. The gate never touches a
//! lock: a reader holds no registry or bundle lock while it consults it,
//! and the repair itself keeps the registry's own transaction.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::envelope_cache::EnvelopeStamp;

/// How long a rejected rebuild suppresses retries over unchanged bundles.
const REJECTED_RETRY_INTERVAL: Duration = Duration::from_secs(600);
/// How long any other failure — a busy or unwritable registry — does.
const STORE_RETRY_INTERVAL: Duration = Duration::from_secs(15);

type GateKey = (PathBuf, String);

static GATES: LazyLock<Mutex<HashMap<GateKey, GateState>>> = LazyLock::new(Default::default);

/// What a repair attempt reads: every registered task and its envelope stamp
/// (`None` while it cannot be stamped), captured before the bundle scan so a
/// write racing the scan shows up as changed evidence on the next read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct RepairEvidence {
    pub(super) envelopes: BTreeMap<String, Option<EnvelopeStamp>>,
}

struct Failure {
    evidence: RepairEvidence,
    unresolved_targets: BTreeSet<String>,
    retry_at: Instant,
    suppressed_reads: u64,
}

#[derive(Default)]
struct GateState {
    in_flight: bool,
    failure: Option<Failure>,
    #[cfg(test)]
    attempts: u64,
}

/// One registry partition's repair record.
pub(super) struct RepairGate {
    key: GateKey,
}

impl RepairGate {
    pub(super) fn new(registry_root: &Path, workspace_id: &str) -> Self {
        Self {
            key: (registry_root.to_path_buf(), workspace_id.to_string()),
        }
    }

    /// The targets the recorded failure found unresolved, for the caller to
    /// check against the registry before [`Self::admit`].
    pub(super) fn unresolved_targets(&self) -> BTreeSet<String> {
        gates()
            .get(&self.key)
            .and_then(|state| state.failure.as_ref())
            .map(|failure| failure.unresolved_targets.clone())
            .unwrap_or_default()
    }

    /// Admit one repair attempt, or `None` when another read is repairing
    /// this partition or the recorded failure still holds: same evidence,
    /// no recorded target restored, retry interval not yet elapsed.
    pub(super) fn admit(
        &self,
        evidence: RepairEvidence,
        target_restored: bool,
    ) -> Option<RepairTicket> {
        let mut gates = gates();
        let state = gates.entry(self.key.clone()).or_default();
        if state.in_flight {
            return None;
        }
        if let Some(failure) = &mut state.failure
            && failure.evidence == evidence
            && !target_restored
            && Instant::now() < failure.retry_at
        {
            failure.suppressed_reads += 1;
            return None;
        }
        state.in_flight = true;
        #[cfg(test)]
        {
            state.attempts += 1;
        }
        Some(RepairTicket {
            key: self.key.clone(),
            evidence: Some(evidence),
        })
    }

    #[cfg(test)]
    pub(super) fn attempts(&self) -> u64 {
        gates().get(&self.key).map_or(0, |state| state.attempts)
    }

    #[cfg(test)]
    pub(super) fn suppressed_reads(&self) -> u64 {
        gates()
            .get(&self.key)
            .and_then(|state| state.failure.as_ref())
            .map_or(0, |failure| failure.suppressed_reads)
    }

    #[cfg(test)]
    pub(super) fn expire_failure(&self) {
        if let Some(failure) = gates()
            .get_mut(&self.key)
            .and_then(|state| state.failure.as_mut())
        {
            failure.retry_at = Instant::now();
        }
    }
}

/// The right to run one repair. Dropping it unsettled — the bundle scan
/// failed, say — releases the partition without recording anything, so the
/// next read tries again.
pub(super) struct RepairTicket {
    key: GateKey,
    evidence: Option<RepairEvidence>,
}

impl RepairTicket {
    /// The rebuild published: forget any recorded failure.
    pub(super) fn succeeded(mut self) {
        self.evidence = None;
        if let Some(state) = gates().get_mut(&self.key) {
            state.in_flight = false;
            state.failure = None;
        }
    }

    /// The rebuild was refused. `rejected` marks a validator verdict over the
    /// bundles, which only a bundle or registry change can alter. Returns
    /// how many reads the previous record suppressed, for the warning.
    pub(super) fn failed(mut self, rejected: bool, unresolved_targets: BTreeSet<String>) -> u64 {
        let Some(evidence) = self.evidence.take() else {
            return 0;
        };
        let interval = if rejected {
            REJECTED_RETRY_INTERVAL
        } else {
            STORE_RETRY_INTERVAL
        };
        let mut gates = gates();
        let state = gates.entry(self.key.clone()).or_default();
        state.in_flight = false;
        let suppressed = state
            .failure
            .as_ref()
            .map_or(0, |failure| failure.suppressed_reads);
        state.failure = Some(Failure {
            evidence,
            unresolved_targets,
            retry_at: Instant::now() + interval,
            suppressed_reads: 0,
        });
        suppressed
    }
}

impl Drop for RepairTicket {
    fn drop(&mut self) {
        if self.evidence.is_none() {
            return;
        }
        if let Some(state) = gates().get_mut(&self.key) {
            state.in_flight = false;
        }
    }
}

/// A poisoned lock only means a thread panicked mid-update; every record is
/// advisory (the worst case is one extra or one skipped attempt), so reads
/// keep going rather than failing on it.
fn gates() -> MutexGuard<'static, HashMap<GateKey, GateState>> {
    GATES.lock().unwrap_or_else(PoisonError::into_inner)
}
