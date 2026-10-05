use std::path::PathBuf;

use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::bootstrap::task_migration::ExportSelection;
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

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
    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

impl Execute for TaskExportArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let selection = if self.ids.is_empty() {
            ExportSelection::All
        } else {
            ExportSelection::Ids(self.ids)
        };
        let outcome =
            runtime.export_tasks(self.task_workspace.as_deref(), selection, &self.output)?;

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
