//! Plan and write a final-recovery decision under the task write lock.

use chrono::{Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    FINAL_RECOVERY_DECIDED_EVENT, Task, TaskComment, TaskDecision, TaskHistoryEntry, TaskStatus,
};
use orbit_types::workflow::{
    FinalRecoveryDecision, TRANSIENT_INFRA_BLOCKED_EVENT, TransientInfraBlock,
};
use serde_json::Value;

use super::super::TaskRecordUpdateParams as StoreTaskUpdateParams;
use super::super::helpers::SYSTEM_ACTOR_LABEL;
use super::super::legacy_decisions;
use super::super::lifecycle::{ensure_completion_run_stopped, ensure_post_deploy_criteria_met};
use super::super::resolves::pending_resolves_history;
use super::FINAL_RECOVERY_REQUEUED_EVENT;
use super::request::{FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest};
use super::revision::FinalRecoveryTaskRevision;
use super::verify::verify_on_base;
use crate::OrbitRuntime;
use crate::application::task::{TaskJobPort, TaskRecords};

const COMPLETED_EVENT: &str = "final_recovery_completed";
const REJECTED_EVENT: &str = "final_recovery_rejected";
const ARCHIVED_EVENT: &str = "final_recovery_archived";
const ESCALATED_EVENT: &str = "final_recovery_escalated";

/// One planned write: the status, its history event and note, and the
/// comment that records the decision.
struct Plan {
    status: TaskStatus,
    event: &'static str,
    note: String,
    comment: String,
    execution_summary: Option<String>,
    outcome: FinalRecoveryOutcome,
    /// The engine's transient-infrastructure classification an escalation
    /// carries, recorded beside it for the cleared-infra requeue.
    infra: Option<TransientInfraBlock>,
}

impl OrbitRuntime {
    /// Apply a `final_recovery` activity result to its task.
    ///
    /// `output` is the activity's raw result; a missing or malformed one
    /// escalates. `resume` returns without writing. Every other decision is
    /// re-checked under the task write lock and recorded as one task comment
    /// naming the run, whether it was applied, overridden or refused:
    ///
    /// - a run whose comment the task already holds was applied before; its
    ///   recorded outcome is returned without another decision write. Pending
    ///   resolves and open PRs from its terminal transition are retried;
    /// - a task that is already terminal, or whose lifecycle changed after
    ///   `request.observed` ([`FinalRecoveryTaskRevision`]), is refused;
    /// - `complete_no_diff` completes only when `evidence_commit` is reachable
    ///   from `request.base_ref`, and otherwise escalates;
    /// - `requeue` past `request.requeue_bound` escalates;
    /// - `escalate` blocks the task with the diagnosis and the human action.
    pub fn apply_final_recovery(
        &self,
        request: &FinalRecoveryRequest,
        output: Option<&Value>,
    ) -> Result<FinalRecoveryOutcome, OrbitError> {
        self.apply_classified_final_recovery(request, output, None)
    }

    /// [`Self::apply_final_recovery`] for a decision the engine made itself
    /// after classifying the failure as transient infrastructure. `infra`
    /// is set only by that engine path, never from an agent's output; an
    /// `escalate` it accompanies is recorded as an infra block.
    pub(crate) fn apply_classified_final_recovery(
        &self,
        request: &FinalRecoveryRequest,
        output: Option<&Value>,
        infra: Option<TransientInfraBlock>,
    ) -> Result<FinalRecoveryOutcome, OrbitError> {
        let decision = FinalRecoveryDecision::from_output(output);
        if let FinalRecoveryDecision::Resume { step_id, .. } = &decision {
            return Ok(FinalRecoveryOutcome::Resume {
                step_id: step_id.trim().to_string(),
            });
        }
        self.ensure_coordination_task_write_permitted()?;
        let mut applied = None;
        let mut replayed = false;
        // The status and note this run wrote, for the after-lock PR closure.
        let mut transition = None;
        self.stores()
            .tasks()
            .with_task_write_lock(&request.task_id, &mut || {
                if let Some(recorded) =
                    self.recorded_final_recovery(&request.task_id, &request.run_id)?
                {
                    let marker = format!("final recovery (run_id={}):", request.run_id);
                    transition = self
                        .get_task_history(&request.task_id)?
                        .into_iter()
                        .find(|entry| {
                            matches!(
                                entry.event.as_str(),
                                COMPLETED_EVENT | REJECTED_EVENT | ARCHIVED_EVENT
                            ) && entry
                                .note
                                .as_deref()
                                .is_some_and(|note| note.starts_with(&marker))
                        })
                        .and_then(|entry| {
                            Some((entry.from_status?, entry.to_status?, entry.note?))
                        });
                    applied = Some(recorded);
                    replayed = true;
                    return Ok(());
                }
                let task = self.get_task(&request.task_id)?;
                let current = self.final_recovery_revision_after(&request.observed, &task)?;
                let plan = match refusal(&task, &current, request) {
                    Some(reason) => Plan::refused(&task, request, &decision, reason),
                    None => self.plan(&task, request, &decision, infra.as_ref())?,
                };
                if plan.status == TaskStatus::Done {
                    self.ensure_resolves_are_workspace_local(&task)?;
                    ensure_post_deploy_criteria_met(self, &task)?;
                    ensure_completion_run_stopped(self, &task, None, Some(&request.run_id))?;
                }
                self.write_plan(&task, request, &decision, &plan)?;
                transition = Some((task.status, plan.status, plan.note.clone()));
                applied = Some(plan.outcome);
                Ok(())
            })?;
        let outcome = applied.ok_or_else(|| {
            OrbitError::Execution(
                "final recovery applier did not run under the task lock".to_string(),
            )
        })?;
        if let Some((previous_status, committed_status, note)) = transition
            && committed_status.is_terminal()
        {
            let task = self.final_recovery_side_effect_task(&request.task_id)?;
            // A later lifecycle decision wins over this run's transition.
            if task.status != committed_status {
                return Ok(outcome);
            }
            self.record_resolves_side_effects(
                if replayed {
                    task.status
                } else {
                    previous_status
                },
                &task,
            );
            self.close_task_prs_after_transition(previous_status, &task, Some(&note));
        }
        Ok(outcome)
    }

    fn final_recovery_side_effect_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        #[cfg(test)]
        if FAIL_SIDE_EFFECT_READ.with(|fail| fail.replace(false)) {
            return Err(OrbitError::Execution(
                "injected post-lock task read failure".to_string(),
            ));
        }
        self.get_task(task_id)
    }

    /// The revision `task` is at now, for a later [`Self::apply_final_recovery`]
    /// to compare against.
    pub fn final_recovery_revision(
        &self,
        task: &Task,
    ) -> Result<FinalRecoveryTaskRevision, OrbitError> {
        Ok(FinalRecoveryTaskRevision::of(
            task,
            &self.get_task_comments(&task.id)?,
            &self.get_task_history(&task.id)?,
        ))
    }

    /// The revision `task` is at now, measured against `observed`
    /// ([`FinalRecoveryTaskRevision::after`]), for judging whether it changed
    /// since.
    pub(crate) fn final_recovery_revision_after(
        &self,
        observed: &FinalRecoveryTaskRevision,
        task: &Task,
    ) -> Result<FinalRecoveryTaskRevision, OrbitError> {
        Ok(FinalRecoveryTaskRevision::after(
            observed,
            task,
            &self.get_task_comments(&task.id)?,
            &self.get_task_history(&task.id)?,
        ))
    }

    /// The outcome the applier recorded on the task for `run_id`'s decision,
    /// when it already applied one: read back from the typed decision it
    /// recorded with the task change. An operator or cleared-infra requeue of
    /// that run's block records a later one, which supersedes it.
    pub(crate) fn recorded_final_recovery(
        &self,
        task_id: &str,
        run_id: &str,
    ) -> Result<Option<FinalRecoveryOutcome>, OrbitError> {
        let typed = self
            .get_task_history(task_id)?
            .into_iter()
            .rev()
            .find_map(|entry| match entry.decision {
                Some(TaskDecision::FinalRecovery {
                    run_id: recorded,
                    outcome,
                    ..
                }) if entry.by == SYSTEM_ACTOR_LABEL && recorded == run_id => {
                    Some(FinalRecoveryOutcome::from(outcome))
                }
                _ => None,
            });
        if typed.is_some() {
            return Ok(typed);
        }
        Ok(legacy_decisions::final_recovery_outcome(
            &self.get_task_comments(task_id)?,
            run_id,
        ))
    }

    fn plan(
        &self,
        task: &Task,
        request: &FinalRecoveryRequest,
        decision: &FinalRecoveryDecision,
        infra: Option<&TransientInfraBlock>,
    ) -> Result<Plan, OrbitError> {
        let header = comment_header(request, decision);
        Ok(match decision {
            FinalRecoveryDecision::Resume { step_id, .. } => {
                return Err(OrbitError::Execution(format!(
                    "final recovery resume from `{step_id}` is applied by the workflow engine"
                )));
            }
            FinalRecoveryDecision::CompleteNoDiff {
                evidence_commit,
                rationale,
            } => match verify_on_base(&request.repo_root, &request.base_ref, evidence_commit) {
                Ok(commit) => {
                    let status = match request.completion {
                        FinalRecoveryCompletion::Review => TaskStatus::Review,
                        FinalRecoveryCompletion::Done => TaskStatus::Done,
                    };
                    let note = format!(
                        "final recovery (run_id={}): covering commit {commit} is reachable from \
                         '{}'",
                        request.run_id, request.base_ref
                    );
                    Plan {
                        status,
                        event: COMPLETED_EVENT,
                        execution_summary: task.execution_summary.trim().is_empty().then(|| {
                            format!(
                                "Outcome: success\nAlready delivered on '{}' by {commit} \
                                 (verified by final recovery, run_id={}).\n{}",
                                request.base_ref,
                                request.run_id,
                                rationale.trim()
                            )
                        }),
                        comment: format!(
                            "{header} outcome=completed status={status}\n\
                             evidence_commit: {commit}\nbase_ref: {}\nrationale: {}",
                            request.base_ref,
                            rationale.trim()
                        ),
                        note,
                        outcome: FinalRecoveryOutcome::Completed {
                            status,
                            evidence_commit: commit,
                        },
                        infra: None,
                    }
                }
                Err(reason) => {
                    let reason = format!("complete_no_diff refused: {reason}");
                    Plan::escalated(
                        request,
                        &header,
                        &reason,
                        &format!(
                            "evidence_commit: {}\nrationale: {}\nhuman_action: find the commit \
                             that delivers this task on '{}', or move the task by hand",
                            evidence_commit.trim(),
                            rationale.trim(),
                            request.base_ref
                        ),
                        Some(reason.clone()),
                    )
                }
            },
            FinalRecoveryDecision::Reject { reason, evidence } => Plan {
                status: TaskStatus::Rejected,
                event: REJECTED_EVENT,
                note: format!(
                    "final recovery (run_id={}): {}",
                    request.run_id,
                    reason.trim()
                ),
                comment: format!(
                    "{header} outcome=rejected\nreason: {}\nevidence: {}",
                    reason.trim(),
                    evidence.trim()
                ),
                execution_summary: None,
                outcome: FinalRecoveryOutcome::Rejected,
                infra: None,
            },
            FinalRecoveryDecision::Archive { reason } => Plan {
                status: TaskStatus::Archived,
                event: ARCHIVED_EVENT,
                note: format!(
                    "final recovery (run_id={}): {}",
                    request.run_id,
                    reason.trim()
                ),
                comment: format!("{header} outcome=archived\nreason: {}", reason.trim()),
                execution_summary: None,
                outcome: FinalRecoveryOutcome::Archived,
                infra: None,
            },
            FinalRecoveryDecision::Requeue { reason } => {
                let bound = request.requeue_bound;
                let recent = self.recent_requeues(&task.id, bound.window)?;
                if recent >= bound.max_requeues {
                    let limit = format!(
                        "requeue refused: final recovery already requeued this task {recent} \
                         time(s) in the last {} hour(s) (limit {})",
                        bound.window.num_hours(),
                        bound.max_requeues
                    );
                    Plan::escalated(
                        request,
                        &header,
                        &limit,
                        &format!(
                            "requeue_reason: {}\nhuman_action: the failure keeps recurring; \
                             fix its cause before moving the task back to backlog",
                            reason.trim()
                        ),
                        Some(limit.clone()),
                    )
                } else {
                    Plan {
                        status: TaskStatus::Backlog,
                        event: FINAL_RECOVERY_REQUEUED_EVENT,
                        note: format!(
                            "final recovery (run_id={}): {}",
                            request.run_id,
                            reason.trim()
                        ),
                        comment: format!(
                            "{header} outcome=requeued requeue={} of {} per {} hour(s)\n\
                             reason: {}",
                            recent + 1,
                            bound.max_requeues,
                            bound.window.num_hours(),
                            reason.trim()
                        ),
                        execution_summary: None,
                        outcome: FinalRecoveryOutcome::Requeued,
                        infra: None,
                    }
                }
            }
            FinalRecoveryDecision::Escalate {
                diagnosis,
                human_action,
            } => match infra {
                // The engine classified the failure; the block names its
                // condition so a passing probe requeues it.
                Some(infra) => Plan {
                    infra: Some(infra.clone()),
                    ..Plan::escalated(
                        request,
                        &header,
                        diagnosis.trim(),
                        &format!("human_action: {}", human_action.trim()),
                        Some(format!(
                            "blocked on transient infrastructure ({})",
                            infra.class.as_str()
                        )),
                    )
                },
                None => Plan::escalated(
                    request,
                    &header,
                    diagnosis.trim(),
                    &format!("human_action: {}", human_action.trim()),
                    None,
                ),
            },
        })
    }

    /// Final-recovery requeues recorded in the trailing `window`: the one
    /// count the requeue bound reads, shared by final recovery and the
    /// blocked-task triage. A caller that cannot read it must not requeue.
    pub(crate) fn recent_requeues(
        &self,
        task_id: &str,
        window: Duration,
    ) -> Result<usize, OrbitError> {
        #[cfg(test)]
        if FAIL_REQUEUE_COUNT_READ.with(|fail| fail.replace(false)) {
            return Err(OrbitError::Store(
                "injected requeue count read failure".to_string(),
            ));
        }
        let since = Utc::now() - window;
        Ok(self
            .get_task_history(task_id)?
            .iter()
            .filter(|entry| entry.event == FINAL_RECOVERY_REQUEUED_EVENT && entry.at >= since)
            .count())
    }

    fn write_plan(
        &self,
        task: &Task,
        request: &FinalRecoveryRequest,
        decision: &FinalRecoveryDecision,
        plan: &Plan,
    ) -> Result<(), OrbitError> {
        let comment = TaskComment {
            at: Utc::now(),
            by: SYSTEM_ACTOR_LABEL.to_string(),
            message: plan.comment.clone(),
            operation_id: None,
        };
        // The comment is the human-readable record; the typed decision is
        // what a replay and the blocked-task backstop read back.
        let decided = plan.outcome.applied().map(|outcome| TaskHistoryEntry {
            at: comment.at,
            by: SYSTEM_ACTOR_LABEL.to_string(),
            event: FINAL_RECOVERY_DECIDED_EVENT.to_string(),
            note: plan.comment.lines().next().map(str::to_string),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: Some(TaskDecision::FinalRecovery {
                run_id: request.run_id.clone(),
                decision: decision.kind().to_string(),
                outcome,
            }),
        });
        let published = if matches!(plan.outcome, FinalRecoveryOutcome::Requeued) {
            self.published_owner_preservation(task, &request.run_id)?
        } else {
            None
        };
        let infra = plan
            .infra
            .as_ref()
            .map(|infra| {
                serde_json::to_string(infra).map(|note| TaskHistoryEntry {
                    at: comment.at,
                    by: SYSTEM_ACTOR_LABEL.to_string(),
                    event: TRANSIENT_INFRA_BLOCKED_EVENT.to_string(),
                    note: Some(note),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                })
            })
            .transpose()
            .map_err(|error| OrbitError::Execution(format!("encode infra block: {error}")))?;
        let transition = plan.status != task.status;
        // A requeue is a new retry decision even if the task is already in
        // backlog. Its event must guard cleanup and count toward the bound.
        let record_status = transition || matches!(plan.outcome, FinalRecoveryOutcome::Requeued);
        self.with_mutation(|| {
            let updated = self.stores().task_records().update(
                &task.id,
                StoreTaskUpdateParams {
                    actor: SYSTEM_ACTOR_LABEL.to_string(),
                    status: transition.then_some(plan.status),
                    status_event: record_status.then(|| plan.event.to_string()),
                    status_note: record_status.then(|| plan.note.clone()),
                    execution_summary: plan.execution_summary.clone(),
                    append_comments: vec![comment.clone()],
                    append_history: decided
                        .clone()
                        .into_iter()
                        .chain(published.clone())
                        .chain(infra.clone())
                        .chain(if transition && plan.status == TaskStatus::Done {
                            pending_resolves_history(task)
                        } else {
                            Vec::new()
                        })
                        .collect(),
                    expected_status: Some(vec![task.status]),
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
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    pub(in super::super) static FAIL_SIDE_EFFECT_READ: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(in super::super) static FAIL_REQUEUE_COUNT_READ: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Plan {
    fn refused(
        task: &Task,
        request: &FinalRecoveryRequest,
        decision: &FinalRecoveryDecision,
        reason: String,
    ) -> Self {
        Self {
            status: task.status,
            event: ESCALATED_EVENT,
            note: String::new(),
            comment: format!(
                "{} outcome=refused\nreason: {reason}",
                comment_header(request, decision)
            ),
            execution_summary: None,
            outcome: FinalRecoveryOutcome::Refused { reason },
            infra: None,
        }
    }

    fn escalated(
        request: &FinalRecoveryRequest,
        header: &str,
        diagnosis: &str,
        detail: &str,
        override_reason: Option<String>,
    ) -> Self {
        Self {
            status: TaskStatus::Blocked,
            event: ESCALATED_EVENT,
            note: format!(
                "final recovery (run_id={}) escalated: {diagnosis}",
                request.run_id
            ),
            comment: format!("{header} outcome=escalated\ndiagnosis: {diagnosis}\n{detail}"),
            execution_summary: None,
            outcome: FinalRecoveryOutcome::Escalated {
                reason: override_reason,
            },
            infra: None,
        }
    }
}

/// Why the decision may not be applied to `task`, at `current`, at all, if
/// it may not.
fn refusal(
    task: &Task,
    current: &FinalRecoveryTaskRevision,
    request: &FinalRecoveryRequest,
) -> Option<String> {
    if task.status.is_terminal() {
        return Some(format!(
            "task is already {}; final recovery never reopens settled work",
            task.status
        ));
    }
    if current.changed_since(&request.observed) {
        return Some(format!(
            "task changed after the failure (was {} at {}, now {} at {}); the later decision \
             stands",
            request.observed.status,
            request.observed.updated_at.to_rfc3339(),
            current.status,
            current.updated_at.to_rfc3339()
        ));
    }
    None
}

fn comment_header(request: &FinalRecoveryRequest, decision: &FinalRecoveryDecision) -> String {
    format!(
        "final_recovery run_id={} decision={}",
        request.run_id,
        decision.kind()
    )
}
