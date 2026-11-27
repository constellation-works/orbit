use clap::Args;
use orbit_cmd::workspace_removal::{self, WorkspaceTeardownOptions};
use orbit_common::OrbitError;
use orbit_core::Core;
use orbit_core::app::workspace::{Command, exec};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
pub struct WorkspaceTeardownArgs {
    /// Registered workspace name, logical id (`ws_*`), or absolute checkout path
    #[arg(value_name = "WORKSPACE", id = "workspace_selector")]
    pub workspace: String,

    /// Required flag to confirm destructive operation
    #[arg(long)]
    pub confirm: bool,
}

impl ExecuteCore for WorkspaceTeardownArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let mut torn_down = None;
        exec(
            core,
            Command::Teardown {
                confirmed: self.confirm,
                teardown: &mut |global_root| {
                    let cwd = std::env::current_dir().map_err(|e| OrbitError::Io(e.to_string()))?;
                    torn_down = Some(workspace_removal::teardown_workspace(
                        global_root,
                        &self.workspace,
                        &cwd,
                        WorkspaceTeardownOptions {
                            confirm: self.confirm,
                        },
                    )?);
                    Ok(())
                },
            },
        )?;
        let Some(result) = torn_down else {
            unreachable!("workspace teardown ran no teardown");
        };
        let mut text = format!("teardown plan:\n{}\nteardown complete:", result.plan);
        for item in &result.removed {
            text.push_str(&format!("\n  - {item}"));
        }
        if result.removed.is_empty() {
            text.push_str("\n  (nothing to remove)");
        }
        Ok(Payload::detail(
            json!({
                "workspace": result.workspace,
                "id": result.id,
                "checkout": result.checkout.display().to_string(),
                "removed": result.removed,
            }),
            text,
        )
        .into())
    }
}
