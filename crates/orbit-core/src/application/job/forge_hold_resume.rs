//! [ORB-14617] Clock retry of runs held because the forge refused their push.
//!
//! A delivery push the forge kept refusing for a server-side reason ends its
//! run `held` with a [`ForgeUnavailableHold`] in its state, rather than
//! failing it. Each clock tick resumes every such run that no retry has
//! resumed yet: the resumed run skips its checkpointed steps, so it pushes
//! the same candidate head and continues to the pull request without
//! implementing or reviewing again. A resumed run that the forge still
//! refuses holds again, keeping the lineage's first hold time.
//!
//! The retry is bounded by [`FORGE_HOLD_RETRY_WINDOW`] from that first hold.
//! Past it the clock stops retrying and blocks the run's coupled in-progress tasks
//! with [`FORGE_UNAVAILABLE_EXPIRED_EVENT`], naming the held run so that
//! resuming it by hand re-admits them. The recorded expiry makes later ticks
//! leave this hold alone, including tasks an operator has since re-queued.

use chrono::{DateTime, TimeDelta, Utc};
use orbit_common::OrbitError;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    FORGE_UNAVAILABLE_EXPIRED_EVENT, ForgeUnavailableHold, JobRun, JobRunState,
};

use crate::OrbitRuntime;
use crate::application::job::{RunOwnerLiveness, run_owner_liveness};
use crate::runtime::task::resumed_task_run_id;

use super::resume::task_ids_from_input;

/// How long after a lineage's first hold the clock keeps resuming it.
pub(crate) const FORGE_HOLD_RETRY_WINDOW: TimeDelta = TimeDelta::hours(2);

const CLOCK_ACTOR: &str = "clock";

/// What one clock pass did with forge-held runs.
#[derive(Debug, Default)]
pub(crate) struct ForgeHoldTick {
    /// Runs resumed this pass.
    pub(crate) resumed: Vec<String>,
    /// Tasks blocked because their run's hold outlasted the retry window.
    pub(crate) expired: Vec<String>,
}

impl OrbitRuntime {
    /// Resume each run held for the forge within its retry window; block the
    /// in-progress tasks of one whose window has closed.
    pub(crate) fn auto_resume_forge_held_runs(
        &self,
        now: DateTime<Utc>,
    ) -> Result<ForgeHoldTick, OrbitError> {
        let mut tick = ForgeHoldTick::default();
        if self.is_write_free() {
            return Ok(tick);
        }
        let held = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            state: Some(JobRunState::Held),
            ..JobRunQuery::default()
        })?;
        for run in held {
            let Some(state) = self.read_run_state(&run.run_id)? else {
                continue;
            };
            if state.forge_hold_expired_at.is_some() {
                continue;
            }
            let Some(hold) = state.forge_hold else {
                continue;
            };
            // A retry already resumed this run; that retry is the lineage's
            // current attempt, held again or not.
            if !self
                .stores()
                .jobs()
                .job_run_retries(&run.run_id, 1)?
                .is_empty()
                || run_owner_liveness(&run) == RunOwnerLiveness::Alive
                // The owner's claim recovery re-admits a claimed execution.
                || self.is_claimed_execution(&run.run_id)?
            {
                continue;
            }
            if now.signed_duration_since(hold.held_since) > FORGE_HOLD_RETRY_WINDOW {
                // One run's failed expiry stays unacknowledged for the next
                // tick; it must not starve the other held runs of this pass.
                match self.expire_forge_hold(&run, &hold, now) {
                    Ok(blocked) => tick.expired.extend(blocked),
                    Err(error) => tracing::warn!(
                        target: "orbit.core.sweep",
                        run_id = %run.run_id,
                        error = %error,
                        "could not expire a run's forge hold",
                    ),
                }
                continue;
            }
            match self.submit_resume_run(&run.run_id, Some(CLOCK_ACTOR), None) {
                Ok(invoke) => {
                    tracing::info!(
                        target: "orbit.core.sweep",
                        source_run_id = %run.run_id,
                        resumed_run_id = %invoke.run_id,
                        held_since = %hold.held_since,
                        "clock tick resumed a run held for the forge",
                    );
                    tick.resumed.push(invoke.run_id);
                }
                // Not a decision: the next tick inside the window retries.
                Err(error) => tracing::warn!(
                    target: "orbit.core.sweep",
                    source_run_id = %run.run_id,
                    error = %error,
                    "failed to resume a run held for the forge",
                ),
            }
        }
        Ok(tick)
    }

    /// Block the held run's in-progress tasks for a human. Returns the ids
    /// it blocked; a task an operator or another run already moved is left.
    fn expire_forge_hold(
        &self,
        run: &JobRun,
        hold: &ForgeUnavailableHold,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>, OrbitError> {
        let mut blocked = Vec::new();
        let task_ids = run
            .input
            .as_ref()
            .and_then(task_ids_from_input)
            .unwrap_or_default();
        for task_id in task_ids {
            let note = format!(
                "{FORGE_UNAVAILABLE_EXPIRED_EVENT}: run={}; the forge has refused the push of {} \
                 to {} since {}, longer than the clock retries; the candidate and its worktree \
                 are kept, and `orbit job resume {}` pushes it again",
                run.run_id,
                hold.head_sha,
                hold.target_ref,
                hold.held_since.to_rfc3339(),
                run.run_id,
            );
            let update = TaskAutomationUpdate {
                expected_status: Some(TaskStatus::InProgress),
                status: Some(TaskStatus::Blocked),
                status_event: Some(FORGE_UNAVAILABLE_EXPIRED_EVENT.to_string()),
                status_note: Some(note),
                ..TaskAutomationUpdate::default()
            };
            // The admission binding can change after the run list was read.
            // Keep the ownership decision and block under the same task lock,
            // including a resume that retained its checkpoint's batch id.
            let mut changed = false;
            self.stores()
                .tasks()
                .with_task_write_lock(&task_id, &mut || {
                    let current = self.get_task(&task_id)?;
                    if current.status != TaskStatus::InProgress
                        || current.job_run_machine.as_ref().is_some_and(|bound| {
                            run.executed_on
                                .as_ref()
                                .is_none_or(|local| local.machine_id != bound.machine_id)
                        })
                    {
                        return Ok(());
                    }
                    let history = self.get_task_history(&task_id)?;
                    let coupled_run = current
                        .job_run_id
                        .as_deref()
                        .map(|owner| resumed_task_run_id(&history, owner).unwrap_or(owner));
                    if coupled_run != Some(run.run_id.as_str()) {
                        return Ok(());
                    }
                    // Task history also protects an already-applied expiry if
                    // recording the run acknowledgement failed or was interrupted.
                    let expired_note_prefix =
                        format!("{FORGE_UNAVAILABLE_EXPIRED_EVENT}: run={};", run.run_id);
                    if history.iter().any(|entry| {
                        entry.event == FORGE_UNAVAILABLE_EXPIRED_EVENT
                            && entry
                                .note
                                .as_deref()
                                .is_some_and(|note| note.starts_with(&expired_note_prefix))
                    }) {
                        return Ok(());
                    }
                    self.apply_task_automation_update(&task_id, update.clone())?;
                    changed = true;
                    Ok(())
                })?;
            if changed {
                tracing::warn!(
                    target: "orbit.core.sweep",
                    run_id = %run.run_id,
                    task_id = %task_id,
                    held_since = %hold.held_since,
                    "blocked a task whose push the forge refused past the retry window",
                );
                blocked.push(task_id);
            }
        }
        // Preserve the hold so `orbit job resume` remains available. Mark it
        // only after task decisions succeeded; partial expiry can retry safely
        // using the per-task history above, without blocking or logging twice.
        self.stores()
            .jobs()
            .update_run_state(&run.run_id, &mut |_, state| {
                state.forge_hold_expired_at = Some(now);
                Ok(())
            })?;
        Ok(blocked)
    }
}
