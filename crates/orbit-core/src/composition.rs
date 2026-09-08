//! Composition root joining resolved configuration, the runtime kernel, and adapters.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_config::{ConfigRoots, ResolvedConfig};
use orbit_store::compose::global_policy_def_store;

use crate::bootstrap::global_defaults::global_defaults_are_current;
use crate::bootstrap::init::ensure_orbit_root_initialized;
use crate::bootstrap::policy::seed_default_policies;
use crate::bootstrap::task_migration::apply_configured_id_start;
use crate::runtime::run_input::managed_run_context_from_env;
use crate::runtime::{
    HostLifetime, OrbitRuntime, OrbitRuntimeRoots, ResolvedOrbitRoots, WorkspaceRootHint,
    WorkspaceRuntimeBinding, resolve_bootstrap_roots, resolve_bootstrap_roots_with_hint,
    resolve_global_root, resolve_initialize_roots, resolve_initialize_roots_with_hint,
};

impl OrbitRuntime {
    pub fn initialize() -> Result<Self, OrbitError> {
        Self::initialize_with_root_override(None)
    }

    pub fn initialize_with_root_override(root_override: Option<&Path>) -> Result<Self, OrbitError> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let roots = Self::resolve_roots_for_cwd(&cwd, root_override)?;
        Self::initialize_from_resolved_roots(roots, None)
    }

    pub fn initialize_from_resolved_roots(
        roots: OrbitRuntimeRoots,
        binding: Option<WorkspaceRuntimeBinding>,
    ) -> Result<Self, OrbitError> {
        ensure_orbit_root_initialized(&roots.global_root, &roots.shared_root)?;
        build_runtime(
            &roots.global_root,
            &roots.shared_root,
            &roots.local_root,
            binding,
            true,
            HostLifetime::ShortLived,
        )
    }

    /// Open an existing workspace for an observation-only command without
    /// reconciling stale job runs as a side effect of runtime construction.
    pub fn initialize_from_resolved_roots_read_only(
        roots: OrbitRuntimeRoots,
        binding: Option<WorkspaceRuntimeBinding>,
    ) -> Result<Self, OrbitError> {
        build_runtime(
            &roots.global_root,
            &roots.shared_root,
            &roots.local_root,
            binding,
            false,
            HostLifetime::ShortLived,
        )
    }

    pub fn resolve_roots_for_cwd(
        cwd: &Path,
        root_override: Option<&Path>,
    ) -> Result<OrbitRuntimeRoots, OrbitError> {
        roots_from_resolved(
            resolve_initialize_roots(cwd, root_override)?,
            has_explicit_root_override(root_override),
        )
    }

    pub fn resolve_roots_for_cwd_with_hint(
        cwd: &Path,
        root_override: Option<&Path>,
        hint: Option<&WorkspaceRootHint>,
    ) -> Result<OrbitRuntimeRoots, OrbitError> {
        roots_from_resolved(
            resolve_initialize_roots_with_hint(cwd, root_override, hint)?,
            has_explicit_root_override(root_override),
        )
    }

    pub fn resolve_bootstrap_roots_for_cwd(
        cwd: &Path,
        root_override: Option<&Path>,
    ) -> Result<OrbitRuntimeRoots, OrbitError> {
        roots_from_resolved(
            resolve_bootstrap_roots(cwd, root_override)?,
            has_explicit_root_override(root_override),
        )
    }

    pub fn resolve_bootstrap_roots_for_cwd_with_hint(
        cwd: &Path,
        root_override: Option<&Path>,
        hint: Option<&WorkspaceRootHint>,
    ) -> Result<OrbitRuntimeRoots, OrbitError> {
        roots_from_resolved(
            resolve_bootstrap_roots_with_hint(cwd, root_override, hint)?,
            has_explicit_root_override(root_override),
        )
    }

    pub fn from_roots(global_root: &Path, workspace_root: &Path) -> Result<Self, OrbitError> {
        Self::from_resolved_roots(global_root, workspace_root, workspace_root)
    }

    pub fn from_roots_with_binding(
        global_root: &Path,
        workspace_root: &Path,
        binding: WorkspaceRuntimeBinding,
    ) -> Result<Self, OrbitError> {
        Self::from_roots_with_binding_for(
            global_root,
            workspace_root,
            binding,
            HostLifetime::ShortLived,
        )
    }

    pub fn from_roots_with_binding_for(
        global_root: &Path,
        workspace_root: &Path,
        binding: WorkspaceRuntimeBinding,
        host_lifetime: HostLifetime,
    ) -> Result<Self, OrbitError> {
        Self::from_resolved_roots_with_binding_for(
            global_root,
            workspace_root,
            workspace_root,
            binding,
            host_lifetime,
        )
    }

    pub fn from_resolved_roots(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
    ) -> Result<Self, OrbitError> {
        build_runtime(
            global_root,
            shared_root,
            local_root,
            None,
            true,
            HostLifetime::ShortLived,
        )
    }

    pub fn from_resolved_roots_with_binding(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
        binding: WorkspaceRuntimeBinding,
    ) -> Result<Self, OrbitError> {
        Self::from_resolved_roots_with_binding_for(
            global_root,
            shared_root,
            local_root,
            binding,
            HostLifetime::ShortLived,
        )
    }

    /// Open a runtime whose embed worker matches the constructing host's lifetime.
    pub fn from_resolved_roots_with_binding_for(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
        binding: WorkspaceRuntimeBinding,
        host_lifetime: HostLifetime,
    ) -> Result<Self, OrbitError> {
        build_runtime(
            global_root,
            shared_root,
            local_root,
            Some(binding),
            true,
            host_lifetime,
        )
    }

    pub fn from_resolved_roots_read_only_with_binding(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
        binding: WorkspaceRuntimeBinding,
    ) -> Result<Self, OrbitError> {
        build_runtime(
            global_root,
            shared_root,
            local_root,
            Some(binding),
            false,
            HostLifetime::ShortLived,
        )
    }

    pub fn in_memory() -> Result<Self, OrbitError> {
        let temp_dir = tempfile::Builder::new()
            .prefix("orbit-in-memory-")
            .tempdir()
            .map_err(|error| OrbitError::Io(error.to_string()))?;
        let data_root = temp_dir.path().to_path_buf();
        let workspace_root = data_root.join(".orbit");
        let runtime_config = prepare_resolved_config(&data_root, &workspace_root)?;
        Self::build_in_memory_from_resolved_config(
            &data_root,
            &workspace_root,
            &runtime_config,
            temp_dir,
        )
    }
}

fn build_runtime(
    global_root: &Path,
    shared_root: &Path,
    local_root: &Path,
    binding: Option<WorkspaceRuntimeBinding>,
    reconcile_stale_runs: bool,
    host_lifetime: HostLifetime,
) -> Result<OrbitRuntime, OrbitError> {
    let layout_report = match orbit_store::workflow::layout::upgrade_workspace_layout(shared_root) {
        Ok(report) => report,
        Err(error) if error.is_readonly_or_access_failure() => {
            tracing::warn!(
                target: "orbit.core.bootstrap",
                root = %shared_root.display(),
                error = %error,
                "skipped incidental workspace layout persistence"
            );
            orbit_store::workflow::layout::LayoutUpgradeReport::default()
        }
        Err(error) => return Err(error),
    };
    let runtime_config = prepare_resolved_config(global_root, shared_root)?;
    let runtime = OrbitRuntime::build_from_resolved_config(
        global_root,
        shared_root,
        local_root,
        binding,
        &runtime_config,
        layout_report,
        host_lifetime,
    )?;
    if reconcile_stale_runs && !managed_run_context_from_env() {
        runtime.reconcile_stale_job_runs_on_open();
    }
    Ok(runtime)
}

fn prepare_resolved_config(
    global_root: &Path,
    workspace_root: &Path,
) -> Result<ResolvedConfig, OrbitError> {
    let resolved = ResolvedConfig::load(&ConfigRoots::new(global_root, workspace_root))?;
    if let Some(start) = resolved.tasks_id_start
        && let Err(error) = apply_configured_id_start(global_root, start)
    {
        if error.is_readonly_or_access_failure() {
            tracing::warn!(
                target: "orbit.core.bootstrap",
                root = %global_root.display(),
                error = %error,
                "skipped incidental task allocator bootstrap persistence"
            );
        } else {
            return Err(error);
        }
    }
    // Not every runtime open bootstraps its global root: opening a registered
    // checkout or an observation-only runtime goes straight to composition, so
    // the shipped default policy is seeded here. `policy_dir` is derived from
    // the global root and cannot be relocated by config, so the same stamp that
    // lets bootstrap skip reconciliation settles this seed too — a root already
    // reconciled by this binary carries the policy.
    if !global_defaults_are_current(global_root) {
        let global_policy_store = global_policy_def_store(resolved.persistence.policy_dir.clone());
        if let Err(error) = seed_default_policies(global_policy_store.as_ref(), false) {
            if error.is_readonly_or_access_failure() {
                tracing::warn!(
                    target: "orbit.core.bootstrap",
                    root = %global_root.display(),
                    error = %error,
                    "skipped incidental default-policy persistence"
                );
            } else {
                return Err(error);
            }
        }
    }
    Ok(resolved)
}

fn roots_from_resolved(
    resolved: ResolvedOrbitRoots,
    pin_global_to_shared: bool,
) -> Result<OrbitRuntimeRoots, OrbitError> {
    let global_root = if pin_global_to_shared {
        resolved.shared_root.clone()
    } else {
        resolve_global_root()?
    };
    Ok(OrbitRuntimeRoots {
        global_root,
        shared_root: resolved.shared_root,
        local_root: resolved.local_root,
    })
}

fn has_explicit_root_override(root_override: Option<&Path>) -> bool {
    root_override.is_some()
        || std::env::var("ORBIT_ROOT").is_ok_and(|value| !value.trim().is_empty())
}
