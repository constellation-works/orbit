use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;
use orbit_registry::{load_host_identity, workspace_registry};
use orbit_types::workspace::{
    DEFAULT_PUBLICATION_BRANCH, WorkspacePublicationBinding, redact_git_remote,
};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload, require_confirmation};

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

impl Execute for WorkspacePublicationCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self.command {
            WorkspacePublicationSubcommand::Bind(args) => args.execute_bind(runtime, false),
            WorkspacePublicationSubcommand::Show(args) => args.execute(runtime),
            WorkspacePublicationSubcommand::Rebind(args) => args.execute_bind(runtime, true),
            WorkspacePublicationSubcommand::Remove(args) => args.execute(runtime),
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
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

impl WorkspacePublicationBindArgs {
    fn execute_bind(self, runtime: &OrbitRuntime, rebind: bool) -> CommandOut {
        let workspace_id = selected_workspace_id(runtime)?;
        let task_workspace_id = selected_task_workspace_id(runtime)?;
        let global_root = runtime.global_root();
        let machine_id = load_host_identity(&global_root)?.machine_id;
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;
        let binding = if rebind {
            workspace_registry::rebind_publication_by_id(
                &mut registry,
                &workspace_id,
                &self.remote,
                &self.branch,
                &self.publication_id,
                Some(&machine_id),
            )?
        } else {
            workspace_registry::bind_publication_by_id(
                &mut registry,
                &workspace_id,
                &self.remote,
                &self.branch,
                &self.publication_id,
                Some(&machine_id),
            )?
        };
        runtime.record_task_publication_source(
            &task_workspace_id,
            &binding.source_repository_fingerprint,
        )?;
        workspace_registry::save_registry_to(&registry, &registry_path)?;

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
pub struct WorkspacePublicationShowArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

impl Execute for WorkspacePublicationShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let workspace_id = selected_workspace_id(runtime)?;
        let registry = workspace_registry::load_registry_from(
            &workspace_registry::registry_path_for(&runtime.global_root()),
        )?;
        match workspace_registry::find_publication_binding_by_id(&registry, &workspace_id) {
            Some(binding) => {
                Ok(Payload::detail(binding_json(binding, "shown"), format_binding(binding)).into())
            }
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
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

impl Execute for WorkspacePublicationRemoveArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        require_confirmation(
            self.confirm,
            "removing a task-publication binding and its local last-success record",
        )?;
        let workspace_id = selected_workspace_id(runtime)?;
        let global_root = runtime.global_root();
        let machine_id = load_host_identity(&global_root)?.machine_id;
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;
        let removed = workspace_registry::unbind_publication_by_id(
            &mut registry,
            &workspace_id,
            Some(&machine_id),
        )?;
        workspace_registry::save_registry_to(&registry, &registry_path)?;
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

fn selected_workspace_id(runtime: &OrbitRuntime) -> Result<String, orbit_core::OrbitError> {
    runtime
        .workspace_runtime_binding()
        .map(|binding| binding.logical_workspace_id.clone())
        .map_or_else(|| runtime.workspace_id(), Ok)
}

fn selected_task_workspace_id(runtime: &OrbitRuntime) -> Result<String, orbit_core::OrbitError> {
    runtime
        .workspace_runtime_binding()
        .map(|binding| binding.task_partition_id.clone())
        .map_or_else(|| runtime.workspace_id(), Ok)
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
