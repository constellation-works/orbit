//! The owner's follow-through for a delivered pull request nobody is left to
//! merge [ORB-25966].
//!
//! A delivery run that was authorised to complete (`completion: done`) can
//! end with its pull request open and the task in `review`: the run held
//! after a checks retrigger or a rerun hold, or its watch ended before the
//! checks went green. Nothing watched the pull request after that, so an
//! operator merged it by hand. Each owner sweep pass now finishes it: the same
//! merge and settlement as `orbit.workflow.land`, for a task that meets all of
//! these:
//!
//! - it is in `review`, no run or claim of its own is live, and no decision
//!   request is open;
//! - its newest delivery run carried `completion: done` and ended `success`
//!   or `held` on a delivery hold a fresh forge read can clear;
//! - no `completion.hold` rule holds it, and the pre-landing review covers the
//!   pull request's head;
//! - its one open delivery pull request (several record
//!   `ambiguous_delivery_pull_requests` and merge nothing) is mergeable with every required
//!   check passed, and, when a stopped native run published it, still is the
//!   revision that run published: settlement rejects any other, so a moved
//!   branch records `publication_identity_changed` and merges nothing.
//!
//! Checks still running, or mergeability GitHub has not computed yet, wait for
//! the next pass. The pass runs on every owner sweep tick, so a pull request
//! whose checks pass is merged within one tick interval.
//!
//! A task that cannot be merged does not sit silently: the pass appends one
//! [`LANDING_FOLLOW_UP_EVENT`] entry naming a typed `code`, the `owner` of the
//! next step and the pull request head, once per code and head. A review task
//! whose last observed checks passed but whose delivery never carried
//! completion authority (a run that had it and stopped is not that case) records `completion_not_authorized` with the operator
//! as owner: merging it is the operator's choice, never Orbit's.

use orbit_common::OrbitError;
use orbit_types::task::{
    DecisionRequestState, Task, TaskHistoryEntry, TaskStatus, decision_requests_from_history,
};
use orbit_types::workflow::handoff::{
    HistoricalLandingChecksObservation, LandingChecksObservationState,
};
use orbit_types::workflow::{COMPLETION_HELD_EVENT, current_completion_hold};
use serde::{Deserialize, Serialize};

use super::land::OpenDelivery;
use crate::OrbitRuntime;
use crate::application::task::{
    DeliveryCompletion, TaskJobPort, TaskRecordUpdateParams, TaskRecords, TaskReviewPort,
    TaskWorkspaceAutoPort,
};

/// History event recorded when the owner leaves a review task's pull request
/// unmerged and names why and who acts next. The note is a
/// [`LandingFollowUp`].
pub const LANDING_FOLLOW_UP_EVENT: &str = "landing_follow_up";

/// Who records the follow-up and settles the merge in task history.
const AUTO_LAND_ACTOR: &str = "owner-landing";

/// Follow-up code for a task with several open delivery pull requests.
const AMBIGUOUS_DELIVERY_CODE: &str = "ambiguous_delivery_pull_requests";

/// The owner of a follow-up that needs a person.
pub const FOLLOW_UP_OWNER_OPERATOR: &str = "operator";

/// Why the owner left a review task's pull request unmerged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingFollowUp {
    /// Stable reason code: `completion_not_authorized`,
    /// `pull_request_conflicting`, `required_checks_failed`,
    /// `publication_identity_changed`, `review_not_settled`, `merge_refused` or
    /// `ambiguous_delivery_pull_requests`.
    pub code: String,
    /// Who acts next: `operator`.
    pub owner: String,
    /// The pull request; for `ambiguous_delivery_pull_requests`, the lowest of
    /// the several open ones (the detail lists all).
    pub pull_request: u64,
    /// The pull request head the reason was judged at.
    pub head_sha: Option<String>,
    /// One line with the specifics.
    pub detail: String,
}

/// A pull request the owner merged and settled on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoLanding {
    /// The settled task.
    pub task_id: String,
    /// The merged pull request.
    pub pull_request: u64,
    /// The commit it merged as.
    pub merge_commit: String,
}

impl OrbitRuntime {
    /// Merge and settle every `review` task whose authorised delivery left a
    /// mergeable pull request behind (the owner pass; see the module docs). A
    /// task that cannot be judged or written is logged and skipped.
    pub fn land_completion_authorized_tasks(&self) -> Result<Vec<AutoLanding>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let tasks = self.stores().tasks().list_tasks_filtered(
            Some(TaskStatus::Review),
            None,
            None,
            None,
            None,
            None,
        )?;
        if tasks.is_empty() {
            return Ok(Vec::new());
        }
        let carried = self.live_leaf_task_ids()?;
        let mut landed = Vec::new();
        for task in tasks {
            let id = task.id.clone();
            match self.auto_land_one(task, &carried) {
                Ok(Some(landing)) => landed.push(landing),
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    target: "orbit.core.external_merge",
                    task_id = %id,
                    error = %error,
                    "could not land a task's delivered pull request"
                ),
            }
        }
        Ok(landed)
    }

    fn auto_land_one(
        &self,
        task: Task,
        carried: &std::collections::BTreeSet<String>,
    ) -> Result<Option<AutoLanding>, OrbitError> {
        let id = task.id.clone();
        let history = self.get_task_history(&id)?;
        if decision_requests_from_history(&task, &history, None, chrono::Utc::now())
            .iter()
            .any(|request| request.state == DecisionRequestState::Open)
        {
            return Ok(None);
        }
        let (run_id, needs_passed_check) = match self.delivery_completion(&id)? {
            DeliveryCompletion::Live => return Ok(None),
            DeliveryCompletion::NotAuthorized => {
                self.record_unauthorized_follow_up(&task, &history)?;
                return Ok(None);
            }
            // The run had completion authority and stopped; its own hold or
            // failure carries the disposition, not a missing-authority note.
            DeliveryCompletion::Stopped => return Ok(None),
            DeliveryCompletion::Authorized {
                run_id,
                needs_passed_check,
            } => (run_id, needs_passed_check),
        };
        if current_completion_hold(task.status, &history).is_some() {
            return Ok(None);
        }
        let Ok(candidate) = self.settlement_candidate(task.clone(), carried) else {
            return Ok(None);
        };
        if candidate.revoked.is_some()
            || self
                .ensure_mutation_proposals_applied(&candidate.task)
                .is_err()
        {
            return Ok(None);
        }
        let (number, expected_base) = match self.open_delivery_pull_request(&candidate)? {
            OpenDelivery::One {
                number,
                target_branch,
            } => (number, target_branch),
            OpenDelivery::None => return Ok(None),
            OpenDelivery::Several(numbers) => {
                // Choosing between them is the operator's: merge nothing and
                // say so on the task.
                self.record_follow_up(
                    &task,
                    &history,
                    LandingFollowUp {
                        code: AMBIGUOUS_DELIVERY_CODE.to_string(),
                        owner: FOLLOW_UP_OWNER_OPERATOR.to_string(),
                        pull_request: numbers[0],
                        head_sha: None,
                        detail: format!(
                            "several open pull requests deliver this task: {numbers:?}; \
                             close all but the one to land"
                        ),
                    },
                )?;
                return Ok(None);
            }
        };
        let repo_root = self.context.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let blockers = match forge.pull_request_blockers(&repo_root, number) {
            Ok(Some(blockers)) => blockers,
            // GitHub is still computing mergeability, or cannot say: the next
            // pass reads again.
            Ok(None) | Err(OrbitError::PullRequestMergeabilityUnknown { .. }) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let head = blockers.head_sha.clone();
        let follow_up = |code: &str, detail: String| LandingFollowUp {
            code: code.to_string(),
            owner: FOLLOW_UP_OWNER_OPERATOR.to_string(),
            pull_request: number,
            head_sha: Some(head.clone()),
            detail,
        };
        let base = forge
            .lease_pull_request(&repo_root, &format!("#{number}"))?
            .map(|bound| bound.base_branch);
        // A stopped native run's publication is the revision settlement will
        // accept after the merge; a pull request that moved off it would merge
        // the wrong revision and leave the task in review.
        if let Some(difference) =
            self.stopped_publication_mismatch(&candidate, &blockers.pull, base.as_deref(), &head)?
        {
            self.record_follow_up(
                &task,
                &history,
                follow_up(
                    "publication_identity_changed",
                    format!(
                        "pull request #{number} no longer matches the publication its run \
                         recorded: {difference}"
                    ),
                ),
            )?;
            return Ok(None);
        }
        if let Err(error) =
            super::land::ensure_delivery_target_branch(&id, number, base.as_deref(), &expected_base)
        {
            match error {
                OrbitError::PolicyDenied(reason) => {
                    self.record_follow_up(&task, &history, follow_up("merge_refused", reason))?;
                    return Ok(None);
                }
                other => return Err(other),
            }
        }
        if blockers.conflicting {
            self.record_follow_up(
                &task,
                &history,
                follow_up(
                    "pull_request_conflicting",
                    format!("pull request #{number} conflicts with its base"),
                ),
            )?;
            return Ok(None);
        }
        if !blockers.failed_required_checks.is_empty() {
            self.record_follow_up(
                &task,
                &history,
                follow_up(
                    "required_checks_failed",
                    format!(
                        "required checks failed on pull request #{number}: {:?}",
                        blockers.failed_required_checks
                    ),
                ),
            )?;
            return Ok(None);
        }
        if !blockers.pending_required_checks.is_empty() {
            return Ok(None);
        }
        // A run held because the head reported no required check run has no
        // check evidence until one passes; a forge read with none is not
        // permission to merge. The next pass reads again.
        if needs_passed_check && blockers.passed_required_checks.is_empty() {
            return Ok(None);
        }
        if !self.handoff_landing_review_ready(&task, Some(&head), None)? {
            self.record_follow_up(
                &task,
                &history,
                follow_up(
                    "review_not_settled",
                    "the required pre-landing review does not cover the pull request head"
                        .to_string(),
                ),
            )?;
            return Ok(None);
        }
        if let Some(base) = base.as_deref()
            && let Some(hold) = orbit_engine::candidate_completion_hold(
                crate::runtime::RuntimeKernel::completion_hold_rules(self),
                &task,
                &repo_root,
                base,
                &head,
                &run_id,
            )?
        {
            // The same record a held run leaves: the task now carries its
            // reason and an operator approves it after merging by hand.
            self.stores().task_records().update(
                &id,
                TaskRecordUpdateParams {
                    actor: AUTO_LAND_ACTOR.to_string(),
                    append_history: vec![TaskHistoryEntry {
                        at: chrono::Utc::now(),
                        by: AUTO_LAND_ACTOR.to_string(),
                        event: COMPLETION_HELD_EVENT.into(),
                        note: Some(hold.summary()),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: Some(orbit_types::task::TaskDecision::CompletionHeld(hold)),
                    }],
                    expected_status: Some(vec![TaskStatus::Review]),
                    ..Default::default()
                },
            )?;
            return Ok(None);
        }
        match self.merge_delivery_pull_request(&id, number, Some(&head), &expected_base) {
            Ok(merge_commit) => {
                // The work has landed; a settlement that fails here is read
                // again from the merge by the next settlement pass.
                if let Err(error) =
                    self.settle_externally_merged_task_as(&id, Some(AUTO_LAND_ACTOR))
                {
                    tracing::warn!(
                        target: "orbit.core.external_merge",
                        task_id = %id,
                        error = %error,
                        "merged a delivered pull request but could not settle its task yet"
                    );
                }
                Ok(Some(AutoLanding {
                    task_id: id,
                    pull_request: number,
                    merge_commit,
                }))
            }
            Err(OrbitError::PolicyDenied(reason)) => {
                self.record_follow_up(&task, &history, follow_up("merge_refused", reason))?;
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// A review task whose delivery watch saw its checks pass, but whose
    /// delivery never carried completion authority, waits for an operator's
    /// merge. Say so on the task, once per head.
    fn record_unauthorized_follow_up(
        &self,
        task: &Task,
        history: &[TaskHistoryEntry],
    ) -> Result<(), OrbitError> {
        let Some(observed) = history
            .iter()
            .rev()
            .find_map(HistoricalLandingChecksObservation::from_history_entry)
        else {
            return Ok(());
        };
        let Some(observation) = observed.observation() else {
            return Ok(());
        };
        if observation.state() != LandingChecksObservationState::Passed {
            return Ok(());
        }
        let Ok(number) = observation.pr_number().parse::<u64>() else {
            return Ok(());
        };
        self.record_follow_up(
            task,
            history,
            LandingFollowUp {
                code: "completion_not_authorized".to_string(),
                owner: FOLLOW_UP_OWNER_OPERATOR.to_string(),
                pull_request: number,
                head_sha: observation.head_sha().map(str::to_string),
                detail: format!(
                    "the checks of pull request #{number} passed, but its delivery was not \
                     authorised to complete; an operator merges it"
                ),
            },
        )
    }

    /// Append `follow_up` unless this code was already recorded for its head,
    /// even when another reason was recorded in between. An ambiguity is written
    /// again only when its set of pull requests changes, so it is judged against
    /// the newest ambiguity alone.
    fn record_follow_up(
        &self,
        task: &Task,
        history: &[TaskHistoryEntry],
        follow_up: LandingFollowUp,
    ) -> Result<(), OrbitError> {
        let mut recorded = history
            .iter()
            .rev()
            .filter(|entry| entry.event == LANDING_FOLLOW_UP_EVENT)
            .filter_map(|entry| {
                serde_json::from_str::<LandingFollowUp>(entry.note.as_deref()?).ok()
            });
        let already_recorded = if follow_up.code == AMBIGUOUS_DELIVERY_CODE {
            recorded
                .find(|entry| entry.code == AMBIGUOUS_DELIVERY_CODE)
                .is_some_and(|entry| entry.detail == follow_up.detail)
        } else {
            recorded
                .any(|entry| entry.code == follow_up.code && entry.head_sha == follow_up.head_sha)
        };
        if already_recorded {
            return Ok(());
        }
        let note = serde_json::to_string(&follow_up)
            .map_err(|error| OrbitError::Execution(format!("landing follow-up: {error}")))?;
        self.stores().task_records().update(
            &task.id,
            TaskRecordUpdateParams {
                actor: AUTO_LAND_ACTOR.to_string(),
                append_history: vec![
                    TaskHistoryEntry::new(
                        chrono::Utc::now(),
                        AUTO_LAND_ACTOR,
                        LANDING_FOLLOW_UP_EVENT,
                    )
                    .with_note(Some(note)),
                ],
                expected_status: Some(vec![TaskStatus::Review]),
                ..Default::default()
            },
        )?;
        Ok(())
    }
}
