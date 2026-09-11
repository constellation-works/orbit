use clap::Args;
use orbit_cmd::registry_runtime::{
    RegisteredRuntimeFactory, global_root_for, workspace_runtime_binding,
};
use orbit_cmd::{MigrateCommands, MigrateStatus, migrate_dry_run_at};
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::json;

use crate::command::{Block, CommandOut, Execute, Payload};

// ORB-10012: source provenance for the versioned migration surface.
/// `orbit migrate` — versioned `.orbit/` upgrade surface.
///
/// Both migration ledgers auto-apply on workspace open (the SQLite schema
/// ledger inside `Store::open`, the workspace-layout registry in the runtime
/// pre-flight), so the apply path simply opens the runtime and reports what
/// happened. The default and `--dry-run` inspect *without* opening a runtime,
/// which is the only way to list pending migrations instead of silently
/// applying them. `--confirm` selects the apply path.
#[derive(Args)]
#[command(about = "Apply or inspect pending .orbit layout and store schema migrations")]
pub struct MigrateCommand {
    /// Apply pending migrations
    #[arg(long, conflicts_with = "dry_run")]
    pub confirm: bool,
    /// Explicitly request the default non-destructive inspection mode
    #[arg(long, conflicts_with = "confirm")]
    pub dry_run: bool,
    /// Emit machine-readable JSON instead of the table.
    #[arg(long)]
    pub json: bool,
}

impl MigrateCommand {
    /// `--dry-run` dispatches before the runtime bootstrap in `main.rs`:
    /// opening a runtime would auto-apply the very migrations being listed.
    pub fn execute_without_runtime(
        self,
        root_override: Option<&std::path::Path>,
        workspace_selector: Option<&str>,
    ) -> CommandOut {
        let (global_root, orbit_dir) = match workspace_selector
            .map(str::trim)
            .filter(|selector| !selector.is_empty())
        {
            Some(selector) => {
                let global_root = global_root_for(root_override)?;
                let selected =
                    RegisteredRuntimeFactory::resolve_workspace_selector(&global_root, selector)?;
                // Validate the checkout-local identity without opening a
                // runtime: missing or malformed identity is a re-homing
                // boundary, not permission to inspect a different root.
                workspace_runtime_binding(&selected.workspace, &selected.checkout)?;
                (global_root, selected.checkout.orbit_dir)
            }
            None => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let Some(resolved) =
                    RegisteredRuntimeFactory::try_resolve_initialized_roots(&cwd, root_override)?
                else {
                    return Err(OrbitError::WorkspaceError(
                        "no initialized orbit workspace found from the current directory; run `orbit init` first"
                            .to_string(),
                    ));
                };
                let global_root =
                    RegisteredRuntimeFactory::resolve_bootstrap_roots_for_cwd(&cwd, root_override)?
                        .global_root;
                (global_root, resolved.shared_root)
            }
        };
        let status = migrate_dry_run_at(&global_root, &orbit_dir)?;

        // `--dry-run` exits nonzero when there is anything pending, and a
        // failing command has no payload — stdout carries records only
        // (spec §5). The readout is the diagnostic that explains the exit
        // code, so on the failing paths it goes to stderr alongside it.
        if status.newer_than_binary() {
            eprintln!("{}", status_trailer(&status));
            return Err(OrbitError::Migration(format!(
                "workspace '{}' was written by a newer orbit than this binary supports; \
                 upgrade orbit",
                status.orbit_dir.display()
            )));
        }
        let pending = status.pending_total();
        if pending > 0 {
            eprintln!("{}", status_trailer(&status));
            return Err(OrbitError::Execution(format!(
                "{pending} migration(s) pending; run `orbit migrate --confirm` to apply"
            )));
        }
        status_payload(&status, true)
    }
}

impl Execute for MigrateCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        // Apply path: opening the runtime (before this command ran) already
        // applied everything pending; report the outcome.
        let status = runtime.migrate_status()?;
        status_payload(&status, false)
    }
}

fn status_payload(status: &MigrateStatus, dry_run: bool) -> CommandOut {
    let doc = json!({
        "orbit_dir": status.orbit_dir.to_string_lossy(),
        "binary_version": status.binary_version,
        "dry_run": dry_run,
        "layout": {
            "current": status.layout_version,
            "supported": status.layout_supported,
            "pending": status.pending_layout.iter().map(|m| json!({
                "version": m.version,
                "name": m.name,
                "description": m.description,
            })).collect::<Vec<_>>(),
        },
        "schema": {
            "current": status.schema_version,
            "supported": status.schema_supported,
            "pending": status.pending_schema.iter().map(|m| json!({
                "version": m.version,
                "name": m.name,
            })).collect::<Vec<_>>(),
        },
        "applied_layout": status.applied_layout.iter().map(|m| json!({
            "version": m.version,
            "name": m.name,
        })).collect::<Vec<_>>(),
        "up_to_date": status.pending_total() == 0 && !status.newer_than_binary(),
    });

    // A fixed-shape status readout, not a result set: every component is named
    // even when the two version columns happen to agree.
    let mut table = crate::output::table::build_table(&["COMPONENT", "CURRENT", "SUPPORTED"])
        .keep_all_columns();
    table.add_row(vec![
        "workspace layout".to_string(),
        status.layout_version.to_string(),
        status.layout_supported.to_string(),
    ]);
    table.add_row(vec![
        "store schema".to_string(),
        status.schema_version.to_string(),
        status.schema_supported.to_string(),
    ]);

    Ok(Payload::blocks(
        doc,
        vec![Block::table(table), Block::text(status_trailer(status))],
    )
    .into())
}

/// The prose under the version table: where the workspace is, what applied on
/// open, and what is still pending.
fn status_trailer(status: &MigrateStatus) -> String {
    let mut trailer = format!(
        "\nworkspace: {} (orbit {})",
        status.orbit_dir.display(),
        status.binary_version
    );
    for applied in &status.applied_layout {
        trailer.push_str(&format!(
            "\napplied on open: layout v{} ({})",
            applied.version, applied.name
        ));
    }

    // The caller emits the refusal error for a workspace newer than the binary,
    // so the readout stops here rather than advising a migration that will not
    // run.
    if !status.newer_than_binary() {
        if status.pending_total() == 0 {
            trailer.push_str(&format!(
                "\n\n{}",
                crate::output::color::text(
                    "Workspace is up to date.",
                    crate::output::color::Role::Ok
                )
            ));
        } else {
            trailer.push_str("\n\nPending migrations:");
            for pending in &status.pending_layout {
                trailer.push_str(&format!(
                    "\n  layout v{} ({}) — {}",
                    pending.version, pending.name, pending.description
                ));
            }
            for pending in &status.pending_schema {
                trailer.push_str(&format!(
                    "\n  schema v{} ({})",
                    pending.version, pending.name
                ));
            }
            trailer.push_str(&format!(
                "\n\nRun `orbit migrate --confirm` to apply (migrations also auto-apply on workspace open).\n\
                 Before a major upgrade, consider backing up workspace state first:\n\
                 cp -a {orbit_dir} {orbit_dir}.bak",
                orbit_dir = status.orbit_dir.display()
            ));
        }
    }
    trailer
}
