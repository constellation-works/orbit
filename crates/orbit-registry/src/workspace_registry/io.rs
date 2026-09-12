use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, with_exclusive_file_lock};
pub use orbit_common::fs::path::global_orbit_dir;
use orbit_types::workspace::WorkspaceRegistry;

use super::{WorkspaceRegistryHostContext, parse_workspace_registry, validate_workspace_registry};
use crate::{HostIdentityState, inspect_host_identity};

const REGISTRY_FILE_NAME: &str = "workspaces.json";

/// A validated registry snapshot that has not performed any maintenance write.
#[derive(Debug)]
pub struct ReadOnlyRegistryLoad {
    pub registry: WorkspaceRegistry,
    pub migration_required: bool,
}

/// Return the path to the machine-global workspace registry.
pub fn registry_path() -> Result<PathBuf, OrbitError> {
    Ok(registry_path_for(&global_orbit_dir()?))
}

/// Return the workspace registry path under an already-resolved global root.
pub fn registry_path_for(global_root: &Path) -> PathBuf {
    global_root.join(REGISTRY_FILE_NAME)
}

/// Load the machine-global workspace registry.
pub fn load_registry() -> Result<WorkspaceRegistry, OrbitError> {
    load_registry_from(&registry_path()?)
}

/// Run `op` while holding the exclusive lock for the registry at `path`.
///
/// `load_registry_from` and `save_registry_to` are each atomic on their own,
/// but a caller that loads, edits, and saves is not: two such callers (the
/// scheduled sweep validating checkouts, `orbit workspace init` registering a
/// new one) interleave, and the second save silently drops the first one's
/// edit. Wrap the whole read-modify-write in this.
pub fn with_registry_lock<T>(
    path: &Path,
    op: impl FnOnce() -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    let parent = registry_parent(path)?;
    std::fs::create_dir_all(parent).map_err(|error| {
        OrbitError::Io(format!(
            "create workspace registry directory {}: {error}",
            parent.display()
        ))
    })?;

    let path = validated_registry_path(path)?;
    with_exclusive_file_lock(&path, "workspace registry", op)
}

/// Load, migrate, and validate a registry from an explicit path.
pub fn load_registry_from(path: &Path) -> Result<WorkspaceRegistry, OrbitError> {
    load_registry_from_with_writer(path, write_registry)
}

/// Load and validate a registry without creating a lock or persisting migrations.
///
/// Callers that only inspect current registry data can use the returned snapshot
/// directly. A caller that sees `migration_required` must re-read and migrate
/// while holding [`with_registry_lock`] before it performs maintenance.
pub fn load_registry_from_read_only(path: &Path) -> Result<ReadOnlyRegistryLoad, OrbitError> {
    let path = validated_registry_path(path)?;
    if !path.exists() {
        return Ok(ReadOnlyRegistryLoad {
            registry: WorkspaceRegistry::default(),
            migration_required: false,
        });
    }
    let content =
        std::fs::read_to_string(&path).map_err(|error| OrbitError::Io(error.to_string()))?;
    let context = registry_host_context(&path)?;
    let (registry, migration_required) = parse_workspace_registry(&content, &context)?;
    Ok(ReadOnlyRegistryLoad {
        registry,
        migration_required,
    })
}

pub(crate) fn load_registry_from_with_writer(
    path: &Path,
    writer: impl FnOnce(&WorkspaceRegistry, &Path) -> Result<(), OrbitError>,
) -> Result<WorkspaceRegistry, OrbitError> {
    let path = validated_registry_path(path)?;
    let loaded = load_registry_from_read_only(&path)?;
    if loaded.migration_required {
        writer(&loaded.registry, &path)?;
    }
    Ok(loaded.registry)
}

/// Save the machine-global workspace registry atomically.
pub fn save_registry(registry: &WorkspaceRegistry) -> Result<(), OrbitError> {
    save_registry_to(registry, &registry_path()?)
}

/// Validate and atomically save a registry to an explicit path.
pub fn save_registry_to(registry: &WorkspaceRegistry, path: &Path) -> Result<(), OrbitError> {
    let path = validated_registry_path(path)?;
    let context = registry_host_context(&path)?;
    let mut canonical = registry.clone();
    validate_workspace_registry(&mut canonical, &context)?;
    write_registry(&canonical, &path)
}

/// Resolve the registry path before it reaches a filesystem operation.
///
/// Registry callers may select a custom Orbit data root, but the file inside
/// that root is fixed. Canonicalizing the parent also removes `..` components,
/// and inspecting the final component without following it rejects a registry
/// symlink that would redirect reads or writes outside the selected root.
fn validated_registry_path(path: &Path) -> Result<PathBuf, OrbitError> {
    let parent = registry_parent(path)?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|error| OrbitError::Io(format!("canonicalize {}: {error}", parent.display())))?;
    let canonical_path = canonical_parent.join(REGISTRY_FILE_NAME);
    if !canonical_path.starts_with(&canonical_parent) {
        return Err(OrbitError::WorkspaceError(format!(
            "workspace registry path '{}' resolves outside its selected root",
            path.display()
        )));
    }

    match std::fs::symlink_metadata(&canonical_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(OrbitError::WorkspaceError(format!(
                "workspace registry path '{}' must not be a symlink",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect {}: {error}",
                canonical_path.display()
            )));
        }
    }

    Ok(canonical_path)
}

fn registry_parent(path: &Path) -> Result<&Path, OrbitError> {
    if path.file_name() != Some(OsStr::new(REGISTRY_FILE_NAME)) {
        return Err(OrbitError::WorkspaceError(format!(
            "workspace registry path must name '{REGISTRY_FILE_NAME}': {}",
            path.display()
        )));
    }

    let parent = path.parent().ok_or_else(|| {
        OrbitError::WorkspaceError(format!(
            "workspace registry path '{}' has no parent directory",
            path.display()
        ))
    })?;

    Ok(parent)
}

fn registry_host_context(path: &Path) -> Result<WorkspaceRegistryHostContext, OrbitError> {
    let global_root = path.parent().ok_or_else(|| {
        OrbitError::WorkspaceError(format!(
            "registry path '{}' has no parent directory",
            path.display()
        ))
    })?;
    match inspect_host_identity(global_root)? {
        HostIdentityState::Present(identity) => Ok(WorkspaceRegistryHostContext {
            machine_id: Some(identity.machine_id),
            host_id: Some(identity.host_id),
        }),
        HostIdentityState::Legacy { .. } | HostIdentityState::Absent => {
            Ok(WorkspaceRegistryHostContext::default())
        }
    }
}

fn write_registry(registry: &WorkspaceRegistry, path: &Path) -> Result<(), OrbitError> {
    let content = serde_json::to_string_pretty(registry)
        .map_err(|error| OrbitError::WorkspaceError(format!("serialize registry: {error}")))?;
    atomic_write_text(path, &content).map_err(|error| OrbitError::from_write_io(path, error))
}
