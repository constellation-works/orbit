//! Materialize and reconcile a workspace's default routines against their
//! managed manifest: seed, refresh, adopt, and retire by provenance.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use orbit_automation::routines::loader::retired_routine_job_reason;
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_common::security::release::sha256_hex;

use super::seed::{DEFAULT_ROUTINE_FILES, RoutineSeedIdentity};
use super::template::{
    ShippedShape, adoptable_binding, binding_of, reconcile_lifecycle_variant, render_refresh,
    render_routine_template, shipped_shape_of,
};
use crate::application::managed_assets::{
    MANAGED_ASSET_MANIFEST_FILE, ManagedAssetAction, ManagedAssetLayout, ManagedAssetManifest,
    ManagedAssetOutcome, ManagedAssetReconcileMode, ManagedAssetReconciliation,
    ROUTINE_MANAGED_ASSET_MANIFEST_SCHEMA_VERSION, RoutineAssetProvenance,
    encode_managed_asset_manifest, load_managed_asset_manifest, preserve_modified_retired_asset,
    record_managed_manifest_write, retired_preservation_path,
};

/// Seed every entry in [`DEFAULT_ROUTINE_FILES`] under `routines_dir`,
/// resolving the routine-name placeholder. Mirrors the activity /
/// job seeding convention: when `overwrite` is false (plain re-init),
/// existing files are preserved. Destructive initialization may set
/// `overwrite`, though `--force` normally recreates the whole root first.
///
/// Seeding is manifest-aware: the recorded digest is taken over the *rendered*
/// document — after routine-name substitution — because that is
/// what actually lands on disk. A default dropped from a later release is
/// therefore retired by content provenance, and a re-seed of unchanged
/// embedded content is a no-op rather than a rewrite.
#[cfg(test)]
pub(crate) fn seed_default_routines(
    routines_dir: &Path,
    workspace_name: &str,
    overwrite: bool,
) -> Result<ManagedAssetReconciliation, OrbitError> {
    let identity = RoutineSeedIdentity::new(workspace_name, "hm_test", "main")?;
    reconcile_default_routines(
        routines_dir,
        &identity,
        overwrite,
        ManagedAssetReconcileMode::Apply,
    )
}

/// Reconcile the shipped default routines in `routines_dir` against its
/// managed manifest.
///
/// Every path this reads, creates, refreshes, retires, or preserves into is
/// held to one confinement contract: the catalog, its manifest, each
/// definition, and the `.retired-managed/routines/` route beside it must be a
/// real directory or regular file — or absent — judged without following
/// links. A symbolic link anywhere on that route, dangling or not, is refused
/// and reported, so no sync creates, overwrites, or retires a file outside the
/// catalog. The routine loader refuses the same links.
pub(crate) fn reconcile_default_routines(
    routines_dir: &Path,
    identity: &RoutineSeedIdentity,
    overwrite_bindings: bool,
    mode: ManagedAssetReconcileMode,
) -> Result<ManagedAssetReconciliation, OrbitError> {
    let manifest_path = routines_dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let mut result = ManagedAssetReconciliation::default();
    if let Some(detail) = unconfined_catalog(routines_dir, &manifest_path)? {
        // The manifest and every definition live behind the refused path, so
        // nothing below can be judged, let alone written.
        result.warnings.push(detail.clone());
        for (name, _) in DEFAULT_ROUTINE_FILES {
            result.actions.push(ManagedAssetAction {
                name: (*name).to_string(),
                path: routines_dir.join(format!("{name}.yaml")),
                outcome: ManagedAssetOutcome::Preserved,
                detail: Some(detail.clone()),
            });
        }
        return Ok(result);
    }
    let previous =
        load_managed_asset_manifest(&manifest_path, "routine", ManagedAssetLayout::YamlStem)?;
    let preservation_confined = preservation_route_is_confined(routines_dir)?;
    let shipped: BTreeSet<&str> = DEFAULT_ROUTINE_FILES
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let mut next_assets = BTreeMap::new();
    let mut next_provenance = BTreeMap::new();

    if let Some(previous) = &previous {
        for (name, rendered_digest) in &previous.assets {
            if shipped.contains(name.as_str()) {
                continue;
            }
            let path = routines_dir.join(format!("{name}.yaml"));
            let entry = inspect_catalog_entry(&path, CatalogEntryKind::File)?;
            if entry == CatalogEntry::Unsafe {
                // Keep tracking it so the retirement completes once the link
                // is replaced by the definition it stands for.
                refuse_unconfined_routine(&mut result, name, &path, &unconfined_detail(&path));
                next_assets.insert(name.clone(), rendered_digest.clone());
                if let Some(provenance) = previous.routine_provenance.get(name) {
                    next_provenance.insert(name.clone(), provenance.clone());
                }
                continue;
            }
            let mut retired_detail = None;
            if entry == CatalogEntry::Present {
                let existing = fs::read_to_string(&path).map_err(|error| {
                    OrbitError::Io(format!(
                        "read retired managed routine '{}': {error}",
                        path.display()
                    ))
                })?;
                let byte_exact = sha256_hex(existing.as_bytes()) == *rendered_digest;
                if !byte_exact && !preservation_confined {
                    refuse_unconfined_preservation(&mut result, name, &path, routines_dir);
                    next_assets.insert(name.clone(), rendered_digest.clone());
                    if let Some(provenance) = previous.routine_provenance.get(name) {
                        next_provenance.insert(name.clone(), provenance.clone());
                    }
                    continue;
                }
                // Orbit deletes outright only bytes it can prove it wrote. A
                // lifecycle variant — the operator's `enabled` opt-in, the
                // retired `hosts:` key they were told to drop, a comment they
                // added — still retires without demanding a manual move, but
                // a copy is kept so nothing they authored is destroyed.
                if byte_exact {
                    if mode == ManagedAssetReconcileMode::Apply {
                        fs::remove_file(&path).map_err(|error| {
                            OrbitError::Io(format!(
                                "retire managed routine '{}': {error}",
                                path.display()
                            ))
                        })?;
                    }
                } else if shipped_shape_of(name, &existing).is_some() {
                    let preserved = if mode == ManagedAssetReconcileMode::Apply {
                        preserve_modified_retired_asset(
                            routines_dir,
                            "routine",
                            ManagedAssetLayout::YamlStem,
                            name,
                            &path,
                        )?
                    } else {
                        retired_preservation_path(
                            routines_dir,
                            "routine",
                            ManagedAssetLayout::YamlStem,
                            name,
                        )
                    };
                    retired_detail = Some(format!(
                        "retired a prior release's routine whose only differences were operator lifecycle settings; a copy is at '{}'",
                        preserved.display()
                    ));
                } else {
                    let preserved = if mode == ManagedAssetReconcileMode::Apply {
                        preserve_modified_retired_asset(
                            routines_dir,
                            "routine",
                            ManagedAssetLayout::YamlStem,
                            name,
                            &path,
                        )?
                    } else {
                        retired_preservation_path(
                            routines_dir,
                            "routine",
                            ManagedAssetLayout::YamlStem,
                            name,
                        )
                    };
                    let detail = format!(
                        "retired managed routine '{}' was locally modified; Orbit {} it from the active catalog and preserved it at '{}'. Review that file, then migrate it under a new user-authored name or delete it",
                        path.display(),
                        if mode == ManagedAssetReconcileMode::Apply {
                            "removed"
                        } else {
                            "would remove"
                        },
                        preserved.display()
                    );
                    result.warnings.push(detail.clone());
                    result.actions.push(ManagedAssetAction {
                        name: name.clone(),
                        path: path.clone(),
                        outcome: ManagedAssetOutcome::Preserved,
                        detail: Some(detail),
                    });
                }
            }
            result.actions.push(ManagedAssetAction {
                name: name.clone(),
                path,
                outcome: ManagedAssetOutcome::Retired,
                detail: retired_detail,
            });
            result.retired += 1;
        }
    }

    for (name, template) in DEFAULT_ROUTINE_FILES {
        let path = routines_dir.join(format!("{name}.yaml"));
        let requested_binding = identity.binding(name, template);
        let template_digest = sha256_hex(template.as_bytes());
        let previous_digest = previous.as_ref().and_then(|value| value.assets.get(*name));
        let previous_provenance = previous
            .as_ref()
            .and_then(|value| value.routine_provenance.get(*name));
        let present = match inspect_catalog_entry(&path, CatalogEntryKind::File)? {
            CatalogEntry::Missing => false,
            CatalogEntry::Present => true,
            CatalogEntry::Unsafe => {
                // Neither read nor written: a dangling link would otherwise
                // look missing and be "recreated" at its external target.
                refuse_unconfined_routine(&mut result, name, &path, &unconfined_detail(&path));
                if let Some(provenance) = previous_provenance {
                    next_assets.insert((*name).to_string(), provenance.rendered_digest.clone());
                    next_provenance.insert((*name).to_string(), provenance.clone());
                } else if let Some(digest) = previous_digest {
                    next_assets.insert((*name).to_string(), digest.clone());
                }
                continue;
            }
        };

        if let Some(provenance) = previous_provenance {
            let binding = if overwrite_bindings {
                requested_binding.clone()
            } else {
                identity.complete(name, template, &provenance.binding)
            };
            let rendered = render_routine_template(name, template, &binding)?;
            let rendered_digest = sha256_hex(rendered.as_bytes());
            if present {
                let existing = fs::read_to_string(&path).map_err(|error| {
                    OrbitError::Io(format!(
                        "read managed routine '{}': {error}",
                        path.display()
                    ))
                })?;
                let existing_digest = sha256_hex(existing.as_bytes());
                if existing_digest != provenance.rendered_digest && !overwrite_bindings {
                    if let Some(outcome) = reconcile_lifecycle_variant(
                        identity,
                        name,
                        template,
                        &template_digest,
                        &path,
                        &existing,
                        mode,
                        &mut result,
                    )? {
                        next_assets.insert((*name).to_string(), outcome.rendered_digest.clone());
                        next_provenance.insert((*name).to_string(), outcome);
                        continue;
                    }
                    let detail = format!(
                        "locally modified managed routine '{}' was preserved; restore the Orbit-written bytes or move/rename the file, then rerun `orbit workspace sync`",
                        path.display()
                    );
                    result.actions.push(ManagedAssetAction {
                        name: (*name).to_string(),
                        path: path.clone(),
                        outcome: ManagedAssetOutcome::Preserved,
                        detail: Some(detail),
                    });
                    next_assets.insert((*name).to_string(), provenance.rendered_digest.clone());
                    next_provenance.insert((*name).to_string(), provenance.clone());
                    continue;
                }
                if !overwrite_bindings && binding != requested_binding {
                    result.actions.push(ManagedAssetAction {
                        name: (*name).to_string(),
                        path: path.clone(),
                        outcome: ManagedAssetOutcome::BindingDrift,
                        detail: Some(format!(
                            "current workspace binding would render {}; preserving recorded {}",
                            requested_binding.describe(),
                            binding.describe()
                        )),
                    });
                }
                // A plain re-seed converges on the recorded provenance, which
                // may be an adopted operator edit. An overwriting seed also
                // requires the bytes on disk to be the ones this template
                // renders, so `--force` restores a hand-edited definition.
                let converged = provenance.template_digest == template_digest
                    && provenance.binding == binding
                    && (!overwrite_bindings || existing_digest == rendered_digest);
                if converged {
                    result.actions.push(ManagedAssetAction {
                        name: (*name).to_string(),
                        path: path.clone(),
                        outcome: ManagedAssetOutcome::Unchanged,
                        detail: None,
                    });
                    next_assets.insert((*name).to_string(), provenance.rendered_digest.clone());
                    next_provenance.insert((*name).to_string(), provenance.clone());
                    continue;
                }
                // The bytes are Orbit's, but `enabled` is the operator's: an
                // ordinary refresh must not flip an opt-in back to the
                // template default. An overwriting seed (`--force`) is the
                // deliberate exception — it restores the template verbatim.
                let (rendered, rendered_digest) = if overwrite_bindings {
                    (rendered, rendered_digest)
                } else {
                    let refreshed = render_refresh(name, template, &binding, &existing)?;
                    let digest = sha256_hex(refreshed.as_bytes());
                    (refreshed, digest)
                };
                if mode == ManagedAssetReconcileMode::Apply {
                    write_confined_routine(&path, &rendered)?;
                }
                result.refreshed += 1;
                result.actions.push(ManagedAssetAction {
                    name: (*name).to_string(),
                    path: path.clone(),
                    outcome: ManagedAssetOutcome::Refreshed,
                    detail: Some("shipped routine template changed; preserved the recorded materialization binding".to_string()),
                });
                next_assets.insert((*name).to_string(), rendered_digest.clone());
                next_provenance.insert(
                    (*name).to_string(),
                    RoutineAssetProvenance {
                        template_digest,
                        rendered_digest,
                        binding,
                    },
                );
                continue;
            } else {
                if mode == ManagedAssetReconcileMode::Apply {
                    write_confined_routine(&path, &rendered)?;
                }
                result.refreshed += 1;
                result.actions.push(ManagedAssetAction {
                    name: (*name).to_string(),
                    path: path.clone(),
                    outcome: ManagedAssetOutcome::Created,
                    detail: Some(
                        "recreated a missing managed routine with its recorded binding".to_string(),
                    ),
                });
            }
            next_assets.insert((*name).to_string(), rendered_digest.clone());
            next_provenance.insert(
                (*name).to_string(),
                RoutineAssetProvenance {
                    template_digest,
                    rendered_digest,
                    binding,
                },
            );
            continue;
        }

        if let Some(legacy_digest) = previous_digest {
            if !present {
                let rendered = render_routine_template(name, template, &requested_binding)?;
                let rendered_digest = sha256_hex(rendered.as_bytes());
                if mode == ManagedAssetReconcileMode::Apply {
                    write_confined_routine(&path, &rendered)?;
                }
                result.refreshed += 1;
                result.actions.push(ManagedAssetAction {
                    name: (*name).to_string(),
                    path: path.clone(),
                    outcome: ManagedAssetOutcome::Created,
                    detail: None,
                });
                next_assets.insert((*name).to_string(), rendered_digest.clone());
                next_provenance.insert(
                    (*name).to_string(),
                    RoutineAssetProvenance {
                        template_digest,
                        rendered_digest,
                        binding: requested_binding,
                    },
                );
                continue;
            }
            let existing = fs::read_to_string(&path).map_err(|error| {
                OrbitError::Io(format!(
                    "read legacy managed routine '{}': {error}",
                    path.display()
                ))
            })?;
            if sha256_hex(existing.as_bytes()) != *legacy_digest {
                if let Some(outcome) = reconcile_lifecycle_variant(
                    identity,
                    name,
                    template,
                    &template_digest,
                    &path,
                    &existing,
                    mode,
                    &mut result,
                )? {
                    next_assets.insert((*name).to_string(), outcome.rendered_digest.clone());
                    next_provenance.insert((*name).to_string(), outcome);
                    continue;
                }
                let detail = format!(
                    "legacy managed routine '{}' no longer matches Orbit's recorded digest and was preserved; restore the recorded bytes or move/rename it, then rerun `orbit workspace sync`",
                    path.display()
                );
                result.warnings.push(detail.clone());
                result.actions.push(ManagedAssetAction {
                    name: (*name).to_string(),
                    path: path.clone(),
                    outcome: ManagedAssetOutcome::Preserved,
                    detail: Some(detail),
                });
                next_assets.insert((*name).to_string(), legacy_digest.clone());
                continue;
            }
            let definition = parse_routine_yaml(&existing).map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "legacy managed routine '{}' matches its manifest but cannot be parsed to recover its recorded binding: {error}",
                    path.display()
                ))
            })?;
            let binding = identity.complete(name, template, &binding_of(&definition));
            let rendered = render_refresh(name, template, &binding, &existing)?;
            let rendered_digest = sha256_hex(rendered.as_bytes());
            let changed = rendered_digest != *legacy_digest;
            if changed && mode == ManagedAssetReconcileMode::Apply {
                write_confined_routine(&path, &rendered)?;
            }
            if changed {
                result.refreshed += 1;
                result.actions.push(ManagedAssetAction {
                    name: (*name).to_string(),
                    path: path.clone(),
                    outcome: ManagedAssetOutcome::Refreshed,
                    detail: Some("refreshed a legacy Orbit-written routine using the binding parsed from that exact instance".to_string()),
                });
            }
            result.actions.push(ManagedAssetAction {
                name: (*name).to_string(),
                path: path.clone(),
                outcome: ManagedAssetOutcome::Migrated,
                detail: Some(
                    "migrated legacy rendered-only provenance using the exact on-disk instance"
                        .to_string(),
                ),
            });
            next_assets.insert((*name).to_string(), rendered_digest.clone());
            next_provenance.insert(
                (*name).to_string(),
                RoutineAssetProvenance {
                    template_digest,
                    rendered_digest,
                    binding,
                },
            );
            continue;
        }

        let rendered = render_routine_template(name, template, &requested_binding)?;
        let rendered_digest = sha256_hex(rendered.as_bytes());
        if present {
            let existing = fs::read_to_string(&path).map_err(|error| {
                OrbitError::Io(format!(
                    "read colliding routine '{}': {error}",
                    path.display()
                ))
            })?;
            let existing_digest = sha256_hex(existing.as_bytes());
            if existing_digest != rendered_digest {
                // A routines directory with no manifest at all predates managed
                // provenance, so content alone cannot separate a routine Orbit
                // seeded and the operator then customized — flipping `enabled`
                // and pinning a host is the documented lifecycle — from a file
                // the operator wrote from scratch. Adopt the on-disk instance's
                // own binding so a later shipped-template change can be
                // reconciled, and reserve the collision warning for a directory
                // Orbit already tracks, where a shipped name missing from the
                // manifest really is user-authored. Mirrors the guard in
                // `reconcile_managed_assets`. Adoption records the bytes now on
                // disk as Orbit's own, so a later shipped-template change
                // refreshes them onto the adopted binding; an edit made after
                // adoption is detected and preserved as usual.
                //
                // A default a prior release wrote to disk without recording
                // it — a newly shipped default seeded by a binary whose
                // manifest write did not land — still carries the name this
                // workspace seeds and a shape that release shipped. Adopt or
                // refresh it rather than calling Orbit's own file a
                // user-authored collision forever. The name is the
                // discriminator: an operator's own routine wearing a bundled
                // filename declares its own name and is still reported.
                if parse_routine_yaml(&existing)
                    .is_ok_and(|definition| definition.name == requested_binding.name)
                    && let Some(outcome) = reconcile_lifecycle_variant(
                        identity,
                        name,
                        template,
                        &template_digest,
                        &path,
                        &existing,
                        mode,
                        &mut result,
                    )?
                {
                    next_assets.insert((*name).to_string(), outcome.rendered_digest.clone());
                    next_provenance.insert((*name).to_string(), outcome);
                    continue;
                }
                if previous.is_none()
                    && let Some(binding) = adoptable_binding(identity, name, template, &existing)
                {
                    result.actions.push(ManagedAssetAction {
                        name: (*name).to_string(),
                        path: path.clone(),
                        outcome: ManagedAssetOutcome::Migrated,
                        detail: Some(
                            "adopted a pre-provenance routine using the binding parsed from that exact instance".to_string(),
                        ),
                    });
                    next_assets.insert((*name).to_string(), existing_digest.clone());
                    next_provenance.insert(
                        (*name).to_string(),
                        RoutineAssetProvenance {
                            template_digest,
                            rendered_digest: existing_digest,
                            binding,
                        },
                    );
                    continue;
                }
                let detail = format!(
                    "user-authored routine '{}' collides with bundled default `{name}` and was preserved; move or rename it, then rerun `orbit workspace sync`",
                    path.display()
                );
                result.warnings.push(detail.clone());
                result.actions.push(ManagedAssetAction {
                    name: (*name).to_string(),
                    path: path.clone(),
                    outcome: ManagedAssetOutcome::Preserved,
                    detail: Some(detail),
                });
                continue;
            }
            result.actions.push(ManagedAssetAction {
                name: (*name).to_string(),
                path: path.clone(),
                outcome: ManagedAssetOutcome::Migrated,
                detail: Some(
                    "recorded provenance for an exact existing shipped routine".to_string(),
                ),
            });
        } else {
            if mode == ManagedAssetReconcileMode::Apply {
                write_confined_routine(&path, &rendered)?;
            }
            result.refreshed += 1;
            result.actions.push(ManagedAssetAction {
                name: (*name).to_string(),
                path: path.clone(),
                outcome: ManagedAssetOutcome::Created,
                detail: None,
            });
        }
        next_assets.insert((*name).to_string(), rendered_digest.clone());
        next_provenance.insert(
            (*name).to_string(),
            RoutineAssetProvenance {
                template_digest,
                rendered_digest,
                binding: requested_binding,
            },
        );
    }

    // A definition targeting a retired job that neither loop above could
    // reach: the manifest never recorded it (a release wrote the file without
    // recording the write, or the manifest was since reset or hand-edited) and
    // it wears no shipped name. Left alone it loads as retired on every tick
    // while every surface advises a sync that reports `unchanged` forever
    // [DANI-10502], so judge it by content exactly as a tracked file is judged.
    reconcile_untracked_retired_routines(
        routines_dir,
        previous.as_ref(),
        &shipped,
        preservation_confined,
        mode,
        &mut result,
    )?;

    let next = ManagedAssetManifest {
        schema_version: ROUTINE_MANAGED_ASSET_MANIFEST_SCHEMA_VERSION,
        asset_kind: "routine".to_string(),
        assets: next_assets,
        routine_provenance: next_provenance,
        opted_out: Default::default(),
    };
    if mode == ManagedAssetReconcileMode::Apply && previous.as_ref() != Some(&next) {
        let encoded = encode_managed_asset_manifest(&next)?;
        let recorded = record_managed_manifest_write(
            &manifest_path,
            "routine",
            atomic_write_text(&manifest_path, &encoded),
            &mut result.warnings,
        )?;
        if !recorded {
            // Same contract as the other catalogs: a denied manifest write is
            // reported through `result.warnings`, and no adoption claims
            // provenance that never landed.
            for action in result
                .actions
                .iter_mut()
                .filter(|action| action.outcome == ManagedAssetOutcome::Migrated)
            {
                action.detail = Some(
                    "adopted in place; its provenance was not recorded because the manifest write was denied"
                        .to_string(),
                );
            }
        }
    }
    Ok(result)
}

/// Reconcile every top-level definition in `routines_dir` that targets a
/// retired job and that the manifest does not track. A copy of a template a
/// prior release shipped leaves the active catalog exactly as a tracked one
/// does; anything else is the operator's own file and is only reported —
/// with the step that actually clears it, since synchronization never deletes
/// a routine Orbit did not write.
///
/// Untracked bytes are never deleted outright: with no recorded digest there
/// is nothing proving Orbit wrote this exact file, so the copy under
/// `.retired-managed/routines/` is what makes the removal safe.
fn reconcile_untracked_retired_routines(
    routines_dir: &Path,
    previous: Option<&ManagedAssetManifest>,
    shipped: &BTreeSet<&str>,
    preservation_confined: bool,
    mode: ManagedAssetReconcileMode,
    result: &mut ManagedAssetReconciliation,
) -> Result<(), OrbitError> {
    for path in top_level_routine_files(routines_dir) {
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if shipped.contains(stem)
            || previous.is_some_and(|manifest| manifest.assets.contains_key(stem))
        {
            continue;
        }
        // An unreadable or unparsable file states no target, so it is not a
        // retired definition; the loader and `orbit doctor` report it.
        let Ok(existing) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(definition) = parse_routine_yaml(&existing) else {
            continue;
        };
        let job = definition.target.job_name();
        let Some(retirement) = retired_routine_job_reason(job) else {
            continue;
        };

        if shipped_shape_of(stem, &existing) != Some(ShippedShape::Retired) {
            let detail = format!(
                "routine '{}' targets retired job '{job}' ({retirement}) but is not one Orbit wrote, so `orbit workspace sync` cannot retire it; delete the file or retarget it at a job this Orbit ships",
                path.display()
            );
            result.warnings.push(detail.clone());
            result.actions.push(ManagedAssetAction {
                name: stem.to_string(),
                path,
                outcome: ManagedAssetOutcome::Preserved,
                detail: Some(detail),
            });
            continue;
        }
        if !preservation_confined {
            refuse_unconfined_preservation(result, stem, &path, routines_dir);
            continue;
        }

        let preserved = if mode == ManagedAssetReconcileMode::Apply {
            preserve_modified_retired_asset(
                routines_dir,
                "routine",
                ManagedAssetLayout::YamlStem,
                stem,
                &path,
            )?
        } else {
            retired_preservation_path(routines_dir, "routine", ManagedAssetLayout::YamlStem, stem)
        };
        result.actions.push(ManagedAssetAction {
            name: stem.to_string(),
            path,
            outcome: ManagedAssetOutcome::Retired,
            detail: Some(format!(
                "retired a prior release's routine the managed manifest never recorded; a copy is at '{}'",
                preserved.display()
            )),
        });
        result.retired += 1;
    }
    Ok(())
}

/// Regular `*.yaml` / `*.yml` files directly in `routines_dir`, in stable
/// filename order. The `local/` subdirectory is a separate origin that
/// seeding never writes to, so it is skipped with every other subdirectory;
/// a symbolic link is skipped without being followed, as the loader skips it.
/// A directory that cannot be listed yields nothing: reconciliation of the
/// managed defaults has already reported what it could not read.
fn top_level_routine_files(routines_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(routines_dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_file()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml")
                })
        })
        .collect();
    paths.sort();
    paths
}

/// A routine catalog path judged without following links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogEntry {
    Missing,
    /// A real directory or regular file, as the path requires.
    Present,
    /// A symbolic link (dangling or not) or an entry of the wrong type.
    Unsafe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogEntryKind {
    Directory,
    File,
}

fn inspect_catalog_entry(path: &Path, kind: CatalogEntryKind) -> Result<CatalogEntry, OrbitError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CatalogEntry::Missing);
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect routine catalog path '{}': {error}",
                path.display()
            )));
        }
    };
    // `symlink_metadata` reports a link as neither a directory nor a file.
    let file_type = metadata.file_type();
    let expected = match kind {
        CatalogEntryKind::Directory => file_type.is_dir(),
        CatalogEntryKind::File => file_type.is_file(),
    };
    Ok(if expected {
        CatalogEntry::Present
    } else {
        CatalogEntry::Unsafe
    })
}

/// Why the catalog itself cannot be reconciled: `routines_dir` or its
/// manifest is a link or the wrong kind of entry. Every definition and the
/// provenance that judges it sit behind that path.
fn unconfined_catalog(
    routines_dir: &Path,
    manifest_path: &Path,
) -> Result<Option<String>, OrbitError> {
    if inspect_catalog_entry(routines_dir, CatalogEntryKind::Directory)? == CatalogEntry::Unsafe {
        return Ok(Some(unconfined_detail(routines_dir)));
    }
    if inspect_catalog_entry(manifest_path, CatalogEntryKind::File)? == CatalogEntry::Unsafe {
        return Ok(Some(unconfined_detail(manifest_path)));
    }
    Ok(None)
}

/// Whether `.retired-managed/routines/` beside the catalog can receive a
/// preserved copy without a link redirecting the move out of the workspace.
fn preservation_route_is_confined(routines_dir: &Path) -> Result<bool, OrbitError> {
    let preserved = retired_preservation_path(
        routines_dir,
        "routine",
        ManagedAssetLayout::YamlStem,
        "routine",
    );
    let Some(kind_dir) = preserved.parent() else {
        return Ok(false);
    };
    let Some(backup_root) = kind_dir.parent() else {
        return Ok(false);
    };
    for dir in [backup_root, kind_dir] {
        if inspect_catalog_entry(dir, CatalogEntryKind::Directory)? == CatalogEntry::Unsafe {
            return Ok(false);
        }
    }
    Ok(true)
}

fn unconfined_detail(path: &Path) -> String {
    format!(
        "routine catalog path '{}' is a symbolic link or not the expected regular file or directory; Orbit did not read, create, overwrite, or retire anything through it. Replace it with a regular entry inside the workspace, then rerun `orbit workspace sync`",
        path.display()
    )
}

fn refuse_unconfined_routine(
    result: &mut ManagedAssetReconciliation,
    name: &str,
    path: &Path,
    detail: &str,
) {
    result.warnings.push(detail.to_string());
    result.actions.push(ManagedAssetAction {
        name: name.to_string(),
        path: path.to_path_buf(),
        outcome: ManagedAssetOutcome::Preserved,
        detail: Some(detail.to_string()),
    });
}

fn refuse_unconfined_preservation(
    result: &mut ManagedAssetReconciliation,
    name: &str,
    path: &Path,
    routines_dir: &Path,
) {
    let preserved =
        retired_preservation_path(routines_dir, "routine", ManagedAssetLayout::YamlStem, name);
    let detail = format!(
        "retiring routine '{}' needs a preserved copy, but '{}' is reached through a symbolic link or a non-directory; the routine was left in place. Replace that path with a regular directory inside the workspace, then rerun `orbit workspace sync`",
        path.display(),
        preserved.parent().unwrap_or(routines_dir).display()
    );
    refuse_unconfined_routine(result, name, path, &detail);
}

/// Write a managed routine definition under the catalog confinement
/// contract: the catalog must be a real directory (created when absent) and
/// the definition a regular file or absent. Stage and sync the complete
/// content before renaming it into place, then sync the catalog directory.
/// Readers keep the previous complete file until replacement, and a failed
/// staging write leaves it untouched. The staging file is created exclusively
/// with `O_NOFOLLOW` on Unix; rename replaces a final-component link that
/// appears after inspection without writing through it.
pub(super) fn write_confined_routine(path: &Path, content: &str) -> Result<(), OrbitError> {
    let routines_dir = path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "managed routine '{}' has no catalog directory",
            path.display()
        ))
    })?;
    match inspect_catalog_entry(routines_dir, CatalogEntryKind::Directory)? {
        CatalogEntry::Missing => fs::create_dir_all(routines_dir).map_err(|error| {
            OrbitError::Io(format!(
                "create routine catalog '{}': {error}",
                routines_dir.display()
            ))
        })?,
        CatalogEntry::Present => {}
        CatalogEntry::Unsafe => {
            return Err(OrbitError::InvalidInput(unconfined_detail(routines_dir)));
        }
    }
    if inspect_catalog_entry(path, CatalogEntryKind::File)? == CatalogEntry::Unsafe {
        return Err(OrbitError::InvalidInput(unconfined_detail(path)));
    }
    atomic_write_text(path, content).map_err(|error| {
        OrbitError::Io(format!(
            "write managed routine '{}': {error}",
            path.display()
        ))
    })
}
