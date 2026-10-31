//! `orbit run task-pilot` CLI entrypoint.

use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime, TaskStatus, find_workflow};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute};

use super::support::{WaitArgs, dispatch_workflow, workflow_dispatch_payload};

pub(super) const TASK_PILOT_WORKFLOW: &str = "task-pilot";
/// The shipped task-pilot job's `max_tasks`, which prepare also applies when
/// the job's default input declares none.
const SHIPPED_MAX_TASKS: u64 = 50;

#[derive(Args)]
#[command(
    name = "task-pilot",
    about = "Preflight proposed/backlog tasks and persist validated selectors",
    override_usage = "orbit run task-pilot [<TASK_ID>...] [OPTIONS]",
    after_help = "Examples:\n  orbit run task-pilot\n  orbit run task-pilot <TASK_ID> <TASK_ID>\n  orbit run task-pilot --wait --json\n\nPilots never promote or dispatch tasks. An omitted task list runs automatic\ndiscovery; explicit ids audit those tasks. Explicit tasks with different stored\ncrews are submitted in separate runs. Inspect submitted runs with\n`orbit run history -j task_pilot_pipeline` and `orbit run show <RUN_ID>`."
)]
pub struct TaskPilotCommand {
    /// Optional task IDs to audit. Omit for automatic discovery of
    /// proposed/backlog tasks with empty context_files or unassessed complexity.
    #[arg(value_name = "TASK_ID", num_args = 0..)]
    pub task_ids: Vec<String>,
    /// Base branch to inspect. Omit to use the registered workspace
    /// base branch, else `[workflow] base_branch`.
    #[arg(long = "base-branch", value_name = "BRANCH")]
    pub base_branch: Option<String>,
    /// Maximum tasks to select. Omit to use the job default (50).
    #[arg(long = "max-tasks", value_name = "N")]
    pub max_tasks: Option<u32>,
    /// Maximum tasks per pilot partition. Omit to use the job default (5).
    #[arg(long = "max-partition-size", value_name = "N")]
    pub max_partition_size: Option<u32>,
    #[command(flatten)]
    pub(super) wait: WaitArgs,
}

impl TaskPilotCommand {
    /// The total cap on an explicit selection: `--max-tasks` when given, else
    /// the `max_tasks` default of the job the run executes, so a custom
    /// definition's default applies here as it does at preparation.
    fn effective_max_tasks(&self, runtime: &OrbitRuntime) -> Result<u64, OrbitError> {
        if let Some(max_tasks) = self.max_tasks {
            return Ok(u64::from(max_tasks));
        }
        let job_id = find_workflow(TASK_PILOT_WORKFLOW)
            .map(|workflow| workflow.job_id)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("unknown workflow '{TASK_PILOT_WORKFLOW}'"))
            })?;
        let default_input = runtime.job_default_input(job_id)?;
        Ok(default_input
            .as_ref()
            .and_then(|input| input.get("max_tasks"))
            .and_then(Value::as_u64)
            .unwrap_or(SHIPPED_MAX_TASKS))
    }
}

impl Execute for TaskPilotCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let timeout_seconds = self.wait.timeout_seconds()?;
        let input = build_task_pilot_input(
            &self.task_ids,
            self.base_branch.as_deref(),
            self.max_tasks,
            self.max_partition_size,
        )?;
        // Refuse the whole explicit selection before submitting a run. Prepare
        // repeats this check against current state for every other caller and
        // for status changes after this read. Mixed stored crews are submitted
        // as separate, homogeneous runs so no unanimous run fails at startup.
        let mut unsupported = Vec::new();
        let mut by_crew: Vec<(Option<String>, Vec<String>)> = Vec::new();
        for task_id in &self.task_ids {
            let task = runtime.get_task(task_id)?;
            if !matches!(task.status, TaskStatus::Proposed | TaskStatus::Backlog) {
                unsupported.push(format!("{} (status {})", task.id, task.status));
            }
            let crew = task
                .crew
                .as_deref()
                .map(str::trim)
                .filter(|crew| !crew.is_empty())
                .map(ToOwned::to_owned);
            if let Some((_, ids)) = by_crew.iter_mut().find(|(existing, _)| existing == &crew) {
                ids.push(task.id);
            } else {
                by_crew.push((crew, vec![task.id]));
            }
        }
        if !unsupported.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "explicit task-pilot selection refuses {}; only proposed and backlog tasks may be assessed",
                unsupported.join(", ")
            )));
        }
        if !self.task_ids.is_empty() {
            let max_tasks = self.effective_max_tasks(runtime)?;
            if self.task_ids.len() as u64 > max_tasks {
                return Err(OrbitError::InvalidInput(format!(
                    "explicit task-pilot selection contains {} tasks, exceeding max_tasks {max_tasks}",
                    self.task_ids.len()
                )));
            }
        }
        let selections = if self.task_ids.is_empty() {
            vec![Vec::new()]
        } else {
            by_crew.into_iter().map(|(_, ids)| ids).collect()
        };
        let mut runs = Vec::new();
        for task_ids in selections {
            let run_input = if task_ids.is_empty() {
                input.clone()
            } else {
                build_task_pilot_input(
                    &task_ids,
                    self.base_branch.as_deref(),
                    self.max_tasks,
                    self.max_partition_size,
                )?
            };
            runs.extend(dispatch_workflow(
                runtime,
                TASK_PILOT_WORKFLOW,
                &run_input,
                false,
                timeout_seconds,
                1,
            )?);
        }
        workflow_dispatch_payload(TASK_PILOT_WORKFLOW, &runs)
    }
}

fn build_task_pilot_input(
    task_ids: &[String],
    base_branch: Option<&str>,
    max_tasks: Option<u32>,
    max_partition_size: Option<u32>,
) -> Result<Value, OrbitError> {
    let mut seen = std::collections::HashSet::new();
    for task_id in task_ids {
        if task_id.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task id in explicit task-pilot selection must not be empty".to_string(),
            ));
        }
        if !seen.insert(task_id.as_str()) {
            return Err(OrbitError::InvalidInput(format!(
                "duplicate task id '{task_id}' in explicit task-pilot selection"
            )));
        }
    }

    if task_ids.len() > 500 {
        return Err(OrbitError::InvalidInput(
            "explicit task-pilot selection exceeds the limit of 500 tasks".to_string(),
        ));
    }

    let mut map = serde_json::Map::new();
    if !task_ids.is_empty() {
        map.insert(
            "task_ids".to_string(),
            Value::Array(task_ids.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(branch) = base_branch {
        if branch.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task-pilot base branch must not be empty".to_string(),
            ));
        }
        map.insert("base_branch".to_string(), Value::String(branch.to_string()));
    }
    if let Some(max_tasks) = max_tasks {
        map.insert("max_tasks".to_string(), json!(max_tasks));
    }
    if let Some(max_partition_size) = max_partition_size {
        map.insert("max_partition_size".to_string(), json!(max_partition_size));
    }
    Ok(Value::Object(map))
}
