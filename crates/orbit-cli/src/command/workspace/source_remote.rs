use clap::{Args, Subcommand};
use orbit_core::Core;
use orbit_core::app::workspace::{Command, Outcome, exec};
use orbit_types::workspace::{Workspace, git_remote_identity, redact_git_remote};
use serde_json::{Value, json};

use crate::command::{CommandOut, ExecuteCore, Payload};

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

impl ExecuteCore for WorkspaceSourceRemoteCommand {
    fn execute(self, core: &Core) -> CommandOut {
        match self.command {
            WorkspaceSourceRemoteSubcommand::Show(args) => args.execute(core),
            WorkspaceSourceRemoteSubcommand::Rebind(args) => args.execute(core),
        }
    }
}

#[derive(Args)]
pub struct WorkspaceSourceRemoteShowArgs {}

impl ExecuteCore for WorkspaceSourceRemoteShowArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::SourceRemote(workspace) = exec(core, Command::SourceRemoteShow)? else {
            unreachable!("workspace source-remote show returned an unrelated outcome");
        };

        Ok(Payload::detail(
            source_remote_json(&workspace),
            format_source_remote(&workspace),
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

impl ExecuteCore for WorkspaceSourceRemoteRebindArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::SourceRemoteRebound(outcome) = exec(
            core,
            Command::SourceRemoteRebind {
                remote: self.remote,
                dry_run: self.dry_run,
            },
        )?
        else {
            unreachable!("workspace source-remote rebind returned an unrelated outcome");
        };

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
