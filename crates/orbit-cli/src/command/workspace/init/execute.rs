use std::path::Path;

use chrono::Utc;
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_common::fs::io::atomic_write_text;
use orbit_core::bootstrap::init::{InitOptions, init_workspace_at_root};
use orbit_core::{OrbitError, RoutineSeedIdentity};
use orbit_registry::workspace_registry;
use orbit_registry::{MachineIdentityState, inspect_machine_identity};
use orbit_types::identity::validate_machine_id;
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceStatus,
};
use serde::Serialize;

use crate::command::init::agent_detect::detect;
use crate::command::init::config_seed_from_detection;
use crate::command::{CommandOut, Payload};

use super::super::support::{
    detect_git_remote, dir_name_or_fallback, ensure_orbit_gitignore_entry,
};
use super::WorkspaceInitArgs;
use super::WorkspaceInitResult;
use super::report::{collect_init_report, format_workspace_init, workspace_init_json};
use super::validate::{
    canonical_workspace_id, preserve_corrupt_workspace_identity, read_workspace_identity,
    registry_claims, reject_colliding_routine_names, validate_existing_registration,
    validate_or_recover_workspace_identity, validate_shared_root_identity,
};

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
        let (local_machine_id, task_prefix) = match inspect_machine_identity(global_root)? {
            MachineIdentityState::Present(identity) => {
                (Some(identity.id), Some(identity.task_prefix))
            }
            MachineIdentityState::Absent => (None, None),
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
                validate_machine_id(owner).map_err(|error| {
                    OrbitError::InvalidInput(format!(
                        "--owner '{owner}' is not a usable machine_id ({error}); pass the owner \
                         host's `machine.id` (`orbit config get machine.id` on that host)"
                    ))
                })?;
                if local_machine_id.as_deref() == Some(owner) {
                    return Err(OrbitError::InvalidInput(format!(
                        "--role replica owner '{owner}' is this local machine; `--owner` must name \
                         the other host that owns the workspace (omit --role, or pass `--role \
                         owner`, to register the owner's own checkout)"
                    )));
                }
            }
            _ => {}
        }

        let name = self.name.unwrap_or_else(|| dir_name_or_fallback(cwd));
        let id = canonical_workspace_id(&name);
        let git_remote = detect_git_remote(cwd)?;
        let default_base_branch = checked_out_branch(cwd)?;
        // Every read of the registry below feeds the write at the end; the lock
        // keeps a concurrent sweep or init from saving over this registration.
        let (reconciling_existing, registered_shared_root, checkout_role, owner_machine_id) =
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
                // The delivery defaults are rendered against the base branch
                // this registration will record, so the seeded file and the
                // registry never disagree about which branch is observed.
                let seeded_base_branch = self
                    .base_branch
                    .clone()
                    .or_else(|| existing_workspace.map(|workspace| workspace.base_branch.clone()))
                    .unwrap_or_else(|| default_base_branch.clone());
                // Seeded routine names are suffixed with the registered workspace
                // name, not the checkout directory, so two checkouts sharing a
                // basename stay distinct on one host [ORB-12107]. Validate the
                // name before any write. Cron definitions are machine-independent
                // [ORB-12236]; the state-triggered task-pilot default names this
                // host as its owner and the branch above as what it observes
                // [ORB-12745]. An uninitialized host seeds none, because
                // `orbit init` owns the host state the clock evaluates them against.
                let routine_identity = local_machine_id
                    .as_deref()
                    .map(|machine_id| {
                        RoutineSeedIdentity::new(&name, machine_id, &seeded_base_branch)
                    })
                    .transpose()?;
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
                    // Validate role declarations before bootstrap as well: a
                    // refused owner/replica change must not refresh local state
                    // while attempting to establish a first source identity.
                    if let Some(role) = explicit_role {
                        workspace_registry::assign_checkout_role(
                            &mut registry,
                            &id,
                            role,
                            self.owner.as_deref(),
                            local_machine_id.as_deref(),
                        )?;
                    }
                    workspace_registry::reconcile_workspace_source_remote(
                        &mut registry,
                        &id,
                        git_remote.as_deref(),
                        local_machine_id.as_deref(),
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
                        workspace_base_branch: Some(seeded_base_branch),
                        // Host detection is a CLI concern: Core seeds config from the
                        // families this adapter reports, never by probing PATH itself.
                        config_seed: Some(config_seed_from_detection(&detect())),
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
                if checkout_added || (!reconciling_existing && explicit_role.is_some()) {
                    let assigned_role = explicit_role.unwrap_or(WorkspaceCheckoutRole::Owner);
                    workspace_registry::assign_checkout_role(
                        &mut registry,
                        &id,
                        assigned_role,
                        self.owner.as_deref(),
                        local_machine_id.as_deref(),
                    )?;
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
                let checkout_role = registry
                    .checkouts
                    .iter()
                    .find(|checkout| checkout.workspace_id == id)
                    .and_then(|checkout| checkout.role);
                let owner_machine_id = registry
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == id)
                    .and_then(|workspace| workspace.owner_machine_id.clone());
                workspace_registry::save_registry_to(&registry, registry_path)?;
                if let Some(recovery) = identity_recovery {
                    preserve_corrupt_workspace_identity(orbit_dir, &recovery)?;
                    write_workspace_identity(orbit_dir, &id)?;
                }
                Ok((
                    reconciling_existing,
                    registered_shared_root,
                    checkout_role,
                    owner_machine_id,
                ))
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
            role: checkout_role,
            owner_machine_id,
        })
    }
}

/// Returns the current local branch for a newly registered checkout.
///
/// An explicit `--base-branch` always wins. Repositories without a checked-out
/// branch retain the long-standing `main` fallback. A Git that timed out is an
/// error rather than that fallback.
fn checked_out_branch(cwd: &Path) -> Result<String, OrbitError> {
    let branch = match orbit_common::fs::git::run_git(cwd, &["branch", "--show-current"]) {
        Ok(output) if output.success => output.stdout.trim().to_string(),
        Ok(_) => String::new(),
        Err(error @ OrbitError::ProcessTimeout { .. }) => return Err(error),
        Err(error) => {
            tracing::warn!("cannot read the checked-out branch: {error}");
            String::new()
        }
    };
    Ok(if branch.is_empty() {
        "main".to_string()
    } else {
        branch
    })
}

#[derive(Serialize)]
struct WorkspaceIdentityDocument<'a> {
    schema_version: u32,
    workspace_id: &'a str,
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
