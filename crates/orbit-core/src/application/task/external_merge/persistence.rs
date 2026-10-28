use orbit_common::OrbitError;
use orbit_store::contracts::{ClaimInvocation, ClaimMutation};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{ExternalRef, Task, TaskStatus, push_external_ref_if_missing};

use crate::OrbitRuntime;
use crate::application::task::helpers::SYSTEM_ACTOR_LABEL;
use crate::application::task::lifecycle::EXTERNAL_MERGE_SETTLED_EVENT;
use crate::application::task::{TaskRecordUpdateParams, TaskRecords};
use crate::runtime::task_pr_forge::MergedPullRequest;

use super::candidate::{Candidate, RevokedHandoff};
use super::model::ExternalMergeSettlement;
use super::{refusal, rescued};

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

impl OrbitRuntime {
    /// Settle `candidate` from `landing`, through its revoked handoff's claim
    /// when it has one.
    pub(super) fn write_settlement(
        &self,
        candidate: &Candidate,
        landing: &MergedPullRequest,
    ) -> Result<ExternalMergeSettlement, OrbitError> {
        self.write_settlement_as(candidate, landing, None)
    }

    pub(super) fn write_settlement_as(
        &self,
        candidate: &Candidate,
        landing: &MergedPullRequest,
        actor: Option<&str>,
    ) -> Result<ExternalMergeSettlement, OrbitError> {
        match &candidate.revoked {
            Some(revoked) => {
                self.write_revoked_handoff_settlement(&candidate.task, revoked, landing, actor)
            }
            None => self.write_external_merge_settlement_as(&candidate.task, landing, actor, None),
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
        actor: Option<&str>,
    ) -> Result<ExternalMergeSettlement, OrbitError> {
        let id = observed.id.as_str();
        let number = landing.pull.number;
        let (_, mut note) = landing_note(landing);
        if revoked.candidate.is_some() {
            note.push_str(&format!(
                "; verified accepted candidate plus base merges at head {}",
                landing.head_sha
            ));
        }
        if let Some(live) = self.live_task_run(observed) {
            return Err(refusal::refused(id, &live));
        }
        if let Some(owed) = self.owed_settlement_hold(id) {
            return Err(refusal::refused(id, &owed));
        }
        self.ensure_mutation_proposals_applied(observed)
            .map_err(|error| refusal::refused(id, &error.to_string()))?;
        self.ensure_resolves_are_workspace_local(observed)?;
        let history_actor = actor.unwrap_or(SYSTEM_ACTOR_LABEL).to_string();
        let context = self.external_handoff_settlement_context(
            revoked,
            landing,
            ClaimInvocation::trusted_operator(
                id.to_string(),
                revoked.claim_id.clone(),
                history_actor,
            ),
        )?;
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
                    verified_candidate_head: revoked
                        .candidate
                        .as_ref()
                        .map(|_| landing.head_sha.clone()),
                },
            )
            .map_err(|error| refusal::refused(id, &error.to_string()))?;
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
    pub(super) fn write_external_merge_settlement_as(
        &self,
        observed: &Task,
        landing: &MergedPullRequest,
        actor: Option<&str>,
        rescue: Option<&rescued::RecoveredMerge>,
    ) -> Result<ExternalMergeSettlement, OrbitError> {
        let history_actor = actor.unwrap_or(SYSTEM_ACTOR_LABEL).to_string();
        let id = observed.id.as_str();
        let number = landing.pull.number;
        let (landed, note) = match rescue {
            Some(rescue) => rescue.landing_note(landing),
            None => landing_note(landing),
        };
        let mut written = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            if task.status != observed.status {
                return Err(refusal::refused(
                    id,
                    &format!(
                        "its status changed to '{}' after the merge was matched",
                        task.status
                    ),
                ));
            }
            if let Some(live) = self.live_task_delivery(&task) {
                return Err(refusal::refused(id, &live));
            }
            if let Some(rescue) = rescue {
                self.check_recovered_merge(&task, rescue, landing)?;
            }
            if let Some(owed) = self.owed_settlement_hold(id) {
                return Err(refusal::refused(id, &owed));
            }
            self.ensure_mutation_proposals_applied(&task)
                .map_err(|error| refusal::refused(id, &error.to_string()))?;
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
                        actor: history_actor.clone(),
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
