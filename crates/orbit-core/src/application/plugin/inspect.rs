//! Read-only plugin surfaces: `list`, `show`, `doctor` and `validate`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_automation::auto_tasks::loader::AUTO_TASKS_DIR;
use orbit_automation::routines::loader::ROUTINES_DIR;
use orbit_common::OrbitError;
use orbit_tools::ToolContext;
use orbit_tools::plugin::{
    LoadedPlugin, PLUGIN_TIMEOUT_CEILING_MS, PluginBackend, PluginProgramStatus,
    PluginValidationPolicy, load_plugin_dir, manifest_refusal, program_statuses,
    refuse_covering_fs_write_roots, resolve_declared_programs, validate_loaded_plugin,
};
use orbit_types::plugin::{
    InstalledPlugin, PluginExecutionKind, PluginGrant, PluginGrantSet, PluginProvenance,
    PluginSandbox, PluginStatus, SemverRange, Version, parse_archive_digest, parse_stored_grants,
    plugin_tool_name, remote_archive_source,
};

use super::panels::{PluginLinkSummary, PluginPanelSummary, web_summaries};

use crate::OrbitRuntime;
use crate::runtime::plugin::backend::{build_plugin_backend, plugin_backend};
use crate::runtime::plugin::cache::load_installed_plugin;
use crate::runtime::plugin::config::plugin_config_section;
use crate::runtime::plugin::grants::recorded_program_paths;
use crate::runtime::plugin::paths::{plugin_state_dir, read_pin_file};
use crate::runtime::plugin::requirements::{host_api_deprecation, unmet_requirement};

/// One plugin tool as the CLI reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginToolSummary {
    /// Canonical registry name (`<ns>.<verb>` or `orbit.<ns>.<verb>`).
    pub name: String,
    /// MCP-advertised name, absent when `mcp_scope: none`.
    pub advertised_name: Option<String>,
    pub execution_kind: PluginExecutionKind,
    pub mcp_scope: String,
    pub active: bool,
}

/// One grant as `orbit plugin show` reports it: what the manifest asks for
/// beside whether the operator granted it (design §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPermissionSummary {
    pub grant: PluginGrant,
    /// The manifest's request, `None` when it does not ask for this grant.
    pub requested: Option<String>,
    pub granted: bool,
    /// The roots the operator scoped this grant to, `None` when the grant is
    /// unscoped — either not granted at all, or granted as the whole request
    /// the manifest makes. Reading it beside `requested` is how a surface
    /// shows the delta: the manifest asks for these paths, the operator
    /// allowed those, and the sandbox opens the intersection.
    pub granted_roots: Option<Vec<String>>,
}

/// One plugin as `orbit plugin list` / `show` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSummary {
    pub name: String,
    pub version: String,
    pub status: PluginStatus,
    pub source: String,
    pub install_path: String,
    pub manifest_digest: String,
    pub publisher: Option<String>,
    pub description: String,
    pub first_party: bool,
    /// Every grant, with the manifest's request and the operator's answer
    /// side by side (§4.1). Only the answer is authority.
    pub permissions: Vec<PluginPermissionSummary>,
    /// Grants recorded at `orbit plugin enable --grant …`.
    pub granted: Vec<String>,
    /// `backend.sandbox: none` with the `unsandboxed` grant: the backend runs
    /// unconfined, which `doctor` reports as a finding (§4.3).
    pub unsandboxed: bool,
    /// Every `requires.programs` entry beside the path the last enabling
    /// command resolved it to, and why the sandbox will not grant it when it
    /// will not (§4.3).
    pub programs: Vec<PluginProgramStatus>,
    pub tools: Vec<PluginToolSummary>,
    /// `spec.web.panels[]` of an active plugin (§4.7). Empty for a plugin
    /// that is not serving its tools: a panel reads one of them.
    pub panels: Vec<PluginPanelSummary>,
    /// `spec.web.links[]`, with `{{config.<key>}}` resolved.
    pub links: Vec<PluginLinkSummary>,
    /// The Orbit version this plugin's conformance goldens last passed on
    /// (§5), when `orbit plugin test` has recorded one.
    pub certified_orbit_version: Option<String>,
    /// Why the plugin is not active, when it is not.
    pub diagnostic: Option<String>,
    /// Whether `.orbit/plugins.yaml` pins this plugin.
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDoctorResult {
    pub plugin: String,
    pub status: PluginStatus,
    /// The step an operator has to take, or empty when there is none.
    pub message: String,
}

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

/// The exact sandbox and environment projection a validated backend would
/// receive for the selected workspace on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRenderedProfile {
    pub workspace: String,
    pub read: Vec<String>,
    pub read_denies: Vec<String>,
    pub write: Vec<String>,
    pub write_files: Vec<String>,
    pub network: String,
    pub unsandboxed: bool,
    pub environments: Vec<PluginRenderedEnvironment>,
}

/// One child environment. Exec backends have one per tool because
/// `ORBIT_TOOL_NAME` differs; an MCP backend has one shared environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRenderedEnvironment {
    pub tool: Option<String>,
    pub variables: BTreeMap<String, String>,
}

pub fn list_plugins(runtime: &OrbitRuntime) -> Result<Vec<PluginSummary>, OrbitError> {
    let pinned = pinned_names(runtime);
    let mut summaries: Vec<PluginSummary> = runtime
        .stores()
        .plugins()
        .list_plugins()?
        .iter()
        .map(|installed| {
            let mut summary = summary_from_runtime(runtime, installed);
            summary.pinned = pinned.contains(&summary.name);
            summary
        })
        .collect();
    // A pin this host never installed has no record, and is exactly what an
    // operator needs to see here.
    for name in pinned {
        if summaries.iter().any(|summary| summary.name == name) {
            continue;
        }
        summaries.push(PluginSummary {
            name: name.clone(),
            version: String::new(),
            status: PluginStatus::Missing,
            source: String::new(),
            install_path: String::new(),
            manifest_digest: String::new(),
            publisher: None,
            description: String::new(),
            first_party: false,
            permissions: Vec::new(),
            granted: Vec::new(),
            unsandboxed: false,
            programs: Vec::new(),
            tools: Vec::new(),
            panels: Vec::new(),
            links: Vec::new(),
            certified_orbit_version: None,
            diagnostic: runtime_diagnostic(runtime, &name),
            pinned: true,
        });
    }
    summaries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(summaries)
}

pub fn show_plugin(runtime: &OrbitRuntime, name: &str) -> Result<PluginSummary, OrbitError> {
    list_plugins(runtime)?
        .into_iter()
        .find(|summary| summary.name == name)
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "plugin '{name}' is neither installed on this host nor pinned by this workspace"
            ))
        })
}

/// One row per plugin, naming the step that would make it active, or the
/// finding an active plugin carries (an unsandboxed backend).
pub fn plugin_doctor(runtime: &OrbitRuntime) -> Result<Vec<PluginDoctorResult>, OrbitError> {
    let summaries = list_plugins(runtime)?;
    let invalid_pin_file = read_pin_file(&runtime.paths().local_dir)
        .err()
        .map(|error| PluginDoctorResult {
            plugin: "pin file".to_string(),
            status: PluginStatus::Inactive,
            message: error.to_string(),
        });
    let stale_seeded = stale_seeded_definition_rows(runtime, &summaries)?;
    let archive_drift = archive_digest_drift_rows(runtime, &summaries)?;
    let scoped_out = scoped_out_fs_root_rows(runtime)?;
    let ungranted_programs = ungranted_program_rows(&summaries);
    let host_api_deprecated = host_api_deprecation_rows(runtime);
    let unset_secrets = super::secrets::unset_secret_rows(runtime, &summaries)?;
    // A skill link whose target is gone is invisible to the skill catalog's
    // own doctor — it only walks seeded trees — and to the plugin record,
    // which says nothing about the provider discovery roots (§3).
    let mut dangling = Vec::new();
    for summary in &summaries {
        if summary.install_path.is_empty() {
            continue;
        }
        for (link, target) in super::skills::dangling_plugin_skill_links(
            &runtime.global_root(),
            Path::new(&summary.install_path),
        ) {
            dangling.push(PluginDoctorResult {
                plugin: summary.name.clone(),
                status: summary.status,
                message: format!(
                    "skill link '{}' points at '{}', which no longer exists; run `orbit plugin \
                     enable {}` to relink it, or delete the link",
                    link.display(),
                    target.display(),
                    summary.name
                ),
            });
        }
    }
    let mut rows: Vec<PluginDoctorResult> = summaries
        .into_iter()
        .map(|summary| {
            let message = summary
                .diagnostic
                .clone()
                .unwrap_or_else(|| match summary.status {
                    PluginStatus::Active if summary.unsandboxed => format!(
                    "plugin '{}' runs unsandboxed: its manifest declares `backend.sandbox: none` \
                     and this host granted `unsandboxed`, so its backend is not confined by \
                     Landlock or sandbox-exec",
                    summary.name
                ),
                    PluginStatus::Active => String::new(),
                    PluginStatus::Disabled => format!(
                        "plugin '{}' is installed but disabled; run `orbit plugin enable {}`",
                        summary.name, summary.name
                    ),
                    PluginStatus::Missing => format!(
                    "plugin '{}' is pinned by this workspace but not installed on this host; run \
                     `orbit plugin sync`",
                    summary.name
                ),
                    PluginStatus::Inactive => format!(
                    "plugin '{}' is enabled but was refused at load; run `orbit plugin show {}`",
                    summary.name, summary.name
                ),
                });
            PluginDoctorResult {
                plugin: summary.name,
                status: summary.status,
                message,
            }
        })
        .collect();
    rows.extend(dangling);
    rows.extend(stale_seeded);
    rows.extend(archive_drift);
    rows.extend(scoped_out);
    rows.extend(ungranted_programs);
    rows.extend(host_api_deprecated);
    rows.extend(unset_secrets);
    if let Some(finding) = invalid_pin_file {
        rows.push(finding);
    }
    Ok(rows)
}

/// Findings for an active plugin whose declared program the sandbox will not
/// grant: it did not resolve when the plugin was enabled, or the recorded
/// path no longer names that executable.
///
/// Without this row an active plugin looks healthy until its backend tries
/// to run the program and gets `Permission denied` from inside the sandbox.
fn ungranted_program_rows(summaries: &[PluginSummary]) -> Vec<PluginDoctorResult> {
    summaries
        .iter()
        .filter(|summary| summary.status == PluginStatus::Active)
        .flat_map(|summary| {
            summary.programs.iter().filter_map(|program| {
                let problem = program.problem.as_deref()?;
                Some(PluginDoctorResult {
                    plugin: summary.name.clone(),
                    status: summary.status,
                    message: format!(
                        "plugin '{}' declares program `{}` in `requires.programs`, but {problem}; \
                         its sandboxed backend cannot execute it. Make it resolve on PATH (or \
                         declare its absolute path), then re-run `orbit plugin enable {}` to \
                         record it",
                        summary.name, program.name, summary.name,
                    ),
                })
            })
        })
        .collect()
}

/// Findings for a plugin running on the `host_api` previous-major grace
/// window (§4.8): still active, but due to refuse once this host drops it.
fn host_api_deprecation_rows(runtime: &OrbitRuntime) -> Vec<PluginDoctorResult> {
    runtime
        .plugin_load()
        .registered
        .iter()
        .filter_map(|entry| {
            let plugin = entry.loaded.as_deref()?;
            let message = host_api_deprecation(plugin)?;
            Some(PluginDoctorResult {
                plugin: entry.name.clone(),
                status: entry.status,
                message,
            })
        })
        .collect()
}

/// Findings for a plugin whose `fs` grant is scoped past a root its own
/// manifest asks for.
///
/// The narrowing itself is the operator's decision and not a problem, so
/// `orbit plugin show` reports it and `doctor` stays quiet about it. What
/// `doctor` names is the part the operator cannot see from either side alone:
/// a requested root that overlaps *nothing* granted will never open, so the
/// plugin will fail somewhere inside itself rather than at load [ORB-12840].
fn scoped_out_fs_root_rows(runtime: &OrbitRuntime) -> Result<Vec<PluginDoctorResult>, OrbitError> {
    let mut rows = Vec::new();
    // The workspace a call from here would resolve to, so the overlap this
    // reports is the one that call would compute. A root rendered against
    // some other workspace could report a drop that never happens.
    let workspace_root = runtime.paths().repo_root.clone();
    // The effective `[plugins.<ns>]` section, because an fs root may be
    // written `{{config.<key>}}` and would otherwise fail to render here and
    // report nothing.
    let config = orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::new(
        runtime.global_root(),
        runtime.shared_root(),
    ))?;
    for entry in &runtime.plugin_load().registered {
        // A row whose grants did not verify granted nothing at all, and is
        // already its own finding; reading its scope would repeat an
        // unauthorized claim back as though it were a narrowing.
        if !entry.grants_authorized {
            continue;
        }
        let Some(plugin) = entry.loaded.as_deref() else {
            continue;
        };
        let Some(installed) = runtime.stores().plugins().get_plugin(&entry.name)? else {
            continue;
        };
        let backend = plugin_backend(&runtime.global_root(), &installed, plugin, &config.plugins);
        for root in backend.spec().dropped_fs_roots(Some(&workspace_root)) {
            rows.push(PluginDoctorResult {
                plugin: entry.name.clone(),
                status: entry.status,
                message: format!(
                    "plugin '{}' requests `{}` at {}, which lies outside every root this host \
                     granted; the sandbox will not open it. Re-run `orbit plugin enable {} \
                     --grant fs=<root>[,<root>]` with a list that covers it, or `--grant fs` to \
                     grant the whole request",
                    entry.name, root.declared, root.field, entry.name,
                ),
            });
        }
    }
    Ok(rows)
}

/// Findings for a pinned `https://` archive whose digest no longer describes
/// what this host installed.
///
/// The comparison is between the pin file and the digest recorded at install
/// time, so it is offline and deterministic: doctor reports that the two
/// disagree, it does not go back to the network to re-hash the URL. That
/// covers the case that matters — a pin bumped to a new release, or edited to
/// a digest nobody has installed — without turning a diagnostic command into
/// a download.
fn archive_digest_drift_rows(
    runtime: &OrbitRuntime,
    summaries: &[PluginSummary],
) -> Result<Vec<PluginDoctorResult>, OrbitError> {
    // A pin file that does not parse is already its own doctor row; this
    // check has nothing to add about it.
    let Ok(Some(pins)) = read_pin_file(&runtime.shared_root()) else {
        return Ok(Vec::new());
    };
    let status_of = summaries
        .iter()
        .map(|summary| (summary.name.as_str(), summary.status))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::new();
    for pin in &pins.plugins {
        let Some(url) = pin.source.as_deref().and_then(remote_archive_source) else {
            continue;
        };
        // A malformed digest is already a pin-file finding of its own; this
        // row is only about two well-formed digests disagreeing.
        let Some(expected) = pin
            .digest
            .as_deref()
            .and_then(|digest| parse_archive_digest(digest).ok())
        else {
            continue;
        };
        let Some(installed) = runtime.stores().plugins().get_plugin(&pin.name)? else {
            continue;
        };
        let name = &pin.name;
        let message = match installed.archive_digest.as_deref() {
            Some(actual) if actual == expected => continue,
            Some(actual) => format!(
                "plugin '{name}' was installed from an archive hashing to sha256:{actual}, but \
                 the workspace pin for '{name}' now names sha256:{expected}; run `orbit plugin \
                 upgrade {name} {url} --digest sha256:{expected}` to install the pinned archive"
            ),
            None => format!(
                "plugin '{name}' is pinned to the archive {url} at sha256:{expected}, but this \
                 host's install records no archive digest and so was never checked against it; \
                 reinstall it with `orbit plugin upgrade {name} {url} --digest sha256:{expected}`"
            ),
        };
        rows.push(PluginDoctorResult {
            plugin: name.clone(),
            status: status_of
                .get(name.as_str())
                .copied()
                .unwrap_or(PluginStatus::Missing),
            message,
        });
    }
    Ok(rows)
}

/// Findings for workspace files seeded by an older installed plugin version.
/// A customised file is deliberately preserved by sync, so doctor names both
/// the ordinary refresh and the reviewed `--force` recovery.
fn stale_seeded_definition_rows(
    runtime: &OrbitRuntime,
    summaries: &[PluginSummary],
) -> Result<Vec<PluginDoctorResult>, OrbitError> {
    let installed = summaries
        .iter()
        .filter(|summary| !summary.version.is_empty())
        .map(|summary| (summary.name.as_str(), summary))
        .collect::<BTreeMap<_, _>>();
    let mut stale: BTreeMap<&str, Vec<(PathBuf, String)>> = BTreeMap::new();

    for dir in [
        runtime.shared_root().join(ROUTINES_DIR),
        runtime.paths().local_dir.join(AUTO_TASKS_DIR),
    ] {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "read seeded plugin definitions '{}': {error}",
                    dir.display()
                )));
            }
        };
        for entry in entries {
            let entry = entry.map_err(|error| {
                OrbitError::Io(format!(
                    "read seeded plugin definitions '{}': {error}",
                    dir.display()
                ))
            })?;
            let path = entry.path();
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let Some((namespace, seeded_version)) =
                crate::runtime::plugin::definitions::read_definition_provenance(&path)
            else {
                continue;
            };
            let Some(summary) = installed.get(namespace.as_str()) else {
                continue;
            };
            if seeded_version_lags(&seeded_version, &summary.version) {
                stale
                    .entry(summary.name.as_str())
                    .or_default()
                    .push((path, seeded_version));
            }
        }
    }

    Ok(stale
        .into_iter()
        .filter_map(|(name, mut definitions)| {
            let summary = installed.get(name)?;
            definitions.sort_by(|left, right| left.0.cmp(&right.0));
            let details = definitions
                .iter()
                .map(|(path, version)| format!("{} (v{version})", path.display()))
                .collect::<Vec<_>>()
                .join(", ");
            Some(PluginDoctorResult {
                plugin: name.to_string(),
                status: summary.status,
                message: format!(
                    "workspace has seeded definitions that lag installed plugin '{name}' \
                     v{}: {details}; run `orbit plugin sync` to refresh unchanged files, or \
                     review customised files and run `orbit plugin enable {name} --force`",
                    summary.version
                ),
            })
        })
        .collect())
}

fn seeded_version_lags(seeded: &str, installed: &str) -> bool {
    let Ok(installed) = installed.parse::<Version>() else {
        return false;
    };
    SemverRange::parse(&format!(">{seeded}")).is_ok_and(|range| range.matches(&installed))
}

/// Validate a plugin directory without installing it.
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
    let plugin = load_plugin_dir(dir)?;
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
        if let Some(skill_id) = super::skills::plugin_skill_link_id(plugin.namespace(), skill_dir) {
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
    match super::load_plugin_definitions(&plugin, &super::shipped_job_names()) {
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

fn render_backend_profile(
    runtime: &OrbitRuntime,
    plugin: &LoadedPlugin,
    backend: &PluginBackend,
    workspace: &Path,
) -> Result<PluginRenderedProfile, OrbitError> {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let workspace_text = workspace.to_string_lossy().into_owned();
    let profile = backend.spec().sandbox_profile(Some(&workspace))?;
    let context = ToolContext {
        cwd: Some(workspace_text.clone()),
        workspace_root: Some(workspace.clone()),
        proc_allowed_programs: plugin.manifest.spec.requires.programs.clone(),
        proc_spawn_environment: Some(runtime.execution_env_policy().agent_subprocess_env(&[])),
        ..ToolContext::default()
    };
    let environment_tools: Vec<Option<String>> = match backend {
        PluginBackend::Exec(_) => plugin
            .tools
            .iter()
            .map(|tool| {
                Some(plugin_tool_name(
                    plugin.namespace(),
                    &tool.verb,
                    plugin.manifest.claims_first_party_namespace(),
                ))
            })
            .collect(),
        PluginBackend::Mcp(_) => vec![None],
    };
    let environments = environment_tools
        .into_iter()
        .map(|tool| PluginRenderedEnvironment {
            variables: backend
                .spec()
                .child_environment(&context, &workspace_text, tool.as_deref())
                .into_iter()
                .collect(),
            tool,
        })
        .collect();
    let paths = |items: Vec<PathBuf>| {
        items
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    };
    let network = match profile.network {
        orbit_types::plugin::PluginNetworkPermission::None => "none",
        orbit_types::plugin::PluginNetworkPermission::Loopback => "loopback",
        orbit_types::plugin::PluginNetworkPermission::Any => "any",
    }
    .to_string();
    Ok(PluginRenderedProfile {
        workspace: workspace_text,
        read: paths(profile.read),
        read_denies: paths(profile.read_denies),
        write: paths(profile.write),
        write_files: paths(profile.write_files),
        network,
        unsandboxed: profile.unsandboxed,
        environments,
    })
}

/// Summary of an installed plugin using this runtime's own load outcome, so
/// the report and the live tool surface cannot disagree.
fn summary_from_runtime(runtime: &OrbitRuntime, installed: &InstalledPlugin) -> PluginSummary {
    let registered = runtime
        .plugin_load()
        .registered
        .iter()
        .find(|entry| entry.name == installed.name);
    let status = registered.map_or_else(
        || {
            if installed.enabled {
                PluginStatus::Inactive
            } else {
                PluginStatus::Disabled
            }
        },
        |entry| entry.status,
    );
    // Disabled rows are deliberately not loaded during ordinary runtime
    // construction. `plugin list` still reports their manifest details, but
    // only this command pays that one cached load.
    let disabled = if installed.enabled {
        None
    } else {
        load_installed_plugin(installed).ok()
    };
    let loaded = registered
        .and_then(|entry| entry.loaded.as_deref())
        .or(disabled.as_deref());
    let mut summary = summary_for_installed(installed, loaded, status, &runtime.global_root());
    // Panels and links are the *active* surface, so they are projected from
    // the load pass that built it — including the effective `[plugins.<ns>]`
    // values a link template reads — rather than from the manifest alone.
    if let Some(entry) = registered.filter(|entry| entry.status == PluginStatus::Active)
        && let Some(plugin) = entry.loaded.as_ref()
    {
        let (panels, links) = web_summaries(plugin, installed.first_party, &entry.config_values);
        summary.panels = panels;
        summary.links = links;
    }
    // A row whose grants this host could not verify granted nothing, so the
    // report says so rather than repeating the row's claim back as authority —
    // otherwise `orbit plugin show` would print `unsandboxed` for a plugin the
    // loader refused to run at all [ORB-12778].
    if registered.is_some_and(|entry| !entry.grants_authorized) {
        summary.granted.clear();
        summary.unsandboxed = false;
        for permission in &mut summary.permissions {
            permission.granted = false;
            permission.granted_roots = None;
        }
    }
    summary.diagnostic = registered.and_then(|entry| entry.diagnostic.clone());
    if summary.diagnostic.is_none() && status == PluginStatus::Inactive && loaded.is_none() {
        summary.diagnostic = Some(format!(
            "plugin '{}' no longer loads from {}; reinstall it with `orbit plugin add`",
            installed.name, installed.install_path
        ));
    }
    summary
}

fn runtime_diagnostic(runtime: &OrbitRuntime, name: &str) -> Option<String> {
    runtime
        .plugin_load()
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.plugin == name)
        .map(|diagnostic| diagnostic.message.clone())
}

/// Shared projection of one installed plugin plus, when it loads, its manifest.
pub(super) fn summary_for_installed(
    installed: &InstalledPlugin,
    plugin: Option<&LoadedPlugin>,
    status: PluginStatus,
    global_root: &Path,
) -> PluginSummary {
    let (panels, links) = plugin
        .filter(|_| status == PluginStatus::Active)
        .map(|plugin| web_summaries(plugin, installed.first_party, &BTreeMap::new()))
        .unwrap_or_default();
    let tools = plugin
        .map(|plugin| {
            plugin
                .tools
                .iter()
                .map(|tool| {
                    let name =
                        plugin_tool_name(plugin.namespace(), &tool.verb, installed.first_party);
                    let advertised = match tool.mcp_scope {
                        orbit_types::plugin::PluginMcpScope::None => None,
                        _ => Some(orbit_types::tool::mcp_advertised_tool_name(&name)),
                    };
                    PluginToolSummary {
                        name,
                        advertised_name: advertised,
                        execution_kind: tool.execution_kind,
                        mcp_scope: mcp_scope_label(tool.mcp_scope).to_string(),
                        active: status == PluginStatus::Active,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    PluginSummary {
        name: installed.name.clone(),
        version: installed.version.clone(),
        status,
        source: installed.source.clone(),
        install_path: installed.install_path.clone(),
        manifest_digest: installed.manifest_digest.clone(),
        publisher: plugin.and_then(|plugin| plugin.manifest.metadata.publisher.clone()),
        description: plugin
            .map(|plugin| plugin.manifest.metadata.description.clone())
            .unwrap_or_default(),
        first_party: installed.first_party,
        permissions: plugin
            .map(|plugin| permission_rows(plugin, &installed.grants))
            .unwrap_or_default(),
        granted: installed.grants.clone(),
        unsandboxed: plugin.is_some_and(|plugin| {
            plugin.manifest.spec.backend.sandbox == PluginSandbox::None
                && installed
                    .grants
                    .iter()
                    .any(|grant| grant == PluginGrant::Unsandboxed.as_str())
        }),
        programs: plugin
            .map(|plugin| {
                program_statuses(
                    &plugin.manifest.spec.requires.programs,
                    &recorded_program_paths(global_root, &installed.name),
                    global_root,
                    &plugin_state_dir(global_root, &installed.name),
                )
            })
            .unwrap_or_default(),
        tools,
        panels,
        links,
        certified_orbit_version: installed.certified_orbit_version.clone(),
        diagnostic: None,
        pinned: false,
    }
}

fn mcp_scope_label(scope: orbit_types::plugin::PluginMcpScope) -> &'static str {
    match scope {
        orbit_types::plugin::PluginMcpScope::Workspace => "workspace",
        orbit_types::plugin::PluginMcpScope::Global => "global",
        orbit_types::plugin::PluginMcpScope::None => "none",
    }
}

/// Requested versus granted, one row per grant, for `orbit plugin show`.
///
/// A row this build cannot parse reports nothing as granted: the loader
/// refuses it for the same reason, and repeating an unreadable claim back as
/// authority is what [ORB-12778] closed.
fn permission_rows(plugin: &LoadedPlugin, granted: &[String]) -> Vec<PluginPermissionSummary> {
    let granted = parse_stored_grants(granted).unwrap_or_default();
    plugin
        .manifest
        .grant_requests()
        .into_iter()
        .map(|request| PluginPermissionSummary {
            granted: granted.contains(request.grant),
            granted_roots: granted
                .entry(request.grant)
                .and_then(|entry| entry.roots.clone()),
            grant: request.grant,
            requested: request.requested,
        })
        .collect()
}

fn pinned_names(runtime: &OrbitRuntime) -> Vec<String> {
    read_pin_file(&runtime.shared_root())
        .ok()
        .flatten()
        .map(|pins| {
            pins.plugins
                .into_iter()
                .map(|pin| pin.name)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}
