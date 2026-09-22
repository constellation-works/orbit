//! Enable, disable, remove, sync and migrate.

use std::path::{Path, PathBuf};

use orbit_automation::auto_tasks::loader::AUTO_TASKS_DIR;
use orbit_automation::routines::loader::ROUTINES_DIR;
use orbit_common::OrbitError;
use orbit_tools::plugin::{load_plugin_dir, load_sidecar_manifest, migrate_sidecars};
use orbit_types::plugin::{InstalledPlugin, MANIFEST_FILE_NAME, PluginStatus, parse_grants};
use orbit_types::record::OrbitEvent;

use crate::OrbitRuntime;
use crate::runtime::plugin_grants::{
    forget_authorized_grants, record_authorized_grants, verify_install_path,
};
use crate::runtime::plugin_host::{
    plugin_current_link, plugin_namespace_dir, projected_status, read_pin_file,
};

use super::inspect::{PluginSummary, show_plugin, summary_for_installed};
use super::seed::{PluginSeedOutcome, seed_plugin_definitions};
use super::skills::{PluginSkillLink, link_plugin_skills, unlink_plugin_skills};
use crate::runtime::plugin_definitions::load_plugin_definitions;

/// What `orbit plugin enable` was asked to do beyond recording grants.
#[derive(Debug, Clone, Default)]
pub struct PluginEnableOptions {
    /// Complete grant set to record when `--grant` is present. An empty list
    /// preserves the existing set for an ordinary re-enable.
    pub grants: Vec<String>,
    /// Overwrite a seeded definition the operator has since edited (§3).
    pub force: bool,
}

/// The installed tree a lifecycle verb may read from or delete, or the
/// operator-facing refusal that says why it may not.
///
/// The `plugins` row is writable by any backend holding `orbit_tools` (see the
/// `runtime::plugin_grants` module docs), so `install_path` is authority only
/// once it has been held to the install root — the same check the loader
/// applies before it reads the tree [ORB-12785]. Callers run this *before*
/// their first mutation: a refused row is the one an operator most needs to be
/// able to act on, so the refusal must leave the record, and the recovery the
/// diagnostic names, intact [ORB-12800].
fn verified_install_path(
    runtime: &OrbitRuntime,
    installed: &InstalledPlugin,
) -> Result<PathBuf, OrbitError> {
    verify_install_path(&runtime.global_root(), installed).map_err(OrbitError::PolicyDenied)?;
    Ok(PathBuf::from(&installed.install_path))
}

/// The recorded row for `name`, or the diagnostic naming what to do instead.
fn installed_plugin(runtime: &OrbitRuntime, name: &str) -> Result<InstalledPlugin, OrbitError> {
    runtime
        .stores()
        .plugins()
        .get_plugin(name)?
        .ok_or_else(|| missing_install(runtime, name))
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
    // Seeding reads definitions and skills out of the recorded tree, so the
    // row buys nothing until the path it names is this host's install.
    let install_path = verified_install_path(runtime, &installed_plugin(runtime, name)?)?;
    let contributions = apply_enabled_contributions(runtime, &install_path, options.force)?;
    let plugin = load_plugin_dir(&install_path)?;
    let mut warnings = unrequested_grant_warnings(&plugin, &grants);
    warnings.extend(contributions.warnings);
    let summary = set_enabled(runtime, name, true, &grants)?;

    Ok(PluginEnableResult {
        summary,
        seeded: contributions.seeded,
        skills: contributions.skills,
        warnings,
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
    verified_install_path(runtime, &installed_plugin(runtime, name)?)?;
    let summary = set_enabled(runtime, name, false, &[])?;
    unlink_namespace_skills(runtime, name)?;
    Ok(summary)
}

/// Drop every discovery link that points into this namespace's install family.
///
/// The selector is the namespace directory this host installs into, not the
/// row's `install_path`: a row naming `/home/<user>` would otherwise unlink
/// every skill beneath it, shipped ones included, and a record-only removal
/// has to clean up after a row whose path it deliberately never reads
/// [ORB-12800]. The namespace directory also covers links left pointing at a
/// previously installed version.
fn unlink_namespace_skills(runtime: &OrbitRuntime, name: &str) -> Result<(), OrbitError> {
    unlink_plugin_skills(&plugin_namespace_dir(&runtime.global_root(), name))?;
    Ok(())
}

fn set_enabled(
    runtime: &OrbitRuntime,
    name: &str,
    enabled: bool,
    grants: &[String],
) -> Result<PluginSummary, OrbitError> {
    let mut existing = runtime
        .stores()
        .plugins()
        .get_plugin(name)?
        .ok_or_else(|| missing_install(runtime, name))?;
    let grants = if enabled && !grants.is_empty() {
        grants.to_vec()
    } else {
        existing.grants.clone()
    };
    let projection = if enabled {
        let plugin = load_plugin_dir(Path::new(&existing.install_path))?;
        existing.enabled = true;
        existing.grants.clone_from(&grants);
        let config = orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::global_only(
            runtime.global_root(),
        ))?;
        Some((
            projected_status(&existing, &plugin, &runtime.global_root(), &config.plugins),
            plugin,
        ))
    } else {
        None
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
    // This command is the authorization, so it is what records the integrity
    // value the loader checks the row back against. Written after the row, so
    // a failure here leaves a plugin that refuses to load and says why, rather
    // than a witness authorizing a grant set the store never took [ORB-12778].
    record_authorized_grants(&runtime.global_root(), name, enabled, &grants)?;
    if let Some((projection, plugin)) = projection {
        let mut summary = summary_for_installed(&existing, Some(&plugin), projection.status);
        summary.diagnostic = projection.diagnostic;
        return Ok(summary);
    }
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

fn unrequested_grant_warnings(
    plugin: &orbit_tools::plugin::LoadedPlugin,
    grants: &[String],
) -> Vec<String> {
    let requested = plugin.manifest.required_grants();
    grants
        .iter()
        .filter(|grant| {
            !requested
                .iter()
                .any(|requested| requested.as_str() == grant.as_str())
        })
        .map(|grant| {
            format!(
                "grant `{grant}` was supplied but plugin '{}' does not request it; it was recorded but grants no additional access",
                plugin.namespace()
            )
        })
        .collect()
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

/// What `orbit plugin remove` was asked to delete.
#[derive(Debug, Clone, Default)]
pub struct PluginRemoveOptions {
    /// Drop this host's record of the plugin — its `plugins` row, its grant
    /// witness and its discovery links — and leave every file under the
    /// install root where it is.
    ///
    /// The recovery path for a row whose `install_path` this host cannot
    /// verify: ordinary removal deletes the recorded tree and therefore
    /// refuses such a row, which would otherwise leave the operator with a
    /// refused plugin and no command that removes it [ORB-12800].
    pub record_only: bool,
}

/// Remove the host's install. Derived data a plugin wrote elsewhere is
/// deliberately retained (§3).
///
/// Only the tree this host installed is deleted. The recorded `install_path`
/// is as writable as the rest of the row, so it is verified against the
/// namespace install directory before anything is removed; a row that fails
/// the check is refused whole, before any mutation, and
/// [`PluginRemoveOptions::record_only`] is what clears it.
pub fn remove_plugin(
    runtime: &OrbitRuntime,
    name: &str,
    options: &PluginRemoveOptions,
) -> Result<(), OrbitError> {
    let installed = installed_plugin(runtime, name)?;
    let owned_install = if options.record_only {
        None
    } else {
        Some(verified_install_path(runtime, &installed)?)
    };

    // Take the plugin off the surface before the record goes. Besides
    // stopping its tools and seeded definitions from registering, this removes
    // every discovery link into this namespace's install family; deleting the
    // tree first would leave those links dangling with no plugin row left for
    // doctor to inspect.
    set_enabled(runtime, name, false, &[])?;
    unlink_namespace_skills(runtime, name)?;

    runtime.with_mutation(|| {
        runtime.stores().plugins().delete_plugin(name)?;
        Ok((
            (),
            OrbitEvent::PluginRemoved {
                name: name.to_string(),
            },
        ))
    })?;

    // The authority goes with the install: a later reinstall of this namespace
    // starts from no authorized grants rather than inheriting these.
    forget_authorized_grants(&runtime.global_root(), name);

    // Everything below deletes files, so it runs only for a verified install.
    let Some(install_path) = owned_install else {
        return Ok(());
    };
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
    let namespace_dir = plugin_namespace_dir(&runtime.global_root(), name);
    if namespace_dir
        .read_dir()
        .is_ok_and(|mut entries| entries.next().is_none())
    {
        let _ = std::fs::remove_dir(&namespace_dir);
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

/// Read `.orbit/plugins.yaml` and converge the host enable state plus the
/// current workspace's enabled contributions. A grant-requesting plugin is
/// enabled only when this invocation supplies explicit grant consent.
pub fn sync_plugins(
    runtime: &OrbitRuntime,
    dry_run: bool,
    grants: &[String],
) -> Result<Vec<PluginSyncOutcome>, OrbitError> {
    let grants = parse_grants(grants)
        .map_err(OrbitError::InvalidInput)?
        .into_iter()
        .map(|grant| grant.as_str().to_string())
        .collect::<Vec<_>>();
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
                let version_message = (!satisfied).then(|| {
                    format!(
                        "installed v{} does not satisfy the pinned '{}'; reinstall with `orbit \
                         plugin add {}`",
                        installed.version,
                        pin.version.clone().unwrap_or_default(),
                        pin.source.clone().unwrap_or_else(|| "<source>".to_string())
                    )
                });
                let (status, action_message) = if dry_run {
                    let message = match (pin.enabled, installed.enabled) {
                        (false, true) => "would disable".to_string(),
                        (true, false) => "would enable after grant review".to_string(),
                        (true, true) => "would reconcile workspace contributions".to_string(),
                        (false, false) => format!("installed v{}", installed.version),
                    };
                    (
                        if installed.enabled {
                            PluginStatus::Active
                        } else {
                            PluginStatus::Disabled
                        },
                        message,
                    )
                } else if !pin.enabled && installed.enabled {
                    match disable_plugin(runtime, &pin.name) {
                        Ok(_) => (
                            PluginStatus::Disabled,
                            "disabled by workspace pin".to_string(),
                        ),
                        Err(error) => (
                            PluginStatus::Active,
                            format!("cannot disable to match workspace pin: {error}"),
                        ),
                    }
                } else if !pin.enabled {
                    (
                        PluginStatus::Disabled,
                        format!("installed v{} (disabled)", installed.version),
                    )
                } else if !installed.enabled {
                    match enable_for_sync(runtime, &pin.name, &grants) {
                        Ok(SyncEnable::Enabled(result)) => (
                            result.summary.status,
                            describe_seeded(
                                format!("enabled installed v{}", installed.version),
                                &result.seeded,
                            ),
                        ),
                        Ok(SyncEnable::NeedsGrant(names)) => (
                            PluginStatus::Disabled,
                            format!(
                                "installed but disabled; {}",
                                grant_review_message(&pin.name, &names)
                            ),
                        ),
                        Err(error) => (
                            PluginStatus::Disabled,
                            format!("installed but could not be enabled: {error}"),
                        ),
                    }
                } else {
                    match verified_install_path(runtime, &installed).and_then(|install_path| {
                        apply_enabled_contributions(runtime, &install_path, false)
                    }) {
                        Ok(contributions) => (
                            PluginStatus::Active,
                            describe_seeded(
                                format!("installed v{}", installed.version),
                                &contributions.seeded,
                            ),
                        ),
                        Err(error) => (
                            PluginStatus::Inactive,
                            format!(
                                "installed v{}, but workspace contributions could not be \
                                 reconciled: {error}",
                                installed.version
                            ),
                        ),
                    }
                };
                let message = match version_message {
                    Some(version) => format!("{version}; {action_message}"),
                    None => action_message,
                };
                outcomes.push(PluginSyncOutcome {
                    name: pin.name.clone(),
                    status,
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
                    // A committed pin is not grant consent. Install disabled,
                    // then take the same reviewed enable path used above.
                    enable: false,
                    grants: Vec::new(),
                };
                match super::install::install_pinned_plugin(
                    runtime,
                    &pin.name,
                    pin.version.as_deref(),
                    &source,
                    &options,
                ) {
                    Ok(summary) if pin.enabled => {
                        let (status, message) = match enable_for_sync(runtime, &pin.name, &grants) {
                            Ok(SyncEnable::Enabled(result)) => (
                                result.summary.status,
                                describe_seeded(
                                    format!("installed v{} from {source}", summary.version),
                                    &result.seeded,
                                ),
                            ),
                            Ok(SyncEnable::NeedsGrant(names)) => (
                                PluginStatus::Disabled,
                                format!(
                                    "installed v{} from {source}, but left disabled; {}",
                                    summary.version,
                                    grant_review_message(&pin.name, &names)
                                ),
                            ),
                            Err(error) => (
                                PluginStatus::Disabled,
                                format!(
                                    "installed v{} from {source}, but could not be enabled: \
                                     {error}",
                                    summary.version
                                ),
                            ),
                        };
                        outcomes.push(PluginSyncOutcome {
                            name: pin.name.clone(),
                            status,
                            message,
                        });
                    }
                    Ok(summary) => outcomes.push(PluginSyncOutcome {
                        name: pin.name.clone(),
                        status: PluginStatus::Disabled,
                        message: format!(
                            "installed v{} from {source} (disabled by workspace pin)",
                            summary.version
                        ),
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

enum SyncEnable {
    Enabled(Box<PluginEnableResult>),
    NeedsGrant(Vec<String>),
}

fn enable_for_sync(
    runtime: &OrbitRuntime,
    name: &str,
    grants: &[String],
) -> Result<SyncEnable, OrbitError> {
    let summary = show_plugin(runtime, name)?;
    let requested = summary
        .permissions
        .iter()
        .filter(|permission| permission.requested.is_some())
        .map(|permission| permission.grant.as_str().to_string())
        .collect::<Vec<_>>();
    if requested
        .iter()
        .any(|requested| !grants.contains(requested))
    {
        return Ok(SyncEnable::NeedsGrant(requested));
    }
    // One sync can consent to the union needed by several pins. Each plugin
    // records only the grants its own manifest requested, never the union.
    let plugin_grants = requested;
    enable_plugin(
        runtime,
        name,
        &PluginEnableOptions {
            grants: plugin_grants,
            force: false,
        },
    )
    .map(Box::new)
    .map(SyncEnable::Enabled)
}

fn grant_review_message(name: &str, grants: &[String]) -> String {
    format!(
        "plugin '{name}' requests {}; review the requests, then re-run with the complete set \
         `orbit plugin sync --grant {}` to consent and enable it",
        grants.join(", "),
        grants.join(",")
    )
}

fn describe_seeded(mut message: String, seeded: &[PluginSeedOutcome]) -> String {
    for outcome in seeded {
        message.push_str(&format!(
            "; {} {} {} ({})",
            outcome.kind,
            outcome.name,
            outcome.action.as_str(),
            outcome.path.display()
        ));
    }
    message
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
