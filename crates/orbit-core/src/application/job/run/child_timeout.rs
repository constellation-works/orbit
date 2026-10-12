//! Stop a blocking child whose parent's wait on it expired [ORB-15643].
//!
//! A parent that gives up waiting must not leave its child running: the run
//! graph would end with terminal parents and live work nobody joins. The
//! parent cancels the child instead. Before the cancelled child is finalized,
//! the host that stopped it runs the child job's failure handoff under
//! [`CHILD_TIMEOUT_ERROR_CODE`], which keeps the candidate the way any other
//! unjudged candidate is kept: committed, carried to its durable ref, and
//! recorded for the task's next run to resume. The cancellation then returns
//! the task to the backlog with a note naming the timeout and that candidate.

use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_types::workflow::{
    CHILD_TIMEOUT_ERROR_CODE, JobRun, JobV2Step, JobV2StepBody, TaskCancellationPolicy,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::actions::{CancellationRequest, cancellation_note};
use super::owner::signal_run_owner_process;
use super::types::JobRunCancelResult;

/// `source` recorded on the cancellation of a child its parent stopped
/// waiting for.
const CHILD_TIMEOUT_SOURCE: &str = "parent_wait_timeout";

/// The failure activity whose handoff keeps a timed-out child's candidate.
const FAILURE_HANDOFF: &str = "pr_failure_handoff";

impl OrbitRuntime {
    /// Cancel `child_run_id` because its parent's wait on it expired, keeping
    /// its candidate. Its task goes back to the backlog with `reason` and the
    /// kept candidate in the note, and a blocking child of its own is
    /// cancelled the same way.
    pub(crate) fn cancel_timed_out_child(
        &self,
        child_run_id: &str,
        reason: &str,
    ) -> Result<JobRunCancelResult, OrbitError> {
        self.cancel_job_run_cascading(
            child_run_id,
            CancellationRequest {
                actor: "orbit",
                source: CHILD_TIMEOUT_SOURCE,
                reason: Some(reason),
                block_task: false,
                keep_candidate: true,
            },
            signal_run_owner_process,
            0,
        )
    }

    /// Run `run`'s failure handoff under [`CHILD_TIMEOUT_ERROR_CODE`] once
    /// its worker is stopped, and name what it kept in the task note the
    /// cancellation settles with. Best effort: the run is cancelled whether
    /// or not its candidate could be kept.
    pub(super) fn keep_timed_out_candidate(&self, run: &JobRun, request: CancellationRequest<'_>) {
        let reason = request
            .reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .unwrap_or(CHILD_TIMEOUT_ERROR_CODE);
        let kept = match self.try_keep_timed_out_candidate(run, reason) {
            Ok(kept) => kept,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.job_run",
                    run_id = %run.run_id,
                    %error,
                    "the timed-out child's candidate could not be kept"
                );
                Some(format!("its candidate could not be kept: {error}"))
            }
        };
        let Some(kept) = kept else {
            return;
        };
        let note = cancellation_note(request.actor, Some(&format!("{reason}; {kept}")));
        if let Err(error) = self
            .stores()
            .jobs()
            .update_run_state(&run.run_id, &mut |_, state| {
                state.task_cancellation_policy = Some(TaskCancellationPolicy {
                    block: request.block_task,
                    note: note.clone(),
                });
                Ok(())
            })
        {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id = %run.run_id,
                %error,
                "the timed-out child's cancellation note could not name its candidate"
            );
        }
    }

    /// What the handoff kept, as a clause for the task note; `None` when the
    /// run's job keeps no candidate this way or it never set up a worktree.
    fn try_keep_timed_out_candidate(
        &self,
        run: &JobRun,
        reason: &str,
    ) -> Result<Option<String>, OrbitError> {
        let Some(state) = self.read_run_state(&run.run_id)? else {
            return Ok(None);
        };
        if let Some(checkpoint) = &state.failure_activity_checkpoint {
            return Ok(Some(kept_clause(&checkpoint.output)));
        }
        if state.pipeline.get("worktree").is_none_or(Value::is_null) {
            return Ok(None);
        }
        let (_, job) = self.resolve_run_definition(run)?;
        if job.failure_activity.as_deref() != Some(FAILURE_HANDOFF) {
            return Ok(None);
        }
        let Some(step) = unfinished_step(&job.steps, &state.pipeline) else {
            return Ok(None);
        };
        let input = json!({
            "failed_step_id": step.id,
            "activity_name": step_activity_name(step),
            "error_code": CHILD_TIMEOUT_ERROR_CODE,
            "error_message": reason,
            "job_input": run.input.clone().unwrap_or_else(|| json!({})),
            "pipeline": state.pipeline,
            "run_id": run.run_id,
        });
        let output = orbit_engine::execute_deterministic_action(
            self,
            FAILURE_HANDOFF,
            &json!({}),
            &input,
            false,
            &HashMap::new(),
            None,
        )?;
        self.stores()
            .jobs()
            .update_run_state(&run.run_id, &mut |_, state| {
                state.record_failure_activity(
                    FAILURE_HANDOFF.to_string(),
                    step.id.clone(),
                    output.clone(),
                );
                Ok(())
            })?;
        Ok(Some(kept_clause(&output)))
    }
}

/// The candidate a child's failure handoff recorded, for a parent's wait
/// result: its branch, head and durable ref.
pub(crate) fn kept_candidate(handoff: &Value) -> Option<Value> {
    let head_sha = handoff.get("head_sha").and_then(Value::as_str)?;
    Some(json!({
        "head_sha": head_sha,
        "branch": handoff.get("branch"),
        "durable_ref": handoff.get("durable_ref"),
        "decision": handoff.get("decision"),
    }))
}

/// "candidate `<sha>` kept on branch `<branch>`", with where it was carried.
/// A child stopped after it published keeps its pull request instead, and
/// its task stays in review.
pub(crate) fn kept_clause(handoff: &Value) -> String {
    let Some(candidate) = kept_candidate(handoff) else {
        return match handoff.get("pr_number").and_then(Value::as_str) {
            Some(number) => format!("its pull request #{number} stays open"),
            None => "it kept no candidate".to_string(),
        };
    };
    let place = match candidate["durable_ref"].as_str() {
        Some(reference) => format!(" and on origin at `{reference}`"),
        None => " on this host only".to_string(),
    };
    format!(
        "candidate `{}` kept on branch `{}`{place}",
        candidate["head_sha"].as_str().unwrap_or_default(),
        candidate["branch"].as_str().unwrap_or_default(),
    )
}

/// The first top-level step after the last one whose output the run
/// recorded: the step it was running when stopped. A step a `when:` skipped
/// records nothing either, so this can name an earlier step than the running
/// one, never a later one: a resume then redoes work rather than skips it.
fn unfinished_step<'a>(steps: &'a [JobV2Step], pipeline: &Value) -> Option<&'a JobV2Step> {
    let next = steps
        .iter()
        .rposition(|step| {
            pipeline
                .get(&step.id)
                .is_some_and(|output| !output.is_null())
        })
        .map_or(0, |last| last + 1);
    steps.get(next).or_else(|| steps.last())
}

fn step_activity_name(step: &JobV2Step) -> String {
    match &step.body {
        JobV2StepBody::Target(target) => target
            .activity_name
            .clone()
            .unwrap_or_else(|| step.id.clone()),
        JobV2StepBody::TargetRef(target) => target
            .target
            .strip_prefix("activity:")
            .unwrap_or(&target.target)
            .to_string(),
        JobV2StepBody::Parallel { .. } => "parallel".to_string(),
        JobV2StepBody::FanOut { .. } => "fan_out".to_string(),
        JobV2StepBody::Loop { .. } => "loop".to_string(),
    }
}
