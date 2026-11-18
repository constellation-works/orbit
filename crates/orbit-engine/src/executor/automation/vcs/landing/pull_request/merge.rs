//! Base refresh, merged settlement, pinned identity, draft refusal and the
//! merge request for one landing poll.

use orbit_common::OrbitError;
use orbit_types::workflow::DeliveryRefusal;
use serde_json::{Value, json};

use crate::context::{HandoffLandingStep, RuntimeHost};

use super::super::super::base::refresh::RefreshStep;
use super::super::super::commit::squash_merge_fields;
use super::super::super::operations;
use super::super::super::pr::{
    PrMergeState, PullRequestStatus, ReviewedMergeOutcome, draft_recovery, resolve_permitted_merge,
};
use super::super::intent::reconcile_intent;
use super::super::observe::observe_candidate;
use super::super::record::{behind_base, complete, record, refuse_pr};
use super::poll::{LandingPoll, Step};

impl<'a, H: RuntimeHost + ?Sized> LandingPoll<'a, H> {
    pub(super) fn refresh_base(
        &self,
        status: &PullRequestStatus,
        diagnose: bool,
    ) -> Result<RefreshStep, OrbitError> {
        let context = self.context;
        super::super::super::base::refresh::recover(
            self.host,
            &self.workspace,
            std::slice::from_ref(&context.task_id),
            &context.handoff_id,
            &context.candidate,
            status,
            self.deadline_ms,
            diagnose,
            Some(&context.handoff_id),
        )
        .map_err(|error| self.stop_on(error))
    }

    /// Settles a merged pull request as this handoff's delivery.
    pub(super) fn settle_merged(
        &mut self,
        status: &PullRequestStatus,
        updated_head: Option<String>,
    ) -> Result<Value, OrbitError> {
        let (host, context) = (self.host, self.context);
        let delivered = match self.pin.ensure_delivered(status, &self.pr_number) {
            Ok(delivered) => delivered,
            // A merged pull request whose identity is not ours is not this
            // handoff's delivery, whatever it merged.
            Err(error) => return Err(self.stop(false, &error.to_string())?),
        };
        let mut evidence = json!({
            "delivery": delivered.as_json(),
            "pull_request": self.number,
            "candidate": context.candidate.candidate.commit,
        });
        if let Some(head) = &updated_head {
            evidence["updated_head"] = json!(head);
        }
        if let Some(actions) = self.recovery.evidence() {
            evidence["check_recovery"] = actions;
        }
        let observed =
            observe_candidate(context, Some(status), self.view).map_err(|e| self.stop_on(e))?;
        // A merge with no intent of ours is the operator's; the completion
        // record carries that in its history note.
        evidence["external_merge"] = json!(self.intent.is_none());
        if let Some(intent_id) = self.intent.take() {
            record(
                host,
                context,
                HandoffLandingStep::ResolveIntent {
                    intent_id,
                    merged: true,
                },
                Some(observed.clone()),
                &evidence,
            )?;
        }
        complete(host, context, observed, &evidence)
    }

    /// Every non-merged state is read against the pinned identity first: a
    /// repointed branch or base must never receive a merge request.
    pub(super) fn ensure_pinned(&self, status: &PullRequestStatus) -> Result<(), OrbitError> {
        match self.pin.ensure_pinned_candidate(status, &self.pr_number) {
            Ok(_) => Ok(()),
            Err(error) => Err(self.stop(false, &error.to_string())?),
        }
    }

    /// A follower can hand off a PR drafted for missing evidence. Send that
    /// candidate through the existing repair flow, whose pr_open refreshes
    /// the body and marks it ready only once every gap is filled. Configured
    /// and unrelated drafts still require the operator.
    pub(super) fn refuse_draft(&self, status: &PullRequestStatus) -> Result<(), OrbitError> {
        if !(status.is_draft || status.merge_state_status.as_deref() == Some("DRAFT")) {
            return Ok(());
        }
        let context = self.context;
        let pr_number = &self.pr_number;
        let configured = self.host.pr_config().conventions.draft;
        // Only an explicit draft carries a body to repair. A DRAFT merge
        // state without isDraft is an older answer without body evidence,
        // so refuse without an extra view.
        if !configured && status.is_draft {
            let missing = draft_recovery::handed_off_section_gaps(
                self.host,
                &self.workspace,
                &context.task_id,
                pr_number,
            )?;
            if !missing.is_empty() {
                let action = if context.automatic_repair_spent {
                    "return the preserved candidate to the backlog for a repair round"
                } else {
                    "return the claim for one automatic repair round"
                };
                let reason = format!(
                    "pull request #{pr_number} is still a section-gap draft; missing required \
                     sections: {}; next action: {action}. Fill the named sections or attach \
                     their required evidence, then resume pr_open to refresh the body and \
                     mark the PR ready before a new validated handoff",
                    missing.join(", ")
                );
                return Err(if context.automatic_repair_spent {
                    refuse_pr(
                        self.host,
                        context,
                        &self.diagnostic,
                        HandoffLandingStep::StopForRequeue,
                        &reason,
                    )?
                } else {
                    self.stop(true, &reason)?
                });
            }
        }
        let reason = if configured {
            "the delivery conventions open every pull request as a draft; next action: \
             explicitly mark the PR ready on the forge, then retry landing"
        } else {
            "no section-gap recovery evidence; next action: mark the PR ready on the \
             forge, then retry landing"
        };
        Err(self.hold(
            false,
            DeliveryRefusal::PullRequestDraft,
            &format!(
                "pull request #{pr_number} cannot be merged (the pull request is still a draft); {reason}"
            ),
        )?)
    }

    /// Stops on a state the landing cannot merge from; mergeable and pending
    /// states proceed.
    pub(super) fn stop_on_terminal_state(
        &self,
        status: &PullRequestStatus,
        state: PrMergeState,
    ) -> Result<(), OrbitError> {
        let pr_number = &self.pr_number;
        match state {
            PrMergeState::Closed => Err(self.hold(
                false,
                DeliveryRefusal::PullRequestClosed,
                &format!(
                    "pull request #{pr_number} was closed without merging; the candidate \
                     stays in review"
                ),
            )?),
            PrMergeState::Contradictory(reason) => Err(self.stop(
                false,
                &format!(
                    "pull request #{pr_number} reports a contradictory merge state \
                     ({reason}); landing needs an unambiguous merged state"
                ),
            )?),
            PrMergeState::Blocked(reason) => {
                let refusal = reason.refusal;
                let reason = format!(
                    "pull request #{pr_number} cannot be merged ({reason}); branch \
                     protection and required checks are not bypassed"
                );
                // A branch that is only behind its base is stale, not refused:
                // the repair brings it up to date and revalidates it.
                Err(if behind_base(status) {
                    self.stop(true, &reason)?
                } else if let Some(refusal) = refusal {
                    self.hold(false, refusal, &reason)?
                } else {
                    self.stop(false, &reason)?
                })
            }
            PrMergeState::Conflict => Err(self.hold(
                true,
                DeliveryRefusal::LandingConflict,
                &format!(
                    "pull request #{pr_number} conflicts with its base; the owner does not \
                     rebase unvalidated code, so a repair needs fresh validation and a new \
                     handoff"
                ),
            )?),
            // A merged pull request is settled before the pinned-identity
            // read, so it never reaches this phase.
            PrMergeState::Merged | PrMergeState::Mergeable | PrMergeState::Pending => Ok(()),
        }
    }

    /// Requests the merge under a durable intent, and re-reads after a
    /// refusal caused by a concurrent base change.
    pub(super) fn request_merge(
        &mut self,
        status: &PullRequestStatus,
        updated_head: bool,
    ) -> Result<Step, OrbitError> {
        let (host, context) = (self.host, self.context);
        let candidate = &context.candidate;
        let number = self.number;
        let pr_number = self.pr_number.clone();
        if self.deadline_passed()? {
            return Err(self.timeout(&context.owed_hosted_checks)?);
        }
        if updated_head && let Step::Continue = self.wait_for_current_gates(status)? {
            return Ok(Step::Continue);
        }
        let capabilities = resolve_permitted_merge(host, &self.workspace_path, &pr_number)
            .map_err(|error| {
                operations::carry_class(
                    &error,
                    OrbitError::Execution(format!(
                        "handoff_land: no permitted merge method for pull request \
                         #{pr_number}: {error}"
                    )),
                )
            })?;
        if self.deadline_passed()? {
            return Err(self.timeout(&context.owed_hosted_checks)?);
        }
        let observed =
            observe_candidate(context, Some(status), self.view).map_err(|e| self.stop_on(e))?;
        if observed.repository != capabilities.repository {
            return Err(self.stop(
                false,
                &format!(
                    "pull request #{pr_number} belongs to repository \
                     '{}', not the handoff's '{}'",
                    capabilities.repository, candidate.repository
                ),
            )?);
        }
        // A refused request never reached the merge, so each request after
        // the first carries its own intent id: the store replays a mutation
        // it has already recorded under the same id.
        let intent_id = if self.base_modified_refusals == 0 {
            format!("{}:{pr_number}", context.handoff_id)
        } else {
            format!(
                "{}:{pr_number}:{}",
                context.handoff_id,
                self.base_modified_refusals + 1
            )
        };
        // Durable before the external mutation: a lost reply after this
        // point is uncertainty the next attempt reconciles.
        let head = self
            .pin
            .candidate_sha()
            .unwrap_or(&candidate.candidate.commit)
            .to_string();
        record(
            host,
            context,
            HandoffLandingStep::PublishIntent {
                intent_id: intent_id.clone(),
            },
            Some(observed),
            &json!({
                "sending": "pull_request_merge",
                "pull_request": number,
                "strategy": capabilities.strategy.as_str(),
                "candidate": candidate.candidate.commit,
                "head": head,
            }),
        )?;
        self.intent = Some(intent_id.clone());
        // The synchronous mutation carries the pinned head, so the
        // provider itself refuses a candidate that moved. A base-modified
        // refusal comes back as a value, so the intent is resolved below.
        let mut request = json!({
            "pr": pr_number,
            "strategy": capabilities.strategy.as_str(),
            "auto": false,
            "reviewed_head_sha": head,
            "report_base_modified": true,
            "workspace_path": self.workspace_path,
        });
        // An unreadable task still lands: the squash takes its subject from
        // the candidate's own commits, which were composed from the task.
        let tasks = match host.get_task(&context.task_id) {
            Ok(task) => vec![task],
            Err(error) => {
                tracing::warn!(
                    task_id = %context.task_id,
                    %error,
                    "handoff_land: squash message composed from the candidate's commits"
                );
                Vec::new()
            }
        };
        if let Value::Object(request) = &mut request {
            request.extend(squash_merge_fields(
                host,
                &self.workspace,
                &tasks,
                Some(&candidate.base_branch),
                &head,
                &pr_number,
            )?);
        }
        let response = host.run_private_vcs_operation(operations::PR_MERGE, request)?;
        let refusal = match ReviewedMergeOutcome::from_response(&response) {
            // Merged is what the provider reports, not what we asked for.
            ReviewedMergeOutcome::Accepted { .. } => return Ok(Step::Continue),
            ReviewedMergeOutcome::BaseModified { diagnostic: text } => text,
            ReviewedMergeOutcome::InProgress { diagnostic } => {
                if let Some(completed) = reconcile_intent(host, context, &intent_id, self.view)? {
                    return Ok(Step::Done(completed));
                }
                self.intent = None;
                return Err(self.stop(
                    false,
                    &format!(
                        "pull request #{pr_number} merge is already in progress ({diagnostic}); \
                         one re-read did not confirm it merged; retry landing after checking its status"
                    ),
                )?);
            }
            // This caller does not opt into completion's reconciliation.
            ReviewedMergeOutcome::Ambiguous { diagnostic } => {
                return Err(OrbitError::Execution(diagnostic));
            }
        };
        self.base_modified_refusals += 1;
        let refusals = self.base_modified_refusals;
        tracing::warn!(
            pull_request = number,
            attempt = refusals,
            "handoff_land: merge refused by a concurrent base change; re-reading the \
             pull request before asking again"
        );
        // The provider refused, so nothing merged: resolve the intent as
        // unmerged by reading the pull request, as reconciliation does. A
        // pull request that did merge still completes here.
        if let Some(completed) = reconcile_intent(host, context, &intent_id, self.view)? {
            return Ok(Step::Done(completed));
        }
        self.intent = None;
        if refusals >= operations::BASE_MODIFIED_MERGE_ATTEMPTS {
            return Err(self.stop(
                false,
                &format!(
                    "pull request #{pr_number} was refused {refusals} \
                     merge requests because the base branch was modified ({refusal}); \
                     the attempt ceiling is reached"
                ),
            )?);
        }
        // Wait one normal poll, then re-read before asking again.
        self.wait_one_poll();
        Ok(Step::Continue)
    }
}
