use clap::Args;
use orbit_cmd::workspace_removal::{self, WorkspaceTeardownOptions};
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct WorkspaceTeardownArgs {
    /// Registered workspace name, logical id (`ws_*`), or absolute checkout path
    #[arg(value_name = "WORKSPACE", id = "workspace_selector")]
    pub workspace: String,

    /// Required flag to confirm destructive operation
    #[arg(long)]
    pub confirm: bool,
}

impl Execute for WorkspaceTeardownArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let cwd = std::env::current_dir().map_err(|e| OrbitError::Io(e.to_string()))?;
        let result = workspace_removal::teardown_workspace(
            &runtime.global_root(),
            &self.workspace,
            &cwd,
            WorkspaceTeardownOptions {
                confirm: self.confirm,
            },
        )?;
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
