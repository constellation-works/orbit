//! `orbit run task-pilot` CLI entrypoint.

use clap::Args;
use orbit_core::Core;
use orbit_core::app::run::{Command, PilotRequest, exec};

use crate::command::{CommandOut, ExecuteCore};

use super::support::{WaitArgs, workflow_dispatch_payload};

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

impl ExecuteCore for TaskPilotCommand {
    fn execute(self, core: &Core) -> CommandOut {
        let timeout = self.wait.timeout_seconds()?;
        let request = PilotRequest {
            task_ids: self.task_ids,
            base_branch: self.base_branch,
            max_tasks: self.max_tasks,
            max_partition_size: self.max_partition_size,
        };
        let runs = exec(core, Command::task_pilot(request, timeout))?.value;
        workflow_dispatch_payload("task-pilot", &runs)
    }
}
