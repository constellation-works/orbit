use clap::Args;
use orbit_cmd::workspace_removal;
use orbit_core::Core;
use orbit_core::app::workspace::{Command, exec};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
pub struct WorkspaceRemoveArgs {
    /// Workspace name, id, or absolute checkout path
    #[arg(value_name = "WORKSPACE", id = "workspace_selector")]
    pub workspace: String,
}

impl ExecuteCore for WorkspaceRemoveArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let mut removed = None;
        exec(
            core,
            Command::Remove(&mut |global_root| {
                removed = Some(workspace_removal::remove_workspace(
                    global_root,
                    &self.workspace,
                )?);
                Ok(())
            }),
        )?;
        let Some(result) = removed else {
            unreachable!("workspace remove ran no removal");
        };
        let mut text = format!("workspace '{}' removed from registry", result.workspace);
        let leftover = result.left_task_store;
        if let Some(partition) = leftover.as_ref() {
            text.push_str(&format!(
                "\nleft task-store partition {} ({} task bundle(s)); \
                 the task-registry workspace binding is retained as checkout evidence \
                 so `orbit doctor` can classify it. If the checkout is absent \
                 beneath a populated, readable immediate parent with no checkout \
                 entry, `orbit doctor --fix-orphan-task-stores --confirm` can \
                 reclaim the partition and delete its task bundles. Restore \
                 offline volumes first. If the parent is empty, missing or \
                 unreadable, or absence cannot otherwise be confirmed, doctor \
                 preserves the partition and binding as unreachable; restore \
                 access and run doctor again.",
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
