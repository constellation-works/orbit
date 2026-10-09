//! Reconciling a managed directory against its embedded defaults, and operator opt-outs.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::security::release::sha256_hex;

use super::{
    ConfinedAssetPath, MANAGED_ASSET_MANIFEST_FILE, MANAGED_ASSET_MANIFEST_SCHEMA_VERSION,
    ManagedAssetAction, ManagedAssetLayout, ManagedAssetManifest, ManagedAssetOutcome,
    ManagedAssetReconcileMode, ManagedAssetReconciliation, encode_managed_asset_manifest,
    load_managed_asset_manifest, preserve_modified_retired_asset, record_managed_manifest_write,
    resolve_confined_asset_path, retired_preservation_path, write_managed_asset_manifest,
};

use super::manifest::{validate_managed_asset_name, write_confined_asset};
use super::retired::{ambiguous_legacy_yaml_files, unsafe_preservation_component};

/// Materialize the current embedded resource set and reconcile assets retired
/// since the previous manifest-aware seed.
///
/// The manifest records the digest Orbit last wrote for each managed file.
/// Retired files that still match that digest are deleted. Locally modified
/// retired files are moved outside the recursively loaded catalog tree, so
/// their content survives without keeping a removed subsystem active. A
/// legacy directory without a manifest is migrated conservatively: exact
/// current defaults gain provenance, while every other YAML file stays in
/// place and produces an actionable warning. Every asset path is resolved
/// through [`resolve_confined_asset_path`] first: one that crosses a link or a
/// wrongly typed component is reported and left untouched in both modes.
// ADR-0346: content provenance, rather than filenames, authorizes retirement.
pub(crate) fn reconcile_managed_assets<'a>(
    dir: &Path,
    asset_kind: &str,
    layout: ManagedAssetLayout,
    files: &'a [(&'a str, &'a str)],
    overwrite: bool,
    render: impl FnMut(&'a str, &'a str) -> Result<Cow<'a, str>, OrbitError>,
) -> Result<ManagedAssetReconciliation, OrbitError> {
    reconcile_managed_assets_in_mode(
        dir,
        asset_kind,
        layout,
        files,
        overwrite,
        ManagedAssetReconcileMode::Apply,
        render,
    )
}

pub(crate) fn reconcile_managed_assets_in_mode<'a>(
    dir: &Path,
    asset_kind: &str,
    layout: ManagedAssetLayout,
    files: &'a [(&'a str, &'a str)],
    overwrite: bool,
    mode: ManagedAssetReconcileMode,
    mut render: impl FnMut(&'a str, &'a str) -> Result<Cow<'a, str>, OrbitError>,
) -> Result<ManagedAssetReconciliation, OrbitError> {
    validate_managed_asset_name(asset_kind, ManagedAssetLayout::YamlStem, "asset kind")?;
    for (name, _) in files {
        validate_managed_asset_name(name, layout, "embedded asset")?;
    }

    let manifest_path = dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let previous = load_managed_asset_manifest(&manifest_path, asset_kind, layout)?;
    let current_names: BTreeSet<&str> = files.iter().map(|(name, _)| *name).collect();
    let opted_out: BTreeSet<String> = previous
        .as_ref()
        .map(|manifest| {
            manifest
                .opted_out
                .iter()
                .filter(|name| current_names.contains(name.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let mut result = ManagedAssetReconciliation::default();
    // Create-only keeps every recorded digest; only absent files gain new ones.
    let mut next_assets: BTreeMap<String, String> = match &previous {
        Some(previous) if mode.creates_only() => previous.assets.clone(),
        _ => BTreeMap::new(),
    };

    if let Some(previous) = previous.as_ref().filter(|_| !mode.creates_only()) {
        for (name, managed_digest) in &previous.assets {
            if current_names.contains(name.as_str()) {
                continue;
            }
            let relative = layout.relative_path(name);
            let path = dir.join(&relative);
            let resolved = resolve_confined_asset_path(dir, &relative)?;
            if let ConfinedAssetPath::Unsafe(component) = resolved {
                // Retirement would read, delete, or move through a link to a
                // target outside this catalog. Keep the provenance so a later
                // pass retires the asset once the operator repairs the path.
                next_assets.insert(name.clone(), managed_digest.clone());
                let warning = format!(
                    "retired managed {asset_kind} `{name}` was left in place because '{}' is linked or is not the expected file or directory type; Orbit did not read, remove, or preserve anything through it. Replace it with a regular file inside '{}' or remove the link, then rerun `orbit workspace sync`",
                    component.display(),
                    dir.display()
                );
                result.warnings.push(warning.clone());
                result.actions.push(ManagedAssetAction {
                    name: name.clone(),
                    path,
                    outcome: ManagedAssetOutcome::Preserved,
                    detail: Some(warning),
                });
                continue;
            }
            if resolved == ConfinedAssetPath::Missing {
                result.actions.push(ManagedAssetAction {
                    name: name.clone(),
                    path,
                    outcome: ManagedAssetOutcome::Retired,
                    detail: Some(
                        "removed stale manifest provenance for an absent artifact".to_string(),
                    ),
                });
                result.retired += 1;
                continue;
            }
            let content = fs::read_to_string(&path).map_err(|error| {
                OrbitError::io_with_context(
                    &error,
                    format!(
                        "read retired managed {asset_kind} '{}': {error}",
                        path.display()
                    ),
                )
            })?;
            if sha256_hex(content.as_bytes()) == *managed_digest {
                if mode == ManagedAssetReconcileMode::Apply {
                    fs::remove_file(&path).map_err(|error| {
                        OrbitError::io_with_context(
                            &error,
                            format!("retire managed {asset_kind} '{}': {error}", path.display()),
                        )
                    })?;
                }
            } else {
                if let Some(component) =
                    unsafe_preservation_component(dir, asset_kind, layout, name)?
                {
                    next_assets.insert(name.clone(), managed_digest.clone());
                    let warning = format!(
                        "retired managed {asset_kind} `{name}` was locally modified, but its preservation destination '{}' is linked or is not a directory; Orbit left the file in the active catalog. Repair that path, then rerun `orbit workspace sync`",
                        component.display()
                    );
                    result.warnings.push(warning.clone());
                    result.actions.push(ManagedAssetAction {
                        name: name.clone(),
                        path,
                        outcome: ManagedAssetOutcome::Preserved,
                        detail: Some(warning),
                    });
                    continue;
                }
                let preserved = if mode == ManagedAssetReconcileMode::Apply {
                    preserve_modified_retired_asset(dir, asset_kind, layout, name, &path)?
                } else {
                    retired_preservation_path(dir, asset_kind, layout, name)
                };
                let warning = format!(
                    "retired managed {asset_kind} `{name}` was locally modified; Orbit {} it from the active catalog and preserved it at '{}'. Review that file, then migrate it under a new user-authored name or delete it",
                    if mode == ManagedAssetReconcileMode::Apply {
                        "removed"
                    } else {
                        "would remove"
                    },
                    preserved.display()
                );
                result.warnings.push(warning.clone());
                result.actions.push(ManagedAssetAction {
                    name: name.clone(),
                    path: path.clone(),
                    outcome: ManagedAssetOutcome::Preserved,
                    detail: Some(warning),
                });
            }
            result.actions.push(ManagedAssetAction {
                name: name.clone(),
                path,
                outcome: ManagedAssetOutcome::Retired,
                detail: None,
            });
            result.retired += 1;
        }
    }

    for (name, embedded) in files {
        let relative = layout.relative_path(name);
        let path = dir.join(&relative);
        if opted_out.contains(*name) {
            result.actions.push(ManagedAssetAction {
                name: (*name).to_string(),
                path,
                outcome: ManagedAssetOutcome::Unchanged,
                detail: Some(format!(
                    "shipped {asset_kind} `{name}` was deleted by an operator and stays opted out"
                )),
            });
            continue;
        }
        let rendered = render(name, embedded)?;
        let rendered_digest = sha256_hex(rendered.as_bytes());
        let previous_digest = previous
            .as_ref()
            .and_then(|manifest| manifest.assets.get(*name));

        let resolved = resolve_confined_asset_path(dir, &relative)?;
        if let ConfinedAssetPath::Unsafe(component) = &resolved {
            // Writing here would create or overwrite a file outside this
            // catalog. Leave the path alone and carry forward only the
            // provenance already recorded: nothing new was written.
            if let Some(previous_digest) = previous_digest {
                next_assets.insert((*name).to_string(), previous_digest.clone());
            }
            let warning = format!(
                "managed {asset_kind} `{name}` was not written because '{}' is linked or is not the expected file or directory type; Orbit left it and any link target untouched. Replace it with a regular file inside '{}' or remove the link, then rerun `orbit workspace sync`",
                component.display(),
                dir.display()
            );
            result.warnings.push(warning.clone());
            result.actions.push(ManagedAssetAction {
                name: (*name).to_string(),
                path,
                outcome: ManagedAssetOutcome::Preserved,
                detail: Some(warning),
            });
            continue;
        }

        let exists = matches!(resolved, ConfinedAssetPath::File(_));
        if exists && mode.creates_only() {
            // Its recorded digest was carried forward unchanged above.
            continue;
        }
        if exists {
            if previous_digest.is_none() {
                let existing = fs::read_to_string(&path).map_err(|error| {
                    OrbitError::io_with_context(
                        &error,
                        format!("read existing {asset_kind} '{}': {error}", path.display()),
                    )
                })?;
                if sha256_hex(existing.as_bytes()) == rendered_digest {
                    next_assets.insert((*name).to_string(), rendered_digest);
                    result.actions.push(ManagedAssetAction {
                        name: (*name).to_string(),
                        path: path.clone(),
                        outcome: ManagedAssetOutcome::Migrated,
                        detail: Some(
                            "recorded provenance for an exact existing shipped artifact"
                                .to_string(),
                        ),
                    });
                } else if previous.is_some() {
                    let warning = format!(
                        "untracked user-authored {asset_kind} '{}' collides with bundled default `{name}` and was preserved in place. Move or rename it, then rerun `orbit init` to install the bundled default",
                        path.display()
                    );
                    result.warnings.push(warning.clone());
                    result.actions.push(ManagedAssetAction {
                        name: (*name).to_string(),
                        path: path.clone(),
                        outcome: ManagedAssetOutcome::Preserved,
                        detail: Some(warning),
                    });
                }
                continue;
            }

            if !overwrite {
                let Some(previous_digest) = previous_digest else {
                    continue;
                };
                let existing = fs::read_to_string(&path).map_err(|error| {
                    OrbitError::io_with_context(
                        &error,
                        format!("read existing {asset_kind} '{}': {error}", path.display()),
                    )
                })?;

                // A digest match proves this is an unedited file Orbit wrote.
                // Refresh it during ordinary bootstrap when a newer binary
                // ships different content; otherwise a removed tool or schema
                // value can leave the runtime unable to load its own catalog.
                // Any mismatch is a local edit and must remain untouched.
                if sha256_hex(existing.as_bytes()) == *previous_digest
                    && previous_digest != &rendered_digest
                {
                    if mode == ManagedAssetReconcileMode::Apply {
                        write_confined_asset(&path, &rendered, true, asset_kind)?;
                    }
                    next_assets.insert((*name).to_string(), rendered_digest);
                    result.refreshed += 1;
                    result.actions.push(ManagedAssetAction {
                        name: (*name).to_string(),
                        path: path.clone(),
                        outcome: ManagedAssetOutcome::Refreshed,
                        detail: None,
                    });
                } else {
                    next_assets.insert((*name).to_string(), previous_digest.clone());
                    let modified = sha256_hex(existing.as_bytes()) != *previous_digest;
                    result.actions.push(ManagedAssetAction {
                        name: (*name).to_string(),
                        path: path.clone(),
                        outcome: if modified {
                            ManagedAssetOutcome::Preserved
                        } else {
                            ManagedAssetOutcome::Unchanged
                        },
                        detail: modified.then(|| {
                            format!(
                                "locally modified managed {asset_kind} '{}' was preserved; restore the Orbit-written bytes or move/rename the file, then rerun `orbit workspace sync`",
                                path.display()
                            )
                        }),
                    });
                }
                continue;
            }

            // The manifest records the last embedded content written for this
            // asset. If it already matches the current embedded content, this
            // bootstrap has nothing to refresh. Avoid touching the asset so a
            // steady-state runtime can operate with global resources mounted
            // read-only.
            if previous_digest == Some(&rendered_digest) {
                next_assets.insert((*name).to_string(), rendered_digest);
                result.actions.push(ManagedAssetAction {
                    name: (*name).to_string(),
                    path: path.clone(),
                    outcome: ManagedAssetOutcome::Unchanged,
                    detail: None,
                });
                continue;
            }
        }

        if mode.writes() {
            write_confined_asset(&path, &rendered, exists, asset_kind)?;
        }
        next_assets.insert((*name).to_string(), rendered_digest);
        result.refreshed += 1;
        result.actions.push(ManagedAssetAction {
            name: (*name).to_string(),
            path,
            outcome: ManagedAssetOutcome::Created,
            detail: None,
        });
    }

    // The legacy sweep only makes sense for the flat YAML catalogs: a skill
    // tree's untracked files are ordinary reference material inside an
    // otherwise-managed directory, not stray definitions the loader would pick
    // up. Create-only never records the existing shipped files, so a sweep
    // would report them as untracked; it leaves that to a converging pass.
    if previous.is_none()
        && layout == ManagedAssetLayout::YamlStem
        && !mode.creates_only()
        && dir.exists()
    {
        let ambiguous = ambiguous_legacy_yaml_files(dir, &next_assets)?;
        if !ambiguous.is_empty() {
            result.warnings.push(format!(
                "untracked {asset_kind} YAML assets have no managed provenance and were preserved in place: {}. If any came from an older Orbit release, move or delete them manually before retrying catalog/list commands",
                ambiguous
                    .iter()
                    .map(|path| format!("'{}'", path.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    let manifest = ManagedAssetManifest {
        schema_version: MANAGED_ASSET_MANIFEST_SCHEMA_VERSION,
        asset_kind: asset_kind.to_string(),
        assets: next_assets,
        routine_provenance: BTreeMap::new(),
        opted_out,
    };
    if mode.writes() && previous.as_ref() != Some(&manifest) {
        let encoded = encode_managed_asset_manifest(&manifest)?;
        let recorded = record_managed_manifest_write(
            &manifest_path,
            asset_kind,
            atomic_write_text(&manifest_path, &encoded),
            &mut result.warnings,
        )?;
        if !recorded {
            // An adoption only records provenance once the manifest lands;
            // the skipped write is reported through `result.warnings`.
            for action in result
                .actions
                .iter_mut()
                .filter(|action| action.outcome == ManagedAssetOutcome::Migrated)
            {
                action.detail = Some(
                    "exact existing shipped artifact; its provenance was not recorded because the manifest write was denied"
                        .to_string(),
                );
            }
        }
    }

    for warning in &result.warnings {
        tracing::warn!(
            target: "orbit.core.managed_assets",
            asset_kind,
            warning,
            "managed asset reconciliation requires operator attention"
        );
    }

    Ok(result)
}

/// Record that an operator deleted the shipped default `name`, so later
/// reconciliation leaves it absent. Any provenance for it is dropped: the file
/// is gone, and a hand-written replacement is user-authored.
///
/// A directory that was never reconciled gains a manifest here; without one
/// the next seed would treat the directory as legacy and re-create the file.
/// The returned record undoes the opt-out when a later step fails.
pub(crate) fn record_managed_asset_opt_out(
    dir: &Path,
    asset_kind: &str,
    layout: ManagedAssetLayout,
    name: &str,
) -> Result<ManagedAssetOptOut, OrbitError> {
    validate_managed_asset_name(name, layout, "opted-out asset")?;
    let manifest_path = dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let previous = load_managed_asset_manifest(&manifest_path, asset_kind, layout)?;
    let mut next = previous.clone().unwrap_or_else(|| ManagedAssetManifest {
        schema_version: MANAGED_ASSET_MANIFEST_SCHEMA_VERSION,
        asset_kind: asset_kind.to_string(),
        assets: BTreeMap::new(),
        routine_provenance: BTreeMap::new(),
        opted_out: BTreeSet::new(),
    });
    next.assets.remove(name);
    next.opted_out.insert(name.to_string());
    let changed = previous.as_ref() != Some(&next);
    if changed {
        write_managed_asset_manifest(&manifest_path, &next)?;
    }
    Ok(ManagedAssetOptOut {
        manifest_path,
        previous,
        changed,
    })
}

/// The manifest an opt-out replaced, kept so the caller can put it back.
#[must_use = "revert the opt-out when the operation it belongs to fails"]
pub(crate) struct ManagedAssetOptOut {
    manifest_path: PathBuf,
    previous: Option<ManagedAssetManifest>,
    changed: bool,
}

impl ManagedAssetOptOut {
    /// Restore the manifest as it stood before the opt-out, removing one the
    /// opt-out created.
    pub(crate) fn revert(self) -> Result<(), OrbitError> {
        if !self.changed {
            return Ok(());
        }
        match &self.previous {
            Some(previous) => write_managed_asset_manifest(&self.manifest_path, previous),
            None => fs::remove_file(&self.manifest_path).map_err(|error| {
                OrbitError::io_with_context(
                    &error,
                    format!(
                        "remove managed asset manifest '{}': {error}",
                        self.manifest_path.display()
                    ),
                )
            }),
        }
    }
}

/// Clear an operator opt-out and record `digest` as the provenance of the
/// shipped content just written back for `name`.
pub(crate) fn restore_managed_asset(
    dir: &Path,
    asset_kind: &str,
    layout: ManagedAssetLayout,
    name: &str,
    digest: String,
) -> Result<(), OrbitError> {
    validate_managed_asset_name(name, layout, "restored asset")?;
    let manifest_path = dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let mut next =
        load_managed_asset_manifest(&manifest_path, asset_kind, layout)?.unwrap_or_else(|| {
            ManagedAssetManifest {
                schema_version: MANAGED_ASSET_MANIFEST_SCHEMA_VERSION,
                asset_kind: asset_kind.to_string(),
                assets: BTreeMap::new(),
                routine_provenance: BTreeMap::new(),
                opted_out: BTreeSet::new(),
            }
        });
    next.opted_out.remove(name);
    next.assets.insert(name.to_string(), digest);
    write_managed_asset_manifest(&manifest_path, &next)
}
