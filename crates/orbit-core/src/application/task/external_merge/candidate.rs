use std::collections::{BTreeMap, BTreeSet};

use orbit_store::contracts::{
    AdmissionIdentity, AdmissionLookup, ClaimInspection, ExecutionClaimPhase,
};
use orbit_types::task::{
    GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskStatus, names_task_delivery_pull_request,
};
use orbit_types::workflow::ConventionTemplate;
use orbit_types::workflow::handoff::{
    HandoffDelivery, LandingAttemptState, LandingStartState, LandingStopEvidence,
};
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
    requeued_handoff: Option<u64>,
    /// The task's handed-off claim whose landing stopped or whose completion
    /// authority was revoked. Only its candidate PR settles it with the claim.
    pub(super) revoked: Option<RevokedHandoff>,
}

/// A `handed_off` claim whose owner landing stopped or whose completion
/// authority an operator revoked, with the PR that handoff delivered.
#[derive(Debug, Clone)]
pub(super) struct RevokedHandoff {
    pub(super) claim_id: String,
    pub(super) handoff_id: String,
    pub(super) pull_request: u64,
    pub(super) landing_branch: String,
    /// A stopped authorized handoff needs independent candidate/head verification.
    pub(super) candidate: Option<orbit_types::workflow::handoff::HandoffCandidate>,
    /// The latest stopped attempt's readable cause, when authority is pending.
    pub(super) stopped_reason: Option<String>,
}

impl RevokedHandoff {
    pub(super) fn handoff_landing_refusal(&self) -> String {
        if self.candidate.is_some() {
            format!(
                "its owner landing stopped: {}; its handoff still holds completion authority. \
                 Resume it with the owner's Retry landing action or `orbit task retry-handoff` \
                 (see `orbit task retry-handoff --help`)",
                self.stopped_reason
                    .as_deref()
                    .unwrap_or("no readable stop reason was recorded")
            )
        } else {
            "an operator revoked its handoff's completion authority".to_string()
        }
    }
}

impl Candidate {
    /// Every PR Orbit recorded for the task, newest first. A stopped or revoked
    /// handoff's task has exactly one: the handoff's candidate PR.
    pub(super) fn pr_numbers(&self) -> Vec<u64> {
        if let Some(revoked) = &self.revoked {
            return vec![revoked.pull_request];
        }
        if let Some(number) = self.requeued_handoff {
            return vec![number];
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

/// Why a stopped or revoked handoff's task did not settle: its candidate PR, as the
/// forge returned it (`None` when it is not merged), is not merged, names
/// another task, or merged into another branch.
pub(super) fn revoked_handoff_unmerged(
    branch_template: &ConventionTemplate,
    revoked: &RevokedHandoff,
    fetched: Option<&MergedPullRequest>,
    task_id: &str,
) -> String {
    let number = revoked.pull_request;
    if revoked.candidate.is_some() && fetched.is_none() {
        return format!(
            "{}; the handoff's pull request #{number} is not merged",
            revoked.handoff_landing_refusal()
        );
    }
    let prefix = if revoked.candidate.is_some() {
        "its authorized owner landing stopped, and"
    } else {
        "its handed-off claim's completion authority was revoked, and"
    };
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

/// Lazily-read claim ledger shared by one settlement pass: the outer
/// `None` means not read yet, the inner `None` means the read failed.
pub(super) type SettlementClaimsCache = Option<Option<Vec<ClaimInspection>>>;

impl OrbitRuntime {
    /// The pass-shared ledger, reading it on first use. `None` when the
    /// ledger cannot be read; callers keep their own failure semantics.
    fn settlement_claims<'a>(
        &self,
        cache: &'a mut SettlementClaimsCache,
    ) -> Option<&'a [ClaimInspection]> {
        if cache.is_none() {
            *cache = Some(self.stores().tasks().inspect_execution_claims().ok());
        }
        cache.as_ref().and_then(|claims| claims.as_deref())
    }

    /// `task` as a settlement candidate, or why it is not one.
    pub(super) fn settlement_candidate(
        &self,
        task: Task,
        carried: &BTreeSet<String>,
    ) -> Result<Candidate, String> {
        let mut cache = None;
        self.settlement_candidate_with_claims(task, carried, &mut cache)
    }

    /// [`Self::settlement_candidate`] reusing `cache`'s one ledger read.
    pub(super) fn settlement_candidate_with_claims(
        &self,
        task: Task,
        carried: &BTreeSet<String>,
        claims_cache: &mut SettlementClaimsCache,
    ) -> Result<Candidate, String> {
        if !SETTLEABLE_STATUSES.contains(&task.status) {
            return Err(format!(
                "it is '{}'; only a blocked, in-progress, review or preserved failure-handoff backlog task settles",
                task.status
            ));
        }
        let requeued_handoff = if task.status == TaskStatus::Backlog {
            let receipt = self.handoff_receipt(&task).map_err(|e| e.to_string())?;
            match receipt {
                Some(receipt) => {
                    let history = self.get_task_history(&task.id).map_err(|e| e.to_string())?;
                    if super::super::requeue::current_run_requeue_stands(&task, &history)
                        && task.spec_digest_matches(&receipt.task_spec_digest)
                    {
                        Some(
                            receipt
                                .pull_request
                                .parse::<u64>()
                                .map_err(|e| e.to_string())?,
                        )
                    } else {
                        return Err("backlog task has no preserved failure-handoff requeue".into());
                    }
                }
                _ => return Err("backlog task has no preserved failure-handoff requeue".into()),
            }
        } else {
            None
        };
        if let Some(live) = self.live_task_run(&task) {
            return Err(format!("{live}; that run lands or fails the task itself"));
        }
        if carried.contains(&task.id) {
            return Err(
                "a live `run auto` leaf carries it; that run lands or fails the task itself"
                    .to_string(),
            );
        }
        let claims = self.settlement_claims(claims_cache);
        let revoked = self.revoked_handoff_with_claims(&task, claims)?;
        if let Some(owed) = self.owed_settlement_hold(&task.id) {
            return Err(owed);
        }
        let follower_prs = match &revoked {
            Some(revoked) => {
                BTreeMap::from([(revoked.pull_request, revoked.landing_branch.clone())])
            }
            None => {
                let mut prs = self.released_claim_pull_requests_with_claims(&task.id, claims);
                prs.extend(self.accepted_handoff_pull_requests_with_claims(&task.id, claims));
                prs
            }
        };
        let candidate = Candidate {
            follower_prs,
            requeued_handoff,
            task,
            revoked,
        };
        if candidate.pr_numbers().is_empty() {
            return Err("Orbit recorded no delivery pull request for it".to_string());
        }
        Ok(candidate)
    }

    /// The task's unsettled claim as a stopped or revoked handoff that may
    /// settle, `None` when it has no unsettled claim, or why its claim refuses.
    fn revoked_handoff_with_claims(
        &self,
        task: &Task,
        claims: Option<&[ClaimInspection]>,
    ) -> Result<Option<RevokedHandoff>, String> {
        let Some(claim) = Self::unsettled_task_claim_with_claims(task, claims)? else {
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
        let attempt = tasks
            .landing_attempts()
            .map_err(unreadable)?
            .into_iter()
            .find(|attempt| attempt.handoff_id == accepted.handoff_id);
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
                if attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.state == LandingAttemptState::Stopped) => {}
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
        if attempt
            .as_ref()
            .is_some_and(|attempt| attempt.state == LandingAttemptState::Dispatched)
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
            stopped_reason: (authority == Some(LandingStartState::Pending))
                .then(|| attempt.and_then(|attempt| attempt.evidence))
                .flatten()
                .map(|evidence| match LandingStopEvidence::parse(&evidence) {
                    LandingStopEvidence::JsonString { decoded, .. } => decoded,
                    other => other.operator_reason().to_string(),
                }),
        }))
    }

    /// PRs kept with this task's released or failed claim candidates. The
    /// original admission must still prove which branch each claim targeted;
    /// missing, expired or unreadable receipts contribute no authority.
    fn released_claim_pull_requests_with_claims(
        &self,
        task_id: &str,
        claims: Option<&[ClaimInspection]>,
    ) -> BTreeMap<u64, String> {
        let Some(claims) = claims else {
            return BTreeMap::new();
        };
        let tasks = self.stores().tasks();
        claims
            .iter()
            .filter(|state| {
                state.claim.task_id == task_id
                    && matches!(
                        state.claim.phase,
                        ExecutionClaimPhase::Revoked | ExecutionClaimPhase::Failed
                    )
            })
            .filter_map(|state| {
                let number = state
                    .preserved_candidate
                    .as_ref()?
                    .candidate
                    .pull_request
                    .as_ref()?
                    .parse::<u64>()
                    .ok()?;
                let claim = state.claim.clone();
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

    /// Every PR Orbit recorded for `task`, whether or not it may settle: those
    /// on the task and those its accepted handoffs and kept claims delivered.
    pub(super) fn recorded_pull_requests(&self, task: &Task) -> BTreeSet<u64> {
        let claims = self.stores().tasks().inspect_execution_claims().ok();
        let claims = claims.as_deref();
        task.external_refs
            .iter()
            .filter(|reference| reference.system == GITHUB_PR_EXTERNAL_REF_SYSTEM)
            .filter_map(|reference| reference.id.parse::<u64>().ok())
            .chain(
                self.released_claim_pull_requests_with_claims(&task.id, claims)
                    .into_keys(),
            )
            .chain(
                self.accepted_handoff_pull_requests_with_claims(&task.id, claims)
                    .into_keys(),
            )
            .collect()
    }

    /// Whether a PR of `author` into `base` may deliver the candidate.
    pub(super) fn delivers_candidate(
        &self,
        candidate: &Candidate,
        number: u64,
        author: &str,
        base: &str,
        authors: &[String],
    ) -> bool {
        match candidate.follower_prs.get(&number) {
            Some(landing) => base == landing,
            None => {
                candidate.revoked.is_none()
                    && authors.contains(&author.to_ascii_lowercase())
                    && base == self.delivery_target_branch(&candidate.task)
            }
        }
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
        merged
            .iter()
            .filter(|merged| {
                candidate
                    .requeued_handoff
                    .is_none_or(|number| merged.pull.number == number)
            })
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
            .filter(|merged| {
                self.delivers_candidate(
                    candidate,
                    merged.pull.number,
                    &merged.pull.author,
                    &merged.base_branch,
                    authors,
                )
            })
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
