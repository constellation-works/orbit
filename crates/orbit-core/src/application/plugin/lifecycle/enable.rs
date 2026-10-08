//! `orbit plugin enable`: record grant consent and apply what the plugin
//! contributes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use orbit_automation::auto_tasks::loader::AUTO_TASKS_DIR;
use orbit_automation::routines::loader::ROUTINES_DIR;
use orbit_common::OrbitError;
use orbit_tools::plugin::{LoadedPlugin, load_plugin_dir, resolve_declared_programs};
use orbit_types::plugin::{parse_stored_grants, resolve_grant_selection};

use crate::OrbitRuntime;
use crate::runtime::plugin::grants::{recorded_program_paths, witnessed_program_paths};

use super::super::inspect::PluginSummary;
use super::super::secrets::unset_secret_warnings;
use super::super::seed::{PluginSeedOutcome, seed_plugin_definitions};
use super::super::skills::{PluginSkillLink, link_plugin_skills};
use super::record::{installed_plugin, set_enabled, verified_install_path};
use crate::runtime::plugin::definitions::load_plugin_definitions;

/// What `orbit plugin enable` was asked to do beyond recording grants.
#[derive(Debug, Clone, Default)]
pub struct PluginEnableOptions {
    /// Complete grant set to record when `--grant` is present. An empty list
    /// preserves the existing set for an ordinary re-enable; a lone `none`,
    /// `all` or `requested` selects by name instead of listing grants
    /// (see [`resolve_grant_selection`]).
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
    enable_plugin_checked(runtime, name, options, false)
}

/// Dashboard re-enable keeps the CLI's recorded consent exactly. The guard
/// runs before seeding or writing a row, and the path comparison uses the
/// same resolution later recorded by this invocation.
pub fn enable_plugin_from_dashboard(
    runtime: &OrbitRuntime,
    name: &str,
) -> Result<PluginEnableResult, OrbitError> {
    enable_plugin_checked(runtime, name, &PluginEnableOptions::default(), true)
}

fn enable_plugin_checked(
    runtime: &OrbitRuntime,
    name: &str,
    options: &PluginEnableOptions,
    preserve_consent: bool,
) -> Result<PluginEnableResult, OrbitError> {
    // Seeding reads definitions and skills out of the recorded tree, so the
    // row buys nothing until the path it names is this host's install.
    let installed = installed_plugin(runtime, name)?;
    let install_path = verified_install_path(runtime, &installed)?;
    // `requested` needs the manifest, so it is resolved before the writes
    // below rather than deferred to `set_enabled`; an invalid `--grant` then
    // fails closed with nothing yet touched, as it always has.
    let plugin = load_plugin_dir(&install_path)?;
    // An empty `options.grants` is "no `--grant` flag": preserve the
    // recorded set (`None`). Anything else — including the explicit `none`
    // alias — is a complete replacement, even when it resolves to zero
    // grants (`Some(vec![])`).
    let record_grants: Option<Vec<String>> = if options.grants.is_empty() {
        None
    } else {
        let resolved = resolve_grant_selection(&options.grants, &plugin.manifest)?;
        Some(resolved.to_recorded())
    };
    // Resolve once: a dashboard comparison must be against the paths this
    // very enable will record, not against a second PATH lookup after writes.
    let (programs, program_warnings) = resolve_consented_programs(&runtime.global_root(), &plugin);
    if preserve_consent {
        let requested =
            resolve_grant_selection(&["requested".to_string()], &plugin.manifest)?.to_recorded();
        let witnessed = witnessed_program_paths(&runtime.global_root(), &installed);
        let requested_set: BTreeSet<_> = requested.iter().collect();
        let recorded_set: BTreeSet<_> = installed.grants.iter().collect();
        if witnessed.is_none() || requested_set != recorded_set {
            let flags = if requested.is_empty() {
                "--grant none".to_string()
            } else {
                requested
                    .iter()
                    .map(|grant| format!("--grant {grant}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            return Err(OrbitError::PolicyDenied(format!(
                "plugin '{name}' requests grants [{}], which differ from recorded consent or have no recorded witness; review with `orbit plugin enable {name} {flags}`",
                requested.join(", ")
            )));
        }
        if witnessed.as_ref() != Some(&programs)
            || (!plugin.manifest.spec.requires.programs.is_empty() && programs.is_empty())
        {
            return Err(OrbitError::PolicyDenied(format!(
                "plugin '{name}' program paths differ from recorded consent (or none are recorded): requested {programs:?}, recorded {witnessed:?}; review with `orbit plugin enable {name}`"
            )));
        }
    }
    let contributions = apply_enabled_contributions(runtime, &install_path, options.force)?;
    let warn_against: &[String] = record_grants.as_deref().unwrap_or_default();
    let mut warnings = unrequested_grant_warnings(&plugin, warn_against);
    warnings.extend(contributions.warnings);
    // Every enable is consent, with or without `--grant`, so each one
    // resolves the declared programs afresh; re-running it is how an
    // operator records a program that moved or was installed since.
    warnings.extend(program_warnings);
    warnings.extend(unset_secret_warnings(&runtime.global_root(), &plugin));
    let summary = set_enabled(
        runtime,
        name,
        true,
        record_grants.as_deref(),
        Some(&programs),
    )?;

    Ok(PluginEnableResult {
        summary,
        seeded: contributions.seeded,
        skills: contributions.skills,
        warnings,
    })
}

/// Resolve `plugin`'s `requires.programs` for an enabling command, against
/// this process's `PATH` — the consenting operator's — together with a
/// warning for every entry that did not resolve, and for every one that now
/// resolves somewhere other than the last consent recorded (design §4.3).
pub(in crate::application::plugin) fn resolve_consented_programs(
    global_root: &Path,
    plugin: &LoadedPlugin,
) -> (BTreeMap<String, PathBuf>, Vec<String>) {
    let (resolved, unresolved) = resolve_declared_programs(
        &plugin.manifest.spec.requires.programs,
        std::env::var_os("PATH").as_deref(),
    );
    let previous = recorded_program_paths(global_root, plugin.namespace());
    let mut warnings: Vec<String> = unresolved
        .into_iter()
        .map(|(program, reason)| {
            format!(
                "plugin '{}' declares program `{program}`, which did not resolve: {reason}; its \
                 sandboxed backend cannot execute it. Make it resolve on PATH (or declare its \
                 absolute path), then re-run `orbit plugin enable {}`",
                plugin.namespace(),
                plugin.namespace()
            )
        })
        .collect();
    for (program, path) in &resolved {
        if let Some(before) = previous.get(program).filter(|before| *before != path) {
            warnings.push(format!(
                "plugin '{}' program `{program}` now resolves to {} (previously {}); this \
                 enable records the new path",
                plugin.namespace(),
                path.display(),
                before.display()
            ));
        }
    }
    (resolved, warnings)
}

/// What a plugin contributes to the workspace once it is enabled.
#[derive(Debug, Clone, Default)]
pub(in crate::application::plugin) struct PluginContributions {
    pub(in crate::application::plugin) seeded: Vec<PluginSeedOutcome>,
    pub(in crate::application::plugin) skills: Vec<PluginSkillLink>,
    pub(in crate::application::plugin) warnings: Vec<String>,
}

/// Seed the plugin's schedules and link its skills.
///
/// Applied at the moment the operator enables the plugin rather than at the
/// next runtime build: enabling is when they accepted what it contributes, and
/// a seeded file has to exist before the clock tick can see it. `orbit plugin
/// add --enable` runs the same path, so the two ways of enabling leave the
/// workspace in the same state.
pub(in crate::application::plugin) fn apply_enabled_contributions(
    runtime: &OrbitRuntime,
    install_path: &Path,
    force: bool,
) -> Result<PluginContributions, OrbitError> {
    let plugin = load_plugin_dir(install_path)?;
    let definitions = load_plugin_definitions(&plugin, &super::super::shipped_job_names())
        .map_err(|message| {
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
    let (skills, mut warnings) = link_plugin_skills(&runtime.global_root(), &plugin);
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

pub(in crate::application::plugin) fn unrequested_grant_warnings(
    plugin: &orbit_tools::plugin::LoadedPlugin,
    grants: &[String],
) -> Vec<String> {
    let requested = plugin.manifest.required_grants();
    parse_stored_grants(grants)
        .unwrap_or_default()
        .iter()
        .filter(|entry| !requested.contains(&entry.grant))
        .map(|grant| {
            format!(
                "grant `{grant}` was supplied but plugin '{}' does not request it; it was recorded but grants no additional access",
                plugin.namespace()
            )
        })
        .collect()
}
