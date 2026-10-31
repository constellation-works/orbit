//! Operator-attested recovery when a refused follower handoff recorded no PR.
//!
//! Recovery has already ended the claim. This route verifies its admission and
//! the merged PR, then uses the ordinary task settlement under the task lock;
//! it never reopens a claim or relaxes the handoff journal's authority checks.

use orbit_common::OrbitError;
use orbit_store::contracts::{AdmissionIdentity, AdmissionLookup, ExecutionClaimPhase};
use orbit_types::task::Task;
use orbit_types::workflow::handoff::{HandoffDelivery, LandingAttemptState};

use super::{ExternalMergeSettlement, SETTLEABLE_STATUSES, names_delivery, refused};
use crate::OrbitRuntime;
use crate::application::task::TaskWorkspaceAutoPort;
use crate::runtime::task_pr_forge::MergedPullRequest;

pub(super) struct RecoveredMerge {
    claim_id: String,
    published_head: String,
}

impl RecoveredMerge {
    pub(super) fn landing_note(&self, landing: &MergedPullRequest) -> (String, String) {
        let landed = format!(
            "pull request #{} (operator-rescued claim {}; branch {}; operator-reported published \
             head {}; merged head {}), merged into {} as {} outside Orbit",
            landing.pull.number,
            self.claim_id,
            landing.pull.head_branch,
            self.published_head,
            landing.head_sha,
            landing.base_branch,
            landing.merge_commit,
        );
        let note = format!("settled from {landed}");
        (landed, note)
    }
}

impl OrbitRuntime {
    /// Settle a recovered follower delivery whose refused handoff did not
    /// record its PR. The operator supplies the PR and original published head;
    /// the forge supplies the merged head and merge commit. History labels the
    /// original head as operator-reported and records both heads. The original
    /// admission, task identity, delivery author and landing branch must match.
    pub fn settle_recovered_claim_merge(
        &self,
        id: &str,
        claim_id: &str,
        pull_request: u64,
        published_head: &str,
    ) -> Result<ExternalMergeSettlement, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        if claim_id.trim().is_empty() || pull_request == 0 || !full_commit(published_head) {
            return Err(refused(
                id,
                "a recovered claim, pull request number and full published head commit are required",
            ));
        }
        let task = self.get_task(id)?;
        if !SETTLEABLE_STATUSES.contains(&task.status) {
            return Err(refused(
                id,
                "only a blocked, in-progress or review task settles",
            ));
        }
        if let Some(live) = self.live_task_delivery(&task) {
            return Err(refused(id, &live));
        }
        let landing = self
            .task_pr_forge()
            .merged_pull_request(&self.context.paths().repo_root, pull_request)?
            .ok_or_else(|| refused(id, "the specified pull request is not merged"))?;
        let rescue = RecoveredMerge {
            claim_id: claim_id.to_string(),
            published_head: published_head.to_ascii_lowercase(),
        };
        self.check_recovered_merge(&task, &rescue, &landing)?;
        let authors = self.settlement_authors(&self.context.paths().repo_root);
        if !authors.contains(&landing.pull.author.to_ascii_lowercase()) {
            return Err(refused(
                id,
                "the merged pull request was not authored by a delivery identity",
            ));
        }
        self.write_external_merge_settlement_as(&task, &landing, None, Some(&rescue))
    }

    /// Rechecked under the task lock immediately before writing `done`.
    pub(super) fn check_recovered_merge(
        &self,
        task: &Task,
        rescue: &RecoveredMerge,
        landing: &MergedPullRequest,
    ) -> Result<(), OrbitError> {
        let id = task.id.as_str();
        if self.live_leaf_task_ids()?.contains(id) {
            return Err(refused(
                id,
                "a live owner drain leaf still carries this task",
            ));
        }
        let tasks = self.stores().tasks();
        let state = tasks
            .inspect_execution_claims()?
            .into_iter()
            .find(|state| state.claim.claim_id == rescue.claim_id && state.claim.task_id == id)
            .ok_or_else(|| refused(id, "the recovered claim does not belong to this task"))?;
        if state.claim.phase != ExecutionClaimPhase::Revoked || state.last_event != "claim_revoked"
        {
            return Err(refused(
                id,
                "the specified claim was not recovered by an owner operator",
            ));
        }
        if state.unresolved_merge_intent.is_some() {
            return Err(refused(
                id,
                "the recovered claim has an unresolved merge intent",
            ));
        }
        let latest_claim = self
            .get_task_history(id)?
            .into_iter()
            .rev()
            .find(|entry| entry.event == "pulled_by")
            .and_then(|entry| {
                serde_json::from_str::<serde_json::Value>(entry.note.as_deref()?).ok()
            })
            .and_then(|note| note.get("claim_id")?.as_str().map(str::to_string));
        if latest_claim.as_deref() != Some(rescue.claim_id.as_str()) {
            return Err(refused(
                id,
                "a later claim superseded the recovered delivery",
            ));
        }
        let claim = &state.claim;
        let AdmissionLookup::Found { receipt, .. } = tasks.lookup_admission(
            &AdmissionIdentity::trusted_local(claim.executed_on.clone()),
            &claim.request_id,
        )?
        else {
            return Err(refused(
                id,
                "the original admission receipt is missing or expired",
            ));
        };
        if receipt.claim.as_ref().is_none_or(|original| {
            original.claim_id != claim.claim_id
                || original.task_id != id
                || original.request_id != claim.request_id
                || original.executed_on != claim.executed_on
        }) || receipt.machine_id != claim.executed_on.machine_id
            || receipt.request.request_id != claim.request_id
            || receipt.request.ship.mode != "pr"
            || receipt.request.ship.landing_branch != landing.base_branch
        {
            return Err(refused(
                id,
                "the merged pull request does not match the recovered claim's admission and landing branch",
            ));
        }
        if !names_delivery(
            &self
                .context
                .settings()
                .pr_settings()
                .conventions
                .branch_name,
            landing,
            id,
        ) || !full_commit(&landing.head_sha)
            || !full_commit(&landing.merge_commit)
        {
            return Err(refused(
                id,
                "the merged pull request must name this task on its delivery branch and report full head and merge commits",
            ));
        }
        if let Some(kept) = &state.preserved_candidate {
            let candidate = &kept.candidate;
            if candidate.branch != landing.pull.head_branch
                || !candidate
                    .head_sha
                    .eq_ignore_ascii_case(&rescue.published_head)
                || candidate.pull_request.as_deref() != Some(&landing.pull.number.to_string())
            {
                return Err(refused(
                    id,
                    "the supplied delivery contradicts the claim's preserved candidate",
                ));
            }
        }
        if let Some(accepted) = tasks.find_accepted_handoff(&rescue.claim_id)? {
            let candidate = accepted.handoff.candidate;
            if candidate.delivery
                != (HandoffDelivery::PullRequest {
                    number: landing.pull.number,
                })
                || candidate.source_branch != landing.pull.head_branch
                || candidate.landing_branch != landing.base_branch
                || !candidate
                    .candidate
                    .commit
                    .eq_ignore_ascii_case(&rescue.published_head)
            {
                return Err(refused(
                    id,
                    "the supplied delivery contradicts the claim's accepted handoff",
                ));
            }
            if tasks.landing_attempts()?.iter().any(|attempt| {
                attempt.handoff_id == accepted.handoff_id
                    && attempt.state == LandingAttemptState::Dispatched
            }) {
                return Err(refused(
                    id,
                    "an owner landing job still carries this handoff",
                ));
            }
        }
        Ok(())
    }
}

fn full_commit(commit: &str) -> bool {
    commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
}
