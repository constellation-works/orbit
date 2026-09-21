//! Enable, disable, remove, sync and migrate.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_tools::plugin::{load_sidecar_manifest, migrate_sidecars};
use orbit_types::plugin::{MANIFEST_FILE_NAME, PluginStatus};
use orbit_types::record::OrbitEvent;

use crate::OrbitRuntime;
use crate::runtime::plugin_host::{plugin_current_link, read_pin_file};

use super::inspect::{PluginSummary, show_plugin};

/// Record the operator's grants and put the plugin's tools on the surface at
/// the next runtime build. The grants are recorded, not enforced (§4.1).
pub fn enable_plugin(
    runtime: &OrbitRuntime,
    name: &str,
    grants: &[String],
) -> Result<PluginSummary, OrbitError> {
    set_enabled(runtime, name, true, grants)
}

pub fn disable_plugin(runtime: &OrbitRuntime, name: &str) -> Result<PluginSummary, OrbitError> {
    set_enabled(runtime, name, false, &[])
}

fn set_enabled(
    runtime: &OrbitRuntime,
    name: &str,
    enabled: bool,
    grants: &[String],
) -> Result<PluginSummary, OrbitError> {
    let existing = runtime
        .stores()
        .plugins()
        .get_plugin(name)?
        .ok_or_else(|| missing_install(runtime, name))?;
    let grants = if enabled {
        let mut merged = existing.grants.clone();
        for grant in grants {
            if !merged.iter().any(|existing| existing == grant) {
                merged.push(grant.clone());
            }
        }
        merged
    } else {
        existing.grants.clone()
    };
    runtime.with_mutation(|| {
        runtime
            .stores()
            .plugins()
            .set_plugin_enabled(name, enabled, &grants)?;
        let event = if enabled {
            OrbitEvent::PluginEnabled {
                name: name.to_string(),
            }
        } else {
            OrbitEvent::PluginDisabled {
                name: name.to_string(),
            }
        };
        Ok(((), event))
    })?;
    // The live runtime built its registry before this write, so report the
    // stored state rather than the surface this process happens to hold.
    let mut summary = show_plugin(runtime, name)?;
    summary.status = if enabled {
        PluginStatus::Active
    } else {
        PluginStatus::Disabled
    };
    summary.granted = grants;
    summary.diagnostic = None;
    Ok(summary)
}

fn missing_install(runtime: &OrbitRuntime, name: &str) -> OrbitError {
    let pinned = read_pin_file(&runtime.shared_root())
        .ok()
        .flatten()
        .is_some_and(|pins| pins.plugins.iter().any(|pin| pin.name == name));
    if pinned {
        OrbitError::InvalidInput(format!(
            "plugin '{name}' is pinned by this workspace but not installed on this host; run \
             `orbit plugin sync` first"
        ))
    } else {
        OrbitError::InvalidInput(format!(
            "plugin '{name}' is not installed on this host; run `orbit plugin add <source>` first"
        ))
    }
}

/// Remove the host's install. Derived data a plugin wrote elsewhere is
/// deliberately retained (§3).
pub fn remove_plugin(runtime: &OrbitRuntime, name: &str) -> Result<(), OrbitError> {
    let installed = runtime
        .stores()
        .plugins()
        .get_plugin(name)?
        .ok_or_else(|| missing_install(runtime, name))?;
    runtime.with_mutation(|| {
        runtime.stores().plugins().delete_plugin(name)?;
        Ok((
            (),
            OrbitEvent::PluginRemoved {
                name: name.to_string(),
            },
        ))
    })?;

    let install_path = PathBuf::from(&installed.install_path);
    if install_path.is_dir() {
        std::fs::remove_dir_all(&install_path).map_err(|error| {
            OrbitError::Io(format!("remove {}: {error}", install_path.display()))
        })?;
    }
    let link = plugin_current_link(&runtime.global_root(), name);
    if link.symlink_metadata().is_ok() {
        let _ = std::fs::remove_file(&link).or_else(|_| std::fs::remove_dir_all(&link));
    }
    // Leave the namespace directory only when another version still lives in it.
    if let Some(parent) = install_path.parent()
        && parent
            .read_dir()
            .is_ok_and(|mut entries| entries.next().is_none())
    {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(())
}

/// What `orbit plugin sync` found for one pinned plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSyncOutcome {
    pub name: String,
    pub status: PluginStatus,
    /// What sync did, or what the operator has to do.
    pub message: String,
}

/// Read `.orbit/plugins.yaml` and install what the host is missing, or report
/// it when the pin names no source.
pub fn sync_plugins(
    runtime: &OrbitRuntime,
    dry_run: bool,
) -> Result<Vec<PluginSyncOutcome>, OrbitError> {
    let Some(pins) = read_pin_file(&runtime.shared_root())? else {
        return Ok(Vec::new());
    };
    let mut outcomes = Vec::new();
    for pin in &pins.plugins {
        let installed = runtime.stores().plugins().get_plugin(&pin.name)?;
        match installed {
            Some(installed) => {
                let satisfied = pin
                    .version
                    .as_deref()
                    .is_none_or(|requirement| version_satisfies(requirement, &installed.version));
                let message = if !satisfied {
                    format!(
                        "installed v{} does not satisfy the pinned '{}'; reinstall with `orbit \
                         plugin add {}`",
                        installed.version,
                        pin.version.clone().unwrap_or_default(),
                        pin.source.clone().unwrap_or_else(|| "<source>".to_string())
                    )
                } else if pin.enabled && !installed.enabled {
                    format!(
                        "installed but disabled; run `orbit plugin enable {}`",
                        pin.name
                    )
                } else {
                    format!("installed v{}", installed.version)
                };
                outcomes.push(PluginSyncOutcome {
                    name: pin.name.clone(),
                    status: if installed.enabled {
                        PluginStatus::Active
                    } else {
                        PluginStatus::Disabled
                    },
                    message,
                });
            }
            None => {
                let Some(source) = pin.source.clone() else {
                    outcomes.push(PluginSyncOutcome {
                        name: pin.name.clone(),
                        status: PluginStatus::Missing,
                        message: "not installed and the pin names no `source`; add a source \
                                  to `.orbit/plugins.yaml` or run `orbit plugin add <source>`"
                            .to_string(),
                    });
                    continue;
                };
                if dry_run {
                    outcomes.push(PluginSyncOutcome {
                        name: pin.name.clone(),
                        status: PluginStatus::Missing,
                        message: format!("would install from {source}"),
                    });
                    continue;
                }
                let options = super::install::PluginAddOptions {
                    force: false,
                    enable: pin.enabled,
                    grants: Vec::new(),
                };
                match super::install::install_plugin(runtime, &source, &options) {
                    Ok(summary) => outcomes.push(PluginSyncOutcome {
                        name: pin.name.clone(),
                        status: summary.status,
                        message: format!("installed v{} from {source}", summary.version),
                    }),
                    // One pin's failure must not stop the rest: a host that
                    // cannot reach one source still converges on the others.
                    Err(error) => outcomes.push(PluginSyncOutcome {
                        name: pin.name.clone(),
                        status: PluginStatus::Missing,
                        message: format!("cannot install from {source}: {error}"),
                    }),
                }
            }
        }
    }
    Ok(outcomes)
}

fn version_satisfies(requirement: &str, version: &str) -> bool {
    match (
        orbit_types::plugin::SemverRange::parse(requirement),
        version.parse::<orbit_types::plugin::Version>(),
    ) {
        (Ok(range), Ok(version)) => range.matches(&version),
        _ => false,
    }
}

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
    /// Where to write `plugin.yaml`; `None` returns the YAML without writing.
    pub out_dir: Option<PathBuf>,
}

/// Write a v2 manifest from a set of v1 sidecars (§4.8). The v1 sidecars and
/// `orbit tool add` keep working; this only produces the new file.
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
    let manifest = migrate_sidecars(
        &sidecars,
        &request.backend_command,
        &request.version,
        request.namespace.as_deref(),
    )?;
    let yaml = serde_yaml::to_string(&manifest)
        .map_err(|error| OrbitError::Execution(format!("serialize plugin manifest: {error}")))?;
    let Some(out_dir) = request.out_dir.clone() else {
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
    std::fs::write(&path, &yaml)
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
