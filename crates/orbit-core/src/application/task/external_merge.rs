//! Settle a task to `done` when its delivery PR merged outside Orbit.
//!
//! An operator who merges a task's delivery PR on the forge lands the task's
//! work, but no Orbit run observes that merge, so the task stayed where it
//! was (usually `blocked`). Walking it through the lifecycle by hand meant a
//! `blocked → in-progress` write that the footprint guard on starting work
//! refuses while another task's claim overlaps it, although nothing will
//! start. This is the reverse of [`pr_closure`](super::pr_closure): there a
//! terminal task closes its PRs, here a task follows its merged PR.
//!
//! A task settles when all of these hold:
//! - it is `blocked`, `in-progress` or `review`;
//! - no run or unsettled claim of its own is live, and no live `run auto`
//!   leaf carries it: that run lands or fails the task itself;
//! - one of its delivery PRs is merged into the task's target branch;
//! - it owes no acceptance criterion after deploy that an operator has not
//!   recorded met.
//!
//! A delivery PR is matched as the closure matches it: its head branch is
//! `orbit/<TASK-ID>-…`, its body names the task, and it was authored by a
//! delivery identity (`pr.delivery_authors`, else the login this machine's
//! forge credentials act as), or it is the delivery PR of a follower handoff
//! this owner accepted. The target branch is that handoff's landing branch,
//! else the base branch the task's delivery run was submitted against, else
//! the workspace base branch.
//!
//! The write is Orbit's own: `done` with a landing note naming the PR and its
//! merge commit, recorded in history as [`EXTERNAL_MERGE_SETTLED_EVENT`]. It
//! starts no execution, so no footprint guard applies to it. A task with an
//! execution claim of its own is still refused by the store.
//!
//! Two entry points run it. Each `orbit run auto` admission pass settles from
//! one listing of the most recently merged PRs, and reads the forge only when
//! a candidate task has a PR to match. `orbit task settle-merged <id>` reads
//! the task's own PRs one by one, so a merge older than that listing settles
//! too.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ExternalRef, GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskStatus, names_task_delivery_pull_request,
    push_external_ref_if_missing,
};
use serde_json::Value;

use super::TaskRecordUpdateParams;
use super::helpers::SYSTEM_ACTOR_LABEL;
use super::lifecycle::EXTERNAL_MERGE_SETTLED_EVENT;
use crate::OrbitRuntime;
use crate::application::workspace_auto::read_live_leaf_runs;
use crate::runtime::task_pr_forge::MergedPullRequest;

/// The statuses a task settles from.
const SETTLEABLE_STATUSES: [TaskStatus; 3] = [
    TaskStatus::Blocked,
    TaskStatus::InProgress,
    TaskStatus::Review,
];

/// A task settled from its merged delivery PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalMergeSettlement {
    /// The settled task.
    pub task_id: String,
    /// The status the task settled from.
    pub previous_status: TaskStatus,
    /// The merged delivery PR.
    pub pr_number: u64,
    /// The branch it merged into.
    pub base_branch: String,
    /// The commit it merged as.
    pub merge_commit: String,
}

/// A task that may settle, with the PRs that may have delivered it.
struct Candidate {
    task: Task,
    /// Accepted follower handoff PRs, each with its landing branch.
    follower_prs: BTreeMap<u64, String>,
}

impl Candidate {
    /// Every PR Orbit recorded for the task, newest first.
    fn pr_numbers(&self) -> Vec<u64> {
        let mut numbers = self
            .task
            .external_refs
            .iter()
            .filter(|reference| reference.system == GITHUB_PR_EXTERNAL_REF_SYSTEM)
            .filter_map(|reference| reference.id.parse::<u64>().ok())
            .chain(self.follower_prs.keys().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        numbers.reverse();
        numbers
    }
}

impl OrbitRuntime {
    /// Settle every task whose delivery PR is among the forge's most recently
    /// merged ones (the owner pass; see the module docs). One task that cannot
    /// be written is logged and skipped; the others still settle.
    pub fn settle_externally_merged_tasks(
        &self,
    ) -> Result<Vec<ExternalMergeSettlement>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let carried = live_leaf_task_ids(self)?;
        let mut candidates = Vec::new();
        for status in SETTLEABLE_STATUSES {
            for task in self.stores().tasks().list_tasks_filtered(
                Some(status),
                None,
                None,
                None,
                None,
                None,
            )? {
                if let Ok(candidate) = self.settlement_candidate(task, &carried) {
                    candidates.push(candidate);
                }
            }
        }
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let repo_root = self.context.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let merged = forge
            .merged_pull_requests(&repo_root)?
            .into_iter()
            .filter(|merged| {
                candidates
                    .iter()
                    .any(|candidate| names_delivery(merged, &candidate.task.id))
            })
            .collect::<Vec<_>>();
        // The delivery identity costs a forge call; only a merge that names a
        // candidate needs it.
        if merged.is_empty() {
            return Ok(Vec::new());
        }
        let authors = self.settlement_authors(&repo_root);
        let mut settled = Vec::new();
        for candidate in candidates {
            let Some(landing) = self.delivery_merge(&candidate, &merged, &authors) else {
                continue;
            };
            match self.write_external_merge_settlement(&candidate.task, &landing) {
                Ok(settlement) => settled.push(settlement),
                Err(error) => tracing::warn!(
                    target: "orbit.core.external_merge",
                    task_id = %candidate.task.id,
                    pr = landing.pull.number,
                    error = %error,
                    "could not settle a task from its merged delivery pull request"
                ),
            }
        }
        Ok(settled)
    }

    /// Settle task `id` from its merged delivery PR, reading each PR Orbit
    /// recorded for it (the operator command). Refuses, naming why, a task
    /// that cannot settle.
    pub fn settle_externally_merged_task(
        &self,
        id: &str,
    ) -> Result<ExternalMergeSettlement, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let carried = live_leaf_task_ids(self)?;
        let task = self.get_task(id)?;
        let candidate = self
            .settlement_candidate(task, &carried)
            .map_err(|reason| refused(id, &reason))?;

        let repo_root = self.context.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let mut merged = Vec::new();
        for number in candidate.pr_numbers() {
            merged.extend(
                forge
                    .merged_pull_request(&repo_root, number)?
                    .filter(|merged| names_delivery(merged, id)),
            );
        }
        let authors = if merged.is_empty() {
            Vec::new()
        } else {
            self.settlement_authors(&repo_root)
        };
        let landing = self
            .delivery_merge(&candidate, &merged, &authors)
            .ok_or_else(|| {
                refused(
                    id,
                    &format!(
                        "none of its delivery pull requests is merged into its target branch {}",
                        self.delivery_target_branch(&candidate.task)
                    ),
                )
            })?;
        self.write_external_merge_settlement(&candidate.task, &landing)
    }

    /// `task` as a settlement candidate, or why it is not one.
    fn settlement_candidate(
        &self,
        task: Task,
        carried: &BTreeSet<String>,
    ) -> Result<Candidate, String> {
        if !SETTLEABLE_STATUSES.contains(&task.status) {
            return Err(format!(
                "it is '{}'; only a blocked, in-progress or review task settles",
                task.status
            ));
        }
        if let Some(live) = self.live_task_delivery(&task) {
            return Err(format!("{live}; that run lands or fails the task itself"));
        }
        if carried.contains(&task.id) {
            return Err(
                "a live `run auto` leaf carries it; that run lands or fails the task itself"
                    .to_string(),
            );
        }
        if let Some(owed) = self.owed_settlement_hold(&task.id) {
            return Err(owed);
        }
        let candidate = Candidate {
            follower_prs: self.accepted_handoff_pull_requests(&task.id),
            task,
        };
        if candidate.pr_numbers().is_empty() {
            return Err("Orbit recorded no delivery pull request for it".to_string());
        }
        Ok(candidate)
    }

    /// The delivery identities, or none when they cannot be read: only
    /// accepted follower PRs then prove themselves Orbit's.
    fn settlement_authors(&self, repo_root: &std::path::Path) -> Vec<String> {
        self.delivery_authors(self.task_pr_forge(), repo_root)
            .unwrap_or_else(|error| {
                tracing::warn!(
                    target: "orbit.core.external_merge",
                    error = %error,
                    "could not read the delivery identity; only accepted follower pull \
                     requests can settle a task"
                );
                Vec::new()
            })
    }

    /// The newest of `merged` that delivered the candidate into its target
    /// branch.
    fn delivery_merge(
        &self,
        candidate: &Candidate,
        merged: &[MergedPullRequest],
        authors: &[String],
    ) -> Option<MergedPullRequest> {
        let task = &candidate.task;
        let target = self.delivery_target_branch(task);
        merged
            .iter()
            .filter(|merged| names_delivery(merged, &task.id))
            .filter(
                |merged| match candidate.follower_prs.get(&merged.pull.number) {
                    Some(landing) => merged.base_branch == *landing,
                    None => {
                        authors.contains(&merged.pull.author.to_ascii_lowercase())
                            && merged.base_branch == target
                    }
                },
            )
            .max_by_key(|merged| merged.pull.number)
            .cloned()
    }

    /// The branch the task's own delivery targets: the base its linked run
    /// was submitted against, else the workspace base branch.
    fn delivery_target_branch(&self, task: &Task) -> String {
        task.job_run_id
            .as_deref()
            .filter(|_| self.task_run_is_local(task))
            .and_then(|run_id| self.stores().jobs().get_job_run(run_id).ok().flatten())
            .and_then(|run| {
                run.input?
                    .get("base_branch")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|branch| !branch.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| self.workspace_base_branch().to_string())
    }

    /// Write `observed` to `done` from `landing`, re-checking under the task
    /// lock that it has not moved and that nothing of it went live since.
    fn write_external_merge_settlement(
        &self,
        observed: &Task,
        landing: &MergedPullRequest,
    ) -> Result<ExternalMergeSettlement, OrbitError> {
        let id = observed.id.as_str();
        let number = landing.pull.number;
        let landed = format!(
            "pull request #{number}, merged into {} as {} outside Orbit",
            landing.base_branch, landing.merge_commit
        );
        let note = format!("settled from {landed}");
        let mut written = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            if task.status != observed.status {
                return Err(refused(
                    id,
                    &format!(
                        "its status changed to '{}' after the merge was matched",
                        task.status
                    ),
                ));
            }
            if let Some(live) = self.live_task_delivery(&task) {
                return Err(refused(id, &live));
            }
            if let Some(owed) = self.owed_settlement_hold(id) {
                return Err(refused(id, &owed));
            }
            self.ensure_resolves_are_workspace_local(&task)?;
            let mut external_refs = task.external_refs.clone();
            push_external_ref_if_missing(
                &mut external_refs,
                ExternalRef::github_pr(number.to_string())?,
            );
            let execution_summary = task
                .execution_summary
                .trim()
                .is_empty()
                .then(|| format!("Delivered by {landed}."));
            let updated = self.with_mutation(|| {
                let updated = self.stores().task_records().update(
                    id,
                    TaskRecordUpdateParams {
                        actor: SYSTEM_ACTOR_LABEL.to_string(),
                        status: Some(TaskStatus::Done),
                        status_event: Some(EXTERNAL_MERGE_SETTLED_EVENT.to_string()),
                        status_note: Some(note.clone()),
                        execution_summary,
                        external_refs: Some(external_refs),
                        expected_status: Some(vec![task.status]),
                        ..TaskRecordUpdateParams::default()
                    },
                )?;
                Ok((
                    updated.clone(),
                    OrbitEvent::TaskUpdated { id: id.to_string() },
                ))
            })?;
            written = Some(updated);
            Ok(())
        })?;
        let task = written.ok_or_else(|| {
            OrbitError::Execution(
                "external merge settlement did not run under the task lock".to_string(),
            )
        })?;
        tracing::info!(
            target: "orbit.core.external_merge",
            task_id = id,
            pr = number,
            merge_commit = %landing.merge_commit,
            from = %observed.status,
            "settled a task from its delivery pull request merged outside Orbit"
        );
        self.record_resolves_side_effects(observed.status, &task);
        self.close_task_prs_after_transition(observed.status, &task, Some(&note));
        Ok(ExternalMergeSettlement {
            task_id: task.id,
            previous_status: observed.status,
            pr_number: number,
            base_branch: landing.base_branch.clone(),
            merge_commit: landing.merge_commit.clone(),
        })
    }
}

/// Tasks a live `run auto` leaf carries. A leaf hands a task to its child
/// before the task records the child's run.
fn live_leaf_task_ids(runtime: &OrbitRuntime) -> Result<BTreeSet<String>, OrbitError> {
    Ok(read_live_leaf_runs(runtime)?
        .into_iter()
        .flat_map(|run| run.task_ids)
        .collect())
}

/// Whether `merged` is on the task's delivery branch and names it.
fn names_delivery(merged: &MergedPullRequest, task_id: &str) -> bool {
    names_task_delivery_pull_request(&merged.pull.head_branch, &merged.pull.body, task_id)
}

impl OrbitRuntime {
    /// Why a delivered task that still owes criteria after deploy stays in
    /// review, or `None` when it owes none. An unreadable history holds it.
    fn owed_settlement_hold(&self, task_id: &str) -> Option<String> {
        self.get_task(task_id)
            .and_then(|task| super::lifecycle::ensure_post_deploy_criteria_met(self, &task))
            .err()
            .map(|error| error.to_string())
    }
}

fn refused(id: &str, reason: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "task '{id}' does not settle from a merged delivery pull request: {reason}"
    ))
}
