//! Task coupling-out on run terminalization: when a job run reaches a terminal
//! *failure* state (`failed`, `timeout`) or is reconciled `interrupted`, every
//! task coupled to that run — stamped with its `job_run_id` during
//! `worktree_setup` — is moved to `blocked` so a human/orchestrator has to look
//! before anything runs again. Operator cancellation also blocks by default
//! for older/internal callers; an explicit persisted operator policy can
//! instead return the task to backlog with its cancellation reason.
//! A backlog task whose latest status decision is this run's final-recovery
//! requeue is preserved: recovery already authorized another attempt.
//! Resumes keep the checkpoint batch's `job_run_id` for delivery, but their
//! readmission history records the resumed run and that batch binding. Cleanup
//! uses the latest matching readmission as the coupled owner instead, including
//! when the resumed worker never starts. Review and withdrawal protections
//! still apply, and an older run cannot overwrite a newer resume's decision.
//!
//! [ORB-14258] A run that failed because a required command fails on its
//! base exactly as on the candidate (`[baseline_red]`) holds its tasks in the
//! backlog instead (`baseline_red_hold`): nothing about the work is wrong, and
//! admission releases the hold once the required command passes on a new base.
//! A task the run's failure
//! handoff already held is left as it is. Since [ORB-15202] such a run ends
//! `held`, not `failed`, and its tasks are held exactly as before.
//!
//! [ORB-14266] A run that failed on its provider — `[provider_capacity]`,
//! `[provider_unavailable]` or `[provider_refusal]` — did not judge the work
//! either. Its tasks go back to the backlog under a `provider_failure_hold`
//! that excludes the failing crews until a backoff passes, and admission
//! draws another crew or defers (see `provider_hold`).
//!
//! A run whose agent an Orbit upgrade refused mid-step (`[upgrade_pending]`)
//! did not judge the work either. Its failure handoff kept the candidate, so
//! its tasks go back to the backlog (`upgrade_pending_requeued`), and the
//! next run resumes that candidate once the upgrade settles.
//!
//! This is the symmetric counterpart to the coupling-in that
//! `worktree_setup` performs (stamping `job_run_id` and moving tasks to
//! `in_progress`). The update comes from the engine's
//! `blocked_workflow_failure_update`, which owns the status event
//! (`workflow_run_failed`) and the note format.
//!
//! [ORB-12969] An `interrupted` run blocks its tasks too, through
//! `blocked_workflow_interruption_update` (`workflow_run_interrupted`, same
//! note shape plus the resume command). Leaving them `in-progress` hid work
//! stranded by a host reboot behind a status identical to healthy in-flight
//! work. The distinct event tells an operator to resume rather than diagnose,
//! and resume re-admits the task (`application::job::resume`), so the block
//! costs the recovery path nothing.
//!
//! `blocked` is a deliberate dead end for automation: workflow admission
//! accepts only `backlog` and `in-progress`, so the ship sweep skips these
//! tasks. The ways out are a human/orchestrator decision (moving the task
//! to `backlog` or `in-progress` with `orbit.task.update`), a final-recovery
//! requeue, or resuming the run that blocked it.
//!
//! Some failures are the host's, not the task's: dispatch could not find the
//! provider launcher. That error is permanent for its run, but installing the
//! launcher clears it, and nothing else would ever re-evaluate the block.
//! [`OrbitRuntime::infra_blocked_tasks`] classifies those blocks from the
//! failure note and re-resolves the launcher now, so `orbit doctor` can report
//! cleared ones and `orbit task recheck-blocked --confirm` can return them to
//! backlog. Every other block keeps the human decision described above.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::activity_job::cli_runner::missing_launcher_in;
use orbit_engine::{
    RuntimeHost, TaskAutomationUpdate, WORKFLOW_RUN_FAILED_EVENT, baseline_red_hold_update,
    blocked_workflow_failure_update, blocked_workflow_interruption_update,
};
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    BASELINE_RED_HOLD_EVENT, BaselineRedHold, HeldFailure, JobRun, JobRunState,
    ProviderFailureClass, UPGRADE_PENDING_REQUEUED_EVENT, is_baseline_red_failure,
    is_upgrade_pending,
};

use crate::OrbitRuntime;

use super::FINAL_RECOVERY_REQUEUED_EVENT;
use super::provider_hold::{held_by_run, provider_failure_hold_update};

pub(crate) const WORKFLOW_RUN_CANCELLED_EVENT: &str = "workflow_run_cancelled";

/// A blocked task whose block was caused by host configuration — its run
/// failed because dispatch could not find the provider launcher — rather than
/// by the task's own work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfraBlockedTask {
    pub task_id: String,
    pub title: String,
    /// When the blocking history entry was recorded; identifies the block, so
    /// a requeue can confirm it is still the one it classified.
    pub blocked_at: DateTime<Utc>,
    pub run_id: Option<String>,
    /// Launcher program named by the failure, as the executor configured it.
    pub program: String,
    pub provider: String,
    /// Where dispatch from this workspace would launch `program` now, or
    /// `None` while the condition still reproduces.
    pub launcher: Option<PathBuf>,
}

/// Terminal run states that strand a coupled task and therefore trigger task
/// cleanup. `Interrupted` is included [ORB-12969]: the run is
/// resumable from its step checkpoints, but nothing resumes it on its own, so
/// its task must not keep looking like live work. Resume re-admits the blocked
/// task, so blocking it does not get in the way of the resume.
///
/// `Held` is included for a run held on a failure it did not cause
/// [ORB-15202] (see [`HeldFailure`]): its tasks get the disposition the
/// failure always had. A run held for review evidence or the forge is left to
/// its own resumption.
pub(crate) fn run_state_blocks_coupled_tasks(state: JobRunState) -> bool {
    matches!(
        state,
        JobRunState::Failed
            | JobRunState::Timeout
            | JobRunState::Cancelled
            | JobRunState::Interrupted
            | JobRunState::Held
    )
}

/// Task statuses that should NOT be touched when their run terminalizes as a
/// failure: `Done`/`Archived`/`Rejected` are terminal or human decisions,
/// `Review` was already shipped (don't clobber it), and `Blocked` is already
/// where we want it (keeps the transition idempotent).
///
/// [ORB-11305] `Proposed` and `Someday` join them. Both are withdrawals — the
/// way a human takes work back out of the backlog — and a withdrawal is
/// routinely what *causes* the run to be cancelled. Cleanup that ran after it
/// would replace the human's decision with `blocked`, so the withdrawal would
/// have to be re-applied by hand once the run finished unwinding. Only a task
/// that is still `in-progress` (or already `backlog`) under the failed run is
/// this transition's business.
fn task_is_blockable_on_run_failure(status: TaskStatus) -> bool {
    !matches!(
        status,
        TaskStatus::Done
            | TaskStatus::Review
            | TaskStatus::Blocked
            | TaskStatus::Rejected
            | TaskStatus::Archived
            | TaskStatus::Proposed
            | TaskStatus::Someday
    )
}

/// The latest resume's cleanup owner, only while its recorded batch binding
/// still matches the task. A later restamp invalidates the old coupling.
pub(crate) fn resumed_task_run_id<'a>(
    history: &'a [TaskHistoryEntry],
    owner_run_id: &str,
) -> Option<&'a str> {
    let entry = history.iter().rev().find(|entry| {
        entry.by == "system"
            && matches!(
                entry.event.as_str(),
                "resume_readmitted" | "resume_review_restored"
            )
    })?;
    let note = entry.note.as_deref()?;
    let (note, owner) = note.rsplit_once("; owner_run_id=")?;
    if owner != owner_run_id {
        return None;
    }
    note.strip_prefix("resume lineage reconciliation: run '")?
        .split_once("' resumes '")
        .map(|(run, _)| run)
}

/// Extract `(error_code, error_message)` for the failure note from the run's
/// most recent errored step. The pipeline records a single job-level
/// diagnostic step whose message already names the failing step, so surfacing
/// it verbatim keeps the note actionable.
pub(crate) fn failed_run_error_context(run: &JobRun) -> (Option<String>, Option<String>) {
    run.steps
        .iter()
        .rev()
        .find(|step| step.error_code.is_some() || step.error_message.is_some())
        .map(|step| (step.error_code.clone(), step.error_message.clone()))
        .unwrap_or((None, None))
}

/// The entry that put the task in its current block, when that entry is a
/// workflow failure naming a missing provider launcher. A later block of any
/// other kind (an operator's, another run's failure) supersedes it.
fn launcher_block(history: &[TaskHistoryEntry]) -> Option<(&TaskHistoryEntry, String, String)> {
    let entry = history
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(TaskStatus::Blocked))?;
    if entry.event != WORKFLOW_RUN_FAILED_EVENT {
        return None;
    }
    let missing = missing_launcher_in(entry.note.as_deref()?)?;
    Some((entry, missing.program, missing.provider))
}

impl OrbitRuntime {
    /// Blocked tasks in this workspace whose block is a missing provider
    /// launcher, each with where that launcher resolves now. Resolution uses
    /// dispatch's own lookup, from this process's `PATH` and `HOME`.
    pub fn infra_blocked_tasks(&self) -> Result<Vec<InfraBlockedTask>, OrbitError> {
        let blocked =
            self.list_tasks_filtered(Some(TaskStatus::Blocked), None, None, None, None, None)?;
        let mut infra_blocked = Vec::new();
        for task in blocked {
            if let Some(entry) = self.infra_block_of(&task)? {
                infra_blocked.push(entry);
            }
        }
        Ok(infra_blocked)
    }

    /// Classify one task's current block; `None` unless it is blocked by a
    /// missing provider launcher.
    pub(crate) fn infra_block_of(
        &self,
        task: &Task,
    ) -> Result<Option<InfraBlockedTask>, OrbitError> {
        if task.status != TaskStatus::Blocked {
            return Ok(None);
        }
        let history = self.get_task_history(&task.id)?;
        let Some((entry, program, provider)) = launcher_block(&history) else {
            return Ok(None);
        };
        Ok(Some(InfraBlockedTask {
            task_id: task.id.clone(),
            title: task.title.clone(),
            blocked_at: entry.at,
            run_id: task.job_run_id.clone(),
            launcher: self.locate_provider_launcher(&program),
            program,
            provider,
        }))
    }

    /// Best-effort variant used from run terminalization: a status-write
    /// failure is logged and swallowed so the run still reaches its terminal
    /// state and releases its reservations/file locks.
    ///
    /// `diagnostic` is the `(error_code, message)` a caller knows before its
    /// diagnostic step is durable — orphan reconciliation records that step
    /// only after the terminal write wins. Without it the note falls back to
    /// the run's most recent errored step.
    pub(crate) fn best_effort_block_tasks_for_terminal_run(
        &self,
        run_id: &str,
        state: JobRunState,
        diagnostic: Option<(&str, &str)>,
    ) {
        if let Err(error) = self.block_tasks_for_terminal_run(run_id, state, diagnostic) {
            tracing::warn!(
                run_id,
                state = %state,
                "failed to block tasks coupled to terminal job run: {error}"
            );
        }
    }

    fn block_tasks_for_terminal_run(
        &self,
        run_id: &str,
        state: JobRunState,
        diagnostic: Option<(&str, &str)>,
    ) -> Result<(), OrbitError> {
        // A claimed leaf does not block its own task. The claim's settlement
        // is the one terminal-failure transition the owner accepts: the leaf's
        // bound worker (or any later settle pass) delivers it with the run's
        // failure as evidence, atomically blocking the task, fencing the claim
        // and releasing its reservation (distributed-drain design §3,
        // "Settlement"). A generic blocked update from the worker carries no
        // such evidence and is refused (`failure settlement requires
        // evidence`).
        if self
            .worker_invocation()
            .is_some_and(|binding| binding.bound_run_id == run_id)
        {
            tracing::debug!(
                run_id,
                state = %state,
                "claimed leaf terminalized; its bound worker or a later settle pass delivers the claim settlement"
            );
            return Ok(());
        }
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(());
        };
        let (error_code, error_message) = match diagnostic {
            Some((code, message)) => (Some(code.to_string()), Some(message.to_string())),
            None => failed_run_error_context(&run),
        };
        // [ORB-15202] A run held on a red base or an awaited decision couples
        // out exactly as the failure it records; any other hold names its own
        // resumption and leaves its tasks alone.
        let state = match state {
            JobRunState::Held
                if HeldFailure::of(error_code.as_deref(), error_message.as_deref()).is_some() =>
            {
                JobRunState::Failed
            }
            JobRunState::Held => return Ok(()),
            state => state,
        };
        let blocked_update = if state == JobRunState::Interrupted {
            blocked_workflow_interruption_update
        } else {
            blocked_workflow_failure_update
        };
        // [ORB-14258] A run that failed because a required command is red on
        // its base holds its tasks in the backlog instead of blocking them.
        let hold = (state == JobRunState::Failed)
            .then(|| {
                error_message
                    .as_deref()
                    .filter(|message| is_baseline_red_failure(error_code.as_deref(), Some(message)))
                    .and_then(BaselineRedHold::from_text)
            })
            .flatten()
            .map(|mut hold| {
                if hold.run_id.is_empty() {
                    hold.run_id = run_id.to_string();
                }
                hold
            });
        // [ORB-14266] Nor does a run its provider failed; its tasks wait in
        // the backlog for another crew instead.
        let provider_failure = (state == JobRunState::Failed && hold.is_none())
            .then(|| ProviderFailureClass::of(error_code.as_deref(), error_message.as_deref()))
            .flatten();
        // Nor does a run whose agent an Orbit upgrade refused mid-step.
        let upgrade_pending = state == JobRunState::Failed
            && is_upgrade_pending(error_code.as_deref(), error_message.as_deref());
        let task_cancellation_policy = if state == JobRunState::Cancelled {
            self.read_run_state(run_id)?
                .and_then(|state| state.task_cancellation_policy)
        } else {
            None
        };
        let mut tasks: BTreeMap<_, _> = self
            .list_run_tasks(run_id)?
            .into_iter()
            .map(|task| (task.id.clone(), task))
            .collect();
        if run.retry_source_run_id.is_some() {
            // A resume can readmit tasks without an input task list, and can
            // retain a descendant's binding when no checkpoint is reused.
            // Enumerate blockable statuses, then require this run's exact
            // durable coupling under the lock; lineage alone is not ownership.
            for status in [TaskStatus::InProgress, TaskStatus::Backlog] {
                for task in self.list_tasks_filtered(Some(status), None, None, None, None, None)? {
                    tasks.entry(task.id.clone()).or_insert(task);
                }
            }
        }
        let requeue_note_prefix = format!("final recovery (run_id={run_id}): ");
        for task in tasks.into_values() {
            // Recovery and cleanup serialize the decision with the status
            // write. Re-read the binding too: another run may have admitted
            // this task since list_run_tasks took its snapshot.
            let result = self
                .stores()
                .tasks()
                .with_task_write_lock(&task.id, &mut || {
                    let current = self.get_task(&task.id)?;
                    if !task_is_blockable_on_run_failure(current.status) {
                        return Ok(());
                    }
                    // Run ids are machine-local, just as in list_run_tasks.
                    if current.job_run_machine.as_ref().is_some_and(|bound| {
                        run.executed_on
                            .as_ref()
                            .is_none_or(|local| local.machine_id != bound.machine_id)
                    }) {
                        return Ok(());
                    }
                    let history = self.get_task_history(&task.id)?;
                    let coupled_run = current.job_run_id.as_deref().map(|owner| {
                        resumed_task_run_id(&history, owner).unwrap_or(owner)
                    });
                    if coupled_run != Some(run_id) {
                        return Ok(());
                    }
                    // The task event, written with the requeue or the
                    // failure handoff's baseline hold, survives even when
                    // recording its run-state outcome failed. An older
                    // decision or another run's grants no exemption.
                    if current.status == TaskStatus::Backlog
                        && history
                            .iter()
                            .rev()
                            .find(|entry| {
                                entry.to_status.is_some()
                                    || entry.event == FINAL_RECOVERY_REQUEUED_EVENT
                                    || entry.event == BASELINE_RED_HOLD_EVENT
                            })
                            .is_some_and(|entry| {
                                let note = entry.note.as_deref().unwrap_or_default();
                                (entry.event == FINAL_RECOVERY_REQUEUED_EVENT
                                    && note.starts_with(&requeue_note_prefix))
                                    || (entry.event == BASELINE_RED_HOLD_EVENT
                                        && BaselineRedHold::from_text(note)
                                            .is_some_and(|held| held.run_id == run_id))
                            })
                    {
                        return Ok(());
                    }
                    // A replayed finalization finds this run's provider hold.
                    if current.status == TaskStatus::Backlog
                        && held_by_run(&history, run_id)
                    {
                        return Ok(());
                    }
                    // A review timeout has already requeued a continuation;
                    // an external-evidence hold has a named resumption condition.
                    // Do not turn either decision into an operator-only block.
                    if history
                        .iter()
                        .rev()
                        .find(|entry| {
                            entry.to_status.is_some()
                                || matches!(
                                    entry.event.as_str(),
                                    "review_timeout_incomplete"
                                        | "review_awaiting_evidence"
                                        | "review_evidence_received"
                                )
                        })
                        .is_some_and(|entry| {
                            matches!(
                                entry.event.as_str(),
                                "review_timeout_incomplete"
                                    | "review_awaiting_evidence"
                                    | "review_evidence_received"
                            ) && entry.note.as_deref().is_some_and(|note| {
                                note.contains(&format!("run={run_id},"))
                                    || note.starts_with(&format!("run={run_id};"))
                            })
                        })
                    {
                        return Ok(());
                    }
                    if upgrade_pending && hold.is_none() && provider_failure.is_none() {
                        return self.apply_task_automation_update(
                            &task.id,
                            TaskAutomationUpdate {
                                status: Some(TaskStatus::Backlog),
                                status_event: Some(UPGRADE_PENDING_REQUEUED_EVENT.to_string()),
                                status_note: Some(format!(
                                    "workflow run stopped by an Orbit upgrade: job={}, \
                                     run_id={run_id}; an upgrade refused the step's agent, so \
                                     the task is back in the backlog and its next run resumes \
                                     the held candidate once the upgrade settles",
                                    run.job_id
                                )),
                                ..TaskAutomationUpdate::default()
                            },
                        );
                    }
                    let update = if state == JobRunState::Cancelled
                        && let Some(policy) = task_cancellation_policy
                            .as_ref()
                            .filter(|policy| !policy.block)
                    {
                        TaskAutomationUpdate {
                            status: Some(TaskStatus::Backlog),
                            status_event: Some(WORKFLOW_RUN_CANCELLED_EVENT.to_string()),
                            status_note: Some(format!(
                                "workflow run cancelled: job={}, run_id={}; returned to backlog; {}",
                                run.job_id, run_id, policy.note
                            )),
                            ..TaskAutomationUpdate::default()
                        }
                    } else {
                        match (&hold, provider_failure) {
                            (Some(hold), _) => baseline_red_hold_update(&run.job_id, hold),
                            (None, Some(class)) => provider_failure_hold_update(
                                &run.job_id,
                                &self.provider_failure_hold(
                                    &current,
                                    &run,
                                    class,
                                    error_message.as_deref(),
                                    Utc::now(),
                                )?,
                            ),
                            (None, None) => blocked_update(
                                &run.job_id,
                                run_id,
                                error_code.as_deref(),
                                error_message.as_deref(),
                            ),
                        }
                    };
                    self.apply_task_automation_update(&task.id, update)
                });
            // Per-task best-effort: one task's write failure must not strand the
            // rest of the bundle.
            if let Err(error) = result {
                tracing::warn!(
                    run_id,
                    task_id = %task.id,
                    "failed to block task coupled to terminal job run: {error}"
                );
            }
        }
        Ok(())
    }
}
