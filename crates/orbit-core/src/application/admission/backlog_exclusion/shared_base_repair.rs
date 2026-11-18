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
//! - that task's current head (no auto-fix push after the record) fails with
//!   the repair's exact signature, by either typed record the auto-fix
//!   watcher writes on it: the `pr_auto_fix_head_failure` its admitted check
//!   repair was handed, or a `known_red_base` `pr_auto_fix_base_red` hold.
//!   The sweep repair's creation identity is itself the evidence that the
//!   base fails with that signature, so the two records together prove the
//!   failure is shared.
//!
//! The motivating incident (ORB-25470 and ORB-25479) is the first shape: the
//! base was not yet proven red when auto-fix admitted the check repair, the
//! repair widened `file:Cargo.lock` and could not clear the failure, and the
//! run held in run state only. The head-failure record is what that hold now
//! leaves on the task.
//!
//! The lock itself is not touched: admission still grants no overlapping
//! write, and the PR keeps its active run, candidate and pull request. What
//! changes is that the wait is no longer silent. Every pass names both tasks,
//! the selector, the failure evidence and which task owns the repair in the
//! exclusion's `detail`. The owner's admission pass also files one human-crew
//! task and raises one `agent_request` decision on it that names the blocker
//! and asks the operator to choose the single owner; its id is recorded on the
//! repair as a `shared_base_repair_wait` entry, so a later pass with the same
//! holder, head, failure and selectors raises nothing again. Answering it
//! records the choice and completes that task; nothing is applied to either
//! task automatically. Without a configured human crew no request can be
//! held, so the detail says so and nothing is recorded. Once the PR lands or
//! leaves `review`, or pushes a new head, the cycle is no longer proven and
//! the exclusion reads as an ordinary lock conflict again.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::lock_mode::FootprintLockModes;
use orbit_types::task::{
    AgentDecisionOptionInput, CiFailureIdentity, ContextFilesWidening, ContextWideningStep, Task,
    TaskComplexity, TaskHistoryEntry, TaskOrigin, TaskPriority, TaskRelation, TaskRelationType,
    TaskStatus, TaskType,
};
use orbit_types::workflow::{PR_AUTO_FIX_PUSHED_EVENT, PrAutoFixBaseRed, PrAutoFixHeadFailure};
use serde::Serialize;
use serde_json::Value;

use super::{BacklogTaskExclusion, BacklogTaskExclusionReason};
use crate::OrbitRuntime;
use crate::application::contracts::CI_FAILURE_TAG;
use crate::application::task::{
    AgentDecisionRaise, SYSTEM_ACTOR_LABEL, TaskAddParams, TaskRecordUpdateParams, TaskRecords,
};

/// History event on the repair task recording one proven shared-base wait
/// and the decision request it raised. The note is the wait's JSON.
pub(crate) const SHARED_BASE_REPAIR_WAIT_EVENT: &str = "shared_base_repair_wait";

/// Tag on the human-crew task that holds a shared-base owner decision.
pub(crate) const SHARED_BASE_REPAIR_DECISION_TAG: &str = "shared-base-repair-decision";

/// The filer the decision request is raised as.
const SHARED_BASE_REPAIR_FILER: &str = "orbit-admission";

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

/// Which typed record on the holder proved its head fails as the base does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SharedFailureProof {
    /// The holder's `pr_auto_fix_head_failure`: the failure its admitted
    /// check repair was handed, matched to the sweep's base identity.
    HeadFailure,
    /// The holder's `known_red_base` `pr_auto_fix_base_red` hold.
    KnownRedBase,
}

/// The operator request a proven wait raised, as recorded on the repair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SharedBaseDecision {
    /// The human-crew task holding the decision.
    pub(crate) task_id: String,
    /// The decision request on it.
    pub(crate) request_id: String,
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
    pub(crate) proof: SharedFailureProof,
    /// The PR head the holder's record names.
    pub(crate) failed_head: String,
    pub(crate) base_branch: Option<String>,
    /// The failing check names on that head with the shared signature.
    pub(crate) failing_checks: Vec<String>,
    /// The base commits seen failing with the same signature: the sweep's
    /// tested commit, and any base commit a known-red record names.
    pub(crate) base_commits: Vec<String>,
    /// The operator request, once one is raised or found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) decision: Option<SharedBaseDecision>,
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

    fn request_state(&self) -> String {
        match &self.decision {
            Some(decision) => format!(
                "the owner decision is decision request {} on {}",
                decision.request_id, decision.task_id
            ),
            None => String::from(
                "no owner decision is recorded yet: the owner's admission pass raises one on a \
                 human-crew task, and needs a configured human crew to hold it",
            ),
        }
    }

    /// The exclusion detail every pass reports: both tasks, the selector, the
    /// failure evidence, which work can progress and the decision to answer.
    pub(crate) fn detail(&self) -> String {
        format!(
            "shared base failure: {repair} repairs the base failure that keeps {holder} from \
             landing ({evidence}). {holder} is in review and holds {selectors}, which its \
             recovery run {runs} added, so {repair} waits on {holder} while {holder} waits on \
             {repair}. {repair} owns the repair; {holder} keeps its lock, candidate and pull \
             request while its landing waits for a green base. Neither moves until an operator \
             frees {holder}'s lock or chooses {holder} as the owner; {state}.",
            repair = self.repair_task_id,
            holder = self.holding_task_id,
            evidence = self.evidence(),
            selectors = self.selector_list(),
            runs = self.widening_runs(),
            state = self.request_state(),
        )
    }

    /// The question of the one decision request, within the raise's bound.
    fn question(&self) -> String {
        format!(
            "{repair} and {holder} wait on each other over {selectors}: {holder} cannot land \
             until the shared base failure `{key}` is repaired, and admission will not start \
             {repair} while {holder} holds that lock. Which task owns the repair?",
            repair = self.repair_task_id,
            holder = self.holding_task_id,
            selectors = self.selector_list(),
            key = self.failure_key,
        )
    }

    fn options(&self) -> Vec<AgentDecisionOptionInput> {
        let repair = &self.repair_task_id;
        let holder = &self.holding_task_id;
        vec![
            AgentDecisionOptionInput {
                label: format!("{repair} owns the repair; free {holder}'s lock"),
                consequence: format!(
                    "Stop {holder}'s held landing without discarding its candidate or pull \
                     request; {repair} then starts, and {holder} completes against the repaired base"
                ),
                risk_tier: None,
                recommended: true,
            },
            AgentDecisionOptionInput {
                label: format!("{holder}'s pull request carries the repair"),
                consequence: format!(
                    "{holder} repairs the shared failure through its own review and CI; close \
                     {repair} once {holder} lands"
                ),
                risk_tier: None,
                recommended: false,
            },
            AgentDecisionOptionInput {
                label: String::from("Keep both waiting"),
                consequence: String::from(
                    "Nothing changes; readiness and the drain keep naming this wait",
                ),
                risk_tier: None,
                recommended: false,
            },
        ]
    }

    fn decision_task_description(&self) -> String {
        format!(
            "Admission found a circular wait between a CI-failure sweep repair and a pull \
             request task that holds a lock the repair needs.\n\n\
             - Repair: {repair} (backlog), failure key `{key}`.\n\
             - Holder: {holder} (review) holds {selectors}, added by its recovery run {runs}.\n\
             - Failure: {evidence}.\n\n\
             Answer the decision request on this task to choose the single owner. Orbit grants \
             neither task an overlapping write, keeps {holder}'s candidate and pull request, \
             and applies nothing on its own: the chosen owner's next step stays an operator \
             action through the usual run, review and CI.",
            repair = self.repair_task_id,
            holder = self.holding_task_id,
            key = self.failure_key,
            selectors = self.selector_list(),
            runs = self.widening_runs(),
            evidence = self.evidence(),
        )
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// Annotate every lock-conflict exclusion that is a proven shared-base wait
/// and return the waits by repair task. With `record`, the owner's admission
/// pass also raises each wait's decision request once. An unreadable history
/// proves nothing, so it leaves the exclusion as it was.
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
        let Some(mut wait) = wait else {
            continue;
        };
        wait.decision = recorded_decision(&repair_history, &wait.key());
        if record && wait.decision.is_none() {
            match raise_decision_once(runtime, &wait) {
                Ok(decision) => wait.decision = decision,
                Err(error) => tracing::warn!(
                    task_id = %wait.repair_task_id,
                    "could not raise the shared base repair decision: {error}"
                ),
            }
        }
        exclusion.detail = Some(match exclusion.detail.take() {
            Some(existing) => format!("{} {existing}", wait.detail()),
            None => wait.detail(),
        });
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
    let failure = current_shared_failure(holder_history, signature)?;
    let mut base_commits = failure.base_commits;
    if !identity.tested_commit.is_empty() {
        base_commits.insert(0, identity.tested_commit.clone());
    }
    base_commits.dedup();
    Some(SharedBaseRepairWait {
        repair_task_id: repair.id.clone(),
        holding_task_id: holder.id.clone(),
        selectors,
        signature: signature.to_string(),
        failure_key: identity.failure_key.clone(),
        proof: failure.proof,
        failed_head: failure.failed_head,
        base_branch: failure.base_branch,
        failing_checks: failure.checks,
        base_commits,
        decision: None,
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

/// The holder's head failure carrying `signature`, read from its typed
/// auto-fix records.
struct SharedFailure {
    proof: SharedFailureProof,
    failed_head: String,
    base_branch: Option<String>,
    checks: Vec<String>,
    base_commits: Vec<String>,
}

/// The failure of the head still under landing that carries `signature`.
///
/// Only records after the holder's last auto-fix push count: a push moved
/// the head, so earlier records no longer describe the pull request. Of
/// those, the newest head-bound record names the current head, and only a
/// record for that head can prove the failure.
fn current_shared_failure(history: &[TaskHistoryEntry], signature: &str) -> Option<SharedFailure> {
    let since_push = history
        .iter()
        .rposition(|entry| entry.event == PR_AUTO_FIX_PUSHED_EVENT)
        .map_or(0, |index| index + 1);
    let current = &history[since_push..];
    let current_head = current.iter().rev().find_map(|entry| {
        PrAutoFixHeadFailure::from_entry(entry)
            .map(|record| record.failed_head)
            .or_else(|| PrAutoFixBaseRed::from_entry(entry).map(|record| record.failed_head))
    })?;
    for entry in current.iter().rev() {
        if let Some(record) = PrAutoFixBaseRed::from_entry(entry)
            && record.failed_head == current_head
            && record.path.as_deref() == Some("known_red_base")
        {
            let shared = record
                .checks
                .iter()
                .flatten()
                .filter(|check| check["signature"].as_str() == Some(signature))
                .collect::<Vec<_>>();
            if !shared.is_empty() {
                let collect = |field: fn(&Value) -> Option<&str>| {
                    let mut values = shared
                        .iter()
                        .filter_map(|check| field(check).map(str::to_string))
                        .collect::<Vec<_>>();
                    values.sort();
                    values.dedup();
                    values
                };
                return Some(SharedFailure {
                    proof: SharedFailureProof::KnownRedBase,
                    failed_head: record.failed_head.clone(),
                    base_branch: record.base_branch.clone(),
                    checks: collect(|check| check["failed_job"]["name"].as_str()),
                    base_commits: collect(|check| check["base_commit"].as_str()),
                });
            }
        }
        if let Some(record) = PrAutoFixHeadFailure::from_entry(entry)
            && record.failed_head == current_head
        {
            let mut checks = record
                .checks
                .iter()
                .filter(|check| check.signature == signature)
                .map(|check| check.check.clone())
                .collect::<Vec<_>>();
            if !checks.is_empty() {
                checks.sort();
                checks.dedup();
                return Some(SharedFailure {
                    proof: SharedFailureProof::HeadFailure,
                    failed_head: record.failed_head,
                    base_branch: record.base_branch,
                    checks,
                    base_commits: Vec::new(),
                });
            }
        }
    }
    None
}

/// The decision a `shared_base_repair_wait` entry recorded for `key`.
fn recorded_decision(repair_history: &[TaskHistoryEntry], key: &str) -> Option<SharedBaseDecision> {
    repair_history
        .iter()
        .filter(|entry| entry.event == SHARED_BASE_REPAIR_WAIT_EVENT)
        .filter_map(|entry| entry.note.as_deref())
        .filter_map(|note| serde_json::from_str::<Value>(note).ok())
        .find(|note| note["key"].as_str() == Some(key))
        .and_then(|note| {
            Some(SharedBaseDecision {
                task_id: note["decision"]["task_id"].as_str()?.to_string(),
                request_id: note["decision"]["request_id"].as_str()?.to_string(),
            })
        })
}

/// The human crew a system-raised decision is assigned to: the only one, or
/// the operator's own, or else the first by name.
fn decision_human_crew(runtime: &OrbitRuntime) -> Option<String> {
    let registry = runtime.configured_crew_registry_projection();
    registry.viewer_human_crew.or_else(|| {
        registry
            .human_crews
            .iter()
            .map(|crew| crew.name.clone())
            .min()
    })
}

/// File one human-crew task for `wait` and raise its decision request,
/// unless one is already recorded on the repair. A workspace lock serializes
/// admission passes, so concurrent passes raise one request; the repair's
/// history is re-read under it. `Ok(None)` when no human crew can hold it.
fn raise_decision_once(
    runtime: &OrbitRuntime,
    wait: &SharedBaseRepairWait,
) -> Result<Option<SharedBaseDecision>, OrbitError> {
    runtime.ensure_coordination_task_write_permitted()?;
    let Some(crew) = decision_human_crew(runtime) else {
        tracing::warn!(
            task_id = %wait.repair_task_id,
            "no human crew is configured to hold the shared base repair decision"
        );
        return Ok(None);
    };
    let key = wait.key();
    let lock_target = runtime
        .paths()
        .orbit_dir
        .join("tasks/shared-base-repair-decision");
    let mut outcome = None;
    orbit_common::fs::io::with_exclusive_file_lock(
        &lock_target,
        "shared base repair decision",
        || -> Result<(), OrbitError> {
            let history = runtime.get_task_history(&wait.repair_task_id)?;
            if let Some(decision) = recorded_decision(&history, &key) {
                outcome = Some(decision);
                return Ok(());
            }
            let task = runtime.add_task(TaskAddParams {
                title: format!(
                    "Choose who repairs the base failure {} and {} wait on",
                    wait.repair_task_id, wait.holding_task_id
                ),
                description: wait.decision_task_description(),
                relations: vec![
                    TaskRelation {
                        relation_type: TaskRelationType::RelatedTo,
                        target: wait.repair_task_id.clone(),
                        regression: None,
                    },
                    TaskRelation {
                        relation_type: TaskRelationType::RelatedTo,
                        target: wait.holding_task_id.clone(),
                        regression: None,
                    },
                ],
                tags: vec![SHARED_BASE_REPAIR_DECISION_TAG.to_string()],
                priority: TaskPriority::High,
                complexity: TaskComplexity::Low,
                task_type: Some(TaskType::Chore),
                status: Some(TaskStatus::Backlog),
                system_created: true,
                crew: Some(crew.clone()),
                origin: Some(TaskOrigin::Automation),
                ..TaskAddParams::default()
            })?;
            let raised = runtime.raise_agent_decision_request(AgentDecisionRaise {
                task_id: task.id.clone(),
                question: wait.question(),
                options: wait.options(),
                topic: None,
                filer: SHARED_BASE_REPAIR_FILER.to_string(),
            })?;
            let decision = SharedBaseDecision {
                task_id: task.id.clone(),
                request_id: raised.request.id.clone(),
            };
            let mut recorded = wait.clone();
            recorded.decision = Some(decision.clone());
            let note = serde_json::to_string(
                &serde_json::json!({"key": key, "decision": decision, "wait": recorded}),
            )
            .map_err(|error| OrbitError::Execution(format!("encode wait note: {error}")))?;
            runtime.stores().task_records().update(
                &wait.repair_task_id,
                TaskRecordUpdateParams {
                    actor: SYSTEM_ACTOR_LABEL.to_string(),
                    append_history: vec![TaskHistoryEntry {
                        at: Utc::now(),
                        by: SYSTEM_ACTOR_LABEL.to_string(),
                        event: SHARED_BASE_REPAIR_WAIT_EVENT.to_string(),
                        note: Some(note),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: None,
                    }],
                    ..Default::default()
                },
            )?;
            outcome = Some(decision);
            Ok(())
        },
    )?;
    Ok(outcome)
}
