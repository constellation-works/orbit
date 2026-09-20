//! `orbit run task-pilot` CLI entrypoint.

use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute};

use super::support::{dispatch_workflow, workflow_dispatch_payload};

pub(super) const TASK_PILOT_WORKFLOW: &str = "task-pilot";

#[derive(Args)]
#[command(
    name = "task-pilot",
    about = "Preflight proposed/backlog tasks and persist validated selectors",
    override_usage = "orbit run task-pilot [<TASK_ID>...] [OPTIONS]",
    after_help = "Examples:\n  orbit run task-pilot\n  orbit run task-pilot <TASK_ID> <TASK_ID>\n  orbit run task-pilot --wait --json\n\nPilots never promote or dispatch tasks. An omitted task list runs automatic\ndiscovery; explicit ids audit those tasks. Inspect submitted runs with\n`orbit run history -j task_pilot_pipeline` and `orbit run show <RUN_ID>`."
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
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
    /// Block until the submitted run reaches a terminal state.
    #[arg(long)]
    pub wait: bool,
}

impl Execute for TaskPilotCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let input = build_task_pilot_input(
            &self.task_ids,
            self.base_branch.as_deref(),
            self.max_tasks,
            self.max_partition_size,
        )?;
        let runs = dispatch_workflow(runtime, TASK_PILOT_WORKFLOW, &input, false, self.wait, 1)?;
        workflow_dispatch_payload(TASK_PILOT_WORKFLOW, &runs)
    }
}

pub(crate) fn build_task_pilot_input(
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
