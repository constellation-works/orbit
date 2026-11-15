use clap::Args;
use orbit_cmd::workspace_removal;
use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct WorkspaceRemoveArgs {
    /// Workspace name, id, or absolute checkout path
    #[arg(value_name = "WORKSPACE", id = "workspace_selector")]
    pub workspace: String,
}

impl Execute for WorkspaceRemoveArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let result = workspace_removal::remove_workspace(&runtime.global_root(), &self.workspace)?;
        let mut text = format!("workspace '{}' removed from registry", result.workspace);
        let leftover = result.left_task_store;
        if let Some(partition) = leftover.as_ref() {
            text.push_str(&format!(
                "\nleft task-store partition {} ({} task bundle(s)); \
                 the task-registry workspace binding is retained as checkout evidence \
                 so `orbit doctor` can classify it. Reclaim with \
                 `orbit doctor --fix-orphan-task-stores --confirm`.",
                partition.path.display(),
                partition.task_bundles
            ));
        }
        Ok(Payload::detail(
            json!({
                "workspace": result.workspace,
                "id": result.id,
                "removed": true,
                "left_task_store": leftover.as_ref().map(|partition| json!({
                    "path": partition.path.display().to_string(),
                    "task_bundles": partition.task_bundles,
                })),
            }),
            text,
        )
        .into())
    }
}
