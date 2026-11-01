//! `orbit workspace sync --all`: converge every workspace registered on this
//! host, one result per workspace.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_core::{OrbitError, WorkspaceManagedArtifactSyncReport, workspace_live_run_ids};
use orbit_registry::workspace_registry;
use orbit_types::workspace::{Workspace, WorkspaceCheckout};
use serde::Serialize;
use serde_json::json;

use super::sync::{converge_checkout, host_machine_id};
use crate::command::{CommandOut, Payload};

/// Live run IDs a skip reason names before it summarises the rest.
const LIVE_RUNS_NAMED: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WorkspaceSyncStatus {
    /// Changes were applied.
    Synced,
    /// `--check` found changes a sync would apply.
    Pending,
    AlreadyCurrent,
    /// Left untouched, with the `reason`; never a failure.
    Skipped,
    Failed,
}

impl WorkspaceSyncStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Synced => "synced",
            Self::Pending => "pending",
            Self::AlreadyCurrent => "already current",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Serialize)]
struct WorkspaceSyncRecord {
    workspace: String,
    id: String,
    repo_root: PathBuf,
    status: WorkspaceSyncStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// What was applied (or, with `--check`, is pending), as
    /// `<scope> <kind> <outcome>=<count>`.
    changes: Vec<String>,
    warnings: Vec<String>,
}

impl WorkspaceSyncRecord {
    fn new(
        workspace: &str,
        checkout: &WorkspaceCheckout,
        status: WorkspaceSyncStatus,
    ) -> WorkspaceSyncRecord {
        WorkspaceSyncRecord {
            workspace: workspace.to_string(),
            id: checkout.workspace_id.clone(),
            repo_root: checkout.repo_root.clone(),
            status,
            reason: None,
            error: None,
            changes: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn skipped(workspace: &str, checkout: &WorkspaceCheckout, reason: String) -> Self {
        Self {
            reason: Some(reason),
            ..Self::new(workspace, checkout, WorkspaceSyncStatus::Skipped)
        }
    }

    fn failed(workspace: &str, checkout: &WorkspaceCheckout, error: &OrbitError) -> Self {
        Self {
            error: Some(error.to_string()),
            ..Self::new(workspace, checkout, WorkspaceSyncStatus::Failed)
        }
    }

    fn line(&self) -> String {
        let mut line = format!("{}: {}", self.workspace, self.status.label());
        if let Some(detail) = self.reason.as_ref().or(self.error.as_ref()) {
            line.push_str(&format!(" — {detail}"));
        } else if !self.changes.is_empty() {
            line.push_str(&format!(" ({})", self.changes.join(", ")));
        }
        line
    }
}

/// Sync (or with `check`, inspect) every checkout in this host's workspace
/// registry. One workspace failing never stops the rest: the exit code is `1`
/// when any failed, else `3` for `--check` with pending changes, else `0`.
pub(super) fn execute_sync_all(check: bool, root_override: Option<&Path>) -> CommandOut {
    // The host root, never derived from cwd: the command works from any
    // directory, including a home directory that holds the global root.
    let global_root = orbit_core::runtime::resolve_generation_root(root_override)?;
    let registry = workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
        &global_root,
    ))?;
    let machine_id = host_machine_id(&global_root)?;

    let mut records = Vec::new();
    // Checkouts that share one data root would converge the same files under
    // different routine bindings, so only the first is synced.
    let mut synced_roots: BTreeMap<&Path, &str> = BTreeMap::new();
    for checkout in &registry.checkouts {
        let Some(workspace) =
            workspace_registry::find_workspace_by_id(&registry, &checkout.workspace_id)
        else {
            records.push(WorkspaceSyncRecord::failed(
                &checkout.workspace_id,
                checkout,
                &OrbitError::WorkspaceError(format!(
                    "registry has no workspace record for checkout '{}'",
                    checkout.repo_root.display()
                )),
            ));
            continue;
        };
        let record = if let Some(first) = synced_roots.get(checkout.orbit_dir.as_path()) {
            WorkspaceSyncRecord::skipped(
                &workspace.name,
                checkout,
                format!("shares its data root with workspace `{first}`"),
            )
        } else {
            synced_roots.insert(&checkout.orbit_dir, &workspace.name);
            sync_registered_checkout(&global_root, &machine_id, workspace, checkout, check)
        };
        records.push(record);
    }
    present(check, records)
}

fn sync_registered_checkout(
    global_root: &Path,
    machine_id: &str,
    workspace: &Workspace,
    checkout: &WorkspaceCheckout,
    check: bool,
) -> WorkspaceSyncRecord {
    if let Some(reason) = unsyncable_checkout(checkout) {
        return WorkspaceSyncRecord::skipped(&workspace.name, checkout, reason);
    }
    // `--check` writes nothing, so it can read a workspace mid-run. A sync
    // must not rewrite the managed definitions a drain or run is executing.
    if !check {
        match workspace_live_run_ids(global_root, &checkout.orbit_dir) {
            Ok(run_ids) if !run_ids.is_empty() => {
                return WorkspaceSyncRecord::skipped(
                    &workspace.name,
                    checkout,
                    live_runs_reason(&run_ids),
                );
            }
            Ok(_) => {}
            Err(error) => return WorkspaceSyncRecord::failed(&workspace.name, checkout, &error),
        }
    }
    match converge_checkout(global_root, machine_id, workspace, checkout, check) {
        Ok(report) => converged_record(&workspace.name, checkout, check, &report),
        Err(error) => WorkspaceSyncRecord::failed(&workspace.name, checkout, &error),
    }
}

/// Why a registered checkout cannot be converged, when it cannot: sync never
/// recreates a directory the operator removed.
fn unsyncable_checkout(checkout: &WorkspaceCheckout) -> Option<String> {
    if !checkout.repo_root.is_dir() {
        return Some(format!(
            "checkout path is missing: {}",
            checkout.repo_root.display()
        ));
    }
    if !checkout.repo_root.join(".git").exists() {
        return Some(format!(
            "not a git checkout: {}",
            checkout.repo_root.display()
        ));
    }
    if !checkout.orbit_dir.is_dir() {
        return Some(format!(
            "Orbit data directory is missing: {}",
            checkout.orbit_dir.display()
        ));
    }
    None
}

fn live_runs_reason(run_ids: &[String]) -> String {
    let named = run_ids
        .iter()
        .take(LIVE_RUNS_NAMED)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let more = run_ids.len().saturating_sub(LIVE_RUNS_NAMED);
    let more = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    format!("a live drain or run holds the workspace ({named}{more}); sync again once it ends")
}

fn converged_record(
    workspace: &str,
    checkout: &WorkspaceCheckout,
    check: bool,
    report: &WorkspaceManagedArtifactSyncReport,
) -> WorkspaceSyncRecord {
    let status = match (report.has_pending_changes(), check) {
        (false, _) => WorkspaceSyncStatus::AlreadyCurrent,
        (true, true) => WorkspaceSyncStatus::Pending,
        (true, false) => WorkspaceSyncStatus::Synced,
    };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for action in report
        .actions
        .iter()
        .filter(|action| action.outcome.requires_write())
    {
        let scope = match action.scope {
            orbit_core::ManagedArtifactScope::HostGlobal => "host-global",
            orbit_core::ManagedArtifactScope::WorkspaceLocal => "workspace-local",
        };
        *counts
            .entry(format!(
                "{scope} {} {}",
                action.kind,
                action.outcome.as_str()
            ))
            .or_default() += 1;
    }
    WorkspaceSyncRecord {
        changes: counts
            .into_iter()
            .map(|(change, count)| format!("{change}={count}"))
            .collect(),
        warnings: report.warnings.clone(),
        ..WorkspaceSyncRecord::new(workspace, checkout, status)
    }
}

fn present(check: bool, records: Vec<WorkspaceSyncRecord>) -> CommandOut {
    let count = |status: WorkspaceSyncStatus| {
        records
            .iter()
            .filter(|record| record.status == status)
            .count()
    };
    let failed = count(WorkspaceSyncStatus::Failed);
    let pending = count(WorkspaceSyncStatus::Pending);
    let exit_code = if failed > 0 {
        1
    } else if pending > 0 {
        3
    } else {
        0
    };

    let mut text = String::new();
    if records.is_empty() {
        text.push_str("no workspaces registered on this host\n");
    }
    for record in &records {
        text.push_str(&record.line());
        text.push('\n');
        for warning in &record.warnings {
            text.push_str(&format!("  warning: {warning}\n"));
        }
    }
    let skipped = count(WorkspaceSyncStatus::Skipped);
    if failed > 0 {
        text.push_str(&format!(
            "{failed} of {} workspaces failed; resolve the errors above, then rerun `orbit workspace sync --all`\n",
            records.len()
        ));
    } else if pending > 0 {
        text.push_str(
            "pending managed-artifact changes; run `orbit workspace sync --all` to apply them\n",
        );
    }
    if skipped > 0 {
        text.push_str(&format!("{skipped} skipped; the reasons are above\n"));
    }
    let doc = json!({
        "check": check,
        "workspaces": records,
        "summary": {
            "synced": count(WorkspaceSyncStatus::Synced),
            "pending": pending,
            "already_current": count(WorkspaceSyncStatus::AlreadyCurrent),
            "skipped": skipped,
            "failed": failed,
        },
    });
    Ok(Payload::detail(doc, text).with_exit_code(exit_code).into())
}
