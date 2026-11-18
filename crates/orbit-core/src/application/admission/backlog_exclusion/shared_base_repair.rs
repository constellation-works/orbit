//! A red pull request and the base repair of the failure it shares wait on
//! each other [ORB-25510].
//!
//! A PR task's check repair may widen its footprint (`file:Cargo.lock`) while
//! repairing a failure that, it later turns out, the base branch has too.
//! Its landing then waits for a green base (`pr_auto_fix_base_red`) while
//! still in `review`, so it keeps every lock it holds. The CI-failure sweep
//! files the base repair for that same failure, and admission defers the
//! repair behind the PR's widened lock. Neither can move: the PR waits for the
//! repair, the repair waits for the PR's lock.
//!
//! This module recognizes that cycle only when it is proven, and otherwise
//! leaves the ordinary lock conflict alone:
//!
//! - the waiting task is a CI-failure sweep repair whose creation recorded a
//!   failure identity with a normalized signature;
//! - its conflict is with a `review` task, on a selector that task's own
//!   recovery step appended (a `context_files_widened` entry with step
//!   `recovery`) and still declares;
//! - that task's newest `pr_auto_fix_base_red` record is known-red base
//!   evidence for its current head (no auto-fix push after it), and one of its
//!   checks carries the repair's exact signature.
//!
//! The lock itself is not touched: admission still grants no overlapping
//! write, and the PR keeps its active run, candidate and pull request. What
//! changes is that the wait is no longer silent. Every pass names both tasks,
//! the selector, the failure evidence and which task owns the repair in the
//! exclusion's `detail`, and the owner's admission pass records one
//! `shared_base_repair_wait` entry and one comment on the repair task
//! asking an operator to choose the single owner. A later pass with the same
//! holder, head, failure and selectors writes nothing again. Once the PR
//! lands or leaves `review`, or pushes a new head, the cycle is no longer
//! proven and the exclusion reads as an ordinary lock conflict again.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::fs::lock_mode::FootprintLockModes;
use orbit_types::task::{
    CiFailureIdentity, ContextFilesWidening, ContextWideningStep, Task, TaskComment,
    TaskHistoryEntry, TaskStatus,
};
use orbit_types::workflow::{PR_AUTO_FIX_PUSHED_EVENT, PrAutoFixBaseRed};
use serde::Serialize;
use serde_json::Value;

use super::{BacklogTaskExclusion, BacklogTaskExclusionReason};
use crate::OrbitRuntime;
use crate::application::contracts::CI_FAILURE_TAG;
use crate::application::task::{SYSTEM_ACTOR_LABEL, TaskRecordUpdateParams, TaskRecords};

/// History event on the repair task recording one proven shared-base wait
/// and the operator request it raised. The note is the wait's JSON.
pub(crate) const SHARED_BASE_REPAIR_WAIT_EVENT: &str = "shared_base_repair_wait";

/// One selector the repair requests that the holder's recovery added.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SharedBaseSelector {
    /// The repair task's own lock selector.
    pub(crate) requested_selector: String,
    /// The selector the holder's recovery appended.
    pub(crate) widened_selector: String,
    /// The run whose recovery step appended it.
    pub(crate) widened_by_run: String,
}

/// A proven circular wait between a base repair and a red PR task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SharedBaseRepairWait {
    /// The waiting CI-failure sweep repair: the owner of the shared failure.
    pub(crate) repair_task_id: String,
    /// The `review` task holding the widened lock while its landing waits.
    pub(crate) holding_task_id: String,
    pub(crate) selectors: Vec<SharedBaseSelector>,
    /// The repair's normalized failure signature, matched exactly.
    pub(crate) signature: String,
    pub(crate) failure_key: String,
    /// The PR head the holder's base-red record names.
    pub(crate) failed_head: String,
    pub(crate) base_branch: Option<String>,
    /// The base tip the holder's landing compared against.
    pub(crate) landed_base: Option<String>,
    /// The failing check names on that head with the shared signature.
    pub(crate) failing_checks: Vec<String>,
    /// The base commits whose same job failed with the same signature.
    pub(crate) base_commits: Vec<String>,
}

impl SharedBaseRepairWait {
    /// The identity a recorded request is deduplicated by.
    pub(crate) fn key(&self) -> String {
        let selectors = self
            .selectors
            .iter()
            .map(|selector| selector.widened_selector.as_str())
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{}|{}|{}|{}|{}",
            self.repair_task_id,
            self.holding_task_id,
            self.failed_head,
            self.failure_key,
            selectors
        )
    }

    fn selector_list(&self) -> String {
        self.selectors
            .iter()
            .map(|selector| format!("`{}`", selector.widened_selector))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn widening_runs(&self) -> String {
        let mut runs = self
            .selectors
            .iter()
            .map(|selector| selector.widened_by_run.as_str())
            .collect::<Vec<_>>();
        runs.sort_unstable();
        runs.dedup();
        runs.join(", ")
    }

    fn evidence(&self) -> String {
        let checks = if self.failing_checks.is_empty() {
            String::from("a required check")
        } else {
            self.failing_checks
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let base = match (&self.base_branch, self.base_commits.first()) {
            (Some(branch), Some(commit)) => format!("`{branch}` at {}", short(commit)),
            (Some(branch), None) => format!("`{branch}`"),
            (None, Some(commit)) => format!("base {}", short(commit)),
            (None, None) => String::from("its base"),
        };
        format!(
            "{checks} fails on head {} with `{}`, as on {base}",
            short(&self.failed_head),
            self.signature
        )
    }

    /// The exclusion detail every pass reports: both tasks, the selector, the
    /// failure evidence and which work can progress.
    pub(crate) fn detail(&self) -> String {
        format!(
            "shared base failure: {repair} repairs the base failure that keeps {holder} from \
             landing ({evidence}). {holder} is in review and holds {selectors}, which its \
             recovery run {runs} added, so {repair} waits on {holder} while {holder} waits on \
             {repair}. {repair} owns the repair; {holder} keeps its lock, candidate and pull \
             request while its landing waits for a green base. Neither moves until an operator \
             frees {holder}'s lock or chooses {holder} as the owner; the request is recorded on \
             {repair}.",
            repair = self.repair_task_id,
            holder = self.holding_task_id,
            evidence = self.evidence(),
            selectors = self.selector_list(),
            runs = self.widening_runs(),
        )
    }

    /// The one operator request recorded on the repair task.
    fn request(&self) -> String {
        format!(
            "Operator decision needed: {repair} and {holder} wait on each other.\n\n\
             - Failure: {evidence} (failure key `{key}`).\n\
             - Blocker: {holder} (review) holds {selectors}, added by its recovery run {runs}; \
             admission will not start {repair} while that lock is held.\n\
             - {holder} cannot land until this base failure is repaired.\n\n\
             Choose one owner for the repair:\n\
             1. Recommended: {repair} owns it. Free {holder}'s lock on {selectors} without \
             discarding its candidate or pull request (for example, stop its landing run). \
             A later admission pass can then start {repair}; once it lands, {holder}'s \
             candidate goes through its normal review and checks against the repaired base.\n\
             2. {holder} owns it: its pull request carries the repair through review and CI; \
             close {repair} once that lands.\n\
             3. Keep waiting: change nothing. Readiness and the drain keep naming this wait.\n\n\
             Orbit grants neither task an overlapping write and records this request once for \
             this head, failure and selector.",
            repair = self.repair_task_id,
            holder = self.holding_task_id,
            evidence = self.evidence(),
            key = self.failure_key,
            selectors = self.selector_list(),
            runs = self.widening_runs(),
        )
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// Annotate every lock-conflict exclusion that is a proven shared-base wait
/// and return the waits by repair task. With `record`, the owner's admission
/// pass also records each wait's request once. An unreadable history proves
/// nothing, so it leaves the exclusion as it was.
pub(super) fn annotate_shared_base_repair_waits(
    runtime: &OrbitRuntime,
    task_lookup: &BTreeMap<String, Task>,
    excluded: &mut [BacklogTaskExclusion],
    lock_modes: &FootprintLockModes,
    record: bool,
) -> BTreeMap<String, SharedBaseRepairWait> {
    let mut waits = BTreeMap::new();
    let mut holder_histories: BTreeMap<String, Option<Vec<TaskHistoryEntry>>> = BTreeMap::new();
    for exclusion in excluded.iter_mut() {
        if exclusion.reason != BacklogTaskExclusionReason::ContextLockConflict {
            continue;
        }
        let Some(repair) = task_lookup.get(&exclusion.id) else {
            continue;
        };
        if repair.status != TaskStatus::Backlog
            || !repair.tags.iter().any(|tag| tag == CI_FAILURE_TAG)
        {
            continue;
        }
        let repair_history = match runtime.get_task_history(&repair.id) {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(task_id = %repair.id, "could not read repair history: {error}");
                continue;
            }
        };
        let Some(identity) = CiFailureIdentity::from_history(&repair_history)
            .ok()
            .flatten()
        else {
            continue;
        };
        let mut holders = exclusion
            .conflicts
            .iter()
            .map(|conflict| conflict.locking_task_id.as_str())
            .filter(|holder| *holder != repair.id)
            .collect::<Vec<_>>();
        holders.sort_unstable();
        holders.dedup();
        let wait = holders.into_iter().find_map(|holder_id| {
            let holder = task_lookup.get(holder_id)?;
            let history = holder_histories
                .entry(holder_id.to_string())
                .or_insert_with(|| {
                    runtime
                        .get_task_history(holder_id)
                        .inspect_err(|error| {
                            tracing::warn!(
                                task_id = holder_id,
                                "could not read holder history: {error}"
                            );
                        })
                        .ok()
                })
                .as_deref()?;
            let requested = exclusion
                .conflicts
                .iter()
                .filter(|conflict| conflict.locking_task_id == holder_id)
                .map(|conflict| conflict.requested_file.as_str())
                .collect::<Vec<_>>();
            shared_base_wait(repair, &identity, &requested, holder, history, lock_modes)
        });
        let Some(wait) = wait else {
            continue;
        };
        exclusion.detail = Some(match exclusion.detail.take() {
            Some(existing) => format!("{} {existing}", wait.detail()),
            None => wait.detail(),
        });
        if record && let Err(error) = record_request_once(runtime, &wait) {
            tracing::warn!(
                task_id = %wait.repair_task_id,
                "could not record the shared base repair request: {error}"
            );
        }
        waits.insert(wait.repair_task_id.clone(), wait);
    }
    waits
}

/// The proven wait between `repair` and `holder`, or `None` when any link of
/// the proof is missing.
fn shared_base_wait(
    repair: &Task,
    identity: &CiFailureIdentity,
    requested: &[&str],
    holder: &Task,
    holder_history: &[TaskHistoryEntry],
    lock_modes: &FootprintLockModes,
) -> Option<SharedBaseRepairWait> {
    if holder.status != TaskStatus::Review {
        return None;
    }
    let signature = identity.signature.as_deref().filter(|s| !s.is_empty())?;
    let selectors = recovery_widened_overlaps(requested, holder, holder_history, lock_modes);
    if selectors.is_empty() {
        return None;
    }
    let base_red = current_known_red_base(holder_history)?;
    let shared = base_red
        .checks
        .iter()
        .flatten()
        .filter(|check| check["signature"].as_str() == Some(signature))
        .collect::<Vec<_>>();
    if shared.is_empty() {
        return None;
    }
    let collect = |field: fn(&Value) -> Option<&str>| {
        let mut values = shared
            .iter()
            .filter_map(|check| field(check).map(str::to_string))
            .collect::<Vec<_>>();
        values.sort();
        values.dedup();
        values
    };
    Some(SharedBaseRepairWait {
        repair_task_id: repair.id.clone(),
        holding_task_id: holder.id.clone(),
        selectors,
        signature: signature.to_string(),
        failure_key: identity.failure_key.clone(),
        failed_head: base_red.failed_head.clone(),
        base_branch: base_red.base_branch.clone(),
        landed_base: base_red.landed_base.clone(),
        failing_checks: collect(|check| check["failed_job"]["name"].as_str()),
        base_commits: collect(|check| check["base_commit"].as_str()),
    })
}

/// The repair's requested selectors that overlap a selector the holder's
/// recovery step appended and the holder still declares.
fn recovery_widened_overlaps(
    requested: &[&str],
    holder: &Task,
    holder_history: &[TaskHistoryEntry],
    lock_modes: &FootprintLockModes,
) -> Vec<SharedBaseSelector> {
    let mut selectors = Vec::new();
    for widening in holder_history
        .iter()
        .filter_map(ContextFilesWidening::from_entry)
        .filter(|widening| widening.step == ContextWideningStep::Recovery)
    {
        for widened in widening
            .selectors
            .iter()
            .filter(|widened| holder.context_files.contains(widened))
        {
            for requested in requested {
                if lock_modes.selectors_block(requested, widened) {
                    selectors.push(SharedBaseSelector {
                        requested_selector: (*requested).to_string(),
                        widened_selector: widened.clone(),
                        widened_by_run: widening.run_id.clone(),
                    });
                }
            }
        }
    }
    selectors.sort_by(|a, b| {
        (
            &a.widened_selector,
            &a.requested_selector,
            &a.widened_by_run,
        )
            .cmp(&(
                &b.widened_selector,
                &b.requested_selector,
                &b.widened_by_run,
            ))
    });
    selectors.dedup_by(|a, b| {
        a.widened_selector == b.widened_selector && a.requested_selector == b.requested_selector
    });
    selectors
}

/// The holder's newest base-red record when it is known-red base evidence
/// for the head still under landing: an auto-fix push after it moved the
/// head, so the record no longer describes the pull request.
fn current_known_red_base(history: &[TaskHistoryEntry]) -> Option<PrAutoFixBaseRed> {
    let (index, record) = history
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, entry)| PrAutoFixBaseRed::from_entry(entry).map(|r| (index, r)))?;
    if record.path.as_deref() != Some("known_red_base") {
        return None;
    }
    if history[index + 1..]
        .iter()
        .any(|entry| entry.event == PR_AUTO_FIX_PUSHED_EVENT)
    {
        return None;
    }
    Some(record)
}

/// Record `wait`'s request on the repair task unless the same wait is
/// already recorded. The check and the write share the task write lock.
fn record_request_once(
    runtime: &OrbitRuntime,
    wait: &SharedBaseRepairWait,
) -> Result<(), orbit_common::OrbitError> {
    runtime.ensure_coordination_task_write_permitted()?;
    let key = wait.key();
    let note =
        serde_json::to_string(&serde_json::json!({"key": key, "wait": wait})).map_err(|error| {
            orbit_common::OrbitError::Execution(format!("encode wait note: {error}"))
        })?;
    runtime
        .stores()
        .tasks()
        .with_task_write_lock(&wait.repair_task_id, &mut || {
            let recorded = runtime
                .get_task_history(&wait.repair_task_id)?
                .iter()
                .filter(|entry| entry.event == SHARED_BASE_REPAIR_WAIT_EVENT)
                .filter_map(|entry| entry.note.as_deref())
                .filter_map(|note| serde_json::from_str::<Value>(note).ok())
                .any(|note| note["key"].as_str() == Some(key.as_str()));
            if recorded {
                return Ok(());
            }
            let at = Utc::now();
            runtime.stores().task_records().update(
                &wait.repair_task_id,
                TaskRecordUpdateParams {
                    actor: SYSTEM_ACTOR_LABEL.to_string(),
                    append_history: vec![TaskHistoryEntry {
                        at,
                        by: SYSTEM_ACTOR_LABEL.to_string(),
                        event: SHARED_BASE_REPAIR_WAIT_EVENT.to_string(),
                        note: Some(note.clone()),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: None,
                    }],
                    append_comments: vec![TaskComment {
                        at,
                        by: SYSTEM_ACTOR_LABEL.to_string(),
                        message: wait.request(),
                        operation_id: None,
                    }],
                    ..Default::default()
                },
            )?;
            Ok(())
        })
}
