use std::path::PathBuf;

use clap::Args;
use orbit_core::Core;
use orbit_core::app::task::{self, Command, Outcome};
use orbit_core::application::task_bundle::migration::ExportSelection;
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

/// `orbit task export` — pack a workspace's task bundles into a portable tar.zst.
#[derive(Args)]
pub struct TaskExportArgs {
    /// Output archive path (tar.zst).
    #[arg(short = 'o', long = "output", value_name = "ARCHIVE")]
    pub output: PathBuf,
    /// Task-registry workspace id to export from (default: current workspace).
    #[arg(long = "task-workspace", value_name = "TASK_WORKSPACE")]
    pub task_workspace: Option<String>,
    /// Export only these task ids (comma-separated). Defaults to all tasks.
    #[arg(long, value_delimiter = ',', conflicts_with = "all")]
    pub ids: Vec<String>,
    /// Export every task in the workspace (the default when `--ids` is omitted).
    #[arg(long)]
    pub all: bool,
}

impl ExecuteCore for TaskExportArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let selection = if self.ids.is_empty() {
            ExportSelection::All
        } else {
            ExportSelection::Ids(self.ids)
        };
        let outcome = task::exec(
            core,
            Command::Export {
                task_workspace: self.task_workspace,
                selection,
                output: self.output,
            },
        )
        .and_then(Outcome::into_exported)?;

        Ok(Payload::detail(
            json!({
                "archive": outcome.archive_path.display().to_string(),
                "workspace_id": outcome.workspace_id,
                "task_ids": outcome.task_ids,
                "count": outcome.task_ids.len(),
            }),
            format!(
                "exported {} task(s) from workspace '{}' to {}",
                outcome.task_ids.len(),
                outcome.workspace_id,
                outcome.archive_path.display()
            ),
        )
        .into())
    }
}
