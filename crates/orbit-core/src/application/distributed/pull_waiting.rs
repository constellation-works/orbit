//! What a pull drain records of the backlog its owner kept off this host
//! [ORB-14475].
//!
//! The owner decides admission, so the only account a follower has of what
//! waits in the owner's backlog is the diagnostics of the receipt the owner
//! returned. This reads them into the same [`DrainAdmissionPass`] fields a
//! local drain fills, so `orbit run show` and the dashboard print the same
//! `Still waiting` lines for both.

use std::collections::BTreeMap;

use orbit_store::contracts::{AdmissionDiagnostic, AdmissionReceipt};
use orbit_types::workflow::DrainWaitingTask;

/// Reason codes a pull drain records. The first, third and fourth are the
/// codes a local drain's classifier uses for the same cause.
pub(crate) const FOOTPRINT_HELD: &str = "context_lock_conflict";
pub(crate) const DEPENDENCY_NOT_DONE: &str = "dependency_not_done";
pub(crate) const HOST_OS_MISMATCH: &str = "host_os_mismatch";
pub(crate) const CREW_UNAVAILABLE: &str = "crew_unavailable";
pub(crate) const OWNER_HOLD: &str = "owner_hold";
pub(crate) const INVALID_CANDIDATE: &str = "invalid_candidate";

/// How many tasks each of `deferred` and `excluded` lists; the full counts
/// are recorded beside them, so a truncated list still reads as truncated.
const LISTED: usize = 20;

/// One owner answer, read as a drain's last-pass backlog fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PullWaiting {
    pub(crate) queued: u64,
    pub(crate) deferred: Vec<DrainWaitingTask>,
    pub(crate) deferred_total: u64,
    pub(crate) excluded: Vec<DrainWaitingTask>,
    pub(crate) excluded_total: u64,
    /// Every task kept off this host by reason code: the deferred and the
    /// excluded, whole.
    pub(crate) by_reason: BTreeMap<String, u64>,
}

/// What the owner said to the requests one pull pass sent.
#[derive(Debug, Clone, Copy)]
pub(crate) enum OwnerAnswer<'a> {
    /// The pass sent no request: throttled, held, breaker open, the owner
    /// unreachable, or its window closed.
    None,
    /// Every request the pass sent claimed a task. A claiming receipt lists
    /// only what the scan skipped on the way to it, so it does not replace
    /// the backlog view an idle answer gave.
    Claimed,
    /// The last request was answered idle: the receipt names everything the
    /// owner left in its backlog.
    Idle(&'a AdmissionReceipt),
}

impl PullWaiting {
    /// The tasks the owner left in its backlog, from its idle `receipt`.
    ///
    /// - `queued` is the owner's `queue_depth`: backlog tasks it could start.
    /// - `deferred` is the tasks the owner is holding back for a footprint, a
    ///   hold or a rule it keeps; `excluded` is the dependency, OS and crew
    ///   refusals, each with its reason and the tasks it waits on.
    pub(crate) fn of(receipt: &AdmissionReceipt) -> Self {
        let mut by_reason = BTreeMap::new();
        let mut count = |reason: &str| *by_reason.entry(reason.to_string()).or_insert(0_u64) += 1;
        let deferred_all = receipt
            .deferred_conflicts
            .iter()
            .map(|diagnostic| {
                let code = if diagnostic.blocked_by.is_empty() {
                    OWNER_HOLD
                } else {
                    FOOTPRINT_HELD
                };
                count(code);
                waiting_task(diagnostic, code)
            })
            .collect::<Vec<_>>();
        let excluded_all = receipt
            .invalid_candidates
            .iter()
            .map(|diagnostic| {
                let code = if diagnostic.blocked_by.is_empty() {
                    INVALID_CANDIDATE
                } else {
                    DEPENDENCY_NOT_DONE
                };
                (diagnostic, code)
            })
            .chain(
                receipt
                    .os_unavailable
                    .iter()
                    .map(|diagnostic| (diagnostic, HOST_OS_MISMATCH)),
            )
            .chain(
                receipt
                    .crew_unavailable
                    .iter()
                    .map(|diagnostic| (diagnostic, CREW_UNAVAILABLE)),
            )
            .map(|(diagnostic, code)| {
                count(code);
                waiting_task(diagnostic, code)
            })
            .collect::<Vec<_>>();
        Self {
            queued: receipt.queue_depth as u64,
            deferred_total: deferred_all.len() as u64,
            excluded_total: excluded_all.len() as u64,
            deferred: deferred_all.into_iter().take(LISTED).collect(),
            excluded: excluded_all.into_iter().take(LISTED).collect(),
            by_reason,
        }
    }

    /// How many tasks this answer says are kept off this host.
    pub(crate) fn kept_off(&self) -> u64 {
        self.by_reason.values().sum()
    }
}

/// A footprint holder or an unfinished dependency is named in `blocked_by`,
/// so the owner's sentence adds nothing for those; every other reason keeps
/// the sentence as `detail`.
fn waiting_task(diagnostic: &AdmissionDiagnostic, code: &str) -> DrainWaitingTask {
    let named = matches!(code, FOOTPRINT_HELD | DEPENDENCY_NOT_DONE);
    DrainWaitingTask {
        task_id: diagnostic.task_id.clone(),
        reason: Some(code.to_string()),
        blocked_by: diagnostic.blocked_by.clone(),
        detail: (!named).then(|| diagnostic.reason.clone()),
    }
}
