//! `orbit plugin migrate`: fold v1 sidecars into a v2 manifest.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_tools::plugin::{load_sidecar_manifest, migrate_sidecars};
use orbit_types::plugin::{MANIFEST_FILE_NAME, plugin_root_in};

/// What `orbit plugin migrate` was asked to fold together.
#[derive(Debug, Clone)]
pub struct PluginMigrateRequest {
    /// The executable the v1 sidecars belong to.
    pub backend_command: String,
    /// Explicit sidecar files; when empty, every `*.orbit-tool.yaml` beside
    /// the executable is used.
    pub sidecars: Vec<PathBuf>,
    /// Version for the generated manifest.
    pub version: String,
    /// Namespace override when the v1 names do not imply one.
    pub namespace: Option<String>,
    /// Source directory to write the plugin into: the manifest and the copied
    /// backend go in its `.orbit-plugin/`. `None` returns the YAML without
    /// writing.
    pub out_dir: Option<PathBuf>,
}

/// Write a v2 manifest from a set of v1 sidecars (§4.8). The v1 sidecars and
/// `orbit tool add` keep working; this only produces the new file.
#[allow(
    clippy::disallowed_methods,
    reason = "migration writes a user-selected plugin source tree, outside Orbit state"
)]
pub fn migrate_plugin_sidecars(
    request: &PluginMigrateRequest,
) -> Result<(String, Option<PathBuf>), OrbitError> {
    let backend = Path::new(&request.backend_command);
    let sidecar_paths = if request.sidecars.is_empty() {
        discover_sidecars(backend)?
    } else {
        request.sidecars.clone()
    };
    let mut sidecars = Vec::with_capacity(sidecar_paths.len());
    for path in &sidecar_paths {
        sidecars.push(load_sidecar_manifest(path)?);
    }
    let migrated_command = request.out_dir.as_ref().map_or_else(
        || Ok(request.backend_command.clone()),
        |_| {
            backend
                .file_name()
                .filter(|name| !name.is_empty())
                .map(|name| Path::new("bin").join(name).to_string_lossy().into_owned())
                .ok_or_else(|| {
                    OrbitError::InvalidInput(format!(
                        "cannot copy backend '{}': it has no file name",
                        backend.display()
                    ))
                })
        },
    )?;
    let manifest = migrate_sidecars(
        &sidecars,
        &migrated_command,
        &request.version,
        request.namespace.as_deref(),
    )?;
    let yaml = serde_yaml::to_string(&manifest)
        .map_err(|error| OrbitError::Execution(format!("serialize plugin manifest: {error}")))?;
    let Some(out_dir) = request.out_dir.as_deref().map(plugin_root_in) else {
        return Ok((yaml, None));
    };
    std::fs::create_dir_all(&out_dir)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", out_dir.display())))?;
    let path = out_dir.join(MANIFEST_FILE_NAME);
    if path.exists() {
        return Err(OrbitError::InvalidInput(format!(
            "refusing to overwrite {}",
            path.display()
        )));
    }
    let backend_target = out_dir.join("bin").join(backend.file_name().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "cannot copy backend '{}': it has no file name",
            backend.display()
        ))
    })?);
    if backend_target.exists() {
        return Err(OrbitError::InvalidInput(format!(
            "refusing to overwrite copied backend {}",
            backend_target.display()
        )));
    }
    let backend_parent = backend_target.parent().ok_or_else(|| {
        OrbitError::Execution(format!(
            "backend target {} has no parent",
            backend_target.display()
        ))
    })?;
    std::fs::create_dir_all(backend_parent)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", backend_parent.display())))?;
    std::fs::copy(backend, &backend_target).map_err(|error| {
        OrbitError::Io(format!(
            "copy backend {} to {}: {error}",
            backend.display(),
            backend_target.display()
        ))
    })?;
    atomic_write_text(&path, &yaml)
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", path.display())))?;
    Ok((yaml, Some(path)))
}

/// Every `*.orbit-tool.yaml` beside the executable, in a stable order.
fn discover_sidecars(backend: &Path) -> Result<Vec<PathBuf>, OrbitError> {
    let dir = backend.parent().filter(|dir| !dir.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", dir.display())))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.ends_with(".orbit-tool.yaml") || name.ends_with(".orbit-tool.yml")
                })
        })
        .collect();
    found.sort();
    if found.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "no `*.orbit-tool.yaml` sidecars found beside {}; pass --sidecar explicitly",
            backend.display()
        )));
    }
    Ok(found)
}
