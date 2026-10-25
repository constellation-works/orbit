use std::path::Path;

use clap::{Args, Subcommand};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::{load_machine_identity, workspace_registry};
use orbit_types::workspace::{
    Workspace, WorkspaceRegistry, git_remote_identity, redact_git_remote,
};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
#[command(
    about = "Inspect or explicitly rebind a workspace source Git remote",
    after_long_help = "Repository-move workflow:\n  1. Run `show --json` and save the old remote. Record any `workspace publication show --json` settings. Existing publication bindings must be removed explicitly before moving.\n  2. Run `rebind --remote <URL> --dry-run --json`.\n  3. Apply without `--dry-run`, update the checkout's Git origin separately, then run `show` again to verify. Rebind also reconciles the task-registry source pairing; repeat it to repair a transfer made with an older Orbit version. Equivalent SSH and HTTPS URLs identify the same repository.\n  4. Use `workspace publication rebind` with a new publication ID and an empty publication branch, then verify with `workspace publication show --json`. Orbit never rewrites old snapshots.\n  5. To roll back, remove any new publication binding and rebind to the saved old source remote."
)]
pub struct WorkspaceSourceRemoteCommand {
    #[command(subcommand)]
    pub command: WorkspaceSourceRemoteSubcommand,
}

#[derive(Subcommand)]
pub enum WorkspaceSourceRemoteSubcommand {
    /// Show the registered portable source-repository identity
    Show(WorkspaceSourceRemoteShowArgs),
    /// Replace the source remote after a repository transfer
    Rebind(WorkspaceSourceRemoteRebindArgs),
}

impl Execute for WorkspaceSourceRemoteCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self.command {
            WorkspaceSourceRemoteSubcommand::Show(args) => args.execute(runtime),
            WorkspaceSourceRemoteSubcommand::Rebind(args) => args.execute(runtime),
        }
    }
}

#[derive(Args)]
pub struct WorkspaceSourceRemoteShowArgs {}

impl Execute for WorkspaceSourceRemoteShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let workspace_id = selected_workspace_id(runtime)?;
        let registry = workspace_registry::load_registry_from(
            &workspace_registry::registry_path_for(&runtime.global_root()),
        )?;
        let workspace = workspace_registry::find_workspace_by_id(&registry, &workspace_id)
            .ok_or_else(|| {
                OrbitError::WorkspaceError(format!("unknown workspace '{workspace_id}'"))
            })?;

        Ok(Payload::detail(
            source_remote_json(workspace),
            format_source_remote(workspace),
        )
        .into())
    }
}

#[derive(Args)]
pub struct WorkspaceSourceRemoteRebindArgs {
    /// New portable Git URL. Credentials, local paths, and remote aliases are refused.
    #[arg(long, value_name = "URL")]
    remote: String,
    /// Validate and report the transition without changing the registry.
    #[arg(long)]
    dry_run: bool,
}

impl Execute for WorkspaceSourceRemoteRebindArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let workspace_id = selected_workspace_id(runtime)?;
        let global_root = runtime.global_root();
        let local_machine_id = load_machine_identity(&global_root)?.id;
        let task_workspace_id = runtime
            .workspace_runtime_binding()
            .map(|binding| binding.task_partition_id.clone())
            .map_or_else(|| runtime.workspace_id(), Ok)?;
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let outcome = rebind_at_registry_path(
            &registry_path,
            &workspace_id,
            &self.remote,
            &local_machine_id,
            self.dry_run,
            workspace_registry::save_registry_to,
            |source_remote| {
                runtime.rebind_task_publication_source(&task_workspace_id, source_remote)
            },
        )?;

        let action = if !outcome.changed {
            "unchanged"
        } else if outcome.dry_run {
            "would_rebind"
        } else {
            "rebound"
        };
        Ok(Payload::detail(
            json!({
                "action": action,
                "workspace_id": outcome.workspace_id,
                "changed": outcome.changed,
                "dry_run": outcome.dry_run,
                "old": {
                    "remote": redact_git_remote(&outcome.old_remote),
                    "repository_identity": outcome.old_repository_identity,
                },
                "new": {
                    "remote": redact_git_remote(&outcome.new_remote),
                    "repository_identity": outcome.new_repository_identity,
                },
            }),
            format!(
                "source remote {action} for workspace '{}'\nold:      {}\nold_id:   {}\nnew:      {}\nnew_id:   {}\nchanged:  {}\ndry_run:  {}",
                outcome.workspace_id,
                redact_git_remote(&outcome.old_remote),
                outcome.old_repository_identity.as_deref().unwrap_or("-"),
                redact_git_remote(&outcome.new_remote),
                outcome.new_repository_identity,
                outcome.changed,
                outcome.dry_run,
            ),
        )
        .into())
    }
}

fn rebind_at_registry_path(
    registry_path: &Path,
    workspace_id: &str,
    remote: &str,
    local_machine_id: &str,
    dry_run: bool,
    save: impl FnOnce(&WorkspaceRegistry, &Path) -> Result<(), OrbitError>,
    pair: impl FnOnce(&str) -> Result<(), OrbitError>,
) -> Result<workspace_registry::WorkspaceSourceRemoteRebind, OrbitError> {
    workspace_registry::with_registry_lock(registry_path, || {
        let mut registry = workspace_registry::load_registry_from(registry_path)?;
        let outcome = workspace_registry::rebind_workspace_source_remote(
            &mut registry,
            workspace_id,
            remote,
            Some(local_machine_id),
            dry_run,
        )?;
        if outcome.changed && !outcome.dry_run {
            save(&registry, registry_path)?;
        }
        // Save the authoritative registration first. An interrupted/failed
        // pairing write is recovered by repeating the same audited command,
        // even when the source registration itself is already unchanged.
        if !outcome.dry_run {
            pair(&outcome.new_remote).map_err(|error| {
                OrbitError::WorkspaceError(format!(
                    "source remote is registered but its task-registry pairing could not be reconciled: {error}; repeat `orbit workspace source-remote rebind --remote <URL>` with the registered source URL"
                ))
            })?;
        }
        Ok(outcome)
    })
}

fn selected_workspace_id(runtime: &OrbitRuntime) -> Result<String, OrbitError> {
    runtime
        .workspace_runtime_binding()
        .map(|binding| binding.logical_workspace_id.clone())
        .map_or_else(|| runtime.workspace_id(), Ok)
}

fn source_remote_json(workspace: &Workspace) -> Value {
    let identity = workspace
        .git_remote
        .as_deref()
        .and_then(|remote| git_remote_identity(remote).ok());
    json!({
        "workspace_id": workspace.id,
        "remote": workspace.git_remote.as_deref().map(redact_git_remote),
        "repository_identity": identity,
    })
}

fn format_source_remote(workspace: &Workspace) -> String {
    let Some(remote) = workspace.git_remote.as_deref() else {
        return format!(
            "workspace '{}' has no registered source remote; set a portable Git origin on the declared owner's checkout, then run `orbit workspace init --name {} --force` there to bind its first source identity",
            workspace.id, workspace.name
        );
    };
    let identity = git_remote_identity(remote).ok();
    format!(
        "workspace: {}\nremote:    {}\nidentity:  {}",
        workspace.id,
        redact_git_remote(remote),
        identity.as_deref().unwrap_or("-"),
    )
}
