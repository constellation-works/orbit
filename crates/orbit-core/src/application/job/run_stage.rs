//! The delivery stage a task's run is on, for the task detail panel.
//! Derived only from durable run and step state plus the stage
//! declarations in the job definition the run executed. Detail-only: list
//! rows never pay the run and audit reads behind this projection.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{JobRun, JobStage, JobV2};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::contracts::ClaimedLeafStage;
use crate::application::job::run_display_steps;
use crate::application::task::{TaskRow, job_run_is_locally_navigable};

fn eligible(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::InProgress | TaskStatus::Review)
}

/// The stage list for a run whose leaf stage is all the owner knows.
fn claimed_stage_value(stage: ClaimedLeafStage, run_state: &str) -> Value {
    let (implementing, reviewing) = match stage {
        ClaimedLeafStage::Implementing => ("current", "future"),
        ClaimedLeafStage::Reviewing => ("past", "current"),
    };
    json!({
        "stage": stage.as_str(),
        "stage_label": stage.display_label(),
        "step_id": null,
        "step_state": null,
        "run_state": run_state,
        "started_at": null,
        "step_index": null,
        "step_count": null,
        "stages": [
            { "id": "implementing", "label": "Implementing", "state": implementing },
            { "id": "reviewing", "label": "Reviewing", "state": reviewing },
        ],
    })
}

/// One stage segment: its state relative to the current step position in
/// the job's flattened step order. A stage whose steps straddle the
/// current position (the claimed pipeline validates both before and after
/// its review) has already been visited, so it reads `past`.
fn stage_state(positions: &[usize], current: usize) -> &'static str {
    if positions.contains(&current) {
        "current"
    } else if positions.iter().all(|position| *position < current) {
        "past"
    } else if positions.iter().all(|position| *position > current) {
        "future"
    } else {
        "past"
    }
}

impl OrbitRuntime {
    /// `job_run_stage` for one task row: the current stage, step and run
    /// state, and the ordered stage list with past/current/future states.
    ///
    /// `None` when the task is not `in-progress` or `review`, has no run,
    /// or its run is on another host with no local claim record — the
    /// panel then shows no stage strip rather than a guessed one. A job
    /// with no `stage:` declarations reports the raw step id with an
    /// empty stage list.
    pub fn job_run_stage(&self, row: &TaskRow) -> Result<Option<Value>, OrbitError> {
        let task = &row.task;
        if !eligible(task.status) {
            return Ok(None);
        }
        let Some(run_id) = task.job_run_id.as_deref() else {
            return Ok(None);
        };
        let navigable = job_run_is_locally_navigable(self.automation_machine_identity(), task);
        let run = self.stores().jobs().get_job_run(run_id)?;
        let Some(run) = run.filter(|_| navigable) else {
            // The run lives on another host (or is unreadable here): the
            // claim's leaf stage is the only durable local fact, and only
            // where this host holds the pull admission.
            return Ok(self
                .claimed_leaf_stage_for_task(&task.id)?
                .map(|(stage, _, leaf_state)| claimed_stage_value(stage, &leaf_state)));
        };
        Ok(self.local_run_stage(&run))
    }

    fn local_run_stage(&self, run: &JobRun) -> Option<Value> {
        let display = run_display_steps(run, || {
            self.collect_run_audit_steps(&run.run_id)
                .unwrap_or_default()
        });
        let records = &display.records;
        if records.is_empty() {
            return None;
        }
        let current = records
            .iter()
            .find(|step| step.state == "running")
            .or_else(|| records.iter().rev().find(|step| step.state != "skipped"))
            .or_else(|| records.last())?;
        let executed: BTreeSet<&str> = records
            .iter()
            .filter(|step| step.state != "skipped")
            .map(|step| step.target_id.as_str())
            .collect();

        // Prefer the definition the run recorded; catalog-backed runs
        // resolve to the currently loaded job of the same name. A
        // definition that no longer loads degrades to the raw step.
        let definition: Option<JobV2> = self.resolve_run_definition(run).ok().map(|(_, spec)| spec);
        let step_stages = definition
            .as_ref()
            .map(|job| job.step_stages())
            .unwrap_or_default();
        let current_position = step_stages
            .iter()
            .position(|(id, _)| *id == current.target_id);
        let (step_index, step_count) = match current_position {
            Some(position) => (position + 1, step_stages.len()),
            None => (
                records
                    .iter()
                    .position(|step| std::ptr::eq(step, current))
                    .map_or(records.len(), |position| position + 1),
                if step_stages.is_empty() {
                    records.len()
                } else {
                    step_stages.len()
                },
            ),
        };
        let effective: Option<&JobStage> = current_position
            .and_then(|position| step_stages.get(position))
            .and_then(|(_, stage)| *stage);

        let mut stages = Vec::new();
        if let (Some(job), Some(current_position)) = (&definition, current_position) {
            for stage in job.ordered_stages() {
                let positions: Vec<usize> = step_stages
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, effective))| {
                        effective.is_some_and(|effective| effective.id == stage.id)
                    })
                    .map(|(position, _)| position)
                    .collect();
                let state = stage_state(&positions, current_position);
                // A terminal run never reaches the stages after where it
                // stopped; an unreached conditional branch (re-review on
                // a plain merge) is not part of this run's story.
                let reached = positions
                    .iter()
                    .any(|position| executed.contains(step_stages[*position].0));
                if run.state.is_terminal() && state == "future" && !reached {
                    continue;
                }
                stages.push(json!({
                    "id": stage.id,
                    "label": stage.display_label(),
                    "state": state,
                }));
            }
        }

        Some(json!({
            "stage": effective.map(|stage| stage.id.clone()),
            "stage_label": effective.map(|stage| stage.display_label().to_string()),
            "step_id": current.target_id,
            "step_state": current.state,
            "run_state": run.state.to_string(),
            "started_at": current.started_at.map(|at| at.to_rfc3339()),
            "step_index": step_index,
            "step_count": step_count,
            "stages": stages,
        }))
    }
}
