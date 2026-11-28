//! Authenticating a published backlog return and an operator requeue's history.

use orbit_common::OrbitError;
use orbit_types::task::{CANDIDATE_DISCARDED_EVENT, RecordedFinalRecovery, Task, TaskHistoryEntry};
use orbit_types::workflow::SourceRepairBinding;

use super::{HandoffReceipt, operator_discard_hint};
use crate::OrbitRuntime;
use crate::application::contracts::ConflictSourceRepairRoute;
use crate::application::task::TaskJobPort;
use crate::application::task::helpers;
use orbit_types::task::TaskStatus;

impl OrbitRuntime {
    /// Authenticate a plain blocked/review-to-backlog status update without a
    /// discard, exactly as an operator requeue does. Returns the
    /// publication-preservation record to append with the status change
    /// (`None` when the same record is already on file or nothing was
    /// published). A refusal leaves the task in its current status with
    /// nothing written.
    pub(crate) fn authenticate_published_backlog_return(
        &self,
        task: &Task,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        self.authenticate_published_backlog_return_with(task, None, false)
    }

    pub(crate) fn authenticate_published_backlog_return_with(
        &self,
        task: &Task,
        history: Option<&[TaskHistoryEntry]>,
        observational: bool,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        // No bound publishing run: there is no publication this move could
        // resume or strand (a bare pull-request record on a task that was
        // blocked by hand is the PR-closure path's concern, not a stopped
        // run's candidate).
        if task.job_run_id.is_none() {
            self.kept_claim_requeue_with(task, history, observational)?;
            return Ok(None);
        }
        let receipt = self.handoff_receipt(task)?;
        if let Some((preserved, _binding)) =
            self.authenticate_operator_requeue_with(task, receipt.as_ref(), history, observational)?
        {
            return Ok(preserved);
        }
        // A kept claim candidate the specification has outgrown is refused
        // the same way, as a requeue refuses it.
        self.kept_claim_requeue_with(task, history, observational)?;
        Ok(None)
    }

    /// Authenticate what an operator requeue without a discard resumes: the
    /// failure handoff's publication when there is one, else nothing, refusing
    /// exactly what the next admission would refuse. The preservation record
    /// is `None` when the task already holds that same record.
    /// Whether returning `task` to the backlog without `--discard-candidate` is
    /// already guaranteed to fail based solely on local state (without querying
    /// remotes or the forge), and the exact CLI command that can succeed.
    pub(crate) fn backlog_return_refused_locally(&self, task: &Task) -> Option<String> {
        let discard = format!(
            "orbit task update {} --status backlog --discard-candidate",
            task.id
        );
        let cancel_if_live = |run_id: &str| {
            self.observed_job_run(run_id)
                .ok()
                .filter(|run| !run.state.is_terminal())
                .map(|_| format!("orbit run cancel {run_id} --confirm"))
        };
        if let Ok(Some((live_run_id, _))) =
            crate::application::task::lifecycle::live_local_run_owning(self, &task.id)
        {
            return Some(format!("orbit run cancel {live_run_id} --confirm"));
        }
        if self.unsettled_run(task).ok().flatten().is_some() {
            if let Some(run_id) = task.job_run_id.as_deref()
                && let Some(cancel) = cancel_if_live(run_id)
            {
                return Some(cancel);
            }
            return Some(discard);
        }
        if task.job_run_id.is_none() {
            return self.kept_claim_requeue(task).is_err().then_some(discard);
        }
        if let Ok(Some(receipt)) = self.handoff_receipt(task) {
            if !task.spec_digest_matches(&receipt.task_spec_digest) {
                return Some(discard);
            }
            if let Err(err) = self.check_failure_handoff_requeue(task) {
                if err.source_repair_run_state_refusal().is_some()
                    && err.to_string().contains("orbit run cancel")
                    && let Some(cancel) = cancel_if_live(&receipt.run_id)
                {
                    return Some(cancel);
                }
                return Some(discard);
            }
            return None;
        }
        if self.run_published_through_pipeline(task).ok() == Some(true) {
            if let Err(err) = self.check_escalated_publication_requeue(task) {
                if err.source_repair_run_state_refusal().is_some()
                    && err.to_string().contains("orbit run cancel")
                    && let Some(run_id) = task.job_run_id.as_deref()
                    && let Some(cancel) = cancel_if_live(run_id)
                {
                    return Some(cancel);
                }
                return Some(discard);
            }
            return None;
        }
        if self.unbound_publication(task).ok() == Some(true)
            || self.kept_claim_requeue(task).is_err()
        {
            return Some(discard);
        }
        None
    }

    pub(super) fn authenticate_operator_requeue(
        &self,
        task: &Task,
        receipt: Option<&HandoffReceipt>,
    ) -> Result<Option<(Option<TaskHistoryEntry>, SourceRepairBinding)>, OrbitError> {
        self.authenticate_operator_requeue_with(task, receipt, None, false)
    }

    pub(super) fn authenticate_operator_requeue_with(
        &self,
        task: &Task,
        receipt: Option<&HandoffReceipt>,
        history: Option<&[TaskHistoryEntry]>,
        observational: bool,
    ) -> Result<Option<(Option<TaskHistoryEntry>, SourceRepairBinding)>, OrbitError> {
        let discard_hint = operator_discard_hint(&task.id, task.status);
        if let Some(receipt) = receipt {
            if !task.spec_digest_matches(&receipt.task_spec_digest) {
                return Err(OrbitError::InvalidInput(format!(
                    "the task specification changed after its run published pull request \
                     #{}, so that candidate no longer implements this task and cannot be \
                     resumed. Nothing was changed. To {discard_hint}.",
                    receipt.pull_request
                )));
            }
            // Every authentication refusal (an interrupted or timed-out
            // run, a newer execution, a foreign source, ...) has the same exit.
            let authenticated = match history.filter(|_| observational) {
                Some(history) => self
                    .observe_failure_handoff_preservation(task, history)
                    .and_then(|(entry, binding)| {
                        Ok((
                            self.observe_preservation_append(task, entry, history)?,
                            binding,
                        ))
                    }),
                None => self
                    .failure_handoff_preservation(task)
                    .and_then(|(entry, binding)| {
                        Ok((self.preservation_append(task, entry)?, binding))
                    }),
            };
            return authenticated
                .map(Some)
                .map_err(|error| match error.job_validation_message() {
                    Some(refusal)
                        if error.source_repair_run_state_refusal().is_some()
                            && refusal.contains("orbit run cancel") =>
                    {
                        OrbitError::InvalidInput(format!("{refusal}. Nothing was changed."))
                    }
                    Some(refusal) => OrbitError::InvalidInput(format!(
                        "{refusal}. Nothing was changed. To {discard_hint}."
                    )),
                    None => error,
                });
        }
        // The run pushed and opened its pull request itself: the requeue
        // resumes that pull request, as a final-recovery requeue would, once it
        // is still open at the recorded head with every required check passed.
        if self.run_published_through_pipeline(task)? {
            let authenticated =
                match history.filter(|_| observational) {
                    Some(history) => self
                        .observe_escalated_publication_preservation(task, history)
                        .and_then(|(entry, binding)| {
                            Ok((
                                self.observe_preservation_append(task, entry, history)?,
                                binding,
                            ))
                        }),
                    None => self.escalated_publication_preservation(task).and_then(
                        |(entry, binding)| Ok((self.preservation_append(task, entry)?, binding)),
                    ),
                };
            let error = match authenticated {
                Ok(pair) => return Ok(Some(pair)),
                Err(error) => error,
            };
            return Err(match error.job_validation_message() {
                // A refusal whose cause is a live non-terminal run:
                // discard fails until the run is cancelled.
                Some(refusal)
                    if error.source_repair_run_state_refusal().is_some()
                        && refusal.contains("orbit run cancel") =>
                {
                    OrbitError::InvalidInput(format!("{refusal}. Nothing was changed."))
                }
                // A refusal whose cause is the run's state or changed spec,
                // not the pull request's checks: retrying later cannot pass.
                Some(refusal)
                    if error.source_repair_run_state_refusal().is_some()
                        || refusal.contains("the task specification changed") =>
                {
                    OrbitError::InvalidInput(format!(
                        "{refusal}. Nothing was changed. To {discard_hint}."
                    ))
                }
                // A requeue cannot resume a conflicting pull request and
                // waiting does not clear a conflict: guide the operator to
                // the source-repair route that the current publication
                // state and rework policy admit, or explain when unavailable.
                Some(refusal) if refusal.contains("conflicts with its base") => {
                    let route = self.conflict_source_repair_route(task)?;
                    OrbitError::InvalidInput(conflict_recovery_message(
                        task,
                        &refusal,
                        route,
                        &discard_hint,
                    ))
                }
                Some(refusal) => OrbitError::InvalidInput(format!(
                    "{refusal}. Nothing was changed. The requeue resumes the pull request its \
                     run published once it is open at the recorded head with every required \
                     check passed: requeue again when they pass. If the pull request already \
                     merged, settle the task with `orbit task settle-merged`. Otherwise, \
                     {discard_hint}."
                )),
                None => error,
            });
        }
        let unbound = match history {
            Some(history) => self.observe_unbound_publication(task, history)?,
            None => self.unbound_publication(task)?,
        };
        if unbound {
            return Err(OrbitError::InvalidInput(format!(
                "the task has a pull request or preserved candidate on record that its stopped \
                 run's handoff does not bind, so its next run would refuse to start. Nothing \
                 was changed. Inspect it with explicit source repair, or {discard_hint}."
            )));
        }
        Ok(None)
    }
}

/// Whether the newest typed final-recovery decision Orbit recorded for
/// `task`'s current run is `requeued`, with no discard after it. Such a
/// requeue keeps the run's publication for the next run to resume, whichever
/// status event carried it (final recovery, an operator, the cleared-infra
/// requeue), so the task's moves back to the backlog must not close it.
pub(crate) fn current_run_requeue_stands(task: &Task, history: &[TaskHistoryEntry]) -> bool {
    let Some(run_id) = task.job_run_id.as_deref() else {
        return false;
    };
    let after_discard = history
        .iter()
        .rposition(|entry| entry.event == CANDIDATE_DISCARDED_EVENT)
        .map_or(0, |index| index + 1);
    helpers::authoritative_recovery_outcome(&history[after_discard..], run_id)
        == Some(&RecordedFinalRecovery::Requeued)
}

fn conflict_recovery_message(
    task: &Task,
    refusal: &str,
    route: ConflictSourceRepairRoute,
    discard_hint: &str,
) -> String {
    let run = task.job_run_id.as_deref().unwrap_or("<publishing-run>");
    let supersedes = if task.status == TaskStatus::Blocked {
        " It supersedes an open requeue request."
    } else {
        ""
    };
    match route {
        ConflictSourceRepairRoute::FieldFree => format!(
            "{refusal}. Nothing was changed. A requeue cannot resume a conflicting pull request. \
             Resolve the conflict with explicit source repair on run `{run}`: `orbit.run` action \
             `resume` with `source_repair` action `inspect` for this task, then `submit` the \
             returned binding with a `request_key` and a `diagnosis` naming the conflict, leaving \
             `unmet_acceptance` and `policy_digest` unset.{supersedes} Otherwise, {discard_hint}."
        ),
        ConflictSourceRepairRoute::ReworkRequired {
            adopt_retained_edits,
        } => {
            let inspect_action = if adopt_retained_edits {
                "action `inspect` (with `adopt_retained_edits: true` to carry retained uncommitted edits)"
            } else {
                "action `inspect`"
            };
            format!(
                "{refusal}. Nothing was changed. A requeue cannot resume a conflicting pull \
                 request. Resolve the conflict with explicit source repair on run `{run}`: \
                 `orbit.run` action `resume` with `source_repair` {inspect_action} for this \
                 task, then `submit` the returned binding with a `request_key`, a `diagnosis` \
                 naming the conflict, `unmet_acceptance` evidence on this published candidate, \
                 and the inspected `policy_digest`.{supersedes} Otherwise, {discard_hint}."
            )
        }
        ConflictSourceRepairRoute::Unavailable { source_state } => {
            let state_phrase = source_state
                .map(|state| format!(" for a `{state}` run"))
                .unwrap_or_default();
            format!(
                "{refusal}. Nothing was changed. A requeue cannot resume a conflicting pull \
                 request, and explicit source repair on run `{run}` is unavailable while this \
                 task is `{}`{state_phrase}. To {discard_hint}.",
                task.status
            )
        }
    }
}
