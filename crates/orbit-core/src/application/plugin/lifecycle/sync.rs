//! `orbit plugin sync`: converge the workspace on `.orbit/plugins.yaml`.

use orbit_common::OrbitError;
use orbit_types::plugin::{
    InstalledPlugin, PluginGrantEntry, PluginGrantSet, PluginPin, PluginStatus, git_commit_source,
    parse_archive_digest, parse_grants,
};

use crate::OrbitRuntime;
use crate::runtime::plugin::build_witness::verify_build_record;
use crate::runtime::plugin::cache::load_installed_plugin;
use crate::runtime::plugin::grants::{verify_install_path, verify_recorded_grants};
use crate::runtime::plugin::host::projected_status;
use crate::runtime::plugin::paths::read_pin_file;

use super::super::inspect::show_plugin;
use super::super::seed::PluginSeedOutcome;
use super::enable::{
    PluginEnableOptions, PluginEnableResult, apply_enabled_contributions, enable_plugin,
};
use super::record::verified_install_path;
use super::workspace::{
    disable_plugin_in_workspace, enable_plugin_in_workspace, workspace_plugin_toggles,
    write_workspace_toggle,
};

/// How an installed build differs from its pin: a different commit, or an
/// artifact digest the install does not record. Offline: compares the row
/// with the pin and nothing else.
pub(crate) fn build_pin_drift(pin: &PluginPin, installed: &InstalledPlugin) -> Option<String> {
    let name = &pin.name;
    let pinned_commit = pin
        .source
        .as_deref()
        .and_then(git_commit_source)
        .map(|(_, commit)| commit);
    let pinned_digest = pin
        .artifact_digest
        .as_deref()
        .and_then(|digest| parse_archive_digest(digest).ok())
        .map(|hex| format!("sha256:{hex}"));
    let upgrade = format!(
        "`orbit plugin upgrade {name} {} --allow-build`",
        pin.source.as_deref().unwrap_or("<source>")
    );
    match &installed.build {
        Some(build) => {
            if let Some(commit) = pinned_commit.filter(|commit| *commit != build.commit) {
                return Some(format!(
                    "installed build of commit {} does not match the pinned commit {commit}; \
                     rebuild it with {upgrade}",
                    build.commit
                ));
            }
            pinned_digest
                .filter(|digest| *digest != build.artifact_digest)
                .map(|digest| {
                    format!(
                        "installed build has artifact digest {}, but the pin names {digest}; \
                         rebuild it with {upgrade}",
                        build.artifact_digest
                    )
                })
        }
        None => pinned_digest.map(|digest| {
            format!(
                "the pin names artifact digest {digest}, but this install records no build; \
                 rebuild it with {upgrade}"
            )
        }),
    }
}

/// What `orbit plugin sync` found for one pinned plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSyncOutcome {
    pub name: String,
    pub status: PluginStatus,
    /// What sync did, or what the operator has to do.
    pub message: String,
}

/// Read `.orbit/plugins.yaml` and converge the current workspace on it.
///
/// A pin's `enabled:` applies to this workspace's `[plugin_enablement]`
/// toggle: `false` switches the plugin off here and never touches the host
/// row or another workspace; `true` clears a `false` toggle. `true` for a
/// plugin the host has disabled still takes the grant-reviewed host enable —
/// a grant-requesting plugin is enabled only when this invocation supplies
/// explicit grant consent.
pub fn sync_plugins(
    runtime: &OrbitRuntime,
    dry_run: bool,
    grants: &[String],
) -> Result<Vec<PluginSyncOutcome>, OrbitError> {
    // Kept as the parsed set, not a name list: a `--grant fs=<root>` consent
    // has to carry its roots through to the row each pin records.
    let grants = parse_grants(grants).map_err(OrbitError::InvalidInput)?;
    let Some(pins) = read_pin_file(&runtime.shared_root())? else {
        return Ok(Vec::new());
    };
    // Pins act on this workspace's toggles; read them once, as they are on
    // disk, rather than from the runtime's build-time snapshot.
    let toggles = workspace_plugin_toggles(runtime)?;
    let mut outcomes = Vec::new();
    // Pins whose installed build does not satisfy them; the final projection
    // must keep that visible rather than report the host row as `Active`.
    let mut unsatisfied = std::collections::HashSet::new();
    for pin in &pins.plugins {
        let installed = runtime.stores().plugins().get_plugin(&pin.name)?;
        let drift = installed
            .as_ref()
            .and_then(|installed| build_pin_drift(pin, installed));
        match (installed, drift) {
            // §3.7: an installed build that differs from what the pin
            // expects is unsatisfied here, and is neither enabled, toggled on
            // nor seeded. A differing pin never causes a rebuild. A pin's
            // `enabled: false` is the safe direction, so it still applies.
            (Some(_), Some(drift)) => {
                unsatisfied.insert(pin.name.clone());
                let message = if pin.enabled {
                    drift
                } else {
                    let switched_off = if toggles.get(&pin.name) == Some(&false) {
                        "switched off in this workspace".to_string()
                    } else if dry_run {
                        "would switch off in this workspace".to_string()
                    } else {
                        match disable_plugin_in_workspace(runtime, &pin.name) {
                            Ok(_) => "switched off in this workspace by the pin".to_string(),
                            Err(error) => format!(
                                "cannot switch off in this workspace to match the pin: {error}"
                            ),
                        }
                    };
                    format!("{drift}; {switched_off}")
                };
                outcomes.push(PluginSyncOutcome {
                    name: pin.name.clone(),
                    status: PluginStatus::Inactive,
                    message,
                });
            }
            (Some(installed), None) => {
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
                let toggle = toggles.get(&pin.name).copied();
                let (status, action_message) = if dry_run {
                    let message = match (pin.enabled, installed.enabled, toggle) {
                        (false, _, Some(false)) => "switched off in this workspace".to_string(),
                        (false, _, _) => "would switch off in this workspace".to_string(),
                        (true, false, _) => "would enable after grant review".to_string(),
                        (true, true, Some(false)) => "would switch back on in this workspace \
                                                      and reconcile workspace contributions"
                            .to_string(),
                        (true, true, _) => "would reconcile workspace contributions".to_string(),
                    };
                    (
                        if installed.enabled && toggle != Some(false) {
                            PluginStatus::Active
                        } else {
                            PluginStatus::Disabled
                        },
                        message,
                    )
                } else if !pin.enabled {
                    // A pin is workspace content, so `enabled: false` switches
                    // the plugin off here and nowhere else: never the host
                    // row, never another workspace.
                    if toggle == Some(false) {
                        (
                            PluginStatus::Disabled,
                            format!(
                                "installed v{} (switched off in this workspace)",
                                installed.version
                            ),
                        )
                    } else {
                        match disable_plugin_in_workspace(runtime, &pin.name) {
                            Ok(_) => (
                                PluginStatus::Disabled,
                                "switched off in this workspace by the pin".to_string(),
                            ),
                            Err(error) => (
                                if installed.enabled {
                                    PluginStatus::Active
                                } else {
                                    PluginStatus::Disabled
                                },
                                format!(
                                    "cannot switch off in this workspace to match the pin: \
                                     {error}"
                                ),
                            ),
                        }
                    }
                } else if !installed.enabled {
                    match enable_for_sync(runtime, &pin.name, &grants) {
                        Ok(SyncEnable::Enabled(result)) => {
                            let (status, reopened) = reopen_workspace_toggle(
                                runtime,
                                &pin.name,
                                toggle,
                                result.summary.status,
                            );
                            (
                                status,
                                describe_seeded(
                                    format!("enabled installed v{}{reopened}", installed.version),
                                    &result.seeded,
                                ),
                            )
                        }
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
                } else if toggle == Some(false) {
                    match enable_plugin_in_workspace(runtime, &pin.name, false) {
                        Ok(result) => (
                            result.summary.status,
                            describe_seeded(
                                format!(
                                    "installed v{}; switched back on in this workspace",
                                    installed.version
                                ),
                                &result.seeded,
                            ),
                        ),
                        Err(error) => (
                            PluginStatus::Disabled,
                            format!(
                                "installed v{}, but could not be switched back on in this \
                                 workspace: {error}",
                                installed.version
                            ),
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
            (None, _) => {
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
                let options = super::super::install::PluginAddOptions {
                    force: false,
                    // The pin file is the only place an archive's digest is
                    // declared, and the resolver refuses a fetched archive
                    // that carries none.
                    digest: pin.digest.clone(),
                    // A pin is not grant consent. Install disabled,
                    // then take the same reviewed enable path used above.
                    enable: false,
                    grants: Vec::new(),
                    // §3.7: a pin never starts a build, and sync
                    // has no flag that could.
                    allow_build: false,
                    show_build_plan: None,
                };
                match super::super::install::install_pinned_plugin(
                    runtime,
                    &pin.name,
                    pin.version.as_deref(),
                    &source,
                    &options,
                ) {
                    Ok(summary) if pin.enabled => {
                        // §3.7: the fresh install is held to the same build
                        // check as an existing one before anything enables it.
                        let drift = runtime
                            .stores()
                            .plugins()
                            .get_plugin(&pin.name)?
                            .and_then(|installed| build_pin_drift(pin, &installed));
                        if let Some(drift) = drift {
                            unsatisfied.insert(pin.name.clone());
                            outcomes.push(PluginSyncOutcome {
                                name: pin.name.clone(),
                                status: PluginStatus::Inactive,
                                message: format!(
                                    "installed v{} from {source}, but left disabled; {drift}",
                                    summary.version
                                ),
                            });
                            continue;
                        }
                        let (status, message) = match enable_for_sync(runtime, &pin.name, &grants) {
                            Ok(SyncEnable::Enabled(result)) => {
                                let (status, reopened) = reopen_workspace_toggle(
                                    runtime,
                                    &pin.name,
                                    toggles.get(&pin.name).copied(),
                                    result.summary.status,
                                );
                                (
                                    status,
                                    describe_seeded(
                                        format!(
                                            "installed v{} from {source}{reopened}",
                                            summary.version
                                        ),
                                        &result.seeded,
                                    ),
                                )
                            }
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
                    Ok(summary) => {
                        // Installed disabled on the host; the pin's `false`
                        // is also recorded as this workspace's toggle, so a
                        // later host enable does not switch it on here.
                        let message = match disable_plugin_in_workspace(runtime, &pin.name) {
                            Ok(_) => format!(
                                "installed v{} from {source} (switched off in this workspace by \
                                 the pin)",
                                summary.version
                            ),
                            Err(error) => format!(
                                "installed v{} from {source} (disabled), but the pin could not \
                                 switch it off in this workspace: {error}",
                                summary.version
                            ),
                        };
                        outcomes.push(PluginSyncOutcome {
                            name: pin.name.clone(),
                            status: PluginStatus::Disabled,
                            message,
                        });
                    }
                    Err(OrbitError::PluginBuildConsentRequired(_)) => {
                        outcomes.push(PluginSyncOutcome {
                            name: pin.name.clone(),
                            status: PluginStatus::Missing,
                            message: format!(
                                "not installed: {source} builds at install time, and a pin never \
                                 starts a build; review the plan and consent with `orbit plugin \
                                 add {source} --allow-build`"
                            ),
                        });
                    }
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
    // This runtime predates the writes above. Project from the final stored
    // host row and workspace config, using the loader's eligibility decision
    // rather than the success of seeding or a pre-toggle enable summary.
    for outcome in &mut outcomes {
        let Some(installed) = runtime.stores().plugins().get_plugin(&outcome.name)? else {
            continue;
        };
        let (mut status, diagnostic) = sync_effective_status(runtime, &installed)?;
        if unsatisfied.contains(&outcome.name) {
            // Unsatisfied is not active, and an unsatisfied entry the pin
            // wants enabled stays unsatisfied even though its row is disabled.
            let pin_enabled = pins
                .plugins
                .iter()
                .any(|pin| pin.name == outcome.name && pin.enabled);
            if status == PluginStatus::Active || pin_enabled {
                status = PluginStatus::Inactive;
            }
        }
        outcome.status = status;
        if let Some(diagnostic) = diagnostic {
            outcome.message.push_str("; ");
            outcome.message.push_str(&diagnostic);
        }
    }
    Ok(outcomes)
}

/// Read the final state as the next workspace load will see it. The loader
/// verifies an enabled row before applying the workspace toggle, then checks
/// the loaded manifest against the effective plugin config.
fn sync_effective_status(
    runtime: &OrbitRuntime,
    installed: &InstalledPlugin,
) -> Result<(PluginStatus, Option<String>), OrbitError> {
    if !installed.enabled {
        return Ok((PluginStatus::Disabled, None));
    }
    let global_root = runtime.global_root();
    if let Err(diagnostic) = verify_recorded_grants(&global_root, installed) {
        return Ok((PluginStatus::Inactive, Some(diagnostic)));
    }
    if let Err(diagnostic) = verify_install_path(&global_root, installed) {
        return Ok((PluginStatus::Inactive, Some(diagnostic)));
    }
    if let Err(diagnostic) = verify_build_record(&global_root, installed) {
        return Ok((PluginStatus::Inactive, Some(diagnostic)));
    }
    if workspace_plugin_toggles(runtime)?.get(&installed.name) == Some(&false) {
        return Ok((PluginStatus::Disabled, None));
    }
    let plugin = match load_installed_plugin(installed) {
        Ok(plugin) => plugin,
        Err(error) => {
            return Ok((
                PluginStatus::Inactive,
                Some(format!(
                    "plugin '{}' no longer loads from {}: {error}; reinstall it with `orbit plugin add`",
                    installed.name, installed.install_path
                )),
            ));
        }
    };
    let config = orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::new(
        global_root,
        runtime.shared_root(),
    ))?;
    let projection = projected_status(installed, &plugin, &runtime.global_root(), &config.plugins);
    Ok((projection.status, projection.diagnostic))
}

/// After sync enabled the host row for a pin that says `enabled: true`, clear
/// a `false` workspace toggle too, so the pin's intent holds here. The status
/// is provisional until sync projects the final stored state for all pins.
fn reopen_workspace_toggle(
    runtime: &OrbitRuntime,
    name: &str,
    toggle: Option<bool>,
    status: PluginStatus,
) -> (PluginStatus, &'static str) {
    if toggle != Some(false) {
        return (status, "");
    }
    let path = runtime.shared_root().join("config.toml");
    match write_workspace_toggle(&path, &runtime.global_root(), name, true) {
        Ok(()) => (status, "; switched back on in this workspace"),
        Err(error) => {
            tracing::warn!(
                target: "orbit.core.plugin",
                plugin = %name,
                "sync could not switch the plugin back on in this workspace: {error}"
            );
            (
                PluginStatus::Disabled,
                "; still switched off in this workspace (the toggle could not be written)",
            )
        }
    }
}

enum SyncEnable {
    Enabled(Box<PluginEnableResult>),
    NeedsGrant(Vec<String>),
}

fn enable_for_sync(
    runtime: &OrbitRuntime,
    name: &str,
    grants: &PluginGrantSet,
) -> Result<SyncEnable, OrbitError> {
    let summary = show_plugin(runtime, name)?;
    let requested = summary
        .permissions
        .iter()
        .filter(|permission| permission.requested.is_some())
        .map(|permission| permission.grant)
        .collect::<Vec<_>>();
    if requested.iter().any(|grant| !grants.contains(*grant)) {
        return Ok(SyncEnable::NeedsGrant(
            requested.iter().map(|grant| grant.to_string()).collect(),
        ));
    }
    // One sync can consent to the union needed by several pins. Each plugin
    // records only the grants its own manifest requested, never the union —
    // and records each one exactly as it was consented to, so a scoped `fs`
    // reaches the row with its roots rather than as the whole request.
    let plugin_grants = requested
        .iter()
        .filter_map(|grant| grants.entry(*grant))
        .map(PluginGrantEntry::to_recorded)
        .collect::<Vec<_>>();
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
