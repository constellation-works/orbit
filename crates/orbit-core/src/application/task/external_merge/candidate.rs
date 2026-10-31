use std::collections::{BTreeMap, BTreeSet};

use orbit_store::contracts::{AdmissionIdentity, AdmissionLookup, ExecutionClaimPhase};
use orbit_types::task::{GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, names_task_delivery_pull_request};
use orbit_types::workflow::ConventionTemplate;
use orbit_types::workflow::handoff::{HandoffDelivery, LandingAttemptState, LandingStartState};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::task::pr_closure::unsettled_claim_reason;
use crate::runtime::task_pr_forge::MergedPullRequest;

use super::model::SETTLEABLE_STATUSES;

/// A task that may settle, with the PRs that may have delivered it.
pub(super) struct Candidate {
    pub(super) task: Task,
    /// Accepted handoff and released claim PRs, each with its landing branch.
    follower_prs: BTreeMap<u64, String>,
    /// The task's handed-off claim whose completion authority was revoked.
    /// Only its candidate PR then settles the task, together with the claim.
    pub(super) revoked: Option<RevokedHandoff>,
}

/// A `handed_off` claim whose accepted handoff's completion authority an
/// operator revoked, with the PR that handoff delivered.
#[derive(Debug, Clone)]
pub(super) struct RevokedHandoff {
    pub(super) claim_id: String,
    pub(super) handoff_id: String,
    pub(super) pull_request: u64,
    pub(super) landing_branch: String,
    /// A stopped authorized handoff needs independent candidate/head verification.
    pub(super) candidate: Option<orbit_types::workflow::handoff::HandoffCandidate>,
}

impl Candidate {
    /// Every PR Orbit recorded for the task, newest first. A revoked
    /// handoff's task has exactly one: the handoff's candidate PR.
    pub(super) fn pr_numbers(&self) -> Vec<u64> {
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

/// Why a revoked handoff's task did not settle: its candidate PR, as the
/// forge returned it (`None` when it is not merged), is not merged, names
/// another task, or merged into another branch.
pub(super) fn revoked_handoff_unmerged(
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
pub(super) fn names_delivery(
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
    /// `task` as a settlement candidate, or why it is not one.
    pub(super) fn settlement_candidate(
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
        let phase = crate::application::task::execution_claim_phase_label(claim.claim.phase);
        if claim.claim.phase != ExecutionClaimPhase::HandedOff {
            return Err(format!(
                "{unsettled}; that run lands or fails the task itself; if the attempt is over, \
                 recover it with `orbit task recover-claim {} --phase {phase} --status blocked --reason <reason> \
                 --request <key>` (see `orbit task recover-claim --help`)",
                claim.claim.claim_id
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
                    "{unsettled}: its handoff awaits completion approval, which completes it; \
                     to withdraw it instead, use `orbit task revoke-handoff {} --candidate \
                     <commit> --base <commit> --reason <reason> --request <key>`, then \
                     `orbit task recover-claim {} --phase {phase} --status blocked --reason <reason> --request \
                     <key>`",
                    accepted.handoff_id, claim.claim.claim_id
                ));
            }
            Some(LandingStartState::Pending)
                if tasks
                    .landing_attempts()
                    .map_err(unreadable)?
                    .iter()
                    .any(|attempt| {
                        attempt.handoff_id == accepted.handoff_id
                            && attempt.state == LandingAttemptState::Stopped
                    }) => {}
            Some(LandingStartState::Pending) => {
                return Err(format!(
                    "{unsettled}: its handoff holds completion authority, and the owner's \
                     landing job completes it; to withdraw it instead, use `orbit task \
                     revoke-handoff {} --candidate <commit> --base <commit> --reason <reason> \
                     --request <key>`, then `orbit task recover-claim {} --phase {phase} --status blocked \
                     --reason <reason> --request <key>`",
                    accepted.handoff_id, claim.claim.claim_id
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
                 to blocked or backlog with `orbit task recover-claim {} --phase {phase} --status blocked \
                 --reason <reason> --request <key>`",
                claim.claim.claim_id
            ));
        };
        Ok(Some(RevokedHandoff {
            claim_id: claim.claim.claim_id,
            handoff_id: accepted.handoff_id,
            pull_request: number,
            landing_branch: candidate.landing_branch.clone(),
            candidate: (authority == Some(LandingStartState::Pending)).then_some(candidate),
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
    pub(super) fn settlement_authors(&self, repo_root: &std::path::Path) -> Vec<String> {
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
    pub(super) fn delivery_merge(
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
    pub(super) fn delivery_target_branch(&self, task: &Task) -> String {
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

    /// Why a delivered task that still owes criteria after deploy stays in
    /// review, or `None` when it owes none. An unreadable history holds it.
    pub(super) fn owed_settlement_hold(&self, task_id: &str) -> Option<String> {
        self.get_task(task_id)
            .and_then(|task| {
                crate::application::task::lifecycle::ensure_post_deploy_criteria_met(self, &task)
            })
            .err()
            .map(|error| error.to_string())
    }
}
