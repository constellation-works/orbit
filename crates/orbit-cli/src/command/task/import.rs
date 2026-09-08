use std::path::PathBuf;

use clap::{Args, ValueEnum};
use orbit_core::OrbitRuntime;
use orbit_core::bootstrap::task_migration::{ImportAction, ImportConflictPolicy};
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

/// `orbit task import` — import task bundles from a tar.zst archive.
#[derive(Args)]
pub struct TaskImportArgs {
    /// Archive to import (tar.zst produced by `orbit task export`).
    #[arg(value_name = "ARCHIVE")]
    pub archive: PathBuf,
    /// Target task-registry workspace id (default: the archive's source
    /// workspace, registering it locally if unknown).
    #[arg(long)]
    pub workspace: Option<String>,
    /// How to resolve an incoming task id that already exists locally.
    #[arg(long = "on-conflict", value_enum, default_value_t = ConflictArg::Renumber)]
    pub on_conflict: ConflictArg,
    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

/// CLI spelling of [`ImportConflictPolicy`].
#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum ConflictArg {
    /// Allocate a fresh local id for colliding tasks and rewrite references.
    Renumber,
    /// Leave the local task and drop the incoming one.
    Skip,
    /// Abort the whole import on the first collision.
    Fail,
}

impl From<ConflictArg> for ImportConflictPolicy {
    fn from(value: ConflictArg) -> Self {
        match value {
            ConflictArg::Renumber => ImportConflictPolicy::Renumber,
            ConflictArg::Skip => ImportConflictPolicy::Skip,
            ConflictArg::Fail => ImportConflictPolicy::Fail,
        }
    }
}

fn action_label(action: ImportAction) -> &'static str {
    match action {
        ImportAction::Kept => "kept",
        ImportAction::Renumbered => "renumbered",
        ImportAction::AlreadyPresent => "already-present",
        ImportAction::SkippedConflict => "skipped",
    }
}

impl Execute for TaskImportArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let outcome = runtime.import_tasks(
            &self.archive,
            self.workspace.as_deref(),
            self.on_conflict.into(),
        )?;

        let tasks: Vec<_> = outcome
            .tasks
            .iter()
            .map(|task| {
                json!({
                    "source_id": task.source_id,
                    "final_id": task.final_id,
                    "action": action_label(task.action),
                })
            })
            .collect();
        let doc = json!({
            "workspace_id": outcome.workspace_id,
            "registered_workspace": outcome.registered_workspace,
            "id_remap": outcome.id_remap,
            "id_map_path": outcome.id_map_path.as_ref().map(|p| p.display().to_string()),
            "projection_degraded": outcome.projection.degraded_reason,
            "tasks": tasks,
        });
        let mut lines = vec![format!(
            "imported into workspace '{}'",
            outcome.workspace_id
        )];
        if outcome.registered_workspace {
            lines.push("  registered new workspace binding".to_string());
        }
        for task in &outcome.tasks {
            if task.source_id == task.final_id {
                lines.push(format!(
                    "  {}  {}",
                    action_label(task.action),
                    task.source_id
                ));
            } else {
                lines.push(format!(
                    "  {}  {} -> {}",
                    action_label(task.action),
                    task.source_id,
                    task.final_id
                ));
            }
        }
        if let Some(path) = &outcome.id_map_path {
            lines.push(format!("  id mapping written to {}", path.display()));
        }
        if let Some(reason) = &outcome.projection.degraded_reason {
            lines.push(format!("  warning: projection degraded: {reason}"));
        }
        Ok(Payload::detail(doc, lines.join("\n")).into())
    }
}
