use clap::{Args, ValueEnum};
use orbit_core::Core;
use orbit_core::app::workspace::{Command, Outcome, RoleAssignment, exec};
use orbit_types::workspace::WorkspaceCheckoutRole;
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Clone, Copy, ValueEnum)]
pub enum CliCheckoutRole {
    Owner,
    Replica,
}

impl From<CliCheckoutRole> for WorkspaceCheckoutRole {
    fn from(role: CliCheckoutRole) -> Self {
        match role {
            CliCheckoutRole::Owner => WorkspaceCheckoutRole::Owner,
            CliCheckoutRole::Replica => WorkspaceCheckoutRole::Replica,
        }
    }
}

#[derive(Args)]
#[command(
    about = "Validate or reassert this checkout's declared role (choose its initial role during workspace init)"
)]
pub struct WorkspaceRoleArgs {
    /// Logical workspace id or name.
    workspace: String,
    /// Local role for this machine's checkout.
    #[arg(value_enum)]
    role: CliCheckoutRole,
    /// Stable owner machine_id (required for replica; rejected for owner).
    #[arg(long)]
    owner: Option<String>,
}

fn role_json(assignment: &RoleAssignment) -> serde_json::Value {
    json!({
        "workspace_id": assignment.workspace_id,
        "role": assignment.role.as_str(),
        "owner_machine_id": assignment.owner_machine_id,
    })
}

fn role_text(assignment: &RoleAssignment) -> String {
    match (assignment.role, assignment.owner_machine_id.as_deref()) {
        (WorkspaceCheckoutRole::Replica, Some(owner)) => format!(
            "workspace '{}' local role set to replica (owner {owner}); run \
             `orbit run auto --pull <selector>` here to drain from it",
            assignment.workspace_id
        ),
        _ => format!(
            "workspace '{}' local role set to {}",
            assignment.workspace_id, assignment.role
        ),
    }
}

impl ExecuteCore for WorkspaceRoleArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::RoleAssigned(assignment) = exec(
            core,
            Command::Role {
                workspace: self.workspace,
                role: self.role.into(),
                owner: self.owner,
            },
        )?
        else {
            unreachable!("workspace role returned an unrelated outcome");
        };
        Ok(Payload::detail(role_json(&assignment), role_text(&assignment)).into())
    }
}
