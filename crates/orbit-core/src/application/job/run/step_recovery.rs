//! Authentication for executor-owned step recovery.
//!
//! Before a recovery hook asks the host process to mutate Git metadata, the
//! live run, the task lineage it claims, and the worktree checkpoint it
//! inherited are all revalidated against durable state. An agent subprocess
//! cannot confer this authority through its response payload, so every fact
//! is re-read here rather than taken from the request.

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::Value;

/// The tasks a run carries, from its persisted input.
fn run_task_ids(run: &JobRun) -> Vec<String> {
    let Some(input) = run.input.as_ref() else {
        return Vec::new();
    };
    let mut ids = input
        .get("task_ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(id) = input.get("task_id").and_then(Value::as_str) {
        ids.push(id.to_string());
    }
    ids.sort();
    ids.dedup();
    ids
}

impl crate::OrbitRuntime {
    /// Authenticate the active run, tasks, and inherited worktree checkpoint
    /// immediately before executor-owned recovery writes Git metadata.
    pub(crate) fn validate_step_recovery_mutation(
        &self,
        run_id: &str,
        step_id: &str,
        task_ids: &[String],
        workspace_path: &std::path::Path,
    ) -> Result<(), OrbitError> {
        let run = self.get_job_run_backend(run_id)?.ok_or_else(|| {
            OrbitError::Execution(format!(
                "step recovery '{step_id}' has no durable run '{run_id}'"
            ))
        })?;
        if run.state != JobRunState::Running {
            return Err(OrbitError::Execution(format!(
                "step recovery '{step_id}' refuses Git mutation for run '{run_id}' in state '{}'",
                run.state
            )));
        }

        let mut expected_task_ids = run_task_ids(&run);
        let mut observed_task_ids = task_ids.to_vec();
        expected_task_ids.sort();
        expected_task_ids.dedup();
        observed_task_ids.sort();
        observed_task_ids.dedup();
        if expected_task_ids != observed_task_ids {
            return Err(OrbitError::Execution(format!(
                "step recovery '{step_id}' task lineage does not match run '{run_id}'"
            )));
        }
        let mut task_owner = None;
        for task_id in &observed_task_ids {
            let task = self.get_task(task_id)?;
            let owner = task.job_run_id.as_deref().ok_or_else(|| {
                OrbitError::Execution(format!(
                    "step recovery '{step_id}' task '{task_id}' has no active run owner"
                ))
            })?;
            if !matches!(task.status, TaskStatus::InProgress | TaskStatus::Review) {
                return Err(OrbitError::Execution(format!(
                    "step recovery '{step_id}' task '{task_id}' is no longer owned by active run '{run_id}'"
                )));
            }
            match task_owner.as_deref() {
                None => task_owner = Some(owner.to_string()),
                Some(current) if current == owner => {}
                Some(_) => {
                    return Err(OrbitError::Execution(format!(
                        "step recovery '{step_id}' tasks do not share one worktree owner"
                    )));
                }
            }
        }

        let state = self.read_run_state(run_id)?.ok_or_else(|| {
            OrbitError::Execution(format!(
                "step recovery '{step_id}' run '{run_id}' has no durable pipeline state"
            ))
        })?;
        let worktree = state.pipeline.get("worktree").ok_or_else(|| {
            OrbitError::Execution(format!(
                "step recovery '{step_id}' run '{run_id}' has no worktree checkpoint"
            ))
        })?;
        let checkpoint_path = worktree
            .get("workspace_path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "step recovery '{step_id}' run '{run_id}' has no checkpointed workspace path"
                ))
            })?;
        let checkpoint_path = std::fs::canonicalize(checkpoint_path).map_err(|error| {
            OrbitError::Execution(format!(
                "step recovery '{step_id}' cannot resolve checkpointed workspace '{checkpoint_path}': {error}"
            ))
        })?;
        let requested_path = workspace_path.canonicalize().map_err(|error| {
            OrbitError::Execution(format!(
                "step recovery '{step_id}' cannot resolve assigned workspace '{}': {error}",
                workspace_path.display()
            ))
        })?;
        if checkpoint_path != requested_path {
            return Err(OrbitError::Execution(format!(
                "step recovery '{step_id}' assigned workspace '{}' does not match run '{run_id}' checkpoint '{}'",
                requested_path.display(),
                checkpoint_path.display()
            )));
        }

        let checkpoint_owner = worktree
            .get("job_run_id")
            .or_else(|| worktree.get("batch_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "step recovery '{step_id}' run '{run_id}' has no worktree checkpoint owner"
                ))
            })?;
        if task_owner.as_deref() != Some(checkpoint_owner) {
            return Err(OrbitError::Execution(format!(
                "step recovery '{step_id}' task owner does not match worktree owner '{checkpoint_owner}'"
            )));
        }
        if !self.run_descends_from(run, checkpoint_owner)? {
            return Err(OrbitError::Execution(format!(
                "step recovery '{step_id}' worktree owner '{checkpoint_owner}' is outside run '{run_id}' retry lineage"
            )));
        }
        Ok(())
    }

    /// Whether `run` is `expected_ancestor` or one of its retries.
    fn run_descends_from(
        &self,
        mut run: JobRun,
        expected_ancestor: &str,
    ) -> Result<bool, OrbitError> {
        let mut visited = std::collections::BTreeSet::new();
        loop {
            if run.run_id == expected_ancestor {
                return Ok(true);
            }
            if !visited.insert(run.run_id.clone()) {
                return Ok(false);
            }
            let Some(parent_id) = run.retry_source_run_id.as_deref() else {
                return Ok(false);
            };
            let Some(parent) = self.get_job_run_backend(parent_id)? else {
                return Ok(false);
            };
            run = parent;
        }
    }
}
