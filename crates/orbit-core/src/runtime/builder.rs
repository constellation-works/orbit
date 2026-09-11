use std::path::Path;
use std::sync::Arc;

use orbit_policy::PolicyEngine;
use orbit_search::{EmbedWorker, SemanticIndex};
use orbit_store::Store;
use orbit_store::compose::{
    WorkspaceTaskBackends, audit_event_store_sqlite, automation_store, coordination_task_backends,
    global_executor_def_store, global_policy_def_store, invocation_store_from_store,
    layered_policy_def_store, operation_store, review_store, task_reservation_store_sqlite,
    tool_store_sqlite, v2_audit_store_from_store, workspace_job_run_store,
    workspace_policy_def_store, workspace_task_backends,
};
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, WorkspaceConfig, read_workspace_config_optional,
    task_registry_path, write_workspace_config,
};

use orbit_common::OrbitError;
use orbit_config::ResolvedConfig;
use orbit_engine::PrConfig;
use orbit_tools::ToolRegistry;
use orbit_tools::external::ExternalTool;
use orbit_types::policy::DEFAULT_POLICY_NAME;
use orbit_types::workspace::WorkspacePaths;

use crate::context::OrbitContext;
use crate::context::{
    ActorIdentity, OrbitExecutionAssets, OrbitHostStore, OrbitPolicyContext, OrbitRuntimeSettings,
    OrbitStores,
};
use crate::runtime::{HostLifetime, WorkspaceRuntimeBinding};
use crate::skill_catalog::SkillCatalog;

/// Job-run partition used when an explicit `--root` data directory is opened
/// without a selected checkout. Never written to `config.yaml` and never
/// inserted as a `workspace_checkout_bindings` row.
///
/// Public because it is the one partition id no registry ever claims: host
/// maintenance that decides whether a task-store partition is orphaned must
/// recognize it rather than delete the tasks every `--root` write lands in
/// [ORB-12119].
pub const UNBOUND_DATA_DIR_WORKSPACE_ID: &str = "ws_unbound-data-dir";

/// Runtime builder. Global root provides activities, jobs, executors, policies,
/// config, global skills, and SQLite. Shared root provides existing workspace
/// state. Local root is carried for per-worktree artifact phases.
pub(crate) fn build_context_from_roots(
    global_root: &Path,
    workspace_root: &Path,
    local_root: &Path,
    binding: Option<&WorkspaceRuntimeBinding>,
    runtime_config: &ResolvedConfig,
    host_lifetime: HostLifetime,
) -> Result<OrbitContext, OrbitError> {
    let persistence = &runtime_config.persistence;

    let store = Store::open(&persistence.audit_db)?;

    // workspace_root IS the .orbit dir. For custom roots outside the repo,
    // prefer the registry's workspace root over the parent-directory fallback.
    let repo_root = binding
        .map(|binding| binding.repo_root.clone())
        .unwrap_or_else(|| {
            workspace_root
                .parent()
                .unwrap_or(workspace_root)
                .to_path_buf()
        });
    let paths = WorkspacePaths::new_with_local(
        repo_root,
        workspace_root.to_path_buf(),
        local_root.to_path_buf(),
        global_root.to_path_buf(),
    );

    let task_backends = build_v2_task_backends(global_root, &paths, binding)?;
    let configured = read_workspace_config_optional(&paths.orbit_dir)?;
    let workspace_id = if is_explicit_data_dir(global_root, &paths.orbit_dir) {
        binding
            .map(|binding| binding.logical_workspace_id.clone())
            .or_else(|| {
                configured
                    .as_ref()
                    .map(|config| config.workspace_id.clone())
            })
    } else {
        configured
            .as_ref()
            .map(|config| config.workspace_id.clone())
    }
    .unwrap_or_else(|| UNBOUND_DATA_DIR_WORKSPACE_ID.to_string());
    let import_report = if configured.is_none() {
        orbit_store::workflow::legacy_state::ImportReport::skipped()
    } else {
        match orbit_store::workflow::legacy_state::import_legacy_v2_state(
            &store,
            &paths.orbit_dir,
            &workspace_id,
        ) {
            Ok(report) => report,
            Err(error) if error.is_readonly_or_access_failure() => {
                tracing::warn!(
                    target: "orbit.core.bootstrap",
                    root = %paths.orbit_dir.display(),
                    error = %error,
                    "skipped incidental legacy-state import persistence"
                );
                orbit_store::workflow::legacy_state::ImportReport::skipped()
            }
            Err(error) => return Err(error),
        }
    };
    if import_report.skipped_records() {
        tracing::warn!(
            workspace_id = %workspace_id,
            audit_events_skipped = import_report.audit_events_skipped,
            "skipped malformed legacy state records during SQLite import",
        );
    }
    let semantic_index = SemanticIndex::open(&persistence.semantic_db)?;
    // The worker writes embeddings. Without an index to write into there is
    // nothing for it to drain, so it stays disabled alongside the short-lived
    // hosts that refresh through `orbit semantic index` instead.
    let semantic_worker = match (host_lifetime, semantic_index.store()) {
        (HostLifetime::LongLived, Ok(vector)) => Arc::new(EmbedWorker::start(vector.clone())),
        _ => Arc::new(EmbedWorker::disabled()),
    };
    let job_run_store = workspace_job_run_store(store.clone(), workspace_id);

    // Executors and policies are global-only. Jobs always persist run state
    // under the workspace state directory.
    let tool_store = tool_store_sqlite(store.clone());
    let audit_event_store = audit_event_store_sqlite(store.clone());
    let task_reservation_store = task_reservation_store_sqlite(store.clone());
    let host_store = OrbitHostStore {
        sqlite: store.clone(),
        automation: automation_store(store.clone())?,
        review: review_store(store.clone())?,
        operation: operation_store(store.clone())?,
        v2_audit: v2_audit_store_from_store(store.clone()),
        invocation: invocation_store_from_store(store.clone()),
    };
    let executor_def_store = global_executor_def_store(persistence.executor_dir.clone());
    let global_policy_store = global_policy_def_store(persistence.policy_dir.clone());
    let workspace_policy_store = workspace_policy_def_store(paths.policies_dir.clone());
    let policy_def_store = layered_policy_def_store(workspace_policy_store, global_policy_store);
    let active_policy = policy_def_store
        .get_policy_def(DEFAULT_POLICY_NAME)?
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "default policy `{DEFAULT_POLICY_NAME}` was not found after seeding"
            ))
        })?;

    let skill_catalog =
        SkillCatalog::layered(persistence.skill_dir.clone(), global_root.join("skills"));
    if let Err(error) = skill_catalog.ensure_layout() {
        if error.is_readonly_or_access_failure() {
            tracing::warn!(
                target: "orbit.core.bootstrap",
                root = %global_root.display(),
                error = %error,
                "skipped incidental skill-catalog layout persistence"
            );
        } else {
            return Err(error);
        }
    }

    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    load_external_tools(&store, &mut registry)?;

    let execution_env_policy = runtime_config.execution_env.clone();
    let codex_execution_policy = runtime_config.codex_execution.clone();
    let persistence = runtime_config.persistence.clone();
    let actor = ActorIdentity::from_env();
    let scoring_enabled = runtime_config.scoring_enabled;
    // Config owns PR settings as plain data; Core is the composition layer
    // that translates them into the execution engine's shape.
    let pr_config = PrConfig {
        task_url_template: runtime_config.pr.task_url_template.clone(),
    };
    let workflow_base_branch = runtime_config.workflow_base_branch.clone();
    let workflow_auto_ship = runtime_config.workflow_auto_ship;
    let routines_source = runtime_config.routines_source;
    let crews = runtime_config.crews.clone();
    let default_crew = runtime_config.default_crew.clone();
    let system_crew = runtime_config.system_crew.clone();
    let operation = runtime_config.operation.clone();

    Ok(OrbitContext::new(
        paths,
        OrbitStores::new(
            task_backends.task,
            task_backends.document,
            task_backends.history,
            task_backends.artifact,
            semantic_index,
            semantic_worker,
            task_reservation_store,
            job_run_store,
            tool_store,
            audit_event_store,
            executor_def_store,
            policy_def_store,
            host_store,
        ),
        OrbitExecutionAssets::new(Arc::new(registry), skill_catalog),
        OrbitPolicyContext::new(
            PolicyEngine::from_def(&active_policy)?,
            execution_env_policy,
            codex_execution_policy,
        ),
        OrbitRuntimeSettings::new(
            persistence,
            actor,
            scoring_enabled,
            pr_config,
            workflow_base_branch,
            workflow_auto_ship,
            routines_source,
            crews,
            default_crew,
            runtime_config.complexity_crews.clone(),
            system_crew,
            operation,
        ),
    ))
}

fn build_v2_task_backends(
    global_root: &Path,
    paths: &WorkspacePaths,
    runtime_binding: Option<&WorkspaceRuntimeBinding>,
) -> Result<WorkspaceTaskBackends, OrbitError> {
    let registry = TaskRegistryStore::open(&task_registry_path(global_root))?;
    let config = read_workspace_config_optional(&paths.orbit_dir)?;
    // Several registered repositories may intentionally share one explicit
    // Orbit root. That root has only one compatibility config.yaml and cannot
    // represent every selected workspace. Workspace init has already created
    // each logical task-registry partition, so route directly by the selected
    // registry identity and omit a checkout-local projection that would itself
    // be shared by every repository.
    if is_explicit_data_dir(global_root, &paths.orbit_dir)
        && let Some(binding) = runtime_binding
    {
        ensure_explicit_root_task_binding(&registry, paths, binding)?;
        return Ok(coordination_task_backends(
            registry,
            binding.logical_workspace_id.clone(),
        ));
    }
    let workspace_id_hint = runtime_binding.map(|binding| binding.workspace_id.as_str());
    // An explicit `--root` data directory is not a checkout. Binding
    // `parent(data-dir)` as `repo_root` mints a synthetic workspace (e.g.
    // `tmp-XXXXXX` for `/tmp`) that later `workspace init --force` cannot
    // reclaim. Skip that mint unless a selected checkout supplied a hint.
    if workspace_id_hint.is_none() && is_explicit_data_dir(global_root, &paths.orbit_dir) {
        let workspace_id = config
            .as_ref()
            .map(|config| config.workspace_id.clone())
            .unwrap_or_else(|| UNBOUND_DATA_DIR_WORKSPACE_ID.to_string());
        return Ok(coordination_task_backends(registry, workspace_id));
    }
    let configured_id = config.as_ref().map(|config| config.workspace_id.as_str());
    if let (Some(hint), Some(configured)) = (workspace_id_hint, configured_id)
        && configured != hint
    {
        return Err(OrbitError::WorkspaceError(format!(
            "workspace binding id '{hint}' does not match configured workspace id '{configured}'"
        )));
    }
    // A checkout whose `config.yaml` identity drifted from the registry row for
    // its orbit dir still keeps its task state under the bound id. Adopt that
    // row instead of asking `bind_workspace` for the drifted id, which fails
    // closed and takes every command in the checkout down (ORB-10985). The
    // config write below reconciles the checkout identity back onto it.
    let workspace_id = match registry.find_checkout_by_orbit_dir(&paths.orbit_dir)? {
        Some(bound) => {
            if configured_id.is_some_and(|configured| configured != bound.workspace_id) {
                tracing::warn!(
                    target: "orbit.core.bootstrap",
                    orbit_dir = %paths.orbit_dir.display(),
                    bound_workspace_id = %bound.workspace_id,
                    configured_workspace_id = configured_id,
                    "checkout identity diverged from its task-registry binding; adopting the bound workspace"
                );
            }
            Some(bound.workspace_id)
        }
        None => match configured_id.or(workspace_id_hint) {
            Some(id) => Some(id.to_string()),
            None => rebind_candidate_workspace_id(&registry, paths)?,
        },
    };
    let binding = registry.bind_workspace(BindWorkspaceParams {
        workspace_id,
        slug: workspace_slug(&paths.repo_root),
        repo_root: paths.repo_root.clone(),
        workspace_path: paths.repo_root.clone(),
        orbit_dir: paths.orbit_dir.clone(),
        repo_fingerprint: None,
    })?;
    if config
        .as_ref()
        .is_none_or(|config| config.workspace_id != binding.workspace_id)
        && let Err(error) = write_workspace_config(
            &paths.orbit_dir,
            &WorkspaceConfig {
                schema_version: 1,
                workspace_id: binding.workspace_id.clone(),
            },
        )
    {
        // Reads must still work from a read-only mount; the identity file is
        // reconciled the next time the checkout is writable.
        if error.is_readonly_or_access_failure() {
            tracing::warn!(
                target: "orbit.core.bootstrap",
                orbit_dir = %paths.orbit_dir.display(),
                error = %error,
                "skipped incidental workspace-identity reconciliation"
            );
        } else {
            return Err(error);
        }
    }

    Ok(workspace_task_backends(registry, binding.workspace_id))
}

/// Recreate the selected checkout's task-registry binding after its index was
/// lost. An explicit root has no checkout-local bootstrap path, so the
/// registry binding supplied by the workspace catalog is the authoritative
/// identity to restore.
fn ensure_explicit_root_task_binding(
    registry: &TaskRegistryStore,
    paths: &WorkspacePaths,
    binding: &WorkspaceRuntimeBinding,
) -> Result<(), OrbitError> {
    if registry
        .find_workspace_binding(&binding.logical_workspace_id)?
        .is_some()
    {
        return Ok(());
    }

    registry.bind_workspace(BindWorkspaceParams {
        workspace_id: Some(binding.logical_workspace_id.clone()),
        slug: workspace_slug(&binding.repo_root),
        repo_root: binding.repo_root.clone(),
        workspace_path: binding.repo_root.clone(),
        orbit_dir: paths.orbit_dir.clone(),
        repo_fingerprint: None,
    })?;
    Ok(())
}

fn rebind_candidate_workspace_id(
    registry: &TaskRegistryStore,
    paths: &WorkspacePaths,
) -> Result<Option<String>, OrbitError> {
    let candidates =
        registry.find_rebind_candidates(&paths.repo_root, &paths.repo_root, &paths.orbit_dir)?;
    match candidates.as_slice() {
        [] => Ok(None),
        [candidate] => Ok(Some(candidate.workspace_id.clone())),
        _ => Err(OrbitError::WorkspaceError(format!(
            "workspace config is missing and multiple task artifact bindings match '{}'; restore .orbit/config.yaml or choose a workspace binding",
            paths.orbit_dir.display()
        ))),
    }
}

fn is_explicit_data_dir(global_root: &Path, orbit_dir: &Path) -> bool {
    if global_root == orbit_dir {
        return true;
    }
    match (
        std::fs::canonicalize(global_root),
        std::fs::canonicalize(orbit_dir),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn workspace_slug(repo_root: &Path) -> String {
    repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace")
        .to_string()
}

pub(crate) type TempDir = tempfile::TempDir;

fn load_external_tools(store: &Store, registry: &mut ToolRegistry) -> Result<(), OrbitError> {
    let stored_tools = store.list_tools()?;
    for tool in stored_tools {
        if !tool.builtin && tool.enabled && !registry.has(&tool.name) {
            registry.register(ExternalTool {
                name: tool.name,
                path: tool.path,
                description: tool.description,
                parameters: tool.parameters,
            });
        }
    }
    Ok(())
}
