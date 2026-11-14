//! An operator accepts a published failure handoff without another execution.

use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ArtifactManifestFileV2, DecisionAction, DecisionAnswer, DecisionGate, DecisionOption,
    DecisionRequest, DecisionRequestState, DecisionResolution, DecisionRiskTier, ExternalRef, Task,
    TaskComment, TaskDecision, TaskHistoryEntry, TaskStatus, push_external_ref_if_missing,
};
use orbit_types::workflow::COMPLETION_HELD_EVENT;

use crate::OrbitRuntime;
use crate::application::task::{
    EXTERNAL_MERGE_SETTLED_EVENT, TaskJobPort, TaskRecordUpdateParams, TaskRecords, TaskReviewPort,
};
use crate::runtime::task_pr_forge::PullRequestMergeOutcome;

use super::answering::decision_comment;

fn handoff_answer_refused(reason: impl std::fmt::Display) -> OrbitError {
    OrbitError::decision_refused(
        DecisionRefusalCode::AnswerNotApplicable,
        format!(
            "the handoff cannot land ({reason}); nothing was applied and the request stays open"
        ),
    )
}

impl OrbitRuntime {
    /// Derive the answer without changing old stored requests. Eligibility is
    /// rechecked under the task lock when the human applies it.
    pub(in crate::application::task) fn add_handoff_landing_answers(
        &self,
        task: &Task,
        requests: &mut [DecisionRequest],
        manifest: Option<&[ArtifactManifestFileV2]>,
    ) -> Result<(), OrbitError> {
        for request in requests {
            if request.gate != DecisionGate::BlockTriage {
                continue;
            }
            let accepted = request
                .resolution
                .as_ref()
                .is_some_and(|r| r.answer_id == "land");
            // Never offer a stored land option around the effective review policy.
            request.answers.retain(|answer| {
                !answer
                    .option
                    .as_ref()
                    .is_some_and(|o| o.action == Some(DecisionAction::Land))
            });
            if !accepted && request.state != DecisionRequestState::Open {
                continue;
            }
            let Some(approval) = request
                .answers
                .iter()
                .find(|answer| {
                    answer.option.as_ref().is_some_and(|option| {
                        option.action == Some(DecisionAction::Requeue) && option.rewrite.is_none()
                    })
                })
                .cloned()
            else {
                continue;
            };
            if request.answers.iter().any(|answer| answer.id == "land") {
                continue;
            }
            if !accepted {
                // Landing merges a published failure handoff; without its receipt
                // there is no candidate to land, so no `land` answer is derived.
                let Some(receipt) = self.handoff_receipt(task)? else {
                    continue;
                };
                if !self.handoff_landing_review_ready(
                    task,
                    Some(receipt.head_sha.as_str()),
                    manifest,
                )? {
                    continue;
                }
            }
            let signoff = approval
                .option
                .as_ref()
                .map_or(approval.label.as_str(), |o| o.summary.as_str());
            request.answers.push(DecisionAnswer {
                id: "land".into(),
                label: "Approve and land".into(),
                consequence: format!(
                    "{signoff}. Merge the green handoff pull request and complete the task."
                ),
                risk_tier: DecisionRiskTier::Irreversible,
                option: Some(DecisionOption {
                    summary: signoff.to_string(),
                    rewrite: None,
                    action: Some(DecisionAction::Land),
                }),
            });
            request.risk_tier = request.risk_tier.max(DecisionRiskTier::Irreversible);
        }
        Ok(())
    }

    /// The caller holds the task lock. Every refusal precedes the merge;
    /// completion, human sign-off and resolution then share one store write.
    pub(super) fn land_handoff_decision(
        &self,
        request: &DecisionRequest,
        resolution: &DecisionResolution,
        answered: TaskHistoryEntry,
        corpus: Option<TaskHistoryEntry>,
    ) -> Result<(), OrbitError> {
        if self.actor().kind != crate::runtime::context::ActorKind::Human
            || self.worker_invocation().is_some()
        {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::CallerNotAllowed,
                "only the operator may land a failure handoff; agent sessions cannot apply this answer",
            ));
        }
        let answer = request
            .answers
            .iter()
            .find(|answer| answer.id == resolution.answer_id)
            .ok_or_else(|| handoff_answer_refused("the landing option is not offered"))?;
        let approval = request
            .answers
            .iter()
            .find(|answer| {
                answer.option.as_ref().is_some_and(|option| {
                    option.action == Some(DecisionAction::Requeue) && option.rewrite.is_none()
                })
            })
            .ok_or_else(|| handoff_answer_refused("no approval requeue option is offered"))?;
        let task = self.get_task(&request.task_id)?;
        if request.gate != DecisionGate::BlockTriage || task.status != TaskStatus::Blocked {
            return Err(handoff_answer_refused(
                "the task is not blocked on a block decision",
            ));
        }
        if let Some(live) = self.unsettled_run(&task)? {
            return Err(handoff_answer_refused(live));
        }
        let receipt = self.handoff_receipt(&task)?.ok_or_else(|| {
            handoff_answer_refused("no failure-handoff receipt binds a candidate")
        })?;
        if !task.spec_digest_matches(&receipt.task_spec_digest) {
            return Err(handoff_answer_refused(
                "the task specification changed after publication",
            ));
        }
        if self.get_task_history(&task.id)?.iter().any(|entry| {
            entry.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT
                && entry.at >= request.raised_at
        }) {
            return Err(handoff_answer_refused("the candidate was discarded"));
        }
        if !self.handoff_landing_review_ready(&task, Some(&receipt.head_sha), None)? {
            return Err(handoff_answer_refused(
                "required pre-landing review has not settled for this candidate; requeue it",
            ));
        }
        let number = receipt
            .pull_request
            .parse::<u64>()
            .map_err(|_| handoff_answer_refused("the receipt has no valid pull request number"))?;
        let repo = &self.context.paths().repo_root;
        let forge = self.task_pr_forge();
        let pull = forge
            .handoff_landing_pull_request(repo, number)
            .map_err(handoff_answer_refused)?
            .ok_or_else(|| {
                handoff_answer_refused(format!("pull request #{number} is closed or merged"))
            })?;
        if pull.draft {
            return Err(handoff_answer_refused(format!(
                "pull request #{number} is a draft"
            )));
        }
        let blockers = &pull.blockers;
        if blockers.head_sha != receipt.head_sha || blockers.pull.head_branch != receipt.branch {
            return Err(handoff_answer_refused(
                "the pull request head differs from the handoff candidate",
            ));
        }
        if blockers.conflicting {
            return Err(handoff_answer_refused(
                "the pull request conflicts with its base",
            ));
        }
        if !blockers.failed_required_checks.is_empty()
            || !blockers.pending_required_checks.is_empty()
        {
            return Err(handoff_answer_refused(format!(
                "required checks failed {:?} or are pending {:?}",
                blockers.failed_required_checks, blockers.pending_required_checks,
            )));
        }
        self.preflight_handoff_landing(&task, &pull)
            .map_err(handoff_answer_refused)?;
        // Reuse the publication authenticator without persisting a requeue:
        // immutable owner/task/checkpoint chain, exact PR, remote and candidate.
        self.failure_handoff_preservation(&task)
            .map_err(handoff_answer_refused)?;
        let hold = orbit_engine::candidate_completion_hold(
            self.completion_hold_rules(),
            &task,
            repo,
            &pull.base_branch,
            &receipt.head_sha,
            &receipt.run_id,
        )?;
        let merge_commit = match forge.merge_pull_request(repo, number, &receipt.head_sha)? {
            PullRequestMergeOutcome::Merged { merge_commit } => merge_commit,
            PullRequestMergeOutcome::Refused { reason } => {
                return Err(handoff_answer_refused(reason));
            }
        };
        let actor = &resolution.answered_by;
        let mut history = vec![answered];
        history.extend(corpus);
        if let Some(hold) = hold {
            history.push(TaskHistoryEntry {
                at: resolution.answered_at,
                by: actor.clone(),
                event: COMPLETION_HELD_EVENT.into(),
                note: Some(format!(
                    "{}; approved by the answering operator",
                    hold.summary()
                )),
                from_status: None,
                to_status: None,
                operation_id: Some(request.id.clone()),
                decision: Some(TaskDecision::CompletionHeld(hold)),
            });
        }
        let note = format!(
            "settled from pull request #{number}, merged into {} as {merge_commit} by owner decision",
            pull.base_branch
        );
        let comment = decision_comment(
            request,
            &answer.id,
            &answer.label,
            resolution
                .provenance
                .as_deref()
                .unwrap_or("operator surface"),
            resolution.note.as_deref(),
            &format!(
                "{}\nOriginal approval: {}\nOriginal approval consequence: {}\n{note}.",
                answer.consequence, approval.label, approval.consequence
            ),
        );
        let mut refs = task.external_refs.clone();
        push_external_ref_if_missing(&mut refs, ExternalRef::github_pr(number.to_string())?);
        let updated = self.with_mutation(|| {
            let updated = self.stores().task_records().update(
                &task.id,
                TaskRecordUpdateParams {
                    actor: actor.clone(),
                    status: Some(TaskStatus::Done),
                    status_event: Some(EXTERNAL_MERGE_SETTLED_EVENT.into()),
                    status_note: Some(note.clone()),
                    execution_summary: task
                        .execution_summary
                        .trim()
                        .is_empty()
                        .then(|| format!("Delivered by {actor}: {note}.")),
                    external_refs: Some(refs.clone()),
                    append_history: history.clone(),
                    append_comments: vec![TaskComment {
                        at: resolution.answered_at,
                        by: actor.clone(),
                        message: comment.clone(),
                        operation_id: Some(request.id.clone()),
                    }],
                    expected_status: Some(vec![TaskStatus::Blocked]),
                    ..Default::default()
                },
            )?;
            Ok((
                updated,
                OrbitEvent::TaskUpdated {
                    id: task.id.clone(),
                },
            ))
        })?;
        self.record_resolves_side_effects(task.status, &updated);
        self.close_task_prs_after_transition(task.status, &updated, Some(&note));
        Ok(())
    }
}
