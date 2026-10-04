//! `orbit plugin doctor` and its drift and seed checks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_automation::auto_tasks::loader::AUTO_TASKS_DIR;
use orbit_automation::routines::loader::ROUTINES_DIR;
use orbit_common::OrbitError;
use orbit_tools::plugin::{
    PLUGIN_BUILD_DIR_PREFIX, installed_artifact_digest, is_live_plugin_build_dir,
};
use orbit_types::plugin::{
    PluginBuildRecord, PluginDisabledLayer, PluginStatus, SemverRange, Version,
    parse_archive_digest, remote_archive_source,
};

use super::summary::{PluginSummary, list_plugins};

use crate::OrbitRuntime;
use crate::application::plugin::lifecycle::build_pin_drift;
use crate::runtime::plugin::backend::plugin_backend;
use crate::runtime::plugin::paths::{plugin_install_root, read_pin_file};
use crate::runtime::plugin::requirements::host_api_deprecation;
use crate::runtime::plugin::sandbox_mask::plugin_trees_masked;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDoctorResult {
    pub plugin: String,
    pub status: PluginStatus,
    /// The step an operator has to take, or empty when there is none.
    pub message: String,
    /// The row reports an operator's deliberate choice — a plugin switched
    /// off in this workspace, or a build they consented to — not a problem.
    /// It carries a message so the state stays visible, but is not a finding.
    pub intentional: bool,
}

/// One row per plugin, naming the step that would make it active, or the
/// finding an active plugin carries (an unsandboxed backend).
pub fn plugin_doctor(runtime: &OrbitRuntime) -> Result<Vec<PluginDoctorResult>, OrbitError> {
    let summaries = list_plugins(runtime)?;
    let invalid_pin_file = read_pin_file(&runtime.paths().local_dir)
        .err()
        .map(|error| PluginDoctorResult {
            intentional: false,
            plugin: "pin file".to_string(),
            status: PluginStatus::Inactive,
            message: error.to_string(),
        });
    let stale_seeded = stale_seeded_definition_rows(runtime, &summaries)?;
    let archive_drift = archive_digest_drift_rows(runtime, &summaries)?;
    let builds = build_rows(runtime, &summaries)?;
    let scoped_out = scoped_out_fs_root_rows(runtime)?;
    let ungranted_programs = ungranted_program_rows(&summaries);
    let host_api_deprecated = host_api_deprecation_rows(runtime);
    // Inside an agent sandbox the secret store is masked: one row says so,
    // rather than one unreadable-store row per plugin or a false "not set".
    let unset_secrets = if plugin_trees_masked(&runtime.global_root()) {
        vec![PluginDoctorResult {
            intentional: true,
            plugin: "plugin state".to_string(),
            status: PluginStatus::Inactive,
            message: "plugin state and secrets are not visible from an agent sandbox; run \
                      `orbit plugin doctor` on the host to check them"
                .to_string(),
        }]
    } else {
        super::super::secrets::unset_secret_rows(runtime, &summaries)?
    };
    // A skill link whose target is gone is invisible to the skill catalog's
    // own doctor — it only walks seeded trees — and to the plugin record,
    // which says nothing about the provider discovery roots (§3).
    let mut dangling = Vec::new();
    for summary in &summaries {
        if summary.install_path.is_empty() {
            continue;
        }
        for (link, target) in super::super::skills::dangling_plugin_skill_links(
            &runtime.global_root(),
            Path::new(&summary.install_path),
        ) {
            dangling.push(PluginDoctorResult {
                intentional: false,
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
                    PluginStatus::Disabled
                        if summary.disabled_by == Some(PluginDisabledLayer::Workspace) =>
                    {
                        format!(
                            "plugin '{}' is enabled on this host but switched off in this \
                             workspace; run `orbit plugin enable {} --scope workspace` to turn \
                             it back on here",
                            summary.name, summary.name
                        )
                    }
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
                intentional: summary.status == PluginStatus::Disabled
                    && summary.disabled_by == Some(PluginDisabledLayer::Workspace),
                plugin: summary.name,
                status: summary.status,
                message,
            }
        })
        .collect();
    rows.extend(dangling);
    rows.extend(stale_seeded);
    rows.extend(archive_drift);
    rows.extend(builds);
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
                    intentional: false,
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
                intentional: false,
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
                intentional: false,
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
            intentional: false,
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

/// The source-built plugin rows alone, for the plugin section of
/// `orbit doctor`.
pub fn plugin_build_doctor(runtime: &OrbitRuntime) -> Result<Vec<PluginDoctorResult>, OrbitError> {
    build_rows(runtime, &list_plugins(runtime)?)
}

/// One informational row per plugin built on this host, and the findings
/// about those builds (`docs/design/plugins/3_install_time_build.md` §3.9).
/// Offline: nothing here runs a build, contacts a source or re-fetches.
///
/// A build record that disagrees with its witness is not repeated here: the
/// row registers inactive at load, and its plugin row already says why.
fn build_rows(
    runtime: &OrbitRuntime,
    summaries: &[PluginSummary],
) -> Result<Vec<PluginDoctorResult>, OrbitError> {
    // A pin file that does not parse is already its own doctor row.
    let pins = read_pin_file(&runtime.shared_root())
        .ok()
        .flatten()
        .map(|file| file.plugins)
        .unwrap_or_default();
    let mut rows = Vec::new();
    for summary in summaries {
        let name = &summary.name;
        let row = |intentional: bool, message: String| PluginDoctorResult {
            plugin: name.clone(),
            status: summary.status,
            message,
            intentional,
        };
        if let Some(build) = &summary.build {
            rows.push(row(true, describe_build(build)));
            if let Err(detail) =
                installed_artifact_digest(Path::new(&summary.install_path), &build.outputs)
                    .and_then(|current| {
                        if current == build.artifact_digest {
                            Ok(())
                        } else {
                            Err(format!(
                                "the installed outputs now hash to {current}, not the recorded {}",
                                build.artifact_digest
                            ))
                        }
                    })
            {
                rows.push(row(
                    false,
                    format!(
                        "plugin '{name}' was modified after its build: {detail}; reinstall it \
                         with `orbit plugin upgrade {name} {} --allow-build`",
                        build.source
                    ),
                ));
            }
            let gone = build
                .programs
                .iter()
                .map(|program| program.path.as_str())
                .chain(build.toolchain_roots.iter().map(String::as_str))
                .filter(|path| !Path::new(path).exists())
                .collect::<Vec<_>>();
            if !gone.is_empty() {
                rows.push(row(
                    true,
                    format!(
                        "plugin '{name}' was built with {}, which no longer exist; the installed \
                         plugin is unaffected, but its next build will be refused until they do",
                        gone.join(", ")
                    ),
                ));
            }
        }
        if let Some(pin) = pins.iter().find(|pin| pin.name == *name)
            && let Some(installed) = runtime.stores().plugins().get_plugin(name)?
            && let Some(drift) = build_pin_drift(pin, &installed)
        {
            rows.push(row(false, format!("plugin '{name}': {drift}")));
        }
    }
    rows.extend(leftover_build_dir_rows(&runtime.global_root()));
    Ok(rows)
}

/// The consent and provenance of one build, in one line.
fn describe_build(build: &PluginBuildRecord) -> String {
    let fetch = build
        .fetch
        .as_ref()
        .map(|argv| format!("fetch `{}`, ", argv.join(" ")))
        .unwrap_or_default();
    let landlock = build
        .landlock_abi
        .map(|abi| format!(", Landlock ABI {abi}"))
        .unwrap_or_default();
    format!(
        "built on this host from {} at commit {}: {fetch}command `{}`; profile {}{landlock}; \
         consented with {} at {} by {} (Orbit {}); artifact digest {}",
        build.source,
        build.commit,
        build.command.join(" "),
        build.profile,
        build.consent.flag,
        build.consent.at,
        build.consent.os_user,
        build.consent.orbit_version,
        build.artifact_digest
    )
}

/// A `.build-*` directory under a plugin namespace that no live build owns:
/// what a killed `orbit plugin add --allow-build` left behind.
fn leftover_build_dir_rows(global_root: &Path) -> Vec<PluginDoctorResult> {
    let Ok(namespaces) = std::fs::read_dir(plugin_install_root(global_root)) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for namespace in namespaces.flatten() {
        let name = namespace.file_name();
        if name.to_string_lossy().starts_with('.')
            || !namespace.file_type().is_ok_and(|kind| kind.is_dir())
        {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(namespace.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let entry_name = entry.file_name();
            if !entry_name
                .to_string_lossy()
                .starts_with(PLUGIN_BUILD_DIR_PREFIX)
                || is_live_plugin_build_dir(&entry_name)
            {
                continue;
            }
            rows.push(PluginDoctorResult {
                plugin: name.to_string_lossy().into_owned(),
                status: PluginStatus::Inactive,
                message: format!(
                    "'{}' is a build directory no running build owns; the next install or \
                     upgrade of this plugin removes it, or delete it by hand",
                    entry.path().display()
                ),
                intentional: false,
            });
        }
    }
    rows
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
                intentional: false,
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
