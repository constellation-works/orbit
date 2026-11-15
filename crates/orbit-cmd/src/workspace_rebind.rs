//! Host-local checkout relocation, with a durable decision before projections.

use std::path::Path;

use orbit_common::fs::{git::run_git, io::atomic_write_text};
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_core::{OrbitError, workspace_live_run_ids};
use orbit_registry::workspace_registry;
use orbit_store::Store;
use orbit_store::compose::{workspace_job_run_store, workspace_observational_backends};
use orbit_store::contracts::{AuditEventInsertParams, BindWorkspaceParams};
use orbit_store::maintenance::task_registry::{
    TaskRegistryStore, read_workspace_config, task_registry_path,
};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workspace::{WorkspaceCheckout, git_remotes_equivalent};
use serde::{Deserialize, Serialize};

const PENDING_REBIND: &str = "workspace-rebind-pending.json";

/// Refuse a missing host store before CLI generation admission can write.
/// The domain operation checks the same prerequisite for non-CLI callers.
pub fn require_checkout_rebind_host_store(global_root: &Path) -> Result<(), OrbitError> {
    let host_store = global_root.join("orbit.db");
    if !host_store.try_exists()? {
        return Err(OrbitError::WorkspaceRebindHostStoreRequired { path: host_store });
    }
    Ok(())
}

/// Options supplied by the invoking surface.
#[derive(Clone)]
pub struct CheckoutRebindOptions {
    pub dry_run: bool,
    pub audit_role: String,
}

/// The old and new host-local bindings, shared by both output modes.
#[derive(Serialize)]
pub struct CheckoutRebindResult {
    pub workspace_id: String,
    pub old: WorkspaceCheckout,
    pub new: WorkspaceCheckout,
    pub dry_run: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RebindDecision {
    schema_version: u32,
    execution_id: String,
    audit_role: String,
    workspace_id: String,
    partition_id: String,
    slug: String,
    old: WorkspaceCheckout,
    new: WorkspaceCheckout,
}

/// Keep runtime construction closed until every projection of a rebind is durable.
/// The runtime-free rebind command is the recovery route; it uses the saved decision.
pub(crate) fn refuse_pending_checkout_rebind(
    global_root: &Path,
    workspace_orbit_root: &Path,
) -> Result<(), OrbitError> {
    let pending = global_root.join(PENDING_REBIND);
    let bytes = match std::fs::read(&pending) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let decision: RebindDecision = serde_json::from_slice(&bytes).map_err(|error| {
        refused(format!(
            "cannot read pending checkout rebind '{}': {error}",
            pending.display()
        ))
    })?;
    if decision.schema_version != 1 {
        return Err(refused(format!(
            "checkout rebind decision has unsupported schema {}; upgrade Orbit before recovering it",
            decision.schema_version,
        )));
    }
    if workspace_orbit_root == decision.old.orbit_dir
        || workspace_orbit_root == decision.new.orbit_dir
    {
        return Err(refused(format!(
            "an interrupted checkout rebind needs recovery; repeat `orbit --workspace {} workspace rebind --path '{}'` before opening this workspace",
            decision.slug,
            decision.new.repo_root.display(),
        )));
    }
    Ok(())
}

/// Rebind only this host's registered checkout after an operator directory move.
///
/// Lock order is the workspace catalog lock, then short store transactions.
/// Git subprocesses finish before acquiring the catalog lock. The pending file
/// is the durable commit decision: catalog, task-checkout projection and audit
/// replay from it, and runtime construction refuses while it exists. Task
/// bundles and logical catalog/partition records are never rewritten.
pub fn rebind_checkout_directory(
    global_root: &Path,
    selector: &str,
    target: &Path,
    options: CheckoutRebindOptions,
) -> Result<CheckoutRebindResult, OrbitError> {
    // Refuse before taking a write lock or opening any store. Even a dry run
    // needs the activity checks, and must never initialize the host store.
    require_checkout_rebind_host_store(global_root)?;
    let target = target.canonicalize().map_err(|error| {
        refused(format!(
            "cannot use target '{}': {error}; pass an existing moved checkout",
            target.display()
        ))
    })?;
    let registry_path = workspace_registry::registry_path_for(global_root);
    let snapshot = workspace_registry::load_registry_from_read_only(&registry_path)?.registry;
    let workspace = match workspace_registry::find_workspace(&snapshot, selector)? {
        Some(workspace) => Some(workspace),
        None if Path::new(selector).is_absolute() => {
            workspace_registry::find_checkout_by_path(&snapshot, Path::new(selector)).and_then(
                |checkout| {
                    workspace_registry::find_workspace_by_id(&snapshot, &checkout.workspace_id)
                },
            )
        }
        None => None,
    }
    .ok_or_else(|| {
        refused(format!(
            "unknown workspace '{selector}'; pass a registered name, ID, or absolute checkout path"
        ))
    })?;
    let old =
        workspace_registry::find_checkout_by_id(&snapshot, &workspace.id).ok_or_else(|| {
            refused(format!(
                "workspace '{}' has no checkout on this host",
                workspace.id
            ))
        })?;
    validate_target_git(&target, workspace.git_remote.as_deref())?;
    let orbit_dir = target.join(".orbit");
    if orbit_dir.canonicalize()? != orbit_dir {
        return Err(refused(
            "the target .orbit must be a checkout-local directory, not a symlink",
        ));
    }
    let identity = read_workspace_config(&orbit_dir)?;
    let task_registry = TaskRegistryStore::open_read_only(&task_registry_path(global_root))?;
    let expected_partition = match task_registry.find_checkout_by_orbit_dir(&old.orbit_dir)? {
        Some(binding) => binding.partition_id,
        None => {
            let pending = global_root.join(PENDING_REBIND);
            if !pending.try_exists()? {
                return Err(refused(
                    "registered checkout has no task-store identity; restore its original binding before moving it",
                ));
            }
            let saved: RebindDecision = serde_json::from_str(&std::fs::read_to_string(pending)?)
                .map_err(|error| {
                    refused(format!("cannot read pending checkout rebind: {error}"))
                })?;
            if saved.workspace_id != workspace.id || saved.new.repo_root != target {
                return Err(refused(
                    "a different checkout rebind is pending; finish that rebind first",
                ));
            }
            saved.partition_id
        }
    };
    if identity.workspace_id != expected_partition {
        return Err(refused(
            "target .orbit identity differs from the registered checkout; use the original moved checkout",
        ));
    }
    let task_checkout = task_registry.find_workspace_checkout(&identity.workspace_id)?
        .ok_or_else(|| refused("target .orbit identity has no registered task checkout; use the original moved checkout"))?;
    let mut new = old.clone();
    new.repo_root = target.clone();
    new.orbit_dir = orbit_dir;
    for alias in &mut new.path_overrides {
        if let Ok(relative) = alias.strip_prefix(&old.repo_root) {
            *alias = target.join(relative);
        }
    }
    if old.repo_root != target && old.repo_root.try_exists()? {
        return Err(refused(format!(
            "old checkout '{}' still exists; move the original directory first",
            old.repo_root.display()
        )));
    }
    if old.orbit_dir != old.repo_root.join(".orbit") {
        return Err(refused(
            "this checkout uses an external Orbit data root; checkout rebind requires a checkout-local .orbit directory",
        ));
    }
    reject_managed_worktrees(&target, old)?;
    let decision = RebindDecision {
        schema_version: 1,
        execution_id: audit_execution_id("workspace-rebind"),
        audit_role: options.audit_role.clone(),
        workspace_id: workspace.id.clone(),
        partition_id: identity.workspace_id,
        slug: workspace.name.clone(),
        old: old.clone(),
        new,
    };
    workspace_registry::with_registry_lock(&registry_path, || {
        let mut registry =
            workspace_registry::load_registry_from_read_only(&registry_path)?.registry;
        if registry != snapshot {
            return Err(refused(
                "workspace registry changed during validation; repeat the rebind command",
            ));
        }
        let pending_path = global_root.join(PENDING_REBIND);
        let decision = if pending_path.try_exists()? {
            let saved: RebindDecision = serde_json::from_str(&std::fs::read_to_string(
                &pending_path,
            )?)
            .map_err(|error| refused(format!("cannot read pending checkout rebind: {error}")))?;
            if saved.schema_version != 1
                || saved.workspace_id != decision.workspace_id
                || saved.new.repo_root != target
                || saved.partition_id != decision.partition_id
                || (decision.old != saved.old && decision.old != saved.new)
            {
                return Err(refused(
                    "a different checkout rebind is pending; repeat the original workspace and target directory",
                ));
            }
            saved
        } else {
            decision
        };
        if (task_checkout.orbit_dir != decision.old.orbit_dir
            || task_checkout.repo_root != decision.old.repo_root)
            && (task_checkout.orbit_dir != decision.new.orbit_dir
                || task_checkout.repo_root != decision.new.repo_root)
        {
            return Err(refused(
                "target checkout identity is bound to another checkout",
            ));
        }
        reject_checkout_activity(global_root, &decision, task_registry.clone())?;
        let checkout = registry
            .checkouts
            .iter_mut()
            .find(|checkout| checkout.workspace_id == decision.workspace_id)
            .ok_or_else(|| refused("registered checkout disappeared; repeat the command"))?;
        *checkout = decision.new.clone();
        let machine = orbit_registry::inspect_machine_identity(global_root)?;
        workspace_registry::validate_workspace_registry(&mut registry, &(&machine).into())?;
        let result = CheckoutRebindResult {
            workspace_id: decision.workspace_id.clone(),
            old: decision.old.clone(),
            new: decision.new.clone(),
            dry_run: options.dry_run,
        };
        if options.dry_run {
            return Ok(result);
        }
        // Nothing mutates either registry until every refusal check succeeds.
        if !pending_path.try_exists()? {
            atomic_write_text(
                &pending_path,
                &serde_json::to_string_pretty(&decision)
                    .map_err(|error| OrbitError::Store(error.to_string()))?,
            )?;
        }
        finish_rebind_decision(global_root, &registry_path, &registry, &decision)
            .map_err(|error| refused(format!("checkout rebind needs recovery: {error}; repeat the same workspace rebind command")))?;
        Ok(result)
    })
}

fn validate_target_git(target: &Path, remote: Option<&str>) -> Result<(), OrbitError> {
    let root = run_git(target, &["rev-parse", "--show-toplevel"])?;
    if !root.success
        || Path::new(root.stdout.trim()).canonicalize()? != target
        || !target.join(".git").is_dir()
    {
        return Err(refused(format!(
            "target '{}' is not a primary Git checkout root; pass the moved repository directory",
            target.display()
        )));
    }
    let origin = run_git(target, &["remote", "get-url", "origin"])?;
    let Some(remote) = remote else {
        return Err(refused(
            "workspace has no registered source remote; bind its source identity before moving it",
        ));
    };
    if !origin.success
        || !git_remotes_equivalent(remote, origin.stdout.trim())
            .map_err(|error| refused(error.to_string()))?
    {
        return Err(refused(
            "target Git origin differs from the registered source remote; use the original moved checkout",
        ));
    }
    Ok(())
}

fn reject_managed_worktrees(target: &Path, old: &WorkspaceCheckout) -> Result<(), OrbitError> {
    let listing = run_git(target, &["worktree", "list", "--porcelain", "-z"])?;
    if !listing.success {
        return Err(refused(
            "cannot inspect Git worktrees; resolve the Git error before rebinding",
        ));
    }
    let old_managed = old.orbit_dir.join("state/worktrees");
    let new_managed = target.join(".orbit/state/worktrees");
    if listing
        .stdout
        .split('\0')
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(Path::new)
        .any(|path| path.starts_with(&old_managed) || path.starts_with(&new_managed))
    {
        return Err(refused(
            "Orbit-managed worktrees remain registered; finish their work and remove them before moving the primary checkout",
        ));
    }
    match std::fs::read_dir(&new_managed) {
        Ok(entries) => {
            for entry in entries {
                if entry?.path().join(".git").try_exists()? {
                    return Err(refused(
                        "an Orbit-managed worktree remains in the moved checkout; finish and remove it before rebinding",
                    ));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn reject_checkout_activity(
    global_root: &Path,
    decision: &RebindDecision,
    registry: TaskRegistryStore,
) -> Result<(), OrbitError> {
    let store = Store::open_read_only(&global_root.join("orbit.db"))?;
    let jobs = workspace_job_run_store(store.clone(), &decision.partition_id);
    if !workspace_live_run_ids(global_root, &decision.new.orbit_dir)?.is_empty() {
        return Err(refused(
            "workspace has a live run; stop or reconcile its runs before rebinding",
        ));
    }
    if !jobs.unsettled_local_pull_admissions()?.is_empty() {
        return Err(refused(
            "workspace has an unsettled pulled claim; settle it before rebinding",
        ));
    }
    for root in [&decision.old.orbit_dir, &decision.new.orbit_dir] {
        let root = root.to_string_lossy();
        if store.has_active_workspace_claim(&root, Some(&decision.partition_id))?
            || !store
                .inspect_active_task_reservations(&root, Some(&decision.partition_id))?
                .is_empty()
        {
            return Err(refused(
                "workspace has an active claim or path reservation; release it before rebinding",
            ));
        }
    }
    let backends =
        workspace_observational_backends(registry, decision.partition_id.clone(), store)?;
    if !backends
        .task
        .task
        .inspect_live_execution_claims()?
        .is_empty()
    {
        return Err(refused(
            "workspace has a live execution claim; settle it before rebinding",
        ));
    }
    Ok(())
}

fn finish_rebind_decision(
    global_root: &Path,
    registry_path: &Path,
    registry: &orbit_types::workspace::WorkspaceRegistry,
    decision: &RebindDecision,
) -> Result<(), OrbitError> {
    workspace_registry::save_registry_to(registry, registry_path)?;
    TaskRegistryStore::open(&task_registry_path(global_root))?.rebind_checkout(
        BindWorkspaceParams {
            partition_id: Some(decision.partition_id.clone()),
            slug: decision.slug.clone(),
            repo_root: decision.new.repo_root.clone(),
            workspace_path: decision.new.repo_root.clone(),
            orbit_dir: decision.new.orbit_dir.clone(),
            repo_fingerprint: None,
        },
    )?;
    record_checkout_rebind_audit(global_root, decision)?;
    // Preserve the durable decision as additional audit/recovery evidence.
    let history = global_root.join("workspace-rebinds");
    orbit_common::fs::io::create_private_dir_all(&history)?;
    std::fs::rename(
        global_root.join(PENDING_REBIND),
        history.join(format!("{}.json", decision.execution_id)),
    )?;
    orbit_common::fs::io::sync_parent_dir(&std::fs::File::open(&history)?)?;
    orbit_common::fs::io::sync_parent_dir(&std::fs::File::open(global_root)?)?;
    Ok(())
}

fn record_checkout_rebind_audit(
    global_root: &Path,
    decision: &RebindDecision,
) -> Result<(), OrbitError> {
    if Store::open_read_only(&global_root.join("orbit.db"))?
        .audit_execution_is_recorded(&decision.execution_id)?
    {
        return Ok(());
    }
    let params = AuditEventInsertParams {
        execution_id: decision.execution_id.clone(),
        command: "workspace".into(),
        subcommand: Some("rebind".into()),
        tool_name: None,
        target_type: Some("workspace".into()),
        target_id: Some(decision.workspace_id.clone()),
        role: decision.audit_role.clone(),
        status: AuditEventStatus::Success,
        exit_code: 0,
        duration_ms: 0,
        working_directory: decision.new.repo_root.to_string_lossy().into_owned(),
        arguments_json: Some(
            serde_json::to_string(decision)
                .map_err(|error| OrbitError::Store(error.to_string()))?,
        ),
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: None,
        host: None,
        pid: std::process::id(),
        session_id: None,
        workspace_id: Some(decision.partition_id.clone()),
        caller_machine_id: None,
        caller_machine_name: None,
        process_machine_id: None,
        process_machine_name: None,
        transport: None,
        effective_capabilities: Default::default(),
        origin_session_id: None,
        mcp_call_id: None,
        lease_id: None,
        task_id: None,
        job_run_id: None,
        activity_id: None,
        step_index: None,
    };
    Store::append_audit_event_at_path(&global_root.join("orbit.db"), &params, Default::default())
}

fn refused(message: impl Into<String>) -> OrbitError {
    OrbitError::WorkspaceError(message.into())
}
