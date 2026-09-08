use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use clap::Args;
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_core::{
    ManagedArtifactOutcome, ManagedArtifactScope, OrbitError, WorkspaceManagedArtifactSyncReport,
    reconcile_workspace_managed_artifacts,
};
use orbit_registry::workspace_registry;
use orbit_registry::{HostIdentityState, inspect_host_identity};

use crate::command::{CommandOut, Payload};

#[derive(Args)]
pub struct WorkspaceSyncArgs {
    /// Inspect and report pending convergence without writing anything
    #[arg(long)]
    pub check: bool,
    /// Emit structured JSON (equivalent to --format json)
    #[arg(long)]
    pub json: bool,
}

impl WorkspaceSyncArgs {
    pub fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        let cwd = std::env::current_dir().map_err(|error| OrbitError::Io(error.to_string()))?;
        let roots = RegisteredRuntimeFactory::try_resolve_initialized_roots(&cwd, root_override)?
            .ok_or_else(workspace_init_required)?;
        let bootstrap_roots =
            RegisteredRuntimeFactory::resolve_bootstrap_roots_for_cwd(&cwd, root_override)?;
        let global_root = bootstrap_roots.global_root;
        let registry = workspace_registry::load_registry_from(
            &workspace_registry::registry_path_for(&global_root),
        )?;
        let checkout = registry
            .checkouts
            .iter()
            .filter(|checkout| {
                checkout.orbit_dir == roots.shared_root && cwd.starts_with(&checkout.repo_root)
            })
            .max_by_key(|checkout| checkout.repo_root.components().count())
            .ok_or_else(workspace_init_required)?;
        if workspace_registry::find_workspace_by_id(&registry, &checkout.workspace_id).is_none() {
            return Err(workspace_init_required());
        }
        let host_id = match inspect_host_identity(&global_root)? {
            HostIdentityState::Present(identity) => identity.host_id,
            HostIdentityState::Legacy { .. } | HostIdentityState::Absent => {
                return Err(OrbitError::WorkspaceError(
                    "cannot sync workspace managed artifacts without an initialized host identity; run `orbit init`, then `orbit workspace init`".to_string(),
                ));
            }
        };
        let slug = checkout
            .repo_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let report = reconcile_workspace_managed_artifacts(
            &global_root,
            &checkout.orbit_dir,
            Some(&host_id),
            slug.as_deref(),
            self.check,
        )?;
        let exit_code = if self.check && report.has_pending_changes() {
            3
        } else {
            0
        };
        let doc = serde_json::to_value(&report)
            .map_err(|error| OrbitError::Execution(format!("serialize workspace sync: {error}")))?;
        Ok(Payload::detail(doc, format_report(&report))
            .with_exit_code(exit_code)
            .into())
    }
}

fn workspace_init_required() -> OrbitError {
    OrbitError::WorkspaceError(
        "current directory is not an initialized and registered Orbit workspace; run `orbit workspace init` first".to_string(),
    )
}

fn format_report(report: &WorkspaceManagedArtifactSyncReport) -> String {
    let mut counts: BTreeMap<(&str, &str, &str), usize> = BTreeMap::new();
    for action in &report.actions {
        *counts
            .entry((
                scope_name(action.scope),
                action.kind.as_str(),
                action.outcome.as_str(),
            ))
            .or_default() += 1;
    }
    let mut output = if report.check {
        "workspace managed-artifact sync check\n".to_string()
    } else {
        "workspace managed-artifact sync\n".to_string()
    };
    for ((scope, kind, outcome), count) in counts {
        let _ = writeln!(output, "  {scope} {kind}: {outcome}={count}");
    }
    for action in report.actions.iter().filter(|action| {
        matches!(
            action.outcome,
            ManagedArtifactOutcome::Preserved | ManagedArtifactOutcome::BindingDrift
        )
    }) {
        let _ = writeln!(
            output,
            "  {} {} {}: {}{}",
            scope_name(action.scope),
            action.kind,
            action.outcome.as_str(),
            action.path.display(),
            action
                .detail
                .as_deref()
                .map(|detail| format!(" — {detail}"))
                .unwrap_or_default()
        );
    }
    if report.check && report.has_pending_changes() {
        output.push_str(
            "pending managed-artifact changes; run `orbit workspace sync` to apply them\n",
        );
    } else if report.has_pending_changes() {
        output.push_str("managed artifacts converged\n");
    } else {
        output.push_str("already converged\n");
    }
    output
}

fn scope_name(scope: ManagedArtifactScope) -> &'static str {
    match scope {
        ManagedArtifactScope::HostGlobal => "host-global",
        ManagedArtifactScope::WorkspaceLocal => "workspace-local",
    }
}
