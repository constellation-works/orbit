use std::path::Path;

use clap::{Args, ValueEnum};
use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_registry::workspace_registry;
use orbit_registry::{MachineIdentityState, inspect_machine_identity};
use orbit_types::workspace::WorkspaceCheckoutRole;
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

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

/// What one role assignment recorded, for both output views.
#[derive(Debug)]
pub(super) struct RoleAssignment {
    workspace_id: String,
    role: WorkspaceCheckoutRole,
    owner_machine_id: Option<String>,
}

impl RoleAssignment {
    pub(super) fn to_json(&self) -> serde_json::Value {
        json!({
            "workspace_id": self.workspace_id,
            "role": self.role.as_str(),
            "owner_machine_id": self.owner_machine_id,
        })
    }

    pub(super) fn to_text(&self) -> String {
        match (self.role, self.owner_machine_id.as_deref()) {
            (WorkspaceCheckoutRole::Replica, Some(owner)) => format!(
                "workspace '{}' local role set to replica (owner {owner}); run \
                 `orbit run auto --pull <selector>` here to drain from it",
                self.workspace_id
            ),
            _ => format!(
                "workspace '{}' local role set to {}",
                self.workspace_id, self.role
            ),
        }
    }
}

/// Validate and persist one checkout role declaration.
pub(super) fn assign_role_at(
    registry_path: &Path,
    workspace: &str,
    role: WorkspaceCheckoutRole,
    owner: Option<&str>,
    local_machine_id: Option<&str>,
) -> Result<RoleAssignment, OrbitError> {
    workspace_registry::with_registry_lock(registry_path, || {
        let mut registry = workspace_registry::load_registry_from(registry_path)?;

        workspace_registry::assign_checkout_role(
            &mut registry,
            workspace,
            role,
            owner,
            local_machine_id,
        )?;
        let (workspace_id, owner_machine_id) =
            workspace_registry::find_workspace(&registry, workspace)?
                .map(|found| (found.id.clone(), found.owner_machine_id.clone()))
                .unwrap_or_else(|| (workspace.to_string(), None));
        // save_registry_to validates a clone before writing, so a contradictory
        // declaration (owner role on a non-owner machine, replica of self, …)
        // fails here and leaves the previous registry file byte-valid.
        workspace_registry::save_registry_to(&registry, registry_path)?;

        Ok(RoleAssignment {
            workspace_id,
            role,
            owner_machine_id,
        })
    })
}

impl Execute for WorkspaceRoleArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let global_root = runtime.global_root();
        let local_machine_id = match inspect_machine_identity(&global_root)? {
            MachineIdentityState::Present(identity) => Some(identity.id),
            MachineIdentityState::Absent => None,
        };
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let assignment = assign_role_at(
            &registry_path,
            &self.workspace,
            self.role.into(),
            self.owner.as_deref(),
            local_machine_id.as_deref(),
        )?;
        Ok(Payload::detail(assignment.to_json(), assignment.to_text()).into())
    }
}
