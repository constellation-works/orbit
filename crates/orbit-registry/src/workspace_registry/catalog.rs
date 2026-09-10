//! Versioned logical workspace catalog and machine-local checkout bindings.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::identity::{validate_host_id, validate_machine_id};
use orbit_types::workspace::{
    WORKSPACE_REGISTRY_SCHEMA_VERSION, Workspace, WorkspaceCheckout, WorkspaceCheckoutRole,
    WorkspaceRegistry, WorkspaceStatus, git_remote_identity, redact_git_remote,
    validate_source_repository_fingerprint,
};
use serde::Deserialize;
use serde_json::Value;

/// Registers a new workspace. Errors if a workspace with the same id or name already exists.
pub fn register_workspace(
    registry: &mut WorkspaceRegistry,
    ws: Workspace,
) -> Result<(), OrbitError> {
    if registry.workspaces.iter().any(|w| w.id == ws.id) {
        return Err(OrbitError::WorkspaceError(format!(
            "workspace with id '{}' already exists",
            ws.id
        )));
    }
    if registry.workspaces.iter().any(|w| w.name == ws.name) {
        return Err(OrbitError::WorkspaceError(format!(
            "workspace with name '{}' already exists",
            ws.name
        )));
    }
    registry.workspaces.push(ws);
    Ok(())
}

/// Registers a machine-local checkout for an existing logical workspace.
pub fn register_checkout(
    registry: &mut WorkspaceRegistry,
    checkout: WorkspaceCheckout,
) -> Result<(), OrbitError> {
    if !registry
        .workspaces
        .iter()
        .any(|workspace| workspace.id == checkout.workspace_id)
    {
        return Err(OrbitError::not_found(
            NotFoundKind::Workspace,
            checkout.workspace_id,
        ));
    }
    if registry
        .checkouts
        .iter()
        .any(|existing| existing.workspace_id == checkout.workspace_id)
    {
        return Err(OrbitError::WorkspaceError(format!(
            "local checkout for workspace '{}' already exists",
            checkout.workspace_id
        )));
    }
    if let Some(existing) = registry
        .checkouts
        .iter()
        .find(|existing| existing.repo_root == checkout.repo_root)
    {
        return Err(OrbitError::WorkspaceError(format!(
            "checkout path '{}' is already registered to workspace '{}'",
            checkout.repo_root.display(),
            existing.workspace_id
        )));
    }
    registry.checkouts.push(checkout);
    Ok(())
}

/// The inspected result of explicitly rebinding one workspace's source remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSourceRemoteRebind {
    pub workspace_id: String,
    pub old_remote: String,
    pub old_repository_identity: Option<String>,
    pub new_remote: String,
    pub new_repository_identity: String,
    pub changed: bool,
    pub dry_run: bool,
}

/// Rebind an owned workspace's portable source-repository identity.
///
/// Every refusal happens before the registry is mutated. Publication bindings
/// deliberately fail closed because their source fingerprint is part of the
/// published lineage; the operator must remove and recreate that binding
/// explicitly around a repository move.
pub fn rebind_workspace_source_remote(
    registry: &mut WorkspaceRegistry,
    id_or_name: &str,
    new_remote: &str,
    local_machine_id: Option<&str>,
    dry_run: bool,
) -> Result<WorkspaceSourceRemoteRebind, OrbitError> {
    validate_source_repository_fingerprint(new_remote)
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    let new_repository_identity = git_remote_identity(new_remote)
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;

    let local_machine_id = local_machine_id.ok_or_else(|| {
        OrbitError::WorkspaceError(
            "source-remote rebinding requires a local host identity; run `orbit init` first"
                .to_string(),
        )
    })?;
    validate_machine_id(local_machine_id)?;

    let workspace = find_workspace(registry, id_or_name)?
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, id_or_name.to_string()))?;
    let workspace_id = workspace.id.clone();
    let old_remote = workspace.git_remote.clone().ok_or_else(|| {
        OrbitError::WorkspaceError(format!(
            "workspace '{workspace_id}' has no registered source remote; initialize it from a checkout with an origin before rebinding"
        ))
    })?;
    let old_repository_identity = git_remote_identity(&old_remote).ok();
    let old_remote_is_portable = validate_source_repository_fingerprint(&old_remote).is_ok();

    let checkout = registry
        .checkouts
        .iter()
        .find(|checkout| checkout.workspace_id == workspace_id)
        .ok_or_else(|| {
            OrbitError::WorkspaceError(format!(
                "workspace '{workspace_id}' has no local checkout; source-remote rebinding is owner-local"
            ))
        })?;
    match checkout.role {
        Some(WorkspaceCheckoutRole::Owner) => {}
        Some(WorkspaceCheckoutRole::Replica) => {
            return Err(OrbitError::WorkspaceError(format!(
                "workspace '{workspace_id}' is a replica checkout; run the source-remote rebind on its declared owner machine"
            )));
        }
        None => {
            return Err(OrbitError::WorkspaceError(format!(
                "workspace '{workspace_id}' has no declared local checkout role; reassert its owner role before rebinding the source remote"
            )));
        }
    }
    let owner_machine_id = workspace.owner_machine_id.as_deref().ok_or_else(|| {
        OrbitError::WorkspaceError(format!(
            "workspace '{workspace_id}' has no declared owner; reassert its owner role before rebinding the source remote"
        ))
    })?;
    if owner_machine_id != local_machine_id {
        return Err(OrbitError::WorkspaceError(format!(
            "workspace '{workspace_id}' is owned by machine '{owner_machine_id}'; local machine '{local_machine_id}' cannot rebind its source remote"
        )));
    }

    let changed = !old_remote_is_portable
        || old_repository_identity.as_deref() != Some(&new_repository_identity);
    let outcome = WorkspaceSourceRemoteRebind {
        workspace_id: workspace_id.clone(),
        old_remote,
        old_repository_identity,
        new_remote: new_remote.to_string(),
        new_repository_identity,
        changed,
        dry_run,
    };
    if !changed {
        return Ok(outcome);
    }

    if let Some(binding) = registry
        .publication_bindings
        .iter()
        .find(|binding| binding.workspace_id == workspace_id)
    {
        return Err(OrbitError::WorkspaceError(format!(
            "workspace '{workspace_id}' has publication binding '{}' tied to source '{}'; refusing to rewrite publication lineage. Inspect it with `orbit --workspace {workspace_id} workspace publication show --json`, record its settings, remove it with `orbit --workspace {workspace_id} workspace publication remove --confirm`, perform the source-remote rebind, then create a new binding explicitly",
            binding.publication_id,
            redact_git_remote(&binding.source_repository_fingerprint),
        )));
    }
    if dry_run {
        return Ok(outcome);
    }

    let workspace = registry
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, workspace_id.clone()))?;
    workspace.git_remote = Some(new_remote.to_string());
    workspace.updated_at = Utc::now();

    Ok(outcome)
}

/// Record a machine-local checkout role for an existing logical workspace.
///
/// `Owner` requires no replica owner and leaves the logical owner to be
/// declared explicitly from the validated local `machine_id`. `Replica`
/// requires an explicit non-local `owner_machine_id`, which is mirrored onto
/// both the checkout binding and the logical workspace record so the stable
/// owner identity stays consistent. The optional local identity exists only
/// for pre-host-identity standalone compatibility. This mutates the in-memory
/// registry only; the caller persists via [`super::save_registry_to`], which validates a clone and
/// therefore leaves the previous file byte-valid on any contradiction. Owner
/// and replica declarations are never inferred from paths, workspace names,
/// presence, hostnames, or Git remotes.
pub fn assign_checkout_role(
    registry: &mut WorkspaceRegistry,
    id_or_name: &str,
    role: WorkspaceCheckoutRole,
    owner_machine_id: Option<&str>,
    local_machine_id: Option<&str>,
) -> Result<(), OrbitError> {
    let workspace = find_workspace(registry, id_or_name)?
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, id_or_name.to_string()))?;
    let workspace_id = workspace.id.clone();
    let declared_owner = workspace.owner_machine_id.clone();
    let checkout_index = registry
        .checkouts
        .iter()
        .position(|checkout| checkout.workspace_id == workspace_id)
        .ok_or_else(|| {
            OrbitError::WorkspaceError(format!(
                "workspace '{workspace_id}' has no local checkout binding"
            ))
        })?;

    match role {
        WorkspaceCheckoutRole::Owner => {
            if owner_machine_id.is_some() {
                return Err(OrbitError::WorkspaceError(format!(
                    "workspace '{workspace_id}' owner role does not take an owner machine_id"
                )));
            }
            if let Some(local_machine_id) = local_machine_id {
                validate_machine_id(local_machine_id)?;
                if let Some(existing_owner) = declared_owner.as_deref()
                    && existing_owner != local_machine_id
                {
                    return Err(OrbitError::WorkspaceError(format!(
                        "workspace '{workspace_id}' is declared owned by machine \
                         '{existing_owner}'; local machine '{local_machine_id}' cannot assign \
                         itself the owner role"
                    )));
                }
                if declared_owner.is_none()
                    && let Some(workspace) = registry
                        .workspaces
                        .iter_mut()
                        .find(|workspace| workspace.id == workspace_id)
                {
                    workspace.owner_machine_id = Some(local_machine_id.to_string());
                }
            }
        }
        WorkspaceCheckoutRole::Replica => {
            let owner = owner_machine_id.ok_or_else(|| {
                OrbitError::WorkspaceError(format!(
                    "workspace '{workspace_id}' replica role requires an owner machine_id"
                ))
            })?;
            validate_machine_id(owner)?;
            if local_machine_id == Some(owner) {
                return Err(OrbitError::WorkspaceError(format!(
                    "workspace '{workspace_id}' cannot declare the local machine '{owner}' as \
                     its replica owner"
                )));
            }
            if let Some(existing_owner) = declared_owner.as_deref()
                && existing_owner != owner
            {
                return Err(OrbitError::WorkspaceError(format!(
                    "workspace '{workspace_id}' is already owned by machine '{existing_owner}'; \
                     refusing to rebind it to '{owner}' while assigning a replica role"
                )));
            }
            if let Some(existing_owner) = registry.checkouts[checkout_index]
                .owner_machine_id
                .as_deref()
                && existing_owner != owner
            {
                return Err(OrbitError::WorkspaceError(format!(
                    "workspace '{workspace_id}' checkout already mirrors owner machine \
                     '{existing_owner}'; refusing contradictory owner '{owner}'"
                )));
            }
            if let Some(workspace) = registry
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == workspace_id)
            {
                workspace.owner_machine_id = Some(owner.to_string());
            }
        }
    }

    let checkout = &mut registry.checkouts[checkout_index];
    checkout.role = Some(role);
    checkout.owner_machine_id = match role {
        WorkspaceCheckoutRole::Owner => None,
        WorkspaceCheckoutRole::Replica => owner_machine_id.map(str::to_string),
    };
    Ok(())
}

/// Removes a workspace by id or name. Returns the removed workspace.
pub fn remove_workspace(
    registry: &mut WorkspaceRegistry,
    id_or_name: &str,
) -> Result<Workspace, OrbitError> {
    let workspace_id = find_workspace(registry, id_or_name)?
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, id_or_name.to_string()))?
        .id
        .clone();
    let idx = registry
        .workspaces
        .iter()
        .position(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, id_or_name.to_string()))?;
    let removed = registry.workspaces.remove(idx);
    registry
        .checkouts
        .retain(|checkout| checkout.workspace_id != removed.id);
    registry
        .publication_bindings
        .retain(|binding| binding.workspace_id != removed.id);
    if let Some(owner) = removed.owner_machine_id.as_deref()
        && !registry
            .workspaces
            .iter()
            .any(|workspace| workspace.owner_machine_id.as_deref() == Some(owner))
    {
        registry.owner_host_ids.remove(owner);
    }
    Ok(removed)
}

/// Finds a workspace by id or name, rejecting ambiguous selectors.
pub fn find_workspace<'a>(
    registry: &'a WorkspaceRegistry,
    id_or_name: &str,
) -> Result<Option<&'a Workspace>, OrbitError> {
    match resolve_logical_workspace_match(registry, id_or_name) {
        LogicalWorkspaceMatch::Found(workspace) => Ok(Some(workspace)),
        LogicalWorkspaceMatch::NotFound => Ok(None),
        LogicalWorkspaceMatch::Ambiguous => Err(ambiguous_workspace_selector(id_or_name)),
    }
}

/// Finds a workspace by its exact catalog ID.
///
/// Checkout bindings and other persisted relations already store
/// `workspace_id`. Those lookups must not reuse [`find_workspace`], which
/// treats the argument as an id-or-name selector and fails closed when one
/// workspace's ID equals another's name.
pub fn find_workspace_by_id<'a>(
    registry: &'a WorkspaceRegistry,
    workspace_id: &str,
) -> Option<&'a Workspace> {
    registry
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
}

/// Resolve a logical selector (registered name or `ws_*` id) to exactly one workspace.
///
/// The same fail-closed name/id grammar is used by the CLI `--workspace` flag
/// and MCP `resolve_workspace`. First-match is not
/// enough — two workspaces sharing a name must not silently pick one.
pub fn resolve_logical_workspace<'a>(
    registry: &'a WorkspaceRegistry,
    selector: &str,
) -> Result<&'a Workspace, OrbitError> {
    match resolve_logical_workspace_match(registry, selector) {
        LogicalWorkspaceMatch::Found(workspace) => Ok(workspace),
        LogicalWorkspaceMatch::NotFound => Err(unknown_workspace_selector(selector)),
        LogicalWorkspaceMatch::Ambiguous => Err(ambiguous_workspace_selector(selector)),
    }
}

enum LogicalWorkspaceMatch<'a> {
    Found(&'a Workspace),
    NotFound,
    Ambiguous,
}

fn resolve_logical_workspace_match<'a>(
    registry: &'a WorkspaceRegistry,
    selector: &str,
) -> LogicalWorkspaceMatch<'a> {
    let mut matches = registry
        .workspaces
        .iter()
        .filter(|workspace| workspace.id == selector || workspace.name == selector);
    let Some(workspace) = matches.next() else {
        return LogicalWorkspaceMatch::NotFound;
    };
    if matches.next().is_some() {
        LogicalWorkspaceMatch::Ambiguous
    } else {
        LogicalWorkspaceMatch::Found(workspace)
    }
}

pub(crate) fn unknown_workspace_selector(selector: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "unknown workspace selector '{selector}'; pass a registered workspace name, a logical workspace ID, or an absolute local checkout path"
    ))
}

pub(crate) fn ambiguous_workspace_selector(selector: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "ambiguous workspace selector '{selector}'; it matches more than one registered workspace"
    ))
}

/// Finds the local checkout for a workspace ID or name.
pub fn find_checkout<'a>(
    registry: &'a WorkspaceRegistry,
    id_or_name: &str,
) -> Result<Option<&'a WorkspaceCheckout>, OrbitError> {
    let Some(workspace) = find_workspace(registry, id_or_name)? else {
        return Ok(None);
    };
    Ok(find_checkout_by_id(registry, &workspace.id))
}

/// Finds the local checkout for an exact workspace ID.
///
/// Persisted relations and resolved bindings already store `workspace_id`.
/// Those lookups must not reuse [`find_checkout`], which treats the argument
/// as an id-or-name selector and fails closed when one workspace's ID equals
/// another's name.
pub fn find_checkout_by_id<'a>(
    registry: &'a WorkspaceRegistry,
    workspace_id: &str,
) -> Option<&'a WorkspaceCheckout> {
    registry
        .checkouts
        .iter()
        .find(|checkout| checkout.workspace_id == workspace_id)
}

/// Iterates logical workspaces that have a machine-local checkout binding.
pub fn local_workspaces(
    registry: &WorkspaceRegistry,
) -> impl Iterator<Item = (&Workspace, &WorkspaceCheckout)> {
    registry.checkouts.iter().filter_map(|checkout| {
        find_workspace_by_id(registry, &checkout.workspace_id)
            .map(|workspace| (workspace, checkout))
    })
}

/// Finds the local checkout for a path using longest-prefix matching across
/// its repository root and explicit path overrides.
pub fn find_checkout_by_path<'a>(
    registry: &'a WorkspaceRegistry,
    cwd: &Path,
) -> Option<&'a WorkspaceCheckout> {
    let mut best_match: Option<(&WorkspaceCheckout, usize)> = None;

    for checkout in &registry.checkouts {
        for candidate in std::iter::once(&checkout.repo_root).chain(&checkout.path_overrides) {
            if !cwd.starts_with(candidate) {
                continue;
            }
            let candidate_len = candidate.as_os_str().len();
            if best_match.is_none_or(|(_, current_len)| candidate_len > current_len) {
                best_match = Some((checkout, candidate_len));
            }
        }
    }

    best_match.map(|(checkout, _)| checkout)
}

/// Finds the logical workspace for a path, but only through a machine-local
/// checkout binding. Checkoutless catalog entries can never match a path.
pub fn find_workspace_by_path<'a>(
    registry: &'a WorkspaceRegistry,
    cwd: &Path,
) -> Option<&'a Workspace> {
    let checkout = find_checkout_by_path(registry, cwd)?;
    find_workspace_by_id(registry, &checkout.workspace_id)
}

/// Sets a path override binding a directory to a workspace.
///
/// The path must not already be another checkout's `repo_root` or override.
/// Persistence repeats this uniqueness rule so a hand-edited or CLI-rewritten
/// registry cannot save a collision that would make `find_checkout_by_path`
/// order-dependent.
pub fn set_path_override(
    registry: &mut WorkspaceRegistry,
    path: PathBuf,
    workspace_id: &str,
) -> Result<(), OrbitError> {
    let checkout_index = registry
        .checkouts
        .iter()
        .position(|checkout| checkout.workspace_id == workspace_id)
        .ok_or_else(|| {
            OrbitError::WorkspaceError(format!(
                "workspace '{workspace_id}' has no local checkout binding"
            ))
        })?;
    if let Some(existing) = registry
        .checkouts
        .iter()
        .enumerate()
        .find(|(index, checkout)| {
            *index != checkout_index
                && (checkout.repo_root == path
                    || checkout
                        .path_overrides
                        .iter()
                        .any(|claimed| claimed == &path))
        })
        .map(|(_, checkout)| checkout.workspace_id.as_str())
    {
        return Err(OrbitError::WorkspaceError(format!(
            "checkout path '{}' is already registered to workspace '{existing}'",
            path.display()
        )));
    }
    let checkout = &mut registry.checkouts[checkout_index];
    if !checkout.path_overrides.contains(&path) {
        checkout.path_overrides.push(path);
        checkout.path_overrides.sort();
    }
    Ok(())
}

/// Validates local checkout paths, marking their logical workspace invalid
/// when the local repository root no longer exists. Checkoutless logical
/// workspaces retain their catalog status.
///
/// Returns `true` when any workspace status changed, so callers that only
/// read the registry (e.g. `workspace list`) can skip writing it back.
pub fn validate_workspaces(registry: &mut WorkspaceRegistry) -> bool {
    let now = Utc::now();
    let mut changed = false;
    for ws in &mut registry.workspaces {
        let Some(checkout) = registry
            .checkouts
            .iter()
            .find(|checkout| checkout.workspace_id == ws.id)
        else {
            continue;
        };
        if checkout.repo_root.exists() {
            if ws.status == WorkspaceStatus::Invalid {
                ws.status = WorkspaceStatus::Active;
                ws.updated_at = now;
                changed = true;
            }
        } else if ws.status == WorkspaceStatus::Active {
            ws.status = WorkspaceStatus::Invalid;
            ws.updated_at = now;
            changed = true;
        }
    }
    changed
}

/// Machine identity facts used while validating a local registry file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceRegistryHostContext {
    pub machine_id: Option<String>,
    pub host_id: Option<String>,
}

/// Parse, migrate, and validate one workspace registry JSON document.
pub fn parse_workspace_registry(
    content: &str,
    context: &WorkspaceRegistryHostContext,
) -> Result<(WorkspaceRegistry, bool), OrbitError> {
    let value: Value = serde_json::from_str(content)
        .map_err(|error| invalid_registry(format!("malformed JSON: {error}")))?;
    let Some(version_value) = value.get("schema_version") else {
        let mut migrated = migrate_legacy_registry(value, context)?;
        validate_workspace_registry(&mut migrated, context)?;
        return Ok((migrated, true));
    };
    let version = version_value.as_u64().ok_or_else(|| {
        invalid_registry("schema_version must be a non-negative integer".to_string())
    })?;
    if version > u64::from(WORKSPACE_REGISTRY_SCHEMA_VERSION) {
        return Err(invalid_registry(format!(
            "unsupported schema_version {version}; this build supports up to {WORKSPACE_REGISTRY_SCHEMA_VERSION}. Upgrade Orbit; the file is left unchanged"
        )));
    }
    if version != u64::from(WORKSPACE_REGISTRY_SCHEMA_VERSION) {
        return Err(invalid_registry(format!(
            "invalid schema_version {version}; expected {WORKSPACE_REGISTRY_SCHEMA_VERSION}"
        )));
    }

    validate_role_tokens(&value)?;
    let mut registry: WorkspaceRegistry =
        serde_json::from_value(value).map_err(|error| invalid_registry(error.to_string()))?;
    let changed = validate_workspace_registry(&mut registry, context)?;
    Ok((registry, changed))
}

fn validate_role_tokens(value: &Value) -> Result<(), OrbitError> {
    let Some(checkouts) = value.get("checkouts").and_then(Value::as_array) else {
        return Ok(());
    };
    for checkout in checkouts {
        let workspace_id = checkout
            .get("workspace_id")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let Some(role) = checkout.get("role") else {
            continue;
        };
        match role.as_str() {
            Some("owner" | "replica") => {}
            Some(other) => {
                return Err(invalid_registry(format!(
                    "workspace '{workspace_id}' has unknown checkout role '{other}'"
                )));
            }
            None => {
                return Err(invalid_registry(format!(
                    "workspace '{workspace_id}' has a non-string checkout role"
                )));
            }
        }
    }
    Ok(())
}

/// Validate and canonicalize an in-memory workspace registry.
pub fn validate_workspace_registry(
    registry: &mut WorkspaceRegistry,
    context: &WorkspaceRegistryHostContext,
) -> Result<bool, OrbitError> {
    if registry.schema_version != WORKSPACE_REGISTRY_SCHEMA_VERSION {
        return Err(invalid_registry(format!(
            "schema_version {} is not supported by this build",
            registry.schema_version
        )));
    }

    let mut changed = false;
    let mut workspace_ids = HashSet::new();
    let mut workspace_names = HashSet::new();
    for workspace in &registry.workspaces {
        if !workspace_ids.insert(workspace.id.clone()) {
            return Err(invalid_registry(format!(
                "duplicate workspace id '{}'",
                workspace.id
            )));
        }
        if !workspace_names.insert(workspace.name.clone()) {
            return Err(invalid_registry(format!(
                "duplicate workspace name '{}'",
                workspace.name
            )));
        }
        if let Some(owner_machine_id) = workspace.owner_machine_id.as_deref() {
            validate_machine_id(owner_machine_id).map_err(|error| {
                invalid_registry(format!(
                    "workspace '{}' has invalid owner_machine_id: {error}",
                    workspace.id
                ))
            })?;
        }
    }

    let owner_machine_ids = registry
        .workspaces
        .iter()
        .filter_map(|workspace| workspace.owner_machine_id.as_deref())
        .collect::<HashSet<_>>();
    for (machine_id, host_id) in &registry.owner_host_ids {
        validate_machine_id(machine_id).map_err(|error| {
            invalid_registry(format!("invalid owner host-name machine_id: {error}"))
        })?;
        validate_host_id(host_id).map_err(|error| {
            invalid_registry(format!(
                "owner machine '{machine_id}' has invalid host_id: {error}"
            ))
        })?;
        if !owner_machine_ids.contains(machine_id.as_str()) {
            return Err(invalid_registry(format!(
                "owner host name for machine '{machine_id}' has no local workspace record"
            )));
        }
    }
    if let (Some(machine_id), Some(host_id)) =
        (context.machine_id.as_deref(), context.host_id.as_deref())
        && owner_machine_ids.contains(machine_id)
        && registry.owner_host_ids.get(machine_id).map(String::as_str) != Some(host_id)
    {
        registry
            .owner_host_ids
            .insert(machine_id.to_string(), host_id.to_string());
        changed = true;
    }

    let mut checkout_ids = HashSet::new();
    for checkout in &mut registry.checkouts {
        let workspace = registry
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == checkout.workspace_id)
            .ok_or_else(|| {
                invalid_registry(format!(
                    "checkout references unknown workspace '{}'",
                    checkout.workspace_id
                ))
            })?;
        if !checkout_ids.insert(checkout.workspace_id.clone()) {
            return Err(invalid_registry(format!(
                "workspace '{}' has more than one local checkout binding",
                checkout.workspace_id
            )));
        }

        if checkout.role.is_none() {
            if context.machine_id.is_none() {
                checkout.role = Some(WorkspaceCheckoutRole::Owner);
                changed = true;
            } else {
                return Err(invalid_registry(format!(
                    "workspace '{}' is missing a local checkout role; run `orbit workspace role` to declare owner or replica",
                    checkout.workspace_id
                )));
            }
        }

        match checkout.role {
            Some(WorkspaceCheckoutRole::Owner) => {
                if checkout.owner_machine_id.is_some() {
                    return Err(invalid_registry(format!(
                        "workspace '{}' has contradictory owner role and replica owner_machine_id",
                        checkout.workspace_id
                    )));
                }
                if let Some(machine_id) = context.machine_id.as_deref() {
                    match workspace.owner_machine_id.as_deref() {
                        Some(owner) if owner != machine_id => {
                            return Err(invalid_registry(format!(
                                "workspace '{}' declares local owner role, but logical owner is machine '{owner}' instead of local machine '{machine_id}'",
                                checkout.workspace_id
                            )));
                        }
                        None if context.machine_id.is_none() => {
                            workspace.owner_machine_id = Some(machine_id.to_string());
                            changed = true;
                        }
                        None => {
                            return Err(invalid_registry(format!(
                                "workspace '{}' declares local owner role but has no declared owner_machine_id",
                                checkout.workspace_id
                            )));
                        }
                        Some(_) => {}
                    }
                }
            }
            Some(WorkspaceCheckoutRole::Replica) => {
                if context.machine_id.is_none() {
                    return Err(invalid_registry(format!(
                        "workspace '{}' cannot validate replica role without a local machine_id",
                        checkout.workspace_id
                    )));
                }
                let binding_owner = checkout.owner_machine_id.as_deref().ok_or_else(|| {
                    invalid_registry(format!(
                        "workspace '{}' has replica role without owner_machine_id",
                        checkout.workspace_id
                    ))
                })?;
                validate_machine_id(binding_owner).map_err(|error| {
                    invalid_registry(format!(
                        "workspace '{}' has invalid replica owner_machine_id: {error}",
                        checkout.workspace_id
                    ))
                })?;
                let logical_owner = workspace.owner_machine_id.as_deref().ok_or_else(|| {
                    invalid_registry(format!(
                        "workspace '{}' has replica role but its logical record has no owner_machine_id",
                        checkout.workspace_id
                    ))
                })?;
                if binding_owner != logical_owner {
                    return Err(invalid_registry(format!(
                        "workspace '{}' replica owner '{binding_owner}' contradicts logical owner '{logical_owner}'",
                        checkout.workspace_id
                    )));
                }
                if context.machine_id.as_deref() == Some(binding_owner) {
                    return Err(invalid_registry(format!(
                        "workspace '{}' declares replica role of the local machine '{binding_owner}'",
                        checkout.workspace_id
                    )));
                }
            }
            None => unreachable!("missing checkout role handled above"),
        }

        let before = checkout.path_overrides.len();
        checkout.path_overrides.sort();
        checkout.path_overrides.dedup();
        changed |= checkout.path_overrides.len() != before;
    }

    // A path may belong to only one checkout. `register_checkout` and
    // `set_path_override` refuse the same collision early; this check is the
    // persistence rule so a rewritten `repo_root` or hand-edited override
    // cannot save and then resolve by JSON order.
    let mut claimed_paths: HashMap<&Path, &str> = HashMap::new();
    for checkout in &registry.checkouts {
        claim_checkout_path(
            &mut claimed_paths,
            &checkout.repo_root,
            &checkout.workspace_id,
        )?;
        for override_path in &checkout.path_overrides {
            claim_checkout_path(&mut claimed_paths, override_path, &checkout.workspace_id)?;
        }
    }

    if context.machine_id.is_some() {
        for workspace in &registry.workspaces {
            if workspace.owner_machine_id.is_none() {
                return Err(invalid_registry(format!(
                    "workspace '{}' is missing owner_machine_id",
                    workspace.id
                )));
            }
        }
    }
    super::publication::validate_publication_bindings(registry, context)?;
    Ok(changed)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyWorkspaceRegistry {
    #[serde(default)]
    workspaces: Vec<LegacyWorkspace>,
    #[serde(default)]
    path_overrides: BTreeMap<PathBuf, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyWorkspace {
    id: String,
    name: String,
    root: PathBuf,
    orbit_dir: PathBuf,
    #[serde(default)]
    git_remote: Option<String>,
    #[serde(default)]
    ship_mode: Option<String>,
    #[serde(default = "legacy_default_base_branch")]
    base_branch: String,
    #[serde(default = "legacy_default_status")]
    status: WorkspaceStatus,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn migrate_legacy_registry(
    value: Value,
    context: &WorkspaceRegistryHostContext,
) -> Result<WorkspaceRegistry, OrbitError> {
    let legacy: LegacyWorkspaceRegistry = serde_json::from_value(value)
        .map_err(|error| invalid_registry(format!("invalid legacy registry: {error}")))?;
    let valid_ids: HashSet<String> = legacy
        .workspaces
        .iter()
        .map(|workspace| workspace.id.clone())
        .collect();
    let mut overrides_by_workspace: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for (path, workspace_id) in legacy.path_overrides {
        if valid_ids.contains(&workspace_id) {
            overrides_by_workspace
                .entry(workspace_id)
                .or_default()
                .push(path);
        }
    }

    let mut registry = WorkspaceRegistry::default();
    for legacy_workspace in legacy.workspaces {
        let workspace_id = legacy_workspace.id.clone();
        registry.workspaces.push(Workspace {
            id: legacy_workspace.id,
            name: legacy_workspace.name,
            owner_machine_id: context.machine_id.clone(),
            git_remote: legacy_workspace.git_remote,
            ship_mode: legacy_workspace.ship_mode,
            base_branch: legacy_workspace.base_branch,
            status: legacy_workspace.status,
            created_at: legacy_workspace.created_at,
            updated_at: legacy_workspace.updated_at,
        });
        registry.checkouts.push(WorkspaceCheckout {
            workspace_id: workspace_id.clone(),
            repo_root: legacy_workspace.root,
            orbit_dir: legacy_workspace.orbit_dir,
            // Only installations without a host identity may use the legacy
            // owner default. Identity-bearing machines must declare a role.
            role: context
                .machine_id
                .is_none()
                .then_some(WorkspaceCheckoutRole::Owner),
            owner_machine_id: None,
            path_overrides: overrides_by_workspace
                .remove(&workspace_id)
                .unwrap_or_default(),
        });
    }
    Ok(registry)
}

/// Update the local display name attached to workspace records owned by
/// `machine_id`. Returns the number of logical workspace records affected.
/// Stable owner machine ids and task prefixes are deliberately untouched.
pub fn rename_local_owner_host_id(
    registry: &mut WorkspaceRegistry,
    machine_id: &str,
    new_host_id: &str,
) -> Result<usize, OrbitError> {
    validate_machine_id(machine_id)?;
    validate_host_id(new_host_id)?;
    let affected = registry
        .workspaces
        .iter()
        .filter(|workspace| workspace.owner_machine_id.as_deref() == Some(machine_id))
        .count();
    if affected > 0 {
        registry
            .owner_host_ids
            .insert(machine_id.to_string(), new_host_id.to_string());
    }
    Ok(affected)
}

fn claim_checkout_path<'a>(
    claimed: &mut HashMap<&'a Path, &'a str>,
    path: &'a Path,
    workspace_id: &'a str,
) -> Result<(), OrbitError> {
    match claimed.get(path) {
        Some(existing) if *existing != workspace_id => {
            let (first, second) = if *existing <= workspace_id {
                (*existing, workspace_id)
            } else {
                (workspace_id, *existing)
            };
            Err(invalid_registry(format!(
                "checkout path '{}' is claimed by both '{first}' and '{second}'",
                path.display()
            )))
        }
        Some(_) => Ok(()),
        None => {
            claimed.insert(path, workspace_id);
            Ok(())
        }
    }
}

fn invalid_registry(message: String) -> OrbitError {
    OrbitError::WorkspaceError(format!("invalid registry: {message}"))
}

fn legacy_default_base_branch() -> String {
    "main".to_string()
}

fn legacy_default_status() -> WorkspaceStatus {
    WorkspaceStatus::Active
}
