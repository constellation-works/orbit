//! `orbit plugin add`: resolve a source, refuse an in-repository one, copy the
//! tree into the host install root, and record it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use orbit_common::OrbitError;
use orbit_tools::plugin::{
    LoadedPlugin, PluginValidationPolicy, first_party_source, load_plugin_dir, manifest_refusal,
    plugin_symlink_refusal, refuse_plugin_tree_symlinks, resolve_plugin_source,
    validate_loaded_plugin,
};
use orbit_types::plugin::{
    InstalledPlugin, PluginGrant, PluginManifest, PluginNetworkPermission, PluginStatus,
    SemverRange, Version,
};
use orbit_types::record::OrbitEvent;

use crate::OrbitRuntime;
use crate::runtime::plugin_grants::{record_authorized_grants, verify_install_path};
use crate::runtime::plugin_host::{plugin_install_path, plugin_namespace_dir, projected_status};

use super::inspect::{PluginSummary, summary_for_installed};
use super::lifecycle::unrequested_grant_warnings;
use super::seed::PluginSeedOutcome;
use super::skills::PluginSkillLink;

#[derive(Debug, Clone, Default)]
pub struct PluginAddOptions {
    /// Replace an existing install of the same namespace and version.
    pub force: bool,
    /// Enable the plugin as part of the install.
    pub enable: bool,
    /// Grants recorded when `enable` is set.
    pub grants: Vec<String>,
}

/// One requested-permission change between the installed and candidate
/// manifests. `widened` means carrying the old grant would authorize
/// something the operator did not previously review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPermissionChange {
    pub grant: PluginGrant,
    pub previous: Option<String>,
    pub requested: Option<String>,
    pub widened: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PluginUpgradeOptions {
    /// Complete grant set authorizing and enabling the upgraded manifest.
    /// Without it, a safe upgrade preserves the existing row; a widening
    /// disables the plugin and clears its grants.
    pub grants: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PluginUpgradeResult {
    pub summary: PluginSummary,
    pub permission_changes: Vec<PluginPermissionChange>,
    pub grants_reset: bool,
}

pub(crate) struct PluginInstallOutcome {
    pub(crate) summary: PluginSummary,
    permission_changes: Vec<PluginPermissionChange>,
    grants_reset: bool,
    /// Routines and auto-tasks seeded by `--enable`; empty otherwise.
    pub(crate) seeded: Vec<PluginSeedOutcome>,
    /// Skill links maintained by `--enable`; empty otherwise.
    pub(crate) skills: Vec<PluginSkillLink>,
    /// Non-fatal problems `--enable` surfaced; empty otherwise.
    pub(crate) warnings: Vec<String>,
}

/// Install `source` for this host: a local directory, a `git+<url>#<ref>`
/// reference, or a tar archive.
pub fn install_plugin(
    runtime: &OrbitRuntime,
    source: &str,
    options: &PluginAddOptions,
) -> Result<PluginSummary, OrbitError> {
    install_plugin_inner(runtime, source, options, None).map(|outcome| outcome.summary)
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
    install_plugin_inner(runtime, source, options, None)
}

struct ExpectedPluginIdentity<'a> {
    name: &'a str,
    version: Option<&'a str>,
}

/// Install a workspace pin only when its declared identity matches the source
/// manifest. The checks happen before the install tree or host row is written,
/// so a mismatching pin is refused the same way on every sync attempt.
pub(super) fn install_pinned_plugin(
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
        }),
    )
    .map(|outcome| outcome.summary)
}

/// Replace an installed namespace, using its recorded source when the caller
/// does not provide one. An explicit grant list is re-consent for the new
/// manifest and enables it; otherwise the same widening rules as plain `add`
/// apply.
pub fn upgrade_plugin(
    runtime: &OrbitRuntime,
    name: &str,
    source: Option<&str>,
    options: &PluginUpgradeOptions,
) -> Result<PluginUpgradeResult, OrbitError> {
    let existing = runtime
        .stores()
        .plugins()
        .get_plugin(name)?
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "plugin '{name}' is not installed on this host; run `orbit plugin add <source>` first"
            ))
        })?;
    let source = source.unwrap_or(&existing.source);
    if source.trim().is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "plugin '{name}' has no recorded source; pass one as `orbit plugin upgrade {name} <source>`"
        )));
    }
    let add_options = PluginAddOptions {
        force: true,
        enable: !options.grants.is_empty(),
        grants: options.grants.clone(),
    };
    let outcome = install_plugin_inner(
        runtime,
        source,
        &add_options,
        Some(ExpectedPluginIdentity {
            name,
            version: None,
        }),
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
) -> Result<PluginInstallOutcome, OrbitError> {
    if !options.enable && !options.grants.is_empty() {
        return Err(OrbitError::InvalidInput(
            "--grant requires --enable; grants are recorded only when the plugin is enabled"
                .to_string(),
        ));
    }
    let resolved = resolve_plugin_source(source)?;
    let source_root = resolved.root.clone();
    refuse_in_repository_source(runtime, &source_root)?;

    let plugin = load_plugin_dir(&source_root)?;
    let first_party = plugin.manifest.claims_first_party_namespace() && first_party_source(source);
    let policy = PluginValidationPolicy::host_default().with_first_party_verified(first_party);
    validate_loaded_plugin(&plugin, &policy).map_err(manifest_refusal)?;

    let name = plugin.namespace().to_string();
    let version = plugin.manifest.metadata.version.clone();
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
    let existing = runtime.stores().plugins().get_plugin(&name)?;
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
    let (permission_changes, previous_manifest_error) = if manifest_changed {
        match existing.as_ref().map(|installed| {
            verify_install_path(&global_root, installed).and_then(|()| {
                load_plugin_dir(Path::new(&installed.install_path))
                    .map(|previous| permission_diff(&previous, &plugin))
                    .map_err(|error| error.to_string())
            })
        }) {
            Some(Ok(changes)) => (changes, None),
            Some(Err(message)) => (Vec::new(), Some(message)),
            None => (Vec::new(), None),
        }
    } else {
        (Vec::new(), None)
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
    staged.publish()?;

    let enabled =
        !grants_reset && (options.enable || existing.as_ref().is_some_and(|plugin| plugin.enabled));
    let grants = if options.enable {
        orbit_types::plugin::parse_grants(&options.grants)
            .map_err(OrbitError::InvalidInput)?
            .into_iter()
            .map(|grant| grant.as_str().to_string())
            .collect()
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
    // `--enable` (including upgrade re-consent) is the operator authorizing
    // this grant set, so it records the integrity value the loader checks the
    // row back against. An ordinary plain `add` deliberately does not rewrite
    // a witness for carried grants: doing so would authorize a set an
    // `orbit.db` writer could have put there. The widening branch above writes
    // only the disabled/empty revocation witness [ORB-12778].
    if options.enable {
        record_authorized_grants(&global_root, &name, enabled, &record.grants)?;
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
        match super::lifecycle::apply_enabled_contributions(runtime, &install_path, false) {
            Ok(contributions) => {
                // Same report `orbit plugin enable` returns, so the two ways
                // of enabling render identically rather than this path
                // silently dropping it into the install summary.
                enable_warnings = unrequested_grant_warnings(&plugin, &record.grants);
                enable_warnings.extend(contributions.warnings);
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
    let mut summary = summary_for_installed(&stored, Some(&plugin), status);
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

fn permission_diff(
    previous: &LoadedPlugin,
    requested: &LoadedPlugin,
) -> Vec<PluginPermissionChange> {
    previous
        .manifest
        .grant_requests()
        .into_iter()
        .zip(requested.manifest.grant_requests())
        .filter_map(|(before, after)| {
            (before.requested != after.requested).then(|| PluginPermissionChange {
                grant: before.grant,
                previous: before.requested,
                requested: after.requested,
                widened: request_widened(before.grant, &previous.manifest, &requested.manifest),
            })
        })
        .collect()
}

fn request_widened(
    grant: PluginGrant,
    previous: &PluginManifest,
    requested: &PluginManifest,
) -> bool {
    let before = &previous.spec.permissions;
    let after = &requested.spec.permissions;
    match grant {
        PluginGrant::Fs => {
            contains_added(&before.fs.read, &after.fs.read)
                || contains_added(&before.fs.write, &after.fs.write)
        }
        PluginGrant::Network => network_rank(after.network) > network_rank(before.network),
        PluginGrant::EnvPass => contains_added(&before.env_pass, &after.env_pass),
        PluginGrant::OrbitTools => contains_added(&before.orbit_tools, &after.orbit_tools),
        PluginGrant::Unsandboxed => {
            previous.spec.backend.sandbox != requested.spec.backend.sandbox
                && requested.spec.backend.sandbox == orbit_types::plugin::PluginSandbox::None
        }
    }
}

fn contains_added(previous: &[String], requested: &[String]) -> bool {
    let previous: BTreeSet<&str> = previous.iter().map(String::as_str).collect();
    requested
        .iter()
        .map(String::as_str)
        .any(|value| !previous.contains(value))
}

fn network_rank(permission: PluginNetworkPermission) -> u8 {
    match permission {
        PluginNetworkPermission::None => 0,
        PluginNetworkPermission::Loopback => 1,
        PluginNetworkPermission::Any => 2,
    }
}

fn permission_widening_message(
    name: &str,
    manifest: &PluginManifest,
    changes: &[PluginPermissionChange],
    previous_manifest_error: Option<&str>,
) -> String {
    let mut message = String::from(
        "Requested permissions widened; the plugin was disabled and its grants were cleared:\n",
    );
    if let Some(error) = previous_manifest_error {
        message.push_str(&format!(
            "  previous manifest could not be compared safely: {error}\n"
        ));
        for request in manifest
            .grant_requests()
            .into_iter()
            .filter(|request| request.requested.is_some())
        {
            message.push_str(&format!(
                "  {}: (unavailable) -> {}\n",
                request.grant,
                request.requested.as_deref().unwrap_or("(not requested)")
            ));
        }
    } else {
        for change in changes.iter().filter(|change| change.widened) {
            message.push_str(&format!(
                "  {}: {} -> {}\n",
                change.grant,
                change.previous.as_deref().unwrap_or("(not requested)"),
                change.requested.as_deref().unwrap_or("(not requested)")
            ));
        }
    }
    let grants = manifest
        .required_grants()
        .into_iter()
        .map(|grant| grant.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let enable_command = if grants.is_empty() {
        format!("orbit plugin enable {name}")
    } else {
        format!("orbit plugin enable {name} --grant {grants}")
    };
    message.push_str(&format!(
        "Review the new requests, then re-consent with `{enable_command}`."
    ));
    message
}

/// Global install only (§3): a plugin tree inside the repository would be
/// vendored state the workspace must not carry.
fn refuse_in_repository_source(
    runtime: &OrbitRuntime,
    source_root: &Path,
) -> Result<(), OrbitError> {
    let repo_root = runtime.paths().repo_root.clone();
    let Ok(repo_root) = std::fs::canonicalize(&repo_root) else {
        return Ok(());
    };
    if !source_root.starts_with(&repo_root) {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "refusing to install '{}': it is inside the repository at {}. Orbit plugins are \
         global-install-only — the host installs once under `~/.orbit/plugins/` and the \
         repository commits only `.orbit/plugins.yaml`, so a plugin tree is never vendored \
         into a checkout. Move the plugin outside the repository and add it from there, or \
         pin it in `.orbit/plugins.yaml` and run `orbit plugin sync`.",
        source_root.display(),
        repo_root.display()
    )))
}

/// A plugin tree staged beside the version directories, and the tree it
/// replaces.
///
/// `add --force` used to delete the live `<version>/` and copy the new tree
/// into it file by file, so a concurrent `orbit` — a clock tick, an MCP
/// server, a dashboard panel — could load a `plugin.yaml` that was already in
/// place while `bin/backend` was still being written, and execute truncated
/// bytes. The copy now lands in a staging directory in the same namespace
/// directory and becomes visible with a single `rename`, so a reader sees
/// either the whole old tree or the whole new one. Replacing a tree does leave
/// a brief moment with no `<version>/` at all, between renaming the old one
/// aside and renaming the new one in: `rename` cannot replace a non-empty
/// directory, and a reader that lands there gets a plain "not installed"
/// error rather than half a plugin.
///
/// The swap rolls back unless [`Self::commit`] is reached, so an install that
/// fails after the copy leaves neither a tree without a `plugins` row — which
/// the next `add` would demand `--force` for — nor a namespace whose row and
/// tree disagree.
struct StagedInstall {
    staging: PathBuf,
    install_path: PathBuf,
    /// Where the replaced tree was moved, held until the row names the new one.
    displaced: Option<PathBuf>,
    published: bool,
    committed: bool,
}

impl StagedInstall {
    fn begin(global_root: &Path, name: &str, version: &str) -> Result<Self, OrbitError> {
        let namespace_dir = plugin_namespace_dir(global_root, name);
        std::fs::create_dir_all(&namespace_dir).map_err(|error| {
            OrbitError::Io(format!("create {}: {error}", namespace_dir.display()))
        })?;
        Ok(Self {
            staging: namespace_dir.join(scratch_name("staging")),
            install_path: namespace_dir.join(version),
            displaced: None,
            published: false,
            committed: false,
        })
    }

    /// Where the tree is copied before it is anything a reader can reach.
    fn staging(&self) -> &Path {
        &self.staging
    }

    /// Move any tree already at `<version>/` aside, then make the staged one
    /// visible with one rename.
    fn publish(&mut self) -> Result<(), OrbitError> {
        if self.install_path.symlink_metadata().is_ok() {
            let displaced = self.install_path.with_file_name(scratch_name("replaced"));
            std::fs::rename(&self.install_path, &displaced).map_err(|error| {
                OrbitError::Io(format!("replace {}: {error}", self.install_path.display()))
            })?;
            self.displaced = Some(displaced);
        }
        std::fs::rename(&self.staging, &self.install_path).map_err(|error| {
            OrbitError::Io(format!("install {}: {error}", self.install_path.display()))
        })?;
        self.published = true;
        Ok(())
    }

    /// The row names the staged tree: keep it, and drop the replaced one.
    fn commit(&mut self) {
        self.committed = true;
        if let Some(displaced) = self.displaced.take() {
            remove_install_scratch(&displaced);
        }
    }
}

impl Drop for StagedInstall {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if self.published {
            remove_install_scratch(&self.install_path);
            // The replaced tree stays where it is if it cannot be put back:
            // the row still names it, and leaving it under a scratch name the
            // warning points at beats deleting the operator's only copy.
            if let Some(displaced) = self.displaced.take()
                && let Err(error) = std::fs::rename(&displaced, &self.install_path)
            {
                tracing::warn!(
                    target: "orbit.core.plugin",
                    path = %self.install_path.display(),
                    replaced = %displaced.display(),
                    "a failed install could not put the replaced plugin tree back: {error}",
                );
            }
        }
        remove_install_scratch(&self.staging);
        if let Some(displaced) = self.displaced.take() {
            remove_install_scratch(&displaced);
        }
    }
}

/// A name inside the namespace directory that only this install owns. The
/// leading dot cannot collide with a version directory: a plugin version is
/// semver, which never starts with one.
fn scratch_name(kind: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let (seconds, nanos) = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or((0, 0), |since| (since.as_secs(), since.subsec_nanos()));
    format!(
        ".{kind}-{pid}-{seconds:x}{nanos:x}-{nonce:x}",
        pid = std::process::id(),
        nonce = COUNTER.fetch_add(1, Ordering::Relaxed),
    )
}

/// Delete a leftover the install owns. Best effort: the caller is either
/// unwinding from an error it will report, or finishing an install that has
/// already landed.
fn remove_install_scratch(path: &Path) {
    let Ok(metadata) = path.symlink_metadata() else {
        return;
    };
    let removed = if metadata.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    if let Err(error) = removed {
        tracing::warn!(
            target: "orbit.core.plugin",
            path = %path.display(),
            "left behind a plugin install directory that could not be removed: {error}",
        );
    }
}

/// Delete everything in the namespace install directory except the tree the
/// `plugins` row now names.
///
/// Each upgrade used to leave `plugins/<ns>/<oldversion>/` behind. Those trees
/// are readable to every plugin backend — the install family is always
/// readable (§4.3) — and are enough to make a later `add` of that version
/// demand `--force`. One host row names one version, so nothing else under the
/// namespace directory is referenced: not an older version, not a `current`
/// link an earlier Orbit wrote beside them, not scratch a crashed install left.
///
/// Best effort, and only after the row is written: an install that landed is
/// not reported as a failure because a stale directory would not delete.
fn prune_namespace(global_root: &Path, name: &str, keep: &Path) {
    let namespace_dir = plugin_namespace_dir(global_root, name);
    let Ok(entries) = std::fs::read_dir(&namespace_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path != keep {
            remove_install_scratch(&path);
        }
    }
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), OrbitError> {
    refuse_plugin_tree_symlinks(source)?;
    copy_tree_inner(source, source, target)
}

fn copy_tree_inner(tree_root: &Path, source: &Path, target: &Path) -> Result<(), OrbitError> {
    std::fs::create_dir_all(target)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", target.display())))?;
    for entry in std::fs::read_dir(source)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", source.display())))?
    {
        let entry =
            entry.map_err(|error| OrbitError::Io(format!("read {}: {error}", source.display())))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| OrbitError::Io(format!("stat {}: {error}", path.display())))?;
        if file_type.is_symlink() {
            let target = std::fs::read_link(&path).ok();
            return Err(OrbitError::InvalidInput(plugin_symlink_refusal(
                path.strip_prefix(tree_root).unwrap_or(&path),
                target.as_deref(),
            )));
        }
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree_inner(tree_root, &path, &destination)?;
        } else {
            std::fs::copy(&path, &destination)
                .map_err(|error| OrbitError::Io(format!("copy {}: {error}", path.display())))?;
            copy_permissions(&path, &destination)?;
        }
    }
    Ok(())
}

fn copy_permissions(source: &Path, target: &Path) -> Result<(), OrbitError> {
    #[cfg(unix)]
    {
        let metadata = std::fs::metadata(source)
            .map_err(|error| OrbitError::Io(format!("stat {}: {error}", source.display())))?;
        std::fs::set_permissions(target, metadata.permissions())
            .map_err(|error| OrbitError::Io(format!("chmod {}: {error}", target.display())))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (source, target);
    }
    Ok(())
}
