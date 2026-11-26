use clap::{Args, Subcommand};
use orbit_core::Core;
use orbit_core::app::workspace::{Command, Outcome, exec};
use orbit_types::workspace::{
    DEFAULT_PUBLICATION_BRANCH, WorkspacePublicationBinding, redact_git_remote,
};
use serde_json::{Value, json};

use crate::command::{CommandOut, ExecuteCore, Payload, require_confirmation};

#[derive(Args)]
#[command(about = "Manage the owner-local task-publication repository binding")]
pub struct WorkspacePublicationCommand {
    #[command(subcommand)]
    pub command: WorkspacePublicationSubcommand,
}

#[derive(Subcommand)]
pub enum WorkspacePublicationSubcommand {
    /// Bind the selected owned workspace to a dedicated publication repository
    Bind(WorkspacePublicationBindArgs),
    /// Show the selected workspace's local publication binding
    Show(WorkspacePublicationShowArgs),
    /// Explicitly replace the selected workspace's publication lineage
    Rebind(WorkspacePublicationBindArgs),
    /// Remove the local binding without deleting or changing the repository
    Remove(WorkspacePublicationRemoveArgs),
}

impl ExecuteCore for WorkspacePublicationCommand {
    fn execute(self, core: &Core) -> CommandOut {
        match self.command {
            WorkspacePublicationSubcommand::Bind(args) => args.execute_bind(core, false),
            WorkspacePublicationSubcommand::Show(args) => args.execute(core),
            WorkspacePublicationSubcommand::Rebind(args) => args.execute_bind(core, true),
            WorkspacePublicationSubcommand::Remove(args) => args.execute(core),
        }
    }
}

#[derive(Args)]
pub struct WorkspacePublicationBindArgs {
    /// Dedicated publication repository URL. Credentials and source-repository reuse are refused.
    #[arg(long, value_name = "URL")]
    remote: String,
    /// Stable opaque lineage identifier unique on this machine.
    #[arg(long, value_name = "ID")]
    publication_id: String,
    /// Ordinary publication branch (short name or refs/heads/*).
    #[arg(long, default_value = DEFAULT_PUBLICATION_BRANCH)]
    branch: String,
}

impl WorkspacePublicationBindArgs {
    fn execute_bind(self, core: &Core, rebind: bool) -> CommandOut {
        let Outcome::PublicationBound { binding, .. } = exec(
            core,
            Command::PublicationBind {
                remote: self.remote,
                publication_id: self.publication_id,
                branch: self.branch,
                rebind,
            },
        )?
        else {
            unreachable!("workspace publication bind returned an unrelated outcome");
        };

        let action = if rebind { "rebound" } else { "bound" };
        Ok(Payload::detail(
            binding_json(&binding, action),
            format!(
                "{action} workspace '{}' to publication '{}'\nremote:  {}\nbranch:  {}\nprivacy: operator-managed",
                binding.workspace_id,
                binding.publication_id,
                redact_git_remote(&binding.publication_remote),
                binding.publication_branch,
            ),
        )
        .into())
    }
}

#[derive(Args)]
pub struct WorkspacePublicationShowArgs {}

impl ExecuteCore for WorkspacePublicationShowArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Publication {
            workspace_id,
            binding,
        } = exec(core, Command::PublicationShow)?
        else {
            unreachable!("workspace publication show returned an unrelated outcome");
        };
        match binding {
            Some(binding) => Ok(Payload::detail(
                binding_json(&binding, "shown"),
                format_binding(&binding),
            )
            .into()),
            None => Ok(Payload::detail(
                json!({
                    "workspace_id": workspace_id,
                    "bound": false,
                    "privacy": "operator-managed",
                }),
                format!("workspace '{workspace_id}' has no publication binding"),
            )
            .into()),
        }
    }
}

#[derive(Args)]
pub struct WorkspacePublicationRemoveArgs {
    /// Confirm removal of the local lineage and last-success record.
    #[arg(long)]
    pub confirm: bool,
}

impl ExecuteCore for WorkspacePublicationRemoveArgs {
    fn execute(self, core: &Core) -> CommandOut {
        require_confirmation(
            self.confirm,
            "removing a task-publication binding and its local last-success record",
        )?;
        let Outcome::PublicationRemoved(removed) = exec(core, Command::PublicationRemove)? else {
            unreachable!("workspace publication remove returned an unrelated outcome");
        };
        Ok(Payload::detail(
            json!({
                "workspace_id": removed.workspace_id,
                "publication_id": removed.publication_id,
                "removed": true,
                "repository_changed": false,
            }),
            format!(
                "removed publication binding '{}' from workspace '{}'; the repository was not changed",
                removed.publication_id, removed.workspace_id
            ),
        )
        .into())
    }
}

fn binding_json(binding: &WorkspacePublicationBinding, action: &str) -> Value {
    json!({
        "action": action,
        "bound": true,
        "workspace_id": binding.workspace_id,
        "source_repository_fingerprint": binding.source_repository_fingerprint,
        "publication_remote": redact_git_remote(&binding.publication_remote),
        "publication_branch": binding.publication_branch,
        "publication_id": binding.publication_id,
        "authority_machine_id": binding.authority_machine_id,
        "last_success_generation": binding.last_success_generation,
        "last_success_commit": binding.last_success_commit,
        "privacy": "operator-managed",
    })
}

fn format_binding(binding: &WorkspacePublicationBinding) -> String {
    format!(
        "workspace:       {}\npublication:     {}\nremote:          {}\nbranch:          {}\nsource:          {}\nauthority:       {}\nlast_generation: {}\nlast_commit:     {}\nprivacy:         operator-managed",
        binding.workspace_id,
        binding.publication_id,
        redact_git_remote(&binding.publication_remote),
        binding.publication_branch,
        redact_git_remote(&binding.source_repository_fingerprint),
        binding.authority_machine_id,
        binding
            .last_success_generation
            .map_or_else(|| "-".to_string(), |generation| generation.to_string()),
        binding.last_success_commit.as_deref().unwrap_or("-"),
    )
}
