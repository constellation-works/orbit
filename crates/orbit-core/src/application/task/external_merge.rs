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
//! One unsettled claim does not stand in the way: a `handed_off` claim whose
//! completion authority an operator revoked. Nothing will land that handoff,
//! and the revocation cannot be undone, so its task would otherwise stay in
//! `review` for good. It settles when no landing attempt is dispatched for
//! the handoff, no merge intent is unresolved, and the merged PR is exactly
//! the handoff's candidate PR, merged into its landing branch. Every other
//! claim still refuses: a `claimed`, `running` or `repair_pending` one, and a
//! handoff that awaits approval or is authorized, which approval or the
//! owner's landing job completes.
//!
//! A delivery PR is matched as the closure matches it: its head branch is
//! `orbit/<TASK-ID>-…`, its body names the task, and it was authored by a
//! delivery identity (`pr.delivery_authors`, else the login this machine's
//! forge credentials act as), or it is the delivery PR of a follower handoff
//! this owner accepted, or a PR preserved with a released or failed claim's
//! candidate. A claim PR targets its accepted handoff's landing branch, or
//! the landing branch in its original admission receipt. Other PRs target
//! the base branch the task's delivery run was submitted against, else the
//! workspace base branch.
//!
//! The write is Orbit's own: `done` with a landing note naming the PR and its
//! merge commit, recorded in history as [`EXTERNAL_MERGE_SETTLED_EVENT`]. It
//! starts no execution, so no footprint guard applies to it. A task with an
//! execution claim of its own is still refused by the store, except through
//! the claim journal: a revoked handoff's settlement is one owner store write
//! that ends the claim `revoked` and moves the task `review → done` together,
//! so a crash cannot leave one without the other. It grants no completion
//! authorization, records no merge intent or landing attempt, and leaves the
//! revoked landing request revoked.
//!
//! Two entry points run it. Each `orbit run auto` admission pass settles from
//! one listing of the most recently merged PRs, and reads the forge only when
//! a candidate task has a PR to match. `orbit task settle-merged <id>` reads
//! the task's own PRs one by one, so a merge older than that listing settles
//! too.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionIdentity, AdmissionLookup, ClaimInvocation, ClaimMutation, ExecutionClaimPhase,
};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ExternalRef, GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskStatus, names_task_delivery_pull_request,
    push_external_ref_if_missing,
};
use orbit_types::workflow::ConventionTemplate;
use orbit_types::workflow::handoff::{HandoffDelivery, LandingAttemptState, LandingStartState};
use serde_json::Value;

use super::TaskRecordUpdateParams;
use super::helpers::SYSTEM_ACTOR_LABEL;
use super::lifecycle::EXTERNAL_MERGE_SETTLED_EVENT;
use super::pr_closure::unsettled_claim_reason;
use crate::OrbitRuntime;
use crate::application::task::{TaskRecords, TaskWorkspaceAutoPort};
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
    /// Accepted handoff and released claim PRs, each with its landing branch.
    follower_prs: BTreeMap<u64, String>,
    /// The task's handed-off claim whose completion authority was revoked.
    /// Only its candidate PR then settles the task, together with the claim.
    revoked: Option<RevokedHandoff>,
}

/// A `handed_off` claim whose accepted handoff's completion authority an
/// operator revoked, with the PR that handoff delivered.
#[derive(Debug, Clone)]
struct RevokedHandoff {
    claim_id: String,
    handoff_id: String,
    pull_request: u64,
    landing_branch: String,
}

impl Candidate {
    /// Every PR Orbit recorded for the task, newest first. A revoked
    /// handoff's task has exactly one: the handoff's candidate PR.
    fn pr_numbers(&self) -> Vec<u64> {
        if let Some(revoked) = &self.revoked {
            return vec![revoked.pull_request];
        }
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
        let carried = self.live_leaf_task_ids()?;
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
                candidates.iter().any(|candidate| {
                    names_delivery(
                        &self
                            .context
                            .settings()
                            .pr_settings()
                            .conventions
                            .branch_name,
                        merged,
                        &candidate.task.id,
                    )
                })
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
            match self.write_settlement(&candidate, &landing) {
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
        let carried = self.live_leaf_task_ids()?;
        let task = self.get_task(id)?;
        let candidate = self
            .settlement_candidate(task, &carried)
            .map_err(|reason| refused(id, &reason))?;

        let repo_root = self.context.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let mut fetched = Vec::new();
        for number in candidate.pr_numbers() {
            fetched.extend(forge.merged_pull_request(&repo_root, number)?);
        }
        let merged = fetched
            .iter()
            .filter(|merged| {
                names_delivery(
                    &self
                        .context
                        .settings()
                        .pr_settings()
                        .conventions
                        .branch_name,
                    merged,
                    id,
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        // A revoked handoff's PR proves itself by the handoff, not its author.
        let authors = if merged.is_empty() || candidate.revoked.is_some() {
            Vec::new()
        } else {
            self.settlement_authors(&repo_root)
        };
        let Some(landing) = self.delivery_merge(&candidate, &merged, &authors) else {
            let reason = match &candidate.revoked {
                Some(revoked) => revoked_handoff_unmerged(
                    &self
                        .context
                        .settings()
                        .pr_settings()
                        .conventions
                        .branch_name,
                    revoked,
                    fetched.first(),
                    id,
                ),
                None => format!(
                    "none of its delivery pull requests is merged into its target branch {}",
                    self.delivery_target_branch(&candidate.task)
                ),
            };
            return Err(refused(id, &reason));
        };
        self.write_settlement(&candidate, &landing)
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
        if let Some(live) = self.live_task_run(&task) {
            return Err(format!("{live}; that run lands or fails the task itself"));
        }
        let revoked = self.revoked_handoff(&task)?;
        if carried.contains(&task.id) {
            return Err(
                "a live `run auto` leaf carries it; that run lands or fails the task itself"
                    .to_string(),
            );
        }
        if let Some(owed) = self.owed_settlement_hold(&task.id) {
            return Err(owed);
        }
        let follower_prs = match &revoked {
            Some(revoked) => {
                BTreeMap::from([(revoked.pull_request, revoked.landing_branch.clone())])
            }
            None => {
                let mut prs = self.released_claim_pull_requests(&task.id);
                prs.extend(self.accepted_handoff_pull_requests(&task.id));
                prs
            }
        };
        let candidate = Candidate {
            follower_prs,
            task,
            revoked,
        };
        if candidate.pr_numbers().is_empty() {
            return Err("Orbit recorded no delivery pull request for it".to_string());
        }
        Ok(candidate)
    }

    /// The task's unsettled claim as a revoked handoff that may settle, `None`
    /// when the task has no unsettled claim, or why its claim refuses.
    fn revoked_handoff(&self, task: &Task) -> Result<Option<RevokedHandoff>, String> {
        let Some(claim) = self.unsettled_task_claim(task)? else {
            return Ok(None);
        };
        let unsettled = unsettled_claim_reason(&claim);
        if claim.claim.phase != ExecutionClaimPhase::HandedOff {
            return Err(format!(
                "{unsettled}; that run lands or fails the task itself"
            ));
        }
        let tasks = self.stores().tasks();
        let unreadable = |_| format!("{unsettled}, and its handoff cannot be read");
        let accepted = tasks
            .find_accepted_handoff(&claim.claim.claim_id)
            .map_err(unreadable)?
            .ok_or_else(|| format!("{unsettled}, and it has no accepted handoff"))?;
        if claim.unresolved_merge_intent.is_some() {
            return Err(format!(
                "{unsettled}, and its merge intent is unresolved; reconcile that merge first"
            ));
        }
        let authority = tasks
            .landing_start_requests()
            .map_err(unreadable)?
            .into_iter()
            .find(|request| request.handoff_id == accepted.handoff_id)
            .map(|request| request.state);
        match authority {
            Some(LandingStartState::Revoked) => {}
            None => {
                return Err(format!(
                    "{unsettled}: its handoff awaits completion approval, which completes it"
                ));
            }
            Some(LandingStartState::Pending) => {
                return Err(format!(
                    "{unsettled}: its handoff holds completion authority, and the owner's \
                     landing job completes it"
                ));
            }
            Some(LandingStartState::Completed) => {
                return Err(format!(
                    "{unsettled}: its handoff's landing already completed"
                ));
            }
        }
        if tasks
            .landing_attempts()
            .map_err(unreadable)?
            .iter()
            .any(|attempt| {
                attempt.handoff_id == accepted.handoff_id
                    && attempt.state == LandingAttemptState::Dispatched
            })
        {
            return Err(format!(
                "{unsettled}: an owner landing job still carries its revoked handoff"
            ));
        }
        let candidate = accepted.handoff.candidate;
        let HandoffDelivery::PullRequest { number } = candidate.delivery else {
            return Err(format!(
                "{unsettled}: its revoked handoff delivered no pull request; recover the claim \
                 to blocked or backlog"
            ));
        };
        Ok(Some(RevokedHandoff {
            claim_id: claim.claim.claim_id,
            handoff_id: accepted.handoff_id,
            pull_request: number,
            landing_branch: candidate.landing_branch,
        }))
    }

    /// PRs kept with this task's released or failed claim candidates. The
    /// original admission must still prove which branch each claim targeted;
    /// missing, expired or unreadable receipts contribute no authority.
    fn released_claim_pull_requests(&self, task_id: &str) -> BTreeMap<u64, String> {
        let tasks = self.stores().tasks();
        let Ok(claims) = tasks.inspect_execution_claims() else {
            return BTreeMap::new();
        };
        claims
            .into_iter()
            .filter(|state| {
                state.claim.task_id == task_id
                    && matches!(
                        state.claim.phase,
                        ExecutionClaimPhase::Revoked | ExecutionClaimPhase::Failed
                    )
            })
            .filter_map(|state| {
                let number = state
                    .preserved_candidate?
                    .candidate
                    .pull_request?
                    .parse::<u64>()
                    .ok()?;
                let claim = state.claim;
                let AdmissionLookup::Found { receipt, .. } = tasks
                    .lookup_admission(
                        &AdmissionIdentity::trusted_local(claim.executed_on.clone()),
                        &claim.request_id,
                    )
                    .ok()?
                else {
                    return None;
                };
                let original = receipt.claim.as_ref()?;
                if original.claim_id != claim.claim_id
                    || original.task_id != task_id
                    || original.request_id != claim.request_id
                    || original.executed_on != claim.executed_on
                    || receipt.machine_id != claim.executed_on.machine_id
                    || receipt.request.request_id != claim.request_id
                    || receipt.request.ship.landing_branch.trim().is_empty()
                {
                    return None;
                }
                Some((number, receipt.request.ship.landing_branch))
            })
            .collect()
    }

    /// The delivery identities, or none when they cannot be read: only
    /// recorded claim and accepted handoff pull requests then prove
    /// themselves Orbit's.
    fn settlement_authors(&self, repo_root: &std::path::Path) -> Vec<String> {
        self.delivery_authors(self.task_pr_forge(), repo_root)
            .unwrap_or_else(|error| {
                tracing::warn!(
                    target: "orbit.core.external_merge",
                    error = %error,
                    "could not read the delivery identity; only recorded claim and \
                     accepted handoff pull requests can settle a task"
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
            .filter(|merged| {
                names_delivery(
                    &self
                        .context
                        .settings()
                        .pr_settings()
                        .conventions
                        .branch_name,
                    merged,
                    &task.id,
                )
            })
            .filter(
                |merged| match candidate.follower_prs.get(&merged.pull.number) {
                    Some(landing) => merged.base_branch == *landing,
                    None => {
                        candidate.revoked.is_none()
                            && authors.contains(&merged.pull.author.to_ascii_lowercase())
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

    /// Settle `candidate` from `landing`, through its revoked handoff's claim
    /// when it has one.
    fn write_settlement(
        &self,
        candidate: &Candidate,
        landing: &MergedPullRequest,
    ) -> Result<ExternalMergeSettlement, OrbitError> {
        match &candidate.revoked {
            Some(revoked) => {
                self.write_revoked_handoff_settlement(&candidate.task, revoked, landing)
            }
            None => self.write_external_merge_settlement(&candidate.task, landing),
        }
    }

    /// Settle `observed` and its revoked handoff's claim from `landing` in one
    /// owner store write: the claim ends `revoked` and the task moves
    /// `review → done` with the landing note. The store rechecks, inside that
    /// write, the claim's phase and the task's status, the handoff's revoked
    /// authority, its candidate PR and landing branch, a dispatched landing
    /// attempt and an unresolved merge intent, so a refused write leaves the
    /// claim handed off and the task in review.
    fn write_revoked_handoff_settlement(
        &self,
        observed: &Task,
        revoked: &RevokedHandoff,
        landing: &MergedPullRequest,
    ) -> Result<ExternalMergeSettlement, OrbitError> {
        let id = observed.id.as_str();
        let number = landing.pull.number;
        let (_, note) = landing_note(landing);
        if let Some(live) = self.live_task_run(observed) {
            return Err(refused(id, &live));
        }
        if let Some(owed) = self.owed_settlement_hold(id) {
            return Err(refused(id, &owed));
        }
        self.ensure_resolves_are_workspace_local(observed)?;
        let context = ClaimInvocation::trusted_operator(
            id.to_string(),
            revoked.claim_id.clone(),
            SYSTEM_ACTOR_LABEL.to_string(),
        );
        self.with_mutation(|| {
            self.mutate_execution_claim(
                Some(&context),
                &format!(
                    "external-merge:{}:{number}:{}",
                    revoked.handoff_id, landing.merge_commit
                ),
                &ClaimMutation::SettleExternalMerge {
                    handoff_id: revoked.handoff_id.clone(),
                    pull_request: number,
                    base_branch: landing.base_branch.clone(),
                    merge_commit: landing.merge_commit.clone(),
                    note: note.clone(),
                },
            )
            .map_err(|error| refused(id, &error.to_string()))?;
            Ok(((), OrbitEvent::TaskUpdated { id: id.to_string() }))
        })?;
        let task = self.get_task(id)?;
        tracing::info!(
            target: "orbit.core.external_merge",
            task_id = id,
            pr = number,
            merge_commit = %landing.merge_commit,
            claim_id = %revoked.claim_id,
            "settled a revoked handoff and its task from the pull request merged outside Orbit"
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

    /// Write `observed` to `done` from `landing`, re-checking under the task
    /// lock that it has not moved and that nothing of it went live since.
    fn write_external_merge_settlement(
        &self,
        observed: &Task,
        landing: &MergedPullRequest,
    ) -> Result<ExternalMergeSettlement, OrbitError> {
        let id = observed.id.as_str();
        let number = landing.pull.number;
        let (landed, note) = landing_note(landing);
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

/// How the task landed (`pull request #N, merged into B as C outside Orbit`),
/// and the landing note its settlement records.
fn landing_note(landing: &MergedPullRequest) -> (String, String) {
    let landed = format!(
        "pull request #{}, merged into {} as {} outside Orbit",
        landing.pull.number, landing.base_branch, landing.merge_commit
    );
    let note = format!("settled from {landed}");
    (landed, note)
}

/// The full merge commit a settlement note from [`landing_note`] records;
/// `None` for any other note or an abbreviated commit.
pub(crate) fn settled_merge_commit(note: &str) -> Option<&str> {
    let landed = note.strip_prefix("settled from pull request #")?;
    let (_, merged) = landed.split_once(", merged into ")?;
    let (_, commit) = merged.strip_suffix(" outside Orbit")?.rsplit_once(" as ")?;
    (commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(commit)
}

/// Why a revoked handoff's task did not settle: its candidate PR, as the
/// forge returned it (`None` when it is not merged), is not merged, names
/// another task, or merged into another branch.
fn revoked_handoff_unmerged(
    branch_template: &ConventionTemplate,
    revoked: &RevokedHandoff,
    fetched: Option<&MergedPullRequest>,
    task_id: &str,
) -> String {
    let number = revoked.pull_request;
    let prefix = "its handed-off claim's completion authority was revoked, and";
    match fetched {
        None => format!(
            "{prefix} the handoff's pull request #{number} is not merged; if it will not merge, \
             recover the claim to blocked or backlog"
        ),
        Some(merged) if !names_delivery(branch_template, merged, task_id) => format!(
            "{prefix} the handoff's pull request #{number} does not name it as its delivery"
        ),
        Some(merged) => format!(
            "{prefix} the handoff's pull request #{number} merged into {}, not its landing \
             branch {}",
            merged.base_branch, revoked.landing_branch
        ),
    }
}

/// Whether `merged` is on the task's delivery branch, as `branch_template`
/// names it, and names the task.
fn names_delivery(
    branch_template: &ConventionTemplate,
    merged: &MergedPullRequest,
    task_id: &str,
) -> bool {
    names_task_delivery_pull_request(
        branch_template,
        &merged.pull.head_branch,
        &merged.pull.body,
        task_id,
    )
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
