use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::fs::io::atomic_write_bytes;
use orbit_core::{
    OrbitError, RoutineNameCollision, RoutineSeedIdentity, default_routine_name_collisions,
};
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry};
use serde::Deserialize;

/// Refuse to seed routines whose names another registered workspace on this
/// host already declares.
///
/// Routine discovery drops *every* definition sharing a name, so a silent
/// duplicate would disable the colliding workspace's routines too. Checkouts
/// of the workspace being initialized are excluded: re-initializing rebinds
/// them rather than adding a second source [ORB-12107].
pub(super) fn reject_colliding_routine_names(
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

#[derive(Deserialize)]
pub(super) struct StoredWorkspaceIdentity {
    schema_version: u32,
    pub(super) workspace_id: String,
}

pub(super) enum WorkspaceIdentityRecovery {
    Missing,
    Corrupt(Vec<u8>),
}

pub(super) fn validate_existing_registration(
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
pub(super) fn registry_claims(registry: &WorkspaceRegistry, workspace_id: &str) -> bool {
    registry
        .workspaces
        .iter()
        .any(|workspace| workspace.id == workspace_id)
        || registry
            .checkouts
            .iter()
            .any(|checkout| checkout.workspace_id == workspace_id)
}

pub(super) fn read_workspace_identity(
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
pub(super) fn validate_or_recover_workspace_identity(
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

pub(super) fn preserve_corrupt_workspace_identity(
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

pub(super) fn validate_shared_root_identity(orbit_dir: &Path) -> Result<(), OrbitError> {
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
