use clap::{Args, ValueEnum};
use orbit_core::OrbitRuntime;
use orbit_registry::workspace_registry;
use orbit_registry::{MachineIdentityState, inspect_machine_identity};
use orbit_types::workspace::WorkspaceCheckoutRole;

use crate::command::{CommandOut, CommandOutput, Execute};

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

impl Execute for WorkspaceRoleArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let global_root = runtime.global_root();
        let local_machine_id = match inspect_machine_identity(&global_root)? {
            MachineIdentityState::Present(identity) => Some(identity.id),
            MachineIdentityState::Absent => None,
        };
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;

        let role: WorkspaceCheckoutRole = self.role.into();
        workspace_registry::assign_checkout_role(
            &mut registry,
            &self.workspace,
            role,
            self.owner.as_deref(),
            local_machine_id.as_deref(),
        )?;
        // save_registry_to validates a clone before writing, so a contradictory
        // declaration (owner role on a non-owner machine, replica of self, …)
        // fails here and leaves the previous registry file byte-valid.
        workspace_registry::save_registry_to(&registry, &registry_path)?;

        println!("workspace '{}' local role set to {}", self.workspace, role);
        Ok(CommandOutput::Silent)
    }
}
