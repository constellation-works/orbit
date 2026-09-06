use std::path::{Path, PathBuf};

use chrono::Utc;
use clap::Args;
use orbit_cmd::agent_rules::{InjectionAction, inject_agent_rules};
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_common::fs::io::{atomic_write_bytes, atomic_write_text};
use orbit_core::OrbitError;
use orbit_core::bootstrap::init::{InitOptions, init_workspace_at_root};
use orbit_registry::workspace_registry;
use orbit_registry::{HostIdentityState, inspect_host_identity};
use orbit_types::identity::validate_machine_id;
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use serde::{Deserialize, Serialize};

use crate::command::init::agent_detect::{RealAgentEnvProbe, detect};
use crate::command::init::config_seed_from_detection;

use super::role::CliCheckoutRole;
use super::support::{detect_git_remote, dir_name_or_fallback, ensure_orbit_gitignore_entry};
use crate::command::{CommandOut, CommandOutput};

#[derive(Args)]
pub struct WorkspaceInitArgs {
    /// Workspace name (defaults to directory name)
    #[arg(long)]
    pub name: Option<String>,
    /// Base branch for this workspace (default: main)
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

impl WorkspaceInitArgs {
    pub fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        let cwd = std::env::current_dir().map_err(|e| OrbitError::Io(e.to_string()))?;
        let roots = RegisteredRuntimeFactory::resolve_bootstrap_roots_for_cwd(&cwd, root_override)?;
        let orbit_dir = roots.shared_root;
        let global_root = roots.global_root;
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let mcp = self.mcp;
        let inject_rules = self.inject_agent_rules;
        let task_id_start = self.task_id_start;
        let init_result = self.execute_at_path(&cwd, &orbit_dir, &global_root, &registry_path)?;

        println!("workspace '{}' initialized", init_result.name);
        println!("  id:        {}", init_result.id);
        println!("  root:      {}", init_result.root.display());
        println!("  orbit_dir: {}", init_result.orbit_dir.display());
        println!("  onboarding: {ONBOARDING_FINALIZE_GUIDANCE}");

        if let Some(start) = task_id_start {
            let outcome =
                orbit_core::bootstrap::task_migration::seed_task_id_start(&global_root, start)?;
            if outcome.changed {
                println!("  id_start:  allocator seeded to ORB-{:05}", outcome.next);
            } else {
                println!(
                    "  id_start:  allocator already at ORB-{:05} (unchanged)",
                    outcome.next
                );
            }
        }

        if mcp {
            let providers = crate::command::mcp::init_auto_for_workspace(
                &init_result.root,
                &init_result.orbit_dir,
                &init_result.id,
            )?;
            if providers.is_empty() {
                println!("  mcp:       no providers auto-detected");
            } else {
                println!(
                    "  mcp:       {} (operator-authorized: orbit.workflow.ship, run observe/resume, orbit.command.exec)",
                    providers.join(", ")
                );
            }
        } else {
            println!("  mcp:       skipped (pass --mcp to set up integrations)");
        }

        if inject_rules {
            let outcome = inject_agent_rules(&init_result.root)?;
            for entry in &outcome.outcomes {
                let label = entry
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| entry.path.display().to_string());
                let verb = match entry.action {
                    InjectionAction::Created => "created with Orbit rules block",
                    InjectionAction::AppendedBlock => "Orbit rules block appended",
                    InjectionAction::ReplacedBlock => "Orbit rules block refreshed",
                };
                println!("  rules:     {label}: {verb}");
            }
        }

        Ok(CommandOutput::Silent)
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
        let (local_machine_id, local_host_id) = match inspect_host_identity(global_root)? {
            HostIdentityState::Present(identity) => {
                (Some(identity.machine_id), Some(identity.host_id))
            }
            HostIdentityState::Legacy { .. } | HostIdentityState::Absent => (None, None),
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
        let git_remote = detect_git_remote(cwd);
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

                init_workspace_at_root(
                    orbit_dir,
                    InitOptions {
                        refresh_defaults: true,
                        global_root_override: Some(global_root.to_path_buf()),
                        routine_host_id: local_host_id.clone(),
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
                        base_branch: self.base_branch.unwrap_or_else(|| "main".to_string()),
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
        })
    }
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
}
