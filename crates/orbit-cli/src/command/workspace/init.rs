use std::path::{Path, PathBuf};

use chrono::Utc;
use clap::Args;
use orbit_cmd::agent_rules::{InjectionAction, InjectionOutcome, inject_agent_rules};
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_common::fs::io::{atomic_write_bytes, atomic_write_text};
use orbit_core::bootstrap::init::{InitOptions, init_workspace_at_root};
use orbit_core::{
    OrbitError, RoutineNameCollision, RoutineSeedIdentity, default_routine_name_collisions,
};
use orbit_registry::workspace_registry;
use orbit_registry::{HostIdentityState, inspect_host_identity};
use orbit_types::identity::validate_machine_id;
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::command::init::agent_detect::{RealAgentEnvProbe, detect};
use crate::command::init::config_seed_from_detection;

use super::role::CliCheckoutRole;
use super::support::{
    detect_git_remote, dir_name_or_fallback, ensure_orbit_gitignore_entry,
    manages_checkout_local_orbit_files,
};
use crate::command::{CommandOut, Payload};

#[derive(Args)]
pub struct WorkspaceInitArgs {
    /// Workspace name (defaults to directory name)
    #[arg(long)]
    pub name: Option<String>,
    /// Base branch for this workspace (default: the checked-out branch, or main)
    ///
    /// Kept optional so re-initializing an existing workspace can distinguish
    /// an omitted value from an explicit request to reset it to `main`.
    #[arg(long)]
    pub base_branch: Option<String>,
    /// Ship-pipeline mode for this workspace: `pr` or `local`. When omitted, the
    /// effective mode defaults to `pr`; pass `--ship-mode local` for in-place delivery.
    #[arg(long, value_name = "MODE")]
    pub ship_mode: Option<String>,
    /// Explicit local checkout role. Omit for the compatible local-owner
    /// default; use `replica --owner hm_...` to bootstrap a replica atomically.
    #[arg(long, value_enum)]
    pub role: Option<CliCheckoutRole>,
    /// Stable owner machine_id. Required with `--role replica` and rejected
    /// for the local-owner role.
    #[arg(long)]
    pub owner: Option<String>,
    /// Seed the local task-id allocator so the next task id is N (e.g. hand this
    /// machine a disjoint id range like 10000+). The counter only moves forward;
    /// a value below the current position is refused.
    #[arg(long, value_name = "N")]
    pub task_id_start: Option<u32>,
    /// Set up MCP client integrations for auto-detected providers. The
    /// registered server is granted OPERATOR authority: governed operations
    /// such as `orbit.workflow.ship`, workflow run observation/resume, and
    /// `orbit.command.exec` become reachable through it. Bare `orbit mcp
    /// serve` and worker/agent MCP startup remain agent-only.
    #[arg(long)]
    pub mcp: bool,
    /// Inject (or refresh) an Orbit workflow-rules block in CLAUDE.md and AGENTS.md at the workspace root.
    #[arg(long)]
    pub inject_agent_rules: bool,
    /// No-op (kept for backwards compatibility — defaults are always refreshed on init)
    #[arg(long, hide = true)]
    pub refresh_defaults: bool,
    /// Reconcile an already registered workspace after validating its logical
    /// and checkout binding. A missing or malformed identity is restored only
    /// for that exact binding; malformed bytes are archived first. Also
    /// replaces a checkout identity that no registration claims.
    #[arg(long)]
    pub force: bool,
}

pub(crate) const ONBOARDING_FINALIZE_GUIDANCE: &str = "review and commit generated definitions (.gitignore, .orbit/auto_tasks, .orbit/routines) before local workflows (Orbit does not auto-commit or discard operator changes)";
const RELOCATED_ROOT_ONBOARDING_GUIDANCE: &str = "review generated Orbit definitions in the configured Orbit root before local workflows (Orbit does not auto-commit or discard operator changes)";

pub(crate) fn onboarding_finalize_guidance(
    workspace_root: &Path,
    orbit_dir: &Path,
) -> &'static str {
    if manages_checkout_local_orbit_files(workspace_root, orbit_dir) {
        ONBOARDING_FINALIZE_GUIDANCE
    } else {
        RELOCATED_ROOT_ONBOARDING_GUIDANCE
    }
}

impl WorkspaceInitArgs {
    pub fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        let cwd = std::env::current_dir().map_err(|e| OrbitError::Io(e.to_string()))?;
        let roots = RegisteredRuntimeFactory::resolve_bootstrap_roots_for_cwd(&cwd, root_override)?;
        let orbit_dir = roots.shared_root;
        let global_root = roots.global_root;

        // Registry path validation canonicalizes its parent before reading or
        // locking the registry. Create a fresh global root here so first-time
        // workspace initialization can reach that validation step.
        std::fs::create_dir_all(&global_root).map_err(|error| {
            OrbitError::Io(format!(
                "create global Orbit root '{}': {error}",
                global_root.display()
            ))
        })?;

        let registry_path = workspace_registry::registry_path_for(&global_root);
        let mcp = self.mcp;
        let inject_rules = self.inject_agent_rules;
        let task_id_start = self.task_id_start;

        let init_result = self.execute_at_path(&cwd, &orbit_dir, &global_root, &registry_path)?;
        let report =
            collect_init_report(init_result, &global_root, task_id_start, mcp, inject_rules)?;

        Ok(Payload::detail(workspace_init_json(&report), format_workspace_init(&report)).into())
    }

    fn execute_at_path(
        self,
        cwd: &Path,
        orbit_dir: &Path,
        global_root: &Path,
        registry_path: &Path,
    ) -> Result<WorkspaceInitResult, OrbitError> {
        // Validate before bootstrapping any workspace state so invalid modes
        // fail closed at the command boundary.
        if let Some(mode) = self.ship_mode.as_deref() {
            orbit_core::ShipMode::parse(mode)?;
        }
        let (local_machine_id, local_host_id, task_prefix) =
            match inspect_host_identity(global_root)? {
                HostIdentityState::Present(identity) => (
                    Some(identity.machine_id),
                    Some(identity.host_id),
                    Some(identity.task_prefix),
                ),
                HostIdentityState::Legacy { .. } | HostIdentityState::Absent => (None, None, None),
            };
        let explicit_role = self.role.map(WorkspaceCheckoutRole::from);
        match (explicit_role, self.owner.as_deref()) {
            (None, Some(_)) => {
                return Err(OrbitError::InvalidInput(
                    "--owner requires `--role replica`".to_string(),
                ));
            }
            (Some(WorkspaceCheckoutRole::Owner), Some(_)) => {
                return Err(OrbitError::InvalidInput(
                    "--role owner does not take --owner".to_string(),
                ));
            }
            (Some(WorkspaceCheckoutRole::Replica), None) => {
                return Err(OrbitError::InvalidInput(
                    "--role replica requires --owner <machine_id>".to_string(),
                ));
            }
            (Some(WorkspaceCheckoutRole::Replica), Some(owner)) => {
                validate_machine_id(owner)?;
                if local_machine_id.as_deref() == Some(owner) {
                    return Err(OrbitError::InvalidInput(format!(
                        "--role replica owner '{owner}' is this local machine; declare owner role instead"
                    )));
                }
            }
            _ => {}
        }

        let name = self.name.unwrap_or_else(|| dir_name_or_fallback(cwd));
        let id = canonical_workspace_id(&name);
        // Seeded routine names are suffixed with the registered workspace name,
        // not the checkout directory, so two checkouts sharing a basename stay
        // distinct on one host [ORB-12107]. Validate the name before any write.
        // The definitions themselves are machine-independent [ORB-12236]; an
        // uninitialized host still seeds none, because `orbit init` owns the
        // host state the clock evaluates them against.
        let routine_identity = local_host_id
            .is_some()
            .then(|| RoutineSeedIdentity::new(&name))
            .transpose()?;
        let git_remote = detect_git_remote(cwd);
        let default_base_branch = checked_out_branch(cwd);
        // Every read of the registry below feeds the write at the end; the lock
        // keeps a concurrent sweep or init from saving over this registration.
        let (reconciling_existing, registered_shared_root) =
            workspace_registry::with_registry_lock(registry_path, || {
                let mut registry = workspace_registry::load_registry_from(registry_path)?;
                let existing_workspace = registry
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == id);
                let existing_checkout = registry
                    .checkouts
                    .iter()
                    .find(|checkout| checkout.repo_root == cwd);
                let reconciling_existing =
                    existing_workspace.is_some() || existing_checkout.is_some();
                let registered_shared_root = global_root == orbit_dir
                    && registry
                        .checkouts
                        .iter()
                        .any(|checkout| checkout.orbit_dir == orbit_dir);
                if registered_shared_root {
                    validate_shared_root_identity(orbit_dir)?;
                }

                if reconciling_existing && !self.force {
                    return Err(OrbitError::WorkspaceError(format!(
                        "workspace registration already exists for '{}' or '{}'; rerun with --force to reconcile it",
                        id,
                        cwd.display()
                    )));
                }

                let mut identity_recovery = None;
                if reconciling_existing {
                    validate_existing_registration(
                        existing_workspace,
                        existing_checkout,
                        cwd,
                        orbit_dir,
                        &id,
                    )?;
                    if !registered_shared_root {
                        identity_recovery = validate_or_recover_workspace_identity(orbit_dir, &id)?;
                    }
                } else if !registered_shared_root
                    && let Some(identity) = read_workspace_identity(orbit_dir)?
                    && identity.workspace_id != id
                {
                    // A checkout can carry an identity the registry never recorded:
                    // any command that opens a runtime in an uninitialized checkout
                    // seeds a bootstrap id. Replacing one is explicit reconciliation,
                    // so it needs --force — but --force must not detach an identity a
                    // durable registration still claims.
                    if !self.force {
                        return Err(OrbitError::WorkspaceError(format!(
                            "workspace identity '{}' at '{}' conflicts with requested workspace '{}'; rerun with --force to reconcile it",
                            identity.workspace_id,
                            orbit_dir.join("config.yaml").display(),
                            id
                        )));
                    }
                    if registry_claims(&registry, &identity.workspace_id) {
                        return Err(OrbitError::WorkspaceError(format!(
                            "cannot reconcile workspace '{}': checkout identity '{}' at '{}' is claimed by an existing registration",
                            id,
                            identity.workspace_id,
                            orbit_dir.join("config.yaml").display()
                        )));
                    }
                }

                if let Some(identity) = routine_identity.as_ref() {
                    reject_colliding_routine_names(&registry, &id, orbit_dir, identity, &name)?;
                }

                init_workspace_at_root(
                    orbit_dir,
                    InitOptions {
                        refresh_defaults: true,
                        global_root_override: Some(global_root.to_path_buf()),
                        routine_seed_identity: routine_identity.clone(),
                        // Host detection is a CLI concern: Core seeds config from the
                        // families this adapter reports, never by probing PATH itself.
                        config_seed: Some(config_seed_from_detection(&detect(&RealAgentEnvProbe))),
                        ..Default::default()
                    },
                )?;
                ensure_orbit_gitignore_entry(cwd, orbit_dir)?;
                let mut checkout_added = false;
                if let Some(existing) = registry.workspaces.iter_mut().find(|w| w.id == id) {
                    if let Some(ship_mode) = self.ship_mode {
                        existing.ship_mode = Some(ship_mode);
                    }
                    if let Some(base_branch) = self.base_branch {
                        existing.base_branch = base_branch;
                    }
                    existing.updated_at = Utc::now();
                    if let Some(checkout) = registry
                        .checkouts
                        .iter_mut()
                        .find(|checkout| checkout.workspace_id == id)
                    {
                        checkout.repo_root = cwd.to_path_buf();
                        checkout.orbit_dir = orbit_dir.to_path_buf();
                    } else {
                        workspace_registry::register_checkout(
                            &mut registry,
                            unassigned_checkout(&id, cwd, orbit_dir),
                        )?;
                        checkout_added = true;
                    }
                } else {
                    let now = Utc::now();
                    let ws = Workspace {
                        id: id.clone(),
                        name: name.clone(),
                        // The explicit role assignment below writes owner identity
                        // and checkout role together before this registry is saved.
                        owner_machine_id: None,
                        git_remote,
                        ship_mode: self.ship_mode,
                        base_branch: self.base_branch.unwrap_or(default_base_branch),
                        status: WorkspaceStatus::Active,
                        created_at: now,
                        updated_at: now,
                    };
                    workspace_registry::register_workspace(&mut registry, ws)?;
                    workspace_registry::register_checkout(
                        &mut registry,
                        unassigned_checkout(&id, cwd, orbit_dir),
                    )?;
                    checkout_added = true;
                }

                // A new checkout defaults compatibly to the local owner. An explicit
                // replica declaration supplies its stable owner in this same in-memory
                // mutation, so no transient local-owner binding is ever persisted.
                if checkout_added || explicit_role.is_some() {
                    let assigned_role = explicit_role.unwrap_or(WorkspaceCheckoutRole::Owner);
                    workspace_registry::assign_checkout_role(
                        &mut registry,
                        &id,
                        assigned_role,
                        self.owner.as_deref(),
                        local_machine_id.as_deref(),
                    )?;
                    match assigned_role {
                        WorkspaceCheckoutRole::Owner => {
                            if let (Some(machine_id), Some(host_id)) =
                                (local_machine_id.as_deref(), local_host_id.as_deref())
                            {
                                workspace_registry::rename_local_owner_host_id(
                                    &mut registry,
                                    machine_id,
                                    host_id,
                                )?;
                            }
                        }
                        WorkspaceCheckoutRole::Replica => {
                            // v1 has no fleet lookup from stable machine id to display
                            // name. Until the local record is enriched with a human
                            // name, the explicit owner id is itself recognizable to
                            // routine-pin diagnostics as a known-elsewhere owner.
                            if let Some(owner) = self.owner.as_deref() {
                                registry
                                    .owner_host_ids
                                    .entry(owner.to_string())
                                    .or_insert_with(|| owner.to_string());
                            }
                        }
                    }
                }
                orbit_core::adapter::HubCoordinationExecutor::register_workspace(
                    global_root,
                    &id,
                    &name,
                )?;
                // A first checkout for this data dir must land in sqlite before the
                // JSON catalog is saved. Shared-root follow-on checkouts reuse one
                // orbit_dir (UNIQUE) and must not steal that row. `--force` rebinds
                // a leftover synthetic parent(data-dir) mint.
                if !registered_shared_root {
                    orbit_core::adapter::HubCoordinationExecutor::bind_checkout(
                        global_root,
                        &id,
                        &name,
                        cwd,
                        orbit_dir,
                        self.force,
                    )?;
                }
                workspace_registry::save_registry_to(&registry, registry_path)?;
                if let Some(recovery) = identity_recovery {
                    preserve_corrupt_workspace_identity(orbit_dir, &recovery)?;
                    write_workspace_identity(orbit_dir, &id)?;
                }
                Ok((reconciling_existing, registered_shared_root))
            })?;
        if !reconciling_existing && !registered_shared_root {
            write_workspace_identity(orbit_dir, &id)?;
        }

        Ok(WorkspaceInitResult {
            id,
            name,
            root: cwd.to_path_buf(),
            orbit_dir: orbit_dir.to_path_buf(),
            task_prefix,
        })
    }
}

/// Returns the current local branch for a newly registered checkout.
///
/// An explicit `--base-branch` always wins. Repositories without a checked-out
/// branch retain the long-standing `main` fallback.
pub(crate) fn checked_out_branch(cwd: &Path) -> String {
    let output = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output();

    output
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|branch| branch.trim().to_string())
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "main".to_string())
}

pub(super) fn render_task_id_start(task_prefix: Option<&str>, next: u32) -> String {
    match task_prefix {
        Some(task_prefix) => format!("{task_prefix}-{next:05}"),
        None => format!("{next:05}"),
    }
}

/// Refuse to seed routines whose names another registered workspace on this
/// host already declares.
///
/// Routine discovery drops *every* definition sharing a name, so a silent
/// duplicate would disable the colliding workspace's routines too. Checkouts
/// of the workspace being initialized are excluded: re-initializing rebinds
/// them rather than adding a second source [ORB-12107].
fn reject_colliding_routine_names(
    registry: &WorkspaceRegistry,
    workspace_id: &str,
    orbit_dir: &Path,
    identity: &RoutineSeedIdentity,
    name: &str,
) -> Result<(), OrbitError> {
    let other_orbit_dirs: Vec<PathBuf> = registry
        .checkouts
        .iter()
        .filter(|checkout| checkout.workspace_id != workspace_id && checkout.orbit_dir != orbit_dir)
        .map(|checkout| checkout.orbit_dir.clone())
        .collect();

    let collisions = default_routine_name_collisions(identity, &other_orbit_dirs);
    if collisions.is_empty() {
        return Ok(());
    }

    Err(OrbitError::WorkspaceError(format!(
        "workspace '{name}' would seed routine names another workspace on this host already \
         defines ({}); routine names must be unique across every routine source on a host, so \
         rerun `orbit workspace init --name <other-name>`",
        describe_routine_collisions(&collisions)
    )))
}

fn describe_routine_collisions(collisions: &[RoutineNameCollision]) -> String {
    collisions
        .iter()
        .map(|collision| {
            format!(
                "'{}' at {}",
                collision.name,
                collision.declared_in.display()
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn canonical_workspace_id(name: &str) -> String {
    let mut canonical = String::new();
    let mut separator = false;
    for character in name.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() || character == '_' {
            canonical.push(character);
            separator = false;
        } else if !canonical.is_empty() {
            separator = true;
        }
        if separator && !canonical.ends_with('-') {
            canonical.push('-');
            separator = false;
        }
    }
    while canonical.ends_with('-') {
        canonical.pop();
    }
    if canonical.is_empty() {
        canonical.push_str("workspace");
    }
    format!("ws_{canonical}")
}

#[derive(Serialize)]
struct WorkspaceIdentityDocument<'a> {
    schema_version: u32,
    workspace_id: &'a str,
}

#[derive(Deserialize)]
struct StoredWorkspaceIdentity {
    schema_version: u32,
    workspace_id: String,
}

enum WorkspaceIdentityRecovery {
    Missing,
    Corrupt(Vec<u8>),
}

fn validate_existing_registration(
    workspace: Option<&Workspace>,
    checkout: Option<&WorkspaceCheckout>,
    cwd: &Path,
    orbit_dir: &Path,
    workspace_id: &str,
) -> Result<(), OrbitError> {
    let workspace = workspace.ok_or_else(|| {
        OrbitError::WorkspaceError(format!(
            "cannot reconcile workspace '{workspace_id}': the target checkout is bound to a different durable workspace"
        ))
    })?;
    let checkout = checkout.ok_or_else(|| {
        OrbitError::WorkspaceError(format!(
            "cannot reconcile workspace '{workspace_id}': its durable record has no target checkout binding"
        ))
    })?;
    if checkout.workspace_id != workspace.id
        || checkout.repo_root != cwd
        || checkout.orbit_dir != orbit_dir
    {
        return Err(OrbitError::WorkspaceError(format!(
            "cannot reconcile workspace '{workspace_id}': logical record and checkout binding do not match the requested checkout"
        )));
    }
    Ok(())
}

/// True when either registry authority still binds `workspace_id`.
fn registry_claims(registry: &WorkspaceRegistry, workspace_id: &str) -> bool {
    registry
        .workspaces
        .iter()
        .any(|workspace| workspace.id == workspace_id)
        || registry
            .checkouts
            .iter()
            .any(|checkout| checkout.workspace_id == workspace_id)
}

fn read_workspace_identity(
    orbit_dir: &Path,
) -> Result<Option<StoredWorkspaceIdentity>, OrbitError> {
    let path = orbit_dir.join("config.yaml");
    if !path.exists() {
        return Ok(None);
    }
    let content =
        std::fs::read_to_string(&path).map_err(|error| OrbitError::Io(error.to_string()))?;
    let identity = serde_yaml::from_str(&content).map_err(|error| {
        OrbitError::WorkspaceError(format!(
            "invalid workspace identity '{}': {error}",
            path.display()
        ))
    })?;
    Ok(Some(identity))
}

/// A registered checkout may recover an identity that carries no competing
/// claim. The registry binding is validated before this function is called;
/// a parseable different workspace id remains an ownership conflict.
fn validate_or_recover_workspace_identity(
    orbit_dir: &Path,
    workspace_id: &str,
) -> Result<Option<WorkspaceIdentityRecovery>, OrbitError> {
    let path = orbit_dir.join("config.yaml");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(WorkspaceIdentityRecovery::Missing));
        }
        Err(error) => return Err(OrbitError::Io(error.to_string())),
    };
    let identity: StoredWorkspaceIdentity = match serde_yaml::from_slice(&bytes) {
        Ok(identity) => identity,
        Err(_) => return Ok(Some(WorkspaceIdentityRecovery::Corrupt(bytes))),
    };
    if identity.workspace_id.trim().is_empty() {
        return Ok(Some(WorkspaceIdentityRecovery::Corrupt(bytes)));
    }
    if identity.workspace_id != workspace_id {
        return Err(OrbitError::WorkspaceError(format!(
            "cannot reconcile workspace '{workspace_id}': checkout identity '{}' does not match",
            path.display()
        )));
    }
    if identity.schema_version != 1 {
        return Ok(Some(WorkspaceIdentityRecovery::Corrupt(bytes)));
    }
    Ok(None)
}

fn preserve_corrupt_workspace_identity(
    orbit_dir: &Path,
    recovery: &WorkspaceIdentityRecovery,
) -> Result<(), OrbitError> {
    let WorkspaceIdentityRecovery::Corrupt(bytes) = recovery else {
        return Ok(());
    };
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.9fZ");
    let archive = orbit_dir
        .join("state/recovery/workspace-identity")
        .join(format!("config.yaml.{timestamp}.corrupt"));
    atomic_write_bytes(&archive, bytes).map_err(|error| {
        OrbitError::Io(format!(
            "archive corrupt workspace identity '{}' before recovery: {error}",
            archive.display()
        ))
    })
}

fn validate_shared_root_identity(orbit_dir: &Path) -> Result<(), OrbitError> {
    let path = orbit_dir.join("config.yaml");
    let identity = read_workspace_identity(orbit_dir)?.ok_or_else(|| {
        OrbitError::WorkspaceError(format!(
            "cannot use registered shared root: runtime identity '{}' is missing",
            path.display()
        ))
    })?;
    if identity.schema_version != 1 || identity.workspace_id.trim().is_empty() {
        return Err(OrbitError::WorkspaceError(format!(
            "cannot use registered shared root: runtime identity '{}' is invalid",
            path.display()
        )));
    }
    Ok(())
}

fn write_workspace_identity(orbit_dir: &Path, workspace_id: &str) -> Result<(), OrbitError> {
    let content = serde_yaml::to_string(&WorkspaceIdentityDocument {
        schema_version: 1,
        workspace_id,
    })
    .map_err(|error| OrbitError::Store(format!("serialize workspace identity: {error}")))?;
    atomic_write_text(&orbit_dir.join("config.yaml"), &content).map_err(OrbitError::from)
}

fn unassigned_checkout(
    workspace_id: &str,
    repo_root: &Path,
    orbit_dir: &Path,
) -> WorkspaceCheckout {
    WorkspaceCheckout {
        workspace_id: workspace_id.to_string(),
        repo_root: repo_root.to_path_buf(),
        orbit_dir: orbit_dir.to_path_buf(),
        role: None,
        owner_machine_id: None,
        path_overrides: Vec::new(),
    }
}

struct WorkspaceInitResult {
    id: String,
    name: String,
    root: PathBuf,
    orbit_dir: PathBuf,
    task_prefix: Option<String>,
}

struct WorkspaceInitReport {
    id: String,
    name: String,
    root: PathBuf,
    orbit_dir: PathBuf,
    onboarding: &'static str,
    allocator: AllocatorOutcome,
    mcp: McpOutcome,
    rules: RulesOutcome,
}

enum AllocatorOutcome {
    Skipped,
    Ran { next: String, changed: bool },
}

enum McpOutcome {
    Skipped,
    NoneDetected,
    Configured(Vec<String>),
}

enum RulesOutcome {
    Skipped,
    Injected(Vec<RuleFileOutcome>),
}

struct RuleFileOutcome {
    label: String,
    action: InjectionAction,
}

fn collect_init_report(
    init_result: WorkspaceInitResult,
    global_root: &Path,
    task_id_start: Option<u32>,
    mcp: bool,
    inject_rules: bool,
) -> Result<WorkspaceInitReport, OrbitError> {
    let onboarding = onboarding_finalize_guidance(&init_result.root, &init_result.orbit_dir);

    let allocator = match task_id_start {
        Some(start) => {
            let outcome = orbit_core::bootstrap::task_migration::seed_task_id_start(
                global_root,
                init_result.task_prefix.as_deref(),
                start,
            )?;
            AllocatorOutcome::Ran {
                next: render_task_id_start(init_result.task_prefix.as_deref(), outcome.next),
                changed: outcome.changed,
            }
        }
        None => AllocatorOutcome::Skipped,
    };

    let mcp_outcome = if mcp {
        let providers = crate::command::mcp::init_auto_for_workspace(
            &init_result.root,
            &init_result.orbit_dir,
            &init_result.id,
        )?;
        if providers.is_empty() {
            McpOutcome::NoneDetected
        } else {
            McpOutcome::Configured(providers)
        }
    } else {
        McpOutcome::Skipped
    };

    let rules_outcome = if inject_rules {
        let outcome = inject_agent_rules(&init_result.root)?;
        RulesOutcome::Injected(
            outcome
                .outcomes
                .into_iter()
                .map(rule_file_outcome)
                .collect(),
        )
    } else {
        RulesOutcome::Skipped
    };

    Ok(WorkspaceInitReport {
        id: init_result.id,
        name: init_result.name,
        root: init_result.root,
        orbit_dir: init_result.orbit_dir,
        onboarding,
        allocator,
        mcp: mcp_outcome,
        rules: rules_outcome,
    })
}

fn rule_file_outcome(entry: InjectionOutcome) -> RuleFileOutcome {
    RuleFileOutcome {
        label: entry
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| entry.path.display().to_string()),
        action: entry.action,
    }
}

fn workspace_init_json(report: &WorkspaceInitReport) -> Value {
    json!({
        "id": report.id,
        "name": report.name,
        "root": report.root.to_string_lossy(),
        "orbit_dir": report.orbit_dir.to_string_lossy(),
        "onboarding": report.onboarding,
        "allocator": allocator_json(&report.allocator),
        "mcp": mcp_json(&report.mcp),
        "rules": rules_json(&report.rules),
    })
}

fn allocator_json(outcome: &AllocatorOutcome) -> Value {
    match outcome {
        AllocatorOutcome::Skipped => json!({
            "status": "skipped",
            "next": null,
            "changed": null,
        }),
        AllocatorOutcome::Ran { next, changed } => json!({
            "status": if *changed { "seeded" } else { "unchanged" },
            "next": next,
            "changed": changed,
        }),
    }
}

fn mcp_json(outcome: &McpOutcome) -> Value {
    match outcome {
        McpOutcome::Skipped => json!({
            "status": "skipped",
            "providers": null,
        }),
        McpOutcome::NoneDetected => json!({
            "status": "none_detected",
            "providers": [],
        }),
        McpOutcome::Configured(providers) => json!({
            "status": "configured",
            "providers": providers,
        }),
    }
}

fn rules_json(outcome: &RulesOutcome) -> Value {
    match outcome {
        RulesOutcome::Skipped => json!({
            "status": "skipped",
            "outcomes": null,
        }),
        RulesOutcome::Injected(entries) => json!({
            "status": "injected",
            "outcomes": entries
                .iter()
                .map(|entry| json!({
                    "file": entry.label,
                    "action": injection_action_token(&entry.action),
                }))
                .collect::<Vec<_>>(),
        }),
    }
}

fn injection_action_token(action: &InjectionAction) -> &'static str {
    match action {
        InjectionAction::Created => "created",
        InjectionAction::AppendedBlock => "appended",
        InjectionAction::ReplacedBlock => "replaced",
    }
}

fn format_workspace_init(report: &WorkspaceInitReport) -> String {
    let mut lines = vec![
        format!("workspace '{}' initialized", report.name),
        format!("  id:        {}", report.id),
        format!("  root:      {}", report.root.display()),
        format!("  orbit_dir: {}", report.orbit_dir.display()),
        format!("  onboarding: {}", report.onboarding),
    ];
    match &report.allocator {
        AllocatorOutcome::Skipped => {}
        AllocatorOutcome::Ran {
            next,
            changed: true,
        } => lines.push(format!("  id_start:  allocator seeded to {next}")),
        AllocatorOutcome::Ran {
            next,
            changed: false,
        } => lines.push(format!(
            "  id_start:  allocator already at {next} (unchanged)"
        )),
    }
    match &report.mcp {
        McpOutcome::Skipped => {
            lines.push("  mcp:       skipped (pass --mcp to set up integrations)".to_string());
        }
        McpOutcome::NoneDetected => {
            lines.push("  mcp:       no providers auto-detected".to_string());
        }
        McpOutcome::Configured(providers) => lines.push(format!(
            "  mcp:       {} (operator-authorized: orbit.workflow.ship, run observe/resume, orbit.command.exec)",
            providers.join(", ")
        )),
    }
    if let RulesOutcome::Injected(entries) = &report.rules {
        for entry in entries {
            let verb = match entry.action {
                InjectionAction::Created => "created with Orbit rules block",
                InjectionAction::AppendedBlock => "Orbit rules block appended",
                InjectionAction::ReplacedBlock => "Orbit rules block refreshed",
            };
            lines.push(format!("  rules:     {}: {verb}", entry.label));
        }
    }
    lines.join("\n")
}
