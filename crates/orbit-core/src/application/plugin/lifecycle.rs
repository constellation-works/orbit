//! Enable, disable, remove, sync and migrate.

use std::path::{Path, PathBuf};

use orbit_automation::auto_tasks::loader::AUTO_TASKS_DIR;
use orbit_automation::routines::loader::ROUTINES_DIR;
use orbit_common::OrbitError;
use orbit_tools::plugin::{load_plugin_dir, load_sidecar_manifest, migrate_sidecars};
use orbit_types::plugin::{MANIFEST_FILE_NAME, PluginStatus, parse_grants};
use orbit_types::record::OrbitEvent;

use crate::OrbitRuntime;
use crate::runtime::plugin_host::{plugin_current_link, read_pin_file};

use super::inspect::{PluginSummary, show_plugin};
use super::seed::{PluginSeedOutcome, seed_plugin_definitions};
use super::skills::{PluginSkillLink, link_plugin_skills, unlink_plugin_skills};
use crate::runtime::plugin_definitions::load_plugin_definitions;

/// What `orbit plugin enable` was asked to do beyond recording grants.
#[derive(Debug, Clone, Default)]
pub struct PluginEnableOptions {
    /// Grants to record (`--grant`).
    pub grants: Vec<String>,
    /// Overwrite a seeded definition the operator has since edited (§3).
    pub force: bool,
}

/// Record the operator's grants, seed the plugin's schedules and link its
/// skills, then put its tools on the surface at the next runtime build.
///
/// The recorded grants are the only authority the loader consults (§4.1): a
/// tool whose plugin still lacks a required grant registers inactive with a
/// diagnostic naming it. Seeding writes each routine and auto-task once with
/// `enabled: false` and a provenance header, and never silently overwrites a
/// file the operator edited.
pub fn enable_plugin(
    runtime: &OrbitRuntime,
    name: &str,
    options: &PluginEnableOptions,
) -> Result<PluginEnableResult, OrbitError> {
    let grants: Vec<String> = parse_grants(&options.grants)
        .map_err(OrbitError::InvalidInput)?
        .into_iter()
        .map(|grant| grant.as_str().to_string())
        .collect();
    let summary = set_enabled(runtime, name, true, &grants)?;

    let installed = runtime
        .stores()
        .plugins()
        .get_plugin(name)?
        .ok_or_else(|| missing_install(runtime, name))?;
    let contributions =
        apply_enabled_contributions(runtime, Path::new(&installed.install_path), options.force)?;

    Ok(PluginEnableResult {
        summary,
        seeded: contributions.seeded,
        skills: contributions.skills,
        warnings: contributions.warnings,
    })
}

/// What a plugin contributes to the workspace once it is enabled.
#[derive(Debug, Clone, Default)]
pub(super) struct PluginContributions {
    pub(super) seeded: Vec<PluginSeedOutcome>,
    pub(super) skills: Vec<PluginSkillLink>,
    pub(super) warnings: Vec<String>,
}

/// Seed the plugin's schedules and link its skills.
///
/// Applied at the moment the operator enables the plugin rather than at the
/// next runtime build: enabling is when they accepted what it contributes, and
/// a seeded file has to exist before the clock tick can see it. `orbit plugin
/// add --enable` runs the same path, so the two ways of enabling leave the
/// workspace in the same state.
pub(super) fn apply_enabled_contributions(
    runtime: &OrbitRuntime,
    install_path: &Path,
    force: bool,
) -> Result<PluginContributions, OrbitError> {
    let plugin = load_plugin_dir(install_path)?;
    let definitions =
        load_plugin_definitions(&plugin, &super::shipped_job_names()).map_err(|message| {
            OrbitError::InvalidInput(format!(
                "plugin '{}' is refused: {message}",
                plugin.namespace()
            ))
        })?;
    let seeded = seed_plugin_definitions(
        &plugin,
        &definitions,
        &runtime.shared_root().join(ROUTINES_DIR),
        &runtime.paths().local_dir.join(AUTO_TASKS_DIR),
        force,
    )?;
    let (skills, mut warnings) = link_plugin_skills(&plugin);
    warnings.extend(seeded.iter().filter_map(|outcome| outcome.warning.clone()));
    Ok(PluginContributions {
        seeded,
        skills,
        warnings,
    })
}

/// Everything `orbit plugin enable` did, beyond flipping the record.
#[derive(Debug, Clone)]
pub struct PluginEnableResult {
    pub summary: PluginSummary,
    /// Routines and auto-tasks written (or deliberately preserved).
    pub seeded: Vec<PluginSeedOutcome>,
    /// Skill links maintained in the provider discovery roots.
    pub skills: Vec<PluginSkillLink>,
    /// Non-fatal problems an operator has to know about.
    pub warnings: Vec<String>,
}

/// Take the plugin off the surface: its tools stop registering, its seeded
/// definitions are skipped with a warning by the clock tick, and its skills
/// are unlinked from provider discovery.
///
/// The seeded files themselves stay: they are workspace content that may
/// carry an operator's edits, and a re-enable must not have to recreate them
/// (§3).
pub fn disable_plugin(runtime: &OrbitRuntime, name: &str) -> Result<PluginSummary, OrbitError> {
    let installed = runtime.stores().plugins().get_plugin(name)?;
    let summary = set_enabled(runtime, name, false, &[])?;
    if let Some(installed) = installed {
        unlink_plugin_skills(Path::new(&installed.install_path))?;
    }
    Ok(summary)
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
    for permission in &mut summary.permissions {
        permission.granted = grants.iter().any(|name| name == permission.grant.as_str());
    }
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
