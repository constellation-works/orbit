//! `orbit plugin validate`.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::plugin::{
    PLUGIN_TIMEOUT_CEILING_MS, PluginValidationPolicy, load_plugin_dir, manifest_refusal,
    refuse_covering_fs_write_roots, resolve_declared_programs, resolve_plugin_root,
    validate_loaded_plugin,
};
use orbit_types::plugin::{PluginGrantSet, PluginProvenance, PluginSandbox, plugin_tool_name};

use super::profile::{PluginRenderedProfile, render_backend_profile};

use crate::OrbitRuntime;
use crate::runtime::plugin::backend::build_plugin_backend;
use crate::runtime::plugin::config::plugin_config_section;
use crate::runtime::plugin::paths::plugin_state_dir;
use crate::runtime::plugin::requirements::{host_api_deprecation, unmet_requirement};

/// What `orbit plugin validate <dir>` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginValidationReport {
    pub name: String,
    pub version: String,
    pub root: String,
    pub manifest_digest: String,
    pub tools: Vec<String>,
    /// Effective call-time profile, when validation was asked to render it.
    pub rendered: Option<PluginRenderedProfile>,
    /// Non-fatal observations: a `requires` this host does not satisfy, a
    /// first-party claim this source cannot support, sections parsed but not
    /// yet consumed.
    pub warnings: Vec<String>,
}

/// Validate a plugin source directory without installing it. `dir` is a
/// checkout holding `.orbit-plugin/` or that directory itself.
pub fn validate_plugin_dir(
    runtime: &OrbitRuntime,
    dir: &Path,
    first_party_verified: bool,
) -> Result<PluginValidationReport, OrbitError> {
    validate_plugin_dir_for_workspace(runtime, dir, first_party_verified, None)
}

/// Validate and optionally render the effective profile for one workspace.
pub fn validate_plugin_dir_for_workspace(
    runtime: &OrbitRuntime,
    dir: &Path,
    first_party_verified: bool,
    workspace: Option<&Path>,
) -> Result<PluginValidationReport, OrbitError> {
    let plugin = load_plugin_dir(&resolve_plugin_root(dir)?)?;
    let policy =
        PluginValidationPolicy::host_default().with_first_party_verified(first_party_verified);
    validate_loaded_plugin(&plugin, &policy).map_err(manifest_refusal)?;
    let global_root = runtime.global_root();
    let config = orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::new(
        &global_root,
        runtime.shared_root(),
    ))?;
    // Validation reports on the manifest, which has no operator behind it
    // yet: every grant is the unscoped form of what it requests.
    let grants = PluginGrantSet::from_grants(plugin.manifest.required_grants());
    let backend = build_plugin_backend(
        &plugin,
        PluginProvenance {
            name: plugin.namespace().to_string(),
            version: plugin.manifest.metadata.version.clone(),
            manifest_digest: plugin.manifest_digest.clone(),
            grants: grants.to_recorded(),
        },
        &plugin_state_dir(&global_root, plugin.namespace()),
        &global_root,
        grants,
        plugin_config_section(&plugin, &config.plugins),
        // No operator has consented yet, so the programs resolve against this
        // process's `PATH`, exactly as `orbit plugin enable` from here would
        // record them.
        resolve_declared_programs(
            &plugin.manifest.spec.requires.programs,
            std::env::var_os("PATH").as_deref(),
        )
        .0,
        // Validation never calls the backend, so nothing is read.
        None,
    );
    refuse_covering_fs_write_roots(backend.spec(), None).map_err(manifest_refusal)?;
    let rendered = workspace
        .map(|workspace| render_backend_profile(runtime, &plugin, &backend, workspace))
        .transpose()?;

    let mut warnings = Vec::new();
    if let Some(timeout_ms) = plugin.manifest.spec.backend.timeout_ms
        && timeout_ms > PLUGIN_TIMEOUT_CEILING_MS
    {
        warnings.push(format!(
            "`backend.timeout_ms: {timeout_ms}` exceeds the host ceiling \
             `PLUGIN_TIMEOUT_CEILING_MS` ({PLUGIN_TIMEOUT_CEILING_MS} ms); runtime caps it at \
             {PLUGIN_TIMEOUT_CEILING_MS} ms"
        ));
    }
    for skill_dir in &plugin.skills {
        if let Some(skill_id) =
            super::super::skills::plugin_skill_link_id(plugin.namespace(), skill_dir)
        {
            warnings.push(format!(
                "skill '{}' will be linked into provider discovery as '{skill_id}' when enabled",
                skill_dir.display()
            ));
        }
    }
    if let Some(message) = unmet_requirement(&plugin) {
        warnings.push(message);
    }
    if let Some(message) = host_api_deprecation(&plugin) {
        warnings.push(message);
    }
    if let Some(web) = plugin.manifest.spec.web.as_ref()
        && !(web.panels.is_empty() && web.links.is_empty())
    {
        warnings.push(format!(
            "this plugin contributes {} dashboard panel(s) and {} link tile(s) to the \
             dashboard's Plugins tab once it is enabled",
            web.panels.len(),
            web.links.len()
        ));
    }
    let tests: usize = plugin
        .tests
        .iter()
        .map(|loaded| loaded.file.tests.len())
        .sum();
    if tests == 0 {
        warnings.push(
            "this plugin ships no `spec.tests` goldens, so `orbit plugin test` cannot certify \
             it for this Orbit"
                .to_string(),
        );
    } else {
        warnings.push(format!(
            "run `orbit plugin test {}` to check its {tests} conformance golden(s) against this \
             Orbit",
            dir.display()
        ));
    }
    match super::super::load_plugin_definitions(&plugin, &super::super::shipped_job_names()) {
        Ok(definitions) => {
            if !definitions.routines.is_empty() || !definitions.auto_tasks.is_empty() {
                warnings.push(format!(
                    "`orbit plugin enable {}` seeds {} routine(s) and {} auto-task(s) as \
                     `enabled: false`; review each before switching it on",
                    plugin.namespace(),
                    definitions.routines.len(),
                    definitions.auto_tasks.len()
                ));
            }
        }
        Err(message) => return Err(OrbitError::InvalidInput(message)),
    }
    let required = plugin.manifest.required_grants();
    if !required.is_empty() {
        warnings.push(format!(
            "this plugin needs the grant{} {} at `orbit plugin enable --grant …`; without them \
             its tools register inactive",
            if required.len() == 1 { "" } else { "s" },
            required
                .iter()
                .map(|grant| format!("`{grant}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if plugin.manifest.spec.backend.sandbox == PluginSandbox::None {
        warnings.push(
            "`backend.sandbox: none` runs the backend unconfined once `unsandboxed` is granted; \
             `orbit plugin doctor` reports it"
                .to_string(),
        );
    }
    Ok(PluginValidationReport {
        name: plugin.namespace().to_string(),
        version: plugin.manifest.metadata.version.clone(),
        root: plugin.root.to_string_lossy().into_owned(),
        manifest_digest: plugin.manifest_digest.clone(),
        tools: plugin
            .tools
            .iter()
            .map(|tool| {
                plugin_tool_name(
                    plugin.namespace(),
                    &tool.verb,
                    plugin.manifest.claims_first_party_namespace(),
                )
            })
            .collect(),
        rendered,
        warnings,
    })
}
