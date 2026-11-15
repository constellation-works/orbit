use std::path::{Path, PathBuf};

use clap::Args;
use orbit_cmd::workspace_rebind::{self, CheckoutRebindOptions};
use orbit_core::OrbitError;
use orbit_types::workspace::WorkspaceCheckout;
use serde_json::json;

use crate::command::{CommandOut, Payload};

#[derive(Args)]
#[command(
    about = "Rebind this host to a checkout directory the operator already moved",
    after_long_help = "Stop workspace runs and release claims before moving the directory. Remove retained Orbit-managed worktrees first. This command never moves files, changes the source remote, or changes another host. It requires the registered Git origin and .orbit identity. Repeat the same command to finish an interrupted rebind.\n\nExamples:\n  orbit --workspace <name> workspace rebind --path /new/checkout --dry-run --json\n  orbit --workspace <name> workspace rebind --path /new/checkout --json"
)]
pub struct WorkspaceRebindArgs {
    /// Existing Git checkout directory after the operator moved it
    #[arg(long, value_name = "DIR")]
    path: PathBuf,
    /// Validate and report the binding without writing anything (default: apply)
    #[arg(long)]
    dry_run: bool,
}

impl WorkspaceRebindArgs {
    pub fn execute_workspace_rebind_without_runtime(
        self,
        root_override: Option<&Path>,
        workspace_selector: Option<&str>,
    ) -> CommandOut {
        let selector = workspace_selector.ok_or_else(|| {
            OrbitError::InvalidInput(
                "workspace rebind requires --workspace <registered-name-or-id>".into(),
            )
        })?;
        let global_root = orbit_core::runtime::resolve_generation_root(root_override)?;
        let result = workspace_rebind::rebind_checkout_directory(
            &global_root,
            selector,
            &self.path,
            CheckoutRebindOptions {
                dry_run: self.dry_run,
                audit_role: orbit_core::ActorIdentity::from_env()
                    .audit_role()
                    .to_string(),
            },
        )?;
        let text = format!(
            "workspace '{}' {}\nrepo_root: {} -> {}\norbit_dir: {} -> {}",
            result.workspace_id,
            if result.dry_run {
                "would rebind"
            } else {
                "rebound"
            },
            result.old.repo_root.display(),
            result.new.repo_root.display(),
            result.old.orbit_dir.display(),
            result.new.orbit_dir.display(),
        );
        let checkout_json = |checkout: &WorkspaceCheckout| {
            json!({
                "workspace_id": checkout.workspace_id,
                "repo_root": checkout.repo_root,
                "orbit_dir": checkout.orbit_dir,
                "role": checkout.role,
                "owner_machine_id": checkout.owner_machine_id,
                "path_overrides": checkout.path_overrides,
            })
        };
        let value = json!({
            "workspace_id": result.workspace_id,
            "old": checkout_json(&result.old),
            "new": checkout_json(&result.new),
            "dry_run": result.dry_run,
        });
        Ok(Payload::detail(value, text).into())
    }
}
