//! The install and upgrade flow: resolve, check, stage, publish and record.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::plugin::{
    PluginSourceRequest, PluginValidationPolicy, first_party_source, load_plugin_dir,
    manifest_refusal, resolve_plugin_source, validate_loaded_plugin,
};
use orbit_types::plugin::{InstalledPlugin, PluginStatus, SemverRange, Version};
use orbit_types::record::OrbitEvent;

use crate::OrbitRuntime;
use crate::runtime::plugin::build_witness::record_build_witness;
use crate::runtime::plugin::grants::{
    record_authorization, record_authorized_grants, verify_install_path,
};
use crate::runtime::plugin::host::projected_status;
use crate::runtime::plugin::paths::plugin_install_path;

use super::super::inspect::{PluginSummary, summary_for_installed};
use super::super::lifecycle::{resolve_consented_programs, unrequested_grant_warnings};
use super::super::secrets::{prune_undeclared_secrets, unset_secret_warnings};

use super::permissions::{
    fs_compare_layout, fs_request_references_config, permission_diff, permission_widening_message,
};
use super::staging::{
    StagedInstall, copy_tree, lock_plugin_namespace, prune_namespace, refuse_in_repository_source,
};
use super::{PluginAddOptions, PluginInstallOutcome, PluginUpgradeOptions, PluginUpgradeResult};

/// Install `source` for this host: a local directory, a `git+<url>#<ref>`
/// reference, a local archive, or a digest-pinned `https://` archive.
pub fn install_plugin(
    runtime: &OrbitRuntime,
    source: &str,
    options: &PluginAddOptions,
) -> Result<PluginSummary, OrbitError> {
    install_plugin_inner(runtime, source, options, None, FLAG_DIGEST_ORIGIN)
        .map(|outcome| outcome.summary)
}

/// Same install as [`install_plugin`], but keeping the enable-time report
/// (seeded schedules, linked skills, warnings) that `--enable` produced, so
/// the adapter boundary can render `add --enable` the same way `orbit plugin
/// enable` does instead of collapsing it into the install summary.
pub(crate) fn install_plugin_reporting_enable(
    runtime: &OrbitRuntime,
    source: &str,
    options: &PluginAddOptions,
) -> Result<PluginInstallOutcome, OrbitError> {
    install_plugin_inner(runtime, source, options, None, FLAG_DIGEST_ORIGIN)
}

/// Where a digest came from when the operator passed one directly, named in
/// the refusal an unpinned or mismatching archive produces.
const FLAG_DIGEST_ORIGIN: &str = "the `--digest` option";

struct ExpectedPluginIdentity<'a> {
    name: &'a str,
    version: Option<&'a str>,
    /// An upgrade replaces a recorded install; it must not turn into a fresh
    /// one because a `remove` landed between its first read and the lock.
    installed: bool,
}

/// Install a workspace pin only when its declared identity matches the source
/// manifest. The checks happen before the install tree or host row is written,
/// so a mismatching pin is refused the same way on every sync attempt.
pub(in crate::application::plugin) fn install_pinned_plugin(
    runtime: &OrbitRuntime,
    expected_name: &str,
    expected_version: Option<&str>,
    source: &str,
    options: &PluginAddOptions,
) -> Result<PluginSummary, OrbitError> {
    install_plugin_inner(
        runtime,
        source,
        options,
        Some(ExpectedPluginIdentity {
            name: expected_name,
            version: expected_version,
            installed: false,
        }),
        &format!("the `.orbit/plugins.yaml` pin for '{expected_name}'"),
    )
    .map(|outcome| outcome.summary)
}

/// Replace an installed namespace from an explicit, non-empty source. The
/// recorded source is informational: a plugin with `orbit_tools` can rewrite
/// the database, and the host-owned grant witness does not bind that field.
/// An explicit grant list is re-consent for the new manifest and enables it;
/// otherwise the same widening rules as plain `add` apply.
pub fn upgrade_plugin(
    runtime: &OrbitRuntime,
    name: &str,
    source: Option<&str>,
    options: &PluginUpgradeOptions,
) -> Result<PluginUpgradeResult, OrbitError> {
    runtime
        .stores()
        .plugins()
        .get_plugin(name)?
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "plugin '{name}' is not installed on this host; run `orbit plugin add <source>` first"
            ))
        })?;
    let source = source
        .filter(|source| !source.trim().is_empty())
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "plugin '{name}' requires an explicit upgrade source; the recorded source is not \
                 trusted; run `orbit plugin upgrade {name} <source>`"
            ))
        })?;
    let add_options = PluginAddOptions {
        force: true,
        digest: options.digest.clone(),
        enable: !options.grants.is_empty(),
        grants: options.grants.clone(),
        allow_build: options.allow_build,
        show_build_plan: options.show_build_plan,
    };
    let outcome = install_plugin_inner(
        runtime,
        source,
        &add_options,
        Some(ExpectedPluginIdentity {
            name,
            version: None,
            installed: true,
        }),
        FLAG_DIGEST_ORIGIN,
    )?;
    Ok(PluginUpgradeResult {
        summary: outcome.summary,
        permission_changes: outcome.permission_changes,
        grants_reset: outcome.grants_reset,
    })
}

fn install_plugin_inner(
    runtime: &OrbitRuntime,
    source: &str,
    options: &PluginAddOptions,
    expected_identity: Option<ExpectedPluginIdentity<'_>>,
    digest_origin: &str,
) -> Result<PluginInstallOutcome, OrbitError> {
    if !options.enable && !options.grants.is_empty() {
        return Err(OrbitError::InvalidInput(
            "--grant requires --enable; grants are recorded only when the plugin is enabled"
                .to_string(),
        ));
    }
    let resolved = resolve_plugin_source(&PluginSourceRequest {
        source,
        expected_digest: options.digest.as_deref(),
        digest_origin,
    })?;
    let source_root = resolved.root.clone();
    refuse_in_repository_source(runtime, &source_root)?;

    let plugin = load_plugin_dir(&source_root)?;
    let first_party = plugin.manifest.claims_first_party_namespace() && first_party_source(source);
    let policy = PluginValidationPolicy::host_default().with_first_party_verified(first_party);
    validate_loaded_plugin(&plugin, &policy).map_err(manifest_refusal)?;

    let name = plugin.namespace().to_string();
    let version = plugin.manifest.metadata.version.clone();
    let requires_installed = expected_identity
        .as_ref()
        .is_some_and(|expected| expected.installed);
    if let Some(expected) = expected_identity {
        if name != expected.name {
            return Err(OrbitError::InvalidInput(format!(
                "source manifest declares plugin namespace '{name}', but the requested name is \
                 '{}'",
                expected.name
            )));
        }
        if let Some(requirement) = expected.version {
            let range = SemverRange::parse(requirement).map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "invalid pinned version requirement '{requirement}': {error}"
                ))
            })?;
            let parsed_version = version.parse::<Version>().map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "source manifest declares invalid plugin version '{version}': {error}"
                ))
            })?;
            if !range.matches(&parsed_version) {
                return Err(OrbitError::InvalidInput(format!(
                    "source manifest declares plugin '{name}' v{version}, which does not satisfy \
                     the pinned version requirement '{requirement}'"
                )));
            }
        }
    }
    let global_root = runtime.global_root();
    if plugin.manifest.spec.build.is_some()
        && !options.force
        && plugin_install_path(&global_root, &name, &version).exists()
    {
        return Err(OrbitError::InvalidInput(format!(
            "plugin '{name}' v{version} is already installed; pass --force to rebuild and \
             replace it"
        )));
    }
    // A build runs before the namespace lock, as source resolution does, so
    // a long build never blocks another lifecycle operation on the namespace.
    let prepared_build = super::super::build::prepare_build(
        runtime,
        &plugin,
        &resolved,
        source,
        options.allow_build,
        options.show_build_plan,
    )?;
    // Everything from reading the row to the last witness write is one
    // namespace transition. Declared before `staged`, so a rollback in its
    // `Drop` also runs under the lock.
    let _namespace_lock = lock_plugin_namespace(&global_root, &name)?;
    let existing = runtime.stores().plugins().get_plugin(&name)?;
    if requires_installed && existing.is_none() {
        return Err(OrbitError::InvalidInput(format!(
            "plugin '{name}' is not installed on this host; run `orbit plugin add <source>` first"
        )));
    }
    let manifest_changed = existing
        .as_ref()
        .is_some_and(|installed| installed.manifest_digest != plugin.manifest_digest);
    // The permission diff is read out of the tree the previous row records,
    // and reinstalling is the recovery the relocated-install refusal names, so
    // this is a path a tampered row reaches. A row pointing outside the
    // install root is not evidence about what the plugin previously asked
    // for: report it as an unreadable previous manifest, which resets carried
    // grants below, rather than diffing against a tree a backend could have
    // written itself [ORB-12800].
    // Loaded before the diff so a config-read failure aborts prior to publish.
    // Rendering runs only when an fs template actually names `{{config.*}}`.
    let compared = if manifest_changed {
        existing.as_ref().map(|installed| {
            verify_install_path(&global_root, installed).and_then(|()| {
                load_plugin_dir(Path::new(&installed.install_path))
                    .map_err(|error| error.to_string())
            })
        })
    } else {
        None
    };
    let fs_layout = match &compared {
        Some(Ok(previous))
            if fs_request_references_config(&previous.manifest)
                || fs_request_references_config(&plugin.manifest) =>
        {
            Some(fs_compare_layout(runtime, &global_root, &name, &version)?)
        }
        _ => None,
    };
    let (permission_changes, previous_manifest_error) = match compared {
        Some(Ok(previous)) => match permission_diff(&previous, &plugin, fs_layout.as_ref()) {
            Ok(changes) => (changes, None),
            Err(message) => (Vec::new(), Some(message)),
        },
        Some(Err(message)) => (Vec::new(), Some(message)),
        None => (Vec::new(), None),
    };
    let grants_reset = existing.as_ref().is_some_and(|installed| {
        !options.enable
            && !installed.grants.is_empty()
            && manifest_changed
            && (previous_manifest_error.is_some()
                || permission_changes.iter().any(|change| change.widened))
    });
    let install_path = plugin_install_path(&global_root, &name, &version);
    if install_path.exists() && !options.force {
        return Err(OrbitError::InvalidInput(format!(
            "plugin '{name}' v{version} is already installed at {}; pass --force to replace it",
            install_path.display()
        )));
    }
    let mut staged = StagedInstall::begin(&global_root, &name, &version)?;
    copy_tree(&source_root, staged.staging())?;
    let build = prepared_build
        .map(|prepared| {
            super::super::build::finish_build(runtime, &name, prepared, staged.staging())
        })
        .transpose()?;
    staged.publish()?;

    let enabled =
        !grants_reset && (options.enable || existing.as_ref().is_some_and(|plugin| plugin.enabled));
    let grants = if options.enable {
        orbit_types::plugin::parse_grants(&options.grants)?.to_recorded()
    } else if grants_reset {
        Vec::new()
    } else {
        existing
            .as_ref()
            .map(|plugin| plugin.grants.clone())
            .unwrap_or_default()
    };
    // A conformance run certifies one tree. Installing a different manifest
    // drops the claim rather than carrying it onto bytes no suite has run
    // against (§5).
    let certified_orbit_version = existing.as_ref().and_then(|installed| {
        (installed.manifest_digest == plugin.manifest_digest)
            .then(|| installed.certified_orbit_version.clone())
            .flatten()
    });
    let record = InstalledPlugin {
        name: name.clone(),
        version: version.clone(),
        source: source.to_string(),
        install_path: install_path.to_string_lossy().into_owned(),
        manifest_digest: plugin.manifest_digest.clone(),
        archive_digest: resolved.archive_digest.clone(),
        build,
        enabled,
        grants,
        first_party,
        certified_orbit_version,
        // The store keeps the original `installed_at`; these are the values a
        // fresh row takes.
        installed_at: String::new(),
        updated_at: String::new(),
    };
    // Revoke the old authorization witness before replacing the row. If the
    // database write then fails, the old enabled row fails closed rather than
    // leaving a witness that a database writer could replay onto the new
    // manifest and its wider request.
    if grants_reset {
        record_authorized_grants(&global_root, &name, false, &[])?;
    }
    // Likewise the build witness: written first, so a failed row write leaves
    // the old row disagreeing with it and refused at load.
    record_build_witness(&global_root, &name, record.build.as_ref())?;
    runtime.with_mutation(|| {
        runtime.stores().plugins().upsert_plugin(&record)?;
        Ok((
            (),
            OrbitEvent::PluginInstalled {
                name: name.clone(),
                version: version.clone(),
            },
        ))
    })?;
    // The row names the new tree from here on, so the replaced one may go and
    // the staged swap must not roll back. Anything that fails below leaves an
    // install that landed, which is what the row says.
    staged.commit();
    prune_namespace(&global_root, &name, &install_path);
    // An upgrade keeps each secret the new manifest still declares, value
    // and version intact, and drops the rest.
    prune_undeclared_secrets(&global_root, &plugin);
    // `--enable` (including upgrade re-consent) is the operator authorizing
    // this grant set, so it records the integrity value the loader checks the
    // row back against. An ordinary plain `add` deliberately does not rewrite
    // a witness for carried grants: doing so would authorize a set an
    // `orbit.db` writer could have put there. The widening branch above writes
    // only the disabled/empty revocation witness [ORB-12778].
    //
    // The same consent resolves `requires.programs` into the paths the
    // sandbox grants, exactly as `orbit plugin enable` does.
    let mut program_warnings = Vec::new();
    if options.enable {
        let (programs, warnings) = resolve_consented_programs(&global_root, &plugin);
        program_warnings = warnings;
        record_authorization(&global_root, &name, enabled, &record.grants, &programs)?;
    }

    // `--enable` is an enable: the plugin's schedules are seeded and its
    // skills linked here too, so a one-step install leaves the same state as
    // `add` followed by `enable`. A plugin whose definitions break the §4.5
    // rules contributes nothing and is reported inactive — the refusal belongs
    // to the load, which states it on every later command, so it is not raised
    // as this command's error and the install record stands.
    let mut seeded_outcomes = Vec::new();
    let mut skill_links = Vec::new();
    let mut enable_warnings = Vec::new();
    let contributions_refused = if enabled {
        // `--force` on `add` replaces an install of the same version; it is
        // deliberately not an answer about a definition the operator edited.
        // Overwriting one of those stays `orbit plugin enable <ns> --force`.
        match super::super::lifecycle::apply_enabled_contributions(runtime, &install_path, false) {
            Ok(contributions) => {
                // Same report `orbit plugin enable` returns, so the two ways
                // of enabling render identically rather than this path
                // silently dropping it into the install summary.
                enable_warnings = unrequested_grant_warnings(&plugin, &record.grants);
                enable_warnings.extend(contributions.warnings);
                enable_warnings.append(&mut program_warnings);
                enable_warnings.extend(unset_secret_warnings(&global_root, &plugin));
                seeded_outcomes = contributions.seeded;
                skill_links = contributions.skills;
                None
            }
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.plugin",
                    plugin = %name,
                    "installed, but its definitions and skills were not applied: {error}",
                );
                Some(error.to_string())
            }
        }
    } else {
        None
    };

    // Report the status the next runtime will register it with, so `add
    // --enable` on an incompatible host says so now rather than at first use.
    let stored = runtime
        .stores()
        .plugins()
        .get_plugin(&name)?
        .unwrap_or(record);
    let projection = if enabled {
        let config = orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::global_only(
            &global_root,
        ))?;
        Some(projected_status(
            &stored,
            &plugin,
            &global_root,
            &config.plugins,
        ))
    } else {
        None
    };
    let status = projection
        .as_ref()
        .map_or(PluginStatus::Disabled, |projection| projection.status);
    // Held until the copy above finished, so a fetched tree is not collected
    // out from under it.
    drop(resolved);
    let mut summary = summary_for_installed(&stored, Some(&plugin), status, &global_root);
    summary.diagnostic = if grants_reset {
        Some(permission_widening_message(
            &name,
            &plugin.manifest,
            &permission_changes,
            previous_manifest_error.as_deref(),
        ))
    } else if let Some(message) = contributions_refused {
        Some(message)
    } else {
        projection.and_then(|projection| projection.diagnostic)
    };
    Ok(PluginInstallOutcome {
        summary,
        permission_changes,
        grants_reset,
        seeded: seeded_outcomes,
        skills: skill_links,
        warnings: enable_warnings,
    })
}
