use clap::Args;
use orbit_core::Core;
use orbit_core::app::task::{self, Command, Outcome};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

/// `orbit task reindex` — rebuild the registry index from on-disk bundles.
#[derive(Args)]
pub struct TaskReindexArgs {
    /// Task-registry workspace id to reindex (default: current workspace).
    #[arg(long = "task-workspace", value_name = "TASK_WORKSPACE")]
    pub task_workspace: Option<String>,
}

impl ExecuteCore for TaskReindexArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let outcome = task::exec(
            core,
            Command::Reindex {
                task_workspace: self.task_workspace,
            },
        )
        .and_then(Outcome::into_reindexed)?;

        let doc = json!({
            "workspace_id": outcome.workspace_id,
            "indexed": outcome.indexed,
            "removed_stale": outcome.removed_stale,
        });
        let text = format!(
            "reindexed workspace '{}': {} bundle(s), {} stale binding(s) dropped",
            outcome.workspace_id, outcome.indexed, outcome.removed_stale
        );
        Ok(Payload::detail(doc, text).into())
    }
}
