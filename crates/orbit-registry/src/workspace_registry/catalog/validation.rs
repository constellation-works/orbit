//! Registry validation: schema/role diagnostics and canonicalization.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::identity::validate_machine_id;
use orbit_types::workspace::{
    WORKSPACE_REGISTRY_SCHEMA_VERSION, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use serde::Deserialize;
use serde_json::Value;

use super::WorkspaceRegistryMachineContext;
use super::legacy::migrate_legacy_registry;

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

/// Parse, migrate, and validate one workspace registry JSON document.
///
/// The happy path is one typed deserialize straight from the text: the
/// document is not materialised as a [`Value`] first, so every `orbit`
/// invocation parses `workspaces.json` once [DANI-10371]. Only a document the
/// typed pass rejects is re-read through `RegistryProbe`, which keeps the
/// registry-worded diagnostics for versions and role tokens and the legacy
/// migration branch.
pub fn parse_workspace_registry(
    content: &str,
    context: &WorkspaceRegistryMachineContext,
) -> Result<(WorkspaceRegistry, bool), OrbitError> {
    let mut registry: WorkspaceRegistry = match serde_json::from_str(content) {
        Ok(registry) => registry,
        Err(error) => return diagnose_registry_document(content, context, error),
    };
    check_schema_version(u64::from(registry.schema_version))?;
    let changed = validate_workspace_registry(&mut registry, context)?;
    Ok((registry, changed))
}

/// The fields checked before a typed deserialize is trusted to explain a
/// failure: a missing `schema_version` selects legacy migration, and a version
/// or role-token problem is reported in registry terms rather than as a serde
/// variant error.
#[derive(Debug, Deserialize)]
struct RegistryProbe {
    schema_version: Option<Value>,
    #[serde(default)]
    checkouts: Vec<CheckoutRoleProbe>,
}

#[derive(Debug, Deserialize)]
struct CheckoutRoleProbe {
    workspace_id: Option<String>,
    role: Option<Value>,
}

/// Explain why the typed deserialize of `content` failed, or migrate it when
/// it is a legacy document. `typed_error` is the answer when the probe finds
/// nothing more specific to say.
fn diagnose_registry_document(
    content: &str,
    context: &WorkspaceRegistryMachineContext,
    typed_error: serde_json::Error,
) -> Result<(WorkspaceRegistry, bool), OrbitError> {
    if !typed_error.is_data() {
        return Err(invalid_registry(format!("malformed JSON: {typed_error}")));
    }
    let probe: RegistryProbe = serde_json::from_str(content).map_err(|error| {
        if error.is_data() {
            invalid_registry(error.to_string())
        } else {
            invalid_registry(format!("malformed JSON: {error}"))
        }
    })?;
    let Some(version_value) = probe.schema_version else {
        let mut migrated = migrate_legacy_registry(content, context)?;
        validate_workspace_registry(&mut migrated, context)?;
        return Ok((migrated, true));
    };
    let version = version_value.as_u64().ok_or_else(|| {
        invalid_registry("schema_version must be a non-negative integer".to_string())
    })?;
    check_schema_version(version)?;
    validate_role_tokens(&probe.checkouts)?;
    Err(invalid_registry(typed_error.to_string()))
}

fn check_schema_version(version: u64) -> Result<(), OrbitError> {
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
    Ok(())
}

fn validate_role_tokens(checkouts: &[CheckoutRoleProbe]) -> Result<(), OrbitError> {
    for checkout in checkouts {
        let workspace_id = checkout.workspace_id.as_deref().unwrap_or("<unknown>");
        let Some(role) = &checkout.role else {
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
    context: &WorkspaceRegistryMachineContext,
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
                        // A standalone workspace may have been registered before this
                        // machine received an identity. The explicit owner role is
                        // the local binding, so canonicalize its logical owner now.
                        None => {
                            workspace.owner_machine_id = Some(machine_id.to_string());
                            changed = true;
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
    super::super::publication::validate_publication_bindings(registry, context)?;
    Ok(changed)
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

pub(super) fn invalid_registry(message: String) -> OrbitError {
    OrbitError::WorkspaceError(format!("invalid registry: {message}"))
}
