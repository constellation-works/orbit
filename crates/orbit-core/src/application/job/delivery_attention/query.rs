use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::{run_display_steps, run_error_step};
use crate::application::task::TaskRow;

use super::{history, value};

const DELIVERY_JOB: &str = "task_pr_pipeline";

const CLAIMED_DELIVERY_JOB: &str = "task_claimed_pr_pipeline";

pub(super) fn failed_step_id(runtime: &OrbitRuntime, run: &JobRun) -> Option<String> {
    if let Ok(Some(state)) = runtime.read_run_state(&run.run_id)
        && let Some(checkpoint) = &state.failure_activity_checkpoint
    {
        return Some(checkpoint.failed_step_id.clone());
    }
    // Audit-derived steps carry the YAML step id (e.g. `complete_pr`).
    let display = run_display_steps(run, || {
        runtime
            .collect_run_audit_steps(&run.run_id)
            .unwrap_or_default()
    });
    let from_display = display
        .records
        .iter()
        .rev()
        .find(|step| {
            step.state != "skipped"
                && (step.error_message.is_some()
                    || matches!(step.state.as_str(), "failed" | "timeout" | "interrupted"))
        })
        .map(|step| step.target_id.clone());
    if from_display.is_some() {
        return from_display;
    }
    run_error_step(run).map(|step| step.target_id.clone())
}

fn is_live(run: &JobRun) -> bool {
    matches!(
        run.state,
        JobRunState::Pending | JobRunState::Running | JobRunState::Retrying
    )
}

impl OrbitRuntime {
    /// Attention for one task row, using the same lookup and projection as lists.
    pub fn delivery_attention(&self, row: &TaskRow) -> Result<Option<Value>, OrbitError> {
        Ok(self.delivery_attention_map([row])?.remove(&row.task.id))
    }

    /// Attention for a list page: chunked indexed ownership seeks, with an
    /// independent eight-run recency window per eligible task and batched step
    /// reads. Audit reads are only for flagged rows. No forge calls.
    pub fn delivery_attention_map<'a>(
        &self,
        rows: impl IntoIterator<Item = &'a TaskRow>,
    ) -> Result<BTreeMap<String, Value>, OrbitError> {
        let rows: Vec<&TaskRow> = rows
            .into_iter()
            .filter(|row| history::eligible(&row.task))
            .collect();
        let ids = rows
            .iter()
            .map(|row| row.task.id.clone())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let runs = self
            .stores()
            .jobs()
            .recent_job_runs_for_tasks(DELIVERY_JOB, &ids, 8)?;
        let mut map = BTreeMap::new();
        for row in rows {
            let runs = runs
                .get(&row.task.id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let newest = runs
                .iter()
                .find(|run| history::single_task(run, &row.task.id));
            // Detail has always treated any carried live run, including a
            // multi-task repair, as repairing within this task's window.
            let repairing = runs.iter().any(is_live);
            let failed = newest.filter(|run| {
                matches!(
                    run.state,
                    JobRunState::Failed | JobRunState::Timeout | JobRunState::Interrupted
                )
            });
            let step = failed.and_then(|run| failed_step_id(self, run));
            if let Some(value) = value::delivery_attention_value(
                &row.task,
                &row.history,
                newest,
                step.as_deref(),
                repairing,
            ) {
                map.insert(row.task.id.clone(), value);
            }
        }
        Ok(map)
    }

    /// The newest single-task delivery run for `task_id`.
    ///
    /// Owner (`task_pr_pipeline`) and claimed (`task_claimed_pr_pipeline`)
    /// pipelines both count. Each contributes its newest run that carries
    /// exactly this task, and the later of the two wins. An equal timestamp
    /// keeps the owner pipeline, which is read first. This does not change
    /// which runs [`Self::delivery_attention_map`] treats as delivery attention.
    pub fn newest_single_task_delivery_run(
        &self,
        task_id: &str,
    ) -> Result<Option<JobRun>, OrbitError> {
        let ids = [task_id.to_string()];
        let mut best: Option<JobRun> = None;
        for job in [DELIVERY_JOB, CLAIMED_DELIVERY_JOB] {
            let runs = self
                .stores()
                .jobs()
                .recent_job_runs_for_tasks(job, &ids, 8)?;
            let Some(runs) = runs.get(task_id) else {
                continue;
            };
            let Some(newest) = runs.iter().find(|run| history::single_task(run, task_id)) else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|current| history::run_at(newest) > history::run_at(current))
            {
                best = Some(newest.clone());
            }
        }
        Ok(best)
    }
}
